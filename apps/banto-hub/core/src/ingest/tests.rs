//! R2a-1c の完了の確かめ（docs/banto-hub-ingest-design.md §11 の R2a-1c の行）。
//!
//! 前半は純関数の表（Live の判定・§4.5 の表・commit の表・件ごとの検証）。後半は
//! `IngestHub::apply` を直接呼ぶ統合テスト: 本物の `CollectorManager` と
//! `CollectionController`（`HubDrivers` はテスト用の空の表。`push` はドライバを
//! 使わない）で収集を開始・停止し、現在値の読み出し（`hub::read_current_view`）と
//! tstore のデータファイルの行で確かめる。送信口（REST・WS）は R2a-2c / R2a-2d。
//!
//! 時間に依存するテストは周期 100 ms のグループと、Live の判定に実時間
//! （`SystemClock`）を使う。待ちは `wait_until` の期限付きの条件待ちで、固定の
//! sleep は「この間に何が記録されたか」を見るときだけ使う。

use super::*;

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use banto_collect::{CollectorOptions, EventKind, Quality};
use banto_tags::{
    CollectionGroupInput, CollectionGroupService, PlcConnectionInput, PlcConnectionService,
    TagInput, TagService,
};
use banto_tstore::{ManualClock, SystemClock, TsReader, WriterOptions};
use serde_json::json;

use crate::broker_glue::{BrokerSimRegistry, HubSessions};
use crate::computed::{ComputedEngine, ServerTagStore};
use crate::controller::{CollectionController, CollectionState, CollectionStatus, RunMode};
use crate::hub::CollectorManager;

// --- 純関数の表 ------------------------------------------------------------

/// §4.3: `now - last_seen <= stale_timeout` で Live。境界ちょうどは Live、1 ms 超で
/// Down。一度も通信が無ければ Down（理由つき）。時計が戻っても Live。
#[test]
fn the_live_determination_table() {
    let live = ExternalLink::Live;
    let rows: Vec<(Option<i64>, u64, i64, bool)> = vec![
        // (last_seen_ms, stale_timeout_ms, now_ms, live)
        (None, 10_000, 0, false),
        (None, 10_000, 1_000_000, false),
        (Some(1_000), 10_000, 1_000, true),
        (Some(1_000), 10_000, 11_000, true),
        (Some(1_000), 10_000, 11_001, false),
        (Some(1_000), 1_000, 2_000, true),
        (Some(1_000), 1_000, 2_001, false),
        (Some(5_000), 1_000, 4_000, true), // 時計が戻った
        (Some(0), 3_600_000, 3_600_000, true),
        (Some(0), 3_600_000, 3_600_001, false),
    ];
    for (last_seen, timeout, now, expect_live) in rows {
        let state = link_state(last_seen, timeout, now);
        assert_eq!(
            state == live,
            expect_live,
            "{last_seen:?} {timeout} {now}: {state:?}"
        );
    }
    assert_eq!(
        link_state(None, 10_000, 0),
        ExternalLink::Down {
            detail: Some("送り手が一度も接続していません".to_string())
        }
    );
    assert_eq!(
        link_state(Some(0), 10_000, 12_500),
        ExternalLink::Down {
            detail: Some("送り手からの通信が 12 秒ありません".to_string())
        }
    );
}

/// §4.5 の表（停止・開始・開始の確定 × 一般の送り手・ブリッジ）。
#[test]
fn the_running_stopped_table() {
    use CollectionTransition as T;
    use SenderKind as K;
    let effect = |discard_held, advance_epoch, notify| HeldEffect {
        discard_held,
        advance_epoch,
        notify,
    };
    let rows = [
        (T::Stopped, K::General, effect(true, false, true)),
        (T::Stopped, K::Bridge, effect(true, true, true)),
        (T::Starting, K::General, effect(false, false, false)),
        (T::Starting, K::Bridge, effect(true, true, false)),
        (T::Started, K::General, effect(false, false, true)),
        (T::Started, K::Bridge, effect(false, false, true)),
    ];
    for (transition, kind, expected) in rows {
        assert_eq!(
            transition_effect(kind, transition),
            expected,
            "{transition:?} {kind:?}"
        );
    }
}

/// §4.6: commit での設定の変更の表。
#[test]
fn the_commit_effect_table() {
    let a = json!({ "kind": "db", "queries": [1] });
    let b = json!({ "kind": "db", "queries": [2] });
    let none = HeldEffect {
        discard_held: false,
        advance_epoch: false,
        notify: false,
    };
    let reset = HeldEffect {
        discard_held: true,
        advance_epoch: true,
        notify: true,
    };
    let discard_only = HeldEffect {
        discard_held: true,
        advance_epoch: false,
        notify: false,
    };
    let rows: Vec<(Option<&Value>, Option<&Value>, bool, HeldEffect)> = vec![
        (None, None, false, none),
        (Some(&a), Some(&a), false, none),
        (Some(&a), Some(&b), false, reset),
        (None, Some(&a), false, reset),
        (Some(&a), None, false, reset),
        (None, None, true, discard_only),
        (Some(&a), Some(&a), true, reset),
    ];
    for (old, new, enabled_changed, expected) in rows {
        assert_eq!(
            commit_effect(old, new, enabled_changed),
            expected,
            "{old:?} {new:?} {enabled_changed}"
        );
    }
}

