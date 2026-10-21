//! `crate::upgrade` の単体テスト（R2b-1d、設計 §6.3・§6.4・§6.6・§5.3・§9）。
//!
//! 「古い DB」は Hub の migrator の全部と `banto-tags` の 0001〜0024（`run_to(24)`、
//! checklist §5「migration のテストは対象の版まで」）で作り、生の SQL で alpha.33 の
//! 列（平文の `password` を含む）の行を入れる。レジストリは `test_drivers()`
//! （`modbus-tcp` / `slmp` は何でも通す偽物、`push` / `postgres` / `virtual` は本物）。
//! 本物の `modbus-tcp` / `slmp` で断る行（port 70000・host が空）と、起動の全体
//! （`HubRuntime::start` が断り、元の DB のハッシュが変わらない）は `banto-hub-host` の
//! `tests/upgrade_0025.rs`。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use sha2::{Digest, Sha256};
use sqlx::SqlitePool;

use super::*;
use crate::test_support::TempDir;

const PG_PASSWORD: &str = "TOPSECRET-pg-pw-7f3a";
const QUEUED_PASSWORD: &str = "TOPSECRET-queued-pw-91c2";

fn registry() -> Arc<DriverRegistry> {
    crate::test_support::test_drivers().registry
}

/// alpha.33 の形の DB（Hub の migrator 全部 + banto-tags 0001〜0024）を `dir` に作る。
async fn old_db(dir: &Path) -> (PathBuf, SqlitePool) {
    let path = dir.join("banto-hub.sqlite3");
    let pool = banto_storage::connect_sqlite(&path)
        .await
        .expect("connect the old DB");
    let mut hub = sqlx::migrate!("./migrations-sqlite");
    hub.dangerous_set_table_name(crate::db::MIGRATIONS_TABLE);
    hub.run(&pool).await.expect("hub migrations");
    let tags_dir =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../crates/banto-tags/migrations");
    let mut tags = sqlx::migrate::Migrator::new(tags_dir.as_path())
        .await
        .expect("banto-tags migrations");
    tags.dangerous_set_table_name(banto_tags::MIGRATIONS_TABLE);
    tags.run_to(24, &pool).await.expect("banto-tags up to 0024");
    (path, pool)
}

/// alpha.33 の平らな列で 1 行入れる（列の並びは 0023 の表のまま）。
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

