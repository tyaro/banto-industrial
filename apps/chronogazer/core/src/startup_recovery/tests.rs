use super::*;
use crate::db::LegacyDatabase;
use crate::engine::{EngineConfig, EngineListen, EngineRuntime, RunningEngine};
use crate::hub::UnavailableKeyStore;
use crate::instance_lock::{acquire_for_db, ChronoGazerHost, InstanceLockFailure};
use crate::settings::read_data_dir;
use crate::test_support::TempDir;
use std::sync::Arc;

fn config(dir: &Path) -> EngineConfig {
    let db_path = dir.join("chronogazer.sqlite3");
    EngineConfig {
        drivers: crate::test_support::drivers(),
        data_base: dir.to_path_buf(),
        service_data_root: None,
        db_pin: crate::engine::DbPin::Off,
        db_path,
        key_store: Arc::new(UnavailableKeyStore),
        hub_tls: crate::hub_tls::HubTlsPolicy::production(),
        attach_hub: false,
        host: ChronoGazerHost::Desktop,
        listen: EngineListen::Disabled,
        seed_viewer_public: false,
        log_label: "recovery-test",
        audit_origin: "tauri",
        // デスクトップと同じ（保存設定を読めなければ止める）。
        store_config_required: true,
        store_dir_check_required: true,
    }
}

fn storage(message: &str) -> BantoError {
    BantoError::Storage(message.to_string())
}

fn db_storage(message: &str) -> EngineStartError {
    EngineStartError::Database(InitDbError::Storage(storage(message)))
}

/// 失敗 → 分類の総当たり（モジュール doc「分類の根拠」）。
#[test]
fn classify_maps_every_failure() {
    let cases: Vec<(&str, EngineStartError, FailureKind)> = vec![
        (
            "A1",
            EngineStartError::InstanceLock(InstanceLockFailure::SymlinkDbPath {
                db: PathBuf::from("x"),
            }),
            FailureKind::AlreadyRunning,
        ),
        (
            "旧形式",
            EngineStartError::Database(InitDbError::Legacy(LegacyDatabase {
                path: PathBuf::from("x"),
                kind: crate::db::LegacyKind::BeforeSchemaReset,
            })),
            FailureKind::LegacyDatabase,
        ),
        (
            "DB ではない",
            db_storage("error returned from database: (code: 26) file is not a database"),
            FailureKind::DatabaseCorrupt,
        ),
        (
            "壊れている",
            db_storage("error returned from database: (code: 11) database disk image is malformed"),
            FailureKind::DatabaseCorrupt,
        ),
        (
            "新しい版",
            db_storage(
                "migration 20261101000000 was previously applied but is missing in the resolved migrations",
            ),
            FailureKind::DatabaseFromNewerVersion,
        ),
        (
            "開けない",
            db_storage("error returned from database: (code: 14) unable to open database file"),
            FailureKind::DatabaseUnopenable,
        ),
        (
            "Hub・壊れている",
            EngineStartError::Hub(storage("database disk image is malformed")),
            FailureKind::DatabaseCorrupt,
        ),
        (
            "Hub・書けない",
            EngineStartError::Hub(storage("attempt to write a readonly database")),
            FailureKind::DatabaseUnopenable,
        ),
        (
            "Hub・migration の文言でも新しい版にはしない",
            EngineStartError::Hub(storage("was previously applied but is missing")),
            FailureKind::DatabaseUnopenable,
        ),
        (
            "保存先を読めない",
            EngineStartError::StoreConfig {
                source: storage("mismatched types"),
                data_dir_unreadable: true,
            },
            FailureKind::StoreDirUnreadable,
        ),
        (
            "保存先を読めないが DB が壊れている",
            EngineStartError::StoreConfig {
                source: storage("database disk image is malformed"),
                data_dir_unreadable: true,
            },
            FailureKind::DatabaseCorrupt,
        ),
        (
            "保存先は読めるが保持期間を読めない",
            EngineStartError::StoreConfig {
                source: storage("mismatched types"),
                data_dir_unreadable: false,
            },
            FailureKind::DatabaseUnopenable,
        ),
        (
            "保存先に書き込めない",
            EngineStartError::StoreDirUnwritable {
                dir: PathBuf::from("E:/data"),
                reason: "作れませんでした".to_string(),
            },
            FailureKind::StoreDirUnwritable,
        ),
        (
            "待ち受け",
            EngineStartError::Listen {
                config: banto_server::ServerConfig {
                    bind: "127.0.0.1".to_string(),
                    port: 1,
                },
                source: storage("in use"),
            },
            FailureKind::Other,
        ),
    ];
    for (name, err, expected) in cases {
        assert_eq!(classify(&err), expected, "{name}: {err:?}");
    }
}

/// 分類 → 窓と選択肢（オーナー決定の表）。対処を選ぶ窓は必ず「終了」で終わり、
/// 確認をはさむのは作り直す・戻す・既定に戻すだけ。
#[test]
fn plan_matches_the_owner_table() {
    use RecoveryChoice::*;
    let cases = [
        (FailureKind::AlreadyRunning, WindowPlan::ExistingNotice),
        (FailureKind::LegacyDatabase, WindowPlan::ExistingNotice),
        (
            FailureKind::StoreDirUnreadable,
            WindowPlan::Choices(vec![ChooseStoreDir, ResetStoreDir, Quit]),
        ),
        (
            FailureKind::StoreDirUnwritable,
            WindowPlan::Choices(vec![RetryStoreCheck, ChooseStoreDir, ResetStoreDir, Quit]),
        ),
        (
            FailureKind::DatabaseCorrupt,
            WindowPlan::Choices(vec![RestoreBackup, RecreateDatabase, Quit]),
        ),
        (
            FailureKind::DatabaseUnopenable,
            WindowPlan::Choices(vec![RestoreBackup, RecreateDatabase, Quit]),
        ),
        (
            FailureKind::DatabaseFromNewerVersion,
            WindowPlan::Choices(vec![Quit]),
        ),
        (FailureKind::Other, WindowPlan::Choices(vec![Quit])),
    ];
    for (kind, expected) in cases {
        let actual = plan(kind);
        assert_eq!(actual, expected, "{kind:?}");
        if let WindowPlan::Choices(choices) = actual {
            assert_eq!(choices.last(), Some(&Quit), "{kind:?}");
        }
    }
    let confirmed: Vec<_> = [
        RetryStoreCheck,
        ChooseStoreDir,
        ResetStoreDir,
        RestoreBackup,
        RecreateDatabase,
        Quit,
    ]
    .into_iter()
    .filter(|choice| choice.needs_confirmation())
    .collect();
    assert_eq!(
        confirmed,
        vec![ResetStoreDir, RestoreBackup, RecreateDatabase]
    );
}

