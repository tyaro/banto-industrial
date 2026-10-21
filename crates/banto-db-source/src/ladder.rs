//! 品質の梯子（docs/banto-hub-external-db-design.md §4.3「Quality の変換」）を
//! **純関数の状態機械**として切り出したもの。
//!
//! 元は `banto-hub-core` の `db_source/task.rs`（`run_group_tick` の 0 行・
//! 値の判定、`on_group_failure`、`write_stale`）に、値の書き込み（`ServerTagStore`）・
//! 時刻・ログ・状態表示と絡んで散っていた判断の部分。Hub 内の Source と DB
//! ブリッジ（R2a-3b）が**同じ梯子**を通るよう、判断だけをここへ置き、書き込みは
//! 呼び出し側が [`Verdict`] を見て行う（この crate は書き込み先を知らない）。
//!
//! ## 梯子（§4.3 の表）
//!
//! | 事象                       | 判断                       | 単位     | 入り口                         |
//! | -------------------------- | -------------------------- | -------- | ------------------------------ |
//! | 正常に列が取れた           | [`Verdict::Good`]（変換値）| タグ     | [`cell_verdict`]               |
//! | 列が NULL・非有限値        | [`Verdict::Bad`]           | タグ     | [`cell_verdict`]               |
//! | 列が無い / 型が未対応      | Bad（毎ティック）          | タグ     | [`crate::resolve_columns`]     |
//! | 0 行                       | Bad（梯子には乗せない）    | グループ | [`GroupLadder::on_zero_rows`]  |
//! | クエリエラー・タイムアウト | [`Verdict::Stale`]（直前値）| グループ | [`GroupLadder::on_query_failure`] |
//! | 同上が2周期連続            | Bad                        | グループ | 同上                           |
//! | 接続断（バックオフ中）     | Bad                        | 接続     | [`GroupLadder::connection_lost`] |
//!
//! **境界: 直前値の無い Stale は Bad。** Stale は「古い値がある」という意味
//! なので、まだ一度も値が読めていないタグに Stale を付けるのは嘘になる。
//! 呼び出し側は直前値の有無を [`Verdict::settle`] に渡して、Stale を Bad へ
//! 落とす（Stale のとき直前値と、その `ptime` を据え置くのも呼び出し側）。
//!
//! 「1回の失敗で値を捨てない」（Stale を1周期だけ挟む）は収集キャッシュの
//! `STALE_PERIOD_FACTOR = 2.5` に合わせた運用感（§4.3 末尾）。

/// 1タグに対する梯子の判断。書き込み（値・品質・時刻）は呼び出し側が行う。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Verdict {
    /// 新しい値を Good で書く。
    Good(f64),
    /// 直前値を残したまま Stale にする（直前値の `ptime` も据え置く -
    /// 値が実際に読めた時刻を後ろへずらさないため）。直前値が無ければ
    /// [`Verdict::settle`] で Bad になる。
    Stale,
    /// 値なし（`None`）で Bad を書く。
    Bad,
}

impl Verdict {
    /// 直前値の有無を踏まえて確定する。**直前値の無い Stale は Bad**（このモジュール
    /// doc comment の「境界」）。それ以外はそのまま。
    pub fn settle(self, has_previous_value: bool) -> Verdict {
        match self {
            Verdict::Stale if !has_previous_value => Verdict::Bad,
            other => other,
        }
    }
}

/// 結果の1セルの判断。有限の値は Good、NULL（`None`）と非有限値
/// （`'NaN'::float8` など）は Bad - 非有限値は JSON にも載せられず、
/// 「欠測を隠さない」（設計 §4）の一環として Bad に倒す。
pub fn cell_verdict(cell: Option<f64>) -> Verdict {
    match cell {
        Some(x) if x.is_finite() => Verdict::Good(x),
        _ => Verdict::Bad,
    }
}

/// [`GroupLadder::on_query_failure`] の結果。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QueryFailure {
    /// この失敗を数えた後の連続失敗回数（1 始まり）。
    pub consecutive_failures: u32,
    /// 初回の失敗か（`warn` は初回だけ出す - ログの抑制）。
    pub first: bool,
    /// グループの全タグ（値の取れる列も、列が無い・型が未対応のタグも）に
    /// 適用する判断。1回目は [`Verdict::Stale`]、2回目以降は [`Verdict::Bad`]。
    pub verdict: Verdict,
}

/// 1グループのクエリ失敗の連続回数を持つ状態機械。
///
/// 状態は連続失敗回数だけ。成功（0 行を含む）で 0 に戻る。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GroupLadder {
    consecutive_failures: u32,
}

impl GroupLadder {
    /// この回数連続で失敗したら Stale から Bad へ落ちる（§4.3「2周期連続」）。
    pub const DEGRADE_AT: u32 = 2;

    pub fn new() -> Self {
        Self::default()
    }

