//! 握った置き場（`banto_tstore::DataPlace::Managed`）で読む（#623 PR7、
//! [`TsQuery::with_place`]）: 4 つの問い合わせが、書き手が握って開いている当日のファイルも
//! 含めてパスの版と同じ結果を返し、`-shm` が外のファイルへのハードリンクなら読まずに断る。

mod common;

use std::sync::Arc;

use banto_safe_root::root::ManagedDir;
use banto_safe_root::{Mode, SafeRoot};
use banto_tsquery::{TsQuery, TsQueryError};
use banto_tstore::{DataPlace, TsWriter, WriterOptions};
use common::*;

fn modes() -> Vec<Mode> {
    let mut modes = vec![Mode::Plain];
    if cfg!(windows) {
        modes.push(Mode::Guarded);
    }
    modes
}

/// `root/data` を作って握る（ChronoGazer のサービスの `data.dir` と同じく、ルートの下の子）。
struct Held {
    data: Arc<ManagedDir>,
    _root: SafeRoot,
    data_path: std::path::PathBuf,
    outside: std::path::PathBuf,
    _temp: tempfile::TempDir,
}

fn held(mode: Mode) -> Held {
    let temp = tempfile::tempdir().unwrap();
    let root_path = temp.path().join("root");
    let outside = temp.path().join("outside");
    std::fs::create_dir_all(root_path.join("data")).unwrap();
    std::fs::create_dir(&outside).unwrap();
    let owners = match mode {
        #[cfg(windows)]
        Mode::Guarded => vec![banto_safe_root::acl::current_user_sid().expect("今の利用者の SID")],
        _ => Vec::new(),
    };
    let root = banto_safe_root::open_root(&banto_safe_root::RootSpec {
        expected: root_path,
        known: banto_safe_root::guard::KnownFolders::default(),
        owners,
        create_access: None,
        create: false,
        mode,
    })
    .expect("open the root");
    let mut set = root
        .open_managed(
            &[banto_safe_root::ManagedSpec {
                key: "data",
                parent: None,
                name: "data",
                create: false,
            }],
            None,
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
}

fn config() -> banto_tstore::StoreConfig {
    store_config(vec![group("g1", 1_000, vec![tag("t1", None, 1)])])
}

async fn write_rows(place: DataPlace, n: i64) -> TsWriter {
    let writer = TsWriter::open_in(
        place,
        config(),
        clock_at(DAY1_START_MS),
        WriterOptions::default(),
    )
    .await
    .expect("open the writer");
    for i in 0..n {
        writer
            .append("g1", DAY1_START_MS + i * 1_000, &[Some(i as f64)])
            .await
            .unwrap();
    }
    writer.flush().await.unwrap();
    writer
}

async fn query_all(query: &TsQuery) -> Result<(usize, usize, i64, usize), TsQueryError> {
    let keys = tag_keys(&["t1"]);
    let to = DAY1_START_MS + 60_000;
    let raw = query
        .read_range("g1", &keys, DAY1_START_MS, to, None)
        .await?;
    let decimated = query
        .read_decimated("g1", &keys, DAY1_START_MS, to, 10)
        .await?;
    let aggregate = query.aggregate("g1", &keys, DAY1_START_MS, to).await?;
    let catalog = query.catalog().await?;
    Ok((
        raw.rows.len(),
        decimated.bins.len(),
        aggregate[0].count,
        catalog.groups.len(),
    ))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_managed_place_reads_like_the_path_while_the_writer_holds_today() {
    for mode in modes() {
        let held = held(mode);
        let writer = write_rows(held.place(), 5).await;

        let managed = query_all(&TsQuery::with_place(held.place()))
            .await
            .unwrap_or_else(|e| panic!("{mode:?}: {e}"));
        let by_path = query_all(&TsQuery::new(held.data_path.clone()))
            .await
            .unwrap();
        assert_eq!(managed, by_path, "{mode:?}");
        assert_eq!(managed.0, 5, "{mode:?}");
        assert_eq!(managed.2, 5, "{mode:?}");
        assert_eq!(managed.3, 1, "{mode:?}");

        // 読んだ後も書き手は書ける（読み手の握りは離れている）。
        writer
            .append("g1", DAY1_START_MS + 5_000, &[Some(5.0)])
            .await
            .unwrap();
        writer.close().await.unwrap();
        let after = query_all(&TsQuery::with_place(held.place())).await.unwrap();
        assert_eq!(after.0, 6, "{mode:?}");
        assert_eq!(TsQuery::with_place(held.place()).data_dir(), held.data_path);
    }
}

#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_managed_place_refuses_a_shm_hard_linked_outside() {
    let held = held(Mode::Guarded);
    write_rows(held.place(), 3).await.close().await.unwrap();
    let name = held.place().list_data_files().unwrap()[0]
        .path
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    let shm = held.data_path.join(format!("{name}-shm"));
    let victim = held.outside.join("victim");
    std::fs::write(&victim, b"outside").unwrap();
    std::fs::remove_file(&shm).unwrap();
    std::fs::hard_link(&victim, &shm).unwrap();

    let query = TsQuery::with_place(held.place());
    let keys = tag_keys(&["t1"]);
    let to = DAY1_START_MS + 60_000;
    assert!(query
        .read_range("g1", &keys, DAY1_START_MS, to, None)
        .await
        .is_err());
    assert!(query
        .read_decimated("g1", &keys, DAY1_START_MS, to, 10)
        .await
        .is_err());
    assert!(query
        .aggregate("g1", &keys, DAY1_START_MS, to)
        .await
        .is_err());
    assert!(query.catalog().await.is_err());
    assert_eq!(std::fs::read(&victim).unwrap(), b"outside");
}
