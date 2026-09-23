//! [`CollectorConfig`]: the immutable snapshot a running [`crate::Collector`]
//! is built from, and [`build_config`] which assembles it from the tag
//! registry (I1).
//!
//! ## Boundary (司令塔決定)
//!
//! Reading the registry to build this snapshot lives *here* (I3b), but
//! detecting a later config change and restarting the engine is the caller's
//! job (the future ChronoGazer app) - a `CollectorConfig` is a point-in-time
//! photograph, not a live view. Rebuild it and start a fresh [`crate::Collector`]
//! when definitions change; the frozen-schema `banto-tstore` file rotation
//! (I3a) absorbs the resulting shape change on the storage side.
//!
//! Only `enabled` rows are included, and only reachable ones: a group is
//! collected only if its own `enabled` flag *and* its owning connection's are
//! set; a tag only if its own `enabled` flag is set. Everything downstream
//! (the tstore [`StoreConfig`], the per-connection tasks) is derived from
//! this filtered set, so a disabled connection contributes nothing - no
//! socket, no columns, no cache entries.
//!
//! ## Strict and lenient builds (#414 段階2, 2026-09-23 オーナー決定)
//!
//! [`build_config_from`] (and [`build_config`]) is **strict**: one unusable
//! connection, group or tag fails the whole build with
//! [`CollectError::Config`]. banto-hub relies on that (`last_config_error`
//! keeps the previous catalog running) and its wording is frozen.
//!
//! [`build_config_lenient_from`] **leaves the unusable items out and builds
//! the rest**, returning what it left out as [`ConfigExclusion`]s (a broken
//! connection takes its groups and tags with it, a broken group its tags).
//! ChronoGazer starts collection with it: "開始時に不正なタグ・接続だけを
//! 外し、残りを動かす". There is one interpreter - the strict build *is* the
//! lenient build failing on its first exclusion - so the two cannot
//! disagree about what is unusable. An excluded tag gets no tstore column,
//! so its history reads as `null` (not an error) through `banto-tsquery`.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use banto_core::ListParams;
use banto_plc::{Address, DataType, ModbusTcpConfig, ReadRequest, SlmpConfig, WordOrder};
use banto_tags::{
    scaling::Scaling, CollectionGroup, CollectionGroupService, PlcConnection, PlcConnectionService,
    Tag, TagService,
};
use banto_tstore::{GroupConfig, StoreConfig, TagColumn};
use sqlx::{SqliteConnection, SqlitePool};

use crate::error::CollectError;

/// Stable key helpers - `conn:<id>`/`grp:<id>`/`tag:<id>`. Derived from the
/// registry primary keys (not names, which can be edited) so a restart with
/// unchanged definitions produces the identical `StoreConfig` shape (and thus
/// the same `banto-tstore` config hash - no spurious file rotation) and the
/// same event/status/cache keys.
fn connection_key(id: i64) -> String {
    format!("conn:{id}")
}
fn group_key(id: i64) -> String {
    format!("grp:{id}")
}
fn tag_key(id: i64) -> String {
    format!("tag:{id}")
}

/// The wire protocol a connection speaks. An enum (not the raw string) so
/// protocol dispatch is a single exhaustive `match` in the client factory
/// (`task.rs`) - the design's "プロトコル分岐は factory 関数に隔離". Also
/// decides which address notation a tag's `address` column is parsed under
/// ([`build_request`]) - `Address::parse` (Modbus reference numbers) for
/// [`Protocol::ModbusTcp`], `Address::parse_slmp` (MELSEC device codes, e.g.
/// `D100`) for [`Protocol::Slmp`] (I8, 2026-08-05: `banto-plc`'s SLMP client
/// wired into collection).
///
/// T9-2 (apps/banto-hub/core's broker-routed simulation path): also `pub`
/// (not `pub(crate)`) because `banto_hub_core::broker_glue::BrokerSimRegistry`
/// needs to pass a `Protocol` value to `crate::simulation::start` when it
/// starts an in-process simulator ahead of establishing a broker session -
/// `BrokerSimRegistry::resolve` (`broker_glue.rs`) picks `Protocol::ModbusTcp`
/// or `Protocol::Slmp` depending on the connection's own protocol (see
/// `crate::simulation`'s module doc, "SLMP + banto-hub の broker 経路
/// について").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    ModbusTcp,
    Slmp,
}

/// The protocol-specific client configuration for one connection - exactly
/// one variant per [`Protocol`], carrying the config [`crate::task::build_client`]
/// hands to the matching `PlcClient` constructor. An enum (rather than two
/// separate `Option` fields on [`ConnectionPlan`]) so a connection can never
/// end up with a config for the wrong protocol, or none at all, and
/// [`crate::collector::Collector::start`]'s per-option-timeout override
/// (`connect_timeout`/`response_timeout`) has one `match` to update instead
/// of two independently-fallible `Option` unwraps.
/// `PartialEq` (T7-1, docs/tag-server-design.md §4.3): built from
/// [`ModbusTcpConfig`]/[`SlmpConfig`]'s own derives, so two connections'
/// protocol configs compare structurally - the building block
/// [`crate::collector::Collector::apply_config`] uses to tell "this
/// connection's settings changed" from "byte-for-byte identical, leave its
/// task alone".
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ProtocolConfig {
    ModbusTcp(ModbusTcpConfig),
    Slmp(SlmpConfig),
}

/// A tag's fixed H/HH/L/LL limits (any subset may be set), compared against
/// the *scaled* value. Ordering (`ll <= l <= h <= hh` among the set ones) is
/// already guaranteed by `banto-tags` validation at write time.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct Thresholds {
    pub hh: Option<f64>,
    pub h: Option<f64>,
    pub l: Option<f64>,
    pub ll: Option<f64>,
}

impl Thresholds {
    /// True when no limit is set - the hot loop skips threshold
    /// classification entirely for such tags.
    pub(crate) fn is_empty(&self) -> bool {
        self.hh.is_none() && self.h.is_none() && self.l.is_none() && self.ll.is_none()
    }
}

/// Everything a collection task needs about one tag, resolved once at
/// build time so the hot loop never re-parses an address or re-validates a
/// scaling.
/// `PartialEq` (T7-1, docs/tag-server-design.md §4.3): [`Scaling`] and
/// [`Thresholds`] are already structurally comparable, so this derive is
/// exact - part of the [`crate::collector::Collector::apply_config`] diff
/// chain (`TagPlan` -> [`GroupPlan`] -> [`ConnectionPlan`]).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TagPlan {
    pub key: String,
    /// `None` = no scaling (raw passes through); applied via
    /// `banto_tags::scale_raw`. Never applied to a bit tag: those decode as
    /// `banto_plc::TagValue::Bit` and map straight to 0.0/1.0 (there is no
    /// meaningful "scaled bit"). The tag's wire read lives positionally in
    /// [`GroupPlan::requests`] (aligned 1:1 with `tags`), not duplicated here.
    pub scaling: Option<Scaling>,
    /// Empty when the tag has no limits set (the hot loop skips threshold
    /// classification entirely for those).
    pub thresholds: Thresholds,
}

/// One collection group: a shared-period batch read against its connection.
///
/// `PartialEq` (T7-1, docs/tag-server-design.md §4.3): compares `requests`
/// (already `PartialEq`/`Eq` on [`ReadRequest`]) and `tags` positionally, so
/// two `GroupPlan`s are equal iff every wire read and every resolved tag
/// (scaling, thresholds) is identical - any address/type/scaling/threshold
/// edit inside a group makes its owning [`ConnectionPlan`] compare unequal,
/// which is exactly the "this connection changed" signal
/// [`crate::collector::Collector::apply_config`] diffs on.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct GroupPlan {
    pub key: String,
    pub period: Duration,
    pub period_ms: u32,
    /// Wire reads in tag order (`requests[i]` <-> `tags[i]` <-> tstore column
    /// `c{i+1}`). Passed straight to `PlcClient::read_batch`.
    pub requests: Vec<ReadRequest>,
    pub tags: Vec<TagPlan>,
}

/// One PLC connection and the groups collected over its single socket.
///
/// `PartialEq` (T7-1, docs/tag-server-design.md §4.3): the load-bearing
/// derive for [`crate::collector::Collector::apply_config`]'s diff - two
/// `ConnectionPlan`s with the same `key` compare equal iff their protocol
/// config *and* every group/tag are identical, which is precisely "nothing
/// about this connection's task needs to change". A `ProtocolConfig`-only
/// difference (host/port edited, same groups/tags) still compares unequal
/// here, so the connection is classified "replaced" even though the derived
/// `StoreConfig` (schema) is unaffected - see `apply_config`'s doc comment
/// for why those two facts (connection replaced vs. writer rotated) are
/// deliberately independent.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ConnectionPlan {
    pub key: String,
    pub config: ProtocolConfig,
    pub groups: Vec<GroupPlan>,
    /// T9-1 (docs/ux-plan.md §1, 2026-08-06 オーナー決定): mirrors
    /// `banto_tags::PlcConnection::simulation`. Carried on the plan (not just
    /// consulted once during `build_config`) for two reasons: (1) it must
    /// participate in this struct's `PartialEq` so toggling simulation on/off
    /// with every other setting unchanged still makes `apply_config` (T7-1)
    /// classify the connection "replaced" - the ordinary "this connection's
    /// task gets stopped and respawned" path is exactly what starting/
    /// stopping the in-process simulator needs to piggyback on (see
    /// `crate::collector`'s module doc, T9-1 addendum); (2)
    /// `crate::collector::Collector` reads it at task-spawn time to decide
    /// whether to start a simulator and substitute its loopback address for
    /// `config`'s real host/port before the task ever sees the plan - see
    /// `crate::simulation`'s module doc for why that substitution happens
    /// there and not here (this function keeps producing the DB-truth plan;
    /// `Collector` is what needs a stable address across repeated
    /// `apply_config` calls to diff against).
    pub simulation: bool,
}

/// The immutable configuration snapshot [`crate::Collector::start`] consumes.
/// Opaque by design: build it with [`build_config`] and hand it to `start`;
/// its internals (the per-connection plans and the derived tstore
/// [`StoreConfig`]) are this crate's concern.
/// `PartialEq` (T7-1, docs/tag-server-design.md §4.3): a convenience derive
/// over the now-`PartialEq` `ConnectionPlan`/`StoreConfig` fields -
/// [`crate::collector::Collector::apply_config`] itself diffs
/// connection-by-connection (see `ConnectionPlan`'s doc comment) rather than
/// comparing whole configs, but this derive costs nothing and lets tests
/// assert "the applied config is exactly X" directly.
#[derive(Debug, Clone, PartialEq)]
pub struct CollectorConfig {
    pub(crate) connections: Vec<ConnectionPlan>,
    /// The frozen-schema store shape derived from the same filtered group/tag
    /// set - one `samples_<n>` table per collected group, columns in tag
    /// order. Handed to `TsWriter::open`.
    pub(crate) store_config: StoreConfig,
}

impl CollectorConfig {
    /// Total number of collected groups across every connection - a cheap
    /// sanity accessor for callers/tests (e.g. "did enabling change how much
    /// gets collected"). Zero is possible (nothing enabled) and rejected by
    /// [`crate::Collector::start`], not here.
    pub fn group_count(&self) -> usize {
        self.connections.iter().map(|c| c.groups.len()).sum()
    }

    /// Total number of collected tags across every group.
    pub fn tag_count(&self) -> usize {
        self.connections
            .iter()
            .flat_map(|c| c.groups.iter())
            .map(|g| g.tags.len())
            .sum()
    }

    /// T9-2 (docs/ux-plan.md §1 / apps/banto-hub/core/src/broker_glue.rs's
    /// "T9-1/T9-2 note", option (a)): force `ConnectionPlan::simulation =
    /// false` for every connection whose key is in `connection_keys` -
    /// banto-hub's `CollectorManager` calls this after `build_config` to
    /// stop `Collector` from starting a second, unused in-process simulator
    /// for a broker-managed connection (SLMP or, since #337, Modbus TCP)
    /// whose dial address `CollectorManager`'s own `BrokerSimRegistry` has
    /// already substituted
    /// before the broker session was established (`Collector`'s own
    /// simulator substitution happens too late in that path - see
    /// `crate::simulation`'s module doc). A key not present in
    /// `self.connections` is silently ignored (e.g. a connection with zero
    /// collectible groups was already dropped by `build_config`).
    pub fn suppress_simulation_for(&mut self, connection_keys: &std::collections::HashSet<String>) {
        for conn in &mut self.connections {
            if connection_keys.contains(&conn.key) {
                conn.simulation = false;
            }
        }
    }

