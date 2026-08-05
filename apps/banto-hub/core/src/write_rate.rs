//! 書き込みレート制限 (docs/tag-server-design.md §6-4「レート制限
//! ブレーカ」、§6 実装指示 §3)。relay-wright の `engine/rate_limiter.rs`
//! (コネクション毎 + 全体の2段スライディングウィンドウ) を、hub の語彙で
//! **タグ毎 + 全体**の2段に読み替えて移植したもの。構造・意味論・
//! テストの形は relay-wright のものと同一 (キーが `connection_id` から
//! `tag_id` に変わっただけ)。
//!
//! ## Pure・クロック非所有
//!
//! [`WriteRateLimiter`] のメソッドはすべて明示的な `now: Instant` を
//! 受け取り、自分では時計を持たない。`crate::rest` の書き込みハンドラは
//! 本番では `Instant::now()` を渡すだけで、この型自体は
//! `tokio::time`/壁時計に一切依存しない - 単体テストは
//! `Duration` 算術だけで完全に決定論的に組める(relay-wright の
//! `rate_limiter.rs` のテストと同じ手法)。
//!
//! ## peek (`would_exceed`) と record を分離する
//!
//! [`WriteRateLimiter::would_exceed`] はウィンドウを覗くだけで何も
//! 記録しない。実際の書き込みバジェットを消費するのは
//! [`WriteRateLimiter::record`] のみ - `crate::rest` の書き込みハンドラは
//! ゲート順(§6 実装指示の6番)で「超過するなら 429 + トリップ」を
//! `would_exceed` で判定し、そのゲートを通過して初めて(物理書き込み
//! 直前に)`record` を呼ぶ(「ゲート通過後・物理書き込み前」= 実際に
//! 試みた書き込みだけを数える意味論。relay-wright の dry-run が
//! バジェットを消費しないのと同じ理由付け)。
//!
//! ## 既定値 (T2 では固定・設定不可、2026-08-05 判断)
//!
//! `window` 60秒 / `global_max` 30 / `per_tag_max` 10 -
//! §6 に具体的な数値の指定はないため、relay-wright の
//! `RateLimitConfig::default()` をそのまま踏襲した(「一分間に数件の
//! 書き込みでも実機への自動書き込みとしては十分多い」という
//! relay-wright 側の既定の理由付けがそのまま当てはまる)。T2では
//! `settings` 経由で変更可能にはしない- 固定値。将来 hub 独自の
//! チューニングが必要になった時点で settings に昇格させる。

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

/// [`WriteRateLimiter`] の上限とウィンドウ幅。
#[derive(Debug, Clone, Copy)]
pub struct WriteRateLimitConfig {
    /// 各上限を測るスライディングウィンドウ幅。
    pub window: Duration,
    /// `window` 内の全タグ合計の書き込み上限。
    pub global_max: usize,
    /// `window` 内の1タグあたりの書き込み上限。
    pub per_tag_max: usize,
}

impl Default for WriteRateLimitConfig {
    fn default() -> Self {
        Self {
            window: Duration::from_secs(60),
            global_max: 30,
            per_tag_max: 10,
        }
    }
}

/// スライディングウィンドウ書き込みカウンタ(タグ毎 + 全体の2段)。
/// 直近の書き込み時刻を `VecDeque` に保持し、問い合わせ/記録の都度
/// 古いものを先頭から遅延 prune する。
pub struct WriteRateLimiter {
    config: WriteRateLimitConfig,
    global: VecDeque<Instant>,
    per_tag: HashMap<i64, VecDeque<Instant>>,
}

impl WriteRateLimiter {
    pub fn new(config: WriteRateLimitConfig) -> Self {
        Self {
            config,
            global: VecDeque::new(),
            per_tag: HashMap::new(),
        }
    }

    pub fn config(&self) -> WriteRateLimitConfig {
        self.config
    }

