//! R2b-2b（docs/banto-hub-driver-address-design.md §3・§9）: 本物の Modbus / SLMP のドライバ
//! （`banto_drivers::builtin().registry`）で `TagService::with_registry` がアドレスを正規形で
//! 保存し、文法違反・種類に依る断りをその場で断ることの確認。`banto-tags` 自身のテストは
//! ドライバ crate に依存できない（循環）ので偽のドライバで形を固定しており、ここは同じ事を
//! 実物の文法で確かめる。

use std::sync::Arc;

use banto_core::BantoError;
use banto_tags::{
    migrate, BatchTagOutcome, BatchTagUpdateOutcome, CollectionGroupInput, CollectionGroupService,
    PlcConnectionInput, PlcConnectionService, TagInput, TagService,
};
use sqlx::SqlitePool;

struct Fixture {
    pool: SqlitePool,
    tags: TagService,
    modbus_group: i64,
    slmp_group: i64,
}

async fn fixture() -> Fixture {
    let pool = banto_storage::connect_sqlite_memory().await.unwrap();
    migrate(&pool).await.unwrap();
    let registry = Arc::new(banto_drivers::builtin().registry);
    let connections = PlcConnectionService::with_registry(pool.clone(), registry.clone());
    let groups = CollectionGroupService::with_registry(pool.clone(), registry.clone());
    let mut group_ids = Vec::new();
    for (name, protocol, port) in [("M1", "modbus-tcp", 502), ("S1", "slmp", 5007)] {
        let connection = connections
            .create(PlcConnectionInput {
                name: name.to_string(),
                protocol: protocol.to_string(),
                host: "10.0.0.1".to_string(),
                port,
                unit_id: 1,
                enabled: true,
                simulation: false,
                word_order: "low_high".to_string(),
                cpu_type: None,
                database: None,
                username: None,
                secrets: Default::default(),
                settings: None,
            })
            .await
            .unwrap();
        let group = groups
            .create(CollectionGroupInput {
                name: format!("{name}-g"),
                plc_connection_id: connection.id,
                period_ms: 1_000,
                enabled: true,
                default_writable: true,
                query_sql: None,
            })
            .await
            .unwrap();
        group_ids.push(group.id);
    }
    Fixture {
        tags: TagService::with_registry(pool.clone(), registry),
        pool,
        modbus_group: group_ids[0],
        slmp_group: group_ids[1],
    }
}

fn tag(name: &str, group: i64, address: &str, data_type: &str) -> TagInput {
    TagInput {
        name: name.to_string(),
        collection_group_id: group,
        address: address.to_string(),
        data_type: data_type.to_string(),
        string_length: None,
        string_encoding: "utf8".to_string(),
        raw_lo: None,
        raw_hi: None,
        eng_lo: None,
        eng_hi: None,
        unit: None,
        decimals: 0,
        enabled: true,
        writable: false,
        tag_kind: "plc".to_string(),
        expression: None,
        retain: false,
        expected_revision: None,
    }
}

fn field_errors(err: BantoError) -> Vec<(String, String)> {
    match err {
        BantoError::Validation { field_errors } => field_errors
            .into_iter()
            .map(|e| (e.field, e.message))
            .collect(),
        other => panic!("expected Validation, got {other:?}"),
    }
}

async fn count(pool: &SqlitePool) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM tags")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn modbus_and_slmp_addresses_are_saved_canonical_on_every_write_path() {
    let f = fixture().await;
    let created = f
        .tags
        .create(tag("a", f.modbus_group, " 40001 ", "u16"))
        .await
        .unwrap();
    assert_eq!(created.address, "400001");
    assert_eq!(
        f.tags
            .create(tag("b", f.modbus_group, "40001.03", "bit"))
            .await
            .unwrap()
            .address,
        "400001.3"
    );
    assert_eq!(
        f.tags
            .create(tag("c", f.slmp_group, "d100", "u16"))
            .await
            .unwrap()
            .address,
        "D100"
    );
    assert_eq!(
        f.tags
            .create(tag("d", f.slmp_group, "X0010", "bit"))
            .await
            .unwrap()
            .address,
        "X10"
    );
    // update
    let updated = f
        .tags
        .update(created.id, tag("a", f.modbus_group, "40002", "u16"))
        .await
        .unwrap();
    assert_eq!(updated.address, "400002");
    // transactional + batch paths
    let mut conn = f.pool.acquire().await.unwrap();
    let tx_created = f
        .tags
        .create_tx(&mut conn, tag("e", f.slmp_group, "d0200", "u16"))
        .await
        .unwrap();
    assert_eq!(tx_created.address, "D200");
    let BatchTagOutcome::Valid {
        tags: Some(batch), ..
    } = f
        .tags
        .create_batch_tx(
            &mut conn,
            &[
                tag("f1", f.modbus_group, "40010", "u16"),
                tag("f2", f.slmp_group, "w1a", "u16"),
            ],
        )
        .await
        .unwrap()
    else {
        panic!("expected Valid");
    };
    assert_eq!(batch[0].address, "400010");
    assert_eq!(batch[1].address, "W1A");
    let BatchTagUpdateOutcome::Valid {
        tags: Some(updated),
        ..
    } = f
        .tags
        .update_batch_tx(
            &mut conn,
            &[(batch[0].id, tag("f1", f.modbus_group, "40011", "u16"))],
        )
        .await
        .unwrap()
    else {
        panic!("expected Valid");
    };
    assert_eq!(updated[0].address, "400011");
    drop(conn);
    let BatchTagOutcome::Valid {
        tags: Some(created),
        ..
    } = f
        .tags
        .create_batch(vec![tag("g", f.modbus_group, "40020", "u16")], false)
        .await
        .unwrap()
    else {
        panic!("expected Valid");
    };
    assert_eq!(created[0].address, "400020");
}

