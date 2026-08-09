//! gRPC タグ空間 API (T4、docs/tag-server-design.md §5.4「gRPC（T4）」)。
//!
//! `proto/tagserver/v1/tagserver.proto`(リポジトリルート相対)を
//! `build.rs` が `tonic-prost-build` でビルド時コード生成し
//! (`OUT_DIR`、コミットしない)、この module がそれを
//! [`tagserver_v1`] としてラップする。ポートは REST と分離
//! (既定 50051、設計 §8・[`crate::settings::DEFAULT_GRPC_PORT`])。bind
//! アドレスは既定 `127.0.0.1`(2026-08-08 オーナー決定、
//! docs/improvement-plan.md H3・[`crate::settings::DEFAULT_GRPC_BIND`])
//! で、以前のように無条件で `0.0.0.0`(全インターフェース)へ bind する
//! ことはない([`GrpcServer::apply`] のdoc comment参照)。
//!
//! ## 意味論は REST/WS と完全に同一(設計 §5.4)
//!
//! - **catalog / 現在値**: `crate::hub::{CollectorManager, TagEntry,
//!   effective_sample}` を読むだけで完結する(REST の `v1_tags`/`v1_values`
//!   と同じ土台)。
//! - **購読(`StreamValues`)**: パターン解釈・resolve・on_change diff・
//!   interval クランプは `crate::subscribe_core` を経由し、`crate::stream`
//!   （WebSocket）と完全に共有する。250ms 評価・初期スナップショット必須・
//!   スロットルなしの規律もそこから来る。バックプレッシャは WS の
//!   `mpsc`満杯 → close frame と同じ発想で、送信チャネル（[`STREAM_QUEUE_CAPACITY`]）
//!   が満杯なら黙って送信タスクを終了させる(`try_send` の `Err` でループを
//!   抜ける - gRPC のストリームは「サーバー側が終了する」ことがそのまま
//!   「切断」に相当し、WS の明示的な close frame に相当するものは不要)。
//! - **`config_changed` 相当の通知は送らない**: proto にその専用メッセージを
//!   持たせていない(設計のメッセージスケッチが `TagValue`/`ValueBatch`
//!   中心であるため)。新規タグの出現は WS と同じ仕組み(250ms ごとに
//!   `crate::subscribe_core::resolve` が最新の `TagMap` へ再照合する)で
//!   暗黙に処理される - クライアントは「ワイルドカード購読に新しい
//!   `ValueBatch` が来る」ことでしか構成変更を検知できない点が WS との
//!   唯一の意味論差(WS は加えて `op: "config_changed"` の明示通知も送る)。
//! - **書き込み(`WriteValue`)**: ゲート1〜8の本体は `crate::write_path::execute_write`
//!   に**完全に**委譲する(REST の `POST /api/v1/values/{tag}` と1つの
//!   実装を共有 - 実装指示「二重実装は絶対に不可」)。このモジュールが
//!   独自に持つのは (a) 認証(後述)、(b) `write:{tag}` スコープの完全一致
//!   検査、(c) proto の `oneof num|bool` から
//!   `crate::write_path::RequestedValue` への正規化(型は潰さない - gate 7
//!   が data_type との対称性を検査するため)、
//!   (d) [`write_rejection_status`] による `tonic::Status` への変換、の4つ
//!   だけ。
//!
//! ## 認証(設計 §5.4「認証」)
//!
//! metadata `authorization: Bearer bh_...`(**API キーのみ** - セッション
//! token は gRPC では受けない、設計「機械クライアント専用 IF」)。
//!
//! **実装判断: interceptor ではなく各ハンドラ冒頭**(設計は「tonic の
//! interceptor または各ハンドラ冒頭で」のどちらも許容している)。tonic の
//! `Interceptor` はサービス全体(`TagServiceServer<T>`)に1枚被せる形の
//! ミドルウェアで、`WriteValue` だけスコープ要件が違う(`read` 不要・
//! `write:{tag}` は body(`tag` フィールド)を見ないと判定できない)ことを
//! interceptor 側で分岐するには、`tonic::Request<()>` から呼び出された
//! gRPC メソッド名を取り出す必要があり、REST の
//! `require_tag_space_auth`(パスで判定)より複雑になる。各ハンドラの
//! 冒頭で [`GrpcService::authenticate`] を呼ぶ方式は REST の
//! `v1_write_value`(認証ミドルウェア通過後にハンドラ自身がさらに
//! `write:{tag}` を検査する)と同じ構造で、認証ロジックの分岐点を1箇所
//! （[`GrpcService::authenticate`]）に保ったまま各ハンドラが要件
//! （`read` 必須か否か）だけを渡せる。
//!
//! `Revoked`/`Tripped`/未認証は REST と同じく `crate::audit::AuditLogService`
//! に `denied` を記録する(`origin: "grpc"`)。エラー写像は
//! [`GrpcService::authenticate`] の doc comment参照。
//!
//! ## H10 ③: per-tag read スコープ(Option B、
//! docs/h10-3-read-scope-proposal.md §5・§6、REST/WS と同じ絞り方)
//!
//! `GetCatalog` は絞らない(全タグ・PLC アドレス込みで返す - 案 B の核、
//! `has_any_read` だけを要求)。`ReadValues`/`StreamValues` はそれぞれの
//! ハンドラが [`GrpcService::authenticate`] から受け取った
//! `ApiKeyContext` で `can_read_value` を適用する:
//! `ReadValues` は明示 `tags` にスコープ外を含めば `PERMISSION_DENIED`、
//! 省略(全件)時はスコープ外を黙って除く。`StreamValues` は
//! `crate::subscribe_core::{initial_values, evaluate}` の `scope` 引数に
//! `Some(&ctx)` を渡し、resolve したマッチ集合をスコープで交差させる
//! （`crate::stream` の WebSocket 実装と同じ絞り方 - 同モジュールの doc
//! comment「per-tag read スコープの交差」参照）。

