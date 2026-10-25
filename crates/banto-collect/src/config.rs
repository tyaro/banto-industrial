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

use banto_broker::SettingsFingerprint;
use banto_driver_api::{DriverRegistry, Settings, TagSpec};
use banto_tags::{
    scaling::Scaling, CollectionGroup, PlcConnection, StoredSecretRow, Tag, PUSH_PROTOCOL,
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

/// How one connection is collected (R2b-3a-3, docs/banto-hub-driver-io-design.md
/// §4.3): read through its broker session ([`Self::Broker`]), or sampled from an
/// [`crate::external::ExternalSource`] ([`Self::External`]).
///
/// Until R2b-3a-3 this had one variant per protocol carrying that protocol's
/// client configuration (`ModbusTcpConfig` / `SlmpConfig`), projected by hand
/// into a public `ClientSpec` and back into a client - the copy that dropped
/// `word_order` (#334) and the SLMP CPU (#605). The broker now dials through
/// the driver registry with the connection's `Settings`, so the plan only has
/// to carry what makes two plans differ.
///
/// `PartialEq` (T7-1, docs/tag-server-design.md §4.3): the building block
/// [`crate::collector::Collector::apply_config`] uses to tell "this
/// connection's settings changed" from "identical, leave its task alone".
///
/// #383 段階3 P3: [`Self::External`] is a `"hub"` (or R2a-1b `"push"`)
/// connection - nothing to dial; its task samples a
/// [`crate::external::ExternalSource`] instead
/// (`crate::task::run_external_connection`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ProtocolConfig {
    Broker(BrokerPlan),
    External,
}

/// A broker-read connection's part of the plan (see [`ProtocolConfig`]).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BrokerPlan {
    /// The connection's settings as the registry stores them
    /// (`banto_tags::settings_to_api`, no secret values) - an edit to any of
    /// them makes the plan compare unequal, so the connection's task is
    /// replaced.
    pub settings: Settings,
    /// The fingerprint of the session the task reads through, stamped by the
    /// host after it ensured the session ([`CollectorConfig::set_broker_dial_target`]);
    /// `None` until then.
    pub dial: Option<SettingsFingerprint>,
}

/// A tag's fixed H/HH/L/LL limits (any subset may be set), compared against
/// the *scaled* value. Ordering (`ll <= l <= h <= hh` among the set ones) is
/// the caller's job to guarantee at write time - its own validation of the
/// limits it supplies ([`build_config_lenient_with_thresholds`]).
///
/// `pub` since #532 (2026-10-08 owner decision: しきい値は使う側の設定): the
/// tag registry carries no limits at all (#533 dropped the `threshold_*`
/// columns), so a caller that judges thresholds (ChronoGazer's recorder-side
/// per-tag settings) builds these itself and hands them in. The builders that
/// take no limits ([`build_config`] / [`build_config_from`] /
/// [`build_config_lenient_from`] - banto-hub's) give every tag empty limits,
/// so they never raise a `threshold_*` event.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Thresholds {
    pub hh: Option<f64>,
    pub h: Option<f64>,
    pub l: Option<f64>,
    pub ll: Option<f64>,
}

impl Thresholds {
    /// True when no limit is set - the hot loop skips threshold
    /// classification entirely for such tags.
    pub fn is_empty(&self) -> bool {
        self.hh.is_none() && self.h.is_none() && self.l.is_none() && self.ll.is_none()
    }

    /// The limit that defines `level`'s band (#532: recorded on every
    /// `threshold_*` event as the value the judgement was made against).
    /// `None` only if that level's limit is unset - which cannot happen for a
    /// level [`crate::task`]'s classifier actually returned for these limits.
    pub fn limit_for(&self, level: crate::event::ThresholdLevel) -> Option<f64> {
        use crate::event::ThresholdLevel;
        match level {
            ThresholdLevel::Hh => self.hh,
            ThresholdLevel::H => self.h,
            ThresholdLevel::L => self.l,
            ThresholdLevel::Ll => self.ll,
        }
    }
}

/// Where [`build_config_lenient_inner`] takes each tag's [`Thresholds`] from.
#[derive(Clone, Copy)]
enum ThresholdSource<'a> {
    /// No limits for any tag (#533: banto-hub holds no thresholds and judges
    /// no alarms - Hub はデータを集めて配る役に徹する).
    None,
    /// Limits supplied by the caller, keyed by tag id. A tag with no entry
    /// has no limits (#532: 既定は設定なし).
    Supplied(&'a HashMap<i64, Thresholds>),
}

impl ThresholdSource<'_> {
    fn for_tag(&self, tag: &Tag) -> Thresholds {
        match self {
            Self::None => Thresholds::default(),
            Self::Supplied(map) => map.get(&tag.id).copied().unwrap_or_default(),
        }
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
    /// `banto_tags::scale_raw`. Never applied to a bit tag: those read as
    /// `banto_driver_api::Value::Bit` and map straight to 0.0/1.0 (there is no
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
/// (`PartialEq` on [`TagSpec`]) and `tags` positionally, so
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
    /// Reads in tag order (`requests[i]` <-> `tags[i]` <-> tstore column
    /// `c{i+1}`), each the connection driver's own `parse_address` result
    /// with the tag's kind (R2b-3a-3). Passed straight to
    /// `crate::CollectClient::read_batch`. Empty for an external (`"hub"`)
    /// group.
    pub requests: Vec<TagSpec>,
    /// #383 段階3 P3: for an external (`"hub"`) group, the external name of
    /// each tag in tag order (`external_names[i]` <-> `tags[i]`) - what the
    /// task looks up in the source's snapshot. Empty for a PLC group.
    pub external_names: Vec<String>,
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
    /// `banto_tags::PlcConnection::simulation`. Decides whether a tick of
    /// this connection is appended to the store: a simulated value is shown
    /// (current values, thresholds, events) but never recorded as a fact of
    /// the physical PLC (`task.rs::record_ticks`). Part of this struct's
    /// `PartialEq`, so toggling it replaces the connection's task. The
    /// simulator itself is no longer started here: since R2b-3a-3 the host
    /// substitutes its address before the broker session is ensured
    /// ([`crate::BrokerSimRegistry`]).
    pub simulation: bool,
}

/// The immutable configuration snapshot [`crate::Collector::start_with_client_factory`]
/// consumes. Opaque by design: build it with [`build_config`] and hand it to the collector;
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

    /// T9-2 (apps/banto-hub/core/src/hub.rs): force `ConnectionPlan::simulation
    /// = false` for every connection whose key is in `connection_keys`, so
    /// those connections' ticks are recorded even while simulated. banto-hub
    /// calls this for every broker-read connection (its simulated values are
    /// recorded like any Good value); ChronoGazer does not (simulated values
    /// are never recorded there). A key not present in `self.connections` is
    /// silently ignored.
    pub fn suppress_simulation_for(&mut self, connection_keys: &std::collections::HashSet<String>) {
        for conn in &mut self.connections {
            if connection_keys.contains(&conn.key) {
                conn.simulation = false;
            }
        }
    }

    /// Stamp the broker-read plan keyed by `key` with the fingerprint of the
    /// session its task will read through (no-op for an absent key or an
    /// external plan). R2b-3a-3: the fingerprint replaced the `(host, port)`
    /// this used to stamp (T9-2), so a change of any dial setting - the
    /// simulator's address, a SLMP CPU type, a re-sealed secret reference -
    /// counts, not just the endpoint.
    ///
    /// **Why it is needed**: [`crate::Collector::apply_config`]'s diff is
    /// only `ConnectionPlan == ConnectionPlan`. A connection whose plan
    /// compares equal keeps its running task untouched, **including the
    /// `crate::ClientFactory` closure (and so the broker handle) it captured
    /// at spawn time**. When the host replaced the connection's broker
    /// session (the dial settings changed: simulation toggled, simulator
    /// restarted), the registry row - and so the rest of the plan - can be
    /// identical; without the stamp the old task would keep reading a
    /// superseded session forever. With it the plan compares unequal, the
    /// connection is classified "replaced", and the new task gets the new
    /// handle. Stamping an unchanged session again writes the same value, so
    /// the connection stays "unchanged".
    pub fn set_broker_dial_target(&mut self, key: &str, fingerprint: SettingsFingerprint) {
        if let Some(conn) = self.connections.iter_mut().find(|c| c.key == key) {
            if let ProtocolConfig::Broker(plan) = &mut conn.config {
                plan.dial = Some(fingerprint);
            }
        }
    }

    /// The keys of the connections this config reads through the broker -
    /// the ones a host must ensure a session for.
    pub fn broker_connection_keys(&self) -> HashSet<String> {
        self.connections
            .iter()
            .filter(|conn| matches!(conn.config, ProtocolConfig::Broker(_)))
            .map(|conn| conn.key.clone())
            .collect()
    }

    /// #383 段階3 P3: does this config collect any external (`"hub"` or,
    /// R2a-1b, `"push"`)
    /// connection? [`crate::Collector::start_with_sources`] refuses such a
    /// config without an [`crate::external::ExternalSource`]. For a config
    /// from the lenient build this means "at least one hub tag is
    /// collected": an external group with no collectable tag is never
    /// planned (`build_config_lenient_inner`), so neither is a connection
    /// left without groups.
    pub fn has_external_connections(&self) -> bool {
        self.connections
            .iter()
            .any(|conn| conn.config == ProtocolConfig::External)
    }

    /// #383 段階3 P4: every external name this config actually samples from
    /// an external (`"hub"`) connection, sorted and without duplicates. For a
    /// config from the lenient build this is the post-reconciliation set - a
    /// tag left out against the Hub catalog (`hubTagNotFound` /
    /// `hubTagTypeChanged`) is not in it - so a caller can narrow the demand
    /// it raised before the build to what is really collected, without
    /// another round-trip to the Hub. Empty for a PLC-only config.
    pub fn external_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .connections
            .iter()
            .filter(|conn| conn.config == ProtocolConfig::External)
            .flat_map(|conn| conn.groups.iter())
            .flat_map(|group| group.external_names.iter().cloned())
            .collect();
        names.sort();
        names.dedup();
        names
    }
}

/// #383 段階3 P3: the Hub catalog the lenient build reconciles hub tags
/// against - external name -> the Hub's `data_type` for that tag (H3:
/// identity is the external name).
pub type HubCatalog = HashMap<String, String>;

/// #383 段階3 P3: whether the calling runtime can collect `"hub"`
/// connections, passed to [`build_config_lenient_with_thresholds`].
///
/// | value | a `"hub"` connection is |
/// | --- | --- |
/// | [`Self::Unavailable`] | excluded ([`ExclusionReason::HubSourceUnavailable`]) with its groups and tags |
/// | [`Self::NotConfigured`] | excluded ([`ExclusionReason::HubNotConfigured`]) with its groups and tags |
/// | [`Self::Available`] with a catalog | collected; a tag missing from the catalog or whose data type differs is excluded alone ([`ExclusionReason::HubTagNotFound`] / [`ExclusionReason::HubTagTypeChanged`], #414's "外して残りを動かす") |
/// | [`Self::Available`] without a catalog | collected, nothing reconciled (the Hub could not be asked at start; a tag it does not have reads Bad until the next start) |
#[derive(Debug, Clone, Copy)]
pub enum HubSource<'a> {
    /// This runtime has no Hub subscription at all (banto-hub, ChronoGazer's
    /// `banto-serve` which has no OS keyring).
    Unavailable,
    /// The runtime could subscribe, but no Hub connection is set up
    /// (ChronoGazer's `hub.record` is absent).
    NotConfigured,
    /// The runtime subscribes; `catalog` is the Hub's tag list read at
    /// start, if it could be read.
    Available { catalog: Option<&'a HubCatalog> },
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
    /// R2b-1d (docs/banto-hub-connection-settings-design.md §5.2・§7): the **sealed**
    /// secret rows (`plc_connection_secrets`) the connections' `settings` refer to with
    /// `{"$secret": "<ref>"}`, read in the same transaction as the rows above. Never
    /// plaintext: the reader that needs a value (banto-hub's DB Source) opens it in
    /// banto-hub's `secrets` module right before use. The collection plan itself does
    /// not read them.
    pub secrets: Vec<StoredSecretRow>,
}

impl RegistrySnapshot {
    /// Read the registry rows once into a reusable logical snapshot.
    ///
    /// PR #777 owner review P2: all four reads (connections, groups, tags, secret rows)
    /// run in **one** read transaction ([`Self::load_connection`] on a deferred
    /// `BEGIN`), so a write that commits in between - e.g. a password update that writes
    /// a new reference + row and deletes the old row - cannot leave the snapshot with a
    /// connection that refers to a row it does not hold (a healthy DB Source connection
    /// read as `Missing`).
    pub async fn load(pool: &SqlitePool) -> Result<Self, CollectError> {
        Self::load_between(pool, || async {}).await
    }

