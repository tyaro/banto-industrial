//! T14-1（docs/banto-hub-t14-design.md §3「D1 — HubRuntime ライブラリ化」・
//! §12「T14-1」）: banto-hub の起動〜シャットダウンの共通シーケンス
//! （旧 `bin/banto_hub/hub_run.rs` の `run(shutdown)`、
//! docs/tag-server-design.md §8「常駐」）を、bin 側の composition root から
//! この lib クレート（`banto_hub_core`）の再利用可能ランタイムへ抽出した
//! もの。
//!
//! [`HubRuntime::start`] はコンソールモード（`bin/banto-hub.rs`が唯一の
//! 起動経路であることは変わらない - 設計 §3.1）と Windows サービスモード
//! （`bin/banto_hub/win_service.rs`）の**両方**から呼ばれる。両者の違いは
//! 「何をもって停止シグナルとするか」だけ - コンソールモードは Ctrl-C
//! （`tokio::signal::ctrl_c`）、サービスモードは SCM からの停止コントロール
//! （`win_service`の `ServiceControl::Stop`ハンドラが叩く
//! `tokio::sync::Notify`）。旧 `run(shutdown: impl Future<Output = ()>)`は
//! この違いを1つの関数に閉じ込めていたが、T14-1 でランタイムを再利用可能な
//! ライブラリ API にするため**制御を反転**した:
//! [`HubRuntime::start`]が構築してサーバー稼働状態の[`RunningHub`]を返し、
//! 各ホストは自分の停止トリガーを待ってから[`RunningHub::shutdown`]を呼ぶ
//! （「構築 → shutdown 待機 → teardown」というホスト側のループ自体は
//! 変わらない - DB初期化・各サービス構築・`CollectorManager::rebuild`・
//! MQTT/gRPC起動・axumサーバー起動・シャットダウン順序という本体のロジックは
//! 元どおりこの1ファイルにしかない）。
//!
//! 出力は `crate::hub_log::log_line`/`log_err_line`経由（`println!`/
//! `eprintln!`の薄いラッパー - `hub_log`のモジュール doc 参照。T14-1 で
//! bin 側からこの lib クレートへ移設した - このモジュールは lib にあるため
//! bin 側限定だった旧 `hub_log`を直接参照できなかった）。コンソールモードでは
//! 元の `println!`/`eprintln!`直書きと出力内容は一切変わらない。
//!
//! ## 環境変数の読み取り位置（T14-1 で変更）
//!
//! `BANTO_DB`/`BANTO_ALLOW_SETUP`/`PORT`/`BANTO_BIND`/`BANTO_HUB_DATA`の
//! 読み取りは、旧 `hub_run::run`ではこのシーケンス自身が行っていたが、
//! T14-1 でホスト側（`bin/banto-hub.rs`・`bin/banto_hub/win_service.rs`）へ
//! 移した - 将来のデスクトップホスト（env なしで設定できる）を見据えた
//! 設計判断（docs/banto-hub-t14-design.md §3）。**このモジュール自身は
//! 環境変数を一切読まない** - ホストが読み取り結果を[`HubConfig`]に詰めて
//! 渡す。読み取りロジック自体（既定値・レイヤー順）は現行と1バイトも
//! 変えていない - 各ホストのモジュール doc 参照。
//!
//! port/bind/data_dir は settings の永続値との重ね合わせ（[`HubConfig`]の
//! `*_override`フィールドが `None` なら settings 側、`Some`なら env 側が
//! 勝つ）が必要なため、この重ね合わせ自体は（settings を読む都合上）
//! 引き続きこの中で行う（[`HubRuntime::start`]参照）。
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
//! 起動 → [`RunningHub`]を返す（旧: `shutdown`待機まで一括してから戻って
//! いたが、T14-1 で「構築して返す」だけに切り出した - 後続のシャットダウン
//! 順序は[`RunningHub::shutdown`]側に移った、中身は不変）。
//!
//! ## シャットダウン順序（T2-2、設計 §6-5 / T3、設計 §5.3 / T4、設計 §5.4）
//!
//! [`RunningHub::shutdown`]の中身: （T14-1 で追加した常駐ループ2本の
//! `abort`（このモジュール doc の「T14-1 での唯一の挙動変化」節参照）→）
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
//! ## T14-1 での唯一の挙動変化（設計 §3・§9「D7」）
//!
//! 旧 `hub_run::run`は computed 250ms 評価ループ（`tokio::spawn`した
//! `JoinHandle`を捨てていた）と tstore 剪定24hループ（同様）を「プロセス
//! 終了任せ」で止めていた。T14-1 はこの2本の `JoinHandle`を[`RunningHub`]
//! に保持し、[`RunningHub::shutdown`]の**冒頭**（"shutting down" ログの
//! 直後、`mqtt.shutdown()`より前）で `abort()` する - ライブラリとして
//! 呼び出し側に確実な clean shutdown を保証するために必要な変更（設計
//! §3「D1」）。**これが T14-1 で許容される唯一の挙動変化**であり、他は
//! 一切変えていない。外部から観測できる収集/IF の挙動は不変 - 両ループは
//! `manager.tag_map()/current_values()`や settings/data_dir を読むだけの
//! read-only ループで、副作用を持たない（設計 §9「D7」決定事項）。
//! abort の位置を「消費者を先に止める」既存シャットダウン方針（mqtt/gRPC を
//! `manager`より先に、のくだり）と揃えたのも同じ理由 - eval ループは
//! `manager`の、prune ループは settings/data_dir の読み取り専用消費者。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use banto_collect::{CollectorOptions, Quality};
use banto_core::BantoError;
use banto_server::{lan_urls, start, static_router, AuthState, RunningServer, ServerConfig};
use banto_tags::{
    CollectionGroupService, PlcConnectionInput, PlcConnectionService, TagService,
    CALC_CONNECTION_NAME, MEM_CONNECTION_NAME, VIRTUAL_PROTOCOL,
};
use banto_tstore::{LocalDate, SystemClock};
use sqlx::SqlitePool;
use thiserror::Error;
use tokio::sync::Mutex as AsyncMutex;
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;

