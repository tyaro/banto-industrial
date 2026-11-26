//! 時刻だけで値が決まる筋書き（版つきの JSON）の読込と評価（tyaro/temp-scada#389）。
//!
//! temp-scada の `crates/scada-scenario`（`43e641cc`）から移した（banto-industrial#1335）。
//! 形の正本は `docs/crates/banto-scenario-format.md`。TypeScript の評価部（当面は temp-scada の
//! `apps/banto-scada/src/scenario/scenario.ts`）と同じ筋書き・同じ時刻から同じ値を出す
//! （共通の試験ケースは `tests/cases/`）。計算の手順（余り・丸め・足す順）は両方で揃える。

use std::collections::BTreeMap;
use std::f64::consts::PI;
use std::fmt;

use serde_json::{Map, Value};

pub const SCENARIO_FORMAT: &str = "banto-scenario";
pub const SCENARIO_VERSION: u64 = 1;

/// 信号の値。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ScenarioValue {
    Number(f64),
    Bit(bool),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Wave {
    pub amplitude: f64,
    pub period_sec: f64,
    pub shift_sec: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NumberSignal {
    pub base: f64,
    pub waves: Vec<Wave>,
    /// 周期の中の秒と値の点列。同じ秒の点を続けると段差になる。
    pub points: Vec<(f64, f64)>,
    pub ramp_per_hour: f64,
    /// ランプの分を、この値で割った余りにする（0 は割らない）。
    pub ramp_wrap: f64,
    /// 小数の桁数（`None` は丸めない）。
    pub digits: Option<u32>,
    pub shift_sec: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BitSignal {
    pub value: bool,
    /// ON・OFF の周期（0 は固定の `value`）。
    pub period_sec: f64,
    pub on_sec: f64,
    pub shift_sec: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Signal {
    Number(NumberSignal),
    Bit(BitSignal),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub at_sec: f64,
    pub duration_sec: f64,
    pub values: BTreeMap<String, ScenarioValue>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Scenario {
    pub cycle_sec: f64,
    pub epoch_sec: f64,
    pub freeze_at_sec: Option<f64>,
    pub signals: BTreeMap<String, Signal>,
    /// 後の出来事ほど優先する。
    pub events: Vec<Event>,
}

/// 筋書きの形の誤り。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScenarioError(pub String);

impl fmt::Display for ScenarioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ScenarioError {}

type Result<T> = std::result::Result<T, ScenarioError>;

fn err<T>(message: impl Into<String>) -> Result<T> {
    Err(ScenarioError(message.into()))
}

fn only_keys(object: &Map<String, Value>, at: &str, allowed: &[&str]) -> Result<()> {
    match object.keys().find(|key| !allowed.contains(&key.as_str())) {
        Some(key) => err(format!("{at}: 知らない項目 {key}")),
        None => Ok(()),
    }
}

fn finite(object: &Map<String, Value>, key: &str, at: &str, fallback: Option<f64>) -> Result<f64> {
    match (object.get(key), fallback) {
        (None, Some(value)) => Ok(value),
        (Some(value), _) => match value.as_f64() {
            Some(n) if n.is_finite() => Ok(n),
            _ => err(format!("{at}.{key}: 有限の数が要る")),
        },
        (None, None) => err(format!("{at}.{key}: 有限の数が要る")),
    }
}

fn positive(object: &Map<String, Value>, key: &str, at: &str) -> Result<f64> {
    let value = finite(object, key, at, None)?;
    if value > 0.0 {
        Ok(value)
    } else {
        err(format!("{at}.{key}: 正の数が要る"))
    }
}

/// 小数部の無い 0 以上の数（JSON の `2` と `2.0` を同じに扱う。TypeScript と揃える）。
fn whole(value: &Value) -> Option<u64> {
    value
        .as_f64()
        .filter(|n| *n >= 0.0 && n.fract() == 0.0 && *n <= u32::MAX as f64)
        .map(|n| n as u64)
}

fn object<'a>(value: &'a Value, at: &str) -> Result<&'a Map<String, Value>> {
    value
        .as_object()
        .ok_or_else(|| ScenarioError(format!("{at}: 物が要る")))
}

fn array<'a>(object: &'a Map<String, Value>, key: &str, at: &str) -> Result<&'a [Value]> {
    match object.get(key) {
        None => Ok(&[]),
        Some(Value::Array(items)) => Ok(items),
        Some(_) => err(format!("{at}.{key}: 配列が要る")),
    }
}

