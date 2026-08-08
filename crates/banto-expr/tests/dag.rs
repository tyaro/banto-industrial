//! DAG 検証の網羅テスト: 自己参照・2ノード循環・長い循環・ダイヤモンド
//! （非循環）・トポロジカル順の正しさ・グラフ外の外部名（葉）の扱い。

use banto_expr::validate_dag;

fn nodes(pairs: &[(&str, &[&str])]) -> Vec<(String, Vec<String>)> {
    pairs
        .iter()
        .map(|(name, deps)| {
            (
                name.to_string(),
                deps.iter().map(|d| d.to_string()).collect(),
            )
        })
        .collect()
}

/// `order` が `nodes` に対する有効なトポロジカル順であることを検証する:
/// 全ノードがちょうど1回現れ、各ノードの依存先（`nodes` に実在するものに
/// 限る）はそのノードより前に現れる。
fn assert_valid_topo_order(all: &[(String, Vec<String>)], order: &[String]) {
    assert_eq!(
        order.len(),
        all.len(),
        "topo order must contain every node exactly once"
    );
    let pos: std::collections::HashMap<&str, usize> = order
        .iter()
        .enumerate()
        .map(|(i, n)| (n.as_str(), i))
        .collect();
    for (name, deps) in all {
        let name_pos = pos[name.as_str()];
        for dep in deps {
            if let Some(&dep_pos) = pos.get(dep.as_str()) {
                assert!(
                    dep_pos < name_pos,
                    "dependency {dep} must come before {name} in {order:?}"
                );
            }
        }
    }
}

/// 長さ `n` の直列依存チェーンを作る: `a0 -> a1 -> a2 -> ... -> a(n-1)`
/// （`a(n-1)` が誰にも依存しない末端）。improvement-plan.md H1 残件
/// （`dag.rs` の `visit` 反復化）の回帰テスト用 - 数千段のチェーンは
/// banto-hub の T11 一括登録 API で現実に作られうる入力を模している。
fn linear_chain(n: usize) -> Vec<(String, Vec<String>)> {
    (0..n)
        .map(|i| {
            let deps = if i + 1 < n {
                vec![format!("a{}", i + 1)]
            } else {
                vec![]
            };
            (format!("a{i}"), deps)
        })
        .collect()
}

#[test]
fn self_reference_is_a_cycle() {
    let n = nodes(&[("a", &["a"])]);
    let err = validate_dag(&n).unwrap_err();
    assert_eq!(err.cycle, vec!["a".to_string(), "a".to_string()]);
}

#[test]
fn two_node_cycle_detected() {
    let n = nodes(&[("a", &["b"]), ("b", &["a"])]);
    let err = validate_dag(&n).unwrap_err();
    assert_eq!(
        err.cycle,
        vec!["a".to_string(), "b".to_string(), "a".to_string()]
    );
}

#[test]
fn long_cycle_of_five_nodes_detected() {
    let n = nodes(&[
        ("a", &["b"]),
        ("b", &["c"]),
        ("c", &["d"]),
        ("d", &["e"]),
        ("e", &["a"]),
    ]);
    let err = validate_dag(&n).unwrap_err();
    assert_eq!(err.cycle.first(), err.cycle.last());
    assert_eq!(err.cycle.len(), 6); // a,b,c,d,e,a
    let unique: std::collections::HashSet<&String> = err.cycle[..5].iter().collect();
    assert_eq!(unique.len(), 5, "cycle path should visit 5 distinct nodes");
}

#[test]
fn diamond_shape_is_not_a_cycle_and_has_valid_topo_order() {
    // a -> b, a -> c, b -> d, c -> d  (d は末端)
    let n = nodes(&[("a", &["b", "c"]), ("b", &["d"]), ("c", &["d"]), ("d", &[])]);
    let order = validate_dag(&n).unwrap();
    assert_valid_topo_order(&n, &order);
}

#[test]
fn linear_chain_topo_order_is_dependency_first() {
    let n = nodes(&[("a", &["b"]), ("b", &["c"]), ("c", &[])]);
    let order = validate_dag(&n).unwrap();
    assert_valid_topo_order(&n, &order);
    // c は誰にも依存しないので最初、a は誰も依存されないので最後。
    assert_eq!(order.last().unwrap(), "a");
    assert_eq!(order[0], "c");
}

#[test]
fn external_names_outside_nodes_are_leaves_not_part_of_cycle_detection() {
    // "calc.a" は "plc.tag" という computed でない外部名を参照するが、
    // それは nodes に無いので単なる葉として扱われ、循環とは無関係。
    let n = nodes(&[("calc.a", &["plc.tag", "calc.b"]), ("calc.b", &["plc.tag"])]);
    let order = validate_dag(&n).unwrap();
    assert_valid_topo_order(&n, &order);
    assert_eq!(order.len(), 2); // "plc.tag" は order に含まれない
}

#[test]
fn no_dependencies_at_all_is_trivially_valid() {
    let n = nodes(&[("a", &[]), ("b", &[]), ("c", &[])]);
    let order = validate_dag(&n).unwrap();
    assert_valid_topo_order(&n, &order);
    assert_eq!(order.len(), 3);
}

