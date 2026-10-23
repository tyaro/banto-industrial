//! `GET /api/drivers`（R2b-6b、docs/banto-hub-driver-ui-design.md §4.1）: Hub のドライバの
//! レジストリ（`CollectorManager::registry()`。保存・アドレスの解析と同じもの）に載っている
//! **機器のドライバ**の一覧。汎用の接続フォーム（設定の欄 = `settingsSchema`）とタグの画面
//! （データ型の選択肢 = `supportedKinds`、アドレスの書き方 = `addressHelp`）が、プロトコルの
//! 名前の分岐と TS の写しの代わりに読む。
//!
//! - **ホストの接続（`DriverKind::HostProvided`: `postgres`・`push`・`virtual`）は載せない**。
//!   フォームは専用で（設計 §6.2）、UI は「この一覧 + ホストの接続」をまとめて選択肢にする。
//! - 並びはレジストリの順（`DriverRegistry::ids()`。レジストリは id の順で持ち、登録の順は
//!   持たない）。
//! - 形: 外側は camelCase。`settingsSchema` と `supportedKinds` は `banto-driver-api` の
//!   `serde` の形そのまま（`kind` は `{"type": "integer", "min", "max"}` のようなタグつき、
//!   `group` は `"basic"` / `"advanced"`、`supportedKinds` は `"u16"`・`"string"` の文字列）。
//!   UI がそのまま読むので、形は `banto-hub-host` の統合テストで固定する。
//!
//! `/api/tags` の読み取りと同じく副作用が無いので、`tag_registry_router` の `require_session`
//! （viewer 以上）だけで `require_editor` は呼ばない。`/api/tags/parse-addresses` と同じ理由で
//! `#[utoipa::path]` を付けず `ApiDoc` にも加えない（`/api/v1/*` だけを文書化する既存方針）。

use banto_driver_api::{
    Capabilities, DriverEntry, DriverKind, DriverRegistry, KindName, SettingsSchema, Transport,
};

use super::{Json, Serialize, State, TagRegistryState};

/// 一覧の 1 行。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DriverListItem {
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
pub(super) fn driver_list(registry: &DriverRegistry) -> Vec<DriverListItem> {
    registry
        .ids()
        .filter_map(|id| registry.get(id.as_str()))
        .filter_map(list_item)
        .collect()
}

/// `GET /api/drivers`（モジュールの doc）。
pub(super) async fn drivers_list(
    State(state): State<TagRegistryState>,
) -> Json<Vec<DriverListItem>> {
    Json(driver_list(state.manager.registry()))
}
