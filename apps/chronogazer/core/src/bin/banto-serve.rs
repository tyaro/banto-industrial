//! Standalone dev server for the embedded-server milestone (spec §11): runs
//! the full REST + static stack WITHOUT Tauri, so it can be exercised in
//! any environment - including this repo's containers, which cannot build
//! the `src-tauri` crate because they lack webkit2gtk. This is also a
//! user-facing way to preview LAN mode before the settings-screen toggle
//! (Phase B) wires it into the Tauri app itself.
//!
//! This binary always builds (bins cannot be feature-gated the way a
//! library module can); whether it serves the real frontend or the
//! built-in placeholder page depends on the `embed-ui` feature on
//! `chronogazer_core::assets::FrontendAssets`, which is applied
//! internally - this file does not need its own `#[cfg(feature = ...)]`.
//!
//! ```text
//! pnpm --filter chronogazer build   # produces apps/chronogazer/build
//! cargo run -p chronogazer-core --bin banto-serve --features embed-ui
//! ```
//!
//! (Omit `--features embed-ui` to serve the built-in placeholder page
//! instead of the real frontend build - useful for exercising the REST API
//! alone.)
//!
//! Env vars: `PORT` (default `8721`), `BANTO_BIND` (default `0.0.0.0`, so
//! the LAN-access URLs printed at startup are actually reachable - the
//! Tauri app's default of `127.0.0.1`-only is a setting applied at the
//! settings-screen layer, Phase B, not a property of this dev vehicle),
//! `BANTO_DB` (default `./banto-dev.sqlite3`), `BANTO_VIEWER_PUBLIC` (`1` to
//! seed `server.viewer_public = true` at startup so LAN clients may mint a
//! viewer-public grant session through `POST /api/auth/grant/publicViewer`
//! without logging in - ADR-0012 / ADR-0017; 2026-10-04 オーナー決定で
//! ChronoGazer でも閲覧公開を使う).
//!
//! `BANTO_HUB_MEMORY_KEYS` (`1` to keep the Hub API key in process memory
//! instead of the unavailable OS keyring, **and** to let the collector collect
//! `hub` connections - 開発・E2E 用。#383 段階3 P5): without it this server
//! cannot save a Hub key (no keyring) and a `hub` connection is left out of
//! the collection (`hubSourceUnavailable`). With it the key lives only until
//! the process exits, so the Hub link is never resumed after a restart.
//!
//! 起動と終了の組み立ては `chronogazer_core::engine::EngineRuntime`（#392 段階 B の
//! B1-1、2026-10-12）。このファイルが決めるのは環境変数の読み方と既定値・
//! 起動時の表示・Ctrl-C だけで、それらは乗り換えの前と変えていない（E2E が
//! 依存する）。乗り換えで変わったのは終了で、デスクトップと同じ順序
//! （収集の終了の印 → LAN の listener → 収集の停止（最終 flush）→ Hub の購読の
//! 停止 → DB の pool を閉じる）を [`SHUTDOWN_BUDGET`] の中で行う（前は Hub と
//! pool を閉じていなかった）。起動の失敗（同じ DB の使用中・DB を開けない・
//! Hub の初期化・ポートの使用中）は理由を標準エラー出力に出して終了コード 1 で
//! 終わる（前は Hub の初期化とポートの使用中が panic だった）。
//!
//! ## 最初の管理者: `banto-serve create-admin`（banto v8、ADR-0034 §7）
//!
//! 新しい DB はログインなしで始まり、ログインなしの REST は読み取り専用になる
//! （`POST /api/auth/setup`・`BANTO_ALLOW_SETUP` は無い）。ログインに頼る E2E は、
//! 起動の前に次を実行して管理者を作り、ログイン必須に切り替える（待ち受けずに終わる。
//! 上流 banto の `banto-serve create-admin` と同じ形）:
//!
//! ```text
//! BANTO_DB=./app.sqlite3 banto-serve create-admin --username owner [--display-name NAME] < password.txt
//! ```
//!
//! パスワードは引数に入れず標準入力の 1 行目（末尾の改行だけを落とす）。この開発用
//! サーバーは配布しないので、端末からの非表示入力は持たない（Hub の `create-admin` とは
//! 違う）。予約名 `admin` は作れない。実行の間はサーバーを止めておく（同じ DB の排他を取る）。
//! 書き込み（管理者・`auth.disabled = false`・監査）は `chronogazer-server create-admin` と同じ
//! 1 つのトランザクション（`chronogazer_core::first_admin`）。予約されたリストアがあれば止める。
//!
//! `BANTO_VIEWER_PUBLIC` は admin-template v3.0.0 の `banto-serve.rs` の同名の
//! 入口を写したもの（開発・E2E 用）。保存されて
//! いる設定を**書く**（プロセス内の上書きではない - 条件は
//! `SettingsService` から毎回読まれるため）ので、変数を外しても閲覧公開は
//! OFF に戻らない（設定画面か新しい DB で戻す）。書くのはこの 1 項目だけで、
//! `server.enabled`・bind・port はそのまま残す。

