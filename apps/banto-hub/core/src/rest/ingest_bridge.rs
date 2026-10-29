//! DB ブリッジの設定の取得口 `GET /api/ingest/bridge-config`（R2a-3b、
//! docs/banto-hub-ingest-design.md §10.2）。
//!
//! DB ブリッジ（`banto-hub-sink` の `source/`、§10.3）は、`settings.bridge`
//! （`kind: "db"`）を持つ `push` 接続ごとに、参照先の `postgres` 接続へ問い合わせて
//! 取り込み口 `/api/v1/ingest/{id}/stream` へ値を送る。そのために要るもの（送り先の
//! 接続 id・クエリ・列とキーの対応・読む DB の接続先とパスワード）をここで渡す。
//!
//! ## ゲート（§10.2、§12 Q13 = (a)「新しい信頼の形を作らない」）
//!
//! `GET /api/sink/config` と**同じ** [`super::sink::require_sink_admin`]（`admin`
//! スコープの API キーか admin ロールのセッション）。新しいスコープ・新しい置き場は
//! 作らない。`/api/sink/config` には混ぜない（Sink の設定は Sink のグループの単位で、
//! 取り込みとは寿命が違う。§12 Q13）。CSRF のレイヤーの外側（`/api/sink/*`・
//! `/api/v1/*` と同じ扱い）。`/api/v1/*` ではないので OpenAPI には載せない
//! （`/api/sink/config` と同じ）。
//!
//! ## パスワードを返す（§11 のオーナーの監査 (5)）
//!
//! `db.password` は参照先の `postgres` 接続の `password` 列の**平文**（§12 Q7・Q8:
//! `banto-secret-store` へ移すのは R2b-1）。埋め合わせは `/api/sink/config` と同じく
//! admin のゲートと「Hub とブリッジは同じマシンのループバック」の運用（初回の呼び出しで
//! 1 回だけ warn する。external-db §2.2・§6-15 の延長）。加えて:
//!
//! - **ログに出さない**: このファイルはパスワードも応答の本文もログに書かない。
//!   捨てた source の warn も接続の id と名前だけ。
//! - **送れない source のパスワードは渡さない**: 呼び出し手の API キーがその接続の
//!   `ingest:#{id}` を持たないとき（`ingestAllowed: false`、admin ロールのセッションも
//!   含む）は `db.password` を `null` にする。ブリッジはその source を動かさない
//!   （§10.3「スコープの無い接続は動かさない」、§11 の R2a-3b の監査）ので、パスワードは
//!   要らない。
//!
//! ## 何を返すか
//!
//! 有効な `push` 接続のうち `settings.bridge.kind == "db"` のもの（§10.2）。`settings` は
//! 書き込みの全入口が `banto_tags::validate_push_settings`（§7.1 の見積もりを含む）を
//! 通すので、ここに出るのは保存できた設定 = 送りきれる設定だけ。読めない設定（手で
//! 書き換えた DB）と、参照先が `postgres` 接続でない・無いもの（削除は断るので本来は
//! 起きない）は載せずに warn を 1 行出す。
//!
//! - `configFingerprint`: `settings.bridge` の JSON の SHA-256（`"sha256:{hex}"`）。
//!   `crate::ingest::IngestHub` が epoch を進める「設定の変更」（§4.6）と同じ単位で
//!   変わる。ブリッジは指紋が変わった source を止めて describe からやり直す（§10.3）。
//! - `ingestAllowed`: 呼び出し手の API キーが `ingest:#{connectionId}` を持つか。
//! - `db.enabled`: 参照先の `postgres` 接続が有効か。無効ならブリッジはその source を
//!   動かさない（Hub 内の Source が無効な接続を読まないのと同じ。運用者が DB への
//!   問い合わせを止める手段を残す）。
//!
//! 応答の DTO は `apps/banto-hub-sink/src/hub_api.rs` の `BridgeConfig` 一式と
//! 1 対 1（camelCase）。

use sha2::{Digest, Sha256};

use super::{
    get, ApiError, ApiKeysService, Arc, AuthState, CollectorManager, HeaderMap, IntoResponse, Json,
    PlcConnectionService, Response, Router, Serialize, State,
};
use crate::api_keys::ApiKeyContext;
use banto_tags::{
    validate_push_settings, PlcConnection, BRIDGE_KIND_DB, POSTGRES_PROTOCOL, PUSH_PROTOCOL,
};
use serde_json::Value;

