//! [`super`] のテスト（#623 PR2a）。握る・閉じる・反証は Windows（Guarded）だけ。

use super::*;
use crate::guard::KnownFolders;
use crate::root::{open_root, Mode, RootSpec};
use sqlx::{ConnectOptions, Connection, Row};
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(f)
}

fn plain(expected: PathBuf) -> RootSpec {
    RootSpec {
        expected,
        known: KnownFolders::default(),
        owners: Vec::new(),
        create_access: None,
        create: true,
        mode: Mode::Plain,
    }
}

async fn create_and_insert(pool: &SqlitePool, rows: i64) {
    sqlx::query("CREATE TABLE IF NOT EXISTS t (v INTEGER)")
        .execute(pool)
        .await
        .unwrap();
    for v in 0..rows {
        sqlx::query("INSERT INTO t VALUES (?)")
            .bind(v)
            .execute(pool)
            .await
            .unwrap();
    }
}

async fn count(pool: &SqlitePool) -> i64 {
    sqlx::query("SELECT count(*) FROM t")
        .fetch_one(pool)
        .await
        .unwrap()
        .get(0)
}

/// `SQLITE_OK` でなければ失敗を返す。`:memory:` にはファイルが無いので
/// `SQLITE_FCNTL_PERSIST_WAL` は `SQLITE_NOTFOUND`。
#[test]
fn set_persist_wal_reports_a_failure() {
    let err = block_on(async {
        let mut conn = SqliteConnectOptions::new()
            .filename(":memory:")
            .connect()
            .await
            .unwrap();
        let result = set_persist_wal(&mut conn).await;
        conn.close().await.unwrap();
        result
    })
    .expect_err("PERSIST_WAL を立てられないのに通った");
    assert!(
        err.to_string().contains("SQLITE_FCNTL_PERSIST_WAL"),
        "{err}"
    );
}

/// sqlx 0.9 の挙動の固定: pool の `after_connect` の失敗は `acquire_timeout` までやり直され、
/// 理由の無い `PoolTimedOut` になる（だから [`PinnedSqlite::connect`] は先に 1 本で確かめる）。
#[test]
fn a_failed_after_connect_only_times_out_the_pool() {
    let err = block_on(async {
        SqlitePoolOptions::new()
            .acquire_timeout(Duration::from_millis(500))
            .after_connect(|conn, _meta| Box::pin(set_persist_wal(conn)))
            .connect_with(SqliteConnectOptions::new().filename(":memory:"))
            .await
    })
    .expect_err("PERSIST_WAL を立てられないのに開けた");
    assert!(matches!(err, sqlx::Error::PoolTimedOut), "{err}");
}

/// Plain は握らず、PERSIST_WAL も立てない（閉じれば SQLite が `-wal`・`-shm` を消す）。
#[test]
fn plain_does_not_hold_and_sqlite_removes_its_companions() {
    let dir = tempfile::tempdir().unwrap();
    let root = open_root(&plain(dir.path().join("root"))).unwrap();
    let pinned = root.as_dir().pin_sqlite("t.sqlite3").unwrap();
    assert!(!pinned.is_held());
    block_on(async {
        let pool = pinned.connect().await.unwrap();
        create_and_insert(&pool, 3).await;
        pool.close().await;
    });
    assert!(pinned.path().exists());
    assert!(!pinned.wal_path().exists() && !pinned.shm_path().exists());
    assert!(!root
        .as_dir()
        .final_path()
        .join("t.sqlite3-journal")
        .exists());
}

#[cfg(windows)]
mod windows {
    use super::*;
    use crate::acl::tests::windows::me;
    use crate::error::{Refusal, SafeRootError};

    fn guarded(expected: PathBuf) -> RootSpec {
        RootSpec {
            owners: vec![me()],
            mode: Mode::Guarded,
            ..plain(expected)
        }
    }

    /// 握ったまま開いて書き、閉じても `-wal`・`-shm` を消しに行かない（数秒待たない）。
    /// 開いている間、3 つの名前は変更・削除できない。`-journal` は作らない。
    #[test]
    fn pinned_names_resist_rename_and_delete_and_close_is_quick() {
        let dir = tempfile::tempdir().unwrap();
        let root = open_root(&guarded(dir.path().join("root"))).unwrap();
        let pinned = root.as_dir().pin_sqlite("t.sqlite3").unwrap();
        assert!(pinned.is_held());
        block_on(async {
            let pool = pinned.connect().await.expect("開けない");
            create_and_insert(&pool, 100).await;
            for path in [pinned.path(), pinned.wal_path(), pinned.shm_path()] {
                let moved = path.with_extension("moved");
                assert!(
                    std::fs::rename(path, &moved).is_err(),
                    "{path:?} を改名できた"
                );
                assert!(std::fs::remove_file(path).is_err(), "{path:?} を消せた");
            }
            let t = Instant::now();
            pool.close().await;
            let took = t.elapsed();
            assert!(took < Duration::from_secs(1), "close に {took:?}");
        });
        assert!(pinned.wal_path().exists() && pinned.shm_path().exists());
        assert!(!root
            .as_dir()
            .final_path()
            .join("t.sqlite3-journal")
            .exists());
        // 閉じた後も握っている間は消せない。
        assert!(std::fs::remove_file(pinned.wal_path()).is_err());
    }

    /// 反証: 同じ握り方で PERSIST_WAL を立てないと、`close` が共有違反を待って数秒かかる。
    #[test]
    fn without_persist_wal_close_waits_for_the_pinned_companions() {
        let dir = tempfile::tempdir().unwrap();
        let root = open_root(&guarded(dir.path().join("root"))).unwrap();
        let pinned = root.as_dir().pin_sqlite("t.sqlite3").unwrap();
        block_on(async {
            let pool = SqlitePoolOptions::new()
                .connect_with(pinned.connect_options())
                .await
                .unwrap();
            create_and_insert(&pool, 10).await;
            let t = Instant::now();
            pool.close().await;
            let took = t.elapsed();
            assert!(took >= Duration::from_secs(2), "close が {took:?} で返った");
        });
    }

