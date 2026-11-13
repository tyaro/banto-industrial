//! 握ったフォルダの置き場（[`DataPlace::Managed`]）の書き手の確かめ（#623 PR4）。Plain と
//! （Windows では）Guarded の両方で回す。
//!
//! `TempDir` の後片付けは SQLite のハンドルが残ると失敗しうるが、`tempfile` は失敗を無視する
//! （読み手の pool は close せずに drop するので、閉じ終えは後になる。`test_support` の doc）。
//! そのため `writer.rs` と同じく 2 本の worker の multi-thread のランタイムで走らせる。

use std::path::Path;
use std::sync::Arc;

use banto_safe_root::root::ManagedDir;
use banto_safe_root::{Mode, SafeRoot};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use super::*;
use crate::clock::ManualClock;
use crate::config::{GroupConfig, StoreConfig, TagColumn};
use crate::reader::TsReader;
use crate::writer::{TsWriter, WriterOptions};

const OFFSET_MS: i64 = 9 * 3_600_000;
// 2026-07-12T00:00:00Z == 2026-07-12T09:00 local.
const DAY1_START_MS: i64 = 20_646 * 86_400_000;
const DAY_MS: i64 = 86_400_000;

fn config() -> StoreConfig {
    StoreConfig {
        groups: vec![GroupConfig {
            key: "g1".to_string(),
            name: "Group 1".to_string(),
            period_ms: 1_000,
            tags: vec![TagColumn {
                key: "t1".to_string(),
                name: "Tag t1".to_string(),
                data_type: "f32".to_string(),
                unit: None,
                decimals: 1,
            }],
        }],
    }
}

fn modes() -> Vec<Mode> {
    let mut modes = vec![Mode::Plain];
    if cfg!(windows) {
        modes.push(Mode::Guarded);
    }
    modes
}

fn root_of(dir: &Path, mode: Mode) -> SafeRoot {
    let owners = match mode {
        #[cfg(windows)]
        Mode::Guarded => vec![banto_safe_root::acl::current_user_sid().expect("今の利用者の SID")],
        _ => Vec::new(),
    };
    banto_safe_root::open_root(&banto_safe_root::RootSpec {
        expected: dir.to_path_buf(),
        known: banto_safe_root::guard::KnownFolders::default(),
        owners,
        create_access: None,
        create: false,
        mode,
    })
    .expect("open the root")
}

/// `root/data` を作って、その `data` を握る（ChronoGazer のサービスの data.dir と同じく、
/// ルートの下の子を握る）。
struct Held {
    // 握った子は、ルートより先に閉じる（宣言の順に drop）。
    data: Arc<ManagedDir>,
    _root: SafeRoot,
    data_path: std::path::PathBuf,
    outside: std::path::PathBuf,
    _temp: tempfile::TempDir,
}

fn held(mode: Mode) -> Held {
    held_with(mode, None)
}

/// `create_access` が `Some` なら、`data` をその形（保護された DACL）で作る。`None` は先に
/// 普通に作ってから開く（親から継承）。
fn held_with(mode: Mode, create_access: Option<&banto_safe_root::DirAccess>) -> Held {
    let temp = tempfile::tempdir().unwrap();
    let root_path = temp.path().join("root");
    let outside = temp.path().join("outside");
    std::fs::create_dir_all(&root_path).unwrap();
    if create_access.is_none() {
        std::fs::create_dir(root_path.join("data")).unwrap();
    }
    std::fs::create_dir(&outside).unwrap();
    let root = root_of(&root_path, mode);
    let mut set = root
        .open_managed(
            &[banto_safe_root::ManagedSpec {
                key: "data",
                parent: None,
                name: "data",
                create: create_access.is_some(),
            }],
            create_access,
        )
        .unwrap();
    let data = Arc::new(set.take("data").unwrap());
    Held {
        data_path: data.final_path().to_path_buf(),
        data,
        _root: root,
        outside,
        _temp: temp,
    }
}

impl Held {
    fn place(&self) -> DataPlace {
        DataPlace::Managed {
            dir: self.data.clone(),
            expected: None,
        }
    }

    fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&self.data_path)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }
}

async fn open(place: DataPlace, clock: Arc<ManualClock>) -> TsWriter {
    TsWriter::open_in(place, config(), clock, WriterOptions::default())
        .await
        .expect("open the writer")
}

fn clock_at(now_ms: i64) -> Arc<ManualClock> {
    Arc::new(ManualClock::new(now_ms, OFFSET_MS))
}

fn day1_name() -> String {
    crate::schema::data_file_name(LocalDate::from_epoch_ms(DAY1_START_MS, OFFSET_MS), 1)
}

