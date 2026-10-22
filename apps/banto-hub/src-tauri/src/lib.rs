//! banto-hub-shell — T16-0（docs/banto-hub-t16-design.md §4「T16-0 設計」）の
//! 薄いデスクトップシェル。
//!
//! ## この crate がやること・やらないこと
//!
//! [`banto_hub_core::runtime::HubRuntime`] をそのまま埋め込み、起動できたら
//! メインウィンドウを Hub 自身が配信する `http://127.0.0.1:{port}/`
//! （axum が同一 origin で UI + `/api/*` を配信する - 設計 §4「P3」）へ
//! `navigate` するだけの composition root。**独自の `frontendDist`/`invoke`
//! コマンドは一切持たない** - タグ CRUD・収集開始/停止などの「運転 API」は
//! Hub の REST/WS を管理 UI (SvelteKit) 側から叩くものであり、この crate が
//! 二重実装することはない（実装指示「二重 UI / invoke による運転 API は
//! 作らない」、`docs/tag-server-design.md` §3.1 T0 の 2026-08-09 追記
//! 「フル Tauri アプリ（独自 frontendDist + invoke 面）にはしない」）。
//!
//! ヘッドレス bin `banto-hub`（`apps/banto-hub/host/src/bin/banto-hub.rs`）
//! が一次形態であることは変わらない - このシェルは同じ
//! [`banto_hub_core::runtime::HubRuntime`] を薄く包む二次ホストにすぎない
//! （設計 §2「T0 再解釈」）。
//!
//! ## 起動シーケンス（T16-2 で更新、[`decide_startup`] が判定する全体）
//!
//! ```text
//! setup:
//!   #[cfg(windows)] WindowsServiceManager::query_status()
//!     Running かつ HttpHubHealthProbe が Healthy
//!       -> HubRuntime::start をスキップし、メインウィンドウを
//!          サービスの localhost URL へ navigate（ShellView::Service）
//!          トレイ: 「サービス: Running」・画面を開く・
//!          (Operators のみ)サービスを停止・管理画面を終了
//!     Running だが Healthy でない（Unreachable/PortConflict/
//!     WrongProfileOrVersion/MutexOwnerUnknown）
//!       -> HubRuntime::start は試みない（サービスが port を握っている
//!          可能性が高いため）- fallback 画面（ShellView::Fallback）
//!     Stopped/NotInstalled
//!       -> HubRuntime::start をデスクトップホストとして試みる（従来どおり）
//!     StartPending/StopPending/Other（遷移中）
//!       -> HubRuntime::start は試みず fallback 画面（「再試行」で様子見）
//!   非 Windows: SCM 判定自体をスキップし、常にデスクトップホストとして
//!     HubRuntime::start を試みる（従来どおり）
//!   HubRuntime::start 失敗（デスクトップ試行時）
//!     -> fallback 画面（ShellView::Fallback、SCM 状態 + health 診断 +
//!        起動エラー文言）
//!   トレイ: 状態ラベル・「画面を開く」・(収集中のみ)「収集を停止」・
//!          「アプリを終了」（デスクトップホスト時、T16-1、design §3 /
//!          desktop-plan §9.9）
//!   CollectionController::subscribe_status() の変化を tray tooltip/menu へ
//!   反映（デスクトップホスト時のみ）
//!   CloseRequested (×) -> prevent_close + hide（トレイへ格納）+ 初回だけ通知
//! トレイ「管理画面を終了」/「終了」 -> 確認ダイアログ -> exit_cleanup
//!          （デスクトップホストなら RunningHub::shutdown()、予算 15 秒）
//!          -> app.exit(0)
//! RunEvent::Exit（app.exit の後・OS のサインアウト/シャットダウン）
//!          -> exit_cleanup（予算 5 秒。トレイの終了と合わせてプロセスで
//!          1 回だけ走り、2 つ目は先行の完了を待つ - #586）
//! トレイ「収集を停止」（デスクトップホスト時） -> controller.stop().await
//! トレイ「サービスを停止」（サービス接続時、Operators/管理者のみ）
//!          -> [`run_host_switch`]（`HostKind::Service` を起点に
//!          [`banto_hub_core::host_switch::HostSwitchEngine`]で
//!          `SwitchCommand::SwitchToDesktop`を駆動 - T16-2 第二スライス、
//!          このモジュール doc「T16-2 第二スライスで追加したもの」節）。
//!          バックグラウンドスレッドで SCM `stop()`発行 →
//!          `TransitionHandle`相当のポーリングで`Stopped`到達 → 旧 health
//!          （probe）が`Unreachable`になるまで待ってから、初めて
//!          `HubRuntime::start`でデスクトップホストとしての起動を試みる
//!          （「サービスを停止したらこのアプリが代わりに運転する」という
//!          帰結自体は第一スライスと同じだが、安全に引き継げると確認して
//!          からにした）。専用の切替ウィザードは実装しない。
//! トレイ「サービスを開始」（fallback 画面、Operators/管理者のみ）
//!          -> [`run_host_switch`]（`HostKind::Offline`を起点に
//!          `SwitchCommand::SwitchToService`を駆動）。SCM `start()`発行 →
//!          `Running`到達 → health `Healthy`到達まで待ってから接続する。
//! トレイ「再試行」（fallback 画面） -> 起動判定をやり直すだけ
//!          （engine を使わない単純な再評価 - [`decide_startup`]）
//! 第二インスタンス起動 -> 既存ウィンドウを show/unminimize/set_focus、自身は終了
//! ```
//!
//! `crates/banto-collect`本体・収集の開始/停止・タグ CRUD 等の実処理は一切
//! ここに書かない - [`banto_hub_core::runtime::HubRuntime`] に完全に委譲する
//! （このモジュール自身が新規実装するのは「どのホストで、何を起動/終了の
//! トリガーにするか」という composition だけ - `apps/banto-hub/host/src/bin/
//! banto-hub.rs`のモジュール doc と同じ役割分担）。「収集を停止」も
//! [`banto_hub_core::controller::CollectionController::stop`] を直接呼ぶだけで、
//! Hub REST の運転 API を再実装するものではない。サービスの起動/停止も
//! [`banto_hub_core::service_manager::ServiceManager`]（T17-0）を呼ぶだけで、
//! SCM API・`windows-service`クレートを直接叩くことはない。
//!
//! ## T16-1 で追加したもの
//!
//! - [`tray_status`]: 状態 → ラベル/tooltip/メニュー構成の純粋関数（テスト対象）。
//! - トレイの状態購読ループ（[`watch_collection_status`]）: `AppState` が持つ
//!   [`banto_hub_core::controller::CollectionController`] の
//!   `subscribe_status()` を購読し、変化のたびに tray tooltip とメニューを
//!   再構築する。
//! - トレイ「アプリを終了」の確認ダイアログ（[`confirm_quit`]、
//!   `tauri-plugin-dialog`）。T16-0 は確認なしだった。
//! - `×` でトレイ格納した初回だけの OS 通知（[`TRAY_HINT`]、
//!   `tauri-plugin-notification`）。既読フラグは `app_data_dir` 配下のファイルへ
//!   永続化する（実装指示「できれば永続化」）。
//!
//! #392 A2 で、窓の前面化・× でトレイへ格納・初回通知・終了の確認ダイアログの
//! 呼び出しは、ChronoGazer と共有する `banto-desktop-tray`（`crates/`）へ
//! 移した（振る舞いは変えていない）。トレイのメニュー・状態の文言・終了処理の
//! 中身はこの crate に残る。
//!
//! ## T16-2 で追加したもの（docs/banto-hub-t16-design.md §3「T16-2」）
//!
//! - [`decide_startup`]: `#[cfg(windows)]`で SCM（`BantoHub`サービス）の状態を
//!   [`banto_hub_core::service_manager::WindowsServiceManager`]で問い合わせ、
//!   `Running`なら[`banto_hub_core::http_hub_health::HttpHubHealthProbe`]
//!   （実装指示 3.「T17-3 で deferred だった実 HTTP probe」）で health を
//!   確認する。Healthy ならデスクトップ Hub を起動せずサービスへ接続する
//!   （実装指示 1.「service Running なら Desktop Collector を起動しない」）。
//! - [`ShellView`]/[`FallbackInfo`]: 現在シェルが表示している見え方
//!   （デスクトップホスト／サービス接続／fallback）とその診断情報。
//! - fallback 画面（[`render_fallback`]、`ui/index.html`の`#banto-hub-status`
//!   をプレースホルダから拡張）: SCM 状態・health 診断・Operators 可否を
//!   表示する（実装指示 2.）。ボタン等の対話要素は webview 側には置かず、
//!   トレイメニュー（「サービスを開始」「再試行」「終了」）から操作する -
//!   `invoke`面を新設しない（このモジュール doc冒頭の「二重 UI を作らない」
//!   方針と同じ理由でこのスライスでは最小構成にした）。
//! - [`tray_status::show_start_service_action`]/[`tray_status::show_stop_service_action`]:
//!   `is_current_process_operator`（T17-2、実装指示 4.）の結果で
//!   サービス操作系トレイ項目の表示可否を決める - `can_operate_service`は
//!   起動時に一度確定する（[`AppState::can_operate_service`]）。
//! - 非 Windows: SCM 判定自体をコンパイル対象から外す
//!   （`#[cfg(windows)]`）- 実装指示「Non-Windows: keep current Desktop-only
//!   path」どおり、[`decide_startup`]は常にデスクトップホストとして
//!   `HubRuntime::start`を試みる。
//!
//! これらは全てデスクトップホスト（アプリがランタイムを所有する場合）専用。
//! サービス接続時のホスト表示・fallback メニューは T16-2 の対象。
//!
//! ## T16-2 第二スライスで追加したもの（docs/banto-hub-t16-design.md §5
//! 「T16-2 第一スライスの既知の gap」への対応）
//!
//! 第一スライスの`decide_startup`（初回起動判定の決定木）自体は変えず、
//! トレイからの明示的な切替操作（「サービスを開始/停止」）だけを
//! 安全な完了待ちに置き換えた。
//!
//! - [`run_host_switch`]/[`drive_host_switch`]/[`apply_host_switch_outcome`]:
//!   トレイ「サービスを開始/停止」の実処理を、発行のみの fire-and-forget
//!   から[`banto_hub_core::host_switch::HostSwitchEngine`]（T17-3）による
//!   完了待ちへ置き換えた。フルの切替ウィザード UI は依然として作らない
//!   （第一スライスと同じ方針、実装指示「without rewriting the whole
//!   first-slice decision tree」）が、トレイ操作という単一の入り口に限り
//!   engine の不変条件（`ServiceManager::start`/`stop`後に
//!   `TransitionHandle`相当のポーリングで settled を確認する・Desktop 起動
//!   前に SCM `Stopped`**かつ**health`Unreachable`を確認する）をそのまま
//!   再利用する。バックグラウンドスレッド
//!   （`std::thread::spawn`、`std::thread::sleep`ベースの
//!   `HostSwitchEngine::step(SwitchCommand::Poll)`ループ - トレイクリックの
//!   応答性を保つため、呼び出し元のスレッドはブロックしない）で完了まで
//!   進め、終わったら[`apply_startup_outcome`]で結果を画面・トレイへ反映する
//!   （[`retry_startup`]と共通の[`apply_outcome_and_sync`]経由）。
//! - [`ShellDesktopControl`][]: `HostSwitchEngine`が要求する
//!   [`banto_hub_core::host_switch::DesktopHostControl`]の実装。
//!   Desktop→Service では`request_stop`がこのプロセスの`RunningHub`を
//!   `shutdown`し、`is_stopped`は hub 未保有かつ進行中の起動・停止が無い
//!   ときに`true`を返す（#586 で「未保有」だけから変えた - [`hub_slot`]）
//!   （切替ウィザード UI / `switch_to_service` invoke がこの経路を使う）。
//! - [`host_switch_ipc`]: Hub 管理 UI 向けの最小 invoke
//!   （`host_switch_status` / `switch_to_service` / `switch_to_desktop` /
//!   `set_service_autostart`）と`host_switch_progress`イベント。
//!   **運転 API（タグ CRUD・収集開始/停止）の invoke は作らない** -
//!   シェル composition（SCM／切替／昇格）だけを許可する。
//! - navigate/probe 先ホストの解決（[`resolve_navigate_host`]、既知の gap
//!   「navigate 先を`127.0.0.1`固定にしている」への対応）:
//!   `BANTO_BIND`（`console`/`service`ホストと同じ env）が設定されていれば
//!   それを使う。ただし空文字列・`0.0.0.0`・`::`（全インターフェース bind）は
//!   このプロセス自身が接続する用途では意味を持たないため、loopback
//!   （`127.0.0.1`）へ読み替える。[`ProbeTarget`]がこの解決結果を保持し、
//!   [`decide_startup`]・[`attempt_desktop_start`]・[`run_host_switch`]が
//!   共通で使う。
//! - Operators に加え Windows ローカル Administrators グループのメンバーも
//!   サービス操作系トレイ項目を表示できるようにした
//!   （[`banto_hub_core::service_operators::is_current_process_admin`]、
//!   既知の gap「Operators ゲートがローカル管理者を誤って隠す」への対応 -
//!   desktop-plan §8.3「Hub Admin と Windows の専用ローカルグループ
//!   `BantoHub Operators`（**または Windows 管理者**）」の意図に合わせた）。
//!   `AppState::can_operate_service`は起動時に両方を1回ずつ確認した
//!   論理和になる。
//!
//! ## T16 実機検証で発覚した不具合の修正（2026-08-30）
//!
//! 実機（Windows）で切替ウィザード UI がまったく動作しない不具合があった。
//! 開発者ツールに `event.listen not allowed on window "main" ... URL:
//! http://127.0.0.1:8722/ allowed on: [windows: "main", URL: local]`
//! というエラーが出ており、原因は
//! `capabilities/default.json` に Tauri v2 の `remote.urls` 設定が無く、
//! [`navigate_to_hub`]で読み込む自プロセス配信 origin（`URL: local`＝
//! ローカルアセット由来ではない）からの `invoke`/`event.listen` が
//! ケイパビリティの既定（ローカルアセット origin のみ許可）で拒否されて
//! いたこと。[`host_switch_ipc`]の4コマンドが呼べず、`/status`の
//! 「Windows サービス」カードが「シェル状態を読み込み中…」のまま
//! 進まなくなっていた。`capabilities/default.json`に
//! `remote.urls: ["http://127.0.0.1:*/*"]`を追加して解消した - この
//! crate は`resolve_navigate_host`/`navigate_to_hub`の実装どおり自プロセス
//! 内`HubRuntime`（既定`127.0.0.1`、`BANTO_BIND`が空/`0.0.0.0`/`::`なら
//! 同じくloopbackへ読み替える）以外へ navigate する経路を持たないため、
//! 許可 origin をこの loopback に限定できる（詳細な安全性の論拠は
//! `capabilities/default.json`の`description`に記載）。ポートは`PORT`
//! env で可変なためワイルドカードにしたが、ホストは`127.0.0.1`固定のまま
//! 広げていない。
//!
//! ## OS の終了で収集を止める（#586、2026-10-12）
//!
//! 以前は `.run(tauri::generate_context!())` で起動して `RunEvent::Exit` を
//! 扱っておらず、OS のサインアウト・シャットダウンでは `RunningHub::shutdown`
//! （収集の停止・最終 flush）を通らずに落ちていた。`.build(..).run(..)` に
//! して、`Exit` から [`exit_cleanup`] を通す。トレイの「アプリを終了」
//! （[`quit`]）も同じ [`exit_cleanup`] を通り、後始末はプロセスで 1 回だけ
//! 走って 2 つ目は先行の完了を期限まで待つ（共有 crate の
//! `banto_desktop_tray::ExitCleanupOnce`、ChronoGazer の #585 と同じ型）。
//! ホスト切替との競合（切替が Hub を取り出して止めている最中・新しい Hub を
//! 起動している最中の終了）は、Hub の置き場所 [`hub_slot::HubSlot`] が
//! 「進行中の起動・停止」と「閉じた（終了が始まった）」印を持つことで扱う。
//!
//! ## profile の形を見てから Desktop を許す（#661 661-5、2026-10-20）
//!
//! - [`decide_startup`] は Desktop になる組合せ（Stopped・NotInstalled・照会の失敗）で
//!   `HubRuntime::start` の前に profile の形を見て、運用モードの形なら Fallback の窓
//!   （「運用モード（停止中）」・終了コード 5 なら「運用モードのまま残っています」）にする
//!   （判定は [`launch_gate`] の純関数）。起動・再試行・切り替えの後・「設定モードに戻す」 の
//!   後のどれもここを通る。
//! - 「設定モードに戻す」（トレイ・invoke `restore_setting_mode`）は [`run_restore_setting`]:
//!   `relax-only` → 戻らなければ `banto-hub-elev.exe grant-profile-acl`（UAC）→ 同じ判定。
//! - サービスの見張り（[`spawn_service_watch`]、Windows）: 接続中・Fallback の間に止まったら
//!   終了コード 4・5 と形で窓を選ぶ（Hub は起動しない）。
//! - デスクトップのログ（`logs\banto-hub-desktop-YYYY-MM-DD.log`）: 設定モードへの切り替えの
//!   開始・完了と 5 の案内を、起きた時刻で（[`AppState::pending_log`] に覚えて、Hub が動き
//!   始めてから書く）。

use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use banto_desktop_tray::{
    ConfirmPrompt, ExitCleanupOnce, ExitCleanupOutcome, ExitCleanupReport, QuitAdmission,
    QuitAnswerDecision, QuitGate, QuitRequestDecision, TrayHint,
};
use banto_hub_core::broker_glue::HubDrivers;
use banto_hub_core::controller::{CollectionController, RuntimeStatus};
#[cfg(windows)]
use banto_hub_core::host_switch::{
    relax_only_and_wait, DesktopHostControl, DesktopHostError, HostKind, HostSwitchConfig,
    HostSwitchEngine, RealClock, ServiceFault, StepOutcome, SwitchCommand, SwitchError,
    SwitchPhase, SWITCH_TIMINGS,
};
use banto_hub_core::http_hub_health::HttpHubHealthProbe;
use banto_hub_core::hub_health::{HealthOutcome, HubHealthProbe};
use banto_hub_core::hub_log::{log_err_line, log_line};
use banto_hub_core::profile_lock::HubHostKind;
use banto_hub_core::profile_paths::{build_hub_config_from_env, resolve_profile_paths_from_env};
use banto_hub_core::runtime::{HubRuntime, RunningHub};
use banto_hub_core::service_manager::ScmState;
#[cfg(windows)]
use banto_hub_core::service_manager::{hub_service_manager, ServiceManager, WindowsServiceManager};
use banto_hub_core::service_operators::{is_current_process_admin, is_current_process_operator};
use tauri::menu::{Menu, MenuBuilder, MenuItemBuilder};
use tauri::tray::{TrayIcon, TrayIconBuilder};
use tauri::webview::PageLoadEvent;
use tauri::{AppHandle, Manager, Runtime, Webview, WebviewWindow, Wry};

#[cfg(windows)]
use banto_hub_core::desktop_log;
use banto_hub_core::desktop_log::DesktopLogEntry;
use hub_slot::{run_hub_exit, HubExitReport, HubSlot, InFlight};
#[cfg(windows)]
use launch_gate::AfterRelax;
use launch_gate::{FallbackCause, ProfileModeObservation, StartupPlan};

mod host_switch_ipc;
mod hub_slot;
// 見張り・切り替え・「設定モードに戻す」 の部品は Windows だけで使う（テストはどの OS でも走る）。
#[cfg_attr(not(windows), allow(dead_code))]
mod launch_gate;
mod sink_service_ipc;
mod tray_status;

/// R1-3b: このシェルが組み立てて `HubConfig.drivers` に注入する組み込みドライバ一式。
/// コアは具体的なドライバを知らない（docs/banto-driver-api-design.md §6.4）。
fn hub_drivers() -> HubDrivers {
    let builtin = banto_drivers::builtin();
    // R2b-1a: `modbus-tcp` / `slmp`（R2b-3a-2 からセッション型）に、host が I/O を持つ `push` /
    // `postgres` / `virtual`（`HostProvided`）を足す。R2b-1d から Hub の入口・収集の計画・
    // 移行の preflight がこのレジストリを正にする（docs/banto-hub-connection-settings-design.md §4）。
    let mut registry = builtin.registry;
    banto_hub_core::register_host_provided(&mut registry)
        .expect("host-provided driver ids are distinct from the built-ins");
    HubDrivers::new(std::sync::Arc::new(registry), builtin.client_factory)
}

