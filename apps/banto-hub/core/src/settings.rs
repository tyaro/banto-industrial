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
//! T4（docs/tag-server-design.md §5.4）で `grpc.*`（3キー、
//! [`GrpcSettings`]）を追加した。`grpc.enabled` の既定は `false`（設計
//! 「grpc.enabled(既定 false)」）- REST/WS と違い gRPC は既定で listen
//! しない、管理 UI で明示的に有効化する形（`WriteControl` の「起動時
//! disabled」ほど安全上の意味はないが、既定で新しいポートを勝手に開けない
//! という運用上の配慮）。
//!
//! H3（2026-08-08 オーナー決定、docs/improvement-plan.md H3）で
//! `grpc.bind` を追加した。gRPC は API キー認証必須だが TLS が無いため、
//! それまで `crate::grpc::GrpcServer::apply` が `"0.0.0.0:{port}"` を
//! リテラルで bind していたのは REST/WS の既定（`server.bind` =
//! `127.0.0.1`）と非対称かつ危険（有効化すると API キーが平文で全
//! インターフェースに流れる）だった。`grpc.bind` の既定を `127.0.0.1`
//! にし、LAN 公開は管理者が明示的に `0.0.0.0` 等へ変更する opt-in に
//! 揃える。既に gRPC を LAN 公開で運用していた環境は、アップグレード後に
//! `grpc.bind` の再設定が必要（意図した安全側の破壊的変更）。
//!
//! docs/banto-hub-remaining-plan.md P3-a（2026-08-12）で `audit.*`
//! （2キー、[`AuditSettings`]）を追加した - `crate::audit::AuditLogService`
//! の監査ログ retention（保持日数・保持件数の上限）を設定する。
//! `apps/chronogazer/core/src/settings.rs`・
//! `apps/relay-wright/core/src/settings.rs` の `AuditSettings` と
//! 設定形・既定値（90日/100,000件）・「0以下は無制限」規約が完全に同じ -
//! [`normalize_retention`]/[`parse_retention`] はそちらからそのまま移植
//! した。`crate::rest::audit_log_router`（`GET/PUT /api/audit-log/config`）
//! と `crate::runtime::HubRuntime::start`（起動時1回 + 24h 周期タスク、
//! P3-a 追補・2026-08-12 - `crate::runtime::audit_prune_once`のdoc
//! comment参照）がこの設定を読む。

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
const KEY_GRPC_BIND: &str = "grpc.bind";
const KEY_GRPC_PORT: &str = "grpc.port";

const KEY_AUDIT_RETENTION_DAYS: &str = "audit.retention_days";
const KEY_AUDIT_RETENTION_ROWS: &str = "audit.retention_rows";

/// 監査ログ retention の既定値（90日/100,000件） - chronogazer/relay-wright
/// の `AuditSettings` と同じ既定（このモジュールの doc comment参照）。
const DEFAULT_AUDIT_RETENTION_DAYS: i64 = 90;
const DEFAULT_AUDIT_RETENTION_ROWS: i64 = 100_000;

/// `0以下は「無制限」として None 扱い` - chronogazer/relay-wright の
/// `normalize_retention` と同一（このモジュールの doc comment参照）:
/// 保存済み値・[`SettingsService::set_audit_config`]直後の読み戻しの
/// どちらでも、非正の値は常に「無制限」に丸める。
fn normalize_retention(value: i64) -> Option<i64> {
    if value > 0 {
        Some(value)
    } else {
        None
    }
}

