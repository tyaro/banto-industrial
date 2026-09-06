//! `println!`/`eprintln!` の薄いラッパー + サービスモードのファイル
//! ミラー。`banto_hub_core::hub_log` と同じ設計で、同じ理由による:
//! Windows サービスとして動く間はコンソールが無いので標準出力が誰にも
//! 見えない。
//!
//! **banto-hub の `hub_log` をそのまま使わない理由**: それを使うには
//! `banto-hub-core`（axum/tonic/rumqttc/sqlx-sqlite …、250 クレート）を
//! サイドカーの**本番**依存に入れることになる。サイドカーは Hub に HTTP
//! で繋ぐだけの小さなプロセスなので、40 行のログヘルパーのために依存木を
//! 丸ごと引き込む取引は割に合わない。文言・作法（タイムスタンプ付き
//! 1 行・書き込み失敗は fatal にしない・コンソールモードでは素の
//! `println!` と同じ出力）は `hub_log` に合わせてある。
//!
//! ログレベル機構は無い（このワークスペースは `log`/`tracing` を
//! アプリ側に導入しない方針）。`warn:` / `info:` は文字列の接頭辞として
//! 表現し、**抑制の規律**（初回とバックオフ段階・状態が変わったときだけ
//! `warn`、復帰時に `info` を 1 行）は呼び出し側が持つ（設計 §5.5）。

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

/// サービスログの既定ファイル名（`banto_hub_core::hub_log::
/// SERVICE_LOG_FILE_NAME` = `banto-hub-service.log` と並ぶ名前）。
pub const SERVICE_LOG_FILE_NAME: &str = "banto-hub-sink-service.log";

static SERVICE_LOG_FILE: OnceLock<Mutex<Option<File>>> = OnceLock::new();

/// `path` を作成（既存なら追記）で開き、以降 [`log_line`]/[`log_err_line`]
/// がタイムスタンプ付きの 1 行を追記する状態にする。サービスモードの
/// 入口（[`crate::service`]）だけが呼ぶ - コンソールモードは呼ばない。
pub fn enable_service_log_file(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let file = OpenOptions::new().create(true).append(true).open(path)?;
    let slot = SERVICE_LOG_FILE.get_or_init(|| Mutex::new(None));
    *slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(file);
    Ok(())
}

fn mirror_to_service_log(line: &str) {
    let Some(slot) = SERVICE_LOG_FILE.get() else {
        return;
    };
    let mut guard = slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(file) = guard.as_mut() else {
        return;
    };
    // 書き込み失敗（ディスクフル等）は fatal にしない - 次の呼び出しで
    // また試みるだけでよい（`hub_log` と同じ判断）。
    let _ = writeln!(file, "[{}] {line}", local_timestamp());
}

/// `YYYY-MM-DD HH:MM:SS`（OS のローカル時刻）。オフセットが取れない
/// 異常時は UTC で書く（`banto_tstore::SystemClock::utc_offset_ms` と同じ
/// フォールバック）。
fn local_timestamp() -> String {
    let now = time::OffsetDateTime::now_utc();
    let offset = time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC);
    let local = now.to_offset(offset);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        local.year(),
        local.month() as u8,
        local.day(),
        local.hour(),
        local.minute(),
        local.second()
    )
}

/// `println!` 相当 + （サービスモードのみ）ログファイルへのミラー。
pub fn log_line(msg: &str) {
    println!("{msg}");
    mirror_to_service_log(msg);
}

/// `eprintln!` 相当 + （サービスモードのみ）ログファイルへのミラー。
pub fn log_err_line(msg: &str) {
    eprintln!("{msg}");
    mirror_to_service_log(msg);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_has_the_expected_shape() {
        let rendered = local_timestamp();
        assert_eq!(rendered.len(), 19, "{rendered}");
        assert_eq!(&rendered[4..5], "-", "{rendered}");
        assert_eq!(&rendered[10..11], " ", "{rendered}");
        assert_eq!(&rendered[13..14], ":", "{rendered}");
    }

    /// ログファイルを開いていない（= コンソールモード）状態でも
    /// `log_line`/`log_err_line` は panic しない。
    #[test]
    fn logging_without_a_service_log_file_is_a_no_op() {
        log_line("banto-hub-sink: unit test line");
        log_err_line("banto-hub-sink: unit test error line");
    }
}