/// 移行が通る alpha.33 の DB の中身: postgres（パスワードあり）・slmp（捨てる unit_id）・
/// push（既定）・hub（報告だけ）・予約の calc、と平文のパスワードを持つ未適用キューの行。
async fn seed_passing(pool: &SqlitePool) {
    insert_old(
        pool,
        &format!(
            "(1, 'pg', 'postgres', '10.0.0.50', 5432, 1, 1, 0, 'low_high', 'erp', 'reader', \
             '{PG_PASSWORD}', 'R', NULL)"
        ),
    )
    .await;
    insert_old(
        pool,
        "(2, 'S', 'slmp', '10.0.0.3', 5007, 3, 1, 0, 'low_high', NULL, NULL, NULL, 'Q', NULL)",
    )
    .await;
    insert_old(
        pool,
        "(3, 'P', 'push', '', 0, 1, 1, 0, 'low_high', NULL, NULL, NULL, 'R', NULL)",
    )
    .await;
    insert_old(
        pool,
        "(4, 'Hub', 'hub', '', 0, 1, 0, 0, 'low_high', NULL, NULL, NULL, 'R', NULL)",
    )
    .await;
    insert_old(
        pool,
        "(5, 'calc', 'virtual', '', 0, 1, 1, 0, 'low_high', NULL, NULL, NULL, 'R', NULL)",
    )
    .await;
    sqlx::query(
        "INSERT INTO pending_changes (state, source, payload, base_configured_revision) \
         VALUES ('pending', 'plc_connections.update', ?, 0), \
                ('applied', 'plc_connections.create', ?, 0)",
    )
    .bind(format!(
        r#"{{"id":1,"input":{{"name":"pg","protocol":"postgres","password":"{QUEUED_PASSWORD}"}}}}"#
    ))
    .bind(format!(
        r#"{{"input":{{"name":"pg2","protocol":"postgres","password":"{QUEUED_PASSWORD}"}}}}"#
    ))
    .execute(pool)
    .await
    .expect("seed the queue");
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

/// `dir` の中のファイル名（並べて）。
fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn sha256_of(path: &Path) -> String {
    hex::encode(Sha256::digest(std::fs::read(path).expect("read file")))
}

/// §6.4・§6.6: 毎回の起動の掃除は残り物だけを消し（完成した `.bak`・DB は残す）、消せない
/// ものがあれば、そのパスを持って起動を止める。
#[test]
fn remove_leftovers_deletes_only_leftovers_and_stops_on_one_it_cannot_delete() {
    let dir = TempDir::new("upgrade-leftovers");
    let db = dir.path().join("banto-hub.sqlite3");
    for name in [
        "banto-hub.sqlite3",
        "banto-hub.sqlite3.before-0025-20261021T000000Z.bak",
        "banto-hub.sqlite3.before-0025-20261021T000001Z.bak.partial",
        "banto-hub.sqlite3.before-0025-20261021T000001Z.bak.partial-journal",
        ".banto-hub-0025-dryrun-aa.sqlite3",
    ] {
        std::fs::write(dir.path().join(name), b"x").unwrap();
    }
    let removed = remove_leftovers(&db).expect("remove leftovers");
    assert_eq!(removed.len(), 3, "{removed:?}");
    assert_eq!(
        names(dir.path()),
        vec![
            "banto-hub.sqlite3".to_string(),
            "banto-hub.sqlite3.before-0025-20261021T000000Z.bak".to_string(),
        ]
    );

    // A leftover that cannot be deleted (here: a directory of that name) stops the start
    // with its path.
    let stuck = dir.path().join(".banto-hub-0025-dryrun-bb.sqlite3");
    std::fs::create_dir(&stuck).unwrap();
    match remove_leftovers(&db) {
        Err(UpgradeError::LeftoverNotRemoved { path, .. }) => assert_eq!(path, stuck),
        other => panic!("expected LeftoverNotRemoved, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pending_0025_is_true_only_for_a_database_before_the_migration() {
    let dir = TempDir::new("upgrade-pending");
    let fresh = banto_storage::connect_sqlite(dir.path().join("fresh.sqlite3"))
        .await
        .unwrap();
    assert!(
        !pending_0025(&fresh).await.unwrap(),
        "a new DB has nothing to move"
    );
    fresh.close().await;

    let (_, pool) = old_db(dir.path()).await;
    assert!(pending_0025(&pool).await.unwrap());
    banto_tags::migrate(&pool).await.unwrap();
    assert!(!pending_0025(&pool).await.unwrap());
    pool.close().await;
}

/// §6.3: 予行は写しだけを変え（DB 本体の 0025 は未適用のまま）、写しを残さない。`hub` の
/// 行と捨てる値（slmp の unit_id 3）は報告だけで、通る。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_dry_run_passes_reports_notes_and_leaves_no_copy() {
    let dir = TempDir::new("upgrade-dry-run-pass");
    let (path, pool) = old_db(dir.path()).await;
    seed_passing(&pool).await;

    let report = dry_run_and_preflight(&pool, &path, &registry())
        .await
        .expect("dry run");
    assert!(!report.is_refused(), "{report:?}");
    let notes: Vec<(i64, &str)> = report
        .notes
        .iter()
        .map(|row| (row.id, row.errors[0].as_str()))
        .collect();
    assert!(
        notes
            .iter()
            .any(|(id, text)| *id == 4 && text.contains("hub 接続")),
        "{notes:?}"
    );
    assert!(
        notes
            .iter()
            .any(|(id, text)| *id == 2 && text.contains("unitId \"3\" → \"1\"")),
        "{notes:?}"
    );
    // The DB itself is untouched; no copy is left behind.
    assert!(pending_0025(&pool).await.unwrap());
    assert!(
        !names(dir.path())
            .iter()
            .any(|name| name.starts_with(DRYRUN_PREFIX)),
        "{:?}",
        names(dir.path())
    );
    pool.close().await;
}

/// §6.3 の「断る」: ドライバの検証に落ちる行（postgres の host が空・port 70000、
/// 0024 までの CHECK は通す）と、写しへの migrate が落ちる行（push の settings が
/// オブジェクトでない）。どれも DB を変えず、写しを残さない。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_dry_run_refuses_rows_the_registry_rejects_and_a_failing_migration() {
    let dir = TempDir::new("upgrade-dry-run-refuse");
    let (path, pool) = old_db(dir.path()).await;
    seed_passing(&pool).await;
    insert_old(
        &pool,
        "(6, 'pg-bad', 'postgres', '', 70000, 1, 1, 0, 'low_high', 'erp', 'reader', NULL, 'R', NULL)",
    )
    .await;
    let report = dry_run_and_preflight(&pool, &path, &registry())
        .await
        .expect("dry run");
    assert!(report.is_refused());
    assert_eq!(
        report.refused.iter().map(|row| row.id).collect::<Vec<_>>(),
        vec![6]
    );
    let errors = report.refused[0].errors.join(" / ");
    assert!(
        errors.contains("host") && errors.contains("port"),
        "{errors}"
    );
    assert!(report.problems.is_empty(), "{report:?}");

    // A push row whose settings is not an object: the copy's migration fails, the row is named.
    sqlx::query("DELETE FROM plc_connections WHERE id = 6")
        .execute(&pool)
        .await
        .unwrap();
    insert_old(
        &pool,
        "(7, 'P-array', 'push', '', 0, 1, 1, 0, 'low_high', NULL, NULL, NULL, 'R', '[1]')",
    )
    .await;
    let report = dry_run_and_preflight(&pool, &path, &registry())
        .await
        .expect("dry run");
    assert!(report.is_refused());
    assert_eq!(report.refused[0].id, 7, "{report:?}");
    assert!(report.problems[0].contains("migration 0025"), "{report:?}");
    assert!(
        pending_0025(&pool).await.unwrap(),
        "the DB itself is unchanged"
    );
    assert!(
        !names(dir.path())
            .iter()
            .any(|name| name.starts_with(DRYRUN_PREFIX)),
        "{:?}",
        names(dir.path())
    );
    pool.close().await;
}

/// §6.4: 完成したバックアップには、接続の平文のパスワードも、キューのペイロードの
/// パスワードも（空きページにも）無い。行と DB 本体はそのまま。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_backup_holds_no_password_bytes() {
    let dir = TempDir::new("upgrade-backup");
    let (path, pool) = old_db(dir.path()).await;
    seed_passing(&pool).await;

    let backup = backup_before_0025(&pool, &path).await.expect("backup");
    let name = backup.file_name().unwrap().to_string_lossy().into_owned();
    assert!(
        name.starts_with("banto-hub.sqlite3.before-0025-") && name.ends_with(".bak"),
        "{name}"
    );
    let bytes = std::fs::read(&backup).unwrap();
    assert!(
        !contains(&bytes, PG_PASSWORD),
        "the connection password is in the backup"
    );
    assert!(
        !contains(&bytes, QUEUED_PASSWORD),
        "a queued password is in the backup"
    );
    assert!(
        !names(dir.path())
            .iter()
            .any(|n| n.ends_with(PARTIAL_SUFFIX)),
        "{:?}",
        names(dir.path())
    );

    // The backup still opens as the old DB with every row (passwords cleared).
    let opened = banto_storage::connect_sqlite(&backup).await.unwrap();
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM plc_connections")
        .fetch_one(&opened)
        .await
        .unwrap();
    assert_eq!(rows, 5);
    let passwords: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM plc_connections WHERE password IS NOT NULL")
            .fetch_one(&opened)
            .await
            .unwrap();
    assert_eq!(passwords, 0);
    let names_in_queue: Vec<String> =
        sqlx::query_scalar("SELECT json_extract(payload, '$.input.name') FROM pending_changes")
            .fetch_all(&opened)
            .await
            .unwrap();
    assert_eq!(
        names_in_queue.len(),
        2,
        "the queue rows are kept, minus the password"
    );
    opened.close().await;

    // The live DB is not touched by the backup.
    let still: Option<String> =
        sqlx::query_scalar("SELECT password FROM plc_connections WHERE id = 1")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(still.as_deref(), Some(PG_PASSWORD));
    pool.close().await;
}

