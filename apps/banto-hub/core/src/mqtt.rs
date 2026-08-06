//! T3（docs/tag-server-design.md §5.3「MQTT publish（T3）」）: 外部 MQTT
//! ブローカーへ接続する**クライアントモード**の発行機能（rumqttc）。組み込み
//! ブローカー（§10-4 の判断待ち）・MQTT 経由の書き込み（§6 で不採用決定）は
//! スコープ外。
//!
//! ```text
//! §5.3 引用:
//! - トピック: {prefix}/{connection}/{group}/{tag}（prefix 既定 banto、設定可）
//! - ペイロード: {"v": 25.4, "q": "good", "t": 1722758400100}（WebSocket と同形）
//! - 発行モード: タグ毎に on_change / interval を設定（既定 on_change、
//!   最短発行間隔でスロットル）。retain 有効
//! - QoS: 既定 1。設定で 0/1 切り替え（2 は使わない）
//! - LWT: {prefix}/$state に online/offline（birth/death）
//! - MQTT 経由の書き込み（.../set 購読）はやらない
//! ```
//!
//! ## 実装判断（このスライスでの段階化・§5.3 追記事項）
//!
//! - **タグ毎の発行モード設定は T3 では未実装**: 設計は「タグ毎に on_change /
//!   interval を設定」と書くが、per-tag 設定を持たせるには I1（`banto-tags`
//!   の `tags` テーブル）にもう1列足すスキーマ変更が要る。このスライスは
//!   REST/UI 設定（[`crate::settings::MqttSettings`]）だけで完結させる段階
//!   実装とし、**全タグ一律 on_change + `min_interval_ms` スロットル**を
//!   既定動作として先行実装する（per-tag 化はバックログ、
//!   docs/tag-server-design.md §5.3 に追記した）。
//! - **削除されたタグの retain クリアはやらない**: `.../set` 相当の空
//!   ペイロード発行を catalog 削除時に打つには「消えた外部名」を追跡する
//!   仕組みが要るが、`crate::hub::TagMap` は revision ごとに丸ごと作り直す
//!   スナップショットで前世代を保持しない（`hub.rs` の doc comment参照）。
//!   古い retain が残る既知の制約として許容する — トピックが catalog の
//!   `{connection}/{group}/{tag}` から機械的に決まるので、購読側は
//!   `GET /api/v1/tags` の現行 catalog と突き合わせれば「もう存在しない
//!   トピック」を判別できる。
//! - **`mqtt.password` は平文保存**: [`crate::settings::MqttSettings`] の
//!   フィールド doc comment参照（§5.6「v1 では平文 + 閉域 LAN 前提」と同じ
//!   線引き）。
//! - **TLS 非対応**: `rumqttc` の既定 feature `use-rustls` を落として依存
//!   （workspace Cargo.toml のコメント参照）。§5.6 の「v1 は平文」前提と
//!   一致する - TLS が要る場合はリバースプロキシでの終端に委譲する設計。
//!
//! ## タスク構成（設計 §3.4「収集に背圧をかけない」）
//!
//! [`MqttPublisher`] は [`CollectorManager`] の `tag_map`/`current_values`/
//! `subscribe_revision` を**読むだけ**の消費者で、収集エンジン側には一切
//! 書き込まない（`crate::stream` と同じ立ち位置）。1インスタンス = 2タスク:
//!
//! - **poll タスク**（[`run_eventloop`]）: `rumqttc::EventLoop::poll` を
//!   回し続け、接続確立/断を検知して `connected` watch へ反映する。
//!   **再接続は rumqttc の自動再接続に任せる**（実装指示どおり）—
//!   `poll()` がエラーを返した後、次の `poll()` 呼び出しで rumqttc が内部の
//!   `MqttOptions` を使って自動的に再接続を試みる。ここでの
//!   `sleep`（[`RECONNECT_POLL_BACKOFF_MS`]）はその再接続ロジック自体では
//!   なく、ブローカー不通時に `poll()` をタイトループさせないための
//!   ウェイトに過ぎない。
//! - **eval タスク**（[`run_eval_loop`]）: `crate::stream` と同じ **250ms
//!   固定**の評価タイマ（[`EVAL_TICK_MS`]）で on_change の diff を検出し、
//!   `AsyncClient::publish`（`retain=true`）で発行する。`AsyncClient` は
//!   `Clone` なので poll タスクとは独立に呼べる - `publish().await` が
//!   ブローカー不調で詰まっても、詰まるのはこの eval タスクだけで
//!   `CollectorManager`/収集自体には一切影響しない。
//!
//! 2つのタスクは容量1の `resync` チャネル（[`RESYNC_CHANNEL_CAPACITY`]）で
//! 結ばれる: poll タスクが `ConnAck` を受け取ったら（初回接続・再接続の
//! どちらも）`try_send` で eval タスクへ「全タグ一斉発行して」と伝える
//! （設計 §5.3「接続時は全タグの現在値を一斉発行(retain の初期化)」）。同じ
//! 一斉発行は `revision` watch の変化（config_changed 相当）でも起きる
//! （設計「revision 変化時は新 catalog で同様に一斉発行」）。一斉発行は
//! **スロットル・diff を無視**して毎回全タグを送る（retain 初期化という
//! 別種のイベントであり、「値が変わった」イベントではないため）。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use rumqttc::{AsyncClient, EventLoop, LastWill, MqttOptions, QoS};
use serde::Serialize;
use tokio::sync::{mpsc, watch, Mutex as AsyncMutex};
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;

