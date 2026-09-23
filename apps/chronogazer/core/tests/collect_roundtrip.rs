//! R1-C の完了条件（docs/r1-plan.md）「シミュレータ相手に、設定 → 収集開始 →
//! データファイル生成 → イベント記録まで一巡」を、**実際のワイヤ経路**で
//! 固定する（C-4、2026-09-23）。
//!
//! `src/collect.rs` の単体テストは偽クライアント（`OfflineClient`）で状態遷移
//! だけを見ていて、PLC と話す経路は一度も通っていない。ここでは
//! `banto_collect::simulation` のランプ波シミュレータ（`banto-plc` の実 TCP
//! シミュレータ）を**そこに居る PLC** として立て、ChronoGazer には**普通の
//! Modbus TCP / SLMP 接続**（`simulation = false`・ホスト 127.0.0.1・
//! ポート = シミュレータの待ち受け）として登録する - 接続単位の
//! シミュレーション（`simulation = true`）は値が tstore に記録されない
//! （`crates/banto-collect/src/task.rs` の `if ctx.simulation { return; }`
//! 付近）ため、データファイル生成まで含む一巡はこの経路でしか確かめられない。
//! `CollectorService::new`（本番と同じクライアント生成・同じ既定の
//! タイムアウト）を使う。`examples/dev_plc.rs` が別プロセスで立てるものと
//! 同じシミュレータで、ここでは OS 割り当てのポートを使う（テストを並列に
//! 走らせても衝突しない）。
//!
//! 確かめること（Modbus と SLMP の両方で）:
//!
//! * (a) 状態が `Running`、
//! * (b) 現在値が `good` で、**値が変化する**（ランプ波が届いている）、
//! * (c) `data.dir` に `YYYYMMDD-NNN.sqlite3` が生成され、**サンプル行が入って
//!   いる**（ファイルがあるだけでは足りない - `TsReader` で読み戻す）、
//! * (d) `collect_events` に `collection_started` と、この接続の
//!   `plc_connected` が記録される、
//! * (e) 停止後に `collection_stopped` が記録され、状態が `Stopped`。
//!
//! **待ちはすべて「条件をポーリング + 上限時間」**（[`wait_until`]）。固定
//! sleep で緑にしない。上限に達したら、最後に観測した値を添えて落とす。
//!
//! レジストリへの登録は `banto_tags` の公開サービス（REST / Tauri と同じ
//! 入口）を使い、SQL を直接書かない。

use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use banto_collect::simulation::{self, SimulatorHandle};
use banto_collect::Protocol;
use banto_tags::{
    CollectionGroupInput, CollectionGroupService, PlcConnectionInput, PlcConnectionService,
    TagInput, TagService,
};
use banto_tstore::{list_data_files, TsReader};
use chronogazer_core::collect::{
    resolve_data_dir, CollectEventRow, CollectorService, CollectorState, ConnectionStatusView,
    EventPage, QualityView, Readout,
};
use chronogazer_core::db::init_db;
use sqlx::SqlitePool;

/// 条件待ちの上限。既定の接続タイムアウト（3 秒）・tstore の flush 間隔
/// （1 秒）に対して十分な余裕を取る。**普段はここまで待たない** - 条件が
/// 成り立った時点で抜ける。
const WAIT_LIMIT: Duration = Duration::from_secs(20);
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// `cond` が `Ok` を返すまでポーリングする。[`WAIT_LIMIT`] を超えたら、
/// `what` と最後の観測（`Err` の中身）を添えて落とす。
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

/// 後始末を再試行する一時ディレクトリ（`src/test_support.rs` の `TempDir` と
/// 同じ理由: Windows では WAL の SQLite を閉じた直後もファイルが掴まれて
/// いることがあり、`tempfile` の 1 回きりの削除では残る。あちらは
/// `#[cfg(test)]` の crate 内部なので、統合テストからは使えない）。
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

