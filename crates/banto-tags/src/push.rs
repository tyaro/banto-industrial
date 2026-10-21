//! `push` 接続の純粋な規則（R2a-2a、docs/banto-hub-ingest-design.md §3.2・§3.3・
//! §7・§7.1）: 受信の上限の定数 [`PUSH_LIMITS`]、接続の `settings`（JSON）の検証
//! [`validate_push_settings`]、タグのキーの検証 [`validate_push_key`]、ブリッジの
//! 送る量の見積もり [`bridge_load`]。
//!
//! どれも DB にも時計にも触らない純関数で、`PlcConnectionService`（設定の検証）、
//! `TagService`（キーの検証、`crate::tag` の `push_tag_errors`）、banto-hub の
//! `PushDriver`（`DriverBase::validate_config` / `parse_address`）、そして後段の
//! 受信（R2a-2c の上限）とブリッジ（R2a-3b の分割）が同じものを使う。上限の数値を
//! 1 か所に置くのは、登録時の見積もり・受信の拒否・ブリッジの分割がずれると
//! 「保存できたのに送れない」設定ができるため（§7.1、§11 のオーナーの監査 (8)）。
//!
//! ## `settings` の形（§3.2）
//!
//! ```jsonc
//! { "stale_timeout_ms": 10000,            // 任意。既定 10000、1000..=3600000
//!   "bridge": { "kind": "db", "db_connection_id": 12,
//!               "queries": [ { "name": "line1", "period_ms": 1000, "sql": "SELECT ...",
//!                              "columns": [ { "column": "temp", "key": "line1.temp" } ] } ] } }
//! ```
//!
//! 未知のキーはどの階層でも断る（Hub が書く唯一の正なので、綴りの間違いを黙って
//! 通さない）。`bridge.kind` は `"db"` だけ（fail closed）。大きさは保存する形
//! （`serde_json::to_string`）で [`PushLimits::max_settings_bytes`] まで。キーは区画
//! 全体で一意。加えて [`bridge_load`] が上限の 8 割（§12 Q20）を超える設定を断る。
//! `db_connection_id` が `postgres` 接続を指すかは DB を見る必要があるので、ここ
//! ではなく `PlcConnectionService` が確かめる。`columns[].key` が実在するタグかは
//! 確かめない（タグは後から作る。ブリッジは未知のキーの拒否で気づく、§3.2）。

use std::collections::HashSet;

use banto_core::FieldError;
use serde_json::{Map, Value};

use crate::collection_group::{query_sql_error, ALLOWED_PERIOD_MS};
use crate::support::{max_length_message, range_message, required_message};
use crate::tag::{is_db_column_identifier, MAX_DB_COLUMN_NAME_LEN};

/// 取り込み（`push`）の上限（§7、2026-10-21 オーナー決定 Q4）。値は Hub の起動中の
/// 固定値で、[`PUSH_LIMITS`] の 1 つだけを使う。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PushLimits {
    /// 1 メッセージ（WS の `values`・REST の一括）の件数。超えたら全体を断る。
    pub max_items_per_message: u64,
    /// WebSocket の 1 メッセージ・1 フレームのバイト数（256 KiB）。超えたら close 1009。
    pub max_ws_message_bytes: u64,
    /// REST の本文のバイト数（1 MiB）。超えたら 413。
    pub max_rest_body_bytes: u64,
    /// 接続ごとのメッセージの頻度（持続、WS と REST の合計）。
    pub messages_per_sec: u64,
    /// 同じく瞬間（トークンバケットの容量）。
    pub messages_burst: u64,
    /// 接続ごとの件数の頻度（持続）。
    pub items_per_sec: u64,
    /// 同じく瞬間。
    pub items_burst: u64,
    /// `status` の `detail` のバイト数。超えた分は切り詰める。
    pub max_status_detail_bytes: u64,
    /// 接続の `settings`（保存する JSON の文字列）のバイト数（64 KiB）。
    pub max_settings_bytes: u64,
    /// 登録時の見積もりが使ってよい割合（%）。残りは `ping`・`status`・瞬間の
    /// 重なりの余裕（§7.1、Q20 = 80 %）。
    pub registration_headroom_percent: u64,
    /// [`bridge_load`] の 1 件の JSON の最悪の大きさのうち、キー以外の分（値・品質・
    /// 時刻・区切り）。
    pub bridge_item_overhead_bytes: u64,
    /// [`bridge_load`] の 1 メッセージの外枠（`op`・`seq`・`epoch`・括弧）の見積もり。
    pub bridge_message_overhead_bytes: u64,
}

/// 上限の唯一の置き場（§7 の表、§7.1 の式の定数）。
pub const PUSH_LIMITS: PushLimits = PushLimits {
    max_items_per_message: 1_000,
    max_ws_message_bytes: 256 * 1024,
    max_rest_body_bytes: 1024 * 1024,
    messages_per_sec: 20,
    messages_burst: 40,
    items_per_sec: 5_000,
    items_burst: 10_000,
    max_status_detail_bytes: 1024,
    max_settings_bytes: 64 * 1024,
    registration_headroom_percent: 80,
    bridge_item_overhead_bytes: 96,
    bridge_message_overhead_bytes: 512,
};

impl PushLimits {
    /// 登録時に許すメッセージの頻度（持続の 8 割。既定で 16 / 秒）。
    pub const fn registration_messages_per_sec(&self) -> u64 {
        self.messages_per_sec * self.registration_headroom_percent / 100
    }

