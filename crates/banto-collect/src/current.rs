//! [`CurrentValuesHandle`]: the live "latest value per tag" cache the R1
//! digital/bar/gauge widgets and health display read
//! (recorder-requirements.md §3.2). One shared, cheaply-cloneable handle that
//! the collection tasks write and the UI reads.
//!
//! ## Quality: Good/Bad stored, Stale derived
//!
//! Each write records [`Quality::Good`] (the last read of this tag
//! succeeded) or [`Quality::Bad`] (it failed - a PLC exception, an
//! unsupported address/type combo, or a whole-connection drop). [`Quality::Stale`]
//! is **not** stored: it is computed at read time as "the last update for
//! this tag is older than its collection period x [`STALE_PERIOD_FACTOR`]"
//! (recorder-requirements.md §3.2). This split matters for the PLC-down case:
//! a disconnected connection keeps appending Bad samples every tick, so those
//! tags stay *fresh in time* but *Bad in quality* (correctly "通信エラー", not
//! "更新停止"); Stale only appears when updates genuinely stop arriving - e.g.
//! a collection task that is no longer ticking at all.
//!
//! `std::sync::RwLock` (not `tokio::sync::RwLock`): reads come from the
//! synchronous UI/render path and writes are short, non-`await` critical
//! sections inside the collection tasks - an async lock would buy nothing and
//! force every reader to be async.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};

use banto_tstore::Clock;

/// How many collection periods a tag may go without a fresh update before a
/// stored-Good sample reads back as [`Quality::Stale`]. 2.5 (design decision)
/// is loose enough that a single skipped tick (a `MissedTickBehavior::Skip`
/// gap, see `task.rs`) does not flap a healthy tag into Stale, but tight
/// enough that a stalled feed is flagged within a few periods.
pub const STALE_PERIOD_FACTOR: f64 = 2.5;

/// Sample quality (recorder-requirements.md §2 "品質").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quality {
    /// The most recent read of this tag succeeded.
    Good,
    /// The most recent read failed (PLC exception, unsupported address/type,
    /// or the whole connection is down).
    Bad,
    /// No fresh update within period x [`STALE_PERIOD_FACTOR`] - derived at
    /// read time, never stored.
    Stale,
}

/// The latest known state of one tag. `quality` here is the *effective*
/// quality as returned by [`CurrentValuesHandle::get`]/[`CurrentValuesHandle::snapshot`]
/// (i.e. already upgraded to [`Quality::Stale`] where applicable); the value
/// stored internally only ever carries Good/Bad.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CurrentSample {
    /// The scaled engineering value, or `None` for a missing/failed reading.
    pub value: Option<f64>,
    /// UTC epoch milliseconds of this sample (the collection PC's clock).
    pub ptime_ms: i64,
    pub quality: Quality,
}

/// What the collection tasks store (before Stale derivation) plus the period
/// needed to derive Stale on read.
#[derive(Debug, Clone, Copy)]
struct Entry {
    value: Option<f64>,
    ptime_ms: i64,
    /// Good or Bad only - never Stale (Stale is a read-time derivation).
    stored_quality: Quality,
    period_ms: u32,
}

/// Cheaply-cloneable shared handle onto the current-value cache. Clones share
/// the same underlying map (recorder-requirements.md §3.2: the collection
/// engine writes, the display layer reads).
#[derive(Clone)]
pub struct CurrentValuesHandle {
    map: Arc<RwLock<HashMap<String, Entry>>>,
    clock: Arc<dyn Clock>,
}

