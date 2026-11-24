//! 保存先の世代（`crate::store_generation`、docs/banto-v8-upgrade-plan.md §5 の 1）の筋書きの試験。
//!
//! DB を作り直して同じ id が別のタグに付いても、前の DB の時系列ファイルの値が履歴・日報・
//! 期間 CSV（`/api/collect/history`・`daily-report`・`export` と同じ `CollectorService` の読み出し）
//! に混ざらないこと、前の DB の保存先・v8 より前の保存先では起動が止まりファイルが変わらない
//! ことを、エンジンの起動（デスクトップの設定）で確かめる。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use banto_tstore::{ManualClock, StoreConfig, TsWriter};

use crate::collect::history::tests::{connection, group, group_config, tag, BASE_MS, OFFSET_MS};
use crate::collect::{
    validate_daily_report_request, validate_export_request, validate_history_request,
    CollectorService, Readout,
};
use crate::engine::{EngineConfig, EngineListen, EngineRuntime, EngineStartError, RunningEngine};
use crate::hub::UnavailableKeyStore;
use crate::instance_lock::ChronoGazerHost;
use crate::store_generation::{ClaimError, StoreRefusal, MARKER_FILE_NAME};
use crate::test_support::TempDir;

/// 前の DB が記録した値（新しい DB の読み出しに出てはいけない）。
const OLD_VALUE: f64 = 111.0;

fn config(dir: &Path) -> EngineConfig {
    EngineConfig {
        drivers: crate::test_support::drivers(),
        data_base: dir.to_path_buf(),
        service_data_root: None,
        db_pin: crate::engine::DbPin::Off,
        db_path: dir.join("chronogazer.sqlite3"),
        key_store: Arc::new(UnavailableKeyStore),
        hub_tls: crate::hub_tls::HubTlsPolicy::production(),
        attach_hub: false,
        host: ChronoGazerHost::Desktop,
        listen: EngineListen::Disabled,
        seed_viewer_public: false,
        log_label: "store-generation-test",
        audit_origin: "tauri",
        store_config_required: true,
        store_dir_check_required: true,
    }
}

async fn stop(engine: RunningEngine) {
    let report = engine.shutdown(Duration::from_secs(10)).await;
    assert!(!report.timed_out);
    drop(engine);
}

/// 接続・グループ・タグを 1 つずつ作り、(グループ, タグ) の id を返す。
async fn register(engine: &RunningEngine) -> (i64, i64) {
    let pool = &engine.services().pool;
    let conn = connection(pool, "plc", false).await;
    let grp = group(pool, "g", conn, 1000).await;
    (grp, tag(pool, "t", grp).await)
}

/// `store` に `grp:<grp>` の `tag:<tag>` = [`OLD_VALUE`] を 10 秒ぶん書く（前の DB の記録）。
async fn write_old_history(store: &Path, grp: i64, tag_id: i64) {
    let clock = Arc::new(ManualClock::new(BASE_MS, OFFSET_MS));
    let writer = TsWriter::open(
        store,
        StoreConfig {
            groups: vec![group_config(grp, 1000, &[tag_id])],
        },
        clock,
    )
    .await
    .expect("writer を開ける");
    for k in 0..10_i64 {
        writer
            .append(
                &format!("grp:{grp}"),
                BASE_MS + k * 1000,
                &[Some(OLD_VALUE)],
            )
            .await
            .expect("書ける");
    }
    writer.close().await.expect("閉じる");
}

