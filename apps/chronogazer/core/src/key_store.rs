//! Hub の API キーの保存先（#392 段階 B、決定 4・H10）。
//!
//! B1-5 で `crate::server_host` に置いた [`open_key_store`] と [`FailedKeyStore`] を、
//! B2-4a で（挙動を変えずに）ここへ移した。`chronogazer-server` に加え、B2-4b から
//! デスクトップ host も同じ保存先（`<root>\secrets\hub-keys\`、
//! `banto_secret_store::FileSecretStore<DpapiMachine>`）を使う（`src-tauri` の
//! `desktop_key_store`。#332 から B2-4a までの OS キーリングのキーは移さない）。
//! `crate::server_host` は再公開するので、既存の `server_host::open_key_store`・
//! `server_host::FailedKeyStore` の参照はそのまま動く。
//!
//! B3-2a で、ACL の利用者の欄を呼び出し側が決める [`open_key_store_with`] を足した
//! （サービスがインストール利用者の SID を渡す。[`open_key_store`] は今のプロセスの
//! 利用者を渡す薄い包みで、挙動は同じ）。

use std::sync::Arc;

use banto_hub_bootstrap::error::{Error as BootstrapError, ErrorKind as BootstrapErrorKind};
use banto_hub_bootstrap::KeyStore;

use crate::paths::DataLayout;

/// 開けなかった KeyStore の代わり: すべての操作がその理由で失敗する
/// （`ErrorKind::KeyStore`。bootstrap は fail closed し、保てないキーを発行しない）。
/// 理由に秘密は入らない（`SecretStoreError` の文言）。
#[derive(Debug, Clone)]
pub struct FailedKeyStore {
    reason: String,
}

impl FailedKeyStore {
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }

    fn error(&self) -> BootstrapError {
        BootstrapError::with_detail(
            BootstrapErrorKind::KeyStore,
            format!("Hub の API キーの保存先を使えません: {}", self.reason),
        )
    }
}

impl KeyStore for FailedKeyStore {
    fn get(&self, _account: &str) -> banto_hub_bootstrap::error::Result<Option<String>> {
        Err(self.error())
    }

    fn set(&self, _account: &str, _secret: &str) -> banto_hub_bootstrap::error::Result<()> {
        Err(self.error())
    }

    fn delete(&self, _account: &str) -> banto_hub_bootstrap::error::Result<()> {
        Err(self.error())
    }
}

/// Hub の API キーの保存先（`<root>\secrets\hub-keys\`、DPAPI machine）を開く。
/// 開けなければ [`FailedKeyStore`] と理由（ログに出す）を返す。
///
/// ディレクトリの ACL の利用者（決定 4 のインストール利用者の欄）は、**今の
/// プロセスの利用者**:
///
/// - コンソールを一般の利用者が動かすと、`%ProgramData%` の既定の ACL で
///   `ChronoGazer\` は作れる（作った人が所有者）が、SYSTEM・Administrators だけの
///   ディレクトリでは自分で読み書きできない（所有者も許可の外で、`banto-secret-store`
///   の確認に通らない）。その人に「変更」を付ければ、作った本人が使え、他の
///   一般の利用者は読めない。
/// - 昇格したコンソールでも、トークンの利用者はその人の SID（所有者は
///   Administrators）なので同じ形になる。
/// - LocalSystem（`S-1-5-18`。利用者のアカウントの SID ではない）で動いていれば
///   利用者の ACE を付けない（B3 のサービスは、登録時にインストール利用者の SID を
///   渡す形にする）。
///
/// 先に別の利用者が作ったディレクトリ（ACL が違う）は使わない（`AclMismatch`、
/// 自動では直さない - B1-3 の決定）。B3-3 の昇格ヘルパが ACL を整える。
pub fn open_key_store(layout: &DataLayout) -> (Arc<dyn KeyStore>, Option<String>) {
    into_key_store(open_key_store_impl(layout))
}

