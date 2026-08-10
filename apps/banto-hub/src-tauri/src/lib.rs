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
//! ## 起動シーケンス（[`run`] が組み立てる全体）
//!
//! ```text
//! setup:
//!   HubRuntime::start(build_hub_config_from_env(HubHostKind::Shell)) -> RunningHub  // 収集は Stopped のまま
//!   成功: メインウィンドウを Hub の localhost URL へ navigate
//!   失敗: プレースホルダ (ui/index.html) にエラーを表示、終了操作のみ提供
//!   トレイ: 状態ラベル・「画面を開く」・(収集中のみ)「収集を停止」・「アプリを終了」
//!          （T16-1、design §3 / desktop-plan §9.9）
//!   CollectionController::subscribe_status() の変化を tray tooltip/menu へ反映
//!   CloseRequested (×) -> prevent_close + hide（トレイへ格納）+ 初回だけ通知
//! トレイ「アプリを終了」 -> 確認ダイアログ -> RunningHub::shutdown() -> app.exit(0)
//! トレイ「収集を停止」 -> controller.stop().await（invoke 二重 API ではなく
//!                         シェルが保持する controller ハンドル経由）
//! 第二インスタンス起動 -> 既存ウィンドウを show/unminimize/set_focus、自身は終了
//! ```
//!
//! `crates/banto-collect`本体・収集の開始/停止・タグ CRUD 等の実処理は一切
//! ここに書かない - [`banto_hub_core::runtime::HubRuntime`] に完全に委譲する
//! （このモジュール自身が新規実装するのは「どのホストで、何を起動/終了の
//! トリガーにするか」という composition だけ - `apps/banto-hub/core/src/bin/
//! banto-hub.rs`のモジュール doc と同じ役割分担）。「収集を停止」も
//! [`banto_hub_core::controller::CollectionController::stop`] を直接呼ぶだけで、
//! Hub REST の運転 API を再実装するものではない。
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
//! これらは全てデスクトップホスト（アプリがランタイムを所有する場合）専用。
//! サービス接続時のホスト表示・fallback メニューは T16-2 の対象。

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};

use banto_hub_core::controller::{CollectionController, RuntimeStatus};
use banto_hub_core::profile_lock::HubHostKind;
use banto_hub_core::profile_paths::build_hub_config_from_env;
use banto_hub_core::runtime::{HubRuntime, RunningHub};
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

/// `app.manage()` で保持するアプリ全体の唯一の可変状態。
struct AppState {
    /// 稼働中の Hub。起動に失敗した場合は `None` のまま
    /// （[`show_startup_error`] 参照 - 終了操作だけを提供する）。
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
    /// 起動に失敗した場合は`None`のまま。
    controller: StdMutex<Option<Arc<CollectionController>>>,
}

/// JS 文字列リテラルへの最小エスケープ。[`show_startup_error`] が
/// プレースホルダ (`ui/index.html`) へ [`WebviewWindow::eval`] でエラー文言を
/// 書き込むためだけに使う小さなヘルパー - この1箇所のためだけに
/// `serde_json` 依存を増やさない（実装指示「serde 等は最小限」）。埋め込む
/// 文字列は `HubStartError`（`thiserror` 由来の日本語定型文）由来で、任意の
/// 外部入力（HTML/JS インジェクション経路）を埋め込むことはない。
fn js_string_literal(value: &str) -> String {
    let escaped = value
        .replace('\\', "\\\\")
        .replace('\'', "\\'")
        .replace('\r', "")
        .replace('\n', "\\n");
    format!("'{escaped}'")
}

/// 起動失敗時の表示（実装指示「起動失敗」節: 「サービス接続へ逃がさず、
/// エラーをログ/ダイアログ相当で示して終了操作だけ提供する」）。
///
/// ポート競合等で [`HubRuntime::start`] が失敗した場合、プレースホルダ
/// (`ui/index.html`) の `#banto-hub-status` をエラー文言へ書き換えるだけの
/// 最小実装 - T16-0 はこれ以上作り込まない（ダイアログ相当の UI 強化は
/// T16-2 以降、設計 §5「T16-1 以降で別途設計するもの」）。ウィンドウは
/// 表示されたままなので、トレイ「アプリを終了」で終了できる。
fn show_startup_error(window: &WebviewWindow, message: &str) {
    eprintln!("banto-hub-shell: {message}");
    let display =
        format!("起動できませんでした。\n{message}\n\nトレイメニューから終了してください。");
    let js = format!(
        "document.getElementById('banto-hub-status').textContent = {};",
        js_string_literal(&display)
    );
    if let Err(err) = window.eval(js) {
        eprintln!("banto-hub-shell: エラー表示の反映に失敗しました: {err}");
    }
}

