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

use banto_server::{lan_urls, start, static_router, ServerConfig};
use chronogazer_core::assets::FrontendAssets;
use chronogazer_core::audit::{AuditEntry, AuditLogService};
use chronogazer_core::backup::BackupService;
// #383 段階2b / R1-C（C-2）: 収集ランタイム。この単体サーバーも
// デスクトップアプリと同じく**起動時に自動開始**する（`api_router` に
// `/api/collect*` を生やす以上、ここを配線しないと「呼べるが何も走って
// いない」口になる）。
use chronogazer_core::collect::{resolve_data_dir, CollectorService};
use chronogazer_core::db::init_db;
use chronogazer_core::events::event_channel;
use chronogazer_core::hub::{HubService, UnavailableKeyStore};
use chronogazer_core::rest::{api_router, user_auth_state};
use chronogazer_core::settings::SettingsService;
use chronogazer_core::users::UsersService;
// #383 段階2a / R1-B: レジストリ3サービス。`chronogazer_core::lib.rs`の
// re-export 経由（`db::DbPool`と同じ理由 - このバイナリ自身は banto-tags を
// 直接 depend していない）。
use chronogazer_core::{CollectionGroupService, PlcConnectionService, TagService};
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
    let users = UsersService::new(pool.clone());
    let settings = SettingsService::new(pool.clone());
    let backup = BackupService::new(db_path_buf.clone(), pool.clone());
    // #383 段階2b / R1-C（C-2）: 収集サービス用の pool ハンドル。`audit` が
    // 下で `pool` を消費するので、その前に取っておく。
    let pool_for_collect = pool.clone();
    // #383 段階2a / R1-B: レジストリ3サービス。`*Service::new(pool.clone())`
    // の3行（指示書どおり）- テーブルは `db::init_db` が呼ぶ
    // `banto_tags::migrate` で既に作成済み。
    let plc_connections = PlcConnectionService::new(pool.clone());
    let collection_groups = CollectionGroupService::new(pool.clone());
    let tags = TagService::new(pool.clone());
    let audit = AuditLogService::new(pool);
    // Credential verifier from `chronogazer_core::rest` (spec §8.2),
    // backed by `UsersService`'s argon2id-hashed accounts - replaces the old
    // fixed admin/admin check that used to live here directly. Also records
    // `login`/`login_failed` audit entries (spec M14), and re-checks the
    // account on every request so deleting/demoting/re-keying it ends its
    // sessions (banto v1.7.0 #204).
    let auth = user_auth_state(users.clone(), audit.clone());

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

    // #332: この開発用サーバーには OS キーリングが無い（keyring は
    // `src-tauri` だけの依存 - ワークスペース `Cargo.toml` の注記参照）ので、
    // 書き込みが必ず失敗する `UnavailableKeyStore` を渡す。到達確認・
    // ロックダウン判定・未設定判定といった「キーを保存しない範囲」は
    // そのまま動くため、E2E はこのサーバーで実施できる。
    let hub = HubService::new(settings.clone(), std::sync::Arc::new(UnavailableKeyStore))
        .await
        .expect("HubService should initialize");

    // #383 段階1: 保存済みの接続と選択タグがあれば購読を張り直す。この
    // サーバーは `UnavailableKeyStore` なので実際には keyring からキーを
    // 取り出せず、購読状態は理由付きの「停止」になる - それでも呼ぶのは、
    // 起動経路をデスクトップと同じ形にしておくため。**起動は止めない**。
    {
        let hub = hub.clone();
        tokio::spawn(async move { hub.resume().await });
    }

    // #383 段階2b / R1-C（C-2）: 収集ランタイム。`data.dir` の相対パスは
    // **DB ファイルの置き場を基準**に解決する - デスクトップ側が
    // 「アプリのデータディレクトリ基準」で解決しているのと同じ考え方で、
    // そちらでも DB はそのディレクトリに置かれている。既定値 `"./data"` を
    // そのまま使うとプロセスの作業ディレクトリに時系列ファイルを作って
    // しまうので、ここで基準を決めておく（`resolve_data_dir` の doc）。
    // `BANTO_DB` がファイル名だけのとき `parent()` は空になるので、
    // そのときは作業ディレクトリ（`"."`）を基準にする。
    let data_base = match db_path_buf.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let store_settings = settings.store_config().await.unwrap_or_else(|err| {
        eprintln!("banto-serve: 時系列データの保存設定の読み取りに失敗しました（既定値で続行します）: {err}");
        Default::default()
    });
    let collect = CollectorService::new(
        pool_for_collect,
        resolve_data_dir(&data_base, &store_settings.data_dir),
    );

    // 起動時の自動開始（docs/r1-plan.md の R1-C）。`hub.resume()` と同じく
    // **spawn して投げっぱなし** - 失敗しても起動は止めず、理由は状態に残る
    // （`CollectorService::autostart` の doc）。**レジストリを後から編集
    // しても自動では再起動しない**（反映は `POST /api/collect/restart`）。
    //
    // **注意**: デスクトップアプリとこのサーバーを**同時に起動して同じ
    // `data.dir` を指すと二重書き込みになる**（防止機構は未実装 -
    // `chronogazer_core::collect` のモジュール doc「同じ `data.dir` を
    // 2 つのプロセスで開かないこと」）。
    {
        let collect = collect.clone();
        tokio::spawn(async move { collect.autostart().await });
    }

    let app = api_router(
        users,
        settings,
        audit,
        backup,
        hub,
        plc_connections,
        collection_groups,
        tags,
        collect.clone(),
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
    // #383 段階2b / R1-C（C-2）: 収集（書き手）は**消費者を止めた後**に止める
    // - `src-tauri` の `shutdown_app_state` と同じ順序（理由はあちらの doc）。
    // ここを通らないと、tstore の最後の未 flush 分が落ちる。失敗しても
    // 終了は止めない。
    if let Err(err) = collect.stop().await {
        eprintln!("banto-serve: 終了時の収集の停止に失敗しました: {err}");
    }
}
