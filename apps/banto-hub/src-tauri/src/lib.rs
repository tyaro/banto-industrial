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
//! ヘッドレス bin `banto-hub`（`apps/banto-hub/core/src/bin/banto-hub.rs`）
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
//! トレイ「管理画面を終了」/「終了」 -> 確認ダイアログ -> (デスクトップ
//!          ホストなら) RunningHub::shutdown() -> app.exit(0)
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
//! トリガーにするか」という composition だけ - `apps/banto-hub/core/src/bin/
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
//! - `×` でトレイ格納した初回だけの OS 通知（[`maybe_notify_first_tray_hide`]、
//!   `tauri-plugin-notification`）。既読フラグは `app_data_dir` 配下のファイルへ
//!   永続化する（実装指示「できれば永続化」）。
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
//!   `shutdown`し、`is_stopped`は hub 未保有で`true`を返す
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

use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};
#[cfg(windows)]
use std::time::Duration;

use banto_hub_core::controller::{CollectionController, RuntimeStatus};
#[cfg(windows)]
use banto_hub_core::host_switch::{
    DesktopHostControl, DesktopHostError, HostKind, HostSwitchConfig, HostSwitchEngine,
    StepOutcome, SwitchCommand, SwitchError,
};
use banto_hub_core::http_hub_health::HttpHubHealthProbe;
use banto_hub_core::hub_health::{HealthOutcome, HubHealthProbe};
use banto_hub_core::profile_lock::HubHostKind;
use banto_hub_core::profile_paths::{build_hub_config_from_env, resolve_profile_paths_from_env};
use banto_hub_core::runtime::{HubRuntime, RunningHub};
use banto_hub_core::service_manager::ScmState;
#[cfg(windows)]
use banto_hub_core::service_manager::{ServiceManager, WindowsServiceManager};
use banto_hub_core::service_operators::{is_current_process_admin, is_current_process_operator};
use tauri::menu::{Menu, MenuBuilder, MenuItemBuilder};
use tauri::tray::{TrayIcon, TrayIconBuilder};
use tauri::{AppHandle, Manager, WebviewWindow, WindowEvent, Wry};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use tauri_plugin_notification::NotificationExt;
use tokio::sync::Mutex as AsyncMutex;

mod host_switch_ipc;
mod tray_status;

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
    Desktop(RunningHub),
    /// fallback 画面を表示する。
    Fallback(FallbackInfo),
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
}

/// `app.manage()` で保持するアプリ全体の唯一の可変状態。
pub(crate) struct AppState {
    /// 稼働中の Hub。デスクトップホストとして起動できた場合のみ`Some`
    /// （サービス接続時・fallback 時は`None` - [`show_fallback_ui`]・
    /// [`show_startup_error`]参照）。
    ///
    /// `tokio::sync::Mutex`（`std::sync::Mutex` ではない）を使うのは、
    /// [`RunningHub::shutdown`] 自体が `.await` を挟む非同期メソッドで、
    /// ロックを保持したまま `.await` する必要があるため
    /// （`apps/chronogazer/src-tauri`の`AppState::server`フィールドと同じ
    /// 理由 - あちらも同じ形の`AsyncMutex<Option<RunningServer>>>`）。
    hub: AsyncMutex<Option<RunningHub>>,
    /// T16-1: トレイの状態表示・「収集を停止」・終了確認の文言決定に使う
    /// controller ハンドル。`hub`とは別フィールドにしているのは、
    /// [`quit`]が`hub`の中身を`take()`して`shutdown()`へ渡した後は参照できなく
    /// なってよいのに対し、こちらは読み書きだけで `.await` を挟まないため
    /// `std::sync::Mutex`（`CollectionController`のメソッド自体が内部で
    /// 非同期処理するので、ここは同期ロックで足りる）で揃えたいため。
    /// デスクトップホストとして起動できなかった場合は`None`のまま。
    controller: StdMutex<Option<Arc<CollectionController>>>,
    /// T16-2: 現在シェルが表示している見え方（[`ShellView`]）。
    /// トレイメニューの再構築・「サービスを開始/停止」「再試行」の分岐に使う。
    pub(crate) view: StdMutex<ShellView>,
    /// T16-2: `is_current_process_operator().unwrap_or(false)`
    /// （実装指示 4.「Operators 委任」）。T16-2 第二スライスで
    /// `is_current_process_admin().unwrap_or(false)`との論理和になった
    /// （既知の gap「Operators ゲートがローカル管理者を誤って隠す」への
    /// 対応、`is_current_process_admin`のモジュール doc参照）。起動時に
    /// 一度確定し、以後は変えない - グループ参加・昇格状態の変更は
    /// プロセス再起動が前提（Windows のトークンはプロセス生存中に
    /// 再評価されないため、このスライスでは再評価の仕組みは作らない）。
    pub(crate) can_operate_service: bool,
}

