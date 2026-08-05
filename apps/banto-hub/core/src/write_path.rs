//! 書き込みの共有実装 (docs/tag-server-design.md §6「書き込み経路の安全設計」・
//! T4 実装指示「WriteValue: REST の POST /api/v1/values/{tag} と同一のゲート・
//! 監査・レート制限を通す...二重実装は絶対に不可」)。
//!
//! ## 抽出の経緯
//!
//! T2-4 は `crate::rest::v1_write_value` 1本にゲート1〜8（catalog 解決 →
//! writable → 実効 enabled → プロトコル対応 → 受付トグル → レート制限 →
//! 値変換 → log-before-write）を直接実装していた。T4（gRPC の
//! `WriteValue`）はこの安全ゲート列を**REST と一言一句同じ意味論**で通す
//! 必要があり（§6 全体は「受付経路は REST と gRPC の2つのみ」という前提の
//! 上に成り立つ安全設計 - 認証・監査・レート制限が2系統に割れることは
//! 許容されない）、この関数 [`execute_write`] へ切り出して両方が呼ぶ形に
//! リファクタリングした。`crate::rest::v1_write_value`/`crate::grpc`
//! の `write_value` ハンドラは、**この関数に来る前の transport 固有の前段**
//! （REST: セッション token 拒否・body の JSON パース。gRPC: メタデータの
//! bearer 認証）だけを自分で行い、以降は完全にここへ委譲する。
//!
//! ## この関数が扱わないもの(呼び出し元の責務)
//!
//! - **認証そのもの**(有効な API キーであることの確認)は呼び出し元が
//!   事前に済ませ、[`crate::api_keys::ApiKeyContext`] を渡す。
//! - **`write:{tag}` スコープの完全一致検査**も呼び出し元の責務
//!   (REST・gRPC いずれも「外部名 `tag` が分かった時点で検査する」規律が
//!   ここより外側にあるほうが自然 - `crate::api_keys::ApiKeyContext::has_write_scope`
//!   を各ハンドラの冒頭で呼ぶ)。
//! - **body/request のワイヤ形式からの値抽出**(REST の JSON
//!   `{"v": <number|bool>}` パース、gRPC の `oneof num|bool` 分解)は
//!   呼び出し元が済ませ、正規化後の `Option<f64>`(`None` = 型として
//!   受理できない値 - REST の 422 `unsupported_value_type` に相当)を渡す。
//!
//! ## ゲート順(§6 実装指示 §5、relay-wright `engine/writer.rs` のゲート順を踏襲)
//!
//! 1. catalog 解決(未定義 → [`WriteRejection::NotFound`])
//! 2. `writable == false` → [`WriteRejection::NotWritable`](監査不要 -
//!    定義上の拒否)
//! 3. 実効 enabled == false → [`WriteRejection::TagDisabled`]
//! 4. Modbus 接続配下 → [`WriteRejection::UnsupportedProtocol`](§6-7:
//!    v1 の書き込みは SLMP のみ)
//! 5. write_enabled(受付)off → [`WriteRejection::WritesDisabled`] +
//!    write_audit に `suppressed_disabled`
//! 6. レート制限 would_exceed → [`WriteRejection::RateLimited`] + キー
//!    trip + `rate_limit_tripped` 記録
//! 7. 値変換: 工学値 → `banto_tags::unscale`(スケーリング設定があれば)
//!    → data_type に応じた `banto_plc::TagValue`(bit は bool、数値は
//!    範囲チェックで [`WriteRejection::ValueOutOfRange`])。文字列タグは
//!    [`WriteRejection::UnsupportedValueType`] で拒否
//! 8. **log-before-write** → `CollectorManager::write_broker_handle`
//!    経由の `BrokerHandle::write`(1タグ=1リクエスト)→ set_result →
//!    [`WriteOk`] または [`WriteRejection::WriteFailed`]
//!
//! gate 6 の「would_exceed」判定(peek)と、gate 8 直前の実際の
//! `record`(消費)は別々のロック区間 - 「ゲート通過後・物理書き込み前」の
//! 意味論を守るため、gate 7(値変換・拒否になりうる)の**後**、gate 8 の
//! broker 呼び出しの**前**に record する(`crate::write_rate` のモジュール
//! doc comment参照)。