fn day2_name() -> String {
    crate::schema::data_file_name(
        LocalDate::from_epoch_ms(DAY1_START_MS + DAY_MS, OFFSET_MS),
        1,
    )
}

/// banto-tsquery の `plan::open_readonly` と同じ開き方（読み取り専用・作らない）。
async fn tsquery_style_count(path: &Path) -> i64 {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(false)
        .read_only(true);
    let pool = SqlitePoolOptions::new()
        .connect_with(options)
        .await
        .expect("open read-only like banto-tsquery");
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM samples_1")
        .fetch_one(&pool)
        .await
        .unwrap();
    pool.close().await;
    count
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_held_writer_round_trips_and_readers_can_read_the_open_file() {
    for mode in modes() {
        let held = held(mode);
        let place = held.place();
        let writer = open(place.clone(), clock_at(DAY1_START_MS)).await;
        for i in 0..3 {
            writer
                .append("g1", DAY1_START_MS + i * 1_000, &[Some(i as f64)])
                .await
                .unwrap();
        }
        writer.flush().await.unwrap();

        let files = place.list_data_files().unwrap();
        assert_eq!(files.len(), 1, "{mode:?}");
        assert_eq!(
            files,
            crate::files::list_data_files(&held.data_path).unwrap(),
            "{mode:?}: the held listing matches the path-based one"
        );
        // 握っている当日のファイルを、書き手が開いたまま読める（TsReader・tsquery の開き方）。
        let reader = TsReader::open(&files[0].path).await.unwrap();
        let rows = reader
            .read_range("g1", DAY1_START_MS, DAY1_START_MS + 10_000)
            .await
            .unwrap();
        assert_eq!(rows.len(), 3, "{mode:?}");
        assert_eq!(rows[2].values, vec![Some(2.0)]);
        drop(reader);
        assert_eq!(tsquery_style_count(&files[0].path).await, 3, "{mode:?}");

        writer.close().await.unwrap();
        // 閉じた後も読める（Guarded は PERSIST_WAL で `-wal`・`-shm` が残る）。
        let reader = TsReader::open(&files[0].path).await.unwrap();
        assert_eq!(
            reader
                .read_range("g1", DAY1_START_MS, DAY1_START_MS + 10_000)
                .await
                .unwrap()
                .len(),
            3,
            "{mode:?}"
        );
        drop(reader);

        // 同じ設定で開き直すと同じファイルに足す（Path 版と同じ）。
        let writer = open(place.clone(), clock_at(DAY1_START_MS + 5_000)).await;
        writer
            .append("g1", DAY1_START_MS + 5_000, &[Some(5.0)])
            .await
            .unwrap();
        writer.close().await.unwrap();
        assert_eq!(place.list_data_files().unwrap().len(), 1, "{mode:?}");
        assert_eq!(tsquery_style_count(&files[0].path).await, 4, "{mode:?}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_config_change_rotates_to_the_next_sequence_in_a_held_dir() {
    for mode in modes() {
        let held = held(mode);
        // banto-collect の `apply_config` と同じく、前の書き手を開いたまま（同じ日付ファイルを
        // 握ったまま）次の書き手を開く。
        let first = open(held.place(), clock_at(DAY1_START_MS)).await;
        let mut other = config();
        other.groups[0].tags[0].decimals = 3;
        let writer = TsWriter::open_in(
            held.place(),
            other,
            clock_at(DAY1_START_MS),
            WriterOptions::default(),
        )
        .await
        .unwrap();
        let seqs: Vec<u32> = held
            .place()
            .list_data_files()
            .unwrap()
            .iter()
            .map(|f| f.seq)
            .collect();
        assert_eq!(seqs, vec![1, 2], "{mode:?}");
        writer.close().await.unwrap();
        first.close().await.unwrap();
    }
}

/// 日付の切り替えで前の日付の握りが離れ、今の日付は握ったまま。`close` で今の日付も離れる。
#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rotation_and_close_release_the_previous_pins() {
    let held = held(Mode::Guarded);
    let clock = clock_at(DAY1_START_MS);
    let writer = open(held.place(), clock.clone()).await;
    writer
        .append("g1", DAY1_START_MS, &[Some(1.0)])
        .await
        .unwrap();
    writer.flush().await.unwrap();

    let dir = &held.data_path;
    let day1 = day1_name();
    assert!(
        std::fs::rename(dir.join(&day1), dir.join("moved-day1")).is_err(),
        "the open day's file is held"
    );
    assert!(std::fs::rename(dir.join(format!("{day1}-wal")), dir.join("moved-wal")).is_err());

    clock.advance_ms(DAY_MS);
    writer
        .append("g1", DAY1_START_MS + DAY_MS, &[Some(2.0)])
        .await
        .unwrap();
    writer.flush().await.unwrap();

    // 前の日付: 握りも SQLite の接続も離れている。
    for suffix in ["", "-wal", "-shm"] {
        let from = dir.join(format!("{day1}{suffix}"));
        let to = dir.join(format!("moved-day1{suffix}"));
        std::fs::rename(&from, &to).unwrap_or_else(|e| panic!("{from:?}: {e}"));
        std::fs::rename(&to, &from).unwrap();
    }
    // 今の日付は握ったまま。
    let day2 = day2_name();
    for suffix in ["", "-wal", "-shm"] {
        assert!(
            std::fs::rename(
                dir.join(format!("{day2}{suffix}")),
                dir.join(format!("moved-day2{suffix}"))
            )
            .is_err(),
            "{day2}{suffix} is held"
        );
    }

    writer.close().await.unwrap();
    for suffix in ["", "-wal", "-shm"] {
        let from = dir.join(format!("{day2}{suffix}"));
        let to = dir.join(format!("moved-day2{suffix}"));
        std::fs::rename(&from, &to).unwrap_or_else(|e| panic!("{from:?}: {e}"));
        std::fs::rename(&to, &from).unwrap();
    }
    // PERSIST_WAL: 閉じた後も `-wal`（0 バイト）と `-shm` が残る。
    assert_eq!(
        std::fs::metadata(dir.join(format!("{day2}-wal")))
            .unwrap()
            .len(),
        0
    );
}

/// 保持期間の削除は本体と `-wal`・`-shm`・`-journal` を消し、今の日付は付随ごと残す。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prune_removes_the_body_and_its_sidecars_but_keeps_the_open_day() {
    for mode in modes() {
        let held = held(mode);
        let clock = clock_at(DAY1_START_MS);
        let writer = open(held.place(), clock.clone()).await;
        writer
            .append("g1", DAY1_START_MS, &[Some(1.0)])
            .await
            .unwrap();
        clock.advance_ms(DAY_MS);
        writer
            .append("g1", DAY1_START_MS + DAY_MS, &[Some(2.0)])
            .await
            .unwrap();
        writer.flush().await.unwrap();

        let day1 = day1_name();
        let day2 = day2_name();
        // Plain は SQLite が閉じるときに付随を消すので、残っている形を作っておく。
        for suffix in ["-wal", "-shm", "-journal"] {
            let path = held.data_path.join(format!("{day1}{suffix}"));
            if !path.exists() {
                std::fs::write(&path, b"").unwrap();
            }
        }
        let today = LocalDate::from_epoch_ms(DAY1_START_MS + DAY_MS, OFFSET_MS);

        let plan = writer.plan_prune(0, today).await.unwrap();
        assert_eq!(plan.deleted.len(), 1, "{mode:?}");
        let report = writer.prune_files(0, today).await.unwrap();
        assert_eq!(report, plan, "{mode:?}");
        assert_eq!(report.deleted, vec![held.data_path.join(&day1)], "{mode:?}");

        let names = held.names();
        assert!(
            !names.iter().any(|n| n.starts_with(&day1)),
            "{mode:?}: {names:?}"
        );
        assert!(names.contains(&day2), "{mode:?}: {names:?}");
        if mode == Mode::Guarded {
            // 今の日付の付随は握ったまま残る。
            for suffix in ["-wal", "-shm"] {
                assert!(names.contains(&format!("{day2}{suffix}")), "{names:?}");
            }
        }

        // 保持期間 0 でも、書き手が今開いている日付（=今日）は消さない。止めた後に、書き手の
        // 無い削除（`DataPlace::prune_files`）でも今日は消さない。
        writer.close().await.unwrap();
        let report = held.place().prune_files(0, today, &[]).unwrap();
        assert!(report.deleted.is_empty(), "{mode:?}");
        assert!(held.names().contains(&day2));
    }
}

/// 付随が消せなければ本体を残して `Err`（次の削除でやり直せる）。
#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_sidecar_that_cannot_be_removed_keeps_the_body() {
    let held = held(Mode::Guarded);
    let day1 = day1_name();
    let writer = open(held.place(), clock_at(DAY1_START_MS)).await;
    writer.close().await.unwrap();
    let today = LocalDate::from_epoch_ms(DAY1_START_MS + DAY_MS, OFFSET_MS);
    // `-shm` を他者が削除なしの共有で開いたまま。
    let blocker = {
        use std::os::windows::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            .share_mode(1 | 2) // FILE_SHARE_READ | FILE_SHARE_WRITE
            .open(held.data_path.join(format!("{day1}-shm")))
            .unwrap()
    };
    let err = held.place().prune_files(0, today, &[]).unwrap_err();
    assert!(matches!(err, TstoreError::Storage(_)), "{err:?}");
    let names = held.names();
    assert!(names.contains(&day1), "{names:?}");
    assert!(names.contains(&format!("{day1}-shm")), "{names:?}");
    drop(blocker);
    let report = held.place().prune_files(0, today, &[]).unwrap();
    assert_eq!(report.deleted.len(), 1);
    assert!(!held.names().iter().any(|n| n.starts_with(&day1)));
}