/// `GET /api/ingest/bridge-config` のパス。
pub(crate) const BRIDGE_CONFIG_PATH: &str = "/api/ingest/bridge-config";

#[derive(Clone)]
struct BridgeConfigState {
    plc_connections: PlcConnectionService,
    manager: Arc<CollectorManager>,
    api_keys: ApiKeysService,
    auth: AuthState,
}

/// `GET /api/ingest/bridge-config` の応答（§10.2）。
///
/// R2b-1d（docs/banto-hub-connection-settings-design.md §5.4、追加だけ）: 読み先の
/// `postgres` 接続のパスワードを読めない source は `sources` に**載せず**、
/// `unavailable_sources` に理由つきで出す（`password: null` では返さない。古いブリッジは
/// 載っていない source を引かない）。`ingestAllowed: false` の source は今どおり開封も
/// しないので、ここには出ない。
#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BridgeConfigResponse {
    pub generated_at: i64,
    pub sources: Vec<BridgeSourceEntry>,
    pub unavailable_sources: Vec<BridgeUnavailableSource>,
}

/// R2b-1d（設計 §5.4）: 読み先のパスワードを読めないので載せなかった source 1 件。
/// `connection_id` は `push` の接続、`db_connection_id` は読めない `postgres` の接続。
/// `reason` は `missing` / `undecryptable`。値も参照も持たない。
#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BridgeUnavailableSource {
    pub connection_id: i64,
    pub name: String,
    pub db_connection_id: i64,
    pub reason: &'static str,
}

/// [`build_sources`] の結果。
#[derive(Debug, Default, PartialEq)]
pub(crate) struct BuiltSources {
    pub sources: Vec<BridgeSourceEntry>,
    pub unavailable: Vec<BridgeUnavailableSource>,
    /// 載せなかった接続の warn の文言（パスワードを含まない）。
    pub warnings: Vec<String>,
}

/// ブリッジの source 1 件（= ブリッジの `push` 接続 1 つ）。
#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BridgeSourceEntry {
    /// 送信の URL（`/api/v1/ingest/{id}/stream`）とスコープ（`ingest:#{id}`）の id。
    pub connection_id: i64,
    /// 表示・ログ用（束縛は id）。
    pub name: String,
    pub stale_timeout_ms: u64,
    /// `settings.bridge` の指紋（このモジュールの doc「何を返すか」）。
    pub config_fingerprint: String,
    /// 呼び出し手のキーがこの接続の `ingest:#{id}` を持つか。
    pub ingest_allowed: bool,
    pub queries: Vec<BridgeQueryEntry>,
    pub db: BridgeDbEntry,
}

#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BridgeQueryEntry {
    pub name: String,
    pub period_ms: i64,
    pub sql: String,
    pub columns: Vec<BridgeColumnEntry>,
}

#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BridgeColumnEntry {
    pub column: String,
    pub key: String,
}

/// 読む DB（参照先の `postgres` 接続）。**`password` は平文**（このモジュールの doc
/// 「パスワードを返す」）。`ingestAllowed: false` の source では `null`。
#[derive(Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BridgeDbEntry {
    pub id: i64,
    pub name: String,
    pub enabled: bool,
    pub host: String,
    pub port: i64,
    pub database: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
}

/// `Debug` にパスワードを出さない（テストの失敗の表示・将来のログの書き間違いの
/// 多層防御）。
impl std::fmt::Debug for BridgeDbEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BridgeDbEntry")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("enabled", &self.enabled)
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

/// `settings.bridge` の指紋（`"sha256:{hex}"`）。`serde_json::to_vec` の並びは保存した
/// JSON と同じなので、同じ設定なら同じ値になる。
pub(crate) fn bridge_fingerprint(bridge: &Value) -> String {
    let bytes = serde_json::to_vec(bridge).unwrap_or_default();
    format!("sha256:{}", hex::encode(Sha256::digest(&bytes)))
}

