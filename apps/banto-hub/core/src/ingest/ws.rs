//! WebSocket の送信口 `GET /api/v1/ingest/{id}/stream`（R2a-2d、
//! docs/banto-hub-ingest-design.md §6.1）。常時つなぐ送り手（DB ブリッジ）の口。
//! 一括で送るだけの送り手は REST（[`super::rest`]）を使う。
//!
//! ## メッセージ（§6.1 の JSON をそのまま実装）
//!
//! ```jsonc
//! // 送り手 → Hub（テキストのフレームだけ。1 メッセージ・1 フレーム 256 KiB まで）
//! { "op": "values", "seq": 17, "epoch": 3, "items": [ { "key": "line1.temp", "v": 12.5, "q": "good", "t": 1761000000000 } ] }
//! { "op": "ping", "seq": 18 }
//! { "op": "status", "detail": "line1: クエリが失敗しました（...）" }   // 1 KiB に切り詰める
//! // Hub → 送り手
//! { "op": "welcome", "connection_id": 12, "connection": "Bridge1", "collecting": true, "epoch": 3,
//!   "stale_timeout_ms": 10000, "limits": { "max_items": 1000, "max_message_bytes": 262144, "messages_per_sec": 20, "items_per_sec": 5000 } }
//! { "op": "ack", "seq": 17, "accepted": 1, "rejected": [ { "index": 0, "key": "line1.tmp", "code": "unknown_key" } ] }
//! { "op": "pong", "seq": 18 }
//! { "op": "error", "seq": 19, "code": "too_many_items" | "rate_limited" | "invalid_message" | "stale_epoch", "detail": "..." }
//! { "op": "state", "collecting": false, "epoch": 4, "config_changed": false }
//! ```
//!
//! - `values` 1 つに `ack`（または `error`）を 1 つ、`ping` 1 つに `pong` を 1 つ返す。
//!   `status` には返さない。`seq` は送り手の任意の整数で、そのまま返すだけ（順序は
//!   見ない）。無ければ応答にも載せない。
//! - `values` の判定は REST と同じ部品と同じ順序（[`super::rest`] の「1 要求の流れ」の
//!   3〜5）: 形（[`parse_values_message`]、`invalid_message`）→ 上限
//!   （[`IngestHub::admit`]、REST と同じ接続ごとのバケット。`too_many_items` /
//!   `rate_limited`）→ 反映（[`IngestHub::apply_ws`]、`stale_epoch`）。断ったメッセージは
//!   何も反映せず、Live（§4.3 の `last_seen_ms`）も進めない。未知のキーと上限の端は
//!   REST と同じく監査する（[`super::audit`]）。
//! - `ping`・`status` は Live を進めるだけ（[`IngestHub::ping_ws`]）。頻度のバケットは
//!   **使わない**: ブリッジは値を送っていない間だけ `ping` を送る（§10.3）ので、値の
//!   メッセージで頻度の上限に当たっている間に `ping` まで断ると Live が切れる。`ping` を
//!   読まずに投げ続ける送り手は、下の送信キューの背圧で切る。
//! - バイナリのフレーム・JSON として読めない・知らない `op` は `error`
//!   `invalid_message`（閉じない）。
//! - `state` は接続ごとの `watch`（[`IngestHub::subscribe_state`] と同じチャンネル、
//!   [`super::WsSession::state`]）が変わるたびに 1 つ（収集の開始・停止、ブリッジの設定の
//!   変更、§4.5・§4.6）。`watch` なので、送り手が読む前に 2 回変わったら最後の 1 回だけ
//!   届く（epoch は最新のものが届けば足りる）。
//!
//! ## 認証と upgrade の順序（§6、オーナーの監査 (2)）
//!
//! このルートは [`super::auth::require_ingest_auth`]（`Transport::Ws`）を
//! `route_layer` で被せる。キー・パスの形・`ingest:#{id}`・接続の有無と有効を確かめて
//! から、ハンドラの `WebSocketUpgrade` が upgrade の応答（101）を返す - **認証を
//! 通らない要求は upgrade されない**（401 / 403 / 404 / 409 の普通の HTTP 応答）。
//! トークンは `Authorization: Bearer` だけで、`Sec-WebSocket-Protocol` からは取り出さ
//! ない（`crate::rest` の `extract_ws_protocol_token` はパスの完全一致で
//! `/api/v1/stream` と `/api/tag-stream` だけ。このルートの upgrade も
//! `protocols([..])` を選ばない）。
//!
//! ## 1 本の制限（§6.1・§7、Q10 = (a)）
//!
//! upgrade の後に [`IngestHub::open_ws_session`] で接続の slot に自分を登録し、前の WS を
//! close 1008 `superseded` で閉じさせる。登録・置き換え・commit による close
//! （`connection_changed` / `connection_removed`）の競合の扱いは `super`（`ingest/mod.rs`）
//! のモジュールの doc「WebSocket の 1 本の制限」。認証の層を通った後、登録までの間に
//! commit が接続を消した・無効にしたら、登録は断られ、`connection_removed` /
//! `connection_changed` で閉じる。
//!
//! ## 再検証（§6.1、`/api/v1/stream` と同じ `crate::stream::Revalidator`）
//!
//! 15 秒ごと（照合が終わった時点から）にキーを照合し直し、失効・期限切れ・存在しない・
//! トリップと確認できたら close 1008（理由文は `api_key_revoked` ほか、
//! `crate::stream::RevokedReason`）。照合できない（DB エラー・5 秒の上限）ときは閉じない。
//! スコープは発行後に変えられない（`crate::api_keys` の「発行・一覧・失効だけ」）ので、
//! 照合し直すのはキーの有効さだけでよい（`ingest:#{id}` は開いた時点で認証の層が確かめた
//! もの）。接続の改名・無効化・削除は再検証ではなく commit が即座に閉じさせる。
//!
//! ## 送信キューと背圧（§6.1、`/api/v1/stream` と同じ形）
//!
//! 1 本 = 受信のループ（このタスク）+ 送信のタスク（[`writer_task`]）。応答は容量
//! [`OUTBOUND_QUEUE_CAPACITY`]（256）の `mpsc` に `try_send` で積み、満杯なら close 1013
//! `backpressure` で閉じる（`ack` を読まない送り手で Hub のメモリが膨らまない）。close
//! は容量 1 の別のチャンネルで送り、送信のタスクはそちらを優先する。
//!
//! **送信のタスクの終わりは上限つきで待つ**（[`WRITER_DRAIN_TIMEOUT`]、5 秒）。読まない
//! 送り手・半開きの TCP（§12 Q10 の理由）では送信のタスクが送り先の読み出しを待って
//! 止まったままになるので、上限を過ぎたら `abort` する（DB を触らないタスクなので
//! implementation-checklist §5 の「DB を触る背景タスクを abort で止めない」に当たらない）。
//! そのときは close フレームは届かず TCP ごと閉じる。どの閉じ方でも、受信のループ・
//! 送信のタスク・再検証のタイマー・slot の登録（[`super::WsSession`] の `Drop`）は
//! この関数の終わりまでにすべて片付く。
//!
//! ## close コード
//!
//! | code | 理由文 | いつ | 監査の `reason` |
//! | --- | --- | --- | --- |
//! | （送らない） | - | 送り手が閉じた・TCP が切れた・読み取りのエラー | `client_closed` |
//! | 1008 | `superseded` | 同じ接続に新しい WS が開いた | `superseded` |
//! | 1008 | `api_key_revoked` / `api_key_expired` / `api_key_not_found` / `api_key_tripped` | 再検証 | `revoked`（`revoked_reason` に理由文） |
//! | 1008 | `connection_changed` | 接続の無効化・`push` でなくなった（commit） | `connection_changed` |
//! | 1008 | `connection_removed` | 接続の削除（commit） | `connection_removed` |
//! | 1009 | `message_too_big` | 1 メッセージ・1 フレームが 256 KiB を超えた | `message_too_big` |
//! | 1013 | `backpressure` | 送信キューが満杯 | `backpressure` |
//!
//! 1009 は tungstenite の読み取りの上限（`WebSocketUpgrade::max_message_size` /
//! `max_frame_size`、256 KiB ちょうどは通る）が返すエラーで判定する。axum は
//! tungstenite のエラーを `axum::Error` に包むだけで型を出さないので、表示の文
//! （`Message too long`）で見分ける（[`is_message_too_big`]。実際の WebSocket で 1 バイト
//! 超を送るテストがこの判定を固定する）。
//!
//! Hub の終了（`server_shutdown`、§8）は今の `/api/v1/stream` と同じく WS に知らせる
//! 経路が無い（プロセスの終わりで TCP ごと閉じる。監査の行は残らない）。
//!
//! ## Live（§4.3）
//!
//! WS が開いていること自体は Live の条件ではない。切断しても `last_seen_ms` は残るので、
//! 最後の通信から `stale_timeout_ms` までは Live のまま（ブリッジの再接続で Bad が
//! 挟まらない）。開いたまま黙った送り手はその時間で Down になる。

