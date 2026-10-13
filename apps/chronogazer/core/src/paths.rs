//! データの置き場所（ルート）の解決と、その下の配置（#392 段階 B の B1-4、
//! `docs/chronogazer-service-design.md` §3.1(b)・決定 7）。
//!
//! 使うのは B1-5 の `chronogazer-server`（`crate::server_host`）。デスクトップは
//! まだ今の `%APPDATA%\dev.tyaro.chronogazer\` のままで、B2-4 のシェルが
//! デスクトップ host をこのルートへ寄せる。
//!
//! # ルートの解決（[`resolve_data_root`]）
//!
//! 優先順位は banto-hub の `profile_paths::resolve_hub_root` と同じ作法:
//!
//! 1. 環境変数 `CHRONOGAZER_ROOT`（空文字列は未設定と同じ）。全 OS 共通。
//! 2. Windows: `%ProgramData%\ChronoGazer`。`%ProgramData%` が取れない（未設定・
//!    空）環境では `C:\ProgramData\ChronoGazer`。
//! 3. 非 Windows（開発・CI 用）: `$XDG_DATA_HOME/ChronoGazer` →
//!    `$HOME/.local/share/ChronoGazer` → `/var/lib/ChronoGazer`。
//!
//! 環境の読み取りは [`resolve_data_root_from_env`] だけが行い、判断は純関数
//! [`resolve_data_root_impl`]（`is_windows` を引数で受ける）にあるので、全分岐を
//! どの OS でもテストできる。ここでは `CHRONOGAZER_ROOT` が相対パスでもそのまま返す
//! （banto-hub の `BANTO_HUB_ROOT` と同じ）が、`chronogazer-server` は相対パスを
//! 拒否して起動しない（#392 B1-5、`crate::server_host::validate_root`。相対の基準は
//! プロセスの作業ディレクトリで、サービスでは `System32` になるため）。
//!
//! # 配置（[`DataLayout`]）
//!
//! ```text
//! <root>\
//!   chronogazer.sqlite3, chronogazer.sqlite3.instance.lock   （A1 の排他の診断）
//!   data\                                                    （時系列データ）
//!   backups\chronogazer.sqlite3\                             （DB の隣、banto の規約）
//!   audit-spool\chronogazer.sqlite3\                         （DB の隣、`audit_spool`）
//!   logs\
//!   secrets\hub-keys\                                        （`banto-secret-store`）
//!   run\endpoint.json                                        （[`crate::endpoint`]）
//! ```
//!
//! バックアップと監査の保留は「DB のフォルダ / 種別 / DB のファイル名」という既存の
//! 規約（`BackupService`・[`crate::audit_spool::spool_dir`]）のままで、DB を
//! `<root>\chronogazer.sqlite3` に置けば自然にこの配置になる。ここの関数群は
//! それを**同じ規約で組み立てるだけ**で、ディレクトリは作らない（作る側は
//! 各サービス・B3 のインストーラ）。
//!
//! # 旧データ（[`legacy_desktop_db_path`]）
//!
//! 既存のデスクトップのデータ（`%APPDATA%\dev.tyaro.chronogazer\chronogazer.sqlite3`）
//! は**読まない・移さない・消さない**（H9）。場所を求める関数は、利用者として動く
//! シェル（B2-4）とインストーラが「旧データは使われません」と案内するため
//! （決定 3）。サービス（LocalSystem）は利用者の `%APPDATA%` を指さないので使わない。

use std::path::{Path, PathBuf};

/// ルートを上書きする環境変数。
pub const ROOT_ENV: &str = "CHRONOGAZER_ROOT";
/// ルートのフォルダ名（`%ProgramData%\ChronoGazer`）。
pub const PRODUCT_DIR_NAME: &str = "ChronoGazer";
/// DB のファイル名。
pub const DB_FILE_NAME: &str = "chronogazer.sqlite3";
/// 旧デスクトップのデータフォルダ名（Tauri の identifier）。
pub const LEGACY_DESKTOP_DIR_NAME: &str = "dev.tyaro.chronogazer";

/// `%ProgramData%` が取れない Windows で使う既定。
const FALLBACK_PROGRAM_DATA: &str = r"C:\ProgramData";

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.filter(|v| !v.is_empty())
}

/// ルートの解決（純関数。`is_windows` を引数にして全分岐をどの OS でもテストする）。
pub fn resolve_data_root_impl(
    is_windows: bool,
    env_root: Option<&str>,
    program_data: Option<&str>,
    xdg: Option<&str>,
    home: Option<&str>,
) -> PathBuf {
    if let Some(root) = non_empty(env_root) {
        return PathBuf::from(root);
    }
    if is_windows {
        let base = non_empty(program_data).unwrap_or(FALLBACK_PROGRAM_DATA);
        return PathBuf::from(base).join(PRODUCT_DIR_NAME);
    }
    if let Some(xdg) = non_empty(xdg) {
        return PathBuf::from(xdg).join(PRODUCT_DIR_NAME);
    }
    if let Some(home) = non_empty(home) {
        return PathBuf::from(home)
            .join(".local")
            .join("share")
            .join(PRODUCT_DIR_NAME);
    }
    PathBuf::from("/var/lib").join(PRODUCT_DIR_NAME)
}

