//! banto-hub のサービスの「設定モード ⇔ 運用モード」の判定と文言（#661 661-2、
//! docs/banto-hub-t17-design.md §1 の「2026-10-19 オーナー決定（#661）」）。
//!
//! サービス本体（`bin/banto_hub/win_service.rs`、Windows だけ）が使う**純関数**だけを置く
//! （どの OS でもテストする）。ファイル・レジストリ・SCM には触れない。手本は ChronoGazer の
//! `chronogazer_core::server_service`（#623 PR-3）。
//!
//! ## 段の順序（`win_service.rs` の `HubProgram::run_inner`）
//!
//! 1. Redirection Guard → 2. 開始の引数（[`parse_start_args`]。`relax-only` ちょうどか、
//!    何も無いか。それ以外は何も変えずに 3）→ 3. 戻す相手を決める（[`plan_owners`]）→
//!    4. profile を辿らずに開いて握る（作らない）→ 5. profile の排他 → 6. `logs` を設定モードの
//!    形で開いてログを始める → 7. 相手が決まらなければ 4／5 → 8. 1 周目（`scan_tree`）→
//!    （設定モードの形からなら）HKLM に相手を記録 → 9. 2 周目（`close_tree`）→ 10. 管理する
//!    フォルダと印 → 11. Hub を起動して Running。
//! - STOP: Hub を止めて設定 DB を閉じる（閉じ切れなければ緩めずに 5）→ tokio ランタイムを
//!   終える（終わらなければ緩めずに 5）→ 最後の行（[`relax_begin_line`]）→ ログを閉じる →
//!   `relax_tree` → 0／5。PRESHUTDOWN では緩めない。
//!
//! ## 戻す相手（[`plan_owners`]）
//!
//! 起動のたびに DACL から採り直さない。profile の形を先に見て分ける:
//! - **設定モードの形（緩んでいる）**: profile の DACL から採る（`plan_carry_over`。明示の・
//!   木全体に継承する「変更」以上の利用者のアカウント）。0 人なら何も変えずに 4。
//! - **運用モードの形（締まっている。運用中・締めかけ・緩めかけ）**: DACL からは採らない
//!   （利用者は「読み取りと実行」だけで、採ると空になる）。HKLM の記録を確かめて使い、
//!   上書きしない。記録が無い・空・読めないなら何も変えずに 5。

use std::ffi::OsString;
use std::time::Duration;

use banto_safe_root::guard::{plan_carry_over, AccountKind};
use banto_safe_root::mode::{
    classify_mode, AbortReason, MarkerError, ModeObservation, Progress, RootObservation,
    ScanRefusal, StopSignal as TreeStop, TreeProblem,
};
use banto_safe_root::{InstallUserSid, RedirectionGuard};

use crate::service_manager::{
    HUB_EXIT_LEFT_TIGHT, HUB_EXIT_NOT_OPERATIONAL, HUB_EXIT_START_FAILED,
};

/// 行の先頭に付ける製品名。
const LABEL: &str = "banto-hub";

/// 終了コード 0（中止・正常）。
const EXIT_OK: u32 = 0;

/// 修復の案内（緩め直せない・締めたまま残ったとき）。
const REPAIR_HINT: &str = "サービスを relax-only で開始するか、banto-hub-elev.exe grant-profile-acl（管理者）で戻してください";

// --- 開始の引数 ------------------------------------------------------------------------

/// 緩めるだけの開始の引数（`StartService` の引数）。
pub const RELAX_ONLY_ARG: &str = "relax-only";

/// 開始の引数の解釈（[`parse_start_args`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartArgs {
    /// 引数なし: ふつうの開始（運用モードへ締めて Hub を動かす）。
    Normal,
    /// `["relax-only"]` ちょうど: 緩めるだけ（Hub を動かさず、Running を報告しない）。
    RelaxOnly,
    /// それ以外。**何も変えずに断る**（終了コード 3）。中身は表示用。
    Unknown(String),
}

/// `ServiceMain` の argv（先頭はサービス名）を解釈する。**引数は信頼できない入力**:
/// 決まった値との完全一致だけで分ける（打ち間違いを「ふつうの開始」= 締める向きにしない）。
pub fn parse_start_args(args: &[OsString]) -> StartArgs {
    let rest = args.get(1..).unwrap_or_default();
    if rest.is_empty() {
        return StartArgs::Normal;
    }
    if rest.len() == 1 && rest[0] == RELAX_ONLY_ARG {
        return StartArgs::RelaxOnly;
    }
    StartArgs::Unknown(
        rest.iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(" "),
    )
}

