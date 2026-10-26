//! `crate::server_host` の単体テスト（純関数の表と、一時ルートでの起動・終了）。
//! できた bin をプロセスとして動かすテストは `tests/chronogazer_server.rs`。

use super::*;
use crate::db::Db;
use crate::settings::SettingsService;
use crate::test_support::TempDir;
use banto_hub_bootstrap::state::memory::MemoryKeyStore;
use banto_safe_root::guard::KnownFolders;
use banto_safe_root::{open_root, ManagedSpec, Mode, RootSpec};
use banto_tstore::ManualClock;

fn server(enabled: bool, bind: &str) -> ServerSettings {
    ServerSettings {
        enabled,
        bind: bind.to_string(),
        port: 8721,
        viewer_public: false,
    }
}

fn cfg(bind: &str, port: u16) -> ServerConfig {
    ServerConfig {
        bind: bind.to_string(),
        port,
    }
}

// --- ルート -------------------------------------------------------------------

#[test]
fn relative_and_empty_roots_are_rejected() {
    for root in ["", "data", "./ChronoGazer", "..\\x", "ChronoGazer/sub"] {
        let err = validate_root(Path::new(root)).expect_err(root);
        assert!(!err.is_empty(), "{root}");
    }
    let err = validate_root(Path::new("data")).unwrap_err();
    assert!(err.contains("CHRONOGAZER_ROOT"), "{err}");
}

#[cfg(windows)]
#[test]
fn windows_absolute_roots_are_accepted_but_drive_relative_ones_are_not() {
    assert!(validate_root(Path::new(r"C:\ProgramData\ChronoGazer")).is_ok());
    assert!(validate_root(Path::new(r"\\server\share\ChronoGazer")).is_ok());
    // ドライブの無い `\foo` と、ドライブだけの `C:foo` は相対。
    assert!(validate_root(Path::new(r"\ChronoGazer")).is_err());
    assert!(validate_root(Path::new(r"C:ChronoGazer")).is_err());
}

/// PR #604 のレビュー: `.`・`..` を含む絶対パスも断る（文言は相対パスと同じく
/// 環境変数の名前と例を出す）。
#[test]
fn roots_with_dot_segments_are_rejected() {
    let cases: &[&str] = if cfg!(windows) {
        &[
            r"C:\ProgramData\ChronoGazer\child\..",
            r"C:\ProgramData\ChronoGazer\.",
            r"C:\ProgramData\.\ChronoGazer",
            r"C:\ProgramData\..\ChronoGazer",
            r"C:/ProgramData/ChronoGazer/child/..",
            r"C:\ProgramData\ChronoGazer\...",
            r"\\server\share\..\ChronoGazer",
        ]
    } else {
        &[
            "/var/lib/ChronoGazer/child/..",
            "/var/lib/ChronoGazer/.",
            "/var/./lib/ChronoGazer",
            "/var/lib/../ChronoGazer",
        ]
    };
    for root in cases {
        let err = validate_root(Path::new(root)).expect_err(root);
        assert!(err.contains("CHRONOGAZER_ROOT"), "{err}");
        assert!(err.contains(".."), "{err}");
    }
    // 名前の一部としての点は通す。
    let fine = if cfg!(windows) {
        r"C:\ProgramData\Chrono.Gazer\v1..2"
    } else {
        "/var/lib/Chrono.Gazer/v1..2"
    };
    assert!(validate_root(Path::new(fine)).is_ok());
}

#[cfg(not(windows))]
#[test]
fn unix_absolute_roots_are_accepted() {
    assert!(validate_root(Path::new("/var/lib/ChronoGazer")).is_ok());
}

// --- 待ち受け -----------------------------------------------------------------

/// (LAN 有効, 組み合わせが許される, bind, 期待の loopback, 期待の LAN, LAN に出るか)
type PlanCase = (
    bool,
    bool,
    &'static str,
    &'static str,
    Option<&'static str>,
    bool,
);

