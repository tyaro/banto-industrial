//! トリップワイヤ 3（R2b-1a、docs/banto-hub-connection-settings-design.md §4.1）:
//! Hub のレジストリの id の集合 == Hub の収集が計画できるプロトコルの集合。
//!
//! Hub の収集が計画できるのは、broker が管理する `HostBound`（`modbus-tcp`・`slmp`）と、
//! host が I/O を持つ `HostProvided`（`push`・`postgres`・`virtual`）。`hub` は
//! ChronoGazer の接続で、Hub は作らない（`reject_hub_connection_protocol`）のでレジストリに
//! 載せない。どれかを足し忘れる・余計に載せると、R2b-1d で検証の正がレジストリになった
//! ときに「保存できるのに計画で外れる」「計画できるのに保存で断る」が起きる。
//!
//! 既存のトリップワイヤ（`crates/banto-drivers/src/lib.rs`）は変えない: 1 は
//! `banto_drivers::builtin()` の registry ⊆ `ALLOWED_PROTOCOLS`、2 は broker の表 ==
//! `HostBound` の id（この PR は `builtin()` に触らない）。ここは host が組み立てた
//! 後の Hub のレジストリを見る（`tests/common` の `hub_drivers()` は `banto-hub` の bin・
//! `src-tauri` と同じ手順で組み立てる）。

mod common;

use std::collections::BTreeSet;

use banto_driver_api::DriverKind;
use banto_tags::{
    ALLOWED_PROTOCOLS, HUB_PROTOCOL, POSTGRES_PROTOCOL, PUSH_PROTOCOL, VIRTUAL_PROTOCOL,
};

fn ids_of_kind(
    registry: &banto_driver_api::DriverRegistry,
    want: impl Fn(&DriverKind) -> bool,
) -> BTreeSet<String> {
    registry
        .ids()
        .filter(|id| registry.get(id.as_str()).is_some_and(|e| want(e.kind())))
        .map(|id| id.as_str().to_string())
        .collect()
}

fn set(ids: &[&str]) -> BTreeSet<String> {
    ids.iter().map(|s| s.to_string()).collect()
}

#[test]
fn the_hub_registry_is_exactly_what_the_hub_collection_can_plan() {
    let drivers = common::hub_drivers();
    let registry = &drivers.registry;

    // 収集が計画できる集合: 旧い規則の一覧から、Hub が作らない `hub` を除いたもの。
    let plannable: BTreeSet<String> = ALLOWED_PROTOCOLS
        .iter()
        .filter(|p| **p != HUB_PROTOCOL)
        .map(|p| p.to_string())
        .collect();
    assert_eq!(
        plannable,
        set(&["modbus-tcp", "slmp", "postgres", "push", "virtual"]),
        "ALLOWED_PROTOCOLS が変わった: Hub の収集が計画できる集合を見直す"
    );

    let registered: BTreeSet<String> = registry.ids().map(|id| id.as_str().to_string()).collect();
    assert_eq!(registered, plannable);
    assert!(!registry.contains(HUB_PROTOCOL), "hub は Hub に載せない");

    // 種類: broker 管理の 2 つが HostBound、host が I/O を持つ 3 つが HostProvided。
    let host_bound = ids_of_kind(registry, |k| matches!(k, DriverKind::HostBound(_)));
    let host_provided = ids_of_kind(registry, |k| matches!(k, DriverKind::HostProvided(_)));
    assert_eq!(host_bound, set(&["modbus-tcp", "slmp"]));
    assert_eq!(
        host_provided,
        set(&[PUSH_PROTOCOL, POSTGRES_PROTOCOL, VIRTUAL_PROTOCOL])
    );
    assert_eq!(host_bound.len() + host_provided.len(), registry.len());

    // broker が管理するのは HostBound だけ（トリップワイヤ 2 の Hub 側の写し）。
    for id in &registered {
        assert_eq!(
            drivers.broker.is_supported(id),
            host_bound.contains(id),
            "{id}: broker の表と HostBound が食い違う"
        );
    }
}
