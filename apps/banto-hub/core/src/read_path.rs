//! Read-on-demand(「その場読み」、T20 ①b、docs/banto-hub-t20-design.md §3.1、
//! 案A「分離経路」)の共有実装。
//!
//! ## ①a(write_path.rs)との対称性・非対称性
//!
//! ①a は「文字列タグへの書き込みは write_path 経由のみ、記録計の
//! read/current_values/tstore には触れない」という境界を書き込み側に敷いた。
//! ①b はその読み取り版 - **文字列タグは収集パイプラインから意図的に
//! スキップされている**(`banto_collect::config`の S1 制約)ため、
//! `GET /api/v1/values/{tag}`(cache 読み、`crate::hub::read_current`
//! 経由)では文字列タグの値を一切取得できない。この経路はそれを埋める
//! 「その場で PLC から直接読む」別経路であり、**current_values/tstore/
//! 収集タスクの string スキップのいずれにも触れない**(案A の境界その
//! ものをそのまま踏襲)。
//!
//! ①a の書き込みゲート(1〜8: catalog解決→writable→enabled→simulation→
//! protocol→write_enabled→rate limit→値変換→log-before-write)と違い、
//! こちらは**副作用が無い読み取り**なので、writable・write_enabled・
//! レート制限・監査は一切無い。ゲートは以下のみ:
//!
//! 1. catalog 解決(未定義 → [`ReadNowRejection::NotFound`])
//! 2. `tag_kind != "plc"`(internal/computed タグは PLC 接続を経由しない。
//!    §4.2「internal タグ...タグ空間内で完結」「computed タグ...式で決まる」
//!    のとおり、このその場読み経路が意味を持たないので
//!    [`ReadNowRejection::NotPlcBacked`]。cache 読みを使うべき)
//! 3. 接続のプロトコルに broker ドライバが登録されていない →
//!    [`ReadNowRejection::UnsupportedProtocol`](①a の gate 5 と同じ
//!    `banto_broker::is_supported_protocol` が唯一の正)
//! 4. peek handle が無い(収集停止中等、新規にはダイヤルしない) →
//!    [`ReadNowRejection::NoSession`]
//! 5. broker からの応答が per-request Bad、または `handle.read` 自体が
//!    `Err` → [`ReadNowRejection::ReadFailed`]
//!
//! ## peek handle(非スポーン、T15-4/T12 と同じ規律)
//!
//! ①a の書き込みは `CollectorManager::write_broker_handle_peek`
//! (T15-4、write 専用の non-spawning peek)を使う。この読み取り経路は
//! その読み取り版として `CollectorManager::sessions().handle_for`
//! (T12、`crate::rest`の接続テストが既に使っている non-spawning
//! `ReadOnlyHandle` peek)を使う - どちらも「セッションが既に生きていれば
//! それを覗くだけ、無ければ新規にダイヤルしない(fail closed)」という
//! 同じ契約(`crate::broker_glue::HubSessions`のモジュール doc「T15-4」節
//! 参照)。
//!
//! ## スケーリング(オーナー方針: cache 読みと揃える)
//!
//! 数値タグは `crate::hub::read_current`(cache 読み)と同じ
//! `banto_tags::scale_raw` を適用し、工学値を返す - read-on-demand と
//! キャッシュ読みで同じタグの数字が食い違うと利用者が混乱するため
//! (「決めてコメントに明記」の実装指示に基づくオーナー方針の明文化)。
//! 文字列にスケーリングは無い(`banto_tags::tag::validate_tag_input`が
//! string タグへの raw/eng 設定を登録時に拒否している -
//! `crate::write_path::build_plc_string_write_request`のdoc comment と
//! 同じ理由)。ビットも(cache 読みと同じく、`banto_collect::task::record_group`
//! 参照)スケーリングしない。

use banto_broker::is_supported_protocol;
use banto_core::BantoError;
use banto_plc::{
    Address, BatchReadRequest, BatchReadResult, DataType, PlcValue, ReadRequest, StringReadRequest,
};
use banto_tags::{
    scale_raw, PlcConnectionService, Scaling, TagService, PLC_TAG_KIND, STRING_DATA_TYPE,
};

use crate::hub::CollectorManager;
use crate::write_path::string_encoding_from_tag;