/// 待ち受けの形の総当たり（モジュール doc「待ち受け」）。
#[test]
fn listen_plan_table() {
    // (LAN 有効, 組み合わせが許される, bind, 期待の loopback, 期待の LAN, LAN に出るか)
    let cases: &[PlanCase] = &[
        // LAN 無効・許されない → loopback だけ。
        (false, false, "0.0.0.0", "127.0.0.1", None, false),
        (true, false, "0.0.0.0", "127.0.0.1", None, false),
        (true, false, "192.168.1.5", "127.0.0.1", None, false),
        // ワイルドカードは 1 本にまとめる（loopback を含む）。
        (true, true, "0.0.0.0", "0.0.0.0", None, true),
        (true, true, " 0.0.0.0 ", "0.0.0.0", None, true),
        // loopback そのものなら LAN の分は無い。
        (true, true, "127.0.0.1", "127.0.0.1", None, false),
        (true, true, "127.0.0.2", "127.0.0.1", None, false),
        (true, true, "localhost", "127.0.0.1", None, false),
        (true, true, "LocalHost", "127.0.0.1", None, false),
        // 特定の LAN の IP は 2 本。
        (
            true,
            true,
            "192.168.1.5",
            "127.0.0.1",
            Some("192.168.1.5"),
            true,
        ),
        (true, true, "10.0.0.1", "127.0.0.1", Some("10.0.0.1"), true),
        // IPv6 は当面対象外: `::` はワイルドカードとして扱わない（2 本）。
        (true, true, "::", "127.0.0.1", Some("::"), true),
    ];
    for &(enabled, allowed, bind, loopback, lan, exposes) in cases {
        let plan = listen_plan(&server(enabled, bind), allowed && enabled);
        let label = format!("enabled={enabled} allowed={allowed} bind={bind:?}");
        assert_eq!(plan.loopback, cfg(loopback, 8721), "{label}");
        assert_eq!(plan.lan, lan.map(|lan| cfg(lan, 8721)), "{label}");
        assert_eq!(plan.exposes_lan(), exposes, "{label}");
        // `0.0.0.0` にまとめたときだけ、開けなければ `127.0.0.1` に落とす（#598）。
        let expected_fallback = (plan.loopback.bind == "0.0.0.0").then(|| cfg("127.0.0.1", 8721));
        assert_eq!(plan.loopback_fallback, expected_fallback, "{label}");
    }
}

/// 認証モードとの組み合わせはデスクトップと同じ判定（`lan_autostart_allowed`）:
/// ログイン不要モード + LAN は閲覧公開が ON のときだけ LAN を開ける。
#[test]
fn listen_plan_from_settings_follows_the_desktop_auth_rule() {
    let lan = ServerSettings {
        enabled: true,
        bind: "192.168.1.5".to_string(),
        port: 9000,
        viewer_public: false,
    };
    let login_free = AuthSettings {
        disabled: true,
        ..AuthSettings::default()
    };
    let plan = listen_plan_from_settings(&login_free, &lan);
    assert_eq!(plan.lan, None);
    assert_eq!(plan.loopback, cfg("127.0.0.1", 9000));

    let public = ServerSettings {
        viewer_public: true,
        ..lan.clone()
    };
    assert_eq!(
        listen_plan_from_settings(&login_free, &public).lan,
        Some(cfg("192.168.1.5", 9000))
    );
    assert_eq!(
        listen_plan_from_settings(&AuthSettings::default(), &lan).lan,
        Some(cfg("192.168.1.5", 9000))
    );
}

// --- 起動の失敗 ---------------------------------------------------------------

#[test]
fn exit_codes_follow_the_service_convention() {
    assert_eq!(exit_code_for(FailureKind::AlreadyRunning), 2);
    for kind in [
        FailureKind::LegacyDatabase,
        FailureKind::StoreDirUnreadable,
        FailureKind::StoreDirUnwritable,
        FailureKind::DatabaseCorrupt,
        FailureKind::DatabaseUnopenable,
        FailureKind::DatabaseFromNewerVersion,
        FailureKind::Other,
    ] {
        assert_eq!(exit_code_for(kind), 3, "{kind:?}");
    }
}

#[test]
fn console_guidance_names_the_manual_steps_for_each_kind() {
    let layout = DataLayout::new(if cfg!(windows) {
        r"C:\cg-root"
    } else {
        "/cg-root"
    });
    let backups = layout.backups_dir().display().to_string();
    let joined = |kind| console_guidance(kind, &layout).join("\n");

    let corrupt = joined(FailureKind::DatabaseCorrupt);
    assert!(corrupt.contains(&backups), "{corrupt}");
    assert!(corrupt.contains("作り直す"), "{corrupt}");

    let unwritable = joined(FailureKind::StoreDirUnwritable);
    assert!(unwritable.contains("もう一度起動"), "{unwritable}");
    assert!(unwritable.contains("data.dir"), "{unwritable}");

    let running = joined(FailureKind::AlreadyRunning);
    assert!(!running.contains(&backups), "{running}");
    assert!(running.contains("終了してから"), "{running}");

    let newer = joined(FailureKind::DatabaseFromNewerVersion);
    assert!(
        !newer.contains("作り直す"),
        "壊れていない DB の作り直しを勧めた: {newer}"
    );

    for kind in [FailureKind::Other, FailureKind::LegacyDatabase] {
        assert!(joined(kind).contains(&layout.root().display().to_string()));
    }
}

