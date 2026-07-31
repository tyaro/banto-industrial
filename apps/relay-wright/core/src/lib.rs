//! relay-wright's domain/service layer (spec §10). Kept tauri-free so it
//! is testable without the src-tauri crate (which cannot be built in every
//! environment, e.g. CI containers without webkit2gtk). Thin
//! `tauri::command` adapters in `src-tauri` call into this crate; the same
//! services back the embedded REST server in M6.

pub mod assets;
pub mod audit;
pub mod backup;
pub mod db;
// W3-A (luminous-discovering-goblet.md): the PLC access broker - one live SLMP
// session per CPU, request-driven, serializing reads and writes onto a single
// socket. Infrastructure only: no arming, rate-limiting, rule evaluation, or
// write auditing (that is W3-B's auto-write engine, which will hold the
// broker's handles).
pub mod engine;
pub mod events;
pub mod rest;
pub mod settings;
// Crate-internal validation/error helpers shared by the write_* service
// modules (not part of the public API - see `support.rs`'s doc comment).
mod support;
pub mod users;
pub mod write_rule_conditions;
pub mod write_rules;
pub mod write_targets;