    /// 連続した失敗の回数。
    pub fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures
    }

    /// Bad の梯子に乗って（[`Self::DEGRADE_AT`] 周期以上）いるか。呼び出し側は
    /// これが真のグループを再 describe の対象にする。
    pub fn is_degraded(&self) -> bool {
        self.consecutive_failures >= Self::DEGRADE_AT
    }

    /// クエリが成功して 1 行以上が返った。連続失敗を 0 に戻す。タグごとの
    /// 判断は [`cell_verdict`]。
    pub fn on_success(&mut self) {
        self.consecutive_failures = 0;
    }

    /// 再 describe の成功などで梯子を最初に戻す（`on_success` と同じ）。
    pub fn reset(&mut self) {
        self.consecutive_failures = 0;
    }

    /// クエリは成功したが 0 行だった。**Stale の梯子には乗せず**直ちに Bad
    /// （クエリ自体は成功している）。連続失敗は 0 に戻る。全タグに適用する。
    pub fn on_zero_rows(&mut self) -> Verdict {
        self.consecutive_failures = 0;
        Verdict::Bad
    }

    /// クエリエラー・タイムアウト（グループ単位の失敗）。1回目は Stale、
    /// 2回目以降は Bad。
    pub fn on_query_failure(&mut self) -> QueryFailure {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        QueryFailure {
            consecutive_failures: self.consecutive_failures,
            first: self.consecutive_failures == 1,
            verdict: if self.is_degraded() {
                Verdict::Bad
            } else {
                Verdict::Stale
            },
        }
    }

    /// 接続断（サーバーへ届かない・セッションが壊れた）。接続単位で配下の全タグを
    /// Bad にする - 梯子の段は経ない（接続を張り直すとグループの状態は作り直し）。
    pub fn connection_lost() -> Verdict {
        Verdict::Bad
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_verdict_is_good_only_for_finite_values() {
        assert_eq!(cell_verdict(Some(12.5)), Verdict::Good(12.5));
        assert_eq!(cell_verdict(Some(0.0)), Verdict::Good(0.0));
        assert_eq!(cell_verdict(None), Verdict::Bad);
        assert_eq!(cell_verdict(Some(f64::NAN)), Verdict::Bad);
        assert_eq!(cell_verdict(Some(f64::INFINITY)), Verdict::Bad);
        assert_eq!(cell_verdict(Some(f64::NEG_INFINITY)), Verdict::Bad);
    }

    /// 境界: 直前値の無い Stale は Bad。ほかの判断は変わらない。
    #[test]
    fn settle_turns_stale_without_a_previous_value_into_bad() {
        assert_eq!(Verdict::Stale.settle(true), Verdict::Stale);
        assert_eq!(Verdict::Stale.settle(false), Verdict::Bad);
        assert_eq!(Verdict::Bad.settle(true), Verdict::Bad);
        assert_eq!(Verdict::Bad.settle(false), Verdict::Bad);
        assert_eq!(Verdict::Good(1.0).settle(false), Verdict::Good(1.0));
    }

    /// §4.3: 1回目の失敗は Stale、2回目で Bad、以降も Bad。
    #[test]
    fn query_failure_goes_stale_first_then_bad() {
        let mut ladder = GroupLadder::new();
        assert!(!ladder.is_degraded());

        let first = ladder.on_query_failure();
        assert_eq!(
            first,
            QueryFailure {
                consecutive_failures: 1,
                first: true,
                verdict: Verdict::Stale
            }
        );
        assert!(!ladder.is_degraded());

        let second = ladder.on_query_failure();
        assert_eq!(
            second,
            QueryFailure {
                consecutive_failures: 2,
                first: false,
                verdict: Verdict::Bad
            }
        );
        assert!(ladder.is_degraded());

        let third = ladder.on_query_failure();
        assert_eq!(third.verdict, Verdict::Bad);
        assert_eq!(third.consecutive_failures, 3);
        assert_eq!(ladder.consecutive_failures(), 3);
    }

    /// 成功で梯子は最初に戻り、次の失敗はまた Stale から。
    #[test]
    fn success_resets_the_ladder() {
        let mut ladder = GroupLadder::new();
        ladder.on_query_failure();
        ladder.on_query_failure();
        ladder.on_success();
        assert_eq!(ladder.consecutive_failures(), 0);
        assert!(!ladder.is_degraded());
        let again = ladder.on_query_failure();
        assert_eq!(again.verdict, Verdict::Stale);
        assert!(again.first);
    }

    /// 0 行は Stale を挟まず Bad で、連続失敗も 0 に戻す（クエリ自体は成功）。
    #[test]
    fn zero_rows_is_bad_without_climbing_the_ladder() {
        let mut ladder = GroupLadder::new();
        ladder.on_query_failure();
        assert_eq!(ladder.on_zero_rows(), Verdict::Bad);
        assert_eq!(ladder.consecutive_failures(), 0);
        assert_eq!(ladder.on_query_failure().verdict, Verdict::Stale);
    }

    #[test]
    fn reset_returns_to_the_first_rung() {
        let mut ladder = GroupLadder::new();
        ladder.on_query_failure();
        ladder.on_query_failure();
        ladder.reset();
        assert_eq!(ladder, GroupLadder::new());
    }

    #[test]
    fn connection_lost_is_bad_for_every_tag() {
        assert_eq!(GroupLadder::connection_lost(), Verdict::Bad);
    }
}
