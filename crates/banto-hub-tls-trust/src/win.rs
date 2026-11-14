//! [`TlsTrustOps`] の Windows の実装（本番）。部品はすべて `banto_safe_root`（unsafe な Win32 を
//! この crate に複製しない）: 辿らずに開くのは `acl::PinnedDir`。フォルダは、banto-hub-core の
//! `prepare-hub-tls` が削除・付け替えに `ManagedDir` の口を使うため、[`ManagedDir::from_pinned`]
//! で包んで持つ（[`WinTlsDir`]。中身は常に辿らずに握ったハンドル）。
//!
//! フォルダの開き方は 2 つ。クライアント（[`WindowsTlsTrustOps`]）は**読み取り専用**の pin
//! （[`pin_dir_read_only`]・[`open_child_dir_read_only`]。`WRITE_DAC | WRITE_OWNER` を求めない）で
//! 開く: 一般利用者（昇格なし）のデスクトップでも動くため（ルート・`tls\` に `WRITE_DAC` を持た
//! ない）。Hub（banto-hub-core の `WindowsHubTlsOps`。SYSTEM・昇格した管理者）は今までどおり
//! DACL を書ける pin（[`pin_dir_for_hub`]・[`open_child_dir_for_hub`]）で開く。

use std::path::{Path, PathBuf};

use banto_safe_root::acl::{FileSecurity, PinnedDir, PinnedDirInfo, PinnedFile};
use banto_safe_root::guard::win::RegistryMachineEnvironment;
use banto_safe_root::{ManagedDir, SafeRootError};

use crate::text::secret_error_text;
use crate::{FileOpenError, ReadError, TlsDir, TlsFile, TlsTrustOps};

/// 本番の読み取りの操作（クライアント用。フォルダは読み取り専用の pin で開くので、一般利用者
/// でも動く）。サービスの Hub は banto-hub-core の `WindowsHubTlsOps` が同じ部品で同じことを
/// する（フォルダは DACL を書ける pin）。
pub struct WindowsTlsTrustOps;

/// 辿らずに開いたフォルダ。
pub struct WinTlsDir(ManagedDir);

impl WinTlsDir {
    /// 辿らずに開いたフォルダを包む。
    pub fn new(pinned: PinnedDir) -> Self {
        Self(ManagedDir::from_pinned(pinned))
    }

    /// 握ったハンドル。
    pub fn pinned(&self) -> &PinnedDir {
        // `from_pinned` で作るので、いつも握ったハンドルがある。
        self.0
            .pinned()
            .expect("WinTlsDir は辿らずに開いたフォルダだけを包む")
    }

    /// 削除・付け替えの口（banto-hub-core の `prepare-hub-tls` 用）。
    pub fn managed(&self) -> &ManagedDir {
        &self.0
    }
}

impl TlsDir for WinTlsDir {
    fn info(&self) -> PinnedDirInfo {
        self.pinned().info().clone()
    }

    fn final_path(&self) -> PathBuf {
        self.pinned().final_path().to_path_buf()
    }

    fn security(&self) -> Result<FileSecurity, String> {
        self.pinned()
            .security()
            .map_err(|err| secret_error_text(&err))
    }
}

impl TlsFile for PinnedFile {
    fn security(&self) -> FileSecurity {
        PinnedFile::security(self).clone()
    }

    fn final_path(&self) -> PathBuf {
        PinnedFile::final_path(self).to_path_buf()
    }

    fn read_all(&mut self, max_len: usize) -> Result<Vec<u8>, ReadError> {
        self.read_to_end(max_len).map_err(|err| match err {
            banto_safe_root::SafeRootError::Io {
                kind: std::io::ErrorKind::FileTooLarge,
                ..
            } => ReadError::TooLarge,
            other => ReadError::Other(secret_error_text(&other)),
        })
    }
}

/// `ERROR_SHARING_VIOLATION`。
const ERROR_SHARING_VIOLATION: i32 = 32;

/// Hub 用: DACL・所有者を書ける権利（`banto_safe_root::acl::pin_dir`）で辿らずに開く（無ければ
/// `Ok(None)`）。一般利用者では開けない（ルートに `WRITE_DAC` を持たない）ので、クライアントは
/// [`pin_dir_read_only`] を使う。
pub fn pin_dir_for_hub(dir: &Path) -> Result<Option<WinTlsDir>, String> {
    banto_safe_root::acl::pin_dir(dir)
        .map(|found| found.map(WinTlsDir::new))
        .map_err(|err| err.to_string())
}