    /// [`Self::load`] with `between` awaited after the connection rows are read and
    /// before the secret rows are (the tests commit a write there).
    async fn load_between<F, Fut>(pool: &SqlitePool, between: F) -> Result<Self, CollectError>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        let storage = |err: sqlx::Error| {
            CollectError::Registry(banto_core::BantoError::Storage(err.to_string()))
        };
        let mut tx = pool.begin().await.map_err(storage)?;
        let snapshot = Self::load_connection_between(&mut tx, between).await?;
        tx.rollback().await.map_err(storage)?;
        Ok(snapshot)
    }

    /// Read the same registry snapshot from an already-open SQLite connection.
    /// Callers use this with a transaction/savepoint so proposed mutations and
    /// all three registry reads observe one transaction-local state.
    pub async fn load_connection(connection: &mut SqliteConnection) -> Result<Self, CollectError> {
        Self::load_connection_between(connection, || async {}).await
    }

    async fn load_connection_between<F, Fut>(
        connection: &mut SqliteConnection,
        between: F,
    ) -> Result<Self, CollectError>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        // Column names must match `PlcConnection` (`banto-tags`' `COLUMNS`).
        // `FromRow` looks them up by name, so a missing name fails every
        // preflight with "no column found" even though the table has the column.
        // R2b-1d (migration 0025): the flat endpoint columns moved into `settings`;
        // `FromRow` projects them back (`banto_tags::PlcConnection::from_settings`).
        let connections = sqlx::query_as::<_, PlcConnection>(
            "SELECT id, name, protocol, enabled, simulation, settings, settings_version \
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
             enabled, writable, tag_kind, expression, retain, revision FROM tags ORDER BY id",
        )
        .fetch_all(&mut *connection)
        .await
        .map_err(|err| CollectError::Registry(banto_core::BantoError::Storage(err.to_string())))?;

        between().await;
        let secrets = banto_tags::connection_secret::load_all_secret_rows(&mut *connection)
            .await
            .map_err(|err| {
                CollectError::Registry(banto_core::BantoError::Storage(err.to_string()))
            })?;

        Ok(Self {
            connections,
            groups,
            tags,
            secrets,
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
/// Fails with [`CollectError::Config`] if any *included* connection's driver
/// is missing or refuses its settings, or any included tag's stored address
/// is refused by that driver's `parse_address` or its `data_type` is outside
/// the known vocabulary. Every tag is read through `drivers` (R2b-3a-3,
/// docs/banto-hub-driver-io-design.md §4.3): the plan carries the driver's own
/// `TagSpec`s. On the legacy save path (the plain `TagService::new`,
/// ChronoGazer) `banto-tags` does not validate the address with the driver, so
/// this is where such a tag is caught, before any session is opened. Registry
/// read failures surface as [`CollectError::Registry`].
pub async fn build_config(
    pool: &SqlitePool,
    drivers: &DriverRegistry,
) -> Result<CollectorConfig, CollectError> {
    let snapshot = RegistrySnapshot::load(pool).await?;
    build_config_from(&snapshot, drivers)
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
pub fn build_config_from(
    snapshot: &RegistrySnapshot,
    drivers: &DriverRegistry,
) -> Result<CollectorConfig, CollectError> {
    build_config_from_with_registry(snapshot, PushSource::Unavailable, drivers)
}

/// R2b-1d (docs/banto-hub-connection-settings-design.md §4.4・§7): the strict build for
/// a runtime that can (or cannot) collect `"push"` connections
/// (docs/banto-hub-ingest-design.md §4.1, R2a-1b; [`PushSource::Unavailable`] is
/// exactly [`build_config_from`]). It checks every collected connection against `registry` first
/// ([`driver_exclusion`]): a connection whose driver is missing, whose stored
/// `settings` the driver refuses, or whose settings shape is newer than the driver is
/// an exclusion ([`ExclusionReason::DriverUnavailable`] /
/// [`ExclusionReason::InvalidSettings`] / [`ExclusionReason::SettingsVersionTooNew`]).
/// This being the strict build, it then fails the build with that item's wording, the
/// same way an unusable port does (banto-hub's plan and its save-time preflight; the
/// lenient builders keep the rest running). banto-hub's plan goes through this
/// (`banto_hub_core::ingest::build_hub_config`). Since R2b-3a-3 every builder takes the
/// host's registry (ChronoGazer's is `banto_drivers::builtin().registry`).
pub fn build_config_from_with_registry(
    snapshot: &RegistrySnapshot,
    push: PushSource,
    registry: &DriverRegistry,
) -> Result<CollectorConfig, CollectError> {
    let (config, exclusions) =
        build_config_lenient_inner(snapshot, ThresholdSource::None, None, push, registry);
    match exclusions.iter().find_map(ConfigExclusion::strict_error) {
        Some(err) => Err(err),
        None => Ok(config),
    }
}

/// R2b-1d (design §4.4): why `conn` cannot be collected under `registry`, if it
/// cannot - checked in this order:
///
/// 1. `registry.get(protocol)` is `None` → [`ExclusionReason::DriverUnavailable`].
/// 2. `settings_version` > the driver's `settings_version` →
///    [`ExclusionReason::SettingsVersionTooNew`] (design §3.4).
/// 3. the driver's `validate_config` (`banto_tags::validate_settings_with_registry`),
///    then the storage-layer shape check (`banto_tags::check_settings_shape`: a secret
///    key holds only a reference, design §3.2's JSON types) → [`ExclusionReason::InvalidSettings`].
///
/// Pure; also used by banto-hub's migration preflight and status surfaces.
pub fn driver_exclusion(
    conn: &PlcConnection,
    registry: &DriverRegistry,
) -> Option<ExclusionReason> {
    let Some(entry) = registry.get(&conn.protocol) else {
        return Some(ExclusionReason::DriverUnavailable {
            protocol: conn.protocol.clone(),
        });
    };
    let supported = i64::from(entry.descriptor().settings_version);
    if conn.settings_version > supported {
        return Some(ExclusionReason::SettingsVersionTooNew {
            stored: conn.settings_version,
            supported,
        });
    }
    let mut errors =
        banto_tags::validate_settings_with_registry(registry, &conn.protocol, &conn.settings);
    if errors.is_empty() {
        errors = banto_tags::check_settings_shape(
            &conn.protocol,
            &conn.settings,
            &entry.base().secret_keys(),
        );
    }
    if errors.is_empty() {
        None
    } else {
        Some(ExclusionReason::InvalidSettings {
            errors: errors
                .into_iter()
                .map(|error| format!("{}: {}", error.field, error.message))
                .collect(),
        })
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

/// Why a stored tag was excluded from a collection plan. Save-time validation
/// lives in banto-tags; this describes rows encountered when building a plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TagExclusionCause {
    InvalidAddress { reason: String },
    UnknownDataType,
}

impl TagExclusionCause {
    fn code(&self) -> &'static str {
        match self {
            Self::InvalidAddress { .. } => "invalidAddress",
            Self::UnknownDataType => "unknownDataType",
        }
    }

    fn message(&self) -> String {
        match self {
            Self::InvalidAddress { reason } => {
                format!("このドライバのアドレスとして解釈できません。{reason}")
            }
            Self::UnknownDataType => "未対応のデータ型です".to_string(),
        }
    }
}

/// Why [`build_config_lenient_from`] left an item out (#414 段階2).
///
/// The own-fault variants correspond to `CollectError::Config` cases from
/// [`build_config_from`]. The final two variants are cascades from an excluded
/// connection or group. Values come from registry rows and driver errors,
/// never host credentials or file paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExclusionReason {
    /// The connection's driver is in the registry but the broker does not
    /// read it (not a session-type driver - e.g. a host-provided one,
    /// `banto_broker::is_broker_managed`). A `simulation = true` connection
    /// is no reason to exclude (R2b-5b: its session is the value layer's
    /// `SimDevice`, for any broker-managed protocol).
    UnsupportedProtocol { protocol: String },
    /// #383 段階3 P3: a `"hub"` connection (banto-hub as a source,
    /// `banto_tags::HUB_PROTOCOL`) in a runtime that cannot subscribe to the
    /// Hub ([`HubSource::Unavailable`]: banto-hub itself, ChronoGazer's
    /// `banto-serve`). The connection - and its groups and tags as cascades -
    /// is left out, and the rest keeps running (#414). (P2 called this
    /// `hubSourceNotYetCollected`; P3 added the sampler, so the reason is now
    /// about the runtime, not the crate.)
    HubSourceUnavailable,
    /// R2a-1b (docs/banto-hub-ingest-design.md §4.1): a `"push"` connection
    /// (`banto_tags::PUSH_PROTOCOL`) in a runtime that cannot receive pushed
    /// values ([`PushSource::Unavailable`]: ChronoGazer, and every build that
    /// is not banto-hub's ingest-enabled one). Cascades like
    /// [`Self::HubSourceUnavailable`]: the connection is left out with its
    /// groups and tags, the rest keeps running.
    PushSourceUnavailable,
    /// #383 段階3 P3: a `"hub"` connection while no Hub connection is set up
    /// ([`HubSource::NotConfigured`]). Cascades like the one above.
    HubNotConfigured,
    /// #383 段階3 P3 (H3): a hub tag whose external name is not in the Hub's
    /// catalog at collection start (deleted / renamed on the Hub, or not
    /// visible to this key). The tag alone is left out.
    HubTagNotFound { external_name: String },
    /// #383 段階3 P3 (H3): a hub tag whose registered data type no longer
    /// matches the Hub's. The tag alone is left out (its column type would
    /// lie).
    HubTagTypeChanged {
        external_name: String,
        registered: String,
        catalog: String,
    },
    /// `collection_groups.period_ms` does not fit `u32`.
    InvalidPeriod { period_ms: i64 },
    /// The stored tag's address/data type cannot be read by its driver.
    InvalidTag {
        address: String,
        data_type: String,
        issue: TagExclusionCause,
    },
    /// R2b-1d (docs/banto-hub-connection-settings-design.md §4.4): the connection's
    /// `protocol` is not in the caller's driver registry - a `"hub"` row in banto-hub
    /// (the Hub registers no `hub` driver), or a plugin that was removed. The row is
    /// kept (never deleted or reinterpreted as another protocol, plugin-design §7.5);
    /// only this connection is left out. Only a build given a registry
    /// ([`build_config_from_with_registry`]) raises it.
    DriverUnavailable { protocol: String },
    /// R2b-1d (design §4.4): the driver's `validate_config` (or the storage-layer shape
    /// check, `banto_tags::check_settings_shape`) refuses the stored `settings` - a row
    /// edited by hand. `errors` are `"<field>: <message>"` (REST spellings), never a
    /// secret value (secret keys hold only references, and references are not passed to
    /// `validate_config`).
    InvalidSettings { errors: Vec<String> },
    /// R2b-1d (design §3.4): the row was written with a newer settings shape than this
    /// driver reads (`settings_version` > the driver's `settings_version`) - a database
    /// written by a newer Hub or a plugin rolled back.
    SettingsVersionTooNew { stored: i64, supported: i64 },
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
            ExclusionReason::HubSourceUnavailable => "hubSourceUnavailable",
            ExclusionReason::PushSourceUnavailable => "pushSourceUnavailable",
            ExclusionReason::HubNotConfigured => "hubNotConfigured",
            ExclusionReason::HubTagNotFound { .. } => "hubTagNotFound",
            ExclusionReason::HubTagTypeChanged { .. } => "hubTagTypeChanged",
            ExclusionReason::InvalidPeriod { .. } => "invalidPeriod",
            ExclusionReason::InvalidTag { issue, .. } => issue.code(),
            ExclusionReason::DriverUnavailable { .. } => "driverUnavailable",
            ExclusionReason::InvalidSettings { .. } => "invalidSettings",
            ExclusionReason::SettingsVersionTooNew { .. } => "settingsVersionTooNew",
            ExclusionReason::ConnectionExcluded { .. } => "connectionExcluded",
            ExclusionReason::GroupExcluded { .. } => "groupExcluded",
        }
    }

    /// A human-readable reason, for a list that shows the item's unit and
    /// name next to it (so, unlike the strict error text, it does not repeat
    /// the name). Stored invalid tags carry the driver's refusal.
    pub fn message(&self) -> String {
        match self {
            ExclusionReason::UnsupportedProtocol { protocol } => {
                format!(
                    "プロトコル {protocol} は収集に未対応です（broker が読むドライバと hub のみ。シミュレーションは modbus-tcp / slmp のみ）"
                )
            }
            ExclusionReason::HubSourceUnavailable => {
                "この実行形態では Hub 経由の接続から収集できません".to_string()
            }
            ExclusionReason::PushSourceUnavailable => {
                "この実行形態では push 接続から収集できません".to_string()
            }
            ExclusionReason::HubNotConfigured => {
                "Hub への接続が設定されていないため収集しません（Hub に接続してから収集を再起動してください）"
                    .to_string()
            }
            ExclusionReason::HubTagNotFound { external_name } => format!(
                "Hub のタグ一覧に {external_name} が見つからないため収集しません（Hub で削除・改名されたか、このキーでは見えません）"
            ),
            ExclusionReason::HubTagTypeChanged {
                external_name,
                registered,
                catalog,
            } => format!(
                "Hub のタグ {external_name} のデータ型が変わったため収集しません（登録: {registered} / Hub: {catalog}）"
            ),
            ExclusionReason::InvalidPeriod { period_ms } => {
                format!("収集周期（period_ms）が不正です: {period_ms}")
            }
            ExclusionReason::InvalidTag { issue, .. } => issue.message(),
            ExclusionReason::DriverUnavailable { protocol } => format!(
                "プロトコル {protocol} のドライバがこの実行形態にありません（接続は残しています）"
            ),
            ExclusionReason::InvalidSettings { errors } => {
                format!("接続の設定が検証を通りません: {}", errors.join(" / "))
            }
            ExclusionReason::SettingsVersionTooNew { stored, supported } => format!(
                "接続の設定の版（{stored}）がドライバの読める版（{supported}）より新しいため収集しません"
            ),
            ExclusionReason::ConnectionExcluded { connection_name } => {
                format!("所属する接続「{connection_name}」が除外されたため、収集しません")
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
                "接続 {name} のプロトコル {protocol} は収集に未対応です（broker が読むドライバと hub のみ。シミュレーションは modbus-tcp / slmp のみ）"
            ),
            ExclusionReason::HubSourceUnavailable => format!(
                "接続 {name} は Hub 経由の接続です。この実行形態では Hub 経由の接続から収集できません"
            ),
            ExclusionReason::PushSourceUnavailable => format!(
                "接続 {name} は push 接続です。この実行形態では push 接続から収集できません"
            ),
            ExclusionReason::HubNotConfigured => format!(
                "接続 {name} は Hub 経由の接続ですが、Hub への接続が設定されていません"
            ),
            ExclusionReason::HubTagNotFound { external_name } => {
                format!("タグ {name} の Hub のタグ {external_name} が Hub に見つかりません")
            }
            ExclusionReason::HubTagTypeChanged {
                external_name,
                registered,
                catalog,
            } => format!(
                "タグ {name} の Hub のタグ {external_name} のデータ型が変わりました（登録: {registered} / Hub: {catalog}）"
            ),
            ExclusionReason::InvalidPeriod { period_ms } => {
                format!("グループ {name} の period_ms が不正です: {period_ms}")
            }
            ExclusionReason::InvalidTag {
                address,
                data_type,
                issue,
            } => match issue {
                TagExclusionCause::InvalidAddress { reason } => {
                    format!("タグ {name} のアドレス {address} が不正です: {reason}")
                }
                TagExclusionCause::UnknownDataType => {
                    format!("タグ {name} のデータ型 {data_type} は未対応です")
                }
            },
            ExclusionReason::DriverUnavailable { protocol } => format!(
                "接続 {name} のプロトコル {protocol} のドライバがありません（接続は残しています）"
            ),
            ExclusionReason::InvalidSettings { errors } => format!(
                "接続 {name} の設定が検証を通りません: {}",
                errors.join(" / ")
            ),
            ExclusionReason::SettingsVersionTooNew { stored, supported } => format!(
                "接続 {name} の設定の版（{stored}）がドライバの読める版（{supported}）より新しいです"
            ),
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
/// keeps using the strict one; chronogazer starts collection with
/// [`build_config_lenient_with_thresholds`].
///
/// #533: every tag gets **no** limits (the registry has no threshold
/// columns any more), so a collector built from this raises no
/// `threshold_*` event.
///
/// #383 段階3 P3: a `"hub"` connection is excluded - this is the builder
/// banto-hub's strict path shares, and banto-hub never subscribes to a Hub.
/// Since R2b-3a-3 the reason is the driver check's
/// [`ExclusionReason::DriverUnavailable`] (banto-hub registers no `hub`
/// driver), as in the strict build; ChronoGazer's builders say
/// [`ExclusionReason::HubSourceUnavailable`] ([`HubSource::Unavailable`]).
pub fn build_config_lenient_from(
    snapshot: &RegistrySnapshot,
    drivers: &DriverRegistry,
) -> (CollectorConfig, Vec<ConfigExclusion>) {
    build_config_lenient_inner(
        snapshot,
        ThresholdSource::None,
        // The strict build's twin (banto-hub): a `hub` row is the driver
        // check's, like the strict build's (`resolve_connection`'s doc).
        None,
        PushSource::Unavailable,
        drivers,
    )
}

/// [`build_config_lenient_from`] with the tags' H/HH/L/LL limits **supplied
/// by the caller** (#532, 2026-10-08 owner decision: しきい値はタグ定義の属性
/// ではなく、使う側（記録計・SCADA）が持つ設定). `thresholds` is keyed by tag
/// id; a tag with no entry gets no limits (既定は設定なし).
///
/// Everything else - filtering, order, exclusions - is identical (same
/// interpreter): thresholds never cause an exclusion. ChronoGazer uses this;
/// banto-hub uses [`build_config_from`], which gives no tag any limits (#533:
/// Hub は警報を判定しない). The caller is responsible for the
/// `ll <= l <= h <= hh` ordering of what it supplies (see [`Thresholds`]).
///
/// #383 段階3 P3: `hub` says whether the caller can collect `"hub"`
/// connections and, if so, which Hub catalog to reconcile their tags
/// against ([`HubSource`]'s table). Only an [`HubSource::Available`] build
/// produces external connection plans, which then need an
/// [`crate::external::ExternalSource`] at
/// [`crate::Collector::start_with_sources`].
pub fn build_config_lenient_with_thresholds(
    snapshot: &RegistrySnapshot,
    thresholds: &HashMap<i64, Thresholds>,
    hub: HubSource<'_>,
    drivers: &DriverRegistry,
) -> (CollectorConfig, Vec<ConfigExclusion>) {
    build_config_lenient_inner(
        snapshot,
        ThresholdSource::Supplied(thresholds),
        Some(hub),
        PushSource::Unavailable,
        drivers,
    )
}

/// R2a-1b (docs/banto-hub-ingest-design.md §4.1): whether the calling runtime
/// can collect `"push"` connections (values pushed into banto-hub by an
/// external sender, read through an [`crate::external::ExternalSource`]).
///
/// | value | a `"push"` connection is |
/// | --- | --- |
/// | [`Self::Unavailable`] | excluded ([`ExclusionReason::PushSourceUnavailable`]) with its groups and tags - ChronoGazer, and every existing entry point |
/// | [`Self::Available`] | collected as [`ProtocolConfig::External`] (key = trimmed address, no scaling), like a `"hub"` connection; a group without a collectable tag is not planned |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushSource {
    Unavailable,
    Available,
}

/// How one enabled connection is collected, once its row is understood.
enum ResolvedConnection<'a> {
    /// Read through its broker session.
    Plc(ProtocolConfig),
    /// Sampled from an [`crate::external::ExternalSource`] (#383 段階3 P3).
    Hub { catalog: Option<&'a HubCatalog> },
    /// Received from pushed values (R2a-1b): same [`ProtocolConfig::External`]
    /// plan as `Hub`, but the key is the sender's own name and nothing is
    /// reconciled against a catalog.
    Push,
}

fn build_config_lenient_inner(
    snapshot: &RegistrySnapshot,
    threshold_source: ThresholdSource<'_>,
    hub: Option<HubSource<'_>>,
    push: PushSource,
    drivers: &DriverRegistry,
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
    // protocol string, which `resolve_connection` still excludes). Any tags
    // registered under it (computed/internal, per banto-tags' own
    // `calc`/`mem` placement rule) are evaluated/stored entirely by
    // `apps/banto-hub/core/src/computed.rs`'s `ComputedEngine`/
    // `ServerTagStore`, never by this recorder pipeline.
    //
    // S1 (docs/banto-hub-external-db-design.md §4.1・§7 スライス表「S1」,
    // 2026-09-06): a `"postgres"` connection ([`PlcConnection::is_db_source`])
    // gets the identical exclusion, for the identical reason - it is not a
    // wire-protocol PLC connection either, and its driver (if any) is not
    // broker-read. This is defense-in-depth as much as anything: S1 also
    // forbids creating any collection group under a `"postgres"` connection
    // (`banto_tags::collection_group::CollectionGroupService`), so in
    // practice such a connection is always tagless - but without this
    // exclusion, merely *enabling* a `"postgres"` connection (zero groups or
    // not) would make the strict build reject the whole config, since the
    // loop below runs over every enabled connection regardless of whether it
    // has any groups yet.
    //
    // (Neither of these is a `ConfigExclusion`: they are not collected by
    // design, not "left out because broken".)
    //
    // #383 段階3 P2/P3: a `"hub"` connection ([`PlcConnection::is_hub_source`])
    // is deliberately NOT filtered here. It is collected through an external
    // source when the caller can ([`HubSource::Available`]); otherwise
    // `resolve_connection` below turns it into an explicit
    // [`ExclusionReason::HubSourceUnavailable`] / [`ExclusionReason::HubNotConfigured`],
    // which takes its groups and tags with it as cascades and leaves every
    // other connection running (#414's "外して残りを動かす") - silently
    // contributing nothing would hide its tags' missing history.
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

        let resolved = match resolve_connection(conn, hub, push, drivers) {
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
            let mut external_names = Vec::new();
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
                // This must happen *before* build_spec, whose
                // tag_kind would otherwise exclude the tag
                // over a tag that belongs to a different app (relay-wright's
                // S2 engine is the consumer of string tags).
                if tag.data_type == banto_tags::STRING_DATA_TYPE {
                    continue;
                }
                let scaling = match &resolved {
                    // R2b-2c / R2b-3a-3: every tag is read through the driver.
                    ResolvedConnection::Plc(_) => {
                        match build_spec(tag, &conn.protocol, drivers) {
                            Ok(request) => {
                                requests.push(request);
                                tag.scaling()
                            }
                            Err(issue) => {
                                // #414 段階2: this tag alone is left out - no
                                // read, no store column (its history reads as
                                // null), and the rest of the group still
                                // collects.
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
                        }
                    }
                    ResolvedConnection::Push => match push_external_key(tag) {
                        Ok(key) => {
                            external_names.push(key);
                            // The sender delivers engineering values; the
                            // registry only accepts an identity scaling for a
                            // push tag (§3.3), and none is applied here.
                            None
                        }
                        Err(reason) => {
                            exclusions.push(ConfigExclusion::tag(tag, reason));
                            continue;
                        }
                    },
                    ResolvedConnection::Hub { catalog } => match hub_external_name(tag, *catalog) {
                        Ok(name) => {
                            external_names.push(name);
                            // The Hub delivers engineering values (its own
                            // scaling is already applied), so a scaling on
                            // the registry row is never applied again here.
                            None
                        }
                        Err(reason) => {
                            // #383 段階3 P3 (H3, #414): this tag alone is
                            // left out; the rest of the group collects.
                            exclusions.push(ConfigExclusion::tag(tag, reason));
                            continue;
                        }
                    },
                };
                readable_tags.push(tag);
                tag_plans.push(TagPlan {
                    key: tag_key(tag.id),
                    scaling,
                    thresholds: threshold_source.for_tag(tag),
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

            // #383 段階3 P3（PR #578 レビュー）: an external (`"hub"`) group
            // with no collectable tag left (none registered, or every one
            // excluded - vanished from / retyped on the Hub) is not planned
            // at all - no task, no store group. A PLC group keeps the old
            // behaviour (an empty group still gets a store group). So an
            // external connection is in the plan **iff it collects at least
            // one tag**, which is exactly what
            // [`CollectorConfig::has_external_connections`] answers: a caller
            // deciding "do I still need the Hub subscription for this run"
            // (ChronoGazer's demand) cannot be told "yes" for a connection
            // that records nothing.
            if matches!(
                resolved,
                ResolvedConnection::Hub { .. } | ResolvedConnection::Push
            ) && tag_plans.is_empty()
            {
                continue;
            }

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
                external_names,
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
        let (config, simulation) = match resolved {
            ResolvedConnection::Plc(client_config) => (client_config, conn.simulation),
            // Connection-level simulation does not apply to a Hub connection
            // (banto-tags stores it as `false`; the Hub labels its own
            // simulated values, `crate::external`).
            ResolvedConnection::Hub { .. } | ResolvedConnection::Push => {
                (ProtocolConfig::External, false)
            }
        };
        connection_plans.push(ConnectionPlan {
            key: connection_key(conn.id),
            config,
            groups: group_plans,
            simulation,
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

/// #414 段階2: just the exclusions the lenient build would make for
/// `snapshot` - "which registry items would be left out if collection were
/// started now". For a registry editor's marks (chronogazer's `/tags`), so
/// the verdict comes from the one builder instead of a re-implementation.
///
/// #383 段階3 P3: these marks are about **the registry's own content**, so
/// a `"hub"` connection is judged as collectable without a catalog
/// ([`HubSource::Available`] with `catalog: None`). Whether this runtime can
/// reach a Hub, and whether a hub tag still exists on the Hub, are facts of
/// a running collection, not of the registry - they show up in the running
/// collector's own exclusion list (the build at collection start), not here.
pub fn config_exclusions(
    snapshot: &RegistrySnapshot,
    drivers: &DriverRegistry,
) -> Vec<ConfigExclusion> {
    build_config_lenient_inner(
        snapshot,
        ThresholdSource::None,
        Some(HubSource::Available { catalog: None }),
        PushSource::Unavailable,
        drivers,
    )
    .1
}

/// Understand one enabled connection row: read through the broker
/// (a session-type driver), sampled from the Hub (`hub`, when `hub` allows it),
/// received from pushed values (`push`, when `push` allows it), or excluded
/// with the reason.
///
/// Order (R2b-3a-3): a `push` row in a runtime without a push source, and a
/// `hub` row in a build that says how this runtime samples the Hub
/// (`hub = Some(..)`: ChronoGazer's builders and the registry-editor marks),
/// are decided by the runtime ([`PushSource`] / [`HubSource`]) before the
/// driver check - neither is a driver of ChronoGazer's registry, and neither
/// has a session to dial. The strict builds (`hub = None`, banto-hub's) keep
/// the R2b-1d order: a hand-written `hub` row is the driver check's
/// [`ExclusionReason::DriverUnavailable`] (banto-hub registers no `hub`
/// driver; its migration report says so).
/// Every other row is checked against the registry first (R2b-1d,
/// docs/banto-hub-connection-settings-design.md §4.4・§7: a row the registry
/// does not know - an unknown protocol, a removed plugin -, whose settings the
/// driver refuses or whose settings shape is newer than the driver is excluded
/// alone; the row itself is kept), and is then read through the broker when
/// the broker reads its driver (`banto_broker::is_broker_managed`, the same
/// predicate as the session sync), `simulation = true` rows included (R2b-5b:
/// a simulated session is the value layer's `SimDevice`, never the wire).
fn resolve_connection<'a>(
    conn: &PlcConnection,
    hub: Option<HubSource<'a>>,
    push: PushSource,
    drivers: &DriverRegistry,
) -> Result<ResolvedConnection<'a>, ExclusionReason> {
    if conn.protocol == PUSH_PROTOCOL && push == PushSource::Unavailable {
        return Err(ExclusionReason::PushSourceUnavailable);
    }
    if let (true, Some(hub)) = (conn.is_hub_source(), hub) {
        return match hub {
            HubSource::Unavailable => Err(ExclusionReason::HubSourceUnavailable),
            HubSource::NotConfigured => Err(ExclusionReason::HubNotConfigured),
            HubSource::Available { catalog } => Ok(ResolvedConnection::Hub { catalog }),
        };
    }
    if let Some(reason) = driver_exclusion(conn, drivers) {
        return Err(reason);
    }
    if conn.protocol == PUSH_PROTOCOL {
        return Ok(ResolvedConnection::Push);
    }
    if conn.is_hub_source() {
        // A registry with a `hub` driver and no hub runtime: still not ours.
        return Err(ExclusionReason::HubSourceUnavailable);
    }
    // R2b-3a-3 (design §4.3): planned exactly when the broker reads the
    // driver - the same predicate the session sync filters on
    // (`banto_broker::is_broker_managed`: a session-type driver; R2b-3b adds
    // frame type there), so a driver added to the registry is collected
    // without a name table here, and the plan never holds a connection the
    // sync would not give a session. Any other kind (a host-provided driver
    // such as `virtual` / `postgres`) is not read through the broker.
    if !banto_broker::is_broker_managed(drivers, &conn.protocol) {
        return Err(ExclusionReason::UnsupportedProtocol {
            protocol: conn.protocol.clone(),
        });
    }
    // Simulation needs no per-protocol support since R2b-5b: a
    // `simulation = true` connection is planned like any other broker-managed
    // one and its session is the value layer's `SimDevice`.
    Ok(ResolvedConnection::Plc(ProtocolConfig::Broker(
        BrokerPlan {
            settings: banto_tags::settings_to_api(&conn.settings),
            dial: None,
        },
    )))
}

/// #383 段階3 P3: a hub tag's external name, if the tag can be collected -
/// its data type must be one the store knows, and (when the Hub's catalog
/// could be read at start, H3) the external name must still be on the Hub
/// with the same data type. The registry already enforced the external-name
/// shape and "not string, not writable" on save
/// (`banto_tags::validate_hub_tag_address`).
fn hub_external_name(tag: &Tag, catalog: Option<&HubCatalog>) -> Result<String, ExclusionReason> {
    if banto_tags::tag_kind(&tag.data_type, tag.string_length, &tag.string_encoding).is_none() {
        return Err(ExclusionReason::InvalidTag {
            address: tag.address.clone(),
            data_type: tag.data_type.clone(),
            issue: TagExclusionCause::UnknownDataType,
        });
    }
    let name = tag.address.trim().to_string();
    if let Some(catalog) = catalog {
        match catalog.get(&name) {
            None => {
                return Err(ExclusionReason::HubTagNotFound {
                    external_name: name,
                })
            }
            Some(hub_type) if *hub_type != tag.data_type => {
                return Err(ExclusionReason::HubTagTypeChanged {
                    external_name: name,
                    registered: tag.data_type.clone(),
                    catalog: hub_type.clone(),
                })
            }
            Some(_) => {}
        }
    }
    Ok(name)
}

/// R2a-1b: a push tag's external key - the trimmed `address` (the registry
/// already validated the shape on save, §3.3), provided its data type is one
/// the store knows. `string` tags never get here (skipped before the
/// per-protocol match, like every other protocol).
fn push_external_key(tag: &Tag) -> Result<String, ExclusionReason> {
    if banto_tags::tag_kind(&tag.data_type, tag.string_length, &tag.string_encoding).is_none() {
        return Err(ExclusionReason::InvalidTag {
            address: tag.address.clone(),
            data_type: tag.data_type.clone(),
            issue: TagExclusionCause::UnknownDataType,
        });
    }
    Ok(tag.address.trim().to_string())
}

/// R2b-2c / R2b-3a-3 (docs/banto-hub-driver-io-design.md §4.3): a tag's read as
/// the plan carries it - the connection's *driver* parses the address
/// (`parse_address` with the tag's [`banto_tags::tag_kind`]) and the result is
/// the [`TagSpec`] itself (the bridge back to `banto_plc::Address` is gone). A
/// refusal carries the driver's message and leaves only that tag out. A bit-in-word address on a
/// non-`bit` tag is the driver's refusal too (R2b-2a moved that rule into
/// `parse_address`).
fn build_spec(
    tag: &Tag,
    conn_protocol: &str,
    registry: &DriverRegistry,
) -> Result<TagSpec, TagExclusionCause> {
    let invalid = |reason: String| TagExclusionCause::InvalidAddress { reason };
    let kind = banto_tags::tag_kind(&tag.data_type, tag.string_length, &tag.string_encoding)
        .ok_or(TagExclusionCause::UnknownDataType)?;
    let entry = registry
        .get(conn_protocol)
        .ok_or_else(|| invalid(format!("ドライバ {conn_protocol} がありません")))?;
    let address = entry
        .base()
        .parse_address(tag.address.trim(), &kind)
        .map_err(|err| invalid(err.message().to_string()))?;
    Ok(TagSpec::new(address, kind))
}

#[cfg(test)]
mod tests {
    use super::*;
    use banto_driver_api::DataType;
    use banto_plc::Address;

    /// The built-in drivers (Modbus TCP and SLMP) every plan is read through
    /// since R2b-3a-3.
    fn drivers() -> &'static DriverRegistry {
        static DRIVERS: std::sync::LazyLock<DriverRegistry> =
            std::sync::LazyLock::new(|| banto_drivers::builtin().registry);
        &DRIVERS
    }

    /// The settings a broker-read plan carries (panics on an external plan).
    fn broker_settings(plan: &ConnectionPlan) -> &Settings {
        match &plan.config {
            ProtocolConfig::Broker(broker) => &broker.settings,
            ProtocolConfig::External => panic!("expected a broker-read plan"),
        }
    }
    use banto_tags::{
        CollectionGroupInput, CollectionGroupService, PlcConnectionInput, PlcConnectionService,
        TagInput, TagService,
    };

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
            cpu_type: None,
            database: None,
            username: None,
            secrets: Default::default(),
            settings: None,
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
        let config = build_config(&pool, drivers()).await.unwrap();
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
            build_config(&pool, drivers()).await.unwrap(),
            build_config_from(&snapshot, drivers()).unwrap()
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

        let config = build_config(&pool, drivers()).await.unwrap();
        assert_eq!(config.connections.len(), 1);
        assert_eq!(config.group_count(), 1);
        assert_eq!(config.tag_count(), 2);

        let c = &config.connections[0];
        assert_eq!(c.key, format!("conn:{}", conn.id));
        assert!(!c.simulation, "conn_input defaults to simulation: false");
        let settings = broker_settings(c);
        assert_eq!(
            settings.get(banto_driver_api::keys::HOST),
            Some("127.0.0.1")
        );
        assert_eq!(settings.get(banto_driver_api::keys::PORT), Some("502"));
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

        let config = build_config_from(&snapshot, drivers()).unwrap();
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

        let config = build_config(&pool, drivers()).await.unwrap();
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

        let config = build_config(&pool, drivers()).await.unwrap();
        let group = &config.connections[0].groups[0];
        assert_eq!(group.tags.len(), 2);
        assert!(group.tags[0].scaling.is_some());
        assert_eq!(
            group.requests[0].kind,
            banto_driver_api::TagKind::numeric(DataType::I16)
        );
        assert!(group.tags[1].scaling.is_none());
        assert_eq!(
            group.requests[1].kind,
            banto_driver_api::TagKind::numeric(DataType::Bit)
        );
    }

    /// #532 / #533: the builders without limits (banto-hub's
    /// [`build_config_from`] and [`build_config_lenient_from`]) give **every**
    /// tag empty limits, so banto-hub never judges a threshold;
    /// [`build_config_lenient_with_thresholds`] takes only what the caller
    /// supplies - a tag missing from the map has no limits. Everything other
    /// than the limits is the same plan.
    ///
    /// 反証（2026-10-09 実施）: `ThresholdSource::None` の `for_tag` が
    /// 固定の limits（`h: Some(50.0)`）を返すようにすると、Hub 側の
    /// `is_empty` の assert（1 つ目）で落ちる。
    #[tokio::test]
    async fn only_supplied_thresholds_reach_the_plan() {
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
        let unlisted = tag_svc
            .create(tag_input("Unlisted", group.id, "40001"))
            .await
            .unwrap();
        let plain = tag_svc
            .create(tag_input("Plain", group.id, "40002"))
            .await
            .unwrap();

        let snapshot = RegistrySnapshot::load(&pool).await.unwrap();

        // Without limits (banto-hub): no tag has any.
        let (classic, _) = build_config_lenient_from(&snapshot, drivers());
        assert!(classic.connections[0].groups[0]
            .tags
            .iter()
            .all(|tag| tag.thresholds.is_empty()));
        let strict = build_config_from(&snapshot, drivers()).unwrap();
        assert!(strict.connections[0].groups[0]
            .tags
            .iter()
            .all(|tag| tag.thresholds.is_empty()));

        // Supplied: only the map. `unlisted` is absent -> no limits.
        let supplied = HashMap::from([(
            plain.id,
            Thresholds {
                hh: None,
                h: None,
                l: Some(10.0),
                ll: Some(5.0),
            },
        )]);
        let (config, exclusions) = build_config_lenient_with_thresholds(
            &snapshot,
            &supplied,
            HubSource::Unavailable,
            drivers(),
        );
        assert!(exclusions.is_empty());
        let tags = &config.connections[0].groups[0].tags;
        assert_eq!(tags[0].key, format!("tag:{}", unlisted.id));
        assert!(tags[0].thresholds.is_empty());
        assert_eq!(tags[1].thresholds, supplied[&plain.id]);

        // Same plan apart from the limits.
        assert_eq!(config.store_config, classic.store_config);
        assert_eq!(
            config.connections[0].groups[0].requests,
            classic.connections[0].groups[0].requests
        );
    }

    #[test]
    fn limit_for_names_each_levels_own_limit() {
        use crate::event::ThresholdLevel;
        let t = Thresholds {
            hh: Some(4.0),
            h: Some(3.0),
            l: Some(2.0),
            ll: Some(1.0),
        };
        assert_eq!(t.limit_for(ThresholdLevel::Hh), Some(4.0));
        assert_eq!(t.limit_for(ThresholdLevel::H), Some(3.0));
        assert_eq!(t.limit_for(ThresholdLevel::L), Some(2.0));
        assert_eq!(t.limit_for(ThresholdLevel::Ll), Some(1.0));
        assert_eq!(Thresholds::default().limit_for(ThresholdLevel::H), None);
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

        let config = build_config(&pool, drivers())
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
        let err = build_config(&pool, drivers()).await.unwrap_err();
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
        let err = build_config(&pool, drivers()).await.unwrap_err();
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
        let err = build_config(&pool, drivers()).await.unwrap_err();
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
        let config = build_config(&pool, drivers())
            .await
            .expect("a bit-in-word address on a bit tag must build");
        assert_eq!(config.tag_count(), 1);
        assert_eq!(
            config.connections[0].groups[0].requests[0].kind,
            banto_driver_api::TagKind::numeric(DataType::Bit)
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

        let config = build_config(&pool, drivers())
            .await
            .expect("slmp config should build");
        assert_eq!(config.connections.len(), 1);
        let c = &config.connections[0];
        let settings = broker_settings(c);
        assert_eq!(
            settings.get(banto_driver_api::keys::HOST),
            Some("127.0.0.1")
        );
        assert_eq!(settings.get(banto_driver_api::keys::PORT), Some("5007"));
        assert_eq!(c.groups[0].requests.len(), 1);
    }

    /// P3-b (監査指摘 2026-08-12) / #334: the plan must carry the
    /// connection's own `word_order` (R2b-3a-3: in the settings the broker
    /// hands the driver), not silently fall back to a default.
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

        let config = build_config(&pool, drivers())
            .await
            .expect("slmp config should build");
        assert_eq!(
            broker_settings(&config.connections[0]).get(banto_driver_api::keys::WORD_ORDER),
            Some("high_low"),
            "a connection asking for high_low must not silently get the default"
        );
    }

    /// The default-omitted counterpart: a `"slmp"` connection that never sets
    /// `word_order` (this file's `conn_input_slmp`, unchanged) still gets
    /// `low_high` - the same value every SLMP connection got before this
    /// column existed.
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

        let config = build_config(&pool, drivers())
            .await
            .expect("slmp config should build");
        assert_eq!(
            broker_settings(&config.connections[0]).get(banto_driver_api::keys::WORD_ORDER),
            Some("low_high")
        );
    }

    /// #605: an explicit Q CPU reaches the plan's settings (the SLMP driver
    /// reads `cpu_type` from them; an omitted one is the driver's default R).
    #[tokio::test]
    async fn slmp_plan_carries_cpu_type() {
        let pool = registry().await;
        let mut q_input = conn_input_slmp("QSeries", 5008);
        q_input.cpu_type = Some("Q".to_string());
        let q = PlcConnectionService::new(pool.clone())
            .create(q_input)
            .await
            .unwrap();
        assert_eq!(q.setting_str("cpu_type"), Some("Q"));
        let group = CollectionGroupService::new(pool.clone())
            .create(group_input("G1", q.id, 1_000))
            .await
            .unwrap();
        TagService::new(pool.clone())
            .create(tag_input("T1", group.id, "D100"))
            .await
            .unwrap();

        let config = build_config(&pool, drivers())
            .await
            .expect("slmp config should build");
        assert_eq!(
            broker_settings(&config.connections[0]).get(banto_driver_api::keys::CPU_TYPE),
            Some("Q")
        );
    }

    /// Regression test for the bug fixed 2026-09-08: the plan must carry a
    /// Modbus connection's own `word_order` (both directions), not a default
    /// - before the fix collection polling decoded with the default while
    /// the read-on-demand path honoured the column. Since R2b-3a-3 both read
    /// through the same driver with the same settings.
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

        let config = build_config(&pool, drivers())
            .await
            .expect("modbus config should build");

        let high_low_plan = config
            .connections
            .iter()
            .find(|c| c.key == format!("conn:{}", high_low_conn.id))
            .unwrap();
        assert_eq!(
            broker_settings(high_low_plan).get(banto_driver_api::keys::WORD_ORDER),
            Some("high_low"),
            "a connection asking for high_low must not silently get low_high"
        );

        let low_high_plan = config
            .connections
            .iter()
            .find(|c| c.key == format!("conn:{}", low_high_conn.id))
            .unwrap();
        assert_eq!(
            broker_settings(low_high_plan).get(banto_driver_api::keys::WORD_ORDER),
            Some("low_high"),
            "a connection asking for low_high must not silently get high_low"
        );
    }

    /// T9-1 (docs/ux-plan.md §1): `PlcConnection::simulation` carries through
    /// unchanged into `ConnectionPlan::simulation` - `build_config` itself
    /// never starts a simulator or touches host/port (since R2b-3a-3 the
    /// host's `BrokerSimRegistry` substitutes the address before the broker
    /// session is ensured) - this just pins down that the flag is not
    /// dropped along the way.
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

        let config = build_config(&pool, drivers()).await.unwrap();
        assert_eq!(config.connections.len(), 2);
        let sim_plan = config
            .connections
            .iter()
            .find(|c| c.key == format!("conn:{}", sim_conn.id))
            .expect("simulated connection should be in the config");
        assert!(sim_plan.simulation);
        // build_config never rewrites host/port for a simulated connection -
        // that is the host's BrokerSimRegistry's job (ConnectionPlan's doc).
        assert_eq!(
            broker_settings(sim_plan).get(banto_driver_api::keys::PORT),
            Some("502")
        );
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
        let err = build_config(&pool, drivers()).await.unwrap_err();
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
                cpu_type: None,
                database: None,
                username: None,
                secrets: Default::default(),
                settings: None,
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

        let config = build_config(&pool, drivers())
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
                cpu_type: None,
                database: Some("erp".to_string()),
                username: Some("reader".to_string()),
                secrets: Default::default(),
                settings: None,
            })
            .await
            .unwrap();

        let config = build_config(&pool, drivers())
            .await
            .expect("an enabled postgres connection must not fail config build");
        assert_eq!(
            config.connections.len(),
            1,
            "only the PLC connection should be collected - the postgres one is not a PLC pipeline participant"
        );
        assert_eq!(config.connections[0].key, format!("conn:{}", real_conn.id));
    }

    /// A registry with one PLC connection (`T1` at `40001`) and one `"hub"`
    /// connection whose group holds `HT` (`PLC9.G.Temp`, `f64`), `Gone`
    /// (`PLC9.G.Gone`, `f64`) and `Retyped` (`PLC9.G.Retyped`, `i16`).
    /// Returns `(pool, plc conn id, plc group id, hub conn id, hub group id,
    /// [HT, Gone, Retyped] tag ids)`.
    async fn registry_with_a_hub_connection() -> (SqlitePool, i64, i64, i64, i64, [i64; 3]) {
        let pool = registry().await;
        let plc_svc = PlcConnectionService::new(pool.clone());
        let real_conn = plc_svc.create(conn_input("PLC1", 502)).await.unwrap();
        let group = CollectionGroupService::new(pool.clone())
            .create(group_input("G1", real_conn.id, 1_000))
            .await
            .unwrap();
        let tag_svc = TagService::new(pool.clone());
        tag_svc
            .create(tag_input("T1", group.id, "40001"))
            .await
            .unwrap();

        let mut hub_input = conn_input("Hub", 0);
        hub_input.protocol = banto_tags::HUB_PROTOCOL.to_string();
        let hub = plc_svc.create(hub_input).await.unwrap();
        let mut hub_group_input = group_input("HG", hub.id, 1_000);
        hub_group_input.default_writable = false;
        let hub_group = CollectionGroupService::new(pool.clone())
            .create(hub_group_input)
            .await
            .unwrap();
        let mut ids = [0_i64; 3];
        for (slot, (name, address, data_type)) in [
            ("HT", "PLC9.G.Temp", "f64"),
            ("Gone", "PLC9.G.Gone", "f64"),
            ("Retyped", "PLC9.G.Retyped", "i16"),
        ]
        .into_iter()
        .enumerate()
        {
            let mut input = tag_input(name, hub_group.id, address);
            input.data_type = data_type.to_string();
            ids[slot] = tag_svc.create(input).await.unwrap().id;
        }
        (pool, real_conn.id, group.id, hub.id, hub_group.id, ids)
    }

    /// #383 段階3 P4 (2026-10-11 owner decision): a **disabled** `"hub"`
    /// connection is left out of collection exactly like a disabled PLC
    /// connection - not planned, no exclusion listed (disabling is a choice,
    /// not an error), its tags' external names not sampled - even where the
    /// runtime can collect hub connections and the catalog has every tag.
    #[tokio::test]
    async fn a_disabled_hub_connection_is_not_collected() {
        let (pool, plc_id, _plc_group, hub_id, _hub_group, _ids) =
            registry_with_a_hub_connection().await;
        let plc_svc = PlcConnectionService::new(pool.clone());
        let mut hub_input = conn_input("Hub", 0);
        hub_input.protocol = banto_tags::HUB_PROTOCOL.to_string();
        hub_input.enabled = false;
        plc_svc.update(hub_id, hub_input).await.unwrap();
        let snapshot = RegistrySnapshot::load(&pool).await.unwrap();
        let catalog: HubCatalog = HashMap::from([
            ("PLC9.G.Temp".to_string(), "f64".to_string()),
            ("PLC9.G.Gone".to_string(), "f64".to_string()),
            ("PLC9.G.Retyped".to_string(), "i16".to_string()),
        ]);
        let (config, exclusions) = build_config_lenient_with_thresholds(
            &snapshot,
            &HashMap::new(),
            HubSource::Available {
                catalog: Some(&catalog),
            },
            drivers(),
        );
        assert!(!config.has_external_connections());
        assert!(config.external_names().is_empty());
        assert!(exclusions.is_empty(), "{exclusions:?}");
        let keys: Vec<&str> = config.connections.iter().map(|c| c.key.as_str()).collect();
        assert_eq!(keys, vec![format!("conn:{plc_id}").as_str()]);
    }

    /// #383 段階3 P3: where the runtime cannot collect a `"hub"` connection
    /// (banto-hub, `banto-serve`) it is left out with an explicit reason -
    /// the connection as [`ExclusionReason::HubSourceUnavailable`] (or
    /// [`ExclusionReason::HubNotConfigured`] when no Hub is set up), its
    /// group and tags as cascades - while the PLC connection next to it is
    /// still collected (#414: 外して残りを動かす). The strict build
    /// (banto-hub) fails on it, and the hub tags' external-name addresses
    /// are never handed to the PLC address interpreter.
    #[tokio::test]
    async fn a_hub_connection_is_excluded_where_the_runtime_cannot_collect_it() {
        let (pool, plc_id, plc_group, hub_id, hub_group, ids) =
            registry_with_a_hub_connection().await;
        let snapshot = RegistrySnapshot::load(&pool).await.unwrap();

        for (source, code, message) in [
            (
                HubSource::Unavailable,
                "hubSourceUnavailable",
                "この実行形態では Hub 経由の接続から収集できません",
            ),
            (
                HubSource::NotConfigured,
                "hubNotConfigured",
                "Hub への接続が設定されていないため収集しません（Hub に接続してから収集を再起動してください）",
            ),
        ] {
            let (config, exclusions) =
                build_config_lenient_with_thresholds(&snapshot, &HashMap::new(), source, drivers());
            assert_eq!(config.connections.len(), 1);
            assert_eq!(config.connections[0].key, format!("conn:{plc_id}"));
            assert!(!config.has_external_connections());
            assert_eq!(config.store_config.groups.len(), 1);
            assert_eq!(
                config.store_config.groups[0].key,
                format!("grp:{plc_group}")
            );
            let listed: Vec<(ExclusionUnit, i64, &str)> = exclusions
                .iter()
                .map(|e| (e.unit, e.id, e.reason.code()))
                .collect();
            assert_eq!(
                listed,
                vec![
                    (ExclusionUnit::Connection, hub_id, code),
                    (ExclusionUnit::Group, hub_group, "connectionExcluded"),
                    (ExclusionUnit::Tag, ids[0], "connectionExcluded"),
                    (ExclusionUnit::Tag, ids[1], "connectionExcluded"),
                    (ExclusionUnit::Tag, ids[2], "connectionExcluded"),
                ]
            );
            assert_eq!(exclusions[0].reason.message(), message);
        }

        // banto-hub's builds (strict and its lenient twin): R2b-3a-3 - a `hub`
        // row is the driver check's there (banto-hub registers no `hub` driver).
        let (_, exclusions) = build_config_lenient_from(&snapshot, drivers());
        assert_eq!(
            exclusions[0].reason,
            ExclusionReason::DriverUnavailable {
                protocol: "hub".to_string()
            }
        );
        match build_config_from(&snapshot, drivers()) {
            Err(CollectError::Config(message)) => {
                assert_eq!(
                    message,
                    "接続 Hub のプロトコル hub のドライバがありません（接続は残しています）"
                );
            }
            other => panic!("the strict build must refuse a hub connection, got {other:?}"),
        }
    }

    /// #383 段階3 P3: where the runtime can collect it, a `"hub"` connection
    /// becomes an external plan sampling each tag's external name, in tag
    /// order, with no scaling and no simulation. Against the Hub catalog
    /// (H3), a tag that vanished and one whose data type changed are left
    /// out **alone** with their reasons, the rest of the group and the PLC
    /// connection keep running (#414). Without a catalog nothing is
    /// reconciled. The registry marks (`config_exclusions`) judge only the
    /// registry, so they list nothing for a well-formed hub connection.
    #[tokio::test]
    async fn an_available_hub_is_collected_and_reconciled_against_its_catalog() {
        let (pool, plc_id, _plc_group, hub_id, hub_group, ids) =
            registry_with_a_hub_connection().await;
        let snapshot = RegistrySnapshot::load(&pool).await.unwrap();

        let catalog: HubCatalog = HashMap::from([
            ("PLC9.G.Temp".to_string(), "f64".to_string()),
            ("PLC9.G.Retyped".to_string(), "f32".to_string()),
            ("PLC9.G.Other".to_string(), "bit".to_string()),
        ]);
        let (config, exclusions) = build_config_lenient_with_thresholds(
            &snapshot,
            &HashMap::new(),
            HubSource::Available {
                catalog: Some(&catalog),
            },
            drivers(),
        );
        assert!(config.has_external_connections());
        // P4: the post-reconciliation set - the vanished and retyped tags are
        // not sampled, so they are not in it.
        assert_eq!(config.external_names(), vec!["PLC9.G.Temp".to_string()]);
        let keys: Vec<&str> = config.connections.iter().map(|c| c.key.as_str()).collect();
        let (plc_key, hub_key) = (format!("conn:{plc_id}"), format!("conn:{hub_id}"));
        assert_eq!(keys, vec![plc_key.as_str(), hub_key.as_str()]);
        let hub_plan = &config.connections[1];
        assert_eq!(hub_plan.config, ProtocolConfig::External);
        assert!(!hub_plan.simulation);
        assert_eq!(hub_plan.groups.len(), 1);
        assert_eq!(hub_plan.groups[0].key, format!("grp:{hub_group}"));
        assert!(hub_plan.groups[0].requests.is_empty());
        assert_eq!(hub_plan.groups[0].external_names, vec!["PLC9.G.Temp"]);
        let tag_keys: Vec<&str> = hub_plan.groups[0]
            .tags
            .iter()
            .map(|t| t.key.as_str())
            .collect();
        assert_eq!(tag_keys, vec![format!("tag:{}", ids[0]).as_str()]);
        assert_eq!(hub_plan.groups[0].tags[0].scaling, None);
        // The hub group gets a store group with only the surviving column.
        let hub_store = config
            .store_config
            .groups
            .iter()
            .find(|g| g.key == format!("grp:{hub_group}"))
            .expect("hub store group");
        assert_eq!(hub_store.tags.len(), 1);
        assert_eq!(hub_store.tags[0].data_type, "f64");

        assert_eq!(
            exclusions,
            vec![
                ConfigExclusion {
                    unit: ExclusionUnit::Tag,
                    id: ids[1],
                    key: format!("tag:{}", ids[1]),
                    name: "Gone".to_string(),
                    reason: ExclusionReason::HubTagNotFound {
                        external_name: "PLC9.G.Gone".to_string(),
                    },
                },
                ConfigExclusion {
                    unit: ExclusionUnit::Tag,
                    id: ids[2],
                    key: format!("tag:{}", ids[2]),
                    name: "Retyped".to_string(),
                    reason: ExclusionReason::HubTagTypeChanged {
                        external_name: "PLC9.G.Retyped".to_string(),
                        registered: "i16".to_string(),
                        catalog: "f32".to_string(),
                    },
                },
            ]
        );
        assert_eq!(exclusions[0].reason.code(), "hubTagNotFound");
        assert_eq!(exclusions[1].reason.code(), "hubTagTypeChanged");
        assert_eq!(
            exclusions[1].reason.message(),
            "Hub のタグ PLC9.G.Retyped のデータ型が変わったため収集しません（登録: i16 / Hub: f32）"
        );

        // Without a catalog: every hub tag is collected, nothing reconciled.
        let (config, exclusions) = build_config_lenient_with_thresholds(
            &snapshot,
            &HashMap::new(),
            HubSource::Available { catalog: None },
            drivers(),
        );
        assert!(exclusions.is_empty(), "{exclusions:?}");
        assert_eq!(
            config.connections[1].groups[0].external_names,
            vec!["PLC9.G.Temp", "PLC9.G.Gone", "PLC9.G.Retyped"]
        );
        assert!(config_exclusions(&snapshot, drivers()).is_empty());

        // PR #578 review: when **every** hub tag is excluded, the hub group and
        // connection are not planned at all (nothing to collect through
        // them), so `has_external_connections` is false and the caller can
        // drop its Hub demand. The exclusions still list each tag.
        let retyped_everything: HubCatalog = HashMap::from([
            ("PLC9.G.Temp".to_string(), "i32".to_string()),
            ("PLC9.G.Retyped".to_string(), "f32".to_string()),
        ]);
        let (config, exclusions) = build_config_lenient_with_thresholds(
            &snapshot,
            &HashMap::new(),
            HubSource::Available {
                catalog: Some(&retyped_everything),
            },
            drivers(),
        );
        assert!(!config.has_external_connections());
        assert!(config.external_names().is_empty());
        let keys: Vec<&str> = config.connections.iter().map(|c| c.key.as_str()).collect();
        assert_eq!(keys, vec![plc_key.as_str()]);
        assert!(config
            .store_config
            .groups
            .iter()
            .all(|g| g.key != format!("grp:{hub_group}")));
        let codes: Vec<(i64, &str)> = exclusions.iter().map(|e| (e.id, e.reason.code())).collect();
        assert_eq!(
            codes,
            vec![
                (ids[0], "hubTagTypeChanged"),
                (ids[1], "hubTagNotFound"),
                (ids[2], "hubTagTypeChanged"),
            ]
        );
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

        let a = build_config(&pool, drivers()).await.unwrap();
        let b = build_config(&pool, drivers()).await.unwrap();
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

        let mut config = build_config(&pool, drivers()).await.unwrap();
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

    /// T9-2: `set_broker_dial_target` stamps a broker-read plan with the
    /// session's fingerprint (used purely so `Collector::apply_config`'s
    /// `PartialEq` diff notices a moved session and respawns the
    /// connection's task with a fresh `ClientFactory` - see the method's own
    /// doc comment), makes the plan differ from an unstamped one and from one
    /// stamped differently, leaves a non-matching key untouched, and is a
    /// silent no-op for an absent key. #337: SLMP and Modbus alike.
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

        let unstamped = build_config(&pool, drivers()).await.unwrap();
        let slmp_key = format!("conn:{}", slmp_conn.id);
        let modbus_key = format!("conn:{}", modbus_conn.id);
        assert_eq!(
            unstamped.broker_connection_keys(),
            HashSet::from([slmp_key.clone(), modbus_key.clone()])
        );
        let fingerprint = |port: &str| {
            let mut settings = Settings::new();
            settings.insert(banto_driver_api::keys::PORT, port);
            SettingsFingerprint::of("slmp", &settings, &[])
        };
        let plan = |config: &CollectorConfig, key: &str| {
            config
                .connections
                .iter()
                .find(|c| c.key == key)
                .unwrap()
                .clone()
        };

        let mut config = unstamped.clone();
        config.set_broker_dial_target(&slmp_key, fingerprint("19999"));
        config.set_broker_dial_target(&modbus_key, fingerprint("19998"));
        // An absent key must still be a silent no-op.
        config.set_broker_dial_target("conn:999999", fingerprint("1"));

        assert_ne!(plan(&config, &slmp_key), plan(&unstamped, &slmp_key));
        assert_ne!(
            plan(&config, &modbus_key),
            plan(&unstamped, &modbus_key),
            "#337: a broker-read Modbus plan is stamped just like an SLMP one"
        );

        // The same stamp again: unchanged. Another one: changed.
        let mut again = config.clone();
        again.set_broker_dial_target(&slmp_key, fingerprint("19999"));
        assert_eq!(plan(&again, &slmp_key), plan(&config, &slmp_key));
        again.set_broker_dial_target(&slmp_key, fingerprint("20000"));
        assert_ne!(plan(&again, &slmp_key), plan(&config, &slmp_key));
        assert_eq!(plan(&again, &modbus_key), plan(&config, &modbus_key));
    }

    // --- #414 段階2: the strict builder's wording is frozen ----------------
    //
    // banto-hub's `last_config_error` shows `build_config`'s text verbatim,
    // so the strict path must keep failing on the *same* item with the
    // *same* words after `build_config_from` was rebuilt on top of the
    // lenient builder. These goldens were run against the pre-#414-段階2
    // implementation first (they passed there unchanged). R2b-3a-3: every
    // build reads through the drivers, so a connection's settings and a tag's
    // address are judged by the driver - those cases carry the driver's words
    // (the strict path banto-hub uses already did since R2b-1d / R2b-2c), and
    // a broken setting is made in `settings` (what the broker dials with), not
    // in the flat columns.

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
        match build_config_from(snapshot, drivers()) {
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
            "接続 PLC1 のプロトコル ethernet-ip のドライバがありません（接続は残しています）",
        ),
        (
            "modbus port",
            |s| set_setting(s, "port", 70_000),
            "接続 PLC1 の設定が検証を通りません: settings.port: port は 1〜65535 の整数で指定してください",
        ),
        (
            "modbus unit id",
            |s| set_setting(s, "unit_id", 300),
            "接続 PLC1 の設定が検証を通りません: settings.unit_id: unit_id は 0〜255 の整数で指定してください",
        ),
        (
            "slmp port",
            |s| {
                s.connections[0].protocol = "slmp".to_string();
                s.tags[0].address = "D100".to_string();
                set_setting(s, "port", -1);
            },
            "接続 PLC1 の設定が検証を通りません: settings.port: port は 1〜65535 の整数で指定してください",
        ),
        (
            "group period",
            |s| s.groups[0].period_ms = -5,
            "グループ G1 の period_ms が不正です: -5",
        ),
        (
            "tag address",
            |s| s.tags[0].address = "99999".to_string(),
            "タグ T1 のアドレス 99999 が不正です: \"99999\" は Modbus の参照番号（例: 40001, 40001.3）として読めません",
        ),
        (
            "tag data type",
            |s| s.tags[0].data_type = "f128".to_string(),
            "タグ T1 のデータ型 f128 は未対応です",
        ),
        (
            "bit address on a non-bit tag",
            |s| s.tags[0].address = "40001.3".to_string(),
            "タグ T1 のアドレス 40001.3 が不正です: ビット指定アドレス \"40001.3\" はデータ型 bit \
             のタグでのみ使えます（現在のデータ型: i16）",
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
                set_setting(s, "port", 70_000);
                s.tags[0].data_type = "f128".to_string();
            },
            "接続 PLC1 の設定が検証を通りません: settings.port: port は 1〜65535 の整数で指定してください",
        ),
        (
            // A broken connection fails the strict build even when it has
            // nothing to collect.
            "bad connection without any group",
            |s| {
                set_setting(s, "port", 70_000);
                s.groups.clear();
                s.tags.clear();
            },
            "接続 PLC1 の設定が検証を通りません: settings.port: port は 1〜65535 の整数で指定してください",
        ),
    ];

    /// Break the first connection's stored settings (what the broker dials
    /// with since R2b-3a-3) - the flat columns are no longer read by the plan.
    fn set_setting(snapshot: &mut RegistrySnapshot, key: &str, value: i64) {
        snapshot.connections[0]
            .settings
            .insert(key.to_string(), value.into());
    }

    fn golden_expected(expected: &str) -> String {
        expected.to_string()
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
        set_setting(&mut snapshot, "port", 70_000);
        snapshot.connections[0].enabled = false;
        assert_eq!(
            build_config_from(&snapshot, drivers()).unwrap().tag_count(),
            0
        );

        let mut snapshot = base.clone();
        snapshot.groups[0].period_ms = -5;
        snapshot.groups[0].enabled = false;
        assert_eq!(
            build_config_from(&snapshot, drivers()).unwrap().tag_count(),
            0
        );

        let mut snapshot = base;
        snapshot.tags[0].address = "99999".to_string();
        snapshot.tags[0].enabled = false;
        assert_eq!(
            build_config_from(&snapshot, drivers()).unwrap().tag_count(),
            0
        );
        assert!(config_exclusions(&snapshot, drivers()).is_empty());
    }

    // --- #414 段階2: the lenient builder ---------------------------------

    /// The strict build is "the lenient build, failing on the first
    /// exclusion": for every golden case the lenient build's first exclusion
    /// rebuilds exactly the golden text, and a clean snapshot has none.
    #[tokio::test]
    async fn the_first_exclusion_is_the_item_the_strict_build_fails_on() {
        let base = golden_snapshot().await;
        let (config, exclusions) = build_config_lenient_from(&base, drivers());
        assert!(exclusions.is_empty());
        assert_eq!(config, build_config_from(&base, drivers()).unwrap());

        for (label, break_it, expected) in GOLDEN_CASES {
            let mut snapshot = base.clone();
            break_it(&mut snapshot);
            let (_, exclusions) = build_config_lenient_from(&snapshot, drivers());
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
        snapshot.connections[i]
            .settings
            .insert("port".to_string(), 70_000.into());
        let i = snapshot.groups.iter().position(|g| g.id == g3.id).unwrap();
        snapshot.groups[i].period_ms = -5;
        let i = snapshot.tags.iter().position(|t| t.id == t6.id).unwrap();
        snapshot.tags[i].data_type = "f128".to_string();

        let (config, exclusions) = build_config_lenient_from(&snapshot, drivers());

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
                    "invalidSettings"
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
        match build_config_from(&snapshot, drivers()) {
            Err(CollectError::Config(message)) => {
                assert!(message.starts_with("タグ T2 のアドレス D3000 が不正です: "))
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(config_exclusions(&snapshot, drivers()), exclusions);
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

        let (lenient, exclusions) = build_config_lenient_from(&with_bad, drivers());
        assert_eq!(exclusions.len(), 1);
        assert_eq!(lenient, build_config_from(&without_bad, drivers()).unwrap());
    }

    /// A broken connection with no groups is still listed (the strict build
    /// fails on it too), with nothing cascading from it.
    #[tokio::test]
    async fn a_broken_connection_without_groups_is_listed_alone() {
        let mut snapshot = golden_snapshot().await;
        snapshot.connections[0].protocol = "ethernet-ip".to_string();
        snapshot.groups.clear();
        snapshot.tags.clear();
        let (config, exclusions) = build_config_lenient_from(&snapshot, drivers());
        assert_eq!(config.tag_count(), 0);
        assert_eq!(exclusions.len(), 1);
        assert_eq!(exclusions[0].unit, ExclusionUnit::Connection);
        assert_eq!(exclusions[0].reason.code(), "driverUnavailable");
        assert_eq!(
            exclusions[0].reason.message(),
            "プロトコル ethernet-ip のドライバがこの実行形態にありません（接続は残しています）"
        );
    }

    // ---- R2a-1b: "push" connections (hand-built snapshots) ----------------
    // Written before R2a-2a let `ALLOWED_PROTOCOLS` / the SQL CHECK accept
    // "push"; hand-built snapshots keep these independent of the services.
    // `load_connection_reads_a_push_row_with_settings` goes through them.

    fn push_conn(id: i64, name: &str) -> PlcConnection {
        PlcConnection {
            id,
            name: name.to_string(),
            protocol: PUSH_PROTOCOL.to_string(),
            enabled: true,
            simulation: false,
            settings: Default::default(),
            settings_version: 1,
        }
    }

    fn group_of(id: i64, conn: i64, period_ms: i64) -> CollectionGroup {
        CollectionGroup {
            id,
            name: format!("g{id}"),
            plc_connection_id: conn,
            period_ms,
            enabled: true,
            default_writable: false,
            query_sql: None,
        }
    }

    fn tag_of(id: i64, group: i64, address: &str, data_type: &str) -> Tag {
        Tag {
            id,
            name: format!("t{id}"),
            collection_group_id: group,
            address: address.to_string(),
            data_type: data_type.to_string(),
            string_length: None,
            string_encoding: "utf-8".to_string(),
            raw_lo: None,
            raw_hi: None,
            eng_lo: None,
            eng_hi: None,
            unit: None,
            decimals: 0,
            enabled: true,
            writable: false,
            tag_kind: "plc".to_string(),
            expression: None,
            retain: false,
            revision: 1,
        }
    }

    fn push_snapshot() -> RegistrySnapshot {
        RegistrySnapshot {
            connections: vec![push_conn(7, "mes")],
            groups: vec![group_of(70, 7, 1000)],
            tags: vec![
                tag_of(701, 70, "  line1/temp \t", "f32"),
                tag_of(702, 70, "Line1/Temp", "bit"),
                tag_of(703, 70, "label", "string"),
                tag_of(704, 70, "count", "u64"),
            ],
            secrets: Vec::new(),
        }
    }

    /// R2b-1d (implementation-checklist §5, #605; design §7): both registry readers
    /// see the new columns (`load_connection`'s hand-written SELECT included) and the
    /// same sealed secret rows - read in the same transaction as the connections, and
    /// only sealed (no plaintext field exists on the snapshot).
    #[tokio::test]
    async fn both_registry_readers_see_the_new_columns_and_the_sealed_secrets() {
        let pool = registry().await;
        let service = PlcConnectionService::new(pool.clone());
        let mut pg = PlcConnectionInput {
            protocol: banto_tags::POSTGRES_PROTOCOL.to_string(),
            database: Some("erp".to_string()),
            username: Some("reader".to_string()),
            ..conn_input("PG", 5432)
        };
        pg.secrets.insert(
            banto_tags::PASSWORD_KEY.to_string(),
            banto_tags::SecretChange::Set(banto_tags::SealedSecretRow::new(
                "0123456789abcdef0123456789abcdef",
                banto_tags::SCHEME_PLAIN_V1,
                b"pw".to_vec(),
            )),
        );
        let pg = service.create(pg).await.unwrap();
        service.create(conn_input_slmp("S", 5007)).await.unwrap();

        let loaded = RegistrySnapshot::load(&pool).await.unwrap();
        let mut tx = pool.begin().await.unwrap();
        let in_tx = RegistrySnapshot::load_connection(&mut tx).await.unwrap();
        drop(tx);
        assert_eq!(loaded, in_tx);
        assert_eq!(in_tx.connections.len(), 2);
        assert_eq!(in_tx.connections[0].setting_str("host"), Some("127.0.0.1"));
        assert_eq!(in_tx.connections[0].setting_i64("port"), Some(5432));
        assert_eq!(in_tx.connections[0].setting_str("database"), Some("erp"));
        assert_eq!(in_tx.connections[1].settings_version, 1);
        assert_eq!(in_tx.secrets.len(), 1);
        assert_eq!(in_tx.secrets[0].connection_id, pg.id);
        assert_eq!(
            Some(in_tx.secrets[0].sealed.r#ref.as_str()),
            in_tx.connections[0].secret_ref(banto_tags::PASSWORD_KEY)
        );
    }

    /// PR #777 owner review P2: [`RegistrySnapshot::load`] reads the connections and
    /// the secret rows in one read transaction. A password update committed (from
    /// another pool connection, a file DB in WAL) between the connection read and the
    /// secret rows read is not seen half: the snapshot keeps the old reference **and**
    /// the old row, so DB Source does not read a healthy connection as `Missing`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn load_reads_connections_and_secret_rows_in_one_snapshot() {
        const OLD: &str = "0123456789abcdef0123456789abcdef";
        const NEW: &str = "fedcba9876543210fedcba9876543210";
        fn with_password(r#ref: &str) -> PlcConnectionInput {
            let mut pg = PlcConnectionInput {
                protocol: banto_tags::POSTGRES_PROTOCOL.to_string(),
                database: Some("erp".to_string()),
                username: Some("reader".to_string()),
                ..conn_input("PG", 5432)
            };
            pg.secrets.insert(
                banto_tags::PASSWORD_KEY.to_string(),
                banto_tags::SecretChange::Set(banto_tags::SealedSecretRow::new(
                    r#ref,
                    banto_tags::SCHEME_PLAIN_V1,
                    b"pw".to_vec(),
                )),
            );
            pg
        }
        let dir = tempfile::tempdir().unwrap();
        let pool = banto_storage::connect_sqlite(dir.path().join("snapshot.sqlite3"))
            .await
            .unwrap();
        banto_tags::migrate(&pool).await.unwrap();
        let service = PlcConnectionService::new(pool.clone());
        let id = service.create(with_password(OLD)).await.unwrap().id;

        let writer = service.clone();
        let snapshot = RegistrySnapshot::load_between(&pool, || async move {
            writer
                .update(id, with_password(NEW))
                .await
                .expect("the update commits while the snapshot is being read");
        })
        .await
        .unwrap();
        assert_eq!(
            snapshot.connections[0].secret_ref(banto_tags::PASSWORD_KEY),
            Some(OLD)
        );
        assert_eq!(snapshot.secrets.len(), 1, "{:?}", snapshot.secrets);
        assert_eq!(snapshot.secrets[0].sealed.r#ref, OLD);

        let after = RegistrySnapshot::load(&pool).await.unwrap();
        assert_eq!(
            after.connections[0].secret_ref(banto_tags::PASSWORD_KEY),
            Some(NEW)
        );
        assert_eq!(after.secrets[0].sealed.r#ref, NEW);
        pool.close().await;
    }

    /// R2a-2a: a "push" row saved through the services (now that the CHECK
    /// accepts it) reads back through both registry readers with its
    /// `settings` - `load_connection`'s hand-written SELECT names the new
    /// column (implementation-checklist §5, #605) - and plans as an external
    /// connection when the runtime can receive pushes.
    #[tokio::test]
    async fn load_connection_reads_a_push_row_with_settings() {
        let pool = registry().await;
        let conn = PlcConnectionService::new(pool.clone())
            .create(PlcConnectionInput {
                protocol: PUSH_PROTOCOL.to_string(),
                ..conn_input("Bridge1", 0)
            })
            .await
            .unwrap();
        // (This crate has no serde_json of its own; the column is JSON text.)
        sqlx::query(
            "UPDATE plc_connections SET settings = '{\"stale_timeout_ms\":2000}' WHERE id = ?",
        )
        .bind(conn.id)
        .execute(&pool)
        .await
        .unwrap();
        let group = CollectionGroupService::new(pool.clone())
            .create(group_input("G1", conn.id, 1_000))
            .await
            .unwrap();
        TagService::new(pool.clone())
            .create(tag_input("T1", group.id, " line1.temp "))
            .await
            .unwrap();

        let loaded = RegistrySnapshot::load(&pool).await.unwrap();
        let mut tx = pool.begin().await.unwrap();
        let in_tx = RegistrySnapshot::load_connection(&mut tx).await.unwrap();
        drop(tx);
        assert_eq!(loaded.connections, in_tx.connections);
        assert_eq!(
            in_tx.connections[0].settings.get("stale_timeout_ms"),
            Some(&2000.into())
        );

        let config =
            build_config_from_with_registry(&in_tx, PushSource::Available, push_drivers()).unwrap();
        assert_eq!(config.connections[0].config, ProtocolConfig::External);
        assert_eq!(config.external_names(), vec!["line1.temp".to_string()]);
        // ChronoGazer's (and today's Hub's) strict build still refuses it.
        assert!(build_config_from(&in_tx, drivers()).is_err());
    }

    #[test]
    fn push_available_plans_an_external_connection_with_trimmed_keys() {
        let snapshot = push_snapshot();
        let config =
            build_config_from_with_registry(&snapshot, PushSource::Available, push_drivers())
                .unwrap();
        assert_eq!(config.connections.len(), 1);
        let conn = &config.connections[0];
        assert_eq!(conn.key, "conn:7");
        assert_eq!(conn.config, ProtocolConfig::External);
        assert!(!conn.simulation);
        assert!(config.has_external_connections());
        let group = &conn.groups[0];
        assert!(group.requests.is_empty());
        // Trimmed, case preserved, string tag (703) skipped, in tag-id order.
        assert_eq!(
            group.external_names,
            vec!["line1/temp", "Line1/Temp", "count"]
        );
        assert_eq!(
            config.external_names(),
            vec!["Line1/Temp", "count", "line1/temp"]
        );
        let plan_keys: Vec<&str> = group.tags.iter().map(|t| t.key.as_str()).collect();
        assert_eq!(plan_keys, vec!["tag:701", "tag:702", "tag:704"]);
        assert!(group.tags.iter().all(|t| t.scaling.is_none()));
        assert_eq!(config.store_config.groups.len(), 1);
        assert_eq!(config.store_config.groups[0].tags.len(), 3);
    }

    #[test]
    fn push_scaling_on_the_row_is_never_applied() {
        let mut snapshot = push_snapshot();
        snapshot.tags[0].raw_lo = Some(0.0);
        snapshot.tags[0].raw_hi = Some(100.0);
        snapshot.tags[0].eng_lo = Some(0.0);
        snapshot.tags[0].eng_hi = Some(1000.0);
        let config =
            build_config_from_with_registry(&snapshot, PushSource::Available, push_drivers())
                .unwrap();
        assert!(config.connections[0].groups[0].tags[0].scaling.is_none());
    }

    #[test]
    fn push_unparseable_data_type_is_an_invalid_tag_and_the_rest_collects() {
        let mut snapshot = push_snapshot();
        snapshot.tags.push(tag_of(705, 70, "weird", "complex128"));
        let (config, exclusions) = build_config_lenient_inner(
            &snapshot,
            ThresholdSource::None,
            None,
            PushSource::Available,
            push_drivers(),
        );
        let listed: Vec<(ExclusionUnit, i64, &str)> = exclusions
            .iter()
            .map(|e| (e.unit, e.id, e.reason.code()))
            .collect();
        assert_eq!(listed, vec![(ExclusionUnit::Tag, 705, "unknownDataType")]);
        assert_eq!(config.connections[0].groups[0].external_names.len(), 3);
        // The strict build fails with the tag's wording.
        let err = build_config_from_with_registry(&snapshot, PushSource::Available, push_drivers())
            .unwrap_err();
        assert!(err.to_string().contains("complex128"), "{err}");
    }

    #[test]
    fn push_group_without_a_collectable_tag_is_not_planned() {
        for tags in [
            vec![],
            vec![tag_of(1, 70, "only-string", "string")],
            vec![tag_of(1, 70, "bad", "complex128")],
        ] {
            let snapshot = RegistrySnapshot {
                connections: vec![push_conn(7, "mes")],
                groups: vec![group_of(70, 7, 1000)],
                tags,
                secrets: Vec::new(),
            };
            let (config, _) = build_config_lenient_inner(
                &snapshot,
                ThresholdSource::None,
                None,
                PushSource::Available,
                push_drivers(),
            );
            assert!(config.connections.is_empty());
            assert!(config.store_config.groups.is_empty());
            assert!(!config.has_external_connections());
        }
    }

    #[test]
    fn push_unavailable_excludes_the_connection_with_cascades() {
        let snapshot = push_snapshot();
        let (config, exclusions) = build_config_lenient_from(&snapshot, push_drivers());
        assert!(config.connections.is_empty());
        assert!(config.store_config.groups.is_empty());
        let listed: Vec<(ExclusionUnit, i64, &str)> = exclusions
            .iter()
            .map(|e| (e.unit, e.id, e.reason.code()))
            .collect();
        assert_eq!(
            listed,
            vec![
                (ExclusionUnit::Connection, 7, "pushSourceUnavailable"),
                (ExclusionUnit::Group, 70, "connectionExcluded"),
                (ExclusionUnit::Tag, 701, "connectionExcluded"),
                (ExclusionUnit::Tag, 702, "connectionExcluded"),
                (ExclusionUnit::Tag, 704, "connectionExcluded"),
            ]
        );
        assert_eq!(
            exclusions[0].reason.message(),
            "この実行形態では push 接続から収集できません"
        );
        // Strict wording, and `build_config_from` == `..._with(Unavailable)`.
        let expected = "接続 mes は push 接続です。この実行形態では push 接続から収集できません";
        for err in [
            build_config_from(&snapshot, push_drivers()).unwrap_err(),
            build_config_from_with_registry(&snapshot, PushSource::Unavailable, push_drivers())
                .unwrap_err(),
        ] {
            assert!(
                matches!(&err, CollectError::Config(m) if m == expected),
                "{err}"
            );
        }
    }

    /// The regression R2a-1b must prevent: ChronoGazer's entry points (the
    /// lenient builders and the registry-editor marks) never plan a `push`
    /// connection, so its Hub-demand (`external_names`) cannot pick up a
    /// push key.
    #[test]
    fn existing_entry_points_never_plan_a_push_connection() {
        let mut snapshot = push_snapshot();
        // A hub-less PLC next to it, to show the rest still runs.
        snapshot.connections.push(PlcConnection {
            protocol: "modbus-tcp".to_string(),
            // R2b-3a-3: the plan reads `settings` (what the broker dials with).
            settings: serde_json::json!({"host": "127.0.0.1", "port": 502})
                .as_object()
                .unwrap()
                .clone(),
            ..push_conn(8, "plc")
        });
        snapshot.groups.push(group_of(80, 8, 1000));
        snapshot.tags.push(tag_of(801, 80, "40001", "u16"));

        let mut configs = vec![
            build_config_lenient_from(&snapshot, drivers()).0,
            build_config_lenient_with_thresholds(
                &snapshot,
                &HashMap::new(),
                HubSource::Unavailable,
                drivers(),
            )
            .0,
            build_config_lenient_with_thresholds(
                &snapshot,
                &HashMap::new(),
                HubSource::Available { catalog: None },
                drivers(),
            )
            .0,
            build_config_lenient_with_thresholds(
                &snapshot,
                &HashMap::new(),
                HubSource::NotConfigured,
                drivers(),
            )
            .0,
        ];
        configs.push(
            build_config_from_with_registry(&snapshot, PushSource::Available, push_drivers())
                .unwrap(),
        );
        let (last, rest) = configs.split_last().unwrap();
        for config in rest {
            let keys: Vec<&str> = config.connections.iter().map(|c| c.key.as_str()).collect();
            assert_eq!(keys, vec!["conn:8"]);
            assert!(!config.has_external_connections());
            assert!(config.external_names().is_empty());
        }
        // ... whereas the explicit opt-in does (so the assertions above bite).
        assert!(last.has_external_connections());
        // The editor marks flag the push connection too (never silently drop).
        assert!(config_exclusions(&snapshot, drivers())
            .iter()
            .any(|e| e.reason.code() == "pushSourceUnavailable"));
    }

    // ---- R2b-1d: §4.4 exclusion reasons (a driver registry) ---------------

    /// A minimal driver for the registry: refuses `port == "1"`, holds `secret_keys`.
    struct FakeDriver {
        descriptor: banto_driver_api::DriverDescriptor,
        secret_keys: Vec<String>,
    }

    impl banto_driver_api::DriverBase for FakeDriver {
        fn descriptor(&self) -> &banto_driver_api::DriverDescriptor {
            &self.descriptor
        }
        fn validate_config(
            &self,
            settings: &banto_driver_api::Settings,
        ) -> Vec<banto_driver_api::FieldError> {
            if settings.get("port") == Some("1") {
                vec![banto_driver_api::FieldError::new("port", "driver says no")]
            } else {
                Vec::new()
            }
        }
        fn secret_keys(&self) -> Vec<String> {
            self.secret_keys.clone()
        }
        /// R2b-2c: a stand-in that reads an address the way the real drivers do
        /// (`banto_plc`'s grammar of the driver's own id, canonical = `Display`), so
        /// the registry-carrying plan has something faithful to ask. Any other id
        /// (a driver the crate has no notation for, R2b-3a-3) takes the text as is.
        fn parse_address(
            &self,
            text: &str,
            _kind: &banto_driver_api::TagKind,
        ) -> Result<banto_driver_api::DriverAddress, banto_driver_api::AddressError> {
            let id = &self.descriptor.id;
            let parsed = match id.as_str() {
                "modbus-tcp" => Address::parse(text),
                "slmp" => Address::parse_slmp(text),
                _ => {
                    return Ok(banto_driver_api::DriverAddress::new(
                        id.clone(),
                        text,
                        "area",
                        true,
                        1,
                    ))
                }
            }
            .map_err(|err| banto_driver_api::AddressError::new(err.to_string()))?;
            Ok(banto_driver_api::DriverAddress::new(
                id.clone(),
                parsed.to_string(),
                "area",
                true,
                1,
            ))
        }
        fn offset_address(
            &self,
            canonical: &str,
            _kind: &banto_driver_api::TagKind,
            _n: i64,
        ) -> Result<String, banto_driver_api::AddressError> {
            Ok(canonical.to_owned())
        }
        fn address_help(&self) -> String {
            String::new()
        }
    }

    /// R2b-3a-3: the plan reads only through session-type (broker-managed)
    /// drivers, so the fake is one too. The plan never dials; a connect is a
    /// refusal.
    impl banto_driver_api::SessionDriver for FakeDriver {
        fn connect(
            &self,
            _settings: &Settings,
            _cx: banto_driver_api::ConnectContext,
        ) -> banto_driver_api::BoxFuture<
            'static,
            Result<Box<dyn banto_driver_api::Session>, banto_driver_api::FatalError>,
        > {
            Box::pin(async {
                Err(banto_driver_api::FatalError::Io(
                    "the fake driver never dials".to_string(),
                ))
            })
        }
    }

    fn fake_driver(id: &str, secret_keys: &[&str]) -> FakeDriver {
        FakeDriver {
            descriptor: banto_driver_api::DriverDescriptor::new(
                banto_driver_api::DriverId::new(id).unwrap(),
                "0.0.0",
                id,
                banto_driver_api::Capabilities::READ_ONLY,
            ),
            secret_keys: secret_keys.iter().map(|k| k.to_string()).collect(),
        }
    }

    /// A session-type fake per id, except `push` (host-provided, as in
    /// banto-hub's registry).
    fn fake_registry(drivers: &[(&str, &[&str])]) -> DriverRegistry {
        let mut registry = DriverRegistry::new();
        for (id, secret_keys) in drivers {
            let driver = std::sync::Arc::new(fake_driver(id, secret_keys));
            let entry = if *id == PUSH_PROTOCOL {
                banto_driver_api::DriverEntry::host_provided(driver)
            } else {
                banto_driver_api::DriverEntry::session(driver)
            };
            registry.register(entry).unwrap();
        }
        registry
    }

    /// The built-in drivers plus a host-provided `push` (banto-hub's
    /// registry has one; the built-ins do not), for the push plans.
    fn push_drivers() -> &'static DriverRegistry {
        static DRIVERS: std::sync::LazyLock<DriverRegistry> = std::sync::LazyLock::new(|| {
            let mut registry = banto_drivers::builtin().registry;
            registry
                .register(banto_driver_api::DriverEntry::host_provided(
                    std::sync::Arc::new(fake_driver(PUSH_PROTOCOL, &[])),
                ))
                .unwrap();
            registry
        });
        &DRIVERS
    }

    fn modbus_conn(id: i64, name: &str, settings: serde_json::Value) -> PlcConnection {
        PlcConnection::from_settings(
            id,
            name.to_string(),
            "modbus-tcp".to_string(),
            true,
            false,
            settings.as_object().unwrap().clone(),
            1,
        )
    }

    /// R2b-2c (docs/banto-hub-driver-address-design.md §4.2・§9): with a registry the
    /// plan reads each address through the connection's driver. A hand-edited row
    /// (`D3000` on a Modbus tag) leaves **that tag alone** out as `InvalidTag` carrying
    /// the driver's message; the others stay planned. 5-digit (pre-R2b-2) and 6-digit
    /// (canonical) spellings both plan. The registry-free build is unchanged.
    #[test]
    fn a_registry_plan_excludes_only_the_tag_its_driver_refuses() {
        let registry = banto_drivers::builtin().registry;
        let conn = modbus_conn(1, "c", serde_json::json!({"host": "h", "port": 502}));
        let snapshot = RegistrySnapshot {
            connections: vec![conn],
            groups: vec![group_of(10, 1, 1000)],
            tags: vec![
                tag_of(100, 10, "D3000", "u16"),
                tag_of(101, 10, "40001", "u16"),
                tag_of(102, 10, "400001", "u16"),
                tag_of(103, 10, "40001.3", "u16"),
            ],
            secrets: Vec::new(),
        };
        let (config, exclusions) = build_config_lenient_inner(
            &snapshot,
            ThresholdSource::None,
            None,
            PushSource::Available,
            &registry,
        );
        let tag_exclusions: Vec<(i64, &str)> = exclusions
            .iter()
            .filter(|e| e.unit == ExclusionUnit::Tag)
            .map(|e| (e.id, e.reason.code()))
            .collect();
        // 100: the driver refuses `D3000`; 103: a bit address on a non-bit
        // tag - the driver's rule since R2b-2a, so its refusal too.
        assert_eq!(
            tag_exclusions,
            vec![(100, "invalidAddress"), (103, "invalidAddress")]
        );
        let message = exclusions
            .iter()
            .find(|e| e.id == 100)
            .map(|e| e.reason.message())
            .unwrap();
        assert!(message.contains("Modbus"), "{message}");
        let group = &config.connections[0].groups[0];
        assert_eq!(group.requests.len(), 2, "40001 and 400001 are both planned");
    }

    /// Design §4.4・§7: with a registry, a connection whose driver is missing, whose
    /// settings the driver (or the storage-layer shape check) refuses, or whose
    /// settings version is newer than the driver's is excluded **alone** - its groups
    /// and tags cascade, every other connection is planned, the row itself is untouched.
    /// The strict build fails with that connection's wording; without a registry
    /// (ChronoGazer) nothing changes.
    #[test]
    fn a_registry_excludes_only_the_connection_it_cannot_drive() {
        let registry = fake_registry(&[("modbus-tcp", &[]), ("push", &[])]);
        let good = modbus_conn(1, "good", serde_json::json!({"host": "h", "port": 502}));
        let refused = modbus_conn(2, "refused", serde_json::json!({"host": "h", "port": 1}));
        let wrong_type = modbus_conn(
            3,
            "wrong-type",
            serde_json::json!({"host": "h", "port": "502"}),
        );
        let mut too_new = modbus_conn(4, "too-new", serde_json::json!({"host": "h", "port": 502}));
        too_new.settings_version = 2;
        let mut no_driver = modbus_conn(5, "no-driver", serde_json::json!({}));
        no_driver.protocol = "slmp".to_string();
        let mut hub = modbus_conn(6, "hub", serde_json::json!({}));
        hub.protocol = banto_tags::HUB_PROTOCOL.to_string();
        let connections = vec![good, refused, wrong_type, too_new, no_driver, hub];
        let groups: Vec<CollectionGroup> = (1..=6).map(|id| group_of(id * 10, id, 1000)).collect();
        let tags: Vec<Tag> = (1..=6)
            .map(|id| tag_of(id * 100, id * 10, "40001", "u16"))
            .collect();
        let snapshot = RegistrySnapshot {
            connections,
            groups,
            tags,
            secrets: Vec::new(),
        };

        let (config, exclusions) = build_config_lenient_inner(
            &snapshot,
            ThresholdSource::None,
            None,
            PushSource::Available,
            &registry,
        );
        let planned: Vec<&str> = config.connections.iter().map(|c| c.key.as_str()).collect();
        assert_eq!(planned, vec!["conn:1"]);
        let own: Vec<(i64, &str)> = exclusions
            .iter()
            .filter(|e| e.unit == ExclusionUnit::Connection)
            .map(|e| (e.id, e.reason.code()))
            .collect();
        assert_eq!(
            own,
            vec![
                (2, "invalidSettings"),
                (3, "invalidSettings"),
                (4, "settingsVersionTooNew"),
                (5, "driverUnavailable"),
                (6, "driverUnavailable"),
            ]
        );
        // Each excluded connection takes its group and tag along as cascades.
        assert_eq!(
            exclusions
                .iter()
                .filter(|e| e.reason.code() == "connectionExcluded")
                .count(),
            10
        );
        let reason_of = |id: i64| {
            exclusions
                .iter()
                .find(|e| e.unit == ExclusionUnit::Connection && e.id == id)
                .unwrap()
                .reason
                .clone()
        };
        assert_eq!(
            reason_of(2),
            ExclusionReason::InvalidSettings {
                errors: vec!["settings.port: driver says no".to_string()]
            }
        );
        assert!(matches!(
            reason_of(3),
            ExclusionReason::InvalidSettings { errors } if errors[0].starts_with("port: ")
        ));
        assert_eq!(
            reason_of(4),
            ExclusionReason::SettingsVersionTooNew {
                stored: 2,
                supported: 1
            }
        );
        assert!(reason_of(5).message().contains("slmp"));

        // The strict build (banto-hub) fails on the first one, by name.
        let err = build_config_from_with_registry(&snapshot, PushSource::Available, &registry)
            .unwrap_err();
        assert!(err.to_string().contains("接続 refused の設定"), "{err}");
        // A snapshot of only good rows builds.
        let only_good = RegistrySnapshot {
            connections: vec![snapshot.connections[0].clone()],
            groups: vec![snapshot.groups[0].clone()],
            tags: vec![snapshot.tags[0].clone()],
            secrets: Vec::new(),
        };
        assert!(
            build_config_from_with_registry(&only_good, PushSource::Available, &registry).is_ok()
        );
    }

    /// R2b-3a-3 (design §4.3): whether a connection is planned is the registry's
    /// driver kind, not a protocol-name table. A session-type driver under a new id
    /// is planned (read through the broker); a host-provided one is not
    /// (`unsupportedProtocol`, as before); and a `simulation = true` connection of
    /// a session-type driver is planned like any other (R2b-5b: the session is
    /// the value layer's `SimDevice`, so no protocol needs its own simulator).
    #[test]
    fn the_plan_follows_the_driver_kind_and_plans_a_simulation_of_any_session_driver() {
        let mut registry = fake_registry(&[("fake-session", &[])]);
        registry
            .register(banto_driver_api::DriverEntry::host_provided(
                std::sync::Arc::new(fake_driver("fake-provided", &[])),
            ))
            .unwrap();
        let mut session = modbus_conn(1, "session", serde_json::json!({}));
        session.protocol = "fake-session".to_string();
        let mut provided = modbus_conn(2, "provided", serde_json::json!({}));
        provided.protocol = "fake-provided".to_string();
        let mut simulated = session.clone();
        simulated.id = 3;
        simulated.name = "simulated".to_string();
        simulated.simulation = true;
        let snapshot = RegistrySnapshot {
            connections: vec![session, provided, simulated],
            groups: (1..=3).map(|id| group_of(id * 10, id, 1000)).collect(),
            tags: (1..=3)
                .map(|id| tag_of(id * 100, id * 10, "X1", "u16"))
                .collect(),
            secrets: Vec::new(),
        };

        let (config, exclusions) = build_config_lenient_inner(
            &snapshot,
            ThresholdSource::None,
            None,
            PushSource::Unavailable,
            &registry,
        );
        let planned: Vec<&str> = config.connections.iter().map(|c| c.key.as_str()).collect();
        assert_eq!(planned, vec!["conn:1", "conn:3"]);
        assert_eq!(
            config.broker_connection_keys(),
            ["conn:1".to_string(), "conn:3".to_string()]
                .into_iter()
                .collect()
        );
        assert_eq!(config.connections[0].groups[0].requests.len(), 1);
        let own: Vec<(i64, &str)> = exclusions
            .iter()
            .filter(|e| e.unit == ExclusionUnit::Connection)
            .map(|e| (e.id, e.reason.code()))
            .collect();
        assert_eq!(own, vec![(2, "unsupportedProtocol")]);
    }

    /// A secret key holding a plaintext (a hand-edited row) is an invalid setting -
    /// and the message names the field only, never the value.
    #[test]
    fn a_plaintext_under_a_secret_key_is_invalid_settings_without_the_value() {
        let registry = fake_registry(&[("modbus-tcp", &["password"])]);
        let conn = modbus_conn(
            1,
            "m",
            serde_json::json!({"host": "h", "port": 502, "password": "TOPSECRET"}),
        );
        let reason = driver_exclusion(&conn, &registry).unwrap();
        assert_eq!(reason.code(), "invalidSettings");
        assert!(
            !reason.message().contains("TOPSECRET"),
            "{}",
            reason.message()
        );
        assert!(!format!("{reason:?}").contains("TOPSECRET"));
    }
}
