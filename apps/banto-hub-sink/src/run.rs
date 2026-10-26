//! サイドカーの実行ループ（設計 §5.1・§5.2・§5.6）。
//!
//! `main.rs` ではなくこの lib 側に置いてあるのは、統合テスト
//! （`tests/sidecar.rs`）が**プロセス内**の Hub に対してこのループを
//! そのまま回せるようにするため。
//!
//! ## 1 ループでやること
//!
//! 3 つの締め切り（設定取得・状態 push・購読の生存確認）のうち最も早い
//! ものまで眠り、来たものだけを処理する。停止シグナル（コンソールの
//! Ctrl-C / SCM の STOP / OS の PRESHUTDOWN）は、眠りの間だけでなく
//! その処理の `await`（設定取得・状態 push・購読の確認と、その中の
//! catalog 取得）の途中でも抜ける。応答しない相手に捕まって
//! PRESHUTDOWN の作業予算を使い切らないため。
//!
//! - **設定取得**（既定 30 秒）: `GET /api/sink/config` を取り、前回適用した
//!   集合との差分だけを start/stop する（`banto_hub_core::db_source::
//!   DbSourceEngine::commit` と同じ考え方 - 変わっていないグループの
//!   キューと接続はそのまま残す）。失敗中は取得間隔を待たず指数
//!   バックオフで再試行する（**Hub 未起動のまま起動しても待つだけで
//!   終了しない** - 設計 §5.6）。
//! - **状態 push**（既定 5 秒）: 全グループの `state`/`queued`/`dropped`/
//!   `lastFlushAt`/`lastError` を `PUT /api/sink/status` で送る。Hub は
//!   これを全量スナップショットとして置き換える。
//! - **購読の生存確認**（1 秒）: SDK の worker が終端エラー
//!   （`Unauthorized` 等）で終わっていたら作り直す。通常の切断・再接続は
//!   SDK 自身のバックオフが面倒を見るので、ここでは何もしない。
//!
//! ## 差分適用の順序
//!
//! `グループ停止 → 接続停止 → 接続開始 → グループ開始`。プロデューサは
//! 起床先（その接続の flusher）を持つので、接続を先に用意する必要がある。
//!
//! ## 停止（§5.6）
//!
//! `プロデューサ停止 → 購読停止 → 残キューを最大 shutdown_flush_secs
//! だけ flush → flusher 停止 → 残りを dropped に計上 → 最後の状態 push`。
//! 最後の push まで済ませてから終了コード 0 で抜けるので、Hub 側には
//! 「取りこぼした行数」まで残る。
//!
//! DB ブリッジ（[`crate::source`]、R2a-3b）は同じ停止の合図で並行して止める: 各 source が
//! close 1000 を送り、問い合わせのタスクの終わりを待つ。待つのは
//! [`BRIDGE_STOP_CAP`]（OS シャットダウンでは残り時間との短い方）まで。ブリッジには
//! flush は無い（送れなかった周期は捨てる、docs/banto-hub-ingest-design.md §10.3「停止」）。
//! 上限までに止まらなければ、ブリッジは自分のタスクを全部 `abort` して落ちるのを待ってから
//! 戻る（handle を切り離したまま runtime の終わりに任せない。その待ちも上限の内、
//! [`crate::source::BridgeSupervisor::shutdown`]）。
//!
//! OS の PRESHUTDOWN（#589）では、この列の合計が SCM の待ち時間を超えうる
//! （flush 秒数は全体の上限ではない。flusher ごとに最大 2 秒、状態 push は
//! HTTP の上限 10 秒）。そのときは残り時間で各段階を頭打ちにし、
//! 完了と打ち切りを終了前のログで区別する。通常の STOP / Ctrl-C は
//! 設定の flush のまま打ち切らない。

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant as StdInstant};

use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::config::SidecarConfig;
use crate::flush::{
    backoff_delay, build_pool, spawn_flusher, FlushOptions, FlusherHandle, FlusherStopOutcome,
};
use crate::group::{
    run_interval_producer, run_on_change_producer, GroupRegistry, GroupRuntime, GroupSetupError,
    SinkMode, DEFAULT_TABLE_RECHECK,
};
use crate::hub_api::{
    HubApi, HubApiError, SinkConfig, SinkConfigConnection, SinkConfigGroup, SinkStatusPush,
    UnavailableConnection,
};
use crate::log::{log_err_line, log_line};
use crate::source::{BridgeStop, BridgeSupervisor};
use crate::status::{push_entry, GroupState, Redactor};
use crate::values::{normalize_names, plan_subscription, BindingPlan, Subscription, ValueView};

/// 購読の生存確認の間隔。
const HEALTH_TICK: Duration = Duration::from_secs(1);

/// unresolved なタグが残っている間の購読の再計画間隔（設定取得の既定と
/// 同じ 30 秒相当）。
const DEFAULT_REPLAN_INTERVAL: Duration = Duration::from_secs(30);

/// 停止時に flusher の join とプールの close を合わせた上限（これを超えたら
/// abort し、閉じていないプールは待たずに捨てる。commit 前なので二重
/// 書き込みにはならない。応答しない DB の `pool.close()` もこの中）。
const FLUSHER_STOP_TIMEOUT: Duration = Duration::from_secs(2);

/// 停止時の drain 監視のポーリング間隔。
const DRAIN_POLL: Duration = Duration::from_millis(50);

/// OS シャットダウンで購読の停止に使ってよい上限。通常停止では掛けない
/// （購読の停止が延びても、これまでどおり終わるまで待つ）。
const OS_SUBSCRIPTION_STOP_CAP: Duration = Duration::from_secs(2);

/// 停止で DB ブリッジ（[`crate::source`]）の終わりを待つ上限（close 1000 と問い合わせの
/// タスクの終わり）。Sink の flush と並行して待つ。
const BRIDGE_STOP_CAP: Duration = Duration::from_secs(3);

/// OS シャットダウンで最後の状態 push に使ってよい上限。
/// HTTP クライアントの上限は 10 秒なので、flush のあとに予算を使い切らない
/// よう短くする。通常停止ではクライアントの上限のまま待つ。
const OS_STATUS_PUSH_CAP: Duration = Duration::from_secs(5);

/// [`run`] に渡す停止合図の出力。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopRequest {
    /// Ctrl-C と SCM の STOP。設定の `shutdown_flush` と flusher ごとの
    /// 停止待ちをそのまま使う。全体の打ち切りはしない。
    Graceful,
    /// OS の PRESHUTDOWN。`deadline` を過ぎたら残りを打ち切り、
    /// 書けなかった行数をログに残す。
    OsShutdown {
        /// この時刻を過ぎたら打ち切る（SCM の待ち時間より前）。
        deadline: StdInstant,
    },
}

/// この段階に使ってよい時間。`remaining` が `None` のときは通常停止で、
/// 設定値をそのまま返す。OS シャットダウンでは残り時間で頭を抑える。
fn phase_limit(remaining: Option<Duration>, configured: Duration) -> Duration {
    match remaining {
        None => configured,
        Some(left) => configured.min(left),
    }
}

/// OS シャットダウンの完了と打ち切りを区別するログ行。
fn os_shutdown_log_line(completed: bool, unsent_rows: usize) -> String {
    if completed {
        format!(
            "banto-hub-sink: OS シャットダウンの停止が完了しました（送信できなかった行: {unsent_rows}）"
        )
    } else {
        format!(
            "banto-hub-sink: warn: OS シャットダウンの予算で停止を打ち切りました（送信できなかった行: {unsent_rows}）"
        )
    }
}

/// 実行時の調整値。**設定ファイルには出さない**（運用者が触る値ではなく、
/// テストが実時間を待たないための注入口）。
#[derive(Clone, Copy, Debug)]
pub struct SidecarOptions {
    /// テーブル検査の再試行間隔（設計 §5.3 の 30 秒）。
    pub table_recheck: Duration,
    /// 購読の生存確認の間隔。
    pub health_tick: Duration,
    /// unresolved なタグが残っている間、catalog を引き直して購読の計画を
    /// 作り直す間隔（同名で再作成されたタグ・rename 後に設定へ入った新名を
    /// 拾う）。
    pub replan_interval: Duration,
}

