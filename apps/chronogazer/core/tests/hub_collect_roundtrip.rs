//! #383 段階3 P3（2026-10-10 オーナー決定 H2〜H4）: **Hub 経由のタグを
//! PLC 直結のタグと同じ収集エンジンで記録する**一巡を、モック Hub 相手の
//! 実際のワイヤ経路（REST の catalog / values と WebSocket の購読）で固定する。
//!
//! 流れは利用者の操作と同じ: Hub に手動キーで接続（`HubService::adopt_manual_key`）
//! → レジストリに `protocol = hub` の接続・グループ・タグを登録（P4 の画面は
//! まだ無いので `banto_tags` の公開サービスで）→ 収集を開始
//! （`CollectorService::start`、`attach_hub` 済み = デスクトップと同じ形）→
//! 現在値（`values_response` = `GET /api/collect/values` の応答そのもの）→
//! 停止 → データファイル（`TsReader`）と `collect_events`。
//!
//! 確かめること:
//!
//! * (a) Hub のタグ一覧から消えたタグは `hubTagNotFound` で**そのタグだけ**
//!   外れ、残りで `Running`（H3・#414）、
//! * (b) 現在値: Hub の good の値は `value_source` に依らず（`simulation`・
//!   `derived_simulation`・未知を含む）`good`、Hub の `stale` は `stale`、
//!   外したタグは `invalid`、
//! * (c) データファイル: good の値は `value_source` に依らず**毎周期**記録され、
//!   stale だけが null（H4。`value_source` を解釈しない = 2026-10-11 オーナー
//!   決定。P3 では simulation 系と未知を記録しなかった）、
//! * (d) `collect_events` に Hub の接続キーの `plc_connected`。
//!
//! P5（#383 段階3）で足したもの: Hub の切断→再接続が Hub の接続キーの
//! `plc_connected` → `plc_disconnected` → `plc_reconnected` で出る、bit
//! タグの記録（0/1）、Hub 経由のタグへのしきい値（記録計の側の
//! `recorder_tag_settings`）がシミュレーション由来の値でも判定され記録される。
//!
//! 待ちはすべて「条件をポーリング + 上限時間」。固定 sleep で緑にしない。

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use banto_hub_bootstrap::state::memory::MemoryKeyStore;
use banto_hub_bootstrap::{HubStatus, KeyStore};
use banto_tagclient::{
    CatalogSnapshot, CatalogTag, CollectionMode, StableTagId, ValueEntry, ValueQuality,
    ValueSource, ValuesSnapshot,
};
use banto_tags::{
    CollectionGroupInput, CollectionGroupService, PlcConnectionInput, PlcConnectionService,
    TagInput, TagService,
};
use banto_tstore::{list_data_files, TsReader};
use chronogazer_core::collect::{
    resolve_data_dir, CollectorService, CollectorState, EventPage, QualityView, Readout,
};
use chronogazer_core::db::{init_db, Db};
use chronogazer_core::hub::HubService;
use chronogazer_core::settings::SettingsService;
use chronogazer_core::tag_thresholds::{TagThresholdService, TagThresholdsPayload};
use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::{accept_async, tungstenite::Message};

const WAIT_LIMIT: Duration = Duration::from_secs(20);
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// 購読の ID（`banto-tagclient` の `handle::SUBSCRIPTION_ID`）。data フレームの
/// `id` がこれと一致しないと捨てられる。
const SUBSCRIPTION_ID: i64 = 1;

/// Hub 側のタグ（外部名）。
const REAL: &str = "PLC9.G.real";
const SIM: &str = "PLC9.G.sim";
const DSIM: &str = "PLC9.G.dsim";
const STALE: &str = "PLC9.G.stale";
/// Hub が未知の `value_source` を付けた値（将来のラベル）。
const FUTURE: &str = "PLC9.G.future";
/// 登録はしてあるが Hub のタグ一覧に無い（Hub で消された）。
const GONE: &str = "PLC9.G.gone";

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