use banto_collect::Quality;

use crate::hub::{quality_str, read_current, CollectorManager, TagEntry};
use crate::settings::MqttSettings;

/// 評価タイマの固定周期 - `crate::stream::EVAL_TICK_MS` と同じ値・同じ理由
/// （このモジュールの doc comment「タスク構成」参照）。
const EVAL_TICK_MS: u64 = 250;

/// `rumqttc::AsyncClient::new` の送信チャネル容量。`crate::stream` の
/// `OUTBOUND_QUEUE_CAPACITY` と同じ桁を踏襲 - タグ数が数百程度の想定
/// （設計の実績値）なら一斉発行1回分がまとめて詰まれても十分収まる。
const OUTGOING_CHANNEL_CAPACITY: usize = 256;

/// poll タスクが `EventLoop::poll` のエラー後に空ける待ち時間 -
/// 「再接続そのもの」ではなく、ブローカー不通時のタイトループ防止
/// （このモジュールの doc comment参照）。
const RECONNECT_POLL_BACKOFF_MS: u64 = 1000;

/// keep-alive 秒数。設定項目としては実装指示に無いため固定値とした
/// （実装判断: 頻繁な発行がある運用を想定し、一般的な既定値を採用）。
const KEEP_ALIVE_SECS: u64 = 30;

/// poll タスク → eval タスクの「今すぐ全タグ一斉発行して」信号チャネルの
/// 容量。1で十分 - 複数回シグナルが積もっても「一斉発行」自体は冪等
/// （retain の再発行）なので `try_send` の `Full` は無視してよい。
const RESYNC_CHANNEL_CAPACITY: usize = 1;

const STATE_TOPIC_SUFFIX: &str = "$state";
const ONLINE_PAYLOAD: &str = "online";
const OFFLINE_PAYLOAD: &str = "offline";

/// `{"v": ..., "q": ..., "t": ...}`（設計 §5.3「ペイロード...WebSocket と
/// 同形」）- `crate::stream::ValueWire`/`crate::rest::ValueEntry` と同じ
/// 3フィールドをこのモジュール専用に持つ（型を共有すると `serde` の
/// 派生機能やフィールド名の意味が3モジュールで結合してしまうため、ワイヤ
/// 形式が同じでも型は independently に定義する - 各モジュールの既存の
/// 流儀を踏襲）。
#[derive(Debug, Serialize)]
struct ValuePayload {
    v: Option<f64>,
    q: &'static str,
    t: i64,
}

fn qos_from_setting(qos: u8) -> QoS {
    // 設計 §5.3「QoS: 既定1。設定で0/1切り替え(2は使わない)」- 0/1以外の
    // 値が settings に紛れ込んだ場合も(REST 層で validate 済みのはずだが)
    // panic させず 0 のみ AtMostOnce・それ以外(1 は元より、想定外の2以上も)
    // は AtLeastOnce 側へ丸める(このモジュールの外の異常値でここが死ぬのは
    // 避ける - どちらに丸めても実害がないので、単純な if で足りる)。
    if qos >= 1 {
        QoS::AtLeastOnce
    } else {
        QoS::AtMostOnce
    }
}

