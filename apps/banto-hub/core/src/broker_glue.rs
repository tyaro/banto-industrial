//! T2-2 (docs/tag-server-design.md §6-5, 2026-08-05 決定): wires banto-hub's
//! SLMP collection reads through the shared `banto-broker` (I6) session
//! instead of banto-collect opening its own socket, so a later write (T2-4)
//! can share that same session ("読み書き単一セッション") - Modbus connections
//! are unaffected (§6 item 7: "Modbus 接続は現行の直接クライアントのまま").
//!
//! ## Two pieces
//!
//! - [`BrokerReadClient`]: a `banto_collect::PlcClient` adapter around a
//!   [`banto_broker::ReadOnlyHandle`] - the client
//!   [`crate::hub::CollectorManager`]'s `banto_collect::ClientFactory`
//!   (T2-2's injection seam, `crates/banto-collect/src/task.rs`) hands back
//!   for every SLMP connection.
//! - [`HubSessions`]: the broker session directory itself, owned **outside**
//!   `CollectorManager` (design §6-5: "broker 本体は CollectorManager の外で
//!   生存させ、構成再構築を跨いで SLMP セッションを維持する") so a
//!   `CollectorManager::rebuild` never tears down a live SLMP socket - only
//!   `bin/banto-hub.rs`'s own shutdown does, via [`HubSessions::shutdown`].
//!
//! ## Why `connect()` always returns `Ok` immediately
//!
//! [`BrokerReadClient::connect`] never touches the network - the broker task
//! behind the wrapped handle owns the actual TCP session end-to-end
//! (connect/reconnect/backoff, `banto-broker`'s own `run_broker_task`) and
//! that task is *already running* by the time any `BrokerReadClient` exists
//! (spawned by [`HubSessions::ensure_connection`] during
//! `CollectorManager::rebuild`, independent of when banto-collect's
//! connection task happens to call `connect()`). Reporting immediate success
//! here just means banto-collect's own `ConnState` moves straight to
//! `Connected` and starts calling `read_batch` - which is exactly what should
//! happen, because whether the *broker's* session is actually up is a
//! question `read_batch`/[`banto_broker::BrokerError::Disconnected`] answers
//! per call, not something `connect()` could usefully pre-check (a check now
//! would just describe a state that may have already changed by the time the
//! next `read_batch` runs).
//!
//! ## The two-backoff double bookkeeping this deliberately creates
//!
//! banto-collect's own per-connection task (`crates/banto-collect/src/task.rs`)
//! still runs its own `ConnState`/backoff loop against this adapter, exactly
//! as it does for a direct `ModbusTcpClient`/`SlmpClient` - T2-2's
//! instructions are explicit that "banto-collect の接続タスク構造は変えず"
//! ("それ以外のタスク構造(ConnState/バックオフ/イベント)は一切変更しない" per
//! I8/T2-2's shared discipline). So there end up being **two** independent
//! backoff loops for one physical SLMP session once a connection is
//! broker-managed:
//!
//! 1. **banto-broker's own** (`crates/banto-broker/src/lib.rs`'s
//!    `run_broker_task`) - the one that actually owns the socket and
//!    reconnects it.
//! 2. **banto-collect's** (`task.rs`'s `run_connection`) - which, from this
//!    adapter's perspective, only ever sees "read succeeded" or "read
//!    failed" (`BrokerReadClient::connect` never fails, so banto-collect's
//!    own `ConnState` is `Connected` almost all the time; a `read_batch`
//!    `Err` drops it into `Backoff` for one cycle, then the very next
//!    connect attempt succeeds immediately per the point above and it is
//!    right back to calling `read_batch`).
//!
//! This is not a bug or wasted work: banto-collect's loop degrades into a
//! **retry-interval governor** for "how often to try `read_batch` again while
//! the broker is down" (its backoff no longer gates an actual socket connect,
//! just the polling cadence), while the broker's loop is the one true
//! reconnect/backoff authority for the physical session. The design's own
//! `/api/v1/status` decision (see `crate::hub::CollectorManager::broker_status`'s
//! doc comment) follows from this: banto-collect's `ConnectionStatus` for a
//! broker-managed connection is not a lie exactly, but it answers a less
//! useful question ("is banto-collect's own retry loop momentarily backing
//! off") than the broker's status answers ("is the physical session up") -
//! so `/api/v1/status` surfaces the broker's answer for SLMP connections,
//! per the design decision this module implements.
//!
//! ## T9-1/T9-2 note: SLMP simulation mode, wired via [`SlmpSimRegistry`]
//!
//! docs/ux-plan.md §1 (2026-08-06, 「接続単位のシミュレーションモード」) adds
//! `banto_tags::PlcConnection::simulation`; for `simulation = true` Modbus
//! connections and for SLMP connections that bypass this broker entirely,
//! `banto_collect::Collector` starts an in-process simulator and substitutes
//! its loopback address for the connection's real host/port itself, at
//! task-spawn time (`crates/banto-collect/src/simulation.rs` and
//! `crates/banto-collect/src/collector.rs`'s "T9-1 addendum" doc section) -
//! no change needed in this module for those.
//!
//! A broker-managed SLMP connection is different: [`HubSessions::ensure_connection`]
//! dials `conn.host`/`conn.port` straight from the `banto_tags::PlcConnection`
//! row, and `crate::hub::CollectorManager::rebuild` calls it (session sync)
//! *before* building the [`hub_client_factory`] it hands to
//! `Collector::apply_config` - i.e. the broker session for an SLMP connection
//! is already established by the time `Collector` would otherwise decide to
//! start that connection's simulator. So, unmodified, a simulated
//! broker-managed SLMP connection would have its broker session dial the
//! *real* (and, in dev/test use, generally unreachable) host/port -
//! simulation mode would silently not take effect for it.
//!
//! The natural substitution point: `rebuild` must know the simulator's
//! address *before* calling `ensure_connection`, which means the simulator
//! for such a connection cannot be owned by `Collector` (whose simulator only
//! exists once its task spawns, which is necessarily later in the same
//! `rebuild` call). Two options were considered: (a) give `CollectorManager`
//! its own simulator registry, sibling to [`HubSessions`], keyed by
//! connection id, consulted before `ensure_connection` for any enabled
//! `simulation = true` SLMP connection (and torn down on the same removal
//! sweep this struct's "Session sync policy" section already performs) -
//! `Collector` would then see `simulation = false` effectively for
//! broker-routed connections and simply not start a second, redundant
//! simulator; or (b) reorder `rebuild` so session sync runs *after*
//! `Collector::apply_config`, threading the address `Collector` already
//! assigned back into `ensure_connection` - rejected as the bigger change,
//! since every other ordering constraint this struct documents (the T7-2
//! "Session sync policy" section, and `CollectorManager::rebuild`'s own doc
//! comment) is built around sync-then-apply. (a) was the smaller, more
//! surgical change and is what T9-2 implemented, as [`SlmpSimRegistry`] -
//! see that struct's own doc comment for the concrete mechanism, and
//! `CollectorConfig::suppress_simulation_for`
//! (`crates/banto-collect/src/config.rs`) for how `Collector` is told to
//! stand down for connections `SlmpSimRegistry` already handles.
//!
//! ## Value type coverage: numeric/bit only
//!
//! [`BrokerReadClient::read_batch`] only ever needs to translate
//! `banto_plc::TagValue::{Bit, F64}` - `banto_collect::config::build_config`
//! (S1, `crates/banto-collect/src/config.rs`) skips every `"string"`-typed
//! tag before it ever becomes a `ReadRequest`
//! (`banto_tags::STRING_DATA_TYPE`), so no `ReadRequest` this adapter ever
//! receives can decode as `banto_plc::PlcValue::Str`. The mapping still
//! handles that case defensively (folds it into a per-request `Bad` rather
//! than panicking) in case that invariant is ever broken, but it is not a
//! path this crate's tests need to exercise as a *product* behavior - S2
//! string-tag reads are relay-wright's engine, which talks to
//! [`banto_broker::BrokerHandle::read`] directly with `BatchReadRequest::String`
//! entries, never through banto-collect/this adapter.