    /// T9-2 (found necessary by this crate's own E2E coverage of the T9-2
    /// simulation-toggle path, `apps/banto-hub/core/tests/t9_simulation.rs`):
    /// overwrite the `host`/`port` of the broker-routed connection plan keyed
    /// by `key` (no-op if `key` is absent).
    ///
    /// **#337 (2026-09-08)**: this used to touch `ProtocolConfig::Slmp` plans
    /// only, deliberately - a Modbus plan's `simulation` flag was never
    /// suppressed back then (its collection reads were not broker-routed), so
    /// `apply_config`'s diff already noticed a simulation toggle on its own
    /// and no synthetic diff signal was needed. Once #337 made Modbus reads
    /// broker-routed, `CollectorManager` started calling
    /// [`Self::suppress_simulation_for`] for Modbus plans too, which flattens
    /// that natural signal away - so a Modbus plan now needs exactly the same
    /// stamping as an SLMP one, for exactly the reasons below.
    ///
    /// This is purely a *diffing* signal for [`crate::Collector::apply_config`],
    /// not a real dial instruction - a broker-routed connection is never
    /// actually dialed from this `ProtocolConfig` (its `banto_collect::PlcClient`
    /// is always the injected `BrokerReadClient`, wrapping a
    /// `banto_broker::ReadOnlyHandle` banto-hub already resolved - see
    /// `crate::task::ClientFactory`/`apps/banto-hub/core/src/broker_glue.rs`'s
    /// `hub_client_factory`). But `apply_config`'s diff is *only*
    /// `ConnectionPlan == ConnectionPlan` (see that struct's own `PartialEq`
    /// doc comment) - a connection whose plan compares equal to its previous
    /// one is classified "unchanged" and its already-running task is left
    /// completely untouched, **including the `ClientFactory` closure it
    /// captured at spawn time** (`crate::task::run_connection`'s `ctx.factory`
    /// is fixed for the task's whole lifetime; a factory rebuilt on a later
    /// rebuild is simply never seen by an "unchanged" task).
    ///
    /// For a broker-routed connection, `ConnectionPlan::simulation` is
    /// unconditionally forced `false` by [`Self::suppress_simulation_for`]
    /// (both before and after any simulation toggle), and this plan's
    /// `host`/`port` otherwise mirror the registry row verbatim
    /// (`crate::config::slmp_config_for`/`modbus_config_for`) - neither field reflects
    /// the *actual resolved dial target* a broker-routed connection uses,
    /// which lives entirely outside this plan (`apps/banto-hub/core/src/broker_glue.rs`'s
    /// `BrokerSimRegistry`). So toggling `simulation` on/off (or editing the
    /// connection's real host/port while it stays broker-routed) can leave
    /// this plan comparing byte-for-byte equal across rebuilds even though
    /// the broker session actually moved underneath it - "unchanged" would
    /// then leave the running task's `ClientFactory` wired to a
    /// `ReadOnlyHandle` for a broker session `HubSessions::remove` +
    /// `ensure_connection` has already superseded, silently reading a stale
    /// or dead session forever.
    ///
    /// banto-hub's `CollectorManager` calls this with the SAME resolved
    /// `(host, port)` `BrokerSimRegistry::resolve` just computed for every
    /// broker-routed connection (regardless of whether `resolve`
    /// reported `changed` - applying it unconditionally is harmless: for an
    /// unchanged target the value written back is identical to what was
    /// already there, so the plan still compares equal and the connection
    /// stays correctly classified "unchanged"). When the resolved target DID
    /// change, this now makes the plan compare unequal, so `apply_config`
    /// classifies the connection "replaced" - stopping the stale task and
    /// spawning a fresh one with the freshly-built `ClientFactory`
    /// (`CollectorManager::rebuild` builds it from this same rebuild's
    /// `broker_handles`, i.e. the NEW broker session) - exactly the "this
    /// connection's task gets stopped and respawned" path `BrokerSimRegistry::resolve`'s
    /// own doc comment relies on to make the swap actually observable.
    pub fn set_broker_dial_target(&mut self, key: &str, host: String, port: i64) {
        if let Some(conn) = self.connections.iter_mut().find(|c| c.key == key) {
            let port = u16::try_from(port).ok();
            match &mut conn.config {
                ProtocolConfig::Slmp(cfg) => {
                    cfg.host = host;
                    if let Some(port) = port {
                        cfg.port = port;
                    }
                }
                ProtocolConfig::ModbusTcp(cfg) => {
                    cfg.host = host;
                    if let Some(port) = port {
                        cfg.port = port;
                    }
                }
            }
        }
    }
}

/// One logical, point-in-time registry input used by both catalog and
/// collector preflight builders. The three vectors are intentionally kept as
/// the registry row types so callers do not need a second DTO conversion and
/// all existing validation remains in the builders below.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RegistrySnapshot {
    pub connections: Vec<PlcConnection>,
    pub groups: Vec<CollectionGroup>,
    pub tags: Vec<Tag>,
}

impl RegistrySnapshot {
    /// Read the registry rows once into a reusable logical snapshot.
    pub async fn load(pool: &SqlitePool) -> Result<Self, CollectError> {
        Ok(Self {
            connections: PlcConnectionService::new(pool.clone())
                .list(ListParams::default())
                .await?
                .rows,
            groups: CollectionGroupService::new(pool.clone())
                .list(ListParams::default())
                .await?
                .rows,
            tags: TagService::new(pool.clone())
                .list(ListParams::default())
                .await?
                .rows,
        })
    }

    /// Read the same registry snapshot from an already-open SQLite connection.
    /// Callers use this with a transaction/savepoint so proposed mutations and
    /// all three registry reads observe one transaction-local state.
    pub async fn load_connection(connection: &mut SqliteConnection) -> Result<Self, CollectError> {
        let connections = sqlx::query_as::<_, PlcConnection>(
            "SELECT id, name, protocol, host, port, unit_id, enabled, simulation, word_order, \
             database, username, password \
             FROM plc_connections ORDER BY id",
        )
        .fetch_all(&mut *connection)
        .await
        .map_err(|err| CollectError::Registry(banto_core::BantoError::Storage(err.to_string())))?;
        let groups = sqlx::query_as::<_, CollectionGroup>(
            "SELECT id, name, plc_connection_id, period_ms, enabled, default_writable, \
             query_sql \
             FROM collection_groups ORDER BY id",
        )
        .fetch_all(&mut *connection)
        .await
        .map_err(|err| CollectError::Registry(banto_core::BantoError::Storage(err.to_string())))?;
        let tags = sqlx::query_as::<_, Tag>(
            "SELECT id, name, collection_group_id, address, data_type, \
             string_length, string_encoding, raw_lo, raw_hi, eng_lo, eng_hi, unit, decimals, \
             threshold_h, threshold_hh, threshold_l, threshold_ll, enabled, \
             writable, tag_kind, expression, retain, revision FROM tags ORDER BY id",
        )
        .fetch_all(&mut *connection)
        .await
        .map_err(|err| CollectError::Registry(banto_core::BantoError::Storage(err.to_string())))?;

        Ok(Self {
            connections,
            groups,
            tags,
        })
    }
}

/// Connection ids that have at least one *enabled* [`CollectionGroup`] -
/// the exact predicate [`build_config_from`] itself uses below to decide
/// whether a connection gets a [`ConnectionPlan`] at all (see that
/// function's "A connection with no collected groups gets no task and no
/// socket" comment). Note this is a *group*-level check, not a *tag*-level
/// one: an enabled group with zero tags still counts here, matching
/// `build_config_from`'s own behaviour (a group is pushed into its
/// `group_plans` regardless of whether any of its tags survived the loop
/// below) - "収集対象グループが1つ以上ある", not "タグが1件でもある".
///
/// Deliberately does **not** replicate `build_config_from`'s
/// protocol-specific parsing (`parse_protocol`/`modbus_config_for`/
/// `slmp_config_for`, all of which can fail for reasons unrelated to "does
/// this connection have anything to collect") - a caller asking only "is
/// this connection tagless" should not be able to fail over a malformed
/// port number on an unrelated field. This means the two can disagree only
/// when the connection's own config is already invalid, in which case
/// `build_config_from`/`build_config` fail outright and surface that error
/// through their own, separate path.
///
/// Exposed so callers outside this crate (banto-hub's write-side broker
/// session sync, T19 S2-a/UX-48: `crate::hub::CollectorManager`'s session
/// sync in `apps/banto-hub/core/src/hub.rs`) can decide "does this
/// connection need a session" from the exact same input data
/// (`RegistrySnapshot`) and the exact same rule the collector itself uses,
/// rather than maintaining a second, potentially drifting definition of
/// "tagless connection".
pub fn connections_with_collected_groups(groups: &[CollectionGroup]) -> HashSet<i64> {
    groups
        .iter()
        .filter(|group| group.enabled)
        .map(|group| group.plc_connection_id)
        .collect()
}

/// Assemble a [`CollectorConfig`] from the tag registry in `pool` (the app's
/// shared database). Reads via `banto-tags`' services and keeps only enabled,
/// reachable rows (see this module's doc comment).
///
/// Fails with [`CollectError::Config`] if any *included* tag's stored address
/// does not parse under the PLC addressing rules or its `data_type` is
/// outside the known vocabulary - `banto-tags` deliberately does not validate
/// those (address format is I2/I3b's concern), so this is where a
/// misconfigured tag is caught, before any socket is opened. Registry read
/// failures surface as [`CollectError::Registry`].
pub async fn build_config(pool: &SqlitePool) -> Result<CollectorConfig, CollectError> {
    let snapshot = RegistrySnapshot::load(pool).await?;
    build_config_from(&snapshot)
}

/// Build a collector configuration from an already-loaded registry snapshot.
/// This is the shared preflight path used by banto-hub's catalog commit and
/// run application; it preserves the filtering and validation semantics of
/// [`build_config`].
///
/// #414 段階2: this is now [`build_config_lenient_from`] plus "fail on the
/// first exclusion". There is **one** interpreter - the lenient builder walks
/// the registry in exactly the order this function always did (connections
/// by id; per connection: protocol, then port/unit id; then its groups by
/// id; per group: every tag by id, *then* the period), and records each
/// failure as a [`ConfigExclusion`] where this function used to `?` out.
/// So the first exclusion is exactly the item this function used to fail
/// on, and [`ConfigExclusion::strict_error`] rebuilds the very same
/// [`CollectError::Config`] text - banto-hub's `last_config_error` does not
/// change by a character (the goldens in this module's tests were run
/// against the pre-段階2 implementation first).
pub fn build_config_from(snapshot: &RegistrySnapshot) -> Result<CollectorConfig, CollectError> {
    let (config, exclusions) = build_config_lenient_from(snapshot);
    match exclusions.iter().find_map(ConfigExclusion::strict_error) {
        Some(err) => Err(err),
        None => Ok(config),
    }
}

/// Which level of the registry a [`ConfigExclusion`] took out of collection
/// (#414 段階2) - the three units [`build_config_from`] can fail on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExclusionUnit {
    Connection,
    Group,
    Tag,
}

impl ExclusionUnit {
    /// Stable wire spelling (`connection` / `group` / `tag`).
    pub fn as_str(&self) -> &'static str {
        match self {
            ExclusionUnit::Connection => "connection",
            ExclusionUnit::Group => "group",
            ExclusionUnit::Tag => "tag",
        }
    }
}

/// Why [`build_config_lenient_from`] left an item out (#414 段階2).
///
/// The first five variants are the item's *own* fault - each one is exactly
/// one of the `CollectError::Config` cases [`build_config_from`] raises. The
/// last two are cascades: the item itself may be fine, but its connection or
/// group was excluded, so there is nothing to collect it through. A cascade
/// names its parent so the list can be followed back to the item that needs
/// fixing.
///
/// Every value in here (protocol string, port, unit id, period, address,
/// data type, `banto_plc`'s parse error for the address, parent names) is
/// registry data the row already carries - never a host name, credential or
/// file path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExclusionReason {
    /// `plc_connections.protocol` is not `modbus-tcp` / `slmp`.
    UnsupportedProtocol { protocol: String },
    /// `plc_connections.port` does not fit a TCP port (`u16`).
    InvalidPort { port: i64 },
    /// `plc_connections.unit_id` does not fit a Modbus unit id (`u8`).
    InvalidUnitId { unit_id: i64 },
    /// `collection_groups.period_ms` does not fit `u32`.
    InvalidPeriod { period_ms: i64 },
    /// The tag's address/data type cannot be read under its connection's
    /// protocol (the same [`TagAddressIssue`] the save-time check returns).
    InvalidTag {
        address: String,
        data_type: String,
        issue: TagAddressIssue,
    },
    /// The owning connection was excluded.
    ConnectionExcluded { connection_name: String },
    /// The owning group was excluded.
    GroupExcluded { group_name: String },
}

impl ExclusionReason {
    /// Machine-readable classification (stable wire spelling).
    pub fn code(&self) -> &'static str {
        match self {
            ExclusionReason::UnsupportedProtocol { .. } => "unsupportedProtocol",
            ExclusionReason::InvalidPort { .. } => "invalidPort",
            ExclusionReason::InvalidUnitId { .. } => "invalidUnitId",
            ExclusionReason::InvalidPeriod { .. } => "invalidPeriod",
            ExclusionReason::InvalidTag { issue, .. } => match issue {
                TagAddressIssue::InvalidAddress { .. } => "invalidAddress",
                TagAddressIssue::UnknownDataType => "unknownDataType",
                TagAddressIssue::BitAddressOnNonBitType => "bitAddressOnNonBitType",
            },
            ExclusionReason::ConnectionExcluded { .. } => "connectionExcluded",
            ExclusionReason::GroupExcluded { .. } => "groupExcluded",
        }
    }

    /// A human-readable reason, for a list that shows the item's unit and
    /// name next to it (so, unlike the strict error text, it does not repeat
    /// the name). For a tag it is **exactly** [`TagAddressIssue::message`] -
    /// the wording the save-time check (#414 段階1) rejects the same tag with.
    pub fn message(&self) -> String {
        match self {
            ExclusionReason::UnsupportedProtocol { protocol } => {
                format!("プロトコル {protocol} は未対応です（modbus-tcp / slmp のみ対応）")
            }
            ExclusionReason::InvalidPort { port } => format!("ポート番号が不正です: {port}"),
            ExclusionReason::InvalidUnitId { unit_id } => {
                format!("ユニットIDが不正です: {unit_id}")
            }
            ExclusionReason::InvalidPeriod { period_ms } => {
                format!("収集周期（period_ms）が不正です: {period_ms}")
            }
            ExclusionReason::InvalidTag { issue, .. } => issue.message(),
            ExclusionReason::ConnectionExcluded { connection_name } => {
                format!("所属する PLC接続「{connection_name}」が除外されたため、収集しません")
            }
            ExclusionReason::GroupExcluded { group_name } => {
                format!("所属する収集グループ「{group_name}」が除外されたため、収集しません")
            }
        }
    }
}

