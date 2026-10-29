//! OPC UA の自己署名の鍵と証明書の置き場（#634。2026-10-29 オーナー決定「採用する」）。
//!
//! banto-opcua の `ServerConfig::with_pki_dir`（tyaro/banto-opcua#23）に渡す
//! フォルダを決め、**秘密鍵を作る・読む前に**そのフォルダを用意して確かめる。
//! 以前は起動のたびに一時 PKI で RSA 鍵を作っていて遅かった（数秒〜10 秒超）。
//! `with_pki_dir` は初回だけ鍵と証明書を作り、次の起動からは再利用する。
//!
//! ## 置き場（[`pki_dir`]）
//!
//! - profile のフォルダを握って起動したとき（本番）: **profile の `data\` の本当のパス**の
//!   下の `opcua\pki`。`BANTO_HUB_DATA` で tstore の置き場を profile の外へ移していても、
//!   鍵は profile の中に置く（製品のルートや、profile をまたいで共有されうる場所に置かない）。
//!   profile の保護は `crate::profile_grant`（profile の単位）と、サービスの設定モード・
//!   運用モードの切り替え（木の全体）が担う。
//! - profile を握らずに起動したとき（テストなど）: 実効のデータのフォルダの下の
//!   `opcua\pki`。
//!
//! profile は 1 プロセスだけが握る（`crate::profile_lock`）ので、置き場は profile ごとに
//! 分かれ、別のプロセスと重ならない。同じプロセスの中では、`crate::opcua` の起動タスクが
//! 前の起動タスクの終わりを待ってから次を始める（#650 の起動の直列化）ので、同じ置き場への
//! 起動が重ならない（banto-opcua#23 の「同じ `pki_dir` を同時に起動する複数のサーバーで
//! 共有しない」を満たす）。
//!
//! ## 鍵を作る前の確かめ（[`prepare`]）
//!
//! - **Windows**: 置き場が無ければ、保護された DACL（SYSTEM・Administrators のフルと、
//!   このプロセスの利用者の「変更」。サービス（LocalSystem）なら SYSTEM・Administrators
//!   だけ）で作る（`banto_safe_root::acl::create_dir_if_absent`。作ってから絞るまでの間に
//!   広い権限で見える時間を作らない）。その後、置き場と中の子のすべて（既にある鍵・証明書を
//!   含む）を辿らずに開いて記述子を読み（`inspect_file`）、**許可リスト**で確かめる
//!   （#943 のレビュー P1）。許可リストは次の相手:
//!   1. SYSTEM・Administrators（と、`exposure` が同じく信頼する TrustedInstaller）。
//!   2. このプロセスのトークンの利用者（実行しているアカウント）。
//!   3. **明示的に許可された profile の利用者**: 握った profile のフォルダ自身の DACL で、
//!      普通の許可の ACE（`INHERIT_ONLY` でないもの）を持つ利用者のアカウント（SID が
//!      `S-1-5-21-…`・`S-1-12-1-…` で、`check_user_account`（`LookupAccountSidW` の
//!      `SidTypeUser`）が通るもの。グループ・well-known は入れない）。profile の DACL は
//!      `grant-profile-acl`・サービスの設定モード ⇔ 運用モードの切り替えが決める（どちらも
//!      profile のフォルダを根に、木の全体へ同じ利用者を書く）。デスクトップだけで使う継承の
//!      profile では空になりうる（そのときは 1・2 だけ）。profile を握らずに起動したとき
//!      （テストなど）も 1・2 だけ。
//!
//!   次のものがあれば**鍵を作らず・読まずに**起動を断る:
//!   - 許可リストにない相手への**許可**の ACE（明示・継承・`INHERIT_ONLY` を問わず、マスクも
//!     問わない。多めに断る側に倒す）。`CREATOR OWNER`（`S-1-3-0`）などの継承専用の相手も、
//!     banto-safe-root が書く DACL（`create_dir_if_absent`・`relax_tree`・`close_tree`・
//!     `service_only`）には現れないので断る。広いグループ（[`BROAD_SIDS`] と、ドメインの
//!     Domain Users・Domain Guests・Domain Computers）は、理由の文言でその名前を出す。
//!   - 許可リストにない所有者（所有者は DACL に関わらず ACL を書き換えられる）。
//!   - null DACL・種類の分からない ACE・SID の読めない ACE。
//!   - reparse point（ジャンクション・シンボリックリンク）・ハードリンク（リンク数 2 以上）。
//!
//!   拒否の ACE は断らない（読める相手を減らすだけなので）。profile のフォルダの DACL を
//!   読めないときも断る（許可リストを決められないため）。
//! - **Unix**: banto-opcua が鍵のフォルダを 0700・鍵を 0600 にする。ここでは、既にある鍵が
//!   グループ・他人に開いている（`mode & 0o077`）、置き場・`own`・`private` と
//!   証明書がグループ・他人に書ける（`mode & 0o022`）、どれかがシンボリックリンクなら断る。
//!   **所有者**も確かめる（#943 のレビュー P1）: 置き場・`own`・`private`・証明書・鍵の
//!   所有者の UID が、実行している利用者の実効 UID と違えば断る（別の利用者が持つ `0700` の
//!   フォルダ・`0600` の鍵を、root で動く Hub が使い回さない・その所有者に読ませない）。
//!   親の `opcua` は、所有者が実行している利用者か root で、グループ・他人に書けないこと。
//!   実効 UID は、既にある依存の `sysinfo` で自分のプロセスの実効 UID を読む（`libc` を
//!   足さない。読めなければ断る）。置き場がなければ 0700 で作る（既にある置き場の権限は
//!   書き換えない）。
//!
//! ### 親を辿らずに作る・確かめる（両方の OS。#943 のレビュー P2）
//!
//! データのフォルダから `opcua`、`opcua/pki` へと 1 段ずつ、**最後の要素のリンクを辿らずに**
//! 確かめながら作る（`create_dir_all` で辿らない）。各段がリンク（Unix はシンボリックリンク、
//! Windows は reparse point = ジャンクション・シンボリックリンクなど）か、フォルダでなければ
//! 断る。無ければ作り（Unix は 0700、Windows の `opcua` は親から継承・`pki` は上の保護された
//! DACL）、作ったあともう一度確かめる。最後に、置き場の本当のパス（`canonicalize`。Windows では
//! `GetFinalPathNameByHandleW`）が、データのフォルダの本当のパスの下にあることを確かめる
//! （別の profile などの外を指していれば断る）。
//!
//! - **データのフォルダ自身とその上は確かめない**: 本番ではデータのフォルダは握った profile の
//!   `data\` の本当のパスで、profile の仕組み（`crate::profile_lock`・`crate::profile_grant`）が
//!   起動時に確かめてある、という前提に立つ。profile を握らないとき（テストなど）は、実効の
//!   データのフォルダを `create_dir_all` で作る。
//! - Windows で `pin_dir`（ハンドルを握って相対で作る）を使わないのは、それが
//!   `WRITE_DAC | WRITE_OWNER` を求めるため（設定モードの profile では、デスクトップの利用者は
//!   「変更」しか持たず開けない）。代わりに、読むだけの権利で辿らずに開く `inspect_file` で
//!   各段を確かめる。確かめから使うまでの間のすり替えは、下の競合と同じく防がない。
//!
//! 断ったとき・banto-opcua の `Error::Pki` のときは、理由をログと状態の API
//! （`GET /api/opcua-status` の `failed`）に出す（`crate::opcua`）。原因を直したあと、
//! 同じ設定をもう一度保存すれば再試行になる。置き場を消せば、次の起動で保護された形で
//! 作り直し、鍵と証明書も作り直す（クライアントは新しい証明書を信頼し直す）。
//!
//! 確かめてから banto-opcua が鍵を作るまでの間に ACL を変えられる競合は防がない
//! （置き場は保護された DACL なので、変えられるのは SYSTEM・Administrators・
//! 許可された利用者だけ）。profile のフォルダは本当のパスの最後の要素を辿らずに開いて
//! 読むが、途中の要素は辿る（profile は起動時に握って確かめてある）。

