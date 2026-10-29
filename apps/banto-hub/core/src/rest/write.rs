//! 書き込みエンドポイント: `POST /api/v1/values/{tag}`（T2-4、設計 §5.1・§6）と
//! レシピ一括書き込み `POST /api/v1/values/batch`（T20-3a、docs/banto-hub-t20-design.md §3.3）。
//!
//! `rest.rs` から中身を変えずに移したもの（docs/banto-hub-driver-refactor-plan.md §8.1、
//! P0-1 の手順 7）。ルーターへの組み込み（`tag_space_router`）は `rest/tag_space.rs`、
//! OpenAPI の `ApiDoc` は親の `rest.rs` にある。外から使われるパス（`crate::rest::X`）は
//! 親の re-export で保つ。

use super::{
    broadcast, missing_write_scope_response, session_token_cannot_write_response, ApiError,
    ApiKeyContext, ApiKeysService, Arc, AsyncMutex, BantoError, CollectionController,
    CollectorManager, Deserialize, Extension, IntoResponse, Json, Path, Response, Serialize,
    ServerEvent, State, StatusCode, ToSchema, WriteAuditService, WriteControl, WriteRateLimiter,
};

// --- 書き込みエンドポイント (T2-4、設計 §5.1「POST /api/v1/values/{tag}」・
// §6 全体) -------------------------------------------------------------
//
// ゲート順・監査・レート制限・受付トグルの規律は relay-wright の
// `engine/writer.rs` を直接の下敷きにしている（§6 実装指示: 「参考実装は
// relay-wright の engine/{writer,rate_limiter,arming,write_audit}.rs（設計は
// 流用、コードは hub 語彙で書き直し）」）。hub 固有の差分は主に3点:
// - relay-wright の `Writer` はルールエンジンが生成する `PendingWrite` を
//   1タスクが順に処理する専有構造だが、hub は複数の REST/gRPC リクエストが
//   並行に飛んでくるので、[`WriteRateLimiter`] を `tokio::sync::Mutex` で
//   包んで共有する（peek→exceed 判定→(ゲート通過なら)record を1つの
//   ロック区間に収めず、ロックは短時間だけ握って離す - 詳細は
//   `crate::write_path::execute_write` 本体のコメント参照）。
// - relay-wright の disarm は「ルールエンジン全体」を止めるが、hub の
//   `WriteControl` は書き込みエンドポイントの受付可否のみを制御する
//   （収集自体は止めない）。
// - 認証主体が「ルール」ではなく「API キー」なので、監査行の主語も
//   `api_key_id`/`api_key_name_snapshot`（`crate::write_audit` 参照）。
//
// **T4（設計 §5.4）でゲート1〜8の本体を `crate::write_path::execute_write`
// へ抽出した** - gRPC の `WriteValue`（`crate::grpc`）が REST と同一の
// ゲート・監査・レート制限を通る必要があり（実装指示「二重実装は絶対に
// 不可」）、以降このモジュールに残るのは REST 固有の前段（セッション token
// 拒否・JSON body のパース）と、`WriteRejection` を HTTP ステータス + JSON
// へ変換する [`write_rejection_response`] のみ。

/// `POST /api/v1/values/{tag}` の request body（設計 §5.1「body
/// `{ "v": <number|bool> }`」、T20 ①a で文字列を追加 - docs/banto-hub-t20-design.md
/// §3.1）。`v` は `serde_json::Value` のまま保持し、[`parse_requested_value`]
/// で数値/真偽値/文字列のみを受理する（配列・オブジェクトは 422）—
/// `bool`/`number`/`string` を1つの Rust 型に素直にマップする serde 表現が
/// なく、utoipa の untagged enum サポートも弱いため、検証はハンドラ内で行う。
#[derive(Debug, Deserialize, ToSchema)]
pub(super) struct WriteValueRequest {
    /// 書き込む工学値。数値タグには数値、bit タグには真偽値、string タグ
    /// には文字列（T20 ①a）。（2026-08-06〜: 型が data_type と一致しない
    /// 場合は 422 `unsupported_value_type` - 暗黙の型変換はしない）。
    #[schema(value_type = f64, example = 1)]
    v: serde_json::Value,
}

/// `POST /api/v1/values/{tag}` の成功応答（設計 §6 実装指示 §5「応答
/// `{ "tag", "result": "ok" }`」）。gRPC の `WriteValueResponse`
/// （`crate::grpc`）も同じ2フィールドの型付き版。
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct WriteValueResponse {
    tag: String,
    result: String,
}

