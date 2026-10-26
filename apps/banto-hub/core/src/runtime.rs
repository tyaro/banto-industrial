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
//! ## T17-1 での唯一の例外（docs/banto-hub-t17-design.md §3「T17-1」・P2）
//!
//! 上記の「このモジュール自身は環境変数を一切読まない」原則に対し、
//! [`HubRuntime::start`]は profile 排他（[`crate::profile_lock`]）の
//! ためだけに`BANTO_HUB_ROOT`/`ProgramData`/`XDG_DATA_HOME`/`HOME`を読む -
//! [`HubConfig`]の`db_path`/`data_dir_override`は`BANTO_DB`/
//! `BANTO_HUB_DATA`で任意の場所へ上書きできてしまう（ホストが渡した値を
//! そのまま使う既存契約）ため、そこから逆算した場所に profile ロックを
//! 置くと「排他の対象にしている場所」と「実際にホストが読み書きする
//! DB/data」が食い違い得る。profile 排他は「同じ machine の同じ
//! `<profile-id>`を同時に2プロセスが運転しない」という安全機構
//! （desktop-plan §13 の最重要リスク）そのものなので、`db_path`/
//! `data_dir_override`の個別上書きとは独立に、常に
//! `crate::profile_paths::resolve_hub_root`が返す**同じ**正準ディレクトリを
//! 参照する必要がある - そのため、この1箇所だけ例外的に env を直接読む
//! （`crate::profile_paths::build_hub_config_from_env`が読む env と同じ
//! 変数・同じ優先順位関数を再利用するので、同一プロセス内で両者が食い違う
//! ことはない）。
//!
//! ## 起動シーケンス
//!
//! init_db → 各サービス構築 → `HubSessions` 構築（T2-2、設計 §6-5。
//! `CollectorManager` の外で生存するブローカーセッション directory）→
//! `BrokerSimRegistry` 構築（T9-2、docs/ux-plan.md §1。同じく
//! `CollectorManager` の外で生存する broker シミュレータ registry）→
//! `CollectorManager::rebuild()`（起動時1回、設計 §4.3）→ tstore 剪定
//! （起動時1回 + 24h 周期、設計 §3.3・保持既定7日）→ 監査ログ剪定
//! （起動時1回 + tstore と同じ24h周期タスクに相乗り、
//! docs/banto-hub-remaining-plan.md P3-a 追補・2026-08-12。banto-hub は
//! 24/7 常駐のヘッドレスサーバーで、chronogazer/relay-wright のような
//! 「人間が頻繁に再起動するデスクトップアプリ」ではないため、
//! chronogazer/relay-wright と同じ「起動時 + list実行時 opportunistic の
//! みで専用の周期タスクは持たない」だけでは、監査ログ画面を長期間開かず
//! 再起動もしない運用で剪定が実質発火しない。[`audit_prune_once`]の
//! doc comment参照）→ `MqttPublisher`構築 +
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
//! `abort`（このモジュール doc の「T14-1 での唯一の挙動変化」節参照）→
//! **`db_source.shutdown()`（外部 DB 連携 S2、DB Source の接続タスクを
//! abort して join）** → `mqtt.shutdown()`（MQTT publish タスク停止）→
//! `grpc_server.shutdown()`（gRPC サーバータスク停止）→
//! `manager.shutdown()`（`Collector` 停止・tstore flush）→
//! `sessions.shutdown()`（broker タスク停止）→
//! `sim_registry.shutdown()`（T9-2、broker シミュレータ停止）の順を守る。
//!
//! **DB Source の位置**（S2、docs/banto-hub-external-db-design.md §4.2）:
//! eval/prune ループの abort と同じ「消費者を先に止める」区画に置く -
//! DB Source のタスクは `ServerTagStore`（`manager` が
//! `computed_engine` 越しに共有するもの）へ値を書く**書き手**なので、
//! `manager.shutdown()` より確実に前で止める必要がある（順序が逆だと、
//! 畳んだ後のタグ空間へ1周期分だけ値が書き戻されうる）。eval ループの
//! `abort()` と違って `shutdown()` は abort 後に join まで待つ - タスクが
//! `PgPool` を持っており、待たずに落とすとプール解放が
//! プロセス終了任せになるため。
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
//! ダイヤルしている先がシミュレータのことがある（`BrokerSimRegistry`が
//! アドレスを差し替えた broker 経由接続）ので、シミュレータを broker
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

