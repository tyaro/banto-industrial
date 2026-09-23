//! #414 段階2（2026-09-23 オーナー決定「開始時に不正なタグ・接続だけを
//! 外し、残りを動かす。DB は null でよい」）を、**実際のワイヤ経路と実際の
//! データファイル**で固定する。
//!
//! `collect_roundtrip.rs` と同じく、`banto_collect::simulation` の Modbus
//! シミュレータを**そこに居る PLC** として立て、普通の Modbus TCP 接続として
//! 登録する。その下に、
//!
//! * 正常なタグ `good`（40001 = ランプ波）と、
//! * **保存時の検証（#418）より前に入った**不正なタグ `legacy`（Modbus 接続の
//!   下に MELSEC 表記の `D3000`）
//!
//! を置く。`banto_tags` のサービスはアドレスの書式を検証しない（検証は
//! REST / Tauri の層）ので、サービスを直接使えば検証をくぐった既存データを
//! そのまま作れる。さらに**ポート番号が壊れた**接続（行を SQL で直接
//! 書き換える - 画面・サービスの検証はどちらも拒否する）とその下の
//! グループ・タグも置き、接続ごと外れる場合も見る。
//!
//! 確かめること:
//!
//! * (a) 状態は `Running`（`StartFailed` ではない）で、外した 4 件
//!   （タグ `legacy`・壊れた接続・その下のグループとタグ）が一覧に載る、
//! * (b) 現在値: `good` は `good`、外したタグは `invalid`（値も時刻も無い）、
//! * (c) **履歴**: `banto-tsquery` で `good` と `legacy` を一緒に読むと、
//!   **エラーではなく**、`good` には値が、`legacy` には `null` が並ぶ
//!   （外したタグには tstore の列を作らない = 方式 B。読み出しは
//!   「そのファイルのスキーマに無い `tag_key`」を欠測として返す）。
//!   間引き読み出しでも `legacy` は `Gap`、
//! * (d) 接続ごと外したグループは行そのものが無い（読んでもエラーにならず
//!   0 行 = 欠測）。
//!
//! 待ちはすべて「条件をポーリング + 上限時間」（固定 sleep で緑にしない）。

use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use banto_collect::simulation;
use banto_collect::Protocol;
use banto_tags::{
    CollectionGroupInput, CollectionGroupService, PlcConnectionInput, PlcConnectionService,
    TagInput, TagService,
};
use banto_tsquery::{BinValue, TsQuery};
use chronogazer_core::collect::{
    resolve_data_dir, CollectorService, CollectorState, CurrentSampleView, ExclusionUnitView,
    QualityView, Readout,
};
use chronogazer_core::db::init_db;
use sqlx::SqlitePool;

const WAIT_LIMIT: Duration = Duration::from_secs(20);
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// 読み出し範囲の上端（今から 1 日後）。`i64::MAX` は `banto-tsquery` の
/// ファイル選別（UTC オフセットの加減算）で溢れるので使わない。下端は 0。
fn far_future_ms() -> i64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after epoch")
        .as_millis();
    i64::try_from(now).expect("fits i64") + 86_400_000
}

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

/// `collect_roundtrip.rs` の `TempDir` と同じ（Windows で WAL の SQLite を
/// 閉じた直後の削除を再試行する）。
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