// --- モック Hub（`src/hub.rs` の `live_subscription` のテストと同じ作法） ----

struct MockHub {
    catalog: String,
    values: String,
    frame: String,
}

fn catalog_tag(name: &str, index: i64) -> CatalogTag {
    CatalogTag {
        external_name: name.to_owned(),
        tag_key: format!("tag:{index}"),
        ids: StableTagId::new(1, 1, index),
        connection: "PLC9".into(),
        group: "G".into(),
        name: name.rsplit('.').next().unwrap_or(name).into(),
        address: "D100".into(),
        data_type: "f64".into(),
        unit: None,
        decimals: 0,
        period_ms: 100,
        enabled: true,
        writable: false,
        tag_kind: "plc".into(),
        expression: None,
        retain: false,
        simulation: false,
        configured_simulation: false,
        effective_simulation: false,
        value_source: ValueSource::Real,
    }
}

fn entry(tag: &str, v: f64, q: ValueQuality, value_source: ValueSource) -> ValueEntry {
    ValueEntry {
        tag: tag.to_owned(),
        v: Some(v),
        q,
        t: 1_000,
        value_source,
    }
}

fn mock_hub() -> MockHub {
    let catalog = CatalogSnapshot {
        revision: 1,
        run_id: Some(1),
        collection_mode: CollectionMode::Configured,
        tags: [REAL, SIM, DSIM, STALE, FUTURE]
            .iter()
            .enumerate()
            .map(|(index, name)| catalog_tag(name, index as i64 + 1))
            .collect(),
    };
    let values = ValuesSnapshot {
        revision: catalog.revision,
        t: 1_000,
        run_id: catalog.run_id,
        collection_mode: catalog.collection_mode.clone(),
        values: vec![
            entry(REAL, 1.0, ValueQuality::Good, ValueSource::Real),
            entry(SIM, 2.0, ValueQuality::Good, ValueSource::Simulation),
            entry(
                DSIM,
                3.0,
                ValueQuality::Good,
                ValueSource::DerivedSimulation,
            ),
            entry(STALE, 4.0, ValueQuality::Stale, ValueSource::Real),
            entry(
                FUTURE,
                5.0,
                ValueQuality::Good,
                ValueSource::Unknown("future_source".to_owned()),
            ),
        ],
    };
    // WS は tag/v/q/t だけを運ぶ。`t` を REST より新しくして、こちらが勝つ。
    let frame = format!(
        concat!(
            r#"{{"op":"data","id":{},"t":2000,"#,
            r#""values":[{{"tag":"{}","v":21.5,"q":"good","t":2000}}]}}"#
        ),
        SUBSCRIPTION_ID, REAL
    );
    MockHub {
        catalog: serde_json::to_string(&catalog).unwrap(),
        values: serde_json::to_string(&values).unwrap(),
        frame,
    }
}

async fn read_http_request(stream: &mut TcpStream) {
    let mut request = Vec::new();
    let mut buffer = [0_u8; 1024];
    loop {
        let count = stream.read(&mut buffer).await.unwrap_or(0);
        if count == 0 {
            return;
        }
        request.extend_from_slice(&buffer[..count]);
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            return;
        }
    }
}

async fn peek_head(stream: &TcpStream) -> String {
    let mut head = [0_u8; 1024];
    for _ in 0..50 {
        let count = stream.peek(&mut head).await.unwrap_or(0);
        let text = String::from_utf8_lossy(&head[..count]).to_ascii_lowercase();
        if text.contains("\r\n\r\n") {
            return text;
        }
        tokio::task::yield_now().await;
    }
    String::from_utf8_lossy(&head).to_ascii_lowercase()
}