/// [`open_key_store`] の、ディレクトリの ACL の利用者の欄を呼び出し側が決める形
/// （#392 B3-2a）。サービス（LocalSystem）は登録時に記録したインストール利用者の
/// SID を渡す（今のプロセスの利用者は SYSTEM で、`service_only` で開くとデスクトップ
/// が作った 3 つの ACE の `hub-keys` が `AclMismatch` になるため。B2-4a の申し送り
/// (3)）。開けないときの扱い（[`FailedKeyStore`] と理由）は [`open_key_store`] と同じ。
pub fn open_key_store_with(
    layout: &DataLayout,
    access: banto_safe_root::DirAccess,
) -> (Arc<dyn KeyStore>, Option<String>) {
    into_key_store(open_key_store_with_impl(layout, access))
}

/// [`crate::paths::DirAccessSource`] の決め方で開く（今のプロセスの利用者なら
/// [`open_key_store`]、指定があれば [`open_key_store_with`]）。
pub fn open_key_store_from(
    layout: &DataLayout,
    access: &crate::paths::DirAccessSource,
) -> (Arc<dyn KeyStore>, Option<String>) {
    match access {
        crate::paths::DirAccessSource::CurrentProcess => open_key_store(layout),
        crate::paths::DirAccessSource::Given(access) => open_key_store_with(layout, access.clone()),
    }
}

/// **握った** `secrets\hub-keys\`（`SafeRoot::open_managed` で運用モードの形で作って握った物）を
/// 保存先にして開く（サービス。#623 PR-3、設計 §6.1）。`FileSecretStore::open_in`: 作らず、
/// 握ったハンドルで `access`（運用モードの形）を確かめ、以後の読む・書く・消すもハンドル基準。
/// 開けなければ `Err`（サービスは起動を止める。[`open_key_store`] の「`FailedKeyStore` で
/// 続ける」はデスクトップ・コンソールだけ）。
pub fn open_key_store_in(
    dir: banto_safe_root::ManagedDir,
    access: banto_safe_root::DirAccess,
) -> Result<Arc<dyn KeyStore>, String> {
    open_key_store_in_impl(dir, access)
}

#[cfg(windows)]
fn open_key_store_in_impl(
    dir: banto_safe_root::ManagedDir,
    access: banto_safe_root::DirAccess,
) -> Result<Arc<dyn KeyStore>, String> {
    use banto_secret_store::{DpapiMachine, FileSecretStore};

    FileSecretStore::open_in(dir, access, DpapiMachine)
        .map(|store| Arc::new(store) as Arc<dyn KeyStore>)
        .map_err(|err| err.to_string())
}

#[cfg(not(windows))]
fn open_key_store_in_impl(
    _dir: banto_safe_root::ManagedDir,
    _access: banto_safe_root::DirAccess,
) -> Result<Arc<dyn KeyStore>, String> {
    Err(NO_DPAPI.to_string())
}

fn into_key_store(
    opened: Result<Arc<dyn KeyStore>, String>,
) -> (Arc<dyn KeyStore>, Option<String>) {
    match opened {
        Ok(store) => (store, None),
        Err(reason) => (Arc::new(FailedKeyStore::new(reason.clone())), Some(reason)),
    }
}

#[cfg(windows)]
fn open_key_store_impl(layout: &DataLayout) -> Result<Arc<dyn KeyStore>, String> {
    // 今のプロセスの利用者に「変更」（LocalSystem なら付けない）。データのルートを
    // 作るときと同じ判断（`crate::paths::current_process_dir_access`）。
    let access = crate::paths::current_process_dir_access().map_err(|err| err.to_string())?;
    open_key_store_with_impl(layout, access)
}

