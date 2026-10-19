//! モードの ACL（#623 PR-1c、設計 §2）。設定モード ⇔ 運用モードの切り替えで、サービス
//! （LocalSystem）自身がルート／profile の木の全体の ACL を**締める・緩める**ための部品と、
//! デスクトップがルートの DACL を読むだけでモードを見分ける純関数。
//!
//! # 2 つの形（[`ModeAccess`]）
//!
//! | モード | 利用者の権利 | 所有者 |
//! | --- | --- | --- |
//! | 設定モード | 「変更」（[`UserRight::Modify`]） | SYSTEM・Administrators・その利用者 |
//! | 運用モード | 「読み取りと実行」（[`UserRight::Read`]） | SYSTEM・Administrators だけ |
//!
//! どちらも保護された DACL（親から継承しない）で、SYSTEM・Administrators にフル。利用者を
//! 決められないとき（0 人）は [`ModeAccess::for_users`] が `None` を返し、呼ぶ側は**何も
//! 変えずに**断る（設計 §2.7 の `NoReader`。SYSTEM・Administrators だけで締めると、シェルが
//! `run\endpoint.json` を読めず運用モードとして使えないうえ、戻すのに UAC が要る）。
//!
//! # 締める（運用モードへ）
//!
//! 1. **1 周目**（`PinnedDir::scan_tree`、読むだけ）: 木の全体を辿らずに確かめ、reparse point
//!    （tag を問わず）・ハードリンク（ファイルでリンク数 2 以上）・EFS で暗号化された物・
//!    開けない子・使用中の子・深すぎる木のどれかがあれば、**何も変えずに**
//!    [`ScanRefusal::Problems`] で断る（見つけた物を全部並べる。呼ぶ側はログに最初の 20 件と
//!    総数を出す）。
//! 2. **2 周目**（`PinnedDir::close_tree`）: **上から順に、締めてから中を列挙する**。締めた後の
//!    フォルダには利用者が名前を作れない（作る・名前を変える・ハードリンク・ジャンクションの
//!    どれも親の `FILE_ADD_FILE`／`FILE_ADD_SUBDIRECTORY` が要る）ので、列挙の結果は以後
//!    増えない。1 周目と 2 周目の間に植えられた物に会ったら、**飛ばさずにその場で止める**
//!    （[`CloseAbort`]）。止めたら呼ぶ側が `relax_tree` で締めた分を緩め直す（PRESHUTDOWN なら
//!    緩めない）。
//! 3. ID で開いた物は、**本当のパスがルートの中にあることを確かめてから**書く（列挙と開くの
//!    間に木の外へ移された物に書かない。`OpenFileById` はボリューム全体の ID で開くので、親の
//!    中にあることは保証されない。Fable-2 A-3）。さらに、ルート以外のフォルダは**部分木を
//!    書き終えた後に本当のパスをもう一度読み、木の中かを確かめる**（締める前から握っていた古い
//!    `DELETE` のハンドルで、書いている最中に木の外へ移される筋。#623 S-1、実測 2026-10-19）。
//!    外なら [`TreeProblem::MovedOut`] で止めて緩め直す。移される前に書いた分（そのフォルダ
//!    自身・印・移される前に開いた子）は移った先に残り、戻せない（緩め直すのは木の中だけ）。
//!    緩める（`relax_tree`、下から・自身を最後に書く）は同じ確かめで記録して続け、移された
//!    フォルダ自身には書かない（書く前に確かめる）。修復（`reset_acl`、上から・自身を先に書く）は
//!    記録して続けるが、気づいた時点でそのフォルダ自身は既に書かれて移った先にある。
//!    確かめと書き込みの間の窓は残る。1 周目は読むだけで、
//!    設定モードの利用者は自分の木を自由に動かせるので確かめない。
//! 4. 所有者はどの物も **Administrators** に移す（利用者が所有者のままだと暗黙の `WRITE_DAC` で
//!    DACL を戻せる）。LocalSystem・昇格した管理者のトークンは Administrators を所有者にできる
//!    属性を持つので特権は要らない（非昇格は `ERROR_INVALID_OWNER`(1307) で何も書かずに
//!    失敗する）。
//!
//! # 緩める（設定モードへ）
//!
//! `PinnedDir::relax_tree` は**下から順、ルートを最後に**書く。これで「ルートが緩んでいる ⇔
//! 全体が緩み終わった」「ルートが締まっている ⇔ 運用モードか切り替えの途中」となり、
//! デスクトップは[`classify_mode`]でルートの DACL だけを見ればよい。この同値は木を書く
//! **すべての書き手**（サービス・昇格ヘルパーの修復・install・uninstall）が同じ順序を守る
//! ときだけ成り立つ（設計 §3.5）。緩めるときに会った reparse point・ハードリンク・開けない子
//! は飛ばして記録し、続ける（締まったまま残る = 安全側）。
//!
//! # 印のファイル（[`MARKER_NAME`]、設計 §2.9 (a)）
//!
//! 締める前から利用者が開いていたハンドルは、締めた後も開いた時点のアクセスのまま効く。
//! 空のフォルダを書き込みで開いていたハンドルは、締めた後に `FSCTL_SET_REPARSE_POINT` で
//! そのフォルダをジャンクションにできる（確かめるのはハンドルのアクセスと「空であること」
//! だけ）。そこで**木のすべてのフォルダ**に、**利用者が消せない空のファイル**（SYSTEM・
//! Administrators だけ、所有者 Administrators、利用者の ACE なし）を置く。空でないフォルダは
//! reparse point にできない。設定モードでも利用者は消せない（「変更」に `FILE_DELETE_CHILD` が
//! 無く、印のファイル自身に DELETE が無い）。
//!
//! - **2 周目**（`close_tree`）は、フォルダに書いた直後・中を列挙する前に印を置く（置けなければ、
//!   その間に古いハンドルで reparse にされたとみなして止める。Codex P1）。締めた直後のフォルダは
//!   こうして**空のまま残らない**ので、以後は古いハンドルでも reparse にできない。既にある印
//!   （前回の運用モードの物・利用者が置いた偽物）は共有 READ だけ・`DELETE` を求めて開き、
//!   利用者が握っていれば止め（`Busy`）、記述子を印の形に書き直す。
//! - `PinnedDir::ensure_marker`（管理するフォルダ。締めた後に `open_managed` が作った物のため）は
//!   同じ部品で置き、直後に握ったハンドルでそのフォルダの reparse 属性を読み直して、印を置く前に
//!   reparse にされていたら [`MarkerError::DirIsReparse`] で止める。
//! - 印は**綴りではなく OS の名前の解決とファイル ID**で見分ける（Fable レビュー M-1・オーナー
//!   レビュー P2）: 各フォルダで、親の列挙に名前のマスク `MARKER_NAME` を付けて（子を開かずに）
//!   席の子の ID を得て、列挙した子の ID と比べる。席の DACL に依らないので、開けない席を後から
//!   開けるようにしても印の同一性を見失わない。利用者が
//!   `.BANTO-SAFE-ROOT` と綴ったフォルダ・ファイルを置いても、フォルダなら 1 周目が
//!   `MarkerOccupied` で断り、ファイルなら 2 周目が印として扱う（利用者の ACE を付けない）。
//!   `ManagedDir::remove` も綴り違いの名前で印を消さない。
//! - 緩めても印の記述子は変えない。印は crate からは消さない（uninstall の昇格ヘルパーが消す）。
//!
//! **残る窓**: 締める前に開いた古いハンドルのうち、`WRITE_DAC`（DACL を戻す）・`FILE_WRITE_DATA`
//! （ファイルの中身）・`DELETE`（消す・木の外へ移す）は印では塞がらない（設計 §2.9 の表。ID で開いた
//! 物の本当のパスの確かめと、パスで渡す直前の `recheck` で狭める）。実測（手元 Windows 11 26300、
//! テスト `an_old_write_dac_handle_still_works_after_tightening_and_the_security_watch_is_measured`
//! が記録する）では、ハンドル経由の `SetKernelObjectSecurity` による DACL の変更は
//! `ReadDirectoryChangesW(FILE_NOTIFY_CHANGE_SECURITY)` に**その場では通知されず、そのハンドルを
//! 閉じたときに遅れて通知された**（パスの `SetNamedSecurityInfoW` は閉じて戻るので直ちに来る）。
//! ただし CI（管理者）ではその場で通知が来ることもあった: 通知の有無は OS・環境（負荷・管理者か・
//! ほかのフィルタドライバ）で変わる観測なので、テストは assert せず記録だけにする（古いハンドルで
//! DACL を戻せること自体は assert）。見張りは補助で、古いハンドルを握り続ける相手には当てにせず、
//! 主は印のファイルと定期の確かめ直しで守る。
//!
//! **既知の制限**: 一度でも運用モードにした木では、すべてのフォルダに印があり、設定モードの
//! 利用者はフォルダをフォルダごと消せない（中身は消せる）。tstore の日次ファイル・LogPrune・
//! banto の backup の一覧・audit spool は名前のパターンで選ぶので印を拾わない。
//!
//! # 記述子の確かめ直し（`PinnedDir::recheck`、設計 §2.9 (b)(d)）
//!
//! パスで渡す直前（`ManagedDir::reopen_path_checked`）に、握ったフォルダの reparse 属性と
//! 記述子をハンドルで読み直し、運用モードの形（`check_dacl`）に通るか確かめる。
//!
//! # 見分け（[`classify_mode`]、設計 §2.10）
//!
//! デスクトップは[`observe_root`]でルートを読むだけで開き（`WRITE_DAC` を求めない）、記述子を
//! [`classify_mode`]に渡す。**Desktop を断るのは [`ModeObservation::Operation`] のときだけ**
//! （どちらの形でもない・自分の ACE が無いは Desktop へ進み、書き込みの試しで落ちたら
//! Fallback。サービスを入れていない Hub の profile（継承の既定の DACL）・旧版の ChronoGazer
//! のルートを起動できなくしないため。Fable-2 A-2）。
//!
//! # 使う側（PR-3／PR-5）の順序
//!
//! 読み取りを付ける相手を決める → `scan_tree`（断ったら終了コード 4）→ `close_tree`（止まったら
//! `relax_tree` して 4、緩め直せなければ 5）→ 管理するフォルダを開いて `ensure_marker` → 運用。
//! STOP で `relax_tree`。どの段でも、crate はログを出さず・サービスのランナーに依存しない
//! （止める合図・進み具合はコールバック）。