impl CurrentValuesHandle {
    /// Build an empty cache. `clock` is the same clock the rest of the
    /// engine uses, so Stale derivation and the sample timestamps agree on
    /// "now".
    pub(crate) fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            map: Arc::new(RwLock::new(HashMap::new())),
            clock,
        }
    }

    /// Record a fresh sample for `tag_key`. Called once per tag per tick by
    /// the collection tasks. `stored_quality` must be Good or Bad;
    /// [`Quality::Stale`] is derived on read and must never be passed here.
    pub(crate) fn set(
        &self,
        tag_key: &str,
        value: Option<f64>,
        ptime_ms: i64,
        stored_quality: Quality,
        period_ms: u32,
    ) {
        debug_assert!(
            stored_quality != Quality::Stale,
            "Stale is derived at read time, never stored"
        );
        let mut map = self
            .map
            .write()
            .expect("current-value cache lock poisoned (a writer panicked)");
        map.insert(
            tag_key.to_string(),
            Entry {
                value,
                ptime_ms,
                stored_quality,
                period_ms,
            },
        );
    }

    fn derive(entry: &Entry, now_ms: i64) -> CurrentSample {
        // Bad wins over Stale: a failed read is more specific/actionable than
        // "no recent update" (and a PLC-down feed keeps ptime fresh anyway,
        // so it would not be Stale regardless). Only a stored-Good sample can
        // age into Stale.
        let quality = match entry.stored_quality {
            Quality::Bad => Quality::Bad,
            _ => {
                let staleness_limit = (entry.period_ms as f64 * STALE_PERIOD_FACTOR) as i64;
                if now_ms - entry.ptime_ms > staleness_limit {
                    Quality::Stale
                } else {
                    Quality::Good
                }
            }
        };
        CurrentSample {
            value: entry.value,
            ptime_ms: entry.ptime_ms,
            quality,
        }
    }

    /// The latest sample for `tag_key`, with quality already upgraded to
    /// [`Quality::Stale`] if it has aged out. `None` if the tag has never
    /// been sampled.
    pub fn get(&self, tag_key: &str) -> Option<CurrentSample> {
        let now_ms = self.clock.now_ms();
        let map = self
            .map
            .read()
            .expect("current-value cache lock poisoned (a writer panicked)");
        map.get(tag_key).map(|entry| Self::derive(entry, now_ms))
    }

    /// A snapshot of every known tag's latest sample, quality derived as in
    /// [`Self::get`]. Taken against a single "now" so every entry's Stale
    /// derivation is consistent.
    pub fn snapshot(&self) -> HashMap<String, CurrentSample> {
        let now_ms = self.clock.now_ms();
        let map = self
            .map
            .read()
            .expect("current-value cache lock poisoned (a writer panicked)");
        map.iter()
            .map(|(k, entry)| (k.clone(), Self::derive(entry, now_ms)))
            .collect()
    }

    /// Drop every cached entry whose key is not in `keys` (T7-1,
    /// docs/tag-server-design.md §4.3: after [`crate::collector::Collector::apply_config`]
    /// adopts a new config, a tag removed from the registry - or one whose
    /// owning connection was removed - must not linger in the snapshot
    /// forever with a slowly-staling last value). Keys are `tag_key`s
    /// (`"tag:<id>"`), the same identity `Self::set`/[`Self::get`] use.
    /// One write-lock pass, same cost class as a single `Self::set` call.
    pub fn retain(&self, keys: &HashSet<String>) {
        let mut map = self
            .map
            .write()
            .expect("current-value cache lock poisoned (a writer panicked)");
        map.retain(|k, _| keys.contains(k));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use banto_tstore::ManualClock;

    fn handle_at(now_ms: i64) -> (CurrentValuesHandle, Arc<ManualClock>) {
        let clock = Arc::new(ManualClock::new(now_ms, 0));
        (CurrentValuesHandle::new(clock.clone()), clock)
    }

    #[test]
    fn unknown_tag_is_none() {
        let (handle, _clock) = handle_at(0);
        assert_eq!(handle.get("nope"), None);
    }

    #[test]
    fn good_sample_reads_back_good_when_fresh() {
        let (handle, _clock) = handle_at(1_000);
        handle.set("t1", Some(42.0), 1_000, Quality::Good, 1_000);
        let s = handle.get("t1").expect("present");
        assert_eq!(s.value, Some(42.0));
        assert_eq!(s.quality, Quality::Good);
        assert_eq!(s.ptime_ms, 1_000);
    }

    #[test]
    fn bad_sample_reads_back_bad() {
        let (handle, _clock) = handle_at(1_000);
        handle.set("t1", None, 1_000, Quality::Bad, 1_000);
        assert_eq!(handle.get("t1").unwrap().quality, Quality::Bad);
    }

    #[test]
    fn good_sample_becomes_stale_after_period_times_factor() {
        let (handle, clock) = handle_at(1_000);
        handle.set("t1", Some(1.0), 1_000, Quality::Good, 1_000);
        // Just under 2.5 x period: still Good.
        clock.set_now_ms(1_000 + 2_400);
        assert_eq!(handle.get("t1").unwrap().quality, Quality::Good);
        // Past 2.5 x period: Stale.
        clock.set_now_ms(1_000 + 2_600);
        assert_eq!(handle.get("t1").unwrap().quality, Quality::Stale);
    }

    #[test]
    fn bad_sample_stays_bad_even_when_old() {
        // A PLC-down tag keeps updating ptime with Bad; it must read Bad, not
        // Stale, no matter how much wall-clock time passes.
        let (handle, clock) = handle_at(1_000);
        handle.set("t1", None, 1_000, Quality::Bad, 1_000);
        clock.set_now_ms(1_000_000);
        assert_eq!(handle.get("t1").unwrap().quality, Quality::Bad);
    }

    #[test]
    fn snapshot_returns_every_tag_with_derived_quality() {
        let (handle, clock) = handle_at(0);
        handle.set("fresh", Some(1.0), 0, Quality::Good, 1_000);
        handle.set("bad", None, 0, Quality::Bad, 1_000);
        handle.set("old", Some(2.0), 0, Quality::Good, 1_000);
        clock.set_now_ms(10_000); // ages "fresh"/"old" out
        let snap = handle.snapshot();
        assert_eq!(snap.len(), 3);
        assert_eq!(snap["fresh"].quality, Quality::Stale);
        assert_eq!(snap["old"].quality, Quality::Stale);
        assert_eq!(snap["bad"].quality, Quality::Bad);
    }

    #[test]
    fn retain_drops_entries_not_in_the_given_key_set() {
        let (handle, _clock) = handle_at(0);
        handle.set("keep", Some(1.0), 0, Quality::Good, 1_000);
        handle.set("drop", Some(2.0), 0, Quality::Good, 1_000);

        let keep: std::collections::HashSet<String> = ["keep".to_string()].into_iter().collect();
        handle.retain(&keep);

        assert!(handle.get("keep").is_some());
        assert!(handle.get("drop").is_none());
    }

    #[test]
    fn set_overwrites_previous_sample() {
        let (handle, _clock) = handle_at(0);
        handle.set("t1", Some(1.0), 0, Quality::Good, 1_000);
        handle.set("t1", Some(2.0), 500, Quality::Good, 1_000);
        let s = handle.get("t1").unwrap();
        assert_eq!(s.value, Some(2.0));
        assert_eq!(s.ptime_ms, 500);
    }
}