    /// 登録時に許す件数の頻度（持続の 8 割。既定で 4,000 / 秒）。
    pub const fn registration_items_per_sec(&self) -> u64 {
        self.items_per_sec * self.registration_headroom_percent / 100
    }
}

/// `stale_timeout_ms` の既定（§4.3、Q3 = 10 秒）。
pub const DEFAULT_STALE_TIMEOUT_MS: u64 = 10_000;
/// `stale_timeout_ms` の下限（1 秒）。
pub const MIN_STALE_TIMEOUT_MS: u64 = 1_000;
/// `stale_timeout_ms` の上限（1 時間）。
pub const MAX_STALE_TIMEOUT_MS: u64 = 3_600_000;
/// `push` のキー（タグの `address`）の文字数の上限（§3.3）。
pub const MAX_PUSH_KEY_LEN: usize = 256;
/// ブリッジのクエリの `name` の文字数の上限（§3.2）。
pub const MAX_BRIDGE_QUERY_NAME_LEN: usize = 64;
/// R2a の Hub が知るブリッジの種類（§3.2。ほかは断る）。
pub const BRIDGE_KIND_DB: &str = "db";

/// 検証を通った `settings`。`stale_timeout_ms` は省略なら既定で埋めてある。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushSettings {
    /// 送り手が黙ってから Down とみなすまで（ms、§4.3）。
    pub stale_timeout_ms: u64,
    /// ブリッジの区画（ブリッジの接続だけ）。
    pub bridge: Option<PushBridge>,
}

impl Default for PushSettings {
    fn default() -> Self {
        PushSettings {
            stale_timeout_ms: DEFAULT_STALE_TIMEOUT_MS,
            bridge: None,
        }
    }
}

/// `settings.bridge`（§3.2・§10.2）。`kind` は [`BRIDGE_KIND_DB`] だけなので持たない。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushBridge {
    /// 読む DB の `postgres` 接続の id（§10.2、Q8）。
    pub db_connection_id: i64,
    /// クエリ（周期ごとに 1 回読む）。
    pub queries: Vec<BridgeQuery>,
}

/// ブリッジの 1 クエリ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeQuery {
    /// 区画の中で一意な名前。
    pub name: String,
    /// 読む周期（`collection_groups.period_ms` と同じ集合）。
    pub period_ms: i64,
    /// `SELECT` / `WITH` の単文。
    pub sql: String,
    /// 結果の列とキーの対応。
    pub columns: Vec<BridgeColumn>,
}

/// 結果の 1 列を `push` のキー 1 つへ送る対応。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeColumn {
    /// 結果の列名（`db` タグの `address` と同じ規則）。
    pub column: String,
    /// 送り先のキー（[`validate_push_key`] の正規形）。
    pub key: String,
}

/// `push` のタグのキー（`address`）を検証し、正規形（前後の空白を除いた文字列）を
/// 返す（§3.3）。1..=[`MAX_PUSH_KEY_LEN`] 文字、制御文字なし。大文字小文字は区別し、
/// それ以外の書き換えはしない（`parse(canonical) == canonical`）。`Err` は項目の文言。
pub fn validate_push_key(text: &str) -> Result<String, String> {
    let key = text.trim();
    if key.is_empty() {
        return Err(required_message());
    }
    if key.chars().count() > MAX_PUSH_KEY_LEN {
        return Err(max_length_message(MAX_PUSH_KEY_LEN));
    }
    if key.chars().any(char::is_control) {
        return Err("push のキーに制御文字は使えません".to_string());
    }
    Ok(key.to_string())
}

/// `settings` の項目エラーの `field` の接頭辞（`PlcConnectionInput::settings` の wire 名）。
const SETTINGS_FIELD: &str = "settings";

/// `push` 接続の `settings` を検証する（§3.2。純関数）。通れば既定を埋めた
/// [`PushSettings`]、通らなければ全部の項目エラー（`field` は `settings` /
/// `settings.stale_timeout_ms` / `settings.bridge.queries[0].columns[1].key` の形）。
///
/// 大きさの上限を超えたら、中を読まずにそれだけを返す（64 KiB を超える入力を
/// 先まで歩かない）。
pub fn validate_push_settings(settings: &Value) -> Result<PushSettings, Vec<FieldError>> {
    let size = serde_json::to_string(settings).map_or(u64::MAX, |s| s.len() as u64);
    if size > PUSH_LIMITS.max_settings_bytes {
        return Err(vec![field_error(
            SETTINGS_FIELD,
            format!(
                "settings は {} バイトまでです（今は {size} バイト）",
                PUSH_LIMITS.max_settings_bytes
            ),
        )]);
    }
    let mut errors = Vec::new();
    let Some(object) = settings.as_object() else {
        return Err(vec![field_error(
            SETTINGS_FIELD,
            "settings は JSON のオブジェクトで指定してください".to_string(),
        )]);
    };
    reject_unknown_keys(
        object,
        &["stale_timeout_ms", "bridge"],
        SETTINGS_FIELD,
        &mut errors,
    );

    let stale_field = format!("{SETTINGS_FIELD}.stale_timeout_ms");
    let stale_timeout_ms = match object.get("stale_timeout_ms") {
        None => DEFAULT_STALE_TIMEOUT_MS,
        Some(value) => match value.as_u64() {
            Some(ms) if (MIN_STALE_TIMEOUT_MS..=MAX_STALE_TIMEOUT_MS).contains(&ms) => ms,
            _ => {
                errors.push(field_error(
                    &stale_field,
                    range_message(MIN_STALE_TIMEOUT_MS as i64, MAX_STALE_TIMEOUT_MS as i64),
                ));
                DEFAULT_STALE_TIMEOUT_MS
            }
        },
    };

    let bridge = match object.get("bridge") {
        None => None,
        Some(value) => parse_bridge(value, &format!("{SETTINGS_FIELD}.bridge"), &mut errors),
    };

    if errors.is_empty() {
        Ok(PushSettings {
            stale_timeout_ms,
            bridge,
        })
    } else {
        Err(errors)
    }
}

