//! The per-connection collection loop - one `tokio` task per PLC connection
//! (recorder-requirements.md §3.1, plan.md §5's I3b design), the crate's
//! central concurrency decision.
//!
//! ## Why one task per connection, with an in-task min-deadline scheduler
//!
//! A single connection's socket must never be read by two `read_batch` calls
//! at once. Rather than a sub-task per group feeding an mpsc to a socket-owner
//! task, this uses the simpler shape the design permitted: **one task owns the
//! one client and services every group on that connection sequentially**. The
//! groups' independent periods are multiplexed by a hand-rolled next-deadline
//! scheduler (`next_fire[i]`) instead of N `tokio::time::Interval`s + a
//! dynamic `select!` - it handles an arbitrary group count without pulling in
//! `futures::select_all`, and implements `MissedTickBehavior::Skip` semantics
//! explicitly: after a group fires, its deadline advances by *whole periods
//! from its original phase* until it lands in the future. Staying
//! phase-aligned (rather than rescheduling from "now") keeps per-tick wake
//! latency - notably Windows' ~15ms timer granularity - from accumulating
//! into a longer effective period over a 24/365 run; skipping (never firing
//! twice back-to-back to catch up) means a period missed while the task was
//! busy reading another group is simply absent. The gap surfaces as a
//! missing row in the store - which is the correct record-of-fact for a
//! recorder (recorder-requirements.md §3.1: a missed sample is a gap, not
//! something to back-fill with a burst).
//!
//! Because everything runs in one task with one `await` in flight at a time,
//! single-socket exclusivity is structural, not enforced by a lock.
//!
//! ## Ticks never stop, even while disconnected
//!
//! The scheduler keeps firing on every group's period regardless of
//! connection state. When the connection is down (or a reconnect is in
//! flight), a fired group appends an all-NULL row and marks its tags Bad
//! (recorder-requirements.md §3.1: "PLC 断で Bad を記録し続け" - the gap is an
//! explicit row of NULLs, present in the timeline, not a hole). Reconnection
//! runs in a *spawned* sub-task (so a slow `connect()` cannot stall the
//! scheduler) with exponential backoff (1s, 2s, 4s ... capped at 30s),
//! reset to immediate on a fresh drop and on any success.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use banto_plc::{ModbusTcpClient, PlcClient, PlcError, ReadResult, SlmpClient, TagValue};
use banto_tags::scale_raw;
use banto_tstore::{Clock, TsWriter};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::config::{ConnectionPlan, GroupPlan, ProtocolConfig};
use crate::current::{CurrentValuesHandle, Quality};
use crate::event::{CollectEvent, EventKind, EventSink, ThresholdLevel};

/// Reconnect backoff bounds (recorder-requirements.md §3.1: "失敗時バックオフ
/// 1s→2s→...上限30s、成功でリセット"). Parameterized so tests can shrink the
/// timings (design: "バックオフはテスト用に短縮可能なパラメータ化").
#[derive(Debug, Clone, Copy)]
pub struct BackoffConfig {
    pub base: Duration,
    pub max: Duration,
}

impl Default for BackoffConfig {
    fn default() -> Self {
        Self {
            base: Duration::from_secs(1),
            max: Duration::from_secs(30),
        }
    }
}

/// Backoff delay before connect attempt number `attempt` (1-based). Attempt 0
/// is immediate (the very first startup attempt, and the first retry right
/// after a fresh drop): `base * 2^(attempt-1)`, saturating and capped at
/// `max`. So attempt 1 -> `base`, 2 -> `2*base`, 3 -> `4*base`, ... -> `max`.
pub(crate) fn backoff_delay(attempt: u32, cfg: BackoffConfig) -> Duration {
    if attempt == 0 {
        return Duration::ZERO;
    }
    let factor = 1u64.checked_shl(attempt - 1).unwrap_or(u64::MAX);
    let ms = (cfg.base.as_millis() as u64).saturating_mul(factor);
    Duration::from_millis(ms).min(cfg.max)
}

