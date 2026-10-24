//! T2-2 (docs/tag-server-design.md §6-5, 2026-08-05 決定): wires banto-hub's
//! SLMP collection reads through the shared `banto-broker` (I6) session
//! instead of banto-collect opening its own socket, so a later write (T2-4)
//! can share that same session ("読み書き単一セッション").
//!
//! **#131 (2026-09-01) update**: this is no longer SLMP-only. `banto-broker`
//! also has a `"modbus-tcp"` driver (`HubSessions::is_broker_managed`),
//! and this module's write/status surface
//! (`HubSessions::write_broker_handle_peek`,
//! `CollectorManager::broker_status`) is wired generically for any
//! broker-managed protocol - so a Modbus TCP connection's *writes* and
//! *status* go through this same broker session infrastructure exactly like
//! SLMP's.
//!
//! **#337 (2026-09-08 オーナー決定) update**: a Modbus TCP connection's
//! *collection reads* now go through that same broker session too, so this
//! module is protocol-uniform end to end - every broker-managed connection
//! is **1接続 = 1ソケット** shared by reads, writes and status. #131 had
//! deliberately left Modbus reads on banto-collect's own direct
//! `ModbusTcpClient` (two sockets per collecting Modbus connection); that
//! made collection impossible against a Modbus/TCP server which accepts only
//! one client connection: on real hardware (オムロン KM-D1-ETN 電力量モニタ,
//! manual KANC-718B 12-1) the broker session connected first and the
//! collection client's second socket was dropped the instant it opened,
//! leaving every tag Bad. Sharing one session is the whole reason the broker
//! exists ("読み書き単一セッション", §6-5); the tradeoff is that collection
//! reads and writes of one connection serialize over one socket, as SLMP's
//! always have. Regression coverage: `apps/banto-hub/host/tests/integration.rs`'s
//! `modbus_collection_uses_exactly_one_socket`.
//!
//! ## Two pieces
//!
//! - [`HubSessions`]: the broker session directory itself, owned **outside**
//!   `CollectorManager` (design §6-5: "broker 本体は CollectorManager の外で
//!   生存させ、構成再構築を跨いで SLMP セッションを維持する") so a
//!   `CollectorManager::rebuild` never tears down a live SLMP socket - only
//!   `bin/banto-hub.rs`'s own shutdown does, via [`HubSessions::shutdown`].
//! - The collection reads go through `banto_collect::BrokerReadClient` (a
//!   `banto_collect::CollectClient` around a [`banto_broker::ReadOnlyHandle`]),
//!   which `crate::hub::CollectorManager` hands to the collector through
//!   `banto_collect::broker_client_factory` over the handles of this
//!   directory - SLMP since T2-2, Modbus TCP since #337. R2b-3a-3
//!   (docs/banto-hub-driver-io-design.md §4.3) moved that client - with #344's
//!   `connect` semantics (it reports the broker session's real state and waits
//!   `banto_collect::BROKER_RECONNECT_GRACE` for a reconnecting one) - and
//!   [`BrokerSimRegistry`] into `banto-collect`, so ChronoGazer, which reads
//!   through the broker too since then, shares them. See `banto_collect::client`'s
//!   module doc for the #344 derivation.
//!
//! ## The two-backoff double bookkeeping this still creates
//!
//! banto-collect's own per-connection task (`crates/banto-collect/src/task.rs`)
//! still runs its own `ConnState`/backoff loop against the broker read client - T2-2's
//! instructions are explicit that "banto-collect の接続タスク構造は変えず"
//! ("それ以外のタスク構造(ConnState/バックオフ/イベント)は一切変更しない" per
//! I8/T2-2's shared discipline). So there are still **two** independent
//! backoff loops for one physical session once a connection is
//! broker-managed:
//!
//! 1. **banto-broker's own** (`crates/banto-broker/src/lib.rs`'s
//!    `run_broker_task`) - the one that actually owns the socket, reconnects
//!    it, and publishes the status `banto_collect::BrokerReadClient` reads.
//! 2. **banto-collect's** (`task.rs`'s `run_connection`) - which, since #344,
//!    *follows* that status rather than ignoring it: while the broker session
//!    is down, `connect()` fails, so banto-collect stays in `Backoff` and
//!    climbs its own 1 s → 30 s ladder instead of retrying `read_batch` every
//!    period.
//!
//! The division of labour is unchanged, only the coupling is: the broker's
//! loop is the one true reconnect/backoff authority for the physical session,
//! and banto-collect's loop is a **retry-interval governor** for "how often
//! to try reading again while the broker is down". The design's own
//! `/api/v1/status` decision (see `crate::hub::CollectorManager::broker_status`'s
//! doc comment) still follows from this: banto-collect's `ConnectionStatus`
//! for a broker-managed connection answers a less useful question ("is
//! banto-collect's own retry loop momentarily backing off") than the broker's
//! status answers ("is the physical session up") - so `/api/v1/status`
//! surfaces the broker's answer for every broker-managed connection (SLMP
//! since T2-2, Modbus TCP since #131), per the design decision this module
//! implements. Since #344 the two agree far more often than they used to,
//! but the broker remains the source of truth.
//!
//! ## T9-1/T9-2 note: broker-managed connections' simulation mode, wired via [`BrokerSimRegistry`]
//!
//! docs/ux-plan.md §1 (2026-08-06, 「接続単位のシミュレーションモード」) adds
//! `banto_tags::PlcConnection::simulation`. Until R2b-3a-3 a connection that
//! bypassed this broker (any non-hub `banto-collect` caller) had
//! `banto_collect::Collector` start an in-process simulator itself at
//! task-spawn time; since R2b-3a-3 every caller reads through a broker, and
//! the substitution happens before the session is ensured, as described next.
//!
//! A broker-managed connection: [`HubSessions::ensure_connection`]
//! dials `conn.host`/`conn.port` straight from the `banto_tags::PlcConnection`
//! row, and `crate::hub::CollectorManager::rebuild` calls it (session sync)
//! *before* building the client factory (`banto_collect::broker_client_factory`)
//! it hands to `Collector::apply_config` - i.e. the broker session for an SLMP connection
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
//! surgical change and is what T9-2 implemented, as [`BrokerSimRegistry`] -
//! see that struct's own doc comment for the concrete mechanism (it lives in
//! `banto-collect` since R2b-3a-3 and is re-exported here), and
//! `CollectorConfig::set_broker_dial_target` (`crates/banto-collect/src/config.rs`)
//! for how a moved session replaces the collect task that held the old handle.
//!
//! ## Value type coverage: numeric/bit only
//!
//! Collection only ever reads numeric/bit tags - `banto_collect`'s plan skips
//! every `"string"`-typed tag (`banto_tags::STRING_DATA_TYPE`), and a `Str`
//! answer is recorded as Bad. String reads on demand go through
//! `crate::read_path`, which talks to [`banto_broker::ReadOnlyHandle::read`]
//! directly with a string `TagSpec`.

