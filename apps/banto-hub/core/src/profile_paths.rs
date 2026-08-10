//! T17-1（docs/banto-hub-t17-design.md §3「T17-1」・P1、
//! docs/banto-hub-desktop-plan.md §11「データプロファイルと移行」）:
//! 3ホスト（console `bin/banto-hub.rs`／Windows service
//! `bin/banto_hub/win_service.rs`／shell `apps/banto-hub/src-tauri`）が
//! 個別に複製していた `DEFAULT_DB_PATH = "./banto-hub.sqlite3"`／
//! `store_config.data_dir` 既定 `"./data"`／`hub_log::resolve_service_log_dir`
//! 既定 `"./data"` という3つの相対パス既定値を、モード非依存の絶対パス
//! 解決関数へ一本化するモジュール。
//!
//! ## layout（desktop-plan §11）
//!
//! ```text
//! {root}/profiles/<profile-id>/
//!   config/banto-hub.sqlite3
//!   data/
//!   logs/
//! ```
//!
//! `root` の既定は Windows が `%ProgramData%\BantoHub`、非 Windows は
//! `BANTO_HUB_ROOT` → `XDG_DATA_HOME/BantoHub` → `$HOME/.local/share/BantoHub`
//! → `/var/lib/BantoHub` の順（[`resolve_hub_root`]）。`<profile-id>` の
//! 既定は[`DEFAULT_PROFILE_ID`]（`"default"`）で、env `BANTO_HUB_PROFILE`が
//! 上書きする。
//!
//! ## この T17-1 で行ったこと・行っていないこと
//!
//! - [`build_hub_config_from_env`]を3ホスト共通の`HubConfig`組み立て関数
//!   として新設した - 各ホストの`build_hub_config`はこれを呼ぶだけの薄い
//!   ラッパになった（`bin/banto-hub.rs`・`win_service.rs`・
//!   `apps/banto-hub/src-tauri/src/lib.rs`）。
//! - `DEFAULT_DB_PATH`（`crate::runtime`）自体は後方互換・ドキュメント用に
//!   残したが、**T17-1 以降はこのモジュール（[`resolve_profile_paths`]）が
//!   db/data の既定パスの正**であり、`build_hub_config_from_env`はこの
//!   定数を使わない。
//! - 旧 `./banto-hub.sqlite3`／`./data`からの自動移行は行わない
//!   （desktop-plan §11「黙って移動しない」、本スライスのスコープ外）。

use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::profile_lock::HubHostKind;
use crate::runtime::HubConfig;

/// profile-id の既定値。env `BANTO_HUB_PROFILE`が空／未設定のとき使う。
pub const DEFAULT_PROFILE_ID: &str = "default";

/// `<profile-id>`の検証エラー（空文字列、パス区切り文字、`..`等）。
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ProfileIdError {
    #[error("profile id を空にすることはできません")]
    Empty,
    #[error("profile id にパス区切り文字は使えません: {0:?}")]
    PathSeparator(String),
    #[error("profile id にディレクトリ移動（'.'/'..'）は使えません: {0:?}")]
    DotSegment(String),
}

/// `profile_id`が絶対パスの1コンポーネントとしてそのまま安全に使えるかを
/// 検証する - 空文字列、`/`・`\`（Windows パス区切り、非 Windows でも
/// 混乱を避けるため一律禁止）、`.`/`..`（ディレクトリ移動）を拒否する。
pub fn validate_profile_id(profile_id: &str) -> Result<(), ProfileIdError> {
    if profile_id.is_empty() {
        return Err(ProfileIdError::Empty);
    }
    if profile_id.contains('/') || profile_id.contains('\\') {
        return Err(ProfileIdError::PathSeparator(profile_id.to_string()));
    }
    if profile_id == "." || profile_id == ".." {
        return Err(ProfileIdError::DotSegment(profile_id.to_string()));
    }
    Ok(())
}

/// 1 profile の絶対パス一式（desktop-plan §11 の layout そのもの）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfilePaths {
    /// `%ProgramData%\BantoHub`相当の root（[`resolve_hub_root`]の戻り値）。
    pub root: PathBuf,
    pub profile_id: String,
    /// `{root}/profiles/<profile-id>/`。
    pub profile_dir: PathBuf,
    /// `{profile_dir}/config/banto-hub.sqlite3`。
    pub db_path: PathBuf,
    /// `{profile_dir}/data/`。
    pub data_dir: PathBuf,
    /// `{profile_dir}/logs/`。
    pub logs_dir: PathBuf,
}

