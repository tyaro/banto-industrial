//! [`StoreConfig`]: the configuration snapshot a data file's schema is
//! frozen from at creation time (design principle: "データファイル
//! （SQLite）は作成時にスキーマ確定・以後変更しない"). Deliberately
//! independent of `banto_tags` - this crate never reads the tag registry
//! (see this crate's `lib.rs` doc), so building a `StoreConfig` from
//! `banto_tags::Tag`/`CollectionGroup` rows is I3b's (the collection
//! engine's) job, not this crate's.

use std::collections::HashSet;
use std::hash::{Hash, Hasher};

use crate::error::TstoreError;

/// One tag's column definition within a [`GroupConfig`]. `key` is the
/// column's identity across `open()` calls (config-sameness hashing,
/// `compute_config_hash`) and what a later `TsReader` reports back
/// (`tstore_columns.tag_key`) - it is *not* the SQL column name (that is
/// always the positional `c1`, `c2`, ... `schema.rs` generates; see this
/// module's doc and `schema.rs`'s doc for why `key`/`tag_key` never appear
/// inside generated SQL identifiers).
///
/// `data_type` is a free-form label, not an enum: this crate stores every
/// value as SQL `REAL` regardless of the tag's original wire type (design
/// principle: "値はスケーリング適用後の工学値（REAL、bitは0/1）"), so
/// `data_type` is purely descriptive metadata for a later reader/display
/// layer (I4/R1). By convention I3b passes `banto_plc::DataType`/
/// `banto_tags::ALLOWED_DATA_TYPES` strings (`"bit" | "i16" | "u16" | "i32" |
/// "u32" | "f32"`) here, but this crate does not enforce that vocabulary -
/// enforcing it would be exactly the registry dependency this crate is
/// designed to avoid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagColumn {
    pub key: String,
    pub name: String,
    pub data_type: String,
    pub unit: Option<String>,
    pub decimals: u8,
}

/// One collection group's worth of columns, sharing one `samples_<n>` table
/// (design: "収集周期はタグ毎ではなく収集グループ毎" - `banto_tags`'s
/// `CollectionGroup`, mirrored here independently).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupConfig {
    pub key: String,
    pub name: String,
    pub period_ms: u32,
    pub tags: Vec<TagColumn>,
}

/// A full store configuration snapshot: every group a [`crate::writer::TsWriter`]
/// will accept `append`s for, in the fixed order that becomes each group's
/// `samples_<n>` table index and each tag's `c<i>` column position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreConfig {
    pub groups: Vec<GroupConfig>,
}

impl StoreConfig {
    /// Structural validation, independent of any existing file on disk
    /// (`open()`'s config-hash comparison against a *file's* recorded
    /// config is a separate, later step - see `writer.rs`). Checked here:
    /// at least one group, non-empty/unique `group_key`s, non-empty/unique
    /// `tag_key`s within each group, and `period_ms > 0`.
    pub(crate) fn validate(&self) -> Result<(), TstoreError> {
        if self.groups.is_empty() {
            return Err(TstoreError::Config(
                "StoreConfig には少なくとも1つのグループが必要です".to_string(),
            ));
        }

        let mut seen_group_keys: HashSet<&str> = HashSet::new();
        for group in &self.groups {
            let trimmed = group.key.trim();
            if trimmed.is_empty() {
                return Err(TstoreError::Config(
                    "グループの key は空にできません".to_string(),
                ));
            }
            if !seen_group_keys.insert(group.key.as_str()) {
                return Err(TstoreError::Config(format!(
                    "グループ key が重複しています: {}",
                    group.key
                )));
            }
            if group.period_ms == 0 {
                return Err(TstoreError::Config(format!(
                    "グループ {} の period_ms は0より大きい必要があります",
                    group.key
                )));
            }

            let mut seen_tag_keys: HashSet<&str> = HashSet::new();
            for tag in &group.tags {
                let trimmed_tag = tag.key.trim();
                if trimmed_tag.is_empty() {
                    return Err(TstoreError::Config(format!(
                        "グループ {} 内のタグ key は空にできません",
                        group.key
                    )));
                }
                if !seen_tag_keys.insert(tag.key.as_str()) {
                    return Err(TstoreError::Config(format!(
                        "グループ {} 内でタグ key が重複しています: {}",
                        group.key, tag.key
                    )));
                }
            }
        }

        Ok(())
    }
}