/// 設計 §5.3「トピック: {prefix}/{connection}/{group}/{tag}(外部名の . を /
/// に)」- `TagEntry` は既に `connection`/`group`/`name` を分離済みフィールド
/// として持つ（`hub.rs::build_catalog` 参照）ので、外部名の文字列を組み立てて
/// から `.` を `/` に置換するより、分離済みフィールドを直接 `/` で連結する
/// 方が単純かつ等価（タグ名に `.` が混じっていても壊れない - 判断の記録）。
fn topic_for(prefix: &str, entry: &TagEntry) -> String {
    format!(
        "{prefix}/{}/{}/{}",
        entry.connection, entry.group, entry.name
    )
}

fn state_topic(prefix: &str) -> String {
    format!("{prefix}/{STATE_TOPIC_SUFFIX}")
}

/// eval タスクが on_change の diff とスロットルを判定するための、タグ1本
/// 分の直近発行状態（`crate::stream::Subscription::last`と同種の目的）。
#[derive(Debug, Clone)]
struct PublishedState {
    value: Option<f64>,
    quality: Quality,
    published_at_ms: i64,
}

/// 実行中の1インスタンス分のタスクハンドル。[`MqttPublisher::stop`]（また
/// は再適用時の `apply` 内部）で両方 `abort` する - `Collector`の `stop`の
/// ような grace 待ちはしない(§5.3 は明示的な切断手順を要求しておらず、
/// abort による通知未達切断は LWT の `offline` がそのまま意味を持つ)。
struct RunningPublisher {
    poll_task: JoinHandle<()>,
    eval_task: JoinHandle<()>,
}

/// [`CollectorManager`]と同型の「restart 可能なマネージャ」（設計実装指示:
/// 「`MqttPublisher` は restart 可能なマネージャ構造...`CollectorManager.
/// rebuild` と同じ『古いタスクを止めて新しいタスク』パターン」）。
///
/// `bin/banto-hub.rs`が起動時に1個構築し、`crate::rest`の
/// `PUT /api/mqtt-settings` ハンドラが設定保存の直後に
/// [`MqttPublisher::apply`]を呼んで即時適用する（`enabled=false`なら
/// 何も起動しない停止状態のまま）。
pub struct MqttPublisher {
    manager: Arc<CollectorManager>,
    connected_tx: watch::Sender<bool>,
    running: AsyncMutex<Option<RunningPublisher>>,
}

impl MqttPublisher {
    /// 停止状態で構築する - 呼び出し側が続けて
    /// [`MqttPublisher::apply`]`(&settings)`を呼ぶまで何もしない
    /// （`bin/banto-hub.rs`は起動直後に settings から読んだ設定を渡す）。
    pub fn new(manager: Arc<CollectorManager>) -> Self {
        let (connected_tx, _rx) = watch::channel(false);
        Self {
            manager,
            connected_tx,
            running: AsyncMutex::new(None),
        }
    }

    /// `/api/v1/status`の`mqtt.connected`（`crate::rest::v1_status`参照）。
    /// `watch::Sender::borrow`は受信側を必要としない（`watch::channel`が
    /// 返す初期 `Receiver`は誰も保持しないため - `Self::new`参照。
    /// `subscribe()`ではなく`borrow()`を使うのは、`send`ではなく
    /// `send_replace`を使うのと同じ理由: 送信側自身が「今の値」を読むのに
    /// 受信側の生死を条件にする必要がない）。
    pub fn connected(&self) -> bool {
        *self.connected_tx.borrow()
    }

    /// 現在の設定を即時反映する: 実行中インスタンスがあれば止め、
    /// `settings.enabled`なら新しい設定で起動し直す
    /// （`CollectorManager::rebuild`と同じ「古いタスクを止めて新しいタスク」
    /// パターン、実装指示どおり）。`bin/banto-hub.rs`の起動シーケンスと
    /// `PUT /api/mqtt-settings`の両方から呼ばれる。
    pub async fn apply(&self, settings: &MqttSettings) {
        let mut guard = self.running.lock().await;
        Self::stop_locked(&mut guard, &self.connected_tx);
        if settings.enabled {
            *guard = Some(self.spawn(settings.clone()));
        }
    }