/// リクエスト body の `v` を [`crate::write_path::RequestedValue`] に正規化
/// する。`bool` は [`RequestedValue::Bool`]、数値は [`RequestedValue::Num`]、
/// 文字列は [`RequestedValue::Str`](T20 ①a、docs/banto-hub-t20-design.md
/// §3.1 で追加 - string タグへの書き込みに使う)。どちらの型が来たかは
/// gate 7（`crate::write_path::execute_write`）が data_type との対称性検査に
/// 使うので、ここで `f64` へ潰さない（2026-08-06 変更: 従来は `bool` を
/// `1.0`/`0.0` に潰して数値と区別せずに渡していた）。配列・オブジェクト・
/// `null` は `None`（呼び出し元が 422 `unsupported_value_type` を返す）。
/// REST 固有の wire 形式（JSON の `v`）からの変換であり、gRPC 側は
/// `oneof num|bool` を直接分解するだけで済む（文字列は本スライスの対象外 -
/// gRPC の `WriteValueRequest` に文字列 oneof が無い）ため、この関数は
/// `crate::write_path` へは移していない。
pub(crate) fn parse_requested_value(
    v: &serde_json::Value,
) -> Option<crate::write_path::RequestedValue> {
    use crate::write_path::RequestedValue;
    if let Some(b) = v.as_bool() {
        Some(RequestedValue::Bool(b))
    } else if let Some(s) = v.as_str() {
        Some(RequestedValue::Str(s.to_string()))
    } else {
        v.as_f64().map(RequestedValue::Num)
    }
}

/// [`crate::write_path::WriteRejection`] を REST の HTTP ステータス + JSON
/// 本文へ変換する（gRPC 側の対応物は `crate::grpc::write_rejection_status`）。
/// `NotFound`（404）だけは特別扱いする - `crate::write_path` は transport に
/// 依存しない設計上、`banto_core::BantoError`/`ApiError` を一切知らないため、
/// ここで `ApiError(BantoError::NotFound { .. })` へ組み立て直す。それ以外は
/// `WriteRejection::rest_error_code`/`detail`（gate の意味論のみを持つ純粋な
/// 情報）から機械的に HTTP ステータスを割り当てる - 対応表は
/// `crate::write_path::WriteRejection` の doc comment に列挙したものと一致
/// させる。
fn write_rejection_response(tag: String, rejection: crate::write_path::WriteRejection) -> Response {
    use crate::write_path::WriteRejection;

    if matches!(rejection, WriteRejection::NotFound) {
        return ApiError(BantoError::NotFound {
            resource: "tags".to_string(),
            id: tag,
        })
        .into_response();
    }

    let status = match &rejection {
        WriteRejection::CollectionNotRunning(_) => StatusCode::SERVICE_UNAVAILABLE,
        WriteRejection::NotFound => unreachable!("上で特別扱い済み"),
        WriteRejection::NotWritable => StatusCode::FORBIDDEN,
        WriteRejection::TagDisabled => StatusCode::CONFLICT,
        WriteRejection::UnsupportedProtocol => StatusCode::NOT_IMPLEMENTED,
        WriteRejection::WritesDisabled => StatusCode::SERVICE_UNAVAILABLE,
        WriteRejection::RateLimited => StatusCode::TOO_MANY_REQUESTS,
        WriteRejection::UnsupportedValueType(_)
        | WriteRejection::ValueOutOfRange(_)
        | WriteRejection::InvalidAddress(_) => StatusCode::UNPROCESSABLE_ENTITY,
        WriteRejection::WriteFailed(_) => StatusCode::BAD_GATEWAY,
        WriteRejection::AuditWriteFailed | WriteRejection::Internal(_) => {
            StatusCode::INTERNAL_SERVER_ERROR
        }
        // T20-3a: `execute_write`(単票、この関数が変換する唯一の呼び出し
        // 元)は `BatchAborted`/`DuplicateTagInBatch` を絶対に返さない -
        // `crate::write_path`の各バリアントのdoc comment参照。バッチ専用の
        // `POST /api/v1/values/batch`(`v1_write_values_batch`)は
        // `WriteRejection::to_json()`をそのまま封筒に載せる別の応答経路
        // (下記)を使い、この関数は通らない。
        WriteRejection::BatchAborted => {
            unreachable!(
                "execute_write は BatchAborted を返さない - execute_write_batch 専用の結果"
            )
        }
        WriteRejection::DuplicateTagInBatch(_) => {
            unreachable!(
                "execute_write は DuplicateTagInBatch を返さない - execute_write_batch 専用の結果"
            )
        }
    };
    (status, Json(rejection.to_json())).into_response()
}