#[tokio::test]
async fn grammar_errors_disabled_and_string_tags_are_refused_with_the_drivers_message() {
    let f = fixture().await;
    // A SLMP address under a Modbus connection (and the reverse).
    let errors = field_errors(
        f.tags
            .create(tag("x", f.modbus_group, "D100", "u16"))
            .await
            .unwrap_err(),
    );
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].0, "address");
    assert!(errors[0].1.contains("Modbus"), "{}", errors[0].1);
    // Disabled tags are validated too (Q1).
    let mut disabled = tag("y", f.slmp_group, "40001", "u16");
    disabled.enabled = false;
    assert_eq!(
        field_errors(f.tags.create(disabled).await.unwrap_err())[0].0,
        "address"
    );
    // A Modbus `string` is refused (the driver does not list it in `supported_kinds`,
    // R2b-6b: a `dataType` error), an SLMP word `string` is fine.
    let mut modbus_string = tag("z", f.modbus_group, "40001", "string");
    modbus_string.string_length = Some(4);
    let errors = field_errors(f.tags.create(modbus_string).await.unwrap_err());
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].0, "dataType");
    assert!(
        errors[0].1.contains("string 型を使えません"),
        "{}",
        errors[0].1
    );
    let mut slmp_string = tag("z", f.slmp_group, "d100", "string");
    slmp_string.string_length = Some(4);
    assert_eq!(f.tags.create(slmp_string).await.unwrap().address, "D100");
    // Bit suffix on a word type.
    assert_eq!(
        field_errors(
            f.tags
                .create(tag("w", f.modbus_group, "40001.3", "u16"))
                .await
                .unwrap_err()
        )[0]
        .0,
        "address"
    );
    assert_eq!(count(&f.pool).await, 1);
}

#[tokio::test]
async fn writable_area_and_64_bit_follow_the_drivers() {
    let f = fixture().await;
    let mut input_register = tag("w", f.modbus_group, "30001", "u16");
    input_register.writable = true;
    let errors = field_errors(f.tags.create(input_register.clone()).await.unwrap_err());
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].0, "writable");
    assert!(errors[0].1.contains("読み取り専用領域"), "{}", errors[0].1);
    input_register.writable = false;
    assert_eq!(
        f.tags.create(input_register).await.unwrap().address,
        "300001"
    );
    // #325 kept by the drivers' `supported_kinds` (R2b-6b): every 64-bit type is fine
    // under Modbus and a `dataType` error under SLMP (the protocol-name rule
    // `MODBUS_ONLY_DATA_TYPES` is not consulted with a registry).
    for (i, data_type) in ["i64", "u64"].into_iter().enumerate() {
        let address = format!("4020{i}");
        f.tags
            .create(tag(
                &format!("big-{data_type}"),
                f.modbus_group,
                &address,
                data_type,
            ))
            .await
            .unwrap_or_else(|e| panic!("{data_type} under Modbus: {e:?}"));
    }
    for data_type in ["u64", "f64"] {
        let errors = field_errors(
            f.tags
                .create(tag("big", f.slmp_group, "d200", data_type))
                .await
                .unwrap_err(),
        );
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].0, "dataType");
        assert!(
            errors[0].1.contains(&format!("{data_type} 型を使えません")),
            "{}",
            errors[0].1
        );
    }
    assert_eq!(
        f.tags
            .create(tag("big", f.modbus_group, "40100", "f64"))
            .await
            .unwrap()
            .address,
        "400100"
    );
    let errors = field_errors(
        f.tags
            .create(tag("big", f.slmp_group, "d100", "i64"))
            .await
            .unwrap_err(),
    );
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].0, "dataType");
}

/// Modbus and SLMP grammars do not overlap, so a protocol change with tags is refused
/// with the list, and one without tags (or after the tags are gone) passes.
#[tokio::test]
async fn changing_modbus_to_slmp_with_tags_is_refused_and_changes_nothing() {
    let f = fixture().await;
    let registry = Arc::new(banto_drivers::builtin().registry);
    let connections = PlcConnectionService::with_registry(f.pool.clone(), registry);
    let mut disabled = tag("off", f.modbus_group, "40001", "u16");
    disabled.enabled = false;
    f.tags.create(disabled).await.unwrap();
    let id: i64 = sqlx::query_scalar("SELECT id FROM plc_connections WHERE name = 'M1'")
        .fetch_one(&f.pool)
        .await
        .unwrap();
    let before: Vec<(String, i64)> = sqlx::query_as("SELECT address, revision FROM tags")
        .fetch_all(&f.pool)
        .await
        .unwrap();
    let errors = field_errors(
        connections
            .update(
                id,
                PlcConnectionInput {
                    name: "M1".to_string(),
                    protocol: "slmp".to_string(),
                    host: "10.0.0.1".to_string(),
                    port: 5007,
                    unit_id: 1,
                    enabled: true,
                    simulation: false,
                    word_order: "low_high".to_string(),
                    cpu_type: None,
                    database: None,
                    username: None,
                    secrets: Default::default(),
                    settings: None,
                },
            )
            .await
            .unwrap_err(),
    );
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].0, "protocol");
    assert!(errors[0].1.contains("「off」"), "{}", errors[0].1);
    let after: Vec<(String, i64)> = sqlx::query_as("SELECT address, revision FROM tags")
        .fetch_all(&f.pool)
        .await
        .unwrap();
    assert_eq!(before, after);
    let protocol: String = sqlx::query_scalar("SELECT protocol FROM plc_connections WHERE id = ?")
        .bind(id)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(protocol, "modbus-tcp");
}