use std::path::{Path, PathBuf};

/// 置き場の、データのフォルダからの相対（`opcua/pki`）。
const PKI_SUBDIRS: [&str; 2] = ["opcua", "pki"];

/// データのフォルダ `data_dir` の下の、OPC UA の鍵と証明書の置き場。
pub(crate) fn pki_dir(data_dir: &Path) -> PathBuf {
    PKI_SUBDIRS
        .iter()
        .fold(data_dir.to_path_buf(), |dir, name| dir.join(name))
}

/// 広いグループの SID（Windows）。許可リストの外の相手のうち、断る理由の文言で名前を出す
/// もの。profile の DACL にあっても許可リストに入れない。
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) const BROAD_SIDS: [(&str, &str); 20] = [
    ("S-1-1-0", "Everyone"),
    ("S-1-2-0", "LOCAL"),
    ("S-1-2-1", "CONSOLE LOGON"),
    ("S-1-5-1", "DIALUP"),
    ("S-1-5-2", "NETWORK"),
    ("S-1-5-3", "BATCH"),
    ("S-1-5-4", "INTERACTIVE"),
    ("S-1-5-6", "SERVICE"),
    ("S-1-5-7", "ANONYMOUS LOGON"),
    ("S-1-5-11", "Authenticated Users"),
    ("S-1-5-13", "TERMINAL SERVER USER"),
    ("S-1-5-14", "REMOTE INTERACTIVE LOGON"),
    ("S-1-5-32-545", "Users"),
    ("S-1-5-32-546", "Guests"),
    ("S-1-5-32-547", "Power Users"),
    ("S-1-5-32-555", "Remote Desktop Users"),
    ("S-1-5-113", "Local account"),
    (
        "S-1-5-114",
        "Local account and member of Administrators group",
    ),
    ("S-1-15-2-1", "ALL APPLICATION PACKAGES"),
    ("S-1-15-2-2", "ALL RESTRICTED APPLICATION PACKAGES"),
];

/// `sid` が広いグループなら、その名前（表示用）。
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn broad_group_name(sid: &str) -> Option<&'static str> {
    if let Some((_, name)) = BROAD_SIDS.iter().find(|(broad, _)| *broad == sid) {
        return Some(name);
    }
    // ドメインの相対 ID: 513 = Domain Users、514 = Domain Guests、515 = Domain Computers。
    let rest = sid.strip_prefix("S-1-5-21-")?;
    match rest.rsplit('-').next()? {
        "513" => Some("Domain Users"),
        "514" => Some("Domain Guests"),
        "515" => Some("Domain Computers"),
        _ => None,
    }
}

/// 鍵と証明書の置き場 `dir` を用意し、許されていない利用者が鍵を読めない（書けない）
/// ことを確かめる（モジュール doc「鍵を作る前の確かめ」）。`profile_dir` は握った profile の
/// フォルダ（本当のパス）で、その DACL で許可されている利用者も許可リストに入る（Windows）。
/// 断るときは、ログと状態の API に出す理由（直し方つき）を返す。同期の I/O なので、
/// blocking 用のスレッドで呼ぶ。
pub(crate) fn prepare(dir: &Path, profile_dir: Option<&Path>) -> Result<(), String> {
    imp::prepare(dir, profile_dir).map_err(|reason| {
        format!(
            "OPC UA の鍵と証明書の置き場（{}）を使えないため、鍵を作らず（読まず）に止めました: {reason}。原因を直してから、同じ設定でもう一度保存すると再試行します（置き場を消すと、次の起動で保護された形で作り直し、鍵と証明書も作り直します）",
            dir.display()
        )
    })
}

/// 置き場 `dir`（[`pki_dir`] の形）から、データのフォルダを取る。`…/opcua/pki` で
/// 終わっていなければ断る（1 段ずつ辿って確かめる範囲を決められない）。
#[cfg_attr(not(any(windows, unix)), allow(dead_code))]
fn data_dir_of(dir: &Path) -> Result<&Path, String> {
    let mut data = dir;
    for name in PKI_SUBDIRS.iter().rev() {
        if data.file_name().and_then(|n| n.to_str()) != Some(name) {
            return Err(format!(
                "置き場のパスが `…{sep}opcua{sep}pki` の形ではありません",
                sep = std::path::MAIN_SEPARATOR
            ));
        }
        data = data
            .parent()
            .ok_or_else(|| "置き場の親のデータのフォルダがありません".to_owned())?;
    }
    Ok(data)
}

/// 置き場の本当のパス `pki_real` が、データのフォルダの本当のパス `data_real` の下（同じ
/// パスは除く）にあるかを確かめる純関数（モジュール doc「親を辿らずに作る・確かめる」）。
#[cfg_attr(not(any(windows, unix)), allow(dead_code))]
fn ensure_inside(data_real: &Path, pki_real: &Path) -> Result<(), String> {
    if pki_real != data_real && pki_real.starts_with(data_real) {
        Ok(())
    } else {
        Err(format!(
            "置き場の本当のパス（{}）が、データのフォルダの本当のパス（{}）の下にありません（リンクで profile の外を指しています）",
            pki_real.display(),
            data_real.display()
        ))
    }
}

