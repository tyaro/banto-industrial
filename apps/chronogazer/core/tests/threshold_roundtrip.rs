//! #532（2026-10-08 オーナー決定）: 収集のしきい値の判定は**記録計の側の
//! タグごとの設定**（`chronogazer_core::tag_thresholds`）から作り、banto-tags の
//! `tags.threshold_*` の列は使わない。判定に使ったしきい値はイベントに残る。
//!
//! 実際のワイヤ経路（`banto_collect::simulation` のシミュレータを普通の
//! Modbus TCP 接続として登録 - `collect_roundtrip.rs` と同じ）で、次を固定する:
//!
//! * タグの列には `HH = 0`（#532 より前の DB に残っていた値の想定。u16 は必ず
//!   0 以上なので、列を使えば最初の読みで `HH` に入る）、記録計の側の設定には
//!   `H = 0` を入れる。収集を始めると、**`H` に入った**イベントが、判定に使った
//!   しきい値 `0` 付きで記録される（`HH` は出ない = 列は使われていない）。
//! * 保存は走っている収集には反映されない。再起動で反映される（タグの保存と
//!   同じ約束）: 走行中に設定を消しても新しいイベントは出ず、再起動後は判定
//!   しない（`H` に入ったイベントは 1 件のまま）。
//!
//! 待ちはすべて「条件をポーリング + 上限時間」。

use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use banto_collect::simulation;
use banto_collect::Protocol;
use banto_tags::{
    CollectionGroupInput, CollectionGroupService, PlcConnectionInput, PlcConnectionService,
    TagInput, TagService,
};
use chronogazer_core::collect::{
    resolve_data_dir, CollectEventRow, CollectorService, EventPage, QualityView, Readout,
};
use chronogazer_core::db::init_db;
use chronogazer_core::tag_thresholds::{TagThresholdService, TagThresholdsPayload};
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

/// 後始末を再試行する一時ディレクトリ（`collect_roundtrip.rs` と同じ理由）。
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

/// 接続 1 / 収集グループ 1（100ms）/ タグ 1（列に `HH = 0`）。タグ id を返す。
async fn register(pool: &SqlitePool, port: u16) -> i64 {
    let conn = PlcConnectionService::new(pool.clone())
        .create(PlcConnectionInput {
            name: "dev-plc".to_string(),
            protocol: "modbus-tcp".to_string(),
            host: "127.0.0.1".to_string(),
            port: i64::from(port),
            unit_id: 1,
            enabled: true,
            simulation: false,
            word_order: String::new(),
            database: None,
            username: None,
            password: None,
        })
        .await
        .expect("PLC 接続を登録できる");
    let group = CollectionGroupService::new(pool.clone())
        .create(CollectionGroupInput {
            name: "しきい値".to_string(),
            plc_connection_id: conn.id,
            period_ms: 100,
            enabled: true,
            default_writable: false,
            query_sql: None,
        })
        .await
        .expect("収集グループを登録できる");
    // banto-tags のサービスを直接使い、列にしきい値を入れる（ChronoGazer の
    // 画面・REST はもう列に書かない。#532 より前の DB の想定）。
    TagService::new(pool.clone())
        .create(TagInput {
            name: "ランプ".to_string(),
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
            threshold_hh: Some(0.0),
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
        .expect("タグを登録できる")
        .id
}

async fn threshold_events(svc: &CollectorService) -> Result<Vec<CollectEventRow>, String> {
    match svc.events(EventPage::default()).await {
        Readout::Ready { data } => Ok(data
            .rows
            .into_iter()
            .filter(|row| row.kind.starts_with("threshold_"))
            .collect()),
        other => Err(format!("イベント一覧が読めない: {}", other.as_str())),
    }
}

async fn wait_for_good_value(svc: &CollectorService, tag_key: &str) {
    wait_until("現在値が good で値を持つこと", || async {
        let Readout::Ready { data } = svc.values() else {
            return Err(svc.values().as_str().to_string());
        };
        match data.get(tag_key) {
            Some(sample) if sample.quality == QualityView::Good && sample.value.is_some() => Ok(()),
            other => Err(format!("{tag_key} = {other:?}")),
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn collection_judges_by_the_recorder_side_thresholds_and_records_the_limit() {
    let dir = TempDir::new();
    let pool = init_db(dir.path().join("chronogazer.sqlite3"))
        .await
        .expect("設定 DB を初期化できる");
    let data_dir = resolve_data_dir(dir.path(), "./data");
    let sim = simulation::start(Protocol::ModbusTcp).await;
    let tag_id = register(&pool, sim.addr().port()).await;
    let tag_key = format!("tag:{tag_id}");

    let thresholds = TagThresholdService::new(pool.clone());
    let saved = thresholds
        .update(
            tag_id,
            &TagThresholdsPayload {
                threshold_h: Some(0.0),
                expected_revision: Some(0),
                ..TagThresholdsPayload::default()
            },
        )
        .await
        .expect("記録計の側のしきい値を保存できる");

    let svc = CollectorService::new(pool.clone(), data_dir);
    svc.start().await.expect("収集を開始できる");
    wait_for_good_value(&svc, &tag_key).await;

    let rows = wait_until(
        "しきい値のイベントが記録されること",
        || async {
            let rows = threshold_events(&svc).await?;
            if rows.is_empty() {
                Err("（0 件）".to_string())
            } else {
                Ok(rows)
            }
        },
    )
    .await;
    let summary: Vec<(String, Option<String>, Option<f64>)> = rows
        .iter()
        .map(|row| (row.kind.clone(), row.level.clone(), row.limit_value))
        .collect();
    assert_eq!(
        summary,
        vec![(
            "threshold_entered".to_string(),
            Some("H".to_string()),
            Some(0.0)
        )],
        "記録計の側の H = 0 で判定し、タグの列の HH は使わない"
    );
    assert_eq!(rows[0].tag_key.as_deref(), Some(tag_key.as_str()));

    // 走行中に設定を消しても、走っている収集には反映されない（新しいイベントは
    // 出ない）。再起動で反映され、以後は判定しない。
    thresholds
        .update(
            tag_id,
            &TagThresholdsPayload {
                expected_revision: Some(saved.thresholds.revision),
                ..TagThresholdsPayload::default()
            },
        )
        .await
        .expect("しきい値を消せる");
    svc.restart().await.expect("収集を再起動できる");
    wait_for_good_value(&svc, &tag_key).await;
    // 再起動後の数周期（100ms 周期）を待ってから数える。
    tokio::time::sleep(Duration::from_millis(500)).await;
    let rows = threshold_events(&svc).await.expect("イベント一覧");
    assert_eq!(
        rows.iter()
            .filter(|row| row.kind == "threshold_entered")
            .count(),
        1,
        "設定を消して再起動した後に、しきい値のイベントが出た: {:?}",
        rows.iter().map(|row| &row.kind).collect::<Vec<_>>()
    );

    svc.stop().await.expect("収集を停止できる");
    sim.stop().await;
    pool.close().await;
}