/// [`v1_write_value`] とその周辺(レート制限・受付・監査)が必要とする状態
/// 一式。`TagSpaceState`とは別の型にしているのは、axum のルーターは
/// 1つの `State<T>` にしか `.with_state` できないため -
/// [`tag_space_router`] は GET 系ハンドラ用の `TagSpaceState` の
/// ルーターとこの `WriteState` のルーターを別々に組み立てて `.merge`
/// する(同じパス `/api/v1/values/{tag}` に GET/POST 両方が乗るのは axum
/// の `MethodRouter::merge_for_path` がそのまま面倒を見る)。
///
/// T4: 各フィールドは `crate::write_path::WriteDeps` へそのまま borrow
/// できる形にしてある - `crate::grpc::GrpcService` も同じ形のフィールドを
/// 持ち、両者がここから `WriteDeps` を組み立てて
/// `crate::write_path::execute_write` を呼ぶ(このファイル冒頭「書き込み
/// エンドポイント」節の doc comment参照)。
#[derive(Clone)]
pub(super) struct WriteState {
    pub(super) manager: Arc<CollectorManager>,
    pub(super) collection_controller: Option<Arc<CollectionController>>,
    pub(super) api_keys: ApiKeysService,
    pub(super) write_audit: WriteAuditService,
    pub(super) write_control: Arc<WriteControl>,
    /// タグ毎+全体の2段レート制限(設計 §6-4)。複数リクエストが並行して
    /// 飛んでくるため`tokio::sync::Mutex`で包んで共有する - relay-wright
    /// の `Writer` が単一タスク専有だったのと違い、hub はこの1個の
    /// リミッタを複数リクエストが取り合う。ロックは「peek」「record」の
    /// 各操作の間だけ短時間握る(`crate::write_path::execute_write` 本体の
    /// コメント参照)。
    pub(super) rate_limiter: Arc<AsyncMutex<WriteRateLimiter>>,
    pub(super) events: broadcast::Sender<ServerEvent>,
}

/// `POST /api/v1/values/{tag}` - 書き込み(設計 §5.1・§6 全体)。
///
/// このハンドラ自身が担うのは REST 固有の前段だけ:
///
/// 事前段(番号なし、§6-8「認証」): `crate::rest::require_tag_space_auth`
/// を通過した時点で「有効な `bh_` API キー」であることは確定しているが、
/// このハンドラ自身がさらに (a) セッション token では到達できない
/// (ミドルウェア側で弾く - `ctx` extension が無ければ 403)、(b)
/// `write:{tag}` スコープの完全一致、の2つを検査する。
///
/// body の `v` を [`parse_requested_value`] で工学値へ正規化した後は、
/// ゲート1〜8の本体(catalog 解決・writable・実効 enabled・プロトコル対応・
/// 受付トグル・レート制限・値変換・log-before-write)を
/// `crate::write_path::execute_write` へそのまま委譲する(T4、実装指示
/// 「二重実装は絶対に不可」) - 結果を [`write_rejection_response`] で
/// HTTP ステータス + JSON へ変換するだけ。ゲートの詳細な番号・意味論は
/// `crate::write_path::execute_write` の doc comment を参照。
#[utoipa::path(
    post,
    path = "/api/v1/values/{tag}",
    params(("tag" = String, Path, description = "外部名 {connection}.{group}.{tag}")),
    request_body = WriteValueRequest,
    responses(
        (status = 200, description = "書き込み成功", body = WriteValueResponse),
        (status = 403, description = "not_writable / missing_write_scope / session_token_cannot_write / key_tripped"),
        (status = 404, description = "catalog に存在しない外部名"),
        (status = 409, description = "tag_disabled"),
        (status = 422, description = "unsupported_value_type / value_out_of_range"),
        (status = 429, description = "rate_limited"),
        (status = 501, description = "write_unsupported_protocol"),
        (status = 502, description = "write_failed"),
        (status = 503, description = "writes_disabled / collection_not_running"),
    ),
    tag = "tag-space",
)]
pub(super) async fn v1_write_value(
    State(state): State<WriteState>,
    Path(tag): Path<String>,
    ctx: Option<Extension<ApiKeyContext>>,
    Json(body): Json<WriteValueRequest>,
) -> Response {
    // 事前段(a): セッション token では書けない(§6-8) - require_tag_space_auth
    // は API キー・セッション token どちらでもここまで到達させるので、
    // ここで ApiKeyContext extension の有無により再確認する。
    let Some(Extension(ctx)) = ctx else {
        return session_token_cannot_write_response();
    };
    // 事前段(b): write:{tag} スコープの完全一致(read スコープでは書けない)。
    if !ctx.has_write_scope(&tag) {
        // #711: スコープ外の拒否も書込み監査に残す。
        crate::write_path::record_missing_write_scope(
            state.manager.as_ref(),
            &state.write_audit,
            &ctx,
            &tag,
        )
        .await;
        return missing_write_scope_response();
    }

    let requested = parse_requested_value(&body.v);
    let deps = crate::write_path::WriteDeps {
        manager: state.manager.as_ref(),
        collection_controller: state.collection_controller.as_deref(),
        api_keys: &state.api_keys,
        write_audit: &state.write_audit,
        write_control: state.write_control.as_ref(),
        rate_limiter: state.rate_limiter.as_ref(),
        events: &state.events,
    };

    match crate::write_path::execute_write(&deps, &ctx, &tag, requested).await {
        Ok(ok) => Json(WriteValueResponse {
            tag: ok.tag,
            result: "ok".to_string(),
        })
        .into_response(),
        Err(rejection) => write_rejection_response(tag, rejection),
    }
}

