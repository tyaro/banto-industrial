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
//!   **現在値と履歴は別々のタスクで並行に回す**（画面でも別々の要求。PR #546 の
//!   レビュー: 1 本のループで順に呼ぶと、遅い履歴の読み取り（30 分で最長 8.9 秒）の
//!   間は現在値のポーリングも止まり、並行の負荷を試していなかった）。現在値の
//!   ポーリングの最長間隔と、履歴の所要時間の分布（p50 / p95 / 最長）も出す。
//!   HTTP の層（axum）は通さない - 測りたいのは収集・現在値・履歴の読み出しが
//!   長く回ったときの行欠落とメモリで、HTTP の層は banto 本体のもの。
//!
//! ## 何を測るか（合否）
//!
//! * **行欠落**: 停止後にデータファイルを `TsReader` で読み戻し、グループごとに
//!   計測区間（全タグが good になってから停止まで）の行について
//!   (1) 行数が「計測区間 / 周期」の ±[`EDGE_ROWS`] 行（開始・停止の端のずれ）に
//!   収まる、(2) 隣り合う行の間隔が**周期の 2 倍以上**の箇所が無い（抜けた行数は
//!   `round(間隔 / 周期) - 1`。2 倍未満は刻みの揺れ）、(3) 値が `null` のセルが無い、
//!   の 3 つすべてを合否にする（recorder-requirements.md §4 の「欠測ゼロ」）。
//!   判定は純関数 [`judge_rows`] で、通常の `cargo test` で走る単体テスト
//!   （`judge_tests`: 最初の 3 行だけ・1 行おきの抜け・ちょうど 2 周期の抜けが落ち、
//!   端の ±2 行・遅れた行の揺れは通る）で固定する。
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

// ---------------------------------------------------------------------------
// 判定（純関数。下の `judge_tests` で総当たり）
// ---------------------------------------------------------------------------

/// 行数の許容（開始・停止の端）。計測の開始時刻・停止時刻と収集の刻みの位相の
/// ずれで、端の行が 1 つずつ入ったり外れたりする（30 分の実測は目安 ±1 行）。
/// それ以上の差は欠落（または重複）として落とす。
const EDGE_ROWS: i64 = 2;

/// 計測区間 `span_ms` の間に周期 `period_ms` で書かれるはずの行数。
fn expected_rows(span_ms: i64, period_ms: i64) -> i64 {
    span_ms / period_ms
}

/// 隣り合う 2 行の間に抜けた行の数。間隔が**周期の 2 倍以上**なら抜けとみなし、
/// `round(間隔 / 周期) - 1` 行を数える（ちょうど 2 周期 = 1 行の抜け）。2 倍未満は
/// 刻みの揺れ（遅れた行の次の行は詰まる）として 0。
fn missing_between(interval_ms: i64, period_ms: i64) -> i64 {
    if interval_ms < period_ms * 2 {
        return 0;
    }
    ((interval_ms as f64 / period_ms as f64).round() as i64 - 1).max(1)
}

/// グループ 1 つ分の行の時刻（計測区間のもの、昇順）の判定結果。
#[derive(Debug, Default, PartialEq, Eq)]
struct RowJudgement {
    rows: i64,
    expected: i64,
    /// 抜けのあった箇所（間隔が周期の 2 倍以上）の数と、抜けた行の数。
    gaps: usize,
    missing: i64,
    max_interval_ms: i64,
}

impl RowJudgement {
    /// 合格か。抜けが 0 で、行数が目安の ±[`EDGE_ROWS`] に収まる。
    fn passes(&self) -> bool {
        self.gaps == 0 && self.missing == 0 && (self.rows - self.expected).abs() <= EDGE_ROWS
    }
}

