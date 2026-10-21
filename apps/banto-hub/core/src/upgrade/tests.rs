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
        &crate::secrets::r2b1d_connection_secrets(),
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
    let secrets = crate::secrets::r2b1d_connection_secrets();
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
    let secrets = crate::secrets::r2b1d_connection_secrets();
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
