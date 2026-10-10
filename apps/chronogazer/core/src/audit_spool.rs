//! #437（banto v6.5.0 の `AuditLogService::with_spool`、banto ADR-0019）:
//! 監査の保留を ChronoGazer に配線する場所。`banto-serve`（LAN/REST の単体
//! サーバー）と Tauri のデスクトップアプリの**両方**が [`build_audit_service`]
//! を通る（片方だけ保留が付く状態にしない）。banto-hub の同名モジュール
//! （`apps/banto-hub/core/src/audit_spool.rs`）と同じ設計。
//!
//! ## 何を解くか
//!
//! 監査（`audit_log`）は設定・タグ・接続・ユーザーの変更など、ほとんどの操作が
//! 記録する。DB に書けない（表が壊れている・ディスクが満杯・pool を握られて
//! 応答しない）と、これまでは `record` が失敗を標準エラー出力に出して**監査を
//! 失う**か、DB の応答を待って要求を止めていた。保留を付けると、
//! `AuditLogService::record` を通る監査（REST・Tauri の両方）は:
//!
//! - INSERT を最大 [`SpoolConfig::timeout`]（既定 3 秒）だけ待ち、失敗・時間切れ
//!   なら [`spool_dir`] に 1 件 1 ファイルで退避して戻る（要求を待たせない）。
//! - 後で同じ `pending_id` で `audit_log` に流し込まれる（`detail.spooled = true`）。
//!   時間切れの INSERT が遅れて完了しても、`pending_id` の一意インデックス
//!   （マイグレーション `0008`）で 1 行になる。
//!
//! 流し込みのきっかけは 3 つ: 起動時（呼び出し側がマイグレーションの後に 1 回
//! `flush_spool` を呼ぶ）、監査の書き込みの成功、定期実行
//! （`spawn_spool_flusher`、既定 30 秒ごと。起こすのは**最後に失敗しうる起動手順
//! の後**で、止めるのは終了時。呼び出し側の責務）。`try_record`（監査に失敗したら
//! 操作も失敗させる）は保留を通らない。
//!
//! ## 置き場所
//!
//! **DB ファイルの置き場所**（Tauri ではアプリのデータディレクトリ、`banto-serve`
//! では `BANTO_DB` の親）の中の [`SPOOL_DIR_NAME`]。`backups/` とリストア予約と
//! 同じ「DB ごとのファイルは DB の隣」の決まり。時系列データのディレクトリ
//! （`data.dir` 設定）の中には**置かない**: あれは設定で動かせるので、動かすと
//! 保留が取り残され、しかも DB が壊れている最中は設定を読めない。tstore の
//! 剪定・一覧は `YYYYMMDD-NNN.sqlite3` のファイルしか見ないので、どのみち
//! サブディレクトリは触られない。中身は監査の内容そのもの（`detail` を含む）。
//!
//! ## 作れなかったとき
//!
//! 保留ディレクトリを作れない・読めない（[`AuditLogService::with_spool`] の
//! `Err`）ときは、起動を止めずに**保留なし**（従来の `record`）で続け、標準
//! エラー出力に警告を出す。収集や設定の変更を、監査の退避先が無いことを理由に
//! 使えなくするほうが危ないと判断した。
//!
//! ## ログの出口
//!
//! ChronoGazer にはログファイルが無い（警告はすべて `eprintln!`）ので、
//! banto-hub と違って `with_log_sink` は付けず、banto の既定（標準エラー出力）の
//! ままにしている。状態は監査ログ画面の警告帯（`GET /api/audit-log/spool`）で
//! 見える。

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::audit::{AuditLogService, SpoolConfig};
use crate::db::Db;

/// DB ファイルの置き場所の中の、監査の保留ディレクトリの名前。
pub const SPOOL_DIR_NAME: &str = "audit-spool";

/// `db_dir`（DB ファイルのあるディレクトリ）の中の監査の保留ディレクトリ。
pub fn spool_dir(db_dir: &Path) -> PathBuf {
    db_dir.join(SPOOL_DIR_NAME)
}