/// catalog（`/api/v1/tags`）・values（`/api/v1/values`）・WS（subscribe を
/// 待って data フレームを 1 つ送り、閉じられるまで開けておく）に答え続ける。
///
/// WS の接続は `JoinSet` に持つので、この関数の future を止める（`abort` して
/// 待つ）と開いている WS もまとめて閉じる（Hub が落ちた状態を作れる）。
async fn serve_hub(listener: TcpListener, hub: Arc<MockHub>) {
    let mut sockets = tokio::task::JoinSet::new();
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        let head = peek_head(&stream).await;
        if head.contains("upgrade: websocket") {
            let frame = hub.frame.clone();
            sockets.spawn(async move {
                let Ok(mut socket) = accept_async(stream).await else {
                    return;
                };
                let subscribe = tokio::time::timeout(Duration::from_secs(5), socket.next()).await;
                if !matches!(subscribe, Ok(Some(Ok(Message::Text(_))))) {
                    return;
                }
                if socket.send(Message::Text(frame.into())).await.is_err() {
                    return;
                }
                while let Some(message) = socket.next().await {
                    match message {
                        Ok(Message::Close(reply)) => {
                            let _ = socket.send(Message::Close(reply)).await;
                            break;
                        }
                        Ok(_) => {}
                        Err(_) => break,
                    }
                }
            });
            continue;
        }
        let body = if head.contains("/api/v1/values") {
            hub.values.clone()
        } else {
            hub.catalog.clone()
        };
        let mut stream = stream;
        read_http_request(&mut stream).await;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(response.as_bytes()).await;
    }
}

// --- レジストリ ----------------------------------------------------------------

/// `protocol = hub` の接続 1・グループ 1（100ms）・タグ 5 を登録し、
/// `(接続 id, グループ id, [REAL, SIM, DSIM, STALE, FUTURE, GONE] のタグ id)` を返す。
async fn register_hub_tags(pool: &sqlx::SqlitePool) -> (i64, i64, Vec<i64>) {
    let specs: Vec<(&str, &str)> = [REAL, SIM, DSIM, STALE, FUTURE, GONE]
        .iter()
        .map(|name| (*name, "f64"))
        .collect();
    register_hub_specs(pool, &specs).await
}

/// `protocol = hub` の接続 1・グループ 1（100ms）と、`(外部名, データ型)` の
/// タグを登録する。`(接続 id, グループ id, タグ id（指定順）)` を返す。
async fn register_hub_specs(
    pool: &sqlx::SqlitePool,
    specs: &[(&str, &str)],
) -> (i64, i64, Vec<i64>) {
    let conn = PlcConnectionService::new(pool.clone())
        .create(PlcConnectionInput {
            name: "Hub".to_string(),
            protocol: banto_tags::HUB_PROTOCOL.to_string(),
            host: String::new(),
            port: 0,
            unit_id: 1,
            enabled: true,
            simulation: false,
            word_order: String::new(),
            cpu_type: None,
            database: None,
            username: None,
            secrets: Default::default(),
            settings: None,
        })
        .await
        .expect("Hub 接続を登録できる");
    let group = CollectionGroupService::new(pool.clone())
        .create(CollectionGroupInput {
            name: "Hub経由".to_string(),
            plc_connection_id: conn.id,
            period_ms: 100,
            enabled: true,
            default_writable: false,
            query_sql: None,
        })
        .await
        .expect("収集グループを登録できる");
    let tags = TagService::new(pool.clone());
    let mut ids = Vec::new();
    for (name, data_type) in specs {
        let tag = tags
            .create(TagInput {
                name: name.rsplit('.').next().unwrap().to_string(),
                collection_group_id: group.id,
                address: name.to_string(),
                data_type: data_type.to_string(),
                string_length: None,
                string_encoding: "utf8".to_string(),
                raw_lo: None,
                raw_hi: None,
                eng_lo: None,
                eng_hi: None,
                unit: None,
                decimals: 2,
                enabled: true,
                writable: false,
                tag_kind: "plc".to_string(),
                expression: None,
                retain: false,
                expected_revision: None,
            })
            .await
            .expect("Hub 経由のタグを登録できる");
        ids.push(tag.id);
    }
    (conn.id, group.id, ids)
}

