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

/// profile の設定 DB のファイル名（`{profile_dir}/config/` の中）。
pub const DB_FILE_NAME: &str = "banto-hub.sqlite3";

/// `<profile-id>`の検証エラー（空文字列、パス区切り文字、`..`、`:`、予約名、
/// 末尾の`.`・空白）。
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum ProfileIdError {
    #[error("profile id を空にすることはできません")]
    Empty,
    #[error("profile id にパス区切り文字は使えません: {0:?}")]
    PathSeparator(String),
    #[error("profile id にディレクトリ移動（'.'/'..'）は使えません: {0:?}")]
    DotSegment(String),
    /// `:` を含む（#626、2026-10-16）。Windows の`Path::join`はドライブの接頭辞を
    /// 持つ要素（`C:`・`x:y`）で**前を捨てる**ので、`root.join("profiles").join("C:")`
    /// は`C:`（C: の今のフォルダ = ドライブ相対）になる。NTFS の代替データストリーム
    /// （`default::$DATA`）も`:`で書く。
    #[error(
        "profile id に ':' は使えません（ドライブ・代替データストリームの指定になります）: {0:?}"
    )]
    Colon(String),
    /// Windows の予約デバイス名（`CON`・`NUL`・`COM1` など。拡張子つきの`nul.txt`も）。
    #[error("profile id に Windows の予約名（CON・NUL・COM1 など）は使えません: {0:?}")]
    ReservedName(String),
    /// 末尾が`.`か空白（Win32 がパスを正規化するときに落とすので、`default.`が
    /// `default`と同じフォルダを指す。`...`は`profiles`そのものを指す）。
    #[error("profile id の末尾に '.' や空白は使えません: {0:?}")]
    TrailingDotOrSpace(String),
}

/// Windows の予約デバイス名か（大文字小文字を無視。最初の`.`より前を、末尾の空白を
/// 除いて見るので`nul.txt`・`CON .log`も予約名。`COM0`〜`COM9`・`LPT0`〜`LPT9`と
/// 上付き数字の`COM¹`なども含む - Win32 はこれらをファイルではなくデバイスとして開く）。
/// 判断は共有の`banto_safe_root::guard::is_reserved_device_name`（#642 で移した。
/// 昇格ヘルパーが親のハンドルを基準に作る名前の確かめと同じもの）。`COM0`・`LPT0`は
/// #641 のオーナーレビュー Nit 4 で足した。
pub use banto_safe_root::guard::is_reserved_device_name;

/// `profile_id`が絶対パスの1コンポーネントとしてそのまま安全に使えるかを
/// 検証する - 空文字列、`/`・`\`（Windows パス区切り、非 Windows でも
/// 混乱を避けるため一律禁止）、`.`/`..`（ディレクトリ移動）を拒否する。
///
/// #626（2026-10-16 オーナー決定 論点 7）で、**Windows で別の場所を指しうるもの**も
/// 断るようにした: `:`（ドライブ相対・代替データストリーム）、予約デバイス名、末尾の
/// `.`・空白。既存の profile-id を使えなくしないよう、ここは許可リストにまでは狭めない
/// （昇格ヘルパー`banto-hub-elev`だけは
/// [`crate::profile_grant::validate_strict_profile_id`]の許可リストを使う）。
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
    if profile_id.contains(':') {
        return Err(ProfileIdError::Colon(profile_id.to_string()));
    }
    if profile_id.ends_with('.') || profile_id.ends_with(' ') {
        return Err(ProfileIdError::TrailingDotOrSpace(profile_id.to_string()));
    }
    if is_reserved_device_name(profile_id) {
        return Err(ProfileIdError::ReservedName(profile_id.to_string()));
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
        db_path: profile_dir.join("config").join(DB_FILE_NAME),
        data_dir: profile_dir.join("data"),
        logs_dir: profile_dir.join("logs"),
        profile_dir,
    })
}

/// #661 661-1: `path`（`BANTO_DB`・`BANTO_HUB_DATA` などで外から渡されたパス）が
/// profile の**外**か（純関数。ファイルシステムに触れない）。`profile_dir` は渡された綴り、
/// `profile_final` は握った profile の本当のパス（`banto_safe_root::SafeRoot::final_path`）で、
/// どちらかの下なら中とみなす（要素単位の前方一致。`profiles\a` と `profiles\ab` は別）。
///
/// 相対パスは外とみなす（どこを基準に開くかは呼ぶ時点のカレントディレクトリ次第で、
/// profile のリンクの保護と運用モードの ACL の範囲に入るとは言えないため）。外のパスは
/// 辿らない作り方・ACL の付け替えの対象外で、サービスはその旨をログに 1 行出す
/// （[`external_path_line`]。呼ぶのは 661-2）。
pub fn is_outside_profile(path: &Path, profile_dir: &Path, profile_final: &Path) -> bool {
    if !path.is_absolute() {
        return true;
    }
    !(path.starts_with(profile_dir) || path.starts_with(profile_final))
}

/// profile の外にあるもの（[`external_path_line`] の文言の切り替え）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalPath {
    /// 設定 DB（`BANTO_DB`）。
    Db,
    /// データのフォルダ（`BANTO_HUB_DATA`、または設定 DB の `data.dir`）。
    DataDir,
}