use std::collections::HashMap;
use std::sync::Arc;

use banto_broker::{
    BrokerConnectionStatus, BrokerError, BrokerHandle, BrokerSupervisor, ReadOnlyHandle,
    SessionDirectory,
};
use banto_plc::{
    BatchReadRequest, BatchReadResult, BoxFuture, PlcClient, PlcError, PlcValue, ReadRequest,
    ReadResult, TagValue,
};
use banto_tags::PlcConnection;
use tokio::sync::{watch, Mutex as AsyncMutex};

/// A `banto_collect::PlcClient` that reads through a shared broker session
/// instead of owning a socket - see this module's doc comment for the full
/// rationale. Cheap to construct (wraps a clonable [`ReadOnlyHandle`]); a
/// fresh one is handed out per connect attempt by
/// [`crate::hub::CollectorManager`]'s `banto_collect::ClientFactory` closure,
/// same as a real `ModbusTcpClient`/`SlmpClient` would be, even though every
/// instance shares the exact same underlying broker task.
pub struct BrokerReadClient {
    handle: ReadOnlyHandle,
}

impl BrokerReadClient {
    pub fn new(handle: ReadOnlyHandle) -> Self {
        Self { handle }
    }
}

/// Map one [`BatchReadResult`] (the broker's string-capable superset) back to
/// the numeric-only [`ReadResult`] banto-collect's `PlcClient` trait speaks -
/// see this module's doc comment ("Value type coverage") for why the `Str`
/// arm is unreachable in practice but handled anyway.
fn to_read_result(result: BatchReadResult) -> ReadResult {
    match result {
        BatchReadResult::Value(PlcValue::Bit(b)) => ReadResult::Value(TagValue::Bit(b)),
        BatchReadResult::Value(PlcValue::F64(v)) => ReadResult::Value(TagValue::F64(v)),
        BatchReadResult::Value(PlcValue::Str(_)) => ReadResult::Bad(PlcError::Protocol(
            "broker から数値タグに対する予期しない文字列応答がありました".to_string(),
        )),
        BatchReadResult::Bad(err) => ReadResult::Bad(err),
    }
}

