//! R2b-1d（docs/banto-hub-connection-settings-design.md §6.3・§9「予行と preflight」）:
//! 本物のドライバ（`banto_drivers::builtin()` + host-provided、`tests/common::hub_drivers()`
//! と bin・shell と同じ組み立て）で、alpha.33 の形の DB を `HubRuntime::start` に渡す。
//!
//! - 0024 までの `CHECK` は通るが、ドライバの検証に落ちる行（`modbus-tcp` の port 70000・
//!   `slmp` の host が空）があると、起動を断る（`HubStartError::ConnectionSettingsPreflight`）。
//!   元の DB はバイト単位で変わらず、予行の写しも、バックアップも残らない。
//! - `hub` の行は報告だけで、移行は進む（起動できる。`.bak` ができ、0025 が当たる）。

mod common;

use std::path::{Path, PathBuf};

use banto_hub_core::profile_lock::HubHostKind;
use banto_hub_core::runtime::{HubConfig, HubRuntime, HubStartError};
use sqlx::SqlitePool;

/// alpha.33 の形の DB（Hub の migrator 全部 + banto-tags 0001〜0024）を作る。
async fn old_db(path: &Path) -> SqlitePool {
    old_db_at(path, 24).await
}

/// Hub の migrator 全部 + banto-tags 0001〜`tags_version` の DB（alpha.32 は 0022 まで）。
async fn old_db_at(path: &Path, tags_version: i64) -> SqlitePool {
    // WAL, like every Hub DB (`banto_storage::connect_sqlite`): a DB in another journal
    // mode would have its header rewritten by the Hub's ordinary connect (2-a), which is
    // not what this test is about.
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);
    let pool = SqlitePool::connect_with(options).await.expect("connect");
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let mut hub =
        sqlx::migrate::Migrator::new(root.join("apps/banto-hub/core/migrations-sqlite").as_path())
            .await
            .expect("hub migrations");
    hub.dangerous_set_table_name(banto_hub_core::db::MIGRATIONS_TABLE);
    hub.run(&pool).await.expect("run hub migrations");
    let mut tags =
        sqlx::migrate::Migrator::new(root.join("crates/banto-tags/migrations").as_path())
            .await
            .expect("banto-tags migrations");
    tags.dangerous_set_table_name(banto_tags::MIGRATIONS_TABLE);
    tags.run_to(tags_version, &pool)
        .await
        .expect("banto-tags up to the old version");
    pool
}

async fn insert_old(pool: &SqlitePool, values: &str) {
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "INSERT INTO plc_connections (id, name, protocol, host, port, unit_id, enabled, \
         simulation, word_order, database, username, password, cpu_type, settings) \
         VALUES {values}"
    )))
    .execute(pool)
    .await
    .unwrap_or_else(|err| panic!("seed failed ({values}): {err}"));
}

async fn seed_valid(pool: &SqlitePool) {
    insert_old(
        pool,
        "(1, 'M', 'modbus-tcp', '10.0.0.1', 502, 1, 1, 0, 'high_low', NULL, NULL, NULL, 'R', NULL)",
    )
    .await;
    insert_old(
        pool,
        "(2, 'pg', 'postgres', '10.0.0.50', 5432, 1, 1, 0, 'low_high', 'erp', 'reader', \
         'TOPSECRET-host-test', 'R', NULL)",
    )
    .await;
}

fn config(db_path: &Path, data_dir: PathBuf) -> HubConfig {
    HubConfig {
        drivers: common::hub_drivers(),
        db_path: db_path.to_string_lossy().into_owned(),
        allow_setup: false,
        port_override: Some(0),
        bind_override: Some("127.0.0.1".to_string()),
        data_dir_override: Some(data_dir),
        profile_id: "upgrade-0025-test".to_string(),
        host_kind: HubHostKind::Console,
        skip_profile_lock: true,
    }
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| !n.ends_with("-wal") && !n.ends_with("-shm"))
        .collect();
    names.sort();
    names
}