// --- 戻す相手 --------------------------------------------------------------------------

/// profile の形（profile のフォルダ自身の DACL だけで決める。締めるのは上から・緩めるのは
/// profile を最後に書くので、「profile が締まっている ⇔ 運用中か切り替えの途中」）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileShape {
    /// profile のフォルダが無い。
    Absent,
    /// 運用モードの形ではない（設定モードの形・継承の既定の DACL・手で変えた形など）。
    Loose,
    /// 運用モードの形（`classify_mode` が `Operation`）。
    Tight,
    /// 記述子を読めない（LocalSystem で拒否された。形を決められない）。
    Unreadable,
}

/// profile の記述子の観察から形を決める。
pub fn profile_shape(observation: &RootObservation) -> ProfileShape {
    match observation {
        RootObservation::Absent => ProfileShape::Absent,
        RootObservation::Denied => ProfileShape::Unreadable,
        RootObservation::Present(_) => {
            // `Operation` は「自分」に依らない（利用者の ACE が全部「読み取りと実行」）。
            // 引数の利用者は形の判定には使われない値。
            let anyone = InstallUserSid::parse("S-1-5-21-0-0-0-0").expect("形の正しい SID");
            match classify_mode(observation, &anyone) {
                ModeObservation::Operation => ProfileShape::Tight,
                _ => ProfileShape::Loose,
            }
        }
    }
}

/// HKLM の記録（`crate::profile_owners::read_owners`）を確かめた結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoredOwners {
    /// 空でなく、どれも利用者の SID の形（重複はまとめた）。
    Valid(Vec<InstallUserSid>),
    /// 記録が無い。
    Missing,
    /// 空・SID として読めない・読めなかった。中身は理由。
    Unusable(String),
}

/// 読んだ記録を確かめる（空でない・どれも `InstallUserSid::parse` に通る）。
pub fn check_stored_owners(read: Result<Option<Vec<String>>, String>) -> StoredOwners {
    let items = match read {
        Ok(None) => return StoredOwners::Missing,
        Ok(Some(items)) => items,
        Err(reason) => return StoredOwners::Unusable(reason),
    };
    if items.is_empty() {
        return StoredOwners::Unusable("記録が空です".to_string());
    }
    let mut owners: Vec<InstallUserSid> = Vec::new();
    for item in &items {
        match InstallUserSid::parse(item) {
            Ok(sid) => {
                if !owners.contains(&sid) {
                    owners.push(sid);
                }
            }
            Err(_) => {
                return StoredOwners::Unusable(format!(
                    "記録に利用者の SID でない値があります（{item:?}）"
                ))
            }
        }
    }
    StoredOwners::Valid(owners)
}

/// ふつうの開始で、戻す相手をどう決めるか（[`plan_owners`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnersPlan {
    /// 設定モードの形から締める: DACL から採った組を、1 周目の後・2 周目の前に HKLM に
    /// 記録してから締める。
    FromDacl(Vec<InstallUserSid>),
    /// 運用モードの形から再開する: HKLM の記録を使う（書かない）。
    FromRecord(Vec<InstallUserSid>),
    /// 相手が 0 人（profile が無い・「変更」以上の利用者がいない・読めない ACE がある）。
    /// 何も変えずに 4。中身は理由。
    NoOwners(String),
    /// profile が締まっているのに記録が使えない。何も変えずに 5。中身は理由。
    RecordUnusable(String),
}

impl OwnersPlan {
    /// 締める・緩めるに使う組（決まらなければ空）。`open_root` の所有者として許す利用者にも
    /// 使う。
    pub fn owners(&self) -> &[InstallUserSid] {
        match self {
            Self::FromDacl(owners) | Self::FromRecord(owners) => owners,
            Self::NoOwners(_) | Self::RecordUnusable(_) => &[],
        }
    }

    /// 相手が決まらないときの終了コード（決まれば `None`）。
    pub fn refusal_exit(&self) -> Option<u32> {
        match self {
            Self::FromDacl(_) | Self::FromRecord(_) => None,
            Self::NoOwners(_) => Some(HUB_EXIT_NOT_OPERATIONAL),
            Self::RecordUnusable(_) => Some(HUB_EXIT_LEFT_TIGHT),
        }
    }
}