impl PlcClient for BrokerReadClient {
    /// Always succeeds immediately without touching the network - see this
    /// module's doc comment ("Why `connect()` always returns `Ok`
    /// immediately").
    fn connect(&mut self) -> BoxFuture<'_, Result<(), PlcError>> {
        Box::pin(async { Ok(()) })
    }

    /// Every request in `requests` is numeric/bit (see this module's doc
    /// comment) - wrapped as `BatchReadRequest::Numeric` and submitted to the
    /// broker in one call, then unwrapped back to `ReadResult` in the same
    /// order the broker guarantees (`banto_broker::BrokerHandle::read`'s doc:
    /// results align 1:1 with the request `Vec`). A [`BrokerError`] of any
    /// variant - the session being down
    /// ([`BrokerError::Disconnected`]/[`BrokerError::ConnectionFailed`]) or
    /// the broker task itself having exited
    /// ([`BrokerError::TaskGone`]) - becomes a whole-call
    /// [`PlcError::Connection`], which `banto-collect`'s `task.rs` treats as
    /// connection-fatal unconditionally (it does not branch on `PlcError`
    /// variant - see this module's doc comment for how that folds into the
    /// existing Bad-row/backoff machinery).
    fn read_batch<'a>(
        &'a mut self,
        requests: &'a [ReadRequest],
    ) -> BoxFuture<'a, Result<Vec<ReadResult>, PlcError>> {
        let handle = self.handle.clone();
        let batch: Vec<BatchReadRequest> = requests
            .iter()
            .copied()
            .map(BatchReadRequest::Numeric)
            .collect();
        Box::pin(async move {
            match handle.read(batch).await {
                Ok(results) => Ok(results.into_iter().map(to_read_result).collect()),
                Err(err) => Err(PlcError::Connection(err.to_string())),
            }
        })
    }

    /// No-op - the broker session is a shared, long-lived asset this client
    /// never owns and must never close (module doc comment: "broker セッションは
    /// 共有資産なので閉じない"). `banto-collect`'s own task calls this on
    /// graceful shutdown; the broker session simply outlives it, exactly as
    /// designed (§6-5: "broker 本体は CollectorManager の外で生存").
    fn disconnect(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
}