use banto_hub_bootstrap::state::memory::MemoryKeyStore;
use banto_hub_bootstrap::KeyStore;
use banto_server::ServerConfig;
use chronogazer_core::engine::{EngineConfig, EngineListen, EngineRuntime};
use chronogazer_core::hub::UnavailableKeyStore;
use chronogazer_core::instance_lock::ChronoGazerHost;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

const DEFAULT_PORT: u16 = 8721;
const DEFAULT_BIND: &str = "0.0.0.0";
const DEFAULT_DB_PATH: &str = "./banto-dev.sqlite3";

/// Ctrl-C からの終了の予算（`chronogazer_core::engine` のモジュール doc
/// 「終了の順序」）。開発用なのでデスクトップの 5 秒より長めに取り、
/// 計画中の `chronogazer-server` の Ctrl-C と同じ 15 秒にする
/// （docs/chronogazer/chronogazer-service-design.md §3.1(b)）。
const SHUTDOWN_BUDGET: Duration = Duration::from_secs(15);

/// `create-admin` の引数。パスワードは引数に入れない（プロセスの一覧と履歴に残る）。
struct CreateAdminArgs {
    username: String,
    display_name: Option<String>,
}

fn parse_create_admin_args(args: &[String]) -> Result<CreateAdminArgs, String> {
    let mut username = None;
    let mut display_name = None;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        let slot = match arg.as_str() {
            "--username" => &mut username,
            "--display-name" => &mut display_name,
            other => return Err(format!("知らない引数です: {other}")),
        };
        match rest.next() {
            Some(value) => *slot = Some(value.clone()),
            None => return Err(format!("{arg} に値がありません")),
        }
    }
    Ok(CreateAdminArgs {
        username: username.ok_or_else(|| "--username が必要です".to_string())?,
        display_name,
    })
}

/// 標準入力の 1 行目（末尾の改行 `\n`・`\r\n` だけを落とす。空白は残す）。
fn read_password_line(input: &mut impl std::io::BufRead) -> Result<String, String> {
    let mut line = String::new();
    let read = input
        .read_line(&mut line)
        .map_err(|err| format!("パスワードを読めませんでした: {err}"))?;
    if read == 0 {
        return Err("標準入力にパスワードがありません".to_string());
    }
    if line.ends_with('\n') {
        line.pop();
        if line.ends_with('\r') {
            line.pop();
        }
    }
    Ok(line)
}

