//! 送信口の上限（R2a-2c、docs/banto-hub-ingest-design.md §7・§12 Q4）。
//!
//! 数値は `banto_tags::push::PUSH_LIMITS` の 1 か所だけ（登録時の見積もり
//! `bridge_load`・ブリッジの分割と同じ値。§7.1、オーナーの監査 (8)）。ここは判定だけを
//! 持ち、状態（[`ConnectionLimiter`]）は `IngestHub` の接続の slot に置く - REST と
//! WS（R2a-2d）が**同じバケット**を使い（§7「WS と REST の合計」）、改名をまたいで続き、
//! 削除された接続の分は slot と一緒に消える。
//!
//! | 対象 | 上限 | 超えたとき |
//! | --- | --- | --- |
//! | 1 メッセージの件数 | 1,000 | 全体を断る（[`LimitKind::TooManyItems`]、REST 422 / WS `too_many_items`） |
//! | メッセージの頻度 | 20 / 秒、瞬間 40 | 全体を断る（[`LimitKind::MessageRate`]、REST 429 / WS `rate_limited`） |
//! | 件数の頻度 | 5,000 / 秒、瞬間 10,000 | 同上（[`LimitKind::ItemRate`]） |
//! | REST の本文 | 1 MiB | 413（`DefaultBodyLimit`、`super::rest` のルートだけ） |
//! | WS の 1 メッセージ | 256 KiB | close 1009（R2a-2d） |
//!
//! 超えても切断・トリップはしない（§7: 切ると再接続の嵐になり Live の判定も乱れる）。
//!
//! ## 判定の順序（[`ConnectionLimiter::admit`]）
//!
//! 1. **メッセージのバケット**から 1 つ取る。無ければ `MessageRate`。断られる
//!    メッセージ（件数の超過・件数のバケットの不足）も 1 つ取る - 断る仕事にも CPU と
//!    監査の行を使うので、壊れた送り手が断られ続けるメッセージの数も 20 / 秒で
//!    頭打ちにする（§11 のオーナーの監査 (3)）。
//! 2. **件数**が 1,000 を超えれば `TooManyItems`（件数のバケットは減らさない）。
//! 3. **件数のバケット**から件数分を取る。足りなければ `ItemRate`（何も減らさない）。
//!
//! JSON として読めない本文（400）・認証で断った要求はここまで来ない（バケットを
//! 減らさない）。読めない本文の頻度は本文の上限（1 MiB）と認証で抑える。
//!
//! ## バケットの算術
//!
//! トークンは 1/1000 単位の整数で持つ（`milli`）。経過 `ms` ミリ秒の補充は
//! `ms × per_sec` milli（= `per_sec` トークン / 秒）で、浮動小数を使わないので境界の
//! テスト（40 個目は通り 41 個目は断る、1 秒後に 20 個）が決定的になる。時計が戻ったら
//! 補充しない（経過を 0 とみなす）。
//!
//! ## 監査の端（§8 の `ingest_limit_exceeded` / `ingest_limit_cleared`）
//!
//! append-health の端の規則（`banto_collect` の `task.rs`）と同じく、種類ごとに
//! 「超過中か」を持ち、超過が**始まったとき**に [`LimitEdge::Exceeded`]（種類・上限値）
//! を 1 つ、次に上限をすべて通ったメッセージで [`LimitEdge::Cleared`]（その間に断った
//! 件数）を 1 つ返す。超過が続く間は行を増やさない。
//!
//! 端の**行を書くこと**は、さらに接続ごとに 1 時間に 20 組（`Exceeded` と対の
//! `Cleared` で 1 組）までに間引く（2026-10-21 オーナー決定、
//! [`super::audit::LimitEdgeThrottle`]）。良いメッセージと悪いメッセージが交互に来ると
//! 端がメッセージごとに出うる（20 / 秒のバケットでしか抑えられない）ため。超えた分は
//! 組の数だけを、窓が明けた後に判定を受けた最初のメッセージで
//! [`LimitEdge::Suppressed`] の 1 行にする。端の状態（`exceeded`）は間引きと関係なく
//! 今までどおり持つ - 抑えた組の `Cleared` も状態としては正しく起きる。

pub use banto_tags::{PushLimits, PUSH_LIMITS};

use super::audit::LimitEdgeThrottle;

/// 1 トークン（= 1 メッセージ・1 件）の milli 単位。
const MILLI: u64 = 1_000;

