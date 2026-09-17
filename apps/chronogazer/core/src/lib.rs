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