/// ふつうの開始で戻す相手を決める（「採るか、使うかの分岐」）。`account` は SID が利用者の
/// アカウントか（本番は `banto_safe_root::guard::account_kind`）。
pub fn plan_owners(
    observation: &RootObservation,
    stored: StoredOwners,
    account: impl FnMut(&InstallUserSid) -> AccountKind,
) -> OwnersPlan {
    match (profile_shape(observation), observation) {
        (ProfileShape::Absent, _) => {
            OwnersPlan::NoOwners("profile のフォルダがありません".to_string())
        }
        (ProfileShape::Unreadable, _) => {
            OwnersPlan::NoOwners("profile の権限を読めませんでした".to_string())
        }
        (ProfileShape::Tight, _) => match stored {
            StoredOwners::Valid(owners) => OwnersPlan::FromRecord(owners),
            StoredOwners::Missing => OwnersPlan::RecordUnusable("記録がありません".to_string()),
            StoredOwners::Unusable(reason) => OwnersPlan::RecordUnusable(reason),
        },
        (ProfileShape::Loose, RootObservation::Present(dacl)) => {
            let carry = plan_carry_over(dacl, account);
            if carry.null_dacl {
                return OwnersPlan::NoOwners(
                    "profile の DACL が空（null）で、戻す相手を決められません".to_string(),
                );
            }
            if !carry.blocked.is_empty() {
                return OwnersPlan::NoOwners(format!(
                    "profile の DACL に読めない ACE が {} 件あります",
                    carry.blocked.len()
                ));
            }
            if carry.carried.is_empty() {
                return OwnersPlan::NoOwners(
                    "profile に「変更」以上の権限を明示で持つ利用者がいません".to_string(),
                );
            }
            OwnersPlan::FromDacl(carry.carried)
        }
        (ProfileShape::Loose, _) => unreachable!("Loose は Present からだけ"),
    }
}

/// `relax-only` で何をするか（[`plan_relax_only`]）。DACL からは採らない。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelaxOnlyPlan {
    /// 記録の組で緩める。
    Relax(Vec<InstallUserSid>),
    /// 緩める物が無い（profile が無い・締まっていないのに記録も無い）。0。
    NothingToRelax,
    /// 締まっている（か形を読めない）のに記録が使えない。何も変えずに 5。中身は理由。
    LeftTight(String),
}

/// `relax-only` の相手を決める。
pub fn plan_relax_only(observation: &RootObservation, stored: StoredOwners) -> RelaxOnlyPlan {
    let shape = profile_shape(observation);
    if shape == ProfileShape::Absent {
        return RelaxOnlyPlan::NothingToRelax;
    }
    match stored {
        StoredOwners::Valid(owners) => RelaxOnlyPlan::Relax(owners),
        StoredOwners::Missing | StoredOwners::Unusable(_) if shape == ProfileShape::Loose => {
            RelaxOnlyPlan::NothingToRelax
        }
        StoredOwners::Missing => RelaxOnlyPlan::LeftTight("記録がありません".to_string()),
        StoredOwners::Unusable(reason) => RelaxOnlyPlan::LeftTight(reason),
    }
}

// --- 段ごとの終了コード ----------------------------------------------------------------

/// 1 周目が断った後の終了コード: 見つけた物があれば何も変えずに 4、I/O は 3、止める合図は
/// 中止 = 0。ただし締まった形から再開していたとき（`from_tight`）は、何も変えていなくても
/// profile は締まったままなので、理由によらず 5（4 は「緩んだまま」、0 は「緩んでいる」と
/// 読まれ、呼び出し側が Desktop を許してしまうため）。
pub fn exit_after_scan_refusal(refusal: &ScanRefusal, from_tight: bool) -> u32 {
    if from_tight {
        return HUB_EXIT_LEFT_TIGHT;
    }
    match refusal {
        ScanRefusal::Problems(_) => HUB_EXIT_NOT_OPERATIONAL,
        ScanRefusal::Io(_) => HUB_EXIT_START_FAILED,
        ScanRefusal::Stopped(_) => EXIT_OK,
    }
}

/// 2 周目が止まった後にすること。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AbortPlan {
    /// 締めた分を `relax_tree` で緩め直すか（PRESHUTDOWN なら緩めない）。
    pub relax_back: bool,
    /// 緩め直せた（または緩めない）ときの終了コード。
    pub exit_if_relaxed: u32,
}