    /// プロセス終了時の停止（`bin/banto-hub.rs`のシャットダウン配線）。
    pub async fn shutdown(&self) {
        let mut guard = self.running.lock().await;
        Self::stop_locked(&mut guard, &self.connected_tx);
    }

    fn stop_locked(guard: &mut Option<RunningPublisher>, connected_tx: &watch::Sender<bool>) {
        if let Some(running) = guard.take() {
            running.poll_task.abort();
            running.eval_task.abort();
        }
        // 起動していなかった場合も含め、停止後は必ず false - `apply`が
        // enabled=false で呼ばれた場合の「即座に disabled 表示になる」を
        // 保証する。`send`ではなく`send_replace`を使う理由: `Self::new`が
        // 作る `watch::channel`の初期 `Receiver`は誰も保持しない（購読者は
        // `Self::connected`が呼ばれる度に使い捨てで作る設計）ため、
        // `send`は「受信側が1人もいない」場合 `Err`を返し**値を更新しない**
        // （`tokio::sync::watch::Sender::send`の仕様 - 実機で確認済みの
        // ハマりどころ）。`send_replace`は受信側の有無に関わらず必ず値を
        // 更新する。
        connected_tx.send_replace(false);
    }

    fn spawn(&self, settings: MqttSettings) -> RunningPublisher {
        let mut options = MqttOptions::new(
            settings.client_id.clone(),
            settings.host.clone(),
            settings.port,
        );
        options.set_keep_alive(Duration::from_secs(KEEP_ALIVE_SECS));
        if let Some(username) = settings.username.clone() {
            options.set_credentials(username, settings.password.clone().unwrap_or_default());
        }

        let qos = qos_from_setting(settings.qos);
        let will_topic = state_topic(&settings.prefix);
        // 設計 §5.3「LWT: {prefix}/$state に retain で online(接続時発行)/
        // offline(Last Will)」。
        options.set_last_will(LastWill::new(will_topic, OFFLINE_PAYLOAD, qos, true));

        let (client, eventloop) = AsyncClient::new(options, OUTGOING_CHANNEL_CAPACITY);
        let (resync_tx, resync_rx) = mpsc::channel::<()>(RESYNC_CHANNEL_CAPACITY);

        let poll_task = tokio::spawn(run_eventloop(
            eventloop,
            client.clone(),
            self.connected_tx.clone(),
            state_topic(&settings.prefix),
            qos,
            resync_tx,
        ));

        let eval_task = tokio::spawn(run_eval_loop(
            self.manager.clone(),
            client,
            settings.prefix,
            qos,
            settings.min_interval_ms,
            resync_rx,
        ));

        RunningPublisher {
            poll_task,
            eval_task,
        }
    }
}

/// poll タスク本体 - このモジュールの doc comment「タスク構成」参照。
async fn run_eventloop(
    mut eventloop: EventLoop,
    client: AsyncClient,
    connected_tx: watch::Sender<bool>,
    state_topic: String,
    qos: QoS,
    resync_tx: mpsc::Sender<()>,
) {
    loop {
        match eventloop.poll().await {
            Ok(rumqttc::Event::Incoming(rumqttc::Packet::ConnAck(_))) => {
                // `send`ではなく`send_replace` - `MqttPublisher::stop_locked`
                // の doc comment参照(受信側が1人もいない状態でも確実に値を
                // 反映させる必要がある)。
                connected_tx.send_replace(true);
                // 設計 §5.3「online(接続時発行)」。publish 自体の失敗は次回
                // 再接続時にもう一度試みられるので、ここでは best-effort。
                if let Err(err) = client
                    .publish(&state_topic, qos, true, ONLINE_PAYLOAD)
                    .await
                {
                    eprintln!("banto-hub: MQTT online 状態の発行に失敗しました: {err}");
                }
                // 設計 §5.3「接続時は全タグの現在値を一斉発行」- Full
                // (既に1件溜まっている)は無視してよい(冪等)。
                let _ = resync_tx.try_send(());
            }
            Ok(_) => {}
            Err(err) => {
                connected_tx.send_replace(false);
                eprintln!("banto-hub: MQTT イベントループでエラーが発生しました(再接続は rumqttc が自動で試行します): {err}");
                // このモジュールの doc comment「タスク構成」参照 - 再接続
                // ロジックそのものではなく、次の poll() 呼び出しまでの
                // タイトループ防止。
                tokio::time::sleep(Duration::from_millis(RECONNECT_POLL_BACKOFF_MS)).await;
            }
        }
    }
}