use std::path::PathBuf;

use crate::acl::{
    DirAccess, InstallUserSid, ObservedDacl, UserRight, ACCESS_ALLOWED_ACE_TYPE,
    ADMINISTRATORS_SID, FULL_ACCESS_MASK, INHERIT_FLAGS, MODIFY_ACCESS_MASK,
    READ_EXECUTE_ACCESS_MASK, SYSTEM_SID,
};
use crate::error::{AclMismatch, Refusal, SafeRootError};

/// 管理するフォルダとルートに置く印のファイルの名前（設計 §2.9 (a)。中身は空）。
pub const MARKER_NAME: &str = ".banto-safe-root";

/// 製品が渡す「この木の 2 つの形」（設計 §2.2）。同じ利用者の組に、設定モードは「変更」、
/// 運用モードは「読み取りと実行」。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModeAccess {
    setting: DirAccess,
    operation: DirAccess,
}

impl ModeAccess {
    /// `users`（重複はまとめる）から 2 つの形を作る。**空なら `None`**（利用者を決められない
    /// ときは締めない - 設計 §2.7 の `NoReader`。呼ぶ側は何も変えずに断る）。
    pub fn for_users(users: impl IntoIterator<Item = InstallUserSid>) -> Option<Self> {
        let setting = DirAccess::with_install_users(users);
        if setting.install_users().is_empty() {
            return None;
        }
        let operation = setting.to_right(UserRight::Read);
        Some(Self { setting, operation })
    }