#[test]
fn choice_names_round_trip_and_unknown_names_are_refused() {
    for choice in [
        RecoveryChoice::RetryStoreCheck,
        RecoveryChoice::ChooseStoreDir,
        RecoveryChoice::ResetStoreDir,
        RecoveryChoice::RestoreBackup,
        RecoveryChoice::RecreateDatabase,
        RecoveryChoice::Quit,
    ] {
        assert_eq!(RecoveryChoice::parse(choice.as_str()), Some(choice));
    }
    assert_eq!(RecoveryChoice::parse("deleteEverything"), None);
    assert_eq!(RecoveryChoice::parse(""), None);
}

/// 日時はローカル時刻（JST なら UTC + 9 時間、日付もまたぐ）。
#[test]
fn broken_stamp_is_local_time() {
    // 2026-10-12T06:30:05Z
    let epoch_ms = 1_791_786_605_000;
    let jst = 9 * 3_600_000;
    assert_eq!(broken_stamp(epoch_ms, 0), "20261012-063005");
    assert_eq!(broken_stamp(epoch_ms, jst), "20261012-153005");
    // 2026-10-12T20:00:00Z は JST では翌日の 05:00。
    assert_eq!(
        broken_stamp(epoch_ms + (13 * 3600 + 29 * 60 + 55) * 1000, jst),
        "20261013-050000"
    );
}

async fn start(dir: &Path) -> RunningEngine {
    EngineRuntime::start(config(dir)).await.expect("start")
}

async fn stop(engine: RunningEngine) {
    let report = engine.shutdown(Duration::from_secs(10)).await;
    assert!(!report.timed_out);
    drop(engine);
}

/// 起動して閉じた DB（`-wal` は畳まれている）を、SQLite の DB ではない中身で
/// 上書きする。
async fn make_corrupt_db(dir: &Path) -> PathBuf {
    stop(start(dir).await).await;
    let db_path = dir.join("chronogazer.sqlite3");
    std::fs::write(
        &db_path,
        b"this is not a sqlite database at all".repeat(200),
    )
    .expect("DB を壊す");
    db_path
}

async fn audit_rows(engine: &RunningEngine, action: &str) -> Vec<String> {
    sqlx::query_scalar::<_, String>("SELECT COALESCE(detail, '') FROM audit_log WHERE action = ?")
        .bind(action)
        .fetch_all(&engine.services().pool)
        .await
        .expect("audit_log")
}

/// 壊れた DB は「壊れている」に分類され（SQLite の実際の文言で）、排他は返って
/// いる。作り直すと旧 DB は `.broken-<日時>` に**中身のまま**残り、新しい DB で
/// 起動でき、監査に `startup_recovery` が残る。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn corrupt_db_is_classified_then_recreated_keeping_the_old_file() {
    let dir = TempDir::new();
    let db_path = make_corrupt_db(dir.path()).await;
    let corrupt_bytes = std::fs::read(&db_path).expect("read");

    let err = match EngineRuntime::start(config(dir.path())).await {
        Ok(_) => panic!("壊れた DB で起動できた"),
        Err(err) => err,
    };
    assert_eq!(classify(&err), FailureKind::DatabaseCorrupt, "{err}");
    drop(acquire_for_db(&db_path, ChronoGazerHost::Serve).expect("失敗の後は排他が返っている"));

    let Prepared { lock, set_aside } = recreate_database(&db_path, "20261012-153005")
        .await
        .expect("名前を変える");
    let broken = dir
        .path()
        .join("chronogazer.sqlite3.broken-20261012-153005");
    assert_eq!(set_aside.broken_db.as_deref(), Some(broken.as_path()));
    assert_eq!(
        set_aside.broken_file_name().as_deref(),
        Some("chronogazer.sqlite3.broken-20261012-153005")
    );
    assert_eq!(std::fs::read(&broken).expect("旧 DB が残る"), corrupt_bytes);
    assert!(!db_path.exists());

    let note = RecoveryNote::new(RecoveryChoice::RecreateDatabase)
        .with("brokenFileName", set_aside.broken_file_name().unwrap());
    // 排他は対処からエンジンへ途切れずに引き継ぐ（その間に別のプロセスは取れない）。
    assert!(acquire_for_db(&db_path, ChronoGazerHost::Serve).is_err());
    // banto v8（`crate::store_generation`）: 作り直した DB は新しい世代なので、前の DB の
    // 保存先（既定の `data/history`）には書かずに止まる（旧い履歴が新しいタグに混ざらない）。
    let store = dir.path().join("data").join("history");
    let store_before = snapshot(&store);
    let err = match EngineRuntime::open_with_recovery_lock(config(dir.path()), lock)
        .await
        .expect("新しい DB を開ける")
        .with_recovery_note(note)
        .start()
        .await
    {
        Ok(_) => panic!("前の DB の保存先で起動した"),
        Err(err) => err,
    };
    assert!(
        matches!(
            err,
            EngineStartError::StoreGeneration(crate::store_generation::ClaimError::Refused {
                refusal: crate::store_generation::StoreRefusal::OtherGeneration { .. },
                ..
            })
        ),
        "{err:?}"
    );
    assert_eq!(classify(&err), FailureKind::StoreDirRefused);
    assert_eq!(snapshot(&store), store_before, "前の保存先に触れた");
    // 前の保存先を退けると、新しい DB で起動できる。
    std::fs::rename(&store, dir.path().join("data").join("history-old")).unwrap();
    let engine = EngineRuntime::start(config(dir.path()))
        .await
        .expect("新しい DB で起動できる");
    let rows = audit_rows(&engine, "startup_recovery").await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    let detail: serde_json::Value = serde_json::from_str(&rows[0]).expect("JSON");
    assert_eq!(detail["choice"], "recreateDatabase");
    assert_eq!(
        detail["brokenFileName"],
        "chronogazer.sqlite3.broken-20261012-153005"
    );
    stop(engine).await;
    assert!(broken.exists(), "作り直した後も旧 DB は残る");
}

