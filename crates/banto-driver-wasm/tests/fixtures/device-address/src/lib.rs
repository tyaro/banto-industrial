//! Test-only component with a grammar no built-in driver accepts: `D:<decimal>`.
//! It uses an FC3 wire exchange so the local simulator tests the real broker path.

use banto::driver::types::{FatalError, ItemError, TagSpec, Value};
use exports::banto::driver::{address, config, read, session};
use std::cell::Cell;

wit_bindgen::generate!({ path: "../../../wit", world: "driver" });

struct Component;
struct Connection {
    next_transaction: Cell<u16>,
}

fn setting<'a>(settings: &'a [config::Setting], key: &str) -> Option<&'a str> {
    settings
        .iter()
        .find(|s| s.key == key)
        .map(|s| s.value.as_str())
}

fn check_settings(settings: &[config::Setting]) -> Vec<config::FieldError> {
    let mut errors = Vec::new();
    if setting(settings, "host").is_none_or(|s| s.trim().is_empty()) {
        errors.push(config::FieldError {
            key: "host".into(),
            message: "host required".into(),
        });
    }
    if !setting(settings, "port").is_some_and(|s| s.parse::<u16>().is_ok_and(|n| n > 0)) {
        errors.push(config::FieldError {
            key: "port".into(),
            message: "port required".into(),
        });
    }
    errors
}

fn parse_device(text: &str, data_type: &str, words: Option<u32>) -> Result<u16, String> {
    if data_type != "u16" || words.is_some() {
        return Err("only u16 is supported".into());
    }
    let digits = text.strip_prefix("D:").ok_or("expected D:<decimal>")?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err("expected D:<decimal>".into());
    }
    digits
        .parse::<u16>()
        .map_err(|_| "device index out of range".into())
}

fn request(transaction: u16, index: u16) -> Vec<u8> {
    let [t0, t1] = transaction.to_be_bytes();
    let [a0, a1] = index.to_be_bytes();
    vec![t0, t1, 0, 0, 0, 6, 1, 3, a0, a1, 0, 1]
}

fn step(send: Vec<u8>) -> session::Step {
    session::Step {
        send,
        expect: session::FrameRule::LengthPrefixed(session::PrefixSpec {
            offset: 4,
            width: 2,
            big_endian: true,
            adjust: 6,
            encoding: session::PrefixEncoding::Binary,
        }),
        max_len: 260,
    }
}

impl session::GuestConnection for Connection {
    fn create(settings: Vec<session::Setting>) -> Result<session::Connection, FatalError> {
        if let Some(error) = check_settings(&settings).first() {
            return Err(FatalError::InvalidConfig(error.message.clone()));
        }
        Ok(session::Connection::new(Self {
            next_transaction: Cell::new(0),
        }))
    }
    fn handshake_next(
        &self,
        _previous: Option<Vec<u8>>,
    ) -> Result<Option<session::Step>, FatalError> {
        Ok(None)
    }
    fn farewell(&self) -> Option<session::Step> {
        None
    }
    fn is_complete(
        &self,
        _step: session::Step,
        _buf: Vec<u8>,
    ) -> Result<session::FrameCheck, FatalError> {
        Err(FatalError::Protocol("plugin-decides is unused".into()))
    }
}
impl session::Guest for Component {
    type Connection = Connection;
}