use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use banto_collect::Quality;
use banto_server::ServerEvent;
use serde_json::json;
use tokio::sync::{broadcast, mpsc, Mutex as AsyncMutex};
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::Stream;
use tonic::transport::Server;
use tonic::{Request, Response, Status};

use crate::api_keys::{ApiKeyContext, ApiKeyLookup, ApiKeysService};
use crate::audit::{AuditEntry, AuditLogService};
use crate::computed::ServerTagStore;
use crate::controller::{CollectionController, RunMode};
use crate::hub::{read_current, CollectorManager, TagEntry};
use crate::settings::GrpcSettings;
use crate::subscribe_core::{
    self, interval_floor_ms, Mode, Subscription, TagPattern, EVAL_TICK_MS,
};
use crate::write_audit::WriteAuditService;
use crate::write_control::WriteControl;
use crate::write_path::{self, WriteRejection};
use crate::write_rate::WriteRateLimiter;

/// `build.rs`(`tonic-prost-build`)が生成した型一式。proto のパッケージ名
/// `tagserver.v1` をそのままモジュール名にする(prost の既定の命名規則)。
pub mod tagserver_v1 {
    include!(concat!(env!("OUT_DIR"), "/tagserver.v1.rs"));
}

use tagserver_v1::tag_service_server::{TagService as TagServiceTrait, TagServiceServer};
use tagserver_v1::{
    tag_value, write_value_request, Event as ProtoEvent, GetCatalogRequest, GetCatalogResponse,
    Quality as ProtoQuality, ReadValuesRequest, ReadValuesResponse, StreamEventsRequest,
    StreamValuesRequest, SubscribeMode, TagEntry as ProtoTagEntry, TagValue as ProtoTagValue,
    ValueBatch, WriteValueRequest, WriteValueResponse,
};

/// gRPC ストリーム(`StreamValues`/`StreamEvents`)の送信チャネル容量 -
/// `crate::stream::OUTBOUND_QUEUE_CAPACITY` と同じ桁(設計 §5.2 要件6
/// 「送信キュー(mpsc、容量 256 程度)」を gRPC 側にもそのまま適用)。
const STREAM_QUEUE_CAPACITY: usize = 256;

// --- 型変換 -------------------------------------------------------------------

fn to_proto_quality(quality: Quality) -> ProtoQuality {
    match quality {
        Quality::Good => ProtoQuality::Good,
        Quality::Bad => ProtoQuality::Bad,
        Quality::Stale => ProtoQuality::Stale,
    }
}

fn to_proto_tag_entry(entry: &TagEntry) -> ProtoTagEntry {
    let (connection_id, group_id, tag_id) = entry.ids;
    ProtoTagEntry {
        external_name: entry.external_name.clone(),
        connection_id,
        group_id,
        tag_id,
        connection: entry.connection.clone(),
        group: entry.group.clone(),
        name: entry.name.clone(),
        address: entry.address.clone(),
        data_type: entry.data_type.clone(),
        unit: entry.unit.clone(),
        decimals: entry.decimals,
        period_ms: entry.period_ms,
        enabled: entry.enabled,
        writable: entry.writable,
        tag_kind: entry.tag_kind.clone(),
        expression: entry.expression.clone(),
        retain: entry.retain,
    }
}

/// `crate::hub::effective_sample` の結果を [`ProtoTagValue`] へ包む -
/// REST の `crate::rest::value_entry`/WS の `ValueWire::from` と同じ変換点
/// (このモジュールの doc comment「意味論は REST/WS と完全に同一」参照)。
/// `str` oneof は使わない(T4 時点では `CurrentSample::value` が常に
/// `Option<f64>` - proto 自身の doc comment参照)。
fn tag_value_from_sample(
    external_name: &str,
    entry: &TagEntry,
    collect: Option<&banto_collect::CurrentValuesHandle>,
    server_store: &ServerTagStore,
    now_ms: i64,
) -> ProtoTagValue {
    let (v, q, t) = read_current(entry, collect, server_store, now_ms);
    ProtoTagValue {
        tag: external_name.to_string(),
        value: v.map(tag_value::Value::Num),
        quality: to_proto_quality(q) as i32,
        timestamp_ms: t,
    }
}