/// Hub 用: `parent` のハンドル基準に、名前 1 要素の子のフォルダを DACL・所有者を書ける権利で
/// 辿らずに開く。
pub fn open_child_dir_for_hub(parent: &WinTlsDir, name: &str) -> Result<Option<WinTlsDir>, String> {
    parent
        .pinned()
        .open_child_dir(name)
        .map(|found| found.map(WinTlsDir::new))
        .map_err(|err| err.to_string())
}

/// クライアント用: 読み取り専用の pin（`banto_safe_root::acl::pin_dir_read_only`）で辿らずに
/// 開く（無ければ `Ok(None)`）。持っている間は名前の変更・削除を止める。
pub fn pin_dir_read_only(dir: &Path) -> Result<Option<WinTlsDir>, String> {
    banto_safe_root::acl::pin_dir_read_only(dir)
        .map(|found| found.map(WinTlsDir::new))
        .map_err(|err| err.to_string())
}

/// クライアント用: `parent` のハンドル基準に、名前 1 要素の子のフォルダを読み取り専用の pin で
/// 辿らずに開く。
pub fn open_child_dir_read_only(
    parent: &WinTlsDir,
    name: &str,
) -> Result<Option<WinTlsDir>, String> {
    parent
        .pinned()
        .open_child_dir_read_only(name)
        .map(|found| found.map(WinTlsDir::new))
        .map_err(|err| err.to_string())
}

/// `parent` のハンドル基準に、名前 1 要素の子を読み取りで辿らずに開く（共有は読み取りだけ）。
pub fn open_child_file(
    parent: &WinTlsDir,
    name: &str,
) -> Result<Option<PinnedFile>, FileOpenError> {
    parent
        .pinned()
        .open_child_file(name)
        .map_err(|err| match err {
            SafeRootError::Io {
                os_error: Some(ERROR_SHARING_VIOLATION),
                ..
            } => FileOpenError::InUse,
            other => FileOpenError::Other(secret_error_text(&other)),
        })
}

impl TlsTrustOps for WindowsTlsTrustOps {
    type Env = RegistryMachineEnvironment;
    type Dir = WinTlsDir;
    type File = PinnedFile;

    fn machine_env(&self) -> &RegistryMachineEnvironment {
        &RegistryMachineEnvironment
    }

    fn pin_dir(&self, dir: &Path) -> Result<Option<WinTlsDir>, String> {
        pin_dir_read_only(dir)
    }

    fn open_child_dir(&self, parent: &WinTlsDir, name: &str) -> Result<Option<WinTlsDir>, String> {
        open_child_dir_read_only(parent, name)
    }

    fn open_child_file(
        &self,
        parent: &WinTlsDir,
        name: &str,
    ) -> Result<Option<PinnedFile>, FileOpenError> {
        open_child_file(parent, name)
    }
}

/// 利用者ごとの置き場（#1121 PR1a）の読み取り。部品は `banto_safe_root` のもの:
/// `%LOCALAPPDATA%` は既知のフォルダ（`guard::win::local_app_data`）、SID はプロセスのトークン
/// （`acl::current_user_sid`）、ドライブの種類は `guard::win::drive_kind`。フォルダは読み取り
/// 専用の pin で開く（[`WindowsTlsTrustOps`] のまま）。
impl crate::UserTlsOps for WindowsTlsTrustOps {
    fn local_app_data(&self) -> Result<PathBuf, String> {
        banto_safe_root::guard::win::local_app_data()
    }

    fn current_user_sid(&self) -> Result<String, String> {
        banto_safe_root::acl::current_user_sid()
            .map(|sid| sid.as_str().to_string())
            .map_err(|err| err.to_string())
    }

    fn drive_kind(&self, real: &Path) -> Result<banto_safe_root::guard::DriveKind, String> {
        banto_safe_root::guard::win::drive_kind(real)
    }
}

/// 置き換えを、宛先が読み取りで開かれている間の失敗（共有違反・アクセス拒否）でやり直す回数と
/// 間隔。ロック無しの照合（[`crate::read_user_revocations`]）は記録を一瞬だけ読み取りで握り、
/// その間の置き換えは OS が断る。握る時間は短いので、少し待てば通る。
const REPLACE_TRIES: u32 = 5;
const REPLACE_RETRY_WAIT: std::time::Duration = std::time::Duration::from_millis(20);
/// `ERROR_ACCESS_DENIED`。
const ERROR_ACCESS_DENIED: i32 = 5;