use crate::api_keys::ApiKeysService;
use crate::assets::FrontendAssets;
use crate::audit::AuditLogService;
use crate::broker_glue::{HubSessions, SlmpSimRegistry};
use crate::computed::{load_retained_values, ComputedEngine, ServerTagStore};
use crate::db::init_db;
use crate::diag_log::DiagLog;
use crate::events::event_channel;
use crate::grpc::{GrpcServer, GrpcService};
use crate::hub::CollectorManager;
use crate::hub_log::{log_err_line, log_line};
use crate::mqtt::MqttPublisher;
use crate::rest::{api_router, audited_credential_verifier};
use crate::settings::SettingsService;
use crate::subscribe_core::EVAL_TICK_MS;
use crate::users::UsersService;
use crate::write_audit::WriteAuditService;
use crate::write_control::{load_persisted_enabled, WriteControl};
use crate::write_rate::{WriteRateLimitConfig, WriteRateLimiter};

/// `BANTO_DB`未設定時の既定 DB パス。旧 `hub_run.rs`の同名定数を移設した
/// もの - `pub`にしたのは、env 読み取りが T14-1 でホスト側
/// （`bin/banto-hub.rs`・`bin/banto_hub/win_service.rs`）へ移ったため、
/// 各ホストがこの既定値を参照する必要があるから（値そのものは不変）。
pub const DEFAULT_DB_PATH: &str = "./banto-hub.sqlite3";

/// tstore 保持期間剪定の周期（設計 §3.3: 24h）。ホストからは参照されない
/// ため非 `pub`（旧 `hub_run.rs`の同名定数のまま）。
const PRUNE_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// [`HubRuntime::start`]への入力。ホスト（コンソール/Windows サービス/
/// 将来のデスクトップシェル）が環境変数を読み、その結果をここに詰めて渡す
/// - このモジュール doc の「環境変数の読み取り位置」節参照。
#[derive(Debug, Clone)]
pub struct HubConfig {
    /// DB ファイルパス。旧 `BANTO_DB` env（既定 [`DEFAULT_DB_PATH`]）。
    pub db_path: String,
    /// `POST /api/auth/setup`を許可するか。旧 `BANTO_ALLOW_SETUP=="1"`。
    pub allow_setup: bool,
    /// settings の `server.port`を上書きする値。旧 `PORT` env
    /// （パース成功時のみ `Some`、未設定/パース失敗は `None` - 現行の
    /// `.ok().and_then(|v| v.parse().ok())`と同じ意味）。
    pub port_override: Option<u16>,
    /// settings の `server.bind`を上書きする値。旧 `BANTO_BIND` env
    /// （設定されていれば（空文字列でも）`Some`）。
    pub bind_override: Option<String>,
    /// settings の `data.dir`を上書きする値。旧 `BANTO_HUB_DATA` env。
    pub data_dir_override: Option<PathBuf>,
}