use std::sync::Arc;

use banto_broker::{
    BrokerConnectionStatus, BrokerError, BrokerHandle, BrokerOptions, BrokerSupervisor,
    SecretOpener, SessionDirectory, SettingsFingerprint,
};
use banto_driver_api::{DriverRegistry, Settings};
use banto_tags::PlcConnection;
use tokio::sync::{watch, Mutex as AsyncMutex};

/// T9-2's simulation registry (see this module's doc). Moved to
/// `banto-collect` in R2b-3a-3 (ChronoGazer uses it too); re-exported here so
/// the Hub's callers keep one path.
pub use banto_collect::BrokerSimRegistry;

/// The drivers the host injects into the engine (R1-3b,
/// docs/banto-driver-api-design.md §6.4): the driver registry, which the
/// broker dials through since R2b-3a-2 (collection included since R2b-3a-3,
/// which removed the direct collection client factory this used to carry).
///
/// This crate never names a concrete driver: the composition root (the
/// `banto-hub` bin, the shell, tests) builds this from
/// `banto_drivers::builtin()` and hands it in through `HubConfig::drivers` /
/// [`HubSessions::new`]. Cheap to clone.
///
/// `registry` is `builtin().registry` (`modbus-tcp` / `slmp` as session-type
/// drivers since R2b-3a-2) plus the Hub's own host-provided protocols. Its
/// session-type entries are exactly the broker-managed protocols
/// ([`HubSessions::is_broker_managed`]); the former side table `broker`
/// (`banto_broker::DriverTable`) is gone (docs/banto-hub-driver-io-design.md
/// §4.2).
#[derive(Clone, Debug)]
pub struct HubDrivers {
    pub registry: Arc<DriverRegistry>,
}

