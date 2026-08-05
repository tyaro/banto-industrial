//! 評価器。純関数・副作用なし・外部 I/O なし（`inputs` はただの関数
//! ポインタ／クロージャで、タグ空間へのアクセスは呼び出し側（T6-2）
//! が用意する）。
//!
//! ## NaN・ゼロ除算・オーバーフローの方針
//!
//! `compile` を通った式は型検査済みなので、評価そのものが失敗することは
//! ない - 数値演算は IEEE 754 の f64 セマンティクスにそのまま従い、
//! ゼロ除算・NaN・無限大は **エラーにせず値として伝播** させる
//! （`1.0 / 0.0 == f64::INFINITY`、`0.0 / 0.0` は NaN、`NaN + 1` は NaN、
//! 比較演算子は IEEE 754 の規則どおり NaN を含む比較はすべて `false`
//! になる、等）。理由（実装指示のとおり）: 演算タグの値の品質は入力タグの
//! 品質（Bad/Stale/Good、T6-2 が継承する）から決まるべきであり、
//! エンジン自身が「この数値はおかしいから Err」と判定すると、品質管理の
//! 責務がここに漏れ出してしまう。`EvalError` は「呼び出し側の契約違反」
//! （参照タグが `inputs` にない、`inputs` が返した値の型が期待と違う）
//! だけを表す。
//!
//! ## `bit(tag, n)` の対象値の整数化
//!
//! ワード値（f64）を Rust の `as i64` キャストにそのまま従わせる - この
//! キャストは Rust 1.45 以降 **必ず飽和する**（NaN は 0、範囲外は
//! `i64::MIN`/`i64::MAX` に飽和、それ以外は 0 方向への切り捨て）と
//! 言語仕様で保証されているため、追加の場合分けをせずに「NaN・非整数・
//! 極端に大きい値」のすべてに決定論的な挙動を与えられる。切り捨てる
//! 理由: ワード値は本来整数のはずで、小数部があるなら異常値と考えられる。
//! `round()` のような四捨五入をすると意図しないビット反転（例えば
//! 1.9999 を 2 に丸めてビットパターンが変わる）を招きかねないため、
//! 丸めではなく切り捨てを選ぶ。結果は下位16ビットに切り詰め、符号は
//! 2の補数として扱う（`i16` タグの負値でもビット抽出が自然になる）。

use crate::ast::{BinOp, Expr, UnaryOp};
use crate::error::EvalError;
use crate::types::Value;

pub fn eval(expr: &Expr, inputs: &dyn Fn(&str) -> Option<Value>) -> Result<Value, EvalError> {
    match expr {
        Expr::Num(v, _) => Ok(Value::Num(*v)),
        Expr::Bool(b, _) => Ok(Value::Bool(*b)),
        Expr::TagRef { name, .. } => match inputs(name) {
            None => Err(EvalError::MissingTag(name.clone())),
            Some(Value::Num(v)) => Ok(Value::Num(v)),
            Some(Value::Bool(_)) => Err(EvalError::UnexpectedValueType { tag: name.clone() }),
        },
        Expr::Unary { op, expr, .. } => {
            let v = eval(expr, inputs)?;
            match (op, v) {
                (UnaryOp::Neg, Value::Num(n)) => Ok(Value::Num(-n)),
                (UnaryOp::Not, Value::Bool(b)) => Ok(Value::Bool(!b)),
                // 型検査済みの式では起こらない組み合わせ - eval はここに
                // 来ても panic せず、呼び出し側の契約違反として報告する。
                (UnaryOp::Neg, Value::Bool(_)) => Err(EvalError::UnexpectedValueType {
                    tag: "<内部エラー: 型検査済みの式で単項 '-' に Bool が渡りました>".to_string(),
                }),
                (UnaryOp::Not, Value::Num(_)) => Err(EvalError::UnexpectedValueType {
                    tag: "<内部エラー: 型検査済みの式で '!' に Num が渡りました>".to_string(),
                }),
            }
        }
        Expr::Binary { op, lhs, rhs, .. } => {
            let l = eval(lhs, inputs)?;
            let r = eval(rhs, inputs)?;
            eval_binary(*op, l, r)
        }
        Expr::Call { name, args, .. } => eval_call(name, args, inputs),
    }
}

