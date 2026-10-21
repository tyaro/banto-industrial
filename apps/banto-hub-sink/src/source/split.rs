//! 1 周期の値を送信の単位に分ける（docs/banto-hub-ingest-design.md §7.1 の (a)）。
//!
//! 1 周期の全キーを、**件数が `max_items`（1,000）以下、かつ JSON の `items` の配列が
//! `max_message_bytes − 512` バイト（256 KiB − 512）以下**になるように、複数の
//! `values` メッセージへ分ける（同じ `epoch`、`seq` は送るときに連番）。外枠
//! （`{"op":"values","seq":..,"epoch":..,"items":}`）は 100 バイトに満たないので、
//! 512 バイトの余裕の中に収まり、1 メッセージは Hub の 1 メッセージ・1 フレームの上限
//! （256 KiB、超えたら close 1009）を超えない。
//!
//! - 上限の数値は Hub の `welcome` の `limits` から取る（§7「ブリッジ（`welcome` で受けた
//!   値）が同じ値を使う」）。外枠の余裕 [`MESSAGE_OVERHEAD_BYTES`] は登録時の見積もり
//!   （`banto_tags::bridge_load` の `bridge_message_overhead_bytes`）と同じ値で、テストが
//!   `banto_tags::PUSH_LIMITS` と突き合わせる（このクレートは `banto-tags` を本番の依存に
//!   しない - sqlx の sqlite がサイドカーの exe に入るため。dev-dependency でだけ照合する）。
//! - バイト数は見積もりではなく**実際に送る JSON** で数える。登録時の見積もり
//!   （1 件 = キーのバイト数 + 96）は最悪の大きさなので、分けた数はその見積もりの
//!   `messages_per_cycle` を超えない（テストで確かめる）。
//! - 1 件だけで予算を超える件は無い（キーは 256 文字 = UTF-8 で最大 1 KiB）が、
//!   来ても 1 件のメッセージにして先へ進む（止まらない）。

use crate::source::wire::ValueItem;

/// 1 メッセージの外枠の余裕（バイト）。`banto_tags::PUSH_LIMITS.bridge_message_overhead_bytes`
/// と同じ値（テストで照合）。
pub const MESSAGE_OVERHEAD_BYTES: u64 = 512;

/// `items` の配列（`[` と `]` を含む）に使ってよいバイト数。
pub fn items_budget_bytes(max_message_bytes: u64) -> usize {
    usize::try_from(max_message_bytes.saturating_sub(MESSAGE_OVERHEAD_BYTES)).unwrap_or(usize::MAX)
}

/// 1 件の JSON のバイト数。
fn item_bytes(item: &ValueItem) -> usize {
    serde_json::to_string(item).map_or(usize::MAX, |text| text.len())
}

