//! #392 B1-5: できた `chronogazer-server` をプロセスとして動かす統合テスト
//! （docs/chronogazer-service-design.md §4 の B1-5 の受入条件）。
//!
//! - 一時ルート（`CHRONOGAZER_ROOT`）で起動 → シミュレータの PLC（Modbus TCP、
//!   `banto_collect::simulation`。開発用 PLC の `dev_plc` example と同じもの）から
//!   収集 → Ctrl-C と同じ経路で停止: `collection_stopped` と行が残り、
//!   `run\endpoint.json` が消え、（Windows）ログに終了の行が残る。
//! - （Windows）コンソールのウィンドウを閉じたとき（`CTRL_CLOSE_EVENT`）の
//!   ハンドラーと同じ関数を通しても同じものが残る。
//! - （Windows）Hub に接続 → 再起動 → resume が張れる（キーは DPAPI のファイル）。
//! - デスクトップと同じ DB を指すと A1 で拒否（終了コード 2）。
//! - `%APPDATA%`・`%LOCALAPPDATA%`・作業ディレクトリに何も作らない。
//! - 相対パスの `CHRONOGAZER_ROOT` は拒否（終了コード 3、何も作らない）。
//!
//! 止め方は bin の隠し引数 `--control-stdin`（標準入力の `ctrl-c` / `close`）。
//! 本物のコンソールの Ctrl（`SetConsoleCtrlHandler`）は、テストのプロセスから
//! 別のプロセスのコンソールへ確実に送る手段が無いので、実機の確認
//! （docs の B1-5 の実装メモ）で確かめる。待ちはすべて「条件をポーリング + 上限」。

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
#[cfg(windows)]
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use banto_collect::simulation;
use banto_collect::Protocol;
use banto_tags::{
    CollectionGroupInput, CollectionGroupService, PlcConnectionInput, PlcConnectionService,
    TagInput, TagService,
};
use banto_tstore::{list_data_files, TsReader};
use chronogazer_core::collect::{CollectEventRow, CollectorService, EventPage, Readout};
use chronogazer_core::db::{init_db, Db};
use chronogazer_core::endpoint::read_endpoint;
use chronogazer_core::instance_lock::{acquire_for_db, ChronoGazerHost};
use chronogazer_core::paths::DataLayout;
use chronogazer_core::settings::SettingsService;

const WAIT_LIMIT: Duration = Duration::from_secs(40);
const POLL: Duration = Duration::from_millis(100);
const ADMIN_PASSWORD: &str = "admin-pass-1";

// --- 足場 ---------------------------------------------------------------------

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

/// テスト 1 本分の場所: データのルート・利用者のプロファイル（`APPDATA` など）・
/// 作業ディレクトリ・子の出力。
struct Sandbox {
    _dir: TempDir,
    root: PathBuf,
    appdata: PathBuf,
    local_appdata: PathBuf,
    cwd: PathBuf,
    output: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let dir = TempDir::new();
        let root = dir.path().join("root");
        let appdata = dir.path().join("profile").join("Roaming");
        let local_appdata = dir.path().join("profile").join("Local");
        let cwd = dir.path().join("cwd");
        for path in [&appdata, &local_appdata, &cwd] {
            std::fs::create_dir_all(path).unwrap();
        }
        let output = dir.path().join("server-output.txt");
        Self {
            root,
            appdata,
            local_appdata,
            cwd,
            output,
            _dir: dir,
        }
    }

    fn layout(&self) -> DataLayout {
        DataLayout::new(&self.root)
    }

    /// 利用者のプロファイルと作業ディレクトリに何も作られていない。
    fn assert_profile_untouched(&self) {
        for dir in [&self.appdata, &self.local_appdata, &self.cwd] {
            let entries: Vec<_> = std::fs::read_dir(dir)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect();
            assert!(
                entries.is_empty(),
                "{} に作られた: {entries:?}",
                dir.display()
            );
        }
    }

    fn output_text(&self) -> String {
        std::fs::read_to_string(&self.output).unwrap_or_default()
    }
}

/// 子プロセス（テストが失敗しても残さない）。
struct Server {
    child: Option<Child>,
}

