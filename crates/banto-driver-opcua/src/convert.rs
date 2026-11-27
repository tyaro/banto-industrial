//! Pure mappings between the component's vocabulary (`banto-opcua-core`) and
//! the Driver API's (docs/chronogazer/opcua-client-design.md §3.4-§3.6).
//! No I/O, so every rule is unit-tested here.
//!
//! ## Reading a value (`reading_of`)
//!
//! | StatusCode (by its severity bits) | Value | Result |
//! | --- | --- | --- |
//! | Good | present | `Ok(Reading { quality: Good })` |
//! | Uncertain | present | `Ok(Reading { quality: Uncertain })` |
//! | Good / Uncertain | none | `ItemError::Device` (no value to record) |
//! | `BadNotSupported` (the component's "array or unsupported type") | - | `ItemError::Unsupported` |
//! | any other Bad (also the reserved severity `11`) | - | `ItemError::Device { code: status }` |
//!
//! The value then has to fit the tag's kind (`value_of`): Boolean for `bit`;
//! a number for the numeric kinds, widened to `f64`. An integer kind
//! (`i16`..`u64`) refuses a fraction, NaN / infinity and anything outside its
//! range (checked exactly, before widening, for 64-bit sources); `f32`
//! refuses a finite value that overflows `f32` and otherwise rounds to `f32`
//! precision. A Boolean for a numeric kind, a number for `bit` and a String
//! are `ItemError::Unsupported` - never coerced.
//!
//! ## Writing a value (`scalar_for_write`)
//!
//! The component never converts types (ADR-0005), so the value is sent as
//! the node's built-in type. `bit` writes a Boolean node only; a number writes
//! a numeric node, exactly (an integer node refuses a fraction or an
//! out-of-range value as `ItemError::Rejected`: nothing was sent). A String
//! node, or a bit to a numeric node, is `ItemError::Unsupported`.

use std::time::{SystemTime, UNIX_EPOCH};

use banto_driver_api::{
    DataType, DeviceError, FatalError, ItemError, ReadQuality, Reading, TagKind, Value,
};
use banto_opcua_client::{Error, NodeReading};
use banto_opcua_core::{ScalarType, ScalarValue, StatusCode};

/// `BadTimeout` (0x800A0000; the component's `StatusCode` has no name for it).
const BAD_TIMEOUT: u32 = 0x800A_0000;
/// Codes that mean the peer broke the binary protocol (a broken chunk, an
/// impossible response): `BadEncodingError`, `BadDecodingError`,
/// `BadUnknownResponse`, `BadTcpMessageTypeInvalid`, `BadTcpMessageTooLarge`,
/// `BadSequenceNumberInvalid`.
const PROTOCOL_CODES: [u32; 6] = [
    0x8006_0000,
    0x8007_0000,
    0x8009_0000,
    0x807E_0000,
    0x8080_0000,
    0x8088_0000,
];

/// A Read result as the tag's [`Reading`] (the table in the module doc).
pub(crate) fn reading_of(node: &NodeReading, kind: &TagKind) -> Result<Reading, ItemError> {
    let status = node.status;
    let quality = if status.is_good() {
        ReadQuality::Good
    } else if status.is_uncertain() {
        ReadQuality::Uncertain
    } else {
        return Err(status_error(status));
    };
    let Some(value) = &node.value else {
        return Err(ItemError::Device(DeviceError::new(
            status.code(),
            format!("値がありません（{status}）"),
        )));
    };
    let value = value_of(value, kind)?;
    Ok(Reading::new(
        value,
        quality,
        node.source_timestamp.and_then(unix_ms),
    ))
}

/// A Bad StatusCode as the tag's error.
pub(crate) fn status_error(status: StatusCode) -> ItemError {
    if status == StatusCode::BAD_NOT_SUPPORTED {
        return ItemError::Unsupported(
            "配列や対応しない型の値です（BadNotSupported）。スカラーの Boolean か数値のノードを指定してください"
                .into(),
        );
    }
    ItemError::Device(DeviceError::new(status.code(), status.to_string()))
}

/// Milliseconds since the UNIX epoch (negative before it); `None` when it
/// does not fit an `i64`.
pub(crate) fn unix_ms(time: SystemTime) -> Option<i64> {
    match time.duration_since(UNIX_EPOCH) {
        Ok(after) => i64::try_from(after.as_millis()).ok(),
        Err(before) => i64::try_from(before.duration().as_millis())
            .ok()
            .map(|ms| -ms),
    }
}

