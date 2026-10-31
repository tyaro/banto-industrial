//! Hub-compatible read-only DTOs. Wire names intentionally follow the
//! machine-facing snake_case contract.

use std::fmt;
use std::sync::Arc;

use serde::de::Deserializer;
use serde::{Deserialize, Serialize, Serializer};

use crate::binding::{BindingResolution, ResolvedBinding, UnresolvedBinding};

/// Stable Hub identity represented on the wire as `ids: [i64, i64, i64]`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct StableTagId {
    pub connection_id: i64,
    pub group_id: i64,
    pub tag_id: i64,
}

impl StableTagId {
    pub const fn new(connection_id: i64, group_id: i64, tag_id: i64) -> Self {
        Self {
            connection_id,
            group_id,
            tag_id,
        }
    }

    pub const fn as_array(self) -> [i64; 3] {
        [self.connection_id, self.group_id, self.tag_id]
    }
}

impl Serialize for StableTagId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.as_array().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for StableTagId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let [connection_id, group_id, tag_id] = <[i64; 3]>::deserialize(deserializer)?;
        Ok(Self::new(connection_id, group_id, tag_id))
    }
}

/// The Hub collection generation mode.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum CollectionMode {
    Configured,
    AllSimulation,
    Unknown(String),
}

impl CollectionMode {
    pub fn parse(raw: impl Into<String>) -> Self {
        match raw.into().as_str() {
            "configured" => Self::Configured,
            "all_simulation" => Self::AllSimulation,
            raw => Self::Unknown(raw.to_owned()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Configured => "configured",
            Self::AllSimulation => "all_simulation",
            Self::Unknown(raw) => raw,
        }
    }
}

impl Serialize for CollectionMode {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for CollectionMode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer).map(Self::parse)
    }
}

/// The authoritative source classification carried by Hub values.
///
/// `Computed` and `Db` were added 2026-09-14（#335 / 既存ギャップ対応）:
/// `Computed`は banto-hub の `value_source_for_tag`が computed タグの既定
/// ラベルとして返すようになった値（`derived_simulation`は真に
/// シミュレーション中のときだけの情報ラベルに変わった）。`Db`は外部 DB
/// 連携（2026-09-06 オーナー決定）で hub が既に送っていたが、この enum に
/// 対応する variant が無く`Unknown("db")`へ落ちていたギャップを埋めた。
/// `Unknown(raw)`は今後もフォールバックとして残す - 将来 hub 側に新しい
/// ラベルが増えても、このクレートを更新するまでは`Unknown`で読める。
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum ValueSource {
    Real,
    Simulation,
    Computed,
    DerivedSimulation,
    Internal,
    Db,
    Unknown(String),
}

impl ValueSource {
    pub fn parse(raw: impl Into<String>) -> Self {
        match raw.into().as_str() {
            "real" => Self::Real,
            "simulation" => Self::Simulation,
            "computed" => Self::Computed,
            "derived_simulation" => Self::DerivedSimulation,
            "internal" => Self::Internal,
            "db" => Self::Db,
            raw => Self::Unknown(raw.to_owned()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Real => "real",
            Self::Simulation => "simulation",
            Self::Computed => "computed",
            Self::DerivedSimulation => "derived_simulation",
            Self::Internal => "internal",
            Self::Db => "db",
            Self::Unknown(raw) => raw,
        }
    }
}

impl Serialize for ValueSource {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ValueSource {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer).map(Self::parse)
    }
}

/// Hub value quality. Unknown values remain unknown instead of becoming Good.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum ValueQuality {
    Good,
    Stale,
    Bad,
    Unknown(String),
}

impl ValueQuality {
    pub fn parse(raw: impl Into<String>) -> Self {
        match raw.into().as_str() {
            "good" => Self::Good,
            "stale" => Self::Stale,
            "bad" => Self::Bad,
            raw => Self::Unknown(raw.to_owned()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Good => "good",
            Self::Stale => "stale",
            Self::Bad => "bad",
            Self::Unknown(raw) => raw,
        }
    }
}

impl Serialize for ValueQuality {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ValueQuality {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer).map(Self::parse)
    }
}

/// One catalog row returned by `GET /api/v1/tags`.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct CatalogTag {
    pub external_name: String,
    pub tag_key: String,
    pub ids: StableTagId,
    pub connection: String,
    pub group: String,
    pub name: String,
    pub address: String,
    pub data_type: String,
    pub unit: Option<String>,
    pub decimals: i64,
    pub period_ms: i64,
    pub enabled: bool,
    pub writable: bool,
    pub tag_kind: String,
    pub expression: Option<String>,
    pub retain: bool,
    pub simulation: bool,
    pub configured_simulation: bool,
    pub effective_simulation: bool,
    pub value_source: ValueSource,
}