/// `settings.bridge` を読む。どこかが壊れていれば `None`（項目エラーは `errors`）。
/// 形がすべて正しいときだけ [`bridge_load`] の見積もりを掛ける。
fn parse_bridge(value: &Value, field: &str, errors: &mut Vec<FieldError>) -> Option<PushBridge> {
    let Some(object) = value.as_object() else {
        errors.push(field_error(
            field,
            "bridge は JSON のオブジェクトで指定してください".to_string(),
        ));
        return None;
    };
    let before = errors.len();
    reject_unknown_keys(
        object,
        &["kind", "db_connection_id", "queries"],
        field,
        errors,
    );

    match object.get("kind").map(Value::as_str) {
        Some(Some(BRIDGE_KIND_DB)) => {}
        None => errors.push(field_error(&format!("{field}.kind"), required_message())),
        Some(_) => errors.push(field_error(
            &format!("{field}.kind"),
            format!("ブリッジの種類は {BRIDGE_KIND_DB} だけです"),
        )),
    }

    let db_connection_id = match object.get("db_connection_id").map(Value::as_i64) {
        Some(Some(id)) if id > 0 => id,
        None => {
            errors.push(field_error(
                &format!("{field}.db_connection_id"),
                required_message(),
            ));
            0
        }
        Some(_) => {
            errors.push(field_error(
                &format!("{field}.db_connection_id"),
                "db_connection_id は postgres 接続の id（正の整数）で指定してください".to_string(),
            ));
            0
        }
    };

    let queries_field = format!("{field}.queries");
    let mut queries = Vec::new();
    match object.get("queries") {
        None => errors.push(field_error(&queries_field, required_message())),
        Some(Value::Array(items)) => {
            let mut names: HashSet<&str> = HashSet::new();
            let mut keys: HashSet<String> = HashSet::new();
            for (i, item) in items.iter().enumerate() {
                let query_field = format!("{queries_field}[{i}]");
                if let Some(query) = parse_query(item, &query_field, &mut keys, errors) {
                    queries.push(query);
                }
                if let Some(name) = item.get("name").and_then(Value::as_str) {
                    if !names.insert(name) {
                        errors.push(field_error(
                            &format!("{query_field}.name"),
                            format!("クエリの名前 {name} が重複しています"),
                        ));
                    }
                }
            }
        }
        Some(_) => errors.push(field_error(
            &queries_field,
            "queries は配列で指定してください".to_string(),
        )),
    }

    if errors.len() != before {
        return None;
    }
    let bridge = PushBridge {
        db_connection_id,
        queries,
    };
    if let Some(message) = bridge_load(&bridge).over_registration_limit_message() {
        errors.push(field_error(field, message));
        return None;
    }
    Some(bridge)
}

/// `bridge.queries[i]` を読む。`keys` は区画全体のキーの集合（重複を断る）。
fn parse_query(
    value: &Value,
    field: &str,
    keys: &mut HashSet<String>,
    errors: &mut Vec<FieldError>,
) -> Option<BridgeQuery> {
    let Some(object) = value.as_object() else {
        errors.push(field_error(
            field,
            "クエリは JSON のオブジェクトで指定してください".to_string(),
        ));
        return None;
    };
    let before = errors.len();
    reject_unknown_keys(
        object,
        &["name", "period_ms", "sql", "columns"],
        field,
        errors,
    );

    let name_field = format!("{field}.name");
    let name = match object.get("name").map(Value::as_str) {
        Some(Some(name)) if name.trim().is_empty() => {
            errors.push(field_error(&name_field, required_message()));
            String::new()
        }
        Some(Some(name)) if name.chars().count() > MAX_BRIDGE_QUERY_NAME_LEN => {
            errors.push(field_error(
                &name_field,
                max_length_message(MAX_BRIDGE_QUERY_NAME_LEN),
            ));
            String::new()
        }
        Some(Some(name)) => name.to_string(),
        None => {
            errors.push(field_error(&name_field, required_message()));
            String::new()
        }
        Some(None) => {
            errors.push(field_error(
                &name_field,
                "name は文字列で指定してください".to_string(),
            ));
            String::new()
        }
    };

    let period_field = format!("{field}.period_ms");
    let period_ms = match object.get("period_ms").map(Value::as_i64) {
        Some(Some(ms)) if ALLOWED_PERIOD_MS.contains(&ms) => ms,
        None => {
            errors.push(field_error(&period_field, required_message()));
            0
        }
        Some(_) => {
            let choices = ALLOWED_PERIOD_MS
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            errors.push(field_error(
                &period_field,
                format!("period_ms は {choices} のいずれかです"),
            ));
            0
        }
    };

    let sql_field = format!("{field}.sql");
    let sql = match object.get("sql").map(Value::as_str) {
        Some(Some(sql)) => match query_sql_error(sql) {
            None => sql.trim().to_string(),
            Some(message) => {
                errors.push(field_error(&sql_field, message));
                String::new()
            }
        },
        None => {
            errors.push(field_error(&sql_field, required_message()));
            String::new()
        }
        Some(None) => {
            errors.push(field_error(
                &sql_field,
                "sql は文字列で指定してください".to_string(),
            ));
            String::new()
        }
    };

    let columns_field = format!("{field}.columns");
    let mut columns = Vec::new();
    match object.get("columns") {
        None => errors.push(field_error(&columns_field, required_message())),
        Some(Value::Array(items)) => {
            for (i, item) in items.iter().enumerate() {
                if let Some(column) =
                    parse_column(item, &format!("{columns_field}[{i}]"), keys, errors)
                {
                    columns.push(column);
                }
            }
        }
        Some(_) => errors.push(field_error(
            &columns_field,
            "columns は配列で指定してください".to_string(),
        )),
    }

    (errors.len() == before).then_some(BridgeQuery {
        name,
        period_ms,
        sql,
        columns,
    })
}

