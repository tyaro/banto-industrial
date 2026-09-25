//! `GET /api/v1/stream` の WebSocket 購読 (docs/tag-server-design.md
//! §5.2「WebSocket（T1）」が仕様書。§4「タグ空間のセマンティクス」・
//! §4.1「catalog はバインディング契約である」の `config_changed` も前提)。
//!
//! ## メッセージプロトコル（設計 §5.2 の JSON をそのまま実装）
//!
//! ```jsonc
//! // クライアント → サーバー
//! { "op": "subscribe",   "id": 1, "tags": [...], "mode": "on_change" | "interval",
//!   "interval_ms": 1000 }          // mode=interval のとき必須
//! { "op": "unsubscribe", "id": 1 }
//! { "op": "ping" }
//!
//! // サーバー → クライアント
//! { "op": "data",  "id": 1, "t": <送信時刻ms>, "values": [{ "tag", "v", "q", "t" }] }
//! { "op": "event", "kind": "...", "connection": "...", "t": ... }
//! { "op": "config_changed", "revision": 42 }
//! { "op": "error", "id": 1, "code": "unknown_tag" | "invalid_request", "detail": "..." }
//! ```
//!
//! ## コネクションのタスク構造
//!
//! 1コネクション = 1 `tokio::spawn`（[`handle_socket`]）+ 1 writer タスク
//! （[`writer_task`]）。`handle_socket` はクライアント受信ループ・250ms
//! 評価タイマ（[`EVAL_TICK_MS`]）・`CollectEvent` 中継・`revision` watch を
//! 1本の `tokio::select!` で直列に処理する（複数プロデューサ間の排他が
//! 不要 - 全て同じタスク内で順番に処理されるため、購読状態
//! （`HashMap<i64, Subscription>`）にロックは要らない）。送信だけは
//! 別タスクに分離し、`mpsc`（容量 [`OUTBOUND_QUEUE_CAPACITY`]）越しに渡す -
//! 遅いクライアントの TCP 送信待ちが評価ループやクライアント受信処理を
//! 巻き込んで止めないため（設計 §5.2「バックプレッシャ...収集側を止めない」
//! と同じ思想を1コネクション内に適用）。
//!
//! ## バックプレッシャ切断（設計 §5.2 要件6）
//!
//! 送信キューが満杯（`mpsc::Sender::try_send` が `Full`）になったら、その
//! 場でコネクション全体を切断する。理由を close frame に載せるため、通常の
//! データ用キューとは別に容量1の `close_tx`（`mpsc::Sender<CloseFrame>`）を
//! 持ち、writer タスクは `select!` の `biased` 分岐でこちらを優先する -
//! データ用キューが満杯でも close 信号だけは必ず届く（キューに積む方式だと
//! 満杯を検知した張本人の close メッセージ自体が積めない可能性がある）。
//!
//! ## on_change の評価方式（設計 §5.2 要件2）
//!
//! Stale は読み出し時判定（`banto_collect::CurrentValuesHandle`）なので、
//! 「品質が変わった」という事実自体は誰かが定期的に読みにいかない限り
//! 検知できない。評価周期は **250ms 固定**（[`EVAL_TICK_MS`]）— 最小
//! グループ周期 100ms（設計 §9 T0 実装時点の実績値）に対して十分な解像度
//! があり、`CurrentValuesHandle::snapshot`/`get` は安価（`RwLock` 読み取り
//! のみ）なので250msごとに全購読を舐めても負荷にならない。
//!
//! ## ワイルドカードは評価時に TagMap へ照合（設計 §5.2 要件4）
//!
//! `subscribe` 時点でタグ集合を確定させず、[`Subscription::patterns`] だけ
//! 保持して、評価の都度 [`resolve`] で最新の `TagMap` に照合する。
//! `config_changed` 後に新しいタグが自動で購読範囲へ入る（catalog
//! バインドモデル §4.1 の「revision 進行 = 収集スナップショット世代」と
//! 整合する挙動）。未知の**具体名**（ワイルドカードでない）だけは
//! subscribe 時点で catalog にあるか検証し、無ければ購読自体を
//! `unknown_tag` で拒否する（REST `?tags=` と同じ「部分成功で誤解させ
//! ない」規律 - ワイルドカードは0件マッチでもエラーにしない）。
//!
//! ## interval の下限クランプ（設計 §5.2 要件3、判断の記録）
//!
//! 設計文書の書きぶりは「下限を下回る指定をどう扱うか」が確定していな
//! かった（エラー拒否とクランプ採用のどちらとも取れる書き方）。ここでは
//! **クランプを採用**する: `invalid_request` で拒否すると、クライアントが
//! 「このグループの現在の周期は何か」を先に catalog から調べてから
//! subscribe しなければならず、購読が周期変更のたびに壊れる（catalog の
//! `period_ms` が変わると、以前は妥当だった `interval_ms` が突然エラーに
//! なる）。クランプなら購読は常に成立し、単に「要求より粗い間隔になる」
//! だけで済む - FA-Server 型タグ空間の「取りこぼしは許容、購読は落とさな
//! い」という設計思想（recorder-requirements.md 由来のBad/Stale運用と同じ
//! 発想）に近い。下限は「マッチしたタグが属するグループの `period_ms` の
//! 最小値」と「評価ループ自体の周期（[`EVAL_TICK_MS`]、これより速くは
//! どのみち送れない）」の大きい方（[`interval_floor_ms`]）。クランプは
//! **subscribe 時点で1回だけ**計算し、購読の生存期間中は固定する
//! （動的に変えると「一定間隔で届く」というクライアント側の期待を壊す）。
//!
//! ## 接続中の再検証（#430、banto #231 / #234 と同じ考え方）
//!
//! 認証（`crate::rest::require_tag_space_auth`）は**接続したときにしか**
//! 走らない。ストリームは切断まで開いたままなので、失効したキーでも値の配信
//! を受け取り続けてしまう。そこで各ストリームが [`REVALIDATE_INTERVAL`]
//! （15 秒）ごとに自分の資格情報を照合し直し、使えないと**確認できたら**
//! close フレーム（[`REVOKED_CLOSE_CODE`] = 1008 Policy Violation、理由文に
//! `api_key_revoked` / `api_key_expired` / `api_key_tripped` /
//! `api_key_not_found`）で閉じる。
//!
//! - **対象**: いまは API キーで開いたストリーム（`/api/v1/stream`）だけ。
//!   照合は `crate::api_keys::ApiKeysService::check` →
//!   `crate::api_keys::api_key_verdict`（#434 / #435 と同じ分類）。
//!   セッションで開いたストリーム（`/api/v1/stream` のセッション・
//!   `/api/tag-stream`）は、今は従来どおり接続時の検証だけ - banto-server の
//!   `AuthState::revalidate`（アイドルのタイマーを延ばさない照合）が
//!   公開されたら（tyaro/banto#239）、[`StreamCredential`] の実装を 1 つ
//!   足して同じ仕組みに差し込む（`TODO(#239)`、[`ws_upgrade`] 参照）。
//! - **照合できない（DB エラー・タイムアウト）ときは閉じない**。次の期限で
//!   もう一度照合する。
//! - **タイマーはストリーム 1 本につき 1 つ**（[`Revalidator`]、
//!   [`handle_socket`] のローカル変数）。別タスクは起こさないので、切断で
//!   `handle_socket` が終われば、期限も照合中の future も一緒に捨てられる。
//! - 照合 1 回の上限は [`REVALIDATE_TIMEOUT`]（5 秒、周期より短い）。超えたら
//!   「照合できない」扱い。打ち切った照合の future は捨てるだけで、API キーの
//!   照合は読み取りのみ（`touch_last_used` も呼ばない）なので、あとから状態を
//!   変えることはない。
//! - **次の期限は照合が終わった時点から 1 周期後**。照合がどれだけ遅くても、
//!   2 回の照合の間には必ず 1 周期ぶんの配信の時間がある。さらに banto の SSE
//!   と違い、照合は `select!` の 1 分岐として**配信と並行に**進める（照合を
//!   待つ間も 250ms の評価・受信は止まらない。値そのものを流すストリームで、
//!   照合が返らない 5 秒間の配信停止を避けるため）。同時に走る照合は常に 1 つ。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Extension, State};
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::SqlitePool;
use tokio::sync::{broadcast, mpsc};
use tokio::time::MissedTickBehavior;

