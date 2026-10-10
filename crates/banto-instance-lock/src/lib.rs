//! `banto-instance-lock`（#392 A1）: 同じデータを 2 つのプロセスが同時に開か
//! ないための、プロセス間の単一インスタンス排他。
//!
//! もとは `banto-hub-core::profile_lock`（T17-1）の中身で、2 つ目のアプリ
//! （ChronoGazer）が同じ保護を必要としたため、重複させずに共有部分を
//! ここへ移した。**排他の単位（何と何を衝突させるか）は呼び出し側が決める**:
//! この crate は「mutex 名」と「診断ファイルのパス」を受け取るだけで、
//! アプリ固有の命名は持たない。
//!
//! # 仕組み（banto-hub-core から移した設計をそのまま保つ）
//!
//! - **Windows**: named mutex（例 `Global\BantoHub.<profile-id>`）を
//!   `CreateMutexW` で取得する。既に別プロセスが所有していれば
//!   `ERROR_ALREADY_EXISTS` で判定し [`InstanceLockError::AlreadyHeld`]。
//!   LocalSystem（Session 0 のサービス）が保持中のとき、ユーザーセッションの
//!   `CreateMutexW` は `ERROR_ACCESS_DENIED` で null を返すので、
//!   `OpenMutexW(SYNCHRONIZE)` で「開けた＝他者が保持中」と確定する
//!   （2026-08-10 実機観察）。
//! - **全 OS（診断）**: 診断ファイル（`lock_file`）へ所有者 PID・ホスト種別・
//!   取得時刻を JSON で書く。失敗経路で「誰が持っているか」を出す情報源で、
//!   Windows ではロックの正当性そのものは持たない。
//! - **非 Windows**: `Global\` 名前空間が無いので、診断ファイルへの
//!   `flock(LOCK_EX|LOCK_NB)` **自体**を排他の実体にする（Linux CI でも
//!   二重取得の失敗を検証できる）。
//!
//! ガード（[`InstanceLockGuard`]）を持っている間だけ排他が続く。`Drop` で
//! 返る（Windows: handle の `CloseHandle`／非 Windows: fd が閉じて自動解放）。
//! プロセスが落ちても OS が返すので、古いロックが残って起動できなくなる
//! ことは無い（診断ファイルが残っても排他には影響しない）。
//!
//! # 排他の単位を「パスから」作る補助
//!
//! banto-hub は profile-id という名前で単位を決める。ChronoGazer のように
//! 「同じ DB ファイルを開く 2 プロセス」を衝突させたいアプリのために、
//! [`path_scope_id`] が対象パスを正規化して短い安定 ID にする（mutex 名に
//! `\` や長いパスを入れられないため）。

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// 診断ファイルの中身（診断用 JSON）。排他の正当性はこの内容ではなく OS の
/// 機構（Windows: named mutex／非 Windows: flock）が持つ。
/// フィールド名は banto-hub の既存 `profile.lock` と同一（既存インストール
/// のファイルをそのまま読める）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OwnerInfo {
    pub pid: u32,
    /// 呼び出し側が決める自由文字列（banto-hub: `"console"`/`"service"`/`"shell"`、
    /// ChronoGazer: `"desktop"`/`"serve"`）。
    pub host_kind: String,
    pub acquired_at_unix_ms: i64,
}

