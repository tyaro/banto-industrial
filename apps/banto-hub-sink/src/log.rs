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
//!
//! ## サービスログファイルの置き場（I4 追従、
//! docs/banto-hub-installer-design.md §8.2、2026-09-07）
//!
//! 旧仕様は exe と同じディレクトリ（`Program Files\BantoHub\` 配下）
//! 固定だった。ここは管理者権限でしか書けない上、サービス実行中はファイル
//! が開きっぱなしになるため、アンインストール時の `$INSTDIR` 削除がロック
//! で失敗し、`Program Files` 配下にディレクトリが残ってしまう問題が
//! あった。[`resolve_service_log_path`] はこれを
//! `%ProgramData%\BantoHub\logs\banto-hub-sink-service.log`
//! （利用者権限で書ける・サービス削除後もディレクトリごと消せる）へ
//! 変更する。banto-hub 自身のサービスログ（`apps/banto-hub/core/src/
//! hub_log.rs::resolve_service_log_dir` → `profile_paths::
//! ProfilePaths::logs_dir`、既定 root は同じ `%ProgramData%\BantoHub`）と
//! 揃えた場所 - サイドカーには banto-hub のような profile 概念（複数
//! インスタンス）が無いので、root 直下の `logs\` をそのまま使う。

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// サービスログの既定ファイル名（`banto_hub_core::hub_log::
/// SERVICE_LOG_FILE_NAME` = `banto-hub-service.log` と並ぶ名前）。
pub const SERVICE_LOG_FILE_NAME: &str = "banto-hub-sink-service.log";

/// `%ProgramData%` 配下でインストーラが作るサブディレクトリ名
/// （`crate::config` の同名 private const と同じ値。2箇所の複製は許容
/// するが値のドリフトは許容しない - `service.rs` の
/// `service_name_matches_shell_side_sink_constant` と同じ判断）。
const PROGRAM_DATA_SUBDIR: &str = "BantoHub";

/// サービスログファイルのパスを明示的に上書きする環境変数（テスト・
/// 複数インスタンス検証用）。[`crate::config::ENV_CONFIG_PATH`] と違い
/// **ファイルそのもの**のパスを渡す（ディレクトリではない）。
pub const ENV_SERVICE_LOG_PATH: &str = "BANTO_HUB_SINK_LOG";

static SERVICE_LOG_FILE: OnceLock<Mutex<Option<File>>> = OnceLock::new();

/// サービスログファイルのパスを解決する（[`crate::service::
/// run_service_body`] が [`enable_service_log_file`] を呼ぶ前に使う）。
///
/// - [`ENV_SERVICE_LOG_PATH`]（`BANTO_HUB_SINK_LOG`）が設定されていれば
///   最優先でそのまま使う（テスト・複数インスタンス検証用。存在確認は
///   しない - `config::resolve_config_path` の env 優先と同じ判断）。
/// - Windows は `%ProgramData%\BantoHub\logs\banto-hub-sink-service.log`
///   （`%ProgramData%` が取得できない異常系のみ exe 隣にフォールバック -
///   旧仕様のまま）。
/// - 非 Windows は exe と同じディレクトリ（旧仕様のまま。本番は Windows
///   専用製品なのでこの分岐が使われることはない）。
///
/// ディレクトリの作成はここでは行わない - [`enable_service_log_file`] が
/// 親ディレクトリを `create_dir_all` する。
pub fn resolve_service_log_path(exe_dir: Option<&Path>) -> PathBuf {
    resolve_service_log_path_impl(
        cfg!(windows),
        std::env::var_os(ENV_SERVICE_LOG_PATH).map(PathBuf::from),
        std::env::var_os("ProgramData"),
        exe_dir,
    )
}

/// [`resolve_service_log_path`] の純粋な内側 - `is_windows`/env 読み取りを
/// 引数として外に出してあるので、実際の OS 判定・`%ProgramData%` に一切
/// 触れずに両分岐をユニットテストできる（`profile_paths::
/// resolve_hub_root_impl` と同じ手法。CI（非 Windows ランナー）でも
/// Windows 側のパス組み立てを検証できる）。
fn resolve_service_log_path_impl(
    is_windows: bool,
    env_override: Option<PathBuf>,
    program_data: Option<std::ffi::OsString>,
    exe_dir: Option<&Path>,
) -> PathBuf {
    if let Some(path) = env_override {
        return path;
    }
    if is_windows {
        if let Some(root) = program_data {
            return PathBuf::from(root)
                .join(PROGRAM_DATA_SUBDIR)
                .join("logs")
                .join(SERVICE_LOG_FILE_NAME);
        }
    }
    match exe_dir {
        Some(dir) => dir.join(SERVICE_LOG_FILE_NAME),
        None => PathBuf::from(SERVICE_LOG_FILE_NAME),
    }
}

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

    // --- resolve_service_log_path（I4 追従・モジュール doc「サービスログ
    // --- ファイルの置き場」参照）。`resolve_service_log_path_impl` を直接
    // --- 呼び、実際の env var/OS 判定に触れずに両分岐を検証する。

    #[test]
    fn env_override_wins_regardless_of_os_or_program_data() {
        let overridden = PathBuf::from(r"D:\custom\sink.log");
        for is_windows in [true, false] {
            let resolved = resolve_service_log_path_impl(
                is_windows,
                Some(overridden.clone()),
                Some(std::ffi::OsString::from(r"C:\ProgramData")),
                Some(Path::new(r"C:\Program Files\BantoHub")),
            );
            assert_eq!(resolved, overridden);
        }
    }

    #[test]
    fn windows_defaults_to_program_data_logs_subdir() {
        let resolved = resolve_service_log_path_impl(
            true,
            None,
            Some(std::ffi::OsString::from(r"C:\ProgramData")),
            Some(Path::new(r"C:\Program Files\BantoHub")),
        );
        assert_eq!(
            resolved,
            PathBuf::from(r"C:\ProgramData\BantoHub\logs\banto-hub-sink-service.log")
        );
    }

    #[test]
    fn windows_falls_back_to_exe_dir_when_program_data_is_unavailable() {
        let resolved = resolve_service_log_path_impl(
            true,
            None,
            None,
            Some(Path::new(r"C:\Program Files\BantoHub")),
        );
        assert_eq!(
            resolved,
            PathBuf::from(r"C:\Program Files\BantoHub\banto-hub-sink-service.log")
        );
    }

    #[test]
    fn windows_falls_back_to_bare_file_name_when_nothing_is_available() {
        let resolved = resolve_service_log_path_impl(true, None, None, None);
        assert_eq!(resolved, PathBuf::from(SERVICE_LOG_FILE_NAME));
    }

    #[test]
    fn non_windows_ignores_program_data_and_uses_exe_dir() {
        let resolved = resolve_service_log_path_impl(
            false,
            None,
            Some(std::ffi::OsString::from(r"C:\ProgramData")),
            Some(Path::new("/opt/banto-hub-sink")),
        );
        assert_eq!(
            resolved,
            PathBuf::from("/opt/banto-hub-sink/banto-hub-sink-service.log")
        );
    }

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