fn judge_rows(times_ms: &[i64], period_ms: i64, span_ms: i64) -> RowJudgement {
    let mut judgement = RowJudgement {
        rows: times_ms.len() as i64,
        expected: expected_rows(span_ms, period_ms),
        ..RowJudgement::default()
    };
    for pair in times_ms.windows(2) {
        let interval = pair[1] - pair[0];
        judgement.max_interval_ms = judgement.max_interval_ms.max(interval);
        let missing = missing_between(interval, period_ms);
        if missing > 0 {
            judgement.gaps += 1;
            judgement.missing += missing;
        }
    }
    judgement
}

// ---------------------------------------------------------------------------
// データファイルの読み戻し
// ---------------------------------------------------------------------------

/// グループ 1 つ分のデータファイルの中身。抜け・`null` は**計測区間**（全タグが
/// good になった後）の行だけで数える。収集の開始直後、接続ができる前の周期は
/// 値の無い行として記録される（読めなかった周期を 0 にしない設計どおり）ので、
/// それは暖機の行として別に数える。
#[derive(Debug, Default)]
struct GroupRows {
    /// 計測区間の行の時刻（昇順）。
    times_ms: Vec<i64>,
    /// 計測区間で値が `null` のセルの数。
    null_cells: usize,
    /// 計測区間より前（暖機中）の行数と、そのうち値が `null` のセルの数。
    warmup_rows: usize,
    warmup_null_cells: usize,
}

async fn read_rows(data_dir: &Path, group: &RegisteredGroup, measured_from_ms: i64) -> GroupRows {
    let mut out = GroupRows::default();
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
                out.warmup_rows += 1;
                out.warmup_null_cells += nulls;
            } else {
                out.null_cells += nulls;
                out.times_ms.push(sample.ptime_ms);
            }
        }
    }
    out.times_ms.sort_unstable();
    out
}

fn mib(bytes: Option<u64>) -> String {
    bytes.map_or_else(
        || "-".to_string(),
        |b| format!("{:.1} MiB", b as f64 / (1024.0 * 1024.0)),
    )
}

// ---------------------------------------------------------------------------
// 閲覧の 2 本のタスク（現在値・履歴）は**別々に**回す
// ---------------------------------------------------------------------------

/// 現在値のポーリングの集計。
#[derive(Debug, Default)]
struct ValuesStats {
    polls: u64,
    not_ready: u64,
    samples_not_good: u64,
    json_bytes: usize,
    /// 前のポーリングの開始から次の開始までの最長（ms）。履歴の読み取りに
    /// 引きずられて止まっていないかを見る（周期は [`VALUES_POLL`]）。
    max_poll_interval_ms: u128,
}

/// 現在値を [`VALUES_POLL`] ごとに読む（監視画面のポーリング）。`deadline` まで。
async fn poll_values(svc: CollectorService, tag_ids: Vec<i64>, deadline: Instant) -> ValuesStats {
    let mut stats = ValuesStats::default();
    let mut last_start: Option<Instant> = None;
    while Instant::now() < deadline {
        let start = Instant::now();
        if let Some(prev) = last_start {
            stats.max_poll_interval_ms = stats
                .max_poll_interval_ms
                .max(start.duration_since(prev).as_millis());
        }
        last_start = Some(start);
        let response = svc.values_response();
        stats.polls += 1;
        match &response.readout {
            Readout::Ready { data } => {
                for id in &tag_ids {
                    match data.get(&format!("tag:{id}")) {
                        Some(s) if s.quality == QualityView::Good && s.value.is_some() => {}
                        _ => stats.samples_not_good += 1,
                    }
                }
            }
            _ => stats.not_ready += 1,
        }
        stats.json_bytes = serde_json::to_vec(&response).expect("JSON にできる").len();
        tokio::time::sleep(VALUES_POLL).await;
    }
    stats
}

/// 履歴の読み取りの集計。
#[derive(Debug, Default)]
struct HistoryStats {
    reads: u64,
    not_ready: u64,
    max_ms: u128,
    /// 読み取りにかかった時間（ms）。分布を出すため全部持つ（30 分で 180 個）。
    durations_ms: Vec<u128>,
}