    /// 設定モードの形（利用者に「変更」）。
    pub fn setting(&self) -> &DirAccess {
        &self.setting
    }

    /// 運用モードの形（利用者に「読み取りと実行」）。
    pub fn operation(&self) -> &DirAccess {
        &self.operation
    }

    /// 利用者の組（重複なし・指定の順）。
    pub fn users(&self) -> &[InstallUserSid] {
        self.setting.install_users()
    }
}

/// 止める合図（サービスのランナーから。crate はランナーに依存しないのでコールバックで受ける。
/// 設計 §2.11・§3.3）。1 周目・2 周目の各フォルダの境で 1 回見る。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StopSignal {
    /// 続ける。
    None,
    /// SCM の STOP（= 設定モードへ）。2 周目なら止めて、呼ぶ側が緩め直す。
    Stop,
    /// PRESHUTDOWN（OS の終了）。2 周目なら止めて、**緩めずに**終える。
    Preshutdown,
}

/// 進み具合（コールバックで受ける。サービスはこれで SCM の checkpoint を進める。設計 §3.8）。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Progress {
    /// 辿らずに開いて確かめた物の数（ルートを含む）。
    pub examined: u64,
    /// 記述子を書いた物の数（1 周目は 0）。
    pub written: u64,
}

/// 木の中で見つけた、締められない・確かめられない物（設計 §2.3 の表）。`path` は表示用
/// （本当のパス基準。開き直すのには使わない）。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TreeProblem {
    /// reparse point（tag を問わない: ジャンクション・シンボリックリンク・WOF（`compact`）・
    /// 重複除去・クラウドのプレースホルダも。fail closed。`tag` はログで見分けるため）。
    ReparsePoint { path: PathBuf, tag: u32 },
    /// ファイルでリンク数 2 以上（締めても木の外の名前が残る。#624）。
    Hardlink { path: PathBuf, links: u32 },
    /// 開けない（`ERROR_ACCESS_DENIED`。利用者が自分の物から SYSTEM・Administrators を外した。
    /// 特権は有効にしない。1 周目は何も変えずに断るので木は設定モードのまま = 利用者自身が
    /// 権限を戻せる。所有者が別の利用者のときだけ管理者の `takeown`）。
    Denied { path: PathBuf },
    /// 使用中で開けない（共有違反・ロック違反。利用者が `DELETE` を持つハンドルで開いている）。
    Busy { path: PathBuf, os_error: u32 },
    /// EFS で暗号化されている（`FILE_ATTRIBUTE_ENCRYPTED`。設定モードで `cipher /e` を
    /// かけたフォルダの中に SYSTEM が作るファイルは SYSTEM の鍵で暗号化され、設定モードに
    /// 戻った利用者が読めなくなる。Fable-2 C-4）。1 周目・2 周目だけ（緩めるときは見ない）。
    Encrypted { path: PathBuf },
    /// 深さの上限（64 段。`acl` の `MAX_DEPTH`）を超えた。その下は見ていない。
    TooDeep { path: PathBuf },
    /// 印のファイルの名前（[`MARKER_NAME`]）をフォルダが塞いでいて、印を置けない（1 周目・
    /// 2 周目。利用者が設定モードで作った物）。
    MarkerOccupied { path: PathBuf },
    /// 書く歩き方（2 周目・緩める・修復）で、フォルダの部分木を書き終えた後に、そのフォルダの
    /// 本当のパスが**木の外**にあった（締める前から握っていた古い `DELETE` のハンドルで、書いて
    /// いる最中に木の外へ移された。#623 S-1）。`path` は木の中にあったときの表示用のパス、
    /// `now` は確かめたときの本当のパス。2 周目はここで止めて緩め直す（緩め直すのは木の中だけ。
    /// 移された先に書いた分は戻せない）。緩める（下から・自身を最後に書く）は記録して続け、そのフォルダ自身には書かない。
    /// 修復（上から・自身を先に書く）は記録して続けるが、気づいた時点でそのフォルダ自身は既に
    /// 書かれて移った先にある。
    MovedOut { path: PathBuf, now: PathBuf },
}