/// 上限の種類（監査の `kind`・429 の本文の `limit`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LimitKind {
    /// 1 メッセージの件数（422 `too_many_items`）。
    TooManyItems,
    /// 接続ごとのメッセージの頻度（429 `rate_limited`）。
    MessageRate,
    /// 接続ごとの件数の頻度（429 `rate_limited`）。
    ItemRate,
}

impl LimitKind {
    const ALL: [LimitKind; 3] = [Self::TooManyItems, Self::MessageRate, Self::ItemRate];

    /// 監査の `kind` と応答の `limit` の綴り。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TooManyItems => "max_items",
            Self::MessageRate => "messages_per_sec",
            Self::ItemRate => "items_per_sec",
        }
    }

    /// 応答の `error` の綴り（REST の本文・WS の `error`）。
    pub fn error_code(self) -> &'static str {
        match self {
            Self::TooManyItems => "too_many_items",
            Self::MessageRate | Self::ItemRate => "rate_limited",
        }
    }

    /// その種類の上限値（監査の `limit`）。
    pub fn limit(self, limits: &PushLimits) -> u64 {
        match self {
            Self::TooManyItems => limits.max_items_per_message,
            Self::MessageRate => limits.messages_per_sec,
            Self::ItemRate => limits.items_per_sec,
        }
    }

    pub(crate) fn index(self) -> usize {
        match self {
            Self::TooManyItems => 0,
            Self::MessageRate => 1,
            Self::ItemRate => 2,
        }
    }
}

/// メッセージ全体を上限で断った理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LimitRefusal {
    pub kind: LimitKind,
    /// 頻度の超過のとき、足りる分が貯まるまでの秒（切り上げ、1 以上）。REST の
    /// `Retry-After`。件数の超過では `None`（待っても通らない）。
    pub retry_after_secs: Option<u64>,
}

/// 監査の端（§8）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitEdge {
    /// 超過が始まった（`ingest_limit_exceeded`、種類・上限値）。
    Exceeded { kind: LimitKind, limit: u64 },
    /// 超過が終わった（`ingest_limit_cleared`、その間に断ったメッセージの数）。
    Cleared { kind: LimitKind, rejected: u64 },
    /// 前の 1 時間の窓で行を書かずに抑えた超過の組の数（`ingest_limit_exceeded` に
    /// `suppressed`）。[`super::audit::LimitEdgeThrottle`] だけが作る。
    Suppressed { episodes: u64, window_start_ms: i64 },
}

/// [`ConnectionLimiter::admit`] の結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Admission {
    /// 通したか、どの上限で断ったか。
    pub outcome: Result<(), LimitRefusal>,
    /// このメッセージで記録する監査の端（無ければ空）。間引いた後のもの
    /// （モジュールの doc「監査の端」）。
    pub edges: Vec<LimitEdge>,
}

/// トークンバケット（milli 単位の整数、モジュールの doc「バケットの算術」）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenBucket {
    per_sec: u64,
    capacity_milli: u64,
    tokens_milli: u64,
    /// 最後に補充した時刻。初回は満杯から始める。
    last_ms: Option<i64>,
}

impl TokenBucket {
    /// 持続 `per_sec` / 秒、瞬間 `burst`（容量）。満杯から始める。
    pub fn new(per_sec: u64, burst: u64) -> Self {
        let capacity_milli = burst.saturating_mul(MILLI);
        Self {
            per_sec,
            capacity_milli,
            tokens_milli: capacity_milli,
            last_ms: None,
        }
    }

    fn refill(&mut self, now_ms: i64) {
        if let Some(last) = self.last_ms {
            let elapsed = u64::try_from(now_ms.saturating_sub(last)).unwrap_or(0);
            self.tokens_milli = self
                .tokens_milli
                .saturating_add(elapsed.saturating_mul(self.per_sec))
                .min(self.capacity_milli);
        }
        // 時計が戻ったときは最後の時刻を戻さない（戻った分を後で二重に補充しない）。
        self.last_ms = Some(self.last_ms.map_or(now_ms, |last| last.max(now_ms)));
    }