/// `CollectEvent` の中継(設計「kind/connection(外部名)/tag_key/level/value/
/// detail/ts_ms」)。`crate::stream::send_event`(WS の `EventWire`)と同じ
/// 情報源・同じ「接続キー → 外部名」解決(`crate::stream::resolve_connection_name`
/// を共有 - `banto_collect` の内部キー形式 `"conn:{id}"` は WS/gRPC どちらも
/// 外部名(接続の `name` 列)へ解決してから送る)。見つからなければ
/// (削除された接続の残留イベント等)`None` のまま送る - proto の
/// `optional` フィールドがそのまま「無ければ送らない」を表す。
async fn to_proto_event(
    event: &banto_collect::CollectEvent,
    pool: &sqlx::SqlitePool,
) -> ProtoEvent {
    let connection = match &event.connection_key {
        Some(key) => crate::stream::resolve_connection_name(pool, key).await,
        None => None,
    };
    ProtoEvent {
        kind: event.kind.as_str().to_string(),
        connection,
        tag_key: event.tag_key.clone(),
        level: event.level.map(|l| l.as_str().to_string()),
        value: event.value,
        detail: event.detail.clone(),
        timestamp_ms: event.ts_ms,
    }
}

fn to_proto_value_batch(
    timestamp_ms: i64,
    values: Vec<subscribe_core::ResolvedValue>,
) -> ValueBatch {
    ValueBatch {
        timestamp_ms,
        values: values
            .into_iter()
            .map(|rv| ProtoTagValue {
                tag: rv.tag,
                value: rv.v.map(tag_value::Value::Num),
                quality: to_proto_quality(rv.q) as i32,
                timestamp_ms: rv.t,
            })
            .collect(),
    }
}

fn simulation_output_disabled_status(status: &crate::controller::CollectionStatus) -> bool {
    status.mode == RunMode::AllSimulation
}

fn simulation_output_disabled(controller: Option<&CollectionController>) -> bool {
    controller.is_some_and(|controller| simulation_output_disabled_status(&controller.status()))
}

/// [`crate::write_path::WriteRejection`] を `tonic::Status` へ変換する
/// (設計 §5.4「gRPC 側のエラー写像」)。REST 側の対応物は
/// `crate::rest::write_rejection_response`。コード対応表:
///
/// | REST | gRPC |
/// | --- | --- |
/// | 404 | `NOT_FOUND` |
/// | 403 | `PERMISSION_DENIED` |
/// | 409 | `FAILED_PRECONDITION` |
/// | 422 | `INVALID_ARGUMENT` |
/// | 429 | `RESOURCE_EXHAUSTED` |
/// | 501 | `UNIMPLEMENTED` |
/// | 502 | `UNAVAILABLE` |
/// | 503 | `UNAVAILABLE`(collection_not_running / simulation_write_rejected) / `FAILED_PRECONDITION`(writes_disabled) |
/// | 500(監査書き込み失敗等、防御的分岐) | `INTERNAL` |
///
/// message は常に `"{rest_error_code}: {detail}"`(detail が無ければ
/// `rest_error_code` のみ)- 設計「detail 文字列でコード名を併記」の実現。
fn write_rejection_status(rejection: WriteRejection) -> Status {
    let code = match &rejection {
        WriteRejection::CollectionNotRunning(_) => tonic::Code::Unavailable,
        // Keep the simulation safety gate fail-closed at the transport
        // boundary. It is a stable runtime safety condition, but the REST
        // mapping is 503; UNAVAILABLE keeps the two transports aligned.
        WriteRejection::SimulationWriteRejected => tonic::Code::Unavailable,
        WriteRejection::NotFound => tonic::Code::NotFound,
        WriteRejection::NotWritable => tonic::Code::PermissionDenied,
        WriteRejection::TagDisabled => tonic::Code::FailedPrecondition,
        WriteRejection::UnsupportedProtocol => tonic::Code::Unimplemented,
        WriteRejection::WritesDisabled => tonic::Code::FailedPrecondition,
        WriteRejection::RateLimited => tonic::Code::ResourceExhausted,
        WriteRejection::UnsupportedValueType(_)
        | WriteRejection::ValueOutOfRange(_)
        | WriteRejection::InvalidAddress(_) => tonic::Code::InvalidArgument,
        WriteRejection::WriteFailed(_) => tonic::Code::Unavailable,
        WriteRejection::AuditWriteFailed | WriteRejection::Internal(_) => tonic::Code::Internal,
    };
    let rest_code = rejection.rest_error_code();
    let message = match rejection.detail() {
        Some(detail) => format!("{rest_code}: {detail}"),
        None => rest_code.to_string(),
    };
    Status::new(code, message)
}