/// Catalog response metadata and rows.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct CatalogSnapshot {
    pub revision: u64,
    pub run_id: Option<u64>,
    pub collection_mode: CollectionMode,
    pub tags: Vec<CatalogTag>,
}

/// One value row. Its field names are the exact Hub wire names `tag`, `v`,
/// `q`, `t`, and `value_source`.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ValueEntry {
    pub tag: String,
    pub v: Option<f64>,
    pub q: ValueQuality,
    pub t: i64,
    pub value_source: ValueSource,
}

/// Values response metadata and rows. The response timestamp is the wire
/// field `t`; each row carries its own source timestamp in its own `t`.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ValuesSnapshot {
    pub revision: u64,
    pub t: i64,
    pub run_id: Option<u64>,
    pub collection_mode: CollectionMode,
    pub values: Vec<ValueEntry>,
}

/// Public connection state for the future streaming client. A current value
/// is only exposed while the state is `Live`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TagClientConnectionState {
    Stopped,
    Connecting,
    Handshaking,
    Live,
    Rebinding,
    Reconnecting,
    Unauthorized,
}

impl fmt::Display for TagClientConnectionState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Stopped => "stopped",
            Self::Connecting => "connecting",
            Self::Handshaking => "handshaking",
            Self::Live => "live",
            Self::Rebinding => "rebinding",
            Self::Reconnecting => "reconnecting",
            Self::Unauthorized => "unauthorized",
        };
        f.write_str(name)
    }
}

/// The catalog, binding resolution, and values that one SDK generation
/// produced together (#682, docs/banto-tagclient-partial-binding-design.md
/// §4.1 / §6).
///
/// All three parts come from the same attempt (R1): when `values` is present
/// its `revision`, `run_id`, and `collection_mode` equal the catalog's
/// (`PublishGate::finalize` already enforces this). Each part is its own
/// [`Arc`] so cloning a [`TagClientState`] only bumps reference counts (R6)
/// and a caller can keep the catalog alone for as long as it likes.
#[derive(Clone, Debug, PartialEq)]
pub struct GenerationSnapshot {
    /// SDK-assigned generation number (R2). It advances on every attempt
    /// that fetched a catalog, including attempts that later failed the
    /// publish gate, so observers only see "greater than before", not "+1".
    /// The counter is process-wide, so it is unique across handles and
    /// survives `restart` and a fresh `start` after `shutdown`.
    pub generation: u64,
    /// The catalog this generation resolved its bindings against.
    pub catalog: Arc<CatalogSnapshot>,
    /// The resolution of every requested binding against `catalog`;
    /// `resolved` plus `unresolved` is the whole request set.
    pub resolution: Arc<BindingResolution>,
    /// `Some` while `Live`.
    pub values: Option<Arc<ValuesSnapshot>>,
}

impl GenerationSnapshot {
    /// The catalog revision (equal to `values.revision` when values exist).
    pub fn revision(&self) -> u64 {
        self.catalog.revision
    }

    /// The catalog run id (equal to `values.run_id` when values exist).
    pub fn run_id(&self) -> Option<u64> {
        self.catalog.run_id
    }

    /// The catalog collection mode (equal to `values.collection_mode` when
    /// values exist).
    pub fn collection_mode(&self) -> &CollectionMode {
        &self.catalog.collection_mode
    }

    /// Look up one application `binding_key` in this generation. A resolved
    /// key returns its catalog row and, while `Live`, its value (R4: always
    /// `Some` while `Live`). A key that was never requested returns `None`.
    ///
    /// The lookup is a linear scan of the resolution, catalog, and values.
    pub fn binding(&self, binding_key: &str) -> Option<BindingView<'_>> {
        if let Some(binding) = self
            .resolution
            .resolved
            .iter()
            .find(|binding| binding.binding_key == binding_key)
        {
            let tag = self
                .catalog
                .tags
                .iter()
                .find(|tag| tag.external_name == binding.external_name)?;
            let value = self.values.as_deref().and_then(|values| {
                values
                    .values
                    .iter()
                    .find(|value| value.tag == binding.external_name)
            });
            return Some(BindingView::Resolved {
                binding,
                tag,
                value,
            });
        }
        self.resolution
            .unresolved
            .iter()
            .find(|binding| binding.binding_key == binding_key)
            .map(BindingView::Unresolved)
    }

    /// Test-only generation whose catalog metadata and resolution are derived
    /// from `values` (one resolved binding per value row, keyed by its tag).
    #[cfg(test)]
    pub(crate) fn for_values(generation: u64, values: ValuesSnapshot) -> Self {
        let resolved = values
            .values
            .iter()
            .map(|value| ResolvedBinding {
                binding_key: value.tag.clone(),
                external_name: value.tag.clone(),
                tag_key: value.tag.clone(),
            })
            .collect();
        Self {
            generation,
            catalog: Arc::new(CatalogSnapshot {
                revision: values.revision,
                run_id: values.run_id,
                collection_mode: values.collection_mode.clone(),
                tags: Vec::new(),
            }),
            resolution: Arc::new(BindingResolution {
                resolved,
                unresolved: Vec::new(),
            }),
            values: Some(Arc::new(values)),
        }
    }
}

