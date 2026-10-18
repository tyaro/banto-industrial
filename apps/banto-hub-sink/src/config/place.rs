//! 設定ファイルの置き場と、読む前の保護の確かめ（#630 PR-2、2026-10-17 オーナー決定）。
//!
//! 設定ファイルには Hub の API キー（`admin` + `read`）が平文で入る。以前は
//! `%ProgramData%\BantoHub\` の直下か exe の隣に置いていて、どちらも一般の利用者
//! （`BUILTIN\Users`）が読め、ルート直下には一般の利用者が先に同じ名前のファイルを
//! 作れた。そこで置き場を **`%ProgramData%\BantoHub\sink\` だけ**にし、置き場と
//! ファイルが保護されていなければ起動しない（破壊的変更。互換は保たない）。
//!
//! ## どこを読むか（[`decide_paths`]、純関数）
//!
//! - **LocalSystem のサービス**（[`RunMode::Service`]）: 環境変数の上書きを**見ない**。
//!   置き場は `<ProgramData>\BantoHub\sink\banto-hub-sink.toml` だけ。ProgramData は
//!   既知のフォルダ（`SHGetKnownFolderPath(FOLDERID_ProgramData)`）から取り、
//!   `BANTO_HUB_ROOT` には追従しない。
//! - **コンソールの `run`**（[`RunMode::Console`]、開発・利用者として動かす形）:
//!   [`super::ENV_CONFIG_PATH`] があればそのファイル（絶対パスに限る）、無ければ上と同じ。
//!   環境変数で指したファイルにも**同じ保護の確かめ**をかける。`BANTO_HUB_SINK_CONFIG` を
//!   指していても、旧い置き場に toml があれば止まる（移行を強制するため。#655 のレビュー Nit 1）。
//! - 旧い置き場（ルート直下の `banto-hub-sink.toml` と exe の隣）は読まない。そこに
//!   ファイルがあれば（辿らずに見るだけ）、移し方とキーの失効・再発行を案内して起動しない。
//!   旧いファイルを消したり動かしたりはしない。
//!
//! ## 読み方（[`read_protected`]。PR-1 の申し送りの順）
//!
//! 1. 旧い置き場のファイルの有無（`inspect_file`、辿らない。確かめられなければ止める）。
//! 2. 置き場を `pin_dir`（辿らずに開き、持っている間は名前を変えさせない）→ 本当のパスが
//!    期待と同じ場所か → `security()` + `exposure`（所有者が SYSTEM・Administrators・
//!    TrustedInstaller 以外、それ以外への許可の ACE、null DACL、reparse point）。
//! 3. 置き場のハンドルを基準に `open_child_file`（辿らない）→ 本当のパス →
//!    `security()` + `exposure`（加えてハードリンク）。
//! 4. 理由が 1 つでもあれば、置き場・ファイルの理由を 1 行ずつ出して起動しない
//!    （`banto-hub-elev prepare-sink-config` を案内）。中身は読まない。
//! 5. 全部通ったら、確かめた**そのハンドルのまま** `read_to_end`（上限
//!    [`MAX_CONFIG_BYTES`]）。
//!
//! OS を触る操作は [`PlaceOps`] を通す（Windows の実装は `WindowsPlaceOps`、テストは偽物）。
//! 非 Windows ではこの確かめができないので、起動しない（[`super::ConfigError::UnsupportedPlatform`]）。

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use banto_safe_root::acl::{exposure, Exposure, FileSecurity};
use banto_safe_root::guard::paths_match;

use super::{ConfigError, DEFAULT_CONFIG_FILE_NAME, PROGRAM_DATA_SUBDIR};

/// 置き場のフォルダ名（`<ProgramData>\BantoHub\sink\`）。
pub const SINK_DIR: &str = "sink";

/// 設定ファイルの大きさの上限（これを超えるものは読まずに断る）。
pub const MAX_CONFIG_BYTES: usize = 64 * 1024;

/// どの形で動いているか（環境変数の上書きを見るか）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    /// LocalSystem のサービス（`run-service`）。環境変数の上書きを見ない。
    Service,
    /// コンソールの `run`（開発・利用者として動かす形）。環境変数の上書きを見る。
    Console,
}

