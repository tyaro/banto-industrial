//! Hub の現在値を読み取り専用 OPC UA サーバーへ公開する。
//! `HubRuntime` が所有し、管理 API の設定変更と終了処理で同じインスタンスを使う。
//!
//! ## listener の起動は裏で行う（#634、2026-10-17 オーナー決定 案 2）
//!
//! `OpcUaServer::start` は初回の RSA 鍵の生成などで数秒〜10 秒超かかる。
//! [`OpcUaPublisher::apply`] は設定を記録して起動を背景タスクへ渡したらすぐ
//! 返り、起動の結果は [`OpcUaStatus`]（`GET /api/opcua-status`）に出す。
//! 状態は「停止 → 起動中 → 起動済み / 失敗（理由つき）」。
//!
//! 並行の約束:
//! - **後の要求が勝つ**: `apply` のたびに世代（`generation`）を 1 つ進める。
//!   起動タスクは自分の世代を持ち、結果を書くときに世代が変わっていたら
//!   結果を捨てる（起動できた listener は止めて閉じる）。古い起動の結果が
//!   新しい状態（停止・別の設定の起動中）を上書きしない。
//! - **停止と起動が重ならない**: 新しい起動タスクは、前の起動タスク（と、
//!   その世代が古くなったときの後始末）が終わるのを待ってから bind する。
//!   動いている listener は `apply` が `inner` のロックの下で止めてから
//!   次の起動タスクを起こす。同じポートへ設定し直しても、古い listener と
//!   取り合わない。
//!   ただし `apply` が `stopped`（や次の `starting`）を返した時点では、古い
//!   起動が開いた listener がまだ閉じ終わっていないことがある（古い起動
//!   タスクが起動を終えてから閉じる）。次の bind はその後始末を待つ。
//! - **終了**: [`OpcUaPublisher::shutdown`] は世代を進めて起動中のタスクの
//!   終わりを待つ。起動しかけの listener を Hub の終了後に残さない。
//! - ロックの順序: 起動タスクは `OpcUaServer::start` を待つ間 `inner` の
//!   ロックを持たない（状態の読み取り・次の `apply` を塞がない）。
//! - **async の worker を塞がない**（PR #650 のオーナーレビュー）:
//!   `OpcUaServer::start` は中で同期の `builder.build`・RSA 鍵の生成
//!   （`create_sample_keypair`）を行う。そのまま worker の上で走らせると、
//!   worker が 1 本の runtime では鍵の生成の間、PUT の応答も状態の GET も
//!   進まない。本番の starter は `spawn_blocking` の中で
//!   `Handle::block_on` して blocking 用のスレッドで実行する。listener の
//!   タスクは `Handle` の runtime（Hub の runtime）に spawn される。
//! - **panic しても `starting` に残さない**: 起動タスクが panic したら、
//!   世代が最新のときは `failed`（理由つき）にする。`initialize` の起動の
//!   待ちには、ハングを検出するための有限の上限（[`SETTLE_LIMIT`]）がある。
//!
//! ## 鍵と証明書の置き場（#634 の残り、2026-10-29 オーナー決定「採用する」）
//!
//! banto-opcua の `ServerConfig::with_pki_dir`（banto-opcua#23）に、profile の保護された
//! データの下の置き場（[`crate::opcua_pki::pki_dir`]）を渡す。鍵と証明書は初回だけ作り、
//! 次の起動からは再利用する（以前は起動のたびに一時 PKI で作っていた）。
//! - 本番の starter は、blocking 用のスレッドで**鍵を作る・読む前に**
//!   [`crate::opcua_pki::prepare`] で置き場を用意して ACL を許可リストで確かめ、断られたら
//!   起動しない（`failed` と理由）。
//! - banto-opcua の `Error::Pki`（置き場・組を用意できない）も `failed` と理由（置き場と
//!   再試行のしかたつき）にしてログに出す。原因を直したあと、同じ設定の保存が再試行になる
//!   （`apply` は `failed` のときの同じ設定を再試行として扱う）。
//! - 同じ置き場への起動は重ならない: 置き場は profile ごと（profile は 1 プロセスだけが
//!   握る）で、このプロセスの中では上の「停止と起動が重ならない」の鎖で起動が 1 つずつ走る。

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use banto_collect::Quality;
use banto_opcua_server::model::{DataType, Quality as UaQuality, SampleTimestamps};
use banto_opcua_server::{OpcUaServer, RunningServer, ServerConfig, TagReading};
use futures_util::future::{BoxFuture, FutureExt};
use serde::Serialize;
use tokio::sync::{watch, Mutex};
use tokio::task::JoinHandle;

use crate::hub::{read_current_view, CollectorManager, CurrentView, TagEntry};
use crate::hub_log::log_err_line;
use crate::settings::OpcUaSettings;

const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// `initialize` が listener の起動の結果を待つ上限（ハング検出用）。超えたら
/// 警告の行を出して Hub の起動を続ける（状態は `starting` のまま見える）。
const SETTLE_LIMIT: Duration = Duration::from_secs(120);

/// listener の状態（#634）。`snake_case` で状態の API に出す。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OpcUaState {
    /// 無効、または Hub の終了後。
    Stopped,
    /// 有効化・再設定を受け付け、listener を起動している。
    Starting,
    /// listener が待ち受けている。
    Running,
    /// 起動に失敗した（bind の失敗・設定の不正など）。理由は `error`。
    Failed,
}