/// [`HubRuntime::start`]の失敗モード。旧 `hub_run::run`は同じ4箇所を
/// `expect()`でプロセスごと落としていた（設計 §2「現行コード地図」）。
/// T14-1 でライブラリとして呼び出し側が構造化エラーとして扱えるよう
/// `Result`化した（設計 §3「D1」）。**この4箇所以外**（`HubSessions::new`
/// 内の `BrokerSupervisor::spawn(...).expect(...)`、非致命フォールバック
/// 各所）は変更していない - `HubSessions::new`のドキュメント化済み不変条件
/// （broker_glue.rs参照）は T14-1 のスコープ外。
#[derive(Debug, Error)]
pub enum HubStartError {
    /// `db::init_db`失敗（旧: `"init_db should succeed"`で `expect`）。
    #[error("banto-hub: DB 初期化に失敗しました: {0}")]
    InitDb(BantoError),
    /// `SettingsService::server_config`失敗（旧: `"server_config should
    /// succeed"`で `expect`）。
    #[error("banto-hub: サーバー設定の読み取りに失敗しました: {0}")]
    ServerConfig(BantoError),
    /// `SettingsService::store_config`失敗（旧: `"store_config should
    /// succeed"`で `expect`）。
    #[error("banto-hub: ストア設定の読み取りに失敗しました: {0}")]
    StoreConfig(BantoError),
    /// `banto_server::start`失敗（旧: `"server should start"`で `expect`）。
    #[error("banto-hub: サーバーの起動に失敗しました: {0}")]
    ServerStart(BantoError),
}

/// 起動〜シャットダウンの共通シーケンス本体（このモジュール doc 参照）への
/// エントリポイント。中身を持たない型 - `HubRuntime::start`は関連関数
/// （`self`を取らない）。将来 controller 等を持つ拡張点が要るなら
/// [`RunningHub`]側（構築後の状態）に足す方針（T14-2 以降、設計
/// §12「T14-2」- `controller()`は T14-1 では生やさない）。
pub struct HubRuntime;

