//! banto-hub の起動〜シャットダウンの共通シーケンス (T5-1、
//! docs/tag-server-design.md §8「常駐」)。
//!
//! [`run`] はコンソールモード（元々の `main()`のすべての中身、`bin/
//! banto-hub.rs`が唯一の起動経路であることは変わらない - 設計 §3.1）と
//! Windows サービスモード（`win_service::run_service_main`）の**両方**から
//! 呼ばれる。両者の違いは「何をもって停止シグナルとするか」だけ - コンソール
//! モードは Ctrl-C（`tokio::signal::ctrl_c`）、サービスモードは SCM からの
//! 停止コントロール（`win_service`の `ServiceControl::Stop`ハンドラが叩く
//! `tokio::sync::Notify`）。この違いを `shutdown: impl Future<Output = ()>`
//! というジェネリックパラメータに閉じ込めることで、DB初期化・各サービス
//! 構築・`CollectorManager::rebuild`・MQTT/gRPC起動・axumサーバー起動・
//! シャットダウン順序という本体のロジックは1箇所にしかない。
//!
//! 出力は `hub_log::log_line`/`log_err_line`経由（`println!`/`eprintln!`の
//! 薄いラッパー - `hub_log`のモジュール doc 参照）。コンソールモードでは
//! 元の `println!`/`eprintln!`直書きと出力内容は一切変わらない。
//!
//! ## 起動シーケンス
//!
//! init_db → 各サービス構築 → `HubSessions` 構築（T2-2、設計 §6-5。
//! `CollectorManager` の外で生存するブローカーセッション directory）→
//! `SlmpSimRegistry` 構築（T9-2、docs/ux-plan.md §1。同じく
//! `CollectorManager` の外で生存する SLMP シミュレータ registry）→
//! `CollectorManager::rebuild()`（起動時1回、設計 §4.3）→ tstore 剪定
//! （起動時1回 + 24h 周期、設計 §3.3・保持既定7日）→ `MqttPublisher`構築 +
//! settings の永続値を`apply`（T3、設計 §5.3。`mqtt.enabled=false`なら
//! 何も起動しない）→ `GrpcServer`構築 + settings の永続値を`apply`（T4、
//! 設計 §5.4。`grpc.enabled=false`(既定)なら bind しない）→ axum サーバー
//! 起動 → `shutdown`待機 → MQTT 停止 → gRPC 停止 → Collector 停止（flush）→
//! ブローカーセッション停止 → サーバー停止。
//!
//! ## シャットダウン順序（T2-2、設計 §6-5 / T3、設計 §5.3 / T4、設計 §5.4）
//!
//! `mqtt.shutdown()`（MQTT publish タスク停止）→ `grpc_server.shutdown()`
//! （gRPC サーバータスク停止）→ `manager.shutdown()`（`Collector` 停止・
//! tstore flush）→ `sessions.shutdown()`（broker タスク停止）→
//! `sim_registry.shutdown()`（T9-2、SLMP シミュレータ停止）の順を守る。
//! `manager`→`sessions`の順が先に必要な理由（逆順だと broker セッションが
//! 消えた後もまだ実行中の収集タスクが `BrokerReadClient::read_batch` を
//! 呼び、`BrokerError::TaskGone` 由来の `PlcError` を毎回受け取ってから
//! 初めて停止することになる - 実害はない、既存の read_batch エラー処理が
//! そのまま吸収する、が無駄な1サイクル分のエラー往復を避けるため、この
//! 順序を守る）に加え、`mqtt`/gRPC はどちらも`manager`の
//! `tag_map`/`current_values`を読むだけの消費者（`crate::mqtt`/`crate::grpc`
//! のモジュール doc comment参照）なので、依存する側（`mqtt`/gRPC）を先に
//! 止める（両者間の順序自体はどちらが先でもよい - 独立した消費者）。
//! `sessions`→`sim_registry`の順が最後に必要な理由（T9-2）: broker が
//! ダイヤルしている先がシミュレータのことがある（`SlmpSimRegistry`が
//! アドレスを差し替えた broker 経由 SLMP 接続）ので、シミュレータを broker
//! セッションより先に止めると、まだ止まりきっていない broker タスクが
//! 存在しない相手へ接続しようとする無駄が起きうる - シミュレータは
//! それをダイヤルする broker セッションより長生きしなければならない。
//!
//! 環境変数: `PORT`（既定は settings の `server.port`、さらに未設定なら
//! 8722）、`BANTO_BIND`（既定は settings の `server.bind`、さらに未設定なら
//! `127.0.0.1`）、`BANTO_DB`（既定 `./banto-hub.sqlite3`）、`BANTO_HUB_DATA`
//! （tstore データディレクトリ。**port/bind と同じ層構造**: env
//! `BANTO_HUB_DATA` > settings の `data.dir` > 既定 `"./data"`）、
//! `BANTO_ALLOW_SETUP`（`1` で `POST /api/auth/setup` を許可）。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use banto_collect::{CollectorOptions, Quality};
use banto_hub_core::api_keys::ApiKeysService;
use banto_hub_core::assets::FrontendAssets;
use banto_hub_core::audit::AuditLogService;
use banto_hub_core::broker_glue::{HubSessions, SlmpSimRegistry};
use banto_hub_core::computed::{load_retained_values, ComputedEngine, ServerTagStore};
use banto_hub_core::db::init_db;
use banto_hub_core::diag_log::DiagLog;
use banto_hub_core::events::event_channel;
use banto_hub_core::grpc::{GrpcServer, GrpcService};
use banto_hub_core::hub::CollectorManager;
use banto_hub_core::mqtt::MqttPublisher;
use banto_hub_core::rest::{api_router, audited_credential_verifier};
use banto_hub_core::settings::SettingsService;
use banto_hub_core::subscribe_core::EVAL_TICK_MS;
use banto_hub_core::users::UsersService;
use banto_hub_core::write_audit::WriteAuditService;
use banto_hub_core::write_control::{load_persisted_enabled, WriteControl};
use banto_hub_core::write_rate::{WriteRateLimitConfig, WriteRateLimiter};
use banto_server::{lan_urls, start, static_router, AuthState, ServerConfig};
use banto_tags::{
    CollectionGroupService, PlcConnectionInput, PlcConnectionService, TagService,
    CALC_CONNECTION_NAME, MEM_CONNECTION_NAME, VIRTUAL_PROTOCOL,
};
use banto_tstore::{LocalDate, SystemClock};
use sqlx::SqlitePool;
use tokio::sync::Mutex as AsyncMutex;
use tokio::time::MissedTickBehavior;

