//! ブリッジの送る量の見積もり（`banto_tags::bridge_load`、docs/banto-hub-ingest-design.md
//! §7.1）の表を、画面の見積もり（`apps/banto-hub/src/lib/banto/bridgeLoad.ts`、R2a-2e）と
//! **同じ fixtures**（`apps/banto-hub/src/lib/banto/bridgeLoad.fixtures.json`）で確かめる。
//!
//! §7.1 の 3.「計算は `bridge_load` と同じ式を TS に写し、Rust の表のテストの値を
//! fixtures にして一致させる」。fixtures を画面の側（`apps/banto-hub/`）に置き、Rust の
//! 側のテストをこの crate に置くのは、CI の変更の判定（`.github/scripts/ci-changes.mjs`）で
//! どちらを変えても両方のテストが走るようにするため:
//!
//! - fixtures か TS を変えた → `apps/banto-hub/` の変更なので、画面の vitest と
//!   banto-hub-core のテスト（このファイル）が走る。
//! - `banto_tags::push` の式を変えた → banto-tags に依存する banto-hub-core のテストが
//!   走り、fixtures とずれれば落ちる（fixtures を直すと画面の vitest も走る）。
//!
//! fixtures の `expected` は `UPDATE_BRIDGE_LOAD_FIXTURES=1 cargo test -p banto-hub-core
//! --lib bridge_load_fixtures` で作り直せる（`openapi_snapshot_matches` と同じ流儀。作り
//! 直すとオブジェクトのキーは辞書順で書かれるので、`pnpm exec prettier --write` で整形し、
//! 差分を見て意図した変更か確かめる。並びは読みやすさのためで、比較は値で行う）。

use banto_tags::{bridge_load, BridgeColumn, BridgeLoad, BridgeQuery, PushBridge};
use serde_json::{json, Value};

fn fixtures_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("src")
        .join("lib")
        .join("banto")
        .join("bridgeLoad.fixtures.json")
}

/// fixtures の `keys`（配列、または `{ count, template, padStart, padChar, repeat, times }`）
/// からキーの並びを作る。`bridgeLoad.test.ts` の `expandKeys` と同じ規則。
fn expand_keys(spec: &Value) -> Vec<String> {
    if let Some(keys) = spec.as_array() {
        return keys
            .iter()
            .map(|k| k.as_str().expect("keys[] は文字列").to_string())
            .collect();
    }
    let count = spec["count"].as_u64().expect("keys.count") as usize;
    if let Some(repeat) = spec.get("repeat").and_then(Value::as_str) {
        let times = spec["times"].as_u64().expect("keys.times") as usize;
        return vec![repeat.repeat(times); count];
    }
    let template = spec["template"].as_str().expect("keys.template");
    let pad_start = spec.get("padStart").and_then(Value::as_u64).unwrap_or(0) as usize;
    let pad_char = spec
        .get("padChar")
        .and_then(Value::as_str)
        .and_then(|s| s.chars().next())
        .unwrap_or(' ');
    (0..count)
        .map(|i| {
            let key = template.replace("{i}", &i.to_string());
            let len = key.chars().count();
            if len >= pad_start {
                key
            } else {
                let mut padded: String = std::iter::repeat_n(pad_char, pad_start - len).collect();
                padded.push_str(&key);
                padded
            }
        })
        .collect()
}

fn bridge_of(case: &Value) -> PushBridge {
    PushBridge {
        db_connection_id: 1,
        queries: case["queries"]
            .as_array()
            .expect("queries")
            .iter()
            .enumerate()
            .map(|(i, q)| BridgeQuery {
                name: format!("q{i}"),
                period_ms: q["periodMs"].as_i64().expect("periodMs"),
                sql: "SELECT 1 AS v".to_string(),
                columns: expand_keys(&q["keys"])
                    .into_iter()
                    .map(|key| BridgeColumn {
                        column: "v".to_string(),
                        key,
                    })
                    .collect(),
            })
            .collect(),
    }
}

/// `bridge_load` の結果を fixtures の `expected` の形にする（`bridgeLoad.ts` の
/// `BridgeLoad` と同じ名前）。
fn expected_of(load: &BridgeLoad) -> Value {
    json!({
        "queries": load.queries.iter().map(|q| json!({
            "items": q.items,
            "itemBytes": q.item_bytes,
            "messagesPerCycle": q.messages_per_cycle,
        })).collect::<Vec<_>>(),
        "messagesPerMinute": load.messages_per_minute,
        "itemsPerMinute": load.items_per_minute,
        "messagesPerSecText": load.messages_per_sec_text(),
        "itemsPerSecText": load.items_per_sec_text(),
        "withinLimit": load.within_registration_limit(),
        "message": load.over_registration_limit_message(),
    })
}

#[test]
fn bridge_load_matches_the_shared_fixtures() {
    let path = fixtures_path();
    let text = std::fs::read_to_string(&path).expect("bridgeLoad.fixtures.json を読めない");
    let mut fixtures: Value = serde_json::from_str(&text).expect("fixtures の JSON");
    let update = matches!(
        std::env::var("UPDATE_BRIDGE_LOAD_FIXTURES").as_deref(),
        Ok("1")
    );
    let cases = fixtures["cases"].as_array_mut().expect("cases");
    assert!(cases.len() >= 10, "fixtures の場面が少なすぎる");
    for case in cases.iter_mut() {
        let actual = expected_of(&bridge_load(&bridge_of(case)));
        if update {
            case["expected"] = actual;
            continue;
        }
        assert_eq!(
            case["expected"], actual,
            "場面「{}」の expected が bridge_load と違う（UPDATE_BRIDGE_LOAD_FIXTURES=1 で作り直せる）",
            case["name"]
        );
    }
    if update {
        let mut out = serde_json::to_string_pretty(&fixtures).unwrap();
        out.push('\n');
        std::fs::write(&path, out).unwrap();
    }
}

/// fixtures の数のいくつかを §7.1 の表（banto-tags の境界値のテスト）と手で突き合わせる
/// （fixtures を作り直したときに、式そのものの退行を見落とさないため）。
#[test]
fn the_fixtures_agree_with_the_design_table() {
    let text = std::fs::read_to_string(fixtures_path()).unwrap();
    let fixtures: Value = serde_json::from_str(&text).unwrap();
    let by_name = |prefix: &str| -> Value {
        fixtures["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"].as_str().unwrap().starts_with(prefix))
            .unwrap_or_else(|| panic!("場面 {prefix} が無い"))["expected"]
            .clone()
    };
    assert_eq!(by_name("1,000 件")["queries"][0]["messagesPerCycle"], 1);
    assert_eq!(by_name("1,001 件")["queries"][0]["messagesPerCycle"], 2);
    let long = by_name("200 バイト");
    assert_eq!(long["queries"][0]["itemBytes"], 296_000);
    assert_eq!(long["queries"][0]["messagesPerCycle"], 2);
    assert_eq!(by_name("16 メッセージ")["withinLimit"], true);
    assert_eq!(by_name("17 メッセージ")["withinLimit"], false);
    let parity = by_name("§10.4 の 9: 500 ms");
    assert_eq!(parity["messagesPerSecText"], "8");
    assert_eq!(parity["itemsPerSecText"], "4,000");
    assert_eq!(parity["withinLimit"], true);
    assert_eq!(by_name("§10.4 の 9: 同じ")["withinLimit"], false);
    assert_eq!(by_name("60,000 ms")["messagesPerSecText"], "0.1");
}
