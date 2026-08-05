//! banto-hub 用アプリ設定 (docs/tag-server-design.md §8): `settings` テーブル
//! （migration `db.rs::apply_app_schema` 内、key/value）に対する型付きラッパ。
//! `apps/chronogazer/core/src/settings.rs` の `SettingsService`
//! get/set/upsert パターンをそのまま流用するが、ChronoGazer の
//! `ServerSettings`（LAN 公開トグル付き、既定オフ）とは違い、**hub は常時
//! サーバーであって「LAN 公開する/しない」を切り替えるデスクトップアプリでは
//! ない**ので `enabled` トグルを持たない — 起動したら常にリッスンする
//! （設計 §3.1「単一プロセスのヘッドレス axum サーバー」）。
//!
//! 設定項目は4つ、既定値は全て設計 §8/§3.3 の決定どおり:
//! - `server.bind`（既定 `"127.0.0.1"`）/ `server.port`（既定 `8722`、
//!   設計 §8「banto-hub = 8722」）
//! - `data.dir`（既定 `"./data"`）: tstore ファイルの出力先
//! - `retention.days`（既定 `7`、設計 §3.3 (a) 決定）: tstore 保持期間

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use banto_core::BantoError;

const KEY_SERVER_BIND: &str = "server.bind";
const KEY_SERVER_PORT: &str = "server.port";
const KEY_DATA_DIR: &str = "data.dir";
const KEY_RETENTION_DAYS: &str = "retention.days";

/// hub の既定ポート（docs/tag-server-design.md §8: 「管理 UI + REST + WS =
/// 8722」）。
pub const DEFAULT_PORT: u16 = 8722;
/// tstore 保持期間の既定日数（§3.3 (a) 決定: 「保持期間は既定7日」）。
pub const DEFAULT_RETENTION_DAYS: i64 = 7;

/// hub サーバー本体の bind/port（設計 §8）。ChronoGazer の
/// `ServerSettings` と違い `enabled` は持たない - hub は常時サーバーで
/// あって切替スイッチの対象ではない（このモジュールの doc comment参照）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerSettings {
    pub bind: String,
    pub port: u16,
}

impl Default for ServerSettings {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1".to_string(),
            port: DEFAULT_PORT,
        }
    }
}

/// tstore のデータディレクトリと保持期間（設計 §3.3）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoreSettings {
    pub data_dir: String,
    pub retention_days: i64,
}

impl Default for StoreSettings {
    fn default() -> Self {
        Self {
            data_dir: "./data".to_string(),
            retention_days: DEFAULT_RETENTION_DAYS,
        }
    }
}

/// Generic key/value settings store, backed by the `settings` table -
/// mirrors `chronogazer_core::settings::SettingsService`'s
/// get/set/upsert shape.
#[derive(Clone)]
pub struct SettingsService {
    pool: SqlitePool,
}

impl SettingsService {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn get(&self, key: &str) -> Result<Option<String>, BantoError> {
        sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = ?")
            .bind(key)
            .fetch_optional(&self.pool)
            .await
            .map_err(banto_storage::storage_error)
    }

    pub async fn set(&self, key: &str, value: &str) -> Result<(), BantoError> {
        sqlx::query(
            "INSERT INTO settings (key, value) VALUES (?, ?) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        )
        .bind(key)
        .bind(value)
        .execute(&self.pool)
        .await
        .map_err(banto_storage::storage_error)?;
        Ok(())
    }

    /// Read the server bind/port, falling back to [`ServerSettings::default`]
    /// for any key that has not been set yet (e.g. a fresh database).
    pub async fn server_config(&self) -> Result<ServerSettings, BantoError> {
        let defaults = ServerSettings::default();
        let bind = self.get(KEY_SERVER_BIND).await?.unwrap_or(defaults.bind);
        let port = self
            .get(KEY_SERVER_PORT)
            .await?
            .and_then(|value| value.parse::<u16>().ok())
            .unwrap_or(defaults.port);
        Ok(ServerSettings { bind, port })
    }

    pub async fn set_server_config(&self, config: &ServerSettings) -> Result<(), BantoError> {
        self.set(KEY_SERVER_BIND, &config.bind).await?;
        self.set(KEY_SERVER_PORT, &config.port.to_string()).await?;
        Ok(())
    }

    /// Read the tstore data dir / retention settings, falling back to
    /// [`StoreSettings::default`] for any unset key.
    pub async fn store_config(&self) -> Result<StoreSettings, BantoError> {
        let defaults = StoreSettings::default();
        let data_dir = self.get(KEY_DATA_DIR).await?.unwrap_or(defaults.data_dir);
        let retention_days = self
            .get(KEY_RETENTION_DAYS)
            .await?
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(defaults.retention_days);
        Ok(StoreSettings {
            data_dir,
            retention_days,
        })
    }

    pub async fn set_store_config(&self, config: &StoreSettings) -> Result<(), BantoError> {
        self.set(KEY_DATA_DIR, &config.data_dir).await?;
        self.set(KEY_RETENTION_DAYS, &config.retention_days.to_string())
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrate_memory;

    async fn service() -> SettingsService {
        let pool = migrate_memory().await.expect("migrate_memory");
        SettingsService::new(pool)
    }

    #[tokio::test]
    async fn get_missing_key_is_none() {
        let svc = service().await;
        assert_eq!(svc.get("nope").await.unwrap(), None);
    }

    #[tokio::test]
    async fn set_then_get_round_trips() {
        let svc = service().await;
        svc.set("k", "v").await.unwrap();
        assert_eq!(svc.get("k").await.unwrap(), Some("v".to_string()));
    }

    #[tokio::test]
    async fn server_config_defaults_when_unset() {
        let svc = service().await;
        let config = svc.server_config().await.unwrap();
        assert_eq!(config, ServerSettings::default());
        assert_eq!(config.bind, "127.0.0.1");
        assert_eq!(config.port, 8722);
    }

    #[tokio::test]
    async fn server_config_round_trips_through_set() {
        let svc = service().await;
        let config = ServerSettings {
            bind: "0.0.0.0".to_string(),
            port: 9000,
        };
        svc.set_server_config(&config).await.unwrap();
        assert_eq!(svc.server_config().await.unwrap(), config);
    }

    #[tokio::test]
    async fn store_config_defaults_when_unset() {
        let svc = service().await;
        let config = svc.store_config().await.unwrap();
        assert_eq!(config, StoreSettings::default());
        assert_eq!(config.data_dir, "./data");
        assert_eq!(config.retention_days, 7);
    }

    #[tokio::test]
    async fn store_config_round_trips_through_set() {
        let svc = service().await;
        let config = StoreSettings {
            data_dir: "/var/banto-hub/data".to_string(),
            retention_days: 14,
        };
        svc.set_store_config(&config).await.unwrap();
        assert_eq!(svc.store_config().await.unwrap(), config);
    }
}