/// [`try_acquire`] の失敗モード。
#[derive(Debug, Error)]
pub enum InstanceLockError {
    /// 既に別プロセスが保持している（安全側で起動を拒否する）。`owner` は
    /// 診断ファイルが読めた場合のみ `Some`。
    #[error("既に別プロセスが使用中です（owner: {owner:?}）")]
    AlreadyHeld { owner: Option<OwnerInfo> },
    /// 診断ファイルの open/書き込み、mutex 作成の I/O エラー。
    #[error("ロックの I/O に失敗しました: {0}")]
    Io(#[from] std::io::Error),
}

/// [`try_acquire`] が成功した間だけ生存するガード。
pub struct InstanceLockGuard {
    lock_file_path: PathBuf,
    // 非 Windows: このファイルへの `flock(LOCK_EX)` が排他の実体。`File` の
    // `Drop` が fd を閉じる時点でカーネルが自動で unlock する。
    #[cfg(not(windows))]
    _lock_file: File,
    #[cfg(windows)]
    _mutex: WindowsMutexHandle,
}

impl InstanceLockGuard {
    /// 診断ファイルのパス。
    pub fn lock_file_path(&self) -> &Path {
        &self.lock_file_path
    }
}

#[cfg(windows)]
struct WindowsMutexHandle(windows_sys::Win32::Foundation::HANDLE);

// `HANDLE`(`*mut c_void`) は本来 `!Send`/`!Sync` だが、Win32 の mutex handle
// はスレッドに紐付かない（別スレッドから `CloseHandle` しても安全）ため、
// tokio マルチスレッドランタイム上でガードを保持するのに必要な
// `Send`/`Sync` をここだけ明示的に付与する。
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

/// 排他を取得する。
///
/// - `mutex_name`: Windows で使う named mutex の完全名（`Global\...`）。
///   非 Windows では使わない。
/// - `lock_file`: 診断ファイル（非 Windows では flock の対象でもある）。
///   親ディレクトリは呼び出し側が用意しておく（無ければ `Io`）。
/// - `host_kind`: 診断ファイルに書く自由文字列。
///
/// 取得成功後、診断ファイルへ [`OwnerInfo`] を上書きする（失敗しても Windows
/// では致命的にしない — 診断情報が更新されないだけ）。
pub fn try_acquire(
    mutex_name: &str,
    lock_file: &Path,
    host_kind: &str,
) -> Result<InstanceLockGuard, InstanceLockError> {
    #[cfg(windows)]
    {
        acquire_windows(mutex_name, lock_file, host_kind)
    }
    #[cfg(not(windows))]
    {
        let _ = mutex_name;
        acquire_unix(lock_file, host_kind)
    }
}

#[cfg(not(windows))]
fn acquire_unix(lock_path: &Path, host_kind: &str) -> Result<InstanceLockGuard, InstanceLockError> {
    use std::os::unix::io::AsRawFd;

    // `truncate(false)`: `flock` 取得前に既存内容を消さない — 取得に失敗した
    // とき、既存の `OwnerInfo` を診断用に読み直すため。取得成功後の上書きは
    // `write_owner_info` が明示的に `set_len(0)` する。
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)?;

    // `LOCK_NB`: 既に他プロセスが保持していれば即座に `EWOULDBLOCK`（起動処理を
    // 止めない）。
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        return Err(InstanceLockError::AlreadyHeld {
            owner: read_owner_info(lock_path),
        });
    }

    write_owner_info(&mut file, host_kind)?;

    Ok(InstanceLockGuard {
        lock_file_path: lock_path.to_path_buf(),
        _lock_file: file,
    })
}