/// 2 周目が止まった理由から決める: 想定外の物（木の外へ移された物を含む）・I/O → 緩め直して
/// 4（緩め直せなければ 5）、STOP → 緩め直して 0、PRESHUTDOWN → 緩めずに 0。
pub fn after_close_abort(reason: &AbortReason) -> AbortPlan {
    match reason {
        AbortReason::Problem(_) | AbortReason::Io(_) => AbortPlan {
            relax_back: true,
            exit_if_relaxed: HUB_EXIT_NOT_OPERATIONAL,
        },
        AbortReason::Stopped(TreeStop::Preshutdown) => AbortPlan {
            relax_back: false,
            exit_if_relaxed: EXIT_OK,
        },
        AbortReason::Stopped(_) => AbortPlan {
            relax_back: true,
            exit_if_relaxed: EXIT_OK,
        },
    }
}

/// 締めた後の管理するフォルダの印の失敗: reparse にされた・印の席を取られた・使用中は
/// 締めた後の改ざんとみなして 5、I/O は 3（どちらも緩め直す）。戻り値は（改ざんか, 緩め
/// 直せたときの終了コード）。
pub fn after_marker_error(error: &MarkerError) -> (bool, u32) {
    match error {
        MarkerError::DirIsReparse { .. } | MarkerError::Occupied(_) | MarkerError::Busy { .. } => {
            (true, HUB_EXIT_LEFT_TIGHT)
        }
        MarkerError::Io(_) => (false, HUB_EXIT_START_FAILED),
    }
}

/// 緩め直しの結果で終了コードを決める: 緩め直せたら `if_relaxed`、できなければ 5。
pub fn exit_after_relax_back(relaxed: bool, if_relaxed: u32) -> u32 {
    if relaxed {
        if_relaxed
    } else {
        HUB_EXIT_LEFT_TIGHT
    }
}

/// STOP で緩めてよいか: 設定 DB を閉じ終え（`db_closed`）、Hub の tokio ランタイムを上限の
/// 中で終えた（`runtime_finished`）ときだけ。どちらかが欠ければ締めたまま 5（残った接続・
/// タスクが、緩めた後に利用者の書ける木へ書くのを防ぐ）。
pub fn relax_allowed_after_shutdown(db_closed: bool, runtime_finished: bool) -> bool {
    db_closed && runtime_finished
}

// --- 文言 ------------------------------------------------------------------------------

/// 1 周目が見つけた物の一覧の行の上限。
pub const SCAN_PROBLEMS_SHOWN: usize = 20;

/// Redirection Guard の結果の 1 行。
pub fn redirection_guard_line(result: RedirectionGuard) -> String {
    match result {
        RedirectionGuard::Enabled => {
            format!("{LABEL}: Redirection Guard を有効にしました（非管理者のジャンクションを辿りません）")
        }
        RedirectionGuard::Failed { os_error } => format!(
            "{LABEL}: Redirection Guard を有効にできませんでした（os error {os_error}。古い Windows の可能性）。運用モードの ACL で続けます"
        ),
        RedirectionGuard::NotWindows => {
            format!("{LABEL}: Redirection Guard はこの OS にはありません")
        }
    }
}

/// 運用モードへの切り替えの開始の行（ログを開いた直後）。
pub fn mode_switch_begin_line() -> String {
    format!("{LABEL}: 運用モードへの切り替えを始めます（profile の ACL を確かめて締めます。締め終わるまで Hub は動かしません）")
}

/// 運用モードへの切り替えの完了の行（Running を報告した後）。
pub fn mode_switch_done_line(progress: &Progress, elapsed: Duration) -> String {
    format!(
        "{LABEL}: 運用モードへの切り替えを終えました（確かめた物 {} 件・書いた物 {} 件、所要 {:.1} 秒）",
        progress.examined,
        progress.written,
        elapsed.as_secs_f64()
    )
}

/// 設定モードの形から締めるときの、相手を決めた行。
pub fn owners_from_dacl_line(count: usize, location: &str) -> String {
    format!("{LABEL}: profile の権限から、緩めるときに「変更」を戻す相手を {count} 人決めました（記録: {location}）")
}

/// 運用モードの形から再開するときの、記録を使う行。
pub fn owners_from_record_line(count: usize, location: &str) -> String {
    format!("{LABEL}: profile は既に運用モードの形です。記録した相手 {count} 人を使います（{location}。採り直しません）")
}

/// 相手が 0 人のときの行（何も変えずに 4）。
pub fn no_owners_line(reason: &str) -> String {
    format!(
        "{LABEL}: 運用モードにできません（終了コード {HUB_EXIT_NOT_OPERATIONAL}）: {reason}。何も変えていません（設定モードのまま）。banto-hub-elev.exe grant-profile-acl で利用者に権限を付けてから、もう一度開始してください"
    )
}