/// `GET /api/opcua-status` の本文（#634）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpcUaStatus {
    pub state: OpcUaState,
    /// 起動済みのときだけ、クライアントへ案内する endpoint の URL。
    pub endpoint: Option<String>,
    /// 失敗のときだけ、その理由。
    pub error: Option<String>,
    /// この状態を作った設定の世代。`apply` のたびに 1 つ進む。
    pub generation: u64,
}

impl OpcUaStatus {
    fn stopped(generation: u64) -> Self {
        Self {
            state: OpcUaState::Stopped,
            endpoint: None,
            error: None,
            generation,
        }
    }

    fn starting(generation: u64) -> Self {
        Self {
            state: OpcUaState::Starting,
            ..Self::stopped(generation)
        }
    }

    fn running(generation: u64, endpoint: String) -> Self {
        Self {
            state: OpcUaState::Running,
            endpoint: Some(endpoint),
            ..Self::stopped(generation)
        }
    }

    fn failed(generation: u64, error: String) -> Self {
        Self {
            state: OpcUaState::Failed,
            error: Some(error),
            ..Self::stopped(generation)
        }
    }
}

/// listener を起動する関数。本番は `OpcUaServer::start`、テストは起動の
/// 遅い・失敗する偽物に差し替える。
type Starter =
    Arc<dyn Fn(ServerConfig) -> BoxFuture<'static, Result<RunningServer, String>> + Send + Sync>;

/// 本番の starter。`OpcUaServer::start` の同期の処理（`builder.build`・
/// 鍵の生成）を blocking 用のスレッドで行い、async の worker を塞がない
/// （モジュール doc「async の worker を塞がない」）。
///
/// 鍵と証明書の置き場（`config.pki_dir`）があれば、鍵を作る・読む前に
/// [`crate::opcua_pki::prepare`] で用意して確かめ、断られたら起動しない
/// （モジュール doc「鍵と証明書の置き場」）。
///
/// `profile` は握った profile（フォルダの本当のパスと id）。その DACL で許可されている
/// 利用者のうち、確かめ済みの記録にある（記録が無ければ利用者のアカウントに解決できる）
/// ものも、置き場の許可リストに入る（[`crate::opcua_pki::prepare`]）。
fn real_starter(profile: Option<crate::opcua_pki::PkiProfile>) -> Starter {
    Arc::new(move |config| {
        let profile = profile.clone();
        Box::pin(async move {
            let handle = tokio::runtime::Handle::current();
            tokio::task::spawn_blocking(move || start_blocking(&handle, config, profile.as_ref()))
                .await
                .map_err(|err| format!("OPC UA の起動処理が異常終了しました: {err}"))?
        })
    })
}

/// blocking 用のスレッドで行う起動の本体（置き場の確かめ → `OpcUaServer::start`）。
fn start_blocking(
    handle: &tokio::runtime::Handle,
    config: ServerConfig,
    profile: Option<&crate::opcua_pki::PkiProfile>,
) -> Result<RunningServer, String> {
    #[cfg(test)]
    start_gate::wait(config.port);
    let pki_dir = config.pki_dir.clone();
    if let Some(dir) = &pki_dir {
        crate::opcua_pki::prepare(dir, profile)?;
    }
    handle
        .block_on(OpcUaServer::start(config))
        .map_err(|err| describe_start_error(&err, pki_dir.as_deref()))
}

/// 起動の失敗の文言。`Error::Pki`（置き場・鍵と証明書の組を用意できない）には、
/// 置き場と再試行のしかたを足す（banto-opcua#23: 片方だけ残ったファイルは次の起動で
/// 消して組で作り直す。存在の確かめ自体が権限エラーのときは消さずに止まるので、
/// 原因を直してから再試行する）。
fn describe_start_error(
    err: &banto_opcua_server::Error,
    pki_dir: Option<&std::path::Path>,
) -> String {
    match (err, pki_dir) {
        (banto_opcua_server::Error::Pki(_), Some(dir)) => format!(
            "{err}（鍵と証明書の置き場: {}。原因を直してから、同じ設定でもう一度保存すると再試行します。片方だけ残った鍵・証明書は次の起動で作り直します）",
            dir.display()
        ),
        _ => err.to_string(),
    }
}

/// テストだけの、起動を止める門（`runtime` のテスト
/// `opcua_put_and_status_respond_during_a_real_start_on_one_worker`）。ポートごとに
/// 登録し、本物の starter の blocking 用のスレッドの中で**同期で**待つ（鍵の生成の
/// 遅さに頼らずに「起動中」を作る。`spawn_blocking` をやめると唯一の worker が
/// 塞がって落ちる）。送り手を落とすと待ちが終わる。
#[cfg(test)]
pub(crate) mod start_gate {
    use std::collections::HashMap;
    use std::sync::mpsc::{channel, Receiver, Sender};
    use std::sync::{LazyLock, Mutex};

    static GATES: LazyLock<Mutex<HashMap<u16, Receiver<()>>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));

    /// `port` への次の起動を、返した送り手が送る（落ちる）まで止める。
    pub(crate) fn hold(port: u16) -> Sender<()> {
        let (tx, rx) = channel();
        GATES.lock().expect("gates").insert(port, rx);
        tx
    }

    pub(super) fn wait(port: u16) {
        let gate = GATES.lock().expect("gates").remove(&port);
        if let Some(gate) = gate {
            let _ = gate.recv();
        }
    }
}