// --- ログ ---------------------------------------------------------------------

#[test]
fn log_file_names_round_trip_and_only_the_exact_shape_parses() {
    let date = LocalDate::new(2026, 10, 3);
    assert_eq!(log_file_name(date), "chronogazer-server-2026-10-03.log");
    assert_eq!(parse_log_file_date(&log_file_name(date)), Some(date));
    for other in [
        "chronogazer-server-2026-02-30.log",
        "chronogazer-server-2026-1-03.log",
        "chronogazer-server-20261003.log",
        "chronogazer-server-2026-10-03.log.bak",
        "chronogazer-server-2026-10-03.txt",
        "banto-hub-2026-10-03.log",
        "chronogazer-server-2026-10-0x.log",
        "Chronogazer-server-2026-10-03.log",
        "",
    ] {
        assert_eq!(parse_log_file_date(other), None, "{other}");
    }
}

/// #623 PR-4: デスクトップのログ（設定モードの間、`chronogazer-desktop-YYYY-MM-DD.log`）も同じ
/// 形で読み、同じ保持日数で消す（サービスの `LogPrune` の対象に足した）。
#[test]
fn desktop_log_files_parse_and_are_pruned_with_the_same_retention() {
    let date = LocalDate::new(2026, 10, 3);
    assert_eq!(
        desktop_log_file_name(date),
        "chronogazer-desktop-2026-10-03.log"
    );
    assert_eq!(
        parse_log_file_date(&desktop_log_file_name(date)),
        Some(date)
    );
    assert_eq!(
        parse_log_file_date("chronogazer-desktop-2026-02-30.log"),
        None
    );
    assert_eq!(
        parse_log_file_date("chronogazer-desktop-2026-10-03.txt"),
        None
    );
    let today = LocalDate::new(2026, 10, 13);
    let names = [
        "chronogazer-desktop-2026-10-13.log",
        "chronogazer-desktop-2026-09-13.log",
        "chronogazer-server-2026-09-13.log",
    ];
    assert_eq!(
        log_files_to_prune(names.iter().copied(), today, 30),
        vec![
            "chronogazer-desktop-2026-09-13.log".to_string(),
            "chronogazer-server-2026-09-13.log".to_string(),
        ]
    );
}

#[test]
fn prune_keeps_retention_days_including_today_and_ignores_foreign_names() {
    let today = LocalDate::new(2026, 10, 13);
    let names = [
        "chronogazer-server-2026-10-13.log",
        "chronogazer-server-2026-09-14.log", // 30 日目（残す）
        "chronogazer-server-2026-09-13.log", // 31 日目（消す）
        "chronogazer-server-2025-01-01.log",
        "chronogazer-server-2026-09-13.log.bak",
        "notes-2025-01-01.log",
        "chronogazer-server-2099-01-01.log", // 未来の日付（時計が戻った）は残す
    ];
    assert_eq!(
        log_files_to_prune(names.iter().copied(), today, 30),
        vec![
            "chronogazer-server-2026-09-13.log".to_string(),
            "chronogazer-server-2025-01-01.log".to_string(),
        ]
    );
    // 1 日なら今日だけ残す。0 は 1 と同じ（今日のファイルは消さない）。
    for days in [0, 1] {
        assert_eq!(
            log_files_to_prune(names.iter().copied(), today, days).len(),
            3,
            "days={days}"
        );
    }
}

#[test]
fn log_retention_setting_parses_like_the_other_retention_settings() {
    assert_eq!(parse_log_retention(None), Some(30));
    assert_eq!(parse_log_retention(Some("")), Some(30));
    assert_eq!(parse_log_retention(Some("7")), Some(7));
    assert_eq!(parse_log_retention(Some(" 90 ")), Some(90));
    assert_eq!(parse_log_retention(Some("0")), None);
    assert_eq!(parse_log_retention(Some("-1")), None);
    assert_eq!(parse_log_retention(Some("abc")), Some(30));
}

const DAY_MS: i64 = 86_400_000;
const JST: i64 = 9 * 3_600_000;

/// 2026-10-13 23:59:58 JST の epoch ms。
fn just_before_midnight_jst() -> i64 {
    LocalDate::new(2026, 10, 14).to_days_since_epoch() * DAY_MS - JST - 2_000
}