/// 締まっているのに記録が使えないときの行（何も変えずに 5）。
pub fn record_unusable_line(reason: &str, location: &str) -> String {
    format!(
        "{LABEL}: profile は締まっていますが、戻す相手の記録（{location}）が使えません（{reason}）。何も変えずに終わります（終了コード {HUB_EXIT_LEFT_TIGHT}）。banto-hub-elev.exe grant-profile-acl（管理者）で記録を書き直して戻してください"
    )
}

/// 相手を記録できなかった行（締める前。何も変えていない）。
pub fn owners_record_failed_line(error: &str) -> String {
    format!(
        "{LABEL}: 戻す相手を記録できなかったため、締めずに終わります（終了コード {HUB_EXIT_START_FAILED}。何も変えていません）: {error}"
    )
}

/// 1 周目が断ったときの行。`from_tight` は締まった形から再開していたか（そのときは
/// 「設定モードのまま」と書かず、締まったまま 5 で終わると書く）。
pub fn scan_refused_lines(problems: &[TreeProblem], from_tight: bool) -> Vec<String> {
    let first = if from_tight {
        format!(
            "{LABEL}: 運用モードで再開できません（終了コード {HUB_EXIT_LEFT_TIGHT}）: profile に締められない物が {} 件あります。何も変えていません（profile は締まったまま＝運用モードの形のまま）。リンク・ハードリンクは外し、暗号化は解除し、開けないファイルは権限を戻してから、もう一度開始してください。設定モードに戻すなら{REPAIR_HINT}",
            problems.len()
        )
    } else {
        format!(
            "{LABEL}: 運用モードにできません（終了コード {HUB_EXIT_NOT_OPERATIONAL}）: profile に締められない物が {} 件あります。何も変えていません（設定モードのまま）。リンク・ハードリンクは外し、暗号化は解除し、開けないファイルは権限を戻してから、もう一度運用モードにしてください",
            problems.len()
        )
    };
    let mut lines = vec![first];
    for problem in problems.iter().take(SCAN_PROBLEMS_SHOWN) {
        lines.push(format!("{LABEL}:   {problem}"));
    }
    if problems.len() > SCAN_PROBLEMS_SHOWN {
        lines.push(format!(
            "{LABEL}:   …ほか {} 件",
            problems.len() - SCAN_PROBLEMS_SHOWN
        ));
    }
    lines
}

/// 2 周目が止まったときの行。
pub fn close_abort_line(written: u64, reason: &AbortReason) -> String {
    let why = match reason {
        AbortReason::Problem(problem) => format!("締められない物に会いました: {problem}"),
        AbortReason::Stopped(signal) => format!("止める合図（{signal:?}）を受けました"),
        AbortReason::Io(error) => format!("書けませんでした: {error}"),
    };
    format!("{LABEL}: 運用モードへの締めを {written} 件で止めました。{why}")
}

/// 締めた分を緩め直す前の行（この後ログを閉じる）。
pub fn relax_back_begin_line(why: &str, exit_if_relaxed: u32) -> String {
    format!(
        "{LABEL}: {why}。締めた分を緩め直します（終了コード {exit_if_relaxed}、戻せなければ {HUB_EXIT_LEFT_TIGHT}。この後の行はありません）"
    )
}

/// 締めた後の管理するフォルダで改ざんを見つけたときの行。
pub fn tampered_line(key: &str, error: &MarkerError) -> String {
    format!(
        "{LABEL}: フォルダ「{key}」が締めた後に外から変えられました（終了コード {HUB_EXIT_LEFT_TIGHT}）: {error}。Hub は動かしません"
    )
}

/// STOP で、緩め始める前の**最後の**行（この後はログに書かない）。
pub fn relax_begin_line() -> String {
    format!("{LABEL}: 設定モードへの切り替えのため profile の ACL を緩め始めます（Hub の停止・設定 DB を閉じる・ランタイムの終了を終えました。この後の行はありません）")
}

/// `relax-only` で緩め始める前の最後の行。
pub fn relax_only_begin_line() -> String {
    format!("{LABEL}: relax-only で開始しました。Hub は動かさず、profile の ACL を緩め始めます（この後の行はありません）")
}

/// `relax-only` で緩める物が無いときの行。
pub fn relax_only_nothing_line() -> String {
    format!("{LABEL}: relax-only: profile は締まっていません（緩める物がありません）。何も変えずに終わります")
}

/// `relax-only` で記録が使えないときの行（締めたまま 5）。
pub fn relax_only_left_tight_line(reason: &str, location: &str) -> String {
    format!(
        "{LABEL}: relax-only: 戻す相手の記録（{location}）が使えないため（{reason}）、緩めずに終わります（終了コード {HUB_EXIT_LEFT_TIGHT}）。banto-hub-elev.exe grant-profile-acl（管理者）で戻してください"
    )
}