/// panic の内容を文言にする。
fn panic_message(panic: &(dyn std::any::Any + Send)) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|message| (*message).to_owned())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "理由不明".to_owned())
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct ReadingKey {
    value: Option<f64>,
    quality: Quality,
    source_time_ms: Option<i64>,
    quality_since_ms: Option<i64>,
}

impl From<CurrentView> for ReadingKey {
    fn from(view: CurrentView) -> Self {
        Self {
            value: view.value,
            quality: view.quality,
            source_time_ms: view.source_time_ms,
            quality_since_ms: view.quality_since_ms,
        }
    }
}

#[derive(Debug, Clone)]
struct PublishedTag {
    ids: (i64, i64, i64),
    data_type: DataType,
    reading: ReadingKey,
    connection: String,
    group: String,
    name: String,
}

struct Inner {
    server: Option<RunningServer>,
    tags: HashMap<String, PublishedTag>,
    settings: Option<OpcUaSettings>,
    worker: Option<JoinHandle<()>>,
    last_errors: HashMap<String, String>,
    stopping: bool,
    /// `apply`・`shutdown` のたびに進む世代。起動タスクの結果は、自分の世代が
    /// まだ最新のときだけ書く（モジュール doc「並行の約束」）。
    generation: u64,
    /// いちばん新しい起動タスク。次の起動タスクと `shutdown` がこれの終わりを
    /// 待つ（起動タスクは前の起動タスクを待つので、鎖で全部を待てる）。
    start_task: Option<JoinHandle<()>>,
}

pub struct OpcUaPublisher {
    manager: Arc<CollectorManager>,
    inner: Mutex<Inner>,
    /// 状態は `inner` のロックの下でだけ書く（世代の判定と同じ順序に乗せる）。
    status: watch::Sender<OpcUaStatus>,
    starter: Starter,
    /// 鍵と証明書の置き場（[`crate::opcua_pki`]）。`None` は起動のたびの一時 PKI
    /// （テストだけ）。
    pki_dir: Option<PathBuf>,
}

impl OpcUaPublisher {
    /// `pki_dir` は鍵と証明書の置き場（profile の保護されたデータの下。
    /// [`crate::opcua_pki::pki_dir`]）。`profile` は握った profile（フォルダの本当のパスと
    /// id。握っていなければ `None`）。その DACL で許可されている利用者のうち、確かめ済みの
    /// 記録にある（記録が無ければ利用者のアカウントに解決できる）ものも、置き場の許可リストに
    /// 入る（[`crate::opcua_pki::prepare`]）。
    pub fn new(
        manager: Arc<CollectorManager>,
        pki_dir: PathBuf,
        profile: Option<crate::opcua_pki::PkiProfile>,
    ) -> Arc<Self> {
        Self::with_starter(manager, real_starter(profile), Some(pki_dir))
    }

    fn with_starter(
        manager: Arc<CollectorManager>,
        starter: Starter,
        pki_dir: Option<PathBuf>,
    ) -> Arc<Self> {
        Arc::new(Self {
            manager,
            inner: Mutex::new(Inner {
                server: None,
                tags: HashMap::new(),
                settings: None,
                worker: None,
                last_errors: HashMap::new(),
                stopping: false,
                generation: 0,
                start_task: None,
            }),
            status: watch::channel(OpcUaStatus::stopped(0)).0,
            starter,
            pki_dir,
        })
    }

    /// 今の listener の状態（`GET /api/opcua-status`）。
    pub fn status(&self) -> OpcUaStatus {
        self.status.borrow().clone()
    }

    /// 設定を記録し、listener の停止・起動を始めてすぐ返る（#634）。起動の
    /// 結果（起動済み・失敗とその理由）は [`Self::status`] に出る。Hub 本体は
    /// 起動・bind の失敗でも継続する。
    pub async fn apply(self: &Arc<Self>, settings: &OpcUaSettings) {
        self.apply_inner(settings, false).await;
    }

    /// REST listener 起動後に設定を復元する。先に管理 API から設定変更が
    /// 入った場合、その新しい設定を古い起動時スナップショットで巻き戻さない。
    /// Hub の起動手順はこれまでどおり listener の起動の結果まで待つ
    /// （起動直後から endpoint が使える。失敗しても Hub は継続する）。
    pub async fn initialize(self: &Arc<Self>, settings: &OpcUaSettings) {
        self.apply_inner(settings, true).await;
        self.wait_settled().await;
    }

    /// 起動中でなくなるまで待つ（上限 [`SETTLE_LIMIT`]）。
    async fn wait_settled(&self) {
        self.wait_settled_within(SETTLE_LIMIT).await;
    }

    /// 起動中でなくなるまで、`limit` を上限に待つ。上限を超えたら警告の行を
    /// 出して戻り、`false` を返す。
    async fn wait_settled_within(&self, limit: Duration) -> bool {
        let mut status = self.status.subscribe();
        let settled = tokio::time::timeout(
            limit,
            status.wait_for(|status| status.state != OpcUaState::Starting),
        )
        .await
        .is_ok();
        if !settled {
            log_err_line(&format!(
                "[WARN] banto-hub: OPC UA の起動が {} 秒以内に終わりませんでした。Hub の起動を続けます（状態は GET /api/opcua-status）",
                limit.as_secs()
            ));
        }
        settled
    }