/// メインウィンドウのラベル - `tauri.conf.json` の `app.windows[0].label` と
/// 一致させること。
const MAIN_WINDOW_LABEL: &str = "main";

/// このシェルが現在表示している見え方（[`AppState::view`]）。
///
/// `Desktop`自体は追加のペイロードを持たない - 実際の収集状態は
/// [`AppState::controller`]の`subscribe_status()`を購読する
/// [`watch_collection_status`]が別途トレイへ反映するため、ここでは
/// 「今どのホストを見せているか」の分岐にだけ使う。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ShellView {
    /// このプロセスが保有する[`RunningHub`]を表示中。
    Desktop,
    /// Windows サービス（`BantoHub`、`Running`かつ health `Healthy`確認済み）
    /// へ接続中 - このプロセスは`RunningHub`を持たない。
    Service { scm_state: ScmState },
    /// デスクトップ起動失敗・サービス health 不良・SCM 遷移中等の fallback。
    Fallback(FallbackInfo),
}

/// fallback 画面（[`render_fallback`]・トレイ状態行）が使う診断情報。
#[derive(Debug, Clone, PartialEq, Eq)]
struct FallbackInfo {
    /// SCM に問い合わせられた場合の状態（非 Windows、または SCM 問い合わせ
    /// 自体が失敗した場合は`None`）。
    scm_state: Option<ScmState>,
    /// [`HttpHubHealthProbe::probe`]の結果（問い合わせていない場合は`None`）。
    health: Option<HealthOutcome>,
    /// `HubRuntime::start`（デスクトップ試行）が失敗した場合のエラー文言。
    desktop_error: Option<String>,
    /// #661 661-5: 窓の見出し・案内・トレイの「設定モードに戻す」を選ぶ理由
    /// （[`launch_gate::FallbackCause`]。従来の診断だけの窓は `General`）。
    cause: FallbackCause,
}

/// [`decide_startup`]の判定結果。
enum StartupOutcome {
    /// サービスへ接続する - `host`/`port`が navigate 先（実装指示 3.、
    /// [`resolve_navigate_host`]参照 - 第一スライスは`127.0.0.1`固定
    /// だった）。
    Service {
        host: String,
        port: u16,
        scm_state: ScmState,
    },
    /// デスクトップホストとして起動できた。
    Desktop(Box<StartedHub>),
    /// fallback 画面を表示する。
    Fallback(FallbackInfo),
}

/// デスクトップホストとして起動し終えた Hub と、置き場所
/// （[`AppState::hub`]）に置くまでの「進行中の起動」の札（#586、
/// [`hub_slot`] の doc）。札を持っている間は、終了がこの Hub を待つ。
struct StartedHub {
    hub: RunningHub,
    ticket: InFlight<RunningHub>,
}

/// T16-2 第二スライス: [`decide_startup`]・[`attempt_desktop_start`]・
/// [`run_host_switch`]が共通で使う「今回の起動が期待する対象」をまとめた
/// 薄い構造体。第一スライスの`expected_probe_target() -> (PathBuf, String,
/// u16)`に`host`（実装指示 3.）を足しただけで、値の意味自体は変えていない。
#[derive(Debug, Clone)]
struct ProbeTarget {
    root: PathBuf,
    profile_id: String,
    port: u16,
    /// navigate/probe 先ホスト（[`resolve_navigate_host`]が解決）。
    host: String,
    /// #661 661-5: profile のフォルダ（Desktop へ進む前に形を見る - [`launch_gate`]）。
    profile_dir: PathBuf,
    /// #661 661-5: デスクトップのログを書く profile の `logs\`（Windows だけで使う）。
    #[cfg_attr(not(windows), allow(dead_code))]
    logs_dir: PathBuf,
}

/// `app.manage()` で保持するアプリ全体の唯一の可変状態。
pub(crate) struct AppState {
    /// 稼働中の Hub の置き場所。デスクトップホストとして起動できた場合のみ
    /// Hub が置かれる（サービス接続時・fallback 時は空 - [`render_fallback`]・
    /// [`show_startup_error`]参照）。
    ///
    /// #586: 以前は `tokio::sync::Mutex<Option<RunningHub>>` で、空かどうか
    /// しか分からなかった。ホスト切替が Hub を取り出して止めている最中・
    /// 新しい Hub を起動している最中も終了から見えるように、置き場所と
    /// 「進行中の起動・停止」の数と「閉じた（終了が始まった）」印を同じロックで
    /// 持つ [`HubSlot`] にした（[`hub_slot`] の doc）。`shutdown` は Hub を
    /// 取り出してから、ロックの外で待つ。
    hub: Arc<HubSlot<RunningHub>>,
    /// T16-1: トレイの状態表示・「収集を停止」・終了確認の文言決定に使う
    /// controller ハンドル。`hub`とは別フィールドにしているのは、
    /// 終了・切替が`hub`の中身を取り出して`shutdown()`へ渡した後は参照できなく
    /// なってよいのに対し、こちらはトレイの文言・確認の受付から同期的に
    /// 読むため（`CollectionController`のメソッド自体が内部で非同期処理する
    /// ので、ここは同期ロックで足りる）。
    /// デスクトップホストとして起動できなかった場合は`None`のまま。
    controller: StdMutex<Option<Arc<CollectionController>>>,
    /// T16-2: 現在シェルが表示している見え方（[`ShellView`]）。
    /// トレイメニューの再構築・「サービスを開始/停止」「再試行」の分岐に使う。
    pub(crate) view: StdMutex<ShellView>,
    /// #706 レビュー: [`view`](Self::view) の世代。画面を書き換えるたびに、`view` のロックを
    /// 持ったまま進める（[`set_view`]）。見張りは観測した世代と適用の直前の世代を比べ、違えば
    /// 結果を捨てる（[`launch_gate::watch_result_is_fresh`]）。
    view_generation: launch_gate::ViewGeneration,
    /// T16-2: `is_current_process_operator().unwrap_or(false)`
    /// （実装指示 4.「Operators 委任」）。T16-2 第二スライスで
    /// `is_current_process_admin().unwrap_or(false)`との論理和になった
    /// （既知の gap「Operators ゲートがローカル管理者を誤って隠す」への
    /// 対応、`is_current_process_admin`のモジュール doc参照）。起動時に
    /// 一度確定し、以後は変えない - グループ参加・昇格状態の変更は
    /// プロセス再起動が前提（Windows のトークンはプロセス生存中に
    /// 再評価されないため、このスライスでは再評価の仕組みは作らない）。
    pub(crate) can_operate_service: bool,
    /// #514: プレースホルダ（`ui/index.html`）の`#banto-hub-status`に出すべき
    /// 文言（[`set_status_text`]が最後に書いたもの）。`None`は「書き込んで
    /// いない」か「Hub の画面へ navigate した後」（[`navigate_to_hub`]が消す）。
    ///
    /// [`WebviewWindow::eval`]は「その時点の文書」で一度だけ走り、失敗しても
    /// Err にならない。起動判定（`setup`内）はプレースホルダの読み込みと
    /// 並行して走るため、読み込みの前（`document.readyState`が`loading`で
    /// 要素がまだ無い時点）に eval が走ると文言が捨てられ、「起動中…」のまま
    /// 残っていた（#514、実機の計測で確認）。そこで文言をここに持っておき、
    /// ページの読み込みが終わるたびに[`reapply_status_text`]が書き直す -
    /// eval が読み込みの前でも後でも、ページを再読み込みしても文言が残る。
    /// #706 レビュー: 文言と一緒に、Fallback の窓の操作のボタンも持つ（[`PlaceholderPage`]）。
    status_text: StdMutex<Option<PlaceholderPage>>,
    /// #661 661-5: プレースホルダ（`ui/index.html`）の URL（起動時の窓の URL）。Hub の画面
    /// （Service・Desktop）を出している窓を Fallback の画面へ移すときの navigate 先
    /// （[`apply_startup_outcome`]）。
    placeholder_url: StdMutex<Option<tauri::Url>>,
    /// #661 661-5: デスクトップのログに書く行（起きた時刻つき）。運用モードの間は profile の
    /// `logs\` に書けないので覚えておき、このアプリの Hub が動き始めてから書く
    /// （[`flush_desktop_log`]）。
    #[cfg_attr(not(windows), allow(dead_code))]
    pending_log: StdMutex<Vec<DesktopLogEntry>>,
}

/// JS 文字列リテラルへの最小エスケープ。[`show_startup_error`]・
/// [`render_fallback`] が プレースホルダ (`ui/index.html`) へ
/// [`WebviewWindow::eval`] でエラー文言を書き込むためだけに使う小さな
/// ヘルパー（#706 レビューで操作のボタンを渡すために `serde_json` を通常の依存にしたが、
/// 文言はこれまでどおりこの関数で埋め込む）。埋め込む文字列は `HubStartError`
/// （`thiserror` 由来の日本語定型文）や[`tray_status`]の純粋関数が返す
/// 固定文言由来で、任意の外部入力（HTML/JS インジェクション経路）を
/// 埋め込むことはない。
fn js_string_literal(value: &str) -> String {
    let escaped = value
        .replace('\\', "\\\\")
        .replace('\'', "\\'")
        .replace('\r', "")
        .replace('\n', "\\n");
    format!("'{escaped}'")
}

/// `#banto-hub-status`の文言を`message`にする JS を組み立てる純粋関数
/// （#514、Tauri の型に依存しないため単体テストできる）。
///
/// 文書の読み込み中（`document.readyState === 'loading'`、要素がまだ無い
/// ことがある）なら`DOMContentLoaded`を待ってから書き込み、それ以外なら
/// すぐ書き込む。要素が無い文書（navigate した後の Hub の画面など）では
/// 何もしない - 例外を投げない。
///
/// #706 レビュー: 操作のボタン（`actions`）も渡す。プレースホルダの
/// `window.__bantoHubRenderActions` が描く（Hub の画面には無いので何もしない）。操作は
/// [`launch_gate::PageAction`] の固定の値だけで、JSON にして埋め込む。
fn status_text_script(page: &PlaceholderPage) -> String {
    let actions = serde_json::to_string(&page.actions).expect("PageAction serializes");
    format!(
        "(function () {{var message = {};var actions = {actions};function apply() {{var el = document.getElementById('banto-hub-status');if (el) {{ el.textContent = message; }}if (typeof window.__bantoHubRenderActions === 'function') {{ window.__bantoHubRenderActions(actions); }}}}if (document.readyState === 'loading') {{document.addEventListener('DOMContentLoaded', apply, {{ once: true }});}} else {{ apply(); }}}})();",
        js_string_literal(&page.message)
    )
}

/// プレースホルダ（`ui/index.html`）に出す中身（#706 レビュー）: 文言と操作のボタン。
#[derive(Debug, Clone, PartialEq, Eq)]
struct PlaceholderPage {
    message: String,
    actions: Vec<launch_gate::PageAction>,
}

/// `#banto-hub-status`の文言を書き換える共通ヘルパー（[`show_startup_error`]・
/// [`render_fallback`]の重複を避ける）。
///
/// #514: 文言を[`AppState::status_text`]に持ってから eval する。eval が
/// プレースホルダの読み込みの前に走って捨てられても、読み込みの完了で
/// [`reapply_status_text`]が書き直す（順序はどちらが先でもよい: 先に
/// 文言を保存するので、読み込みの完了がこの保存より前なら eval の時点で
/// 文書は読み込み済み、後なら完了時の書き直しが保存済みの文言を読む）。
fn set_status_text(window: &WebviewWindow, message: &str) {
    set_placeholder_page(
        window,
        PlaceholderPage {
            message: message.to_string(),
            actions: Vec::new(),
        },
    );
}

/// [`set_status_text`] の、操作のボタンも出す形（#706 レビュー。Fallback の窓 -
/// [`render_fallback`]）。
fn set_placeholder_page(window: &WebviewWindow, page: PlaceholderPage) {
    let script = status_text_script(&page);
    if let Some(state) = window.try_state::<AppState>() {
        *state
            .status_text
            .lock()
            .expect("status_text mutex poisoned") = Some(page);
    }
    if let Err(err) = window.eval(script) {
        report_shell_error(
            window.app_handle(),
            format!("画面表示の更新に失敗しました: {err}"),
        );
    }
}

/// シェルの失敗の行（#706 実機確認）。release は GUI のサブシステム（`windows_subsystem =
/// "windows"`）で標準エラー出力がどこにも残らないので、Windows ではデスクトップのログにも残す
/// （[`push_desktop_log`]。運用モードの間は profile に書けないので覚えておき、設定モードに
/// 戻ってから [`flush_desktop_log`] が書く）。
fn report_shell_error<R: Runtime>(app: &AppHandle<R>, text: impl Into<String>) {
    #[cfg(windows)]
    push_desktop_log(app, text);
    #[cfg(not(windows))]
    {
        let _ = app;
        log_err_line(&format!("banto-hub-shell: {}", text.into()));
    }
}

/// #514: メインウィンドウのページの読み込みが終わるたびに（`Builder::on_page_load`、
/// [`PageLoadEvent::Finished`]）、[`AppState::status_text`]に持っている
/// 文言を書き直す。初回の読み込みより前に[`set_status_text`]の eval が
/// 走って捨てられた場合と、プレースホルダを再読み込みした場合の両方を
/// 拾う。文言が無い（Hub の画面へ navigate した後など）ときは何もしない。
///
/// #706 実機確認: 読み込んだのがプレースホルダなら、その URL を navigate 先として覚え直す
/// （[`AppState::placeholder_url`]。`useHttpsScheme` などで起動時に組み立てた URL と違っても、
/// 実際に読めた URL を使う）。
fn reapply_status_text<R: Runtime>(webview: &Webview<R>, url: &tauri::Url, event: PageLoadEvent) {
    if event != PageLoadEvent::Finished || webview.label() != MAIN_WINDOW_LABEL {
        return;
    }
    let Some(state) = webview.try_state::<AppState>() else {
        return;
    };
    if launch_gate::is_placeholder_url(url) {
        *state
            .placeholder_url
            .lock()
            .expect("placeholder_url mutex poisoned") = Some(url.clone());
    }
    let page = state
        .status_text
        .lock()
        .expect("status_text mutex poisoned")
        .clone();
    if let Some(page) = page {
        if let Err(err) = webview.eval(status_text_script(&page)) {
            report_shell_error(
                webview.app_handle(),
                format!("画面表示の更新に失敗しました: {err}"),
            );
        }
    }
}

/// 起動失敗時の表示（実装指示「起動失敗」節: 「サービス接続へ逃がさず、
/// エラーをログ/ダイアログ相当で示して終了操作だけ提供する」）。
///
/// [`decide_startup`]/[`retry_startup`]が想定していない例外的な失敗
/// （URL 組み立て失敗等、[`FallbackInfo`]を経由しない即時エラー表示）用 -
/// 通常の SCM/health 判定に基づく fallback は[`render_fallback`]を使う。
fn show_startup_error(window: &WebviewWindow, message: &str) {
    report_shell_error(window.app_handle(), message);
    let display =
        format!("起動できませんでした。\n{message}\n\nトレイメニューから終了してください。");
    set_status_text(window, &display);
}

/// T16-2 fallback 画面の本文（実装指示 2.「SCM 状態、service start/stop
/// actions gated by Operators membership」）を組み立てる純粋関数 - Tauri の
/// 型に依存しないため単体テストできる（[`tray_status`]と同じ方針）。
fn fallback_message(info: &FallbackInfo, can_operate: bool) -> String {
    let mut lines = Vec::new();
    // #661 661-5: 理由のある窓は、見出しと案内を先に出す（診断の行はその後）。
    if let Some((title, steps)) = launch_gate::cause_page(info.cause) {
        lines.push(title.to_string());
        lines.extend(steps.iter().map(|step| step.to_string()));
        lines.push(String::new());
    }
    lines.push(match &info.scm_state {
        Some(state) => format!("サービス状態: {state}"),
        None => "サービス状態: 確認できませんでした".to_string(),
    });
    if let Some(health) = &info.health {
        lines.push(format!(
            "管理画面: {}",
            tray_status::describe_health_outcome(health)
        ));
    }
    if let Some(err) = &info.desktop_error {
        lines.push(format!("このアプリでの起動に失敗しました: {err}"));
    }
    lines.push(if can_operate {
        "下のボタンかタスクトレイからサービスの開始・停止や再試行を操作できます。".to_string()
    } else {
        "サービス操作には 'BantoHub Operators' グループへの参加が必要です。下のボタンかトレイの「再試行」で状態を再確認できます。".to_string()
    });
    lines.join("\n")
}

/// [`decide_startup`]が`StartupOutcome::Fallback`を返したときに呼ぶ表示更新。
fn render_fallback(window: &WebviewWindow, info: &FallbackInfo, can_operate: bool) {
    set_placeholder_page(window, fallback_page(info, can_operate));
}

/// Fallback の窓の中身（純関数。#706 レビュー）: 文言（[`fallback_message`]）と、理由に合う
/// 操作（トレイと同じ集合 - [`launch_gate::fallback_actions`]・[`build_fallback_tray_menu`]）。
fn fallback_page(info: &FallbackInfo, can_operate: bool) -> PlaceholderPage {
    PlaceholderPage {
        message: fallback_message(info, can_operate),
        actions: fallback_actions(info, can_operate),
    }
}

/// Fallback の窓とトレイの操作（[`launch_gate::fallback_actions`] に開始の条件を渡す）。
fn fallback_actions(info: &FallbackInfo, can_operate: bool) -> Vec<launch_gate::PageAction> {
    launch_gate::fallback_actions(
        info.cause,
        tray_status::show_start_service_action(
            info.scm_state.as_ref(),
            info.health.as_ref(),
            can_operate,
        ),
    )
}

/// [`HubRuntime::start`] 成功後、プレースホルダから Hub 自身の管理画面
/// （axum が同一 origin で配信する UI、設計 §4「P3」）へ navigate する。
/// `frontendDist`（`ui/index.html`）の出番はここまで - 二重配布は行わない。
/// T16-2: サービス接続時（[`StartupOutcome::Service`]）の navigate にも
/// 共用する - 接続先が自プロセスの`RunningHub`かサービスかは呼び出し元が
/// 区別するだけで、navigate 自体の処理は同じ。`host_port`は呼び出し元が
/// 組み立てた`host:port`文字列（Desktop は`RunningHub::local_addr()`の
/// `Display`、Service は[`format_host_port`] - 実装指示 3.「navigate 先を
/// 127.0.0.1 固定にしない」）。
fn navigate_to_hub(window: &WebviewWindow, host_port: &str) {
    let url = match tauri::Url::parse(&format!("http://{host_port}/")) {
        Ok(url) => url,
        Err(err) => {
            // `host_port` は `RunningHub::local_addr()` またはサービスの
            // 期待ポート・[`resolve_navigate_host`]から組み立てた文字列
            // なので、パース失敗は事実上起こり得ない（`BANTO_BIND`に URL
            // として不正な値を設定した場合を除く）- 起きてもプロセスを
            // 落とさず診断だけ表示する。
            show_startup_error(
                window,
                &format!("Hub の URL の組み立てに失敗しました: {err}"),
            );
            return;
        }
    };
    // #514: Hub の画面へ移るので、プレースホルダ用の文言はもう書き直さない
    // （navigate に失敗したら下の`show_startup_error`がまた保存する）。
    if let Some(state) = window.try_state::<AppState>() {
        *state
            .status_text
            .lock()
            .expect("status_text mutex poisoned") = None;
    }
    if let Err(err) = window.navigate(url) {
        show_startup_error(
            window,
            &format!("管理画面への画面遷移に失敗しました: {err}"),
        );
    }
}

/// トレイ「画面を開く」・第二インスタンス起動時の前面化で共有する処理
/// （設計 §4.2/§4.3）。中身は ChronoGazer と共有（`banto-desktop-tray`）。
fn show_main_window(app: &AppHandle) {
    banto_desktop_tray::show_window(app, &[MAIN_WINDOW_LABEL]);
}