// --- レシピ一括書き込み（T20-3a、docs/banto-hub-t20-design.md §3.3） -------
//
// `POST /api/v1/values/batch`。ゲート1〜8の本体は
// `crate::write_path::execute_write_batch` へ丸ごと委譲する（単票
// `v1_write_value` と同じ「二重実装は絶対に不可」規律、§3.7「ゲートを
// 迂回しない」）。このハンドラ自身が担うのは REST 固有の前段(セッション
// token 拒否・**各エントリの** `write:{tag}` スコープ検査・JSON body の
// 正規化)と、per-entry 結果を常に 200 の封筒へ変換するだけ
// （`crate::rest`の `POST /api/tags/batch`(`BatchTagsResponse`)と同じ
// 「1件でも不正なら全体拒否は例外ではなく通常の応答」という思想 -
// このモジュールの `BatchTagsResponse` doc comment参照）。

/// `POST /api/v1/values/batch` の1エントリぶんの request body。`v` は単票
/// [`WriteValueRequest::v`]・[`parse_requested_value`] と同じ正規化。
#[derive(Debug, Deserialize, ToSchema)]
pub(super) struct BatchWriteEntryRequest {
    /// 外部名 `{connection}.{group}.{tag}`。
    tag: String,
    /// 書き込む工学値(単票と同じ - 数値タグには数値、bit タグには真偽値)。
    /// 2026-09-05 レビュー対応: `#[schema(value_type = f64)]` を外した -
    /// 数値タグには数値、bit タグには真偽値を送れる(`parse_requested_value`
    /// 参照)にもかかわらず、以前は OpenAPI スキーマ上 `f64` 固定になって
    /// おり、bit タグへの `true`/`false` が生成ドキュメント上表現できて
    /// いなかった。注釈を外すと `serde_json::Value` の既定の
    /// `ToSchema`(utoipa 組み込み、`schema_type = AnyValue`)にフォール
    /// バックし、number・boolean のどちらも受理できることが正しく表れる。
    /// 単票 [`WriteValueRequest::v`] は同じ `f64` 固定の問題を抱えている
    /// が、今回の指摘対象はバッチのみなのでそちらは変更しない(別の既存
    /// 課題として残す)。
    v: serde_json::Value,
}

/// `POST /api/v1/values/batch` の request body（設計 §3.3
/// 「`[{tag, value}, ...]` を受ける」）。
#[derive(Debug, Deserialize, ToSchema)]
pub(super) struct BatchWriteValueRequest {
    writes: Vec<BatchWriteEntryRequest>,
}

