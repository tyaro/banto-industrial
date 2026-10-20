//! 演算タグの式チェック API（#342 段階A: `POST /api/tags/expression/check`、保存前検証・
//! エラー位置・プレビュー）と、組み込み関数表の配布（#342 段階B:
//! `GET /api/tags/expression/functions`、式欄のセグメント補完用）。
//!
//! `rest.rs` から中身を変えずに移したもの（docs/banto-hub-driver-refactor-plan.md §8.1、
//! P0-1 の手順 8）。ルーターへの組み込み（`tag_registry_router`）は `rest/tags.rs`、
//! 認証・ガード・補助は親の `rest.rs` にある。どちらも `use super::{...}` で使う。
//! 外から使われるパス（`crate::rest::X`）は親の re-export で保つ。

use super::{
    require_editor, ApiError, CollectorManager, ComputedEngine, Deserialize, HeaderMap, Json,
    Quality, Serialize, State, TagMap, TagRegistryState, COMPUTED_TAG_KIND,
};

// --- #342 段階A: 演算タグの式チェック API（保存前検証・エラー位置・プレビュー）
// -----------------------------------------------------------------------
//
// issue #342 は「`banto_expr::typecheck::check` をそのまま呼び、参照タグの
// 型は現行 catalog から解決する」前提で書かれていたが、これはコード事実と
// 食い違う: `typecheck` は `banto-expr` の非公開モジュール（`lib.rs` の
// `mod typecheck;`）で、公開 API は `compile(source) -> Result<CompiledExpr,
// CompileError>` だけ。しかも `banto-expr` はレジストリを持たない設計で、
// タグ参照の型は常に `Type::Num` 固定（`crates/banto-expr/src/lib.rs` 冒頭
// doc「タグ参照は常に Num 型」参照）なので「参照タグの型解決を渡す」引数
// 自体が存在しない。そのためここでは `compile()` を呼び、
// `CompiledExpr::referenced_tags()` を `TagMap` と突き合わせる**2段構成**に
// する - `crate::computed::build_plan` が既にこの形（`resolve_referenced_tag`
// を両者で共有 - 判定条件を新しく発明しない）。`build_plan` 自体は呼ばない:
// あれは登録済み computed タグ全件の rebuild 検証用で、単発式のプレビューには
// 過剰（未登録の全タグを毎回コンパイルし直す）なうえ `String` エラーしか
// 返さない。

/// `POST /api/tags/expression/check` の body。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ExpressionCheckRequest {
    expression: String,
    /// この式を保存する予定のタグの完全名（省略可）。循環参照の判定にだけ
    /// 使う（[`cycle_check_nodes`] 参照）。
    #[serde(default)]
    external_name: Option<String>,
}

/// 参照タグ1件の要約（コンパイル成功後、存在確認・文字列タグ拒否を通った
/// ものだけが入る）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExpressionRefEntry {
    name: String,
    data_type: String,
    unit: Option<String>,
    tag_kind: String,
}

/// 現在値による試算結果 - `evaluated: false` のときは `value: null` で
/// `reason` に日本語の理由（参照が Bad/Stale・`eval` 失敗のいずれか）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExpressionPreview {
    value: Option<f64>,
    evaluated: bool,
    reason: Option<String>,
}

/// `banto_expr::CompileError` の全 variant 名を写した `kind` 文字列 - `pos`
/// は `CompileError::SourceTooLong` だけ持たない（バイトオフセット =
/// 文字オフセット、`crates/banto-expr/src/error.rs` 冒頭のコメント参照）。
/// `message` は `CompileError`/`CycleError` の `Display`（既に日本語）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExpressionCheckError {
    kind: String,
    pos: Option<usize>,
    message: String,
}

/// `POST /api/tags/expression/check` の応答。**常に 200** - 式が不正でも
/// `ok: false` で返す（保存 API ではなくプレビュー用のため、HTTP エラーに
/// しない。#342 実装指示「レスポンス」節）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ExpressionCheckResponse {
    ok: bool,
    result_type: Option<String>,
    refs: Vec<ExpressionRefEntry>,
    preview: ExpressionPreview,
    error: Option<ExpressionCheckError>,
}

