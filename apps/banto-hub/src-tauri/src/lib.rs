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
//! トレイ「サービスを停止」（サービス接続時、Operators のみ）
//!          -> ServiceManager::stop() 発行 -> 起動判定をやり直す
//!          （[`retry_startup`]と同じ経路 - 停止直後は SCM がまだ`Running`/
//!          `StopPending`のことが多く、その場合は fallback 画面へ落ちる。
//!          `Stopped`まで落ち着いていれば、そのままデスクトップホストとして
//!          `HubRuntime::start`を試みる - 「サービスを停止したらこのアプリが
//!          代わりに運転する」という単純な帰結で、専用の切替ウィザードは
//!          実装しない）
//! トレイ「サービスを開始」（fallback 画面、Operators のみ）
//!          -> ServiceManager::start() 発行 -> 起動判定をやり直す
//! トレイ「再試行」（fallback 画面） -> 起動判定をやり直すだけ
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

use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};

use banto_hub_core::controller::{CollectionController, RuntimeStatus};
use banto_hub_core::http_hub_health::HttpHubHealthProbe;
use banto_hub_core::hub_health::{HealthOutcome, HubHealthProbe};
use banto_hub_core::profile_lock::HubHostKind;
use banto_hub_core::profile_paths::{build_hub_config_from_env, resolve_profile_paths_from_env};
use banto_hub_core::runtime::{HubRuntime, RunningHub};
use banto_hub_core::service_manager::ScmState;
#[cfg(windows)]
use banto_hub_core::service_manager::{ServiceManager, WindowsServiceManager};
use banto_hub_core::service_operators::is_current_process_operator;
use tauri::menu::{Menu, MenuBuilder, MenuItemBuilder};
use tauri::tray::{TrayIcon, TrayIconBuilder};
use tauri::{AppHandle, Manager, WebviewWindow, WindowEvent, Wry};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use tauri_plugin_notification::NotificationExt;
use tokio::sync::Mutex as AsyncMutex;

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
enum ShellView {
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
    /// サービスへ接続する - `addr`は navigate 先。
    Service {
        addr: SocketAddr,
        scm_state: ScmState,
    },
    /// デスクトップホストとして起動できた。
    Desktop(RunningHub),
    /// fallback 画面を表示する。
    Fallback(FallbackInfo),
}