/// [`SettingsService::audit_config`]の2フィールドで共有: 未設定キーは
/// `default`にフォールバックし、設定済みだがパース不能な値も同様に
/// `default`にフォールバックする（他の `*_config` の壊れた値の扱いと
/// 同じ規約）。パース成功時は[`normalize_retention`]で正規化する - `"0"`
/// は「ユーザーが明示的に無制限を選んだ」ことを意味するため`default`へは
/// フォールバックしない。
fn parse_retention(raw: Option<String>, default: Option<i64>) -> Option<i64> {
    match raw {
        Some(value) => value
            .parse::<i64>()
            .map(normalize_retention)
            .unwrap_or(default),
        None => default,
    }
}

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
/// gRPC の既定 bind アドレス（2026-08-08 オーナー決定、
/// docs/improvement-plan.md H3: 「設定キー `grpc.bind` を追加し既定を
/// `127.0.0.1` に変更。公開は管理者の明示 opt-in」）。`ServerSettings`
/// （REST/WS、`server.bind`）と同じ既定値に揃える。
pub const DEFAULT_GRPC_BIND: &str = "127.0.0.1";

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
///
/// `retention_days`: T19 S2-d（UX-39、docs/banto-hub-t19-design.md §5.1）で
/// `i64` から `Option<i64>` へ変更した。**以前は素の `i64`（既定7）で、
/// `crate::runtime::prune_once` はこれをそのまま `u32` へキャストして
/// `banto_tstore::prune_files` に渡していた（`0` は「当日のみ保持」という
/// 意味だった）。今回から非正の値・未設定はすべて `None`＝「無制限（剪定
/// しない）」を意味するように変わる** - 監査ログ側の
/// [`AuditSettings::retention_days`]・[`normalize_retention`]/
/// [`parse_retention`] と同じ規約に揃えた（UX-39 のオーナー決定「無制限を
/// 選べるようにする」）。実運用では store 保持を 0 に設定する UI がこれまで
/// 存在しなかったため（REST/UI は本 T19 S2-d が最初の追加）、この意味変更
/// による実挙動の退行は無い。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoreSettings {
    pub data_dir: String,
    pub retention_days: Option<i64>,
}

impl Default for StoreSettings {
    fn default() -> Self {
        Self {
            data_dir: "./data".to_string(),
            retention_days: Some(DEFAULT_RETENTION_DAYS),
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
///
/// `bind`（2026-08-08 オーナー決定、docs/improvement-plan.md H3、既定
/// [`DEFAULT_GRPC_BIND`] = `"127.0.0.1"`）: `crate::grpc::GrpcServer::apply`
/// がこの値を `std::net::IpAddr` としてパースして bind する
/// （`ServerSettings::bind` と同じ「文字列のまま保持し、使う側が検証・
/// 変換する」層構造 — このモジュールでは形式検証しない。不正な文字列が
/// DB に直接書き込まれた場合の扱いは `GrpcServer::apply` のdoc comment
/// 参照）。`String` フィールドを持つため `Copy` は付けない
/// （`MqttSettings`/`ServerSettings` と同じ判断）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrpcSettings {
    pub enabled: bool,
    pub bind: String,
    pub port: u16,
}

impl Default for GrpcSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            bind: DEFAULT_GRPC_BIND.to_string(),
            port: DEFAULT_GRPC_PORT,
        }
    }
}

/// 監査ログ retention 設定（docs/banto-hub-remaining-plan.md P3-a）:
/// [`crate::audit::AuditLogService::prune`]に渡す日数上限・件数上限。
/// どちらのフィールドも `None` はその軸が無制限であることを意味する
/// （[`normalize_retention`]参照）。
///
/// `Deserialize`（`Serialize`に加えて）が必要 - `crate::rest`の
/// `GET/PUT /api/audit-log/config` がこの型を直接 request/response body
/// として使う（`crate::rest::GrpcSettingsBody`等と違い、専用の request
/// 型を起こしていない - 秘匿フィールドが無い単純な2フィールド設定値
/// なので素の`AuditSettings`をそのまま往復させて問題ない）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditSettings {
    pub retention_days: Option<i64>,
    pub retention_rows: Option<i64>,
}