/// profile の外のパスを使うときにログに出す 1 行（#661 661-1。ChronoGazer の
/// `external_data_dir_line` 相当。呼ぶのは 661-2 のサービス）。
pub fn external_path_line(kind: ExternalPath, path: &Path) -> String {
    let what = match kind {
        ExternalPath::Db => "設定 DB",
        ExternalPath::DataDir => "データのフォルダ",
    };
    format!(
        "banto-hub: {what}（外部: {}）は profile の外にあり、リンクの保護と運用モードの ACL の対象外です（#661）",
        path.display()
    )
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

    /// #626: Windows で別の場所を指しうるものを断る（`:`・予約名・末尾の`.`と空白）。
    #[test]
    fn validate_profile_id_rejects_names_that_point_elsewhere_on_windows() {
        for id in ["C:", "x:y", "D:evil", "default::$DATA"] {
            assert_eq!(
                validate_profile_id(id),
                Err(ProfileIdError::Colon(id.to_string())),
                "{id:?}"
            );
        }
        for id in ["default.", "default ", "...", "a. "] {
            assert_eq!(
                validate_profile_id(id),
                Err(ProfileIdError::TrailingDotOrSpace(id.to_string())),
                "{id:?}"
            );
        }
        for id in [
            "nul",
            "NUL",
            "Con",
            "prn",
            "aux",
            "COM1",
            "lpt9",
            // #641 のオーナーレビュー Nit 4: `COM0`・`LPT0` も予約名として断る。
            "COM0",
            "lpt0",
            "nul.txt",
            "CON .log",
            "com\u{b9}",
            "CONIN$",
        ] {
            assert_eq!(
                validate_profile_id(id),
                Err(ProfileIdError::ReservedName(id.to_string())),
                "{id:?}"
            );
        }
        // 予約名に似ているだけのものは通る。
        for id in [
            "console",
            "nul1",
            "com10",
            "lpt00",
            "auxiliary",
            "p6-123-456",
        ] {
            assert!(validate_profile_id(id).is_ok(), "{id:?}");
        }
    }

    /// #626 で見つけた穴の固定: `C:`・`x:y` を profile-id にすると、`resolve_profile_paths`
    /// は断る（以前は`Path::join`がドライブ相対のパス`C:`を返し、昇格した
    /// `grant-profile-acl`が C: の今のフォルダを再帰で書き換えうる経路だった）。
    #[test]
    fn resolve_profile_paths_rejects_drive_relative_ids() {
        let root = PathBuf::from(r"C:\ProgramData\BantoHub");
        for id in ["C:", "x:y", "D:evil"] {
            assert!(
                matches!(
                    resolve_profile_paths(&root, id),
                    Err(ProfileIdError::Colon(_))
                ),
                "{id:?}"
            );
        }
    }

    /// 上の確かめが要る理由（Windows の`Path::join`の挙動そのもの）を固定する: ドライブの
    /// 接頭辞を持つ要素を join すると前が捨てられ、`profiles`の下ではなくなる。この挙動が
    /// 変わったらこのテストが落ちる（そのときは確かめを外してよいかを見直す）。
    #[cfg(windows)]
    #[test]
    fn windows_join_with_a_drive_prefix_discards_the_base() {
        let profiles = PathBuf::from(r"C:\ProgramData\BantoHub\profiles");
        assert_eq!(profiles.join("C:"), PathBuf::from("C:"));
        assert_eq!(profiles.join("x:y"), PathBuf::from("x:y"));
        assert!(!profiles.join("C:").starts_with(&profiles));
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

    /// #661 661-1: 渡された綴りか本当のパスの下なら中、それ以外・相対パスは外。
    #[test]
    fn is_outside_profile_matches_by_components_of_either_spelling() {
        // どの OS でも絶対パスになる基準（純関数なので存在しなくてよい）。
        let base = std::env::temp_dir();
        let spelled = base.join("hub-root").join("profiles").join("default");
        let real = base.join("real-root").join("profiles").join("default");

        let db = spelled.join("config").join(DB_FILE_NAME);
        assert!(!is_outside_profile(&db, &spelled, &real));
        assert!(!is_outside_profile(&real.join("data"), &spelled, &real));
        assert!(!is_outside_profile(&spelled, &spelled, &real));

        let sibling = base.join("hub-root").join("profiles").join("default2");
        assert!(is_outside_profile(&sibling.join("data"), &spelled, &real));
        assert!(is_outside_profile(&base.join("elsewhere"), &spelled, &real));
        assert!(is_outside_profile(Path::new("data"), &spelled, &real));
        assert!(is_outside_profile(
            Path::new("./banto-hub.sqlite3"),
            &spelled,
            &real
        ));
    }

    #[test]
    fn external_path_line_names_what_and_where() {
        let path = std::env::temp_dir().join("external-data");
        let data = external_path_line(ExternalPath::DataDir, &path);
        assert!(
            data.starts_with("banto-hub: データのフォルダ（外部: "),
            "{data}"
        );
        assert!(data.contains(&path.display().to_string()), "{data}");
        assert!(data.contains("ACL の対象外"), "{data}");
        assert!(data.contains("#661"), "{data}");
        let db = external_path_line(ExternalPath::Db, &path);
        assert!(db.starts_with("banto-hub: 設定 DB（外部: "), "{db}");
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