impl Default for SidecarOptions {
    fn default() -> Self {
        Self {
            table_recheck: DEFAULT_TABLE_RECHECK,
            health_tick: HEALTH_TICK,
            replan_interval: DEFAULT_REPLAN_INTERVAL,
        }
    }
}

/// 実行できないグループ（設定が壊れている / 参照先の接続が無い）。
/// 実行時オブジェクトは作れないが、**Hub には `error` として見せ続ける**
/// 必要があるのでここに残す（設計 §5.2「グループ単位で state を push」）。
#[derive(Clone, Debug)]
struct BrokenGroup {
    message: String,
    /// 壊れる前の世代が捨てた行数（累計を 0 に戻さないため）。
    dropped: u64,
}

/// 設定の差分（純粋関数 [`plan_changes`] の結果）。
#[derive(Debug, Default, PartialEq)]
pub struct ConfigPlan {
    pub groups_to_stop: Vec<i64>,
    pub connections_to_stop: Vec<i64>,
    pub connections_to_start: Vec<SinkConfigConnection>,
    pub groups_to_start: Vec<SinkConfigGroup>,
}

impl ConfigPlan {
    pub fn is_empty(&self) -> bool {
        self.groups_to_stop.is_empty()
            && self.connections_to_stop.is_empty()
            && self.connections_to_start.is_empty()
            && self.groups_to_start.is_empty()
    }
}

/// 「今動いている集合」と「Hub が返した集合」から、start/stop すべき
/// ものだけを取り出す。**内容が 1 バイトも変わっていないものは触らない**
/// のがこの関数の全て（触ればキューが失われ、接続が張り直される）。
pub fn plan_changes(
    applied_groups: &HashMap<i64, SinkConfigGroup>,
    applied_connections: &HashMap<i64, SinkConfigConnection>,
    desired: &SinkConfig,
) -> ConfigPlan {
    let desired_groups: HashMap<i64, &SinkConfigGroup> =
        desired.groups.iter().map(|g| (g.id, g)).collect();
    let desired_connections: HashMap<i64, &SinkConfigConnection> =
        desired.connections.iter().map(|c| (c.id, c)).collect();

    let mut plan = ConfigPlan::default();

    for (id, applied) in applied_groups {
        match desired_groups.get(id) {
            Some(desired) if *desired == applied => {}
            _ => plan.groups_to_stop.push(*id),
        }
    }
    for (id, applied) in applied_connections {
        match desired_connections.get(id) {
            Some(desired) if *desired == applied => {}
            _ => plan.connections_to_stop.push(*id),
        }
    }
    for connection in &desired.connections {
        if applied_connections.get(&connection.id) != Some(connection) {
            plan.connections_to_start.push(connection.clone());
        }
    }
    for group in &desired.groups {
        if applied_groups.get(&group.id) != Some(group) {
            plan.groups_to_start.push(group.clone());
        }
    }
    // R2b-1d（docs/banto-hub-connection-settings-design.md §5.4）: グループの中身は同じでも、
    // 引く接続が設定から**消えた**（削除・Hub がパスワードを読めなくなった
    // `unavailableConnections`）か、**現れた**（入れ直して読めるようになった）なら、その
    // グループも作り直す。消えた接続の flusher は止めるので、そのまま走らせると誰も書かない
    // キューに溜め続け、状態も古いまま。現れた接続では、壊れたままの（`MissingConnection` /
    // 読めない）グループを動かす。接続の中身が変わっただけ（同じ id が残る）では作り直さない
    // （キューを失わない）。
    let appeared: HashSet<i64> = desired
        .connections
        .iter()
        .map(|c| c.id)
        .filter(|id| !applied_connections.contains_key(id))
        .collect();
    let vanished: HashSet<i64> = applied_connections
        .keys()
        .copied()
        .filter(|id| !desired_connections.contains_key(id))
        .collect();
    for group in &desired.groups {
        let unchanged = applied_groups.get(&group.id) == Some(group);
        if unchanged
            && (appeared.contains(&group.db_connection_id)
                || vanished.contains(&group.db_connection_id))
        {
            plan.groups_to_stop.push(group.id);
            plan.groups_to_start.push(group.clone());
        }
    }

    plan.groups_to_stop.sort_unstable();
    plan.connections_to_stop.sort_unstable();
    plan.connections_to_start.sort_by_key(|c| c.id);
    plan.groups_to_start.sort_by_key(|g| g.id);
    plan
}

/// サイドカー本体。[`run`] だけが構築する。
struct Engine {
    config: SidecarConfig,
    options: SidecarOptions,
    hub: HubApi,
    groups: Arc<GroupRegistry>,
    producers: HashMap<i64, JoinHandle<()>>,
    flushers: HashMap<i64, FlusherHandle>,
    broken: HashMap<i64, BrokenGroup>,
    applied_groups: HashMap<i64, SinkConfigGroup>,
    applied_connections: HashMap<i64, SinkConfigConnection>,
    subscription: Option<Subscription>,
    view_tx: watch::Sender<Arc<ValueView>>,
    view_rx: watch::Receiver<Arc<ValueView>>,
    redactor: Redactor,
    /// 設定取得の連続失敗回数（バックオフ用）。
    config_attempt: u32,
    /// 直前に `warn` した設定取得エラー（同じ理由を毎回出さない - §5.5）。
    logged_config_error: Option<HubApiError>,
    /// 状態 push が失敗中か（復帰時に `info` を 1 行出すため）。
    status_failing: bool,
    /// 購読の作り直し回数（バックオフ用）。
    subscription_attempt: u32,
    /// 次に購読を作り直してよい時刻。
    subscription_retry_at: Option<Instant>,
    /// unresolved が残っている購読を、次に再計画してよい時刻。
    next_replan_at: Instant,
}

impl Engine {
    fn new(config: SidecarConfig, options: SidecarOptions) -> Result<Self, HubApiError> {
        let hub = HubApi::new(&config)?;
        let (view_tx, view_rx) = watch::channel(Arc::new(ValueView::default()));
        let mut redactor = Redactor::new();
        redactor.add_secret(&config.api_key);
        Ok(Self {
            config,
            options,
            hub,
            groups: Arc::new(GroupRegistry::new()),
            producers: HashMap::new(),
            flushers: HashMap::new(),
            broken: HashMap::new(),
            applied_groups: HashMap::new(),
            applied_connections: HashMap::new(),
            subscription: None,
            view_tx,
            view_rx,
            redactor,
            config_attempt: 0,
            logged_config_error: None,
            status_failing: false,
            subscription_attempt: 0,
            subscription_retry_at: None,
            next_replan_at: Instant::now(),
        })
    }

    fn flush_options(&self) -> FlushOptions {
        FlushOptions {
            flush_interval: self.config.flush_interval,
            batch_size: self.config.batch_size,
            table_recheck: self.options.table_recheck,
        }
    }

    // --- 設定の取得と適用 ---------------------------------------------------

    /// `GET /api/sink/config` を取り、差分を適用する。成功したら true。
    async fn refresh_config(&mut self) -> bool {
        match self.hub.fetch_config().await {
            Ok(config) => {
                if self.logged_config_error.take().is_some() {
                    log_line("banto-hub-sink: Hub から設定を取得できるようになりました");
                }
                self.config_attempt = 0;
                self.apply_config(config).await;
                true
            }
            Err(err) => {
                self.config_attempt = self.config_attempt.saturating_add(1);
                if self.logged_config_error.as_ref() != Some(&err) {
                    log_err_line(&format!(
                        "banto-hub-sink: warn: 設定を取得できません（再試行します）: {err}"
                    ));
                    self.logged_config_error = Some(err);
                }
                false
            }
        }
    }