/// One registry item [`build_config_lenient_from`] left out of the
/// [`CollectorConfig`] it returned (#414 段階2).
///
/// Only enabled, otherwise-collected rows can be excluded: a disabled row, a
/// `virtual`/`postgres` connection and a `string` tag are not collected in
/// the first place (see [`build_config_from`]) and never show up here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigExclusion {
    pub unit: ExclusionUnit,
    /// The registry row id (`plc_connections.id` / `collection_groups.id` /
    /// `tags.id`, according to `unit`).
    pub id: i64,
    /// `conn:<id>` / `grp:<id>` / `tag:<id>` - the same keys the collector
    /// uses for its status map, events and current values.
    pub key: String,
    /// The row's `name`.
    pub name: String,
    pub reason: ExclusionReason,
}

impl ConfigExclusion {
    fn connection(conn: &PlcConnection, reason: ExclusionReason) -> Self {
        Self {
            unit: ExclusionUnit::Connection,
            id: conn.id,
            key: connection_key(conn.id),
            name: conn.name.clone(),
            reason,
        }
    }

    fn group(group: &CollectionGroup, reason: ExclusionReason) -> Self {
        Self {
            unit: ExclusionUnit::Group,
            id: group.id,
            key: group_key(group.id),
            name: group.name.clone(),
            reason,
        }
    }

    fn tag(tag: &Tag, reason: ExclusionReason) -> Self {
        Self {
            unit: ExclusionUnit::Tag,
            id: tag.id,
            key: tag_key(tag.id),
            name: tag.name.clone(),
            reason,
        }
    }

    /// The [`CollectError::Config`] the strict [`build_config_from`] raises
    /// for this item - **the pre-#414-段階2 wording, character for
    /// character** (it names the item, since a config-build failure has no
    /// form field to sit next to). `None` for a cascade: the strict build
    /// has already failed on the parent by then.
    pub fn strict_error(&self) -> Option<CollectError> {
        let name = &self.name;
        let message = match &self.reason {
            ExclusionReason::UnsupportedProtocol { protocol } => format!(
                "接続 {name} のプロトコル {protocol} は未対応です（modbus-tcp / slmp のみ対応）"
            ),
            ExclusionReason::InvalidPort { port } => {
                format!("接続 {name} のポート番号が不正です: {port}")
            }
            ExclusionReason::InvalidUnitId { unit_id } => {
                format!("接続 {name} のユニットIDが不正です: {unit_id}")
            }
            ExclusionReason::InvalidPeriod { period_ms } => {
                format!("グループ {name} の period_ms が不正です: {period_ms}")
            }
            ExclusionReason::InvalidTag {
                address,
                data_type,
                issue,
            } => match issue {
                TagAddressIssue::InvalidAddress { reason, .. } => {
                    format!("タグ {name} のアドレス {address} が不正です: {reason}")
                }
                TagAddressIssue::UnknownDataType => {
                    format!("タグ {name} のデータ型 {data_type} は未対応です")
                }
                TagAddressIssue::BitAddressOnNonBitType => format!(
                    "タグ {name} のアドレス {address} はビット指定アドレスです。ビット指定アドレスは \
                     data_type=bit のタグでのみ使えます（現在のデータ型: {data_type}）"
                ),
            },
            ExclusionReason::ConnectionExcluded { .. } | ExclusionReason::GroupExcluded { .. } => {
                return None
            }
        };
        Some(CollectError::Config(message))
    }
}

/// #414 段階2 (2026-09-23 オーナー決定「開始時に不正なタグ・接続だけを外し、
/// 残りを動かす」): build a [`CollectorConfig`] from `snapshot`, **leaving
/// out** every connection, group and tag [`build_config_from`] would fail
/// on, and return what was left out.
///
/// - A connection whose protocol/port/unit id is unusable is excluded
///   together with all of its enabled groups and their tags (each listed as
///   [`ExclusionReason::ConnectionExcluded`]).
/// - A group whose period is unusable is excluded together with its tags
///   ([`ExclusionReason::GroupExcluded`] for the tags that were fine on
///   their own; a tag that is also broken itself is listed with its own
///   reason, ahead of the group - the order [`build_config_from`] checks in).
/// - A tag whose address/data type is unusable is excluded alone; the rest
///   of its group is collected. **It gets no tstore column**, so its history
///   in files written while it was excluded reads as `null`
///   (`banto-tsquery` resolves a `tag_key` a file does not describe to
///   "no value", not to an error).
///
/// Same filtering, same order, same interpreter as [`build_config_from`]
/// (which is this function plus "fail on the first exclusion"). banto-hub
/// keeps using the strict one; chronogazer starts collection with this one.
pub fn build_config_lenient_from(
    snapshot: &RegistrySnapshot,
) -> (CollectorConfig, Vec<ConfigExclusion>) {
    let connections = &snapshot.connections;
    let groups = &snapshot.groups;
    let tags = &snapshot.tags;

    // Index groups by connection and tags by group, so each connection's plan
    // is one pass rather than repeated full scans.
    let mut groups_by_connection: HashMap<i64, Vec<&CollectionGroup>> = HashMap::new();
    for group in groups {
        if group.enabled {
            groups_by_connection
                .entry(group.plc_connection_id)
                .or_default()
                .push(group);
        }
    }
    let mut tags_by_group: HashMap<i64, Vec<&Tag>> = HashMap::new();
    for tag in tags {
        if tag.enabled {
            tags_by_group
                .entry(tag.collection_group_id)
                .or_default()
                .push(tag);
        }
    }

    // Deterministic order everywhere (by id) so the derived StoreConfig - and
    // therefore its config hash - is stable across restarts.
    //
    // T6-2 (docs/tag-server-design.md §4.2/§4.3(a)): a `"virtual"` connection
    // (banto-hub's auto-provisioned `calc`/`mem` reserved namespace for
    // computed/internal tags) is excluded from collection here, not treated
    // as a config error - it never has a socket to open, so silently
    // contributing nothing is the correct behaviour for a connection that by
    // design has no wire protocol at all (unlike an actually-unsupported
    // protocol string, which `parse_protocol` below still rejects). Any tags
    // registered under it (computed/internal, per banto-tags' own
    // `calc`/`mem` placement rule) are evaluated/stored entirely by
    // `apps/banto-hub/core/src/computed.rs`'s `ComputedEngine`/
    // `ServerTagStore`, never by this recorder pipeline.
    //
    // S1 (docs/banto-hub-external-db-design.md §4.1・§7 スライス表「S1」,
    // 2026-09-06): a `"postgres"` connection ([`PlcConnection::is_db_source`])
    // gets the identical exclusion, for the identical reason - it is not a
    // wire-protocol PLC connection either, and `parse_protocol` below has no
    // variant for it. This is defense-in-depth as much as anything: S1 also
    // forbids creating any collection group under a `"postgres"` connection
    // (`banto_tags::collection_group::CollectionGroupService`), so in
    // practice such a connection is always tagless - but without this
    // exclusion, merely *enabling* a `"postgres"` connection (zero groups or
    // not) would make `parse_protocol` reject the whole config, since the
    // loop below runs over every enabled connection regardless of whether it
    // has any groups yet.
    //
    // (Neither of these is a `ConfigExclusion`: they are not collected by
    // design, not "left out because broken".)
    let mut enabled_connections: Vec<&PlcConnection> = connections
        .iter()
        .filter(|c| c.enabled && c.protocol != "virtual" && !c.is_db_source())
        .collect();
    enabled_connections.sort_by_key(|c| c.id);

    let mut connection_plans = Vec::new();
    let mut store_groups = Vec::new();
    let mut exclusions = Vec::new();

    for conn in enabled_connections {
        let mut conn_groups = groups_by_connection.remove(&conn.id).unwrap_or_default();
        conn_groups.sort_by_key(|g| g.id);

        let resolved = parse_protocol(&conn.protocol).and_then(|protocol| {
            let client_config = match protocol {
                Protocol::ModbusTcp => ProtocolConfig::ModbusTcp(modbus_config_for(conn)?),
                Protocol::Slmp => ProtocolConfig::Slmp(slmp_config_for(conn)?),
            };
            Ok((protocol, client_config))
        });
        let (protocol, client_config) = match resolved {
            Ok(resolved) => resolved,
            Err(reason) => {
                // The connection first (it is what needs fixing), then
                // everything that was to be collected through it.
                exclusions.push(ConfigExclusion::connection(conn, reason));
                for group in conn_groups {
                    exclusions.push(ConfigExclusion::group(
                        group,
                        ExclusionReason::ConnectionExcluded {
                            connection_name: conn.name.clone(),
                        },
                    ));
                    let mut group_tags = tags_by_group.remove(&group.id).unwrap_or_default();
                    group_tags.sort_by_key(|t| t.id);
                    for tag in group_tags {
                        if tag.data_type == banto_tags::STRING_DATA_TYPE {
                            continue;
                        }
                        exclusions.push(ConfigExclusion::tag(
                            tag,
                            ExclusionReason::ConnectionExcluded {
                                connection_name: conn.name.clone(),
                            },
                        ));
                    }
                }
                continue;
            }
        };

        let mut group_plans = Vec::new();
        let mut conn_store_groups = Vec::new();
        for group in conn_groups {
            let mut group_tags = tags_by_group.remove(&group.id).unwrap_or_default();
            group_tags.sort_by_key(|t| t.id);

            let mut requests = Vec::with_capacity(group_tags.len());
            let mut tag_plans = Vec::with_capacity(group_tags.len());
            let mut store_columns = Vec::with_capacity(group_tags.len());
            // Tags that were fine on their own - listed as cascades if the
            // group itself turns out to be excluded below.
            let mut readable_tags = Vec::with_capacity(group_tags.len());

            for tag in group_tags {
                // S1 (relay-wright 文字列タグ): "string" is registry-legal
                // vocabulary that the recorder pipeline must NEVER see - the
                // banto-tstore schema is frozen numeric-only, and there is no
                // meaningful sample for a string anyway. Skip such tags
                // entirely, exactly like a disabled tag: never read, never a
                // store column, and the rest of the group still collects.
                // This must happen *before* build_request, whose
                // DataType::parse would otherwise fail the whole config build
                // over a tag that belongs to a different app (relay-wright's
                // S2 engine is the consumer of string tags).
                if tag.data_type == banto_tags::STRING_DATA_TYPE {
                    continue;
                }
                let request = match build_request(tag, protocol) {
                    Ok(request) => request,
                    Err(issue) => {
                        // #414 段階2: this tag alone is left out - no read,
                        // no store column (its history reads as null), and
                        // the rest of the group still collects.
                        exclusions.push(ConfigExclusion::tag(
                            tag,
                            ExclusionReason::InvalidTag {
                                address: tag.address.clone(),
                                data_type: tag.data_type.clone(),
                                issue,
                            },
                        ));
                        continue;
                    }
                };
                readable_tags.push(tag);
                requests.push(request);
                tag_plans.push(TagPlan {
                    key: tag_key(tag.id),
                    scaling: tag.scaling(),
                    thresholds: Thresholds {
                        hh: tag.threshold_hh,
                        h: tag.threshold_h,
                        l: tag.threshold_l,
                        ll: tag.threshold_ll,
                    },
                });
                store_columns.push(TagColumn {
                    key: tag_key(tag.id),
                    name: tag.name.clone(),
                    data_type: tag.data_type.clone(),
                    unit: tag.unit.clone(),
                    // banto-tags validates decimals in 0..=6, so the cast is
                    // always in range; clamp defensively rather than wrap.
                    decimals: tag.decimals.clamp(0, u8::MAX as i64) as u8,
                });
            }

            // The period is checked after the tags - the order
            // `build_config_from` has always failed in (a bad tag in a group
            // with a bad period is reported as the tag's error).
            let Ok(period_ms) = u32::try_from(group.period_ms) else {
                exclusions.push(ConfigExclusion::group(
                    group,
                    ExclusionReason::InvalidPeriod {
                        period_ms: group.period_ms,
                    },
                ));
                for tag in readable_tags {
                    exclusions.push(ConfigExclusion::tag(
                        tag,
                        ExclusionReason::GroupExcluded {
                            group_name: group.name.clone(),
                        },
                    ));
                }
                continue;
            };

            let gkey = group_key(group.id);
            conn_store_groups.push(GroupConfig {
                key: gkey.clone(),
                name: group.name.clone(),
                period_ms,
                tags: store_columns,
            });
            group_plans.push(GroupPlan {
                key: gkey,
                period: Duration::from_millis(period_ms as u64),
                period_ms,
                requests,
                tags: tag_plans,
            });
        }

        // A connection with no collected groups gets no task and no socket -
        // reading nothing from a PLC is pointless. Skip it entirely (and do
        // not contribute its - empty - store groups).
        if group_plans.is_empty() {
            continue;
        }

        store_groups.extend(conn_store_groups);
        connection_plans.push(ConnectionPlan {
            key: connection_key(conn.id),
            config: client_config,
            groups: group_plans,
            simulation: conn.simulation,
        });
    }

    (
        CollectorConfig {
            connections: connection_plans,
            store_config: StoreConfig {
                groups: store_groups,
            },
        },
        exclusions,
    )
}

/// #414 段階2: just the exclusions [`build_config_lenient_from`] would make
/// for `snapshot` - "which registry items would be left out if collection
/// were started now". For a registry editor's marks (chronogazer's `/tags`),
/// so the verdict comes from the one builder instead of a re-implementation.
pub fn config_exclusions(snapshot: &RegistrySnapshot) -> Vec<ConfigExclusion> {
    build_config_lenient_from(snapshot).1
}

fn parse_protocol(protocol: &str) -> Result<Protocol, ExclusionReason> {
    match protocol {
        "modbus-tcp" => Ok(Protocol::ModbusTcp),
        "slmp" => Ok(Protocol::Slmp),
        other => Err(ExclusionReason::UnsupportedProtocol {
            protocol: other.to_string(),
        }),
    }
}