/// Per-connection status (recorder-requirements.md §5's health display,
/// design: "Connected / Reconnecting{attempt} / Stopped"). `attempt` is the
/// number of the connect attempt currently in flight or scheduled (starts at
/// 1 for the initial connect).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionStatus {
    Connected,
    Reconnecting { attempt: u32 },
    Stopped,
}

/// Shared status map: each connection task writes its own key; `Collector`
/// reads the whole map. `std::sync::RwLock` for the same reason as the
/// current-value cache (short, non-async critical sections; sync readers).
pub(crate) type StatusMap = Arc<RwLock<HashMap<String, ConnectionStatus>>>;

/// Drop every status entry whose key is not in `keys` - the [`StatusMap`]
/// twin of [`crate::current::CurrentValuesHandle::retain`] (T7-1,
/// docs/tag-server-design.md §4.3: a connection removed by
/// [`crate::collector::Collector::apply_config`] must not leave a stale
/// `Stopped`/`Reconnecting` entry behind forever).
pub(crate) fn retain_status(status: &StatusMap, keys: &HashSet<String>) {
    status
        .write()
        .expect("status map lock poisoned")
        .retain(|k, _| keys.contains(k));
}

/// Everything one connection task shares with the rest of the engine.
pub(crate) struct TaskContext {
    /// The *current* writer, read fresh on every append via `borrow().clone()`
    /// rather than held for the task's lifetime (T7-1, docs/tag-server-design.md
    /// §4.3): [`crate::collector::Collector::apply_config`] can rotate the
    /// writer (a config change that alters the collected tag/group set - and
    /// therefore the frozen `StoreConfig` schema - forces a fresh file) while
    /// this connection's task keeps running untouched. A `watch` channel
    /// (not a `Mutex<Arc<TsWriter>>`) because the update is a simple
    /// broadcast-the-latest-value, not a read-modify-write. See
    /// `collector.rs`'s module doc for the full picture.
    pub writer_rx: watch::Receiver<Arc<TsWriter>>,
    pub clock: Arc<dyn Clock>,
    pub current: CurrentValuesHandle,
    pub events: EventSink,
    pub status: StatusMap,
    pub backoff: BackoffConfig,
    /// See [`ClientFactory`]'s doc comment.
    pub factory: ClientFactory,
}

/// The wire protocol one [`ClientSpec`] describes - the public twin of
/// `crate::config::Protocol` (that one is `pub(crate)`, tied to the registry
/// parsing step; this one is the minimal vocabulary a factory outside this
/// crate needs to dispatch on).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientProtocol {
    ModbusTcp,
    Slmp,
}

/// A connection's client-construction parameters, exposed as a thin `pub`
/// projection of the crate-private [`ConnectionPlan`]/[`ProtocolConfig`] (T2-2,
/// docs/tag-server-design.md §6-5) - a [`ClientFactory`] outside this crate
/// (banto-hub's broker adapter) cannot see those `pub(crate)` types, so this
/// carries everything [`default_client_factory`] needs to reproduce the exact
/// client the old hardcoded `build_client` built: not just "host/port" but
/// every field that actually varies a constructed client's behavior
/// (`unit_id` for Modbus, and the per-[`crate::collector::CollectorOptions`]
/// timeout overrides [`crate::collector::Collector::start_with_client_factory`]
/// already folded into `plan.config` before this is derived). `unit_id` is
/// meaningless for SLMP and always `0` there (SLMP has no such concept - see
/// `crate::config::slmp_config_for`'s doc comment).
#[derive(Debug, Clone)]
pub struct ClientSpec {
    /// `"conn:{id}"` - matches [`ConnectionPlan::key`] and every other
    /// `conn:`-keyed surface this crate exposes (status map, events).
    pub connection_key: String,
    pub protocol: ClientProtocol,
    pub host: String,
    pub port: u16,
    pub unit_id: u8,
    pub connect_timeout: Duration,
    pub response_timeout: Duration,
}