    async fn apply_config(&mut self, desired: SinkConfig) {
        let plan = plan_changes(&self.applied_groups, &self.applied_connections, &desired);

        // 接続のパスワードは秘密なので、伏せ字の対象へ足しておく
        // （sqlx のエラーに混じる可能性への多層防御 - `crate::status`）。
        for connection in &desired.connections {
            if let Some(password) = connection.password.as_deref() {
                self.redactor.add_secret(password);
            }
        }

        if !plan.is_empty() {
            log_line(&format!(
                "banto-hub-sink: 設定を反映します（グループ: 停止 {} / 開始 {}、接続: 停止 {} / 開始 {}）",
                plan.groups_to_stop.len(),
                plan.groups_to_start.len(),
                plan.connections_to_stop.len(),
                plan.connections_to_start.len()
            ));
        }

        // 1. 変わった/消えたグループを止める（捨てた行数は引き継ぐ）。
        let mut carried_dropped: HashMap<i64, u64> = HashMap::new();
        for id in &plan.groups_to_stop {
            if let Some(task) = self.producers.remove(id) {
                task.abort();
            }
            if let Some(runtime) = self.groups.remove(*id) {
                let queued = runtime.queued();
                let dropped = runtime.dropped() + queued as u64;
                if queued > 0 {
                    log_err_line(&format!(
                        "banto-hub-sink: warn: グループ id={id} の設定が変わったため、未送信の {queued} 行を破棄しました"
                    ));
                }
                carried_dropped.insert(*id, dropped);
            }
            if let Some(broken) = self.broken.remove(id) {
                carried_dropped.insert(*id, broken.dropped);
            }
        }

        // 2. 変わった/消えた接続の flusher を止める。
        for id in &plan.connections_to_stop {
            if let Some(flusher) = self.flushers.remove(id) {
                // 設定反映の停止は打ち切りでも続ける（ログは stop が出す）。
                let _ = flusher.stop(FLUSHER_STOP_TIMEOUT).await;
            }
        }

        // 3. 接続を先に用意する（プロデューサが flusher の起床口を持つため）。
        let connections: HashMap<i64, SinkConfigConnection> = desired
            .connections
            .iter()
            .map(|c| (c.id, c.clone()))
            .collect();
        // R2b-1d（設計 §5.4）: Hub がパスワードを読めない接続（`connections` には無い）。
        // これを引くグループは繋がずに理由を出す（[`Self::start_group`]）。
        let unavailable: HashMap<i64, UnavailableConnection> = desired
            .unavailable_connections
            .iter()
            .map(|c| (c.id, c.clone()))
            .collect();
        for connection in &plan.connections_to_start {
            match build_pool(connection) {
                Ok(pool) => {
                    let handle = spawn_flusher(
                        connection.clone(),
                        pool,
                        self.groups.clone(),
                        self.flush_options(),
                        self.redactor.clone(),
                    );
                    self.flushers.insert(connection.id, handle);
                }
                Err(message) => {
                    log_err_line(&format!(
                        "banto-hub-sink: warn: DB 接続 {} (id={}) を構成できません: {message}",
                        connection.name, connection.id
                    ));
                }
            }
        }

        // 4. グループを開始する。
        for group in &plan.groups_to_start {
            let carried = carried_dropped.get(&group.id).copied().unwrap_or(0);
            self.start_group(group, &connections, &unavailable, carried);
        }

        self.applied_groups = desired.groups.iter().map(|g| (g.id, g.clone())).collect();
        self.applied_connections = connections;

        self.ensure_subscription().await;
    }

    fn start_group(
        &mut self,
        group: &SinkConfigGroup,
        connections: &HashMap<i64, SinkConfigConnection>,
        unavailable: &HashMap<i64, UnavailableConnection>,
        carried_dropped: u64,
    ) {
        if let Some(connection) = unavailable.get(&group.db_connection_id) {
            self.mark_broken(
                group.id,
                GroupSetupError::ConnectionUnavailable {
                    id: connection.id,
                    name: connection.name.clone(),
                    reason: connection.reason.clone(),
                }
                .message(),
                carried_dropped,
            );
            return;
        }
        if !connections.contains_key(&group.db_connection_id) {
            self.mark_broken(
                group.id,
                GroupSetupError::MissingConnection(group.db_connection_id).message(),
                carried_dropped,
            );
            return;
        }
        let Some(flusher) = self.flushers.get(&group.db_connection_id) else {
            // 接続の構成に失敗している（ポート範囲外など）。
            self.mark_broken(
                group.id,
                GroupSetupError::MissingConnection(group.db_connection_id).message(),
                carried_dropped,
            );
            return;
        };
        let runtime = match GroupRuntime::new(group, self.config.queue_max_rows, carried_dropped) {
            Ok(runtime) => Arc::new(runtime),
            Err(err) => {
                self.mark_broken(group.id, err.message(), carried_dropped);
                return;
            }
        };
        let waker = flusher.waker();
        let batch_size = self.config.batch_size;
        let rx = self.view_rx.clone();
        let task = match runtime.mode {
            SinkMode::Interval => tokio::spawn(run_interval_producer(
                runtime.clone(),
                rx,
                waker,
                batch_size,
            )),
            SinkMode::OnChange => tokio::spawn(run_on_change_producer(
                runtime.clone(),
                rx,
                waker,
                batch_size,
            )),
        };
        self.broken.remove(&group.id);
        self.producers.insert(group.id, task);
        self.groups.insert(runtime);
        log_line(&format!(
            "banto-hub-sink: グループ {} (id={}) を開始しました（{} / {}ms / {} タグ → {}）",
            group.name,
            group.id,
            group.mode,
            group.interval_ms,
            group.tags.len(),
            group.table_name
        ));
    }

    fn mark_broken(&mut self, id: i64, message: String, dropped: u64) {
        log_err_line(&format!(
            "banto-hub-sink: warn: グループ id={id} を実行できません: {message}"
        ));
        self.broken.insert(id, BrokenGroup { message, dropped });
    }

    // --- 購読 ---------------------------------------------------------------