/// §6.3 の値と品質の検証（キーの照合の後）。
#[test]
fn the_item_validation_table() {
    let good = |v: f64| {
        Ok(ExternalReading {
            value: Some(v),
            quality: ExternalQuality::Good,
        })
    };
    let item = |v: Value, q: Option<Value>, t: Option<Value>| IngestItem {
        key: "k".to_string(),
        v,
        q,
        t,
    };
    let s = |text: &str| Some(Value::String(text.to_string()));
    let rows: Vec<(IngestItem, &str, Result<ExternalReading, RejectCode>)> = vec![
        (item(json!(12.5), None, None), "f32", good(12.5)),
        (item(json!(70000), s("good"), None), "i16", good(70000.0)),
        (item(json!(true), None, None), "bit", good(1.0)),
        (item(json!(false), None, None), "f64", good(0.0)),
        (item(json!(1), None, None), "bit", good(1.0)),
        (
            item(json!(2), None, None),
            "bit",
            Err(RejectCode::InvalidValue),
        ),
        (
            item(json!(0.5), None, None),
            "bit",
            Err(RejectCode::InvalidValue),
        ),
        (
            item(Value::Null, None, None),
            "f32",
            Err(RejectCode::InvalidValue),
        ),
        (
            item(json!("12"), None, None),
            "f32",
            Err(RejectCode::InvalidValue),
        ),
        (
            item(json!([1]), None, None),
            "f32",
            Err(RejectCode::InvalidValue),
        ),
        (
            item(json!(4.0), s("stale"), None),
            "f32",
            Ok(ExternalReading {
                value: Some(4.0),
                quality: ExternalQuality::Stale,
            }),
        ),
        (
            item(Value::Null, s("stale"), None),
            "f32",
            Ok(ExternalReading {
                value: None,
                quality: ExternalQuality::Stale,
            }),
        ),
        (
            item(json!(5.0), s("bad"), None),
            "f32",
            Ok(ExternalReading {
                value: None,
                quality: ExternalQuality::Bad,
            }),
        ),
        (
            item(json!(5.0), s("uncertain"), None),
            "f32",
            Err(RejectCode::InvalidQuality),
        ),
        (
            item(json!(5.0), Some(json!(1)), None),
            "f32",
            Err(RejectCode::InvalidQuality),
        ),
        (
            item(json!(5.0), None, Some(json!(1_761_000_000_000_i64))),
            "f32",
            good(5.0),
        ),
        (
            item(json!(5.0), None, Some(json!(1.5))),
            "f32",
            Err(RejectCode::InvalidTime),
        ),
        (
            item(json!(5.0), None, Some(json!("now"))),
            "f32",
            Err(RejectCode::InvalidTime),
        ),
    ];
    for (input, data_type, expected) in rows {
        assert_eq!(
            validate_item(&input, data_type),
            expected,
            "{input:?} {data_type}"
        );
    }
}