use std::time::Instant;

use banto_collect::Quality;
use banto_core::BantoError;
use banto_plc::{Address, DataType, TagValue as PlcTagValue};
use banto_plc_write::{
    BatchWriteRequest, WriteRequest as PlcWriteRequest, WriteResult as PlcWriteResult,
};
use banto_server::ServerEvent;
use banto_tags::{
    unscale, PlcConnectionService, Scaling, TagService, PLC_TAG_KIND, STRING_DATA_TYPE,
};
use serde_json::json;
use tokio::sync::broadcast;
use tokio::sync::Mutex as AsyncMutex;

use crate::api_keys::{ApiKeyContext, ApiKeysService};
use crate::computed::upsert_retained_value;
use crate::hub::CollectorManager;
use crate::write_audit::{WriteAuditAction, WriteAuditResult, WriteAuditRow, WriteAuditService};
use crate::write_control::WriteControl;
use crate::write_rate::WriteRateLimiter;

/// 書き込み成功時の結果 - `tag` は呼び出し元がそのままエコーバックできる
/// よう外部名を持つ(REST の `WriteValueResponse`/gRPC の
/// `WriteValueResponse` いずれも `{ tag, result: "ok" }` 形)。
#[derive(Debug, Clone, PartialEq)]
pub struct WriteOk {
    pub tag: String,
}

/// [`execute_write`] の拒否理由。各 transport がここから自分のワイヤ表現
/// (REST: HTTP ステータス + JSON、gRPC: `tonic::Status`)へ変換する -
/// `crate::rest::write_rejection_response`/`crate::grpc::write_rejection_status`
/// 参照。ここに独自のワイヤ知識(HTTP ステータスコード等)は一切持たせない -
/// それこそが「ゲートの実装を2つに割らない」ことの核心。
#[derive(Debug, Clone, PartialEq)]
pub enum WriteRejection {
    /// gate 1: catalog に存在しない外部名(REST: 404, gRPC: NOT_FOUND)。
    NotFound,
    /// gate 2: `writable == false`(REST: 403, gRPC: PERMISSION_DENIED)。
    NotWritable,
    /// gate 3: 実効 enabled が false(REST: 409, gRPC: FAILED_PRECONDITION)。
    TagDisabled,
    /// gate 4: Modbus 接続配下(REST: 501, gRPC: UNIMPLEMENTED)。
    UnsupportedProtocol,
    /// gate 5: 書き込み受付 off(REST: 503, gRPC: FAILED_PRECONDITION)。
    WritesDisabled,
    /// gate 6: レート制限超過(REST: 429, gRPC: RESOURCE_EXHAUSTED)。
    RateLimited,
    /// body/request の値が型として受理できない、または文字列タグ
    /// (REST: 422, gRPC: INVALID_ARGUMENT)。
    UnsupportedValueType(Option<String>),
    /// gate 7: 数値範囲外(REST: 422, gRPC: INVALID_ARGUMENT)。
    ValueOutOfRange(String),
    /// gate 7: catalog のアドレスが SLMP としてパースできない(防御的分岐 -
    /// 登録時検証済みのはずで実運用では到達しない、REST: 422, gRPC:
    /// INVALID_ARGUMENT)。
    InvalidAddress(String),
    /// gate 8: broker への書き込み要求自体、または物理書き込みが失敗
    /// (REST: 502, gRPC: UNAVAILABLE)。
    WriteFailed(String),
    /// log-before-write の DB 書き込みに失敗(REST: 500, gRPC: INTERNAL) -
    /// 防御的分岐。
    AuditWriteFailed,
    /// レジストリからの接続/タグ行の再読み込みに失敗(防御的分岐 - catalog
    /// に載っている時点で存在するはずだが、レース(削除)を排除しない -
    /// REST: 元の `BantoError` をそのまま伝播、gRPC: INTERNAL)。
    Internal(String),
}