/// `-wal`・`-shm` も本体と同じ名前＋接尾辞へ移り（古い WAL を元の名前に
/// 残さない）、同じ名前が既にあれば `-1` を付ける（既存のものは触らない）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_aside_moves_sidecars_and_avoids_existing_names() {
    let dir = TempDir::new();
    let db = dir.path().join("chronogazer.sqlite3");
    std::fs::write(&db, b"main").unwrap();
    std::fs::write(dir.path().join("chronogazer.sqlite3-wal"), b"wal").unwrap();
    std::fs::write(dir.path().join("chronogazer.sqlite3-shm"), b"shm").unwrap();
    let earlier = dir
        .path()
        .join("chronogazer.sqlite3.broken-20261012-153005");
    std::fs::write(&earlier, b"earlier").unwrap();

    let set_aside = set_aside_broken_db(&db, "20261012-153005")
        .await
        .expect("名前を変える");
    let base = dir
        .path()
        .join("chronogazer.sqlite3.broken-20261012-153005-1");
    assert_eq!(set_aside.broken_db.as_deref(), Some(base.as_path()));
    assert_eq!(std::fs::read(&base).unwrap(), b"main");
    assert_eq!(
        std::fs::read(
            dir.path()
                .join("chronogazer.sqlite3.broken-20261012-153005-1-wal")
        )
        .unwrap(),
        b"wal"
    );
    assert_eq!(
        std::fs::read(
            dir.path()
                .join("chronogazer.sqlite3.broken-20261012-153005-1-shm")
        )
        .unwrap(),
        b"shm"
    );
    assert_eq!(std::fs::read(&earlier).unwrap(), b"earlier");
    for name in [
        "chronogazer.sqlite3",
        "chronogazer.sqlite3-wal",
        "chronogazer.sqlite3-shm",
    ] {
        assert!(!dir.path().join(name).exists(), "{name} が元の名前に残った");
    }
    // 付随ファイルが本体より先（本体だけ動いて WAL が残る瞬間を作らない）。
    assert_eq!(set_aside.moved.last().map(|(from, _)| from), Some(&db));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_aside_without_a_db_does_nothing() {
    let dir = TempDir::new();
    let set_aside = set_aside_broken_db(&dir.path().join("chronogazer.sqlite3"), "x")
        .await
        .expect("無ければ何もしない");
    assert_eq!(set_aside.broken_db, None);
    assert!(set_aside.moved.is_empty());
}

/// 本体の名前を変えられない（他のプロセスが共有なしで開いている）ときは、先に
/// 移した `-wal` を元に戻して理由を返す（黙って半端な状態にしない）。
#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_aside_rolls_back_when_the_db_is_locked() {
    use std::os::windows::fs::OpenOptionsExt;
    let dir = TempDir::new();
    let db = dir.path().join("chronogazer.sqlite3");
    let wal = dir.path().join("chronogazer.sqlite3-wal");
    std::fs::write(&db, b"main").unwrap();
    std::fs::write(&wal, b"wal").unwrap();
    let held = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&db)
        .expect("共有なしで開く");

    let err = set_aside_with(
        &db,
        "20261012-153005",
        RetryPolicy {
            attempts: 2,
            interval: Duration::from_millis(10),
        },
    )
    .await
    .expect_err("開かれている DB の名前は変えられない");
    assert!(err.0.contains("名前を変えられませんでした"), "{err}");
    drop(held);
    assert_eq!(
        std::fs::read(&wal).unwrap(),
        b"wal",
        "WAL が元に戻っていない"
    );
    assert_eq!(std::fs::read(&db).unwrap(), b"main");
    assert!(!dir
        .path()
        .join("chronogazer.sqlite3.broken-20261012-153005-wal")
        .exists());
}