impl TreeProblem {
    /// 表示用のパス。
    pub fn path(&self) -> &std::path::Path {
        match self {
            Self::ReparsePoint { path, .. }
            | Self::Hardlink { path, .. }
            | Self::Denied { path }
            | Self::Busy { path, .. }
            | Self::Encrypted { path }
            | Self::TooDeep { path }
            | Self::MarkerOccupied { path }
            | Self::MovedOut { path, .. } => path,
        }
    }
}

impl std::fmt::Display for TreeProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ReparsePoint { path, tag } => write!(
                f,
                "{}: リンク（reparse point、tag {tag:#010x}）",
                path.display()
            ),
            Self::Hardlink { path, links } => write!(
                f,
                "{}: ハードリンクの別名があります（リンク数 {links}）",
                path.display()
            ),
            Self::Denied { path } => write!(f, "{}: 開けません（アクセス拒否）", path.display()),
            Self::Busy { path, os_error } => write!(
                f,
                "{}: 使用中で開けません（os error {os_error}）",
                path.display()
            ),
            Self::Encrypted { path } => write!(f, "{}: EFS で暗号化されています", path.display()),
            Self::TooDeep { path } => write!(f, "{}: フォルダの入れ子が深すぎます", path.display()),
            Self::MarkerOccupied { path } => write!(
                f,
                "{}: 印のファイルの名前をフォルダが塞いでいます",
                path.display()
            ),
            Self::MovedOut { path, now } => write!(
                f,
                "{}: 書いている最中に木の外（{}）へ移されました",
                path.display(),
                now.display()
            ),
        }
    }
}

