//! Hub 側のサイドカー専用 API（設計 §5.2・§6-15）へのクライアント:
//! `GET /api/sink/config`（設定の取得）と `PUT /api/sink/status`（運転状態の
//! push）。どちらも `Authorization: Bearer <api_key>` の admin スコープ限定
//! （`banto_hub_core::rest::require_sink_admin`）。
//!
//! ## DTO は Hub 側の応答型の鏡写し
//!
//! [`SinkConfig`]/[`SinkConfigGroup`]/[`SinkConfigTag`]/
//! [`SinkConfigConnection`] は `apps/banto-hub/core/src/rest.rs` の
//! `SinkConfigResponse`/`SinkConfigGroupEntry`/`SinkConfigTagEntry`/
//! `SinkConfigConnectionEntry`（camelCase）と 1 対 1。Hub 側に項目が
//! 増えても壊れないよう `deny_unknown_fields` は**付けない**（設定
//! ファイルとは逆の判断: あちらは人間の綴り間違いを弾きたい、こちらは
//! Hub の前方互換を保ちたい）。
//!
//! ## パスワードを運ぶ応答である
//!
//! `connections[].password` は**平文**（§6-15 のオーナー決定: admin
//! スコープ + ループバック運用がその埋め合わせ）。この型の値は
//! `Debug` でダンプしない - [`SinkConfigConnection`] は `Debug` を
//! 手書きし、パスワードを `<redacted>` にしてある。
//!
//! ## エラー
//!
//! [`HubApiError`] は「認証（401/403）」「その他の HTTP ステータス」
//! 「トランスポート」「本文が解釈できない」の 4 分類だけを持つ。
//! 401/403 は**設定の問題**（キーが失効した・スコープが足りない）で
//! 再試行では直らないため、呼び出し側は 1 回だけ `warn` してバック
//! オフする（設計 §5.2 の実装指示）。

use std::fmt;
use std::time::Duration;

use reqwest::{Client, ClientBuilder, StatusCode};
use serde::{Deserialize, Serialize};

use crate::config::SidecarConfig;

/// 1 リクエストの上限。Hub は同一マシンのループバックなので短くてよいが、
/// 起動直後（Hub が SQLite を開いている最中）に取りこぼさない程度の余裕は
/// 見る。
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// `GET /api/sink/config` の応答（Hub 側 `SinkConfigResponse`）。
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SinkConfig {
    pub generated_at: i64,
    pub groups: Vec<SinkConfigGroup>,
    pub connections: Vec<SinkConfigConnection>,
}

/// 有効な sink group 1 件（Hub 側 `SinkConfigGroupEntry`）。
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SinkConfigGroup {
    pub id: i64,
    pub name: String,
    pub db_connection_id: i64,
    /// `"interval"` | `"on_change"`（`banto_hub_core::sink::
    /// ALLOWED_SINK_MODES`）。未知の値は起動を拒否せず、そのグループだけ
    /// `error` にする（前方互換 - [`crate::group::SinkMode::parse`]）。
    pub mode: String,
    pub interval_ms: i64,
    pub table_name: String,
    pub store_bad: bool,
    pub tags: Vec<SinkConfigTag>,
}

/// 対象タグ 1 件（Hub 側 `SinkConfigTagEntry`）。3 つ組は
/// `banto_tagclient::StableTagId` と完全に同じ意味。
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SinkConfigTag {
    pub connection_id: i64,
    pub group_id: i64,
    pub tag_id: i64,
    /// Hub の現在の catalog 上の外部名（`{connection}.{group}.{tag}`）。
    /// Hub は `CollectorManager::tag_map()` から詰めるので、**取得時点の
    /// 最新**である。リネーム直後は次の設定取得までこちらが古くなりうる
    /// （[`crate::values`] のモジュール doc「リネームの追従」参照）。
    pub external_name: String,
}

