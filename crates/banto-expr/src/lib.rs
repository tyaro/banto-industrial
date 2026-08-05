//! banto-expr: T6-1 演算タグの式評価エンジン
//! (docs/tag-server-design.md §4.2「演算タグ・内部タグ」、§10-12
//! 「式文法の確定」2026-08-05)。
//!
//! ## このクレートが存在する理由
//!
//! 演算タグ（`tag_kind = "computed"`）はタグサーバー側で一元実装する
//! （§4.2 冒頭）: 同じ演算を各クライアントアプリ（ChronoGazer /
//! relay-wright / SCADA 予定）がそれぞれ持つと、式のバージョン差で
//! 値が食い違う事故が起きる。タグ空間で1回計算すれば、WS/MQTT/gRPC の
//! どの経路から見ても同一の値・品質・時刻になる。このクレート自体は
//! そのうち「式1本を解釈して値1個を返す」計算部分だけを担い、タグ空間・
//! DB・評価ループへの配線は T6-2 が行う（本クレートはレジストリにも
//! tokio にも依存しない - 依存は `thiserror` のみ）。
//!
//! ## 文法は意図的に閉じている（拡張しない）
//!
//! §4.2 は FA-Server（ロボスクリプト SC1/SC2/演算式構文）との比較調査を
//! 踏まえ、**SC2 型のフル言語（if/for・ユーザー定義関数を持つ）への
//! 滑り坂を意図的に断つ**と決めている: ループなし・ユーザー定義関数
//! なし・代入なし。これはアクション化（外部プロセス実行・SQL・メール等の
//! 副作用）を許さないための境界線であり、表現力の不足ではなく安全設計
//! そのもの。式に必要な機能が増えたときの拡張手段は「組み込み関数の
//! 追加」（`avg` 等）に限定し、制御構文は入れない（§4.2 末尾）。
//! 本クレートの実装もこの制約をそのまま体現する: パーサは自前の再帰下降
//! （外部式評価クレート禁止、§4.2「自前の小さな AST + 純関数評価器」）、
//! 評価器は副作用なし・外部 I/O なしの純関数（`eval` は `&dyn Fn(&str) ->
//! Option<Value>` を受け取るだけで、タグ空間にもファイルにもネットワーク
//! にも触れない）。文法が閉じていることは同時に**監査可能性**でもある:
//! 演算タグの式は静的に列挙可能な演算子・関数の組み合わせに限られるため、
//! 「この式が何をするか」はソースを読むだけで完全に判定できる。
//!
//! ## 型システム: なぜ2種類だけで、暗黙変換がないか
//!
//! 型は [`Type::Num`]（f64）と [`Type::Bool`] の2種のみ。文字列型は
//! 存在しない（文字列タグの参照拒否は T6-2 の登録時検証の責務 - 詳細は
//! [`typecheck`] モジュール doc）。SC2 は `"1" + 2` が数値になる、`&` が
//! 左辺文字列なら文字列結合になる、ブール値が四則で 1/0 として扱われる
//! 等の文脈依存の暗黙変換を多用するが、本設計はこれを踏襲しない
//! （§4.2「暗黙型変換は踏襲しない」）- I1 がタグごとの型情報を持つため、
//! 曖昧な暗黙変換に頼らず**登録時に型検査**できる。型不一致は
//! [`CompileError::TypeMismatch`] として登録時（= `compile` 呼び出し時）
//! に拒否され、実行時（`eval`）まで先送りされない。
//!
//! ## NaN・ゼロ除算・丸め・`bit()` の挙動（実装判断）
//!
//! - **NaN・±∞・ゼロ除算は IEEE 754 のままエラーにせず伝播させる**
//!   （[`eval`] モジュール doc に詳細）。品質（Bad/Stale/Good）は入力タグ
//!   の品質から決まるべきで、エンジンが「怪しい数値」を独自に Err 化する
//!   と品質管理の責務がここへ漏れ出すため。
//! - **`round()` は half-away-from-zero**（`f64::round` そのもの。例:
//!   `round(2.5) == 3.0`、`round(-2.5) == -3.0`）。
//! - **`bit(tag, n)` の対象値**（NaN・非整数・極端に大きい/小さい値を
//!   含む）は Rust の `f64 as i64` キャスト（NaN→0、範囲外は飽和）に
//!   そのまま従わせ、下位16ビットを2の補数として読む（詳細は [`eval`]
//!   モジュール doc）。`n` 自体は 0〜15 の整数リテラルのみ許可し、それ
//!   以外（式・タグ参照・小数・範囲外）は登録時に [`CompileError`] で
//!   拒否する（[`typecheck`] モジュール doc）。
//!
//! ## タグ参照は常に `Num` 型（このクレート単体で完結する型検査の鍵）
//!
//! 詳細は [`typecheck`] モジュール doc 参照。要点だけ: I1 のデータ型
//! 一覧に Bool は存在せず（`bit` データ型も 0/1 の数値）、レジストリを
//! 持たない本クレートでも「タグ参照の型は常に Num」という前提だけで
//! 型検査が完結する。文字列タグの参照そのものを拒否する検証（レジストリ
//! 照会が要る）は T6-2 の責務。
//!
//! ## DAG 検証
//!
//! [`validate_dag`] は演算タグ同士の依存関係（自身も演算タグを参照
//! しうる、§4.2）から循環を検出する。詳細・トポロジカル順の作り方は
//! [`mod@dag`] モジュール doc 参照。
//!
//! ## モジュール map
//!
//! - [`lexer`][]: 字句解析（ASCII 前提・識別子とハイフンの曖昧性解消）。
//! - [`ast`][]: 構文木。
//! - [`parser`][]: 再帰下降パーサ（優先順位表つき）。
//! - [`typecheck`][]: 型検査（登録時エラーの生成元）。
//! - [`eval`][]: 評価器（純関数）。
//! - [`dag`][]: DAG 検証・トポロジカル順。
//! - [`types`][]: 型システム（[`Type`]・[`Value`]）。
//! - [`error`][]: エラー型（[`CompileError`]・[`EvalError`]・[`CycleError`]）。