/// JS 文字列リテラルへの最小エスケープ。[`show_startup_error`]・
/// [`render_fallback`] が プレースホルダ (`ui/index.html`) へ
/// [`WebviewWindow::eval`] でエラー文言を書き込むためだけに使う小さな
/// ヘルパー - この用途のためだけに `serde_json` 依存を増やさない
/// （実装指示「serde 等は最小限」）。埋め込む文字列は `HubStartError`
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

/// [`WebviewWindow::eval`]で`#banto-hub-status`の文言を書き換える共通ヘルパー
/// （[`show_startup_error`]・[`render_fallback`]の重複を避ける）。
fn set_status_text(window: &WebviewWindow, message: &str) {
    let js = format!(
        "document.getElementById('banto-hub-status').textContent = {};",
        js_string_literal(message)
    );
    if let Err(err) = window.eval(js) {
        eprintln!("banto-hub-shell: 画面表示の更新に失敗しました: {err}");
    }
}

/// 起動失敗時の表示（実装指示「起動失敗」節: 「サービス接続へ逃がさず、
/// エラーをログ/ダイアログ相当で示して終了操作だけ提供する」）。
///
/// [`decide_startup`]/[`retry_startup`]が想定していない例外的な失敗
/// （URL 組み立て失敗等、[`FallbackInfo`]を経由しない即時エラー表示）用 -
/// 通常の SCM/health 判定に基づく fallback は[`render_fallback`]を使う。
fn show_startup_error(window: &WebviewWindow, message: &str) {
    eprintln!("banto-hub-shell: {message}");
    let display =
        format!("起動できませんでした。\n{message}\n\nトレイメニューから終了してください。");
    set_status_text(window, &display);
}

/// T16-2 fallback 画面の本文（実装指示 2.「SCM 状態、service start/stop
/// actions gated by Operators membership」）を組み立てる純粋関数 - Tauri の
/// 型に依存しないため単体テストできる（[`tray_status`]と同じ方針）。
fn fallback_message(info: &FallbackInfo, can_operate: bool) -> String {
    let mut lines = Vec::new();
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
        "タスクトレイからサービスの開始・停止や再試行を操作できます。".to_string()
    } else {
        "サービス操作には 'BantoHub Operators' グループへの参加が必要です。トレイの「再試行」で状態を再確認できます。".to_string()
    });
    lines.join("\n")
}

/// [`decide_startup`]が`StartupOutcome::Fallback`を返したときに呼ぶ表示更新。
fn render_fallback(window: &WebviewWindow, info: &FallbackInfo, can_operate: bool) {
    set_status_text(window, &fallback_message(info, can_operate));
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
    if let Err(err) = window.navigate(url) {
        show_startup_error(
            window,
            &format!("管理画面への画面遷移に失敗しました: {err}"),
        );
    }
}

/// トレイ「画面を開く」・第二インスタンス起動時の前面化で共有する処理
/// （設計 §4.2/§4.3）。
fn show_main_window(app: &AppHandle) {
    let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) else {
        return;
    };
    let _ = window.unminimize();
    let _ = window.show();
    let _ = window.set_focus();
}