/// per-entry 結果の1件分。成功時は `ok: true` のみ、失敗時は `ok: false` +
/// [`crate::write_path::WriteRejection::rest_error_code`]/`detail`
/// （単票のエラー本文と同じ語彙 - `error`/`detail` フィールド名も揃える）。
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct BatchWriteEntryResponse {
    tag: String,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

impl From<crate::write_path::BatchEntryOutcome> for BatchWriteEntryResponse {
    fn from(outcome: crate::write_path::BatchEntryOutcome) -> Self {
        match outcome.result {
            Ok(()) => Self {
                tag: outcome.tag,
                ok: true,
                error: None,
                detail: None,
            },
            Err(rejection) => Self {
                tag: outcome.tag,
                ok: false,
                error: Some(rejection.rest_error_code().to_string()),
                detail: rejection.detail(),
            },
        }
    }
}

/// `POST /api/v1/values/batch` の応答。**常に 200**（設計 §3.3・このモジュール
/// の `BatchTagsResponse` doc comment と同じ判断 - 事前ゲート
/// all-or-nothing による全体中止は例外ではなく通常の応答で、クライアントは
/// `writes[].ok`/`error`/`detail` を行ごとに見る）。認証エラー(403)や JSON
/// パース失敗(400)は既存の `ApiError`/`Json` extractor 経路のまま区別する。
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct BatchWriteValuesResponse {
    writes: Vec<BatchWriteEntryResponse>,
}

/// `POST /api/v1/values/batch` - レシピ一括書き込み（T20-3a）。
///
/// 事前段(a): セッション token では書けない([`v1_write_value`]と同じ)。
/// 事前段(b): **各エントリの** `write:{tag}` スコープを検査する - 1件でも
/// スコープ不足なら(事前ゲート all-or-nothing の思想を認証にも適用)
/// **全体を拒否**して 403 `missing_write_scope` を返し、1件も書かない
/// (`crate::write_path::execute_write_batch` へは進まない)。
///
/// スコープを通過した後は、`v` の正規化(単票と同じ
/// [`parse_requested_value`])を済ませ、ゲート1〜8の本体をまるごと
/// `crate::write_path::execute_write_batch` へ委譲する。
#[utoipa::path(
    post,
    path = "/api/v1/values/batch",
    request_body = BatchWriteValueRequest,
    responses(
        (status = 200, description = "常に200 - per-entry の ok/error は writes[] を参照", body = BatchWriteValuesResponse),
        (status = 403, description = "missing_write_scope（1件でもスコープ不足）/ session_token_cannot_write"),
    ),
    tag = "tag-space",
)]
pub(super) async fn v1_write_values_batch(
    State(state): State<WriteState>,
    ctx: Option<Extension<ApiKeyContext>>,
    Json(body): Json<BatchWriteValueRequest>,
) -> Response {
    let Some(Extension(ctx)) = ctx else {
        return session_token_cannot_write_response();
    };
    // 事前ゲート all-or-nothing をスコープ検査にも適用する(設計 §3.3
    // REST 節): 1件でも write スコープが無ければ、どのエントリも
    // execute_write_batch へ進めず全体を 403 で拒否する。
    // #711: スコープが無かったエントリごとに書込み監査へ残す。
    let mut scope_missing = false;
    for w in body.writes.iter().filter(|w| !ctx.has_write_scope(&w.tag)) {
        scope_missing = true;
        crate::write_path::record_missing_write_scope(
            state.manager.as_ref(),
            &state.write_audit,
            &ctx,
            &w.tag,
        )
        .await;
    }
    if scope_missing {
        return missing_write_scope_response();
    }

    let entries: Vec<(String, Option<crate::write_path::RequestedValue>)> = body
        .writes
        .into_iter()
        .map(|w| (w.tag, parse_requested_value(&w.v)))
        .collect();

    let deps = crate::write_path::WriteDeps {
        manager: state.manager.as_ref(),
        collection_controller: state.collection_controller.as_deref(),
        api_keys: &state.api_keys,
        write_audit: &state.write_audit,
        write_control: state.write_control.as_ref(),
        rate_limiter: state.rate_limiter.as_ref(),
        events: &state.events,
    };

    let outcomes = crate::write_path::execute_write_batch(&deps, &ctx, entries).await;
    Json(BatchWriteValuesResponse {
        writes: outcomes.into_iter().map(Into::into).collect(),
    })
    .into_response()
}
