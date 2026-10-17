//! 診断ファイルを**リンクを辿らずに**開く（#623 PR-2）。
//!
//! 診断ファイルはデータのルート（ChronoGazer の `<root>`、banto-hub の
//! `profiles\<id>`）に置かれ、そこにはインストール利用者が「変更」を持つ。
//! LocalSystem のサービスがパスのまま `create + truncate` で開くと、利用者が事前に
//! 置いたシンボリックリンク・ハードリンク・途中のジャンクションを辿り、**外の任意の
//! ファイルを 0 バイトに切り詰めて書く**（失敗は握りつぶすので気付かれない）。
//! ここでは次の順で扱い、確かめに合わないものは書かず・読まずに捨てる:
//!
//! - **Windows**: `FILE_FLAG_OPEN_REPARSE_POINT` で開く（最後の要素がリンクなら、
//!   リンクそのものが開く）。新規は `CREATE_NEW`、既にあれば `OPEN_EXISTING` で
//!   開き直す（`OPEN_ALWAYS`/`CREATE_ALWAYS` のように開く時点で切り詰めない）。
//!   開いた**ハンドル**で、reparse point でない・ディレクトリでない・リンク数が 1
//!   （ハードリンクの別名が無い）・本当のパス（`GetFinalPathNameByHandleW`）が渡された
//!   パス（8.3 の短い名前は長い名前に直して比べる）と同じ、を確かめる。最後の確かめで、
//!   途中の要素がジャンクション・シンボリックリンクにされていて外へ出た場合を捕まえる。
//!   確かめた後にだけ `set_len(0)` して書く（ハンドルが指す実体は確かめた後に
//!   すり替えられない）。この呼び出しで作ったファイルが確かめに合わなければ、
//!   ハンドルで消す（外に空のファイルを残さない）。
//! - **非 Windows**: `O_NOFOLLOW`（最後の要素のシンボリックリンクを断る）と、開いた
//!   fd の `fstat` で「通常のファイル・リンク数 1」を確かめる。非 Windows は開発・CI
//!   用なので、途中のディレクトリのリンクまでは見ない。読みは `O_NONBLOCK` も付ける
//!   （FIFO だと `open(2)` が書き手を待って戻らないため。開いた後は通常のファイルで
//!   あることを確かめるので、読みそのものは通常のファイルとして終わる）。
//!
//! 渡されたパスの途中にジャンクション・マウントポイント・`subst`・ネットワーク
//! ドライブがあると、本当のパスが渡されたパスと食い違うので診断ファイルは書かれず、
//! 読みは `Corrupt` になる（排他そのものは Windows では mutex なので影響しない）。
//! そのため呼び出し側は、**管理者が決める部分（ルート）だけ**を `canonicalize`
//! してから利用者が「変更」を持つ部分を連結して渡す（banto-hub は Hub のルート、
//! ChronoGazer は DB の親 = ルート）。利用者が「変更」を持つ部分は解かないので、
//! そこに植えたリンクはここで捕まる。

use std::fmt;
use std::fs::File;
use std::path::PathBuf;

/// 開いたものが診断ファイルとして扱えない理由。
#[derive(Debug)]
pub(crate) enum Refusal {
    /// reparse point（シンボリックリンク・ジャンクション等）そのもの。
    ReparsePoint,
    /// 通常のファイルではない（ディレクトリ等）。
    NotARegularFile,
    /// ハードリンクの別名がある（リンク数が 1 でない）。
    HardLinked { links: u64 },
    /// 本当のパスが渡されたパスと違う（途中の要素がリンクで外へ出た）。
    /// 非 Windows は途中のリンクを見ないので作らない。
    #[cfg_attr(not(windows), allow(dead_code))]
    Elsewhere { actual: PathBuf },
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::ReparsePoint => {
                write!(f, "診断ファイルがリンク（reparse point）なので辿りません")
            }
            Refusal::NotARegularFile => write!(f, "診断ファイルが通常のファイルではありません"),
            Refusal::HardLinked { links } => write!(
                f,
                "診断ファイルにハードリンクの別名があります（リンク数 {links}）"
            ),
            Refusal::Elsewhere { actual } => write!(
                f,
                "診断ファイルの本当の場所が渡されたパスと違います（途中にリンク: {}）",
                actual.display()
            ),
        }
    }
}

/// 開いた結果。`Refused` のときハンドルは既に閉じている。
pub(crate) enum Opened {
    File(File),
    Refused(Refusal),
}

#[cfg(windows)]
pub(crate) use windows_impl::{open_for_read, open_for_write};

#[cfg(not(windows))]
pub(crate) use unix_impl::{open_for_lock, open_for_read};

#[cfg(windows)]
mod windows_impl {
    use super::{Opened, Refusal};
    use std::fs::File;
    use std::io;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
    use std::path::{Path, PathBuf};