impl HubRuntime {
    /// `config`から構築し、サーバー稼働状態の[`RunningHub`]を返す。旧
    /// `hub_run::run`の「構築」部分に相当（このモジュール doc の
    /// 「起動シーケンス」節参照）。返った後、呼び出し側（ホスト）は自分の
    /// 停止トリガーを待ってから[`RunningHub::shutdown`]を呼ぶこと。
    pub async fn start(config: HubConfig) -> Result<RunningHub, HubStartError> {
        let HubConfig {
            db_path,
            allow_setup,
            port_override,
            bind_override,
            data_dir_override,
        } = config;

        let pool = init_db(&db_path).await.map_err(HubStartError::InitDb)?;

        // T6-2 (docs/tag-server-design.md §4.2/§4.3(a)): auto-provision the
        // reserved `calc`/`mem` virtual connections BEFORE the first rebuild
        // below, so an operator can start creating computed/internal tags
        // immediately - the registry's own `UNIQUE` constraint on `name` is
        // what then protects both names from ever being claimed by a real
        // connection (`banto_tags::plc_connection`'s module doc "virtual"
        // section).
        ensure_virtual_connection(&pool, CALC_CONNECTION_NAME).await;
        ensure_virtual_connection(&pool, MEM_CONNECTION_NAME).await;

        let events = event_channel();
        let users = UsersService::new(pool.clone());
        let settings = SettingsService::new(pool.clone());
        let audit = AuditLogService::new(pool.clone());
        let auth = AuthState::new(audited_credential_verifier(users.clone(), audit.clone()));

        // PORT/BANTO_BIND/BANTO_HUB_DATA (via `*_override`) override the
        // persisted settings, which in turn fall back to their own defaults
        // (8722/127.0.0.1/"./data") - same layering `banto-serve.rs` uses
        // for chronogazer's port/bind. Read BEFORE constructing
        // `CollectorManager` so the collector and the retention sweep below
        // agree on the same `data_dir`.
        let server_config = settings
            .server_config()
            .await
            .map_err(HubStartError::ServerConfig)?;
        let store_config = settings
            .store_config()
            .await
            .map_err(HubStartError::StoreConfig)?;
        let port: u16 = port_override.unwrap_or(server_config.port);
        let bind = bind_override.unwrap_or(server_config.bind);
        let data_dir = data_dir_override.unwrap_or_else(|| PathBuf::from(store_config.data_dir));

        let clock = Arc::new(SystemClock);
        // T2-2 (docs/tag-server-design.md §6-5): constructed here, OUTSIDE
        // `CollectorManager`, so an SLMP broker session survives every
        // `CollectorManager::rebuild` - see `HubSessions`'s doc comment.
        // Held as its own `Arc` (not only the clone `CollectorManager` gets)
        // so this binary can call `sessions.shutdown()` after
        // `manager.shutdown()` on the way out - see this module's doc
        // comment ("シャットダウン順序").
        let sessions = Arc::new(HubSessions::new(banto_broker::BackoffConfig::default()));

        // T9-2 (docs/ux-plan.md §1): constructed here, OUTSIDE
        // `CollectorManager`, for the same reason `sessions` is - a
        // simulator started for a `simulation = true` broker-routed SLMP
        // connection must survive every `CollectorManager::rebuild` (see
        // `SlmpSimRegistry`'s doc comment). Held as its own `Arc` so this
        // binary can call `sim_registry.shutdown()` at the correct point on
        // the way out - see this module's doc comment ("シャットダウン順序")
        // for why that is AFTER `sessions.shutdown()`.
        let sim_registry = Arc::new(SlmpSimRegistry::new());

        // T6-2 (docs/tag-server-design.md §4.2): constructed here, OUTSIDE
        // `CollectorManager`, for the same reason `sessions` is - the
        // computed engine's plan and `ServerTagStore`'s values must outlive
        // every single `rebuild`, and the background evaluation loop below
        // needs its own `Arc` clone independent of `CollectorManager`'s
        // lifecycle.
        let server_store = Arc::new(ServerTagStore::new());
        let computed_engine = Arc::new(ComputedEngine::new(server_store.clone()));

        // T6-2 (design §4.2 "retain フラグで再起動時の最終値復元"): seed
        // every persisted internal-tag value BEFORE the startup
        // rebuild/eval loop start touching the store - quality Good,
        // timestamp = the time it was saved (design: "起動時にロードして
        // ServerTagStore を初期化(品質 Good・時刻は保存時刻)"). A tag_id
        // with no persisted row here simply stays absent from the store,
        // which `hub::read_current` already reads as Bad (design:
        // "retain=false は起動時 Bad") - no special-casing needed.
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

        // Startup rebuild (design §4.3: T0 は起動時に1回). A failure here
        // (e.g. a stray invalid tag left over from a hand-edited DB) must
        // not prevent the server from starting - it surfaces via
        // `/api/v1/status`'s `last_config_error` instead, exactly like a
        // rebuild triggered by a later CRUD write. T9-2: the "simulation
        // 接続あり" startup diagnostic (docs/ux-plan.md §1,
        // accident-prevention (c)) is emitted from inside
        // `CollectorManager::rebuild` itself - it now routes through
        // `with_diag_log` (just above) to `hub_log::log_line`, so it reaches
        // the Windows service log file too (T9-2 フォローアップ
        // 2026-08-06, `crate::diag_log` モジュール doc 参照) - this call
        // already covers "hub 起動時" logging, nothing further is needed
        // here.
        if let Err(err) = manager.rebuild().await {
            log_err_line(&format!(
                "banto-hub: 起動時の collector 構築に失敗しました: {err}"
            ));
        }

        // T6-2 (design §4.2「評価タイミング」): the computed-tag 250ms
        // evaluation loop - same fixed tick (`EVAL_TICK_MS`,
        // `crate::subscribe_core`) the WS/gRPC subscription evaluators use.
        // T14-1: the `JoinHandle` is now captured (`eval_handle`) instead of
        // discarded - see this module's doc comment ("T14-1 での唯一の
        // 挙動変化"). Still runs for the process/runtime lifetime otherwise
        // (not tied to any collection start/stop - design §9 "D7").
        let eval_handle: JoinHandle<()> = {
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
            })
        };

        // Retention sweep (design §3.3: 既定7日、起動時+日次). Best-effort:
        // a prune failure must never stop the server from starting or
        // running. T14-1: the `JoinHandle` is now captured (`prune_handle`)
        // instead of discarded - see this module's doc comment ("T14-1 での
        // 唯一の挙動変化").
        prune_once(&settings, &data_dir, clock.as_ref()).await;
        let prune_handle: JoinHandle<()> = {
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
            })
        };

        let plc_connections = PlcConnectionService::new(pool.clone());
        let collection_groups = CollectionGroupService::new(pool.clone());
        let tags = TagService::new(pool.clone());
        let api_keys = ApiKeysService::new(pool.clone());

        // T2-4 (docs/tag-server-design.md §6-6): the live write-acceptance
        // flag ALWAYS constructs disabled, no matter what was persisted -
        // only `was_enabled_before_restart` (display-only,
        // `/api/v1/status`) reads the persisted value. See `WriteControl`'s
        // module doc for the one rule this exists to enforce (a restart
        // must never silently resume write acceptance).
        let write_was_enabled_persisted =
            load_persisted_enabled(&pool).await.unwrap_or_else(|err| {
                log_err_line(&format!(
                    "banto-hub: 書き込み受付の永続状態の読み取りに失敗しました: {err}"
                ));
                false
            });
        let write_control = Arc::new(WriteControl::new(write_was_enabled_persisted));
        let write_audit = WriteAuditService::new(pool.clone());

        // T3 (docs/tag-server-design.md §5.3): construct stopped, then
        // apply the persisted settings - same "constructed disabled, then
        // explicitly brought up" shape as `WriteControl` above, but here
        // `enabled` itself (not just a display-only history flag) comes
        // straight from settings - MQTT publish has no "restart always
        // disables" safety rule like the write path does (design has no
        // such requirement for T3; publishing is read-only against the tag
        // space).
        let mqtt = Arc::new(MqttPublisher::new(manager.clone()));
        let mqtt_settings = settings.mqtt_config().await.unwrap_or_else(|err| {
            log_err_line(&format!(
                "banto-hub: MQTT 設定の読み取りに失敗しました: {err}"
            ));
            crate::settings::MqttSettings::default()
        });
        mqtt.apply(&mqtt_settings).await;

        // T4 (docs/tag-server-design.md §5.4): gRPC は既定
        // disabled(§8「grpc.enabled(既定 false)」) - MqttPublisher と同じ
        // 「停止状態で構築 → 永続設定を apply」パターン。`rate_limiter` は
        // REST の書き込みハンドラ (`crate::rest::WriteState`)と**同一の**
        // `Arc` を共有する必要がある (`crate::rest::tag_space_router`の
        // フィールド doc comment参照 - 別インスタンスだとタグ毎+全体の
        // レート制限バジェットが実質2倍緩む)。
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
            crate::settings::GrpcSettings::default()
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
            .map_err(HubStartError::ServerStart)?;

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
        // このメッセージはコンソール専用の案内文だが、`HubRuntime::start`は
        // コンソール/サービス両モード共通のコードパスなので、サービスモード
        // のログファイルにもそのまま出力される（実際に押せる Ctrl-C は無いが、
        // 実害のない案内文がログに1行残るだけ）。「コンソールモードの出力は
        // 一切変更しない」という要件を優先し、モード分岐は入れていない
        // （旧 `hub_run::run`から挙動不変）。
        log_line("banto-hub: press Ctrl-C to stop");

        Ok(RunningHub {
            mqtt,
            grpc_server,
            manager,
            sessions,
            sim_registry,
            server,
            eval_handle,
            prune_handle,
        })
    }
}

