//! `banto-tagclient` S2b-1: read-only Hub contracts and direct WebSocket handshake core.
//!
//! This crate provides safe endpoint construction, Hub wire DTOs, an opaque
//! API-key boundary, stable-ID binding resolution, read-only REST requests,
//! network-free publish-gate core, and direct authenticated WebSocket handshakes.
//! Workers, subscriptions, reconnect/backoff, PLC/Modbus access, writes, Tauri,
//! and keyring integration remain S2b-2/S3 (or an application-side concern).
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
/// S2a remains crate-internal until the S2b-2 worker slice wires it in.
mod stream_core;
pub mod types;
mod worker;
mod ws_transport;

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
