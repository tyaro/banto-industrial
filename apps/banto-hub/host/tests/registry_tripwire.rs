//! トリップワイヤ 3（R2b-1a、docs/banto-hub-connection-settings-design.md §4.1）:
//! Hub のレジストリの id の集合 == Hub の収集が計画できるプロトコルの集合。
//!
//! Hub の収集が計画できるのは、broker が管理するセッション型（`modbus-tcp`・`slmp`。
//! R2b-3a-2 までは `HostBound`）と、
//! host が I/O を持つ `HostProvided`（`push`・`postgres`・`virtual`）。`hub` は
//! ChronoGazer の接続で、Hub は作らない（`reject_hub_connection_protocol`）のでレジストリに
//! 載せない。どれかを足し忘れる・余計に載せると、R2b-1d で検証の正がレジストリになった
//! ときに「保存できるのに計画で外れる」「計画できるのに保存で断る」が起きる。
//!
//! 既存のトリップワイヤ（`crates/banto-drivers/src/lib.rs`）: 1 は
//! `banto_drivers::builtin()` の registry ⊆ `ALLOWED_PROTOCOLS`、2 は R2b-3a-2
//! （docs/banto-hub-driver-io-design.md §4.2）から「`builtin()` の項目はすべて `Frame` か
//! `Session`」（broker の表と `HostBound` が無くなったため）。ここは host が組み立てた
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

#[tokio::test]
async fn the_hub_registry_is_exactly_what_the_hub_collection_can_plan() {
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

    // 種類: broker 管理の 2 つがセッション型（R2b-3a-2。3b 以降は Frame も）、host が
    // I/O を持つ 3 つが HostProvided。
    let dialed = ids_of_kind(registry, |k| {
        matches!(k, DriverKind::Session(_) | DriverKind::Frame(_))
    });
    let host_provided = ids_of_kind(registry, |k| matches!(k, DriverKind::HostProvided(_)));
    assert_eq!(dialed, set(&["modbus-tcp", "slmp"]));
    assert_eq!(
        host_provided,
        set(&[PUSH_PROTOCOL, POSTGRES_PROTOCOL, VIRTUAL_PROTOCOL])
    );
    assert_eq!(dialed.len() + host_provided.len(), registry.len());

    // トリップワイヤ 2 の Hub 側の写し（R2b-3a-2 で書き換え）: Hub の registry の
    // Frame / Session の集合 == broker が管理するプロトコルの集合。
    let sessions = banto_hub_core::broker_glue::HubSessions::new(
        banto_broker::BackoffConfig::default(),
        drivers.clone(),
    );
    for id in &registered {
        assert_eq!(
            sessions.is_broker_managed(id),
            dialed.contains(id),
            "{id}: broker が管理する集合と Frame / Session の集合が食い違う"
        );
        assert_eq!(
            banto_broker::is_broker_managed(registry, id),
            dialed.contains(id)
        );
    }
    sessions.shutdown().await;
}
