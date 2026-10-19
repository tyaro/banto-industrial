//! デスクトップのログ（#661 661-5、docs/banto-hub-t17-design.md §1 の「2026-10-19 オーナー決定
//! （#661）」の「デスクトップのログ」）。
//!
//! 設定モードへの切り替え（サービスの停止・`relax-only`・昇格ヘルパーの修復）の開始・完了と、
//! 終了コード 5（締めたまま残った）の案内を、profile の `logs\banto-hub-desktop-YYYY-MM-DD.log`
//! に**起きた時刻で**書く。手本は ChronoGazer の `chronogazer_core::mode_switch` の
//! `DesktopLogEntry`・`append_desktop_log`（#623 PR-4）。
//!
//! 運用モードの間は `logs\` が利用者に読み取りだけなので、シェルは行を起きた時刻とともに
//! 覚えておき（[`DesktopLogEntry`]）、デスクトップの Hub が動き始めてから（設定モードに
//! なってから）まとめて書く。書けないことで切り替えを止めない（失敗は呼び出し側が
//! 標準エラー出力へ出す）。

use std::io::Write;
use std::path::{Path, PathBuf};

use banto_tstore::{Clock, LocalDate};

/// ファイル名の先頭（`banto-hub-desktop-YYYY-MM-DD.log`）。
pub const DESKTOP_LOG_FILE_PREFIX: &str = "banto-hub-desktop-";

/// 運用モード → 設定モードの切り替え（サービスを止める）を始めた行。
pub const LOG_TO_SETTING_STARTED: &str =
    "設定モードへの切り替えを開始（サービスを停止し、profile のファイルの権限を戻します）";

/// 「設定モードに戻す」（サービスを relax-only で開始する）を始めた行。
pub const LOG_RESTORE_STARTED: &str =
    "設定モードに戻す操作を開始（サービスを relax-only で開始し、profile のファイルの権限を戻します）";

/// 設定モードへの切り替えを終えた行（デスクトップの Hub が動き始めた後）。
pub const LOG_TO_SETTING_COMPLETED: &str =
    "設定モードへの切り替えを完了（この PC の中で Hub を起動しました）";

/// サービスが終了コード 5（締めたまま残った）で止まったときの案内の行。
pub const LOG_LEFT_TIGHT: &str = "サービスは profile のファイルの権限を元に戻せませんでした（終了コード 5。運用モードのまま残っています）。画面かトレイの「設定モードに戻す」か、管理者が banto-hub-elev.exe grant-profile-acl で戻します";

/// `relax-only`・昇格ヘルパーの修復で設定モードに戻せたときの行。
pub fn log_restored_by(how: &str) -> String {
    format!("profile のファイルの権限を設定モードに戻しました（{how}）")
}

/// 設定モードに戻せなかったときの行。
pub fn log_restore_failed(reason: &str) -> String {
    format!("設定モードに戻せませんでした（{reason}）")
}

/// デスクトップのログの 1 行（書く時刻ではなく、起きた時刻を持つ）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopLogEntry {
    pub at_ms: i64,
    pub text: String,
}

impl DesktopLogEntry {
    /// 今の時刻の行。
    pub fn now(clock: &dyn Clock, text: impl Into<String>) -> Self {
        Self {
            at_ms: clock.now_ms(),
            text: text.into(),
        }
    }

    /// 今の時刻（OS の時計）の行。
    pub fn now_system(text: impl Into<String>) -> Self {
        Self::now(&banto_tstore::SystemClock, text)
    }
}

/// その日のファイル名（純関数）。
pub fn desktop_log_file_name(date: LocalDate) -> String {
    format!(
        "{DESKTOP_LOG_FILE_PREFIX}{:04}-{:02}-{:02}.log",
        date.year, date.month, date.day
    )
}