#[cfg(windows)]
fn open_key_store_with_impl(
    layout: &DataLayout,
    access: banto_safe_root::DirAccess,
) -> Result<Arc<dyn KeyStore>, String> {
    use banto_secret_store::{DpapiMachine, FileSecretStore};

    FileSecretStore::open(layout.hub_keys_dir(), access, DpapiMachine)
        .map(|store| Arc::new(store) as Arc<dyn KeyStore>)
        .map_err(|err| err.to_string())
}

#[cfg(not(windows))]
fn open_key_store_impl(_layout: &DataLayout) -> Result<Arc<dyn KeyStore>, String> {
    Err(NO_DPAPI.to_string())
}

#[cfg(not(windows))]
fn open_key_store_with_impl(
    _layout: &DataLayout,
    _access: banto_safe_root::DirAccess,
) -> Result<Arc<dyn KeyStore>, String> {
    Err(NO_DPAPI.to_string())
}

#[cfg(not(windows))]
const NO_DPAPI: &str = "この OS では Hub の API キーを保存できません（DPAPI は Windows だけ）";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_key_store_fails_closed_on_every_operation() {
        let store = FailedKeyStore::new("ACL が違います");
        for err in [
            store.get("a").unwrap_err(),
            store.set("a", "secret-value").unwrap_err(),
            store.delete("a").unwrap_err(),
        ] {
            assert_eq!(err.kind(), BootstrapErrorKind::KeyStore);
            assert!(err.to_string().contains("ACL が違います"), "{err}");
            assert!(!err.to_string().contains("secret-value"));
        }
    }

    /// #392 B3-2a: 利用者の欄を渡す形は、渡した指定で保存先を作る。利用者の SID を
    /// 渡せばその SID に「変更」が付いて使え、グループの SID なら作らずに理由を返す
    /// （渡した指定が使われている証拠。今のプロセスの利用者なら開ける）。
    #[cfg(windows)]
    #[test]
    fn open_key_store_with_uses_the_given_access() {
        use banto_safe_root::acl::{current_user_sid, read_dir_dacl, MODIFY_ACCESS_MASK};
        use banto_safe_root::{DirAccess, InstallUserSid};

        let dir = tempfile::tempdir().unwrap();
        let me = current_user_sid().unwrap();

        let layout = DataLayout::new(dir.path().join("user"));
        let (store, reason) =
            open_key_store_with(&layout, DirAccess::with_install_user(me.clone()));
        assert_eq!(reason, None);
        store.set("hub-a", "secret-value").unwrap();
        assert_eq!(store.get("hub-a").unwrap().as_deref(), Some("secret-value"));
        let dacl = read_dir_dacl(&layout.hub_keys_dir()).unwrap();
        assert!(dacl
            .aces
            .as_ref()
            .unwrap()
            .iter()
            .any(|ace| ace.sid == me.as_str() && ace.mask == MODIFY_ACCESS_MASK));

        // グループの SID の作り方とその前提は `crate::paths` の
        // `create_root_dir_with_uses_the_given_access` と同じ（PR #617 のレビュー）。
        assert!(
            me.as_str().starts_with("S-1-5-21-"),
            "ローカル・ドメインの利用者のアカウントで走らせる前提（グループの SID を -513 で作る）: {me:?}"
        );
        let machine = me.as_str().rsplit_once('-').unwrap().0;
        let group = InstallUserSid::parse(&format!("{machine}-513")).unwrap();
        let layout = DataLayout::new(dir.path().join("group"));
        let (store, reason) = open_key_store_with(&layout, DirAccess::with_install_user(group));
        assert!(reason.is_some(), "グループの SID で開けた");
        assert!(store.get("hub-a").is_err(), "fail closed");
        assert!(!layout.hub_keys_dir().exists(), "断ったときは作らない");
    }

    /// `server_host` からの再公開で、B1-5 の参照がそのまま使える。
    #[test]
    fn server_host_reexports_the_moved_items() {
        let store: crate::server_host::FailedKeyStore = FailedKeyStore::new("x");
        assert!(store.get("a").is_err());
        let _open = crate::server_host::open_key_store;
    }
}