impl Server {
    fn spawn(sandbox: &Sandbox, root: &str) -> Self {
        Self::spawn_with_env(sandbox, root, &[])
    }

    fn spawn_with_env(sandbox: &Sandbox, root: &str, envs: &[(&str, &str)]) -> Self {
        let output = File::create(&sandbox.output).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_chronogazer-server"))
            .envs(envs.iter().copied())
            .arg("--control-stdin")
            .env("CHRONOGAZER_ROOT", root)
            .env("APPDATA", &sandbox.appdata)
            .env("LOCALAPPDATA", &sandbox.local_appdata)
            .env_remove("BANTO_HUB_MEMORY_KEYS")
            .env_remove("BANTO_DB")
            .current_dir(&sandbox.cwd)
            .stdin(Stdio::piped())
            .stdout(output.try_clone().unwrap())
            .stderr(output)
            .spawn()
            .expect("chronogazer-server を起動できる");
        Self { child: Some(child) }
    }

    fn pid(&self) -> u32 {
        self.child.as_ref().unwrap().id()
    }

    fn send(&mut self, line: &str) {
        let stdin = self.child.as_mut().unwrap().stdin.as_mut().unwrap();
        writeln!(stdin, "{line}").unwrap();
        stdin.flush().unwrap();
    }

    async fn wait_exit(&mut self) -> ExitStatus {
        let deadline = Instant::now() + WAIT_LIMIT;
        loop {
            if let Some(status) = self.child.as_mut().unwrap().try_wait().unwrap() {
                self.child = None;
                return status;
            }
            assert!(Instant::now() < deadline, "chronogazer-server が終わらない");
            tokio::time::sleep(POLL).await;
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

async fn wait_for<T>(what: &str, mut probe: impl AsyncFnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + WAIT_LIMIT;
    loop {
        if let Some(value) = probe().await {
            return value;
        }
        assert!(
            Instant::now() < deadline,
            "{what} が {} 秒以内に成り立たなかった",
            WAIT_LIMIT.as_secs()
        );
        tokio::time::sleep(POLL).await;
    }
}

/// 起動を待ち、`run\endpoint.json` の port（loopback）を返す。
async fn wait_ready(sandbox: &Sandbox, server: &Server) -> u16 {
    let path = sandbox.layout().endpoint_file();
    let pid = server.pid();
    wait_for("endpoint.json の書き込み", async || {
        read_endpoint(&path)
            .filter(|info| info.pid == pid)
            .map(|info| info.port)
    })
    .await
}

// --- REST ---------------------------------------------------------------------

async fn request(
    method: reqwest::Method,
    url: String,
    token: Option<&str>,
    body: Option<serde_json::Value>,
) -> (u16, serde_json::Value) {
    let mut builder = reqwest::Client::new()
        .request(method, url)
        .header("X-Banto-Client", "banto");
    if let Some(token) = token {
        builder = builder.header("Authorization", format!("Bearer {token}"));
    }
    if let Some(body) = body {
        builder = builder
            .header("Content-Type", "application/json")
            .body(body.to_string());
    }
    let response = builder.send().await.expect("send");
    let status = response.status().as_u16();
    let text = response.text().await.unwrap_or_default();
    (status, serde_json::from_str(&text).unwrap_or_default())
}

/// LAN に出していないので、最初の管理者を作れる（token が返る）。
async fn setup_admin(port: u16) -> String {
    let (status, body) = request(
        reqwest::Method::POST,
        format!("http://127.0.0.1:{port}/api/auth/setup"),
        None,
        Some(serde_json::json!({"username":"admin","password":ADMIN_PASSWORD,"displayName":"管理者"})),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    body["token"].as_str().expect("token").to_string()
}

async fn login(port: u16) -> String {
    let (status, body) = request(
        reqwest::Method::POST,
        format!("http://127.0.0.1:{port}/api/auth/login"),
        None,
        Some(serde_json::json!({"username":"admin","password":ADMIN_PASSWORD})),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    body["token"].as_str().expect("token").to_string()
}

// --- DB の準備と読み戻し -------------------------------------------------------

/// 一時ルートの DB を作り、待ち受けを port 0（OS が選ぶ）にする。`plc_port` が
/// あれば、シミュレータの Modbus TCP の接続 1・グループ 1（100ms）・タグ 1 を登録する。
async fn prepare_root(sandbox: &Sandbox, plc_port: Option<u16>, hub: bool) {
    std::fs::create_dir_all(&sandbox.root).unwrap();
    let pool = init_db(sandbox.layout().db_path()).await.expect("init_db");
    let settings = SettingsService::new(Db::Sqlite(pool.clone()));
    settings.set("server.port", "0").await.unwrap();
    if let Some(port) = plc_port {
        register(&pool, "modbus-tcp", "127.0.0.1", port, "40001", "u16").await;
    }
    if hub {
        register(&pool, banto_tags::HUB_PROTOCOL, "", 0, HUB_TAG, "f64").await;
    }
    pool.close().await;
}

async fn register(
    pool: &sqlx::SqlitePool,
    protocol: &str,
    host: &str,
    port: u16,
    address: &str,
    data_type: &str,
) {
    let conn = PlcConnectionService::new(pool.clone())
        .create(PlcConnectionInput {
            name: format!("server-{protocol}"),
            protocol: protocol.to_string(),
            host: host.to_string(),
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
        .expect("接続");
    let group = CollectionGroupService::new(pool.clone())
        .create(CollectionGroupInput {
            name: format!("group-{protocol}"),
            plc_connection_id: conn.id,
            period_ms: 100,
            enabled: true,
            default_writable: false,
            query_sql: None,
        })
        .await
        .expect("グループ");
    TagService::new(pool.clone())
        .create(TagInput {
            name: "値".to_string(),
            collection_group_id: group.id,
            address: address.to_string(),
            data_type: data_type.to_string(),
            string_length: None,
            string_encoding: "utf8".to_string(),
            raw_lo: None,
            raw_hi: None,
            eng_lo: None,
            eng_hi: None,
            unit: None,
            decimals: 0,
            enabled: true,
            writable: false,
            tag_kind: "plc".to_string(),
            expression: None,
            retain: false,
            expected_revision: None,
        })
        .await
        .expect("タグ");
}

/// 止めた後の DB の最新のイベントの種類。
async fn latest_event(sandbox: &Sandbox) -> Option<String> {
    let layout = sandbox.layout();
    let pool = init_db(layout.db_path()).await.expect("開き直す");
    let reader = CollectorService::new(pool.clone(), layout.data_dir());
    let rows: Vec<CollectEventRow> = match reader.events(EventPage::default()).await {
        Readout::Ready { data } => data.rows,
        other => panic!("イベントが読めない: {}", other.as_str()),
    };
    pool.close().await;
    rows.first().map(|row| row.kind.clone())
}

/// データファイルの行数（全グループ）。
async fn stored_row_count(sandbox: &Sandbox) -> usize {
    let mut count = 0;
    for file in list_data_files(&sandbox.layout().data_dir()).expect("データファイルの列挙")
    {
        let reader = TsReader::open(&file.path).await.expect("開ける");
        for group in reader.groups() {
            count += reader
                .read_range(&group.key, i64::MIN, i64::MAX)
                .await
                .expect("読める")
                .len();
        }
    }
    count
}

#[cfg(windows)]
fn log_text(sandbox: &Sandbox) -> String {
    let logs = sandbox.layout().logs_dir();
    let mut names: Vec<String> = std::fs::read_dir(&logs)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert!(
        names
            .iter()
            .all(|name| chronogazer_core::server_host::parse_log_file_date(name).is_some()),
        "{names:?}"
    );
    names
        .iter()
        .map(|name| std::fs::read_to_string(logs.join(name)).unwrap())
        .collect()
}

// --- 収集して止める -------------------------------------------------------------

/// 収集が走り、現在値に good の値が出るまで待つ。
async fn wait_collecting(port: u16, token: &str) {
    wait_for("収集の現在値", async || {
        let (_, state) = request(
            reqwest::Method::GET,
            format!("http://127.0.0.1:{port}/api/collect"),
            Some(token),
            None,
        )
        .await;
        if state["state"] != "running" {
            return None;
        }
        let (_, values) = request(
            reqwest::Method::GET,
            format!("http://127.0.0.1:{port}/api/collect/values"),
            Some(token),
            None,
        )
        .await;
        values.to_string().contains("\"good\"").then_some(())
    })
    .await;
}

/// 起動 → 収集 → `stop_line` で停止。終了の状態を返す。
/// 収集中に、本文を送り切らない要求を 1 本開いたままにする（#598 のオーナー
/// レビュー P1 の再現）: `Content-Length: 100` で本文の先頭 1 バイトだけ送る。
/// 返した接続を持っている間、サーバーの graceful な停止はこの要求を待つ。
fn hold_unfinished_request(port: u16) -> std::net::TcpStream {
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).expect("接続");
    let head = format!(
        "POST /api/auth/login HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nX-Banto-Client: banto\r\nContent-Type: application/json\r\nContent-Length: 100\r\n\r\n{{"
    );
    stream.write_all(head.as_bytes()).unwrap();
    stream.flush().unwrap();
    stream
}

/// 起動 → 収集 → `stop_line` で停止（`hold_slow` なら、その前に本文を送り切らない
/// 要求を開いたままにする）。終了の状態と、停止の指示から終了までの時間を返す。
async fn collect_then_stop_with(
    sandbox: &Sandbox,
    stop_line: &str,
    hold_slow: bool,
) -> (ExitStatus, Duration) {
    let sim = simulation::start(Protocol::ModbusTcp).await;
    prepare_root(sandbox, Some(sim.addr().port()), false).await;
    let mut server = Server::spawn(sandbox, sandbox.root.to_str().unwrap());
    let port = wait_ready(sandbox, &server).await;

    let (status, host) = request(
        reqwest::Method::GET,
        format!("http://127.0.0.1:{port}/api/host"),
        None,
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(host["hostKind"], "server");
    assert_eq!(host["product"], "chronogazer");

    let token = setup_admin(port).await;
    wait_collecting(port, &token).await;
    let held = hold_slow.then(|| hold_unfinished_request(port));
    if held.is_some() {
        // 要求の先頭がサーバーに読まれて、本文を待つ状態になってから止める。
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    let stopping = Instant::now();
    server.send(stop_line);
    let status = server.wait_exit().await;
    let elapsed = stopping.elapsed();
    drop(held);
    drop(sim);
    (status, elapsed)
}

async fn collect_then_stop(sandbox: &Sandbox, stop_line: &str) -> ExitStatus {
    collect_then_stop_with(sandbox, stop_line, false).await.0
}

async fn assert_stopped_cleanly(sandbox: &Sandbox) {
    assert!(
        !sandbox.layout().endpoint_file().exists(),
        "endpoint.json が残った"
    );
    assert_eq!(
        latest_event(sandbox).await.as_deref(),
        Some("collection_stopped"),
        "最新のイベントが collection_stopped でない"
    );
    assert!(stored_row_count(sandbox).await > 0, "行が残っていない");
    sandbox.assert_profile_untouched();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn collects_from_a_plc_and_stops_cleanly_on_ctrl_c() {
    let sandbox = Sandbox::new();
    let status = collect_then_stop(&sandbox, "ctrl-c").await;
    assert_eq!(status.code(), Some(0), "{}", sandbox.output_text());
    assert_stopped_cleanly(&sandbox).await;
    #[cfg(windows)]
    {
        let log = log_text(&sandbox);
        for expected in [
            "chronogazer-server: 起動します",
            "chronogazer-server: 停止します（Ctrl-C）",
            "chronogazer-server: 終了の後始末が完了しました（Ctrl-C）",
            "chronogazer-server: 終了しました（終了コード 0）",
        ] {
            assert!(log.contains(expected), "{expected} がログに無い:\n{log}");
        }
        // ログの各行に時刻が付く。
        assert!(log.lines().all(|line| line.starts_with('[')), "{log}");
        // コンソール（ここでは子の標準エラー出力のファイル）にも写っている。
        assert!(sandbox
            .output_text()
            .contains("終了の後始末が完了しました（Ctrl-C）"));
    }
}

/// `CTRL_CLOSE_EVENT` のハンドラーと同じ関数（`StopController::on_close`）を通した
/// 後に、OS と同じくプロセスが終わる（`STATUS_CONTROL_C_EXIT`）。それでも最終の
/// 書き出し・`endpoint.json` の削除・ログの終了の行が残る。
#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn closing_the_console_window_still_flushes_within_the_short_budget() {
    let sandbox = Sandbox::new();
    let started = Instant::now();
    let status = collect_then_stop(&sandbox, "close").await;
    assert_eq!(
        status.code(),
        Some(0xC000_013A_u32 as i32),
        "{}",
        sandbox.output_text()
    );
    assert_stopped_cleanly(&sandbox).await;
    let log = log_text(&sandbox);
    for expected in [
        "終了の後始末が完了しました（コンソールのウィンドウを閉じた）",
        "終了します（コンソールのウィンドウを閉じた、終了コード 0）",
    ] {
        assert!(log.contains(expected), "{expected} がログに無い:\n{log}");
    }
    assert!(!log.contains("停止します（Ctrl-C）"), "{log}");
    assert!(started.elapsed() < WAIT_LIMIT);
}

/// #598 のオーナーレビュー P1: 本文を送り切らない HTTP の要求を開いたまま Ctrl-C で
/// 止めても、listener の停止を待ちきらずに収集を止める（listener の待ちは上限 1 秒。
/// 以前は banto の graceful の待ち 5 秒を使い切ってから収集を止めていた）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unfinished_http_request_does_not_delay_the_final_flush_on_ctrl_c() {
    let sandbox = Sandbox::new();
    let (status, elapsed) = collect_then_stop_with(&sandbox, "ctrl-c", true).await;
    assert_eq!(status.code(), Some(0), "{}", sandbox.output_text());
    assert_stopped_cleanly(&sandbox).await;
    assert!(
        elapsed < Duration::from_secs(4),
        "止まるまで {elapsed:?}（listener の停止を待ちすぎ）"
    );
}

/// #598 のオーナーレビュー P1: 同じ要求を開いたままコンソールのウィンドウを閉じても
/// （予算 4 秒）、最終の書き出しと `collection_stopped` が残る。以前は listener の
/// 停止の待ちが予算を使い切り、収集を止めないまま OS がプロセスを終わらせていた。
#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unfinished_http_request_does_not_skip_the_final_flush_on_close() {
    let sandbox = Sandbox::new();
    let (status, _) = collect_then_stop_with(&sandbox, "close", true).await;
    assert_eq!(
        status.code(),
        Some(0xC000_013A_u32 as i32),
        "{}",
        sandbox.output_text()
    );
    assert_stopped_cleanly(&sandbox).await;
    let log = log_text(&sandbox);
    assert!(
        log.contains("終了の後始末が完了しました（コンソールのウィンドウを閉じた）"),
        "{log}"
    );
}

/// #598 の Copilot の指摘: Ctrl-C の後始末の間にコンソールのウィンドウを閉じても、
/// main が先に `exit` して close のハンドラーの最後の行を切らない（ハンドラーが
/// 終わってから OS と同じくプロセスが終わる）。
#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn closing_during_a_ctrl_c_shutdown_keeps_the_close_handlers_last_lines() {
    let sandbox = Sandbox::new();
    // `ctrl-c` と `close` を続けて送る（標準入力のスレッドが順に処理する）。
    let status = collect_then_stop(&sandbox, "ctrl-c\nclose").await;
    assert_eq!(
        status.code(),
        Some(0xC000_013A_u32 as i32),
        "close のハンドラーの前に main が終わった: {}",
        sandbox.output_text()
    );
    assert_stopped_cleanly(&sandbox).await;
    let log = log_text(&sandbox);
    // 後始末の本体を走らせるのは先に `ExitCleanupOnce` に入った方（main の Ctrl-C か
    // close のハンドラー）で、どちらでも 1 回だけ。close のハンドラーの最後の行は必ず残る。
    assert_eq!(
        log.matches("終了の後始末が完了しました").count(),
        1,
        "後始末の完了がちょうど 1 回でない:\n{log}"
    );
    for expected in [
        "停止します（Ctrl-C）",
        "終了します（コンソールのウィンドウを閉じた、終了コード 0）",
    ] {
        assert!(log.contains(expected), "{expected} がログに無い:\n{log}");
    }
}

/// #598 の Copilot の指摘: 標準出力・標準エラー出力の向け直しに失敗したら、起動を
/// 止め（終了コード 3）、理由を日次ログに直接残す（失敗はデバッグビルドだけの
/// 環境変数で注入する）。
#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_stdio_redirect_stops_the_start_and_is_logged() {
    let sandbox = Sandbox::new();
    let mut server = Server::spawn_with_env(
        &sandbox,
        sandbox.root.to_str().unwrap(),
        &[("CHRONOGAZER_TEST_FAIL_STDIO_REDIRECT", "1")],
    );
    let status = server.wait_exit().await;
    let output = sandbox.output_text();
    assert_eq!(status.code(), Some(3), "{output}");
    assert!(output.contains("ログへ向け直せませんでした"), "{output}");
    let log = log_text(&sandbox);
    assert!(log.contains("ログへ向け直せませんでした"), "{log}");
    assert!(!sandbox.layout().endpoint_file().exists());
}

/// デスクトップが同じ DB を使っていれば A1 で拒否され、終了コード 2。DB も
/// `endpoint.json` も作らない。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refuses_the_db_the_desktop_is_using_with_exit_code_2() {
    let sandbox = Sandbox::new();
    std::fs::create_dir_all(&sandbox.root).unwrap();
    let db_path = sandbox.layout().db_path();
    let _desktop = acquire_for_db(&db_path, ChronoGazerHost::Desktop).expect("デスクトップの排他");
    let mut server = Server::spawn(&sandbox, sandbox.root.to_str().unwrap());
    let status = server.wait_exit().await;
    let output = sandbox.output_text();
    assert_eq!(status.code(), Some(2), "{output}");
    assert!(output.contains("起動できませんでした"), "{output}");
    assert!(output.contains("デスクトップ"), "{output}");
    assert!(!db_path.exists());
    assert!(!sandbox.layout().endpoint_file().exists());
    // #598 の Copilot の指摘: 排他を取れない側は、ルートの下に何も作らない（Hub の
    // キーの保存先を自分の ACL で作って、動いている側の保存先を使えなくしない）。
    let layout = sandbox.layout();
    for dir in [
        layout.secrets_dir(),
        layout.logs_dir(),
        layout.run_dir(),
        layout.data_dir(),
    ] {
        assert!(!dir.exists(), "{} を作った", dir.display());
    }
    sandbox.assert_profile_untouched();
}

/// #598 の Copilot の指摘: Ctrl のハンドラーを登録できなければ、制御された停止の
/// 手段が無いまま収集しないよう、何も作らずに終了コード 3 で終わる（失敗はデバッグ
/// ビルドだけの環境変数で注入する）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_ctrl_handler_registration_stops_before_anything_is_created() {
    let sandbox = Sandbox::new();
    let mut server = Server::spawn_with_env(
        &sandbox,
        sandbox.root.to_str().unwrap(),
        &[("CHRONOGAZER_TEST_FAIL_CTRL_HANDLER", "1")],
    );
    let status = server.wait_exit().await;
    let output = sandbox.output_text();
    assert_eq!(status.code(), Some(3), "{output}");
    assert!(
        output.contains("Ctrl のハンドラーを登録できませんでした"),
        "{output}"
    );
    assert!(!sandbox.root.exists(), "ルートを作った");
    sandbox.assert_profile_untouched();
}

/// #598 のオーナーレビュー P2: A1 の排他を取った後・DB の初期化の前にログを始める
/// ので、壊れた DB で起動して終了コード 3 で終わるとき、その理由と対処が日次ログに
/// 残る。
#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_broken_db_failure_is_written_to_the_daily_log() {
    let sandbox = Sandbox::new();
    std::fs::create_dir_all(&sandbox.root).unwrap();
    std::fs::write(sandbox.layout().db_path(), vec![0x5a_u8; 8192]).unwrap();
    let mut server = Server::spawn(&sandbox, sandbox.root.to_str().unwrap());
    let status = server.wait_exit().await;
    assert_eq!(status.code(), Some(3), "{}", sandbox.output_text());
    let log = log_text(&sandbox);
    for expected in [
        "起動できませんでした",
        "作り直す",
        "終了します（終了コード 3）",
    ] {
        assert!(log.contains(expected), "{expected} がログに無い:\n{log}");
    }
    sandbox.assert_profile_untouched();
}

/// #598 のオーナーレビュー P2: リストア予約の適用の警告（ここでは壊れた予約ファイル。
/// 適用をやめて起動を続ける）が日次ログに残る。
#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pending_restore_warning_is_written_to_the_daily_log() {
    let sandbox = Sandbox::new();
    prepare_root(&sandbox, None, false).await;
    let pending = sandbox
        .layout()
        .backups_dir()
        .join("restore-pending.sqlite3");
    std::fs::create_dir_all(pending.parent().unwrap()).unwrap();
    std::fs::write(&pending, b"not a sqlite file").unwrap();
    let mut server = Server::spawn(&sandbox, sandbox.root.to_str().unwrap());
    wait_ready(&sandbox, &server).await;
    server.send("ctrl-c");
    assert_eq!(server.wait_exit().await.code(), Some(0));
    let log = log_text(&sandbox);
    assert!(log.contains("リストア予約ファイルが不正"), "{log}");
}