/// 1 周目の結果（何も変えていない）。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ScanReport {
    /// 確かめた物の数（ルートを含む）。
    pub examined: u64,
}

/// 1 周目が断った理由。どれも**何も変えていない**。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScanRefusal {
    /// 見つけた物（1 つ以上。見つけた順）。
    Problems(Vec<TreeProblem>),
    /// 止める合図が来た（途中まで確かめた）。
    Stopped(StopSignal),
    /// 列挙・開く・読むの失敗。
    Io(SafeRootError),
}

/// 2 周目が止まった理由。`written` までは締めてある（呼ぶ側が `relax_tree` で緩め直す）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloseAbort {
    /// 止めるまでに記述子を書いた物の数（0 なら何も変えていない）。
    pub written: u64,
    pub reason: AbortReason,
}

/// [`CloseAbort`] の中身。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AbortReason {
    /// 1 周目と 2 周目の間に植えられた・変えられた物に会った。
    Problem(TreeProblem),
    /// 止める合図が来た。
    Stopped(StopSignal),
    /// 書く・列挙する・開くの失敗（非昇格の `ERROR_INVALID_OWNER`(1307) もここ）。
    Io(SafeRootError),
}

/// 緩めた結果。飛ばした物は締まったまま残る（安全側）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RelaxReport {
    /// 書いた物の数（ルートを含む）。
    pub written: u64,
    /// 飛ばした物（reparse point・ハードリンク・開けない・使用中・深すぎる。締まったまま）。
    pub skipped: Vec<TreeProblem>,
}

/// `PinnedDir::ensure_marker` の結果。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkerOutcome {
    /// 置いた。
    Created,
    /// 既にあった（ファイル・reparse でない・リンク数 1。記述子は印の形に書き直した）。
    Existed,
    /// [`crate::Mode::Plain`] なので置かない（デスクトップの挙動を変えない）。
    Skipped,
}