/// The broker session directory itself, owned outside `CollectorManager` (see
/// this module's doc comment for why) and shared (via `Arc`) between
/// `bin/banto-hub.rs` (construction + final shutdown) and
/// `crate::hub::CollectorManager` (session sync on every `rebuild`, status
/// lookups for `/api/v1/status`).
///
/// ## Session sync policy: additive + removal (T7-2, 2026-08-05 更新)
///
/// T2-2 originally left this one-directional (`ensure_connection`-only, see
/// this struct's git history / docs/tag-server-design.md §6-5's "不要分の
/// 放置" note for the original 2026-08-05 判断記録): every enabled SLMP
/// connection the registry currently has gets [`HubSessions::ensure_connection`]'d
/// (spawns a fresh broker task on first sight, returns the existing handle -
/// and therefore the existing live session - on every later rebuild for the
/// same connection id, which is exactly the "維持" §6-5 requires), but a
/// connection that was deleted or disabled left its broker task running,
/// parked, idle forever - an accepted resource leak at the time because
/// [`banto_broker::SessionDirectory`] had no removal API at all.
///
/// **T7-2 closes that gap**: `SessionDirectory` now has
/// [`banto_broker::SessionDirectory::remove`] (see that crate's own doc
/// comment for the exact mechanism and its explicit non-guarantee), so
/// `crate::hub::CollectorManager::rebuild` now performs a full sync each
/// call - `ensure_connection` for every currently-enabled SLMP connection
/// (unchanged, additive), THEN [`HubSessions::remove`] for every connection
/// id this directory still tracks (via
/// [`banto_broker::SessionDirectory::connection_ids`]) that is no longer in
/// that enabled-SLMP set (deleted from the registry, disabled, or changed
/// away from `protocol == "slmp"`). The removal step runs strictly AFTER the
/// collector-side commit that stops the corresponding collect task (if any)
/// succeeds - see `CollectorManager::rebuild`'s own doc comment for why that
/// ordering matters (a `BrokerReadClient` must never outlive the session it
/// reads through). This was chosen over "stop everything and respawn"
/// (which would re-open every SLMP session on every rebuild, defeating the
/// whole point of session continuity across registry writes) and over a full
/// `SessionDirectory` rebuild-from-scratch (same problem) - a targeted
/// per-connection `remove` for exactly the ids that fell out of the wanted
/// set is the minimal change that actually reclaims resources without ever
/// touching a session nothing asked to remove.
pub struct HubSessions {
    directory: SessionDirectory,
    /// `Some` until [`Self::shutdown`] consumes it (`BrokerSupervisor::shutdown`
    /// takes `self` by value) - guarded by an async mutex rather than
    /// `std::sync::Mutex` because `shutdown` itself is `.await`-heavy (drains
    /// every broker task) and must not hold a sync lock across that.
    supervisor: AsyncMutex<Option<BrokerSupervisor>>,
}

impl HubSessions {
    /// Start with zero sessions - `BrokerSupervisor::spawn` with an empty
    /// connection slice cannot fail (its only failure mode,
    /// [`BrokerError::UnsupportedProtocol`], requires a connection to reject)
    /// and gives a real, empty [`SessionDirectory`] that
    /// [`Self::ensure_connection`] grows on demand as `CollectorManager`
    /// discovers SLMP connections during its first (and every later) rebuild.
    pub fn new(backoff: banto_broker::BackoffConfig) -> Self {
        let supervisor = BrokerSupervisor::spawn(&[], backoff)
            .expect("spawning a broker supervisor with zero connections cannot fail");
        let directory = supervisor.directory();
        Self {
            directory,
            supervisor: AsyncMutex::new(Some(supervisor)),
        }
    }

    /// The handle for `conn` (must be `protocol == "slmp"`), spawning its
    /// broker task on first sight and reusing the same live session on every
    /// later call for the same connection id - see this struct's "Session
    /// sync policy" doc section.
    pub fn ensure_connection(&self, conn: &PlcConnection) -> Result<BrokerHandle, BrokerError> {
        self.directory.ensure_connection(conn)
    }

    /// `connection_id` の broker セッションが既に生きていれば、その読み取り
    /// 専用ハンドルを返す(新規に起動はしない) - T12(docs/ux-plan.md §4)の
    /// 接続テストが、収集稼働中の SLMP 接続に対して2本目をダイヤルしてしまう
    /// 誤診(実機R08ENCPUは対象ポートが既に使用中だと同じポートへの2本目を
    /// 受け付けない、このモジュールの doc comment参照)を避けるための
    /// 読み取り専用の覗き見。
    pub fn handle_for(&self, connection_id: i64) -> Option<banto_broker::ReadOnlyHandle> {
        self.directory.handle(connection_id).map(|h| h.read_only())
    }

