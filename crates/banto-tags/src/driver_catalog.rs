//! Shared driver catalog for Hub and ChronoGazer. Registration order is preserved.
use banto_driver_api::{
    Capabilities, DriverEntry, DriverKind, DriverRegistry, KindName, SettingsSchema, Transport,
};
use serde::Serialize;

/// 一覧の 1 行。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DriverListItem {
    id: String,
    display_name: String,
    vendor: Option<String>,
    version: String,
    /// `{ "read", "write" }`（`banto-driver-api` の `serde` の形）。
    capabilities: Capabilities,
    transports: Vec<Transport>,
    settings_schema: SettingsSchema,
    supported_kinds: Vec<KindName>,
    address_help: String,
    /// `"frame"` / `"session"`（`"host"` はこの一覧に載らない。設計 §4.1 の語彙）。
    kind: &'static str,
}

/// `entry` の一覧の行。ホストの接続（と、このホストが知らない種類）は `None`。
fn list_item(entry: &DriverEntry) -> Option<DriverListItem> {
    let kind = match entry.kind() {
        DriverKind::Frame(_) => "frame",
        DriverKind::Session(_) => "session",
        // `HostProvided` と、`#[non_exhaustive]` の後の版で増えた種類。
        _ => return None,
    };
    let base = entry.base();
    let descriptor = base.descriptor();
    Some(DriverListItem {
        id: descriptor.id.to_string(),
        display_name: descriptor.display_name.clone(),
        vendor: descriptor.vendor.clone(),
        version: descriptor.version.clone(),
        capabilities: descriptor.capabilities,
        transports: descriptor.transports.clone(),
        settings_schema: base.settings_schema(),
        supported_kinds: base.supported_kinds(),
        address_help: base.address_help(),
        kind,
    })
}

/// レジストリの順に、機器のドライバだけ。
pub fn driver_list(registry: &DriverRegistry) -> Vec<DriverListItem> {
    registry
        .ids()
        .filter_map(|id| registry.get(id.as_str()))
        .filter_map(list_item)
        .collect()
}
