//! T14-2 collection lifecycle state machine.
//!
//! `CollectionController` is deliberately a thin layer above
//! [`crate::hub::CollectorManager`]. The manager keeps ownership of the
//! collector, broker sessions, simulators, and computed engine; this module
//! owns only lifecycle state, transition serialization, and run identifiers.
//!
//! 外部 DB 連携 S2b（docs/banto-hub-external-db-design.md §4.8・§6-16、
//! 2026-09-06 オーナー決定）以降、ここは「収集が Running か」に連動して
//! 動く**唯一の**もう1つの機構、`crate::db_source::DbSourceEngine` の
//! 起動・停止点でもある（`CollectionController::db_source` のフィールド
//! doc comment に、なぜ `CollectorManager` 側ではなくここなのかを書いて
//! ある）。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use banto_collect::RegistrySnapshot;
use serde::Serialize;
use tokio::sync::watch;
use tokio::sync::Mutex as AsyncMutex;

use crate::db_source::DbSourceEngine;
use crate::hub::CollectorManager;
use crate::test_output::TestOutputControl;

/// A collection lifecycle state.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CollectionState {
    Stopped,
    Starting,
    Running,
    Stopping,
    Faulted,
}

impl CollectionState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stopped => "stopped",
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Stopping => "stopping",
            Self::Faulted => "faulted",
        }
    }
}

/// The configured collection mode. `AllSimulation` applies a non-persistent
/// simulation override to enabled physical connections for one run.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunMode {
    Configured,
    AllSimulation,
}

impl RunMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Configured => "configured",
            Self::AllSimulation => "all_simulation",
        }
    }
}

/// Monotonically allocated identifier for one start attempt.
pub type RunId = u64;

/// Identity of the current start attempt while it is starting, running, or
/// faulted. A stopped controller has no active run context.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct RunContext {
    pub mode: RunMode,
    pub run_id: RunId,
}

/// Snapshot returned by lifecycle operations and available through
/// [`CollectionController::status`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CollectionStatus {
    pub state: CollectionState,
    pub mode: RunMode,
    pub run_id: Option<RunId>,
    pub last_error: Option<String>,
    pub configured_revision: u64,
    pub running_revision: u64,
}

/// Public name used by runtime/status consumers for the lifecycle snapshot.
pub type RuntimeStatus = CollectionStatus;

/// The serialized collection lifecycle controller.
pub struct CollectionController {
    manager: Arc<CollectorManager>,
    /// T15-3（設計 §6.3）: `start_locked`/`stop_locked`/モード切替の遷移点で
    /// `disable()`する - テスト出力が「現在の run コンテキストのみ」に
    /// 留まることをここで保証する（`crate::test_output`のモジュール doc
    /// comment参照）。2026-09-09 オーナー決定（#340）で `write_control` は
    /// これらの遷移点から外れたが、`test_output` はここで引き続き連動する
    /// （T15-3、変更なし）。
    test_output: Arc<TestOutputControl>,
    /// 外部 DB 連携 S2b（docs/banto-hub-external-db-design.md §4.8・§6-16、
    /// 2026-09-06 オーナー決定「DB Source は収集が Running のときだけ
    /// 動く」）: `manager` が所有する `Arc` の複製
    /// （[`CollectorManager::db_source_engine`]）。
    ///
    /// **ここに置く理由**: 「収集が Running か」の唯一の権威はこの
    /// controller であり、Running へ入る・出る遷移は必ず
    /// [`Self::start_locked`]/[`Self::stop_locked`] のどちらかを通る
    /// （`start`/`stop`/`set_mode` の3つの公開入口 - REST の
    /// `POST /api/collection/{start,start-simulation,stop}` と
    /// `PUT /api/collection/mode`、MCP の `collection_control`、Windows
    /// サービス起動時の自動 start がすべてそこへ集まる）。`CollectorManager`
    /// 側（`apply_run`/`stop`）に置くと、収集の遷移ではない
    /// [`Self::commit_catalog_and_apply_live`]（#341 の live re-apply。
    /// 2026-09-14 より前は `commit_catalog_and_notify` の legacy live
    /// reconfigure）も `apply_run` を呼ぶため、「停止中なのに DB へ繋ぐ」
    /// 経路が1本残ってしまう。なお `commit_catalog_and_apply_live` は
    /// `Running` のときしか `apply_run` を呼ばないので、DB Source の
    /// task を起こす/落とす点が増えることはない（plan の入れ替えは
    /// `commit_catalog` が担当し、走行中なら接続単位の差分で task を
    /// 作り直す - `crate::db_source` のモジュール doc comment 参照）。
    db_source: Arc<DbSourceEngine>,
    state: Mutex<ControllerState>,
    transition: AsyncMutex<()>,
    run_seq: AtomicU64,
    status_tx: watch::Sender<RuntimeStatus>,
}