/// トレイ「アプリを終了」の実処理（確認ダイアログ通過後に [`confirm_quit`]
/// から呼ばれる - T16-0 時点はここが確認なしで直接メニューへ繋がっていた）。
///
/// 稼働中の Hub があれば [`RunningHub::shutdown`] を必ず待ってから
/// プロセスを終了する（実装指示「Exit / トレイ終了 -> RunningHub::shutdown()
/// -> app.exit」- 収集停止・tstore flush・broker セッション切断は
/// `RunningHub::shutdown`/`CollectionController` 側の既存責務で、ここで
/// 個別に呼び直すことはしない）。デスクトップホストとして起動できていない
/// 場合（サービス接続中・fallback 中）は`state.hub`が`None`のままなので、
/// そのままプロセスを終了する - **サービス自体は停止しない**（実装指示の
/// 対象外、サービスは Windows が管理する独立プロセス）。
fn quit(app: &AppHandle) {
    let state = app.state::<AppState>();
    tauri::async_runtime::block_on(async {
        if let Some(hub) = state.hub.lock().await.take() {
            hub.shutdown().await;
        }
    });
    app.exit(0);
}

/// トレイ「アプリを終了」クリック時のエントリポイント（T16-1、実装指示
/// 3.「アプリを終了の確認」- T16-0 では確認なしだったものをここで追加）。
///
/// 収集中（[`tray_status::show_stop_item`]が真になる状態）かどうかで文言を
/// 変える。キャンセルすれば[`quit`]は一切呼ばれない - ダイアログの結果を
/// 見ずに副作用を起こさない。`show`（非ブロッキング版）を使うのは、
/// `blocking_show`がメインスレッドでの使用を禁じているため
/// （メニューイベントハンドラがどのスレッドで呼ばれても安全にする）。
fn confirm_quit(app: &AppHandle) {
    let state = app.state::<AppState>();
    let controller = state
        .controller
        .lock()
        .expect("controller mutex poisoned")
        .clone();
    let collecting = controller
        .as_ref()
        .map(|controller| tray_status::show_stop_item(&controller.status()))
        .unwrap_or(false);
    let message = if collecting {
        "収集を停止し、履歴を flush してから終了します。よろしいですか?"
    } else {
        "Banto Hub を終了します。よろしいですか?"
    };

    let app_handle = app.clone();
    app.dialog()
        .message(message)
        .title("Banto Hub を終了")
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(
            "終了".to_string(),
            "キャンセル".to_string(),
        ))
        .show(move |confirmed| {
            if confirmed {
                quit(&app_handle);
            }
        });
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
    WindowsServiceManager::new(std::env::current_exe().unwrap_or_default())
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
fn decide_startup(target: &ProbeTarget) -> StartupOutcome {
    #[cfg(windows)]
    {
        let manager = build_service_manager();
        match manager.query_status() {
            Ok(status) if status.state == ScmState::Running => {
                let probe = HttpHubHealthProbe::with_host(target.root.clone(), target.host.clone());
                match probe.probe(&target.profile_id, target.port) {
                    Ok(HealthOutcome::Healthy { .. }) => StartupOutcome::Service {
                        host: target.host.clone(),
                        port: target.port,
                        scm_state: status.state,
                    },
                    Ok(other) => StartupOutcome::Fallback(FallbackInfo {
                        scm_state: Some(status.state),
                        health: Some(other),
                        desktop_error: None,
                    }),
                    Err(err) => StartupOutcome::Fallback(FallbackInfo {
                        scm_state: Some(status.state),
                        health: None,
                        desktop_error: Some(format!("health probe に失敗しました: {err}")),
                    }),
                }
            }
            Ok(status) if matches!(status.state, ScmState::Stopped | ScmState::NotInstalled) => {
                attempt_desktop_start(target, Some(status.state))
            }
            Ok(status) => {
                // StartPending/StopPending/Other: 遷移中 - デスクトップを
                // 起動せず、利用者に「再試行」で様子を見てもらう
                // （このモジュール doc「起動シーケンス」節参照）。
                StartupOutcome::Fallback(FallbackInfo {
                    scm_state: Some(status.state),
                    health: None,
                    desktop_error: None,
                })
            }
            Err(_err) => {
                // SCM 自体に問い合わせられない（未インストール環境・権限不足等）
                // - 判定材料が無いだけなので、従来どおりデスクトップ起動を試みる。
                attempt_desktop_start(target, None)
            }
        }
    }
    #[cfg(not(windows))]
    {
        attempt_desktop_start(target, None)
    }
}