/// DB 接続 1 件（Hub 側 `SinkConfigConnectionEntry`）。**パスワードを
/// 平文で含む** - このモジュールの doc comment参照。
#[derive(Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SinkConfigConnection {
    pub id: i64,
    pub name: String,
    pub host: String,
    pub port: i64,
    pub database: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
}

impl fmt::Debug for SinkConfigConnection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SinkConfigConnection")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("database", &self.database)
            .field("username", &self.username)
            .field(
                "password",
                &self
                    .password
                    .as_ref()
                    .map(|_| "<redacted>")
                    .unwrap_or("None"),
            )
            .finish()
    }
}

/// `PUT /api/sink/status` のボディ（Hub 側 `SinkStatusPushRequest`）。
#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SinkStatusPush {
    pub groups: Vec<SinkGroupStatusPush>,
}

/// 1 グループ分の運転状態（Hub 側 `banto_hub_core::sink::
/// SinkGroupStatusPush` と同じ wire 形）。`last_error` は
/// [`crate::status::Redactor`] を通した後の文字列だけを載せる。
#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SinkGroupStatusPush {
    pub id: i64,
    pub state: &'static str,
    pub queued: u64,
    pub dropped: u64,
    pub last_flush_at: Option<i64>,
    pub last_error: Option<String>,
}

/// Hub 呼び出しの失敗（このモジュールの doc comment「エラー」参照）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HubApiError {
    /// 401/403 - キーの失効・スコープ不足。再試行では直らない。
    Unauthorized,
    /// 2xx 以外（上記を除く）。
    Status(u16),
    /// 接続できない・タイムアウト（Hub 未起動を含む正常な待ち状態）。
    Transport,
    /// 本文が期待した JSON ではない。
    Protocol,
}

impl fmt::Display for HubApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unauthorized => f.write_str(
                "Hub に拒否されました（401/403）。API キーのスコープ（admin と read）と有効性を確認してください",
            ),
            Self::Status(code) => write!(f, "Hub が想定外のステータスを返しました: HTTP {code}"),
            Self::Transport => f.write_str("Hub へ接続できません（未起動・ポート・ファイアウォール）"),
            Self::Protocol => f.write_str("Hub の応答を解釈できませんでした"),
        }
    }
}

impl HubApiError {
    /// バックオフの対象か（`Unauthorized` は再試行しても直らないが、
    /// キーの再発行で直るので**やめずに**間隔を空けて再試行する -
    /// 呼び出し側はログだけ 1 回に抑える）。
    pub fn is_auth(&self) -> bool {
        matches!(self, Self::Unauthorized)
    }
}

/// Hub のサイドカー API クライアント。`reqwest::Client` を 1 つ持ち回る
/// （接続を使い回すため）。プロキシとリダイレクトは無効
/// （`banto_tagclient::RestClient` と同じ理由: ループバックの Hub へ
/// 環境変数由来のプロキシを通さない）。
pub struct HubApi {
    http: Client,
    config_url: String,
    status_url: String,
    api_key: String,
}

impl HubApi {
    pub fn new(config: &SidecarConfig) -> Result<Self, HubApiError> {
        let http = ClientBuilder::new()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|_| HubApiError::Transport)?;
        let base = config.hub_url.trim_end_matches('/');
        Ok(Self {
            http,
            config_url: format!("{base}/api/sink/config"),
            status_url: format!("{base}/api/sink/status"),
            api_key: config.api_key.clone(),
        })
    }

    /// `GET /api/sink/config`。
    pub async fn fetch_config(&self) -> Result<SinkConfig, HubApiError> {
        let response = self
            .http
            .get(&self.config_url)
            .bearer_auth(&self.api_key)
            .send()
            .await
            .map_err(|_| HubApiError::Transport)?;
        let status = response.status();
        if !status.is_success() {
            return Err(classify_status(status));
        }
        let body = response.bytes().await.map_err(|_| HubApiError::Transport)?;
        serde_json::from_slice(&body).map_err(|_| HubApiError::Protocol)
    }

    /// `PUT /api/sink/status`（成功は 204 No Content）。
    pub async fn push_status(&self, body: &SinkStatusPush) -> Result<(), HubApiError> {
        let payload = serde_json::to_vec(body).map_err(|_| HubApiError::Protocol)?;
        let response = self
            .http
            .put(&self.status_url)
            .bearer_auth(&self.api_key)
            .header("content-type", "application/json")
            .body(payload)
            .send()
            .await
            .map_err(|_| HubApiError::Transport)?;
        let status = response.status();
        if status.is_success() {
            Ok(())
        } else {
            Err(classify_status(status))
        }
    }
}