/// `q` / `t` の明示的な `null` は省略と区別する（JSON から読んだ形で）。
#[test]
fn explicit_null_quality_and_time_are_rejected_but_omitted_are_accepted() {
    let judge = |text: &str| {
        let item: IngestItem = serde_json::from_str(text).expect("item");
        validate_item(&item, "f64")
    };
    assert_eq!(
        judge(r#"{"key":"k","v":12.5,"q":null}"#),
        Err(RejectCode::InvalidQuality)
    );
    assert_eq!(
        judge(r#"{"key":"k","v":12.5,"t":null}"#),
        Err(RejectCode::InvalidTime)
    );
    assert_eq!(
        judge(r#"{"key":"k","v":12.5}"#),
        Ok(ExternalReading {
            value: Some(12.5),
            quality: ExternalQuality::Good
        })
    );
}

#[test]
fn connection_keys_parse_only_the_conn_form() {
    assert_eq!(parse_connection_key("conn:12"), Some(12));
    assert_eq!(parse_connection_key("conn:x"), None);
    assert_eq!(parse_connection_key("grp:12"), None);
    assert_eq!(parse_connection_key("12"), None);
}

// --- 統合テストの足場 --------------------------------------------------------

struct Env {
    // 宣言順に drop される: プールを閉じる前にディレクトリを消さない
    // （`crate::test_support` のモジュール doc）。
    manager: Arc<CollectorManager>,
    controller: Arc<CollectionController>,
    ingest: Arc<IngestHub>,
    pool: sqlx::SqlitePool,
    data_dir: std::path::PathBuf,
    _dir: crate::test_support::TempDir,
}

async fn env(label: &str) -> Env {
    let dir = crate::test_support::TempDir::new(label);
    let pool =
        crate::test_support::init_db_from_template(&dir.path().join("registry.sqlite3")).await;
    let data_dir = dir.path().join("data");
    let manager = Arc::new(CollectorManager::new(
        pool.clone(),
        data_dir.clone(),
        Arc::new(SystemClock),
        CollectorOptions {
            connect_timeout: Duration::from_millis(50),
            response_timeout: Duration::from_millis(50),
            writer_options: WriterOptions {
                max_buffered_rows: 500,
                flush_interval_ms: 100,
            },
            ..CollectorOptions::default()
        },
        Arc::new(HubSessions::new(
            banto_broker::BackoffConfig::default(),
            crate::test_support::test_drivers(),
        )),
        Arc::new(BrokerSimRegistry::new()),
        Arc::new(ComputedEngine::new(Arc::new(ServerTagStore::new()))),
    ));
    let controller = Arc::new(CollectionController::new(manager.clone()));
    let ingest = manager.ingest_hub();
    Env {
        manager,
        controller,
        ingest,
        pool,
        data_dir,
        _dir: dir,
    }
}

impl Env {
    fn now_ms(&self) -> i64 {
        self.manager.clock().now_ms()
    }

    /// 保存の後の本番の経路（CRUD → `commit_catalog_and_apply_live`）。
    async fn commit(&self) {
        self.controller
            .commit_catalog_and_apply_live()
            .await
            .expect("commit");
    }

    async fn start(&self) {
        let status = self
            .controller
            .start(RunMode::Configured)
            .await
            .expect("start");
        assert_eq!(status.state, CollectionState::Running, "{status:?}");
    }

    async fn stop(&self) {
        let status = self.controller.stop().await;
        assert_eq!(status.state, CollectionState::Stopped, "{status:?}");
    }

    async fn finish(self) {
        self.controller.stop().await;
        self.manager.shutdown().await;
        self.pool.close().await;
    }

    async fn push_connection(&self, name: &str, settings: Option<Value>) -> i64 {
        let input: PlcConnectionInput = serde_json::from_value(json!({
            "name": name, "protocol": "push", "host": "", "port": 0, "settings": settings,
        }))
        .unwrap();
        PlcConnectionService::new(self.pool.clone())
            .create(input)
            .await
            .expect("push connection")
            .id
    }

    async fn postgres_connection(&self, name: &str) -> i64 {
        let input: PlcConnectionInput = serde_json::from_value(json!({
            "name": name, "protocol": "postgres", "host": "127.0.0.1", "port": 5432,
            "database": "plant", "username": "reader", "password": "x",
        }))
        .unwrap();
        PlcConnectionService::new(self.pool.clone())
            .create(input)
            .await
            .expect("postgres connection")
            .id
    }

    async fn group(&self, connection_id: i64, name: &str, period_ms: i64) -> i64 {
        let input: CollectionGroupInput = serde_json::from_value(json!({
            "name": name, "plc_connection_id": connection_id, "period_ms": period_ms,
        }))
        .unwrap();
        CollectionGroupService::new(self.pool.clone())
            .create(input)
            .await
            .expect("group")
            .id
    }

    async fn tag(&self, group_id: i64, name: &str, key: &str, data_type: &str) -> i64 {
        let input: TagInput = serde_json::from_value(json!({
            "name": name, "collection_group_id": group_id, "address": key, "data_type": data_type,
        }))
        .unwrap();
        TagService::new(self.pool.clone())
            .create(input)
            .await
            .expect("tag")
            .id
    }

    async fn sql(&self, statement: &str) {
        sqlx::query(sqlx::AssertSqlSafe(statement.to_string()))
            .execute(&self.pool)
            .await
            .expect(statement);
    }

    /// 読み出しの統一点（`hub::read_current_view`）で見たタグの値と品質。
    fn current(&self, tag_id: i64) -> (Option<f64>, Quality) {
        let map = self.manager.tag_map();
        let entry = map
            .iter()
            .find(|entry| entry.ids.2 == tag_id)
            .expect("tag in catalog");
        let view = crate::hub::read_current_view(
            entry,
            self.manager.current_values().as_ref(),
            &self.manager.server_store(),
            self.now_ms(),
        );
        (view.value, view.quality)
    }

    async fn wait_current(&self, tag_id: i64, expected: (Option<f64>, Quality)) {
        let reached = wait_until(Duration::from_secs(5), || self.current(tag_id) == expected).await;
        assert!(
            reached,
            "tag {tag_id}: expected {expected:?}, got {:?}",
            self.current(tag_id)
        );
    }

    fn apply(&self, conn_id: i64, epoch: Option<u64>, items: &[IngestItem]) -> ApplyOutcome {
        self.ingest.apply(conn_id, epoch, items, self.now_ms())
    }

    fn epoch(&self, conn_id: i64) -> u64 {
        self.ingest.connection_info(conn_id).expect("slot").epoch
    }

    /// `push` 接続 `conn_id` へ 50 ms ごとに `ping` を送り続ける（戻り値を落とすと止まる）。
    fn keep_pinging(&self, conn_id: i64) -> Pinger {
        let stop = Arc::new(AtomicBool::new(false));
        let ingest = self.ingest.clone();
        let clock = self.manager.clock();
        let flag = stop.clone();
        tokio::spawn(async move {
            while !flag.load(Ordering::SeqCst) {
                let _ = ingest.ping(conn_id, clock.now_ms());
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        });
        Pinger(stop)
    }

    /// データファイルに記録された `(ptime_ms, 値)`（グループ `group_id` のタグ `tag_id`
    /// の列、全ファイル、時刻順）。収集を止めて（書き手が flush してから）読む。
    async fn recorded(&self, group_id: i64, tag_id: i64) -> Vec<(i64, Option<f64>)> {
        let group_key = format!("grp:{group_id}");
        let tag_key = format!("tag:{tag_id}");
        let mut rows = Vec::new();
        for file in banto_tstore::list_data_files(&self.data_dir).expect("list data files") {
            let reader = TsReader::open(&file.path).await.expect("open data file");
            let Some(group) = reader.group(&group_key) else {
                continue;
            };
            let Some(column) = group.columns.iter().position(|c| c.tag_key == tag_key) else {
                continue;
            };
            for sample in reader
                .read_range(&group_key, i64::MIN, i64::MAX)
                .await
                .expect("read range")
            {
                rows.push((sample.ptime_ms, sample.values[column]));
            }
        }
        rows.sort_by_key(|(ptime, _)| *ptime);
        rows
    }
}

struct Pinger(Arc<AtomicBool>);

impl Drop for Pinger {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

async fn wait_until(timeout: Duration, mut predicate: impl FnMut() -> bool) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if predicate() {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn good(v: f64) -> (Option<f64>, Quality) {
    (Some(v), Quality::Good)
}

const BAD: (Option<f64>, Quality) = (None, Quality::Bad);

fn value(key: &str, v: f64) -> IngestItem {
    IngestItem::new(key, json!(v))
}

/// ブリッジの `settings`（クエリ 1 本、`keys` を送る）。
fn bridge_settings(db_connection_id: i64, keys: &[&str], stale_timeout_ms: u64) -> Value {
    let columns: Vec<Value> = keys
        .iter()
        .enumerate()
        .map(|(i, key)| json!({ "column": format!("c{i}"), "key": key }))
        .collect();
    json!({
        "stale_timeout_ms": stale_timeout_ms,
        "bridge": {
            "kind": "db",
            "db_connection_id": db_connection_id,
            "queries": [{ "name": "q1", "period_ms": 1000, "sql": "SELECT 1", "columns": columns }],
        },
    })
}

// --- 統合テスト -------------------------------------------------------------

/// `apply` の件ごとの結果（§6.3）とメッセージ全体の拒否（§6）。収集は止めたまま
/// （Q11: 停止中も受ける、`collecting: false`）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn apply_judges_each_item_and_refuses_whole_messages() {
    let env = env("ingest-apply").await;
    let conn = env.push_connection("sender", None).await;
    let group = env.group(conn, "g", 100).await;
    env.tag(group, "temp", "line1.temp", "f32").await;
    env.tag(group, "run", "line1.run", "bit").await;
    let off = env.tag(group, "off", "line1.off", "f32").await;
    env.sql(&format!("UPDATE tags SET enabled = 0 WHERE id = {off}"))
        .await;
    let disabled = env.push_connection("disabled", None).await;
    env.sql(&format!(
        "UPDATE plc_connections SET enabled = 0 WHERE id = {disabled}"
    ))
    .await;
    env.commit().await;

    let ack = env
        .apply(
            conn,
            None,
            &[
                value("line1.temp", 12.5),
                value("line1.tmp", 1.0),
                value("line1.off", 1.0),
                IngestItem::new("line1.run", json!(2)),
                IngestItem::new("line1.run", json!(true)),
                value("line1.temp", 13.0),
                IngestItem::new("LINE1.TEMP", json!(1.0)),
            ],
        )
        .expect("accepted message");
    assert_eq!(ack.accepted, 1, "{ack:?}");
    let codes: Vec<(usize, RejectCode)> = ack.rejected.iter().map(|r| (r.index, r.code)).collect();
    assert_eq!(
        codes,
        vec![
            (1, RejectCode::UnknownKey),
            (2, RejectCode::InactiveKey),
            (3, RejectCode::InvalidValue),
            // 1 件目（index 3）は値で断ったが、キーは出ているので後の方は重複。
            (4, RejectCode::DuplicateKey),
            (5, RejectCode::DuplicateKey),
            (6, RejectCode::UnknownKey),
        ]
    );
    assert!(!ack.collecting);
    assert_eq!(ack.epoch, 1);
    assert_eq!(ack.stale_timeout_ms, DEFAULT_STALE_TIMEOUT_MS);
    assert_eq!(env.ingest.connection_info(conn).unwrap().held_values, 1);

    // 一般の送り手でも epoch を付ければ照合する。違えば全体を断り、何も反映しない。
    assert_eq!(
        env.apply(conn, Some(7), &[value("line1.temp", 99.0)]),
        Err(ApplyRefusal::StaleEpoch { epoch: 1 })
    );
    assert!(env.apply(conn, Some(1), &[]).is_ok());
    assert_eq!(
        env.apply(disabled, None, &[]),
        Err(ApplyRefusal::ConnectionDisabled)
    );
    assert_eq!(
        env.ingest.ping(disabled, 0),
        Err(ApplyRefusal::ConnectionDisabled)
    );
    assert_eq!(
        env.apply(9_999, None, &[]),
        Err(ApplyRefusal::UnknownConnection)
    );
    assert_eq!(
        env.ingest.ping(9_999, 0),
        Err(ApplyRefusal::UnknownConnection)
    );
    assert!(env.ingest.subscribe_state(9_999).is_none());
    env.finish().await;
}

/// データ型が変わったキーの保持値は commit で捨てる（型違いの古い値を、収集の
/// 開始後に新しい型の Good として見せない）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_commit_drops_the_held_value_of_a_key_whose_data_type_changed() {
    let env = env("ingest-type-change").await;
    let conn = env.push_connection("sender", None).await;
    let group = env.group(conn, "g", 100).await;
    let tag = env.tag(group, "k", "k", "f64").await;
    let hub = IngestHub::new(Arc::new(ManualClock::new(1_000_000, 0)));
    hub.commit(&RegistrySnapshot::load(&env.pool).await.unwrap());
    let key = format!("conn:{conn}");

    let ack = hub
        .apply(conn, None, &[value("k", 42.5)], 1_000_000)
        .unwrap();
    assert_eq!(ack.accepted, 1, "{ack:?}");
    assert_eq!(hub.connection_info(conn).unwrap().held_values, 1);

    env.sql(&format!(
        "UPDATE tags SET data_type = 'bit' WHERE id = {tag}"
    ))
    .await;
    hub.commit(&RegistrySnapshot::load(&env.pool).await.unwrap());
    assert_eq!(hub.connection_info(conn).unwrap().held_values, 0);

    hub.on_collection_starting();
    hub.on_collection_started();
    assert_eq!(hub.snapshot(&key).reading("k"), None);
    env.finish().await;
}

/// `snapshot` の Live / Down（§4.3）を手で進める時計で。メッセージ全体の拒否は
/// 最後の通信を進めない。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn snapshot_follows_the_last_accepted_message() {
    let env = env("ingest-snapshot").await;
    let conn = env
        .push_connection("sender", Some(json!({ "stale_timeout_ms": 2000 })))
        .await;
    let group = env.group(conn, "g", 100).await;
    env.tag(group, "temp", "line1.temp", "f32").await;
    let clock = Arc::new(ManualClock::new(1_000_000, 0));
    let hub = IngestHub::new(clock.clone());
    hub.commit(&RegistrySnapshot::load(&env.pool).await.unwrap());
    let key = format!("conn:{conn}");

    assert_eq!(
        hub.snapshot(&key).link,
        ExternalLink::Down {
            detail: Some("送り手が一度も接続していません".to_string())
        }
    );
    assert_eq!(hub.snapshot("conn:9999"), ExternalSnapshot::down(None));
    assert!(hub
        .apply(conn, Some(5), &[value("line1.temp", 1.0)], clock.now_ms())
        .is_err());
    assert!(!hub.snapshot(&key).is_live());

    hub.apply(conn, None, &[value("line1.temp", 1.5)], clock.now_ms())
        .unwrap();
    let snapshot = hub.snapshot(&key);
    assert!(snapshot.is_live());
    assert_eq!(
        snapshot.reading("line1.temp"),
        Some(&ExternalReading {
            value: Some(1.5),
            quality: ExternalQuality::Good
        })
    );
    clock.advance_ms(2_000);
    assert!(hub.snapshot(&key).is_live());
    clock.advance_ms(1);
    assert_eq!(
        hub.snapshot(&key).link,
        ExternalLink::Down {
            detail: Some("送り手からの通信が 2 秒ありません".to_string())
        }
    );
    // ping は値を変えずに Live へ戻す。
    hub.ping(conn, clock.now_ms()).unwrap();
    assert_eq!(
        hub.snapshot(&key).reading("line1.temp").unwrap().value,
        Some(1.5)
    );
    env.finish().await;
}

/// 値が周期ごとに記録される・`stale` は表示だけで記録は NULL・`stale` + 値なしは
/// 表示も Bad（§4.4）・収集の停止で Bad。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn values_are_recorded_every_cycle_stale_records_null_and_stop_gives_bad() {
    let env = env("ingest-record").await;
    let conn = env.push_connection("sender", None).await;
    let group = env.group(conn, "g", 100).await;
    let temp = env.tag(group, "temp", "line1.temp", "f32").await;
    let level = env.tag(group, "level", "line1.level", "f32").await;
    let empty = env.tag(group, "empty", "line1.empty", "f32").await;
    env.commit().await;
    env.start().await;

    let ack = env
        .apply(
            conn,
            None,
            &[
                value("line1.temp", 12.5),
                IngestItem::new("line1.level", json!(7.0)).with_quality("stale"),
                IngestItem::new("line1.empty", Value::Null).with_quality("stale"),
            ],
        )
        .unwrap();
    assert_eq!(ack.accepted, 3, "{ack:?}");
    assert!(ack.collecting);
    env.wait_current(temp, good(12.5)).await;
    env.wait_current(level, (Some(7.0), Quality::Stale)).await;
    assert_eq!(env.current(empty), BAD);
    let since = env.now_ms();
    tokio::time::sleep(Duration::from_millis(600)).await;
    let until = env.now_ms();

    env.stop().await;
    assert_eq!(env.current(temp), BAD);
    assert_eq!(env.current(level), BAD);

    let temp_rows: Vec<_> = env
        .recorded(group, temp)
        .await
        .into_iter()
        .filter(|(ptime, _)| (since..=until).contains(ptime))
        .collect();
    assert!(temp_rows.len() >= 4, "{temp_rows:?}");
    assert!(
        temp_rows.iter().all(|(_, v)| *v == Some(12.5)),
        "{temp_rows:?}"
    );
    let level_rows = env.recorded(group, level).await;
    assert!(!level_rows.is_empty());
    assert!(
        level_rows.iter().all(|(_, v)| v.is_none()),
        "{level_rows:?}"
    );
    env.finish().await;
}

/// タイムアウトで Bad と `plc_disconnected`（理由つき）。送り直せば戻る。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_silent_sender_goes_bad_with_plc_disconnected() {
    let env = env("ingest-timeout").await;
    let conn = env
        .push_connection("sender", Some(json!({ "stale_timeout_ms": 1000 })))
        .await;
    let group = env.group(conn, "g", 100).await;
    let temp = env.tag(group, "temp", "line1.temp", "f32").await;
    env.commit().await;
    let mut events = env.manager.subscribe_events();
    env.start().await;

    env.apply(conn, None, &[value("line1.temp", 3.0)]).unwrap();
    env.wait_current(temp, good(3.0)).await;
    env.wait_current(temp, BAD).await;

    let conn_key = format!("conn:{conn}");
    let disconnected = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let event = events.recv().await.expect("event stream");
            if event.kind == EventKind::PlcDisconnected
                && event.connection_key.as_deref() == Some(conn_key.as_str())
            {
                return event;
            }
        }
    })
    .await
    .expect("plc_disconnected");
    let detail = disconnected.detail.unwrap_or_default();
    assert!(detail.starts_with("送り手からの通信が "), "{detail}");

    env.apply(conn, None, &[value("line1.temp", 4.0)]).unwrap();
    env.wait_current(temp, good(4.0)).await;
    env.finish().await;
}