/// データファイルの `group_key` の全行（列はタグの順）。
async fn stored_rows(data_dir: &Path, group_key: &str) -> Vec<Vec<Option<f64>>> {
    let mut rows = Vec::new();
    for file in list_data_files(data_dir).expect("データファイルを列挙できる") {
        let reader = TsReader::open(&file.path)
            .await
            .expect("データファイルを開ける");
        if reader.group(group_key).is_none() {
            continue;
        }
        for sample in reader
            .read_range(group_key, i64::MIN, i64::MAX)
            .await
            .expect("読める")
        {
            rows.push(sample.values);
        }
    }
    rows
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hub_sourced_tags_are_recorded_by_the_same_collector() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(serve_hub(listener, Arc::new(mock_hub())));

    let dir = TempDir::new();
    let pool = init_db(dir.path().join("chronogazer.sqlite3"))
        .await
        .expect("設定 DB を初期化できる");
    let data_dir = resolve_data_dir(dir.path(), "./data");

    // Hub に手動キーで接続する（デスクトップの「APIキーを採用」と同じ経路）。
    let settings = SettingsService::new(Db::Sqlite(pool.clone()));
    let keys: Arc<dyn KeyStore> = Arc::new(MemoryKeyStore::new());
    let hub = HubService::new(settings, keys)
        .await
        .expect("HubService を組める");
    let view = hub
        .adopt_manual_key(&endpoint, "bh_abcd1234_opaque-secret".to_string())
        .await
        .expect("モック Hub に接続できる");
    assert!(
        matches!(view.status, HubStatus::Connected { .. }),
        "{:?}",
        view.status
    );

    let (conn_id, group_id, ids) = register_hub_tags(&pool).await;
    let conn_key = format!("conn:{conn_id}");
    let group_key = format!("grp:{group_id}");
    let key = |i: usize| format!("tag:{}", ids[i]);

    let svc = CollectorService::new(
        pool.clone(),
        data_dir.clone(),
        chronogazer_core::engine::EngineConfig::builtin_drivers(),
    );
    assert!(svc.attach_hub(hub.clone()));

    // (a) 消えたタグだけ外れて Running。
    let outcome = svc.start().await.expect("収集を開始できる");
    assert!(
        !outcome.pending,
        "開始が打ち切られた（{:?}）",
        outcome.status
    );
    match svc.state() {
        CollectorState::Running {
            groups,
            tags,
            exclusions,
        } => {
            assert_eq!((groups, tags), (1, 5));
            let excluded: Vec<(&str, &str)> = exclusions
                .iter()
                .map(|e| (e.key.as_str(), e.reason))
                .collect();
            assert_eq!(excluded, vec![(key(5).as_str(), "hubTagNotFound")]);
        }
        other => panic!("Running を期待したが {other:?}"),
    }

    // (b) 現在値（`GET /api/collect/values` の応答そのもの）。購読が Live に
    // なり、WS の新しい値（21.5）が届くまで待つ。
    let values = wait_until("Hub の値が現在値に届く", || {
        let response = svc.values_response();
        let real = key(0);
        async move {
            let Readout::Ready { data } = &response.readout else {
                return Err(response.readout.as_str().to_string());
            };
            match data.get(&real) {
                Some(sample) if sample.value == Some(21.5) => Ok(data.clone()),
                other => Err(format!("{other:?}")),
            }
        }
    })
    .await;
    assert_eq!(values[&key(0)].quality, QualityView::Good);
    assert_eq!(
        (values[&key(1)].value, values[&key(1)].quality),
        (Some(2.0), QualityView::Good),
        "simulation も good の値として扱う"
    );
    assert_eq!(
        (values[&key(2)].value, values[&key(2)].quality),
        (Some(3.0), QualityView::Good),
        "derived_simulation も同じ"
    );
    assert_eq!(
        (values[&key(3)].value, values[&key(3)].quality),
        (Some(4.0), QualityView::Stale)
    );
    assert_eq!(
        (values[&key(4)].value, values[&key(4)].quality),
        (Some(5.0), QualityView::Good),
        "未知の value_source も good の値として扱う"
    );
    assert_eq!(values[&key(5)].quality, QualityView::Invalid);

    // Hub は値を変えない（on-change なので何も送ってこない）まま、定常値が
    // 毎周期記録されるのを待つ（100ms 周期で 10 行以上）。
    wait_until("定常値が毎周期記録される", || {
        let data_dir = data_dir.clone();
        let group_key = group_key.clone();
        async move {
            let recorded = stored_rows(&data_dir, &group_key)
                .await
                .iter()
                .filter(|row| row[0] == Some(21.5))
                .count();
            if recorded >= 10 {
                Ok(())
            } else {
                Err(format!("{recorded} 行"))
            }
        }
    })
    .await;
    // まだ Good のまま（Hub の `t` が古くなっても Stale にしない）。
    match svc.values_response().readout {
        Readout::Ready { data } => assert_eq!(data[&key(0)].quality, QualityView::Good),
        other => panic!("{}", other.as_str()),
    }

    svc.stop().await.expect("停止できる");

    // (c) データファイル: good の値は出所（simulation・derived_simulation・
    // 未知）に依らず記録され、stale だけが null。外したタグには列が無い
    // （5 列）。
    let rows = stored_rows(&data_dir, &group_key).await;
    let live_rows: Vec<&Vec<Option<f64>>> = rows.iter().filter(|row| row[0].is_some()).collect();
    assert!(live_rows.len() >= 10, "{rows:?}");
    for row in &rows {
        assert_eq!(row.len(), 5, "{row:?}");
        if row[0].is_some() {
            assert_eq!(
                &row[1..],
                &[Some(2.0), Some(3.0), None, Some(5.0)],
                "simulation・derived_simulation・未知の出所も記録される: {row:?}"
            );
        }
    }
    assert!(
        live_rows
            .iter()
            .all(|row| row[0] == Some(1.0) || row[0] == Some(21.5)),
        "{rows:?}"
    );

    // (d) Hub の接続キーの `plc_connected`。
    let events = match svc.events(EventPage::default()).await {
        Readout::Ready { data } => data.rows,
        other => panic!("イベント一覧が読めない: {}", other.as_str()),
    };
    assert!(
        events.iter().any(|row| row.kind == "plc_connected"
            && row.connection_key.as_deref() == Some(conn_key.as_str())),
        "{:?}",
        events
            .iter()
            .map(|row| (&row.kind, &row.connection_key))
            .collect::<Vec<_>>()
    );
    assert!(
        !events.iter().any(|row| row.kind == "plc_disconnected"),
        "Live のままなら切断は記録されない"
    );

    hub.shutdown().await;
    server.abort();
}

