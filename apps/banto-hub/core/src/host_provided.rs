//! host が自分で I/O を持つプロトコルのレジストリ項目（R2b-1a、
//! docs/banto-hub-connection-settings-design.md §4.1）。
//!
//! `push`（取り込み、[`crate::ingest::driver::PushDriver`]）・`postgres`
//! （[`crate::db_source::driver::PostgresDriver`]）・`virtual`
//! （[`crate::computed::driver::VirtualDriver`]）は、識別・設定の検証・秘密のキー・
//! アドレスの関数を `DriverBase` の形で持ち、I/O は Hub の収集の外部ソース・
//! `db_source`・計算が持ち続ける。`DriverKind::HostProvided`（恒久の種類。R2b-3 で
//! 消える `HostBound` とは違う）で載せる。
//!
//! 登録するのは**この crate ではなく host**（`banto-hub-host`・`src-tauri`・テストの
//! `test_drivers`）: core は `banto-drivers` に依存できない（R2b-0）ので、組み込みの
//! `modbus-tcp` / `slmp`（`banto_drivers::builtin().registry`）に host がこの 3 つを足す。
//! `hub` は Hub のレジストリに載せない（Hub は `hub` 接続を作らない、§4.1）。
//!
//! **R2b-1a では本番の経路はレジストリを読まない**（振る舞いは変わらない）。R2b-1d から
//! Hub の入口（`PlcConnectionService::with_registry`）・収集の計画
//! （`crate::ingest::build_hub_config`）・移行の preflight（`crate::upgrade`）が読む。

use std::sync::Arc;

use banto_driver_api::{DriverEntry, DriverRegistry, RegistryError};

use crate::computed::driver::VirtualDriver;
use crate::db_source::driver::PostgresDriver;
use crate::ingest::driver::PushDriver;

/// `push`・`postgres`・`virtual` を `DriverKind::HostProvided` で（この順）。
pub fn host_provided_drivers() -> Vec<DriverEntry> {
    vec![
        DriverEntry::host_provided(Arc::new(PushDriver::new())),
        DriverEntry::host_provided(Arc::new(PostgresDriver::new())),
        DriverEntry::host_provided(Arc::new(VirtualDriver::new())),
    ]
}

/// [`host_provided_drivers`] を `registry` に足す。同じ id がすでにあれば
/// [`RegistryError::DuplicateId`]（組み込みの id は上書きできない）。
pub fn register_host_provided(registry: &mut DriverRegistry) -> Result<(), RegistryError> {
    for entry in host_provided_drivers() {
        registry.register(entry)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use banto_driver_api::DriverKind;

    #[test]
    fn the_three_are_host_provided_and_registerable() {
        let mut registry = DriverRegistry::new();
        register_host_provided(&mut registry).unwrap();
        let ids: Vec<&str> = registry.ids().map(|id| id.as_str()).collect();
        assert_eq!(ids, ["postgres", "push", "virtual"]);
        for id in ids {
            assert!(matches!(
                registry.get(id).unwrap().kind(),
                DriverKind::HostProvided(_)
            ));
        }
        // 二重の登録は断られる。
        assert!(register_host_provided(&mut registry).is_err());
    }
}