use banto_collect::CollectEvent;

use crate::api_keys::{
    api_key_verdict, ApiKeyCheck, ApiKeyContext, ApiKeyRejection, ApiKeysService,
    UnauthenticatedReason,
};
use crate::hub::{quality_str, CollectorManager};
use crate::rest::TagSpaceState;
use crate::subscribe_core::{
    self, interval_floor_ms, Mode, ResolvedValue, Subscription, TagPattern, EVAL_TICK_MS,
};

/// 送信キュー容量（設計 §5.2 要件6「送信キュー(mpsc、容量 256 程度)」）。
const OUTBOUND_QUEUE_CAPACITY: usize = 256;

/// バックプレッシャ切断の WebSocket close code。RFC 6455 の 1013 (Try Again
/// Later) - 「サーバーは正常だがこのクライアントの処理が追いついていない」
/// を最も素直に表す標準コード。
const BACKPRESSURE_CLOSE_CODE: u16 = 1013;

// --- 接続中の再検証（#430、このモジュールの doc comment 参照） ----------------

/// 開いているストリームが資格情報を照合し直す間隔。banto の SSE
/// （`banto_server::events::REVALIDATE_INTERVAL`）と同じ 15 秒。照合が
/// **終わった時点**から数える。
pub const REVALIDATE_INTERVAL: Duration = Duration::from_secs(15);

/// 照合 1 回の上限。超えたら「照合できない」（閉じない）。周期より短い。
pub const REVALIDATE_TIMEOUT: Duration = Duration::from_secs(5);

const _: () = assert!(REVALIDATE_TIMEOUT.as_nanos() < REVALIDATE_INTERVAL.as_nanos());

/// 資格情報が使えないと確認できたときの close code。RFC 6455 の 1008
/// (Policy Violation) - 「このエンドポイントの方針に反する（もう認められて
/// いない）」。理由は close フレームの理由文（`api_key_revoked` 等）で分ける。
pub const REVOKED_CLOSE_CODE: u16 = 1008;

/// 再検証の間隔と 1 回の上限。本番は [`Default`]（15 秒 / 5 秒）。テストは
/// router に `Extension(StreamRevalidationTiming { .. })` を重ねて短くする
/// （[`ws_upgrade`] が extensions から読む。外部のリクエストからは差し込め
/// ない）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamRevalidationTiming {
    pub interval: Duration,
    pub timeout: Duration,
}

impl Default for StreamRevalidationTiming {
    fn default() -> Self {
        Self {
            interval: REVALIDATE_INTERVAL,
            timeout: REVALIDATE_TIMEOUT,
        }
    }
}

/// 1 回の照合の結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecheckVerdict {
    /// まだ使える。
    Valid,
    /// 使えないと確認できた（閉じる）。`reason` は close フレームの理由文。
    Revoked { reason: &'static str },
    /// 照合できなかった（DB エラー・タイムアウト）。閉じない。
    Unknown,
}

/// ストリームを開いた資格情報の照合し直し方。API キー用
/// （[`ApiKeyStreamCredential`]）とセッション用（`TODO(#239)`）を差し替え
/// られるようにする。照合は `'static` な future を返す（ストリームの
/// ループが保持したまま、配信と並行に進めるため）。
pub trait StreamCredential: Send + Sync + 'static {
    fn recheck(&self) -> futures_util::future::BoxFuture<'static, RecheckVerdict>;
}

/// API キーで開いたストリームの資格情報。`crate::rest::require_tag_space_auth`
/// が API キーの認証に通った要求の extensions に載せる（[`Self::new`]）。
///
/// 平文のキーは、照合（[`ApiKeysService::check`]）に渡す以外に外へ出さない
/// （フィールドは非公開、`Debug` も持たない）。持つ期間はストリームが開いて
/// いる間だけ。
#[derive(Clone)]
pub(crate) struct ApiKeyStreamCredential {
    api_keys: ApiKeysService,
    token: String,
    manager: Arc<CollectorManager>,
}

impl ApiKeyStreamCredential {
    pub(crate) fn new(
        api_keys: ApiKeysService,
        token: String,
        manager: Arc<CollectorManager>,
    ) -> Self {
        Self {
            api_keys,
            token,
            manager,
        }
    }
}

impl StreamCredential for ApiKeyStreamCredential {
    fn recheck(&self) -> futures_util::future::BoxFuture<'static, RecheckVerdict> {
        let api_keys = self.api_keys.clone();
        let token = self.token.clone();
        let manager = self.manager.clone();
        Box::pin(async move {
            let now_ms = manager.clock().now_ms();
            api_key_recheck_verdict(api_keys.check(&token, now_ms).await)
        })
    }
}

/// API キーの照合の結果（#434 / #435 と同じ分類）を再検証の結果に変える純関数。
pub fn api_key_recheck_verdict(check: ApiKeyCheck) -> RecheckVerdict {
    match api_key_verdict(check) {
        Ok(_) => RecheckVerdict::Valid,
        Err((ApiKeyRejection::Unauthenticated(UnauthenticatedReason::Revoked), _)) => {
            RecheckVerdict::Revoked {
                reason: "api_key_revoked",
            }
        }
        Err((ApiKeyRejection::Unauthenticated(UnauthenticatedReason::Expired), _)) => {
            RecheckVerdict::Revoked {
                reason: "api_key_expired",
            }
        }
        Err((ApiKeyRejection::Unauthenticated(UnauthenticatedReason::NotFound), _)) => {
            RecheckVerdict::Revoked {
                reason: "api_key_not_found",
            }
        }
        Err((ApiKeyRejection::Tripped, _)) => RecheckVerdict::Revoked {
            reason: "api_key_tripped",
        },
        Err((ApiKeyRejection::Unavailable(err), _)) => {
            eprintln!(
                "banto-hub: ストリームの API キーを照合できませんでした（閉じずに続けます）: {err}"
            );
            RecheckVerdict::Unknown
        }
    }
}

