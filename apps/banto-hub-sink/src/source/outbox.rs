//! 送りきる前に次の周期が出たら古い方を捨てる送信待ち（docs/banto-hub-ingest-design.md
//! §7.1 の (a)「最新を優先。キューを膨らませない」）。
//!
//! クエリごとに「まだ送っていない、最後の周期のメッセージ」だけを持つ。同じクエリの
//! 次の周期が来たら、前の周期の**まだ送っていない分**を捨てて新しい周期で置き換える
//! （送り済みの分は Hub に届いているので取り消さない - 1 周期の値が 2 つのスナップショットに
//! またがることがあるが、次の tick でそろう。§7.1）。捨てた周期・件数は数えて、
//! `status` で Hub に知らせる（[`super::session`]）。
//!
//! 送る順はクエリをまたいで周期の到着順（一番古い未送信の周期のクエリから）。速いクエリが
//! 遅いクエリを飢えさせないよう、1 メッセージ送るごとに次の到着順を見直す。
//!
//! メモリは「クエリの数 × 1 周期分」で頭打ち（周期が速くても積み上がらない）。
//!
//! ## 失敗の知らせ（[`Outbox::push_failure`]）
//!
//! 問い合わせのタスクが止めていないのに終わったときの全キー `bad`（§10.3「タスクの異常」:
//! 全キーを Bad にしてから作り直す）は、周期とは**別の列**に置く。後から来た周期
//! （作り直したタスクの最初の周期）では置き換えず、周期より**先に**送る。周期の列で
//! 持つと、バケットの待ちの間に作り直したタスクの周期が未送信の `bad` を置き換え、
//! 一部のキーに `bad` が届かないまま新しい Good が届く（64 クエリ × 1 キーで、
//! 1 秒 16 メッセージの速さだと 1 秒後の作り直しに間に合わない）。失敗の列もクエリごとに
//! 1 回分（同じクエリの次の失敗の知らせは、送っていない前の分と同じ全キーなので置き換える）
//! なので、メモリは「クエリの数 × 2 周期分」で頭打ち。

use std::collections::VecDeque;

use crate::source::wire::ValueItem;

/// 1 クエリの送信待ち。
#[derive(Debug, Default)]
struct Pending {
    /// まだ送っていないメッセージ（先頭から送る）。
    messages: VecDeque<Vec<ValueItem>>,
    /// この周期が入った順番（クエリをまたいだ到着順の比較用）。
    arrival: u64,
}

/// [`Outbox::push_cycle`] で捨てた古い周期。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Superseded {
    /// 捨てたメッセージの数。
    pub messages: usize,
    /// 捨てた件数。
    pub items: usize,
}

/// 1 つの source の送信待ち。
#[derive(Debug)]
pub struct Outbox {
    queries: Vec<Pending>,
    /// 失敗の知らせ（全キー `bad`）。周期で置き換えず、周期より先に送る。
    failures: Vec<Pending>,
    next_arrival: u64,
    /// 捨てた周期の数（累計。`status` の文言に出す）。
    superseded_cycles: u64,
    /// 捨てた件数（累計）。
    superseded_items: u64,
}

impl Outbox {
    pub fn new(query_count: usize) -> Self {
        Self {
            queries: (0..query_count).map(|_| Pending::default()).collect(),
            failures: (0..query_count).map(|_| Pending::default()).collect(),
            next_arrival: 0,
            superseded_cycles: 0,
            superseded_items: 0,
        }
    }

    /// クエリ `query` の 1 周期（分けた後のメッセージ）を入れる。まだ送っていない前の周期の
    /// 分があれば捨てて、それを返す。
    pub fn push_cycle(
        &mut self,
        query: usize,
        messages: Vec<Vec<ValueItem>>,
    ) -> Option<Superseded> {
        let pending = self.queries.get_mut(query)?;
        let superseded = (!pending.messages.is_empty()).then(|| Superseded {
            messages: pending.messages.len(),
            items: pending.messages.iter().map(Vec::len).sum(),
        });
        if let Some(dropped) = superseded {
            self.superseded_cycles += 1;
            self.superseded_items += dropped.items as u64;
        }
        pending.messages = messages.into_iter().filter(|m| !m.is_empty()).collect();
        pending.arrival = self.next_arrival;
        self.next_arrival += 1;
        superseded
    }

