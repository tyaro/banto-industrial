//! T5-4（docs/t5-handoff.md §3「ソークテスト」・docs/tag-server-design.md
//! §8「配布・運用」）: 収集 + WebSocket 購読 + MQTT 発行を**同時に**維持した
//! 連続稼働試験。出荷条件のそもそもの一次ソースは
//! docs/recorder-requirements.md §4「非機能要件」の
//! 「模擬 PLC 相手に 72 時間連続収集、欠測ゼロ（意図的断線区間を除く）・
//! メモリ増加なし」（ChronoGazer 向けに定義されたもの）。tag-server-design.md
//! §8 はこれを banto-hub 向けに拡張し、「収集 24/365 + 外部クライアント購読を
//! 維持した状態での連続稼働試験（banto-collect の 72h ソーク雛形を流用）」と
//! している。
//!
//! ## 雛形との関係
//!
//! `crates/banto-collect/tests/integration.rs` の
//! `mini_soak_100ms_three_groups_row_counts_within_tolerance`（通常 CI で毎回
//! 走る短時間版）/ `long_soak_sixty_seconds`（`#[ignore]`、将来の 72h
//! リリースゲートの種）と同じ設計思想を踏襲する:
//!
//! - 通常の `cargo test` では短時間版（[`mini_soak_collect_ws_mqtt_stay_alive`]）
//!   だけが走る。
//! - 長時間版（[`long_soak_collect_ws_mqtt_stay_alive`]）は `#[ignore]` 付き。
//!   `cargo test -p banto-hub-core --test soak -- --ignored` で明示実行する。
//!   既定の実行時間は 60 秒（このセッションで実際に動作確認できる長さ） -
//!   `SOAK_DURATION_SECS` 環境変数で上書きできる。**本番の 72h ソーク**は
//!   `SOAK_DURATION_SECS=259200`（72*3600）を指定して明示実行する
//!   （手順は docs/banto-hub-operations.md「72h 出荷判定ソークテスト」節に
//!   も記載 - オーナーが出荷判定として別途実施する。72h 通しの実行自体は
//!   このタスクのスコープ外）。
//! - タイミングの許容範囲は banto-collect の雛形と同じく緩め: CI が詰まって
//!   ティックを取りこぼすことはあっても、バーストで余分に受信することは
//!   ない設計（`MissedTickBehavior::Skip`/`Delay`）なので、下限だけを
//!   チェックする。
//!
//! ## banto-collect との違い: 3経路を同時に維持する
//!
//! banto-collect の雛形は収集エンジン単体（tstore への行数）しか見ていない。
//! banto-hub は PLC 収集に加えて WebSocket 購読（`crate::stream`）と MQTT
//! publish（`crate::mqtt`）を常時起動したまま、3経路それぞれで「値が来続けて
//! いる」ことを検証する必要がある（設計 §8「収集 + 外部クライアント購読を
//! 維持」・実装指示「収集 + WebSocket 購読 + MQTT 発行を維持した連続稼働」）。
//!
//! - **収集**: `manager.shutdown()`（flush 相当）後に tstore ファイルを直接
//!   読んで行数を数える（`crates/banto-collect/tests/integration.rs` の
//!   `read_single_group_rows`と同じ手法）。
//! - **WebSocket**: `mode: "interval"`・`interval_ms: 250`
//!   （`crate::subscribe_core::EVAL_TICK_MS`の下限クランプそのもの、
//!   `tests/stream.rs::interval_mode_sends_data_on_a_schedule_even_without_changes`
//!   参照）で購読する - 値が変化したかどうかに関係なく評価ループの周期で
//!   必ず届くので、後述のレジスタ変更タイミングに依存しない安定したカウント
//!   になる。
//! - **MQTT**: `crate::mqtt` の発行モードは on_change 一択（タグ毎の
//!   interval モードは T3 で未実装 - `crate::mqtt` のモジュール doc comment
//!   参照）なので、代わりにテスト側がソーク走行中ずっとレジスタ値を
//!   単調増加させ続け（収集周期と同じ 100ms 間隔）、250ms の MQTT 評価
//!   ループが毎ティック「前回発行値と違う」を検知できるようにする。
//!
//! ## メモリ増加の検証（実装判断）
//!
//! `working_set_bytes()`（[`mem_probe`]）でこのテストプロセス自身の RSS を
//! ソーク走行中サンプリングし、開始/終了値を `println!` でログする
//! （`--nocapture`で可視）。banto-hub は Windows 専用前提（tag-server-design.md
//! §8「Windows 専用前提は tstore と同じ」）なので Win32
//! `GetProcessMemoryInfo`を直接呼ぶ薄い FFI ラッパーとし、新規 crate
//! 依存は追加していない（tokio/mio/tempfile 等が既に`windows-sys 0.61`を
//! 依存木に持っているため、`[target.'cfg(windows)'.dev-dependencies]`への
//! 追加は新しい依存ノードを増やさない - `Cargo.toml`のコメント参照）。
//! 非 Windows ビルド（`cargo check --workspace`等のクロスプラットフォーム
//! カバレッジ用）は`None`を返すフォールバックにしてある。
//!
//! ただし**このログはあくまで診断用の観測であって、合否判定には使わない**
//! （60秒程度の走行時間ではアロケータのウォームアップ/断片化の揺らぎを
//! リーク兆候と誤検知しかねない）。「メモリ増加なし」という出荷条件の
//! 実際の判定は、72h 本番ソーク走行中にタスクマネージャ/`Get-Process`で
//! 目視確認する運用手順として docs/banto-hub-operations.md に記載した -
//! このリポジトリの既存流儀（CI: タイミングの緩い自動チェック / 実機・
//! 長時間: オーナー実施の手順書）に合わせた判断。
//!
//! ## 欠測ゼロの検証
//!
//! 収集行数・WS `data`受信数・MQTT publish 受信数それぞれについて、経過
//! 時間と周期から求めた理論値に対する緩い下限だけを assert する
//! （banto-collect の`long_soak_sixty_seconds`「理論値600に対し500超えている
//! こと」と同じ考え方 - 短時間版は CI 揺らぎを見込んでさらに緩くしてある）。
//!
//! ## ヘルパーの重複について
//!
//! `tests/stream.rs`/`tests/mqtt.rs`と重なるヘルパー（`fast_options`・
//! `wait_until`・rumqttd 起動一式・`LiveSubscriber`等）はこのファイルにも
//! 複製している - 各`tests/*.rs`は独立バイナリとしてコンパイルされ、
//! private helper を共有できないため（両ファイルのモジュール doc comment に
//! 同じ注記がある）。`TempEnv`は`tests/common/mod.rs`に集約済み
//! （2026-08-08、テスト一時ディレクトリリークの根治）。

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use banto_collect::{BackoffConfig, CollectorOptions};
use banto_hub_core::api_keys::ApiKeysService;
use banto_hub_core::audit::AuditLogService;
use banto_hub_core::broker_glue::{HubSessions, SlmpSimRegistry};
use banto_hub_core::computed::{ComputedEngine, ServerTagStore};
use banto_hub_core::db::init_db;
use banto_hub_core::grpc::{GrpcServer, GrpcService};
use banto_hub_core::hub::CollectorManager;
use banto_hub_core::mqtt::MqttPublisher;
use banto_hub_core::rest::api_router;
use banto_hub_core::settings::MqttSettings;
use banto_hub_core::users::UsersService;
use banto_hub_core::write_audit::WriteAuditService;
use banto_hub_core::write_control::WriteControl;
use banto_hub_core::write_rate::{WriteRateLimitConfig, WriteRateLimiter};
use banto_plc::modbus::simulator::Simulator;
use banto_server::{start, AuthState, Identity, ServerConfig};
use banto_tags::{
    CollectionGroupInput, CollectionGroupService, PlcConnectionInput, PlcConnectionService,
    TagInput, TagService,
};
use banto_tstore::{SystemClock, TsReader};
use futures_util::{SinkExt, StreamExt};
use rumqttc::{AsyncClient, Event, MqttOptions, Packet, QoS};
use serde_json::{json, Value};
use sqlx::SqlitePool;
use tokio::sync::{broadcast, Mutex as AsyncMutex};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