/// §6.6・§9: バックアップの工程の間のどこで落ちても（VACUUM INTO の後・除去の後・
/// VACUUM の後・rename の前）、残るのは `.partial` の名前のものだけで、次の起動の掃除が
/// それを消す。平文を含む `.bak` はできない。その後のバックアップは平文を含まない。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_crash_between_backup_steps_leaves_only_a_partial_that_the_next_start_removes() {
    for step in [
        BackupStep::AfterVacuumInto,
        BackupStep::AfterStrip,
        BackupStep::AfterVacuum,
        BackupStep::BeforeRename,
    ] {
        let dir = TempDir::new("upgrade-backup-crash");
        let (path, pool) = old_db(dir.path()).await;
        seed_passing(&pool).await;

        let err = backup_with(&pool, &path, Some(step))
            .await
            .expect_err("the injected crash");
        assert!(matches!(err, UpgradeError::Backup { .. }), "{err:?}");
        let partials: Vec<String> = names(dir.path())
            .into_iter()
            .filter(|n| n.ends_with(PARTIAL_SUFFIX))
            .collect();
        assert_eq!(partials.len(), 1, "{step:?}: {:?}", names(dir.path()));
        assert!(
            !names(dir.path()).iter().any(|n| n.ends_with(BACKUP_SUFFIX)),
            "{step:?}: no finished backup may exist yet"
        );

        // The next start: the partial (with whatever plaintext it held) is gone.
        let removed = remove_leftovers(&path).expect("remove leftovers");
        assert!(!removed.is_empty(), "{step:?}");
        assert!(
            !names(dir.path()).iter().any(|n| n.contains(".bak.partial")),
            "{step:?}: {:?}",
            names(dir.path())
        );
        // ... and the backup it then makes holds no password.
        let backup = backup_before_0025(&pool, &path).await.expect("backup");
        for name in names(dir.path()) {
            if name.ends_with(BACKUP_SUFFIX) {
                let bytes = std::fs::read(dir.path().join(&name)).unwrap();
                assert!(!contains(&bytes, PG_PASSWORD), "{step:?}: {name}");
                assert!(!contains(&bytes, QUEUED_PASSWORD), "{step:?}: {name}");
            }
        }
        assert!(backup.exists());
        pool.close().await;
    }
}

/// §6.1 の 2-b〜2-c を `init_db_with_registry` で通す: 予行 → バックアップ → 清掃待ちの
/// 印 → 本物の移行。移行後の postgres 接続は封印した行を参照し、開ける。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn init_with_a_registry_backs_up_marks_and_migrates() {
    let dir = TempDir::new("upgrade-init");
    let (path, pool) = old_db(dir.path()).await;
    seed_passing(&pool).await;
    pool.close().await;

    let pool = crate::db::init_db_with_registry(&path, &registry())
        .await
        .expect("init with the registry");
    assert!(!pending_0025(&pool).await.unwrap());
    let mark: Option<String> = sqlx::query_scalar("SELECT value FROM settings WHERE key = ?")
        .bind(VACUUM_PENDING_KEY)
        .fetch_optional(&pool)
        .await
        .unwrap();
    assert_eq!(mark.as_deref(), Some("true"));
    let backups: Vec<String> = names(dir.path())
        .into_iter()
        .filter(|n| n.ends_with(BACKUP_SUFFIX))
        .collect();
    assert_eq!(backups.len(), 1, "{:?}", names(dir.path()));

    let service = PlcConnectionService::with_registry(pool.clone(), registry());
    let pg = service.get(1).await.unwrap();
    let rows = service.secret_rows(1).await.unwrap();
    let opened = crate::secrets::open_connection_secret(
        &crate::secrets::ConnectionSecrets::platform_default(),
        &pg,
        banto_tags::PASSWORD_KEY,
        &rows,
    )
    .expect("readable")
    .expect("set");
    assert_eq!(opened.as_str(), PG_PASSWORD);
    pool.close().await;

    // The second start: nothing pending, no second backup, nothing removed.
    let pool = crate::db::init_db_with_registry(&path, &registry())
        .await
        .expect("second start");
    assert_eq!(
        names(dir.path())
            .into_iter()
            .filter(|n| n.ends_with(BACKUP_SUFFIX))
            .count(),
        1
    );
    pool.close().await;
}

/// A refused preflight leaves the DB exactly as it was (byte for byte, after closing),
/// makes no backup and leaves no copy.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_preflight_changes_nothing() {
    let dir = TempDir::new("upgrade-refused");
    let (path, pool) = old_db(dir.path()).await;
    seed_passing(&pool).await;
    insert_old(
        &pool,
        "(6, 'pg-bad', 'postgres', '', 5432, 1, 1, 0, 'low_high', 'erp', 'reader', NULL, 'R', NULL)",
    )
    .await;
    // Checkpoint so the main file holds everything before hashing.
    sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
    let before = sha256_of(&path);
    let files_before = names(dir.path());

    let err = crate::db::init_db_with_registry(&path, &registry())
        .await
        .expect_err("the preflight refuses");
    match &err {
        crate::db::InitDbError::Upgrade(UpgradeError::Preflight(refusal)) => {
            assert_eq!(refusal.rows.len(), 1, "{refusal:?}");
            assert_eq!(refusal.rows[0].id, 6);
            let text = refusal.to_string();
            assert!(text.contains("旧版"), "{text}");
            assert!(text.contains("pg-bad"), "{text}");
        }
        other => panic!("expected a preflight refusal, got {other:?}"),
    }
    assert_eq!(sha256_of(&path), before, "the DB file must not change");
    let after: Vec<String> = names(dir.path())
        .into_iter()
        .filter(|n| !n.ends_with("-wal") && !n.ends_with("-shm"))
        .collect();
    let before_files: Vec<String> = files_before
        .into_iter()
        .filter(|n| !n.ends_with("-wal") && !n.ends_with("-shm"))
        .collect();
    assert_eq!(after, before_files, "no backup, no copy");
}

/// Without a registry (the elevated helper) a DB that still needs 0025 is not migrated.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn init_without_a_registry_refuses_a_database_that_needs_0025() {
    let dir = TempDir::new("upgrade-no-registry");
    let (path, pool) = old_db(dir.path()).await;
    seed_passing(&pool).await;
    pool.close().await;
    let err = crate::db::init_db(&path).await.expect_err("refused");
    assert!(
        matches!(
            err,
            crate::db::InitDbError::Upgrade(UpgradeError::NeedsHubStartup { .. })
        ),
        "{err:?}"
    );
    let pool = banto_storage::connect_sqlite(&path).await.unwrap();
    assert!(pending_0025(&pool).await.unwrap(), "nothing was migrated");
    pool.close().await;
}

