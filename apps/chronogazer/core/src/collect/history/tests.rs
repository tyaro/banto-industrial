//! 履歴の読み出し（D-3a）のテスト。データファイルは `banto-tstore` の
//! `TsWriter` + `ManualClock` で作る（手書きの SQL にしない - 本物の収集が
//! 書くのと同じ形を読むため。`banto-tsquery` の `tests/common` と同じ作法）。

use std::sync::Arc;

use banto_collect::CollectorOptions;
use banto_tags::{
    CollectionGroupInput, CollectionGroupService, PlcConnectionInput, PlcConnectionService,
    TagInput, TagService,
};
use banto_tstore::{GroupConfig, ManualClock, StoreConfig, TagColumn, TsWriter};

use super::*;
use crate::collect::{CollectorState, StartGate};
use crate::db::init_db_memory;
use crate::test_support::TempDir;

/// 2026-07-12T00:00:00Z（`banto-tsquery` のテストと同じ基準日）+ 3 時間。
/// ローカル日付（+09:00）でも同じ日に収まる。
const BASE_MS: i64 = 20_646 * 86_400_000 + 3 * 3_600_000;
const OFFSET_MS: i64 = 9 * 3_600_000;

fn field_names(err: BantoError) -> Vec<String> {
    match err {
        BantoError::Validation { field_errors } => {
            field_errors.into_iter().map(|e| e.field).collect()
        }
        other => panic!("Validation を期待したが {other:?}"),
    }
}

// --- 要求の検証（上限） -------------------------------------------------------

/// 上限ちょうどは通り、1 つでも超えたら該当フィールドの誤りになる。
///
/// 反証: `HISTORY_MAX_WINDOW_MS` の比較を `>=` にすると「ちょうど 1 時間」が
/// 落ちる。重複除去を外すと「同じ ID を 9 回」が `tagIds` の誤りになって落ちる。
#[test]
fn request_limits_accept_the_boundary_and_reject_one_beyond() {
    let eight: Vec<i64> = (1..=8).collect();
    let ok = validate_history_request(&eight, 0, HISTORY_MAX_WINDOW_MS, 600)
        .expect("8 本・ちょうど 1 時間は通る");
    assert_eq!(ok.tag_ids(), eight.as_slice());

    // 重複は 1 本に数え、最初に現れた順を保つ。
    let dup = validate_history_request(&[3, 1, 3, 3, 1, 3, 1, 3, 3], 0, 1, 1)
        .expect("重複は 1 本に数える");
    assert_eq!(dup.tag_ids(), &[3, 1]);

    validate_history_request(&[1], 0, 1, HISTORY_MAX_BINS as u64)
        .expect("bins = 上限ちょうどは通る");

    let nine: Vec<i64> = (1..=9).collect();
    assert_eq!(
        field_names(validate_history_request(&nine, 0, 1, 1).unwrap_err()),
        vec!["tagIds"]
    );
    assert_eq!(
        field_names(validate_history_request(&[], 0, 1, 1).unwrap_err()),
        vec!["tagIds"]
    );
    assert_eq!(
        field_names(validate_history_request(&[1], 0, HISTORY_MAX_WINDOW_MS + 1, 1).unwrap_err()),
        vec!["toMs"],
        "1 時間を 1ms でも超えたら拒否する"
    );
    assert_eq!(
        field_names(validate_history_request(&[1], 5, 5, 1).unwrap_err()),
        vec!["toMs"],
        "from == to は拒否する"
    );
    assert_eq!(
        field_names(validate_history_request(&[1], 6, 5, 1).unwrap_err()),
        vec!["toMs"]
    );
    assert_eq!(
        field_names(validate_history_request(&[1], 0, 1, 0).unwrap_err()),
        vec!["bins"]
    );
    assert_eq!(
        field_names(validate_history_request(&[1], 0, 1, HISTORY_MAX_BINS as u64 + 1).unwrap_err()),
        vec!["bins"]
    );
    // 誤りはまとめて返す。
    assert_eq!(
        field_names(validate_history_request(&[], 1, 0, 0).unwrap_err()),
        vec!["tagIds", "toMs", "bins"]
    );
    // 極端な端でも幅の計算が溢れない（i128 で測る）。
    assert_eq!(
        field_names(validate_history_request(&[1], i64::MIN, i64::MAX, 1).unwrap_err()),
        vec!["toMs"]
    );
    assert_eq!(
        field_names(validate_history_request(&[1], i64::MAX, i64::MIN, 1).unwrap_err()),
        vec!["toMs"]
    );
}