/// `PinnedDir::ensure_marker` が止めた理由。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MarkerError {
    /// 握ったフォルダ自身が reparse point になっていた（印を置く前に、締める前から開いていた
    /// ハンドルで reparse にされた。改ざんとみなす - 設計 §3.1 の 8 `Tampered`）。
    DirIsReparse { tag: u32 },
    /// [`MARKER_NAME`] の名前に印でない物がある（フォルダ・リンク・ハードリンク）。
    Occupied(Refusal),
    /// 既にある印を誰かが書き込み・削除の権利で開いている（共有違反 32／ロック違反 33。設定
    /// モードで利用者が置いた偽の印を握っている筋）。
    Busy { os_error: u32 },
    /// 作る・書く・読むの失敗。
    Io(SafeRootError),
}

impl std::fmt::Display for MarkerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DirIsReparse { tag } => write!(
                f,
                "フォルダが外からリンク（reparse point、tag {tag:#010x}）に変えられました"
            ),
            Self::Occupied(refusal) => write!(f, "印のファイルの名前が使えません: {refusal}"),
            Self::Busy { os_error } => write!(f, "印のファイルが使用中です（os error {os_error}）"),
            Self::Io(error) => write!(f, "印のファイルを置けませんでした: {error}"),
        }
    }
}

/// `PinnedDir::recheck` が止めた理由。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecheckFailure {
    /// 握ったフォルダ自身が reparse point になっている。
    ReparsePoint { tag: u32 },
    /// フォルダではない。
    NotADirectory,
    /// 記述子が期待の形（運用モード）ではない。
    Acl(AclMismatch),
    /// 読むのに失敗した。
    Io(SafeRootError),
}

impl std::fmt::Display for RecheckFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ReparsePoint { tag } => {
                write!(f, "リンク（reparse point、tag {tag:#010x}）になっています")
            }
            Self::NotADirectory => f.write_str("フォルダではありません"),
            Self::Acl(mismatch) => write!(
                f,
                "フォルダの権限が運用モードの形ではありません: {mismatch}"
            ),
            Self::Io(error) => write!(f, "確かめられませんでした: {error}"),
        }
    }
}

// --- 見分け（デスクトップ） ----------------------------------------------------------

/// デスクトップがルートを読むだけで開いた結果（[`observe_root`]）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RootObservation {
    /// ある。記述子はそのまま（判断は [`classify_mode`]）。
    Present(ObservedDacl),
    /// 無い。
    Absent,
    /// 開けない（`ERROR_ACCESS_DENIED`。自分の ACE が無い形で締まっている）。
    Denied,
}

/// ルートの DACL から見たモード（[`classify_mode`]、設計 §2.10）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModeObservation {
    /// 設定モードの形（保護された DACL・SYSTEM と Administrators が FA・利用者は全員
    /// 「変更」で、自分もその 1 人）。
    Setting,
    /// 運用モードの形（保護された DACL・SYSTEM と Administrators が FA・利用者は全員
    /// 「読み取りと実行」だけで、書き込みの ACE が無い）。自分が入っているかは問わない
    /// （入っていなくても Desktop を起動して書けないのは同じで、案内は「運用モード」が正しい）。
    /// 運用中か、締めたまま止まった。**これのときだけ Desktop を断る。**
    Operation,
    /// 自分の SID の ACE が無い（別の利用者で設定された・利用者の ACE が 1 つも無い）、または
    /// ルートを読めない（拒否）。Desktop へ進み、書き込みの試しで落ちたら Fallback。
    NoAccessForMe,
    /// どちらの形でもない（継承の既定の DACL・手で変えた・旧版で作った等）。中身は最初に
    /// 見つけた食い違い。Desktop へ進み、書き込みの試しで落ちたら Fallback。
    Unexpected(AclMismatch),
    /// ルートがまだ無い。
    Absent,
}