/// `app.manage()` で保持するアプリ全体の唯一の可変状態。
struct AppState {
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
    view: StdMutex<ShellView>,
    /// T16-2: `is_current_process_operator().unwrap_or(false)`
    /// （実装指示 4.「Operators 委任」）。起動時に一度確定し、以後は
    /// 変えない - Operators グループへの参加はプロセス再起動が前提
    /// （Windows のトークンはプロセス生存中に再評価されないため、
    /// このスライスでは再評価の仕組みは作らない）。
    can_operate_service: bool,
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
/// 区別するだけで、navigate 自体の処理は同じ。
fn navigate_to_hub(window: &WebviewWindow, addr: SocketAddr) {
    let url = match tauri::Url::parse(&format!("http://{addr}/")) {
        Ok(url) => url,
        Err(err) => {
            // `addr` は `RunningHub::local_addr()` またはサービスの期待
            // ポートから組み立てた実バインドアドレスなので、パース失敗は
            // 事実上起こり得ない - 起きてもプロセスを落とさず診断だけ表示する。
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

/// T16-2: env（`BANTO_HUB_ROOT`/`BANTO_HUB_PROFILE`/`PORT`）から
/// [`decide_startup`]が必要とする「期待する root・profile-id・port」を
/// 解決する。`build_hub_config_from_env`と同じ env を読むが、`HubConfig`
/// 全体ではなく[`HttpHubHealthProbe`]が必要とする3値だけを返す薄いラッパ。
fn expected_probe_target() -> (PathBuf, String, u16) {
    let paths = resolve_profile_paths_from_env();
    let port = std::env::var("PORT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(banto_hub_core::settings::DEFAULT_PORT);
    (paths.root, paths.profile_id, port)
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
/// とサービスが同時に同じ profile を掴もうとするのを避ける - 完全な
/// `HostSwitchEngine`（T17-3）ほど厳密ではないが、実装指示「この
/// スライスでは HostSwitchEngine を使わない」に沿った最小限の安全策）。
/// `Stopped`/`NotInstalled`、または SCM 問い合わせ自体が失敗した場合は
/// 従来どおりデスクトップホストとして起動を試みる。
///
/// 非 Windows: SCM という概念が無いため、常に[`attempt_desktop_start`]を
/// 呼ぶ（実装指示「Non-Windows: keep current Desktop-only path」）。
fn decide_startup(root: &Path, profile_id: &str, port: u16) -> StartupOutcome {
    #[cfg(windows)]
    {
        let manager = build_service_manager();
        match manager.query_status() {
            Ok(status) if status.state == ScmState::Running => {
                let probe = HttpHubHealthProbe::new(root.to_path_buf());
                match probe.probe(profile_id, port) {
                    Ok(HealthOutcome::Healthy { .. }) => StartupOutcome::Service {
                        addr: SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
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
                attempt_desktop_start(root, profile_id, port, Some(status.state))
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
                attempt_desktop_start(root, profile_id, port, None)
            }
        }
    }
    #[cfg(not(windows))]
    {
        attempt_desktop_start(root, profile_id, port, None)
    }
}

/// デスクトップホストとして[`HubRuntime::start`]を試みる（T16-0/T16-1 の
/// 従来の起動処理そのもの）。失敗した場合は診断用に同じポートへ
/// [`HttpHubHealthProbe`]を1回投げてから[`StartupOutcome::Fallback`]を返す -
/// 「ポート競合の相手が別の banto-hub インスタンスかどうか」の手がかりに
/// なる（`scm_state`の`allow(dead_code)`同様、Err時のみ使う値）。
#[allow(unused_variables)]
fn attempt_desktop_start(
    root: &Path,
    profile_id: &str,
    port: u16,
    scm_state: Option<ScmState>,
) -> StartupOutcome {
    // HubRuntime::start はここで同期的に待つ - `tauri::async_runtime`
    // が既定で保持する多重スレッド tokio ランタイム上で block_on
    // する（`apps/chronogazer/src-tauri`の`run()`が`init_db`等を
    // `tauri::async_runtime::block_on`で待つのと同じ流儀）。
    match tauri::async_runtime::block_on(HubRuntime::start(build_hub_config_from_env(
        HubHostKind::Shell,
    ))) {
        Ok(hub) => StartupOutcome::Desktop(hub),
        Err(err) => {
            let health = HttpHubHealthProbe::new(root.to_path_buf())
                .probe(profile_id, port)
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
/// 「もう一方を起動したまま切り替わる」二重接続を避けるための最小限の
/// 安全策（`HostSwitchEngine`ほど厳密な待ち合わせは行わない、実装指示
/// 「full HostSwitchEngine は作らない」の範囲内での安全策）。
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
        StartupOutcome::Service { addr, scm_state } => {
            navigate_to_hub(window, addr);
            *state.view.lock().expect("view mutex poisoned") = ShellView::Service { scm_state };
            None
        }
        StartupOutcome::Desktop(hub) => {
            let addr = hub.local_addr();
            navigate_to_hub(window, addr);
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

/// 起動判定をやり直す共通処理（トレイ「再試行」「サービスを開始」
/// 「サービスを停止」の全てが最終的にこれを呼ぶ - このモジュール doc
/// 「起動シーケンス」節の該当行参照）。
///
/// デスクトップへ新規遷移した場合のみ[`watch_collection_status`]を新しく
/// spawn する（[`ShellView`]が直前 Desktop でなかった場合 - 二重 spawn を
/// 避けるため）。
fn retry_startup(app: &AppHandle) {
    let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) else {
        return;
    };
    let state = app.state::<AppState>();
    let was_desktop = matches!(
        *state.view.lock().expect("view mutex poisoned"),
        ShellView::Desktop
    );

    let (root, profile_id, port) = expected_probe_target();
    let outcome = decide_startup(&root, &profile_id, port);
    let newly_desktop = matches!(outcome, StartupOutcome::Desktop(_));
    if let Some((_status, controller)) = apply_startup_outcome(app, &window, outcome) {
        if !was_desktop && newly_desktop {
            let app_handle = app.clone();
            tauri::async_runtime::spawn(watch_collection_status(app_handle, controller));
        }
    }
    sync_tray(app);
}

/// トレイ「サービスを停止」クリック時のエントリポイント（サービス接続時、
/// Operators のみ表示 - [`build_service_tray_menu`]）。
///
/// [`ServiceManager::stop`]を発行するだけで完了を待たない（SCM の
/// `StopPending`遷移は非同期 - 完了待ちのポーリングは実装しない、実装指示
/// 「full HostSwitchEngine は作らない」の範囲内）。発行後すぐに
/// [`retry_startup`]で状態を再評価する - 停止がまだ完了していなければ
/// fallback（「再試行」で様子見）、完了していれば（`Stopped`まで進んでいれば）
/// デスクトップホストとしての起動を自動的に試みる（このモジュール doc
/// 「起動シーケンス」節参照）。
#[cfg(windows)]
fn stop_service(app: &AppHandle) {
    let manager = build_service_manager();
    if let Err(err) = manager.stop() {
        eprintln!("banto-hub-shell: サービス停止の要求に失敗しました: {err}");
    }
    retry_startup(app);
}

#[cfg(not(windows))]
fn stop_service(_app: &AppHandle) {}

/// トレイ「サービスを開始」クリック時のエントリポイント（fallback 時、
/// Operators かつ SCM が`Stopped`のときだけ表示 -
/// [`build_fallback_tray_menu`]）。[`stop_service`]と対称的に、発行後すぐ
/// [`retry_startup`]で再評価する（`StartPending`のままなら fallback の
/// ままだが、「サービスを開始」自体は`StartPending`では隠れる -
/// [`tray_status::show_start_service_action`]）。
#[cfg(windows)]
fn start_service(app: &AppHandle) {
    let manager = build_service_manager();
    if let Err(err) = manager.start() {
        eprintln!("banto-hub-shell: サービス開始の要求に失敗しました: {err}");
    }
    retry_startup(app);
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
            can_operate_service: is_current_process_operator().unwrap_or(false),
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
            let (root, profile_id, port) = expected_probe_target();
            let outcome = decide_startup(&root, &profile_id, port);
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
        let (root, profile_id, port) = expected_probe_target();
        let expected_paths = resolve_profile_paths_from_env();
        assert_eq!(root, expected_paths.root);
        assert_eq!(profile_id, expected_paths.profile_id);
        match std::env::var("PORT")
            .ok()
            .and_then(|value| value.parse::<u16>().ok())
        {
            Some(env_port) => assert_eq!(port, env_port),
            None => assert_eq!(port, banto_hub_core::settings::DEFAULT_PORT),
        }
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
