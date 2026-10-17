//! [`FileSecretStore`]: 1 アカウント 1 ファイルの秘密の保存先。

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use banto_safe_root::DirAccess;

use crate::error::{CorruptKind, InvalidAccount, IoOp, Result, SecretStoreError};
use crate::format::{self, MAX_ACCOUNT_LEN, MAX_FILE_LEN, MAX_SECRET_LEN};
use crate::protector::Protector;

/// entropy の前置き（版 1）。
const ENTROPY_PREFIX: &str = "banto-secret-store/v1:";
/// 秘密のファイルの拡張子。
const FILE_EXTENSION: &str = "bin";
/// 一時ファイルの名前の衝突でやり直す回数。
const TEMP_ATTEMPTS: usize = 16;

/// `account` のファイルに使う entropy（`"banto-secret-store/v1:" + account`）。
///
/// ファイルを別のアカウントの名前に入れ替えても（中のアカウントの欄まで
/// 書き換えても）、守ったときと違う entropy になるので復号できない。
pub fn entropy_for(account: &str) -> Vec<u8> {
    let mut entropy = Vec::with_capacity(ENTROPY_PREFIX.len() + account.len());
    entropy.extend_from_slice(ENTROPY_PREFIX.as_bytes());
    entropy.extend_from_slice(account.as_bytes());
    entropy
}

/// `account` のファイル名（`sha256(account)` の小文字 hex + `.bin`）。
pub fn file_name_for(account: &str) -> String {
    format!(
        "{}.{FILE_EXTENSION}",
        hex::encode(Sha256::digest(account.as_bytes()))
    )
}

fn validate_account(account: &str) -> Result<()> {
    if account.is_empty() {
        return Err(SecretStoreError::InvalidAccount(InvalidAccount::Empty));
    }
    if account.len() > MAX_ACCOUNT_LEN {
        return Err(SecretStoreError::InvalidAccount(InvalidAccount::TooLong));
    }
    Ok(())
}

/// ディレクトリの ACL をどう扱うか。
#[derive(Clone, Debug, Eq, PartialEq)]
enum DirMode {
    /// 開く・読む・書く・消すたびに `banto_safe_root::acl` の形を確かめる（製品）。
    Enforced(DirAccess),
    /// 確かめない（`test-support` / この crate のテストだけ）。
    #[cfg(any(test, feature = "test-support"))]
    Unchecked,
}

/// 書き込みの段階（テストで途中の失敗を注入する位置）。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WriteStage {
    /// 一時ファイルを作った直後（中身は空）。
    TempCreated,
    /// 一時ファイルに書いて閉じた後、置き換える前。
    BeforeReplace,
}

/// 1 アカウント 1 ファイルの秘密の保存先（crate の doc）。
///
/// プロセスの中の読み書きは `Mutex` で 1 つずつにする。別のプロセスとの間の
/// 直列化はしない（同じ保存先を使うエンジンは A1 の排他で 1 つだけ動く前提。
/// それでも重なったときは、置き換えが原子的なので「どちらかの値が丸ごと残る」）。
pub struct FileSecretStore<P: Protector> {
    dir: PathBuf,
    mode: DirMode,
    protector: P,
    lock: Mutex<()>,
    #[cfg(test)]
    fail_at: Mutex<Option<WriteStage>>,
}

impl<P: Protector> fmt::Debug for FileSecretStore<P> {
    /// 秘密は持たないが、protector の中身も出さない（型名だけ）。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileSecretStore")
            .field("dir", &self.dir)
            .field("mode", &self.mode)
            .field("protector", &std::any::type_name::<P>())
            .finish_non_exhaustive()
    }
}

impl<P: Protector> FileSecretStore<P> {
    /// `dir` を保存先にする。無ければ `access` の DACL で作り、あれば DACL を
    /// 確かめる（違えば [`banto_safe_root::SafeRootError::AclMismatch`]。`banto_safe_root::acl` の doc）。
    /// どちらの経路でも、先にインストール利用者の SID が利用者のアカウントで
    /// あることを確かめる（グループ・解決できない SID は
    /// [`banto_safe_root::SafeRootError::InvalidUserSid`]）。SID の検証はここでの 1 回だけ。
    /// 非 Windows では [`banto_safe_root::SafeRootError::UnsupportedPlatform`]。
    pub fn open(dir: impl Into<PathBuf>, access: DirAccess, protector: P) -> Result<Self> {
        let dir = dir.into();
        banto_safe_root::acl::ensure_dir(&dir, &access)?;
        Ok(Self::with_mode(dir, DirMode::Enforced(access), protector))
    }