/// 日付が変わると次のファイルへ移り、各行に時刻が付く。保持日数を設定するまでは
/// 消さず、設定すると自分の形の古いファイルだけ消す。削除は `ServerLog` の中では
/// 行わず、`LogPrune` として返す（返すだけでは何も消えない。#598 の Copilot の指摘:
/// ログの `Mutex` を持ったまま上限の無い削除をしない）。
#[test]
fn server_log_rotates_by_local_date_and_prunes_only_after_retention_is_known() {
    let dir = TempDir::new();
    let logs = dir.path().join("logs");
    std::fs::create_dir_all(&logs).unwrap();
    let old = logs.join("chronogazer-server-2026-01-01.log");
    let foreign = logs.join("chronogazer-server-2026-01-01.log.keep");
    let other = logs.join("other.log");
    for path in [&old, &foreign, &other] {
        std::fs::write(path, "x").unwrap();
    }

    let clock = Arc::new(ManualClock::new(just_before_midnight_jst(), JST));
    let mut log = ServerLog::open(&logs, clock.clone()).expect("open");
    assert_eq!(
        log.current_path(),
        logs.join("chronogazer-server-2026-10-13.log")
    );
    assert_eq!(log.write_line("一行目"), None);
    // まだ保持日数を知らないので消さない（日付が変わっても）。
    clock.advance_ms(4_000);
    assert_eq!(log.write_line("二行目"), None);
    assert!(old.exists(), "保持日数を読む前に消した");
    assert_eq!(
        log.current_path(),
        logs.join("chronogazer-server-2026-10-14.log")
    );
    log.flush();

    let first = std::fs::read_to_string(logs.join("chronogazer-server-2026-10-13.log")).unwrap();
    assert_eq!(first, "[2026-10-13 23:59:58] 一行目\n");
    let second = std::fs::read_to_string(logs.join("chronogazer-server-2026-10-14.log")).unwrap();
    assert_eq!(second, "[2026-10-14 00:00:02] 二行目\n");

    let prune = log
        .set_retention(Some(30))
        .expect("保持日数があれば削除を返す");
    assert!(old.exists(), "ServerLog の中で消した");
    prune.run();
    assert!(!old.exists(), "古い自分のログが残った");
    assert!(
        foreign.exists() && other.exists(),
        "形の違うファイルを消した"
    );
    assert!(logs.join("chronogazer-server-2026-10-13.log").exists());

    // 無制限（None）なら、日付が変わっても消さない。
    let older = logs.join("chronogazer-server-2025-06-01.log");
    std::fs::write(&older, "x").unwrap();
    assert_eq!(log.set_retention(None), None);
    clock.advance_ms(DAY_MS);
    assert_eq!(log.write_line("三行目"), None);
    assert!(older.exists());
    // 保持日数ありで日付が変わると、そのときの削除を返す。
    log.set_retention(Some(600)).expect("削除").run();
    assert!(older.exists(), "600 日以内のものを消した");
    log.retention_days = Some(30);
    clock.advance_ms(DAY_MS);
    let prune = log
        .write_line("四行目")
        .expect("日付の切り替えで削除を返す");
    assert!(older.exists(), "ServerLog の中で消した");
    prune.run();
    assert!(!older.exists(), "日付の切り替えで消えなかった");
}

#[test]
fn timestamps_are_local_and_zero_padded() {
    assert_eq!(format_timestamp(0, 0), "1970-01-01 00:00:00");
    let epoch_ms = LocalDate::new(2026, 1, 2).to_days_since_epoch() * DAY_MS + 3_723_000;
    assert_eq!(format_timestamp(epoch_ms, 0), "2026-01-02 01:02:03");
    assert_eq!(format_timestamp(epoch_ms, JST), "2026-01-02 10:02:03");
}

// --- 起動と終了 ---------------------------------------------------------------

/// テスト用: 一時ルートに DB を作り、待ち受けのポートを 0（OS が選ぶ）にしておく。
async fn prepare_root(root: &Path, extra: &[(&str, &str)]) {
    let layout = DataLayout::new(root);
    let pool = crate::db::init_db(layout.db_path()).await.expect("init_db");
    let settings = SettingsService::new(Db::Sqlite(pool.clone()));
    settings.set("server.port", "0").await.unwrap();
    for (key, value) in extra {
        settings.set(key, value).await.unwrap();
    }
    pool.close().await;
}