/// Build a [`ModbusTcpConfig`] from a `"modbus-tcp"`-protocol
/// [`PlcConnection`] row. `host`/`port`/`unit_id`/`word_order` all come from
/// the registry - `word_order` from the same `word_order` column
/// [`slmp_config_for`] reads (migration `0010_plc_connections_add_word_order.sql`,
/// P3-b, 2026-08-12); it is a registry field for Modbus exactly as much as
/// it is for SLMP.
///
/// **Bug fixed 2026-09-08**: until this change, this fn ignored that column
/// and hardcoded `word_order: WordOrder::default()`
/// (`ModbusTcpConfig::default()`'s `WordOrder::HighLow`) with a comment
/// claiming the column did not exist - stale ever since P3-b actually added
/// it. Meanwhile `banto_broker::modbus_config_for` (the read-on-demand and
/// write path) already called `parse_word_order(&conn.word_order)` for
/// Modbus. So a Modbus connection left at the column's then-default
/// (`'low_high'`) got `WordOrder::LowHigh` for on-demand reads/writes but
/// `WordOrder::HighLow` for collection polling - the same u32/f32 tag
/// decoded with its two registers swapped depending on which path touched
/// it. The mismatch surfaced while adding the 64bit types (`i64`/`u64`/
/// `f64`, 4 registers) for issue #325.
///
/// Owner decision (2026-09-08): Modbus is standardized on `'high_low'`.
/// Existing `modbus-tcp` rows are backfilled to `'high_low'` and new Modbus
/// connections now default to `'high_low'` too, both by a `banto-tags`-side
/// migration (out of scope here) - this fn's job is only to stop ignoring
/// the column, which is what the `parse_word_order` call below now does.
///
/// The 64bit types added for #325 apply this same `word_order` across their
/// two register pairs exactly like the existing 32bit types do - there is no
/// separate word-order concept for them. A real device that needs
/// `'low_high'` here: the OMRON KM-D1-ETN power meter.
fn modbus_config_for(conn: &PlcConnection) -> Result<ModbusTcpConfig, ExclusionReason> {
    let port =
        u16::try_from(conn.port).map_err(|_| ExclusionReason::InvalidPort { port: conn.port })?;
    let unit_id = u8::try_from(conn.unit_id).map_err(|_| ExclusionReason::InvalidUnitId {
        unit_id: conn.unit_id,
    })?;
    Ok(ModbusTcpConfig {
        host: conn.host.clone(),
        port,
        unit_id,
        // `parse_word_order`'s fallback to `WordOrder::LowHigh` for an
        // unrecognized string is unconditional regardless of protocol, so a
        // hand-edited/pre-migration-0010 Modbus row fails open to
        // `WordOrder::LowHigh` here too - not to `ModbusTcpConfig::default()`'s
        // `WordOrder::HighLow`. This matches `banto_broker::modbus_config_for`,
        // which reaches the same fallback through its own copy of
        // `parse_word_order`.
        word_order: parse_word_order(&conn.word_order),
        ..ModbusTcpConfig::default()
    })
}

/// I8 (2026-08-05): build an [`SlmpConfig`] from a `"slmp"`-protocol
/// [`PlcConnection`] row. `host`/`port`/`word_order` come from the registry -
/// `word_order` since P3-b（監査指摘 2026-08-12, migration
/// `0010_plc_connections_add_word_order.sql`); before that this fn always
/// fell back to [`SlmpConfig::default`]'s `WordOrder::LowHigh` regardless of
/// what the device actually needed, silently byte-swapping any u32/f32 tag
/// read through a connection that required `WordOrder::HighLow`.
/// `banto-tags::PlcConnection` still has no columns for SLMP's CPU series or
/// access route (network/PC/IO/area id; `unit_id` is Modbus-only and is not
/// read here), so those fields still fall back to [`SlmpConfig::default`] (R
/// series CPU, the CPU-on-the-other-end access route). **Known
/// limitation**: a device that needs a non-default CPU series or a routed
/// (not-directly-connected) access route cannot be configured through the
/// registry today - adding those would need new `plc_connections` columns,
/// deliberately out of scope for both I8 and P3-b (P3-b task instructions:
/// "word_order に集中し、他は trivial でない限り実装しない").
/// `connect_timeout`/`response_timeout` are overridden uniformly by
/// [`crate::collector::Collector::start`] from [`crate::collector::CollectorOptions`],
/// exactly like [`modbus_config_for`]'s.
fn slmp_config_for(conn: &PlcConnection) -> Result<SlmpConfig, ExclusionReason> {
    let port =
        u16::try_from(conn.port).map_err(|_| ExclusionReason::InvalidPort { port: conn.port })?;
    Ok(SlmpConfig {
        host: conn.host.clone(),
        port,
        // P3-b: `parse_word_order` mirrors `banto_broker`'s private fn of the
        // same name/behavior (fails open to `WordOrder::LowHigh` for
        // anything unrecognized - see its own doc comment) - not shared
        // because the two crates have no dependency relationship to hang a
        // shared helper off of, and it is three lines.
        word_order: parse_word_order(&conn.word_order),
        ..SlmpConfig::default()
    })
}

/// Shared by both [`slmp_config_for`] and [`modbus_config_for`] - originally
/// written for SLMP only (see [`slmp_config_for`]'s doc comment for why this
/// is a copy of `banto_broker`'s identical fn rather than a shared helper),
/// extended to Modbus by the 2026-09-08 fix documented on
/// [`modbus_config_for`].
fn parse_word_order(value: &str) -> WordOrder {
    match value {
        "high_low" => WordOrder::HighLow,
        _ => WordOrder::LowHigh,
    }
}

/// Parse a tag's address + data type into a wire [`ReadRequest`]. `protocol`
/// selects the address notation - `Address::parse` (Modbus reference
/// numbers, e.g. `40001`) for [`Protocol::ModbusTcp`], `Address::parse_slmp`
/// (MELSEC device codes, e.g. `D100`) for [`Protocol::Slmp`] - so a tag's
/// stored text is validated under the rules its own connection's protocol
/// actually speaks, never guessed at (mirrors `banto_plc::address`'s own
/// module doc: "never inferred from the address text"). A malformed address
/// or unknown data type is a `CollectError::Config` - caught here, not
/// folded into a runtime `Bad`, because it is a configuration mistake the
/// operator must fix, not a transient PLC condition. (An address that parses
/// but whose area/type combination the wire cannot serve - e.g. a bit at a
/// register address - is *not* rejected here: `banto-plc`'s planner turns
/// that into a per-tick `ReadResult::Bad`, which the loop already records as
/// Bad quality.)
///
/// T8-2 (docs/tag-server-design.md §6.1, 2026-08-06): a T8 bit-in-word
/// address (`"D100.5"` / `"40001.3"`, parsed into `Address::{Slmp,
/// ModbusRef}`'s `bit: Some(_)`) only ever decodes/encodes to a single
/// `bool` (`decode_register_bit`/`banto-plc-write`'s RMW both work in terms
/// of one bit), so it is meaningless on any tag whose `data_type` is not
/// `"bit"` - a `data_type = "i16"` tag at `"D100.5"` would otherwise silently
/// collect nothing but a truncated 1-bit reading of the word. This is
/// rejected here as a `CollectError::Config`, not left for `banto-plc`'s
/// planner to fail per-tick, so the mistake surfaces at config-build time
/// exactly like every other address/data-type mismatch this function already
/// catches (a malformed address, an unknown data type). `banto-tags` (I1)
/// deliberately does not validate this at registration time (address format
/// is I2/I3b's concern, per this function's own doc comment above) - this is
/// the one place it is caught, and `CollectorManager::rebuild`
/// (`apps/banto-hub/core/src/hub.rs`) already treats any `CollectError::Config`
/// from `build_config` as an all-or-nothing failure that keeps the previous
/// catalog/`Collector` running (`last_config_error`), so a tag registered
/// this way simply never reaches the live catalog until fixed.
///
/// **The single interpreter** behind the config build
/// ([`build_config_lenient_from`], and so [`build_config_from`]) and the
/// save-time check [`check_tag_address`] (#414 段階1). Keeping one function
/// means "what a registry editor accepts on save" and "what the config build
/// accepts" cannot drift apart. Only the *wording* differs per caller: the
/// strict build names the tag ([`ConfigExclusion::strict_error`] - the
/// pre-段階2 text, so banto-hub's `last_config_error` is unchanged), a form
/// or an exclusion list does not ([`TagAddressIssue::message`]). #414 段階2:
/// a failure here no longer fails the lenient build - the tag alone is left
/// out ([`ExclusionReason::InvalidTag`]).
fn build_request(tag: &Tag, protocol: Protocol) -> Result<ReadRequest, TagAddressIssue> {
    parse_read_request(protocol, &tag.address, &tag.data_type)
}

/// The interpreter itself (see [`build_request`] for the rules), on raw
/// strings so [`check_tag_address`] can call it for a tag that is not saved
/// yet.
fn parse_read_request(
    protocol: Protocol,
    address: &str,
    data_type: &str,
) -> Result<ReadRequest, TagAddressIssue> {
    let parsed = match protocol {
        Protocol::ModbusTcp => Address::parse(address),
        Protocol::Slmp => Address::parse_slmp(address),
    }
    .map_err(|err| TagAddressIssue::InvalidAddress {
        protocol,
        reason: err.to_string(),
    })?;
    let data_type = DataType::parse(data_type).ok_or(TagAddressIssue::UnknownDataType)?;

    let is_bit_qualified = match parsed {
        Address::ModbusRef { bit, .. } => bit.is_some(),
        Address::Slmp { bit, .. } => bit.is_some(),
    };
    if is_bit_qualified && data_type != DataType::Bit {
        return Err(TagAddressIssue::BitAddressOnNonBitType);
    }

    Ok(ReadRequest {
        address: parsed,
        data_type,
    })
}

/// Which input field a [`TagAddressIssue`] is about - so a registry editor
/// (chronogazer's REST/Tauri tag handlers) can attach it to the right form
/// field without this crate knowing any wire/field naming.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagAddressField {
    Address,
    DataType,
}

/// Why [`check_tag_address`] rejected a tag (#414 段階1). Each variant is
/// exactly one of the `CollectError::Config` cases the strict
/// [`build_config_from`] raises for a tag (via [`build_request`]), so
/// "rejected on save" and "would fail [`build_config_from`]" are the same
/// predicate. #414 段階2: also carried by [`ExclusionReason::InvalidTag`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TagAddressIssue {
    /// The address text does not parse under the connection protocol's
    /// notation (e.g. MELSEC `D3000` on a Modbus TCP connection). `reason`
    /// is `banto_plc`'s own error text.
    InvalidAddress { protocol: Protocol, reason: String },
    /// The data type is outside `banto_plc::DataType`'s vocabulary.
    UnknownDataType,
    /// A bit-in-word address (`40001.3` / `D100.5`) on a tag whose data type
    /// is not `bit` (T8-2).
    BitAddressOnNonBitType,
}

impl TagAddressIssue {
    /// The input field this issue belongs to.
    pub fn field(&self) -> TagAddressField {
        match self {
            TagAddressIssue::InvalidAddress { .. } | TagAddressIssue::BitAddressOnNonBitType => {
                TagAddressField::Address
            }
            TagAddressIssue::UnknownDataType => TagAddressField::DataType,
        }
    }

    /// A human-readable reason for a form field (and, #414 段階2, for an
    /// exclusion list - [`ExclusionReason::message`]). Unlike the strict
    /// build's messages ([`ConfigExclusion::strict_error`]) it does not name
    /// the tag (the operator is looking at that tag's own form, or at a list
    /// row that already shows its name), but it does name the notation the
    /// connection expects, with an example.
    pub fn message(&self) -> String {
        match self {
            TagAddressIssue::InvalidAddress { protocol, reason } => {
                let (label, example) = match protocol {
                    Protocol::ModbusTcp => ("Modbus TCP", "40001"),
                    Protocol::Slmp => ("SLMP（MELSEC）", "D100"),
                };
                format!("{label} のアドレスとして解釈できません（例: {example}）。{reason}")
            }
            TagAddressIssue::UnknownDataType => "未対応のデータ型です".to_string(),
            TagAddressIssue::BitAddressOnNonBitType => {
                "ビット指定アドレス（例: 40001.3 / D100.5）はデータ型 bit のタグでのみ使えます"
                    .to_string()
            }
        }
    }
}