/// ストリーム 1 本の再検証のタイマーと、照合中の future（このモジュールの
/// doc comment「接続中の再検証」）。[`Self::next_verdict`] はキャンセルされても
/// 状態（期限・照合中の future）を `self` に残すので、`select!` の分岐に
/// そのまま置ける。
pub(crate) struct Revalidator {
    credential: Arc<dyn StreamCredential>,
    timing: StreamRevalidationTiming,
    next: tokio::time::Instant,
    in_flight: Option<futures_util::future::BoxFuture<'static, RecheckVerdict>>,
}

impl Revalidator {
    /// 最初の期限は開いてから 1 周期後（開いた時点は認証の層が照合済み）。
    pub(crate) fn new(
        credential: Arc<dyn StreamCredential>,
        timing: StreamRevalidationTiming,
    ) -> Self {
        Self {
            credential,
            timing,
            next: tokio::time::Instant::now() + timing.interval,
            in_flight: None,
        }
    }

    /// 次の照合が終わったら、その結果を返す。期限が来たら照合を 1 つ始め
    /// （上限 `timing.timeout`、超えたら [`RecheckVerdict::Unknown`]）、
    /// 終わった時点から次の期限を 1 周期後に置く。
    pub(crate) async fn next_verdict(&mut self) -> RecheckVerdict {
        if self.in_flight.is_none() {
            tokio::time::sleep_until(self.next).await;
            let check = self.credential.recheck();
            let timeout = self.timing.timeout;
            self.in_flight = Some(Box::pin(async move {
                match tokio::time::timeout(timeout, check).await {
                    Ok(verdict) => verdict,
                    // 照合の future はここで捨てる（あとから何も変えない）。
                    Err(_) => {
                        eprintln!(
                            "banto-hub: ストリームの資格情報の照合が {} ms 以内に終わりませんでした（閉じずに続けます）",
                            timeout.as_millis()
                        );
                        RecheckVerdict::Unknown
                    }
                }
            }));
        }
        let verdict = match self.in_flight.as_mut() {
            Some(check) => check.await,
            None => RecheckVerdict::Unknown,
        };
        self.in_flight = None;
        self.next = tokio::time::Instant::now() + self.timing.interval;
        verdict
    }
}

/// [`Revalidator`] が無いストリーム（`TODO(#239)`: いまはセッション）では
/// 永久に来ない分岐にする。
async fn next_revalidation(revalidator: &mut Option<Revalidator>) -> RecheckVerdict {
    match revalidator {
        Some(revalidator) => revalidator.next_verdict().await,
        None => std::future::pending().await,
    }
}

// --- ルーティング -----------------------------------------------------------

/// `GET /api/v1/stream` ハンドラ。`crate::rest::tag_space_router` に直接
/// マウントされ、他の `/api/v1/*` と同じ `require_tag_space_auth`
/// （read スコープ必須）を通る - アップグレードリクエスト自体は普通の
/// HTTP GET なので、ミドルウェアがそのまま効く（設計 §5.2「アップグレード
/// リクエストの Authorization ヘッダで検証」）。
///
/// `ctx`(H10 ③、Option B): `require_tag_space_auth` が API キー認証時に
/// 挿入した [`ApiKeyContext`] を extensions から取り出す
/// （`crate::rest::v1_write_value` と同じパターン）。session token 認証
/// なら extension は無く `None` - [`handle_socket`] 以下へその
/// `Option<ApiKeyContext>` をそのまま持ち回し（一部を borrow するだけの
/// `Option<&ApiKeyContext>` ではなく所有権ごと渡す - コネクションの生存
/// 期間中ずっと必要で、`ApiKeyContext` は安価にクローンできるが、この
/// 経路では move で足りるためクローンもしない)、`resolve` した購読対象を
/// per-tag read スコープで絞る（`crate::subscribe_core` のモジュール doc
/// comment「per-tag read スコープの交差」参照）。`None` は無フィルタ
/// （session token = 従来どおり全アクセス、管理 UI 不変）。
pub(crate) async fn ws_upgrade(
    ws: WebSocketUpgrade,
    State(state): State<TagSpaceState>,
    ctx: Option<Extension<ApiKeyContext>>,
    credential: Option<Extension<ApiKeyStreamCredential>>,
    timing: Option<Extension<StreamRevalidationTiming>>,
) -> Response {
    let manager = state.manager;
    let scope = ctx.map(|Extension(ctx)| ctx);
    // #430: API キーで開いたストリームは接続中も再検証する（このモジュールの
    // doc comment「接続中の再検証」）。
    // TODO(#239): セッションで開いたストリーム（`/api/v1/stream` のセッション・
    // `/api/tag-stream`）は、banto-server の `AuthState::revalidate` が公開
    // されたら、それを呼ぶ `StreamCredential` を足してここで同じく渡す。今は
    // 従来どおり接続時の検証だけ。
    let timing = timing.map(|Extension(timing)| timing).unwrap_or_default();
    let revalidator = credential.map(|Extension(credential)| {
        Revalidator::new(Arc::new(credential) as Arc<dyn StreamCredential>, timing)
    });
    // T10（判断の記録、2026-08-07、`rest.rs::extract_ws_protocol_token` の
    // doc comment も参照）: `.protocols(["bearer"])` は**選択**であって
    // **無条件エコー**ではない - axum の実装（`WebSocketUpgrade::protocols`）
    // はクライアントが実際にリクエストへ `Sec-WebSocket-Protocol` を含めて
    // いた場合に限り、その中に "bearer" があれば応答へエコーする。
    // クライアントが何もオファーしていなければ（`Authorization` ヘッダで
    // 認証する既存の Rust テスト・API キークライアントは何もオファーし
    // ない）応答は素のままで、RFC 6455 が禁じる「オファーされていない
    // サブプロトコルの一方的な選択」には当たらない。
    //
    // これが要る理由: このリポジトリのテストクライアント
    // `tokio-tungstenite`（`tungstenite` 0.29）は、クライアントが
    // `Sec-WebSocket-Protocol` をオファーしたにもかかわらず応答に同ヘッダが
    // 一切無いと、ハンドシェイク自体をクライアント側で `NoSubProtocol`
    // エラーとして拒否する（RFC 6455 の文言そのものはここまで厳格ではない
    // が、`tungstenite::handshake::client` の実装がそう検証している - 実測
    // 済み）。ブラウザはこのケースでもエラーにせず `.protocol` が空文字に
    // なるだけなので、この変更はブラウザ向けの動作を壊さず、むしろ
    // `Sec-WebSocket-Protocol` 認証を使う全クライアント（ブラウザ・この
    // テストスイート）でハンドシェイクが一貫して成功するようにする。
    ws.protocols(["bearer"])
        .on_upgrade(move |socket| handle_socket(socket, manager, scope, revalidator))
}

