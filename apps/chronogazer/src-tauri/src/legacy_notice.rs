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
//! - 今のデータのルートが旧フォルダと**同じか、その中**ではない（[`overlap`]、PR #606 の
//!   オーナーレビュー P2）。`CHRONOGAZER_ROOT` を旧フォルダの中に向けると、「旧フォルダは
//!   使われません、手で削除してください」が今使っている DB・時系列データ・秘密ごと
//!   消させる案内になる。**判断**: その場合はダイアログを出さず（削除を勧める文を一切
//!   出さない。DB のファイルだけの削除を勧める文も、同じフォルダで `-wal`・`-shm`・
//!   バックアップを取り違えうるので作らない）、標準エラー出力に「案内を出さない理由」を
//!   1 行だけ出し、印も付けない（ルートを外へ移した後の起動で案内が出る）。このケースは
//!   開発・試験で `CHRONOGAZER_ROOT` を明示したときだけ起きる（既定の
//!   `%ProgramData%\ChronoGazer` は `%APPDATA%` と重ならない）。逆向き（旧フォルダが
//!   ルートの中、例 `CHRONOGAZER_ROOT=%APPDATA%`）は普通に案内する: ルートの直下に
//!   作るもの（`chronogazer.sqlite3`・`data`・`backups`・`audit-spool`・`secrets`・`logs`・
//!   `run`）に `dev.tyaro.chronogazer` は無いので、旧フォルダを消してもルートのものは
//!   消えない。比較は両方を `canonicalize` できればその結果で、できなければ文字列の
//!   正規化（Windows は大文字小文字・`/` と `\`・末尾の区切り・`\\?\` の違いを無視）で、
//!   要素単位の前方一致（[`normalize_for_compare`]）。
//!
//! 出すのは Desktop の画面（[`crate::launch_desktop`] が成功した後）と、B2-4c からは
//! Service の画面（そのときもこの [`show_once`] を呼ぶ）。起動に失敗した窓・fallback
//! では出さない。
//!
//! # 印を付ける時（判断）
//!
//! 印は**ダイアログを閉じた後**に付ける（[`acknowledge`] = `claim_once_flag`）。出す前に
//! 付けると、ダイアログを出せなかった・読む前にプロセスが終わったときに二度と出ない。
//! 逆に、印を書けない環境では毎回出てしまうので、出す前に印のフォルダを作り、
//! **そこへ実際に一時ファイルを書いて消せるか**を確かめ（印そのものの名前は使わない。
//! フォルダがあるだけでは書けるとは限らない - PR #606 の Codex P2）、だめなら出さない（毎回出続けるより、出ない方が「1 回だけ」の意図を
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
             要らなければ、ChronoGazer を終了してから手で削除してください\
             （設定の時系列データの保存先をそのフォルダの中に向けているときは、削除しないでください）。\n\n\
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

/// 今のデータのルートと旧データのフォルダの重なり（モジュール doc「出す条件」）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Overlap {
    /// 重ならない（普通。案内する）。
    Separate,
    /// ルートが旧フォルダそのもの（案内しない）。
    Same,
    /// ルートが旧フォルダの中（案内しない）。
    RootInsideLegacy,
    /// 旧フォルダがルートの中（案内する。旧フォルダを消してもルートのものは消えない）。
    LegacyInsideRoot,
}

impl Overlap {
    /// 削除を勧める案内を出してよいか。
    pub(crate) fn allows_notice(self) -> bool {
        matches!(self, Overlap::Separate | Overlap::LegacyInsideRoot)
    }
}

/// 比較用にパスを正規化する（純関数）: `\\?\`（`\\?\UNC\` は `\\`）を外し、Windows では
/// `/` を `\` に、大文字小文字を畳む。末尾の区切りは要素に分けるときに消える。
pub(crate) fn normalize_for_compare(path: &Path, is_windows: bool) -> Vec<String> {
    let mut text = path.to_string_lossy().into_owned();
    if is_windows {
        text = text.replace('/', "\\");
        if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
            text = format!(r"\\{rest}");
        } else if let Some(rest) = text.strip_prefix(r"\\?\") {
            text = rest.to_string();
        }
        text = text.to_lowercase();
    }
    let separator = if is_windows { '\\' } else { '/' };
    let absolute = text.starts_with(separator);
    let mut parts: Vec<String> = text
        .split(separator)
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect();
    if absolute {
        parts.insert(0, String::new());
    }
    parts
}

/// 正規化した 2 つのパスの重なり（純関数）。
pub(crate) fn overlap_of(root: &[String], legacy_dir: &[String]) -> Overlap {
    if root == legacy_dir {
        Overlap::Same
    } else if root.starts_with(legacy_dir) {
        Overlap::RootInsideLegacy
    } else if legacy_dir.starts_with(root) {
        Overlap::LegacyInsideRoot
    } else {
        Overlap::Separate
    }
}

