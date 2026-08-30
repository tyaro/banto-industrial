//! Stable-ID binding resolution. Resolution is deliberately all-or-nothing
//! for duplicate input/catalog identities.

use std::collections::{HashMap, HashSet};

use crate::error::{Error, ErrorKind, Result};
use crate::types::{CatalogTag, StableTagId};

/// An application-owned key mapped to a catalog row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BindingRequest {
    pub binding_key: String,
    pub stable_id: StableTagId,
}

/// A successfully resolved binding. It contains no current value; values are
/// a later transport concern.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedBinding {
    pub binding_key: String,
    pub stable_id: StableTagId,
    pub external_name: String,
    pub tag_key: String,
}

/// An explicit unresolved result for an unknown stable ID. No current value is
/// attached, preventing an unresolved row from being mistaken for current.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnresolvedBinding {
    pub binding_key: String,
    pub stable_id: StableTagId,
}

/// Result of resolving all requested bindings against one catalog snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BindingResolution {
    pub resolved: Vec<ResolvedBinding>,
    pub unresolved: Vec<UnresolvedBinding>,
}

/// Resolve every request against the catalog, failing closed on any duplicate.
/// Unknown stable IDs are not a global error and appear in `unresolved`.
pub fn resolve_bindings(
    requests: &[BindingRequest],
    catalog: &[CatalogTag],
) -> Result<BindingResolution> {
    let mut binding_keys = HashSet::with_capacity(requests.len());
    let mut requested_ids = HashSet::with_capacity(requests.len());
    for request in requests {
        if !binding_keys.insert(request.binding_key.as_str()) {
            return Err(Error::new(ErrorKind::DuplicateBindingKey));
        }
        if !requested_ids.insert(request.stable_id) {
            return Err(Error::new(ErrorKind::DuplicateRequestedStableId));
        }
    }

    let mut by_id = HashMap::with_capacity(catalog.len());
    for entry in catalog {
        if by_id.insert(entry.ids, entry).is_some() {
            return Err(Error::new(ErrorKind::DuplicateCatalogStableId));
        }
    }

    let mut resolved = Vec::with_capacity(requests.len());
    let mut unresolved = Vec::new();
    for request in requests {
        match by_id.get(&request.stable_id) {
            Some(entry) => resolved.push(ResolvedBinding {
                binding_key: request.binding_key.clone(),
                stable_id: request.stable_id,
                external_name: entry.external_name.clone(),
                tag_key: entry.tag_key.clone(),
            }),
            None => unresolved.push(UnresolvedBinding {
                binding_key: request.binding_key.clone(),
                stable_id: request.stable_id,
            }),
        }
    }
    Ok(BindingResolution {
        resolved,
        unresolved,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog_entry(id: StableTagId, name: &str) -> CatalogTag {
        CatalogTag {
            external_name: name.to_owned(),
            tag_key: format!("tag:{name}"),
            ids: id,
            connection: "connection-a".into(),
            group: "group-a".into(),
            name: name.into(),
            address: "address-a".into(),
            data_type: "f64".into(),
            unit: None,
            decimals: 0,
            period_ms: 100,
            enabled: true,
            writable: false,
            tag_kind: "plc".into(),
            expression: None,
            retain: false,
            simulation: false,
            configured_simulation: false,
            effective_simulation: false,
            value_source: crate::ValueSource::Real,
        }
    }

    fn request(key: &str, id: StableTagId) -> BindingRequest {
        BindingRequest {
            binding_key: key.into(),
            stable_id: id,
        }
    }

    #[test]
    fn resolves_and_reports_unknown_without_partial_current_value() {
        let known = StableTagId::new(1, 2, 3);
        let unknown = StableTagId::new(4, 5, 6);
        let result = resolve_bindings(
            &[request("first", known), request("missing", unknown)],
            &[catalog_entry(known, "tag-a")],
        )
        .unwrap();
        assert_eq!(result.resolved.len(), 1);
        assert_eq!(result.unresolved.len(), 1);
        assert_eq!(result.unresolved[0].binding_key, "missing");
    }

    #[test]
    fn every_duplicate_class_fails_before_partial_result_is_returned() {
        let id = StableTagId::new(1, 2, 3);
        let duplicate_key = resolve_bindings(
            &[
                request("same", id),
                request("same", StableTagId::new(4, 5, 6)),
            ],
            &[],
        );
        assert_eq!(
            duplicate_key.unwrap_err().kind(),
            ErrorKind::DuplicateBindingKey
        );

        let duplicate_request =
            resolve_bindings(&[request("first", id), request("second", id)], &[]);
        assert_eq!(
            duplicate_request.unwrap_err().kind(),
            ErrorKind::DuplicateRequestedStableId
        );

        let duplicate_catalog = resolve_bindings(
            &[request("first", id)],
            &[catalog_entry(id, "tag-a"), catalog_entry(id, "tag-b")],
        );
        assert_eq!(
            duplicate_catalog.unwrap_err().kind(),
            ErrorKind::DuplicateCatalogStableId
        );
    }
}