// --- P5: 切断→再接続・bit タグ・しきい値 ------------------------------------

/// モック Hub に出させる 1 本のタグ（外部名・データ型・値・出所）。品質は good。
struct HubSpec {
    name: &'static str,
    data_type: &'static str,
    value: f64,
    source: ValueSource,
}

fn spec(name: &'static str, data_type: &'static str, value: f64, source: ValueSource) -> HubSpec {
    HubSpec {
        name,
        data_type,
        value,
        source,
    }
}

/// `specs` を catalog と values に載せ、WS は先頭のタグの同じ値を 1 つ送る
/// モック Hub。
fn mock_hub_from(specs: &[HubSpec]) -> MockHub {
    let catalog = CatalogSnapshot {
        revision: 1,
        run_id: Some(1),
        collection_mode: CollectionMode::Configured,
        tags: specs
            .iter()
            .enumerate()
            .map(|(index, s)| {
                let mut tag = catalog_tag(s.name, index as i64 + 1);
                tag.data_type = s.data_type.into();
                tag
            })
            .collect(),
    };
    let values = ValuesSnapshot {
        revision: catalog.revision,
        t: 1_000,
        run_id: catalog.run_id,
        collection_mode: catalog.collection_mode.clone(),
        values: specs
            .iter()
            .map(|s| entry(s.name, s.value, ValueQuality::Good, s.source.clone()))
            .collect(),
    };
    let frame = format!(
        concat!(
            r#"{{"op":"data","id":{},"t":2000,"#,
            r#""values":[{{"tag":"{}","v":{},"q":"good","t":2000}}]}}"#
        ),
        SUBSCRIPTION_ID, specs[0].name, specs[0].value
    );
    MockHub {
        catalog: serde_json::to_string(&catalog).unwrap(),
        values: serde_json::to_string(&values).unwrap(),
        frame,
    }
}

