//! DB ブリッジ（R2a-3b、docs/banto-hub-ingest-design.md §10.3）: Hub のブリッジの `push`
//! 接続（`settings.bridge.kind == "db"`）ごとに、参照先の PostgreSQL へ問い合わせて、
//! 取り込み口の WebSocket `GET /api/v1/ingest/{connectionId}/stream` へ値を送る。
//!
//! Sink のエンジン（購読 → INSERT、[`crate::run`]）と**同じプロセス**で、**別のタスク群**として
//! 動く（§10.3 末尾）。片方の不具合がもう片方を止めないよう、設定の取得
//! （`GET /api/ingest/bridge-config`）と、その失敗の扱いはエンジンの `GET /api/sink/config` と
//! 独立。
//!
//! ## 構成
//!
//! ```text
//!   banto-hub                                   banto-hub-sink（ブリッジ）              PostgreSQL
//!   ─────────                                   ─────────────────────────              ──────────
//!   GET /api/ingest/bridge-config ◀──────────── [監督（このファイル）] 30 秒ごと・頼まれたらすぐ
//!                                                  │ 差分 → source を起動・停止
//!   GET /api/v1/ingest/{id}/stream ◀──────────▶ [session] source ごとに WS 1 本 ─┐
//!        welcome / state / ack / error                │ 分割・速さの抑え・最新を優先   │
//!        values / ping / status                       │                               │
//!                                                [poll] 問い合わせのタスク ─────────────▶ SELECT
//!                                                  （collecting の間だけ）
//! ```
//!
//! | モジュール   | 内容                                                                                   |
//! | ------------ | -------------------------------------------------------------------------------------- |
//! | このファイル | 監督: 設定の取得・差分（[`plan_sources`]）・source の起動と停止・取り直しの依頼          |
//! | [`session`]  | source 1 つ: WS・`welcome` / `state` / `ack` / `error`・epoch・送信・`ping`・`status`   |
//! | [`poll`]     | 問い合わせのタスク: describe・最小デッドライン・品質の梯子（共有 crate `banto-db-source`） |
//! | [`split`]    | 1 周期を件数（1,000）・バイト数（256 KiB − 512）で分ける（§7.1 の (a)）                  |
//! | [`budget`]   | 接続ごとのトークンバケット（`welcome` の上限の 8 割、§7.1・§12 Q20）                    |
//! | [`outbox`]   | 送りきる前に次の周期が出たら古い方を捨てる送信待ち                                      |
//! | [`wire`]     | §6.1 のメッセージ                                                                       |
//!
//! ## 設定の差分（§10.3「設定」）
//!
//! `config_refresh_secs`（既存の設定、既定 30 秒）ごとと、source が頼んだとき（WS の
//! `welcome` のたび・Hub の `state { config_changed: true }`・404・409・close の
//! `connection_changed` / `connection_removed`）にすぐ取り直す。依頼は 1 つの許可に畳むので、
//! 取得の最中に何本の source が頼んでも、その後に 1 回取るだけ（[`session`] の「設定の
//! 取り直しの待ち」）。toml の項目は増やさない。差分は純関数
//! [`plan_sources`]: 内容が 1 バイトも変わっていない source はそのまま（WS を張り直さない）、
//! 変わった（`configFingerprint`・DB の接続先とパスワード・`staleTimeoutMs` を含む）ものは
//! 止めて作り直し（describe からやり直す）、消えたものは止める。
//!
//! **動かさない source**（[`Parked`]）: API キーに `ingest:#{id}` が無い（`ingestAllowed:
//! false` = 「権限なし」）、参照先の DB 接続が Hub で無効、クエリが 1 本も無い。どれも
//! 状態（[`BridgeSupervisor::statuses`]）に理由を出し、ログに 1 回だけ warn して、DB にも
//! Hub の送信口にも繋がない（§11 の R2a-3b の監査「スコープの無い接続を動かさない」）。
//! API キーは今の Sink の 1 本に `ingest:#{id}` を足す（`admin` + `read` + `ingest:#12` …、
//! §10.3）。Hub は `ingestAllowed: false` の source のパスワードを返さない。
//!
//! 取得の失敗（Hub 未起動・キーの失効など）では動いている source を止めない（WS の側で
//! それぞれ張り直す）。この版のブリッジを知らない古い Hub（404）は「source なし」として
//! 全部止める。
//!
//! ## 資格情報
//!
//! パスワードは [`crate::hub_api::BridgeDbConfig`]（`Debug` で伏せる）と問い合わせのタスクの
//! 接続オプションにだけ置く。取得のたびに [`crate::status::Redactor`] へ登録し（API キーも）、
//! ログ・`status` の文言はそれを通す。ログに出すのは source の名前・id・件数・Hub のコード・
//! サニタイズした DB の文言だけ。
//!
//! ## 止め方（§10.3「停止」）
//!
//! [`BridgeSupervisor::shutdown`] は全 source に止める合図を送り、各 source は close 1000 を
//! 送って問い合わせのタスクの終わりを待つ。待つのは呼び出し側が渡した予算まで
//! （[`crate::run`] の停止の予算の中）。送れなかった周期は捨てる（flush は無い）。
//!
//! 予算内に終わらなかったら（応答しない DB で問い合わせが上限まで掛かっている、など）、
//! ブリッジのタスク（監督・source・問い合わせ）を**手放さずに** `abort` し、全部の future が
//! 落ちたことを確かめてから戻る（[`BridgeTasks`]。PR #764 の Codex レビュー P2）。handle を
//! 落として切り離すと、`run` の後で runtime が落ちるときに sqlx の操作・プールの後始末の途中で
//! 止まり、それが「止まった」ことを誰も確かめない（implementation-checklist §5「待ちを時間
//! 切れで落とすときは handle を手放さない」）。`abort` の待ちの分は予算から先に取り分けて
//! おくので、全体は予算を超えない。
//!
//! ## 問い合わせは source ごとに 1 本（PR #764 の Codex レビュー P2）
//!
//! 接続 id ごとに枠（[`tokio::sync::Semaphore`] の許可 1 つ）を 1 つ持ち、問い合わせのタスクは
//! 枠を持っている間だけ動く（[`session`] の「問い合わせは 1 本ずつ」）。監督が設定の変更で
//! source を作り直したときも、新しい source は古い source の問い合わせが終わるまで枠を取れない。