    /// 対象タグの和集合を購読し続ける（必要なら作り直す）。
    ///
    /// 作り直す条件は 3 つ:
    ///
    /// 1. `dead` - SDK の worker が終了した（バックオフあり）。
    /// 2. `changed` / `needs_replan` - 購読したい名前の集合が変わった（設定
    ///    変更）、または SDK が `BindingUnresolved` を報告した（購読中の
    ///    rename 等）。SDK は 1 件でも unresolved だと世代全体を落とすので、
    ///    Sink が catalog を引いて解決できる名前だけで**直ちに**張り直す
    ///    （[`crate::values::plan_bindings`]。バックオフは掛けない）。
    /// 3. 周期的 replan - 上のどれでもなく、unresolved が残っていて再計画の
    ///    時刻（`replan_interval`）を過ぎた。同名再作成や新名の復帰を拾うための
    ///    ものだが、**現在の購読を止める前に** catalog を取って計画だけを作り
    ///    （[`plan_subscription`]）、計画が前回と同じなら `next_replan_at` を
    ///    進めるだけで購読を維持する（正常タグの記録を 30 秒ごとに途切れ
    ///    させない）。catalog を取得できなかったときも購読を維持して次の
    ///    周期で再試行する（一時的な失敗で正常だった購読を失わない）。計画が
    ///    変わったときだけ、取得済みの計画で張り直す（catalog を二度取らない）。
    async fn ensure_subscription(&mut self) {
        let desired = self.desired_external_names();
        // 即時の張り直し（`needs_replan`）のときの、前世代の計画（ログの
        // 重複抑止用）。周期的 replan は計画が変わったときしか張り直さない
        // ので、常にログを出せばよく、これは使わない。
        let mut previous_plan: Option<BindingPlan> = None;
        // 周期的 replan が先に作った新しい計画（`Some` なら catalog は取得済み）。
        let mut fresh_plan: Option<BindingPlan> = None;

        if let Some(subscription) = &self.subscription {
            let dead = subscription.is_dead();
            let changed = subscription.desired() != desired.as_slice();
            let needs_replan = subscription.needs_replan();
            let periodic_due = !dead
                && !changed
                && !needs_replan
                && !subscription.unresolved().is_empty()
                && Instant::now() >= self.next_replan_at;
            if !dead && !changed && !needs_replan && !periodic_due {
                if self.view_rx.borrow().live {
                    // 一度でも Live になったら、次の障害は 1 秒から
                    // やり直す（`banto_tagclient` の RetryTracker と同じ
                    // 考え方）。
                    self.subscription_attempt = 0;
                }
                return;
            }
            if periodic_due {
                // 購読を止める前に計画だけを作る。
                let current = subscription.plan();
                let fresh = plan_subscription(&self.config, &desired).await;
                let error_kind = fresh
                    .as_ref()
                    .err()
                    .map(|err| err.kind().as_str().to_owned());
                match replan_decision(&current, fresh) {
                    ReplanDecision::Keep => {
                        self.next_replan_at = Instant::now() + self.options.replan_interval;
                        return;
                    }
                    ReplanDecision::KeepAfterError => {
                        self.next_replan_at = Instant::now() + self.options.replan_interval;
                        // 30 秒ごとなので rate limit はしない。
                        log_err_line(&format!(
                            "banto-hub-sink: warn: 未解決タグの再計画で catalog を取得できません（購読は維持します）: {}",
                            error_kind.unwrap_or_default()
                        ));
                        return;
                    }
                    ReplanDecision::Restart(plan) => fresh_plan = Some(plan),
                }
            } else if !dead && !changed {
                previous_plan = Some(subscription.plan());
            }
            if let Some(subscription) = self.subscription.take() {
                subscription.stop().await;
            }
            let _ = self.view_tx.send(Arc::new(ValueView::default()));
            if dead {
                self.subscription_attempt = self.subscription_attempt.saturating_add(1);
                let delay = backoff_delay(self.subscription_attempt);
                self.subscription_retry_at = Some(Instant::now() + delay);
                log_err_line(&format!(
                    "banto-hub-sink: warn: Hub の購読が終了しました（{:.0} 秒後に張り直します。API キーのスコープに read が含まれているか確認してください）",
                    delay.as_secs_f64()
                ));
                return;
            }
        }

        if desired.is_empty() {
            return;
        }
        if let Some(retry_at) = self.subscription_retry_at {
            if Instant::now() < retry_at {
                return;
            }
        }
        self.subscription_retry_at = None;
        let started = match fresh_plan {
            Some(plan) => {
                Subscription::start_with_plan(&self.config, desired, plan, self.view_tx.clone())
            }
            None => Subscription::start(&self.config, desired, self.view_tx.clone()).await,
        };
        match started {
            Ok(subscription) => {
                self.next_replan_at = Instant::now() + self.options.replan_interval;
                self.subscription = subscription;
                if let Some(subscription) = &self.subscription {
                    let plan = subscription.plan();
                    // 即時の張り直しで結果が変わらなかったときは黙る
                    // （SDK が落とす世代を繰り返し張り直す間、同じログが
                    // 出続けない）。
                    if previous_plan.as_ref() != Some(&plan) {
                        log_line(&subscribed_message(
                            subscription.resolved().len(),
                            &plan.unresolved,
                        ));
                    }
                }
            }
            Err(err) => {
                self.subscription_attempt = self.subscription_attempt.saturating_add(1);
                self.subscription_retry_at =
                    Some(Instant::now() + backoff_delay(self.subscription_attempt));
                log_err_line(&format!(
                    "banto-hub-sink: warn: Hub の購読を開始できません: {}",
                    err.kind().as_str()
                ));
            }
        }
    }

    /// 全 sink group の対象タグの和集合（`external_name`、重複除去・ソート済み）。
    /// 購読の鍵は名前（2026-09-30 オーナー決定、docs/scada-design.md §9.6）で、
    /// 安定 ID の 3 つ組は DB 行用に `SinkConfigTag` へ残っているだけ。
    fn desired_external_names(&self) -> Vec<String> {
        let mut names = Vec::new();
        for group in self.applied_groups.values() {
            for tag in &group.tags {
                names.push(tag.external_name.clone());
            }
        }
        normalize_names(names)
    }

    // --- 状態 push ----------------------------------------------------------

    fn status_body(&self) -> SinkStatusPush {
        let mut groups = Vec::new();
        for runtime in self.groups.all() {
            groups.push(push_entry(
                runtime.id,
                runtime.state(),
                runtime.queued(),
                runtime.dropped(),
                runtime.last_flush_at(),
                runtime.last_error().map(|err| self.redactor.apply(&err)),
            ));
        }
        for (id, broken) in &self.broken {
            groups.push(push_entry(
                *id,
                GroupState::Error,
                0,
                broken.dropped,
                None,
                Some(self.redactor.apply(&broken.message)),
            ));
        }
        groups.sort_by_key(|entry| entry.id);
        SinkStatusPush { groups }
    }

    async fn push_status(&mut self) {
        let body = self.status_body();
        match self.hub.push_status(&body).await {
            Ok(()) => {
                if self.status_failing {
                    self.status_failing = false;
                    log_line("banto-hub-sink: Hub への状態 push が復帰しました");
                }
            }
            Err(err) => {
                if !self.status_failing {
                    self.status_failing = true;
                    log_err_line(&format!(
                        "banto-hub-sink: warn: Hub への状態 push に失敗しました（再試行します）: {err}"
                    ));
                }
                // 422 = Hub の知らないグループ id を送った（グループが
                // 削除された直後）。設定を取り直せば直るので、次の
                // 取得を待たずに即時取得する。
                if matches!(err, HubApiError::Status(422)) {
                    self.refresh_config().await;
                }
            }
        }
    }

    // --- 停止（設計 §5.6） --------------------------------------------------

    async fn shutdown(mut self, request: StopRequest) {
        log_line("banto-hub-sink: 停止します（残りのキューを flush します）");
        let os_deadline = match request {
            StopRequest::Graceful => None,
            StopRequest::OsShutdown { deadline } => Some(deadline),
        };
        let mut cutoff = false;
        // 1. 行の生産を止める。
        for (_, task) in self.producers.drain() {
            task.abort();
        }
        // 2. 購読を止める（Hub 側の購読を残さない）。通常停止は終わるまで
        // 待つ。OS シャットダウンだけ残り時間で頭を抑える。
        if let Some(subscription) = self.subscription.take() {
            match os_deadline {
                None => subscription.stop().await,
                Some(deadline) => {
                    let limit = phase_limit(
                        Some(deadline.saturating_duration_since(StdInstant::now())),
                        OS_SUBSCRIPTION_STOP_CAP,
                    );
                    if limit.is_zero()
                        || tokio::time::timeout(limit, subscription.stop())
                            .await
                            .is_err()
                    {
                        cutoff = true;
                    }
                }
            }
        }
        // 3. 残キューを最大 shutdown_flush_secs だけ流す。OS シャットダウンでは
        // その上限と残り時間の短い方。
        for flusher in self.flushers.values() {
            flusher.request_drain();
        }
        let flush_end = {
            let configured = StdInstant::now() + self.config.shutdown_flush;
            match os_deadline {
                Some(deadline) => configured.min(deadline),
                None => configured,
            }
        };
        while StdInstant::now() < flush_end {
            if self.groups.all().iter().all(|group| group.queued() == 0) {
                break;
            }
            tokio::time::sleep(DRAIN_POLL).await;
        }
        if os_deadline.is_some_and(|deadline| StdInstant::now() >= deadline)
            && self.groups.all().iter().any(|group| group.queued() > 0)
        {
            cutoff = true;
        }
        // 4. flusher を止める。通常停止は各 2 秒。OS シャットダウンは残り時間。
        let flushers: Vec<FlusherHandle> = self.flushers.drain().map(|(_, f)| f).collect();
        for flusher in flushers {
            let limit = match os_deadline {
                None => FLUSHER_STOP_TIMEOUT,
                Some(deadline) => phase_limit(
                    Some(deadline.saturating_duration_since(StdInstant::now())),
                    FLUSHER_STOP_TIMEOUT,
                ),
            };
            // 期限切れで abort した・プールを閉じ切れなかったときも打ち切り
            // （「完了しました」と出さない）。通常停止はログに出さないので
            // 結果は使わない。
            if flusher.stop(limit).await == FlusherStopOutcome::CutShort && os_deadline.is_some() {
                cutoff = true;
            }
        }
        // 5. 流しきれなかった行は dropped に計上する。
        let mut lost = 0usize;
        for group in self.groups.all() {
            lost += group.discard_queue();
        }
        if lost > 0 {
            log_err_line(&format!(
                "banto-hub-sink: warn: 停止時に {lost} 行を送信できませんでした（dropped に計上しました）"
            ));
        }
        // 6. 最後の状態 push（上の dropped を含んだ状態を Hub に残す）。
        // OS シャットダウンで予算が尽きたら push を待たずに打ち切る。
        match os_deadline {
            None => self.push_status().await,
            Some(deadline) => {
                let limit = phase_limit(
                    Some(deadline.saturating_duration_since(StdInstant::now())),
                    OS_STATUS_PUSH_CAP,
                );
                if limit.is_zero()
                    || tokio::time::timeout(limit, self.push_status())
                        .await
                        .is_err()
                {
                    cutoff = true;
                    log_err_line(
                        "banto-hub-sink: warn: 最後の状態 push を OS シャットダウンの予算内に終えられませんでした",
                    );
                }
            }
        }
        if os_deadline.is_some() {
            if cutoff {
                log_err_line(&os_shutdown_log_line(false, lost));
            } else {
                log_line(&os_shutdown_log_line(true, lost));
            }
        }
        log_line("banto-hub-sink: 停止しました");
        crate::log::sync_service_log();
    }
}

