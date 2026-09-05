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
//!   `{"v": <number|bool|string>}` パース(T20 ①a で文字列を追加)、gRPC の
//!   `oneof num|bool` 分解(gRPC は本スライスの対象外 - 文字列書き込みは
//!   REST/MCP のみ))は呼び出し元が済ませ、型情報を保った [`RequestedValue`] に正規化した
//!   `Option<RequestedValue>`(`None` = 型として受理できない値 - REST の
//!   422 `unsupported_value_type` に相当)を渡す。
//!
//! ## ゲート順(§6 実装指示 §5、relay-wright `engine/writer.rs` のゲート順を踏襲)
//!
//! 1. catalog 解決(未定義 → [`WriteRejection::NotFound`])
//! 2. `writable == false` → [`WriteRejection::NotWritable`](監査不要 -
//!    定義上の拒否)
//! 3. 実効 enabled == false → [`WriteRejection::TagDisabled`]
//! 4. シミュレーション中の PLC タグ → [`WriteRejection::SimulationWriteRejected`]
//!    (保存済み simulation または production controller の AllSimulation。
//!    internal/mem タグはこのゲートの対象外)
//! 5. 接続のプロトコルに broker ドライバが登録されていない →
//!    [`WriteRejection::UnsupportedProtocol`](`banto_broker::is_supported_protocol`/
//!    `banto_broker::DRIVERS` が唯一の正 - #131（2026-09-01）以降、slmp と
//!    modbus-tcp の両方がこのゲートを通過する。Modbus は Numeric（数値/
//!    ビット、FC5/6/15/16）のみ対応、String は broker 側で per-request Bad
//!    になる）
//! 6. write_enabled(受付)off → [`WriteRejection::WritesDisabled`] +
//!    write_audit に `suppressed_disabled`
//! 7. レート制限 would_exceed → [`WriteRejection::RateLimited`] + キー
//!    trip + `rate_limit_tripped` 記録
//! 8. 値変換: まず [`RequestedValue`] の種別と data_type の対称性を検査する
//!    (bit タグには bool のみ、数値タグには数値のみ、string タグには文字列
//!    のみ - 暗黙の型変換はしない。2026-08-06 追加、§4.2 の「タグ種別を
//!    跨いだ暗黙変換をしない」設計思想を書き込み経路にも適用した。T20 ①a で
//!    string タグの対称性検査を追加)。一致しなければ
//!    [`WriteRejection::UnsupportedValueType`]。数値/bit タグが一致すれば
//!    工学値 → `banto_tags::unscale`(スケーリング設定があれば) →
//!    data_type に応じた `banto_plc::TagValue`(数値は範囲チェックで
//!    [`WriteRejection::ValueOutOfRange`])。string タグはスケーリングを
//!    経由せず(登録時に禁止されている)、`banto_tags::Tag::string_encoding`
//!    に応じた `banto_plc_write::StringWriteRequest` を組み立てる(T20 ①a、
//!    このモジュールの「T20 ①a」節参照)
//! 9. **log-before-write** → `CollectorManager::write_broker_handle_peek`
//!    (T15-4、既存セッションの覗き見のみ・新規ダイヤルしない)経由の
//!    `BrokerHandle::write`(1タグ=1リクエスト)→ set_result →
//!    [`WriteOk`] または [`WriteRejection::WriteFailed`]
//!
//! ### T15-4: gate 8 は broker セッションを新規に張らない(no-spawn peek)
//!
//! gate 8 に達するまでの区間(レジストリからのタグ/接続行の再読み込み、
//! 監査行 insert、レート制限の record など)はすべて `.await` を含み、
//! `CollectionController::stop()` の `stop_and_join`(収集停止時に broker
//! セッションを止めて join する)と時間的に競合しうる。外側の gate
//! (`CollectionNotRunning`)は `execute_write` 呼び出し**前**の一点で
//! `CollectionState` を見るだけなので、このレースを塞げない - 呼び出し後に
//! 収集が止まっても `execute_write` は最後まで走り切ってしまう。
//!
//! 従来は `CollectorManager::write_broker_handle`
//! (`banto_broker::SessionDirectory::ensure_connection`)を使っていたが、
//! これは「セッションが無ければ新規に接続する」実装のため、上記のレースで
//! セッションが `stop_and_join` 済みだった場合に**実機へ新しい TCP
//! セッションをダイヤルしてしまう** - 収集を止めたつもりの PLC へ、意図せず
//! 書き込み用の接続が張られる。これを閉じるため、書き込み経路は
//! `CollectorManager::write_broker_handle_peek`
//! (`banto_broker::SessionDirectory::handle`、新規スポーンしない覗き見)
//! だけを使う: セッションが既に無ければ新規に張らず、そのまま
//! [`WriteRejection::WriteFailed`] で fail closed する
//! (`crate::broker_glue::HubSessions::write_handle_for`の doc comment参照)。
//!
//! gate 6 の「would_exceed」判定(peek)と、gate 8 直前の実際の
//! `record`(消費)は別々のロック区間 - 「ゲート通過後・物理書き込み前」の
//! 意味論を守るため、gate 7(値変換・拒否になりうる)の**後**、gate 8 の
//! broker 呼び出しの**前**に record する(`crate::write_rate` のモジュール
//! doc comment参照)。
//!
//! ## T20 ①a: 文字列タグへの書き込み(docs/banto-hub-t20-design.md §3.1、案A)
//!
//! [`RequestedValue::Str`] を追加し、gate 7([`convert_value`])が
//! `data_type == "string"` タグに対して文字列を受け入れる([`ConvertedValue::Str`])。
//! 書き込みは `banto_plc_write` の write_path 経由(`StringWriteRequest`)のみで、
//! **記録計の read パス(current_values/tstore/収集タスクの string スキップ)
//! には一切触れない**(案A の境界。文字列 read は①bの対象)。文字コードは
//! `banto_tags::Tag::string_encoding`(既定 UTF-8、タグ単位で Shift-JIS も
//! 選択可)から決まる - `banto-plc-write`の`StringEncoding`参照。
//!
//! **監査の文字列 value 表現(T20 宿題#1、解消済み)**:
//! `hub_write_audit.value_requested` は `REAL` 列で文字列を持てないため、
//! [`RequestedValue::as_audit_value`]/[`ConvertedValue::as_audit_value`] は
//! 文字列書き込みに対して常に `None`(NULL)を返す(`detail` 列は
//! `insert_pending` → `set_result` の2段階で `set_result` の引数に必ず
//! 上書きされる仕組みのため、仮置きしても成功時に消える - `write_audit.rs`
//! のモジュール doc comment参照)。当初はこの制約により文字列書き込みの
//! テキストが監査に一切残らなかったが、専用列 `hub_write_audit.value_requested_text`
//! (`db.rs::apply_app_schema` の `ALTER TABLE ADD COLUMN`)を追加し、
//! [`RequestedValue::as_audit_text`]/[`ConvertedValue::as_audit_text`] が
//! そのテキストを `WriteAuditRow::with_value_requested_text` 経由で
//! `insert_pending`/`insert_row` 時点に書き込むことで解消した(`set_result`
//! は `value_requested_text` に触れないので、後段の更新で消えない)。

use std::collections::HashMap;
use std::time::Instant;