/// バックアップから戻す: 今のリストアと同じ経路（予約 → 次の `open` で適用、
/// `restore_applied`）で、バックアップの時点の中身に戻る。壊れた DB は
/// `.broken-<日時>` に残る。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restore_from_backup_uses_the_restore_path_and_keeps_the_broken_db() {
    let dir = TempDir::new();
    let engine = start(dir.path()).await;
    let settings = engine.services().settings.clone();
    settings.set("test.marker", "before").await.unwrap();
    let backup = engine
        .services()
        .backup
        .create()
        .await
        .expect("バックアップ");
    settings.set("test.marker", "after").await.unwrap();
    stop(engine).await;
    let db_path = dir.path().join("chronogazer.sqlite3");
    std::fs::write(&db_path, b"garbage".repeat(500)).unwrap();

    let listed = list_backups(&db_path).await.expect("一覧");
    assert_eq!(
        listed
            .iter()
            .map(|b| b.file_name.clone())
            .collect::<Vec<_>>(),
        vec![backup.file_name.clone()]
    );

    let Prepared { lock, set_aside } =
        restore_from_backup(&db_path, &backup.file_name, "20261012-153005")
            .await
            .expect("戻す");
    // 適用は対処の中で済んでいる（予約は残っていない）。
    assert!(db_path.exists());
    assert!(!dir
        .path()
        .join("backups/chronogazer.sqlite3/restore-pending.sqlite3")
        .exists());
    let broken = set_aside.broken_db.clone().expect("壊れた DB を退けた");
    assert_eq!(std::fs::read(&broken).unwrap(), b"garbage".repeat(500));

    let note = RecoveryNote::new(RecoveryChoice::RestoreBackup)
        .with("backupFileName", backup.file_name.clone())
        .with("brokenFileName", set_aside.broken_file_name().unwrap());
    let engine = EngineRuntime::open_with_recovery_lock(config(dir.path()), lock)
        .await
        .expect("戻した DB を開ける")
        .with_recovery_note(note)
        .start()
        .await
        .expect("起動");
    assert_eq!(
        engine.services().settings.get("test.marker").await.unwrap(),
        Some("before".to_string())
    );
    assert_eq!(audit_rows(&engine, "restore_applied").await.len(), 1);
    let rows = audit_rows(&engine, "startup_recovery").await;
    let detail: serde_json::Value = serde_json::from_str(&rows[0]).expect("JSON");
    assert_eq!(detail["choice"], "restoreBackup");
    assert_eq!(detail["backupFileName"], backup.file_name.as_str());
    stop(engine).await;
}

/// 壊れたバックアップを選んだら理由を返し、DB には触れない（名前も変えず、
/// リストアの予約も残さない）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_broken_backup_is_refused_without_touching_the_db() {
    let dir = TempDir::new();
    let db_path = make_corrupt_db(dir.path()).await;
    let scope = dir.path().join("backups").join("chronogazer.sqlite3");
    std::fs::create_dir_all(&scope).unwrap();
    std::fs::write(scope.join("banto-20260101-000000.sqlite3"), b"not sqlite").unwrap();

    let err = restore_from_backup(&db_path, "banto-20260101-000000.sqlite3", "x")
        .await
        .expect_err("壊れたバックアップは使えない");
    assert!(err.0.contains("banto-20260101-000000.sqlite3"), "{err}");
    // 「validation failed」だけでなく、何がだめかを出す。
    assert!(!err.0.contains("validation failed"), "{err}");
    assert!(
        err.0.contains("整合性チェック") || err.0.contains("開けませんでした"),
        "{err}"
    );
    assert!(db_path.exists(), "DB の名前を変えた");
    assert!(!scope.join("restore-pending.sqlite3").exists());
    // ファイル名の外へは出られない。
    assert!(restore_from_backup(&db_path, "../chronogazer.sqlite3", "x")
        .await
        .is_err());
    assert!(db_path.exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_backups_is_an_empty_list() {
    let dir = TempDir::new();
    assert!(list_backups(&dir.path().join("chronogazer.sqlite3"))
        .await
        .expect("一覧")
        .is_empty());
}

/// 保存先（`data.dir`）を読めない DB を作り、`start` が `StoreDirUnreadable` に
/// 分類される失敗を返すことを確かめる。
async fn make_unreadable_data_dir(dir: &Path) {
    let opened = EngineRuntime::open(config(dir)).await.expect("open");
    sqlx::query(
        "INSERT INTO settings (key, value) VALUES ('data.dir', X'FF00FE') \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .execute(opened.pool())
    .await
    .expect("data.dir を読めない値にする");
    let err = match opened.start().await {
        Ok(_) => panic!("保存先を読めないのに起動した"),
        Err(err) => err,
    };
    assert_eq!(classify(&err), FailureKind::StoreDirUnreadable, "{err}");
}

/// 保存先を選び直す・既定に戻す: 設定に保存され、そのまま起動を続けられ、監査に
/// 残る。書き込めないフォルダは理由を返し、設定を変えない。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn store_dir_choice_is_saved_and_startup_continues() {
    let dir = TempDir::new();
    make_unreadable_data_dir(dir.path()).await;

    // 書き込めない（フォルダの代わりにファイルがある）場所は断る。
    let blocked = dir.path().join("blocked");
    std::fs::write(&blocked, b"file").unwrap();
    let opened = EngineRuntime::open(config(dir.path())).await.expect("open");
    let err = apply_store_dir(
        &opened.settings(),
        dir.path(),
        &StoreDirChoice::Folder(blocked.clone()),
    )
    .await
    .expect_err("ファイルの場所は保存先にできない");
    assert!(err.0.contains(&blocked.display().to_string()), "{err}");
    assert!(
        read_data_dir(&opened.settings()).await.is_err(),
        "失敗したのに設定を書き換えた"
    );
    // 相対パスも断る。
    assert!(apply_store_dir(
        &opened.settings(),
        dir.path(),
        &StoreDirChoice::Folder(PathBuf::from("relative")),
    )
    .await
    .is_err());

    // 選び直す。
    let chosen = dir.path().join("chosen-data");
    let note = apply_store_dir(
        &opened.settings(),
        dir.path(),
        &StoreDirChoice::Folder(chosen.clone()),
    )
    .await
    .expect("選んだフォルダを保存する");
    assert_eq!(note.choice, RecoveryChoice::ChooseStoreDir);
    let engine = opened
        .with_recovery_note(note)
        .start()
        .await
        .expect("起動を続ける");
    assert_eq!(
        read_data_dir(&engine.services().settings).await.unwrap(),
        Some(chosen.display().to_string())
    );
    assert!(chosen.is_dir());
    let rows = audit_rows(&engine, "startup_recovery").await;
    let detail: serde_json::Value = serde_json::from_str(&rows[0]).expect("JSON");
    assert_eq!(detail["choice"], "chooseStoreDir");
    assert_eq!(detail["dataDir"], chosen.display().to_string());
    stop(engine).await;

    // 既定に戻す。
    make_unreadable_data_dir(dir.path()).await;
    let opened = EngineRuntime::open(config(dir.path())).await.expect("open");
    let note = apply_store_dir(&opened.settings(), dir.path(), &StoreDirChoice::Default)
        .await
        .expect("既定に戻す");
    assert_eq!(note.choice, RecoveryChoice::ResetStoreDir);
    // 窓・監査に出す既定の場所は `./` を含まない（同じ場所）。
    assert_eq!(
        note.detail["resolvedDataDir"],
        dir.path()
            .join("data")
            .join("history")
            .display()
            .to_string()
    );
    let engine = opened
        .with_recovery_note(note)
        .start()
        .await
        .expect("起動を続ける");
    assert_eq!(
        read_data_dir(&engine.services().settings).await.unwrap(),
        Some("./data/history".to_string())
    );
    stop(engine).await;
}

/// 「もう一度確かめる」で起動できたら、監査に `startup_recovery`（choice=retryStoreCheck）が
/// 残る（#594 のオーナー再レビュー）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_successful_retry_is_recorded_in_the_audit() {
    let dir = TempDir::new();
    let engine = EngineRuntime::open(config(dir.path()))
        .await
        .expect("open")
        .with_recovery_note(RecoveryNote::new(RecoveryChoice::RetryStoreCheck))
        .start()
        .await
        .expect("起動できる");
    let rows = audit_rows(&engine, "startup_recovery").await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    let detail: serde_json::Value = serde_json::from_str(&rows[0]).expect("JSON");
    assert_eq!(detail["choice"], "retryStoreCheck");
    stop(engine).await;
}

