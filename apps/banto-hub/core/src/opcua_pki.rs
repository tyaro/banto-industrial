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
//!   含む）を辿らずに開いて記述子を読み（`inspect_file`）、`exposure` の理由のうち次の
//!   ものがあれば**鍵を作らず・読まずに**起動を断る:
//!   - 広いグループ（[`BROAD_SIDS`] と、ドメインの Domain Users・Domain Guests・
//!     Domain Computers）への**許可**の ACE（明示・継承・`INHERIT_ONLY` を問わず、マスクも
//!     問わない。多めに断る側に倒す）。所有者が広いグループのときも。
//!   - null DACL・種類の分からない ACE・SID の読めない ACE。
//!   - reparse point（ジャンクション・シンボリックリンク）・ハードリンク（リンク数 2 以上）。
//!
//!   特定の利用者（profile のインストール利用者など）への許可は断らない（profile の
//!   ACL の設計が決める範囲）。
//! - **Unix**: banto-opcua が鍵のフォルダを 0700・鍵を 0600 にする。ここでは、既にある鍵が
//!   グループ・他人に開いている（`mode & 0o077`）、置き場・`own`・`private` が他人に
//!   書ける（`mode & 0o002`）、どれかがシンボリックリンクなら断る。
//!
//! 断ったとき・banto-opcua の `Error::Pki` のときは、理由をログと状態の API
//! （`GET /api/opcua-status` の `failed`）に出す（`crate::opcua`）。原因を直したあと、
//! 同じ設定をもう一度保存すれば再試行になる。置き場を消せば、次の起動で保護された形で
//! 作り直し、鍵と証明書も作り直す（クライアントは新しい証明書を信頼し直す）。
//!
//! 確かめてから banto-opcua が鍵を作るまでの間に ACL を変えられる競合は防がない
//! （置き場は保護された DACL なので、変えられるのは SYSTEM・Administrators・
//! 許可された利用者だけ）。

use std::path::{Path, PathBuf};

/// 置き場の、データのフォルダからの相対（`opcua/pki`）。
const PKI_SUBDIRS: [&str; 2] = ["opcua", "pki"];

/// データのフォルダ `data_dir` の下の、OPC UA の鍵と証明書の置き場。
pub(crate) fn pki_dir(data_dir: &Path) -> PathBuf {
    PKI_SUBDIRS
        .iter()
        .fold(data_dir.to_path_buf(), |dir, name| dir.join(name))
}

/// 広いグループの SID（Windows）。これらへの許可があれば、許されていない利用者が
/// 鍵を読める（書ける）とみなす。
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
/// ことを確かめる（モジュール doc「鍵を作る前の確かめ」）。断るときは、ログと状態の API に
/// 出す理由（直し方つき）を返す。同期の I/O なので、blocking 用のスレッドで呼ぶ。
pub(crate) fn prepare(dir: &Path) -> Result<(), String> {
    imp::prepare(dir).map_err(|reason| {
        format!(
            "OPC UA の鍵と証明書の置き場（{}）を使えないため、鍵を作らず（読まず）に止めました: {reason}。原因を直してから、同じ設定でもう一度保存すると再試行します（置き場を消すと、次の起動で保護された形で作り直し、鍵と証明書も作り直します）",
            dir.display()
        )
    })
}

#[cfg(windows)]
mod imp {
    use std::path::Path;

    use banto_safe_root::acl::{self, Exposure};
    use banto_safe_root::DirAccess;

    /// 辿る子の数・深さの上限（置き場の中は async-opcua の数個のフォルダだけ）。
    const MAX_ENTRIES: usize = 1024;
    const MAX_DEPTH: usize = 8;