/// [`execute_write`] が必要とする共有状態一式への借用。REST/gRPC いずれの
/// ハンドラも自分の `State`/サービス構造体からこれを組み立てて渡す -
/// 所有権を持たない(呼び出し元の `Arc`/サービスを borrow するだけ)ので、
/// 呼び出しの都度使い捨てで作ってよい。
pub struct WriteDeps<'a> {
    pub manager: &'a CollectorManager,
    pub api_keys: &'a ApiKeysService,
    pub write_audit: &'a WriteAuditService,
    pub write_control: &'a WriteControl,
    /// タグ毎+全体の2段レート制限(設計 §6-4)。複数リクエストが並行して
    /// 飛んでくるため`tokio::sync::Mutex`で包んで共有する - `crate::rest`の
    /// `WriteState`のモジュール doc comment参照。
    pub rate_limiter: &'a AsyncMutex<WriteRateLimiter>,
    pub events: &'a broadcast::Sender<ServerEvent>,
}

/// 値変換ゲート(§6 実装指示 §5の7番「範囲チェックで422」)の数値範囲検査。
/// `banto_plc_write::encode`(`encode_word_value`)と同じ境界値を意図的に
/// ここで独立に再現している - その関数は `pub(crate)` でこの crate から
/// 呼べないため。物理書き込み自体は broker 内部でもう一度
/// `banto_plc_write::encode` を通るので二重チェックになるが、ここでの
/// チェックには「範囲外の値を write_audit に一切残さず(gate 8 の
/// log-before-write に到達する前に)弾ける」という利点がある。
/// `DataType::Bit` はこの関数の呼び出し元([`execute_write`])で既に
/// 分岐済みなので扱わない。
fn validate_numeric_range(data_type: DataType, x: f64) -> Result<(), String> {
    if !x.is_finite() {
        return Err("値が有限ではありません".to_string());
    }
    let integral_in_range = |lo: f64, hi: f64| -> Result<(), String> {
        if x.fract() != 0.0 {
            return Err("整数ではありません".to_string());
        }
        if x < lo || x > hi {
            return Err(format!("範囲 [{lo}, {hi}] の外です"));
        }
        Ok(())
    };
    match data_type {
        DataType::U16 => integral_in_range(0.0, u16::MAX as f64),
        DataType::I16 => integral_in_range(i16::MIN as f64, i16::MAX as f64),
        DataType::U32 => integral_in_range(0.0, u32::MAX as f64),
        DataType::I32 => integral_in_range(i32::MIN as f64, i32::MAX as f64),
        DataType::F32 => {
            if (x as f32).is_finite() {
                Ok(())
            } else {
                Err("f32 で表現するには大きすぎます".to_string())
            }
        }
        DataType::Bit => Ok(()),
    }
}

fn map_registry_error(err: BantoError) -> WriteRejection {
    WriteRejection::Internal(err.to_string())
}