/// A number read from a node, kept exact until it meets the tag's kind.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Number {
    Int(i128),
    Float(f64),
}

fn number_of(value: &ScalarValue) -> Option<Number> {
    Some(match *value {
        ScalarValue::SByte(v) => Number::Int(v.into()),
        ScalarValue::Byte(v) => Number::Int(v.into()),
        ScalarValue::Int16(v) => Number::Int(v.into()),
        ScalarValue::UInt16(v) => Number::Int(v.into()),
        ScalarValue::Int32(v) => Number::Int(v.into()),
        ScalarValue::UInt32(v) => Number::Int(v.into()),
        ScalarValue::Int64(v) => Number::Int(v.into()),
        ScalarValue::UInt64(v) => Number::Int(v.into()),
        ScalarValue::Float(v) => Number::Float(v.into()),
        ScalarValue::Double(v) => Number::Float(v),
        ScalarValue::Boolean(_) | ScalarValue::String(_) => return None,
    })
}

/// The inclusive range of an integer kind; `None` for `bit` / `f32` / `f64`.
fn int_range(data_type: DataType) -> Option<(i128, i128)> {
    Some(match data_type {
        DataType::I16 => (i16::MIN.into(), i16::MAX.into()),
        DataType::U16 => (0, u16::MAX.into()),
        DataType::I32 => (i32::MIN.into(), i32::MAX.into()),
        DataType::U32 => (0, u32::MAX.into()),
        DataType::I64 => (i64::MIN.into(), i64::MAX.into()),
        DataType::U64 => (0, u64::MAX.into()),
        DataType::Bit | DataType::F32 | DataType::F64 => return None,
    })
}

/// An integral, finite `x` as an exact integer (`None` otherwise).
fn exact_int(x: f64) -> Option<i128> {
    // `as i128` saturates, so a huge `x` lands outside every range below.
    (x.is_finite() && x.fract() == 0.0).then_some(x as i128)
}

/// The node's value as the tag's [`Value`] (the module doc's rules).
pub(crate) fn value_of(value: &ScalarValue, kind: &TagKind) -> Result<Value, ItemError> {
    let TagKind::Numeric(data_type) = *kind else {
        return Err(ItemError::Unsupported(crate::base::STRING_REFUSED.into()));
    };
    let kind_name = data_type.to_string();
    if data_type == DataType::Bit {
        return match value {
            ScalarValue::Boolean(b) => Ok(Value::Bit(*b)),
            other => Err(ItemError::Unsupported(format!(
                "bit 型のタグに {} の値が来ました（Boolean のノードを指定してください）",
                other.scalar_type()
            ))),
        };
    }
    let Some(number) = number_of(value) else {
        return Err(ItemError::Unsupported(format!(
            "{kind_name} 型のタグに {} の値が来ました（数値のノードを指定するか、型を bit にしてください）",
            value.scalar_type()
        )));
    };
    if let Some((min, max)) = int_range(data_type) {
        let exact = match number {
            Number::Int(v) => Some(v),
            Number::Float(x) => exact_int(x),
        };
        return match exact {
            Some(v) if (min..=max).contains(&v) => Ok(Value::F64(v as f64)),
            _ => Err(ItemError::Unsupported(format!(
                "{} は {kind_name} 型で表せません（小数・範囲外・NaN は丸めずに Bad にします。型を f64 にしてください）",
                show(number)
            ))),
        };
    }
    let x = match number {
        Number::Int(v) => v as f64,
        Number::Float(x) => x,
    };
    if data_type == DataType::F32 {
        let narrowed = x as f32;
        if x.is_finite() && narrowed.is_infinite() {
            return Err(ItemError::Unsupported(format!(
                "{x} は f32 型の範囲外です（型を f64 にしてください）"
            )));
        }
        return Ok(Value::F64(narrowed.into()));
    }
    Ok(Value::F64(x))
}

fn show(number: Number) -> String {
    match number {
        Number::Int(v) => v.to_string(),
        Number::Float(x) => x.to_string(),
    }
}

/// The built-in type a tag of `data_type` is written as when the node's own
/// type is not known (its value could not be read).
pub(crate) fn scalar_type_for(data_type: DataType) -> ScalarType {
    match data_type {
        DataType::Bit => ScalarType::Boolean,
        DataType::I16 => ScalarType::Int16,
        DataType::U16 => ScalarType::UInt16,
        DataType::I32 => ScalarType::Int32,
        DataType::U32 => ScalarType::UInt32,
        DataType::F32 => ScalarType::Float,
        DataType::I64 => ScalarType::Int64,
        DataType::U64 => ScalarType::UInt64,
        DataType::F64 => ScalarType::Double,
    }
}

