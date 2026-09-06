//! グループごとの bounded queue（設計 §5.4・§6-8）。
//!
//! ## 2 つの不変条件
//!
//! 1. **メモリを無制限に使わない**: 満杯になったら**最も古い行**を捨てて
//!    `dropped` を増やす。DB が止まっている間に新しい値を捨てるのではなく
//!    古い値を捨てるのは、「直近の状況が残っている方が現場で役に立つ」
//!    という判断（§6-8 のオーナー決定）。ディスクスプールは将来。
//! 2. **at-least-once**: 行は INSERT が **commit されてから**キューを
//!    出る（§5.4）。そのため取り出しは [`RowQueue::peek_batch`]（複製）と
//!    [`RowQueue::commit`]（先頭 n 件の削除）の 2 段構えで、失敗した
//!    バッチは何もしなければ次回そのまま再送される。`(ts, tag_id)` の
//!    一意制約を張るかどうかは利用者の選択（§5.4）。
//!
//! 排他は `std::sync::Mutex`（`tokio::sync::Mutex` ではない）で、
//! ロック中に `await` しないことを構造で保証する - プロデューサ側の
//! `push` は `abort` されうるタスクから呼ばれるので、await 点を挟まない
//! ことがそのままキャンセル安全性になる。

use std::collections::VecDeque;

/// 保存する 1 行（設計 §5.3 の long スキーマ）。
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    /// 値の `ptime`（epoch ミリ秒）。取れない場合は採取時刻
    /// （[`crate::group`] が決める）。
    pub ts_ms: i64,
    /// Hub の安定 ID の 3 つ目（`tags.id`）。
    pub tag_id: i64,
    /// 採取時点の外部名（リネーム後の行は新しい名前になる - §5.3）。
    pub external_name: String,
    pub value: Option<f64>,
    /// wire の文字列そのまま（`good`/`bad`/`stale`、未知の値はその値）。
    pub quality: String,
}

/// 先入れ先出し・満杯なら最古を捨てる行キュー。
#[derive(Debug)]
pub struct RowQueue {
    rows: VecDeque<Row>,
    max_rows: usize,
    dropped: u64,
}

impl RowQueue {
    pub fn new(max_rows: usize) -> Self {
        Self {
            // 満杯時に再確保しないよう最初から確保する…のは 1,000 万行
            // 設定で無駄なので、実運用の既定（10,000）程度までに留める。
            rows: VecDeque::with_capacity(max_rows.min(10_000)),
            max_rows,
            dropped: 0,
        }
    }

