//! 送り手のメッセージの検証（R2a-2c、docs/banto-hub-ingest-design.md §6.2・§6.3・§7）。
//! REST（[`super::rest`]）と R2a-2d の WebSocket が同じ関数を使う。
//!
//! 2 つの段に分ける:
//!
//! - **メッセージ全体**: [`parse_values_message`] が本文（WS ではテキストのフレーム）を
//!   [`IngestValuesMessage`]（`epoch` と `items`）に読む。JSON・形の誤りは
//!   [`MessageError::InvalidMessage`]（REST 400 / WS `error` `invalid_message`）で、
//!   何も反映しない。件数の上限（1,000 件、§7）はここでは見ない - 頻度のバケットと
//!   同じ点で数えるため [`super::limits::ConnectionLimiter::admit`] が見る。
//! - **件ごと**: [`validate_item`]（純関数）が 1 件の値と品質を §6.3 の表で判定する。
//!   キーの照合（登録・有効・送る集合・重複）は `IngestHub::apply` が先に済ませる
//!   （登録済みのキーの表を持つのは受信側だけなので）。
//!
//! 浮動小数の読み取りは往復で一致させる（§6.3）: `banto-hub-core` の `serde_json` は
//! `float_roundtrip` を有効にしている（`Cargo.toml`）。既定の解析は最後の桁が
//! ずれうるので、DB ブリッジと Hub 内の Source の値の一致（§10.4）の前提になる。

use banto_collect::{ExternalQuality, ExternalReading};
use serde::Deserialize;
use serde_json::Value;
use utoipa::ToSchema;

use super::{IngestItem, RejectCode};

/// 値のメッセージ 1 つ（REST の本文・WebSocket の `values` の中身）。知らない
/// フィールドは読み飛ばす。
// 実装の注: WS（R2a-2d）の `op`・`seq` もこの読み飛ばしで同じ型に読める。OpenAPI の
// スキーマ（REST の要求の本文）はこの型から生成する（コードとスキーマを 1 つにする）。
#[derive(Debug, Clone, PartialEq, Deserialize, ToSchema)]
pub struct IngestValuesMessage {
    /// 接続の世代。DB ブリッジの接続では必須で、今の epoch と違えば 409
    /// `stale_epoch`。一般の送り手では任意（付ければ同じく照合する）。
    #[serde(default)]
    pub epoch: Option<u64>,
    /// 値（1,000 件まで）。
    pub items: Vec<IngestItem>,
}

/// メッセージ全体を断る理由（件ごとの [`RejectCode`] とは別）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageError {
    /// JSON として読めない・形が違う（`items` が無い、`epoch` が負の数など）。
    /// `detail` は serde の文言（値は含めるが、本文は送り手自身のものなので応答に
    /// 返してよい。監査には載せない）。
    InvalidMessage { detail: String },
}

impl MessageError {
    /// wire の綴り（§6.1 の `error`・§6.2 の 400）。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::InvalidMessage { .. } => "invalid_message",
        }
    }
}

/// 本文を [`IngestValuesMessage`] に読む（§6.2 の 400 `invalid_message`）。
pub fn parse_values_message(body: &[u8]) -> Result<IngestValuesMessage, MessageError> {
    serde_json::from_slice(body).map_err(|err| MessageError::InvalidMessage {
        detail: err.to_string(),
    })
}

/// 1 件の値と品質の検証（§6.3 の `invalid_*`、純関数）。キーの照合（登録・有効・
/// 送る集合・重複）は `IngestHub::apply` が先に済ませる。`data_type` はタグの型
/// （`bit` は 0・1・bool だけ）。数値の範囲は見ない（§6.3）。
pub fn validate_item(item: &IngestItem, data_type: &str) -> Result<ExternalReading, RejectCode> {
    let quality = match &item.q {
        None => ExternalQuality::Good,
        Some(Value::String(q)) => match q.as_str() {
            "good" => ExternalQuality::Good,
            "stale" => ExternalQuality::Stale,
            "bad" => ExternalQuality::Bad,
            _ => return Err(RejectCode::InvalidQuality),
        },
        Some(_) => return Err(RejectCode::InvalidQuality),
    };
    if let Some(t) = &item.t {
        if t.as_i64().is_none() {
            return Err(RejectCode::InvalidTime);
        }
    }
    let value = match &item.v {
        Value::Null => None,
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::Number(n) => match n.as_f64() {
            Some(v) if v.is_finite() => Some(v),
            _ => return Err(RejectCode::InvalidValue),
        },
        _ => return Err(RejectCode::InvalidValue),
    };
    if data_type == "bit" && value.is_some_and(|v| v != 0.0 && v != 1.0) {
        return Err(RejectCode::InvalidValue);
    }
    match (quality, value) {
        (ExternalQuality::Good, None) => Err(RejectCode::InvalidValue),
        // bad は値を持たない（表示も記録も Bad / NULL、§4.4）。
        (ExternalQuality::Bad, _) => Ok(ExternalReading {
            value: None,
            quality,
        }),
        _ => Ok(ExternalReading { value, quality }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// メッセージの形: `epoch` は任意、`items` は必須。知らないフィールドは読み飛ばす。
    /// 形の誤りは `invalid_message`。
    #[test]
    fn the_message_shape_table() {
        let ok = parse_values_message(br#"{"epoch":3,"items":[{"key":"a","v":1}]}"#).unwrap();
        assert_eq!(ok.epoch, Some(3));
        assert_eq!(ok.items, vec![IngestItem::new("a", serde_json::json!(1))]);
        let no_epoch = parse_values_message(br#"{"op":"values","seq":1,"items":[]}"#).unwrap();
        assert_eq!(no_epoch.epoch, None);
        assert!(no_epoch.items.is_empty());
        for bad in [
            &b""[..],
            b"not json",
            b"[]",
            b"{}",
            br#"{"items":{}}"#,
            br#"{"epoch":-1,"items":[]}"#,
            br#"{"epoch":1.5,"items":[]}"#,
            br#"{"items":[{"v":1}]}"#,
            br#"{"items":[{"key":1,"v":1}]}"#,
        ] {
            let err = parse_values_message(bad).unwrap_err();
            assert_eq!(err.as_str(), "invalid_message", "{bad:?}");
        }
    }

    /// `float_roundtrip`（§6.3）: 最短の 10 進表記を往復して同じビットになる。
    /// 既定の解析（`float_roundtrip` なし）では最後の桁がずれうる値を含める。
    #[test]
    fn floats_are_read_back_bit_exactly() {
        for v in [
            0.1 + 0.2,
            1.0 / 3.0,
            f64::MIN_POSITIVE,
            f64::MAX,
            123_456.789_012_345_68,
            2.225_073_858_507_201e-308,
            9_007_199_254_740_993.0,
        ] {
            let text = format!(r#"{{"items":[{{"key":"k","v":{v:?}}}]}}"#);
            let message = parse_values_message(text.as_bytes()).unwrap();
            let reading = validate_item(&message.items[0], "f64").unwrap();
            assert_eq!(reading.value.map(f64::to_bits), Some(v.to_bits()), "{text}");
        }
    }
}
