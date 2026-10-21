//! 1 つの source（= Hub の `push` 接続 1 つ）の実行: 取り込み口の WebSocket
//! `GET /api/v1/ingest/{connectionId}/stream` を 1 本持ち、Hub の `state` に合わせて
//! 問い合わせのタスク（[`super::poll`]）を起こし・止め、周期の値を分けて
//! （[`super::split`]）速さを抑えて（[`super::budget`]）送る（docs/banto-hub-ingest-design.md
//! §10.3 の「送信」「収集の停止」「設定の変更」「タスクの異常」「状態」「停止」）。
//!
//! ## 1 本の WS の一生
//!
//! 1. `Authorization: Bearer <api_key>` で upgrade（Hub はスコープ `ingest:#{id}` を upgrade の
//!    前に確かめる）。断られたら HTTP の状態で分ける: 403 は「権限なし」、404・409 は
//!    設定を取り直させる（接続が消えた・無効）、401 はキーの失効。
//! 2. `welcome` を待つ（10 秒）。`epoch`・`collecting`・`stale_timeout_ms`・`limits` を受ける。
//!    速さのバケット（[`super::budget::SendBudget`]）は source ごとに 1 つで WS をまたいで
//!    持ち続け、`limits` が変わったときだけ作り直す。
//! 3. `collecting: true` なら問い合わせのタスクを起こす（describe → すぐ最初の問い合わせ →
//!    その周期の全キー）。`false` なら起こさない（DB へ繋がない。external-db §4.8）。
//! 4. 周期の値は件数・バイト数で分けて送信待ち（[`super::outbox::Outbox`]）へ入れ、バケットが
//!    許す速さで `values`（今の `epoch`・連番の `seq`）を送る。送りきる前に同じクエリの次の
//!    周期が来たら、送っていない古い分を捨てる。
//! 5. `values` を送っていない間だけ `stale_timeout_ms / 3` ごとに `ping`（§4.3。Hub の Live の
//!    判定）。DB 側の問題と捨てた周期は `status` で知らせる（1 秒に 1 回まで、変わったときだけ）。
//!
//! ## `state`（§4.5・§4.6）
//!
//! | 受けたもの                                   | すること                                                                 |
//! | -------------------------------------------- | ------------------------------------------------------------------------ |
//! | `collecting: false`                          | 問い合わせを止めてプールを閉じ、送信待ちを捨てる                           |
//! | `collecting: true` で epoch が変わった       | 問い合わせを作り直し（describe から）、新しい epoch で最初の周期から送る     |
//! | `config_changed: true`                       | 問い合わせを止め、監督（[`super`]）に設定を取り直させる。設定が同じなら      |
//! |                                              | 取り直しの後に新しい epoch で再開、違えば監督がこの source ごと作り直す     |
//!
//! epoch が変わったら送信待ちは捨てる（古い epoch の値は Hub が `stale_epoch` で断る）。
//! `stale_epoch` で断られた分は送り直さない（次の `state` を待つ）。
//!
//! **設定の取り直しの待ちは WS をまたいで持つ**（[`Runtime`] の `awaiting_config`）。
//! `config_changed` を受けた後、取得が遅れている・失敗している間に WS が切れても、張り直した
//! `welcome` が `collecting: true` だからといって手元の古い設定で問い合わせを始めない
//! （古い設定の値が新しい epoch で Hub に受け付けられてしまう）。再開は、取り直しを頼んだ
//! 時点より**後に始まった**取得が成功したとき（§10.3 の実装の注 (3)）。その取得が張り直しの
//! 前に終わっていれば、`welcome` の時点でそれを見て再開する。
//!
//! ## WS の切断
//!
//! 切れたら問い合わせを止めて（DB に繋いだまま値を捨て続けない）、1 秒 → 30 秒の
//! バックオフで張り直す。張り直したら describe からやり直す（§10.3「起動時と再接続後に
//! describe」）。Hub の Live は最後の通信から `stale_timeout_ms` まで続く（§4.3）ので、
//! 短い切断で Bad は挟まらない。
//!
//! ## タスクの異常
//!
//! 問い合わせのタスクが止めていないのに終わった（panic）ら、そのクエリ群の全キーへ
//! `bad` を送ってから、バックオフして作り直す（§10.3。`ping` だけが続いて古い Good が
//! 残らないように）。全キーの `bad` は送信待ちの**失敗の列**（[`Outbox::push_failure`]）に
//! 置く: 作り直したタスクの周期で置き換わらず、周期より先に、同じバケットの速さで送る。
//! 作り直しは送りきるのを待たないが、どのキーにも新しい値より先に `bad` が届く
//! （送りきる前に WS が切れたら、張り直しで describe から作り直したタスクが最初の周期で
//! 全キーを送り直す）。
//!
//! ## 止め方
//!
//! 止める合図で close 1000 を送り（相手の close を 1 秒まで待つ）、問い合わせのタスクを
//! 合図で止めて終わりを待つ（`abort` しない。[`super::poll`] の「止め方」）。止める側が
//! 待てる時間は呼び出し側（[`super::SourceHandle::stop`]）が決める。
//!
//! ## 資格情報
//!
//! API キーは upgrade の要求のヘッダ（`set_sensitive`）にだけ載せる。ログ・`status`・
//! [`SourceStatus`] に出すのは、Hub が返したコード・DB の文言（[`crate::status::Redactor`] で
//! パスワードとキーを伏せたもの）だけ。tungstenite のエラーの `Display` は URL や相手の本文を
//! 含みうるので出さない（分類だけ）。

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch, Notify};
use tokio::task::{JoinError, JoinHandle};
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{header::AUTHORIZATION, HeaderValue};
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, Message, WebSocketConfig};
use tokio_tungstenite::tungstenite::Error as WsError;
use tokio_tungstenite::{connect_async_with_config, MaybeTlsStream, WebSocketStream};

use crate::flush::backoff_delay;
use crate::hub_api::BridgeSourceConfig;
use crate::log::{log_err_line, log_line};
use crate::source::budget::SendBudget;
use crate::source::outbox::Outbox;
use crate::source::poll::{run_poller, PollEvent, PollerSpec, QueryPlan};
use crate::source::split::split_cycle;
use crate::source::wire::{
    parse_server_message, ping_message, status_message, values_message, ServerMessage, ValueItem,
    WireLimits,
};
use crate::status::Redactor;

/// upgrade（TCP の接続と HTTP の握手）の上限。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// upgrade の後に `welcome` を待つ上限。
const WELCOME_TIMEOUT: Duration = Duration::from_secs(10);

/// 1 メッセージの送信の上限（読まない相手に捕まらない）。
const SEND_TIMEOUT: Duration = Duration::from_secs(10);

/// 止めるときに相手の close を待つ上限。
const CLOSE_TIMEOUT: Duration = Duration::from_secs(1);

/// `status` を送る最短の間隔（変わったときだけ、ただし 1 秒に 1 回まで）。
const STATUS_MIN_INTERVAL: Duration = Duration::from_secs(1);

/// `status` の `detail` のバイト数の上限（`banto_tags::PUSH_LIMITS.max_status_detail_bytes`
/// と同じ。Hub も切り詰めるが、送る前にも揃える）。
pub const MAX_STATUS_DETAIL_BYTES: usize = 1024;

/// Hub から受けるメッセージの上限（Hub が送るのは小さな応答だけ）。
const MAX_INCOMING_MESSAGE_BYTES: usize = 1024 * 1024;