fn file_bytes(path: &Path) -> Vec<u8> {
    // No sha2 in this package's dev-dependencies: the bytes themselves are compared.
    std::fs::read(path).expect("read the DB file")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_startup_is_refused_for_rows_the_real_drivers_reject_and_the_db_is_untouched() {
    let env = common::TempEnv::new("banto-hub-upgrade-0025", "refused");
    let config_dir = env.registry_path().parent().unwrap().join("config");
    std::fs::create_dir_all(&config_dir).unwrap();
    let db_path = config_dir.join("banto-hub.sqlite3");
    let pool = old_db(&db_path).await;
    seed_valid(&pool).await;
    // Within 0024's CHECKs (port and host have none), refused by the drivers.
    insert_old(
        &pool,
        "(3, 'M-bad-port', 'modbus-tcp', '10.0.0.2', 70000, 1, 1, 0, 'high_low', NULL, NULL, \
         NULL, 'R', NULL)",
    )
    .await;
    insert_old(
        &pool,
        "(4, 'S-no-host', 'slmp', '', 5007, 1, 1, 0, 'low_high', NULL, NULL, NULL, 'Q', NULL)",
    )
    .await;
    sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
    let before = file_bytes(&db_path);
    let files_before = names(&config_dir);

    let err = match HubRuntime::start(config(&db_path, env.data_dir())).await {
        Ok(running) => {
            running.shutdown().await;
            panic!("the start must be refused");
        }
        Err(err) => err,
    };
    match &err {
        HubStartError::ConnectionSettingsPreflight { rows, problems, .. } => {
            let ids: Vec<i64> = rows.iter().map(|row| row.id).collect();
            assert_eq!(ids, vec![3, 4], "{rows:?}");
            assert!(
                rows[0]
                    .errors
                    .iter()
                    .any(|e| e.starts_with("settings.port")),
                "{rows:?}"
            );
            assert!(
                rows[1]
                    .errors
                    .iter()
                    .any(|e| e.starts_with("settings.host")),
                "{rows:?}"
            );
            assert!(problems.is_empty(), "{problems:?}");
        }
        other => panic!("expected ConnectionSettingsPreflight, got {other:?}"),
    }
    let message = err.to_string();
    assert!(
        message.contains("M-bad-port") && message.contains("S-no-host"),
        "{message}"
    );
    assert!(message.contains("旧版"), "{message}");
    assert!(!message.contains("TOPSECRET"), "{message}");

    assert!(
        file_bytes(&db_path) == before,
        "the DB file must not change"
    );
    assert_eq!(
        names(&config_dir),
        files_before,
        "no dry-run copy, no backup left behind"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hub_row_is_reported_only_and_the_migration_proceeds() {
    let env = common::TempEnv::new("banto-hub-upgrade-0025", "hub-row");
    let config_dir = env.registry_path().parent().unwrap().join("config");
    std::fs::create_dir_all(&config_dir).unwrap();
    let db_path = config_dir.join("banto-hub.sqlite3");
    let pool = old_db(&db_path).await;
    seed_valid(&pool).await;
    insert_old(
        &pool,
        "(5, 'Hub', 'hub', '', 0, 1, 0, 0, 'low_high', NULL, NULL, NULL, 'R', NULL)",
    )
    .await;
    pool.close().await;

    let running = HubRuntime::start(config(&db_path, env.data_dir()))
        .await
        .expect("a hub row does not refuse the start");
    running.shutdown().await;

    let backups: Vec<String> = names(&config_dir)
        .into_iter()
        .filter(|n| n.starts_with("banto-hub.sqlite3.before-0025-") && n.ends_with(".bak"))
        .collect();
    assert_eq!(backups.len(), 1, "{:?}", names(&config_dir));
    let backup = std::fs::read(config_dir.join(&backups[0])).unwrap();
    assert!(
        !backup
            .windows("TOPSECRET-host-test".len())
            .any(|w| w == b"TOPSECRET-host-test"),
        "the backup holds no password"
    );
    assert!(
        !names(&config_dir)
            .iter()
            .any(|n| n.contains(".partial") || n.starts_with(".banto-hub-0025-dryrun-")),
        "{:?}",
        names(&config_dir)
    );

    // Migrated: the hub row is kept (with `{}` settings), the postgres password is a
    // reference to a sealed row.
    let pool =
        SqlitePool::connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&db_path))
            .await
            .unwrap();
    let applied: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT COUNT(*) FROM {} WHERE version = 25",
        banto_tags::MIGRATIONS_TABLE
    )))
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(applied, 1);
    let hub: String = sqlx::query_scalar("SELECT settings FROM plc_connections WHERE id = 5")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(hub, "{}");
    let secrets: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM plc_connection_secrets")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(secrets, 1);
    pool.close().await;
}

/// Opens `db_path` after a start and returns `(0025 applied, settings by id, secret rows)`.
async fn migrated_state(db_path: &Path) -> (i64, Vec<(i64, String)>, i64) {
    let pool =
        SqlitePool::connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(db_path))
            .await
            .unwrap();
    let applied: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT COUNT(*) FROM {} WHERE version = 25",
        banto_tags::MIGRATIONS_TABLE
    )))
    .fetch_one(&pool)
    .await
    .unwrap();
    let settings: Vec<(i64, String)> =
        sqlx::query_as("SELECT id, settings FROM plc_connections ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
    let secrets: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM plc_connection_secrets")
        .fetch_one(&pool)
        .await
        .unwrap();
    pool.close().await;
    (applied, settings, secrets)
}

