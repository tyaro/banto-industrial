//! `compile()` の網羅テスト: 字句・構文（全演算子・優先順位・タグ参照・
//! エラー位置）、型検査（全型規則の受理・拒否ペア）、文法の閉性（拒否すべき
//! 構文の網羅）。評価そのものは `tests/eval.rs`、DAG は `tests/dag.rs`。

use banto_expr::{compile, CompileError, Type};

fn assert_compiles(source: &str) {
    compile(source).unwrap_or_else(|e| panic!("expected {source:?} to compile, got {e:?}"));
}

fn assert_result_type(source: &str, ty: Type) {
    let compiled =
        compile(source).unwrap_or_else(|e| panic!("expected {source:?} to compile, got {e:?}"));
    assert_eq!(
        compiled.result_type(),
        ty,
        "wrong result type for {source:?}"
    );
}

fn assert_rejected(source: &str) -> CompileError {
    compile(source).expect_err(&format!("expected {source:?} to be rejected"))
}

// ---------- 全演算子が受理される ----------

#[test]
fn all_arithmetic_operators_accepted() {
    for src in ["1 + 2", "1 - 2", "1 * 2", "1 / 2", "-1"] {
        assert_result_type(src, Type::Num);
    }
}

#[test]
fn all_comparison_operators_accepted() {
    for src in ["1 == 2", "1 != 2", "1 < 2", "1 > 2", "1 <= 2", "1 >= 2"] {
        assert_result_type(src, Type::Bool);
    }
}

#[test]
fn all_logical_operators_accepted() {
    for src in ["true && false", "true || false", "!true"] {
        assert_result_type(src, Type::Bool);
    }
}

#[test]
fn all_builtin_functions_accepted() {
    assert_result_type("if(true, 1, 2)", Type::Num);
    assert_result_type("if(true, true, false)", Type::Bool);
    assert_result_type("min(1, 2)", Type::Num);
    assert_result_type("max(1, 2)", Type::Num);
    assert_result_type("abs(-1)", Type::Num);
    assert_result_type("round(1.5)", Type::Num);
    assert_result_type("clamp(1, 0, 10)", Type::Num);
    assert_result_type("bit(calc.a.b, 0)", Type::Bool);
}

// ---------- 優先順位 ----------

#[test]
fn precedence_examples_all_compile() {
    for src in [
        "1 + 2 * 3",
        "-1 + 2",
        "(1 + 2) * 3",
        "1 < 2 && 3 > 4",
        "1 < 2 == 3 > 4",
        "true && false || true",
    ] {
        assert_compiles(src);
    }
}

// ---------- タグ参照 ----------

#[test]
fn tag_ref_with_hyphen_and_underscore_segments_compiles_and_is_referenced() {
    let compiled = compile("plc-1.grp_a.tag-1_b + 1").unwrap();
    assert_eq!(compiled.referenced_tags(), &["plc-1.grp_a.tag-1_b"]);
}

#[test]
fn referenced_tags_deduplicated_preserving_first_occurrence() {
    let compiled = compile("a.b.c + a.b.c + d.e.f").unwrap();
    assert_eq!(compiled.referenced_tags(), &["a.b.c", "d.e.f"]);
}

#[test]
fn referenced_tags_collected_from_nested_calls_and_bit() {
    let compiled = compile("if(bit(calc.x.y, 3), min(a.b.c, 1), d.e.f)").unwrap();
    let refs = compiled.referenced_tags();
    assert!(refs.contains(&"calc.x.y".to_string()));
    assert!(refs.contains(&"a.b.c".to_string()));
    assert!(refs.contains(&"d.e.f".to_string()));
    assert_eq!(refs.len(), 3);
}

#[test]
fn tag_ref_two_segments_rejected() {
    assert!(matches!(
        assert_rejected("a.b"),
        CompileError::Syntax { .. }
    ));
}

#[test]
fn tag_ref_four_segments_rejected() {
    assert!(matches!(
        assert_rejected("a.b.c.d"),
        CompileError::Syntax { .. }
    ));
}

// ---------- 型検査: 受理・拒否ペア ----------

