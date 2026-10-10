//! 同じ設定 DB を 2 つのプロセスで開かないための単一インスタンス排他
//! （#392 A1、2026-10-10）。
//!
//! デスクトップアプリ（`src-tauri`）と `banto-serve` は**どちらも**起動時に
//! 収集を自動開始するので、同じ DB を指して両方を動かすと 2 つの収集エンジン
//! が同じ時系列ファイルへ書き込む（[`crate::collect`] の旧「未防止の制約」）。
//! 排他の仕組み自体は共有 crate [`banto_instance_lock`]（banto-hub の
//! `profile_lock` から移したもの）で、このモジュールは ChronoGazer 固有の
//! **排他の単位**と名前だけを決める。
//!
//! # 排他の単位 = 正規化した DB ファイルのパス
//!
//! banto-hub は `profile-id`（名前）で単位を決めるが、ChronoGazer には profile
//! の概念が無く、プロセスが開くのは**パスで指された 1 つの SQLite ファイル**
//! （デスクトップ: アプリのデータディレクトリの `chronogazer.sqlite3`、
//! `banto-serve`: `BANTO_DB`）。そこで DB のパスから単位を作る:
//!
//! - 同じ DB を指す 2 プロセス（相対・絶対・`..` 入りの別表記を含む）は衝突する。
//! - 別の DB（E2E が作る一時ディレクトリ同士など）は衝突しない。
//! - 実装は [`banto_instance_lock::path_scope_id`]（親ディレクトリを
//!   `canonicalize` → ファイル名を足す → Windows は小文字化 → SHA-256 の先頭
//!   8 バイト）。mutex 名は `Global\ChronoGazer.<16桁 hex>`、診断ファイルは
//!   DB の隣の `<DB のファイル名>.instance.lock`（SQLite の `-wal`/`-shm` や
//!   バックアップの置き場とは名前が重ならない）。
//!
//! 守る範囲は**DB 単位**であり、`data.dir` 単位ではない。別の DB は同時に動かせるが、
//! **解決した `data.dir` が同じ**（相対でも絶対でも。例: 同じフォルダに置いた 2 つの
//! DB で `data.dir` が既定のまま、どちらも `<フォルダ>/data` になる）と、この排他では
//! 二重書き込みを防げない。DB ごとに専用のフォルダか `data.dir` を割り当てること
//! （収集開始時に `data.dir` へもロックを取るのは今後の課題）。
//!
//! 既にあるファイルは全体を正規化するので、シンボリックリンク経由の別名は同じ
//! DB として扱う。ハードリンクの別名は検出しない（非対応）。
//!
//! # 呼ぶ位置とガードの寿命
//!
//! [`acquire_for_db`] は**DB に触れる前**（リストア予約の適用より前）に呼ぶ。
//! 返ったガードをプロセスの終わりまで持ち続け、終了で OS が返す。取得できな
//! ければ DB には一切触れずに終了する。

use std::path::{Path, PathBuf};

use banto_instance_lock::{InstanceLockError, InstanceLockGuard, OwnerInfo};

/// 診断ファイルの `host_kind`（誰が持っているかの表示用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChronoGazerHost {
    /// デスクトップアプリ（`apps/chronogazer/src-tauri`）。
    Desktop,
    /// `banto-serve`（開発・E2E 用の単体サーバー）。
    Serve,
}

impl ChronoGazerHost {
    fn as_str(self) -> &'static str {
        match self {
            ChronoGazerHost::Desktop => "desktop",
            ChronoGazerHost::Serve => "serve",
        }
    }

    /// 診断ファイルの `host_kind` から人が読む名前へ。
    fn describe(host_kind: &str) -> &'static str {
        match host_kind {
            "desktop" => "ChronoGazer デスクトップアプリ",
            "serve" => "banto-serve",
            _ => "別のプロセス",
        }
    }
}