/// ルートと旧フォルダの重なり。両方を `canonicalize` できればその結果で（ジャンクション・
/// 8.3 形式・`..` を解く）、どちらかができなければ両方とも文字列の正規化だけで比べる。
pub(crate) fn overlap(root: &Path, legacy_dir: &Path) -> Overlap {
    let is_windows = cfg!(windows);
    let (root, legacy_dir) = match (root.canonicalize(), legacy_dir.canonicalize()) {
        (Ok(root), Ok(legacy_dir)) => (root, legacy_dir),
        _ => (root.to_path_buf(), legacy_dir.to_path_buf()),
    };
    overlap_of(
        &normalize_for_compare(&root, is_windows),
        &normalize_for_compare(&legacy_dir, is_windows),
    )
}

/// 既読の印を付ける（ダイアログを閉じた後）。今回付けたら `true`。既にある・
/// 書けないときは `false`。
pub(crate) fn acknowledge(marker: &Path) -> bool {
    banto_desktop_tray::claim_once_flag(marker)
}

/// 印のフォルダを作り、そこへ書けるか確かめる（だめなら案内しない。モジュール doc）。
fn marker_dir_ready(marker: &Path) -> bool {
    marker_dir_ready_with(marker, probe_write)
}

/// [`marker_dir_ready`] の本体（書けるかの確かめ方を差し替えてテストする）。
fn marker_dir_ready_with(marker: &Path, probe: impl Fn(&Path) -> std::io::Result<()>) -> bool {
    let Some(parent) = marker.parent() else {
        return false;
    };
    std::fs::create_dir_all(parent).is_ok() && probe(parent).is_ok()
}