/// A caller-supplied seam for building the `PlcClient` a connection task
/// reconnects with (T2-2, docs/tag-server-design.md §6-5 「broker の読み取り
/// ハンドルを PlcClient trait のアダプタで包んでクライアント生成の差し替え口
/// (新設)から注入する」). [`crate::collector::Collector::start`] delegates to
/// [`crate::collector::Collector::start_with_client_factory`] with
/// [`default_client_factory`] - the same `ModbusTcpClient`/`SlmpClient`
/// construction the old hardcoded `build_client` did, byte-for-byte (every
/// field [`ClientSpec`] carries was already a field `build_client` read off
/// `plan.config`). banto-hub's own factory calls this crate's default for
/// Modbus connections and swaps in a `banto_broker`-backed adapter for SLMP
/// ones - see `apps/banto-hub/core/src/broker_glue.rs`'s module doc.
///
/// Called once per connect attempt (same call site `build_client` used to
/// occupy, inside `run_connection`'s `ConnEvent::Due` arm) - a factory may
/// freely return a fresh client every time, or close over shared state (e.g.
/// a `banto_broker::BrokerHandle` clone) to hand back an adapter around a
/// session that outlives any single attempt.
pub type ClientFactory = Arc<dyn Fn(&ClientSpec) -> Box<dyn PlcClient> + Send + Sync>;

/// Project a [`ConnectionPlan`]'s protocol config down to the public
/// [`ClientSpec`] a [`ClientFactory`] receives. Computed once per task start
/// (the plan is immutable for the task's lifetime), not per connect attempt.
fn client_spec(plan: &ConnectionPlan) -> ClientSpec {
    match &plan.config {
        ProtocolConfig::ModbusTcp(cfg) => ClientSpec {
            connection_key: plan.key.clone(),
            protocol: ClientProtocol::ModbusTcp,
            host: cfg.host.clone(),
            port: cfg.port,
            unit_id: cfg.unit_id,
            connect_timeout: cfg.connect_timeout,
            response_timeout: cfg.response_timeout,
        },
        ProtocolConfig::Slmp(cfg) => ClientSpec {
            connection_key: plan.key.clone(),
            protocol: ClientProtocol::Slmp,
            host: cfg.host.clone(),
            port: cfg.port,
            unit_id: 0,
            connect_timeout: cfg.connect_timeout,
            response_timeout: cfg.response_timeout,
        },
    }
}

/// The [`ClientFactory`] [`crate::collector::Collector::start`] uses -
/// reproduces the pre-T2-2 hardcoded `build_client` dispatch (design:
/// "プロトコル分岐は factory 関数に隔離") exactly, just re-homed behind the
/// public seam so `start` stays behaviorally unchanged for every existing
/// caller (T2-2 instructions: "既存呼び出し互換維持"). A new protocol is
/// still one new match arm here plus one `Protocol`/`ProtocolConfig`/
/// `ClientProtocol` variant.
pub fn default_client_factory() -> ClientFactory {
    Arc::new(|spec: &ClientSpec| -> Box<dyn PlcClient> {
        match spec.protocol {
            ClientProtocol::ModbusTcp => {
                Box::new(ModbusTcpClient::new(banto_plc::ModbusTcpConfig {
                    host: spec.host.clone(),
                    port: spec.port,
                    unit_id: spec.unit_id,
                    connect_timeout: spec.connect_timeout,
                    response_timeout: spec.response_timeout,
                    ..banto_plc::ModbusTcpConfig::default()
                }))
            }
            ClientProtocol::Slmp => Box::new(SlmpClient::new(banto_plc::SlmpConfig {
                host: spec.host.clone(),
                port: spec.port,
                connect_timeout: spec.connect_timeout,
                response_timeout: spec.response_timeout,
                ..banto_plc::SlmpConfig::default()
            })),
        }
    })
}

