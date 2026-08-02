//! Rule evaluation (W3-B safety invariants #2 and #3,
//! `luminous-discovering-goblet.md`). Given the current-value cache and a set of
//! compiled rules, this produces [`PendingWrite`] *intents* - and nothing else.
//!
//! ## Structural eval/exec separation (invariant #2)
//!
//! [`RuleEngine::evaluate`] has no access to a broker handle, a
//! write-capable channel, or the DB. It cannot issue a write; the worst a bug
//! in here can do is emit a wrong *intent*, which [`crate::engine::writer`] then
//! still gates on arming/rate-limit/dry-run before any socket is touched. The
//! separation is enforced by types: this module never imports `BrokerHandle`.
//!
//! ## Edge-triggered, and why the first observation only seeds (invariant #3)
//!
//! Each rule remembers its previous determinate "conditions-met" boolean. A
//! write intent is emitted only on the specific transition its `edge_mode`
//! names (`rising` = false→true, `falling` = true→false, `change` = either), so
//! a condition that stays true across many polls fires exactly once. The very
//! first determinate evaluation of a rule only *seeds* that remembered state and
//! never fires - otherwise a condition that is already true at startup (or right
//! after a restart) would fire immediately, which is exactly the auto-resume
//! footgun the arming rule exists to prevent.
//!
//! ## Indeterminate sources never cause a transition
//!
//! If any source a rule needs (a condition source, or the copy-from-source tag)
//! is missing or `Bad` this cycle, the rule is *indeterminate*: its remembered
//! state is left untouched and it cannot fire. In particular a source going
//! `Bad` never manufactures a falling edge - the engine only ever transitions on
//! values it actually confirmed.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use banto_plc::{DataType, PlcValue};

use super::current_values::CurrentValues;

/// A write target's or source tag's wire shape as the engine needs it: a
/// numeric/bit device with its [`DataType`], or a MELSEC string device
/// spanning `words` consecutive 16-bit registers (S2 文字列タグ). Resolved once
/// at engine start (see [`crate::engine::compile_rules`]) from the row's
/// `data_type`/`string_length`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireShape {
    Numeric(DataType),
    Str { words: u16 },
}

/// How often the engine may log a value-type mismatch (a numeric operator hit
/// a string value, or a string target got a non-string copy source). A
/// misconfiguration that survived save-time validation would otherwise spam a
/// line every evaluation cycle.
const TYPE_WARN_INTERVAL: Duration = Duration::from_secs(5);

/// A monotonic-clock throttle so a persistently-mismatched rule warns at most
/// once per [`TYPE_WARN_INTERVAL`] rather than every cycle. Single-tasked with
/// the rest of the engine, so no locking.
#[derive(Debug, Default)]
struct WarnThrottle {
    last: Option<Instant>,
}

impl WarnThrottle {
    /// `true` (and arms the next window) if a warning is due at `now`.
    fn allow(&mut self, now: Instant) -> bool {
        match self.last {
            Some(t) if now.saturating_duration_since(t) < TYPE_WARN_INTERVAL => false,
            _ => {
                self.last = Some(now);
                true
            }
        }
    }
}

/// A condition's comparison operator (mirrors `write_rule_conditions.operator`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operator {
    Eq,
    Neq,
    Gt,
    Gte,
    Lt,
    Lte,
    Between,
    BitIs,
}

impl Operator {
    /// Parse the persisted string form. `None` for an unknown operator (a
    /// schema/CHECK-constraint bug, not a runtime condition).
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "eq" => Operator::Eq,
            "neq" => Operator::Neq,
            "gt" => Operator::Gt,
            "gte" => Operator::Gte,
            "lt" => Operator::Lt,
            "lte" => Operator::Lte,
            "between" => Operator::Between,
            "bit_is" => Operator::BitIs,
            _ => return None,
        })
    }

    /// Evaluate this operator against a confirmed *numeric/bit* source value.
    /// String comparison is handled separately (see [`condition_met`]) - a
    /// numeric operator never reaches a string value.
    fn eval_numeric(self, value: &PlcValue, threshold: f64, threshold_2: Option<f64>) -> bool {
        let v = plc_value_as_f64(value);
        match self {
            Operator::Eq => v == threshold,
            Operator::Neq => v != threshold,
            Operator::Gt => v > threshold,
            Operator::Gte => v >= threshold,
            Operator::Lt => v < threshold,
            Operator::Lte => v <= threshold,
            Operator::Between => threshold_2.is_some_and(|hi| v >= threshold && v <= hi),
            Operator::BitIs => plc_value_as_bool(value) == (threshold != 0.0),
        }
    }
}