    /// クエリ `query` の失敗の知らせ（全キー `bad` を分けたメッセージ）を入れる。周期の列の
    /// 未送信分は捨てる（止まったタスクが出した古い値）。後から来る周期では置き換えず、
    /// 周期より先に送る（このモジュールの doc「失敗の知らせ」）。
    pub fn push_failure(&mut self, query: usize, messages: Vec<Vec<ValueItem>>) {
        let Some(pending) = self.failures.get_mut(query) else {
            return;
        };
        pending.messages = messages.into_iter().filter(|m| !m.is_empty()).collect();
        pending.arrival = self.next_arrival;
        self.next_arrival += 1;
        self.queries[query].messages.clear();
    }

    /// 次に送るメッセージの件数（無ければ `None`）。
    pub fn peek_len(&self) -> Option<usize> {
        self.next().map(|(lane, q)| lane[q].messages[0].len())
    }

    /// 次に送るメッセージを取り出す（クエリの添字と一緒に）。失敗の知らせが残っていれば
    /// それが先。
    pub fn pop(&mut self) -> Option<(usize, Vec<ValueItem>)> {
        let (failure, query) = match Self::oldest(&self.failures) {
            Some(query) => (true, query),
            None => (false, Self::oldest(&self.queries)?),
        };
        let lane = if failure {
            &mut self.failures
        } else {
            &mut self.queries
        };
        let message = lane[query].messages.pop_front()?;
        Some((query, message))
    }

    /// 送信待ちを全部捨てる（epoch が変わった・収集が止まった - 古い epoch の値は Hub が
    /// `stale_epoch` で断るので、送っても無駄）。失敗の知らせも捨てる（新しい epoch では
    /// 作り直したタスクが最初の周期で全キーを送る）。捨てた分は「周期の置き換え」には
    /// 数えない。
    pub fn clear(&mut self) {
        for pending in self.queries.iter_mut().chain(self.failures.iter_mut()) {
            pending.messages.clear();
        }
    }

    pub fn is_empty(&self) -> bool {
        self.queries
            .iter()
            .chain(self.failures.iter())
            .all(|p| p.messages.is_empty())
    }

    /// 失敗の知らせが残っているか。
    pub fn has_failures(&self) -> bool {
        self.failures.iter().any(|p| !p.messages.is_empty())
    }

    pub fn superseded_cycles(&self) -> u64 {
        self.superseded_cycles
    }

    pub fn superseded_items(&self) -> u64 {
        self.superseded_items
    }

    /// 次に送る列とクエリの添字（失敗の知らせが先）。
    fn next(&self) -> Option<(&[Pending], usize)> {
        match Self::oldest(&self.failures) {
            Some(query) => Some((&self.failures, query)),
            None => Self::oldest(&self.queries).map(|query| (self.queries.as_slice(), query)),
        }
    }