/// Hub に手動キーで接続し、`specs` を登録して、`attach_hub` 済みの
/// `CollectorService` を持つ（まだ開始しない）。
struct Fixture {
    _dir: TempDir,
    pool: sqlx::SqlitePool,
    data_dir: PathBuf,
    hub: HubService,
    svc: CollectorService,
    conn_key: String,
    group_key: String,
    /// `specs` と同じ順のタグ id。
    ids: Vec<i64>,
}

impl Fixture {
    async fn new(endpoint: &str, specs: &[HubSpec]) -> Self {
        let dir = TempDir::new();
        let pool = init_db(dir.path().join("chronogazer.sqlite3"))
            .await
            .expect("設定 DB を初期化できる");
        let data_dir = resolve_data_dir(dir.path(), "./data");
        let settings = SettingsService::new(Db::Sqlite(pool.clone()));
        let keys: Arc<dyn KeyStore> = Arc::new(MemoryKeyStore::new());
        let hub = HubService::new(settings, keys)
            .await
            .expect("HubService を組める");
        let view = hub
            .adopt_manual_key(endpoint, "bh_abcd1234_opaque-secret".to_string())
            .await
            .expect("モック Hub に接続できる");
        assert!(
            matches!(view.status, HubStatus::Connected { .. }),
            "{:?}",
            view.status
        );
        let pairs: Vec<(&str, &str)> = specs.iter().map(|s| (s.name, s.data_type)).collect();
        let (conn_id, group_id, ids) = register_hub_specs(&pool, &pairs).await;
        let svc = CollectorService::new(
            pool.clone(),
            data_dir.clone(),
            chronogazer_core::engine::EngineConfig::builtin_drivers(),
        );
        assert!(svc.attach_hub(hub.clone()));
        Self {
            _dir: dir,
            pool,
            data_dir,
            hub,
            svc,
            conn_key: format!("conn:{conn_id}"),
            group_key: format!("grp:{group_id}"),
            ids,
        }
    }

    fn tag_key(&self, index: usize) -> String {
        format!("tag:{}", self.ids[index])
    }

    /// この接続の `plc_*` イベントの種別（古い順）。
    async fn link_events(&self) -> Vec<String> {
        match self.svc.events(EventPage::default()).await {
            Readout::Ready { data } => {
                let mut rows: Vec<_> = data
                    .rows
                    .into_iter()
                    .filter(|row| {
                        row.kind.starts_with("plc_")
                            && row.connection_key.as_deref() == Some(self.conn_key.as_str())
                    })
                    .collect();
                rows.sort_by_key(|row| row.id);
                rows.into_iter().map(|row| row.kind).collect()
            }
            other => panic!("イベント一覧が読めない: {}", other.as_str()),
        }
    }

    async fn wait_good(&self, index: usize, expected: f64) {
        let key = self.tag_key(index);
        wait_until("現在値が good で Hub の値になる", || {
            let response = self.svc.values_response();
            let key = key.clone();
            async move {
                let Readout::Ready { data } = &response.readout else {
                    return Err(response.readout.as_str().to_string());
                };
                match data.get(&key) {
                    Some(s) if s.quality == QualityView::Good && s.value == Some(expected) => {
                        Ok(())
                    }
                    other => Err(format!("{other:?}")),
                }
            }
        })
        .await;
    }

    async fn finish(self) {
        self.hub.shutdown().await;
        self.pool.close().await;
    }
}