/// データのフォルダ `data_dir` と置き場 `dir` の本当のパスを読み、[`ensure_inside`] で
/// 確かめる（Windows の `canonicalize` は `GetFinalPathNameByHandleW`）。
#[cfg_attr(not(any(windows, unix)), allow(dead_code))]
fn check_inside(data_dir: &Path, dir: &Path) -> Result<(), String> {
    let real = |path: &Path| {
        std::fs::canonicalize(path)
            .map_err(|err| format!("{} の本当のパスを読めません: {err}", path.display()))
    };
    ensure_inside(&real(data_dir)?, &real(dir)?)
}

#[cfg(windows)]
mod imp {
    use std::path::Path;

    use banto_safe_root::acl::{self, Exposure, InstallUserSid, ObservedDacl};
    use banto_safe_root::DirAccess;

    /// 辿る子の数・深さの上限（置き場の中は async-opcua の数個のフォルダだけ）。
    const MAX_ENTRIES: usize = 1024;
    const MAX_DEPTH: usize = 8;

    /// `ACE_HEADER.AceFlags` の `INHERIT_ONLY_ACE`・`INHERITED_ACE`。
    const INHERIT_ONLY_ACE: u8 = 0x08;
    const INHERITED_ACE: u8 = 0x10;

    pub(super) fn prepare(dir: &Path, profile_dir: Option<&Path>) -> Result<(), String> {
        // `./data` のような `.` の要素は `inspect_file` が断るので、先に絶対パスにする
        // （Windows では `GetFullPathNameW` で `.`・`..` も畳む）。
        let dir =
            std::path::absolute(dir).map_err(|err| format!("パスを絶対パスにできません: {err}"))?;
        let data_dir = super::data_dir_of(&dir)?;
        // データのフォルダとその上は profile の仕組みが確かめてある前提（モジュール doc）。
        std::fs::create_dir_all(data_dir).map_err(|err| {
            format!(
                "データのフォルダ（{}）を作れません: {err}",
                data_dir.display()
            )
        })?;
        // このプロセスの利用者（デスクトップ）には「変更」。LocalSystem など利用者の
        // アカウントでなければ SYSTEM・Administrators だけ。
        let me = acl::current_user_sid().ok();
        let access = me
            .clone()
            .map(DirAccess::with_install_user)
            .unwrap_or_else(DirAccess::service_only);
        // `opcua` → `opcua\pki` を 1 段ずつ、辿らずに確かめながら作る（#943 のレビュー P2）。
        let opcua = dir.parent().expect("data_dir_of が確かめた");
        ensure_dir_step(opcua, || match std::fs::create_dir(opcua) {
            Err(err) if err.kind() != std::io::ErrorKind::AlreadyExists => Err(err.to_string()),
            _ => Ok(()),
        })?;
        ensure_dir_step(&dir, || {
            acl::create_dir_if_absent(&dir, &access)
                .map(|_| ())
                .map_err(|err| format!("保護された置き場を作れません: {err}"))
        })?;
        super::check_inside(data_dir, &dir)?;
        // 許可リストの 2・3（1 は `exposure` が理由にしない）。
        let mut allowed: Vec<String> = me.iter().map(|sid| sid.as_str().to_owned()).collect();
        if let Some(profile) = profile_dir {
            for user in profile_users(profile)? {
                if !allowed.contains(&user) {
                    allowed.push(user);
                }
            }
        }
        let mut problems = Vec::new();
        let mut count = 0;
        walk(&dir, 0, &mut count, &allowed, &mut problems)?;
        if problems.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "許されていない利用者が読める（書ける）状態です（{}）",
                problems.join(" / ")
            ))
        }
    }

    /// `path` を辿らずに開いて確かめ（`inspect_file`）、無ければ `create` で作ってもう一度
    /// 確かめる。reparse point・フォルダでないものは断る（モジュール doc「親を辿らずに作る・
    /// 確かめる」）。
    fn ensure_dir_step(
        path: &Path,
        create: impl FnOnce() -> Result<(), String>,
    ) -> Result<(), String> {
        let inspect = || {
            acl::inspect_file(path)
                .map_err(|err| format!("{} を確かめられません: {err}", path.display()))
        };
        let security = match inspect()? {
            Some(security) => security,
            None => {
                create().map_err(|err| format!("{} を作れません: {err}", path.display()))?;
                inspect()?
                    .ok_or_else(|| format!("{} を作ったのに見つかりません", path.display()))?
            }
        };
        if security.is_reparse_point {
            return Err(format!(
                "{} がリンク（reparse point。ジャンクション・シンボリックリンクなど）です。リンクを消して、置き場を profile の中に作り直させてください",
                path.display()
            ));
        }
        if !security.is_dir {
            return Err(format!("{} がフォルダではありません", path.display()));
        }
        Ok(())
    }

    /// profile のフォルダ自身の DACL で許可されている利用者のアカウント（モジュール doc の
    /// 許可リストの 3）。読めなければ断る（許可リストを決められない）。
    fn profile_users(profile: &Path) -> Result<Vec<String>, String> {
        let fail = |reason: String| {
            format!(
                "profile のフォルダ（{}）の権限を読めず、許可する利用者を決められません: {reason}",
                profile.display()
            )
        };
        let security = acl::inspect_file(profile)
            .map_err(|err| fail(err.to_string()))?
            .ok_or_else(|| fail("見つかりません".to_owned()))?;
        if security.is_reparse_point || !security.is_dir {
            return Err(fail(
                "リンク（reparse point）か、フォルダではありません".to_owned(),
            ));
        }
        Ok(profile_user_candidates(&security.dacl)
            .into_iter()
            .filter(|sid| {
                InstallUserSid::parse(sid)
                    .and_then(|sid| acl::check_user_account(&sid))
                    .is_ok()
            })
            .collect())
    }

    /// profile の DACL から、許可リストの 3 の候補（利用者のアカウントかの確かめの前）を取る
    /// 純関数: 普通の許可の ACE（条件付き・オブジェクトの許可は入れない）で、`INHERIT_ONLY`
    /// でなく、SID が利用者のアカウントの形（`S-1-5-21-…`・`S-1-12-1-…`）で、広いグループ
    /// （Domain Users など）でないもの。null DACL なら空。
    pub(super) fn profile_user_candidates(dacl: &ObservedDacl) -> Vec<String> {
        let mut users: Vec<String> = Vec::new();
        for ace in dacl.aces.iter().flatten() {
            let candidate = ace.ace_type == acl::ACCESS_ALLOWED_ACE_TYPE
                && ace.flags & INHERIT_ONLY_ACE == 0
                && InstallUserSid::parse(&ace.sid).is_ok()
                && super::broad_group_name(&ace.sid).is_none();
            if candidate && !users.contains(&ace.sid) {
                users.push(ace.sid.clone());
            }
        }
        users
    }

    /// `path` とその中を辿らずに確かめ、断る理由を `problems` に足す。
    fn walk(
        path: &Path,
        depth: usize,
        count: &mut usize,
        allowed: &[String],
        problems: &mut Vec<String>,
    ) -> Result<(), String> {
        *count += 1;
        if *count > MAX_ENTRIES {
            return Err(format!("中の物が多すぎます（{MAX_ENTRIES} を超えました）"));
        }
        let Some(security) = acl::inspect_file(path)
            .map_err(|err| format!("{} の権限を読めません: {err}", path.display()))?
        else {
            // 確かめの間に消えた。
            return Ok(());
        };
        for exposure in acl::exposure(&security) {
            if let Some(reason) = refusal(&exposure, allowed) {
                problems.push(format!("{}: {reason}", path.display()));
            }
        }
        if security.is_dir && !security.is_reparse_point {
            if depth >= MAX_DEPTH {
                return Err(format!("{} の階層が深すぎます", path.display()));
            }
            let entries = std::fs::read_dir(path)
                .map_err(|err| format!("{} の中を読めません: {err}", path.display()))?;
            for entry in entries {
                let entry =
                    entry.map_err(|err| format!("{} の中を読めません: {err}", path.display()))?;
                walk(&entry.path(), depth + 1, count, allowed, problems)?;
            }
        }
        Ok(())
    }

    /// 相手の表示（広いグループなら名前つき）。
    fn describe(sid: &str) -> String {
        match super::broad_group_name(sid) {
            Some(name) => format!("広いグループ {name}（{sid}）"),
            None => format!("許可リストにない相手（{sid}）"),
        }
    }

    /// `exposure` の理由のうち、鍵の置き場で断るもの（モジュール doc）。`allowed` は許可リストの
    /// 2・3（実行しているアカウントと profile の利用者）。1（SYSTEM・Administrators・
    /// TrustedInstaller）は `exposure` が理由にしない。
    pub(super) fn refusal(exposure: &Exposure, allowed: &[String]) -> Option<String> {
        let is_allowed = |sid: &str| allowed.iter().any(|user| user == sid);
        match exposure {
            Exposure::ReparsePoint => Some("リンク（reparse point）です".to_owned()),
            Exposure::HardLinked(links) => {
                Some(format!("ハードリンクです（名前が {links} 個あります）"))
            }
            Exposure::ForeignOwner(sid) => {
                (!is_allowed(sid)).then(|| format!("所有者が{}です", describe(sid)))
            }
            Exposure::NullDacl => Some("DACL が無く、全員に開いています".to_owned()),
            Exposure::AllowsOther {
                flags, mask, sid, ..
            } => (!is_allowed(sid)).then(|| {
                let mut kind = String::new();
                if flags & INHERITED_ACE != 0 {
                    kind.push_str("・継承");
                }
                if flags & INHERIT_ONLY_ACE != 0 {
                    kind.push_str("・継承専用");
                }
                format!(
                    "{}への許可の ACE があります（mask {mask:#x}{kind}）",
                    describe(sid)
                )
            }),
            Exposure::UnknownAce { ace_type, .. } => Some(format!(
                "種類の分からない ACE があります（種類 {ace_type}）"
            )),
            Exposure::UnreadableSid { ace_type, .. } => {
                Some(format!("SID の読めない ACE があります（種類 {ace_type}）"))
            }
        }
    }
}

