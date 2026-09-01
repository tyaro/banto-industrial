//! `banto-tagclient` S4a: read-only Hub contracts with owned restartable
//! connection generations, plus (Issue #123) a single-tag write path.
//!
//! This crate provides safe endpoint construction, Hub wire DTOs, an opaque
//! API-key boundary, stable-ID binding resolution, REST requests (read and a
//! single-tag write), network-free publish-gate core, direct authenticated
//! WebSocket handshakes, and a public owner for one generation. The consumed
//! `TagClientHandle::restart` API replaces credentials and endpoint ownership
//! only after the old worker is stopped and joined. PLC/Modbus access, batch/
//! recipe writes, Tauri, and keyring integration remain outside this crate's
//! boundary (batch/recipe writes are deferred until a real requirement
//! appears - see the `write` module doc).
//!
//! [`RestClient::write_tag`] is deliberately independent of `worker.rs`'s
//! reconnect/backoff supervisor: it is a single request that never retries
//! automatically (2026-09-01 owner decision), because resending a write the
//! caller cannot confirm was lost risks a double write to the PLC.
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
pub mod write;
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
pub use write::RequestedValue;
