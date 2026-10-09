//! R1 完了時のミニソーク（docs/r1-plan.md の「検証方針」: シミュレータ相手に
//! 収集 30 分 + UI 閲覧で行欠落・メモリ増加がないこと。72h ソークは R4）。
//! R1-D の D-4（2026-10-09）で足した。
//!
//! ## 何を回すか
//!
//! * **収集**: `CollectorService::new`（本番と同じクライアント生成・既定の
//!   タイムアウト・既定の書き手の設定）で、`banto_collect::simulation` の
//!   Modbus TCP シミュレータ（`examples/dev_plc.rs` が別プロセスで立てるものと
//!   同じランプ波）を**普通の接続**として収集する。収集グループは 2 つ:
//!   周期 1 秒 × 16 タグ（保持レジスタ 40001-40016）と、周期 200ms × 4 タグ
//!   （入力レジスタ 30001-30004）。
//! * **監視画面の閲覧**: 画面が叩くのと同じ入口を、画面と同じ間隔で呼ぶ。
//!   現在値（`CollectorService::values_response` = REST `/api/collect/values`・
//!   Tauri `collect_values`）を 500ms ごと（監視画面のポーリングの下限 = Q1）、
//!   直近 10 分の履歴（`CollectorService::history` = `/api/collect/history`、
//!   トレンドを開いたときの初期窓）を 8 タグ × 1000 区間で 10 秒ごと。どちらも
//!   応答を JSON にする（REST が返す形を作るところまで含めてメモリを見る）。
//!   HTTP の層（axum）は通さない - 測りたいのは収集・現在値・履歴の読み出しが
//!   長く回ったときの行欠落とメモリで、HTTP の層は banto 本体のもの。
//!
//! ## 何を測るか（合否）
//!
//! * **行欠落**: 停止後にデータファイルを `TsReader` で読み戻し、グループごとに
//!   (1) 行数を経過時間 / 周期と比べ、(2) 隣り合う行の時刻の差が**周期の 2 倍を
//!   超えた**箇所（= 1 行以上抜けた）を数え、(3) 値が `null` のセルを数える。
//!   (2) と (3) が 0 であることを合否にする（recorder-requirements.md §4 の
//!   「欠測ゼロ」）。(1) は報告だけ（開始・停止の端で 1〜2 行ずれる）。
//! * **エラー**: 現在値の読み取りが `ready` でなかった回数・`good` でなかった
//!   サンプル数、履歴の読み取りが `ready` でなかった回数。どれも 0 であること。
//! * **メモリ**: このプロセスの working set（RSS 相当、Windows の
//!   `GetProcessMemoryInfo`）を 30 秒ごとに記録し、最初・最大・最後と、
//!   **走行の 1/4 を過ぎた時点（暖機の後）から最後までの増分**を出す。
//!   **合否には使わない**（`apps/banto-hub/core/tests/soak.rs` と同じ判断 -
//!   短い走行ではアロケータの揺らぎをリークと見分けられない）。結果は
//!   r1-plan.md に記録して人が判断する。Windows 以外では測らない。
//!
//! ## 実行方法
//!
//! 通常の `cargo test` では走らない（`#[ignore]`）。既定は 60 秒で、R1 の
//! ミニソークは 30 分（1800 秒）を指定して明示実行する:
//!
//! ```text
//! SOAK_DURATION_SECS=1800 cargo test -p chronogazer-core --test mini_soak -- --ignored --nocapture
//! ```
//!
//! 結果は標準出力に 1 ブロックで出る（`--nocapture` で見える）。

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use banto_collect::simulation;
use banto_collect::Protocol;
use banto_tags::{
    CollectionGroupInput, CollectionGroupService, PlcConnectionInput, PlcConnectionService,
    TagInput, TagService,
};
use banto_tstore::{list_data_files, TsReader};
use chronogazer_core::collect::{
    resolve_data_dir, validate_history_request, CollectorService, QualityView, Readout,
};
use chronogazer_core::db::init_db;
use sqlx::SqlitePool;

#[cfg(windows)]
mod mem_probe {
    //! このプロセスの working set を読む診断専用の薄いラッパー
    //! （`apps/banto-hub/core/tests/soak.rs` の `mem_probe` と同じ）。失敗したら
    //! `None`（合否に使わない値なので、走行を止める理由にしない）。

    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    pub fn working_set_bytes() -> Option<u64> {
        // SAFETY: `counters` はこの関数のスタック上にあり、`cb` に大きさを
        // 入れて渡す。`GetCurrentProcess` の疑似ハンドルは閉じなくてよい。
        unsafe {
            let mut counters: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
            counters.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
            if GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb) != 0 {
                Some(counters.WorkingSetSize as u64)
            } else {
                None
            }
        }
    }
}