    async fn apply_inner(self: &Arc<Self>, settings: &OpcUaSettings, only_if_uninitialized: bool) {
        let mut inner = self.inner.lock().await;
        if inner.stopping {
            return;
        }
        if only_if_uninitialized && inner.settings.is_some() {
            return;
        }
        if inner.settings.as_ref() == Some(settings) {
            // 同じ設定で、止まっている・起動中・起動済みなら何もしない。
            // 失敗していたら、同じ設定の保存を再試行として扱う。
            let state = self.status.borrow().state;
            if !settings.enabled || matches!(state, OpcUaState::Starting | OpcUaState::Running) {
                return;
            }
        }
        inner.generation += 1;
        let generation = inner.generation;
        Self::stop_listener(&mut inner).await;
        inner.settings = Some(settings.clone());
        if !settings.enabled {
            self.status.send_replace(OpcUaStatus::stopped(generation));
            return;
        }
        let config = match server_config(settings, self.pki_dir.as_deref()) {
            Ok(config) => config,
            Err(reason) => {
                log_err_line(&format!("banto-hub: OPC UA を起動しません: {reason}"));
                self.status
                    .send_replace(OpcUaStatus::failed(generation, reason));
                return;
            }
        };
        self.status.send_replace(OpcUaStatus::starting(generation));
        let previous = inner.start_task.take();
        let publisher = self.clone();
        inner.start_task = Some(tokio::spawn(async move {
            let outcome =
                AssertUnwindSafe(publisher.clone().run_start(generation, config, previous))
                    .catch_unwind()
                    .await;
            if let Err(panic) = outcome {
                publisher
                    .fail_after_panic(generation, panic_message(&*panic))
                    .await;
            }
        }));
    }

    /// 起動タスクが panic したとき、世代が最新で `starting` のままなら
    /// `failed` にする（`starting` に残すと、起動の待ちが戻らず、同じ設定の
    /// 保存が再試行にならず、画面が読み続ける）。
    async fn fail_after_panic(&self, generation: u64, message: String) {
        let inner = self.inner.lock().await;
        let reason = format!("OPC UA の起動処理が異常終了しました（panic: {message}）");
        log_err_line(&format!("banto-hub: {reason}"));
        if inner.stopping
            || inner.generation != generation
            || self.status.borrow().state != OpcUaState::Starting
        {
            return;
        }
        self.status
            .send_replace(OpcUaStatus::failed(generation, reason));
    }

    /// 起動タスクの本体。`generation` が最新のときだけ結果を書く。
    async fn run_start(
        self: Arc<Self>,
        generation: u64,
        config: ServerConfig,
        previous: Option<JoinHandle<()>>,
    ) {
        // 前の起動（と、その結果を捨てるときの listener の停止）が終わって
        // から bind する。同じポートを取り合わない。
        if let Some(previous) = previous {
            let _ = previous.await;
        }
        {
            let inner = self.inner.lock().await;
            if inner.stopping || inner.generation != generation {
                return;
            }
        }
        let result = (self.starter)(config).await;
        let mut inner = self.inner.lock().await;
        if inner.stopping || inner.generation != generation {
            // 起動の間に無効化・再設定・終了が来た。古い起動の結果は状態に
            // 書かず、起動できた listener は止める（ロックは外して止める。
            // 次の起動タスクはこのタスクの終わりを待つので重ならない）。
            drop(inner);
            if let Ok(server) = result {
                if let Err(err) = server.shutdown().await {
                    log_err_line(&format!("banto-hub: OPC UA の停止に失敗しました: {err}"));
                }
            }
            return;
        }
        let server = match result {
            Ok(server) => server,
            Err(err) => {
                log_err_line(&format!("banto-hub: OPC UA の起動に失敗しました: {err}"));
                self.status
                    .send_replace(OpcUaStatus::failed(generation, err));
                return;
            }
        };
        log_err_line(&format!(
            "banto-hub: OPC UA endpoint {} (SecurityPolicy None / Anonymous; LAN 公開は機密保護なし)",
            server.endpoint()
        ));
        self.status.send_replace(OpcUaStatus::running(
            generation,
            server.endpoint().to_owned(),
        ));
        inner.server = Some(server);
        self.reconcile(&mut inner);
        let publisher = self.clone();
        inner.worker = Some(tokio::spawn(async move {
            let mut timer = tokio::time::interval(POLL_INTERVAL);
            timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            timer.tick().await;
            loop {
                timer.tick().await;
                let mut inner = publisher.inner.lock().await;
                publisher.reconcile(&mut inner);
            }
        }));
    }

    /// 動いている同期タスクと listener を止める（`inner` のロックの下で）。
    async fn stop_listener(inner: &mut Inner) {
        if let Some(worker) = inner.worker.take() {
            worker.abort();
            let _ = worker.await;
        }
        if let Some(server) = inner.server.take() {
            if let Err(err) = server.shutdown().await {
                log_err_line(&format!("banto-hub: OPC UA の停止に失敗しました: {err}"));
            }
        }
        inner.tags.clear();
        inner.last_errors.clear();
    }