/// 1 回の起床で続けて送ってよいメッセージの数（他の仕事 - 受信・`state` - を待たせない）。
const MAX_SENDS_PER_WAKE: usize = 64;

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// source の状態（ログ・テスト用。資格情報も値も持たない）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SourceState {
    /// 起動直後・WS を張っている。
    #[default]
    Connecting,
    /// WS は開いていて、収集が止まっている（問い合わせない）。
    Idle,
    /// WS が開いていて、問い合わせて送っている。
    Running,
    /// Hub が設定の変更を知らせた。取り直しを待っている（問い合わせない）。
    AwaitingConfig,
    /// WS が切れた・張れない。バックオフしている。
    Backoff,
    /// Hub がこのキーでは送れないと断った（403）か、設定の `ingestAllowed` が `false`。
    NoPermission,
    /// 参照先の DB 接続が Hub で無効になっている。
    DbDisabled,
    /// 止めた。
    Stopped,
}

impl SourceState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Connecting => "connecting",
            Self::Idle => "idle",
            Self::Running => "running",
            Self::AwaitingConfig => "awaiting_config",
            Self::Backoff => "backoff",
            Self::NoPermission => "no_permission",
            Self::DbDisabled => "db_disabled",
            Self::Stopped => "stopped",
        }
    }
}

/// 1 つの source の状態と数（[`super::BridgeSupervisor::statuses`]）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SourceStatus {
    pub connection_id: i64,
    pub name: String,
    pub state: SourceState,
    pub epoch: Option<u64>,
    pub collecting: Option<bool>,
    /// 送った `values` のメッセージの数・件数。
    pub sent_messages: u64,
    pub sent_items: u64,
    /// `ack` で受け付けられた件数・断られた件数。
    pub accepted_items: u64,
    pub rejected_items: u64,
    /// `error` で断られたメッセージの数（コード別）。
    pub rate_limited: u64,
    pub too_many_items: u64,
    pub stale_epoch: u64,
    pub invalid_message: u64,
    /// 送りきる前に次の周期が来て捨てた周期の数。
    pub superseded_cycles: u64,
    /// `welcome` まで行った WS の数（張り直しを含む）。
    pub sessions: u64,
    /// 問い合わせのタスクを起こした回数。
    pub pollers_started: u64,
    /// 最後に送った（送ろうとした）`status` の文言。
    pub last_status: Option<String>,
    /// 最後に WS が閉じた理由（close のコードと理由文、または分類）。
    pub last_close: Option<String>,
}

/// 監督（[`super`]）と source が共有するもの。
#[derive(Clone)]
pub struct SourceContext {
    /// `ws://{hub}/api/v1/ingest/{connectionId}/stream`。
    pub ws_url: String,
    pub api_key: String,
    pub redactor: Redactor,
    /// 設定を取り直させる（`config_changed`・404・409・`connection_changed`）。
    pub refresh: Arc<Notify>,
    /// 監督が始めた設定の取得の番号（取得を始めるたびに 1 つ進める）。
    pub fetch_started: Arc<AtomicU64>,
    /// 監督が終えた（成功した）設定の取得の番号（`config_changed` の後の再開の合図）。
    /// source は「取り直しを頼んだ時点の [`Self::fetch_started`] より後に始まった取得」が
    /// 終わるまで待つ - 頼む前から走っていた取得（古い設定を返しうる）で再開しない。
    pub config_generation: watch::Receiver<u64>,
}

/// 動いている source 1 つ。
pub struct SourceHandle {
    pub config: BridgeSourceConfig,
    stop_tx: watch::Sender<bool>,
    task: JoinHandle<()>,
    status: Arc<Mutex<SourceStatus>>,
}

impl SourceHandle {
    pub fn spawn(config: BridgeSourceConfig, ctx: SourceContext) -> Self {
        let (stop_tx, stop_rx) = watch::channel(false);
        let status = Arc::new(Mutex::new(SourceStatus {
            connection_id: config.connection_id,
            name: config.name.clone(),
            ..SourceStatus::default()
        }));
        let task = tokio::spawn(run_source(config.clone(), ctx, stop_rx, status.clone()));
        Self {
            config,
            stop_tx,
            task,
            status,
        }
    }

    pub fn status(&self) -> SourceStatus {
        lock(&self.status).clone()
    }

    pub fn shared_status(&self) -> Arc<Mutex<SourceStatus>> {
        self.status.clone()
    }

    /// 止める合図だけを送る（待たない）。
    pub fn request_stop(&self) {
        let _ = self.stop_tx.send(true);
    }

    pub fn is_finished(&self) -> bool {
        self.task.is_finished()
    }

    /// 止めて、`limit` まで終わりを待つ。終わらなければ handle は `self` ごと呼び出し側に
    /// 残す（`Err(self)`）- 手放すと終わりを確かめられない。
    pub async fn stop(mut self, limit: Duration) -> Result<(), Box<Self>> {
        self.request_stop();
        match tokio::time::timeout(limit, &mut self.task).await {
            Ok(_) => Ok(()),
            Err(_) => Err(Box::new(self)),
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// `http://host:port` → `ws://host:port/api/v1/ingest/{id}/stream`（`https` は `wss`）。
pub fn ingest_ws_url(hub_url: &str, connection_id: i64) -> String {
    let base = hub_url.trim_end_matches('/');
    let base = if let Some(rest) = base.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = base.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        base.to_string()
    };
    format!("{base}/api/v1/ingest/{connection_id}/stream")
}

/// upgrade の失敗の分類（ログに URL・本文を出さない）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnectError {
    Http(u16),
    Transport,
    Timeout,
    InvalidRequest,
}

impl ConnectError {
    fn describe(self) -> String {
        match self {
            Self::Http(401) => "API キーが無効です（401。失効・期限切れ）".to_string(),
            Self::Http(403) => {
                "この API キーでは送れません（403。ingest:#{id} のスコープが必要です）".to_string()
            }
            Self::Http(404) => "Hub にこの push 接続がありません（404）".to_string(),
            Self::Http(409) => "Hub でこの push 接続が無効です（409）".to_string(),
            Self::Http(code) => format!("Hub が upgrade を断りました（HTTP {code}）"),
            Self::Transport => "Hub へ接続できません".to_string(),
            Self::Timeout => "Hub への接続がタイムアウトしました".to_string(),
            Self::InvalidRequest => "送信口の URL を組み立てられません".to_string(),
        }
    }
}

async fn connect_ws(url: &str, api_key: &str) -> Result<Ws, ConnectError> {
    let mut request = url
        .into_client_request()
        .map_err(|_| ConnectError::InvalidRequest)?;
    let mut value = HeaderValue::from_str(&format!("Bearer {api_key}"))
        .map_err(|_| ConnectError::InvalidRequest)?;
    value.set_sensitive(true);
    request.headers_mut().insert(AUTHORIZATION, value);
    let config = WebSocketConfig::default()
        .max_message_size(Some(MAX_INCOMING_MESSAGE_BYTES))
        .max_frame_size(Some(MAX_INCOMING_MESSAGE_BYTES));
    match tokio::time::timeout(
        CONNECT_TIMEOUT,
        connect_async_with_config(request, Some(config), false),
    )
    .await
    {
        Err(_) => Err(ConnectError::Timeout),
        Ok(Ok((ws, _))) => Ok(ws),
        Ok(Err(WsError::Http(response))) => Err(ConnectError::Http(response.status().as_u16())),
        Ok(Err(_)) => Err(ConnectError::Transport),
    }
}

/// 1 本の WS の終わり方。
#[derive(Debug)]
enum SessionEnd {
    /// 止める合図。
    Stopped,
    /// Hub が閉じた・切れた（張り直す）。`welcomed` は `welcome` まで行ったか（バックオフを
    /// 最初からにする）。
    Ended { reason: String, welcomed: bool },
}

/// 問い合わせのタスク 1 つ。
struct Poller {
    stop_tx: watch::Sender<bool>,
    handle: JoinHandle<()>,
}

/// source のタスクの本体。
async fn run_source(
    config: BridgeSourceConfig,
    ctx: SourceContext,
    mut stop: watch::Receiver<bool>,
    status: Arc<Mutex<SourceStatus>>,
) {
    let mut runtime = Runtime {
        config,
        ctx,
        status,
        budget: None,
        seq: 0,
        retiring: Vec::new(),
        logged_connect_error: None,
        awaiting_config: None,
    };
    let mut attempt: u32 = 0;
    loop {
        if *stop.borrow() {
            break;
        }
        runtime.set_state(SourceState::Connecting);
        let connected = tokio::select! {
            biased;
            _ = stop.changed() => break,
            result = connect_ws(&runtime.ctx.ws_url, &runtime.ctx.api_key) => result,
        };
        match connected {
            Ok(ws) => {
                runtime.logged_connect_error = None;
                match runtime.session(ws, &mut stop).await {
                    SessionEnd::Stopped => break,
                    SessionEnd::Ended { reason, welcomed } => {
                        if welcomed {
                            attempt = 0;
                        }
                        lock(&runtime.status).last_close = Some(reason);
                    }
                }
            }
            Err(err) => runtime.on_connect_error(err),
        }
        attempt = attempt.saturating_add(1);
        let delay = backoff_delay(attempt);
        if !matches!(
            runtime.state(),
            SourceState::NoPermission | SourceState::DbDisabled
        ) {
            runtime.set_state(SourceState::Backoff);
        }
        tokio::select! {
            biased;
            _ = stop.changed() => break,
            _ = tokio::time::sleep(delay) => {}
        }
    }
    runtime.finish().await;
}

/// source の WS をまたいで持つもの。
struct Runtime {
    config: BridgeSourceConfig,
    ctx: SourceContext,
    status: Arc<Mutex<SourceStatus>>,
    budget: Option<SendBudget>,
    seq: u64,
    /// 止めたが終わりをまだ確かめていない問い合わせのタスク（手放さない）。
    retiring: Vec<JoinHandle<()>>,
    /// 直前にログに出した upgrade の失敗（同じものを繰り返し出さない）。
    logged_connect_error: Option<ConnectError>,
    /// 設定の取り直しを待っている（`config_changed` を受けた）。値は取り直しを頼んだ時点の
    /// [`SourceContext::fetch_started`]（これより後に始まった取得の成功を待つ）。WS をまたいで
    /// 持つ（このモジュールの doc「`state`」）。
    awaiting_config: Option<u64>,
}

impl Runtime {
    fn set_state(&self, state: SourceState) {
        lock(&self.status).state = state;
    }