use crate::hub_log::{log_err_line, log_line};

const DEFAULT_DB_PATH: &str = "./banto-hub.sqlite3";
const PRUNE_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// 起動〜シャットダウンの共通シーケンス本体（このファイルのモジュール doc
/// 参照）。`shutdown`が解決したら、この関数がシャットダウン順序を実行して
/// から戻る - 呼び出し側（コンソールモードの`main`/サービスモードの
/// `win_service::run_service_main`）はその後プロセス（あるいは SCM ステータス
/// を`Stopped`に）を終了させてよい。
pub async fn run(shutdown: impl std::future::Future<Output = ()>) {
    let db_path = std::env::var("BANTO_DB").unwrap_or_else(|_| DEFAULT_DB_PATH.to_string());
    let allow_setup = std::env::var("BANTO_ALLOW_SETUP")
        .map(|value| value == "1")
        .unwrap_or(false);

    let pool = init_db(&db_path).await.expect("init_db should succeed");

    // T6-2 (docs/tag-server-design.md §4.2/§4.3(a)): auto-provision the
    // reserved `calc`/`mem` virtual connections BEFORE the first rebuild
    // below, so an operator can start creating computed/internal tags
    // immediately - the registry's own `UNIQUE` constraint on `name` is what
    // then protects both names from ever being claimed by a real connection
    // (`banto_tags::plc_connection`'s module doc "virtual" section).
    ensure_virtual_connection(&pool, CALC_CONNECTION_NAME).await;
    ensure_virtual_connection(&pool, MEM_CONNECTION_NAME).await;

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
    // T2-2 (docs/tag-server-design.md §6-5): constructed here, OUTSIDE
    // `CollectorManager`, so an SLMP broker session survives every
    // `CollectorManager::rebuild` - see `HubSessions`'s doc comment. Held as
    // its own `Arc` (not only the clone `CollectorManager` gets) so this
    // binary can call `sessions.shutdown()` after `manager.shutdown()` on the
    // way out - see this module's doc comment ("シャットダウン順序").
    let sessions = Arc::new(HubSessions::new(banto_broker::BackoffConfig::default()));

    // T9-2 (docs/ux-plan.md §1): constructed here, OUTSIDE `CollectorManager`,
    // for the same reason `sessions` is - a simulator started for a
    // `simulation = true` broker-routed SLMP connection must survive every
    // `CollectorManager::rebuild` (see `SlmpSimRegistry`'s doc comment).
    // Held as its own `Arc` so this binary can call `sim_registry.shutdown()`
    // at the correct point on the way out - see this module's doc comment
    // ("シャットダウン順序") for why that is AFTER `sessions.shutdown()`.
    let sim_registry = Arc::new(SlmpSimRegistry::new());

    // T6-2 (docs/tag-server-design.md §4.2): constructed here, OUTSIDE
    // `CollectorManager`, for the same reason `sessions` is - the computed
    // engine's plan and `ServerTagStore`'s values must outlive every single
    // `rebuild`, and the background evaluation loop below needs its own
    // `Arc` clone independent of `CollectorManager`'s lifecycle.
    let server_store = Arc::new(ServerTagStore::new());
    let computed_engine = Arc::new(ComputedEngine::new(server_store.clone()));

    // T6-2 (design §4.2 "retain フラグで再起動時の最終値復元"): seed every
    // persisted internal-tag value BEFORE the startup rebuild/eval loop
    // start touching the store - quality Good, timestamp = the time it was
    // saved (design: "起動時にロードして ServerTagStore を初期化(品質 Good・
    // 時刻は保存時刻)"). A tag_id with no persisted row here simply stays
    // absent from the store, which `hub::read_current` already reads as Bad
    // (design: "retain=false は起動時 Bad") - no special-casing needed.
    match load_retained_values(&pool).await {
        Ok(rows) => {
            for (tag_id, value, ptime_ms) in rows {
                server_store.set(
                    &format!("tag:{tag_id}"),
                    Some(value),
                    Quality::Good,
                    ptime_ms,
                );
            }
        }
        Err(err) => {
            log_err_line(&format!("banto-hub: retain 値の復元に失敗しました: {err}"));
        }
    }

    let manager = Arc::new(
        CollectorManager::new(
            pool.clone(),
            data_dir.clone(),
            clock.clone(),
            CollectorOptions::default(),
            sessions.clone(),
            sim_registry.clone(),
            computed_engine.clone(),
        )
        .with_diag_log(DiagLog::new(log_line, log_err_line)),
    );

    // Startup rebuild (design §4.3: T0 は起動時に1回). A failure here (e.g.
    // a stray invalid tag left over from a hand-edited DB) must not prevent
    // the server from starting - it surfaces via `/api/v1/status`'s
    // `last_config_error` instead, exactly like a rebuild triggered by a
    // later CRUD write. T9-2: the "simulation 接続あり" startup diagnostic
    // (docs/ux-plan.md §1, accident-prevention (c)) is emitted from inside
    // `CollectorManager::rebuild` itself - it now routes through
    // `with_diag_log` (just above) to `hub_log::log_line`, so it reaches the
    // Windows service log file too (T9-2 フォローアップ 2026-08-06,
    // `banto_hub_core::diag_log` モジュール doc 参照) - this call already
    // covers "hub 起動時" logging, nothing further is needed here.
    if let Err(err) = manager.rebuild().await {
        log_err_line(&format!(
            "banto-hub: 起動時の collector 構築に失敗しました: {err}"
        ));
    }

    // T6-2 (design §4.2「評価タイミング」): the computed-tag 250ms
    // evaluation loop - same fixed tick (`EVAL_TICK_MS`,
    // `crate::subscribe_core`) the WS/gRPC subscription evaluators use.
    // Runs for the process lifetime (no graceful shutdown handle, same as
    // the retention-sweep task below - both are read-only background loops
    // that simply stop existing when the process exits).
    {
        let eval_manager = manager.clone();
        let eval_engine = computed_engine.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_millis(EVAL_TICK_MS as u64));
            tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                let map = eval_manager.tag_map();
                let now_ms = eval_manager.clock().now_ms();
                let current = eval_manager.current_values();
                eval_engine.evaluate_tick(&map, current.as_ref(), now_ms);
            }
        });
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
    let tags = TagService::new(pool.clone());
    let api_keys = ApiKeysService::new(pool.clone());

    // T2-4 (docs/tag-server-design.md §6-6): the live write-acceptance flag
    // ALWAYS constructs disabled, no matter what was persisted - only
    // `was_enabled_before_restart` (display-only, `/api/v1/status`) reads
    // the persisted value. See `WriteControl`'s module doc for the one rule
    // this exists to enforce (a restart must never silently resume write
    // acceptance).
    let write_was_enabled_persisted = load_persisted_enabled(&pool).await.unwrap_or_else(|err| {
        log_err_line(&format!(
            "banto-hub: 書き込み受付の永続状態の読み取りに失敗しました: {err}"
        ));
        false
    });
    let write_control = Arc::new(WriteControl::new(write_was_enabled_persisted));
    let write_audit = WriteAuditService::new(pool.clone());

    // T3 (docs/tag-server-design.md §5.3): construct stopped, then apply the
    // persisted settings - same "constructed disabled, then explicitly
    // brought up" shape as `WriteControl` above, but here `enabled` itself
    // (not just a display-only history flag) comes straight from settings -
    // MQTT publish has no "restart always disables" safety rule like the
    // write path does (design has no such requirement for T3; publishing is
    // read-only against the tag space).
    let mqtt = Arc::new(MqttPublisher::new(manager.clone()));
    let mqtt_settings = settings.mqtt_config().await.unwrap_or_else(|err| {
        log_err_line(&format!(
            "banto-hub: MQTT 設定の読み取りに失敗しました: {err}"
        ));
        banto_hub_core::settings::MqttSettings::default()
    });
    mqtt.apply(&mqtt_settings).await;

    // T4 (docs/tag-server-design.md §5.4): gRPC は既定 disabled(§8「grpc.enabled
    // (既定 false)」) - MqttPublisher と同じ「停止状態で構築 → 永続設定を
    // apply」パターン。`rate_limiter` は REST の書き込みハンドラ
    // (`crate::rest::WriteState`)と**同一の** `Arc` を共有する必要がある
    // (`crate::rest::tag_space_router`のフィールド doc comment参照 - 別
    // インスタンスだとタグ毎+全体のレート制限バジェットが実質2倍緩む)。
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
        events.clone(),
    );
    let grpc_server = Arc::new(GrpcServer::new(grpc_service));
    let grpc_settings = settings.grpc_config().await.unwrap_or_else(|err| {
        log_err_line(&format!(
            "banto-hub: gRPC 設定の読み取りに失敗しました: {err}"
        ));
        banto_hub_core::settings::GrpcSettings::default()
    });
    grpc_server.apply(&grpc_settings).await;

    let app = api_router(
        users,
        audit,
        plc_connections,
        collection_groups,
        tags,
        api_keys,
        manager.clone(),
        auth,
        events,
        allow_setup,
        write_control,
        write_audit,
        mqtt.clone(),
        grpc_server.clone(),
        rate_limiter,
    )
    .merge(static_router::<FrontendAssets>());

    let server = start(ServerConfig { bind, port }, app)
        .await
        .expect("server should start");

    log_line(&format!("banto-hub: DB at {db_path}"));
    log_line(&format!("banto-hub: data dir at {}", data_dir.display()));
    log_line("banto-hub: listening at:");
    for url in lan_urls(server.local_addr().port()) {
        log_line(&format!("  {url}"));
    }
    if grpc_settings.enabled {
        log_line(&format!(
            "banto-hub: gRPC (docs/tag-server-design.md §5.4) listening on port {}",
            grpc_settings.port
        ));
    } else {
        log_line(
            "banto-hub: gRPC is DISABLED - enable it from the admin UI settings page (PUT /api/grpc-settings)",
        );
    }
    if allow_setup {
        log_line("banto-hub: first-run setup is ENABLED (BANTO_ALLOW_SETUP=1) - POST /api/auth/setup will create the first account");
    } else {
        log_line(
            "banto-hub: first-run setup is DISABLED - set BANTO_ALLOW_SETUP=1 to allow POST /api/auth/setup",
        );
    }
    // このメッセージはコンソール専用の案内文だが、`run`はコンソール/
    // サービス両モード共通のコードパスなので、サービスモードのログファイル
    // にもそのまま出力される（実際に押せる Ctrl-C は無いが、実害のない
    // 案内文がログに1行残るだけ）。「コンソールモードの出力は一切変更
    // しない」という要件を優先し、モード分岐は入れていない。
    log_line("banto-hub: press Ctrl-C to stop");

    shutdown.await;
    log_line("banto-hub: shutting down");
    // T3: stop the MQTT publisher (a consumer of `manager`) before
    // `manager.shutdown()` - same dependency-order reasoning as
    // `manager.shutdown()` before `sessions.shutdown()` below (stop the
    // dependent first, then the thing it depends on).
    mqtt.shutdown().await;
    // T4: gRPC サーバーも `manager` の消費者(read-only)なので、mqtt と同じ
    // 理由で `manager.shutdown()` より先に止める。
    grpc_server.shutdown().await;
    manager.shutdown().await;
    sessions.shutdown().await;
    // T9-2: simulators must outlive both the collector's tasks (`manager`)
    // and the broker sessions that may be dialing them (`sessions`), so they
    // are stopped last - see this module's doc comment ("シャットダウン順序").
    sim_registry.shutdown().await;
    server.stop().await;
}