    pub(super) fn prepare(dir: &Path) -> Result<(), String> {
        // `./data` のような `.` の要素は `inspect_file` が断るので、先に絶対パスにする
        // （Windows では `GetFullPathNameW` で `.`・`..` も畳む）。
        let dir =
            std::path::absolute(dir).map_err(|err| format!("パスを絶対パスにできません: {err}"))?;
        if let Some(parent) = dir.parent() {
            std::fs::create_dir_all(parent).map_err(|err| {
                format!("親のフォルダ（{}）を作れません: {err}", parent.display())
            })?;
        }
        // このプロセスの利用者（デスクトップ）には「変更」。LocalSystem など利用者の
        // アカウントでなければ SYSTEM・Administrators だけ。
        let access = acl::current_user_sid()
            .map(DirAccess::with_install_user)
            .unwrap_or_else(|_| DirAccess::service_only());
        acl::create_dir_if_absent(&dir, &access)
            .map_err(|err| format!("保護された置き場を作れません: {err}"))?;
        let mut problems = Vec::new();
        let mut count = 0;
        walk(&dir, 0, &mut count, &mut problems)?;
        if problems.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "許されていない利用者が読める（書ける）状態です（{}）",
                problems.join(" / ")
            ))
        }
    }

    /// `path` とその中を辿らずに確かめ、断る理由を `problems` に足す。
    fn walk(
        path: &Path,
        depth: usize,
        count: &mut usize,
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
            if let Some(reason) = refusal(&exposure) {
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
                walk(&entry.path(), depth + 1, count, problems)?;
            }
        }
        Ok(())
    }

    /// `exposure` の理由のうち、鍵の置き場で断るもの（モジュール doc）。
    pub(super) fn refusal(exposure: &Exposure) -> Option<String> {
        match exposure {
            Exposure::ReparsePoint => Some("リンク（reparse point）です".to_owned()),
            Exposure::HardLinked(links) => {
                Some(format!("ハードリンクです（名前が {links} 個あります）"))
            }
            Exposure::ForeignOwner(sid) => super::broad_group_name(sid)
                .map(|name| format!("所有者が広いグループ {name}（{sid}）です")),
            Exposure::NullDacl => Some("DACL が無く、全員に開いています".to_owned()),
            Exposure::AllowsOther {
                flags, mask, sid, ..
            } => super::broad_group_name(sid).map(|name| {
                let inherited = if flags & 0x10 != 0 { "・継承" } else { "" };
                format!("広いグループ {name}（{sid}）への許可の ACE があります（mask {mask:#x}{inherited}）")
            }),
            Exposure::UnknownAce { ace_type, .. } => {
                Some(format!("種類の分からない ACE があります（種類 {ace_type}）"))
            }
            Exposure::UnreadableSid { ace_type, .. } => {
                Some(format!("SID の読めない ACE があります（種類 {ace_type}）"))
            }
        }
    }
}

#[cfg(unix)]
mod imp {
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    pub(super) fn prepare(dir: &Path) -> Result<(), String> {
        std::fs::create_dir_all(dir).map_err(|err| format!("置き場を作れません: {err}"))?;
        let key = dir.join("private").join("private.pem");
        for (path, forbidden) in [
            (dir.to_path_buf(), 0o002),
            (dir.join("own"), 0o002),
            (dir.join("private"), 0o002),
            (key, 0o077),
        ] {
            let metadata = match std::fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                Err(err) => return Err(format!("{} を確かめられません: {err}", path.display())),
            };
            if metadata.file_type().is_symlink() {
                return Err(format!("{} がシンボリックリンクです", path.display()));
            }
            let mode = metadata.permissions().mode();
            if mode & forbidden != 0 {
                return Err(format!(
                    "{} の権限が広すぎます（{:o}）",
                    path.display(),
                    mode & 0o777
                ));
            }
        }
        Ok(())
    }
}

#[cfg(not(any(windows, unix)))]
mod imp {
    pub(super) fn prepare(_dir: &std::path::Path) -> Result<(), String> {
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
        prepare(&dir).expect("fresh pki dir");
        assert!(dir.is_dir());
        prepare(&dir).expect("existing pki dir");
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
        prepare(&dir).expect("fresh pki dir");
        let key = dir.join("private").join("private.pem");
        std::fs::create_dir_all(key.parent().unwrap()).unwrap();
        std::fs::write(&key, b"dummy").unwrap();
        icacls(&key, &["/grant", "*S-1-5-32-545:(R)"]);

        let reason = prepare(&dir).expect_err("Users can read the key");
        assert!(reason.contains("Users（S-1-5-32-545）"), "{reason}");
        assert!(reason.contains("private.pem"), "{reason}");
        assert!(reason.contains("鍵を作らず"), "{reason}");

        icacls(&key, &["/remove:g", "*S-1-5-32-545"]);
        prepare(&dir).expect("ACE removed");
    }

    /// 既にある置き場が Everyone に継承で開いていれば（子にも伝わる）断る。
    #[cfg(windows)]
    #[test]
    fn an_inheritable_everyone_ace_on_the_pki_dir_is_refused() {
        let root = crate::test_support::TempDir::new("opcua-pki-everyone");
        let dir = pki_dir(&root.path().join("data"));
        std::fs::create_dir_all(&dir).unwrap();
        icacls(&dir, &["/grant", "*S-1-1-0:(OI)(CI)(RX)"]);
        let reason = prepare(&dir).expect_err("Everyone can read");
        assert!(reason.contains("Everyone（S-1-1-0）"), "{reason}");
    }

    #[cfg(windows)]
    #[test]
    fn exposures_other_than_broad_allows_are_refused() {
        use banto_safe_root::acl::Exposure;
        assert!(imp::refusal(&Exposure::ReparsePoint).is_some());
        assert!(imp::refusal(&Exposure::HardLinked(2)).is_some());
        assert!(imp::refusal(&Exposure::NullDacl).is_some());
        assert!(imp::refusal(&Exposure::ForeignOwner("S-1-5-21-1-2-3-1001".into())).is_none());
        assert!(imp::refusal(&Exposure::AllowsOther {
            ace_type: 0,
            flags: 0,
            mask: 0x1301bf,
            sid: "S-1-5-21-1-2-3-1001".into(),
        })
        .is_none());
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
        let reason = prepare(&dir).expect_err("world readable key");
        assert!(reason.contains("private.pem"), "{reason}");
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
        prepare(&dir).expect("restricted key");
    }
}
