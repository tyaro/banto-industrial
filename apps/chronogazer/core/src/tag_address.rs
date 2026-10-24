//! Shared optimistic-lock response for REST and Tauri tag updates.
//! Address validation belongs to the registry-backed `banto-tags` services.

use banto_core::BantoError;
use banto_tags::{Tag, TagService, TagUpdateError};

pub use crate::revision::{is_revision_conflict, REVISION_CONFLICT_FIELD};

/// Message shown when a tag changed in another client.
pub const REVISION_CONFLICT_MESSAGE: &str =
    "他の人（または別の画面）がこのタグを先に更新しました。一覧を再読み込みしてから、もう一度編集して保存してください。";

pub fn revision_conflict_error() -> BantoError {
    crate::revision::revision_conflict_error(REVISION_CONFLICT_MESSAGE)
}

/// Update a tag and preserve the structured revision-conflict response.
pub async fn update_tag_checked(
    tags: &TagService,
    id: i64,
    input: banto_tags::TagInput,
) -> Result<Tag, BantoError> {
    match tags.update_checked(id, input).await {
        Ok(tag) => Ok(tag),
        Err(TagUpdateError::RevisionConflict(_)) => Err(revision_conflict_error()),
        Err(TagUpdateError::Banto(err)) => Err(err),
    }
}