// --- 認証 ---------------------------------------------------------------------

/// [`GrpcService::authenticate`] が呼び出し元に求めるスコープ要件。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RequireScope {
    /// `GetCatalog`/`ReadValues`/`StreamValues`/`StreamEvents` - `read`
    /// スコープ必須(設計「ApiKeysService::lookup(read スコープ...)」)。
    Read,
    /// `WriteValue` - `read` は要求しない(`write:{tag}` の完全一致は
    /// ハンドラ自身が別途検査する、REST の `v1_write_value` と同じ構造)。
    None,
}

// --- サービス本体 ---------------------------------------------------------------

/// `TagService`(gRPC)の実装。REST の `TagSpaceState`/`WriteState` を
/// 1つの構造体にまとめたもの - gRPC は1つの `tonic::Request` にどの
/// メソッドかが紐づくため、REST のように読み取り用/書き込み用で状態型を
/// 分ける必要がない。`Clone` は安価(`Arc`/`SqlitePool` バックドの
/// フィールドのみ)。
#[derive(Clone)]
pub struct GrpcService {
    manager: Arc<CollectorManager>,
    collection_controller: Option<Arc<CollectionController>>,
    api_keys: ApiKeysService,
    audit: AuditLogService,
    write_audit: WriteAuditService,
    write_control: Arc<WriteControl>,
    rate_limiter: Arc<AsyncMutex<WriteRateLimiter>>,
    events: broadcast::Sender<ServerEvent>,
}

impl GrpcService {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        manager: Arc<CollectorManager>,
        api_keys: ApiKeysService,
        audit: AuditLogService,
        write_audit: WriteAuditService,
        write_control: Arc<WriteControl>,
        rate_limiter: Arc<AsyncMutex<WriteRateLimiter>>,
        events: broadcast::Sender<ServerEvent>,
    ) -> Self {
        Self {
            manager,
            collection_controller: None,
            api_keys,
            audit,
            write_audit,
            write_control,
            rate_limiter,
            events,
        }
    }

    /// Enable the T14-4 stopped-state write gate for production wiring.
    /// `new` intentionally remains ungated for legacy integration tests and
    /// embedders that construct the service directly.
    pub fn with_controller(mut self, controller: Arc<CollectionController>) -> Self {
        self.collection_controller = Some(controller);
        self
    }

    /// `authorization: Bearer bh_...` メタデータを照合する - このモジュール
    /// の doc comment「認証」参照。
    ///
    /// - metadata が無い、`Bearer ` で始まらない、または `bh_` で始まらない
    ///   (セッション token 相当) → `UNAUTHENTICATED`(設計「セッション
    ///   token は gRPC では受けない」)
    /// - [`ApiKeyLookup::Valid`] → `require == Read` のときのみ `read`
    ///   スコープを要求(無ければ `PERMISSION_DENIED`)。`last_used_at` を
    ///   60秒スロットルで更新(REST と同じ [`crate::api_keys::should_touch_last_used`])
    /// - [`ApiKeyLookup::Revoked`] → `UNAUTHENTICATED` + audit_log 記録
    /// - [`ApiKeyLookup::Tripped`] → `PERMISSION_DENIED` + audit_log 記録
    ///   (read/write いずれも拒否 - REST と同じ規律)
    /// - [`ApiKeyLookup::Expired`]（H10 ①、docs/improvement-plan.md・
    ///   2026-08-08 オーナー決定）→ `UNAUTHENTICATED` + audit_log 記録
    ///   （`Revoked` と同じ扱い - REST の `require_tag_space_auth` と同じ
    ///   規律）
    /// - [`ApiKeyLookup::NotFound`] → `UNAUTHENTICATED`(記録しない - REST
    ///   の `require_tag_space_auth` と同じ理由、偽装キーは「誰が」を
    ///   特定できないノイズ)
    async fn authenticate<T>(
        &self,
        request: &Request<T>,
        require: RequireScope,
    ) -> Result<ApiKeyContext, Status> {
        let token = request
            .metadata()
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "));
        let Some(token) = token else {
            return Err(Status::unauthenticated(
                "authorization メタデータ(Bearer bh_...)が必要です",
            ));
        };
        if !token.starts_with("bh_") {
            return Err(Status::unauthenticated(
                "gRPC は API キー(bh_...)のみ認証できます。セッション token は使えません",
            ));
        }

        // H10 ①: 期限切れ判定 ([`crate::api_keys::ApiKeysService::lookup`])
        // に使う「今」も、last_used_at 更新に使う「今」も同じ
        // `self.manager.clock()` から一度だけ取る(REST の
        // `require_tag_space_auth` と同じ規約)。
        let now_ms = self.manager.clock().now_ms();
        match self.api_keys.lookup(token, now_ms).await {
            Ok(ApiKeyLookup::Valid(ctx)) => {
                // H10 ③(Option B): 「read 系 RPC に入れるか」だけを見る
                // ゲート(has_any_read = 素の read か任意の read:... を1つ
                // でも持つか)。個々のタグの値を読めるか(can_read_value)は
                // ここでは判定しない - `GetCatalog` は絞らず、
                // `ReadValues`/`StreamValues` は各ハンドラが ctx を使って
                // 個別に絞る(`crate::api_keys::ApiKeyContext` の doc
                // comment「read のタグ単位化」参照)。
                if require == RequireScope::Read && !ctx.has_any_read() {
                    return Err(Status::permission_denied("read スコープが必要です"));
                }
                if let Err(err) = self
                    .api_keys
                    .touch_last_used(ctx.id, now_ms, ctx.last_used_at_ms)
                    .await
                {
                    eprintln!("banto-hub: gRPC API キーの last_used_at 更新に失敗しました: {err}");
                }
                Ok(ctx)
            }
            Ok(ApiKeyLookup::Revoked { id, name }) => {
                self.record_denied(id, "revoked", &name).await;
                Err(Status::unauthenticated("この API キーは失効しています"))
            }
            Ok(ApiKeyLookup::Tripped { id, name }) => {
                self.record_denied(id, "tripped", &name).await;
                Err(Status::permission_denied("key_tripped"))
            }
            Ok(ApiKeyLookup::Expired { id, name }) => {
                self.record_denied(id, "expired", &name).await;
                Err(Status::unauthenticated("この API キーは有効期限切れです"))
            }
            Ok(ApiKeyLookup::NotFound) => Err(Status::unauthenticated("無効な API キーです")),
            Err(err) => {
                eprintln!("banto-hub: gRPC 用 API キー照合に失敗しました: {err}");
                Err(Status::internal("認証処理に失敗しました"))
            }
        }
    }

    async fn record_denied(&self, id: i64, reason: &str, name: &str) {
        self.audit
            .record(AuditEntry {
                actor_username: None,
                actor_role: None,
                action: "denied",
                resource: "api_keys",
                entity_id: Some(&id.to_string()),
                detail: Some(json!({ "reason": reason, "name": name, "origin": "grpc" })),
                origin: "grpc",
                result: "denied",
            })
            .await;
    }
}