/// 停止前に Good だった値が、開始の直後の最初の tick で Good にならない（`ping` を
/// 送り続けても）。tstore にも開始の後の停止前の値の行が無い（§4.5、Q19）。
/// 一般の送り手とブリッジの両方。ブリッジは停止の後の epoch で停止中に送った値も
/// 開始で捨てる。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_good_value_before_a_stop_is_not_good_after_the_restart() {
    let env = env("ingest-restart").await;
    let general = env.push_connection("sender", None).await;
    let general_group = env.group(general, "g", 100).await;
    let general_tag = env.tag(general_group, "temp", "line1.temp", "f32").await;
    let pg = env.postgres_connection("plant-db").await;
    let bridge = env
        .push_connection("bridge", Some(bridge_settings(pg, &["b.temp"], 10_000)))
        .await;
    let bridge_group = env.group(bridge, "g", 100).await;
    let bridge_tag = env.tag(bridge_group, "temp", "b.temp", "f32").await;
    env.commit().await;
    env.start().await;

    let epoch = env.epoch(bridge);
    env.apply(general, None, &[value("line1.temp", 42.5)])
        .unwrap();
    env.apply(bridge, Some(epoch), &[value("b.temp", 42.5)])
        .unwrap();
    env.wait_current(general_tag, good(42.5)).await;
    env.wait_current(bridge_tag, good(42.5)).await;

    let _general_ping = env.keep_pinging(general);
    let _bridge_ping = env.keep_pinging(bridge);
    env.stop().await;
    assert_eq!(env.ingest.connection_info(general).unwrap().held_values, 0);
    assert_eq!(env.ingest.connection_info(bridge).unwrap().held_values, 0);
    // ブリッジ: 停止で epoch が進み、停止の前の epoch の送信は断る。停止の後の
    // epoch の値は受けて保持する（ただし次の開始で捨てる）。
    let stopped_epoch = env.epoch(bridge);
    assert_eq!(stopped_epoch, epoch + 1);
    assert_eq!(
        env.apply(bridge, Some(epoch), &[value("b.temp", 42.5)]),
        Err(ApplyRefusal::StaleEpoch {
            epoch: stopped_epoch
        })
    );
    env.apply(bridge, Some(stopped_epoch), &[value("b.temp", 42.5)])
        .unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;
    let restart_ms = env.now_ms();
    env.start().await;
    // 開始の直後（最初の tick はタスクの起動と同時）から Bad。ping は続いている。
    assert_eq!(env.current(general_tag), BAD);
    assert_eq!(env.current(bridge_tag), BAD);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(env.current(general_tag), BAD);
    assert_eq!(env.current(bridge_tag), BAD);
    env.stop().await;

    for (group, tag) in [(general_group, general_tag), (bridge_group, bridge_tag)] {
        let rows = env.recorded(group, tag).await;
        let before: Vec<_> = rows.iter().filter(|(p, _)| *p < restart_ms).collect();
        let after: Vec<_> = rows.iter().filter(|(p, _)| *p >= restart_ms).collect();
        assert!(
            before.iter().any(|(_, v)| *v == Some(42.5)),
            "the value was recorded before the stop: {before:?}"
        );
        assert!(after.len() >= 3, "{after:?}");
        assert!(
            after.iter().all(|(_, v)| v.is_none()),
            "no row of the pre-stop value after the restart: {after:?}"
        );
    }
    env.finish().await;
}