/// `value` as the node's built-in type `target` (the module doc's rules).
pub(crate) fn scalar_for_write(
    value: &Value,
    target: ScalarType,
) -> Result<ScalarValue, ItemError> {
    let x = match (value, target) {
        (Value::Bit(b), ScalarType::Boolean) => return Ok(ScalarValue::Boolean(*b)),
        (_, ScalarType::String) => {
            return Err(ItemError::Unsupported(
                "String のノードには書けません（string 型は #678 で決めます）".into(),
            ))
        }
        (Value::Bit(_), _) => {
            return Err(ItemError::Unsupported(format!(
                "{target} のノードに bit の値は書けません（Boolean のノードを指定してください）"
            )))
        }
        (Value::F64(_), ScalarType::Boolean) => {
            return Err(ItemError::Unsupported(
                "Boolean のノードに数値は書けません（型を bit にしてください）".into(),
            ))
        }
        (Value::F64(x), _) => *x,
        (Value::Str(_), _) => {
            return Err(ItemError::Unsupported(
                "OPC UA のドライバは string 型にまだ対応していません（#678 で決めます）".into(),
            ))
        }
    };
    let refused = || {
        ItemError::Rejected(format!(
            "{x} は {target} で表せないので送りません（小数・範囲外）"
        ))
    };
    let int = |min: i128, max: i128| exact_int(x).filter(|v| (min..=max).contains(v));
    Ok(match target {
        ScalarType::SByte => {
            ScalarValue::SByte(int(i8::MIN.into(), i8::MAX.into()).ok_or_else(refused)? as i8)
        }
        ScalarType::Byte => ScalarValue::Byte(int(0, u8::MAX.into()).ok_or_else(refused)? as u8),
        ScalarType::Int16 => {
            ScalarValue::Int16(int(i16::MIN.into(), i16::MAX.into()).ok_or_else(refused)? as i16)
        }
        ScalarType::UInt16 => {
            ScalarValue::UInt16(int(0, u16::MAX.into()).ok_or_else(refused)? as u16)
        }
        ScalarType::Int32 => {
            ScalarValue::Int32(int(i32::MIN.into(), i32::MAX.into()).ok_or_else(refused)? as i32)
        }
        ScalarType::UInt32 => {
            ScalarValue::UInt32(int(0, u32::MAX.into()).ok_or_else(refused)? as u32)
        }
        ScalarType::Int64 => {
            ScalarValue::Int64(int(i64::MIN.into(), i64::MAX.into()).ok_or_else(refused)? as i64)
        }
        ScalarType::UInt64 => {
            ScalarValue::UInt64(int(0, u64::MAX.into()).ok_or_else(refused)? as u64)
        }
        ScalarType::Float => {
            let narrowed = x as f32;
            if !narrowed.is_finite() {
                return Err(refused());
            }
            ScalarValue::Float(narrowed)
        }
        ScalarType::Double => {
            if !x.is_finite() {
                return Err(refused());
            }
            ScalarValue::Double(x)
        }
        ScalarType::Boolean | ScalarType::String => unreachable!("handled above"),
    })
}

/// The StatusCode a component error message starts with (`"{status}: ..."`,
/// the component's `describe_failure`), by name or as `StatusCode(0x...)`.
fn leading_status(message: &str) -> Option<u32> {
    let head = message.split(':').next()?.trim();
    if let Some(hex) = head
        .strip_prefix("StatusCode(0x")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        return u32::from_str_radix(hex, 16).ok();
    }
    [
        StatusCode::BAD_UNKNOWN_RESPONSE,
        StatusCode::BAD_CONNECTION_CLOSED,
        StatusCode::BAD_COMMUNICATION_ERROR,
        StatusCode::BAD_INTERNAL_ERROR,
        StatusCode::BAD_TOO_MANY_OPERATIONS,
        StatusCode::BAD,
    ]
    .into_iter()
    .find(|status| status.to_string() == head)
    .map(StatusCode::code)
}

