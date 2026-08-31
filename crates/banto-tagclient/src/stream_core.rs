//! Network-free S2a Hub wire parsing and race-free publish gate.
//!
//! This module deliberately has no socket, async task, WebSocket client,
//! worker, reconnect, or secret handling. See `docs/banto-tagclient-design.md`
//! §5-§9 and the S2a implementation slice.

use std::collections::{HashMap, HashSet};

use serde::Deserialize;
use serde_json::Value;

use crate::error::{Error, ErrorKind, Result};
use crate::types::{CatalogSnapshot, CollectionMode, ValueQuality, ValuesSnapshot};

/// Parsed value from a Hub `data` frame.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct HubWireValue {
    pub(crate) tag: String,
    pub(crate) v: Option<f64>,
    pub(crate) q: ValueQuality,
    pub(crate) t: i64,
}

/// Values-bearing Hub wire message. Event and pong messages are intentionally
/// represented as `Ignored` because they do not affect the value gate.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum HubWireMessage {
    Data { values: Vec<HubWireValue> },
    ConfigChanged,
    Ignored,
}

fn protocol_error() -> Error {
    Error::new(ErrorKind::ProtocolError)
}

/// Parse and validate one Hub wire message against the active subscription and
/// explicit allowed tag set. Unknown tags and subscription IDs fail closed.
pub(crate) fn parse_hub_wire(
    raw: &str,
    subscription_id: i64,
    allowed_tags: &HashSet<String>,
) -> Result<HubWireMessage> {
    let object: Value = serde_json::from_str(raw).map_err(|_| protocol_error())?;
    let op = object
        .get("op")
        .and_then(Value::as_str)
        .ok_or_else(protocol_error)?;
    match op {
        "data" => {
            let wire: WireData = serde_json::from_value(object).map_err(|_| protocol_error())?;
            if wire.id != subscription_id {
                return Err(protocol_error());
            }
            let mut seen = HashSet::with_capacity(wire.values.len());
            let values = wire
                .values
                .into_iter()
                .map(|value| {
                    if !allowed_tags.contains(&value.tag) || !seen.insert(value.tag.clone()) {
                        return Err(protocol_error());
                    }
                    Ok(HubWireValue {
                        tag: value.tag,
                        v: value.v,
                        q: ValueQuality::parse(value.q),
                        t: value.t,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(HubWireMessage::Data { values })
        }
        "config_changed" => {
            let _: WireConfigChanged =
                serde_json::from_value(object).map_err(|_| protocol_error())?;
            Ok(HubWireMessage::ConfigChanged)
        }
        "error" => {
            let wire: WireError = serde_json::from_value(object).map_err(|_| protocol_error())?;
            if wire.code == "unknown_tag" && wire.id == Some(subscription_id) {
                Err(Error::new(ErrorKind::BindingUnresolved))
            } else {
                Err(protocol_error())
            }
        }
        "event" | "pong" => Ok(HubWireMessage::Ignored),
        _ => Err(protocol_error()),
    }
}

#[derive(Deserialize)]
struct WireData {
    id: i64,
    #[serde(rename = "t")]
    _timestamp: i64,
    values: Vec<WireValue>,
}

#[derive(Deserialize)]
struct WireValue {
    tag: String,
    v: Option<f64>,
    q: String,
    t: i64,
}

#[derive(Deserialize)]
struct WireConfigChanged {
    #[serde(rename = "revision")]
    _revision: u64,
}

#[derive(Deserialize)]
struct WireError {
    id: Option<i64>,
    code: String,
}

#[derive(Clone, Debug)]
struct PendingValue {
    value: HubWireValue,
    sequence: u64,
}

/// A bounded, single-owner publish gate for one explicit subscription.
#[derive(Debug)]
pub(crate) struct PublishGate {
    subscription_id: i64,
    allowed_tags: HashSet<String>,
    pending: HashMap<String, PendingValue>,
    sequence: u64,
    rest_snapshot: Option<ValuesSnapshot>,
    rest_marker: Option<u64>,
    invalid_reason: Option<ErrorKind>,
}

impl PublishGate {
    /// Construct a gate. Empty, blank, comma-containing, or duplicate tags are
    /// rejected before any transport can be involved.
    pub(crate) fn new<I, S>(subscription_id: i64, tags: I) -> Result<Self>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut allowed_tags = HashSet::new();
        for tag in tags {
            let tag = tag.into();
            if tag.trim().is_empty() || tag.contains(',') || !allowed_tags.insert(tag) {
                return Err(Error::new(ErrorKind::InvalidTagSelection));
            }
        }
        if allowed_tags.is_empty() {
            return Err(Error::new(ErrorKind::InvalidTagSelection));
        }
        Ok(Self {
            subscription_id,
            allowed_tags,
            pending: HashMap::new(),
            sequence: 0,
            rest_snapshot: None,
            rest_marker: None,
            invalid_reason: None,
        })
    }

    #[cfg(test)]
    fn pending_len(&self) -> usize {
        self.pending.len()
    }

    pub(crate) fn accept_wire(&mut self, raw: &str) -> Result<()> {
        if let Some(reason) = self.invalid_reason {
            return Err(Error::new(reason));
        }
        let message = match parse_hub_wire(raw, self.subscription_id, &self.allowed_tags) {
            Ok(message) => message,
            Err(error) => {
                self.invalid_reason = Some(error.kind());
                return Err(error);
            }
        };
        match message {
            HubWireMessage::Data { values } => self.accept_data(&values),
            HubWireMessage::ConfigChanged => {
                self.invalid_reason = Some(ErrorKind::RevisionMismatch);
                Err(Error::new(ErrorKind::RevisionMismatch))
            }
            HubWireMessage::Ignored => Ok(()),
        }
    }

    fn accept_data(&mut self, values: &[HubWireValue]) -> Result<()> {
        if let Some(reason) = self.invalid_reason {
            return Err(Error::new(reason));
        }
        if values.len() as u64 > u64::MAX - self.sequence {
            self.invalid_reason = Some(ErrorKind::ProtocolError);
            return Err(protocol_error());
        }
        if values
            .iter()
            .any(|value| !self.allowed_tags.contains(&value.tag))
        {
            self.invalid_reason = Some(ErrorKind::ProtocolError);
            return Err(protocol_error());
        }
        for value in values {
            self.sequence += 1;
            let entry = PendingValue {
                value: value.clone(),
                sequence: self.sequence,
            };
            let replace = self
                .pending
                .get(&value.tag)
                .map(|old| (value.t, self.sequence) >= (old.value.t, old.sequence))
                .unwrap_or(true);
            if replace {
                self.pending.insert(value.tag.clone(), entry);
            }
        }
        Ok(())
    }

    pub(crate) fn record_rest_snapshot(&mut self, snapshot: ValuesSnapshot) -> Result<()> {
        if let Some(reason) = self.invalid_reason {
            return Err(Error::new(reason));
        }
        if self.rest_snapshot.is_some() {
            self.invalid_reason = Some(ErrorKind::ProtocolError);
            return Err(protocol_error());
        }
        self.rest_marker = Some(self.sequence);
        self.rest_snapshot = Some(snapshot);
        Ok(())
    }

    /// Validate metadata and explicit values, then atomically overlay newer WS
    /// entries. No partial or pre-gate current snapshot is ever returned.
    pub(crate) fn finalize(self, catalog: &CatalogSnapshot) -> Result<ValuesSnapshot> {
        if let Some(reason) = self.invalid_reason {
            return Err(Error::new(reason));
        }
        if self.rest_marker.is_none() {
            return Err(Error::new(ErrorKind::RevisionMismatch));
        }
        let rest = self.rest_snapshot.as_ref().ok_or_else(protocol_error)?;
        if catalog.revision != rest.revision
            || catalog.run_id != rest.run_id
            || catalog.collection_mode != rest.collection_mode
        {
            return Err(Error::new(if catalog.revision != rest.revision {
                ErrorKind::RevisionMismatch
            } else {
                ErrorKind::RuntimeMetadataMismatch
            }));
        }
        if matches!(catalog.collection_mode, CollectionMode::Unknown(_)) {
            return Err(Error::new(ErrorKind::RuntimeMetadataMismatch));
        }
        let mut rest_by_tag = HashMap::with_capacity(rest.values.len());
        for value in &rest.values {
            if !self.allowed_tags.contains(&value.tag)
                || rest_by_tag.insert(value.tag.clone(), value).is_some()
            {
                return Err(protocol_error());
            }
        }
        if rest_by_tag.len() != self.allowed_tags.len() {
            return Err(protocol_error());
        }

        let marker = self.rest_marker.ok_or_else(protocol_error)?;
        let mut values = rest.values.clone();
        for value in &mut values {
            if let Some(pending) = self.pending.get(&value.tag) {
                if pending.value.t > value.t
                    || (pending.value.t == value.t && pending.sequence > marker)
                {
                    value.v = pending.value.v;
                    value.q = pending.value.q.clone();
                    value.t = pending.value.t;
                }
            }
        }
        let result = ValuesSnapshot {
            revision: rest.revision,
            t: rest.t,
            run_id: rest.run_id,
            collection_mode: rest.collection_mode.clone(),
            values,
        };
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ValueEntry, ValueSource};

    fn catalog(mode: CollectionMode) -> CatalogSnapshot {
        CatalogSnapshot {
            revision: 1,
            run_id: Some(2),
            collection_mode: mode,
            tags: Vec::new(),
        }
    }

    fn rest(values: Vec<ValueEntry>) -> ValuesSnapshot {
        ValuesSnapshot {
            revision: 1,
            t: 10,
            run_id: Some(2),
            collection_mode: CollectionMode::Configured,
            values,
        }
    }

    fn value(tag: &str, v: f64, t: i64) -> ValueEntry {
        ValueEntry {
            tag: tag.into(),
            v: Some(v),
            q: ValueQuality::Good,
            t,
            value_source: ValueSource::Real,
        }
    }

    fn valid_gate() -> PublishGate {
        PublishGate::new(1, ["a", "b"]).unwrap()
    }

    #[test]
    fn wire_parser_is_fail_closed() {
        let gate = valid_gate();
        assert!(
            matches!(parse_hub_wire("not-json", 1, &gate.allowed_tags), Err(e) if e.kind() == ErrorKind::ProtocolError)
        );
        assert!(
            matches!(parse_hub_wire(r#"{"op":"future"}"#, 1, &gate.allowed_tags), Err(e) if e.kind() == ErrorKind::ProtocolError)
        );
        assert!(
            matches!(parse_hub_wire(r#"{"op":"data","id":2,"t":1,"values":[]}"#, 1, &gate.allowed_tags), Err(e) if e.kind() == ErrorKind::ProtocolError)
        );
        assert!(
            matches!(parse_hub_wire(r#"{"op":"data","id":1,"t":1,"values":[{"tag":"x","v":1,"q":"good","t":1}]}"#, 1, &gate.allowed_tags), Err(e) if e.kind() == ErrorKind::ProtocolError)
        );
        assert!(
            matches!(parse_hub_wire(r#"{"op":"data","id":1,"t":1,"values":[{"tag":"a","v":1,"q":"good","t":1},{"tag":"a","v":2,"q":"good","t":2}]}"#, 1, &gate.allowed_tags), Err(e) if e.kind() == ErrorKind::ProtocolError)
        );
        assert!(
            matches!(parse_hub_wire(r#"{"op":"error","id":1,"code":"unknown_tag"}"#, 1, &gate.allowed_tags), Err(e) if e.kind() == ErrorKind::BindingUnresolved)
        );
        assert!(
            matches!(parse_hub_wire(r#"{"op":"error","id":2,"code":"unknown_tag"}"#, 1, &gate.allowed_tags), Err(e) if e.kind() == ErrorKind::ProtocolError)
        );
        assert!(
            matches!(parse_hub_wire(r#"{"op":"error","code":"unknown_tag"}"#, 1, &gate.allowed_tags), Err(e) if e.kind() == ErrorKind::ProtocolError)
        );
        assert!(
            matches!(parse_hub_wire(r#"{"op":"error","id":1,"code":"bad_protocol","detail":"secret"}"#, 1, &gate.allowed_tags), Err(e) if e.kind() == ErrorKind::ProtocolError)
        );
        assert_eq!(
            parse_hub_wire(r#"{"op":"event"}"#, 1, &gate.allowed_tags).unwrap(),
            HubWireMessage::Ignored
        );
        assert_eq!(
            parse_hub_wire(r#"{"op":"pong"}"#, 1, &gate.allowed_tags).unwrap(),
            HubWireMessage::Ignored
        );
    }

    #[test]
    fn gate_selection_and_pending_map_are_bounded() {
        for tags in [
            Vec::<&str>::new(),
            vec![""],
            vec!["  "],
            vec!["a,b"],
            vec!["a", "a"],
        ] {
            assert_eq!(
                PublishGate::new(1, tags).unwrap_err().kind(),
                ErrorKind::InvalidTagSelection
            );
        }
        assert!(PublishGate::new(1, ["a"]).is_ok());

        let mut gate = valid_gate();
        gate.accept_wire(r#"{"op":"data","id":1,"t":1,"values":[{"tag":"a","v":1,"q":"good","t":10},{"tag":"b","v":2,"q":"good","t":10}]}"#)
            .unwrap();
        gate.accept_wire(
            r#"{"op":"data","id":1,"t":1,"values":[{"tag":"a","v":0,"q":"good","t":9}]}"#,
        )
        .unwrap();
        gate.accept_wire(
            r#"{"op":"data","id":1,"t":1,"values":[{"tag":"a","v":3,"q":"good","t":11}]}"#,
        )
        .unwrap();
        assert_eq!(gate.pending_len(), 2);

        let mut repeated = valid_gate();
        for timestamp in 0..1024 {
            let raw = format!(
                r#"{{"op":"data","id":1,"t":1,"values":[{{"tag":"a","v":{},"q":"good","t":{}}}]}}"#,
                timestamp, timestamp
            );
            repeated.accept_wire(&raw).unwrap();
            assert!(repeated.pending_len() <= 2);
        }
        repeated
            .record_rest_snapshot(rest(vec![value("a", 0.0, 1022), value("b", 0.0, 1022)]))
            .unwrap();
        assert_eq!(
            repeated
                .finalize(&catalog(CollectionMode::Configured))
                .unwrap()
                .values[0]
                .v,
            Some(1023.0)
        );

        gate.sequence = u64::MAX - 1;
        assert_eq!(
            gate.accept_wire(r#"{"op":"data","id":1,"t":1,"values":[{"tag":"a","v":4,"q":"good","t":12},{"tag":"b","v":5,"q":"good","t":12}]}"#)
                .unwrap_err()
                .kind(),
            ErrorKind::ProtocolError
        );
        assert_eq!(gate.sequence, u64::MAX - 1);
    }

    #[test]
    fn config_change_invalidates_gate_and_current_is_not_published_early() {
        let mut gate = valid_gate();
        gate.record_rest_snapshot(rest(vec![value("a", 1.0, 1), value("b", 2.0, 1)]))
            .unwrap();
        assert_eq!(
            gate.accept_wire(r#"{"op":"config_changed","revision":2}"#)
                .unwrap_err()
                .kind(),
            ErrorKind::RevisionMismatch
        );
        assert_eq!(
            gate.finalize(&catalog(CollectionMode::Configured))
                .unwrap_err()
                .kind(),
            ErrorKind::RevisionMismatch
        );
    }

    #[test]
    fn invalid_wire_reason_survives_rest_record_and_finalize() {
        for (wire, expected) in [
            ("not-json", ErrorKind::ProtocolError),
            (
                r#"{"op":"error","id":1,"code":"unknown_tag"}"#,
                ErrorKind::BindingUnresolved,
            ),
            (
                r#"{"op":"config_changed","revision":2}"#,
                ErrorKind::RevisionMismatch,
            ),
        ] {
            let mut gate = valid_gate();
            assert_eq!(gate.accept_wire(wire).unwrap_err().kind(), expected);
            assert_eq!(
                gate.record_rest_snapshot(rest(vec![value("a", 1.0, 1), value("b", 2.0, 1)]))
                    .unwrap_err()
                    .kind(),
                expected
            );
            assert_eq!(
                gate.finalize(&catalog(CollectionMode::Configured))
                    .unwrap_err()
                    .kind(),
                expected
            );
        }
    }

    #[test]
    fn rest_snapshot_can_only_be_recorded_once() {
        let mut gate = valid_gate();
        gate.record_rest_snapshot(rest(vec![value("a", 1.0, 1), value("b", 2.0, 1)]))
            .unwrap();
        assert_eq!(
            gate.record_rest_snapshot(rest(vec![value("a", 3.0, 1), value("b", 4.0, 1)]))
                .unwrap_err()
                .kind(),
            ErrorKind::ProtocolError
        );
        assert_eq!(
            gate.finalize(&catalog(CollectionMode::Configured))
                .unwrap_err()
                .kind(),
            ErrorKind::ProtocolError
        );
    }

    #[test]
    fn rest_and_ws_merge_uses_timestamp_then_marker_and_keeps_value_source() {
        let mut rest_newer = valid_gate();
        rest_newer
            .accept_wire(
                r#"{"op":"data","id":1,"t":1,"values":[{"tag":"a","v":9,"q":"good","t":9}]}"#,
            )
            .unwrap();
        rest_newer
            .record_rest_snapshot(rest(vec![
                value("a", 5.0, 10),
                ValueEntry {
                    value_source: ValueSource::Simulation,
                    ..value("b", 6.0, 10)
                },
            ]))
            .unwrap();
        let result = rest_newer
            .finalize(&catalog(CollectionMode::Configured))
            .unwrap();
        assert_eq!(result.values[0].v, Some(5.0));
        assert_eq!(result.values[1].value_source, ValueSource::Simulation);

        let mut ws_newer = valid_gate();
        ws_newer
            .accept_wire(
                r#"{"op":"data","id":1,"t":1,"values":[{"tag":"a","v":11,"q":"good","t":11}]}"#,
            )
            .unwrap();
        ws_newer
            .record_rest_snapshot(rest(vec![
                value("a", 5.0, 10),
                ValueEntry {
                    value_source: ValueSource::Simulation,
                    ..value("b", 6.0, 10)
                },
            ]))
            .unwrap();
        let result = ws_newer
            .finalize(&catalog(CollectionMode::Configured))
            .unwrap();
        assert_eq!(result.values[0].v, Some(11.0));
        assert_eq!(result.values[0].value_source, ValueSource::Real);
        assert_eq!(result.values[1].value_source, ValueSource::Simulation);

        let mut equal_before_marker = valid_gate();
        equal_before_marker
            .accept_wire(
                r#"{"op":"data","id":1,"t":1,"values":[{"tag":"a","v":9,"q":"good","t":10}]}"#,
            )
            .unwrap();
        equal_before_marker
            .record_rest_snapshot(rest(vec![value("a", 5.0, 10), value("b", 6.0, 10)]))
            .unwrap();
        assert_eq!(
            equal_before_marker
                .finalize(&catalog(CollectionMode::Configured))
                .unwrap()
                .values[0]
                .v,
            Some(5.0)
        );

        let mut equal_after_marker = valid_gate();
        equal_after_marker
            .record_rest_snapshot(rest(vec![value("a", 5.0, 10), value("b", 6.0, 10)]))
            .unwrap();
        equal_after_marker
            .accept_wire(
                r#"{"op":"data","id":1,"t":1,"values":[{"tag":"a","v":9,"q":"good","t":10}]}"#,
            )
            .unwrap();
        assert_eq!(
            equal_after_marker
                .finalize(&catalog(CollectionMode::Configured))
                .unwrap()
                .values[0]
                .v,
            Some(9.0)
        );
    }

    #[test]
    fn metadata_and_rest_shape_fail_closed() {
        let mut gate = valid_gate();
        gate.record_rest_snapshot(rest(vec![value("a", 1.0, 1), value("a", 2.0, 1)]))
            .unwrap();
        assert_eq!(
            gate.finalize(&catalog(CollectionMode::Configured))
                .unwrap_err()
                .kind(),
            ErrorKind::ProtocolError
        );

        let mut gate = valid_gate();
        gate.record_rest_snapshot(rest(vec![value("a", 1.0, 1), value("b", 2.0, 1)]))
            .unwrap();
        let mut mismatch = catalog(CollectionMode::Configured);
        mismatch.revision = 2;
        assert_eq!(
            gate.finalize(&mismatch).unwrap_err().kind(),
            ErrorKind::RevisionMismatch
        );

        for (run_id, mode, expected) in [
            (
                Some(3),
                CollectionMode::Configured,
                ErrorKind::RuntimeMetadataMismatch,
            ),
            (
                Some(2),
                CollectionMode::AllSimulation,
                ErrorKind::RuntimeMetadataMismatch,
            ),
            (
                Some(2),
                CollectionMode::Unknown("future".into()),
                ErrorKind::RuntimeMetadataMismatch,
            ),
        ] {
            let mut gate = valid_gate();
            let mut snapshot = rest(vec![value("a", 1.0, 1), value("b", 2.0, 1)]);
            snapshot.run_id = run_id;
            gate.record_rest_snapshot(snapshot).unwrap();
            assert_eq!(gate.finalize(&catalog(mode)).unwrap_err().kind(), expected);
        }

        for values in [
            vec![value("a", 1.0, 1)],
            vec![value("a", 1.0, 1), value("b", 2.0, 1), value("c", 3.0, 1)],
            vec![value("a", 1.0, 1), value("x", 2.0, 1)],
        ] {
            let mut gate = valid_gate();
            gate.record_rest_snapshot(rest(values)).unwrap();
            assert_eq!(
                gate.finalize(&catalog(CollectionMode::Configured))
                    .unwrap_err()
                    .kind(),
                ErrorKind::ProtocolError
            );
        }
    }

    #[test]
    fn all_public_states_and_non_live_transitions_clear_current() {
        use crate::types::{TagClientConnectionState, TagClientState};
        let states = [
            TagClientConnectionState::Stopped,
            TagClientConnectionState::Connecting,
            TagClientConnectionState::Handshaking,
            TagClientConnectionState::Live,
            TagClientConnectionState::Rebinding,
            TagClientConnectionState::Reconnecting,
            TagClientConnectionState::Unauthorized,
        ];
        for state in states {
            let mut public = TagClientState::new(state);
            public.publish(rest(vec![value("a", 1.0, 1), value("b", 2.0, 1)]));
            assert!(public.current().is_some());
            assert_eq!(public.connection_state(), TagClientConnectionState::Live);
            public.transition(state);
            if state != TagClientConnectionState::Live {
                assert_eq!(public.current(), None);
            }
        }
    }

    #[test]
    fn new_public_types_do_not_expose_secret_endpoint_or_path() {
        use crate::types::{TagClientConnectionState, TagClientState};
        let state = TagClientState::new(TagClientConnectionState::Connecting);
        let debug = format!("{state:?}");
        let display = state.to_string();
        assert!(
            !debug.contains("secret") && !debug.contains("/private") && !debug.contains("endpoint")
        );
        assert!(
            !display.contains("secret")
                && !display.contains("/private")
                && !display.contains("endpoint")
        );
        let error = Error::new(ErrorKind::ProtocolError);
        assert!(!format!("{error:?}").contains("secret"));
        assert!(!error.to_string().contains("/private"));
    }
}