/// 一般の送り手が停止中に送った値は、開始の直後（最初の tick）から Good で記録される
/// （Q11）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_general_senders_values_received_while_stopped_are_good_right_after_start() {
    let env = env("ingest-general-start").await;
    let conn = env.push_connection("sender", None).await;
    let group = env.group(conn, "g", 100).await;
    let temp = env.tag(group, "temp", "line1.temp", "f32").await;
    env.commit().await;

    let ack = env.apply(conn, None, &[value("line1.temp", 3.0)]).unwrap();
    assert!(!ack.collecting);
    let start_ms = env.now_ms();
    env.start().await;
    env.wait_current(temp, good(3.0)).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    env.stop().await;

    let rows: Vec<_> = env
        .recorded(group, temp)
        .await
        .into_iter()
        .filter(|(p, _)| *p >= start_ms)
        .collect();
    // 最初の行（開始と同時の tick）から Good の値。
    assert!(rows.len() >= 3, "{rows:?}");
    assert!(rows.iter().all(|(_, v)| *v == Some(3.0)), "{rows:?}");
    env.finish().await;
}

/// ブリッジの接続は開始の後の epoch の値が届くまで Bad。古い epoch のメッセージと
/// epoch の無いメッセージは `stale_epoch`（§4.6）。`state` は開始の確定と停止で届く。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bridge_stays_bad_until_values_with_the_new_epoch_arrive() {
    let env = env("ingest-bridge-epoch").await;
    let pg = env.postgres_connection("plant-db").await;
    let bridge = env
        .push_connection("bridge", Some(bridge_settings(pg, &["b.temp"], 10_000)))
        .await;
    let group = env.group(bridge, "g", 100).await;
    let temp = env.tag(group, "temp", "b.temp", "f32").await;
    env.commit().await;
    let mut state = env.ingest.subscribe_state(bridge).unwrap();
    assert_eq!(
        *state.borrow_and_update(),
        IngestState {
            collecting: false,
            epoch: 1,
            config_changed: false
        }
    );

    env.start().await;
    assert!(state.has_changed().unwrap());
    let started = *state.borrow_and_update();
    assert_eq!(
        started,
        IngestState {
            collecting: true,
            epoch: 2,
            config_changed: false
        }
    );
    assert_eq!(
        env.apply(bridge, Some(1), &[value("b.temp", 1.0)]),
        Err(ApplyRefusal::StaleEpoch { epoch: 2 })
    );
    assert_eq!(
        env.apply(bridge, None, &[value("b.temp", 1.0)]),
        Err(ApplyRefusal::StaleEpoch { epoch: 2 })
    );
    // 断ったメッセージは最後の通信を進めないので、リンクもまだ Down。
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(env.current(temp), BAD);

    let ack = env
        .apply(bridge, Some(started.epoch), &[value("b.temp", 5.0)])
        .unwrap();
    assert_eq!((ack.accepted, ack.epoch, ack.collecting), (1, 2, true));
    env.wait_current(temp, good(5.0)).await;

    env.stop().await;
    let stopped = *state.borrow_and_update();
    assert_eq!(
        stopped,
        IngestState {
            collecting: false,
            epoch: 3,
            config_changed: false
        }
    );
    env.apply(bridge, Some(3), &[value("b.temp", 6.0)]).unwrap();
    env.start().await;
    assert_eq!(state.borrow_and_update().epoch, 4);
    assert_eq!(env.current(temp), BAD);
    assert_eq!(
        env.apply(bridge, Some(3), &[value("b.temp", 6.0)]),
        Err(ApplyRefusal::StaleEpoch { epoch: 4 })
    );
    env.apply(bridge, Some(4), &[value("b.temp", 7.0)]).unwrap();
    env.wait_current(temp, good(7.0)).await;
    env.finish().await;
}