#[cfg(unix)]
mod imp {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    use std::path::Path;

    /// root の UID。
    const ROOT_UID: u32 = 0;

    /// 実行している利用者の実効 UID。既にある依存の `sysinfo` で、自分のプロセスの実効 UID を
    /// 読む（Linux は `/proc/self/status` の `Uid:` の 2 つ目）。`libc` を足さないため
    /// （`Cargo.lock` を変えない）。読めなければ断る（所有者を確かめられない）。
    fn effective_uid() -> Result<u32, String> {
        use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
        let fail = |reason: &str| format!("実行している利用者の UID を読めません（{reason}）");
        let pid = sysinfo::get_current_pid().map_err(fail)?;
        let mut system = System::new();
        system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[pid]),
            false,
            ProcessRefreshKind::nothing().with_user(UpdateKind::Always),
        );
        let uid = system
            .process(pid)
            .and_then(|process| process.effective_user_id())
            .ok_or_else(|| fail("自分のプロセスの実効 UID が取れません"))?;
        Ok(**uid)
    }

    /// 所有者の UID `owner` を、実行している利用者の実効 UID `me` と比べる純関数（#943 の
    /// レビュー P1）。`root_ok` なら root（UID 0）の所有も通す（親の `opcua`）。断るときは
    /// 直し方つきの理由を返す。
    pub(super) fn owner_refusal(path: &Path, owner: u32, me: u32, root_ok: bool) -> Option<String> {
        if owner == me || (root_ok && owner == ROOT_UID) {
            return None;
        }
        Some(format!(
            "{} の所有者（UID {owner}）が、実行している利用者（UID {me}）ではありません{}。`chown {me} {}` で直すか、置き場を消して作り直させてください",
            path.display(),
            if root_ok { "・root でもありません" } else { "" },
            path.display()
        ))
    }

    /// `path` を辿らずに確かめ（`symlink_metadata`）、無ければ 0700 で作ってもう一度確かめる。
    /// シンボリックリンク・フォルダでないものは断る（モジュール doc「親を辿らずに作る・
    /// 確かめる」）。umask に左右されないよう、作ったときは確かめたあと明示的に 0700 にする。
    /// 既にあるフォルダの権限は書き換えず、広ければ呼ぶ側の確かめで断る。
    fn ensure_dir_step(path: &Path) -> Result<(), String> {
        let fail = |err: std::io::Error| format!("{} を作れません: {err}", path.display());
        let created = match std::fs::symlink_metadata(path) {
            Ok(_) => false,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                match std::fs::DirBuilder::new().mode(0o700).create(path) {
                    Ok(()) => true,
                    Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => false,
                    Err(err) => return Err(fail(err)),
                }
            }
            Err(err) => return Err(format!("{} を確かめられません: {err}", path.display())),
        };
        let metadata = std::fs::symlink_metadata(path)
            .map_err(|err| format!("{} を確かめられません: {err}", path.display()))?;
        if metadata.file_type().is_symlink() {
            return Err(format!(
                "{} がシンボリックリンクです。リンクを消して、置き場を profile の中に作り直させてください",
                path.display()
            ));
        }
        if !metadata.is_dir() {
            return Err(format!("{} がフォルダではありません", path.display()));
        }
        if created {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).map_err(fail)?;
        }
        Ok(())
    }

    /// 権限が広すぎるときの理由（`forbidden` のビットが立っていれば）。
    fn mode_refusal(path: &Path, mode: u32, forbidden: u32, is_dir: bool) -> Option<String> {
        if mode & forbidden == 0 {
            return None;
        }
        let fix = if is_dir { "chmod 700" } else { "chmod 600" };
        Some(format!(
            "{} の権限が広すぎます（{:o}。グループ・他人が書込み{}できます）。`{fix} {}` で絞ってください",
            path.display(),
            mode & 0o777,
            if forbidden == 0o077 { "・読取り" } else { "" },
            path.display()
        ))
    }

    pub(super) fn prepare(dir: &Path, _profile_dir: Option<&Path>) -> Result<(), String> {
        let data_dir = super::data_dir_of(dir)?;
        // データのフォルダとその上は profile の仕組みが確かめてある前提（モジュール doc）。
        std::fs::create_dir_all(data_dir).map_err(|err| {
            format!(
                "データのフォルダ（{}）を作れません: {err}",
                data_dir.display()
            )
        })?;
        let me = effective_uid()?;
        // `opcua` → `opcua/pki` を 1 段ずつ、辿らずに確かめながら作る（#943 のレビュー P2）。
        let opcua = dir.parent().expect("data_dir_of が確かめた");
        ensure_dir_step(opcua)?;
        let metadata = std::fs::symlink_metadata(opcua)
            .map_err(|err| format!("{} を確かめられません: {err}", opcua.display()))?;
        if let Some(reason) = owner_refusal(opcua, metadata.uid(), me, true)
            .or_else(|| mode_refusal(opcua, metadata.permissions().mode(), 0o022, true))
        {
            return Err(reason);
        }
        ensure_dir_step(dir)?;
        super::check_inside(data_dir, dir)?;

        let key = dir.join("private").join("private.pem");
        let cert = dir.join("own").join("cert.der");
        // ディレクトリ・証明書はグループ/他人の書込み（0o022）、鍵はグループ/他人の
        // あらゆる権限（0o077）を断る。所有者はどれも実行している利用者に限る。
        for (path, forbidden, is_dir) in [
            (dir.to_path_buf(), 0o022, true),
            (dir.join("own"), 0o022, true),
            (dir.join("private"), 0o022, true),
            (cert, 0o022, false),
            (key, 0o077, false),
        ] {
            let metadata = match std::fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                Err(err) => return Err(format!("{} を確かめられません: {err}", path.display())),
            };
            if metadata.file_type().is_symlink() {
                return Err(format!("{} がシンボリックリンクです", path.display()));
            }
            if let Some(reason) = owner_refusal(&path, metadata.uid(), me, false)
                .or_else(|| mode_refusal(&path, metadata.permissions().mode(), forbidden, is_dir))
            {
                return Err(reason);
            }
        }
        Ok(())
    }
}