/// A connection-wide failure from a Read (or from the Read that learns a
/// node's type before a Write) as a [`FatalError`].
///
/// The component folds the service error into its message, so the class is
/// read back from the leading StatusCode: `BadTimeout` is
/// [`FatalError::Timeout`], a broken binary protocol is
/// [`FatalError::Protocol`], everything else (a closed or lost connection)
/// is [`FatalError::Io`].
pub(crate) fn fatal_of(error: &Error) -> FatalError {
    let text = error.to_string();
    match error {
        Error::Disconnected => FatalError::Io(text),
        Error::Read(message) | Error::Write(message) => {
            if message.starts_with("応答の結果の数が要求と合いません") {
                return FatalError::Protocol(text);
            }
            match leading_status(message) {
                Some(BAD_TIMEOUT) => FatalError::Timeout,
                Some(code) if PROTOCOL_CODES.contains(&code) => FatalError::Protocol(text),
                _ => FatalError::Io(text),
            }
        }
        _ => FatalError::Io(text),
    }
}

/// A failed connect as a [`FatalError`]: settings the server cannot serve
/// (no None / Anonymous endpoint, a refused user, the component refusing the
/// configuration) are [`FatalError::InvalidConfig`]; the rest is
/// [`FatalError::Io`] (the host backs off and retries either way).
pub(crate) fn fatal_of_connect(error: &Error) -> FatalError {
    let text = error.to_string();
    match error {
        Error::Build(_)
        | Error::EndpointNotFound { .. }
        | Error::UserRejected(_)
        | Error::ServerCertificateUntrusted { .. }
        | Error::ServerCertificateInvalid { .. }
        | Error::ClientCertificateRejected(_) => FatalError::InvalidConfig(text),
        _ => FatalError::Io(text),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn n(data_type: DataType) -> TagKind {
        TagKind::numeric(data_type)
    }

    fn node(value: Option<ScalarValue>, status: StatusCode) -> NodeReading {
        NodeReading::new(value, status, None)
    }

    fn unsupported(r: Result<Value, ItemError>) -> bool {
        matches!(r, Err(ItemError::Unsupported(_)))
    }

    #[test]
    fn status_is_mapped_by_severity_not_by_equality() {
        let k = n(DataType::F64);
        let v = Some(ScalarValue::Double(1.5));
        // Good with info bits (e.g. an overflow bit) is still Good.
        let good_with_bits = StatusCode::from_code(0x0000_0480);
        let r = reading_of(&node(v.clone(), good_with_bits), &k).unwrap();
        assert_eq!(r.quality, ReadQuality::Good);
        assert_eq!(r.value, Value::F64(1.5));
        let r = reading_of(
            &node(v.clone(), StatusCode::UNCERTAIN_LAST_USABLE_VALUE),
            &k,
        )
        .unwrap();
        assert_eq!(r.quality, ReadQuality::Uncertain);
        // Another Uncertain code (UncertainSensorNotAccurate 0x408D0000).
        let r = reading_of(&node(v.clone(), StatusCode::from_code(0x408D_0000)), &k).unwrap();
        assert_eq!(r.quality, ReadQuality::Uncertain);
        assert_eq!(
            reading_of(&node(None, StatusCode::BAD_NODE_ID_UNKNOWN), &k),
            Err(ItemError::Device(DeviceError::new(
                0x8034_0000,
                "BadNodeIdUnknown"
            )))
        );
        // Bad with a value still has no usable value.
        assert!(matches!(
            reading_of(&node(v.clone(), StatusCode::BAD), &k),
            Err(ItemError::Device(e)) if e.code == 0x8000_0000
        ));
        // The reserved severity (11) is not Good either.
        assert!(matches!(
            reading_of(&node(v, StatusCode::from_code(0xC000_0000)), &k),
            Err(ItemError::Device(_))
        ));
        // Uncertain or Good without a value.
        assert!(matches!(
            reading_of(&node(None, StatusCode::UNCERTAIN_LAST_USABLE_VALUE), &k),
            Err(ItemError::Device(e)) if e.code == 0x4090_0000
        ));
        assert!(matches!(
            reading_of(&node(None, StatusCode::GOOD), &k),
            Err(ItemError::Device(_))
        ));
        assert!(matches!(
            reading_of(&node(None, StatusCode::BAD_NOT_SUPPORTED), &k),
            Err(ItemError::Unsupported(_))
        ));
    }

    #[test]
    fn source_timestamp_is_kept_and_never_invented() {
        let k = n(DataType::F64);
        let at = UNIX_EPOCH + Duration::from_millis(1_700_000_000_123);
        let r = reading_of(
            &NodeReading::new(Some(ScalarValue::Double(1.0)), StatusCode::GOOD, Some(at)),
            &k,
        )
        .unwrap();
        assert_eq!(r.source_time_ms, Some(1_700_000_000_123));
        let r = reading_of(&node(Some(ScalarValue::Double(1.0)), StatusCode::GOOD), &k).unwrap();
        assert_eq!(r.source_time_ms, None);
        assert_eq!(unix_ms(UNIX_EPOCH - Duration::from_millis(5)), Some(-5));
    }

    #[test]
    fn booleans_are_bits_and_nothing_else_is() {
        assert_eq!(
            value_of(&ScalarValue::Boolean(true), &n(DataType::Bit)),
            Ok(Value::Bit(true))
        );
        assert!(unsupported(value_of(
            &ScalarValue::Int16(1),
            &n(DataType::Bit)
        )));
        assert!(unsupported(value_of(
            &ScalarValue::Double(0.0),
            &n(DataType::Bit)
        )));
        assert!(unsupported(value_of(
            &ScalarValue::Boolean(true),
            &n(DataType::U16)
        )));
        assert!(unsupported(value_of(
            &ScalarValue::Boolean(true),
            &n(DataType::F64)
        )));
        assert!(unsupported(value_of(
            &ScalarValue::String("1".into()),
            &n(DataType::F64)
        )));
        assert!(unsupported(value_of(
            &ScalarValue::String("1".into()),
            &n(DataType::Bit)
        )));
    }

    #[test]
    fn numbers_widen_to_f64() {
        for (value, kind, expected) in [
            (ScalarValue::SByte(-3), DataType::I16, -3.0),
            (ScalarValue::Byte(200), DataType::U16, 200.0),
            (ScalarValue::Int16(-2), DataType::I16, -2.0),
            (ScalarValue::UInt16(65535), DataType::U16, 65535.0),
            (ScalarValue::Int32(-100_000), DataType::I32, -100_000.0),
            (ScalarValue::UInt32(4_000_000_000), DataType::U32, 4e9),
            (ScalarValue::Int64(-5), DataType::I64, -5.0),
            (ScalarValue::UInt64(7), DataType::U64, 7.0),
            (ScalarValue::Double(12.0), DataType::I16, 12.0),
            (ScalarValue::Float(2.5), DataType::F64, 2.5),
            (ScalarValue::Double(0.1), DataType::F64, 0.1),
            (ScalarValue::Int32(3), DataType::F64, 3.0),
            (ScalarValue::Int16(3), DataType::F32, 3.0),
        ] {
            assert_eq!(
                value_of(&value, &n(kind)),
                Ok(Value::F64(expected)),
                "{value:?} as {kind}"
            );
        }
        // f32 rounds to f32 precision (two kinds, two values at one node).
        assert_eq!(
            value_of(&ScalarValue::Double(0.1), &n(DataType::F32)),
            Ok(Value::F64(f64::from(0.1_f32)))
        );
        // f32 / f64 keep NaN and infinity (that is the device's value).
        assert!(matches!(
            value_of(&ScalarValue::Double(f64::NAN), &n(DataType::F64)),
            Ok(Value::F64(x)) if x.is_nan()
        ));
        assert_eq!(
            value_of(&ScalarValue::Float(f32::INFINITY), &n(DataType::F32)),
            Ok(Value::F64(f64::INFINITY))
        );
    }

    #[test]
    fn integer_kinds_refuse_fractions_out_of_range_and_nan() {
        for (value, kind) in [
            (ScalarValue::Double(1.5), DataType::I16),
            (ScalarValue::Double(-0.25), DataType::U32),
            (ScalarValue::Double(f64::NAN), DataType::I32),
            (ScalarValue::Double(f64::INFINITY), DataType::I64),
            (ScalarValue::Double(32768.0), DataType::I16),
            (ScalarValue::Double(-1.0), DataType::U16),
            (ScalarValue::Int32(70_000), DataType::I16),
            (ScalarValue::Int32(-1), DataType::U32),
            (ScalarValue::UInt64(u64::MAX), DataType::I64),
            (ScalarValue::Int64(-1), DataType::U64),
            (
                ScalarValue::Double(9_223_372_036_854_775_808.0),
                DataType::I64,
            ),
            (ScalarValue::Double(1e300), DataType::U64),
            (ScalarValue::Float(1e10), DataType::I32),
        ] {
            assert!(
                unsupported(value_of(&value, &n(kind))),
                "{value:?} as {kind}"
            );
        }
        // The exact edges are accepted.
        assert_eq!(
            value_of(&ScalarValue::Int64(i64::MIN), &n(DataType::I64)),
            Ok(Value::F64(i64::MIN as f64))
        );
        assert_eq!(
            value_of(&ScalarValue::UInt64(u64::MAX), &n(DataType::U64)),
            Ok(Value::F64(u64::MAX as f64))
        );
        assert_eq!(
            value_of(&ScalarValue::Double(-32768.0), &n(DataType::I16)),
            Ok(Value::F64(-32768.0))
        );
        // f32 refuses a finite overflow.
        assert!(unsupported(value_of(
            &ScalarValue::Double(1e300),
            &n(DataType::F32)
        )));
    }

    #[test]
    fn writes_go_out_as_the_nodes_type() {
        use ScalarType::*;
        let f = |x: f64| Value::F64(x);
        for (value, target, expected) in [
            (Value::Bit(true), Boolean, ScalarValue::Boolean(true)),
            (f(-3.0), SByte, ScalarValue::SByte(-3)),
            (f(255.0), Byte, ScalarValue::Byte(255)),
            (f(-2.0), Int16, ScalarValue::Int16(-2)),
            (f(65535.0), UInt16, ScalarValue::UInt16(65535)),
            (f(-100_000.0), Int32, ScalarValue::Int32(-100_000)),
            (f(4e9), UInt32, ScalarValue::UInt32(4_000_000_000)),
            (f(-5.0), Int64, ScalarValue::Int64(-5)),
            (f(7.0), UInt64, ScalarValue::UInt64(7)),
            (f(2.25), Float, ScalarValue::Float(2.25)),
            (f(0.1), Double, ScalarValue::Double(0.1)),
        ] {
            assert_eq!(
                scalar_for_write(&value, target),
                Ok(expected),
                "{value:?} to {target}"
            );
        }
        for (value, target) in [
            (f(1.5), Int16),
            (f(32768.0), Int16),
            (f(-1.0), UInt32),
            (f(f64::NAN), Int32),
            (f(f64::NAN), Double),
            (f(f64::INFINITY), Double),
            (f(1e300), Float),
            (f(256.0), Byte),
        ] {
            assert!(
                matches!(
                    scalar_for_write(&value, target),
                    Err(ItemError::Rejected(_))
                ),
                "{value:?} to {target}"
            );
        }
        for (value, target) in [
            (Value::Bit(true), Int16),
            (Value::Bit(true), Double),
            (f(1.0), Boolean),
            (f(1.0), String),
            (Value::Bit(true), String),
            (Value::Str("x".into()), Double),
        ] {
            assert!(
                matches!(
                    scalar_for_write(&value, target),
                    Err(ItemError::Unsupported(_))
                ),
                "{value:?} to {target}"
            );
        }
    }

    #[test]
    fn kind_types_match_the_publish_types() {
        assert_eq!(scalar_type_for(DataType::Bit), ScalarType::Boolean);
        assert_eq!(scalar_type_for(DataType::U64), ScalarType::UInt64);
        assert_eq!(scalar_type_for(DataType::F32), ScalarType::Float);
    }

    #[test]
    fn errors_are_classified_from_the_leading_status() {
        assert_eq!(
            fatal_of(&Error::Read("StatusCode(0x800A0000): BadTimeout".into())),
            FatalError::Timeout
        );
        assert!(matches!(
            fatal_of(&Error::Read(
                "StatusCode(0x80070000): BadDecodingError".into()
            )),
            FatalError::Protocol(_)
        ));
        assert!(matches!(
            fatal_of(&Error::Read("BadUnknownResponse: x".into())),
            FatalError::Protocol(_)
        ));
        assert!(matches!(
            fatal_of(&Error::Read(
                "応答の結果の数が要求と合いません（要求 2 件、結果 1 件）".into()
            )),
            FatalError::Protocol(_)
        ));
        assert!(matches!(
            fatal_of(&Error::Read("BadConnectionClosed: closed".into())),
            FatalError::Io(_)
        ));
        assert!(matches!(fatal_of(&Error::Disconnected), FatalError::Io(_)));
        assert!(matches!(
            fatal_of(&Error::Read("something else".into())),
            FatalError::Io(_)
        ));
        assert!(matches!(
            fatal_of_connect(&Error::Build("x".into())),
            FatalError::InvalidConfig(_)
        ));
        assert!(matches!(
            fatal_of_connect(&Error::Connect("refused".into())),
            FatalError::Io(_)
        ));
    }
}