/// トレイ「アプリを終了」の実処理（確認ダイアログ通過後に [`confirm_quit`]
/// から呼ばれる - T16-0 時点はここが確認なしで直接メニューへ繋がっていた）。
///
/// 終了の後始末（[`exit_cleanup`]）を [`TRAY_QUIT_BUDGET`] の範囲で待って
/// から `app.exit(0)` する（実装指示「Exit / トレイ終了 ->
/// RunningHub::shutdown() -> app.exit」- 収集停止・tstore flush・broker
/// セッション切断は `RunningHub::shutdown`/`CollectionController` 側の既存
/// 責務で、ここで個別に呼び直すことはしない）。後始末は `RunEvent::Exit`
/// （OS のサインアウト・シャットダウンでも来る）と共有で、プロセスで 1 回しか
/// 走らない（#586）。デスクトップホストとして起動できていない場合（サービス
/// 接続中・fallback 中）は止める Hub が無いので、そのままプロセスを終了する -
/// **サービス自体は停止しない**（実装指示の対象外、サービスは Windows が
/// 管理する独立プロセス。[`exit_cleanup`] は SCM を呼ばない）。
fn quit(app: &AppHandle) {
    exit_cleanup(app, ExitTrigger::TrayQuit);
    app.exit(0);
}

/// 終了の後始末に入った経路（記録の文言と予算を選ぶため）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExitTrigger {
    /// トレイの「アプリを終了」（確認の後）。
    TrayQuit,
    /// `tauri::RunEvent::Exit`。トレイの終了の `app.exit(0)` の後にも来るほか、
    /// OS のサインアウト・シャットダウンで来る（tao が `WM_ENDSESSION` で
    /// イベントループを畳み、戻った直後に `std::process::exit` する）。
    RunEventExit,
}

impl ExitTrigger {
    fn label(self) -> &'static str {
        match self {
            ExitTrigger::TrayQuit => "トレイの終了",
            ExitTrigger::RunEventExit => {
                "アプリの終了イベント（OS のサインアウト・シャットダウンを含む）"
            }
        }
    }

    /// この経路が後始末を待つ上限（[`TRAY_QUIT_BUDGET`]・[`OS_EXIT_BUDGET`]）。
    fn budget(self) -> Duration {
        match self {
            ExitTrigger::TrayQuit => TRAY_QUIT_BUDGET,
            ExitTrigger::RunEventExit => OS_EXIT_BUDGET,
        }
    }
}

/// トレイの「アプリを終了」が後始末を待つ上限（#586）。
///
/// 以前（#585 まで）は上限なしで `RunningHub::shutdown` を待っていた。
/// 確認ダイアログで「停止して flush してから終了」に同意した利用者が待つ
/// 経路なので、OS の終了より長めに取る: `RunningHub::shutdown` は DB Source・
/// MQTT・gRPC・収集（最終 flush）・broker セッション・シミュレータ・HTTP を
/// 順に止め、外の相手（MQTT ブローカー・PLC）を待つことがある。それでも、
/// 応答の無い相手のせいで窓の無いプロセスが残り続けるよりは、上限で切って
/// 終了する。切ったことは [`exit_cleanup`] が記録する。**実機で足りないと
/// 分かったら、勝手に延ばさずオーナーに報告すること**（ChronoGazer の
/// `EXIT_CLEANUP_BUDGET` と同じ扱い）。
const TRAY_QUIT_BUDGET: Duration = Duration::from_secs(15);

/// `RunEvent::Exit`（OS のサインアウト・シャットダウン）が後始末を待つ上限（#586）。
///
/// Windows は終了の通知（`WM_ENDSESSION`）の処理におよそ 5 秒を見込み、
/// それを超えると「このアプリがシャットダウンを妨げています」を出して利用者を
/// 待たせ、最後は強制終了する。そこで ChronoGazer の `EXIT_CLEANUP_BUDGET` と
/// 同じ 5 秒にした。トレイの終了が先に始めた後始末に合流したときも、この経路は
/// 自分の 5 秒を超えて待たない（`banto_desktop_tray::ExitCleanupOnce::run_or_wait`）。
const OS_EXIT_BUDGET: Duration = Duration::from_secs(5);

/// 終了の後始末を 1 回に限るための入れ物（#586）。`run()` が manage する。
struct ExitOnce(ExitCleanupOnce<HubExitReport, ExitTrigger>);

/// 終了の後始末（#586）。トレイの「アプリを終了」（[`quit`]）と
/// `RunEvent::Exit`（[`run`]）の両方から呼ばれる。
///
/// 1. **待たずに、すぐ**: Hub の置き場所を閉じ（[`HubSlot::close`]。以後は
///    新しい Hub を起動させず、起動し終えた Hub も置かせない - ホスト切替の
///    結果の適用・「再試行」を含む）、今の controller に終了の取り消せない印
///    （`CollectionController::begin_shutdown`）を立てる（以後の収集の開始を
///    断る。`RunningHub::shutdown` も先頭で同じ印を立てるが、そこへ来るまでの
///    間も断る）。閉じた後は新しい Hub＝新しい controller が作られないので、
///    「切替で作られた新しい controller に終了の印が引き継がれない」
///    （#585 の残件）は起こらない。
/// 2. **1 回だけ、期限まで待つ**: 置かれている Hub を取り出して
///    `RunningHub::shutdown` で止め、切替が進めている停止・起動と、登録済みの
///    ホスト切替そのものが終わるのを待つ（[`run_hub_exit`]、#587）。2 つ目の呼び出し（トレイの終了の最中に来た
///    OS の終了、トレイの終了の `app.exit(0)` の後の `Exit`）は 1 つ目の完了を
///    待つ（`ExitCleanupOnce`）。待つ上限は経路ごとの予算
///    （[`ExitTrigger::budget`]）で、ロックの取得・進行中の停止の完了待ちも
///    その中に入る（本体は専用のスレッドで走り、呼び出し元は期限で戻る）。
/// 3. 結果を記録する。予算切れ（待つのをやめた・進行中のものが終わらな
///    かった）は正常終了と別の文言で `log_err_line` に出す。
///
/// **ダイアログは出さない**（OS の終了経路で利用者の応答を待たないため。
/// トレイの確認ダイアログが開いたままでも、それを待たずに進む）。
///
/// **サービスは止めない**: ホストの種類（デスクトップ／サービス接続／
/// fallback）によらず、ここは SCM を一切呼ばない。サービス接続中・fallback
/// では置き場所が空なので、止めるものは無く（切替が進行中ならそれを待つ
/// だけ）、サービスはそのまま動き続ける（既存の境界）。終了の開始後に新しい
/// 切替は始まらず、最初の SCM 操作の前に閉じられた切替は SCM を呼ばずに
/// 終わる。SCM を呼んだ後の切替は完走させ、ここがその完了を待つ
/// （[`run_host_switch`]。SCM を呼ぶのは利用者が求めた切替で、終了ではない）。
///
/// `AppState`/[`ExitOnce`] が無い（`setup` の前に落ちた）なら何もしない。
fn exit_cleanup(app: &AppHandle, trigger: ExitTrigger) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    let Some(once) = app.try_state::<ExitOnce>() else {
        return;
    };
    state.hub.close();
    if let Some(controller) = current_controller(app) {
        let _ = controller.begin_shutdown();
    }
    let slot = Arc::clone(&state.hub);
    run_exit_cleanup(
        &once.0,
        slot,
        trigger,
        trigger.budget(),
        |hub| {
            tauri::async_runtime::block_on(hub.shutdown());
        },
        log_sink(),
    );
}

/// [`exit_cleanup`] が記録する 1 行（重さつき）。
#[derive(Debug, PartialEq, Eq)]
enum ExitLogLine {
    Info(String),
    Error(String),
}

/// 記録先。本番は `hub_log`（[`log_sink`]）、テストは差し替えて数える。
type ExitSink = Arc<dyn Fn(ExitLogLine) + Send + Sync>;

fn log_sink() -> ExitSink {
    Arc::new(|line| match line {
        ExitLogLine::Info(line) => log_line(&line),
        ExitLogLine::Error(line) => log_err_line(&line),
    })
}

/// 終了の後始末を 1 回に限って走らせ、記録する（[`exit_cleanup`] の本体）。
///
/// 記録は 2 か所に分かれる（#586、オーナーレビュー P2）:
///
/// - **後始末の本体の結果**（完了・期限までに切替が落ち着かなかった）は、本体を
///   走らせた側の**ワーカーの中で、結果を公開する前に** 1 回だけ記録する
///   （[`body_log_line`]）。公開してから呼び出し元が記録する作りだと、先行より
///   先に結果を受け取った後続（OS の終了）が即座に戻り、`RunEvent::Exit` から
///   戻った直後にプロセスが終わって、先行の診断が 1 行も残らないことがある。
///   先に記録してあれば、結果を受け取った側が即戻っても診断は失われない。
///   記録は本体について 1 回だけなので、後続が受け取っただけのときは出さない
///   （二重にならない）。
/// - **呼び出し元の待ちの打ち切り**（自分の予算・先行の期限切れ）は、公開とは
///   無関係な呼び出し元自身の判断なので、各呼び出しが自分で記録する
///   （[`wait_log_line`]）。`RanWork(TimedOut)` の後に本体が裏で終わったときは、
///   ワーカーが「完了しました」を後から記録する（「待たずに終了します」と
///   矛盾しない: 前者は待ちの打ち切り、後者は本体の結果で、プロセスが先に
///   終わればどちらかだけが残る）。
fn run_exit_cleanup<H: Send + 'static>(
    once: &ExitCleanupOnce<HubExitReport, ExitTrigger>,
    slot: Arc<HubSlot<H>>,
    trigger: ExitTrigger,
    budget: Duration,
    shutdown: impl FnOnce(H) + Send + 'static,
    sink: ExitSink,
) -> ExitCleanupReport<HubExitReport, ExitTrigger> {
    let body_sink = Arc::clone(&sink);
    let report = once.run_or_wait(trigger, budget, move |deadline| {
        let result = run_hub_exit(&slot, deadline, shutdown);
        // 結果を公開する前に記録する（この関数の doc）。
        body_sink(body_log_line(trigger, &result));
        result
    });
    if let Some(line) = wait_log_line(trigger, budget, &report) {
        sink(line);
    }
    report
}

/// 後始末の本体の結果の記録（本体を走らせた経路 `origin` の名で、1 回だけ）。
fn body_log_line(origin: ExitTrigger, result: &HubExitReport) -> ExitLogLine {
    if result.settled {
        ExitLogLine::Info(format!(
            "banto-hub-shell: 終了の後始末が完了しました（{}）",
            origin.label()
        ))
    } else {
        ExitLogLine::Error(format!(
            "banto-hub-shell: 終了の後始末の期限までに、ホスト切替の停止・起動が終わりませんでした。待たずに終了します（{}）",
            origin.label()
        ))
    }
}

/// 呼び出し元の**待ち**の結果（[`ExitCleanupReport`]）から、その呼び出しが
/// 記録する文言を決める純関数（#586）。本体の結果は [`body_log_line`] が
/// ワーカーで記録済みなので、ここでは出さない:
///
/// - 本体の完了を受け取った（`RanWork(Finished)`・`ReceivedPrior`）: 何も出さない。
///   普通のトレイ終了で `app.exit(0)` から来る `RunEvent::Exit` が OS の終了の
///   文言で完了をもう 1 行出さない。
/// - 自分が本体を走らせて自分の予算で切った（`RanWork(TimedOut)`）: 自分の上限を出す。
/// - 後から来て待てなかったときは、**どちらの上限で**やめたかを区別する。先行の
///   期限が来ていた（または来た）なら自分の上限は名乗らず、先行の期限切れを
///   受け取ったと出す。自分の予算が先行の期限より短くて自分でやめたときだけ
///   「この経路の上限◯秒」と出す。
fn wait_log_line(
    trigger: ExitTrigger,
    budget: Duration,
    report: &ExitCleanupReport<HubExitReport, ExitTrigger>,
) -> Option<ExitLogLine> {
    match report {
        ExitCleanupReport::RanWork(ExitCleanupOutcome::Finished(_))
        | ExitCleanupReport::ReceivedPrior { .. } => None,
        ExitCleanupReport::RanWork(ExitCleanupOutcome::TimedOut) => {
            Some(ExitLogLine::Error(format!(
                "banto-hub-shell: 終了の後始末が期限までに終わらなかったため、待たずに終了します（この経路の上限{}秒、{}）",
                budget.as_secs(),
                trigger.label()
            )))
        }
        ExitCleanupReport::PriorExpired { origin } => Some(ExitLogLine::Error(format!(
            "banto-hub-shell: 先行の終了の後始末（{}）が期限までに終わっていなかったため、待たずに終了します（{}）",
            origin.label(),
            trigger.label()
        ))),
        ExitCleanupReport::OwnBudgetExpired { origin } => Some(ExitLogLine::Error(format!(
            "banto-hub-shell: 先行の終了の後始末（{}）がまだ終わっていませんが、この経路の上限{}秒で待つのをやめ、待たずに終了します（{}）",
            origin.label(),
            budget.as_secs(),
            trigger.label()
        ))),
    }
}

/// 今の controller（サービス接続中・fallback なら `None`）。
fn current_controller(app: &AppHandle) -> Option<Arc<CollectionController>> {
    app.state::<AppState>()
        .controller
        .lock()
        .expect("controller mutex poisoned")
        .clone()
}

/// 今、確認文を「収集を停止して終了」の側にすべき状態か
/// （[`tray_status::show_stop_item`]が真になる状態）。controller が無い
/// （サービス接続中・fallback）なら偽。**文言を選ぶためだけ**に使う -
/// 終了へ進んでよいかの判断は [`ControllerQuitAdmission`] が印を立てて
/// 読んだ状態で行う。
fn currently_collecting(app: &AppHandle) -> bool {
    current_controller(app)
        .map(|controller| tray_status::show_stop_item(&controller.status()))
        .unwrap_or(false)
}

/// トレイの「アプリを終了」の答えで [`QuitGate::answer`] に渡す、終了の受付の
/// 口（#392 A2 レビュー、2026-10-12。`banto_desktop_tray` の doc「終了の確定と
/// 収集の開始の直列化」・`banto_hub_core::controller` の doc「終了の受付」）。
///
/// [`CollectionController::begin_quit_confirm`] は、収集の開始（REST・MCP の
/// `start`/`start-all-simulation`/走行中の `mode`）が `Starting` を書くのと
/// 同じロックで印を立てて状態を読むので、「停止中の文言で同意 → 状態を
/// 読む → その直後に開始 → 古い『停止中』で終了へ進み、新しい収集を黙って
/// 止める」が起きない。
///
/// controller が無い（サービス接続中・fallback）ときは、このプロセスに
/// 止める収集が無いので常に「暇」で、印も無い（サービスの収集はサービスが
/// 持ち、このアプリの終了では止めない - [`quit`] の doc）。controller は
/// 答えを受けた時点のものを 1 つ掴み、立てた印と下ろす印が別の controller に
/// なることはない。扱うのは**確認のための一時的な印**だけで、
/// `RunningHub::shutdown` が立てる取り消せない印（`begin_shutdown`）は
/// `AskAgain` で下ろせない（#392 A2 再レビュー P2-2）。
struct ControllerQuitAdmission(Option<Arc<CollectionController>>);

impl QuitAdmission for ControllerQuitAdmission {
    fn begin_quit_confirm(&self) -> bool {
        self.0
            .as_ref()
            .map(|controller| tray_status::show_stop_item(&controller.begin_quit_confirm()))
            .unwrap_or(false)
    }

    fn cancel_quit_confirm(&self) {
        if let Some(controller) = &self.0 {
            controller.cancel_quit_confirm();
        }
    }
}

/// 「アプリを終了」の確認文。収集中かどうかで変わる。
fn quit_prompt_message(collecting: bool) -> &'static str {
    if collecting {
        "収集を停止し、履歴を flush してから終了します。よろしいですか?"
    } else {
        "Banto Hub を終了します。よろしいですか?"
    }
}

/// トレイ「アプリを終了」クリック時のエントリポイント（T16-1、実装指示
/// 3.「アプリを終了の確認」- T16-0 では確認なしだったものをここで追加）。
///
/// 収集中かどうかで文言を変える。キャンセルすれば[`quit`]は一切呼ばれない -
/// ダイアログの結果を見ずに副作用を起こさない。ダイアログは待たない版
/// （[`banto_desktop_tray::ask_confirm`]）で、メニューイベントハンドラが
/// どのスレッドで呼ばれても安全にする。
///
/// #392 A2（オーナー決定 2026-10-12）: 共有の [`QuitGate`] を通す。確認中・
/// 終了処理中にもう一度「アプリを終了」が押されても、確認ダイアログを
/// 2 つ出さず、[`quit`] を 2 回走らせない。「収集していない」文言で同意を
/// 取った後に収集が始まっていたら（LAN の REST・MCP からの開始）、
/// 黙って止めず「収集中」の文言でもう一度確かめる。その判断と終了の確定は、
/// 収集の開始と [`ControllerQuitAdmission`] で直列化している（同意の後に
/// 来た開始は 409 で断られる）。
fn confirm_quit(app: &AppHandle) {
    let Some(gate) = app.try_state::<QuitGate>() else {
        return;
    };
    match gate.request() {
        QuitRequestDecision::ShowPrompt => prompt_quit(app, currently_collecting(app)),
        QuitRequestDecision::AlreadyConfirming | QuitRequestDecision::AlreadyQuitting => {}
    }
}

/// 確認ダイアログを出す。`prompted_collecting` は文言を選んだ時点の状態
/// （終了の受付の印を立てた時点の状態と比べて[`QuitGate::answer`]が判断する）。
fn prompt_quit(app: &AppHandle, prompted_collecting: bool) {
    let app_handle = app.clone();
    banto_desktop_tray::ask_confirm(
        app,
        ConfirmPrompt {
            title: "Banto Hub を終了",
            message: quit_prompt_message(prompted_collecting),
            ok_label: "終了",
            cancel_label: "キャンセル",
        },
        move |confirmed| {
            let Some(gate) = app_handle.try_state::<QuitGate>() else {
                return;
            };
            let admission = ControllerQuitAdmission(current_controller(&app_handle));
            match gate.answer(confirmed, prompted_collecting, &admission) {
                QuitAnswerDecision::Proceed => quit(&app_handle),
                QuitAnswerDecision::AskAgain => prompt_quit(&app_handle, true),
                QuitAnswerDecision::Cancelled | QuitAnswerDecision::Ignored => {}
            }
        },
    );
}

/// トレイ「収集を停止」クリック時のエントリポイント（desktop-plan §9.9
/// 「アプリ・収集中」行）。
///
/// [`banto_hub_core::controller::CollectionController::stop`] を直接呼ぶだけで、
/// Hub REST の運転 API を invoke 経由で二重実装しない（実装指示「invoke で
/// Hub REST を二重実装」しないこと）。メニューイベントハンドラ自体は同期
/// 関数なので、`.await`を挟む`stop()`は`tauri::async_runtime::spawn`で
/// 背景タスクとして実行し、結果は`CollectionController::subscribe_status`
/// 経由の[`watch_collection_status`]がトレイへ反映する（呼び出し元へは
/// 何も返さない fire-and-forget）。
fn stop_collection(app: &AppHandle) {
    let state = app.state::<AppState>();
    let controller = state
        .controller
        .lock()
        .expect("controller mutex poisoned")
        .clone();
    let Some(controller) = controller else {
        return;
    };
    tauri::async_runtime::spawn(async move {
        controller.stop().await;
    });
}