    use windows_sys::Win32::Foundation::{
        ERROR_ALREADY_EXISTS, ERROR_FILE_EXISTS, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FileDispositionInfo, GetFileInformationByHandle, GetFinalPathNameByHandleW,
        GetLongPathNameW, SetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, CREATE_NEW,
        DELETE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_DISPOSITION_INFO, FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
        FILE_NAME_NORMALIZED, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
        FILE_SHARE_WRITE, OPEN_EXISTING, VOLUME_NAME_DOS,
    };

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    /// `FILE_FLAG_OPEN_REPARSE_POINT` で開く（最後の要素がリンクでも辿らない）。
    /// 共有は `std::fs::OpenOptions` の既定と同じ（読み・書き・削除）。
    fn open_raw(path: &Path, access: u32, disposition: u32) -> io::Result<OwnedHandle> {
        let wide = wide(path);
        // SAFETY: `wide` は NUL 終端で呼び出しの間生存する。属性・テンプレートは null。
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                access,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                disposition,
                FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `CreateFileW` が返した有効なハンドルで、所有をここで移す。
        Ok(unsafe { OwnedHandle::from_raw_handle(handle as RawHandle) })
    }

    fn raw(handle: &OwnedHandle) -> HANDLE {
        handle.as_raw_handle() as HANDLE
    }

    /// ハンドルが指すものの本当のパス（`\\?\C:\...` 形式）。
    fn final_path(handle: &OwnedHandle) -> io::Result<PathBuf> {
        let mut buf = vec![0u16; 512];
        loop {
            // SAFETY: `buf` は書ける長さ `buf.len()` の領域。
            let len = unsafe {
                GetFinalPathNameByHandleW(
                    raw(handle),
                    buf.as_mut_ptr(),
                    buf.len() as u32,
                    FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
                )
            } as usize;
            if len == 0 {
                return Err(io::Error::last_os_error());
            }
            if len < buf.len() {
                buf.truncate(len);
                return Ok(PathBuf::from(std::ffi::OsString::from_wide(&buf)));
            }
            // 足りないときは終端を含む必要な長さが返る。
            buf.resize(len + 1, 0);
        }
    }

    /// 渡されたパスを、比べられる形にする: 絶対パスにして、8.3 の短い名前を長い名前に
    /// 直す（`GetLongPathNameW` はリンクを解かない - 途中のジャンクションは名前のまま
    /// 残るので、本当のパスと食い違う）。直せなければ絶対パスのまま（比べて食い違えば
    /// 書かない側に倒れる）。
    fn expected_path(path: &Path) -> PathBuf {
        let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        let wide = wide(&absolute);
        let mut buf = vec![0u16; 512];
        loop {
            // SAFETY: `wide` は NUL 終端、`buf` は書ける長さ `buf.len()` の領域。
            let len = unsafe { GetLongPathNameW(wide.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) }
                as usize;
            if len == 0 {
                return absolute;
            }
            if len < buf.len() {
                buf.truncate(len);
                return PathBuf::from(std::ffi::OsString::from_wide(&buf));
            }
            buf.resize(len + 1, 0);
        }
    }