/// レジストリに「接続 1 / 収集グループ 1（100ms）/ タグ 1」を入れ、
/// `(接続 id, グループ id, タグ id)` を返す。
async fn register(pool: &SqlitePool, protocol: &str, port: u16, address: &str) -> (i64, i64, i64) {
    let conn = PlcConnectionService::new(pool.clone())
        .create(PlcConnectionInput {
            name: format!("dev-plc-{protocol}"),
            protocol: protocol.to_string(),
            host: "127.0.0.1".to_string(),
            port: i64::from(port),
            unit_id: 1,
            enabled: true,
            // データファイル生成まで確かめるため、接続単位のシミュレーション
            // （tstore に記録されない）ではなく、外に立てたシミュレータを
            // 普通の PLC として登録する。
            simulation: false,
            // "" = プロトコルの既定（u16 の 1 ワードなので結果は変わらない）。
            word_order: String::new(),
            database: None,
            username: None,
            password: None,
        })
        .await
        .expect("PLC 接続を登録できる");
    let group = CollectionGroupService::new(pool.clone())
        .create(CollectionGroupInput {
            name: "一巡".to_string(),
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
            name: "ランプ".to_string(),
            collection_group_id: group.id,
            address: address.to_string(),
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

/// 収集イベントの種類（とその接続キー）を古い順に並べた要約。落ちたときの
/// メッセージ用。
fn summarize(rows: &[CollectEventRow]) -> String {
    let mut kinds: Vec<String> = rows
        .iter()
        .rev()
        .map(|row| match &row.connection_key {
            Some(key) => format!("{}({key})", row.kind),
            None => row.kind.clone(),
        })
        .collect();
    if kinds.is_empty() {
        kinds.push("（0 件）".to_string());
    }
    kinds.join(", ")
}

async fn read_events(svc: &CollectorService) -> Result<Vec<CollectEventRow>, String> {
    match svc.events(EventPage::default()).await {
        Readout::Ready { data } => Ok(data.rows),
        other => Err(format!("イベント一覧が読めない: {}", other.as_str())),
    }
}

/// データディレクトリの全ファイルから、グループ `group_key` のサンプルのうち
/// 値の入っている行を数える。
async fn count_samples_with_value(data_dir: &Path, group_key: &str) -> Result<usize, String> {
    let files = list_data_files(data_dir).map_err(|err| format!("列挙に失敗: {err}"))?;
    if files.is_empty() {
        return Err(format!("{} にデータファイルがまだ無い", data_dir.display()));
    }
    let mut count = 0;
    for file in &files {
        // 書き手がファイルを作った直後はスキーマがまだ無いことがある
        // （`TstoreError::Uninitialized`）- それも「まだ」として待つ。
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
        count += samples
            .iter()
            .filter(|sample| sample.values.first().copied().flatten().is_some())
            .count();
    }
    if count == 0 {
        return Err(format!(
            "{} 個のファイルに {group_key} の値の入った行がまだ無い",
            files.len()
        ));
    }
    Ok(count)
}

async fn roundtrip(protocol: Protocol, protocol_name: &str, address: &str) {
    let dir = TempDir::new();
    let db_path = dir.path().join("chronogazer.sqlite3");
    let pool = init_db(&db_path).await.expect("設定 DB を初期化できる");
    // banto-serve と同じく、既定の `./data` を DB の置き場基準で解決する。
    let data_dir = resolve_data_dir(dir.path(), "./data");

    let sim: SimulatorHandle = simulation::start(protocol).await;
    let (conn_id, group_id, tag_id) =
        register(&pool, protocol_name, sim.addr().port(), address).await;
    let conn_key = format!("conn:{conn_id}");
    let group_key = format!("grp:{group_id}");
    let tag_key = format!("tag:{tag_id}");

    let svc = CollectorService::new(pool.clone(), data_dir.clone());

    // (a) 開始して Running。
    let outcome = svc.start().await.expect("収集を開始できる");
    assert!(
        !outcome.pending,
        "開始が打ち切られた（{:?}）",
        outcome.status
    );
    assert_eq!(
        svc.state(),
        CollectorState::Running { groups: 1, tags: 1 },
        "開始後の状態"
    );

    // 接続ごとの状態が「接続中」になる（(d) の plc_connected と対になる観測）。
    wait_until("接続状態が connected になること", || async {
        match svc.connections().await {
            Readout::Ready { data } => match data.get(&conn_key) {
                Some(ConnectionStatusView::Connected) => Ok(()),
                other => Err(format!("{conn_key} = {other:?}")),
            },
            other => Err(other.as_str().to_string()),
        }
    })
    .await;

    // (b) 現在値が good で、値が変化する。
    let first = wait_until("現在値が good で値を持つこと", || async {
        let Readout::Ready { data } = svc.values() else {
            return Err(svc.values().as_str().to_string());
        };
        match data.get(&tag_key) {
            Some(sample) if sample.quality == QualityView::Good => sample
                .value
                .ok_or_else(|| format!("{tag_key} は good だが値が無い")),
            other => Err(format!("{tag_key} = {other:?}")),
        }
    })
    .await;
    wait_until(
        "現在値が変化すること（ランプ波が届いている）",
        || async {
            let Readout::Ready { data } = svc.values() else {
                return Err(svc.values().as_str().to_string());
            };
            match data.get(&tag_key) {
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

    // (c) data.dir にデータファイルができ、値の入ったサンプル行がある。
    wait_until(
        "データファイルに値の入ったサンプル行が入ること",
        || count_samples_with_value(&data_dir, &group_key),
    )
    .await;

    // (d) collection_started と、この接続の plc_connected。
    wait_until(
        "collection_started と plc_connected が記録されること",
        || async {
            let rows = read_events(&svc).await?;
            let started = rows.iter().any(|row| row.kind == "collection_started");
            let connected = rows.iter().any(|row| {
                row.kind == "plc_connected" && row.connection_key.as_deref() == Some(&conn_key)
            });
            if started && connected {
                Ok(())
            } else {
                Err(summarize(&rows))
            }
        },
    )
    .await;

    // (e) 停止 → collection_stopped と Stopped。
    let outcome = svc.stop().await.expect("収集を停止できる");
    assert!(!outcome.pending, "停止が打ち切られた");
    assert_eq!(svc.state(), CollectorState::Stopped, "停止後の状態");
    wait_until("collection_stopped が記録されること", || async {
        let rows = read_events(&svc).await?;
        if rows.iter().any(|row| row.kind == "collection_stopped") {
            Ok(())
        } else {
            Err(summarize(&rows))
        }
    })
    .await;
    // 停止後の最終 flush を経ても、行は消えていない。
    count_samples_with_value(&data_dir, &group_key)
        .await
        .expect("停止後もサンプル行が残っている");

    sim.stop().await;
    pool.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn modbus_tcp_roundtrip_against_the_simulator() {
    // 保持レジスタ 40001 = オフセット 0（ランプ波の対象）。
    roundtrip(Protocol::ModbusTcp, "modbus-tcp", "40001").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slmp_roundtrip_against_the_simulator() {
    // D0（ランプ波の対象）。
    roundtrip(Protocol::Slmp, "slmp", "D0").await;
}
