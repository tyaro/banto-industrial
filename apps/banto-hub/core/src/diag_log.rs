//! T9-2 フォローアップ（PR #43 監査指摘、2026-08-06 対応）: `hub.rs` の
//! [`crate::hub::CollectorManager`] は、稼働中の SLMP シミュレーションモード
//! 接続を知らせる警告（`log_simulation_warnings`）や `rebuild`/
//! `sync_slmp_sessions` の診断ログを素の `println!`/`eprintln!` で出力して
//! いた - `bin/banto_hub` の `hub_log`（`{data_dir}/banto-hub-service.log`
//! へのミラー、`hub_log` のモジュール doc 参照）はバイナリクレート限定で、
//! ライブラリクレート `banto-hub-core` からは届かないため。結果、Windows
//! サービスモード（コンソール無し）ではこれらの診断が誰にも見えなくなって
//! いた - 同種の診断が `bin/banto_hub` 側では `hub_log::log_line`/
//! `log_err_line` 経由でサービスログファイルにも残るのと非対称だった。
//!
//! [`DiagLog`] はこの非対称を解消する注入可能なコールバック対 - `bin/
//! banto_hub` が [`crate::hub::CollectorManager::with_diag_log`] で
//! `hub_log::log_line`/`log_err_line` を配線し、それ以外（テスト・将来の
//! 他バイナリ）は [`DiagLog::default`] を使う。`DiagLog::default` は素の
//! `println!`/`eprintln!` とバイト単位で同じ出力しかしない - `hub_log`
//! 自身がコンソールモードで守っている契約と同じもの。

use std::sync::Arc;

type LogFn = Arc<dyn Fn(&str) + Send + Sync>;

/// ライブラリクレートの診断ログを、バイナリクレート限定のログシンク
/// （例: `bin/banto_hub` の `hub_log`）へ、ライブラリ側がバイナリクレートに
/// 依存することなく届けるための注入可能な stdout/stderr 1行コールバック対。
/// このモジュールの doc comment 参照。
#[derive(Clone)]
pub struct DiagLog {
    line: LogFn,
    err_line: LogFn,
}

impl DiagLog {
    /// `line`/`err_line` は `println!`/`eprintln!` と同じ「1行、`msg` に
    /// 末尾改行を含めない」契約を守ること - 呼び出し側は整形済みの
    /// メッセージを渡すだけで、改行を付けるのはコールバック側の責務
    /// （`hub_log::log_line`/`log_err_line` 自身の契約と揃える）。
    pub fn new(
        line: impl Fn(&str) + Send + Sync + 'static,
        err_line: impl Fn(&str) + Send + Sync + 'static,
    ) -> Self {
        Self {
            line: Arc::new(line),
            err_line: Arc::new(err_line),
        }
    }

    pub fn line(&self, msg: &str) {
        (self.line)(msg);
    }

    pub fn err_line(&self, msg: &str) {
        (self.err_line)(msg);
    }
}

impl Default for DiagLog {
    /// 素の `println!`/`eprintln!` とバイト単位で同じ - `hub_log` 自身が
    /// コンソールモードで使っているのと同じフォールバック。
    fn default() -> Self {
        Self::new(|msg| println!("{msg}"), |msg| eprintln!("{msg}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn line_and_err_line_invoke_the_injected_callbacks() {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let errs = Arc::new(Mutex::new(Vec::new()));
        let (l, e) = (lines.clone(), errs.clone());
        let diag = DiagLog::new(
            move |msg| l.lock().unwrap().push(msg.to_string()),
            move |msg| e.lock().unwrap().push(msg.to_string()),
        );
        diag.line("hello");
        diag.err_line("world");
        assert_eq!(*lines.lock().unwrap(), vec!["hello".to_string()]);
        assert_eq!(*errs.lock().unwrap(), vec!["world".to_string()]);
    }
}