impl Default for AuditSettings {
    fn default() -> Self {
        Self {
            retention_days: Some(DEFAULT_AUDIT_RETENTION_DAYS),
            retention_rows: Some(DEFAULT_AUDIT_RETENTION_ROWS),
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
    /// [`StoreSettings::default`] for any unset key. `retention_days` uses
    /// the same [`parse_retention`] as the audit log settings - a
    /// non-positive or unparsable stored value normalizes to `None`
    /// (unlimited), see [`StoreSettings::retention_days`]'s doc comment for
    /// why this crosses from a plain `i64` to `Option<i64>`.
    pub async fn store_config(&self) -> Result<StoreSettings, BantoError> {
        let defaults = StoreSettings::default();
        let data_dir = self.get(KEY_DATA_DIR).await?.unwrap_or(defaults.data_dir);
        let retention_days =
            parse_retention(self.get(KEY_RETENTION_DAYS).await?, defaults.retention_days);
        Ok(StoreSettings {
            data_dir,
            retention_days,
        })
    }

    /// Save the tstore data dir / retention settings. `retention_days` of
    /// `None` is written as `"0"` - the same "no separate sentinel needed"
    /// convention as [`Self::set_audit_config`], since [`parse_retention`]
    /// already treats a stored `"0"` as unlimited on the read side.
    pub async fn set_store_config(&self, config: &StoreSettings) -> Result<(), BantoError> {
        self.set(KEY_DATA_DIR, &config.data_dir).await?;
        self.set(
            KEY_RETENTION_DAYS,
            &config.retention_days.unwrap_or(0).to_string(),
        )
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
    /// にフォールバック（既定 `enabled: false`、`bind: "127.0.0.1"`。
    /// 2026-08-08 オーナー決定、docs/improvement-plan.md H3）。`bind` は
    /// `server_config`の`bind`と同じく、ここでは文字列のまま読むだけで
    /// 形式検証はしない（[`GrpcSettings`]のdoc comment参照）。
    pub async fn grpc_config(&self) -> Result<GrpcSettings, BantoError> {
        let defaults = GrpcSettings::default();
        let enabled = self
            .get(KEY_GRPC_ENABLED)
            .await?
            .map(|value| value == "true")
            .unwrap_or(defaults.enabled);
        let bind = self.get(KEY_GRPC_BIND).await?.unwrap_or(defaults.bind);
        let port = self
            .get(KEY_GRPC_PORT)
            .await?
            .and_then(|value| value.parse::<u16>().ok())
            .unwrap_or(defaults.port);
        Ok(GrpcSettings {
            enabled,
            bind,
            port,
        })
    }

    pub async fn set_grpc_config(&self, config: &GrpcSettings) -> Result<(), BantoError> {
        self.set(
            KEY_GRPC_ENABLED,
            if config.enabled { "true" } else { "false" },
        )
        .await?;
        self.set(KEY_GRPC_BIND, &config.bind).await?;
        self.set(KEY_GRPC_PORT, &config.port.to_string()).await?;
        Ok(())
    }

    /// 監査ログ retention 設定を読む。未設定キーは
    /// [`AuditSettings::default`]にフォールバックする（[`parse_retention`]
    /// 参照）。
    pub async fn audit_config(&self) -> Result<AuditSettings, BantoError> {
        let defaults = AuditSettings::default();

        let retention_days = parse_retention(
            self.get(KEY_AUDIT_RETENTION_DAYS).await?,
            defaults.retention_days,
        );
        let retention_rows = parse_retention(
            self.get(KEY_AUDIT_RETENTION_ROWS).await?,
            defaults.retention_rows,
        );

        Ok(AuditSettings {
            retention_days,
            retention_rows,
        })
    }

    /// 監査ログ retention 設定を保存する。`None`は`"0"`として書き込む -
    /// [`parse_retention`]が読み取り側で非正の値を「無制限」に丸めるため、
    /// 「未設定かどうか」を区別するための別センチネルは不要（chronogazer/
    /// relay-wright の`set_audit_config`と同じ規約）。
    pub async fn set_audit_config(&self, config: &AuditSettings) -> Result<(), BantoError> {
        self.set(
            KEY_AUDIT_RETENTION_DAYS,
            &config.retention_days.unwrap_or(0).to_string(),
        )
        .await?;
        self.set(
            KEY_AUDIT_RETENTION_ROWS,
            &config.retention_rows.unwrap_or(0).to_string(),
        )
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
        assert_eq!(config.retention_days, Some(7));
    }

    #[tokio::test]
    async fn store_config_round_trips_through_set() {
        let svc = service().await;
        let config = StoreSettings {
            data_dir: "/var/banto-hub/data".to_string(),
            retention_days: Some(14),
        };
        svc.set_store_config(&config).await.unwrap();
        assert_eq!(svc.store_config().await.unwrap(), config);
    }

    /// UX-39（無制限の選択肢）: `None` は保存側で `"0"` として書かれ、
    /// 読み取り側は [`parse_retention`] で `None` に丸め戻る - 監査ログの
    /// `audit_config_none_round_trips_as_unlimited` と同じ規約。
    #[tokio::test]
    async fn store_config_none_round_trips_as_unlimited() {
        let svc = service().await;
        svc.set_store_config(&StoreSettings {
            data_dir: "./data".to_string(),
            retention_days: None,
        })
        .await
        .unwrap();

        let config = svc.store_config().await.unwrap();
        assert_eq!(config.retention_days, None);
    }

    /// 防御的な扱い: 保存経路を通らず直接キーへ非正の値が書き込まれた
    /// 場合でも「無制限」に丸める（監査ログの
    /// `audit_config_non_positive_value_normalizes_to_unlimited` と同じ）。
    #[tokio::test]
    async fn store_config_non_positive_value_normalizes_to_unlimited() {
        let svc = service().await;
        svc.set(KEY_RETENTION_DAYS, "-3").await.unwrap();

        let config = svc.store_config().await.unwrap();
        assert_eq!(config.retention_days, None);
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
        assert_eq!(config.bind, "127.0.0.1");
        assert_eq!(config.port, 50051);
    }

    #[tokio::test]
    async fn grpc_config_round_trips_through_set() {
        let svc = service().await;
        let config = GrpcSettings {
            enabled: true,
            bind: "0.0.0.0".to_string(),
            port: 51000,
        };
        svc.set_grpc_config(&config).await.unwrap();
        assert_eq!(svc.grpc_config().await.unwrap(), config);
    }

    /// アップグレード互換性（2026-08-08 オーナー決定、
    /// docs/improvement-plan.md H3）: `grpc.bind` を導入する前から
    /// `grpc.enabled`/`grpc.port` だけが保存されている DB（＝この機能が
    /// 無かった頃の既存環境）でも、`grpc.bind` キーが無ければ
    /// [`DEFAULT_GRPC_BIND`]（`"127.0.0.1"`）にフォールバックする
    /// （`grpc.bind` 抜きで直接キーを書き込み、他の2キーだけが設定された
    /// 状態を再現する）。
    #[tokio::test]
    async fn grpc_config_bind_defaults_to_loopback_when_only_bind_key_is_missing() {
        let svc = service().await;
        svc.set(KEY_GRPC_ENABLED, "true").await.unwrap();
        svc.set(KEY_GRPC_PORT, "51000").await.unwrap();

        let config = svc.grpc_config().await.unwrap();
        assert!(config.enabled);
        assert_eq!(config.port, 51000);
        assert_eq!(config.bind, "127.0.0.1");
    }

    // --- 監査ログ retention 設定（docs/banto-hub-remaining-plan.md P3-a）---

    #[tokio::test]
    async fn audit_config_defaults_when_unset() {
        let svc = service().await;
        let config = svc.audit_config().await.unwrap();
        assert_eq!(config, AuditSettings::default());
        assert_eq!(config.retention_days, Some(90));
        assert_eq!(config.retention_rows, Some(100_000));
    }

    #[tokio::test]
    async fn audit_config_round_trips_through_set() {
        let svc = service().await;
        let config = AuditSettings {
            retention_days: Some(30),
            retention_rows: Some(5_000),
        };
        svc.set_audit_config(&config).await.unwrap();
        assert_eq!(svc.audit_config().await.unwrap(), config);
    }

    /// `None` は「そのフィールドは無制限」として保存・読み戻る
    /// （[`normalize_retention`]参照）。
    #[tokio::test]
    async fn audit_config_none_round_trips_as_unlimited() {
        let svc = service().await;
        svc.set_audit_config(&AuditSettings {
            retention_days: None,
            retention_rows: None,
        })
        .await
        .unwrap();

        let config = svc.audit_config().await.unwrap();
        assert_eq!(config.retention_days, None);
        assert_eq!(config.retention_rows, None);
    }

    /// 片方のキーだけ明示的に無制限へ変更しても、もう片方は未変更の
    /// キーとして残り、次回読み取りでも [`AuditSettings::default`]
    /// にはフォールバックしない（`set` 済みキーは常にそのまま読める -
    /// [`parse_retention`]が「設定済みだが値0」を無制限と区別して扱う
    /// ことの確認）。
    #[tokio::test]
    async fn audit_config_days_only_change_leaves_rows_untouched() {
        let svc = service().await;
        svc.set(KEY_AUDIT_RETENTION_DAYS, "0").await.unwrap();

        let config = svc.audit_config().await.unwrap();
        assert_eq!(config.retention_days, None);
        assert_eq!(config.retention_rows, Some(100_000)); // 未設定キーは既定値のまま
    }

    /// 非正の値は保存経路を通らず直接キーに書き込まれた場合でも
    /// 「無制限」に丸める（[`normalize_retention`]の防御的な扱い）。
    #[tokio::test]
    async fn audit_config_non_positive_value_normalizes_to_unlimited() {
        let svc = service().await;
        svc.set(KEY_AUDIT_RETENTION_DAYS, "-5").await.unwrap();

        let config = svc.audit_config().await.unwrap();
        assert_eq!(config.retention_days, None);
    }
}