/// 保存先がまだ書けないまま「もう一度確かめる」を押しても成功の監査は残らず、
/// 書けるようにして成功したときだけ 1 行残る（#594 のオーナーレビュー P2）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_retry_is_audited_only_after_start_succeeds() {
    let dir = TempDir::new();
    let blocked = dir.path().join("usb-data");
    std::fs::write(&blocked, b"not a folder").unwrap();
    let opened = EngineRuntime::open(config(dir.path())).await.expect("open");
    crate::settings::set_data_dir(&opened.settings(), &blocked.display().to_string())
        .await
        .unwrap();
    opened.close().await;
    let retry = || async {
        EngineRuntime::open(config(dir.path()))
            .await
            .expect("open")
            .with_recovery_note(RecoveryNote::new(RecoveryChoice::RetryStoreCheck))
            .start()
            .await
    };

    // 書けないまま 2 回押しても、成功の行は増えない。
    for _ in 0..2 {
        match retry().await {
            Ok(_) => panic!("書き込めない保存先で起動した"),
            Err(err) => assert_eq!(classify(&err), FailureKind::StoreDirUnwritable, "{err}"),
        }
    }
    let engine = EngineRuntime::start(config(dir.path())).await;
    assert!(engine.is_err());
    std::fs::remove_file(&blocked).unwrap();
    let engine = retry().await.expect("書けるようになったら起動できる");
    let rows = audit_rows(&engine, "startup_recovery").await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    let detail: serde_json::Value = serde_json::from_str(&rows[0]).expect("JSON");
    assert_eq!(detail["choice"], "retryStoreCheck");
    stop(engine).await;
}

/// `OpenedEngine::close` は排他を返す（同じ DB をすぐ開き直せる）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn closing_an_opened_engine_releases_the_lock() {
    let dir = TempDir::new();
    let opened = EngineRuntime::open(config(dir.path())).await.expect("open");
    let db_path = opened.db_path().to_path_buf();
    opened.close().await;
    drop(acquire_for_db(&db_path, ChronoGazerHost::Serve).expect("排他が返っている"));
}

/// 保存先に書き込めない（フォルダの代わりに同名のファイルがある）と、`start` は
/// `StoreDirUnwritable` で止まり（保存先の「書き込めない」の窓・「もう一度確かめる」
/// つき）、排他と pool を返し DB の設定は変えない。書けるようにしてから同じ設定で
/// やり直す（=「もう一度確かめる」）と起動を続ける。`banto-serve` の設定
/// （`store_dir_check_required: false`）は書き込みを確かめないが、保存先の世代を確かめられない
/// ので止まる（banto v8、`crate::store_generation`）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unwritable_store_dir_stops_until_it_becomes_writable() {
    let dir = TempDir::new();
    let blocked = dir.path().join("usb-data");
    std::fs::write(&blocked, b"not a folder").unwrap();
    let opened = EngineRuntime::open(config(dir.path())).await.expect("open");
    crate::settings::set_data_dir(&opened.settings(), &blocked.display().to_string())
        .await
        .unwrap();
    opened.close().await;

    let err = match EngineRuntime::start(config(dir.path())).await {
        Ok(_) => panic!("書き込めない保存先で起動した"),
        Err(err) => err,
    };
    assert_eq!(classify(&err), FailureKind::StoreDirUnwritable, "{err}");
    assert_eq!(
        plan(classify(&err)),
        WindowPlan::Choices(vec![
            RecoveryChoice::RetryStoreCheck,
            RecoveryChoice::ChooseStoreDir,
            RecoveryChoice::ResetStoreDir,
            RecoveryChoice::Quit,
        ])
    );
    assert!(err.to_string().contains("usb-data"), "{err}");
    let db_path = dir.path().join("chronogazer.sqlite3");
    drop(acquire_for_db(&db_path, ChronoGazerHost::Serve).expect("失敗の後は排他が返っている"));
    assert_eq!(
        std::fs::read(&blocked).unwrap(),
        b"not a folder",
        "保存先の場所の中身に触れた"
    );

    // banto-serve の設定は書き込みを確かめないが、保存先の世代（banto v8、
    // `crate::store_generation`）を確かめられない保存先では起動しない（安全側）。
    let err = match EngineRuntime::start(EngineConfig {
        drivers: crate::test_support::drivers(),
        store_dir_check_required: false,
        store_config_required: false,
        ..config(dir.path())
    })
    .await
    {
        Ok(_) => panic!("banto-serve が世代を確かめられない保存先で起動した"),
        Err(err) => err,
    };
    assert!(
        matches!(
            err,
            EngineStartError::StoreGeneration(crate::store_generation::ClaimError::Io { .. })
        ),
        "{err:?}"
    );
    assert_eq!(std::fs::read(&blocked).unwrap(), b"not a folder");

    // 書けるようにして「もう一度確かめる」（同じ設定でやり直す）。
    std::fs::remove_file(&blocked).unwrap();
    let engine = EngineRuntime::start(config(dir.path()))
        .await
        .expect("書けるようになったら起動を続ける");
    assert_eq!(
        read_data_dir(&engine.services().settings).await.unwrap(),
        Some(blocked.display().to_string()),
        "設定は変えない"
    );
    assert!(blocked.is_dir());
    stop(engine).await;
}

