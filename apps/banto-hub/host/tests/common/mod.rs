//! Shared test scaffolding for `banto-hub-host`'s integration tests
//! (`tests/*.rs`, pulled in via `mod common;` - the standard
//! `tests/common/mod.rs` pattern).
//!
//! R2b-0 (docs/banto-driver-api-design.md §2.4): the integration tests that
//! run real SLMP / Modbus sessions through the hub (simulators, the
//! one-socket check, broker routing) moved here from
//! `apps/banto-hub/core/tests/`, because `banto-hub-core` no longer depends on
//! `banto-drivers` at all. They share `TempEnv` / `shutdown_test_app` with the
//! tests that stayed in core, so this module includes core's
//! `tests/common/mod.rs` as is (one copy - see its module doc for why it
//! exists and the leak it fixes) and adds the one thing core cannot have: the
//! built-in drivers.
//!
//! A change to that file is a change under `apps/banto-hub/core`, which
//! already selects this package in CI (`banto-hub-host` depends on core).
#![allow(dead_code)]

#[path = "../../../core/tests/common/mod.rs"]
mod core_common;

#[allow(unused_imports)] // not every test binary uses every helper
pub use core_common::*;

/// The built-in drivers as the `HubDrivers` an integration test injects into
/// `HubSessions::new` / `HubConfig` (docs/banto-driver-api-design.md §6.4).
pub fn hub_drivers() -> banto_hub_core::broker_glue::HubDrivers {
    let builtin = banto_drivers::builtin();
    // R2b-1a: the host-provided protocols join the built-ins, as a host does.
    let mut registry = builtin.registry;
    banto_hub_core::register_host_provided(&mut registry)
        .expect("host-provided driver ids are distinct from the built-ins");
    banto_hub_core::broker_glue::HubDrivers::new(std::sync::Arc::new(registry))
}
