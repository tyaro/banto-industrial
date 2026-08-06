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

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::SqlitePool;
use tokio::sync::{broadcast, mpsc};
use tokio::time::MissedTickBehavior;

use banto_collect::CollectEvent;

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

// --- ルーティング -----------------------------------------------------------

/// `GET /api/v1/stream` ハンドラ。`crate::rest::tag_space_router` に直接
/// マウントされ、他の `/api/v1/*` と同じ `require_tag_space_auth`
/// （read スコープ必須）を通る - アップグレードリクエスト自体は普通の
/// HTTP GET なので、ミドルウェアがそのまま効く（設計 §5.2「アップグレード
/// リクエストの Authorization ヘッダで検証」）。
pub(crate) async fn ws_upgrade(
    ws: WebSocketUpgrade,
    State(state): State<TagSpaceState>,
) -> Response {
    let manager = state.manager;
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
        .on_upgrade(move |socket| handle_socket(socket, manager))
}

async fn handle_socket(socket: WebSocket, manager: Arc<CollectorManager>) {
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
            msg = incoming.next() => match msg {
                Some(Ok(Message::Text(text))) => {
                    handle_text(&text, &manager, &mut subscriptions, &data_tx, &close_tx).await
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
            _ = tick.tick() => evaluate(&manager, &mut subscriptions, &data_tx, &close_tx),
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
            Ok(msg) => handle_subscribe(msg, manager, subscriptions, data_tx, close_tx).await,
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
    let (initial, last) =
        subscribe_core::initial_values(&patterns, &map, current.as_ref(), &server_store, now_ms);
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
            subscribe_core::evaluate(sub, &map, current.as_ref(), &server_store, now_ms)
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