#[cfg(windows)]
fn acquire_windows(
    name: &str,
    lock_path: &Path,
    host_kind: &str,
) -> Result<InstanceLockGuard, InstanceLockError> {
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, SetLastError, ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS,
    };
    use windows_sys::Win32::System::Threading::{CreateMutexW, OpenMutexW};

    /// `OpenMutexW` の `dwDesiredAccess` — Win32 `SYNCHRONIZE`（0x0010_0000）。
    /// windows-sys 0.61 の既定 feature セットに定数 export が無いためリテラル化。
    const MUTEX_SYNCHRONIZE: u32 = 0x0010_0000;

    let wide_name: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();

    // SAFETY: `wide_name` は呼び出しが終わるまで生存する NUL 終端 UTF-16。
    // `lpmutexattributes` の null は既定のセキュリティ記述子（作成プロセスの
    // 資格情報が継承される既定動作）。`Global\` 名前空間への書き込みには
    // `SeCreateGlobalPrivilege` 相当が要る（通常ユーザーは既定で保有）。
    //
    // `SetLastError(0)` は CreateMutexW の既知の落とし穴対策 — 新規作成に
    // 成功しても前回のスレッド last-error をクリアしないことがあるため。
    let handle = unsafe {
        SetLastError(0);
        CreateMutexW(std::ptr::null(), 1, wide_name.as_ptr())
    };
    if handle.is_null() {
        // 2026-08-10 Windows 実機観察: LocalSystem（Session 0 サービス）が既に
        // mutex を保持していると、ユーザーセッションからの `CreateMutexW` は
        // `ERROR_ACCESS_DENIED`(5) で null を返す（`ERROR_ALREADY_EXISTS` に
        // ならない）。`OpenMutexW(SYNCHRONIZE)` で存在確認する — 開けたら
        // 他プロセスが保持中と確定できる。
        let last_error = unsafe { GetLastError() };
        if last_error == ERROR_ACCESS_DENIED {
            let open_handle = unsafe { OpenMutexW(MUTEX_SYNCHRONIZE, 0, wide_name.as_ptr()) };
            if !open_handle.is_null() {
                unsafe {
                    CloseHandle(open_handle);
                }
                return Err(InstanceLockError::AlreadyHeld {
                    owner: read_owner_info(lock_path),
                });
            }
            // `OpenMutexW` も拒否された場合（更に厳しい ACL 等）でも、診断
            // ファイルが読めるなら owner 付きの `AlreadyHeld` に正規化する。
            if let Some(owner) = read_owner_info(lock_path) {
                return Err(InstanceLockError::AlreadyHeld { owner: Some(owner) });
            }
        }
        return Err(InstanceLockError::Io(std::io::Error::last_os_error()));
    }
    let already_exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    if already_exists {
        unsafe {
            CloseHandle(handle);
        }
        return Err(InstanceLockError::AlreadyHeld {
            owner: read_owner_info(lock_path),
        });
    }

    // 診断用ファイル — Windows では排他の実体ではないので、書き込み失敗は
    // 致命的にしない。
    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(lock_path)
    {
        let _ = write_owner_info(&mut file, host_kind);
    }

    Ok(InstanceLockGuard {
        lock_file_path: lock_path.to_path_buf(),
        _mutex: WindowsMutexHandle(handle),
    })
}

