//! 接続ごとの送る速さの抑え（docs/banto-hub-ingest-design.md §7.1 の (a)、§12 Q20 = 8 割）。
//!
//! Hub は接続ごとに 2 つのトークンバケット（メッセージ 20 / 秒・瞬間 40、件数 5,000 / 秒・
//! 瞬間 10,000）で受け、超えたメッセージを `rate_limited` で全体ごと断る（§7）。ブリッジは
//! その手前で、`welcome` の `limits` の**8 割**の速さのバケットを 2 つ持ち、足りない分は
//! 送るのを待つ（断られて値が欠けるより、待って遅れる方がよい。待ちが周期を超えたら
//! 古い周期を捨てるのは [`super::outbox`]）。
//!
//! | バケット   | 持続（`welcome` × 80 %） | 容量                          |
//! | ---------- | ------------------------ | ----------------------------- |
//! | メッセージ | 16 / 秒                  | 16（1 秒分）                  |
//! | 件数       | 4,000 / 秒               | max(4,000（1 秒分）, 1,000)   |
//!
//! **容量を 1 秒分にする理由**: このバケットが出す量は、どの長さ T の区間でも
//! 「容量 + 持続 × T」（16 + 16T 件のメッセージ）を超えない。Hub のバケット（40 + 20T）は
//! 満杯から始まり、送り手が 1 つならこの範囲の入力を 1 つも断らない（入力の瞬間の量
//! 16 ≤ 40・持続 16 ≤ 20）。ブリッジのプロセスが再起動してこちらのバケットが満杯に
//! 戻っても、再起動をまたぐ区間で 16 + 16 + 16T ≤ 40 + 20T なので断られない。件数も
//! 4,000 + 4,000 ≤ 10,000。テストが Hub の実装（`banto_hub_core::ingest::limits::
//! ConnectionLimiter`）に同じ並びを流して、1 件も断られないことを確かめる。件数の容量は
//! 1 メッセージの上限（1,000 件）以上にする（それより小さいと最大のメッセージが永久に
//! 送れない）。
//!
//! 割合 [`HEADROOM_PERCENT`] は登録時の見積もり（`banto_tags::PUSH_LIMITS.
//! registration_headroom_percent`）と同じ値で、テストが突き合わせる。`ping`・`status` は
//! Hub のバケットを使わない（§7、R2a-2d）ので、ここでも数えない。

use std::time::Duration;

use tokio::time::Instant;

use crate::source::wire::WireLimits;

/// `welcome` の上限のうち使ってよい割合（%）。`banto_tags::PUSH_LIMITS.
/// registration_headroom_percent` と同じ（§12 Q20）。
pub const HEADROOM_PERCENT: u64 = 80;

/// 1 つのトークンバケット（浮動小数。持続 `rate` / 秒、容量 `capacity`。満杯から始める）。
#[derive(Debug, Clone)]
struct Bucket {
    rate: f64,
    capacity: f64,
    tokens: f64,
    last: Instant,
}

impl Bucket {
    fn new(rate: f64, capacity: f64, now: Instant) -> Self {
        Self {
            rate,
            capacity,
            tokens: capacity,
            last: now,
        }
    }

    fn refill(&mut self, now: Instant) {
        if now > self.last {
            let elapsed = now.duration_since(self.last).as_secs_f64();
            self.tokens = (self.tokens + elapsed * self.rate).min(self.capacity);
            self.last = now;
        }
    }

    /// `count` 個が貯まるまでの待ち（貯まっていれば 0）。
    fn wait_for(&mut self, count: f64, now: Instant) -> Duration {
        self.refill(now);
        if self.tokens >= count || self.rate <= 0.0 {
            return Duration::ZERO;
        }
        // 丸めで「まだ足りない」が続かないよう 1 ms 切り上げる。
        Duration::from_secs_f64((count - self.tokens) / self.rate) + Duration::from_millis(1)
    }

    fn take(&mut self, count: f64, now: Instant) {
        self.refill(now);
        self.tokens -= count;
    }
}

/// 1 つの source（= Hub の `push` 接続 1 つ）の送る速さ。WS の切断・再接続をまたいで
/// 持ち続ける（Hub のバケットも接続の slot にあり、WS をまたいで続く）。
#[derive(Debug, Clone)]
pub struct SendBudget {
    limits: WireLimits,
    messages: Bucket,
    items: Bucket,
}

impl SendBudget {
    pub fn new(limits: WireLimits, now: Instant) -> Self {
        let message_rate = limits.messages_per_sec.saturating_mul(HEADROOM_PERCENT) as f64 / 100.0;
        let item_rate = limits.items_per_sec.saturating_mul(HEADROOM_PERCENT) as f64 / 100.0;
        Self {
            limits,
            messages: Bucket::new(message_rate, message_rate.max(1.0), now),
            items: Bucket::new(
                item_rate,
                item_rate.max(limits.max_items.max(1) as f64),
                now,
            ),
        }
    }

    /// 作ったときの上限（`welcome` が変わったら作り直すための比較用）。
    pub fn limits(&self) -> WireLimits {
        self.limits
    }

    /// `items` 件の 1 メッセージを送れるまでの待ち（今送れるなら 0）。
    pub fn wait_for(&mut self, items: usize, now: Instant) -> Duration {
        let messages = self.messages.wait_for(1.0, now);
        let items = self.items.wait_for(items as f64, now);
        messages.max(items)
    }