/// 書き込みゲート1〜8の本体(このモジュールの doc comment 参照)。
///
/// `requested`: 呼び出し元が transport 固有の表現(REST の JSON `v`、gRPC の
/// `oneof num|bool`)から正規化した工学値。`None` は「型として受理できない
/// 値」(REST の 422 `unsupported_value_type` に相当)を意味し、gate 4 の
/// **後**(REST の元実装と同じ位置 - プロトコル非対応の 501 を型エラーの
/// 422 より先に返す)で [`WriteRejection::UnsupportedValueType`] として
/// 拒否する。
pub async fn execute_write(
    deps: &WriteDeps<'_>,
    ctx: &ApiKeyContext,
    tag: &str,
    requested: Option<f64>,
) -> Result<WriteOk, WriteRejection> {
    // gate 1: catalog 解決
    let map = deps.manager.tag_map();
    let Some(entry) = map.get(tag).cloned() else {
        return Err(WriteRejection::NotFound);
    };

    // gate 2: writable opt-in
    if !entry.writable {
        return Err(WriteRejection::NotWritable);
    }

    // gate 3: 実効 enabled(接続・グループ・タグいずれかが無効なら false)
    if !entry.enabled {
        return Err(WriteRejection::TagDisabled);
    }

    let (connection_id, _group_id, tag_id) = entry.ids;

    // gate 4: Modbus 接続配下は非対応(§6-7: v1 の書き込みは SLMP のみ)。
    //
    // T6-2 決定(docs/tag-server-design.md §4.2「internal タグ...PLC へ送ら
    // ない」): `internal` タグは PLC 接続を一切経由しないため、このプロト
    // コルゲート自体を丸ごとスキップする(接続行を読みにいく必要すらない)。
    // `computed` タグはそもそもここへ到達しない - gate 2 の
    // `writable == false` が、banto_tags 側で computed タグには常に
    // `writable = false` を強制する登録時検証と噛み合って、§4.2 表の
    // 「書き込み: 不可(値は式が決まる)」を特別扱いなしに成立させている。
    // writable opt-in・write スコープ・レート制限・write_enabled・監査は
    // `internal` タグにもそのまま一様に適用する - タグ種別で緩めない保守的
    // な一様ルール(SLMP プロトコルゲートだけがタグ種別で分岐する唯一の
    // 例外)。
    let conn = if entry.tag_kind == PLC_TAG_KIND {
        let conn = PlcConnectionService::new(deps.manager.pool())
            .get(connection_id)
            .await
            .map_err(map_registry_error)?;
        if conn.protocol != "slmp" {
            return Err(WriteRejection::UnsupportedProtocol);
        }
        Some(conn)
    } else {
        None
    };

    // body/request の値の型検査(このモジュールの doc comment 参照 - gate
    // 5 の前段、独自の番号は持たない)。
    let Some(requested) = requested else {
        return Err(WriteRejection::UnsupportedValueType(None));
    };

    // gate 5: 書き込み受付(WriteControl)が off
    if !deps.write_control.is_enabled() {
        let row = WriteAuditRow::new(
            ctx.id,
            ctx.name.clone(),
            tag_id,
            tag.to_string(),
            WriteAuditAction::Write,
            WriteAuditResult::SuppressedDisabled,
        )
        .with_value_requested(requested);
        if let Err(err) = deps.write_audit.insert_row(&row).await {
            eprintln!("banto-hub: 書き込み監査(suppressed_disabled)の記録に失敗しました: {err}");
        }
        return Err(WriteRejection::WritesDisabled);
    }

    // gate 6: レート制限(peek のみ - 実際の消費は gate 7 通過後)
    let now = Instant::now();
    let would_exceed = {
        let mut limiter = deps.rate_limiter.lock().await;
        limiter.would_exceed(tag_id, now)
    };
    if would_exceed {
        let trip_result = deps.api_keys.trip(ctx.id).await;
        let detail = match trip_result {
            Ok(_) => "レート制限を超過したため API キーをトリップしました".to_string(),
            Err(err) => {
                format!("レート制限を超過しましたが、API キーのトリップに失敗しました: {err}")
            }
        };
        let row = WriteAuditRow::new(
            ctx.id,
            ctx.name.clone(),
            tag_id,
            tag.to_string(),
            WriteAuditAction::RateLimitTripped,
            WriteAuditResult::SuppressedRateLimited,
        )
        .with_value_requested(requested)
        .with_detail(detail);
        if let Err(err) = deps.write_audit.insert_row(&row).await {
            eprintln!("banto-hub: 書き込み監査(rate_limit_tripped)の記録に失敗しました: {err}");
        }
        // collect_events には書かない(T2-4 の設計判断、`crate::rest`の
        // 元コメント参照) - 管理 UI 向けの SSE のみで通知する。
        let _ = deps.events.send(ServerEvent::Notice {
            level: "warning".to_string(),
            message: format!(
                "書き込みレート制限を超過したため API キー '{}' をトリップしました(タグ: {tag})",
                ctx.name
            ),
        });
        let _ = deps.events.send(ServerEvent::ResourceChanged {
            resource: "api_keys".to_string(),
        });
        return Err(WriteRejection::RateLimited);
    }

    // gate 7: 値変換 - 文字列タグは拒否
    if entry.data_type == STRING_DATA_TYPE {
        return Err(WriteRejection::UnsupportedValueType(Some(
            "文字列タグへの書き込みは対応していません".to_string(),
        )));
    }
    let Some(data_type) = DataType::parse(&entry.data_type) else {
        // catalog に載っている時点で banto-tags の CHECK 制約を通過済みの
        // はずなので実運用では到達しない防御的分岐。
        return Err(WriteRejection::UnsupportedValueType(None));
    };

    let tag_row = TagService::new(deps.manager.pool())
        .get(tag_id)
        .await
        .map_err(map_registry_error)?;

    // gate 8: log-before-write(PLC/internal 共通 - 実行前に必ず監査行を
    // 先に作る、§6-3)。
    let pending_row = WriteAuditRow::new(
        ctx.id,
        ctx.name.clone(),
        tag_id,
        tag.to_string(),
        WriteAuditAction::Write,
        WriteAuditResult::Ok,
    )
    .with_value_requested(requested);
    let audit_id = match deps.write_audit.insert_pending(&pending_row).await {
        Ok(id) => id,
        Err(err) => {
            eprintln!("banto-hub: 書き込み監査(log-before-write)の記録に失敗しました: {err}");
            return Err(WriteRejection::AuditWriteFailed);
        }
    };

    // record はゲート通過後・物理書き込み前(§6 実装指示、
    // `crate::write_rate` のモジュール doc comment 参照)。
    {
        let mut limiter = deps.rate_limiter.lock().await;
        limiter.record(tag_id, now);
    }

    let outcome = if let Some(conn) = conn {
        write_plc_tag(deps, &conn, &entry, data_type, &tag_row, requested).await
    } else {
        write_internal_tag(deps, &entry, tag_id, data_type, tag_row.retain, requested).await
    };

    let final_result = match &outcome {
        Ok(()) => WriteAuditResult::Ok,
        Err(_) => WriteAuditResult::Failed,
    };
    if let Err(err) = deps.write_audit.set_result(audit_id, final_result).await {
        eprintln!("banto-hub: 書き込み監査の確定に失敗しました: {err}");
    }

    outcome.map(|()| WriteOk {
        tag: tag.to_string(),
    })
}