/// T16-2: env（`BANTO_HUB_ROOT`/`BANTO_HUB_PROFILE`/`PORT`/`BANTO_BIND`）から
/// [`decide_startup`]が必要とする「期待する root・profile-id・port・host」を
/// 解決する。`build_hub_config_from_env`と同じ env を読むが、`HubConfig`
/// 全体ではなく[`ProbeTarget`]が必要とする値だけを返す薄いラッパ。
///
/// #479: ここで返す `port` は「**サービス版の Hub が待ち受ける期待ポート**」
/// （`PORT` 環境変数、無ければ既定 8722）で、サービスへの接続・health probe・
/// host_switch の判定に使う。デスクトップシェルの Hub は、`PORT` 指定が無く既定の
/// ポートが埋まっているときだけ OS 割り当てのポートへ退避する
/// （`banto_hub_core::runtime::port_fallback_applies`）ので、デスクトップが実際に
/// 待ち受けているポートとは限らない。デスクトップ側の実ポートは常に
/// `RunningHub::local_addr()` を使うこと（navigate は
/// [`apply_startup_outcome`] がそうしている）。host_switch の待ちは
/// 「サービスの」health（Service→Desktop は旧サービスの消失、Desktop→Service は
/// サービスの起動）を期待ポートで見るだけで、デスクトップの実ポートを待つ
/// ことはない（切替前にデスクトップの Hub は `shutdown` 済みでポートも解放済み）。
fn expected_probe_target() -> ProbeTarget {
    let paths = resolve_profile_paths_from_env();
    let port = std::env::var("PORT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(banto_hub_core::settings::DEFAULT_PORT);
    ProbeTarget {
        root: paths.root,
        profile_id: paths.profile_id,
        port,
        host: resolve_navigate_host(),
        profile_dir: paths.profile_dir,
        logs_dir: paths.logs_dir,
    }
}

/// T16-2 第二スライス（実装指示 3.、docs/banto-hub-t16-design.md §5 既知の
/// gap「navigate 先を`127.0.0.1`固定にしている」）: navigate/probe 先ホストを
/// `BANTO_BIND`から解決する。
///
/// console/service ホスト（`apps/banto-hub/core/src/runtime.rs::HubRuntime::start`）
/// と同じ env を読むが、あちらは「自分自身がどこへ bind するか」で
/// persisted `server_config.bind`へフォールバックできるのに対し、こちらは
/// 「まだ起動していないかもしれない別ホストへこのプロセス自身が接続しに
/// 行く」用途なので、persisted 設定は参照しない（接続先が起動していなければ
/// そもそも読めないため）。全インターフェース bind（空文字列・`0.0.0.0`・
/// IPv6 ワイルドカード`::`）は「このホスト上のどこからでも」という意味で
/// あり、同一ホスト上のこのプロセスからの接続先としては意味を持たないため、
/// loopback（`127.0.0.1`）へ読み替える（実装指示「Keep loopback-safe
/// defaults」）。
fn resolve_navigate_host() -> String {
    match std::env::var("BANTO_BIND") {
        Ok(value) if !value.is_empty() && value != "0.0.0.0" && value != "::" => value,
        _ => "127.0.0.1".to_string(),
    }
}

/// URL の authority 部分へ埋め込める`host:port`文字列を組み立てる。IPv6
/// リテラルは`[host]:port`と角括弧が必要（`std::net::SocketAddr`の
/// `Display`実装と同じ判定 - `BANTO_BIND`にIPv6アドレスを設定した場合の
/// 保険、banto-hub の現行既定は IPv4 のみだが将来の変更に備える）。
fn format_host_port(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

/// T16-2: サービス操作（`query_status`/`start`/`stop`）に使う
/// [`WindowsServiceManager`]を構築する。
///
/// `executable_path`は`set_auto_start`での再登録にしか使われず
/// （`banto_hub_core::service_manager`のモジュール doc「`set_auto_start`の
/// 制約」参照）、このスライスは`set_auto_start`を一切呼ばない - そのため
/// 実際に SCM へ登録されているサービス実行ファイルのパスと一致している
/// 必要はなく、`current_exe()`（このシェル自身のパス）を仮に渡しておく。
#[cfg(windows)]
fn build_service_manager() -> WindowsServiceManager {
    hub_service_manager(std::env::current_exe().unwrap_or_default())
}

/// T16-2 の中核 - SCM 状態 + health probe から今回の起動でどのホストに
/// 接続する/起動するかを判定する（このモジュール doc「起動シーケンス」節の
/// 決定木そのもの）。
///
/// `#[cfg(windows)]`: SCM を問い合わせる。`Running`かつ probe が
/// `Healthy`ならサービスへ接続し、`HubRuntime::start`は一切呼ばない
/// （実装指示 1.「service Running なら Desktop Collector を起動しない」）。
/// `Running`だが health が怪しい、または SCM が遷移中（StartPending/
/// StopPending/Other）の間は、サービスが port/profile lock を握っている
/// 可能性が高いため`HubRuntime::start`を試みず fallback へ回す（デスクトップ
/// とサービスが同時に同じ profile を掴もうとするのを避ける）。起動直後の
/// 判定では`HostSwitchEngine`を使わず、トレイからの開始/停止だけが
/// [`run_host_switch`]経由で engine を駆動する（T16-2 第二スライス）。
/// `Stopped`/`NotInstalled`、または SCM 問い合わせ自体が失敗した場合は
/// 従来どおりデスクトップホストとして起動を試みる。
///
/// 非 Windows: SCM という概念が無いため、常に[`attempt_desktop_start`]を
/// 呼ぶ（実装指示「Non-Windows: keep current Desktop-only path」）。
///
/// #661 661-5: Desktop になる組合せ（Stopped・NotInstalled・照会の失敗）では、
/// [`HubRuntime::start`] の前に profile の形を見る。締まっていれば（運用モードの形）
/// Desktop に進まず、Fallback の窓「運用モード（停止中）」「運用モードのまま残っています」
/// （終了コード 5）を出す（判定は [`launch_gate::plan_startup`]、純関数）。切り替え・
/// 「設定モードに戻す」の後に Desktop へ進むかも、この同じ関数で決める。
fn decide_startup(target: &ProbeTarget, slot: &Arc<HubSlot<RunningHub>>) -> StartupOutcome {
    #[cfg(windows)]
    let (scm, fault) = match build_service_manager().query_status() {
        Ok(status) => {
            // 終了コード（4・5）は停止のときだけ意味を持つ（文言を選ぶためだけに使う）。
            let fault = (status.state == ScmState::Stopped)
                .then(|| banto_hub_core::host_switch::service_fault(&status))
                .flatten();
            (Some(status.state), fault)
        }
        // SCM 自体に問い合わせられない（未インストール環境・権限不足等）
        // - 判定材料が無いだけなので、従来どおりデスクトップ起動を試みる
        // （ただし profile が締まっていれば断る - [`launch_gate::plan_startup`]）。
        Err(_err) => (None, None),
    };
    #[cfg(not(windows))]
    let (scm, fault): (Option<ScmState>, _) = (None, None);
    // #661 661-5: profile の形は Desktop になりうるときだけ観測する（Running・遷移中は
    // 見ない。観測は profile を読むだけで開く - [`launch_gate::observe_profile_mode`]）。
    let mode = match scm {
        Some(ScmState::Stopped | ScmState::NotInstalled) | None => {
            launch_gate::observe_profile_mode(&target.profile_dir)
        }
        Some(_) => ProfileModeObservation::NotObserved,
    };
    match launch_gate::plan_startup(scm.as_ref(), &mode, fault) {
        StartupPlan::ProbeService => probe_service_or_fallback(target),
        StartupPlan::TryDesktop => attempt_desktop_start(target, slot, scm),
        StartupPlan::Fallback(cause) => {
            if let Some(why) = mode.blocks_desktop() {
                log_line(&format!(
                    "banto-hub-shell: profile が運用モードの形か使えないため、このアプリでは Hub を起動しません（{why:?}・サービス: {}）",
                    scm.as_ref().map_or_else(|| "確認できませんでした".to_string(), ToString::to_string)
                ));
            }
            // StartPending/StopPending/Other: 遷移中 - デスクトップを
            // 起動せず、利用者に「再試行」で様子を見てもらう
            // （このモジュール doc「起動シーケンス」節参照）。
            StartupOutcome::Fallback(FallbackInfo {
                scm_state: scm,
                health: None,
                desktop_error: None,
                cause,
            })
        }
    }
}

/// サービスが `Running` のとき: health が `Healthy` ならサービスへ接続し、それ以外は
/// fallback（デスクトップを起動しない - サービスが port/profile lock を握っている
/// 可能性が高いため）。
fn probe_service_or_fallback(target: &ProbeTarget) -> StartupOutcome {
    let probe = HttpHubHealthProbe::with_host(target.root.clone(), target.host.clone());
    match probe.probe(&target.profile_id, target.port) {
        Ok(HealthOutcome::Healthy { .. }) => StartupOutcome::Service {
            host: target.host.clone(),
            port: target.port,
            scm_state: ScmState::Running,
        },
        Ok(other) => StartupOutcome::Fallback(FallbackInfo {
            scm_state: Some(ScmState::Running),
            health: Some(other),
            desktop_error: None,
            cause: FallbackCause::General,
        }),
        Err(err) => StartupOutcome::Fallback(FallbackInfo {
            scm_state: Some(ScmState::Running),
            health: None,
            desktop_error: Some(format!("health probe に失敗しました: {err}")),
            cause: FallbackCause::General,
        }),
    }
}

/// デスクトップホストとして[`HubRuntime::start`]を試みる（T16-0/T16-1 の
/// 従来の起動処理そのもの）。失敗した場合は診断用に同じポートへ
/// [`HttpHubHealthProbe`]を1回投げてから[`StartupOutcome::Fallback`]を返す -
/// 「ポート競合の相手が別の banto-hub インスタンスかどうか」の手がかりに
/// なる（`scm_state`の`allow(dead_code)`同様、Err時のみ使う値）。
///
/// #479: 既定ポートが埋まっているだけなら、`PORT` 指定が無い限り
/// [`HubRuntime::start`] が OS 割り当てのポートへ退避して起動するので、
/// ここには来ない（退避のログは `RunningHub::port_fallback_from` と
/// `hub_log` に出る）。`PORT` を明示した構成でそのポートが埋まっていれば、
/// 従来どおりここへ来て、同じポートへの probe で相手を診断する。
///
/// #586: 起動の前に置き場所（`slot`）から「進行中の起動」の札を取る。終了が
/// 始まっていれば（置き場所が閉じていれば）札は取れず、**起動しない**
/// （[`EXITING_MESSAGE`] の fallback を返すが、閉じた後の適用は
/// [`apply_startup_outcome`] が捨てるので画面には出ない）。札は起動し終えた
/// Hub と一緒に [`StartupOutcome::Desktop`] で返し、置き場所に置くまで持つ -
/// その間に終了が始まっても、終了はこの Hub を待つ。
#[allow(unused_variables)]
fn attempt_desktop_start(
    target: &ProbeTarget,
    slot: &Arc<HubSlot<RunningHub>>,
    scm_state: Option<ScmState>,
) -> StartupOutcome {
    let Some(ticket) = slot.begin_start() else {
        return StartupOutcome::Fallback(FallbackInfo {
            scm_state,
            health: None,
            desktop_error: Some(EXITING_MESSAGE.to_string()),
            cause: FallbackCause::General,
        });
    };
    // HubRuntime::start はここで同期的に待つ - `tauri::async_runtime`
    // が既定で保持する多重スレッド tokio ランタイム上で block_on
    // する（`apps/chronogazer/src-tauri`の`run()`が`init_db`等を
    // `tauri::async_runtime::block_on`で待つのと同じ流儀）。
    match tauri::async_runtime::block_on(HubRuntime::start(build_hub_config_from_env(
        HubHostKind::Shell,
        hub_drivers(),
    ))) {
        Ok(hub) => StartupOutcome::Desktop(Box::new(StartedHub { hub, ticket })),
        Err(err) => {
            // 起動できなかった - 札を返す（終了が待っていればここで起きる）。
            drop(ticket);
            let health = HttpHubHealthProbe::with_host(target.root.clone(), target.host.clone())
                .probe(&target.profile_id, target.port)
                .ok();
            StartupOutcome::Fallback(FallbackInfo {
                scm_state,
                health,
                desktop_error: Some(err.to_string()),
                cause: FallbackCause::General,
            })
        }
    }
}

/// [`decide_startup`]/[`retry_startup`]共通の適用処理 - `outcome`に応じて
/// メインウィンドウの表示（navigate/fallback文言）と`AppState`
/// （`hub`/`controller`/`view`）を更新する。
///
/// デスクトップ以外（サービス接続・fallback）へ切り替わる際、直前まで
/// このプロセスが`RunningHub`を保有していた場合は先に`shutdown()`する -
/// 「もう一方を起動したまま切り替わる」二重接続を避けるため。トレイからの
/// サービス開始/停止の完了待ちは[`run_host_switch`]（`HostSwitchEngine`）が
/// 担当し、ここは起動判定（[`decide_startup`]）結果の適用に留まる。
///
/// 戻り値の`(RuntimeStatus, Arc<CollectionController>)`はデスクトップへ
/// 新規遷移した場合のみ`Some` - 呼び出し元がトレイ構築・
/// [`watch_collection_status`]の起動要否を判断するために使う。
///
/// #586: 終了が始まった後（置き場所が閉じた後）は何も適用しない - 画面・
/// トレイも変えず、起動し終えた Hub があれば置かずに止める
/// （[`discard_started_hub`]）。閉じたかどうかの確認と Hub を置くことの間に
/// 終了が始まった場合も、[`HubSlot::install`] が突き返すので同じく止める。
fn apply_startup_outcome(
    app: &AppHandle,
    window: &WebviewWindow,
    outcome: StartupOutcome,
) -> Option<(RuntimeStatus, Arc<CollectionController>)> {
    let state = app.state::<AppState>();

    if state.hub.is_closed() {
        if let StartupOutcome::Desktop(started) = outcome {
            discard_started_hub(started.hub, started.ticket);
        }
        return None;
    }

    // 直前が Desktop で、今回 Desktop 以外へ切り替わる場合に備えて
    // 稼働中の hub を先に取り出して止める。取り出すのと同じロックで
    // 「進行中の停止」の札を取るので、止めている最中に終了が始まっても、
    // 終了はこの停止を待つ（#586）。
    if !matches!(outcome, StartupOutcome::Desktop(_)) {
        if let Some((hub, ticket)) = state.hub.take_for_stop() {
            tauri::async_runtime::block_on(hub.shutdown());
            *state.controller.lock().expect("controller mutex poisoned") = None;
            drop(ticket);
        }
    }

    match outcome {
        StartupOutcome::Service {
            host,
            port,
            scm_state,
        } => {
            navigate_to_hub(window, &format_host_port(&host, port));
            set_view(&state, ShellView::Service { scm_state });
            None
        }
        StartupOutcome::Desktop(started) => {
            let StartedHub { hub, ticket } = *started;
            let addr = hub.local_addr();
            let controller = hub.controller();
            // 先に置く（置けなければ画面を Hub へ移さない）。置けなかったのは
            // 終了が始まった（または別の Hub が置かれていた）ときで、起動した
            // この Hub は誰も止めないので、ここで止める（#586）。
            if let Err(rejected) = state.hub.install(hub, ticket) {
                discard_started_hub(rejected.hub, rejected.ticket);
                return None;
            }
            let status = controller.status();
            *state.controller.lock().expect("controller mutex poisoned") = Some(controller.clone());
            set_view(&state, ShellView::Desktop);
            navigate_to_hub(window, &addr.to_string());
            Some((status, controller))
        }
        StartupOutcome::Fallback(info) => {
            // 文言を先に持たせてから移る（移った先の読み込みの完了で書き直される）。
            render_fallback(window, &info, state.can_operate_service);
            let previous = match &*state.view.lock().expect("view mutex poisoned") {
                ShellView::Desktop => launch_gate::WatchedView::Desktop,
                ShellView::Service { .. } => launch_gate::WatchedView::Service,
                ShellView::Fallback(prev) => launch_gate::WatchedView::Fallback(prev.cause),
            };
            // Hub の画面を出していた窓は、プレースホルダへ戻さないと文言もボタンも見えない。
            if launch_gate::needs_placeholder_navigation(previous) {
                navigate_to_placeholder(app, window);
            }
            set_view(&state, ShellView::Fallback(info));
            None
        }
    }
}

/// 画面を書き換え、同じロックの中で世代を進める（#706 レビュー。画面を変える経路はすべてここを
/// 通る - 切り替え・再試行・「設定モードに戻す」・見張り・起動の判定）。
fn set_view(state: &AppState, view: ShellView) {
    let mut current = state.view.lock().expect("view mutex poisoned");
    *current = view;
    state.view_generation.advance();
}

/// 見張りが見る画面と世代、窓に出している SCM の状態（#706 レビュー。同じロックで読む）。
#[cfg(windows)]
fn watched_view(state: &AppState) -> (launch_gate::ViewStamp, Option<ScmState>) {
    let current = state.view.lock().expect("view mutex poisoned");
    let (view, shown) = match &*current {
        ShellView::Desktop => (launch_gate::WatchedView::Desktop, None),
        ShellView::Service { scm_state } => {
            (launch_gate::WatchedView::Service, Some(scm_state.clone()))
        }
        ShellView::Fallback(info) => (
            launch_gate::WatchedView::Fallback(info.cause),
            info.scm_state.clone(),
        ),
    };
    let stamp = launch_gate::ViewStamp {
        generation: state.view_generation.current(),
        view,
    };
    (stamp, shown)
}

/// 窓をプレースホルダ（Fallback の画面）へ戻す。URL は起動時に組み立て、プレースホルダを
/// 読み込むたびに覚え直したもの（[`launch_gate::placeholder_url`]・[`reapply_status_text`]）。
fn navigate_to_placeholder(app: &AppHandle, window: &WebviewWindow) {
    let url = app
        .state::<AppState>()
        .placeholder_url
        .lock()
        .expect("placeholder_url mutex poisoned")
        .clone();
    match url {
        Some(url) => {
            if let Err(err) = window.navigate(url.clone()) {
                report_shell_error(
                    app,
                    format!("窓をプレースホルダ（{url}）へ戻せませんでした: {err}"),
                );
            }
        }
        None => report_shell_error(app, "プレースホルダの URL が分からないため窓を移せません"),
    }
}

/// 終了が始まった後に起動し終えた Hub を、置かずに止める（#586）。札は止め
/// 終えてから返す - 終了はそれを待つ。
fn discard_started_hub(hub: RunningHub, ticket: InFlight<RunningHub>) {
    log_line("banto-hub-shell: 終了処理中のため、起動し終えた Hub を使わずに停止します");
    tauri::async_runtime::block_on(hub.shutdown());
    drop(ticket);
}

/// 終了が始まった後に起動・切替を断るときの文言（#586）。
const EXITING_MESSAGE: &str = "終了処理中のため、Hub を起動しません";

/// 終了が始まった後に切替を始めない（SCM を呼ばない）ときの文言（#586/#587）。
#[cfg(windows)]
const EXITING_SWITCH_MESSAGE: &str =
    "終了処理中のため、切替を開始しません（サービスの操作はしていません）";

/// トレイメニューを組み立てる（デスクトップホスト用、desktop-plan §9.9 の
/// 表どおりの3構成）。
///
/// - `status`が`None`（起動失敗で controller が無い場合）: T16-0 と同じ
///   「画面を開く」「アプリを終了」の2項目のみ。
/// - 収集停止中（`show_stop_item`が偽）: 状態、画面を開く、アプリを終了。
/// - 収集中（`show_stop_item`が真）: 状態、画面を開く、収集を停止、
///   アプリを終了。
///
/// 状態項目は disabled の [`MenuItemBuilder`] - クリックしても
/// `on_menu_event`には来る（`_ => {}`で無視される）が、無効化されているため
/// クリック自体が UI 上できない。
fn build_desktop_tray_menu(
    app: &AppHandle,
    status: Option<&RuntimeStatus>,
) -> tauri::Result<Menu<Wry>> {
    let mut builder = MenuBuilder::new(app);
    if let Some(status) = status {
        let status_item = MenuItemBuilder::with_id("status", tray_status::status_label(status))
            .enabled(false)
            .build(app)?;
        builder = builder.item(&status_item).separator();
    }
    builder = builder.text("open", "画面を開く");
    if status.map(tray_status::show_stop_item).unwrap_or(false) {
        builder = builder.text("stop", "収集を停止");
    }
    builder = builder.text("quit", "アプリを終了");
    builder.build()
}

/// トレイメニューを組み立てる（T16-2、サービス接続時 - desktop-plan §9.9
/// 「サービス・運転中」行: 状態、画面を開く、サービスを停止、管理画面を
/// 終了）。「サービスを停止」は Operators のみ
/// （[`tray_status::show_stop_service_action`]）。
fn build_service_tray_menu(
    app: &AppHandle,
    scm_state: &ScmState,
    can_operate: bool,
) -> tauri::Result<Menu<Wry>> {
    let status_item =
        MenuItemBuilder::with_id("status", tray_status::service_status_label(scm_state))
            .enabled(false)
            .build(app)?;
    let mut builder = MenuBuilder::new(app).item(&status_item).separator();
    builder = builder.text("open", "画面を開く");
    if tray_status::show_stop_service_action(scm_state, can_operate) {
        builder = builder.text("stop_service", "サービスを停止");
    }
    builder = builder.text("quit", "管理画面を終了");
    builder.build()
}

/// トレイメニューを組み立てる（T16-2、fallback 時）。「サービスを開始」は
/// Operators かつ SCM が`Stopped`のときだけ
/// （[`tray_status::show_start_service_action`]、実装指示 3.「Hide start
/// when health outcomes are ...」）。
///
/// #706 レビュー: 操作は Fallback の窓のボタンと同じ集合（[`fallback_actions`]）。
fn build_fallback_tray_menu(
    app: &AppHandle,
    info: &FallbackInfo,
    can_operate: bool,
) -> tauri::Result<Menu<Wry>> {
    let status_item = MenuItemBuilder::with_id("status", tray_status::fallback_status_label())
        .enabled(false)
        .build(app)?;
    let mut builder = MenuBuilder::new(app).item(&status_item).separator();
    builder = builder.text("open", "画面を開く");
    // #661 661-5: profile が締まったまま止まっている窓では「設定モードに戻す」も出る
    // （`relax-only`。戻らなければ管理者の許可で修復するので、Operators でなくても出す）。
    for action in fallback_actions(info, can_operate) {
        builder = builder.text(action.id, action.label);
    }
    builder = builder.text("quit", "終了");
    builder.build()
}

/// `state.view`の現在値から`(tooltip, menu)`を組み立てる - 初回のトレイ構築
/// （[`run`]の`setup`）と、以後の更新（[`sync_tray`]）の両方から呼ばれる
/// 共通ロジック。デスクトップ時の`desktop_status`は、初回構築時は
/// `apply_startup_outcome`が返した値、更新時は
/// `state.controller`から都度読み直した最新値を渡す想定
/// （実際のトレイ更新自体は`watch_collection_status`が担うため、
/// このモジュールが直接呼ぶのはトレイ未構築時 - `try_state`が`None`を
/// 返す間 - に限られる）。
fn tray_content_for_view(
    app: &AppHandle,
    view: &ShellView,
    can_operate: bool,
    desktop_status: Option<&RuntimeStatus>,
) -> tauri::Result<(String, Menu<Wry>)> {
    match view {
        ShellView::Desktop => {
            let tooltip = desktop_status
                .map(tray_status::tooltip_text)
                .unwrap_or_else(|| "banto-hub".to_string());
            let menu = build_desktop_tray_menu(app, desktop_status)?;
            Ok((tooltip, menu))
        }
        ShellView::Service { scm_state } => {
            let tooltip = tray_status::service_tooltip_text(scm_state);
            let menu = build_service_tray_menu(app, scm_state, can_operate)?;
            Ok((tooltip, menu))
        }
        ShellView::Fallback(info) => {
            let tooltip = tray_status::fallback_tooltip_text();
            let menu = build_fallback_tray_menu(app, info, can_operate)?;
            Ok((tooltip, menu))
        }
    }
}

/// 既存のトレイ（[`app.manage`]済みの[`TrayIcon`]）の tooltip/menu を
/// `state.view`の現在値に合わせて更新する。トレイがまだ構築されていない
/// （`setup`の初回パス中）場合は何もしない - 初回構築は[`run`]内で
/// [`tray_content_for_view`]を直接呼んで行う。
fn sync_tray(app: &AppHandle) {
    let Some(tray) = app.try_state::<TrayIcon<Wry>>() else {
        return;
    };
    let state = app.state::<AppState>();
    let view = state.view.lock().expect("view mutex poisoned").clone();
    // Desktop の場合、最新の収集状態を controller から読み直す -
    // `apply_startup_outcome`が返す値は「切り替わった瞬間」のものだが、
    // `sync_tray`は`retry_startup`等、切り替わった直後にしか呼ばれないため
    // 実質的には同じ値になる（以後の変化は`watch_collection_status`が
    // 引き続き反映する）。
    let desktop_status = if matches!(view, ShellView::Desktop) {
        state
            .controller
            .lock()
            .expect("controller mutex poisoned")
            .as_ref()
            .map(|controller| controller.status())
    } else {
        None
    };
    if let Ok((tooltip, menu)) = tray_content_for_view(
        app,
        &view,
        state.can_operate_service,
        desktop_status.as_ref(),
    ) {
        let _ = tray.set_tooltip(Some(tooltip));
        let _ = tray.set_menu(Some(menu));
    }
}

/// [`retry_startup`]・[`apply_host_switch_outcome`]共通の適用処理 -
/// `apply_startup_outcome`を呼び、デスクトップへ新規遷移した場合だけ
/// [`watch_collection_status`]を新しく spawn してからトレイを同期する
/// （[`ShellView`]が直前 Desktop でなかった場合 - 二重 spawn を避けるため）。
/// 第一スライスでは`retry_startup`の末尾にだけ書かれていたロジックを、
/// 第二スライスで追加した`apply_host_switch_outcome`とも共有できるよう
/// 切り出した。
fn apply_outcome_and_sync(app: &AppHandle, window: &WebviewWindow, outcome: StartupOutcome) {
    let state = app.state::<AppState>();
    let was_desktop = matches!(
        *state.view.lock().expect("view mutex poisoned"),
        ShellView::Desktop
    );
    let newly_desktop = matches!(outcome, StartupOutcome::Desktop(_));
    if let Some((_status, controller)) = apply_startup_outcome(app, window, outcome) {
        if !was_desktop && newly_desktop {
            let app_handle = app.clone();
            tauri::async_runtime::spawn(watch_collection_status(app_handle, controller));
        }
    }
    // #586: 終了が始まった後は適用していない（`apply_startup_outcome`）ので、
    // トレイも作り直さない。
    if state.hub.is_closed() {
        return;
    }
    sync_tray(app);
}

/// 起動判定をやり直す共通処理（トレイ「再試行」が呼ぶ - このモジュール doc
/// 「起動シーケンス」節の該当行参照）。[`decide_startup`]の単純な決定木を
/// 呼ぶだけで、`HostSwitchEngine`は使わない（単なる状態の再評価であり、
/// 明示的な切替操作ではないため - 「サービスを開始/停止」は
/// [`run_host_switch`]を使う）。
///
/// #706 レビュー: 画面を変えるので、切り替えと同じ進行中フラグを取る（見張りが古い結果を
/// 適用しないための前提 - [`spawn_service_watch`]）。進行中ならトレイへ知らせて何もしない。
fn retry_startup(app: &AppHandle) {
    if !host_switch_ipc::begin_switch_or_warn(app) {
        return;
    }
    retry_startup_locked(app);
    host_switch_ipc::end_switch();
}

/// [`retry_startup`] の本体。呼ぶ側が進行中フラグを取ってから呼ぶ（Fallback の窓の [再試行]
/// - `host_switch_ipc::retry_startup` - も使う）。
pub(crate) fn retry_startup_locked(app: &AppHandle) {
    let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) else {
        return;
    };
    let slot = Arc::clone(&app.state::<AppState>().hub);
    // #586: 終了が始まった後は判定し直さない（起動もしない）。この確認の後に
    // 終了が始まっても、起動は `attempt_desktop_start` の札で、適用は
    // `apply_startup_outcome` で断られる。
    if slot.is_closed() {
        return;
    }
    let target = expected_probe_target();
    let outcome = decide_startup(&target, &slot);
    apply_outcome_and_sync(app, &window, outcome);
}

/// [`HostSwitchEngine`]が要求する
/// [`banto_hub_core::host_switch::DesktopHostControl`]の実装
/// （Desktop→Service 切替ウィザード、desktop-plan §9.7）。
///
/// `request_stop`はこのプロセスが保有する[`RunningHub`]を取り出して
/// `shutdown`し、controller ハンドルも消す。profile mutex は
/// `RunningHub::shutdown`側で解放される。
///
/// #586: 取り出すときに「進行中の停止」の札を取り（[`HubSlot::take_for_stop`]）、
/// `shutdown` を終えてから返す。その間に終了が始まっても、終了はこの停止を
/// 待つ。`is_stopped` は「置き場所が空」ではなく「置き場所が空で、進行中の
/// 起動・停止も無い」（[`HubSlot::is_idle`]）で答える - 置き場所が空でも、
/// 終了の後始末が取り出した Hub をまだ止めている最中のことがある。
#[cfg(windows)]
struct ShellDesktopControl {
    app: AppHandle,
}

#[cfg(windows)]
impl DesktopHostControl for ShellDesktopControl {
    fn request_stop(&mut self) -> Result<(), DesktopHostError> {
        let state = self.app.state::<AppState>();
        if let Some((hub, ticket)) = state.hub.take_for_stop() {
            tauri::async_runtime::block_on(hub.shutdown());
            *state.controller.lock().expect("controller mutex poisoned") = None;
            drop(ticket);
        }
        Ok(())
    }

    fn is_stopped(&self) -> bool {
        self.app.state::<AppState>().hub.is_idle()
    }
}

/// [`HostSwitchEngine::step`]の1フェーズに許すタイムアウト（実装指示 1./2.
/// 「wait_until_settled を呼ぶ」「SCM Stopped かつ health Unreachable まで
/// 待つ」）。SCM の`StartPending`/`StopPending`は実運用で数秒〜十数秒
/// かかりうるため、単一フェーズの上限としては`WindowsServiceManager::restart`
/// が使う`wait_until_settled`の例（30秒）よりやや短いが十分な値にした。
#[cfg(windows)]
const HOST_SWITCH_PHASE_TIMEOUT: Duration = Duration::from_secs(15);

/// ポーリング間隔（`WindowsServiceManager::restart`の例の 200ms と近い
/// 値 - トレイ操作の体感待ち時間として長すぎない範囲にした）。
#[cfg(windows)]
const HOST_SWITCH_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// [`run_host_switch`]の最終結果 - [`apply_host_switch_outcome`]が
/// [`StartupOutcome`]へ変換する前の中間表現（バックグラウンドスレッド
/// から`AppHandle`越しに結果だけを持ち帰るための値型）。
#[cfg(windows)]
enum HostSwitchResult {
    /// サービスへ接続してよい（health `Healthy`まで確認済み）。
    Service { scm_state: ScmState },
    /// デスクトップホストとして起動してよい（SCM `Stopped`+旧 health
    /// `Unreachable`まで確認済み - 実際の`HubRuntime::start`は
    /// [`apply_host_switch_outcome`]側で試みる）。
    Desktop,
    /// 失敗到達・権限なし等 - fallback 画面に表示する診断情報。
    Faulted {
        scm_state: Option<ScmState>,
        health: Option<HealthOutcome>,
        reason: String,
        /// #661 661-5: サービスが終了コード 4・5 で止まったか（窓の選び方）。
        fault: Option<ServiceFault>,
    },
}

/// [`HostSwitchEngine::step`]を`Poll`で繰り返し、終端の
/// [`StepOutcome`]（`Completed`/`Faulted`）に到達するまでブロッキングに
/// 進める（実装指示 1./2. - `TransitionHandle::wait_until_settled`と同じ
/// 「ポーリング＋sleep」形だが、engine 側が持つ複数フェーズ分の待ちを
/// まとめて1回のブロッキング呼び出しにする）。呼び出し元
/// （[`run_host_switch`]）が背景スレッドで呼ぶ前提 - ここ自体はスレッドを
/// 起動しない。
///
/// #586/#587: シェルの終了が始まっても、ここでは途中で打ち切らない（終端まで
/// 進める）。終了との関係は [`run_host_switch`] が決める: 最初の `step` の前に
/// 閉じられていればここを呼ばない（SCM を一切呼ばない）、呼んだ後は完走させて
/// 終了がその完了を待つ（理由は [`hub_slot`] の doc「ホスト切替そのものも
/// 数える」- SCM を呼んだ後に途中で止めると、サービスもアプリも止まった状態に
/// 落ちうる）。#586 の最初の版はここで `Poll` ごとに打ち切っていたが、
/// Desktop→Service でデスクトップの Hub を止めた後・サービスを開始する前に
/// 打ち切ると両方止まるので外した。
#[cfg(windows)]
fn drive_host_switch<M, P, D>(
    engine: &mut HostSwitchEngine<M, P, D>,
    command: SwitchCommand,
    on_phase: &mut dyn FnMut(&SwitchPhase),
) -> HostSwitchResult
where
    M: ServiceManager,
    P: HubHealthProbe,
    D: DesktopHostControl,
{
    let mut outcome = match engine.step(command) {
        Ok(outcome) => outcome,
        Err(SwitchError::PermissionDenied) => {
            return HostSwitchResult::Faulted {
                scm_state: None,
                health: None,
                reason: SwitchError::PermissionDenied.to_string(),
                fault: None,
            }
        }
    };
    // #661 661-5: 段が変わるたびに進み具合を知らせる（サービスの開始・停止の段は
    // checkpoint が進む間は待ちを延ばすので、締める・緩める間ずっと同じ文が出る）。
    let mut last_phase: Option<SwitchPhase> = None;
    loop {
        let phase = engine.state().phase.clone();
        if last_phase.as_ref() != Some(&phase) {
            on_phase(&phase);
            last_phase = Some(phase);
        }
        match outcome {
            StepOutcome::Completed(HostKind::Service) => {
                return HostSwitchResult::Service {
                    scm_state: ScmState::Running,
                };
            }
            StepOutcome::Completed(HostKind::Desktop) => {
                return HostSwitchResult::Desktop;
            }
            StepOutcome::Completed(HostKind::Offline) => {
                // `SwitchToService`/`SwitchToDesktop`が`Offline`へ完了する
                // 経路は無い（`host_switch`モジュール doc参照）- 呼び出し側
                // の前提が崩れていない限り到達しないが、念のため安全側で
                // fallback 表示にする。
                return HostSwitchResult::Faulted {
                    scm_state: None,
                    health: engine.state().last_health.clone(),
                    reason: "予期しない完了状態(Offline)です".to_string(),
                    fault: engine.state().service_fault,
                };
            }
            StepOutcome::Faulted {
                reached,
                stage,
                reason,
            } => {
                let scm_state = match reached {
                    HostKind::Service => Some(ScmState::Running),
                    HostKind::Offline => Some(ScmState::Stopped),
                    HostKind::Desktop => None,
                };
                return HostSwitchResult::Faulted {
                    scm_state,
                    health: engine.state().last_health.clone(),
                    reason: format!("{stage}: {reason}"),
                    fault: engine.state().service_fault,
                };
            }
            StepOutcome::NoOp | StepOutcome::TransitionInProgress => {
                // 呼び出し元は毎回新しい engine を構築するため、通常は
                // 起こらない（`current`が既に目的地、または他の遷移が
                // 進行中というのはこの engine インスタンス自身の状態と
                // 矛盾する）- 発生したら診断として fallback へ回す。
                return HostSwitchResult::Faulted {
                    scm_state: None,
                    health: None,
                    reason: "既に目的の状態か、他の遷移と競合しました".to_string(),
                    fault: None,
                };
            }
            StepOutcome::Waiting => {
                std::thread::sleep(HOST_SWITCH_POLL_INTERVAL);
            }
            StepOutcome::Progressed => {
                // 要求発行自体は同期 API なので即座に次の判定へ進む
                // （`host_switch`モジュール doc「DesktopStopping/
                // ServiceStopping」節と同じ理由）。
            }
        }
        outcome = match engine.step(SwitchCommand::Poll) {
            Ok(outcome) => outcome,
            Err(SwitchError::PermissionDenied) => {
                return HostSwitchResult::Faulted {
                    scm_state: None,
                    health: engine.state().last_health.clone(),
                    reason: SwitchError::PermissionDenied.to_string(),
                    fault: engine.state().service_fault,
                }
            }
        };
    }
}

/// [`run_host_switch`]がバックグラウンドスレッドで[`drive_host_switch`]を
/// 終えた後、結果を[`StartupOutcome`]へ変換して画面・トレイへ反映する。
///
/// `HostSwitchResult::Desktop`は「Desktop 起動してよい」という engine の
/// 許可でしかない（`host_switch`モジュール doc「実際の`HubRuntime::start`は
/// シェル側が呼ぶ」）ため、ここで初めて[`attempt_desktop_start`]を呼んで
/// 実際の起動を試みる。
#[cfg(windows)]
fn apply_host_switch_outcome(app: &AppHandle, target: &ProbeTarget, result: HostSwitchResult) {
    let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) else {
        return;
    };
    let outcome = match result {
        HostSwitchResult::Service { scm_state } => StartupOutcome::Service {
            host: target.host.clone(),
            port: target.port,
            scm_state,
        },
        HostSwitchResult::Desktop => {
            // #661 661-5: engine の「Desktop へ進んでよい」（SCM Stopped・旧 health 消失）の
            // 後でも、起動の判定（[`decide_startup`]）をもう一度通す - profile が締まった
            // まま（PRESHUTDOWN の後の 0・scan より前の 2・3）なら Desktop に進まない。
            // #586: 終了が始まっていれば起動しない（札が取れない）。
            let slot = Arc::clone(&app.state::<AppState>().hub);
            decide_startup(target, &slot)
        }
        HostSwitchResult::Faulted {
            scm_state,
            health,
            reason,
            fault,
        } => StartupOutcome::Fallback(FallbackInfo {
            scm_state,
            health,
            desktop_error: Some(reason),
            cause: launch_gate::switch_fault_cause(fault),
        }),
    };
    apply_outcome_and_sync(app, &window, outcome);
}