/// [`HubRuntime::start`] 成功後、プレースホルダから Hub 自身の管理画面
/// （axum が同一 origin で配信する UI、設計 §4「P3」）へ navigate する。
/// `frontendDist`（`ui/index.html`）の出番はここまで - 二重配布は行わない。
fn navigate_to_hub(window: &WebviewWindow, addr: SocketAddr) {
    let url = match tauri::Url::parse(&format!("http://{addr}/")) {
        Ok(url) => url,
        Err(err) => {
            // `addr` は `RunningHub::local_addr()` が返す実バインドアドレス
            // なので、パース失敗は事実上起こり得ない - 起きてもプロセスを
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
/// 個別に呼び直すことはしない）。起動に失敗していて `state.hub` が `None`
/// のままの場合もそのままプロセスを終了する。
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

/// トレイメニューを組み立てる（desktop-plan §9.9 の表どおりの3構成）。
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
fn build_tray_menu(app: &AppHandle, status: Option<&RuntimeStatus>) -> tauri::Result<Menu<Wry>> {
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

/// 状態変化をトレイの tooltip とメニューへ反映する（[`watch_collection_status`]
/// から毎回呼ばれる）。tray が未`manage`のうちに呼ばれた場合は何もしない -
/// `run`のセットアップ順（tray 構築 → 監視 spawn）により通常は起こらないが、
/// `try_state`で防御的に扱う。
fn apply_status_to_tray(app: &AppHandle, status: &RuntimeStatus) {
    let Some(tray) = app.try_state::<TrayIcon<Wry>>() else {
        return;
    };
    let _ = tray.set_tooltip(Some(tray_status::tooltip_text(status)));
    if let Ok(menu) = build_tray_menu(app, Some(status)) {
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
/// それはプロセス終了処理中のみ - ループを抜けてタスクを終える。
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

            // HubRuntime::start はここで同期的に待つ - `tauri::async_runtime`
            // が既定で保持する多重スレッド tokio ランタイム上で block_on
            // する（`apps/chronogazer/src-tauri`の`run()`が`init_db`等を
            // `tauri::async_runtime::block_on`で待つのと同じ流儀）。
            // `HubRuntime::start`内部が spawn するバックグラウンドタスク
            // （250ms 評価ループ・保持期間剪定・収集タスク等）はこの
            // block_on と同じランタイム上で並行に走り続ける。
            let mut initial_status: Option<RuntimeStatus> = None;
            match tauri::async_runtime::block_on(HubRuntime::start(build_hub_config_from_env(
                HubHostKind::Shell,
            ))) {
                Ok(hub) => {
                    // 収集は Stopped のまま（HubRuntime 既存挙動 - T14-3の
                    // 「起動時は catalog の commit のみ」）。T16-1 では
                    // controller を保持して状態購読・「収集を停止」に使う
                    // （開始/再起動操作は引き続きトレイに置かない）。
                    let addr = hub.local_addr();
                    navigate_to_hub(&window, addr);
                    let controller = hub.controller();
                    initial_status = Some(controller.status());
                    let state = app.state::<AppState>();
                    *tauri::async_runtime::block_on(state.hub.lock()) = Some(hub);
                    *state.controller.lock().expect("controller mutex poisoned") = Some(controller);
                }
                Err(err) => {
                    show_startup_error(&window, &err.to_string());
                }
            }

            // トレイ（T16-1、desktop-plan §9.9）: 起動直後の状態
            // （`initial_status`、起動失敗時は`None`）に応じたメニュー・
            // tooltip で構築する。以後の変化は tray 構築後に spawn する
            // 監視タスクが反映する。
            let tray_menu = build_tray_menu(app.handle(), initial_status.as_ref())?;
            let tooltip = initial_status
                .as_ref()
                .map(tray_status::tooltip_text)
                .unwrap_or_else(|| "banto-hub".to_string());
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
                    "quit" => confirm_quit(app),
                    _ => {}
                })
                .build(app.handle())?;
            // `TrayIcon`は参照カウント式で、最後の1つが drop されると消える
            // （tauri本体のdoc comment参照） - アプリの生存期間中ずっと
            // 保持するためだけに managed state へ入れる
            // （[`apply_status_to_tray`]が`try_state`で読み出す）。
            app.manage(tray);

            // 状態購読はトレイ構築後に開始する
            // （[`watch_collection_status`]のモジュール doc 参照 - tray が
            // 無い間に状態変化が届いても適用先が無いため）。
            let controller = app
                .state::<AppState>()
                .controller
                .lock()
                .expect("controller mutex poisoned")
                .clone();
            if let Some(controller) = controller {
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

    /// [`js_string_literal`] は [`show_startup_error`] が
    /// `window.eval` へ渡す JS ソースの中に埋め込まれる - バックスラッシュ・
    /// シングルクォート・改行を潰さないと、生成した JS 自体が構文エラーに
    /// なる（エラー文言は `HubStartError` の `Display` 由来で改行を含み
    /// 得る）。
    #[test]
    fn js_string_literal_escapes_quotes_and_newlines() {
        let literal = js_string_literal("banto-hub: line1\nline2 it's \\ok\\");
        assert_eq!(literal, "'banto-hub: line1\\nline2 it\\'s \\\\ok\\\\'");
    }
}
