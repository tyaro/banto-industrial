//! External value sources (#383 段階3 P3, 2026-10-10 オーナー決定 H2/H4):
//! a connection whose values come from **another process that already
//! collected them** (banto-hub, `banto_tags::HUB_PROTOCOL`) instead of from a
//! PLC socket this crate dials itself.
//!
//! ## Why a trait here, implemented elsewhere
//!
//! H2: Hub-sourced tags share *this* crate's daily tstore file, single
//! writer, events, quality and collection-group cycle - so the sampling loop
//! lives here (`crate::task::run_external_connection`). But the subscription
//! (WebSocket, API key, catalog, reconnect supervisor) is ChronoGazer's
//! (`apps/chronogazer/core/src/hub.rs`, on top of `banto-tagclient`). To keep
//! the layering one-way, this crate only **defines** [`ExternalSource`]; the
//! app implements it and hands it to
//! [`crate::Collector::start_with_sources`]. This crate depends on neither
//! `banto-tagclient` nor ChronoGazer, and banto-hub (which never has a hub
//! connection - its strict build rejects one) passes no source at all.
//!
//! ## Sample and hold, never the network
//!
//! The Hub subscription is **on-change**: the Hub sends a value only when it
//! or its quality changes. So the loop does not wait for messages; on every
//! group tick it takes [`ExternalSource::snapshot`] - the latest value the
//! source *holds* for every external name - and records that. A value that
//! has not changed for an hour is still recorded every cycle with Good
//! quality (H4: the Hub's `t` is **not** used to judge freshness; Stale and
//! Bad come only from the Hub's own quality label and from the Hub link not
//! being Live). `snapshot` is synchronous and must be lock-light: it never
//! awaits, never touches the network, and so can never stall a tick.
//!
//! ## The quality / record mapping (one pure function, [`classify_external`])
//!
//! | Link | Hub quality | Hub `value_source` | current value / quality | recorded (tstore) |
//! | --- | --- | --- | --- | --- |
//! | not Live | - | - | `null` / Bad | `null` |
//! | Live, name absent (not subscribed / no value yet) | - | - | `null` / Bad | `null` |
//! | Live | good, value present | measured (`real`/`computed`/`internal`/`db`) | value / Good | **value** |
//! | Live | good, value present | `simulation` / `derived_simulation` | value / Good | `null` |
//! | Live | good, value present | unknown label | value / Good | `null` |
//! | Live | good, no value | any | `null` / Bad | `null` |
//! | Live | stale | any | Hub's last value / Stale | `null` |
//! | Live | bad, or an unknown label | any | `null` / Bad | `null` |
//!
//! - **Simulation values are shown but not recorded** - the same promise
//!   #413 made for `simulation = true` connections (`crate::task`'s
//!   `if ctx.simulation { return; }`): the current-value cache and threshold
//!   events see them, the data file never does. Here it is per tag, because
//!   the Hub labels each value. An **unknown** `value_source` is treated the
//!   same way (shown, not recorded): `banto-tagclient` keeps unknown labels
//!   as unknown precisely "so a future Hub cannot silently become a real
//!   value source" - recording one would do exactly that.
//! - **Hub `stale` records `null`**, consistent with this crate's own Stale:
//!   a PLC tag goes Stale only when its updates stop, and a stopped update
//!   leaves nothing in the data file. The Hub's stale value is the last one
//!   it had, not a fresh measurement, so writing it again each cycle would
//!   put a repeated old value into the history as if it were new. The
//!   current-value cache keeps showing it, labelled Stale.
//! - **Unknown quality is Bad**, never rounded up to Good (H4).
//!
//! ## Link events
//!
//! The Hub link reuses the PLC connection events under the hub connection's
//! key (H4: no new `EventKind`) with exactly the PLC tasks' meaning
//! ([`LinkTracker`]): `plc_connected` the first time the link is Live,
//! `plc_disconnected` when a Live link drops, `plc_reconnected` when it comes
//! back. The state is sampled **once per tick-wake** and only edges are
//! emitted, so a sub-tick flicker of the link never reaches the event table
//! and a long outage is one `plc_disconnected` (the #344 rule).

use std::collections::HashMap;

/// The external source's link as a whole, at one instant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExternalLink {
    /// The source is delivering values (banto-hub: the subscription is
    /// `Live`).
    Live,
    /// The source is not delivering values right now. `detail` is a short
    /// operator-facing reason recorded on `plc_disconnected` - it must not
    /// carry a credential, URL or file path.
    Down { detail: Option<String> },
}

