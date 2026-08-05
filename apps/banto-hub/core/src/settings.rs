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
//!
//! T3（docs/tag-server-design.md §5.3）で `mqtt.*`（9キー、[`MqttSettings`]）
//! を追加した。`mqtt.password` は**平文保存** — §5.6「v1 では平文 + 閉域
//! LAN 前提」と同じ線引きで、[`MqttSettings`] のフィールド doc comment に
//! 判断根拠を記す。
//!
//! T4（docs/tag-server-design.md §5.4）で `grpc.*`（2キー、
//! [`GrpcSettings`]）を追加した。`grpc.enabled` の既定は `false`（設計
//! 「grpc.enabled(既定 false)」）- REST/WS と違い gRPC は既定で listen
//! しない、管理 UI で明示的に有効化する形（`WriteControl` の「起動時
//! disabled」ほど安全上の意味はないが、既定で新しいポートを勝手に開けない
//! という運用上の配慮）。

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use banto_core::BantoError;

const KEY_SERVER_BIND: &str = "server.bind";
const KEY_SERVER_PORT: &str = "server.port";
const KEY_DATA_DIR: &str = "data.dir";
const KEY_RETENTION_DAYS: &str = "retention.days";

const KEY_MQTT_ENABLED: &str = "mqtt.enabled";
const KEY_MQTT_HOST: &str = "mqtt.host";
const KEY_MQTT_PORT: &str = "mqtt.port";
const KEY_MQTT_CLIENT_ID: &str = "mqtt.client_id";
const KEY_MQTT_USERNAME: &str = "mqtt.username";
const KEY_MQTT_PASSWORD: &str = "mqtt.password";
const KEY_MQTT_PREFIX: &str = "mqtt.prefix";
const KEY_MQTT_QOS: &str = "mqtt.qos";
const KEY_MQTT_MIN_INTERVAL_MS: &str = "mqtt.min_interval_ms";

const KEY_GRPC_ENABLED: &str = "grpc.enabled";
const KEY_GRPC_PORT: &str = "grpc.port";

/// hub の既定ポート（docs/tag-server-design.md §8: 「管理 UI + REST + WS =
/// 8722」）。
pub const DEFAULT_PORT: u16 = 8722;
/// tstore 保持期間の既定日数（§3.3 (a) 決定: 「保持期間は既定7日」）。
pub const DEFAULT_RETENTION_DAYS: i64 = 7;

/// MQTT ブローカーの既定ポート（設計 §5.3）。
pub const DEFAULT_MQTT_PORT: u16 = 1883;
/// MQTT クライアント ID の既定値（設計 §5.3）。
pub const DEFAULT_MQTT_CLIENT_ID: &str = "banto-hub";
/// トピック prefix の既定値（設計 §5.3「prefix 既定 `banto`」）。
pub const DEFAULT_MQTT_PREFIX: &str = "banto";
/// QoS の既定値（設計 §5.3「既定 1」）。
pub const DEFAULT_MQTT_QOS: u8 = 1;
/// 最短発行間隔スロットルの既定値（実装指示: 「既定 1000」）。
pub const DEFAULT_MQTT_MIN_INTERVAL_MS: i64 = 1000;

/// gRPC の既定ポート（設計 §5.4/§8「既定: REST 880x 系 / gRPC 50051」）。
pub const DEFAULT_GRPC_PORT: u16 = 50051;

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

