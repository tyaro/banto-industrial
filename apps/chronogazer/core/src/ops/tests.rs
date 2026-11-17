//! `crate::ops` を MCP から呼ぶ想定の単体テスト（#1161 PR5a）。REST からの
//! 挙動は `crate::rest` のテストが確かめる（PR5a で変えていない）。

use std::path::PathBuf;
use std::sync::Arc;

use banto_tags::{CollectionGroupService, PlcConnectionService, TagService};
use serde_json::{json, Value};

use super::*;
use crate::api_keys::KeyScopes;
use crate::db::{migrate_memory, Db};
use crate::settings::SettingsService;

struct Fixture {
    pool: sqlx::SqlitePool,
    registry: RegistryOps,
    display_groups: DisplayGroupOps,
    thresholds: ThresholdOps,
    collect: CollectOps,
}

async fn fixture() -> Fixture {
    let pool = migrate_memory().await.expect("migrate_memory");
    let drivers = crate::test_support::drivers();
    let audit = AuditLogService::new(Db::Sqlite(pool.clone()));
    let hub = HubService::new(
        SettingsService::new(Db::Sqlite(pool.clone())),
        Arc::new(crate::hub::UnavailableKeyStore),
        crate::hub_tls::HubTlsPolicy::plain_loopback_http_for_dev(),
    )
    .await
    .expect("HubService::new");
    // `registry_changed` が登録済みの hub タグを読み直せるように（本番と同じ）。
    hub.attach_registry(pool.clone());
    let display_groups = DisplayGroupService::new(pool.clone());
    Fixture {
        registry: RegistryOps {
            plc_connections: PlcConnectionService::with_registry(pool.clone(), drivers.clone())
                .accept_hub_connections(),
            collection_groups: CollectionGroupService::with_registry(pool.clone(), drivers.clone())
                .accept_hub_connections(),
            tags: TagService::with_registry(pool.clone(), drivers.clone()).accept_hub_connections(),
            display_groups: display_groups.clone(),
            hub,
            audit: audit.clone(),
            drivers: drivers.clone(),
        },
        display_groups: DisplayGroupOps {
            display_groups,
            audit: audit.clone(),
        },
        thresholds: ThresholdOps {
            tag_thresholds: TagThresholdService::new(pool.clone()),
            audit: audit.clone(),
        },
        collect: CollectOps {
            // 登録が空なので `start` は `NoTargets` で止まり、このディレクトリは触らない。
            collect: CollectorService::new(pool.clone(), PathBuf::from("unused-in-tests"), drivers),
            audit,
        },
        pool,
    }
}

fn key(scope: &str) -> Actor {
    Actor::ApiKey {
        id: 1,
        label: "agent（cg_abcd）".to_owned(),
        scopes: KeyScopes::from_strings(&[scope.to_owned()]),
    }
}

/// `(action, resource, entity_id, detail, origin, result, actor_role)` を id 順に。
async fn audit_rows(
    pool: &sqlx::SqlitePool,
) -> Vec<(
    String,
    String,
    Option<String>,
    Option<String>,
    String,
    String,
    String,
)> {
    sqlx::query_as(
        "SELECT action, resource, entity_id, detail, origin, result, actor_role \
         FROM audit_log ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

fn detail(raw: &Option<String>) -> Value {
    serde_json::from_str(raw.as_deref().expect("detail")).expect("detail json")
}

fn connection_payload(name: &str) -> PlcConnectionPayload {
    serde_json::from_value(json!({
        "name": name,
        "protocol": "modbus-tcp",
        "enabled": true,
        "settings": {"host": "192.168.11.200", "port": 502}
    }))
    .unwrap()
}

/// MCP の呼び出しは `origin: "mcp"` で残り、action・resource・entity_id・
/// detail は REST と同じ値になる。
#[tokio::test]
async fn mcp_config_key_writes_are_audited_with_the_mcp_origin() {
    let f = fixture().await;
    let actor = key("config");
    let created = f
        .registry
        .create_connection(
            &actor,
            &Via::Mcp {
                tool: "create_connection",
            },
            connection_payload("plc-1"),
        )
        .await
        .expect("create_connection");
    let group: CollectionGroupPayload = serde_json::from_value(
        json!({ "name": "g", "plcConnectionId": created.id, "periodMs": 1000 }),
    )
    .unwrap();
    let group = f
        .registry
        .create_group(
            &actor,
            &Via::Mcp {
                tool: "create_group",
            },
            group,
        )
        .await
        .expect("create_group");
    f.registry
        .delete_group(
            &actor,
            &Via::Mcp {
                tool: "delete_group",
            },
            group.id,
        )
        .await
        .expect("delete_group");

    let rows = audit_rows(&f.pool).await;
    assert_eq!(rows.len(), 3, "{rows:?}");
    let (action, resource, entity_id, raw, origin, result, role) = &rows[0];
    assert_eq!(
        (
            action.as_str(),
            resource.as_str(),
            origin.as_str(),
            result.as_str()
        ),
        ("create", "plc_connections", "mcp", "ok")
    );
    assert_eq!(entity_id.as_deref(), Some(created.id.to_string().as_str()));
    assert_eq!(role, "api_key");
    assert_eq!(detail(raw)["name"], "plc-1");
    assert_eq!(
        (rows[1].0.as_str(), rows[1].1.as_str(), rows[1].4.as_str()),
        ("create", "collection_groups", "mcp")
    );
    assert_eq!(
        (rows[2].0.as_str(), rows[2].1.as_str(), rows[2].4.as_str()),
        ("delete", "collection_groups", "mcp")
    );
}

/// `read` のキーは config の操作を断られ、`denied`（`origin: "mcp"`、
/// `detail: {tool}`）が残る。サービスは呼ばれない。
#[tokio::test]
async fn read_key_is_forbidden_from_config_operations_and_denied_is_audited() {
    let f = fixture().await;
    let actor = key("read");

    let err = f
        .registry
        .create_connection(
            &actor,
            &Via::Mcp {
                tool: "create_connection",
            },
            connection_payload("plc-1"),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, BantoError::Forbidden), "{err:?}");
    assert!(f.registry.list_connections().await.unwrap().is_empty());

    let err = f
        .display_groups
        .reorder(
            &actor,
            &Via::Mcp {
                tool: "reorder_display_groups",
            },
            &[],
        )
        .await
        .unwrap_err();
    assert!(matches!(err, BantoError::Forbidden), "{err:?}");

    let payload: TagThresholdsPayload = serde_json::from_value(json!({})).unwrap();
    let err = f
        .thresholds
        .update(
            &actor,
            &Via::Mcp {
                tool: "set_threshold",
            },
            1,
            &payload,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, BantoError::Forbidden), "{err:?}");

    let err = f
        .collect
        .run(
            &actor,
            &Via::Mcp {
                tool: "start_collection",
            },
            CollectOperation::Start,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, BantoError::Forbidden), "{err:?}");

    let rows = audit_rows(&f.pool).await;
    let summary: Vec<(&str, &str, &str, &str, Value)> = rows
        .iter()
        .map(|(action, resource, _, raw, origin, result, _)| {
            (
                action.as_str(),
                resource.as_str(),
                origin.as_str(),
                result.as_str(),
                detail(raw),
            )
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            (
                "denied",
                "plc_connections",
                "mcp",
                "denied",
                json!({ "tool": "create_connection" })
            ),
            (
                "denied",
                "display_groups",
                "mcp",
                "denied",
                json!({ "tool": "reorder_display_groups" })
            ),
            (
                "denied",
                "tag_thresholds",
                "mcp",
                "denied",
                json!({ "tool": "set_threshold" })
            ),
            (
                "denied",
                "collect",
                "mcp",
                "denied",
                json!({ "tool": "start_collection" })
            ),
        ]
    );
}