/// 接続の一覧から応答の `sources`・`unavailableSources` を作る（純関数。DB を読まない -
/// 表のテストに掛ける）。`caller` は通ったキー（admin ロールのセッションなら `None` =
/// どの接続にも送れない）。
///
/// R2b-1d: `secret_rows` are the sealed secret rows (`plc_connection_secrets`); the
/// referenced `postgres` connection's password is opened from them only for a source
/// whose caller may ingest (design §5.4). A password that cannot be opened is never sent
/// as `null`/empty: the source is left out of `sources` and listed in `unavailable`
/// with the reason (and a warning line).
pub(crate) fn build_sources(
    connections: &[PlcConnection],
    secret_rows: &[banto_tags::StoredSecretRow],
    caller: Option<&ApiKeyContext>,
) -> BuiltSources {
    let mut sources = Vec::new();
    let mut unavailable = Vec::new();
    let mut warnings = Vec::new();
    for conn in connections {
        if conn.protocol != PUSH_PROTOCOL || !conn.enabled {
            continue;
        }
        let raw = &Value::Object(conn.settings.clone());
        let Some(raw_bridge) = raw.get("bridge") else {
            continue;
        };
        if raw_bridge.get("kind").and_then(Value::as_str) != Some(BRIDGE_KIND_DB) {
            continue;
        }
        let settings = match validate_push_settings(raw) {
            Ok(settings) => settings,
            Err(_) => {
                warnings.push(format!(
                    "接続 {}（#{}）の settings が検証を通らないため、ブリッジの設定に載せません",
                    conn.name, conn.id
                ));
                continue;
            }
        };
        let Some(bridge) = settings.bridge else {
            continue;
        };
        let Some(db) = connections.iter().find(|c| c.id == bridge.db_connection_id) else {
            warnings.push(format!(
                "接続 {}（#{}）のブリッジの読み取り先 #{} がありません",
                conn.name, conn.id, bridge.db_connection_id
            ));
            continue;
        };
        if db.protocol != POSTGRES_PROTOCOL {
            warnings.push(format!(
                "接続 {}（#{}）のブリッジの読み取り先 #{} は postgres 接続ではありません",
                conn.name, conn.id, bridge.db_connection_id
            ));
            continue;
        }
        let ingest_allowed = caller.is_some_and(|ctx| ctx.can_ingest(conn.id));
        let password = if ingest_allowed {
            match crate::secrets::open_connection_secret(
                &crate::secrets::ConnectionSecrets::platform_default(),
                db,
                banto_tags::PASSWORD_KEY,
                secret_rows,
            ) {
                Ok(password) => password.map(|plain| plain.as_str().to_owned()),
                Err(reason) => {
                    unavailable.push(BridgeUnavailableSource {
                        connection_id: conn.id,
                        name: conn.name.clone(),
                        db_connection_id: db.id,
                        reason: reason.as_str(),
                    });
                    warnings.push(format!(
                        "接続 {}（#{}）のブリッジの読み取り先 {}（#{}）のパスワードを読めません（{}）",
                        conn.name,
                        conn.id,
                        db.name,
                        db.id,
                        reason.as_str()
                    ));
                    continue;
                }
            }
        } else {
            None
        };
        sources.push(BridgeSourceEntry {
            connection_id: conn.id,
            name: conn.name.clone(),
            stale_timeout_ms: settings.stale_timeout_ms,
            config_fingerprint: bridge_fingerprint(raw_bridge),
            ingest_allowed,
            queries: bridge
                .queries
                .into_iter()
                .map(|query| BridgeQueryEntry {
                    name: query.name,
                    period_ms: query.period_ms,
                    sql: query.sql,
                    columns: query
                        .columns
                        .into_iter()
                        .map(|column| BridgeColumnEntry {
                            column: column.column,
                            key: column.key,
                        })
                        .collect(),
                })
                .collect(),
            db: BridgeDbEntry {
                id: db.id,
                name: db.name.clone(),
                enabled: db.enabled,
                host: db.setting_str("host").unwrap_or("").to_string(),
                port: db.setting_i64("port").unwrap_or_default(),
                database: db.setting_str("database").map(str::to_string),
                username: db.setting_str("username").map(str::to_string),
                password,
            },
        });
    }
    sources.sort_by_key(|source| source.connection_id);
    unavailable.sort_by_key(|source| source.connection_id);
    BuiltSources {
        sources,
        unavailable,
        warnings,
    }
}

/// パスワードの平文の返却はループバックの前提（このモジュールの doc）を、プロセスの
/// 起動後の初回の呼び出しで 1 回だけ warn する（`GET /api/sink/config` と同じ形）。
static BRIDGE_CONFIG_LOOPBACK_WARNING: std::sync::Once = std::sync::Once::new();

