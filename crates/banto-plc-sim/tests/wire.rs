//! 結合の試験: 実の TCP で模擬 PLC を立て、生のフレームで読んだ値が筋書きの評価
//! （× `scale`）と一致すること、`faults` の間は答えず外へ出たら読めることを確かめる
//! （設計書「試験」の節）。クライアントは `banto-plc` に残っていないので、フレームは
//! シミュレータの試験と同じ形を手で組む。

use std::path::{Path, PathBuf};
use std::time::Duration;

use banto_plc::modbus::frame::build_request_frame;
use banto_plc::SlmpDevice;
use banto_plc_sim::{load_plan, parse_plan, Plan, Runtime};
use banto_register_codec as codec;
use banto_scenario::ScenarioValue;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

fn examples_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("examples")
}

fn loopback() -> Option<std::net::SocketAddr> {
    Some("127.0.0.1:0".parse().unwrap())
}

fn number(plan: &Plan, t: f64, signal: &str) -> f64 {
    match plan.scenario.evaluate(t)[signal] {
        ScenarioValue::Number(n) => n,
        ScenarioValue::Bit(_) => panic!("{signal} は数値でない"),
    }
}

fn bit(plan: &Plan, t: f64, signal: &str) -> bool {
    match plan.scenario.evaluate(t)[signal] {
        ScenarioValue::Bit(b) => b,
        ScenarioValue::Number(_) => panic!("{signal} はビットでない"),
    }
}

const SHORT: Duration = Duration::from_millis(300);

// ---- SLMP ----

async fn slmp_read(
    stream: &mut TcpStream,
    device: SlmpDevice,
    start: u32,
    count: u16,
    bit_unit: bool,
) -> std::io::Result<Vec<u8>> {
    let mut command = Vec::new();
    command.extend_from_slice(&0x0401u16.to_le_bytes());
    command.extend_from_slice(&(if bit_unit { 1u16 } else { 0 }).to_le_bytes());
    command.extend_from_slice(&start.to_le_bytes()[..3]);
    command.push(device.to_wire().to_code());
    command.extend_from_slice(&count.to_le_bytes());

    let mut frame = vec![0x54, 0x00];
    frame.extend_from_slice(&1u16.to_le_bytes());
    frame.extend_from_slice(&[0x00, 0x00, 0x00, 0xFF]);
    frame.extend_from_slice(&0x03FFu16.to_le_bytes());
    frame.push(0x00);
    frame.extend_from_slice(&((command.len() + 2) as u16).to_le_bytes());
    frame.extend_from_slice(&0x0010u16.to_le_bytes());
    frame.extend_from_slice(&command);
    stream.write_all(&frame).await?;

    let mut prefix = [0u8; 15];
    stream.read_exact(&mut prefix).await?;
    let end_code = u16::from_le_bytes([prefix[13], prefix[14]]);
    assert_eq!(end_code, 0, "SLMP の終了コード");
    let declared = u16::from_le_bytes([prefix[11], prefix[12]]) as usize;
    let mut payload = vec![0u8; declared.saturating_sub(2)];
    stream.read_exact(&mut payload).await?;
    Ok(payload)
}

