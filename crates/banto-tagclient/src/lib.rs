//! `banto-tagclient` S1b: read-only, public banto-hub data-plane contracts.
//!
//! This crate provides safe endpoint construction, Hub wire DTOs, an opaque
//! API-key boundary, stable-ID binding resolution, and read-only REST requests.
//! WebSocket subscriptions, PLC/Modbus access, writes, Tauri, and keyring
//! integration remain S2/S3 (or an application-side concern).
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
pub mod types;

pub use binding::{
    resolve_bindings, BindingRequest, BindingResolution, ResolvedBinding, UnresolvedBinding,
};
pub use endpoint::{Endpoint, RestUrls};
pub use error::{Error, ErrorKind, Result};
pub use rest::RestClient;
pub use secret::{SecretApiKey, SecretError};
pub use types::{
    CatalogSnapshot, CatalogTag, CollectionMode, StableTagId, ValueEntry, ValueQuality,
    ValueSource, ValuesSnapshot,
};
