//! 期間指定出力の読み出し（#1207 R3 PR2）のテスト。データファイルは履歴の
//! テストと同じ道具（`TsWriter` + `ManualClock`）で作る。

use std::sync::Arc;

use banto_tstore::{ManualClock, StoreConfig, TsWriter};

use super::*;
use crate::collect::history::tests::{connection, group, group_config, tag, BASE_MS, OFFSET_MS};
use crate::db::init_db_memory;
use crate::test_support::TempDir;

fn field_names(err: BantoError) -> Vec<String> {
    match err {
        BantoError::Validation { field_errors } => {
            field_errors.into_iter().map(|e| e.field).collect()
        }
        other => panic!("Validation を期待したが {other:?}"),
    }
}

// --- 検証 ---------------------------------------------------------------------

#[test]
fn validation_table() {
    let ok = |tags: &[i64], f: i64, t: i64, mode: &str, iv: Option<i64>| {
        let stat = (mode == "interval").then_some("avg");
        validate_export_request(tags, f, t, mode, iv, stat)
    };
    let with_stat = |mode: &str, iv: Option<i64>, stat: Option<&str>| {
        validate_export_request(&[1], 0, 1, mode, iv, stat)
    };
    // 通る形。stat は interval でだけ要り、それぞれ選ばれる。
    for (name, expected) in [
        ("avg", ExportStat::Avg),
        ("min", ExportStat::Min),
        ("max", ExportStat::Max),
    ] {
        let req = with_stat("interval", Some(1000), Some(name)).unwrap();
        assert_eq!(req.stat, Some(expected));
    }
    assert_eq!(with_stat("raw", None, None).unwrap().stat, None);
    let req = ok(&[3, 1, 3], 0, 10, "raw", None).unwrap();
    assert_eq!(req.tag_ids(), &[3, 1], "重複は除き、順は保つ");
    assert!(ok(&[1], 5, 5, "raw", None).is_ok(), "from == to は通る");
    assert!(ok(&[1], 0, HISTORY_MAX_WINDOW_MS, "raw", None).is_ok());
    assert!(ok(&[1], 0, 10_000, "interval", Some(1000)).is_ok());
    // ちょうど 10 万区間は通る。
    assert!(ok(&[1], 0, 99_999 * 1000, "interval", Some(1000)).is_ok());

    let cases: Vec<(Result<ExportRequest, BantoError>, Vec<&str>)> = vec![
        (ok(&[], 0, 1, "raw", None), vec!["tagIds"]),
        (
            ok(&[1, 2, 3, 4, 5, 6, 7, 8, 9], 0, 1, "raw", None),
            vec!["tagIds"],
        ),
        (ok(&[1], 2, 1, "raw", None), vec!["toMs"]),
        (
            ok(&[1], 0, HISTORY_MAX_WINDOW_MS + 1, "raw", None),
            vec!["toMs"],
        ),
        (ok(&[1], 0, 1, "csv", None), vec!["mode"]),
        (ok(&[1], 0, 1, "", None), vec!["mode"]),
        (ok(&[1], 0, 1, "raw", Some(1000)), vec!["intervalMs"]),
        (ok(&[1], 0, 1, "interval", None), vec!["intervalMs"]),
        (ok(&[1], 0, 1, "interval", Some(999)), vec!["intervalMs"]),
        (ok(&[1], 0, 1, "interval", Some(0)), vec!["intervalMs"]),
        (ok(&[1], 0, 1, "interval", Some(-5)), vec!["intervalMs"]),
        (with_stat("interval", Some(1000), None), vec!["stat"]),
        (
            with_stat("interval", Some(1000), Some("median")),
            vec!["stat"],
        ),
        (with_stat("interval", Some(1000), Some("AVG")), vec!["stat"]),
        (with_stat("interval", Some(1000), Some("")), vec!["stat"]),
        (with_stat("raw", None, Some("avg")), vec!["stat"]),
        // intervalMs と stat の誤りは両方並ぶ。
        (
            with_stat("interval", Some(5), Some("x")),
            vec!["intervalMs", "stat"],
        ),
        // 区間の数 = 100001 > 10 万。
        (
            ok(&[1], 0, 100_000 * 1000, "interval", Some(1000)),
            vec!["intervalMs"],
        ),
        // 複数の誤りは全部並ぶ。
        (ok(&[], 2, 1, "x", Some(1)), vec!["tagIds", "toMs", "mode"]),
    ];
    for (i, (result, expected)) in cases.into_iter().enumerate() {
        assert_eq!(
            field_names(result.expect_err(&format!("ケース {i} が通っている"))),
            expected,
            "ケース {i}"
        );
    }
}

