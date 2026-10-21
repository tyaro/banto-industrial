//! 取り込み口の WebSocket のメッセージ（docs/banto-hub-ingest-design.md §6.1）の
//! ブリッジ側の写し。Hub 側は `banto_hub_core::ingest::ws`。
//!
//! ```jsonc
//! // ブリッジ → Hub
//! { "op": "values", "seq": 17, "epoch": 3, "items": [ { "key": "line1.temp", "v": 12.5, "q": "good" } ] }
//! { "op": "ping", "seq": 18 }
//! { "op": "status", "detail": "line1: クエリが失敗しました（...）" }
//! // Hub → ブリッジ
//! { "op": "welcome", "connection_id": 12, "connection": "Bridge1", "collecting": true, "epoch": 3,
//!   "stale_timeout_ms": 10000, "limits": { "max_items": 1000, "max_message_bytes": 262144, "messages_per_sec": 20, "items_per_sec": 5000 } }
//! { "op": "ack", "seq": 17, "accepted": 1, "rejected": [ { "index": 0, "key": "line1.tmp", "code": "unknown_key" } ] }
//! { "op": "pong", "seq": 18 }
//! { "op": "error", "seq": 19, "code": "rate_limited", "detail": "..." }
//! { "op": "state", "collecting": false, "epoch": 4, "config_changed": false }
//! ```
//!
//! - `t`（発生時刻）は送らない（任意で、Hub は鮮度・記録の時刻に使わない。H4）。
//! - Hub が足した知らない `op`・項目は読み飛ばす（前方互換。設定ファイルと逆の判断で、
//!   [`crate::hub_api`] の DTO と同じ）。

use serde::{Deserialize, Serialize};

/// 送る品質（§4.4。ブリッジは梯子の判断をこの 3 つに写す）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum WireQuality {
    Good,
    Stale,
    Bad,
}

/// `values` の 1 件。`v` は `Good` / `Stale` で値、`Bad` で `null`（梯子の
/// [`banto_db_source::Verdict::settle`] の後なので、値の無い `Stale` は来ない - §4.4 の
/// 「`stale` + `v: null` は Bad」を送らない）。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ValueItem {
    pub key: String,
    pub v: Option<f64>,
    pub q: WireQuality,
}

impl ValueItem {
    pub fn good(key: &str, value: f64) -> Self {
        Self {
            key: key.to_string(),
            v: Some(value),
            q: WireQuality::Good,
        }
    }

    pub fn stale(key: &str, value: f64) -> Self {
        Self {
            key: key.to_string(),
            v: Some(value),
            q: WireQuality::Stale,
        }
    }

    pub fn bad(key: &str) -> Self {
        Self {
            key: key.to_string(),
            v: None,
            q: WireQuality::Bad,
        }
    }
}

#[derive(Serialize)]
struct ValuesWire<'a> {
    op: &'static str,
    seq: u64,
    epoch: u64,
    items: &'a [ValueItem],
}

#[derive(Serialize)]
struct PingWire {
    op: &'static str,
    seq: u64,
}

#[derive(Serialize)]
struct StatusWire<'a> {
    op: &'static str,
    detail: &'a str,
}

/// `values` の 1 メッセージの JSON。
pub fn values_message(seq: u64, epoch: u64, items: &[ValueItem]) -> String {
    serde_json::to_string(&ValuesWire {
        op: "values",
        seq,
        epoch,
        items,
    })
    .expect("ValueItem は常にシリアライズできる（v は有限値か null）")
}

/// `ping` の JSON。
pub fn ping_message(seq: u64) -> String {
    serde_json::to_string(&PingWire { op: "ping", seq }).expect("ping は常にシリアライズできる")
}

/// `status` の JSON（`detail` は呼び出し側が秘密を伏せて切り詰めたもの）。
pub fn status_message(detail: &str) -> String {
    serde_json::to_string(&StatusWire {
        op: "status",
        detail,
    })
    .expect("status は常にシリアライズできる")
}

/// `welcome` の `limits`（§7。ブリッジの分割と速さの抑えはこの値から作る - §7「ブリッジ
/// （`welcome` で受けた値）が同じ値を使う」）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct WireLimits {
    pub max_items: u64,
    pub max_message_bytes: u64,
    pub messages_per_sec: u64,
    pub items_per_sec: u64,
}

