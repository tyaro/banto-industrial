//! T17-1（docs/banto-hub-t17-design.md §3「T17-1」・P2、
//! docs/banto-hub-desktop-plan.md §16.2「mutex 命名決定」）:
//! `HubRuntime::start`冒頭で取る profile 単位の排他。3層のうち (b)/(c) を
//! ここに実装する（(a) SCM `query_status`は`crate::service_manager`側、
//! T17-0 のまま・このスライスでは呼ばない）。
//!
//! - **(b) Windows**: named mutex `Global\BantoHub.<profile-id>`
//!   （[`crate::profile_paths::mutex_name`]、desktop-plan §16.2）を
//!   `CreateMutexW`で取得する。既に別プロセスが所有していれば
//!   `GetLastError() == ERROR_ALREADY_EXISTS`で判定し、安全側で起動を
//!   拒否する（[`ProfileLockError::AlreadyHeld`]）。
//! - **(c) 全 OS**: profile ディレクトリ直下の`profile.lock`へ所有者
//!   PID・ホスト種別・取得時刻を JSON で書く - fallback UI の「mutex:
//!   所有者不明」等の診断情報源（ロック自体の正当性は Windows では (b)、
//!   非 Windows では下記 flock が持つ）。
//! - **非 Windows**: `Global\`名前空間が無いため、`profile.lock`への
//!   `flock(2)`（`LOCK_EX|LOCK_NB`）**自体**を排他の実体にする - 同一
//!   ファイルへの2つ目の`try_acquire_profile_lock`は Linux 上でも確実に
//!   失敗する（CI で検証可能）。
//!
//! SCM 経由の状態確認（T17-0、`crate::service_manager`）はこのモジュールの
//! スコープ外 - `HubRuntime::start`はこのモジュールの[`try_acquire_profile_lock`]
//! だけを呼ぶ。

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::profile_paths::ProfilePaths;

/// profile lock の診断ファイル名（`{profile_dir}/profile.lock`）。
pub const LOCK_FILE_NAME: &str = "profile.lock";

/// profile を運転しようとしているホストの種別（[`ProfileOwnerInfo::host_kind`]
/// に文字列として書き込む診断用の値 - `crate::service_manager::ServiceManager`
/// が扱う SCM 状態とは無関係）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HubHostKind {
    /// `bin/banto-hub.rs`（引数なし、Ctrl-C で停止）。
    Console,
    /// `bin/banto_hub/win_service.rs`（Windows サービス）。
    Service,
    /// `apps/banto-hub/src-tauri`（デスクトップシェル）。
    Shell,
}

impl std::fmt::Display for HubHostKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HubHostKind::Console => write!(f, "console"),
            HubHostKind::Service => write!(f, "service"),
            HubHostKind::Shell => write!(f, "shell"),
        }
    }
}

/// `profile.lock`の中身（診断用 JSON）。ロックの正当性そのものはこの内容が
/// 持つのではなく、OS レベルの機構（Windows: named mutex／非 Windows:
/// flock）が持つ - このモジュール doc 参照。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProfileOwnerInfo {
    pub pid: u32,
    /// [`HubHostKind`]の`Display`表現（`"console"`/`"service"`/`"shell"`）。
    pub host_kind: String,
    pub acquired_at_unix_ms: i64,
}

