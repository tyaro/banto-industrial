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

    /// 次に送るメッセージの件数（無ければ `None`）。
    pub fn peek_len(&self) -> Option<usize> {
        self.oldest().map(|q| self.queries[q].messages[0].len())
    }

    /// 次に送るメッセージを取り出す（クエリの添字と一緒に）。
    pub fn pop(&mut self) -> Option<(usize, Vec<ValueItem>)> {
        let query = self.oldest()?;
        let message = self.queries[query].messages.pop_front()?;
        Some((query, message))
    }

    /// 送信待ちを全部捨てる（epoch が変わった・収集が止まった - 古い epoch の値は Hub が
    /// `stale_epoch` で断るので、送っても無駄）。捨てた分は「周期の置き換え」には数えない。
    pub fn clear(&mut self) {
        for pending in &mut self.queries {
            pending.messages.clear();
        }
    }

    pub fn is_empty(&self) -> bool {
        self.queries.iter().all(|p| p.messages.is_empty())
    }

    pub fn superseded_cycles(&self) -> u64 {
        self.superseded_cycles
    }

    pub fn superseded_items(&self) -> u64 {
        self.superseded_items
    }

    fn oldest(&self) -> Option<usize> {
        self.queries
            .iter()
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
        assert!(outbox.is_empty());
    }
}