#[test]
fn arithmetic_requires_num_both_sides() {
    assert_compiles("1 + 2");
    assert!(matches!(
        assert_rejected("1 + true"),
        CompileError::TypeMismatch { .. }
    ));
    assert!(matches!(
        assert_rejected("true + 1"),
        CompileError::TypeMismatch { .. }
    ));
    assert!(matches!(
        assert_rejected("true + false"),
        CompileError::TypeMismatch { .. }
    ));
}

#[test]
fn unary_neg_requires_num() {
    assert_compiles("-1");
    assert!(matches!(
        assert_rejected("-true"),
        CompileError::TypeMismatch { .. }
    ));
}

#[test]
fn unary_not_requires_bool() {
    assert_compiles("!true");
    assert!(matches!(
        assert_rejected("!1"),
        CompileError::TypeMismatch { .. }
    ));
}

#[test]
fn relational_comparison_requires_num_both_sides() {
    assert_compiles("1 < 2");
    for op in ["<", ">", "<=", ">="] {
        let src = format!("true {op} false");
        assert!(
            matches!(assert_rejected(&src), CompileError::TypeMismatch { .. }),
            "expected rejection for {src:?}"
        );
    }
}

#[test]
fn equality_requires_same_type_both_sides() {
    assert_compiles("1 == 2");
    assert_compiles("true == false");
    assert!(matches!(
        assert_rejected("1 == true"),
        CompileError::TypeMismatch { .. }
    ));
    assert!(matches!(
        assert_rejected("true != 1"),
        CompileError::TypeMismatch { .. }
    ));
}

#[test]
fn logical_and_or_require_bool_both_sides() {
    assert_compiles("true && false");
    assert_compiles("true || false");
    assert!(matches!(
        assert_rejected("true && 1"),
        CompileError::TypeMismatch { .. }
    ));
    assert!(matches!(
        assert_rejected("1 || true"),
        CompileError::TypeMismatch { .. }
    ));
    assert!(matches!(
        assert_rejected("1 && 2"),
        CompileError::TypeMismatch { .. }
    ));
}

#[test]
fn if_condition_must_be_bool() {
    assert_compiles("if(true, 1, 2)");
    assert!(matches!(
        assert_rejected("if(1, 2, 3)"),
        CompileError::TypeMismatch { .. }
    ));
}

#[test]
fn if_branches_must_share_the_same_type() {
    assert_compiles("if(true, 1, 2)");
    assert_compiles("if(true, true, false)");
    assert!(matches!(
        assert_rejected("if(true, 1, false)"),
        CompileError::TypeMismatch { .. }
    ));
}

#[test]
fn min_max_require_num_args() {
    assert_compiles("min(1, 2)");
    assert_compiles("max(1, 2)");
    assert!(matches!(
        assert_rejected("min(1, true)"),
        CompileError::TypeMismatch { .. }
    ));
    assert!(matches!(
        assert_rejected("max(true, 1)"),
        CompileError::TypeMismatch { .. }
    ));
}

#[test]
fn abs_round_require_num_arg() {
    assert_compiles("abs(-1)");
    assert_compiles("round(1.5)");
    assert!(matches!(
        assert_rejected("abs(true)"),
        CompileError::TypeMismatch { .. }
    ));
    assert!(matches!(
        assert_rejected("round(false)"),
        CompileError::TypeMismatch { .. }
    ));
}

#[test]
fn clamp_requires_three_num_args() {
    assert_compiles("clamp(1, 0, 10)");
    assert!(matches!(
        assert_rejected("clamp(1, true, 10)"),
        CompileError::TypeMismatch { .. }
    ));
    assert!(matches!(
        assert_rejected("clamp(true, 0, 10)"),
        CompileError::TypeMismatch { .. }
    ));
}

#[test]
fn bit_target_must_be_tag_ref_not_intermediate_expression() {
    assert_compiles("bit(calc.a.b, 0)");
    assert!(matches!(
        assert_rejected("bit(1 + 2, 0)"),
        CompileError::BadBitTarget { .. }
    ));
    assert!(matches!(
        assert_rejected("bit(true, 0)"),
        CompileError::BadBitTarget { .. }
    ));
}