/// [`try_acquire_profile_lock`]の失敗モード。
#[derive(Debug, Error)]
pub enum ProfileLockError {
    /// 既に別プロセスがこの profile を保持している（安全側で起動拒否）。
    /// `owner`は`profile.lock`から読めた場合のみ`Some`（診断用、
    /// このモジュール doc の「(c) 全 OS」節参照）。
    #[error("banto-hub: profile '{profile_id}' は既に別プロセスが使用中です（owner: {owner:?}）")]
    AlreadyHeld {
        profile_id: String,
        owner: Option<ProfileOwnerInfo>,
    },
    /// profile ディレクトリ作成・lock ファイル open/書き込みの I/O エラー。
    #[error("banto-hub: profile ロックの I/O に失敗しました: {0}")]
    Io(#[from] std::io::Error),
    /// `profile_id`自体が不正（`crate::profile_paths::validate_profile_id`
    /// が拒否する文字列）。`crate::runtime::HubRuntime::start`が
    /// `crate::profile_paths::resolve_profile_paths`の失敗をここへ変換して
    /// 使う（[`crate::runtime::HubStartError::ProfileLock`]が単一の変種で
    /// profile 関連の失敗を統一的に扱えるようにするため）。
    #[error("banto-hub: profile id が不正です: {0}")]
    InvalidProfile(#[from] crate::profile_paths::ProfileIdError),
}

/// `try_acquire_profile_lock`が成功した間だけ生存するガード。
/// `HubRuntime::start`が構築する`RunningHub`がこれを保持し、`Drop`で
/// OS レベルの排他（Windows: mutex handle の`CloseHandle`／非 Windows:
/// lock file の fd が閉じることによる自動`flock`解放）を返す。
pub struct ProfileLockGuard {
    lock_file_path: PathBuf,
    // 非 Windows: このファイルへの `flock(LOCK_EX)` が排他の実体そのもの
    // (モジュール doc「非 Windows」節)。`File`の`Drop`が fd を閉じる時点で
    // カーネルが自動的に unlock する - 明示的な `flock(LOCK_UN)` は不要。
    #[cfg(not(windows))]
    _lock_file: File,
    #[cfg(windows)]
    _mutex: WindowsMutexHandle,
}

impl ProfileLockGuard {
    /// 診断ファイル（`profile.lock`）の絶対パス - fallback UI（T16-2）が
    /// 「mutex: 所有者不明」等の表示に使う所有者情報の在り処として参照する
    /// 想定（このモジュール doc「(c) 全 OS」節）。
    pub fn lock_file_path(&self) -> &Path {
        &self.lock_file_path
    }
}

#[cfg(windows)]
struct WindowsMutexHandle(windows_sys::Win32::Foundation::HANDLE);

// `HANDLE`(`*mut c_void`)は本来 `!Send`/`!Sync`だが、Win32 の mutex handle
// はスレッドに紐付かない（生成スレッドと別スレッドから`CloseHandle`しても
// 安全 - Win32 ドキュメントの標準的な保証）ため、`RunningHub`を
// tokio マルチスレッドランタイム上で保持・`.await`越しに運ぶのに必要な
// `Send`/`Sync`をここだけ明示的に付与する。
#[cfg(windows)]
unsafe impl Send for WindowsMutexHandle {}
#[cfg(windows)]
unsafe impl Sync for WindowsMutexHandle {}

#[cfg(windows)]
impl Drop for WindowsMutexHandle {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

/// `paths`が指す profile の排他を取得する。`HubRuntime::start`が DB 初期化
/// より前に呼ぶ（このモジュール doc 参照）。
///
/// - `profile_dir`/`config`/`data`/`logs`を`create_dir_all`する（初回起動時
///   はまだ存在しないため）。
/// - Windows: `Global\BantoHub.<profile-id>`を`CreateMutexW`で取得する。
///   `ERROR_ALREADY_EXISTS`なら[`ProfileLockError::AlreadyHeld`]。
/// - 非 Windows: `profile.lock`を`flock(LOCK_EX|LOCK_NB)`する。既に
///   ロックされていれば同様に[`ProfileLockError::AlreadyHeld`]。
/// - 取得成功後、`profile.lock`へ`host_kind`を含む[`ProfileOwnerInfo`]を
///   上書きする（失敗しても致命的にはしない - 診断情報が更新されないだけ）。
pub fn try_acquire_profile_lock(
    paths: &ProfilePaths,
    host_kind: HubHostKind,
) -> Result<ProfileLockGuard, ProfileLockError> {
    std::fs::create_dir_all(&paths.profile_dir)?;
    if let Some(config_dir) = paths.db_path.parent() {
        std::fs::create_dir_all(config_dir)?;
    }
    std::fs::create_dir_all(&paths.data_dir)?;
    std::fs::create_dir_all(&paths.logs_dir)?;

    let lock_path = paths.profile_dir.join(LOCK_FILE_NAME);

    #[cfg(windows)]
    {
        acquire_windows(paths, host_kind, &lock_path)
    }
    #[cfg(not(windows))]
    {
        acquire_unix(paths, host_kind, &lock_path)
    }
}

#[cfg(not(windows))]
fn acquire_unix(
    paths: &ProfilePaths,
    host_kind: HubHostKind,
    lock_path: &Path,
) -> Result<ProfileLockGuard, ProfileLockError> {
    use std::os::unix::io::AsRawFd;

    // `truncate(false)`: `flock`取得前に既存内容を消さない - 取得に失敗
    // した場合、既存の`ProfileOwnerInfo`を診断用に読み直す
    // （`read_owner_info`）ため。取得成功後の上書きは`write_owner_info`が
    // 明示的に`set_len(0)`する。
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)?;

    // このモジュール doc「非 Windows」節: `flock`自体が排他の実体 -
    // `LOCK_NB`なので既に他プロセスが保持していれば即座に`EWOULDBLOCK`で
    // 返る（ブロックしない - 起動処理を止めないため）。
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        let owner = read_owner_info(lock_path);
        return Err(ProfileLockError::AlreadyHeld {
            profile_id: paths.profile_id.clone(),
            owner,
        });
    }

    write_owner_info(&mut file, host_kind)?;

    Ok(ProfileLockGuard {
        lock_file_path: lock_path.to_path_buf(),
        _lock_file: file,
    })
}

#[cfg(windows)]
fn acquire_windows(
    paths: &ProfilePaths,
    host_kind: HubHostKind,
    lock_path: &Path,
) -> Result<ProfileLockGuard, ProfileLockError> {
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, SetLastError, ERROR_ALREADY_EXISTS,
    };
    use windows_sys::Win32::System::Threading::CreateMutexW;

