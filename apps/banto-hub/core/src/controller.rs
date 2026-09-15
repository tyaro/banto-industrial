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

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use banto_collect::RegistrySnapshot;
use serde::Serialize;
use tokio::sync::watch;
use tokio::sync::Mutex as AsyncMutex;

use crate::db_source::DbSourceEngine;
use crate::hub::CollectorManager;

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
    /// #341 レビュー対応2（2026-09-14）: `transition` を保持しているのが
    /// **live apply**（[`Self::commit_catalog_and_apply_live`]）かどうか。
    ///
    /// ライフサイクル操作（[`Self::start`]/[`Self::stop`]/[`Self::set_mode`]）は
    /// 「遷移中に重ねて来た要求は待たずに現在状態を返す」という冪等契約
    /// （docs/banto-hub-desktop-plan.md）を持つため `try_lock` する。ところが
    /// #341 で live apply が同じロックを**保持して待つ**ようになった結果、
    /// live apply 中（PLC へのダイヤルを含み数秒かかりうる）に運用者の
    /// 「停止」が来ると黙って無視され、収集が続いてしまう。
    ///
    /// そこで、ロックが取れなかった相手が live apply のときだけ**待って
    /// から**実行する（[`Self::acquire_transition`]）。相手が本物の
    /// ライフサイクル遷移（start/stop/set_mode）のときは従来どおり
    /// no-op のまま - そちらは「同じことを二重にやらない」ための冪等契約
    /// そのもので、待って実行すると start→start が2回走ってしまう。
    live_apply_in_progress: AtomicBool,
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
    pub fn new(manager: Arc<CollectorManager>) -> Self {
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
            db_source,
            state: Mutex::new(ControllerState {
                state: CollectionState::Stopped,
                mode: RunMode::Configured,
                context: None,
                last_error: None,
            }),
            transition: AsyncMutex::new(()),
            live_apply_in_progress: AtomicBool::new(false),
            run_seq: AtomicU64::new(0),
            status_tx,
        }
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
    /// - **古い snapshot の窓は閉じてある**（#341 レビュー対応、
    ///   2026-09-14）: catalog へコミットするのは呼び出し元のトランザク
    ///   ション内 snapshot ではなく、**この `transition` ロックを取った後に
    ///   読み直した最新の `RegistrySnapshot`**。呼び出し元の in-tx snapshot
    ///   は preflight（保存前検証）専用で、反映には使わない。これが無いと
    ///   「A が tx commit → B が tx commit → B が catalog commit → A が
    ///   catalog commit」の順に並んだときに catalog だけ B の行を欠く
    ///   （collector 側は `apply_run` が元から読み直しているので常に最新
    ///   ＝ catalog と collector が食い違う）。読み直しに失敗した場合は
    ///   何も変更せずに `Err` を返す。
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
    /// **ロールバックの範囲（#341 レビュー対応2、2026-09-14）**: `apply_run`
    /// が失敗した場合、
    /// (a) collector のタスク集合は `apply_config` の all-or-nothing で元の
    /// まま、(b) broker セッション集合は
    /// [`CollectorManager::apply_run`] 自身が直前の成功構成へ戻し、
    /// (c) catalog・演算タグ plan・DB Source plan は
    /// [`CollectorManager::rollback_catalog`] が直前の成功構成へ戻す。
    /// つまり失敗後の runtime 側は**全部まとめて旧構成**で一貫し、
    /// **DB だけが新**という状態になる（レジストリ行は保存済み）。
    /// 再反映の手段は `crate::rest::LIVE_APPLY_RECOVERY_HINT` のとおり
    /// （次の構成変更 / 収集の停止→開始 / `POST /api/collection/reapply`）。
    /// (c) のロールバックでも `configured_revision` は進み
    /// `config_changed` が飛ぶ - 新 catalog を取得済みのクライアントに
    /// 「戻った」ことを知らせる契機として意図的。
    ///
    /// `Running` 以外（`Stopped`/`Starting`/`Stopping`/`Faulted`）では
    /// catalog だけをコミットして返る - 停止中の catalog commit で PLC へ
    /// ダイヤルしてしまう T15-4 型の事故を起こさないため、**`Running` の
    /// 確認は必ずこのロックの下で行う**。
    pub async fn commit_catalog_and_apply_live(&self) -> Result<(), String> {
        // #341 レビュー対応2: ここから下で来た start/stop/set_mode は no-op では
        // なく「待って実行」させる（`live_apply_in_progress` のフィールド doc
        // comment・[`Self::acquire_transition`] 参照）。`_reset` の `Drop` が、
        // この後のどの早期 return でも必ず下ろす。
        //
        // **フラグを立てるのは `transition` を取る前**（#341 監査、2026-09-14）:
        // ローカル変数は宣言の逆順で drop されるので、ロックの後で宣言すると
        // 関数終了時に「フラグ解除 → ロック解放」の順になり、その隙間に来た
        // ライフサイクル要求が `try_lock` に失敗しつつフラグも false を見て
        // no-op に落ちる（塞いだはずの穴が幅マイクロ秒で残る）。先に立てれば
        // drop 順が「ロック解放 → フラグ解除」になり、解放直後の要求は
        // `try_lock` に成功するか、別遷移に取られていた場合でも余計に待つだけで
        // 済む。副作用は「ロック取得を待っている間に来た要求が、先行する別の
        // ライフサイクル遷移の完了まで余計に待つことがある」だけで、無害
        // （待った後にその時点の状態で判断するため、二重実行にはならない）。
        self.live_apply_in_progress.store(true, Ordering::SeqCst);
        let _reset = LiveApplyFlagGuard(&self.live_apply_in_progress);
        let _guard = self.transition.lock().await;
        // ロックを取った**後**に読み直す（上の「古い snapshot の窓」）。
        // `RegistrySnapshot::load` は読み取りだけなので、失敗しても catalog・
        // 実行構成のどちらにも副作用は無い。
        let snapshot = RegistrySnapshot::load(&self.manager.pool())
            .await
            .map_err(|err| format!("レジストリのスナップショット取得に失敗しました: {err}"))?;
        self.manager.commit_catalog(&snapshot).await?;
        let current = self.status();
        if current.state != CollectionState::Running {
            return Ok(());
        }
        if let Err(err) = self.manager.apply_run(current.mode).await {
            // #341 レビュー対応2（2026-09-14）: ここまでで catalog・演算 plan・
            // DB Source plan は**新構成**になっているが、収集側は
            // `apply_run` 自身のロールバック（collector は all-or-nothing、
            // broker セッションは `rollback_broker_sessions`）で**旧構成**の
            // まま。読み側と収集側が食い違ったままにしないため、catalog 側も
            // 直前の成功構成へ戻す（`CollectorManager::rollback_catalog` の
            // doc comment に保証の範囲と `config_changed` の意図を書いてある）。
            self.manager.rollback_catalog().await;
            return Err(err);
        }
        Ok(())
    }

    /// #341 レビュー対応2（2026-09-14）: ライフサイクル操作
    /// （[`Self::start`]/[`Self::stop`]/[`Self::set_mode`]）が `transition`
    /// ロックを取る唯一の入口。
    ///
    /// - **取れた**: そのまま実行（従来どおり）。
    /// - **取れず、保持者が live apply**（[`Self::commit_catalog_and_apply_live`]）:
    ///   `lock().await` で**待ってから**実行する。live apply は「収集の遷移」
    ///   ではなく構成の反映で、しかも PLC へのダイヤルを含んで数秒かかり
    ///   うるため、その間に来た運用者の「停止」を無視してはいけない。
    ///   待ったあとは**その時点の状態で**判断する（呼び出し元が
    ///   `self.status()` を読み直す）- ループは不要で、ロックを取った
    ///   時点の状態が唯一の判断材料。
    /// - **取れず、保持者が別のライフサイクル遷移**: 従来どおり `None`
    ///   （呼び出し元は現在状態を返して終わる）。これは「遷移中に重ねて
    ///   来た開始・停止要求は待たずに現在状態を返す」という冪等契約
    ///   （docs/banto-hub-desktop-plan.md §9.1・T14）そのもので、待って
    ///   実行すると start→start が2回走ってしまう。
    ///
    /// 競合について: `try_lock` 失敗 → フラグ確認の間に live apply が
    /// 終わって別のライフサイクル遷移がロックを取ることはありうる。その
    /// 場合はそちらの完了も待ってから状態を見るだけで、誤った二重実行には
    /// ならない（ロック取得後に状態を評価するため）。
    async fn acquire_transition(&self) -> Option<tokio::sync::MutexGuard<'_, ()>> {
        match self.transition.try_lock() {
            Ok(guard) => Some(guard),
            Err(_) if self.live_apply_in_progress.load(Ordering::SeqCst) => {
                Some(self.transition.lock().await)
            }
            Err(_) => None,
        }
    }

    /// Start the requested mode. A request arriving during another lifecycle
    /// transition is not queued; it returns the state already published by
    /// that transition. A request arriving during a live apply
    /// ([`Self::commit_catalog_and_apply_live`]) **waits** for it and then
    /// runs normally - see [`Self::acquire_transition`]. Starting the same
    /// mode while running is idempotent.
    pub async fn start(&self, mode: RunMode) -> CollectionStatus {
        let Some(_guard) = self.acquire_transition().await else {
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
        let Some(_guard) = self.acquire_transition().await else {
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
        let Some(_guard) = self.acquire_transition().await else {
            return self.status();
        };

        let current = self.status();
        if current.state == CollectionState::Stopped {
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

/// [`CollectionController::live_apply_in_progress`] を必ず下ろすための
/// RAII ガード（live apply がどの早期 return / パニックで抜けても、
/// 「live apply 中」と誤認されたままにならないようにする）。
struct LiveApplyFlagGuard<'a>(&'a AtomicBool);

impl Drop for LiveApplyFlagGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
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
    use crate::broker_glue::{BrokerSimRegistry, HubSessions};
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
            Arc::new(BrokerSimRegistry::new()),
            Arc::new(ComputedEngine::new(Arc::new(ServerTagStore::new()))),
        ));
        let controller = Arc::new(CollectionController::new(manager));
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

    /// #341 レビュー対応2（2026-09-14）: **live apply 中に来た `stop` は
    /// 無視されず、live apply の完了を待ってから実行される**
    /// （[`CollectionController::acquire_transition`]）。
    ///
    /// #341 で `commit_catalog_and_apply_live` が `transition` ロックを
    /// **保持して待つ**ようになったため、`try_lock` のままだと運用者の
    /// 「停止」が黙って落ちて収集が続いてしまう、というレビュー指摘への
    /// 回帰テスト。live apply は `CollectorManager::delay_next_apply_for_test`
    /// で意図的に遅らせ、その最中に `stop()` を投げる。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn stop_during_a_live_apply_waits_instead_of_being_ignored() {
        let (_dir, controller) = controller_env().await;
        let running = controller.start(RunMode::Configured).await;
        assert_eq!(running.state, CollectionState::Running);

        // 次の `apply_run`（= live apply の中身）を 500ms 止める。
        controller
            .manager
            .delay_next_apply_for_test(Duration::from_millis(500));

        let applying = {
            let controller = controller.clone();
            tokio::spawn(async move { controller.commit_catalog_and_apply_live().await })
        };

        // live apply がロックを掴むまで待つ（掴む前に stop を投げると
        // そもそも競合にならずテストの意味が無い）。
        assert!(
            wait_until(Duration::from_secs(5), || async {
                controller
                    .live_apply_in_progress
                    .load(std::sync::atomic::Ordering::SeqCst)
            })
            .await,
            "live apply が transition ロックを取るはず"
        );

        let stopped = controller.stop().await;
        assert_eq!(
            stopped.state,
            CollectionState::Stopped,
            "live apply 中の stop は無視されず、完了を待って実行されること"
        );
        assert!(applying.await.expect("join").is_ok());
        assert_eq!(controller.status().state, CollectionState::Stopped);
    }

    /// 対照実験（#341 レビュー対応2）: **ライフサイクル遷移**同士は従来
    /// どおり no-op のまま - `acquire_transition` が待つのは live apply の
    /// ときだけで、start→start を二重に走らせたりしない。
    /// `concurrent_starts_do_not_allocate_two_running_ids`（上）と同じ
    /// 性質を、`acquire_transition` 導入後も保つことの明示。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_second_start_during_a_start_is_still_a_no_op() {
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
        let settled = wait_until(Duration::from_secs(5), || async {
            controller.status().state == CollectionState::Running
        })
        .await;
        assert!(settled);
        assert_eq!(
            controller.status().run_id,
            Some(1),
            "run_id が2つ払い出されていない = 二重起動していない"
        );
    }

    /// Poll `predicate` every 10ms until it returns true or `timeout` elapses.
    async fn wait_until<F, Fut>(timeout: Duration, mut predicate: F) -> bool
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = bool>,
    {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if predicate().await {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}
