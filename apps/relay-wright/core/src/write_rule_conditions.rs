//! Write rule condition: one AND-combined threshold test within a
//! [`crate::write_rules::WriteRule`] (plan `luminous-discovering-goblet.md`,
//! W1/W2), backed by the `write_rule_conditions` table
//! (`migrations/0007_write_rule_conditions.sql` +
//! `0012_write_rule_conditions_threshold_text.sql`). A rule with N condition
//! rows requires ALL N to hold (no OR / free-form expression language,
//! `recorder-requirements.md` §7).
//!
//! Conditions have no independent top-level CRUD: they are always created,
//! read, and replaced together with their parent rule (the 1..N AND rows of
//! the rule form), so this module only owns the row type and the per-row
//! validation the aggregate [`crate::write_rules::WriteRuleService`] runs.
//! The threshold validation mirrors `banto_tags::tag`'s `validate_thresholds`
//! "compare only the set values, in order" style.
//!
//! ## S2 文字列タグ: type-dependent comparand
//!
//! A condition's comparand depends on its SOURCE TAG's data type, which lives
//! in banto-tags' `tags` table (cross-crate) - so the aggregate resolves the
//! tag at save time (the same lookup its write-cycle check already performs)
//! and passes the resolved [`SourceTagKind`] into
//! [`validate_condition_input`]:
//!
//! - **numeric/bit source**: any of the 8 operators; `threshold_value`
//!   required (`threshold_value_2` for `between`); `threshold_text` must be
//!   absent.
//! - **string source**: operator must be `eq`/`neq`; `threshold_text`
//!   required (non-empty, Shift-JIS-encodable, encoded bytes ≤
//!   2 × the tag's `string_length` - a comparand the device can never hold
//!   would make the condition permanently false, so it is rejected at save
//!   time); the numeric threshold columns must be absent. Comparison
//!   semantics at runtime are the S1 recommendation: exact Unicode match
//!   after the read side's NUL-trim - no space-trim, no normalization.
//!
//! When the source tag cannot be resolved at all (`kind == None`, e.g. a
//! dangling id), only the type-independent checks run - the aggregate
//! reports the missing tag itself.

use banto_core::FieldError;
use serde::{Deserialize, Serialize};

use crate::support::sjis_text_error;

/// Operators accepted in `write_rule_conditions.operator`. Mirrors the SQL
/// `CHECK` in `0007_write_rule_conditions.sql`; kept in Rust too so
/// [`validate_condition_input`] produces a friendly `FieldError` instead of a
/// raw SQLite CHECK violation. Change both together.
pub const ALLOWED_OPERATORS: &[&str] =
    &["eq", "neq", "gt", "gte", "lt", "lte", "between", "bit_is"];

/// The operator subset legal for a STRING source tag (S2): exact match /
/// exact mismatch only - ordering comparisons over SJIS text are meaningless.
pub const STRING_OPERATORS: &[&str] = &["eq", "neq"];

/// A condition source tag's resolved shape, as far as validation cares: which
/// comparand family applies, and (for strings) the byte budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SourceTagKind {
    /// Any of the numeric/bit data types.
    Numeric,
    /// A string tag with its registered `string_length` (words).
    Str { length: i64 },
}

/// A row of the `write_rule_conditions` table, wire-shaped (camelCase).
/// Exactly one comparand side is populated: `threshold_value`
/// (+`threshold_value_2` for `between`) for a numeric source,
/// `threshold_text` for a string source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct WriteRuleCondition {
    pub id: i64,
    pub write_rule_id: i64,
    pub source_tag_id: i64,
    pub operator: String,
    pub threshold_value: Option<f64>,
    pub threshold_value_2: Option<f64>,
    pub threshold_text: Option<String>,
}

/// One condition row of a create/update rule payload (no `id`/`write_rule_id`
/// - those are assigned by the aggregate when it inserts the rows).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteRuleConditionInput {
    pub source_tag_id: i64,
    pub operator: String,
    #[serde(default)]
    pub threshold_value: Option<f64>,
    #[serde(default)]
    pub threshold_value_2: Option<f64>,
    #[serde(default)]
    pub threshold_text: Option<String>,
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
/// resulting `FieldError`s within the rule form's condition list;
/// `source_kind` is the save-time-resolved shape of the row's source tag
/// (`None` = unresolved, so only the type-independent checks run - see the
/// module doc comment).
pub(crate) fn validate_condition_input(
    input: &WriteRuleConditionInput,
    index: usize,
    source_kind: Option<SourceTagKind>,
) -> Vec<FieldError> {
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

    match source_kind {
        None => {}
        Some(SourceTagKind::Numeric) => {
            if input.threshold_text.is_some() {
                errors.push(FieldError {
                    field: field(index, "thresholdText"),
                    message: "数値タグの条件にはテキストしきい値は設定できません".to_string(),
                });
            }
            if input.threshold_value.is_none() {
                errors.push(FieldError {
                    field: field(index, "thresholdValue"),
                    message: "必須項目です".to_string(),
                });
            }
            if input.is_between() {
                match (input.threshold_value, input.threshold_value_2) {
                    (_, None) => errors.push(FieldError {
                        field: field(index, "thresholdValue2"),
                        message: "between には上限値（2つ目のしきい値）が必要です".to_string(),
                    }),
                    // "compare only the set values, in order".
                    (Some(lower), Some(upper)) if lower > upper => errors.push(FieldError {
                        field: field(index, "thresholdValue2"),
                        message: "上限値は下限値以上にしてください".to_string(),
                    }),
                    _ => {}
                }
            }
            if input.operator == "bit_is"
                && input.threshold_value.is_some_and(|v| v != 0.0 && v != 1.0)
            {
                errors.push(FieldError {
                    field: field(index, "thresholdValue"),
                    message: "bit_is のしきい値は 0 または 1 にしてください".to_string(),
                });
            }
        }
        Some(SourceTagKind::Str { length }) => {
            if !STRING_OPERATORS.contains(&input.operator.as_str()) {
                errors.push(FieldError {
                    field: field(index, "operator"),
                    message: format!(
                        "文字列タグの条件で使える演算子は {} のみです",
                        STRING_OPERATORS.join(", ")
                    ),
                });
            }
            if input.threshold_value.is_some() || input.threshold_value_2.is_some() {
                errors.push(FieldError {
                    field: field(index, "thresholdValue"),
                    message: "文字列タグの条件には数値しきい値は設定できません".to_string(),
                });
            }
            match input.threshold_text.as_deref() {
                None | Some("") => errors.push(FieldError {
                    field: field(index, "thresholdText"),
                    message: "必須項目です".to_string(),
                }),
                Some(text) => {
                    if let Some(message) = sjis_text_error(text, length) {
                        errors.push(FieldError {
                            field: field(index, "thresholdText"),
                            message,
                        });
                    }
                }
            }
        }
    }

    errors
}