/// トレイ「サービスを開始/停止」および切替ウィザード invoke 共通の
/// エントリポイント。呼び出し元が[`host_switch_ipc::begin_switch_or_warn`]で
/// 進行中フラグを立てたうえで呼ぶこと（完了時にこの関数がクリアする）。
///
/// 現在の[`ShellView`]から[`HostKind`]（engine の初期状態）を求め、
/// [`HostSwitchEngine`]を1回だけ構築してバックグラウンドスレッドで
/// [`drive_host_switch`]を完了まで進める - 呼び出し元（トレイのメニュー
/// イベントハンドラ / invoke）はブロックしない。
///
/// #586/#587: 切替全体を終了の追跡に入れる（[`hub_slot`] の doc「ホスト切替
/// そのものも数える」）。
///
/// 1. **spawn する前に**、閉じた印の確認と同じロックの中で切替を登録する
///    （[`HubSlot::begin_switch`]）。閉じていれば（終了が始まっていれば）
///    切替を始めず、進行中フラグを下ろして失敗の進捗を出すだけ（SCM を
///    呼ばない）。登録してあるので、この後に終了が閉じても、終了は切替を
///    知らずに戻ることが無い（#587 のレビュー: 以前は確認の後・最初の
///    `step` の前に閉じられると、終了は空と見てすぐ戻り、その後で切替が
///    `ServiceManager::stop` を呼んでいた）。
/// 2. スレッドは最初の `engine.step` の**直前に**もう一度閉じた印を見る
///    （[`hub_slot::run_switch`]）。閉じていれば SCM を一切呼ばずに終える。
/// 3. 閉じていなければ、終端まで完走させ、結果の適用まで行ってから札を返す。
///    終了はその完了を自分の予算まで待つ。終了の開始後の適用ではデスクトップの
///    Hub を起動しない（Service→Desktop の途中で終了すると、サービスは止まり
///    アプリも終わる。終了がサービスを開始し直すことはしない）。
#[cfg(windows)]
pub(crate) fn run_host_switch(app: &AppHandle, command: SwitchCommand) {
    let state = app.state::<AppState>();
    let Some(switch_ticket) = state.hub.begin_switch() else {
        host_switch_ipc::end_switch();
        host_switch_ipc::emit_progress(
            app,
            host_switch_ipc::HostSwitchProgressEvent {
                phase: "faulted".into(),
                message: EXITING_SWITCH_MESSAGE.into(),
                done: true,
                error: Some(EXITING_SWITCH_MESSAGE.into()),
                fallback_cause: None,
            },
        );
        return;
    };
    let initial_host = match &*state.view.lock().expect("view mutex poisoned") {
        ShellView::Desktop => HostKind::Desktop,
        ShellView::Service { .. } => HostKind::Service,
        ShellView::Fallback(_) => HostKind::Offline,
    };
    let can_operate = state.can_operate_service;
    let target = expected_probe_target();
    let manager = build_service_manager();
    let probe = HttpHubHealthProbe::with_host(target.root.clone(), target.host.clone());
    let mut engine = HostSwitchEngine::new(
        manager,
        probe,
        ShellDesktopControl { app: app.clone() },
        HostSwitchConfig {
            expected_profile: target.profile_id.clone(),
            expected_port: target.port,
            can_operate_service: can_operate,
            initial_host,
            phase_timeout: HOST_SWITCH_PHASE_TIMEOUT,
        },
    );

    let app_handle = app.clone();
    std::thread::spawn(move || {
        host_switch_ipc::emit_progress(
            &app_handle,
            host_switch_ipc::HostSwitchProgressEvent {
                phase: "running".into(),
                message: "切替処理を実行しています…".into(),
                done: false,
                error: None,
                fallback_cause: None,
            },
        );
        if command == SwitchCommand::SwitchToDesktop {
            push_desktop_log(&app_handle, desktop_log::LOG_TO_SETTING_STARTED);
        }
        // 札は `run_switch` が、駆動と結果の適用を終えてから返す。
        let ran = hub_slot::run_switch(switch_ticket, || {
            let progress_app = app_handle.clone();
            let result = drive_host_switch(&mut engine, command, &mut |phase| {
                if let Some(message) = launch_gate::switch_phase_message(phase) {
                    emit_running(&progress_app, message);
                }
            });
            apply_host_switch_outcome(&app_handle, &target, result);
            // #661 661-5: 結果は適用した後の画面で決める（engine が Desktop を許しても、
            // profile が締まっていれば Fallback になる）。
            let progress = progress_after_apply(&app_handle);
            if command == SwitchCommand::SwitchToDesktop {
                note_setting_outcome(&app_handle, &target);
            }
            progress
        });
        let progress = ran.unwrap_or_else(|| SwitchProgress {
            phase: "faulted",
            message: EXITING_SWITCH_MESSAGE.to_string(),
            error: Some(EXITING_SWITCH_MESSAGE.to_string()),
            fallback_cause: None,
        });
        host_switch_ipc::end_switch();
        emit_done(&app_handle, progress);
    });
}

