//! The condition poller (W3-B, `luminous-discovering-goblet.md`). On a fixed
//! cadence it reads every source tag any enabled rule references and writes the
//! results into the [`CurrentValues`] cache. It holds only [`ReadOnlyHandle`]s -
//! it is structurally incapable of writing to a PLC.
//!
//! ## Source resolution
//!
//! Each source is a `banto_tags::Tag`, resolved once at engine start (see
//! [`crate::engine::Engine::start`]) to `(connection_id, Address, DataType)`:
//! the tag's `address` string is parsed with `Address::parse_slmp` and its PLC
//! connection is the one its collection group points at. This engine is
//! SLMP-only; a source on a non-SLMP connection is dropped at compile time (with
//! a logged warning) and simply never polled.
//!
//! ## Cadence
//!
//! A single fixed [`std::time::Duration`] interval (`EngineConfig::poll_interval`,
//! default 500 ms) drives all connections, rather than per-collection-group
//! `period_ms`. This is the simpler, easier-to-reason-about choice for a write
//! engine whose job is reacting to threshold crossings, not faithfully
//! reproducing each group's historian cadence; it is documented here as a
//! deliberate simplification.
//!
//! ## Failure handling
//!
//! A connection that is down (the broker returns [`BrokerError`]) or a per-tag
//! `Bad` read downgrades those tags to [`Quality::Bad`] and the loop moves on -
//! a single dead PLC never crashes the poller or blocks the others.

use std::collections::HashMap;
use std::time::Instant;

use banto_plc::{Address, BatchReadRequest, BatchReadResult, ReadRequest, StringReadRequest};
use tokio::sync::watch;

use super::current_values::CurrentValues;
use super::rule_engine::WireShape;
use banto_broker::ReadOnlyHandle;

/// A source tag resolved to its wire coordinates - numeric or, since S2
/// 文字列タグ, string ([`WireShape`] carries which, plus a string's word span).
#[derive(Debug, Clone)]
pub struct ResolvedSource {
    pub tag_id: i64,
    pub connection_id: i64,
    pub address: Address,
    pub shape: WireShape,
}

impl ResolvedSource {
    /// The (numeric or string) batch read request this source turns into.
    /// `pub(super)` since the タグモニタ (`super::monitor`) builds its group
    /// reads through the same resolution.
    pub(super) fn to_request(&self) -> BatchReadRequest {
        match self.shape {
            WireShape::Numeric(data_type) => BatchReadRequest::Numeric(ReadRequest {
                address: self.address,
                data_type,
            }),
            WireShape::Str { words } => BatchReadRequest::String(StringReadRequest {
                address: self.address,
                words,
            }),
        }
    }
}

/// Read every source once and fold the results into `cache`. Reads are grouped
/// per connection (one broker round trip each). Never panics on a down
/// connection or a bad tag.
pub async fn poll_once(
    handles: &HashMap<i64, ReadOnlyHandle>,
    sources_by_conn: &HashMap<i64, Vec<ResolvedSource>>,
    cache: &CurrentValues,
) {
    for (connection_id, sources) in sources_by_conn {
        let Some(handle) = handles.get(connection_id) else {
            // No read handle (e.g. connection not in the broker): mark all bad.
            let now = Instant::now();
            for src in sources {
                cache.mark_bad(src.tag_id, now);
            }
            continue;
        };

        let requests: Vec<BatchReadRequest> = sources.iter().map(|s| s.to_request()).collect();

        match handle.read(requests).await {
            Ok(results) => {
                let now = Instant::now();
                // The broker preserves request order in its results.
                for (src, result) in sources.iter().zip(results) {
                    match result {
                        BatchReadResult::Value(value) => cache.set_good(src.tag_id, value, now),
                        BatchReadResult::Bad(_) => cache.mark_bad(src.tag_id, now),
                    }
                }
            }
            Err(_broker_err) => {
                // Whole connection down/failed this cycle: every tag on it is
                // indeterminate until it recovers.
                let now = Instant::now();
                for src in sources {
                    cache.mark_bad(src.tag_id, now);
                }
            }
        }
    }
}

/// The poller task: [`poll_once`] every `interval` until `shutdown` fires. Exits
/// promptly on the shutdown signal (it `select!`s on it rather than relying on
/// any channel closing).
pub async fn run_poller(
    handles: HashMap<i64, ReadOnlyHandle>,
    sources_by_conn: HashMap<i64, Vec<ResolvedSource>>,
    cache: CurrentValues,
    interval: std::time::Duration,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = shutdown.changed() => break,
            _ = ticker.tick() => {
                poll_once(&handles, &sources_by_conn, &cache).await;
            }
        }
    }
}
