//! ChronoGazer の API キー（#1161 PR4、docs/chronogazer-agent-api-design.md
//! §3）。
//!
//! キーの形式・ハッシュ・照合の順・表への SQL・拒否の応答は共有 crate
//! `banto-api-keys`（HUB と同じ物）が持つ。ここが決めるのは次の 2 つだけ:
//!
//! - 接頭辞 `cg`（キーは `cg_{prefix}_{secret}`）。
//! - スコープの書式と意味（§3.3）: `read` と `config` の 2 つ。`config` は
//!   `read` を含む（設定を変えるには今の設定を読む必要があるため）。キーには
//!   対象を絞る書式（HUB の `read:{conn}.{grp}.{tag}` のような物）を設けない。
//!
//! どのスコープでも、ユーザー管理・サーバー設定・HUB 連携・バックアップ・
//! 監査ログ・API キー自身・認証・UI 設定・SSE はキーでは通さない。その境界は
//! スコープではなくルートの集まりで決める（`crate::rest` の `api_router` と
//! `rest/actor.rs` のモジュール doc）。

use banto_api_keys::ApiKeyScopes;

pub use banto_api_keys::{
    api_key_rejection_response, api_key_verdict, unauthorized_response, ApiKeyContext,
    ApiKeyDeleteOutcome, ApiKeyRejection, ApiKeySummary, IssuedApiKey,
};

/// キーの接頭辞（`cg_…`）。認証の層が `Authorization: Bearer` の値をキーとして
/// 照合するかどうかをこれで決める。
pub const KEY_PREFIX: &str = "cg";

/// 設定と状態の読み取り（§3.3）。
pub const SCOPE_READ: &str = "read";
/// `read` のすべてに加えて、設定の作成・更新・削除と収集の開始・停止・再起動
/// （§3.3）。
pub const SCOPE_CONFIG: &str = "config";

/// ChronoGazer の API キーのサービス（共有 crate のサービスに ChronoGazer の
/// スコープを渡した型の別名）。
pub type ApiKeysService = banto_api_keys::ApiKeysService<CgScopes>;

/// ChronoGazer がキーに決めること: 接頭辞 `cg` とスコープの書式。値を持たない
/// 印の型。
#[derive(Debug, Clone, Copy)]
pub struct CgScopes;

impl ApiKeyScopes for CgScopes {
    const KEY_PREFIX: &'static str = KEY_PREFIX;

    fn validate_scope(scope: &str) -> Result<(), String> {
        match scope {
            SCOPE_READ | SCOPE_CONFIG => Ok(()),
            other => Err(format!(
                "不明なスコープです: \"{other}\"（使えるのは \"read\" と \"config\" だけです）"
            )),
        }
    }
}

/// 照合で得たスコープの並び（文字列）を、判定に使う形にした物。発行のときに
/// [`CgScopes::validate_scope`] を通っているので、知らない文字列は無視する
/// （許す側には倒さない）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct KeyScopes {
    read: bool,
    config: bool,
}

impl KeyScopes {
    pub fn from_strings(scopes: &[String]) -> Self {
        let mut parsed = Self::default();
        for scope in scopes {
            match scope.as_str() {
                SCOPE_READ => parsed.read = true,
                SCOPE_CONFIG => parsed.config = true,
                _ => {}
            }
        }
        parsed
    }

    /// 設定と状態を読めるか（`read` か `config`。`config` は `read` を含む）。
    pub fn can_read(self) -> bool {
        self.read || self.config
    }

    /// 設定を変えられ、収集を操作できるか（`config`）。
    pub fn can_configure(self) -> bool {
        self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_read_and_config_are_valid_scopes() {
        assert!(CgScopes::validate_scope("read").is_ok());
        assert!(CgScopes::validate_scope("config").is_ok());
        for bad in ["admin", "write", "read:a.b.c", "", "Read", " read"] {
            assert!(CgScopes::validate_scope(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn config_includes_read() {
        let config = KeyScopes::from_strings(&["config".to_string()]);
        assert!(config.can_read());
        assert!(config.can_configure());
        let read = KeyScopes::from_strings(&["read".to_string()]);
        assert!(read.can_read());
        assert!(!read.can_configure());
        let none = KeyScopes::from_strings(&["admin".to_string()]);
        assert!(!none.can_read());
        assert!(!none.can_configure());
    }
}