pub mod budget;
pub mod outbox;
pub mod poll;
pub mod session;
pub mod split;
pub mod wire;

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use banto_tagclient::PinnedHubTls;

use tokio::sync::{watch, Notify, Semaphore};
use tokio::task::{AbortHandle, JoinHandle};
use tokio::time::Instant;

use crate::config::SidecarConfig;
use crate::flush::backoff_delay;
use crate::hub_api::{BridgeConfig, BridgeSourceConfig, HubApi, HubApiError, UnavailableSource};
use crate::log::{log_err_line, log_line};
use crate::status::Redactor;

pub use session::{ingest_ws_url, SourceHandle, SourceState, SourceStatus};

/// 動かさない理由（このモジュールの doc「動かさない source」）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Parked {
    /// API キーに `ingest:#{id}` が無い（「権限なし」）。
    NoPermission,
    /// 参照先の `postgres` 接続が Hub で無効。
    DbDisabled,
    /// クエリ（か送るキー）が 1 つも無い。
    NothingToQuery,
}

impl Parked {
    fn state(self) -> SourceState {
        match self {
            Self::NoPermission => SourceState::NoPermission,
            Self::DbDisabled | Self::NothingToQuery => SourceState::DbDisabled,
        }
    }

    fn message(self) -> &'static str {
        match self {
            Self::NoPermission => {
                "権限なし: API キーにこの接続の ingest:#{id} のスコープがありません（キーを発行し直して banto-hub-sink.toml を差し替えてください）"
            }
            Self::DbDisabled => "参照先の DB 接続が Hub で無効になっています",
            Self::NothingToQuery => "クエリがありません",
        }
    }
}