#[cfg(not(any(windows, unix)))]
mod imp {
    pub(super) fn prepare(
        _dir: &std::path::Path,
        _profile_dir: Option<&std::path::Path>,
    ) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pki_dir_is_under_the_data_dir() {
        let data = Path::new("data-root");
        assert_eq!(pki_dir(data), data.join("opcua").join("pki"));
    }

    #[test]
    fn broad_groups_are_named_and_specific_users_are_not() {
        assert_eq!(broad_group_name("S-1-5-32-545"), Some("Users"));
        assert_eq!(broad_group_name("S-1-1-0"), Some("Everyone"));
        assert_eq!(broad_group_name("S-1-5-11"), Some("Authenticated Users"));
        assert_eq!(broad_group_name("S-1-5-21-1-2-3-513"), Some("Domain Users"));
        assert_eq!(broad_group_name("S-1-5-21-1-2-3-1001"), None);
        assert_eq!(broad_group_name("S-1-5-18"), None);
        assert_eq!(broad_group_name("S-1-5-32-544"), None);
    }

    /// 新しい置き場は保護された形で作られ、断られない。2 回目もそのまま通る。
    #[test]
    fn a_fresh_pki_dir_is_created_and_accepted() {
        let root = crate::test_support::TempDir::new("opcua-pki-fresh");
        let dir = pki_dir(&root.path().join("data"));
        prepare(&dir, None).expect("fresh pki dir");
        assert!(dir.is_dir());
        prepare(&dir, None).expect("existing pki dir");
    }

    #[cfg(windows)]
    fn icacls(path: &Path, args: &[&str]) {
        let output = std::process::Command::new("icacls")
            .arg(path)
            .args(args)
            .output()
            .expect("icacls");
        assert!(output.status.success(), "icacls {args:?}: {output:?}");
    }

    /// 既にある鍵に Users への明示の許可があれば断り、理由に Users と鍵のパスが出る。
    /// ACE を外すと通る。
    #[cfg(windows)]
    #[test]
    fn an_explicit_users_ace_on_an_existing_key_is_refused() {
        let root = crate::test_support::TempDir::new("opcua-pki-users-ace");
        let dir = pki_dir(&root.path().join("data"));
        prepare(&dir, None).expect("fresh pki dir");
        let key = dir.join("private").join("private.pem");
        std::fs::create_dir_all(key.parent().unwrap()).unwrap();
        std::fs::write(&key, b"dummy").unwrap();
        icacls(&key, &["/grant", "*S-1-5-32-545:(R)"]);

        let reason = prepare(&dir, None).expect_err("Users can read the key");
        assert!(reason.contains("Users（S-1-5-32-545）"), "{reason}");
        assert!(reason.contains("private.pem"), "{reason}");
        assert!(reason.contains("鍵を作らず"), "{reason}");

        icacls(&key, &["/remove:g", "*S-1-5-32-545"]);
        prepare(&dir, None).expect("ACE removed");
    }

    /// 既にある置き場が Everyone に継承で開いていれば（子にも伝わる）断る。
    #[cfg(windows)]
    #[test]
    fn an_inheritable_everyone_ace_on_the_pki_dir_is_refused() {
        let root = crate::test_support::TempDir::new("opcua-pki-everyone");
        let dir = pki_dir(&root.path().join("data"));
        std::fs::create_dir_all(&dir).unwrap();
        icacls(&dir, &["/grant", "*S-1-1-0:(OI)(CI)(RX)"]);
        let reason = prepare(&dir, None).expect_err("Everyone can read");
        assert!(reason.contains("Everyone（S-1-1-0）"), "{reason}");
    }