/// `dir` に一時ファイルを作って消す（印そのものの名前は使わない。新規に作れること
/// を確かめるので `create_new`）。消せなかった一時ファイルは害が無いので残す。
fn probe_write(dir: &Path) -> std::io::Result<()> {
    let probe = dir.join(format!(".legacy-notice-probe-{}", std::process::id()));
    let _ = std::fs::remove_file(&probe);
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)?;
    std::fs::remove_file(&probe)
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
    // 今のルートが旧フォルダと同じか、その中なら、削除を勧める案内を出さない
    // （印も付けない。モジュール doc「出す条件」）。
    if let Some(legacy_dir) = legacy_db.as_deref().and_then(Path::parent) {
        if !overlap(layout.root(), legacy_dir).allows_notice() {
            eprintln!(
                "banto: データのルート（{}）が以前の版のデータのフォルダ（{}）と同じかその中にあるため、旧データの案内を出しません",
                layout.root().display(),
                legacy_dir.display()
            );
            return;
        }
    }
    let Some(marker) = marker_path(app) else {
        return;
    };
    if !should_show(legacy_db.as_deref(), &marker) {
        return;
    }
    if !marker_dir_ready(&marker) {
        eprintln!(
            "banto: 旧データの案内の既読の印のフォルダ（{}）に書けないため、案内を出しません",
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
        assert!(text.body.contains("削除しないでください"), "{}", text.body);
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

    /// PR #606 の Codex P2: フォルダがあっても書けなければ出さない。書けるかは実際に
    /// 一時ファイルを作って確かめ、印は付けず、一時ファイルも残さない。
    #[test]
    fn readiness_requires_an_actual_write_into_the_marker_folder() {
        let local = tempfile::tempdir().unwrap();
        let dir = local.path().join("dev.tyaro.chronogazer");
        let marker = dir.join(MARKER_FILE_NAME);
        // 書けるフォルダ: 本物の確かめ方で通り、何も残さない。
        assert!(marker_dir_ready(&marker));
        assert!(dir.is_dir());
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            0,
            "印も一時ファイルも残さない"
        );
        // フォルダはあるが書けない: 出さない。確かめ方はフォルダを受け取る。
        let seen = std::cell::RefCell::new(None);
        let denied = |probe_dir: &Path| {
            *seen.borrow_mut() = Some(probe_dir.to_path_buf());
            Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
        };
        assert!(!marker_dir_ready_with(&marker, denied));
        assert_eq!(seen.borrow().as_deref(), Some(dir.as_path()));
        assert!(!marker.exists());
    }

    /// 本物の書き込みの確かめ（`probe_write`）が、書けないフォルダで失敗すること
    /// （Windows: 今の利用者を書き込み拒否にした ACL のフォルダ）。
    #[cfg(windows)]
    #[test]
    fn probe_write_fails_in_a_folder_the_user_cannot_write() {
        let local = tempfile::tempdir().unwrap();
        let dir = local.path().join("ro");
        std::fs::create_dir(&dir).unwrap();
        let me = std::env::var("USERNAME").unwrap();
        let deny = std::process::Command::new("icacls")
            .arg(&dir)
            .args(["/deny", &format!("{me}:(W,AD)")])
            .output()
            .expect("icacls");
        assert!(deny.status.success(), "{deny:?}");
        let result = probe_write(&dir);
        // 後片付けのために拒否を外す。
        let _ = std::process::Command::new("icacls")
            .arg(&dir)
            .args(["/remove:d", &me])
            .output();
        assert!(result.is_err(), "書けないフォルダで成功した");
        assert!(probe_write(&dir).is_ok(), "拒否を外せば書ける");
    }

    fn norm(p: &str, win: bool) -> Vec<String> {
        normalize_for_compare(Path::new(p), win)
    }

    /// PR #606 のオーナーレビュー P2: ルートが旧フォルダと同じ・その中なら案内しない。
    /// 大文字小文字・区切り・末尾の区切り・`\\?\` の違いに強い。
    #[test]
    fn overlap_classifies_roots_against_the_legacy_folder() {
        let legacy = norm(r"C:\Users\a\AppData\Roaming\dev.tyaro.chronogazer", true);
        let cases = [
            (r"C:\ProgramData\ChronoGazer", Overlap::Separate),
            (
                r"C:\Users\a\AppData\Roaming\dev.tyaro.chronogazer",
                Overlap::Same,
            ),
            (
                r"c:/users/A/appdata/roaming/DEV.TYARO.CHRONOGAZER/",
                Overlap::Same,
            ),
            (
                r"\\?\C:\Users\a\AppData\Roaming\dev.tyaro.chronogazer\",
                Overlap::Same,
            ),
            (
                r"C:\Users\a\AppData\Roaming\dev.tyaro.chronogazer\new-root",
                Overlap::RootInsideLegacy,
            ),
            (
                r"C:/USERS/a/AppData/Roaming/dev.tyaro.chronogazer//x\y",
                Overlap::RootInsideLegacy,
            ),
            (r"C:\Users\a\AppData\Roaming", Overlap::LegacyInsideRoot),
            // 名前の前方一致は重なりではない（要素単位）。
            (
                r"C:\Users\a\AppData\Roaming\dev.tyaro.chronogazer2",
                Overlap::Separate,
            ),
            (
                r"D:\Users\a\AppData\Roaming\dev.tyaro.chronogazer",
                Overlap::Separate,
            ),
        ];
        for (root, expected) in cases {
            assert_eq!(overlap_of(&norm(root, true), &legacy), expected, "{root}");
        }
        assert!(Overlap::Separate.allows_notice());
        assert!(Overlap::LegacyInsideRoot.allows_notice());
        assert!(!Overlap::Same.allows_notice());
        assert!(!Overlap::RootInsideLegacy.allows_notice());
        // UNC の `\\?\UNC\` も外す。
        assert_eq!(
            norm(r"\\?\UNC\srv\share\x", true),
            norm(r"\\srv\share\X\", true)
        );
        // 非 Windows は大文字小文字を区別し、`\` は区切りではない。
        let legacy_posix = norm("/home/a/.local/dev.tyaro.chronogazer", false);
        assert_eq!(
            overlap_of(
                &norm("/home/a/.local/dev.tyaro.chronogazer/r/", false),
                &legacy_posix
            ),
            Overlap::RootInsideLegacy
        );
        assert_eq!(
            overlap_of(
                &norm("/home/a/.local/DEV.TYARO.CHRONOGAZER", false),
                &legacy_posix
            ),
            Overlap::Separate
        );
    }

    /// 実在のフォルダで: 旧フォルダの中のルート・同じルートは案内しない、独立した
    /// ルートと旧フォルダを含むルートは案内する（`canonicalize` の経路）。
    #[test]
    fn overlap_on_real_folders() {
        let base = tempfile::tempdir().unwrap();
        let legacy = base.path().join("dev.tyaro.chronogazer");
        let inside = legacy.join("new-root");
        let separate = base.path().join("ChronoGazer");
        for dir in [&inside, &separate] {
            std::fs::create_dir_all(dir).unwrap();
        }
        assert_eq!(overlap(&inside, &legacy), Overlap::RootInsideLegacy);
        assert_eq!(overlap(&legacy, &legacy), Overlap::Same);
        assert_eq!(overlap(&separate, &legacy), Overlap::Separate);
        assert_eq!(overlap(base.path(), &legacy), Overlap::LegacyInsideRoot);
        // まだ無いルート（canonicalize できない）も文字列の正規化で同じ判定。
        assert_eq!(
            overlap(&legacy.join("not-yet"), &legacy),
            Overlap::RootInsideLegacy
        );
    }
}