type WsStream = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

// ---------------------------------------------------------------------------
// メモリサンプリング（このファイルのモジュール doc comment「メモリ増加の
// 検証」参照）
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod mem_probe {
    //! `GetProcessMemoryInfo`（psapi.dll、`Cargo.toml`の
    //! `[target.'cfg(windows)'.dev-dependencies]`に追加した`windows-sys`
    //! 経由）でこのプロセス自身の working set（RSS 相当）を読む、診断専用の
    //! 薄いラッパー。呼び出し失敗時は`None`を返す（panic しない - 合否判定
    //! には使わない値であるため、失敗しても呼び出し側のソーク走行自体を
    //! 止める理由にはならない）。

    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    pub fn working_set_bytes() -> Option<u64> {
        unsafe {
            let mut counters: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
            counters.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
            let process: HANDLE = GetCurrentProcess();
            let ok = GetProcessMemoryInfo(process, &mut counters, counters.cb);
            if ok != 0 {
                Some(counters.WorkingSetSize as u64)
            } else {
                None
            }
        }
    }
}

#[cfg(not(windows))]
mod mem_probe {
    //! banto-hub は Windows 専用前提（tag-server-design.md §8「Windows
    //! 専用前提は tstore と同じ」）- 非 Windows は`cargo check --workspace`
    //! 等のクロスプラットフォームカバレッジのためにビルドが通ればよく、
    //! メモリサンプリング自体は常に利用不可（`None`）でよい。