#[derive(Clone, Debug)]
struct ControllerState {
    state: CollectionState,
    mode: RunMode,
    context: Option<RunContext>,
    last_error: Option<String>,
}

impl CollectionController {
    pub fn new(manager: Arc<CollectorManager>, test_output: Arc<TestOutputControl>) -> Self {
        let initial = CollectionStatus {
            state: CollectionState::Stopped,
            mode: RunMode::Configured,
            run_id: None,
            last_error: None,
            configured_revision: manager.configured_revision(),
            running_revision: manager.running_revision(),
        };
        let (status_tx, _status_rx) = watch::channel(initial);
        let db_source = manager.db_source_engine();
        Self {
            manager,
            test_output,
            db_source,
            state: Mutex::new(ControllerState {
                state: CollectionState::Stopped,
                mode: RunMode::Configured,
                context: None,
                last_error: None,
            }),
            transition: AsyncMutex::new(()),
            run_seq: AtomicU64::new(0),
            status_tx,
        }
    }

    /// T15-3: `crate::rest`の`GET /api/v1/status`の`test_output`欄と
    /// `POST /api/test-output/enable|disable`ハンドラのため。
    pub fn test_output(&self) -> Arc<TestOutputControl> {
        self.test_output.clone()
    }

    /// Return the current state without waiting for an in-flight transition.
    pub fn status(&self) -> CollectionStatus {
        let mut status = self
            .state
            .lock()
            .expect("collection controller state lock poisoned")
            .status();
        status.configured_revision = self.manager.configured_revision();
        status.running_revision = self.manager.running_revision();
        status
    }

    /// Subscribe to lifecycle transitions. Catalog-only commits do not start
    /// a run and therefore do not cause a running transition notification.
    pub fn subscribe_status(&self) -> watch::Receiver<RuntimeStatus> {
        self.status_tx.subscribe()
    }

    /// Refresh the status watch after an external catalog-only commit.
    pub fn refresh_status(&self) {
        self.status_tx.send_replace(self.status());
    }