const NUMBER_KEYS: &[&str] = &[
    "kind",
    "like",
    "base",
    "waves",
    "points",
    "rampPerHour",
    "rampWrap",
    "digits",
    "shiftSec",
];
const BIT_KEYS: &[&str] = &["kind", "like", "value", "periodSec", "onSec", "shiftSec"];

fn read_number(raw: &Map<String, Value>, at: &str, cycle_sec: f64) -> Result<NumberSignal> {
    only_keys(raw, at, NUMBER_KEYS)?;
    let mut waves = Vec::new();
    for (index, wave) in array(raw, "waves", at)?.iter().enumerate() {
        let at = format!("{at}.waves[{index}]");
        let wave = object(wave, &at)?;
        only_keys(wave, &at, &["amplitude", "periodSec", "shiftSec"])?;
        waves.push(Wave {
            amplitude: finite(wave, "amplitude", &at, None)?,
            period_sec: positive(wave, "periodSec", &at)?,
            shift_sec: finite(wave, "shiftSec", &at, Some(0.0))?,
        });
    }
    let mut points: Vec<(f64, f64)> = Vec::new();
    for (index, point) in array(raw, "points", at)?.iter().enumerate() {
        let pair = point
            .as_array()
            .filter(|pair| pair.len() == 2)
            .and_then(|pair| Some((pair[0].as_f64()?, pair[1].as_f64()?)))
            .filter(|(sec, value)| sec.is_finite() && value.is_finite());
        let Some((sec, value)) = pair else {
            return err(format!("{at}.points[{index}]: [秒, 値] が要る"));
        };
        if !(0.0..=cycle_sec).contains(&sec) {
            return err(format!("{at}.points[{index}]: 秒は 0〜周期"));
        }
        if points.last().is_some_and(|(previous, _)| sec < *previous) {
            return err(format!("{at}.points[{index}]: 秒が戻っている"));
        }
        points.push((sec, value));
    }
    let digits = match raw.get("digits") {
        None => None,
        Some(value) => match whole(value) {
            Some(n) if n <= 9 => Some(n as u32),
            _ => return err(format!("{at}.digits: 0〜9 の整数が要る")),
        },
    };
    let ramp_wrap = finite(raw, "rampWrap", at, Some(0.0))?;
    if ramp_wrap < 0.0 {
        return err(format!("{at}.rampWrap: 0 以上が要る"));
    }
    Ok(NumberSignal {
        base: finite(raw, "base", at, Some(0.0))?,
        waves,
        points,
        ramp_per_hour: finite(raw, "rampPerHour", at, Some(0.0))?,
        ramp_wrap,
        digits,
        shift_sec: finite(raw, "shiftSec", at, Some(0.0))?,
    })
}

fn read_bit(raw: &Map<String, Value>, at: &str) -> Result<BitSignal> {
    only_keys(raw, at, BIT_KEYS)?;
    let value = match raw.get("value") {
        None => false,
        Some(Value::Bool(value)) => *value,
        Some(_) => return err(format!("{at}.value: true・false が要る")),
    };
    let period_sec = finite(raw, "periodSec", at, Some(0.0))?;
    if period_sec < 0.0 {
        return err(format!("{at}.periodSec: 0 以上が要る"));
    }
    let on_sec = finite(raw, "onSec", at, Some(0.0))?;
    if on_sec < 0.0 || (period_sec > 0.0 && on_sec > period_sec) {
        return err(format!("{at}.onSec: 0〜periodSec"));
    }
    Ok(BitSignal {
        value,
        period_sec,
        on_sec,
        shift_sec: finite(raw, "shiftSec", at, Some(0.0))?,
    })
}

