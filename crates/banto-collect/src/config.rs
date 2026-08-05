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

use std::collections::HashMap;
use std::time::Duration;

use banto_core::ListParams;
use banto_plc::{Address, DataType, ModbusTcpConfig, ReadRequest, SlmpConfig, WordOrder};
use banto_tags::{
    scaling::Scaling, CollectionGroup, CollectionGroupService, PlcConnection, PlcConnectionService,
    Tag, TagService,
};
use banto_tstore::{GroupConfig, StoreConfig, TagColumn};
use sqlx::SqlitePool;

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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Protocol {
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
#[derive(Debug, Clone)]
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
#[derive(Debug, Clone)]
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
#[derive(Debug, Clone)]
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
#[derive(Debug, Clone)]
pub(crate) struct ConnectionPlan {
    pub key: String,
    pub config: ProtocolConfig,
    pub groups: Vec<GroupPlan>,
}

/// The immutable configuration snapshot [`crate::Collector::start`] consumes.
/// Opaque by design: build it with [`build_config`] and hand it to `start`;
/// its internals (the per-connection plans and the derived tstore
/// [`StoreConfig`]) are this crate's concern.
#[derive(Debug, Clone)]
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
    // Pagination `None` returns every row (banto-storage list_query appends no
    // LIMIT), so one unpaginated list per resource is the whole registry.
    let connections = PlcConnectionService::new(pool.clone())
        .list(ListParams::default())
        .await?
        .rows;
    let groups = CollectionGroupService::new(pool.clone())
        .list(ListParams::default())
        .await?
        .rows;
    let tags = TagService::new(pool.clone())
        .list(ListParams::default())
        .await?
        .rows;

    // Index groups by connection and tags by group, so each connection's plan
    // is one pass rather than repeated full scans.
    let mut groups_by_connection: HashMap<i64, Vec<&CollectionGroup>> = HashMap::new();
    for group in &groups {
        if group.enabled {
            groups_by_connection
                .entry(group.plc_connection_id)
                .or_default()
                .push(group);
        }
    }
    let mut tags_by_group: HashMap<i64, Vec<&Tag>> = HashMap::new();
    for tag in &tags {
        if tag.enabled {
            tags_by_group
                .entry(tag.collection_group_id)
                .or_default()
                .push(tag);
        }
    }

    // Deterministic order everywhere (by id) so the derived StoreConfig - and
    // therefore its config hash - is stable across restarts.
    let mut enabled_connections: Vec<&PlcConnection> =
        connections.iter().filter(|c| c.enabled).collect();
    enabled_connections.sort_by_key(|c| c.id);

    let mut connection_plans = Vec::new();
    let mut store_groups = Vec::new();

    for conn in enabled_connections {
        let protocol = parse_protocol(&conn.protocol, &conn.name)?;
        let client_config = match protocol {
            Protocol::ModbusTcp => ProtocolConfig::ModbusTcp(modbus_config_for(conn)?),
            Protocol::Slmp => ProtocolConfig::Slmp(slmp_config_for(conn)?),
        };

        let mut conn_groups = groups_by_connection.remove(&conn.id).unwrap_or_default();
        conn_groups.sort_by_key(|g| g.id);

        let mut group_plans = Vec::new();
        let mut conn_store_groups = Vec::new();
        for group in conn_groups {
            let mut group_tags = tags_by_group.remove(&group.id).unwrap_or_default();
            group_tags.sort_by_key(|t| t.id);

            let mut requests = Vec::with_capacity(group_tags.len());
            let mut tag_plans = Vec::with_capacity(group_tags.len());
            let mut store_columns = Vec::with_capacity(group_tags.len());

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
                let request = build_request(tag, protocol)?;
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

            let period_ms = u32::try_from(group.period_ms).map_err(|_| {
                CollectError::Config(format!(
                    "グループ {} の period_ms が不正です: {}",
                    group.name, group.period_ms
                ))
            })?;

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
        });
    }

    Ok(CollectorConfig {
        connections: connection_plans,
        store_config: StoreConfig {
            groups: store_groups,
        },
    })
}