impl HubDrivers {
    pub fn new(registry: Arc<DriverRegistry>) -> Self {
        Self { registry }
    }

    /// No PLC drivers: no session-type entry in the registry (every PLC
    /// connection is rejected by the broker as
    /// [`BrokerError::UnsupportedProtocol`]).
    ///
    /// R2b-1d (docs/banto-hub-connection-settings-design.md §4.2): the registry still
    /// holds the Hub's own host-provided protocols (`push` / `postgres` / `virtual`,
    /// [`crate::host_provided::register_host_provided`]) - their I/O is the Hub's, not a
    /// driver's, and without `virtual` the Hub could not even create its reserved
    /// `calc` / `mem` connections now that every entrance validates through the
    /// registry. `modbus-tcp` / `slmp` are absent (a connection of either is excluded as
    /// `driverUnavailable`).
    pub fn none() -> Self {
        let mut registry = DriverRegistry::new();
        crate::host_provided::register_host_provided(&mut registry)
            .expect("the host-provided ids are distinct");
        Self {
            registry: Arc::new(registry),
        }
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
/// call - `ensure_connection` for every currently-enabled connection whose
/// protocol the broker manages (unchanged, additive - as of #131,
/// 2026-09-01, this is [`HubSessions::is_broker_managed`], i.e. `"slmp"`
/// and `"modbus-tcp"`, not just SLMP), THEN [`HubSessions::stop_and_join`] for every
/// connection id this directory still tracks (via
/// [`banto_broker::SessionDirectory::connection_ids`]) that is no longer in
/// that wanted set (deleted from the registry, disabled, or changed away
/// from a broker-managed protocol). The removal step runs strictly AFTER the
/// collector-side commit that stops the corresponding collect task (if any)
/// succeeds - see `CollectorManager::rebuild`'s own doc comment for why that
/// ordering matters (a `banto_collect::BrokerReadClient` must never outlive the
/// session it reads through). This was chosen over "stop everything and respawn"
/// (which would re-open every SLMP session on every rebuild, defeating the
/// whole point of session continuity across registry writes) and over a full
/// `SessionDirectory` rebuild-from-scratch (same problem) - a targeted
/// per-connection stop for exactly the ids that fell out of the wanted
/// set is the minimal change that actually reclaims resources without ever
/// touching a session nothing asked to remove.
///
/// **Changed settings (2026-10-24, owner decision)**: a connection that stays
/// in the wanted set but whose settings fingerprint changed
/// (`BrokerSimRegistry::resolve`'s `changed`) has its old task stopped AND
/// joined with [`HubSessions::stop_and_join`] *before* the new session is
/// ensured - during the sync itself, not after the collector commit. Only
/// [`HubSessions::remove`]-ing it let the new session dial while the old task
/// might still hold its socket (other clones of the old handle outlive the
/// sync: the collector hands every task of a generation the same factory, so
/// a sibling connection's collect task still holds it), and a PLC that refuses
/// a second connection on the same port (R08: 10061) refuses the new one. The
/// changed connection's own collect task reads Bad through the stopped handle
/// until the commit replaces it; no sibling session is touched. See
/// `CollectorManager::sync_broker_sessions_from`'s doc comment.
///
/// **T19 S2-a addendum (UX-48, docs/banto-hub-t19-design.md §3.8,
/// 2026-09-03)**: "every currently-enabled connection whose protocol the
/// broker manages" above got one more condition - the connection also needs
/// at least one *enabled collection group* (`banto_collect::
/// connections_with_collected_groups`, the same predicate `build_config_from`
/// uses to decide whether a connection collects anything at all). A
/// connection with zero enabled groups no longer falls in the wanted set, so
/// it now goes through the exact same removal path described above the
/// moment it stops qualifying - no new mechanism, just a narrower wanted
/// set. See `CollectorManager::sync_broker_sessions_from`'s doc comment for the
/// full derivation. **T19 S2-a 案B (2026-09-03) / #341 (2026-09-14)**: a tag
/// added to a still-unsynced connection no longer has to wait for the next
/// rebuild/apply_run to become writable - `crate::rest::commit_catalog_and_notify`
/// drives `crate::controller::CollectionController::commit_catalog_and_apply_live`,
/// which runs the whole of `CollectorManager::apply_run` (session sync
/// included) whenever the registry change commits while a run is already
/// `Running` - see that method's doc comment for the full derivation and its
/// transition-lock discipline.
pub struct HubSessions {
    directory: SessionDirectory,
    /// The drivers the supervisor below dials through (its registry) - kept
    /// here so [`Self::is_broker_managed`] answers from exactly the registry
    /// [`Self::ensure_connection`] dispatches through (R2b-3a-2).
    drivers: HubDrivers,
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
    /// discovers broker-managed connections during its first (and every
    /// later) rebuild.
    ///
    /// R1-3b: the driver table comes from the composition root as part of
    /// `drivers` (docs/banto-driver-api-design.md §6.4), not from this crate.
    pub fn new(backoff: banto_broker::BackoffConfig, drivers: HubDrivers) -> Self {
        Self::with_options(
            drivers,
            BrokerOptions {
                backoff,
                ..BrokerOptions::default()
            },
        )
    }

    /// As [`Self::new`], with the connections' secrets opened by `secrets`
    /// just before each dial (R2b-3a-2, connection-settings design §3.5; the
    /// runtime passes [`crate::secrets::ConnectionSecretOpener`]). Without it
    /// a driver that needs a secret refuses to dial instead of dialing
    /// without it.
    pub fn with_secret_opener(
        backoff: banto_broker::BackoffConfig,
        drivers: HubDrivers,
        secrets: Arc<dyn SecretOpener>,
    ) -> Self {
        Self::with_options(
            drivers,
            BrokerOptions {
                backoff,
                secrets,
                ..BrokerOptions::default()
            },
        )
    }

    fn with_options(drivers: HubDrivers, options: BrokerOptions) -> Self {
        let supervisor = BrokerSupervisor::spawn_with(&[], drivers.registry.clone(), options)
            .expect("spawning a broker supervisor with zero connections cannot fail");
        let directory = supervisor.directory();
        Self {
            directory,
            drivers,
            supervisor: AsyncMutex::new(Some(supervisor)),
        }
    }

    /// Whether the broker manages connections of `protocol` - i.e. whether
    /// [`Self::ensure_connection`] would spawn a session for one rather than
    /// reject it with [`BrokerError::UnsupportedProtocol`]. The single source
    /// of truth for every "is this a broker-managed connection" gate in this
    /// crate (session sync in `CollectorManager`, the read-now and write
    /// protocol gates, the connection test, `/api/v1/status`). Since R2b-3a-2
    /// it is [`banto_broker::is_broker_managed`] over the registry the
    /// supervisor dials through: the protocol has a Session or Frame driver.
    pub fn is_broker_managed(&self, protocol: &str) -> bool {
        banto_broker::is_broker_managed(&self.drivers.registry, protocol)
    }

    /// The fingerprint a session dialed with `settings` for a `protocol`
    /// connection carries ([`BrokerHandle::settings_fingerprint`]): the
    /// settings without the driver's secret keys, plus the secret references
    /// `saved` (the stored connection, if any) holds for them
    /// ([`SettingsFingerprint::of_connection`]). `None` for a protocol the
    /// broker does not manage. The connection test compares it with a live
    /// session's to decide whether the session's reading answers the form.
    pub fn dial_fingerprint(
        &self,
        protocol: &str,
        saved: Option<&PlcConnection>,
        settings: &Settings,
    ) -> Option<SettingsFingerprint> {
        if !self.is_broker_managed(protocol) {
            return None;
        }
        Some(banto_broker::real_dial_fingerprint(
            &self.drivers.registry,
            protocol,
            saved,
            settings,
        ))
    }

    /// The drivers this directory was built with (R1-3b).
    pub fn drivers(&self) -> &HubDrivers {
        &self.drivers
    }

    /// The handle for `conn` (must be a protocol the broker manages -
    /// [`Self::is_broker_managed`], currently `"slmp"` and
    /// `"modbus-tcp"`), spawning its broker task on first sight and reusing
    /// the same live session on every later call for the same connection id -
    /// see this struct's "Session sync policy" doc section.
    pub fn ensure_connection(&self, conn: &PlcConnection) -> Result<BrokerHandle, BrokerError> {
        self.directory.ensure_connection(conn)
    }

    /// As [`Self::ensure_connection`], dialing with `settings` (what
    /// `BrokerSimRegistry::resolve` returns: the connection's own settings,
    /// or with `host` / `port` replaced by its simulator's, design §4.4).
    pub fn ensure_connection_with_settings(
        &self,
        conn: &PlcConnection,
        settings: Settings,
    ) -> Result<BrokerHandle, BrokerError> {
        self.directory
            .ensure_connection_with_settings(conn, settings)
    }

    /// As [`Self::ensure_connection_with_settings`], for a `simulation = true`
    /// connection: the session reads and writes `device` (what
    /// `BrokerSimRegistry::resolve` returns as `DialTarget::Sim`, R2b-5b)
    /// instead of dialing.
    pub fn ensure_simulated_connection(
        &self,
        conn: &PlcConnection,
        settings: Settings,
        device: banto_broker::SimDevice,
    ) -> Result<BrokerHandle, BrokerError> {
        self.directory
            .ensure_simulated_connection(conn, settings, device)
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
    /// that id (e.g. no rebuild has run yet, or the connection's protocol has
    /// never been one the broker manages). Consumed by
    /// `crate::hub::CollectorManager::broker_status` for `/api/v1/status`
    /// (see `broker_glue`'s module doc, "The two-backoff double bookkeeping"
    /// section, for why a broker-managed connection's status is sourced here
    /// rather than from banto-collect's own status map - as of #131 this
    /// applies to Modbus TCP connections too, not just SLMP).
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
    /// (T7-2, widened past SLMP by #131) diffs this against the registry's
    /// current enabled-and-broker-managed-protocol connection set to find
    /// which ids to [`Self::remove`].
    pub fn connection_ids(&self) -> Vec<i64> {
        self.directory.connection_ids()
    }

    /// Untrack the session for `connection_id`, if one is tracked - proxies
    /// [`SessionDirectory::remove`] (see that type's "Removal" doc section for
    /// the exact mechanism: the task is signalled to stop but not waited for -
    /// use [`Self::stop_and_join`] when the old task must be gone before going
    /// on). Returns `true` if a session was tracked and is now untracked.
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
    /// no broker read client is still mid-`read_batch` when the session it
    /// depends on goes away) - see that binary's doc comment for the full
    /// ordering. A second call is a no-op (the `Option` is already `None`).
    pub async fn shutdown(&self) {
        let supervisor = self.supervisor.lock().await.take();
        if let Some(supervisor) = supervisor {
            supervisor.shutdown().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// R2b-0: the "device" these tests point a session at. The fake drivers
    /// (`crate::test_support::test_drivers`) only dial TCP - a session is up
    /// iff something accepts on the port - so a bound listener stands in for
    /// the SLMP simulator these tests used while core still ran the real
    /// drivers (the kernel completes the handshake from the backlog; nothing
    /// needs to `accept`). None of them looks at the protocol on the wire.
    fn fake_device() -> std::net::TcpListener {
        std::net::TcpListener::bind("127.0.0.1:0").expect("bind fake device")
    }

    fn device_port(device: &std::net::TcpListener) -> u16 {
        device.local_addr().expect("local_addr").port()
    }

    fn slmp_conn(id: i64, port: u16) -> PlcConnection {
        crate::test_support::plc_conn(id, "slmp", port)
    }

    /// T15-4 の核心の単体テスト: `stop_and_join` 後、`write_handle_for` は
    /// (再ダイヤルせず)`None` を返し、`connection_count` は 0 のまま
    /// 留まる - `ensure_connection` を呼べば同じ id のセッションが
    /// **蘇る**(=新規にダイヤルする)ことと対照させ、書き込み経路が
    /// なぜ `ensure_connection` を使ってはいけないかを実証する。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn write_handle_for_does_not_resurrect_a_stopped_session() {
        let device = fake_device();
        let conn = slmp_conn(1, device_port(&device));

        let sessions = HubSessions::new(
            banto_broker::BackoffConfig::default(),
            crate::test_support::test_drivers(),
        );
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
        drop(device);
    }

    // #605 `cpu_only_change_replaces_the_slmp_broker_session` は R2b-0 で
    // `apps/banto-hub/host/tests/broker_glue.rs` へ移した（SLMP のフレームの
    // サブコマンドを実シミュレータで見るので、実ドライバが要る）。
}