    /// Non-spawning write-capable peek (T15-4, docs/banto-hub-desktop-plan.md
    /// T15 節): `connection_id` のセッションが既に生きていれば、その
    /// **書き込み可能な**フル [`banto_broker::BrokerHandle`] をそのまま返す
    /// (`Self::handle_for` と異なり `read_only()` へ絞らない) - なければ
    /// `None`(新規にはダイヤルしない)。
    ///
    /// `execute_write` の gate 8 は「`CollectionController::stop`(→
    /// `CollectorManager::stop`→この directory の `stop_and_join`)完了後は
    /// `CollectionNotRunning` を返す」という外側のゲートでガードしているが、
    /// `execute_write` 自身は非同期(レジストリ再読み込み・監査行 insert・
    /// レート制限)で、その `.await` の間に**別の**タスクが `stop_and_join`
    /// を完了させてしまう競合が理論上あり得る(=外側ゲートを通過した後、
    /// 物理書き込み直前にセッションが止まっている)。もし従来どおり
    /// `ensure_connection` を使っていた場合、そこでセッションが無い(=消えた)
    /// と分かった瞬間に**新しいセッションを実機へダイヤルしてしまう** -
    /// 収集が止まっているつもりの実機へ、意図せず書き込み用の TCP セッション
    /// が新規に張られる、というのがこの T15-4 が閉じるレース。多重防御として、
    /// 書き込み経路は必ずこの覗き見を使い、セッションが無ければ
    /// (`ensure_connection` を呼ばず)そのまま fail closed する
    /// (`crate::write_path::write_plc_tag` 参照)。
    pub fn write_handle_for(&self, connection_id: i64) -> Option<banto_broker::BrokerHandle> {
        self.directory.handle(connection_id)
    }

    /// The connection-status watch for `connection_id`, if a broker task is
    /// running for it - `None` before the first `ensure_connection` call for
    /// that id (e.g. no rebuild has run yet, or the connection has never been
    /// SLMP). Consumed by `crate::hub::CollectorManager::broker_status` for
    /// `/api/v1/status` (see `broker_glue`'s module doc, "The two-backoff
    /// double bookkeeping" section, for why SLMP status is sourced here
    /// rather than from banto-collect's own status map).
    pub fn status_watch(
        &self,
        connection_id: i64,
    ) -> Option<watch::Receiver<BrokerConnectionStatus>> {
        self.directory.status_watch(connection_id)
    }

    /// How many broker sessions are currently tracked (seeded-empty + every
    /// later `ensure_connection`, minus anything [`Self::remove`]d since -
    /// see this struct's "Session sync policy" doc section). Mainly a
    /// test/diagnostic helper - proxies [`SessionDirectory::connection_count`] -
    /// the test suite uses it to assert a rebuild reused an existing session
    /// rather than spawning a second one for the same connection id, and (T7-2)
    /// that a deleted connection's session was actually untracked.
    pub fn connection_count(&self) -> usize {
        self.directory.connection_count()
    }

    /// Every connection id this directory currently tracks a session for -
    /// proxies [`SessionDirectory::connection_ids`]. `CollectorManager::rebuild`
    /// (T7-2) diffs this against the registry's current enabled-SLMP-connection
    /// set to find which ids to [`Self::remove`].
    pub fn connection_ids(&self) -> Vec<i64> {
        self.directory.connection_ids()
    }

    /// Untrack the session for `connection_id`, if one is tracked - proxies
    /// [`SessionDirectory::remove`] (see that method's doc comment for the
    /// exact mechanism and its explicit non-guarantee: this drops OUR clone
    /// of the `BrokerHandle`, but the broker task only exits once every
    /// clone anywhere is gone). Returns `true` if a session was tracked and
    /// is now untracked.
    ///
    /// **Caller must call this only after the corresponding collect task (if
    /// any) has already stopped** - see this struct's "Session sync policy"
    /// doc section for why the ordering matters.
    pub fn remove(&self, connection_id: i64) -> bool {
        self.directory.remove(connection_id)
    }

    /// Stop one broker session and await its task's completion. This is the
    /// collection-run-context stop primitive; unlike [`Self::remove`], it
    /// guarantees that the per-connection task has joined before returning.
    pub async fn stop_and_join(&self, connection_id: i64) -> bool {
        self.directory.stop_and_join(connection_id).await
    }

    /// Stop every broker task this directory ever spawned (seeded-empty +
    /// every later `ensure_connection`) and await their clean exit. Called
    /// once, from `bin/banto-hub.rs`'s shutdown sequence, **after**
    /// `CollectorManager::shutdown` has already stopped the `Collector` (so
    /// no `BrokerReadClient` is still mid-`read_batch` when the session it
    /// depends on goes away) - see that binary's doc comment for the full
    /// ordering. A second call is a no-op (the `Option` is already `None`).
    pub async fn shutdown(&self) {
        let supervisor = self.supervisor.lock().await.take();
        if let Some(supervisor) = supervisor {
            supervisor.shutdown().await;
        }
    }
}

