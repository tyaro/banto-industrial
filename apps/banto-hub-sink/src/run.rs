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
//! Ctrl-C / SCM の STOP）はどの待ちからでも即座に抜ける。
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

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use banto_tagclient::StableTagId;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::config::SidecarConfig;
use crate::flush::{backoff_delay, build_pool, spawn_flusher, FlushOptions, FlusherHandle};
use crate::group::{
    run_interval_producer, run_on_change_producer, GroupRegistry, GroupRuntime, GroupSetupError,
    SinkMode, DEFAULT_TABLE_RECHECK,
};
use crate::hub_api::{
    HubApi, HubApiError, SinkConfig, SinkConfigConnection, SinkConfigGroup, SinkStatusPush,
};
use crate::log::{log_err_line, log_line};
use crate::status::{push_entry, GroupState, Redactor};
use crate::values::{normalize_ids, Subscription, ValueView};

/// 購読の生存確認の間隔。
const HEALTH_TICK: Duration = Duration::from_secs(1);

/// 停止時に flusher の join を待つ上限（これを超えたら abort - commit 前
/// なので二重書き込みにはならない）。
const FLUSHER_STOP_TIMEOUT: Duration = Duration::from_secs(2);

/// 停止時の drain 監視のポーリング間隔。
const DRAIN_POLL: Duration = Duration::from_millis(50);

/// 実行時の調整値。**設定ファイルには出さない**（運用者が触る値ではなく、
/// テストが実時間を待たないための注入口）。
#[derive(Clone, Copy, Debug)]
pub struct SidecarOptions {
    /// テーブル検査の再試行間隔（設計 §5.3 の 30 秒）。
    pub table_recheck: Duration,
    /// 購読の生存確認の間隔。
    pub health_tick: Duration,
}

impl Default for SidecarOptions {
    fn default() -> Self {
        Self {
            table_recheck: DEFAULT_TABLE_RECHECK,
            health_tick: HEALTH_TICK,
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
                flusher.stop(FLUSHER_STOP_TIMEOUT).await;
            }
        }

        // 3. 接続を先に用意する（プロデューサが flusher の起床口を持つため）。
        let connections: HashMap<i64, SinkConfigConnection> = desired
            .connections
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
            self.start_group(group, &connections, carried);
        }

        self.applied_groups = desired.groups.iter().map(|g| (g.id, g.clone())).collect();
        self.applied_connections = connections;

        self.ensure_subscription().await;
    }

    fn start_group(
        &mut self,
        group: &SinkConfigGroup,
        connections: &HashMap<i64, SinkConfigConnection>,
        carried_dropped: u64,
    ) {
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
    async fn ensure_subscription(&mut self) {
        let desired = self.desired_stable_ids();

        if let Some(subscription) = &self.subscription {
            let dead = subscription.is_dead();
            let changed = subscription.subscribed() != desired.as_slice();
            if !dead && !changed {
                if self.view_rx.borrow().live {
                    // 一度でも Live になったら、次の障害は 1 秒から
                    // やり直す（`banto_tagclient` の RetryTracker と同じ
                    // 考え方）。
                    self.subscription_attempt = 0;
                }
                return;
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
        match Subscription::start(&self.config, desired, self.view_tx.clone()) {
            Ok(subscription) => {
                if subscription.is_some() {
                    log_line(&format!(
                        "banto-hub-sink: Hub の購読を開始しました（{} タグ）",
                        self.subscribed_len()
                    ));
                }
                self.subscription = subscription;
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

    fn subscribed_len(&self) -> usize {
        self.desired_stable_ids().len()
    }

    /// 全 sink group の対象タグの和集合（重複除去・ソート済み）。
    fn desired_stable_ids(&self) -> Vec<StableTagId> {
        let mut ids = Vec::new();
        for group in self.applied_groups.values() {
            for tag in &group.tags {
                ids.push(StableTagId::new(
                    tag.connection_id,
                    tag.group_id,
                    tag.tag_id,
                ));
            }
        }
        normalize_ids(ids)
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

    async fn shutdown(mut self) {
        log_line("banto-hub-sink: 停止します（残りのキューを flush します）");
        // 1. 行の生産を止める。
        for (_, task) in self.producers.drain() {
            task.abort();
        }
        // 2. 購読を止める（Hub 側の購読を残さない）。
        if let Some(subscription) = self.subscription.take() {
            subscription.stop().await;
        }
        // 3. 残キューを最大 shutdown_flush_secs だけ流す。
        for flusher in self.flushers.values() {
            flusher.request_drain();
        }
        let deadline = Instant::now() + self.config.shutdown_flush;
        while Instant::now() < deadline {
            if self.groups.all().iter().all(|group| group.queued() == 0) {
                break;
            }
            tokio::time::sleep(DRAIN_POLL).await;
        }
        // 4. flusher を止める。
        let flushers: Vec<FlusherHandle> = self.flushers.drain().map(|(_, f)| f).collect();
        for flusher in flushers {
            flusher.stop(FLUSHER_STOP_TIMEOUT).await;
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
        self.push_status().await;
        log_line("banto-hub-sink: 停止しました");
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
    F: Future<Output = ()>,
{
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

    let mut engine = Engine::new(config, options)?;
    tokio::pin!(shutdown);

    // 起動直後は「取れるまで待つ」だけ - 取れなくても終了しない。
    let ok = tokio::select! {
        _ = &mut shutdown => { engine.shutdown().await; return Ok(()); }
        ok = engine.refresh_config() => ok,
    };
    let now = Instant::now();
    let mut next_config_at = now + next_config_delay(ok, config_refresh, engine.config_attempt);
    let mut next_status_at = now;
    let mut next_health_at = now + health_tick;

    loop {
        let wake_at = next_config_at.min(next_status_at).min(next_health_at);
        tokio::select! {
            _ = &mut shutdown => break,
            _ = tokio::time::sleep_until(wake_at) => {}
        }
        let now = Instant::now();
        if now >= next_config_at {
            let ok = engine.refresh_config().await;
            next_config_at =
                Instant::now() + next_config_delay(ok, config_refresh, engine.config_attempt);
        }
        if now >= next_status_at {
            engine.push_status().await;
            next_status_at = Instant::now() + status_push;
        }
        if now >= next_health_at {
            engine.ensure_subscription().await;
            next_health_at = Instant::now() + health_tick;
        }
    }

    engine.shutdown().await;
    Ok(())
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
    use super::*;
    use crate::hub_api::SinkConfigTag;

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
        }
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
}