/// #414 段階1: would a tag with this `address`/`data_type`, placed under a
/// connection whose `plc_connections.protocol` is `protocol`, be accepted by
/// [`build_config_from`]? `Ok(())` exactly when the tag cannot be the reason
/// `build_config_from` fails - the same interpreter ([`parse_read_request`])
/// and the same skips:
///
/// - `data_type == "string"` is skipped by `build_config_from` before any
///   address parsing, so it is always `Ok` here.
/// - A `protocol` other than `"modbus-tcp"`/`"slmp"` is `Ok` here:
///   `"virtual"`/`"postgres"` connections are excluded from collection, and
///   an unsupported protocol string is a *connection*-level error
///   (`parse_protocol`) that no tag address could fix.
///
/// Whether the tag/group/connection is `enabled` is deliberately not an
/// input - a disabled tag with an unreadable address still breaks
/// collection the moment it is enabled, so a registry editor checks it on
/// save regardless.
pub fn check_tag_address(
    protocol: &str,
    address: &str,
    data_type: &str,
) -> Result<(), TagAddressIssue> {
    if data_type == banto_tags::STRING_DATA_TYPE {
        return Ok(());
    }
    let protocol = match protocol {
        "modbus-tcp" => Protocol::ModbusTcp,
        "slmp" => Protocol::Slmp,
        _ => return Ok(()),
    };
    parse_read_request(protocol, address, data_type).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use banto_tags::{CollectionGroupInput, PlcConnectionInput, TagInput};

    async fn registry() -> SqlitePool {
        let pool = banto_storage::connect_sqlite_memory().await.unwrap();
        banto_tags::migrate(&pool).await.unwrap();
        pool
    }

    fn conn_input(name: &str, port: i64) -> PlcConnectionInput {
        PlcConnectionInput {
            name: name.to_string(),
            protocol: "modbus-tcp".to_string(),
            host: "127.0.0.1".to_string(),
            port,
            unit_id: 1,
            enabled: true,
            simulation: false,

            word_order: "low_high".to_string(),
            database: None,
            username: None,
            password: None,
        }
    }

    fn group_input(name: &str, conn_id: i64, period_ms: i64) -> CollectionGroupInput {
        CollectionGroupInput {
            name: name.to_string(),
            plc_connection_id: conn_id,
            period_ms,
            enabled: true,
            default_writable: true,
            query_sql: None,
        }
    }

    fn tag_input(name: &str, group_id: i64, address: &str) -> TagInput {
        TagInput {
            name: name.to_string(),
            collection_group_id: group_id,
            address: address.to_string(),
            data_type: "i16".to_string(),
            string_length: None,
            string_encoding: "utf8".to_string(),
            raw_lo: None,
            raw_hi: None,
            eng_lo: None,
            eng_hi: None,
            unit: None,
            decimals: 0,
            threshold_h: None,
            threshold_hh: None,
            threshold_l: None,
            threshold_ll: None,
            enabled: true,
            writable: false,
            tag_kind: "plc".to_string(),
            expression: None,
            retain: false,
            expected_revision: None,
        }
    }

    #[tokio::test]
    async fn empty_registry_builds_an_empty_config() {
        let pool = registry().await;
        let config = build_config(&pool).await.unwrap();
        assert_eq!(config.group_count(), 0);
        assert_eq!(config.tag_count(), 0);
        assert!(config.store_config.groups.is_empty());
    }

    #[tokio::test]
    async fn snapshot_builder_matches_the_pool_compatibility_wrapper() {
        let pool = registry().await;
        let conn = PlcConnectionService::new(pool.clone())
            .create(conn_input("PLC1", 502))
            .await
            .unwrap();
        let group = CollectionGroupService::new(pool.clone())
            .create(group_input("G1", conn.id, 1_000))
            .await
            .unwrap();
        TagService::new(pool.clone())
            .create(tag_input("T1", group.id, "40001"))
            .await
            .unwrap();

        let snapshot = RegistrySnapshot::load(&pool).await.unwrap();
        assert_eq!(
            build_config(&pool).await.unwrap(),
            build_config_from(&snapshot).unwrap()
        );
    }

    #[tokio::test]
    async fn builds_connection_group_tag_hierarchy() {
        let pool = registry().await;
        let conn = PlcConnectionService::new(pool.clone())
            .create(conn_input("PLC1", 502))
            .await
            .unwrap();
        let group = CollectionGroupService::new(pool.clone())
            .create(group_input("G1", conn.id, 1_000))
            .await
            .unwrap();
        let tag_svc = TagService::new(pool.clone());
        tag_svc
            .create(tag_input("T1", group.id, "40001"))
            .await
            .unwrap();
        tag_svc
            .create(tag_input("T2", group.id, "40002"))
            .await
            .unwrap();

        let config = build_config(&pool).await.unwrap();
        assert_eq!(config.connections.len(), 1);
        assert_eq!(config.group_count(), 1);
        assert_eq!(config.tag_count(), 2);

        let c = &config.connections[0];
        assert_eq!(c.key, format!("conn:{}", conn.id));
        assert!(!c.simulation, "conn_input defaults to simulation: false");
        match &c.config {
            ProtocolConfig::ModbusTcp(modbus) => {
                assert_eq!(modbus.host, "127.0.0.1");
                assert_eq!(modbus.port, 502);
            }
            ProtocolConfig::Slmp(_) => panic!("expected ModbusTcp config"),
        }
        assert_eq!(c.groups[0].requests.len(), 2);

        // Derived store config mirrors the group/tag shape.
        assert_eq!(config.store_config.groups.len(), 1);
        assert_eq!(config.store_config.groups[0].tags.len(), 2);
        assert_eq!(
            config.store_config.groups[0].key,
            format!("grp:{}", group.id)
        );
    }

    /// T19 S2-a (UX-48): `connections_with_collected_groups` is a pure
    /// function over `groups` alone, so it can be tested without a DB at
    /// all - a connection with no group, one with only a disabled group,
    /// and one with an enabled (even tagless) group.
    #[test]
    fn connections_with_collected_groups_is_group_based_not_tag_based() {
        let groups = vec![
            CollectionGroup {
                id: 1,
                name: "G-enabled-empty".to_string(),
                plc_connection_id: 10,
                period_ms: 1_000,
                enabled: true,
                default_writable: true,
                query_sql: None,
            },
            CollectionGroup {
                id: 2,
                name: "G-disabled".to_string(),
                plc_connection_id: 20,
                period_ms: 1_000,
                enabled: false,
                default_writable: true,
                query_sql: None,
            },
        ];

        let collectible = connections_with_collected_groups(&groups);

        // conn 10 has an enabled group (even with zero tags of its own in
        // this fixture) - it counts, matching build_config_from's own
        // group-level (not tag-level) skip rule.
        assert!(collectible.contains(&10));
        // conn 20's only group is disabled - it does not count, same as a
        // connection with no group at all (conn 30, never mentioned here).
        assert!(!collectible.contains(&20));
        assert!(!collectible.contains(&30));
    }

    #[tokio::test]
    async fn connections_with_collected_groups_matches_build_config_from_skip_rule() {
        let pool = registry().await;
        let conn_svc = PlcConnectionService::new(pool.clone());

        // conn_tagless: enabled connection, zero groups - build_config_from
        // skips it entirely ("reading nothing from a PLC is pointless").
        let conn_tagless = conn_svc.create(conn_input("Tagless", 502)).await.unwrap();
        // conn_tagged: enabled connection with an enabled group and a tag -
        // build_config_from gives it a ConnectionPlan.
        let conn_tagged = conn_svc.create(conn_input("Tagged", 503)).await.unwrap();
        let group = CollectionGroupService::new(pool.clone())
            .create(group_input("G1", conn_tagged.id, 1_000))
            .await
            .unwrap();
        TagService::new(pool.clone())
            .create(tag_input("T1", group.id, "40001"))
            .await
            .unwrap();

        let snapshot = RegistrySnapshot::load(&pool).await.unwrap();
        let collectible = connections_with_collected_groups(&snapshot.groups);
        assert!(!collectible.contains(&conn_tagless.id));
        assert!(collectible.contains(&conn_tagged.id));

        let config = build_config_from(&snapshot).unwrap();
        let plan_ids: HashSet<i64> = config
            .connections
            .iter()
            .map(|c| c.key.strip_prefix("conn:").unwrap().parse::<i64>().unwrap())
            .collect();
        assert_eq!(
            plan_ids, collectible,
            "connections_with_collected_groups must exactly match which \
             connections build_config_from actually gives a ConnectionPlan"
        );
    }

    #[tokio::test]
    async fn disabled_rows_are_excluded_at_every_level() {
        let pool = registry().await;
        let conn_svc = PlcConnectionService::new(pool.clone());
        let group_svc = CollectionGroupService::new(pool.clone());
        let tag_svc = TagService::new(pool.clone());

        // Enabled connection with one enabled + one disabled group; the
        // enabled group has one enabled + one disabled tag.
        let conn = conn_svc.create(conn_input("PLC1", 502)).await.unwrap();
        let g_on = group_svc
            .create(group_input("Gon", conn.id, 1_000))
            .await
            .unwrap();
        let mut g_off = group_input("Goff", conn.id, 1_000);
        g_off.enabled = false;
        group_svc.create(g_off).await.unwrap();
        tag_svc
            .create(tag_input("Ton", g_on.id, "40001"))
            .await
            .unwrap();
        let mut t_off = tag_input("Toff", g_on.id, "40002");
        t_off.enabled = false;
        tag_svc.create(t_off).await.unwrap();

        // A fully-disabled connection contributes nothing.
        let mut c_off = conn_input("PLCoff", 503);
        c_off.enabled = false;
        let conn_off = conn_svc.create(c_off).await.unwrap();
        group_svc
            .create(group_input("Gorphan", conn_off.id, 1_000))
            .await
            .unwrap();

        let config = build_config(&pool).await.unwrap();
        assert_eq!(config.connections.len(), 1, "disabled connection excluded");
        assert_eq!(config.group_count(), 1, "disabled group excluded");
        assert_eq!(config.tag_count(), 1, "disabled tag excluded");
    }

    #[tokio::test]
    async fn scaling_and_bit_flags_are_resolved() {
        let pool = registry().await;
        let conn = PlcConnectionService::new(pool.clone())
            .create(conn_input("PLC1", 502))
            .await
            .unwrap();
        let group = CollectionGroupService::new(pool.clone())
            .create(group_input("G1", conn.id, 1_000))
            .await
            .unwrap();
        let tag_svc = TagService::new(pool.clone());

        let mut scaled = tag_input("Scaled", group.id, "40001");
        scaled.raw_lo = Some(0.0);
        scaled.raw_hi = Some(4095.0);
        scaled.eng_lo = Some(0.0);
        scaled.eng_hi = Some(100.0);
        tag_svc.create(scaled).await.unwrap();

        let mut bit = tag_input("Bit", group.id, "00001");
        bit.data_type = "bit".to_string();
        tag_svc.create(bit).await.unwrap();

        let config = build_config(&pool).await.unwrap();
        let group = &config.connections[0].groups[0];
        assert_eq!(group.tags.len(), 2);
        assert!(group.tags[0].scaling.is_some());
        assert_eq!(group.requests[0].data_type, DataType::I16);
        assert!(group.tags[1].scaling.is_none());
        assert_eq!(group.requests[1].data_type, DataType::Bit);
    }

    /// The S1 hard constraint (ChronoGazer safety): a `"string"` tag in the
    /// shared registry is *skipped* by this recorder pipeline - never read,
    /// never a tstore column - and the rest of its group still collects.
    /// The string tag deliberately carries a MELSEC-notation address
    /// (`D100`) that `Address::parse` (Modbus) would reject: if the skip ever
    /// moved after `build_request`, this test would fail with the config
    /// error instead of passing, proving the tag is skipped *before* any
    /// parsing, not merely tolerated.
    #[tokio::test]
    async fn a_string_tag_is_skipped_and_the_rest_of_the_group_still_collects() {
        let pool = registry().await;
        let conn = PlcConnectionService::new(pool.clone())
            .create(conn_input("PLC1", 502))
            .await
            .unwrap();
        let group = CollectionGroupService::new(pool.clone())
            .create(group_input("G1", conn.id, 1_000))
            .await
            .unwrap();
        let tag_svc = TagService::new(pool.clone());
        let numeric = tag_svc
            .create(tag_input("Numeric", group.id, "40001"))
            .await
            .unwrap();
        let mut string_tag = tag_input("Recipe", group.id, "D100");
        string_tag.data_type = "string".to_string();
        string_tag.string_length = Some(16);
        tag_svc.create(string_tag).await.unwrap();

        let config = build_config(&pool)
            .await
            .expect("a string tag must not fail the recorder's config build");

        // Only the numeric tag is collected...
        assert_eq!(config.group_count(), 1);
        assert_eq!(config.tag_count(), 1, "the string tag must be skipped");
        let g = &config.connections[0].groups[0];
        assert_eq!(g.requests.len(), 1);
        assert_eq!(g.tags[0].key, format!("tag:{}", numeric.id));

        // ...and the frozen numeric store schema never sees the string:
        // exactly one column, and no "string" data type anywhere.
        assert_eq!(config.store_config.groups.len(), 1);
        let columns = &config.store_config.groups[0].tags;
        assert_eq!(columns.len(), 1);
        assert_eq!(columns[0].name, "Numeric");
        assert!(columns.iter().all(|c| c.data_type != "string"));
    }

    #[tokio::test]
    async fn invalid_address_is_a_config_error() {
        let pool = registry().await;
        let conn = PlcConnectionService::new(pool.clone())
            .create(conn_input("PLC1", 502))
            .await
            .unwrap();
        let group = CollectionGroupService::new(pool.clone())
            .create(group_input("G1", conn.id, 1_000))
            .await
            .unwrap();
        // "99999" has an unknown area prefix (9); passes banto-tags'
        // non-empty check but fails Address::parse.
        TagService::new(pool.clone())
            .create(tag_input("Bad", group.id, "99999"))
            .await
            .unwrap();
        let err = build_config(&pool).await.unwrap_err();
        assert!(matches!(err, CollectError::Config(_)));
    }

    /// T8-2 (docs/tag-server-design.md §6.1, 2026-08-06): a bit-in-word
    /// address (`"40001.3"`) on a tag whose `data_type` is not `"bit"` is a
    /// config error, not silently accepted as a truncated numeric reading.
    #[tokio::test]
    async fn a_bit_qualified_modbus_address_on_a_non_bit_tag_is_a_config_error() {
        let pool = registry().await;
        let conn = PlcConnectionService::new(pool.clone())
            .create(conn_input("PLC1", 502))
            .await
            .unwrap();
        let group = CollectionGroupService::new(pool.clone())
            .create(group_input("G1", conn.id, 1_000))
            .await
            .unwrap();
        // tag_input defaults data_type to "i16" - "40001.3" is a bit-in-word
        // address, which is only meaningful for a "bit" tag.
        TagService::new(pool.clone())
            .create(tag_input("Bad", group.id, "40001.3"))
            .await
            .unwrap();
        let err = build_config(&pool).await.unwrap_err();
        assert!(matches!(err, CollectError::Config(_)));
    }

    /// The SLMP-notation counterpart of the above (`"D100.5"`).
    #[tokio::test]
    async fn a_bit_qualified_slmp_address_on_a_non_bit_tag_is_a_config_error() {
        let pool = registry().await;
        let conn = PlcConnectionService::new(pool.clone())
            .create(conn_input_slmp("PLC1", 5007))
            .await
            .unwrap();
        let group = CollectionGroupService::new(pool.clone())
            .create(group_input("G1", conn.id, 1_000))
            .await
            .unwrap();
        TagService::new(pool.clone())
            .create(tag_input("Bad", group.id, "D100.5"))
            .await
            .unwrap();
        let err = build_config(&pool).await.unwrap_err();
        assert!(matches!(err, CollectError::Config(_)));
    }

    /// A bit-in-word address paired with `data_type = "bit"` builds cleanly -
    /// the config error above is specific to the data-type mismatch, not to
    /// bit-in-word notation itself.
    #[tokio::test]
    async fn a_bit_qualified_address_with_a_bit_data_type_builds() {
        let pool = registry().await;
        let conn = PlcConnectionService::new(pool.clone())
            .create(conn_input_slmp("PLC1", 5007))
            .await
            .unwrap();
        let group = CollectionGroupService::new(pool.clone())
            .create(group_input("G1", conn.id, 1_000))
            .await
            .unwrap();
        let mut bit_tag = tag_input("Ok", group.id, "D100.5");
        bit_tag.data_type = "bit".to_string();
        TagService::new(pool.clone()).create(bit_tag).await.unwrap();
        let config = build_config(&pool)
            .await
            .expect("a bit-in-word address on a bit tag must build");
        assert_eq!(config.tag_count(), 1);
        assert_eq!(
            config.connections[0].groups[0].requests[0].data_type,
            DataType::Bit
        );
    }

    fn conn_input_slmp(name: &str, port: i64) -> PlcConnectionInput {
        PlcConnectionInput {
            protocol: "slmp".to_string(),
            ..conn_input(name, port)
        }
    }

    /// I8 (2026-08-05): `"slmp"` is no longer an unsupported-protocol config
    /// error, and its tag addresses are parsed under MELSEC device-code
    /// notation (`D100`), not Modbus reference numbers.
    #[tokio::test]
    async fn slmp_connection_builds_with_melsec_addresses() {
        let pool = registry().await;
        let conn = PlcConnectionService::new(pool.clone())
            .create(conn_input_slmp("PLC1", 5007))
            .await
            .unwrap();
        let group = CollectionGroupService::new(pool.clone())
            .create(group_input("G1", conn.id, 1_000))
            .await
            .unwrap();
        TagService::new(pool.clone())
            .create(tag_input("T1", group.id, "D100"))
            .await
            .unwrap();

        let config = build_config(&pool).await.expect("slmp config should build");
        assert_eq!(config.connections.len(), 1);
        let c = &config.connections[0];
        match &c.config {
            ProtocolConfig::Slmp(slmp) => {
                assert_eq!(slmp.host, "127.0.0.1");
                assert_eq!(slmp.port, 5007);
            }
            ProtocolConfig::ModbusTcp(_) => panic!("expected Slmp config"),
        }
        assert_eq!(c.groups[0].requests.len(), 1);
    }

    /// P3-b (監査指摘 2026-08-12): `slmp_config_for` must carry the
    /// connection's own `word_order` into the built `SlmpConfig`, not
    /// silently fix every connection to `SlmpConfig::default()`'s
    /// `WordOrder::LowHigh` - the regression test for the audit finding.
    #[tokio::test]
    async fn slmp_config_reflects_the_connections_own_word_order() {
        let pool = registry().await;
        let plc_svc = PlcConnectionService::new(pool.clone());

        let mut swapped_input = conn_input_slmp("Swapped", 5007);
        swapped_input.word_order = "high_low".to_string();
        let swapped = plc_svc.create(swapped_input).await.unwrap();
        CollectionGroupService::new(pool.clone())
            .create(group_input("G1", swapped.id, 1_000))
            .await
            .unwrap();
        TagService::new(pool.clone())
            .create(tag_input("T1", swapped.id, "D100"))
            .await
            .unwrap();

        let config = build_config(&pool).await.expect("slmp config should build");
        match &config.connections[0].config {
            ProtocolConfig::Slmp(slmp) => {
                assert_eq!(
                    slmp.word_order,
                    WordOrder::HighLow,
                    "a connection asking for high_low must not silently get the default"
                );
            }
            ProtocolConfig::ModbusTcp(_) => panic!("expected Slmp config"),
        }
    }

    /// The default-omitted counterpart: a `"slmp"` connection that never sets
    /// `word_order` (this file's `conn_input_slmp`, unchanged) still gets
    /// `WordOrder::LowHigh` - the same value every SLMP connection got before
    /// this column existed, so existing databases are unaffected.
    #[tokio::test]
    async fn slmp_config_defaults_to_low_high_word_order() {
        let pool = registry().await;
        let conn = PlcConnectionService::new(pool.clone())
            .create(conn_input_slmp("PLC1", 5007))
            .await
            .unwrap();
        CollectionGroupService::new(pool.clone())
            .create(group_input("G1", conn.id, 1_000))
            .await
            .unwrap();
        TagService::new(pool.clone())
            .create(tag_input("T1", conn.id, "D100"))
            .await
            .unwrap();

        let config = build_config(&pool).await.expect("slmp config should build");
        match &config.connections[0].config {
            ProtocolConfig::Slmp(slmp) => assert_eq!(slmp.word_order, WordOrder::LowHigh),
            ProtocolConfig::ModbusTcp(_) => panic!("expected Slmp config"),
        }
    }

    /// Regression test for the bug fixed 2026-09-08 (see [`modbus_config_for`]'s
    /// doc comment for the full history): `modbus_config_for` must carry the
    /// connection's own `word_order` column into the built `ModbusTcpConfig`,
    /// not silently fix every connection to
    /// `ModbusTcpConfig::default()`'s `WordOrder::HighLow` regardless of what
    /// the registry says - which is exactly what it did before this fix,
    /// even though the column had existed since P3-b and
    /// `banto_broker::modbus_config_for` already read it correctly for its
    /// read-on-demand/write path. Mirrors
    /// `slmp_config_reflects_the_connections_own_word_order` above, checking
    /// both directions (not just the non-default one) because Modbus's own
    /// default word order disagreed with SLMP's before the fix.
    #[tokio::test]
    async fn modbus_config_reflects_the_connections_own_word_order() {
        let pool = registry().await;
        let plc_svc = PlcConnectionService::new(pool.clone());

        let mut high_low_input = conn_input("HighLow", 502);
        high_low_input.word_order = "high_low".to_string();
        let high_low_conn = plc_svc.create(high_low_input).await.unwrap();
        CollectionGroupService::new(pool.clone())
            .create(group_input("G1", high_low_conn.id, 1_000))
            .await
            .unwrap();
        TagService::new(pool.clone())
            .create(tag_input("T1", high_low_conn.id, "40001"))
            .await
            .unwrap();

        let mut low_high_input = conn_input("LowHigh", 503);
        low_high_input.word_order = "low_high".to_string();
        let low_high_conn = plc_svc.create(low_high_input).await.unwrap();
        CollectionGroupService::new(pool.clone())
            .create(group_input("G2", low_high_conn.id, 1_000))
            .await
            .unwrap();
        TagService::new(pool.clone())
            .create(tag_input("T2", low_high_conn.id, "40001"))
            .await
            .unwrap();

        let config = build_config(&pool)
            .await
            .expect("modbus config should build");

        let high_low_plan = config
            .connections
            .iter()
            .find(|c| c.key == format!("conn:{}", high_low_conn.id))
            .unwrap();
        match &high_low_plan.config {
            ProtocolConfig::ModbusTcp(modbus) => assert_eq!(
                modbus.word_order,
                WordOrder::HighLow,
                "a connection asking for high_low must not silently get low_high"
            ),
            ProtocolConfig::Slmp(_) => panic!("expected ModbusTcp config"),
        }

        let low_high_plan = config
            .connections
            .iter()
            .find(|c| c.key == format!("conn:{}", low_high_conn.id))
            .unwrap();
        match &low_high_plan.config {
            ProtocolConfig::ModbusTcp(modbus) => assert_eq!(
                modbus.word_order,
                WordOrder::LowHigh,
                "a connection asking for low_high must not silently get \
                 ModbusTcpConfig::default()'s high_low"
            ),
            ProtocolConfig::Slmp(_) => panic!("expected ModbusTcp config"),
        }
    }

    /// `parse_word_order`'s fallback for an unrecognized string is
    /// `WordOrder::LowHigh`, and this must hold for Modbus exactly as it
    /// does for SLMP (see this file's `parse_word_order` doc comment and
    /// `banto_broker`'s identical fn) - not `ModbusTcpConfig::default()`'s
    /// `WordOrder::HighLow`. Calls `parse_word_order` directly rather than
    /// going through `build_config`/the registry because
    /// `banto_tags::plc_connection::ALLOWED_WORD_ORDERS` plus the SQL
    /// `CHECK` added by migration `0010` make an unrecognized `word_order`
    /// value unreachable through normal CRUD (see
    /// `PlcConnectionService::create`'s own `create_rejects_unknown_word_order`
    /// test) - this is pinning down defense-in-depth behavior, not a
    /// reachable-through-the-registry scenario.
    #[test]
    fn parse_word_order_falls_back_to_low_high_for_an_unrecognized_value() {
        assert_eq!(parse_word_order("sideways"), WordOrder::LowHigh);
        assert_eq!(parse_word_order(""), WordOrder::LowHigh);
    }

    /// T9-1 (docs/ux-plan.md §1): `PlcConnection::simulation` carries through
    /// unchanged into `ConnectionPlan::simulation` - `build_config` itself
    /// never starts a simulator or touches host/port (`ConnectionPlan`'s own
    /// doc comment: that substitution is `crate::collector::Collector`'s job,
    /// done at task-spawn time, not here) - this just pins down that the flag
    /// is not dropped along the way.
    #[tokio::test]
    async fn simulation_flag_is_carried_into_the_connection_plan() {
        let pool = registry().await;
        let plc_svc = PlcConnectionService::new(pool.clone());
        let mut sim_input = conn_input("Simulated", 502);
        sim_input.simulation = true;
        let sim_conn = plc_svc.create(sim_input).await.unwrap();
        let sim_group = CollectionGroupService::new(pool.clone())
            .create(group_input("G1", sim_conn.id, 1_000))
            .await
            .unwrap();
        TagService::new(pool.clone())
            .create(tag_input("T1", sim_group.id, "40001"))
            .await
            .unwrap();

        let real_conn = plc_svc.create(conn_input("Real", 503)).await.unwrap();
        let real_group = CollectionGroupService::new(pool.clone())
            .create(group_input("G2", real_conn.id, 1_000))
            .await
            .unwrap();
        TagService::new(pool.clone())
            .create(tag_input("T2", real_group.id, "40001"))
            .await
            .unwrap();

        let config = build_config(&pool).await.unwrap();
        assert_eq!(config.connections.len(), 2);
        let sim_plan = config
            .connections
            .iter()
            .find(|c| c.key == format!("conn:{}", sim_conn.id))
            .expect("simulated connection should be in the config");
        assert!(sim_plan.simulation);
        // build_config never rewrites host/port for a simulated connection -
        // that is Collector's job (see ConnectionPlan's doc comment).
        match &sim_plan.config {
            ProtocolConfig::ModbusTcp(modbus) => assert_eq!(modbus.port, 502),
            ProtocolConfig::Slmp(_) => panic!("expected ModbusTcp config"),
        }
        let real_plan = config
            .connections
            .iter()
            .find(|c| c.key == format!("conn:{}", real_conn.id))
            .expect("real connection should be in the config");
        assert!(!real_plan.simulation);
    }

    /// A Modbus-notation address on an `"slmp"` connection must be rejected
    /// at config-build time (`Address::parse_slmp` does not understand
    /// `"40001"`), mirroring `invalid_address_is_a_config_error` for the
    /// Modbus side.
    #[tokio::test]
    async fn a_modbus_address_on_an_slmp_connection_is_a_config_error() {
        let pool = registry().await;
        let conn = PlcConnectionService::new(pool.clone())
            .create(conn_input_slmp("PLC1", 5007))
            .await
            .unwrap();
        let group = CollectionGroupService::new(pool.clone())
            .create(group_input("G1", conn.id, 1_000))
            .await
            .unwrap();
        TagService::new(pool.clone())
            .create(tag_input("Bad", group.id, "40001"))
            .await
            .unwrap();
        let err = build_config(&pool).await.unwrap_err();
        assert!(matches!(err, CollectError::Config(_)));
    }

    /// T6-2 (docs/tag-server-design.md §4.2/§4.3(a)): a `"virtual"` connection
    /// (banto-hub's reserved `calc`/`mem` namespace) contributes nothing to
    /// collection - excluded, not a config error - while an ordinary
    /// connection alongside it still collects normally.
    #[tokio::test]
    async fn a_virtual_connection_is_excluded_from_collection_without_erroring() {
        let pool = registry().await;
        let plc_svc = PlcConnectionService::new(pool.clone());
        let real_conn = plc_svc.create(conn_input("PLC1", 502)).await.unwrap();
        let group = CollectionGroupService::new(pool.clone())
            .create(group_input("G1", real_conn.id, 1_000))
            .await
            .unwrap();
        TagService::new(pool.clone())
            .create(tag_input("T1", group.id, "40001"))
            .await
            .unwrap();

        // A virtual connection (host/port left blank, like banto-hub's
        // auto-provisioned "calc") with its own group and tag - if this were
        // NOT excluded, build_config would fail (parse_protocol rejects
        // "virtual", and Address::parse would reject an empty address).
        let virtual_conn = plc_svc
            .create(PlcConnectionInput {
                name: "calc".to_string(),
                protocol: "virtual".to_string(),
                host: String::new(),
                port: 0,
                unit_id: 1,
                enabled: true,
                simulation: false,

                word_order: "low_high".to_string(),
                database: None,
                username: None,
                password: None,
            })
            .await
            .unwrap();
        let virtual_group = CollectionGroupService::new(pool.clone())
            .create(group_input("calc-group", virtual_conn.id, 1_000))
            .await
            .unwrap();
        let mut computed_tag = tag_input("avg", virtual_group.id, "");
        computed_tag.tag_kind = "computed".to_string();
        computed_tag.expression = Some("1 + 1".to_string());
        TagService::new(pool.clone())
            .create(computed_tag)
            .await
            .unwrap();

        let config = build_config(&pool)
            .await
            .expect("a virtual connection must not fail config build");
        assert_eq!(
            config.connections.len(),
            1,
            "only the real connection should be collected"
        );
        assert_eq!(config.tag_count(), 1);
        assert_eq!(config.connections[0].key, format!("conn:{}", real_conn.id));
    }

    /// S1 (docs/banto-hub-external-db-design.md §4.1・§7): the `"postgres"`
    /// twin of `a_virtual_connection_is_excluded_from_collection_without_erroring`
    /// - if `"postgres"` were not excluded here, `build_config` would fail
    /// the moment the connection is merely *enabled* (`parse_protocol` has no
    /// variant for it), even with zero groups under it (S1 forbids creating
    /// any collection group under a `"postgres"` connection in the first
    /// place, so this connection stays groupless for real).
    #[tokio::test]
    async fn a_postgres_connection_is_excluded_from_collection_without_erroring() {
        let pool = registry().await;
        let plc_svc = PlcConnectionService::new(pool.clone());
        let real_conn = plc_svc.create(conn_input("PLC1", 502)).await.unwrap();
        let group = CollectionGroupService::new(pool.clone())
            .create(group_input("G1", real_conn.id, 1_000))
            .await
            .unwrap();
        TagService::new(pool.clone())
            .create(tag_input("T1", group.id, "40001"))
            .await
            .unwrap();

        plc_svc
            .create(PlcConnectionInput {
                name: "ERP DB".to_string(),
                protocol: "postgres".to_string(),
                host: "10.0.0.50".to_string(),
                port: 5432,
                unit_id: 1,
                enabled: true,
                simulation: false,
                word_order: "low_high".to_string(),
                database: Some("erp".to_string()),
                username: Some("reader".to_string()),
                password: None,
            })
            .await
            .unwrap();

        let config = build_config(&pool)
            .await
            .expect("an enabled postgres connection must not fail config build");
        assert_eq!(
            config.connections.len(),
            1,
            "only the PLC connection should be collected - the postgres one is not a PLC pipeline participant"
        );
        assert_eq!(config.connections[0].key, format!("conn:{}", real_conn.id));
    }

    #[tokio::test]
    async fn config_is_deterministic_across_rebuilds() {
        let pool = registry().await;
        let conn = PlcConnectionService::new(pool.clone())
            .create(conn_input("PLC1", 502))
            .await
            .unwrap();
        let group = CollectionGroupService::new(pool.clone())
            .create(group_input("G1", conn.id, 1_000))
            .await
            .unwrap();
        let tag_svc = TagService::new(pool.clone());
        tag_svc
            .create(tag_input("T1", group.id, "40001"))
            .await
            .unwrap();
        tag_svc
            .create(tag_input("T2", group.id, "40002"))
            .await
            .unwrap();

        let a = build_config(&pool).await.unwrap();
        let b = build_config(&pool).await.unwrap();
        assert_eq!(a.store_config, b.store_config);
    }

    /// T9-2: `suppress_simulation_for` forces `simulation = false` for every
    /// connection whose key is in the given set, and leaves every other
    /// connection's `simulation` flag untouched - the mechanism
    /// `CollectorManager::rebuild` (apps/banto-hub/core) uses to prevent
    /// `Collector` from starting a redundant simulator for a broker-routed
    /// connection that `BrokerSimRegistry` already simulates itself.
    #[tokio::test]
    async fn suppress_simulation_for_forces_flag_off_only_for_matching_keys() {
        let pool = registry().await;
        let plc_svc = PlcConnectionService::new(pool.clone());

        let mut sim_input = conn_input("Suppressed", 502);
        sim_input.simulation = true;
        let suppressed_conn = plc_svc.create(sim_input).await.unwrap();
        let suppressed_group = CollectionGroupService::new(pool.clone())
            .create(group_input("G1", suppressed_conn.id, 1_000))
            .await
            .unwrap();
        TagService::new(pool.clone())
            .create(tag_input("T1", suppressed_group.id, "40001"))
            .await
            .unwrap();

        let mut other_input = conn_input("StillSimulated", 503);
        other_input.simulation = true;
        let other_conn = plc_svc.create(other_input).await.unwrap();
        let other_group = CollectionGroupService::new(pool.clone())
            .create(group_input("G2", other_conn.id, 1_000))
            .await
            .unwrap();
        TagService::new(pool.clone())
            .create(tag_input("T2", other_group.id, "40001"))
            .await
            .unwrap();

        let mut config = build_config(&pool).await.unwrap();
        let mut keys = std::collections::HashSet::new();
        keys.insert(format!("conn:{}", suppressed_conn.id));
        // A key with no matching connection must be silently ignored.
        keys.insert("conn:999999".to_string());
        config.suppress_simulation_for(&keys);

        let suppressed_plan = config
            .connections
            .iter()
            .find(|c| c.key == format!("conn:{}", suppressed_conn.id))
            .unwrap();
        assert!(
            !suppressed_plan.simulation,
            "connection in the set must have simulation forced off"
        );

        let other_plan = config
            .connections
            .iter()
            .find(|c| c.key == format!("conn:{}", other_conn.id))
            .unwrap();
        assert!(
            other_plan.simulation,
            "connection not in the set must be untouched"
        );
    }

    /// T9-2: `set_broker_dial_target` overwrites a broker-routed plan's
    /// `host`/`port` (used purely so `Collector::apply_config`'s `PartialEq`
    /// diff notices a resolved-target change and respawns the connection's
    /// task with a fresh `ClientFactory` - see the method's own doc comment
    /// for the full derivation), leaves a non-matching key untouched, and is
    /// a silent no-op for an absent key.
    ///
    /// #337 (2026-09-08): the Modbus plan is now stamped too - it used to be
    /// asserted here as an explicit no-op, which stopped being correct once
    /// Modbus collection reads became broker-routed (and therefore
    /// `suppress_simulation_for`-suppressed, flattening away the natural
    /// `simulation`-toggle diff signal the Modbus plan used to carry).
    #[tokio::test]
    async fn set_broker_dial_target_overwrites_the_matching_broker_routed_plan() {
        let pool = registry().await;
        let plc_svc = PlcConnectionService::new(pool.clone());

        let slmp_conn = plc_svc.create(conn_input_slmp("PLC1", 5007)).await.unwrap();
        let slmp_group = CollectionGroupService::new(pool.clone())
            .create(group_input("G1", slmp_conn.id, 1_000))
            .await
            .unwrap();
        TagService::new(pool.clone())
            .create(tag_input("T1", slmp_group.id, "D100"))
            .await
            .unwrap();

        let modbus_conn = plc_svc.create(conn_input("PLC2", 502)).await.unwrap();
        let modbus_group = CollectionGroupService::new(pool.clone())
            .create(group_input("G2", modbus_conn.id, 1_000))
            .await
            .unwrap();
        TagService::new(pool.clone())
            .create(tag_input("T2", modbus_group.id, "40001"))
            .await
            .unwrap();

        let mut config = build_config(&pool).await.unwrap();
        let slmp_key = format!("conn:{}", slmp_conn.id);
        let modbus_key = format!("conn:{}", modbus_conn.id);

        config.set_broker_dial_target(&slmp_key, "127.0.0.1".to_string(), 19999);
        config.set_broker_dial_target(&modbus_key, "127.0.0.1".to_string(), 19998);
        // An absent key must still be a silent no-op.
        config.set_broker_dial_target("conn:999999", "127.0.0.1".to_string(), 1);

        let slmp_plan = config
            .connections
            .iter()
            .find(|c| c.key == slmp_key)
            .unwrap();
        match &slmp_plan.config {
            ProtocolConfig::Slmp(cfg) => assert_eq!(cfg.port, 19999),
            ProtocolConfig::ModbusTcp(_) => panic!("expected Slmp config"),
        }

        let modbus_plan = config
            .connections
            .iter()
            .find(|c| c.key == modbus_key)
            .unwrap();
        match &modbus_plan.config {
            ProtocolConfig::ModbusTcp(cfg) => assert_eq!(
                cfg.port, 19998,
                "#337: a broker-routed Modbus plan is stamped just like an SLMP one"
            ),
            ProtocolConfig::Slmp(_) => panic!("expected ModbusTcp config"),
        }
    }

    /// #414 段階1: the save-time check's accept/reject table.
    /// `None` = accepted; `Some(field)` = rejected on that field.
    const TAG_ADDRESS_TABLE: &[(&str, &str, &str, Option<TagAddressField>)] = &[
        // Modbus TCP: reference numbers only.
        ("modbus-tcp", "40001", "i16", None),
        ("modbus-tcp", "00001", "bit", None),
        ("modbus-tcp", "30010", "f32", None),
        ("modbus-tcp", "40001.3", "bit", None),
        ("modbus-tcp", "D3000", "i16", Some(TagAddressField::Address)),
        (
            "modbus-tcp",
            "D100.5",
            "bit",
            Some(TagAddressField::Address),
        ),
        ("modbus-tcp", "99999", "i16", Some(TagAddressField::Address)),
        (
            "modbus-tcp",
            "40001.3",
            "i16",
            Some(TagAddressField::Address),
        ),
        (
            "modbus-tcp",
            "40001",
            "i128",
            Some(TagAddressField::DataType),
        ),
        // SLMP: MELSEC device codes only.
        ("slmp", "D100", "i16", None),
        ("slmp", "M100", "bit", None),
        ("slmp", "D100.5", "bit", None),
        ("slmp", "40001", "i16", Some(TagAddressField::Address)),
        ("slmp", "D100.5", "i16", Some(TagAddressField::Address)),
        // `build_config_from` skips string tags before parsing anything.
        ("modbus-tcp", "D3000", "string", None),
        // Not collected / connection-level error: never the tag's fault.
        ("virtual", "anything", "f64", None),
        ("postgres", "anything", "f64", None),
    ];

    #[test]
    fn check_tag_address_accepts_and_rejects_per_protocol() {
        for &(protocol, address, data_type, expected) in TAG_ADDRESS_TABLE {
            let got = check_tag_address(protocol, address, data_type)
                .err()
                .map(|issue| issue.field());
            assert_eq!(got, expected, "{protocol} / {address} / {data_type}");
        }
    }

    #[test]
    fn an_address_issue_names_the_expected_notation() {
        let issue = check_tag_address("modbus-tcp", "D3000", "i16").unwrap_err();
        let message = issue.message();
        assert!(message.contains("Modbus TCP"), "{message}");
        assert!(message.contains("40001"), "{message}");
        let issue = check_tag_address("slmp", "40001", "i16").unwrap_err();
        assert!(issue.message().contains("D100"), "{}", issue.message());
    }

    /// The whole point of `check_tag_address`: it rejects exactly what
    /// `build_config` rejects. Every table row whose protocol is a wire
    /// protocol and whose data type the registry accepts is written to a
    /// real registry and built - the two verdicts must agree.
    #[tokio::test]
    async fn check_tag_address_agrees_with_build_config() {
        for &(protocol, address, data_type, _) in TAG_ADDRESS_TABLE {
            if !["modbus-tcp", "slmp"].contains(&protocol)
                || !banto_tags::ALLOWED_DATA_TYPES.contains(&data_type)
            {
                continue;
            }
            let pool = registry().await;
            let mut conn = conn_input("PLC1", 502);
            conn.protocol = protocol.to_string();
            let conn = PlcConnectionService::new(pool.clone())
                .create(conn)
                .await
                .unwrap();
            let group = CollectionGroupService::new(pool.clone())
                .create(group_input("G1", conn.id, 1_000))
                .await
                .unwrap();
            let mut tag = tag_input("T", group.id, address);
            tag.data_type = data_type.to_string();
            if data_type == banto_tags::STRING_DATA_TYPE {
                tag.string_length = Some(4);
            }
            TagService::new(pool.clone()).create(tag).await.unwrap();

            let built = build_config(&pool).await;
            let checked = check_tag_address(protocol, address, data_type);
            assert_eq!(
                built.is_ok(),
                checked.is_ok(),
                "{protocol} / {address} / {data_type}: build={built:?} check={checked:?}"
            );
        }
    }

    // --- #414 段階2: the strict builder's wording is frozen ----------------
    //
    // banto-hub's `last_config_error` shows `build_config`'s text verbatim,
    // so the strict path must keep failing on the *same* item with the
    // *same* words after `build_config_from` was rebuilt on top of the
    // lenient builder. These goldens were run against the pre-#414-段階2
    // implementation first (they passed there unchanged).

    /// One valid Modbus connection "PLC1" (port 502) with group "G1"
    /// (1000 ms) and tag "T1" (40001, i16), loaded as a snapshot - each
    /// golden case then breaks exactly one field in memory (the registry
    /// services would reject most of these values on write, which is the
    /// point: they model rows that got in some other way).
    async fn golden_snapshot() -> RegistrySnapshot {
        let pool = registry().await;
        let conn = PlcConnectionService::new(pool.clone())
            .create(conn_input("PLC1", 502))
            .await
            .unwrap();
        let group = CollectionGroupService::new(pool.clone())
            .create(group_input("G1", conn.id, 1_000))
            .await
            .unwrap();
        TagService::new(pool.clone())
            .create(tag_input("T1", group.id, "40001"))
            .await
            .unwrap();
        RegistrySnapshot::load(&pool).await.unwrap()
    }

    fn strict_message(snapshot: &RegistrySnapshot) -> String {
        match build_config_from(snapshot) {
            Err(CollectError::Config(message)) => message,
            other => panic!("expected CollectError::Config, got {other:?}"),
        }
    }

    type Break = fn(&mut RegistrySnapshot);

    /// `(label, how to break the golden snapshot, the strict message)`.
    /// Shared with the lenient-builder tests, which check that the *first*
    /// exclusion is exactly the item named here.
    const GOLDEN_CASES: &[(&str, Break, &str)] = &[
        (
            "unsupported protocol",
            |s| s.connections[0].protocol = "ethernet-ip".to_string(),
            "接続 PLC1 のプロトコル ethernet-ip は未対応です（modbus-tcp / slmp のみ対応）",
        ),
        (
            "modbus port",
            |s| s.connections[0].port = 70_000,
            "接続 PLC1 のポート番号が不正です: 70000",
        ),
        (
            "modbus unit id",
            |s| s.connections[0].unit_id = 300,
            "接続 PLC1 のユニットIDが不正です: 300",
        ),
        (
            "slmp port",
            |s| {
                s.connections[0].protocol = "slmp".to_string();
                s.tags[0].address = "D100".to_string();
                s.connections[0].port = -1;
            },
            "接続 PLC1 のポート番号が不正です: -1",
        ),
        (
            "group period",
            |s| s.groups[0].period_ms = -5,
            "グループ G1 の period_ms が不正です: -5",
        ),
        (
            "tag address",
            |s| s.tags[0].address = "99999".to_string(),
            "タグ T1 のアドレス 99999 が不正です: GOLDEN_ADDRESS_REASON",
        ),
        (
            "tag data type",
            |s| s.tags[0].data_type = "f128".to_string(),
            "タグ T1 のデータ型 f128 は未対応です",
        ),
        (
            "bit address on a non-bit tag",
            |s| s.tags[0].address = "40001.3".to_string(),
            "タグ T1 のアドレス 40001.3 はビット指定アドレスです。ビット指定アドレスは \
             data_type=bit のタグでのみ使えます（現在のデータ型: i16）",
        ),
        (
            // Within one group the tags are parsed before the period is
            // checked, so a bad tag wins over a bad period.
            "bad tag and bad period in one group",
            |s| {
                s.tags[0].data_type = "f128".to_string();
                s.groups[0].period_ms = -5;
            },
            "タグ T1 のデータ型 f128 は未対応です",
        ),
        (
            // The connection is checked before any of its groups.
            "bad connection and bad tag under it",
            |s| {
                s.connections[0].port = 70_000;
                s.tags[0].data_type = "f128".to_string();
            },
            "接続 PLC1 のポート番号が不正です: 70000",
        ),
        (
            // A broken connection fails the strict build even when it has
            // nothing to collect.
            "bad connection without any group",
            |s| {
                s.connections[0].port = 70_000;
                s.groups.clear();
                s.tags.clear();
            },
            "接続 PLC1 のポート番号が不正です: 70000",
        ),
    ];

    fn golden_expected(expected: &str) -> String {
        let address_reason = Address::parse("99999").unwrap_err().to_string();
        expected.replace("GOLDEN_ADDRESS_REASON", &address_reason)
    }

    #[tokio::test]
    async fn strict_messages_are_unchanged_for_every_failure_unit() {
        let base = golden_snapshot().await;
        for (label, break_it, expected) in GOLDEN_CASES {
            let mut snapshot = base.clone();
            break_it(&mut snapshot);
            assert_eq!(
                strict_message(&snapshot),
                golden_expected(expected),
                "{label}"
            );
        }
    }

    /// Disabled rows never fail the strict build, whatever they contain.
    #[tokio::test]
    async fn strict_build_ignores_broken_disabled_rows() {
        let base = golden_snapshot().await;

        let mut snapshot = base.clone();
        snapshot.connections[0].port = 70_000;
        snapshot.connections[0].enabled = false;
        assert_eq!(build_config_from(&snapshot).unwrap().tag_count(), 0);

        let mut snapshot = base.clone();
        snapshot.groups[0].period_ms = -5;
        snapshot.groups[0].enabled = false;
        assert_eq!(build_config_from(&snapshot).unwrap().tag_count(), 0);

        let mut snapshot = base;
        snapshot.tags[0].address = "99999".to_string();
        snapshot.tags[0].enabled = false;
        assert_eq!(build_config_from(&snapshot).unwrap().tag_count(), 0);
        assert!(config_exclusions(&snapshot).is_empty());
    }

    // --- #414 段階2: the lenient builder ---------------------------------

    /// The strict build is "the lenient build, failing on the first
    /// exclusion": for every golden case the lenient build's first exclusion
    /// rebuilds exactly the golden text, and a clean snapshot has none.
    #[tokio::test]
    async fn the_first_exclusion_is_the_item_the_strict_build_fails_on() {
        let base = golden_snapshot().await;
        let (config, exclusions) = build_config_lenient_from(&base);
        assert!(exclusions.is_empty());
        assert_eq!(config, build_config_from(&base).unwrap());

        for (label, break_it, expected) in GOLDEN_CASES {
            let mut snapshot = base.clone();
            break_it(&mut snapshot);
            let (_, exclusions) = build_config_lenient_from(&snapshot);
            let first = exclusions.first().expect(label);
            match first.strict_error() {
                Some(CollectError::Config(message)) => {
                    assert_eq!(message, golden_expected(expected), "{label}")
                }
                other => panic!("{label}: {other:?}"),
            }
        }
    }

    /// Everything that can go wrong at once, across two connections. Only
    /// the good tag of the good group of the good connection is collected;
    /// every other enabled item is listed, parent before its cascades, in
    /// the order the strict build walks the registry.
    #[tokio::test]
    async fn broken_items_are_left_out_and_the_rest_is_built() {
        let pool = registry().await;
        let conns = PlcConnectionService::new(pool.clone());
        let groups = CollectionGroupService::new(pool.clone());
        let tags = TagService::new(pool.clone());
        let conn_a = conns.create(conn_input("PLC-A", 502)).await.unwrap();
        let conn_b = conns.create(conn_input("PLC-B", 503)).await.unwrap();
        let g1 = groups
            .create(group_input("G1", conn_a.id, 1_000))
            .await
            .unwrap();
        let g2 = groups
            .create(group_input("G2", conn_b.id, 1_000))
            .await
            .unwrap();
        let g3 = groups
            .create(group_input("G3", conn_a.id, 1_000))
            .await
            .unwrap();
        let t1 = tags.create(tag_input("T1", g1.id, "40001")).await.unwrap();
        let t2 = tags.create(tag_input("T2", g1.id, "D3000")).await.unwrap();
        let mut string_tag = tag_input("T-str", g1.id, "D3000");
        string_tag.data_type = banto_tags::STRING_DATA_TYPE.to_string();
        string_tag.string_length = Some(4);
        tags.create(string_tag).await.unwrap();
        let t4 = tags.create(tag_input("T4", g2.id, "40001")).await.unwrap();
        let t5 = tags.create(tag_input("T5", g3.id, "40001")).await.unwrap();
        let t6 = tags.create(tag_input("T6", g3.id, "40002")).await.unwrap();

        let mut snapshot = RegistrySnapshot::load(&pool).await.unwrap();
        let i = snapshot
            .connections
            .iter()
            .position(|c| c.id == conn_b.id)
            .unwrap();
        snapshot.connections[i].port = 70_000;
        let i = snapshot.groups.iter().position(|g| g.id == g3.id).unwrap();
        snapshot.groups[i].period_ms = -5;
        let i = snapshot.tags.iter().position(|t| t.id == t6.id).unwrap();
        snapshot.tags[i].data_type = "f128".to_string();

        let (config, exclusions) = build_config_lenient_from(&snapshot);

        // What is left: PLC-A / G1 / T1 only (T2 out, the string tag
        // skipped as always).
        assert_eq!(config.connections.len(), 1);
        assert_eq!(config.connections[0].key, format!("conn:{}", conn_a.id));
        assert_eq!(config.group_count(), 1);
        assert_eq!(config.tag_count(), 1);
        assert_eq!(
            config.connections[0].groups[0].tags[0].key,
            format!("tag:{}", t1.id)
        );
        let columns: Vec<&str> = config.store_config.groups[0]
            .tags
            .iter()
            .map(|c| c.key.as_str())
            .collect();
        assert_eq!(columns, vec![format!("tag:{}", t1.id).as_str()]);

        let listed: Vec<(ExclusionUnit, String, &str)> = exclusions
            .iter()
            .map(|e| (e.unit, e.key.clone(), e.reason.code()))
            .collect();
        assert_eq!(
            listed,
            vec![
                (
                    ExclusionUnit::Tag,
                    format!("tag:{}", t2.id),
                    "invalidAddress"
                ),
                (
                    ExclusionUnit::Tag,
                    format!("tag:{}", t6.id),
                    "unknownDataType"
                ),
                (
                    ExclusionUnit::Group,
                    format!("grp:{}", g3.id),
                    "invalidPeriod"
                ),
                (
                    ExclusionUnit::Tag,
                    format!("tag:{}", t5.id),
                    "groupExcluded"
                ),
                (
                    ExclusionUnit::Connection,
                    format!("conn:{}", conn_b.id),
                    "invalidPort"
                ),
                (
                    ExclusionUnit::Group,
                    format!("grp:{}", g2.id),
                    "connectionExcluded"
                ),
                (
                    ExclusionUnit::Tag,
                    format!("tag:{}", t4.id),
                    "connectionExcluded"
                ),
            ]
        );

        // Names and ids are the rows' own; a cascade names its parent so the
        // list can be followed back to what needs fixing.
        assert_eq!(exclusions[0].name, "T2");
        assert_eq!(exclusions[0].id, t2.id);
        assert!(exclusions[3].reason.message().contains("「G3」"));
        assert!(exclusions[5].reason.message().contains("「PLC-B」"));
        assert!(exclusions[6].reason.message().contains("「PLC-B」"));
        // Cascades never produce a strict error of their own.
        assert!(exclusions[3].strict_error().is_none());
        assert!(exclusions[5].strict_error().is_none());

        // The strict build fails on the first one, with the pre-段階2 text.
        match build_config_from(&snapshot) {
            Err(CollectError::Config(message)) => {
                assert!(message.starts_with("タグ T2 のアドレス D3000 が不正です: "))
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(config_exclusions(&snapshot), exclusions);
    }

    /// A tag's exclusion reason is worded exactly like the save-time
    /// rejection of the same tag (#414 段階1) - one vocabulary for "this
    /// address cannot be read".
    #[tokio::test]
    async fn a_tag_exclusion_uses_the_save_time_wording() {
        for (address, data_type) in [("D3000", "i16"), ("40001", "f128"), ("40001.3", "i16")] {
            let mut snapshot = golden_snapshot().await;
            snapshot.tags[0].address = address.to_string();
            snapshot.tags[0].data_type = data_type.to_string();
            let exclusions = config_exclusions(&snapshot);
            assert_eq!(exclusions.len(), 1, "{address} / {data_type}");
            let saved = check_tag_address("modbus-tcp", address, data_type).unwrap_err();
            assert_eq!(exclusions[0].reason.message(), saved.message());
            assert_eq!(exclusions[0].unit, ExclusionUnit::Tag);
        }
    }

    /// Leaving a tag out is exactly "the same config without that tag": the
    /// same plans and the same `StoreConfig` (so the same tstore config hash
    /// - no column is kept for it, and fixing it later rotates the file like
    /// adding a tag does).
    #[tokio::test]
    async fn an_excluded_tag_builds_the_same_config_as_not_having_it() {
        let pool = registry().await;
        let conn = PlcConnectionService::new(pool.clone())
            .create(conn_input("PLC1", 502))
            .await
            .unwrap();
        let group = CollectionGroupService::new(pool.clone())
            .create(group_input("G1", conn.id, 1_000))
            .await
            .unwrap();
        let tags = TagService::new(pool.clone());
        tags.create(tag_input("T1", group.id, "40001"))
            .await
            .unwrap();
        let bad = tags
            .create(tag_input("T2", group.id, "D3000"))
            .await
            .unwrap();
        tags.create(tag_input("T3", group.id, "40003"))
            .await
            .unwrap();

        let with_bad = RegistrySnapshot::load(&pool).await.unwrap();
        let mut without_bad = with_bad.clone();
        without_bad.tags.retain(|t| t.id != bad.id);

        let (lenient, exclusions) = build_config_lenient_from(&with_bad);
        assert_eq!(exclusions.len(), 1);
        assert_eq!(lenient, build_config_from(&without_bad).unwrap());
    }

    /// A broken connection with no groups is still listed (the strict build
    /// fails on it too), with nothing cascading from it.
    #[tokio::test]
    async fn a_broken_connection_without_groups_is_listed_alone() {
        let mut snapshot = golden_snapshot().await;
        snapshot.connections[0].protocol = "ethernet-ip".to_string();
        snapshot.groups.clear();
        snapshot.tags.clear();
        let (config, exclusions) = build_config_lenient_from(&snapshot);
        assert_eq!(config.tag_count(), 0);
        assert_eq!(exclusions.len(), 1);
        assert_eq!(exclusions[0].unit, ExclusionUnit::Connection);
        assert_eq!(exclusions[0].reason.code(), "unsupportedProtocol");
        assert_eq!(
            exclusions[0].reason.message(),
            "プロトコル ethernet-ip は未対応です（modbus-tcp / slmp のみ対応）"
        );
    }
}