/// A rule's edge-detection mode (mirrors `write_rules.edge_mode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeMode {
    Rising,
    Falling,
    Change,
}

impl EdgeMode {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "rising" => EdgeMode::Rising,
            "falling" => EdgeMode::Falling,
            "change" => EdgeMode::Change,
            _ => return None,
        })
    }

    /// Does a `prev` → `now` transition fire under this mode?
    fn fires(self, prev: bool, now: bool) -> bool {
        match self {
            EdgeMode::Rising => !prev && now,
            EdgeMode::Falling => prev && !now,
            EdgeMode::Change => prev != now,
        }
    }
}

/// What value a firing rule writes (mirrors `write_rules.write_value_mode` plus
/// its dependent column).
#[derive(Debug, Clone, PartialEq)]
pub enum ValueMode {
    /// Write a fixed numeric constant (`write_constant_value`).
    Constant(f64),
    /// Write a fixed string constant (`write_constant_text`, S2 文字列タグ) - a
    /// string write target's constant mode.
    ConstantText(String),
    /// Copy the current value of this source tag.
    CopyFromSource(i64),
}

/// One AND-combined condition of a compiled rule.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledCondition {
    pub source_tag_id: i64,
    pub operator: Operator,
    /// The numeric comparand (`0.0` and unused for a string condition, which
    /// compares [`Self::threshold_text`] instead).
    pub threshold: f64,
    pub threshold_2: Option<f64>,
    /// The text comparand for a STRING source condition (`eq`/`neq` only, S2
    /// 文字列タグ); `None` for a numeric condition.
    pub threshold_text: Option<String>,
}

/// A rule compiled to the shape the engine evaluates each cycle: enums instead
/// of strings, a resolved target data type, and its condition set. Built once at
/// engine start from the DB rows (see [`crate::engine`]).
#[derive(Debug, Clone)]
pub struct CompiledRule {
    pub rule_id: i64,
    pub rule_name: String,
    pub edge_mode: EdgeMode,
    /// `None` = no cooldown. Otherwise, after an emitted fire, further fires are
    /// suppressed for this long.
    pub cooldown: Option<Duration>,
    pub write_target_id: i64,
    /// The write target's wire shape, so the produced [`PlcValue`] matches it
    /// (a `bit` target gets a `Bit`, a string target a `Str`, everything else
    /// an `F64`).
    pub target_shape: WireShape,
    pub value_mode: ValueMode,
    pub conditions: Vec<CompiledCondition>,
}

/// A write *intent* emitted by [`RuleEngine::evaluate`]. Carries the value to
/// write plus a source snapshot for the audit row. NOT a write - the writer
/// still gates it.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingWrite {
    pub rule_id: i64,
    pub rule_name: String,
    pub write_target_id: i64,
    /// Representative source for the audit snapshot: the first condition's
    /// source tag and its confirmed value at fire time.
    pub source_tag_id: Option<i64>,
    /// The numeric source snapshot (`None` for a string source - its text goes
    /// in [`Self::source_text`] and the numeric audit column stays NULL).
    pub source_value: Option<f64>,
    /// The string source snapshot (S2 文字列タグ), `None` for a numeric source.
    /// The writer records it in the audit detail JSON.
    pub source_text: Option<String>,
    /// The value to write, already matched to the target's wire shape.
    pub value: PlcValue,
}

/// Per-rule remembered state between evaluations.
#[derive(Debug, Clone, Copy, Default)]
struct RuleState {
    /// The last determinate conditions-met value, or `None` if never yet
    /// observed (un-seeded).
    last_met: Option<bool>,
    /// When this rule last *emitted* a fire (for cooldown).
    last_fire: Option<Instant>,
}

/// Holds the compiled rules and their per-rule remembered state, and turns a
/// snapshot of the current-value cache into write intents.
pub struct RuleEngine {
    rules: Vec<CompiledRule>,
    state: HashMap<i64, RuleState>,
    /// Throttle for value-type-mismatch diagnostics (see [`WarnThrottle`]).
    warn: WarnThrottle,
}