    fn state(&self) -> SourceState {
        lock(&self.status).state
    }

    fn update(&self, f: impl FnOnce(&mut SourceStatus)) {
        f(&mut lock(&self.status));
    }

    fn next_seq(&mut self) -> u64 {
        self.seq = self.seq.wrapping_add(1);
        self.seq
    }

    fn label(&self) -> String {
        format!(
            "ブリッジ {}（#{}）",
            self.config.name, self.config.connection_id
        )
    }

    fn on_connect_error(&mut self, err: ConnectError) {
        match err {
            ConnectError::Http(403) => self.set_state(SourceState::NoPermission),
            // 接続が消えた・無効になった: 設定を取り直させる（監督が止めるか、同じなら続ける）。
            ConnectError::Http(404) | ConnectError::Http(409) => self.ctx.refresh.notify_one(),
            _ => {}
        }
        if self.logged_connect_error != Some(err) {
            log_err_line(&format!(
                "banto-hub-sink: warn: {}: 送信口に繋げません（再試行します）: {}",
                self.label(),
                err.describe()
            ));
            self.logged_connect_error = Some(err);
        }
        self.update(|s| s.last_close = Some(err.describe()));
    }

    /// 取り直しを待っていて、頼んだ後に始まった取得がもう成功していれば待ちを解く。
    /// 待ちが残っていれば `true`。
    fn still_awaiting_config(&mut self) -> bool {
        if let Some(after) = self.awaiting_config {
            if *self.ctx.config_generation.borrow() > after {
                self.awaiting_config = None;
            }
        }
        self.awaiting_config.is_some()
    }

    /// 問い合わせのタスクを止める（合図だけ。終わりは `retiring` で確かめる）。
    fn retire(&mut self, poller: &mut Option<Poller>) {
        if let Some(poller) = poller.take() {
            let _ = poller.stop_tx.send(true);
            self.retiring.push(poller.handle);
        }
        self.retiring.retain(|handle| !handle.is_finished());
    }

    fn start_poller(&mut self, epoch: u64) -> (Poller, mpsc::Receiver<PollEvent>) {
        let (tx, rx) = mpsc::channel(64);
        let (stop_tx, stop_rx) = watch::channel(false);
        let spec = PollerSpec::new(&self.config.name, &self.config.db, &self.config.queries);
        let handle = tokio::spawn(run_poller(spec, tx, stop_rx, self.ctx.redactor.clone()));
        self.update(|s| {
            s.pollers_started += 1;
            s.epoch = Some(epoch);
        });
        (Poller { stop_tx, handle }, rx)
    }

    /// 止めた後: 問い合わせのタスクの終わりを待つ（合図は送ってある。上限は呼び出し側が
    /// [`SourceHandle::stop`] で掛ける）。
    async fn finish(mut self) {
        for handle in self.retiring.drain(..) {
            let _ = handle.await;
        }
        self.set_state(SourceState::Stopped);
    }