/// T9-2 (docs/ux-plan.md §1, 2026-08-06 オーナー決定): broker 経由 SLMP 接続の
/// シミュレーションモードを実現するレジストリ - この module doc の
/// 「T9-1/T9-2 note」節で説明した設計 (a) の実体。[`HubSessions`] と対の
/// ライフサイクル(`bin/banto_hub`が構築し、`CollectorManager`の外で
/// `rebuild`を跨いで生存させ、プロセス終了時に一度だけ [`Self::shutdown`])
/// を持つが、責務は独立: `HubSessions`は broker セッション自体を保持し、
/// `SlmpSimRegistry`は「その broker セッションが実際にダイヤルすべき
/// (host, port)」を、必要ならシミュレータを起動して差し替える。
///
/// ## なぜ`ensure_connection`より前に必要か
///
/// `banto_broker::SessionDirectory::ensure_connection`
/// (`crates/banto-broker/src/lib.rs`)は接続 id だけで既存セッションを
/// 再利用する - `conn.host`/`conn.port`を毎回読み直しはしない。したがって
/// `simulation = true`への切り替えでシミュレータの実アドレスを知るのは
/// `ensure_connection`を呼ぶ*前*でなければならない。`crate::hub::CollectorManager::sync_slmp_sessions`
/// は接続ごとに [`Self::resolve`] を先に呼び、その結果(シミュレータの
/// loopback アドレス、または実接続の host/port そのまま)を
/// `ensure_connection`に渡す`banto_tags::PlcConnection`のコピーへ差し込む。
pub struct SlmpSimRegistry {
    /// `simulation = true` として現在起動中のシミュレータ - 接続 id ごとに
    /// 高々1個。[`Self::resolve`]が simulation フラグの on/off に応じて
    /// 起動・停止する。
    simulators: tokio::sync::Mutex<HashMap<i64, banto_collect::simulation::SimulatorHandle>>,
    /// 各接続 id について直近に「実際にダイヤルした(host, port)」を記録する -
    /// [`Self::resolve`]が「宛先が変わったか」を判定するための唯一の情報源。
    /// エントリなし(`None`)は「初回」を意味し、常に "changed" 扱いになる。
    last_target: tokio::sync::Mutex<HashMap<i64, (String, i64)>>,
}