//! #644 exception: the optional TLS listener reads BANTO_HUB_TLS_PORT,
//! BANTO_HUB_TLS_CERT and BANTO_HUB_TLS_KEY here so all Hub hosts retain
//! their current public config shape. Incomplete settings fail before DB init.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use banto_collect::{CollectorOptions, Quality, RegistrySnapshot};
use banto_core::BantoError;
use banto_server::{
    lan_urls_for_bind, request_is_loopback_local, start, static_router,
    with_security_headers_using, RunningServer, SecurityHeaders, ServerConfig,
    TAURI_IPC_CONNECT_SRC,
};
use banto_tags::{
    CollectionGroupService, ConnectionSettingsInput, PlcConnectionService, TagService,
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
use crate::broker_glue::{BrokerSimRegistry, HubSessions};
use crate::commissioning::CommissioningService;
use crate::computed::{load_retained_values, ComputedEngine, ServerTagStore};
use crate::controller::CollectionController;
use crate::db::{init_db_with_registry, Db, InitDbError, LegacyDatabase};
use crate::diag_log::DiagLog;
use crate::events::event_channel;
use crate::grpc::{GrpcServer, GrpcService};
use crate::hub::CollectorManager;
use crate::hub_log::{log_err_line, log_line};
use crate::mqtt::MqttPublisher;
use crate::profile_lock::{
    try_acquire_profile_lock, try_acquire_profile_lock_in, HubHostKind, ProfileDirs,
    ProfileDirsError, ProfileLockError, ProfileLockGuard,
};
use crate::profile_paths::{resolve_hub_root, resolve_profile_paths, DB_FILE_NAME};
use crate::rest::{api_router_with_controller, opcua_settings_router, user_auth_state};
use crate::settings::{HubSettingsExt, SettingsService, DEFAULT_PORT};
use crate::subscribe_core::EVAL_TICK_MS;
pub use crate::tls_listener::RunningTlsListener as RunningTlsServer;
use crate::tls_listener::{self, TlsConfig};
use crate::users::UsersService;
use crate::write_audit::WriteAuditService;
use crate::write_control::{load_startup_decision, WriteControl};
use crate::write_rate::{WriteRateLimitConfig, WriteRateLimiter};

/// 旧 `BANTO_DB`未設定時の相対既定 DB パス（後方互換・ドキュメント用）。
/// T17-1（docs/banto-hub-t17-design.md §3「T17-1」・P1）以降、3ホストの
/// 既定パスの正は[`crate::profile_paths::resolve_profile_paths`] /
/// [`crate::profile_paths::build_hub_config_from_env`]が返す絶対パスであり、
/// この定数はホストの`HubConfig`組み立てでは使わない。
pub const DEFAULT_DB_PATH: &str = "./banto-hub.sqlite3";

/// tstore 保持期間剪定の周期（設計 §3.3: 24h）。ホストからは参照されない
/// ため非 `pub`（旧 `hub_run.rs`の同名定数のまま）。
const PRUNE_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// [`HubRuntime::start`]への入力。ホスト（コンソール/Windows サービス/
/// 将来のデスクトップシェル）が環境変数を読み、その結果をここに詰めて渡す
/// - このモジュール doc の「環境変数の読み取り位置」節参照。
#[derive(Debug, Clone)]
pub struct HubConfig {
    /// 注入されるドライバ一式（R1-3b、docs/banto-driver-api-design.md §6.4）。
    /// ホスト（bin・shell・テスト）が `banto_drivers::builtin()` から組み立てる。
    pub drivers: crate::broker_glue::HubDrivers,
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
    /// T17-1（docs/banto-hub-t17-design.md §3「T17-1」・P1）: この起動が
    /// 使う profile id（`crate::profile_paths::DEFAULT_PROFILE_ID`が既定）。
    /// `skip_profile_lock`が`false`の場合、`start`冒頭でこの id と
    /// （env から再解決した）root から profile ディレクトリを組み立てて
    /// 排他を取る（このモジュール doc「T17-1 での唯一の例外」節・
    /// [`HubRuntime::start`]参照 - `db_path`/`data_dir_override`は
    /// `BANTO_DB`/`BANTO_HUB_DATA`で任意の場所へ上書きできるため、そこから
    /// 逆算せず独立に持つ）。
    pub profile_id: String,
    /// T17-1（同§3・P2）: この起動を行っているホストの種別
    /// （`crate::profile_lock::ProfileOwnerInfo`の診断情報に書く）。
    pub host_kind: HubHostKind,
    /// テスト専用: `true`なら[`HubRuntime::start`]は profile 排他を一切
    /// 取らない。既存の unit/integration テストは同じプロセス内で複数の
    /// `HubRuntime`を起動することがあり（並行実行される他テストとの
    /// profile 衝突を避ける意図はそもそも無かった - 各テストが自前の一時
    /// DB/data_dir を使うだけで足りていた）、それらを`false`のままにすると
    /// 意味のない profile ロック競合が発生するため既定で`true`にして
    /// 呼び出す。
    pub skip_profile_lock: bool,
}

/// Explicit TLS listener settings for callers that manage their own temporary
/// or provisioned PEM files. The listener always binds to 127.0.0.1.
#[derive(Debug, Clone)]
pub struct HubTlsConfig {
    /// TLS port. Zero requests an OS-assigned port for an isolated test.
    pub port: u16,
    pub certificate_pem: PathBuf,
    pub private_key_pem: PathBuf,
}

/// Serve an existing Hub router on a dedicated loopback TLS listener.
/// Callers can reuse the exact router used by the normal HTTP listener.
pub async fn start_tls_router(
    router: axum::Router,
    tls: HubTlsConfig,
) -> Result<RunningTlsServer, HubStartError> {
    let config = TlsConfig::explicit(tls.port, tls.certificate_pem, tls.private_key_pem)
        .map_err(|err| HubStartError::Tls(err.to_string()))?;
    tls_listener::start(&config, router)
        .await
        .map_err(|err| HubStartError::Tls(err.to_string()))
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
    #[error("banto-hub: TLS listener setup failed: {0}")]
    Tls(String),
    /// `db::init_db`失敗（旧: `"init_db should succeed"`で `expect`）。
    #[error("banto-hub: DB 初期化に失敗しました: {0}")]
    InitDb(BantoError),
    /// DB スキーマの整理（2026-10-02 オーナー決定）: 旧形式（整理より前）の DB を開こうと
    /// した - 自動移行はせず起動を拒否する（`crate::db` のモジュール doc
    /// 「旧形式の DB の拒否」）。メッセージ自体に DB のパスと対処（削除
    /// または退避して起動し直す、手順は docs/banto-hub-operations.md）を含む。
    #[error("banto-hub: {0}")]
    LegacyDatabase(LegacyDatabase),
    /// R2b-1d（docs/banto-hub-connection-settings-design.md §6.3）: 戻せない移行
    /// （`banto-tags` の 0025）の写しでの予行が、新しい検証を通らない接続（`rows`）や
    /// 行に結び付かない問題（`problems`: 秘密値の行の数の食い違い・写しへの migrate の
    /// 失敗）を見つけた - 移行せずに起動を断る。DB は一切変わっていない。メッセージに
    /// 直し方（旧版の Hub でその接続を直すか消す）と行の一覧を含む
    /// （`crate::upgrade::PreflightRefusal` の `Display`）。コンソールの stderr・
    /// サービスのログは [`Self::LegacyDatabase`] と同じ起動失敗の経路に乗る。
    #[error("banto-hub: {}", crate::upgrade::PreflightRefusal { db_path: db_path.clone(), rows: rows.clone(), problems: problems.clone() })]
    ConnectionSettingsPreflight {
        db_path: PathBuf,
        rows: Vec<crate::upgrade::PreflightRow>,
        problems: Vec<String>,
    },
    /// R2b-1d（設計 §6.1 の 2-b・§6.4）: 移行の前後処理のそれ以外の失敗（残り物・予行の
    /// 写しを消せない・バックアップを作れない・予行そのものの失敗）。メッセージにパスと
    /// 対処を含む。DB は変わっていない。
    #[error("banto-hub: {0}")]
    Upgrade(crate::upgrade::UpgradeError),
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
    /// T17-1（docs/banto-hub-t17-design.md §3「T17-1」・P2）:
    /// `crate::profile_lock::try_acquire_profile_lock`失敗（既に別プロセスが
    /// この profile を保持している、または profile ディレクトリ・lock
    /// ファイルの I/O エラー）。DB init より前に検出するため、この4つの中で
    /// 唯一 DB を一切開かずに返る失敗モード - 安全側で起動そのものを拒否
    /// する（設計 §1 P2「失敗時は安全側で起動拒否」）。
    #[error(transparent)]
    ProfileLock(#[from] ProfileLockError),
    /// 試運転モードとロックダウン（設計 §5.6・2026-08-30 オーナー決定）:
    /// `CommissioningService::load`失敗（DB からロックダウン状態を読めない
    /// - `settings`テーブルへのアクセス自体が壊れているのと同種の異常）。
    #[error("banto-hub: 試運転モード/ロックダウン状態の読み取りに失敗しました: {0}")]
    Commissioning(BantoError),
    /// 試運転モードとロックダウン（設計 §5.6 制約1）:
    /// `crate::commissioning::enforce_loopback_when_commissioning`が非
    /// loopback バインド + 未ロックダウンを検出した - 起動そのものを
    /// 拒否する（メッセージ自体に対処法を含む、同関数の doc comment参照）。
    #[error("{0}")]
    UnsafeCommissioningBind(String),
}

/// 起動〜シャットダウンの共通シーケンス本体（このモジュール doc 参照）への
/// エントリポイント。中身を持たない型 - `HubRuntime::start`は関連関数
/// （`self`を取らない）。構築後の状態と収集制御は[`RunningHub`]側が保持し、
/// T14-2 では [`RunningHub::controller`] から直列化された controller を取得する。
pub struct HubRuntime;

impl HubRuntime {
    /// `config`から構築し、サーバー稼働状態の[`RunningHub`]を返す。旧
    /// `hub_run::run`の「構築」部分に相当（このモジュール doc の
    /// 「起動シーケンス」節参照）。返った後、呼び出し側（ホスト）は自分の
    /// 停止トリガーを待ってから[`RunningHub::shutdown`]を呼ぶこと。
    pub async fn start(config: HubConfig) -> Result<RunningHub, HubStartError> {
        Self::start_inner(config, None).await
    }

    /// [`Self::start`] の、開いて握った profile のフォルダの組（`dirs`）を受け取る版
    /// （#661 661-1。661-2 のサービスが `Mode::Guarded` で開いた組を渡す）。
    ///
    /// - 排他は `dirs` を渡して取る（`crate::profile_lock::try_acquire_profile_lock_in`。
    ///   `skip_profile_lock` なら取らない）。
    /// - 設定 DB が profile の `config\banto-hub.sqlite3` なら、`dirs.config` で確かめた
    ///   パス（`ManagedDir::reopen_path`。付随の `-wal`・`-shm`・`-journal` も）を sqlx に
    ///   渡す。データのフォルダが profile の `data\` なら `dirs.data` の本当のパスを使い、
    ///   書き込み受付の状態ファイルは `dirs.data` のハンドル基準で書く
    ///   （`WriteControl::restore_in`）。
    /// - それ以外（`BANTO_DB`・`BANTO_HUB_DATA`・設定 DB の `data.dir` が profile の外）は
    ///   渡されたパスのまま使う（[`crate::profile_paths::is_outside_profile`]。ログの
    ///   1 行は 661-2 で出す）。sqlx・tstore・banto の監査の保留はパスしか取らないので、
    ///   確かめた後もパスで渡す（ChronoGazer と同じ受け入れ）。
    pub async fn start_with_profile_dirs(
        config: HubConfig,
        dirs: ProfileDirs,
    ) -> Result<RunningHub, HubStartError> {
        Self::start_inner(config, Some(dirs)).await
    }

    async fn start_inner(
        config: HubConfig,
        given_dirs: Option<ProfileDirs>,
    ) -> Result<RunningHub, HubStartError> {
        let HubConfig {
            drivers,
            db_path,
            allow_setup,
            port_override,
            bind_override,
            data_dir_override,
            profile_id,
            host_kind,
            skip_profile_lock,
        } = config;

        // Optional dedicated loopback transport. Validate all three operator
        // settings before taking a profile lock or opening the settings DB.
        let tls_config =
            TlsConfig::from_env().map_err(|err| HubStartError::Tls(err.to_string()))?;

        // T17-1（docs/banto-hub-t17-design.md §3「T17-1」・P2）: DB init を
        // 含む一切のリソース確保より前に profile 排他を取る - 失敗した場合
        // DB を一度も開かずに即座に安全側で起動を拒否する（設計 §1 P2）。
        // このモジュール doc「T17-1 での唯一の例外」節参照: ここだけ env を
        // 直接読む。
        let profile_lock: Option<ProfileLockGuard> = if skip_profile_lock {
            None
        } else {
            let root = resolve_hub_root(
                std::env::var("BANTO_HUB_ROOT").ok().as_deref(),
                std::env::var("ProgramData").ok().as_deref(),
                std::env::var("XDG_DATA_HOME").ok().as_deref(),
                std::env::var("HOME").ok().as_deref(),
            );
            let guard = resolve_profile_paths(&root, &profile_id)
                .map_err(ProfileLockError::from)
                .and_then(|paths| match &given_dirs {
                    Some(dirs) => try_acquire_profile_lock_in(&paths, host_kind, dirs.clone()),
                    None => try_acquire_profile_lock(&paths, host_kind),
                })
                .map_err(HubStartError::ProfileLock)?;
            Some(guard)
        };

        // #661 661-1: 握った組を渡されたときだけ、profile の中の設定 DB は確かめたパスで
        // 開く（`start` の経路 = デスクトップ・コンソールは以前と同じパスのまま）。
        let db_path = match &given_dirs {
            Some(dirs) => profile_db_path(dirs, db_path)
                .map_err(|err| HubStartError::ProfileLock(ProfileLockError::Dirs(err)))?,
            None => db_path,
        };

        // R2b-1d（docs/banto-hub-connection-settings-design.md §6.1）: 2-a〜2-c。レジストリは
        // `drivers` を `HubSessions` へ渡す前に写しておく（予行の preflight・入口の
        // `with_registry`・予約接続が使う）。
        let registry = drivers.registry.clone();
        let pool = init_db_with_registry(&db_path, &registry)
            .await
            .map_err(|err| match err {
                InitDbError::Legacy(legacy) => HubStartError::LegacyDatabase(legacy),
                InitDbError::Upgrade(crate::upgrade::UpgradeError::Preflight(refusal)) => {
                    HubStartError::ConnectionSettingsPreflight {
                        db_path: refusal.db_path,
                        rows: refusal.rows,
                        problems: refusal.problems,
                    }
                }
                InitDbError::Upgrade(err) => HubStartError::Upgrade(err),
                InitDbError::Storage(err) => HubStartError::InitDb(err),
            })?;

        // R2b-1d（設計 §5.3 の最後の行、毎回の起動）: 秘密値の行と `settings` の参照の
        // 突き合わせ。参照があって行が無い接続はログに出す（読む側は `Unreadable {
        // Missing }` として空のパスワードで繋がない）。参照の無い行は消す。失敗しても
        // 起動は止めない（読む側の判断は行ごとに同じ）。後処理より先に行う: 消した参照の無い
        // `plain-v1` の行は migration 0026 のトリガが清掃を記録するので、同じ起動の後処理の
        // `VACUUM` で空きページと WAL から消える（#778 のレビュー P2）。
        match crate::upgrade::check_secret_integrity(&pool).await {
            Ok(report) => crate::upgrade::log_integrity(&report),
            Err(err) => log_err_line(&format!(
                "banto-hub: [WARN] 秘密値の整合の確認に失敗しました: {err}"
            )),
        }

        // R2b-1e（設計 §6.1 の 3・§6.5・§5.8、毎回の起動・冪等）: 秘密値の後処理。Windows は
        // `plain-v1` の行と未適用キューを DPAPI で封印し直し、履歴の行からパスワードを外し、
        // 清掃待ちの印があれば VACUUM する。どの失敗も起動を止めない（行は今の守りのまま
        // 残り、状態の画面に出る。次の起動でやり直す）。Windows 以外は平文（`plain-v1`）で
        // 保存していることをログに 1 行出す。
        let connection_secrets = crate::secrets::ConnectionSecrets::platform_default();
        if connection_secrets.seal_scheme() == crate::secrets::SecretScheme::PlainV1 {
            log_err_line(&format!(
                "banto-hub: [WARN] {}（postgres 接続のパスワードは DB に plain-v1 で置きます）",
                crate::secrets::PLAINTEXT_STORAGE_WARNING
            ));
        }
        let upkeep = crate::upgrade::secrets_upkeep(&pool, &connection_secrets).await;
        crate::upgrade::log_upkeep(&upkeep);

        // T6-2 (docs/tag-server-design.md §4.2/§4.3(a)): auto-provision the
        // reserved `calc`/`mem` virtual connections BEFORE the first rebuild
        // below, so an operator can start creating computed/internal tags
        // immediately - the registry's own `UNIQUE` constraint on `name` is
        // what then protects both names from ever being claimed by a real
        // connection (`banto_tags::plc_connection`'s module doc "virtual"
        // section).
        ensure_virtual_connection(&pool, &registry, CALC_CONNECTION_NAME).await;
        ensure_virtual_connection(&pool, &registry, MEM_CONNECTION_NAME).await;

        let events = event_channel();
        let users = UsersService::new(Db::Sqlite(pool.clone()));
        let settings = SettingsService::new(Db::Sqlite(pool.clone()));

        // PORT/BANTO_BIND/BANTO_HUB_DATA (via `*_override`) override the
        // persisted settings, which in turn fall back to their own defaults
        // (8722/127.0.0.1/"./data") - same layering `banto-serve.rs` uses
        // for chronogazer's port/bind. Read BEFORE constructing
        // `CollectorManager` so the collector and the retention sweep below
        // agree on the same `data_dir`.
        let server_config = settings
            .hub_server_config()
            .await
            .map_err(HubStartError::ServerConfig)?;
        let store_config = settings
            .store_config()
            .await
            .map_err(HubStartError::StoreConfig)?;
        let port: u16 = port_override.unwrap_or(server_config.port);
        let bind = bind_override.unwrap_or(server_config.bind);
        let data_dir = data_dir_override.unwrap_or_else(|| PathBuf::from(store_config.data_dir));
        // #661 661-1: profile の `data\` なら、握ったフォルダの本当のパスに揃え、状態ファイルは
        // そのハンドルで書く。外（`BANTO_HUB_DATA` など）はパスのまま。
        let held_data = given_dirs
            .as_ref()
            .filter(|dirs| is_profile_dir(&data_dir, &dirs.data))
            .map(|dirs| dirs.data.clone());
        let data_dir = match &held_data {
            Some(dir) => dir.final_path().to_path_buf(),
            None => data_dir,
        };

        // #437（banto ADR-0019）: 監査は保留つきで組む - DB に書けない・
        // 応答しない監査はデータディレクトリの保留ファイルに退避し、`record` は
        // 3 秒で戻る（`crate::audit_spool` のモジュール doc）。保留の置き場所が
        // データディレクトリなので、その実効値が決まったここで組む（以前は
        // `users`/`settings` と並べて組んでいたが、`auth` を含めて上の設定の
        // 読み取りでは使っていない）。マイグレーション（`init_db`、`0008` の
        // `pending_id` を含む）は済んでいるので、前回の実行で残った保留を
        // ここで 1 回流し込み、以後は定期実行に任せる（ハンドルは
        // `RunningHub::shutdown` で abort する）。
        let audit = crate::audit_spool::build_audit_service(Db::Sqlite(pool.clone()), &data_dir);
        match audit.flush_spool().await {
            Ok(report) => {
                if report.flushed > 0
                    || report.remaining > 0
                    || report.quarantined > 0
                    || report.error.is_some()
                {
                    log_line(&format!(
                        "banto-hub: 監査の保留: 起動時に {} 件を DB へ流し込みました\
                         （残り {} 件、読めずに隔離 {} 件{}）",
                        report.flushed,
                        report.remaining,
                        report.quarantined,
                        report
                            .error
                            .as_ref()
                            .map(|err| format!("、中断: {err}"))
                            .unwrap_or_default()
                    ));
                }
            }
            Err(err) => log_err_line(&format!(
                "banto-hub: [WARN] 監査の保留ディレクトリを読めませんでした: {err}"
            )),
        }
        // 定期の流し込み（`spawn_spool_flusher`）は、ここでは起こさない。以降にも
        // 失敗しうる起動手順（試運転モードの読み込み・bind 検査・サーバー起動）が
        // あり、そこで失敗して戻ると `JoinHandle` が捨てられて「止まらないまま
        // 残る」ため（デスクトップのトレイ再試行で起動のたびに積み上がる。#557 の
        // レビュー指摘）。最後に失敗しうる手順（サーバー起動）の後で起こす。
        let auth = user_auth_state(users.clone(), audit.clone());

        // 試運転モードとロックダウン（docs/tag-server-design.md §5.6・
        // 2026-08-30 オーナー決定）: DB の永続状態からロックダウン済みかを
        // 解決する（`CommissioningService::load`・`crate::commissioning`の
        // モジュール doc 参照 - フラグ未設定は常に試運転モード、ユーザーの
        // 有無は判定に関与しない）。
        let commissioning =
            CommissioningService::load(settings.clone(), users.clone(), auth.clone())
                .await
                .map_err(HubStartError::Commissioning)?;

        // 制約1（設計 §5.6）: 非 loopback バインド（`bind`が`0.0.0.0`等）
        // かつ未ロックダウンの構成は起動そのものを拒否する -
        // `crate::commissioning::enforce_loopback_when_commissioning`の
        // doc comment参照。**`HubRuntime::start`はコンソール
        // （`bin/banto-hub.rs`）・Windows サービス（`win_service.rs`）・
        // デスクトップシェル（`apps/banto-hub/src-tauri`）の全ホストが
        // 唯一経由するこの1箇所**（このモジュール doc「起動シーケンス」・
        // `HubRuntime::start`自身の doc comment参照）なので、ここに置くだけで
        // 実装指示が名指しした`bin/banto-hub.rs`の起動経路を含む全ホストに
        // 確実に効く - ホスト側の env 読み取り（`build_hub_config_from_env`
        // 等）にこのチェックを置いてしまうと、ホストを追加・変更するたびに
        // 個別にガードし直す必要が生まれ、抜け道になり得る。
        crate::commissioning::enforce_loopback_when_commissioning(
            &bind,
            commissioning.is_locked_down(),
        )
        .map_err(HubStartError::UnsafeCommissioningBind)?;

        // 起動ログへの警告（実装指示5・設計 §5.6「未ロックダウンの間は…
        // 起動ログにも出す」）: 未ロックダウンで起動したときは、試運転
        // モードで動いていることを一目で分かるように警告を出す。
        if !commissioning.is_locked_down() {
            log_line(
                "banto-hub: [WARN] 試運転モード: この PC からはログインなしで（試運転の grant で）\
                 管理 UI / 管理 REST を操作できます。運用に入る前に管理 UI からロックダウンを\
                 実行してください\
                 （docs/tag-server-design.md §5.6「試運転モードとロックダウン」）。",
            );
        }

        let clock = Arc::new(SystemClock);
        // T2-2 (docs/tag-server-design.md §6-5): constructed here, OUTSIDE
        // `CollectorManager`, so a broker-managed session (SLMP, Modbus TCP)
        // survives every `CollectorManager::rebuild` - see `HubSessions`'s
        // doc comment.
        // Held as its own `Arc` (not only the clone `CollectorManager` gets)
        // so this binary can call `sessions.shutdown()` after
        // `manager.shutdown()` on the way out - see this module's doc
        // comment ("シャットダウン順序").
        //
        // R2b-3a-2 (docs/banto-hub-driver-io-design.md §4.2): the broker opens
        // a connection's secrets (only its driver's `secret_keys()`) just
        // before each dial, through this opener over the same database.
        let sessions = Arc::new(HubSessions::with_secret_opener(
            banto_broker::BackoffConfig::default(),
            drivers,
            Arc::new(crate::secrets::ConnectionSecretOpener::new(
                pool.clone(),
                crate::secrets::ConnectionSecrets::platform_default(),
            )),
        ));

        // T9-2 (docs/ux-plan.md §1): constructed here, OUTSIDE
        // `CollectorManager`, for the same reason `sessions` is - a
        // simulator started for a `simulation = true` broker-routed
        // connection must survive every `CollectorManager::rebuild` (see
        // `BrokerSimRegistry`'s doc comment). Held as its own `Arc` so this
        // binary can call `sim_registry.shutdown()` at the correct point on
        // the way out - see this module's doc comment ("シャットダウン順序")
        // for why that is AFTER `sessions.shutdown()`.
        let sim_registry = Arc::new(BrokerSimRegistry::new());

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

        // T14-3: startup commits the catalog/preflight only. The desktop host
        // remains stopped; the Windows service host explicitly starts
        // Configured mode through `RunningHub::controller()`.
        match RegistrySnapshot::load(&pool).await {
            Ok(snapshot) => {
                if let Err(err) = manager.commit_catalog(&snapshot).await {
                    log_err_line(&format!(
                        "banto-hub: 起動時 catalog の commit に失敗しました: {err}"
                    ));
                }
            }
            Err(err) => log_err_line(&format!(
                "banto-hub: 起動時レジストリ snapshot の取得に失敗しました: {err}"
            )),
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
        prune_once(&settings, &manager, clock.as_ref()).await;

        // Audit-log retention sweep (docs/banto-hub-remaining-plan.md
        // P3-a): startup-once, same as the tstore sweep above - see
        // [`audit_prune_once`]'s doc comment for why this rides along on
        // the tstore sweep's existing 24h loop below rather than staying
        // "startup + opportunistic-on-list only" like chronogazer/
        // relay-wright. Best-effort: a failure here must never stop the
        // server from starting.
        audit_prune_once(&settings, &audit).await;

        let prune_handle: JoinHandle<()> = {
            let prune_settings = settings.clone();
            let prune_manager = manager.clone();
            let prune_clock = clock.clone();
            let prune_audit = audit.clone();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(PRUNE_INTERVAL);
                interval.tick().await; // first tick fires immediately; startup sweeps above already ran
                loop {
                    interval.tick().await;
                    prune_once(&prune_settings, &prune_manager, prune_clock.as_ref()).await;
                    audit_prune_once(&prune_settings, &prune_audit).await;
                }
            })
        };

        // R2b-1d（設計 §4.2）: Hub の入口（REST・MCP・未適用キューの適用・Sink とブリッジの
        // 設定）の接続のサービスはレジストリつき。`manager.plc_connections()` と同じもの。
        let plc_connections = PlcConnectionService::with_registry(pool.clone(), registry.clone());
        // R2b-2b（アドレス設計 §3.1）: グループとタグのサービスもレジストリつき
        // （`manager.collection_groups()` / `manager.tags()` と同じもの）。
        let collection_groups =
            CollectionGroupService::with_registry(pool.clone(), registry.clone());
        let tags = TagService::with_registry(pool.clone(), registry.clone());
        let api_keys = ApiKeysService::new(pool.clone());

        // T2-4 (docs/tag-server-design.md §6-6)。2026-09-09 オーナー決定
        // (#340) で「起動時は必ず disabled」ルールを撤回し、永続値を復元する。
        // #433 (2026-09-24): 停止の状態は DB とデータディレクトリ内の状態
        // ファイルの 2 か所に記録し、起動時は「どちらか一方でも停止（または
        // 読めない）なら停止」で起動する（`crate::write_control` のモジュール
        // doc 参照）。食い違いはログに出し、管理画面にも同じ注意書きを出す
        // （`WriteControl::persistence_warning`）。
        let write_state_file = crate::write_control::state_file_path(&data_dir);
        let write_decision = load_startup_decision(&pool, &write_state_file).await;
        if let Some(warning) = &write_decision.warning {
            log_err_line(&format!("banto-hub: [WARN] 書き込み受付: {warning}"));
        }
        log_line(&format!(
            "banto-hub: 書き込み受付は{}で起動しました（状態ファイル: {}）",
            if write_decision.enabled {
                "有効"
            } else {
                "停止"
            },
            write_state_file.display()
        ));
        let write_control = Arc::new(match held_data {
            Some(dir) => WriteControl::restore_in(write_decision, dir),
            None => WriteControl::restore(write_decision, write_state_file),
        });
        let controller = Arc::new(CollectionController::new(manager.clone()));
        let write_audit = WriteAuditService::new(pool.clone());

        // T3 (docs/tag-server-design.md §5.3): construct stopped, then
        // apply the persisted settings - `enabled` comes straight from
        // settings (`SettingsService::mqtt_config`, applied a few lines
        // below), read fresh on every startup. MQTT publish has no
        // "restart always disables" safety rule like the write path does
        // (design has no such requirement for T3; publishing is read-only
        // against the tag space).
        let mqtt = Arc::new(MqttPublisher::new_with_controller(
            manager.clone(),
            controller.clone(),
        ));
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
        )
        .with_controller(controller.clone());
        let grpc_server = Arc::new(GrpcServer::new(grpc_service));
        let grpc_settings = settings.grpc_config().await.unwrap_or_else(|err| {
            log_err_line(&format!(
                "banto-hub: gRPC 設定の読み取りに失敗しました: {err}"
            ));
            crate::settings::GrpcSettings::default()
        });
        grpc_server.apply(&grpc_settings).await;
        let opcua = crate::opcua::OpcUaPublisher::new(manager.clone());
        let opcua_settings = settings.opcua_config().await.unwrap_or_else(|err| {
            log_err_line(&format!(
                "banto-hub: OPC UA 設定の読み取りに失敗しました: {err}"
            ));
            crate::settings::OpcUaSettings::default()
        });

        // セキュリティヘッダー（banto #500）は最後（最外）に掛け、静的 UI・
        // `/api/*`・SSE・WebSocket の upgrade 応答のすべてに付ける（ヘッダーを
        // 足すだけで、ルーティング・認証・本文には触らない）。loopback の
        // 接続元にだけ CSP の `connect-src` に Tauri IPC を足す
        // （[`hub_security_headers`]、#505）。
        let audit_for_spool = audit.clone();
        let opcua_admin = opcua_settings_router(
            manager.clone(),
            opcua.clone(),
            audit.clone(),
            auth.clone(),
            events.clone(),
        );
        let app = with_hub_security_headers(
            api_router_with_controller(
                users,
                audit,
                plc_connections,
                collection_groups,
                tags,
                api_keys,
                manager.clone(),
                controller.clone(),
                auth,
                commissioning,
                events,
                allow_setup,
                write_control,
                write_audit,
                mqtt.clone(),
                grpc_server.clone(),
                rate_limiter,
                profile_id,
            )
            .merge(opcua_admin)
            .merge(static_router::<FrontendAssets>()),
        );

        // #479: デスクトップシェルだけ、既定ポートが埋まっていたら OS 割り当てへ
        // 退避する（規則は [`port_fallback_applies`]）。退避したときは元のポートを
        // `port_fallback_from` に持ち、`RunningHub::port_fallback_from` で見せる。
        let fallback_allowed = port_fallback_applies(host_kind, port_override, port);
        let (server, port_fallback_from) =
            match start_with_port_fallback(&bind, port, app.clone(), fallback_allowed).await {
                Ok(started) => started,
                Err(err) => {
                    // ここまでに起こした背景タスクを残さない（`JoinHandle` を捨てても
                    // タスクは止まらない。#557 のレビュー指摘）。
                    eval_handle.abort();
                    prune_handle.abort();
                    mqtt.shutdown().await;
                    grpc_server.shutdown().await;
                    opcua.shutdown().await;
                    return Err(HubStartError::ServerStart(err));
                }
            };
        let tls_server = if let Some(config) = tls_config.as_ref() {
            match tls_listener::start(config, app).await {
                Ok(listener) => Some(listener),
                Err(err) => {
                    server.stop().await;
                    eval_handle.abort();
                    prune_handle.abort();
                    mqtt.shutdown().await;
                    grpc_server.shutdown().await;
                    opcua.shutdown().await;
                    return Err(HubStartError::Tls(err.to_string()));
                }
            }
        } else {
            None
        };
        // REST が先にポートを確保する。同じポートを OPC UA に指定しても
        // 失敗するのはオプションの OPC UA だけで、Hub 本体は継続する。
        opcua.initialize(&opcua_settings).await;

        // 背景タスクの順序の約束（#557 のレビュー指摘）: 起動手順のうち失敗しうる
        // ものがすべて済んでから起こす。先に起こしたものは、失敗する経路で
        // abort する（上のサーバー起動の失敗側）。
        let audit_spool_handle: Option<JoinHandle<()>> = audit_for_spool.spawn_spool_flusher();

        log_line(&format!("banto-hub: DB at {db_path}"));
        log_line(&format!("banto-hub: data dir at {}", data_dir.display()));
        if let Some(wanted) = port_fallback_from {
            log_err_line(&format!(
                "[WARN] banto-hub: port {wanted} is in use; started on {} instead (OS-assigned, desktop shell only). External clients that expect {wanted} (Sink, ChronoGazer, etc.) cannot reach this Hub - use the service Hub or set PORT / server.port to a fixed port",
                server.local_addr()
            ));
        }
        log_line("banto-hub: listening at:");
        for url in listening_urls(server.local_addr()) {
            log_line(&format!("  {url}"));
        }
        if let Some(listener) = &tls_server {
            log_line(&format!(
                "banto-hub: TLS listening at https://{}",
                listener.local_addr()
            ));
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
            opcua,
            controller,
            db_source: manager.db_source_engine(),
            manager,
            sessions,
            sim_registry,
            server,
            tls_server,
            port_fallback_from,
            eval_handle,
            prune_handle,
            audit_spool_handle,
            pool,
            _profile_lock: profile_lock,
            _profile_dirs: given_dirs,
        })
    }
}