    /// 1 本の WS（このモジュールの doc「1 本の WS の一生」）。
    async fn session(&mut self, ws: Ws, stop: &mut watch::Receiver<bool>) -> SessionEnd {
        let (mut ws_tx, mut ws_rx) = ws.split();

        // --- welcome ---
        let welcome = tokio::select! {
            biased;
            _ = stop.changed() => {
                close_ws(&mut ws_tx, &mut ws_rx).await;
                return SessionEnd::Stopped;
            }
            welcome = tokio::time::timeout(WELCOME_TIMEOUT, read_welcome(&mut ws_rx)) => welcome,
        };
        let (connection_id, mut collecting, mut epoch, stale_timeout_ms, limits) = match welcome {
            Ok(Some(ServerMessage::Welcome {
                connection_id,
                collecting,
                epoch,
                stale_timeout_ms,
                limits,
                ..
            })) => (connection_id, collecting, epoch, stale_timeout_ms, limits),
            Ok(_) => {
                return SessionEnd::Ended {
                    reason: "Hub が welcome を送らずに閉じました".to_string(),
                    welcomed: false,
                }
            }
            Err(_) => {
                return SessionEnd::Ended {
                    reason: "welcome が届きませんでした".to_string(),
                    welcomed: false,
                }
            }
        };
        if connection_id != self.config.connection_id {
            close_ws(&mut ws_tx, &mut ws_rx).await;
            return SessionEnd::Ended {
                reason: format!("Hub が別の接続（#{connection_id}）の welcome を返しました"),
                welcomed: false,
            };
        }
        let now = Instant::now();
        if self.budget.as_ref().map(SendBudget::limits) != Some(limits) {
            self.budget = Some(SendBudget::new(limits, now));
        }
        self.update(|s| {
            s.sessions += 1;
            s.epoch = Some(epoch);
            s.collecting = Some(collecting);
        });
        log_line(&format!(
            "banto-hub-sink: {}: 送信口に繋がりました（epoch {epoch}・収集 {}）",
            self.label(),
            if collecting { "中" } else { "停止中" }
        ));

        let queries: Vec<QueryPlan> = self
            .config
            .queries
            .iter()
            .map(QueryPlan::from_config)
            .collect();
        let ping_interval = Duration::from_millis((stale_timeout_ms / 3).max(100));
        let mut s = SessionState {
            outbox: Outbox::new(queries.len()),
            queries,
            limits,
            poller: None,
            events: None,
            poller_failures: 0,
            poller_restart_at: None,
            last_sent: now,
            db_status: None,
            reported_status: None,
            status_ready_at: now,
            logged_codes: HashSet::new(),
        };
        let mut config_generation = self.ctx.config_generation.clone();
        config_generation.borrow_and_update();
        // 前の WS で頼んだ取り直しがまだ終わっていなければ、`collecting: true` でも
        // 問い合わせない（古い設定の値を新しい epoch で送らない）。
        if self.still_awaiting_config() {
            log_line(&format!(
                "banto-hub-sink: {}: 設定の取り直しを待っています（終わるまで問い合わせません）",
                self.label()
            ));
        }
        self.resume(&mut s, collecting, epoch);

        let end = loop {
            let now = Instant::now();
            let status_pending = s.compose_status(&self.status) != s.reported_status;
            let wake_at = s.next_wake(
                now,
                ping_interval,
                self.budget.as_mut().expect("welcome で作った"),
                status_pending,
            );
            let event = tokio::select! {
                biased;
                _ = stop.changed() => Event::Stop,
                incoming = ws_rx.next() => Event::Incoming(incoming),
                ended = join_poller(&mut s.poller), if s.poller.is_some() => Event::PollerEnded(ended),
                polled = recv_events(&mut s.events), if s.events.is_some() => Event::Polled(polled),
                changed = config_generation.changed(), if self.awaiting_config.is_some() => {
                    // 監督が居なくなったら止まる（閉じたチャンネルで空回りしない）。
                    if changed.is_ok() { Event::ConfigConfirmed } else { Event::Stop }
                }
                _ = tokio::time::sleep_until(wake_at) => Event::Timer,
            };
            match event {
                Event::Stop => {
                    self.retire(&mut s.poller);
                    close_ws(&mut ws_tx, &mut ws_rx).await;
                    break SessionEnd::Stopped;
                }
                Event::Incoming(None) => {
                    break SessionEnd::Ended {
                        reason: "Hub との接続が切れました".to_string(),
                        welcomed: true,
                    }
                }
                Event::Incoming(Some(Err(_))) => {
                    break SessionEnd::Ended {
                        reason: "Hub との接続の読み取りに失敗しました".to_string(),
                        welcomed: true,
                    }
                }
                Event::Incoming(Some(Ok(message))) => match message {
                    Message::Text(text) => match parse_server_message(text.as_str()) {
                        Some(ServerMessage::State {
                            collecting: next_collecting,
                            epoch: next_epoch,
                            config_changed,
                        }) => {
                            let restart = next_epoch != epoch || next_collecting != collecting;
                            epoch = next_epoch;
                            collecting = next_collecting;
                            self.update(|st| {
                                st.epoch = Some(epoch);
                                st.collecting = Some(collecting);
                            });
                            if config_changed {
                                // §4.6: 設定を取り直してから送る（`config_refresh_secs` を待たない）。
                                self.awaiting_config =
                                    Some(self.ctx.fetch_started.load(Ordering::SeqCst));
                                config_generation.borrow_and_update();
                                self.ctx.refresh.notify_one();
                            }
                            if restart || config_changed {
                                self.retire(&mut s.poller);
                                s.events = None;
                                s.outbox.clear();
                                s.poller_restart_at = None;
                            }
                            if restart || config_changed || s.poller.is_none() {
                                self.resume(&mut s, collecting, epoch);
                            }
                        }
                        Some(ServerMessage::Ack {
                            accepted, rejected, ..
                        }) => {
                            self.update(|st| {
                                st.accepted_items += accepted;
                                st.rejected_items += rejected.len() as u64;
                            });
                            for item in rejected {
                                if s.logged_codes.insert(format!("ack:{}", item.code)) {
                                    log_err_line(&format!(
                                        "banto-hub-sink: warn: {}: Hub がキー {} を断りました（{}）。タグの登録とブリッジの設定を確認してください",
                                        self.label(),
                                        item.key.as_deref().unwrap_or("?"),
                                        item.code
                                    ));
                                }
                            }
                        }
                        Some(ServerMessage::Error { code, .. }) => {
                            self.update(|st| match code.as_str() {
                                "rate_limited" => st.rate_limited += 1,
                                "too_many_items" => st.too_many_items += 1,
                                "stale_epoch" => st.stale_epoch += 1,
                                _ => st.invalid_message += 1,
                            });
                            // `stale_epoch` は遷移の直後に起こりうる正常な行き違い（次の `state`
                            // を待つ）。ほかは 1 回だけ warn。
                            if code != "stale_epoch"
                                && s.logged_codes.insert(format!("error:{code}"))
                            {
                                log_err_line(&format!(
                                    "banto-hub-sink: warn: {}: Hub がメッセージを断りました（{code}）",
                                    self.label()
                                ));
                            }
                        }
                        // 知らない・読めないメッセージは読み飛ばす（前方互換）。
                        _ => {}
                    },
                    Message::Close(frame) => {
                        let reason = match &frame {
                            Some(frame) => format!(
                                "Hub が閉じました（{} {}）",
                                u16::from(frame.code),
                                frame.reason.as_str()
                            ),
                            None => "Hub が閉じました".to_string(),
                        };
                        let text = frame.as_ref().map(|f| f.reason.as_str().to_string());
                        match text.as_deref() {
                            Some("connection_changed") | Some("connection_removed") => {
                                self.ctx.refresh.notify_one();
                            }
                            Some("superseded") => log_err_line(&format!(
                                "banto-hub-sink: warn: {}: 同じ push 接続に別の送り手が繋いだため閉じられました（ブリッジを 2 つ動かしていないか確認してください）",
                                self.label()
                            )),
                            _ => {}
                        }
                        break SessionEnd::Ended {
                            reason,
                            welcomed: true,
                        };
                    }
                    // バイナリ・ping・pong は使わない（tungstenite が ping に応える）。
                    _ => {}
                },
                Event::PollerEnded(result) => {
                    // 止めていないのに終わった（§10.3「タスクの異常」）。
                    s.poller = None;
                    s.events = None;
                    s.poller_failures = s.poller_failures.saturating_add(1);
                    let delay = backoff_delay(s.poller_failures);
                    s.poller_restart_at = Some(Instant::now() + delay);
                    log_err_line(&format!(
                        "banto-hub-sink: warn: {}: 問い合わせのタスクが止まりました（{}）。全キーを bad にして {} 秒後に作り直します",
                        self.label(),
                        if result.as_ref().is_err_and(JoinError::is_panic) { "panic" } else { "終了" },
                        delay.as_secs_f64()
                    ));
                    s.queue_failure_notices();
                }
                Event::Polled(None) => {
                    // 送り手の口が閉じた = 問い合わせのタスクが終わる。終わりは PollerEnded で拾う。
                    s.events = None;
                }
                Event::Polled(Some(PollEvent::Cycle { query, items })) => {
                    let messages =
                        split_cycle(items, s.limits.max_items, s.limits.max_message_bytes);
                    if s.outbox.push_cycle(query, messages).is_some() {
                        let cycles = s.outbox.superseded_cycles();
                        self.update(|st| st.superseded_cycles = cycles);
                    }
                    // 周期が届いた = 問い合わせのタスクは動いている。
                    s.poller_failures = 0;
                }
                Event::Polled(Some(PollEvent::Status(text))) => {
                    s.db_status = text;
                }
                Event::ConfigConfirmed => {
                    // 頼んだ後に始まった取得が終わった: 設定は変わっていなかった（変わっていれば
                    // 監督がこの source を止めている - 止める合図は `biased` の先頭で拾う）。
                    config_generation.borrow_and_update();
                    if !self.still_awaiting_config() {
                        self.resume(&mut s, collecting, epoch);
                    }
                }
                Event::Timer => {
                    if let Err(reason) = self
                        .on_timer(&mut s, &mut ws_tx, epoch, collecting, ping_interval)
                        .await
                    {
                        break SessionEnd::Ended {
                            reason,
                            welcomed: true,
                        };
                    }
                }
            }
        };
        // WS が切れた・止めた: 問い合わせを止める（再接続の後に describe からやり直す）。
        self.retire(&mut s.poller);
        end
    }