impl SlmpSimRegistry {
    /// 空のレジストリで開始する - `CollectorManager`の最初の`rebuild`が
    /// SLMP 接続を見つけるたびに [`Self::resolve`] 経由で育つ
    /// ([`HubSessions::new`]と同じ「起動時は空、後から育つ」設計)。
    pub fn new() -> Self {
        Self {
            simulators: tokio::sync::Mutex::new(HashMap::new()),
            last_target: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    /// `conn`(SLMP 接続)が実際にダイヤルすべき`(host, port)`を解決する。
    ///
    /// - `conn.simulation == true` の場合: `conn.id`用のシミュレータが
    ///   既に起動中ならそれを再利用し(`addr()`を返す)、なければ
    ///   `banto_collect::simulation::start(banto_collect::Protocol::Slmp)`で
    ///   新規に起動して登録する。
    /// - `conn.simulation == false` の場合: `conn.id`用のシミュレータが
    ///   まだ追跡されていれば取り除いて停止する(もう不要なので)。
    ///
    /// いずれの場合も、有効な宛先を`crates/banto-collect/src/collector.rs`の
    /// `plan_for_task`が使うのと同じ規約
    /// (`cfg.host = addr.ip().to_string(); cfg.port = addr.port();`、ここでは
    /// `PlcConnection::port`が`i64`であることに合わせてキャストする)で
    /// 計算し、`last_target`に記録されている直前の値(`conn.id`が未記録なら
    /// `None` = 常に「変わった」扱い)と比較して`changed`を求め、
    /// `last_target`をこの新しい値で更新してから`(host, port, changed)`を
    /// 返す。
    ///
    /// ## `changed`が重要な理由(ここが本体)
    ///
    /// `banto_broker::SessionDirectory::ensure_connection`
    /// (`crates/banto-broker/src/lib.rs`、545行目付近)は接続 id だけで
    /// 既存セッションを再利用する実装になっている
    /// (`if let Some(handle) = handles.get(&conn.id) { return Ok(handle.clone()); }`) -
    /// 既にその id のセッションが生きていれば、host/port が変わっていても
    /// 再ダイヤルも検知もしない。したがって呼び出し側
    /// (`CollectorManager::sync_slmp_sessions`)は、`resolve`が
    /// `changed == true`を返すたびに、`ensure_connection`を呼ぶ*前*に必ず
    /// [`HubSessions::remove`]`(conn.id)`を呼ばなければならない - そうしな
    /// ければ`ensure_connection`は同じ id の古いセッション(シミュレーション
    /// を切ったあとの実ホスト、あるいは前回のシミュレータインスタンスの
    /// アドレスなど)へ黙って接続し続けてしまう。
    ///
    /// **これだけでは実は不十分**(`crate::hub`のモジュール doc「SLMP 接続
    /// 単位のシミュレーションモード」節参照、自前の E2E テストで発覚): broker
    /// セッションを`HubSessions::remove`+`ensure_connection`で入れ替えても、
    /// 既に動いている`banto_collect`側の収集タスクはその変化に気づかない
    /// (タスクが起動時に捕まえた`ClientFactory`はタスクの生存期間を通じて
    /// 固定 - `crate::hub::CollectorManager`は追加で
    /// `banto_collect::CollectorConfig::set_broker_dial_target`を呼び、
    /// `Collector::apply_config`がこの接続を「replaced」に分類してタスクごと
    /// 再起動するよう仕向けている。詳細は`set_broker_dial_target`自身の doc
    /// comment、および`crate::hub`のモジュール doc参照)。
    ///
    /// この仕組みはシミュレーションに限らない副次効果を持つ: 従来、同じ
    /// 接続行の host/port を実際に編集しても(接続 id は変わらないので)
    /// broker セッションは古いアドレスへ接続したまま再ダイヤルされない、
    /// という既存の潜在的な不具合があった - 「直前のダイヤル先」を接続 id
    /// ごとに記録・比較するこの仕組みは、その不具合も副次的に(正しく)
    /// 修正する。これは回帰リスクではない:
    /// `apps/banto-hub/core/tests/integration.rs`には「同じ接続の別グループ
    /// へのタグ追加のような無関係な編集ではセッションを再生成しない」ことを
    /// 保証するテストが既にあり、ダイヤル先が実際に変わっていない限り
    /// `changed`は`false`のままであることを、この T9-2 のテスト
    /// (`apps/banto-hub/core/tests/t9_simulation.rs`)でも同様に確認する。
    pub async fn resolve(&self, conn: &PlcConnection) -> (String, i64, bool) {
        let mut simulators = self.simulators.lock().await;
        let target = if conn.simulation {
            let addr = match simulators.get(&conn.id) {
                Some(handle) => handle.addr(),
                None => {
                    let handle =
                        banto_collect::simulation::start(banto_collect::Protocol::Slmp).await;
                    let addr = handle.addr();
                    simulators.insert(conn.id, handle);
                    addr
                }
            };
            (addr.ip().to_string(), addr.port() as i64)
        } else {
            if let Some(handle) = simulators.remove(&conn.id) {
                handle.stop().await;
            }
            (conn.host.clone(), conn.port)
        };
        drop(simulators);

        let mut last_target = self.last_target.lock().await;
        let changed = last_target.get(&conn.id) != Some(&target);
        last_target.insert(conn.id, target.clone());
        (target.0, target.1, changed)
    }

    /// `connection_id`用に追跡しているシミュレータ(あれば)を停止・忘却し、
    /// `last_target`のエントリも忘れる。有効な SLMP 集合から外れた接続
    /// (削除・無効化・プロトコル変更)の掃除ステップから呼ばれる -
    /// [`HubSessions::remove`]と同じタイミング保証: **対応する収集タスクが
    /// 既に停止した後にのみ**呼び出すこと。
    pub async fn remove(&self, connection_id: i64) {
        if let Some(handle) = self.simulators.lock().await.remove(&connection_id) {
            handle.stop().await;
        }
        self.last_target.lock().await.remove(&connection_id);
    }

    /// 追跡中の全シミュレータを停止する。`bin/banto_hub`のシャットダウン
    /// シーケンスから一度だけ呼ばれる - [`HubSessions::shutdown`]より
    /// *後*に呼ぶこと(シミュレータは、それをダイヤルする broker セッション
    /// より長生きしなければならない)。
    pub async fn shutdown(&self) {
        let mut simulators = self.simulators.lock().await;
        for (_, handle) in simulators.drain() {
            handle.stop().await;
        }
    }
}

impl Default for SlmpSimRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Build the `banto_collect::ClientFactory`
/// [`crate::hub::CollectorManager::rebuild`] hands to
/// `banto_collect::Collector::start_with_client_factory`: SLMP connections
/// (looked up by `banto_collect::ClientSpec::connection_key`, the same
/// `"conn:{id}"` key `slmp_handles` is keyed by - see
/// `crate::hub::CollectorManager`'s session-sync step) get a
/// [`BrokerReadClient`]; everything else (Modbus, and the defensive fallback
/// for an SLMP connection somehow missing from `slmp_handles`) gets
/// banto-collect's own `default_client_factory` - the same direct
/// `ModbusTcpClient`/`SlmpClient` construction every non-hub caller still
/// gets (T2-2: "Modbus 接続は現行の直接クライアントのまま").
pub fn hub_client_factory(
    slmp_handles: Arc<HashMap<String, ReadOnlyHandle>>,
) -> banto_collect::ClientFactory {
    let default = banto_collect::default_client_factory();
    Arc::new(
        move |spec: &banto_collect::ClientSpec| -> Box<dyn PlcClient> {
            match spec.protocol {
                banto_collect::ClientProtocol::Slmp => {
                    match slmp_handles.get(&spec.connection_key) {
                        Some(handle) => Box::new(BrokerReadClient::new(handle.clone())),
                        // Defensive only: every enabled SLMP connection was
                        // ensure_connection'd (and inserted here) earlier in the
                        // same rebuild, before this factory was built - see
                        // `crate::hub::CollectorManager::rebuild`. Falling back to
                        // the default direct SlmpClient rather than panicking
                        // means a task still runs (degraded to an unshared
                        // session) instead of the whole connection silently
                        // never collecting.
                        None => default(spec),
                    }
                }
                banto_collect::ClientProtocol::ModbusTcp => default(spec),
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use banto_plc_write::slmp::simulator::Simulator;

    fn slmp_conn(id: i64, port: u16) -> PlcConnection {
        PlcConnection {
            id,
            name: format!("conn-{id}"),
            protocol: "slmp".to_string(),
            host: "127.0.0.1".to_string(),
            port: port as i64,
            unit_id: 1,
            enabled: true,
            simulation: false,

            word_order: "low_high".to_string(),
        }
    }

    /// T15-4 の核心の単体テスト: `stop_and_join` 後、`write_handle_for` は
    /// (再ダイヤルせず)`None` を返し、`connection_count` は 0 のまま
    /// 留まる - `ensure_connection` を呼べば同じ id のセッションが
    /// **蘇る**(=新規にダイヤルする)ことと対照させ、書き込み経路が
    /// なぜ `ensure_connection` を使ってはいけないかを実証する。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn write_handle_for_does_not_resurrect_a_stopped_session() {
        let sim = Simulator::start().await;
        let conn = slmp_conn(1, sim.addr.port());

        let sessions = HubSessions::new(banto_broker::BackoffConfig::default());
        sessions
            .ensure_connection(&conn)
            .expect("ensure_connection should spawn a session");
        assert_eq!(sessions.connection_count(), 1);
        assert!(
            sessions.write_handle_for(conn.id).is_some(),
            "a live session should be peekable"
        );

        assert!(sessions.stop_and_join(conn.id).await);
        assert_eq!(
            sessions.connection_count(),
            0,
            "stop_and_join should untrack the session"
        );
        assert!(
            sessions.write_handle_for(conn.id).is_none(),
            "peek must not spawn a new session for a stopped connection id"
        );

        // Contrast: `ensure_connection` (what the write path used to call,
        // pre-T15-4) WOULD resurrect it - proving `write_handle_for`'s
        // no-spawn behavior is the meaningful difference, not an accident of
        // `SessionDirectory::handle` always being empty here.
        sessions
            .ensure_connection(&conn)
            .expect("ensure_connection resurrects the session (contrast case)");
        assert_eq!(sessions.connection_count(), 1);

        sessions.shutdown().await;
        sim.stop();
    }
}
