//! #413（2026-09-23 オーナー決定）: 接続単位シミュレーションを開けた。
//! 画面は「シミュレーション中（値は記録されません）」と出すので、**本当に
//! 記録されないこと**をここで固定する。
//!
//! 約束の出どころは `banto-collect`（`crates/banto-collect/src/task.rs` の
//! `if ctx.simulation { return; }` - 現在値としきい値イベントまでは流し、
//! tstore への追記だけをしない）で、banto-hub と共有しているのでこのアプリ
//! では変えない。ここで確かめるのは**このアプリの配線を通しても**その約束が
//! 成り立つこと:
//!
//! * `simulation = true` の接続（ホストは到達しない TEST-NET のまま。実際には
//!   `Collector` がプロセス内シミュレータへ差し替える）で収集を開始すると、
//! * (a) 接続状態の読み出しに `simulation: true` が出て、
//! * (b) 現在値が `good` で**値が変化する**（ランプ波が届いている）が、
//! * (c) **データファイルにそのグループの行が 1 行も無い**。
//!
//! (c) が「まだ flush されていないだけ」で緑にならないよう、**対照として
//! 実機扱い（`simulation = false`）の接続を同じ収集に並べ**、そちらの行が
//! データファイルに入ったのを確かめてから (c) を見る（同じ writer・同じ
//! flush なので、対照が書けていればシミュレーション側も書ける時間は経って
//! いる）。停止（最終 flush）の後にもう一度見る。対照の相手は
//! `banto_collect::simulation` のシミュレータを「そこに居る PLC」として
//! 外に立てたもの（`tests/collect_roundtrip.rs`（#412）と同じ作法）。
//!
//! 待ちはすべて「条件をポーリング + 上限時間」。固定 sleep で緑にしない。

use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use banto_collect::simulation;
use banto_collect::Protocol;
use banto_tags::{
    CollectionGroupInput, CollectionGroupService, PlcConnectionInput, PlcConnectionService,
    TagInput, TagService,
};
use banto_tstore::{list_data_files, TsReader};
use chronogazer_core::collect::{
    resolve_data_dir, CollectorService, CollectorState, ConnectionStatusView, QualityView, Readout,
};
use chronogazer_core::db::init_db;
use sqlx::SqlitePool;

const WAIT_LIMIT: Duration = Duration::from_secs(20);
const POLL_INTERVAL: Duration = Duration::from_millis(50);