/// ディレクトリの中身（相対パス → 中身）。何も変わっていないことを比べる。
fn snapshot(dir: &Path) -> std::collections::BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut std::collections::BTreeMap<String, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.strip_prefix(root).unwrap().display().to_string();
            if path.is_dir() {
                out.insert(name.clone(), Vec::new());
                walk(root, &path, out);
            } else if !name.ends_with(".instance.lock") {
                // A1 の排他のファイル（中身は持ち主の診断）は比べない。
                out.insert(name, std::fs::read(&path).unwrap());
            }
        }
    }
    let mut out = std::collections::BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

/// #594 のオーナーレビュー P1: 同じ DB を別のプロセス（`banto-serve`）が使用中なら、
/// 作り直す・バックアップから戻すは排他を取れずに理由を返し、DB・付随ファイル・
/// リストアの予約が**一切変わらない**。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recovery_does_nothing_while_another_process_holds_the_db() {
    let dir = TempDir::new();
    let engine = start(dir.path()).await;
    let backup = engine
        .services()
        .backup
        .create()
        .await
        .expect("バックアップ");
    stop(engine).await;
    let db_path = make_corrupt_db_keeping_backups(dir.path());
    std::fs::write(dir.path().join("chronogazer.sqlite3-wal"), b"wal").unwrap();
    let before = snapshot(dir.path());

    let _serve = acquire_for_db(&db_path, ChronoGazerHost::Serve).expect("banto-serve が使用中");
    let err = recreate_database(&db_path, "20261012-153005")
        .await
        .expect_err("使用中の DB は作り直さない");
    assert!(err.0.contains("使用中"), "{err}");
    let err = restore_from_backup(&db_path, &backup.file_name, "20261012-153005")
        .await
        .expect_err("使用中の DB には戻さない");
    assert!(err.0.contains("使用中"), "{err}");
    assert_eq!(
        snapshot(dir.path()),
        before,
        "使用中の DB のまわりのファイルが変わった"
    );
}

/// 起動して閉じた DB を壊す（バックアップは残す）。
fn make_corrupt_db_keeping_backups(dir: &Path) -> PathBuf {
    let db_path = dir.join("chronogazer.sqlite3");
    std::fs::write(
        &db_path,
        b"this is not a sqlite database at all".repeat(200),
    )
    .expect("DB を壊す");
    db_path
}

/// #594 のレビュー P1（Codex）: 予約は検査を通ったが適用に失敗したら、空の DB で
/// 続けずに理由を返し、予約を取り消して退けた DB を元の名前に戻す（何も消さない）。
/// 排他も返る。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_restore_apply_puts_everything_back() {
    let dir = TempDir::new();
    let engine = start(dir.path()).await;
    let backup = engine
        .services()
        .backup
        .create()
        .await
        .expect("バックアップ");
    stop(engine).await;
    let db_path = make_corrupt_db_keeping_backups(dir.path());
    std::fs::write(dir.path().join("chronogazer.sqlite3-wal"), b"wal").unwrap();
    let before = snapshot(dir.path());

    // 適用の I/O エラー（最終の rename の失敗など）を注入する。
    let err = restore_with(&db_path, &backup.file_name, "20261012-153005", |_| async {
        Err(BantoError::Other("注入した適用の失敗".to_string()))
    })
    .await
    .expect_err("適用できなければ失敗");
    assert!(err.0.contains("適用できませんでした"), "{err}");
    assert!(err.0.contains("注入した適用の失敗"), "{err}");
    assert!(err.0.contains("元の名前に戻しました"), "{err}");
    assert_eq!(snapshot(dir.path()), before, "元の状態に戻っていない");

    // 適用されなかった（検査に通らなかった）場合も同じ。
    let err = restore_with(&db_path, &backup.file_name, "20261012-153005", |_| async {
        Ok(None)
    })
    .await
    .expect_err("適用されなければ失敗");
    assert!(err.0.contains("適用されませんでした"), "{err}");
    assert_eq!(snapshot(dir.path()), before, "元の状態に戻っていない");
    drop(acquire_for_db(&db_path, ChronoGazerHost::Serve).expect("失敗の後は排他が返っている"));
}

/// 書き込み確認の試し書きは、既にあるファイル（旧方式の PID だけの名前を含む）を壊さず、
/// 終わったら自分のファイルを残さない。
#[tokio::test]
async fn write_check_leaves_existing_files_untouched_and_cleans_up() {
    let dir = TempDir::new();
    let legacy = dir.path().join(format!(
        ".chronogazer-write-check-{}.tmp",
        std::process::id()
    ));
    std::fs::write(&legacy, b"someone else's data").unwrap();
    let other = dir.path().join("data.bin");
    std::fs::write(&other, b"keep").unwrap();

    for _ in 0..3 {
        check_writable_dir(dir.path()).await.expect("書ける");
    }

    assert_eq!(std::fs::read(&legacy).unwrap(), b"someone else's data");
    assert_eq!(std::fs::read(&other).unwrap(), b"keep");
    let mut names: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names.len(), 2, "試し書きが残った: {names:?}");
}