/// §5.3 の最後の行: 参照があって行が無い → 報告だけ（行も参照も変えない）、行があって
/// 参照が無い（別の接続・別のキー・参照の違うものを含む）→ 消す。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_integrity_check_reports_missing_rows_and_deletes_orphans() {
    let pool = crate::db::init_db_memory().await.unwrap();
    let service = PlcConnectionService::with_registry(pool.clone(), registry());
    let secrets = crate::secrets::ConnectionSecrets::platform_default();
    let mut input = banto_tags::PlcConnectionInput {
        name: "pg".to_string(),
        protocol: "postgres".to_string(),
        host: "db".to_string(),
        port: 5432,
        unit_id: 1,
        enabled: true,
        simulation: false,
        word_order: String::new(),
        cpu_type: None,
        database: Some("erp".to_string()),
        username: Some("reader".to_string()),
        secrets: crate::secrets::password_changes(&secrets, Some("pw-a")).unwrap(),
        settings: None,
    };
    let a = service.create(input.clone()).await.unwrap();
    input.name = "pg2".to_string();
    input.secrets = crate::secrets::password_changes(&secrets, Some("pw-b")).unwrap();
    let b = service.create(input).await.unwrap();

    // Healthy: nothing to report.
    assert_eq!(
        check_secret_integrity(&pool).await.unwrap(),
        IntegrityReport::default()
    );

    // b's row goes missing; an orphan row appears (no connection 99 refers to it).
    sqlx::query("DELETE FROM plc_connection_secrets WHERE connection_id = ?")
        .bind(b.id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO plc_connection_secrets (ref, connection_id, key, scheme, blob) \
         VALUES ('0123456789abcdef0123456789abcdef', 99, 'password', 'plain-v1', X'6f')",
    )
    .execute(&pool)
    .await
    .unwrap();
    let report = check_secret_integrity(&pool).await.unwrap();
    assert_eq!(
        report.missing,
        vec![(b.id, "pg2".to_string(), "password".to_string())]
    );
    assert_eq!(report.orphans.len(), 1);
    assert_eq!(report.orphans[0].connection_id, 99);
    let left: Vec<i64> = sqlx::query_scalar(
        "SELECT connection_id FROM plc_connection_secrets ORDER BY connection_id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(left, vec![a.id], "only the orphan was deleted");
    // b still refers to its (missing) row: it reads as unreadable, never as "no password".
    let b_conn = service.get(b.id).await.unwrap();
    assert!(b_conn.secret_ref("password").is_some());
    assert_eq!(
        crate::secrets::secret_state(&secrets, &b_conn, "password", &[]),
        crate::secrets::SecretState::Unreadable {
            reason: crate::secrets::UnreadableReason::Missing
        }
    );

    // A row of the right connection but under a reference nobody holds is an orphan too.
    sqlx::query(
        "INSERT INTO plc_connection_secrets (ref, connection_id, key, scheme, blob) \
         VALUES ('fedcba9876543210fedcba9876543210', ?, 'token', 'plain-v1', X'6f')",
    )
    .bind(a.id)
    .execute(&pool)
    .await
    .unwrap();
    let report = check_secret_integrity(&pool).await.unwrap();
    assert_eq!(report.orphans.len(), 1, "{report:?}");
    assert_eq!(report.orphans[0].key, "token");
    // The second run has nothing left to delete.
    assert!(check_secret_integrity(&pool)
        .await
        .unwrap()
        .orphans
        .is_empty());
}

/// PR #777 のオーナーレビュー P2: 形の違う参照（生の SQL で書いた `{"$secret": 123}`・
/// 余計なキー・平文の文字列）は起動時の整合の確認が `malformed` として報告し、読む側は
/// `Unreadable { Invalid }`（「パスワード無し」にしない）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_integrity_check_reports_malformed_references() {
    let pool = crate::db::init_db_memory().await.unwrap();
    let service = PlcConnectionService::with_registry(pool.clone(), registry());
    let secrets = crate::secrets::ConnectionSecrets::platform_default();
    let mut ids = Vec::new();
    for (name, password) in [
        ("pg-number", r#"{"$secret":123}"#),
        (
            "pg-extra",
            r#"{"$secret":"0123456789abcdef0123456789abcdef","x":1}"#,
        ),
        ("pg-plain", r#""plain-text""#),
    ] {
        let id: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "INSERT INTO plc_connections (name, protocol, enabled, simulation, settings, \
             settings_version) VALUES ('{name}', 'postgres', 1, 0, \
             '{{\"host\":\"db\",\"port\":5432,\"password\":{password}}}', 1) RETURNING id"
        )))
        .fetch_one(&pool)
        .await
        .unwrap();
        ids.push((id, name.to_string()));
    }
    // The row the extra-key value names is kept (reported, not deleted as an orphan).
    sqlx::query(
        "INSERT INTO plc_connection_secrets (ref, connection_id, key, scheme, blob) \
         VALUES ('0123456789abcdef0123456789abcdef', ?, 'password', 'plain-v1', X'6f')",
    )
    .bind(ids[1].0)
    .execute(&pool)
    .await
    .unwrap();
    let report = check_secret_integrity(&pool).await.unwrap();
    assert!(report.orphans.is_empty(), "{report:?}");
    let kept: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM plc_connection_secrets")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(kept, 1);
    let expected: Vec<(i64, String, String)> = ids
        .iter()
        .map(|(id, name)| (*id, name.clone(), "password".to_string()))
        .collect();
    assert_eq!(report.malformed, expected, "{report:?}");
    assert!(report.missing.is_empty(), "{report:?}");
    for (id, _) in &ids {
        let conn = service.get(*id).await.unwrap();
        assert_eq!(
            crate::secrets::secret_state(&secrets, &conn, "password", &[]),
            crate::secrets::SecretState::Unreadable {
                reason: crate::secrets::UnreadableReason::Invalid
            }
        );
    }
}

