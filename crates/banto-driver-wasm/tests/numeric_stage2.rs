//! Opt-in Wasmtime/host acceptance for pinned stage-2 guests.
//! Set BANTO_STAGE2_MODBUS_WASM, BANTO_STAGE2_MODBUS_MANIFEST, and
//! BANTO_STAGE2_SLMP_WASM, then run this test target with `--ignored`.
//! Transport responses are generated in memory; no PLC or TCP listener is used.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use banto_driver_api::{
    BoxFuture, DataType, DriverBase, FatalError, FrameDriver, ItemError, RawValue, Settings,
    TagKind, TagSpec, Value, WriteItem, WriteProgress,
};
use banto_driver_engine::transport::Transport;
use banto_driver_engine::FrameSession;
use banto_driver_wasm::WasmDriver;

const SLMP_MANIFEST: &[u8] = br#"{"manifestVersion":1,"id":"slmp-read-wasm","version":"0.1.0","hubApi":"1","settingsVersion":1,"displayName":"SLMP numeric test guest","transports":["tcp"],"capabilities":["read"]}"#;

#[derive(Clone, Copy)]
enum Protocol {
    Modbus,
    SlmpR,
    SlmpQ,
}

struct ScriptedTransport {
    protocol: Protocol,
    words: BTreeMap<u32, u16>,
    sent: Arc<Mutex<Vec<Vec<u8>>>>,
    pending: VecDeque<u8>,
}

impl ScriptedTransport {
    fn new(protocol: Protocol, words: BTreeMap<u32, u16>) -> (Self, Arc<Mutex<Vec<Vec<u8>>>>) {
        let sent = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                protocol,
                words,
                sent: sent.clone(),
                pending: VecDeque::new(),
            },
            sent,
        )
    }

    fn response(&self, request: &[u8]) -> Vec<u8> {
        match self.protocol {
            Protocol::Modbus => {
                assert_eq!(request.len(), 12);
                assert_eq!(request[7], 3);
                let start = u32::from(u16::from_be_bytes([request[8], request[9]]));
                let count = u16::from_be_bytes([request[10], request[11]]);
                let mut response = request[..2].to_vec();
                response.extend_from_slice(&[0, 0]);
                response.extend_from_slice(&(3 + count * 2).to_be_bytes());
                response.extend_from_slice(&[request[6], 3, (count * 2) as u8]);
                for address in start..start + u32::from(count) {
                    response.extend_from_slice(&self.words[&address].to_be_bytes());
                }
                response
            }
            Protocol::SlmpR | Protocol::SlmpQ => {
                assert_eq!(&request[..2], &[0x54, 0]);
                let command = u16::from_le_bytes([request[15], request[16]]);
                let subcommand = u16::from_le_bytes([request[17], request[18]]);
                let body = if command == 0x0101 {
                    let (name, code) = match self.protocol {
                        Protocol::SlmpR => ("R04CPU", 0x4800_u16),
                        Protocol::SlmpQ => ("Q02UCPU", 0x0263_u16),
                        Protocol::Modbus => unreachable!(),
                    };
                    let mut body = vec![b' '; 18];
                    body[..name.len()].copy_from_slice(name.as_bytes());
                    body[16..18].copy_from_slice(&code.to_le_bytes());
                    body
                } else {
                    assert_eq!(command, 0x0401);
                    let is_r = matches!(self.protocol, Protocol::SlmpR);
                    assert_eq!(subcommand, if is_r { 2 } else { 0 });
                    let start = u32::from_le_bytes([
                        request[19],
                        request[20],
                        request[21],
                        if is_r { request[22] } else { 0 },
                    ]);
                    let device = request[if is_r { 23 } else { 22 }];
                    let count_offset = if is_r { 25 } else { 23 };
                    let count =
                        u16::from_le_bytes([request[count_offset], request[count_offset + 1]]);
                    if device == 0xa9 {
                        assert_eq!(start, if is_r { 280 } else { 302 });
                        if is_r {
                            100_u32.to_le_bytes().to_vec()
                        } else {
                            100_u16.to_le_bytes().to_vec()
                        }
                    } else {
                        assert_eq!(device, 0xa8);
                        let mut body = Vec::with_capacity(usize::from(count) * 2);
                        for address in start..start + u32::from(count) {
                            body.extend_from_slice(&self.words[&address].to_le_bytes());
                        }
                        body
                    }
                };
                let mut response = vec![0xd4, 0];
                response.extend_from_slice(&request[2..11]);
                response.extend_from_slice(&((body.len() + 2) as u16).to_le_bytes());
                response.extend_from_slice(&0_u16.to_le_bytes());
                response.extend_from_slice(&body);
                response
            }
        }
    }
}

