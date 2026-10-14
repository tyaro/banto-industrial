//! 旧データ（`%APPDATA%\dev.tyaro.chronogazer\`）の 1 回だけの案内
//! （#392 B2-4b、設計書 §5 決定 3・2026-10-14 オーナー決定 O10）。
//!
//! B2-4b からデスクトップ host もデータのルート（既定 `%ProgramData%\ChronoGazer`）を
//! 使うので、以前の版のデータ（DB・時系列データ・バックアップ）は使われなくなる。
//! **読まない・移さない・消さない**（H9）。ここは「使われません。不要なら手で消して
//! ください」と、利用者ごとに 1 回だけ知らせるだけ。
//!
//! # 出す条件（[`should_show`]）
//!
//! - 旧 DB（`chronogazer_core::paths::legacy_desktop_db_path`、
//!   `<APPDATA>\dev.tyaro.chronogazer\chronogazer.sqlite3`）が**ファイルとして**ある。
//!   見るのは有無だけで、中身は開かない。フォルダだけ（トレイの初回の通知の印
//!   `tray-hint-shown` は今もそこに作られる - 共有 crate の `banto-desktop-tray` が
//!   `app_data_dir()` に置くため）では出さない。
//! - 利用者ごとの既読の印（`%LOCALAPPDATA%\dev.tyaro.chronogazer\legacy-data-notice-shown`、
//!   [`MARKER_FILE_NAME`]）がまだ無い。印をデータのルートではなく利用者の
//!   `%LOCALAPPDATA%` に置くのは、旧データが利用者ごと（`%APPDATA%`）で、ルートは
//!   PC に 1 つだから（O10）。
//!
//! 出すのは Desktop の画面（[`crate::launch_desktop`] が成功した後）と、B2-4c からは
//! Service の画面（そのときもこの [`show_once`] を呼ぶ）。起動に失敗した窓・fallback
//! では出さない。
//!
//! # 印を付ける時（判断）
//!
//! 印は**ダイアログを閉じた後**に付ける（[`acknowledge`] = `claim_once_flag`）。出す前に
//! 付けると、ダイアログを出せなかった・読む前にプロセスが終わったときに二度と出ない。
//! 逆に、印を書けない環境では毎回出てしまうので、出す前に印のフォルダを作れるかを
//! 確かめ、作れなければ出さない（毎回出続けるより、出ない方が「1 回だけ」の意図を
//! 壊さない - `banto-desktop-tray` のトレイの通知と同じ安全側）。同じプロセスの中で
//! 2 回呼ばれても（ダイアログを閉じる前に Desktop の起動をやり直した等）、2 つ目は
//! 出さない。標準エラー出力の 1 行は、ダイアログを出すたびに 1 行。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use chronogazer_core::paths::DataLayout;
use tauri::{AppHandle, Manager};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

/// 利用者ごとの既読の印のファイル名（`app_local_data_dir()` の下）。
pub(crate) const MARKER_FILE_NAME: &str = "legacy-data-notice-shown";

/// このプロセスで既にダイアログを出したか。
static SHOWN_IN_THIS_PROCESS: AtomicBool = AtomicBool::new(false);

/// 案内の文言。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NoticeText {
    pub(crate) title: String,
    pub(crate) body: String,
    /// 標準エラー出力の 1 行。
    pub(crate) log_line: String,
}