impl RuleEngine {
    pub fn new(rules: Vec<CompiledRule>) -> Self {
        Self {
            rules,
            state: HashMap::new(),
            warn: WarnThrottle::default(),
        }
    }

    pub fn rule_count(&self) -> usize {
        self.rules.len()
    }

    /// Evaluate every rule against `cache` at `now`, returning the write intents
    /// whose edges fired this cycle (and are not cooldown-suppressed). Mutates
    /// each rule's remembered state.
    pub fn evaluate(&mut self, cache: &CurrentValues, now: Instant) -> Vec<PendingWrite> {
        let mut pending = Vec::new();
        for rule in &self.rules {
            let Some(met) = rule_met(rule, cache, &mut self.warn, now) else {
                // Indeterminate: leave remembered state untouched, do not fire.
                continue;
            };
            let entry = self.state.entry(rule.rule_id).or_default();
            let Some(prev) = entry.last_met else {
                // First determinate observation: seed only, never fire.
                entry.last_met = Some(met);
                continue;
            };

            if rule.edge_mode.fires(prev, met) {
                let cooling = match (rule.cooldown, entry.last_fire) {
                    (Some(cd), Some(last)) => now.saturating_duration_since(last) < cd,
                    _ => false,
                };
                if !cooling {
                    if let Some(write) = build_pending(rule, cache) {
                        pending.push(write);
                        entry.last_fire = Some(now);
                    }
                }
            }
            entry.last_met = Some(met);
        }
        pending
    }
}

/// A rule's determinate conditions-met value, or `None` (indeterminate) if any
/// source it needs is missing/`Bad` this cycle. All condition sources AND the
/// copy-from-source tag (if any) must be confirmed. A `Bad`-quality read is
/// already filtered out by [`CurrentValues::good_value`], so a string source
/// that could not be confirmed this cycle satisfies NEITHER `eq` nor `neq` -
/// the whole rule simply stays indeterminate.
fn rule_met(
    rule: &CompiledRule,
    cache: &CurrentValues,
    warn: &mut WarnThrottle,
    now: Instant,
) -> Option<bool> {
    let mut all = true;
    for c in &rule.conditions {
        let value = cache.good_value(c.source_tag_id)?;
        if !condition_met(c, &value, warn, now) {
            all = false;
        }
    }
    if let ValueMode::CopyFromSource(tag_id) = &rule.value_mode {
        // The value we would copy must be confirmed too, or we cannot fire.
        cache.good_value(*tag_id)?;
    }
    Some(all)
}

/// Evaluate one condition against a *confirmed* (Good-quality) source value.
///
/// - A string value compares by exact Unicode match (after S1's NUL-trim on
///   the read side) against `threshold_text`: `eq` is equality, `neq` is
///   inequality with a present comparand. Any numeric operator on a string
///   value is a save-time-prevented misconfiguration - it counts as *unmet*
///   (never a panic) and logs a rate-limited warning.
/// - A numeric/bit value uses the numeric operators exactly as before.
fn condition_met(
    cond: &CompiledCondition,
    value: &PlcValue,
    warn: &mut WarnThrottle,
    now: Instant,
) -> bool {
    match value {
        PlcValue::Str(s) => match cond.operator {
            Operator::Eq => cond.threshold_text.as_deref() == Some(s.as_str()),
            Operator::Neq => cond
                .threshold_text
                .as_deref()
                .is_some_and(|t| t != s.as_str()),
            _ => {
                if warn.allow(now) {
                    eprintln!(
                        "relay-wright engine: numeric operator on string source tag {} - condition treated as unmet",
                        cond.source_tag_id
                    );
                }
                false
            }
        },
        PlcValue::Bit(_) | PlcValue::F64(_) => {
            cond.operator
                .eval_numeric(value, cond.threshold, cond.threshold_2)
        }
    }
}