/// ブリッジの設定から外したキーは commit で Bad、その後の値は `not_in_bridge_set`。
/// 設定の変更は epoch を進めて `config_changed: true` を知らせる（§4.6）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_key_removed_from_the_bridge_config_goes_bad_and_is_then_refused() {
    let env = env("ingest-bridge-set").await;
    let pg = env.postgres_connection("plant-db").await;
    let bridge = env
        .push_connection(
            "bridge",
            Some(bridge_settings(pg, &["b.temp", "b.level"], 10_000)),
        )
        .await;
    let group = env.group(bridge, "g", 100).await;
    let temp = env.tag(group, "temp", "b.temp", "f32").await;
    let level = env.tag(group, "level", "b.level", "f32").await;
    env.commit().await;
    env.start().await;
    let mut state = env.ingest.subscribe_state(bridge).unwrap();
    let epoch = state.borrow_and_update().epoch;
    env.apply(
        bridge,
        Some(epoch),
        &[value("b.temp", 1.0), value("b.level", 2.0)],
    )
    .unwrap();
    env.wait_current(temp, good(1.0)).await;
    env.wait_current(level, good(2.0)).await;

    let settings = bridge_settings(pg, &["b.temp"], 10_000).to_string();
    sqlx::query("UPDATE plc_connections SET settings = ? WHERE id = ?")
        .bind(settings)
        .bind(bridge)
        .execute(&env.pool)
        .await
        .unwrap();
    env.commit().await;
    assert!(state.has_changed().unwrap());
    let changed = *state.borrow_and_update();
    assert_eq!(
        changed,
        IngestState {
            collecting: true,
            epoch: epoch + 1,
            config_changed: true
        }
    );
    env.wait_current(level, BAD).await;
    env.wait_current(temp, BAD).await;

    let ack = env
        .apply(
            bridge,
            Some(changed.epoch),
            &[value("b.temp", 3.0), value("b.level", 4.0)],
        )
        .unwrap();
    assert_eq!(ack.accepted, 1);
    assert_eq!(
        ack.rejected,
        vec![ItemRejection {
            index: 1,
            key: "b.level".to_string(),
            code: RejectCode::NotInBridgeSet
        }]
    );
    env.wait_current(temp, good(3.0)).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(env.current(level), BAD);
    env.finish().await;
}

