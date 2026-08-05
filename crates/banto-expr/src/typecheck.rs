//! 型検査（= 演算タグの登録時検査、§4.2「型不一致は登録時エラー」）。
//!
//! ## タグ参照はなぜ常に `Num` か
//!
//! 本クレートはレジストリ（I1 の `tags` テーブル）に依存しない
//! （`crate` トップレベル doc・実装指示の「参照タグの存在確認は呼び出し側
//! の責務」）。にもかかわらず `1 + calc.line1.avg` のような式を型検査
//! できるのは、I1 のデータ型一覧（`bit`/`i16`/`u16`/`i32`/`u32`/`f32`/
//! `string`）に**ネイティブな Bool 型が存在しない**ため - `bit` 型ですら
//! 0/1 の数値として保持される（`crates/banto-tags/src/tag.rs` の
//! `NUMERIC_DATA_TYPES`）。したがって「タグ参照の型」は本クレート単体で
//! 一意に決まり、常に `Type::Num` として扱ってよい。`string` データ型の
//! タグだけが例外的に演算へ使えないが、その拒否は「このタグが文字列か」
//! というレジストリ照会そのものなので T6-2（呼び出し側）の責務であり、
//! 本クレートはそもそも文字列 `Value` を持たない（型システムに `String`
//! がない）ことでその境界を体現している。

use crate::ast::{BinOp, Expr, UnaryOp};
use crate::error::CompileError;
use crate::types::Type;

/// 既知の関数と固定引数個数。`if`/`min`/`max`/`abs`/`round`/`clamp`/`bit`
/// のみ（可変長引数・ユーザー定義関数はない、§4.2）。
const KNOWN_FUNCTIONS: &[(&str, usize)] = &[
    ("if", 3),
    ("min", 2),
    ("max", 2),
    ("abs", 1),
    ("round", 1),
    ("clamp", 3),
    ("bit", 2),
];

/// `expr` を型検査し、結果型を返す。参照したタグ参照の外部名を出現順
/// （重複を含む - 呼び出し元の `compile` が去重する）で `refs` に積む。
pub fn check(expr: &Expr, refs: &mut Vec<String>) -> Result<Type, CompileError> {
    match expr {
        Expr::Num(_, _) => Ok(Type::Num),
        Expr::Bool(_, _) => Ok(Type::Bool),
        Expr::TagRef { name, .. } => {
            refs.push(name.clone());
            Ok(Type::Num)
        }
        Expr::Unary { op, expr, pos } => {
            let inner = check(expr, refs)?;
            match (op, inner) {
                (UnaryOp::Neg, Type::Num) => Ok(Type::Num),
                (UnaryOp::Neg, Type::Bool) => Err(CompileError::TypeMismatch {
                    pos: *pos,
                    message: "単項 '-' は Num にのみ適用できます（Bool が与えられました）"
                        .to_string(),
                }),
                (UnaryOp::Not, Type::Bool) => Ok(Type::Bool),
                (UnaryOp::Not, Type::Num) => Err(CompileError::TypeMismatch {
                    pos: *pos,
                    message: "'!' は Bool にのみ適用できます（Num が与えられました）".to_string(),
                }),
            }
        }
        Expr::Binary { op, lhs, rhs, pos } => {
            let lt = check(lhs, refs)?;
            let rt = check(rhs, refs)?;
            check_binary(*op, lt, rt, *pos)
        }
        Expr::Call { name, args, pos } => check_call(name, args, *pos, refs),
    }
}

fn check_binary(op: BinOp, lt: Type, rt: Type, pos: usize) -> Result<Type, CompileError> {
    use BinOp::*;
    match op {
        Add | Sub | Mul | Div => match (lt, rt) {
            (Type::Num, Type::Num) => Ok(Type::Num),
            _ => Err(CompileError::TypeMismatch {
                pos,
                message: format!("四則演算は Num 同士のみ可能です（左辺 {lt}, 右辺 {rt}）"),
            }),
        },
        Lt | Gt | Le | Ge => match (lt, rt) {
            (Type::Num, Type::Num) => Ok(Type::Bool),
            _ => Err(CompileError::TypeMismatch {
                pos,
                message: format!(
                    "比較演算（<,>,<=,>=）は Num 同士のみ可能です（左辺 {lt}, 右辺 {rt}）"
                ),
            }),
        },
        Eq | Ne => {
            if lt == rt {
                Ok(Type::Bool)
            } else {
                Err(CompileError::TypeMismatch {
                    pos,
                    message: format!("==/!= は同じ型同士のみ比較できます（左辺 {lt}, 右辺 {rt}）"),
                })
            }
        }
        And | Or => match (lt, rt) {
            (Type::Bool, Type::Bool) => Ok(Type::Bool),
            _ => Err(CompileError::TypeMismatch {
                pos,
                message: format!("&&/|| は Bool 同士のみ可能です（左辺 {lt}, 右辺 {rt}）"),
            }),
        },
    }
}

