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
//!
//! # 終了の受付（#392 A2 レビュー、2026-10-12）
//!
//! デスクトップのトレイの「アプリを終了」は、「収集していない」文言で同意を
//! 取ったら止めずに終了する。その確認と終了の確定の間に、LAN の REST・MCP
//! から収集が開始されると、**新しく始まった収集を再確認なしで止めて終了**
//! してしまう。そこで、このコントローラが「終了受付中」の印を**2 種類**持つ
//! （#392 A2 再レビュー P2-2、2026-10-12）:
//!
//! - **確認のための一時的な印**（`ControllerState::quit_confirm_hold`）。トレイの
//!   確認の答えで立て、確かめ直すとき（`AskAgain`）に下ろす。
//! - **終了処理の取り消せない印**（`ControllerState::shutting_down`）。終了処理
//!   （`crate::runtime::RunningHub::shutdown`）が立て、**誰も下ろさない**。
//!   一時的な印を下ろす口（`cancel_quit_confirm`）はこちらに触れないので、
//!   遅れて届いたトレイの `AskAgain` が、別の終了経路が始めた終了処理の印を
//!   解除することは無い。
//!
//! 開始はどちらかが立っていれば断る。
//!
//! - [`CollectionController::begin_quit_confirm`] - 一時的な印を立て、**同じ `state`
//!   ロックの中で**今の状態を読んで返す（トレイはこれで「忙しいか」を判断する）、
//! - [`CollectionController::cancel_quit_confirm`] - 一時的な印だけを下ろす（トレイが
//!   「収集中」の文言でもう一度確かめるとき）、
//! - [`CollectionController::begin_shutdown`] - 取り消せない印を立てる、
//! - 開始（[`CollectionController::start`]・走行中の
//!   [`CollectionController::set_mode`]）は、状態を `Starting` にする**同じ
//!   `state` ロックの区間で**印を見て、立っていれば [`QuitInProgress`] で
//!   **明示的に断る**（黙って捨てない。REST は 409、MCP はツールエラー）、
//!
//! とする。`Starting` への書き込みと `begin_quit_confirm` の読み取りが同じロックの
//! 下にあるので、どちらが先でも「黙って止める」結果にならない: 開始が先なら
//! `begin_quit_confirm` が `Starting` 以降を読み（トレイは確認し直す）、`begin_quit_confirm`
//! が先なら開始が断られる。`transition` ロックではなく `state` ロックで
//! 直列化するのは、`transition` は live apply が PLC へのダイヤルを含めて
//! 数秒握ることがあり、トレイの答えのコールバックをそこで待たせないため
//! （`state` ロックは短い同期の区間でしか握られない）。
//!
//! 取り消せない印は終了処理（`crate::runtime::RunningHub::shutdown`）が先頭で
//! 立てる - トレイ以外の終了経路（Windows サービスの停止）でも、終了処理中に
//! REST から開始されて止めたはずの収集が動き出すことは無い。

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

/// 終了処理中のため収集の開始を断ったときの文言（REST・MCP がそのまま返す）。
pub const QUIT_IN_PROGRESS_MESSAGE: &str =
    "Banto Hub の終了処理中のため、収集を開始できません（この操作は実行されていません）";

/// 終了の受付の印が立っている間の開始の拒否（このモジュールの doc
/// 「終了の受付」）。開始は**実行されていない**。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QuitInProgress;

impl std::fmt::Display for QuitInProgress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(QUIT_IN_PROGRESS_MESSAGE)
    }
}