/// 切り替え・「設定モードに戻す」 の終わりに出す進み具合（[`emit_done`]）。
#[cfg(windows)]
struct SwitchProgress {
    phase: &'static str,
    message: String,
    error: Option<String>,
    fallback_cause: Option<&'static str>,
}

/// 適用した後の画面から、終わりの進み具合を決める（#661 661-5）。Fallback なら理由の窓の
/// 見出し（と診断の文）を失敗として出し、`/status` が理由に合う文言と操作を選べるよう
/// 理由の名前（[`FallbackCause::key`]）を添える。
#[cfg(windows)]
fn progress_after_apply(app: &AppHandle) -> SwitchProgress {
    let view = app
        .state::<AppState>()
        .view
        .lock()
        .expect("view mutex poisoned")
        .clone();
    match view {
        ShellView::Desktop => SwitchProgress {
            phase: "completed",
            message: "アプリへの切替が完了しました".to_string(),
            error: None,
            fallback_cause: None,
        },
        ShellView::Service { .. } => SwitchProgress {
            phase: "completed",
            message: "サービスへの切替が完了しました".to_string(),
            error: None,
            fallback_cause: None,
        },
        ShellView::Fallback(info) => {
            let detail = match (launch_gate::cause_page(info.cause), &info.desktop_error) {
                (Some((title, _)), Some(err)) => format!("{title}（{err}）"),
                (Some((title, _)), None) => title.to_string(),
                (None, Some(err)) => err.clone(),
                (None, None) => "このアプリで Hub を起動できませんでした".to_string(),
            };
            SwitchProgress {
                phase: "faulted",
                message: format!("切替に失敗しました: {detail}"),
                error: Some(detail),
                fallback_cause: info.cause.key(),
            }
        }
    }
}

/// 進み具合（途中）を出す。
#[cfg(windows)]
fn emit_running(app: &AppHandle, message: &str) {
    host_switch_ipc::emit_progress(
        app,
        host_switch_ipc::HostSwitchProgressEvent {
            phase: "running".into(),
            message: message.into(),
            done: false,
            error: None,
            fallback_cause: None,
        },
    );
}

/// 進み具合（終わり）を出す。
#[cfg(windows)]
fn emit_done(app: &AppHandle, progress: SwitchProgress) {
    host_switch_ipc::emit_progress(
        app,
        host_switch_ipc::HostSwitchProgressEvent {
            phase: progress.phase.into(),
            message: progress.message,
            done: true,
            error: progress.error,
            fallback_cause: progress.fallback_cause.map(str::to_string),
        },
    );
}

/// デスクトップのログに書く行を、起きた時刻で覚えておく（#661 661-5。書くのは
/// [`flush_desktop_log`]）。シェルのログ（標準エラー出力）にも同じ行を出す。
#[cfg(windows)]
fn push_desktop_log<R: Runtime>(app: &AppHandle<R>, text: impl Into<String>) {
    let entry = DesktopLogEntry::now_system(text);
    log_line(&format!("banto-hub-shell: {}", entry.text));
    if let Some(state) = app.try_state::<AppState>() {
        if let Ok(mut pending) = state.pending_log.lock() {
            pending.push(entry);
        }
    }
}

/// 覚えておいた行を profile の `logs\banto-hub-desktop-YYYY-MM-DD.log` に書く（このアプリの
/// Hub が動いている = 設定モードのときだけ呼ぶ）。書けなければ標準エラー出力へ出して捨てる
/// （ログが書けないことで切り替えを止めない）。
#[cfg(windows)]
fn flush_desktop_log(app: &AppHandle, logs_dir: &std::path::Path) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    let entries = match state.pending_log.lock() {
        Ok(mut pending) => std::mem::take(&mut *pending),
        Err(_) => return,
    };
    if entries.is_empty() {
        return;
    }
    if let Err(err) = desktop_log::append_desktop_log_now(logs_dir, &entries) {
        log_err_line(&format!(
            "banto-hub-shell: デスクトップのログに書けませんでした（{err}）"
        ));
    }
}

/// このアプリの Hub が動き始めていたら、デスクトップのログに「完了」を足して書く。
#[cfg(windows)]
fn log_setting_completed_if_desktop(app: &AppHandle, target: &ProbeTarget) {
    let desktop = matches!(
        *app.state::<AppState>()
            .view
            .lock()
            .expect("view mutex poisoned"),
        ShellView::Desktop
    );
    if desktop {
        push_desktop_log(app, desktop_log::LOG_TO_SETTING_COMPLETED);
        flush_desktop_log(app, &target.logs_dir);
    }
}

/// 設定モードへの切り替え（サービスの停止）の結果をデスクトップのログに残す: このアプリの Hub が
/// 動き始めたら「完了」を足して書き、締めたまま残った（5）なら案内を覚えておく（運用モードの
/// 間は書けないので、次に設定モードになったときに書く）。
#[cfg(windows)]
fn note_setting_outcome(app: &AppHandle, target: &ProbeTarget) {
    let left_tight = matches!(
        &*app
            .state::<AppState>()
            .view
            .lock()
            .expect("view mutex poisoned"),
        ShellView::Fallback(FallbackInfo {
            cause: FallbackCause::OperationStopped { left_tight: true },
            ..
        })
    );
    if left_tight {
        push_desktop_log(app, desktop_log::LOG_LEFT_TIGHT);
    }
    log_setting_completed_if_desktop(app, target);
}

/// 「設定モードに戻す」（`restore_setting_mode` invoke・トレイ）のエントリポイント（#661 661-5）。
/// 呼び出し元が [`host_switch_ipc::begin_switch_or_warn`] で進行中フラグを立てたうえで呼ぶ
/// （完了時にこの関数がクリアする）。手本は ChronoGazer の `run_restore_setting`（#623 PR-4）。
///
/// 1. サービスを `relax-only` で開始して止まるのを待つ（[`relax_only_and_wait`]。Operators は
///    UAC なし）。
/// 2. 0（緩め終えた）→ 起動の判定（[`decide_startup`]）→ 形が緩んでいれば Desktop。
/// 3. 5・失敗 → 昇格ヘルパーの修復（`banto-hub-elev.exe grant-profile-acl`、UAC）→ 成功なら 2.
///    と同じ判定、失敗なら Fallback「運用モードのまま残っています」に理由を添える。
/// 4. サービスが動いている（停止中ではなかった）→ 何も変えずに知らせるだけ（画面は変えない）。
///
/// 切り替えと同じく終了の追跡に入れる（[`HubSlot::begin_switch`]・[`hub_slot::run_switch`]）。
#[cfg(windows)]
pub(crate) fn run_restore_setting(app: &AppHandle) {
    let state = app.state::<AppState>();
    let Some(switch_ticket) = state.hub.begin_switch() else {
        host_switch_ipc::end_switch();
        emit_done(
            app,
            SwitchProgress {
                phase: "faulted",
                message: EXITING_SWITCH_MESSAGE.to_string(),
                error: Some(EXITING_SWITCH_MESSAGE.to_string()),
                fallback_cause: None,
            },
        );
        return;
    };
    let target = expected_probe_target();
    let app_handle = app.clone();
    std::thread::spawn(move || {
        emit_running(&app_handle, launch_gate::RESTORE_RELAXING_MESSAGE);
        push_desktop_log(&app_handle, desktop_log::LOG_RESTORE_STARTED);
        let ran =
            hub_slot::run_switch(switch_ticket, || restore_setting_body(&app_handle, &target));
        let progress = ran.unwrap_or_else(|| SwitchProgress {
            phase: "faulted",
            message: EXITING_SWITCH_MESSAGE.to_string(),
            error: Some(EXITING_SWITCH_MESSAGE.to_string()),
            fallback_cause: None,
        });
        host_switch_ipc::end_switch();
        emit_done(&app_handle, progress);
    });
}

/// [`run_restore_setting`] の本体（札を持った背景のスレッドで呼ぶ）。
#[cfg(windows)]
fn restore_setting_body(app: &AppHandle, target: &ProbeTarget) -> SwitchProgress {
    let manager = build_service_manager();
    let outcome = relax_only_and_wait(&manager, &RealClock, SWITCH_TIMINGS);
    let after = launch_gate::after_relax(&outcome);
    let relax_line = format!("設定モードに戻す（relax-only）: {outcome:?}");
    if matches!(after, AfterRelax::Decide) {
        log_line(&format!("banto-hub-shell: {relax_line}"));
    } else {
        // #706 実機確認: 失敗はデスクトップのログにも残す（GUI では標準出力が見えない）。
        push_desktop_log(app, relax_line);
    }
    let failed = match after {
        AfterRelax::Decide => {
            push_desktop_log(app, desktop_log::log_restored_by("relax-only"));
            None
        }
        AfterRelax::Running(notice) => {
            // 何も変えていない - 画面はそのまま、知らせるだけ。
            return SwitchProgress {
                phase: "faulted",
                message: notice.clone(),
                error: Some(notice),
                fallback_cause: None,
            };
        }
        AfterRelax::Elevate { left_tight } => {
            if left_tight {
                push_desktop_log(app, desktop_log::LOG_LEFT_TIGHT);
            }
            emit_running(app, launch_gate::RESTORE_REPAIRING_MESSAGE);
            match repair_profile_acl(target) {
                Ok(()) => {
                    push_desktop_log(app, desktop_log::log_restored_by("管理者の許可で修復"));
                    None
                }
                Err(reason) => {
                    push_desktop_log(app, desktop_log::log_restore_failed(&reason));
                    Some(reason)
                }
            }
        }
    };
    let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) else {
        report_shell_error(app, "設定モードに戻す: メインウィンドウが見つかりません");
        return progress_after_apply(app);
    };
    let outcome = match failed {
        // 戻せた（はず）: 再試行と同じ判定。形がまだ締まっていれば Desktop に進まない。
        None => {
            let slot = Arc::clone(&app.state::<AppState>().hub);
            decide_startup(target, &slot)
        }
        Some(reason) => StartupOutcome::Fallback(FallbackInfo {
            scm_state: build_service_manager()
                .query_status()
                .ok()
                .map(|status| status.state),
            health: None,
            desktop_error: Some(format!("設定モードに戻せませんでした。{reason}")),
            cause: FallbackCause::OperationStopped { left_tight: true },
        }),
    };
    apply_outcome_and_sync(app, &window, outcome);
    log_setting_completed_if_desktop(app, target);
    progress_after_apply(app)
}

/// 昇格ヘルパーで profile の ACL を設定モードの形に直す（`grant-profile-acl`、UAC。呼ぶ
/// スレッドは終わるまで止まる）。#706 実機確認（オーナー決定）: どの profile も、このアプリを
/// 動かしている利用者の SID と profile-id を渡す（[`launch_gate::repair_tokens`]）。SID を
/// 得られない・引数に使えない文字があるときだけ、手順を案内して断る。
///
/// #706 レビュー: `grant-profile-acl` は**マシンの環境**の root を使う。シェルは
/// このプロセスの環境で root を解くので、起動する前に両者が一致するかを確かめ、一致しない・
/// 確かめられないときは起動せずに手順を案内する（[`launch_gate::check_repair_root`]）。
#[cfg(windows)]
fn repair_profile_acl(target: &ProbeTarget) -> Result<(), String> {
    use banto_hub_core::profile_grant::{PRODUCT_DIR, ROOT_ENV};
    use banto_hub_core::service_elevated::ElevatedAction;
    use banto_safe_root::guard::{resolve_machine_root, win::RegistryMachineEnvironment};
    let helper_root = resolve_machine_root(&RegistryMachineEnvironment, ROOT_ENV, PRODUCT_DIR)
        .map_err(|error| banto_hub_core::profile_grant::machine_root_text(&error));
    let user_sid = banto_safe_root::acl::current_user_sid().map_err(|err| err.to_string());
    launch_gate::check_repair_root(
        &target.root,
        helper_root.as_deref().map_err(String::as_str),
        user_sid.as_ref().ok().map(|sid| sid.as_str()),
        target.profile_id.as_str(),
    )?;
    let elev = host_switch_ipc::resolve_elev_exe().ok_or_else(|| {
        "banto-hub-elev.exe が見つかりません（シェルと同じディレクトリに配置してください）"
            .to_string()
    })?;
    // #706 実機確認（オーナー決定）: 既定でない profile も、このアプリを動かしている利用者の
    // SID と profile-id を渡して修復する（[`launch_gate::repair_tokens`]）。
    let tokens = launch_gate::repair_tokens(
        user_sid
            .as_ref()
            .map(|sid| sid.as_str())
            .map_err(String::as_str),
        target.profile_id.as_str(),
        ElevatedAction::GRANT_PROFILE_ACL,
    )?;
    let tokens: Vec<&str> = tokens.iter().map(String::as_str).collect();
    host_switch_ipc::elev_result_message(banto_service_elev::launch::run_elevated(&elev, &tokens))
}

/// サービスの見張りの間隔（#661 661-5。Service の画面・Fallback の窓の間だけ SCM を照会する）。
#[cfg(windows)]
const SERVICE_WATCH_INTERVAL: Duration = Duration::from_secs(5);

/// 見張りが Fallback の窓に移す結果（診断は SCM の状態だけ）。
#[cfg(windows)]
fn watch_fallback(scm_state: Option<ScmState>, cause: FallbackCause) -> StartupOutcome {
    StartupOutcome::Fallback(FallbackInfo {
        scm_state,
        health: None,
        desktop_error: None,
        cause,
    })
}

/// サービスの見張り（#661 661-5）: サービスに接続している間にサービスが止まったら（終了コード
/// 4・5 を含む）、合う Fallback の窓へ移る。止まる途中（StopPending）で一般の窓に移った後も、
/// 停止に達したら終了コードと profile の形で理由を選び直す（#690）。**Hub は決して起動しない**
/// （[`launch_gate::on_watch_tick`]）。切り替え・「設定モードに戻す」 の間と、終了が始まった後は
/// 何もしない。
#[cfg(windows)]
fn spawn_service_watch(app: AppHandle) {
    std::thread::spawn(move || loop {
        std::thread::sleep(SERVICE_WATCH_INTERVAL);
        let state = app.state::<AppState>();
        if state.hub.is_closed() {
            return;
        }
        if host_switch_ipc::is_switching() {
            continue;
        }
        // 観測した画面の世代を覚えておく（適用の直前に比べる - #706 レビュー）。
        let (observed, shown_scm) = watched_view(&state);
        let watched = observed.view;
        // このアプリの Hub が動いている間は見張らない（SCM も照会しない）。
        if watched == launch_gate::WatchedView::Desktop {
            continue;
        }
        let mut scm_error = None;
        let (scm, fault) = match build_service_manager().query_status() {
            Ok(status) => {
                let fault = (status.state == ScmState::Stopped)
                    .then(|| banto_hub_core::host_switch::service_fault(&status))
                    .flatten();
                (Some(status.state), fault)
            }
            Err(err) => {
                scm_error = Some(err.to_string());
                (None, None)
            }
        };
        let mode = if scm == Some(ScmState::Stopped) {
            launch_gate::observe_profile_mode(&expected_probe_target().profile_dir)
        } else {
            ProfileModeObservation::NotObserved
        };
        let step = launch_gate::on_watch_tick(watched, scm.as_ref(), &mode, fault);
        // 理由が同じでも SCM の状態が変わったら窓とトレイを直す（#706 レビュー）。
        let step =
            launch_gate::refresh_on_scm_change(step, watched, shown_scm.as_ref(), scm.as_ref());
        let outcome = match step {
            launch_gate::WatchStep::Stay => continue,
            launch_gate::WatchStep::ToFallback(cause) | launch_gate::WatchStep::Refresh(cause) => {
                watch_fallback(scm.clone(), cause)
            }
            // サービスが Running に戻った: health が Healthy なら Service の画面へ戻す。そうでなければ
            // 理由を直すだけ（直す所が無ければ何もしない）。
            launch_gate::WatchStep::Reconnect(cause) => {
                match probe_service_or_fallback(&expected_probe_target()) {
                    service @ StartupOutcome::Service { .. } => service,
                    _ if launch_gate::WatchedView::Fallback(cause) == watched => continue,
                    _ => watch_fallback(scm.clone(), cause),
                }
            }
        };
        let cause = match &outcome {
            StartupOutcome::Fallback(info) => Some(info.cause),
            _ => None,
        };
        // 照会の間に切り替えが始まっていたら任せる（進行中フラグを取ってから適用する）。
        if !host_switch_ipc::try_begin_switch() {
            continue;
        }
        // #706 レビュー: 観測の後に画面が変わっていたら（切り替えが終わって Desktop が起動した・
        // 再試行した等）、この結果は古い。適用すると起動したばかりの Hub を止めるので捨てる。
        // 画面を変える経路は進行中フラグを取るので、フラグを持つ間は世代が進まない。
        if !launch_gate::watch_result_is_fresh(observed, watched_view(&state).0) {
            host_switch_ipc::end_switch();
            continue;
        }
        // #706 実機確認: GUI では標準出力が見えないので、デスクトップのログにも残す。
        push_desktop_log(
            &app,
            format!(
                "サービスの見張り: サービス {} - 窓 {}",
                scm.as_ref().map_or_else(
                    || format!(
                        "確認できませんでした（{}）",
                        scm_error.as_deref().unwrap_or("不明")
                    ),
                    ToString::to_string
                ),
                cause.map_or_else(|| "Service".to_string(), |cause| format!("{cause:?}"))
            ),
        );
        if cause == Some(FallbackCause::OperationStopped { left_tight: true }) {
            push_desktop_log(&app, desktop_log::LOG_LEFT_TIGHT);
        }
        if let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) {
            apply_outcome_and_sync(&app, &window, outcome);
        }
        host_switch_ipc::end_switch();
        // `/status` を開いていれば、理由に合う文言と操作を出す（切り替えの失敗ではないので、
        // 文は窓の見出しだけ）。
        let mut progress = progress_after_apply(&app);
        if cause.is_some() {
            progress.phase = "stopped";
            progress.message = progress.error.clone().unwrap_or_default();
        }
        emit_done(&app, progress);
    });
}