    pub fn working_set_bytes() -> Option<u64> {
        None
    }
}

// ---------------------------------------------------------------------------
// テスト用足場（`tests/stream.rs`/`tests/mqtt.rs`と重複 - このファイルの
// モジュール doc comment「ヘルパーの重複について」参照）
// ---------------------------------------------------------------------------

mod common;
use common::TempEnv;

/// Temp-dir prefix passed to `TempEnv::new` (see `tests/common/mod.rs`).
const TEMP_ENV_PREFIX: &str = "banto-hub-soak-it";

/// PLC 収集の周期(ms)。WS の`interval_ms`下限クランプ（250ms、
/// `EVAL_TICK_MS`）より十分速く、かつソーク走行中のレジスタ変更ティック
/// （[`run_soak`]参照）とも一致させる。
const PERIOD_MS: i64 = 100;

/// WS `mode: "interval"`/MQTT on_change 評価ループの固定周期
/// （`crate::subscribe_core::EVAL_TICK_MS`/`crate::mqtt`の同名定数と同じ値 -
/// このテストは`banto-hub-core`のプライベート定数を見られないので値だけ
/// 複製する）。
const EVAL_TICK_MS: i64 = 250;

fn fast_options() -> CollectorOptions {
    CollectorOptions {
        backoff: BackoffConfig {
            base: Duration::from_millis(20),
            max: Duration::from_millis(100),
        },
        connect_timeout: Duration::from_millis(500),
        response_timeout: Duration::from_millis(500),
        writer_options: banto_tstore::WriterOptions {
            max_buffered_rows: 1,
            flush_interval_ms: 0,
        },
    }
}

fn conn_input(name: &str, port: u16) -> PlcConnectionInput {
    PlcConnectionInput {
        name: name.to_string(),
        protocol: "modbus-tcp".to_string(),
        host: "127.0.0.1".to_string(),
        port: port as i64,
        unit_id: 1,
        enabled: true,
        simulation: false,
    }
}

fn group_input(name: &str, conn_id: i64, period_ms: i64) -> CollectionGroupInput {
    CollectionGroupInput {
        name: name.to_string(),
        plc_connection_id: conn_id,
        period_ms,
        enabled: true,
    }
}

fn tag_input(name: &str, group_id: i64, address: &str, data_type: &str) -> TagInput {
    TagInput {
        name: name.to_string(),
        collection_group_id: group_id,
        address: address.to_string(),
        data_type: data_type.to_string(),
        string_length: None,
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
    }
}