/// ChronoGazer の監査サービスを組む: [`spool_dir`] に保留を付ける
/// （[`SpoolConfig::default`]: 3 秒・10,000 件・30 秒）。保留を開けなければ
/// 警告を出して保留なしで返す（モジュール doc「作れなかったとき」）。
///
/// 起動時の流し込み（`flush_spool`）と定期実行（`spawn_spool_flusher`）は
/// 呼び出し側が行う。
pub fn build_audit_service(db: Db, db_dir: &Path) -> AuditLogService {
    let dir = spool_dir(db_dir);
    match AuditLogService::new(db.clone()).with_spool(&dir, SpoolConfig::default()) {
        Ok(audit) => audit,
        Err(err) => {
            eprintln!(
                "chronogazer: [WARN] 監査の保留ディレクトリ（{}）を使えません。DB に書けない\
                 監査は保留されず失われます: {err}",
                dir.display()
            );
            AuditLogService::new(db)
        }
    }
}

/// 起動時の流し込み。前回の実行で残った保留を `audit_log` へ入れる
/// （マイグレーション済みであること）。結果は標準エラー出力に 1 行出すだけで、
/// 起動は止めない。
pub async fn flush_at_startup(audit: &AuditLogService, who: &str) {
    match audit.flush_spool().await {
        Ok(report) => {
            if report.flushed > 0
                || report.remaining > 0
                || report.quarantined > 0
                || report.error.is_some()
            {
                eprintln!(
                    "{who}: 監査の保留: 起動時に {} 件を DB へ流し込みました\
                     （残り {} 件、読めずに隔離 {} 件{}）",
                    report.flushed,
                    report.remaining,
                    report.quarantined,
                    report
                        .error
                        .as_ref()
                        .map(|err| format!("、中断: {err}"))
                        .unwrap_or_default()
                );
            }
        }
        Err(err) => eprintln!("{who}: [WARN] 監査の保留ディレクトリを読めませんでした: {err}"),
    }
}

/// `GET /api/audit-log/spool` と Tauri の `audit_spool_status` の応答
/// （camelCase）。banto-hub の `audit_pending_*` と同じ 4 項目。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditSpoolStatus {
    /// 流し込み待ちの件数。
    pub pending_count: u64,
    /// 最も古い保留の `ts`（`audit_log.ts` と同じ UTC の文字列）。
    pub pending_oldest_ts: Option<String>,
    /// 上限（10,000 件）を超えて捨てた件数（このプロセスの起動後）。
    pub dropped_count: u64,
    /// 保留ファイルにも書けずに失った件数（このプロセスの起動後）。
    pub failed_count: u64,
}

/// 保留の状態（保留なしで組んだサービスは全部 0）。
pub fn spool_status(audit: &AuditLogService) -> AuditSpoolStatus {
    let backlog = audit.spool_backlog();
    AuditSpoolStatus {
        pending_count: backlog.count,
        pending_oldest_ts: backlog.oldest_ts,
        dropped_count: backlog.dropped,
        failed_count: backlog.failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_spool_lives_next_to_the_db_and_is_created() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init_db_memory().await.unwrap();
        let audit = build_audit_service(Db::Sqlite(pool), dir.path());
        assert!(dir.path().join(SPOOL_DIR_NAME).is_dir());
        assert_eq!(spool_status(&audit), AuditSpoolStatus::default());
        assert!(audit.spawn_spool_flusher().is_some_and(|h| {
            h.abort();
            true
        }));
    }

    /// 保留ディレクトリを作れない（同名のファイルがある）ときは、起動を
    /// 止めずに保留なしで返す。
    #[tokio::test]
    async fn an_unusable_spool_dir_falls_back_to_no_spool() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(SPOOL_DIR_NAME), b"not a directory").unwrap();
        let pool = crate::db::init_db_memory().await.unwrap();
        let audit = build_audit_service(Db::Sqlite(pool), dir.path());
        assert!(audit.spawn_spool_flusher().is_none(), "no spool");
    }
}