/// `like` をたどり、元の信号の項目へ自分の項目を重ねた生の JSON を返す。
fn resolve_like(
    raws: &Map<String, Value>,
    name: &str,
    trail: &mut Vec<String>,
) -> Result<Map<String, Value>> {
    let raw = object(&raws[name], &format!("signals.{name}"))?;
    let Some(like) = raw.get("like") else {
        return Ok(raw.clone());
    };
    let Some(like) = like.as_str().filter(|like| raws.contains_key(*like)) else {
        return err(format!("signals.{name}.like: 無い信号"));
    };
    if trail.iter().any(|seen| seen == like) {
        return err(format!("signals.{name}.like: 循環している"));
    }
    trail.push(name.to_owned());
    let mut merged = resolve_like(raws, like, trail)?;
    trail.pop();
    if let Some(kind) = raw.get("kind") {
        if merged.get("kind") != Some(kind) {
            return err(format!("signals.{name}.kind: like と種類が違う"));
        }
    }
    merged.remove("like");
    for (key, value) in raw {
        if key != "like" {
            merged.insert(key.clone(), value.clone());
        }
    }
    Ok(merged)
}

impl Scenario {
    /// 筋書きの JSON の文字列を読む。
    pub fn from_json_str(text: &str) -> Result<Self> {
        let json: Value =
            serde_json::from_str(text).map_err(|error| ScenarioError(format!("JSON: {error}")))?;
        Self::from_json(&json)
    }

    /// 筋書きの JSON を読み、形を確かめる。
    pub fn from_json(json: &Value) -> Result<Self> {
        let root = object(json, "筋書き")?;
        only_keys(
            root,
            "筋書き",
            &[
                "format",
                "version",
                "cycleSec",
                "epochSec",
                "freezeAtSec",
                "signals",
                "events",
            ],
        )?;
        if root.get("format").and_then(Value::as_str) != Some(SCENARIO_FORMAT) {
            return err(format!("format: \"{SCENARIO_FORMAT}\" が要る"));
        }
        if root.get("version").and_then(whole) != Some(SCENARIO_VERSION) {
            return err(format!("version: {SCENARIO_VERSION} だけを読める"));
        }
        let cycle_sec = positive(root, "cycleSec", "筋書き")?;
        let epoch_sec = finite(root, "epochSec", "筋書き", Some(0.0))?;
        let freeze_at_sec = match root.get("freezeAtSec") {
            None => None,
            Some(_) => {
                let at = finite(root, "freezeAtSec", "筋書き", None)?;
                if !(0.0..cycle_sec).contains(&at) {
                    return err("freezeAtSec: 0 以上・周期未満");
                }
                Some(at)
            }
        };
        let raws = match root.get("signals") {
            Some(Value::Object(raws)) => raws,
            _ => return err("signals: 物が要る"),
        };
        let mut signals = BTreeMap::new();
        for name in raws.keys() {
            if name.is_empty() {
                return err("signals: 空の名前");
            }
            let raw = resolve_like(raws, name, &mut Vec::new())?;
            let at = format!("signals.{name}");
            let signal = match raw.get("kind").and_then(Value::as_str) {
                Some("number") => Signal::Number(read_number(&raw, &at, cycle_sec)?),
                Some("bit") => Signal::Bit(read_bit(&raw, &at)?),
                _ => return err(format!("{at}.kind: \"number\"・\"bit\" が要る")),
            };
            signals.insert(name.clone(), signal);
        }
        let mut events = Vec::new();
        for (index, event) in array(root, "events", "筋書き")?.iter().enumerate() {
            let at = format!("events[{index}]");
            let event = object(event, &at)?;
            only_keys(event, &at, &["atSec", "durationSec", "values"])?;
            let at_sec = finite(event, "atSec", &at, None)?;
            if !(0.0..cycle_sec).contains(&at_sec) {
                return err(format!("{at}.atSec: 0 以上・周期未満"));
            }
            let duration_sec = positive(event, "durationSec", &at)?;
            if duration_sec > cycle_sec {
                return err(format!("{at}.durationSec: 周期以下"));
            }
            let Some(Value::Object(raw_values)) = event.get("values") else {
                return err(format!("{at}.values: 物が要る"));
            };
            let mut values = BTreeMap::new();
            for (name, value) in raw_values {
                let value = match (signals.get(name), value) {
                    (None, _) => return err(format!("{at}.values.{name}: 無い信号")),
                    (Some(Signal::Bit(_)), Value::Bool(bit)) => ScenarioValue::Bit(*bit),
                    (Some(Signal::Number(_)), Value::Number(n))
                        if n.as_f64().is_some_and(f64::is_finite) =>
                    {
                        ScenarioValue::Number(n.as_f64().unwrap_or_default())
                    }
                    _ => return err(format!("{at}.values.{name}: 信号の種類と違う値")),
                };
                values.insert(name.clone(), value);
            }
            events.push(Event {
                at_sec,
                duration_sec,
                values,
            });
        }
        Ok(Self {
            cycle_sec,
            epoch_sec,
            freeze_at_sec,
            signals,
            events,
        })
    }

