//! chronogazer's domain/service layer (spec §10). Kept tauri-free so it
//! is testable without the src-tauri crate (which cannot be built in every
//! environment, e.g. CI containers without webkit2gtk). Thin
//! `tauri::command` adapters in `src-tauri` call into this crate; the same
//! services back the embedded REST server in M6.

pub mod assets;
pub mod audit;
pub mod backup;
pub mod db;
pub mod events;
// #332: Hub 接続（`banto-hub-bootstrap` の配線）。keyring は src-tauri、
// `banto-serve` は `hub::UnavailableKeyStore` を渡す - モジュール doc 参照。
pub mod hub;
pub mod rest;
pub mod settings;
#[cfg(test)]
pub(crate) mod test_support;
pub mod users;

// #383 段階2a / R1-B（レジストリ CRUD）: banto-tags の3サービスと行型を
// re-export し、`src-tauri`（invariant: このアプリ自身の新規依存を持たない -
// `db::DbPool` 相当の precedent と同じ理由）が `AppState` のフィールド・
// コマンドシグネチャに名前を出せるようにする。relay-wright の `lib.rs`
// （R1-B 同名 re-export）と同じ作法。camelCase の create/update wire
// payload は `rest` 側（`rest::PlcConnectionPayload` 等）が持つ -
// banto-tags 自身の `*Input` は snake_case で deserialize し、ワイヤを
// 一切またがない。
pub use banto_tags::{
    CollectionGroup, CollectionGroupService, PlcConnection, PlcConnectionService, Tag, TagService,
};