/// 拒否理由([`crate::write_path::WriteRejection`]の読み取り版)。監査・
/// レート制限・writable/実効enabled検査は無い - このモジュールの doc
/// comment参照。
#[derive(Debug, Clone, PartialEq)]
pub enum ReadNowRejection {
    NotFound,
    /// internal/computed タグ(PLC 接続を経由しない) - このモジュールの
    /// doc comment gate 2 参照。
    NotPlcBacked,
    UnsupportedProtocol,
    /// T12/T15-4 と同じ non-spawning peek: 収集セッションが無い(収集停止中
    /// 等)。新規にはダイヤルしない(fail closed)。
    NoSession,
    InvalidAddress(String),
    ReadFailed(String),
    Internal(String),
}

impl ReadNowRejection {
    /// REST 用の `(error コード, detail)`。HTTP ステータスは
    /// `crate::rest::read_now_rejection_response` がこの分岐と対応付ける
    /// (`crate::write_path::WriteRejection::rest_error_code`と同じ形)。
    pub fn rest_error_code(&self) -> &'static str {
        match self {
            ReadNowRejection::NotFound => "not_found",
            ReadNowRejection::NotPlcBacked => "not_plc_backed",
            ReadNowRejection::UnsupportedProtocol => "read_unsupported_protocol",
            ReadNowRejection::NoSession => "no_session",
            ReadNowRejection::InvalidAddress(_) => "invalid_address",
            ReadNowRejection::ReadFailed(_) => "read_failed",
            ReadNowRejection::Internal(_) => "internal",
        }
    }

    pub fn detail(&self) -> Option<String> {
        match self {
            ReadNowRejection::InvalidAddress(detail)
            | ReadNowRejection::ReadFailed(detail)
            | ReadNowRejection::Internal(detail) => Some(detail.clone()),
            _ => None,
        }
    }

    /// `json!({ "error": ..., "detail"?: ... })` - MCP `read_tag_now` が
    /// (`crate::write_path::WriteRejection::to_json`と同じ形で)そのまま
    /// `tool_error` に載せる。
    pub fn to_json(&self) -> serde_json::Value {
        match self.detail() {
            Some(detail) => {
                serde_json::json!({ "error": self.rest_error_code(), "detail": detail })
            }
            None => serde_json::json!({ "error": self.rest_error_code() }),
        }
    }
}

fn map_registry_error(err: BantoError) -> ReadNowRejection {
    ReadNowRejection::Internal(err.to_string())
}

/// [`execute_read_now`] の成功結果。
#[derive(Debug, Clone, PartialEq)]
pub struct ReadNowValue {
    pub tag: String,
    /// 数値は scale 済み(cache 読みと同じ工学値)、文字列/ビットはそのまま
    /// (このモジュールの doc comment「スケーリング」節参照)。
    pub value: PlcValue,
}

/// `banto_plc::PlcValue` を REST/MCP のワイヤ値(数値・真偽値・文字列)へ
/// 変換する - [`crate::rest`]の `ReadNowResponse` と `crate::mcp` の
/// `read_tag_now` ツールが共有する(二重実装を避ける、`banto_plc::PlcValue`
/// は外部型なので inherent メソッドを生やせず、ここに自由関数として置く)。
pub fn plc_value_to_json(value: &PlcValue) -> serde_json::Value {
    match value {
        PlcValue::F64(x) => serde_json::json!(x),
        PlcValue::Bit(b) => serde_json::json!(b),
        PlcValue::Str(s) => serde_json::json!(s),
    }
}