#[test]
fn tag_ids_are_parsed_from_a_comma_separated_list() {
    assert_eq!(parse_tag_ids("1,2, 3").unwrap(), vec![1, 2, 3]);
    assert_eq!(parse_tag_ids("  ").unwrap(), Vec::<i64>::new());
    assert_eq!(
        field_names(parse_tag_ids("1,,2").unwrap_err()),
        vec!["tagIds"]
    );
    assert_eq!(
        field_names(parse_tag_ids("1,a").unwrap_err()),
        vec!["tagIds"]
    );
}

// --- 公開する形への変換 -------------------------------------------------------

/// 欠測（`Gap`）も非有限（NaN・±∞。包絡の片端だけでも）も、点ごと `null`。
///
/// 反証: `point_from_bin` の `is_finite()` の条件を外すと、∞ がそのまま
/// `Some` で残って落ちる（JSON では `null` になり、欠測と見分けが付かない）。
#[test]
fn gaps_and_non_finite_envelopes_become_null() {
    let null = |t_ms| HistoryPoint {
        t_ms,
        min: None,
        max: None,
    };
    assert_eq!(point_from_bin(1, BinValue::Gap), null(1));
    assert_eq!(
        point_from_bin(
            2,
            BinValue::Range {
                min: f64::NAN,
                max: 1.0
            }
        ),
        null(2)
    );
    assert_eq!(
        point_from_bin(
            3,
            BinValue::Range {
                min: 0.0,
                max: f64::INFINITY
            }
        ),
        null(3)
    );
    assert_eq!(
        point_from_bin(
            4,
            BinValue::Range {
                min: f64::NEG_INFINITY,
                max: f64::NEG_INFINITY
            }
        ),
        null(4)
    );
    assert_eq!(
        point_from_bin(5, BinValue::Range { min: 0.0, max: 0.0 }),
        HistoryPoint {
            t_ms: 5,
            min: Some(0.0),
            max: Some(0.0)
        },
        "0 は値として残る（null に潰さない）"
    );
}

/// ワイヤの綴り（`Readout` の判別共用体 + camelCase）。TS の型
/// （`collectAdmin.ts` の `CollectHistory`）と対。
#[test]
fn the_wire_shape_is_camel_case_inside_a_readout() {
    let readout = Readout::Ready {
        data: CollectHistory {
            from_ms: 1,
            to_ms: 2,
            series: vec![HistorySeries {
                tag_id: 7,
                simulation: true,
                bin_ms: 1000,
                points: vec![HistoryPoint {
                    t_ms: 1,
                    min: None,
                    max: None,
                }],
            }],
            unknown_tag_ids: vec![9],
        },
    };
    assert_eq!(
        serde_json::to_value(&readout).unwrap(),
        serde_json::json!({
            "state": "ready",
            "data": {
                "fromMs": 1,
                "toMs": 2,
                "series": [{
                    "tagId": 7,
                    "simulation": true,
                    "binMs": 1000,
                    "points": [{ "tMs": 1, "min": null, "max": null }]
                }],
                "unknownTagIds": [9]
            }
        })
    );
}

// --- データファイルを読む ---------------------------------------------------