/// `banto-serve create-admin`: 管理者のアカウントを作り、ログイン必須（`auth.disabled =
/// false`）にする。待ち受けずに終わる。終了コードを返す。
async fn create_admin(args: &[String], db_path: &str) -> i32 {
    let parsed = match parse_create_admin_args(args) {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("banto-serve: {message}");
            eprintln!(
                "使い方: banto-serve create-admin --username <名前> [--display-name <表示名>]（パスワードは標準入力）"
            );
            return 2;
        }
    };
    let password = match read_password_line(&mut std::io::stdin().lock()) {
        Ok(password) => password,
        Err(message) => {
            eprintln!("banto-serve: {message}。DB には触れていません");
            return 1;
        }
    };
    let display_name = parsed
        .display_name
        .clone()
        .unwrap_or_else(|| parsed.username.clone());
    // 予約されたリストアがあれば止める（`chronogazer-server create-admin` と同じ。
    // `EngineRuntime::open` が適用してしまう前に）。
    let db_path_buf = PathBuf::from(db_path);
    if let Err(err) = chronogazer_core::first_admin::refuse_a_pending_restore(&db_path_buf) {
        eprintln!("banto-serve: {err}");
        return 1;
    }
    // 起動と同じ入口（排他・旧形式の確認・migration）で DB を開く。
    let opened = match EngineRuntime::open(EngineConfig {
        drivers: EngineConfig::builtin_drivers(),
        data_base: EngineConfig::db_dir_of(&db_path_buf),
        service_data_root: None,
        db_pin: chronogazer_core::engine::DbPin::Off,
        db_path: db_path_buf,
        key_store: Arc::new(UnavailableKeyStore),
        hub_tls: chronogazer_core::hub_tls::HubTlsPolicy::production(),
        attach_hub: false,
        host: ChronoGazerHost::Serve,
        listen: EngineListen::Disabled,
        seed_viewer_public: false,
        log_label: "banto-serve",
        audit_origin: "cli",
        store_config_required: false,
        store_dir_check_required: false,
    })
    .await
    {
        Ok(opened) => opened,
        Err(err) => {
            eprintln!("banto-serve: DB を開けませんでした: {err}");
            return 1;
        }
    };
    // 管理者・ログイン必須・監査を 1 つのトランザクションで（`chronogazer-server
    // create-admin` と同じ関数）。
    let result = chronogazer_core::first_admin::create_admin_and_require_login_on(
        opened.pool(),
        chronogazer_core::first_admin::NewAdmin {
            username: &parsed.username,
            password: &password,
            display_name: &display_name,
        },
    )
    .await
    .map(|created| created.user);
    // Windows は開いた SQLite のファイルを握るので、返る前に閉じる。
    opened.close().await;
    match result {
        Ok(user) => {
            println!(
                "banto-serve: 管理者のアカウント {} を作り、ログイン必須にしました（DB: {db_path}）",
                user.username
            );
            0
        }
        Err(err) => {
            eprintln!(
                "banto-serve: 管理者のアカウントを作れませんでした: {}（アカウント・認証の設定は変更していません）",
                chronogazer_core::first_admin::failure_reason(&err)
            );
            1
        }
    }
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("create-admin") {
        let db_path = std::env::var("BANTO_DB").unwrap_or_else(|_| DEFAULT_DB_PATH.to_string());
        std::process::exit(create_admin(&args[1..], &db_path).await);
    }
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_PORT);
    let bind = std::env::var("BANTO_BIND").unwrap_or_else(|_| DEFAULT_BIND.to_string());
    let db_path = std::env::var("BANTO_DB").unwrap_or_else(|_| DEFAULT_DB_PATH.to_string());
    let seed_viewer_public = std::env::var("BANTO_VIEWER_PUBLIC")
        .map(|value| value == "1")
        .unwrap_or(false);

    let db_path_buf = PathBuf::from(&db_path);

    // #332: この開発用サーバーには OS キーリングが無い（keyring は
    // `src-tauri` だけの依存 - ワークスペース `Cargo.toml` の注記参照）ので、
    // 書き込みが必ず失敗する `UnavailableKeyStore` を渡す。到達確認・
    // ロックダウン判定・未設定判定といった「キーを保存しない範囲」は
    // そのまま動くため、E2E はこのサーバーで実施できる。
    //
    // `BANTO_HUB_MEMORY_KEYS=1`（開発・E2E 用、#383 段階3 P5）のときだけ、キーを
    // プロセス内メモリに置く `MemoryKeyStore` を渡し、収集にも Hub を繋ぐ
    // （`attach_hub`）。実際の Hub 相手の収集をこのサーバーで通すための入口で、
    // 既定ではどちらも変わらない。キーは終了で消える（再起動後の `resume` は
    // 張り直せない）。
    let hub_memory_keys = std::env::var("BANTO_HUB_MEMORY_KEYS")
        .map(|v| v == "1")
        .unwrap_or(false);
    let key_store: Arc<dyn KeyStore> = if hub_memory_keys {
        Arc::new(MemoryKeyStore::new())
    } else {
        Arc::new(UnavailableKeyStore)
    };
    // #644: Hub と話すときの TLS の決まり。既定は製品と同じ（loopback は pin した https
    // だけ）。`BANTO_HUB_LOOPBACK_HTTP=1`（開発・E2E 用。E2E のモック Hub は 127.0.0.1 の
    // http）のときだけ loopback の http を pin なしで受ける。この開発用サーバーは配布しない。
    let hub_tls = if std::env::var("BANTO_HUB_LOOPBACK_HTTP").is_ok_and(|v| v == "1") {
        eprintln!(
            "banto-serve: BANTO_HUB_LOOPBACK_HTTP=1: loopback の Hub へ http で接続します（開発・E2E 用）"
        );
        chronogazer_core::hub_tls::HubTlsPolicy::plain_loopback_http_for_dev()
    } else {
        chronogazer_core::hub_tls::HubTlsPolicy::production()
    };

    // #383 段階2b / R1-C（C-2）: `data.dir` の相対パスは **DB ファイルの置き場を
    // 基準**に解決する（デスクトップが「アプリのデータディレクトリ基準」で解決
    // しているのと同じ考え方で、そちらでも DB はそのディレクトリにある）。
    // `BANTO_DB` がファイル名だけのときは作業ディレクトリ（`"."`）を基準にする。
    //
    // 待ち受けは `BANTO_BIND`/`PORT` から決まり、保存済みの `server.*` 設定は
    // 使わない（必須: 開けなければ起動を止める）。
    let engine = match EngineRuntime::start(EngineConfig {
        drivers: EngineConfig::builtin_drivers(),
        data_base: EngineConfig::db_dir_of(&db_path_buf),
        service_data_root: None,
        db_pin: chronogazer_core::engine::DbPin::Off,
        db_path: db_path_buf,
        key_store,
        hub_tls,
        attach_hub: hub_memory_keys,
        host: ChronoGazerHost::Serve,
        listen: EngineListen::Required(ServerConfig { bind, port }),
        seed_viewer_public,
        log_label: "banto-serve",
        audit_origin: "rest",
        store_config_required: false,
        // #392: 保存先の試し書きはしない（今までどおり。開発・E2E 用で、書けなければ
        // 収集の開始がエラーを状態に残す。E2E の前提を変えないため）。
        store_dir_check_required: false,
    })
    .await
    {
        Ok(engine) => engine,
        Err(err) => {
            eprintln!("banto-serve: {err}");
            std::process::exit(1);
        }
    };

    println!("banto-serve: DB at {db_path}");
    println!("banto-serve: listening at:");
    if let Some(addr) = engine.server_slot().local_addr().await {
        for url in chronogazer_core::listening_urls(addr) {
            println!("  {url}");
        }
    }
    // banto v8（ADR-0034 §7）: 初回セットアップ（`POST /api/auth/setup`・`BANTO_ALLOW_SETUP`）は無い。
    if seed_viewer_public {
        println!("banto-serve: public viewing is ENABLED (BANTO_VIEWER_PUBLIC=1) - POST /api/auth/grant/publicViewer will hand out anonymous viewer sessions");
    }
    println!("banto-serve: press Ctrl-C to stop");

    tokio::signal::ctrl_c()
        .await
        .expect("failed to listen for ctrl-c");
    println!("banto-serve: shutting down");
    // デスクトップの `shutdown_app_state` と同じ順序（`chronogazer_core::engine`）。
    // 失敗（最終 flush・予算切れ）はエンジンが標準エラー出力に出す。
    let _report = engine.shutdown(SHUTDOWN_BUDGET).await;
}