#[test]
fn the_refusal_message_lists_rows_problems_and_the_remedy() {
    let refusal = PreflightRefusal {
        db_path: PathBuf::from("/p/config/banto-hub.sqlite3"),
        rows: vec![PreflightRow {
            id: 3,
            name: "line1".to_string(),
            protocol: "modbus-tcp".to_string(),
            errors: vec!["port: 1〜65535 で指定してください".to_string()],
        }],
        problems: vec!["秘密値の行の数".to_string()],
    };
    let text = refusal.to_string();
    for want in [
        "/p/config/banto-hub.sqlite3",
        "旧版",
        "直すか削除",
        "接続 #3 line1（modbus-tcp）: port",
        "秘密値の行の数",
    ] {
        assert!(text.contains(want), "{want}: {text}");
    }
}

// ---- R2b-1e: 秘密値の後処理（設計 §6.5・§6.6・§9 の「冪等」「キューの移設」） -----------
//
// 「Windows の Hub」は偽の protector（`crate::test_support::XorTestProtector`、全 OS で
// DPAPI の経路の形）で、「非 Windows で作った DB」は protector 無し（`plain-v1`）で作る。
// 本物の DPAPI での同じ確かめは `dpapi_upkeep`（`#[cfg(windows)]`、windows-latest の CI）。

const UPKEEP_PG_PASSWORD: &str = "TOPSECRET-upkeep-pg-41ab";
const UPKEEP_QUEUED_PASSWORD: &str = "TOPSECRET-upkeep-queue-77cd";
const UPKEEP_LEGACY_PASSWORD: &str = "TOPSECRET-upkeep-legacy-02ef";

/// 封印していない（非 Windows の Hub の）[`crate::secrets::ConnectionSecrets`]。
fn plain_secrets() -> crate::secrets::ConnectionSecrets {
    crate::secrets::ConnectionSecrets::new(None)
}

fn base64_of(text: &str) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(text.as_bytes())
}

/// `path` の DB と `-wal`（あれば）のバイト列に、`secrets` のどれか（とその base64）が
/// 含まれていれば、その名前を返す。
fn leaked_in_files(path: &Path, secrets: &[&str]) -> Vec<String> {
    let mut leaks = Vec::new();
    for file in [
        path.to_path_buf(),
        PathBuf::from(format!("{}-wal", path.display())),
    ] {
        let Ok(bytes) = std::fs::read(&file) else {
            continue;
        };
        for secret in secrets {
            for needle in [secret.to_string(), base64_of(secret)] {
                if contains(&bytes, &needle) {
                    leaks.push(format!("{} in {}", needle, file.display()));
                }
            }
        }
    }
    leaks
}

/// R2b-1d 以降の形の DB（`init_db_with_registry` の新規の DB）に、非 Windows の Hub が
/// 書いた形の中身を入れる: `plain-v1` の秘密の行を持つ postgres 接続と、5 つの状態
/// （`pending`・`applying`・`failed`・`applied`・`canceled`）の `plain-v1` の
/// `sealedPassword` を持つキューの行、R2b-1e より前の平文の `password` を持つキューの行
/// （`pending` と `applied`）。接続 id と、状態ごとのキューの id を返す。
async fn seed_non_windows_secrets(pool: &SqlitePool) -> (i64, Vec<(String, i64)>) {
    let service = PlcConnectionService::with_registry(pool.clone(), registry());
    let conn = service
        .create(banto_tags::PlcConnectionInput {
            name: "pg".to_string(),
            protocol: "postgres".to_string(),
            host: "db".to_string(),
            port: 5432,
            unit_id: 1,
            enabled: true,
            simulation: false,
            word_order: String::new(),
            cpu_type: None,
            database: Some("erp".to_string()),
            username: Some("reader".to_string()),
            secrets: crate::secrets::password_changes(&plain_secrets(), Some(UPKEEP_PG_PASSWORD))
                .unwrap(),
            settings: None,
        })
        .await
        .unwrap();
    let mut queue = Vec::new();
    for state in ["pending", "applying", "failed", "applied", "canceled"] {
        let sealed = plain_secrets()
            .seal(banto_tags::PASSWORD_KEY, UPKEEP_QUEUED_PASSWORD)
            .unwrap();
        let payload = serde_json::json!({ "input": {
            "name": format!("pg-{state}"), "protocol": "postgres", "host": "db", "port": 5432,
            "database": "erp", "username": "reader",
            "sealedPassword": crate::secrets::sealed_password_json(&sealed),
        }});
        queue.push((
            state.to_string(),
            insert_queue_row(pool, state, "plc_connections.create", &payload).await,
        ));
    }
    for state in ["pending", "applied"] {
        let payload = serde_json::json!({ "id": conn.id, "input": {
            "name": "pg", "protocol": "postgres", "host": "db", "port": 5432,
            "password": UPKEEP_LEGACY_PASSWORD,
        }});
        queue.push((
            format!("legacy-{state}"),
            insert_queue_row(pool, state, "plc_connections.update", &payload).await,
        ));
    }
    (conn.id, queue)
}

async fn insert_queue_row(
    pool: &SqlitePool,
    state: &str,
    source: &str,
    payload: &serde_json::Value,
) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO pending_changes (state, source, payload, base_configured_revision) \
         VALUES (?, ?, ?, 0) RETURNING id",
    )
    .bind(state)
    .bind(source)
    .bind(payload.to_string())
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn queue_payload(pool: &SqlitePool, id: i64) -> serde_json::Value {
    let text: String = sqlx::query_scalar("SELECT payload FROM pending_changes WHERE id = ?")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap();
    serde_json::from_str(&text).unwrap()
}

async fn vacuum_mark(pool: &SqlitePool) -> Option<String> {
    sqlx::query_scalar("SELECT value FROM settings WHERE key = ?")
        .bind(VACUUM_PENDING_KEY)
        .fetch_optional(pool)
        .await
        .unwrap()
}

async fn secret_rows_of(pool: &SqlitePool) -> Vec<banto_tags::StoredSecretRow> {
    banto_tags::connection_secret::load_all_secret_rows(pool)
        .await
        .unwrap()
}

/// 全部の行とペイロードの写し（「何も変わっていない」を比べるため）。
async fn snapshot_secrets(pool: &SqlitePool) -> (Vec<(String, String, Vec<u8>)>, Vec<String>) {
    let rows: Vec<(String, String, Vec<u8>)> =
        sqlx::query_as("SELECT ref, scheme, blob FROM plc_connection_secrets ORDER BY ref")
            .fetch_all(pool)
            .await
            .unwrap();
    let payloads: Vec<String> =
        sqlx::query_scalar("SELECT payload FROM pending_changes ORDER BY id")
            .fetch_all(pool)
            .await
            .unwrap();
    (rows, payloads)
}