/// `root`と検証済み`profile_id`から[`ProfilePaths`]を組み立てる純関数。
/// ディレクトリの作成は行わない（作成は
/// `crate::profile_lock::try_acquire_profile_lock`側の責務）。
pub fn resolve_profile_paths(
    root: &Path,
    profile_id: &str,
) -> Result<ProfilePaths, ProfileIdError> {
    validate_profile_id(profile_id)?;
    let profile_dir = root.join("profiles").join(profile_id);
    Ok(ProfilePaths {
        root: root.to_path_buf(),
        profile_id: profile_id.to_string(),
        db_path: profile_dir.join("config").join("banto-hub.sqlite3"),
        data_dir: profile_dir.join("data"),
        logs_dir: profile_dir.join("logs"),
        profile_dir,
    })
}

/// `Global\BantoHub.<profile-id>`（desktop-plan §16.2 で命名決定済み）。
/// Windows 実 mutex 名として使う（[`crate::profile_lock`]）。非 Windows でも
/// 文字列組み立てとして純粋にテストできる。
pub fn mutex_name(profile_id: &str) -> String {
    format!("Global\\BantoHub.{profile_id}")
}

/// [`resolve_hub_root`]の内側 - 実際の`cfg!(windows)`判定を`is_windows`
/// パラメータとして外に出したことで、CI（非 Windows ランナー）でも
/// Windows 側のパス組み立てロジックを含めた両分岐を検証できる
/// （T17 設計 §3「パス解決の純関数部分は CI 可」）。
fn resolve_hub_root_impl(
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
        let base = non_empty(program_data).unwrap_or(r"C:\ProgramData");
        PathBuf::from(base).join("BantoHub")
    } else {
        if let Some(xdg) = non_empty(xdg) {
            return PathBuf::from(xdg).join("BantoHub");
        }
        if let Some(home) = non_empty(home) {
            return PathBuf::from(home)
                .join(".local")
                .join("share")
                .join("BantoHub");
        }
        PathBuf::from("/var/lib/BantoHub")
    }
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.filter(|v| !v.is_empty())
}

/// hub の root ディレクトリを解決する純関数（テスト用、T17 設計 §3）。
/// 実際の env 読み取りは[`build_hub_config_from_env`]側が行い、この関数
/// 自体は一切 env に触れない。
///
/// 優先順位: `env_root`（`BANTO_HUB_ROOT`、空文字列は無視）が最優先で
/// 全 OS 共通。以降は実行環境が Windows かどうかで分岐する
/// （[`resolve_hub_root_impl`]参照）:
/// - Windows: `program_data`（`%ProgramData%`、既定`C:\ProgramData`）配下の
///   `BantoHub`。
/// - 非 Windows: `xdg`（`XDG_DATA_HOME`）→ `home`（`$HOME`）→
///   `/var/lib/BantoHub`の順。
pub fn resolve_hub_root(
    env_root: Option<&str>,
    program_data: Option<&str>,
    xdg: Option<&str>,
    home: Option<&str>,
) -> PathBuf {
    resolve_hub_root_impl(cfg!(windows), env_root, program_data, xdg, home)
}

/// env（`BANTO_HUB_ROOT`/`BANTO_HUB_PROFILE`）から[`ProfilePaths`]を解決する。
/// [`build_hub_config_from_env`]の内側で使うのと同じ root/profile_id 解決
/// ロジックを、`db_path`/`data_dir_override`の上書き適用より前の状態
/// （＝profile の正準ディレクトリそのもの）として外部へ公開する - たとえば
/// `win_service.rs`は`HubRuntime::start`より前にログファイルを開くために
/// `logs_dir`だけを先に必要とする。[`build_hub_config_from_env`]自身も
/// これを呼ぶ。
pub fn resolve_profile_paths_from_env() -> ProfilePaths {
    let root = resolve_hub_root(
        std::env::var("BANTO_HUB_ROOT").ok().as_deref(),
        std::env::var("ProgramData").ok().as_deref(),
        std::env::var("XDG_DATA_HOME").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
    );

    let requested_profile_id = std::env::var("BANTO_HUB_PROFILE")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_PROFILE_ID.to_string());
    let profile_id = match validate_profile_id(&requested_profile_id) {
        Ok(()) => requested_profile_id,
        Err(err) => {
            eprintln!(
                "banto-hub: BANTO_HUB_PROFILE={requested_profile_id:?} は不正です（{err}） - 既定 profile '{DEFAULT_PROFILE_ID}' を使います"
            );
            DEFAULT_PROFILE_ID.to_string()
        }
    };

    // `profile_id`はここまでで必ず検証済み（既定値自身も英数字のみなので
    // 常に valid）- `resolve_profile_paths`が失敗するのは
    // `validate_profile_id`が拒否する場合のみなので、ここでの `expect`は
    // パニックしない。
    resolve_profile_paths(&root, &profile_id)
        .expect("BANTO_HUB_PROFILE は事前検証済みのため resolve_profile_paths は失敗しない")
}

