//! 評価の網羅テスト: 演算子・関数の網羅、NaN/ゼロ除算/極値、`bit()` の
//! 全ビット位置、入力欠損。

use banto_expr::{compile, EvalError, Value};
use std::collections::HashMap;

fn eval_with(source: &str, tags: &[(&str, Value)]) -> Value {
    let compiled = compile(source).unwrap_or_else(|e| panic!("compile({source:?}) failed: {e:?}"));
    let map: HashMap<String, Value> = tags.iter().map(|(k, v)| (k.to_string(), *v)).collect();
    compiled
        .eval(&|name| map.get(name).copied())
        .unwrap_or_else(|e| panic!("eval({source:?}) failed: {e:?}"))
}

fn eval_num(source: &str, tags: &[(&str, Value)]) -> f64 {
    match eval_with(source, tags) {
        Value::Num(n) => n,
        other => panic!("expected Num, got {other:?} for {source:?}"),
    }
}

fn eval_bool(source: &str, tags: &[(&str, Value)]) -> bool {
    match eval_with(source, tags) {
        Value::Bool(b) => b,
        other => panic!("expected Bool, got {other:?} for {source:?}"),
    }
}

// ---------- 四則演算 ----------

#[test]
fn arithmetic_operators() {
    assert_eq!(eval_num("2 + 3", &[]), 5.0);
    assert_eq!(eval_num("2 - 3", &[]), -1.0);
    assert_eq!(eval_num("2 * 3", &[]), 6.0);
    assert_eq!(eval_num("6 / 3", &[]), 2.0);
    assert_eq!(eval_num("-5", &[]), -5.0);
    assert_eq!(eval_num("- -5", &[]), 5.0);
}

// ---------- 比較演算 ----------

#[test]
fn comparison_operators() {
    assert!(eval_bool("1 < 2", &[]));
    assert!(!eval_bool("2 < 1", &[]));
    assert!(eval_bool("2 > 1", &[]));
    assert!(eval_bool("1 <= 1", &[]));
    assert!(eval_bool("1 >= 1", &[]));
    assert!(eval_bool("1 == 1", &[]));
    assert!(!eval_bool("1 == 2", &[]));
    assert!(eval_bool("1 != 2", &[]));
    assert!(eval_bool("true == true", &[]));
    assert!(eval_bool("true != false", &[]));
}

// ---------- 論理演算 ----------

#[test]
fn logical_operators() {
    assert!(eval_bool("true && true", &[]));
    assert!(!eval_bool("true && false", &[]));
    assert!(eval_bool("false || true", &[]));
    assert!(!eval_bool("false || false", &[]));
    assert!(!eval_bool("!true", &[]));
    assert!(eval_bool("!false", &[]));
}

// ---------- 関数 ----------

#[test]
fn if_function() {
    assert_eq!(eval_num("if(true, 1, 2)", &[]), 1.0);
    assert_eq!(eval_num("if(false, 1, 2)", &[]), 2.0);
    assert!(eval_bool("if(1 < 2, true, false)", &[]));
}

#[test]
fn min_max_functions() {
    assert_eq!(eval_num("min(1, 2)", &[]), 1.0);
    assert_eq!(eval_num("min(2, 1)", &[]), 1.0);
    assert_eq!(eval_num("max(1, 2)", &[]), 2.0);
    assert_eq!(eval_num("max(2, 1)", &[]), 2.0);
    assert_eq!(eval_num("min(-1, -2)", &[]), -2.0);
}

#[test]
fn abs_function() {
    assert_eq!(eval_num("abs(-5)", &[]), 5.0);
    assert_eq!(eval_num("abs(5)", &[]), 5.0);
    assert_eq!(eval_num("abs(0)", &[]), 0.0);
}

#[test]
fn round_function_half_away_from_zero() {
    assert_eq!(eval_num("round(2.5)", &[]), 3.0);
    assert_eq!(eval_num("round(-2.5)", &[]), -3.0);
    assert_eq!(eval_num("round(2.4)", &[]), 2.0);
    assert_eq!(eval_num("round(2.6)", &[]), 3.0);
    assert_eq!(eval_num("round(0.5)", &[]), 1.0);
    assert_eq!(eval_num("round(-0.5)", &[]), -1.0);
}