/// [`HubRuntime::start`]が返す、稼働中の banto-hub の1インスタンス。
/// teardown に必要なものを保持する - [`RunningHub::shutdown`]がそれらを
/// 使ってこのモジュール doc の「シャットダウン順序」節どおりに畳む。
/// **T14-1 では `controller()` は生やさない**（設計 §12「T14-1」- 収集
/// 状態機械・controller は T14-2 以降）。
pub struct RunningHub {
    mqtt: Arc<MqttPublisher>,
    grpc_server: Arc<GrpcServer>,
    manager: Arc<CollectorManager>,
    sessions: Arc<HubSessions>,
    sim_registry: Arc<SlmpSimRegistry>,
    server: RunningServer,
    /// computed 250ms 評価ループの `JoinHandle`（T14-1 で捕捉。このモジュール
    /// doc の「T14-1 での唯一の挙動変化」節参照）。
    eval_handle: JoinHandle<()>,
    /// tstore 剪定24hループの `JoinHandle`（同上）。
    prune_handle: JoinHandle<()>,
}

impl RunningHub {
    /// axum サーバーの実バインドアドレス（`port: 0`で OS にポートを
    /// 選ばせた場合も実際の値が返る）。旧 `hub_run.rs`が `server.local_addr()`
    /// を直接使っていたのと同じ（設計 §3「D1」の `local_addr()`）。
    pub fn local_addr(&self) -> std::net::SocketAddr {
        self.server.local_addr()
    }