    /// 値取得は共通の `read_current_view` を使う。ServerTimestamp だけが
    /// 進む場合は更新を送らず、不要な DataChange 通知を抑える。
    fn reconcile(&self, inner: &mut Inner) {
        let Some(server) = inner.server.as_ref() else {
            return;
        };
        let map = self.manager.tag_map();
        let collect = self.manager.current_values();
        let store = self.manager.server_store();
        let now_ms = self.manager.clock().now_ms();
        let mut desired = HashMap::new();
        for entry in map.iter() {
            if let Some(data_type) = data_type(entry) {
                desired.insert(entry.external_name.clone(), (entry, data_type));
            }
        }
        inner
            .last_errors
            .retain(|name, _| desired.contains_key(name));

        let mut blocked = HashSet::new();
        inner.tags.retain(|name, old| {
            let keep = desired.get(name).is_some_and(|(entry, ty)| {
                entry.ids == old.ids
                    && *ty == old.data_type
                    && entry.connection == old.connection
                    && entry.group == old.group
                    && entry.name == old.name
            });
            if !keep {
                if let Err(err) = server.remove_tag(&old.connection, &old.group, &old.name) {
                    log_err_line(&format!("banto-hub: OPC UA タグ削除 {name}: {err}"));
                    blocked.insert(name.clone());
                    return true;
                }
            }
            keep
        });
        for (name, (entry, data_type)) in desired {
            if blocked.contains(&name) {
                continue;
            }
            let (connection, group, tag) = (
                entry.connection.as_str(),
                entry.group.as_str(),
                entry.name.as_str(),
            );
            let view = read_current_view(entry, collect.as_ref(), &store, now_ms);
            let reading_key = ReadingKey::from(view);
            if inner
                .tags
                .get(&name)
                .is_some_and(|old| old.reading == reading_key)
            {
                continue;
            }
            let reading = tag_reading(data_type, view);
            let result = if inner.tags.contains_key(&name) {
                server.update_tag_reading(connection, group, tag, reading)
            } else {
                server.add_tag_reading(connection, group, tag, reading)
            };
            match result {
                Ok(()) => {
                    inner.last_errors.remove(&name);
                    inner.tags.insert(
                        name,
                        PublishedTag {
                            ids: entry.ids,
                            data_type,
                            reading: reading_key,
                            connection: entry.connection.clone(),
                            group: entry.group.clone(),
                            name: entry.name.clone(),
                        },
                    );
                }
                Err(err) => {
                    let error = err.to_string();
                    if inner.last_errors.get(&name) != Some(&error) {
                        log_err_line(&format!("banto-hub: OPC UA タグ同期 {name}: {error}"));
                        inner.last_errors.insert(name, error);
                    }
                }
            }
        }
    }

    /// Hub の終了。世代を進めて起動中のタスクの終わりを待ち（起動しかけの
    /// listener はそのタスクが止める）、動いている listener を止める。
    pub async fn shutdown(&self) {
        let start_task = {
            let mut inner = self.inner.lock().await;
            inner.stopping = true;
            inner.generation += 1;
            inner.start_task.take()
        };
        if let Some(task) = start_task {
            let _ = task.await;
        }
        let mut inner = self.inner.lock().await;
        Self::stop_listener(&mut inner).await;
        self.status
            .send_replace(OpcUaStatus::stopped(inner.generation));
    }

    /// テスト用: いちばん新しい起動タスク（と、それが待つ前の起動タスク）の
    /// 終わりを待つ。
    #[cfg(test)]
    async fn join_start_task_for_test(&self) {
        let task = self.inner.lock().await.start_task.take();
        if let Some(task) = task {
            task.await.expect("start task");
        }
    }

    #[cfg(test)]
    pub(crate) async fn endpoint_and_namespace(&self) -> Option<(String, u16)> {
        self.inner
            .lock()
            .await
            .server
            .as_ref()
            .map(|server| (server.endpoint().to_owned(), server.namespace_index()))
    }
}

/// 保存済みの設定から listener の設定を作る。不正なら状態の API に出す理由を返す。
fn server_config(
    settings: &OpcUaSettings,
    pki_dir: Option<&std::path::Path>,
) -> Result<ServerConfig, String> {
    let validation = crate::rest::validate_opcua_settings(settings);
    if !validation.is_empty() {
        let fields = validation
            .iter()
            .map(|error| format!("{}: {}", error.field, error.message))
            .collect::<Vec<_>>()
            .join(" / ");
        return Err(format!("OPC UA 設定が不正です（{fields}）"));
    }
    let bind: IpAddr = settings
        .bind
        .parse()
        .map_err(|err| format!("OPC UA の bind アドレスが不正です: {err}"))?;
    let mut config = ServerConfig::default()
        .with_bind(bind)
        .with_port(settings.port)
        .with_endpoint_path(&settings.endpoint_path)
        .with_namespace_uri(&settings.namespace_uri)
        .with_sample_tags(false);
    if let Some(host) = &settings.advertised_host {
        config = config.with_advertised_host(host);
    }
    if let Some(dir) = pki_dir {
        config = config.with_pki_dir(dir);
    }
    Ok(config)
}

fn data_type(entry: &TagEntry) -> Option<DataType> {
    match entry.data_type.as_str() {
        "string" => None,
        "bit" => Some(DataType::Boolean),
        _ => Some(DataType::Double),
    }
}

fn system_time(ms: i64) -> Option<SystemTime> {
    UNIX_EPOCH.checked_add(Duration::from_millis(u64::try_from(ms).ok()?))
}

fn tag_reading(data_type: DataType, view: CurrentView) -> TagReading {
    let quality = match view.quality {
        Quality::Good => UaQuality::Good,
        Quality::Bad => UaQuality::Bad,
        Quality::Stale => UaQuality::Stale,
    };
    let timestamps = SampleTimestamps::for_quality(
        quality,
        view.source_time_ms.and_then(system_time),
        view.quality_since_ms.and_then(system_time),
        SystemTime::now(),
    );
    TagReading::new(data_type, view.value, quality, timestamps)
}

#[cfg(test)]
mod tests {
    use super::*;

    use tokio::sync::{mpsc, oneshot};