#[cfg(not(windows))]
mod mem_probe {
    //! Windows 以外では測らない（ChronoGazer の配布先は Windows）。
    pub fn working_set_bytes() -> Option<u64> {
        None
    }
}

/// 既定の走行時間（秒）。R1 のミニソークは `SOAK_DURATION_SECS=1800`。
const DEFAULT_DURATION_SECS: u64 = 60;
const VALUES_POLL: Duration = Duration::from_millis(500);
const HISTORY_EVERY: Duration = Duration::from_secs(10);
const MEMORY_EVERY: Duration = Duration::from_secs(30);
const HISTORY_WINDOW_MS: i64 = 600_000;
const HISTORY_BINS: u64 = 1000;

/// 収集グループの定義: (名前, 周期 ms, アドレスの並び)。
fn group_specs() -> Vec<(&'static str, i64, Vec<String>)> {
    vec![
        (
            "ソーク-1秒",
            1000,
            (1..=16).map(|n| format!("{}", 40000 + n)).collect(),
        ),
        (
            "ソーク-200ms",
            200,
            (1..=4).map(|n| format!("{}", 30000 + n)).collect(),
        ),
    ]
}

/// 後始末を再試行する一時ディレクトリ（`tests/collect_roundtrip.rs` と同じ理由:
/// Windows では WAL の SQLite を閉じた直後もファイルが掴まれていることがある）。
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        Self(tempfile::tempdir().expect("tempdir").keep())
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

struct RegisteredGroup {
    key: String,
    name: &'static str,
    period_ms: i64,
    tag_ids: Vec<i64>,
}

async fn register(pool: &SqlitePool, port: u16) -> Vec<RegisteredGroup> {
    let conn = PlcConnectionService::new(pool.clone())
        .create(PlcConnectionInput {
            name: "ソーク用 PLC".to_string(),
            protocol: "modbus-tcp".to_string(),
            host: "127.0.0.1".to_string(),
            port: i64::from(port),
            unit_id: 1,
            enabled: true,
            // データファイルの行を数えるので、記録されないシミュレーション接続
            // ではなく、外に立てたシミュレータを普通の PLC として登録する。
            simulation: false,
            word_order: String::new(),
            database: None,
            username: None,
            password: None,
        })
        .await
        .expect("PLC 接続を登録できる");
    let mut groups = Vec::new();
    for (name, period_ms, addresses) in group_specs() {
        let group = CollectionGroupService::new(pool.clone())
            .create(CollectionGroupInput {
                name: name.to_string(),
                plc_connection_id: conn.id,
                period_ms,
                enabled: true,
                default_writable: false,
                query_sql: None,
            })
            .await
            .expect("収集グループを登録できる");
        let mut tag_ids = Vec::new();
        for address in addresses {
            let tag = TagService::new(pool.clone())
                .create(TagInput {
                    name: format!("{name}-{address}"),
                    collection_group_id: group.id,
                    address,
                    data_type: "u16".to_string(),
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
                })
                .await
                .expect("タグを登録できる");
            tag_ids.push(tag.id);
        }
        groups.push(RegisteredGroup {
            key: format!("grp:{}", group.id),
            name,
            period_ms,
            tag_ids,
        });
    }
    groups
}

fn now_ms() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_millis(),
    )
    .expect("fits i64")
}

/// グループ 1 つ分のデータファイルの読み戻し結果。抜け・`null` は**計測区間**
/// （全タグが good になった後）の行だけで数える。収集の開始直後、接続が
/// できる前の周期は値の無い行として記録される（読めなかった周期を 0 にしない
/// 設計どおり）ので、それは暖機の行として別に数える。
#[derive(Debug, Default)]
struct RowStats {
    /// 計測区間の行数。
    rows: usize,
    /// 計測区間より前（暖機中）の行数と、そのうち値が `null` のセルの数。
    warmup_rows: usize,
    warmup_null_cells: usize,
    /// 隣り合う行の時刻の差が周期の 2 倍を超えた箇所の数。
    gaps: usize,
    /// 抜けた行数の見積もり（差 / 周期 - 1 の合計）。
    missing_estimate: i64,
    /// 一番大きかった行の間隔（ms）。
    max_interval_ms: i64,
    /// 計測区間で値が `null` のセルの数。
    null_cells: usize,
    first_ms: Option<i64>,
    last_ms: Option<i64>,
}