/// T20 ①b(案A、docs/banto-hub-t20-design.md §3.1)の唯一の入口: `tag`
/// (外部名)を PLC からその場で読む。**current_values/tstore/収集タスクの
/// string スキップには一切触れない**(このモジュールの doc comment参照) -
/// catalog から接続情報を引き、`CollectorManager::sessions().handle_for`
/// (non-spawning read-only peek)で既存セッションを覗くだけ。
pub async fn execute_read_now(
    manager: &CollectorManager,
    tag: &str,
) -> Result<ReadNowValue, ReadNowRejection> {
    // gate 1: catalog 解決
    let map = manager.tag_map();
    let Some(entry) = map.get(tag).cloned() else {
        return Err(ReadNowRejection::NotFound);
    };

    // gate 2: PLC タグのみ対象(このモジュールの doc comment参照) - internal/
    // computed タグは PLC 接続を経由しないので、その場読みという概念自体が
    // 意味を持たない(cache 読みを使うべき)。
    if entry.tag_kind != PLC_TAG_KIND {
        return Err(ReadNowRejection::NotPlcBacked);
    }

    let (connection_id, _group_id, tag_id) = entry.ids;

    let conn = PlcConnectionService::new(manager.pool())
        .get(connection_id)
        .await
        .map_err(map_registry_error)?;

    // gate 3: ①a の gate 5 と同じプロトコルゲート。
    if !is_supported_protocol(&conn.protocol) {
        return Err(ReadNowRejection::UnsupportedProtocol);
    }

    let tag_row = TagService::new(manager.pool())
        .get(tag_id)
        .await
        .map_err(map_registry_error)?;

    let request = build_plc_read_request(&conn, &entry, &tag_row)?;

    // gate 4: non-spawning peek(このモジュールの doc comment参照) - 新規に
    // 実機へダイヤルしない。
    let Some(handle) = manager.sessions().handle_for(conn.id) else {
        return Err(ReadNowRejection::NoSession);
    };

    // gate 5: broker への読み取り。
    let results = handle
        .read(vec![request])
        .await
        .map_err(|err| ReadNowRejection::ReadFailed(err.to_string()))?;
    let value = match results.into_iter().next() {
        Some(BatchReadResult::Value(v)) => v,
        Some(BatchReadResult::Bad(err)) => {
            return Err(ReadNowRejection::ReadFailed(err.to_string()))
        }
        None => {
            return Err(ReadNowRejection::ReadFailed(
                "broker から応答がありませんでした".to_string(),
            ))
        }
    };

    // スケーリング(このモジュールの doc comment「スケーリング」節参照):
    // 数値のみ cache 読みと同じ scale_raw を適用する。`Scaling::from_parts`
    // は永続化済みの行に対しては到達不能な Err のみを返す - 防御的に
    // no-scaling へフォールバックする(`crate::write_path::build_plc_write_request`
    // の unscale と同じ流儀)。
    let value = match value {
        PlcValue::F64(raw) => {
            let scaling = Scaling::from_parts(
                tag_row.raw_lo,
                tag_row.raw_hi,
                tag_row.eng_lo,
                tag_row.eng_hi,
                "scaling",
            )
            .unwrap_or(None);
            let scaled = match scaling {
                Some(scaling) => scale_raw(raw, &scaling),
                None => raw,
            };
            PlcValue::F64(scaled)
        }
        other => other,
    };

    Ok(ReadNowValue {
        tag: tag.to_string(),
        value,
    })
}

/// `entry`/`tag_row` から `BatchReadRequest` を組み立てる
/// (`crate::write_path::build_plc_write_request`/
/// `build_plc_string_write_request` の読み取り版 - アドレス解決は#131の
/// プロトコル別分岐をそのまま踏襲する)。**副作用は無い**(broker へは
/// 渡さない)。
///
/// bit-in-word アドレス(T8、`"D100.5"`)は write と違って特別扱いが要らない。
/// `banto_plc::planning`(SLMP)が `ReadRequest{address, data_type: Bit}`
/// のアドレスにビット修飾があれば自動的に `ReadKind::BitInWord` へ振り分ける
/// (write 側が専用の RMW `BatchWriteRequest::BitInWord` variant を要るのは
/// SLMP にビット専用書き込みコマンドが無いためで、読み取りには元々その
/// 制約が無い)。
fn build_plc_read_request(
    conn: &banto_tags::PlcConnection,
    entry: &crate::hub::TagEntry,
    tag_row: &banto_tags::Tag,
) -> Result<BatchReadRequest, ReadNowRejection> {
    // #131 と同じプロトコル別アドレス解決(`crate::write_path::build_plc_write_request`
    // 参照)。
    let address = match conn.protocol.as_str() {
        "modbus-tcp" => Address::parse(&entry.address),
        _ => Address::parse_slmp(&entry.address),
    };
    let address = match address {
        Ok(address) => address,
        Err(err) => return Err(ReadNowRejection::InvalidAddress(err.to_string())),
    };

    if entry.data_type == STRING_DATA_TYPE {
        let words = tag_row.string_length.unwrap_or(0).clamp(0, u16::MAX as i64) as u16;
        let encoding = string_encoding_from_tag(tag_row);
        return Ok(BatchReadRequest::String(StringReadRequest {
            address,
            words,
            encoding,
        }));
    }

    let Some(data_type) = DataType::parse(&entry.data_type) else {
        // catalog の data_type は登録時に banto-tags で検証済みのはずの
        // 防御的分岐(`crate::write_path::convert_value`の同種の到達不能
        // ケースと同じ扱い)。
        return Err(ReadNowRejection::Internal(format!(
            "未知の data_type です: {}",
            entry.data_type
        )));
    };

    Ok(BatchReadRequest::Numeric(ReadRequest {
        address,
        data_type,
    }))
}