async fn connection(pool: &SqlitePool, name: &str, simulation: bool) -> i64 {
    PlcConnectionService::new(pool.clone())
        .create(PlcConnectionInput {
            name: name.to_string(),
            protocol: "modbus-tcp".to_string(),
            host: "127.0.0.1".to_string(),
            port: 502,
            unit_id: 1,
            enabled: true,
            simulation,
            word_order: String::new(),
            database: None,
            username: None,
            password: None,
        })
        .await
        .expect("PLC 接続を登録できる")
        .id
}

async fn group(pool: &SqlitePool, name: &str, conn_id: i64, period_ms: i64) -> i64 {
    CollectionGroupService::new(pool.clone())
        .create(CollectionGroupInput {
            name: name.to_string(),
            plc_connection_id: conn_id,
            period_ms,
            enabled: true,
            default_writable: false,
            query_sql: None,
        })
        .await
        .expect("収集グループを登録できる")
        .id
}

async fn tag(pool: &SqlitePool, name: &str, group_id: i64) -> i64 {
    TagService::new(pool.clone())
        .create(TagInput {
            name: name.to_string(),
            collection_group_id: group_id,
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
        .expect("タグを登録できる")
        .id
}

fn column(tag_id: i64) -> TagColumn {
    TagColumn {
        key: format!("tag:{tag_id}"),
        name: format!("tag {tag_id}"),
        data_type: "f32".to_string(),
        unit: None,
        decimals: 0,
    }
}

fn group_config(group_id: i64, period_ms: u32, tag_ids: &[i64]) -> GroupConfig {
    GroupConfig {
        key: format!("grp:{group_id}"),
        name: format!("group {group_id}"),
        period_ms,
        tags: tag_ids.iter().map(|id| column(*id)).collect(),
    }
}

fn values_of(series: &HistorySeries) -> Vec<(i64, Option<f64>)> {
    series
        .points
        .iter()
        .map(|p| {
            assert_eq!(p.min, p.max, "1 区間 1 サンプルなので min == max");
            (p.t_ms, p.min)
        })
        .collect()
}

/// 2 つの収集グループにまたがる要求・外したタグ・シミュレーション接続の
/// タグ・レジストリに居ないタグ・非有限の値を、1 つのデータディレクトリで
/// まとめて読む。
///
/// * グループ A（周期 1s）: `a1`（値 k）、`a2`（値 2k。k = 10 だけ +∞）。
///   `a_excluded` はレジストリにだけ居て、データファイルに列が無い
///   （= 開始時に外したタグ、#414 段階2）。
/// * グループ B（周期 2s）: `b1`（値 100 + k）。
/// * グループ S（シミュレーション接続）: `s1`。値は記録されない。
///
/// 反証: グループごとに束ねずに 1 つ目のグループのキーで全部読むと、`b1` が
/// 全部 `null` になって落ちる。要求順に並べ直さないと `series` の順が
/// グループ順（a1, a2, b1）になって落ちる。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tags_across_groups_are_merged_in_request_order_with_nulls_where_nothing_was_recorded() {
    let dir = TempDir::new();
    let data_dir = dir.path().join("data");
    let pool = init_db_memory().await.expect("init_db_memory");

    let real = connection(&pool, "real", false).await;
    let sim = connection(&pool, "sim", true).await;
    let grp_a = group(&pool, "A", real, 1000).await;
    let grp_b = group(&pool, "B", real, 2000).await;
    let grp_s = group(&pool, "S", sim, 1000).await;
    let a1 = tag(&pool, "a1", grp_a).await;
    let a2 = tag(&pool, "a2", grp_a).await;
    let a_excluded = tag(&pool, "a_excluded", grp_a).await;
    let b1 = tag(&pool, "b1", grp_b).await;
    let s1 = tag(&pool, "s1", grp_s).await;

    let clock = Arc::new(ManualClock::new(BASE_MS, OFFSET_MS));
    let writer = TsWriter::open(
        &data_dir,
        StoreConfig {
            groups: vec![
                group_config(grp_a, 1000, &[a1, a2]),
                group_config(grp_b, 2000, &[b1]),
            ],
        },
        clock,
    )
    .await
    .expect("writer を開ける");
    for k in 0..60_i64 {
        let t = BASE_MS + k * 1000;
        let a2_value = if k == 10 {
            f64::INFINITY
        } else {
            2.0 * k as f64
        };
        writer
            .append(
                &format!("grp:{grp_a}"),
                t,
                &[Some(k as f64), Some(a2_value)],
            )
            .await
            .expect("A に書ける");
        if k % 2 == 0 {
            writer
                .append(&format!("grp:{grp_b}"), t, &[Some(100.0 + k as f64)])
                .await
                .expect("B に書ける");
        }
    }
    writer.close().await.expect("writer を閉じる");

    let svc = CollectorService::new(pool.clone(), data_dir);
    let unknown = 999_999;
    let request = validate_history_request(
        &[b1, a_excluded, a1, unknown, s1, a2],
        BASE_MS,
        BASE_MS + 59_999,
        60,
    )
    .expect("検証を通る");
    let Readout::Ready { data } = svc.history(&request).await else {
        panic!("読めるはず");
    };

    assert_eq!(data.from_ms, BASE_MS);
    assert_eq!(data.to_ms, BASE_MS + 59_999);
    assert_eq!(data.unknown_tag_ids, vec![unknown]);
    let order: Vec<i64> = data.series.iter().map(|s| s.tag_id).collect();
    assert_eq!(order, vec![b1, a_excluded, a1, s1, a2], "要求順に並ぶ");
    let by_id = |id: i64| data.series.iter().find(|s| s.tag_id == id).unwrap();

    // グループ A: 60 サンプル（周期 1s）。
    let a1_series = by_id(a1);
    assert_eq!(a1_series.bin_ms, 1000);
    assert!(!a1_series.simulation);
    let expected_a1: Vec<(i64, Option<f64>)> = (0..60_i64)
        .map(|k| (BASE_MS + k * 1000, Some(k as f64)))
        .collect();
    assert_eq!(values_of(a1_series), expected_a1);

    // +∞ を記録した 1 点だけ null、他は値のまま。
    let a2_values = values_of(by_id(a2));
    assert_eq!(a2_values.len(), 60);
    for (k, (t, v)) in a2_values.iter().enumerate() {
        assert_eq!(*t, BASE_MS + k as i64 * 1000);
        if k == 10 {
            assert_eq!(*v, None, "非有限の値が null に畳まれていない");
        } else {
            assert_eq!(*v, Some(2.0 * k as f64));
        }
    }

    // グループ B: 30 サンプル（周期 2s）、区間の幅も 2s。
    let b1_series = by_id(b1);
    assert_eq!(b1_series.bin_ms, 2000);
    let expected_b1: Vec<(i64, Option<f64>)> = (0..30_i64)
        .map(|i| (BASE_MS + i * 2000, Some(100.0 + 2.0 * i as f64)))
        .collect();
    assert_eq!(values_of(b1_series), expected_b1);

    // 外したタグ: 系列はあり（居ないタグではない）、値は全部 null。
    let excluded = by_id(a_excluded);
    assert!(!excluded.points.is_empty());
    assert!(
        excluded
            .points
            .iter()
            .all(|p| p.min.is_none() && p.max.is_none()),
        "{:?}",
        excluded.points
    );

    // シミュレーション: 印が付き、値は全部 null（記録されない）。
    let s1_series = by_id(s1);
    assert!(s1_series.simulation);
    assert!(!s1_series.points.is_empty());
    assert!(s1_series
        .points
        .iter()
        .all(|p| p.min.is_none() && p.max.is_none()));

    pool.close().await;
}

/// データファイルがまだ 1 つも無い（収集したことが無い）のは「読めない」では
/// なく、**読めて値が無い**。収集が走っていなくても `notRunning` にしない。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_data_files_yet_is_ready_with_nulls_not_unavailable() {
    let dir = TempDir::new();
    let pool = init_db_memory().await.expect("init_db_memory");
    let conn = connection(&pool, "real", false).await;
    let grp = group(&pool, "A", conn, 1000).await;
    let t = tag(&pool, "t", grp).await;
    let svc = CollectorService::new(pool.clone(), dir.path().join("never-created"));
    assert_eq!(svc.state(), CollectorState::Stopped);

    let request = validate_history_request(&[t], BASE_MS, BASE_MS + 9_999, 10).unwrap();
    let Readout::Ready { data } = svc.history(&request).await else {
        panic!("ファイルが無いだけで unavailable にしている");
    };
    assert_eq!(data.series.len(), 1);
    assert!(data.series[0]
        .points
        .iter()
        .all(|p| p.min.is_none() && p.max.is_none()));
    pool.close().await;
}