/// 直近の履歴を [`HISTORY_EVERY`] ごとに読む（トレンドを開いたときの初期窓）。
async fn read_history(svc: CollectorService, tag_ids: Vec<i64>, deadline: Instant) -> HistoryStats {
    let mut stats = HistoryStats::default();
    let mut next = Instant::now();
    while Instant::now() < deadline {
        let to = now_ms();
        let request = validate_history_request(&tag_ids, to - HISTORY_WINDOW_MS, to, HISTORY_BINS)
            .expect("履歴の要求が検証を通る");
        let t = Instant::now();
        let readout = svc.history(&request).await;
        if matches!(readout, Readout::Ready { .. }) {
            serde_json::to_vec(&readout).expect("JSON にできる");
        } else {
            stats.not_ready += 1;
        }
        let took = t.elapsed().as_millis();
        stats.max_ms = stats.max_ms.max(took);
        stats.durations_ms.push(took);
        stats.reads += 1;
        next += HISTORY_EVERY;
        tokio::time::sleep_until(next.min(deadline).max(Instant::now()).into()).await;
    }
    stats
}

/// `values` の p 分位（0〜100、最近傍）。空なら 0。
fn percentile(values: &[u128], p: usize) -> u128 {
    if values.is_empty() {
        return 0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let index = (sorted.len() * p).div_ceil(100).saturating_sub(1);
    sorted[index.min(sorted.len() - 1)]
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
    let deadline = started + duration;
    // 現在値と履歴は別々のタスク（画面でも別々の要求）。履歴の読み取りが遅くても
    // 現在値のポーリングは止まらないはず - それも測る（`max_poll_interval_ms`）。
    let values_task = tokio::spawn(poll_values(svc.clone(), all_tag_ids.clone(), deadline));
    let history_task = tokio::spawn(read_history(svc.clone(), history_tag_ids.clone(), deadline));
    let mut memory: Vec<(Duration, Option<u64>)> =
        vec![(Duration::ZERO, mem_probe::working_set_bytes())];
    while started.elapsed() < duration {
        let remaining = duration.saturating_sub(started.elapsed());
        tokio::time::sleep(MEMORY_EVERY.min(remaining)).await;
        memory.push((started.elapsed(), mem_probe::working_set_bytes()));
    }
    let values = values_task.await.expect("現在値のタスク");
    let history = history_task.await.expect("履歴のタスク");
    let elapsed = started.elapsed();

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
        let rows = read_rows(&data_dir, group, started_ms).await;
        let judgement = judge_rows(&rows.times_ms, group.period_ms, stopped_ms - started_ms);
        println!(
            "[{}] 周期 {}ms: 計測区間の行 {}（目安 {}、許容 ±{EDGE_ROWS}）、抜け {} 箇所（{} 行）、最大間隔 {}ms、null のセル {}（暖機中の行 {}・うち null のセル {}）",
            group.name,
            group.period_ms,
            judgement.rows,
            judgement.expected,
            judgement.gaps,
            judgement.missing,
            judgement.max_interval_ms,
            rows.null_cells,
            rows.warmup_rows,
            rows.warmup_null_cells,
        );
        if !judgement.passes() || rows.null_cells > 0 {
            row_failures.push(format!(
                "{}: {judgement:?}, null のセル {}",
                group.name, rows.null_cells
            ));
        }
    }
    println!(
        "現在値の読み取り {} 回（ready でない {} 回、good でないサンプル {}、応答 {} バイト、ポーリングの最長間隔 {}ms）",
        values.polls,
        values.not_ready,
        values.samples_not_good,
        values.json_bytes,
        values.max_poll_interval_ms
    );
    println!(
        "履歴の読み取り {} 回（ready でない {} 回、所要 p50 {}ms / p95 {}ms / 最長 {}ms）",
        history.reads,
        history.not_ready,
        percentile(&history.durations_ms, 50),
        percentile(&history.durations_ms, 95),
        history.max_ms
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
    assert_eq!(values.not_ready, 0, "現在値が ready でない回があった");
    assert_eq!(values.samples_not_good, 0, "good でないサンプルがあった");
    assert!(values.polls > 0, "現在値を一度も読まなかった");
    assert_eq!(history.not_ready, 0, "履歴が ready でない回があった");
    assert!(history.reads > 0, "履歴を一度も読まなかった");
}

/// 判定の純関数の総当たり（通常の `cargo test` で走る）。
mod judge_tests {
    use super::*;

    /// `from` から周期 `period` で `count` 行。
    fn series(from: i64, period: i64, count: i64) -> Vec<i64> {
        (0..count).map(|i| from + i * period).collect()
    }

    #[test]
    fn a_full_series_passes() {
        let times = series(0, 1000, 1800);
        let j = judge_rows(&times, 1000, 1_800_000);
        assert!(j.passes(), "{j:?}");
        assert_eq!((j.gaps, j.missing, j.expected), (0, 0, 1800));
    }

    #[test]
    fn rows_at_the_edges_within_the_allowance_pass() {
        for count in [1798, 1799, 1801, 1802] {
            let j = judge_rows(&series(0, 1000, count), 1000, 1_800_000);
            assert!(j.passes(), "{count} 行: {j:?}");
        }
    }

    #[test]
    fn too_few_or_too_many_rows_fail_even_without_gaps() {
        // 最初の 3 行だけ（あとは書かれていない）。間隔は正常でも行数で落ちる。
        let j = judge_rows(&series(0, 1000, 3), 1000, 1_800_000);
        assert!(!j.passes(), "{j:?}");
        assert_eq!(j.gaps, 0);
        for count in [1797, 1803] {
            let j = judge_rows(&series(0, 1000, count), 1000, 1_800_000);
            assert!(!j.passes(), "{count} 行: {j:?}");
        }
        // 空も落ちる。
        assert!(!judge_rows(&[], 1000, 1_800_000).passes());
    }

    #[test]
    fn alternating_loss_fails() {
        // 1 行おきに抜ける = 間隔がちょうど 2 周期。
        let times = series(0, 2000, 900);
        let j = judge_rows(&times, 1000, 1_800_000);
        assert!(!j.passes(), "{j:?}");
        assert_eq!(j.gaps, 899);
        assert_eq!(j.missing, 899);
    }

    #[test]
    fn a_single_gap_of_exactly_two_periods_is_one_missing_row() {
        let mut times = series(0, 200, 9001);
        times.remove(4500);
        let j = judge_rows(&times, 200, 1_800_000);
        assert_eq!((j.gaps, j.missing, j.max_interval_ms), (1, 1, 400));
        // 行数は許容に収まっても、抜けがあれば落ちる。
        assert!((j.rows - j.expected).abs() <= EDGE_ROWS);
        assert!(!j.passes(), "{j:?}");
    }

    #[test]
    fn missing_rows_are_counted_from_the_rounded_interval() {
        assert_eq!(missing_between(1999, 1000), 0, "2 周期未満は揺れ");
        assert_eq!(missing_between(2000, 1000), 1);
        assert_eq!(missing_between(2400, 1000), 1);
        assert_eq!(missing_between(2600, 1000), 2);
        assert_eq!(missing_between(10_000, 1000), 9);
    }

    #[test]
    fn a_late_row_followed_by_a_short_interval_is_jitter_not_loss() {
        // 30 分の実測の最大間隔（200ms の周期で 367ms）の形: 遅れた行の次は詰まる。
        let mut times = series(0, 200, 9001);
        times[10] += 167;
        let j = judge_rows(&times, 200, 1_800_000);
        assert!(j.passes(), "{j:?}");
        assert_eq!(j.max_interval_ms, 367);
    }

    #[test]
    fn percentile_is_nearest_rank() {
        let values: Vec<u128> = (1..=100).collect();
        assert_eq!(percentile(&values, 50), 50);
        assert_eq!(percentile(&values, 95), 95);
        assert_eq!(percentile(&values, 100), 100);
        assert_eq!(percentile(&[], 95), 0);
        assert_eq!(percentile(&[7], 50), 7);
    }
}