#[test]
fn clamp_function() {
    assert_eq!(eval_num("clamp(5, 0, 10)", &[]), 5.0);
    assert_eq!(eval_num("clamp(-5, 0, 10)", &[]), 0.0);
    assert_eq!(eval_num("clamp(15, 0, 10)", &[]), 10.0);
    assert_eq!(eval_num("clamp(5, 0, 0)", &[]), 0.0);
}

#[test]
fn clamp_with_inverted_bounds_is_deterministic_not_an_error() {
    // lo > hi のときの決定論的挙動（doc 参照）: x.max(lo).min(hi) は
    // 常に hi に落ちる。
    assert_eq!(eval_num("clamp(5, 10, 0)", &[]), 0.0);
    assert_eq!(eval_num("clamp(-100, 10, 0)", &[]), 0.0);
    assert_eq!(eval_num("clamp(100, 10, 0)", &[]), 0.0);
}

// ---------- NaN・ゼロ除算・極値 ----------

#[test]
fn division_by_zero_propagates_ieee754_infinity_not_error() {
    assert_eq!(eval_num("1 / 0", &[]), f64::INFINITY);
    assert_eq!(eval_num("-1 / 0", &[]), f64::NEG_INFINITY);
}

#[test]
fn zero_over_zero_is_nan_not_error() {
    assert!(eval_num("0 / 0", &[]).is_nan());
}

#[test]
fn nan_propagates_through_arithmetic() {
    assert!(eval_num("(0 / 0) + 1", &[]).is_nan());
    assert!(eval_num("1 * (0 / 0)", &[]).is_nan());
}

#[test]
fn nan_comparisons_are_all_false_per_ieee754() {
    let tags = [("calc.x.y", Value::Num(f64::NAN))];
    assert!(!eval_bool("calc.x.y < 1", &tags));
    assert!(!eval_bool("calc.x.y > 1", &tags));
    assert!(!eval_bool("calc.x.y == calc.x.y", &tags));
    assert!(eval_bool("calc.x.y != calc.x.y", &tags));
}

#[test]
fn extreme_values_do_not_error() {
    // 本文法は指数表記("1e300")を持たない（整数・小数表記のみ、doc 参照）
    // ため、桁数そのもので極値を作る。
    let huge = "9".repeat(320); // > f64::MAX (~1.8e308) 単体で overflow する
    assert!(
        eval_num(&huge, &[]).is_infinite(),
        "overflow should saturate to infinity, not error"
    );

    let tiny = format!("0.{}1", "0".repeat(400)); // < f64 の最小非正規化数（約4.9e-324）
    assert_eq!(eval_num(&tiny, &[]), 0.0);

    assert!(eval_num(&format!("{huge} * {huge}"), &[]).is_infinite());
}

// ---------- bit() ----------

#[test]
fn bit_extracts_every_position_0_to_15() {
    // word = 0b1010_1010_1010_1010 = 0xAAAA -> even bits 0, odd bits 1
    let tags = [("calc.w.ord", Value::Num(0xAAAA as f64))];
    for n in 0..16 {
        let src = format!("bit(calc.w.ord, {n})");
        let expected = (n % 2) == 1;
        assert_eq!(
            eval_bool(&src, &tags),
            expected,
            "bit {n} of 0xAAAA should be {expected}"
        );
    }
}

#[test]
fn bit_all_ones_word() {
    let tags = [("calc.w.ord", Value::Num(0xFFFF as f64))];
    for n in 0..16 {
        assert!(eval_bool(&format!("bit(calc.w.ord, {n})"), &tags));
    }
}

#[test]
fn bit_all_zero_word() {
    let tags = [("calc.w.ord", Value::Num(0.0))];
    for n in 0..16 {
        assert!(!eval_bool(&format!("bit(calc.w.ord, {n})"), &tags));
    }
}