use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Extension, State};
use axum::http::HeaderMap;
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use banto_server::AuthState;
use banto_tstore::Clock;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Number, Value};
use tokio::sync::mpsc;

use crate::api_keys::ApiKeysService;
use crate::audit::AuditLogService;
use crate::rest::{bearer_token, unauthorized_response};
use crate::stream::{
    api_key_recheck_verdict, RecheckVerdict, Revalidator, RevokedReason, StreamCredential,
    StreamRevalidationTiming, REVOKED_CLOSE_CODE,
};

use super::audit::{self as ingest_audit, IngestAuditContext, IngestAuditRow, Transport};
use super::auth::{require_ingest_auth, IngestAuthState, IngestCaller};
use super::limits::PUSH_LIMITS;
use super::rest::IngestRejectedItem;
use super::validate::{parse_values_message, MessageError};
use super::{ApplyRefusal, IngestHub, RejectCode, WsEnd, WsRefusal, WsSession};

/// WebSocket の送信口のパス。
pub const STREAM_PATH: &str = "/api/v1/ingest/{id}/stream";

/// 送信キューの容量（`/api/v1/stream` と同じ 256）。
pub const OUTBOUND_QUEUE_CAPACITY: usize = 256;

/// 送信のタスクの終わりを待つ上限（モジュールの doc「送信キューと背圧」）。
pub const WRITER_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