    /// #341（オーナー決定 2026-09-09 / 2026-09-14、`docs/tag-server-design.md`
    /// §4.3）: 直前にコミットされたレジストリ変更を catalog へ反映し、収集が
    /// 実際に `Running` なら**収集を止めずに実行構成へも反映する** -
    /// [`crate::rest::commit_catalog_and_notify`]（REST/MCP の全レジストリ
    /// mutation と `execute_pending_apply` が必ず通る唯一の点）専用の
    /// primitive で、他に呼び出し元を作らないこと。
    ///
    /// # なぜ `commit_catalog` + `apply_run` で、`rebuild` ではないのか
    ///
    /// - [`CollectorManager::commit_catalog`] が catalog・演算タグ plan・
    ///   DB Source plan を同じ点で入れ替え、`configured_revision` を進めて
    ///   `config_changed`（`revision_tx`）を飛ばす = 外部クライアントが
    ///   catalog を取り直す契機（設計 §4.1）。
    /// - [`CollectorManager::apply_run`] が `runtime_snapshot_for_mode` で
    ///   **今の run mode のオーバーライドを尊重した**構成を組み立ててから
    ///   [`banto_collect::Collector::apply_config`]（T7 の接続単位の部分
    ///   再構成）へ通し、broker セッションの追加/削除も同じ順序
    ///   （collect タスク停止 → セッション削除）で同期する。
    /// - [`CollectorManager::rebuild`] は run mode を一切見ず
    ///   `RegistrySnapshot` をそのまま `build_config_from` へ渡すため、
    ///   `AllSimulation` で稼働中にこれを呼ぶと**シミュレーション中のはずの
    ///   接続が実機へダイヤルされる**。live re-apply の primitive には
    ///   絶対に使えない（これが `apply_run` を選んだ決定的な理由）。
    ///
    /// この構成により、T19 S2-a 案B の `resync_sessions_for_catalog_change`
    /// （catalog-only commit のときに broker セッションだけ同期していた、
    /// #341 で撤去）が抱えていた「最後のグループを失った接続の collect
    /// タスクが、セッション削除後もしばらく古いセッションを読み続けうる」
    /// 隙間も閉じる - `apply_run` は collector 側のコミット（= 当該タスクの
    /// 停止）が終わってから `remove_stale_broker_sessions` する。
    ///
    /// # 並行性の前提と保証
    ///
    /// - **`start`/`stop`/`set_mode` と同じ `transition` ロックを取る**
    ///   （`try_lock` ではなく `lock().await` - 進行中の遷移が終わるのを
    ///   待って必ず適用する。`try_lock` で諦めると「DB にはあるのに走って
    ///   いる構成には無い」状態を無言で残してしまい、#341 が塞ごうとして
    ///   いる穴そのものになる）。ロック順序は常に `transition` →
    ///   `CollectorManager::rebuild_lock` の一方向で、逆順に取る経路は
    ///   存在しない（`rebuild`/`commit_catalog`/`apply_run` はこの
    ///   controller を一切参照しない）ためデッドロックしない。呼び出し元
    ///   （REST ハンドラ）は DB トランザクションを既にコミット済みで、
    ///   他のロックを保持していないことも前提。
    /// - 逆に、この適用中に届いた `start`/`stop` は（両者の `try_lock` が
    ///   外れて）現在の状態を返すだけの no-op になる - 撤去した
    ///   `resync_sessions_for_catalog_change` と同じ既知の性質で、
    ///   `CollectorManager::stop` は `rebuild_lock` を取らないため、
    ///   「stop が落としたセッションを直後に張り直す」T15-4 型の事故を
    ///   防いでいるのはこの `transition` ロックだけである。
    /// - **適用順序**: `commit_catalog` と `apply_run` の対がこのロックで
    ///   直列化されるので、連続した CRUD が適用を追い越し合うことはない。
    ///   `apply_run` は自分でレジストリを読み直す（`RegistrySnapshot::load`）
    ///   ため、**最後に走った適用は常に最新の DB 状態を反映する**。
    /// - 既知の（#341 以前からある）狭い窓: `commit_catalog` に渡すのは
    ///   呼び出し元のトランザクション内 snapshot なので、2つの CRUD が
    ///   「A が tx commit → B が tx commit → B が catalog commit → A が
    ///   catalog commit」の順に並ぶと catalog だけ一瞬 B の行を欠く。
    ///   collector 側は `apply_run` の読み直しで常に最新、次のどの CRUD の
    ///   catalog commit でも解消する。#341 はこの既存の性質を変えない。
    ///
    /// # 失敗時
    ///
    /// `Err` は「DB には入ったが実行構成へ反映できなかった」ことだけを
    /// 意味する（レジストリ行そのものは既にコミット済み）。`apply_config`
    /// は all-or-nothing なので走行中の収集は元の構成のまま無傷で、
    /// 状態も `Running` のまま（`Faulted` にはしない - 収集自体は正常に
    /// 続いている）。`CollectorManager` 側が `last_error`
    /// （`/api/v1/status` の `last_config_error`）に記録し、呼び出し元は
    /// これを API のエラーとして返す。
    ///
    /// `Running` 以外（`Stopped`/`Starting`/`Stopping`/`Faulted`）では
    /// catalog だけをコミットして返る - 停止中の catalog commit で PLC へ
    /// ダイヤルしてしまう T15-4 型の事故を起こさないため、**`Running` の
    /// 確認は必ずこのロックの下で行う**。
    pub async fn commit_catalog_and_apply_live(
        &self,
        snapshot: &RegistrySnapshot,
    ) -> Result<(), String> {
        let _guard = self.transition.lock().await;
        self.manager.commit_catalog(snapshot).await?;
        let current = self.status();
        if current.state != CollectionState::Running {
            return Ok(());
        }
        self.manager.apply_run(current.mode).await
    }