/// `YYYY-MM-DD HH:MM:SS`（地方時。純関数）。
pub fn format_timestamp(epoch_ms: i64, utc_offset_ms: i64) -> String {
    const MS_PER_DAY: i64 = 86_400_000;
    let date = LocalDate::from_epoch_ms(epoch_ms, utc_offset_ms);
    let ms_of_day = (epoch_ms + utc_offset_ms).rem_euclid(MS_PER_DAY);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        date.year,
        date.month,
        date.day,
        ms_of_day / 3_600_000,
        (ms_of_day / 60_000) % 60,
        (ms_of_day / 1_000) % 60
    )
}

/// 1 行の文字列（純関数）。
pub fn desktop_log_line(entry: &DesktopLogEntry, utc_offset_ms: i64) -> String {
    format!(
        "[{}] {}",
        format_timestamp(entry.at_ms, utc_offset_ms),
        entry.text
    )
}

/// 行を今日のファイルに足す（`logs_dir` が無ければ作る）。**設定モードの間だけ呼ぶ**。
pub fn append_desktop_log(
    logs_dir: &Path,
    clock: &dyn Clock,
    entries: &[DesktopLogEntry],
) -> std::io::Result<PathBuf> {
    let offset_ms = clock.utc_offset_ms();
    let today = LocalDate::from_epoch_ms(clock.now_ms(), offset_ms);
    std::fs::create_dir_all(logs_dir)?;
    let path = logs_dir.join(desktop_log_file_name(today));
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    let mut text = String::new();
    for entry in entries {
        text.push_str(&desktop_log_line(entry, offset_ms));
        text.push('\n');
    }
    file.write_all(text.as_bytes())?;
    file.flush()?;
    Ok(path)
}

/// [`append_desktop_log`] を OS の時計で（シェルが使う）。
pub fn append_desktop_log_now(
    logs_dir: &Path,
    entries: &[DesktopLogEntry],
) -> std::io::Result<PathBuf> {
    append_desktop_log(logs_dir, &banto_tstore::SystemClock, entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use banto_tstore::ManualClock;

    const JST: i64 = 9 * 3_600_000;

    #[test]
    fn file_name_and_line_use_the_local_date_and_time_the_entry_happened() {
        // 2026-10-19T15:30:05Z = JST 2026-10-20 00:30:05
        let at = 1_792_423_805_000;
        let clock = ManualClock::new(at, JST);
        let entry = DesktopLogEntry::now(&clock, LOG_TO_SETTING_STARTED);
        assert_eq!(
            desktop_log_file_name(LocalDate::from_epoch_ms(at, JST)),
            "banto-hub-desktop-2026-10-20.log"
        );
        assert_eq!(
            desktop_log_line(&entry, JST),
            format!("[2026-10-20 00:30:05] {LOG_TO_SETTING_STARTED}")
        );
    }

    #[test]
    fn append_writes_entries_in_order_with_their_own_times() {
        let dir = tempfile::tempdir().expect("tempdir");
        let logs = dir.path().join("logs");
        let started = DesktopLogEntry {
            at_ms: 1_792_423_805_000,
            text: LOG_RESTORE_STARTED.to_string(),
        };
        let done = DesktopLogEntry {
            at_ms: 1_792_423_865_000,
            text: LOG_TO_SETTING_COMPLETED.to_string(),
        };
        // 書く時刻は 2 つ目の 10 分後（ファイル名は書く日、各行は起きた時刻）。
        let clock = ManualClock::new(1_792_424_465_000, JST);
        let path = append_desktop_log(&logs, &clock, &[started, done]).expect("書ける");
        let text = std::fs::read_to_string(path).expect("読める");
        assert_eq!(
            text,
            format!(
                "[2026-10-20 00:30:05] {LOG_RESTORE_STARTED}\n[2026-10-20 00:31:05] {LOG_TO_SETTING_COMPLETED}\n"
            )
        );
    }

    #[test]
    fn the_left_tight_line_names_both_ways_back() {
        assert!(LOG_LEFT_TIGHT.contains("終了コード 5"));
        assert!(LOG_LEFT_TIGHT.contains("設定モードに戻す"));
        assert!(LOG_LEFT_TIGHT.contains("grant-profile-acl"));
    }
}