/// 案内の文言を組む（純関数）。`legacy_db` は旧 DB のパス（案内するのはその
/// フォルダ）、`new_root` は今のデータのルート。
pub(crate) fn notice_text(legacy_db: &Path, new_root: &Path) -> NoticeText {
    let legacy_dir = legacy_db.parent().unwrap_or(legacy_db);
    let legacy_dir = legacy_dir.display();
    let new_root = new_root.display();
    NoticeText {
        title: "ChronoGazer のデータの置き場所が変わりました".to_string(),
        body: format!(
            "この版の ChronoGazer は、データを次の場所に置きます。\n{new_root}\n\n\
             以前の版のデータ（{legacy_dir}）は使われません。自動では移しも消しもしません。\
             要らなければ、ChronoGazer を終了してから手で削除してください。\n\n\
             アカウント・PLC の接続・タグ・設定と Hub への接続は引き継がれません。\
             初回セットアップからやり直し、Hub 経由のタグを使うときは「設定 > Hub接続」から\
             接続し直してください（Hub 側に以前のキーが残っていれば、Hub の管理画面で失効させて\
             ください）。\n\n\
             この案内は 1 回だけ表示します。"
        ),
        log_line: format!(
            "banto: 以前の版のデータ（{legacy_dir}）は使われません。新しいデータの置き場所: {new_root}。\
             自動では移しも消しもしません（要らなければ手で削除してください）。Hub への接続は登録し直しが必要です。"
        ),
    }
}

/// 案内を出すか（旧 DB がファイルとしてあり、既読の印がまだ無い）。旧 DB は有無だけを
/// 見る（開かない）。
pub(crate) fn should_show(legacy_db: Option<&Path>, marker: &Path) -> bool {
    legacy_db.is_some_and(Path::is_file) && !marker.exists()
}

/// 既読の印を付ける（ダイアログを閉じた後）。今回付けたら `true`。既にある・
/// 書けないときは `false`。
pub(crate) fn acknowledge(marker: &Path) -> bool {
    banto_desktop_tray::claim_once_flag(marker)
}

/// 印のフォルダを作れるか確かめる（作れなければ案内しない。モジュール doc）。
fn marker_dir_ready(marker: &Path) -> bool {
    match marker.parent() {
        Some(parent) => std::fs::create_dir_all(parent).is_ok(),
        None => false,
    }
}

/// 既読の印の場所（`%LOCALAPPDATA%\dev.tyaro.chronogazer\legacy-data-notice-shown`）。
fn marker_path(app: &AppHandle) -> Option<PathBuf> {
    match app.path().app_local_data_dir() {
        Ok(dir) => Some(dir.join(MARKER_FILE_NAME)),
        Err(err) => {
            eprintln!("banto: 旧データの案内の既読の印の場所を決められませんでした: {err}");
            None
        }
    }
}