    /// `q` の先頭から `window` 以上経過したタイムスタンプを取り除く。
    fn prune(q: &mut VecDeque<Instant>, now: Instant, window: Duration) {
        while let Some(&front) = q.front() {
            if now.saturating_duration_since(front) >= window {
                q.pop_front();
            } else {
                break;
            }
        }
    }

    /// 今 `tag_id` への書き込みを1件追加で記録したら、全体またはタグ毎の
    /// 上限を超えるか。副作用として prune するが記録はしない。
    pub fn would_exceed(&mut self, tag_id: i64, now: Instant) -> bool {
        let window = self.config.window;
        Self::prune(&mut self.global, now, window);
        if self.global.len() >= self.config.global_max {
            return true;
        }
        let per = self.per_tag.entry(tag_id).or_default();
        Self::prune(per, now, window);
        per.len() >= self.config.per_tag_max
    }

    /// `tag_id` への物理書き込み1件を `now` で記録する(全体・タグ毎
    /// 両方のウィンドウにカウントされる)。**実際に書き込みを試みた
    /// 時にのみ**呼ぶこと(このモジュールの doc comment 「peek と
    /// record を分離する」参照)。
    pub fn record(&mut self, tag_id: i64, now: Instant) {
        self.global.push_back(now);
        self.per_tag.entry(tag_id).or_default().push_back(now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(global_max: usize, per_tag_max: usize) -> WriteRateLimitConfig {
        WriteRateLimitConfig {
            window: Duration::from_secs(60),
            global_max,
            per_tag_max,
        }
    }

    #[test]
    fn per_tag_cap_trips_after_max_writes() {
        let mut rl = WriteRateLimiter::new(cfg(100, 3));
        let t = Instant::now();
        assert!(!rl.would_exceed(1, t));
        rl.record(1, t);
        rl.record(1, t);
        rl.record(1, t);
        // Three writes recorded; the fourth would exceed.
        assert!(rl.would_exceed(1, t));
        // A different tag is unaffected by tag 1's budget.
        assert!(!rl.would_exceed(2, t));
    }

    #[test]
    fn global_cap_trips_across_tags() {
        let mut rl = WriteRateLimiter::new(cfg(3, 100));
        let t = Instant::now();
        rl.record(1, t);
        rl.record(2, t);
        rl.record(3, t);
        // Global budget of 3 exhausted regardless of tag.
        assert!(rl.would_exceed(4, t));
    }

    #[test]
    fn window_slides_so_old_writes_stop_counting() {
        let mut rl = WriteRateLimiter::new(cfg(100, 2));
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

    #[test]
    fn window_boundary_is_inclusive_a_write_exactly_window_old_stops_counting() {
        let mut rl = WriteRateLimiter::new(cfg(100, 1));
        let t0 = Instant::now();
        rl.record(1, t0);
        assert!(
            rl.would_exceed(1, t0 + Duration::from_secs(59)),
            "at 59s the write is still inside the window"
        );
        assert!(
            !rl.would_exceed(1, t0 + Duration::from_secs(60)),
            "at exactly 60s the write has aged out"
        );
    }

    #[test]
    fn partial_slide_frees_exactly_the_aged_out_slots() {
        let mut rl = WriteRateLimiter::new(cfg(100, 2));
        let t0 = Instant::now();
        rl.record(1, t0);
        rl.record(1, t0 + Duration::from_secs(30));
        assert!(
            rl.would_exceed(1, t0 + Duration::from_secs(59)),
            "both in window"
        );

        let t1 = t0 + Duration::from_secs(61);
        assert!(!rl.would_exceed(1, t1), "one slot freed by the slide");
        rl.record(1, t1);
        assert!(
            rl.would_exceed(1, t1),
            "consuming the freed slot fills the window again (t0+30s write still counts)"
        );
    }

    #[test]
    fn default_config_matches_relay_wright_defaults() {
        let cfg = WriteRateLimitConfig::default();
        assert_eq!(cfg.window, Duration::from_secs(60));
        assert_eq!(cfg.global_max, 30);
        assert_eq!(cfg.per_tag_max, 10);
    }
}
