//! The write rate limiter / circuit breaker (W3-B safety invariant #4,
//! `luminous-discovering-goblet.md`). A sliding-window cap on physical writes,
//! enforced BOTH globally and per PLC connection. When a would-be write would
//! exceed either cap the breaker *trips*: the caller
//! ([`crate::engine::writer`]) suppresses that write, auto-disarms the engine,
//! and records a `rate_limit_tripped` audit row. Tripping requires a manual
//! re-arm - the limiter never silently drops-and-retries.
//!
//! ## Pure and single-threaded by construction
//!
//! Every method takes an explicit `now: Instant`, so this type holds no clock
//! and its behaviour is fully deterministic in tests (no `tokio::time` needed -
//! a test builds its own `Instant` ladder with plain `Duration` arithmetic).
//! The writer owns the one and only [`RateLimiter`] and drives it from its
//! single task, so no interior locking is required: `would_exceed`/`record` are
//! `&mut self`.
//!
//! ## Peek-then-record, not check-and-consume
//!
//! [`RateLimiter::would_exceed`] only *inspects* the windows; [`RateLimiter::record`]
//! is what actually consumes a slot. The writer peeks before a write and
//! records only once the write path is truly taken (armed, not dry-run), so a
//! **dry-run never consumes budget** and therefore can never trip the breaker -
//! dry-run makes no physical writes, so there is no write storm to guard
//! against. Because the writer is single-tasked, the peek and the later record
//! cannot race.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

/// Caps and window for [`RateLimiter`]. Defaults are deliberately conservative
/// for an app that writes to live industrial PLCs: a handful of writes a minute
/// is already a lot of automatic actuation.
#[derive(Debug, Clone, Copy)]
pub struct RateLimitConfig {
    /// The sliding window each cap is measured over.
    pub window: Duration,
    /// Max physical writes within `window` across ALL connections.
    pub global_max: usize,
    /// Max physical writes within `window` to any ONE connection.
    pub per_connection_max: usize,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            window: Duration::from_secs(60),
            global_max: 30,
            per_connection_max: 10,
        }
    }
}

/// Sliding-window write counter. Timestamps of recent writes are kept in
/// per-window `VecDeque`s and pruned lazily on each query/record.
pub struct RateLimiter {
    config: RateLimitConfig,
    global: VecDeque<Instant>,
    per_connection: HashMap<i64, VecDeque<Instant>>,
}

impl RateLimiter {
    pub fn new(config: RateLimitConfig) -> Self {
        Self {
            config,
            global: VecDeque::new(),
            per_connection: HashMap::new(),
        }
    }

    pub fn config(&self) -> RateLimitConfig {
        self.config
    }

    /// Drop timestamps at or beyond `window` old from the front of `q`.
    fn prune(q: &mut VecDeque<Instant>, now: Instant, window: Duration) {
        while let Some(&front) = q.front() {
            if now.saturating_duration_since(front) >= window {
                q.pop_front();
            } else {
                break;
            }
        }
    }

    /// Would recording one more write to `connection_id` right now exceed
    /// either the global or the per-connection cap? Prunes stale entries as a
    /// side effect but records nothing.
    pub fn would_exceed(&mut self, connection_id: i64, now: Instant) -> bool {
        let window = self.config.window;
        Self::prune(&mut self.global, now, window);
        if self.global.len() >= self.config.global_max {
            return true;
        }
        let per = self.per_connection.entry(connection_id).or_default();
        Self::prune(per, now, window);
        per.len() >= self.config.per_connection_max
    }

    /// Record one physical write to `connection_id` at `now` (counts against
    /// both the global and the per-connection window). Call this ONLY when a
    /// real write is actually being issued.
    pub fn record(&mut self, connection_id: i64, now: Instant) {
        self.global.push_back(now);
        self.per_connection
            .entry(connection_id)
            .or_default()
            .push_back(now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(global_max: usize, per_connection_max: usize) -> RateLimitConfig {
        RateLimitConfig {
            window: Duration::from_secs(60),
            global_max,
            per_connection_max,
        }
    }

    #[test]
    fn per_connection_cap_trips_after_max_writes() {
        let mut rl = RateLimiter::new(cfg(100, 3));
        let t = Instant::now();
        assert!(!rl.would_exceed(1, t));
        rl.record(1, t);
        rl.record(1, t);
        rl.record(1, t);
        // Three writes recorded; the fourth would exceed.
        assert!(rl.would_exceed(1, t));
        // A different connection is unaffected by connection 1's budget.
        assert!(!rl.would_exceed(2, t));
    }

    #[test]
    fn global_cap_trips_across_connections() {
        let mut rl = RateLimiter::new(cfg(3, 100));
        let t = Instant::now();
        rl.record(1, t);
        rl.record(2, t);
        rl.record(3, t);
        // Global budget of 3 exhausted regardless of connection.
        assert!(rl.would_exceed(4, t));
    }

    #[test]
    fn window_slides_so_old_writes_stop_counting() {
        let mut rl = RateLimiter::new(cfg(100, 2));
        let t0 = Instant::now();
        rl.record(1, t0);
        rl.record(1, t0);
        assert!(rl.would_exceed(1, t0), "at cap within the window");

        // 61s later the two writes have aged out of the 60s window.
        let t1 = t0 + Duration::from_secs(61);
        assert!(
            !rl.would_exceed(1, t1),
            "old writes should have slid out of the window"
        );
    }
}