/// 3ホスト共通の[`HubConfig`]組み立て関数（このモジュール doc の
/// 「この T17-1 で行ったこと」節参照）。読み取る env は次の通り:
///
/// - `BANTO_HUB_PROFILE`: profile id（既定[`DEFAULT_PROFILE_ID`]）。不正な
///   値（[`validate_profile_id`]が拒否する文字列）は stderr へ警告を出し、
///   既定 profile へフォールバックする（"どの env でも起動を拒否しない"
///   より安全側 - 呼び出し元は`Result`を持たない関数なので）。
/// - `BANTO_HUB_ROOT`: root override（[`resolve_hub_root`]参照）。
/// - `BANTO_DB`: db_path 上書き（絶対でも相対でもそのまま渡す - 従来と
///   同じ「env が最終的な文字列そのもの」という意味論）。
/// - `BANTO_HUB_DATA`: data_dir 上書き。
/// - `BANTO_ALLOW_SETUP` / `PORT` / `BANTO_BIND`: 従来どおり。
///
/// `BANTO_DB`/`BANTO_HUB_DATA`が未設定のときの既定値は、[`resolve_profile_paths`]
/// が返す絶対パス（`{root}/profiles/<profile-id>/config/banto-hub.sqlite3`・
/// `.../data/`）- `crate::runtime::DEFAULT_DB_PATH`（後方互換のため残した
/// 相対パス定数）はここでは使わない。
pub fn build_hub_config_from_env(host_kind: HubHostKind) -> HubConfig {
    let paths = resolve_profile_paths_from_env();

    HubConfig {
        db_path: std::env::var("BANTO_DB")
            .unwrap_or_else(|_| paths.db_path.to_string_lossy().into_owned()),
        allow_setup: std::env::var("BANTO_ALLOW_SETUP")
            .map(|value| value == "1")
            .unwrap_or(false),
        port_override: std::env::var("PORT")
            .ok()
            .and_then(|value| value.parse().ok()),
        bind_override: std::env::var("BANTO_BIND").ok(),
        data_dir_override: Some(
            std::env::var("BANTO_HUB_DATA")
                .ok()
                .map(PathBuf::from)
                .unwrap_or_else(|| paths.data_dir.clone()),
        ),
        profile_id: paths.profile_id,
        host_kind,
        skip_profile_lock: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_profile_id_rejects_empty() {
        assert_eq!(validate_profile_id(""), Err(ProfileIdError::Empty));
    }

    #[test]
    fn validate_profile_id_rejects_path_separators() {
        assert!(matches!(
            validate_profile_id("a/b"),
            Err(ProfileIdError::PathSeparator(_))
        ));
        assert!(matches!(
            validate_profile_id("a\\b"),
            Err(ProfileIdError::PathSeparator(_))
        ));
    }

    #[test]
    fn validate_profile_id_rejects_dot_segments() {
        assert!(matches!(
            validate_profile_id("."),
            Err(ProfileIdError::DotSegment(_))
        ));
        assert!(matches!(
            validate_profile_id(".."),
            Err(ProfileIdError::DotSegment(_))
        ));
    }

    #[test]
    fn validate_profile_id_accepts_normal_ids() {
        assert!(validate_profile_id("default").is_ok());
        assert!(validate_profile_id("line-1_2").is_ok());
    }

    #[test]
    fn resolve_profile_paths_builds_desktop_plan_layout() {
        let root = PathBuf::from("/root");
        let paths = resolve_profile_paths(&root, "default").expect("valid profile id");
        assert_eq!(paths.profile_dir, PathBuf::from("/root/profiles/default"));
        assert_eq!(
            paths.db_path,
            PathBuf::from("/root/profiles/default/config/banto-hub.sqlite3")
        );
        assert_eq!(paths.data_dir, PathBuf::from("/root/profiles/default/data"));
        assert_eq!(paths.logs_dir, PathBuf::from("/root/profiles/default/logs"));
    }

    #[test]
    fn resolve_profile_paths_rejects_invalid_profile_id() {
        let root = PathBuf::from("/root");
        assert!(resolve_profile_paths(&root, "../escape").is_err());
    }

    #[test]
    fn mutex_name_matches_desktop_plan_naming() {
        assert_eq!(mutex_name("default"), "Global\\BantoHub.default");
        assert_eq!(mutex_name("line-1"), "Global\\BantoHub.line-1");
    }

    #[test]
    fn resolve_hub_root_env_override_wins_on_every_platform() {
        for is_windows in [true, false] {
            assert_eq!(
                resolve_hub_root_impl(
                    is_windows,
                    Some("/custom/root"),
                    Some("C:\\ProgramData"),
                    Some("/xdg"),
                    Some("/home/user")
                ),
                PathBuf::from("/custom/root")
            );
        }
    }

    #[test]
    fn resolve_hub_root_env_override_ignores_empty_string() {
        assert_eq!(
            resolve_hub_root_impl(false, Some(""), None, Some("/xdg"), None),
            PathBuf::from("/xdg/BantoHub")
        );
    }

    // Windows 側の期待値は`PathBuf::from(...).join(...)`で組み立てる
    // （バックスラッシュのハードコード文字列と比較しない）- `PathBuf::join`
    // はコンパイル対象 OS のセパレータ（Linux CI ではこのテストバイナリ自体
    // `/`区切り）を使うため、実際に生成される値と組み立て方を揃える必要が
    // ある。パス文字列の組み立てロジック自体（root override → BantoHub
    // への join）は実行 OS に関係なくここで検証できる - 実際のバックスラッシュ
    // 区切りは Windows 実機ビルドでのみ観測できる（このモジュール doc・T17
    // 設計 §3「Windows 実機必須」の対象はここではなく named mutex 側）。
    #[test]
    fn resolve_hub_root_windows_uses_program_data() {
        assert_eq!(
            resolve_hub_root_impl(true, None, Some(r"D:\ProgramData"), None, None),
            PathBuf::from(r"D:\ProgramData").join("BantoHub")
        );
    }

    #[test]
    fn resolve_hub_root_windows_defaults_program_data_when_unset() {
        assert_eq!(
            resolve_hub_root_impl(true, None, None, None, None),
            PathBuf::from(r"C:\ProgramData").join("BantoHub")
        );
    }

    #[test]
    fn resolve_hub_root_unix_prefers_xdg_over_home() {
        assert_eq!(
            resolve_hub_root_impl(false, None, None, Some("/xdg-data"), Some("/home/user")),
            PathBuf::from("/xdg-data/BantoHub")
        );
    }

    #[test]
    fn resolve_hub_root_unix_falls_back_to_home_when_xdg_unset() {
        assert_eq!(
            resolve_hub_root_impl(false, None, None, None, Some("/home/user")),
            PathBuf::from("/home/user/.local/share/BantoHub")
        );
    }

    #[test]
    fn resolve_hub_root_unix_falls_back_to_var_lib_when_nothing_set() {
        assert_eq!(
            resolve_hub_root_impl(false, None, None, None, None),
            PathBuf::from("/var/lib/BantoHub")
        );
    }

    #[test]
    fn resolve_hub_root_public_wrapper_reflects_actual_target_os() {
        // `resolve_hub_root`自身は`cfg!(windows)`を通すだけの薄いラッパ -
        // このワークスペースの CI は非 Windows ランナーなので、ここでは
        // 非 Windows の分岐（`resolve_hub_root_impl(false, ...)`と同じ結果）
        // だけを確認する。Windows 側の分岐は上記
        // `resolve_hub_root_windows_*`が`resolve_hub_root_impl(true, ...)`
        // 経由で既にカバーしている。
        assert_eq!(
            resolve_hub_root(None, None, None, Some("/home/user")),
            resolve_hub_root_impl(cfg!(windows), None, None, None, Some("/home/user"))
        );
    }
}
