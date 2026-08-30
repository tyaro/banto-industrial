//! `banto-tagclient` S1a: read-only, public banto-hub data-plane contracts.
//!
//! This crate intentionally stops at safe endpoint construction, Hub wire DTOs,
//! opaque API-key validation, and stable-ID binding resolution. It does not
//! send REST requests or authorization headers. REST transport, redirects,
//! WebSocket subscriptions, PLC/Modbus access, writes, Tauri, and keyring
//! integration are S1b/S2/S3 work and are not part of this crate yet.
//!
//! The DTOs mirror the machine-facing snake_case `/api/v1/tags` and
//! `/api/v1/values` responses. Unknown mode, source, and quality strings are
//! retained as unknown values so a future Hub cannot silently become a real
//! value source or a good-quality value.

pub mod binding;
pub mod endpoint;
pub mod error;
pub mod secret;
pub mod types;

pub use binding::{
    resolve_bindings, BindingRequest, BindingResolution, ResolvedBinding, UnresolvedBinding,
};
pub use endpoint::{Endpoint, RestUrls};
pub use error::{Error, ErrorKind, Result};
pub use secret::{SecretApiKey, SecretError};
pub use types::{
    CatalogSnapshot, CatalogTag, CollectionMode, StableTagId, ValueEntry, ValueQuality,
    ValueSource, ValuesSnapshot,
};