/// `queries[i].columns[j]` を読む。
fn parse_column(
    value: &Value,
    field: &str,
    keys: &mut HashSet<String>,
    errors: &mut Vec<FieldError>,
) -> Option<BridgeColumn> {
    let Some(object) = value.as_object() else {
        errors.push(field_error(
            field,
            "列の対応は JSON のオブジェクトで指定してください".to_string(),
        ));
        return None;
    };
    let before = errors.len();
    reject_unknown_keys(object, &["column", "key"], field, errors);

    let column_field = format!("{field}.column");
    let column = match object.get("column").map(Value::as_str) {
        Some(Some(column))
            if column.chars().count() <= MAX_DB_COLUMN_NAME_LEN
                && is_db_column_identifier(column) =>
        {
            column.to_string()
        }
        None => {
            errors.push(field_error(&column_field, required_message()));
            String::new()
        }
        Some(_) => {
            errors.push(field_error(
                &column_field,
                format!(
                    "column は結果列名（英字またはアンダースコアで始まる英数字/アンダースコア、\
                     {MAX_DB_COLUMN_NAME_LEN} 文字まで）で指定してください"
                ),
            ));
            String::new()
        }
    };

    let key_field = format!("{field}.key");
    let key = match object.get("key").map(Value::as_str) {
        Some(Some(text)) => match validate_push_key(text) {
            Ok(key) if key != text => {
                // タグの address は前後の空白を除いて保存される（§3.3）。ここに空白が
                // 残っていると、ブリッジはそのまま送り、受信は unknown_key で断る。
                errors.push(field_error(
                    &key_field,
                    "key の前後に空白があります（タグのキーは前後の空白を除いて保存されます）"
                        .to_string(),
                ));
                String::new()
            }
            Ok(key) => {
                if !keys.insert(key.clone()) {
                    errors.push(field_error(
                        &key_field,
                        format!("キー {key} が区画の中で重複しています"),
                    ));
                }
                key
            }
            Err(message) => {
                errors.push(field_error(&key_field, message));
                String::new()
            }
        },
        None => {
            errors.push(field_error(&key_field, required_message()));
            String::new()
        }
        Some(None) => {
            errors.push(field_error(
                &key_field,
                "key は文字列で指定してください".to_string(),
            ));
            String::new()
        }
    };

    (errors.len() == before).then_some(BridgeColumn { column, key })
}

fn reject_unknown_keys(
    object: &Map<String, Value>,
    known: &[&str],
    field: &str,
    errors: &mut Vec<FieldError>,
) {
    for key in object.keys() {
        if !known.contains(&key.as_str()) {
            errors.push(field_error(
                field,
                format!(
                    "未知の項目 {key} があります（使える項目は {}）",
                    known.join(", ")
                ),
            ));
        }
    }
}

fn field_error(field: &str, message: String) -> FieldError {
    FieldError {
        field: field.to_string(),
        message,
    }
}

/// [`bridge_load`] のクエリ 1 本分。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueryLoad {
    /// 1 周期に送る件数（`columns` の数）。
    pub items: u64,
    /// 1 周期の件の JSON の最悪の大きさの見積もり（Σ キーの UTF-8 バイト数 + 96）。
    pub item_bytes: u64,
    /// 1 周期のメッセージ数（件数とバイト数のどちらで分かれても足りる数）。
    pub messages_per_cycle: u64,
    /// 周期（ms）。
    pub period_ms: i64,
}

/// [`bridge_load`] の結果: 接続全体の送る量の見積もり。頻度は**1 分あたり**の
/// 整数で持つ（周期の集合 100〜60000 ms はどれも 60000 を割り切るので、境界の
/// 比較を浮動小数の丸めに頼らずに済む）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeLoad {
    /// クエリごとの内訳（`queries` と同じ並び）。
    pub queries: Vec<QueryLoad>,
    /// 接続の合計のメッセージ数 / 分。
    pub messages_per_minute: u64,
    /// 接続の合計の件数 / 分。
    pub items_per_minute: u64,
}

impl BridgeLoad {
    /// 接続の合計のメッセージ数 / 秒（表示用）。
    pub fn messages_per_sec(&self) -> f64 {
        self.messages_per_minute as f64 / 60.0
    }

