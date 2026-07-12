//! Collection events (recorder-requirements.md §3.5) and their two-output
//! delivery ([`EventSink`]): a live `tokio::sync::broadcast` channel for the
//! UI and a durable `collect_events` row for the later Events screen.
//!
//! ## Why threshold events are pure state-change edges, nothing more
//!
//! [`EventSink`] emits `threshold_entered`/`threshold_cleared` only when a
//! tag's classified level actually changes (the edge detection lives in
//! `task.rs`), comparing the *scaled* value against fixed H/HH/L/LL limits.
//! There is deliberately no deadband, on-delay, or ACK/latch state here:
//! alarm state-transition management (ACK, shelving, escalation) is
//! explicitly out of scope for v1 (recorder-requirements.md §7, plan.md §4's
//! "スコープの護り") - this crate records that a limit was crossed and that
//! it later cleared, and stops there. Anything richer belongs to a future
//! alarm-management layer, not the recorder's collection engine.

use serde::Serialize;
use sqlx::SqlitePool;
use tokio::sync::broadcast;

/// Default live-channel capacity. `broadcast` drops the oldest buffered
/// event for a slow receiver once this many are outstanding (the receiver
/// observes a `RecvError::Lagged`) - fine for a live UI feed, which only
/// needs "roughly the recent events" and always has the durable
/// `collect_events` table as the complete record. Generous enough that a
/// normally-responsive UI never lags.
pub const DEFAULT_EVENT_CHANNEL_CAPACITY: usize = 1024;

/// The kind of a [`CollectEvent`]. Serialized to the snake_case string form
/// stored in `collect_events.kind` and sent over the wire to the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// The collector started (emitted once by [`crate::Collector::start`]).
    CollectionStarted,
    /// The collector stopped (emitted once by [`crate::Collector::stop`]).
    CollectionStopped,
    /// A connection established its socket for the first time.
    PlcConnected,
    /// A previously-connected connection lost its socket (`detail` carries
    /// the reason).
    PlcDisconnected,
    /// A connection re-established its socket after a disconnect
    /// (recorder-requirements.md §3.1: "復旧後に自動再接続").
    PlcReconnected,
    /// A tag's scaled value crossed into a threshold band (`level`/`value`
    /// set).
    ThresholdEntered,
    /// A tag's scaled value left a threshold band (`level` = the band it
    /// left, `value` = the reading that cleared it).
    ThresholdCleared,
}

impl EventKind {
    /// The exact string persisted in `collect_events.kind`.
    pub fn as_str(self) -> &'static str {
        match self {
            EventKind::CollectionStarted => "collection_started",
            EventKind::CollectionStopped => "collection_stopped",
            EventKind::PlcConnected => "plc_connected",
            EventKind::PlcDisconnected => "plc_disconnected",
            EventKind::PlcReconnected => "plc_reconnected",
            EventKind::ThresholdEntered => "threshold_entered",
            EventKind::ThresholdCleared => "threshold_cleared",
        }
    }
}

/// A threshold band (recorder-requirements.md §3.2: "タグ毎に H/HH/L/LL").
/// Ordered low-to-high only as a label; the comparison logic lives in
/// `task.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ThresholdLevel {
    /// Low-low (value <= LL limit).
    Ll,
    /// Low (value <= L limit).
    L,
    /// High (value >= H limit).
    H,
    /// High-high (value >= HH limit).
    Hh,
}

impl ThresholdLevel {
    /// The exact string persisted in `collect_events.level`.
    pub fn as_str(self) -> &'static str {
        match self {
            ThresholdLevel::Ll => "LL",
            ThresholdLevel::L => "L",
            ThresholdLevel::H => "H",
            ThresholdLevel::Hh => "HH",
        }
    }
}

/// One collection event. Cloned into every live subscriber (hence
/// `#[derive(Clone)]`) and rendered into one `collect_events` row.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectEvent {
    /// UTC epoch milliseconds (the collection PC's clock).
    pub ts_ms: i64,
    pub kind: EventKind,
    /// The connection this concerns (`None` for collector-wide events).
    pub connection_key: Option<String>,
    /// The tag this concerns (`Some` only for `threshold_*`).
    pub tag_key: Option<String>,
    /// The threshold band (`Some` only for `threshold_*`).
    pub level: Option<ThresholdLevel>,
    /// The scaled value involved (`Some` only for `threshold_*`).
    pub value: Option<f64>,
    /// Free-text detail (e.g. a disconnect reason).
    pub detail: Option<String>,
}