/// デスクトップホストとして[`HubRuntime::start`]を試みる（T16-0/T16-1 の
/// 従来の起動処理そのもの）。失敗した場合は診断用に同じポートへ
/// [`HttpHubHealthProbe`]を1回投げてから[`StartupOutcome::Fallback`]を返す -
/// 「ポート競合の相手が別の banto-hub インスタンスかどうか」の手がかりに
/// なる（`scm_state`の`allow(dead_code)`同様、Err時のみ使う値）。
#[allow(unused_variables)]
fn attempt_desktop_start(target: &ProbeTarget, scm_state: Option<ScmState>) -> StartupOutcome {
    // HubRuntime::start はここで同期的に待つ - `tauri::async_runtime`
    // が既定で保持する多重スレッド tokio ランタイム上で block_on
    // する（`apps/chronogazer/src-tauri`の`run()`が`init_db`等を
    // `tauri::async_runtime::block_on`で待つのと同じ流儀）。
    match tauri::async_runtime::block_on(HubRuntime::start(build_hub_config_from_env(
        HubHostKind::Shell,
    ))) {
        Ok(hub) => StartupOutcome::Desktop(hub),
        Err(err) => {
            let health = HttpHubHealthProbe::with_host(target.root.clone(), target.host.clone())
                .probe(&target.profile_id, target.port)
                .ok();
            StartupOutcome::Fallback(FallbackInfo {
                scm_state,
                health,
                desktop_error: Some(err.to_string()),
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
fn apply_startup_outcome(
    app: &AppHandle,
    window: &WebviewWindow,
    outcome: StartupOutcome,
) -> Option<(RuntimeStatus, Arc<CollectionController>)> {
    let state = app.state::<AppState>();

    // 直前が Desktop で、今回 Desktop 以外へ切り替わる場合に備えて
    // 稼働中の hub を先に取り出しておく（下の match 内でシャットダウンする）。
    let previous_hub = if matches!(outcome, StartupOutcome::Desktop(_)) {
        None
    } else {
        tauri::async_runtime::block_on(state.hub.lock()).take()
    };
    if let Some(hub) = previous_hub {
        tauri::async_runtime::block_on(hub.shutdown());
        *state.controller.lock().expect("controller mutex poisoned") = None;
    }

    match outcome {
        StartupOutcome::Service {
            host,
            port,
            scm_state,
        } => {
            navigate_to_hub(window, &format_host_port(&host, port));
            *state.view.lock().expect("view mutex poisoned") = ShellView::Service { scm_state };
            None
        }
        StartupOutcome::Desktop(hub) => {
            let addr = hub.local_addr();
            navigate_to_hub(window, &addr.to_string());
            let controller = hub.controller();
            let status = controller.status();
            *tauri::async_runtime::block_on(state.hub.lock()) = Some(hub);
            *state.controller.lock().expect("controller mutex poisoned") = Some(controller.clone());
            *state.view.lock().expect("view mutex poisoned") = ShellView::Desktop;
            Some((status, controller))
        }
        StartupOutcome::Fallback(info) => {
            render_fallback(window, &info, state.can_operate_service);
            *state.view.lock().expect("view mutex poisoned") = ShellView::Fallback(info);
            None
        }
    }
}

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
fn build_fallback_tray_menu(
    app: &AppHandle,
    scm_state: Option<&ScmState>,
    health: Option<&HealthOutcome>,
    can_operate: bool,
) -> tauri::Result<Menu<Wry>> {
    let status_item = MenuItemBuilder::with_id("status", tray_status::fallback_status_label())
        .enabled(false)
        .build(app)?;
    let mut builder = MenuBuilder::new(app).item(&status_item).separator();
    builder = builder.text("open", "画面を開く");
    if tray_status::show_start_service_action(scm_state, health, can_operate) {
        builder = builder.text("start_service", "サービスを開始");
    }
    builder = builder.text("retry", "再試行");
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
            let menu = build_fallback_tray_menu(
                app,
                info.scm_state.as_ref(),
                info.health.as_ref(),
                can_operate,
            )?;
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
    sync_tray(app);
}

/// 起動判定をやり直す共通処理（トレイ「再試行」が呼ぶ - このモジュール doc
/// 「起動シーケンス」節の該当行参照）。[`decide_startup`]の単純な決定木を
/// 呼ぶだけで、`HostSwitchEngine`は使わない（単なる状態の再評価であり、
/// 明示的な切替操作ではないため - 「サービスを開始/停止」は
/// [`run_host_switch`]を使う）。
fn retry_startup(app: &AppHandle) {
    let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) else {
        return;
    };
    let target = expected_probe_target();
    let outcome = decide_startup(&target);
    apply_outcome_and_sync(app, &window, outcome);
}

/// [`HostSwitchEngine`]が要求する
/// [`banto_hub_core::host_switch::DesktopHostControl`]の実装
/// （Desktop→Service 切替ウィザード、desktop-plan §9.7）。
///
/// `request_stop`はこのプロセスが保有する[`RunningHub`]を`take`して
/// `shutdown`し、controller ハンドルも消す。完了後の`is_stopped`は
/// hub 未保有（`None`）で`true` - profile mutex は`RunningHub::shutdown`
/// 側で解放される。
#[cfg(windows)]
struct ShellDesktopControl {
    app: AppHandle,
}

#[cfg(windows)]
impl DesktopHostControl for ShellDesktopControl {
    fn request_stop(&mut self) -> Result<(), DesktopHostError> {
        let state = self.app.state::<AppState>();
        let hub = tauri::async_runtime::block_on(state.hub.lock()).take();
        if let Some(hub) = hub {
            tauri::async_runtime::block_on(hub.shutdown());
            *state.controller.lock().expect("controller mutex poisoned") = None;
        }
        Ok(())
    }

    fn is_stopped(&self) -> bool {
        let state = self.app.state::<AppState>();
        let stopped = tauri::async_runtime::block_on(state.hub.lock()).is_none();
        stopped
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
    },
}

/// [`HostSwitchEngine::step`]を`Poll`で繰り返し、終端の
/// [`StepOutcome`]（`Completed`/`Faulted`）に到達するまでブロッキングに
/// 進める（実装指示 1./2. - `TransitionHandle::wait_until_settled`と同じ
/// 「ポーリング＋sleep」形だが、engine 側が持つ複数フェーズ分の待ちを
/// まとめて1回のブロッキング呼び出しにする）。呼び出し元
/// （[`run_host_switch`]）が背景スレッドで呼ぶ前提 - ここ自体はスレッドを
/// 起動しない。
#[cfg(windows)]
fn drive_host_switch<M, P, D>(
    engine: &mut HostSwitchEngine<M, P, D>,
    command: SwitchCommand,
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
            }
        }
    };
    loop {
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
        HostSwitchResult::Desktop => attempt_desktop_start(target, Some(ScmState::Stopped)),
        HostSwitchResult::Faulted {
            scm_state,
            health,
            reason,
        } => StartupOutcome::Fallback(FallbackInfo {
            scm_state,
            health,
            desktop_error: Some(reason),
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
#[cfg(windows)]
pub(crate) fn run_host_switch(app: &AppHandle, command: SwitchCommand) {
    let state = app.state::<AppState>();
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
            },
        );
        let result = drive_host_switch(&mut engine, command);
        let (phase, message, error) = match &result {
            HostSwitchResult::Service { .. } => (
                "completed".to_string(),
                "サービスへの切替が完了しました".to_string(),
                None,
            ),
            HostSwitchResult::Desktop => (
                "completed".to_string(),
                "アプリへの切替が完了しました".to_string(),
                None,
            ),
            HostSwitchResult::Faulted { reason, .. } => (
                "faulted".to_string(),
                format!("切替に失敗しました: {reason}"),
                Some(reason.clone()),
            ),
        };
        apply_host_switch_outcome(&app_handle, &target, result);
        host_switch_ipc::end_switch();
        host_switch_ipc::emit_progress(
            &app_handle,
            host_switch_ipc::HostSwitchProgressEvent {
                phase,
                message,
                done: true,
                error,
            },
        );
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
            message: "アプリへの切替を開始しています…".into(),
            done: false,
            error: None,
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
            message: "サービスへの切替を開始しています…".into(),
            done: false,
            error: None,
        },
    );
    run_host_switch(app, SwitchCommand::SwitchToService);
}

#[cfg(not(windows))]
fn start_service(_app: &AppHandle) {}

/// [`maybe_notify_first_tray_hide`] が既読フラグを永続化するファイルの
/// パス。Hub 本体の DB/data_dir とは独立したシェル固有の状態なので、
/// `app.path().app_data_dir()`（`tauri.conf.json`の`identifier`配下）に置く。
/// `RunningHub`の起動に失敗していても書ける場所であること、複数
/// profile／複数インストール間で共有すべきではないことの両方を満たす。
fn tray_hint_flag_path(app: &AppHandle) -> Option<PathBuf> {
    app.path()
        .app_data_dir()
        .ok()
        .map(|dir| dir.join("tray-hint-shown"))
}

/// `×` でトレイへ格納した直後に呼ぶ（UX-7 / 実装指示 4.「初回だけ」）。
///
/// 既読フラグファイルが無ければ OS 通知を出してからフラグを作成する -
/// 2回目以降の格納では通知しない。フラグの保存場所を解決できない、または
/// 書き込みに失敗した場合は通知自体をあきらめる（毎回通知され続けるより、
/// 通知が出ない方が実装指示の「初回だけ」という意図を壊さない安全側）。
/// OS 通知 API 自体の失敗（通知権限なし等）も同様に無視する - 継続動作の
/// 案内が1回出せなくても Hub の収集・UI 提供は妨げない。
fn maybe_notify_first_tray_hide(app: &AppHandle) {
    let Some(path) = tray_hint_flag_path(app) else {
        return;
    };
    if path.exists() {
        return;
    }
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    if std::fs::write(&path, b"1").is_err() {
        return;
    }
    let _ = app
        .notification()
        .builder()
        .title("Banto Hub")
        .body("Banto Hub はタスクトレイで動作を続けます。")
        .show();
}

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
        .invoke_handler(tauri::generate_handler![
            host_switch_ipc::host_switch_status,
            host_switch_ipc::switch_to_service,
            host_switch_ipc::switch_to_desktop,
            host_switch_ipc::set_service_autostart,
        ])
        .manage(AppState {
            hub: AsyncMutex::new(None),
            controller: StdMutex::new(None),
            view: StdMutex::new(ShellView::Fallback(FallbackInfo {
                scm_state: None,
                health: None,
                desktop_error: None,
            })),
            // T16-2（実装指示 4.）: Operators 判定はプロセス起動時に一度だけ
            // 確定する（[`AppState::can_operate_service`]のフィールド doc参照）。
            // T16-2 第二スライス: Operators**または** Windows ローカル
            // Administrators のメンバーなら操作可能にする
            // （`is_current_process_admin`のモジュール doc「T16-2 第二
            // スライスで追加した is_current_process_admin」節、desktop-plan
            // §8.3 の意図）。
            can_operate_service: is_current_process_operator().unwrap_or(false)
                || is_current_process_admin().unwrap_or(false),
        })
        .setup(|app| {
            let window = app
                .get_webview_window(MAIN_WINDOW_LABEL)
                .expect("tauri.conf.json の app.windows にメインウィンドウを定義済み");

            // CloseRequested（×ボタン）はプロセスを終了せずトレイへ格納
            // するだけ（実装指示「CloseRequested -> prevent_close + hide」）。
            // T16-1 で初回だけの継続通知を追加（UX-7）。
            let window_to_hide = window.clone();
            let app_handle_for_hide = app.handle().clone();
            window.on_window_event(move |event| {
                if let WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = window_to_hide.hide();
                    maybe_notify_first_tray_hide(&app_handle_for_hide);
                }
            });

            // T16-2: サービス検出 → 接続 / デスクトップ起動 / fallback の
            // 判定（このモジュール doc「起動シーケンス」節参照）。
            let target = expected_probe_target();
            let outcome = decide_startup(&target);
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
            let tray = TrayIconBuilder::new()
                .icon(
                    app.default_window_icon()
                        .cloned()
                        .expect("tauri.conf.json の bundle.icon で既定アイコンを設定済み"),
                )
                .tooltip(tooltip)
                .menu(&tray_menu)
                .show_menu_on_left_click(true)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "open" => show_main_window(app),
                    "stop" => stop_collection(app),
                    "stop_service" => stop_service(app),
                    "start_service" => start_service(app),
                    "retry" => retry_startup(app),
                    "quit" => confirm_quit(app),
                    _ => {}
                })
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

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;
    use banto_hub_core::profile_paths::resolve_profile_paths_from_env;

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
        let config = build_hub_config_from_env(HubHostKind::Shell);
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
        };
        assert!(fallback_message(&info, true).contains("確認できませんでした"));
    }
}