async fn get_json(url: &str) -> (u16, serde_json::Value) {
    let response = reqwest::Client::new()
        .get(url)
        .header("X-Banto-Client", "banto")
        .send()
        .await
        .expect("send");
    let status = response.status().as_u16();
    let text = response.text().await.expect("text");
    (status, serde_json::from_str(&text).unwrap_or_default())
}

/// `POST /api/auth/setup`（最初の管理者の作成）。
async fn post_setup(url: &str) -> (u16, serde_json::Value) {
    let body = serde_json::json!({
        "username": "admin",
        "password": "admin-pass-1",
        "displayName": "管理者",
    });
    let response = reqwest::Client::new()
        .post(url)
        .header("X-Banto-Client", "banto")
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .expect("send");
    let status = response.status().as_u16();
    let text = response.text().await.expect("text");
    (status, serde_json::from_str(&text).unwrap_or_default())
}

/// `POST /api/auth/grant/localDesktop`（この PC からの接続、#392 B2-1）の HTTP の状態。
async fn post_local_grant(url: &str) -> u16 {
    reqwest::Client::new()
        .post(url)
        .header("X-Banto-Client", "banto")
        .send()
        .await
        .expect("send")
        .status()
        .as_u16()
}

fn options(root: &Path) -> ServerOptions {
    ServerOptions {
        layout: DataLayout::new(root),
        key_store: Arc::new(MemoryKeyStore::new()),
        host: ChronoGazerHost::Server,
    }
}

/// 一時ルートで起動 → loopback の 1 本だけ（`127.0.0.1`）が `/api/host` に
/// `hostKind = server` で答え、`run\endpoint.json` がその port と自分の pid を指す →
/// 終了で `endpoint.json` が消え、終了コードは 0。LAN に出していないので最初の
/// 管理者の作成（`POST /api/auth/setup`）が開いている。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_serves_loopback_writes_the_endpoint_and_shutdown_clears_it() {
    let dir = TempDir::new();
    prepare_root(dir.path(), &[(LOG_RETENTION_KEY, "7")]).await;
    let started = start(options(dir.path())).await.expect("start");
    assert_eq!(started.log_retention_days(), Some(7));
    assert_eq!(started.plan().lan, None);
    assert_eq!(started.endpoint_error(), None);

    let addr = started.engine().loopback_addr().await.expect("loopback");
    assert!(addr.ip().is_loopback(), "{addr}");
    assert_eq!(started.engine().server_slot().local_addr().await, None);
    let endpoint = crate::endpoint::read_endpoint(started.endpoint_file()).expect("endpoint.json");
    assert_eq!(endpoint.port, addr.port());
    assert_eq!(endpoint.pid, std::process::id());
    assert_eq!(endpoint.host_kind, "server");
    assert_eq!(endpoint.scope, started.engine().services().host.scope);

    let (status, host) = get_json(&format!("http://{addr}/api/host")).await;
    assert_eq!(status, 200);
    assert_eq!(host["hostKind"], "server");
    // #392 B2-1: 未初期化の間は、この PC から管理者として入れる。
    assert_eq!(
        started.engine().services().local_grant,
        crate::local_grant::LocalGrantStart::Uninitialized
    );
    assert_eq!(
        post_local_grant(&format!("http://{addr}/api/auth/grant/localDesktop")).await,
        200
    );
    let (status, body) = post_setup(&format!("http://{addr}/api/auth/setup")).await;
    assert_eq!(status, 200);
    assert_eq!(body["success"], true, "{body}");

    let summary = started.shutdown(Duration::from_secs(15)).await;
    assert_eq!(summary.endpoint, crate::endpoint::EndpointClear::Removed);
    assert_eq!(summary.endpoint_diagnostic(), None);
    assert_eq!(summary.exit_code(), EXIT_OK);
    assert!(!started.endpoint_file().exists());
    assert!(std::net::TcpStream::connect(addr).is_err());
}