impl CollectEvent {
    /// A collector-wide event with no connection/tag/level/value.
    pub(crate) fn lifecycle(ts_ms: i64, kind: EventKind) -> Self {
        Self {
            ts_ms,
            kind,
            connection_key: None,
            tag_key: None,
            level: None,
            value: None,
            detail: None,
        }
    }

    /// A connection-scoped event (`plc_connected`/`plc_disconnected`/
    /// `plc_reconnected`), with an optional reason `detail`.
    pub(crate) fn connection(
        ts_ms: i64,
        kind: EventKind,
        connection_key: impl Into<String>,
        detail: Option<String>,
    ) -> Self {
        Self {
            ts_ms,
            kind,
            connection_key: Some(connection_key.into()),
            tag_key: None,
            level: None,
            value: None,
            detail,
        }
    }

    /// A threshold-edge event for one tag.
    pub(crate) fn threshold(
        ts_ms: i64,
        kind: EventKind,
        connection_key: impl Into<String>,
        tag_key: impl Into<String>,
        level: ThresholdLevel,
        value: f64,
    ) -> Self {
        Self {
            ts_ms,
            kind,
            connection_key: Some(connection_key.into()),
            tag_key: Some(tag_key.into()),
            level: Some(level),
            value: Some(value),
            detail: None,
        }
    }
}

/// The event delivery seam handed to [`crate::Collector::start`]. Cloneable
/// (cheap - an `Arc` inside a `broadcast::Sender` plus an `Arc`-backed
/// `SqlitePool`) so every per-connection task holds its own handle. Build
/// one with [`EventSink::new`], subscribe live consumers with
/// [`EventSink::subscribe`].
#[derive(Clone)]
pub struct EventSink {
    tx: broadcast::Sender<CollectEvent>,
    pool: SqlitePool,
}

impl EventSink {
    /// Create a sink persisting to `pool` (the app's shared database - the
    /// same one [`crate::build_config`] read the registry from and
    /// [`crate::migrate`] created `collect_events` in) with the default live
    /// channel capacity.
    pub fn new(pool: SqlitePool) -> Self {
        Self::with_capacity(pool, DEFAULT_EVENT_CHANNEL_CAPACITY)
    }

    /// As [`EventSink::new`] but with an explicit broadcast buffer capacity.
    pub fn with_capacity(pool: SqlitePool, capacity: usize) -> Self {
        let (tx, _rx) = broadcast::channel(capacity);
        Self { tx, pool }
    }

    /// Subscribe a live consumer. Each receiver sees every event emitted
    /// after it subscribed (a `broadcast` receiver does not replay history -
    /// the durable `collect_events` table is where past events live).
    pub fn subscribe(&self) -> broadcast::Receiver<CollectEvent> {
        self.tx.subscribe()
    }

    /// Emit one event: persist it (best-effort) and broadcast it live.
    ///
    /// Neither output can stop collection (recorder-requirements.md §3.5's
    /// "発行失敗（broadcast の受信者ゼロ等）は収集を止めない"): a DB insert
    /// failure is swallowed (the live feed still gets the event), and
    /// `broadcast::Sender::send` returning `Err` because there are currently
    /// no subscribers is the normal case, not an error. Persistence is
    /// attempted first so a subscriber reacting to the live event can rely on
    /// the row already existing.
    pub(crate) async fn emit(&self, event: CollectEvent) {
        let _ = sqlx::query(
            "INSERT INTO collect_events (ts, kind, connection_key, tag_key, level, value, detail) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(event.ts_ms)
        .bind(event.kind.as_str())
        .bind(&event.connection_key)
        .bind(&event.tag_key)
        .bind(event.level.map(|l| l.as_str()))
        .bind(event.value)
        .bind(&event.detail)
        .execute(&self.pool)
        .await;

        let _ = self.tx.send(event);
    }
}