/// #479（2026-10-10 オーナー決定「おすすめで」= 案 A'）: デスクトップシェル内の
/// Hub だけ、既定ポート（[`DEFAULT_PORT`] = 8722）が**使用中**のときに限り、同じ
/// bind アドレスの OS 割り当てポート（`port: 0`）で 1 回だけ起動し直す。
///
/// 退避してよいのは次の 3 つが**すべて**成り立つときだけ（外から設定された
/// 構成は固定のまま、という約束）:
/// - ホストがデスクトップシェル（[`HubHostKind::Shell`]）。サービス版・コンソール版
///   は外のクライアントが 8722 でつなぎに来るので、埋まっていたら今までどおり
///   起動失敗にする。
/// - `PORT` 環境変数による上書きが無い（`port_override` が `None`）。
/// - 設定 `server.port` が既定値のまま。**既定と違う値は利用者が明示的に選んだ
///   ものとして固定扱い**にする（8722 を明示的に保存していても既定と区別が
///   付かないので退避してよい側に倒れる。設定画面は既定値を保存するだけで
///   「選んだ」とは言えない）。
///
/// `BANTO_BIND` は退避の可否に影響しない（bind アドレスは変えない）。
pub fn port_fallback_applies(
    host_kind: HubHostKind,
    port_override: Option<u16>,
    configured_port: u16,
) -> bool {
    host_kind == HubHostKind::Shell && port_override.is_none() && configured_port == DEFAULT_PORT
}

/// #479: bind の失敗の種類が「そのポートは今使えない」にあたるか。
///
/// - [`std::io::ErrorKind::AddrInUse`]: 他のプロセスが待ち受け中（Windows の
///   `WSAEADDRINUSE` 10048、Linux/macOS の `EADDRINUSE`）。
/// - [`std::io::ErrorKind::PermissionDenied`]: Windows では `WSAEACCES` 10013 で、
///   (a) 他のプロセスが `SO_EXCLUSIVEADDRUSE` で握っている、(b) Hyper-V / WinNAT の
///   除外ポート範囲に入っている、のどちらも「そのポートは使えない」であり、
///   別ポートでなら起動できるので退避の対象にする。Linux/macOS の
///   `EACCES` は 1024 未満の特権ポートでしか起きず、退避対象は 8722 に限られる
///   ので実害がない（誤って他の失敗を握り潰さないよう、判定はこの 2 種類だけ）。
///
/// それ以外（アドレスが不正・ネットワークが無い等）は、別ポートでも直らないので
/// 退避せずそのままエラーにする。
pub fn bind_error_allows_port_fallback(kind: std::io::ErrorKind) -> bool {
    matches!(
        kind,
        std::io::ErrorKind::AddrInUse | std::io::ErrorKind::PermissionDenied
    )
}

/// `banto_server::start` は bind の失敗を文字列の [`BantoError::Other`] に潰して
/// `io::ErrorKind` を返さないので、失敗した後にもう一度 bind を試して種類を
/// 取り直す（#479）。**空きを事前に探す処理ではない**: 本番の bind が既に失敗した
/// 後の分類だけに使い、成功した場合はすぐ閉じて `None`（= 退避しない。すでに
/// 空いたなら元のエラーをそのまま報告して、起動し直しは呼び出し側に任せる）。
///
/// banto 側が `io::ErrorKind` を返す API、または bind 済みの listener を受け取る API を
/// 持てば、この再 bind は不要になる（上流への課題）。
async fn classify_bind_failure(bind: &str, port: u16) -> Option<std::io::ErrorKind> {
    match tokio::net::TcpListener::bind(format!("{bind}:{port}")).await {
        Ok(_listener) => None,
        Err(err) => Some(err.kind()),
    }
}

/// axum サーバーを `bind:port` で起動する。`fallback_allowed`（[`port_fallback_applies`]）
/// で、その bind が [`bind_error_allows_port_fallback`] の失敗だったときだけ、同じ
/// `bind` の `port: 0` で 1 回起動し直す。戻り値の 2 つ目は退避したときの元のポート。
///
/// 退避は **サーバーの bind だけのやり直し**で、`HubRuntime::start` 全体は
/// やり直さない（profile 排他・DB・収集・MQTT・gRPC は 1 回しか作らない）。
/// 失敗時の後始末（背景タスクの abort など）は呼び出し側の既存の経路のまま。
async fn start_with_port_fallback(
    bind: &str,
    port: u16,
    app: axum::Router,
    fallback_allowed: bool,
) -> Result<(RunningServer, Option<u16>), BantoError> {
    let first = start(
        ServerConfig {
            bind: bind.to_string(),
            port,
        },
        app.clone(),
    )
    .await;
    match first {
        Ok(server) => Ok((server, None)),
        Err(err) => {
            if !fallback_allowed {
                return Err(err);
            }
            let allows = classify_bind_failure(bind, port)
                .await
                .is_some_and(bind_error_allows_port_fallback);
            if !allows {
                return Err(err);
            }
            let server = start(
                ServerConfig {
                    bind: bind.to_string(),
                    port: 0,
                },
                app,
            )
            .await?;
            Ok((server, Some(port)))
        }
    }
}

/// [`HubRuntime::start`]が返す、稼働中の banto-hub の1インスタンス。
/// teardown に必要なものを保持する - [`RunningHub::shutdown`]がそれらを
/// 使ってこのモジュール doc の「シャットダウン順序」節どおりに畳む。
/// T14-2 の収集状態機械を保持し、[`Self::controller`] から外部ホストへ
/// [`CollectionController`] を提供する。
pub struct RunningHub {
    mqtt: Arc<MqttPublisher>,
    grpc_server: Arc<GrpcServer>,
    opcua: Arc<crate::opcua::OpcUaPublisher>,
    controller: Arc<CollectionController>,
    manager: Arc<CollectorManager>,
    sessions: Arc<HubSessions>,
    sim_registry: Arc<BrokerSimRegistry>,
    server: RunningServer,
    tls_server: Option<RunningTlsServer>,
    /// #479: デスクトップシェルが既定ポートの競合で OS 割り当てへ退避したとき、
    /// 本来 bind したかったポート。退避しなかったら `None`。
    port_fallback_from: Option<u16>,
    /// computed 250ms 評価ループの `JoinHandle`（T14-1 で捕捉。このモジュール
    /// doc の「T14-1 での唯一の挙動変化」節参照）。
    eval_handle: JoinHandle<()>,
    /// tstore 剪定24hループの `JoinHandle`（同上）。
    prune_handle: JoinHandle<()>,
    /// #437: 監査の保留の定期の流し込み（`AuditLogService::spawn_spool_flusher`、
    /// `crate::audit_spool`）の `JoinHandle`。保留を開けなかったときは `None`。
    /// タスクは監査サービス（延いては DB の pool）の複製を持つので、
    /// [`RunningHub::shutdown`] で abort する。
    audit_spool_handle: Option<JoinHandle<()>>,
    /// 外部 DB 連携 S2: DB Source のポーリングエンジン
    /// （`crate::hub::CollectorManager` が所有する `Arc` の複製 -
    /// [`RunningHub::shutdown`] が接続タスクを止めるために持つ）。
    db_source: Arc<crate::db_source::DbSourceEngine>,
    /// T17-1（docs/banto-hub-t17-design.md §3「T17-1」・P2）: 保持するだけの
    /// フィールド - `RunningHub`（延いては[`RunningHub::shutdown`]消費後の
    /// `self`）が drop される時点で`ProfileLockGuard`自身の`Drop`が
    /// Windows mutex handle の`CloseHandle`／非 Windows lock file の fd
    /// close（`flock`自動解放）を行う。`skip_profile_lock: true`で構築した
    /// 場合は`None`。
    _profile_lock: Option<ProfileLockGuard>,
    /// 設定 DB の pool（#661 661-1）。[`RunningHub::shutdown`] の最後に閉じる
    /// （[`RunningHub::shutdown_and_report`]）。
    pool: SqlitePool,
    /// [`HubRuntime::start_with_profile_dirs`] に渡された握ったフォルダの組（#661 661-1。
    /// 排他を取ったときは `_profile_lock` も同じハンドルを持つ）。`start` では `None`。
    _profile_dirs: Option<ProfileDirs>,
}

/// [`RunningHub::shutdown_and_report`] の結果（#661 661-1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HubShutdownReport {
    /// 設定 DB の pool を [`DB_CLOSE_TIMEOUT`] のうちに閉じ終えたか。`false` なら、
    /// どこかが接続を返さずに上限を過ぎた（661-2 のサービスは ACL を緩める前に見る）。
    pub db_closed: bool,
}

