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
//!   HubRuntime::start(build_hub_config()) -> RunningHub  // 収集は Stopped のまま
//!   成功: メインウィンドウを Hub の localhost URL へ navigate
//!   失敗: プレースホルダ (ui/index.html) にエラーを表示、終了操作のみ提供
//!   トレイ: 「画面を開く」「アプリを終了」の2項目のみ
//!   CloseRequested (×) -> prevent_close + hide（トレイへ格納）
//! トレイ「アプリを終了」 -> RunningHub::shutdown() -> app.exit(0)
//! 第二インスタンス起動 -> 既存ウィンドウを show/unminimize/set_focus、自身は終了
//! ```
//!
//! `crates/banto-collect`本体・収集の開始/停止・タグ CRUD 等の実処理は一切
//! ここに書かない - [`banto_hub_core::runtime::HubRuntime`] に完全に委譲する
//! （このモジュール自身が新規実装するのは「どのホストで、何を起動/終了の
//! トリガーにするか」という composition だけ - `apps/banto-hub/core/src/bin/
//! banto-hub.rs`のモジュール doc と同じ役割分担）。

use std::net::SocketAddr;

use banto_hub_core::runtime::{HubConfig, HubRuntime, RunningHub, DEFAULT_DB_PATH};
use tauri::menu::MenuBuilder;
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, WebviewWindow, WindowEvent};
use tokio::sync::Mutex as AsyncMutex;

/// メインウィンドウのラベル - `tauri.conf.json` の `app.windows[0].label` と
/// 一致させること。
const MAIN_WINDOW_LABEL: &str = "main";

/// `app.manage()` で保持するアプリ全体の唯一の可変状態。
///
/// invoke コマンドを持たないため（このモジュール doc 参照）、フィールドは
/// 稼働中の [`RunningHub`] 1つだけで足りる。
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
}