    /// 収集中で設定の取り直しを待っていなければ、問い合わせのタスクを起こす。
    fn resume(&mut self, s: &mut SessionState, collecting: bool, epoch: u64) {
        if !collecting {
            self.set_state(SourceState::Idle);
            return;
        }
        if self.awaiting_config.is_some() {
            self.set_state(SourceState::AwaitingConfig);
            return;
        }
        if s.poller.is_none() && s.poller_restart_at.is_none() {
            let (poller, rx) = self.start_poller(epoch);
            s.poller = Some(poller);
            s.events = Some(rx);
        }
        self.set_state(SourceState::Running);
    }

    /// 送る・`status`・`ping`・問い合わせのタスクの作り直し。`Err` は送れなかった（切れた）。
    async fn on_timer(
        &mut self,
        s: &mut SessionState,
        ws_tx: &mut futures_util::stream::SplitSink<Ws, Message>,
        epoch: u64,
        collecting: bool,
        ping_interval: Duration,
    ) -> Result<(), String> {
        let now = Instant::now();
        // タスクの異常からの作り直し。
        if s.poller_restart_at.is_some_and(|at| now >= at) {
            s.poller_restart_at = None;
            if collecting && self.awaiting_config.is_none() && s.poller.is_none() {
                let (poller, rx) = self.start_poller(epoch);
                s.poller = Some(poller);
                s.events = Some(rx);
            }
        }
        // values（バケットが許すだけ）。
        let mut sent = 0;
        while sent < MAX_SENDS_PER_WAKE {
            let budget = self.budget.as_mut().expect("welcome で作った");
            let Some(items) = s.take_sendable(budget, Instant::now()) else {
                break;
            };
            let seq = self.next_seq();
            send_text(ws_tx, values_message(seq, epoch, &items)).await?;
            s.last_sent = Instant::now();
            self.update(|st| {
                st.sent_messages += 1;
                st.sent_items += items.len() as u64;
            });
            sent += 1;
        }
        // status（変わったときだけ、1 秒に 1 回まで）。
        let now = Instant::now();
        if now >= s.status_ready_at {
            if let Some(detail) = s.status_change(self.status.as_ref()) {
                let detail =
                    truncate_bytes(&self.ctx.redactor.apply(&detail), MAX_STATUS_DETAIL_BYTES);
                send_text(ws_tx, status_message(&detail)).await?;
                s.last_sent = now;
                s.status_ready_at = now + STATUS_MIN_INTERVAL;
                self.update(|st| st.last_status = Some(detail));
            }
        }
        // ping（values を送っていない間だけ）。
        let now = Instant::now();
        if now.duration_since(s.last_sent) >= ping_interval {
            let seq = self.next_seq();
            send_text(ws_tx, ping_message(seq)).await?;
            s.last_sent = now;
        }
        Ok(())
    }
}

/// 1 本の WS の間だけ持つもの。
struct SessionState {
    queries: Vec<QueryPlan>,
    limits: WireLimits,
    outbox: Outbox,
    poller: Option<Poller>,
    events: Option<mpsc::Receiver<PollEvent>>,
    poller_failures: u32,
    poller_restart_at: Option<Instant>,
    last_sent: Instant,
    /// 問い合わせのタスクが知らせた DB 側の状態。
    db_status: Option<String>,
    /// 最後に Hub に送った `status` の中身（`None` = まだ何も知らせていない・正常）。
    reported_status: Option<String>,
    status_ready_at: Instant,
    /// 1 回だけ warn したコード。
    logged_codes: HashSet<String>,
}

impl SessionState {
    /// 問い合わせのタスクが止めていないのに終わった: 全クエリの全キー `bad` を送信待ちの
    /// 失敗の列へ（作り直したタスクの周期で置き換わらない。このモジュールの doc
    /// 「タスクの異常」）。
    fn queue_failure_notices(&mut self) {
        for (index, query) in self.queries.iter().enumerate() {
            let messages = split_cycle(
                query.all_bad(),
                self.limits.max_items,
                self.limits.max_message_bytes,
            );
            self.outbox.push_failure(index, messages);
        }
    }

    /// バケットが今送ることを許す次のメッセージを取り出し、バケットから引く（無い・待つ
    /// 必要があれば `None`）。
    fn take_sendable(&mut self, budget: &mut SendBudget, now: Instant) -> Option<Vec<ValueItem>> {
        let len = self.outbox.peek_len()?;
        if !budget.wait_for(len, now).is_zero() {
            return None;
        }
        let (_, items) = self.outbox.pop()?;
        budget.take(items.len(), now);
        Some(items)
    }

    /// 今の `status` の文言（DB 側の状態と、捨てた周期・上限で断られた数）。
    fn compose_status(&self, status: &Mutex<SourceStatus>) -> Option<String> {
        let st = lock(status).clone();
        let mut parts = Vec::new();
        if let Some(db) = &self.db_status {
            parts.push(db.clone());
        }
        if st.superseded_cycles > 0 {
            parts.push(format!(
                "送りきる前に次の周期が来たため古い周期の未送信分を {} 回（{} 件）捨てました",
                st.superseded_cycles,
                self.outbox.superseded_items()
            ));
        }
        if st.rate_limited + st.too_many_items > 0 {
            parts.push(format!(
                "Hub が上限で断ったメッセージ: 頻度 {}・件数 {}",
                st.rate_limited, st.too_many_items
            ));
        }
        (!parts.is_empty()).then(|| parts.join("; "))
    }

    /// 送るべき `status` の変化（無ければ `None`）。問題が消えたら「正常」を 1 回送る。
    fn status_change(&mut self, status: &Mutex<SourceStatus>) -> Option<String> {
        let now = self.compose_status(status);
        if now == self.reported_status {
            return None;
        }
        let previous = std::mem::replace(&mut self.reported_status, now.clone());
        match now {
            Some(text) => Some(text),
            None if previous.is_some() => Some("問い合わせは正常です".to_string()),
            None => None,
        }
    }

