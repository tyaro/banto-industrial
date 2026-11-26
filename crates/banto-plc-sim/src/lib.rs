//! 筋書きで動く模擬 PLC（SLMP・Modbus TCP）の設定の読込と実行（#1335 の PR2）。
//!
//! 設計は `docs/crates/banto-plc-sim-design.md`。設備（PLC）ができる前に、本物のサーバーを
//! TCP で模擬の PLC へつないで結合を確かめるための道具で、**設定ファイル（割付と筋書き）だけ**
//! で立つ。筋書きの形と評価は `banto-scenario`（`docs/crates/banto-scenario-format.md`）、
//! 待ち受けと応答は `banto-plc` のシミュレータ（`slmp::simulator`・`modbus::simulator`）に任せ、
//! この crate は次の 3 つだけを持つ。
//!
//! - 設定の読込と割付の検査（[`load_plan`]・[`parse_plan`]）。知らない項目・`null`・範囲外は
//!   筋書きの形と同じ厳しさで誤りにする。誤りの文言には、どの割付の何が悪いかを入れる。
//! - 値の符号化（[`encode`]。型・ワードの並び・`scale`・丸め・飽和）。
//! - 実行（[`Runtime`]）。周期ごとに、ある時刻 t で全信号を評価して割付の番地へ置き、`faults` の
//!   中なら `hang`（要求に答えない）、外へ出たら `stop_hanging` する。時刻は実行時は現在、
//!   `--at`・`--freeze` なら止めた時刻（[`Clock`]）。同じ筋書きと時刻からは同じ値になる。
//!
//! 通信の途切れ（`faults`）を筋書きの形に入れず設定に書くのは、筋書きの形 版 1 をブラウザの
//! 模擬と共有したままにするため（2026-11-26 オーナー決定）。判定は筋書きの出来事と同じ
//! 「周期の中の秒」の規則（`banto-scenario` の `Scenario::phase` と同じ余りの取り方）。

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use banto_driver_api::DataType;
use banto_plc::modbus::simulator::Simulator as ModbusSimulator;
use banto_plc::slmp::address::MAX_DEVICE_NUMBER;
use banto_plc::slmp::simulator::Simulator as SlmpSimulator;
use banto_plc::{Address, AddressArea, SlmpAccess, SlmpDevice};
use banto_scenario::{Scenario, ScenarioValue, Signal};
use serde::Deserialize;
use serde_json::Value;

pub const CONFIG_FORMAT: &str = "banto-plc-sim";
pub const CONFIG_VERSION: u64 = 1;

const DEFAULT_PERIOD_MS: u64 = 100;
const PERIOD_MS_RANGE: std::ops::RangeInclusive<u64> = 10..=60_000;

/// 設定の誤り（どの項目の何が悪いかを文言に入れる）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError(pub String);

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConfigError {}

fn err<T>(message: impl Into<String>) -> Result<T, ConfigError> {
    Err(ConfigError(message.into()))
}

/// 話すプロトコル。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Slmp,
    Modbus,
}

impl std::fmt::Display for Protocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Protocol::Slmp => "slmp",
            Protocol::Modbus => "modbus",
        })
    }
}

impl Protocol {
    /// `wordOrder` を省いたときの並び（Hub の `default_word_order_for` と同じ。
    /// SLMP は下位ワードが先、Modbus は上位ワードが先）。
    fn default_order(self) -> Order {
        match self {
            Protocol::Slmp => Order::LowHigh,
            Protocol::Modbus => Order::HighLow,
        }
    }

    /// 省略時の待ち受け（`dev_plc` と同じ）。
    fn default_listen(self) -> SocketAddr {
        let port = match self {
            Protocol::Slmp => 15000,
            Protocol::Modbus => 15020,
        };
        SocketAddr::from(([127, 0, 0, 1], port))
    }
}

/// 2・4 ワードの型のワードの並び。`HighLow` は先頭（小さい番地）が最上位のワード、
/// `LowHigh` は先頭が最下位のワード（4 ワードなら全体を逆順）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Order {
    HighLow,
    LowHigh,
}

impl Order {
    fn parse(text: &str) -> Option<Self> {
        match text {
            "high_low" => Some(Order::HighLow),
            "low_high" => Some(Order::LowHigh),
            _ => None,
        }
    }
}

/// 割付 1 件（検査済み）。
#[derive(Debug, Clone, PartialEq)]
pub struct Mapping {
    pub signal: String,
    pub address: Address,
    /// `Bit` は 1 ビット（ビットの番地、またはワードの中のビット）。
    pub data_type: DataType,
    pub scale: f64,
    pub order: Order,
}

/// 通信の途切れ 1 件。筋書きの周期の中の秒で書く。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HangFault {
    pub at_sec: f64,
    pub duration_sec: f64,
}

/// 設定ファイルと筋書きを読み、割付を検査した結果。
#[derive(Debug, Clone)]
pub struct Plan {
    pub protocol: Protocol,
    pub listen: SocketAddr,
    pub period: Duration,
    pub scenario: Scenario,
    pub mappings: Vec<Mapping>,
    pub faults: Vec<HangFault>,
}

fn modulo(value: f64, divisor: f64) -> f64 {
    // banto-scenario の余りと同じ手順（`r = x % d; if r < 0 { r += d }`）。
    let rest = value % divisor;
    if rest < 0.0 {
        rest + divisor
    } else {
        rest
    }
}