    /// グループの設定変更でキューを作り直すときに、捨てた行数を引き継ぐ
    /// （`dropped` は Hub の状態画面に出る累積値なので、設定を触った
    /// だけで 0 に戻ると「取りこぼしていない」と誤読される）。
    pub fn with_dropped(max_rows: usize, dropped: u64) -> Self {
        let mut queue = Self::new(max_rows);
        queue.dropped = dropped;
        queue
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// 1 行積む。満杯なら最古を 1 行捨てて `dropped` を増やす。
    pub fn push(&mut self, row: Row) {
        if self.max_rows == 0 {
            self.dropped = self.dropped.saturating_add(1);
            return;
        }
        while self.rows.len() >= self.max_rows {
            self.rows.pop_front();
            self.dropped = self.dropped.saturating_add(1);
        }
        self.rows.push_back(row);
    }

    /// 先頭から最大 `limit` 件を**複製**して返す（キューからは出さない -
    /// このモジュールの doc comment の不変条件 2）。
    pub fn peek_batch(&self, limit: usize) -> Vec<Row> {
        self.rows.iter().take(limit).cloned().collect()
    }

    /// commit 済みの先頭 `count` 件を捨てる。
    pub fn commit(&mut self, count: usize) {
        for _ in 0..count.min(self.rows.len()) {
            self.rows.pop_front();
        }
    }

    /// 停止時に flush しきれなかった残りを `dropped` に計上して捨てる
    /// （設計 §5.6「残りは `dropped` に計上して `warn`」）。捨てた件数を
    /// 返す。
    pub fn discard_all(&mut self) -> usize {
        let count = self.rows.len();
        self.rows.clear();
        self.dropped = self.dropped.saturating_add(count as u64);
        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(tag_id: i64, ts_ms: i64) -> Row {
        Row {
            ts_ms,
            tag_id,
            external_name: format!("line1.fast.tag{tag_id}"),
            value: Some(tag_id as f64),
            quality: "good".to_string(),
        }
    }

    #[test]
    fn push_keeps_order_and_counts_nothing_while_under_the_cap() {
        let mut queue = RowQueue::new(3);
        queue.push(row(1, 10));
        queue.push(row(2, 20));
        assert_eq!(queue.len(), 2);
        assert_eq!(queue.dropped(), 0);
        assert_eq!(
            queue
                .peek_batch(10)
                .iter()
                .map(|r| r.tag_id)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
    }

    #[test]
    fn a_full_queue_drops_the_oldest_row_and_counts_it() {
        let mut queue = RowQueue::new(2);
        queue.push(row(1, 10));
        queue.push(row(2, 20));
        queue.push(row(3, 30));
        assert_eq!(queue.len(), 2);
        assert_eq!(queue.dropped(), 1);
        // 残るのは新しい方の 2 件（最古の 1 が消える）。
        assert_eq!(
            queue
                .peek_batch(10)
                .iter()
                .map(|r| r.tag_id)
                .collect::<Vec<_>>(),
            vec![2, 3]
        );
    }

    /// flusher の刻み方（`peek_batch(batch_size)` → INSERT →
    /// `commit(len)` の繰り返し）で、順序を保ったまま全行がちょうど 1 回
    /// ずつ出ること。
    #[test]
    fn chunking_by_batch_size_covers_every_row_exactly_once_in_order() {
        let mut queue = RowQueue::new(100);
        for i in 1..=7 {
            queue.push(row(i, i * 10));
        }
        let mut sent = Vec::new();
        let mut batches = 0;
        loop {
            let batch = queue.peek_batch(3);
            if batch.is_empty() {
                break;
            }
            batches += 1;
            sent.extend(batch.iter().map(|r| r.tag_id));
            queue.commit(batch.len());
        }
        assert_eq!(batches, 3, "7 行を 3 行刻みで 3 バッチ（3/3/1）");
        assert_eq!(sent, vec![1, 2, 3, 4, 5, 6, 7]);
        assert!(queue.is_empty());
        assert_eq!(queue.dropped(), 0);
    }

    #[test]
    fn peek_does_not_remove_and_commit_does() {
        let mut queue = RowQueue::new(10);
        for i in 1..=5 {
            queue.push(row(i, i * 10));
        }
        let batch = queue.peek_batch(3);
        assert_eq!(batch.len(), 3);
        // 失敗した想定: 何も commit しなければ 5 件のまま。
        assert_eq!(queue.len(), 5);
        queue.commit(batch.len());
        assert_eq!(queue.len(), 2);
        assert_eq!(
            queue
                .peek_batch(10)
                .iter()
                .map(|r| r.tag_id)
                .collect::<Vec<_>>(),
            vec![4, 5]
        );
    }

    #[test]
    fn commit_beyond_the_queue_length_is_clamped() {
        let mut queue = RowQueue::new(10);
        queue.push(row(1, 10));
        queue.commit(99);
        assert!(queue.is_empty());
    }

    #[test]
    fn discard_all_counts_the_remainder_as_dropped() {
        let mut queue = RowQueue::new(10);
        for i in 1..=4 {
            queue.push(row(i, i * 10));
        }
        assert_eq!(queue.discard_all(), 4);
        assert!(queue.is_empty());
        assert_eq!(queue.dropped(), 4);
    }

    #[test]
    fn dropped_is_carried_over_when_a_group_is_rebuilt() {
        let mut queue = RowQueue::with_dropped(10, 7);
        assert_eq!(queue.dropped(), 7);
        queue.push(row(1, 10));
        assert_eq!(queue.discard_all(), 1);
        assert_eq!(queue.dropped(), 8);
    }

    /// 病的な設定（0 行）でも panic せず、すべて drop として数える。
    #[test]
    fn a_zero_capacity_queue_drops_everything() {
        let mut queue = RowQueue::new(0);
        queue.push(row(1, 10));
        assert!(queue.is_empty());
        assert_eq!(queue.dropped(), 1);
    }
}