/// eval タスク本体 - このモジュールの doc comment「タスク構成」参照。
async fn run_eval_loop(
    manager: Arc<CollectorManager>,
    client: AsyncClient,
    prefix: String,
    qos: QoS,
    min_interval_ms: i64,
    mut resync_rx: mpsc::Receiver<()>,
) {
    let mut revision_rx = manager.subscribe_revision();
    let mut tick = tokio::time::interval(Duration::from_millis(EVAL_TICK_MS));
    // `crate::stream`と同じ理由(評価ループは「だいたい250msおき」でよく、
    // 詰まった分を積み上げて連射する必要はない)。
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);

    let mut last: HashMap<String, PublishedState> = HashMap::new();

    loop {
        tokio::select! {
            _ = tick.tick() => {
                publish_changed(&manager, &client, &prefix, qos, min_interval_ms, &mut last).await;
            }
            changed = revision_rx.changed() => {
                if changed.is_err() {
                    // CollectorManager が破棄された(プロセス終了間際) -
                    // このタスクも畳む。
                    break;
                }
                // 設計 §5.3「revision 変化(config_changed)時は新 catalog
                // で同様に一斉発行」。
                publish_all(&manager, &client, &prefix, qos, &mut last).await;
            }
            signal = resync_rx.recv() => {
                if signal.is_none() {
                    // poll タスクが終了した(abort による停止シーケンス) -
                    // このタスクも畳む。
                    break;
                }
                publish_all(&manager, &client, &prefix, qos, &mut last).await;
            }
        }
    }
}

/// 250ms tick 本体: on_change の diff を検出し、スロットル
/// （`min_interval_ms`）を満たしたタグだけ発行する。
///
/// スロットル抑止中のタグは`last`を更新しない（発行できていないので直近の
/// 発行状態は変わっていない）- 次の tick でも現在値と`last`を比較し続け、
/// スロットル窓が明けた最初の tick でその時点の最新値を送る（設計
/// §5.3「抑止された最新値はスロットル明け最初の tick で発行」）。
async fn publish_changed(
    manager: &CollectorManager,
    client: &AsyncClient,
    prefix: &str,
    qos: QoS,
    min_interval_ms: i64,
    last: &mut HashMap<String, PublishedState>,
) {
    let map = manager.tag_map();
    let now_ms = manager.clock().now_ms();
    let current = manager.current_values();
    let server_store = manager.server_store();

    for entry in map.iter() {
        let (value, quality, ts_ms) = read_current(entry, current.as_ref(), &server_store, now_ms);

        let due = match last.get(entry.external_name.as_str()) {
            // 初出のタグ(このインスタンスでまだ一度も発行していない) -
            // スロットル対象がないので即座に発行してよい(`crate::stream`の
            // on_change「新規にマッチしたタグは初回として必ず1回送る」と
            // 同じ判断)。
            None => true,
            Some(state) => {
                let changed = state.value != value || state.quality != quality;
                changed && now_ms.saturating_sub(state.published_at_ms) >= min_interval_ms
            }
        };
        if !due {
            continue;
        }

        publish_one(client, prefix, qos, entry, value, quality, ts_ms).await;
        last.insert(
            entry.external_name.clone(),
            PublishedState {
                value,
                quality,
                published_at_ms: now_ms,
            },
        );
    }
}

/// 一斉発行: diff・スロットルを無視して catalog の全タグを発行する（設計
/// §5.3「接続時は全タグの現在値を一斉発行(retain の初期化)」「revision
/// 変化...時は新 catalog で同様に一斉発行」の共通実装）。無効化されている
/// タグ（`TagEntry::enabled == false`）も含めて全件発行する -
/// `effective_sample`が`(None, Quality::Bad, ...)`を返すので、REST/WS と
/// 同じ「欠測を隠さない」規律のまま`q: "bad"`が飛ぶ（`hub.rs`の
/// `effective_sample`doc comment参照）。
async fn publish_all(
    manager: &CollectorManager,
    client: &AsyncClient,
    prefix: &str,
    qos: QoS,
    last: &mut HashMap<String, PublishedState>,
) {
    let map = manager.tag_map();
    let now_ms = manager.clock().now_ms();
    let current = manager.current_values();
    let server_store = manager.server_store();

    for entry in map.iter() {
        let (value, quality, ts_ms) = read_current(entry, current.as_ref(), &server_store, now_ms);
        publish_one(client, prefix, qos, entry, value, quality, ts_ms).await;
        last.insert(
            entry.external_name.clone(),
            PublishedState {
                value,
                quality,
                published_at_ms: now_ms,
            },
        );
    }
}