/// 日付ファイルを外のファイルへのハードリンクに置き換えてあれば、開かずに断る（外は変わらない）。
#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_date_file_replaced_by_a_link_is_not_opened() {
    let held = held(Mode::Guarded);
    let outside = held.outside.join("victim.sqlite3");
    std::fs::write(&outside, b"outside").unwrap();
    std::fs::hard_link(&outside, held.data_path.join(day1_name())).unwrap();

    let err = TsWriter::open_in(
        held.place(),
        config(),
        clock_at(DAY1_START_MS),
        WriterOptions::default(),
    )
    .await
    .expect_err("a hard link must be refused");
    assert!(matches!(err, TstoreError::Storage(_)), "{err:?}");
    assert_eq!(std::fs::read(&outside).unwrap(), b"outside");
    let names = held.names();
    assert!(
        !names
            .iter()
            .any(|n| n.ends_with("-wal") || n.ends_with("-shm")),
        "nothing is opened next to the link: {names:?}"
    );

    // シンボリックリンク（作れる権利があるときだけ）: 一覧で reparse point は選ばないので、
    // 同じ日付の 1 番として新しく作ろうとし、リンクの名前は握れずに断る。
    std::fs::remove_file(held.data_path.join(day1_name())).unwrap();
    if std::os::windows::fs::symlink_file(&outside, held.data_path.join(day1_name())).is_ok() {
        let err = TsWriter::open_in(
            held.place(),
            config(),
            clock_at(DAY1_START_MS),
            WriterOptions::default(),
        )
        .await
        .expect_err("a symlink must be refused");
        assert!(matches!(err, TstoreError::Storage(_)), "{err:?}");
        assert_eq!(std::fs::read(&outside).unwrap(), b"outside");
    }
}