fn write_owner_info(file: &mut File, host_kind: &str) -> std::io::Result<()> {
    let info = OwnerInfo {
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

/// 診断ファイルから所有者情報を読む（読めなければ `None`）。排他の正当性には
/// 関与しない読み取り専用ヘルパー（banto-hub の health プローブも使う）。
pub fn read_owner_info(lock_path: &Path) -> Option<OwnerInfo> {
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

/// 対象ファイルのパスから、排他の単位になる短い安定 ID（16 桁の小文字 hex）を
/// 作る。同じファイルを指すパス（相対・絶対・`..` 入り）は同じ ID になり、
/// 別のファイルは別の ID になる。
///
/// - 親ディレクトリを `create_dir_all` してから `canonicalize` し、ファイル名を
///   足す（対象ファイル自体はまだ無くてよい — 初回起動の DB）。
/// - Windows ではファイルシステムが大文字小文字を区別しないので、ハッシュの
///   前に小文字へそろえる。
/// - 戻り値の 2 つ目は正規化したパス（診断メッセージ用）。
pub fn path_scope_id(target: &Path) -> std::io::Result<(String, PathBuf)> {
    use sha2::{Digest, Sha256};

    let file_name = target.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "対象パスにファイル名がありません",
        )
    })?;
    let parent = match target.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    std::fs::create_dir_all(parent)?;
    let canonical = parent.canonicalize()?.join(file_name);

    let mut key = canonical.to_string_lossy().into_owned();
    if cfg!(windows) {
        key = key.to_lowercase();
    }
    let digest = Sha256::digest(key.as_bytes());
    let id: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
    Ok((id, canonical))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 実マシンで動いているアプリ（BantoHub サービス等）の mutex と衝突しない
    /// よう、テストごとに一意な名前を使う。
    fn unique_mutex_name(tag: &str) -> String {
        format!(
            "Global\\BantoInstanceLockTest.{tag}.{}.{}",
            std::process::id(),
            now_unix_ms()
        )
    }

    fn lock_path(dir: &tempfile::TempDir, name: &str) -> PathBuf {
        dir.path().join(name)
    }

    #[test]
    fn second_acquire_with_same_identity_fails_and_reports_owner() {
        let dir = tempfile::tempdir().unwrap();
        let name = unique_mutex_name("double");
        let path = lock_path(&dir, "x.lock");

        let _first = try_acquire(&name, &path, "first").expect("first acquire ok");

        match try_acquire(&name, &path, "second") {
            Err(InstanceLockError::AlreadyHeld { owner }) => {
                let owner = owner.expect("owner diagnostics should be readable");
                assert_eq!(owner.host_kind, "first");
                assert_eq!(owner.pid, std::process::id());
            }
            Err(other) => panic!("expected AlreadyHeld, got {other}"),
            Ok(_) => panic!("second acquire must fail while the first guard is held"),
        }
    }

    #[test]
    fn release_then_acquire_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let name = unique_mutex_name("release");
        let path = lock_path(&dir, "x.lock");

        drop(try_acquire(&name, &path, "a").expect("first acquire ok"));
        let _again = try_acquire(&name, &path, "b").expect("reacquire after drop");
    }

    #[test]
    fn different_identities_do_not_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let _a = try_acquire(&unique_mutex_name("a"), &lock_path(&dir, "a.lock"), "a")
            .expect("acquire a ok");
        let _b = try_acquire(&unique_mutex_name("b"), &lock_path(&dir, "b.lock"), "b")
            .expect("acquire b ok");
    }

    #[test]
    fn owner_diagnostics_file_is_written_with_the_documented_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = lock_path(&dir, "x.lock");
        let guard = try_acquire(&unique_mutex_name("diag"), &path, "svc").unwrap();
        assert_eq!(guard.lock_file_path(), path.as_path());

        let raw = std::fs::read_to_string(&path).unwrap();
        let json: serde_json::Value = serde_json::from_str(&raw).unwrap();
        // 既存の banto-hub `profile.lock` と同じ 3 キー（互換のため固定）。
        assert_eq!(json["pid"], std::process::id());
        assert_eq!(json["host_kind"], "svc");
        assert!(json["acquired_at_unix_ms"].is_i64());
    }

    /// 非 Windows は診断ファイル（= flock の対象）を開けず `Io`。
    #[cfg(not(windows))]
    #[test]
    fn missing_parent_directory_is_an_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("no-such-dir").join("x.lock");
        assert!(matches!(
            try_acquire(&unique_mutex_name("io"), &path, "a"),
            Err(InstanceLockError::Io(_))
        ));
    }

    /// Windows は mutex が取れ、診断ファイルだけ書けない（致命的にしない）。
    #[cfg(windows)]
    #[test]
    fn missing_parent_directory_does_not_fail_the_mutex_on_windows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("no-such-dir").join("x.lock");
        let _guard = try_acquire(&unique_mutex_name("io"), &path, "a").expect("mutex ok");
    }

    #[test]
    fn path_scope_id_is_stable_across_spellings_and_distinct_per_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();
        let db = dir.path().join("a.sqlite3");
        let via_dots = dir.path().join("sub").join("..").join("a.sqlite3");

        let (id1, canon1) = path_scope_id(&db).unwrap();
        let (id2, canon2) = path_scope_id(&via_dots).unwrap();
        assert_eq!(id1, id2);
        assert_eq!(canon1, canon2);
        assert_eq!(id1.len(), 16);

        let (id3, _) = path_scope_id(&dir.path().join("b.sqlite3")).unwrap();
        assert_ne!(id1, id3);
        // 親ディレクトリが違えば、同名ファイルでも別 ID。
        let (id4, _) = path_scope_id(&dir.path().join("sub").join("a.sqlite3")).unwrap();
        assert_ne!(id1, id4);
    }

    #[test]
    fn path_scope_id_creates_a_missing_parent() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("fresh").join("db.sqlite3");
        path_scope_id(&target).unwrap();
        assert!(dir.path().join("fresh").is_dir());
    }
}