async fn publish_one(
    client: &AsyncClient,
    prefix: &str,
    qos: QoS,
    entry: &TagEntry,
    value: Option<f64>,
    quality: Quality,
    ts_ms: i64,
) {
    let topic = topic_for(prefix, entry);
    let payload = serde_json::to_vec(&ValuePayload {
        v: value,
        q: quality_str(quality),
        t: ts_ms,
    })
    .expect("ValuePayload は常にシリアライズ可能");

    // `publish().await`はブローカー側の送信バッファ(`AsyncClient::new`の
    // cap引数)が詰まっていると待つ - このモジュールの doc comment
    // 「タスク構成」参照の通り、詰まるのはこの eval タスクだけで
    // `CollectorManager`/収集には一切波及しない(設計 §3.4)。エラーは
    // ログのみ(次の tick か resync で再試行される)。
    if let Err(err) = client.publish(topic, qos, true, payload).await {
        eprintln!("banto-hub: MQTT タグ値の発行に失敗しました: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topic_for_joins_connection_group_name_with_prefix() {
        let entry = TagEntry {
            external_name: "line1.fast.temp01".to_string(),
            tag_key: "tag:1".to_string(),
            ids: (1, 2, 3),
            connection: "line1".to_string(),
            group: "fast".to_string(),
            name: "temp01".to_string(),
            address: "40001".to_string(),
            data_type: "i16".to_string(),
            unit: None,
            decimals: 0,
            period_ms: 100,
            enabled: true,
            writable: false,
            tag_kind: "plc".to_string(),
            expression: None,
            retain: false,
            simulation: false,
        };
        assert_eq!(topic_for("banto", &entry), "banto/line1/fast/temp01");
    }

    #[test]
    fn state_topic_appends_dollar_state() {
        assert_eq!(state_topic("banto"), "banto/$state");
        assert_eq!(state_topic("factory1"), "factory1/$state");
    }

    #[test]
    fn qos_from_setting_maps_0_and_1_only() {
        assert_eq!(qos_from_setting(0), QoS::AtMostOnce);
        assert_eq!(qos_from_setting(1), QoS::AtLeastOnce);
        // 2(ExactlyOnce)は設計上使わない設定 - REST 層で拒否されるはずだが、
        // ここでは(0以外は全て AtLeastOnce へ丸める実装なので)panic せず
        // AtLeastOnce になることだけ確認する。
        assert_eq!(qos_from_setting(2), QoS::AtLeastOnce);
    }

    #[tokio::test]
    async fn a_new_publisher_starts_disconnected_and_apply_with_disabled_settings_stays_disconnected(
    ) {
        let dir = tempfile::tempdir().expect("tempdir");
        let pool = crate::db::migrate_memory().await.expect("migrate_memory");
        let sessions = Arc::new(crate::broker_glue::HubSessions::new(
            banto_broker::BackoffConfig::default(),
        ));
        let sim_registry = Arc::new(crate::broker_glue::SlmpSimRegistry::new());
        let computed = Arc::new(crate::computed::ComputedEngine::new(Arc::new(
            crate::computed::ServerTagStore::new(),
        )));
        let manager = Arc::new(CollectorManager::new(
            pool,
            dir.path().join("data"),
            Arc::new(banto_tstore::SystemClock),
            banto_collect::CollectorOptions::default(),
            sessions,
            sim_registry,
            computed,
        ));
        let publisher = MqttPublisher::new(manager);
        assert!(!publisher.connected());

        publisher.apply(&MqttSettings::default()).await;
        assert!(!publisher.connected());

        publisher.shutdown().await;
        assert!(!publisher.connected());
    }
}
