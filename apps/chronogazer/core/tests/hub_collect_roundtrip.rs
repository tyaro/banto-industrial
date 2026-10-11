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
//! * (b) 現在値: Hub の good の値は `good`、`simulation` / `derived_simulation`
//!   の値も表示はされ（#413）、Hub の `stale` は `stale`、外したタグは
//!   `invalid`、
//! * (c) データファイル: 実値（`real`）だけが**毎周期**記録され、
//!   シミュレーションの 2 種と stale は null（H4・#413）、
//! * (d) `collect_events` に Hub の接続キーの `plc_connected`。
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
        tags: [REAL, SIM, DSIM, STALE]
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
async fn serve_hub(listener: TcpListener, hub: Arc<MockHub>) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        let head = peek_head(&stream).await;
        if head.contains("upgrade: websocket") {
            let frame = hub.frame.clone();
            tokio::spawn(async move {
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
/// `(接続 id, グループ id, [REAL, SIM, DSIM, STALE, GONE] のタグ id)` を返す。
async fn register_hub_tags(pool: &sqlx::SqlitePool) -> (i64, i64, Vec<i64>) {
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
            database: None,
            username: None,
            password: None,
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
    for name in [REAL, SIM, DSIM, STALE, GONE] {
        let tag = tags
            .create(TagInput {
                name: name.rsplit('.').next().unwrap().to_string(),
                collection_group_id: group.id,
                address: name.to_string(),
                data_type: "f64".to_string(),
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

    let svc = CollectorService::new(pool.clone(), data_dir.clone());
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
            assert_eq!((groups, tags), (1, 4));
            let excluded: Vec<(&str, &str)> = exclusions
                .iter()
                .map(|e| (e.key.as_str(), e.reason))
                .collect();
            assert_eq!(excluded, vec![(key(4).as_str(), "hubTagNotFound")]);
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
        "simulation は表示はされる"
    );
    assert_eq!(
        (values[&key(2)].value, values[&key(2)].quality),
        (Some(3.0), QualityView::Good),
        "derived_simulation も表示はされる"
    );
    assert_eq!(
        (values[&key(3)].value, values[&key(3)].quality),
        (Some(4.0), QualityView::Stale)
    );
    assert_eq!(values[&key(4)].quality, QualityView::Invalid);

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

    // (c) データファイル: 実値だけが記録され、シミュレーションの 2 種と
    // stale は null。外したタグには列が無い（4 列）。
    let rows = stored_rows(&data_dir, &group_key).await;
    let live_rows: Vec<&Vec<Option<f64>>> = rows.iter().filter(|row| row[0].is_some()).collect();
    assert!(live_rows.len() >= 10, "{rows:?}");
    for row in &rows {
        assert_eq!(row.len(), 4, "{row:?}");
        assert_eq!(&row[1..], &[None, None, None], "{row:?}");
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