/// 相対パスのルートは拒否（終了コード 3）。作業ディレクトリに何も作らない。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_relative_root_is_rejected_without_creating_anything() {
    let sandbox = Sandbox::new();
    let mut server = Server::spawn(&sandbox, "relative-root");
    let status = server.wait_exit().await;
    let output = sandbox.output_text();
    assert_eq!(status.code(), Some(3), "{output}");
    assert!(output.contains("絶対パスではありません"), "{output}");
    sandbox.assert_profile_untouched();
}

// --- Hub の接続 → 再起動 → resume（Windows: DPAPI のファイル） --------------------

const HUB_TAG: &str = "PLC9.G.real";
const HUB_KEY: &str = "bh_abcd1234_opaque-secret";

/// モック Hub: catalog・values・WS（subscribe を待って data を 1 つ送る）。WS の
/// 接続要求の先頭（`authorization` を含む）を記録する。
#[cfg(windows)]
async fn serve_mock_hub(listener: tokio::net::TcpListener, ws_heads: Arc<Mutex<Vec<String>>>) {
    use banto_tagclient::{
        CatalogSnapshot, CatalogTag, CollectionMode, StableTagId, ValueEntry, ValueQuality,
        ValueSource, ValuesSnapshot,
    };
    use futures_util::{SinkExt, StreamExt};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio_tungstenite::{accept_async, tungstenite::Message};

    let tag = CatalogTag {
        external_name: HUB_TAG.to_owned(),
        tag_key: "tag:1".into(),
        ids: StableTagId::new(1, 1, 1),
        connection: "PLC9".into(),
        group: "G".into(),
        name: "real".into(),
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
    };
    let catalog = serde_json::to_string(&CatalogSnapshot {
        revision: 1,
        run_id: Some(1),
        collection_mode: CollectionMode::Configured,
        tags: vec![tag],
    })
    .unwrap();
    let values = serde_json::to_string(&ValuesSnapshot {
        revision: 1,
        t: 1_000,
        run_id: Some(1),
        collection_mode: CollectionMode::Configured,
        values: vec![ValueEntry {
            tag: HUB_TAG.to_owned(),
            v: Some(1.0),
            q: ValueQuality::Good,
            t: 1_000,
            value_source: ValueSource::Real,
        }],
    })
    .unwrap();
    let frame = format!(
        r#"{{"op":"data","id":1,"t":2000,"values":[{{"tag":"{HUB_TAG}","v":21.5,"q":"good","t":2000}}]}}"#
    );
    let mut sockets = tokio::task::JoinSet::new();
    loop {
        let Ok((mut stream, _)) = listener.accept().await else {
            return;
        };
        let mut head = [0_u8; 2048];
        let mut text = String::new();
        for _ in 0..100 {
            let count = stream.peek(&mut head).await.unwrap_or(0);
            text = String::from_utf8_lossy(&head[..count]).to_ascii_lowercase();
            if text.contains("\r\n\r\n") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        if text.contains("upgrade: websocket") {
            ws_heads.lock().unwrap().push(text);
            let frame = frame.clone();
            sockets.spawn(async move {
                let Ok(mut socket) = accept_async(stream).await else {
                    return;
                };
                if !matches!(
                    tokio::time::timeout(Duration::from_secs(5), socket.next()).await,
                    Ok(Some(Ok(Message::Text(_))))
                ) {
                    return;
                }
                if socket.send(Message::Text(frame.into())).await.is_err() {
                    return;
                }
                while let Some(Ok(message)) = socket.next().await {
                    if matches!(message, Message::Close(_)) {
                        break;
                    }
                }
            });
            continue;
        }
        let body = if text.contains("/api/v1/values") {
            values.clone()
        } else {
            catalog.clone()
        };
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        loop {
            let count = stream.read(&mut buffer).await.unwrap_or(0);
            if count == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..count]);
            if request.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(response.as_bytes()).await;
    }
}