    let name = crate::profile_paths::mutex_name(&paths.profile_id);
    let wide_name: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();

    // SAFETY: `wide_name`は呼び出しが終わるまでスコープ内で生存する
    // NUL 終端 UTF-16 文字列。`lpmutexattributes`に null を渡すのは既定の
    // セキュリティ記述子（作成プロセスの資格情報が継承される既定動作）で
    // 十分なため - `Global\`名前空間へ書き込むにはこのプロセス自体に
    // `SeCreateGlobalPrivilege`相当の権限が必要（通常ユーザーは既定で
    // 保有、docs/banto-hub-t17-design.md §5「要 Windows 実機スパイク」）。
    //
    // `SetLastError(0)`は CreateMutexW の既知の落とし穴対策 - 新規作成に
    // 成功した場合でも前回のスレッド last-error をクリアしないことがある
    // ため、呼び出し直前に 0 にしてから `ERROR_ALREADY_EXISTS` を判定する。
    let handle = unsafe {
        SetLastError(0);
        CreateMutexW(std::ptr::null(), 1, wide_name.as_ptr())
    };
    if handle.is_null() {
        return Err(ProfileLockError::Io(std::io::Error::last_os_error()));
    }
    let already_exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    if already_exists {
        unsafe {
            CloseHandle(handle);
        }
        let owner = read_owner_info(lock_path);
        return Err(ProfileLockError::AlreadyHeld {
            profile_id: paths.profile_id.clone(),
            owner,
        });
    }

    // 診断用ファイル（このモジュール doc「(c) 全 OS」節）- Windows では
    // 排他の実体ではないので、書き込み失敗は致命的にしない。
    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(lock_path)
    {
        let _ = write_owner_info(&mut file, host_kind);
    }

    Ok(ProfileLockGuard {
        lock_file_path: lock_path.to_path_buf(),
        _mutex: WindowsMutexHandle(handle),
    })
}