use banto_broker::is_supported_protocol;
use banto_collect::Quality;
use banto_core::BantoError;
use banto_plc::{Address, DataType, TagValue as PlcTagValue};
use banto_plc_write::{
    BatchWriteRequest, StringEncoding, StringWriteRequest, WriteRequest as PlcWriteRequest,
    WriteResult as PlcWriteResult,
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
use crate::controller::{CollectionController, CollectionState, RunMode};
use crate::hub::CollectorManager;
use crate::write_audit::{WriteAuditAction, WriteAuditResult, WriteAuditRow, WriteAuditService};
use crate::write_control::WriteControl;
use crate::write_rate::WriteRateLimiter;

/// transport から正規化された、書き込みリクエストの値 - `f64` 1本に
/// 潰す前の型情報を持たせる(2026-08-06 追加)。REST の JSON `{"v":
/// <number|bool>}` パース・gRPC の `oneof num|bool` 分解は、それぞれ
/// [`RequestedValue::Num`]/[`RequestedValue::Bool`] を素直に組み立てるだけ。
/// どちらの型が来たかを潰さずに [`execute_write`] まで運び、gate 7 で
/// data_type との対称性(bit タグには bool のみ、数値タグには数値のみ)を
/// 検査するために存在する。
#[derive(Debug, Clone, PartialEq)]
pub enum RequestedValue {
    /// 数値タグ向け。
    Num(f64),
    /// bit タグ向け(ビットデバイス・T8 のビット付きアドレスとも共通)。
    Bool(bool),
    /// T20 ①a(docs/banto-hub-t20-design.md §3.1、案A): string タグ向け。
    /// gate 7([`convert_value`])で `data_type == "string"` との対称性を
    /// 検査する(string タグには文字列のみ、他のタグ種別に文字列は 422)。
    Str(String),
}

impl RequestedValue {
    /// gate 7 の対称性検査を通過した後、以降の工学値パイプライン
    /// (`unscale`・範囲チェック・監査行の `value_requested` 列)が
    /// 引き続き `f64` 1本で扱えるようにする変換。`Bool` は
    /// `true` → `1.0` / `false` → `0.0`(相互変換の唯一の場所 - gate 7 通過後
    /// はこの1回だけ `f64` へ潰す)。`Str` は数値として意味を持たないので
    /// `0.0` を返す - **この戻り値は「文字列だった」ことを表せない**。gate
    /// 5/6(gate 7 より前)の監査行が値を記録する際は、この関数ではなく
    /// [`Self::as_audit_value`] を使うこと。
    fn as_f64(&self) -> f64 {
        match self {
            RequestedValue::Num(v) => *v,
            RequestedValue::Bool(b) => {
                if *b {
                    1.0
                } else {
                    0.0
                }
            }
            RequestedValue::Str(_) => 0.0,
        }
    }

    /// gate 5/6(gate 7 の値変換より前)の監査行 `value_requested` 列向け。
    /// `hub_write_audit.value_requested` は `REAL`(f64)専用の列で文字列を
    /// 持てない - [`Self::Str`] は `None`(NULL)を返し、`0.0` のような
    /// 数値と紛れさせない。**この段階では string タグへの書き込みかどうか
    /// さえ未確定**(gate 7 未実行)なので、文字列の中身を`detail`列などへ
    /// 転記することもしない(このモジュールの doc comment 「気づいた設計上
    /// の問題」参照 - T20 ①a の既知の限界として報告済み)。
    fn as_audit_value(&self) -> Option<f64> {
        match self {
            RequestedValue::Str(_) => None,
            other => Some(other.as_f64()),
        }
    }

    /// T20 宿題#1: [`Self::as_audit_value`] が `None` を返す文字列書き込み
    /// について、監査行の `value_requested_text` 列へ残すテキストを返す
    /// (`Num`/`Bool` は数値列に記録済みなので `None`)。
    fn as_audit_text(&self) -> Option<String> {
        match self {
            RequestedValue::Str(s) => Some(s.clone()),
            _ => None,
        }
    }
}

/// gate 7([`convert_value`])の出力: エンジニアリング値の型。数値/bit タグは
/// 従来どおり `(DataType, f64)`(スケーリング済みではない、raw への unscale
/// は [`build_plc_write_request`] が行う)。string タグは
/// `banto_tags::STRING_DATA_TYPE` 以外の `data_type` を持ち得ないため
/// `DataType` を伴わない(`banto_plc::DataType` に string 相当の variant は
/// 無い - `banto-tags`の`ALLOWED_DATA_TYPES`のdoc comment参照)。
///
/// **string は常に PLC タグ**: `banto_tags::tag::validate_tag_input` が
/// `computed`/`internal` タグへの `data_type = "string"` を登録時に拒否して
/// いるため、`Str`はここへ到達した時点で必ず`resolved.conn.is_some()`
/// (T20 ①a、案A)。
#[derive(Debug, Clone, PartialEq)]
enum ConvertedValue {
    Numeric(DataType, f64),
    Str(String),
}

impl ConvertedValue {
    /// 監査行 `value_requested` 列向け(gate 7 通過後 - [`RequestedValue::as_audit_value`]
    /// の gate 7 後版)。`Str` は同じ理由で `None`。
    fn as_audit_value(&self) -> Option<f64> {
        match self {
            ConvertedValue::Numeric(_, v) => Some(*v),
            ConvertedValue::Str(_) => None,
        }
    }

    /// [`RequestedValue::as_audit_text`] の gate 7 後版。`Numeric` は数値列に
    /// 記録済みなので `None`。
    fn as_audit_text(&self) -> Option<String> {
        match self {
            ConvertedValue::Numeric(_, _) => None,
            ConvertedValue::Str(s) => Some(s.clone()),
        }
    }
}

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
    /// T14-4: the collection is not in a writable running state. This gate is
    /// only installed by production composition; the legacy compatibility
    /// router intentionally leaves it unset for existing embedders/tests.
    CollectionNotRunning(CollectionState),
    /// PLC タグがシミュレーション接続配下、または production controller
    /// の AllSimulation 実行中。実機へ誤書き込みしないため、broker handle
    /// の取得・レート制限消費・監査行作成より前に fail-closed する。
    /// internal/mem タグはこの拒否の対象外。
    SimulationWriteRejected,
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
    /// T20-3a(レシピ一括書き込み、docs/banto-hub-t20-design.md §3.3):
    /// このエントリ自身はゲート(1〜4・値型・gate 7)を通過したが、**同じ
    /// バッチ内の他のエントリがゲート NG だった**ため、事前ゲート
    /// all-or-nothing の原則([`execute_write_batch`]のモジュール doc
    /// comment参照)により物理書き込みを一切試みずバッチ全体を中止した。
    /// `execute_write`(単票)からは絶対に返らない - [`execute_write_batch`]
    /// 専用の結果(REST/gRPC の `write_rejection_response`/
    /// `write_rejection_status` がこの分岐に `unreachable!` を置いている
    /// 理由)。監査行も一切残らない(§3.3「1件も書かない」)。
    BatchAborted,
    /// T20-3a(2026-09-05 監査対応): バッチ内に同じ外部名が2回以上現れた
    /// (=同じタグに2つの値を書けと言われた・どちらが最終値か曖昧なユーザー
    /// 誤り)。DB にもレート制限にも一切触れない、最も安価な事前ゲートで
    /// 全エントリを拒否する([`execute_write_batch`]のモジュール doc
    /// comment「契約」の0番参照)。値は重複していた外部名そのもの。
    /// `execute_write`(単票)からは絶対に返らない([`BatchAborted`]と同じ
    /// 理由でREST/gRPCの変換に `unreachable!` を置く)。
    DuplicateTagInBatch(String),
}

/// [`execute_write`] が必要とする共有状態一式への借用。REST/gRPC いずれの
/// ハンドラも自分の `State`/サービス構造体からこれを組み立てて渡す -
/// 所有権を持たない(呼び出し元の `Arc`/サービスを borrow するだけ)ので、
/// 呼び出しの都度使い捨てで作ってよい。
pub struct WriteDeps<'a> {
    pub manager: &'a CollectorManager,
    /// Optional lifecycle gate. `None` preserves the legacy `api_router` /
    /// `GrpcService::new` behavior; production wiring supplies the controller
    /// so stopped writes fail closed before any broker ensure/spawn path.
    pub collection_controller: Option<&'a CollectionController>,
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

/// [`resolve_write_target`] の出力: 副作用の無い決定的ゲート(1〜4・値型
/// present)を通過した1エントリぶんの「対象が確定した」状態(T20-3a、
/// docs/banto-hub-t20-design.md §3.3)。
///
/// **gate 7(値変換)はまだ通していない**: `requested` は正規化前の
/// [`RequestedValue`] のまま持ち回る(2026-09-05 監査対応: 単票
/// [`execute_write`] は gate 7 を gate 5/6 の**後**で行う元の順序を厳密に
/// 保つ必要があり、gate 7 の実行タイミングを呼び出し元に委ねるため)。
///
/// 単票・バッチの両方がこれを組み立ててから先へ進む - ここまでの区間は
/// **DB からの読み取りのみ**で、監査 insert・レート制限の trip/record・
/// broker への書き込みはまだ一切発生していない。
struct ResolvedWrite {
    /// PLC タグなら対象接続、internal タグなら `None`。
    conn: Option<banto_tags::PlcConnection>,
    entry: crate::hub::TagEntry,
    tag_id: i64,
    /// 呼び出し元が渡した外部名のオウンドコピー - 監査行・
    /// [`BatchEntryOutcome`]・[`WriteOk`]がそのままエコーバックできるよう
    /// 保持する。
    tag_name: String,
    /// gate 7 未適用の正規化済みリクエスト値([`convert_value`]へ渡す)。
    requested: RequestedValue,
}