    /// 周期の中の秒（0 以上・周期未満）。
    pub fn phase(&self, t_sec: f64) -> f64 {
        modulo(t_sec - self.epoch_sec, self.cycle_sec)
    }

    /// 時刻 `t_sec`（UNIX 秒）の全信号の値。同じ筋書きと時刻には同じ値を返す。
    pub fn evaluate(&self, t_sec: f64) -> BTreeMap<String, ScenarioValue> {
        let mut values: BTreeMap<String, ScenarioValue> = self
            .signals
            .iter()
            .map(|(name, signal)| {
                let value = match signal {
                    Signal::Number(signal) => {
                        ScenarioValue::Number(self.evaluate_number(signal, t_sec))
                    }
                    Signal::Bit(signal) => ScenarioValue::Bit(evaluate_bit(signal, t_sec)),
                };
                (name.clone(), value)
            })
            .collect();
        let phase = self.phase(t_sec);
        for event in &self.events {
            if modulo(phase - event.at_sec, self.cycle_sec) < event.duration_sec {
                values.extend(
                    event
                        .values
                        .iter()
                        .map(|(name, value)| (name.clone(), *value)),
                );
            }
        }
        values
    }

    /// 筋書きを止める時刻（UNIX 秒）。`at_sec` が `None` なら筋書きの `freezeAtSec`、
    /// どちらも無ければ `None`（止めない）。
    pub fn frozen_time_sec(&self, at_sec: Option<f64>) -> Option<f64> {
        at_sec.or(self.freeze_at_sec).map(|at| self.epoch_sec + at)
    }

    fn evaluate_number(&self, signal: &NumberSignal, t_sec: f64) -> f64 {
        let local = t_sec - signal.shift_sec;
        let mut value = signal.base;
        for wave in &signal.waves {
            value += wave.amplitude
                * ((2.0 * PI * modulo(local - wave.shift_sec, wave.period_sec)) / wave.period_sec)
                    .sin();
        }
        value += piecewise(
            &signal.points,
            modulo(local - self.epoch_sec, self.cycle_sec),
        );
        if signal.ramp_per_hour != 0.0 {
            let ramp = (signal.ramp_per_hour * (t_sec - self.epoch_sec)) / 3600.0;
            value += if signal.ramp_wrap > 0.0 {
                modulo(ramp, signal.ramp_wrap)
            } else {
                ramp
            };
        }
        match signal.digits {
            Some(digits) => round_to(value, digits),
            None => value,
        }
    }
}

fn evaluate_bit(signal: &BitSignal, t_sec: f64) -> bool {
    if signal.period_sec == 0.0 {
        return signal.value;
    }
    modulo(t_sec - signal.shift_sec, signal.period_sec) < signal.on_sec
}

/// 負にならない余り（TypeScript 側と同じ手順）。
fn modulo(value: f64, divisor: f64) -> f64 {
    let rest = value % divisor;
    if rest < 0.0 {
        rest + divisor
    } else {
        rest
    }
}

const SCALES: [f64; 10] = [1.0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9];

/// 0.5 を 0 から遠い方へ丸める。
fn round_to(value: f64, digits: u32) -> f64 {
    let scale = SCALES[digits as usize];
    let scaled = value.abs() * scale;
    // 桁へ寄せた中間の値が有限でなければ、丸めずに返す（有限の値を無限にしない）。
    if !scaled.is_finite() {
        return value;
    }
    let rounded = scaled.round() / scale;
    if value < 0.0 {
        -rounded
    } else {
        rounded
    }
}

fn piecewise(points: &[(f64, f64)], phase: f64) -> f64 {
    let Some(&(first_sec, first_value)) = points.first() else {
        return 0.0;
    };
    if phase < first_sec {
        return first_value;
    }
    for pair in points.windows(2) {
        let ((t0, v0), (t1, v1)) = (pair[0], pair[1]);
        if phase < t1 {
            return v0 + ((v1 - v0) * (phase - t0)) / (t1 - t0);
        }
    }
    points[points.len() - 1].1
}