/// T3（docs/tag-server-design.md §5.3）: MQTT publish の接続/発行設定。
///
/// `password` は settings テーブルに**平文で保存**する（実装指示「settings
/// テーブルに平文保存 — 閉域 LAN 前提 §5.6 の範囲。doc に明記」）。判断
/// 根拠: ブローカー自体が同一閉域 LAN 内にある前提（§5.6「v1 では平文 +
/// 閉域 LAN 前提」）で、TLS 終端も導入するならリバースプロキシに委譲する
/// 設計（同節）と同じ線引き — ハッシュ化してもクライアントへ渡す瞬間に
/// 平文へ復元する必要があるため保護にならない（api_keys.rs のキーとは
/// 性質が違う: あちらは照合用のワンウェイハッシュで足りるが、こちらは
/// ブローカーへの認証情報そのものを送信する必要がある）。
///
/// `GET /api/mqtt-settings` は `password` を一切返さない
/// （`crate::rest::MqttSettingsResponse` にフィールド自体が無い）。
/// `PUT` の `password` は空文字を「変更なし」として扱う
/// （`crate::rest::mqtt_settings_put` 参照）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MqttSettings {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    pub client_id: String,
    pub username: Option<String>,
    pub password: Option<String>,
    pub prefix: String,
    pub qos: u8,
    pub min_interval_ms: i64,
}

impl Default for MqttSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            host: String::new(),
            port: DEFAULT_MQTT_PORT,
            client_id: DEFAULT_MQTT_CLIENT_ID.to_string(),
            username: None,
            password: None,
            prefix: DEFAULT_MQTT_PREFIX.to_string(),
            qos: DEFAULT_MQTT_QOS,
            min_interval_ms: DEFAULT_MQTT_MIN_INTERVAL_MS,
        }
    }
}

/// T4（docs/tag-server-design.md §5.4）: gRPC サーバーの設定。REST とは
/// 別ポートで listen する（設計「ポートは REST と分離」）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GrpcSettings {
    pub enabled: bool,
    pub port: u16,
}