fn write_owner_info(file: &mut File, host_kind: HubHostKind) -> std::io::Result<()> {
    let info = ProfileOwnerInfo {
        pid: std::process::id(),
        host_kind: host_kind.to_string(),
        acquired_at_unix_ms: now_unix_ms(),
    };
    let json = serde_json::to_string_pretty(&info).unwrap_or_else(|_| "{}".to_string());
    file.set_len(0)?;
    file.seek(SeekFrom::Start(0))?;
    file.write_all(json.as_bytes())?;
    file.flush()?;
    Ok(())
}

fn read_owner_info(lock_path: &Path) -> Option<ProfileOwnerInfo> {
    let mut file = File::open(lock_path).ok()?;
    let mut contents = String::new();
    file.read_to_string(&mut contents).ok()?;
    serde_json::from_str(&contents).ok()
}

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile_paths::resolve_profile_paths;
    use crate::test_support::TempDir;

    #[test]
    fn try_acquire_profile_lock_creates_profile_directories() {
        let root = TempDir::new("profile-lock-layout");
        let paths = resolve_profile_paths(root.path(), "default").expect("valid profile id");

        let _guard =
            try_acquire_profile_lock(&paths, HubHostKind::Console).expect("first acquire ok");

        assert!(paths.profile_dir.is_dir());
        assert!(paths.data_dir.is_dir());
        assert!(paths.logs_dir.is_dir());
        assert!(paths.db_path.parent().unwrap().is_dir());
        assert!(paths.profile_dir.join(LOCK_FILE_NAME).is_file());
    }

    #[test]
    fn try_acquire_profile_lock_writes_owner_diagnostics() {
        let root = TempDir::new("profile-lock-owner-info");
        let paths = resolve_profile_paths(root.path(), "default").expect("valid profile id");

        let _guard =
            try_acquire_profile_lock(&paths, HubHostKind::Service).expect("first acquire ok");

        let owner =
            read_owner_info(&paths.profile_dir.join(LOCK_FILE_NAME)).expect("owner info written");
        assert_eq!(owner.pid, std::process::id());
        assert_eq!(owner.host_kind, "service");
    }

    /// このモジュール doc「非 Windows」節の受入条件そのもの: 同一プロセス内
    /// (Linux CI でも実行できる)で同じ profile を2重に`try_acquire`すると、
    /// 2回目は`AlreadyHeld`になる。
    #[test]
    fn second_acquire_on_the_same_profile_fails() {
        let root = TempDir::new("profile-lock-double-acquire");
        let paths = resolve_profile_paths(root.path(), "default").expect("valid profile id");

        let _first =
            try_acquire_profile_lock(&paths, HubHostKind::Console).expect("first acquire ok");

        match try_acquire_profile_lock(&paths, HubHostKind::Shell) {
            Err(ProfileLockError::AlreadyHeld { profile_id, owner }) => {
                assert_eq!(profile_id, "default");
                let owner = owner.expect("first owner diagnostics should be readable");
                assert_eq!(owner.host_kind, "console");
            }
            Err(other) => panic!("expected AlreadyHeld, got {other}"),
            Ok(_) => panic!("second acquire should fail while the first guard is still held"),
        }
    }

    #[test]
    fn different_profile_dirs_can_both_acquire() {
        let root_a = TempDir::new("profile-lock-independent-a");
        let root_b = TempDir::new("profile-lock-independent-b");
        let paths_a = resolve_profile_paths(root_a.path(), "default").expect("valid profile id");
        let paths_b = resolve_profile_paths(root_b.path(), "default").expect("valid profile id");

        let _guard_a =
            try_acquire_profile_lock(&paths_a, HubHostKind::Console).expect("acquire a ok");
        let _guard_b =
            try_acquire_profile_lock(&paths_b, HubHostKind::Console).expect("acquire b ok");
    }

    #[test]
    fn lock_is_released_after_guard_drop_allowing_reacquire() {
        let root = TempDir::new("profile-lock-release-on-drop");
        let paths = resolve_profile_paths(root.path(), "default").expect("valid profile id");

        {
            let _guard =
                try_acquire_profile_lock(&paths, HubHostKind::Console).expect("first acquire ok");
        }

        let _second =
            try_acquire_profile_lock(&paths, HubHostKind::Console).expect("reacquire after drop");
    }
}