    /// `items` 件の 1 メッセージを送った（[`Self::wait_for`] が 0 のときだけ呼ぶ）。
    pub fn take(&mut self, items: usize, now: Instant) {
        self.messages.take(1.0, now);
        self.items.take(items as f64, now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use banto_hub_core::ingest::limits::ConnectionLimiter;
    use banto_tags::PUSH_LIMITS;

    fn hub_limits() -> WireLimits {
        WireLimits {
            max_items: PUSH_LIMITS.max_items_per_message,
            max_message_bytes: PUSH_LIMITS.max_ws_message_bytes,
            messages_per_sec: PUSH_LIMITS.messages_per_sec,
            items_per_sec: PUSH_LIMITS.items_per_sec,
        }
    }

    #[test]
    fn the_headroom_matches_the_registration_estimate() {
        assert_eq!(HEADROOM_PERCENT, PUSH_LIMITS.registration_headroom_percent);
    }

    /// 送れるだけ送り続ける送り手を `duration` の間ならべ、送った時刻（開始からの ms）を返す。
    fn greedy(items_per_message: usize, duration: Duration) -> Vec<(u64, usize)> {
        let start = Instant::now();
        let mut budget = SendBudget::new(hub_limits(), start);
        let mut now = start;
        let mut sent = Vec::new();
        while now.duration_since(start) < duration {
            let wait = budget.wait_for(items_per_message, now);
            now += wait;
            if now.duration_since(start) >= duration {
                break;
            }
            budget.take(items_per_message, now);
            sent.push((
                now.duration_since(start).as_millis() as u64,
                items_per_message,
            ));
        }
        sent
    }

    /// §11 の R2a-3b: トークンバケットが 8 割を超えない（長い区間の平均で 16 メッセージ /
    /// 秒・4,000 件 / 秒、瞬間は容量の 1 秒分まで）。
    #[test]
    fn a_greedy_sender_stays_within_eighty_percent() {
        let seconds = 60u64;
        // メッセージで縛られる（1 件ずつ）。
        let small = greedy(1, Duration::from_secs(seconds));
        assert!(
            small.len() as u64 <= 16 * seconds + 16,
            "{} messages",
            small.len()
        );
        assert!(small.len() as u64 >= 16 * seconds - 1);
        // 件数で縛られる（1,000 件ずつ）。
        let large = greedy(1_000, Duration::from_secs(seconds));
        let items: u64 = large.iter().map(|(_, n)| *n as u64).sum();
        assert!(items <= 4_000 * seconds + 4_000, "{items} items");
        assert!(items >= 4_000 * (seconds - 1));
        // どの 1 秒の窓でも、容量（1 秒分）+ 持続 1 秒分を超えない。
        for trace in [&small, &large] {
            for (i, (t0, _)) in trace.iter().enumerate() {
                let window: Vec<_> = trace[i..]
                    .iter()
                    .take_while(|(t, _)| *t < t0 + 1_000)
                    .collect();
                assert!(window.len() <= 32, "{} messages in 1 s", window.len());
                let n: usize = window.iter().map(|(_, n)| *n).sum();
                assert!(n <= 8_000, "{n} items in 1 s");
            }
        }
    }

    /// Hub の実装のバケット（`ConnectionLimiter`）に同じ並びを流すと 1 件も断られない。
    /// ブリッジの再起動（こちらのバケットだけ満杯に戻る）をまたいでも。
    #[test]
    fn the_hub_limiter_never_refuses_the_budgeted_stream() {
        for items_per_message in [1usize, 250, 900, 1_000] {
            let mut hub = ConnectionLimiter::new(PUSH_LIMITS);
            let first = greedy(items_per_message, Duration::from_secs(30));
            let restart_at = 30_000;
            let second = greedy(items_per_message, Duration::from_secs(30));
            let stream = first
                .iter()
                .copied()
                .chain(second.iter().map(|(t, n)| (t + restart_at, *n)));
            for (t, n) in stream {
                let admission = hub.admit(n, t as i64);
                assert!(
                    admission.outcome.is_ok(),
                    "refused at {t} ms ({n} items/message): {:?}",
                    admission.outcome
                );
            }
        }
    }

    /// 送れる分が貯まっていれば待ちは 0、使い切ったら持続の速さで待つ。
    #[test]
    fn waits_follow_the_sustained_rate() {
        let now = Instant::now();
        let mut budget = SendBudget::new(hub_limits(), now);
        for _ in 0..16 {
            assert_eq!(budget.wait_for(1, now), Duration::ZERO);
            budget.take(1, now);
        }
        let wait = budget.wait_for(1, now);
        assert!(
            wait >= Duration::from_millis(62) && wait <= Duration::from_millis(64),
            "{wait:?}"
        );
        // 件数: 4,000 を使い切ったら 1,000 件には 250 ms。
        let mut budget = SendBudget::new(hub_limits(), now);
        budget.take(1_000, now);
        budget.take(1_000, now);
        budget.take(1_000, now);
        budget.take(1_000, now);
        let wait = budget.wait_for(1_000, now);
        assert!(
            wait >= Duration::from_millis(250) && wait <= Duration::from_millis(252),
            "{wait:?}"
        );
    }
}