/// 知らない開始の引数で断るときの行。
pub fn unknown_start_arg_line(arg: &str) -> String {
    format!(
        "{LABEL}: 知らない開始の引数です（{arg:?}）。何も変えずに終わります（終了コード {HUB_EXIT_START_FAILED}）。緩めるだけの開始は '{RELAX_ONLY_ARG}' だけです"
    )
}

/// 運用中に profile の権限が外から変えられた（見張り・定期の確かめ直し）ので、締めたまま
/// 止まる行（ChronoGazer の `acl_changed_line` と同じ扱い）。
pub fn acl_changed_line(what: &str) -> String {
    format!(
        "{LABEL}: 運用中に profile の権限が外から変えられました（終了コード {HUB_EXIT_LEFT_TIGHT}。締めたまま止まります）: {what}。管理者の icacls・グループポリシーの「ファイル システム」・バックアップソフトの復元が原因なら、それを止めてから {REPAIR_HINT}"
    )
}

/// 設定 DB を閉じ切れなかったので緩めずに終わる行。
pub fn db_not_closed_keeps_tight_line() -> String {
    format!(
        "{LABEL}: 設定 DB を閉じ切れなかった（返らない接続がある）ため、ACL を緩めずに終わります（終了コード {HUB_EXIT_LEFT_TIGHT}。運用モードの形のまま）。{REPAIR_HINT}"
    )
}

/// Hub のランタイムが上限の中で終わらなかったので緩めずに終わる行。
pub fn runtime_still_busy_keeps_tight_line(waited: Duration) -> String {
    format!(
        "{LABEL}: Hub の非同期ランタイムが {} 秒待っても終わらなかった（処理が残っている可能性）ため、ACL を緩めずに終わります（終了コード {HUB_EXIT_LEFT_TIGHT}。運用モードの形のまま）。{REPAIR_HINT}",
        waited.as_secs()
    )
}

/// PRESHUTDOWN で締めたまま終わる行。
pub fn preshutdown_keeps_tight_line() -> String {
    format!("{LABEL}: OS の終了のため ACL を緩めずに終わります（運用モードの形のまま。次のサービスの開始で再開するか、relax-only で戻ります）")
}

#[cfg(test)]
mod tests {
    use super::*;
    use banto_safe_root::acl::{ObservedAce, ObservedDacl};

    const USER_A: &str = "S-1-5-21-1111111111-2222222222-3333333333-1001";
    const USER_B: &str = "S-1-5-21-1111111111-2222222222-3333333333-1002";
    const SYSTEM: &str = "S-1-5-18";
    const ADMINS: &str = "S-1-5-32-544";
    const ALLOW: u8 = 0;
    const OICI: u8 = 0x03;
    const INHERITED: u8 = 0x10;
    const FULL: u32 = 0x001F_01FF;
    const MODIFY: u32 = 0x0013_01BF;
    const READ: u32 = 0x0012_00A9;

    fn ace(sid: &str, mask: u32, flags: u8) -> ObservedAce {
        ObservedAce {
            ace_type: ALLOW,
            flags,
            mask,
            sid: sid.to_string(),
        }
    }

    fn present(owner: &str, users: &[(&str, u32, u8)]) -> RootObservation {
        let mut aces = vec![ace(SYSTEM, FULL, OICI), ace(ADMINS, FULL, OICI)];
        aces.extend(
            users
                .iter()
                .map(|(sid, mask, flags)| ace(sid, *mask, *flags)),
        );
        RootObservation::Present(ObservedDacl {
            protected: true,
            owner: owner.to_string(),
            aces: Some(aces),
        })
    }

    fn sid(s: &str) -> InstallUserSid {
        InstallUserSid::parse(s).unwrap()
    }

    fn users(_: &InstallUserSid) -> AccountKind {
        AccountKind::User
    }

    #[test]
    fn start_args_match_relax_only_exactly() {
        let args = |rest: &[&str]| -> Vec<OsString> {
            std::iter::once("BantoHub")
                .chain(rest.iter().copied())
                .map(OsString::from)
                .collect()
        };
        assert_eq!(parse_start_args(&args(&[])), StartArgs::Normal);
        assert_eq!(parse_start_args(&[]), StartArgs::Normal);
        assert_eq!(
            parse_start_args(&args(&["relax-only"])),
            StartArgs::RelaxOnly
        );
        for wrong in [&["relax_only"][..], &["Relax-Only"], &["relax-only", "x"]] {
            assert!(
                matches!(parse_start_args(&args(wrong)), StartArgs::Unknown(_)),
                "{wrong:?}"
            );
        }
    }

