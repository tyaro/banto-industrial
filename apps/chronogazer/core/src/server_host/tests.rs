//! `crate::server_host` の単体テスト（純関数の表と、一時ルートでの起動・終了）。
//! できた bin をプロセスとして動かすテストは `tests/chronogazer_server.rs`。

use super::*;
use crate::db::Db;
use crate::settings::SettingsService;
use crate::test_support::TempDir;
use banto_hub_bootstrap::state::memory::MemoryKeyStore;
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

#[test]
fn failed_key_store_fails_closed_on_every_operation() {
    let store = FailedKeyStore::new("ACL が違います");
    for err in [
        store.get("a").unwrap_err(),
        store.set("a", "secret-value").unwrap_err(),
        store.delete("a").unwrap_err(),
    ] {
        assert_eq!(err.kind(), BootstrapErrorKind::KeyStore);
        assert!(err.to_string().contains("ACL が違います"), "{err}");
        assert!(!err.to_string().contains("secret-value"));
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
/// 消さず、設定すると自分の形の古いファイルだけ消す。
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
    log.write_line("一行目");
    // まだ保持日数を知らないので消さない（日付が変わっても）。
    clock.advance_ms(4_000);
    log.write_line("二行目");
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

    log.set_retention(Some(30));
    assert!(!old.exists(), "古い自分のログが残った");
    assert!(
        foreign.exists() && other.exists(),
        "形の違うファイルを消した"
    );
    assert!(logs.join("chronogazer-server-2026-10-13.log").exists());

    // 無制限（None）なら、日付が変わっても消さない。
    let older = logs.join("chronogazer-server-2025-06-01.log");
    std::fs::write(&older, "x").unwrap();
    log.set_retention(None);
    clock.advance_ms(DAY_MS);
    log.write_line("三行目");
    assert!(older.exists());
    // 保持日数ありで日付が変わると、そのときに消す。
    log.set_retention(Some(600));
    assert!(older.exists(), "600 日以内のものを消した");
    log.retention_days = Some(30);
    clock.advance_ms(DAY_MS);
    log.write_line("四行目");
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

fn options(root: &Path) -> ServerOptions {
    ServerOptions {
        layout: DataLayout::new(root),
        key_store: Arc::new(MemoryKeyStore::new()),
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
    let (status, body) = post_setup(&format!("http://{addr}/api/auth/setup")).await;
    assert_eq!(status, 200);
    assert_eq!(body["success"], true, "{body}");

    let summary = started.shutdown(Duration::from_secs(15)).await;
    assert!(summary.endpoint_cleared);
    assert_eq!(summary.exit_code(), EXIT_OK);
    assert!(!started.endpoint_file().exists());
    assert!(std::net::TcpStream::connect(addr).is_err());
}

/// LAN に出しているとき（bind `0.0.0.0` を 1 本にまとめる）は、最初の管理者の
/// 作成を開けない（LAN の誰でも最初の管理者になれてしまうため）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn setup_is_closed_while_exposed_to_the_lan() {
    let dir = TempDir::new();
    prepare_root(
        dir.path(),
        &[("server.enabled", "true"), ("server.bind", "0.0.0.0")],
    )
    .await;
    let started = start(options(dir.path())).await.expect("start");
    assert_eq!(started.plan().loopback.bind, "0.0.0.0");
    assert!(started.plan().exposes_lan());
    let port = started.engine().loopback_addr().await.unwrap().port();
    let (status, _) = post_setup(&format!("http://127.0.0.1:{port}/api/auth/setup")).await;
    assert_eq!(status, 403);
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
