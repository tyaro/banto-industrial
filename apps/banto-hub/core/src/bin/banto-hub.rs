//! banto-hub のヘッドレス起動バイナリ (docs/tag-server-design.md §3.1/§8)。
//! `apps/chronogazer/core/src/bin/banto-serve.rs` を踏襲するが、Tauri
//! アプリのプレビュー用ではなく banto-hub 自身の**唯一の**起動経路である
//! （設計 §3.1: 「Tauri は使わない」）。
//!
//! 起動シーケンス: init_db → 各サービス構築 →
//! `CollectorManager::rebuild()`（起動時1回、設計 §4.3）→ tstore 剪定
//! （起動時1回 + 24h 周期、設計 §3.3・保持既定7日）→ axum サーバー起動 →
//! Ctrl-C 待機 → Collector 停止（flush）→ サーバー停止。
//!
//! 環境変数: `PORT`（既定は settings の `server.port`、さらに未設定なら
//! 8722）、`BANTO_BIND`（既定は settings の `server.bind`、さらに未設定なら
//! `127.0.0.1`）、`BANTO_DB`（既定 `./banto-hub.sqlite3`）、`BANTO_HUB_DATA`
//! （tstore データディレクトリ。**port/bind と同じ層構造**: env
//! `BANTO_HUB_DATA` > settings の `data.dir` > 既定 `"./data"` - settings で
//! `data.dir` を変更しても bin 側が拾わない「死に設定」だった問題を
//! 監査レビューで指摘され修正、2026-08-05）、`BANTO_ALLOW_SETUP`（`1` で
//! `POST /api/auth/setup` を許可）。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use banto_collect::CollectorOptions;
use banto_hub_core::assets::FrontendAssets;
use banto_hub_core::audit::AuditLogService;
use banto_hub_core::db::init_db;
use banto_hub_core::events::event_channel;
use banto_hub_core::hub::CollectorManager;
use banto_hub_core::rest::{api_router, audited_credential_verifier};
use banto_hub_core::settings::SettingsService;
use banto_hub_core::users::UsersService;
use banto_server::{lan_urls, start, static_router, AuthState, ServerConfig};
use banto_tags::{CollectionGroupService, PlcConnectionService, TagService};
use banto_tstore::{LocalDate, SystemClock};

const DEFAULT_DB_PATH: &str = "./banto-hub.sqlite3";
const PRUNE_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

#[tokio::main]
async fn main() {
    let db_path = std::env::var("BANTO_DB").unwrap_or_else(|_| DEFAULT_DB_PATH.to_string());
    let allow_setup = std::env::var("BANTO_ALLOW_SETUP")
        .map(|value| value == "1")
        .unwrap_or(false);

    let pool = init_db(&db_path).await.expect("init_db should succeed");

    let events = event_channel();
    let users = UsersService::new(pool.clone());
    let settings = SettingsService::new(pool.clone());
    let audit = AuditLogService::new(pool.clone());
    let auth = AuthState::new(audited_credential_verifier(users.clone(), audit.clone()));

    // PORT/BANTO_BIND/BANTO_HUB_DATA override the persisted settings, which
    // in turn fall back to their own defaults (8722/127.0.0.1/"./data") -
    // same layering `banto-serve.rs` uses for chronogazer's port/bind. Read
    // BEFORE constructing `CollectorManager` so the collector and the
    // retention sweep below agree on the same `data_dir`.
    let server_config = settings
        .server_config()
        .await
        .expect("server_config should succeed");
    let store_config = settings
        .store_config()
        .await
        .expect("store_config should succeed");
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(server_config.port);
    let bind = std::env::var("BANTO_BIND").unwrap_or(server_config.bind);
    let data_dir = PathBuf::from(std::env::var("BANTO_HUB_DATA").unwrap_or(store_config.data_dir));

    let clock = Arc::new(SystemClock);
    let manager = Arc::new(CollectorManager::new(
        pool.clone(),
        data_dir.clone(),
        clock.clone(),
        CollectorOptions::default(),
    ));

    // Startup rebuild (design §4.3: T0 は起動時に1回). A failure here (e.g.
    // a stray invalid tag left over from a hand-edited DB) must not prevent
    // the server from starting - it surfaces via `/api/v1/status`'s
    // `last_config_error` instead, exactly like a rebuild triggered by a
    // later CRUD write.
    if let Err(err) = manager.rebuild().await {
        eprintln!("banto-hub: 起動時の collector 構築に失敗しました: {err}");
    }

    // Retention sweep (design §3.3: 既定7日、起動時+日次). Best-effort: a
    // prune failure must never stop the server from starting or running.
    prune_once(&settings, &data_dir, clock.as_ref()).await;
    let prune_settings = settings.clone();
    let prune_data_dir = data_dir.clone();
    let prune_clock = clock.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(PRUNE_INTERVAL);
        interval.tick().await; // first tick fires immediately; startup sweep above already ran
        loop {
            interval.tick().await;
            prune_once(&prune_settings, &prune_data_dir, prune_clock.as_ref()).await;
        }
    });

    let plc_connections = PlcConnectionService::new(pool.clone());
    let collection_groups = CollectionGroupService::new(pool.clone());
    let tags = TagService::new(pool);

    let app = api_router(
        users,
        audit,
        plc_connections,
        collection_groups,
        tags,
        manager.clone(),
        auth,
        events,
        allow_setup,
    )
    .merge(static_router::<FrontendAssets>());

    let server = start(ServerConfig { bind, port }, app)
        .await
        .expect("server should start");

    println!("banto-hub: DB at {db_path}");
    println!("banto-hub: data dir at {}", data_dir.display());
    println!("banto-hub: listening at:");
    for url in lan_urls(server.local_addr().port()) {
        println!("  {url}");
    }
    if allow_setup {
        println!("banto-hub: first-run setup is ENABLED (BANTO_ALLOW_SETUP=1) - POST /api/auth/setup will create the first account");
    } else {
        println!(
            "banto-hub: first-run setup is DISABLED - set BANTO_ALLOW_SETUP=1 to allow POST /api/auth/setup"
        );
    }
    println!("banto-hub: press Ctrl-C to stop");

    tokio::signal::ctrl_c()
        .await
        .expect("failed to listen for ctrl-c");
    println!("banto-hub: shutting down");
    manager.shutdown().await;
    server.stop().await;
}

/// One retention sweep (design §3.3): read the configured retention days
/// (falling back to [`banto_hub_core::settings::DEFAULT_RETENTION_DAYS`] if
/// the settings read itself fails) and delete tstore files older than that,
/// today computed from `clock`. Errors are logged, never fatal.
async fn prune_once(
    settings: &SettingsService,
    data_dir: &std::path::Path,
    clock: &dyn banto_tstore::Clock,
) {
    let retention_days = match settings.store_config().await {
        Ok(config) => config.retention_days,
        Err(err) => {
            eprintln!("banto-hub: 保持設定の読み取りに失敗しました: {err}");
            banto_hub_core::settings::DEFAULT_RETENTION_DAYS
        }
    };
    let retention_days = u32::try_from(retention_days).unwrap_or(7);
    let today = LocalDate::from_epoch_ms(clock.now_ms(), clock.utc_offset_ms());
    match banto_tstore::prune_files(data_dir, retention_days, today) {
        Ok(report) => {
            if !report.deleted.is_empty() {
                println!(
                    "banto-hub: tstore 保持期間剪定: {} ファイル削除",
                    report.deleted.len()
                );
            }
        }
        Err(err) => eprintln!("banto-hub: tstore 保持期間剪定に失敗しました: {err}"),
    }
}