/// The quality label the source attached to one value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalQuality {
    Good,
    Stale,
    Bad,
    /// A label this crate does not know (kept distinct so it can never be
    /// mistaken for Good - it maps to Bad).
    Unknown,
}

/// Where the source says one value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalValueSource {
    /// A value of the physical process (banto-hub: `real`, `computed`,
    /// `internal`, `db`). The only kind that is recorded.
    Measured,
    /// A simulated value (banto-hub: `simulation`, `derived_simulation`).
    /// Shown, never recorded (#413).
    Simulated,
    /// A label this crate does not know. Shown, never recorded (module doc).
    Unknown,
}

/// The latest value the source holds for one external name.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExternalReading {
    pub value: Option<f64>,
    pub quality: ExternalQuality,
    pub source: ExternalValueSource,
}

/// One consistent view of everything the source holds, taken once per
/// tick-wake (every group firing in that wake reads the same view, so two
/// groups can never disagree about whether the link was Live).
#[derive(Debug, Clone, PartialEq)]
pub struct ExternalSnapshot {
    pub link: ExternalLink,
    /// Keyed by external name. Ignored unless `link` is
    /// [`ExternalLink::Live`].
    pub values: HashMap<String, ExternalReading>,
}

impl ExternalSnapshot {
    /// A snapshot of a source that is not delivering anything.
    pub fn down(detail: Option<String>) -> Self {
        Self {
            link: ExternalLink::Down { detail },
            values: HashMap::new(),
        }
    }

    /// A Live snapshot with the given values.
    pub fn live(values: HashMap<String, ExternalReading>) -> Self {
        Self {
            link: ExternalLink::Live,
            values,
        }
    }

    pub fn is_live(&self) -> bool {
        self.link == ExternalLink::Live
    }

    /// The reading for `name`, or `None` when the link is not Live or the
    /// source holds nothing under that name.
    pub fn reading(&self, name: &str) -> Option<&ExternalReading> {
        if self.is_live() {
            self.values.get(name)
        } else {
            None
        }
    }
}

/// A source of externally-collected values (see this module's doc). The app
/// implements it; [`crate::Collector::start_with_sources`] takes it.
///
/// **Contract for implementors**: [`Self::snapshot`] is called from the
/// collection task on every tick-wake. It must not block on I/O, must not
/// wait on an async lock, and should cost about one read lock plus a copy of
/// the held values. It reports [`ExternalLink::Live`] only when the values it
/// returns are the source's current ones.
pub trait ExternalSource: Send + Sync {
    fn snapshot(&self) -> ExternalSnapshot;
}

/// What one tag becomes on one tick (the result of [`classify_external`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ExternalTagOutcome {
    /// What the current-value cache shows.
    pub shown: Option<f64>,
    /// Good / Bad / Stale (stored as-is in the cache, see `crate::current`).
    pub quality: crate::current::Quality,
    /// What goes into the data file's column (`None` = SQL NULL).
    pub recorded: Option<f64>,
}

/// The module doc's mapping table, as one pure function (so the whole table
/// is unit-tested without a task, writer or socket).
pub(crate) fn classify_external(reading: Option<&ExternalReading>) -> ExternalTagOutcome {
    use crate::current::Quality;
    const BAD: ExternalTagOutcome = ExternalTagOutcome {
        shown: None,
        quality: Quality::Bad,
        recorded: None,
    };
    let Some(reading) = reading else {
        // Not Live, or nothing held under this name.
        return BAD;
    };
    match (reading.quality, reading.value) {
        (ExternalQuality::Good, Some(value)) => ExternalTagOutcome {
            shown: Some(value),
            quality: Quality::Good,
            recorded: match reading.source {
                ExternalValueSource::Measured => Some(value),
                ExternalValueSource::Simulated | ExternalValueSource::Unknown => None,
            },
        },
        (ExternalQuality::Stale, value) => ExternalTagOutcome {
            shown: value,
            quality: Quality::Stale,
            recorded: None,
        },
        (ExternalQuality::Good, None) | (ExternalQuality::Bad | ExternalQuality::Unknown, _) => BAD,
    }
}

/// The link-edge events [`LinkTracker::observe`] reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LinkEdge {
    /// The link is Live for the first time in this task's life.
    Connected,
    /// A Live link dropped.
    Disconnected,
    /// The link is Live again after a [`LinkEdge::Disconnected`].
    Reconnected,
}