#[tonic::async_trait]
impl TagServiceTrait for GrpcService {
    async fn get_catalog(
        &self,
        request: Request<GetCatalogRequest>,
    ) -> Result<Response<GetCatalogResponse>, Status> {
        self.authenticate(&request, RequireScope::Read).await?;
        let req = request.into_inner();
        let map = self.manager.tag_map();
        let revision = self.manager.revision();
        let tags = map
            .iter()
            .filter(|entry| req.connection.is_empty() || entry.connection == req.connection)
            .filter(|entry| req.group.is_empty() || entry.group == req.group)
            .map(to_proto_tag_entry)
            .collect();
        Ok(Response::new(GetCatalogResponse { revision, tags }))
    }

    async fn read_values(
        &self,
        request: Request<ReadValuesRequest>,
    ) -> Result<Response<ReadValuesResponse>, Status> {
        let ctx = self.authenticate(&request, RequireScope::Read).await?;
        let req = request.into_inner();
        let map = self.manager.tag_map();
        let revision = self.manager.revision();
        let now_ms = self.manager.clock().now_ms();
        let current = self.manager.current_values();
        let server_store = self.manager.server_store();

        let names: Vec<String> = if req.tags.is_empty() {
            map.iter()
                .map(|entry| entry.external_name.clone())
                .collect()
        } else {
            req.tags.clone()
        };

        if !req.tags.is_empty() {
            let unknown: Vec<&str> = names
                .iter()
                .map(String::as_str)
                .filter(|name| map.get(name).is_none())
                .collect();
            if !unknown.is_empty() {
                // 設計「未知タグは REST 同様 INVALID_ARGUMENT で全体拒否
                // (部分成功で誤解させない)」。
                return Err(Status::invalid_argument(format!(
                    "unknown_tag: {}",
                    unknown.join(", ")
                )));
            }
            // H10 ③(Option B、docs/h10-3-read-scope-proposal.md §5 S4、
            // REST の v1_values `?tags=` と同じ規律): 明示指定でスコープ外
            // を1つでも挙げたら拒否(REST の 403 に対応する gRPC ステータス、
            // `write_rejection_status` の doc comment の対応表参照)。
            if let Some(name) = names.iter().find(|name| !ctx.can_read_value(name)) {
                return Err(Status::permission_denied(format!(
                    "missing_read_scope: {name}"
                )));
            }
        }

        // H10 ③: 暗黙の全件(tags 省略)はスコープ外を黙って除く。明示指定
        // (tags 非空)は直前の分岐でスコープ内であることを確認済みなので、
        // ここでのフィルタは no-op(全件そのまま通る)。
        let names: Vec<String> = names
            .into_iter()
            .filter(|name| ctx.can_read_value(name))
            .collect();

        let values = names
            .iter()
            .filter_map(|name| map.get(name).map(|entry| (name, entry)))
            .map(|(name, entry)| {
                tag_value_from_sample(name, entry, current.as_ref(), &server_store, now_ms)
            })
            .collect();

        Ok(Response::new(ReadValuesResponse {
            revision,
            timestamp_ms: now_ms,
            values,
        }))
    }