    #[test]
    fn shape_is_tight_only_for_the_operation_form() {
        assert_eq!(
            profile_shape(&present(ADMINS, &[(USER_A, READ, OICI)])),
            ProfileShape::Tight
        );
        assert_eq!(
            profile_shape(&present(USER_A, &[(USER_A, MODIFY, OICI)])),
            ProfileShape::Loose
        );
        assert_eq!(
            profile_shape(&RootObservation::Absent),
            ProfileShape::Absent
        );
        assert_eq!(
            profile_shape(&RootObservation::Denied),
            ProfileShape::Unreadable
        );
    }

    #[test]
    fn stored_owners_must_be_non_empty_user_sids() {
        assert_eq!(check_stored_owners(Ok(None)), StoredOwners::Missing);
        assert!(matches!(
            check_stored_owners(Ok(Some(vec![]))),
            StoredOwners::Unusable(_)
        ));
        assert!(matches!(
            check_stored_owners(Ok(Some(vec![USER_A.into(), "S-1-5-32-544".into()]))),
            StoredOwners::Unusable(_)
        ));
        assert!(matches!(
            check_stored_owners(Err("読めない".into())),
            StoredOwners::Unusable(_)
        ));
        assert_eq!(
            check_stored_owners(Ok(Some(vec![USER_A.into(), USER_B.into(), USER_A.into()]))),
            StoredOwners::Valid(vec![sid(USER_A), sid(USER_B)])
        );
    }

    /// 設定モードの形からは DACL から採る（記録があっても採り直す）。
    #[test]
    fn a_loose_profile_takes_owners_from_the_dacl() {
        let observation = present(
            USER_A,
            &[
                (USER_A, MODIFY, OICI),
                (USER_B, FULL, OICI),
                // 継承した・読み取りだけの ACE は採らない。
                ("S-1-5-21-1-2-3-1003", MODIFY, OICI | INHERITED),
                ("S-1-5-21-1-2-3-1004", READ, OICI),
            ],
        );
        let plan = plan_owners(
            &observation,
            StoredOwners::Valid(vec![sid("S-1-5-21-9-9-9-1001")]),
            users,
        );
        assert_eq!(plan, OwnersPlan::FromDacl(vec![sid(USER_A), sid(USER_B)]));
        assert_eq!(plan.refusal_exit(), None);
    }

    /// 0 人なら 4（何も書かない）。
    #[test]
    fn a_loose_profile_without_modify_users_is_not_operational() {
        for observation in [
            present(ADMINS, &[]),
            present(ADMINS, &[(USER_A, MODIFY, OICI | INHERITED)]),
            RootObservation::Absent,
            RootObservation::Denied,
        ] {
            let plan = plan_owners(&observation, StoredOwners::Missing, users);
            assert!(matches!(plan, OwnersPlan::NoOwners(_)), "{plan:?}");
            assert_eq!(plan.refusal_exit(), Some(HUB_EXIT_NOT_OPERATIONAL));
            assert!(plan.owners().is_empty());
        }
        // 利用者のアカウントでない SID も採らない。
        let plan = plan_owners(
            &present(USER_A, &[(USER_A, MODIFY, OICI)]),
            StoredOwners::Missing,
            |_| AccountKind::NotAUser,
        );
        assert!(matches!(plan, OwnersPlan::NoOwners(_)), "{plan:?}");
    }

    /// 締まった形からは記録を使い、記録が使えなければ 5。
    #[test]
    fn a_tight_profile_uses_the_record_or_stops_with_5() {
        let tight = present(ADMINS, &[(USER_A, READ, OICI)]);
        assert_eq!(
            plan_owners(&tight, StoredOwners::Valid(vec![sid(USER_B)]), users),
            OwnersPlan::FromRecord(vec![sid(USER_B)])
        );
        for stored in [StoredOwners::Missing, StoredOwners::Unusable("空".into())] {
            let plan = plan_owners(&tight, stored, users);
            assert!(matches!(plan, OwnersPlan::RecordUnusable(_)), "{plan:?}");
            assert_eq!(plan.refusal_exit(), Some(HUB_EXIT_LEFT_TIGHT));
        }
    }