/// T6-2 (docs/tag-server-design.md §4.2/§4.3(a)): create the reserved
/// `"virtual"`-protocol connection named `name` if no connection with that
/// name exists yet - idempotent across restarts (checked by name first
/// rather than relying on the registry's `UNIQUE` constraint to reject a
/// duplicate `create`, so a normal restart never even attempts - and logs -
/// a doomed insert). `host`/`port` are left at their virtual-protocol
/// defaults (empty/`0` - `banto_tags::plc_connection`'s relaxed validation
/// for `"virtual"`). A failure here (e.g. a corrupt registry) is logged and
/// never fatal - same "must not prevent the server from starting" posture
/// as the startup rebuild just above.
async fn ensure_virtual_connection(pool: &SqlitePool, name: &str) {
    let already_exists: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM plc_connections WHERE name = ?")
            .bind(name)
            .fetch_one(pool)
            .await
            .unwrap_or_else(|err| {
                log_err_line(&format!(
                    "banto-hub: 予約接続 {name} の存在確認に失敗しました: {err}"
                ));
                0
            });
    if already_exists > 0 {
        return;
    }

    let svc = PlcConnectionService::new(pool.clone());
    if let Err(err) = svc
        .create(PlcConnectionInput {
            name: name.to_string(),
            protocol: VIRTUAL_PROTOCOL.to_string(),
            host: String::new(),
            port: 0,
            unit_id: 1,
            enabled: true,
            simulation: false,
        })
        .await
    {
        log_err_line(&format!(
            "banto-hub: 予約接続 {name} の自動プロビジョニングに失敗しました: {err}"
        ));
    }
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
            log_err_line(&format!(
                "banto-hub: 保持設定の読み取りに失敗しました: {err}"
            ));
            banto_hub_core::settings::DEFAULT_RETENTION_DAYS
        }
    };
    let retention_days = u32::try_from(retention_days).unwrap_or(7);
    let today = LocalDate::from_epoch_ms(clock.now_ms(), clock.utc_offset_ms());
    match banto_tstore::prune_files(data_dir, retention_days, today) {
        Ok(report) => {
            if !report.deleted.is_empty() {
                log_line(&format!(
                    "banto-hub: tstore 保持期間剪定: {} ファイル削除",
                    report.deleted.len()
                ));
            }
        }
        Err(err) => log_err_line(&format!(
            "banto-hub: tstore 保持期間剪定に失敗しました: {err}"
        )),
    }
}