pub(crate) async fn handle_socket(
    socket: WebSocket,
    manager: Arc<CollectorManager>,
    scope: Option<ApiKeyContext>,
    mut revalidator: Option<Revalidator>,
) {
    let (sink, mut incoming) = socket.split();
    let (data_tx, data_rx) = mpsc::channel::<Message>(OUTBOUND_QUEUE_CAPACITY);
    let (close_tx, close_rx) = mpsc::channel::<CloseFrame>(1);
    let writer = tokio::spawn(writer_task(sink, data_rx, close_rx));

    let mut events = manager.subscribe_events();
    let mut revision_rx = manager.subscribe_revision();
    let mut subscriptions: HashMap<i64, Subscription> = HashMap::new();

    let mut tick = tokio::time::interval(Duration::from_millis(EVAL_TICK_MS as u64));
    // Delay（Skip 相当）: タスクが一時的に詰まっても、詰まった分をまとめて
    // 送りつけるのではなく単に次回を遅らせる - 評価ループはあくまで
    // 「250ms おきにだいたい評価する」ためのものであって、正確な発火回数を
    // 保証する必要はない（設計はそもそも「250ms 固定でよい」という緩い
    // 要求）。
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        let should_continue = tokio::select! {
            // #430: 照合が終わったら判定する（照合は配信と並行に進む -
            // `Revalidator` のキャンセル安全性はその doc comment 参照）。
            verdict = next_revalidation(&mut revalidator) => match verdict {
                RecheckVerdict::Revoked { reason } => {
                    let _ = close_tx.try_send(CloseFrame {
                        code: REVOKED_CLOSE_CODE,
                        reason: reason.into(),
                    });
                    false
                }
                // 照合できない（DB エラー・タイムアウト）ときは閉じない。
                RecheckVerdict::Valid | RecheckVerdict::Unknown => true,
            },
            msg = incoming.next() => match msg {
                Some(Ok(Message::Text(text))) => {
                    handle_text(
                        &text,
                        &manager,
                        &mut subscriptions,
                        &data_tx,
                        &close_tx,
                        scope.as_ref(),
                    )
                    .await
                }
                Some(Ok(Message::Close(_))) => false,
                Some(Ok(Message::Binary(_))) => {
                    send_error(None, "invalid_request", "テキスト以外のフレームは扱えません".to_string(), &data_tx, &close_tx)
                }
                // Ping/Pong: axum/tungstenite が WS プロトコルレベルの
                // 応答を自動で行う（axum::extract::ws::Message の doc
                // comment 参照）ので、ここでは無視するだけでよい。
                Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) => true,
                Some(Err(_)) | None => false,
            },
            _ = tick.tick() => {
                evaluate(&manager, &mut subscriptions, &data_tx, &close_tx, scope.as_ref())
            },
            event = events.recv() => match event {
                Ok(event) => send_event(&event, &manager.pool(), &data_tx, &close_tx).await,
                // broadcast の遅延受信者はスキップするだけ（設計 §5.2/§3.5
                // 「lag はスキップ」）。
                Err(broadcast::error::RecvError::Lagged(_)) => true,
                // 送信側（CollectorManager）が生きている限り起きない -
                // 起きたらプロセス終了間際なのでコネクションも畳む。
                Err(broadcast::error::RecvError::Closed) => false,
            },
            changed = revision_rx.changed() => match changed {
                Ok(()) => send_config_changed(*revision_rx.borrow(), &data_tx, &close_tx),
                Err(_) => false,
            },
        };
        if !should_continue {
            break;
        }
    }

    // `data_tx`/`close_tx` の drop で writer タスクへチャネル終了を伝える -
    // バックプレッシャ切断の場合は既に `close_tx` へ送信済みなので、writer
    // はその close frame を送ってから終了する。
    drop(data_tx);
    drop(close_tx);
    let _ = writer.await;
}

/// 送信専用タスク: `WebSocket` の送信半分をここに閉じ込め、遅いクライアント
/// への `.send().await` のブロッキングが `handle_socket` の評価/受信ループを
/// 巻き込まないようにする（このモジュールの doc comment 参照）。
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

// --- 購読状態 ----------------------------------------------------------------
//
// T4（設計 §5.4）: `TagPattern`/`resolve`/`Mode`/`Subscription`/
// `interval_floor_ms`/評価本体は `crate::subscribe_core` へ抽出した
// （gRPC の `StreamValues` と共有 - モジュール doc comment参照）。以下は
// WebSocket 固有のワイヤ形式（`ValueWire` 等）への変換のみを行う。

// --- クライアント → サーバー のメッセージ -----------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ModeWire {
    OnChange,
    Interval,
}

#[derive(Debug, Deserialize)]
struct SubscribeWire {
    id: i64,
    tags: Vec<String>,
    mode: ModeWire,
    #[serde(default)]
    interval_ms: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct UnsubscribeWire {
    id: i64,
}

/// 1本の受信テキストフレームを処理する。戻り値はコネクションを維持して
/// よいか（`false` はバックプレッシャ切断が発生したことを意味する - 送信
/// キューが満杯で `error`/`pong` すら送れなかった場合）。
async fn handle_text(
    text: &str,
    manager: &CollectorManager,
    subscriptions: &mut HashMap<i64, Subscription>,
    data_tx: &mpsc::Sender<Message>,
    close_tx: &mpsc::Sender<CloseFrame>,
    scope: Option<&ApiKeyContext>,
) -> bool {
    let value: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(err) => {
            return send_error(
                None,
                "invalid_request",
                format!("JSON として解釈できません: {err}"),
                data_tx,
                close_tx,
            );
        }
    };
    let id_hint = value.get("id").and_then(Value::as_i64);
    let op = value.get("op").and_then(Value::as_str).unwrap_or("");

    match op {
        "subscribe" => match serde_json::from_value::<SubscribeWire>(value) {
            Ok(msg) => {
                handle_subscribe(msg, manager, subscriptions, data_tx, close_tx, scope).await
            }
            Err(err) => send_error(
                id_hint,
                "invalid_request",
                err.to_string(),
                data_tx,
                close_tx,
            ),
        },
        "unsubscribe" => match serde_json::from_value::<UnsubscribeWire>(value) {
            Ok(msg) => {
                // 未知の id は無視（冪等 - 設計はこのケースを明記していない
                // が、`crate::api_keys::ApiKeysService::revoke` 等このコード
                // ベースの他の「id 指定の取り消し系操作」と同じ規律に合わせ
                // た判断）。
                subscriptions.remove(&msg.id);
                true
            }
            Err(err) => send_error(
                id_hint,
                "invalid_request",
                err.to_string(),
                data_tx,
                close_tx,
            ),
        },
        "ping" => send_json(&PongWire { op: "pong" }, data_tx, close_tx),
        other => send_error(
            id_hint,
            "invalid_request",
            format!("未知の op です: {other}"),
            data_tx,
            close_tx,
        ),
    }
}