/// 読む設定ファイルを、何で決めたか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// 既定の置き場（`<ProgramData>\BantoHub\sink\`）。
    Default,
    /// コンソールの `run` で、環境変数 [`super::ENV_CONFIG_PATH`] が指したファイル。
    Env,
}

/// [`decide_paths`] の結果: 読むファイルと、あれば起動しない旧い置き場。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// 置き場（フォルダ）。
    pub dir: PathBuf,
    /// 置き場の中のファイル名（名前 1 要素）。
    pub file_name: String,
    /// 何で決めたか（案内の文言を分ける）。
    pub source: Source,
    /// 既定の置き場（旧い置き場の案内に出す移し先）。ProgramData を取れなければ `None`。
    pub default_file: Option<PathBuf>,
    /// 旧い置き場（ルート直下・exe の隣）。ここにファイルがあれば起動しない。
    pub legacy: Vec<PathBuf>,
}

impl Paths {
    /// 読むファイルのパス（表示用）。
    pub fn file(&self) -> PathBuf {
        self.dir.join(&self.file_name)
    }
}

/// 読むファイルと旧い置き場を決める（純関数。モジュール doc「どこを読むか」）。
///
/// - `env_value`: 環境変数 [`super::ENV_CONFIG_PATH`] の値。[`RunMode::Service`] では渡されても
///   見ない。
/// - `program_data`: 既知のフォルダの ProgramData（取れなければ理由）。
/// - `exe_dir`: この exe のフォルダ（旧い置き場の 1 つ）。
pub fn decide_paths(
    mode: RunMode,
    env_value: Option<OsString>,
    program_data: Result<PathBuf, String>,
    exe_dir: Option<&Path>,
) -> Result<Paths, ConfigError> {
    let product_root = program_data
        .as_ref()
        .ok()
        .map(|pd| pd.join(PROGRAM_DATA_SUBDIR));
    let mut legacy = Vec::new();
    if let Some(root) = &product_root {
        legacy.push(root.join(DEFAULT_CONFIG_FILE_NAME));
    }
    if let Some(dir) = exe_dir {
        legacy.push(dir.join(DEFAULT_CONFIG_FILE_NAME));
    }
    let default_file = product_root
        .as_ref()
        .map(|root| root.join(SINK_DIR).join(DEFAULT_CONFIG_FILE_NAME));

    let env_value = match mode {
        RunMode::Service => None,
        RunMode::Console => env_value.filter(|value| !value.is_empty()),
    };
    if let Some(value) = env_value {
        let path = PathBuf::from(value);
        let reason = if !path.is_absolute() {
            Some("絶対パスではありません")
        } else if has_dot_segment(&path) {
            Some("「.」や「..」の要素を含みます")
        } else {
            None
        };
        if let Some(reason) = reason {
            return Err(ConfigError::EnvInvalid {
                value: path,
                reason,
            });
        }
        let (Some(dir), Some(name)) = (
            path.parent().map(Path::to_path_buf),
            path.file_name()
                .and_then(|n| n.to_str())
                .map(str::to_string),
        ) else {
            return Err(ConfigError::EnvInvalid {
                value: path,
                reason: "フォルダとファイル名に分けられません",
            });
        };
        return Ok(Paths {
            dir,
            file_name: name,
            source: Source::Env,
            default_file,
            legacy,
        });
    }

    let root = product_root.ok_or_else(|| {
        ConfigError::ProgramDataUnavailable(program_data.err().unwrap_or_default())
    })?;
    Ok(Paths {
        dir: root.join(SINK_DIR),
        file_name: DEFAULT_CONFIG_FILE_NAME.to_string(),
        source: Source::Default,
        default_file,
        legacy,
    })
}

/// `.`・`..` の要素があるか（OS が開くときに正規化して、確かめた場所と別の場所を指しうる）。
fn has_dot_segment(path: &Path) -> bool {
    path.components().any(|c| {
        matches!(
            c,
            std::path::Component::CurDir | std::path::Component::ParentDir
        )
    })
}