/// 旧データがあれば、利用者ごとに 1 回だけ案内する（待たない。ダイアログは別の
/// スレッドで開き、閉じたら印を付ける）。Desktop の起動の成功の後に呼ぶ。B2-4c の
/// Service の画面からも呼ぶ（`layout` はそのとき使っているデータのルート）。
pub(crate) fn show_once(app: &AppHandle, layout: &DataLayout) {
    let legacy_db = chronogazer_core::paths::legacy_desktop_db_path();
    // 旧 DB が無ければ、印の場所も求めない（何も作らない）。
    if !legacy_db.as_deref().is_some_and(Path::is_file) {
        return;
    }
    let Some(marker) = marker_path(app) else {
        return;
    };
    if !should_show(legacy_db.as_deref(), &marker) {
        return;
    }
    if !marker_dir_ready(&marker) {
        eprintln!(
            "banto: 旧データの案内の既読の印（{}）を書けないため、案内を出しません",
            marker.display()
        );
        return;
    }
    if SHOWN_IN_THIS_PROCESS.swap(true, Ordering::SeqCst) {
        return;
    }
    let Some(legacy_db) = legacy_db else {
        return;
    };
    let text = notice_text(&legacy_db, layout.root());
    eprintln!("{}", text.log_line);
    let mut dialog = app
        .dialog()
        .message(text.body)
        .title(text.title)
        .kind(MessageDialogKind::Info)
        .buttons(MessageDialogButtons::Ok);
    // メインの窓の前に出す（親が無いと、できたばかりのメインの窓の後ろに隠れうる）。
    if let Some(window) = app.get_webview_window(crate::MAIN_WINDOW_LABEL) {
        dialog = dialog.parent(&window);
    }
    dialog.show(move |_| {
            if !acknowledge(&marker) {
                eprintln!(
                    "banto: 旧データの案内の既読の印（{}）を付けられませんでした（次の起動でもう一度出ます）",
                    marker.display()
                );
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 文言は旧データのフォルダ・新しいルート・手で消すこと・Hub の登録し直し・
    /// 1 回だけ、を含む（決定 3・O10）。
    #[test]
    fn notice_text_names_both_locations_and_what_to_do() {
        let legacy = PathBuf::from(r"C:\Users\a\AppData\Roaming")
            .join("dev.tyaro.chronogazer")
            .join("chronogazer.sqlite3");
        let root = PathBuf::from(r"C:\ProgramData\ChronoGazer");
        let text = notice_text(&legacy, &root);
        let legacy_dir = legacy.parent().unwrap().display().to_string();
        let root_text = root.display().to_string();
        for part in [&text.body, &text.log_line] {
            assert!(part.contains(&legacy_dir), "{part}");
            assert!(part.contains(&root_text), "{part}");
            assert!(part.contains("使われません"), "{part}");
            assert!(part.contains("削除"), "{part}");
            assert!(part.contains("Hub"), "{part}");
            // 旧 DB のファイル名ではなくフォルダを案内する。
            assert!(!part.contains("chronogazer.sqlite3"), "{part}");
        }
        assert!(text.body.contains("Hub接続"), "{}", text.body);
        assert!(text.body.contains("1 回だけ"), "{}", text.body);
        assert!(text.body.contains("自動では移しも消しもしません"));
        assert!(!text.log_line.contains('\n'), "ログは 1 行");
        assert!(text.title.contains("置き場所"));
    }

    /// 旧 DB がファイルとしてあり、印がまだ無いときだけ出す。印を付けたら二度と
    /// 出さない（付けるのは 1 回だけ成功する）。旧 DB には触れない。
    #[test]
    fn shown_once_per_marker_and_only_when_the_legacy_db_exists() {
        let appdata = tempfile::tempdir().unwrap();
        let local = tempfile::tempdir().unwrap();
        let legacy =
            chronogazer_core::paths::legacy_desktop_db_path_in(appdata.path().to_str()).unwrap();
        let marker = local
            .path()
            .join("dev.tyaro.chronogazer")
            .join(MARKER_FILE_NAME);

        // 旧データが無い・場所が求められない。
        assert!(!should_show(None, &marker));
        assert!(!should_show(Some(&legacy), &marker));
        // フォルダだけ（トレイの通知の印だけがある等）では出さない。
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(legacy.parent().unwrap().join("tray-hint-shown"), b"1").unwrap();
        assert!(!should_show(Some(&legacy), &marker));
        // 旧 DB があり印が無い → 出す。
        std::fs::write(&legacy, b"old db bytes").unwrap();
        assert!(should_show(Some(&legacy), &marker));
        // 出す前の確かめは印のフォルダを作るだけ（印は付けない）。
        assert!(marker_dir_ready(&marker));
        assert!(should_show(Some(&legacy), &marker));
        // 閉じた後に印を付ける → もう出さない。2 回目の印は付かない。
        assert!(acknowledge(&marker));
        assert!(!should_show(Some(&legacy), &marker));
        assert!(!acknowledge(&marker));
        // 旧 DB はそのまま（H9）。
        assert_eq!(std::fs::read(&legacy).unwrap(), b"old db bytes");
    }

    /// 印のフォルダを作れないときは出さない（毎回出続けない）。
    #[test]
    fn not_shown_when_the_marker_cannot_be_written() {
        let local = tempfile::tempdir().unwrap();
        let blocked = local.path().join("dev.tyaro.chronogazer");
        std::fs::write(&blocked, b"a file where the folder should be").unwrap();
        let marker = blocked.join(MARKER_FILE_NAME);
        assert!(!marker_dir_ready(&marker));
        assert!(!acknowledge(&marker));
    }
}