/// #623 PR-3（設計 §3.2 の 1）: 停止の前に新しいバックアップの受け付けを止め、進行中の物が
/// 終わるのを**打ち切らずに**待つ（待つ間は刻みごとに呼び返す）。止めた後の `POST /api/backups`
/// は 503、`GET` は通る。門の対象は `/api/backups` の下の `GET` 以外だけ（純関数）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drain_backups_waits_for_inflight_work_and_refuses_new_requests() {
    use crate::engine::BackupGate;
    use axum::http::Method;
    for (method, path, guarded) in [
        (Method::POST, "/api/backups", true),
        (Method::POST, "/api/backups/restore", true),
        (Method::POST, "/api/backups/x.sqlite3/restore", true),
        (Method::DELETE, "/api/backups/pending-restore", true),
        (Method::GET, "/api/backups", false),
        (Method::GET, "/api/backups/x.sqlite3", false),
        (Method::POST, "/api/backupsx", false),
        (Method::POST, "/api/users", false),
    ] {
        assert_eq!(
            BackupGate::guards(&method, path),
            guarded,
            "{method} {path}"
        );
    }

    let dir = TempDir::new();
    prepare_root(dir.path(), &[]).await;
    let started = start(options(dir.path())).await.expect("start");
    let addr = started.engine().loopback_addr().await.expect("loopback");
    let (status, body) = post_setup(&format!("http://{addr}/api/auth/setup")).await;
    assert_eq!(status, 200, "{body}");
    let token = body["token"].as_str().expect("token").to_string();
    let client = reqwest::Client::new();
    let get_backups = || {
        client
            .get(format!("http://{addr}/api/backups"))
            .header("X-Banto-Client", "banto")
            .header("Authorization", format!("Bearer {token}"))
            .send()
    };
    assert_eq!(get_backups().await.unwrap().status(), 200);

    let gate = started.backup_gate().clone();
    assert!(!gate.is_closed());
    let inflight = gate.try_enter().expect("開いている間は受け付ける");
    assert_eq!(gate.inflight(), 1);
    let waits = std::sync::Mutex::new(Vec::new());
    let mut on_wait = |elapsed| {
        waits.lock().unwrap().push(elapsed);
    };
    let drain = started.drain_backups(Duration::from_millis(50), &mut on_wait);
    let release = async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        drop(inflight);
    };
    tokio::join!(drain, release);
    let waits = waits.into_inner().unwrap();
    assert!(waits.len() >= 2, "待つ間に呼び返されない: {waits:?}");
    assert!(gate.is_closed());
    assert_eq!(gate.inflight(), 0);
    assert!(gate.try_enter().is_none(), "止めた後に受け付けた");
    // 止めた後の作成は 503、読むのは通る。
    let created = client
        .post(format!("http://{addr}/api/backups"))
        .header("X-Banto-Client", "banto")
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 503);
    assert_eq!(get_backups().await.unwrap().status(), 200);
    // 何も進行していなければすぐ戻る。
    let mut calls = 0;
    started
        .drain_backups(Duration::from_millis(50), &mut |_| calls += 1)
        .await;
    assert_eq!(calls, 0);
    let summary = started.shutdown(Duration::from_secs(15)).await;
    assert_eq!(summary.exit_code(), EXIT_OK);
}

/// #392 B3-2a: 渡した host の種類（サービス）が、A1 の診断ファイル・`GET /api/host`
/// の `hostKind`・`run\endpoint.json` の `host_kind` のすべてに届く（コンソールの
/// `server` の固定ではない）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_given_host_kind_reaches_the_lock_the_host_api_and_the_endpoint() {
    let dir = TempDir::new();
    prepare_root(dir.path(), &[]).await;
    let layout = DataLayout::new(dir.path());
    // Simulate the service's held root, run directory and A1 lock. Plain mode
    // is for this in-process fixture only; RealAcl requires Guarded mode.
    let root = Arc::new(
        open_root(&RootSpec {
            expected: layout.root().to_path_buf(),
            known: KnownFolders::default(),
            owners: vec![],
            create_access: None,
            create: false,
            mode: Mode::Plain,
        })
        .expect("test root"),
    );
    let run = Arc::new(
        root.open_managed(
            &[ManagedSpec {
                key: "run",
                parent: None,
                name: "run",
                create: true,
            }],
            None,
        )
        .expect("run directory")
        .take("run")
        .expect("run directory"),
    );
    let instance_lock =
        crate::instance_lock::acquire_for_db(&layout.db_path(), ChronoGazerHost::Service)
            .expect("lock");
    let locked = lock_held_with_root(layout.clone(), instance_lock, layout.db_path(), root, run);

    // A1 の診断ファイル: 2 人目は持ち主を「service」と読む。
    match crate::instance_lock::acquire_for_db(&layout.db_path(), ChronoGazerHost::Desktop) {
        Err(crate::instance_lock::InstanceLockFailure::AlreadyRunning { owner, .. }) => {
            let owner = owner.expect("診断ファイルを読める");
            assert_eq!(owner.host_kind, "service");
            assert_eq!(owner.pid, std::process::id());
        }
        Err(other) => panic!("AlreadyRunning のはず: {other}"),
        Ok(_) => panic!("同じ DB の排他を 2 つ取れた"),
    }

    let opened = locked.open().await.expect("open");
    let started = opened
        .start(Arc::new(MemoryKeyStore::new()))
        .await
        .expect("start");
    let addr = started.engine().loopback_addr().await.expect("loopback");
    let endpoint = crate::endpoint::read_endpoint(started.endpoint_file()).expect("endpoint.json");
    assert_eq!(endpoint.host_kind, "service");
    let (status, host) = get_json(&format!("http://{addr}/api/host")).await;
    assert_eq!(status, 200);
    assert_eq!(host["hostKind"], "service");
    assert_eq!(
        started.shutdown(Duration::from_secs(15)).await.exit_code(),
        EXIT_OK
    );
}