/// 試し書きが失敗しても（読み取り専用で開いたファイルへの書き込みで再現）、
/// `write_probe` はファイルを閉じてから戻るので、直後に消せる（開いたままのファイルを
/// 消せるかは共有モードに依るため、閉じてから消す順序を保つ。#594 のレビュー）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_probe_write_closes_the_file_before_it_is_removed() {
    let dir = TempDir::new();
    let path = dir.path().join("probe.tmp");
    std::fs::write(&path, b"x").unwrap();
    let file = tokio::fs::OpenOptions::new()
        .read(true)
        .open(&path)
        .await
        .unwrap();
    assert!(super::write_probe(file).await.is_err(), "書けない");
    std::fs::remove_file(&path).expect("閉じた後なので消せる");
}

/// banto v8 への追従（docs/banto-v8-upgrade-plan.md §5 の 2）: v7 の実プロファイル（v7 の DB =
/// 旧い記録の表・旧い 0003/0005、印の無い保存先）に v7 の予約（旧い DB をバックアップの予約として
/// 置いたもの）がある。起動は予約を当てず・消さず、旧い DB・履歴に書かずに旧形式で止まり、
/// プロファイルのファイルは byte で変わらない。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_banto_v7_profile_with_a_v7_reservation_stops_without_touching_anything() {
    let dir = TempDir::new();
    let db_path = dir.path().join("chronogazer.sqlite3");
    crate::db::write_banto_v7_db(&db_path).await;
    let old_store = dir.path().join("data");
    std::fs::create_dir_all(&old_store).unwrap();
    std::fs::write(old_store.join("20261101-001.sqlite3"), b"v7 rows").unwrap();
    let pending = BackupService::pending_restore_file(&db_path).unwrap();
    std::fs::create_dir_all(pending.parent().unwrap()).unwrap();
    crate::db::write_banto_v7_db(&pending).await;
    let before = snapshot(dir.path());

    let err = match EngineRuntime::start(config(dir.path())).await {
        Ok(_) => panic!("v7 のプロファイルで起動した"),
        Err(err) => err,
    };
    match &err {
        EngineStartError::Database(InitDbError::Legacy(legacy)) => {
            assert_eq!(legacy.kind, crate::db::LegacyKind::BeforeBantoV8);
            assert_eq!(legacy.path, db_path);
        }
        other => panic!("旧形式で止まるはず: {other:?}"),
    }
    assert_eq!(classify(&err), FailureKind::LegacyDatabase);
    assert_eq!(
        snapshot(dir.path()),
        before,
        "プロファイルのファイルが変わった"
    );
    assert!(pending.is_file(), "v7 の予約が消えた");
    drop(acquire_for_db(&db_path, ChronoGazerHost::Serve).expect("失敗の後は排他が返っている"));
}

/// 同じく §5 の 2: v8 の DB に v7 の予約だけがある（v7 で予約してから版を上げた）。予約は
/// 「不正」として消されず（banto は検査に通らない予約を起動時に消す）、適用もされず、予約の
/// ファイルを名指しした旧形式で止まる。DB・保存先・予約は byte で変わらない。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_banto_v7_reservation_over_a_v8_db_is_neither_applied_nor_deleted() {
    let dir = TempDir::new();
    stop(start(dir.path()).await).await;
    let db_path = dir.path().join("chronogazer.sqlite3");
    let pending = BackupService::pending_restore_file(&db_path).unwrap();
    std::fs::create_dir_all(pending.parent().unwrap()).unwrap();
    crate::db::write_banto_v7_db(&pending).await;
    let before = snapshot(dir.path());

    let err = match EngineRuntime::start(config(dir.path())).await {
        Ok(_) => panic!("v7 の予約で起動した"),
        Err(err) => err,
    };
    match &err {
        EngineStartError::Database(InitDbError::Legacy(legacy)) => {
            assert_eq!(legacy.kind, crate::db::LegacyKind::BeforeBantoV8);
            assert_eq!(legacy.path, pending);
            assert!(
                err.to_string().contains(&pending.display().to_string()),
                "{err}"
            );
        }
        other => panic!("旧形式で止まるはず: {other:?}"),
    }
    assert_eq!(
        snapshot(dir.path()),
        before,
        "プロファイルのファイルが変わった"
    );
}

/// 新しい版の予約を置いて止めた v8 のプロファイル（予約は検査を通る正しいもの）。
async fn a_v8_profile_with_a_v8_reservation(root: &Path) -> PathBuf {
    let engine = start(root).await;
    let backup = engine.services().backup.create().await.expect("backup");
    engine
        .services()
        .backup
        .stage_restore_from_file(&backup.file_name)
        .await
        .expect("予約できる");
    stop(engine).await;
    let db_path = root.join("chronogazer.sqlite3");
    let pending = BackupService::pending_restore_file(&db_path).unwrap();
    assert!(pending.is_file());
    pending
}