/// 書き込みゲート1〜4・値型 present の本体(このモジュールの doc comment
/// 参照)。**副作用は一切無い**(監査行 insert・レート制限の trip/record・
/// broker への書き込みのいずれも行わない - DB からの読み取り
/// [`PlcConnectionService::get`]のみ)。T20-3a(docs/banto-hub-t20-design.md
/// §3.3)でバッチの事前ゲート all-or-nothing を実現するために
/// [`execute_write`] から抽出した - 単票・バッチの両方がこの関数を呼ぶこと
/// で「ゲートを再実装しない」(設計 §3.7)を保つ。
///
/// **gate 7(値変換・[`convert_value`])はここに含まない**(2026-09-05
/// 監査対応)。理由: 単票 [`execute_write`] の元実装は gate 7 を gate 5
/// (受付トグル)・gate 6(レート制限)の**後**で行っており、「write_control
/// off ＋ 型不一致の値」は 422(型エラー)ではなく 503(writes_disabled)を
/// 返す。gate 7 を副作用の無いゲートとして早出しした最初の実装
/// (T20-3a 初版)はこの順序を壊し、単票の外部挙動を変えてしまっていた
/// (指摘を受けて修正)。gate 7 自体は副作用が無いので事前検証には使える
/// が、**呼び出し元がいつ呼ぶかを選べる**よう、この関数からは独立した
/// [`convert_value`] として切り出してある: 単票は gate 5/6 の後に呼び、
/// バッチは事前ゲート all-or-nothing の一部として resolve と合わせて先に
/// 呼ぶ(§3.3 は各エントリの事前検証を一括で行うことを要求しており、単票
/// のような「gate 5/6 が先」という制約はバッチには無い)。
///
/// `collection_mode`: 呼び出し元([`execute_write`]/[`execute_write_batch`])
/// が collection-running チェックの際に読んだ `RunMode`(コントローラが
/// 無ければ `None`)。バッチでは全エントリで同じ1回の読み取り結果を使い
/// 回す - このゲート自体はここへは含めない(バッチはエントリ毎ではなく
/// バッチ全体で1回だけ判定する、§3.3)。
///
/// `requested`: 呼び出し元が transport 固有の表現から正規化した
/// [`RequestedValue`]。`None` は「型として受理できない値」を意味し、
/// gate 4 の**後**(REST の元実装と同じ位置)で
/// [`WriteRejection::UnsupportedValueType`] として拒否する。
async fn resolve_write_target(
    deps: &WriteDeps<'_>,
    collection_mode: Option<RunMode>,
    tag: &str,
    requested: Option<RequestedValue>,
) -> Result<ResolvedWrite, WriteRejection> {
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

    // Simulation is a safety boundary, not a transport error. Check it before
    // loading the PLC connection or touching the broker, audit, or rate-limit
    // paths. Internal/mem tags deliberately remain writable as server-local
    // values; computed tags are already rejected by `writable == false`.
    if entry.tag_kind == PLC_TAG_KIND
        && (entry.simulation || collection_mode == Some(RunMode::AllSimulation))
    {
        return Err(WriteRejection::SimulationWriteRejected);
    }

    // gate 4: 接続のプロトコルに broker ドライバが登録されていなければ
    // 非対応(#131、2026-09-01: `banto_broker::is_supported_protocol`/
    // `banto_broker::DRIVERS` が唯一の正 - 現状 slmp と modbus-tcp の両方が
    // 登録済みなので、このゲートを実際に通過できないのは未登録プロトコル
    // だけになった。banto-tags の `ALLOWED_PROTOCOLS` は `"virtual"` も
    // 許可しているが、`"virtual"` 接続配下には PLC 種別タグを作成できない
    // ため - `banto_tags::tag::validate_tag_kind_placement` 参照 - 通常の
    // registry 経由ではこのゲートに`"virtual"`行が到達することはなく、この
    // 分岐は将来 broker に未登録のプロトコルが増えた場合や、想定外のデータ
    // に対する防御でもある)。
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
    // な一様ルール(プロトコルゲートだけがタグ種別で分岐する唯一の例外)。
    let conn = if entry.tag_kind == PLC_TAG_KIND {
        let conn = PlcConnectionService::new(deps.manager.pool())
            .get(connection_id)
            .await
            .map_err(map_registry_error)?;
        if !is_supported_protocol(&conn.protocol) {
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

    Ok(ResolvedWrite {
        conn,
        entry,
        tag_id,
        tag_name: tag.to_string(),
        requested,
    })
}

/// gate 7 の本体(このモジュールの doc comment 参照): 値変換 - data_type と
/// [`RequestedValue`] の種別の対称性を検査し(暗黙の型変換はしない - §4.2
/// の設計思想を書き込み経路にも適用)、通れば工学値を得る。**副作用は一切
/// 無い純関数** - [`resolve_write_target`] から独立させてある理由は、その
/// doc comment(2026-09-05 監査対応)参照。
///
/// T20 ①a(docs/banto-hub-t20-design.md §3.1、案A): string タグには
/// [`RequestedValue::Str`] のみを受け入れる(拒否ではなく [`ConvertedValue::Str`]
/// を素通しする)。他の全タグ種別には従来どおり数値/bool の対称性検査のみ -
/// [`RequestedValue::Str`] が来たら 422(数値/bit タグに文字列は書けない、
/// 型対称性の一般化)。
fn convert_value(
    entry: &crate::hub::TagEntry,
    requested: RequestedValue,
) -> Result<ConvertedValue, WriteRejection> {
    if entry.data_type == STRING_DATA_TYPE {
        return match requested {
            RequestedValue::Str(s) => Ok(ConvertedValue::Str(s)),
            _ => Err(WriteRejection::UnsupportedValueType(Some(
                "string タグには文字列を指定してください".to_string(),
            ))),
        };
    }
    if matches!(requested, RequestedValue::Str(_)) {
        return Err(WriteRejection::UnsupportedValueType(Some(
            "数値/真偽値タグに文字列は指定できません".to_string(),
        )));
    }
    let Some(data_type) = DataType::parse(&entry.data_type) else {
        // catalog に載っている時点で banto-tags の CHECK 制約を通過済みの
        // はずなので実運用では到達しない防御的分岐。
        return Err(WriteRejection::UnsupportedValueType(None));
    };

    // data_type と RequestedValue の種別の対称性を検査する(2026-08-06
    // 追加)。bit タグへの数値書き込み(旧実装は `raw != 0.0` で暗黙に
    // bool 化していた)、および数値タグへの bool 書き込みは、どちらも
    // 422 として拒否する。
    let value = match (data_type, requested) {
        (DataType::Bit, RequestedValue::Num(_)) => {
            return Err(WriteRejection::UnsupportedValueType(Some(
                "bit タグには true/false を指定してください".to_string(),
            )));
        }
        (dt, RequestedValue::Bool(_)) if dt != DataType::Bit => {
            return Err(WriteRejection::UnsupportedValueType(Some(
                "数値タグに真偽値は指定できません。数値を指定してください".to_string(),
            )));
        }
        // `Str` は上のガードで既に処理済み(到達しないが、`RequestedValue`
        // の網羅性のため明示的にunreachableとする)。
        (_, RequestedValue::Str(_)) => {
            unreachable!("文字列は関数冒頭で string タグ/非 string タグの両方を既に処理済み")
        }
        (_, value) => value.as_f64(),
    };

    Ok(ConvertedValue::Numeric(data_type, value))
}

/// [`execute_write_batch`] の事前ゲート all-or-nothing フェーズ1件分:
/// [`resolve_write_target`](gate 1〜4・値型 present)→ [`convert_value`]
/// (gate 7)の順に通す。バッチは単票と違い gate 5/6 より**前**にこの2つを
/// 済ませてよい(§3.3、[`resolve_write_target`]のdoc comment参照)。
/// **副作用は一切無い**(`tag_row` はここでは読まない - gate 8 commit の
/// 直前で読む、単票と同じ位置)。
struct PreparedWrite {
    conn: Option<banto_tags::PlcConnection>,
    entry: crate::hub::TagEntry,
    tag_id: i64,
    /// T20 ①a: 数値/bit タグは `Numeric`、string タグは `Str`
    /// ([`ConvertedValue`]のdoc comment参照)。
    value: ConvertedValue,
    tag_name: String,
}

async fn prepare_batch_entry(
    deps: &WriteDeps<'_>,
    collection_mode: Option<RunMode>,
    tag: &str,
    requested: Option<RequestedValue>,
) -> Result<PreparedWrite, WriteRejection> {
    let resolved = resolve_write_target(deps, collection_mode, tag, requested).await?;
    let value = convert_value(&resolved.entry, resolved.requested)?;
    Ok(PreparedWrite {
        conn: resolved.conn,
        entry: resolved.entry,
        tag_id: resolved.tag_id,
        value,
        tag_name: resolved.tag_name,
    })
}

/// 書き込みゲート1〜8の本体(このモジュールの doc comment 参照)。
///
/// T20-3a(docs/banto-hub-t20-design.md §3.3)でゲート1〜4・値型 present を
/// [`resolve_write_target`] へ、gate 7 を [`convert_value`] へ抽出した -
/// この関数自身は「collection-running チェック → [`resolve_write_target`]
/// → gate 5 → gate 6 → [`convert_value`](元の gate 7 の位置)→ `tag_row`
/// 読み込み(元の位置)→ gate 8」という**リファクタ前と一言一句同じ順序**
/// を骨組みとして持つ。**外部から見える挙動(ゲート順・監査行・レート
/// 制限・エラーコード・422/503 の優先順位)はリファクタ前と完全に一致
/// する**(2026-09-05 監査対応 - 初版は gate 7 を gate 5/6 より前に出して
/// しまい、「write_control off ＋ 型不一致の値」が 503 でなく 422 に変わる
/// 回帰があったため、この関数だけは元の直列実装と同じ呼び出し順に戻した。
/// [`resolve_write_target`]のdoc comment参照)。
///
/// `requested`: 呼び出し元が transport 固有の表現(REST の JSON `v`、gRPC の
/// `oneof num|bool`)から正規化した [`RequestedValue`]。詳細は
/// [`resolve_write_target`]のdoc comment参照。
pub async fn execute_write(
    deps: &WriteDeps<'_>,
    ctx: &ApiKeyContext,
    tag: &str,
    requested: Option<RequestedValue>,
) -> Result<WriteOk, WriteRejection> {
    let collection_mode = if let Some(controller) = deps.collection_controller {
        let status = controller.status();
        let state = status.state;
        if state != CollectionState::Running {
            return Err(WriteRejection::CollectionNotRunning(state));
        }
        Some(status.mode)
    } else {
        None
    };

    let resolved = resolve_write_target(deps, collection_mode, tag, requested).await?;

    // gate 5: 書き込み受付(WriteControl)が off。gate 7(値変換)より**前**
    // - 元実装と同じ順序(このモジュールの上のdoc comment参照)。監査の
    // `value_requested` は gate 7 未適用の生値(`RequestedValue::as_audit_value`
    // - 文字列は NULL、このモジュールの doc comment 「気づいた設計上の
    // 問題」参照)。
    if !deps.write_control.is_enabled() {
        let row = WriteAuditRow::new(
            ctx.id,
            ctx.name.clone(),
            resolved.tag_id,
            resolved.tag_name.clone(),
            WriteAuditAction::Write,
            WriteAuditResult::SuppressedDisabled,
        )
        .with_value_requested_opt(resolved.requested.as_audit_value())
        .with_value_requested_text(resolved.requested.as_audit_text());
        if let Err(err) = deps.write_audit.insert_row(&row).await {
            eprintln!("banto-hub: 書き込み監査(suppressed_disabled)の記録に失敗しました: {err}");
        }
        return Err(WriteRejection::WritesDisabled);
    }

    // gate 6: レート制限(peek のみ - 実際の消費は gate 7 通過後)。
    // gate 7 より**前** - 元実装と同じ順序。
    let now = Instant::now();
    let would_exceed = {
        let mut limiter = deps.rate_limiter.lock().await;
        limiter.would_exceed(resolved.tag_id, now)
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
            resolved.tag_id,
            resolved.tag_name.clone(),
            WriteAuditAction::RateLimitTripped,
            WriteAuditResult::SuppressedRateLimited,
        )
        .with_value_requested_opt(resolved.requested.as_audit_value())
        .with_value_requested_text(resolved.requested.as_audit_text())
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

    // gate 7: 値変換(元の位置 - gate 5/6 の後)。
    let converted = convert_value(&resolved.entry, resolved.requested)?;

    // tag_row の読み込みも元の位置(gate 7 の後・gate 8 の前)に戻す。
    let tag_row = TagService::new(deps.manager.pool())
        .get(resolved.tag_id)
        .await
        .map_err(map_registry_error)?;

    // gate 8: log-before-write(PLC/internal 共通 - 実行前に必ず監査行を
    // 先に作る、§6-3)。T20 ①a: 文字列書き込みは `value_requested` を NULL
    // にする(`ConvertedValue::as_audit_value`のdoc comment参照 - このモジュール
    // の doc comment 「気づいた設計上の問題」にも既知の限界として記載)。
    let pending_row = WriteAuditRow::new(
        ctx.id,
        ctx.name.clone(),
        resolved.tag_id,
        resolved.tag_name.clone(),
        WriteAuditAction::Write,
        WriteAuditResult::Ok,
    )
    .with_value_requested_opt(converted.as_audit_value())
    .with_value_requested_text(converted.as_audit_text());
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
        limiter.record(resolved.tag_id, now);
    }

    // T20 ①a(案A): string は常に PLC タグ([`ConvertedValue`]のdoc comment
    // 参照)なので internal 分岐には絶対に来ない - 到達したら防御的に
    // `Internal` を返す(banto-tags の登録時検証が緩んだ場合の保険)。
    let outcome = match &converted {
        ConvertedValue::Numeric(data_type, value) => {
            if let Some(conn) = &resolved.conn {
                write_plc_tag(deps, conn, &resolved.entry, *data_type, &tag_row, *value).await
            } else {
                write_internal_tag(
                    deps,
                    &resolved.entry,
                    resolved.tag_id,
                    *data_type,
                    tag_row.retain,
                    *value,
                )
                .await
            }
        }
        ConvertedValue::Str(text) => match &resolved.conn {
            Some(conn) => write_plc_string_tag(deps, conn, &resolved.entry, &tag_row, text).await,
            None => Err(WriteRejection::Internal(
                "internal タグへの文字列書き込みが解決されました(想定外)".to_string(),
            )),
        },
    };

    let final_result = match &outcome {
        Ok(()) => WriteAuditResult::Ok,
        Err(_) => WriteAuditResult::Failed,
    };
    // T8-2 (docs/tag-server-design.md §6.1, 2026-08-06): carry the
    // rejection's detail (e.g. `PlcWriteError::BitWriteVerificationFailed`'s
    // 「書き戻し競合の可能性があります」for a T8 RMW confirmation-read
    // mismatch) into the confirmed audit row, reusing exactly the same
    // `WriteRejection::detail()` the REST/gRPC response bodies already show
    // the caller - so the write_audit record and the failed response always
    // agree on why. `Ok(())` has no detail (`None`), matching every other
    // successful write's audit row before this change.
    let failure_detail = outcome.as_ref().err().and_then(WriteRejection::detail);
    if let Err(err) = deps
        .write_audit
        .set_result(audit_id, final_result, failure_detail.as_deref())
        .await
    {
        eprintln!("banto-hub: 書き込み監査の確定に失敗しました: {err}");
    }

    outcome.map(|()| WriteOk {
        tag: resolved.tag_name,
    })
}

/// 1エントリぶんのバッチ書き込み結果([`execute_write_batch`]の要素)。
/// `tag` は呼び出し元がそのままエコーバックできるよう外部名を持つ
/// ([`WriteOk`]と同じ理由)。
#[derive(Debug, Clone, PartialEq)]
pub struct BatchEntryOutcome {
    pub tag: String,
    pub result: Result<(), WriteRejection>,
}

/// レシピ一括書き込み(T20-3a、docs/banto-hub-t20-design.md §3.3、
/// 2026-09-04 オーナー承認)の本体。**単票 [`execute_write`] とゲート実装を
/// 共有する**([`resolve_write_target`]・[`convert_value`])- ゲートを
/// 迂回しない(設計 §3.7)。
///
/// ## 契約(オーナー承認、勝手に変えない)
///
/// 0. **重複タグは全体拒否**(2026-09-05 監査対応): 同じバッチ内に同じ
///    外部名が2回以上現れたら、DB へは一切触れずに全エントリを拒否する
///    ([`WriteRejection::DuplicateTagInBatch`])。レシピで同一タグに2つの
///    値を書くのは曖昧(どちらが最終値か不定)なユーザー誤りであり、かつ
///    これを禁止すると「1バッチ内で同じタグの `record` が複数回起きる」
///    ケースが構造的に無くなるため、gate 6(レート制限)peek の粒度問題
///    (後述)も解消される。
/// 1. **collection-running チェックは1回**(バッチ全体で)。停止中なら
///    全エントリを [`WriteRejection::CollectionNotRunning`] にして返す -
///    1件も書かない。
/// 2. **事前ゲートは all-or-nothing**: 全エントリに
///    [`resolve_write_target`](gate 1〜4・値型 present)→ [`convert_value`]
///    (gate 7)を通す(バッチは単票と違い、この2つを gate 5/6 より前に
///    行ってよい - [`resolve_write_target`]のdoc comment参照)。**1件でも
///    Err なら、実書き込みを一切せず**(監査 insert も broker 呼び出しも
///    発生しない)、Err だったエントリはその [`WriteRejection`]、そうで
///    なかったエントリは [`WriteRejection::BatchAborted`] として返す -
///    「事前検証は all-or-nothing」の結果を呼び出し元(REST/MCP)が
///    「全体 NG・無書込」と表現できるようにするための専用バリアント。
/// 3. **gate 5(write_control off)は全エントリで1回だけ判定**: off なら
///    全エントリを拒否する。各エントリに単票と同じ流儀で
///    `suppressed_disabled` 監査行を残す(1件も PLC へ書かない)。
/// 4. **gate 6(レート制限)は全エントリを先に peek**(peek だけ、この時点
///    では `record` しない): 1件でも would_exceed なら API キーを1回だけ
///    trip し、**全エントリを拒否**する(would_exceed だった当該エントリ
///    にのみ `rate_limit_tripped` 監査行を残す - 単票が「その1件が
///    トリップの原因になった」ときだけ監査するのと同じ意味論をバッチへ
///    一般化した)。項目0の重複禁止によりバッチ内で同じ tag_id が複数回
///    peek されることは無いので、この peek は単票の逐次呼び出しと同じ
///    精度を保つ。
/// 5. **gate 8(commit)は同一接続=1ジョブ、`record` は per-entry**:
///    `tag_row` の読み込みは単票と同じ位置(gate 7 の後・commit 直前)で
///    エントリ毎に行う。**`record` も単票と同じタイミング** - 各エントリの
///    `insert_pending` 監査が成功した**直後**(物理書き込みを試みる**前**)
///    に、そのエントリの分だけ `record` する(2026-09-05 監査対応: 以前は
///    commit フェーズに入る前に全エントリをまとめて record していたが、
///    `insert_pending` 失敗や(PLC タグの場合)`tag_row` 読み込み失敗・
///    [`build_plc_write_request`] の拒否で実際には書き込みを試みない
///    エントリまで消費してしまう false rate limiting だった -
///    `crate::write_rate` の「record は実際に試みた書き込みのみ消費する」
///    契約に反していたため是正した)。internal タグは単票と同じ
///    `write_internal_tag` を1件ずつ、PLC タグは接続 id ごとにグルーピング
///    し、グループ内で `insert_pending` 成功のたびに record したエントリ
///    だけを集めて [`build_plc_write_request`] で組み立てた
///    `Vec<BatchWriteRequest>` を `BrokerHandle::write` へ**1回**渡す →
///    返ってきた `Vec<WriteResult>`(入力順)を各エントリへ割り当てて
///    `set_result` で確定する。broker セッションが無い接続は、そのグループ
///    の全エントリを単票と同じ fail-closed(`WriteFailed`)にする(この
///    エントリ達はすでに record 済み - 単票が broker 呼び出し失敗時も
///    record 済みのまま戻すのと同じ)。
/// 6. **per-entry 結果を入力順で返す**。
pub async fn execute_write_batch(
    deps: &WriteDeps<'_>,
    ctx: &ApiKeyContext,
    entries: Vec<(String, Option<RequestedValue>)>,
) -> Vec<BatchEntryOutcome> {
    // 0. 重複タグ検出(2026-09-05 監査対応、このモジュール doc comment
    // 「契約」の0番参照)。DB にもレート制限にも一切触れない、最も安価な
    // 事前ゲート - 同じ外部名が2回以上現れたら全エントリを拒否する。
    // 重複していたタグには理由を特定できる
    // [`WriteRejection::DuplicateTagInBatch`]、それ以外は
    // [`WriteRejection::BatchAborted`](項目2の all-or-nothing と同じ表現)。
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    // Owned (not borrowed) so it does not keep `entries` borrowed once we
    // need to move it in the `return` below.
    let mut duplicated: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (tag, _) in &entries {
        if !seen.insert(tag.as_str()) {
            duplicated.insert(tag.clone());
        }
    }
    if !duplicated.is_empty() {
        return entries
            .into_iter()
            .map(|(tag, _)| {
                let result = if duplicated.contains(&tag) {
                    Err(WriteRejection::DuplicateTagInBatch(tag.clone()))
                } else {
                    Err(WriteRejection::BatchAborted)
                };
                BatchEntryOutcome { tag, result }
            })
            .collect();
    }

    // 1. collection-running チェック(バッチ全体で1回)。
    let collection_mode = if let Some(controller) = deps.collection_controller {
        let status = controller.status();
        let state = status.state;
        if state != CollectionState::Running {
            return entries
                .into_iter()
                .map(|(tag, _)| BatchEntryOutcome {
                    tag,
                    result: Err(WriteRejection::CollectionNotRunning(state)),
                })
                .collect();
        }
        Some(status.mode)
    } else {
        None
    };

    // 2. 事前ゲート all-or-nothing: 全エントリを prepare_batch_entry
    // (resolve_write_target → convert_value)へ通す。
    let mut prepare_results = Vec::with_capacity(entries.len());
    for (tag, requested) in &entries {
        prepare_results
            .push(prepare_batch_entry(deps, collection_mode, tag, requested.clone()).await);
    }

    if prepare_results.iter().any(Result::is_err) {
        // 1件でも NG なら実書き込みを一切しない - 監査行も broker 呼び出し
        // もまだ発生していない(prepare_batch_entry は副作用が無い)ので、
        // ここで返すだけで「1件も書かない」が成立する。
        return entries
            .into_iter()
            .zip(prepare_results)
            .map(|((tag, _), prepared)| {
                let result = match prepared {
                    Err(rejection) => Err(rejection),
                    Ok(_) => Err(WriteRejection::BatchAborted),
                };
                BatchEntryOutcome { tag, result }
            })
            .collect();
    }
    let prepared: Vec<PreparedWrite> = prepare_results
        .into_iter()
        .map(|r| r.expect("checked above: no Err remains"))
        .collect();

    // 3. gate 5: write_control off なら全エントリ拒否(単票と同じ監査行)。
    if !deps.write_control.is_enabled() {
        for p in &prepared {
            let row = WriteAuditRow::new(
                ctx.id,
                ctx.name.clone(),
                p.tag_id,
                p.tag_name.clone(),
                WriteAuditAction::Write,
                WriteAuditResult::SuppressedDisabled,
            )
            .with_value_requested_opt(p.value.as_audit_value())
            .with_value_requested_text(p.value.as_audit_text());
            if let Err(err) = deps.write_audit.insert_row(&row).await {
                eprintln!(
                    "banto-hub: 書き込み監査(suppressed_disabled、バッチ)の記録に失敗しました: {err}"
                );
            }
        }
        return prepared
            .into_iter()
            .map(|p| BatchEntryOutcome {
                tag: p.tag_name,
                result: Err(WriteRejection::WritesDisabled),
            })
            .collect();
    }

    // 4. gate 6: 全エントリの tag を先に peek する(record はまだしない)。
    let now = Instant::now();
    let exceeded: Vec<bool> = {
        let mut limiter = deps.rate_limiter.lock().await;
        prepared
            .iter()
            .map(|p| limiter.would_exceed(p.tag_id, now))
            .collect()
    };
    if exceeded.iter().any(|&x| x) {
        let trip_result = deps.api_keys.trip(ctx.id).await;
        let detail = match trip_result {
            Ok(_) => "レート制限を超過したため API キーをトリップしました".to_string(),
            Err(err) => {
                format!("レート制限を超過しましたが、API キーのトリップに失敗しました: {err}")
            }
        };
        for (p, &was_exceeded) in prepared.iter().zip(&exceeded) {
            if !was_exceeded {
                continue;
            }
            let row = WriteAuditRow::new(
                ctx.id,
                ctx.name.clone(),
                p.tag_id,
                p.tag_name.clone(),
                WriteAuditAction::RateLimitTripped,
                WriteAuditResult::SuppressedRateLimited,
            )
            .with_value_requested_opt(p.value.as_audit_value())
            .with_value_requested_text(p.value.as_audit_text())
            .with_detail(detail.clone());
            if let Err(err) = deps.write_audit.insert_row(&row).await {
                eprintln!(
                    "banto-hub: 書き込み監査(rate_limit_tripped、バッチ)の記録に失敗しました: {err}"
                );
            }
        }
        let _ = deps.events.send(ServerEvent::Notice {
            level: "warning".to_string(),
            message: format!(
                "書き込みレート制限を超過したため API キー '{}' をトリップしました(バッチ書き込み)",
                ctx.name
            ),
        });
        let _ = deps.events.send(ServerEvent::ResourceChanged {
            resource: "api_keys".to_string(),
        });
        return prepared
            .into_iter()
            .map(|p| BatchEntryOutcome {
                tag: p.tag_name,
                result: Err(WriteRejection::RateLimited),
            })
            .collect();
    }
    // 全件 peek OK。**record はここではまだ行わない**(2026-09-05 監査
    // 対応) - 以前はここで全エントリをまとめて record していたが、commit
    // フェーズで実際には書き込みを試みずに終わるエントリ(`tag_row` 読み
    // 込み失敗・`insert_pending` 失敗・[`build_plc_write_request`] の拒否)
    // まで消費してしまう false rate limiting だった。record は単票と同じ
    // タイミング(各エントリの `insert_pending` 成功直後・物理書き込みの
    // 直前)で、commit ループ内から per-entry に呼ぶ(このモジュール doc
    // comment「契約」の5番参照)。

    // 5. gate 8 commit: internal は1件ずつ、PLC は接続 id ごとにグルーピング。
    let mut outcomes: Vec<Option<BatchEntryOutcome>> = vec![None; prepared.len()];
    let mut plc_groups: HashMap<i64, Vec<usize>> = HashMap::new();
    let mut internal_indices: Vec<usize> = Vec::new();
    for (i, p) in prepared.iter().enumerate() {
        match &p.conn {
            Some(conn) => plc_groups.entry(conn.id).or_default().push(i),
            None => internal_indices.push(i),
        }
    }

    for i in internal_indices {
        let p = &prepared[i];
        // tag_row の読み込みは単票と同じ位置(gate 7 の後・insert_pending の
        // 前)- ここで読む。読み込みに失敗した場合は単票と同じく監査行を
        // 一切残さず、このエントリだけ Internal を返す(他のエントリの
        // commit は続行する)。
        let tag_row = match TagService::new(deps.manager.pool()).get(p.tag_id).await {
            Ok(row) => row,
            Err(err) => {
                outcomes[i] = Some(BatchEntryOutcome {
                    tag: p.tag_name.clone(),
                    result: Err(map_registry_error(err)),
                });
                continue;
            }
        };
        // T20 ①a(案A): internal タグに string は絶対に来ない
        // (`ConvertedValue`のdoc comment参照 - banto-tags の登録時検証が
        // `data_type = "string"` を internal/computed タグへ拒否している)。
        // 到達したら防御的に Internal を返す。
        let (data_type, value) = match p.value {
            ConvertedValue::Numeric(data_type, value) => (data_type, value),
            ConvertedValue::Str(_) => {
                outcomes[i] = Some(BatchEntryOutcome {
                    tag: p.tag_name.clone(),
                    result: Err(WriteRejection::Internal(
                        "internal タグへの文字列書き込みが解決されました(想定外)".to_string(),
                    )),
                });
                continue;
            }
        };
        let pending_row = WriteAuditRow::new(
            ctx.id,
            ctx.name.clone(),
            p.tag_id,
            p.tag_name.clone(),
            WriteAuditAction::Write,
            WriteAuditResult::Ok,
        )
        .with_value_requested(value);
        let audit_id = match deps.write_audit.insert_pending(&pending_row).await {
            Ok(id) => id,
            Err(err) => {
                eprintln!(
                    "banto-hub: 書き込み監査(log-before-write、バッチ internal)の記録に失敗しました: {err}"
                );
                outcomes[i] = Some(BatchEntryOutcome {
                    tag: p.tag_name.clone(),
                    result: Err(WriteRejection::AuditWriteFailed),
                });
                continue;
            }
        };
        // record は単票と同じタイミング: insert_pending 成功直後・
        // 物理書き込みの直前(このモジュール doc comment「契約」の5番参照)。
        {
            let mut limiter = deps.rate_limiter.lock().await;
            limiter.record(p.tag_id, now);
        }
        let outcome =
            write_internal_tag(deps, &p.entry, p.tag_id, data_type, tag_row.retain, value).await;
        let final_result = match &outcome {
            Ok(()) => WriteAuditResult::Ok,
            Err(_) => WriteAuditResult::Failed,
        };
        let failure_detail = outcome.as_ref().err().and_then(WriteRejection::detail);
        if let Err(err) = deps
            .write_audit
            .set_result(audit_id, final_result, failure_detail.as_deref())
            .await
        {
            eprintln!("banto-hub: 書き込み監査の確定(バッチ internal)に失敗しました: {err}");
        }
        outcomes[i] = Some(BatchEntryOutcome {
            tag: p.tag_name.clone(),
            result: outcome,
        });
    }

    for (connection_id, group_indices) in plc_groups {
        // tag_row の読み込み(単票と同じ位置)→ log-before-write: グループの
        // 全エントリに先に pending 監査行を作る(単票と同じ順序 - broker を
        // 呼ぶ前に必ず先に作る、§6-3)。tag_row の読み込みに失敗した
        // エントリは監査行を残さず Internal(このエントリだけ、グループの
        // 他のエントリは続行)。
        let mut committed: Vec<(usize, banto_tags::Tag, i64)> =
            Vec::with_capacity(group_indices.len());
        for &i in &group_indices {
            let p = &prepared[i];
            let tag_row = match TagService::new(deps.manager.pool()).get(p.tag_id).await {
                Ok(row) => row,
                Err(err) => {
                    outcomes[i] = Some(BatchEntryOutcome {
                        tag: p.tag_name.clone(),
                        result: Err(map_registry_error(err)),
                    });
                    continue;
                }
            };
            let pending_row = WriteAuditRow::new(
                ctx.id,
                ctx.name.clone(),
                p.tag_id,
                p.tag_name.clone(),
                WriteAuditAction::Write,
                WriteAuditResult::Ok,
            )
            .with_value_requested_opt(p.value.as_audit_value())
            .with_value_requested_text(p.value.as_audit_text());
            match deps.write_audit.insert_pending(&pending_row).await {
                Ok(audit_id) => {
                    // record は単票と同じタイミング: insert_pending 成功
                    // 直後・物理書き込みの直前(このモジュール doc comment
                    // 「契約」の5番参照) - グループ全体の handle.write では
                    // なく、このエントリの insert_pending が成功した時点で
                    // 個別に record する。
                    {
                        let mut limiter = deps.rate_limiter.lock().await;
                        limiter.record(p.tag_id, now);
                    }
                    committed.push((i, tag_row, audit_id));
                }
                Err(err) => {
                    eprintln!(
                        "banto-hub: 書き込み監査(log-before-write、バッチ PLC)の記録に失敗しました: {err}"
                    );
                    outcomes[i] = Some(BatchEntryOutcome {
                        tag: p.tag_name.clone(),
                        result: Err(WriteRejection::AuditWriteFailed),
                    });
                }
            }
        }

        // グループの BatchWriteRequest を組み立てる(単票と共有する
        // build_plc_write_request)。リクエスト自体が組み立てられない
        // (範囲外・アドレス不正)エントリは broker には送らず、ここで
        // 結果を確定する。
        let mut requests = Vec::with_capacity(committed.len());
        let mut sent_indices = Vec::with_capacity(committed.len());
        for (i, tag_row, audit_id) in committed {
            let p = &prepared[i];
            let conn = p.conn.as_ref().expect("group is keyed by connection id");
            let request = match &p.value {
                ConvertedValue::Numeric(data_type, value) => {
                    build_plc_write_request(conn, &p.entry, *data_type, &tag_row, *value)
                }
                ConvertedValue::Str(text) => {
                    build_plc_string_write_request(conn, &p.entry, &tag_row, text)
                }
            };
            match request {
                Ok(request) => {
                    requests.push(request);
                    sent_indices.push((i, audit_id));
                }
                Err(rejection) => {
                    if let Err(err) = deps
                        .write_audit
                        .set_result(
                            audit_id,
                            WriteAuditResult::Failed,
                            rejection.detail().as_deref(),
                        )
                        .await
                    {
                        eprintln!("banto-hub: 書き込み監査の確定(バッチ PLC)に失敗しました: {err}");
                    }
                    outcomes[i] = Some(BatchEntryOutcome {
                        tag: p.tag_name.clone(),
                        result: Err(rejection),
                    });
                }
            }
        }

        if sent_indices.is_empty() {
            continue;
        }

        // T15-4: 単票と同じ non-spawning peek のみを使う(このモジュール
        // doc comment「gate 8 は broker セッションを新規に張らない」節参照)。
        let Some(handle) = deps.manager.write_broker_handle_peek(connection_id) else {
            for (i, audit_id) in sent_indices {
                let rejection = WriteRejection::WriteFailed(
                    "PLC への接続セッションがありません(書き込みは新しいセッションを開始しません。収集が稼働中か確認してください)"
                        .to_string(),
                );
                if let Err(err) = deps
                    .write_audit
                    .set_result(
                        audit_id,
                        WriteAuditResult::Failed,
                        rejection.detail().as_deref(),
                    )
                    .await
                {
                    eprintln!("banto-hub: 書き込み監査の確定(バッチ PLC)に失敗しました: {err}");
                }
                outcomes[i] = Some(BatchEntryOutcome {
                    tag: prepared[i].tag_name.clone(),
                    result: Err(rejection),
                });
            }
            continue;
        };

        // 同一接続は1ジョブ(§3.3 の核心) - `requests` を丸ごと1回
        // `handle.write` へ渡す。
        match handle.write(requests).await {
            Ok(results) => {
                for (k, (i, audit_id)) in sent_indices.into_iter().enumerate() {
                    let outcome = match results.get(k) {
                        Some(PlcWriteResult::Ok) => Ok(()),
                        Some(PlcWriteResult::Bad(write_err)) => {
                            Err(WriteRejection::WriteFailed(write_err.to_string()))
                        }
                        None => Err(WriteRejection::WriteFailed(
                            "broker から応答がありませんでした".to_string(),
                        )),
                    };
                    let final_result = match &outcome {
                        Ok(()) => WriteAuditResult::Ok,
                        Err(_) => WriteAuditResult::Failed,
                    };
                    let failure_detail = outcome.as_ref().err().and_then(WriteRejection::detail);
                    if let Err(err) = deps
                        .write_audit
                        .set_result(audit_id, final_result, failure_detail.as_deref())
                        .await
                    {
                        eprintln!("banto-hub: 書き込み監査の確定(バッチ PLC)に失敗しました: {err}");
                    }
                    outcomes[i] = Some(BatchEntryOutcome {
                        tag: prepared[i].tag_name.clone(),
                        result: outcome,
                    });
                }
            }
            Err(err) => {
                // グループ全体が broker 呼び出し自体で失敗(単票の
                // `handle.write` の `Err(err)` 分岐と同じ扱い)。
                for (i, audit_id) in sent_indices {
                    let rejection = WriteRejection::WriteFailed(err.to_string());
                    if let Err(set_err) = deps
                        .write_audit
                        .set_result(
                            audit_id,
                            WriteAuditResult::Failed,
                            rejection.detail().as_deref(),
                        )
                        .await
                    {
                        eprintln!(
                            "banto-hub: 書き込み監査の確定(バッチ PLC)に失敗しました: {set_err}"
                        );
                    }
                    outcomes[i] = Some(BatchEntryOutcome {
                        tag: prepared[i].tag_name.clone(),
                        result: Err(rejection),
                    });
                }
            }
        }
    }

    // 6. per-entry 結果を入力順で返す。
    outcomes
        .into_iter()
        .map(|o| o.expect("every index was assigned an outcome above"))
        .collect()
}

/// gate 8 の PLC タグ分岐: 従来どおり `banto_tags::unscale` → プロトコル別
/// アドレス解決(#131 以降 SLMP/Modbus TCP の両方 - `banto_collect`の
/// `build_request`と同じ`conn.protocol`分岐)→ `BrokerHandle::write`。
/// `execute_write` から抽出しただけで
/// 挙動は変えていない(T6-2 前の唯一の書き込み経路そのもの)。
///
/// T15-4(このモジュールの doc comment「gate 8 は broker セッションを新規に
/// 張らない」節参照): broker handle の取得は non-spawning peek
/// (`CollectorManager::write_broker_handle_peek`)のみを使う - セッションが
/// 既に無ければ(≒ 収集停止と競合してセッションが落ちていた)、新規に
/// ダイヤルせずそのまま [`WriteRejection::WriteFailed`] を返す。
async fn write_plc_tag(
    deps: &WriteDeps<'_>,
    conn: &banto_tags::PlcConnection,
    entry: &crate::hub::TagEntry,
    data_type: DataType,
    tag_row: &banto_tags::Tag,
    requested: f64,
) -> Result<(), WriteRejection> {
    let request = build_plc_write_request(conn, entry, data_type, tag_row, requested)?;

    let Some(handle) = deps.manager.write_broker_handle_peek(conn.id) else {
        // T15-4: セッションが無い(収集停止・`stop_and_join`との競合など) -
        // 新規に実機へダイヤルしてはならないので fail closed する。
        return Err(WriteRejection::WriteFailed(
            "PLC への接続セッションがありません(書き込みは新しいセッションを開始しません。収集が稼働中か確認してください)"
                .to_string(),
        ));
    };

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

/// gate 8 の PLC タグ分岐(数値/bit)が broker へ渡す `BatchWriteRequest` の
/// 組み立て(単票 [`write_plc_tag`] とバッチ [`execute_write_batch`] の
/// コミット段が**共有する**唯一の場所 - T20-3a、docs/banto-hub-t20-design.md
/// §3.3「BatchWriteRequest 構築ロジックを単票と共有する」)。従来どおり
/// `banto_tags::unscale` → プロトコル別アドレス解決(#131 以降 SLMP/Modbus
/// TCP の両方 - `banto_collect`の `build_request`と同じ`conn.protocol`分岐)
/// → `BatchWriteRequest`(`Numeric`/`BitInWord` の作り分け)。**副作用は
/// 無い**(broker へは渡さない - 呼び出し元が `handle.write` を呼ぶ)。
///
/// **T20 ①a**: `entry.data_type == STRING_DATA_TYPE` は [`convert_value`]
/// の gate 7 で `ConvertedValue::Str` に変換されるため、この関数の呼び出し元
/// は string タグに対してこの関数を呼ばない([`build_plc_string_write_request`]
/// を代わりに呼ぶ) - この関数自身が `BatchWriteRequest::String` を組み立てる
/// ことは無い。
fn build_plc_write_request(
    conn: &banto_tags::PlcConnection,
    entry: &crate::hub::TagEntry,
    data_type: DataType,
    tag_row: &banto_tags::Tag,
    requested: f64,
) -> Result<BatchWriteRequest, WriteRejection> {
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

    // #131 (2026-09-01): dispatch on `conn.protocol` exactly like
    // `crates/banto-collect/src/config.rs`'s `build_request` does for reads -
    // gate 4 (`resolve_write_target`, above) no longer restricts this
    // function to SLMP connections alone, so the address notation must be
    // parsed with the matching protocol's parser.
    let address = match conn.protocol.as_str() {
        "modbus-tcp" => Address::parse(&entry.address),
        // "slmp" and (defensively) anything else the protocol gate above
        // already restricted to a broker-registered protocol.
        _ => Address::parse_slmp(&entry.address),
    };
    let address = match address {
        Ok(address) => address,
        Err(err) => {
            // catalog のアドレスは登録時に banto-tags で検証済みのはずの
            // 防御的分岐(gate 4 で broker が対応するプロトコルの接続だけに
            // 絞られているので、ここに来る時点でアドレスは対応プロトコルの
            // 表記のはず)。
            return Err(WriteRejection::InvalidAddress(err.to_string()));
        }
    };

    // T8-2 (docs/tag-server-design.md §6.1, 2026-08-06): a bit-in-word
    // address (`Address::Slmp { bit: Some(_), .. }`, i.e. the catalog
    // address was registered as `"D100.5"`) goes through the driver's RMW
    // path (`BatchWriteRequest::BitInWord`) instead of an ordinary word
    // write - `banto-collect`'s `build_config` (crates/banto-collect/src/
    // config.rs's `build_request`) already refuses to build a config where a
    // bit-qualified address is paired with a non-`bit` `data_type`, and that
    // refusal keeps the whole catalog (including this write path's `entry`)
    // on its previous state (`CollectorManager::rebuild`'s all-or-nothing
    // commit, `apps/banto-hub/core/src/hub.rs`'s module doc), so reaching
    // here with `data_type == DataType::Bit` false and a bit-qualified
    // address is not reachable in practice - the `matches!` below only ever
    // fires for `DataType::Bit`, but is written independently of `data_type`
    // for the same defense-in-depth reason `Address::parse_slmp`'s error
    // above is handled rather than `unwrap`ped. A genuine bit-*device*
    // address (`"M50"`, no `.N` suffix) still takes the `Numeric` branch
    // exactly as before T8 - only a word device's bit-in-word notation does.
    //
    // #131 (2026-09-01): this `matches!` is on the `Address::Slmp` variant
    // specifically, so a Modbus tag's `address` (always `Address::ModbusRef`
    // - see the protocol dispatch above) can never match it and always takes
    // the `Numeric` branch below, unconditionally - a Modbus bit-in-word
    // write is out of scope for this slice (`banto_broker`'s Modbus driver
    // only ever receives `BatchWriteRequest::Numeric`/`String` from this
    // function, never `BitInWord`, for a Modbus connection).
    let is_bit_in_word = matches!(address, Address::Slmp { bit: Some(_), .. });
    if is_bit_in_word {
        let PlcTagValue::Bit(value) = tag_value else {
            // Unreachable: `is_bit_in_word` can only be true when `address`
            // parsed a `.N` suffix, which `Address::parse_slmp`
            // (`banto-plc`'s `slmp::address::parse`) only accepts on a word
            // device - and this write path's `data_type` for a word-device
            // tag with such an address is `DataType::Bit` by the config-build
            // guarantee documented just above, which is exactly the branch
            // that produces `PlcTagValue::Bit` a few lines up.
            return Err(WriteRejection::WriteFailed(
                "内部エラー: ビット指定アドレスの値が bool ではありません".to_string(),
            ));
        };
        Ok(BatchWriteRequest::BitInWord { address, value })
    } else {
        Ok(BatchWriteRequest::Numeric(PlcWriteRequest {
            address,
            data_type,
            value: tag_value,
        }))
    }
}

/// gate 8 の文字列タグ分岐(T20 ①a、docs/banto-hub-t20-design.md §3.1、
/// 案A「書き込みは write_path 経由（記録計の read/cache には触れない）」)。
/// 数値タグの [`write_plc_tag`] と並ぶ、string タグ専用の書き込み - broker
/// handle の取得規律(T15-4、non-spawning peek)は完全に同じ。文字列タグには
/// スケーリング・範囲チェック(`validate_numeric_range`)が存在しない
/// (`banto_tags::tag::validate_tag_input` が string タグへの raw/eng
/// 設定自体を登録時に拒否している)。
async fn write_plc_string_tag(
    deps: &WriteDeps<'_>,
    conn: &banto_tags::PlcConnection,
    entry: &crate::hub::TagEntry,
    tag_row: &banto_tags::Tag,
    value: &str,
) -> Result<(), WriteRejection> {
    let request = build_plc_string_write_request(conn, entry, tag_row, value)?;

    let Some(handle) = deps.manager.write_broker_handle_peek(conn.id) else {
        // T15-4: 単票の write_plc_tag と同じ fail-closed(このモジュールの
        // doc comment「gate 8 は broker セッションを新規に張らない」参照)。
        return Err(WriteRejection::WriteFailed(
            "PLC への接続セッションがありません(書き込みは新しいセッションを開始しません。収集が稼働中か確認してください)"
                .to_string(),
        ));
    };

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

/// `banto_tags::Tag::string_encoding` の文字列表現を
/// `banto_plc_write::StringEncoding`(=`banto_plc::StringEncoding` - T20 ①b
/// で banto-plc-write は banto-plc の型を再エクスポートするだけになった、
/// `crate::read_path`のモジュール doc comment参照)へマップする。CHECK 制約
/// (`migrations/0013_tags_add_string_encoding.sql`)により登録時に
/// `"utf8"`/`"shift_jis"` 以外は拒否されているはずなので、それ以外の値は
/// 防御的に既定の UTF-8 へフォールバックする(パニックしない)。`pub(crate)`
/// にしてあるのは、T20 ①b の `crate::read_path::execute_read_now` も同じ
/// マッピングを必要とするため(読み書き対称、二重実装しない)。
pub(crate) fn string_encoding_from_tag(tag_row: &banto_tags::Tag) -> StringEncoding {
    match tag_row.string_encoding.as_str() {
        "shift_jis" => StringEncoding::ShiftJis,
        _ => StringEncoding::Utf8,
    }
}

/// 文字列タグの `BatchWriteRequest` 組み立て(単票 [`write_plc_string_tag`]
/// とバッチのコミット段が共有する - 数値の [`build_plc_write_request`] と
/// 対になる、同じ「共有する唯一の場所」原則)。`tag_row.string_length` から
/// ワード数を、`tag_row.string_encoding` からエンコーディングを取り、
/// `banto_plc_write::StringWriteRequest` を組み立てる。**副作用は無い**
/// (broker へは渡さない)。
///
/// 文字列長チェック(422 相当、実装指示「文字列長は string_length を尊重
/// （超過は 422 相当の範囲エラー）」): `banto_plc_write::encode_string_value`
/// を直接呼んで(ここは同一クレート内なので `pub(crate)` ではなく `pub` に
/// 昇格させた実体そのものを呼ぶ - 数値側の `validate_numeric_range` のように
/// 独立re-implementationを持たない、ロジックが drift しようがない)、ワイヤに
/// 渡す前に容量オーバー・表現不能文字を検出する。broker/ドライバ内部でも
/// 同じ関数がもう一度呼ばれる(二重チェック)が、ここでの事前チェックには
/// 「範囲外の値を write_audit に一切残さず(gate 8 の log-before-write に
/// 到達する前に)弾ける」という数値側と同じ利点がある。
fn build_plc_string_write_request(
    conn: &banto_tags::PlcConnection,
    entry: &crate::hub::TagEntry,
    tag_row: &banto_tags::Tag,
    value: &str,
) -> Result<BatchWriteRequest, WriteRejection> {
    // #131 と同じプロトコル別アドレス解決([`build_plc_write_request`]参照)。
    // Modbus には文字列デバイスの対応が無い(`banto_plc_write`の
    // `modbus::planning`のモジュール doc 参照) - アドレス自体は解決できても、
    // broker 側で `PlcWriteError::UnsupportedRequestKind` として per-request
    // Bad になる(`WriteRejection::WriteFailed`として返る)。
    let address = match conn.protocol.as_str() {
        "modbus-tcp" => Address::parse(&entry.address),
        _ => Address::parse_slmp(&entry.address),
    };
    let address = match address {
        Ok(address) => address,
        Err(err) => return Err(WriteRejection::InvalidAddress(err.to_string())),
    };

    let words = tag_row.string_length.unwrap_or(0).clamp(0, u16::MAX as i64) as u16;
    let encoding = string_encoding_from_tag(tag_row);

    if let Err(err) = banto_plc_write::encode_string_value(value, words, encoding) {
        return Err(WriteRejection::ValueOutOfRange(err.to_string()));
    }

    Ok(BatchWriteRequest::String(StringWriteRequest {
        address,
        words,
        value: value.to_string(),
        encoding,
    }))
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
            WriteRejection::CollectionNotRunning(_) => "collection_not_running",
            WriteRejection::SimulationWriteRejected => "simulation_write_rejected",
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
            WriteRejection::BatchAborted => "batch_aborted",
            WriteRejection::DuplicateTagInBatch(_) => "duplicate_tag_in_batch",
        }
    }

    pub fn detail(&self) -> Option<String> {
        match self {
            WriteRejection::UnsupportedValueType(detail) => detail.clone(),
            WriteRejection::ValueOutOfRange(detail)
            | WriteRejection::InvalidAddress(detail)
            | WriteRejection::WriteFailed(detail)
            | WriteRejection::Internal(detail) => Some(detail.clone()),
            WriteRejection::BatchAborted => Some(
                "同じバッチ内の他のエントリがゲートで拒否されたため、書き込みを行いませんでした"
                    .to_string(),
            ),
            WriteRejection::DuplicateTagInBatch(tag) => {
                Some(format!("レシピ内でタグ '{tag}' が重複しています"))
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::CollectionState;

    #[test]
    fn collection_not_running_uses_shared_rest_mapping() {
        let rejection = WriteRejection::CollectionNotRunning(CollectionState::Stopped);

        assert_eq!(rejection.rest_error_code(), "collection_not_running");
        assert_eq!(rejection.detail(), None);
        assert_eq!(
            rejection.to_json(),
            json!({"error": "collection_not_running"})
        );
    }

    #[test]
    fn simulation_write_rejection_has_a_stable_wire_code() {
        let rejection = WriteRejection::SimulationWriteRejected;

        assert_eq!(rejection.rest_error_code(), "simulation_write_rejected");
        assert_eq!(rejection.detail(), None);
        assert_eq!(
            rejection.to_json(),
            json!({"error": "simulation_write_rejected"})
        );
    }

    /// T20 宿題#1: `RequestedValue::Str` は `value_requested`(REAL列)向けの
    /// `as_audit_value` では `None` のままだが、新設した `as_audit_text` は
    /// テキストをそのまま返す(`Num`/`Bool` はその逆)。
    #[test]
    fn requested_value_str_has_no_audit_f64_but_has_audit_text() {
        let requested = RequestedValue::Str("recipe-A".to_string());
        assert_eq!(requested.as_audit_value(), None);
        assert_eq!(requested.as_audit_text(), Some("recipe-A".to_string()));

        let num = RequestedValue::Num(1.5);
        assert_eq!(num.as_audit_value(), Some(1.5));
        assert_eq!(num.as_audit_text(), None);

        let boolean = RequestedValue::Bool(true);
        assert_eq!(boolean.as_audit_value(), Some(1.0));
        assert_eq!(boolean.as_audit_text(), None);
    }

    /// [`ConvertedValue`] 側(gate 7 通過後)も同じ非対称性を持つことの確認。
    #[test]
    fn converted_value_str_has_no_audit_f64_but_has_audit_text() {
        let converted = ConvertedValue::Str("recipe-A".to_string());
        assert_eq!(converted.as_audit_value(), None);
        assert_eq!(converted.as_audit_text(), Some("recipe-A".to_string()));

        let numeric = ConvertedValue::Numeric(DataType::F32, 1.5);
        assert_eq!(numeric.as_audit_value(), Some(1.5));
        assert_eq!(numeric.as_audit_text(), None);
    }
}