fn eval_binary(op: BinOp, l: Value, r: Value) -> Result<Value, EvalError> {
    use BinOp::*;
    Ok(match (op, l, r) {
        (Add, Value::Num(a), Value::Num(b)) => Value::Num(a + b),
        (Sub, Value::Num(a), Value::Num(b)) => Value::Num(a - b),
        (Mul, Value::Num(a), Value::Num(b)) => Value::Num(a * b),
        (Div, Value::Num(a), Value::Num(b)) => Value::Num(a / b),
        (Lt, Value::Num(a), Value::Num(b)) => Value::Bool(a < b),
        (Gt, Value::Num(a), Value::Num(b)) => Value::Bool(a > b),
        (Le, Value::Num(a), Value::Num(b)) => Value::Bool(a <= b),
        (Ge, Value::Num(a), Value::Num(b)) => Value::Bool(a >= b),
        (Eq, Value::Num(a), Value::Num(b)) => Value::Bool(a == b),
        (Eq, Value::Bool(a), Value::Bool(b)) => Value::Bool(a == b),
        (Ne, Value::Num(a), Value::Num(b)) => Value::Bool(a != b),
        (Ne, Value::Bool(a), Value::Bool(b)) => Value::Bool(a != b),
        (And, Value::Bool(a), Value::Bool(b)) => Value::Bool(a && b),
        (Or, Value::Bool(a), Value::Bool(b)) => Value::Bool(a || b),
        // 型検査済みの式では到達しない組み合わせ。
        _ => {
            return Err(EvalError::UnexpectedValueType {
                tag: "<内部エラー: 型検査済みの式で二項演算子の型が一致しませんでした>".to_string(),
            })
        }
    })
}

fn eval_call(
    name: &str,
    args: &[Expr],
    inputs: &dyn Fn(&str) -> Option<Value>,
) -> Result<Value, EvalError> {
    match name {
        "if" => {
            let cond = as_bool(eval(&args[0], inputs)?)?;
            if cond {
                eval(&args[1], inputs)
            } else {
                eval(&args[2], inputs)
            }
        }
        "min" => {
            let a = as_num(eval(&args[0], inputs)?)?;
            let b = as_num(eval(&args[1], inputs)?)?;
            Ok(Value::Num(a.min(b)))
        }
        "max" => {
            let a = as_num(eval(&args[0], inputs)?)?;
            let b = as_num(eval(&args[1], inputs)?)?;
            Ok(Value::Num(a.max(b)))
        }
        "abs" => {
            let a = as_num(eval(&args[0], inputs)?)?;
            Ok(Value::Num(a.abs()))
        }
        "round" => {
            let a = as_num(eval(&args[0], inputs)?)?;
            // half-away-from-zero（Rust の f64::round そのもの）。
            Ok(Value::Num(a.round()))
        }
        "clamp" => {
            let x = as_num(eval(&args[0], inputs)?)?;
            let lo = as_num(eval(&args[1], inputs)?)?;
            let hi = as_num(eval(&args[2], inputs)?)?;
            // lo > hi の場合の検証はしない（呼び出し側/UI の責務）。この式
            // だと lo > hi のときは常に hi を返す（x.max(lo) >= lo > hi な
            // ので最終的に .min(hi) で hi に落ちる）という決定論的な挙動に
            // なる。
            Ok(Value::Num(x.max(lo).min(hi)))
        }
        "bit" => eval_bit(&args[0], &args[1], inputs),
        _ => unreachable!("型検査済みの式に未知関数 '{name}' が含まれています"),
    }
}

fn eval_bit(
    target: &Expr,
    index: &Expr,
    inputs: &dyn Fn(&str) -> Option<Value>,
) -> Result<Value, EvalError> {
    let word = as_num(eval(target, inputs)?)?;
    let n = match index {
        Expr::Num(v, _) => *v as u32,
        _ => unreachable!("型検査済みの式で bit() の第2引数がリテラルではありません"),
    };
    // モジュール doc の「NaN/非整数の対象値」節参照。
    let as_i64 = word as i64;
    let word16 = (as_i64 as u32) & 0xFFFF;
    Ok(Value::Bool((word16 >> n) & 1 == 1))
}

fn as_num(v: Value) -> Result<f64, EvalError> {
    match v {
        Value::Num(n) => Ok(n),
        Value::Bool(_) => Err(EvalError::UnexpectedValueType {
            tag: "<内部エラー: 型検査済みの式で Num を期待した箇所に Bool が渡りました>"
                .to_string(),
        }),
    }
}

fn as_bool(v: Value) -> Result<bool, EvalError> {
    match v {
        Value::Bool(b) => Ok(b),
        Value::Num(_) => Err(EvalError::UnexpectedValueType {
            tag: "<内部エラー: 型検査済みの式で Bool を期待した箇所に Num が渡りました>"
                .to_string(),
        }),
    }
}