/// gate 8 の PLC タグ分岐: 従来どおり `banto_tags::unscale` → SLMP
/// アドレス解決 → `BrokerHandle::write`。`execute_write` から抽出しただけで
/// 挙動は変えていない(T6-2 前の唯一の書き込み経路そのもの)。
async fn write_plc_tag(
    deps: &WriteDeps<'_>,
    conn: &banto_tags::PlcConnection,
    entry: &crate::hub::TagEntry,
    data_type: DataType,
    tag_row: &banto_tags::Tag,
    requested: f64,
) -> Result<(), WriteRejection> {
    // スケーリング設定があれば工学値→raw に unscale する(無ければ工学値
    // そのものが raw)。`Scaling::from_parts` は永続化済みの行に対しては
    // 到達不能な Err のみを返す - 防御的に no-scaling へフォールバックする。
    let scaling = Scaling::from_parts(
        tag_row.raw_lo,
        tag_row.raw_hi,
        tag_row.eng_lo,
        tag_row.eng_hi,
        "scaling",
    )
    .unwrap_or(None);
    let raw = match scaling {
        Some(scaling) => unscale(requested, &scaling),
        None => requested,
    };

    let tag_value = if data_type == DataType::Bit {
        PlcTagValue::Bit(raw != 0.0)
    } else {
        match validate_numeric_range(data_type, raw) {
            Ok(()) => PlcTagValue::F64(raw),
            Err(detail) => return Err(WriteRejection::ValueOutOfRange(detail)),
        }
    };

    let address = match Address::parse_slmp(&entry.address) {
        Ok(address) => address,
        Err(err) => {
            // catalog のアドレスは登録時に banto-tags で検証済みのはずの
            // 防御的分岐(§6-7: writable にできるのは SLMP 接続配下のタグ
            // のみなので、ここに来る時点でアドレスは SLMP 表記のはず)。
            return Err(WriteRejection::InvalidAddress(err.to_string()));
        }
    };

    let handle = deps
        .manager
        .write_broker_handle(conn)
        .map_err(|err| WriteRejection::WriteFailed(err.to_string()))?;

    let request = BatchWriteRequest::Numeric(PlcWriteRequest {
        address,
        data_type,
        value: tag_value,
    });
    match handle.write(vec![request]).await {
        Ok(results) => match results.into_iter().next() {
            Some(PlcWriteResult::Ok) => Ok(()),
            Some(PlcWriteResult::Bad(write_err)) => {
                Err(WriteRejection::WriteFailed(write_err.to_string()))
            }
            None => Err(WriteRejection::WriteFailed(
                "broker から応答がありませんでした".to_string(),
            )),
        },
        Err(err) => Err(WriteRejection::WriteFailed(err.to_string())),
    }
}