    #[cfg(windows)]
    #[test]
    fn exposures_outside_the_allowlist_are_refused() {
        use banto_safe_root::acl::Exposure;
        const OTHER: &str = "S-1-5-21-1-2-3-1001";
        const ALLOWED: &str = "S-1-5-21-1-2-3-1002";
        let allowed = [ALLOWED.to_owned()];
        assert!(imp::refusal(&Exposure::ReparsePoint, &allowed).is_some());
        assert!(imp::refusal(&Exposure::HardLinked(2), &allowed).is_some());
        assert!(imp::refusal(&Exposure::NullDacl, &allowed).is_some());
        // 所有者: 許可リストの外は断り、中は通す。
        let reason =
            imp::refusal(&Exposure::ForeignOwner(OTHER.into()), &allowed).expect("foreign owner");
        assert!(
            reason.contains(&format!("許可リストにない相手（{OTHER}）")),
            "{reason}"
        );
        assert!(imp::refusal(&Exposure::ForeignOwner(ALLOWED.into()), &allowed).is_none());
        let reason = imp::refusal(&Exposure::ForeignOwner("S-1-5-32-545".into()), &allowed)
            .expect("broad owner");
        assert!(reason.contains("広いグループ Users"), "{reason}");
        // 許可の ACE: 許可リストの外は、明示・継承・継承専用を問わず断る。
        for flags in [0x00, 0x03, 0x13, 0x0b] {
            let allows = |sid: &str| Exposure::AllowsOther {
                ace_type: 0,
                flags,
                mask: 0x1200a9,
                sid: sid.into(),
            };
            assert!(
                imp::refusal(&allows(OTHER), &allowed).is_some(),
                "{flags:#x}"
            );
            assert!(
                imp::refusal(&allows(ALLOWED), &allowed).is_none(),
                "{flags:#x}"
            );
        }
        // CREATOR OWNER の継承専用の ACE（banto-safe-root は書かない）も断る。
        let reason = imp::refusal(
            &Exposure::AllowsOther {
                ace_type: 0,
                flags: 0x0b,
                mask: 0x1f01ff,
                sid: "S-1-3-0".into(),
            },
            &allowed,
        )
        .expect("creator owner");
        assert!(
            reason.contains("S-1-3-0") && reason.contains("継承専用"),
            "{reason}"
        );
    }

    /// profile の DACL から許可リストに入れる候補: 普通の許可の ACE で、継承専用でなく、
    /// 利用者のアカウントの形で、広いグループでないものだけ。
    #[cfg(windows)]
    #[test]
    fn profile_user_candidates_are_plain_allows_for_user_accounts() {
        use banto_safe_root::acl::{ObservedAce, ObservedDacl};
        let ace = |ace_type: u8, flags: u8, sid: &str| ObservedAce {
            ace_type,
            flags,
            mask: 0x1301bf,
            sid: sid.into(),
        };
        let dacl = ObservedDacl {
            protected: true,
            owner: "S-1-5-32-544".into(),
            aces: Some(vec![
                ace(0, 0x03, "S-1-5-18"),
                ace(0, 0x03, "S-1-5-32-544"),
                ace(0, 0x03, "S-1-5-21-1-2-3-1001"),
                ace(0, 0x13, "S-1-5-21-1-2-3-1001"), // 重複はまとめる
                ace(0, 0x00, "S-1-12-1-4-5-6-7"),
                ace(0, 0x0b, "S-1-5-21-1-2-3-1003"), // 継承専用
                ace(1, 0x03, "S-1-5-21-1-2-3-1004"), // 拒否
                ace(9, 0x03, "S-1-5-21-1-2-3-1005"), // 条件付きの許可
                ace(0, 0x03, "S-1-5-21-1-2-3-513"),  // Domain Users
                ace(0, 0x03, "S-1-5-32-545"),
                ace(0, 0x0b, "S-1-3-0"),
            ]),
        };
        assert_eq!(
            imp::profile_user_candidates(&dacl),
            vec![
                "S-1-5-21-1-2-3-1001".to_owned(),
                "S-1-12-1-4-5-6-7".to_owned()
            ]
        );
        let null = ObservedDacl { aces: None, ..dacl };
        assert!(imp::profile_user_candidates(&null).is_empty());
    }

    /// banto-safe-root が profile の木に書く DACL の形（作る・緩める・締める・`service_only`・
    /// 印のファイル。フォルダは `OICI` の明示、ファイルは継承のフラグ無し、置き場の中で
    /// 後から作られる物は継承）は、profile のフォルダに同じ形が付いていれば許可リストで通る。
    #[cfg(windows)]
    #[test]
    fn the_shapes_banto_safe_root_writes_are_accepted() {
        use banto_safe_root::acl::{
            self, DirAccess, FileSecurity, InstallUserSid, ObservedAce, ObservedDacl,
        };
        let users = [
            InstallUserSid::parse("S-1-5-21-1-2-3-1001").unwrap(),
            InstallUserSid::parse("S-1-12-1-4-5-6-7").unwrap(),
        ];
        let setting = DirAccess::with_install_users(users.clone());
        let operation = DirAccess::with_readers(users.clone());
        let shape = |access: &DirAccess, flags: u8, owner: &str, is_dir: bool| {
            let mut aces = vec![
                (acl::SYSTEM_SID.to_owned(), acl::FULL_ACCESS_MASK),
                (acl::ADMINISTRATORS_SID.to_owned(), acl::FULL_ACCESS_MASK),
            ];
            for user in access.install_users() {
                aces.push((user.as_str().to_owned(), access.right().mask()));
            }
            FileSecurity {
                is_dir,
                is_reparse_point: false,
                links: 1,
                dacl: ObservedDacl {
                    protected: flags & 0x10 == 0,
                    owner: owner.to_owned(),
                    aces: Some(
                        aces.into_iter()
                            .map(|(sid, mask)| ObservedAce {
                                ace_type: acl::ACCESS_ALLOWED_ACE_TYPE,
                                flags,
                                mask,
                                sid,
                            })
                            .collect(),
                    ),
                },
            }
        };
        for (name, access) in [
            ("setting", &setting),
            ("operation", &operation),
            ("service_only", &DirAccess::service_only()),
        ] {
            // profile のフォルダ（根）には同じ利用者が同じ形で付く（木の全体へ同じ組を書く）。
            let profile = shape(access, 0x03, acl::ADMINISTRATORS_SID, true);
            let allowed = imp::profile_user_candidates(&profile.dacl);
            let expected: Vec<String> = access
                .install_users()
                .iter()
                .map(|user| user.as_str().to_owned())
                .collect();
            assert_eq!(allowed, expected, "{name}");
            for (what, security) in [
                ("dir", shape(access, 0x03, acl::ADMINISTRATORS_SID, true)),
                ("file", shape(access, 0x00, acl::ADMINISTRATORS_SID, false)),
                (
                    "inherited dir",
                    shape(access, 0x13, acl::ADMINISTRATORS_SID, true),
                ),
                (
                    "inherited file",
                    shape(access, 0x10, acl::ADMINISTRATORS_SID, false),
                ),
                ("system owner", shape(access, 0x10, acl::SYSTEM_SID, false)),
                (
                    "marker",
                    shape(
                        &DirAccess::service_only(),
                        0x00,
                        acl::ADMINISTRATORS_SID,
                        false,
                    ),
                ),
            ] {
                let refused: Vec<String> = acl::exposure(&security)
                    .iter()
                    .filter_map(|exposure| imp::refusal(exposure, &allowed))
                    .collect();
                assert!(refused.is_empty(), "{name} {what}: {refused:?}");
            }
        }
        // デスクトップの利用者が作った物（所有者がその利用者）は、実行しているアカウントか
        // profile の利用者として通る。
        let mine = shape(
            &DirAccess::with_install_user(users[0].clone()),
            0x10,
            users[0].as_str(),
            false,
        );
        let allowed = [users[0].as_str().to_owned()];
        assert!(acl::exposure(&mine)
            .iter()
            .all(|exposure| imp::refusal(exposure, &allowed).is_none()));
    }