/// データファイルが壊れている・レジストリが読めないときは `unavailable`。
/// 公開する形には理由（ファイルのパスを含みうる）を載せない。
///
/// 反証: `history` の `Ok(Err(_))` 分岐を `Ready`（空の系列）にすると、
/// 「読めなかった」が「値が無い」に潰れて落ちる。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unreadable_store_or_registry_is_unavailable_without_a_reason() {
    let dir = TempDir::new();
    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    // 名前だけ正しい、中身が SQLite ではないデータファイル。
    std::fs::write(
        data_dir.join("20260712-001.sqlite3"),
        b"this is not a sqlite database, just garbage bytes for the test",
    )
    .unwrap();

    let pool = init_db_memory().await.expect("init_db_memory");
    let conn = connection(&pool, "real", false).await;
    let grp = group(&pool, "A", conn, 1000).await;
    let t = tag(&pool, "t", grp).await;
    let svc = CollectorService::new(pool.clone(), data_dir);
    let request = validate_history_request(&[t], BASE_MS, BASE_MS + 9_999, 10).unwrap();

    let readout = svc.history(&request).await;
    assert_eq!(readout, Readout::Unavailable);
    assert_eq!(
        serde_json::to_value(&readout).unwrap(),
        serde_json::json!({ "state": "unavailable" }),
        "理由やパスを公開する形に載せている"
    );

    // レジストリ（設定 DB）が読めない。
    pool.close().await;
    assert_eq!(svc.history(&request).await, Readout::Unavailable);
}