mod ast;
mod dag;
mod error;
mod eval;
mod lexer;
mod parser;
mod typecheck;
mod types;

pub use dag::validate_dag;
pub use error::{CompileError, CycleError, EvalError};
pub use types::{Type, Value};

/// パース + 型検査済みの式。`compile` の唯一の戻り値であり、これ以降の
/// 評価（[`CompiledExpr::eval`]）は失敗しうるが構文・型の再検証は行わない
/// （すでに済んでいるため）。
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledExpr {
    ast: ast::Expr,
    referenced_tags: Vec<String>,
    result_type: Type,
}

impl CompiledExpr {
    /// この式が参照する外部名の一覧（出現順、重複なし）。存在確認・型
    /// 適合確認（文字列タグでないか等）は呼び出し側（T6-2）の責務 -
    /// このクレートはレジストリを持たないため、ここでは「式のテキストに
    /// 何が書かれているか」だけを返す。
    pub fn referenced_tags(&self) -> &[String] {
        &self.referenced_tags
    }

    /// この式の結果型。
    pub fn result_type(&self) -> Type {
        self.result_type
    }

    /// 評価する。`inputs` は外部名から現在値を引く関数 - 副作用があっては
    /// ならない（毎回同じ引数に対して同じ値を返す想定。呼び出し側が
    /// 用意するタグ空間スナップショットのクロージャを想定している）。
    /// 参照タグが `inputs` に無ければ [`EvalError::MissingTag`]。
    pub fn eval(&self, inputs: &dyn Fn(&str) -> Option<Value>) -> Result<Value, EvalError> {
        eval::eval(&self.ast, inputs)
    }
}

/// 式をパースし型検査する（= 演算タグの登録時検証そのもの）。
///
/// 参照するタグが実在するか・型が合うか（文字列タグでないか）は検証
/// **しない** - レジストリを持たないこのクレートには判定できない
/// （呼び出し側 T6-2 の責務、`crate` トップレベル doc 参照）。ここで
/// 検証するのは構文・演算子/関数の型規則・`bit()` の特殊制約だけ。
pub fn compile(source: &str) -> Result<CompiledExpr, CompileError> {
    let tokens = lexer::tokenize(source)?;
    let ast = parser::parse(&tokens)?;
    let mut refs = Vec::new();
    let result_type = typecheck::check(&ast, &mut refs)?;

    let mut seen = std::collections::HashSet::new();
    let referenced_tags: Vec<String> = refs
        .into_iter()
        .filter(|t| seen.insert(t.clone()))
        .collect();

    Ok(CompiledExpr {
        ast,
        referenced_tags,
        result_type,
    })
}
