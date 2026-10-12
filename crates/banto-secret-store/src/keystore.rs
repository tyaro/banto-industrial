//! `banto_hub_bootstrap::KeyStore` のアダプタ。
//!
//! 失敗はすべて `ErrorKind::KeyStore` に写す（bootstrap はそこで fail closed
//! する - 保てないキーを発行しない）。detail には [`SecretStoreError`] の
//! `Display` だけを入れ、秘密は入らない（そもそも [`SecretStoreError`] が持たない）。

use banto_hub_bootstrap::error::{Error as BootstrapError, ErrorKind, Result as BootstrapResult};
use banto_hub_bootstrap::KeyStore;

use crate::error::SecretStoreError;
use crate::protector::Protector;
use crate::store::FileSecretStore;

fn to_bootstrap(error: SecretStoreError) -> BootstrapError {
    BootstrapError::with_detail(ErrorKind::KeyStore, error.to_string())
}

impl<P: Protector> KeyStore for FileSecretStore<P> {
    fn get(&self, account: &str) -> BootstrapResult<Option<String>> {
        // `Zeroizing<String>` から呼び出し側の `String` へ写す（元は drop で消える）。
        // ここから先の平文の扱いは KeyStore の呼び出し側（bootstrap）の契約。
        self.load(account)
            .map(|secret| secret.map(|s| s.as_str().to_owned()))
            .map_err(to_bootstrap)
    }

    fn set(&self, account: &str, secret: &str) -> BootstrapResult<()> {
        self.store(account, secret).map_err(to_bootstrap)
    }

    fn delete(&self, account: &str) -> BootstrapResult<()> {
        self.remove(account).map_err(to_bootstrap)
    }
}