/// 停止の最後に設定 DB の pool を閉じるのを待つ上限（#661 661-1）。`close` は貸し出し中の
/// 接続が返るのを待つので、返らない接続があっても停止を止めない。デスクトップの終了の
/// 予算（15 秒）の中に収まる長さ。
pub const DB_CLOSE_TIMEOUT: Duration = Duration::from_secs(3);

impl RunningHub {
    /// axum サーバーの実バインドアドレス（`port: 0`で OS にポートを
    /// 選ばせた場合も実際の値が返る）。旧 `hub_run.rs`が `server.local_addr()`
    /// を直接使っていたのと同じ（設計 §3「D1」の `local_addr()`）。
    pub fn local_addr(&self) -> std::net::SocketAddr {
        self.server.local_addr()
    }

    /// The dedicated loopback TLS address, when configured.
    pub fn tls_local_addr(&self) -> Option<std::net::SocketAddr> {
        self.tls_server.as_ref().map(RunningTlsServer::local_addr)
    }

    /// #479: 既定ポートが埋まっていて OS 割り当てのポートへ退避して起動したとき、
    /// 本来 bind したかったポート（[`port_fallback_applies`]）。退避していなければ
    /// `None`。外のクライアント（Sink など）はこの Hub に届かないので、ホストが
    /// 状態として見せたいときに使う。
    pub fn port_fallback_from(&self) -> Option<u16> {
        self.port_fallback_from
    }

    /// Obtain the process-wide serialized collection controller.
    pub fn controller(&self) -> Arc<CollectionController> {
        self.controller.clone()
    }

    /// このモジュール doc の「シャットダウン順序」節どおりに teardown する。
    /// 呼び出し側（ホスト）は自分の停止トリガー（Ctrl-C / SCM Stop /
    /// トレイ）を待ってからこれを呼ぶこと。
    ///
    /// 最後に設定 DB の pool を閉じる（#661 661-1。結果が要るときは
    /// [`Self::shutdown_and_report`]）。
    pub async fn shutdown(self) {
        let _ = self.shutdown_and_report().await;
    }

    /// [`Self::shutdown`] と同じ順で畳み、最後に設定 DB の pool を閉じて（上限
    /// [`DB_CLOSE_TIMEOUT`]）、閉じ終えたかを返す（#661 661-1。661-2 のサービスは、
    /// 閉じ終えてから ACL を緩める）。
    pub async fn shutdown_and_report(self) -> HubShutdownReport {
        log_line("banto-hub: shutting down");
        // #392 A2 レビュー（2026-10-12）: 終了の受付の**取り消せない**印を立て、以降の収集の
        // 開始（REST/MCP はこの関数の最後の `server.stop()` まで受け付けて
        // いる）を 409 で断る。この印は誰も下ろさない（トレイの `AskAgain` が
        // 下ろすのは確認のための一時的な印だけ。`crate::controller` の doc
        // 「終了の受付」）。トレイの「アプリを終了」は確認の段階で一時的な印を
        // 立てており、ここで取り消せない方に上げる。Windows サービスの停止は
        // ここが最初。既に受け付けられて走っている開始（印を立てる前の
        // もの）は、ここでは待たない - 従来どおり下の `manager.shutdown()` が
        // 止める。
        let _ = self.controller.begin_shutdown();
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
        // #437: 監査の保留の定期の流し込みも、上の 2 本と同じく止める。流し込みの
        // 途中で止めても、ファイルは行が入ってから消すので失われない（次の起動で
        // 同じ `pending_id` で流し直し、`ON CONFLICT` で 1 行になる）。
        if let Some(handle) = &self.audit_spool_handle {
            handle.abort();
        }
        // 外部 DB 連携 S2: DB Source の接続タスクは `ServerTagStore` への
        // 書き手なので、`manager.shutdown()` より前で止める（このモジュール
        // doc の「シャットダウン順序」節「DB Source の位置」参照）。
        self.db_source.shutdown().await;

        // T3: stop the MQTT publisher (a consumer of `manager`) before
        // `manager.shutdown()` - same dependency-order reasoning as
        // `manager.shutdown()` before `sessions.shutdown()` below (stop the
        // dependent first, then the thing it depends on).
        self.mqtt.shutdown().await;
        // T4: gRPC サーバーも `manager` の消費者(read-only)なので、mqtt と
        // 同じ理由で `manager.shutdown()` より先に止める。
        self.grpc_server.shutdown().await;
        self.opcua.shutdown().await;
        self.manager.shutdown().await;
        self.sessions.shutdown().await;
        // T9-2: simulators must outlive both the collector's tasks
        // (`manager`) and the broker sessions that may be dialing them
        // (`sessions`), so they are stopped last - see this module's doc
        // comment ("シャットダウン順序").
        self.sim_registry.shutdown().await;
        if let Some(tls_server) = self.tls_server {
            tls_server.stop().await;
        }
        self.server.stop().await;
        // #661 661-1: 書き手（サーバー・収集・監査の流し込み）を止めた後に、設定 DB を
        // 閉じる（WAL を畳んでファイルを離す）。返らない接続があっても上限で切り上げる。
        let db_closed = tokio::time::timeout(DB_CLOSE_TIMEOUT, self.pool.close())
            .await
            .is_ok();
        if !db_closed {
            log_err_line(&format!(
                "banto-hub: [WARN] 設定 DB を {} 秒のうちに閉じ終えられませんでした（返らない接続があります）",
                DB_CLOSE_TIMEOUT.as_secs()
            ));
        }
        HubShutdownReport { db_closed }
    }
}

/// 設定 DB のパス（#661 661-1、[`HubRuntime::start_with_profile_dirs`]）。profile の
/// `config\banto-hub.sqlite3`（渡された綴りか本当のパス）なら、`dirs.config` で確かめた
/// パスに直す（付随の `-wal`・`-shm`・`-journal` も確かめる）。それ以外（`BANTO_DB` が
/// profile の外など）はそのまま返す。
fn profile_db_path(dirs: &ProfileDirs, db_path: String) -> Result<String, ProfileDirsError> {
    let path = Path::new(&db_path);
    let in_config = path == dirs.config.path().join(DB_FILE_NAME)
        || path == dirs.config.final_path().join(DB_FILE_NAME);
    if !in_config {
        return Ok(db_path);
    }
    const COMPANIONS: [&str; 3] = ["-wal", "-shm", "-journal"];
    // #661 661-2: 運用モードの形に締めた組（サービス）なら、渡す直前に `config` の記述子が
    // その形かを確かめる（締めた後に外から変えられていたら渡さない）。
    let checked = match &dirs.checked_access {
        Some(expected) => dirs
            .config
            .reopen_path_checked(DB_FILE_NAME, &COMPANIONS, expected),
        None => dirs.config.reopen_path(DB_FILE_NAME, &COMPANIONS),
    }
    .map_err(|error| ProfileDirsError::Reopen {
        name: DB_FILE_NAME.to_string(),
        error,
    })?;
    Ok(checked.into_path().to_string_lossy().into_owned())
}