/// サイドカーを実行する。`shutdown` が完了したら停止処理へ入り、
/// 全部済んでから戻る。
///
/// 戻り値の `Err` は「そもそも起動できない」場合だけ（HTTP クライアントを
/// 構築できない等）。Hub が未起動・DB が停止しているといった運用上の
/// 状態は**エラーではない** - 待って再試行し続ける（設計 §5.6）。
pub async fn run<F>(
    config: SidecarConfig,
    options: SidecarOptions,
    shutdown: F,
) -> Result<(), HubApiError>
where
    F: Future<Output = StopRequest>,
{
    if !config.transport_is_valid() {
        return Err(HubApiError::Transport);
    }
    let status_push = config.status_push;
    let config_refresh = config.config_refresh;
    let health_tick = options.health_tick;

    log_line(&format!(
        "banto-hub-sink: 起動しました（Hub {} / 設定取得 {}秒 / 状態push {}秒 / キュー上限 {} 行 / batch {} 行 / flush {}ms）",
        config.hub_url,
        config_refresh.as_secs(),
        status_push.as_secs(),
        config.queue_max_rows,
        config.batch_size,
        config.flush_interval.as_millis()
    ));
    if !config.hub_is_loopback() {
        log_err_line(
            "banto-hub-sink: warn: Hub がループバック以外に設定されています。GET /api/sink/config は DB 接続のパスワードを平文で返すため、Hub と同一マシンでの運用を推奨します（docs/banto-hub-external-db-design.md §2.2・§5.2・§6-15）",
        );
    }

    // R2a-3b: DB ブリッジ。エンジンとは別のタスク群で、設定の取得も独立（`crate::source`）。
    let bridge = BridgeSupervisor::spawn(&config)?;
    let mut engine = Engine::new(config, options)?;
    tokio::pin!(shutdown);

    // 起動直後は「取れるまで待つ」だけ - 取れなくても終了しない。
    let ok = tokio::select! {
        request = &mut shutdown => { shutdown_all(engine, bridge, request).await; return Ok(()); }
        ok = engine.refresh_config() => ok,
    };
    let now = Instant::now();
    let mut next_config_at = now + next_config_delay(ok, config_refresh, engine.config_attempt);
    let mut next_status_at = now;
    let mut next_health_at = now + health_tick;

    let stop_request;
    loop {
        let wake_at = next_config_at.min(next_status_at).min(next_health_at);
        tokio::select! {
            biased;
            request = &mut shutdown => {
                stop_request = request;
                break;
            }
            _ = tokio::time::sleep_until(wake_at) => {}
        }
        let now = Instant::now();
        // 各作業は停止合図と一緒に待つ。future を捨てたときの資源は
        // `Engine::shutdown` か、その future の drop（flusher は abort、
        // 購読は `Subscription` の drop）が引き取る。
        if now >= next_config_at {
            tokio::select! {
                biased;
                request = &mut shutdown => {
                    stop_request = request;
                    break;
                }
                ok = engine.refresh_config() => {
                    next_config_at = Instant::now()
                        + next_config_delay(ok, config_refresh, engine.config_attempt);
                }
            }
        }
        if now >= next_status_at {
            tokio::select! {
                biased;
                request = &mut shutdown => {
                    stop_request = request;
                    break;
                }
                _ = engine.push_status() => {
                    next_status_at = Instant::now() + status_push;
                }
            }
        }
        if now >= next_health_at {
            tokio::select! {
                biased;
                request = &mut shutdown => {
                    stop_request = request;
                    break;
                }
                _ = engine.ensure_subscription() => {
                    next_health_at = Instant::now() + health_tick;
                }
            }
        }
    }

    // ループを抜けるのは停止合図を受け取ったときだけ。
    shutdown_all(engine, bridge, stop_request).await;
    Ok(())
}

/// エンジンの停止（flush・最後の状態 push）と DB ブリッジの停止を並行して行う。
async fn shutdown_all(engine: Engine, bridge: BridgeSupervisor, request: StopRequest) {
    let limit = match request {
        StopRequest::Graceful => BRIDGE_STOP_CAP,
        StopRequest::OsShutdown { deadline } => phase_limit(
            Some(deadline.saturating_duration_since(StdInstant::now())),
            BRIDGE_STOP_CAP,
        ),
    };
    let (bridge_stop, ()) = tokio::join!(bridge.shutdown(limit), engine.shutdown(request));
    if let Some(line) = bridge_stop_log_line(bridge_stop) {
        log_err_line(line);
    }
}

/// DB ブリッジの停止の結果のログ行（予算内に止まったら出さない）。
fn bridge_stop_log_line(stop: BridgeStop) -> Option<&'static str> {
    match stop {
        BridgeStop::Stopped => None,
        BridgeStop::Aborted => Some(
            "banto-hub-sink: warn: DB ブリッジが予算内に止まらなかったため、問い合わせを打ち切りました（送れなかった周期は捨てます）",
        ),
        BridgeStop::Detached => Some(
            "banto-hub-sink: warn: DB ブリッジが予算内に止まらず、打ち切りも終わりませんでした（残りはプロセスの終了で止まります。送れなかった周期は捨てます）",
        ),
    }
}

/// 周期的な再計画の判定結果（[`replan_decision`]）。
#[derive(Debug, PartialEq, Eq)]
enum ReplanDecision {
    /// 計画は前回と同じ。購読を維持して `next_replan_at` だけ進める。
    Keep,
    /// 計画が変わった。この計画で購読を張り直す（catalog は取得済み）。
    Restart(BindingPlan),
    /// catalog を取得できなかった。購読は維持して次の周期で再試行する。
    KeepAfterError,
}

/// 周期的な再計画で、現在の購読（`current`）と新しく作った計画（`fresh`）を
/// 比べ、購読を維持するか張り直すかを決める**純関数**。張り直しは購読中の
/// 正常タグの記録を途切れさせるので、計画が変わったときだけに限る。
fn replan_decision<E>(current: &BindingPlan, fresh: Result<BindingPlan, E>) -> ReplanDecision {
    match fresh {
        Ok(plan) if plan == *current => ReplanDecision::Keep,
        Ok(plan) => ReplanDecision::Restart(plan),
        Err(_) => ReplanDecision::KeepAfterError,
    }
}

/// 未解決の名前を何件までログへ列挙するか。
const UNRESOLVED_LOG_LIMIT: usize = 5;