/// 稼働中の再適用（`commit_catalog_and_apply_live`）は epoch を進めず `state` も
/// 送らない。グループの周期の変更でタスクが作り直されても（`apply_config` の
/// replaced）保持値は残り、Good のまま続く（§4.2・§4.5）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_live_reapply_keeps_the_epoch_and_the_held_values() {
    let env = env("ingest-live-reapply").await;
    let pg = env.postgres_connection("plant-db").await;
    let bridge = env
        .push_connection("bridge", Some(bridge_settings(pg, &["b.temp"], 10_000)))
        .await;
    let group = env.group(bridge, "g", 100).await;
    let temp = env.tag(group, "temp", "b.temp", "f32").await;
    env.commit().await;
    env.start().await;
    let mut state = env.ingest.subscribe_state(bridge).unwrap();
    let epoch = state.borrow_and_update().epoch;
    env.apply(bridge, Some(epoch), &[value("b.temp", 8.0)])
        .unwrap();
    env.wait_current(temp, good(8.0)).await;
    let _ping = env.keep_pinging(bridge);

    env.sql(&format!(
        "UPDATE collection_groups SET period_ms = 200 WHERE id = {group}"
    ))
    .await;
    env.commit().await;
    let report = env.manager.last_apply().expect("apply_config ran");
    assert!(
        report.replaced.contains(&format!("conn:{bridge}")),
        "{report:?}"
    );
    assert!(!state.has_changed().unwrap());
    assert_eq!(env.epoch(bridge), epoch);
    assert_eq!(env.ingest.connection_info(bridge).unwrap().held_values, 1);
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(env.current(temp), good(8.0));
    // 同じ epoch のまま送り続けられる。
    env.apply(bridge, Some(epoch), &[value("b.temp", 9.0)])
        .unwrap();
    env.wait_current(temp, good(9.0)).await;
    env.finish().await;
}