/// トレイ「サービスを停止」クリック時のエントリポイント（サービス接続時、
/// Operators/管理者のみ表示 - [`build_service_tray_menu`]）。
///
/// T16-2 第二スライスで[`run_host_switch`]（`HostSwitchEngine`）に置き換えた。
/// SCM `stop()`発行後、`Stopped`到達**かつ**旧 health `Unreachable`到達を
/// 確認してから初めてデスクトップホストとしての起動を試みる（既知の gap
/// 「サービス停止後の自動デスクトップ引き継ぎは『たまたまそうなる』動作」
/// への対応、モジュール doc参照）。
#[cfg(windows)]
fn stop_service(app: &AppHandle) {
    if !host_switch_ipc::begin_switch_or_warn(app) {
        return;
    }
    host_switch_ipc::emit_progress(
        app,
        host_switch_ipc::HostSwitchProgressEvent {
            phase: "starting".into(),
            message: "設定モードへの切り替えを始めています…".into(),
            done: false,
            error: None,
            fallback_cause: None,
        },
    );
    run_host_switch(app, SwitchCommand::SwitchToDesktop);
}

#[cfg(not(windows))]
fn stop_service(_app: &AppHandle) {}

/// トレイ「サービスを開始」クリック時のエントリポイント（fallback 時、
/// Operators/管理者かつ SCM が`Stopped`のときだけ表示 -
/// [`build_fallback_tray_menu`]）。T16-2 第二スライスで[`run_host_switch`]に
/// 置き換えた - SCM `start()`発行後、`Running`到達**かつ**health`Healthy`
/// 到達を確認してから接続する（既知の gap「`ServiceManager::start`/`stop`の
/// 完了待ちをしていない」への対応）。
#[cfg(windows)]
fn start_service(app: &AppHandle) {
    if !host_switch_ipc::begin_switch_or_warn(app) {
        return;
    }
    host_switch_ipc::emit_progress(
        app,
        host_switch_ipc::HostSwitchProgressEvent {
            phase: "starting".into(),
            message: "運用モードへの切り替えを始めています…".into(),
            done: false,
            error: None,
            fallback_cause: None,
        },
    );
    run_host_switch(app, SwitchCommand::SwitchToService);
}

#[cfg(not(windows))]
fn start_service(_app: &AppHandle) {}

/// トレイ「設定モードに戻す」（#661 661-5。profile が締まったまま止まっている Fallback の窓）。
#[cfg(windows)]
fn restore_setting(app: &AppHandle) {
    if !host_switch_ipc::begin_switch_or_warn(app) {
        return;
    }
    run_restore_setting(app);
}

#[cfg(not(windows))]
fn restore_setting(_app: &AppHandle) {}

/// `×` でトレイへ格納した初回だけ出す OS 通知（UX-7 / 実装指示 4.「初回
/// だけ」）。既読フラグは Hub 本体の DB/data_dir とは独立したシェル固有の
/// 状態なので、`app.path().app_data_dir()`（`tauri.conf.json`の`identifier`
/// 配下）に置く - `RunningHub`の起動に失敗していても書ける場所であること、
/// 複数 profile／複数インストール間で共有すべきではないことの両方を満たす。
/// フラグを書けないときは通知をあきらめる（毎回通知され続けるより安全側）-
/// 判断は [`banto_desktop_tray::notify_first_hide_once`]。
const TRAY_HINT: TrayHint = TrayHint {
    flag_file_name: "tray-hint-shown",
    title: "Banto Hub",
    body: "Banto Hub はタスクトレイで動作を続けます。",
};

/// 状態変化をトレイの tooltip とメニューへ反映する（[`watch_collection_status`]
/// から毎回呼ばれる、デスクトップホスト専用）。tray が未`manage`のうちに
/// 呼ばれた場合は何もしない - `run`のセットアップ順（tray 構築 → 監視 spawn）
/// により通常は起こらないが、`try_state`で防御的に扱う。
fn apply_status_to_tray(app: &AppHandle, status: &RuntimeStatus) {
    let Some(tray) = app.try_state::<TrayIcon<Wry>>() else {
        return;
    };
    let _ = tray.set_tooltip(Some(tray_status::tooltip_text(status)));
    if let Ok(menu) = build_desktop_tray_menu(app, Some(status)) {
        let _ = tray.set_menu(Some(menu));
    }
}

/// `CollectionController::subscribe_status()` を購読し続け、変化のたびに
/// トレイへ反映するバックグラウンドタスク（実装指示「状態更新は
/// `tauri::async_runtime::spawn`で`status_rx.changed().await`ループ」）。
///
/// ループの最初に一度現在値を適用してから待つのは、tray 構築から
/// このタスクの起動までの間に状態が変わっていた場合でも取りこぼさない
/// ための防御（`run`の呼び出し順自体は tray 構築後にこれを spawn するため
/// 通常は起こらない）。`watch::Receiver::changed`が`Err`を返すのは
/// `CollectionController`（延いては`RunningHub`）が破棄された場合だけで、
/// それはプロセス終了処理中、または[`retry_startup`]がデスクトップから
/// 他のホストへ切り替えて`RunningHub`を`shutdown`した場合のみ - いずれも
/// ループを抜けてタスクを終える。
async fn watch_collection_status(app: AppHandle, controller: Arc<CollectionController>) {
    let mut status_rx = controller.subscribe_status();
    loop {
        let status = status_rx.borrow().clone();
        apply_status_to_tray(&app, &status);
        if status_rx.changed().await.is_err() {
            break;
        }
    }
}

