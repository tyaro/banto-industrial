//! ドライバが返した生の値をホストの値へ写す。文字列のバイト順はドライバが揃える。

use banto_driver_api::{DataType, ItemError, RawValue, StringEncoding, TagKind, Value};

/// 最初の NUL で切り、置換文字を許さずに decode する。
pub fn decode_string(bytes: &[u8], encoding: StringEncoding) -> Result<String, ItemError> {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    let table = match encoding {
        StringEncoding::Utf8 => encoding_rs::UTF_8,
        StringEncoding::ShiftJis => encoding_rs::SHIFT_JIS,
    };
    let (text, _, errors) = table.decode(&bytes[..end]);
    if errors {
        Err(ItemError::Unsupported("文字列の符号化が不正です".into()))
    } else {
        Ok(text.into_owned())
    }
}

/// 数値の整数精度は API の Value 契約どおり f64 に揃える。
pub fn decode_value(kind: &TagKind, raw: RawValue) -> Result<Value, ItemError> {
    match (kind, raw) {
        (TagKind::Numeric(DataType::Bit), RawValue::Bool(v)) => Ok(Value::Bit(v)),
        (kind, RawValue::Int(v)) if is_scalar_numeric(kind) => Ok(Value::F64(v as f64)),
        (kind, RawValue::Uint(v)) if is_scalar_numeric(kind) => Ok(Value::F64(v as f64)),
        (kind, RawValue::Float(v)) if is_scalar_numeric(kind) => Ok(Value::F64(v)),
        (TagKind::Str { words, encoding }, RawValue::Bytes(bytes))
            if bytes.len() <= usize::from(*words) * 2 =>
        {
            decode_string(&bytes, *encoding).map(Value::Str)
        }
        _ => Err(ItemError::Unsupported(
            "タグの型と生の値が合いません".into(),
        )),
    }
}

fn is_scalar_numeric(kind: &TagKind) -> bool {
    matches!(
        kind,
        TagKind::Numeric(
            DataType::I16
                | DataType::U16
                | DataType::I32
                | DataType::U32
                | DataType::F32
                | DataType::I64
                | DataType::U64
                | DataType::F64
        )
    )
}
