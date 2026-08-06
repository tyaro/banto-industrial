//! T5-1（docs/tag-server-design.md §8「常駐」）: `bin/banto-hub.rs` 全体の
//! 出力ヘルパー。
//!
//! コンソールモードは元々すべて `println!`/`eprintln!` 直書きだった
//! （`log`/`tracing` クレートは未導入 - このワークスペースの依存追加は
//! 慎重に、という方針に沿って今回も導入しない）。Windows サービスとして
//! 動く場合はコンソールが存在しないため、`println!`/`eprintln!` の出力は
//! 誰にも見えない - そこで [`log_line`]/[`log_err_line`] という薄い置き換え
//! を用意し、サービスモードの間だけ同じ内容をファイルにもミラーする。
//!
//! **コンソールモードの出力は一切変更しない**: [`enable_service_log_file`]
//! を一度も呼ばなければ（＝コンソールモード）、[`log_line`]/
//! [`log_err_line`] は素の `println!`/`eprintln!` と バイト単位で同じ出力
//! しかしない（フォーマット済み文字列をそのまま渡すだけ - `println!`同様、
//! 末尾に改行が1つ付く）。

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use banto_tstore::{Clock, LocalDate, SystemClock};

/// サービスログファイルの既定ファイル名（`{data_dir}/banto-hub-service.log`
/// - docs/banto-hub-operations.md に記載するパスと一致させる）。
pub const SERVICE_LOG_FILE_NAME: &str = "banto-hub-service.log";

static SERVICE_LOG_FILE: OnceLock<Mutex<Option<File>>> = OnceLock::new();

/// `path` を作成（既存なら追記）で開き、以降 [`log_line`]/[`log_err_line`]
/// が呼ばれるたびにタイムスタンプ付きの1行を追記する状態にする。
/// `win_service::run_service_main` が `hub_run::run` を呼ぶ**前**に一度だけ
/// 呼ぶ - コンソールモードは一度も呼ばない。
///
/// 親ディレクトリが無ければ作成する（初回インストール直後は `data_dir`
/// 自体がまだ存在しないことがあるため）。失敗は呼び出し側が判断する
/// （このファイル自身は「ログ出力先が無い」以上のことを知らないので
/// fatal 扱いにしない - `hub_run.rs`の他の起動処理と同じ「失敗してもプロセス
/// 継続」の作法）。
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

/// サービスログファイルを格納するディレクトリの解決 - `BANTO_HUB_DATA`
/// 環境変数（未設定なら既定 `"./data"`）のみを見る、**あえて**簡略化した
/// 解決規則。
///
/// `bin/hub_run.rs`側の実際の `data_dir` は「環境変数 → 設定 DB の
/// `data.dir` → 既定値」の3層だが、設定 DB を読むには非同期の DB
/// 接続が要る - このファイルはサービスモードで `hub_run::run` が最初の
/// 1行を出力する**前**にログファイルを開き終えている必要があるため、
/// DB を読まずに済む env 変数だけの層で解決する。両者は env 変数を
/// 設定していれば必ず一致し、設定 DB だけで `data.dir` をカスタマイズ
/// している運用でのみ食い違う（その場合ログファイルは既定
/// `"./data"` 配下に残る - docs/banto-hub-operations.md に明記）。
pub fn resolve_service_log_dir() -> PathBuf {
    resolve_service_log_dir_from(std::env::var("BANTO_HUB_DATA").ok())
}

/// [`resolve_service_log_dir`]の純粋な内側 - 実際の env var 読み取りを外に
/// 追い出してあるので、`std::env::set_var`（テスト間で共有されるプロセス
/// グローバル状態 - 並行実行される他のテストとの競合を避けたい）に一切
/// 触れずにユニットテストできる。
fn resolve_service_log_dir_from(env_value: Option<String>) -> PathBuf {
    PathBuf::from(env_value.unwrap_or_else(|| "./data".to_string()))
}

fn mirror_to_service_log(line: &str) {
    let Some(slot) = SERVICE_LOG_FILE.get() else {
        return;
    };
    let Ok(mut guard) = slot.lock() else {
        return;
    };
    let Some(file) = guard.as_mut() else {
        return;
    };
    let clock = SystemClock;
    let timestamp = format_timestamp(clock.now_ms(), clock.utc_offset_ms());
    // 書き込み失敗（ディスクフル等）はここでも fatal にしない - 次の呼び出し
    // でまた試みるだけでよい。
    let _ = writeln!(file, "[{timestamp}] {line}");
}