/// `src/main.rs` から呼ばれる唯一の公開エントリポイント。
pub fn run() {
    tauri::Builder::default()
        // 設計 §4.2「単一インスタンス」: 第二インスタンスは既存ウィンドウを
        // 前面化して自身は終了する。single-instance プラグインは公式に
        // 「他プラグインより先に登録すること」が推奨されているため、
        // `.plugin(...)` の最初の呼び出しにする。
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main_window(app);
        }))
        // T16-1: トレイ「アプリを終了」の確認ダイアログ用（実装指示 3.）。
        .plugin(tauri_plugin_dialog::init())
        // T16-1: `×` でトレイ格納した初回だけの継続通知用（実装指示 4.、UX-7）。
        .plugin(tauri_plugin_notification::init())
        // #514: プレースホルダの読み込みが終わったら、起動判定が先に書いた
        // 文言（`AppState::status_text`）を書き直す（[`reapply_status_text`]）。
        .on_page_load(|webview, payload| {
            reapply_status_text(webview, payload.url(), payload.event())
        })
        .invoke_handler(tauri::generate_handler![
            host_switch_ipc::host_switch_status,
            host_switch_ipc::switch_to_service,
            host_switch_ipc::switch_to_desktop,
            host_switch_ipc::set_service_autostart,
            // #661 661-5: 「設定モードに戻す」（`relax-only` → 戻らなければ昇格ヘルパーの修復）。
            host_switch_ipc::restore_setting_mode,
            // #706 レビュー: Fallback の窓の [再試行]。
            host_switch_ipc::retry_startup,
            // S6（docs/banto-hub-external-db-design.md §5.5・§7 row S6）:
            // 「サービス」一覧の BantoHubSink 側（`sink_service_ipc`のモジュール
            // doc参照）。
            sink_service_ipc::sink_service_status,
            sink_service_ipc::sink_service_start,
            sink_service_ipc::sink_service_stop,
        ])
        // #392 A2: 「アプリを終了」の確認中・終了処理中の 2 回目を無視する。
        .manage(QuitGate::new())
        // #586: 終了の後始末（トレイの「アプリを終了」と `RunEvent::Exit`）を
        // プロセスで 1 回に限り、2 つ目には完了を待たせる（[`exit_cleanup`]）。
        .manage(ExitOnce(
            ExitCleanupOnce::<HubExitReport, ExitTrigger>::new(),
        ))
        .manage(AppState {
            hub: HubSlot::new(),
            controller: StdMutex::new(None),
            view: StdMutex::new(ShellView::Fallback(FallbackInfo {
                scm_state: None,
                health: None,
                desktop_error: None,
                cause: FallbackCause::General,
            })),
            view_generation: launch_gate::ViewGeneration::default(),
            // T16-2（実装指示 4.）: Operators 判定はプロセス起動時に一度だけ
            // 確定する（[`AppState::can_operate_service`]のフィールド doc参照）。
            // T16-2 第二スライス: Operators**または** Windows ローカル
            // Administrators のメンバーなら操作可能にする
            // （`is_current_process_admin`のモジュール doc「T16-2 第二
            // スライスで追加した is_current_process_admin」節、desktop-plan
            // §8.3 の意図）。
            can_operate_service: is_current_process_operator().unwrap_or(false)
                || is_current_process_admin().unwrap_or(false),
            status_text: StdMutex::new(None),
            placeholder_url: StdMutex::new(None),
            pending_log: StdMutex::new(Vec::new()),
        })
        .setup(|app| {
            let window = app
                .get_webview_window(MAIN_WINDOW_LABEL)
                .expect("tauri.conf.json の app.windows にメインウィンドウを定義済み");

            // CloseRequested（×ボタン）はプロセスを終了せずトレイへ格納
            // するだけ（実装指示「CloseRequested -> prevent_close + hide」）。
            // T16-1 で初回だけの継続通知を追加（UX-7）。#392 A2 で
            // ChronoGazer と共有の `banto-desktop-tray` へ移した。
            banto_desktop_tray::hide_to_tray_on_close(&window, TRAY_HINT);

            // T16-2: サービス検出 → 接続 / デスクトップ起動 / fallback の
            // 判定（このモジュール doc「起動シーケンス」節参照）。
            // サービスが止まったときの navigate 先（プレースホルダ）。#706 実機確認: ここで
            // `window.url()` を読むと、最初の読み込みが確定する前なので `about:blank` が返る
            // （WebView2 の Source）。窓の設定から組み立て、読み込みの完了で覚え直す
            // （[`reapply_status_text`]）。
            let use_https = app
                .config()
                .app
                .windows
                .iter()
                .find(|config| config.label == MAIN_WINDOW_LABEL)
                .is_some_and(|config| config.use_https_scheme);
            *app.state::<AppState>()
                .placeholder_url
                .lock()
                .expect("placeholder_url mutex poisoned") =
                Some(launch_gate::placeholder_url(cfg!(windows), use_https));
            let target = expected_probe_target();
            let slot = Arc::clone(&app.state::<AppState>().hub);
            let outcome = decide_startup(&target, &slot);
            let desktop_started = apply_startup_outcome(app.handle(), &window, outcome);

            // トレイ（T16-1/T16-2、desktop-plan §9.9）: 起動直後の見え方
            // （`state.view`、`apply_startup_outcome`が確定済み）に応じた
            // メニュー・tooltip で構築する。以後の変化は、デスクトップなら
            // 監視タスク（[`watch_collection_status`]）、それ以外なら
            // [`retry_startup`]（→ [`sync_tray`]）が反映する。
            let state = app.state::<AppState>();
            let view = state.view.lock().expect("view mutex poisoned").clone();
            let (tooltip, tray_menu) = tray_content_for_view(
                app.handle(),
                &view,
                state.can_operate_service,
                desktop_started.as_ref().map(|(status, _)| status),
            )?;
            // 左クリック = 窓を戻す、右クリック = メニュー（オーナー決定
            // 2026-10-12、ChronoGazer と共通）。メインの窓は `tauri.conf.json` で
            // 定義済みで、ホストの種類（デスクトップ／サービス接続／fallback）に
            // よらず存在し、閉じても破棄されず隠れるだけ - 戻す窓が無い状態は
            // 無い。万一無ければ [`banto_desktop_tray::show_window`] は何もせず、
            // メニューは右クリックで出る。
            let tray_builder = TrayIconBuilder::new()
                .icon(
                    app.default_window_icon()
                        .cloned()
                        .expect("tauri.conf.json の bundle.icon で既定アイコンを設定済み"),
                )
                .tooltip(tooltip)
                .menu(&tray_menu)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "open" => show_main_window(app),
                    "stop" => stop_collection(app),
                    "stop_service" => stop_service(app),
                    "start_service" => start_service(app),
                    "retry" => retry_startup(app),
                    "restore_setting" => restore_setting(app),
                    "quit" => confirm_quit(app),
                    _ => {}
                });
            let tray =
                banto_desktop_tray::restore_window_on_left_click(tray_builder, show_main_window)
                    .build(app.handle())?;
            // `TrayIcon`は参照カウント式で、最後の1つが drop されると消える
            // （tauri本体のdoc comment参照） - アプリの生存期間中ずっと
            // 保持するためだけに managed state へ入れる
            // （[`apply_status_to_tray`]・[`sync_tray`]が`try_state`で読み出す）。
            app.manage(tray);

            // デスクトップホストとして起動できていれば、状態購読を開始する
            // （[`watch_collection_status`]のモジュール doc 参照 - tray が
            // 無い間に状態変化が届いても適用先が無いため、トレイ構築後に
            // spawn する）。サービス接続時・fallback 時は controller が
            // 無いため何もしない。
            if let Some((_status, controller)) = desktop_started {
                let app_handle = app.handle().clone();
                tauri::async_runtime::spawn(watch_collection_status(app_handle, controller));
            }

            // #661 661-5: サービスに接続している間・Fallback の間、サービスが止まったら
            // （終了コード 4・5 を含む）合う窓へ移る（Hub は起動しない）。
            #[cfg(windows)]
            spawn_service_watch(app.handle().clone());

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            // #586: `RunEvent::Exit` で終了の後始末（[`exit_cleanup`]）を通す。
            // トレイの「アプリを終了」は `app.exit(0)` の前に後始末を済ませて
            // あるので、ここは済んだ結果を受け取るだけ（2 回は走らない）。
            // **OS のサインアウト・シャットダウン**では tao が `WM_ENDSESSION`
            // でここを呼び、戻った直後に `std::process::exit` するので、窓を
            // 隠したまま（トレイ常駐中）でも、デスクトップホストの Hub は
            // ここで `RunningHub::shutdown`（収集の停止・最終 flush）を通る。
            // 以前は `.run(context)` で `Exit` を扱っておらず、OS の終了では
            // shutdown を通らずに落ちていた。待つ上限は [`OS_EXIT_BUDGET`]。
            //
            // **テストの範囲**: 後始末の中身（置き場所を閉じる → Hub を止める →
            // 進行中の起動・停止を待つ、1 回に限る、予算）は `hub_slot` の
            // テストと `banto_desktop_tray` の `exit_once` のテストで押さえて
            // ある。「OS の終了 → Tauri のイベントループ → このコールバック」の
            // 経路そのものは単体テストでは再現できない（ChronoGazer の
            // `RunEvent::Exit` 腕のコメントと同じ事情）ので、実機確認の領域。
            if let tauri::RunEvent::Exit = event {
                exit_cleanup(app_handle, ExitTrigger::RunEventExit);
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settled() -> HubExitReport {
        HubExitReport {
            stopped_hub: true,
            settled: true,
        }
    }

    fn unsettled() -> HubExitReport {
        HubExitReport {
            stopped_hub: true,
            settled: false,
        }
    }

    fn wait_line(
        report: &ExitCleanupReport<HubExitReport, ExitTrigger>,
        trigger: ExitTrigger,
    ) -> Option<ExitLogLine> {
        wait_log_line(trigger, trigger.budget(), report)
    }

    /// 本体の結果の文言: 完了は info、期限までに切替が落ち着かなかったは error。
    /// どちらも**本体を走らせた経路**の名で出る。
    #[test]
    fn the_body_line_names_the_origin_and_distinguishes_unsettled() {
        let ExitLogLine::Info(done) = body_log_line(ExitTrigger::TrayQuit, &settled()) else {
            panic!("settled is info");
        };
        assert!(done.contains("完了しました") && done.contains("トレイの終了"));
        let ExitLogLine::Error(late) = body_log_line(ExitTrigger::TrayQuit, &unsettled()) else {
            panic!("unsettled is an error");
        };
        assert!(late.contains("終わりませんでした") && late.contains("トレイの終了"));
    }

    /// 待ちの文言（実機確認の 1・2）: 完了を受け取った側は何も出さない。トレイが
    /// 打ち切られた後に来た `Exit` は自分の上限（5 秒）を名乗らず先行の期限切れを
    /// 出す。自分の予算で切ったときだけその上限を名乗る。
    #[test]
    fn wait_lines_name_only_what_the_call_did() {
        assert_eq!(
            wait_line(
                &ExitCleanupReport::RanWork(ExitCleanupOutcome::Finished(settled())),
                ExitTrigger::TrayQuit
            ),
            None
        );
        assert_eq!(
            wait_line(
                &ExitCleanupReport::ReceivedPrior {
                    origin: ExitTrigger::TrayQuit,
                    result: settled()
                },
                ExitTrigger::RunEventExit
            ),
            None
        );
        let Some(ExitLogLine::Error(own)) = wait_line(
            &ExitCleanupReport::RanWork(ExitCleanupOutcome::TimedOut),
            ExitTrigger::TrayQuit,
        ) else {
            panic!("the tray quit logs its own time-out");
        };
        assert!(own.contains("この経路の上限15秒"));
        let Some(ExitLogLine::Error(expired)) = wait_line(
            &ExitCleanupReport::PriorExpired {
                origin: ExitTrigger::TrayQuit,
            },
            ExitTrigger::RunEventExit,
        ) else {
            panic!("the prior expiry is logged");
        };
        assert!(!expired.contains("この経路の上限") && !expired.contains("5秒"));
        assert!(expired.contains("先行") && expired.contains("トレイの終了"));
        let Some(ExitLogLine::Error(cut)) = wait_line(
            &ExitCleanupReport::OwnBudgetExpired {
                origin: ExitTrigger::TrayQuit,
            },
            ExitTrigger::RunEventExit,
        ) else {
            panic!("the own-budget cut is logged");
        };
        assert!(cut.contains("上限5秒") && cut.contains("トレイの終了"));
    }

    /// 数えられる記録先。`slow_body` なら、本体の行を記録する前に少し止まる
    /// （記録が遅い先行を作る）。
    fn counting_sink(slow_body: bool) -> (ExitSink, Arc<StdMutex<Vec<ExitLogLine>>>) {
        let lines = Arc::new(StdMutex::new(Vec::new()));
        let sink_lines = Arc::clone(&lines);
        let sink: ExitSink = Arc::new(move |line| {
            let is_body = match &line {
                ExitLogLine::Info(text) => text.contains("完了しました"),
                ExitLogLine::Error(text) => text.contains("終わりませんでした"),
            };
            if slow_body && is_body {
                std::thread::sleep(Duration::from_millis(300));
            }
            sink_lines.lock().unwrap().push(line);
        });
        (sink, lines)
    }

    /// 普通のトレイ終了: 完了ログは 1 行だけ。後から来る `Exit` は受け取るだけで
    /// 出さない。
    #[test]
    fn a_plain_tray_quit_logs_one_completion_line() {
        let once = ExitCleanupOnce::<HubExitReport, ExitTrigger>::new();
        let slot = HubSlot::<u32>::new();
        let (sink, lines) = counting_sink(false);
        slot.close();
        run_exit_cleanup(
            &once,
            Arc::clone(&slot),
            ExitTrigger::TrayQuit,
            Duration::from_secs(5),
            |_| {},
            Arc::clone(&sink),
        );
        let after = run_exit_cleanup(
            &once,
            slot,
            ExitTrigger::RunEventExit,
            Duration::from_secs(5),
            |_| {},
            sink,
        );
        assert!(matches!(after, ExitCleanupReport::ReceivedPrior { .. }));
        let lines = lines.lock().unwrap();
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(matches!(&lines[0], ExitLogLine::Info(text) if text.contains("トレイの終了")));
    }

    /// オーナーレビュー P2: 先行（トレイ）の記録が遅くても、結果を先に受け取った
    /// 後続（OS の終了）が戻る時点で、本体の記録は済んでいる。記録を結果の
    /// 公開より後（呼び出し元）に置く作りだと、後続が戻った時点で 0 行になる。
    #[test]
    fn the_body_is_recorded_before_a_waiting_caller_gets_the_result() {
        let once = Arc::new(ExitCleanupOnce::<HubExitReport, ExitTrigger>::new());
        let slot = HubSlot::<u32>::new();
        slot.close();
        let (sink, lines) = counting_sink(true);
        let tray = {
            let (once, slot, sink) = (Arc::clone(&once), Arc::clone(&slot), Arc::clone(&sink));
            std::thread::spawn(move || {
                run_exit_cleanup(
                    &once,
                    slot,
                    ExitTrigger::TrayQuit,
                    Duration::from_secs(5),
                    |_| {},
                    sink,
                )
            })
        };
        while !once.has_started() {
            std::thread::sleep(Duration::from_millis(5));
        }
        let os = run_exit_cleanup(
            &once,
            slot,
            ExitTrigger::RunEventExit,
            Duration::from_secs(5),
            |_| {},
            Arc::clone(&sink),
        );
        assert!(matches!(os, ExitCleanupReport::ReceivedPrior { .. }));
        // 後続が結果を受け取って戻った時点で、本体の記録はもうある。
        assert_eq!(lines.lock().unwrap().len(), 1);
        tray.join().unwrap();
        assert_eq!(lines.lock().unwrap().len(), 1, "no duplicate line");
    }

    /// 期限までに切替が落ち着かなかった診断は、後続が先に戻っても少なくとも 1 回
    /// 残り、本体の分は 1 回だけ。
    #[test]
    fn an_unsettled_diagnostic_survives_a_waiter_returning_first() {
        let once = Arc::new(ExitCleanupOnce::<HubExitReport, ExitTrigger>::new());
        let slot = HubSlot::<u32>::new();
        // 進行中の起動を 1 つ持ったままにして、落ち着かない（settled: false）ようにする。
        let _ticket = slot.begin_start().expect("open slot accepts a start");
        slot.close();
        let (sink, lines) = counting_sink(true);
        let tray = {
            let (once, slot, sink) = (Arc::clone(&once), Arc::clone(&slot), Arc::clone(&sink));
            std::thread::spawn(move || {
                run_exit_cleanup(
                    &once,
                    slot,
                    ExitTrigger::TrayQuit,
                    Duration::from_millis(150),
                    |_| {},
                    sink,
                )
            })
        };
        while !once.has_started() {
            std::thread::sleep(Duration::from_millis(5));
        }
        run_exit_cleanup(
            &once,
            slot,
            ExitTrigger::RunEventExit,
            Duration::from_secs(5),
            |_| {},
            Arc::clone(&sink),
        );
        let seen = lines.lock().unwrap().len();
        assert!(
            seen >= 1,
            "a diagnostic is already recorded when the waiter returns"
        );
        tray.join().unwrap();
        // 呼び出し元の待ちが先に切れても、本体は裏で続いて自分の診断を 1 回記録する
        // （待ちの打ち切りの行とは別）。記録されるまで上限付きで待つ。
        let give_up = std::time::Instant::now() + Duration::from_secs(5);
        let count_body_errors = || {
            lines
                .lock()
                .unwrap()
                .iter()
                .filter(|line| {
                    matches!(line, ExitLogLine::Error(text) if text.contains("終わりませんでした"))
                })
                .count()
        };
        while count_body_errors() == 0 && std::time::Instant::now() < give_up {
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(count_body_errors(), 1, "{:?}", lines.lock().unwrap());
    }

    /// 「アプリを終了」の確認文は収集中かどうかで変わり、収集中の文言は
    /// 停止と flush を明示する。確認の途中で収集が始まっていた場合は共有の
    /// `QuitGate` が `AskAgain` を返し、`prompt_quit(app, true)` が収集中の
    /// 文言で出し直す（#392 A2）。そのつなぎの文言と状態遷移を確かめる。
    #[test]
    fn quit_prompt_message_depends_on_collecting() {
        assert!(quit_prompt_message(true).contains("収集を停止"));
        assert!(quit_prompt_message(true).contains("flush"));
        assert!(!quit_prompt_message(false).contains("収集を停止"));
        assert_ne!(quit_prompt_message(true), quit_prompt_message(false));
    }

    /// 印を立てた時点で忙しかった（= 確認中に収集が始まっていた）ことにする
    /// 偽の受付口。
    struct BusyAdmission;

    impl QuitAdmission for BusyAdmission {
        fn begin_quit_confirm(&self) -> bool {
            true
        }
        fn cancel_quit_confirm(&self) {}
    }

    /// 暇な文言で同意 → 待つ間に収集が始まった: 出し直し（Confirming のまま
    /// なので 2 回目の「アプリを終了」は重ならない）、収集中の文言で同意すれば
    /// 終了処理へ進み、その後の「アプリを終了」は無視される。印の上げ下げの
    /// 全経路は `banto_desktop_tray` のテスト、開始との直列化は
    /// `banto_hub_core::controller` のテストで固定している。
    #[test]
    fn quit_gate_asks_again_when_collection_started_while_confirming() {
        let gate = QuitGate::new();
        assert_eq!(gate.request(), QuitRequestDecision::ShowPrompt);
        assert_eq!(
            gate.answer(true, false, &BusyAdmission),
            QuitAnswerDecision::AskAgain
        );
        assert_eq!(gate.request(), QuitRequestDecision::AlreadyConfirming);
        assert_eq!(
            gate.answer(true, true, &BusyAdmission),
            QuitAnswerDecision::Proceed
        );
        assert_eq!(gate.request(), QuitRequestDecision::AlreadyQuitting);
    }

    /// controller が無い（サービス接続中・fallback）ときは、このプロセスに
    /// 止める収集が無いので、暇な文言の同意でそのまま進む。
    #[test]
    fn without_a_controller_the_admission_is_never_busy() {
        let admission = ControllerQuitAdmission(None);
        assert!(!admission.begin_quit_confirm());
        admission.cancel_quit_confirm();
        let gate = QuitGate::new();
        assert_eq!(gate.request(), QuitRequestDecision::ShowPrompt);
        assert_eq!(
            gate.answer(true, false, &admission),
            QuitAnswerDecision::Proceed
        );
    }
    use banto_hub_core::profile_paths::resolve_profile_paths_from_env;

    /// #505: 窓（WebView）の CSP は、Hubの応答の CSP（banto の
    /// `CONTENT_SECURITY_POLICY`、#500）に Tauri の IPC（`connect-src` の
    /// `ipc: http://ipc.localhost`、`TAURI_IPC_CONNECT_SRC`）を足しただけのものに
    /// する（admin-template の `tauri.conf.json` と同じ形）。この CSP が掛かるのは `frontendDist` の
    /// プレースホルダ（`ui/index.html`）だけで、navigate した後の Hub の画面には
    /// Hub の応答の CSP（loopback の接続元には同じく IPC を足したもの、
    /// `banto_hub_core::runtime` の `hub_security_headers`）が掛かる。banto が応答の CSP を変えたら、ここが落ちて窓の側を
    /// 揃え忘れないようにする。期待値は banto の `SecurityHeaders` で組み立てる
    /// （banto v5.0.0。文字列の置き換えはしない）。
    #[test]
    fn window_csp_is_the_served_csp_plus_tauri_ipc() -> Result<(), banto_server::InvalidCspSource> {
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).expect("tauri.conf.json");
        let window_csp = conf["app"]["security"]["csp"]
            .as_str()
            .expect("app.security.csp is set");

        let expected = banto_server::SecurityHeaders::new()
            .extra_connect_src(banto_server::TAURI_IPC_CONNECT_SRC)?
            .content_security_policy();
        assert_ne!(expected, banto_server::CONTENT_SECURITY_POLICY);
        assert_eq!(window_csp, expected);
        Ok(())
    }

    /// [`build_hub_config_from_env`] は3ホスト共通の関数（T17-1、
    /// `apps/banto-hub/core/src/profile_paths.rs`）を呼ぶだけになった -
    /// このシェル crate 固有の複製ロジックは無い。`HubRuntime::start`
    /// 自体の start→shutdown ラウンドトリップ（collection が Stopped の
    /// まま起動することを含む）は
    /// `apps/banto-hub/core/src/runtime.rs::tests::start_local_addr_then_shutdown_round_trip`
    /// が既にカバーしている（このシェル crate は Tauri アプリの外形からは
    /// 単体テストできないため）。ここでは`HubHostKind::Shell`を渡した結果が
    /// 共通関数の読み取りロジック（キーごとに「未設定なら profile 既定値」
    /// 「設定されていれば env の値を反映」）どおりになっていることだけを
    /// 確認する。
    ///
    /// 2026-08-09 レビュー指摘で修正: 以前は「5キーとも未設定である」ことを
    /// 前提にしていたため、CI や開発者環境で `BANTO_DB` 等がたまたま
    /// 設定されていると flaky に失敗していた。ここではキーごとに実際の
    /// 環境変数の有無を読み、その分岐に応じたアサーションを行う - どの
    /// 実行環境でも決定的に成功する。`std::env::set_var`/`remove_var` に
    /// よるテスト内での書き換えは、並列実行される他テストのプロセス全体の
    /// 環境を巻き込む副作用があるため使わない（T17-1 でも同じ方針を維持 -
    /// `resolve_profile_paths_from_env()`を「期待値」として直接呼ぶことで、
    /// env を書き換えずに profile 既定パスとの一致を確認できる）。
    #[test]
    fn build_hub_config_from_env_reflects_or_defaults_each_key() {
        let config = build_hub_config_from_env(HubHostKind::Shell, hub_drivers());
        let expected_paths = resolve_profile_paths_from_env();

        match std::env::var("BANTO_DB") {
            Ok(value) => assert_eq!(config.db_path, value),
            Err(_) => assert_eq!(
                config.db_path,
                expected_paths.db_path.to_string_lossy().into_owned()
            ),
        }

        match std::env::var("BANTO_ALLOW_SETUP") {
            Ok(value) => assert_eq!(config.allow_setup, value == "1"),
            Err(_) => assert!(!config.allow_setup),
        }

        match std::env::var("PORT")
            .ok()
            .and_then(|value| value.parse::<u16>().ok())
        {
            Some(port) => assert_eq!(config.port_override, Some(port)),
            None => assert_eq!(config.port_override, None),
        }

        match std::env::var("BANTO_BIND") {
            Ok(value) => assert_eq!(config.bind_override, Some(value)),
            Err(_) => assert_eq!(config.bind_override, None),
        }

        // T17-1（P1）: `BANTO_HUB_DATA`未設定時も、旧既定`None`ではなく
        // profile の`data_dir`（絶対パス）で必ず`Some`になる。
        match std::env::var("BANTO_HUB_DATA") {
            Ok(value) => {
                assert_eq!(
                    config.data_dir_override,
                    Some(std::path::PathBuf::from(value))
                )
            }
            Err(_) => assert_eq!(
                config.data_dir_override,
                Some(expected_paths.data_dir.clone())
            ),
        }

        assert_eq!(config.profile_id, expected_paths.profile_id);
        assert_eq!(config.host_kind, HubHostKind::Shell);
        assert!(!config.skip_profile_lock);
    }

    /// [`js_string_literal`] は [`show_startup_error`]・[`render_fallback`]
    /// が `window.eval` へ渡す JS ソースの中に埋め込まれる - バックスラッシュ・
    /// シングルクォート・改行を潰さないと、生成した JS 自体が構文エラーに
    /// なる（エラー文言は `HubStartError` の `Display` 由来で改行を含み
    /// 得る）。
    #[test]
    fn js_string_literal_escapes_quotes_and_newlines() {
        let literal = js_string_literal("banto-hub: line1\nline2 it's \\ok\\");
        assert_eq!(literal, "'banto-hub: line1\\nline2 it\\'s \\\\ok\\\\'");
    }

    /// #514: [`status_text_script`]は、読み込み中の文書では`DOMContentLoaded`を
    /// 待ってから書き込み、要素が無い文書では何もしない JS を返す。文言は
    /// [`js_string_literal`]でエスケープした形で一度だけ埋め込まれ、生の
    /// 改行が JS ソースへ漏れない（一行の式になる）。
    #[test]
    fn status_text_script_waits_for_dom_and_embeds_escaped_message() {
        let message = "起動できませんでした。\nit's \\ok";
        let script = status_text_script(&PlaceholderPage {
            message: message.to_string(),
            actions: vec![launch_gate::ACTION_RETRY],
        });

        assert_eq!(script.matches(&js_string_literal(message)).count(), 1);
        assert!(!script.contains('\n'), "生の改行を含まない: {script}");
        assert!(script.contains("document.readyState === 'loading'"));
        assert!(script.contains("addEventListener('DOMContentLoaded', apply, { once: true })"));
        assert!(script.contains("document.getElementById('banto-hub-status')"));
        assert!(script.contains("if (el) { el.textContent = message; }"));
        assert!(script.starts_with("(function () {"));
        assert!(script.ends_with("})();"));
        // #706 レビュー: 操作のボタンは JSON で渡し、プレースホルダの描画があるときだけ呼ぶ。
        assert!(script.contains(
            r#"var actions = [{"id":"retry","label":"再試行","command":"retry_startup"}];"#
        ));
        assert!(script.contains("if (typeof window.__bantoHubRenderActions === 'function')"));
    }

    /// #706 レビュー（Codex P2）: Fallback の窓（プレースホルダ）にも、理由に合う操作（トレイと
    /// 同じ集合）を出す。
    #[test]
    fn fallback_page_offers_the_actions_that_fit_the_cause() {
        let ids = |info: &FallbackInfo, can_operate| -> Vec<&'static str> {
            fallback_page(info, can_operate)
                .actions
                .iter()
                .map(|action| action.id)
                .collect()
        };
        let left_tight = FallbackInfo {
            scm_state: Some(ScmState::Stopped),
            health: None,
            desktop_error: None,
            cause: FallbackCause::OperationStopped { left_tight: true },
        };
        assert_eq!(
            ids(&left_tight, true),
            ["start_service", "restore_setting", "retry"]
        );
        assert!(fallback_page(&left_tight, true)
            .message
            .contains("運用モードのまま残っています"));
        // 反証: 止まる途中（StopPending）は開始を出さず、一般の窓は再試行だけ。
        let stopping = FallbackInfo {
            scm_state: Some(ScmState::StopPending),
            health: None,
            desktop_error: None,
            cause: FallbackCause::ServiceLost,
        };
        assert_eq!(ids(&stopping, true), ["retry"]);
    }

    /// T16-2: [`expected_probe_target`]は`resolve_profile_paths_from_env`と
    /// 同じ profile-id/root を返す（port だけ`PORT`/既定値の追加ロジックを
    /// 持つ）ことを確認する - 実際の env の有無に依存しない形で検証する
    /// （上の`build_hub_config_from_env_reflects_or_defaults_each_key`と
    /// 同じ理由で env を書き換えない）。
    #[test]
    fn expected_probe_target_matches_resolved_profile_paths() {
        let target = expected_probe_target();
        let expected_paths = resolve_profile_paths_from_env();
        assert_eq!(target.root, expected_paths.root);
        assert_eq!(target.profile_id, expected_paths.profile_id);
        match std::env::var("PORT")
            .ok()
            .and_then(|value| value.parse::<u16>().ok())
        {
            Some(env_port) => assert_eq!(target.port, env_port),
            None => assert_eq!(target.port, banto_hub_core::settings::DEFAULT_PORT),
        }
        // `resolve_navigate_host`が返しうる値と一致する（env を書き換えない
        // 方針は上と同じ - 実際の`BANTO_BIND`の有無に応じて分岐する）。
        assert_eq!(target.host, resolve_navigate_host());
    }

    /// T16-2 第二スライス（実装指示 3.）: `BANTO_BIND`が未設定・空文字列・
    /// 全インターフェース bind（`0.0.0.0`/`::`）のいずれでも loopback-safe な
    /// 既定`127.0.0.1`へ読み替えることを、実際の env を書き換えずに確認する
    /// （`resolve_navigate_host`自体は`BANTO_BIND`しか読まないので、
    /// 現在の env の値をそのまま期待値の計算に使う）。
    #[test]
    fn resolve_navigate_host_falls_back_to_loopback_for_wildcard_or_unset() {
        let resolved = resolve_navigate_host();
        match std::env::var("BANTO_BIND") {
            Ok(value) if !value.is_empty() && value != "0.0.0.0" && value != "::" => {
                assert_eq!(resolved, value);
            }
            _ => assert_eq!(resolved, "127.0.0.1"),
        }
    }

    #[test]
    fn format_host_port_wraps_ipv6_literals_in_brackets() {
        assert_eq!(format_host_port("127.0.0.1", 8722), "127.0.0.1:8722");
        assert_eq!(format_host_port("::1", 8722), "[::1]:8722");
    }

    /// T16-2 fallback 画面本文（実装指示 2.）の組み立てを確認する - SCM 状態・
    /// health・起動エラーの各行が含まれ、Operators 可否で最終行の文言が
    /// 変わることを検証する。
    #[test]
    fn fallback_message_includes_scm_health_and_desktop_error_lines() {
        let info = FallbackInfo {
            scm_state: Some(ScmState::Stopped),
            health: Some(HealthOutcome::Unreachable),
            desktop_error: Some("banto-hub: サーバーの起動に失敗しました: port in use".to_string()),
            cause: FallbackCause::General,
        };
        let message = fallback_message(&info, true);
        assert!(message.contains("サービス状態: Stopped"));
        assert!(message.contains("応答がありません"));
        assert!(message.contains("port in use"));
        assert!(message.contains("タスクトレイから"));

        let message_without_permission = fallback_message(&info, false);
        assert!(message_without_permission.contains("BantoHub Operators"));
    }

    #[test]
    fn fallback_message_reports_unknown_scm_state_when_not_queried() {
        let info = FallbackInfo {
            scm_state: None,
            health: None,
            desktop_error: None,
            cause: FallbackCause::General,
        };
        assert!(fallback_message(&info, true).contains("確認できませんでした"));
    }

    /// #661 661-5: 理由のある窓は見出しと案内が先、診断の行が後。一般の窓は従来のまま。
    #[test]
    fn fallback_message_leads_with_the_cause_page() {
        let info = FallbackInfo {
            scm_state: Some(ScmState::Stopped),
            health: None,
            desktop_error: None,
            cause: FallbackCause::OperationStopped { left_tight: true },
        };
        let message = fallback_message(&info, true);
        assert!(
            message.starts_with(
                "運用モードのまま残っています
"
            ),
            "{message}"
        );
        assert!(message.contains("設定モードに戻す"));
        assert!(message.contains("切り替えの間は収集が止まります"));
        assert!(message.contains("サービス状態: Stopped"));
        let general = FallbackInfo {
            cause: FallbackCause::General,
            ..info
        };
        assert!(fallback_message(&general, true).starts_with("サービス状態: Stopped"));
    }

    /// #661 661-5: 観測は profile を作らない・書かない。Windows では緩んだ（継承の既定の）
    /// フォルダは Desktop を断らず、無いフォルダは `Absent`。非 Windows は観測しない。
    #[test]
    fn observing_the_profile_never_blocks_a_loose_or_missing_profile() {
        let base = std::env::temp_dir().join(format!("banto-hub-661-5-{}", std::process::id()));
        std::fs::create_dir_all(&base).expect("作れる");
        let missing = base.join("profiles").join("default");
        let observed = launch_gate::observe_profile_mode(&missing);
        assert!(observed.blocks_desktop().is_none(), "{observed:?}");
        assert!(!missing.exists(), "観測で作らない");
        let present = launch_gate::observe_profile_mode(&base);
        let _ = std::fs::remove_dir_all(&base);
        assert!(present.blocks_desktop().is_none(), "{present:?}");
        #[cfg(windows)]
        assert_eq!(
            observed,
            ProfileModeObservation::Shape(banto_hub_core::profile_mode::ProfileShape::Absent)
        );
        #[cfg(not(windows))]
        assert_eq!(observed, ProfileModeObservation::NotObserved);
    }
}