#[test]
fn empty_node_list_is_valid() {
    let n: Vec<(String, Vec<String>)> = vec![];
    let order = validate_dag(&n).unwrap();
    assert!(order.is_empty());
}

#[test]
fn disconnected_components_all_appear_in_order() {
    let n = nodes(&[("a", &["b"]), ("b", &[]), ("x", &["y"]), ("y", &[])]);
    let order = validate_dag(&n).unwrap();
    assert_valid_topo_order(&n, &order);
    assert_eq!(order.len(), 4);
}

#[test]
fn wide_fan_in_is_not_a_cycle() {
    // 多数のノードが同じ末端に依存する（ファンイン）。
    let n = nodes(&[
        ("a", &["z"]),
        ("b", &["z"]),
        ("c", &["z"]),
        ("d", &["z"]),
        ("z", &[]),
    ]);
    let order = validate_dag(&n).unwrap();
    assert_valid_topo_order(&n, &order);
    assert_eq!(order[0], "z");
}

#[test]
fn cycle_reachable_only_through_one_branch_of_a_diamond_is_still_detected() {
    // a -> b -> c -> b という循環が d 経由の非循環パスと共存するケース。
    let n = nodes(&[("a", &["b", "d"]), ("b", &["c"]), ("c", &["b"]), ("d", &[])]);
    let err = validate_dag(&n).unwrap_err();
    assert_eq!(
        err.cycle,
        vec!["b".to_string(), "c".to_string(), "b".to_string()]
    );
}

#[test]
fn cycle_error_display_contains_arrow_separated_path() {
    let n = nodes(&[("a", &["a"])]);
    let err = validate_dag(&n).unwrap_err();
    assert_eq!(err.to_string(), "循環参照を検出しました: a -> a");
}

// 以下は improvement-plan.md H1 残件（`dag.rs` の `visit` を自己再帰から
// 明示スタックの反復 DFS へ書き換えた変更）の回帰テスト。目的はスタック
// オーバーフローで落ちず、数千段のチェーンでも正常系・異常系ともに
// 妥当な結果を返すこと（`crates/banto-expr/src/parser.rs` の
// `MAX_NESTING_DEPTH` 系テストと同じ「クラッシュしないことの確認」という
// 位置付け）。

const DEEP_CHAIN_LEN: usize = 10_000;

#[test]
fn deep_linear_chain_does_not_overflow_and_has_valid_topo_order() {
    // a0 -> a1 -> ... -> a9999（a9999 が末端）を反復 DFS で辿り切れること、
    // かつ得られる順序が正しいトポロジカル順であることを確認する。
    let n = linear_chain(DEEP_CHAIN_LEN);
    let order = validate_dag(&n).unwrap();
    assert_valid_topo_order(&n, &order);
    assert_eq!(order.len(), DEEP_CHAIN_LEN);
    // 誰にも依存しない末端 a9999 が最初、誰にも依存されない根 a0 が最後。
    assert_eq!(order[0], format!("a{}", DEEP_CHAIN_LEN - 1));
    assert_eq!(order.last().unwrap(), "a0");
}

#[test]
fn deep_chain_with_cycle_at_the_tail_does_not_overflow_and_reports_cycle() {
    // a0 -> a1 -> ... -> a9999 だが、末端であるはずの a9999 は葉を持たず
    // 1つ前の a9998 を指し返す（a9998 <-> a9999 の2ノード循環）。循環に
    // 到達するには a0 から9999段降りる必要があるため、循環検出そのものが
    // 深いチェーンの踏破を強制する形になっている。
    let mut n = linear_chain(DEEP_CHAIN_LEN);
    let last = DEEP_CHAIN_LEN - 1;
    n[last].1 = vec![format!("a{}", last - 1)];

    let err = validate_dag(&n).unwrap_err();
    assert_eq!(
        err.cycle,
        vec![
            format!("a{}", last - 1),
            format!("a{}", last),
            format!("a{}", last - 1),
        ]
    );
}

#[test]
fn exact_topo_order_for_branching_graph_matches_pre_rewrite_snapshot() {
    // `visit` を反復実装へ書き換える前の再帰版で得られていたトポロジカル
    // 順をそのまま固定するスナップショット的テスト（b・c がそれぞれ複数の
    // 依存を持ち、e を両方から共有する多分岐グラフ）。`assert_valid_topo_order`
    // が保証する「有効などれか一つの順」ではなく、書き換え前と完全に同じ
    // 具体的な列であることまで確認する。
    let n = nodes(&[
        ("a", &["b", "c"]),
        ("b", &["d", "e"]),
        ("c", &["e", "f"]),
        ("d", &[]),
        ("e", &[]),
        ("f", &[]),
    ]);
    let order = validate_dag(&n).unwrap();
    assert_valid_topo_order(&n, &order);

    let expected: Vec<String> = ["d", "e", "b", "f", "c", "a"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(order, expected);
}