#[test]
fn bit_on_negative_value_uses_twos_complement() {
    // -1 as i64 -> 下位16ビットは全部1。
    let tags = [("calc.w.ord", Value::Num(-1.0))];
    for n in 0..16 {
        assert!(eval_bool(&format!("bit(calc.w.ord, {n})"), &tags));
    }
    // -2 -> ...11111110 なのでビット0だけ0。
    let tags = [("calc.w.ord", Value::Num(-2.0))];
    assert!(!eval_bool("bit(calc.w.ord, 0)", &tags));
    assert!(eval_bool("bit(calc.w.ord, 1)", &tags));
}

#[test]
fn bit_on_nan_target_treated_as_zero_word() {
    let tags = [("calc.w.ord", Value::Num(f64::NAN))];
    for n in 0..16 {
        assert!(
            !eval_bool(&format!("bit(calc.w.ord, {n})"), &tags),
            "NaN word should read as all-zero bits (bit {n})"
        );
    }
}

#[test]
fn bit_on_infinite_target_saturates_rather_than_erroring() {
    let tags = [("calc.w.ord", Value::Num(f64::INFINITY))];
    // as i64 saturates to i64::MAX (0x7FFF...FF) -> 下位16ビットは全部1。
    for n in 0..16 {
        assert!(eval_bool(&format!("bit(calc.w.ord, {n})"), &tags));
    }
    let tags = [("calc.w.ord", Value::Num(f64::NEG_INFINITY))];
    // as i64 saturates to i64::MIN (0x8000...00) -> 下位16ビットは全部0。
    for n in 0..16 {
        assert!(!eval_bool(&format!("bit(calc.w.ord, {n})"), &tags));
    }
}

#[test]
fn bit_on_non_integer_target_truncates_toward_zero() {
    // 5.9 -> 5 (0b101): bit0=1, bit1=0, bit2=1
    let tags = [("calc.w.ord", Value::Num(5.9))];
    assert!(eval_bool("bit(calc.w.ord, 0)", &tags));
    assert!(!eval_bool("bit(calc.w.ord, 1)", &tags));
    assert!(eval_bool("bit(calc.w.ord, 2)", &tags));
}

// ---------- 入力欠損 ----------

#[test]
fn missing_tag_input_is_eval_error() {
    let compiled = compile("calc.a.b + 1").unwrap();
    let err = compiled.eval(&|_| None).unwrap_err();
    assert_eq!(err, EvalError::MissingTag("calc.a.b".to_string()));
}

#[test]
fn missing_one_of_several_tags_still_errors() {
    let compiled = compile("calc.a.b + calc.c.d").unwrap();
    let err = compiled
        .eval(&|name| {
            if name == "calc.a.b" {
                Some(Value::Num(1.0))
            } else {
                None
            }
        })
        .unwrap_err();
    assert_eq!(err, EvalError::MissingTag("calc.c.d".to_string()));
}

#[test]
fn wrong_value_type_from_inputs_is_eval_error_not_panic() {
    // タグ参照は常に Num 扱いだが、呼び出し側の契約違反（Bool を返す）を
    // panic せずエラーとして報告できることを確認する。
    let compiled = compile("calc.a.b + 1").unwrap();
    let err = compiled.eval(&|_| Some(Value::Bool(true))).unwrap_err();
    assert_eq!(
        err,
        EvalError::UnexpectedValueType {
            tag: "calc.a.b".to_string()
        }
    );
}

// ---------- タグ参照を伴う一般式 ----------

#[test]
fn expression_with_tag_references_evaluates_using_inputs_closure() {
    let tags = [
        ("plc1.grp1.temp", Value::Num(20.0)),
        ("plc1.grp1.setpoint", Value::Num(25.0)),
    ];
    assert!(eval_bool("plc1.grp1.temp < plc1.grp1.setpoint", &tags));
    assert_eq!(
        eval_num("(plc1.grp1.temp + plc1.grp1.setpoint) / 2", &tags),
        22.5
    );
}

#[test]
fn eval_is_pure_same_inputs_same_output() {
    let compiled = compile("calc.a.b * 2 + 1").unwrap();
    let inputs = |name: &str| -> Option<Value> { (name == "calc.a.b").then_some(Value::Num(3.0)) };
    let r1 = compiled.eval(&inputs).unwrap();
    let r2 = compiled.eval(&inputs).unwrap();
    assert_eq!(r1, r2);
}