    /// テスト専用: ディレクトリの ACL を確かめずに開く（無ければ普通に作る）。
    /// 全 OS で使える。
    #[cfg(any(test, feature = "test-support"))]
    pub fn open_without_acl(dir: impl Into<PathBuf>, protector: P) -> Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(&dir).map_err(|e| SecretStoreError::io(IoOp::CreateDir, &e))?;
        Ok(Self::with_mode(dir, DirMode::Unchecked, protector))
    }

    fn with_mode(dir: PathBuf, mode: DirMode, protector: P) -> Self {
        Self {
            dir,
            mode,
            protector,
            lock: Mutex::new(()),
            #[cfg(test)]
            fail_at: Mutex::new(None),
        }
    }

    /// 保存先のディレクトリ。
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// `account` のファイルのパス。
    pub fn path_for(&self, account: &str) -> PathBuf {
        self.dir.join(file_name_for(account))
    }

    fn guard(&self) -> std::sync::MutexGuard<'_, ()> {
        // 守っているのは `()` だけなので、他のスレッドの panic の後も使い続けてよい。
        self.lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 読む・消す前: ディレクトリが無ければ `false`（作らない）、あれば確かめる。
    fn check_existing_dir(&self) -> Result<bool> {
        match fs::symlink_metadata(&self.dir) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(SecretStoreError::io(IoOp::InspectDir, &e)),
            Ok(_) => {}
        }
        match &self.mode {
            DirMode::Enforced(access) => {
                // SID は open で確かめ済み（banto_safe_root::acl の doc「インストール利用者の
                // SID の検証」）。ここでは ACL の形だけを確かめ直す。
                banto_safe_root::acl::verify_dir_with_validated_sid(&self.dir, access)?
            }
            #[cfg(any(test, feature = "test-support"))]
            DirMode::Unchecked => {}
        }
        Ok(true)
    }

    /// 書く前: 無ければ作り（期待の DACL で）、あれば確かめる。
    fn ensure_dir_for_write(&self) -> Result<()> {
        match &self.mode {
            DirMode::Enforced(access) => {
                banto_safe_root::acl::ensure_dir_with_validated_sid(&self.dir, access)
                    .map_err(SecretStoreError::from)
            }
            #[cfg(any(test, feature = "test-support"))]
            DirMode::Unchecked => {
                fs::create_dir_all(&self.dir).map_err(|e| SecretStoreError::io(IoOp::CreateDir, &e))
            }
        }
    }

    /// `account` の秘密。無ければ `Ok(None)`。
    ///
    /// ファイルの中のアカウントが `account` と違えば [`SecretStoreError::AccountMismatch`]、
    /// 復号できなければ [`SecretStoreError::Unprotect`]。
    pub fn load(&self, account: &str) -> Result<Option<Zeroizing<String>>> {
        validate_account(account)?;
        let _guard = self.guard();
        if !self.check_existing_dir()? {
            return Ok(None);
        }
        let file = match File::open(self.path_for(account)) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(SecretStoreError::io(IoOp::ReadFile, &e)),
        };
        let mut bytes = Vec::new();
        file.take(MAX_FILE_LEN as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| SecretStoreError::io(IoOp::ReadFile, &e))?;
        let decoded = format::decode(&bytes).map_err(SecretStoreError::Corrupt)?;
        if decoded.account != account {
            return Err(SecretStoreError::AccountMismatch);
        }
        let plain = self
            .protector
            .unprotect(decoded.blob, &entropy_for(account))?;
        let text = std::str::from_utf8(&plain)
            .map_err(|_| SecretStoreError::Corrupt(CorruptKind::SecretNotUtf8))?;
        Ok(Some(Zeroizing::new(text.to_owned())))
    }

    /// `account` の秘密を `secret` にする（あれば置き換える）。
    ///
    /// 同じディレクトリに一時ファイルを作って書き、ディスクへ書き出してから
    /// 本体を原子的に置き換える。どこで失敗しても（取消しでも）一時ファイルは
    /// 残さず、元のファイルはそのまま残る。
    pub fn store(&self, account: &str, secret: &str) -> Result<()> {
        validate_account(account)?;
        if secret.len() > MAX_SECRET_LEN {
            return Err(SecretStoreError::SecretTooLarge);
        }
        let blob = self
            .protector
            .protect(secret.as_bytes(), &entropy_for(account))?;
        if blob.len() > format::MAX_BLOB_LEN {
            return Err(SecretStoreError::SecretTooLarge);
        }
        let bytes = format::encode(account, &blob);
        let _guard = self.guard();
        self.ensure_dir_for_write()?;
        let final_name = file_name_for(account);
        write_atomic(&self.dir, &final_name, &bytes, |stage| self.inject(stage))
    }

    /// `account` の秘密を消す。無ければ `Ok(())`。
    pub fn remove(&self, account: &str) -> Result<()> {
        validate_account(account)?;
        let _guard = self.guard();
        if !self.check_existing_dir()? {
            return Ok(());
        }
        match fs::remove_file(self.path_for(account)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(SecretStoreError::io(IoOp::RemoveFile, &e)),
        }
    }

    #[cfg(test)]
    fn inject(&self, stage: WriteStage) -> io::Result<()> {
        if *self.fail_at.lock().unwrap() == Some(stage) {
            return Err(io::Error::other("injected"));
        }
        Ok(())
    }

    #[cfg(not(test))]
    fn inject(&self, _stage: WriteStage) -> io::Result<()> {
        Ok(())
    }

    /// テスト専用: 次からの書き込みを `stage` で失敗させる（`None` で戻す）。
    #[cfg(test)]
    pub(crate) fn fail_writes_at(&self, stage: Option<WriteStage>) {
        *self.fail_at.lock().unwrap() = stage;
    }
}