async fn wait_until<F, Fut>(timeout: Duration, mut predicate: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if predicate().await {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// `crates/banto-collect/tests/integration.rs::read_single_group_rows`と同じ
/// 手法: `stop()`/`manager.shutdown()`後(flush 済み)に tstore ファイルを直接
/// 開いて全行を読む。
async fn read_single_group_rows(data_dir: &Path) -> Vec<banto_tstore::Sample> {
    let files = banto_tstore::list_data_files(data_dir).expect("list files");
    assert_eq!(files.len(), 1, "expected exactly one data file");
    let reader = TsReader::open(&files[0].path).await.expect("open reader");
    let group_key = reader.groups()[0].key.clone();
    reader
        .read_range(&group_key, 0, i64::MAX)
        .await
        .expect("read range")
}

// --- in-process MQTT ブローカー(rumqttd) - `tests/mqtt.rs`から複製 ---------

fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind a free port");
    listener.local_addr().expect("local_addr").port()
}

fn rumqttd_config(port: u16) -> rumqttd::Config {
    let router = rumqttd::RouterConfig {
        max_connections: 100,
        max_outgoing_packet_count: 200,
        max_segment_size: 1024 * 1024,
        max_segment_count: 10,
        ..Default::default()
    };
    let mut v4 = std::collections::HashMap::new();
    v4.insert(
        "1".to_string(),
        rumqttd::ServerSettings {
            name: "v4-1".to_string(),
            listen: format!("127.0.0.1:{port}")
                .parse()
                .expect("valid listen addr"),
            tls: None,
            next_connection_delay_ms: 1,
            connections: rumqttd::ConnectionSettings {
                connection_timeout_ms: 5000,
                max_payload_size: 20480,
                max_inflight_count: 100,
                auth: None,
                external_auth: None,
                dynamic_filters: true,
            },
        },
    );
    rumqttd::Config {
        id: 0,
        router,
        v4: Some(v4),
        v5: None,
        ws: None,
        cluster: None,
        console: None,
        bridge: None,
        prometheus: None,
        metrics: None,
    }
}

async fn start_test_broker() -> u16 {
    for attempt in 1..=3 {
        let port = free_port();
        let mut broker = rumqttd::Broker::new(rumqttd_config(port));
        std::thread::spawn(move || {
            let _ = broker.start();
        });

        let up = wait_until(Duration::from_secs(4), || async move {
            tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .is_ok()
        })
        .await;
        if up {
            return port;
        }
        eprintln!(
            "banto-hub soak test: rumqttd がポート {port} で起動確認できません(試行 {attempt}/3) - 新しいポートで再試行します"
        );
    }
    panic!("rumqttd が3回の試行後も起動しませんでした");
}

/// `tests/mqtt.rs::LiveSubscriber`と同じ: 1本の接続を張りっぱなしにして
/// 届いた順に蓄積する - ソーク走行中「今何件届いているか」をポーリング
/// できるようにする。
struct LiveSubscriber {
    messages: Arc<AsyncMutex<Vec<(String, String)>>>,
    _task: tokio::task::JoinHandle<()>,
}

impl LiveSubscriber {
    async fn subscribe(port: u16, client_id: &str, filter: &str) -> Self {
        let mut options = MqttOptions::new(client_id, "127.0.0.1", port);
        options.set_keep_alive(Duration::from_secs(5));
        let (client, mut eventloop) = AsyncClient::new(options, 64);
        client
            .subscribe(filter, QoS::AtLeastOnce)
            .await
            .expect("subscribe");

        let messages = Arc::new(AsyncMutex::new(Vec::new()));
        let messages_for_task = messages.clone();
        let task = tokio::spawn(async move {
            loop {
                match eventloop.poll().await {
                    Ok(Event::Incoming(Packet::Publish(publish))) => {
                        messages_for_task.lock().await.push((
                            publish.topic,
                            String::from_utf8_lossy(&publish.payload).to_string(),
                        ));
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
        });
        Self {
            messages,
            _task: task,
        }
    }

    async fn count(&self) -> usize {
        self.messages.lock().await.len()
    }
}

// --- hub テストアプリ(実サーバー) - `tests/stream.rs`から複製 --------------

struct TestApp {
    // `Option`, not a bare `RunningServer`: `run_soak` needs to move it out
    // (via `.take()`) to call `RunningServer::stop(self)` - TestApp
    // implementing `Drop` (below) forbids partially moving a field out of
    // it otherwise (`error[E0509]`).
    server: Option<banto_server::RunningServer>,
    token: String,
    pool: SqlitePool,
    manager: Arc<CollectorManager>,
    mqtt: Arc<MqttPublisher>,
    env: TempEnv,
}

// See `tests/common/mod.rs`'s module doc ("Why `TestApp` also needs
// `shutdown_test_app`") for why this is required, not optional. This file
// already calls `app.manager.shutdown()` explicitly at the end of its own
// tests (see the module doc's "ヘルパーの重複について" / soak flow) - this
// `Drop` impl makes that shutdown idempotent (a second `shutdown()` call is
// a harmless no-op) and covers any path that doesn't call it explicitly.
impl Drop for TestApp {
    fn drop(&mut self) {
        common::shutdown_test_app(&self.manager, &self.pool);
    }
}

impl TestApp {
    fn ws_url(&self, path: &str) -> String {
        format!(
            "ws://127.0.0.1:{}{path}",
            self.server
                .as_ref()
                .expect("server not yet stopped")
                .local_addr()
                .port()
        )
    }
}

async fn test_app(label: &str) -> TestApp {
    let env = TempEnv::new(TEMP_ENV_PREFIX, label);
    let pool = init_db(env.registry_path()).await.expect("init_db");

    let users = UsersService::new(pool.clone());
    let audit = AuditLogService::new(pool.clone());
    users
        .setup_first_user("admin", "password123", "管理者")
        .await
        .expect("setup_first_user");

    let verify_users = users.clone();
    let auth = AuthState::new(move |u: String, p: String| {
        let users = verify_users.clone();
        Box::pin(async move {
            match users.verify(&u, &p).await {
                Ok(Some(identity)) => Some(Identity {
                    id: identity.username,
                    name: identity.display_name,
                    role: identity.role.to_string(),
                }),
                _ => None,
            }
        })
    });
    let token = auth
        .login("admin", "password123")
        .await
        .expect("admin login");

    let sessions = Arc::new(HubSessions::new(banto_broker::BackoffConfig::default()));
    let sim_registry = Arc::new(SlmpSimRegistry::new());
    let computed = Arc::new(ComputedEngine::new(Arc::new(ServerTagStore::new())));
    let manager = Arc::new(CollectorManager::new(
        pool.clone(),
        env.data_dir(),
        Arc::new(SystemClock),
        fast_options(),
        sessions,
        sim_registry,
        computed,
    ));
    manager.rebuild().await.expect("initial rebuild");

    let api_keys = ApiKeysService::new(pool.clone());
    let (events_tx, _rx) = broadcast::channel(16);
    let write_control = Arc::new(WriteControl::new(false));
    let write_audit = WriteAuditService::new(pool.clone());
    let mqtt = Arc::new(MqttPublisher::new(manager.clone()));
    let rate_limiter = Arc::new(AsyncMutex::new(WriteRateLimiter::new(
        WriteRateLimitConfig::default(),
    )));
    let grpc_service = GrpcService::new(
        manager.clone(),
        api_keys.clone(),
        audit.clone(),
        write_audit.clone(),
        write_control.clone(),
        rate_limiter.clone(),
        events_tx.clone(),
    );
    let grpc_server = Arc::new(GrpcServer::new(grpc_service));
    let router: Router = api_router(
        users,
        audit,
        PlcConnectionService::new(pool.clone()),
        CollectionGroupService::new(pool.clone()),
        TagService::new(pool.clone()),
        api_keys,
        manager.clone(),
        auth,
        events_tx,
        false,
        write_control,
        write_audit,
        // Router 側は自分の Arc クローンを持つだけで足りる - 呼び出し側
        // (`TestApp.mqtt`)は元の `mqtt` を握り続けて `apply()`/`connected()`
        // を直接叩く(REST の `PUT /api/mqtt-settings` を経由する必要はない
        // - `RunningServer` は router を消費してしまうので、そちらの経路は
        // ここでは使えない)。
        mqtt.clone(),
        grpc_server,
        rate_limiter,
    );

    let server = start(
        ServerConfig {
            bind: "127.0.0.1".to_string(),
            port: 0,
        },
        router,
    )
    .await
    .expect("server should start");

    TestApp {
        server: Some(server),
        token,
        pool,
        manager,
        mqtt,
        env,
    }
}

// --- WS クライアントヘルパー(`tests/stream.rs`から複製、必要分のみ) -------

async fn connect_ws(
    url: &str,
    token: &str,
) -> Result<WsStream, tokio_tungstenite::tungstenite::Error> {
    let mut request = url.into_client_request().expect("valid ws url");
    request.headers_mut().insert(
        "Authorization",
        format!("Bearer {token}")
            .parse()
            .expect("valid header value"),
    );
    let (stream, _response) = connect_async(request).await?;
    Ok(stream)
}

async fn send_json(ws: &mut WsStream, value: Value) {
    ws.send(WsMessage::Text(value.to_string().into()))
        .await
        .expect("ws send should succeed");
}

async fn recv_matching(ws: &mut WsStream, predicate: impl Fn(&Value) -> bool) -> Value {
    tokio::time::timeout(Duration::from_secs(6), async {
        loop {
            match ws.next().await {
                Some(Ok(WsMessage::Text(text))) => {
                    let value: Value =
                        serde_json::from_str(&text).expect("server should send valid JSON");
                    if predicate(&value) {
                        return value;
                    }
                }
                Some(Ok(WsMessage::Ping(_))) | Some(Ok(WsMessage::Pong(_))) => continue,
                Some(Ok(other)) => panic!("unexpected non-text ws message: {other:?}"),
                Some(Err(err)) => panic!("ws error while waiting for a message: {err}"),
                None => panic!("connection closed while waiting for a message"),
            }
        }
    })
    .await
    .expect("timed out waiting for the expected ws message")
}

// ---------------------------------------------------------------------------
// ソーク本体
// ---------------------------------------------------------------------------

/// 1回のソーク走行の結果 - 収集(tstore 行数)・WS(`data`受信数)・MQTT
/// (publish 受信数)・RSS サンプル(診断用、[`mem_probe`]参照)。
struct SoakReport {
    elapsed: Duration,
    collected_rows: usize,
    ws_data_messages: u64,
    mqtt_publishes: usize,
    rss_start_bytes: Option<u64>,
    rss_end_bytes: Option<u64>,
}

/// PLC 収集(banto-plc の Modbus TCP シミュレータ相手)+ WS 購読(`mode:
/// "interval"`) + MQTT publish(on_change)を`duration`だけ同時に維持し、
/// 3経路それぞれの受信数と RSS サンプルを集めて返す。このファイルのモジュール
/// doc comment「banto-collect との違い」参照。
async fn run_soak(label: &str, duration: Duration) -> SoakReport {
    let broker_port = start_test_broker().await;
    let mut app = test_app(label).await;
    let sim = Simulator::start().await;
    sim.set_holding_register(0, 0);

    let conn = PlcConnectionService::new(app.pool.clone())
        .create(conn_input("line1", sim.addr.port()))
        .await
        .unwrap();
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn.id, PERIOD_MS))
        .await
        .unwrap();
    TagService::new(app.pool.clone())
        .create(tag_input("temp01", group.id, "40001", "i16"))
        .await
        .unwrap();
    app.manager.rebuild().await.expect("rebuild after seeding");

    assert!(
        wait_until(Duration::from_secs(6), || async {
            app.manager
                .current_values()
                .and_then(|c| c.get("tag:1"))
                .is_some()
        })
        .await,
        "collector should observe the initial value before the soak run starts"
    );

    // MQTT を直接 apply する(REST の PUT を経由しない理由は `test_app`の
    // 対応コメント参照)。
    let mqtt_settings = MqttSettings {
        enabled: true,
        host: "127.0.0.1".to_string(),
        port: broker_port,
        client_id: format!("hub-soak-{label}"),
        username: None,
        password: None,
        prefix: "banto".to_string(),
        qos: 1,
        // EVAL_TICK_MS(250ms)より確実に短くしておき、スロットルではなく
        // 評価ループの周期そのものが発行頻度の律速になるようにする(この
        // ファイルのモジュール doc comment「MQTT」参照)。
        min_interval_ms: 100,
    };
    app.mqtt.apply(&mqtt_settings).await;
    assert!(
        wait_until(Duration::from_secs(6), || async { app.mqtt.connected() }).await,
        "mqtt publisher should connect to the in-process broker before the soak run starts"
    );

    // WS: interval モードで固定周期購読(このファイルのモジュール doc
    // comment「WebSocket」参照 - on_change ではなく interval を選ぶことで
    // レジスタ変更タイミングに依存しない安定したカウントにする)。
    let mut ws = connect_ws(&app.ws_url("/api/v1/stream"), &app.token)
        .await
        .expect("ws handshake should succeed");
    send_json(
        &mut ws,
        json!({
            "op": "subscribe",
            "id": 1,
            "tags": ["line1.fast.temp01"],
            "mode": "interval",
            "interval_ms": EVAL_TICK_MS,
        }),
    )
    .await;
    recv_matching(&mut ws, |m| m["op"] == "data" && m["id"] == 1).await; // initial snapshot

    let ws_count = Arc::new(AtomicU64::new(0));
    let ws_count_bg = ws_count.clone();
    let ws_task = tokio::spawn(async move {
        loop {
            match ws.next().await {
                Some(Ok(WsMessage::Text(text))) => {
                    if let Ok(value) = serde_json::from_str::<Value>(&text) {
                        if value["op"] == "data" && value["id"] == 1 {
                            ws_count_bg.fetch_add(1, Ordering::SeqCst);
                        }
                    }
                }
                Some(Ok(WsMessage::Ping(_))) | Some(Ok(WsMessage::Pong(_))) => continue,
                Some(Ok(_)) => continue,
                Some(Err(_)) | None => return,
            }
        }
    });

    let mqtt_topic = "banto/line1/fast/temp01";
    let mqtt_sub =
        LiveSubscriber::subscribe(broker_port, &format!("sub-soak-{label}"), mqtt_topic).await;
    assert!(
        wait_until(Duration::from_secs(6), || async {
            mqtt_sub.count().await > 0
        })
        .await,
        "mqtt should publish the initial forced value before the timed window starts"
    );
    let mqtt_baseline = mqtt_sub.count().await;

    // ここから計測区間: レジスタを収集周期と同じ間隔で単調増加させ続け、
    // 収集(tstore)・WS(interval)・MQTT(on_change)の3経路すべてに継続的な
    // 値の流れを供給する。
    let rss_start = mem_probe::working_set_bytes();
    println!("soak[{label}]: start RSS = {rss_start:?} bytes");

    let started = tokio::time::Instant::now();
    let mut counter: u16 = 0;
    let mut rss_samples: Vec<u64> = Vec::new();
    while started.elapsed() < duration {
        counter = counter.wrapping_add(1);
        sim.set_holding_register(0, counter);
        tokio::time::sleep(Duration::from_millis(PERIOD_MS as u64)).await;
        if let Some(rss) = mem_probe::working_set_bytes() {
            rss_samples.push(rss);
        }
    }
    // 直近の変更が3経路すべてに届き切るのを待ってから確定させる。
    tokio::time::sleep(Duration::from_millis(400)).await;
    let elapsed = started.elapsed();

    let rss_end = mem_probe::working_set_bytes();
    println!(
        "soak[{label}]: end RSS = {rss_end:?} bytes ({} samples collected)",
        rss_samples.len()
    );

    ws_task.abort();
    let ws_data_messages = ws_count.load(Ordering::SeqCst);
    let mqtt_publishes = mqtt_sub.count().await.saturating_sub(mqtt_baseline);

    // manager.shutdown() が Collector::stop() を呼び、tstore を flush する
    // (banto-collect の雛形と同じ「stop 後に読む」規律 - このファイルの
    // `read_single_group_rows` doc comment参照)。
    app.manager.shutdown().await;
    sim.stop();
    if let Some(server) = app.server.take() {
        server.stop().await;
    }

    let rows = read_single_group_rows(&app.env.data_dir()).await;

    SoakReport {
        elapsed,
        collected_rows: rows.len(),
        ws_data_messages,
        mqtt_publishes,
        rss_start_bytes: rss_start,
        rss_end_bytes: rss_end,
    }
}

/// 理論値に対する緩い下限(banto-collect の雛形と同じ考え方 - CI が詰まって
/// ティックを取りこぼすことはあっても、バーストで余分に受信することはない
/// 設計なので下限だけを見る)。`fraction`が小さいほど緩い。
fn loose_lower_bound(theoretical: f64, fraction: f64) -> usize {
    (theoretical * fraction).floor().max(1.0) as usize
}

// ---------------------------------------------------------------------------
// 短時間版(通常 CI で毎回走る)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mini_soak_collect_ws_mqtt_stay_alive_for_several_seconds() {
    let run_secs = 5u64;
    let report = run_soak("mini", Duration::from_secs(run_secs)).await;

    let elapsed_ms = report.elapsed.as_millis() as f64;
    let theoretical_rows = elapsed_ms / PERIOD_MS as f64;
    let theoretical_eval_ticks = elapsed_ms / EVAL_TICK_MS as f64;

    println!(
        "mini-soak: {} rows in {:?} (theoretical ~{theoretical_rows:.0}), \
         {} ws data messages / {} mqtt publishes (theoretical ~{theoretical_eval_ticks:.0} each)",
        report.collected_rows, report.elapsed, report.ws_data_messages, report.mqtt_publishes
    );

    // Liveness floor, not a throughput measurement (H7 ⑤, 2026-08-08):
    // `MissedTickBehavior::Skip` (this file's module doc "タイミングの許容
    // 範囲") means a tick missed under CPU pressure is lost forever, never
    // caught up - so on a severely oversubscribed CI runner these counts can
    // crater to roughly 1/10 of the theoretical value (the same mechanism
    // behind the sibling fix to
    // `crates/banto-collect/tests/integration.rs::mini_soak_100ms_three_groups_row_counts_within_tolerance`).
    // This test's job is only to prove the 3 pipelines (collect/WS/MQTT)
    // stayed *alive* for the whole run, not to pin their exact throughput
    // (that's the #[ignore]d long soak's job - `long_soak_collect_ws_mqtt_stay_alive`
    // below keeps the tighter 60%/80% bounds since a longer run averages out
    // jitter). 1/15 is chosen to sit clearly below that worst-observed ~1/10
    // floor (a further safety margin under it) while `loose_lower_bound`'s
    // `.max(1.0)` still guarantees the assertion fails outright for a dead
    // pipeline (0 progress).
    assert!(
        report.collected_rows >= loose_lower_bound(theoretical_rows, 1.0 / 15.0),
        "expected ~{theoretical_rows:.0} collected rows in {run_secs}s @ {PERIOD_MS}ms \
         (liveness floor >=1/15 tolerated), got {}",
        report.collected_rows
    );
    assert!(
        report.ws_data_messages as usize >= loose_lower_bound(theoretical_eval_ticks, 1.0 / 15.0),
        "expected ~{theoretical_eval_ticks:.0} ws data messages (liveness floor >=1/15 tolerated), got {}",
        report.ws_data_messages
    );
    assert!(
        report.mqtt_publishes >= loose_lower_bound(theoretical_eval_ticks, 1.0 / 15.0),
        "expected ~{theoretical_eval_ticks:.0} mqtt publishes (liveness floor >=1/15 tolerated), got {}",
        report.mqtt_publishes
    );
}

// ---------------------------------------------------------------------------
// 長時間版(`#[ignore]`。将来の 72h リリースゲートの種 -
// `crates/banto-collect/tests/integration.rs::long_soak_sixty_seconds`と同じ
// 運用)
// ---------------------------------------------------------------------------

/// 収集 + WS 購読 + MQTT 発行を同時に維持した長時間ソーク。既定 60 秒
/// （このセッションで実際に動作確認できる長さ）。`SOAK_DURATION_SECS`
/// 環境変数で上書き可能 - **本番の 72h 出荷判定ソーク**は
/// `SOAK_DURATION_SECS=259200 cargo test -p banto-hub-core --test soak -- \
/// --ignored --nocapture` で明示実行する(手順は
/// docs/banto-hub-operations.md「72h 出荷判定ソークテスト」節、判定基準の
/// 一次ソースは docs/recorder-requirements.md §4・docs/tag-server-design.md
/// §8)。メモリ増加の合否判定はこのテストの自動アサーションでは行わない -
/// このファイルのモジュール doc comment「メモリ増加の検証」参照。
#[ignore = "long-running (60s by default) soak; run explicitly with --ignored. \
            Override the duration via SOAK_DURATION_SECS (seconds) - e.g. \
            SOAK_DURATION_SECS=259200 for the real 72h release-gate run \
            (docs/banto-hub-operations.md)."]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn long_soak_collect_ws_mqtt_stay_alive() {
    let run_secs: u64 = std::env::var("SOAK_DURATION_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(60);
    let report = run_soak("long", Duration::from_secs(run_secs)).await;

    let elapsed_ms = report.elapsed.as_millis() as f64;
    let theoretical_rows = elapsed_ms / PERIOD_MS as f64;
    let theoretical_eval_ticks = elapsed_ms / EVAL_TICK_MS as f64;

    println!(
        "long-soak: {} rows in {:?} (theoretical ~{theoretical_rows:.0}), \
         {} ws data messages / {} mqtt publishes (theoretical ~{theoretical_eval_ticks:.0} each)",
        report.collected_rows, report.elapsed, report.ws_data_messages, report.mqtt_publishes
    );
    println!(
        "long-soak: RSS start={:?} bytes, end={:?} bytes (診断用ログ - 合否判定には使わない。\
         72h 本番ソークでのメモリ増加なしの判定手順は docs/banto-hub-operations.md 参照)",
        report.rss_start_bytes, report.rss_end_bytes
    );

    // 走行時間が長いほど揺らぎは平均化されるので、短時間版より厳しい下限
    // (banto-collect の long_soak_sixty_seconds が「理論値600に対し500超え」
    // = 約83%を要求するのと同じ考え方。WS/MQTT は輪をかけてネットワーク/
    // ブローカー往復が絡むぶん少し緩める)。
    assert!(
        report.collected_rows >= loose_lower_bound(theoretical_rows, 0.8),
        "expected ~{theoretical_rows:.0} collected rows in {run_secs}s @ {PERIOD_MS}ms \
         (>=80% tolerated), got {}",
        report.collected_rows
    );
    assert!(
        report.ws_data_messages as usize >= loose_lower_bound(theoretical_eval_ticks, 0.6),
        "expected ~{theoretical_eval_ticks:.0} ws data messages (>=60% tolerated), got {}",
        report.ws_data_messages
    );
    assert!(
        report.mqtt_publishes >= loose_lower_bound(theoretical_eval_ticks, 0.6),
        "expected ~{theoretical_eval_ticks:.0} mqtt publishes (>=60% tolerated), got {}",
        report.mqtt_publishes
    );
}