    /// 接続の合計の件数 / 秒（表示用）。
    pub fn items_per_sec(&self) -> f64 {
        self.items_per_minute as f64 / 60.0
    }

    /// 登録時の上限（持続の 8 割）に収まるか。ちょうどは通す。
    pub fn within_registration_limit(&self) -> bool {
        self.messages_per_minute <= PUSH_LIMITS.registration_messages_per_sec() * 60
            && self.items_per_minute <= PUSH_LIMITS.registration_items_per_sec() * 60
    }

    /// 上限を超えるときの項目の文言（§7.1 の 1.）。収まるなら `None`。
    pub fn over_registration_limit_message(&self) -> Option<String> {
        if self.within_registration_limit() {
            return None;
        }
        Some(format!(
            "この設定は 1 秒あたり約 {} メッセージ・{} 件になり、上限（{} メッセージ・{} 件）を\
             超えます。周期を長くするか、クエリを別の push 接続に分けてください",
            format_rate(self.messages_per_minute),
            format_rate(self.items_per_minute),
            group_thousands(PUSH_LIMITS.registration_messages_per_sec()),
            group_thousands(PUSH_LIMITS.registration_items_per_sec()),
        ))
    }
}

/// ブリッジの設定から接続全体の送る量を見積もる（§7.1 の式。純関数）。
///
/// ```text
/// item_bytes(q) = Σ (key の UTF-8 バイト数 + 96)
/// msgs(q)       = max(ceil(件数(q) / 1000), ceil((item_bytes(q) + 512) / (256 KiB − 512)))
/// messages/s    = Σ msgs(q) × 1000 / period_ms(q)   ≤ 16（20 の 8 割）
/// items/s       = Σ 件数(q) × 1000 / period_ms(q)   ≤ 4,000（5,000 の 8 割）
/// ```
///
/// 定数は [`PUSH_LIMITS`]。ブリッジの送信の分割（R2a-3b）と UI の見積もり（R2a-2e）は
/// 同じ式を使う。`period_ms` は検証済み（[`ALLOWED_PERIOD_MS`]）であること。0 以下の
/// 周期は 1 周期を 1 分として数える（検証を通った設定には起きない）。
pub fn bridge_load(bridge: &PushBridge) -> BridgeLoad {
    let limits = PUSH_LIMITS;
    let bytes_per_message = limits.max_ws_message_bytes - limits.bridge_message_overhead_bytes;
    let mut queries = Vec::with_capacity(bridge.queries.len());
    let mut messages_per_minute = 0u64;
    let mut items_per_minute = 0u64;
    for query in &bridge.queries {
        let items = query.columns.len() as u64;
        let item_bytes: u64 = query
            .columns
            .iter()
            .map(|c| c.key.len() as u64 + limits.bridge_item_overhead_bytes)
            .sum();
        let by_items = items.div_ceil(limits.max_items_per_message);
        let by_bytes =
            (item_bytes + limits.bridge_message_overhead_bytes).div_ceil(bytes_per_message);
        let messages_per_cycle = by_items.max(by_bytes);
        let cycles_per_minute = if query.period_ms > 0 {
            60_000 / query.period_ms as u64
        } else {
            1
        };
        messages_per_minute += messages_per_cycle * cycles_per_minute;
        items_per_minute += items * cycles_per_minute;
        queries.push(QueryLoad {
            items,
            item_bytes,
            messages_per_cycle,
            period_ms: query.period_ms,
        });
    }
    BridgeLoad {
        queries,
        messages_per_minute,
        items_per_minute,
    }
}

/// 1 分あたりの数を 1 秒あたりにして、小数 1 桁まで（整数なら小数なし）で書く。
fn format_rate(per_minute: u64) -> String {
    let tenths = (per_minute * 10).div_ceil(60);
    if tenths.is_multiple_of(10) {
        group_thousands(tenths / 10)
    } else {
        format!("{}.{}", group_thousands(tenths / 10), tenths % 10)
    }
}