#[test]
fn wire_shape() {
    let readout = Readout::Ready {
        data: CollectExport {
            from_ms: 1,
            to_ms: 2,
            mode: ExportMode::Interval,
            interval_ms: Some(1000),
            stat: Some(ExportStat::Max),
            tags: vec![ExportTag {
                tag_id: 7,
                simulation: true,
            }],
            unknown_tag_ids: vec![9],
            rows: vec![ExportRow {
                t_ms: 1,
                values: vec![Some(3.0), None],
            }],
        },
    };
    assert_eq!(
        serde_json::to_value(&readout).unwrap(),
        serde_json::json!({
            "state": "ready",
            "data": {
                "fromMs": 1, "toMs": 2, "mode": "interval", "intervalMs": 1000, "stat": "max",
                "tags": [{ "tagId": 7, "simulation": true }],
                "unknownTagIds": [9],
                "rows": [{ "tMs": 1, "values": [3.0, null] }]
            }
        })
    );
}

// --- データファイルを読む ---------------------------------------------------

struct Fixture {
    pool: SqlitePool,
    svc: CollectorService,
    _dir: TempDir,
    a1: i64,
    b1: i64,
    s1: i64,
}

/// グループ A（1s 周期、`a1` = k、k = 5 だけ +∞）は 0..10 秒の全部、
/// グループ B（2s 周期、`b1` = 100 + k）は偶数秒だけ、グループ S（シミュ）は無記録。
async fn fixture() -> Fixture {
    let dir = TempDir::new();
    let data_dir = dir.path().join("data");
    let pool = init_db_memory().await.expect("init_db_memory");
    let real = connection(&pool, "real", false).await;
    let sim = connection(&pool, "sim", true).await;
    let grp_a = group(&pool, "A", real, 1000).await;
    let grp_b = group(&pool, "B", real, 2000).await;
    let grp_s = group(&pool, "S", sim, 1000).await;
    let a1 = tag(&pool, "a1", grp_a).await;
    let b1 = tag(&pool, "b1", grp_b).await;
    let s1 = tag(&pool, "s1", grp_s).await;

    let clock = Arc::new(ManualClock::new(BASE_MS, OFFSET_MS));
    let writer = TsWriter::open(
        &data_dir,
        StoreConfig {
            groups: vec![
                group_config(grp_a, 1000, &[a1]),
                group_config(grp_b, 2000, &[b1]),
            ],
        },
        clock,
    )
    .await
    .expect("writer を開ける");
    for k in 0..10_i64 {
        let t = BASE_MS + k * 1000;
        let v = if k == 5 { f64::INFINITY } else { k as f64 };
        writer
            .append(&format!("grp:{grp_a}"), t, &[Some(v)])
            .await
            .unwrap();
        if k % 2 == 0 {
            writer
                .append(&format!("grp:{grp_b}"), t, &[Some(100.0 + k as f64)])
                .await
                .unwrap();
        }
    }
    writer.close().await.unwrap();
    let svc = CollectorService::new(pool.clone(), data_dir, crate::test_support::drivers());
    Fixture {
        pool,
        svc,
        _dir: dir,
        a1,
        b1,
        s1,
    }
}

