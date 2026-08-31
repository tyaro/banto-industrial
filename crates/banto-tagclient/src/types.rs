//! Hub-compatible read-only DTOs. Wire names intentionally follow the
//! machine-facing snake_case contract.

use std::fmt;

use serde::de::Deserializer;
use serde::{Deserialize, Serialize, Serializer};

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
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum ValueSource {
    Real,
    Simulation,
    DerivedSimulation,
    Internal,
    Unknown(String),
}

impl ValueSource {
    pub fn parse(raw: impl Into<String>) -> Self {
        match raw.into().as_str() {
            "real" => Self::Real,
            "simulation" => Self::Simulation,
            "derived_simulation" => Self::DerivedSimulation,
            "internal" => Self::Internal,
            raw => Self::Unknown(raw.to_owned()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Real => "real",
            Self::Simulation => "simulation",
            Self::DerivedSimulation => "derived_simulation",
            Self::Internal => "internal",
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

/// Current public state. Non-live states must not retain a current snapshot.
#[derive(Clone, Debug, PartialEq)]
pub struct TagClientState {
    state: TagClientConnectionState,
    current: Option<ValuesSnapshot>,
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
            current: None,
            last_error: None,
        }
    }

    pub const fn connection_state(&self) -> TagClientConnectionState {
        self.state
    }

    pub fn current(&self) -> Option<&ValuesSnapshot> {
        self.current.as_ref()
    }

    pub const fn last_error(&self) -> Option<crate::error::ErrorKind> {
        self.last_error
    }

    pub(crate) const fn reconnecting(error: crate::error::ErrorKind) -> Self {
        Self {
            state: TagClientConnectionState::Reconnecting,
            current: None,
            last_error: Some(error),
        }
    }

    pub(crate) const fn unauthorized() -> Self {
        Self {
            state: TagClientConnectionState::Unauthorized,
            current: None,
            last_error: Some(crate::error::ErrorKind::Unauthorized),
        }
    }

    pub(crate) const fn rebinding(error: crate::error::ErrorKind) -> Self {
        Self {
            state: TagClientConnectionState::Rebinding,
            current: None,
            last_error: Some(error),
        }
    }

    pub(crate) fn transition(&mut self, state: TagClientConnectionState) {
        self.state = state;
        self.last_error = None;
        if state != TagClientConnectionState::Live {
            self.current = None;
        }
    }

    pub(crate) fn publish(&mut self, snapshot: ValuesSnapshot) {
        self.state = TagClientConnectionState::Live;
        self.current = Some(snapshot);
        self.last_error = None;
    }

    pub(crate) fn fail(&mut self, error: crate::error::ErrorKind) {
        self.state = TagClientConnectionState::Stopped;
        self.current = None;
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
}