    /// Start the requested mode. A request arriving during another transition
    /// is not queued; it returns the state already published by that
    /// transition. Starting the same mode while running is idempotent.
    pub async fn start(&self, mode: RunMode) -> CollectionStatus {
        let Ok(_guard) = self.transition.try_lock() else {
            return self.status();
        };

        if self.status().state == CollectionState::Starting
            || self.status().state == CollectionState::Stopping
        {
            return self.status();
        }

        if self.status().state == CollectionState::Running && self.status().mode == mode {
            return self.status();
        }

        if self.status().state == CollectionState::Running {
            self.stop_locked().await;
        }

        self.start_locked(mode).await
    }

    /// Stop the current run. The collector is flushed first, then every
    /// broker session is stopped and joined. Repeated stops are no-ops.
    pub async fn stop(&self) -> CollectionStatus {
        let Ok(_guard) = self.transition.try_lock() else {
            return self.status();
        };

        if matches!(
            self.status().state,
            CollectionState::Stopped | CollectionState::Stopping
        ) {
            return self.status();
        }

        self.stop_locked().await;
        self.status()
    }

    /// Select a mode while stopped. A running mode switch is serialized as
    /// stop → stopped → start, so configured and all-simulation never switch
    /// in place.
    pub async fn set_mode(&self, mode: RunMode) -> CollectionStatus {
        let Ok(_guard) = self.transition.try_lock() else {
            return self.status();
        };

        let current = self.status();
        if current.state == CollectionState::Stopped {
            if current.mode != mode {
                self.test_output.disable();
            }
            self.set_mode_locked(mode);
            self.publish_status();
            return self.status();
        }
        if current.state == CollectionState::Running && current.mode == mode {
            return current;
        }
        if matches!(
            current.state,
            CollectionState::Starting | CollectionState::Stopping
        ) {
            return current;
        }

        self.stop_locked().await;
        self.set_mode_locked(mode);
        self.start_locked(mode).await
    }

    fn set_mode_locked(&self, mode: RunMode) {
        let mut state = self
            .state
            .lock()
            .expect("collection controller state lock poisoned");
        state.mode = mode;
        state.context = None;
        state.last_error = None;
    }

    async fn start_locked(&self, mode: RunMode) -> CollectionStatus {
        let run_id = self.run_seq.fetch_add(1, Ordering::SeqCst) + 1;
        let context = RunContext { mode, run_id };
        {
            let mut state = self
                .state
                .lock()
                .expect("collection controller state lock poisoned");
            state.state = CollectionState::Starting;
            state.mode = mode;
            state.context = Some(context);
            state.last_error = None;
        }
        self.test_output.disable();
        self.publish_status();

        let result = self.manager.apply_run(mode).await;

        let mut state = self
            .state
            .lock()
            .expect("collection controller state lock poisoned");
        let started = match result {
            Ok(()) => {
                state.state = CollectionState::Running;
                state.last_error = None;
                true
            }
            Err(error) => {
                state.state = CollectionState::Faulted;
                state.last_error = Some(error);
                false
            }
        };
        drop(state);
        // 外部 DB 連携 S2b（§4.8・§6-16）: 収集が実際に Running になった
        // ときだけ DB Source のタスクを起こす。`Faulted`（`apply_run` が
        // 失敗して収集が始まらなかった）では起こさない - 「収集開始で
        // 起動」の意味は「Running になったら」であり、片系だけ動いている
        // 状態を作らないため。決定 §4.8「片方が落ちても Running 状態は
        // 変わらない」の裏返しで、そもそも Running になっていない。
        if started {
            self.db_source.start();
        }
        self.publish_status();
        self.status()
    }

