//! Small validation/error helpers shared by this app's own registry/rule
//! service modules ([`crate::write_targets`], [`crate::write_rules`],
//! [`crate::write_rule_conditions`]), factored out so they do not each
//! re-derive the same field-error message strings and constraint-violation
//! mapping.
//!
//! These deliberately DUPLICATE `banto_tags`'s crate-internal `support`
//! module (`crates/banto-tags/src/support.rs`) rather than importing it:
//! that module is `pub(crate)` there (not part of banto-tags' public API on
//! purpose), so it cannot be reached from this crate. The strings are kept
//! byte-for-byte identical to banto-tags' so the two apps' validation
//! messages read the same to an operator who uses both.

use banto_core::{BantoError, FieldError};

pub(crate) fn required_message() -> String {
    "必須項目です".to_string()
}

pub(crate) fn max_length_message(max: usize) -> String {
    format!("{max}文字以内で入力してください")
}

pub(crate) fn range_message(min: i64, max: i64) -> String {
    format!("{min}〜{max}の範囲で入力してください")
}

/// Map a write-time `sqlx::Error` into a friendly `BantoError::Validation`
/// for the two constraint violations this app's write_* schema can hit: a
/// UNIQUE violation on `name` (every entity's `name` column is `UNIQUE`), and
/// a FOREIGN KEY violation on an in-lineage parent id
/// (`write_rules.write_target_id`, `write_rule_conditions.write_rule_id`).
/// Anything else falls back to `banto_storage::storage_error`.
///
/// Mirrors `banto_tags::support::map_write_error` field-for-field. Note that
/// the cross-migrator-lineage references (`write_targets.plc_connection_id`,
/// `write_rules.write_source_tag_id`, `write_rule_conditions.source_tag_id`)
/// are NOT real SQL FOREIGN KEYs (see `migrations/0005_write_targets.sql`'s
/// doc comment), so this helper never fires for them - those are validated
/// explicitly at the service layer instead.
pub(crate) fn map_write_error(
    err: sqlx::Error,
    unique_field: &str,
    fk_field: &str,
    fk_message: &str,
) -> BantoError {
    if let Some(db_err) = err.as_database_error() {
        if db_err.is_unique_violation() {
            return BantoError::Validation {
                field_errors: vec![FieldError {
                    field: unique_field.to_string(),
                    message: "既に使用されています".to_string(),
                }],
            };
        }
        if db_err.is_foreign_key_violation() {
            return BantoError::Validation {
                field_errors: vec![FieldError {
                    field: fk_field.to_string(),
                    message: fk_message.to_string(),
                }],
            };
        }
    }
    banto_storage::storage_error(err)
}