/// ルートの記述子から、デスクトップが見るモードを決める（純関数。設計 §2.10）。
///
/// 見る順: 無い → [`ModeObservation::Absent`]、拒否 → [`ModeObservation::NoAccessForMe`]。
/// ある: null DACL・保護されていない・継承された ACE がある → `Unexpected`。許可以外の
/// 種類・`OICI` でない ACE → `Unexpected`。SYSTEM・Administrators の FA が無い → `Unexpected`
/// （`MissingAce`）。残り（利用者の ACE）のマスクが**すべて**「読み取りと実行」→
/// `Operation`（所有者が SYSTEM・Administrators 以外なら `Unexpected(DisallowedOwner)` -
/// 締めた木の所有者は Administrators のはずで、利用者が所有者なら DACL を戻せる）。
/// **すべて**「変更」→ 自分が入っていれば `Setting`（所有者は SYSTEM・Administrators・
/// 「変更」の利用者のどれか。違えば `Unexpected(DisallowedOwner)`）、入っていなければ
/// `NoAccessForMe`。利用者の ACE が 1 つも無い（SYSTEM・Administrators だけ）→
/// `NoAccessForMe`。マスクが混ざる・別の値 → `Unexpected(UnexpectedAce)`。
pub fn classify_mode(root: &RootObservation, me: &InstallUserSid) -> ModeObservation {
    let dacl = match root {
        RootObservation::Absent => return ModeObservation::Absent,
        RootObservation::Denied => return ModeObservation::NoAccessForMe,
        RootObservation::Present(dacl) => dacl,
    };
    let Some(aces) = &dacl.aces else {
        return ModeObservation::Unexpected(AclMismatch::NoDacl);
    };
    if !dacl.protected {
        return ModeObservation::Unexpected(AclMismatch::NotProtected);
    }
    let mut system_full = false;
    let mut admins_full = false;
    let mut users: Vec<(&str, u32)> = Vec::new();
    for ace in aces {
        if ace.flags & crate::acl::INHERITED_ACE_FLAG != 0 {
            return ModeObservation::Unexpected(AclMismatch::InheritedAce {
                sid: ace.sid.clone(),
            });
        }
        let unexpected = || {
            ModeObservation::Unexpected(AclMismatch::UnexpectedAce {
                ace_type: ace.ace_type,
                flags: ace.flags,
                mask: ace.mask,
                sid: ace.sid.clone(),
            })
        };
        if ace.ace_type != ACCESS_ALLOWED_ACE_TYPE || ace.flags != INHERIT_FLAGS {
            return unexpected();
        }
        match ace.sid.as_str() {
            SYSTEM_SID if ace.mask == FULL_ACCESS_MASK => system_full = true,
            ADMINISTRATORS_SID if ace.mask == FULL_ACCESS_MASK => admins_full = true,
            SYSTEM_SID | ADMINISTRATORS_SID => return unexpected(),
            sid => users.push((sid, ace.mask)),
        }
    }
    if !system_full {
        return ModeObservation::Unexpected(AclMismatch::MissingAce {
            sid: SYSTEM_SID.to_owned(),
            mask: FULL_ACCESS_MASK,
        });
    }
    if !admins_full {
        return ModeObservation::Unexpected(AclMismatch::MissingAce {
            sid: ADMINISTRATORS_SID.to_owned(),
            mask: FULL_ACCESS_MASK,
        });
    }
    if users.is_empty() {
        return ModeObservation::NoAccessForMe;
    }
    let owner_is_service = dacl.owner == SYSTEM_SID || dacl.owner == ADMINISTRATORS_SID;
    if users
        .iter()
        .all(|(_, mask)| *mask == READ_EXECUTE_ACCESS_MASK)
    {
        if !owner_is_service {
            return ModeObservation::Unexpected(AclMismatch::DisallowedOwner {
                sid: dacl.owner.clone(),
            });
        }
        return ModeObservation::Operation;
    }
    if users.iter().all(|(_, mask)| *mask == MODIFY_ACCESS_MASK) {
        if !users.iter().any(|(sid, _)| *sid == me.as_str()) {
            return ModeObservation::NoAccessForMe;
        }
        if !owner_is_service && !users.iter().any(|(sid, _)| *sid == dacl.owner) {
            return ModeObservation::Unexpected(AclMismatch::DisallowedOwner {
                sid: dacl.owner.clone(),
            });
        }
        return ModeObservation::Setting;
    }
    // 混ざっている・別のマスク: 最初の「変更」でも「読み取りと実行」でもない ACE を理由に。
    let odd = users
        .iter()
        .find(|(_, mask)| *mask != READ_EXECUTE_ACCESS_MASK && *mask != MODIFY_ACCESS_MASK)
        .or_else(|| users.iter().find(|(_, mask)| *mask == MODIFY_ACCESS_MASK))
        .expect("混ざっているので 1 つはある");
    ModeObservation::Unexpected(AclMismatch::UnexpectedAce {
        ace_type: ACCESS_ALLOWED_ACE_TYPE,
        flags: INHERIT_FLAGS,
        mask: odd.1,
        sid: odd.0.to_owned(),
    })
}

