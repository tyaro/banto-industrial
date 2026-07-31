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

use banto_plc::{DataType, TagValue};

use super::current_values::CurrentValues;

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

    /// Evaluate this operator against a confirmed source value.
    fn eval(self, value: TagValue, threshold: f64, threshold_2: Option<f64>) -> bool {
        let v = tag_value_as_f64(value);
        match self {
            Operator::Eq => v == threshold,
            Operator::Neq => v != threshold,
            Operator::Gt => v > threshold,
            Operator::Gte => v >= threshold,
            Operator::Lt => v < threshold,
            Operator::Lte => v <= threshold,
            Operator::Between => threshold_2.is_some_and(|hi| v >= threshold && v <= hi),
            Operator::BitIs => tag_value_as_bool(value) == (threshold != 0.0),
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
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ValueMode {
    /// Write a fixed constant.
    Constant(f64),
    /// Copy the current value of this source tag.
    CopyFromSource(i64),
}

/// One AND-combined condition of a compiled rule.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CompiledCondition {
    pub source_tag_id: i64,
    pub operator: Operator,
    pub threshold: f64,
    pub threshold_2: Option<f64>,
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
    /// The write target's data type, so the produced [`TagValue`] matches the
    /// wire type (a `bit` target gets a `Bit`, everything else an `F64`).
    pub target_data_type: DataType,
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
    pub source_value: Option<f64>,
    /// The value to write, already matched to the target's data type.
    pub value: TagValue,
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
}

impl RuleEngine {
    pub fn new(rules: Vec<CompiledRule>) -> Self {
        Self {
            rules,
            state: HashMap::new(),
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
            let Some(met) = rule_met(rule, cache) else {
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
/// copy-from-source tag (if any) must be confirmed.
fn rule_met(rule: &CompiledRule, cache: &CurrentValues) -> Option<bool> {
    let mut all = true;
    for c in &rule.conditions {
        let value = cache.good_value(c.source_tag_id)?;
        if !c.operator.eval(value, c.threshold, c.threshold_2) {
            all = false;
        }
    }
    if let ValueMode::CopyFromSource(tag_id) = rule.value_mode {
        // The value we would copy must be confirmed too, or we cannot fire.
        cache.good_value(tag_id)?;
    }
    Some(all)
}

/// Build the [`PendingWrite`] for a rule that just fired. Returns `None` only if
/// a copy source vanished between [`rule_met`] and here (not expected, since
/// evaluation is single-threaded, but handled defensively).
fn build_pending(rule: &CompiledRule, cache: &CurrentValues) -> Option<PendingWrite> {
    let value = match rule.value_mode {
        ValueMode::Constant(v) => value_for_target(v, rule.target_data_type),
        ValueMode::CopyFromSource(tag_id) => {
            let src = cache.good_value(tag_id)?;
            match rule.target_data_type {
                DataType::Bit => TagValue::Bit(tag_value_as_bool(src)),
                _ => TagValue::F64(tag_value_as_f64(src)),
            }
        }
    };

    // Snapshot the first condition's source for the audit row.
    let (source_tag_id, source_value) = match rule.conditions.first() {
        Some(c) => (
            Some(c.source_tag_id),
            cache.good_value(c.source_tag_id).map(tag_value_as_f64),
        ),
        None => (None, None),
    };

    Some(PendingWrite {
        rule_id: rule.rule_id,
        rule_name: rule.rule_name.clone(),
        write_target_id: rule.write_target_id,
        source_tag_id,
        source_value,
        value,
    })
}

/// Coerce a raw numeric write value to the target's wire type.
fn value_for_target(raw: f64, data_type: DataType) -> TagValue {
    match data_type {
        DataType::Bit => TagValue::Bit(raw != 0.0),
        _ => TagValue::F64(raw),
    }
}

/// A [`TagValue`] as `f64` (a bit becomes 1.0/0.0). Used for numeric comparison
/// and for the `target_value_written`/`source_value_snapshot` audit columns.
pub fn tag_value_as_f64(value: TagValue) -> f64 {
    match value {
        TagValue::F64(v) => v,
        TagValue::Bit(b) => {
            if b {
                1.0
            } else {
                0.0
            }
        }
    }
}

/// A [`TagValue`] as `bool` (a nonzero number becomes `true`).
fn tag_value_as_bool(value: TagValue) -> bool {
    match value {
        TagValue::Bit(b) => b,
        TagValue::F64(v) => v != 0.0,
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
        }
    }

    fn rule(id: i64, edge_mode: EdgeMode, conditions: Vec<CompiledCondition>) -> CompiledRule {
        CompiledRule {
            rule_id: id,
            rule_name: format!("R{id}"),
            edge_mode,
            cooldown: None,
            write_target_id: 100 + id,
            target_data_type: DataType::U16,
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
        let mut engine = RuleEngine::new(vec![rule(1, EdgeMode::Rising, vec![cond(1, Operator::Gt, 10.0)])]);
        let now = t0();

        // Poll 1: below threshold -> seeds `false`, no fire.
        cache.set_good(1, TagValue::F64(0.0), now);
        assert!(engine.evaluate(&cache, now).is_empty(), "first poll only seeds");

        // Poll 2: crosses threshold -> rising edge, one write.
        cache.set_good(1, TagValue::F64(20.0), now);
        let fired = engine.evaluate(&cache, now);
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].rule_id, 1);
        assert_eq!(fired[0].value, TagValue::F64(1.0));
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
        let mut engine = RuleEngine::new(vec![rule(1, EdgeMode::Rising, vec![cond(1, Operator::Gt, 10.0)])]);
        let now = t0();

        cache.set_good(1, TagValue::F64(0.0), now);
        engine.evaluate(&cache, now); // seed false
        cache.set_good(1, TagValue::F64(20.0), now);
        assert_eq!(engine.evaluate(&cache, now).len(), 1, "first rising edge");
        cache.set_good(1, TagValue::F64(0.0), now);
        assert!(engine.evaluate(&cache, now).is_empty(), "back below: no fire on rising mode");
        cache.set_good(1, TagValue::F64(20.0), now);
        assert_eq!(engine.evaluate(&cache, now).len(), 1, "re-trigger fires again");
    }

    #[test]
    fn falling_and_change_modes() {
        let cache = CurrentValues::new();
        let mut falling =
            RuleEngine::new(vec![rule(1, EdgeMode::Falling, vec![cond(1, Operator::Gt, 10.0)])]);
        let mut change =
            RuleEngine::new(vec![rule(2, EdgeMode::Change, vec![cond(1, Operator::Gt, 10.0)])]);
        let now = t0();

        cache.set_good(1, TagValue::F64(20.0), now); // true
        assert!(falling.evaluate(&cache, now).is_empty(), "seed true");
        assert!(change.evaluate(&cache, now).is_empty(), "seed true");

        cache.set_good(1, TagValue::F64(0.0), now); // -> false
        assert_eq!(falling.evaluate(&cache, now).len(), 1, "falling fires on true->false");
        assert_eq!(change.evaluate(&cache, now).len(), 1, "change fires on any transition");

        cache.set_good(1, TagValue::F64(20.0), now); // -> true
        assert!(
            falling.evaluate(&cache, now).is_empty(),
            "falling must not fire on false->true"
        );
        assert_eq!(change.evaluate(&cache, now).len(), 1, "change fires on the reverse transition too");
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
        cache.set_good(1, TagValue::F64(20.0), now);
        cache.set_good(2, TagValue::F64(9.0), now);
        assert!(engine.evaluate(&cache, now).is_empty(), "seed false (2nd cond fails)");

        // Second condition now also holds -> AND true -> rising edge.
        cache.set_good(2, TagValue::F64(1.0), now);
        assert_eq!(engine.evaluate(&cache, now).len(), 1, "fires only when both hold");
    }

    #[test]
    fn indeterminate_source_never_fires_and_preserves_state() {
        let cache = CurrentValues::new();
        let mut engine = RuleEngine::new(vec![rule(1, EdgeMode::Falling, vec![cond(1, Operator::Gt, 10.0)])]);
        let now = t0();

        cache.set_good(1, TagValue::F64(20.0), now); // true
        engine.evaluate(&cache, now); // seed true

        // Source goes bad: must NOT manufacture a true->false falling edge.
        cache.mark_bad(1, now);
        assert!(engine.evaluate(&cache, now).is_empty(), "bad source is indeterminate, no fire");

        // Recovers still-true: no transition happened while it was bad.
        cache.set_good(1, TagValue::F64(20.0), now);
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

        cache.set_good(1, TagValue::F64(0.0), now);
        cache.set_good(2, TagValue::F64(123.0), now);
        engine.evaluate(&cache, now); // seed false
        cache.set_good(1, TagValue::F64(20.0), now); // trigger
        let fired = engine.evaluate(&cache, now);
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].value, TagValue::F64(123.0), "writes the copied source value");
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
        cache.set_good(1, TagValue::F64(0.0), now);
        engine.evaluate(&cache, now);
        cache.set_good(1, TagValue::F64(20.0), now);
        assert!(engine.evaluate(&cache, now).is_empty(), "no copy value -> no fire");
    }

    #[test]
    fn cooldown_suppresses_a_refire_within_the_window() {
        let cache = CurrentValues::new();
        let mut r = rule(1, EdgeMode::Rising, vec![cond(1, Operator::Gt, 10.0)]);
        r.cooldown = Some(Duration::from_secs(10));
        let mut engine = RuleEngine::new(vec![r]);
        let base = t0();

        cache.set_good(1, TagValue::F64(0.0), base);
        engine.evaluate(&cache, base); // seed
        cache.set_good(1, TagValue::F64(20.0), base);
        assert_eq!(engine.evaluate(&cache, base).len(), 1, "first fire");

        // Clear and re-trigger 3s later (within the 10s cooldown) -> suppressed.
        cache.set_good(1, TagValue::F64(0.0), base + Duration::from_secs(1));
        engine.evaluate(&cache, base + Duration::from_secs(1));
        cache.set_good(1, TagValue::F64(20.0), base + Duration::from_secs(3));
        assert!(
            engine.evaluate(&cache, base + Duration::from_secs(3)).is_empty(),
            "re-fire within cooldown is suppressed"
        );

        // Re-trigger past the cooldown -> allowed again.
        cache.set_good(1, TagValue::F64(0.0), base + Duration::from_secs(12));
        engine.evaluate(&cache, base + Duration::from_secs(12));
        cache.set_good(1, TagValue::F64(20.0), base + Duration::from_secs(15));
        assert_eq!(
            engine.evaluate(&cache, base + Duration::from_secs(15)).len(),
            1,
            "fire allowed once cooldown elapsed"
        );
    }

    #[test]
    fn between_and_bit_is_operators() {
        assert!(Operator::Between.eval(TagValue::F64(5.0), 1.0, Some(10.0)));
        assert!(!Operator::Between.eval(TagValue::F64(50.0), 1.0, Some(10.0)));
        assert!(!Operator::Between.eval(TagValue::F64(5.0), 1.0, None), "between needs an upper bound");
        assert!(Operator::BitIs.eval(TagValue::Bit(true), 1.0, None));
        assert!(!Operator::BitIs.eval(TagValue::Bit(true), 0.0, None));
        assert!(Operator::BitIs.eval(TagValue::Bit(false), 0.0, None));
    }
}