/// `path` が握ったフォルダ `dir` そのもの（渡された綴りか本当のパス）か（#661 661-1）。
fn is_profile_dir(path: &Path, dir: &banto_safe_root::ManagedDir) -> bool {
    path == dir.path() || path == dir.final_path()
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
async fn ensure_virtual_connection(
    pool: &SqlitePool,
    registry: &Arc<banto_driver_api::DriverRegistry>,
    name: &str,
) {
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

    // R2b-1d（設計 §4.2）: 予約接続もレジストリつきの入口で作る（`virtual` は
    // `crate::computed::driver::VirtualDriver`、host が登録する）。
    let svc = PlcConnectionService::with_registry(pool.clone(), registry.clone());
    if let Err(err) = svc
        .create_settings(ConnectionSettingsInput {
            name: name.to_string(),
            protocol: VIRTUAL_PROTOCOL.to_string(),
            enabled: true,
            simulation: false,
            secrets: Default::default(),
            settings: Default::default(),
        })
        .await
    {
        log_err_line(&format!(
            "banto-hub: 予約接続 {name} の自動プロビジョニングに失敗しました: {err}"
        ));
    }
}

/// One retention sweep (design §3.3): read the configured retention days
/// and delete tstore files older than that, today computed from `clock`.
/// Errors are logged, never fatal. If the settings read itself fails the
/// sweep is skipped (not defaulted - a default could delete history the
/// operator configured to keep) and retried next cycle; an *unset* value is
/// a different case, defaulted inside `store_config`.
///
/// T19 S2-d（UX-39）: `retention_days` is now `Option<i64>` -
/// `None`（無制限）means the operator explicitly chose "don't prune", so
/// this sweep is skipped entirely rather than calling
/// [`banto_tstore::prune_files`] with some sentinel day count. Likewise, if
/// the stored value doesn't fit in the `u32` [`banto_tstore::prune_files`]
/// expects, the sweep is skipped (not defaulted to some day count) - see the
/// comment at the `u32::try_from` call below.
async fn prune_once(
    settings: &SettingsService,
    manager: &CollectorManager,
    clock: &dyn banto_tstore::Clock,
) {
    let retention_days = match settings.store_config().await {
        Ok(config) => config.retention_days,
        // 読めなかったときは既定日数にせず、この周期は何も消さない（次の周期で
        // 再試行する）。無制限や長い保持日数の設定なのに既定の日数で消すと、
        // 履歴を失う（ChronoGazer の #540 `SweepOutcome::SettingsUnreadable` と
        // 同じ）。「未設定なら既定」は `store_config` 自身がやる別の話。
        Err(err) => {
            log_err_line(&format!(
                "banto-hub: 保持設定の読み取りに失敗したため、今回の剪定をスキップしました（次の周期で再試行）: {err}"
            ));
            return;
        }
    };
    let Some(retention_days) = retention_days else {
        log_line("banto-hub: tstore 保持期間は無制限のため剪定をスキップしました");
        return;
    };
    // 剪定は不可逆な削除なので、保持日数が`u32`に収まらない想定外の値
    // （`parse_retention`は上限クランプをしないため、DBを直接編集された
    // 等で`u32::MAX`超の値が入り得る）のときは、削除する側ではなく
    // 保持する側（上の無制限スキップと同じくスキップ）に倒す。
    let Ok(retention_days) = u32::try_from(retention_days) else {
        log_err_line(&format!(
            "banto-hub: tstore 保持日数の値が想定外のため剪定をスキップしました: {retention_days}"
        ));
        return;
    };
    let today = LocalDate::from_epoch_ms(clock.now_ms(), clock.utc_offset_ms());
    // 収集中は書き込み側を通して消す（開いている日付のファイルを消さない、#541）。
    match manager.prune_data_files(retention_days, today).await {
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

/// One audit-log retention sweep (docs/banto-hub-remaining-plan.md P3-a
/// 追補、2026-08-12): read the configured retention policy (days/rows) and
/// call [`AuditLogService::prune`]. Mirrors [`prune_once`]'s shape
/// (best-effort - a settings-read failure or a prune failure is only
/// logged, never fatal / never stops the caller's loop).
///
/// Called from three places, matching chronogazer/relay-wright's "no
/// separate background task" reasoning MINUS the "no periodic task" part -
/// banto-hub is a 24/7 headless collector (unlike the Tauri-shell reference
/// apps, which are desktop apps a human restarts often), so a deployment
/// that never opens the audit-log viewer and never restarts would otherwise
/// never prune at all. The 24h periodic tstore sweep below already exists
/// for the same class of problem (design §3.3), so audit-log retention now
/// rides along on it instead of inventing a second timer:
/// - once at startup ([`HubRuntime::start`]),
/// - every tick of the existing 24h tstore retention loop
///   ([`HubRuntime::start`]'s `prune_handle`), and
/// - opportunistically before every `POST /api/audit-log/list` without
///   `asOfId` (the list handler of banto's
///   `banto_server::routes::audit_log_router`, I3') - kept for low-latency
///   effect on an admin who just changed the retention policy and
///   immediately reopens the viewer, even though the 24h loop now makes it
///   non-load-bearing.
async fn audit_prune_once(settings: &SettingsService, audit: &AuditLogService) {
    let config = match settings.audit_config().await {
        Ok(config) => config,
        Err(err) => {
            log_err_line(&format!(
                "banto-hub: 監査ログの保持設定の読み取りに失敗しました: {err}"
            ));
            return;
        }
    };
    if let Err(err) = audit
        .prune(config.retention_days, config.retention_rows)
        .await
    {
        log_err_line(&format!("banto-hub: 監査ログの剪定に失敗しました: {err}"));
    }
}

/// The URLs a client can use to reach a server that is ACTUALLY listening on
/// `addr` (its `RunningServer::local_addr()`) - for the startup log's
/// "listening at" list. banto v2.0.0 移行（PR1a オーナーレビュー P3）: the
/// bind setting is a free string handed to `TcpListener::bind`, so a host
/// name such as `localhost` binds fine but `lan_urls_for_bind` cannot parse
/// it as an IP and would list nothing. The bound address is always an IP;
/// an unspecified `0.0.0.0` bind stays `0.0.0.0` here, so every interface is
/// still listed. (An IPv6 bound address lists nothing, by banto's
/// `lan_urls_for_bind` policy - IPv6 is out of scope for now.)
fn listening_urls(addr: std::net::SocketAddr) -> Vec<String> {
    lan_urls_for_bind(&addr.ip().to_string(), addr.port())
}

/// Hub の HTTP 応答に付けるセキュリティヘッダー（banto の `SecurityHeaders`、
/// #500・#505）。既定は banto の厳格な CSP（`connect-src 'self'`）で、
/// **TCP の接続元と要求の宛先（`Host`／`:authority`）の両方が loopback の
/// 要求にだけ**（`request_is_loopback_local`、banto v5.1.0）`connect-src` に
/// Tauri IPC（`ipc:`・`http://ipc.localhost`、`TAURI_IPC_CONNECT_SRC`）を足す。
///
/// なぜ広げるか: デスクトップ版（`banto-hub-shell`）は起動後に窓を
/// `http://127.0.0.1:<port>/` へ navigate するので、管理画面には窓の CSP では
/// なくこの応答の CSP が掛かる。`/status` の「Windows サービス」カードが使う
/// Tauri IPC の fetch が `connect-src 'self'` に止められ、読み込むたびに CSP
/// 違反が出ていた（Tauri が postMessage に切り替えるので動作はしていた）。
///
/// なぜ接続元と宛先の AND か: 接続元は TCP の接続から決まり、リモートの
/// ページやリンクからは選べない。ただし同一ホストのリバースプロキシ
/// （`reverse_proxy 127.0.0.1:8722` の Caddy など）の後ろでは全要求の接続元が
/// loopback になるので、接続元だけでは LAN の閲覧者にも広がってしまう。
/// ブラウザは移った URL から `Host` を付けるので、`Host` を保つプロキシ経由の
/// LAN の閲覧者は公開名や LAN のアドレスを出して厳格なまま、シェルは
/// `127.0.0.1` へ移るので広がる。`Host` は相手が決められるが、AND なので
/// 狭める方向にしか働かない。`X-Forwarded-Host`・`Forwarded` は見ない。
/// 広げた CSP を普通のブラウザが受け取っても、`ipc:` は扱えない scheme で、
/// `http://ipc.localhost` は**閲覧者自身の** loopback の 80/443 番を指すだけ
/// （外への持ち出し先にはならない）。運用条件（プロキシは `Host` を保つ、
/// 共有キャッシュは `Host` ごとに分ける）は docs/banto-hub-operations.md §6・
/// tag-server-design.md §5.6、banto の `security_headers.rs` 冒頭の doc を参照。
pub(crate) fn hub_security_headers() -> SecurityHeaders {
    SecurityHeaders::new()
        .extra_connect_src(TAURI_IPC_CONNECT_SRC)
        .expect("TAURI_IPC_CONNECT_SRC is a fixed, valid set of CSP sources")
        .extra_connect_src_when(request_is_loopback_local)
}

/// `router` に [`hub_security_headers`] を最外で掛ける。
fn with_hub_security_headers(router: axum::Router) -> axum::Router {
    with_security_headers_using(router, hub_security_headers())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::init_db;

    /// PR1a オーナーレビュー P3: the startup log's URLs come from the bound
    /// address - a loopback bind lists exactly that URL, and an unspecified
    /// bind still lists the loopback one (plus one per LAN interface).
    #[test]
    fn listening_urls_come_from_the_bound_address() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        assert_eq!(
            listening_urls(addr),
            vec![format!("http://127.0.0.1:{}", addr.port())]
        );

        let listener = std::net::TcpListener::bind("0.0.0.0:0").expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        assert_eq!(addr.ip().to_string(), "0.0.0.0");
        assert!(
            listening_urls(addr).contains(&format!("http://127.0.0.1:{}", addr.port())),
            "{:?}",
            listening_urls(addr)
        );
    }

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
            drivers: crate::test_support::test_drivers(),
            db_path,
            allow_setup: false,
            port_override: Some(0),
            bind_override: Some("127.0.0.1".to_string()),
            data_dir_override: Some(data_dir),
            profile_id: "test".to_string(),
            host_kind: HubHostKind::Console,
            // T17-1: このテストは既存の start->shutdown ラウンドトリップ
            // だけを検証する - profile 排他自体は下の
            // `start_fails_with_profile_lock_when_another_guard_already_holds_it`
            // が別途検証するので、ここでは無関係な profile ロック競合を
            // 避けるため skip する（このモジュール doc の`skip_profile_lock`
            // フィールド doc comment参照）。
            skip_profile_lock: true,
        };

        let hub = HubRuntime::start(config)
            .await
            .expect("HubRuntime::start should succeed against a fresh temp DB");

        let addr = hub.local_addr();
        assert_eq!(addr.ip().to_string(), "127.0.0.1");
        assert_ne!(addr.port(), 0, "the OS should have assigned a real port");

        hub.shutdown().await;
    }

    fn plain_profile_dirs(dir: &crate::test_support::TempDir) -> ProfileDirs {
        let paths = resolve_profile_paths(dir.path(), "dirs").expect("valid profile id");
        crate::profile_lock::open_profile_dirs(
            &crate::profile_lock::plain_profile_root_spec(&paths),
            None,
        )
        .expect("open dirs")
    }

    /// #661 661-1: 握った組を渡して起動し、止めると設定 DB を閉じ終える（閉じた後は
    /// DB のファイルを消せる - Windows では開いたままだと消せない）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn start_with_profile_dirs_then_shutdown_closes_the_db() {
        let dir = crate::test_support::TempDir::new("hub-runtime-profile-dirs");
        let dirs = plain_profile_dirs(&dir);
        let db = dirs.config.path().join(DB_FILE_NAME);
        let config = HubConfig {
            drivers: crate::test_support::test_drivers(),
            db_path: db.to_string_lossy().into_owned(),
            allow_setup: false,
            port_override: Some(0),
            bind_override: Some("127.0.0.1".to_string()),
            data_dir_override: Some(dirs.data.path().to_path_buf()),
            profile_id: "dirs".to_string(),
            host_kind: HubHostKind::Console,
            skip_profile_lock: true,
        };

        let hub = HubRuntime::start_with_profile_dirs(config, dirs.clone())
            .await
            .expect("start with profile dirs");
        let report = hub.shutdown_and_report().await;
        assert!(report.db_closed, "{report:?}");
        assert!(db.is_file());
        std::fs::remove_file(&db).expect("閉じた DB は消せる");
    }

    /// #693 レビュー: 監査の保留の場所が普通のファイルでも、握った組を渡した起動は
    /// 保留なしで続く（`audit_spool` の方針。DB の初期化より前に止まらない）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn start_with_unusable_audit_spool_runs_without_spool() {
        let dir = crate::test_support::TempDir::new("hub-runtime-bad-spool");
        let paths = resolve_profile_paths(dir.path(), "dirs").expect("valid profile id");
        std::fs::create_dir_all(&paths.data_dir).unwrap();
        std::fs::write(
            paths.data_dir.join(crate::audit_spool::SPOOL_DIR_NAME),
            b"not a directory",
        )
        .unwrap();
        let dirs = plain_profile_dirs(&dir);
        assert!(dirs.audit_spool.is_none());
        let config = HubConfig {
            drivers: crate::test_support::test_drivers(),
            db_path: dirs
                .config
                .path()
                .join(DB_FILE_NAME)
                .to_string_lossy()
                .into_owned(),
            allow_setup: false,
            port_override: Some(0),
            bind_override: Some("127.0.0.1".to_string()),
            data_dir_override: Some(dirs.data.path().to_path_buf()),
            profile_id: "dirs".to_string(),
            host_kind: HubHostKind::Console,
            skip_profile_lock: true,
        };
        let hub = HubRuntime::start_with_profile_dirs(config, dirs)
            .await
            .expect("a bad audit-spool must not stop the start");
        hub.shutdown().await;
    }

    /// #661 661-1: profile の `config\banto-hub.sqlite3` だけを確かめたパスに直し、外の
    /// パス（`BANTO_DB`）はそのまま。`data\` も綴り・本当のパスのどちらでも中とみなす。
    #[test]
    fn profile_paths_are_rewritten_only_inside_the_profile() {
        let dir = crate::test_support::TempDir::new("hub-runtime-profile-db-path");
        let dirs = plain_profile_dirs(&dir);

        let inside = dirs.config.path().join(DB_FILE_NAME);
        let rewritten =
            profile_db_path(&dirs, inside.to_string_lossy().into_owned()).expect("inside");
        assert_eq!(
            PathBuf::from(&rewritten),
            dirs.config.final_path().join(DB_FILE_NAME)
        );

        let outside = dir.path().join("elsewhere.sqlite3");
        let kept = outside.to_string_lossy().into_owned();
        assert_eq!(profile_db_path(&dirs, kept.clone()).unwrap(), kept);
        assert_eq!(
            profile_db_path(&dirs, "./banto-hub.sqlite3".to_string()).unwrap(),
            "./banto-hub.sqlite3"
        );

        assert!(is_profile_dir(dirs.data.path(), &dirs.data));
        assert!(is_profile_dir(dirs.data.final_path(), &dirs.data));
        assert!(!is_profile_dir(&dir.path().join("data"), &dirs.data));
    }

    /// オプションの OPC UA が bind に失敗しても REST/Hub は起動する。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn opcua_busy_port_does_not_stop_hub() {
        let dir = crate::test_support::TempDir::new("hub-runtime-opcua-busy");
        let db_path = dir
            .path()
            .join("registry.sqlite3")
            .to_string_lossy()
            .into_owned();
        let pool = init_db(&db_path).await.expect("init DB");
        let holder = std::net::TcpListener::bind("127.0.0.1:0").expect("hold port");
        let busy = holder.local_addr().expect("port").port();
        let settings = SettingsService::new(Db::Sqlite(pool.clone()));
        settings
            .set_opcua_config(&crate::settings::OpcUaSettings {
                enabled: true,
                port: busy,
                ..Default::default()
            })
            .await
            .expect("save OPC UA settings");
        pool.close().await;

        let hub = HubRuntime::start(HubConfig {
            drivers: crate::test_support::test_drivers(),
            db_path,
            allow_setup: false,
            port_override: Some(0),
            bind_override: Some("127.0.0.1".to_owned()),
            data_dir_override: Some(dir.path().join("data")),
            profile_id: "test-opcua-busy".to_owned(),
            host_kind: HubHostKind::Console,
            skip_profile_lock: true,
        })
        .await
        .expect("Hub REST must start despite OPC UA bind failure");
        assert_ne!(hub.local_addr().port(), busy);
        // #634: bind の失敗は状態の API（`GET /api/opcua-status`）に理由つきで出る。
        let status = hub.opcua.status();
        assert_eq!(status.state, crate::opcua::OpcUaState::Failed);
        assert!(status.error.is_some_and(|error| !error.is_empty()));
        tokio::time::timeout(Duration::from_secs(10), hub.shutdown())
            .await
            .expect("shutdown");
    }

    /// OPC UA の管理 API の試験用: 生の HTTP/1.0 で 1 回呼び、状態と JSON を返す。
    async fn call_path(
        addr: std::net::SocketAddr,
        path: &str,
        operation: &str,
        method: &str,
        token: Option<&str>,
        body: Option<&serde_json::Value>,
        response_timeout: Duration,
    ) -> (u16, serde_json::Value) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let payload = body.map(serde_json::Value::to_string).unwrap_or_default();
        let authorization = token
            .map(|token| format!("Authorization: Bearer {token}\r\n"))
            .unwrap_or_default();
        let request = format!(
            "{method} {path} HTTP/1.0\r\nHost: {addr}\r\nX-Banto-Client: banto\r\n{authorization}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
            payload.len()
        );
        let mut stream = tokio::net::TcpStream::connect(addr)
            .await
            .unwrap_or_else(|error| panic!("{operation}: connect REST: {error}"));
        stream
            .write_all(request.as_bytes())
            .await
            .unwrap_or_else(|error| panic!("{operation}: write REST: {error}"));
        let mut response = Vec::new();
        tokio::time::timeout(response_timeout, stream.read_to_end(&mut response))
            .await
            .unwrap_or_else(|_| {
                panic!("{operation}: REST response timed out after {response_timeout:?}")
            })
            .unwrap_or_else(|error| panic!("{operation}: read REST response: {error}"));
        let raw = std::str::from_utf8(&response)
            .unwrap_or_else(|error| panic!("{operation}: UTF-8 REST response: {error}"));
        let (head, body) = raw
            .split_once("\r\n\r\n")
            .unwrap_or_else(|| panic!("{operation}: HTTP response has no header/body separator"));
        let status = head
            .split_whitespace()
            .nth(1)
            .unwrap_or_else(|| panic!("{operation}: HTTP response has no status"))
            .parse()
            .unwrap_or_else(|error| panic!("{operation}: invalid HTTP status: {error}"));
        let json = serde_json::from_str(body).unwrap_or(serde_json::Value::Null);
        (status, json)
    }

    /// PR #650 のオーナーレビュー P2: async の worker が 1 本でも、本物の starter が
    /// listener を起動している（同期の鍵の生成をしている）間に、有効化の PUT と
    /// 状態の GET が応答し、`starting` が見える。2 本の worker と非同期の待ちでは
    /// この停止は見えないので、worker 1 本で書く。
    ///
    /// 反証: `real_starter` の `spawn_blocking` をやめて `OpcUaServer::start` を
    /// そのまま await すると、鍵の生成が唯一の worker を塞ぎ、PUT が上限内に
    /// 返らないか、最初の状態の GET が起動の終わった後の `running` になって落ちる。
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn opcua_put_and_status_respond_during_a_real_start_on_one_worker() {
        let dir = crate::test_support::TempDir::new("hub-runtime-opcua-one-worker");
        let db_path = dir
            .path()
            .join("registry.sqlite3")
            .to_string_lossy()
            .into_owned();
        let pool = init_db(&db_path).await.expect("init DB");
        UsersService::new(Db::Sqlite(pool.clone()))
            .setup_first_user("admin", "password123", "管理者")
            .await
            .expect("setup admin");
        pool.close().await;
        let hub = HubRuntime::start(HubConfig {
            drivers: crate::test_support::test_drivers(),
            db_path,
            allow_setup: false,
            port_override: Some(0),
            bind_override: Some("127.0.0.1".to_owned()),
            data_dir_override: Some(dir.path().join("data")),
            profile_id: "test-opcua-one-worker".to_owned(),
            host_kind: HubHostKind::Console,
            skip_profile_lock: true,
        })
        .await
        .expect("start Hub");
        let addr = hub.local_addr();
        let (status, login) = call_path(
            addr,
            "/api/auth/login",
            "login",
            "POST",
            None,
            Some(&serde_json::json!({"username": "admin", "password": "password123"})),
            Duration::from_secs(10),
        )
        .await;
        assert_eq!(status, 200, "{login}");
        let token = login["token"].as_str().expect("admin token").to_owned();
        let (status, mut settings) = call_path(
            addr,
            "/api/opcua-settings",
            "read settings",
            "GET",
            Some(&token),
            None,
            Duration::from_secs(10),
        )
        .await;
        assert_eq!(status, 200);
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .expect("reserve OPC port")
            .local_addr()
            .expect("OPC port")
            .port();
        settings["enabled"] = serde_json::json!(true);
        settings["port"] = serde_json::json!(port);

        let (status, _) = call_path(
            addr,
            "/api/opcua-settings",
            "enable OPC UA (must not wait for the key generation)",
            "PUT",
            Some(&token),
            Some(&settings),
            Duration::from_secs(5),
        )
        .await;
        assert_eq!(status, 200);
        let (status, first) = call_path(
            addr,
            "/api/opcua-status",
            "status during the start",
            "GET",
            Some(&token),
            None,
            Duration::from_secs(2),
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(first["state"], "starting", "{first}");

        let started = tokio::time::Instant::now();
        loop {
            let (_, body) = call_path(
                addr,
                "/api/opcua-status",
                "status until running",
                "GET",
                Some(&token),
                None,
                Duration::from_secs(10),
            )
            .await;
            match body["state"].as_str() {
                Some("running") => break,
                Some("starting") => {}
                other => panic!("unexpected OPC UA state {other:?}: {body}"),
            }
            assert!(
                started.elapsed() < Duration::from_secs(60),
                "OPC UA listener did not start within 60s: {body}"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .expect("OPC UA listener started");
        hub.shutdown().await;
    }

    /// 実 Hub の管理 API が保存・即時反映し、不正値を永続化しない。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn opcua_admin_settings_round_trip_and_reconfigure_listener() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        async fn call(
            addr: std::net::SocketAddr,
            operation: &str,
            method: &str,
            token: Option<&str>,
            body: Option<&serde_json::Value>,
            response_timeout: Duration,
        ) -> (u16, serde_json::Value) {
            call_path(
                addr,
                "/api/opcua-settings",
                operation,
                method,
                token,
                body,
                response_timeout,
            )
            .await
        }

        let dir = crate::test_support::TempDir::new("hub-runtime-opcua-admin");
        let db_path = dir
            .path()
            .join("registry.sqlite3")
            .to_string_lossy()
            .into_owned();
        let pool = init_db(&db_path).await.expect("init DB");
        UsersService::new(Db::Sqlite(pool.clone()))
            .setup_first_user("admin", "password123", "管理者")
            .await
            .expect("setup admin");
        pool.close().await;

        let hub = HubRuntime::start(HubConfig {
            drivers: crate::test_support::test_drivers(),
            db_path,
            allow_setup: false,
            port_override: Some(0),
            bind_override: Some("127.0.0.1".to_owned()),
            data_dir_override: Some(dir.path().join("data")),
            profile_id: "test-opcua-admin".to_owned(),
            host_kind: HubHostKind::Console,
            skip_profile_lock: true,
        })
        .await
        .expect("start Hub");
        let addr = hub.local_addr();
        let login_payload = r#"{"username":"admin","password":"password123"}"#;
        let login_request = format!(
            "POST /api/auth/login HTTP/1.0\r\nHost: {addr}\r\nX-Banto-Client: banto\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{login_payload}",
            login_payload.len()
        );
        let mut stream = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect login");
        stream
            .write_all(login_request.as_bytes())
            .await
            .expect("write login");
        let mut response = Vec::new();
        tokio::time::timeout(Duration::from_secs(10), stream.read_to_end(&mut response))
            .await
            .expect("login response timeout")
            .expect("read login");
        let raw = std::str::from_utf8(&response).expect("UTF-8 login response");
        let (_, body) = raw.split_once("\r\n\r\n").expect("login HTTP response");
        let login: serde_json::Value = serde_json::from_str(body).expect("login JSON");
        let token = login["token"].as_str().expect("admin token");

        assert_eq!(
            call(
                addr,
                "unauthenticated GET",
                "GET",
                None,
                None,
                Duration::from_secs(10)
            )
            .await
            .0,
            401
        );
        let (status, initial) = call(
            addr,
            "read initial settings",
            "GET",
            Some(token),
            None,
            Duration::from_secs(10),
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(initial["enabled"], false);
        assert!(hub.opcua.endpoint_and_namespace().await.is_none());

        let mut invalid = initial.clone();
        invalid["port"] = serde_json::json!(0);
        assert_eq!(
            call(
                addr,
                "reject invalid settings",
                "PUT",
                Some(token),
                Some(&invalid),
                Duration::from_secs(10),
            )
            .await
            .0,
            422
        );
        assert_eq!(
            call(
                addr,
                "verify settings unchanged after invalid PUT",
                "GET",
                Some(token),
                None,
                Duration::from_secs(10),
            )
            .await
            .1,
            initial
        );

        let holder = std::net::TcpListener::bind("127.0.0.1:0").expect("reserve OPC port");
        let opcua_port = holder.local_addr().expect("OPC port").port();
        drop(holder);
        let mut enabled = initial.clone();
        enabled["enabled"] = serde_json::json!(true);
        enabled["port"] = serde_json::json!(opcua_port);
        // #634: 有効化の PUT は保存して起動を始めたらすぐ返る（他の PUT と同じ
        // 10 秒の上限）。listener の起動は Windows CI で 10 秒を超えることがある
        // （一時 RSA 鍵の生成を含む）ので、起動済みは状態の API で待つ
        // （ハング検出用の有限な上限）。
        assert_eq!(
            call(
                addr,
                "enable OPC UA listener",
                "PUT",
                Some(token),
                Some(&enabled),
                Duration::from_secs(10),
            )
            .await
            .0,
            200
        );
        let started = tokio::time::Instant::now();
        let running = loop {
            let (status, body) = call_path(
                addr,
                "/api/opcua-status",
                "read OPC UA status",
                "GET",
                Some(token),
                None,
                Duration::from_secs(10),
            )
            .await;
            assert_eq!(status, 200);
            match body["state"].as_str() {
                Some("running") => break body,
                Some("starting") => {}
                other => panic!("unexpected OPC UA state {other:?}: {body}"),
            }
            assert!(
                started.elapsed() < Duration::from_secs(60),
                "OPC UA listener did not start within 60s: {body}"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        assert_eq!(
            running["endpoint"],
            serde_json::json!(format!("opc.tcp://127.0.0.1:{opcua_port}/"))
        );
        assert!(running["error"].is_null());
        assert_eq!(
            call(
                addr,
                "verify enabled settings",
                "GET",
                Some(token),
                None,
                Duration::from_secs(10),
            )
            .await
            .1,
            enabled
        );
        assert!(hub.opcua.endpoint_and_namespace().await.is_some());
        tokio::net::TcpStream::connect(("127.0.0.1", opcua_port))
            .await
            .expect("OPC UA listener started");

        enabled["enabled"] = serde_json::json!(false);
        assert_eq!(
            call(
                addr,
                "disable OPC UA listener",
                "PUT",
                Some(token),
                Some(&enabled),
                Duration::from_secs(10),
            )
            .await
            .0,
            200
        );
        assert_eq!(
            call(
                addr,
                "verify disabled settings",
                "GET",
                Some(token),
                None,
                Duration::from_secs(10),
            )
            .await
            .1,
            enabled
        );
        assert!(hub.opcua.endpoint_and_namespace().await.is_none());
        assert!(tokio::net::TcpStream::connect(("127.0.0.1", opcua_port))
            .await
            .is_err());
        let (status, stopped) = call_path(
            addr,
            "/api/opcua-status",
            "read OPC UA status after disable",
            "GET",
            Some(token),
            None,
            Duration::from_secs(10),
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(stopped["state"], "stopped");
        assert_eq!(
            call_path(
                addr,
                "/api/opcua-status",
                "unauthenticated OPC UA status",
                "GET",
                None,
                None,
                Duration::from_secs(10),
            )
            .await
            .0,
            401
        );
        hub.shutdown().await;
    }

    /// Hub の internal 値が実際の OPC UA Read と購読まで届く結合テスト。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn opcua_reads_and_subscribes_to_hub_internal_value() {
        use opcua::{
            client::{ClientBuilder, DataChangeCallback, IdentityToken},
            crypto::SecurityPolicy,
            types::{
                AttributeId, DateTime, MessageSecurityMode, MonitoredItemCreateRequest, NodeId,
                ReadValueId, StatusCode, TimestampsToReturn, UserTokenPolicy, Variant, WriteValue,
            },
        };

        let dir = crate::test_support::TempDir::new("hub-runtime-opcua-read");
        let db_path = dir
            .path()
            .join("registry.sqlite3")
            .to_string_lossy()
            .into_owned();
        let pool = init_db(&db_path).await.expect("init DB");
        ensure_virtual_connection(
            &pool,
            &crate::test_support::test_drivers().registry,
            MEM_CONNECTION_NAME,
        )
        .await;
        let connection_id: i64 =
            sqlx::query_scalar("SELECT id FROM plc_connections WHERE name = ?")
                .bind(MEM_CONNECTION_NAME)
                .fetch_one(&pool)
                .await
                .expect("mem connection");
        let group = CollectionGroupService::new(pool.clone())
            .create(banto_tags::CollectionGroupInput {
                name: "opcua-group".to_owned(),
                plc_connection_id: connection_id,
                period_ms: 100,
                enabled: true,
                default_writable: true,
                query_sql: None,
            })
            .await
            .expect("group");
        TagService::new(pool.clone())
            .create(banto_tags::TagInput {
                name: "temperature".to_owned(),
                collection_group_id: group.id,
                address: String::new(),
                data_type: "f32".to_owned(),
                string_length: None,
                string_encoding: "utf8".to_owned(),
                raw_lo: None,
                raw_hi: None,
                eng_lo: None,
                eng_hi: None,
                unit: None,
                decimals: 0,
                enabled: true,
                writable: false,
                tag_kind: banto_tags::INTERNAL_TAG_KIND.to_owned(),
                expression: None,
                retain: false,
                expected_revision: None,
            })
            .await
            .expect("internal tag");
        let port_holder = std::net::TcpListener::bind("127.0.0.1:0").expect("reserve OPC port");
        let opcua_port = port_holder.local_addr().expect("port").port();
        drop(port_holder);
        SettingsService::new(Db::Sqlite(pool.clone()))
            .set_opcua_config(&crate::settings::OpcUaSettings {
                enabled: true,
                port: opcua_port,
                ..Default::default()
            })
            .await
            .expect("save OPC UA settings");
        pool.close().await;

        let hub = HubRuntime::start(HubConfig {
            drivers: crate::test_support::test_drivers(),
            db_path,
            allow_setup: false,
            port_override: Some(0),
            bind_override: Some("127.0.0.1".to_owned()),
            data_dir_override: Some(dir.path().join("data")),
            profile_id: "test-opcua-read".to_owned(),
            host_kind: HubHostKind::Console,
            skip_profile_lock: true,
        })
        .await
        .expect("start Hub");
        let (endpoint, namespace) = hub
            .opcua
            .endpoint_and_namespace()
            .await
            .expect("OPC UA is listening");
        let entry = hub
            .manager
            .tag_map()
            .get("mem.opcua-group.temperature")
            .expect("catalog entry")
            .clone();
        let id = NodeId::new(namespace, entry.external_name.clone());

        let pki = tempfile::tempdir().expect("client PKI");
        let mut client = ClientBuilder::new()
            .application_name("banto-hub-opcua-test")
            .application_uri("urn:banto:hub:opcua:test")
            .create_sample_keypair(true)
            .pki_dir(pki.path())
            .trust_server_certs(true)
            .verify_server_certs(false)
            .client()
            .expect("client");
        let (session, event_loop) = client
            .connect_to_matching_endpoint(
                (
                    endpoint.as_str(),
                    SecurityPolicy::None.to_str(),
                    MessageSecurityMode::None,
                    UserTokenPolicy::anonymous(),
                ),
                IdentityToken::Anonymous,
            )
            .await
            .expect("connect");
        let task = event_loop.spawn();
        assert!(
            tokio::time::timeout(Duration::from_secs(10), session.wait_for_connection())
                .await
                .expect("connection timeout")
        );
        let read = || async {
            session
                .read(
                    &[ReadValueId {
                        node_id: id.clone(),
                        attribute_id: AttributeId::Value as u32,
                        ..Default::default()
                    }],
                    TimestampsToReturn::Both,
                    0.0,
                )
                .await
                .expect("Read")
                .remove(0)
        };
        let initial = read().await;
        assert_eq!(initial.status(), StatusCode::Bad);
        assert_eq!(initial.source_timestamp, None);

        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let subscription = session
            .create_subscription(
                Duration::from_millis(100),
                30,
                10,
                0,
                0,
                true,
                DataChangeCallback::new(move |value, _| {
                    let _ = sender.send(value);
                }),
            )
            .await
            .expect("subscription");
        let request: MonitoredItemCreateRequest = id.clone().into();
        session
            .create_monitored_items(subscription, TimestampsToReturn::Both, vec![request])
            .await
            .expect("monitored item");
        // MonitoredItem の登録応答だけでは初期通知の配達前のことがある。
        // Bad を受け取ってから Good に変え、移行通知を取り逃がさない。
        let subscribed_initial = tokio::time::timeout(Duration::from_secs(15), receiver.recv())
            .await
            .expect("initial DataChange timeout")
            .expect("subscription closed");
        assert_eq!(subscribed_initial.status(), StatusCode::Bad);
        hub.manager.server_store().set(
            &entry.tag_key,
            Some(12.5),
            Quality::Good,
            1_700_000_000_000,
        );
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let value = receiver.recv().await.expect("subscription closed");
                if value.status() == StatusCode::Good {
                    assert_eq!(value.value, Some(Variant::Double(12.5)));
                    break;
                }
            }
        })
        .await
        .expect("Good DataChange timeout");
        let current = read().await;
        assert_eq!(current.status(), StatusCode::Good);
        assert_eq!(current.value, Some(Variant::Double(12.5)));
        let source_at = |ms| {
            let time = std::time::UNIX_EPOCH + Duration::from_millis(ms);
            DateTime::from(
                banto_opcua_server::model::SampleTimestamps::new(Some(time), time)
                    .source_ticks()
                    .expect("OPC UA timestamp"),
            )
        };
        assert_eq!(current.source_timestamp, Some(source_at(1_700_000_000_000)));

        let writes = session
            .write(&[WriteValue::value_attr(id.clone(), Variant::Double(99.0))])
            .await
            .expect("Write response");
        assert!(matches!(
            writes.as_slice(),
            [StatusCode::BadNotWritable | StatusCode::BadUserAccessDenied]
        ));
        assert_eq!(read().await.value, Some(Variant::Double(12.5)));
        // 同じ現在値のまま複数回の照合・購読周期が経過しても、公開時刻だけで
        // DataChange を発生させない。
        assert!(
            tokio::time::timeout(Duration::from_millis(800), receiver.recv())
                .await
                .is_err(),
            "unchanged value must not trigger DataChange"
        );

        // Hub の250ms照合とは別に100msの MonitoredItem を作る。
        // 採取時刻が前値から50msだけ進んだ更新は次の照合・サンプリングまで
        // 待たされるが、通知時刻を SourceTimestamp に上書きしてはならない。
        let (sampled_sender, mut sampled_receiver) = tokio::sync::mpsc::unbounded_channel();
        let sampled_subscription = session
            .create_subscription(
                Duration::from_millis(100),
                30,
                10,
                0,
                0,
                true,
                DataChangeCallback::new(move |value, _| {
                    let _ = sampled_sender.send(value);
                }),
            )
            .await
            .expect("100ms subscription");
        let mut sampled_request: MonitoredItemCreateRequest = id.clone().into();
        sampled_request.requested_parameters.sampling_interval = 100.0;
        let sampled_items = session
            .create_monitored_items(
                sampled_subscription,
                TimestampsToReturn::Both,
                vec![sampled_request],
            )
            .await
            .expect("100ms monitored item");
        assert_eq!(sampled_items[0].result.revised_sampling_interval, 100.0);
        let sampled_initial =
            tokio::time::timeout(Duration::from_secs(10), sampled_receiver.recv())
                .await
                .expect("100ms initial DataChange timeout")
                .expect("100ms subscription closed");
        assert_eq!(sampled_initial.value, Some(Variant::Double(12.5)));
        assert_eq!(
            sampled_initial.source_timestamp,
            Some(source_at(1_700_000_000_000))
        );
        hub.manager.server_store().set(
            &entry.tag_key,
            Some(12.75),
            Quality::Good,
            1_700_000_000_050,
        );
        let sampled_update = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let value = sampled_receiver
                    .recv()
                    .await
                    .expect("100ms subscription closed");
                if value.value == Some(Variant::Double(12.75)) {
                    break value;
                }
            }
        })
        .await
        .expect("100ms changed DataChange timeout");
        assert_eq!(sampled_update.status(), StatusCode::Good);
        assert_eq!(
            sampled_update.source_timestamp,
            Some(source_at(1_700_000_000_050))
        );
        assert_eq!(
            read().await.source_timestamp,
            sampled_update.source_timestamp
        );

        hub.manager.server_store().set_with_observation_time(
            &entry.tag_key,
            Some(12.75),
            Quality::Stale,
            1_700_000_000_050,
            1_700_000_001_000,
        );
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let value = receiver.recv().await.expect("subscription closed");
                if value.status() == StatusCode::UncertainLastUsableValue {
                    assert_eq!(value.value, Some(Variant::Double(12.75)));
                    assert_eq!(value.source_timestamp, Some(source_at(1_700_000_001_000)));
                    break;
                }
            }
        })
        .await
        .expect("Stale DataChange timeout");

        hub.manager
            .server_store()
            .set(&entry.tag_key, None, Quality::Bad, 1_700_000_002_000);
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let value = receiver.recv().await.expect("subscription closed");
                if value.status() == StatusCode::Bad {
                    assert_eq!(value.value, Some(Variant::Empty));
                    assert_eq!(value.source_timestamp, Some(source_at(1_700_000_002_000)));
                    break;
                }
            }
        })
        .await
        .expect("Bad DataChange timeout");

        hub.manager.server_store().set(
            &entry.tag_key,
            Some(13.5),
            Quality::Good,
            1_700_000_003_000,
        );
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if receiver.recv().await.expect("subscription closed").status() == StatusCode::Good
                {
                    break;
                }
            }
        })
        .await
        .expect("Good recovery timeout");

        let pool = hub.manager.pool();
        sqlx::query("UPDATE tags SET enabled = 0 WHERE id = ?")
            .bind(entry.ids.2)
            .execute(&pool)
            .await
            .expect("disable tag");
        hub.manager.rebuild().await.expect("publish disabled tag");
        let disabled = tokio::time::timeout(Duration::from_secs(10), receiver.recv())
            .await
            .expect("disabled DataChange timeout")
            .expect("subscription closed");
        assert_eq!(disabled.status(), StatusCode::Bad);
        assert_eq!(disabled.value, Some(Variant::Empty));
        assert_eq!(read().await.status(), StatusCode::Bad);
        assert!(hub.manager.tag_map().get(&entry.external_name).is_some());

        sqlx::query("UPDATE tags SET enabled = 1 WHERE id = ?")
            .bind(entry.ids.2)
            .execute(&pool)
            .await
            .expect("re-enable tag");
        hub.manager.rebuild().await.expect("publish enabled tag");
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if receiver.recv().await.expect("subscription closed").status() == StatusCode::Good
                {
                    break;
                }
            }
        })
        .await
        .expect("re-enabled DataChange timeout");
        assert_eq!(read().await.value, Some(Variant::Double(13.5)));

        sqlx::query("UPDATE tags SET name = 'renamed' WHERE id = ?")
            .bind(entry.ids.2)
            .execute(&pool)
            .await
            .expect("rename tag");
        hub.manager.rebuild().await.expect("publish renamed tag");
        let old = tokio::time::timeout(Duration::from_secs(10), receiver.recv())
            .await
            .expect("rename DataChange timeout")
            .expect("subscription closed");
        assert_eq!(old.status(), StatusCode::BadNodeIdUnknown);
        assert_eq!(read().await.status(), StatusCode::BadNodeIdUnknown);

        let renamed_id = NodeId::new(namespace, "mem.opcua-group.renamed");
        let renamed_read = session
            .read(
                &[ReadValueId {
                    node_id: renamed_id.clone(),
                    attribute_id: AttributeId::Value as u32,
                    ..Default::default()
                }],
                TimestampsToReturn::Both,
                0.0,
            )
            .await
            .expect("Read renamed tag")
            .remove(0);
        assert_eq!(renamed_read.status(), StatusCode::Good);
        assert_eq!(renamed_read.value, Some(Variant::Double(13.5)));

        let (renamed_sender, mut renamed_receiver) = tokio::sync::mpsc::unbounded_channel();
        let renamed_subscription = session
            .create_subscription(
                Duration::from_millis(100),
                30,
                10,
                0,
                0,
                true,
                DataChangeCallback::new(move |value, _| {
                    let _ = renamed_sender.send(value);
                }),
            )
            .await
            .expect("renamed subscription");
        session
            .create_monitored_items(
                renamed_subscription,
                TimestampsToReturn::Both,
                vec![renamed_id.clone().into()],
            )
            .await
            .expect("renamed monitored item");
        let renamed_initial =
            tokio::time::timeout(Duration::from_secs(10), renamed_receiver.recv())
                .await
                .expect("renamed initial DataChange timeout")
                .expect("renamed subscription closed");
        assert_eq!(renamed_initial.status(), StatusCode::Good);

        TagService::new(pool)
            .delete(entry.ids.2)
            .await
            .expect("delete tag");
        hub.manager.rebuild().await.expect("publish deleted tag");
        let deleted = tokio::time::timeout(Duration::from_secs(10), renamed_receiver.recv())
            .await
            .expect("delete DataChange timeout")
            .expect("renamed subscription closed");
        assert_eq!(deleted.status(), StatusCode::BadNodeIdUnknown);
        let deleted_read = session
            .read(
                &[ReadValueId {
                    node_id: renamed_id,
                    attribute_id: AttributeId::Value as u32,
                    ..Default::default()
                }],
                TimestampsToReturn::Both,
                0.0,
            )
            .await
            .expect("Read deleted tag")
            .remove(0);
        assert_eq!(deleted_read.status(), StatusCode::BadNodeIdUnknown);

        session.disconnect().await.expect("disconnect");
        task.abort();
        hub.shutdown().await;
    }

    /// #479: 退避の規則（純関数）。シェル・`PORT` 無し・永続ポートが既定のときだけ。
    #[test]
    fn port_fallback_applies_only_to_shell_with_default_unforced_port() {
        use HubHostKind::{Console, Service, Shell};
        // 退避する: シェル + PORT 無し + 設定が既定。
        assert!(port_fallback_applies(Shell, None, DEFAULT_PORT));
        // PORT 環境変数による明示（既定と同じ値でも、明示は固定）。
        assert!(!port_fallback_applies(
            Shell,
            Some(DEFAULT_PORT),
            DEFAULT_PORT
        ));
        assert!(!port_fallback_applies(Shell, Some(9000), DEFAULT_PORT));
        assert!(!port_fallback_applies(Shell, Some(0), DEFAULT_PORT));
        // 永続設定が既定と違う = 利用者が選んだ値は固定。
        assert!(!port_fallback_applies(Shell, None, DEFAULT_PORT + 1));
        assert!(!port_fallback_applies(Shell, None, 9000));
        // サービス版・コンソール版は退避しない（外のクライアントが既定ポートで来る）。
        assert!(!port_fallback_applies(Service, None, DEFAULT_PORT));
        assert!(!port_fallback_applies(Console, None, DEFAULT_PORT));
    }

    /// #479: 退避の対象にする bind 失敗の種類（純関数）。
    #[test]
    fn bind_error_allows_port_fallback_only_for_in_use_and_denied() {
        use std::io::ErrorKind;
        assert!(bind_error_allows_port_fallback(ErrorKind::AddrInUse));
        assert!(bind_error_allows_port_fallback(ErrorKind::PermissionDenied));
        for kind in [
            ErrorKind::AddrNotAvailable,
            ErrorKind::InvalidInput,
            ErrorKind::NotFound,
            ErrorKind::TimedOut,
            ErrorKind::Other,
        ] {
            assert!(!bind_error_allows_port_fallback(kind), "{kind:?}");
        }
    }

    /// #479: ポートが埋まっていて退避が許されていれば、同じ bind アドレスの別
    /// ポートで起動し、元のポートを返す。許されていなければ今までどおり失敗する。
    /// 8722 には依存しない（テストが自分で `TcpListener` を握った任意のポートを使う）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn start_with_port_fallback_moves_off_a_busy_port_only_when_allowed() {
        let holder = std::net::TcpListener::bind("127.0.0.1:0").expect("hold a port");
        let busy = holder.local_addr().unwrap().port();
        let router = || axum::Router::new().route("/", axum::routing::get(|| async { "ok" }));

        // 許可なし: 失敗（従来どおり）。
        let denied = start_with_port_fallback("127.0.0.1", busy, router(), false).await;
        assert!(denied.is_err(), "must fail when fallback is not allowed");

        // 許可あり: 別ポートで起動し、元のポートを報告する。
        let (server, from) = start_with_port_fallback("127.0.0.1", busy, router(), true)
            .await
            .expect("should fall back to an OS-assigned port");
        assert_eq!(from, Some(busy));
        let addr = server.local_addr();
        assert_eq!(addr.ip().to_string(), "127.0.0.1");
        assert_ne!(addr.port(), 0);
        assert_ne!(addr.port(), busy);
        server.stop().await;

        // 空いていれば退避しない（許可ありでも元のポートのまま）。
        drop(holder);
        let (server, from) = start_with_port_fallback("127.0.0.1", busy, router(), true)
            .await
            .expect("free port should bind as asked");
        assert_eq!(from, None);
        assert_eq!(server.local_addr().port(), busy);
        server.stop().await;

        // 退避しても直らない失敗（不正な bind アドレス）はそのままエラー。
        let bad = start_with_port_fallback("203.0.113.1", 0, router(), true).await;
        assert!(bad.is_err());
    }

    /// #479: シェルが組むのと同じ `HubConfig`（`HubHostKind::Shell`）でも、`PORT`
    /// 相当の明示（`port_override`）があれば埋まったポートで起動に失敗する
    /// （退避しない）。退避する側は上のテストと [`port_fallback_applies`] の表で
    /// 押さえる（既定の 8722 を埋めることはテストにできない）。失敗後に背景タスク・
    /// ロックが残らず、同じ構成の再起動が通ること（空きが出れば）も確かめる。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shell_start_with_explicit_busy_port_fails_and_leaves_nothing_behind() {
        let dir = crate::test_support::TempDir::new("hub-runtime-shell-explicit-port");
        let holder = std::net::TcpListener::bind("127.0.0.1:0").expect("hold a port");
        let busy = holder.local_addr().unwrap().port();
        let make = |port: u16| HubConfig {
            drivers: crate::test_support::test_drivers(),
            db_path: dir
                .path()
                .join("registry.sqlite3")
                .to_string_lossy()
                .into_owned(),
            allow_setup: false,
            port_override: Some(port),
            bind_override: Some("127.0.0.1".to_string()),
            data_dir_override: Some(dir.path().join("data")),
            profile_id: "test".to_string(),
            host_kind: HubHostKind::Shell,
            skip_profile_lock: true,
        };

        let err = match HubRuntime::start(make(busy)).await {
            Ok(_) => panic!("an explicit busy port must not fall back"),
            Err(err) => err,
        };
        assert!(matches!(err, HubStartError::ServerStart(_)), "{err:?}");

        // 失敗した起動が何も残していない: 同じ DB・data_dir でもう一度起動できる。
        let hub = HubRuntime::start(make(0))
            .await
            .expect("restart after a failed bind should succeed");
        assert_eq!(hub.port_fallback_from(), None);
        hub.shutdown().await;
        drop(holder);
    }

    /// #500: `HubRuntime::start` が組み立てる Router（コンソール/サービス/
    /// デスクトップ版の共通経路）の応答に、banto のセキュリティヘッダーが
    /// 付く。静的 UI（`/`）と `/api/*` の JSON の両方を、実際にバインドした
    /// ソケットへの素の HTTP/1.0 要求で確かめる（`with_security_headers` を
    /// 外すとここが落ちる）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn served_responses_carry_security_headers() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let dir = crate::test_support::TempDir::new("hub-runtime-security-headers");
        let db_path = dir
            .path()
            .join("registry.sqlite3")
            .to_string_lossy()
            .into_owned();
        let config = HubConfig {
            drivers: crate::test_support::test_drivers(),
            db_path,
            allow_setup: false,
            port_override: Some(0),
            bind_override: Some("127.0.0.1".to_string()),
            data_dir_override: Some(dir.path().join("data")),
            profile_id: "test-security-headers".to_string(),
            host_kind: HubHostKind::Console,
            skip_profile_lock: true,
        };
        let hub = HubRuntime::start(config)
            .await
            .expect("HubRuntime::start should succeed against a fresh temp DB");
        let addr = hub.local_addr();

        for path in ["/", "/api/auth/status"] {
            let mut stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
            stream
                .write_all(
                    format!(
                        "GET {path} HTTP/1.0
Host: {addr}

"
                    )
                    .as_bytes(),
                )
                .await
                .expect("write");
            let mut raw = Vec::new();
            stream.read_to_end(&mut raw).await.expect("read");
            let head = String::from_utf8_lossy(&raw).to_ascii_lowercase();
            let head = head
                .split(
                    "

",
                )
                .next()
                .unwrap_or_default();
            // #505: このテストの接続元は loopback（`127.0.0.1`）なので、CSP は
            // `connect-src` に Tauri IPC を足したもの（デスクトップ版の窓が
            // navigate する経路）。LAN からの要求が厳格なままであることは
            // `hub_security_headers_widen_connect_src_only_for_loopback_local_requests`。
            let widened = format!(
                "content-security-policy: {}",
                hub_security_headers()
                    .content_security_policy()
                    .to_ascii_lowercase()
            );
            assert!(widened.contains("connect-src 'self' ipc: http://ipc.localhost;"));
            for expected in [
                "x-content-type-options: nosniff",
                "x-frame-options: deny",
                "referrer-policy: same-origin",
                widened.as_str(),
            ] {
                assert!(
                    head.contains(expected),
                    "{path}: missing `{expected}` in {head}"
                );
            }
        }

        hub.shutdown().await;
    }

    /// #505: [`hub_security_headers`] は TCP の接続元と要求の宛先（`Host`）の
    /// 両方が loopback の要求にだけ `connect-src` に Tauri IPC を足す
    /// （`request_is_loopback_local`、banto v5.1.0）。LAN（非 loopback）からの
    /// 要求、接続元が分からない要求（`ConnectInfo` の無い起動経路）、`Host` が
    /// loopback でない要求（Host を保つ同一ホストのプロキシ経由の LAN の
    /// 閲覧者）、`Host` の無い要求には banto の厳格な CSP をそのまま返す。
    /// ほかの 3 つのヘッダーは変わらない。`HubRuntime::start` がこの設定を
    /// 掛けていることは `served_responses_carry_security_headers`（実ソケット・
    /// loopback）が見る。
    #[tokio::test]
    async fn hub_security_headers_widen_connect_src_only_for_loopback_local_requests() {
        use std::net::SocketAddr;

        use axum::body::Body;
        use axum::extract::ConnectInfo;
        use axum::http::{header, Request};
        use axum::routing::get;
        use tower::ServiceExt;

        let router =
            with_hub_security_headers(axum::Router::new().route("/", get(|| async { "ok" })));
        let widened = SecurityHeaders::new()
            .extra_connect_src(TAURI_IPC_CONNECT_SRC)
            .expect("valid sources")
            .content_security_policy();
        assert_ne!(widened, banto_server::CONTENT_SECURITY_POLICY);

        let csp_for = |peer: Option<&str>, host: Option<&str>| {
            let router = router.clone();
            let mut builder = Request::builder().uri("/");
            if let Some(host) = host {
                builder = builder.header(header::HOST, host);
            }
            let mut request = builder.body(Body::empty()).expect("request");
            if let Some(peer) = peer {
                let addr: SocketAddr = peer.parse().expect("socket addr");
                request.extensions_mut().insert(ConnectInfo(addr));
            }
            async move {
                let response = router.oneshot(request).await.expect("response");
                let headers = response.headers();
                assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
                assert_eq!(headers[header::X_FRAME_OPTIONS], "DENY");
                assert_eq!(headers[header::REFERRER_POLICY], "same-origin");
                headers[header::CONTENT_SECURITY_POLICY]
                    .to_str()
                    .expect("ascii")
                    .to_owned()
            }
        };

        // デスクトップ版のシェル（同じ機械の WebView）= loopback の接続元 +
        // loopback の宛先（`http://127.0.0.1:<port>/`）。
        assert_eq!(
            csp_for(Some("127.0.0.1:50000"), Some("127.0.0.1:8722")).await,
            widened
        );
        assert_eq!(
            csp_for(Some("[::1]:50000"), Some("localhost:8722")).await,
            widened
        );
        // 厳格なまま: LAN のブラウザ（直接つなぐ。宛先が loopback でも接続元が
        // LAN）、同一ホストのプロキシが Host を保つ LAN の閲覧者（接続元は
        // loopback・宛先は公開名）、Host の無い要求、接続元が分からない要求。
        for (peer, host, why) in [
            (
                Some("192.168.11.20:50000"),
                Some("192.168.11.5:8722"),
                "LAN peer",
            ),
            (
                Some("10.0.0.5:50000"),
                Some("127.0.0.1:8722"),
                "LAN peer, loopback Host",
            ),
            (
                Some("127.0.0.1:50000"),
                Some("hub.example.lan"),
                "same-host proxy preserving Host",
            ),
            (Some("127.0.0.1:50000"), None, "no Host"),
            (None, Some("127.0.0.1:8722"), "no ConnectInfo"),
        ] {
            assert_eq!(
                csp_for(peer, host).await,
                banto_server::CONTENT_SECURITY_POLICY,
                "{why}: peer {peer:?} host {host:?} must keep the strict CSP"
            );
        }
    }

    /// 試運転モードとロックダウン（設計 §5.6 制約1・2026-08-30 オーナー決定）:
    /// 非 loopback バインド（`0.0.0.0`）+ 未ロックダウン（フラグ未設定の
    /// フレッシュ DB）は`HubRuntime::start`が起動そのものを拒否する -
    /// `HubStartError::UnsafeCommissioningBind`で、DB を開いた後に検出
    /// されるが、`banto_server::start`（実バインド）より前に短絡する
    /// （このテストは`local_addr`を一切呼ばずに`Err`を確認することで、
    /// 実際にソケットを bind していないことを間接的に裏付ける）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn start_rejects_non_loopback_bind_when_not_locked_down() {
        let dir = crate::test_support::TempDir::new("hub-runtime-commissioning-reject");
        let db_path = dir
            .path()
            .join("registry.sqlite3")
            .to_string_lossy()
            .into_owned();
        let data_dir = dir.path().join("data");

        let config = HubConfig {
            drivers: crate::test_support::test_drivers(),
            db_path,
            allow_setup: false,
            port_override: Some(0),
            bind_override: Some("0.0.0.0".to_string()),
            data_dir_override: Some(data_dir),
            profile_id: "test-commissioning-reject".to_string(),
            host_kind: HubHostKind::Console,
            skip_profile_lock: true,
        };

        match HubRuntime::start(config).await {
            Err(HubStartError::UnsafeCommissioningBind(message)) => {
                assert!(message.contains("loopback"));
            }
            Err(other) => panic!("expected UnsafeCommissioningBind, got: {other}"),
            Ok(hub) => {
                hub.shutdown().await;
                panic!(
                    "expected HubRuntime::start to refuse a non-loopback bind while \
                     not locked down"
                );
            }
        }
    }

    /// 試運転モードとロックダウン（設計 §5.6 制約1）: loopback バインド
    /// （`127.0.0.1`）+ 未ロックダウン（フレッシュ DB）は制約1に抵触しない
    /// ため、通常どおり起動できる - 試運転は「Hub が動いている機械の前」
    /// （loopback）で行う前提なので、これが許可される唯一の未ロックダウン
    /// 構成。`start_local_addr_then_shutdown_round_trip`と実質同じ前提
    /// （フレッシュ DB = 未ロックダウン）だが、このテストは名前と doc
    /// comment でこの安全制約の「許可される側」を明示的に固定する。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn start_succeeds_with_loopback_bind_when_not_locked_down() {
        let dir = crate::test_support::TempDir::new("hub-runtime-commissioning-allow");
        let db_path = dir
            .path()
            .join("registry.sqlite3")
            .to_string_lossy()
            .into_owned();
        let data_dir = dir.path().join("data");

        let config = HubConfig {
            drivers: crate::test_support::test_drivers(),
            db_path,
            allow_setup: false,
            port_override: Some(0),
            bind_override: Some("127.0.0.1".to_string()),
            data_dir_override: Some(data_dir),
            profile_id: "test-commissioning-allow".to_string(),
            host_kind: HubHostKind::Console,
            skip_profile_lock: true,
        };

        let hub = HubRuntime::start(config).await.expect(
            "HubRuntime::start should succeed for a loopback bind even while not locked down",
        );
        hub.shutdown().await;
    }

    /// T17-1（docs/banto-hub-t17-design.md §3「T17-1」・P2）: `skip_profile_lock:
    /// false`で、対象 profile を既に別のガードが保持している場合、
    /// `HubRuntime::start`は DB を一度も開かずに`HubStartError::ProfileLock`
    /// で失敗する（設計 §1 P2「失敗時は安全側で起動拒否」）。
    ///
    /// `BANTO_HUB_ROOT`を一時ディレクトリへ向けて`start`内部の root 解決
    /// （このモジュール doc「T17-1 での唯一の例外」節）を決定的にする -
    /// このプロセス内で`BANTO_HUB_ROOT`を読むのはこのテストだけなので、
    /// 他テストとの並行実行による競合は起きない。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn start_fails_with_profile_lock_when_another_guard_already_holds_it() {
        let dir = crate::test_support::TempDir::new("hub-runtime-profile-lock");
        let root = dir.path().join("root");
        let profile_id = "locked-profile";

        let paths = resolve_profile_paths(&root, profile_id).expect("valid profile id");
        let _existing_lock = try_acquire_profile_lock(&paths, HubHostKind::Console)
            .expect("first acquire should succeed");

        let db_path = dir
            .path()
            .join("registry.sqlite3")
            .to_string_lossy()
            .into_owned();
        let config = HubConfig {
            drivers: crate::test_support::test_drivers(),
            db_path,
            allow_setup: false,
            port_override: Some(0),
            bind_override: Some("127.0.0.1".to_string()),
            data_dir_override: Some(dir.path().join("data")),
            profile_id: profile_id.to_string(),
            host_kind: HubHostKind::Service,
            skip_profile_lock: false,
        };

        std::env::set_var("BANTO_HUB_ROOT", root.to_string_lossy().as_ref());
        let result = HubRuntime::start(config).await;
        std::env::remove_var("BANTO_HUB_ROOT");

        match result {
            Err(HubStartError::ProfileLock(_)) => {}
            Err(other) => panic!("expected ProfileLock error, got: {other}"),
            Ok(hub) => {
                hub.shutdown().await;
                panic!("expected HubRuntime::start to fail while the profile lock is held");
            }
        }
    }

    /// P3-a 追補（2026-08-12、docs/banto-hub-remaining-plan.md）:
    /// [`audit_prune_once`] is the exact function [`HubRuntime::start`]
    /// calls both once at startup and on every tick of the 24h
    /// `prune_handle` loop (see that closure's body above) - the periodic
    /// interval itself cannot be waited out in a test, so this pins down
    /// the shared function the loop actually invokes: given a retention
    /// policy that caps rows below the seeded count, calling it once
    /// prunes down to that cap, proving the wiring this function exists for
    /// (settings → `AuditLogService::prune`) is correct independent of
    /// which of the three call sites triggers it.
    #[tokio::test]
    async fn audit_prune_once_prunes_down_to_the_configured_row_cap() {
        let pool = crate::db::migrate_memory().await.expect("migrate_memory");
        let settings = SettingsService::new(Db::Sqlite(pool.clone()));
        let audit = AuditLogService::new(Db::Sqlite(pool.clone()));

        for i in 0..5 {
            sqlx::query(
                "INSERT INTO audit_log (actor_username, actor_role, action, resource, entity_id, detail, origin, result) \
                 VALUES ('admin', 'admin', 'create', 'items', ?, NULL, 'rest', 'ok')",
            )
            .bind(i.to_string())
            .execute(&pool)
            .await
            .unwrap();
        }

        settings
            .set_audit_config(&crate::settings::AuditSettings {
                retention_days: None,
                retention_rows: Some(2),
            })
            .await
            .expect("set_audit_config");

        audit_prune_once(&settings, &audit).await;

        let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(remaining, 2);
    }

    /// A settings read failure (simulated here by a plain misconfigured
    /// `SettingsService`... in practice `SettingsService::audit_config`
    /// only fails on a real storage error, which is hard to induce without
    /// a broken pool) is out of scope for this test - the important
    /// behavior already covered by [`prune_once`]'s call sites in
    /// `HubRuntime::start` is "never panics, never aborts the loop", which
    /// [`audit_prune_once_prunes_down_to_the_configured_row_cap`]'s
    /// successful round trip already exercises the non-error path of. A
    /// `None`/`None` policy (unlimited on both dimensions) must be a no-op,
    /// matching [`crate::audit::AuditLogService::prune`]'s own
    /// `prune_with_both_none_is_a_no_op` unit test.
    #[tokio::test]
    async fn audit_prune_once_with_unlimited_policy_is_a_no_op() {
        let pool = crate::db::migrate_memory().await.expect("migrate_memory");
        let settings = SettingsService::new(Db::Sqlite(pool.clone()));
        let audit = AuditLogService::new(Db::Sqlite(pool.clone()));

        sqlx::query(
            "INSERT INTO audit_log (actor_username, actor_role, action, resource, entity_id, detail, origin, result) \
             VALUES ('admin', 'admin', 'create', 'items', '1', NULL, 'rest', 'ok')",
        )
        .execute(&pool)
        .await
        .unwrap();

        settings
            .set_audit_config(&crate::settings::AuditSettings {
                retention_days: None,
                retention_rows: None,
            })
            .await
            .expect("set_audit_config");

        audit_prune_once(&settings, &audit).await;

        let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(remaining, 1);
    }

    // --- T19 S2-d (docs/banto-hub-t19-design.md §5.1、UX-39):
    // `prune_once`が`StoreSettings.retention_days`の`Option<i64>`化に
    // 追従していること（`Some`は従来どおり剪定、`None`＝無制限は剪定
    // しない）を、`audit_prune_once`の上の2テストと対になる形で固定する。

    fn manual_clock_at(today: LocalDate) -> Arc<dyn banto_tstore::Clock> {
        // 正午 UTC・オフセット0 - `today`がそのままローカル日付になる
        // （`LocalDate::from_epoch_ms`のdoc comment参照）、境界からは
        // 十分離れているので日付計算の丸め誤差を気にしなくてよい。
        let now_ms = today.to_days_since_epoch() * 86_400_000 + 12 * 3_600_000;
        Arc::new(banto_tstore::ManualClock::new(now_ms, 0))
    }

    /// 剪定の呼び出し口（`CollectorManager::prune_data_files`）を通すための、
    /// 収集を始めていない管理役。
    fn test_manager(
        pool: &sqlx::SqlitePool,
        data_dir: &std::path::Path,
        clock: Arc<dyn banto_tstore::Clock>,
    ) -> CollectorManager {
        CollectorManager::new(
            pool.clone(),
            data_dir.to_path_buf(),
            clock,
            CollectorOptions::default(),
            Arc::new(HubSessions::new(
                banto_broker::BackoffConfig::default(),
                crate::test_support::test_drivers(),
            )),
            Arc::new(BrokerSimRegistry::new()),
            Arc::new(ComputedEngine::new(Arc::new(ServerTagStore::new()))),
        )
    }

    fn touch_test_data_file(data_dir: &std::path::Path, date: LocalDate, seq: u32) {
        std::fs::create_dir_all(data_dir).expect("create data dir");
        std::fs::write(
            data_dir.join(format!("{}-{:03}.sqlite3", date.to_yyyymmdd(), seq)),
            b"",
        )
        .expect("touch data file");
    }

    #[tokio::test]
    async fn prune_once_deletes_aged_out_files_with_a_finite_policy() {
        let pool = crate::db::migrate_memory().await.expect("migrate_memory");
        let settings = SettingsService::new(Db::Sqlite(pool.clone()));
        settings
            .set_store_config(&crate::settings::StoreSettings {
                data_dir: "./data".to_string(),
                retention_days: Some(7),
            })
            .await
            .expect("set_store_config");

        let dir = crate::test_support::TempDir::new("prune-once-finite");
        let data_dir = dir.path().join("data");
        let today = LocalDate::new(2026, 7, 12);
        touch_test_data_file(&data_dir, LocalDate::new(2026, 6, 1), 1); // aged out
        touch_test_data_file(&data_dir, today, 1); // kept

        let clock = manual_clock_at(today);
        let manager = test_manager(&pool, &data_dir, clock.clone());
        prune_once(&settings, &manager, clock.as_ref()).await;

        let remaining = banto_tstore::list_data_files(&data_dir).expect("list_data_files");
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].date, today);
    }

    /// UX-39 オーナー決定2（無制限の選択肢）: `retention_days: None`のとき
    /// `prune_once`は`banto_tstore::prune_files`を一切呼ばない - 古い
    /// ファイルがそのまま残ることで間接的に確認する。
    #[tokio::test]
    async fn prune_once_with_unlimited_policy_is_a_no_op() {
        let pool = crate::db::migrate_memory().await.expect("migrate_memory");
        let settings = SettingsService::new(Db::Sqlite(pool.clone()));
        settings
            .set_store_config(&crate::settings::StoreSettings {
                data_dir: "./data".to_string(),
                retention_days: None,
            })
            .await
            .expect("set_store_config");

        let dir = crate::test_support::TempDir::new("prune-once-unlimited");
        let data_dir = dir.path().join("data");
        let today = LocalDate::new(2026, 7, 12);
        touch_test_data_file(&data_dir, LocalDate::new(2000, 1, 1), 1);

        let clock = manual_clock_at(today);
        let manager = test_manager(&pool, &data_dir, clock.clone());
        prune_once(&settings, &manager, clock.as_ref()).await;

        let remaining = banto_tstore::list_data_files(&data_dir).expect("list_data_files");
        assert_eq!(
            remaining.len(),
            1,
            "unlimited retention must delete nothing"
        );
    }

    /// レビュー指摘の安全化: 保持日数が`u32`に収まらない想定外の値
    /// （`parse_retention`は上限クランプをしないため、保存経路を通らず
    /// 直接キーへ書き込まれた等で`u32::MAX`超の値が入り得る）のとき、
    /// `prune_once`は`banto_tstore::prune_files`を呼ばず（＝デフォルト
    /// 7日等にフォールバックせず）何も削除しない - 不可逆な削除を伴う
    /// 剪定の失敗方向は「保持する側」でなければならない。
    #[tokio::test]
    async fn prune_once_with_out_of_range_retention_days_is_a_no_op() {
        let pool = crate::db::migrate_memory().await.expect("migrate_memory");
        let settings = SettingsService::new(Db::Sqlite(pool.clone()));
        settings
            .set_store_config(&crate::settings::StoreSettings {
                data_dir: "./data".to_string(),
                retention_days: Some(u32::MAX as i64 + 1),
            })
            .await
            .expect("set_store_config");

        let dir = crate::test_support::TempDir::new("prune-once-out-of-range");
        let data_dir = dir.path().join("data");
        let today = LocalDate::new(2026, 7, 12);
        touch_test_data_file(&data_dir, LocalDate::new(2000, 1, 1), 1);

        let clock = manual_clock_at(today);
        let manager = test_manager(&pool, &data_dir, clock.clone());
        prune_once(&settings, &manager, clock.as_ref()).await;

        let remaining = banto_tstore::list_data_files(&data_dir).expect("list_data_files");
        assert_eq!(
            remaining.len(),
            1,
            "out-of-range retention_days must delete nothing"
        );
    }

    /// 保持設定が読めないとき、`prune_once` は既定の日数で消さずスキップする
    /// （無制限・長い保持の設定が、読み取り失敗で既定の 7 日として扱われて
    /// 履歴を失うのを防ぐ。ChronoGazer #540 と同じ）。
    #[tokio::test]
    async fn prune_once_with_unreadable_settings_deletes_nothing() {
        let pool = crate::db::migrate_memory().await.expect("migrate_memory");
        let settings = SettingsService::new(Db::Sqlite(pool.clone()));

        let dir = crate::test_support::TempDir::new("prune-once-unreadable");
        let data_dir = dir.path().join("data");
        let today = LocalDate::new(2026, 7, 12);
        touch_test_data_file(&data_dir, LocalDate::new(2026, 6, 1), 1); // 既定 7 日なら消える

        let clock = manual_clock_at(today);
        let manager = test_manager(&pool, &data_dir, clock.clone());
        // 設定テーブルを壊して読み取りを失敗させる。
        sqlx::query("DROP TABLE settings")
            .execute(&pool)
            .await
            .expect("drop settings");
        assert!(
            settings.store_config().await.is_err(),
            "読み取りが失敗する前提"
        );

        prune_once(&settings, &manager, clock.as_ref()).await;

        let remaining = banto_tstore::list_data_files(&data_dir).expect("list_data_files");
        assert_eq!(
            remaining.len(),
            1,
            "settings read failure must not fall back to the default retention"
        );
    }
}