/// この PC の非 loopback の IPv4（LAN 側）。無い環境では `None`。
fn first_lan_ipv4() -> Option<std::net::Ipv4Addr> {
    banto_server::lan_urls_for_bind("0.0.0.0", 1)
        .into_iter()
        .filter_map(|url| {
            url.strip_prefix("http://")?
                .strip_suffix(":1")?
                .parse::<std::net::Ipv4Addr>()
                .ok()
        })
        .find(|ip| !ip.is_loopback())
}

/// LAN に出しているとき（bind `0.0.0.0` を 1 本にまとめる）も、最初の管理者の作成と
/// この PC からの接続（`localDesktop`）は、要求ごとに peer と Host の両方が loopback なら
/// 開く。LAN からの要求（peer が LAN）は同じ listener でも閉じる（2026-10-14 オーナー
/// 決定。B1-5 では listener 単位で、この PC からも閉じていた）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn setup_and_local_grant_follow_the_request_on_the_merged_wildcard_listener() {
    let dir = TempDir::new();
    prepare_root(
        dir.path(),
        &[("server.enabled", "true"), ("server.bind", "0.0.0.0")],
    )
    .await;
    let started = start(options(dir.path())).await.expect("start");
    assert_eq!(started.plan().loopback.bind, "0.0.0.0");
    assert!(started.plan().exposes_lan());
    assert!(started.exposes_lan().await);
    assert_eq!(
        started.engine().services().local_grant,
        crate::local_grant::LocalGrantStart::Uninitialized
    );
    let port = started.engine().loopback_addr().await.unwrap().port();

    // LAN から（peer が LAN の IP）は閉じる。LAN の IP が無い環境では確かめない。
    if let Some(lan_ip) = first_lan_ipv4() {
        let base = format!("http://{lan_ip}:{port}");
        let (status, body) = get_json(&format!("{base}/api/auth/status")).await;
        assert_eq!(status, 200, "{body}");
        assert_eq!(body["grants"]["localDesktop"], false, "{body}");
        assert_eq!(
            post_local_grant(&format!("{base}/api/auth/grant/localDesktop")).await,
            403
        );
        let (status, _) = post_setup(&format!("{base}/api/auth/setup")).await;
        assert_eq!(status, 403, "LAN から最初の管理者を作れた");
    } else {
        eprintln!(
            "skip: この環境には非 loopback の IPv4 アドレスが無い（LAN からの要求は確かめない）"
        );
    }

    // この PC から（peer も Host も 127.0.0.1）は開く。
    let base = format!("http://127.0.0.1:{port}");
    assert_eq!(
        post_local_grant(&format!("{base}/api/auth/grant/localDesktop")).await,
        200
    );
    let (status, body) = post_setup(&format!("{base}/api/auth/setup")).await;
    assert_eq!(status, 200);
    assert_eq!(body["success"], true, "{body}");
    assert_eq!(
        started.shutdown(Duration::from_secs(15)).await.exit_code(),
        0
    );
}

/// #598 のレビュー: 計画では LAN に出す（特定の IP）が、その bind が失敗して
/// loopback だけで動いているなら、最初の管理者の作成を開ける（新しい環境で作る
/// 手段を失わない）。LAN を開けなかった理由は残る。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn setup_opens_when_the_planned_lan_listener_could_not_bind() {
    let dir = TempDir::new();
    prepare_root(
        dir.path(),
        &[("server.enabled", "true"), ("server.bind", "203.0.113.1")],
    )
    .await;
    let started = start(options(dir.path())).await.expect("start");
    assert!(started.plan().exposes_lan(), "計画では LAN に出す");
    assert!(!started.exposes_lan().await, "実際には LAN に出ていない");
    assert!(started.engine().listen_error().is_some());
    let port = started.engine().loopback_addr().await.unwrap().port();
    let (status, body) = post_setup(&format!("http://127.0.0.1:{port}/api/auth/setup")).await;
    assert_eq!(status, 200);
    assert_eq!(body["success"], true, "{body}");
    assert_eq!(
        started.shutdown(Duration::from_secs(15)).await.exit_code(),
        0
    );
}