/// Build the [`PendingWrite`] for a rule that just fired. Returns `None` only if
/// a copy source vanished between [`rule_met`] and here, or a copy source's
/// type no longer matches the target's shape (both save-time-prevented and not
/// expected under single-threaded evaluation, but handled defensively rather
/// than by panicking).
fn build_pending(rule: &CompiledRule, cache: &CurrentValues) -> Option<PendingWrite> {
    let value: PlcValue = match &rule.value_mode {
        ValueMode::Constant(v) => match rule.target_shape {
            WireShape::Numeric(DataType::Bit) => PlcValue::Bit(*v != 0.0),
            WireShape::Numeric(_) => PlcValue::F64(*v),
            // A string target uses ConstantText; a numeric constant on it is
            // rejected at save time. Defensive - drop the fire.
            WireShape::Str { .. } => return None,
        },
        ValueMode::ConstantText(s) => PlcValue::Str(s.clone()),
        ValueMode::CopyFromSource(tag_id) => {
            let src = cache.good_value(*tag_id)?;
            match rule.target_shape {
                WireShape::Numeric(DataType::Bit) => PlcValue::Bit(plc_value_as_bool(&src)),
                WireShape::Numeric(_) => PlcValue::F64(plc_value_as_f64(&src)),
                // string→string passes the text through unchanged; a
                // non-string copy source into a string target is rejected at
                // save time (defensive drop here).
                WireShape::Str { .. } => match src {
                    PlcValue::Str(s) => PlcValue::Str(s),
                    _ => return None,
                },
            }
        }
    };

    // Snapshot the first condition's source for the audit row: a string source
    // fills `source_text` (the numeric column stays NULL), a numeric source
    // fills `source_value`.
    let (source_tag_id, source_value, source_text) = match rule.conditions.first() {
        Some(c) => match cache.good_value(c.source_tag_id) {
            Some(PlcValue::Str(s)) => (Some(c.source_tag_id), None, Some(s)),
            Some(other) => (Some(c.source_tag_id), Some(plc_value_as_f64(&other)), None),
            None => (Some(c.source_tag_id), None, None),
        },
        None => (None, None, None),
    };

    Some(PendingWrite {
        rule_id: rule.rule_id,
        rule_name: rule.rule_name.clone(),
        write_target_id: rule.write_target_id,
        source_tag_id,
        source_value,
        source_text,
        value,
    })
}

/// A [`PlcValue`] as `f64` (a bit becomes 1.0/0.0, a string 0.0 - callers never
/// ask this of a string). Used for numeric comparison and for the
/// `target_value_written`/`source_value_snapshot` audit columns.
pub fn plc_value_as_f64(value: &PlcValue) -> f64 {
    match value {
        PlcValue::F64(v) => *v,
        PlcValue::Bit(b) => {
            if *b {
                1.0
            } else {
                0.0
            }
        }
        PlcValue::Str(_) => 0.0,
    }
}

