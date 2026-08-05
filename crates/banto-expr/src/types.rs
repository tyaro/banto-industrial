//! 式言語の型システム - [`Type`]（コンパイル時）と [`Value`]（実行時）。
//!
//! 型は `Num`・`Bool` の2種のみ（§4.2「暗黙型変換なし」）。文字列型は
//! 存在しない（文字列タグは演算に使えない - 参照自体の拒否は T6-2 の
//! 責務）。

/// コンパイル時の型。`CompiledExpr::result_type` および型検査の内部判定に
/// 使う。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Type {
    /// f64。四則演算・比較・`min`/`max`/`abs`/`round`/`clamp` の対象。
    /// タグ参照は常に `Num`（理由は `crate` トップレベル doc 参照）。
    Num,
    /// 真偽値。比較演算・論理演算・`if` の条件・`bit()` の結果の型。
    Bool,
}

impl std::fmt::Display for Type {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Type::Num => write!(f, "Num"),
            Type::Bool => write!(f, "Bool"),
        }
    }
}

/// 実行時の値。`Type` と1対1対応する（`Num` ⇔ `Value::Num`、`Bool` ⇔
/// `Value::Bool`）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Value {
    Num(f64),
    Bool(bool),
}

impl Value {
    /// この値の型。
    pub fn ty(&self) -> Type {
        match self {
            Value::Num(_) => Type::Num,
            Value::Bool(_) => Type::Bool,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_display_matches_expected_names() {
        assert_eq!(Type::Num.to_string(), "Num");
        assert_eq!(Type::Bool.to_string(), "Bool");
    }

    #[test]
    fn value_ty_reports_matching_type() {
        assert_eq!(Value::Num(1.0).ty(), Type::Num);
        assert_eq!(Value::Bool(true).ty(), Type::Bool);
    }

    #[test]
    fn type_equality() {
        assert_eq!(Type::Num, Type::Num);
        assert_ne!(Type::Num, Type::Bool);
    }
}
