//! Component-level check of the SLMP guest's word-bit read-modify-write
//! (banto-driver-wasm #61, stage F) through this crate's write gate (#906).
//!
//! Run with `BANTO_SLMP_NEW_COMPONENT_WASM=<built guest> cargo test -- --ignored`.
//! The responses are scripted here, so the frame sequence (0401 read, 1401
//! masked write-back, 0401 confirm) and each item's result are checked without
//! a socket.

use banto_driver_api::{
    DataType, DriverBase, FrameConnection, FrameDriver, ItemError, Settings, TagKind, TagSpec,
    Value, WriteItem, WriteProgress,
};
use std::sync::Arc;

use banto_driver_wasm::WasmDriver;

const MANIFEST: &[u8] = br#"{"manifestVersion":1,"id":"slmp-wasm-operational-write","version":"0.1.0","hubApi":"1","writeApi":"1","settingsVersion":1,"displayName":"SLMP word-bit write fixture","transports":["tcp"],"capabilities":["read","write"]}"#;

/// A 4E response to `request` (serial and route echoed).
fn answer(request: &[u8], end_code: u16, body: &[u8]) -> Vec<u8> {
    assert_eq!(&request[..2], &[0x54, 0x00], "4E request");
    let mut response = vec![0xd4, 0x00, request[2], request[3], 0, 0];
    response.extend_from_slice(&request[6..11]);
    response.extend_from_slice(&u16::try_from(body.len() + 2).unwrap().to_le_bytes());
    response.extend_from_slice(&end_code.to_le_bytes());
    response.extend_from_slice(body);
    response
}

fn command(request: &[u8]) -> (u16, u16) {
    (
        u16::from_le_bytes([request[15], request[16]]),
        u16::from_le_bytes([request[17], request[18]]),
    )
}

/// R-series (subcommand 2) word request: device number, device code, points.
fn r_words(request: &[u8]) -> (u32, u8, u16) {
    (
        u32::from_le_bytes([request[19], request[20], request[21], request[22]]),
        request[23],
        u16::from_le_bytes([request[25], request[26]]),
    )
}

/// Load the guest, connect as an R04CPU with D/W counts of 4000 and the given
/// word-bit allowlist, and finish the handshake.
fn connected(allowlist: &str) -> (Arc<WasmDriver>, Box<dyn FrameConnection>) {
    let component =
        std::fs::read(std::env::var("BANTO_SLMP_NEW_COMPONENT_WASM").expect("guest path")).unwrap();
    let driver = WasmDriver::load_operational_write(MANIFEST, &component).unwrap();
    let settings = Settings::from_pairs([
        ("host", "127.0.0.1"),
        ("port", "5000"),
        ("d_u16_write_mode", "enabled"),
        ("d_u16_write_allowlist", allowlist),
    ])
    .unwrap();
    assert!(driver.validate_config(&settings).is_empty());
    let mut conn = driver.create_connection(&settings).unwrap();
    let mut response = None;
    while let Some(step) = conn.handshake_next(response.as_deref()).unwrap() {
        let request = step.send;
        response = Some(match command(&request) {
            (0x0101, _) => {
                let mut body = vec![b' '; 18];
                body[..6].copy_from_slice(b"R04CPU");
                body[16..18].copy_from_slice(&0x4800u16.to_le_bytes());
                answer(&request, 0, &body)
            }
            (0x0401, 2) => {
                assert_eq!(r_words(&request), (280, 0xa9, 4), "SD280-283 only");
                let mut body = 4000u32.to_le_bytes().to_vec();
                body.extend_from_slice(&4000u32.to_le_bytes());
                answer(&request, 0, &body)
            }
            other => panic!("unexpected handshake request {other:x?}"),
        });
    }
    (driver, conn)
}

fn bit_items(driver: &WasmDriver, bits: &[(&str, bool)]) -> Vec<WriteItem> {
    let kind = TagKind::numeric(DataType::Bit);
    bits.iter()
        .map(|(address, value)| {
            WriteItem::new(
                TagSpec::new(driver.parse_address(address, &kind).unwrap(), kind.clone()),
                Value::Bit(*value),
            )
        })
        .collect()
}

fn send(progress: WriteProgress) -> Vec<u8> {
    match progress {
        WriteProgress::Send(planned) => planned.step.send,
        other => panic!("expected a send step, got {other:?}"),
    }
}

/// Read D3010 (0x0001), write back 0x0021 with bit 0 kept, confirm read.
/// Returns the confirm-read request so the caller chooses its answer.
fn read_and_write_back(conn: &mut dyn FrameConnection, items: &[WriteItem]) -> Vec<u8> {
    let read = send(conn.write_next(items, None).unwrap());
    assert_eq!(command(&read), (0x0401, 2));
    assert_eq!(r_words(&read), (3010, 0xa8, 1));
    let write = send(
        conn.write_next(items, Some(&answer(&read, 0, &0x0001u16.to_le_bytes())))
            .unwrap(),
    );
    assert_eq!(command(&write), (0x1401, 2));
    assert_eq!(r_words(&write), (3010, 0xa8, 1));
    assert_eq!(&write[27..], &0x0021u16.to_le_bytes(), "only bit 5 is set");
    let confirm = send(
        conn.write_next(items, Some(&answer(&write, 0, &[])))
            .unwrap(),
    );
    assert_eq!(command(&confirm), (0x0401, 2));
    assert_eq!(r_words(&confirm), (3010, 0xa8, 1));
    confirm
}

#[test]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #61 component"]
fn word_bit_confirm_mismatch_is_verification_failed_per_item() {
    let (driver, mut conn) = connected("D3010.0-D3010.F");
    let items = bit_items(&driver, &[("D3010.5", true), ("D3010.0", true)]);

    // Matching confirm read: both bits succeed.
    let confirm = read_and_write_back(conn.as_mut(), &items);
    let done = conn
        .write_next(&items, Some(&answer(&confirm, 0, &0x0021u16.to_le_bytes())))
        .unwrap();
    assert_eq!(done, WriteProgress::Done(vec![Ok(()), Ok(())]));

    // Bit 5 reads back clear: only that item is verification-failed.
    let confirm = read_and_write_back(conn.as_mut(), &items);
    match conn
        .write_next(&items, Some(&answer(&confirm, 0, &0x0001u16.to_le_bytes())))
        .unwrap()
    {
        WriteProgress::Done(results) => {
            assert!(
                matches!(
                    results.as_slice(),
                    [Err(ItemError::VerificationFailed(_)), Ok(())]
                ),
                "{results:?}"
            );
        }
        other => panic!("expected done, got {other:?}"),
    }

    // A refused confirm read leaves the write-back unconfirmed for every item.
    let confirm = read_and_write_back(conn.as_mut(), &items);
    match conn
        .write_next(&items, Some(&answer(&confirm, 0xc051, &[])))
        .unwrap()
    {
        WriteProgress::Done(results) => {
            assert_eq!(results.len(), 2);
            assert!(
                results
                    .iter()
                    .all(|result| matches!(result, Err(ItemError::VerificationFailed(_)))),
                "{results:?}"
            );
        }
        other => panic!("expected done, got {other:?}"),
    }
}