/// 環境変数から [`HubConfig`] を組み立てる。
///
/// `apps/banto-hub/core/src/bin/banto-hub.rs::build_hub_config` と読み取り
/// ロジック（既定値・レイヤー順）を1バイトも変えずに複製したもの -
/// コンソール/Windows サービスに続く3つめのホストとして、同じ環境変数
/// （`BANTO_DB`/`BANTO_ALLOW_SETUP`/`PORT`/`BANTO_BIND`/`BANTO_HUB_DATA`）を
/// そのまま使えるようにする（設計 §4.1「HubConfig はコンソールと同様に env
/// から構築してよい」）。
fn build_hub_config() -> HubConfig {
    HubConfig {
        db_path: std::env::var("BANTO_DB").unwrap_or_else(|_| DEFAULT_DB_PATH.to_string()),
        allow_setup: std::env::var("BANTO_ALLOW_SETUP")
            .map(|value| value == "1")
            .unwrap_or(false),
        port_override: std::env::var("PORT")
            .ok()
            .and_then(|value| value.parse().ok()),
        bind_override: std::env::var("BANTO_BIND").ok(),
        data_dir_override: std::env::var("BANTO_HUB_DATA")
            .ok()
            .map(std::path::PathBuf::from),
    }
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

/// トレイ「アプリを終了」（設計 §4.3 表: 「確認なし(T16-0) → shutdown +
/// exit。確認ダイアログは T16-1」）。
///
/// 稼働中の Hub があれば [`RunningHub::shutdown`] を必ず待ってから
/// プロセスを終了する（実装指示「Exit / トレイ終了 -> RunningHub::shutdown()
/// -> app.exit」）。起動に失敗していて `state.hub` が `None` のままの場合も
/// そのままプロセスを終了する。
fn quit(app: &AppHandle) {
    let state = app.state::<AppState>();
    tauri::async_runtime::block_on(async {
        if let Some(hub) = state.hub.lock().await.take() {
            hub.shutdown().await;
        }
    });
    app.exit(0);
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
        .manage(AppState {
            hub: AsyncMutex::new(None),
        })
        .setup(|app| {
            let window = app
                .get_webview_window(MAIN_WINDOW_LABEL)
                .expect("tauri.conf.json の app.windows にメインウィンドウを定義済み");

            // CloseRequested（×ボタン）はプロセスを終了せずトレイへ格納
            // するだけ（実装指示「CloseRequested -> prevent_close + hide」）。
            let window_to_hide = window.clone();
            window.on_window_event(move |event| {
                if let WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = window_to_hide.hide();
                }
            });

            // HubRuntime::start はここで同期的に待つ - `tauri::async_runtime`
            // が既定で保持する多重スレッド tokio ランタイム上で block_on
            // する（`apps/chronogazer/src-tauri`の`run()`が`init_db`等を
            // `tauri::async_runtime::block_on`で待つのと同じ流儀）。
            // `HubRuntime::start`内部が spawn するバックグラウンドタスク
            // （250ms 評価ループ・保持期間剪定・収集タスク等）はこの
            // block_on と同じランタイム上で並行に走り続ける。
            match tauri::async_runtime::block_on(HubRuntime::start(build_hub_config())) {
                Ok(hub) => {
                    // 収集は Stopped のまま（HubRuntime 既存挙動 - T14-3の
                    // 「起動時は catalog の commit のみ」）。
                    // `RunningHub::controller()`はT16-0では一切呼ばない -
                    // 開始/再起動操作はトレイに置かない（設計 §4.3）。
                    let addr = hub.local_addr();
                    navigate_to_hub(&window, addr);
                    let state = app.state::<AppState>();
                    *tauri::async_runtime::block_on(state.hub.lock()) = Some(hub);
                }
                Err(err) => {
                    show_startup_error(&window, &err.to_string());
                }
            }

            // トレイ（設計 §4.3 最小表）: 「画面を開く」「アプリを終了」の
            // 2項目のみ - 開始/再起動/サービス操作は置かない
            // （T16-1のトレイ状態表示・確認ダイアログもここでは作らない）。
            let tray_menu = MenuBuilder::new(app.handle())
                .text("open", "画面を開く")
                .text("quit", "アプリを終了")
                .build()?;
            let tray = TrayIconBuilder::new()
                .icon(
                    app.default_window_icon()
                        .cloned()
                        .expect("tauri.conf.json の bundle.icon で既定アイコンを設定済み"),
                )
                .tooltip("banto-hub")
                .menu(&tray_menu)
                .show_menu_on_left_click(true)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "open" => show_main_window(app),
                    "quit" => quit(app),
                    _ => {}
                })
                .build(app.handle())?;
            // `TrayIcon`は参照カウント式で、最後の1つが drop されると消える
            // （tauri本体のdoc comment参照） - アプリの生存期間中ずっと
            // 保持するためだけに managed state へ入れる（読み出しはしない）。
            app.manage(tray);

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [`build_hub_config`] は `HubRuntime::start` 自体の start→shutdown
    /// ラウンドトリップ（collection が Stopped のまま起動することを含む）
    /// を一切変えない - それは
    /// `apps/banto-hub/core/src/runtime.rs::tests::start_local_addr_then_shutdown_round_trip`
    /// が既にカバーしている（このシェル crate は Tauri アプリの外形からは
    /// 単体テストできないため - 実装指示「Tauri なしのロジックを分離できれば
    /// core 側既存テストでも可」）。ここではこの crate 固有のロジックだけを
    /// 検証する: 環境変数が一切設定されていない状態で
    /// `apps/banto-hub/core/src/bin/banto-hub.rs::build_hub_config`
    /// と同じ既定値になること。
    #[test]
    fn build_hub_config_defaults_when_env_unset() {
        for key in [
            "BANTO_DB",
            "BANTO_ALLOW_SETUP",
            "PORT",
            "BANTO_BIND",
            "BANTO_HUB_DATA",
        ] {
            assert!(
                std::env::var(key).is_err(),
                "test environment must not predefine {key} for this assertion to be meaningful"
            );
        }

        let config = build_hub_config();
        assert_eq!(config.db_path, DEFAULT_DB_PATH);
        assert!(!config.allow_setup);
        assert_eq!(config.port_override, None);
        assert_eq!(config.bind_override, None);
        assert_eq!(config.data_dir_override, None);
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