async fn build_response(state: &BridgeConfigState, caller: Option<&ApiKeyContext>) -> Response {
    // PR #777 review P2: the connections and the secret rows come from one read
    // transaction (a password update committing between two separate reads would make
    // the source's DB connection read as `missing`).
    let (connections, secret_rows) = match state.plc_connections.list_with_secret_rows().await {
        Ok(pair) => pair,
        Err(err) => return ApiError(err).into_response(),
    };
    let built = build_sources(&connections, &secret_rows, caller);
    for warning in built.warnings {
        eprintln!("banto-hub: warn: GET {BRIDGE_CONFIG_PATH}: {warning}");
    }
    Json(BridgeConfigResponse {
        generated_at: state.manager.clock().now_ms(),
        sources: built.sources,
        unavailable_sources: built.unavailable,
    })
    .into_response()
}

async fn bridge_config_get(State(state): State<BridgeConfigState>, headers: HeaderMap) -> Response {
    let now_ms = state.manager.clock().now_ms();
    let caller =
        match super::sink::require_sink_admin(&state.api_keys, &state.auth, &headers, now_ms).await
        {
            Ok(caller) => caller,
            Err(resp) => return resp,
        };
    BRIDGE_CONFIG_LOOPBACK_WARNING.call_once(|| {
        eprintln!(
            "banto-hub: warn: GET {BRIDGE_CONFIG_PATH} はブリッジが読む DB 接続のパスワードを平文で返します。\
             ブリッジ（banto-hub-sink）は Hub と同一マシンからのループバック接続のみを前提とします\
             （docs/banto-hub-ingest-design.md §10.2、docs/banto-hub-external-db-design.md §2.2・§6-15）。"
        );
    });
    build_response(&state, caller.as_ref()).await
}

