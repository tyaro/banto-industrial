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
//! `BANTO_DB` (default `./banto-dev.sqlite3`), `BANTO_ALLOW_SETUP` (`1` to
//! enable `POST /api/auth/setup`; unset/anything else keeps it `403`'d, spec
//! §8.2 - the Tauri app never sets this, since desktop first-run goes
//! through the `auth_setup` command instead).

use chronogazer_core::assets::FrontendAssets;
use chronogazer_core::audit::{AuditEntry, AuditLogService};
use chronogazer_core::backup::BackupService;
use chronogazer_core::db::init_db;
use chronogazer_core::events::event_channel;
use chronogazer_core::items::ItemsService;
use chronogazer_core::rest::{api_router, audited_credential_verifier};
use chronogazer_core::settings::SettingsService;
use chronogazer_core::users::UsersService;
use banto_server::{lan_urls, start, static_router, AuthState, ServerConfig};
use std::path::PathBuf;

const DEFAULT_PORT: u16 = 8721;
const DEFAULT_BIND: &str = "0.0.0.0";
const DEFAULT_DB_PATH: &str = "./banto-dev.sqlite3";

#[tokio::main]
async fn main() {
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_PORT);
    let bind = std::env::var("BANTO_BIND").unwrap_or_else(|_| DEFAULT_BIND.to_string());
    let db_path = std::env::var("BANTO_DB").unwrap_or_else(|_| DEFAULT_DB_PATH.to_string());
    let allow_setup = std::env::var("BANTO_ALLOW_SETUP")
        .map(|value| value == "1")
        .unwrap_or(false);

    let db_path_buf = PathBuf::from(&db_path);

    // Apply any staged restore (spec M17) BEFORE `init_db`/the pool is
    // created - see `BackupService::apply_pending_restore_at_startup`'s doc
    // comment for why this must run first. Best-effort at the top level:
    // a failure here must not prevent the server from starting at all (the
    // old db, if any, is left untouched on error - see that function's
    // per-step safety notes).
    let applied_restore = match BackupService::apply_pending_restore_at_startup(&db_path_buf).await
    {
        Ok(applied) => applied,
        Err(err) => {
            eprintln!("banto-serve: 起動時のリストア適用に失敗しました: {err}");
            None
        }
    };

    let pool = init_db(&db_path).await.expect("init_db should succeed");

    let events = event_channel();
    let items = ItemsService::new(pool.clone()).with_events(events.clone());
    let users = UsersService::new(pool.clone());
    let settings = SettingsService::new(pool.clone());
    let backup = BackupService::new(db_path_buf, pool.clone());
    let audit = AuditLogService::new(pool);
    // Credential verifier from `chronogazer_core::rest` (spec §8.2),
    // backed by `UsersService`'s argon2id-hashed accounts - replaces the old
    // fixed admin/admin check that used to live here directly. Also records
    // `login`/`login_failed` audit entries (spec M14).
    let auth = AuthState::new(audited_credential_verifier(users.clone(), audit.clone()));

    // Spec M17: record `restore_applied` now that a real `AuditLogService`
    // exists - `apply_pending_restore_at_startup` itself cannot do this (it
    // runs before any pool/audit service exists at all).
    if let Some(applied) = applied_restore {
        audit
            .record(AuditEntry {
                actor_username: None,
                actor_role: None,
                action: "restore_applied",
                resource: "backups",
                entity_id: None,
                detail: Some(serde_json::json!({
                    "preRestoreBackupFileName": applied.pre_restore_backup_file_name,
                })),
                origin: "rest",
                result: "ok",
            })
            .await;
        println!(
            "banto-serve: 起動時にリストアを適用しました（適用前の自動バックアップ: {}）",
            applied.pre_restore_backup_file_name
        );
    }

    // Startup prune (spec M14: "サーバ起動時に1回 + list実行時に軽く" - see
    // `audit_log_list`'s doc comment in `rest.rs` for why no dedicated
    // background task is needed beyond this plus that opportunistic prune).
    // Best-effort: a prune failure here must not stop the server from
    // starting.
    match settings.audit_config().await {
        Ok(config) => {
            if let Err(err) = audit
                .prune(config.retention_days, config.retention_rows)
                .await
            {
                eprintln!("banto-serve: 起動時の監査ログの剪定に失敗しました: {err}");
            }
        }
        Err(err) => eprintln!("banto-serve: 監査ログの保持設定の読み取りに失敗しました: {err}"),
    }

    let app = api_router(
        items,
        users,
        settings,
        audit,
        backup,
        auth,
        events,
        allow_setup,
    )
    .merge(static_router::<FrontendAssets>());

    let server = start(ServerConfig { bind, port }, app)
        .await
        .expect("server should start");

    println!("banto-serve: DB at {db_path}");
    println!("banto-serve: listening at:");
    for url in lan_urls(server.local_addr().port()) {
        println!("  {url}");
    }
    if allow_setup {
        println!("banto-serve: first-run setup is ENABLED (BANTO_ALLOW_SETUP=1) - POST /api/auth/setup will create the first account");
    } else {
        println!(
            "banto-serve: first-run setup is DISABLED - set BANTO_ALLOW_SETUP=1 to allow POST /api/auth/setup"
        );
    }
    println!("banto-serve: press Ctrl-C to stop");

    tokio::signal::ctrl_c()
        .await
        .expect("failed to listen for ctrl-c");
    println!("banto-serve: shutting down");
    server.stop().await;
}