async fn read_rows(data_dir: &Path, group: &RegisteredGroup, measured_from_ms: i64) -> RowStats {
    let mut times = Vec::new();
    let mut null_cells = 0;
    let mut warmup_rows = 0;
    let mut warmup_null_cells = 0;
    for file in list_data_files(data_dir).expect("データファイルを列挙できる") {
        let reader = TsReader::open(&file.path)
            .await
            .expect("データファイルを開ける");
        if reader.group(&group.key).is_none() {
            continue;
        }
        let samples = reader
            .read_range(&group.key, i64::MIN, i64::MAX)
            .await
            .expect("データファイルを読める");
        for sample in samples {
            let nulls = sample.values.iter().filter(|v| v.is_none()).count();
            if sample.ptime_ms < measured_from_ms {
                warmup_rows += 1;
                warmup_null_cells += nulls;
            } else {
                null_cells += nulls;
                times.push(sample.ptime_ms);
            }
        }
    }
    times.sort_unstable();
    let mut stats = RowStats {
        rows: times.len(),
        warmup_rows,
        warmup_null_cells,
        null_cells,
        first_ms: times.first().copied(),
        last_ms: times.last().copied(),
        ..RowStats::default()
    };
    for pair in times.windows(2) {
        let interval = pair[1] - pair[0];
        stats.max_interval_ms = stats.max_interval_ms.max(interval);
        if interval > group.period_ms * 2 {
            stats.gaps += 1;
            stats.missing_estimate += interval / group.period_ms - 1;
        }
    }
    stats
}