/// 一時ファイルの名前か（`banto_safe_root::root::temp_name` の形: `.{name}.tmp-{16 進 8 桁}`）。
fn is_temp_name_of(candidate: &str, name: &str) -> bool {
    candidate
        .strip_prefix('.')
        .and_then(|rest| rest.strip_prefix(name))
        .and_then(|rest| rest.strip_prefix(".tmp-"))
        .is_some_and(|nonce| nonce.len() == 8 && nonce.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// `dir` の中の `name` を原子的に置き換える（`ManagedDir::write_atomic`。宛先が読み取りで開かれて
/// いる間の失敗は `REPLACE_TRIES` 回までやり直す）。利用者の記録（[`crate::RevocationOps`]）と
/// サービスの記録（banto-hub-core の [`crate::ServiceRevocationOps`] の実装）が共有する。
pub fn write_file_atomic(dir: &WinTlsDir, name: &str, contents: &[u8]) -> Result<(), String> {
    let mut tries = 0;
    loop {
        tries += 1;
        match dir.managed().write_atomic(name, contents) {
            Ok(()) => return Ok(()),
            Err(SafeRootError::Io {
                os_error: Some(ERROR_ACCESS_DENIED | ERROR_SHARING_VIOLATION),
                ..
            }) if tries < REPLACE_TRIES => std::thread::sleep(REPLACE_RETRY_WAIT),
            Err(err) => return Err(err.to_string()),
        }
    }
}

/// `dir` の中に残った、`name` の一時ファイル（`.{name}.tmp-{16 進 8 桁}`）を読まずに消す。
pub fn remove_stale_temps(dir: &WinTlsDir, name: &str) -> Result<(), String> {
    let report = dir
        .managed()
        .remove_where(&|candidate| is_temp_name_of(candidate, name))
        .map_err(|err| err.to_string())?;
    match report.failed.first() {
        Some((left, err)) => Err(format!("{left}: {err}")),
        None => Ok(()),
    }
}

/// 一時ファイルの nonce の最初の値（プロセスと時刻から。衝突したら進める）。
fn temp_nonce_seed() -> u32 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.subsec_nanos())
        .unwrap_or(0);
    nanos ^ std::process::id().rotate_left(16)
}

/// サービスの失効の記録（#1121 PR2b）: `dir` の中に `name` が無ければ作る。同じフォルダの一時
/// ファイル（`.{name}.tmp-{16 進 8 桁}`）を `PinnedDir::create_child_file_users_read`（SYSTEM・
/// Administrators にフル、Users に読み取りと実行、所有者 Administrators を作るのと同時に付け、
/// 書いて書き出し、読み戻して確かめる）で作り、`ManagedDir::rename_no_replace` で**置き換えず
/// に**付け替える。宛先が既にあれば（`AlreadyExists`）一時ファイルを消して `Ok(false)`。
pub fn create_file_no_replace_users_read(
    dir: &WinTlsDir,
    name: &str,
    contents: &[u8],
) -> Result<bool, String> {
    let managed = dir.managed();
    let seed = temp_nonce_seed();
    let mut made = None;
    for step in 0..16u32 {
        let temp = banto_safe_root::root::temp_name(name, seed.wrapping_add(step))
            .ok_or_else(|| format!("{name} の一時ファイルの名前を作れません"))?;
        match dir.pinned().create_child_file_users_read(&temp, contents) {
            Ok(Some(file)) => {
                // 付け替えは名前で開き直して行う（作ったハンドルは先に閉じる）。
                drop(file);
                made = Some(temp);
                break;
            }
            Ok(None) => continue,
            Err(err) => return Err(err.to_string()),
        }
    }
    let Some(temp) = made else {
        return Err(format!(
            "{name} の一時ファイルを作れませんでした（名前が衝突し続けます）"
        ));
    };
    match managed.rename_no_replace(&temp, name) {
        Ok(true) => Ok(true),
        Ok(false) => Err(format!("{temp} を付け替えようとしたら無くなっていました")),
        Err(err) => {
            let _ = managed.remove(&temp);
            match err {
                SafeRootError::Io {
                    kind: std::io::ErrorKind::AlreadyExists,
                    ..
                } => Ok(false),
                other => Err(other.to_string()),
            }
        }
    }
}

