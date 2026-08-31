//! `banto-tagclient` S2a: read-only Hub contracts and pure publish-gate core.
//!
//! This crate provides safe endpoint construction, Hub wire DTOs, an opaque
//! API-key boundary, stable-ID binding resolution, read-only REST requests, and
//! a network-free WebSocket wire/publish-gate core. Real WebSocket transport,
//! workers, reconnect/backoff, PLC/Modbus access, writes, Tauri, and keyring
//! integration remain S2b/S3 (or an application-side concern).
//!
//! The DTOs mirror the machine-facing snake_case `/api/v1/tags` and
//! `/api/v1/values` responses. Unknown mode, source, and quality strings are
//! retained as unknown values so a future Hub cannot silently become a real
//! value source or a good-quality value.

pub mod binding;
pub mod endpoint;
pub mod error;
pub mod rest;
pub mod secret;
/// S2a remains crate-internal until the S2b transport slice wires it in.
#[allow(dead_code, reason = "S2a core is wired by the S2b transport slice")]
mod stream_core;
pub mod types;

pub use binding::{
    resolve_bindings, BindingRequest, BindingResolution, ResolvedBinding, UnresolvedBinding,
};
pub use endpoint::{Endpoint, RestUrls};
pub use error::{Error, ErrorKind, Result};
pub use rest::RestClient;
pub use secret::{SecretApiKey, SecretError};
pub use types::{
    CatalogSnapshot, CatalogTag, CollectionMode, StableTagId, TagClientConnectionState,
    TagClientState, ValueEntry, ValueQuality, ValueSource, ValuesSnapshot,
};