    /// テスト用の「別の利用者」: 実行しているアカウントと同じ機械の Guest（RID 501）。
    /// 実行しているアカウントがローカルの形でない・Guest が解決できない PC では `None`。
    #[cfg(windows)]
    fn local_guest() -> Option<String> {
        use banto_safe_root::acl::{self, InstallUserSid};
        let me = acl::current_user_sid().ok()?;
        let (domain, _) = me.as_str().strip_prefix("S-1-5-21-")?.rsplit_once('-')?;
        let guest = InstallUserSid::parse(&format!("S-1-5-21-{domain}-501")).ok()?;
        acl::check_user_account(&guest).ok()?;
        Some(guest.as_str().to_owned())
    }

    /// 許可リストにない利用者（実在する Guest。`icacls` は解決できない SID を書けない）への
    /// 許可の ACE が既にある鍵にあれば断る。
    #[cfg(windows)]
    #[test]
    fn an_ace_for_a_user_outside_the_allowlist_is_refused() {
        let Some(other) = local_guest() else {
            eprintln!("skip: この PC ではテスト用の別の利用者（Guest）を解決できません");
            return;
        };
        let root = crate::test_support::TempDir::new("opcua-pki-other-user");
        let dir = pki_dir(&root.path().join("data"));
        prepare(&dir, None).expect("fresh pki dir");
        let key = dir.join("private").join("private.pem");
        std::fs::create_dir_all(key.parent().unwrap()).unwrap();
        std::fs::write(&key, b"dummy").unwrap();
        icacls(&key, &["/grant", &format!("*{other}:(R)")]);

        let reason = prepare(&dir, None).expect_err("another user can read the key");
        assert!(
            reason.contains(&format!("許可リストにない相手（{other}）")),
            "{reason}"
        );
        assert!(reason.contains("private.pem"), "{reason}");

        icacls(&key, &["/remove:g", &format!("*{other}")]);
        prepare(&dir, None).expect("ACE removed");
    }

    /// profile のフォルダに利用者 X の許可の ACE があれば、置き場の X の ACE は通る。
    /// profile の木を書く形（設定モード = 「変更」、運用モード = 「読み取りと実行」）を
    /// profile と置き場に付けても通る。
    #[cfg(windows)]
    #[test]
    fn a_user_allowed_on_the_profile_dir_is_accepted_on_the_pki_dir() {
        use banto_safe_root::acl::{self, DirAccess, InstallUserSid};
        let Some(guest) = local_guest() else {
            eprintln!("skip: この PC ではテスト用の別の利用者（Guest）を解決できません");
            return;
        };
        let root = crate::test_support::TempDir::new("opcua-pki-profile-user");
        let profile = root.path().join("profile");
        let dir = pki_dir(&profile.join("data"));
        prepare(&dir, Some(&profile)).expect("fresh pki dir");
        let key = dir.join("private").join("private.pem");
        std::fs::create_dir_all(key.parent().unwrap()).unwrap();
        std::fs::write(&key, b"dummy").unwrap();
        icacls(&dir, &["/grant", &format!("*{guest}:(OI)(CI)(RX)")]);

        // profile を握らない・profile に X がいないなら断る。
        let reason = prepare(&dir, None).expect_err("guest is not allowed");
        assert!(reason.contains(&guest), "{reason}");
        let reason = prepare(&dir, Some(&profile)).expect_err("guest is not on the profile");
        assert!(reason.contains(&guest), "{reason}");

        // profile に X の明示の許可があれば通る。
        icacls(&profile, &["/grant", &format!("*{guest}:(OI)(CI)(RX)")]);
        prepare(&dir, Some(&profile)).expect("guest is allowed on the profile");

        // profile の木を書く形（自分と X）を profile と置き場に付けても通る。
        let me = acl::current_user_sid().expect("current user");
        let users = [me, InstallUserSid::parse(&guest).unwrap()];
        let setting = DirAccess::with_install_users(users.clone());
        let operation = DirAccess::with_readers(users);
        acl::reset_dir_acl(&profile, &setting).expect("relax profile");
        acl::reset_dir_acl(&dir, &setting).expect("relax pki");
        prepare(&dir, Some(&profile)).expect("setting mode shape");
        acl::reset_dir_acl(&dir, &operation).expect("close pki");
        prepare(&dir, Some(&profile)).expect("operation mode shape");
        // 後片付けのために書ける形へ戻す。
        acl::reset_dir_acl(&dir, &setting).expect("relax pki again");
    }

