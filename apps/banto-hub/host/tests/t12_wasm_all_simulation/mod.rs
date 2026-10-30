//! WASM ドライバの接続は、シミュレーションのとき実機（実ホスト役）へ一度も
//! 接続・送信しない（#695 の安全要件）。
//!
//! 実ホスト役は「接続数と受信回数を数えるだけの TCP リスナー」で、WASM の
//! 接続（`modbus-fc3-wasm`、固定の版のゲスト）の `host`/`port` をそこへ向ける。
//! コード上は `runtime_snapshot_for_mode` が全シミュレーション運転で
//! `is_broker_managed` な接続を `simulation = true` にし、`BrokerSimRegistry::resolve`
//! が `DialTarget::Sim` で値の層の `SimDevice` を使うので、ゲストは実ホストを
//! 見ない。WASM の接続でこれを固定する。
//!
//! 1. 保存値 `simulation = false` の接続を **全シミュレーション運転** で収集しても、
//!    タグがシミュレータの値を持ち、リスナーの接続数・受信は 0。
//! 2. 保存値 `simulation = true` の接続は **通常の運転**（Configured）でも同じ。
//! 3. 全シミュレーション運転中の許可された書込み（#363）は実ホストへ届かず、
//!    シミュレータの値に反映されて読み戻せる（REST の書込みは catalog＝rebuild が要り、
//!    停止中の rebuild は保存値のとおり実機へ接続するので、ここだけは開始後の増分が 0）。
//! 4. 対照: 保存値 `simulation = false` を通常の運転で収集すると、同じリスナーに
//!    接続が来る（数える仕組みが効いていることの確認。これが 0 なら 1〜3 は無意味）。
//!
//! 他の t12 の WASM 試験と同じく、固定の版の component が要るため `#[ignore]`
//! （`BANTO_MODBUS_COMPONENT_WASM`・`BANTO_MODBUS_MANIFEST`、CI の Modbus Hub join と同じ手順）。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use banto_hub_core::api_keys::ApiKeysService;
use banto_hub_core::controller::{CollectionState, RunMode};
use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;

/// 実ホスト役。受け付けた接続数と、受信した塊の数を数えるだけで、何も返さない。
struct CountingHost {
    port: u16,
    accepted: Arc<AtomicUsize>,
    requests: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}

impl CountingHost {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let accepted = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(AtomicUsize::new(0));
        let (accept_count, request_count) = (accepted.clone(), requests.clone());
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                accept_count.fetch_add(1, Ordering::SeqCst);
                let request_count = request_count.clone();
                tokio::spawn(async move {
                    let mut buffer = [0u8; 512];
                    while let Ok(read) = socket.read(&mut buffer).await {
                        if read == 0 {
                            break;
                        }
                        request_count.fetch_add(1, Ordering::SeqCst);
                    }
                });
            }
        });
        Self {
            port,
            accepted,
            requests,
            task,
        }
    }

    fn accepted(&self) -> usize {
        self.accepted.load(Ordering::SeqCst)
    }

    fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }
}