fn backups_in(config_dir: &Path) -> Vec<String> {
    names(config_dir)
        .into_iter()
        .filter(|n| n.starts_with("banto-hub.sqlite3.before-0025-") && n.ends_with(".bak"))
        .collect()
}

/// PR #777 のオーナーレビュー P2: alpha.32 の DB（banto-tags は 0022 まで。`settings`（0023）
/// がまだ無い）でも、予行が写しを先に 0024 まで進めてから旧い値を読むので、起動の全体が
/// 通って移行される（予行が `no such column: settings` で断らない）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_alpha32_db_before_0023_upgrades_through_the_full_startup() {
    let env = common::TempEnv::new("banto-hub-upgrade-0025", "alpha32");
    let config_dir = env.registry_path().parent().unwrap().join("config");
    std::fs::create_dir_all(&config_dir).unwrap();
    let db_path = config_dir.join("banto-hub.sqlite3");
    let pool = old_db_at(&db_path, 22).await;
    // 0022 の表の列（`settings` は無い。`cpu_type` は 0021 から）。
    sqlx::raw_sql(
        "INSERT INTO plc_connections (id, name, protocol, host, port, unit_id, enabled, \
         simulation, word_order, database, username, password, cpu_type) VALUES \
         (1, 'M', 'modbus-tcp', '10.0.0.1', 502, 1, 1, 0, 'high_low', NULL, NULL, NULL, 'R'), \
         (2, 'pg', 'postgres', '10.0.0.50', 5432, 1, 1, 0, 'low_high', 'erp', 'reader', \
          'TOPSECRET-host-test', 'R')",
    )
    .execute(&pool)
    .await
    .expect("seed the alpha.32 rows");
    pool.close().await;

    let running = HubRuntime::start(config(&db_path, env.data_dir()))
        .await
        .expect("an alpha.32 DB upgrades");
    running.shutdown().await;

    let backups = backups_in(&config_dir);
    assert_eq!(backups.len(), 1, "{:?}", names(&config_dir));
    let backup = std::fs::read(config_dir.join(&backups[0])).unwrap();
    assert!(
        !backup
            .windows("TOPSECRET-host-test".len())
            .any(|w| w == b"TOPSECRET-host-test"),
        "the backup holds no password"
    );
    let (applied, settings, secrets) = migrated_state(&db_path).await;
    assert_eq!(applied, 1);
    // The start also adds its own reserved connections after ids 1 and 2.
    assert!(settings.len() >= 2, "{settings:?}");
    assert_eq!((settings[0].0, settings[1].0), (1, 2), "{settings:?}");
    assert!(settings[0].1.contains("10.0.0.1"), "{settings:?}");
    assert!(settings[1].1.contains("$secret"), "{settings:?}");
    assert!(!settings[1].1.contains("TOPSECRET"), "{settings:?}");
    assert_eq!(secrets, 1);
}

/// さらに古い DB（banto-tags 0013 まで: `password`（0014）も `cpu_type`（0021）も無い）でも、
/// 予行・バックアップ（`password` の列が無ければ除くものが無い）・移行が通る。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_db_before_0014_without_a_password_column_upgrades_through_the_full_startup() {
    let env = common::TempEnv::new("banto-hub-upgrade-0025", "before-0014");
    let config_dir = env.registry_path().parent().unwrap().join("config");
    std::fs::create_dir_all(&config_dir).unwrap();
    let db_path = config_dir.join("banto-hub.sqlite3");
    let pool = old_db_at(&db_path, 13).await;
    sqlx::raw_sql(
        "INSERT INTO plc_connections (id, name, protocol, host, port, unit_id, enabled, \
         simulation, word_order) VALUES \
         (1, 'M', 'modbus-tcp', '10.0.0.1', 502, 1, 1, 0, 'high_low'), \
         (2, 'S', 'slmp', '10.0.0.3', 5007, 1, 1, 0, 'low_high')",
    )
    .execute(&pool)
    .await
    .expect("seed the 0013 rows");
    pool.close().await;

    let running = HubRuntime::start(config(&db_path, env.data_dir()))
        .await
        .expect("a DB before 0014 upgrades");
    running.shutdown().await;

    assert_eq!(backups_in(&config_dir).len(), 1, "{:?}", names(&config_dir));
    let (applied, settings, secrets) = migrated_state(&db_path).await;
    assert_eq!(applied, 1);
    assert_eq!((settings[0].0, settings[1].0), (1, 2), "{settings:?}");
    assert!(settings[0].1.contains("10.0.0.1"), "{settings:?}");
    assert!(settings[1].1.contains("10.0.0.3"), "{settings:?}");
    assert_eq!(secrets, 0);
}