/// 履歴・日報・期間 CSV で `tag_id` の値を全部集める。
async fn values_read(svc: &CollectorService, tag_id: i64) -> Vec<f64> {
    let mut values = Vec::new();
    let history = validate_history_request(&[tag_id], BASE_MS, BASE_MS + 59_999, 60).unwrap();
    let Readout::Ready { data } = svc.history(&history).await else {
        panic!("履歴を読める");
    };
    for series in data.series {
        values.extend(series.points.iter().filter_map(|point| point.max));
    }
    let hour = crate::collect::DAILY_REPORT_BIN_MS;
    let from = BASE_MS - BASE_MS.rem_euclid(hour);
    let daily = validate_daily_report_request(&[tag_id], from, from + 24 * hour).unwrap();
    let Readout::Ready { data } = svc.daily_report(&daily).await.unwrap() else {
        panic!("日報を読める");
    };
    for hour in data.hours {
        values.extend(hour.values.iter().flatten().map(|stat| stat.max));
    }
    let export =
        validate_export_request(&[tag_id], BASE_MS, BASE_MS + 59_999, "raw", None, None).unwrap();
    let Readout::Ready { data } = svc.export(&export).await.unwrap() else {
        panic!("CSV を読める");
    };
    for row in data.rows {
        values.extend(row.values.iter().flatten());
    }
    values
}