/// 1 周期の値（`items`）を送信の単位に分ける（純関数）。並びは保つ。空なら空。
pub fn split_cycle(
    items: Vec<ValueItem>,
    max_items: u64,
    max_message_bytes: u64,
) -> Vec<Vec<ValueItem>> {
    let max_items = usize::try_from(max_items.max(1)).unwrap_or(usize::MAX);
    let budget = items_budget_bytes(max_message_bytes);
    let mut messages = Vec::new();
    let mut current: Vec<ValueItem> = Vec::new();
    // `[` と `]` の 2 バイト + 件の JSON + 件の間の `,`。
    let mut current_bytes = 2usize;
    for item in items {
        let size = item_bytes(&item);
        let separator = usize::from(!current.is_empty());
        let fits = current.len() < max_items
            && current_bytes.saturating_add(separator).saturating_add(size) <= budget;
        if !fits && !current.is_empty() {
            messages.push(std::mem::take(&mut current));
            current_bytes = 2;
        }
        let separator = usize::from(!current.is_empty());
        current_bytes = current_bytes.saturating_add(separator).saturating_add(size);
        current.push(item);
    }
    if !current.is_empty() {
        messages.push(current);
    }
    messages
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::wire::values_message;
    use banto_tags::{bridge_load, BridgeColumn, BridgeQuery, PushBridge, PUSH_LIMITS};

    const MAX_ITEMS: u64 = 1_000;
    const MAX_BYTES: u64 = 256 * 1024;

    fn items(n: usize, key_len: usize) -> Vec<ValueItem> {
        (0..n)
            .map(|i| {
                let prefix = format!("{i:06}");
                let pad = key_len.saturating_sub(prefix.len());
                ValueItem::good(&format!("{prefix}{}", "k".repeat(pad)), 12.5)
            })
            .collect()
    }

    fn assert_every_message_fits(messages: &[Vec<ValueItem>]) {
        for (seq, message) in messages.iter().enumerate() {
            assert!(message.len() as u64 <= MAX_ITEMS);
            // 外枠を含めた実際の 1 メッセージ（epoch と seq は大きめの数で）。
            let text = values_message(u64::MAX - seq as u64, u64::MAX, message);
            assert!(
                text.len() as u64 <= MAX_BYTES,
                "message {seq} is {} bytes",
                text.len()
            );
        }
    }

    /// 上限は Hub と同じ定数（`banto_tags::PUSH_LIMITS`）に揃っている。
    #[test]
    fn the_overhead_matches_the_registration_estimate() {
        assert_eq!(
            MESSAGE_OVERHEAD_BYTES,
            PUSH_LIMITS.bridge_message_overhead_bytes
        );
        assert_eq!(MAX_ITEMS, PUSH_LIMITS.max_items_per_message);
        assert_eq!(MAX_BYTES, PUSH_LIMITS.max_ws_message_bytes);
    }

    /// §11 の R2a-3b: 1,000 件 → 1 メッセージ、1,001 件 → 2。
    #[test]
    fn item_count_boundary() {
        let one = split_cycle(items(1_000, 12), MAX_ITEMS, MAX_BYTES);
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].len(), 1_000);
        let two = split_cycle(items(1_001, 12), MAX_ITEMS, MAX_BYTES);
        assert_eq!(two.iter().map(Vec::len).collect::<Vec<_>>(), vec![1_000, 1]);
        let many = split_cycle(items(2_500, 12), MAX_ITEMS, MAX_BYTES);
        assert_eq!(
            many.iter().map(Vec::len).collect::<Vec<_>>(),
            vec![1_000, 1_000, 500]
        );
        assert_every_message_fits(&many);
        assert!(split_cycle(Vec::new(), MAX_ITEMS, MAX_BYTES).is_empty());
    }

    /// §11 の R2a-3b: バイト数の境界でも分かれる（256 文字のキーでは 1,000 件より手前で）。
    #[test]
    fn byte_boundary() {
        let key_len = 256;
        let size = item_bytes(&items(1, key_len)[0]);
        let budget = items_budget_bytes(MAX_BYTES);
        // `[` `]` + n 件 + (n − 1) 個の `,` が予算に収まる最大の n。
        let fit = (budget - 2 + 1) / (size + 1);
        assert!(fit < 1_000, "the byte budget must bind first ({fit})");

        let exact = split_cycle(items(fit, key_len), MAX_ITEMS, MAX_BYTES);
        assert_eq!(exact.len(), 1, "exactly at the byte budget is one message");
        let over = split_cycle(items(fit + 1, key_len), MAX_ITEMS, MAX_BYTES);
        assert_eq!(
            over.iter().map(Vec::len).collect::<Vec<_>>(),
            vec![fit, 1],
            "one item over the byte budget splits"
        );
        assert_every_message_fits(&over);

        // 分けた数は登録時の見積もり（`bridge_load`）の 1 周期のメッセージ数を超えない。
        for n in [fit, fit + 1, 1_000, 1_001, 2_500] {
            let cycle = items(n, key_len);
            let bridge = PushBridge {
                db_connection_id: 1,
                queries: vec![BridgeQuery {
                    name: "q".to_string(),
                    period_ms: 1_000,
                    sql: "SELECT 1 AS v".to_string(),
                    columns: cycle
                        .iter()
                        .map(|item| BridgeColumn {
                            column: "v".to_string(),
                            key: item.key.clone(),
                        })
                        .collect(),
                }],
            };
            let estimate = bridge_load(&bridge).queries[0].messages_per_cycle;
            let actual = split_cycle(cycle, MAX_ITEMS, MAX_BYTES).len() as u64;
            assert!(actual <= estimate, "n={n}: {actual} > {estimate}");
        }
    }

    /// §10.4 の 9 の設定（§11 の R2a-3b の単体テスト）: 周期 500 ms のクエリ 3 本（A 1,800 件・
    /// B・C 各 100 件）は見積もりの上限ちょうど（8 メッセージ / 秒・4,000 件 / 秒）で受理され、
    /// A は 2 メッセージに分かれる。A の周期だけ 200 ms にした設定は登録の時点で
    /// `FieldError`（A だけで 10 メッセージ / 秒・9,000 件 / 秒）。
    #[test]
    fn the_section_10_4_case_9_configurations() {
        let settings = |period_a: i64| {
            let columns = |prefix: &str, n: usize| {
                (0..n)
                    .map(
                        |i| serde_json::json!({ "column": "v", "key": format!("{prefix}.{i:04}") }),
                    )
                    .collect::<Vec<_>>()
            };
            serde_json::json!({ "bridge": { "kind": "db", "db_connection_id": 1, "queries": [
                { "name": "A", "period_ms": period_a, "sql": "SELECT 1 AS v", "columns": columns("A", 1_800) },
                { "name": "B", "period_ms": 500, "sql": "SELECT 1 AS v", "columns": columns("B", 100) },
                { "name": "C", "period_ms": 500, "sql": "SELECT 1 AS v", "columns": columns("C", 100) } ] } })
        };
        let accepted = banto_tags::validate_push_settings(&settings(500))
            .expect("the limit-exact configuration is accepted");
        let load = bridge_load(accepted.bridge.as_ref().unwrap());
        assert_eq!(load.messages_per_minute, 8 * 60);
        assert_eq!(load.items_per_minute, 4_000 * 60);
        assert_eq!(load.queries[0].messages_per_cycle, 2);
        // 実際の分割も A は 2 メッセージ。
        let cycle: Vec<ValueItem> = (0..1_800)
            .map(|i| ValueItem::good(&format!("A.{i:04}"), 1.0))
            .collect();
        assert_eq!(split_cycle(cycle, MAX_ITEMS, MAX_BYTES).len(), 2);

        let refused = banto_tags::validate_push_settings(&settings(200))
            .expect_err("A at 200 ms exceeds 80 % of the limits");
        assert!(
            refused.iter().any(|e| e.field.starts_with("settings")),
            "{refused:?}"
        );
    }

    /// 並びを保ち、1 件も落とさない。
    #[test]
    fn splitting_keeps_every_item_in_order() {
        let cycle = items(2_345, 200);
        let keys: Vec<String> = cycle.iter().map(|i| i.key.clone()).collect();
        let messages = split_cycle(cycle, MAX_ITEMS, MAX_BYTES);
        let joined: Vec<String> = messages
            .iter()
            .flat_map(|m| m.iter().map(|i| i.key.clone()))
            .collect();
        assert_eq!(joined, keys);
        assert_every_message_fits(&messages);
    }
}
