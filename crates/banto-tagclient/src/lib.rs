//! `banto-tagclient` S4a: read-only Hub contracts with owned restartable
//! connection generations.
//!
//! This crate provides safe endpoint construction, Hub wire DTOs, an opaque
//! API-key boundary, stable-ID binding resolution, read-only REST requests,
//! network-free publish-gate core, direct authenticated WebSocket handshakes,
//! and a public owner for one generation. The consumed `TagClientHandle::restart`
//! API replaces credentials and endpoint ownership only after the old worker is
//! stopped and joined. PLC/Modbus access, writes, Tauri, and keyring integration
//! remain outside this crate's read-only boundary.
//!
//! The DTOs mirror the machine-facing snake_case `/api/v1/tags` and
//! `/api/v1/values` responses. Unknown mode, source, and quality strings are
//! retained as unknown values so a future Hub cannot silently become a real
//! value source or a good-quality value.

pub mod binding;
pub mod endpoint;
pub mod error;
mod handle;
pub mod rest;
pub mod secret;
/// S2a remains crate-internal until the S2b-2 worker slice wires it in.
mod stream_core;
#[cfg(test)]
mod test_support;
pub mod types;
mod worker;
mod ws_transport;

pub use binding::{
    resolve_bindings, BindingRequest, BindingResolution, ResolvedBinding, UnresolvedBinding,
};
pub use endpoint::{Endpoint, RestUrls};
pub use error::{Error, ErrorKind, Result};
pub use handle::TagClientHandle;
pub use rest::RestClient;
pub use secret::{SecretApiKey, SecretError};
pub use types::{
    CatalogSnapshot, CatalogTag, CollectionMode, StableTagId, TagClientConnectionState,
    TagClientState, ValueEntry, ValueQuality, ValueSource, ValuesSnapshot,
};