/// 予約があるのに今の DB の形式を確かめられないとき、予約を当てずに止まり、DB・`-wal`・
/// 予約は byte で変わらない（オーナーのレビュー P2）。
async fn assert_undetermined_stop_touches_nothing(root: &Path, pending: &Path, db_missing: bool) {
    let db_path = root.join("chronogazer.sqlite3");
    let before = snapshot(root);
    let err = match EngineRuntime::start(config(root)).await {
        Ok(_) => panic!("形式を確かめられない DB の上に予約を当てて起動した"),
        Err(err) => err,
    };
    match &err {
        EngineStartError::Database(InitDbError::Undetermined(undetermined)) => {
            assert_eq!(undetermined.path, db_path);
            assert_eq!(undetermined.pending, pending);
            assert_eq!(undetermined.db_missing, db_missing);
            let text = err.to_string();
            assert!(text.contains("予約"), "{text}");
            assert!(text.contains(&pending.display().to_string()), "{text}");
        }
        other => panic!("確かめられないで止まるはず: {other:?}"),
    }
    assert_eq!(classify(&err), FailureKind::Other);
    assert_eq!(snapshot(root), before, "プロファイルのファイルが変わった");
    assert!(pending.is_file(), "予約が消えた");
    drop(acquire_for_db(&db_path, ChronoGazerHost::Serve).expect("失敗の後は排他が返っている"));
}

/// (a) 空でない `-wal` が残った今の DB（前の起動が落ちたまま）＋ 新版の予約: `immutable` では
/// `-wal` を読めず形式を確かめられないので、予約を当てずに止まる。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reservation_over_a_db_with_a_leftover_wal_is_not_applied() {
    let dir = TempDir::new();
    let pending = a_v8_profile_with_a_v8_reservation(dir.path()).await;
    // 閉じきっていない変更の残り（中身は SQLite が読むことは無い。止まる前に読まない）。
    std::fs::write(
        dir.path().join("chronogazer.sqlite3-wal"),
        vec![0x37u8; 4096],
    )
    .unwrap();
    assert_undetermined_stop_touches_nothing(dir.path(), &pending, false).await;
}

/// (b) 開けない今の DB ＋ 新版の予約: 予約を当てずに止まる。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reservation_over_an_unreadable_db_is_not_applied() {
    let dir = TempDir::new();
    let pending = a_v8_profile_with_a_v8_reservation(dir.path()).await;
    std::fs::write(dir.path().join("chronogazer.sqlite3"), vec![0x5au8; 8192]).unwrap();
    assert_undetermined_stop_touches_nothing(dir.path(), &pending, false).await;
}

/// 今の DB の本体と隣の `-wal`・`-shm`・`-journal` を消す（本体だけを移した・初めての戻しの再現）。
fn remove_db_and_side_files(root: &Path) {
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let path = root.join(format!("chronogazer.sqlite3{suffix}"));
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => panic!("{} を消せない: {err}", path.display()),
        }
    }
}

/// (c) 今の DB の本体が無いのに、空でない `-wal`（または `-journal`）が残っている ＋ 新版の予約:
/// banto の予約の適用は本体が無くても `-wal`・`-shm` を消し、`-journal` は見ないので、「無い」
/// として当てずに止まる。残りのファイルと予約は byte で変わらない（オーナーのレビュー P2）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reservation_over_a_missing_db_with_leftover_side_files_is_not_applied() {
    for suffix in ["-wal", "-journal"] {
        let dir = TempDir::new();
        let pending = a_v8_profile_with_a_v8_reservation(dir.path()).await;
        remove_db_and_side_files(dir.path());
        std::fs::write(
            dir.path().join(format!("chronogazer.sqlite3{suffix}")),
            vec![0x37u8; 4096],
        )
        .unwrap();
        assert_undetermined_stop_touches_nothing(dir.path(), &pending, true).await;
        assert!(
            !dir.path().join("chronogazer.sqlite3").exists(),
            "{suffix}: DB の本体が作られた"
        );
    }
}

/// (d) 今の DB の本体も、空でない `-wal`・`-journal` も無い ＋ 新版の予約（初めての戻し）: 空の
/// `-wal`・`-journal` は数えず、予約が当たって起動する。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reservation_without_a_db_is_applied() {
    for empty_side in [None, Some("-wal"), Some("-journal")] {
        let dir = TempDir::new();
        let engine = start(dir.path()).await;
        engine
            .services()
            .settings
            .set("probe", "at-backup")
            .await
            .unwrap();
        let backup = engine.services().backup.create().await.expect("backup");
        engine
            .services()
            .backup
            .stage_restore_from_file(&backup.file_name)
            .await
            .expect("予約できる");
        stop(engine).await;
        remove_db_and_side_files(dir.path());
        if let Some(suffix) = empty_side {
            std::fs::write(dir.path().join(format!("chronogazer.sqlite3{suffix}")), b"").unwrap();
        }

        let engine = start(dir.path()).await;
        assert_eq!(
            engine
                .services()
                .settings
                .get("probe")
                .await
                .unwrap()
                .as_deref(),
            Some("at-backup"),
            "{empty_side:?}"
        );
        assert!(engine.services().backup.pending_restore().await.is_none());
        stop(engine).await;
    }
}

/// 同じく §5 の 2: 新しい版で取ったバックアップは、予約 → 再起動で正しく戻る（起動時の検査
/// `CandidateCheck` を通り、`restore_applied` が監査に残る）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_backup_taken_by_this_version_is_restored_on_the_next_start() {
    let dir = TempDir::new();
    let engine = start(dir.path()).await;
    let settings = &engine.services().settings;
    settings.set("probe", "at-backup").await.unwrap();
    let backup = engine.services().backup.create().await.expect("backup");
    settings.set("probe", "after-backup").await.unwrap();
    engine
        .services()
        .backup
        .stage_restore_from_file(&backup.file_name)
        .await
        .expect("予約できる");
    stop(engine).await;

    let engine = start(dir.path()).await;
    assert_eq!(
        engine
            .services()
            .settings
            .get("probe")
            .await
            .unwrap()
            .as_deref(),
        Some("at-backup")
    );
    assert!(engine.services().backup.pending_restore().await.is_none());
    assert_eq!(audit_rows(&engine, "restore_applied").await.len(), 1);
    stop(engine).await;
}