/// `GET /api/ingest/bridge-config` のルーター（`crate::rest` が CSRF のレイヤーの外側に
/// `.merge()` する。`sink_admin_router` と同じ置き方）。
pub(super) fn ingest_bridge_router(
    plc_connections: PlcConnectionService,
    manager: Arc<CollectorManager>,
    api_keys: ApiKeysService,
    auth: AuthState,
) -> Router {
    let state = BridgeConfigState {
        plc_connections,
        manager,
        api_keys,
        auth,
    };
    Router::new()
        .route(BRIDGE_CONFIG_PATH, get(bridge_config_get))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn conn(id: i64, protocol: &str, enabled: bool, settings: Option<Value>) -> PlcConnection {
        PlcConnection {
            id,
            name: format!("c{id}"),
            protocol: protocol.to_string(),
            enabled,
            simulation: false,
            // R2b-1d: a postgres row's password is a reference to its sealed row
            // ([`secret_rows`]); a push row's settings are the object itself.
            settings: if protocol == POSTGRES_PROTOCOL {
                json!({ "host": "db01", "port": 5432, "database": "mes", "username": "reader", "password": { "$secret": pw_ref(id) } })
                    .as_object()
                    .unwrap()
                    .clone()
            } else {
                settings
                    .map(|value| value.as_object().unwrap().clone())
                    .unwrap_or_default()
            },
            settings_version: 1,
        }
    }

    fn pw_ref(id: i64) -> String {
        format!("{id:032x}")
    }

    /// The sealed (`plain-v1`) password rows of every postgres connection in
    /// `connections` (what `PlcConnectionService::all_secret_rows` returns).
    fn secret_rows(connections: &[PlcConnection]) -> Vec<banto_tags::StoredSecretRow> {
        connections
            .iter()
            .filter(|c| c.protocol == POSTGRES_PROTOCOL)
            .map(|c| banto_tags::StoredSecretRow {
                connection_id: c.id,
                key: "password".to_string(),
                sealed: banto_tags::SealedSecretRow::new(
                    pw_ref(c.id),
                    "plain-v1",
                    b"s3cret-pw".to_vec(),
                ),
            })
            .collect()
    }

    fn bridge(db_id: i64) -> Value {
        json!({ "stale_timeout_ms": 5000, "bridge": { "kind": "db", "db_connection_id": db_id,
            "queries": [ { "name": "line1", "period_ms": 1000, "sql": "SELECT temp FROM t",
                "columns": [ { "column": "temp", "key": "line1.temp" } ] } ] } })
    }

    fn key(scopes: &[&str]) -> ApiKeyContext {
        ApiKeyContext {
            id: 1,
            name: "bridge".to_string(),
            prefix: "kkkkkkkk".to_string(),
            scopes: scopes.iter().map(|s| s.to_string()).collect(),
            last_used_at_ms: None,
        }
    }

    /// 有効な `push` + `bridge.kind == "db"` だけ。一般の送り手・無効・他のプロトコルは
    /// 載らない。
    #[test]
    fn only_enabled_push_connections_with_a_db_bridge_are_listed() {
        let connections = vec![
            conn(1, POSTGRES_PROTOCOL, true, None),
            conn(2, PUSH_PROTOCOL, true, Some(bridge(1))),
            conn(3, PUSH_PROTOCOL, false, Some(bridge(1))),
            conn(
                4,
                PUSH_PROTOCOL,
                true,
                Some(json!({ "stale_timeout_ms": 2000 })),
            ),
            conn(5, PUSH_PROTOCOL, true, None),
            conn(6, "modbus-tcp", true, None),
        ];
        let ctx = key(&["admin", "ingest:#2"]);
        let BuiltSources {
            sources, warnings, ..
        } = build_sources(&connections, &secret_rows(&connections), Some(&ctx));
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(sources.len(), 1);
        let source = &sources[0];
        assert_eq!(source.connection_id, 2);
        assert_eq!(source.stale_timeout_ms, 5000);
        assert!(source.ingest_allowed);
        assert_eq!(source.queries[0].period_ms, 1000);
        assert_eq!(source.queries[0].columns[0].key, "line1.temp");
        assert_eq!(source.db.id, 1);
        assert_eq!(source.db.password.as_deref(), Some("s3cret-pw"));
        assert!(source.config_fingerprint.starts_with("sha256:"));
        assert_eq!(
            source.config_fingerprint,
            bridge_fingerprint(&bridge(1)["bridge"])
        );
    }

    /// スコープの無いキー・セッション（`None`）は `ingestAllowed: false` で、パスワードを
    /// 渡さない（§11 の監査「スコープの無い接続を動かさない」・パスワードの最小化）。
    #[test]
    fn a_caller_without_the_ingest_scope_gets_no_password() {
        let connections = vec![
            conn(1, POSTGRES_PROTOCOL, true, None),
            conn(2, PUSH_PROTOCOL, true, Some(bridge(1))),
        ];
        for caller in [Some(key(&["admin", "read", "ingest:#3"])), None] {
            let sources =
                build_sources(&connections, &secret_rows(&connections), caller.as_ref()).sources;
            assert_eq!(sources.len(), 1);
            assert!(!sources[0].ingest_allowed);
            assert_eq!(sources[0].db.password, None);
            assert_eq!(sources[0].db.username.as_deref(), Some("reader"));
        }
    }

    /// 指紋は `settings.bridge` だけで決まる（`stale_timeout_ms` の変更では変わらない。
    /// `IngestHub` の epoch を進める単位と同じ、§4.6）。
    #[test]
    fn the_fingerprint_follows_the_bridge_section_only() {
        let a = bridge(1);
        let mut b = bridge(1);
        b["stale_timeout_ms"] = json!(9000);
        let mut c = bridge(1);
        c["bridge"]["queries"][0]["period_ms"] = json!(2000);
        assert_eq!(
            bridge_fingerprint(&a["bridge"]),
            bridge_fingerprint(&b["bridge"])
        );
        assert_ne!(
            bridge_fingerprint(&a["bridge"]),
            bridge_fingerprint(&c["bridge"])
        );
    }

    /// 参照先が無い・`postgres` でない・settings が読めないものは載せず、warn の文言には
    /// パスワードが出ない。無効な `postgres` 接続は `db.enabled: false` で載る。
    #[test]
    fn broken_references_are_skipped_without_leaking_the_password() {
        let mut broken = bridge(1);
        broken["bridge"]["unknown"] = json!(true);
        let connections = vec![
            conn(1, POSTGRES_PROTOCOL, false, None),
            conn(2, PUSH_PROTOCOL, true, Some(bridge(1))),
            conn(3, PUSH_PROTOCOL, true, Some(bridge(99))),
            conn(4, PUSH_PROTOCOL, true, Some(bridge(5))),
            conn(5, "modbus-tcp", true, None),
            conn(6, PUSH_PROTOCOL, true, Some(broken)),
        ];
        let ctx = key(&["admin", "ingest:#2", "ingest:#3", "ingest:#4", "ingest:#6"]);
        let BuiltSources {
            sources,
            unavailable,
            warnings,
        } = build_sources(&connections, &secret_rows(&connections), Some(&ctx));
        assert_eq!(
            sources.iter().map(|s| s.connection_id).collect::<Vec<_>>(),
            vec![2]
        );
        // Broken references are not "unreadable" - nothing is listed there.
        assert!(unavailable.is_empty(), "{unavailable:?}");
        assert!(!sources[0].db.enabled);
        assert_eq!(warnings.len(), 3, "{warnings:?}");
        for warning in &warnings {
            assert!(!warning.contains("s3cret-pw"), "{warning}");
        }
        assert!(!format!("{:?}", sources[0].db).contains("s3cret-pw"));
    }

    /// R2b-1d (design §5.4): a source whose `postgres` password cannot be read (its row
    /// is missing, or its scheme cannot be opened here) is left out of `sources` - never
    /// sent with a `null`/empty password - and listed in `unavailableSources` with the
    /// reason, while the others still go; without the ingest scope nothing is opened at
    /// all.
    #[test]
    fn an_unreadable_password_leaves_only_that_source_out() {
        let connections = vec![
            conn(1, POSTGRES_PROTOCOL, true, None),
            conn(2, PUSH_PROTOCOL, true, Some(bridge(1))),
            conn(3, POSTGRES_PROTOCOL, true, None),
            conn(4, PUSH_PROTOCOL, true, Some(bridge(3))),
        ];
        let rows: Vec<_> = secret_rows(&connections)
            .into_iter()
            .filter(|row| row.connection_id != 3)
            .collect();
        let ctx = key(&["admin", "ingest:#2", "ingest:#4"]);
        let BuiltSources {
            sources,
            unavailable,
            warnings,
        } = build_sources(&connections, &rows, Some(&ctx));
        assert_eq!(
            sources.iter().map(|s| s.connection_id).collect::<Vec<_>>(),
            vec![2]
        );
        assert_eq!(sources[0].db.password.as_deref(), Some("s3cret-pw"));
        assert_eq!(
            unavailable,
            vec![BridgeUnavailableSource {
                connection_id: 4,
                name: "c4".to_string(),
                db_connection_id: 3,
                reason: "missing",
            }]
        );
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].contains("パスワードを読めません"),
            "{warnings:?}"
        );
        // A row whose scheme cannot be opened here: `undecryptable`.
        let mut undecryptable = secret_rows(&connections);
        for row in &mut undecryptable {
            if row.connection_id == 3 {
                row.sealed = banto_tags::SealedSecretRow::new(
                    row.sealed.r#ref.clone(),
                    "dpapi-machine-v1",
                    b"blob".to_vec(),
                );
            }
        }
        let built = build_sources(&connections, &undecryptable, Some(&ctx));
        assert_eq!(built.sources.len(), 1);
        assert_eq!(built.unavailable[0].reason, "undecryptable");
        // Without the scope nothing is opened, so the unreadable one is listed (no password).
        let BuiltSources {
            sources,
            unavailable,
            warnings,
        } = build_sources(&connections, &rows, Some(&key(&["admin"])));
        assert_eq!(sources.len(), 2);
        assert!(unavailable.is_empty());
        assert!(warnings.is_empty());
        assert!(sources.iter().all(|s| s.db.password.is_none()));
    }

    /// 応答の wire 形（camelCase、§10.2）。
    #[test]
    fn response_is_camel_case() {
        let connections = vec![
            conn(1, POSTGRES_PROTOCOL, true, None),
            conn(2, PUSH_PROTOCOL, true, Some(bridge(1))),
        ];
        let sources = build_sources(
            &connections,
            &secret_rows(&connections),
            Some(&key(&["admin", "ingest:#2"])),
        )
        .sources;
        let body = serde_json::to_value(BridgeConfigResponse {
            generated_at: 5,
            sources,
            unavailable_sources: vec![BridgeUnavailableSource {
                connection_id: 7,
                name: "p".to_string(),
                db_connection_id: 8,
                reason: "undecryptable",
            }],
        })
        .unwrap();
        assert_eq!(
            body["unavailableSources"],
            json!([{"connectionId": 7, "name": "p", "dbConnectionId": 8, "reason": "undecryptable"}])
        );
        let source = &body["sources"][0];
        assert_eq!(body["generatedAt"], json!(5));
        assert_eq!(source["connectionId"], json!(2));
        assert_eq!(source["staleTimeoutMs"], json!(5000));
        assert_eq!(source["ingestAllowed"], json!(true));
        assert_eq!(source["queries"][0]["periodMs"], json!(1000));
        assert_eq!(source["db"]["password"], json!("s3cret-pw"));
        assert!(source["configFingerprint"].is_string());
    }
}