fn v(x: f64) -> Option<f64> {
    Some(x)
}

async fn ready(svc: &CollectorService, request: &ExportRequest) -> CollectExport {
    match svc.export(request).await.expect("検証エラーではない") {
        Readout::Ready { data } => data,
        other => panic!("ready を期待したが {other:?}"),
    }
}

/// 生の値: 2 つのグループを時刻でマージして 1 枚の横持ちにする。B は偶数秒だけ
/// なので奇数秒は `null`。+∞ は `null`。居ないタグは `unknownTagIds`、
/// シミュレーションのタグは列に居て `simulation: true`・全部 `null`。
///
/// 反証: グループごとに行を作って連結する（時刻でマージしない）と行数が 15 に
/// なって落ちる。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn raw_rows_are_merged_across_groups_by_time() {
    let f = fixture().await;
    let unknown = 999_999;
    let request = validate_export_request(
        &[f.b1, unknown, f.a1, f.s1],
        BASE_MS,
        BASE_MS + 9_999,
        "raw",
        None,
        None,
    )
    .unwrap();
    let data = ready(&f.svc, &request).await;

    assert_eq!(data.mode, ExportMode::Raw);
    assert_eq!(data.interval_ms, None);
    assert_eq!(data.stat, None);
    assert_eq!(data.unknown_tag_ids, vec![unknown]);
    let cols: Vec<(i64, bool)> = data.tags.iter().map(|t| (t.tag_id, t.simulation)).collect();
    assert_eq!(cols, vec![(f.b1, false), (f.a1, false), (f.s1, true)]);

    assert_eq!(data.rows.len(), 10, "A の 10 時刻に B の 5 時刻が重なる");
    for (k, row) in data.rows.iter().enumerate() {
        let k = k as i64;
        assert_eq!(row.t_ms, BASE_MS + k * 1000, "昇順・重複なし");
        let b = if k % 2 == 0 {
            v(100.0 + k as f64)
        } else {
            None
        };
        let a = if k == 5 { None } else { v(k as f64) };
        assert_eq!(row.values, vec![b, a, None], "k = {k}");
    }
    f.pool.close().await;
}

/// 間隔: 2 秒幅、`stat` ごとに選ばれる数値が違う。全区間の行が出て、サンプルの無い区間（範囲の後ろ）は全部 `null`。
/// +∞ を含む区間の `a1` は `null`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interval_rows_include_gaps_and_aggregate() {
    let f = fixture().await;
    // 0..14 秒を 2 秒ごと = 8 区間（10〜14 秒は無記録）。
    let request = validate_export_request(
        &[f.a1, f.b1],
        BASE_MS,
        BASE_MS + 14_000,
        "interval",
        Some(2000),
        Some("avg"),
    )
    .unwrap();
    let data = ready(&f.svc, &request).await;
    assert_eq!(data.stat, Some(ExportStat::Avg));
    assert_eq!(data.mode, ExportMode::Interval);
    assert_eq!(data.interval_ms, Some(2000));
    assert_eq!(data.rows.len(), 8);
    for (i, row) in data.rows.iter().enumerate() {
        assert_eq!(row.t_ms, BASE_MS + i as i64 * 2000);
    }
    // 区間 0: a1 = {0,1} -> avg 0.5、b1 = {100}。
    assert_eq!(data.rows[0].values, vec![v(0.5), v(100.0)]);
    // 区間 2: a1 = {4, +∞} -> avg は非有限なので null、b1 = {104}。
    assert_eq!(data.rows[2].values, vec![None, v(104.0)]);
    // 区間 5 以降: 無記録の隙間。行はあり、全部 null。
    for row in &data.rows[5..] {
        assert_eq!(row.values, vec![None, None]);
    }
    // 同じ区間 0 を min・max で読むと、選んだ統計量だけが出る。
    // 区間 2 の a1 = {4, +∞} は、選んだ統計量が有限（min = 4）のときだけ値が出る。
    for (stat, expected, gap_bin) in [("min", 0.0, v(4.0)), ("max", 1.0, None)] {
        let request = validate_export_request(
            &[f.a1, f.b1],
            BASE_MS,
            BASE_MS + 14_000,
            "interval",
            Some(2000),
            Some(stat),
        )
        .unwrap();
        let data = ready(&f.svc, &request).await;
        assert_eq!(data.rows[0].values, vec![v(expected), v(100.0)], "{stat}");
        assert_eq!(data.rows[2].values, vec![gap_bin, v(104.0)], "{stat}");
    }
    f.pool.close().await;
}