/// 1 メッセージ・1 フレームが大きすぎるときの close code（RFC 6455 の 1009 Message Too Big）。
pub const MESSAGE_TOO_BIG_CLOSE_CODE: u16 = 1009;

/// 送信キューが満杯のときの close code（RFC 6455 の 1013 Try Again Later、
/// `/api/v1/stream` と同じ）。
pub const BACKPRESSURE_CLOSE_CODE: u16 = 1013;

/// 送信キューの容量と、送信のタスクを待つ上限。本番は [`Default`]。テストは router に
/// `Extension(IngestWsTuning { .. })` を重ねて変える（`StreamRevalidationTiming` と同じ
/// 差し込み方。外部の要求からは差し込めない）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IngestWsTuning {
    pub queue_capacity: usize,
    pub drain_timeout: Duration,
}

impl Default for IngestWsTuning {
    fn default() -> Self {
        Self {
            queue_capacity: OUTBOUND_QUEUE_CAPACITY,
            drain_timeout: WRITER_DRAIN_TIMEOUT,
        }
    }
}

/// ハンドラの state。
#[derive(Clone)]
pub struct IngestWsState {
    pub ingest: Arc<IngestHub>,
    pub audit: AuditLogService,
    pub clock: Arc<dyn Clock>,
    pub api_keys: ApiKeysService,
}

/// `GET /api/v1/ingest/{id}/stream` のルーター。認証の層は `route_layer` でこのルート
/// だけに被せる（[`super::rest::ingest_rest_router`] と同じ形、経路は `Transport::Ws`）。
pub fn ingest_ws_router(
    ingest: Arc<IngestHub>,
    clock: Arc<dyn Clock>,
    auth: AuthState,
    api_keys: ApiKeysService,
    audit: AuditLogService,
) -> Router {
    let auth_state = IngestAuthState {
        auth,
        api_keys: api_keys.clone(),
        audit: audit.clone(),
        ingest: ingest.clone(),
        clock: clock.clone(),
        transport: Transport::Ws,
    };
    Router::new()
        .route(STREAM_PATH, get(v1_ingest_stream))
        .route_layer(middleware::from_fn_with_state(
            auth_state,
            require_ingest_auth,
        ))
        .with_state(IngestWsState {
            ingest,
            audit,
            clock,
            api_keys,
        })
}

/// 取り込みの WS を開いた API キーの照合し直し（`crate::stream::ApiKeyStreamCredential`
/// と同じ判定。時計を `CollectorManager` ではなく取り込みの時計から取るだけ）。平文の
/// キーは照合に渡す以外に外へ出さない（`Debug` を持たない）。
struct IngestKeyCredential {
    api_keys: ApiKeysService,
    token: String,
    clock: Arc<dyn Clock>,
}