    /// `journal_size_limit` が接続ごとに効き、大きな書き込みの後に閉じると `-wal` が切り詰められる。
    /// 反証: `connect_options` から pragma を外すと、`PRAGMA journal_size_limit` の値の確かめで
    /// 落ちる（外すと閉じた後も `-wal` が切り詰められずに残る。以前の実測で約 420KB）。
    #[test]
    fn a_big_write_leaves_no_big_wal_after_close() {
        let dir = tempfile::tempdir().unwrap();
        let root = open_root(&guarded(dir.path().join("root"))).unwrap();
        let pinned = root.as_dir().pin_sqlite("t.sqlite3").unwrap();
        block_on(async {
            let pool = pinned.connect().await.unwrap();
            let limit: i64 = sqlx::query("PRAGMA journal_size_limit")
                .fetch_one(&pool)
                .await
                .unwrap()
                .get(0);
            assert_eq!(limit, JOURNAL_SIZE_LIMIT);
            sqlx::query("CREATE TABLE b (v BLOB)")
                .execute(&pool)
                .await
                .unwrap();
            let mut tx = pool.begin().await.unwrap();
            for _ in 0..3000 {
                sqlx::query("INSERT INTO b VALUES (?)")
                    .bind(vec![7u8; 4096])
                    .execute(&mut *tx)
                    .await
                    .unwrap();
            }
            tx.commit().await.unwrap();
            let before = std::fs::metadata(pinned.wal_path()).unwrap().len();
            eprintln!("wal before close: {before}");
            assert!(before > 4 * 1024 * 1024, "-wal が膨らんでいない: {before}");
            pool.close().await;
        });
        let after = std::fs::metadata(pinned.wal_path()).unwrap().len();
        eprintln!("wal after close: {after}");
        assert_eq!(after, 0, "閉じた後の -wal が切り詰められていない");
        block_on(async {
            let pool = pinned.connect().await.unwrap();
            let n: i64 = sqlx::query("SELECT count(*) FROM b")
                .fetch_one(&pool)
                .await
                .unwrap()
                .get(0);
            assert_eq!(n, 3000);
            pool.close().await;
        });
    }

    /// 開いている（書き込み共有の）ファイルを読んで写す（`CopyFileW` は共有の都合で開けない
    /// ことがある）。
    fn copy_open(from: &std::path::Path, to: &std::path::Path) {
        let mut src = std::fs::File::open(from).unwrap();
        let mut dst = std::fs::File::create(to).unwrap();
        std::io::copy(&mut src, &mut dst).unwrap();
    }

    /// 前回のプロセスが落ちて `-wal` に行が残っていても、握って開けば行は残っている
    /// （握るときに中身を切り詰めない）。落ちた形は、チェックポイント前の本体と `-wal` を
    /// 別のフォルダへ写して作る（`-shm` は写さない）。
    #[test]
    fn a_leftover_wal_survives_pinning() {
        let dir = tempfile::tempdir().unwrap();
        let first = open_root(&guarded(dir.path().join("first"))).unwrap();
        let pinned = first.as_dir().pin_sqlite("t.sqlite3").unwrap();
        let second = open_root(&guarded(dir.path().join("second"))).unwrap();
        let d = second.as_dir();
        block_on(async {
            let pool = pinned.connect().await.unwrap();
            sqlx::query("PRAGMA wal_autocheckpoint = 0")
                .execute(&pool)
                .await
                .unwrap();
            create_and_insert(&pool, 50).await;
            assert!(std::fs::metadata(pinned.wal_path()).unwrap().len() > 0);
            copy_open(pinned.path(), &d.final_path().join("t.sqlite3"));
            copy_open(pinned.wal_path(), &d.final_path().join("t.sqlite3-wal"));
            pool.close().await;
        });
        let again = d.pin_sqlite("t.sqlite3").unwrap();
        assert!(std::fs::metadata(again.wal_path()).unwrap().len() > 0);
        block_on(async {
            let pool = again.connect().await.unwrap();
            assert_eq!(count(&pool).await, 50);
            pool.close().await;
        });
    }

    /// `-journal` は握らないが、以前の `reopen_path` と同じく、在るならリンク・フォルダを断る。
    #[test]
    fn a_journal_that_is_a_dir_or_a_hard_link_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let root = open_root(&guarded(dir.path().join("root"))).unwrap();
        let d = root.as_dir();
        let journal = d.final_path().join("t.sqlite3-journal");
        std::fs::create_dir(&journal).unwrap();
        assert!(matches!(
            d.pin_sqlite("t.sqlite3"),
            Err(SafeRootError::Refused {
                refusal: Refusal::NotAFile,
                ..
            })
        ));
        std::fs::remove_dir(&journal).unwrap();
        let outside = dir.path().join("outside");
        std::fs::write(&outside, b"x").unwrap();
        std::fs::hard_link(&outside, &journal).unwrap();
        assert!(matches!(
            d.pin_sqlite("t.sqlite3"),
            Err(SafeRootError::Refused {
                refusal: Refusal::HardLinked { .. },
                ..
            })
        ));
        std::fs::remove_file(&journal).unwrap();
        // 普通のファイル（古い hot journal）は通す（以前と同じ）。
        std::fs::write(&journal, b"").unwrap();
        d.pin_sqlite("t.sqlite3").unwrap();
    }
}