/// A short, deterministic fingerprint of `config`'s shape (group/tag `key`s,
/// names, types, units, decimals, `period_ms`, and - because it hashes the
/// `Vec`s in their given order - column order too), used to decide whether
/// an existing file's config equals a newly-requested one
/// ("構成の同一性判定: StoreConfig の正規化ハッシュ...で既存ファイルの
/// メタと比較"). Two `StoreConfig`s that are `==` always hash equal; the
/// converse (different configs producing the same hash) is only as unlikely
/// as any 64-bit hash collision.
///
/// Uses `std::collections::hash_map::DefaultHasher` (SipHash) rather than
/// pulling in a checksum/crypto-hash crate - it is already part of `std`, so
/// this needs no new dependency. Its one documented caveat is that the
/// algorithm is not guaranteed stable across Rust *toolchain* versions, so a
/// rebuild with a different compiler could in principle compute a different
/// hash for an unchanged `StoreConfig` and cause one spurious rotation
/// (a harmless extra `-NNN` file, not data loss or corruption) the first
/// time the upgraded binary opens an existing day's file. Accepted trade-off
/// given the alternative is an extra dependency for a property ("stable hash
/// across compiler versions") this crate does not actually need - see this
/// crate's completion report for this call being flagged explicitly as a
/// design judgment call.
pub(crate) fn compute_config_hash(config: &StoreConfig) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    config.groups.len().hash(&mut hasher);
    for group in &config.groups {
        group.key.hash(&mut hasher);
        group.name.hash(&mut hasher);
        group.period_ms.hash(&mut hasher);
        group.tags.len().hash(&mut hasher);
        for tag in &group.tags {
            tag.key.hash(&mut hasher);
            tag.name.hash(&mut hasher);
            tag.data_type.hash(&mut hasher);
            tag.unit.hash(&mut hasher);
            tag.decimals.hash(&mut hasher);
        }
    }
    format!("{:016x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tag(key: &str) -> TagColumn {
        TagColumn {
            key: key.to_string(),
            name: format!("Tag {key}"),
            data_type: "f32".to_string(),
            unit: Some("degC".to_string()),
            decimals: 1,
        }
    }

    fn group(key: &str, tags: Vec<TagColumn>) -> GroupConfig {
        GroupConfig {
            key: key.to_string(),
            name: format!("Group {key}"),
            period_ms: 1_000,
            tags,
        }
    }

    fn sample_config() -> StoreConfig {
        StoreConfig {
            groups: vec![
                group("g1", vec![tag("t1"), tag("t2")]),
                group("g2", vec![tag("t3")]),
            ],
        }
    }

    // --- validate ----------------------------------------------------

    #[test]
    fn validate_accepts_a_well_formed_config() {
        sample_config().validate().expect("should be valid");
    }

    #[test]
    fn validate_rejects_empty_group_list() {
        let config = StoreConfig { groups: vec![] };
        let err = config.validate().unwrap_err();
        assert!(matches!(err, TstoreError::Config(_)));
    }

    #[test]
    fn validate_rejects_empty_group_key() {
        let config = StoreConfig {
            groups: vec![group("   ", vec![tag("t1")])],
        };
        assert!(matches!(config.validate(), Err(TstoreError::Config(_))));
    }

    #[test]
    fn validate_rejects_duplicate_group_keys() {
        let config = StoreConfig {
            groups: vec![group("g1", vec![tag("t1")]), group("g1", vec![tag("t2")])],
        };
        assert!(matches!(config.validate(), Err(TstoreError::Config(_))));
    }

    #[test]
    fn validate_rejects_zero_period_ms() {
        let mut config = sample_config();
        config.groups[0].period_ms = 0;
        assert!(matches!(config.validate(), Err(TstoreError::Config(_))));
    }

    #[test]
    fn validate_rejects_empty_tag_key() {
        let config = StoreConfig {
            groups: vec![group("g1", vec![tag("  ")])],
        };
        assert!(matches!(config.validate(), Err(TstoreError::Config(_))));
    }

    #[test]
    fn validate_rejects_duplicate_tag_keys_within_a_group() {
        let config = StoreConfig {
            groups: vec![group("g1", vec![tag("t1"), tag("t1")])],
        };
        assert!(matches!(config.validate(), Err(TstoreError::Config(_))));
    }

    #[test]
    fn validate_allows_a_group_with_zero_tags() {
        let config = StoreConfig {
            groups: vec![group("g1", vec![])],
        };
        config.validate().expect("zero-tag group is allowed");
    }

    // --- compute_config_hash ------------------------------------------

    #[test]
    fn hash_is_stable_for_the_same_config() {
        let a = compute_config_hash(&sample_config());
        let b = compute_config_hash(&sample_config());
        assert_eq!(a, b);
    }

    #[test]
    fn hash_changes_when_a_tag_is_added() {
        let base = sample_config();
        let mut changed = sample_config();
        changed.groups[1].tags.push(tag("t4"));
        assert_ne!(compute_config_hash(&base), compute_config_hash(&changed));
    }

    #[test]
    fn hash_changes_when_tag_order_differs() {
        let base = sample_config();
        let mut reordered = sample_config();
        reordered.groups[0].tags.swap(0, 1);
        assert_ne!(compute_config_hash(&base), compute_config_hash(&reordered));
    }

    #[test]
    fn hash_changes_when_a_decimals_value_differs() {
        let base = sample_config();
        let mut changed = sample_config();
        changed.groups[0].tags[0].decimals = 3;
        assert_ne!(compute_config_hash(&base), compute_config_hash(&changed));
    }

    #[test]
    fn hash_changes_when_period_ms_differs() {
        let base = sample_config();
        let mut changed = sample_config();
        changed.groups[0].period_ms = 5_000;
        assert_ne!(compute_config_hash(&base), compute_config_hash(&changed));
    }
}
