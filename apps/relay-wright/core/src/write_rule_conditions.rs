//! Write rule condition: one AND-combined threshold test within a
//! [`crate::write_rules::WriteRule`] (plan `luminous-discovering-goblet.md`,
//! W1/W2), backed by the `write_rule_conditions` table
//! (`migrations/0007_write_rule_conditions.sql`). A rule with N condition
//! rows requires ALL N to hold (no OR / free-form expression language,
//! `recorder-requirements.md` §7).
//!
//! Conditions have no independent top-level CRUD: they are always created,
//! read, and replaced together with their parent rule (the 1..N AND rows of
//! the rule form), so this module only owns the row type and the per-row
//! validation the aggregate [`crate::write_rules::WriteRuleService`] runs.
//! The threshold validation mirrors `banto_tags::tag`'s `validate_thresholds`
//! "compare only the set values, in order" style.

use banto_core::FieldError;
use serde::{Deserialize, Serialize};

/// Operators accepted in `write_rule_conditions.operator`. Mirrors the SQL
/// `CHECK` in `0007_write_rule_conditions.sql`; kept in Rust too so
/// [`validate_condition_input`] produces a friendly `FieldError` instead of a
/// raw SQLite CHECK violation. Change both together.
pub const ALLOWED_OPERATORS: &[&str] =
    &["eq", "neq", "gt", "gte", "lt", "lte", "between", "bit_is"];

/// A row of the `write_rule_conditions` table, wire-shaped (camelCase).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct WriteRuleCondition {
    pub id: i64,
    pub write_rule_id: i64,
    pub source_tag_id: i64,
    pub operator: String,
    pub threshold_value: f64,
    pub threshold_value_2: Option<f64>,
}

/// One condition row of a create/update rule payload (no `id`/`write_rule_id`
/// - those are assigned by the aggregate when it inserts the rows).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteRuleConditionInput {
    pub source_tag_id: i64,
    pub operator: String,
    pub threshold_value: f64,
    #[serde(default)]
    pub threshold_value_2: Option<f64>,
}

impl WriteRuleConditionInput {
    /// Is this the `between` operator, the one operator that uses
    /// `threshold_value_2` (its upper bound)? Every other operator leaves
    /// that column NULL (the aggregate forces it to `None` on insert).
    pub fn is_between(&self) -> bool {
        self.operator == "between"
    }
}

/// Field-name prefix so a per-row error points at the right row of the rule
/// form's 1..N condition list, e.g. `conditions.0.thresholdValue2`.
fn field(index: usize, name: &str) -> String {
    format!("conditions.{index}.{name}")
}

/// Validate one condition row, collecting EVERY violation (mirrors the
/// "report everything, not just the first" convention). `index` positions the
/// resulting `FieldError`s within the rule form's condition list.
///
/// Deferred to W3 (documented in the plan): whether `bit_is` is legal for the
/// referenced source tag, and whether an operator suits the tag's data type,
/// both need the source tag's TYPE, which lives in banto-tags' `tags` table
/// (cross-crate). What can be checked here without that - operator membership,
/// `between` bound ordering, and `bit_is`'s 0/1 constant - is checked; the
/// data-type-dependent rules wait for the W3 engine that resolves live tag
/// typing.
pub fn validate_condition_input(input: &WriteRuleConditionInput, index: usize) -> Vec<FieldError> {
    let mut errors: Vec<FieldError> = Vec::new();

    if !ALLOWED_OPERATORS.contains(&input.operator.as_str()) {
        errors.push(FieldError {
            field: field(index, "operator"),
            message: format!(
                "対応演算子は {} のいずれかです",
                ALLOWED_OPERATORS.join(", ")
            ),
        });
        // The remaining per-operator checks assume a known operator; bail on
        // this row to avoid confusing follow-on messages.
        return errors;
    }

    if input.is_between() {
        match input.threshold_value_2 {
            None => errors.push(FieldError {
                field: field(index, "thresholdValue2"),
                message: "between には上限値（2つ目のしきい値）が必要です".to_string(),
            }),
            // "compare only the set values, in order" - here both are always
            // set for `between`, so this is just lower <= upper.
            Some(upper) if input.threshold_value > upper => errors.push(FieldError {
                field: field(index, "thresholdValue2"),
                message: "上限値は下限値以上にしてください".to_string(),
            }),
            Some(_) => {}
        }
    }

    if input.operator == "bit_is" && input.threshold_value != 0.0 && input.threshold_value != 1.0 {
        errors.push(FieldError {
            field: field(index, "thresholdValue"),
            message: "bit_is のしきい値は 0 または 1 にしてください".to_string(),
        });
    }

    errors
}