/// `(client, connect result)` handed back from a spawned connect attempt -
/// the client is returned either way so a failed attempt's socket is dropped
/// by us (uniform ownership), not left dangling in the sub-task.
type ConnectOutcome = (Box<dyn PlcClient>, Result<(), PlcError>);

/// Connection lifecycle state within one task.
enum ConnState {
    /// Waiting until `at` to spawn the next connect attempt.
    Backoff { at: Instant },
    /// A connect attempt is running in a spawned sub-task.
    Connecting(JoinHandle<ConnectOutcome>),
    /// Connected and owning the live client.
    Connected(Box<dyn PlcClient>),
}

/// What woke the connection side of the `select!`.
enum ConnEvent {
    /// A backoff window elapsed - time to spawn the next connect attempt.
    Due,
    /// A spawned connect attempt finished.
    Finished(ConnectOutcome),
    /// The spawned connect task itself panicked/was cancelled (treated as a
    /// failed attempt).
    JoinError,
}

/// Await the connection side's next event, borrowing `state` only for the
/// duration of the await (released before the `select!` handler runs, so the
/// group-fire handler can re-borrow `state` to read the client). `Connected`
/// has no pending connection event, so it parks forever here and only the
/// group scheduler (or stop) can win the `select!`.
async fn next_conn_event(state: &mut ConnState) -> ConnEvent {
    match state {
        ConnState::Connected(_) => std::future::pending().await,
        ConnState::Backoff { at } => {
            tokio::time::sleep_until(*at).await;
            ConnEvent::Due
        }
        ConnState::Connecting(handle) => match handle.await {
            Ok(outcome) => ConnEvent::Finished(outcome),
            Err(_join_err) => ConnEvent::JoinError,
        },
    }
}

fn set_status(ctx: &TaskContext, key: &str, status: ConnectionStatus) {
    ctx.status
        .write()
        .expect("status map lock poisoned")
        .insert(key.to_string(), status);
}