/// The external link's episode-edge detector - the PLC tasks' connection
/// event rule (`crate::task`'s module doc, "What `plc_reconnected` means")
/// applied to a sampled link state. Pure, so the no-flap property is
/// table-tested.
#[derive(Debug, Default)]
pub(crate) struct LinkTracker {
    ever_live: bool,
    live: bool,
}

impl LinkTracker {
    /// Feed one sampled link state; returns the edge, if this sample is one.
    /// A link that was never Live produces nothing while it stays down (a
    /// PLC that never connected emits no `plc_disconnected` either).
    pub(crate) fn observe(&mut self, live: bool) -> Option<LinkEdge> {
        let edge = match (self.live, live) {
            (false, true) if !self.ever_live => Some(LinkEdge::Connected),
            (false, true) => Some(LinkEdge::Reconnected),
            (true, false) => Some(LinkEdge::Disconnected),
            _ => None,
        };
        self.live = live;
        self.ever_live |= live;
        edge
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::current::Quality;

    fn reading(
        value: Option<f64>,
        quality: ExternalQuality,
        source: ExternalValueSource,
    ) -> ExternalReading {
        ExternalReading {
            value,
            quality,
            source,
        }
    }

    /// The whole mapping table from the module doc, row by row.
    #[test]
    fn the_quality_and_record_mapping_table() {
        use ExternalQuality as Q;
        use ExternalValueSource as S;
        // (input, shown, quality, recorded)
        type Row = (Option<ExternalReading>, Option<f64>, Quality, Option<f64>);
        let rows: Vec<Row> = vec![
            // not Live / absent
            (None, None, Quality::Bad, None),
            // good + measured: shown and recorded
            (
                Some(reading(Some(1.5), Q::Good, S::Measured)),
                Some(1.5),
                Quality::Good,
                Some(1.5),
            ),
            // good + simulated: shown, not recorded (#413)
            (
                Some(reading(Some(2.0), Q::Good, S::Simulated)),
                Some(2.0),
                Quality::Good,
                None,
            ),
            // good + unknown source: shown, not recorded
            (
                Some(reading(Some(3.0), Q::Good, S::Unknown)),
                Some(3.0),
                Quality::Good,
                None,
            ),
            // good without a value: Bad
            (
                Some(reading(None, Q::Good, S::Measured)),
                None,
                Quality::Bad,
                None,
            ),
            // stale: last value shown as Stale, not recorded
            (
                Some(reading(Some(4.0), Q::Stale, S::Measured)),
                Some(4.0),
                Quality::Stale,
                None,
            ),
            // bad / unknown quality: Bad + null (never rounded to good)
            (
                Some(reading(Some(5.0), Q::Bad, S::Measured)),
                None,
                Quality::Bad,
                None,
            ),
            (
                Some(reading(Some(6.0), Q::Unknown, S::Measured)),
                None,
                Quality::Bad,
                None,
            ),
        ];
        for (input, shown, quality, recorded) in rows {
            let outcome = classify_external(input.as_ref());
            assert_eq!(
                outcome,
                ExternalTagOutcome {
                    shown,
                    quality,
                    recorded
                },
                "{input:?}"
            );
        }
    }

    #[test]
    fn a_down_snapshot_yields_no_reading_even_for_a_held_name() {
        let mut snapshot = ExternalSnapshot::live(HashMap::from([(
            "a.b.c".to_string(),
            reading(
                Some(1.0),
                ExternalQuality::Good,
                ExternalValueSource::Measured,
            ),
        )]));
        assert!(snapshot.reading("a.b.c").is_some());
        snapshot.link = ExternalLink::Down { detail: None };
        assert_eq!(snapshot.reading("a.b.c"), None);
    }

    /// Edges only, never per sample: the first Live is `Connected`, a drop is
    /// one `Disconnected` however long it lasts, the return is one
    /// `Reconnected`, and a link that was never Live is silent.
    #[test]
    fn the_link_tracker_reports_edges_only() {
        let mut never = LinkTracker::default();
        assert_eq!(never.observe(false), None);
        assert_eq!(never.observe(false), None);

        let mut tracker = LinkTracker::default();
        let seen: Vec<Option<LinkEdge>> = [true, true, false, false, false, true, true, false]
            .into_iter()
            .map(|live| tracker.observe(live))
            .collect();
        assert_eq!(
            seen,
            vec![
                Some(LinkEdge::Connected),
                None,
                Some(LinkEdge::Disconnected),
                None,
                None,
                Some(LinkEdge::Reconnected),
                None,
                Some(LinkEdge::Disconnected),
            ]
        );
    }
}
