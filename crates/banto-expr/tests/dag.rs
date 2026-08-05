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