    #[test]
    fn relax_only_uses_only_the_record() {
        let tight = present(ADMINS, &[(USER_A, READ, OICI)]);
        let loose = present(USER_A, &[(USER_A, MODIFY, OICI)]);
        assert_eq!(
            plan_relax_only(&tight, StoredOwners::Valid(vec![sid(USER_A)])),
            RelaxOnlyPlan::Relax(vec![sid(USER_A)])
        );
        assert!(matches!(
            plan_relax_only(&tight, StoredOwners::Missing),
            RelaxOnlyPlan::LeftTight(_)
        ));
        assert!(matches!(
            plan_relax_only(&RootObservation::Denied, StoredOwners::Missing),
            RelaxOnlyPlan::LeftTight(_)
        ));
        assert_eq!(
            plan_relax_only(&loose, StoredOwners::Missing),
            RelaxOnlyPlan::NothingToRelax
        );
        assert_eq!(
            plan_relax_only(
                &RootObservation::Absent,
                StoredOwners::Valid(vec![sid(USER_A)])
            ),
            RelaxOnlyPlan::NothingToRelax
        );
        // 緩んでいても記録があれば緩める（緩めかけで飛ばした子を戻す）。
        assert_eq!(
            plan_relax_only(&loose, StoredOwners::Valid(vec![sid(USER_A)])),
            RelaxOnlyPlan::Relax(vec![sid(USER_A)])
        );
    }

    #[test]
    fn exits_per_stage() {
        assert_eq!(
            exit_after_scan_refusal(&ScanRefusal::Problems(vec![]), false),
            HUB_EXIT_NOT_OPERATIONAL
        );
        assert_eq!(
            exit_after_scan_refusal(
                &ScanRefusal::Io(banto_safe_root::SafeRootError::UnsupportedPlatform),
                false
            ),
            HUB_EXIT_START_FAILED
        );
        assert_eq!(
            exit_after_scan_refusal(&ScanRefusal::Stopped(TreeStop::Stop), false),
            0
        );
        // 締まった形から再開して断ったら、理由によらず締まったまま 5。
        for refusal in [
            ScanRefusal::Problems(vec![]),
            ScanRefusal::Io(banto_safe_root::SafeRootError::UnsupportedPlatform),
            ScanRefusal::Stopped(TreeStop::Stop),
        ] {
            assert_eq!(exit_after_scan_refusal(&refusal, true), HUB_EXIT_LEFT_TIGHT);
        }
        let io = AbortReason::Io(banto_safe_root::SafeRootError::UnsupportedPlatform);
        assert_eq!(
            after_close_abort(&io),
            AbortPlan {
                relax_back: true,
                exit_if_relaxed: HUB_EXIT_NOT_OPERATIONAL
            }
        );
        assert!(!after_close_abort(&AbortReason::Stopped(TreeStop::Preshutdown)).relax_back);
        assert!(after_close_abort(&AbortReason::Stopped(TreeStop::Stop)).relax_back);
        assert_eq!(exit_after_relax_back(true, 4), 4);
        assert_eq!(exit_after_relax_back(false, 0), HUB_EXIT_LEFT_TIGHT);
        assert!(relax_allowed_after_shutdown(true, true));
        assert!(!relax_allowed_after_shutdown(false, true));
        assert!(!relax_allowed_after_shutdown(true, false));
        assert_eq!(
            after_marker_error(&MarkerError::Io(
                banto_safe_root::SafeRootError::UnsupportedPlatform
            )),
            (false, HUB_EXIT_START_FAILED)
        );
    }

    #[test]
    fn lines_name_the_exit_codes_and_the_repair() {
        assert!(no_owners_line("x").contains("終了コード 4"));
        assert!(no_owners_line("x").contains("grant-profile-acl"));
        assert!(record_unusable_line("x", "HKLM").contains("終了コード 5"));
        assert!(db_not_closed_keeps_tight_line().contains("終了コード 5"));
        assert!(db_not_closed_keeps_tight_line().contains("relax-only"));
        assert!(runtime_still_busy_keeps_tight_line(Duration::from_secs(10)).contains("10 秒"));
        assert!(unknown_start_arg_line("x").contains("終了コード 3"));
        assert!(mode_switch_done_line(
            &Progress {
                examined: 2,
                written: 1
            },
            Duration::from_millis(1500)
        )
        .contains("所要 1.5 秒"));
        let many: Vec<TreeProblem> = Vec::new();
        assert_eq!(scan_refused_lines(&many, false).len(), 1);
        let tight = scan_refused_lines(&many, true);
        assert!(tight[0].contains("終了コード 5"), "{tight:?}");
        assert!(!tight[0].contains("設定モードのまま"), "{tight:?}");
    }
}