/// Run one connection's collection loop until `stop_rx` flips to `true`.
pub(crate) async fn run_connection(
    plan: ConnectionPlan,
    ctx: TaskContext,
    mut stop_rx: watch::Receiver<bool>,
) {
    let conn_key = plan.key.clone();
    let group_count = plan.groups.len();
    // Computed once - the plan (and therefore the spec derived from it) is
    // immutable for the task's lifetime; see `ClientSpec`'s doc comment.
    let spec = client_spec(&plan);

    // Per-group next-fire deadlines (all "now" => every group fires
    // immediately on entry) and per-group per-tag active threshold levels.
    let start = Instant::now();
    let mut next_fire: Vec<Instant> = vec![start; group_count];
    let mut threshold_state: Vec<Vec<Option<ThresholdLevel>>> = plan
        .groups
        .iter()
        .map(|g| vec![None; g.tags.len()])
        .collect();

    // Start out wanting to connect immediately.
    let mut attempt: u32 = 0;
    let mut ever_connected = false;
    let mut state = ConnState::Backoff { at: start };
    set_status(
        &ctx,
        &conn_key,
        ConnectionStatus::Reconnecting { attempt: 1 },
    );

    loop {
        // Soonest group deadline (group_count >= 1 is guaranteed by
        // build_config, which skips connections with no groups; the fallback
        // keeps this total for safety).
        let soonest = next_fire
            .iter()
            .copied()
            .min()
            .unwrap_or_else(|| start + Duration::from_secs(3600));

        tokio::select! {
            biased;

            // Stop first: a stop request should win over pending ticks.
            _ = stop_rx.changed() => {
                if *stop_rx.borrow() {
                    break;
                }
            }

            conn_event = next_conn_event(&mut state) => {
                match conn_event {
                    ConnEvent::Due => {
                        // Spawn the connect off-task so a slow connect() never
                        // stalls the group scheduler.
                        attempt += 1;
                        set_status(
                            &ctx,
                            &conn_key,
                            ConnectionStatus::Reconnecting { attempt },
                        );
                        let mut client = (ctx.factory)(&spec);
                        let handle = tokio::spawn(async move {
                            let result = client.connect().await;
                            (client, result)
                        });
                        state = ConnState::Connecting(handle);
                    }
                    ConnEvent::Finished((client, Ok(()))) => {
                        let now_ms = ctx.clock.now_ms();
                        let kind = if ever_connected {
                            EventKind::PlcReconnected
                        } else {
                            EventKind::PlcConnected
                        };
                        ever_connected = true;
                        attempt = 0;
                        state = ConnState::Connected(client);
                        set_status(&ctx, &conn_key, ConnectionStatus::Connected);
                        ctx.events
                            .emit(CollectEvent::connection(now_ms, kind, conn_key.clone(), None))
                            .await;
                    }
                    ConnEvent::Finished((_, Err(_))) | ConnEvent::JoinError => {
                        // Failed attempt: drop the client, back off before the
                        // next try. No disconnect event here - we were already
                        // disconnected/never-connected (the drop that started
                        // this reconnect already emitted plc_disconnected).
                        let delay = backoff_delay(attempt, ctx.backoff);
                        state = ConnState::Backoff {
                            at: Instant::now() + delay,
                        };
                        set_status(
                            &ctx,
                            &conn_key,
                            ConnectionStatus::Reconnecting { attempt: attempt + 1 },
                        );
                    }
                }
            }

            _ = tokio::time::sleep_until(soonest) => {
                let now = Instant::now();
                let ptime_ms = ctx.clock.now_ms();

                for i in 0..group_count {
                    if next_fire[i] > now {
                        continue;
                    }
                    // MissedTickBehavior::Skip: advance by whole periods from
                    // the original phase until the deadline is in the future.
                    // Phase-aligned (no per-tick latency drift) but never
                    // fires twice back-to-back to catch up - see module doc.
                    while next_fire[i] <= now {
                        next_fire[i] += plan.groups[i].period;
                    }

                    // Read this group iff currently connected. The match's
                    // borrow of `state` is released at the end of the
                    // statement, before we may reassign `state` below.
                    let read_outcome: Option<Result<Vec<ReadResult>, PlcError>> =
                        match &mut state {
                            ConnState::Connected(client) => {
                                Some(client.read_batch(&plan.groups[i].requests).await)
                            }
                            _ => None,
                        };

                    match read_outcome {
                        Some(Ok(results)) => {
                            record_group(
                                &plan.groups[i],
                                Some(&results),
                                ptime_ms,
                                &ctx,
                                &conn_key,
                                &mut threshold_state[i],
                            )
                            .await;
                        }
                        Some(Err(err)) => {
                            // Connection-fatal read failure: record this tick
                            // as all-Bad, emit the disconnect, and drop into an
                            // immediate reconnect (attempt reset to 0).
                            record_group(
                                &plan.groups[i],
                                None,
                                ptime_ms,
                                &ctx,
                                &conn_key,
                                &mut threshold_state[i],
                            )
                            .await;
                            ctx.events
                                .emit(CollectEvent::connection(
                                    ptime_ms,
                                    EventKind::PlcDisconnected,
                                    conn_key.clone(),
                                    Some(err.to_string()),
                                ))
                                .await;
                            attempt = 0;
                            state = ConnState::Backoff { at: Instant::now() };
                            set_status(
                                &ctx,
                                &conn_key,
                                ConnectionStatus::Reconnecting { attempt: 1 },
                            );
                        }
                        None => {
                            // Disconnected/reconnecting: keep the timeline
                            // going with an all-NULL row.
                            record_group(
                                &plan.groups[i],
                                None,
                                ptime_ms,
                                &ctx,
                                &conn_key,
                                &mut threshold_state[i],
                            )
                            .await;
                        }
                    }
                }
            }
        }
    }

    // Graceful task exit: close the socket if we still hold one, and mark the
    // connection Stopped.
    if let ConnState::Connected(mut client) = state {
        client.disconnect().await;
    }
    set_status(&ctx, &conn_key, ConnectionStatus::Stopped);
}