async fn handle_subscribe(
    msg: SubscribeWire,
    manager: &CollectorManager,
    subscriptions: &mut HashMap<i64, Subscription>,
    data_tx: &mpsc::Sender<Message>,
    close_tx: &mpsc::Sender<CloseFrame>,
    scope: Option<&ApiKeyContext>,
) -> bool {
    if msg.tags.is_empty() {
        return send_error(
            Some(msg.id),
            "invalid_request",
            "tags が空です".to_string(),
            data_tx,
            close_tx,
        );
    }

    // 2026-09-15 オーナー決定（#335 追補、「外部出力を PLC への出力と
    // 勘違いしていた」）: 以前ここにあった「API キー購読は run が
    // AllSimulation 中なら simulation_output_disabled で拒否」は撤去した -
    // 外部購読も run mode によらず継続する。値の `value_source`/
    // `collection_mode`（`ValueWire`/`crate::rest`の`GET /api/v1/status`）で
    // 判別させる。T15-3 の `TestOutputControl`/`test_output` opt-in は元々
    // WS には無かった（gRPC 専用の仕組み）ので、この変更で WS 固有の
    // deprecated 概念は増えない。
    let mut patterns = Vec::with_capacity(msg.tags.len());
    for raw in &msg.tags {
        match TagPattern::parse(raw) {
            Ok(pattern) => patterns.push(pattern),
            Err(detail) => {
                return send_error(Some(msg.id), "invalid_request", detail, data_tx, close_tx)
            }
        }
    }

    let map = manager.tag_map();

    // 設計 §5.2 要件4: 未知の**具体名**（ワイルドカードでない）が混ざって
    // いたら購読自体を拒否する（REST `?tags=` と同じ「部分成功で誤解させ
    // ない」規律）。ワイルドカードは0件マッチでもエラーにしない。
    //
    // H10 ③(Option B): この存在チェックは catalog(絞らない)にのみ照らす
    // - per-tag read スコープはここでは見ない。スコープ外の具体名を挙げた
    // subscribe 自体は成立させ、値は resolve 段(下の initial_values/
    // evaluate)で単に「常に0件マッチ」として扱う - 単一/バルクの明示
    // `?tags=` が 403 で即座に拒否するのとは非対称だが、購読プロトコルに
    // 403 相当のエラーコードが無く、新設しても得られる情報(「そのタグは
    // 存在する」)は catalog が既に開示済みなので実害が無いための判断。
    for pattern in &patterns {
        if let TagPattern::Exact(name) = pattern {
            if map.get(name).is_none() {
                return send_error(
                    Some(msg.id),
                    "unknown_tag",
                    format!("未知のタグです: {name}"),
                    data_tx,
                    close_tx,
                );
            }
        }
    }

    let mode = match msg.mode {
        ModeWire::OnChange => Mode::OnChange,
        ModeWire::Interval => {
            let Some(requested) = msg.interval_ms else {
                return send_error(
                    Some(msg.id),
                    "invalid_request",
                    "mode=interval には interval_ms が必須です".to_string(),
                    data_tx,
                    close_tx,
                );
            };
            if requested <= 0 {
                return send_error(
                    Some(msg.id),
                    "invalid_request",
                    "interval_ms は正の値である必要があります".to_string(),
                    data_tx,
                    close_tx,
                );
            }
            // クランプ採用（下回る指定を invalid_request で拒否しない）-
            // このモジュールの doc comment「interval の下限クランプ」参照。
            let floor = interval_floor_ms(&patterns, &map);
            Mode::Interval {
                interval_ms: requested.max(floor),
            }
        }
    };

    let now_ms = manager.clock().now_ms();
    let current = manager.current_values();
    let server_store = manager.server_store();
    let (initial, last) = subscribe_core::initial_values(
        &patterns,
        &map,
        current.as_ref(),
        &server_store,
        now_ms,
        scope,
    );
    let values: Vec<ValueWire> = initial.into_iter().map(ValueWire::from).collect();

    let next_due_ms = match mode {
        Mode::Interval { interval_ms } => now_ms + interval_ms,
        Mode::OnChange => 0,
    };

    // id 重複 subscribe は置き換え（設計 §5.2 要件1）- 単純な `insert` が
    // それを満たす（既存エントリがあれば上書き、無ければ新規）。
    subscriptions.insert(
        msg.id,
        Subscription {
            patterns,
            mode,
            last,
            next_due_ms,
        },
    );

    // 設計 §5.2 要件5: subscribe 受理直後は必ず1回 data を送る（空でも）。
    send_data(msg.id, now_ms, values, data_tx, close_tx)
}

/// 250ms ごとの評価タイマ本体。全購読を1回ずつ評価し、on_change の diff
/// 検出と interval の期限到来判定を行う（本体は `crate::subscribe_core::evaluate`。
/// このモジュールの doc comment「購読状態」参照）。戻り値はコネクションを
/// 維持してよいか（バックプレッシャ切断で `false`）。
fn evaluate(
    manager: &CollectorManager,
    subscriptions: &mut HashMap<i64, Subscription>,
    data_tx: &mpsc::Sender<Message>,
    close_tx: &mpsc::Sender<CloseFrame>,
    scope: Option<&ApiKeyContext>,
) -> bool {
    if subscriptions.is_empty() {
        return true;
    }

    let map = manager.tag_map();
    let now_ms = manager.clock().now_ms();
    let current = manager.current_values();
    let server_store = manager.server_store();

    for (&id, sub) in subscriptions.iter_mut() {
        if let Some(values) =
            subscribe_core::evaluate(sub, &map, current.as_ref(), &server_store, now_ms, scope)
        {
            let values: Vec<ValueWire> = values.into_iter().map(ValueWire::from).collect();
            if !send_data(id, now_ms, values, data_tx, close_tx) {
                return false;
            }
        }
    }
    true
}

// --- サーバー → クライアント のメッセージ -----------------------------------

#[derive(Debug, Serialize)]
struct ValueWire {
    tag: String,
    v: Option<f64>,
    q: &'static str,
    t: i64,
}

/// `crate::subscribe_core::ResolvedValue`(transport 非依存)から WS の
/// ワイヤ表現への変換 - T4 で `crate::subscribe_core` へ評価ロジックを
/// 抽出した後、このモジュールに残る唯一の「値」変換点。
impl From<ResolvedValue> for ValueWire {
    fn from(value: ResolvedValue) -> Self {
        ValueWire {
            tag: value.tag,
            v: value.v,
            q: quality_str(value.q),
            t: value.t,
        }
    }
}

#[derive(Debug, Serialize)]
struct DataWire {
    op: &'static str,
    id: i64,
    t: i64,
    values: Vec<ValueWire>,
}

#[derive(Debug, Serialize)]
struct ErrorWire {
    op: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<i64>,
    code: &'static str,
    detail: String,
}