async fn wait_until<T, F, Fut>(what: &str, mut cond: F) -> T
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, String>>,
{
    let deadline = Instant::now() + WAIT_LIMIT;
    loop {
        let last = match cond().await {
            Ok(value) => return value,
            Err(last) => last,
        };
        if Instant::now() >= deadline {
            panic!(
                "{what} が {} 秒以内に成り立たなかった（最後の観測: {last}）",
                WAIT_LIMIT.as_secs()
            );
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// 後始末を再試行する一時ディレクトリ（Windows では WAL の SQLite を閉じた
/// 直後もファイルが掴まれていることがある - `src/test_support.rs` と同じ理由）。
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        Self(tempfile::tempdir().expect("tempdir").keep())
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        for attempt in 1..=40 {
            match std::fs::remove_dir_all(&self.0) {
                Ok(()) => return,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => return,
                Err(_) if attempt < 40 => std::thread::sleep(Duration::from_millis(50)),
                Err(err) => eprintln!("TempDir: {:?} を削除できませんでした: {err}", self.0),
            }
        }
    }
}

/// 接続 1 / 収集グループ 1（100ms）/ タグ 1（`40001` = ランプ波の対象）を
/// 入れ、`(接続 id, グループ id, タグ id)` を返す。
async fn register(
    pool: &SqlitePool,
    name: &str,
    host: &str,
    port: u16,
    simulation: bool,
) -> (i64, i64, i64) {
    let conn = PlcConnectionService::new(pool.clone())
        .create(PlcConnectionInput {
            name: name.to_string(),
            protocol: "modbus-tcp".to_string(),
            host: host.to_string(),
            port: i64::from(port),
            unit_id: 1,
            enabled: true,
            simulation,
            word_order: String::new(),
            database: None,
            username: None,
            password: None,
        })
        .await
        .expect("PLC 接続を登録できる");
    let group = CollectionGroupService::new(pool.clone())
        .create(CollectionGroupInput {
            name: format!("{name}-group"),
            plc_connection_id: conn.id,
            period_ms: 100,
            enabled: true,
            default_writable: false,
            query_sql: None,
        })
        .await
        .expect("収集グループを登録できる");
    let tag = TagService::new(pool.clone())
        .create(TagInput {
            name: format!("{name}-ramp"),
            collection_group_id: group.id,
            address: "40001".to_string(),
            data_type: "u16".to_string(),
            string_length: None,
            string_encoding: "utf8".to_string(),
            raw_lo: None,
            raw_hi: None,
            eng_lo: None,
            eng_hi: None,
            unit: None,
            decimals: 0,
            threshold_h: None,
            threshold_hh: None,
            threshold_l: None,
            threshold_ll: None,
            enabled: true,
            writable: false,
            tag_kind: "plc".to_string(),
            expression: None,
            retain: false,
            expected_revision: None,
        })
        .await
        .expect("タグを登録できる");
    (conn.id, group.id, tag.id)
}

/// データディレクトリの全ファイルから、グループ `group_key` の行を
/// `(全行, 値の入った行)` で数える。ファイルがまだ 1 つも無ければ `Err`。
async fn count_rows(data_dir: &Path, group_key: &str) -> Result<(usize, usize), String> {
    let files = list_data_files(data_dir).map_err(|err| format!("列挙に失敗: {err}"))?;
    if files.is_empty() {
        return Err(format!("{} にデータファイルがまだ無い", data_dir.display()));
    }
    let (mut all, mut with_value) = (0, 0);
    for file in &files {
        let reader = TsReader::open(&file.path)
            .await
            .map_err(|err| format!("{} を開けない: {err}", file.path.display()))?;
        if reader.group(group_key).is_none() {
            continue;
        }
        let samples = reader
            .read_range(group_key, i64::MIN, i64::MAX)
            .await
            .map_err(|err| format!("{} を読めない: {err}", file.path.display()))?;
        all += samples.len();
        with_value += samples
            .iter()
            .filter(|sample| sample.values.iter().any(Option::is_some))
            .count();
    }
    Ok((all, with_value))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn simulation_connection_values_are_live_but_never_recorded() {
    let dir = TempDir::new();
    let pool = init_db(&dir.path().join("chronogazer.sqlite3"))
        .await
        .expect("設定 DB を初期化できる");
    let data_dir = resolve_data_dir(dir.path(), "./data");

    // 対照: 外に立てたシミュレータを「そこに居る PLC」として普通に登録する。
    let control_plc = simulation::start(Protocol::ModbusTcp).await;
    let (real_conn, real_group, _) = register(
        &pool,
        "control",
        "127.0.0.1",
        control_plc.addr().port(),
        false,
    )
    .await;
    // 本題: `simulation = true`。ホストは到達しないアドレスのまま（収集層が
    // プロセス内シミュレータへ差し替えるので、ここへは接続しない）。
    let (sim_conn, sim_group, sim_tag) = register(&pool, "sim", "192.0.2.1", 502, true).await;
    let real_conn_key = format!("conn:{real_conn}");
    let sim_conn_key = format!("conn:{sim_conn}");
    let real_group_key = format!("grp:{real_group}");
    let sim_group_key = format!("grp:{sim_group}");
    let sim_tag_key = format!("tag:{sim_tag}");

    let svc = CollectorService::new(pool.clone(), data_dir.clone());
    let outcome = svc.start().await.expect("収集を開始できる");
    assert!(!outcome.pending, "開始が打ち切られた: {:?}", outcome.status);
    assert_eq!(
        svc.state(),
        CollectorState::Running {
            groups: 2,
            tags: 2,
            exclusions: vec![],
        }
    );

    // (a) 両方つながり、シミュレーションの印は本題の接続にだけ付く。
    wait_until("両方の接続が connected になること", || async {
        let Readout::Ready { data } = svc.connections().await else {
            return Err("接続状態が読めない".to_string());
        };
        let sim = data.get(&sim_conn_key);
        let real = data.get(&real_conn_key);
        match (sim, real) {
            (Some(sim), Some(real))
                if sim.status == ConnectionStatusView::Connected
                    && real.status == ConnectionStatusView::Connected =>
            {
                assert!(sim.simulation, "シミュレーション接続に印が無い: {sim:?}");
                assert!(!real.simulation, "実機扱いの接続に印が付いた: {real:?}");
                Ok(())
            }
            other => Err(format!("{other:?}")),
        }
    })
    .await;

    // (b) シミュレーション接続の現在値は good で、値が動く。
    let first = wait_until(
        "シミュレーションの現在値が good で値を持つこと",
        || async {
            let Readout::Ready { data } = svc.values() else {
                return Err(svc.values().as_str().to_string());
            };
            match data.get(&sim_tag_key) {
                Some(sample) if sample.quality == QualityView::Good => sample
                    .value
                    .ok_or_else(|| format!("{sim_tag_key} は good だが値が無い")),
                other => Err(format!("{sim_tag_key} = {other:?}")),
            }
        },
    )
    .await;
    wait_until(
        "シミュレーションの現在値が変化すること",
        || async {
            let Readout::Ready { data } = svc.values() else {
                return Err(svc.values().as_str().to_string());
            };
            match data.get(&sim_tag_key) {
                Some(sample)
                    if sample.quality == QualityView::Good
                        && sample.value.is_some()
                        && sample.value != Some(first) =>
                {
                    Ok(())
                }
                other => Err(format!("最初の値 {first} のまま: {other:?}")),
            }
        },
    )
    .await;

    // 対照: 実機扱いの接続の行はデータファイルに入る（= flush は起きている）。
    wait_until(
        "対照の接続の行がデータファイルに入ること",
        || async {
            match count_rows(&data_dir, &real_group_key).await? {
                (_, with_value) if with_value > 0 => Ok(()),
                (all, _) => Err(format!("{real_group_key}: 全 {all} 行、値の入った行 0")),
            }
        },
    )
    .await;

    // (c) シミュレーション接続のグループは 1 行も無い。
    let (all, with_value) = count_rows(&data_dir, &sim_group_key)
        .await
        .expect("データファイルを読める");
    assert_eq!(
        (all, with_value),
        (0, 0),
        "シミュレーションの値がデータファイルに記録されている（{sim_group_key}）"
    );

    // 停止（最終 flush）の後も同じ。
    let outcome = svc.stop().await.expect("収集を停止できる");
    assert!(!outcome.pending, "停止が打ち切られた");
    assert_eq!(svc.state(), CollectorState::Stopped);
    let (_, real_with_value) = count_rows(&data_dir, &real_group_key)
        .await
        .expect("データファイルを読める");
    assert!(real_with_value > 0, "対照の行が消えている");
    assert_eq!(
        count_rows(&data_dir, &sim_group_key)
            .await
            .expect("データファイルを読める"),
        (0, 0),
        "停止後の flush でシミュレーションの値が記録された（{sim_group_key}）"
    );
    // 止まった後は「走っていない」（シミュレーションの印も一緒に消える）。
    assert_eq!(svc.connections().await, Readout::NotRunning);

    control_plc.stop().await;
    pool.close().await;
}