/// R2b-1d（docs/banto-hub-connection-settings-design.md §5.4）: Hub が読み先の
/// パスワードを読めない source の状態の文言（値は含まない）。
pub fn unavailable_message(source: &UnavailableSource) -> String {
    format!(
        "Hub でパスワードを読めません（読み先の DB 接続 #{}、{}）。Hub の接続の編集で\
         パスワードを入れ直してください",
        source.db_connection_id, source.reason
    )
}

/// source を動かせるか。
pub fn parked_reason(source: &BridgeSourceConfig) -> Option<Parked> {
    if !source.ingest_allowed {
        Some(Parked::NoPermission)
    } else if !source.db.enabled {
        Some(Parked::DbDisabled)
    } else if source.queries.iter().all(|q| q.columns.is_empty()) {
        Some(Parked::NothingToQuery)
    } else {
        None
    }
}

/// 設定の差分（純関数 [`plan_sources`] の結果）。
#[derive(Debug, Default, PartialEq)]
pub struct SourcePlan {
    /// 止める source（消えた・変わった・動かせなくなった）。
    pub stop: Vec<i64>,
    /// 起動する source（新しい・変わった）。
    pub start: Vec<BridgeSourceConfig>,
    /// 動かさない source とその理由。
    pub parked: Vec<(i64, String, Parked)>,
}

/// 「今動いている source」と「Hub が返した source」から差分を作る。内容が同じものは触らない。
pub fn plan_sources(
    running: &HashMap<i64, BridgeSourceConfig>,
    desired: &[BridgeSourceConfig],
) -> SourcePlan {
    let desired_by_id: HashMap<i64, &BridgeSourceConfig> =
        desired.iter().map(|s| (s.connection_id, s)).collect();
    let mut plan = SourcePlan::default();
    for (id, current) in running {
        match desired_by_id.get(id) {
            Some(next) if *next == current && parked_reason(next).is_none() => {}
            _ => plan.stop.push(*id),
        }
    }
    for source in desired {
        match parked_reason(source) {
            Some(reason) => {
                plan.parked
                    .push((source.connection_id, source.name.clone(), reason));
            }
            None => {
                if running.get(&source.connection_id) != Some(source) {
                    plan.start.push(source.clone());
                }
            }
        }
    }
    plan.stop.sort_unstable();
    plan.start.sort_by_key(|s| s.connection_id);
    plan.parked.sort_by_key(|(id, _, _)| *id);
    plan
}

/// ブリッジのタスク（監督・source・問い合わせ）の台帳。止める予算を超えたときに、全部を
/// `abort` して、全部の future が落ちる（= 取り消しが届いた）まで待つために使う
/// （このモジュールの doc「止め方」）。
///
/// 数えるのは future の生き死に: [`Self::spawn`] は future に印（[`TaskGuard`]）を持たせ、
/// 印は future が終わったときも `abort` で落とされたときも落ちる。`abort` の後に起こされた
/// タスク（`abort` と同時に動いていた親が起こした子）もすぐ `abort` する。
#[derive(Clone, Default)]
pub struct BridgeTasks {
    inner: Arc<TasksInner>,
}

#[derive(Default)]
struct TasksInner {
    state: Mutex<TasksState>,
    idle: Notify,
}

#[derive(Default)]
struct TasksState {
    live: usize,
    aborts: Vec<AbortHandle>,
    aborting: bool,
}

/// [`BridgeTasks`] の印（future と一緒に落ちる）。
struct TaskGuard(Arc<TasksInner>);

impl Drop for TaskGuard {
    fn drop(&mut self) {
        let mut state = lock(&self.0.state);
        state.live -= 1;
        if state.live == 0 {
            self.0.idle.notify_waiters();
        }
    }
}