async fn slmp_words(
    stream: &mut TcpStream,
    device: SlmpDevice,
    start: u32,
    count: u16,
) -> std::io::Result<Vec<u16>> {
    let payload = slmp_read(stream, device, start, count, false).await?;
    Ok(payload
        .chunks(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect())
}

async fn slmp_bit(
    stream: &mut TcpStream,
    device: SlmpDevice,
    number: u32,
) -> std::io::Result<bool> {
    let payload = slmp_read(stream, device, number, 1, true).await?;
    Ok(payload[0] & 0x10 != 0)
}

#[tokio::test]
async fn slmp_values_match_the_scenario_at_a_frozen_time() {
    let plan = load_plan(&examples_dir().join("plant.sim.json")).unwrap();
    let t = plan.scenario.frozen_time_sec(None).unwrap();
    let mut runtime = Runtime::start(plan.clone(), loopback()).await.unwrap();
    assert!(runtime.tick(t).is_empty());
    let mut stream = TcpStream::connect(runtime.addr()).await.unwrap();

    // Tank.Level: i16、scale 10。
    let level = slmp_words(&mut stream, SlmpDevice::D, 100, 1)
        .await
        .unwrap();
    assert_eq!(
        codec::decode_i16(level[0]),
        (number(&plan, t, "Tank.Level") * 10.0).round() as i16
    );
    // Inflow: f32、既定の low_high。
    let inflow = slmp_words(&mut stream, SlmpDevice::D, 102, 2)
        .await
        .unwrap();
    assert_eq!(
        codec::decode_f32([inflow[0], inflow[1]], codec::WordOrder::LowHigh),
        number(&plan, t, "Inflow") as f32
    );
    // Hours: u32、割付で high_low。
    let hours = slmp_words(&mut stream, SlmpDevice::D, 110, 2)
        .await
        .unwrap();
    assert_eq!(
        codec::decode_u32([hours[0], hours[1]], codec::WordOrder::HighLow),
        number(&plan, t, "Hours").round() as u32
    );
    // Pump.Run: M10。
    assert_eq!(
        slmp_bit(&mut stream, SlmpDevice::M, 10).await.unwrap(),
        bit(&plan, t, "Pump.Run")
    );
    // Lamp: D120 のビット 3（ほかのビットは 0）。
    let word = slmp_words(&mut stream, SlmpDevice::D, 120, 1)
        .await
        .unwrap()[0];
    assert_eq!(word, u16::from(bit(&plan, t, "Lamp")) << 3);
    runtime.stop();
}

#[tokio::test]
async fn slmp_does_not_answer_inside_a_fault_and_recovers_after() {
    let plan = load_plan(&examples_dir().join("plant.sim.json")).unwrap();
    let epoch = plan.scenario.epoch_sec;
    let mut runtime = Runtime::start(plan, loopback()).await.unwrap();

    runtime.tick(epoch + 70.0);
    assert!(!runtime.is_hanging());
    let mut stream = TcpStream::connect(runtime.addr()).await.unwrap();
    assert!(slmp_words(&mut stream, SlmpDevice::D, 100, 1).await.is_ok());

    // faults: 100〜115 秒。
    runtime.tick(epoch + 105.0);
    assert!(runtime.is_hanging());
    let mut stuck = TcpStream::connect(runtime.addr()).await.unwrap();
    assert!(
        timeout(SHORT, slmp_words(&mut stuck, SlmpDevice::D, 100, 1))
            .await
            .is_err(),
        "途切れの間は答えない"
    );

    // 出たら答える（新しい接続で）。
    runtime.tick(epoch + 120.0);
    assert!(!runtime.is_hanging());
    let mut again = TcpStream::connect(runtime.addr()).await.unwrap();
    assert!(
        timeout(SHORT, slmp_words(&mut again, SlmpDevice::D, 100, 1))
            .await
            .expect("答える")
            .is_ok()
    );
    runtime.stop();
}

// ---- Modbus ----

const MODBUS_CONFIG: &str = r#"{
    "format": "banto-plc-sim", "version": 1, "protocol": "modbus",
    "scenario": "plant.scenario.json",
    "map": [
        { "signal": "Tank.Level", "address": "40001", "type": "i16", "scale": 10 },
        { "signal": "Inflow", "address": "40003", "type": "f32", "wordOrder": "high_low" },
        { "signal": "Hours", "address": "40010", "type": "u32" },
        { "signal": "Pump.Run", "address": "00001" },
        { "signal": "Lamp", "address": "40020.3" },
        { "signal": "Lamp", "address": "10001" },
        { "signal": "Tank.Level", "address": "30001", "type": "u16" },
        { "signal": "Lamp", "address": "30005.2" }
    ],
    "faults": [{ "kind": "hang", "atSec": 100, "durationSec": 15 }]
}"#;

async fn modbus_read(
    stream: &mut TcpStream,
    function: u8,
    offset: u16,
    count: u16,
) -> std::io::Result<Vec<u8>> {
    stream
        .write_all(&build_request_frame(1, 1, function, offset, count))
        .await?;
    let mut header = [0u8; 7];
    stream.read_exact(&mut header).await?;
    let length = u16::from_be_bytes([header[4], header[5]]) as usize;
    let mut pdu = vec![0u8; length - 1];
    stream.read_exact(&mut pdu).await?;
    assert_eq!(pdu[0], function, "例外応答でない");
    Ok(pdu[2..].to_vec())
}

async fn modbus_registers(
    stream: &mut TcpStream,
    function: u8,
    offset: u16,
    count: u16,
) -> std::io::Result<Vec<u16>> {
    let data = modbus_read(stream, function, offset, count).await?;
    Ok(data
        .chunks(2)
        .map(|c| u16::from_be_bytes([c[0], c[1]]))
        .collect())
}