/// §9「キューの移設」と §6.5 の (a)〜(e): 非 Windows で作った DB を「Windows の Hub」（偽の
/// protector）の後処理にかけると、`plain-v1` の秘密の行は同じ参照のまま封印し直され、
/// 未適用の 3 つのキューの行は封印し直され（平文の `password` の行も封印され）、履歴の 2 つ
/// からは `password` も `sealedPassword` も消える。印を立てて VACUUM し、DB・WAL のバイト列に
/// 既知のパスワード（と base64）が無い。2 回目は何も書かない（冪等）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_upkeep_reseals_rows_and_the_queue_strips_history_and_is_idempotent() {
    let dir = TempDir::new("upkeep-relocate");
    let path = dir.path().join("banto-hub.sqlite3");
    let pool = crate::db::init_db_with_registry(&path, &registry())
        .await
        .unwrap();
    let (conn_id, queue) = seed_non_windows_secrets(&pool).await;
    let before = secret_rows_of(&pool).await;
    assert_eq!(before.len(), 1);
    assert_eq!(before[0].sealed.scheme, "plain-v1");
    let sealed_ref_before = |payload: &serde_json::Value| {
        payload["input"]["sealedPassword"]["ref"]
            .as_str()
            .map(str::to_owned)
    };
    let mut refs_before = std::collections::HashMap::new();
    for (label, id) in &queue {
        refs_before.insert(
            label.clone(),
            sealed_ref_before(&queue_payload(&pool, *id).await),
        );
    }

    let secrets = crate::test_support::protected_secrets();
    let report = secrets_upkeep(&pool, &secrets).await;
    assert_eq!(report.resealed_rows, 1, "{report:?}");
    assert_eq!(report.sealed_queue, 1, "{report:?}");
    assert_eq!(report.resealed_queue, 3, "{report:?}");
    assert_eq!(report.stripped_history, 3, "{report:?}");
    assert!(report.failures.is_empty(), "{report:?}");
    assert_eq!(report.rewrite_error, None);
    assert!(report.vacuumed, "{report:?}");
    assert_eq!(report.vacuum_error, None);
    assert_eq!(
        vacuum_mark(&pool).await,
        None,
        "the mark is cleared after VACUUM"
    );

    // (a) the secret row: same reference, protected, opens to the same value.
    let after = secret_rows_of(&pool).await;
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].sealed.r#ref, before[0].sealed.r#ref);
    assert_eq!(after[0].sealed.scheme, "dpapi-machine-v1");
    assert!(!contains(&after[0].sealed.blob, UPKEEP_PG_PASSWORD));
    let service = PlcConnectionService::with_registry(pool.clone(), registry());
    let conn = service.get(conn_id).await.unwrap();
    assert_eq!(
        crate::secrets::open_connection_secret(&secrets, &conn, "password", &after)
            .unwrap()
            .unwrap()
            .as_str(),
        UPKEEP_PG_PASSWORD
    );

    // (b)(c)(d) the queue.
    for (label, id) in &queue {
        let payload = queue_payload(&pool, *id).await;
        let input = &payload["input"];
        let unapplied =
            ["pending", "applying", "failed", "legacy-pending"].contains(&label.as_str());
        assert!(input.get("password").is_none(), "{label}: {payload}");
        if unapplied {
            let sealed = crate::secrets::parse_sealed_password(&input["sealedPassword"]).unwrap();
            assert_eq!(sealed.scheme.as_str(), "dpapi-machine-v1", "{label}");
            let want = if label.starts_with("legacy") {
                UPKEEP_LEGACY_PASSWORD
            } else {
                // Resealed under the same reference (design §6.5).
                assert_eq!(
                    Some(sealed.r#ref.as_str().to_owned()),
                    refs_before[label],
                    "{label}"
                );
                UPKEEP_QUEUED_PASSWORD
            };
            assert_eq!(
                secrets.open("password", &sealed).unwrap().as_str(),
                want,
                "{label}"
            );
        } else {
            assert!(input.get("sealedPassword").is_none(), "{label}: {payload}");
            assert_eq!(input["passwordSet"], serde_json::json!(true), "{label}");
        }
    }

    // The second start: nothing to rewrite, no mark, no VACUUM.
    let snapshot = snapshot_secrets(&pool).await;
    let again = secrets_upkeep(&pool, &secrets).await;
    assert_eq!(
        again,
        UpkeepReport::default(),
        "the second run writes nothing"
    );
    assert_eq!(vacuum_mark(&pool).await, None);
    assert_eq!(snapshot_secrets(&pool).await, snapshot);

    pool.close().await;
    let leaks = leaked_in_files(
        &path,
        &[
            UPKEEP_PG_PASSWORD,
            UPKEEP_QUEUED_PASSWORD,
            UPKEEP_LEGACY_PASSWORD,
        ],
    );
    assert!(
        leaks.is_empty(),
        "known passwords left in the files: {leaks:?}"
    );
}

/// §6.6「後処理（§6.5）の途中」: 工程 1 の commit の前に落ちると、トランザクションごと戻り
/// （行もキューも印も変わらない）、次の起動の後処理が最後までやる。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failure_inside_the_upkeep_rolls_back_and_the_next_start_completes() {
    let pool = crate::db::init_db_memory().await.unwrap();
    seed_non_windows_secrets(&pool).await;
    let snapshot = snapshot_secrets(&pool).await;
    let secrets = crate::test_support::protected_secrets();

    let err = rewrite_secrets_failing_before_commit(&pool, &secrets)
        .await
        .expect_err("the injected failure");
    assert!(err.to_string().contains("注入"), "{err}");
    assert_eq!(snapshot_secrets(&pool).await, snapshot, "rolled back");
    assert_eq!(
        vacuum_mark(&pool).await,
        None,
        "the mark rolled back with it"
    );

    let report = secrets_upkeep(&pool, &secrets).await;
    assert!(report.rewrote(), "{report:?}");
    assert!(report.vacuumed, "{report:?}");
    assert_eq!(
        secret_rows_of(&pool).await[0].sealed.scheme,
        "dpapi-machine-v1"
    );
    assert_eq!(
        secrets_upkeep(&pool, &secrets).await,
        UpkeepReport::default()
    );
}