async fn connection(pool: &SqlitePool, name: &str, port: u16) -> i64 {
    PlcConnectionService::new(pool.clone())
        .create(PlcConnectionInput {
            name: name.to_string(),
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
        .expect("PLC 接続を登録できる")
        .id
}

async fn group(pool: &SqlitePool, name: &str, conn_id: i64) -> i64 {
    CollectionGroupService::new(pool.clone())
        .create(CollectionGroupInput {
            name: name.to_string(),
            plc_connection_id: conn_id,
            period_ms: 100,
            enabled: true,
            default_writable: false,
            query_sql: None,
        })
        .await
        .expect("収集グループを登録できる")
        .id
}

/// `banto_tags` のサービスはアドレスの書式を見ないので、不正なアドレスも
/// そのまま入る（= 保存時の検証より前に入った既存データ）。
async fn tag(pool: &SqlitePool, name: &str, group_id: i64, address: &str) -> i64 {
    TagService::new(pool.clone())
        .create(TagInput {
            name: name.to_string(),
            collection_group_id: group_id,
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
        .expect("タグを登録できる")
        .id
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn broken_items_are_left_out_their_values_are_invalid_and_their_history_is_null() {
    let dir = TempDir::new();
    let pool = init_db(&dir.path().join("chronogazer.sqlite3"))
        .await
        .expect("設定 DB を初期化できる");
    let data_dir = resolve_data_dir(dir.path(), "./data");
    let sim = simulation::start(Protocol::ModbusTcp).await;

    let conn = connection(&pool, "dev-plc", sim.addr().port()).await;
    let grp = group(&pool, "一巡", conn).await;
    let good = tag(&pool, "good", grp, "40001").await;
    let legacy = tag(&pool, "legacy", grp, "D3000").await;

    // 接続ごと外れる側: 作ってから、ポート番号を u16 に収まらない値へ
    // 直接書き換える（どの入口の検証も拒否する値 = 検証をくぐった既存行）。
    let broken_conn = connection(&pool, "broken-plc", 1).await;
    let broken_grp = group(&pool, "壊れた接続のグループ", broken_conn).await;
    let orphan = tag(&pool, "orphan", broken_grp, "40001").await;
    sqlx::query("UPDATE plc_connections SET port = 70000 WHERE id = ?")
        .bind(broken_conn)
        .execute(&pool)
        .await
        .expect("ポートを壊す");

    let good_key = format!("tag:{good}");
    let legacy_key = format!("tag:{legacy}");
    let orphan_key = format!("tag:{orphan}");
    let group_key = format!("grp:{grp}");
    let broken_group_key = format!("grp:{broken_grp}");

    let svc = CollectorService::new(pool.clone(), data_dir.clone());
    let outcome = svc.start().await.expect("不正な設定があっても開始できる");
    assert!(!outcome.pending, "開始が打ち切られた: {:?}", outcome.status);

    // (a) Running と、外した一覧。
    let exclusions = match svc.state() {
        CollectorState::Running {
            groups: 1,
            tags: 1,
            exclusions,
        } => exclusions,
        other => panic!("Running {{ groups: 1, tags: 1, .. }} を期待したが {other:?}"),
    };
    let listed: Vec<(ExclusionUnitView, String, &str)> = exclusions
        .iter()
        .map(|e| (e.unit, e.key.clone(), e.reason))
        .collect();
    assert_eq!(
        listed,
        vec![
            (ExclusionUnitView::Tag, legacy_key.clone(), "invalidAddress"),
            (
                ExclusionUnitView::Connection,
                format!("conn:{broken_conn}"),
                "invalidPort"
            ),
            (
                ExclusionUnitView::Group,
                broken_group_key.clone(),
                "connectionExcluded"
            ),
            (
                ExclusionUnitView::Tag,
                orphan_key.clone(),
                "connectionExcluded"
            ),
        ]
    );

    // (b) 現在値。
    wait_until("good の現在値が good で値を持つこと", || async {
        let Readout::Ready { data } = svc.values() else {
            return Err(svc.values().as_str().to_string());
        };
        match data.get(&good_key) {
            Some(sample) if sample.quality == QualityView::Good && sample.value.is_some() => Ok(()),
            other => Err(format!("{good_key} = {other:?}")),
        }
    })
    .await;
    let Readout::Ready { data } = svc.values() else {
        panic!("走っているのに現在値が Ready でない");
    };
    assert_eq!(data.get(&legacy_key), Some(&CurrentSampleView::invalid()));
    assert_eq!(data.get(&orphan_key), Some(&CurrentSampleView::invalid()));

    // (c) 履歴: good と legacy を一緒に読むと、エラーではなく legacy は null。
    let query = TsQuery::new(&data_dir);
    let keys = vec![good_key.clone(), legacy_key.clone()];
    let range = wait_until(
        "good の値の入った行がデータファイルに入ること",
        || async {
            let range = query
                .read_range(&group_key, &keys, 0, far_future_ms(), None)
                .await
                .map_err(|err| format!("読み出しがエラーになった: {err}"))?;
            if range.rows.iter().any(|row| row.values[0].is_some()) {
                Ok(range)
            } else {
                Err(format!("{} 行、good の値はまだ無い", range.rows.len()))
            }
        },
    )
    .await;
    assert_eq!(range.tag_keys, keys);
    assert!(
        range.rows.iter().all(|row| row.values[1].is_none()),
        "外したタグの履歴に値が入っている: {:?}",
        range.rows
    );

    let from = range.rows.first().expect("行がある").ptime_ms;
    let to = range.rows.last().expect("行がある").ptime_ms;
    let decimated = query
        .read_decimated(&group_key, &keys, from, to.max(from + 1), 10)
        .await
        .expect("間引き読み出しもエラーにならない");
    assert!(
        decimated
            .bins
            .iter()
            .all(|bin| bin.tags[1] == BinValue::Gap),
        "外したタグの間引き結果が Gap でない: {:?}",
        decimated.bins
    );

    // (d) 接続ごと外したグループは行そのものが無い（エラーではない）。
    let orphan_range = query
        .read_range(
            &broken_group_key,
            std::slice::from_ref(&orphan_key),
            0,
            far_future_ms(),
            None,
        )
        .await
        .expect("外したグループを読んでもエラーにならない");
    assert!(orphan_range.rows.is_empty(), "{:?}", orphan_range.rows);

    let outcome = svc.stop().await.expect("収集を停止できる");
    assert!(!outcome.pending);
    assert_eq!(svc.state(), CollectorState::Stopped);
    // 停止後の最終 flush を経ても、legacy は null のまま・読み出しはエラーに
    // ならない。
    let after = query
        .read_range(&group_key, &keys, 0, far_future_ms(), None)
        .await
        .expect("停止後も読める");
    assert!(after.rows.iter().all(|row| row.values[1].is_none()));

    sim.stop().await;
    pool.close().await;
}