    fn oldest(lane: &[Pending]) -> Option<usize> {
        lane.iter()
            .enumerate()
            .filter(|(_, p)| !p.messages.is_empty())
            .min_by_key(|(_, p)| p.arrival)
            .map(|(i, _)| i)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(prefix: &str, n: usize) -> Vec<ValueItem> {
        (0..n)
            .map(|i| ValueItem::good(&format!("{prefix}{i}"), i as f64))
            .collect()
    }

    /// §11 の R2a-3b: 送りきる前に次の周期が出たら古い方（未送信の分）を捨てる。
    #[test]
    fn a_newer_cycle_replaces_the_unsent_rest_of_the_older_one() {
        let mut outbox = Outbox::new(1);
        assert_eq!(
            outbox.push_cycle(0, vec![message("a", 3), message("b", 2)]),
            None
        );
        // 1 メッセージ目だけ送れた。
        let (query, sent) = outbox.pop().unwrap();
        assert_eq!((query, sent.len()), (0, 3));
        // 次の周期が来た: 送っていない b の 2 件を捨てて、新しい周期だけを持つ。
        assert_eq!(
            outbox.push_cycle(0, vec![message("c", 4), message("d", 1)]),
            Some(Superseded {
                messages: 1,
                items: 2
            })
        );
        assert_eq!(outbox.superseded_cycles(), 1);
        assert_eq!(outbox.superseded_items(), 2);
        assert_eq!(outbox.pop().unwrap().1[0].key, "c0");
        assert_eq!(outbox.pop().unwrap().1[0].key, "d0");
        assert!(outbox.pop().is_none());
        // 送りきった後の次の周期は何も捨てない。
        assert_eq!(outbox.push_cycle(0, vec![message("e", 1)]), None);
        assert_eq!(outbox.superseded_cycles(), 1);
    }

    /// クエリをまたいだ送る順は周期の到着順。速いクエリが遅いクエリを飢えさせない。
    #[test]
    fn queries_are_served_in_arrival_order() {
        let mut outbox = Outbox::new(2);
        outbox.push_cycle(0, vec![message("a", 1), message("a", 1)]);
        outbox.push_cycle(1, vec![message("b", 1)]);
        assert_eq!(outbox.pop().unwrap().0, 0);
        // クエリ 0 の次の周期が来ても、先に来ていたクエリ 1 の周期が先。
        outbox.push_cycle(0, vec![message("c", 1)]);
        assert_eq!(outbox.pop().unwrap().0, 1);
        assert_eq!(outbox.pop().unwrap().1[0].key, "c0");
        assert!(outbox.is_empty());
        assert_eq!(outbox.peek_len(), None);
    }

    #[test]
    fn clear_drops_everything_without_counting_it_as_superseded() {
        let mut outbox = Outbox::new(2);
        outbox.push_cycle(0, vec![message("a", 2)]);
        outbox.push_cycle(1, vec![message("b", 3)]);
        assert_eq!(outbox.peek_len(), Some(2));
        outbox.clear();
        assert!(outbox.is_empty());
        assert_eq!(outbox.superseded_cycles(), 0);
        // 範囲外のクエリは無視する。
        assert_eq!(outbox.push_cycle(5, vec![message("x", 1)]), None);
        outbox.push_failure(5, vec![message("x", 1)]);
        assert!(outbox.is_empty());
        // 失敗の知らせも捨てる。
        outbox.push_failure(0, vec![bad("a", 2)]);
        assert!(outbox.has_failures());
        outbox.clear();
        assert!(outbox.is_empty());
        assert!(!outbox.has_failures());
    }

    fn bad(prefix: &str, n: usize) -> Vec<ValueItem> {
        (0..n)
            .map(|i| ValueItem::bad(&format!("{prefix}{i}")))
            .collect()
    }

    /// §10.3「タスクの異常」: 失敗の知らせ（全キー `bad`）は後から来た周期で置き換えず、
    /// 周期より先に送る。周期の列の古い未送信分は捨てる。
    #[test]
    fn failure_notices_are_never_superseded_and_go_first() {
        let mut outbox = Outbox::new(2);
        outbox.push_cycle(0, vec![message("old", 1)]);
        outbox.push_failure(0, vec![bad("a", 1), bad("a", 1)]);
        outbox.push_failure(1, vec![bad("b", 1)]);
        // 作り直したタスクの周期が先に来ても、失敗の知らせを置き換えない。
        assert_eq!(outbox.push_cycle(0, vec![message("new", 1)]), None);
        assert_eq!(outbox.push_cycle(1, vec![message("nb", 1)]), None);
        assert_eq!(outbox.superseded_cycles(), 0);
        let order: Vec<String> = std::iter::from_fn(|| outbox.pop())
            .map(|(_, m)| m[0].key.clone())
            .collect();
        // 失敗の知らせ（到着順）→ 周期（到着順）。止まったタスクの古い周期は捨てた。
        assert_eq!(order, vec!["a0", "a0", "b0", "new0", "nb0"]);
        assert!(outbox.is_empty());
        // 同じクエリの次の失敗の知らせは、送っていない前の分を置き換える（同じ全キー）。
        outbox.push_failure(0, vec![bad("a", 1), bad("a", 1)]);
        outbox.pop();
        outbox.push_failure(0, vec![bad("c", 1), bad("c", 1)]);
        assert_eq!(outbox.pop().unwrap().1[0].key, "c0");
        assert_eq!(outbox.pop().unwrap().1[0].key, "c0");
        assert!(outbox.pop().is_none());
    }
}