#[test]
fn bit_index_must_be_integer_literal_in_0_to_15() {
    assert_compiles("bit(calc.a.b, 0)");
    assert_compiles("bit(calc.a.b, 15)");
    assert!(matches!(
        assert_rejected("bit(calc.a.b, 16)"),
        CompileError::BadBitIndex { .. }
    ));
    assert!(matches!(
        assert_rejected("bit(calc.a.b, -1)"),
        CompileError::BadBitIndex { .. }
    ));
    assert!(matches!(
        assert_rejected("bit(calc.a.b, 1.5)"),
        CompileError::BadBitIndex { .. }
    ));
    assert!(matches!(
        assert_rejected("bit(calc.a.b, 1 + 2)"),
        CompileError::BadBitIndex { .. }
    ));
    assert!(matches!(
        assert_rejected("bit(calc.a.b, calc.c.d)"),
        CompileError::BadBitIndex { .. }
    ));
}

// ---------- 未知の関数・引数個数 ----------

#[test]
fn unknown_function_rejected() {
    assert!(matches!(
        assert_rejected("foo(1, 2)"),
        CompileError::UnknownFunction { .. }
    ));
}

#[test]
fn wrong_arity_rejected_for_every_known_function() {
    assert!(matches!(
        assert_rejected("if(true, 1)"),
        CompileError::ArityMismatch { .. }
    ));
    assert!(matches!(
        assert_rejected("if(true, 1, 2, 3)"),
        CompileError::ArityMismatch { .. }
    ));
    assert!(matches!(
        assert_rejected("min(1)"),
        CompileError::ArityMismatch { .. }
    ));
    assert!(matches!(
        assert_rejected("min(1, 2, 3)"),
        CompileError::ArityMismatch { .. }
    ));
    assert!(matches!(
        assert_rejected("max(1, 2, 3)"),
        CompileError::ArityMismatch { .. }
    ));
    assert!(matches!(
        assert_rejected("abs(1, 2)"),
        CompileError::ArityMismatch { .. }
    ));
    assert!(matches!(
        assert_rejected("round()"),
        CompileError::ArityMismatch { .. }
    ));
    assert!(matches!(
        assert_rejected("clamp(1, 2)"),
        CompileError::ArityMismatch { .. }
    ));
    assert!(matches!(
        assert_rejected("bit(calc.a.b)"),
        CompileError::ArityMismatch { .. }
    ));
    assert!(matches!(
        assert_rejected("bit(calc.a.b, 1, 2)"),
        CompileError::ArityMismatch { .. }
    ));
}

// ---------- 文法の閉性: 拒否すべき構文 ----------

#[test]
fn string_literal_rejected() {
    assert!(compile("\"hello\"").is_err());
    assert!(compile("1 + \"x\"").is_err());
}

#[test]
fn assignment_rejected() {
    assert!(compile("a.b.c = 1").is_err());
}

#[test]
fn semicolon_rejected() {
    assert!(compile("1; 2").is_err());
}

#[test]
fn variadic_call_rejected_min_with_three_args() {
    assert!(matches!(
        assert_rejected("min(1, 2, 3)"),
        CompileError::ArityMismatch { .. }
    ));
}

#[test]
fn bitwise_and_or_rejected() {
    assert!(compile("1 & 2").is_err());
    assert!(compile("1 | 2").is_err());
}

#[test]
fn user_defined_function_like_syntax_is_just_unknown_function() {
    assert!(matches!(
        assert_rejected("myFunc(a.b.c)"),
        CompileError::UnknownFunction { .. }
    ));
}

// ---------- エラー位置 ----------

#[test]
fn syntax_error_position_points_at_offending_token() {
    match assert_rejected("1 + ") {
        CompileError::Syntax { pos, .. } => assert_eq!(pos, 4),
        other => panic!("expected Syntax, got {other:?}"),
    }
}

#[test]
fn type_mismatch_error_position_points_at_binary_expression_start() {
    match assert_rejected("1 + true") {
        CompileError::TypeMismatch { pos, .. } => assert_eq!(pos, 0),
        other => panic!("expected TypeMismatch, got {other:?}"),
    }
}

#[test]
fn unknown_function_error_position_points_at_call() {
    match assert_rejected("1 + foo(1)") {
        CompileError::UnknownFunction { pos, .. } => assert_eq!(pos, 4),
        other => panic!("expected UnknownFunction, got {other:?}"),
    }
}