impl ExpressionCheckResponse {
    /// 参照解決・循環検査で失敗したときの共通ショートカット - コンパイル
    /// 自体は成功しているので `result_type`/`refs`（そこまでに解決できた分）
    /// は埋めたまま返す（「ok: false のときは resultType/refs/preview を
    /// 可能な範囲で埋める」の実装）。
    fn resolution_error(
        result_type: Option<String>,
        refs: Vec<ExpressionRefEntry>,
        kind: &'static str,
        message: String,
    ) -> Self {
        Self {
            ok: false,
            result_type,
            refs,
            preview: ExpressionPreview {
                value: None,
                evaluated: false,
                reason: None,
            },
            error: Some(ExpressionCheckError {
                kind: kind.to_string(),
                pos: None,
                message,
            }),
        }
    }
}

fn expression_result_type_wire(ty: banto_expr::Type) -> String {
    match ty {
        banto_expr::Type::Num => "num".to_string(),
        banto_expr::Type::Bool => "bool".to_string(),
    }
}

/// `banto_expr::CompileError` → [`ExpressionCheckError`]。全 variant を
/// 網羅（`match` に `_` を置かない - 新しい variant が増えたらコンパイル
/// エラーで気づけるようにする）。
fn expression_compile_error(err: &banto_expr::CompileError) -> ExpressionCheckError {
    use banto_expr::CompileError;
    let message = err.to_string();
    let (kind, pos) = match err {
        CompileError::Syntax { pos, .. } => ("syntax", Some(*pos)),
        CompileError::TypeMismatch { pos, .. } => ("type_mismatch", Some(*pos)),
        CompileError::UnknownFunction { pos, .. } => ("unknown_function", Some(*pos)),
        CompileError::ArityMismatch { pos, .. } => ("arity_mismatch", Some(*pos)),
        CompileError::BadBitIndex { pos, .. } => ("bad_bit_index", Some(*pos)),
        CompileError::BadBitTarget { pos, .. } => ("bad_bit_target", Some(*pos)),
        // 唯一 `pos` を持たない variant - 式全体の長さについてのエラーで、
        // 特定の文字位置を指す意味がないため（`error.rs` の doc comment）。
        CompileError::SourceTooLong { .. } => ("source_too_long", None),
        CompileError::TooDeep { pos, .. } => ("too_deep", Some(*pos)),
    };
    ExpressionCheckError {
        kind: kind.to_string(),
        pos,
        message,
    }
}

/// 循環参照の判定に使うノード集合を組み立てる（#342 実装指示「循環参照の
/// 判定」節）。現在の `TagMap` の computed タグそれぞれについて、既に
/// commit 済みの [`ComputedEngine`] の plan から参照先一覧を引く
/// （`build_plan` を呼び直して全タグを再コンパイルしない - 既存タグは
/// 登録時に検証済みで、この plan がその正）。`external_name` のノードだけ
/// 今回の式の参照先（`new_refs`）で置き換える - 存在しなければ追加する。
fn cycle_check_nodes(
    map: &TagMap,
    computed: &ComputedEngine,
    external_name: &str,
    new_refs: &[String],
) -> Vec<(String, Vec<String>)> {
    let mut nodes: Vec<(String, Vec<String>)> = Vec::new();
    let mut replaced = false;
    for entry in map.iter() {
        if entry.tag_kind != COMPUTED_TAG_KIND {
            continue;
        }
        if entry.external_name == external_name {
            nodes.push((external_name.to_string(), new_refs.to_vec()));
            replaced = true;
        } else {
            let deps = computed
                .referenced_tags(&entry.external_name)
                .unwrap_or_default();
            nodes.push((entry.external_name.clone(), deps));
        }
    }
    if !replaced {
        nodes.push((external_name.to_string(), new_refs.to_vec()));
    }
    nodes
}