impl Transport for ScriptedTransport {
    fn send<'a>(&'a mut self, bytes: &'a [u8]) -> BoxFuture<'a, Result<(), FatalError>> {
        Box::pin(async move {
            let response = self.response(bytes);
            self.sent.lock().unwrap().push(bytes.to_vec());
            self.pending.extend(response);
            Ok(())
        })
    }

    fn recv<'a>(&'a mut self, buf: &'a mut Vec<u8>) -> BoxFuture<'a, Result<usize, FatalError>> {
        Box::pin(async move {
            let count = self.pending.len().min(7);
            buf.extend(self.pending.drain(..count));
            Ok(count)
        })
    }

    fn discard_input(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(async move { self.pending.clear() })
    }

    fn close(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(async move { self.pending.clear() })
    }
}

fn load_modbus() -> Arc<WasmDriver> {
    let manifest = std::fs::read(std::env::var("BANTO_STAGE2_MODBUS_MANIFEST").unwrap()).unwrap();
    let component = std::fs::read(std::env::var("BANTO_STAGE2_MODBUS_WASM").unwrap()).unwrap();
    WasmDriver::load(&manifest, &component).unwrap()
}

fn load_slmp() -> Arc<WasmDriver> {
    let component = std::fs::read(std::env::var("BANTO_STAGE2_SLMP_WASM").unwrap()).unwrap();
    WasmDriver::load(SLMP_MANIFEST, &component).unwrap()
}

fn sample_words() -> BTreeMap<u32, u16> {
    // LowHigh words for U16, I16, U32, I32, U64, I64, F32, F64.
    [
        42, 0x8000, 0x1234, 0xabcd, 0, 0x8000, 1, 0, 0, 0x0020, 0, 0, 0, 0x8000, 0, 0x3fc0, 0, 0,
        0, 0x3ff8,
    ]
    .into_iter()
    .enumerate()
    .map(|(index, word)| (index as u32, word))
    .collect()
}

fn kinds() -> [DataType; 8] {
    [
        DataType::U16,
        DataType::I16,
        DataType::U32,
        DataType::I32,
        DataType::U64,
        DataType::I64,
        DataType::F32,
        DataType::F64,
    ]
}

fn expected_values() -> Vec<Result<RawValue, banto_driver_api::ItemError>> {
    vec![
        Ok(RawValue::Uint(42)),
        Ok(RawValue::Int(i16::MIN.into())),
        Ok(RawValue::Uint(0xabcd_1234)),
        Ok(RawValue::Int(i32::MIN.into())),
        Ok(RawValue::Uint((1_u64 << 53) + 1)),
        Ok(RawValue::Int(i64::MIN)),
        Ok(RawValue::Float(1.5)),
        Ok(RawValue::Float(1.5)),
    ]
}

#[tokio::test]
#[ignore = "requires the pinned stage-2 Modbus component and manifest"]
async fn modbus_eight_kinds_use_wasmtime_and_scripted_transport() {
    let driver = load_modbus();
    assert_eq!(driver.supported_kinds().len(), 8);
    let starts = [0, 1, 2, 4, 6, 10, 14, 16];
    let specs: Vec<_> = starts
        .into_iter()
        .zip(kinds())
        .map(|(start, data_type)| {
            let kind = TagKind::numeric(data_type);
            let address = driver
                .parse_address(&format!("{}", 400001_u32 + u32::from(start)), &kind)
                .unwrap();
            assert_eq!(address.word_count, kind.word_count());
            TagSpec::new(address, kind)
        })
        .collect();
    let settings = Settings::from_pairs([
        ("host", "127.0.0.1"),
        ("port", "502"),
        ("word_order", "low_high"),
    ])
    .unwrap();
    let (wire, sent) = ScriptedTransport::new(Protocol::Modbus, sample_words());
    let mut session = FrameSession::with_transport(
        driver.as_ref(),
        &settings,
        Box::new(wire),
        Duration::from_secs(2),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        session.read_raw_batch(&specs).await.unwrap(),
        expected_values()
    );
    let requests = sent.lock().unwrap();
    assert_eq!(requests.len(), 8, "one FC3 request per tag");
    for (index, request) in requests.iter().enumerate() {
        assert_eq!(u16::from_be_bytes([request[8], request[9]]), starts[index]);
        assert_eq!(
            u16::from_be_bytes([request[10], request[11]]),
            specs[index].kind.word_count()
        );
    }
}