impl Plan {
    /// 時刻 `t_sec`（UNIX 秒）が `faults` のどれかの中か。出来事と同じ規則
    /// （`(周期の中の秒 − atSec) mod 周期 < durationSec`）。
    pub fn in_fault(&self, t_sec: f64) -> bool {
        let phase = self.scenario.phase(t_sec);
        self.faults
            .iter()
            .any(|fault| modulo(phase - fault.at_sec, self.scenario.cycle_sec) < fault.duration_sec)
    }
}

// ---- 設定の読込 ----------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RawConfig {
    format: String,
    version: u64,
    protocol: String,
    listen: Option<String>,
    scenario: String,
    period_ms: Option<u64>,
    word_order: Option<String>,
    map: Vec<RawMap>,
    faults: Option<Vec<RawFault>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RawMap {
    signal: String,
    address: String,
    #[serde(rename = "type")]
    data_type: Option<String>,
    scale: Option<f64>,
    word_order: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RawFault {
    kind: String,
    at_sec: f64,
    duration_sec: f64,
}

/// `null` は省略と同じに扱わない（筋書きの形と同じ）。場所を `map[2].scale` の形で返す。
fn find_null(value: &Value, path: &str) -> Option<String> {
    match value {
        Value::Null => Some(path.to_string()),
        Value::Array(items) => items
            .iter()
            .enumerate()
            .find_map(|(i, item)| find_null(item, &format!("{path}[{i}]"))),
        Value::Object(map) => map.iter().find_map(|(key, item)| {
            let child = if path.is_empty() {
                key.clone()
            } else {
                format!("{path}.{key}")
            };
            find_null(item, &child)
        }),
        _ => None,
    }
}

/// 設定ファイルを読んで検査する。筋書きの相対パスは設定ファイルの置き場からたどる。
pub fn load_plan(path: &Path) -> Result<Plan, ConfigError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| ConfigError(format!("{}: 読めない（{e}）", path.display())))?;
    let base = path.parent().unwrap_or_else(|| Path::new("."));
    parse_plan(&text, base)
}

/// 設定の本文を検査する（`base_dir` は筋書きの相対パスの起点）。
pub fn parse_plan(text: &str, base_dir: &Path) -> Result<Plan, ConfigError> {
    let json: Value =
        serde_json::from_str(text).map_err(|e| ConfigError(format!("設定が JSON でない: {e}")))?;
    if let Some(path) = find_null(&json, "") {
        return err(format!("{path}: null は書けない（省くなら項目ごと省く）"));
    }
    let raw: RawConfig =
        serde_json::from_value(json).map_err(|e| ConfigError(format!("設定の形が違う: {e}")))?;
    if raw.format != CONFIG_FORMAT {
        return err(format!(
            "format: \"{CONFIG_FORMAT}\" が要る（{}）",
            raw.format
        ));
    }
    if raw.version != CONFIG_VERSION {
        return err(format!(
            "version: {CONFIG_VERSION} が要る（{}）",
            raw.version
        ));
    }
    let protocol = match raw.protocol.as_str() {
        "slmp" => Protocol::Slmp,
        "modbus" => Protocol::Modbus,
        other => {
            return err(format!(
                "protocol: \"slmp\" か \"modbus\" が要る（{other}）"
            ))
        }
    };
    let listen = match &raw.listen {
        Some(text) => text
            .parse::<SocketAddr>()
            .map_err(|e| ConfigError(format!("listen {text}: {e}")))?,
        None => protocol.default_listen(),
    };
    let period_ms = raw.period_ms.unwrap_or(DEFAULT_PERIOD_MS);
    if !PERIOD_MS_RANGE.contains(&period_ms) {
        return err(format!(
            "periodMs: {}〜{} が要る（{period_ms}）",
            PERIOD_MS_RANGE.start(),
            PERIOD_MS_RANGE.end()
        ));
    }
    let default_order = match &raw.word_order {
        Some(text) => Order::parse(text).ok_or_else(|| {
            ConfigError(format!("wordOrder: high_low か low_high が要る（{text}）"))
        })?,
        None => protocol.default_order(),
    };

    let scenario_path = base_dir.join(&raw.scenario);
    let scenario_text = std::fs::read_to_string(&scenario_path).map_err(|e| {
        ConfigError(format!(
            "scenario {}: 読めない（{e}）",
            scenario_path.display()
        ))
    })?;
    let scenario = Scenario::from_json_str(&scenario_text)
        .map_err(|e| ConfigError(format!("scenario {}: {e}", scenario_path.display())))?;

    let mut mappings = Vec::with_capacity(raw.map.len());
    for (index, entry) in raw.map.iter().enumerate() {
        mappings.push(build_mapping(
            index,
            entry,
            protocol,
            default_order,
            &scenario,
        )?);
    }
    check_overlaps(&mappings)?;

    let mut faults = Vec::new();
    for (index, fault) in raw.faults.iter().flatten().enumerate() {
        faults.push(build_fault(index, fault, &scenario)?);
    }

    Ok(Plan {
        protocol,
        listen,
        period: Duration::from_millis(period_ms),
        scenario,
        mappings,
        faults,
    })
}