/// §6.6「1 の commit の後・VACUUM の前」（2026-10-21 再レビュー P2）: 印の無い DB
/// （前の起動で印を消した後）で再処理を commit した直後に落ちても、印が同じトランザクションで
/// 立っているので、次の起動で VACUUM だけが走り、DB・WAL のバイト列に既知のパスワードが無い。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_crash_after_the_rewrite_commit_is_vacuumed_by_the_next_start() {
    let dir = TempDir::new("upkeep-crash-before-vacuum");
    let path = dir.path().join("banto-hub.sqlite3");
    let pool = crate::db::init_db_with_registry(&path, &registry())
        .await
        .unwrap();
    seed_non_windows_secrets(&pool).await;
    assert_eq!(vacuum_mark(&pool).await, None, "no mark before the rewrite");
    let secrets = crate::test_support::protected_secrets();

    // Step 1 only: committed, then the process "dies" before step 2.
    let report = rewrite_secrets(&pool, &secrets).await.unwrap();
    assert!(report.rewrote(), "{report:?}");
    assert_eq!(vacuum_mark(&pool).await.as_deref(), Some("true"));
    pool.close().await;

    // The next start: nothing left to rewrite, but the mark makes it VACUUM.
    let pool = crate::db::init_db_with_registry(&path, &registry())
        .await
        .unwrap();
    let report = secrets_upkeep(&pool, &secrets).await;
    assert!(!report.rewrote(), "{report:?}");
    assert!(report.vacuumed, "{report:?}");
    assert_eq!(vacuum_mark(&pool).await, None);
    pool.close().await;
    let leaks = leaked_in_files(
        &path,
        &[
            UPKEEP_PG_PASSWORD,
            UPKEEP_QUEUED_PASSWORD,
            UPKEEP_LEGACY_PASSWORD,
        ],
    );
    assert!(
        leaks.is_empty(),
        "known passwords left in the files: {leaks:?}"
    );
}

/// §6.5 の最後: 封印し直しの失敗（DPAPI の失敗）は起動を止めず、その行は `plain-v1` のまま
/// 残して結果に数える。状態の画面の件数（`plaintext_remaining`）に出る。履歴の行からの除去は
/// 封印が要らないので進む。protector の無い（非 Windows の）Hub では平文が既定なので件数は 0。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_reseal_keeps_the_row_and_is_counted_as_remaining_plaintext() {
    let pool = crate::db::init_db_memory().await.unwrap();
    seed_non_windows_secrets(&pool).await;
    let failing = crate::secrets::ConnectionSecrets::new(Some(Arc::new(
        crate::test_support::FailingProtector,
    )));

    let report = secrets_upkeep(&pool, &failing).await;
    // 1 secret row + the legacy pending row (seal) + 3 unapplied plain-v1 rows (reseal).
    assert_eq!(report.failures.len(), 5, "{report:?}");
    assert!(report.failures.iter().all(
        |f| !f.error.contains(UPKEEP_PG_PASSWORD) && !f.error.contains(UPKEEP_QUEUED_PASSWORD)
    ));
    assert_eq!(
        report.resealed_rows + report.sealed_queue + report.resealed_queue,
        0
    );
    assert_eq!(report.stripped_history, 3, "{report:?}");
    assert!(report.vacuumed, "{report:?}");
    assert_eq!(secret_rows_of(&pool).await[0].sealed.scheme, "plain-v1");
    assert_eq!(plaintext_remaining(&pool, &failing).await.unwrap(), 5);
    assert_eq!(
        plaintext_remaining(&pool, &plain_secrets()).await.unwrap(),
        0
    );

    // Once the protector works, the rows are resealed and nothing remains.
    let secrets = crate::test_support::protected_secrets();
    let report = secrets_upkeep(&pool, &secrets).await;
    assert_eq!(report.resealed_rows, 1, "{report:?}");
    assert!(report.failures.is_empty(), "{report:?}");
    assert_eq!(plaintext_remaining(&pool, &secrets).await.unwrap(), 0);
}

const MOVE_PASSWORD: &str = "TOPSECRET-move-to-history-5e1a";

/// 接続の作成の提案を `state` で 1 行積む。パスワードは `secrets` で封印した
/// `sealedPassword`。
async fn insert_sealed_queue_row(
    pool: &SqlitePool,
    state: &str,
    secrets: &crate::secrets::ConnectionSecrets,
) -> i64 {
    let sealed = secrets
        .seal(banto_tags::PASSWORD_KEY, MOVE_PASSWORD)
        .unwrap();
    let payload = serde_json::json!({ "input": {
        "name": "pg-move", "protocol": "postgres", "host": "db", "port": 5432,
        "database": "erp", "username": "reader",
        "sealedPassword": crate::secrets::sealed_password_json(&sealed),
    }});
    insert_queue_row(pool, state, "plc_connections.create", &payload).await
}

async fn move_to_history(
    service: &crate::pending_changes::PendingChangesService,
    via_cancel: bool,
    id: i64,
) {
    if via_cancel {
        service.cancel_pending(id).await.unwrap();
    } else {
        service.mark_applied(id).await.unwrap();
    }
}