/// [`acquire_for_db`] の失敗。
#[derive(Debug)]
pub enum InstanceLockFailure {
    /// 同じ DB を別のプロセスが使用中。
    AlreadyRunning {
        /// 正規化した DB のパス。
        db: PathBuf,
        owner: Option<OwnerInfo>,
    },
    /// ロックの取得そのものに失敗した（ディレクトリが作れない等）。
    Io { db: PathBuf, source: std::io::Error },
}

impl std::fmt::Display for InstanceLockFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InstanceLockFailure::AlreadyRunning { db, owner } => {
                f.write_str(&already_running_message(db, owner))
            }
            InstanceLockFailure::Io { db, source } => write!(
                f,
                "起動の排他を取得できませんでした（{}）: {source}",
                display_path(db)
            ),
        }
    }
}

impl std::error::Error for InstanceLockFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            InstanceLockFailure::Io { source, .. } => Some(source),
            InstanceLockFailure::AlreadyRunning { .. } => None,
        }
    }
}

/// 表示用のパス。Windows の `canonicalize` が付ける `\\?\` を外す（人が読む文）。
pub fn display_path(path: &Path) -> String {
    let text = path.display().to_string();
    text.strip_prefix(r"\\?\")
        .map(str::to_string)
        .unwrap_or(text)
}

fn already_running_message(db: &Path, owner: &Option<OwnerInfo>) -> String {
    let who = match owner {
        Some(owner) => format!(
            "{}（PID {}）",
            ChronoGazerHost::describe(&owner.host_kind),
            owner.pid
        ),
        None => "別のプロセス".to_string(),
    };
    format!(
        "このデータベースは既に {who} が使用中です: {}\n\
         同じデータベースを 2 つのプロセスで開くと、時系列データが二重に書き込まれて\
         壊れるため、起動を中止しました。先に起動している ChronoGazer（または banto-serve）を\
         終了してから、もう一度起動してください。",
        display_path(db)
    )
}

/// 排他を保持している間だけ生存するガード。プロセスの終わりまで持ち続ける。
pub struct InstanceGuard {
    _guard: InstanceLockGuard,
}

/// `db_path` の DB を開く前に、その DB の排他を取る。
pub fn acquire_for_db(
    db_path: &Path,
    host: ChronoGazerHost,
) -> Result<InstanceGuard, InstanceLockFailure> {
    let (id, canonical) =
        banto_instance_lock::path_scope_id(db_path).map_err(|source| InstanceLockFailure::Io {
            db: db_path.to_path_buf(),
            source,
        })?;
    let mutex_name = format!("Global\\ChronoGazer.{id}");
    let mut lock_name = canonical
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_default();
    lock_name.push(".instance.lock");
    let lock_file = canonical.with_file_name(lock_name);

    match banto_instance_lock::try_acquire(&mutex_name, &lock_file, host.as_str()) {
        Ok(guard) => Ok(InstanceGuard { _guard: guard }),
        Err(InstanceLockError::AlreadyHeld { owner }) => Err(InstanceLockFailure::AlreadyRunning {
            db: canonical,
            owner,
        }),
        Err(InstanceLockError::Io(source)) => Err(InstanceLockFailure::Io {
            db: canonical,
            source,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_open_of_the_same_db_is_refused_with_the_owner() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("chronogazer.sqlite3");

        let _first = acquire_for_db(&db, ChronoGazerHost::Desktop).expect("first ok");

        match acquire_for_db(&db, ChronoGazerHost::Serve) {
            Err(InstanceLockFailure::AlreadyRunning { owner, .. }) => {
                let owner = owner.expect("owner diagnostics readable");
                assert_eq!(owner.host_kind, "desktop");
                assert_eq!(owner.pid, std::process::id());
            }
            Err(other) => panic!("expected AlreadyRunning, got {other}"),
            Ok(_) => panic!("same DB must not be opened twice"),
        }
    }

    #[test]
    fn a_different_spelling_of_the_same_db_is_still_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();
        let db = dir.path().join("chronogazer.sqlite3");
        let respelled = dir
            .path()
            .join("sub")
            .join("..")
            .join("chronogazer.sqlite3");

        let _first = acquire_for_db(&db, ChronoGazerHost::Serve).expect("first ok");
        assert!(matches!(
            acquire_for_db(&respelled, ChronoGazerHost::Serve),
            Err(InstanceLockFailure::AlreadyRunning { .. })
        ));
    }

    #[test]
    fn a_symlink_alias_of_the_db_is_refused_and_creation_does_not_change_the_identity() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("chronogazer.sqlite3");
        // 初回起動: ファイルがまだ無い状態で取る。
        let first = acquire_for_db(&db, ChronoGazerHost::Desktop).expect("first ok");
        // 後から DB が作られても、同じ DB の 2 つ目は止まる（ID が食い違わない）。
        std::fs::write(&db, b"x").unwrap();
        assert!(matches!(
            acquire_for_db(&db, ChronoGazerHost::Serve),
            Err(InstanceLockFailure::AlreadyRunning { .. })
        ));
        // シンボリックリンク経由の別名も止まる。
        let alias = dir.path().join("alias.sqlite3");
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_file(&db, &alias);
        #[cfg(not(windows))]
        let made = std::os::unix::fs::symlink(&db, &alias);
        match made {
            Ok(()) => assert!(matches!(
                acquire_for_db(&alias, ChronoGazerHost::Serve),
                Err(InstanceLockFailure::AlreadyRunning { .. })
            )),
            Err(err) => eprintln!("skip: シンボリックリンクを作れません（権限?）: {err}"),
        }
        drop(first);
    }

    #[test]
    fn different_dbs_can_run_side_by_side() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let _a =
            acquire_for_db(&a.path().join("db.sqlite3"), ChronoGazerHost::Serve).expect("db a ok");
        let _b = acquire_for_db(&b.path().join("db.sqlite3"), ChronoGazerHost::Serve)
            .expect("db b ok (same file name, different directory)");
        // 同じ親ディレクトリで別ファイル名も共存できる。
        let _c = acquire_for_db(&a.path().join("other.sqlite3"), ChronoGazerHost::Serve)
            .expect("db c ok");
    }

    #[test]
    fn dropping_the_guard_releases_the_db() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("chronogazer.sqlite3");
        drop(acquire_for_db(&db, ChronoGazerHost::Desktop).expect("first ok"));
        let _again = acquire_for_db(&db, ChronoGazerHost::Serve).expect("reacquire after drop");
    }

    #[test]
    fn the_lock_does_not_touch_the_database_file() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("chronogazer.sqlite3");
        let _guard = acquire_for_db(&db, ChronoGazerHost::Serve).expect("ok");
        assert!(!db.exists(), "acquiring must not create the DB file");
        assert!(dir
            .path()
            .join("chronogazer.sqlite3.instance.lock")
            .is_file());
    }

    #[test]
    fn display_path_drops_the_windows_verbatim_prefix() {
        assert_eq!(
            display_path(Path::new(r"\\?\C:\data\a.sqlite3")),
            r"C:\data\a.sqlite3"
        );
        assert_eq!(
            display_path(Path::new("/data/a.sqlite3")),
            "/data/a.sqlite3"
        );
    }

    #[test]
    fn the_conflict_message_is_actionable_japanese() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("chronogazer.sqlite3");
        let _first = acquire_for_db(&db, ChronoGazerHost::Desktop).expect("first ok");
        let err = acquire_for_db(&db, ChronoGazerHost::Serve).err().unwrap();
        let text = err.to_string();
        assert!(text.contains("既に"), "{text}");
        assert!(text.contains("ChronoGazer デスクトップアプリ"), "{text}");
        assert!(text.contains("終了"), "{text}");
    }
}