fn parse_protocol(protocol: &str, conn_name: &str) -> Result<Protocol, CollectError> {
    match protocol {
        "modbus-tcp" => Ok(Protocol::ModbusTcp),
        "slmp" => Ok(Protocol::Slmp),
        other => Err(CollectError::Config(format!(
            "接続 {conn_name} のプロトコル {other} は未対応です（modbus-tcp / slmp のみ対応）"
        ))),
    }
}

fn modbus_config_for(conn: &PlcConnection) -> Result<ModbusTcpConfig, CollectError> {
    let port = u16::try_from(conn.port).map_err(|_| {
        CollectError::Config(format!(
            "接続 {} のポート番号が不正です: {}",
            conn.name, conn.port
        ))
    })?;
    let unit_id = u8::try_from(conn.unit_id).map_err(|_| {
        CollectError::Config(format!(
            "接続 {} のユニットIDが不正です: {}",
            conn.name, conn.unit_id
        ))
    })?;
    Ok(ModbusTcpConfig {
        host: conn.host.clone(),
        port,
        unit_id,
        // Word order is not a registry field (banto-tags has no column for
        // it); v1 uses the Modbus/IEEE default. Revisit if a device profile
        // ever needs per-connection word order.
        word_order: WordOrder::default(),
        ..ModbusTcpConfig::default()
    })
}

/// I8 (2026-08-05): build an [`SlmpConfig`] from a `"slmp"`-protocol
/// [`PlcConnection`] row. Only `host`/`port` come from the registry -
/// `banto-tags::PlcConnection` has no columns for SLMP's CPU series, access
/// route (network/PC/IO/area id), or word order (`unit_id` is Modbus-only and
/// is not read here), so every other field falls back to
/// [`SlmpConfig::default`] (R series CPU, the CPU-on-the-other-end access
/// route, MELSEC's low-word-first order). **Known limitation**: a device
/// that needs a non-default CPU series or a routed (not-directly-connected)
/// access route cannot be configured through the registry today - adding
/// those would need new `plc_connections` columns, deliberately out of scope
/// for I8 (task instructions: "新しい I1 列は追加しない").
/// `connect_timeout`/`response_timeout` are overridden uniformly by
/// [`crate::collector::Collector::start`] from [`crate::collector::CollectorOptions`],
/// exactly like [`modbus_config_for`]'s.
fn slmp_config_for(conn: &PlcConnection) -> Result<SlmpConfig, CollectError> {
    let port = u16::try_from(conn.port).map_err(|_| {
        CollectError::Config(format!(
            "接続 {} のポート番号が不正です: {}",
            conn.name, conn.port
        ))
    })?;
    Ok(SlmpConfig {
        host: conn.host.clone(),
        port,
        ..SlmpConfig::default()
    })
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
fn build_request(tag: &Tag, protocol: Protocol) -> Result<ReadRequest, CollectError> {
    let address = match protocol {
        Protocol::ModbusTcp => Address::parse(&tag.address),
        Protocol::Slmp => Address::parse_slmp(&tag.address),
    }
    .map_err(|err| {
        CollectError::Config(format!(
            "タグ {} のアドレス {} が不正です: {err}",
            tag.name, tag.address
        ))
    })?;
    let data_type = DataType::parse(&tag.data_type).ok_or_else(|| {
        CollectError::Config(format!(
            "タグ {} のデータ型 {} は未対応です",
            tag.name, tag.data_type
        ))
    })?;
    Ok(ReadRequest { address, data_type })
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
        }
    }

    fn group_input(name: &str, conn_id: i64, period_ms: i64) -> CollectionGroupInput {
        CollectionGroupInput {
            name: name.to_string(),
            plc_connection_id: conn_id,
            period_ms,
            enabled: true,
        }
    }

    fn tag_input(name: &str, group_id: i64, address: &str) -> TagInput {
        TagInput {
            name: name.to_string(),
            collection_group_id: group_id,
            address: address.to_string(),
            data_type: "i16".to_string(),
            string_length: None,
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
}