impl read::Guest for Component {
    fn plan(
        conn: read::ConnectionBorrow<'_>,
        specs: Vec<TagSpec>,
    ) -> Result<Vec<read::Planned>, FatalError> {
        let conn = conn.get::<Connection>();
        let mut planned = Vec::new();
        for (index, spec) in specs.iter().enumerate() {
            let Ok(offset) = parse_device(&spec.address, &spec.data_type, spec.string_words) else {
                continue;
            };
            let transaction = conn.next_transaction.get();
            conn.next_transaction.set(transaction.wrapping_add(1));
            planned.push(read::Planned {
                step: step(request(transaction, offset)),
                covers: vec![index as u32],
            });
        }
        Ok(planned)
    }
    fn decode(
        _conn: read::ConnectionBorrow<'_>,
        p: read::Planned,
        specs: Vec<TagSpec>,
        response: Vec<u8>,
    ) -> Result<Vec<Result<Value, ItemError>>, FatalError> {
        let [index] = p.covers.as_slice() else {
            return Err(FatalError::Protocol("one cover required".into()));
        };
        let spec = specs
            .get(*index as usize)
            .ok_or_else(|| FatalError::Protocol("cover outside specs".into()))?;
        let offset = parse_device(&spec.address, &spec.data_type, spec.string_words)
            .map_err(FatalError::Protocol)?;
        if p.step.send.len() != 12
            || p.step.send != request(u16::from_be_bytes([p.step.send[0], p.step.send[1]]), offset)
        {
            return Err(FatalError::Protocol("request differs from plan".into()));
        }
        if response.len() == 9 && response[7] == 0x83 {
            return Ok(vec![Err(ItemError::Device(
                banto::driver::types::DeviceError {
                    code: u32::from(response[8]),
                    message: "FC3 exception".into(),
                },
            ))]);
        }
        if response.len() != 11
            || response[0..2] != p.step.send[0..2]
            || response[2..9] != [0, 0, 0, 5, 1, 3, 2]
        {
            return Err(FatalError::Framing("invalid FC3 response".into()));
        }
        Ok(vec![Ok(Value::Uint(u64::from(u16::from_be_bytes([
            response[9],
            response[10],
        ]))))])
    }
}

impl address::Guest for Component {
    fn parse(
        text: String,
        data_type: String,
        words: Option<u32>,
    ) -> Result<address::Parsed, String> {
        let index = parse_device(&text, &data_type, words)?;
        Ok(address::Parsed {
            canonical: format!("D:{index}"),
            area: "D".into(),
            writable_area: false,
            bit: None,
            word_count: 1,
            index: Some(u64::from(index)),
        })
    }
    fn offset(
        canonical: String,
        data_type: String,
        words: Option<u32>,
        n: i64,
    ) -> Result<String, String> {
        let index = parse_device(&canonical, &data_type, words)?;
        let next = i64::from(index).checked_add(n).ok_or("offset overflow")?;
        Ok(format!(
            "D:{}",
            u16::try_from(next).map_err(|_| "offset out of range")?
        ))
    }
    fn supported_kinds() -> Vec<String> {
        vec!["u16".into()]
    }
    fn offset_words(canonical: String, n: i64) -> Result<String, String> {
        Self::offset(canonical, "u16".into(), None, n)
    }
    fn help() -> String {
        "Test device address: D:<decimal>, U16".into()
    }
    fn probe() -> Option<TagSpec> {
        Some(TagSpec {
            address: "D:0".into(),
            data_type: "u16".into(),
            string_words: None,
        })
    }
}

impl config::Guest for Component {
    fn validate(settings: Vec<config::Setting>) -> Vec<config::FieldError> {
        check_settings(&settings)
    }
    fn secret_keys() -> Vec<String> {
        Vec::new()
    }
    fn schema() -> Vec<config::SettingField> {
        use config::{FieldGroup, FieldKind, IntegerSpec, SettingField, SettingValue};
        vec![
            SettingField {
                key: "host".into(),
                label: "Host".into(),
                help: None,
                kind: FieldKind::Host,
                required: true,
                default: None,
                group: FieldGroup::Basic,
            },
            SettingField {
                key: "port".into(),
                label: "Port".into(),
                help: None,
                kind: FieldKind::Integer(IntegerSpec {
                    min: Some(1),
                    max: Some(65_535),
                }),
                required: true,
                default: Some(SettingValue::Integer(502)),
                group: FieldGroup::Basic,
            },
        ]
    }
}

#[cfg(target_arch = "wasm32")]
export!(Component);