impl Drop for CountingHost {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn component() -> Vec<u8> {
    std::fs::read(
        std::env::var("BANTO_MODBUS_COMPONENT_WASM").expect("BANTO_MODBUS_COMPONENT_WASM"),
    )
    .unwrap()
}

fn manifest() -> Vec<u8> {
    std::fs::read(std::env::var("BANTO_MODBUS_MANIFEST").expect("BANTO_MODBUS_MANIFEST")).unwrap()
}

/// 保存値 `simulation` の WASM 接続（実ホスト役へ向ける）と、その上の 1 グループ・
/// 書込み可の `hold`（保持レジスタ 400001、u16）を作る。書込みの 2 つの許可
/// （ゲストの `write_*`・ホストの `modbus_operational_write_*`）は開けておく。
async fn seed(app: &super::TestApp, host: &CountingHost, simulation: bool, rebuild: bool) -> i64 {
    let connection = app
        .manager
        .plc_connections()
        .create_settings(super::ConnectionSettingsInput {
            name: "simline".into(),
            protocol: "modbus-fc3-wasm".into(),
            enabled: true,
            simulation,
            settings: serde_json::json!({
                "host": "127.0.0.1", "port": host.port, "unit_id": 1,
                "probe_address": "30001",
                "write_mode": "enabled", "write_allowlist": "400001-400010",
                "modbus_operational_write_mode": "enabled",
                "modbus_operational_write_allowlist": "400001-400010"
            })
            .as_object()
            .unwrap()
            .clone(),
            secrets: Default::default(),
        })
        .await
        .expect("save the WASM connection");
    let group = super::CollectionGroupService::with_registry(
        app.pool.clone(),
        app.manager.registry().clone(),
    )
    .create(super::group_input("line", connection.id, 100))
    .await
    .unwrap();
    let mut input = super::tag_input("hold", group.id, "400001", "u16");
    input.writable = true;
    let tag = app.manager.tags().create(input).await.unwrap();
    if rebuild {
        app.manager.rebuild().await.unwrap();
    }
    tag.id
}

fn cached(app: &super::TestApp, tag_id: i64) -> Option<f64> {
    app.manager
        .current_values()
        .and_then(|values| values.get(&format!("tag:{tag_id}")))
        .and_then(|state| state.value)
}

async fn wait_until<F: FnMut() -> bool>(mut ok: F) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        if ok() {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// タグの値が出たあとも少し回し続け、そのあいだに実ホスト役へ何も来ないことを見る
/// （値が出た瞬間だけの確認にしない）。
async fn assert_host_untouched(host: &CountingHost, what: &str) {
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(host.accepted(), 0, "{what}: 実ホスト役に接続が来た");
    assert_eq!(host.requests(), 0, "{what}: 実ホスト役に要求が来た");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the pinned Modbus WASM package files"]
async fn all_simulation_run_never_touches_the_real_host_for_a_real_wasm_connection() {
    let host = CountingHost::start().await;
    let app =
        super::test_app_with_sim_controller("t12-wasm-allsim", &manifest(), &component()).await;
    // 保存値は「実機」のまま。全シミュレーション運転の上書きだけが値の出所を替える。
    let tag = seed(&app, &host, false, false).await;
    let controller = app.controller.as_ref().unwrap();

    let status = controller.start(RunMode::AllSimulation).await.unwrap();
    assert_eq!(status.state, CollectionState::Running, "{status:?}");
    assert_eq!(status.mode, RunMode::AllSimulation);

    assert!(
        wait_until(|| cached(&app, tag).is_some()).await,
        "全シミュレーション運転でタグがシミュレータの値を持つ"
    );
    assert_host_untouched(&host, "全シミュレーション運転").await;
    controller.stop().await;
    assert_host_untouched(&host, "停止まで").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the pinned Modbus WASM package files"]
async fn a_saved_simulation_wasm_connection_never_touches_the_real_host_in_a_normal_run() {
    let host = CountingHost::start().await;
    let app =
        super::test_app_with_sim_controller("t12-wasm-saved-sim", &manifest(), &component()).await;
    let tag = seed(&app, &host, true, false).await;
    let controller = app.controller.as_ref().unwrap();

    let status = controller.start(RunMode::Configured).await.unwrap();
    assert_eq!(status.state, CollectionState::Running, "{status:?}");
    assert_eq!(status.mode, RunMode::Configured);

    assert!(
        wait_until(|| cached(&app, tag).is_some()).await,
        "simulation=true の接続は通常の運転でもシミュレータから読める"
    );
    assert_host_untouched(&host, "simulation=true の通常運転").await;
    controller.stop().await;
    assert_host_untouched(&host, "停止まで").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the pinned Modbus WASM package files"]
async fn all_simulation_write_goes_to_the_simulator_and_not_to_the_real_host() {
    let host = CountingHost::start().await;
    let app =
        super::test_app_with_sim_controller("t12-wasm-allsim-w", &manifest(), &component()).await;
    let tag = seed(&app, &host, false, true).await;
    let controller = app.controller.as_ref().unwrap();
    // 書込みの REST は catalog（rebuild）が要る。停止中の rebuild は保存値のとおり
    // 実機へ接続する（既存の挙動。全シミュレーションの上書きは収集の開始から）ので、
    // 「0」は開始後の増分で見る。

    let status = controller.start(RunMode::AllSimulation).await.unwrap();
    assert_eq!(status.state, CollectionState::Running, "{status:?}");
    app.write_control.enable();
    let issued = ApiKeysService::new(app.pool.clone())
        .issue("sim-writer", vec!["write:simline.line.hold".into()], None)
        .await
        .unwrap();

    assert!(
        wait_until(|| cached(&app, tag).is_some()).await,
        "全シミュレーション運転でタグが値を持つ"
    );
    tokio::time::sleep(Duration::from_millis(400)).await;
    let (accepted, requests) = (host.accepted(), host.requests());
    // 生成値と取り違えない値で書く。
    let (status, body) = super::admin_write(
        &app.router,
        "POST",
        "/api/v1/values/simline.line.hold",
        &issued.key,
        serde_json::json!({ "v": 4321 }),
    )
    .await;
    assert_eq!(
        status,
        super::StatusCode::OK,
        "#363: 書込みは断られない: {body:?}"
    );
    assert!(
        wait_until(|| cached(&app, tag) == Some(4321.0)).await,
        "書いた値がシミュレータの値として読み戻せる"
    );
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(host.accepted(), accepted, "書込みで実ホスト役に接続が来た");
    assert_eq!(host.requests(), requests, "書込みで実ホスト役に要求が来た");
    controller.stop().await;
}

/// 対照。保存値 `simulation = false` を通常の運転で収集すると、同じリスナーに
/// 実際に接続が来る。これが成り立たないと上の 3 つの「0」は何も保証しない。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the pinned Modbus WASM package files"]
async fn control_a_real_wasm_connection_in_a_normal_run_does_reach_the_host() {
    let host = CountingHost::start().await;
    let app =
        super::test_app_with_sim_controller("t12-wasm-control", &manifest(), &component()).await;
    let _tag = seed(&app, &host, false, false).await;
    let controller = app.controller.as_ref().unwrap();

    controller.start(RunMode::Configured).await.unwrap();
    assert!(
        wait_until(|| host.accepted() > 0 && host.requests() > 0).await,
        "実機の接続は実ホスト役に接続して要求を送る（数える仕組みの確認）"
    );
    controller.stop().await;
}