/// REST ハンドラ（[`tags_expression_check`]）と MCP ツール（`crate::mcp` の
/// `check_expression`）が共有する検証本体。認可・pending queue 判定は行わ
/// ない（呼び出し側の責務 - REST は `require_editor`、MCP は
/// `require_admin_scope`）。同期処理のみ（DB I/O も await もない）なので
/// 非同期にしていない - 呼び出し元が async 関数であっても素通しで呼べる。
pub(crate) fn evaluate_expression_check(
    manager: &CollectorManager,
    expression: &str,
    external_name: Option<&str>,
) -> ExpressionCheckResponse {
    let compiled = match banto_expr::compile(expression) {
        Ok(compiled) => compiled,
        Err(err) => {
            return ExpressionCheckResponse {
                ok: false,
                result_type: None,
                refs: Vec::new(),
                preview: ExpressionPreview {
                    value: None,
                    evaluated: false,
                    reason: None,
                },
                error: Some(expression_compile_error(&err)),
            };
        }
    };

    let result_type = Some(expression_result_type_wire(compiled.result_type()));
    let map = manager.tag_map();

    // 参照タグの解決（存在確認・文字列タグ拒否) - `build_plan` と同じ判定を
    // `crate::computed::resolve_referenced_tag` で共有する。
    let mut refs = Vec::with_capacity(compiled.referenced_tags().len());
    for name in compiled.referenced_tags() {
        match crate::computed::resolve_referenced_tag(&map, name) {
            Ok(entry) => refs.push(ExpressionRefEntry {
                name: entry.external_name.clone(),
                data_type: entry.data_type.clone(),
                unit: entry.unit.clone(),
                tag_kind: entry.tag_kind.clone(),
            }),
            Err(crate::computed::ReferencedTagError::Missing) => {
                return ExpressionCheckResponse::resolution_error(
                    result_type,
                    refs,
                    "unknown_tag",
                    format!("参照先タグが存在しません: {name}"),
                );
            }
            Err(crate::computed::ReferencedTagError::StringType) => {
                return ExpressionCheckResponse::resolution_error(
                    result_type,
                    refs,
                    "string_ref",
                    format!("文字列タグは参照できません: {name}"),
                );
            }
        }
    }

    // 循環参照の判定（externalName が与えられたときだけ）。
    if let Some(ext_name) = external_name {
        let computed = manager.computed_engine();
        let nodes = cycle_check_nodes(&map, &computed, ext_name, compiled.referenced_tags());
        if let Err(cycle_err) = banto_expr::validate_dag(&nodes) {
            return ExpressionCheckResponse::resolution_error(
                result_type,
                refs,
                "cycle",
                cycle_err.to_string(),
            );
        }
    }

    // プレビュー試算: 参照が全件 Good で値を持つときだけ評価する。
    let now_ms = manager.clock().now_ms();
    let current = manager.current_values();
    let server_store = manager.server_store();
    let mut inputs: Vec<(String, banto_expr::Value)> =
        Vec::with_capacity(compiled.referenced_tags().len());
    let mut blocked_reason: Option<String> = None;
    for name in compiled.referenced_tags() {
        // 上のループで解決済み（存在確認・文字列タグ拒否を通過済み）。
        let entry = map
            .get(name)
            .expect("#342: referenced tag resolved above via resolve_referenced_tag");
        let (value, quality, _t) =
            crate::hub::read_current(entry, current.as_ref(), &server_store, now_ms);
        if blocked_reason.is_some() {
            continue;
        }
        match (value, quality) {
            (Some(v), Quality::Good) => inputs.push((name.clone(), banto_expr::Value::Num(v))),
            (Some(_), Quality::Stale) => {
                blocked_reason = Some(format!("参照タグ {name} の値が Stale です"));
            }
            _ => {
                blocked_reason = Some(format!("参照タグ {name} の値が Bad です"));
            }
        }
    }

    let preview = match blocked_reason {
        Some(reason) => ExpressionPreview {
            value: None,
            evaluated: false,
            reason: Some(reason),
        },
        None => {
            let closure = |tag: &str| inputs.iter().find(|(n, _)| n == tag).map(|(_, v)| *v);
            match compiled.eval(&closure) {
                Ok(banto_expr::Value::Num(x)) => ExpressionPreview {
                    value: Some(x),
                    evaluated: true,
                    reason: None,
                },
                // 結果型 Bool は `computed.rs::evaluate_tick` と同じ規約で
                // 1.0/0.0 に変換する（I1 に bool データ型はない）。
                Ok(banto_expr::Value::Bool(b)) => ExpressionPreview {
                    value: Some(if b { 1.0 } else { 0.0 }),
                    evaluated: true,
                    reason: None,
                },
                Err(err) => ExpressionPreview {
                    value: None,
                    evaluated: false,
                    reason: Some(err.to_string()),
                },
            }
        }
    };

    ExpressionCheckResponse {
        ok: true,
        result_type,
        refs,
        preview,
        error: None,
    }
}

