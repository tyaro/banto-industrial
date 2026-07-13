//! Resource-change event plumbing (spec §3.5 `EventProvider`, §11.3).
//!
//! `banto-server::ServerEvent` is the wire type; this module just owns the
//! app-wide broadcast channel that feeds it, so `ItemsService` (and any
//! future resource service) and the SSE route
//! (`banto_server::events::sse_route`) share one sender.

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
