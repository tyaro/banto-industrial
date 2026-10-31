//! #682 PR-H: catalog / values 応答の `map` と `revision` が同じ世代であること。
//!
//! `CollectorManager::commit_catalog` は map と revision を 1 つの lock の中で
//! 更新する。応答側がこの 2 つを別々の lock で読むと、その間に commit が挟まって
//! （旧 map, 新 revision）の「裂けた読み」になる。ここでは probe タグの無い
//! snapshot（A）と有る snapshot（B）を交互に commit し、同時に応答を組み立て続けて、
//! 応答の `revision` の偶奇と probe タグの有無が必ず一致することを確かめる。

use super::super::*;
use super::common::test_manager_with_clock;
use super::general::modbus_flat_input;
use crate::db::migrate_memory;
use banto_tstore::SystemClock;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

const PROBE: &str = "line-rev.g1.probe";
const COMMITS: u64 = 4_000;
const READERS: usize = 3;

async fn create_tag(pool: &sqlx::SqlitePool, group_id: i64, name: &str, address: &str) {
    TagService::new(pool.clone())
        .create(
            serde_json::from_value::<TagPayload>(json!({
                "name": name,
                "collectionGroupId": group_id,
                "address": address,
                "dataType": "i16",
            }))
            .unwrap()
            .into(),
        )
        .await
        .unwrap();
}

/// 応答の `revision` が偶数（B を commit した世代）なら probe タグが必ず含まれ、
/// 奇数（A の世代）なら含まれないことを、catalog と values の両方で確かめる。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn catalog_and_values_responses_never_pair_an_old_map_with_a_new_revision() {
    let pool = migrate_memory().await.expect("migrate_memory");
    let (manager, _dir) = test_manager_with_clock(pool.clone(), Arc::new(SystemClock));
    let controller = Arc::new(crate::controller::CollectionController::new(
        manager.clone(),
    ));

    let conn = PlcConnectionService::new(pool.clone())
        .create(modbus_flat_input("line-rev", 15061))
        .await
        .unwrap();
    let group = CollectionGroupService::new(pool.clone())
        .create(
            serde_json::from_value::<CollectionGroupPayload>(json!({
                "name": "g1",
                "plcConnectionId": conn.id,
                "periodMs": 1000,
            }))
            .unwrap()
            .into(),
        )
        .await
        .unwrap();
    create_tag(&pool, group.id, "base", "40001").await;
    let without_probe = RegistrySnapshot::load(&pool).await.unwrap();
    create_tag(&pool, group.id, "probe", "40002").await;
    let with_probe = RegistrySnapshot::load(&pool).await.unwrap();

    // 世代 1 = A（probe 無し）。以後 B, A, B, ... なので偶数 = probe 有り。
    assert_eq!(manager.commit_catalog(&without_probe).await.unwrap(), 1);

    let state = TagSpaceState {
        manager: manager.clone(),
        controller,
        write_control: Arc::new(crate::write_control::WriteControl::new(false)),
        mqtt: Arc::new(crate::mqtt::MqttPublisher::new(manager.clone())),
        system_info: Arc::new(SystemInfoSampler::new()),
        sink_status: SinkStatusStore::new(),
        computed: manager.computed_engine(),
        audit: AuditLogService::new(Db::Sqlite(pool.clone())),
    };

    let stop = Arc::new(AtomicBool::new(false));
    let samples = Arc::new(AtomicUsize::new(0));
    let mut readers = Vec::new();
    for _ in 0..READERS {
        let state = state.clone();
        let stop = stop.clone();
        let samples = samples.clone();
        readers.push(tokio::task::spawn_blocking(move || {
            let tags_query: TagsQuery = serde_json::from_value(json!({})).unwrap();
            let values_query: ValuesQuery = serde_json::from_value(json!({})).unwrap();
            let mut torn = Vec::new();
            while !stop.load(Ordering::Relaxed) {
                let catalog = build_catalog_response(&state, &tags_query);
                let has_probe = catalog
                    .tags
                    .iter()
                    .any(|tag| tag.entry.external_name == PROBE);
                if has_probe != catalog.revision.is_multiple_of(2) {
                    torn.push(("catalog", catalog.revision, has_probe));
                }

                // `v1_values` / `admin_values` と同じ組み立て。
                let (map, revision) = state.manager.catalog_with_revision();
                let runtime = state.controller.status();
                let names = resolve_value_names(&map, &values_query).unwrap();
                let values = build_values_response(&state, &map, revision, &runtime, names);
                let has_probe = serde_json::to_value(&values.values)
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|value| value["tag"] == PROBE);
                if has_probe != values.revision.is_multiple_of(2) {
                    torn.push(("values", values.revision, has_probe));
                }
                samples.fetch_add(1, Ordering::Relaxed);
            }
            torn
        }));
    }

    for expected in 2..=COMMITS + 1 {
        let snapshot = if expected.is_multiple_of(2) {
            &with_probe
        } else {
            &without_probe
        };
        assert_eq!(manager.commit_catalog(snapshot).await.unwrap(), expected);
    }
    stop.store(true, Ordering::Relaxed);

    let mut torn = Vec::new();
    for reader in readers {
        torn.extend(reader.await.unwrap());
    }
    assert!(
        samples.load(Ordering::Relaxed) > 0,
        "readers must have built at least one response"
    );
    assert!(
        torn.is_empty(),
        "{} torn responses (kind, revision, has_probe), first: {:?}",
        torn.len(),
        torn.iter().take(5).collect::<Vec<_>>()
    );
}