/// 行数の上限: 生の値は**グループをまたいだ合計**が上限を超えたら打ち切る
/// （小さい上限を渡して確かめる）。`export` はこれを `toMs` の検証エラーにする。
///
/// 反証: 合計ではなくグループごとに上限を掛けると、A 10 行 + B 5 行（上限 12）が
/// 通って落ちる。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn raw_row_budget_is_shared_across_groups() {
    let f = fixture().await;
    let query = TsQuery::with_place(f.svc.inner.ctx.data_place());
    let request =
        validate_export_request(&[f.a1, f.b1], BASE_MS, BASE_MS + 9_999, "raw", None, None)
            .unwrap();
    for limit in [4, 12] {
        let err = read_export(&f.pool, &query, &request, limit)
            .await
            .expect_err("上限を超えるのに通っている");
        assert!(
            matches!(err, ExportReadError::TooManyRows),
            "{limit}: {err}"
        );
    }
    // 合計ちょうど（10 + 5）なら通る。
    let ok = read_export(&f.pool, &query, &request, 15).await.unwrap();
    assert_eq!(ok.rows.len(), 10);
    assert!(TOO_MANY_ROWS_MESSAGE.contains("10 万行"));
    // `export` が使う写し: 行数超過だけが利用者向けの検証エラー（`toMs`）、
    // それ以外の失敗は `Unavailable`。
    assert_eq!(
        field_names(map_read_error(ExportReadError::TooManyRows).expect_err("検証エラー")),
        vec!["toMs"]
    );
    assert!(matches!(
        map_read_error(ExportReadError::Registry(sqlx::Error::RowNotFound)),
        Ok(Readout::Unavailable)
    ));
    f.pool.close().await;
}

/// データファイルが無い・タグが全部居ない: `ready`（interval は全区間の行）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_data_is_ready_and_unknown_only_has_no_rows() {
    let dir = TempDir::new();
    let pool = init_db_memory().await.expect("init_db_memory");
    let conn = connection(&pool, "real", false).await;
    let grp = group(&pool, "A", conn, 1000).await;
    let t = tag(&pool, "t", grp).await;
    let svc = CollectorService::new(
        pool.clone(),
        dir.path().join("never-created"),
        crate::test_support::drivers(),
    );
    let request = validate_export_request(
        &[t],
        BASE_MS,
        BASE_MS + 2_999,
        "interval",
        Some(1000),
        Some("avg"),
    )
    .unwrap();
    let data = ready(&svc, &request).await;
    assert_eq!(data.rows.len(), 3);
    assert!(data.rows.iter().all(|r| r.values == vec![None]));

    let raw = validate_export_request(&[t], BASE_MS, BASE_MS + 2_999, "raw", None, None).unwrap();
    assert!(ready(&svc, &raw).await.rows.is_empty());

    let unknown =
        validate_export_request(&[424_242], BASE_MS, BASE_MS + 2_999, "raw", None, None).unwrap();
    let data = ready(&svc, &unknown).await;
    assert!(data.tags.is_empty() && data.rows.is_empty());
    assert_eq!(data.unknown_tag_ids, vec![424_242]);
    pool.close().await;
}