fn check_call(
    name: &str,
    args: &[Expr],
    pos: usize,
    refs: &mut Vec<String>,
) -> Result<Type, CompileError> {
    let Some(&(fn_name, arity)) = KNOWN_FUNCTIONS.iter().find(|(n, _)| *n == name) else {
        return Err(CompileError::UnknownFunction {
            pos,
            name: name.to_string(),
        });
    };

    if args.len() != arity {
        return Err(CompileError::ArityMismatch {
            pos,
            name: fn_name,
            expected: arity,
            got: args.len(),
        });
    }

    match fn_name {
        "if" => {
            let cond = check(&args[0], refs)?;
            if cond != Type::Bool {
                return Err(CompileError::TypeMismatch {
                    pos: args[0].pos(),
                    message: format!("if() の第1引数（条件）は Bool が必要です（実際には {cond}）"),
                });
            }
            let a = check(&args[1], refs)?;
            let b = check(&args[2], refs)?;
            if a != b {
                return Err(CompileError::TypeMismatch {
                    pos,
                    message: format!(
                        "if() の第2・第3引数は同じ型が必要です（第2引数 {a}, 第3引数 {b}）"
                    ),
                });
            }
            Ok(a)
        }
        "min" | "max" => {
            let a = check(&args[0], refs)?;
            let b = check(&args[1], refs)?;
            if a != Type::Num || b != Type::Num {
                return Err(CompileError::TypeMismatch {
                    pos,
                    message: format!(
                        "{fn_name}() は Num 2個が必要です（第1引数 {a}, 第2引数 {b}）"
                    ),
                });
            }
            Ok(Type::Num)
        }
        "abs" | "round" => {
            let a = check(&args[0], refs)?;
            if a != Type::Num {
                return Err(CompileError::TypeMismatch {
                    pos,
                    message: format!("{fn_name}() は Num が必要です（実際には {a}）"),
                });
            }
            Ok(Type::Num)
        }
        "clamp" => {
            let x = check(&args[0], refs)?;
            let lo = check(&args[1], refs)?;
            let hi = check(&args[2], refs)?;
            if x != Type::Num || lo != Type::Num || hi != Type::Num {
                return Err(CompileError::TypeMismatch {
                    pos,
                    message: format!("clamp() は Num 3個が必要です（x={x}, lo={lo}, hi={hi}）"),
                });
            }
            Ok(Type::Num)
        }
        "bit" => check_bit(&args[0], &args[1], refs),
        _ => unreachable!("KNOWN_FUNCTIONS と match の分岐が食い違っています: {fn_name}"),
    }
}

/// `bit(tag, n)`: 第1引数はタグ参照そのもの（式の途中結果は不可）、第2引数
/// は `0..=15` の整数リテラル（式・変数・小数・範囲外はすべて拒否）。
fn check_bit(target: &Expr, index: &Expr, refs: &mut Vec<String>) -> Result<Type, CompileError> {
    match target {
        Expr::TagRef { name, .. } => refs.push(name.clone()),
        other => {
            return Err(CompileError::BadBitTarget {
                pos: other.pos(),
                message:
                    "bit() の第1引数はタグ参照でなければなりません（式の途中結果には使えません）"
                        .to_string(),
            })
        }
    }

    match index {
        Expr::Num(v, pos) => {
            if v.fract() != 0.0 {
                return Err(CompileError::BadBitIndex {
                    pos: *pos,
                    message: format!("整数リテラルが必要です（実際には {v}）"),
                });
            }
            if *v < 0.0 || *v > 15.0 {
                return Err(CompileError::BadBitIndex {
                    pos: *pos,
                    message: format!("0〜15の範囲である必要があります（実際には {v}）"),
                });
            }
            Ok(Type::Bool)
        }
        other => Err(CompileError::BadBitIndex {
            pos: other.pos(),
            message: "0〜15の整数リテラルが必要です（式・タグ参照は使えません）".to_string(),
        }),
    }
}