impl StreamCredential for IngestKeyCredential {
    fn recheck(&self) -> futures_util::future::BoxFuture<'static, RecheckVerdict> {
        let api_keys = self.api_keys.clone();
        let token = self.token.clone();
        let now_ms = self.clock.now_ms();
        Box::pin(async move { api_key_recheck_verdict(api_keys.check(&token, now_ms).await) })
    }
}

/// upgrade（認証の層の後ろ。モジュールの doc「認証と upgrade の順序」）。
pub async fn v1_ingest_stream(
    ws: WebSocketUpgrade,
    State(state): State<IngestWsState>,
    Extension(caller): Extension<IngestCaller>,
    headers: HeaderMap,
    timing: Option<Extension<StreamRevalidationTiming>>,
    tuning: Option<Extension<IngestWsTuning>>,
) -> Response {
    // 認証の層が `bh_` のキーで通した要求なので必ずある（無ければ層の外から呼ばれた）。
    let Some(token) = bearer_token(&headers).map(str::to_string) else {
        return unauthorized_response();
    };
    let timing = timing.map(|Extension(t)| t).unwrap_or_default();
    let tuning = tuning.map(|Extension(t)| t).unwrap_or_default();
    let revalidator = Revalidator::new(
        Arc::new(IngestKeyCredential {
            api_keys: state.api_keys.clone(),
            token,
            clock: state.clock.clone(),
        }),
        timing,
    );
    let max = usize::try_from(PUSH_LIMITS.max_ws_message_bytes).unwrap_or(usize::MAX);
    ws.max_message_size(max)
        .max_frame_size(max)
        .on_upgrade(move |socket| run_socket(socket, state, caller, revalidator, tuning))
        .into_response()
}

/// WS が閉じた理由（モジュールの doc の close コードの表の行）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum End {
    ClientClosed,
    Hub(WsEnd),
    Revoked(RevokedReason),
    Backpressure,
    MessageTooBig,
}

impl End {
    fn close_frame(self) -> Option<CloseFrame> {
        let (code, reason) = match self {
            Self::ClientClosed => return None,
            Self::Hub(end) => (REVOKED_CLOSE_CODE, end.as_str()),
            Self::Revoked(reason) => (REVOKED_CLOSE_CODE, reason.as_str()),
            Self::Backpressure => (BACKPRESSURE_CLOSE_CODE, "backpressure"),
            Self::MessageTooBig => (MESSAGE_TOO_BIG_CLOSE_CODE, "message_too_big"),
        };
        Some(CloseFrame {
            code,
            reason: reason.into(),
        })
    }

    /// 監査の `reason`（§8）と、`revoked` のときの理由文。
    fn audit_reason(self) -> (&'static str, Option<&'static str>) {
        match self {
            Self::ClientClosed => ("client_closed", None),
            Self::Hub(end) => (end.as_str(), None),
            Self::Revoked(reason) => ("revoked", Some(reason.as_str())),
            Self::Backpressure => ("backpressure", None),
            Self::MessageTooBig => ("message_too_big", None),
        }
    }
}

/// 読み取りのエラーが 256 KiB の上限の超過か（モジュールの doc「close コード」）。
fn is_message_too_big(err: &axum::Error) -> bool {
    err.to_string().contains("Message too long")
}

/// 送信のチャンネル（`/api/v1/stream` の `enqueue` と同じ規律）。
struct Outbound {
    data_tx: mpsc::Sender<Message>,
}

impl Outbound {
    /// 積めたら `true`。満杯（読まない送り手）なら `false`（呼び出し側が 1013 で閉じる）。
    fn send<T: Serialize>(&self, payload: &T) -> bool {
        let text =
            serde_json::to_string(payload).expect("ingest/ws.rs のワイヤ型は常にシリアライズ可能");
        self.data_tx.try_send(Message::Text(text.into())).is_ok()
    }
}

#[derive(Serialize)]
struct WelcomeLimits {
    max_items: u64,
    max_message_bytes: u64,
    messages_per_sec: u64,
    items_per_sec: u64,
}

#[derive(Serialize)]
struct WelcomeWire<'a> {
    op: &'static str,
    connection_id: i64,
    connection: &'a str,
    collecting: bool,
    epoch: u64,
    stale_timeout_ms: u64,
    limits: WelcomeLimits,
}

#[derive(Serialize)]
struct AckWire<'a> {
    op: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    seq: Option<&'a Number>,
    accepted: usize,
    rejected: Vec<IngestRejectedItem>,
}

#[derive(Serialize)]
struct PongWire<'a> {
    op: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    seq: Option<&'a Number>,
}

