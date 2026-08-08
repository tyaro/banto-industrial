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
//! 後）を返す。アルゴリズムは深さ優先探索の後行順（post-order）- 各ノードは
//! 自分の依存先をすべて処理し終えてから自分自身を結果に積むため、依存先は
//! 必ず依存元より前に来る（Kahn 法と違い、追加のキュー・入次数カウントが
//! 要らず、同時に白/灰/黒の3色 DFS で循環検出も1パスで済む）。探索本体
//! （`visit`）は言語レベルの自己再帰ではなく明示スタックによる反復実装 -
//! 理由は次段落。
//!
//! ## なぜ `visit` は反復実装か（DoS 対策、2026-08-08）
//!
//! `docs/improvement-plan.md` H1 残件。[`crate::parser`] の深さガード
//! （[`crate::MAX_NESTING_DEPTH`]、`crate` トップレベル doc の「DoS 対策」
//! 節参照）は**式1本の内部**のネスト深さしか縛らない。演算タグの依存
//! チェーン（「a は b に依存、b は c に依存、…」）はそれとは別の軸で、
//! T11 の CSV 一括インポートを使えば認証済みクライアントが数千個の演算
//! タグを一列の鎖として登録できてしまう。登録（rebuild）のたびに
//! `validate_dag` が呼ばれる（`apps/banto-hub/core/src/computed.rs`）ため、
//! `visit` を素朴な自己再帰で書くとこのチェーン長がそのまま呼び出し
//! スタックの深さになり、tokio ワーカースレッドの既定スタック（2MiB）を
//! 使い切ってスタックオーバーフロー＝プロセス全体の abort になりうる -
//! `crate` トップレベル doc の「DoS 対策」節と同種のリスクだが、攻撃面が
//! 式の深さではなく**登録されたタグの数**である点が異なり、
//! `MAX_NESTING_DEPTH` では防げない。
//!
//! 対策として `visit` はヒープ上の `Vec`（実質無制限に伸びる。スタックの
//! ような固定サイズの制約がない）をコールスタック代わりに使う明示スタック
//! の反復 DFS で書く。ノードに「入った時」と「依存をすべて処理し終えて
//! 出た時」の2フェーズを1つのフレームで表現する定石を使い、走査順・
//! `order` に積む順序（post-order）・循環検出時のパス復元は元の再帰実装と
//! 完全に同一になるよう作られている（詳細は `visit` 自身の doc 参照）。
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

/// `visit` の明示スタック1段分 - 元の再帰実装で `visit(name, ...)` の
/// 呼び出しごとに生成される呼び出しフレーム（ローカル変数 `name` +
/// 「`deps` の何番目まで処理したか」という実行位置）を構造体として明示的に
/// 持ち出したもの。`dep_idx` が元の `for dep in deps.iter()` の走査位置に
/// 対応する。3フィールドすべて参照 or `usize` で軽量に複製できるため
/// `Copy` を導出し、`call_stack` から読み出すたびに借用ではなく値で
/// 取り出せるようにしている（後述のとおり借用チェッカ的にも扱いやすい）。
#[derive(Clone, Copy)]
struct Frame<'a> {
    name: &'a str,
    deps: &'a [String],
    dep_idx: usize,
}

/// `name` を起点に深さ優先探索する（モジュール doc の「なぜ `visit` は
/// 反復実装か」参照）。
///
/// `call_stack`（ヒープ上の `Vec<Frame>`）が元の再帰の呼び出しスタックに
/// 相当する。ループの1反復は次のいずれか1つを行い、どちらも元の再帰版の
/// 対応する箇所と1対1で対応する:
///
/// - フレームの `dep_idx` が指す依存先がまだ未訪問なら、元の
///   `visit(dep, ...)` 呼び出しに相当する新しいフレームを push する
///   （= ノードに「入った時」。`state` を `InProgress` にし `stack` に
///   積むのも元の関数冒頭と同じ2行）。
/// - フレームが依存先をすべて処理し終えていたら（`dep_idx >= deps.len()`、
///   元の for ループを抜けた直後に相当）、フレームを pop し、`stack` から
///   降ろし、`state` を `Done` にし、`order` に積む（= ノードから
///   「出た時」。post-order で `order` に積む処理そのもの）。
///
/// 依存の走査順は `deps` を添字 0 から順に読むだけなので元の
/// `deps.iter()` と同一、`order` に積む順序も元の post-order と完全に
/// 同一になる。循環検出時のパス復元に使う `stack`（探索中の祖先チェーン）
/// も push/pop のタイミングが元の再帰版と完全に対応しているため、内容・
/// 順序ともに変わらない。循環を見つけたら（元の版が `?` で呼び出し元へ
/// 即座に伝播させるのと同じく）その場で `return Err(...)` し、途中の
/// フレームは `call_stack` ごと捨てる。
fn visit<'a>(
    name: &'a str,
    edges: &HashMap<&'a str, &'a [String]>,
    state: &mut HashMap<&'a str, State>,
    stack: &mut Vec<&'a str>,
    order: &mut Vec<String>,
) -> Result<(), CycleError> {
    let mut call_stack: Vec<Frame<'a>> = Vec::new();

    state.insert(name, State::InProgress);
    stack.push(name);
    call_stack.push(Frame {
        name,
        deps: edges.get(name).copied().unwrap_or(&[]),
        dep_idx: 0,
    });

    // `call_stack.last()` を毎回 `copied()` で値として取り出し、ループ本体
    // に入る時点で `call_stack` への借用を残さない - これにより本体内で
    // 自由に `call_stack.push`/`pop`/`last_mut` を呼べる（`Frame: Copy`
    // なので複製コストも無視できる）。
    while let Some(frame) = call_stack.last().copied() {
        if frame.dep_idx >= frame.deps.len() {
            // 元の for ループを抜けた直後の後始末（post-order）。
            call_stack.pop();
            stack.pop();
            state.insert(frame.name, State::Done);
            order.push(frame.name.to_string());
            continue;
        }

        let dep = frame.deps[frame.dep_idx].as_str();
        call_stack.last_mut().unwrap().dep_idx += 1;

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
                let mut cycle: Vec<String> = stack[start..].iter().map(|s| s.to_string()).collect();
                cycle.push(dep.to_string());
                return Err(CycleError { cycle });
            }
            Some(State::Unvisited) => {
                // 元の `visit(dep, ...)` 呼び出しに相当 - 新しいフレームを
                // 積んで「再帰」する。
                state.insert(dep, State::InProgress);
                stack.push(dep);
                call_stack.push(Frame {
                    name: dep,
                    deps: edges.get(dep).copied().unwrap_or(&[]),
                    dep_idx: 0,
                });
            }
        }
    }

    Ok(())
}