/// `plc_connections_test`と同じ理由で意図的に `#[utoipa::path]` を付けず
/// `ApiDoc` にも加えない（`tags_address_writable` の doc comment参照 -
/// `/api/v1/*` のみを文書化する既存方針。このエンドポイントも同じ
/// `tag_registry_router`＝管理系ルーター配下）。
pub(super) async fn tags_expression_check(
    State(state): State<TagRegistryState>,
    headers: HeaderMap,
    Json(input): Json<ExpressionCheckRequest>,
) -> Result<Json<ExpressionCheckResponse>, ApiError> {
    require_editor(
        &state.auth,
        &state.audit,
        &headers,
        "tags",
        "POST",
        "/api/tags/expression/check",
    )
    .await
    .map_err(ApiError)?;
    Ok(Json(evaluate_expression_check(
        &state.manager,
        &input.expression,
        input.external_name.as_deref(),
    )))
}

// --- #342 段階B: 組み込み関数表の配布（式欄のセグメント補完用） ------------

/// `GET /api/tags/expression/functions` の応答要素。`banto_expr::
/// BuiltinFunction` をそのまま wire 形（camelCase）へ写す。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExpressionFunctionEntry {
    name: String,
    arity: usize,
    /// 補完候補に出す呼び出し形の見本（例: `if(条件, 真のとき, 偽のとき)`）。
    signature: String,
    /// 補完候補に出す1行の日本語説明。
    description: String,
}

/// `GET /api/tags/expression/functions` の応答。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ExpressionFunctionsResponse {
    functions: Vec<ExpressionFunctionEntry>,
}

/// `GET /api/tags/expression/functions`（#342 段階B）: 式欄のセグメント補完に
/// 出す**組み込み関数の一覧**を返す。
///
/// **なぜ API で配るのか**: 関数名と引数個数の正は
/// `banto_expr::BUILTIN_FUNCTIONS`（型検査の `check_call` が読むのと同じ表）
/// 1箇所だけにしたい。TS からは Rust の const を import できないので、
/// 「フロントに関数表を手書きする」（＝2つ目の表を作ってドリフトさせる）
/// 代わりにサーバーが配る。**内容は静的**（DB も設定も読まない）なので
/// クライアントは1回取得してキャッシュしてよい。
///
/// 認可は `POST /api/tags/expression/check` と同じ `require_editor` -
/// 式欄を触れるのは編集権限のあるユーザーだけで、補完もその式欄の付属機能
/// だから（読み取り専用の内容ではあるが、ゲートを分けて2種類にしない）。
/// MCP へは足さない（補完は UI 専用、#342 段階B 実装指示）。
///
/// `plc_connections_test`/`tags_expression_check` と同じ理由で意図的に
/// `#[utoipa::path]` を付けず `ApiDoc` にも加えない（`/api/v1/*` のみを
/// 文書化する既存方針）。
pub(super) async fn tags_expression_functions(
    State(state): State<TagRegistryState>,
    headers: HeaderMap,
) -> Result<Json<ExpressionFunctionsResponse>, ApiError> {
    require_editor(
        &state.auth,
        &state.audit,
        &headers,
        "tags",
        "GET",
        "/api/tags/expression/functions",
    )
    .await
    .map_err(ApiError)?;
    Ok(Json(ExpressionFunctionsResponse {
        functions: banto_expr::BUILTIN_FUNCTIONS
            .iter()
            .map(|f| ExpressionFunctionEntry {
                name: f.name.to_string(),
                arity: f.arity,
                signature: f.signature.to_string(),
                description: f.description.to_string(),
            })
            .collect(),
    }))
}