/// A [`PlcValue`] as `bool` (a nonzero number / non-empty string becomes
/// `true`).
fn plc_value_as_bool(value: &PlcValue) -> bool {
    match value {
        PlcValue::Bit(b) => *b,
        PlcValue::F64(v) => *v != 0.0,
        PlcValue::Str(s) => !s.is_empty(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cond(source_tag_id: i64, operator: Operator, threshold: f64) -> CompiledCondition {
        CompiledCondition {
            source_tag_id,
            operator,
            threshold,
            threshold_2: None,
            threshold_text: None,
        }
    }

    /// A string condition: `eq`/`neq` against `text`.
    fn str_cond(source_tag_id: i64, operator: Operator, text: &str) -> CompiledCondition {
        CompiledCondition {
            source_tag_id,
            operator,
            threshold: 0.0,
            threshold_2: None,
            threshold_text: Some(text.to_string()),
        }
    }

    fn rule(id: i64, edge_mode: EdgeMode, conditions: Vec<CompiledCondition>) -> CompiledRule {
        CompiledRule {
            rule_id: id,
            rule_name: format!("R{id}"),
            edge_mode,
            cooldown: None,
            write_target_id: 100 + id,
            target_shape: WireShape::Numeric(DataType::U16),
            value_mode: ValueMode::Constant(1.0),
            conditions,
        }
    }

    fn t0() -> Instant {
        Instant::now()
    }

    #[test]
    fn rising_edge_fires_exactly_once_and_not_while_held() {
        let cache = CurrentValues::new();
        let mut engine = RuleEngine::new(vec![rule(
            1,
            EdgeMode::Rising,
            vec![cond(1, Operator::Gt, 10.0)],
        )]);
        let now = t0();

        // Poll 1: below threshold -> seeds `false`, no fire.
        cache.set_good(1, PlcValue::F64(0.0), now);
        assert!(
            engine.evaluate(&cache, now).is_empty(),
            "first poll only seeds"
        );

        // Poll 2: crosses threshold -> rising edge, one write.
        cache.set_good(1, PlcValue::F64(20.0), now);
        let fired = engine.evaluate(&cache, now);
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].rule_id, 1);
        assert_eq!(fired[0].value, PlcValue::F64(1.0));
        assert_eq!(fired[0].source_tag_id, Some(1));
        assert_eq!(fired[0].source_value, Some(20.0));

        // Poll 3: still above threshold -> held true, no second fire.
        assert!(
            engine.evaluate(&cache, now).is_empty(),
            "a held-true condition must not re-fire"
        );
    }

    #[test]
    fn condition_clears_then_retriggers_gives_a_second_write() {
        let cache = CurrentValues::new();
        let mut engine = RuleEngine::new(vec![rule(
            1,
            EdgeMode::Rising,
            vec![cond(1, Operator::Gt, 10.0)],
        )]);
        let now = t0();

        cache.set_good(1, PlcValue::F64(0.0), now);
        engine.evaluate(&cache, now); // seed false
        cache.set_good(1, PlcValue::F64(20.0), now);
        assert_eq!(engine.evaluate(&cache, now).len(), 1, "first rising edge");
        cache.set_good(1, PlcValue::F64(0.0), now);
        assert!(
            engine.evaluate(&cache, now).is_empty(),
            "back below: no fire on rising mode"
        );
        cache.set_good(1, PlcValue::F64(20.0), now);
        assert_eq!(
            engine.evaluate(&cache, now).len(),
            1,
            "re-trigger fires again"
        );
    }

    #[test]
    fn falling_and_change_modes() {
        let cache = CurrentValues::new();
        let mut falling = RuleEngine::new(vec![rule(
            1,
            EdgeMode::Falling,
            vec![cond(1, Operator::Gt, 10.0)],
        )]);
        let mut change = RuleEngine::new(vec![rule(
            2,
            EdgeMode::Change,
            vec![cond(1, Operator::Gt, 10.0)],
        )]);
        let now = t0();

        cache.set_good(1, PlcValue::F64(20.0), now); // true
        assert!(falling.evaluate(&cache, now).is_empty(), "seed true");
        assert!(change.evaluate(&cache, now).is_empty(), "seed true");

        cache.set_good(1, PlcValue::F64(0.0), now); // -> false
        assert_eq!(
            falling.evaluate(&cache, now).len(),
            1,
            "falling fires on true->false"
        );
        assert_eq!(
            change.evaluate(&cache, now).len(),
            1,
            "change fires on any transition"
        );

        cache.set_good(1, PlcValue::F64(20.0), now); // -> true
        assert!(
            falling.evaluate(&cache, now).is_empty(),
            "falling must not fire on false->true"
        );
        assert_eq!(
            change.evaluate(&cache, now).len(),
            1,
            "change fires on the reverse transition too"
        );
    }

    #[test]
    fn and_conditions_fire_only_when_all_hold() {
        let cache = CurrentValues::new();
        let mut engine = RuleEngine::new(vec![rule(
            1,
            EdgeMode::Rising,
            vec![cond(1, Operator::Gt, 10.0), cond(2, Operator::Lt, 5.0)],
        )]);
        let now = t0();

        // Seed: only the first condition holds -> met=false.
        cache.set_good(1, PlcValue::F64(20.0), now);
        cache.set_good(2, PlcValue::F64(9.0), now);
        assert!(
            engine.evaluate(&cache, now).is_empty(),
            "seed false (2nd cond fails)"
        );

        // Second condition now also holds -> AND true -> rising edge.
        cache.set_good(2, PlcValue::F64(1.0), now);
        assert_eq!(
            engine.evaluate(&cache, now).len(),
            1,
            "fires only when both hold"
        );
    }

    #[test]
    fn indeterminate_source_never_fires_and_preserves_state() {
        let cache = CurrentValues::new();
        let mut engine = RuleEngine::new(vec![rule(
            1,
            EdgeMode::Falling,
            vec![cond(1, Operator::Gt, 10.0)],
        )]);
        let now = t0();

        cache.set_good(1, PlcValue::F64(20.0), now); // true
        engine.evaluate(&cache, now); // seed true

        // Source goes bad: must NOT manufacture a true->false falling edge.
        cache.mark_bad(1, now);
        assert!(
            engine.evaluate(&cache, now).is_empty(),
            "bad source is indeterminate, no fire"
        );

        // Recovers still-true: no transition happened while it was bad.
        cache.set_good(1, PlcValue::F64(20.0), now);
        assert!(
            engine.evaluate(&cache, now).is_empty(),
            "remembered state was preserved across the bad cycle"
        );
    }

    #[test]
    fn copy_from_source_uses_the_live_source_value() {
        let cache = CurrentValues::new();
        let mut r = rule(1, EdgeMode::Rising, vec![cond(1, Operator::Gt, 10.0)]);
        r.value_mode = ValueMode::CopyFromSource(2);
        let mut engine = RuleEngine::new(vec![r]);
        let now = t0();

        cache.set_good(1, PlcValue::F64(0.0), now);
        cache.set_good(2, PlcValue::F64(123.0), now);
        engine.evaluate(&cache, now); // seed false
        cache.set_good(1, PlcValue::F64(20.0), now); // trigger
        let fired = engine.evaluate(&cache, now);
        assert_eq!(fired.len(), 1);
        assert_eq!(
            fired[0].value,
            PlcValue::F64(123.0),
            "writes the copied source value"
        );
    }

    #[test]
    fn copy_from_source_missing_copy_tag_is_indeterminate() {
        let cache = CurrentValues::new();
        let mut r = rule(1, EdgeMode::Rising, vec![cond(1, Operator::Gt, 10.0)]);
        r.value_mode = ValueMode::CopyFromSource(2);
        let mut engine = RuleEngine::new(vec![r]);
        let now = t0();

        // Condition source present and trending up, but the copy source is
        // absent -> the whole rule stays indeterminate and never fires.
        cache.set_good(1, PlcValue::F64(0.0), now);
        engine.evaluate(&cache, now);
        cache.set_good(1, PlcValue::F64(20.0), now);
        assert!(
            engine.evaluate(&cache, now).is_empty(),
            "no copy value -> no fire"
        );
    }

    #[test]
    fn cooldown_suppresses_a_refire_within_the_window() {
        let cache = CurrentValues::new();
        let mut r = rule(1, EdgeMode::Rising, vec![cond(1, Operator::Gt, 10.0)]);
        r.cooldown = Some(Duration::from_secs(10));
        let mut engine = RuleEngine::new(vec![r]);
        let base = t0();

        cache.set_good(1, PlcValue::F64(0.0), base);
        engine.evaluate(&cache, base); // seed
        cache.set_good(1, PlcValue::F64(20.0), base);
        assert_eq!(engine.evaluate(&cache, base).len(), 1, "first fire");

        // Clear and re-trigger 3s later (within the 10s cooldown) -> suppressed.
        cache.set_good(1, PlcValue::F64(0.0), base + Duration::from_secs(1));
        engine.evaluate(&cache, base + Duration::from_secs(1));
        cache.set_good(1, PlcValue::F64(20.0), base + Duration::from_secs(3));
        assert!(
            engine
                .evaluate(&cache, base + Duration::from_secs(3))
                .is_empty(),
            "re-fire within cooldown is suppressed"
        );

        // Re-trigger past the cooldown -> allowed again.
        cache.set_good(1, PlcValue::F64(0.0), base + Duration::from_secs(12));
        engine.evaluate(&cache, base + Duration::from_secs(12));
        cache.set_good(1, PlcValue::F64(20.0), base + Duration::from_secs(15));
        assert_eq!(
            engine
                .evaluate(&cache, base + Duration::from_secs(15))
                .len(),
            1,
            "fire allowed once cooldown elapsed"
        );
    }

    #[test]
    fn between_and_bit_is_operators() {
        assert!(Operator::Between.eval_numeric(&PlcValue::F64(5.0), 1.0, Some(10.0)));
        assert!(!Operator::Between.eval_numeric(&PlcValue::F64(50.0), 1.0, Some(10.0)));
        assert!(
            !Operator::Between.eval_numeric(&PlcValue::F64(5.0), 1.0, None),
            "between needs an upper bound"
        );
        assert!(Operator::BitIs.eval_numeric(&PlcValue::Bit(true), 1.0, None));
        assert!(!Operator::BitIs.eval_numeric(&PlcValue::Bit(true), 0.0, None));
        assert!(Operator::BitIs.eval_numeric(&PlcValue::Bit(false), 0.0, None));
    }

    // --- S2 string sources --------------------------------------------------

    #[test]
    fn string_eq_fires_once_on_exact_match_and_not_while_held() {
        let cache = CurrentValues::new();
        let mut engine = RuleEngine::new(vec![rule(
            1,
            EdgeMode::Rising,
            vec![str_cond(1, Operator::Eq, "OK")],
        )]);
        let now = t0();

        // Seed with a non-matching value -> met=false, no fire.
        cache.set_good(1, PlcValue::Str("NG".to_string()), now);
        assert!(engine.evaluate(&cache, now).is_empty(), "first poll seeds");

        // Exact match -> rising edge, one fire, source snapshot is the text.
        cache.set_good(1, PlcValue::Str("OK".to_string()), now);
        let fired = engine.evaluate(&cache, now);
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].source_value, None, "string source: numeric NULL");
        assert_eq!(fired[0].source_text.as_deref(), Some("OK"));

        // Still matching -> held true, no second fire.
        assert!(
            engine.evaluate(&cache, now).is_empty(),
            "a held-true string match must not re-fire"
        );
    }

    #[test]
    fn string_neq_requires_a_present_comparand() {
        // neq is true only when the confirmed text differs from a set comparand.
        assert!(condition_met(
            &str_cond(1, Operator::Neq, "OK"),
            &PlcValue::Str("NG".to_string()),
            &mut WarnThrottle::default(),
            t0(),
        ));
        assert!(!condition_met(
            &str_cond(1, Operator::Neq, "OK"),
            &PlcValue::Str("OK".to_string()),
            &mut WarnThrottle::default(),
            t0(),
        ));
    }

    #[test]
    fn numeric_operator_on_a_string_value_is_unmet_not_a_panic() {
        // A gt condition (numeric) accidentally pointed at a string value must
        // count as unmet and never panic.
        let cond = CompiledCondition {
            source_tag_id: 1,
            operator: Operator::Gt,
            threshold: 10.0,
            threshold_2: None,
            threshold_text: None,
        };
        assert!(!condition_met(
            &cond,
            &PlcValue::Str("hello".to_string()),
            &mut WarnThrottle::default(),
            t0(),
        ));
    }

    #[test]
    fn string_copy_from_source_passes_the_text_through() {
        let cache = CurrentValues::new();
        let mut r = rule(1, EdgeMode::Rising, vec![str_cond(1, Operator::Eq, "GO")]);
        r.target_shape = WireShape::Str { words: 4 };
        r.value_mode = ValueMode::CopyFromSource(2);
        let mut engine = RuleEngine::new(vec![r]);
        let now = t0();

        cache.set_good(1, PlcValue::Str("NG".to_string()), now);
        cache.set_good(2, PlcValue::Str("PAYLOAD".to_string()), now);
        engine.evaluate(&cache, now); // seed false
        cache.set_good(1, PlcValue::Str("GO".to_string()), now); // trigger
        let fired = engine.evaluate(&cache, now);
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].value, PlcValue::Str("PAYLOAD".to_string()));
    }

    #[test]
    fn string_constant_text_is_written_verbatim() {
        let cache = CurrentValues::new();
        let mut r = rule(1, EdgeMode::Rising, vec![str_cond(1, Operator::Eq, "GO")]);
        r.target_shape = WireShape::Str { words: 4 };
        r.value_mode = ValueMode::ConstantText("STOP".to_string());
        let mut engine = RuleEngine::new(vec![r]);
        let now = t0();

        cache.set_good(1, PlcValue::Str("NG".to_string()), now);
        engine.evaluate(&cache, now); // seed false
        cache.set_good(1, PlcValue::Str("GO".to_string()), now);
        let fired = engine.evaluate(&cache, now);
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].value, PlcValue::Str("STOP".to_string()));
    }
}