fn mib(bytes: Option<u64>) -> String {
    bytes.map_or_else(
        || "-".to_string(),
        |b| format!("{:.1} MiB", b as f64 / (1024.0 * 1024.0)),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "ミニソーク（既定 60 秒、R1 は SOAK_DURATION_SECS=1800）。--ignored で明示実行する"]
async fn mini_soak_collect_with_monitor_polling() {
    let duration = Duration::from_secs(
        std::env::var("SOAK_DURATION_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_DURATION_SECS),
    );
    let dir = TempDir::new();
    let pool = init_db(&dir.0.join("chronogazer.sqlite3"))
        .await
        .expect("設定 DB を初期化できる");
    let data_dir = resolve_data_dir(&dir.0, "./data");
    let sim = simulation::start(Protocol::ModbusTcp).await;
    let groups = register(&pool, sim.addr().port()).await;
    let all_tag_ids: Vec<i64> = groups.iter().flat_map(|g| g.tag_ids.clone()).collect();
    // トレンドの 1 グループの上限（8 ペン）に合わせて、先頭の 8 タグの履歴を読む。
    let history_tag_ids: Vec<i64> = all_tag_ids.iter().copied().take(8).collect();

    let svc = CollectorService::new(pool.clone(), data_dir.clone());
    let outcome = svc.start().await.expect("収集を開始できる");
    assert!(
        !outcome.pending,
        "開始が打ち切られた（{:?}）",
        outcome.status
    );

    // 接続して全タグが good になるまで待つ（ここまでは計測に入れない）。
    let warmup_deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Readout::Ready { data } = svc.values() {
            let good = all_tag_ids
                .iter()
                .filter(|id| {
                    data.get(&format!("tag:{id}"))
                        .is_some_and(|s| s.quality == QualityView::Good)
                })
                .count();
            if good == all_tag_ids.len() {
                break;
            }
        }
        assert!(
            Instant::now() < warmup_deadline,
            "20 秒以内に全タグが good にならなかった"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let started = Instant::now();
    let started_ms = now_ms();
    let mut values_polls = 0u64;
    let mut values_not_ready = 0u64;
    let mut samples_not_good = 0u64;
    let mut history_reads = 0u64;
    let mut history_not_ready = 0u64;
    let mut history_max_ms = 0u128;
    let mut json_bytes = 0usize;
    let mut memory: Vec<(Duration, Option<u64>)> =
        vec![(Duration::ZERO, mem_probe::working_set_bytes())];
    let mut next_history = started;
    let mut next_memory = started + MEMORY_EVERY;

    while started.elapsed() < duration {
        let response = svc.values_response();
        values_polls += 1;
        match &response.readout {
            Readout::Ready { data } => {
                for id in &all_tag_ids {
                    match data.get(&format!("tag:{id}")) {
                        Some(s) if s.quality == QualityView::Good && s.value.is_some() => {}
                        _ => samples_not_good += 1,
                    }
                }
            }
            _ => values_not_ready += 1,
        }
        json_bytes = serde_json::to_vec(&response).expect("JSON にできる").len();

        if Instant::now() >= next_history {
            next_history += HISTORY_EVERY;
            let to = now_ms();
            let request = validate_history_request(
                &history_tag_ids,
                to - HISTORY_WINDOW_MS,
                to,
                HISTORY_BINS,
            )
            .expect("履歴の要求が検証を通る");
            let t = Instant::now();
            let readout = svc.history(&request).await;
            history_max_ms = history_max_ms.max(t.elapsed().as_millis());
            history_reads += 1;
            match &readout {
                Readout::Ready { .. } => {
                    serde_json::to_vec(&readout).expect("JSON にできる");
                }
                _ => history_not_ready += 1,
            }
        }
        if Instant::now() >= next_memory {
            next_memory += MEMORY_EVERY;
            memory.push((started.elapsed(), mem_probe::working_set_bytes()));
        }
        tokio::time::sleep(VALUES_POLL).await;
    }
    let elapsed = started.elapsed();
    memory.push((elapsed, mem_probe::working_set_bytes()));

    let outcome = svc.stop().await.expect("収集を停止できる");
    assert!(!outcome.pending, "停止が打ち切られた");
    let stopped_ms = now_ms();

    // --- 結果 -------------------------------------------------------------
    println!("=== chronogazer ミニソーク ===");
    println!(
        "走行 {:.0} 秒（指定 {} 秒）、収集グループ {} 個・タグ {} 本",
        elapsed.as_secs_f64(),
        duration.as_secs(),
        groups.len(),
        all_tag_ids.len()
    );
    let mut row_failures = Vec::new();
    for group in &groups {
        let stats = read_rows(&data_dir, group, started_ms).await;
        let span_ms = stopped_ms - started_ms;
        println!(
            "[{}] 周期 {}ms: 計測区間の行 {}（目安 {}）、行の抜け {} 箇所（推定 {} 行）、最大間隔 {}ms、null のセル {}（暖機中の行 {}・うち null のセル {}）、最初 {:?} 最後 {:?}",
            group.name,
            group.period_ms,
            stats.rows,
            span_ms / group.period_ms,
            stats.gaps,
            stats.missing_estimate,
            stats.max_interval_ms,
            stats.null_cells,
            stats.warmup_rows,
            stats.warmup_null_cells,
            stats.first_ms,
            stats.last_ms,
        );
        if stats.gaps > 0 || stats.null_cells > 0 || stats.rows == 0 {
            row_failures.push(format!("{}: {stats:?}", group.name));
        }
    }
    println!(
        "現在値の読み取り {values_polls} 回（ready でない {values_not_ready} 回、good でないサンプル {samples_not_good}、応答 {json_bytes} バイト）"
    );
    println!(
        "履歴の読み取り {history_reads} 回（ready でない {history_not_ready} 回、最長 {history_max_ms}ms）"
    );
    let warm_index = memory.len() / 4;
    let first = memory.first().and_then(|m| m.1);
    let warm = memory.get(warm_index).and_then(|m| m.1);
    let last = memory.last().and_then(|m| m.1);
    let max = memory.iter().filter_map(|m| m.1).max();
    println!(
        "working set: 最初 {} / 暖機後（{:.0} 秒）{} / 最大 {} / 最後 {}",
        mib(first),
        memory.get(warm_index).map_or(0.0, |m| m.0.as_secs_f64()),
        mib(warm),
        mib(max),
        mib(last)
    );
    if let (Some(warm), Some(last)) = (warm, last) {
        println!(
            "暖機後から最後までの増分: {:+.1} MiB",
            (last as f64 - warm as f64) / (1024.0 * 1024.0)
        );
    }
    let samples: Vec<String> = memory
        .iter()
        .map(|(t, b)| format!("{:.0}s={}", t.as_secs_f64(), mib(*b)))
        .collect();
    println!("working set の推移: {}", samples.join(", "));

    sim.stop().await;
    pool.close().await;

    assert!(row_failures.is_empty(), "行の欠落: {row_failures:?}");
    assert_eq!(values_not_ready, 0, "現在値が ready でない回があった");
    assert_eq!(samples_not_good, 0, "good でないサンプルがあった");
    assert_eq!(history_not_ready, 0, "履歴が ready でない回があった");
}