/// ルートを**読むだけで**開いて（`READ_CONTROL | FILE_READ_ATTRIBUTES`、辿らない、共有は
/// すべて許す）記述子を読む（デスクトップ用。`WRITE_DAC` を求めない。設計 §2.11）。無ければ
/// [`RootObservation::Absent`]、`ERROR_ACCESS_DENIED` は [`RootObservation::Denied`]。開いた物が
/// reparse point・フォルダでないときは [`SafeRootError::Refused`]（デスクトップの Plain は
/// リンクを辿るが、モードの判断はリンクそのものの記述子ではできない）。非 Windows は
/// [`SafeRootError::UnsupportedPlatform`]。
pub fn observe_root(dir: &std::path::Path) -> crate::error::Result<RootObservation> {
    imp::observe_root(dir)
}

#[cfg(windows)]
mod imp {
    use super::RootObservation;
    use crate::acl::imp::{last_error, read_dacl_of_handle, OwnedHandle};
    use crate::error::{IoOp, Result, SafeRootError};
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    pub(super) fn observe_root(dir: &Path) -> Result<RootObservation> {
        use windows_sys::Win32::Foundation::{
            ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, INVALID_HANDLE_VALUE,
        };
        use windows_sys::Win32::Storage::FileSystem::{
            CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
            FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
            OPEN_EXISTING, READ_CONTROL,
        };
        if crate::acl::has_dot_segment(dir) {
            return Err(SafeRootError::DotSegmentInPath);
        }
        let wide: Vec<u16> = dir
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: `wide` は NUL 終端。読むだけ（記述子と属性）、辿らない、共有はすべて許す
        // （サービスが握っている間も邪魔をしない）。
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                READ_CONTROL | FILE_READ_ATTRIBUTES,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            let os_error = last_error();
            if os_error == ERROR_FILE_NOT_FOUND || os_error == ERROR_PATH_NOT_FOUND {
                return Ok(RootObservation::Absent);
            }
            if os_error == ERROR_ACCESS_DENIED {
                return Ok(RootObservation::Denied);
            }
            return Err(SafeRootError::io(
                IoOp::InspectDir,
                &std::io::Error::from_raw_os_error(os_error as i32),
            ));
        }
        let handle = OwnedHandle(handle);
        let facts = crate::handle_io::facts(handle.raw())?;
        if let Some(refusal) = crate::handle_io::dir_refusal(&facts) {
            return Err(SafeRootError::Refused {
                path: dir.to_path_buf(),
                refusal,
            });
        }
        Ok(RootObservation::Present(read_dacl_of_handle(handle.raw())?))
    }
}

#[cfg(not(windows))]
mod imp {
    use super::RootObservation;
    use crate::error::{Result, SafeRootError};
    use std::path::Path;

    pub(super) fn observe_root(_dir: &Path) -> Result<RootObservation> {
        Err(SafeRootError::UnsupportedPlatform)
    }
}

#[cfg(windows)]
mod watch;
#[cfg(windows)]
pub use watch::{SecurityWatch, WatchEvent, WatchStop};

#[cfg(test)]
mod tests;