/// How one `binding_key` looks in a [`GenerationSnapshot`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BindingView<'a> {
    /// The name exists in this generation's catalog.
    Resolved {
        binding: &'a ResolvedBinding,
        /// The catalog row. Its `value_source` / `effective_simulation` are
        /// informational labels; `value.value_source` is authoritative (R7).
        tag: &'a CatalogTag,
        /// Always `Some` while `Live` (R4).
        value: Option<&'a ValueEntry>,
    },
    /// The name is absent from this generation's catalog.
    Unresolved(&'a UnresolvedBinding),
}

/// Current public state. Non-live states must not retain a snapshot (R3).
#[derive(Clone, Debug, PartialEq)]
pub struct TagClientState {
    state: TagClientConnectionState,
    snapshot: Option<Arc<GenerationSnapshot>>,
    last_error: Option<crate::error::ErrorKind>,
}

#[allow(
    dead_code,
    reason = "TagClientState mutation is wired by the S3a handle and worker slices"
)]
impl TagClientState {
    pub(crate) const fn new(state: TagClientConnectionState) -> Self {
        Self {
            state,
            snapshot: None,
            last_error: None,
        }
    }

    pub const fn connection_state(&self) -> TagClientConnectionState {
        self.state
    }

    /// The generation snapshot; `Some` only while `Live`.
    pub fn snapshot(&self) -> Option<&Arc<GenerationSnapshot>> {
        self.snapshot.as_ref()
    }

    /// The current values; `Some` only while `Live`. Same as
    /// `snapshot().and_then(|s| s.values.as_deref())`.
    pub fn current(&self) -> Option<&ValuesSnapshot> {
        self.snapshot
            .as_deref()
            .and_then(|snapshot| snapshot.values.as_deref())
    }

    pub const fn last_error(&self) -> Option<crate::error::ErrorKind> {
        self.last_error
    }

    pub(crate) const fn reconnecting(error: crate::error::ErrorKind) -> Self {
        Self {
            state: TagClientConnectionState::Reconnecting,
            snapshot: None,
            last_error: Some(error),
        }
    }

    pub(crate) const fn unauthorized() -> Self {
        Self::unauthorized_because(crate::error::ErrorKind::Unauthorized)
    }

    /// `Unauthorized` with the reason the credential stopped working
    /// (#446): plain [`ErrorKind::Unauthorized`](crate::error::ErrorKind::Unauthorized)
    /// for a 401/403, or one of the close-1008 kinds
    /// ([`ErrorKind::is_credential_rejection`](crate::error::ErrorKind::is_credential_rejection)).
    pub(crate) const fn unauthorized_because(error: crate::error::ErrorKind) -> Self {
        Self {
            state: TagClientConnectionState::Unauthorized,
            snapshot: None,
            last_error: Some(error),
        }
    }

    pub(crate) const fn rebinding(error: crate::error::ErrorKind) -> Self {
        Self {
            state: TagClientConnectionState::Rebinding,
            snapshot: None,
            last_error: Some(error),
        }
    }

    pub(crate) fn transition(&mut self, state: TagClientConnectionState) {
        self.state = state;
        self.last_error = None;
        if state != TagClientConnectionState::Live {
            self.snapshot = None;
        }
    }

    pub(crate) fn publish(&mut self, snapshot: GenerationSnapshot) {
        self.state = TagClientConnectionState::Live;
        self.snapshot = Some(Arc::new(snapshot));
        self.last_error = None;
    }

    pub(crate) fn fail(&mut self, error: crate::error::ErrorKind) {
        self.state = TagClientConnectionState::Stopped;
        self.snapshot = None;
        self.last_error = Some(error);
    }
}

