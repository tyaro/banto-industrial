//! Hub の API キーの保存先（#392 段階 B、決定 4・H10）。
//!
//! B1-5 で `crate::server_host` に置いた [`open_key_store`] と [`FailedKeyStore`] を、
//! B2-4a で（挙動を変えずに）ここへ移した。`chronogazer-server` に加え、B2-4b から
//! デスクトップ host も同じ保存先（`<root>\secrets\hub-keys\`、
//! `banto_secret_store::FileSecretStore<DpapiMachine>`）を使う（`src-tauri` の
//! `desktop_key_store`。#332 から B2-4a までの OS キーリングのキーは移さない）。
//! `crate::server_host` は再公開するので、既存の `server_host::open_key_store`・
//! `server_host::FailedKeyStore` の参照はそのまま動く。

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
    match open_key_store_impl(layout) {
        Ok(store) => (store, None),
        Err(reason) => (Arc::new(FailedKeyStore::new(reason.clone())), Some(reason)),
    }
}

#[cfg(windows)]
fn open_key_store_impl(layout: &DataLayout) -> Result<Arc<dyn KeyStore>, String> {
    use banto_secret_store::{DpapiMachine, FileSecretStore};

    // 今のプロセスの利用者に「変更」（LocalSystem なら付けない）。データのルートを
    // 作るときと同じ判断（`crate::paths::current_process_dir_access`）。
    let access = crate::paths::current_process_dir_access().map_err(|err| err.to_string())?;
    FileSecretStore::open(layout.hub_keys_dir(), access, DpapiMachine)
        .map(|store| Arc::new(store) as Arc<dyn KeyStore>)
        .map_err(|err| err.to_string())
}

#[cfg(not(windows))]
fn open_key_store_impl(_layout: &DataLayout) -> Result<Arc<dyn KeyStore>, String> {
    Err("この OS では Hub の API キーを保存できません（DPAPI は Windows だけ）".to_string())
}

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

    /// `server_host` からの再公開で、B1-5 の参照がそのまま使える。
    #[test]
    fn server_host_reexports_the_moved_items() {
        let store: crate::server_host::FailedKeyStore = FailedKeyStore::new("x");
        assert!(store.get("a").is_err());
        let _open = crate::server_host::open_key_store;
    }
}