fn classify_status(status: StatusCode) -> HubApiError {
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        HubApiError::Unauthorized
    } else {
        HubApiError::Status(status.as_u16())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "generatedAt": 1000,
      "groups": [{
        "id": 7, "name": "line1-log", "dbConnectionId": 3, "mode": "interval",
        "intervalMs": 1000, "tableName": "public.tag_history", "storeBad": false,
        "tags": [{"connectionId": 1, "groupId": 2, "tagId": 5, "externalName": "line1.fast.temp01"}]
      }],
      "connections": [{
        "id": 3, "name": "erp-db", "host": "127.0.0.1", "port": 5432,
        "database": "erp", "username": "app", "password": "s3cret"
      }]
    }"#;

    #[test]
    fn config_response_maps_the_hub_wire_shape() {
        let parsed: SinkConfig = serde_json::from_str(SAMPLE).expect("parse");
        assert_eq!(parsed.generated_at, 1000);
        assert_eq!(parsed.groups.len(), 1);
        assert_eq!(parsed.groups[0].table_name, "public.tag_history");
        assert!(!parsed.groups[0].store_bad);
        assert_eq!(parsed.groups[0].tags[0].tag_id, 5);
        assert_eq!(parsed.groups[0].tags[0].external_name, "line1.fast.temp01");
        assert_eq!(parsed.connections[0].password.as_deref(), Some("s3cret"));
    }

    /// Hub 側に項目が増えても壊れない（前方互換 - このモジュールの doc
    /// comment参照）。
    #[test]
    fn unknown_response_fields_are_ignored() {
        let raw = SAMPLE.replace(
            "\"generatedAt\": 1000,",
            "\"generatedAt\": 1000, \"futureField\": {\"x\": 1},",
        );
        let parsed: SinkConfig = serde_json::from_str(&raw).expect("parse with unknown field");
        assert_eq!(parsed.generated_at, 1000);
    }

    /// 接続の `Debug` にパスワードを出さない。
    #[test]
    fn connection_debug_redacts_the_password() {
        let parsed: SinkConfig = serde_json::from_str(SAMPLE).expect("parse");
        let rendered = format!("{:?}", parsed.connections[0]);
        assert!(!rendered.contains("s3cret"), "{rendered}");
        assert!(rendered.contains("<redacted>"), "{rendered}");
    }

    #[test]
    fn status_push_body_is_camel_case() {
        let body = SinkStatusPush {
            groups: vec![SinkGroupStatusPush {
                id: 7,
                state: "running",
                queued: 3,
                dropped: 1,
                last_flush_at: Some(1234),
                last_error: None,
            }],
        };
        let json = serde_json::to_string(&body).expect("serialize");
        assert!(json.contains("\"lastFlushAt\":1234"), "{json}");
        assert!(json.contains("\"lastError\":null"), "{json}");
        assert!(json.contains("\"queued\":3"), "{json}");
    }

    #[test]
    fn status_codes_are_classified() {
        assert_eq!(
            classify_status(StatusCode::UNAUTHORIZED),
            HubApiError::Unauthorized
        );
        assert_eq!(
            classify_status(StatusCode::FORBIDDEN),
            HubApiError::Unauthorized
        );
        assert_eq!(
            classify_status(StatusCode::UNPROCESSABLE_ENTITY),
            HubApiError::Status(422)
        );
        assert!(HubApiError::Unauthorized.is_auth());
        assert!(!HubApiError::Transport.is_auth());
    }
}