/// Turn one group's read outcome into a stored row, cache updates, and
/// threshold edge events. `results = None` means "no reading this tick"
/// (disconnected or fatal): every tag becomes `None`/Bad. Threshold state is
/// only evaluated on a Good numeric reading - a Bad/absent reading leaves the
/// active level untouched (a comms loss must not spuriously "clear" an alarm).
async fn record_group(
    group: &GroupPlan,
    results: Option<&[ReadResult]>,
    ptime_ms: i64,
    ctx: &TaskContext,
    conn_key: &str,
    threshold_state: &mut [Option<ThresholdLevel>],
) {
    let mut values: Vec<Option<f64>> = Vec::with_capacity(group.tags.len());

    for (idx, tag) in group.tags.iter().enumerate() {
        let (value, quality) = match results.map(|r| &r[idx]) {
            Some(ReadResult::Value(TagValue::Bit(b))) => {
                (Some(if *b { 1.0 } else { 0.0 }), Quality::Good)
            }
            Some(ReadResult::Value(TagValue::F64(raw))) => {
                let scaled = match &tag.scaling {
                    Some(s) => scale_raw(*raw, s),
                    None => *raw,
                };
                (Some(scaled), Quality::Good)
            }
            // Per-tag Bad within an otherwise-good batch, or the whole tick
            // absent (results = None).
            Some(ReadResult::Bad(_)) | None => (None, Quality::Bad),
        };

        values.push(value);
        ctx.current
            .set(&tag.key, value, ptime_ms, quality, group.period_ms);

        if !tag.thresholds.is_empty() {
            if let Some(v) = value {
                let new_level = classify_threshold(v, tag);
                let prev = threshold_state[idx];
                if prev != new_level {
                    if let Some(old) = prev {
                        ctx.events
                            .emit(CollectEvent::threshold(
                                ptime_ms,
                                EventKind::ThresholdCleared,
                                conn_key,
                                &tag.key,
                                old,
                                v,
                            ))
                            .await;
                    }
                    if let Some(level) = new_level {
                        ctx.events
                            .emit(CollectEvent::threshold(
                                ptime_ms,
                                EventKind::ThresholdEntered,
                                conn_key,
                                &tag.key,
                                level,
                                v,
                            ))
                            .await;
                    }
                    threshold_state[idx] = new_level;
                }
            }
        }
    }

    // Swallow append failures rather than tear the loop down: a 24/365
    // recorder keeps collecting through a transient storage hiccup (the
    // in-memory cache and live events still flow). A persistent failure
    // (e.g. disk full) is an operational condition surfaced elsewhere, not a
    // reason to kill this connection's collection.
    //
    // Re-borrowed fresh on every call (T7-1: `apply_config` may have rotated
    // the writer since the last tick) - the `Ref` guard from `borrow()` is
    // dropped at the end of this statement, before the `.await`, so it never
    // needs to be `Send` across an await point.
    let writer = ctx.writer_rx.borrow().clone();
    let _ = writer.append(&group.key, ptime_ms, &values).await;
}