/// サービスの失効の記録（#1121 PR2b）: `dir` の中の `name` を原子的に置き換える
/// （`ManagedDir::write_atomic_users_read`。置き換えた後のファイルは、SYSTEM・Administrators に
/// フル、Users に読み取りと実行、所有者 Administrators）。宛先が読み取りで開かれている間の失敗は
/// `REPLACE_TRIES` 回までやり直す（[`write_file_atomic`] と同じ）。
pub fn write_file_atomic_users_read(
    dir: &WinTlsDir,
    name: &str,
    contents: &[u8],
) -> Result<(), String> {
    let mut tries = 0;
    loop {
        tries += 1;
        match dir.managed().write_atomic_users_read(name, contents) {
            Ok(()) => return Ok(()),
            Err(SafeRootError::Io {
                os_error: Some(ERROR_ACCESS_DENIED | ERROR_SHARING_VIOLATION),
                ..
            }) if tries < REPLACE_TRIES => std::thread::sleep(REPLACE_RETRY_WAIT),
            Err(err) => return Err(err.to_string()),
        }
    }
}

/// Hub 用: `parent` の直下に、SYSTEM・Administrators にフル、`BUILTIN\Users` に読み取りと実行、
/// 所有者 Administrators、保護された DACL（`OICI`）のフォルダを作るのと同時に付けて作る
/// （`PinnedDir::create_child_dir_users_read`。`tls\` とサービスの失効の記録の置き場。既にあれば
/// `Ok(None)`）。
pub fn create_child_dir_users_read(
    parent: &WinTlsDir,
    name: &str,
) -> Result<Option<WinTlsDir>, String> {
    parent
        .pinned()
        .create_child_dir_users_read(name)
        .map(|found| found.map(WinTlsDir::new))
        .map_err(|err| err.to_string())
}

/// 失効の記録の書き込み（#1121 PR1b）。部品はすべて `banto_safe_root` で、どれも握った
/// フォルダのハンドル基準に、名前 1 要素で辿らずに行う:
///
/// - フォルダを作る: `PinnedDir::create_child_dir`（`NtCreateFile` の相対の `FILE_CREATE`。
///   記述子は親から継承）。
/// - 置き換え: `ManagedDir::write_atomic`（同じフォルダの一時ファイル `FILE_CREATE` → 書く →
///   `FlushFileBuffers` → 一時ファイルのハンドルで `FileRenameInformationEx`）。
/// - 残った一時ファイル: `ManagedDir::remove_where`（開いた後のハンドルで種類を確かめて消す）。
/// - ロックファイルを作る: `ManagedDir::create_new`（`FILE_CREATE`）。
/// - 排他の open: `PinnedDir::open_child_file_exclusive`（読み取りだけの権利・**共有なし**）。
///   同じ口の 2 つ目の open は共有違反（`ERROR_SHARING_VIOLATION`）で失敗する。中身は読まない・
///   書かない・消さない（握るだけ）。プロセスが落ちれば OS がハンドルを閉じるので、ロックは
///   残らない。
///
/// フォルダの pin は読み取り専用（[`WindowsTlsTrustOps`]）のまま: 子を作る・置き換える権利は、
/// 握ったハンドルの権利ではなく、フォルダの DACL と今のトークンで OS が確かめる。
impl crate::RevocationOps for WindowsTlsTrustOps {
    fn create_child_dir(&self, parent: &WinTlsDir, name: &str) -> Result<(), String> {
        parent
            .pinned()
            .create_child_dir(name, None)
            .map(|_| ())
            .map_err(|err| err.to_string())
    }

    fn write_atomic(&self, dir: &WinTlsDir, name: &str, contents: &[u8]) -> Result<(), String> {
        write_file_atomic(dir, name, contents)
    }

    fn remove_stale_temps(&self, dir: &WinTlsDir, name: &str) -> Result<(), String> {
        remove_stale_temps(dir, name)
    }

    fn create_file_if_absent(&self, dir: &WinTlsDir, name: &str) -> Result<(), String> {
        dir.managed()
            .create_new(name)
            .map(|_| ())
            .map_err(|err| err.to_string())
    }

    fn open_child_file_exclusive(
        &self,
        dir: &WinTlsDir,
        name: &str,
    ) -> Result<Option<PinnedFile>, FileOpenError> {
        dir.pinned()
            .open_child_file_exclusive(name)
            .map_err(|err| match err {
                SafeRootError::Io {
                    os_error: Some(ERROR_SHARING_VIOLATION),
                    ..
                } => FileOpenError::InUse,
                other => FileOpenError::Other(secret_error_text(&other)),
            })
    }

    fn sleep(&self, duration: std::time::Duration) {
        std::thread::sleep(duration);
    }

    fn now_unix_seconds(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs())
            .unwrap_or(0)
    }
}