    #[cfg(unix)]
    #[test]
    fn a_world_readable_existing_key_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let root = crate::test_support::TempDir::new("opcua-pki-unix-key");
        let dir = pki_dir(&root.path().join("data"));
        let key = dir.join("private").join("private.pem");
        std::fs::create_dir_all(key.parent().unwrap()).unwrap();
        std::fs::write(&key, b"dummy").unwrap();
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap();
        let reason = prepare(&dir, None).expect_err("world readable key");
        assert!(reason.contains("private.pem"), "{reason}");
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
        prepare(&dir, None).expect("restricted key");
    }

    #[cfg(unix)]
    #[test]
    fn group_writable_pki_directories_are_refused() {
        use std::os::unix::fs::PermissionsExt;
        let root = crate::test_support::TempDir::new("opcua-pki-unix-grpw");
        let dir = pki_dir(&root.path().join("data"));
        std::fs::create_dir_all(dir.join("own")).unwrap();
        std::fs::create_dir_all(dir.join("private")).unwrap();
        for sub in ["", "own", "private"] {
            let path = if sub.is_empty() {
                dir.clone()
            } else {
                dir.join(sub)
            };
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o770)).unwrap();
            let reason = prepare(&dir, None).expect_err("group writable dir");
            assert!(
                reason.contains("chmod 700") && reason.contains("770"),
                "{reason}"
            );
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        prepare(&dir, None).expect("restricted dirs");
    }

    #[cfg(unix)]
    #[test]
    fn a_group_writable_certificate_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let root = crate::test_support::TempDir::new("opcua-pki-unix-cert");
        let dir = pki_dir(&root.path().join("data"));
        prepare(&dir, None).unwrap();
        std::fs::create_dir_all(dir.join("own")).unwrap();
        let cert = dir.join("own").join("cert.der");
        std::fs::write(&cert, b"dummy").unwrap();
        std::fs::set_permissions(&cert, std::fs::Permissions::from_mode(0o664)).unwrap();
        assert!(prepare(&dir, None).is_err());
        std::fs::set_permissions(&cert, std::fs::Permissions::from_mode(0o644)).unwrap();
        prepare(&dir, None).expect("cert readable but not writable");
    }

    #[cfg(unix)]
    #[test]
    fn a_new_pki_dir_is_created_with_0700() {
        use std::os::unix::fs::PermissionsExt;
        let root = crate::test_support::TempDir::new("opcua-pki-unix-new");
        let dir = pki_dir(&root.path().join("data"));
        prepare(&dir, None).unwrap();
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
    }

    /// 置き場のパスからデータのフォルダを取る。`…/opcua/pki` の形でなければ断る。
    #[test]
    fn the_data_dir_is_taken_from_the_pki_dir() {
        let data = Path::new("root").join("data");
        assert_eq!(data_dir_of(&pki_dir(&data)).unwrap(), data.as_path());
        assert!(data_dir_of(&data.join("pki")).is_err());
        assert!(data_dir_of(&data.join("opcua").join("other")).is_err());
    }

    /// 置き場の本当のパスがデータのフォルダの本当のパスの下になければ（別の profile・外を
    /// 指すリンクを辿った先など）断る（#943 のレビュー P2）。
    #[test]
    fn a_pki_dir_resolved_outside_the_data_dir_is_refused() {
        let data = Path::new("root").join("profile-a").join("data");
        ensure_inside(&data, &pki_dir(&data)).expect("inside");
        let reason = ensure_inside(&data, &pki_dir(&Path::new("root").join("profile-b")))
            .expect_err("another profile");
        assert!(reason.contains("profile-b"), "{reason}");
        ensure_inside(&data, &data).expect_err("the data dir itself");
        // 名前の前方一致（`data-other`）は下ではない。
        ensure_inside(
            &data,
            &Path::new("root").join("profile-a").join("data-other"),
        )
        .expect_err("sibling with a common prefix");
    }

    #[cfg(windows)]
    fn junction(link: &Path, target: &Path) {
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .stdout(std::process::Stdio::null())
            .status()
            .expect("cmd を起動できない");
        assert!(status.success(), "mklink /J に失敗した");
    }

    /// 親の `opcua`、または置き場の `pki` がジャンクション（別の profile などを指す）なら、
    /// 辿らずに断り、その先に何も作らない（#943 のレビュー P2）。
    #[cfg(windows)]
    #[test]
    fn a_junction_on_opcua_or_pki_is_refused() {
        let root = crate::test_support::TempDir::new("opcua-pki-junction");
        let other = root.path().join("other-profile");
        std::fs::create_dir_all(&other).unwrap();

        let data = root.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        junction(&data.join("opcua"), &other);
        let reason = prepare(&pki_dir(&data), None).expect_err("opcua is a junction");
        assert!(reason.contains("reparse point"), "{reason}");
        assert!(reason.contains("opcua"), "{reason}");
        assert!(!other.join("pki").exists(), "辿った先に作った");

        let data = root.path().join("data2");
        std::fs::create_dir_all(data.join("opcua")).unwrap();
        junction(&data.join("opcua").join("pki"), &other);
        let reason = prepare(&pki_dir(&data), None).expect_err("pki is a junction");
        assert!(reason.contains("reparse point"), "{reason}");
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_on_opcua_or_pki_is_refused() {
        let root = crate::test_support::TempDir::new("opcua-pki-unix-symlink");
        let other = root.path().join("other-profile");
        std::fs::create_dir_all(&other).unwrap();

        let data = root.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        std::os::unix::fs::symlink(&other, data.join("opcua")).unwrap();
        let reason = prepare(&pki_dir(&data), None).expect_err("opcua is a symlink");
        assert!(reason.contains("シンボリックリンク"), "{reason}");
        assert!(!other.join("pki").exists(), "辿った先に作った");

        let data = root.path().join("data2");
        std::fs::create_dir_all(data.join("opcua")).unwrap();
        std::os::unix::fs::symlink(&other, data.join("opcua").join("pki")).unwrap();
        let reason = prepare(&pki_dir(&data), None).expect_err("pki is a symlink");
        assert!(reason.contains("シンボリックリンク"), "{reason}");
    }

    /// 所有者の比較: 実行している利用者と違う UID は断り、理由に UID と `chown` が出る。
    /// root の所有は `opcua`（`root_ok`）だけ通す（#943 のレビュー P1）。
    #[cfg(unix)]
    #[test]
    fn a_foreign_owner_is_refused() {
        let path = Path::new("/srv/data/opcua/pki/private/private.pem");
        assert_eq!(imp::owner_refusal(path, 1000, 1000, false), None);
        let reason = imp::owner_refusal(path, 1001, 1000, false).expect("foreign owner");
        assert!(
            reason.contains("UID 1001") && reason.contains("UID 1000"),
            "{reason}"
        );
        assert!(reason.contains("chown 1000"), "{reason}");
        assert!(imp::owner_refusal(path, 0, 1000, false).is_some());
        assert_eq!(imp::owner_refusal(path, 0, 1000, true), None);
        assert!(imp::owner_refusal(path, 1001, 1000, true).is_some());
        // root で動く Hub は、別の利用者の置き場を断る。
        assert!(imp::owner_refusal(path, 1000, 0, false).is_some());
    }

    /// 実行している利用者が作った置き場は、所有者の確かめに通る（実効 UID が読める）。
    #[cfg(unix)]
    #[test]
    fn a_pki_dir_owned_by_the_current_user_is_accepted() {
        use std::os::unix::fs::MetadataExt;
        let root = crate::test_support::TempDir::new("opcua-pki-unix-owner");
        let dir = pki_dir(&root.path().join("data"));
        prepare(&dir, None).expect("own pki dir");
        let uid = std::fs::symlink_metadata(&dir).unwrap().uid();
        assert_eq!(imp::owner_refusal(&dir, uid, uid, false), None);
    }
}