/// 購読開始ログの本文。未解決が 0 なら「N タグ」だけ、あれば名前を
/// [`UNRESOLVED_LOG_LIMIT`] 件まで列挙し、超えた分は「ほか N 件」。
fn subscribed_message(resolved_len: usize, unresolved: &[String]) -> String {
    if unresolved.is_empty() {
        return format!("banto-hub-sink: Hub の購読を開始しました（{resolved_len} タグ）");
    }
    let mut names = unresolved
        .iter()
        .take(UNRESOLVED_LOG_LIMIT)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if unresolved.len() > UNRESOLVED_LOG_LIMIT {
        names.push_str(&format!(
            "、ほか {} 件",
            unresolved.len() - UNRESOLVED_LOG_LIMIT
        ));
    }
    format!(
        "banto-hub-sink: Hub の購読を開始しました（{resolved_len} タグ。未解決 {}: {names}）",
        unresolved.len()
    )
}

/// 次の設定取得までの間隔。成功したら既定の間隔、失敗中は指数バック
/// オフ（ただし既定の間隔は超えない - Hub が復帰したときに 30 秒以上
/// 待たされないため）。
fn next_config_delay(ok: bool, config_refresh: Duration, attempt: u32) -> Duration {
    if ok {
        config_refresh
    } else {
        backoff_delay(attempt).min(config_refresh)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn subscribed_message_lists_up_to_five_unresolved_names() {
        assert_eq!(
            subscribed_message(3, &[]),
            "banto-hub-sink: Hub の購読を開始しました（3 タグ）"
        );
        let names: Vec<String> = ["a", "b", "c"].iter().map(|n| (*n).to_owned()).collect();
        assert_eq!(
            subscribed_message(97, &names),
            "banto-hub-sink: Hub の購読を開始しました（97 タグ。未解決 3: a, b, c）"
        );
        let many: Vec<String> = (1..=7).map(|n| format!("t{n}")).collect();
        assert_eq!(
            subscribed_message(0, &many),
            "banto-hub-sink: Hub の購読を開始しました（0 タグ。未解決 7: t1, t2, t3, t4, t5、ほか 2 件）"
        );
    }

    use super::*;
    use crate::hub_api::SinkConfigTag;

    fn temporary_hub_tls() -> (
        tempfile::TempDir,
        banto_tagclient::PinnedHubTls,
        tokio_rustls::TlsAcceptor,
    ) {
        use std::process::Command;
        use tokio_rustls::rustls::pki_types::pem::PemObject;
        use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};

        let dir = tempfile::tempdir().unwrap();
        let cert = dir.path().join("hub.pem");
        let key = dir.path().join("hub.key");
        let output = Command::new("openssl")
            .args([
                "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1",
            ])
            .arg("-keyout")
            .arg(&key)
            .arg("-out")
            .arg(&cert)
            .args([
                "-subj",
                "/CN=localhost",
                "-addext",
                "subjectAltName=DNS:localhost,IP:127.0.0.1",
                "-addext",
                "basicConstraints=critical,CA:FALSE",
            ])
            .output()
            .unwrap();
        assert!(output.status.success());
        let pem = std::fs::read_to_string(&cert).unwrap();
        let certs = CertificateDer::pem_file_iter(&cert)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let key = PrivateKeyDer::from_pem_file(&key).unwrap();
        let server = tokio_rustls::rustls::ServerConfig::builder_with_provider(Arc::new(
            tokio_rustls::rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .unwrap();
        let pins = banto_tagclient::PinnedHubTls::from_pem(&[pem]).unwrap();
        (dir, pins, tokio_rustls::TlsAcceptor::from(Arc::new(server)))
    }

    #[tokio::test]
    async fn programmatic_loopback_http_is_rejected_before_starting() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut config = SidecarConfig::from_toml_str(
            "hub_url = 'http://192.0.2.1:8722'\napi_key = 'bh_dummy.secret'\n",
            std::path::Path::new("dummy.toml"),
        )
        .unwrap();
        for host in [
            "127.0.0.1",
            "127.99.4.2",
            "[::ffff:127.0.0.1]",
            "[::ffff:7f00:1]",
            "[::ffff:127.99.4.2]",
            "[::ffff:7f63:402]",
        ] {
            config.hub_url = format!("http://{host}:{port}");
            assert!(HubApi::new(&config).is_err(), "{host}");
            assert!(
                matches!(
                    run(config.clone(), SidecarOptions::default(), async {
                        panic!("started")
                    })
                    .await,
                    Err(HubApiError::Transport)
                ),
                "{host}"
            );
        }
        // A rejected endpoint must never connect to the plaintext listener or send its Bearer.
        assert!(
            tokio::time::timeout(Duration::from_millis(100), listener.accept())
                .await
                .is_err()
        );
        config.hub_url = "https://127.0.0.1:9443".into();
        assert!(HubApi::new(&config).is_err());
        assert!(matches!(
            run(config, SidecarOptions::default(), async {
                panic!("started")
            })
            .await,
            Err(HubApiError::Transport)
        ));
    }

    fn plan(resolved: &[&str], unresolved: &[&str]) -> BindingPlan {
        BindingPlan {
            resolved: resolved.iter().map(|n| (*n).to_owned()).collect(),
            unresolved: unresolved.iter().map(|n| (*n).to_owned()).collect(),
        }
    }

    #[test]
    fn replan_decision_keeps_the_subscription_when_the_plan_is_unchanged() {
        let current = plan(&["a"], &["gone"]);
        assert_eq!(
            replan_decision::<()>(&current, Ok(plan(&["a"], &["gone"]))),
            ReplanDecision::Keep
        );
    }

    #[test]
    fn replan_decision_restarts_with_the_fresh_plan_when_it_changed() {
        let current = plan(&["a"], &["gone"]);
        // 同名再作成で unresolved が resolved へ移った。
        assert_eq!(
            replan_decision::<()>(&current, Ok(plan(&["a", "gone"], &[]))),
            ReplanDecision::Restart(plan(&["a", "gone"], &[]))
        );
        // 新たに catalog から消えた名前がある。
        assert_eq!(
            replan_decision::<()>(&current, Ok(plan(&[], &["a", "gone"]))),
            ReplanDecision::Restart(plan(&[], &["a", "gone"]))
        );
    }

    #[test]
    fn replan_decision_keeps_the_subscription_when_the_catalog_fetch_failed() {
        let current = plan(&["a"], &["gone"]);
        assert_eq!(
            replan_decision(&current, Err("timeout")),
            ReplanDecision::KeepAfterError
        );
    }

    fn connection(id: i64, password: &str) -> SinkConfigConnection {
        SinkConfigConnection {
            id,
            name: format!("db{id}"),
            host: "127.0.0.1".to_string(),
            port: 5432,
            database: Some("erp".to_string()),
            username: Some("app".to_string()),
            password: Some(password.to_string()),
        }
    }

    fn group(id: i64, connection_id: i64, table: &str) -> SinkConfigGroup {
        SinkConfigGroup {
            id,
            name: format!("g{id}"),
            db_connection_id: connection_id,
            mode: "interval".to_string(),
            interval_ms: 1_000,
            table_name: table.to_string(),
            store_bad: false,
            tags: vec![SinkConfigTag {
                connection_id: 1,
                group_id: 2,
                tag_id: id,
                external_name: format!("line1.fast.t{id}"),
            }],
        }
    }

    fn applied(
        groups: &[SinkConfigGroup],
        connections: &[SinkConfigConnection],
    ) -> (
        HashMap<i64, SinkConfigGroup>,
        HashMap<i64, SinkConfigConnection>,
    ) {
        (
            groups.iter().map(|g| (g.id, g.clone())).collect(),
            connections.iter().map(|c| (c.id, c.clone())).collect(),
        )
    }

    fn desired(groups: Vec<SinkConfigGroup>, connections: Vec<SinkConfigConnection>) -> SinkConfig {
        SinkConfig {
            generated_at: 0,
            groups,
            connections,
            unavailable_connections: Vec::new(),
        }
    }

    /// R2b-1d (design §5.4): when the group's connection disappears from `connections`
    /// (Hub cannot read its password any more → `unavailableConnections`, or deleted),
    /// the unchanged group is restarted too (so it is marked broken with the reason and
    /// does not queue rows nobody will write); when the connection comes back (password
    /// re-entered), the still-broken group is restarted again. A connection whose
    /// content changed (same id stays) still restarts only the connection.
    #[test]
    fn a_vanished_or_reappeared_connection_restarts_its_unchanged_groups() {
        let groups = vec![group(1, 10, "t1"), group(2, 11, "t2")];
        let both = vec![connection(10, "pw"), connection(11, "pw")];
        let (ag, ac) = applied(&groups, &both);
        let mut gone = desired(groups.clone(), vec![connection(11, "pw")]);
        gone.unavailable_connections = vec![UnavailableConnection {
            id: 10,
            name: "db10".to_string(),
            reason: "undecryptable".to_string(),
        }];
        let plan = plan_changes(&ag, &ac, &gone);
        assert_eq!(plan.connections_to_stop, vec![10]);
        assert_eq!(plan.groups_to_stop, vec![1]);
        assert_eq!(
            plan.groups_to_start
                .iter()
                .map(|g| g.id)
                .collect::<Vec<_>>(),
            vec![1]
        );

        let (ag, ac) = applied(&groups, &[connection(11, "pw")]);
        let plan = plan_changes(&ag, &ac, &desired(groups.clone(), both));
        assert_eq!(
            plan.connections_to_start
                .iter()
                .map(|c| c.id)
                .collect::<Vec<_>>(),
            vec![10]
        );
        assert_eq!(plan.groups_to_stop, vec![1]);
        assert_eq!(
            plan.groups_to_start
                .iter()
                .map(|g| g.id)
                .collect::<Vec<_>>(),
            vec![1]
        );
    }

    /// R2b-1d (design §5.4): a group whose connection the Hub lists as unavailable is
    /// never given a flusher - its setup error says the Hub cannot read the password.
    #[test]
    fn the_unavailable_message_names_the_connection_and_the_reason() {
        let message = GroupSetupError::ConnectionUnavailable {
            id: 10,
            name: "erp".to_string(),
            reason: "missing".to_string(),
        }
        .message();
        assert!(
            message.contains("Hub でパスワードを読めません"),
            "{message}"
        );
        assert!(
            message.contains("erp") && message.contains("missing"),
            "{message}"
        );
    }

    #[test]
    fn an_unchanged_config_plans_nothing() {
        let groups = vec![group(1, 10, "t1"), group(2, 10, "t2")];
        let connections = vec![connection(10, "pw")];
        let (ag, ac) = applied(&groups, &connections);
        let plan = plan_changes(&ag, &ac, &desired(groups, connections));
        assert!(plan.is_empty(), "{plan:?}");
    }

    #[test]
    fn only_the_changed_group_is_restarted() {
        let connections = vec![connection(10, "pw")];
        let mut groups = vec![group(1, 10, "t1"), group(2, 10, "t2")];
        let (ag, ac) = applied(&groups, &connections);
        groups[1].interval_ms = 5_000;
        let plan = plan_changes(&ag, &ac, &desired(groups.clone(), connections));
        assert_eq!(plan.groups_to_stop, vec![2]);
        assert_eq!(
            plan.groups_to_start
                .iter()
                .map(|g| g.id)
                .collect::<Vec<_>>(),
            vec![2]
        );
        assert!(plan.connections_to_stop.is_empty());
        assert!(plan.connections_to_start.is_empty());
    }

    #[test]
    fn a_new_group_starts_without_touching_the_others() {
        let connections = vec![connection(10, "pw")];
        let groups = vec![group(1, 10, "t1")];
        let (ag, ac) = applied(&groups, &connections);
        let plan = plan_changes(
            &ag,
            &ac,
            &desired(vec![group(1, 10, "t1"), group(2, 10, "t2")], connections),
        );
        assert!(plan.groups_to_stop.is_empty());
        assert_eq!(
            plan.groups_to_start
                .iter()
                .map(|g| g.id)
                .collect::<Vec<_>>(),
            vec![2]
        );
    }

    #[test]
    fn a_removed_group_is_stopped_and_not_restarted() {
        let connections = vec![connection(10, "pw")];
        let groups = vec![group(1, 10, "t1"), group(2, 10, "t2")];
        let (ag, ac) = applied(&groups, &connections);
        let plan = plan_changes(&ag, &ac, &desired(vec![group(1, 10, "t1")], connections));
        assert_eq!(plan.groups_to_stop, vec![2]);
        assert!(plan.groups_to_start.is_empty());
    }

    /// 接続の中身（パスワードを含む）が変わったら、その接続だけ張り直す -
    /// グループは触らない（キューを失わない）。
    #[test]
    fn a_changed_connection_restarts_only_that_connection() {
        let groups = vec![group(1, 10, "t1")];
        let connections = vec![connection(10, "pw"), connection(11, "pw")];
        let (ag, ac) = applied(&groups, &connections);
        let plan = plan_changes(
            &ag,
            &ac,
            &desired(groups, vec![connection(10, "new-pw"), connection(11, "pw")]),
        );
        assert_eq!(plan.connections_to_stop, vec![10]);
        assert_eq!(
            plan.connections_to_start
                .iter()
                .map(|c| c.id)
                .collect::<Vec<_>>(),
            vec![10]
        );
        assert!(plan.groups_to_stop.is_empty());
        assert!(plan.groups_to_start.is_empty());
    }

    #[test]
    fn a_removed_connection_is_stopped() {
        let groups = vec![];
        let connections = vec![connection(10, "pw")];
        let (ag, ac) = applied(&groups, &connections);
        let plan = plan_changes(&ag, &ac, &desired(vec![], vec![]));
        assert_eq!(plan.connections_to_stop, vec![10]);
        assert!(plan.connections_to_start.is_empty());
    }

    /// 空の設定（sink group が 1 つも無い）でも壊れない。
    #[test]
    fn an_empty_config_is_a_no_op_from_empty_state() {
        let plan = plan_changes(&HashMap::new(), &HashMap::new(), &desired(vec![], vec![]));
        assert!(plan.is_empty());
    }

    #[test]
    fn config_retry_uses_backoff_but_never_exceeds_the_refresh_interval() {
        let refresh = Duration::from_secs(30);
        assert_eq!(next_config_delay(true, refresh, 9), refresh);
        assert_eq!(next_config_delay(false, refresh, 1), Duration::from_secs(1));
        assert_eq!(next_config_delay(false, refresh, 3), Duration::from_secs(4));
        // 30 秒で頭打ち（バックオフの上限と取得間隔のどちらか短い方）。
        assert_eq!(
            next_config_delay(false, refresh, 9),
            Duration::from_secs(30)
        );
        let short = Duration::from_secs(5);
        assert_eq!(next_config_delay(false, short, 9), short);
    }

    /// 通常停止は設定値のまま。OS シャットダウンは残り時間で頭を抑える。
    #[test]
    fn phase_limit_keeps_graceful_stops_and_caps_os_shutdown() {
        assert_eq!(
            phase_limit(None, Duration::from_secs(60)),
            Duration::from_secs(60)
        );
        assert_eq!(
            phase_limit(Some(Duration::from_secs(27)), Duration::from_secs(5)),
            Duration::from_secs(5)
        );
        assert_eq!(
            phase_limit(Some(Duration::from_secs(3)), Duration::from_secs(60)),
            Duration::from_secs(3)
        );
        assert_eq!(
            phase_limit(Some(Duration::ZERO), Duration::from_secs(5)),
            Duration::ZERO
        );
    }

    #[test]
    fn os_shutdown_log_distinguishes_completion_from_cutoff() {
        let done = os_shutdown_log_line(true, 0);
        let cut = os_shutdown_log_line(false, 12);
        assert!(done.contains("完了しました"));
        assert!(done.contains("送信できなかった行: 0"));
        assert!(cut.contains("打ち切りました"));
        assert!(cut.contains("送信できなかった行: 12"));
        assert!(cut.contains("warn"));
    }

    /// サービスログを差し替えるテストを直列化する。
    /// `std::sync::Mutex` は await をまたげない（clippy）。
    async fn service_log_lock() -> tokio::sync::MutexGuard<'static, ()> {
        static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
        LOCK.lock().await
    }

    fn enable_test_service_log(name: &str) -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("banto-hub-sink-{name}-{}.log", std::process::id()));
        crate::log::enable_service_log_file(&path).expect("service log");
        path
    }

    /// 起動後の catalog 再取得を握ったまま OsShutdown が来ても、
    /// 運転ループがそこから抜けて `Engine::shutdown` まで進む。
    #[tokio::test]
    async fn held_catalog_still_reaches_shutdown_within_the_deadline() {
        let _log_guard = service_log_lock().await;
        let log_path = enable_test_service_log("held-catalog");
        let (_tls_dir, hub_tls, tls_acceptor) = temporary_hub_tls();

        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind");
        let hub_url = format!("https://{}", listener.local_addr().expect("addr"));
        let tags_seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (held_tx, held_rx) = tokio::sync::oneshot::channel::<()>();
        let held_tx = Arc::new(std::sync::Mutex::new(Some(held_tx)));
        let tags_seen_for_server = tags_seen.clone();
        tokio::spawn(async move {
            let config = r#"{"generatedAt":1,"groups":[{"id":1,"name":"g","dbConnectionId":10,"mode":"interval","intervalMs":60000,"tableName":"public.tag_history","storeBad":false,"tags":[{"connectionId":1,"groupId":2,"tagId":5,"externalName":"line1.fast.temp01"}]}],"connections":[{"id":10,"name":"db","host":"127.0.0.1","port":1,"database":"erp","username":"app","password":"pw"}]}"#;
            let catalog = r#"{"revision":1,"run_id":1,"collection_mode":"configured","tags":[]}"#;
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let tags_seen = tags_seen_for_server.clone();
                let held_tx = held_tx.clone();
                let config = config.to_owned();
                let catalog = catalog.to_owned();
                let tls_acceptor = tls_acceptor.clone();
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let Ok(mut stream) = tls_acceptor.accept(stream).await else {
                        return;
                    };
                    let mut request = Vec::new();
                    let mut buf = [0_u8; 1024];
                    loop {
                        match stream.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(count) => {
                                request.extend_from_slice(&buf[..count]);
                                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                                    break;
                                }
                            }
                        }
                    }
                    let line = String::from_utf8_lossy(&request)
                        .lines()
                        .next()
                        .unwrap_or_default()
                        .to_owned();
                    if line.starts_with("GET /api/v1/tags ") {
                        let n = tags_seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        if n == 0 {
                            let body = format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{catalog}",
                                catalog.len()
                            );
                            let _ = stream.write_all(body.as_bytes()).await;
                        } else {
                            if let Some(tx) = held_tx.lock().unwrap().take() {
                                let _ = tx.send(());
                            }
                            std::future::pending::<()>().await;
                        }
                    } else if line.starts_with("GET /api/sink/config ") {
                        let body = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{config}",
                            config.len()
                        );
                        let _ = stream.write_all(body.as_bytes()).await;
                    } else {
                        let _ = stream
                            .write_all(
                                b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                            )
                            .await;
                    }
                });
            }
        });

        let config = SidecarConfig {
            hub_url,
            api_key: "bh_test.secret".to_owned(),
            hub_tls: Some(hub_tls),
            config_refresh: Duration::from_secs(60),
            status_push: Duration::from_secs(60),
            queue_max_rows: 100,
            flush_interval: Duration::from_secs(60),
            batch_size: 100,
            shutdown_flush: Duration::from_millis(50),
        };
        let options = SidecarOptions {
            table_recheck: Duration::from_secs(30),
            health_tick: Duration::from_millis(40),
            replan_interval: Duration::from_millis(10),
        };
        let shutdown = async move {
            held_rx.await.expect("second catalog request");
            StopRequest::OsShutdown {
                deadline: StdInstant::now() + Duration::from_millis(800),
            }
        };
        let started = StdInstant::now();
        tokio::time::timeout(Duration::from_secs(3), run(config, options, shutdown))
            .await
            .expect("握った catalog のまま shutdown に入れなかった")
            .expect("run");
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "shutdown まで {:?}",
            started.elapsed()
        );
        let text = std::fs::read_to_string(&log_path).unwrap_or_default();
        assert!(
            text.contains("停止しました")
                && (text.contains("OS シャットダウンの停止が完了しました")
                    || text.contains("OS シャットダウンの予算で停止を打ち切りました")),
            "{text}"
        );
    }

    /// 応答しない DB でも `Engine::shutdown` が未送信行のログと
    /// 「停止しました」まで進む（サービスの Stopped 報告の手前）。
    #[tokio::test]
    async fn unresponsive_db_still_reaches_stopped_within_the_deadline() {
        let _log_guard = service_log_lock().await;
        let log_path = enable_test_service_log("blackhole-db");
        let (_tls_dir, hub_tls, tls_acceptor) = temporary_hub_tls();

        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind");
        let hub_url = format!("https://{}", listener.local_addr().expect("addr"));
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let tls_acceptor = tls_acceptor.clone();
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let Ok(mut stream) = tls_acceptor.accept(stream).await else {
                        return;
                    };
                    let mut request = Vec::new();
                    let mut buf = [0_u8; 1024];
                    loop {
                        match stream.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(count) => {
                                request.extend_from_slice(&buf[..count]);
                                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                                    break;
                                }
                            }
                        }
                    }
                    let _ = stream
                        .write_all(
                            b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        )
                        .await;
                });
            }
        });

        let config = SidecarConfig {
            hub_url,
            api_key: "bh_test.secret".to_owned(),
            hub_tls: Some(hub_tls),
            config_refresh: Duration::from_secs(60),
            status_push: Duration::from_secs(60),
            queue_max_rows: 100,
            flush_interval: Duration::from_secs(60),
            batch_size: 100,
            shutdown_flush: Duration::ZERO,
        };
        let mut engine = Engine::new(config, SidecarOptions::default()).expect("engine");
        let group = group(1, 10, "public.tag_history");
        let runtime = crate::group::GroupRuntime::new(&group, 100, 0).expect("group");
        runtime.push_rows(vec![crate::queue::Row {
            ts_ms: 1,
            tag_id: 5,
            external_name: "line1.fast.temp01".to_owned(),
            value: Some(1.0),
            quality: "good".to_owned(),
        }]);
        engine.groups.insert(Arc::new(runtime));
        let (handle, _server) = crate::flush::flusher_on_blackhole_pool(10).await;
        engine.flushers.insert(10, handle);

        // 期限は flusher の停止の上限（2 秒）より長くする。flusher は 2 秒で
        // 打ち切られ、その後の状態 push は期限内に済む - 打ち切りの記録が
        // flusher の結果だけから来ることを確かめるため（#589 のレビュー）。
        let started = StdInstant::now();
        tokio::time::timeout(
            Duration::from_secs(6),
            engine.shutdown(StopRequest::OsShutdown {
                deadline: StdInstant::now() + FLUSHER_STOP_TIMEOUT + Duration::from_secs(2),
            }),
        )
        .await
        .expect("応答しない DB のまま Stopped の手前まで進めなかった");
        assert!(started.elapsed() < Duration::from_secs(6));
        let text = std::fs::read_to_string(&log_path).unwrap_or_default();
        assert!(
            text.contains("停止時に 1 行を送信できませんでした") && text.contains("停止しました"),
            "{text}"
        );
        // flusher を期限で打ち切ったので「完了」とは書かない。
        assert!(
            text.contains("OS シャットダウンの予算で停止を打ち切りました")
                && !text.contains("OS シャットダウンの停止が完了しました"),
            "{text}"
        );
    }
}
