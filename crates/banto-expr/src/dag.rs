//! DAG 検証（§4.2「演算タグが演算タグを参照するのは可、循環は登録時
//! 検証で拒否」）。
//!
//! `validate_dag` は「タグ名 → そのタグの式が参照する外部名一覧」の集合を
//! 受け取り、演算タグ同士の依存グラフに循環がないか検査する。`nodes` に
//! 現れない外部名（PLC タグ・他 `computed` に依存しない `internal` タグ
//! 等）は依存グラフの外側の「葉」として扱い、循環検出の対象にしない -
//! 単にその名前をキーとする依存辺が存在しないだけなので、探索はそこで
//! 自然に止まる。
//!
//! 成功時は評価に使えるトポロジカル順（依存されるタグが先、依存するタグが
//! 後）を返す。実装は深さ優先探索の後行順（post-order）- 各ノードは
//! 自分の依存先をすべて再帰的に処理し終えてから自分自身を結果に積むため、
//! 依存先は必ず依存元より前に来る（Kahn 法と違い、追加のキュー・入次数
//! カウントが要らず、同時に白/灰/黒の3色 DFS で循環検出も1パスで済む）。
//!
//! `nodes` に同名タグが複数現れた場合は最後の要素が使われる（呼び出し側
//! T6-2 のレジストリがタグ名の一意性を保証している前提。詳細は
//! `crate` トップレベル doc 参照）。

use std::collections::HashMap;

use crate::error::CycleError;

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Unvisited,
    InProgress,
    Done,
}

pub fn validate_dag(nodes: &[(String, Vec<String>)]) -> Result<Vec<String>, CycleError> {
    let mut edges: HashMap<&str, &[String]> = HashMap::new();
    for (name, deps) in nodes {
        edges.insert(name.as_str(), deps.as_slice());
    }

    let mut state: HashMap<&str, State> = nodes
        .iter()
        .map(|(name, _)| (name.as_str(), State::Unvisited))
        .collect();
    let mut order: Vec<String> = Vec::with_capacity(nodes.len());
    let mut stack: Vec<&str> = Vec::new();

    for (name, _) in nodes {
        if state.get(name.as_str()).copied() == Some(State::Unvisited) {
            visit(name, &edges, &mut state, &mut stack, &mut order)?;
        }
    }

    Ok(order)
}

fn visit<'a>(
    name: &'a str,
    edges: &HashMap<&'a str, &'a [String]>,
    state: &mut HashMap<&'a str, State>,
    stack: &mut Vec<&'a str>,
    order: &mut Vec<String>,
) -> Result<(), CycleError> {
    state.insert(name, State::InProgress);
    stack.push(name);

    if let Some(deps) = edges.get(name) {
        for dep in deps.iter() {
            let dep = dep.as_str();
            match state.get(dep).copied() {
                None => {
                    // `nodes` に無い外部名 - 依存グラフの葉。探索しない。
                }
                Some(State::Done) => {}
                Some(State::InProgress) => {
                    // 循環発見: スタック上の dep の出現位置から現在までを
                    // 経路として切り出し、先頭を末尾にもう一度積んで
                    // 「a -> b -> ... -> a」の形にする。
                    let start = stack.iter().position(|&n| n == dep).unwrap();
                    let mut cycle: Vec<String> =
                        stack[start..].iter().map(|s| s.to_string()).collect();
                    cycle.push(dep.to_string());
                    return Err(CycleError { cycle });
                }
                Some(State::Unvisited) => {
                    visit(dep, edges, state, stack, order)?;
                }
            }
        }
    }

    stack.pop();
    state.insert(name, State::Done);
    order.push(name.to_string());
    Ok(())
}