/// `epoch_ms`（UTC）を `utc_offset_ms` だけシフトしたローカル時刻の
/// `YYYY-MM-DD HH:MM:SS` 表記。`banto_tstore::LocalDate::from_epoch_ms`と
/// 同じ入力ペアを取り、日付部分はそれに委譲する - 新規に日付/時刻
/// クレートを増やさないため（`banto-tstore`自身が同じ理由で `date.rs`に
/// 素の整数演算だけで実装している方針を踏襲）。
fn format_timestamp(epoch_ms: i64, utc_offset_ms: i64) -> String {
    let date = LocalDate::from_epoch_ms(epoch_ms, utc_offset_ms);
    const MS_PER_DAY: i64 = 86_400_000;
    let local_ms_of_day = (epoch_ms + utc_offset_ms).rem_euclid(MS_PER_DAY);
    let hour = local_ms_of_day / 3_600_000;
    let minute = (local_ms_of_day / 60_000) % 60;
    let second = (local_ms_of_day / 1_000) % 60;
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        date.year, date.month, date.day, hour, minute, second
    )
}

/// `println!`相当 + （サービスモードのみ）ログファイルへのミラー。
pub fn log_line(msg: &str) {
    println!("{msg}");
    mirror_to_service_log(msg);
}

/// `eprintln!`相当 + （サービスモードのみ）ログファイルへのミラー。
pub fn log_err_line(msg: &str) {
    eprintln!("{msg}");
    mirror_to_service_log(msg);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_timestamp_at_epoch_utc() {
        assert_eq!(format_timestamp(0, 0), "1970-01-01 00:00:00");
    }

    #[test]
    fn format_timestamp_applies_offset_across_midnight() {
        // 2026-07-11T23:30:00Z + 9h(JST) = 2026-07-12T08:30:00.
        let epoch_ms = 20_646 * 86_400_000 - 30 * 60_000;
        let jst_offset_ms = 9 * 3_600_000;
        assert_eq!(
            format_timestamp(epoch_ms, jst_offset_ms),
            "2026-07-12 08:30:00"
        );
    }

    #[test]
    fn format_timestamp_pads_single_digit_components() {
        // 2026-01-01T03:04:05Z, offset 0 (day 20454 - cross-checked against
        // banto_tstore::date's own known-date test vector: day 20646 =
        // 2026-07-12, and 2026-01-01 is 192 days earlier).
        let epoch_ms = (20_454 * 86_400_000) + 3 * 3_600_000 + 4 * 60_000 + 5 * 1_000;
        assert_eq!(format_timestamp(epoch_ms, 0), "2026-01-01 03:04:05");
    }

    #[test]
    fn resolve_service_log_dir_defaults_to_data_when_env_unset() {
        assert_eq!(resolve_service_log_dir_from(None), PathBuf::from("./data"));
    }

    #[test]
    fn resolve_service_log_dir_honors_env_override() {
        assert_eq!(
            resolve_service_log_dir_from(Some("/var/banto-hub/data".to_string())),
            PathBuf::from("/var/banto-hub/data")
        );
    }

    // `enable_service_log_file`は `SERVICE_LOG_FILE`というプロセスグローバル
    // な `OnceLock`に書き込む - 一度 Some になった後は同じテストバイナリ内の
    // 以後すべての `log_line`/`log_err_line`呼び出しがこのファイルへも
    // ミラーされ続ける（`OnceLock`は取り消せない）。本番では
    // `win_service::run_service_main`がプロセス起動ごとに高々1回だけ呼ぶので
    // 問題にならないが、テストでは他のテストとの意図しない相互作用を避ける
    // ため、このテストケース1つだけに留める（他のテストで `log_line`の
    // ファイルミラー有無をアサートしない）。ディレクトリは削除しない -
    // ファイルハンドルを開いたまま削除すると Windows では失敗し得るため
    // （このプロダクトの実行環境）、OS の一時ディレクトリに残しておく。
    #[test]
    fn enable_service_log_file_creates_parent_dir_and_appends() {
        let tmp = std::env::temp_dir().join(format!("banto-hub-log-test-{}", std::process::id()));
        let nested = tmp.join("nested");
        let log_path = nested.join(SERVICE_LOG_FILE_NAME);

        assert!(enable_service_log_file(&log_path).is_ok());
        log_line("test line one");
        log_err_line("test line two");

        let contents = std::fs::read_to_string(&log_path).expect("log file should exist");
        assert!(contents.contains("test line one"));
        assert!(contents.contains("test line two"));
        // タイムスタンプの角括弧が付いていること（フォーマット自体は
        // 上の format_timestamp のテストで別途検証済み）。
        assert!(contents.contains('['));
    }
}