/// **操作キューを通らない**（docs/implementation-checklist.md §5）: 収集の
/// 起動処理が止まったまま（ライフサイクルタスクが応答しない）でも、履歴は
/// すぐ読める。
///
/// 反証: `history` の中で `self.connection_status().await`（読み取りキュー）や
/// `self.state()` 以外のライフサイクルへの依頼を挟むと、ここで上限を過ぎて
/// 落ちる。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn history_does_not_wait_for_a_stuck_lifecycle_task() {
    let dir = TempDir::new();
    let pool = init_db_memory().await.expect("init_db_memory");
    let conn = connection(&pool, "real", false).await;
    let grp = group(&pool, "A", conn, 1000).await;
    let t = tag(&pool, "t", grp).await;
    let gate = Arc::new(StartGate::default());
    let svc = CollectorService::new_for_test_gated(
        pool.clone(),
        dir.path().join("data"),
        CollectorOptions::default(),
        banto_collect::default_client_factory(),
        gate.clone(),
    );

    let starter = {
        let svc = svc.clone();
        tokio::spawn(async move { svc.start().await })
    };
    gate.entered.notified().await;
    assert_eq!(svc.state(), CollectorState::Starting);

    let request = validate_history_request(&[t], BASE_MS, BASE_MS + 9_999, 10).unwrap();
    let readout = tokio::time::timeout(Duration::from_secs(5), svc.history(&request))
        .await
        .expect("起動処理の後ろで待たされている");
    assert_eq!(readout.as_str(), "ready");

    gate.release.notify_one();
    let _ = starter.await.expect("start task");
    let _ = svc.stop().await;
    pool.close().await;
}
