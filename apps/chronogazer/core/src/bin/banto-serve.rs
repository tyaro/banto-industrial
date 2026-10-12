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
//! through the `auth_setup` command instead), `BANTO_VIEWER_PUBLIC` (`1` to
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
//! `BANTO_VIEWER_PUBLIC` は admin-template v3.0.0 の `banto-serve.rs` の同名の
//! 入口を写したもの（`BANTO_ALLOW_SETUP` と同じく開発・E2E 用）。保存されて
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
/// （docs/chronogazer-service-design.md §3.1(b)）。
const SHUTDOWN_BUDGET: Duration = Duration::from_secs(15);

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

    // #383 段階2b / R1-C（C-2）: `data.dir` の相対パスは **DB ファイルの置き場を
    // 基準**に解決する（デスクトップが「アプリのデータディレクトリ基準」で解決
    // しているのと同じ考え方で、そちらでも DB はそのディレクトリにある）。
    // `BANTO_DB` がファイル名だけのときは作業ディレクトリ（`"."`）を基準にする。
    //
    // 待ち受けは `BANTO_BIND`/`PORT` から決まり、保存済みの `server.*` 設定は
    // 使わない（必須: 開けなければ起動を止める）。
    let engine = match EngineRuntime::start(EngineConfig {
        data_base: EngineConfig::db_dir_of(&db_path_buf),
        db_path: db_path_buf,
        key_store,
        attach_hub: hub_memory_keys,
        host: ChronoGazerHost::Serve,
        listen: EngineListen::Required(ServerConfig { bind, port }),
        allow_setup,
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
    if allow_setup {
        println!("banto-serve: first-run setup is ENABLED (BANTO_ALLOW_SETUP=1) - POST /api/auth/setup will create the first account");
    } else {
        println!(
            "banto-serve: first-run setup is DISABLED - set BANTO_ALLOW_SETUP=1 to allow POST /api/auth/setup"
        );
    }
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