// ---------- H1: 式の文字数上限・パーサの再帰深さ上限（DoS 対策） ----------
//
// banto-expr::compile は演算タグの登録時に呼ばれる。深いネスト/連鎖式で
// tokio ワーカースレッド（既定スタック 2MiB）がスタックオーバーフロー
// = banto-hub プロセス全体の abort になるのを防ぐガード
// （crate トップレベル doc の「DoS 対策」節参照）。ここでは深さ系のテストは
// すべて MAX_SOURCE_CHARS（1024文字）に引っかからない短い入力を使い、
// 「ネストが原因で TooDeep になる」ことだけを確かめる。文字数上限自体は
// 別セクションで検証する。

fn nested_parens(n: usize) -> String {
    format!("{}1{}", "(".repeat(n), ")".repeat(n))
}

#[test]
fn deeply_nested_parens_rejected_as_too_deep_not_crashed() {
    // 100重括弧（201文字、MAX_SOURCE_CHARS の範囲内）- MAX_NESTING_DEPTH
    // （64）を確実に超え、スタックオーバーフローではなく TooDeep で拒否
    // される。
    assert!(matches!(
        assert_rejected(&nested_parens(100)),
        CompileError::TooDeep { .. }
    ));
}

#[test]
fn nesting_exactly_at_limit_compiles_one_past_is_too_deep() {
    // 境界値: 63重括弧は compile 成功、64重括弧は TooDeep（実装の深さ
    // 会計は「トップレベルの式再入口で1」+「括弧1重につき+1」なので、
    // MAX_NESTING_DEPTH=64 のとき63重が上限内・64重が超過になる - 詳細は
    // crates/banto-expr/src/parser.rs の `descend` / モジュール doc 参照）。
    assert_compiles(&nested_parens(63));
    assert!(matches!(
        assert_rejected(&nested_parens(64)),
        CompileError::TooDeep { .. }
    ));
}

#[test]
fn deeply_chained_unary_minus_rejected_as_too_deep() {
    let src = format!("{}1", "-".repeat(100));
    assert!(matches!(
        assert_rejected(&src),
        CompileError::TooDeep { .. }
    ));
}

#[test]
fn deeply_chained_unary_not_rejected_as_too_deep_before_typecheck_runs() {
    // 100個の '!' を bool に重ねる - 型としては（偶数個なら）Bool のまま
    // 矛盾しないはずだが、型検査に到達する前にパース段階で TooDeep として
    // 拒否されることを確認する。
    let src = format!("{}true", "!".repeat(100));
    assert!(matches!(
        assert_rejected(&src),
        CompileError::TooDeep { .. }
    ));
}

#[test]
fn deeply_nested_function_calls_rejected_as_too_deep() {
    let src = format!("{}1{}", "min(".repeat(100), ")".repeat(100));
    assert!(matches!(
        assert_rejected(&src),
        CompileError::TooDeep { .. }
    ));
}

/// 空白は字句解析で読み飛ばされるので、先頭を空白で埋めた `"1"` は文字数
/// だけを正確に `total_len` に保ったまま常に有効な式になる。
fn padded_expr_of_len(total_len: usize) -> String {
    assert!(total_len >= 1);
    let mut s = " ".repeat(total_len - 1);
    s.push('1');
    s
}

#[test]
fn source_length_exactly_at_max_compiles() {
    let src = padded_expr_of_len(banto_expr::MAX_SOURCE_CHARS);
    assert_eq!(src.chars().count(), banto_expr::MAX_SOURCE_CHARS);
    assert_compiles(&src);
}

#[test]
fn source_length_one_over_max_is_rejected() {
    let src = padded_expr_of_len(banto_expr::MAX_SOURCE_CHARS + 1);
    match assert_rejected(&src) {
        CompileError::SourceTooLong { max, actual } => {
            assert_eq!(max, banto_expr::MAX_SOURCE_CHARS);
            assert_eq!(actual, banto_expr::MAX_SOURCE_CHARS + 1);
        }
        other => panic!("expected SourceTooLong, got {other:?}"),
    }
}
