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
//! **The one stored Stale: an external source's own label** (#383 段階3 P3,
//! 2026-10-10 オーナー決定 H4). A Hub-sourced tag whose value the Hub itself
//! labels `stale` is written here as [`Quality::Stale`] (with the Hub's last
//! value) on every tick - the Hub, not this cache, is the one that knows its
//! value stopped updating. It reads back as Stale regardless of age (the
//! time-based derivation above never turns it back into Good) and does not
//! move `last_good_ms`. See `crate::external`.
//!
//! ## Last successful receive time (`last_good_ms`)
//!
//! [`CurrentSample::ptime_ms`] is the time of the **latest attempt**, whatever
//! its quality: a Bad sample carries the time of the read that failed (a
//! PLC-down feed writes a fresh Bad sample every tick, so this keeps
//! advancing). To answer "when did this tag last deliver a usable value?"
//! the cache also keeps, per tag, the `ptime_ms` of the last **Good sample
//! with a finite value** ([`CurrentReading::last_good_ms`]). It is updated
//! only by such samples, kept unchanged across Bad samples (and across Stale,
//! which is a read-time derivation and never written), and is `None` when the
//! tag has not delivered a usable value since this cache was created (i.e.
//! since collection started - a restart builds a fresh collector and a fresh
//! cache). A non-finite Good value (NaN/±∞ from a float register) does not
//! count: the display layers publish it as Bad (no usable value).
//! Exposed through [`CurrentValuesHandle::snapshot_readings`]; the plain
//! [`CurrentValuesHandle::get`]/[`CurrentValuesHandle::snapshot`] and
//! [`CurrentSample`] are unchanged. banto-hub's server tag store tracks
//! equivalent metadata for computed, internal and DB tags.
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
    /// UTC epoch milliseconds of this sample (the collection PC's clock) -
    /// the time of the latest **attempt**, also for a Bad sample (a failed
    /// read). Not "when a value was last received": see
    /// [`CurrentReading::last_good_ms`].
    pub ptime_ms: i64,
    pub quality: Quality,
}

/// A [`CurrentSample`] plus the time of the tag's last usable value (module
/// doc "Last successful receive time").
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CurrentReading {
    pub sample: CurrentSample,
    /// `ptime_ms` of the last Good sample with a finite value; `None` = none
    /// since this cache was created. Never advanced by Bad samples.
    pub last_good_ms: Option<i64>,
    /// First timestamp of the current effective quality; derived Stale uses
    /// the deterministic threshold crossing rather than the read time.
    /// Missing/non-finite Good values count as Bad for this metadata, as
    /// they do in the display layers; sample quality remains unchanged.
    pub quality_since_ms: Option<i64>,
}

/// What the collection tasks store (before Stale derivation) plus the period
/// needed to derive Stale on read.
#[derive(Debug, Clone, Copy)]
struct Entry {
    value: Option<f64>,
    ptime_ms: i64,
    /// Good or Bad for a PLC tag (its Stale is a read-time derivation);
    /// Stale only for an external-source tag the source labelled stale.
    stored_quality: Quality,
    period_ms: u32,
    /// See [`CurrentReading::last_good_ms`].
    last_good_ms: Option<i64>,
    quality_since_ms: i64,
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

    /// Invalidate a cached PLC value when a same-session diagnostic proves
    /// its address outside the device range. A diagnostic is not a read
    /// attempt, so keep the sample time and last-good time unchanged.
    pub fn mark_outside_bad(&self, tag_key: &str) {
        let now_ms = self.clock.now_ms();
        let mut map = self
            .map
            .write()
            .expect("current-value cache lock poisoned (a writer panicked)");
        if let Some(entry) = map.get_mut(tag_key) {
            let was_bad = entry.stored_quality == Quality::Bad;
            entry.value = None;
            entry.stored_quality = Quality::Bad;
            if !was_bad {
                entry.quality_since_ms = now_ms;
            }
        }
    }