    type StreamValuesStream =
        Pin<Box<dyn Stream<Item = Result<ValueBatch, Status>> + Send + 'static>>;

    async fn stream_values(
        &self,
        request: Request<StreamValuesRequest>,
    ) -> Result<Response<Self::StreamValuesStream>, Status> {
        let ctx = self.authenticate(&request, RequireScope::Read).await?;
        let req = request.into_inner();

        if req.tags.is_empty() {
            return Err(Status::invalid_argument("tags が空です"));
        }
        let mut patterns = Vec::with_capacity(req.tags.len());
        for raw in &req.tags {
            match TagPattern::parse(raw) {
                Ok(pattern) => patterns.push(pattern),
                Err(detail) => return Err(Status::invalid_argument(detail)),
            }
        }

        let map = self.manager.tag_map();
        // 設計 §5.2 要件4(WS と同意味論): 未知の具体名が混ざっていたら
        // 購読自体を拒否する。ワイルドカードは0件マッチでもエラーにしない。
        // H10 ③(Option B): この存在チェックは catalog にのみ照らす(絞らない)
        // - per-tag read スコープは resolve 段(下の initial_values/evaluate)
        // でのみ交差させる。`crate::stream::handle_subscribe` と同じ判断
        // (同モジュールの doc comment参照)。
        for pattern in &patterns {
            if let TagPattern::Exact(name) = pattern {
                if map.get(name).is_none() {
                    return Err(Status::invalid_argument(format!("unknown_tag: {name}")));
                }
            }
        }

        let mode = match SubscribeMode::try_from(req.mode).unwrap_or(SubscribeMode::Unspecified) {
            SubscribeMode::Unspecified => {
                return Err(Status::invalid_argument("mode を指定してください"));
            }
            SubscribeMode::OnChange => Mode::OnChange,
            SubscribeMode::Interval => {
                if req.interval_ms <= 0 {
                    return Err(Status::invalid_argument(
                        "mode=INTERVAL には正の interval_ms が必須です",
                    ));
                }
                // クランプ採用(WS と同意味論、`crate::subscribe_core`
                // 参照)。
                let floor = interval_floor_ms(&patterns, &map);
                Mode::Interval {
                    interval_ms: req.interval_ms.max(floor),
                }
            }
        };

        if simulation_output_disabled(self.collection_controller.as_deref()) {
            return Err(Status::unavailable("simulation_output_disabled"));
        }

        let now_ms = self.manager.clock().now_ms();
        let current = self.manager.current_values();
        let server_store = self.manager.server_store();
        // H10 ③: ctx は gRPC では常に Some(API キー必須、セッション token
        // は受けない - このモジュールの doc comment「認証」参照)。素の
        // `read` キーは `can_read_value` が常に true を返すので、ここで
        // `Some(&ctx)` を渡しても無フィルタ相当のまま(後方互換)。
        let (initial, last) = subscribe_core::initial_values(
            &patterns,
            &map,
            current.as_ref(),
            &server_store,
            now_ms,
            Some(&ctx),
        );
        let next_due_ms = match mode {
            Mode::Interval { interval_ms } => now_ms + interval_ms,
            Mode::OnChange => 0,
        };
        let mut subscription = Subscription {
            patterns,
            mode,
            last,
            next_due_ms,
        };

        let (tx, rx) = mpsc::channel(STREAM_QUEUE_CAPACITY);
        // The lifecycle can switch between the initial status check and the
        // first batch. Re-check immediately before exposing the stream so a
        // newly entered all-simulation run does not receive an initial SIM
        // value through the normal gRPC path.
        if simulation_output_disabled(self.collection_controller.as_deref()) {
            return Err(Status::unavailable("simulation_output_disabled"));
        }
        // 設計「初期スナップショット必須」- subscribe 直後に必ず1回送る
        // (空でも)。作ったばかりのチャネルなので `try_send` が失敗する
        // ことは通常ない(防御的に失敗時は素直にストリームを終える)。
        let _ = tx.try_send(Ok(to_proto_value_batch(now_ms, initial)));

        let manager = self.manager.clone();
        let mut runtime_rx = self
            .collection_controller
            .as_ref()
            .map(|controller| controller.subscribe_status());
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_millis(EVAL_TICK_MS as u64));
            tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    _ = tick.tick() => {}
                    changed = async {
                        match runtime_rx.as_mut() {
                            Some(receiver) => receiver.changed().await.map_err(|_| ()),
                            None => std::future::pending::<Result<(), ()>>().await,
                        }
                    } => {
                        if changed.is_err() {
                            break;
                        }
                    }
                }
                if runtime_rx
                    .as_ref()
                    .is_some_and(|receiver| receiver.borrow().mode == RunMode::AllSimulation)
                {
                    break;
                }
                if tx.is_closed() {
                    break;
                }
                let map = manager.tag_map();
                let now_ms = manager.clock().now_ms();
                let current = manager.current_values();
                let server_store = manager.server_store();
                if let Some(values) = subscribe_core::evaluate(
                    &mut subscription,
                    &map,
                    current.as_ref(),
                    &server_store,
                    now_ms,
                    Some(&ctx),
                ) {
                    // 設計「バックプレッシャは送信バッファ満杯で切断」-
                    // `try_send` が `Full`(または受信側 drop 済みの
                    // `Closed`)ならこのタスクを畳む = ストリーム終了。
                    if tx
                        .try_send(Ok(to_proto_value_batch(now_ms, values)))
                        .is_err()
                    {
                        break;
                    }
                }
            }
        });

        Ok(Response::new(Box::pin(ReceiverStream::new(rx))))
    }

    type StreamEventsStream =
        Pin<Box<dyn Stream<Item = Result<ProtoEvent, Status>> + Send + 'static>>;

    async fn stream_events(
        &self,
        request: Request<StreamEventsRequest>,
    ) -> Result<Response<Self::StreamEventsStream>, Status> {
        self.authenticate(&request, RequireScope::Read).await?;

        let mut events_rx = self.manager.subscribe_events();
        let pool = self.manager.pool();
        let (tx, rx) = mpsc::channel(STREAM_QUEUE_CAPACITY);

        tokio::spawn(async move {
            loop {
                match events_rx.recv().await {
                    Ok(event) => {
                        let proto_event = to_proto_event(&event, &pool).await;
                        if tx.try_send(Ok(proto_event)).is_err() {
                            break;
                        }
                    }
                    // WS と同じ「lag はスキップ」(設計 §5.2/§3.5)。
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });

        Ok(Response::new(Box::pin(ReceiverStream::new(rx))))
    }

    async fn write_value(
        &self,
        request: Request<WriteValueRequest>,
    ) -> Result<Response<WriteValueResponse>, Status> {
        let ctx = self.authenticate(&request, RequireScope::None).await?;
        let req = request.into_inner();

        // REST の事前段(b)と同じ規律: write:{tag} の完全一致(設計「write
        // scope 完全一致」)。
        if !ctx.has_write_scope(&req.tag) {
            return Err(Status::permission_denied("missing_write_scope"));
        }

        // `oneof num|bool` を型情報を保ったまま `RequestedValue` へ - REST の
        // `parse_requested_value`(`crate::rest`)と同じく、ここで `f64` へ
        // 潰さない(2026-08-06 変更)。gate 7(`write_path::execute_write`)が
        // data_type との対称性(bit タグには bool のみ、数値タグには数値の
        // み)を検査する。
        let requested = match req.value {
            Some(write_value_request::Value::Num(n)) => Some(write_path::RequestedValue::Num(n)),
            Some(write_value_request::Value::Flag(b)) => Some(write_path::RequestedValue::Bool(b)),
            None => None,
        };

        let deps = write_path::WriteDeps {
            manager: self.manager.as_ref(),
            collection_controller: self.collection_controller.as_deref(),
            api_keys: &self.api_keys,
            write_audit: &self.write_audit,
            write_control: self.write_control.as_ref(),
            rate_limiter: self.rate_limiter.as_ref(),
            events: &self.events,
        };

        match write_path::execute_write(&deps, &ctx, &req.tag, requested).await {
            Ok(ok) => Ok(Response::new(WriteValueResponse {
                tag: ok.tag,
                result: "ok".to_string(),
            })),
            Err(rejection) => Err(write_rejection_status(rejection)),
        }
    }
}

// --- サーバーのライフサイクル管理 ------------------------------------------------

/// gRPC サーバーの起動/停止マネージャ(設計 §5.4 実装指示「管理 UI 設定
/// ページ...保存で即時適用 - MqttPublisher と同じ再起動可能マネージャ
/// パターン」)。`crate::mqtt::MqttPublisher` と同型: 停止状態で構築し、
/// [`GrpcServer::apply`] が(既存タスクを止めてから)`enabled` なら
/// 新しい設定で起動し直す。
///
/// `MqttPublisher` と違い「接続できているか」のライブ状態は持たない -
/// gRPC サーバーは外部へ接続しに行くクライアントではなく、bind して
/// listen するだけなので、`enabled`(設定値)がそのままサーバーの意図した
/// 状態を表す(`GET /api/v1/status` の `grpc: { enabled, port }` は
/// 設定値をそのまま返すだけで足りる、実装指示どおり)。
pub struct GrpcServer {
    service: GrpcService,
    running: AsyncMutex<Option<JoinHandle<()>>>,
}

impl GrpcServer {
    /// 停止状態で構築する - 呼び出し側が続けて [`GrpcServer::apply`]
    /// `(&settings)` を呼ぶまで何もしない(`bin/banto-hub.rs` は起動直後に
    /// settings から読んだ設定を渡す)。
    pub fn new(service: GrpcService) -> Self {
        Self {
            service,
            running: AsyncMutex::new(None),
        }
    }

    /// 現在の設定を即時反映する: 実行中インスタンスがあれば止め、
    /// `settings.enabled` なら `settings.bind:settings.port` で bind し直す
    /// (`MqttPublisher::apply`/`CollectorManager::rebuild` と同じ「古い
    /// タスクを止めて新しいタスク」パターン)。`bin/banto-hub.rs` の起動
    /// シーケンスと `PUT /api/grpc-settings` の両方から呼ばれる。
    ///
    /// 停止は `JoinHandle::abort`(グレースフルシャットダウンではない) -
    /// `MqttPublisher::stop_locked` が poll/eval タスクを abort するのと
    /// 同じ判断: gRPC サーバータスクは内部状態を持たない(`GrpcService`
    /// は読み取り専用ハンドルの束)ので、即座に打ち切っても不整合が残らない。
    ///
    /// `settings.bind` は `String` → `IpAddr` パース(2026-08-08 オーナー
    /// 決定、docs/improvement-plan.md H3)。以前のような
    /// `format!("0.0.0.0:{port}")` の文字列結合は使わない - IPv6 アドレス
    /// (`::1` 等)を渡すと `"::1:50051"` のような壊れた文字列になり
    /// パースに失敗するため、`SocketAddr::new(ip, port)` で組み立てる。
    /// パースに失敗した場合(DB に不正な文字列が直接書き込まれた場合など)
    /// は **panic しない** - eprintln で通知したうえで gRPC サーバーを
    /// 起動せずに戻る。gRPC は任意設定のサブシステムなので、bind
    /// アドレス1つの不正値でプロセス全体(REST/WS を含む)を落とす理由が
    /// ない(`grpc_config`が失敗したときに既定値へフォールバックする
    /// `crate::runtime`(旧 `bin/banto_hub/hub_run.rs`、T14-1で移設)と
    /// 同じ「壊れた設定で全体を道連れにしない」判断)。
    pub async fn apply(&self, settings: &GrpcSettings) {
        let mut guard = self.running.lock().await;
        if let Some(handle) = guard.take() {
            handle.abort();
        }
        if !settings.enabled {
            return;
        }

        let ip: IpAddr = match settings.bind.parse() {
            Ok(ip) => ip,
            Err(err) => {
                eprintln!(
                    "banto-hub: grpc.bind の値 \"{}\" を IP アドレスとして解釈できません\
                     ({err}) - gRPC サーバーは起動しません(管理 UI または \
                     PUT /api/grpc-settings で bind を修正してください)",
                    settings.bind
                );
                return;
            }
        };
        let addr = SocketAddr::new(ip, settings.port);
        let server = TagServiceServer::new(self.service.clone());
        let handle = tokio::spawn(async move {
            if let Err(err) = Server::builder().add_service(server).serve(addr).await {
                eprintln!("banto-hub: gRPC サーバーが終了しました: {err}");
            }
        });
        *guard = Some(handle);
    }

    /// プロセス終了時の停止(`bin/banto-hub.rs` のシャットダウン配線)。
    pub async fn shutdown(&self) {
        let mut guard = self.running.lock().await;
        if let Some(handle) = guard.take() {
            handle.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::{CollectionState, CollectionStatus};

    fn status(mode: RunMode) -> CollectionStatus {
        CollectionStatus {
            state: CollectionState::Running,
            mode,
            run_id: Some(1),
            last_error: None,
            configured_revision: 1,
            running_revision: 1,
        }
    }

    #[test]
    fn simulation_output_gate_disables_only_all_simulation() {
        assert!(!simulation_output_disabled_status(&status(
            RunMode::Configured
        )));
        assert!(simulation_output_disabled_status(&status(
            RunMode::AllSimulation
        )));
    }

    #[test]
    fn stopped_collection_rejection_maps_to_unavailable() {
        let status = write_rejection_status(WriteRejection::CollectionNotRunning(
            CollectionState::Stopped,
        ));
        assert_eq!(status.code(), tonic::Code::Unavailable);
        assert!(status.message().contains("collection_not_running"));
    }

    #[test]
    fn simulation_write_rejection_maps_to_unavailable() {
        let status = write_rejection_status(WriteRejection::SimulationWriteRejected);
        assert_eq!(status.code(), tonic::Code::Unavailable);
        assert!(status.message().contains("simulation_write_rejected"));
    }
}