impl fmt::Display for TagClientState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.state.fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_id_is_a_three_integer_array_on_the_wire() {
        let id = StableTagId::new(7, 8, 9);
        assert_eq!(serde_json::to_string(&id).unwrap(), "[7,8,9]");
        assert_eq!(serde_json::from_str::<StableTagId>("[7,8,9]").unwrap(), id);
    }

    #[test]
    fn unknown_variants_round_trip_without_fail_open() {
        assert_eq!(CollectionMode::parse("future_mode").as_str(), "future_mode");
        assert_eq!(
            ValueSource::parse("future_source").as_str(),
            "future_source"
        );
        assert_eq!(
            ValueQuality::parse("future_quality").as_str(),
            "future_quality"
        );

        let value: ValueEntry = serde_json::from_str(
            r#"{"tag":"tag-a","v":null,"q":"future_quality","t":17,"value_source":"future_source"}"#,
        )
        .unwrap();
        assert!(matches!(value.q, ValueQuality::Unknown(ref raw) if raw == "future_quality"));
        assert!(
            matches!(value.value_source, ValueSource::Unknown(ref raw) if raw == "future_source")
        );
    }

    /// #335（2026-09-14）: `computed`は computed タグの既定 `value_source`
    /// ラベル、`db`は外部 DB 連携（2026-09-06 オーナー決定）由来のタグの
    /// ラベル - どちらも以前は`Unknown(raw)`へ落ちていた既存ギャップ。
    #[test]
    fn computed_and_db_value_sources_parse_to_dedicated_variants() {
        assert_eq!(ValueSource::parse("computed"), ValueSource::Computed);
        assert_eq!(ValueSource::parse("computed").as_str(), "computed");
        assert_eq!(ValueSource::parse("db"), ValueSource::Db);
        assert_eq!(ValueSource::parse("db").as_str(), "db");

        let value: ValueEntry = serde_json::from_str(
            r#"{"tag":"tag-a","v":1.0,"q":"good","t":17,"value_source":"computed"}"#,
        )
        .unwrap();
        assert_eq!(value.value_source, ValueSource::Computed);

        let value: ValueEntry = serde_json::from_str(
            r#"{"tag":"tag-b","v":2.0,"q":"good","t":18,"value_source":"db"}"#,
        )
        .unwrap();
        assert_eq!(value.value_source, ValueSource::Db);
    }

    #[test]
    fn dto_metadata_and_catalog_fields_are_retained() {
        let input = r#"{
          "revision": 12,
          "run_id": 34,
          "collection_mode": "configured",
          "tags": [{
            "external_name": "tag-a", "tag_key": "tag:9", "ids": [1,2,3],
            "connection": "connection-a", "group": "group-a", "name": "name-a",
            "address": "address-a", "data_type": "f64", "unit": "unit-a",
            "decimals": 2, "period_ms": 100, "enabled": true, "writable": false,
            "tag_kind": "plc", "expression": null, "retain": false,
            "simulation": false, "configured_simulation": false,
            "effective_simulation": false, "value_source": "real"
          }]
        }"#;
        let snapshot: CatalogSnapshot = serde_json::from_str(input).unwrap();
        assert_eq!(snapshot.revision, 12);
        assert_eq!(snapshot.run_id, Some(34));
        assert_eq!(snapshot.tags[0].ids, StableTagId::new(1, 2, 3));
        assert_eq!(snapshot.tags[0].decimals, 2);
    }

    #[test]
    fn values_snapshot_retains_response_and_value_metadata() {
        let input = r#"{
          "revision": 12, "t": 1000, "run_id": null,
          "collection_mode": "all_simulation",
          "values": [{"tag":"tag-a","v":1.5,"q":"stale","t":999,"value_source":"simulation"}]
        }"#;
        let snapshot: ValuesSnapshot = serde_json::from_str(input).unwrap();
        assert_eq!(snapshot.revision, 12);
        assert_eq!(snapshot.t, 1000);
        assert_eq!(snapshot.run_id, None);
        assert_eq!(snapshot.collection_mode, CollectionMode::AllSimulation);
        assert_eq!(snapshot.values[0].v, Some(1.5));
        assert_eq!(snapshot.values[0].t, 999);
        assert_eq!(snapshot.values[0].q, ValueQuality::Stale);
        assert_eq!(snapshot.values[0].value_source, ValueSource::Simulation);
    }

    fn catalog_tag(name: &str) -> CatalogTag {
        CatalogTag {
            external_name: name.into(),
            tag_key: format!("key:{name}"),
            ids: StableTagId::new(1, 1, 1),
            connection: "connection".into(),
            group: "group".into(),
            name: name.into(),
            address: "address".into(),
            data_type: "f64".into(),
            unit: None,
            decimals: 0,
            period_ms: 100,
            enabled: true,
            writable: false,
            tag_kind: "tag".into(),
            expression: None,
            retain: false,
            simulation: false,
            configured_simulation: false,
            effective_simulation: false,
            value_source: ValueSource::Real,
        }
    }

    /// One `Live` generation: `present` resolves to `tag-a`, `missing` does not.
    fn live_generation() -> GenerationSnapshot {
        GenerationSnapshot {
            generation: 3,
            catalog: Arc::new(CatalogSnapshot {
                revision: 5,
                run_id: Some(9),
                collection_mode: CollectionMode::Configured,
                tags: vec![catalog_tag("tag-a"), catalog_tag("tag-b")],
            }),
            resolution: Arc::new(BindingResolution {
                resolved: vec![ResolvedBinding {
                    binding_key: "present".into(),
                    external_name: "tag-a".into(),
                    tag_key: "key:tag-a".into(),
                }],
                unresolved: vec![UnresolvedBinding {
                    binding_key: "missing".into(),
                    external_name: "tag-x".into(),
                    reason: crate::binding::UnresolvedReason::NotInCatalog,
                }],
            }),
            values: Some(Arc::new(ValuesSnapshot {
                revision: 5,
                t: 100,
                run_id: Some(9),
                collection_mode: CollectionMode::Configured,
                values: vec![ValueEntry {
                    tag: "tag-a".into(),
                    v: Some(1.5),
                    q: ValueQuality::Good,
                    t: 99,
                    value_source: ValueSource::Real,
                }],
            })),
        }
    }

    #[test]
    fn generation_snapshot_binding_returns_live_value_unresolved_reason_or_none() {
        let snapshot = live_generation();
        assert_eq!(snapshot.revision(), 5);
        assert_eq!(snapshot.run_id(), Some(9));
        assert_eq!(snapshot.collection_mode(), &CollectionMode::Configured);
        match snapshot.binding("present") {
            Some(BindingView::Resolved {
                binding,
                tag,
                value,
            }) => {
                assert_eq!(binding.external_name, "tag-a");
                assert_eq!(tag.external_name, "tag-a");
                // R4: a resolved binding of a Live generation always has a value.
                assert_eq!(value.unwrap().v, Some(1.5));
            }
            other => panic!("expected resolved, got {other:?}"),
        }
        match snapshot.binding("missing") {
            Some(BindingView::Unresolved(unresolved)) => {
                assert_eq!(unresolved.external_name, "tag-x");
                assert_eq!(
                    unresolved.reason,
                    crate::binding::UnresolvedReason::NotInCatalog
                );
            }
            other => panic!("expected unresolved, got {other:?}"),
        }
        assert_eq!(snapshot.binding("never-requested"), None);
    }

    #[test]
    fn cloning_public_state_shares_the_generation_arcs() {
        let mut state = TagClientState::new(TagClientConnectionState::Connecting);
        state.publish(live_generation());
        let cloned = state.clone();
        let original = state.snapshot().unwrap();
        let copy = cloned.snapshot().unwrap();
        // R6: a clone only bumps reference counts.
        assert!(Arc::ptr_eq(original, copy));
        assert!(Arc::ptr_eq(&original.catalog, &copy.catalog));
        assert!(Arc::ptr_eq(&original.resolution, &copy.resolution));
        assert!(Arc::ptr_eq(
            original.values.as_ref().unwrap(),
            copy.values.as_ref().unwrap()
        ));
        assert_eq!(
            cloned.current().map(|values| values.revision),
            Some(original.catalog.revision)
        );
    }

    #[test]
    fn non_live_public_states_carry_no_snapshot() {
        use crate::error::ErrorKind;
        // R3: every non-Live constructor and `fail` drop the snapshot.
        let states = [
            TagClientState::rebinding(ErrorKind::RevisionMismatch),
            TagClientState::reconnecting(ErrorKind::Transport),
            TagClientState::unauthorized(),
            TagClientState::unauthorized_because(ErrorKind::Unauthorized),
            TagClientState::new(TagClientConnectionState::Stopped),
        ];
        for state in states {
            assert_eq!(state.snapshot(), None);
            assert_eq!(state.current(), None);
        }
        let mut failed = TagClientState::new(TagClientConnectionState::Connecting);
        failed.publish(live_generation());
        failed.fail(ErrorKind::ProtocolError);
        assert_eq!(failed.snapshot(), None);
        assert_eq!(failed.current(), None);
    }
}
