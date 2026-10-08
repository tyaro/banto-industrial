//! 楽観ロック（`expectedRevision`）の食い違いの表し方を、タグ（#525）と表示
//! グループ（#393）で 1 つにそろえたもの。
//!
//! 食い違いは `BantoError::Validation` の `field_errors` に
//! [`REVISION_CONFLICT_FIELD`] を 1 つ載せて表す。REST はこれを `409 Conflict`
//! にし（本文は他の検証エラーと同じ `ErrorBody` 形。`rest.rs` の
//! `RevisionConflictRejection`）、Tauri は通常の検証エラーとして返すので、画面は
//! 両経路を同じ判定（TS 側は `#lib/banto/revisionConflict.ts`）で扱える。
//! 案内の文言だけは対象ごとに違う（「このタグ」「この表示グループ」）。

use banto_core::{BantoError, FieldError};

/// 版の食い違いのときにフォームへ出すフィールド名。
pub const REVISION_CONFLICT_FIELD: &str = "expectedRevision";

/// 版の食い違いを表すエラー。`message` は対象ごとの案内（画面にそのまま出る）。
pub fn revision_conflict_error(message: &str) -> BantoError {
    BantoError::Validation {
        field_errors: vec![FieldError {
            field: REVISION_CONFLICT_FIELD.to_string(),
            message: message.to_string(),
        }],
    }
}

/// [`revision_conflict_error`] が作ったエラーか（REST が `409` にするための判定）。
pub fn is_revision_conflict(err: &BantoError) -> bool {
    matches!(err, BantoError::Validation { field_errors }
        if field_errors.iter().any(|fe| fe.field == REVISION_CONFLICT_FIELD))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_conflict_error_is_recognised_and_others_are_not() {
        assert!(is_revision_conflict(&revision_conflict_error("x")));
        assert!(!is_revision_conflict(&BantoError::Validation {
            field_errors: vec![FieldError {
                field: "name".into(),
                message: "x".into(),
            }],
        }));
        assert!(!is_revision_conflict(&BantoError::Forbidden));
    }
}