impl BridgeTasks {
    /// 台帳に載せてタスクを起こす。
    pub fn spawn<F>(&self, future: F) -> JoinHandle<F::Output>
    where
        F: std::future::Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let mut state = lock(&self.inner.state);
        state.live += 1;
        let guard = TaskGuard(self.inner.clone());
        let handle = tokio::spawn(async move {
            let _guard = guard;
            future.await
        });
        state.aborts.retain(|abort| !abort.is_finished());
        if state.aborting {
            handle.abort();
        }
        state.aborts.push(handle.abort_handle());
        handle
    }

    /// 台帳の全部のタスクを `abort` する（以後に起こされるものも）。
    pub fn abort_all(&self) {
        let mut state = lock(&self.inner.state);
        state.aborting = true;
        for abort in &state.aborts {
            abort.abort();
        }
    }

    /// 台帳の全部のタスクの future が落ちるまで待つ。
    pub async fn wait_idle(&self) {
        loop {
            let notified = self.inner.idle.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if lock(&self.inner.state).live == 0 {
                return;
            }
            notified.await;
        }
    }

    /// 生きているタスクの数（テスト用）。
    pub fn live(&self) -> usize {
        lock(&self.inner.state).live
    }
}

/// [`BridgeSupervisor::shutdown`] の結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgeStop {
    /// 予算内に全部のタスクが終わった。
    Stopped,
    /// 予算内に終わらなかったので、全部を `abort` し、全部の future が落ちたのを確かめた。
    Aborted,
    /// `abort` しても取り分けた時間内に落ちきらなかった（タスクがスレッドを塞いでいる）。
    /// handle は落とすので、残りは runtime の終わりに任せる。
    Detached,
}

/// [`BridgeSupervisor::shutdown`] が予算から `abort` の待ちのために取り分ける時間
/// （予算の 4 分の 1 まで）。`abort` された future は次の poll で落ちるので、ふつうは一瞬。
const ABORT_JOIN_RESERVE: Duration = Duration::from_millis(500);

/// ブリッジ全体（[`crate::run`] が 1 つ起こす）。
pub struct BridgeSupervisor {
    stop_tx: watch::Sender<bool>,
    task: JoinHandle<()>,
    tasks: BridgeTasks,
    statuses: Arc<Mutex<BTreeMap<i64, Arc<Mutex<SourceStatus>>>>>,
}

impl BridgeSupervisor {
    /// 監督のタスクを起こす。戻り値の `Err` は HTTP クライアントを作れないときだけ。
    pub fn spawn(config: &SidecarConfig) -> Result<Self, HubApiError> {
        let hub = HubApi::new(config)?;
        let tasks = BridgeTasks::default();
        let (stop_tx, stop_rx) = watch::channel(false);
        let statuses = Arc::new(Mutex::new(BTreeMap::new()));
        let (generation_tx, generation_rx) = watch::channel(0u64);
        let mut redactor = Redactor::new();
        redactor.add_secret(&config.api_key);
        let supervisor = Supervisor {
            hub,
            hub_url: config.hub_url.clone(),
            hub_tls: config.hub_tls.clone(),
            api_key: config.api_key.clone(),
            refresh_interval: config.config_refresh,
            redactor,
            refresh: Arc::new(Notify::new()),
            fetch_started: Arc::new(AtomicU64::new(0)),
            generation_tx,
            generation_rx,
            tasks: tasks.clone(),
            poller_slots: HashMap::new(),
            running: HashMap::new(),
            retiring: Vec::new(),
            statuses: statuses.clone(),
            logged_parked: HashMap::new(),
            logged_unavailable: HashMap::new(),
            logged_error: None,
            attempt: 0,
        };
        let task = tasks.spawn(supervisor.run(stop_rx));
        Ok(Self {
            stop_tx,
            task,
            tasks,
            statuses,
        })
    }

    /// 全 source の状態（接続 id の順）。資格情報・値は含まない。
    pub fn statuses(&self) -> Vec<SourceStatus> {
        lock(&self.statuses)
            .values()
            .map(|status| lock(status).clone())
            .collect()
    }

    /// 止めて、`limit` まで終わりを待つ。終わらなければ、ブリッジのタスクを全部 `abort` して、
    /// 全部の future が落ちるまで待つ（handle は最後まで手放さない。このモジュールの doc
    /// 「止め方」）。全体で `limit` を超えない: `abort` の待ちの分（[`ABORT_JOIN_RESERVE`]、
    /// `limit` の 4 分の 1 まで）を先に取り分け、合図で待つのは残りまで。
    pub async fn shutdown(self, limit: Duration) -> BridgeStop {
        let _ = self.stop_tx.send(true);
        stop_or_abort(self.task, &self.tasks, limit).await
    }
}

