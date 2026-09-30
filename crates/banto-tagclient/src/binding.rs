//! External-name binding resolution. Resolution is deliberately
//! all-or-nothing for duplicate input/catalog identities.
//!
//! **Bindings are keyed by `external_name` (`{connection}.{group}.{tag}`), not
//! by the Hub's numeric [`StableTagId`]** (2026-09-30 owner decision,
//! docs/scada-design.md §9.6). Every Hub public contract - WebSocket
//! subscribe, `POST /api/v1/values/{tag}`, the write scope, MQTT - already
//! speaks names, and a numeric ID does not survive a delete-and-recreate of
//! the same tag or a CSV re-import, nor can it be carried across
//! environments. So an application stores the name it wants, and this module
//! only checks that the name exists in the current catalog. A rename makes
//! the old name unresolved (`binding_unresolved`; tag-server-design.md §4.1
//! "rename is a breaking change") until the application supplies the new
//! name; a delete-and-recreate under the same name resolves again by itself.
//! [`StableTagId`] stays in [`CatalogTag::ids`] as the Hub's wire shape but
//! is never used to bind, subscribe, or write.

use std::collections::{HashMap, HashSet};

use crate::error::{Error, ErrorKind, Result};
use crate::types::CatalogTag;

/// An application-owned key mapped to a catalog external name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BindingRequest {
    pub binding_key: String,
    pub external_name: String,
}

/// A successfully resolved binding. It contains no current value; values are
/// a later transport concern.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedBinding {
    pub binding_key: String,
    pub external_name: String,
    pub tag_key: String,
}

/// An explicit unresolved result for an external name absent from the
/// catalog. No current value is attached, preventing an unresolved row from
/// being mistaken for current.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnresolvedBinding {
    pub binding_key: String,
    pub external_name: String,
}

/// Result of resolving all requested bindings against one catalog snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BindingResolution {
    pub resolved: Vec<ResolvedBinding>,
    pub unresolved: Vec<UnresolvedBinding>,
}

/// Resolve every request against the catalog by `external_name`, failing
/// closed on any duplicate (requested `binding_key`, requested
/// `external_name`, catalog `external_name`). Names absent from the catalog
/// are not a global error and appear in `unresolved`.
pub fn resolve_bindings(
    requests: &[BindingRequest],
    catalog: &[CatalogTag],
) -> Result<BindingResolution> {
    let mut binding_keys = HashSet::with_capacity(requests.len());
    let mut requested_names = HashSet::with_capacity(requests.len());
    for request in requests {
        if !binding_keys.insert(request.binding_key.as_str()) {
            return Err(Error::new(ErrorKind::DuplicateBindingKey));
        }
        if !requested_names.insert(request.external_name.as_str()) {
            return Err(Error::new(ErrorKind::DuplicateRequestedExternalName));
        }
    }

    let mut by_name = HashMap::with_capacity(catalog.len());
    for entry in catalog {
        if by_name
            .insert(entry.external_name.as_str(), entry)
            .is_some()
        {
            return Err(Error::new(ErrorKind::DuplicateCatalogExternalName));
        }
    }

    let mut resolved = Vec::with_capacity(requests.len());
    let mut unresolved = Vec::new();
    for request in requests {
        match by_name.get(request.external_name.as_str()) {
            Some(entry) => resolved.push(ResolvedBinding {
                binding_key: request.binding_key.clone(),
                external_name: entry.external_name.clone(),
                tag_key: entry.tag_key.clone(),
            }),
            None => unresolved.push(UnresolvedBinding {
                binding_key: request.binding_key.clone(),
                external_name: request.external_name.clone(),
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
    use crate::types::StableTagId;

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

    fn request(key: &str, name: &str) -> BindingRequest {
        BindingRequest {
            binding_key: key.into(),
            external_name: name.into(),
        }
    }

    #[test]
    fn resolves_by_external_name_and_carries_the_tag_key() {
        let result = resolve_bindings(
            &[request("first", "tag-a")],
            &[
                catalog_entry(StableTagId::new(1, 2, 3), "tag-b"),
                catalog_entry(StableTagId::new(1, 2, 4), "tag-a"),
            ],
        )
        .unwrap();
        assert!(result.unresolved.is_empty());
        assert_eq!(
            result.resolved,
            vec![ResolvedBinding {
                binding_key: "first".into(),
                external_name: "tag-a".into(),
                tag_key: "tag:tag-a".into(),
            }]
        );
    }

    #[test]
    fn name_missing_from_catalog_is_unresolved_without_partial_current_value() {
        let result = resolve_bindings(
            &[request("first", "tag-a"), request("missing", "tag-x")],
            &[catalog_entry(StableTagId::new(1, 2, 3), "tag-a")],
        )
        .unwrap();
        assert_eq!(result.resolved.len(), 1);
        assert_eq!(
            result.unresolved,
            vec![UnresolvedBinding {
                binding_key: "missing".into(),
                external_name: "tag-x".into(),
            }]
        );
    }

    #[test]
    fn stable_id_is_not_part_of_the_binding_identity() {
        // The same name with a different numeric ID (delete-and-recreate,
        // CSV re-import) still resolves; the ID plays no role.
        let result = resolve_bindings(
            &[request("first", "tag-a")],
            &[catalog_entry(StableTagId::new(9, 9, 9), "tag-a")],
        )
        .unwrap();
        assert_eq!(result.resolved.len(), 1);
    }

    #[test]
    fn every_duplicate_class_fails_before_partial_result_is_returned() {
        let duplicate_key =
            resolve_bindings(&[request("same", "tag-a"), request("same", "tag-b")], &[]);
        assert_eq!(
            duplicate_key.unwrap_err().kind(),
            ErrorKind::DuplicateBindingKey
        );

        let duplicate_request = resolve_bindings(
            &[request("first", "tag-a"), request("second", "tag-a")],
            &[],
        );
        assert_eq!(
            duplicate_request.unwrap_err().kind(),
            ErrorKind::DuplicateRequestedExternalName
        );

        let duplicate_catalog = resolve_bindings(
            &[request("first", "tag-a")],
            &[
                catalog_entry(StableTagId::new(1, 2, 3), "tag-a"),
                catalog_entry(StableTagId::new(1, 2, 4), "tag-a"),
            ],
        );
        assert_eq!(
            duplicate_catalog.unwrap_err().kind(),
            ErrorKind::DuplicateCatalogExternalName
        );
    }
}