/// 置き場・ファイルが保護されていない理由 1 つ（1 行になる）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// `banto_safe_root::acl::exposure` の理由。
    Exposure(Exposure),
    /// 置き場がフォルダではない。
    NotADirectory,
    /// 設定ファイルがファイルではなくフォルダ。
    IsADirectory,
    /// 開いたハンドルの本当のパスが期待と違う場所（途中の要素のジャンクション・短い名前）。
    RealPathDiffers(PathBuf),
}

impl Problem {
    /// 1 行の文言（純関数）。
    pub fn text(&self) -> String {
        match self {
            Self::Exposure(found) => exposure_text(found),
            Self::NotADirectory => "フォルダではありません".to_string(),
            Self::IsADirectory => "ファイルではなくフォルダです".to_string(),
            Self::RealPathDiffers(real) => format!(
                "本当の場所（{}）が期待と違います（途中のフォルダがジャンクションか短い名前の可能性があります）",
                real.display()
            ),
        }
    }

    /// ほかの利用者が中身を読めた可能性がある理由か（キーの失効・再発行を案内する）。
    pub fn may_have_leaked(&self) -> bool {
        matches!(
            self,
            Self::Exposure(Exposure::AllowsOther { .. } | Exposure::NullDacl)
        )
    }
}

/// [`Exposure`] の文言（純関数）。banto-hub-core の `sink_config_prepare::exposure_text`
/// （`banto-hub-elev prepare-sink-config`）と文言を揃える（片方を直したらもう一方も）。
pub fn exposure_text(found: &Exposure) -> String {
    match found {
        Exposure::ReparsePoint => {
            "ジャンクション・シンボリックリンクなど（reparse point）です".to_string()
        }
        Exposure::HardLinked(links) => format!(
            "ハードリンクです（名前が {links} 個あり、ほかの場所の名前から同じ中身に届きます）"
        ),
        Exposure::ForeignOwner(owner) => format!(
            "所有者（{owner}）が SYSTEM・Administrators ではありません（管理者がメモ帳で編集した後もこうなります）"
        ),
        Exposure::NullDacl => "DACL が null です（だれでもフルアクセスできます）".to_string(),
        Exposure::AllowsOther { sid, mask, .. } => format!(
            "SYSTEM・Administrators 以外（{sid}）にアクセスを許しています（アクセス {mask:#x}）"
        ),
        Exposure::UnknownAce { ace_type, .. } => {
            format!("種類の分からない ACE（種類 {ace_type}）があります")
        }
        Exposure::UnreadableSid { ace_type, .. } => {
            format!("SID を読めない ACE（種類 {ace_type}）があります")
        }
    }
}

/// [`PlaceOps`] の開く操作の失敗。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenError {
    /// アクセスが拒否された（SYSTEM・Administrators だけの置き場を一般の利用者が開いた）。
    Denied,
    /// 共有違反（`ERROR_SHARING_VIOLATION`）。設定ファイルは読み取りだけを共有して開くので、
    /// 別のプロセスが書き込みに開いていると開けない（#630 PR-1 のレビュー）。
    InUse,
    /// それ以外（理由）。
    Other(String),
}

/// 設定ファイルを読むときに OS に対してする操作（Windows の実装は `WindowsPlaceOps`。
/// テストは偽物）。
pub trait PlaceOps {
    /// 辿らずに開いた置き場（持っている間は名前の変更・削除ができない）。
    type Dir;
    /// 置き場のハンドルを基準に辿らずに開いたファイル。
    type File;
    /// パスにファイル・フォルダがあるか（辿らずに見るだけ。中身は読まない）。
    fn exists(&self, path: &Path) -> Result<bool, String>;
    /// フォルダを辿らずに開く（無ければ `Ok(None)`）。
    fn pin_dir(&self, dir: &Path) -> Result<Option<Self::Dir>, OpenError>;
    /// 開いたハンドルの本当のパス。
    fn dir_final_path(&self, dir: &Self::Dir) -> PathBuf;
    /// 開いたハンドルから読んだ属性・所有者・DACL。
    fn dir_security(&self, dir: &Self::Dir) -> Result<FileSecurity, String>;
    /// 置き場のハンドルを基準に、名前 1 要素の子を辿らずに開く（無ければ `Ok(None)`）。
    fn open_child_file(&self, dir: &Self::Dir, name: &str)
        -> Result<Option<Self::File>, OpenError>;
    /// 開いたハンドルの本当のパス。
    fn file_final_path(&self, file: &Self::File) -> PathBuf;
    /// 開いた時点でハンドルから読んだ属性・リンク数・所有者・DACL。
    fn file_security(&self, file: &Self::File) -> FileSecurity;
    /// 開いたハンドルのまま、先頭から `max_len` バイトまで読む（超えたら理由）。
    fn read(&self, file: &mut Self::File, max_len: usize) -> Result<Vec<u8>, String>;
}