    /// `count` 個を取れるか（補充してから判定。取れなければ足りるまでの秒を返す）。
    /// 取れるときも減らさない（[`Self::take`] で減らす）。
    fn check(&mut self, count: u64, now_ms: i64) -> Result<(), u64> {
        self.refill(now_ms);
        let need = count.saturating_mul(MILLI);
        if self.tokens_milli >= need {
            return Ok(());
        }
        let deficit = need - self.tokens_milli;
        let wait_ms = deficit.div_ceil(self.per_sec.max(1));
        Err(wait_ms.div_ceil(1_000).max(1))
    }

    fn take(&mut self, count: u64) {
        self.tokens_milli = self
            .tokens_milli
            .saturating_sub(count.saturating_mul(MILLI));
    }

    /// 今のトークン（整数に切り捨て。テストと状態表示用）。
    pub fn available(&self) -> u64 {
        self.tokens_milli / MILLI
    }
}

/// 1 つの `push` 接続の上限の状態（2 つのバケットと監査の端）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionLimiter {
    limits: PushLimits,
    messages: TokenBucket,
    items: TokenBucket,
    /// 種類ごとの「超過中」と、その間に断ったメッセージの数（[`LimitKind::index`]）。
    exceeded: [Option<u64>; 3],
    /// 端の行の間引き（行を書くことだけ。`exceeded` は間引かない）。
    edge_audit: LimitEdgeThrottle,
}

impl ConnectionLimiter {
    pub fn new(limits: PushLimits) -> Self {
        Self {
            limits,
            messages: TokenBucket::new(limits.messages_per_sec, limits.messages_burst),
            items: TokenBucket::new(limits.items_per_sec, limits.items_burst),
            exceeded: [None; 3],
            edge_audit: LimitEdgeThrottle::default(),
        }
    }

    /// 1 メッセージ（`item_count` 件）を受けてよいか（モジュールの doc「判定の順序」）。
    pub fn admit(&mut self, item_count: usize, now_ms: i64) -> Admission {
        let outcome = self.judge(u64::try_from(item_count).unwrap_or(u64::MAX), now_ms);
        let mut edges = Vec::new();
        match outcome {
            Ok(()) => {
                for kind in LimitKind::ALL {
                    if let Some(rejected) = self.exceeded[kind.index()].take() {
                        edges.push(LimitEdge::Cleared { kind, rejected });
                    }
                }
            }
            Err(refusal) => {
                let slot = &mut self.exceeded[refusal.kind.index()];
                match slot {
                    Some(rejected) => *rejected += 1,
                    None => {
                        *slot = Some(1);
                        edges.push(LimitEdge::Exceeded {
                            kind: refusal.kind,
                            limit: refusal.kind.limit(&self.limits),
                        });
                    }
                }
            }
        }
        let edges = self.edge_audit.filter(edges, now_ms);
        Admission { outcome, edges }
    }

    fn judge(&mut self, item_count: u64, now_ms: i64) -> Result<(), LimitRefusal> {
        if let Err(wait) = self.messages.check(1, now_ms) {
            return Err(LimitRefusal {
                kind: LimitKind::MessageRate,
                retry_after_secs: Some(wait),
            });
        }
        self.messages.take(1);
        if item_count > self.limits.max_items_per_message {
            return Err(LimitRefusal {
                kind: LimitKind::TooManyItems,
                retry_after_secs: None,
            });
        }
        if let Err(wait) = self.items.check(item_count, now_ms) {
            return Err(LimitRefusal {
                kind: LimitKind::ItemRate,
                retry_after_secs: Some(wait),
            });
        }
        self.items.take(item_count);
        Ok(())
    }

    /// 今のメッセージのバケットの残り（テスト用）。
    pub fn messages_available(&self) -> u64 {
        self.messages.available()
    }