#[tokio::test]
#[ignore = "requires the pinned stage-2 SLMP component"]
async fn slmp_known_batch_and_unknown_range_keep_tag_covers() {
    let driver = load_slmp();
    assert_eq!(driver.supported_kinds().len(), 8);
    let starts = [0, 1, 2, 4, 6, 10, 14, 16];
    let specs: Vec<_> = starts
        .into_iter()
        .zip(kinds())
        .map(|(start, data_type)| {
            let kind = TagKind::numeric(data_type);
            let address = driver.parse_address(&format!("D{start}"), &kind).unwrap();
            assert_eq!(address.word_count, kind.word_count());
            if data_type != DataType::U16 {
                assert!(!address.writable_area);
            }
            TagSpec::new(address, kind)
        })
        .collect();
    let settings = Settings::from_pairs([
        ("host", "127.0.0.1"),
        ("port", "5000"),
        ("word_order", "low_high"),
    ])
    .unwrap();
    let (wire, sent) = ScriptedTransport::new(Protocol::SlmpR, sample_words());
    let mut session = FrameSession::with_transport(
        driver.as_ref(),
        &settings,
        Box::new(wire),
        Duration::from_secs(2),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        session.read_raw_batch(&specs).await.unwrap(),
        expected_values()
    );
    {
        let requests = sent.lock().unwrap();
        assert_eq!(requests.len(), 3, "0101, SD, one 20-word D batch");
        assert_eq!(&requests[2][15..17], &0x0401_u16.to_le_bytes());
        assert_eq!(u16::from_le_bytes([requests[2][25], requests[2][26]]), 20);
    }
    let kind = TagKind::numeric(DataType::F64);
    let beyond = TagSpec::new(driver.parse_address("D97", &kind).unwrap(), kind);
    assert!(matches!(
        session.read_raw_batch(&[beyond]).await.unwrap().as_slice(),
        [Err(ItemError::Unsupported(_))]
    ));
    assert_eq!(
        sent.lock().unwrap().len(),
        3,
        "whole-tag range refusal sends no D read"
    );

    let mut words = sample_words();
    words.extend([(100, 1), (101, 0), (102, 0), (103, 0x0020)]);
    let (wire, sent) = ScriptedTransport::new(Protocol::SlmpQ, words);
    let mut session = FrameSession::with_transport(
        driver.as_ref(),
        &settings,
        Box::new(wire),
        Duration::from_secs(2),
        None,
    )
    .await
    .unwrap();
    let kind = TagKind::numeric(DataType::U64);
    let unknown = TagSpec::new(driver.parse_address("D100", &kind).unwrap(), kind);
    let result = session
        .read_raw_batch(&[specs[0].clone(), unknown])
        .await
        .unwrap();
    assert_eq!(
        result,
        [
            Ok(RawValue::Uint(42)),
            Ok(RawValue::Uint((1_u64 << 53) + 1))
        ]
    );
    let requests = sent.lock().unwrap();
    assert_eq!(requests.len(), 4, "Unknown D100 must be a separate read");
    assert_eq!(u16::from_le_bytes([requests[2][23], requests[2][24]]), 1);
    assert_eq!(u16::from_le_bytes([requests[3][23], requests[3][24]]), 4);
}

#[test]
#[ignore = "requires the pinned stage-2 Modbus and SLMP components"]
fn address_endpoints_and_read_only_capability_are_preserved() {
    let modbus = load_modbus();
    let slmp = load_slmp();
    assert!(!modbus.descriptor().capabilities.write);
    assert!(!slmp.descriptor().capabilities.write);
    for (kind, last, beyond) in [
        (DataType::I16, "465536", "465537"),
        (DataType::I32, "465535", "465536"),
        (DataType::F32, "465535", "465536"),
        (DataType::I64, "465533", "465534"),
        (DataType::U64, "465533", "465534"),
        (DataType::F64, "465533", "465534"),
    ] {
        let kind = TagKind::numeric(kind);
        assert!(modbus.parse_address(last, &kind).is_ok(), "{last} {kind:?}");
        assert!(modbus.parse_address(beyond, &kind).is_err());
    }
    let f64_kind = TagKind::numeric(DataType::F64);
    assert!(slmp.parse_address("D4294967292", &f64_kind).is_ok());
    assert!(slmp.parse_address("D4294967293", &f64_kind).is_err());
    assert!(slmp.parse_address("M0", &f64_kind).is_err());
    assert!(
        slmp.parse_address("D3010", &TagKind::numeric(DataType::U16))
            .unwrap()
            .writable_area
    );
    assert!(
        !slmp
            .parse_address("D3010", &f64_kind)
            .unwrap()
            .writable_area
    );
    let settings = Settings::from_pairs([("host", "127.0.0.1"), ("port", "5000")]).unwrap();
    let mut connection = slmp.create_connection(&settings).unwrap();
    let write = WriteItem::new(
        TagSpec::new(
            slmp.parse_address("D3010", &TagKind::numeric(DataType::U16))
                .unwrap(),
            TagKind::numeric(DataType::U16),
        ),
        Value::F64(1.0),
    );
    assert!(matches!(
        connection.write_next(&[write], None),
        Ok(WriteProgress::Done(results)) if matches!(results.as_slice(), [Err(ItemError::Unsupported(_))])
    ));
}