    /// Record a fresh sample for `tag_key`. Called once per tag per tick by
    /// the collection tasks. A PLC task passes Good or Bad only (its Stale is
    /// derived on read); an external-source task (`crate::external`) may
    /// also pass [`Quality::Stale`] when the source itself labelled the value
    /// stale (module doc, "The one stored Stale").
    pub(crate) fn set(
        &self,
        tag_key: &str,
        value: Option<f64>,
        ptime_ms: i64,
        stored_quality: Quality,
        period_ms: u32,
    ) {
        let mut map = self
            .map
            .write()
            .expect("current-value cache lock poisoned (a writer panicked)");
        let usable = stored_quality == Quality::Good && value.is_some_and(f64::is_finite);
        let last_good_ms = if usable {
            Some(ptime_ms)
        } else {
            map.get(tag_key).and_then(|previous| previous.last_good_ms)
        };
        let effective_quality = if stored_quality == Quality::Good && !usable {
            Quality::Bad
        } else {
            stored_quality
        };
        let quality_since_ms = map.get(tag_key).map_or(ptime_ms, |previous| {
            let reading = Self::reading(previous, self.clock.now_ms());
            let previous_quality = if previous.stored_quality == Quality::Good
                && !previous.value.is_some_and(f64::is_finite)
            {
                Quality::Bad
            } else {
                reading.sample.quality
            };
            if previous_quality == effective_quality {
                reading.quality_since_ms.unwrap_or(ptime_ms)
            } else {
                ptime_ms
            }
        });
        map.insert(
            tag_key.to_string(),
            Entry {
                value,
                ptime_ms,
                stored_quality,
                period_ms,
                last_good_ms,
                quality_since_ms,
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
            // An external source's own `stale` label (module doc): a recent
            // ptime never turns it back into Good.
            Quality::Stale => Quality::Stale,
            Quality::Good => {
                let staleness_limit = (entry.period_ms as f64 * STALE_PERIOD_FACTOR) as i64;
                if now_ms.saturating_sub(entry.ptime_ms) > staleness_limit {
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

    fn reading(entry: &Entry, now_ms: i64) -> CurrentReading {
        let sample = Self::derive(entry, now_ms);
        let quality_since_ms = if sample.quality == Quality::Stale
            && entry.stored_quality == Quality::Good
            && entry.value.is_some_and(f64::is_finite)
        {
            entry
                .ptime_ms
                .saturating_add((entry.period_ms as f64 * STALE_PERIOD_FACTOR) as i64)
                .saturating_add(1)
        } else {
            entry.quality_since_ms
        };
        CurrentReading {
            sample,
            last_good_ms: entry.last_good_ms,
            quality_since_ms: Some(quality_since_ms),
        }
    }

    /// Read a sample and its timestamps under one lock and one clock instant.
    pub fn get_reading(&self, tag_key: &str) -> Option<CurrentReading> {
        let now_ms = self.clock.now_ms();
        let map = self
            .map
            .read()
            .expect("current-value cache lock poisoned (a writer panicked)");
        map.get(tag_key).map(|entry| Self::reading(entry, now_ms))
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

    /// [`Self::get`]'s sample together with the quality **as stored** (Good/Bad,
    /// or an external source's own Stale, before the read-time Stale
    /// derivation), both from one entry under one read lock and one clock
    /// instant. For test observation only (試験の観測のため。banto-industrial
    /// #946/#947: telling a stored Stale from a derived one without a gap
    /// between two reads); display layers must use [`Self::get`] /
    /// [`Self::get_reading`].
    #[doc(hidden)]
    pub fn get_with_stored(&self, tag_key: &str) -> Option<(CurrentSample, Quality)> {
        let now_ms = self.clock.now_ms();
        let map = self
            .map
            .read()
            .expect("current-value cache lock poisoned (a writer panicked)");
        map.get(tag_key)
            .map(|entry| (Self::derive(entry, now_ms), entry.stored_quality))
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

    /// Like [`Self::snapshot`], with each tag's last usable-value time
    /// ([`CurrentReading::last_good_ms`]). Same single "now" and single read
    /// lock, so the sample and its `last_good_ms` are from the same instant.
    pub fn snapshot_readings(&self) -> HashMap<String, CurrentReading> {
        let now_ms = self.clock.now_ms();
        let map = self
            .map
            .read()
            .expect("current-value cache lock poisoned (a writer panicked)");
        map.iter()
            .map(|(k, entry)| (k.clone(), Self::reading(entry, now_ms)))
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
    fn outside_diagnostic_invalidates_value_without_faking_a_read() {
        let (handle, clock) = handle_at(1_000);
        handle.set("t1", Some(42.0), 1_000, Quality::Good, 1_000);
        clock.set_now_ms(1_100);
        handle.mark_outside_bad("t1");
        let reading = handle.get_reading("t1").unwrap();
        assert_eq!(reading.sample.value, None);
        assert_eq!(reading.sample.quality, Quality::Bad);
        assert_eq!(reading.sample.ptime_ms, 1_000);
        assert_eq!(reading.last_good_ms, Some(1_000));
        assert_eq!(reading.quality_since_ms, Some(1_100));
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
    fn get_with_stored_returns_derived_and_stored_quality_together() {
        let (handle, clock) = handle_at(1_000);
        handle.set("good", Some(1.0), 1_000, Quality::Good, 1_000);
        handle.set("stale", Some(2.0), 1_000, Quality::Stale, 1_000);
        clock.set_now_ms(1_000 + 2_600);
        // Stored Good aged into a derived Stale.
        assert_eq!(
            handle.get_with_stored("good").unwrap(),
            (
                CurrentSample {
                    value: Some(1.0),
                    ptime_ms: 1_000,
                    quality: Quality::Stale,
                },
                Quality::Good
            )
        );
        assert_eq!(
            handle.get_with_stored("good").unwrap().0,
            handle.get("good").unwrap()
        );
        // Stored Stale stays Stale in both.
        let (derived, stored) = handle.get_with_stored("stale").unwrap();
        assert_eq!((derived.quality, stored), (Quality::Stale, Quality::Stale));
        assert_eq!(handle.get_with_stored("missing"), None);
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

    #[test]
    fn timestamp_transitions_are_stable_and_stale_recovery_starts_good_again() {
        let (handle, clock) = handle_at(1_000);
        assert_eq!(handle.get_reading("missing"), None);
        handle.set("t", Some(1.0), 1_000, Quality::Good, 101);
        assert_eq!(
            handle.get_reading("t").unwrap().quality_since_ms,
            Some(1_000)
        );
        clock.set_now_ms(1_252);
        assert_eq!(
            handle.get_reading("t").unwrap().sample.quality,
            Quality::Good
        );
        clock.set_now_ms(1_253);
        let stale = handle.get_reading("t").unwrap();
        assert_eq!(stale.sample.quality, Quality::Stale);
        assert_eq!(stale.quality_since_ms, Some(1_253));
        clock.set_now_ms(2_000);
        assert_eq!(handle.get_reading("t"), Some(stale));
        handle.set("t", Some(2.0), 2_000, Quality::Good, 101);
        assert_eq!(
            handle.get_reading("t").unwrap().quality_since_ms,
            Some(2_000)
        );
        handle.set("t", None, 2_010, Quality::Bad, 101);
        handle.set("t", None, 2_020, Quality::Bad, 101);
        let bad = handle.get_reading("t").unwrap();
        assert_eq!(bad.quality_since_ms, Some(2_010));
        assert_eq!(bad.last_good_ms, Some(2_000));
        handle.set("t", Some(2.0), 2_030, Quality::Stale, 101);
        handle.set("t", Some(2.0), 2_040, Quality::Stale, 101);
        assert_eq!(
            handle.get_reading("t").unwrap().quality_since_ms,
            Some(2_030)
        );
        assert_eq!(
            handle.snapshot_readings()["t"],
            handle.get_reading("t").unwrap()
        );
    }

    #[test]
    fn stale_derivation_handles_extreme_timestamps_without_overflow() {
        let (handle, clock) = handle_at(i64::MIN);
        handle.set("t", Some(1.0), i64::MIN, Quality::Good, 1);
        clock.set_now_ms(i64::MAX);
        assert_eq!(
            handle.get_reading("t").unwrap().quality_since_ms,
            Some(i64::MIN + 3)
        );
    }

    fn last_good(handle: &CurrentValuesHandle, key: &str) -> Option<i64> {
        handle.snapshot_readings()[key].last_good_ms
    }

    #[test]
    fn last_good_ms_freezes_at_the_last_good_sample_while_bad() {
        // Good -> Bad -> Bad: ptime keeps advancing with every failed attempt,
        // last_good_ms stays at the last Good sample.
        let (handle, clock) = handle_at(1_000);
        handle.set("t1", Some(5.0), 1_000, Quality::Good, 1_000);
        assert_eq!(last_good(&handle, "t1"), Some(1_000));
        handle.set("t1", None, 2_000, Quality::Bad, 1_000);
        handle.set("t1", None, 3_000, Quality::Bad, 1_000);
        clock.set_now_ms(3_000);
        let reading = handle.snapshot_readings()["t1"];
        assert_eq!(reading.sample.ptime_ms, 3_000);
        assert_eq!(reading.sample.quality, Quality::Bad);
        assert_eq!(reading.last_good_ms, Some(1_000));
    }

    #[test]
    fn last_good_ms_is_none_when_bad_from_the_start() {
        let (handle, _clock) = handle_at(1_000);
        handle.set("t1", None, 1_000, Quality::Bad, 1_000);
        handle.set("t1", None, 2_000, Quality::Bad, 1_000);
        assert_eq!(last_good(&handle, "t1"), None);
    }

    #[test]
    fn last_good_ms_advances_again_once_good_returns() {
        let (handle, _clock) = handle_at(1_000);
        handle.set("t1", None, 1_000, Quality::Bad, 1_000);
        handle.set("t1", Some(1.0), 2_000, Quality::Good, 1_000);
        assert_eq!(last_good(&handle, "t1"), Some(2_000));
        handle.set("t1", None, 3_000, Quality::Bad, 1_000);
        handle.set("t1", Some(2.0), 4_000, Quality::Good, 1_000);
        assert_eq!(last_good(&handle, "t1"), Some(4_000));
    }

    #[test]
    fn last_good_ms_is_kept_while_stale() {
        // Stale is derived on read; it never touches last_good_ms.
        let (handle, clock) = handle_at(1_000);
        handle.set("t1", Some(1.0), 1_000, Quality::Good, 1_000);
        clock.set_now_ms(100_000);
        let reading = handle.snapshot_readings()["t1"];
        assert_eq!(reading.sample.quality, Quality::Stale);
        assert_eq!(reading.last_good_ms, Some(1_000));
    }

    /// #383 段階3 P3: a stored Stale (an external source's own label) reads
    /// back Stale even when fresh, keeps its value, and does not move
    /// `last_good_ms`.
    #[test]
    fn a_stored_stale_sample_reads_back_stale_and_keeps_last_good() {
        let (handle, _clock) = handle_at(2_000);
        handle.set("t1", Some(1.0), 1_000, Quality::Good, 1_000);
        handle.set("t1", Some(1.0), 2_000, Quality::Stale, 1_000);
        let reading = handle.snapshot_readings()["t1"];
        assert_eq!(reading.sample.quality, Quality::Stale);
        assert_eq!(reading.sample.value, Some(1.0));
        assert_eq!(reading.sample.ptime_ms, 2_000);
        assert_eq!(reading.last_good_ms, Some(1_000));
    }

    #[test]
    fn a_non_finite_good_value_does_not_count_as_received() {
        let (handle, _clock) = handle_at(1_000);
        handle.set("t1", Some(1.0), 1_000, Quality::Good, 1_000);
        handle.set("t1", Some(f64::NAN), 2_000, Quality::Good, 1_000);
        assert_eq!(last_good(&handle, "t1"), Some(1_000));
    }
}
