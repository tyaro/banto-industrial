//! relay-wright's domain/service layer (spec §10). Kept tauri-free so it
//! is testable without the src-tauri crate (which cannot be built in every
//! environment, e.g. CI containers without webkit2gtk). Thin
//! `tauri::command` adapters in `src-tauri` call into this crate; the same
//! services back the embedded REST server in M6.

pub mod assets;
pub mod audit;
pub mod backup;
pub mod db;
// W3 (luminous-discovering-goblet.md): the PLC access broker (W3-A) plus the
// auto-write engine (W3-B) built on it - condition polling, edge-triggered rule
// evaluation, arming, rate-limiting, and log-before-write auditing. The
// `Engine`/`EngineControl` handles here are what W3-B2's Tauri commands / REST
// routes will wire up.
pub mod engine;

// The auto-write engine's public surface (W3-B): the running engine, its safe
// arm/disarm/dry-run control handle, its config, and a status snapshot.
pub use engine::{Engine, EngineConfig, EngineControl, EngineStatus, SharedEngineControl};
pub mod events;
pub mod rest;
pub mod settings;
// Crate-internal validation/error helpers shared by the write_* service
// modules (not part of the public API - see `support.rs`'s doc comment).
mod support;
pub mod users;
pub mod write_audit_query;
pub mod write_rule_conditions;
pub mod write_rules;
pub mod write_targets;