    async fn stop_locked(&self) {
        {
            let mut state = self
                .state
                .lock()
                .expect("collection controller state lock poisoned");
            state.state = CollectionState::Stopping;
        }
        self.test_output.disable();
        self.publish_status();
        // 外部 DB 連携 S2b（§4.8・§6-16）: `manager.stop()` より**前**に
        // 止める - DB Source のタスクは `ServerTagStore` への書き手なので、
        // `crate::runtime::RunningHub::shutdown` が `db_source.shutdown()` を
        // `manager.shutdown()` の前に置いているのと同じ順序（そちらの
        // モジュール doc comment「シャットダウン順序」節）。`stop` は
        // タスクを join し終えてから計画上の全 `db` タグへ Bad を書くので、
        // これが返った時点で読み手が古い Good を見ることはない。
        self.db_source.stop().await;
        self.manager.stop().await;
        self.manager.advance_running_revision();
        let mut state = self
            .state
            .lock()
            .expect("collection controller state lock poisoned");
        state.state = CollectionState::Stopped;
        state.context = None;
        state.last_error = None;
        drop(state);
        self.publish_status();
    }

    fn publish_status(&self) {
        self.status_tx.send_replace(self.status());
    }
}

impl ControllerState {
    fn status(&self) -> CollectionStatus {
        CollectionStatus {
            state: self.state,
            mode: self.mode,
            run_id: self.context.map(|context| context.run_id),
            last_error: self.last_error.clone(),
            configured_revision: 0,
            running_revision: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broker_glue::{HubSessions, SlmpSimRegistry};
    use crate::computed::{ComputedEngine, ServerTagStore};
    use crate::db::init_db;
    use banto_collect::{CollectorOptions, RegistrySnapshot};
    use banto_tstore::SystemClock;
    use std::time::Duration;

    async fn controller_env() -> (crate::test_support::TempDir, Arc<CollectionController>) {
        let dir = crate::test_support::TempDir::new("collection-controller");
        let pool = init_db(&dir.path().join("registry.sqlite3"))
            .await
            .expect("init_db");
        let manager = Arc::new(CollectorManager::new(
            pool,
            dir.path().join("data"),
            Arc::new(SystemClock),
            CollectorOptions {
                connect_timeout: Duration::from_millis(50),
                response_timeout: Duration::from_millis(50),
                ..CollectorOptions::default()
            },
            Arc::new(HubSessions::new(banto_broker::BackoffConfig::default())),
            Arc::new(SlmpSimRegistry::new()),
            Arc::new(ComputedEngine::new(Arc::new(ServerTagStore::new()))),
        ));
        let test_output = Arc::new(TestOutputControl::new());
        let controller = Arc::new(CollectionController::new(manager, test_output));
        (dir, controller)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn start_stop_is_idempotent_and_run_ids_are_not_reused() {
        let (_dir, controller) = controller_env().await;

        let first = controller.start(RunMode::Configured).await;
        assert_eq!(first.state, CollectionState::Running);
        assert_eq!(first.run_id, Some(1));

        let repeated = controller.start(RunMode::Configured).await;
        assert_eq!(repeated, first);

        let stopped = controller.stop().await;
        assert_eq!(stopped.state, CollectionState::Stopped);
        assert_eq!(stopped.run_id, None);

        let second = controller.start(RunMode::Configured).await;
        assert_eq!(second.state, CollectionState::Running);
        assert_eq!(second.run_id, Some(2));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn all_simulation_starts_without_auto_restart() {
        let (_dir, controller) = controller_env().await;

        let running = controller.start(RunMode::AllSimulation).await;
        assert_eq!(running.state, CollectionState::Running);
        assert_eq!(running.run_id, Some(1));
        assert!(running.last_error.is_none());
        assert_eq!(controller.status(), running);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn switching_from_configured_to_all_simulation_starts_the_new_mode() {
        let (_dir, controller) = controller_env().await;
        let running = controller.start(RunMode::Configured).await;
        assert_eq!(running.state, CollectionState::Running);
        assert_eq!(running.run_id, Some(1));

        let switched = controller.set_mode(RunMode::AllSimulation).await;

        // The mode switch must pass through Stopping → Stopped → Starting.
        assert_eq!(switched.state, CollectionState::Running);
        assert_eq!(switched.mode, RunMode::AllSimulation);
        assert_eq!(switched.run_id, Some(2));
        assert!(switched.last_error.is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_starts_do_not_allocate_two_running_ids() {
        let (_dir, controller) = controller_env().await;
        let left = controller.clone();
        let right = controller.clone();
        let (left, right) = tokio::join!(
            left.start(RunMode::Configured),
            right.start(RunMode::Configured)
        );

        assert!(matches!(
            left.state,
            CollectionState::Starting | CollectionState::Running
        ));
        assert!(matches!(
            right.state,
            CollectionState::Starting | CollectionState::Running
        ));
        let final_status = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let status = controller.status();
                if status.state != CollectionState::Starting {
                    break status;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the first start should complete");
        assert_eq!(final_status.state, CollectionState::Running);
        assert_eq!(final_status.run_id, Some(1));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn configured_and_running_revisions_are_separate() {
        let (_dir, controller) = controller_env().await;
        let snapshot = RegistrySnapshot::load(&controller.manager.pool())
            .await
            .expect("empty registry snapshot");

        assert_eq!(controller.status().configured_revision, 0);
        assert_eq!(controller.status().running_revision, 0);
        controller
            .manager
            .commit_catalog(&snapshot)
            .await
            .expect("catalog commit");
        controller.refresh_status();

        let stopped = controller.status();
        assert_eq!(stopped.state, CollectionState::Stopped);
        assert_eq!(stopped.configured_revision, 1);
        assert_eq!(stopped.running_revision, 0);
        assert!(controller.manager.current_values().is_none());

        let running = controller.start(RunMode::Configured).await;
        assert_eq!(running.state, CollectionState::Running);
        assert_eq!(running.configured_revision, 1);
        assert_eq!(running.running_revision, 1);

        let stopped = controller.stop().await;
        assert_eq!(stopped.state, CollectionState::Stopped);
        assert_eq!(stopped.configured_revision, 1);
        assert_eq!(stopped.running_revision, 2);
    }

    /// T15-3（設計 §6.3「停止／終了／切替／サービス再起動後に必ず無効へ
    /// 戻る」）: `start_locked`/`stop_locked`/停止中のモード切替の遷移点で
    /// `test_output`は必ず disable される（write_control は 2026-09-09
    /// オーナー決定 #340 でこれらの遷移点から外れたが、test_output の
    /// 連動は変更なし）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_output_auto_disables_on_every_lifecycle_transition() {
        let (_dir, controller) = controller_env().await;
        let test_output = controller.test_output();

        let running = controller.start(RunMode::AllSimulation).await;
        assert_eq!(running.state, CollectionState::Running);
        let run_id = running.run_id.expect("running has a run_id");
        test_output.enable(run_id);
        assert!(test_output.is_active_for(Some(run_id)));

        // 新規 start (別 run_id への切替) - 既存 run 用のアーミングは
        // 引き継がれない。
        let restarted = controller.start(RunMode::Configured).await;
        assert_eq!(restarted.state, CollectionState::Running);
        assert_ne!(restarted.run_id, running.run_id);
        assert!(!test_output.is_enabled());
        assert!(!test_output.is_active_for(restarted.run_id));

        test_output.enable(restarted.run_id.expect("running has a run_id"));
        assert!(test_output.is_enabled());
        let stopped = controller.stop().await;
        assert_eq!(stopped.state, CollectionState::Stopped);
        assert!(!test_output.is_enabled(), "stop must disable test_output");

        // 停止中のモード切替も disable する(要件「モード切替」)。
        controller.set_mode(RunMode::AllSimulation).await;
        let running_again = controller.start(RunMode::AllSimulation).await;
        test_output.enable(running_again.run_id.expect("running has a run_id"));
        assert!(test_output.is_enabled());
        controller.set_mode(RunMode::Configured).await;
        assert!(
            !test_output.is_enabled(),
            "a running->stopped mode switch must disable test_output"
        );
    }
}
