//! 日報の読み出し（#1207 R3）のテスト。データファイルは期間 CSV のテストと同じ道具
//! （`TsWriter` + `ManualClock`）で作る。

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

const H: i64 = DAILY_REPORT_BIN_MS;

#[test]
fn validation_table() {
    let req = validate_daily_report_request(&[3, 1, 3], 0, 24 * H).unwrap();
    assert_eq!(req.tag_ids(), &[3, 1], "重複は除き、順は保つ");
    assert!(validate_daily_report_request(&[1], 0, H).is_ok(), "1 時間");
    assert!(
        validate_daily_report_request(&[1], 0, 25 * H).is_ok(),
        "25 時間"
    );
    assert!(validate_daily_report_request(&[1, 2, 3, 4, 5, 6, 7, 8], 0, 24 * H).is_ok());

    let cases: Vec<(Result<DailyReportRequest, BantoError>, Vec<&str>)> = vec![
        (
            validate_daily_report_request(&[], 0, 24 * H),
            vec!["tagIds"],
        ),
        (
            validate_daily_report_request(&[1, 2, 3, 4, 5, 6, 7, 8, 9], 0, 24 * H),
            vec!["tagIds"],
        ),
        (validate_daily_report_request(&[1], 5, 5), vec!["toMs"]),
        (validate_daily_report_request(&[1], 10, 5), vec!["toMs"]),
        (validate_daily_report_request(&[1], 0, H - 1), vec!["toMs"]),
        (
            validate_daily_report_request(&[1], 0, 25 * H + 1),
            vec!["toMs"],
        ),
        (
            validate_daily_report_request(&[1], i64::MIN, i64::MAX),
            vec!["toMs"],
        ),
        (
            validate_daily_report_request(&[], 2, 1),
            vec!["tagIds", "toMs"],
        ),
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
    let stat = DailyReportStat {
        min: 1.0,
        max: 3.0,
        avg: 2.0,
        count: 3,
    };
    let readout = Readout::Ready {
        data: CollectDailyReport {
            from_ms: 1,
            to_ms: 2,
            interval_ms: DAILY_REPORT_BIN_MS,
            tags: vec![DailyReportTag {
                tag_id: 7,
                simulation: true,
            }],
            unknown_tag_ids: vec![9],
            hours: vec![DailyReportHour {
                t_ms: 1,
                values: vec![Some(stat), None],
            }],
            day: vec![Some(stat)],
        },
    };
    assert_eq!(
        serde_json::to_value(&readout).unwrap(),
        serde_json::json!({
            "state": "ready",
            "data": {
                "fromMs": 1, "toMs": 2, "intervalMs": 3_600_000,
                "tags": [{ "tagId": 7, "simulation": true }],
                "unknownTagIds": [9],
                "hours": [{ "tMs": 1, "values": [
                    { "min": 1.0, "max": 3.0, "avg": 2.0, "count": 3 }, null
                ] }],
                "day": [{ "min": 1.0, "max": 3.0, "avg": 2.0, "count": 3 }]
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

/// グループ A（`a1`）: +0h=10、+30m=20、+1h=30、+2h=40、**+3h ちょうど=999**、
/// **+4h ちょうど=+∞**。グループ B（`b1`）は +1h に 5 だけ。グループ S（シミュ）は無記録。
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
    for (offset, value) in [
        (0, 10.0),
        (30 * 60_000, 20.0),
        (H, 30.0),
        (2 * H, 40.0),
        (3 * H, 999.0),
        (4 * H, f64::INFINITY),
    ] {
        writer
            .append(&format!("grp:{grp_a}"), BASE_MS + offset, &[Some(value)])
            .await
            .unwrap();
    }
    writer
        .append(&format!("grp:{grp_b}"), BASE_MS + H, &[Some(5.0)])
        .await
        .unwrap();
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

fn st(min: f64, max: f64, avg: f64, count: i64) -> Option<DailyReportStat> {
    Some(DailyReportStat {
        min,
        max,
        avg,
        count,
    })
}

async fn ready(svc: &CollectorService, request: &DailyReportRequest) -> CollectDailyReport {
    match svc.daily_report(request).await.expect("検証エラーではない") {
        Readout::Ready { data } => data,
        other => panic!("ready を期待したが {other:?}"),
    }
}

/// 3 時間の「日」`[BASE, BASE+3h)`: 時間ごとの統計量と日の合計。`toMs` ちょうどの
/// サンプル（999）は合計にも 4 本目の区間にも入らない。B（別グループ）・
/// シミュレーション・居ないタグもそれぞれ期待どおり。
///
/// 反証: tsquery に `toMs` をそのまま渡すと、999 が a1 の max に入り、区間も
/// 4 本になって落ちる。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hours_and_day_exclude_the_end_instant() {
    let f = fixture().await;
    let unknown = 999_999;
    let request =
        validate_daily_report_request(&[f.b1, unknown, f.a1, f.s1], BASE_MS, BASE_MS + 3 * H)
            .unwrap();
    let data = ready(&f.svc, &request).await;

    assert_eq!(data.interval_ms, H);
    assert_eq!(data.unknown_tag_ids, vec![unknown]);
    let cols: Vec<(i64, bool)> = data.tags.iter().map(|t| (t.tag_id, t.simulation)).collect();
    assert_eq!(cols, vec![(f.b1, false), (f.a1, false), (f.s1, true)]);

    assert_eq!(data.hours.len(), 3, "toMs ちょうどの時刻の 4 本目は出ない");
    for (i, hour) in data.hours.iter().enumerate() {
        assert_eq!(hour.t_ms, BASE_MS + i as i64 * H);
    }
    assert_eq!(
        data.hours[0].values,
        vec![None, st(10.0, 20.0, 15.0, 2), None]
    );
    assert_eq!(
        data.hours[1].values,
        vec![st(5.0, 5.0, 5.0, 1), st(30.0, 30.0, 30.0, 1), None]
    );
    assert_eq!(
        data.hours[2].values,
        vec![None, st(40.0, 40.0, 40.0, 1), None]
    );

    // 日の合計: 999（toMs ちょうど）は入らない。
    assert_eq!(
        data.day,
        vec![st(5.0, 5.0, 5.0, 1), st(10.0, 40.0, 25.0, 4), None]
    );
    f.pool.close().await;
}

/// 日の合計は時間の行と同じ 1 回の読み出しから組むので、必ず一致する:
/// 件数は時間の件数の和、最小・最大は時間の最小の最小・最大の最大、平均は件数の重みつき。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn day_totals_agree_with_hour_rows() {
    let f = fixture().await;
    let request = validate_daily_report_request(&[f.a1, f.b1], BASE_MS, BASE_MS + 3 * H).unwrap();
    let data = ready(&f.svc, &request).await;
    for (column, day) in data.day.iter().enumerate() {
        let hours: Vec<DailyReportStat> =
            data.hours.iter().filter_map(|h| h.values[column]).collect();
        let day = day.expect("サンプルがある");
        assert_eq!(day.count, hours.iter().map(|h| h.count).sum::<i64>());
        assert_eq!(
            day.min,
            hours.iter().map(|h| h.min).fold(f64::INFINITY, f64::min)
        );
        assert_eq!(
            day.max,
            hours
                .iter()
                .map(|h| h.max)
                .fold(f64::NEG_INFINITY, f64::max)
        );
        let weighted = hours.iter().map(|h| h.avg * h.count as f64).sum::<f64>() / day.count as f64;
        assert!(
            (day.avg - weighted).abs() < 1e-9,
            "{} vs {weighted}",
            day.avg
        );
    }
    // a1 は 3 つの時間にまたがる（10,20 / 30 / 40）。
    assert_eq!(data.day[0], st(10.0, 40.0, 25.0, 4));
    f.pool.close().await;
}

/// 非有限は欠測と同じ `null`: +∞ を含む区間と、それを含む日の合計。+∞ が
/// `toMs` ちょうどなら入らない（4 時間の日は有限、5 時間の日は `null`）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn non_finite_is_null_and_end_instant_is_excluded() {
    let f = fixture().await;
    let four = validate_daily_report_request(&[f.a1], BASE_MS, BASE_MS + 4 * H).unwrap();
    let data = ready(&f.svc, &four).await;
    assert_eq!(data.hours.len(), 4);
    assert_eq!(data.hours[3].values, vec![st(999.0, 999.0, 999.0, 1)]);
    assert_eq!(
        data.day,
        vec![st(
            10.0,
            999.0,
            (10.0 + 20.0 + 30.0 + 40.0 + 999.0) / 5.0,
            5
        )]
    );

    let five = validate_daily_report_request(&[f.a1], BASE_MS, BASE_MS + 5 * H).unwrap();
    let data = ready(&f.svc, &five).await;
    assert_eq!(data.hours.len(), 5);
    assert_eq!(data.hours[4].values, vec![None], "+∞ だけの区間は null");
    assert_eq!(data.day, vec![None], "+∞ を含む合計は null");
    f.pool.close().await;
}

/// 23 時間・25 時間・半端な幅: 区間は `ceil(幅 / 1 時間)` 本。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bin_count_follows_the_window() {
    let f = fixture().await;
    for (width, bins) in [
        (23 * H, 23),
        (25 * H, 25),
        (H, 1),
        (H + 1, 2),
        (90 * 60_000, 2),
    ] {
        let request = validate_daily_report_request(&[f.a1], BASE_MS, BASE_MS + width).unwrap();
        let data = ready(&f.svc, &request).await;
        assert_eq!(data.hours.len(), bins, "幅 {width}");
    }
    f.pool.close().await;
}

/// データファイルが無い・タグが全部居ない: `ready`（時間の行は全部 `null`）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_data_is_ready_with_null_rows() {
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
    let request = validate_daily_report_request(&[t], BASE_MS, BASE_MS + 24 * H).unwrap();
    let data = ready(&svc, &request).await;
    assert_eq!(data.hours.len(), 24);
    assert!(data.hours.iter().all(|h| h.values == vec![None]));
    assert_eq!(data.day, vec![None]);

    let unknown = validate_daily_report_request(&[424_242], BASE_MS, BASE_MS + 24 * H).unwrap();
    let data = ready(&svc, &unknown).await;
    assert!(data.tags.is_empty() && data.day.is_empty());
    assert_eq!(data.hours.len(), 24);
    assert!(data.hours.iter().all(|h| h.values.is_empty()));
    assert_eq!(data.unknown_tag_ids, vec![424_242]);
    pool.close().await;
}