/// #778 のレビュー P2: 起動時の封印し直しに失敗して未適用キューに平文が残り、その起動の
/// VACUUM で印が消えた後に、取り消し / 適用で履歴へ移しても、平文を外したので同じ
/// トランザクションで清掃待ちの印が立つ。次の起動で VACUUM が走り、印が消え、DB・WAL に
/// 既知のパスワードが残らない。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn moving_a_plaintext_row_to_history_marks_the_database_for_vacuum() {
    for (via_cancel, state) in [(true, "pending"), (false, "applying")] {
        let dir = TempDir::new("upkeep-move-history");
        let path = dir.path().join("banto-hub.sqlite3");
        let pool = crate::db::init_db_with_registry(&path, &registry())
            .await
            .unwrap();
        let id = insert_sealed_queue_row(&pool, state, &plain_secrets()).await;
        let failing = crate::secrets::ConnectionSecrets::new(Some(Arc::new(
            crate::test_support::FailingProtector,
        )));

        // The reseal fails, the row keeps the plain-v1 value; the VACUUM of this start
        // (forced by a mark) clears the mark.
        set_vacuum_pending(&pool).await.unwrap();
        let report = secrets_upkeep(&pool, &failing).await;
        assert_eq!(report.failures.len(), 1, "{report:?}");
        assert!(report.vacuumed, "{report:?}");
        assert_eq!(vacuum_mark(&pool).await, None, "{state}");
        assert_eq!(
            queue_payload(&pool, id).await["input"]["sealedPassword"]["scheme"],
            "plain-v1"
        );

        // Moving it to history strips the plain value and marks the database.
        let service = crate::pending_changes::PendingChangesService::new(pool.clone());
        move_to_history(&service, via_cancel, id).await;
        assert!(queue_payload(&pool, id).await["input"]
            .get("sealedPassword")
            .is_none());
        assert_eq!(vacuum_mark(&pool).await.as_deref(), Some("true"), "{state}");
        pool.close().await;

        // The next start: nothing to rewrite, the mark makes it VACUUM.
        let pool = crate::db::init_db_with_registry(&path, &registry())
            .await
            .unwrap();
        let report = secrets_upkeep(&pool, &failing).await;
        assert!(!report.rewrote(), "{report:?}");
        assert!(report.vacuumed, "{report:?}");
        assert_eq!(vacuum_mark(&pool).await, None);
        pool.close().await;
        let leaks = leaked_in_files(&path, &[MOVE_PASSWORD]);
        assert!(leaks.is_empty(), "{state}: left in the files: {leaks:?}");
    }
}

/// 上の逆: DPAPI で封印した `sealedPassword` だけを履歴へ移すときは、空きページに残るのは
/// 暗号文で平文ではないので、印を立てない。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn moving_a_dpapi_sealed_row_to_history_does_not_mark_the_database() {
    for (via_cancel, state) in [(true, "pending"), (false, "applying")] {
        let pool = crate::db::init_db_memory().await.unwrap();
        let id =
            insert_sealed_queue_row(&pool, state, &crate::test_support::protected_secrets()).await;
        assert_eq!(
            queue_payload(&pool, id).await["input"]["sealedPassword"]["scheme"],
            "dpapi-machine-v1"
        );
        let service = crate::pending_changes::PendingChangesService::new(pool.clone());
        move_to_history(&service, via_cancel, id).await;
        assert!(queue_payload(&pool, id).await["input"]
            .get("sealedPassword")
            .is_none());
        assert_eq!(vacuum_mark(&pool).await, None, "{state}");
    }
}

/// 非 Windows の Hub（protector 無し）の後処理: `plain-v1` の行はそのまま（封印し直さない）、
/// 古い平文の `password` のキューの行は `plain-v1` の `sealedPassword` に揃え、履歴からは外す。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_upkeep_without_a_protector_only_normalizes_the_queue() {
    let pool = crate::db::init_db_memory().await.unwrap();
    let (_, queue) = seed_non_windows_secrets(&pool).await;
    let report = secrets_upkeep(&pool, &plain_secrets()).await;
    assert_eq!(report.resealed_rows, 0, "{report:?}");
    assert_eq!(report.resealed_queue, 0, "{report:?}");
    assert_eq!(report.sealed_queue, 1, "{report:?}");
    assert_eq!(report.stripped_history, 3, "{report:?}");
    assert_eq!(secret_rows_of(&pool).await[0].sealed.scheme, "plain-v1");
    let legacy = queue
        .iter()
        .find(|(label, _)| label == "legacy-pending")
        .unwrap()
        .1;
    let payload = queue_payload(&pool, legacy).await;
    assert!(payload["input"].get("password").is_none(), "{payload}");
    assert_eq!(payload["input"]["sealedPassword"]["scheme"], "plain-v1");
    assert_eq!(
        secrets_upkeep(&pool, &plain_secrets()).await,
        UpkeepReport::default()
    );
}

/// 本物の DPAPI（machine スコープ）での §9「キューの移設」「平文が残らない」。
/// windows-latest の CI で走る。
#[cfg(windows)]
mod dpapi_upkeep {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_windows_hub_reseals_a_non_windows_database_with_dpapi() {
        let dir = TempDir::new("upkeep-dpapi");
        let path = dir.path().join("banto-hub.sqlite3");
        let pool = crate::db::init_db_with_registry(&path, &registry())
            .await
            .unwrap();
        let (conn_id, queue) = seed_non_windows_secrets(&pool).await;
        let secrets = crate::secrets::ConnectionSecrets::platform_default();
        assert_eq!(
            secrets.seal_scheme(),
            crate::secrets::SecretScheme::DpapiMachineV1
        );

        let report = secrets_upkeep(&pool, &secrets).await;
        assert_eq!(report.resealed_rows, 1, "{report:?}");
        assert_eq!(report.resealed_queue, 3, "{report:?}");
        assert_eq!(report.sealed_queue, 1, "{report:?}");
        assert_eq!(report.stripped_history, 3, "{report:?}");
        assert!(report.failures.is_empty(), "{report:?}");
        assert!(report.vacuumed, "{report:?}");
        assert_eq!(plaintext_remaining(&pool, &secrets).await.unwrap(), 0);

        let rows = secret_rows_of(&pool).await;
        assert_eq!(rows[0].sealed.scheme, "dpapi-machine-v1");
        let conn = PlcConnectionService::with_registry(pool.clone(), registry())
            .get(conn_id)
            .await
            .unwrap();
        assert_eq!(
            crate::secrets::open_connection_secret(&secrets, &conn, "password", &rows)
                .unwrap()
                .unwrap()
                .as_str(),
            UPKEEP_PG_PASSWORD
        );
        for (label, id) in &queue {
            let payload = queue_payload(&pool, *id).await;
            if let Some(value) = payload["input"].get("sealedPassword") {
                let sealed = crate::secrets::parse_sealed_password(value).unwrap();
                assert_eq!(sealed.scheme.as_str(), "dpapi-machine-v1", "{label}");
                assert!(secrets.open("password", &sealed).is_ok(), "{label}");
            }
            assert!(payload["input"].get("password").is_none(), "{label}");
        }
        pool.close().await;
        let leaks = leaked_in_files(
            &path,
            &[
                UPKEEP_PG_PASSWORD,
                UPKEEP_QUEUED_PASSWORD,
                UPKEEP_LEGACY_PASSWORD,
            ],
        );
        assert!(
            leaks.is_empty(),
            "known passwords left in the files: {leaks:?}"
        );
    }
}