/// 置き場とファイルを確かめてから読む（モジュール doc「読み方」）。返すのは読んだファイルの
/// パスと中身（UTF-8）。保護されていなければ中身を読まずに断る。
pub fn read_protected<O: PlaceOps>(
    ops: &O,
    paths: &Paths,
) -> Result<(PathBuf, String), ConfigError> {
    // 1. 旧い置き場（辿らずに見るだけ）。
    let mut found = Vec::new();
    for legacy in &paths.legacy {
        match ops.exists(legacy) {
            Ok(true) => found.push(legacy.clone()),
            Ok(false) => {}
            Err(reason) => {
                return Err(ConfigError::Unverifiable {
                    path: legacy.clone(),
                    reason,
                })
            }
        }
    }
    if !found.is_empty() {
        return Err(ConfigError::LegacyFound {
            paths: found,
            new_place: paths.default_file.clone(),
        });
    }

    // 2. 置き場。
    let file_path = paths.file();
    let env = paths.source == Source::Env;
    let dir = match ops.pin_dir(&paths.dir) {
        Ok(Some(dir)) => dir,
        Ok(None) => {
            return Err(ConfigError::PlaceMissing {
                dir: paths.dir.clone(),
                env,
            })
        }
        Err(OpenError::Denied) => {
            return Err(ConfigError::AccessDenied {
                path: paths.dir.clone(),
            })
        }
        Err(OpenError::InUse) => {
            return Err(ConfigError::Unverifiable {
                path: paths.dir.clone(),
                reason: "共有違反で開けませんでした".to_string(),
            })
        }
        Err(OpenError::Other(reason)) => {
            return Err(ConfigError::Unverifiable {
                path: paths.dir.clone(),
                reason,
            })
        }
    };
    let dir_real = ops.dir_final_path(&dir);
    let mut dir_problems = Vec::new();
    if !paths_match(&dir_real, &paths.dir) {
        dir_problems.push(Problem::RealPathDiffers(dir_real.clone()));
    }
    let dir_security = ops
        .dir_security(&dir)
        .map_err(|reason| ConfigError::Unverifiable {
            path: paths.dir.clone(),
            reason,
        })?;
    if !dir_security.is_dir {
        dir_problems.push(Problem::NotADirectory);
    }
    dir_problems.extend(exposure(&dir_security).into_iter().map(Problem::Exposure));
    let unprotected =
        |dir_problems: Vec<Problem>, file_problems: Vec<Problem>| ConfigError::Unprotected {
            dir: paths.dir.clone(),
            dir_problems,
            file: file_path.clone(),
            file_problems,
            env,
        };
    // 置き場がリンク・フォルダでないなら、中へは進まない（リンクの先を開かない）。
    if dir_security.is_reparse_point || !dir_security.is_dir {
        return Err(unprotected(dir_problems, Vec::new()));
    }

    // 3. ファイル（置き場のハンドル基準）。
    let mut file = match ops.open_child_file(&dir, &paths.file_name) {
        Ok(Some(file)) => file,
        Ok(None) if dir_problems.is_empty() => {
            return Err(ConfigError::FileMissing {
                path: file_path,
                env,
            })
        }
        Ok(None) => return Err(unprotected(dir_problems, Vec::new())),
        Err(OpenError::Denied) => return Err(ConfigError::AccessDenied { path: file_path }),
        Err(OpenError::InUse) => return Err(ConfigError::FileInUse { path: file_path }),
        Err(OpenError::Other(reason)) => {
            return Err(ConfigError::Unverifiable {
                path: file_path,
                reason,
            })
        }
    };
    let mut file_problems = Vec::new();
    let file_real = ops.file_final_path(&file);
    let expected = dir_real.join(&paths.file_name);
    if !paths_match(&file_real, &expected) {
        file_problems.push(Problem::RealPathDiffers(file_real));
    }
    let file_security = ops.file_security(&file);
    if file_security.is_dir {
        file_problems.push(Problem::IsADirectory);
    }
    file_problems.extend(exposure(&file_security).into_iter().map(Problem::Exposure));

    // 4. 理由があれば、中身を読まずに断る。
    if !dir_problems.is_empty() || !file_problems.is_empty() {
        return Err(unprotected(dir_problems, file_problems));
    }

    // 5. 確かめたハンドルのまま読み、**読んだらすぐ閉じる**（#648 の再レビュー Nit 1）。
    // 共有は読み取りだけなので、sink が持ち続けると elev（`prepare-sink-config`）や
    // インストーラが設定を整える・書き換えるときに共有違反になる。ファイル・置き場の
    // ハンドルはこの関数の外へ出さない（返すのはパスと中身だけ）。失敗の経路でも、ここで
    // 返るときに閉じる。
    let read = ops.read(&mut file, MAX_CONFIG_BYTES);
    drop(file);
    drop(dir);
    let bytes = read.map_err(|reason| ConfigError::Unverifiable {
        path: file_path.clone(),
        reason,
    })?;
    let text = String::from_utf8(bytes).map_err(|_| ConfigError::Parse {
        path: file_path.clone(),
        message: "UTF-8 ではありません".to_string(),
    })?;
    Ok((file_path, text))
}