#[derive(Serialize)]
struct ErrorWire<'a> {
    op: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    seq: Option<&'a Number>,
    code: &'a str,
    detail: String,
}

#[derive(Serialize)]
struct StateWire {
    op: &'static str,
    collecting: bool,
    epoch: u64,
    config_changed: bool,
}

/// 受けたメッセージの外枠（`op`・`seq`・`status` の `detail`）。`items` などは読み飛ばし、
/// `values` は [`parse_values_message`] で読み直す（REST と同じ形の検証）。
#[derive(Deserialize)]
struct Envelope {
    op: String,
    #[serde(default)]
    seq: Option<Value>,
    #[serde(default)]
    detail: Option<Value>,
}

/// 1 本の WS（upgrade の後）。モジュールの doc「1 本の制限」「送信キューと背圧」。
async fn run_socket(
    socket: WebSocket,
    state: IngestWsState,
    caller: IngestCaller,
    revalidator: Revalidator,
    tuning: IngestWsTuning,
) {
    let (sink, incoming) = socket.split();
    let (data_tx, data_rx) = mpsc::channel::<Message>(tuning.queue_capacity.max(1));
    let (close_tx, close_rx) = mpsc::channel::<CloseFrame>(1);
    let mut writer = tokio::spawn(writer_task(sink, data_rx, close_rx));
    let outbound = Outbound { data_tx };

    let end = match state.ingest.open_ws_session(caller.connection_id) {
        Ok((session, info)) => {
            let ctx = IngestAuditContext {
                // 登録した時点の名前を控える（認証の層の後に改名されていても今の名前）。
                connection_name: Some(info.name.clone()),
                ..caller.audit_context()
            };
            ingest_audit::record(&state.audit, vec![IngestAuditRow::connected(&ctx)]).await;
            let welcome = WelcomeWire {
                op: "welcome",
                connection_id: caller.connection_id,
                connection: &info.name,
                collecting: info.collecting,
                epoch: info.epoch,
                stale_timeout_ms: info.stale_timeout_ms,
                limits: WelcomeLimits {
                    max_items: PUSH_LIMITS.max_items_per_message,
                    max_message_bytes: PUSH_LIMITS.max_ws_message_bytes,
                    messages_per_sec: PUSH_LIMITS.messages_per_sec,
                    items_per_sec: PUSH_LIMITS.items_per_sec,
                },
            };
            let mut conn = Conn {
                ingest: state.ingest.clone(),
                audit: state.audit.clone(),
                clock: state.clock.clone(),
                ctx,
                outbound,
                accepted: 0,
            };
            let (end, accepted) = if conn.outbound.send(&welcome) {
                let end = conn.run(session, incoming, revalidator).await;
                (end, conn.accepted)
            } else {
                (End::Backpressure, 0)
            };
            // ここで登録・再検証・受信の半分は片付いている（`run` が持って行って捨てた）。
            Some((end, accepted, conn.ctx))
        }
        // 認証の層を通った後、登録までの間に commit と競った（§6.1）。監査の
        // `ingest_connected` は出していないので、切断の行も出さない。
        Err(refusal) => {
            drop(outbound);
            drop(incoming);
            drop(revalidator);
            let end = match refusal {
                ApplyRefusal::ConnectionDisabled => WsEnd::ConnectionChanged,
                ApplyRefusal::UnknownConnection | ApplyRefusal::StaleEpoch { .. } => {
                    WsEnd::ConnectionRemoved
                }
            };
            if let Some(frame) = End::Hub(end).close_frame() {
                let _ = close_tx.try_send(frame);
            }
            None
        }
    };

    if let Some((end, _, _)) = &end {
        if let Some(frame) = end.close_frame() {
            let _ = close_tx.try_send(frame);
        }
    }
    // チャンネルを閉じて送信のタスクに終わりを伝え、上限つきで待つ（読まない送り手・
    // 半開きの TCP で止まったままなら abort して TCP ごと閉じる）。
    drop(close_tx);
    if tokio::time::timeout(tuning.drain_timeout, &mut writer)
        .await
        .is_err()
    {
        writer.abort();
        let _ = writer.await;
    }

    if let Some((end, accepted, ctx)) = end {
        let (reason, revoked_reason) = end.audit_reason();
        ingest_audit::record(
            &state.audit,
            vec![IngestAuditRow::disconnected(
                &ctx,
                reason,
                revoked_reason,
                accepted,
            )],
        )
        .await;
    }
}