    /// 今の件数のバケットの残り（テスト用）。
    pub fn items_available(&self) -> u64 {
        self.items.available()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind_of(admission: &Admission) -> Option<LimitKind> {
        admission.outcome.err().map(|refusal| refusal.kind)
    }

    /// 上限の数値は `PUSH_LIMITS` のもの（§7 の表と同じ）。
    #[test]
    fn the_limits_are_the_shared_constants() {
        assert_eq!(PUSH_LIMITS.max_items_per_message, 1_000);
        assert_eq!(PUSH_LIMITS.max_rest_body_bytes, 1024 * 1024);
        assert_eq!(PUSH_LIMITS.messages_per_sec, 20);
        assert_eq!(PUSH_LIMITS.messages_burst, 40);
        assert_eq!(PUSH_LIMITS.items_per_sec, 5_000);
        assert_eq!(PUSH_LIMITS.items_burst, 10_000);
    }

    /// 件数: 1,000 件ちょうどは通り、1,001 件は `TooManyItems`（待っても通らないので
    /// `Retry-After` なし）。断ったメッセージもメッセージのバケットを 1 つ使い、件数の
    /// バケットは減らさない。
    #[test]
    fn the_item_count_boundary() {
        let mut limiter = ConnectionLimiter::new(PUSH_LIMITS);
        assert_eq!(limiter.admit(1_000, 0).outcome, Ok(()));
        assert_eq!(limiter.items_available(), 9_000);
        let refused = limiter.admit(1_001, 0);
        assert_eq!(
            refused.outcome,
            Err(LimitRefusal {
                kind: LimitKind::TooManyItems,
                retry_after_secs: None
            })
        );
        assert_eq!(limiter.items_available(), 9_000);
        assert_eq!(limiter.messages_available(), 38);
        assert_eq!(limiter.admit(0, 0).outcome, Ok(()));
    }

    /// メッセージのバケット: 瞬間の 40 を使い切ると 41 個目は 429。1 秒後には 20 個
    /// 貯まり、21 個目は 429。`Retry-After` は足りるまでの秒の切り上げ。
    #[test]
    fn the_message_bucket_boundary() {
        let mut limiter = ConnectionLimiter::new(PUSH_LIMITS);
        for n in 0..40 {
            assert_eq!(limiter.admit(1, 0).outcome, Ok(()), "{n}");
        }
        let refused = limiter.admit(1, 0);
        assert_eq!(
            refused.outcome,
            Err(LimitRefusal {
                kind: LimitKind::MessageRate,
                retry_after_secs: Some(1)
            })
        );
        for n in 0..20 {
            assert_eq!(limiter.admit(1, 1_000).outcome, Ok(()), "{n}");
        }
        assert_eq!(
            kind_of(&limiter.admit(1, 1_000)),
            Some(LimitKind::MessageRate)
        );
        // 50 ms で 1 個（20 / 秒）。49 ms では足りない。
        assert_eq!(
            kind_of(&limiter.admit(1, 1_049)),
            Some(LimitKind::MessageRate)
        );
        assert_eq!(limiter.admit(1, 1_050).outcome, Ok(()));
    }

    /// 件数のバケット: 1,000 件 × 10 で瞬間の 10,000 を使い切ると 11 個目は 429
    /// （`ItemRate`）。1 秒後には 5,000 件 = 1,000 件 × 5 が通り、6 個目は 429。
    #[test]
    fn the_item_bucket_boundary() {
        let mut limiter = ConnectionLimiter::new(PUSH_LIMITS);
        for n in 0..10 {
            assert_eq!(limiter.admit(1_000, 0).outcome, Ok(()), "{n}");
        }
        let refused = limiter.admit(1_000, 0);
        assert_eq!(
            refused.outcome,
            Err(LimitRefusal {
                kind: LimitKind::ItemRate,
                retry_after_secs: Some(1)
            })
        );
        // 1 件なら足りない（0 件は通る）。
        assert_eq!(kind_of(&limiter.admit(1, 0)), Some(LimitKind::ItemRate));
        assert_eq!(limiter.admit(0, 0).outcome, Ok(()));
        for n in 0..5 {
            assert_eq!(limiter.admit(1_000, 1_000).outcome, Ok(()), "{n}");
        }
        assert_eq!(
            kind_of(&limiter.admit(1_000, 1_000)),
            Some(LimitKind::ItemRate)
        );
    }

    /// 補充は容量で頭打ち。時計が戻っても補充しない（戻った分を後で二重に数えない）。
    #[test]
    fn refill_is_capped_and_ignores_a_clock_going_back() {
        let mut bucket = TokenBucket::new(20, 40);
        bucket.check(40, 0).unwrap();
        bucket.take(40);
        bucket.check(0, 3_600_000).unwrap();
        assert_eq!(bucket.available(), 40);
        bucket.take(40);
        bucket.check(0, 1_000).unwrap();
        assert_eq!(bucket.available(), 0);
        bucket.check(0, 3_600_500).unwrap();
        assert_eq!(bucket.available(), 10);
    }

    /// 監査の端: 超過が始まったときに `Exceeded` を 1 つ、続く間は出さず、次に通った
    /// メッセージで `Cleared`（断った数）を 1 つ。種類ごとに別に持つ。
    #[test]
    fn exceeded_and_cleared_are_reported_on_the_edges_only() {
        let mut limiter = ConnectionLimiter::new(PUSH_LIMITS);
        assert!(limiter.admit(1, 0).edges.is_empty());
        assert_eq!(
            limiter.admit(1_001, 0).edges,
            vec![LimitEdge::Exceeded {
                kind: LimitKind::TooManyItems,
                limit: 1_000
            }]
        );
        assert!(limiter.admit(5_000, 0).edges.is_empty());
        assert!(limiter.admit(1_001, 0).edges.is_empty());
        for _ in 0..36 {
            limiter.admit(0, 0);
        }
        // ここまでで 40 個を使い切った（0 件のメッセージで TooManyItems は消えた）。
        assert_eq!(limiter.messages_available(), 0);
        let rate = limiter.admit(1, 0);
        assert_eq!(
            rate.edges,
            vec![LimitEdge::Exceeded {
                kind: LimitKind::MessageRate,
                limit: 20
            }]
        );
        assert!(limiter.admit(1, 0).edges.is_empty());
        assert_eq!(
            limiter.admit(1, 1_000).edges,
            vec![LimitEdge::Cleared {
                kind: LimitKind::MessageRate,
                rejected: 2
            }]
        );
        assert!(limiter.admit(1, 1_000).edges.is_empty());
    }

    /// 良いメッセージと悪いメッセージが交互に来ても、端の行は 1 時間に 20 組まで。
    /// 抑えた組の間も端の状態は正しく動き（21 組目以降も「始まり」と「終わり」を
    /// 判定し、記録した組の `rejected` は正しい）、窓が明けた後に通ったメッセージで
    /// 抑えた組の数を 1 行。
    #[test]
    fn alternating_messages_produce_at_most_20_edge_pairs_per_hour() {
        let mut limiter = ConnectionLimiter::new(PUSH_LIMITS);
        let mut edges = Vec::new();
        for n in 0..25_i64 {
            let bad = limiter.admit(1_001, n * 1_000);
            assert_eq!(kind_of(&bad), Some(LimitKind::TooManyItems), "{n}");
            edges.extend(bad.edges);
            let good = limiter.admit(1, n * 1_000);
            assert_eq!(good.outcome, Ok(()), "{n}");
            edges.extend(good.edges);
        }
        let mut expected = Vec::new();
        for _ in 0..20 {
            expected.push(LimitEdge::Exceeded {
                kind: LimitKind::TooManyItems,
                limit: 1_000,
            });
            expected.push(LimitEdge::Cleared {
                kind: LimitKind::TooManyItems,
                rejected: 1,
            });
        }
        assert_eq!(edges, expected);
        // 抑えている間も状態は動いている: 26 組目の `Exceeded` は抑えられ、`Cleared`
        // も書かない（`exceeded` が残っていれば `rejected` が溜まり、ここで 2 になる）。
        assert!(limiter.admit(1_001, 30_000).edges.is_empty());
        assert!(limiter.admit(1, 30_000).edges.is_empty());
        assert_eq!(
            limiter.admit(1, 3_600_000).edges,
            vec![LimitEdge::Suppressed {
                episodes: 6,
                window_start_ms: 0
            }]
        );
        // 新しい窓では記録に戻り、`rejected` は 1（抑えた組の分を持ち越さない）。
        assert_eq!(
            limiter.admit(1_001, 3_601_000).edges,
            vec![LimitEdge::Exceeded {
                kind: LimitKind::TooManyItems,
                limit: 1_000
            }]
        );
        assert_eq!(
            limiter.admit(1, 3_601_000).edges,
            vec![LimitEdge::Cleared {
                kind: LimitKind::TooManyItems,
                rejected: 1
            }]
        );
    }

    /// 件数の超過の後に通ったメッセージで `TooManyItems` の `Cleared`（断った数 3）。
    #[test]
    fn cleared_counts_the_rejected_messages() {
        let mut limiter = ConnectionLimiter::new(PUSH_LIMITS);
        for _ in 0..3 {
            limiter.admit(1_001, 0);
        }
        assert_eq!(
            limiter.admit(1, 0).edges,
            vec![LimitEdge::Cleared {
                kind: LimitKind::TooManyItems,
                rejected: 3
            }]
        );
    }
}