    /// `\\?\`・`\\?\UNC\` の接頭辞を落とし、末尾の区切りを除き、小文字にそろえる
    /// （NTFS は大文字小文字を区別しない）。
    pub(super) fn comparable(path: &Path) -> String {
        let text = path.to_string_lossy();
        let text = if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
            format!(r"\\{rest}")
        } else if let Some(rest) = text.strip_prefix(r"\\?\") {
            rest.to_string()
        } else {
            text.into_owned()
        };
        text.trim_end_matches('\\').to_lowercase()
    }

    /// 開いたハンドルを確かめる。合わなければ理由を返す。
    fn verify(handle: &OwnedHandle, path: &Path) -> io::Result<Option<Refusal>> {
        // SAFETY: ゼロ埋めは BY_HANDLE_FILE_INFORMATION の有効な値（整数だけの構造体）。
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: 有効なハンドルと、書ける構造体へのポインタ。
        if unsafe { GetFileInformationByHandle(raw(handle), &mut info) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Ok(Some(Refusal::ReparsePoint));
        }
        if info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
            return Ok(Some(Refusal::NotARegularFile));
        }
        if info.nNumberOfLinks != 1 {
            return Ok(Some(Refusal::HardLinked {
                links: u64::from(info.nNumberOfLinks),
            }));
        }
        let actual = final_path(handle)?;
        if comparable(&actual) != comparable(&expected_path(path)) {
            return Ok(Some(Refusal::Elsewhere { actual }));
        }
        Ok(None)
    }

    /// このハンドルで作ったファイルを、ハンドルで消す（パスを解決し直さない）。
    fn delete_by_handle(handle: &OwnedHandle) -> io::Result<()> {
        let info = FILE_DISPOSITION_INFO { DeleteFile: true };
        // SAFETY: 有効なハンドル（`DELETE` 付きで開いた）と、大きさが合う構造体。
        let ok = unsafe {
            SetFileInformationByHandle(
                raw(handle),
                FileDispositionInfo,
                (&info as *const FILE_DISPOSITION_INFO).cast(),
                std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// 書くために開く。中身は**切り詰めない**（確かめた後に呼ぶ側が `set_len(0)`）。
    pub(crate) fn open_for_write(path: &Path) -> io::Result<Opened> {
        const WRITE: u32 = FILE_GENERIC_WRITE | FILE_READ_ATTRIBUTES;
        let (handle, created) = match open_raw(path, WRITE | DELETE, CREATE_NEW) {
            Ok(handle) => (handle, true),
            Err(err)
                if err.raw_os_error() == Some(ERROR_FILE_EXISTS as i32)
                    || err.raw_os_error() == Some(ERROR_ALREADY_EXISTS as i32) =>
            {
                (open_raw(path, WRITE, OPEN_EXISTING)?, false)
            }
            Err(err) => return Err(err),
        };
        let refusal = match verify(&handle, path) {
            Ok(None) => return Ok(Opened::File(File::from(handle))),
            Ok(Some(refusal)) => refusal,
            Err(err) => {
                if created {
                    delete_created(&handle, path);
                }
                return Err(err);
            }
        };
        if created {
            // 作ったのが外（途中のリンクの先）なら、空のファイルを残さない。
            delete_created(&handle, path);
        }
        Ok(Opened::Refused(refusal))
    }

    /// この呼び出しで作ったファイルを消す。消せなければ、外に空のファイルが残った
    /// かもしれないことを本当のパスつきで警告 1 行に出す（黙って捨てない）。
    fn delete_created(handle: &OwnedHandle, path: &Path) {
        if let Err(err) = delete_by_handle(handle) {
            let actual = final_path(handle)
                .map(|actual| actual.display().to_string())
                .unwrap_or_else(|_| "(不明)".to_string());
            tracing::warn!(
                path = %path.display(),
                actual = %actual,
                "確かめに合わなかった診断ファイルを作ったが消せませんでした（空のファイルが残っている可能性があります）: {err}"
            );
        }
    }

    /// 読むために開く（既にあるものだけ）。
    pub(crate) fn open_for_read(path: &Path) -> io::Result<Opened> {
        let handle = open_raw(path, FILE_GENERIC_READ, OPEN_EXISTING)?;
        Ok(match verify(&handle, path)? {
            None => Opened::File(File::from(handle)),
            Some(refusal) => Opened::Refused(refusal),
        })
    }
}

#[cfg(not(windows))]
mod unix_impl {
    use super::{Opened, Refusal};
    use std::fs::{File, OpenOptions};
    use std::io;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    use std::path::Path;

    fn finish(opened: io::Result<File>) -> io::Result<Opened> {
        let file = match opened {
            Ok(file) => file,
            // O_NOFOLLOW で最後の要素がシンボリックリンクだった。
            Err(err) if err.raw_os_error() == Some(libc::ELOOP) => {
                return Ok(Opened::Refused(Refusal::ReparsePoint))
            }
            Err(err) => return Err(err),
        };
        let meta = file.metadata()?;
        if !meta.is_file() {
            return Ok(Opened::Refused(Refusal::NotARegularFile));
        }
        if meta.nlink() != 1 {
            return Ok(Opened::Refused(Refusal::HardLinked {
                links: meta.nlink(),
            }));
        }
        Ok(Opened::File(file))
    }

    /// flock の対象として開く（無ければ作る。切り詰めない）。
    pub(crate) fn open_for_lock(path: &Path) -> io::Result<Opened> {
        finish(
            OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(path),
        )
    }

    /// 読むために開く。`O_NONBLOCK` は FIFO で `open(2)` が書き手を待って戻らない
    /// のを防ぐ（`finish` が通常のファイルでないものを断るので、FIFO は読まずに
    /// 捨てる）。通常のファイルの `read(2)` は `O_NONBLOCK` でも待たずにそのまま
    /// 読めるので、読みの扱いは変わらない。
    pub(crate) fn open_for_read(path: &Path) -> io::Result<Opened> {
        finish(
            OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(path),
        )
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::windows_impl::comparable;
    use std::path::Path;

    #[test]
    fn comparable_drops_verbatim_prefixes_case_and_trailing_separators() {
        assert_eq!(
            comparable(Path::new(r"\\?\C:\Data\X.lock")),
            comparable(Path::new(r"c:\data\x.lock"))
        );
        assert_eq!(
            comparable(Path::new(r"\\?\UNC\srv\share\a")),
            comparable(Path::new(r"\\SRV\share\a\"))
        );
        assert_ne!(
            comparable(Path::new(r"C:\data\x.lock")),
            comparable(Path::new(r"C:\outside\x.lock"))
        );
    }
}