async fn bind_with_retry(port: u16) -> TcpListener {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match TcpListener::bind(("127.0.0.1", port)).await {
            Ok(listener) => return listener,
            Err(err) if Instant::now() >= deadline => {
                panic!("ポート {port} を取り直せなかった: {err}")
            }
            Err(_) => tokio::time::sleep(POLL_INTERVAL).await,
        }
    }
}

/// Hub が落ちて戻る: 収集の接続イベントが **Hub の接続キー**で
/// `plc_connected` → `plc_disconnected` → `plc_reconnected` と 1 回ずつ出て、
/// 落ちている間は現在値が good でなくなり、戻ると good に戻る。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hub_drop_and_return_emit_link_events_under_the_hub_connection_key() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let endpoint = format!("http://127.0.0.1:{port}");
    let hub_data = Arc::new(mock_hub_from(&[spec(
        REAL,
        "f64",
        1.0,
        ValueSource::Simulation,
    )]));
    let mut server = tokio::spawn(serve_hub(listener, hub_data.clone()));

    let fx = Fixture::new(
        &endpoint,
        &[spec(REAL, "f64", 1.0, ValueSource::Simulation)],
    )
    .await;
    fx.svc.start().await.expect("収集を開始できる");
    fx.wait_good(0, 1.0).await;
    wait_until("plc_connected が出る", || async {
        let events = fx.link_events().await;
        if events == ["plc_connected"] {
            Ok(())
        } else {
            Err(format!("{events:?}"))
        }
    })
    .await;

    // Hub を落とす（開いている WS も閉じる）。
    server.abort();
    let _ = (&mut server).await;
    wait_until("plc_disconnected が出る", || async {
        let events = fx.link_events().await;
        if events.last().map(String::as_str) == Some("plc_disconnected") {
            Ok(())
        } else {
            Err(format!("{events:?}"))
        }
    })
    .await;
    match fx.svc.values_response().readout {
        Readout::Ready { data } => assert_ne!(
            data[&fx.tag_key(0)].quality,
            QualityView::Good,
            "Hub が落ちている間は good ではない"
        ),
        other => panic!("{}", other.as_str()),
    }

    // 同じポートで Hub を戻す。
    let listener = bind_with_retry(port).await;
    let server = tokio::spawn(serve_hub(listener, hub_data));
    wait_until("plc_reconnected が出る", || async {
        let events = fx.link_events().await;
        if events.last().map(String::as_str) == Some("plc_reconnected") {
            Ok(())
        } else {
            Err(format!("{events:?}"))
        }
    })
    .await;
    fx.wait_good(0, 1.0).await;

    fx.svc.stop().await.expect("停止できる");
    assert_eq!(
        fx.link_events().await,
        ["plc_connected", "plc_disconnected", "plc_reconnected"],
        "接続・切断・再接続が 1 回ずつ、この順で"
    );
    server.abort();
    fx.finish().await;
}

/// bit タグ: Hub の `bit` の値 1 / 0 がそのまま記録される。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hub_bit_tags_are_recorded_as_zero_and_one() {
    let specs = [
        spec("PLC9.G.on", "bit", 1.0, ValueSource::Real),
        spec("PLC9.G.off", "bit", 0.0, ValueSource::Simulation),
    ];
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(serve_hub(listener, Arc::new(mock_hub_from(&specs))));

    let fx = Fixture::new(&endpoint, &specs).await;
    let outcome = fx.svc.start().await.expect("収集を開始できる");
    assert!(!outcome.pending, "{:?}", outcome.status);
    match fx.svc.state() {
        CollectorState::Running {
            tags, exclusions, ..
        } => assert_eq!((tags, exclusions.len()), (2, 0), "{exclusions:?}"),
        other => panic!("Running を期待したが {other:?}"),
    }
    fx.wait_good(0, 1.0).await;
    fx.wait_good(1, 0.0).await;
    wait_until("bit の 1 / 0 が毎周期記録される", || {
        let data_dir = fx.data_dir.clone();
        let group_key = fx.group_key.clone();
        async move {
            let rows = stored_rows(&data_dir, &group_key).await;
            let recorded = rows
                .iter()
                .filter(|row| row == &&vec![Some(1.0), Some(0.0)])
                .count();
            if recorded >= 5 {
                Ok(())
            } else {
                Err(format!("{recorded} 行 / {rows:?}"))
            }
        }
    })
    .await;
    fx.svc.stop().await.expect("停止できる");
    server.abort();
    fx.finish().await;
}

