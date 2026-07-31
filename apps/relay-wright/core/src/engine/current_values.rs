//! The current-value cache (W3-B, `luminous-discovering-goblet.md`): a small
//! concurrent map from source-tag id to that tag's latest reading, written by
//! the [`crate::engine::poller`] and read by the [`crate::engine::rule_engine`].
//!
//! ## Why a `tag_id` key (not `(connection_id, tag_id)`)
//!
//! A `banto_tags::Tag` id is globally unique across the whole registry, so a
//! tag id alone already identifies both the tag and (transitively, via its
//! collection group) its PLC connection. Keying on the tag id keeps the read
//! path a single `HashMap` lookup for the rule engine, which only ever knows a
//! condition's `source_tag_id`. The connection a tag lives on matters only to
//! the poller (which groups reads per connection) and is carried in the
//! poller's own resolved-source list, not here.
//!
//! ## Quality, and why a stale `Good` is downgraded rather than kept
//!
//! Each entry carries a [`Quality`]. A read that comes back `Bad`, or a whole
//! connection that is down for a cycle, downgrades the tag's existing entry to
//! [`Quality::Bad`] (keeping the last value only as history) rather than
//! leaving a stale `Good` in place - the rule engine treats a missing OR `Bad`
//! source as *indeterminate* and refuses to fire on it (see
//! [`crate::engine::rule_engine`]). This is deliberate: a condition must never
//! transition (and therefore never trigger a write) on the strength of a value
//! the poller could not actually confirm this cycle.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Instant;

use banto_plc::TagValue;

/// Whether a cached reading reflects a value the poller actually confirmed this
/// cycle ([`Quality::Good`]) or a failed/absent read ([`Quality::Bad`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quality {
    Good,
    Bad,
}

/// One tag's latest cached reading.
#[derive(Debug, Clone, Copy)]
pub struct CachedValue {
    /// The most recent value seen. For a [`Quality::Bad`] entry this is the
    /// last *good* value (kept only as history); the rule engine ignores it.
    pub value: TagValue,
    pub quality: Quality,
    /// When this entry was last written (monotonic). Not used for staleness
    /// enforcement in W3-B1 (deferred to W5); kept for diagnostics/UI.
    pub at: Instant,
}

/// A cheap-to-clone handle to the shared current-value map. All clones point at
/// the same underlying store (an `Arc<RwLock<..>>`); the poller holds one and
/// writes, the rule engine holds one and reads.
#[derive(Clone, Default)]
pub struct CurrentValues {
    inner: Arc<RwLock<HashMap<i64, CachedValue>>>,
}

impl CurrentValues {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a freshly-read good value for `tag_id`.
    pub fn set_good(&self, tag_id: i64, value: TagValue, at: Instant) {
        self.inner.write().expect("current-values lock poisoned").insert(
            tag_id,
            CachedValue {
                value,
                quality: Quality::Good,
                at,
            },
        );
    }

    /// Downgrade `tag_id` to [`Quality::Bad`] (a failed read this cycle). If the
    /// tag has no entry yet this is a no-op - an absent tag already reads as
    /// indeterminate, so there is nothing to invalidate.
    pub fn mark_bad(&self, tag_id: i64, at: Instant) {
        if let Some(entry) = self
            .inner
            .write()
            .expect("current-values lock poisoned")
            .get_mut(&tag_id)
        {
            entry.quality = Quality::Bad;
            entry.at = at;
        }
    }

    /// The raw cached entry for `tag_id`, if any (quality included).
    pub fn get(&self, tag_id: i64) -> Option<CachedValue> {
        self.inner
            .read()
            .expect("current-values lock poisoned")
            .get(&tag_id)
            .copied()
    }

    /// The tag's value **only if it is currently `Good`** - the exact question
    /// the rule engine asks. `None` for a missing or `Bad` tag (both
    /// indeterminate).
    pub fn good_value(&self, tag_id: i64) -> Option<TagValue> {
        self.get(tag_id).and_then(|entry| match entry.quality {
            Quality::Good => Some(entry.value),
            Quality::Bad => None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn good_value_is_visible_then_invalidated_by_mark_bad() {
        let cache = CurrentValues::new();
        let now = Instant::now();
        assert_eq!(cache.good_value(1), None, "absent tag is indeterminate");

        cache.set_good(1, TagValue::F64(42.0), now);
        assert_eq!(cache.good_value(1), Some(TagValue::F64(42.0)));

        cache.mark_bad(1, now);
        assert_eq!(cache.good_value(1), None, "bad tag is indeterminate");
        // The last value is retained as history but no longer surfaced.
        assert!(matches!(
            cache.get(1),
            Some(CachedValue {
                value: TagValue::F64(v),
                quality: Quality::Bad,
                ..
            }) if v == 42.0
        ));
    }

    #[test]
    fn mark_bad_on_absent_tag_is_a_noop() {
        let cache = CurrentValues::new();
        cache.mark_bad(99, Instant::now());
        assert!(cache.get(99).is_none());
    }
}
