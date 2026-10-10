//! #437: 監査の保留（banto v6.5.0 の `AuditLogService::with_spool`、banto
//! ADR-0019）を banto-hub に配線する場所。
//!
//! ## 何を解くか
//!
//! 緊急停止（`POST /api/write-control/disable`）は、DB が応答しなくても、
//! それまで有効だったセッションなら受け付ける（#431）。停止の状態は DB と
//! 状態ファイルの 2 か所に記録する（#433）。しかし監査（`audit_log`）は同じ
//! DB に書くので、DB 全体が落ちているあいだの監査は残らず、`record` が DB の
//! 応答を待つあいだ停止の応答も待たされていた。
//!
//! banto の保留を付けると、`AuditLogService::record` を通る監査（REST・MCP・
//! gRPC のすべて。緊急停止と書き込み制御の変更を含む）は:
//!
//! - INSERT を最大 [`SpoolConfig::timeout`]（既定 3 秒）だけ待ち、失敗・時間切れ
//!   なら [`spool_dir`] に 1 件 1 ファイルで退避して戻る（要求を待たせない）。
//! - 後で同じ `pending_id` で `audit_log` に流し込まれる（`detail.spooled = true`）。
//!   時間切れの INSERT が遅れて完了しても、`pending_id` の一意インデックス
//!   （マイグレーション `0008`）で 1 行になる。
//!
//! 流し込みのきっかけは 3 つ: 起動時（`crate::runtime::HubRuntime::start` が
//! マイグレーションの後に 1 回 `flush_spool` を呼ぶ）、監査の書き込みの成功、
//! 定期実行（`spawn_spool_flusher`、既定 30 秒ごと）。
//!
//! ## 置き場所
//!
//! データディレクトリ（`BANTO_HUB_DATA` 等の上書き適用後の実効値）の中の
//! [`SPOOL_DIR_NAME`]。書き込み受付の状態ファイル
//! （`crate::write_control::state_file_path`）と同じ「DB ごとのファイルは
//! データディレクトリに置く」決まり（profile ごとに DB とデータディレクトリが
//! 1 組）。tstore の剪定・一覧（`banto_tstore::files`）はファイル名が
//! `YYYYMMDD-NNN.sqlite3` のファイルしか見ないので、サブディレクトリは触られない。
//! 中身は監査の内容そのもの（`detail` を含む）なので、データディレクトリと同じ
//! 扱いで守る（profile の ACL は継承で掛かる、`crate::profile_acl`）。
//!
//! ## 作れなかったとき
//!
//! 保留ディレクトリを作れない・読めない（[`AuditLogService::with_spool`] の
//! `Err`）ときは、起動を止めずに**保留なし**（従来の `record`。失敗は標準
//! エラー出力にだけ残り、DB が応答しなければ `record` も待つ）で続け、
//! `[WARN]` をログに出す。緊急停止や収集を、監査の退避先が無いことを理由に
//! 使えなくするほうが危ないと判断した（データディレクトリに書けない構成では
//! 状態ファイル・tstore も同じく書けず、それぞれのログが出る）。
//!
//! 昇格プロセス（`crate::service_elevated` の `banto-hub-elev.exe`）は自前の
//! `AuditLogService` を作るが、`try_record`（監査に失敗したら操作も失敗させる）
//! しか使わず、`try_record` は保留を通らないので、保留を付けない。

use std::path::{Path, PathBuf};

use crate::audit::{AuditLogService, SpoolConfig};
use crate::db::Db;
use crate::hub_log::log_err_line;

/// データディレクトリの中の、監査の保留ディレクトリの名前。
pub const SPOOL_DIR_NAME: &str = "audit-spool";

/// `data_dir`（実効値）の中の監査の保留ディレクトリ。
pub fn spool_dir(data_dir: &Path) -> PathBuf {
    data_dir.join(SPOOL_DIR_NAME)
}

/// banto-hub の監査サービスを組む: [`spool_dir`] に保留を付ける
/// （[`SpoolConfig::default`]: 3 秒・10,000 件・30 秒）。保留を開けなければ
/// `[WARN]` をログに出して保留なしで返す（モジュール doc「作れなかったとき」）。
///
/// 起動時の流し込み（`flush_spool`）と定期実行（`spawn_spool_flusher`）は
/// 呼び出し側（`crate::runtime::HubRuntime::start`）が行う。
pub fn build_audit_service(db: Db, data_dir: &Path) -> AuditLogService {
    let dir = spool_dir(data_dir);
    match AuditLogService::new(db.clone()).with_spool(&dir, SpoolConfig::default()) {
        Ok(audit) => audit,
        Err(err) => {
            log_err_line(&format!(
                "banto-hub: [WARN] 監査の保留ディレクトリ（{}）を使えません。DB に書けない\
                 監査は保留されず失われます: {err}",
                dir.display()
            ));
            AuditLogService::new(db)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_spool_lives_in_the_data_dir_and_is_created() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init_db_memory().await.unwrap();
        let audit = build_audit_service(Db::Sqlite(pool), dir.path());
        assert!(dir.path().join(SPOOL_DIR_NAME).is_dir());
        assert_eq!(audit.spool_backlog().count, 0);
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