/// Hub 経由のタグに記録計の側のしきい値（`recorder_tag_settings`）を設定
/// すると、`value_source` が simulation / derived_simulation の値でも判定
/// されてイベントが出て、値も記録される。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn thresholds_judge_hub_values_whatever_their_value_source() {
    let specs = [
        spec("PLC9.G.hot", "f64", 5.0, ValueSource::Simulation),
        spec("PLC9.G.cold", "f64", 5.0, ValueSource::DerivedSimulation),
    ];
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(serve_hub(listener, Arc::new(mock_hub_from(&specs))));

    let fx = Fixture::new(&endpoint, &specs).await;
    let thresholds = TagThresholdService::new(fx.pool.clone());
    // hot: 5 は H = 3 を超える。cold: 5 は L = 10 を下回る。
    thresholds
        .update(
            fx.ids[0],
            &TagThresholdsPayload {
                threshold_h: Some(3.0),
                expected_revision: Some(0),
                ..TagThresholdsPayload::default()
            },
        )
        .await
        .expect("しきい値を保存できる");
    thresholds
        .update(
            fx.ids[1],
            &TagThresholdsPayload {
                threshold_l: Some(10.0),
                expected_revision: Some(0),
                ..TagThresholdsPayload::default()
            },
        )
        .await
        .expect("しきい値を保存できる");

    fx.svc.start().await.expect("収集を開始できる");
    fx.wait_good(0, 5.0).await;

    type EventSummary = (String, String, Option<String>, Option<f64>, Option<f64>);
    let summary: Vec<EventSummary> =
        wait_until("しきい値のイベントが 2 件出る", || async {
            match fx.svc.events(EventPage::default()).await {
                Readout::Ready { data } => {
                    let mut rows: Vec<EventSummary> = data
                        .rows
                        .into_iter()
                        .filter(|row| row.kind.starts_with("threshold_"))
                        .map(|row| {
                            (
                                row.kind,
                                row.tag_key.unwrap_or_default(),
                                row.level,
                                row.value,
                                row.limit_value,
                            )
                        })
                        .collect();
                    rows.sort_by(|a, b| a.1.cmp(&b.1));
                    if rows.len() >= 2 {
                        Ok(rows)
                    } else {
                        Err(format!("{rows:?}"))
                    }
                }
                other => Err(other.as_str().to_string()),
            }
        })
        .await;
    let mut expected: Vec<EventSummary> = vec![
        (
            "threshold_entered".to_string(),
            fx.tag_key(0),
            Some("H".to_string()),
            Some(5.0),
            Some(3.0),
        ),
        (
            "threshold_entered".to_string(),
            fx.tag_key(1),
            Some("L".to_string()),
            Some(5.0),
            Some(10.0),
        ),
    ];
    expected.sort_by(|a, b| a.1.cmp(&b.1));
    assert_eq!(summary, expected, "simulation 由来でも判定される");

    wait_until("simulation 由来の値も記録される", || {
        let data_dir = fx.data_dir.clone();
        let group_key = fx.group_key.clone();
        async move {
            let recorded = stored_rows(&data_dir, &group_key)
                .await
                .iter()
                .filter(|row| row == &&vec![Some(5.0), Some(5.0)])
                .count();
            if recorded >= 5 {
                Ok(())
            } else {
                Err(format!("{recorded} 行"))
            }
        }
    })
    .await;
    fx.svc.stop().await.expect("停止できる");
    server.abort();
    fx.finish().await;
}