fn build_fault(
    index: usize,
    fault: &RawFault,
    scenario: &Scenario,
) -> Result<HangFault, ConfigError> {
    let at = format!("faults[{index}]");
    if fault.kind != "hang" {
        return err(format!("{at}.kind: \"hang\" が要る（{}）", fault.kind));
    }
    let cycle = scenario.cycle_sec;
    if !(fault.at_sec.is_finite() && fault.at_sec >= 0.0 && fault.at_sec < cycle) {
        return err(format!(
            "{at}.atSec: 0 以上で周期（{cycle} 秒）未満が要る（{}）",
            fault.at_sec
        ));
    }
    if !(fault.duration_sec.is_finite() && fault.duration_sec > 0.0 && fault.duration_sec <= cycle)
    {
        return err(format!(
            "{at}.durationSec: 正で周期（{cycle} 秒）以下が要る（{}）",
            fault.duration_sec
        ));
    }
    Ok(HangFault {
        at_sec: fault.at_sec,
        duration_sec: fault.duration_sec,
    })
}

fn build_mapping(
    index: usize,
    entry: &RawMap,
    protocol: Protocol,
    default_order: Order,
    scenario: &Scenario,
) -> Result<Mapping, ConfigError> {
    let at = format!("map[{index}] ({})", entry.signal);
    let Some(signal) = scenario.signals.get(&entry.signal) else {
        return err(format!("{at}: 筋書きに無い信号"));
    };
    let address = match protocol {
        Protocol::Slmp => Address::parse_slmp(&entry.address),
        Protocol::Modbus => Address::parse(&entry.address),
    }
    .map_err(|e| {
        ConfigError(format!(
            "{at}.address {}: {protocol} の番地として読めない（{e}）",
            entry.address
        ))
    })?;
    let bit_address = match address {
        Address::Slmp { device, bit, .. } => bit.is_some() || device.access() == SlmpAccess::Bit,
        Address::ModbusRef { area, bit, .. } => {
            bit.is_some() || matches!(area, AddressArea::Coil | AddressArea::DiscreteInput)
        }
    };
    let data_type = match &entry.data_type {
        Some(text) => DataType::parse(text)
            .ok_or_else(|| ConfigError(format!("{at}.type: 知らない型（{text}）")))?,
        None if bit_address => DataType::Bit,
        None => DataType::I16,
    };
    match (bit_address, data_type == DataType::Bit) {
        (true, false) => {
            return err(format!(
                "{at}: ビットの番地（{}）に {data_type} は置けない（type は bit）",
                entry.address
            ))
        }
        (false, true) => {
            return err(format!(
                "{at}: ワードの番地（{}）に bit は置けない（ワードの中のビットは .N で書く）",
                entry.address
            ))
        }
        _ => {}
    }
    match (signal, bit_address) {
        (Signal::Bit(_), false) => {
            return err(format!(
                "{at}: ビットの信号はビットの番地にだけ割り付けられる"
            ))
        }
        (Signal::Number(_), true) => {
            return err(format!(
                "{at}: 数値の信号はワードの番地にだけ割り付けられる"
            ))
        }
        _ => {}
    }
    if data_type == DataType::Bit && (entry.scale.is_some() || entry.word_order.is_some()) {
        return err(format!("{at}: ビットの割付に scale・wordOrder は書けない"));
    }
    let scale = entry.scale.unwrap_or(1.0);
    if !scale.is_finite() || scale == 0.0 {
        return err(format!("{at}.scale: 有限で 0 でない数が要る（{scale}）"));
    }
    let order = match &entry.word_order {
        Some(text) => Order::parse(text).ok_or_else(|| {
            ConfigError(format!(
                "{at}.wordOrder: high_low か low_high が要る（{text}）"
            ))
        })?,
        None => default_order,
    };
    // 範囲（ワードの幅を含めて番地の空間に収まること）。
    let span = u32::from(data_type.register_span());
    match address {
        Address::Slmp { number, .. } if !bit_address && number + span - 1 > MAX_DEVICE_NUMBER => {
            return err(format!("{at}.address: デバイス番号が範囲を超える"))
        }
        Address::ModbusRef { offset, .. } if !bit_address && u32::from(offset) + span > 65_536 => {
            return err(format!("{at}.address: レジスタが範囲を超える"))
        }
        _ => {}
    }
    Ok(Mapping {
        signal: entry.signal.clone(),
        address,
        data_type,
        scale,
        order,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Space {
    Slmp(SlmpDevice),
    Modbus(AddressArea),
}

/// 割付が使う番地の升目: (空間, 番号, ワードの中のビット)。`None` はその番号全体。
fn cells(mapping: &Mapping) -> Vec<(Space, u32, Option<u8>)> {
    let span = u32::from(mapping.data_type.register_span());
    match mapping.address {
        Address::Slmp {
            device,
            number,
            bit,
        } => {
            if device.access() == SlmpAccess::Bit {
                vec![(Space::Slmp(device), number, None)]
            } else if bit.is_some() {
                vec![(Space::Slmp(device), number, bit)]
            } else {
                (0..span)
                    .map(|i| (Space::Slmp(device), number + i, None))
                    .collect()
            }
        }
        Address::ModbusRef { area, offset, bit } => {
            let offset = u32::from(offset);
            if matches!(area, AddressArea::Coil | AddressArea::DiscreteInput) {
                vec![(Space::Modbus(area), offset, None)]
            } else if bit.is_some() {
                vec![(Space::Modbus(area), offset, bit)]
            } else {
                (0..span)
                    .map(|i| (Space::Modbus(area), offset + i, None))
                    .collect()
            }
        }
    }
}

/// 同じ番地（ワードの幅を含めて重なる）に 2 つ割り付けたら誤り。同じワードの別のビットは可。
fn check_overlaps(mappings: &[Mapping]) -> Result<(), ConfigError> {
    type Occupants = Vec<(usize, Option<u8>)>;
    let mut used: HashMap<(Space, u32), Occupants> = HashMap::new();
    for (index, mapping) in mappings.iter().enumerate() {
        for (space, number, bit) in cells(mapping) {
            let occupants = used.entry((space, number)).or_default();
            if let Some(&(other, _)) = occupants
                .iter()
                .find(|(_, other_bit)| other_bit.is_none() || bit.is_none() || *other_bit == bit)
            {
                return err(format!(
                    "map[{index}] ({}): 番地が map[{other}] ({}) と重なる",
                    mapping.signal, mappings[other].signal
                ));
            }
            occupants.push((index, bit));
        }
    }
    Ok(())
}

// ---- 符号化 ------------------------------------------------------------------

/// 整数の型へ丸める（0.5 は 0 から遠い方）。範囲を外れたら飽和させ、飽和したかを返す。
/// `hi_exclusive` は型の最大値の次の値（2 の冪なので f64 で正確）。
fn round_saturating(value: f64, lo: f64, hi_exclusive: f64) -> (f64, bool) {
    let rounded = value.round();
    // NaN も飽和の扱い（`as` は NaN を 0 にする）。
    let in_range = rounded >= lo && rounded < hi_exclusive;
    (rounded, !in_range)
}

fn order_words<const N: usize>(mut words: [u16; N], order: Order) -> Vec<u16> {
    // `words` は上位のワードから。`LowHigh` は全体を逆順にする。
    if order == Order::LowHigh {
        words.reverse();
    }
    words.to_vec()
}

fn split32(bits: u32, order: Order) -> Vec<u16> {
    order_words([(bits >> 16) as u16, bits as u16], order)
}

fn split64(bits: u64, order: Order) -> Vec<u16> {
    order_words(
        [
            (bits >> 48) as u16,
            (bits >> 32) as u16,
            (bits >> 16) as u16,
            bits as u16,
        ],
        order,
    )
}

/// 置く値（`scale` を掛けた後）を型のワード列にする。整数の型は 0 から遠い方へ丸めて範囲で
/// 飽和させ、飽和したかを返す。浮動小数点の型は丸めない。`Bit` は呼べない。
pub fn encode(value: f64, data_type: DataType, order: Order) -> (Vec<u16>, bool) {
    match data_type {
        DataType::Bit => unreachable!("ビットはワードに符号化しない"),
        DataType::I16 => {
            let (r, sat) = round_saturating(value, -32768.0, 32768.0);
            (vec![r as i16 as u16], sat)
        }
        DataType::U16 => {
            let (r, sat) = round_saturating(value, 0.0, 65536.0);
            (vec![r as u16], sat)
        }
        DataType::I32 => {
            let (r, sat) = round_saturating(value, -2147483648.0, 2147483648.0);
            (split32(r as i32 as u32, order), sat)
        }
        DataType::U32 => {
            let (r, sat) = round_saturating(value, 0.0, 4294967296.0);
            (split32(r as u32, order), sat)
        }
        DataType::F32 => (split32((value as f32).to_bits(), order), false),
        DataType::I64 => {
            let (r, sat) = round_saturating(value, -9223372036854775808.0, 9223372036854775808.0);
            (split64(r as i64 as u64, order), sat)
        }
        DataType::U64 => {
            let (r, sat) = round_saturating(value, 0.0, 18446744073709551616.0);
            (split64(r as u64, order), sat)
        }
        DataType::F64 => (split64(value.to_bits(), order), false),
    }
}

// ---- 時刻 --------------------------------------------------------------------

/// 値を決める時刻の元。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Clock {
    /// 今（UNIX 秒、小数あり）。
    Live,
    /// 止めた時刻（UNIX 秒）。
    Fixed(f64),
}

impl Clock {
    /// `--at`（UNIX 秒）が優先、次に `--freeze`（筋書きの `freezeAtSec`。無ければ誤り）。
    pub fn from_args(plan: &Plan, at: Option<f64>, freeze: bool) -> Result<Self, ConfigError> {
        if let Some(at) = at {
            return Ok(Clock::Fixed(at));
        }
        if freeze {
            return match plan.scenario.frozen_time_sec(None) {
                Some(t) => Ok(Clock::Fixed(t)),
                None => err("--freeze: 筋書きに freezeAtSec が無い"),
            };
        }
        Ok(Clock::Live)
    }

    pub fn now(&self) -> f64 {
        match self {
            Clock::Fixed(t) => *t,
            Clock::Live => SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.0),
        }
    }
}

// ---- 実行 --------------------------------------------------------------------

enum Sim {
    Slmp(SlmpSimulator),
    Modbus {
        sim: ModbusSimulator,
        /// input register は読み出し専用でシミュレータから読み戻せないので、置いた値を覚える
        /// （ワードの中のビットを置くときの元の値）。
        input_registers: HashMap<u16, u16>,
    },
}

/// 立てた模擬 PLC と、値を置き直す状態。
pub struct Runtime {
    plan: Plan,
    sim: Sim,
    addr: SocketAddr,
    hanging: bool,
    saturation_warned: Vec<bool>,
}

impl Runtime {
    /// 待ち受けを始める。`listen` は設定の `listen` を上書きする。
    pub async fn start(plan: Plan, listen: Option<SocketAddr>) -> std::io::Result<Self> {
        let want = listen.unwrap_or(plan.listen);
        let (sim, addr) = match plan.protocol {
            Protocol::Slmp => {
                let sim = SlmpSimulator::start_on(want).await?;
                let addr = sim.addr;
                (Sim::Slmp(sim), addr)
            }
            Protocol::Modbus => {
                let sim = ModbusSimulator::start_on(want).await?;
                let addr = sim.addr;
                (
                    Sim::Modbus {
                        sim,
                        input_registers: HashMap::new(),
                    },
                    addr,
                )
            }
        };
        let saturation_warned = vec![false; plan.mappings.len()];
        Ok(Self {
            plan,
            sim,
            addr,
            hanging: false,
            saturation_warned,
        })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn protocol(&self) -> Protocol {
        self.plan.protocol
    }

    pub fn signal_count(&self) -> usize {
        self.plan.mappings.len()
    }

    /// 今 `hang` 中か。
    pub fn is_hanging(&self) -> bool {
        self.hanging
    }

    /// 時刻 `t_sec` で 1 周期ぶん置き直す。`faults` の中か外かが変わったときだけ `hang`・
    /// `stop_hanging` を呼ぶ（途切れの間も値は置き直す）。整数の型が飽和した信号があれば、
    /// その信号で最初の 1 回だけ警告の文言を返す。
    pub fn tick(&mut self, t_sec: f64) -> Vec<String> {
        let in_fault = self.plan.in_fault(t_sec);
        if in_fault != self.hanging {
            self.set_hang(in_fault);
            self.hanging = in_fault;
        }
        let values = self.plan.scenario.evaluate(t_sec);
        let mut warnings = Vec::new();
        for (index, mapping) in self.plan.mappings.iter().enumerate() {
            let value = values
                .get(&mapping.signal)
                .copied()
                .unwrap_or(ScenarioValue::Number(0.0));
            if mapping.data_type == DataType::Bit {
                let on = match value {
                    ScenarioValue::Bit(b) => b,
                    ScenarioValue::Number(n) => n != 0.0,
                };
                self.sim.put_bit(mapping, on);
            } else {
                let number = match value {
                    ScenarioValue::Number(n) => n,
                    ScenarioValue::Bit(b) => f64::from(u8::from(b)),
                };
                let (words, saturated) =
                    encode(number * mapping.scale, mapping.data_type, mapping.order);
                if saturated && !self.saturation_warned[index] {
                    self.saturation_warned[index] = true;
                    warnings.push(format!(
                        "警告: {} の値 {} が {} の範囲を超えたので飽和させた（以後この信号は警告しない）",
                        mapping.signal,
                        number * mapping.scale,
                        mapping.data_type
                    ));
                }
                self.sim.put_words(mapping, &words);
            }
        }
        warnings
    }

    fn set_hang(&self, on: bool) {
        match &self.sim {
            Sim::Slmp(sim) => {
                if on {
                    sim.hang()
                } else {
                    sim.stop_hanging()
                }
            }
            Sim::Modbus { sim, .. } => {
                if on {
                    sim.hang()
                } else {
                    sim.stop_hanging()
                }
            }
        }
    }

    /// `periodMs` ごとに置き直し、Ctrl+C で止める。警告は標準エラーへ。
    pub async fn serve_until_ctrl_c(mut self, clock: Clock) {
        let mut interval = tokio::time::interval(self.plan.period);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    for warning in self.tick(clock.now()) {
                        eprintln!("{warning}");
                    }
                }
                _ = tokio::signal::ctrl_c() => break,
            }
        }
        self.stop();
    }

    /// シミュレータを止める（接続も切る）。
    pub fn stop(self) {
        match self.sim {
            Sim::Slmp(sim) => sim.stop(),
            Sim::Modbus { sim, .. } => sim.stop(),
        }
    }
}

