//! Admin-UI resource-change event plumbing (`banto_server::ServerEvent`,
//! consumed by `banto_server::sse_route`) - **not** to be confused with
//! `banto_collect::CollectEvent` (docs/tag-server-design.md §4.1's
//! `config_changed`/collector lifecycle events), which is a different type
//! flowing over a different channel (see `hub.rs`). This module just owns
//! the app-wide broadcast channel `ServerEvent` rides on, so every resource
//! router (I1 CRUD, users, audit) and the SSE route share one sender - copied
//! verbatim from `apps/chronogazer/core/src/events.rs`.

use banto_server::ServerEvent;
use tokio::sync::broadcast;

/// Small buffer: events are cheap "go refetch resource X" notifications,
/// not a durable log, and browser clients reconnecting after a gap simply
/// miss stale invalidations (their next `getList` call already reflects the
/// current state).
const CHANNEL_CAPACITY: usize = 64;

/// Create the app-wide `ServerEvent` broadcast channel. Clone the returned
/// `Sender` into every service that emits resource-change events and into
/// the SSE route (`banto_server::events::sse_route`) that fans them out to
/// connected browsers.
pub fn event_channel() -> broadcast::Sender<ServerEvent> {
    let (tx, _rx) = broadcast::channel(CHANNEL_CAPACITY);
    tx
}