/// gate 8 の internal タグ分岐(T6-2、docs/tag-server-design.md §4.2「内部
/// タグ...タグ空間内で完結」): PLC アドレス・スケーリングは一切関与せず、
/// `crate::hub::CollectorManager::server_store` へ工学値をそのまま書くだけ
/// (banto-collect の収集タスクが `CurrentValuesHandle` へ書く値も既にスケー
/// ル済みの工学値なので、対称性を保つため raw への変換をしない)。
/// `retain == true` なら書き込み成功時に `hub_retained_values` へ upsert
/// する(§4.2「retain フラグで再起動時の最終値復元」、`crate::computed`の
/// モジュール doc comment参照)。
async fn write_internal_tag(
    deps: &WriteDeps<'_>,
    entry: &crate::hub::TagEntry,
    tag_id: i64,
    data_type: DataType,
    retain: bool,
    requested: f64,
) -> Result<(), WriteRejection> {
    if data_type != DataType::Bit {
        if let Err(detail) = validate_numeric_range(data_type, requested) {
            return Err(WriteRejection::ValueOutOfRange(detail));
        }
    }

    let now_ms = deps.manager.clock().now_ms();
    let server_store = deps.manager.server_store();
    server_store.set(&entry.tag_key, Some(requested), Quality::Good, now_ms);

    if retain {
        if let Err(err) =
            upsert_retained_value(&deps.manager.pool(), tag_id, requested, now_ms).await
        {
            eprintln!("banto-hub: retain 値の永続化に失敗しました(tag_id={tag_id}): {err}");
            // 永続化の失敗は書き込み自体の失敗にしない(ライブの
            // ServerTagStore への書き込みは既に成功している - 次回再起動
            // までの間、値はタグ空間上は正しく見える。再起動時にだけ古い
            // 値へ戻る可能性がある、という劣化に留める)。
        }
    }
    Ok(())
}

/// REST 応答用の `(error, detail?)` 文字列ペア - `crate::rest` がこれを
/// HTTP ステータス + JSON へ変換する(このモジュール自身は HTTP を知らない -
/// モジュール doc comment 参照)。`json!` に載せるための最小限の値のみ返す。
impl WriteRejection {
    /// REST 用の `(error コード, detail)`。HTTP ステータスは
    /// `crate::rest::write_rejection_response` がこの分岐と1対1で対応付ける。
    pub fn rest_error_code(&self) -> &'static str {
        match self {
            WriteRejection::NotFound => "not_found",
            WriteRejection::NotWritable => "not_writable",
            WriteRejection::TagDisabled => "tag_disabled",
            WriteRejection::UnsupportedProtocol => "write_unsupported_protocol",
            WriteRejection::WritesDisabled => "writes_disabled",
            WriteRejection::RateLimited => "rate_limited",
            WriteRejection::UnsupportedValueType(_) => "unsupported_value_type",
            WriteRejection::ValueOutOfRange(_) => "value_out_of_range",
            WriteRejection::InvalidAddress(_) => "invalid_address",
            WriteRejection::WriteFailed(_) => "write_failed",
            WriteRejection::AuditWriteFailed => "audit_write_failed",
            WriteRejection::Internal(_) => "internal",
        }
    }

    pub fn detail(&self) -> Option<String> {
        match self {
            WriteRejection::UnsupportedValueType(detail) => detail.clone(),
            WriteRejection::ValueOutOfRange(detail)
            | WriteRejection::InvalidAddress(detail)
            | WriteRejection::WriteFailed(detail)
            | WriteRejection::Internal(detail) => Some(detail.clone()),
            _ => None,
        }
    }

    /// `json!({ "error": ..., "detail"?: ... })` - REST の元実装が返していた
    /// 本文と同一の形。
    pub fn to_json(&self) -> serde_json::Value {
        match self.detail() {
            Some(detail) => json!({ "error": self.rest_error_code(), "detail": detail }),
            None => json!({ "error": self.rest_error_code() }),
        }
    }
}