fn with_bit(word: u16, bit: u8, on: bool) -> u16 {
    if on {
        word | (1 << bit)
    } else {
        word & !(1 << bit)
    }
}

impl Sim {
    fn put_bit(&mut self, mapping: &Mapping, on: bool) {
        match (self, mapping.address) {
            (
                Sim::Slmp(sim),
                Address::Slmp {
                    device,
                    number,
                    bit,
                },
            ) => match bit {
                Some(bit) => {
                    // ワードの中のビット: 読んで、そのビットだけ替えて書き戻す
                    // （外から書かれて held の番地なら `set_word` は no-op のまま）。
                    let word = sim.get_word(device, number);
                    sim.set_word(device, number, with_bit(word, bit, on));
                }
                None => sim.set_bit(device, number, on),
            },
            (
                Sim::Modbus {
                    sim,
                    input_registers,
                },
                Address::ModbusRef { area, offset, bit },
            ) => {
                match (area, bit) {
                    (AddressArea::Coil, _) => sim.set_coil(offset, on),
                    (AddressArea::DiscreteInput, _) => sim.set_discrete_input(offset, on),
                    (AddressArea::HoldingRegister, Some(bit)) => {
                        let word = sim.get_holding_register(offset);
                        sim.set_holding_register(offset, with_bit(word, bit, on));
                    }
                    (AddressArea::InputRegister, Some(bit)) => {
                        let word = input_registers.get(&offset).copied().unwrap_or(0);
                        let word = with_bit(word, bit, on);
                        input_registers.insert(offset, word);
                        sim.set_input_register(offset, word);
                    }
                    // 検査で弾く組（ビットの型はビットの番地かワードの中のビットだけ）。
                    (_, None) => {}
                }
            }
            _ => {}
        }
    }