    /// 起動の結果をテストが決めるまで待つ偽物の starter。`Ok` を返すと本物の
    /// `OpcUaServer::start` で listener を起動する（`RunningServer` は外から
    /// 作れないため）。
    type StartCall = oneshot::Sender<Result<(), String>>;

    async fn gated_publisher() -> (Arc<OpcUaPublisher>, mpsc::UnboundedReceiver<StartCall>) {
        let (calls_tx, calls_rx) = mpsc::unbounded_channel::<StartCall>();
        let starter: Starter = Arc::new(move |config| {
            let calls_tx = calls_tx.clone();
            Box::pin(async move {
                let (release_tx, release_rx) = oneshot::channel();
                calls_tx
                    .send(release_tx)
                    .expect("test receives start calls");
                release_rx.await.expect("test releases the start")?;
                real_starter(None)(config).await
            })
        });
        (publisher_with(starter).await, calls_rx)
    }

    async fn publisher_with(starter: Starter) -> Arc<OpcUaPublisher> {
        publisher_with_pki(starter, None).await
    }

    async fn publisher_with_pki(starter: Starter, pki_dir: Option<PathBuf>) -> Arc<OpcUaPublisher> {
        let pool = crate::db::migrate_memory().await.expect("migrate_memory");
        let computed = Arc::new(crate::computed::ComputedEngine::new(Arc::new(
            crate::computed::ServerTagStore::new(),
        )));
        let manager = Arc::new(CollectorManager::new(
            pool,
            std::env::temp_dir().join("banto-hub-opcua-publisher-unused"),
            Arc::new(banto_tstore::SystemClock),
            banto_collect::CollectorOptions::default(),
            Arc::new(crate::broker_glue::HubSessions::new(
                banto_broker::BackoffConfig::default(),
                crate::test_support::test_drivers(),
            )),
            Arc::new(crate::broker_glue::BrokerSimRegistry::new()),
            computed,
        ));
        OpcUaPublisher::with_starter(manager, starter, pki_dir)
    }

    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .expect("reserve port")
            .local_addr()
            .expect("port")
            .port()
    }

    fn enabled(port: u16) -> OpcUaSettings {
        OpcUaSettings {
            enabled: true,
            port,
            ..Default::default()
        }
    }

    fn disabled(port: u16) -> OpcUaSettings {
        OpcUaSettings {
            enabled: false,
            ..enabled(port)
        }
    }

    async fn next_call(calls: &mut mpsc::UnboundedReceiver<StartCall>) -> StartCall {
        tokio::time::timeout(Duration::from_secs(10), calls.recv())
            .await
            .expect("start is requested")
            .expect("starter alive")
    }

    async fn wait_state(publisher: &OpcUaPublisher, state: OpcUaState) -> OpcUaStatus {
        let mut status = publisher.status.subscribe();
        let reached = tokio::time::timeout(
            Duration::from_secs(60),
            status.wait_for(|status| status.state == state),
        )
        .await
        .unwrap_or_else(|_| panic!("OPC UA status did not reach {state:?}"))
        .expect("status channel")
        .clone();
        reached
    }

    /// #634: 有効化はすぐ返り、起動中 → 起動済みに移る。無効化で停止に戻る。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn apply_returns_before_a_slow_start_and_reports_running() {
        let (publisher, mut calls) = gated_publisher().await;
        let port = free_port();
        tokio::time::timeout(Duration::from_secs(2), publisher.apply(&enabled(port)))
            .await
            .expect("apply must not wait for the listener start");
        let release = next_call(&mut calls).await;
        // 起動を止めたままでも状態は読め、起動中を示す。
        assert_eq!(publisher.status(), OpcUaStatus::starting(1));
        release.send(Ok(())).expect("release");
        let running = wait_state(&publisher, OpcUaState::Running).await;
        assert_eq!(running.generation, 1);
        assert!(running
            .endpoint
            .as_deref()
            .is_some_and(|endpoint| endpoint.contains(&format!(":{port}"))));
        assert!(publisher.endpoint_and_namespace().await.is_some());

        publisher.apply(&disabled(port)).await;
        assert_eq!(publisher.status(), OpcUaStatus::stopped(2));
        assert!(publisher.endpoint_and_namespace().await.is_none());
        publisher.shutdown().await;
    }

    /// #634: 起動の失敗は理由つきで状態に出る。同じ設定の保存で再試行する。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn start_failure_is_reported_with_its_reason_and_retried() {
        let (publisher, mut calls) = gated_publisher().await;
        let settings = enabled(free_port());
        publisher.apply(&settings).await;
        next_call(&mut calls)
            .await
            .send(Err("bind に失敗しました".to_owned()))
            .expect("release");
        assert_eq!(
            wait_state(&publisher, OpcUaState::Failed).await,
            OpcUaStatus::failed(1, "bind に失敗しました".to_owned())
        );

        publisher.apply(&settings).await;
        assert_eq!(publisher.status(), OpcUaStatus::starting(2));
        next_call(&mut calls)
            .await
            .send(Err("まだ使用中".to_owned()))
            .expect("release");
        assert_eq!(
            wait_state(&publisher, OpcUaState::Failed).await,
            OpcUaStatus::failed(2, "まだ使用中".to_owned())
        );
        publisher.shutdown().await;
    }

    /// #634: 起動中に無効化が来たら、古い起動の失敗は状態を上書きしない。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn disable_during_start_ignores_the_stale_failure() {
        let (publisher, mut calls) = gated_publisher().await;
        let port = free_port();
        publisher.apply(&enabled(port)).await;
        let release = next_call(&mut calls).await;
        publisher.apply(&disabled(port)).await;
        assert_eq!(publisher.status(), OpcUaStatus::stopped(2));
        release
            .send(Err("古い起動の失敗".to_owned()))
            .expect("release");
        publisher.join_start_task_for_test().await;
        assert_eq!(publisher.status(), OpcUaStatus::stopped(2));
        publisher.shutdown().await;
    }

    /// #634: 起動中に無効化が来たら、古い起動で開いた listener は止め、
    /// 起動済みにしない。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn disable_during_start_closes_the_stale_listener() {
        let (publisher, mut calls) = gated_publisher().await;
        let port = free_port();
        publisher.apply(&enabled(port)).await;
        let release = next_call(&mut calls).await;
        publisher.apply(&disabled(port)).await;
        release.send(Ok(())).expect("release");
        publisher.join_start_task_for_test().await;
        assert_eq!(publisher.status(), OpcUaStatus::stopped(2));
        assert!(publisher.endpoint_and_namespace().await.is_none());
        assert!(tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_err());
        publisher.shutdown().await;
    }

    /// #634: 起動中の再設定は前の起動が終わってから始まり、後の要求が勝つ。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reconfigure_during_start_waits_for_the_previous_start_and_latest_wins() {
        let (publisher, mut calls) = gated_publisher().await;
        publisher.apply(&enabled(free_port())).await;
        let first = next_call(&mut calls).await;
        publisher.apply(&enabled(free_port())).await;
        assert_eq!(publisher.status(), OpcUaStatus::starting(2));
        // 前の起動が終わるまで、次の起動（bind）は始まらない。
        assert!(
            tokio::time::timeout(Duration::from_millis(300), calls.recv())
                .await
                .is_err(),
            "the second start must wait for the first one"
        );
        first
            .send(Err("古い起動の失敗".to_owned()))
            .expect("release");
        let second = next_call(&mut calls).await;
        assert_eq!(publisher.status(), OpcUaStatus::starting(2));
        second
            .send(Err("新しい起動の失敗".to_owned()))
            .expect("release");
        assert_eq!(
            wait_state(&publisher, OpcUaState::Failed).await,
            OpcUaStatus::failed(2, "新しい起動の失敗".to_owned())
        );
        publisher.shutdown().await;
    }

    /// PR #650 のオーナーレビュー: 起動タスクが panic したら `starting` に
    /// 残さず `failed`（理由つき）にする。同じ設定の保存で再試行できる。
    ///
    /// 反証: `fail_after_panic` の呼び出しを外すと、状態が `starting` のまま
    /// `wait_state(Failed)` が時間切れで落ちる。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_panicking_start_becomes_failed_and_can_be_retried() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let starter: Starter = Arc::new({
            let calls = calls.clone();
            move |_config| {
                let call = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Box::pin(async move {
                    if call == 0 {
                        panic!("鍵の生成で想定外の失敗");
                    }
                    Err("二度目は普通の失敗".to_owned())
                })
            }
        });
        let publisher = publisher_with(starter).await;
        let settings = enabled(free_port());
        publisher.apply(&settings).await;
        let failed = wait_state(&publisher, OpcUaState::Failed).await;
        assert_eq!(failed.generation, 1);
        assert!(
            failed
                .error
                .as_deref()
                .is_some_and(|error| error.contains("panic") && error.contains("鍵の生成")),
            "{failed:?}"
        );
        assert!(publisher.wait_settled_within(Duration::from_secs(1)).await);

        publisher.apply(&settings).await;
        assert_eq!(
            wait_state(&publisher, OpcUaState::Failed).await,
            OpcUaStatus::failed(2, "二度目は普通の失敗".to_owned())
        );
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
        publisher.shutdown().await;
    }

    /// PR #650 のオーナーレビュー: 起動の待ちには有限の上限がある（起動が
    /// 終わらなくても `initialize` が戻る）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn waiting_for_a_hung_start_is_bounded() {
        let (publisher, mut calls) = gated_publisher().await;
        publisher.apply(&enabled(free_port())).await;
        let release = next_call(&mut calls).await;
        assert!(
            !publisher
                .wait_settled_within(Duration::from_millis(200))
                .await
        );
        assert_eq!(publisher.status().state, OpcUaState::Starting);
        release.send(Err("後始末".to_owned())).expect("release");
        publisher.shutdown().await;
    }

    /// #634: 起動中の Hub の終了は起動の終わりを待ち、listener を残さない。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shutdown_during_start_waits_and_leaves_no_listener() {
        let (publisher, mut calls) = gated_publisher().await;
        let port = free_port();
        publisher.apply(&enabled(port)).await;
        let release = next_call(&mut calls).await;
        let stopping = tokio::spawn({
            let publisher = publisher.clone();
            async move { publisher.shutdown().await }
        });
        release.send(Ok(())).expect("release");
        tokio::time::timeout(Duration::from_secs(60), stopping)
            .await
            .expect("shutdown finishes")
            .expect("shutdown task");
        assert_eq!(publisher.status().state, OpcUaState::Stopped);
        assert!(publisher.endpoint_and_namespace().await.is_none());
        assert!(tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_err());
    }

    /// #634: 鍵と証明書の置き場を渡すと、2 回目の起動は同じ鍵と証明書を再利用する
    /// （ファイルの中身が同じ。初回だけ生成するので 2 回目は速い）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_second_start_reuses_the_key_and_certificate() {
        let root = crate::test_support::TempDir::new("opcua-pki-reuse");
        let pki = crate::opcua_pki::pki_dir(&root.path().join("data"));
        let publisher = publisher_with_pki(real_starter(None), Some(pki.clone())).await;
        let port = free_port();
        let first_started = tokio::time::Instant::now();
        publisher.apply(&enabled(port)).await;
        wait_state(&publisher, OpcUaState::Running).await;
        let first_elapsed = first_started.elapsed();
        let cert = pki.join("own").join("cert.der");
        let key = pki.join("private").join("private.pem");
        let first = (std::fs::read(&cert).unwrap(), std::fs::read(&key).unwrap());
        assert!(!first.0.is_empty() && !first.1.is_empty());

        publisher.apply(&disabled(port)).await;
        let second_started = tokio::time::Instant::now();
        publisher.apply(&enabled(port)).await;
        let running = wait_state(&publisher, OpcUaState::Running).await;
        let second_elapsed = second_started.elapsed();
        assert_eq!(running.generation, 3);
        let second = (std::fs::read(&cert).unwrap(), std::fs::read(&key).unwrap());
        assert_eq!(first, second, "鍵と証明書は作り直さずに再利用する");
        eprintln!("OPC UA の起動: 初回 {first_elapsed:?}、2 回目 {second_elapsed:?}");
        publisher.shutdown().await;
    }

    /// #634: banto-opcua の `Error::Pki`（ここでは証明書の場所がフォルダで、片方だけの
    /// ファイルとして消せない）は `failed` と理由（置き場と再試行のしかた）になる。原因を
    /// 取り除くと、同じ設定の保存で起動できる。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_pki_error_is_reported_and_the_same_settings_retry_after_the_fix() {
        let root = crate::test_support::TempDir::new("opcua-pki-error");
        let pki = crate::opcua_pki::pki_dir(&root.path().join("data"));
        crate::opcua_pki::prepare(&pki, None).expect("pki dir");
        let blocker = pki.join("own").join("cert.der");
        std::fs::create_dir_all(blocker.join("blocker")).unwrap();
        let publisher = publisher_with_pki(real_starter(None), Some(pki.clone())).await;
        let settings = enabled(free_port());
        publisher.apply(&settings).await;
        let failed = wait_state(&publisher, OpcUaState::Failed).await;
        let reason = failed.error.expect("reason");
        assert!(reason.contains("証明書ディレクトリ"), "{reason}");
        assert!(reason.contains(&pki.display().to_string()), "{reason}");
        assert!(reason.contains("再試行"), "{reason}");

        std::fs::remove_dir_all(&blocker).unwrap();
        publisher.apply(&settings).await;
        let running = wait_state(&publisher, OpcUaState::Running).await;
        assert_eq!(running.generation, 2);
        assert!(pki.join("private").join("private.pem").is_file());
        publisher.shutdown().await;
    }

    /// #634: 置き場に広い ACE（Users）があると、鍵を作らずに `failed` になり、理由が状態に
    /// 出る。ACE を外すと、同じ設定の保存で起動できる。
    #[cfg(windows)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_broad_ace_on_the_pki_dir_fails_without_a_key_and_retries_after_the_fix() {
        let root = crate::test_support::TempDir::new("opcua-pki-broad-ace");
        let pki = crate::opcua_pki::pki_dir(&root.path().join("data"));
        crate::opcua_pki::prepare(&pki, None).expect("pki dir");
        let icacls = |args: &[&str]| {
            let output = std::process::Command::new("icacls")
                .arg(&pki)
                .args(args)
                .output()
                .expect("icacls");
            assert!(output.status.success(), "icacls {args:?}: {output:?}");
        };
        icacls(&["/grant", "*S-1-5-32-545:(OI)(CI)(R)"]);
        let publisher = publisher_with_pki(real_starter(None), Some(pki.clone())).await;
        let settings = enabled(free_port());
        publisher.apply(&settings).await;
        let failed = wait_state(&publisher, OpcUaState::Failed).await;
        let reason = failed.error.expect("reason");
        assert!(reason.contains("Users（S-1-5-32-545）"), "{reason}");
        assert!(reason.contains("鍵を作らず"), "{reason}");
        assert!(
            !pki.join("private").join("private.pem").exists(),
            "鍵を作らない"
        );

        icacls(&["/remove:g", "*S-1-5-32-545"]);
        publisher.apply(&settings).await;
        wait_state(&publisher, OpcUaState::Running).await;
        assert!(pki.join("private").join("private.pem").is_file());
        publisher.shutdown().await;
    }

    #[test]
    fn initial_missing_source_stays_unknown() {
        let reading = tag_reading(
            DataType::Double,
            CurrentView {
                value: None,
                quality: Quality::Bad,
                timestamp_ms: 100,
                source_time_ms: None,
                quality_since_ms: None,
            },
        );
        assert_eq!(reading.timestamps.source, None);
    }

    #[test]
    fn stale_uses_quality_transition_not_old_good_sample() {
        let reading = tag_reading(
            DataType::Boolean,
            CurrentView {
                value: Some(1.0),
                quality: Quality::Stale,
                timestamp_ms: 200,
                source_time_ms: Some(100),
                quality_since_ms: Some(175),
            },
        );
        assert_eq!(reading.timestamps.source, system_time(175));
        assert_eq!(reading.quality, UaQuality::Stale);
        assert_eq!(reading.value, Some(1.0));
    }
}