/// Windows の実装（本番）。
#[cfg(windows)]
pub mod win {
    use std::path::{Path, PathBuf};

    use banto_safe_root::acl::{FileSecurity, PinnedDir, PinnedFile};
    use banto_safe_root::SafeRootError;

    use super::{OpenError, PlaceOps};

    /// 本番の操作（`banto_safe_root::acl` の部品）。
    pub struct WindowsPlaceOps;

    /// `ERROR_SHARING_VIOLATION`。
    const ERROR_SHARING_VIOLATION: i32 = 32;

    fn open_error(error: SafeRootError) -> OpenError {
        match error {
            SafeRootError::Io {
                os_error: Some(ERROR_SHARING_VIOLATION),
                ..
            } => OpenError::InUse,
            SafeRootError::Io {
                kind: std::io::ErrorKind::PermissionDenied,
                ..
            } => OpenError::Denied,
            other => OpenError::Other(other.to_string()),
        }
    }

    impl PlaceOps for WindowsPlaceOps {
        type Dir = PinnedDir;
        type File = PinnedFile;

        fn exists(&self, path: &Path) -> Result<bool, String> {
            banto_safe_root::acl::inspect_file(path)
                .map(|found| found.is_some())
                .map_err(|err| err.to_string())
        }

        fn pin_dir(&self, dir: &Path) -> Result<Option<PinnedDir>, OpenError> {
            banto_safe_root::acl::pin_dir(dir).map_err(open_error)
        }

        fn dir_final_path(&self, dir: &PinnedDir) -> PathBuf {
            dir.final_path().to_path_buf()
        }

        fn dir_security(&self, dir: &PinnedDir) -> Result<FileSecurity, String> {
            dir.security().map_err(|err| err.to_string())
        }

        fn open_child_file(
            &self,
            dir: &PinnedDir,
            name: &str,
        ) -> Result<Option<PinnedFile>, OpenError> {
            dir.open_child_file(name).map_err(open_error)
        }

        fn file_final_path(&self, file: &PinnedFile) -> PathBuf {
            file.final_path().to_path_buf()
        }

        fn file_security(&self, file: &PinnedFile) -> FileSecurity {
            file.security().clone()
        }

        fn read(&self, file: &mut PinnedFile, max_len: usize) -> Result<Vec<u8>, String> {
            file.read_to_end(max_len).map_err(|err| err.to_string())
        }
    }

    /// 既知のフォルダの ProgramData（環境変数 `ProgramData` は読まない）。
    pub fn program_data() -> Result<PathBuf, String> {
        use banto_safe_root::guard::MachineEnvironment;
        banto_safe_root::guard::win::RegistryMachineEnvironment.program_data()
    }
}

#[cfg(test)]
mod tests;