impl Default for GrpcSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            port: DEFAULT_GRPC_PORT,
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

    /// T3（設計 §5.3）: MQTT publish 設定、未設定キーは
    /// [`MqttSettings::default`] にフォールバック。`username`/`password` は
    /// 空文字を「未設定」（`None`）に丸める — `set_mqtt_config` が空文字を
    /// そのまま書く経路（`crate::rest::mqtt_settings_put` の「host 必須で
    /// なければ空でも保存してよい」入力）と対称にするため。
    pub async fn mqtt_config(&self) -> Result<MqttSettings, BantoError> {
        let defaults = MqttSettings::default();
        let enabled = self
            .get(KEY_MQTT_ENABLED)
            .await?
            .map(|value| value == "true")
            .unwrap_or(defaults.enabled);
        let host = self.get(KEY_MQTT_HOST).await?.unwrap_or(defaults.host);
        let port = self
            .get(KEY_MQTT_PORT)
            .await?
            .and_then(|value| value.parse::<u16>().ok())
            .unwrap_or(defaults.port);
        let client_id = self
            .get(KEY_MQTT_CLIENT_ID)
            .await?
            .unwrap_or(defaults.client_id);
        let username = self
            .get(KEY_MQTT_USERNAME)
            .await?
            .filter(|value| !value.is_empty());
        let password = self
            .get(KEY_MQTT_PASSWORD)
            .await?
            .filter(|value| !value.is_empty());
        let prefix = self.get(KEY_MQTT_PREFIX).await?.unwrap_or(defaults.prefix);
        let qos = self
            .get(KEY_MQTT_QOS)
            .await?
            .and_then(|value| value.parse::<u8>().ok())
            .unwrap_or(defaults.qos);
        let min_interval_ms = self
            .get(KEY_MQTT_MIN_INTERVAL_MS)
            .await?
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(defaults.min_interval_ms);
        Ok(MqttSettings {
            enabled,
            host,
            port,
            client_id,
            username,
            password,
            prefix,
            qos,
            min_interval_ms,
        })
    }

    pub async fn set_mqtt_config(&self, config: &MqttSettings) -> Result<(), BantoError> {
        self.set(
            KEY_MQTT_ENABLED,
            if config.enabled { "true" } else { "false" },
        )
        .await?;
        self.set(KEY_MQTT_HOST, &config.host).await?;
        self.set(KEY_MQTT_PORT, &config.port.to_string()).await?;
        self.set(KEY_MQTT_CLIENT_ID, &config.client_id).await?;
        self.set(KEY_MQTT_USERNAME, config.username.as_deref().unwrap_or(""))
            .await?;
        self.set(KEY_MQTT_PASSWORD, config.password.as_deref().unwrap_or(""))
            .await?;
        self.set(KEY_MQTT_PREFIX, &config.prefix).await?;
        self.set(KEY_MQTT_QOS, &config.qos.to_string()).await?;
        self.set(
            KEY_MQTT_MIN_INTERVAL_MS,
            &config.min_interval_ms.to_string(),
        )
        .await?;
        Ok(())
    }

    /// T4（設計 §5.4）: gRPC 設定、未設定キーは [`GrpcSettings::default`]
    /// にフォールバック（既定 `enabled: false`）。
    pub async fn grpc_config(&self) -> Result<GrpcSettings, BantoError> {
        let defaults = GrpcSettings::default();
        let enabled = self
            .get(KEY_GRPC_ENABLED)
            .await?
            .map(|value| value == "true")
            .unwrap_or(defaults.enabled);
        let port = self
            .get(KEY_GRPC_PORT)
            .await?
            .and_then(|value| value.parse::<u16>().ok())
            .unwrap_or(defaults.port);
        Ok(GrpcSettings { enabled, port })
    }

    pub async fn set_grpc_config(&self, config: &GrpcSettings) -> Result<(), BantoError> {
        self.set(
            KEY_GRPC_ENABLED,
            if config.enabled { "true" } else { "false" },
        )
        .await?;
        self.set(KEY_GRPC_PORT, &config.port.to_string()).await?;
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

    #[tokio::test]
    async fn mqtt_config_defaults_when_unset() {
        let svc = service().await;
        let config = svc.mqtt_config().await.unwrap();
        assert_eq!(config, MqttSettings::default());
        assert!(!config.enabled);
        assert_eq!(config.port, 1883);
        assert_eq!(config.client_id, "banto-hub");
        assert_eq!(config.prefix, "banto");
        assert_eq!(config.qos, 1);
        assert_eq!(config.min_interval_ms, 1000);
        assert_eq!(config.username, None);
        assert_eq!(config.password, None);
    }

    #[tokio::test]
    async fn mqtt_config_round_trips_through_set() {
        let svc = service().await;
        let config = MqttSettings {
            enabled: true,
            host: "broker.local".to_string(),
            port: 8883,
            client_id: "hub-1".to_string(),
            username: Some("user1".to_string()),
            password: Some("s3cret".to_string()),
            prefix: "factory1".to_string(),
            qos: 0,
            min_interval_ms: 500,
        };
        svc.set_mqtt_config(&config).await.unwrap();
        assert_eq!(svc.mqtt_config().await.unwrap(), config);
    }

    /// `username`/`password` は空文字で保存すると「未設定」（`None`）として
    /// 読み戻る（このモジュールの doc comment「`mqtt_config`」参照）。
    #[tokio::test]
    async fn mqtt_config_empty_username_and_password_read_back_as_none() {
        let svc = service().await;
        let config = MqttSettings {
            username: Some(String::new()),
            password: Some(String::new()),
            ..MqttSettings::default()
        };
        svc.set_mqtt_config(&config).await.unwrap();
        let read_back = svc.mqtt_config().await.unwrap();
        assert_eq!(read_back.username, None);
        assert_eq!(read_back.password, None);
    }

    #[tokio::test]
    async fn grpc_config_defaults_when_unset() {
        let svc = service().await;
        let config = svc.grpc_config().await.unwrap();
        assert_eq!(config, GrpcSettings::default());
        assert!(!config.enabled);
        assert_eq!(config.port, 50051);
    }

    #[tokio::test]
    async fn grpc_config_round_trips_through_set() {
        let svc = service().await;
        let config = GrpcSettings {
            enabled: true,
            port: 51000,
        };
        svc.set_grpc_config(&config).await.unwrap();
        assert_eq!(svc.grpc_config().await.unwrap(), config);
    }
}