/// 台帳の全部のタスク（監督の `task` を含む）の終わりを `limit` まで待ち、終わらなければ
/// 全部を `abort` して落ちるまで待つ（[`BridgeSupervisor::shutdown`] の本体。テストのために
/// 分けた）。`task` は最後まで持ち、全部の future が落ちてから（か取り分けの時間切れで）落とす。
async fn stop_or_abort(task: JoinHandle<()>, tasks: &BridgeTasks, limit: Duration) -> BridgeStop {
    let reserve = ABORT_JOIN_RESERVE.min(limit / 4);
    let graceful = limit.saturating_sub(reserve);
    let outcome = if tokio::time::timeout(graceful, tasks.wait_idle())
        .await
        .is_ok()
    {
        BridgeStop::Stopped
    } else {
        tasks.abort_all();
        task.abort();
        if tokio::time::timeout(reserve, tasks.wait_idle())
            .await
            .is_ok()
        {
            BridgeStop::Aborted
        } else {
            BridgeStop::Detached
        }
    };
    drop(task);
    outcome
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// 監督のタスクの状態。
struct Supervisor {
    hub: HubApi,
    hub_url: String,
    hub_tls: Option<PinnedHubTls>,
    api_key: String,
    refresh_interval: Duration,
    redactor: Redactor,
    refresh: Arc<Notify>,
    fetch_started: Arc<AtomicU64>,
    generation_tx: watch::Sender<u64>,
    generation_rx: watch::Receiver<u64>,
    tasks: BridgeTasks,
    /// 接続 id ごとの問い合わせの枠（このモジュールの doc「問い合わせは source ごとに 1 本」）。
    /// source を作り直しても同じ枠を渡す。
    poller_slots: HashMap<i64, Arc<Semaphore>>,
    running: HashMap<i64, SourceHandle>,
    /// 止めたが終わりをまだ確かめていない source（手放さない）。
    retiring: Vec<SourceHandle>,
    statuses: Arc<Mutex<BTreeMap<i64, Arc<Mutex<SourceStatus>>>>>,
    /// 1 回だけ warn した「動かさない」理由。
    logged_parked: HashMap<i64, Parked>,
    /// R2b-1d: 1 回だけ warn した「Hub でパスワードを読めません」（文言ごと）。
    logged_unavailable: HashMap<i64, String>,
    logged_error: Option<HubApiError>,
    attempt: u32,
}

impl Supervisor {
    async fn run(mut self, mut stop: watch::Receiver<bool>) {
        let mut next_fetch = Instant::now();
        loop {
            tokio::select! {
                biased;
                _ = stop.changed() => break,
                _ = self.refresh.notified() => {}
                _ = tokio::time::sleep_until(next_fetch) => {}
            }
            if *stop.borrow() {
                break;
            }
            let fetch_id = self.fetch_started.fetch_add(1, Ordering::SeqCst) + 1;
            let fetched = tokio::select! {
                biased;
                _ = stop.changed() => break,
                fetched = self.hub.fetch_bridge_config() => fetched,
            };
            let (wait, applied) = self.on_fetched(fetched);
            next_fetch = Instant::now() + wait;
            if applied {
                // 設定を当て終えたことを知らせる（取り直しを頼んで待っている source の再開の
                // 合図）。取得に失敗したときは知らせない（古い設定のまま再開させない）。
                self.generation_tx.send_replace(fetch_id);
            }
            self.retiring.retain(|handle| !handle.is_finished());
        }
        self.shutdown_sources().await;
    }

    /// 取得の結果を当て、次に取るまでの待ちと、設定を当てたかを返す。
    fn on_fetched(&mut self, fetched: Result<BridgeConfig, HubApiError>) -> (Duration, bool) {
        match fetched {
            Ok(config) => {
                if self.logged_error.take().is_some() {
                    log_line("banto-hub-sink: ブリッジの設定を取得できるようになりました");
                }
                self.attempt = 0;
                self.apply(config.sources, &config.unavailable_sources);
                (self.refresh_interval, true)
            }
            // この版のブリッジを知らない古い Hub: source なしとして扱う。
            Err(HubApiError::Status(404)) => {
                if self.logged_error != Some(HubApiError::Status(404)) {
                    log_line(
                        "banto-hub-sink: Hub が DB ブリッジの設定（GET /api/ingest/bridge-config）に対応していないため、ブリッジは動かしません",
                    );
                    self.logged_error = Some(HubApiError::Status(404));
                }
                self.attempt = 0;
                self.apply(Vec::new(), &[]);
                (self.refresh_interval, true)
            }
            Err(err) => {
                self.attempt = self.attempt.saturating_add(1);
                if self.logged_error.as_ref() != Some(&err) {
                    log_err_line(&format!(
                        "banto-hub-sink: warn: ブリッジの設定を取得できません（再試行します。動いている source は止めません）: {err}"
                    ));
                    self.logged_error = Some(err);
                }
                (
                    backoff_delay(self.attempt).min(self.refresh_interval),
                    false,
                )
            }
        }
    }

    /// `unavailable`（R2b-1d、設計 §5.4）: Hub が読み先のパスワードを読めないので
    /// `desired` に載せなかった source。動かさず（`desired` に無いので、動いていれば
    /// 止まる）、状態に「Hub でパスワードを読めません」を出す。
    fn apply(&mut self, desired: Vec<BridgeSourceConfig>, unavailable: &[UnavailableSource]) {
        for source in &desired {
            if let Some(password) = source.db.password.as_deref() {
                self.redactor.add_secret(password);
            }
        }
        let running: HashMap<i64, BridgeSourceConfig> = self
            .running
            .iter()
            .map(|(id, handle)| (*id, handle.config.clone()))
            .collect();
        let plan = plan_sources(&running, &desired);
        if !plan.stop.is_empty() || !plan.start.is_empty() {
            log_line(&format!(
                "banto-hub-sink: ブリッジの設定を反映します（source: 停止 {} / 開始 {}）",
                plan.stop.len(),
                plan.start.len()
            ));
        }
        for id in &plan.stop {
            if let Some(handle) = self.running.remove(id) {
                // 止める合図だけ送って、終わりは `retiring` で確かめる（設定の反映を待たせない）。
                handle.request_stop();
                self.retiring.push(handle);
            }
        }

        // 枠は、Hub の設定に残っている id と、まだ問い合わせが枠を持っている id の分だけ残す。
        self.poller_slots.retain(|id, slot| {
            desired.iter().any(|s| s.connection_id == *id) || slot.available_permits() == 0
        });

        let mut statuses = lock(&self.statuses);
        statuses.retain(|id, _| {
            desired.iter().any(|s| s.connection_id == *id)
                || unavailable.iter().any(|s| s.connection_id == *id)
        });
        for source in unavailable {
            let message = unavailable_message(source);
            if self.logged_unavailable.get(&source.connection_id) != Some(&message) {
                log_err_line(&format!(
                    "banto-hub-sink: warn: ブリッジ {}（#{}）は動かしません: {message}",
                    source.name, source.connection_id
                ));
                self.logged_unavailable
                    .insert(source.connection_id, message.clone());
            }
            statuses.insert(
                source.connection_id,
                Arc::new(Mutex::new(SourceStatus {
                    connection_id: source.connection_id,
                    name: source.name.clone(),
                    state: SourceState::PasswordUnreadable,
                    last_status: Some(message),
                    ..SourceStatus::default()
                })),
            );
        }
        self.logged_unavailable
            .retain(|id, _| unavailable.iter().any(|s| s.connection_id == *id));
        let parked_ids: Vec<i64> = plan.parked.iter().map(|(id, _, _)| *id).collect();
        self.logged_parked.retain(|id, _| parked_ids.contains(id));
        for (id, name, reason) in &plan.parked {
            if self.logged_parked.get(id) != Some(reason) {
                log_err_line(&format!(
                    "banto-hub-sink: warn: ブリッジ {name}（#{id}）は動かしません: {}",
                    reason.message()
                ));
                self.logged_parked.insert(*id, *reason);
            }
            statuses.insert(
                *id,
                Arc::new(Mutex::new(SourceStatus {
                    connection_id: *id,
                    name: name.clone(),
                    state: reason.state(),
                    last_status: Some(reason.message().to_string()),
                    ..SourceStatus::default()
                })),
            );
        }
        for source in plan.start {
            let id = source.connection_id;
            log_line(&format!(
                "banto-hub-sink: ブリッジ {}（#{id}）を開始します（クエリ {} 本 → /api/v1/ingest/{id}/stream）",
                source.name,
                source.queries.len()
            ));
            let poller_slot = self
                .poller_slots
                .entry(id)
                .or_insert_with(|| Arc::new(Semaphore::new(1)))
                .clone();
            let ctx = session::SourceContext {
                ws_url: ingest_ws_url(&self.hub_url, id),
                hub_tls: self.hub_tls.clone(),
                api_key: self.api_key.clone(),
                redactor: self.redactor.clone(),
                refresh: self.refresh.clone(),
                fetch_started: self.fetch_started.clone(),
                config_generation: self.generation_rx.clone(),
                tasks: self.tasks.clone(),
                poller_slot,
            };
            let handle = SourceHandle::spawn(source, ctx);
            statuses.insert(id, handle.shared_status());
            self.running.insert(id, handle);
        }
    }

    async fn shutdown_sources(&mut self) {
        let mut handles: Vec<SourceHandle> = self.running.drain().map(|(_, h)| h).collect();
        handles.append(&mut self.retiring);
        for handle in &handles {
            handle.request_stop();
        }
        // 呼び出し側（[`BridgeSupervisor::shutdown`]）が全体の上限を掛けるので、ここは順に待つ。
        for handle in handles {
            let _ = handle.stop(Duration::from_secs(3600)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub_api::{BridgeColumnConfig, BridgeDbConfig, BridgeQueryConfig};

    fn source(id: i64, fingerprint: &str) -> BridgeSourceConfig {
        BridgeSourceConfig {
            connection_id: id,
            name: format!("bridge{id}"),
            stale_timeout_ms: 10_000,
            config_fingerprint: fingerprint.to_string(),
            ingest_allowed: true,
            queries: vec![BridgeQueryConfig {
                name: "q".to_string(),
                period_ms: 1_000,
                sql: "SELECT 1 AS a".to_string(),
                columns: vec![BridgeColumnConfig {
                    column: "a".to_string(),
                    key: format!("k{id}"),
                }],
            }],
            db: BridgeDbConfig {
                id: 1,
                name: "pg".to_string(),
                enabled: true,
                host: "127.0.0.1".to_string(),
                port: 5432,
                database: Some("db".to_string()),
                username: Some("u".to_string()),
                password: Some("p".to_string()),
            },
        }
    }

    fn running(sources: &[BridgeSourceConfig]) -> HashMap<i64, BridgeSourceConfig> {
        sources
            .iter()
            .map(|s| (s.connection_id, s.clone()))
            .collect()
    }

    /// §11 の R2a-3b「設定の差分の適用」: 同じものは触らない・変わったものは作り直す・
    /// 消えたものは止める・新しいものは起こす。
    #[test]
    fn plan_touches_only_what_changed() {
        let current = running(&[source(1, "a"), source(2, "b"), source(3, "c")]);
        let mut changed_db = source(3, "c");
        changed_db.db.password = Some("rotated".to_string());
        let desired = vec![
            source(1, "a"),
            source(2, "b2"),
            changed_db.clone(),
            source(4, "d"),
        ];
        let plan = plan_sources(&current, &desired);
        assert_eq!(plan.stop, vec![2, 3]);
        assert_eq!(
            plan.start
                .iter()
                .map(|s| s.connection_id)
                .collect::<Vec<_>>(),
            vec![2, 3, 4]
        );
        assert!(plan.parked.is_empty());

        // 消えたものは止めるだけ。
        let plan = plan_sources(&current, &[source(1, "a")]);
        assert_eq!(plan.stop, vec![2, 3]);
        assert!(plan.start.is_empty());

        // 何も変わらなければ空。
        let same = vec![source(1, "a"), source(2, "b"), source(3, "c")];
        assert_eq!(plan_sources(&current, &same), SourcePlan::default());
    }

    /// §11 の R2a-3b の監査「スコープの無い接続を動かさない」: `ingestAllowed: false` は
    /// 起動せず、動いていれば止める。DB 接続の無効・クエリなしも同じ。
    #[test]
    fn sources_without_the_scope_are_never_started() {
        let mut no_scope = source(1, "a");
        no_scope.ingest_allowed = false;
        let mut disabled = source(2, "b");
        disabled.db.enabled = false;
        let mut empty = source(3, "c");
        empty.queries[0].columns.clear();
        let plan = plan_sources(&HashMap::new(), &[no_scope.clone(), disabled, empty]);
        assert!(plan.start.is_empty());
        assert_eq!(
            plan.parked
                .iter()
                .map(|(id, _, reason)| (*id, *reason))
                .collect::<Vec<_>>(),
            vec![
                (1, Parked::NoPermission),
                (2, Parked::DbDisabled),
                (3, Parked::NothingToQuery)
            ]
        );
        // 動いていた source のスコープが消えた（キーを差し替えた）ら止める。
        let plan = plan_sources(&running(&[source(1, "a")]), &[no_scope]);
        assert_eq!(plan.stop, vec![1]);
        assert!(plan.start.is_empty());
        assert_eq!(parked_reason(&source(1, "a")), None);
    }

    /// future が落ちたら数える（取り消しが届いたかの印）。
    struct DropCount(Arc<std::sync::atomic::AtomicUsize>);

    impl Drop for DropCount {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// PR #764 の Codex レビュー P2: 予算内に止まらないブリッジ（監督が、合図を無視して
    /// 終わらない子のタスクを待ち続ける）は、handle を手放さずに全部 `abort` し、全部の future
    /// が落ちてから戻る。全体は予算を超えない。`abort` の後に起こされたタスクもすぐ落ちる。
    #[tokio::test]
    async fn a_bridge_that_misses_the_budget_is_aborted_and_joined_before_returning() {
        let tasks = BridgeTasks::default();
        let dropped = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let children = tasks.clone();
        let flag = dropped.clone();
        let supervisor = tasks.spawn(async move {
            let _parent = DropCount(flag.clone());
            let child = children.spawn(async move {
                let _child = DropCount(flag);
                std::future::pending::<()>().await;
            });
            let _ = child.await;
        });
        tokio::task::yield_now().await;
        let limit = Duration::from_millis(400);
        let started = Instant::now();
        let stop = stop_or_abort(supervisor, &tasks, limit).await;
        let elapsed = started.elapsed();
        assert_eq!(stop, BridgeStop::Aborted);
        assert_eq!(
            dropped.load(Ordering::SeqCst),
            2,
            "every future must be dropped before shutdown returns"
        );
        assert_eq!(tasks.live(), 0);
        assert!(elapsed < limit + Duration::from_millis(200), "{elapsed:?}");

        // `abort` の後に起こされたタスクもすぐ落ちる。
        let late = tasks.spawn(std::future::pending::<()>());
        tokio::time::timeout(Duration::from_secs(1), tasks.wait_idle())
            .await
            .expect("a task spawned after abort_all is aborted too");
        assert!(late.await.unwrap_err().is_cancelled());
    }

    /// 予算内に全部が終われば `Stopped`（`abort` しない）。
    #[tokio::test]
    async fn a_bridge_that_stops_in_time_is_not_aborted() {
        let tasks = BridgeTasks::default();
        let children = tasks.clone();
        let supervisor = tasks.spawn(async move {
            let child = children.spawn(tokio::time::sleep(Duration::from_millis(50)));
            child.await.unwrap();
        });
        let stop = stop_or_abort(supervisor, &tasks, Duration::from_secs(5)).await;
        assert_eq!(stop, BridgeStop::Stopped);
        assert_eq!(tasks.live(), 0);
    }
}