/// 送信専用のタスク（`/api/v1/stream` の `writer_task` と同じ形）: close を優先し、
/// どちらかのチャンネルが閉じたら終わる。
async fn writer_task(
    mut sink: futures_util::stream::SplitSink<WebSocket, Message>,
    mut data_rx: mpsc::Receiver<Message>,
    mut close_rx: mpsc::Receiver<CloseFrame>,
) {
    loop {
        tokio::select! {
            biased;
            frame = close_rx.recv() => {
                if let Some(frame) = frame {
                    let _ = sink.send(Message::Close(Some(frame))).await;
                }
                break;
            }
            msg = data_rx.recv() => match msg {
                Some(msg) => {
                    if sink.send(msg).await.is_err() {
                        break;
                    }
                }
                None => break,
            },
        }
    }
}

/// 受信のループの状態。
struct Conn {
    ingest: Arc<IngestHub>,
    audit: AuditLogService,
    clock: Arc<dyn Clock>,
    ctx: IngestAuditContext,
    outbound: Outbound,
    /// この WS で受け付けた件数（切断の監査）。
    accepted: u64,
}

/// 1 つのメッセージを処理した結果: 続けるか、閉じるか。
type Step = Result<(), End>;

impl Conn {
    /// 受信・`state`・再検証・`IngestHub` からの close を 1 本の `select!` で直列に処理する
    /// （`/api/v1/stream` の `handle_socket` と同じ）。`session`・`incoming`・
    /// `revalidator` はここで捨てる（登録は `WsSession` の `Drop` で外れる）。
    async fn run(
        &mut self,
        mut session: WsSession,
        mut incoming: futures_util::stream::SplitStream<WebSocket>,
        mut revalidator: Revalidator,
    ) -> End {
        loop {
            let step: Step = tokio::select! {
                end = &mut session.ended => Err(End::Hub(end.unwrap_or(WsEnd::ConnectionRemoved))),
                verdict = revalidator.next_verdict() => match verdict {
                    RecheckVerdict::Revoked { reason } => Err(End::Revoked(reason)),
                    // 照合できない（DB エラー・タイムアウト）ときは閉じない。
                    RecheckVerdict::Valid | RecheckVerdict::Unknown => Ok(()),
                },
                changed = session.state.changed() => match changed {
                    Ok(()) => {
                        let state = *session.state.borrow_and_update();
                        self.send(&StateWire {
                            op: "state",
                            collecting: state.collecting,
                            epoch: state.epoch,
                            config_changed: state.config_changed,
                        })
                    }
                    // 送り手（slot）を `session` が持っているので起きない。
                    Err(_) => Err(End::Hub(WsEnd::ConnectionRemoved)),
                },
                msg = incoming.next() => match msg {
                    Some(Ok(Message::Text(text))) => self.handle_text(&session, text.as_str()).await,
                    Some(Ok(Message::Binary(_))) => self.send_error(
                        None,
                        "invalid_message",
                        "テキスト以外のフレームは扱えません".to_string(),
                    ),
                    // Ping/Pong は axum/tungstenite がプロトコルの水準で応える。
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => Ok(()),
                    Some(Ok(Message::Close(_))) | None => Err(End::ClientClosed),
                    Some(Err(err)) if is_message_too_big(&err) => Err(End::MessageTooBig),
                    Some(Err(_)) => Err(End::ClientClosed),
                },
            };
            if let Err(end) = step {
                return end;
            }
        }
    }

    fn send<T: Serialize>(&self, payload: &T) -> Step {
        if self.outbound.send(payload) {
            Ok(())
        } else {
            Err(End::Backpressure)
        }
    }

    fn send_error(&self, seq: Option<&Number>, code: &str, detail: String) -> Step {
        self.send(&ErrorWire {
            op: "error",
            seq,
            code,
            detail,
        })
    }