impl std::error::Error for QuitInProgress {}

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
    /// 終了の確認のための一時的な印（このモジュールの doc「終了の受付」）。
    /// トレイの `AskAgain` で下ろす。
    quit_confirm_hold: bool,
    /// 終了処理の取り消せない印。一度立てたら下ろさない。
    ///
    /// どちらかが立っている間の開始は [`QuitInProgress`] で断る。読み書きは
    /// 必ずこの `state` ロックの中で、`Starting` への書き込みと同じ区間で見る。
    shutting_down: bool,
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
                quit_confirm_hold: false,
                shutting_down: false,
            }),
            transition: AsyncMutex::new(()),
            live_apply_in_progress: AtomicBool::new(false),
            run_seq: AtomicU64::new(0),
            status_tx,
        }
    }

    /// 終了の受付の印を立て、**同じ `state` ロックの中で**読んだその時点の
    /// 状態を返す（このモジュールの doc「終了の受付」）。以降の開始は
    /// [`QuitInProgress`] で断られる。冪等。
    ///
    /// 返した状態が `Starting`/`Running`/`Stopping` なら、印を立てる前に
    /// 開始が受け付けられていた（または走っている）。`Stopped`/`Faulted`
    /// なら、この後に収集が始まることはない（印を下ろすまで）。
    pub fn begin_quit_confirm(&self) -> CollectionStatus {
        let mut status = {
            let mut state = self
                .state
                .lock()
                .expect("collection controller state lock poisoned");
            state.quit_confirm_hold = true;
            state.status()
        };
        status.configured_revision = self.manager.configured_revision();
        status.running_revision = self.manager.running_revision();
        status
    }

    /// 確認のための一時的な印**だけ**を下ろす。トレイが「収集中」の文言で
    /// もう一度確かめるとき（`AskAgain`）だけ使う。終了処理の取り消せない印
    /// （[`Self::begin_shutdown`]）には触れないので、それが立っていれば開始は
    /// 断られ続ける。
    pub fn cancel_quit_confirm(&self) {
        self.state
            .lock()
            .expect("collection controller state lock poisoned")
            .quit_confirm_hold = false;
    }

    /// 終了処理の取り消せない印を立て、同じロックの中で読んだその時点の状態を
    /// 返す（このモジュールの doc「終了の受付」）。以降の開始は断られ続ける
    /// （下ろす口は無い）。冪等。
    pub fn begin_shutdown(&self) -> CollectionStatus {
        let mut state = self
            .state
            .lock()
            .expect("collection controller state lock poisoned");
        state.shutting_down = true;
        state.status()
    }

    /// 開始を断る印（一時的・取り消せないのどちらか）が立っているか。
    /// 入口での早い拒否と表示・テスト用で、開始の可否の最終判断は
    /// `start_locked` が `Starting` を書く区間で行う。
    pub fn is_quitting(&self) -> bool {
        self.state
            .lock()
            .expect("collection controller state lock poisoned")
            .refuses_starts()
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
    ///
    /// 終了の受付の印が立っていれば（[`Self::begin_quit_confirm`]）、何もせずに
    /// [`QuitInProgress`] を返す。印の確認は入口と、状態を `Starting` にする
    /// 区間（`start_locked`）の 2 か所 - 入口は「遷移中の no-op」で現在状態を
    /// 返す前に断るため、`start_locked` は入口を通った後（live apply を
    /// 待っている間など）に印が立った場合を断るため。**直列化の保証は
    /// `start_locked` の側**にある（入口の確認だけでは隙間が残る）。
    pub async fn start(&self, mode: RunMode) -> Result<CollectionStatus, QuitInProgress> {
        if self.is_quitting() {
            return Err(QuitInProgress);
        }
        let Some(_guard) = self.acquire_transition().await else {
            return Ok(self.status());
        };

        if self.status().state == CollectionState::Starting
            || self.status().state == CollectionState::Stopping
        {
            return Ok(self.status());
        }

        if self.status().state == CollectionState::Running && self.status().mode == mode {
            return Ok(self.status());
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
    ///
    /// 走行中の切り替えは開始を含むので、終了の受付の印が立っていれば
    /// [`QuitInProgress`] を返す（停止中にモードを選ぶだけなら開始しないので
    /// 断らない）。印が `stop_locked` の後に立った場合は、止めたまま
    /// `start_locked` が断る（終了処理が止めるはずの収集なので、止まった
    /// ままでよい）。
    pub async fn set_mode(&self, mode: RunMode) -> Result<CollectionStatus, QuitInProgress> {
        let Some(_guard) = self.acquire_transition().await else {
            return Ok(self.status());
        };

        let current = self.status();
        if current.state == CollectionState::Stopped {
            self.set_mode_locked(mode);
            self.publish_status();
            return Ok(self.status());
        }
        if current.state == CollectionState::Running && current.mode == mode {
            return Ok(current);
        }
        if matches!(
            current.state,
            CollectionState::Starting | CollectionState::Stopping
        ) {
            return Ok(current);
        }
        if self.is_quitting() {
            return Err(QuitInProgress);
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

    async fn start_locked(&self, mode: RunMode) -> Result<CollectionStatus, QuitInProgress> {
        {
            let mut state = self
                .state
                .lock()
                .expect("collection controller state lock poisoned");
            // 終了の受付（このモジュールの doc）: 印の確認と `Starting` の
            // 書き込みを**同じロックの区間**で行う。`begin_quit_confirm` も同じロックで
            // 印を立てて状態を読むので、両者の間に隙間は無い。
            if state.refuses_starts() {
                return Err(QuitInProgress);
            }
            let run_id = self.run_seq.fetch_add(1, Ordering::SeqCst) + 1;
            let context = RunContext { mode, run_id };
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
        Ok(self.status())
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
    /// 開始を断るか（終了の受付の印のどちらかが立っている）。
    fn refuses_starts(&self) -> bool {
        self.quit_confirm_hold || self.shutting_down
    }

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

        let first = controller
            .start(RunMode::Configured)
            .await
            .expect("収集を開始できる");
        assert_eq!(first.state, CollectionState::Running);
        assert_eq!(first.run_id, Some(1));

        let repeated = controller
            .start(RunMode::Configured)
            .await
            .expect("収集を開始できる");
        assert_eq!(repeated, first);

        let stopped = controller.stop().await;
        assert_eq!(stopped.state, CollectionState::Stopped);
        assert_eq!(stopped.run_id, None);

        let second = controller
            .start(RunMode::Configured)
            .await
            .expect("収集を開始できる");
        assert_eq!(second.state, CollectionState::Running);
        assert_eq!(second.run_id, Some(2));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn all_simulation_starts_without_auto_restart() {
        let (_dir, controller) = controller_env().await;

        let running = controller
            .start(RunMode::AllSimulation)
            .await
            .expect("収集を開始できる");
        assert_eq!(running.state, CollectionState::Running);
        assert_eq!(running.run_id, Some(1));
        assert!(running.last_error.is_none());
        assert_eq!(controller.status(), running);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn switching_from_configured_to_all_simulation_starts_the_new_mode() {
        let (_dir, controller) = controller_env().await;
        let running = controller
            .start(RunMode::Configured)
            .await
            .expect("収集を開始できる");
        assert_eq!(running.state, CollectionState::Running);
        assert_eq!(running.run_id, Some(1));

        let switched = controller
            .set_mode(RunMode::AllSimulation)
            .await
            .expect("収集を開始できる");

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
        let (left, right) = (left.expect("left start"), right.expect("right start"));

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

        let running = controller
            .start(RunMode::Configured)
            .await
            .expect("収集を開始できる");
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
        let running = controller
            .start(RunMode::Configured)
            .await
            .expect("収集を開始できる");
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
        let (left, right) = (left.expect("left start"), right.expect("right start"));
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

    /// トレイの「忙しい」と同じ判定（`src-tauri` の
    /// `tray_status::show_stop_item`）。
    fn busy(status: &CollectionStatus) -> bool {
        matches!(
            status.state,
            CollectionState::Starting | CollectionState::Running | CollectionState::Stopping
        )
    }

    /// #392 A2 レビュー: 終了の受付の印が立っている間の開始は**明示的に**
    /// 断られ（黙って現在状態を返さない）、印を下ろせば再び受け付ける。
    /// 停止中のモード選択は開始ではないので断らない。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn starts_are_refused_while_quitting_and_accepted_after_cancel() {
        let (_dir, controller) = controller_env().await;

        let at_quit = controller.begin_quit_confirm();
        assert_eq!(at_quit.state, CollectionState::Stopped);
        assert!(!busy(&at_quit));
        assert!(controller.is_quitting());

        assert_eq!(
            controller.start(RunMode::Configured).await,
            Err(QuitInProgress)
        );
        assert_eq!(
            controller.start(RunMode::AllSimulation).await,
            Err(QuitInProgress)
        );
        assert_eq!(controller.status().state, CollectionState::Stopped);
        assert_eq!(
            controller.status().run_id,
            None,
            "run_id を払い出していない"
        );

        let selected = controller
            .set_mode(RunMode::AllSimulation)
            .await
            .expect("停止中のモード選択は開始ではないので断らない");
        assert_eq!(selected.state, CollectionState::Stopped);
        assert_eq!(selected.mode, RunMode::AllSimulation);

        controller.cancel_quit_confirm();
        assert!(!controller.is_quitting());
        let running = controller
            .start(RunMode::Configured)
            .await
            .expect("印を下ろせば開始できる");
        assert_eq!(running.state, CollectionState::Running);
        assert_eq!(running.run_id, Some(1));
    }

    /// 走っている間に印を立てると「忙しい」が返り、走行中のモード切り替え
    /// （停止 → 開始を含む）は断られる（走っている収集はそのまま - 止めるのは
    /// 終了処理の仕事）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn begin_quit_while_running_is_busy_and_refuses_a_mode_switch() {
        let (_dir, controller) = controller_env().await;
        controller
            .start(RunMode::Configured)
            .await
            .expect("収集を開始できる");

        let at_quit = controller.begin_quit_confirm();
        assert_eq!(at_quit.state, CollectionState::Running);
        assert!(busy(&at_quit));

        assert_eq!(
            controller.set_mode(RunMode::AllSimulation).await,
            Err(QuitInProgress)
        );
        let status = controller.status();
        assert_eq!(status.state, CollectionState::Running);
        assert_eq!(status.mode, RunMode::Configured);
        assert_eq!(status.run_id, Some(1));
    }

    /// 境界 1（開始が先）: 開始が `Starting` に入った後に印を立てると、
    /// `begin_quit_confirm` は `Starting` を返す = トレイは「収集中」の文言で確かめ
    /// 直す（黙って止めない）。開始そのものは受け付け済みなので完了する。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn begin_quit_after_a_start_was_admitted_reports_busy() {
        let (_dir, controller) = controller_env().await;
        controller
            .manager
            .delay_next_apply_for_test(Duration::from_millis(300));
        let starting = {
            let controller = controller.clone();
            tokio::spawn(async move { controller.start(RunMode::Configured).await })
        };
        assert!(
            wait_until(Duration::from_secs(5), || async {
                controller.status().state == CollectionState::Starting
            })
            .await,
            "開始が Starting に入るはず"
        );

        let at_quit = controller.begin_quit_confirm();
        assert_eq!(at_quit.state, CollectionState::Starting);
        assert!(busy(&at_quit), "受け付け済みの開始は忙しい側に数える");

        let started = starting
            .await
            .expect("join")
            .expect("印より先に受け付けた開始は断らない");
        assert_eq!(started.state, CollectionState::Running);
    }

    /// 境界 2（印が先、開始は入口を通過済み）: 開始の入口の確認を通った後
    /// （live apply が `transition` を握っていて待っている等）に印が立ったら、
    /// `Starting` を書く区間（`start_locked`）で断る。**入口の確認だけでは
    /// 隙間が残る**ことの固定 - 停止中の文言で同意して終了へ進んだ後に、
    /// 待っていた開始が収集を立ち上げてはいけない。
    ///
    /// 「入口を通過済み」を確実に作るため、`transition` を握った状態で
    /// `start_locked` を直接呼ぶ（`start` の入口の後に続く処理そのもの）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_start_past_the_entry_is_refused_when_quit_wins_the_state_lock() {
        let (_dir, controller) = controller_env().await;
        let _transition = controller.transition.lock().await;

        let at_quit = controller.begin_quit_confirm();
        assert!(!busy(&at_quit), "停止中 = 黙って終了へ進んでよい側");

        assert_eq!(
            controller.start_locked(RunMode::Configured).await,
            Err(QuitInProgress)
        );
        let status = controller.status();
        assert_eq!(status.state, CollectionState::Stopped);
        assert_eq!(status.run_id, None, "run_id を払い出していない");
    }

    /// 同じ境界を、本物の `start` が live apply を待っている形でも確かめる
    /// （上の白箱のテストの対照）。`start` は live apply（`transition` を
    /// 握っている）を待ち、その間に印が立ち、live apply が終わると断られる。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_start_waiting_for_a_live_apply_is_refused_after_begin_quit() {
        let (_dir, controller) = controller_env().await;
        let transition = controller.transition.lock().await;
        controller
            .live_apply_in_progress
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let starting = {
            let controller = controller.clone();
            tokio::spawn(async move { controller.start(RunMode::Configured).await })
        };
        // `start` が入口を通って `transition` を待つまでの猶予。間に合わず
        // 入口で断られても、結果（`Err`）は同じ。
        tokio::time::sleep(Duration::from_millis(100)).await;

        let at_quit = controller.begin_quit_confirm();
        assert!(!busy(&at_quit));
        controller
            .live_apply_in_progress
            .store(false, std::sync::atomic::Ordering::SeqCst);
        drop(transition);

        assert_eq!(starting.await.expect("join"), Err(QuitInProgress));
        assert_eq!(controller.status().state, CollectionState::Stopped);
    }

    /// 開始と印を同時に走らせ、どの順に並んでも「印を立てた時点で暇
    /// （= トレイは確認せずに終了へ進む）なのに、開始が受け付けられた」
    /// 組み合わせにならないこと。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn racing_start_and_begin_quit_never_silently_stop_a_new_run() {
        for _ in 0..50 {
            let (_dir, controller) = controller_env().await;
            let barrier = Arc::new(tokio::sync::Barrier::new(2));
            let start = {
                let controller = controller.clone();
                let barrier = barrier.clone();
                tokio::spawn(async move {
                    barrier.wait().await;
                    controller.start(RunMode::Configured).await
                })
            };
            let quit = {
                let controller = controller.clone();
                let barrier = barrier.clone();
                tokio::spawn(async move {
                    barrier.wait().await;
                    controller.begin_quit_confirm()
                })
            };
            let started = start.await.expect("join start");
            let at_quit = quit.await.expect("join quit");
            match started {
                Ok(status) => {
                    assert_eq!(status.state, CollectionState::Running);
                    assert!(
                        busy(&at_quit),
                        "開始が受け付けられたなら、begin_quit_confirm は忙しいと読んでいるはず: {at_quit:?}"
                    );
                }
                Err(QuitInProgress) => {
                    assert!(!busy(&at_quit));
                    assert_eq!(controller.status().state, CollectionState::Stopped);
                }
            }
        }
    }

    /// #392 A2 再レビュー P2-2: 終了処理の取り消せない印（`begin_shutdown`）は、
    /// 遅れて届いたトレイの確認（一時的な印を立てて、`AskAgain` で下ろす）で
    /// 解除されない。開始は断られ続ける。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_late_ask_again_cannot_lift_the_shutdown_mark() {
        let (_dir, controller) = controller_env().await;
        let at_shutdown = controller.begin_shutdown();
        assert_eq!(at_shutdown.state, CollectionState::Stopped);

        // 遅れて届いた答え: 一時的な印を立てて、確かめ直すので下ろす。
        let _ = controller.begin_quit_confirm();
        controller.cancel_quit_confirm();

        assert!(controller.is_quitting());
        assert_eq!(
            controller.start(RunMode::Configured).await,
            Err(QuitInProgress)
        );
        let _transition = controller.transition.lock().await;
        assert_eq!(
            controller.start_locked(RunMode::Configured).await,
            Err(QuitInProgress)
        );
        assert_eq!(controller.status().state, CollectionState::Stopped);
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