/// 収集の操作は `config` のキーで通り、結果の状態が `origin: "mcp"` で残る。
#[tokio::test]
async fn config_key_operates_collection_through_the_shared_function() {
    let f = fixture().await;
    let outcome = f
        .collect
        .run(
            &key("config"),
            &Via::Mcp {
                tool: "start_collection",
            },
            CollectOperation::Start,
        )
        .await
        .expect("start");
    let rows = audit_rows(&f.pool).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    let (action, resource, _, raw, origin, result, _) = &rows[0];
    assert_eq!(
        (
            action.as_str(),
            resource.as_str(),
            origin.as_str(),
            result.as_str()
        ),
        ("start", "collect", "mcp", "ok")
    );
    assert_eq!(
        detail(raw),
        json!({ "collectState": outcome.status.as_str(), "pending": outcome.pending })
    );
}

/// 記録項目を消すと `registry_changed` が呼ばれ、Hub の購読の登録から外れる
/// （MCP だけ `registry_changed` を忘れる、が起きないこと）。
///
/// 反証: `RegistryOps::delete_tag` の `self.hub.registry_changed()` を消すと、
/// 購読に `L.G.a` が残ってここで落ちる。
#[tokio::test]
async fn deleting_a_tag_calls_registry_changed() {
    let f = fixture().await;
    let row = |external_name: &str, name: &str| crate::hub_tags::HubTagRow {
        external_name: external_name.to_owned(),
        name: name.to_owned(),
        data_type: "f32".to_owned(),
        unit: None,
        decimals: None,
    };
    let registered = crate::hub_tags::register_from_hub(
        &f.pool,
        &f.registry.plc_connections,
        &f.registry.collection_groups,
        &f.registry.tags,
        crate::hub_tags::HubTagRegistration {
            collection_group_id: None,
            new_group: Some(crate::hub_tags::NewHubGroup {
                name: "g".to_owned(),
                period_ms: 1000,
            }),
            tags: vec![row("L.G.a", "a"), row("L.G.b", "b")],
        },
    )
    .await
    .unwrap();
    assert!(registered.ok);
    // 登録を購読に反映しておく（`register_from_hub` 自体は呼ばない）。
    f.registry.hub.registry_changed().await;
    assert_eq!(f.registry.hub.subscription().await.registered.len(), 2);

    let a = registered
        .tags
        .iter()
        .find(|tag| tag.name == "a")
        .expect("tag a");
    f.registry
        .delete_tag(&key("config"), &Via::Mcp { tool: "delete_tag" }, a.id)
        .await
        .expect("delete_tag");
    assert_eq!(
        f.registry.hub.subscription().await.registered,
        vec!["L.G.b".to_owned()]
    );
    let rows = audit_rows(&f.pool).await;
    let last = rows.last().expect("audit");
    assert_eq!(
        (last.0.as_str(), last.1.as_str(), last.4.as_str()),
        ("delete", "tags", "mcp")
    );
}

/// REST の `Via` は今までどおり `origin: "rest"`・`{method, path}`。
#[test]
fn rest_via_keeps_the_rest_origin_and_method_path_detail() {
    let via = Via::Rest {
        method: "PUT",
        path: "/api/tags/{id}",
    };
    assert_eq!(via.origin(), "rest");
    assert_eq!(
        via.denied_detail(),
        json!({ "method": "PUT", "path": "/api/tags/{id}" })
    );
    let via = Via::Mcp { tool: "update_tag" };
    assert_eq!(via.origin(), "mcp");
    assert_eq!(via.denied_detail(), json!({ "tool": "update_tag" }));
}