    async fn handle_text(&mut self, session: &WsSession, text: &str) -> Step {
        let envelope: Envelope = match serde_json::from_str(text) {
            Ok(envelope) => envelope,
            Err(err) => {
                return self.send_error(
                    None,
                    "invalid_message",
                    format!("JSON として解釈できません: {err}"),
                )
            }
        };
        let seq = match &envelope.seq {
            None | Some(Value::Null) => None,
            Some(Value::Number(n)) if n.is_i64() || n.is_u64() => Some(n.clone()),
            Some(_) => {
                return self.send_error(
                    None,
                    "invalid_message",
                    "seq は整数で指定してください".to_string(),
                )
            }
        };
        match envelope.op.as_str() {
            "values" => self.handle_values(session, seq.as_ref(), text).await,
            "ping" => match self.ingest.ping_ws(session, None, self.clock.now_ms()) {
                Ok(()) => self.send(&PongWire {
                    op: "pong",
                    seq: seq.as_ref(),
                }),
                Err(refusal) => Err(refusal_end(refusal)),
            },
            "status" => {
                let detail = match envelope.detail {
                    Some(Value::String(detail)) => sanitize_status(&detail),
                    _ => {
                        return self.send_error(
                            seq.as_ref(),
                            "invalid_message",
                            "status の detail は文字列で指定してください".to_string(),
                        )
                    }
                };
                self.ingest
                    .ping_ws(session, Some(detail), self.clock.now_ms())
                    .map_err(refusal_end)
            }
            other => self.send_error(
                seq.as_ref(),
                "invalid_message",
                format!("未知の op です: {other}"),
            ),
        }
    }

    /// `values`（モジュールの doc の判定の順序。REST の 3〜5 と同じ）。
    async fn handle_values(
        &mut self,
        session: &WsSession,
        seq: Option<&Number>,
        text: &str,
    ) -> Step {
        let message = match parse_values_message(text.as_bytes()) {
            Ok(message) => message,
            Err(err) => {
                let MessageError::InvalidMessage { detail } = &err;
                return self.send_error(seq, err.as_str(), detail.clone());
            }
        };
        let conn_id = self.ctx.connection_id;
        let now_ms = self.clock.now_ms();

        let admission = match self.ingest.admit(conn_id, message.items.len(), now_ms) {
            Ok(admission) => admission,
            Err(refusal) => {
                let rows = ingest_audit::refusal_rows(&self.ingest, &self.ctx, refusal, now_ms);
                ingest_audit::record(&self.audit, rows).await;
                return Err(apply_refusal_end(refusal));
            }
        };
        let mut rows: Vec<IngestAuditRow> = admission
            .edges
            .iter()
            .map(|edge| IngestAuditRow::limit_edge(&self.ctx, edge))
            .collect();
        if let Err(limit) = admission.outcome {
            ingest_audit::record(&self.audit, rows).await;
            let detail = match limit.retry_after_secs {
                Some(secs) => format!(
                    "上限（{}）を超えました。{secs} 秒ほど待ってから送ってください",
                    limit.kind.as_str()
                ),
                None => format!(
                    "1 メッセージの件数の上限（{}）を超えました",
                    PUSH_LIMITS.max_items_per_message
                ),
            };
            return self.send_error(seq, limit.kind.error_code(), detail);
        }

        let ack = match self
            .ingest
            .apply_ws(session, message.epoch, &message.items, now_ms)
        {
            Ok(ack) => ack,
            Err(WsRefusal::Refused(ApplyRefusal::StaleEpoch { epoch })) => {
                ingest_audit::record(&self.audit, rows).await;
                return self.send_error(
                    seq,
                    "stale_epoch",
                    format!("epoch が今のもの（{epoch}）と違います。次の state を待ってください"),
                );
            }
            Err(refusal) => {
                if let WsRefusal::Refused(refusal) = refusal {
                    rows.extend(ingest_audit::refusal_rows(
                        &self.ingest,
                        &self.ctx,
                        refusal,
                        now_ms,
                    ));
                }
                ingest_audit::record(&self.audit, rows).await;
                return Err(refusal_end(refusal));
            }
        };
        self.accepted = self
            .accepted
            .saturating_add(u64::try_from(ack.accepted).unwrap_or(u64::MAX));

        let unknown = ack
            .rejected
            .iter()
            .filter(|rejection| rejection.code == RejectCode::UnknownKey)
            .map(|rejection| rejection.key.as_str());
        rows.extend(
            self.ingest
                .throttle_unknown_keys(conn_id, unknown, now_ms)
                .iter()
                .map(|record| IngestAuditRow::unknown_key(&self.ctx, record)),
        );
        ingest_audit::record(&self.audit, rows).await;

        self.send(&AckWire {
            op: "ack",
            seq,
            accepted: ack.accepted,
            rejected: ack
                .rejected
                .into_iter()
                .map(|rejection| IngestRejectedItem {
                    index: rejection.index,
                    key: rejection.key,
                    code: rejection.code.as_str().to_string(),
                })
                .collect(),
        })
    }
}