/// Hub に手動キーで接続（1 回目）→ 止める → 2 回目の起動で、DPAPI のファイルから
/// キーを読んで購読を張り直す（WS の接続要求にそのキーが載る）。キーのファイルに
/// 平文は入らない。
#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_hub_key_lives_in_the_dpapi_file_and_resume_works_after_a_restart() {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let hub_endpoint = format!("http://{}", listener.local_addr().unwrap());
    let ws_heads = Arc::new(Mutex::new(Vec::new()));
    let hub = tokio::spawn(serve_mock_hub(listener, ws_heads.clone()));

    let sandbox = Sandbox::new();
    prepare_root(&sandbox, None, true).await;

    // 1 回目: 接続（キーを保存）。
    let mut server = Server::spawn(&sandbox, sandbox.root.to_str().unwrap());
    let port = wait_ready(&sandbox, &server).await;
    let token = setup_admin(port).await;
    let (status, body) = request(
        reqwest::Method::POST,
        format!("http://127.0.0.1:{port}/api/hub/adopt-key"),
        Some(&token),
        Some(serde_json::json!({"endpoint": hub_endpoint, "key": HUB_KEY})),
    )
    .await;
    assert_eq!(status, 200, "{body} / {}", sandbox.output_text());
    server.send("ctrl-c");
    assert_eq!(
        server.wait_exit().await.code(),
        Some(0),
        "{}",
        sandbox.output_text()
    );

    let keys_dir = sandbox.layout().hub_keys_dir();
    let key_files: Vec<PathBuf> = std::fs::read_dir(&keys_dir)
        .expect("secrets\\hub-keys がある")
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(key_files.len(), 1, "{key_files:?}");
    let raw = std::fs::read(&key_files[0]).unwrap();
    assert!(
        !raw.windows(HUB_KEY.len()).any(|w| w == HUB_KEY.as_bytes()),
        "キーのファイルに平文がある"
    );
    // ディレクトリは保護された DACL（banto-secret-store の形）。
    let dacl = banto_secret_store::acl::read_dir_dacl(&keys_dir).unwrap();
    assert!(dacl.protected);

    // 2 回目: 何もしなくても、保存したキーで購読を張り直す。
    ws_heads.lock().unwrap().clear();
    let mut server = Server::spawn(&sandbox, sandbox.root.to_str().unwrap());
    let port = wait_ready(&sandbox, &server).await;
    let expected = format!("authorization: bearer {HUB_KEY}");
    wait_for(
        "再起動後の Hub の購読（保存したキーで）",
        async || {
            ws_heads
                .lock()
                .unwrap()
                .iter()
                .any(|head| head.contains(&expected))
                .then_some(())
        },
    )
    .await;
    // Hub 経由のタグの値が収集に届く。
    let token = login(port).await;
    wait_collecting(port, &token).await;
    server.send("ctrl-c");
    assert_eq!(server.wait_exit().await.code(), Some(0));
    assert_eq!(
        latest_event(&sandbox).await.as_deref(),
        Some("collection_stopped")
    );
    sandbox.assert_profile_untouched();
    hub.abort();
}