/// フォルダの中身（相対パス → 中身）。変わっていないことを比べる。
fn snapshot(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.strip_prefix(root).unwrap().to_path_buf();
            if path.is_dir() {
                out.insert(name, Vec::new());
                walk(root, &path, out);
            } else {
                out.insert(name, std::fs::read(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

/// DB を作り直す（本体と `-wal`・`-shm` を別の名前に退ける）。
fn set_db_aside(dir: &Path) {
    for suffix in ["", "-wal", "-shm"] {
        let from = dir.join(format!("chronogazer.sqlite3{suffix}"));
        if from.exists() {
            std::fs::rename(&from, dir.join(format!("old-db{suffix}"))).unwrap();
        }
    }
}

fn refusal_of(err: &EngineStartError) -> &StoreRefusal {
    match err {
        EngineStartError::StoreGeneration(ClaimError::Refused { refusal, .. }) => refusal,
        other => panic!("保存先の世代で断られるはず: {other:?}"),
    }
}

/// id を使い回す筋書き: 前の DB の保存先（既定の `data/history`）では、作り直した DB の起動は
/// 保存先に書かずに止まる。新しい保存先へ移ると、同じ id の新しいタグの履歴・日報・CSV に
/// 前の値は出ない（前の保存先をそのまま読ませると出る = この試験は混ざりを捉える）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_recreated_db_does_not_mix_the_previous_history_into_reused_ids() {
    let dir = TempDir::new();
    let store = dir.path().join("data").join("history");

    // 前の DB: 既定の保存先を使い始める（世代の印が付く）。
    let engine = EngineRuntime::start(config(dir.path()))
        .await
        .expect("start");
    assert_eq!(engine.services().collect.data_dir(), store);
    let (old_grp, old_tag) = register(&engine).await;
    stop(engine).await;
    write_old_history(&store, old_grp, old_tag).await;
    assert!(store.join(MARKER_FILE_NAME).is_file());

    // 作り直した DB: 前の保存先は別の世代なので、書かずに止まる。
    set_db_aside(dir.path());
    let before = snapshot(&store);
    let err = match EngineRuntime::start(config(dir.path())).await {
        Ok(_) => panic!("前の DB の保存先で起動した"),
        Err(err) => err,
    };
    assert!(
        matches!(refusal_of(&err), StoreRefusal::OtherGeneration { .. }),
        "{err:?}"
    );
    assert!(err.to_string().contains("別の設定 DB"), "{err}");
    assert_eq!(snapshot(&store), before, "前の保存先が変わった");

    // 新しい保存先へ移して起動し、同じ id でタグを作り直す。
    let fresh = dir.path().join("fresh-store");
    let opened = EngineRuntime::open(config(dir.path())).await.expect("open");
    crate::settings::set_data_dir(&opened.settings(), &fresh.display().to_string())
        .await
        .unwrap();
    opened.close().await;
    let engine = EngineRuntime::start(config(dir.path()))
        .await
        .expect("新しい保存先で起動できる");
    let (grp, tag_id) = register(&engine).await;
    assert_eq!((grp, tag_id), (old_grp, old_tag), "id が使い回される筋書き");
    assert_eq!(
        values_read(&engine.services().collect, tag_id).await,
        Vec::<f64>::new(),
        "前の DB の値が新しいタグに混ざった"
    );
    assert_eq!(snapshot(&store), before, "前の保存先が変わった");

    // 反証: 前の保存先をそのまま読ませると、同じ id の新しいタグに前の値が出る（読み手が
    // 前の保存先のファイルを開くので、ここから先は前の保存先の byte を比べない）。
    let leaky = CollectorService::new(
        engine.services().pool.clone(),
        store.clone(),
        crate::test_support::drivers(),
    );
    assert!(values_read(&leaky, tag_id).await.contains(&OLD_VALUE));
    stop(engine).await;
}

/// v8 より前の版の保存先（印の無い日付ファイルのあるフォルダ。前の既定 `./data` の直下）を
/// `data.dir` に指定すると、書かずに止まる。既定のまま（`./data/history`）なら、その旧い
/// ファイルに触れずに新しい場所で動く。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pre_v8_store_is_refused_and_the_new_default_leaves_it_alone() {
    let dir = TempDir::new();
    let old_store = dir.path().join("data");
    write_old_history(&old_store, 1, 1).await;
    let before = snapshot(&old_store);

    let opened = EngineRuntime::open(config(dir.path())).await.expect("open");
    crate::settings::set_data_dir(&opened.settings(), "./data")
        .await
        .unwrap();
    opened.close().await;
    let err = match EngineRuntime::start(config(dir.path())).await {
        Ok(_) => panic!("v8 より前の保存先で起動した"),
        Err(err) => err,
    };
    assert_eq!(refusal_of(&err), &StoreRefusal::Unmarked, "{err:?}");
    assert_eq!(
        crate::startup_recovery::classify(&err),
        crate::startup_recovery::FailureKind::StoreDirUnwritable
    );
    assert_eq!(snapshot(&old_store), before, "旧い保存先が変わった");

    // 既定に戻す（起動時の対処の窓の「既定の場所に戻す」と同じ口）と、新しい既定で動く。
    let opened = EngineRuntime::open(config(dir.path())).await.expect("open");
    crate::startup_recovery::apply_store_dir(
        &opened.settings(),
        dir.path(),
        &crate::startup_recovery::StoreDirChoice::Default,
    )
    .await
    .expect("既定に戻せる");
    opened.close().await;
    let engine = EngineRuntime::start(config(dir.path()))
        .await
        .expect("新しい既定の保存先で起動できる");
    let (_, tag_id) = register(&engine).await;
    assert_eq!(tag_id, 1, "旧い保存先と同じ id");
    assert_eq!(
        values_read(&engine.services().collect, tag_id).await,
        Vec::<f64>::new()
    );
    stop(engine).await;
    // 旧い日付ファイルはそのまま（新しい保存先 `history` が足されただけ）。
    let mut after = snapshot(&old_store);
    after.retain(|path, _| !path.starts_with("history"));
    assert_eq!(after, before, "旧い保存先のファイルが変わった");
}

/// 起動時の対処の窓で、別の世代の保存先を選ぶと、書き込みの確かめ（一時ファイル）より前に
/// 断られ、設定もフォルダも変わらない。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn choosing_another_generations_store_in_the_recovery_window_writes_nothing() {
    let dir = TempDir::new();
    let other = dir.path().join("other-store");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(other.join(MARKER_FILE_NAME), b"someone-else\n").unwrap();
    std::fs::write(other.join("20260101-001.sqlite3"), b"rows").unwrap();
    let before = snapshot(&other);

    let opened = EngineRuntime::open(config(dir.path())).await.expect("open");
    let err = crate::startup_recovery::apply_store_dir(
        &opened.settings(),
        dir.path(),
        &crate::startup_recovery::StoreDirChoice::Folder(other.clone()),
    )
    .await
    .expect_err("別の世代の保存先は選べない");
    assert!(err.0.contains("別の設定 DB"), "{}", err.0);
    assert_eq!(
        crate::settings::read_data_dir(&opened.settings())
            .await
            .unwrap(),
        None,
        "設定は変えない"
    );
    opened.close().await;
    assert_eq!(snapshot(&other), before);
}
