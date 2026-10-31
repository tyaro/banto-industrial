//! `banto-tagclient` S4a: read-only Hub contracts with owned restartable
//! connection generations, plus (Issue #123) a single-tag write path.
//!
//! This crate provides safe endpoint construction, Hub wire DTOs, an opaque
//! API-key boundary, external-name binding resolution, REST requests (read and a
//! single-tag write), network-free publish-gate core, direct authenticated
//! WebSocket handshakes, and a public owner for one generation. The consumed
//! `TagClientHandle::restart` API replaces credentials and endpoint ownership
//! only after the old worker is stopped and joined. PLC/Modbus access, batch/
//! recipe writes, Tauri, and keyring integration remain outside this crate's
//! boundary (batch/recipe writes are deferred until a real requirement
//! appears - see the `write` module doc).
//!
//! **Bindings and writes are keyed by `external_name`
//! (`{connection}.{group}.{tag}`), never by the Hub's numeric
//! [`StableTagId`]** (2026-09-30 owner decision, docs/scada-design.md §9.6).
//! Every Hub public contract (WebSocket subscribe, `POST /api/v1/values/{tag}`,
//! the write scope, MQTT) already speaks names, and a numeric ID changes on
//! delete-and-recreate or a CSV re-import and cannot be carried across
//! environments. A rename therefore leaves the old name unresolved
//! (`binding_unresolved`; tag-server-design.md §4.1 "rename is a breaking
//! change") until the application supplies the new name, while a
//! delete-and-recreate under the same name resolves again on the next rebind.
//! [`StableTagId`] remains only as the Hub's wire shape in [`CatalogTag::ids`].
//!
//! [`RestClient::write_tag`] is deliberately independent of `worker.rs`'s
//! reconnect/backoff supervisor: it is a single request that never retries
//! automatically (2026-09-01 owner decision), because resending a write the
//! caller cannot confirm was lost risks a double write to the PLC.
//!
//! #446: when banto-hub closes an open stream with close code 1008 (the
//! credential was revoked, expired, tripped, or is unknown), the worker keeps
//! the reason ([`close::classify_close`]), publishes `Unauthorized` with the
//! specific [`ErrorKind`] in `last_error`, and does not reconnect. Ordinary
//! disconnects still reconnect with backoff.
//!
//! #682 PR-1: a `Live` [`TagClientState`] carries one [`GenerationSnapshot`]
//! (`TagClientState::snapshot`) that holds the catalog, the
//! [`BindingResolution`], and the values of the same attempt, so a caller
//! never pairs values with a catalog from another generation
//! (docs/banto-tagclient-partial-binding-design.md §6, R1-R7). The three parts
//! are separate `Arc`s, so cloning the state out of `state_watch` is O(1)
//! however large the catalog is. `generation` is a process-wide counter
//! (unique across every handle, so a new `start` after `shutdown` never reuses
//! a number); it advances on every attempt that fetched a catalog, so it is
//! only guaranteed to grow, not to grow by one. Every non-`Live` state drops the
//! snapshot together with `current`.
//! `TagClientState::current` stays as the values of that snapshot.
//!
//! #682 PR-2 (partial binding, design §8): an attempt goes `Live` as soon as
//! one requested binding resolves. Only the resolved names are subscribed,
//! fetched, and published; each unresolved request is listed in
//! [`BindingResolution::unresolved`] with its [`UnresolvedReason`] and no
//! value, and a later `config_changed` rebind moves names between the two
//! lists (created names resolve, renamed or deleted ones become unresolved;
//! similar names are never assigned). Duplicate bindings, requests, or
//! catalog names still fail the whole attempt. Until PR-3, an attempt in
//! which nothing resolved still fails with `binding_unresolved`.
//!
//! The DTOs mirror the machine-facing snake_case `/api/v1/tags` and
//! `/api/v1/values` responses. Unknown mode, source, and quality strings are
//! retained as unknown values so a future Hub cannot silently become a real
//! value source or a good-quality value.

pub mod binding;
pub mod close;
pub mod endpoint;
pub mod error;
mod handle;
mod pinned_tls;
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
    UnresolvedReason,
};
pub use endpoint::{Endpoint, RestUrls};
pub use error::{Error, ErrorKind, Result};
pub use handle::TagClientHandle;
pub use pinned_tls::PinnedHubTls;
pub use rest::RestClient;
pub use secret::{SecretApiKey, SecretError};
pub use types::{
    BindingView, CatalogSnapshot, CatalogTag, CollectionMode, GenerationSnapshot, StableTagId,
    TagClientConnectionState, TagClientState, ValueEntry, ValueQuality, ValueSource,
    ValuesSnapshot,
};
pub use write::RequestedValue;