#[cfg(test)]
mod create_admin_tests {
    use super::{parse_create_admin_args, read_password_line};

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_username_is_required_and_the_display_name_is_optional() {
        let parsed = parse_create_admin_args(&args(&["--username", "owner"])).unwrap();
        assert_eq!(parsed.username, "owner");
        assert_eq!(parsed.display_name, None);
        let parsed =
            parse_create_admin_args(&args(&["--display-name", "表示名", "--username", "owner"]))
                .unwrap();
        assert_eq!(parsed.display_name.as_deref(), Some("表示名"));
        assert!(parse_create_admin_args(&args(&[])).is_err());
        assert!(parse_create_admin_args(&args(&["--username"])).is_err());
        assert!(parse_create_admin_args(&args(&["--username", "o", "--password", "x"])).is_err());
    }

    #[test]
    fn the_password_is_the_first_line_without_its_line_ending() {
        let mut lf = std::io::Cursor::new(b"pass word \nignored\n".to_vec());
        assert_eq!(read_password_line(&mut lf).unwrap(), "pass word ");
        let mut crlf = std::io::Cursor::new(b"secret\r\n".to_vec());
        assert_eq!(read_password_line(&mut crlf).unwrap(), "secret");
        let mut empty = std::io::Cursor::new(Vec::new());
        assert!(read_password_line(&mut empty).is_err());
    }
}