#[derive(Debug, Serialize)]
struct EventWire {
    op: &'static str,
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    connection: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    level: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
    t: i64,
}

#[derive(Debug, Serialize)]
struct ConfigChangedWire {
    op: &'static str,
    revision: u64,
}

#[derive(Debug, Serialize)]
struct PongWire {
    op: &'static str,
}

fn send_data(
    id: i64,
    t: i64,
    values: Vec<ValueWire>,
    data_tx: &mpsc::Sender<Message>,
    close_tx: &mpsc::Sender<CloseFrame>,
) -> bool {
    send_json(
        &DataWire {
            op: "data",
            id,
            t,
            values,
        },
        data_tx,
        close_tx,
    )
}

fn send_error(
    id: Option<i64>,
    code: &'static str,
    detail: String,
    data_tx: &mpsc::Sender<Message>,
    close_tx: &mpsc::Sender<CloseFrame>,
) -> bool {
    send_json(
        &ErrorWire {
            op: "error",
            id,
            code,
            detail,
        },
        data_tx,
        close_tx,
    )
}

/// `CollectEvent::connection_key` は `banto_collect` の内部キー形式
/// `"conn:{id}"`（`crates/banto-collect/src/config.rs::connection_key` -
/// `crate::hub::tag_key` の `"tag:{id}"` と同じ流儀）であって、設計 §5.2 の
/// 例 `{ "connection": "line1" }` が示す**外部名**ではない。イベントは稀
/// （PLC 接続/断/再接続・しきい値のみ）なので、都度レジストリへ1回引き
/// （`plc_connections.name`）に行っても実害はない - `crate::rest`'s
/// `v1_status` が同じ理由で毎リクエスト `PlcConnectionService::list` を
/// 引いているのと同じ判断（catalog 側 `TagMap` は「タグを持つ接続」しか
/// 反映しないため、タグ0件の接続の断イベントを引けない - レジストリ直読み
/// のほうが正しい）。見つからなければ（削除された接続の残留イベント等）
/// `None` のまま送る - `EventWire::connection` は `skip_serializing_if`。
pub(crate) async fn resolve_connection_name(
    pool: &SqlitePool,
    connection_key: &str,
) -> Option<String> {
    let id: i64 = connection_key.strip_prefix("conn:")?.parse().ok()?;
    sqlx::query_scalar("SELECT name FROM plc_connections WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
}

async fn send_event(
    event: &CollectEvent,
    pool: &SqlitePool,
    data_tx: &mpsc::Sender<Message>,
    close_tx: &mpsc::Sender<CloseFrame>,
) -> bool {
    let connection = match &event.connection_key {
        Some(key) => resolve_connection_name(pool, key).await,
        None => None,
    };
    let wire = EventWire {
        op: "event",
        kind: event.kind.as_str(),
        connection,
        tag: event.tag_key.clone(),
        level: event.level.map(|l| l.as_str()),
        value: event.value,
        detail: event.detail.clone(),
        t: event.ts_ms,
    };
    send_json(&wire, data_tx, close_tx)
}

fn send_config_changed(
    revision: u64,
    data_tx: &mpsc::Sender<Message>,
    close_tx: &mpsc::Sender<CloseFrame>,
) -> bool {
    send_json(
        &ConfigChangedWire {
            op: "config_changed",
            revision,
        },
        data_tx,
        close_tx,
    )
}

fn send_json<T: Serialize>(
    payload: &T,
    data_tx: &mpsc::Sender<Message>,
    close_tx: &mpsc::Sender<CloseFrame>,
) -> bool {
    let text = serde_json::to_string(payload).expect("stream.rs のワイヤ型は常にシリアライズ可能");
    enqueue(Message::Text(text.into()), data_tx, close_tx)
}

/// キューへの投入を試み、満杯なら（設計 §5.2 要件6）close 信号を最優先
/// チャネルへ送ってバックプレッシャ切断を発火させる。このモジュールの doc
/// comment「バックプレッシャ切断」参照。
fn enqueue(
    msg: Message,
    data_tx: &mpsc::Sender<Message>,
    close_tx: &mpsc::Sender<CloseFrame>,
) -> bool {
    match data_tx.try_send(msg) {
        Ok(()) => true,
        Err(_) => {
            let _ = close_tx.try_send(CloseFrame {
                code: BACKPRESSURE_CLOSE_CODE,
                reason: "send queue full (slow subscriber)".into(),
            });
            false
        }
    }
}

#[cfg(test)]
mod tests {
    //! #430: 接続中の再検証。照合を差し替えられる [`StreamCredential`] の
    //! 偽物で、[`Revalidator`] の時間の規律（仮想時間）と、[`handle_socket`]
    //! に載せたときの振る舞い（実 WebSocket）を確かめる。実際の API キーでの
    //! 確認は `tests/stream.rs`（失効・トリップ・期限切れ・DB エラー・DB が
    //! 答えない）。
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    use crate::broker_glue::{BrokerSimRegistry, HubSessions};
    use crate::computed::{ComputedEngine, ServerTagStore};
    use crate::db::init_db;
    use banto_collect::CollectorOptions;
    use banto_tstore::SystemClock;
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    /// 偽の照合が返すもの。
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Answer {
        Valid,
        Revoked,
        Unknown,
        /// 返らない（上限で打ち切られるまで待つ）。
        Hang,
    }

    /// 偽の照合の観測点。
    struct Probe {
        answer: Mutex<Answer>,
        /// 照合 1 回にかかる時間（答えを返す前に待つ）。
        delay: Duration,
        /// 照合を始めた時刻。
        starts: Mutex<Vec<tokio::time::Instant>>,
        /// 始めた照合の数。
        calls: AtomicUsize,
        /// いま進行中の照合の数（終わるか、捨てられたら減る）。
        in_flight: AtomicUsize,
        /// 終わらずに捨てられた（打ち切り・切断）照合の数。
        dropped: AtomicUsize,
        /// 生きている偽の資格情報の数（ストリームが持つ）。
        live: AtomicUsize,
    }

    impl Probe {
        fn new(answer: Answer, delay: Duration) -> Arc<Self> {
            Arc::new(Self {
                answer: Mutex::new(answer),
                delay,
                starts: Mutex::new(Vec::new()),
                calls: AtomicUsize::new(0),
                in_flight: AtomicUsize::new(0),
                dropped: AtomicUsize::new(0),
                live: AtomicUsize::new(0),
            })
        }

        fn set(&self, answer: Answer) {
            *self.answer.lock().unwrap() = answer;
        }

        fn credential(self: &Arc<Self>) -> Arc<dyn StreamCredential> {
            self.live.fetch_add(1, Ordering::SeqCst);
            Arc::new(FakeCredential {
                probe: self.clone(),
            })
        }
    }

    struct FakeCredential {
        probe: Arc<Probe>,
    }

    impl Drop for FakeCredential {
        fn drop(&mut self) {
            self.probe.live.fetch_sub(1, Ordering::SeqCst);
        }
    }

    /// 照合の future が、終わったか・終わらずに捨てられたかを数える。
    struct InFlight {
        probe: Arc<Probe>,
        finished: bool,
    }

    impl Drop for InFlight {
        fn drop(&mut self) {
            self.probe.in_flight.fetch_sub(1, Ordering::SeqCst);
            if !self.finished {
                self.probe.dropped.fetch_add(1, Ordering::SeqCst);
            }
        }
    }

    impl StreamCredential for FakeCredential {
        fn recheck(&self) -> futures_util::future::BoxFuture<'static, RecheckVerdict> {
            let probe = self.probe.clone();
            probe.calls.fetch_add(1, Ordering::SeqCst);
            probe.in_flight.fetch_add(1, Ordering::SeqCst);
            probe
                .starts
                .lock()
                .unwrap()
                .push(tokio::time::Instant::now());
            // 作った時点から数える（一度も poll されずに捨てられても数える）。
            let guard = InFlight {
                probe: probe.clone(),
                finished: false,
            };
            Box::pin(async move {
                // フィールドだけでなく guard ごと future に持たせる。
                let mut guard = guard;
                tokio::time::sleep(probe.delay).await;
                let answer = *probe.answer.lock().unwrap();
                let verdict = match answer {
                    Answer::Valid => RecheckVerdict::Valid,
                    Answer::Revoked => RecheckVerdict::Revoked {
                        reason: "api_key_revoked",
                    },
                    Answer::Unknown => RecheckVerdict::Unknown,
                    Answer::Hang => std::future::pending().await,
                };
                guard.finished = true;
                verdict
            })
        }
    }

    fn timing(interval_ms: u64, timeout_ms: u64) -> StreamRevalidationTiming {
        StreamRevalidationTiming {
            interval: Duration::from_millis(interval_ms),
            timeout: Duration::from_millis(timeout_ms),
        }
    }

    // --- Revalidator の時間の規律（仮想時間） --------------------------------

    /// 次の期限は照合が**終わった**時点から 1 周期後（開始からではない）。
    #[tokio::test(start_paused = true)]
    async fn the_next_deadline_is_one_interval_after_the_check_finished() {
        let probe = Probe::new(Answer::Valid, Duration::from_millis(40));
        let t0 = tokio::time::Instant::now();
        let mut revalidator = Revalidator::new(probe.credential(), timing(100, 1_000));

        assert_eq!(revalidator.next_verdict().await, RecheckVerdict::Valid);
        assert_eq!(revalidator.next_verdict().await, RecheckVerdict::Valid);

        let starts = probe.starts.lock().unwrap().clone();
        assert_eq!(
            starts,
            vec![
                t0 + Duration::from_millis(100),
                // 1 回目が 140ms に終わり、その 1 周期後。
                t0 + Duration::from_millis(240),
            ]
        );
    }

    /// 上限を超えた照合は「照合できない」。捨てた future はそれきりで、次の
    /// 期限は打ち切った時点から 1 周期後。
    #[tokio::test(start_paused = true)]
    async fn a_check_that_does_not_return_is_abandoned_as_unknown() {
        let probe = Probe::new(Answer::Hang, Duration::ZERO);
        let t0 = tokio::time::Instant::now();
        let mut revalidator = Revalidator::new(probe.credential(), timing(100, 30));

        assert_eq!(revalidator.next_verdict().await, RecheckVerdict::Unknown);
        assert_eq!(probe.dropped.load(Ordering::SeqCst), 1);
        assert_eq!(probe.in_flight.load(Ordering::SeqCst), 0);
        assert_eq!(tokio::time::Instant::now(), t0 + Duration::from_millis(130));

        probe.set(Answer::Revoked);
        assert_eq!(
            revalidator.next_verdict().await,
            RecheckVerdict::Revoked {
                reason: "api_key_revoked"
            }
        );
        assert_eq!(
            *probe.starts.lock().unwrap().last().unwrap(),
            t0 + Duration::from_millis(230)
        );
    }

    /// `select!` で何度キャンセルされても、照合は同時に 1 つだけで、その間も
    /// ほかの分岐（配信）は進む。
    #[tokio::test(start_paused = true)]
    async fn the_check_runs_alongside_delivery_one_at_a_time() {
        let probe = Probe::new(Answer::Valid, Duration::from_millis(40));
        let mut revalidator = Some(Revalidator::new(probe.credential(), timing(100, 1_000)));
        let mut tick = tokio::time::interval(Duration::from_millis(10));
        let mut verdicts = 0;
        let mut ticks_during_checks = 0;
        while verdicts < 3 {
            tokio::select! {
                _ = next_revalidation(&mut revalidator) => verdicts += 1,
                _ = tick.tick() => {
                    let in_flight = probe.in_flight.load(Ordering::SeqCst);
                    assert!(in_flight <= 1, "two checks in flight at once");
                    if in_flight == 1 {
                        ticks_during_checks += 1;
                    }
                }
            }
        }
        assert_eq!(probe.calls.load(Ordering::SeqCst), 3);
        assert_eq!(probe.dropped.load(Ordering::SeqCst), 0);
        assert!(
            ticks_during_checks >= 3,
            "delivery must go on while a check is in flight (got {ticks_during_checks} ticks)"
        );
    }

    /// 資格情報の無いストリーム（`TODO(#239)`: いまはセッション）では再検証の
    /// 分岐は来ない。
    #[tokio::test(start_paused = true)]
    async fn a_stream_without_a_credential_never_rechecks() {
        let mut revalidator: Option<Revalidator> = None;
        let result = tokio::time::timeout(
            Duration::from_secs(3600),
            next_revalidation(&mut revalidator),
        )
        .await;
        assert!(result.is_err());
    }

    /// API キーの照合の結果の分類（#434 / #435 と同じ）を表で確かめる。
    #[test]
    fn api_key_checks_map_to_recheck_verdicts() {
        use crate::api_keys::ApiKeyLookup;
        use banto_core::BantoError;
        let ctx = ApiKeyContext {
            id: 1,
            name: "k".to_string(),
            scopes: vec!["read".to_string()],
            last_used_at_ms: None,
        };
        let key = || (1, "k".to_string());
        let revoked = |reason| RecheckVerdict::Revoked { reason };
        let cases = [
            (
                ApiKeyCheck::Answered(ApiKeyLookup::Valid(ctx)),
                RecheckVerdict::Valid,
            ),
            (
                ApiKeyCheck::Answered(ApiKeyLookup::Revoked {
                    id: key().0,
                    name: key().1,
                }),
                revoked("api_key_revoked"),
            ),
            (
                ApiKeyCheck::Answered(ApiKeyLookup::Expired {
                    id: key().0,
                    name: key().1,
                }),
                revoked("api_key_expired"),
            ),
            (
                ApiKeyCheck::Answered(ApiKeyLookup::Tripped {
                    id: key().0,
                    name: key().1,
                }),
                revoked("api_key_tripped"),
            ),
            (
                ApiKeyCheck::Answered(ApiKeyLookup::NotFound),
                revoked("api_key_not_found"),
            ),
            (
                ApiKeyCheck::Unavailable(BantoError::Storage("db down".to_string())),
                RecheckVerdict::Unknown,
            ),
        ];
        for (check, expected) in cases {
            assert_eq!(api_key_recheck_verdict(check), expected);
        }
    }

    // --- handle_socket に載せたとき（実 WebSocket） --------------------------

    type Client = tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >;

    struct Server {
        server: banto_server::RunningServer,
        _manager: Arc<CollectorManager>,
        _dir: crate::test_support::TempDir,
    }

    /// `/ws` に、偽の資格情報で再検証する [`handle_socket`] を置いたサーバー。
    async fn serve(probe: Arc<Probe>, timing: StreamRevalidationTiming) -> Server {
        let dir = crate::test_support::TempDir::new("stream-revalidation");
        let pool = init_db(&dir.path().join("registry.sqlite3"))
            .await
            .expect("init_db");
        let manager = Arc::new(CollectorManager::new(
            pool,
            dir.path().join("data"),
            Arc::new(SystemClock),
            CollectorOptions::default(),
            Arc::new(HubSessions::new(banto_broker::BackoffConfig::default())),
            Arc::new(BrokerSimRegistry::new()),
            Arc::new(ComputedEngine::new(Arc::new(ServerTagStore::new()))),
        ));
        let handler_manager = manager.clone();
        let router = axum::Router::new().route(
            "/ws",
            axum::routing::get(move |ws: WebSocketUpgrade| {
                let manager = handler_manager.clone();
                let credential = probe.credential();
                async move {
                    ws.on_upgrade(move |socket| {
                        handle_socket(
                            socket,
                            manager,
                            None,
                            Some(Revalidator::new(credential, timing)),
                        )
                    })
                }
            }),
        );
        let server = banto_server::start(
            banto_server::ServerConfig {
                bind: "127.0.0.1".to_string(),
                port: 0,
            },
            router,
        )
        .await
        .expect("server should start");
        Server {
            server,
            _manager: manager,
            _dir: dir,
        }
    }

    async fn connect(server: &Server) -> Client {
        let url = format!("ws://127.0.0.1:{}/ws", server.server.local_addr().port());
        tokio_tungstenite::connect_async(url)
            .await
            .expect("ws handshake")
            .0
    }

    /// ping を送り、pong が返ることを確かめる（ストリームが開いていて、
    /// ループが止まっていない）。
    async fn assert_pong(ws: &mut Client) {
        ws.send(WsMessage::Text(r#"{"op":"ping"}"#.into()))
            .await
            .expect("send ping");
        let reply = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match ws.next().await {
                    Some(Ok(WsMessage::Text(text))) => return text.to_string(),
                    Some(Ok(WsMessage::Ping(_) | WsMessage::Pong(_))) => continue,
                    other => panic!("stream ended while waiting for pong: {other:?}"),
                }
            }
        })
        .await
        .expect("pong within 5s");
        assert!(reply.contains("pong"), "{reply}");
    }

    /// close フレームを待って (code, reason) を返す。
    async fn wait_for_close(ws: &mut Client, bound: Duration) -> (u16, String) {
        tokio::time::timeout(bound, async {
            loop {
                match ws.next().await {
                    Some(Ok(WsMessage::Close(Some(frame)))) => {
                        return (u16::from(frame.code), frame.reason.to_string())
                    }
                    Some(Ok(WsMessage::Close(None))) | None | Some(Err(_)) => {
                        panic!("stream ended without a close frame")
                    }
                    Some(Ok(_)) => continue,
                }
            }
        })
        .await
        .expect("close frame within the bound")
    }

    async fn wait_until(bound: Duration, mut condition: impl FnMut() -> bool) -> bool {
        let deadline = tokio::time::Instant::now() + bound;
        while tokio::time::Instant::now() < deadline {
            if condition() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        condition()
    }

    /// 使えないと確認できたら、1 周期以内に 1008 と理由文で閉じる。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_revoked_credential_closes_the_stream_with_1008() {
        let probe = Probe::new(Answer::Valid, Duration::ZERO);
        let server = serve(probe.clone(), timing(100, 1_000)).await;
        let mut ws = connect(&server).await;
        assert!(
            wait_until(Duration::from_secs(5), || probe
                .calls
                .load(Ordering::SeqCst)
                >= 1)
            .await
        );
        assert_pong(&mut ws).await;

        probe.set(Answer::Revoked);
        let (code, reason) = wait_for_close(&mut ws, Duration::from_secs(2)).await;
        assert_eq!(code, REVOKED_CLOSE_CODE);
        assert_eq!(reason, "api_key_revoked");
    }

    /// 照合が返らない・照合できないあいだは閉じず、ループも止まらない。
    /// 答えが戻って使えないと分かったら閉じる。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_check_that_cannot_answer_keeps_the_stream_open_until_revocation_is_confirmed() {
        let probe = Probe::new(Answer::Hang, Duration::ZERO);
        let server = serve(probe.clone(), timing(100, 300)).await;
        let mut ws = connect(&server).await;

        // 返らない照合が 2 回打ち切られるまで、ping に応え続ける。
        while probe.dropped.load(Ordering::SeqCst) < 2 {
            assert_pong(&mut ws).await;
        }
        // 照合できない（DB エラー）が 2 回。
        probe.set(Answer::Unknown);
        let calls = probe.calls.load(Ordering::SeqCst);
        while probe.calls.load(Ordering::SeqCst) < calls + 2 {
            assert_pong(&mut ws).await;
        }
        assert_pong(&mut ws).await;

        probe.set(Answer::Revoked);
        let (code, reason) = wait_for_close(&mut ws, Duration::from_secs(2)).await;
        assert_eq!(code, REVOKED_CLOSE_CODE);
        assert_eq!(reason, "api_key_revoked");
    }

    /// クライアントが切断したら、照合中のものも含めて再検証が止まる
    /// （資格情報と照合の future がストリームと一緒に捨てられる）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_client_disconnect_stops_the_rechecks() {
        let probe = Probe::new(Answer::Hang, Duration::ZERO);
        let server = serve(probe.clone(), timing(50, 10_000)).await;
        let ws = connect(&server).await;
        assert!(
            wait_until(Duration::from_secs(5), || probe
                .in_flight
                .load(Ordering::SeqCst)
                == 1)
            .await,
            "a check should be in flight"
        );
        assert_eq!(probe.live.load(Ordering::SeqCst), 1);

        drop(ws);
        assert!(
            wait_until(Duration::from_secs(5), || {
                probe.live.load(Ordering::SeqCst) == 0
                    && probe.in_flight.load(Ordering::SeqCst) == 0
            })
            .await,
            "the stream's credential and its in-flight check should be dropped on disconnect"
        );
        assert_eq!(probe.calls.load(Ordering::SeqCst), 1);
        assert_eq!(probe.dropped.load(Ordering::SeqCst), 1);
    }
}
