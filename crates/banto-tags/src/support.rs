//! Small helpers shared by [`crate::plc_connection`], [`crate::collection_group`],
//! and [`crate::tag`]'s service/validation code, factored out so the three
//! modules do not each re-derive the same field-error message strings and
//! constraint-violation mapping (mirrors the spirit of banto's
//! `apps/admin-template/core/src/items.rs`, which has these inline since it
//! is the only resource with this shape there - here there are three).

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
/// for the two constraint violations this crate's schema can hit: a UNIQUE
/// violation on `name` (every entity's `name` column is `UNIQUE`), and a
/// FOREIGN KEY violation on a parent id (`collection_groups.plc_connection_id`,
/// `tags.collection_group_id`). Anything else falls back to
/// `banto_storage::storage_error` (spec: "name一意（DB制約+わかりやすい
/// エラー）" - the DB constraint is the source of truth, this only makes its
/// error message presentable).
///
/// `fk_field`/`fk_message` are ignored (and can be anything) for entities
/// with no foreign key column, e.g. `plc_connections` - a FOREIGN KEY
/// violation can never occur there so that branch is simply unreachable in
/// practice.
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
