//! `banto-hub-bootstrap` (#332): zero-config banto-hub client bootstrap.
//!
//! A Banto app that wants to read tags from banto-hub has to get an API key
//! from somewhere. Until now that meant an operator opening the Hub's admin
//! screen, issuing a key, and copy-pasting it into the app. This crate
//! automates exactly that, and nothing more:
//!
//! ```text
//! GET /api/commissioning/status   -> still in commissioning?
//!   yes -> POST /api/api-keys  (scopes: ["read"])   -> plaintext key, once
//!          KeyStore::set                            -> OS keyring
//!          GET /api/v1/tags (via banto-tagclient)   -> proof it works
//!   no  -> HubStatus::NeedsPairing (an administrator hands over a key)
//! ```
//!
//! **banto-hub needs no changes for this.** Every endpoint above already
//! exists with exactly these semantics (verified in issue #332's 2026-09-08
//! comment): the commissioning status route is deliberately unauthenticated,
//! the admin router bypasses bearer auth while commissioning is open, and
//! `GET /api/v1/tags` answers `200 {"tags": []}` on a Hub with no tags yet.
//!
//! # 境界
//!
//! * **The keyring and the settings store are the app's**, behind the
//!   [`KeyStore`] and [`BootstrapState`] traits - the same boundary
//!   `banto-tagclient` draws ("keyring / Tauri はこの crate の外"). This
//!   crate has no OS, database, or Tauri dependency.
//! * **The catalog is `banto-tagclient`'s.** The connection check calls
//!   [`RestClient::fetch_catalog`](banto_tagclient::RestClient::fetch_catalog);
//!   this crate never parses a catalog body. The only requests it makes
//!   itself are the two admin endpoints above and a status-only probe used
//!   to tell `401` from `403` (see [`Bootstrapper`]'s `verify`).
//! * **Subscribing is not this crate's job.** `TagClientHandle`/`start` is
//!   never called here. [`Bootstrapper::rest_client`] hands out an
//!   authenticated client so the app can own one subscription generation,
//!   but the generation itself - when to start it, when it is still the same
//!   one, when to stop it - belongs to the app (#383 段階1:
//!   `chronogazer_core::hub`).
//!
//! # 固定していること
//!
//! * Issuing is whitelisted to [`ISSUABLE_SCOPES`] (v1: `read`). `admin` and
//!   every `write:` form are refused in code, before any network call - a
//!   self-issued `admin` key would outlive lockdown through the MCP surface
//!   and amount to a permanent bypass of it.
//! * A stored key is reused; a key is not minted per launch.
//! * A key name is `{app_id}-{installation_id}-{unix seconds}`, so a
//!   re-bootstrap after keyring loss cannot collide with the old, now
//!   unrecoverable key (banto-hub's `api_keys.name` is UNIQUE and the
//!   plaintext is returned only once).
//! * Revocation happens **only** by an id this installation issued and
//!   recorded. Keys are never listed and matched by name - that would take
//!   another machine's credential away.
//! * [`Bootstrapper::disconnect`] clears local state only; it never revokes.
//! * Lockdown is never undone, worked around, or re-probed for a way in: a
//!   locked-down Hub with no usable key is [`HubStatus::NeedsPairing`], full
//!   stop.
//!
//! # 同一 PC 限定
//!
//! banto-hub refuses to start on a non-loopback bind while commissioning is
//! open (`enforce_loopback_when_commissioning`, tag-server-design.md §5.6).
//! Self-issuing is therefore structurally limited to an app running on the
//! same machine as the Hub - a third party on the LAN cannot reach the open
//! window at all. An app on another machine uses
//! [`Bootstrapper::adopt_manual_key`] with a key an administrator issued.

mod admin;
mod bootstrap;
pub mod error;
pub mod scopes;
pub mod state;
pub mod status;
#[cfg(test)]
mod test_support;

pub use bootstrap::Bootstrapper;
pub use error::{Error, ErrorKind, Result};
pub use scopes::{validate_issue_scopes, DEFAULT_SCOPES, ISSUABLE_SCOPES};
pub use state::{BootstrapState, HubRecord, KeyStore};
pub use status::{HubConnection, HubStatus, UnreachableCause};