/// [`resolve_data_root_impl`] を実行中の OS で呼ぶ（環境は引数で受ける）。
pub fn resolve_data_root(
    env_root: Option<&str>,
    program_data: Option<&str>,
    xdg: Option<&str>,
    home: Option<&str>,
) -> PathBuf {
    resolve_data_root_impl(cfg!(windows), env_root, program_data, xdg, home)
}

/// 環境変数から読んでルートを解決する（環境を読むのはここだけ）。
pub fn resolve_data_root_from_env() -> PathBuf {
    let var = |name: &str| std::env::var(name).ok();
    resolve_data_root(
        var(ROOT_ENV).as_deref(),
        var("ProgramData").as_deref(),
        var("XDG_DATA_HOME").as_deref(),
        var("HOME").as_deref(),
    )
}

/// データのルートの下の配置（モジュール doc の図）。パスを組み立てるだけで、
/// ファイルシステムには触れない。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataLayout {
    root: PathBuf,
}

impl DataLayout {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// 環境から解決したルートの配置。
    pub fn from_env() -> Self {
        Self::new(resolve_data_root_from_env())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 設定 DB。
    pub fn db_path(&self) -> PathBuf {
        self.root.join(DB_FILE_NAME)
    }

    /// A1 の排他の診断ファイル（`<DB のファイル名>.instance.lock`、DB の隣。
    /// [`crate::instance_lock`] の命名と同じ）。
    pub fn instance_lock_path(&self) -> PathBuf {
        self.root.join(format!("{DB_FILE_NAME}.instance.lock"))
    }

    /// 時系列データ（`data.dir` の既定の基準。`<root>\data`）。
    pub fn data_dir(&self) -> PathBuf {
        self.root.join("data")
    }

    /// バックアップ（`<DB の親>\backups\<DB のファイル名>`。banto の規約）。
    pub fn backups_dir(&self) -> PathBuf {
        self.root.join("backups").join(DB_FILE_NAME)
    }

    /// 監査の保留（[`crate::audit_spool::spool_dir`] と同じ）。
    pub fn audit_spool_dir(&self) -> PathBuf {
        crate::audit_spool::spool_dir(&self.db_path())
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.root.join("logs")
    }

    /// 秘密の置き場（ACL を継承しない。B3-3 が付ける）。
    pub fn secrets_dir(&self) -> PathBuf {
        self.root.join("secrets")
    }

    /// Hub の API キー（`banto-secret-store` の `FileSecretStore` のディレクトリ）。
    pub fn hub_keys_dir(&self) -> PathBuf {
        self.secrets_dir().join("hub-keys")
    }

    /// 実行時の情報（`endpoint.json`）の置き場。
    pub fn run_dir(&self) -> PathBuf {
        self.root.join("run")
    }

    /// 待ち受け中のエンジンの所在（[`crate::endpoint`]）。
    pub fn endpoint_file(&self) -> PathBuf {
        self.run_dir().join(crate::endpoint::ENDPOINT_FILE_NAME)
    }
}

/// 旧デスクトップの DB の場所（`<appdata>\dev.tyaro.chronogazer\chronogazer.sqlite3`）。
/// `appdata` が無い・空なら `None`。**場所を返すだけ**で、読まない・移さない・消さない。
pub fn legacy_desktop_db_path_in(appdata: Option<&str>) -> Option<PathBuf> {
    non_empty(appdata).map(|appdata| {
        PathBuf::from(appdata)
            .join(LEGACY_DESKTOP_DIR_NAME)
            .join(DB_FILE_NAME)
    })
}

/// 実行中の利用者の `%APPDATA%` からの旧 DB の場所（Windows 以外では
/// `APPDATA` が普通無いので `None`）。サービス（LocalSystem）で呼んでも意味が
/// ない - 利用者として動くシェルとインストーラが使う。
pub fn legacy_desktop_db_path() -> Option<PathBuf> {
    legacy_desktop_db_path_in(std::env::var("APPDATA").ok().as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_root_wins_on_every_os() {
        for is_windows in [true, false] {
            assert_eq!(
                resolve_data_root_impl(
                    is_windows,
                    Some("/custom"),
                    Some(r"D:\PD"),
                    Some("/xdg"),
                    Some("/home/u")
                ),
                PathBuf::from("/custom")
            );
        }
    }

    #[test]
    fn empty_env_root_is_ignored() {
        assert_eq!(
            resolve_data_root_impl(true, Some(""), Some(r"D:\PD"), None, None),
            PathBuf::from(r"D:\PD").join("ChronoGazer")
        );
        assert_eq!(
            resolve_data_root_impl(false, Some(""), None, Some("/xdg"), None),
            PathBuf::from("/xdg").join("ChronoGazer")
        );
    }

    #[test]
    fn windows_uses_program_data_and_falls_back_when_unavailable() {
        assert_eq!(
            resolve_data_root_impl(true, None, Some(r"D:\PD"), Some("/xdg"), Some("/h")),
            PathBuf::from(r"D:\PD").join("ChronoGazer")
        );
        // ProgramData が取れない（未設定・空）環境。xdg/home は Windows では見ない。
        for missing in [None, Some("")] {
            assert_eq!(
                resolve_data_root_impl(true, None, missing, Some("/xdg"), Some("/h")),
                PathBuf::from(r"C:\ProgramData").join("ChronoGazer")
            );
        }
    }

    #[test]
    fn non_windows_prefers_xdg_then_home_then_var_lib() {
        assert_eq!(
            resolve_data_root_impl(false, None, Some(r"D:\PD"), Some("/xdg"), Some("/h")),
            PathBuf::from("/xdg").join("ChronoGazer")
        );
        assert_eq!(
            resolve_data_root_impl(false, None, None, Some(""), Some("/h")),
            PathBuf::from("/h")
                .join(".local")
                .join("share")
                .join("ChronoGazer")
        );
        assert_eq!(
            resolve_data_root_impl(false, None, None, None, Some("")),
            PathBuf::from("/var/lib").join("ChronoGazer")
        );
        assert_eq!(
            resolve_data_root_impl(false, None, None, None, None),
            PathBuf::from("/var/lib").join("ChronoGazer")
        );
    }

    #[test]
    fn layout_places_everything_under_the_root_following_existing_conventions() {
        let root = PathBuf::from("/r");
        let layout = DataLayout::new(&root);
        assert_eq!(layout.root(), root);
        assert_eq!(layout.db_path(), root.join("chronogazer.sqlite3"));
        assert_eq!(
            layout.instance_lock_path(),
            root.join("chronogazer.sqlite3.instance.lock")
        );
        assert_eq!(layout.data_dir(), root.join("data"));
        assert_eq!(
            layout.backups_dir(),
            root.join("backups").join("chronogazer.sqlite3")
        );
        assert_eq!(
            layout.audit_spool_dir(),
            root.join("audit-spool").join("chronogazer.sqlite3")
        );
        assert_eq!(layout.logs_dir(), root.join("logs"));
        assert_eq!(layout.hub_keys_dir(), root.join("secrets").join("hub-keys"));
        assert_eq!(
            layout.endpoint_file(),
            root.join("run").join("endpoint.json")
        );
        // 全部がルートの下（ルートの外へ出るものが無い）。
        for p in [
            layout.db_path(),
            layout.instance_lock_path(),
            layout.data_dir(),
            layout.backups_dir(),
            layout.audit_spool_dir(),
            layout.logs_dir(),
            layout.secrets_dir(),
            layout.hub_keys_dir(),
            layout.run_dir(),
            layout.endpoint_file(),
        ] {
            assert!(p.starts_with(&root), "{p:?}");
        }
    }

    /// 組み立てた診断ファイルの名前が A1 の実際の命名と食い違わないこと
    /// （食い違うと、DB を移したときに片方が別の場所を指す）。
    #[test]
    fn instance_lock_path_matches_what_the_real_lock_creates() {
        let dir = tempfile::tempdir().unwrap();
        let layout = DataLayout::new(dir.path());
        let guard = crate::instance_lock::acquire_for_db(
            &layout.db_path(),
            crate::instance_lock::ChronoGazerHost::Server,
        )
        .expect("acquire");
        let expected = layout.instance_lock_path();
        // 実際のファイルは canonicalize した親の下に作られる。同じ名前か比べる。
        assert_eq!(
            expected.file_name(),
            std::fs::read_dir(dir.path())
                .unwrap()
                .filter_map(|e| e.ok())
                .map(|e| e.file_name())
                .find(|n| n.to_string_lossy().ends_with(".instance.lock"))
                .as_deref()
        );
        drop(guard);
    }

    #[test]
    fn legacy_desktop_db_path_points_into_appdata_and_none_without_it() {
        assert_eq!(
            legacy_desktop_db_path_in(Some(r"C:\Users\u\AppData\Roaming")),
            Some(
                PathBuf::from(r"C:\Users\u\AppData\Roaming")
                    .join("dev.tyaro.chronogazer")
                    .join("chronogazer.sqlite3")
            )
        );
        assert_eq!(legacy_desktop_db_path_in(None), None);
        assert_eq!(legacy_desktop_db_path_in(Some("")), None);
    }

    /// 旧データの場所は新しいルートの外（H9: 読まない・移さない）。
    #[test]
    fn legacy_location_is_never_under_the_new_root() {
        let layout = DataLayout::new(PathBuf::from("/pd").join("ChronoGazer"));
        let legacy = legacy_desktop_db_path_in(Some("/roaming")).unwrap();
        assert!(!legacy.starts_with(layout.root()));
    }
}
