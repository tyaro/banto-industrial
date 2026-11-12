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

/// 接続名・グループ名に `.` を使えない規則（2026-11-12 オーナー決定、#635 の続き）。
/// Hub の外部名は `接続.グループ.タグ` なので、接続名・グループ名に `.` が無ければ
/// 外部名は最初の 2 つの `.` で一意に分けられる（タグ名の `.` は引き続き使える）。
///
/// `stored_name` は更新で上書きされる行の今の名前（作成は `None`）。名前を変えない
/// 更新は、この規則より前に作られたドットを含む名前のままでも通す（既存の DB を
/// 拒否しない）。検査は前後の空白を除いた名前（保存される値）で行う。
pub(crate) fn dotted_name_error(
    name: &str,
    stored_name: Option<&str>,
    label: &str,
) -> Option<FieldError> {
    let trimmed = name.trim();
    if !trimmed.contains('.') || stored_name.is_some_and(|stored| stored.trim() == trimmed) {
        return None;
    }
    Some(FieldError {
        field: "name".to_string(),
        message: format!(
            "{label}に「.」は使えません（外部名「接続.グループ.タグ」の区切りに使うため）"
        ),
    })
}

/// [`dotted_name_error`] の `label`（接続）。
pub(crate) const CONNECTION_NAME_LABEL: &str = "接続名";
/// [`dotted_name_error`] の `label`（収集グループ）。
pub(crate) const GROUP_NAME_LABEL: &str = "グループ名";

/// UNIQUE-violation message for entities whose `name` stays globally unique
/// (`plc_connections` only, since migration 0022 made `collection_groups.name`
/// unique per connection - that module has its own scoped message,
/// `NAME_ALREADY_USED_IN_CONNECTION`, 2026-10-19 オーナー決定) - unchanged
/// text, kept here as a named constant now that [`map_write_error`] takes the
/// message as a parameter instead of hard-coding it.
pub(crate) const NAME_ALREADY_USED: &str = "既に使用されています";

/// Map a write-time `sqlx::Error` into a friendly `BantoError::Validation`
/// for the two constraint violations this crate's schema can hit: a UNIQUE
/// violation on `name` (`plc_connections.name` is globally `UNIQUE`;
/// `collection_groups.name` is `UNIQUE(plc_connection_id, name)` as of
/// migration 0022 - 接続内一意, 2026-10-19 オーナー決定 - and `tags.name` is
/// `UNIQUE(collection_group_id, name)` as of migration 0011 - グループ内一意,
/// 2026-08-31 オーナー決定 - so those two callers pass a message that says so
/// instead of the generic one), and a
/// FOREIGN KEY violation on a parent id (`collection_groups.plc_connection_id`,
/// `tags.collection_group_id`). Anything else falls back to
/// `banto_storage::storage_error` (spec: "name一意（DB制約+わかりやすい
/// エラー）" - the DB constraint is the source of truth, this only makes its
/// error message presentable).
///
/// `unique_message` lets each caller phrase the UNIQUE-violation message to
/// match its own uniqueness scope (`tags` is group-scoped, `collection_groups`
/// connection-scoped, `plc_connections` global) without this shared helper
/// hard-coding any of them.
///
/// `fk_field`/`fk_message` are ignored (and can be anything) for entities
/// with no foreign key column, e.g. `plc_connections` - a FOREIGN KEY
/// violation can never occur there so that branch is simply unreachable in
/// practice.
pub(crate) fn map_write_error(
    err: sqlx::Error,
    unique_field: &str,
    unique_message: &str,
    fk_field: &str,
    fk_message: &str,
) -> BantoError {
    if let Some(db_err) = err.as_database_error() {
        if db_err.is_unique_violation() {
            return BantoError::Validation {
                field_errors: vec![FieldError {
                    field: unique_field.to_string(),
                    message: unique_message.to_string(),
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