/// `ack` の `rejected[]` の 1 件。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RejectedItem {
    #[serde(default)]
    pub index: Option<u64>,
    #[serde(default)]
    pub key: Option<String>,
    pub code: String,
}

/// Hub から来るメッセージ。
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum ServerMessage {
    Welcome {
        connection_id: i64,
        #[serde(default)]
        connection: String,
        collecting: bool,
        epoch: u64,
        stale_timeout_ms: u64,
        limits: WireLimits,
    },
    Ack {
        #[serde(default)]
        seq: Option<u64>,
        #[serde(default)]
        accepted: u64,
        #[serde(default)]
        rejected: Vec<RejectedItem>,
    },
    Pong {
        #[serde(default)]
        seq: Option<u64>,
    },
    Error {
        #[serde(default)]
        seq: Option<u64>,
        code: String,
        #[serde(default)]
        detail: String,
    },
    State {
        collecting: bool,
        epoch: u64,
        #[serde(default)]
        config_changed: bool,
    },
    /// 知らない `op`（Hub が後で足したもの）。読み飛ばす。
    #[serde(other)]
    Unknown,
}

/// Hub のメッセージを読む。読めなければ `None`（読み飛ばす - Hub の不具合でブリッジを
/// 落とさない）。
pub fn parse_server_message(text: &str) -> Option<ServerMessage> {
    serde_json::from_str(text).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    #[test]
    fn values_message_matches_the_wire_shape() {
        let items = vec![
            ValueItem::good("line1.temp", 12.5),
            ValueItem::stale("line1.run", 1.0),
            ValueItem::bad("line1.rate"),
        ];
        let text = values_message(17, 3, &items);
        let parsed: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            parsed,
            json!({ "op": "values", "seq": 17, "epoch": 3, "items": [
                { "key": "line1.temp", "v": 12.5, "q": "good" },
                { "key": "line1.run", "v": 1.0, "q": "stale" },
                { "key": "line1.rate", "v": null, "q": "bad" } ] })
        );
    }

    #[test]
    fn ping_and_status_messages() {
        assert_eq!(ping_message(18), r#"{"op":"ping","seq":18}"#);
        assert_eq!(
            status_message("line1: 失敗"),
            r#"{"op":"status","detail":"line1: 失敗"}"#
        );
    }

    #[test]
    fn server_messages_are_parsed() {
        let welcome = parse_server_message(
            r#"{"op":"welcome","connection_id":12,"connection":"Bridge1","collecting":true,"epoch":3,
               "stale_timeout_ms":10000,"limits":{"max_items":1000,"max_message_bytes":262144,
               "messages_per_sec":20,"items_per_sec":5000}}"#,
        );
        assert_eq!(
            welcome,
            Some(ServerMessage::Welcome {
                connection_id: 12,
                connection: "Bridge1".to_string(),
                collecting: true,
                epoch: 3,
                stale_timeout_ms: 10_000,
                limits: WireLimits {
                    max_items: 1000,
                    max_message_bytes: 262_144,
                    messages_per_sec: 20,
                    items_per_sec: 5000,
                },
            })
        );
        assert_eq!(
            parse_server_message(
                r#"{"op":"state","collecting":false,"epoch":4,"config_changed":true}"#
            ),
            Some(ServerMessage::State {
                collecting: false,
                epoch: 4,
                config_changed: true
            })
        );
        assert_eq!(
            parse_server_message(
                r#"{"op":"ack","seq":17,"accepted":1,"rejected":[{"index":0,"key":"k","code":"unknown_key"}]}"#
            ),
            Some(ServerMessage::Ack {
                seq: Some(17),
                accepted: 1,
                rejected: vec![RejectedItem {
                    index: Some(0),
                    key: Some("k".to_string()),
                    code: "unknown_key".to_string()
                }],
            })
        );
        assert_eq!(
            parse_server_message(r#"{"op":"error","seq":19,"code":"stale_epoch","detail":"x"}"#),
            Some(ServerMessage::Error {
                seq: Some(19),
                code: "stale_epoch".to_string(),
                detail: "x".to_string()
            })
        );
        assert_eq!(
            parse_server_message(r#"{"op":"future","x":1}"#),
            Some(ServerMessage::Unknown)
        );
        assert_eq!(parse_server_message("not json"), None);
    }
}