/// `expected` を渡すと `pin_sqlite_checked` で握る: 期待の形のフォルダの中で、継承のままの
/// ファイルなら開ける（Plain は確かめない）。形が違うフォルダでは開かずに断る。
///
/// 本番の `expected` は運用モードの形（利用者は「読み取りと実行」）で、テストは昇格せずに
/// 今の利用者で走るので書けない。ここでは同じ確かめを、今の利用者に「変更」の形
/// （`with_install_user`。safe-root の `modify_root` と同じ）で通す。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_checked_place_opens_files_that_inherit_the_folder_acl() {
    for mode in modes() {
        let expected = match mode {
            #[cfg(windows)]
            Mode::Guarded => {
                let sid = banto_safe_root::acl::current_user_sid().unwrap();
                let access = banto_safe_root::DirAccess::with_install_user(sid);
                // 継承のままのフォルダ（違う形）なら断る（フォルダの記述子の確かめ）。
                let held = held(mode);
                let err = TsWriter::open_in(
                    DataPlace::Managed {
                        dir: held.data.clone(),
                        expected: Some(access.clone()),
                    },
                    config(),
                    clock_at(DAY1_START_MS),
                    WriterOptions::default(),
                )
                .await
                .expect_err("the folder is not in the expected shape yet");
                assert!(matches!(err, TstoreError::Storage(_)), "{err:?}");
                assert!(held.names().is_empty(), "{:?}", held.names());
                access
            }
            _ => banto_safe_root::DirAccess::service_only(),
        };
        // 期待の形で作ったフォルダ（Plain は確かめないので形は問わない）。
        let held = held_with(mode, (mode == Mode::Guarded).then_some(&expected));
        let place = DataPlace::Managed {
            dir: held.data.clone(),
            expected: Some(expected),
        };
        let writer = open(place, clock_at(DAY1_START_MS)).await;
        writer
            .append("g1", DAY1_START_MS, &[Some(1.0)])
            .await
            .unwrap();
        writer.close().await.unwrap();
    }
}