/// Classify a scaled value against a tag's H/HH/L/LL limits. High bands take
/// precedence over low (a value cannot satisfy both given the enforced
/// `ll <= l <= h <= hh` ordering), and HH/LL over H/L.
fn classify_threshold(value: f64, tag: &crate::config::TagPlan) -> Option<ThresholdLevel> {
    let t = &tag.thresholds;
    if let Some(hh) = t.hh {
        if value >= hh {
            return Some(ThresholdLevel::Hh);
        }
    }
    if let Some(h) = t.h {
        if value >= h {
            return Some(ThresholdLevel::H);
        }
    }
    if let Some(ll) = t.ll {
        if value <= ll {
            return Some(ThresholdLevel::Ll);
        }
    }
    if let Some(l) = t.l {
        if value <= l {
            return Some(ThresholdLevel::L);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Thresholds;

    fn cfg() -> BackoffConfig {
        BackoffConfig {
            base: Duration::from_secs(1),
            max: Duration::from_secs(30),
        }
    }

    #[test]
    fn backoff_attempt_zero_is_immediate() {
        assert_eq!(backoff_delay(0, cfg()), Duration::ZERO);
    }

    #[test]
    fn backoff_doubles_each_attempt() {
        assert_eq!(backoff_delay(1, cfg()), Duration::from_secs(1));
        assert_eq!(backoff_delay(2, cfg()), Duration::from_secs(2));
        assert_eq!(backoff_delay(3, cfg()), Duration::from_secs(4));
        assert_eq!(backoff_delay(4, cfg()), Duration::from_secs(8));
        assert_eq!(backoff_delay(5, cfg()), Duration::from_secs(16));
    }

    #[test]
    fn backoff_caps_at_max() {
        assert_eq!(backoff_delay(6, cfg()), Duration::from_secs(30)); // 32 -> capped
        assert_eq!(backoff_delay(100, cfg()), Duration::from_secs(30)); // no overflow
    }

    /// Virtual-time proof (tokio::time::pause) that waiting out the whole
    /// 1s -> 30s backoff ladder is driven purely by tokio's timer - the same
    /// `sleep_until` mechanism `next_conn_event`'s Backoff arm uses - and
    /// sums to the expected deterministic total. The socket-facing reconnect
    /// behaviour itself is exercised end-to-end (real time, real simulator)
    /// in tests/integration.rs; virtual time cannot drive a real TCP peer.
    #[tokio::test(start_paused = true)]
    async fn backoff_ladder_advances_virtual_time_deterministically() {
        let start = Instant::now();
        for attempt in 1..=7 {
            let at = Instant::now() + backoff_delay(attempt, cfg());
            tokio::time::sleep_until(at).await;
        }
        // 1 + 2 + 4 + 8 + 16 + 30 + 30 = 91s of virtual time, instantly.
        assert_eq!(start.elapsed(), Duration::from_secs(91));
    }

    fn tag_with(thresholds: Thresholds) -> crate::config::TagPlan {
        crate::config::TagPlan {
            key: "tag:1".to_string(),
            scaling: None,
            thresholds,
        }
    }

    #[test]
    fn classify_high_bands() {
        let tag = tag_with(Thresholds {
            hh: Some(100.0),
            h: Some(80.0),
            l: Some(20.0),
            ll: Some(0.0),
        });
        assert_eq!(classify_threshold(50.0, &tag), None);
        assert_eq!(classify_threshold(80.0, &tag), Some(ThresholdLevel::H));
        assert_eq!(classify_threshold(99.0, &tag), Some(ThresholdLevel::H));
        assert_eq!(classify_threshold(100.0, &tag), Some(ThresholdLevel::Hh));
        assert_eq!(classify_threshold(20.0, &tag), Some(ThresholdLevel::L));
        assert_eq!(classify_threshold(0.0, &tag), Some(ThresholdLevel::Ll));
    }

    #[test]
    fn classify_with_only_some_limits_set() {
        let tag = tag_with(Thresholds {
            hh: None,
            h: Some(80.0),
            l: None,
            ll: None,
        });
        assert_eq!(classify_threshold(90.0, &tag), Some(ThresholdLevel::H));
        assert_eq!(classify_threshold(10.0, &tag), None);
    }
}