fn group_thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn column(key: &str) -> BridgeColumn {
        BridgeColumn {
            column: "v".to_string(),
            key: key.to_string(),
        }
    }

    fn query(period_ms: i64, keys: impl IntoIterator<Item = String>) -> BridgeQuery {
        BridgeQuery {
            name: "q".to_string(),
            period_ms,
            sql: "SELECT 1 AS v".to_string(),
            columns: keys.into_iter().map(|k| column(&k)).collect(),
        }
    }

    fn short_keys(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("k{i}")).collect()
    }

    fn bridge(queries: Vec<BridgeQuery>) -> PushBridge {
        PushBridge {
            db_connection_id: 1,
            queries,
        }
    }

    fn fields(errors: &[FieldError]) -> Vec<&str> {
        errors.iter().map(|e| e.field.as_str()).collect()
    }

    // ---- 定数 ----------------------------------------------------------

    #[test]
    fn push_limits_match_the_design_table() {
        assert_eq!(PUSH_LIMITS.max_items_per_message, 1_000);
        assert_eq!(PUSH_LIMITS.max_ws_message_bytes, 262_144);
        assert_eq!(PUSH_LIMITS.max_rest_body_bytes, 1_048_576);
        assert_eq!(PUSH_LIMITS.messages_per_sec, 20);
        assert_eq!(PUSH_LIMITS.messages_burst, 40);
        assert_eq!(PUSH_LIMITS.items_per_sec, 5_000);
        assert_eq!(PUSH_LIMITS.items_burst, 10_000);
        assert_eq!(PUSH_LIMITS.max_status_detail_bytes, 1_024);
        assert_eq!(PUSH_LIMITS.max_settings_bytes, 65_536);
        assert_eq!(PUSH_LIMITS.registration_messages_per_sec(), 16);
        assert_eq!(PUSH_LIMITS.registration_items_per_sec(), 4_000);
    }

    // ---- bridge_load の境界値（§11 の R2a-2a の完了条件） ---------------

    #[test]
    fn one_thousand_items_fit_one_message_and_one_more_needs_two() {
        let load = bridge_load(&bridge(vec![query(1000, short_keys(1000))]));
        assert_eq!(load.queries[0].items, 1000);
        assert_eq!(load.queries[0].messages_per_cycle, 1);
        let load = bridge_load(&bridge(vec![query(1000, short_keys(1001))]));
        assert_eq!(load.queries[0].messages_per_cycle, 2);
    }

    #[test]
    fn long_keys_split_by_bytes_before_the_item_count_does() {
        // 200 バイトのキー × 1,000 件 = (200 + 96) × 1000 + 512 = 296,512 バイト
        // > 256 KiB − 512 = 261,632 → 件数では 1 だが、バイト数で 2 に分かれる。
        let keys: Vec<String> = (0..1000).map(|i| format!("{i:0>200}")).collect();
        assert!(keys.iter().all(|k| k.len() == 200));
        let load = bridge_load(&bridge(vec![query(1000, keys)]));
        assert_eq!(load.queries[0].item_bytes, 296_000);
        assert_eq!(load.queries[0].messages_per_cycle, 2);

        // 境界ちょうど: (k + 96) × n + 512 == 261,632 なら 1 メッセージ、1 バイト超で 2。
        // n = 1 で k = 261,024（バイト数だけを動かすための作り物。キーの規則は見ない）。
        let exact = bridge_load(&bridge(vec![query(1000, ["x".repeat(261_024)])]));
        assert_eq!(exact.queries[0].messages_per_cycle, 1);
        let over = bridge_load(&bridge(vec![query(1000, ["x".repeat(261_025)])]));
        assert_eq!(over.queries[0].messages_per_cycle, 2);
    }

    #[test]
    fn multibyte_keys_count_utf8_bytes_not_chars() {
        let load = bridge_load(&bridge(vec![query(1000, ["温度".to_string()])]));
        assert_eq!(load.queries[0].item_bytes, 6 + 96);
    }

    #[test]
    fn exactly_sixteen_messages_per_second_pass_and_seventeen_are_refused() {
        // 100 ms（10/s）+ 200 ms（5/s）+ 1000 ms（1/s）= 16 / 秒ちょうど。
        let at = bridge(vec![
            query(100, short_keys(1)),
            query(200, short_keys(1)),
            query(1000, short_keys(1)),
        ]);
        let load = bridge_load(&at);
        assert_eq!(load.messages_per_minute, 16 * 60);
        assert!(load.within_registration_limit());
        assert_eq!(load.over_registration_limit_message(), None);

        let mut over = at;
        over.queries.push(query(1000, short_keys(1)));
        let load = bridge_load(&over);
        assert_eq!(load.messages_per_minute, 17 * 60);
        assert!(!load.within_registration_limit());
        assert_eq!(
            load.over_registration_limit_message().unwrap(),
            "この設定は 1 秒あたり約 17 メッセージ・17 件になり、上限（16 メッセージ・4,000 件）を\
             超えます。周期を長くするか、クエリを別の push 接続に分けてください"
        );
    }

    #[test]
    fn exactly_four_thousand_items_per_second_pass_and_one_more_is_refused() {
        // 1000 ms × 4,000 件（4 メッセージ / 周期）= 4,000 件 / 秒ちょうど。
        let load = bridge_load(&bridge(vec![query(1000, short_keys(4000))]));
        assert_eq!(load.items_per_minute, 4000 * 60);
        assert_eq!(load.queries[0].messages_per_cycle, 4);
        assert!(load.within_registration_limit());

        let load = bridge_load(&bridge(vec![query(1000, short_keys(4001))]));
        assert_eq!(load.items_per_minute, 4001 * 60);
        assert!(!load.within_registration_limit());
        assert!(load
            .over_registration_limit_message()
            .unwrap()
            .contains("約 5 メッセージ・4,001 件"));

        // 100 ms × 400 件 = 4,000 件 / 秒（10 メッセージ / 秒）。401 件で超える。
        assert!(bridge_load(&bridge(vec![query(100, short_keys(400))])).within_registration_limit());
        assert!(
            !bridge_load(&bridge(vec![query(100, short_keys(401))])).within_registration_limit()
        );
    }

    #[test]
    fn multi_query_totals_three_at_100ms_refused_three_at_200ms_pass() {
        let three = |period| {
            bridge(vec![
                query(period, short_keys(1)),
                query(period, short_keys(1)),
                query(period, short_keys(1)),
            ])
        };
        let fast = bridge_load(&three(100));
        assert_eq!(fast.messages_per_minute, 30 * 60);
        assert_eq!(fast.messages_per_sec(), 30.0);
        assert!(!fast.within_registration_limit());
        let slow = bridge_load(&three(200));
        assert_eq!(slow.messages_per_minute, 15 * 60);
        assert!(slow.within_registration_limit());
    }

    #[test]
    fn a_slow_period_counts_fractional_rates_exactly() {
        // 60000 ms × 1 メッセージ = 1 / 分。表示は切り上げで 0.1 / 秒。
        let load = bridge_load(&bridge(vec![query(60_000, short_keys(1))]));
        assert_eq!(load.messages_per_minute, 1);
        assert_eq!(format_rate(load.messages_per_minute), "0.1");
        assert_eq!(format_rate(16 * 60), "16");
        assert_eq!(format_rate(165 * 6), "16.5");
        assert_eq!(group_thousands(1_234_567), "1,234,567");
    }

    // ---- validate_push_key ---------------------------------------------

    #[test]
    fn validate_push_key_table() {
        assert_eq!(
            validate_push_key("line1.temp"),
            Ok("line1.temp".to_string())
        );
        assert_eq!(
            validate_push_key("  Line1/Temp "),
            Ok("Line1/Temp".to_string())
        );
        assert_eq!(validate_push_key("温度 1"), Ok("温度 1".to_string()));
        let longest = "あ".repeat(MAX_PUSH_KEY_LEN);
        assert_eq!(validate_push_key(&longest), Ok(longest.clone()));
        assert!(validate_push_key(&format!("{longest}x")).is_err());
        for bad in ["", "   ", "a\tb", "a\nb", "a\u{7f}b", "a\u{0}"] {
            assert!(validate_push_key(bad).is_err(), "{bad:?}");
        }
        // 正規形は冪等（PushDriver の C1 の前提）。
        let canonical = validate_push_key(" k ").unwrap();
        assert_eq!(validate_push_key(&canonical), Ok(canonical.clone()));
    }

    // ---- validate_push_settings ----------------------------------------

    fn db_bridge(queries: Value) -> Value {
        json!({ "kind": "db", "db_connection_id": 12, "queries": queries })
    }

    #[test]
    fn empty_settings_take_the_default_timeout() {
        assert_eq!(
            validate_push_settings(&json!({})).unwrap(),
            PushSettings::default()
        );
        assert_eq!(PushSettings::default().stale_timeout_ms, 10_000);
    }

    #[test]
    fn stale_timeout_range_is_one_second_to_one_hour() {
        for ok in [1_000u64, 10_000, 3_600_000] {
            let settings = validate_push_settings(&json!({ "stale_timeout_ms": ok })).unwrap();
            assert_eq!(settings.stale_timeout_ms, ok);
        }
        for bad in [
            json!(999),
            json!(3_600_001),
            json!(0),
            json!(-1),
            json!(1500.5),
            json!("10000"),
        ] {
            let errors = validate_push_settings(&json!({ "stale_timeout_ms": bad })).unwrap_err();
            assert_eq!(fields(&errors), ["settings.stale_timeout_ms"], "{bad}");
        }
    }

    #[test]
    fn unknown_keys_and_non_objects_are_refused_at_every_level() {
        let errors = validate_push_settings(&json!({ "stale_timeout": 1000 })).unwrap_err();
        assert_eq!(fields(&errors), ["settings"]);
        assert!(errors[0].message.contains("stale_timeout"));
        assert_eq!(
            fields(&validate_push_settings(&json!([1])).unwrap_err()),
            ["settings"]
        );
        assert_eq!(
            fields(&validate_push_settings(&Value::Null).unwrap_err()),
            ["settings"]
        );
        let errors = validate_push_settings(&json!({ "bridge": {
            "kind": "db", "db_connection_id": 1, "extra": true,
            "queries": [{ "name": "q", "period_ms": 1000, "sql": "SELECT 1", "x": 1,
                          "columns": [{ "column": "v", "key": "k", "y": 2 }] }]
        }}))
        .unwrap_err();
        assert_eq!(
            fields(&errors),
            [
                "settings.bridge",
                "settings.bridge.queries[0]",
                "settings.bridge.queries[0].columns[0]"
            ]
        );
    }

    #[test]
    fn the_size_limit_is_measured_on_the_stored_json() {
        // {"bridge":{"kind":"db","db_connection_id":1,"queries":[{"name":"q","period_ms":60000,
        //  "sql":"SELECT <pad>","columns":[]}]}} を 64 KiB ちょうどに合わせる。
        let shell = |pad: usize| {
            db_bridge(json!([{ "name": "q", "period_ms": 60000,
                               "sql": format!("SELECT {}", "x".repeat(pad)), "columns": [] }]))
        };
        let base = serde_json::to_string(&json!({ "bridge": shell(0) }))
            .unwrap()
            .len();
        let pad = 65_536 - base;
        let at = json!({ "bridge": shell(pad) });
        assert_eq!(serde_json::to_string(&at).unwrap().len(), 65_536);
        // 大きさは通る（SQL の長さの規則は別。ここでは大きさの検査だけを見たい）。
        let errors = validate_push_settings(&at).err().unwrap_or_default();
        assert!(!fields(&errors).contains(&"settings"), "{errors:?}");

        let over = json!({ "bridge": shell(pad + 1) });
        let errors = validate_push_settings(&over).unwrap_err();
        assert_eq!(fields(&errors), ["settings"]);
        assert!(errors[0].message.contains("65536"), "{}", errors[0].message);
    }

    #[test]
    fn a_valid_bridge_round_trips_into_the_parsed_struct() {
        let settings = validate_push_settings(&json!({
            "stale_timeout_ms": 5000,
            "bridge": db_bridge(json!([{
                "name": "line1", "period_ms": 1000, "sql": "SELECT temp, run FROM line1_now",
                "columns": [ { "column": "temp", "key": "line1.temp" },
                             { "column": "run", "key": "line1.run" } ]
            }]))
        }))
        .unwrap();
        assert_eq!(
            settings,
            PushSettings {
                stale_timeout_ms: 5000,
                bridge: Some(PushBridge {
                    db_connection_id: 12,
                    queries: vec![BridgeQuery {
                        name: "line1".to_string(),
                        period_ms: 1000,
                        sql: "SELECT temp, run FROM line1_now".to_string(),
                        columns: vec![
                            BridgeColumn {
                                column: "temp".to_string(),
                                key: "line1.temp".to_string()
                            },
                            BridgeColumn {
                                column: "run".to_string(),
                                key: "line1.run".to_string()
                            },
                        ],
                    }],
                }),
            }
        );
    }

    #[test]
    fn bridge_field_rules() {
        let check = |bridge: Value| validate_push_settings(&json!({ "bridge": bridge }));
        // kind
        let errors =
            check(json!({ "kind": "mqtt", "db_connection_id": 1, "queries": [] })).unwrap_err();
        assert_eq!(fields(&errors), ["settings.bridge.kind"]);
        let errors = check(json!({ "db_connection_id": 1, "queries": [] })).unwrap_err();
        assert_eq!(fields(&errors), ["settings.bridge.kind"]);
        // db_connection_id
        for bad in [json!(0), json!(-3), json!("12"), json!(1.5)] {
            let errors =
                check(json!({ "kind": "db", "db_connection_id": bad, "queries": [] })).unwrap_err();
            assert_eq!(
                fields(&errors),
                ["settings.bridge.db_connection_id"],
                "{bad}"
            );
        }
        // queries
        let errors = check(json!({ "kind": "db", "db_connection_id": 1 })).unwrap_err();
        assert_eq!(fields(&errors), ["settings.bridge.queries"]);
        // 一つのクエリの全項目
        let errors = check(db_bridge(json!([{
            "name": "", "period_ms": 300, "sql": "DELETE FROM t",
            "columns": [ { "column": "1bad", "key": " k" }, { "key": "\u{1}" } ]
        }])))
        .unwrap_err();
        assert_eq!(
            fields(&errors),
            [
                "settings.bridge.queries[0].name",
                "settings.bridge.queries[0].period_ms",
                "settings.bridge.queries[0].sql",
                "settings.bridge.queries[0].columns[0].column",
                "settings.bridge.queries[0].columns[0].key",
                "settings.bridge.queries[0].columns[1].column",
                "settings.bridge.queries[0].columns[1].key",
            ]
        );
        // 名前の重複・キーの重複（区画全体で一意。別のクエリでも断る）
        let errors = check(db_bridge(json!([
            { "name": "a", "period_ms": 1000, "sql": "SELECT 1", "columns": [{ "column": "v", "key": "k" }] },
            { "name": "a", "period_ms": 1000, "sql": "SELECT 1", "columns": [{ "column": "v", "key": "k" }] },
        ])))
        .unwrap_err();
        assert_eq!(
            fields(&errors),
            [
                "settings.bridge.queries[1].columns[0].key",
                "settings.bridge.queries[1].name"
            ]
        );
        // 名前の長さ
        let long = "n".repeat(MAX_BRIDGE_QUERY_NAME_LEN + 1);
        let errors = check(db_bridge(
            json!([{ "name": long, "period_ms": 1000, "sql": "SELECT 1", "columns": [] }]),
        ))
        .unwrap_err();
        assert_eq!(fields(&errors), ["settings.bridge.queries[0].name"]);
    }

    #[test]
    fn the_registration_estimate_refuses_through_validate_push_settings() {
        let one = |period: i64, n: usize, name: &str| {
            json!({ "name": name, "period_ms": period, "sql": "SELECT 1 AS v",
                    "columns": (0..n).map(|i| json!({ "column": "v", "key": format!("{name}.{i}") }))
                        .collect::<Vec<_>>() })
        };
        // 3 本 × 100 ms = 30 メッセージ / 秒は断り、3 本 × 200 ms = 15 は通る。
        let fast = json!({ "bridge": db_bridge(json!([one(100, 1, "a"), one(100, 1, "b"), one(100, 1, "c")])) });
        let errors = validate_push_settings(&fast).unwrap_err();
        assert_eq!(fields(&errors), ["settings.bridge"]);
        assert!(
            errors[0].message.contains("約 30 メッセージ"),
            "{}",
            errors[0].message
        );
        let slow = json!({ "bridge": db_bridge(json!([one(200, 1, "a"), one(200, 1, "b"), one(200, 1, "c")])) });
        assert!(validate_push_settings(&slow).is_ok());
        // 100 ms × 400 件 = 4,000 件 / 秒ちょうどは通り、401 件は断る。
        let at = json!({ "bridge": db_bridge(json!([one(100, 400, "a")])) });
        assert!(validate_push_settings(&at).is_ok());
        let over = json!({ "bridge": db_bridge(json!([one(100, 401, "a")])) });
        let errors = validate_push_settings(&over).unwrap_err();
        assert_eq!(fields(&errors), ["settings.bridge"]);
        assert!(
            errors[0].message.contains("4,010 件"),
            "{}",
            errors[0].message
        );
    }
}