    /// このモジュール doc の「シャットダウン順序」節どおりに teardown する。
    /// 呼び出し側（ホスト）は自分の停止トリガー（Ctrl-C / SCM Stop /
    /// トレイ）を待ってからこれを呼ぶこと。
    pub async fn shutdown(self) {
        log_line("banto-hub: shutting down");
        // T14-1（このモジュール doc の「T14-1 での唯一の挙動変化」節・設計
        // §3「D1」・§9「D7」）: 旧 `hub_run::run`はこの2本のループの
        // `JoinHandle`を捨てて「プロセス終了任せ」で止めていた - ここが
        // T14-1 で唯一許容される挙動変化（他は一切不変）。両ループは
        // read-only（`manager`/settings・data_dir を読むだけ、副作用なし）
        // なので abort しても外部から観測できる収集/IF の挙動は変わらない。
        // 位置は「消費者を先に止める」既存方針と揃え、`mqtt.shutdown()`より
        // 前 - eval ループは `manager`の、prune ループは settings/data_dir
        // の読み取り専用消費者だから。
        self.eval_handle.abort();
        self.prune_handle.abort();

        // T3: stop the MQTT publisher (a consumer of `manager`) before
        // `manager.shutdown()` - same dependency-order reasoning as
        // `manager.shutdown()` before `sessions.shutdown()` below (stop the
        // dependent first, then the thing it depends on).
        self.mqtt.shutdown().await;
        // T4: gRPC サーバーも `manager` の消費者(read-only)なので、mqtt と
        // 同じ理由で `manager.shutdown()` より先に止める。
        self.grpc_server.shutdown().await;
        self.manager.shutdown().await;
        self.sessions.shutdown().await;
        // T9-2: simulators must outlive both the collector's tasks
        // (`manager`) and the broker sessions that may be dialing them
        // (`sessions`), so they are stopped last - see this module's doc
        // comment ("シャットダウン順序").
        self.sim_registry.shutdown().await;
        self.server.stop().await;
    }
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
/// (falling back to [`crate::settings::DEFAULT_RETENTION_DAYS`] if the
/// settings read itself fails) and delete tstore files older than that,
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
            crate::settings::DEFAULT_RETENTION_DAYS
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

#[cfg(test)]
mod tests {
    use super::*;

    /// T14-1 実装指示 §7「テスト」の最小 smoke テスト:
    /// `HubRuntime::start` → `local_addr()` → `shutdown()` が一時 DB /
    /// 一時 data_dir を使って一巡できることを確認する - レジストリが空
    /// （PLC 接続0件）でも `manager.rebuild()` は成功する
    /// （`crate::hub::tests::rebuild_on_an_empty_registry_is_not_an_error`
    /// と同じ前提）。`port_override: Some(0)`で OS に空きポートを選ばせる
    /// ので、他のテストとの bind 競合が起きない。
    ///
    /// `crate::test_support`のモジュール doc: `TempDir::drop`のリトライは
    /// マルチスレッドランタイムを要する。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn start_local_addr_then_shutdown_round_trip() {
        let dir = crate::test_support::TempDir::new("hub-runtime-smoke");
        let db_path = dir
            .path()
            .join("registry.sqlite3")
            .to_string_lossy()
            .into_owned();
        let data_dir = dir.path().join("data");

        let config = HubConfig {
            db_path,
            allow_setup: false,
            port_override: Some(0),
            bind_override: Some("127.0.0.1".to_string()),
            data_dir_override: Some(data_dir),
        };

        let hub = HubRuntime::start(config)
            .await
            .expect("HubRuntime::start should succeed against a fresh temp DB");

        let addr = hub.local_addr();
        assert_eq!(addr.ip().to_string(), "127.0.0.1");
        assert_ne!(addr.port(), 0, "the OS should have assigned a real port");

        hub.shutdown().await;
    }
}