    fn put_words(&mut self, mapping: &Mapping, words: &[u16]) {
        match (self, mapping.address) {
            (Sim::Slmp(sim), Address::Slmp { device, number, .. }) => {
                sim.set_words(device, number, words)
            }
            (
                Sim::Modbus {
                    sim,
                    input_registers,
                },
                Address::ModbusRef { area, offset, .. },
            ) => {
                match area {
                    AddressArea::HoldingRegister => sim.set_holding_registers(offset, words),
                    AddressArea::InputRegister => {
                        for (i, &word) in words.iter().enumerate() {
                            let at = offset + i as u16;
                            input_registers.insert(at, word);
                            sim.set_input_register(at, word);
                        }
                    }
                    // 検査で弾く組（ワードの型はコイルに置けない）。
                    AddressArea::Coil | AddressArea::DiscreteInput => {}
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use banto_register_codec as codec;

    fn codec_order(order: Order) -> codec::WordOrder {
        match order {
            Order::HighLow => codec::WordOrder::HighLow,
            Order::LowHigh => codec::WordOrder::LowHigh,
        }
    }

    fn examples_dir() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("examples")
    }

    const SCENARIO: &str = r#"{
        "format": "banto-scenario", "version": 1, "cycleSec": 180, "epochSec": 0,
        "signals": {
            "N": { "kind": "number", "base": 5 },
            "B": { "kind": "bit", "value": true }
        }
    }"#;

    /// 試験用に筋書きを一時の置き場へ書いて、設定の本文を検査する。
    fn plan_of(body: &str) -> Result<Plan, ConfigError> {
        // 試験は並列に走るので、呼び出しごとに別の置き場へ書く（終わりに消える）。
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::write(dir.join("s.json"), SCENARIO).unwrap();
        let text = format!(
            r#"{{ "format": "banto-plc-sim", "version": 1, "scenario": "s.json", {body} }}"#
        );
        parse_plan(&text, dir)
    }

    fn message(body: &str) -> String {
        plan_of(body).expect_err("誤りのはず").0
    }

    #[test]
    fn the_example_config_loads() {
        let plan = load_plan(&examples_dir().join("plant.sim.json")).unwrap();
        assert_eq!(plan.protocol, Protocol::Slmp);
        assert_eq!(plan.mappings.len(), 5);
        assert_eq!(plan.faults.len(), 1);
        assert_eq!(plan.period, Duration::from_millis(100));
        assert_eq!(plan.mappings[2].order, Order::HighLow);
        assert_eq!(plan.mappings[3].data_type, DataType::Bit);
    }

    #[test]
    fn a_valid_modbus_config_and_defaults() {
        let plan = plan_of(
            r#""protocol": "modbus", "map": [
                { "signal": "N", "address": "40001" },
                { "signal": "B", "address": "00001" },
                { "signal": "B", "address": "40010.3" }
            ]"#,
        )
        .unwrap();
        assert_eq!(plan.listen, "127.0.0.1:15020".parse().unwrap());
        assert_eq!(plan.mappings[0].data_type, DataType::I16);
        assert_eq!(plan.mappings[1].data_type, DataType::Bit);
        assert_eq!(plan.mappings[2].data_type, DataType::Bit);
        // Modbus の既定の並びは high_low（Hub と同じ）。
        assert_eq!(plan.mappings[0].order, Order::HighLow);
        assert!(plan.faults.is_empty());
    }

    #[test]
    fn the_default_word_order_follows_the_protocol() {
        let plan = plan_of(r#""protocol": "slmp", "map": [{ "signal": "N", "address": "D100" }]"#)
            .unwrap();
        assert_eq!(plan.mappings[0].order, Order::LowHigh);
    }

    #[test]
    fn scale_and_word_order_on_a_bit_mapping_are_errors() {
        let m = message(
            r#""protocol": "slmp", "map": [{ "signal": "B", "address": "M100", "scale": 2 }]"#,
        );
        assert!(m.contains("map[0] (B)") && m.contains("scale"), "{m}");
        let m = message(
            r#""protocol": "slmp", "map": [{ "signal": "B", "address": "M100", "wordOrder": "high_low" }]"#,
        );
        assert!(m.contains("wordOrder"), "{m}");
    }

    #[test]
    fn unknown_fields_and_nulls_are_errors() {
        assert!(message(r#""protocol": "slmp", "map": [], "extra": 1"#).contains("extra"));
        assert!(message(
            r#""protocol": "slmp", "map": [{ "signal": "N", "address": "D1", "bogus": 1 }]"#
        )
        .contains("bogus"));
        let m = message(
            r#""protocol": "slmp", "map": [{ "signal": "N", "address": "D1", "scale": null }]"#,
        );
        assert!(m.contains("map[0].scale") && m.contains("null"), "{m}");
        assert!(message(r#""protocol": "slmp", "listen": null, "map": []"#).contains("listen"));
    }

    #[test]
    fn address_notation_must_match_the_protocol() {
        let m = message(r#""protocol": "slmp", "map": [{ "signal": "N", "address": "40001" }]"#);
        assert!(m.contains("map[0]") && m.contains("slmp"), "{m}");
        let m = message(r#""protocol": "modbus", "map": [{ "signal": "N", "address": "D100" }]"#);
        assert!(m.contains("map[0]") && m.contains("modbus"), "{m}");
    }

    #[test]
    fn signal_kind_and_address_kind_must_agree() {
        let m = message(r#""protocol": "slmp", "map": [{ "signal": "B", "address": "D100" }]"#);
        assert!(m.contains("ビット"), "{m}");
        let m = message(r#""protocol": "slmp", "map": [{ "signal": "N", "address": "M10" }]"#);
        assert!(m.contains("数値"), "{m}");
        let m = message(r#""protocol": "slmp", "map": [{ "signal": "N", "address": "D100.3" }]"#);
        assert!(m.contains("数値"), "{m}");
        // 型の明示もビット/ワードに合わせる。
        let m = message(
            r#""protocol": "slmp", "map": [{ "signal": "N", "address": "M10", "type": "i16" }]"#,
        );
        assert!(m.contains("ビットの番地"), "{m}");
        let m = message(
            r#""protocol": "slmp", "map": [{ "signal": "N", "address": "D10", "type": "bit" }]"#,
        );
        assert!(m.contains("ワードの番地"), "{m}");
    }

    #[test]
    fn unknown_signal_and_bad_type_are_errors() {
        let m = message(r#""protocol": "slmp", "map": [{ "signal": "Nope", "address": "D1" }]"#);
        assert!(m.contains("Nope") && m.contains("筋書きに無い"), "{m}");
        let m = message(
            r#""protocol": "slmp", "map": [{ "signal": "N", "address": "D1", "type": "u128" }]"#,
        );
        assert!(m.contains(".type"), "{m}");
    }

    #[test]
    fn overlaps_are_errors_but_other_bits_of_one_word_are_fine() {
        let m = message(
            r#""protocol": "slmp", "map": [
                { "signal": "N", "address": "D100", "type": "i32" },
                { "signal": "N", "address": "D101" }
            ]"#,
        );
        assert!(
            m.contains("map[1]") && m.contains("map[0]") && m.contains("重なる"),
            "{m}"
        );
        // 同じワードの別のビットは可。
        plan_of(
            r#""protocol": "slmp", "map": [
                { "signal": "B", "address": "D120.3" },
                { "signal": "B", "address": "D120.4" }
            ]"#,
        )
        .unwrap();
        // 同じビットは重なり。ワード全体とそのワードのビットも重なり。
        assert!(message(
            r#""protocol": "slmp", "map": [
                { "signal": "B", "address": "D120.3" },
                { "signal": "B", "address": "D120.3" }
            ]"#
        )
        .contains("重なる"));
        assert!(message(
            r#""protocol": "slmp", "map": [
                { "signal": "N", "address": "D120" },
                { "signal": "B", "address": "D120.3" }
            ]"#
        )
        .contains("重なる"));
        // Modbus: 保持レジスタと入力レジスタは別の空間。
        plan_of(
            r#""protocol": "modbus", "map": [
                { "signal": "N", "address": "40001" },
                { "signal": "N", "address": "30001" }
            ]"#,
        )
        .unwrap();
    }

    #[test]
    fn scale_period_and_fault_ranges_are_checked() {
        assert!(message(
            r#""protocol": "slmp", "map": [{ "signal": "N", "address": "D1", "scale": 0 }]"#
        )
        .contains("scale"));
        assert!(message(r#""protocol": "slmp", "periodMs": 9, "map": []"#).contains("periodMs"));
        assert!(
            message(r#""protocol": "slmp", "periodMs": 60001, "map": []"#).contains("periodMs")
        );
        for fault in [
            r#"{ "kind": "hang", "atSec": 180, "durationSec": 5 }"#,
            r#"{ "kind": "hang", "atSec": -1, "durationSec": 5 }"#,
            r#"{ "kind": "hang", "atSec": 10, "durationSec": 0 }"#,
            r#"{ "kind": "hang", "atSec": 10, "durationSec": 181 }"#,
            r#"{ "kind": "crash", "atSec": 10, "durationSec": 5 }"#,
        ] {
            let m = message(&format!(
                r#""protocol": "slmp", "map": [], "faults": [{fault}]"#
            ));
            assert!(m.contains("faults[0]"), "{m}");
        }
        // 周期の終わりをまたいでよい。
        plan_of(r#""protocol": "slmp", "map": [], "faults": [{ "kind": "hang", "atSec": 170, "durationSec": 20 }]"#)
            .unwrap();
    }

    #[test]
    fn format_and_version_are_checked() {
        let text = r#"{ "format": "other", "version": 1, "protocol": "slmp", "scenario": "s.json", "map": [] }"#;
        assert!(parse_plan(text, Path::new("."))
            .unwrap_err()
            .0
            .contains("format"));
        let text = r#"{ "format": "banto-plc-sim", "version": 2, "protocol": "slmp", "scenario": "s.json", "map": [] }"#;
        assert!(parse_plan(text, Path::new("."))
            .unwrap_err()
            .0
            .contains("version"));
    }

    #[test]
    fn fault_windows_follow_the_event_rule() {
        let plan = plan_of(
            r#""protocol": "slmp", "map": [], "faults": [{ "kind": "hang", "atSec": 170, "durationSec": 20 }]"#,
        )
        .unwrap();
        // epoch 0, 周期 180: 170〜180 と 0〜10（周期をまたぐ）が途切れ。
        assert!(plan.in_fault(170.0));
        assert!(plan.in_fault(179.9));
        assert!(plan.in_fault(180.0 + 5.0));
        assert!(!plan.in_fault(10.0));
        assert!(!plan.in_fault(169.9));
        assert!(plan.in_fault(360.0 + 175.0));
    }

    const ORDERS: [Order; 2] = [Order::HighLow, Order::LowHigh];

    #[test]
    fn integers_round_trip_through_the_codec_in_both_orders() {
        for order in ORDERS {
            let co = codec_order(order);
            let (w, sat) = encode(-1234.0, DataType::I16, order);
            assert!(!sat);
            assert_eq!(codec::decode_i16(w[0]), -1234);
            let (w, _) = encode(65000.0, DataType::U16, order);
            assert_eq!(w, vec![65000]);
            let (w, _) = encode(-70000.0, DataType::I32, order);
            assert_eq!(codec::decode_i32([w[0], w[1]], co), -70000);
            let (w, _) = encode(4_000_000_000.0, DataType::U32, order);
            assert_eq!(codec::decode_u32([w[0], w[1]], co), 4_000_000_000);
            let (w, _) = encode(-9_000_000_000_000.0, DataType::I64, order);
            assert_eq!(
                codec::decode_i64([w[0], w[1], w[2], w[3]], co),
                -9_000_000_000_000
            );
            let (w, _) = encode(9_007_199_254_740_992.0, DataType::U64, order);
            assert_eq!(
                codec::decode_u64([w[0], w[1], w[2], w[3]], co),
                9_007_199_254_740_992
            );
        }
    }

    #[test]
    fn floats_round_trip_through_the_codec_in_both_orders() {
        for order in ORDERS {
            let co = codec_order(order);
            let (w, sat) = encode(62.25, DataType::F32, order);
            assert!(!sat);
            assert_eq!(codec::decode_f32([w[0], w[1]], co), 62.25);
            let (w, _) = encode(-1234.5678901234, DataType::F64, order);
            assert_eq!(
                codec::decode_f64([w[0], w[1], w[2], w[3]], co),
                -1234.5678901234
            );
        }
    }

    #[test]
    fn word_order_places_the_low_word_first_only_for_low_high() {
        assert_eq!(
            encode(f64::from(0x0001_0002u32), DataType::U32, Order::HighLow).0,
            vec![1, 2]
        );
        assert_eq!(
            encode(f64::from(0x0001_0002u32), DataType::U32, Order::LowHigh).0,
            vec![2, 1]
        );
        let (w, _) = encode(f64::from(0x0001_0002u32), DataType::U64, Order::LowHigh);
        assert_eq!(w, vec![2, 1, 0, 0]);
    }

    #[test]
    fn rounding_goes_away_from_zero_on_halves() {
        assert_eq!(encode(0.5, DataType::I16, Order::HighLow).0, vec![1]);
        assert_eq!(
            encode(-0.5, DataType::I16, Order::HighLow).0,
            vec![(-1i16) as u16]
        );
        assert_eq!(encode(2.5, DataType::U16, Order::HighLow).0, vec![3]);
        assert_eq!(encode(0.4, DataType::U16, Order::HighLow).0, vec![0]);
    }

    #[test]
    fn integers_saturate_and_report_it() {
        let (w, sat) = encode(40000.0, DataType::I16, Order::HighLow);
        assert!(sat);
        assert_eq!(w, vec![i16::MAX as u16]);
        let (w, sat) = encode(-40000.0, DataType::I16, Order::HighLow);
        assert!(sat);
        assert_eq!(w, vec![i16::MIN as u16]);
        let (w, sat) = encode(-1.0, DataType::U16, Order::HighLow);
        assert!(sat);
        assert_eq!(w, vec![0]);
        let (w, sat) = encode(1e12, DataType::U32, Order::HighLow);
        assert!(sat);
        assert_eq!(w, vec![0xFFFF, 0xFFFF]);
        let (w, sat) = encode(1e30, DataType::I64, Order::HighLow);
        assert!(sat);
        assert_eq!(w, vec![0x7FFF, 0xFFFF, 0xFFFF, 0xFFFF]);
        let (_, sat) = encode(f64::NAN, DataType::I32, Order::HighLow);
        assert!(sat);
        // 範囲の端は飽和でない。
        assert!(!encode(32767.0, DataType::I16, Order::HighLow).1);
        assert!(!encode(-32768.0, DataType::I16, Order::HighLow).1);
        assert!(!encode(65535.0, DataType::U16, Order::HighLow).1);
        // 丸めて範囲に収まるなら飽和でない。
        assert!(!encode(65535.4, DataType::U16, Order::HighLow).1);
        assert!(encode(65535.5, DataType::U16, Order::HighLow).1);
    }
}
