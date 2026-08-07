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

// R1-B (PLC接続/収集グループ/タグ登録画面): banto-tags' three registry
// services and their row types, re-exported so `src-tauri` (whose invariant
// is to add NO new dependencies of its own - see `db::DbPool`'s precedent)
// can name them for `AppState` fields and command signatures. The services
// themselves are banto-tags' finished building blocks; this app only wires
// them to its two transport paths (`rest::api_router` + the Tauri commands).
// The camelCase create/update wire payloads live in `rest`
// (`rest::PlcConnectionPayload` etc.), shared by both paths - banto-tags' own
// `*Input` structs deserialize snake_case and never cross a wire here.
pub use banto_tags::{
    CollectionGroup, CollectionGroupService, PlcConnection, PlcConnectionService, Tag, TagService,
};
pub mod events;
// Project file export/import (feature/project-file): save the whole
// configuration registry to a versioned JSON project file and load it back.
// Composes the existing registry services; no new dependency (invariant).
pub mod project;
// QR文字列リスト（デバッグ支援）: タッチパネルのQRリーダーに画面から読ませる
// 文字列の登録・並び替えと、そのQRコードSVGのサーバー側生成（/qr-codes 画面）。
pub mod qr_strings;
// feature/easy-delete: cascade delete for the tag registry (connection →
// groups → tags in one transaction) plus the preview counts the confirm
// dialogs show. Lives HERE - not in banto-tags, whose guarded deletes are
// shared semantics other apps rely on and must stay untouched.
pub mod registry_cascade;
pub mod rest;
pub mod settings;
// Crate-internal validation/error helpers shared by the write_* service
// modules (not part of the public API - see `support.rs`'s doc comment).
mod support;
#[cfg(test)]
pub(crate) mod test_support;
pub mod users;
pub mod write_audit_query;
pub mod write_rule_conditions;
pub mod write_rules;
pub mod write_targets;