/// キー（タグの `address`）を変えると、古いキーの保持値は commit で消え、古いキーは
/// `unknown_key`。新しいキーは送られるまで Bad。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn renaming_a_key_drops_the_old_value() {
    let env = env("ingest-rename-key").await;
    let conn = env.push_connection("sender", None).await;
    let group = env.group(conn, "g", 100).await;
    let temp = env.tag(group, "temp", "old.key", "f32").await;
    env.commit().await;
    env.start().await;
    env.apply(conn, None, &[value("old.key", 1.0)]).unwrap();
    env.wait_current(temp, good(1.0)).await;
    let _ping = env.keep_pinging(conn);

    env.sql(&format!(
        "UPDATE tags SET address = 'new.key' WHERE id = {temp}"
    ))
    .await;
    env.commit().await;
    assert_eq!(env.ingest.connection_info(conn).unwrap().held_values, 0);
    env.wait_current(temp, BAD).await;
    let ack = env.apply(conn, None, &[value("old.key", 2.0)]).unwrap();
    assert_eq!(ack.rejected[0].code, RejectCode::UnknownKey);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(env.current(temp), BAD);
    env.apply(conn, None, &[value("new.key", 3.0)]).unwrap();
    env.wait_current(temp, good(3.0)).await;
    env.finish().await;
}

/// `apply_run` が失敗して Faulted になったら `collecting: true` を知らせない
/// （ブリッジは `collecting: false` のまま DB へ問い合わせを始めない。§4.5、
/// 2026-10-21 再レビュー P2）。epoch は開始で進んでいるので、開始の前の epoch の
/// 値も入らない。次の開始が成功すれば知らせる。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_start_does_not_announce_collecting() {
    let env = env("ingest-faulted").await;
    let pg = env.postgres_connection("plant-db").await;
    let bridge = env
        .push_connection("bridge", Some(bridge_settings(pg, &["b.temp"], 10_000)))
        .await;
    let group = env.group(bridge, "g", 100).await;
    env.tag(group, "temp", "b.temp", "f32").await;
    env.commit().await;
    let mut state = env.ingest.subscribe_state(bridge).unwrap();
    let before = *state.borrow_and_update();
    assert!(!before.collecting);

    env.manager.fail_next_apply_for_test();
    let status = env.controller.start(RunMode::Configured).await.unwrap();
    assert_eq!(status.state, CollectionState::Faulted, "{status:?}");
    assert!(!state.has_changed().unwrap(), "no state was announced");
    assert_eq!(*state.borrow(), before);
    assert!(!env.ingest.collecting());
    assert!(!env.ingest.connection_info(bridge).unwrap().collecting);
    assert_eq!(
        env.apply(bridge, Some(before.epoch), &[value("b.temp", 1.0)]),
        Err(ApplyRefusal::StaleEpoch {
            epoch: before.epoch + 1
        })
    );

    env.start().await;
    assert!(state.has_changed().unwrap());
    let started = *state.borrow_and_update();
    assert!(started.collecting);
    assert_eq!(started.epoch, before.epoch + 2);
    assert!(env.ingest.collecting());
    env.finish().await;
}

/// `push` のタグの `value_source` は `"push"`（§12 Q14）。全シミュレーションの運転中
/// でもシミュレーションとは言わない。catalog の wire には `push` の印を出さない。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn push_tags_report_the_push_value_source() {
    let env = env("ingest-value-source").await;
    let conn = env.push_connection("sender", None).await;
    let group = env.group(conn, "g", 100).await;
    let temp = env.tag(group, "temp", "line1.temp", "f32").await;
    env.commit().await;

    let map = env.manager.tag_map();
    let entry = map.iter().find(|e| e.ids.2 == temp).unwrap();
    assert!(entry.push);
    let wire = serde_json::to_value(entry).unwrap();
    assert!(wire.get("push").is_none(), "{wire}");
    let computed = env.manager.computed_engine();
    for mode in [RunMode::Configured, RunMode::AllSimulation] {
        for state in [CollectionState::Running, CollectionState::Stopped] {
            let runtime = CollectionStatus {
                state,
                mode,
                run_id: None,
                last_error: None,
                configured_revision: 0,
                running_revision: 0,
            };
            assert_eq!(
                crate::value_source::value_source_for_tag(entry, &runtime, &map, &computed),
                "push",
                "{mode:?} {state:?}"
            );
            assert!(!crate::value_source::effective_simulation_for_tag(
                entry, &runtime, &map, &computed
            ));
        }
    }
    env.finish().await;
}