#[tokio::test]
#[ignore = "requires the pinned stage-2 Modbus and SLMP components"]
async fn high_low_float_nan_and_infinity_traverse_both_host_adapters() {
    for protocol in [Protocol::Modbus, Protocol::SlmpR] {
        let driver = if matches!(protocol, Protocol::Modbus) {
            load_modbus()
        } else {
            load_slmp()
        };
        let words: BTreeMap<_, _> = [0x1234, 0xabcd, 0x7fc0, 0, 0x7ff0, 0, 0, 0]
            .into_iter()
            .enumerate()
            .map(|(index, word)| (index as u32, word))
            .collect();
        let specs: Vec<_> = [(0, DataType::U32), (2, DataType::F32), (4, DataType::F64)]
            .into_iter()
            .map(|(start, data_type)| {
                let kind = TagKind::numeric(data_type);
                let text = if matches!(protocol, Protocol::Modbus) {
                    format!("{}", 400001 + start)
                } else {
                    format!("D{start}")
                };
                TagSpec::new(driver.parse_address(&text, &kind).unwrap(), kind)
            })
            .collect();
        let settings = Settings::from_pairs([
            ("host", "127.0.0.1"),
            (
                "port",
                if matches!(protocol, Protocol::Modbus) {
                    "502"
                } else {
                    "5000"
                },
            ),
            ("word_order", "high_low"),
        ])
        .unwrap();
        let (wire, sent) = ScriptedTransport::new(protocol, words);
        let mut session = FrameSession::with_transport(
            driver.as_ref(),
            &settings,
            Box::new(wire),
            Duration::from_secs(2),
            None,
        )
        .await
        .unwrap();
        let result = session.read_raw_batch(&specs).await.unwrap();
        assert_eq!(result[0], Ok(RawValue::Uint(0x1234_abcd)));
        assert!(matches!(&result[1], Ok(RawValue::Float(value)) if value.is_nan()));
        assert_eq!(result[2], Ok(RawValue::Float(f64::INFINITY)));
        assert_eq!(sent.lock().unwrap().len(), 3);
    }
}

#[test]
#[ignore = "requires the pinned stage-2 Modbus component"]
fn short_modbus_response_is_rejected_by_actual_component() {
    let driver = load_modbus();
    let settings = Settings::from_pairs([("host", "127.0.0.1"), ("port", "502")]).unwrap();
    let mut connection = driver.create_connection(&settings).unwrap();
    let kind = TagKind::numeric(DataType::F64);
    let spec = TagSpec::new(driver.parse_address("400017", &kind).unwrap(), kind);
    let specs = [spec];
    let planned = connection.plan_read(&specs).unwrap();
    let (wire, _) = ScriptedTransport::new(Protocol::Modbus, sample_words());
    let mut response = wire.response(&planned[0].step.send);
    response.pop();
    assert!(matches!(
        connection.decode_read(&planned[0], &specs, &response),
        Err(FatalError::Framing(_) | FatalError::Protocol(_))
    ));
}

#[tokio::test]
#[ignore = "requires the pinned stage-2 SLMP component"]
async fn short_slmp_response_is_rejected_by_actual_component() {
    let driver = load_slmp();
    let settings = Settings::from_pairs([("host", "127.0.0.1"), ("port", "5000")]).unwrap();
    let (wire, _) = ScriptedTransport::new(Protocol::SlmpR, sample_words());
    let mut session = FrameSession::with_transport(
        driver.as_ref(),
        &settings,
        Box::new(wire),
        Duration::from_secs(2),
        None,
    )
    .await
    .unwrap();
    let kind = TagKind::numeric(DataType::F64);
    let specs = [TagSpec::new(
        driver.parse_address("D16", &kind).unwrap(),
        kind,
    )];
    let connection = session.connection_mut().unwrap();
    let planned = connection.plan_read(&specs).unwrap();
    let (wire, _) = ScriptedTransport::new(Protocol::SlmpR, sample_words());
    let mut response = wire.response(&planned[0].step.send);
    response.pop();
    assert!(matches!(
        connection.decode_read(&planned[0], &specs, &response),
        Err(FatalError::Framing(_) | FatalError::Protocol(_))
    ));
}