/// メッセージ全体の拒否のうち、WS を閉じるもの（接続が消えた・無効になった）。
/// `stale_epoch` は閉じない（呼び出し側が `error` で返す）。
fn apply_refusal_end(refusal: ApplyRefusal) -> End {
    match refusal {
        ApplyRefusal::ConnectionDisabled => End::Hub(WsEnd::ConnectionChanged),
        ApplyRefusal::UnknownConnection | ApplyRefusal::StaleEpoch { .. } => {
            End::Hub(WsEnd::ConnectionRemoved)
        }
    }
}

/// `apply_ws` / `ping_ws` の拒否で閉じる理由。`NotCurrent` は新しい WS に置き換えられた
/// （commit が閉じさせた場合も、理由はすぐ `ended` に届くが、ここでは置き換えとして扱う）。
fn refusal_end(refusal: WsRefusal) -> End {
    match refusal {
        WsRefusal::NotCurrent => End::Hub(WsEnd::Superseded),
        WsRefusal::Refused(refusal) => apply_refusal_end(refusal),
    }
}

/// 送り手の `status` の文（§9「Hub は制御文字を落として切り詰めるだけ」）: 制御文字を
/// 落とし、`PUSH_LIMITS.max_status_detail_bytes`（1 KiB）に文字の境界で切り詰める。
pub fn sanitize_status(detail: &str) -> String {
    let max = usize::try_from(PUSH_LIMITS.max_status_detail_bytes).unwrap_or(usize::MAX);
    let mut out = String::with_capacity(detail.len().min(max));
    for c in detail.chars().filter(|c| !c.is_control()) {
        if out.len() + c.len_utf8() > max {
            break;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// close コードの表（モジュールの doc）。
    #[test]
    fn the_close_code_table() {
        type Row<'a> = (End, Option<(u16, &'a str)>, &'a str, Option<&'a str>);
        let rows: Vec<Row> = vec![
            (End::ClientClosed, None, "client_closed", None),
            (
                End::Hub(WsEnd::Superseded),
                Some((1008, "superseded")),
                "superseded",
                None,
            ),
            (
                End::Hub(WsEnd::ConnectionChanged),
                Some((1008, "connection_changed")),
                "connection_changed",
                None,
            ),
            (
                End::Hub(WsEnd::ConnectionRemoved),
                Some((1008, "connection_removed")),
                "connection_removed",
                None,
            ),
            (
                End::Revoked(RevokedReason::ApiKeyRevoked),
                Some((1008, "api_key_revoked")),
                "revoked",
                Some("api_key_revoked"),
            ),
            (
                End::Revoked(RevokedReason::ApiKeyTripped),
                Some((1008, "api_key_tripped")),
                "revoked",
                Some("api_key_tripped"),
            ),
            (
                End::Backpressure,
                Some((1013, "backpressure")),
                "backpressure",
                None,
            ),
            (
                End::MessageTooBig,
                Some((1009, "message_too_big")),
                "message_too_big",
                None,
            ),
        ];
        for (end, frame, reason, revoked) in rows {
            let actual = end
                .close_frame()
                .map(|frame| (frame.code, frame.reason.to_string()));
            assert_eq!(
                actual,
                frame.map(|(code, reason)| (code, reason.to_string())),
                "{end:?}"
            );
            assert_eq!(end.audit_reason(), (reason, revoked), "{end:?}");
        }
    }

    /// `status` の文: 制御文字を落とし、1 KiB に文字の境界で切り詰める。
    #[test]
    fn status_details_are_sanitized_and_truncated() {
        assert_eq!(sanitize_status("a\nb\tc\u{7}d"), "abcd");
        let long = "あ".repeat(400); // 1,200 バイト
        let cut = sanitize_status(&long);
        assert_eq!(cut, "あ".repeat(341)); // 1,023 バイト（342 文字目で 1,024 を超える）
        assert!(cut.len() <= 1024);
        assert_eq!(sanitize_status(&"x".repeat(2000)).len(), 1024);
    }

    /// 1009 の判定: tungstenite の上限の超過の文を `axum::Error` に包んだものを見分ける。
    #[test]
    fn message_too_big_is_recognised_through_axum_error() {
        use tokio_tungstenite::tungstenite::error::{CapacityError, Error as WsError};
        let too_big = axum::Error::new(WsError::Capacity(CapacityError::MessageTooLong {
            size: 262_145,
            max_size: 262_144,
        }));
        assert!(is_message_too_big(&too_big));
        let other = axum::Error::new(WsError::ConnectionClosed);
        assert!(!is_message_too_big(&other));
    }
}