/// 一時ファイルの名前の nonce（同じプロセスの中で重ならない値 + 時刻）。
fn next_nonce() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    nanos.rotate_left(17) ^ count
}

/// 自分が作った一時ファイル。`commit` しないまま drop されたら消す
/// （失敗・panic・取消しのどの経路でも残さない）。
struct TempFile {
    path: Option<PathBuf>,
}

impl TempFile {
    fn commit(mut self) {
        self.path = None;
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = fs::remove_file(path);
        }
    }
}

/// `dir/final_name` を `bytes` で原子的に置き換える。`hook` は各段階で呼ばれ、
/// `Err` を返すとそこで失敗する（テストの注入用。製品では常に `Ok`）。
pub(crate) fn write_atomic(
    dir: &Path,
    final_name: &str,
    bytes: &[u8],
    hook: impl Fn(WriteStage) -> io::Result<()>,
) -> Result<()> {
    let target = dir.join(final_name);
    for _ in 0..TEMP_ATTEMPTS {
        let temp_path = dir.join(format!(
            ".{final_name}.{:x}-{:016x}.tmp",
            std::process::id(),
            next_nonce()
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = match options.open(&temp_path) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(SecretStoreError::io(IoOp::CreateTemp, &e)),
        };
        let guard = TempFile {
            path: Some(temp_path.clone()),
        };
        // 書く・書き出すの結果を持ったまま、**閉じてから** guard に任せる
        // （閉じる前に guard が消そうとすると、Windows では共有モード次第で
        // 消えずに残りうる）。
        let written = hook(WriteStage::TempCreated)
            .map_err(|e| SecretStoreError::io(IoOp::WriteTemp, &e))
            .and_then(|()| {
                file.write_all(bytes)
                    .map_err(|e| SecretStoreError::io(IoOp::WriteTemp, &e))
            })
            .and_then(|()| {
                file.sync_all()
                    .map_err(|e| SecretStoreError::io(IoOp::SyncTemp, &e))
            });
        drop(file);
        written?;
        hook(WriteStage::BeforeReplace).map_err(|e| SecretStoreError::io(IoOp::Replace, &e))?;
        replace(&temp_path, &target).map_err(|e| SecretStoreError::io(IoOp::Replace, &e))?;
        guard.commit();
        return Ok(());
    }
    Err(SecretStoreError::io(
        IoOp::CreateTemp,
        &io::Error::from(io::ErrorKind::AlreadyExists),
    ))
}

#[cfg(windows)]
fn replace(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };
    let wide = |p: &Path| -> Vec<u16> {
        p.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    };
    let (from, to) = (wide(from), wide(to));
    // SAFETY: どちらも NUL 終端の UTF-16。
    let ok = unsafe {
        MoveFileExW(
            from.as_ptr(),
            to.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(windows))]
fn replace(from: &Path, to: &Path) -> io::Result<()> {
    fs::rename(from, to)?;
    // 置き換えをディレクトリの項目としても書き出す（失敗しても置き換えは済んでいる）。
    if let Some(parent) = to.parent() {
        if let Ok(dir) = File::open(parent) {
            let _ = dir.sync_all();
        }
    }
    Ok(())
}