    fn next_wake(
        &self,
        now: Instant,
        ping_interval: Duration,
        budget: &mut SendBudget,
        status_pending: bool,
    ) -> Instant {
        let mut wake = self.last_sent + ping_interval;
        if let Some(len) = self.outbox.peek_len() {
            wake = wake.min(now + budget.wait_for(len, now));
        }
        if status_pending {
            wake = wake.min(self.status_ready_at.max(now));
        }
        if let Some(at) = self.poller_restart_at {
            wake = wake.min(at);
        }
        wake
    }
}

enum Event {
    Stop,
    Incoming(Option<Result<Message, WsError>>),
    PollerEnded(Result<(), JoinError>),
    Polled(Option<PollEvent>),
    ConfigConfirmed,
    Timer,
}

async fn join_poller(poller: &mut Option<Poller>) -> Result<(), JoinError> {
    match poller {
        Some(poller) => (&mut poller.handle).await,
        None => std::future::pending().await,
    }
}

async fn recv_events(events: &mut Option<mpsc::Receiver<PollEvent>>) -> Option<PollEvent> {
    match events {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

async fn read_welcome(ws_rx: &mut futures_util::stream::SplitStream<Ws>) -> Option<ServerMessage> {
    while let Some(message) = ws_rx.next().await {
        match message.ok()? {
            Message::Text(text) => {
                if let Some(message @ ServerMessage::Welcome { .. }) =
                    parse_server_message(text.as_str())
                {
                    return Some(message);
                }
            }
            Message::Close(_) => return None,
            _ => {}
        }
    }
    None
}

async fn send_text(
    ws_tx: &mut futures_util::stream::SplitSink<Ws, Message>,
    text: String,
) -> Result<(), String> {
    match tokio::time::timeout(SEND_TIMEOUT, ws_tx.send(Message::Text(text.into()))).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err("Hub への送信に失敗しました".to_string()),
        Err(_) => Err("Hub への送信がタイムアウトしました".to_string()),
    }
}

/// close 1000 を送り、相手の close を [`CLOSE_TIMEOUT`] まで待つ（§10.3「停止」）。
async fn close_ws(
    ws_tx: &mut futures_util::stream::SplitSink<Ws, Message>,
    ws_rx: &mut futures_util::stream::SplitStream<Ws>,
) {
    let frame = CloseFrame {
        code: CloseCode::Normal,
        reason: "".into(),
    };
    let _ = tokio::time::timeout(CLOSE_TIMEOUT, ws_tx.send(Message::Close(Some(frame)))).await;
    let _ = tokio::time::timeout(CLOSE_TIMEOUT, async {
        while let Some(Ok(message)) = ws_rx.next().await {
            if matches!(message, Message::Close(_)) {
                break;
            }
        }
    })
    .await;
}

/// UTF-8 の文字の境界で `max` バイトまでに切り詰める。
pub fn truncate_bytes(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ingest_ws_url_maps_the_hub_url() {
        assert_eq!(
            ingest_ws_url("http://127.0.0.1:8800", 12),
            "ws://127.0.0.1:8800/api/v1/ingest/12/stream"
        );
        assert_eq!(
            ingest_ws_url("http://127.0.0.1:8800/", 3),
            "ws://127.0.0.1:8800/api/v1/ingest/3/stream"
        );
        assert_eq!(
            ingest_ws_url("https://hub.local", 1),
            "wss://hub.local/api/v1/ingest/1/stream"
        );
    }

    #[test]
    fn truncate_bytes_keeps_char_boundaries() {
        assert_eq!(truncate_bytes("abc", 10), "abc");
        let text = "あいう"; // 9 バイト
        assert_eq!(truncate_bytes(text, 7), "あい");
        assert_eq!(truncate_bytes(text, 6), "あい");
        assert_eq!(truncate_bytes(&"x".repeat(2000), 1024).len(), 1024);
    }

    #[test]
    fn connect_errors_do_not_echo_anything_from_the_request() {
        for err in [
            ConnectError::Http(401),
            ConnectError::Http(403),
            ConnectError::Http(404),
            ConnectError::Http(409),
            ConnectError::Http(500),
            ConnectError::Transport,
            ConnectError::Timeout,
            ConnectError::InvalidRequest,
        ] {
            let text = err.describe();
            assert!(!text.contains("bh_"), "{text}");
            assert!(!text.contains("ws://"), "{text}");
        }
    }

    fn session_state() -> SessionState {
        let now = Instant::now();
        SessionState {
            queries: Vec::new(),
            limits: WireLimits {
                max_items: 1000,
                max_message_bytes: 262_144,
                messages_per_sec: 20,
                items_per_sec: 5000,
            },
            outbox: Outbox::new(0),
            poller: None,
            events: None,
            poller_failures: 0,
            poller_restart_at: None,
            last_sent: now,
            db_status: None,
            reported_status: None,
            status_ready_at: now,
            logged_codes: HashSet::new(),
        }
    }

    /// §10.3「タスクの異常」（PR #764 のオーナーレビュー P2）: 64 クエリ × 1 キー（周期 60 秒）で
    /// タスクが止まり、1 秒後に作り直したタスクがすぐ全クエリの周期を出しても、バケットの
    /// 速さ（16 メッセージ / 秒）で送る間に、どのキーにも新しい値より先に `bad` が届く。
    /// 送る速さはバケットを超えない。
    #[test]
    fn failure_bads_reach_every_key_before_newer_values() {
        use crate::hub_api::{BridgeColumnConfig, BridgeQueryConfig};
        use crate::source::wire::WireQuality;
        const QUERIES: usize = 64;
        let key = |q: usize| format!("k{q:02}");
        let mut s = session_state();
        s.queries = (0..QUERIES)
            .map(|q| {
                QueryPlan::from_config(&BridgeQueryConfig {
                    name: format!("q{q}"),
                    period_ms: 60_000,
                    sql: "SELECT 1 AS a".to_string(),
                    columns: vec![BridgeColumnConfig {
                        column: "a".to_string(),
                        key: key(q),
                    }],
                })
            })
            .collect();
        s.outbox = Outbox::new(QUERIES);
        let limits = WireLimits {
            max_items: 1000,
            max_message_bytes: 262_144,
            messages_per_sec: 20,
            items_per_sec: 5000,
        };
        s.limits = limits;
        let start = Instant::now();
        let mut budget = SendBudget::new(limits, start);
        let step = Duration::from_millis(10);
        let mut now = start;
        let mut sent: Vec<(Instant, ValueItem)> = Vec::new();
        let mut send_ready = |s: &mut SessionState, now: Instant, sent: &mut Vec<_>| {
            while let Some(items) = s.take_sendable(&mut budget, now) {
                sent.extend(items.into_iter().map(|item| (now, item)));
            }
        };
        // 動いていたタスクの周期（Good 1.0）を送りきる。
        for q in 0..QUERIES {
            s.outbox
                .push_cycle(q, vec![vec![ValueItem::good(&key(q), 1.0)]]);
        }
        while !s.outbox.is_empty() {
            send_ready(&mut s, now, &mut sent);
            now += step;
        }
        // 2 周目の周期の途中（半分だけ入った）でタスクが止まる。
        for q in 0..QUERIES / 2 {
            s.outbox
                .push_cycle(q, vec![vec![ValueItem::good(&key(q), 1.5)]]);
        }
        let failed_at = now;
        s.queue_failure_notices();
        // 1 秒後に作り直したタスクが、すぐ全クエリの最初の周期（Good 2.0）を出す。
        let restart_at = failed_at + Duration::from_secs(1);
        let mut restarted = false;
        while !restarted || !s.outbox.is_empty() {
            if !restarted && now >= restart_at {
                for q in 0..QUERIES {
                    s.outbox
                        .push_cycle(q, vec![vec![ValueItem::good(&key(q), 2.0)]]);
                }
                restarted = true;
            }
            send_ready(&mut s, now, &mut sent);
            now += step;
        }
        for q in 0..QUERIES {
            let after: Vec<&ValueItem> = sent
                .iter()
                .filter(|(at, item)| *at >= failed_at && item.key == key(q))
                .map(|(_, item)| item)
                .collect();
            assert_eq!(
                after.first().map(|i| i.q),
                Some(WireQuality::Bad),
                "{}: {after:?}",
                key(q)
            );
            assert_eq!(
                after.last().map(|i| (i.q, i.v)),
                Some((WireQuality::Good, Some(2.0))),
                "{}",
                key(q)
            );
            // 止まったタスクの未送信の周期（1.5）は送らない。
            assert!(after.iter().all(|i| i.v != Some(1.5)), "{after:?}");
        }
        // 速さはバケット（容量 16 + 持続 16 / 秒）を超えない。
        for (i, (t0, _)) in sent.iter().enumerate() {
            let in_window = sent[i..]
                .iter()
                .take_while(|(t, _)| *t < *t0 + Duration::from_secs(1))
                .count();
            assert!(in_window <= 32, "{in_window} messages in 1 s");
        }
        let total = now.duration_since(start).as_secs_f64();
        assert!(
            (sent.len() as f64) <= 16.0 + 16.0 * total,
            "{} messages in {total} s",
            sent.len()
        );
    }

    /// `status` は変わったときだけ。問題が消えたら「正常」を 1 回。
    #[test]
    fn status_changes_are_reported_once() {
        let status = Mutex::new(SourceStatus::default());
        let mut s = session_state();
        assert_eq!(s.status_change(&status), None);
        s.db_status = Some("line1: 結果が 0 行です".to_string());
        assert_eq!(
            s.status_change(&status).as_deref(),
            Some("line1: 結果が 0 行です")
        );
        assert_eq!(s.status_change(&status), None);
        lock(&status).rate_limited = 2;
        assert!(s.status_change(&status).unwrap().contains("頻度 2"));
        s.db_status = None;
        lock(&status).rate_limited = 0;
        assert_eq!(
            s.status_change(&status).as_deref(),
            Some("問い合わせは正常です")
        );
        assert_eq!(s.status_change(&status), None);
    }

    // --- 偽の Hub（WS の送信口だけ）に対する、DB なしの単体テスト ---------------
    //
    // 参照先の DB のポートを 0 にすると、問い合わせのタスクは接続の失敗（範囲外のポート）で
    // すぐに全キー `bad` の周期を出す（その後 1 秒のバックオフ）。値の中身ではなく、
    // `state` に対して「いつ・どの epoch で送るか」を見る。

    use crate::hub_api::{BridgeColumnConfig, BridgeDbConfig, BridgeQueryConfig};
    use serde_json::{json, Value};
    use tokio::net::TcpListener;
    use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};

    type ServerWs = WebSocketStream<TcpStream>;

    fn test_source(connection_id: i64) -> BridgeSourceConfig {
        BridgeSourceConfig {
            connection_id,
            name: "fake".to_string(),
            stale_timeout_ms: 3_000,
            config_fingerprint: "sha256:00".to_string(),
            ingest_allowed: true,
            queries: vec![BridgeQueryConfig {
                name: "q".to_string(),
                period_ms: 500,
                sql: "SELECT 1 AS a".to_string(),
                columns: vec![
                    BridgeColumnConfig {
                        column: "a".to_string(),
                        key: "k1".to_string(),
                    },
                    BridgeColumnConfig {
                        column: "a".to_string(),
                        key: "k2".to_string(),
                    },
                ],
            }],
            db: BridgeDbConfig {
                id: 1,
                name: "pg".to_string(),
                enabled: true,
                host: "127.0.0.1".to_string(),
                port: 0,
                database: None,
                username: None,
                password: Some("pw-unit".to_string()),
            },
        }
    }

    struct FakeHub {
        ws: ServerWs,
        authorization: Option<String>,
    }

    async fn fake_hub() -> (TcpListener, String) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "ws://{}/api/v1/ingest/7/stream",
            listener.local_addr().unwrap()
        );
        (listener, url)
    }

    // tungstenite の握手のコールバックの型（`Result<Response, ErrorResponse>`）が大きい。
    #[allow(clippy::result_large_err)]
    async fn accept(listener: &TcpListener) -> FakeHub {
        let (stream, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept())
            .await
            .expect("the bridge connects")
            .unwrap();
        let captured = Arc::new(Mutex::new(None));
        let sink = captured.clone();
        let ws =
            tokio_tungstenite::accept_hdr_async(stream, move |req: &Request, resp: Response| {
                *lock(&sink) = req
                    .headers()
                    .get("authorization")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string);
                Ok(resp)
            })
            .await
            .unwrap();
        let authorization = lock(&captured).clone();
        FakeHub { ws, authorization }
    }

    impl FakeHub {
        async fn send(&mut self, value: Value) {
            self.ws
                .send(Message::Text(value.to_string().into()))
                .await
                .unwrap();
        }

        async fn welcome(&mut self, collecting: bool, epoch: u64) {
            self.send(
                json!({ "op": "welcome", "connection_id": 7, "connection": "fake",
                "collecting": collecting, "epoch": epoch, "stale_timeout_ms": 3000,
                "limits": { "max_items": 1000, "max_message_bytes": 262144,
                            "messages_per_sec": 20, "items_per_sec": 5000 } }),
            )
            .await;
        }

        async fn state(&mut self, collecting: bool, epoch: u64, config_changed: bool) {
            self.send(
                json!({ "op": "state", "collecting": collecting, "epoch": epoch,
                "config_changed": config_changed }),
            )
            .await;
        }

        /// `within` の間に届いた `values` を全部（`ping`・`status` は読み飛ばす）。
        async fn values_within(&mut self, within: Duration) -> Vec<Value> {
            let mut out = Vec::new();
            let deadline = tokio::time::Instant::now() + within;
            while let Ok(Some(Ok(message))) =
                tokio::time::timeout_at(deadline, self.ws.next()).await
            {
                if let Message::Text(text) = message {
                    let value: Value = serde_json::from_str(text.as_str()).unwrap();
                    if value["op"] == "values" {
                        out.push(value);
                    }
                }
            }
            out
        }

        /// 次の `values` を待つ。
        async fn next_values(&mut self) -> Value {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
            loop {
                let message = tokio::time::timeout_at(deadline, self.ws.next())
                    .await
                    .expect("values within 10 s")
                    .expect("open")
                    .unwrap();
                if let Message::Text(text) = message {
                    let value: Value = serde_json::from_str(text.as_str()).unwrap();
                    if value["op"] == "values" {
                        return value;
                    }
                }
            }
        }
    }

    fn context(url: &str) -> (SourceContext, watch::Sender<u64>) {
        let (generation_tx, generation_rx) = watch::channel(0);
        let mut redactor = Redactor::new();
        redactor.add_secret("pw-unit");
        (
            SourceContext {
                ws_url: url.to_string(),
                api_key: "bh_unit_test_key".to_string(),
                redactor,
                refresh: Arc::new(Notify::new()),
                fetch_started: Arc::new(AtomicU64::new(0)),
                config_generation: generation_rx,
            },
            generation_tx,
        )
    }

    /// §11 の R2a-3b: `collecting: false` で問い合わせを止める・`state` の epoch で送り直す・
    /// `config_changed` は設定を取り直してから送る。キーは upgrade のヘッダにだけ載る。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_session_follows_state_and_epochs() {
        let (listener, url) = fake_hub().await;
        let (ctx, generation_tx) = context(&url);
        let refresh = ctx.refresh.clone();
        let fetch_started = ctx.fetch_started.clone();
        let handle = SourceHandle::spawn(test_source(7), ctx);
        let mut hub = accept(&listener).await;
        assert_eq!(
            hub.authorization.as_deref(),
            Some("Bearer bh_unit_test_key")
        );

        // 収集の停止中: 問い合わせない・送らない。
        hub.welcome(false, 1).await;
        assert!(hub
            .values_within(Duration::from_millis(700))
            .await
            .is_empty());
        let status = handle.status();
        assert_eq!(status.state, SourceState::Idle);
        assert_eq!(status.pollers_started, 0);

        // 開始（epoch 2）: すぐ最初の周期を送る（DB に届かないので全キー bad）。
        hub.state(true, 2, false).await;
        let first = hub.next_values().await;
        assert_eq!(first["epoch"], json!(2));
        assert_eq!(
            first["items"],
            json!([ { "key": "k1", "v": null, "q": "bad" }, { "key": "k2", "v": null, "q": "bad" } ])
        );

        // epoch が進んだ（設定の変更なし）: 作り直して新しい epoch で最初から送る。
        hub.state(true, 3, false).await;
        let mut resent = hub.next_values().await;
        while resent["epoch"] == json!(2) {
            resent = hub.next_values().await;
        }
        assert_eq!(resent["epoch"], json!(3));
        assert!(handle.status().pollers_started >= 2);

        // 設定の変更: 取り直しを頼み、取得が終わるまで送らない。
        hub.state(true, 4, true).await;
        tokio::time::timeout(Duration::from_secs(5), refresh.notified())
            .await
            .expect("config_changed must ask the supervisor to refetch");
        let during = hub.values_within(Duration::from_millis(1_200)).await;
        assert!(
            during.iter().all(|v| v["epoch"] == json!(3)),
            "no values with the new epoch before the refetch: {during:?}"
        );
        assert_eq!(handle.status().state, SourceState::AwaitingConfig);
        // 頼む前から走っていた取得の終わり（番号 0）では再開しない。
        generation_tx.send_replace(0);
        assert!(hub
            .values_within(Duration::from_millis(500))
            .await
            .iter()
            .all(|v| v["epoch"] != json!(4)));
        // 頼んだ後に始まった取得（番号 1）が終わったら、新しい epoch で送る。
        fetch_started.store(1, Ordering::SeqCst);
        generation_tx.send_replace(1);
        let mut after = hub.next_values().await;
        while after["epoch"] != json!(4) {
            after = hub.next_values().await;
        }

        // 停止（collecting: false）: 問い合わせを止め、それ以降は送らない。
        hub.state(false, 5, false).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        let late = hub.values_within(Duration::from_millis(1_500)).await;
        assert!(late.is_empty(), "values after collecting: false: {late:?}");
        assert_eq!(handle.status().state, SourceState::Idle);

        // 止める: close 1000。
        assert!(handle.stop(Duration::from_secs(5)).await.is_ok());
        let closed = tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(Ok(message)) = hub.ws.next().await {
                if let Message::Close(frame) = message {
                    return frame.map(|f| u16::from(f.code));
                }
            }
            None
        })
        .await
        .unwrap();
        assert_eq!(closed, Some(1000));
    }

    /// §10.3 の実装の注 (3)（PR #764 のオーナーレビュー P2）: `config_changed` の取り直しが
    /// 遅れている・失敗している間に WS が切れても、張り直した `welcome` の `collecting: true`
    /// で手元の古い設定のまま問い合わせ・送信を始めない。頼む前から走っていた取得が終わっても
    /// 再開せず、頼んだ後に始まった取得の成功で、新しい epoch の最初の周期から送る。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_pending_config_refetch_survives_a_reconnect() {
        let (listener, url) = fake_hub().await;
        let (ctx, generation_tx) = context(&url);
        let refresh = ctx.refresh.clone();
        let fetch_started = ctx.fetch_started.clone();
        let handle = SourceHandle::spawn(test_source(7), ctx);
        let mut hub = accept(&listener).await;
        hub.welcome(true, 1).await;
        assert_eq!(hub.next_values().await["epoch"], json!(1));

        // 頼む前から取得（番号 1）が走っている（遅れている）ところへ設定の変更。
        fetch_started.store(1, Ordering::SeqCst);
        hub.state(true, 2, true).await;
        tokio::time::timeout(Duration::from_secs(5), refresh.notified())
            .await
            .expect("config_changed must ask the supervisor to refetch");
        let wait_awaiting = tokio::time::timeout(Duration::from_secs(5), async {
            while handle.status().state != SourceState::AwaitingConfig {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        });
        wait_awaiting.await.expect("AwaitingConfig");
        let pollers_before = handle.status().pollers_started;

        // 取り直しが終わらないうちに WS が切れる（close を送らずに落とす）。
        drop(hub);
        // 頼む前から走っていた取得が（古い設定を返して）終わる - これでは再開しない。
        generation_tx.send_replace(1);

        // 張り直し: Hub は収集中・新しい epoch。
        let mut hub = accept(&listener).await;
        hub.welcome(true, 2).await;
        let during = hub.values_within(Duration::from_millis(1_500)).await;
        assert!(
            during.is_empty(),
            "no values before the requested refetch succeeds: {during:?}"
        );
        let status = handle.status();
        assert_eq!(status.state, SourceState::AwaitingConfig);
        assert_eq!(status.pollers_started, pollers_before, "{status:?}");
        assert_eq!(status.sessions, 2, "{status:?}");

        // 取得の失敗（番号 2 が始まったが成功しない）: generation は進まず、送らない。
        fetch_started.store(2, Ordering::SeqCst);
        assert!(hub
            .values_within(Duration::from_millis(500))
            .await
            .is_empty());

        // 頼んだ後に始まった取得（番号 3）が成功したら、新しい epoch で最初の周期から送る。
        fetch_started.store(3, Ordering::SeqCst);
        generation_tx.send_replace(3);
        let after = hub.next_values().await;
        assert_eq!(after["epoch"], json!(2));
        assert_eq!(handle.status().state, SourceState::Running);
        assert!(handle.stop(Duration::from_secs(5)).await.is_ok());
    }

    /// 取り直しを頼んだ後に始まった取得が WS の切断中に成功していれば、張り直した `welcome`
    /// でそれを見て（`config_generation` の変化を待たずに）すぐ再開する。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_refetch_that_finished_while_disconnected_resumes_on_welcome() {
        let (listener, url) = fake_hub().await;
        let (ctx, generation_tx) = context(&url);
        let refresh = ctx.refresh.clone();
        let fetch_started = ctx.fetch_started.clone();
        let handle = SourceHandle::spawn(test_source(7), ctx);
        let mut hub = accept(&listener).await;
        hub.welcome(true, 1).await;
        hub.next_values().await;
        hub.state(true, 2, true).await;
        tokio::time::timeout(Duration::from_secs(5), refresh.notified())
            .await
            .expect("refetch requested");
        let wait_awaiting = tokio::time::timeout(Duration::from_secs(5), async {
            while handle.status().state != SourceState::AwaitingConfig {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        });
        wait_awaiting.await.expect("AwaitingConfig");
        drop(hub);
        fetch_started.store(1, Ordering::SeqCst);
        generation_tx.send_replace(1);
        let mut hub = accept(&listener).await;
        hub.welcome(true, 2).await;
        assert_eq!(hub.next_values().await["epoch"], json!(2));
        assert!(handle.stop(Duration::from_secs(5)).await.is_ok());
    }

    /// 問い合わせのタスクの `status`（DB に届かない）は Hub に届き、パスワードを含まない。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn db_failures_are_reported_through_status_without_secrets() {
        let (listener, url) = fake_hub().await;
        let (ctx, _generation_tx) = context(&url);
        let handle = SourceHandle::spawn(test_source(7), ctx);
        let mut hub = accept(&listener).await;
        hub.welcome(true, 1).await;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let detail = loop {
            let message = tokio::time::timeout_at(deadline, hub.ws.next())
                .await
                .expect("status within 10 s")
                .unwrap()
                .unwrap();
            if let Message::Text(text) = message {
                let value: Value = serde_json::from_str(text.as_str()).unwrap();
                if value["op"] == "status" {
                    break value["detail"].as_str().unwrap().to_string();
                }
            }
        };
        assert!(detail.contains("DB に接続できません"), "{detail}");
        assert!(!detail.contains("pw-unit"), "{detail}");
        assert!(!detail.contains("bh_unit_test_key"), "{detail}");
        assert!(handle.stop(Duration::from_secs(5)).await.is_ok());
    }
}