async fn modbus_bit(stream: &mut TcpStream, function: u8, offset: u16) -> std::io::Result<bool> {
    Ok(modbus_read(stream, function, offset, 1).await?[0] & 1 == 1)
}

#[tokio::test]
async fn modbus_values_match_the_scenario_at_frozen_times() {
    let plan = parse_plan(MODBUS_CONFIG, &examples_dir()).unwrap();
    // 通常・Pump.Run が OFF の出来事の中（146〜168 秒）・Lamp の周期の違う秒。
    for offset_sec in [70.0, 147.0, 151.0] {
        let t = plan.scenario.epoch_sec + offset_sec;
        let mut runtime = Runtime::start(plan.clone(), loopback()).await.unwrap();
        runtime.tick(t);
        let mut stream = TcpStream::connect(runtime.addr()).await.unwrap();

        let level = modbus_registers(&mut stream, 3, 0, 1).await.unwrap();
        assert_eq!(
            codec::decode_i16(level[0]),
            (number(&plan, t, "Tank.Level") * 10.0).round() as i16
        );
        let inflow = modbus_registers(&mut stream, 3, 2, 2).await.unwrap();
        assert_eq!(
            codec::decode_f32([inflow[0], inflow[1]], codec::WordOrder::HighLow),
            number(&plan, t, "Inflow") as f32
        );
        let hours = modbus_registers(&mut stream, 3, 9, 2).await.unwrap();
        assert_eq!(
            codec::decode_u32([hours[0], hours[1]], codec::WordOrder::HighLow),
            number(&plan, t, "Hours").round() as u32
        );
        assert_eq!(
            modbus_bit(&mut stream, 1, 0).await.unwrap(),
            bit(&plan, t, "Pump.Run")
        );
        let lamp = bit(&plan, t, "Lamp");
        let word = modbus_registers(&mut stream, 3, 19, 1).await.unwrap()[0];
        assert_eq!(word, u16::from(lamp) << 3);
        assert_eq!(modbus_bit(&mut stream, 2, 0).await.unwrap(), lamp);
        // 入力レジスタ（u16、scale 1）と、その中のビット。
        let input = modbus_registers(&mut stream, 4, 0, 1).await.unwrap()[0];
        assert_eq!(f64::from(input), number(&plan, t, "Tank.Level").round());
        let input_bits = modbus_registers(&mut stream, 4, 4, 1).await.unwrap()[0];
        assert_eq!(input_bits, u16::from(lamp) << 2);
        runtime.stop();
    }
}

#[tokio::test]
async fn modbus_does_not_answer_inside_a_fault_and_recovers_after() {
    let plan = parse_plan(MODBUS_CONFIG, &examples_dir()).unwrap();
    let epoch = plan.scenario.epoch_sec;
    let mut runtime = Runtime::start(plan, loopback()).await.unwrap();

    runtime.tick(epoch + 105.0);
    assert!(runtime.is_hanging());
    let mut stuck = TcpStream::connect(runtime.addr()).await.unwrap();
    assert!(
        timeout(SHORT, modbus_registers(&mut stuck, 3, 0, 1))
            .await
            .is_err(),
        "途切れの間は答えない"
    );

    runtime.tick(epoch + 120.0);
    assert!(!runtime.is_hanging());
    let mut again = TcpStream::connect(runtime.addr()).await.unwrap();
    assert!(timeout(SHORT, modbus_registers(&mut again, 3, 0, 1))
        .await
        .expect("答える")
        .is_ok());
    runtime.stop();
}

#[tokio::test]
async fn a_saturated_signal_warns_once_per_signal() {
    // 水位 約 82 × 1000 は i16 を超える。
    let text = r#"{
        "format": "banto-plc-sim", "version": 1, "protocol": "slmp",
        "scenario": "plant.scenario.json",
        "map": [{ "signal": "Tank.Level", "address": "D100", "type": "i16", "scale": 1000 }]
    }"#;
    let plan = parse_plan(text, &examples_dir()).unwrap();
    let epoch = plan.scenario.epoch_sec;
    let mut runtime = Runtime::start(plan, loopback()).await.unwrap();
    assert_eq!(runtime.tick(epoch + 60.0).len(), 1);
    assert!(runtime.tick(epoch + 61.0).is_empty());
    runtime.stop();
}
