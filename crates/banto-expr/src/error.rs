//! エラー型3種 - `compile` が返す [`CompileError`]、[`CompiledExpr::eval`]
//! (`crate::CompiledExpr::eval`) が返す [`EvalError`]、[`validate_dag`]
//! (`crate::validate_dag`) が返す [`CycleError`]。
//!
//! 管理 UI にそのまま出す想定（実装指示原文）のため、メッセージは日本語。
//! 位置情報はソース先頭からのバイトオフセット（`pos`）- 本文法は ASCII
//! のみ（[`crate::lexer`] のモジュール doc 参照）なのでバイトオフセットと
//! 文字オフセットは常に一致し、UI 側で列番号に変換する際も変換テーブルが
//! 要らない。

use thiserror::Error;

/// パース・型検査の失敗（= 演算タグ登録時エラー）。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CompileError {
    /// 字句・構文エラー（未知の文字、閉じ括弧なし、タグ参照のセグメント数
    /// 不正、等）。
    #[error("構文エラー（位置 {pos}）: {message}")]
    Syntax { pos: usize, message: String },

    /// 型規則違反（暗黙変換なしの結果、演算子・関数のオペランド型が
    /// 合わない）。
    #[error("型エラー（位置 {pos}）: {message}")]
    TypeMismatch { pos: usize, message: String },

    /// `if`/`min`/`max`/`abs`/`round`/`clamp`/`bit` 以外の識別子が
    /// 関数呼び出し構文（`ident(...)`）で使われた。
    #[error("未知の関数です（位置 {pos}）: {name}")]
    UnknownFunction { pos: usize, name: String },

    /// 既知の関数に対し引数の個数が合わない（本文法に可変長引数はない）。
    #[error(
        "関数 {name} の引数の数が不正です（位置 {pos}）: {expected} 個が必要ですが {got} 個です"
    )]
    ArityMismatch {
        pos: usize,
        name: &'static str,
        expected: usize,
        got: usize,
    },

    /// `bit(tag, n)` の `n` が「0〜15 の整数リテラル」でない（式・変数・
    /// 範囲外の数値・小数はすべてここで拒否）。
    #[error("bit() のビット位置が不正です（位置 {pos}）: {message}")]
    BadBitIndex { pos: usize, message: String },

    /// `bit(tag, n)` の第1引数がタグ参照そのものでない（式の途中結果には
    /// ビット抽出できない - §4.2 の `bit(tag, n)` はタグの生ワード値専用）。
    #[error("bit() の第1引数が不正です（位置 {pos}）: {message}")]
    BadBitTarget { pos: usize, message: String },
}

/// 評価時の失敗。NaN・ゼロ除算・オーバーフローは失敗ではなく IEEE 754 の
/// 値として伝播する（`crate` トップレベル doc の「NaN 方針」参照）- ここに
/// 挙げているのは「呼び出し側の契約違反」だけ。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum EvalError {
    /// 参照タグが `inputs` に存在しない（値がまだ収集されていない、名前を
    /// 間違えた、等）。
    #[error("タグの値がありません: {0}")]
    MissingTag(String),

    /// `inputs` が返した [`crate::Value`] の型が式の要求する型と違う
    /// （本クレートはレジストリを持たないため `compile` 時点では検出できず、
    /// 呼び出し側の契約 - タグ参照は常に Num 扱い - が破られたときだけ
    /// ここで捕まる。詳細は `crate` トップレベル doc 参照）。
    #[error("タグ {tag} の値の型が不正です（Num を期待しましたが Bool でした）")]
    UnexpectedValueType { tag: String },
}

/// [`validate_dag`](crate::validate_dag) が循環を検出したときに返す経路。
/// `cycle` は `["a", "b", "c", "a"]` のように、循環の開始ノードで始まり
/// 同じノードで終わる（自己参照なら `["a", "a"]`）。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("循環参照を検出しました: {}", .cycle.join(" -> "))]
pub struct CycleError {
    pub cycle: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compile_error_messages_include_position_and_are_japanese() {
        let e = CompileError::Syntax {
            pos: 3,
            message: "テスト".to_string(),
        };
        assert_eq!(e.to_string(), "構文エラー（位置 3）: テスト");

        let e = CompileError::TypeMismatch {
            pos: 5,
            message: "型が違う".to_string(),
        };
        assert_eq!(e.to_string(), "型エラー（位置 5）: 型が違う");

        let e = CompileError::UnknownFunction {
            pos: 1,
            name: "foo".to_string(),
        };
        assert_eq!(e.to_string(), "未知の関数です（位置 1）: foo");

        let e = CompileError::ArityMismatch {
            pos: 0,
            name: "min",
            expected: 2,
            got: 3,
        };
        assert_eq!(
            e.to_string(),
            "関数 min の引数の数が不正です（位置 0）: 2 個が必要ですが 3 個です"
        );
    }

    #[test]
    fn eval_error_messages() {
        let e = EvalError::MissingTag("calc.a.b".to_string());
        assert_eq!(e.to_string(), "タグの値がありません: calc.a.b");

        let e = EvalError::UnexpectedValueType {
            tag: "calc.a.b".to_string(),
        };
        assert_eq!(
            e.to_string(),
            "タグ calc.a.b の値の型が不正です（Num を期待しましたが Bool でした）"
        );
    }

    #[test]
    fn cycle_error_message_joins_path_with_arrows() {
        let e = CycleError {
            cycle: vec!["a".to_string(), "b".to_string(), "a".to_string()],
        };
        assert_eq!(e.to_string(), "循環参照を検出しました: a -> b -> a");
    }

    #[test]
    fn errors_are_equatable_and_cloneable() {
        let e1 = CompileError::UnknownFunction {
            pos: 1,
            name: "foo".to_string(),
        };
        let e2 = e1.clone();
        assert_eq!(e1, e2);
    }
}