/// 同じ DB を別のプロセス（ここでは同じプロセスのデスクトップの排他）が使って
/// いれば A1 で拒否され、終了コードは 2。DB には触れない。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_desktop_holding_the_same_db_is_refused_with_exit_code_2() {
    let dir = TempDir::new();
    let layout = DataLayout::new(dir.path());
    let _desktop =
        crate::instance_lock::acquire_for_db(&layout.db_path(), ChronoGazerHost::Desktop)
            .expect("デスクトップの排他");
    let failure = match start(options(dir.path())).await {
        Ok(_) => panic!("同じ DB で起動できた"),
        Err(failure) => failure,
    };
    assert_eq!(failure.kind, FailureKind::AlreadyRunning);
    assert_eq!(failure.exit_code(), EXIT_ALREADY_RUNNING);
    assert!(
        failure.message.contains("デスクトップ"),
        "{}",
        failure.message
    );
    assert!(!layout.db_path().exists());
    assert!(!layout.endpoint_file().exists());
}

/// 保存先に書けない（`data.dir` が同名のファイル）なら、本番と同じく起動を止める
/// （`store_dir_check_required`）。終了コードは 3 で、対処の文が付く。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unwritable_store_dir_stops_the_start_with_exit_code_3() {
    let dir = TempDir::new();
    let blocker = dir.path().join("blocked");
    std::fs::write(&blocker, "x").unwrap();
    prepare_root(dir.path(), &[("data.dir", blocker.to_str().unwrap())]).await;
    let failure = match start(options(dir.path())).await {
        Ok(_) => panic!("書けない保存先で起動できた"),
        Err(failure) => failure,
    };
    assert_eq!(failure.kind, FailureKind::StoreDirUnwritable);
    assert_eq!(failure.exit_code(), EXIT_START_FAILED);
    assert!(!failure.guidance.is_empty());
    assert!(!DataLayout::new(dir.path()).endpoint_file().exists());
}

/// デスクトップの日次ログ（2026-10-24 オーナー決定「案 A」）: `chronogazer-desktop-` の形で
/// 開き、日付で切り替え、`append_desktop_log` と同じ行の形（`[YYYY-MM-DD HH:MM:SS] ...`）で
/// 同じファイルに書く。保持日数の削除はサーバーと同じく両方の形の古いファイルを消す。
#[test]
fn desktop_log_uses_the_desktop_prefix_and_shares_the_file_with_mode_switch_lines() {
    let dir = TempDir::new();
    let logs = dir.path().join("logs");
    let clock = Arc::new(ManualClock::new(just_before_midnight_jst(), JST));
    let mut log = ServerLog::open_desktop(&logs, clock.clone()).expect("open");
    assert_eq!(
        log.current_path(),
        logs.join("chronogazer-desktop-2026-10-13.log")
    );
    assert_eq!(log.write_line("診断の行"), None);
    log.flush();
    crate::mode_switch::append_desktop_log(
        &logs,
        clock.as_ref(),
        &[crate::mode_switch::DesktopLogEntry::now(
            clock.as_ref(),
            "切り替えの行",
        )],
    )
    .expect("append");
    let text = std::fs::read_to_string(logs.join("chronogazer-desktop-2026-10-13.log")).unwrap();
    assert_eq!(
        text,
        "[2026-10-13 23:59:58] 診断の行\n[2026-10-13 23:59:58] 切り替えの行\n"
    );

    log.set_retention(Some(30)).expect("削除").run();
    let old_desktop = logs.join("chronogazer-desktop-2026-01-01.log");
    let old_server = logs.join("chronogazer-server-2026-01-01.log");
    for path in [&old_desktop, &old_server] {
        std::fs::write(path, "x").unwrap();
    }
    clock.advance_ms(4_000);
    let prune = log
        .write_line("翌日の行")
        .expect("日付の切り替えで削除を返す");
    assert_eq!(
        log.current_path(),
        logs.join("chronogazer-desktop-2026-10-14.log")
    );
    prune.run();
    assert!(!old_desktop.exists() && !old_server.exists());
    assert!(logs.join("chronogazer-desktop-2026-10-13.log").exists());
}
