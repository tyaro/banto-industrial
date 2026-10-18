//! ハンドル基準の I/O の Windows の下回り（#623 PR-1b）。
//!
//! [`crate::root`] の `ManagedDir` と [`crate::nofollow`] が使う、開いたハンドルだけを相手にする
//! 小さな部品。パスは一切受け取らない（名前の付け替えも、握ったフォルダのハンドルを
//! `RootDirectory` にした名前 1 要素だけ）。開く部分（`NtCreateFile` の相対・辿らない開き方）は [`crate::acl`] の
//! `child_relative` を共有し、ここには複製しない。

use std::path::PathBuf;

use windows_sys::Win32::Foundation::HANDLE;

use crate::acl::imp::{api, last_error, OwnedHandle};
use crate::error::{IoOp, Refusal, Result, SafeRootError};

/// 開いたハンドルから読んだ属性（[`facts`]）。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Facts {
    pub(crate) is_dir: bool,
    pub(crate) is_reparse: bool,
    /// reparse の tag（reparse point でなければ 0）。
    pub(crate) reparse_tag: u32,
    pub(crate) links: u32,
}

/// `handle`（`FILE_READ_ATTRIBUTES` を持つ）が指す物の属性・リンク数・reparse の tag を、
/// **ハンドルから**読む。tag は `FileAttributeTagInfo` で、reparse point のときだけ読む。
pub(crate) fn facts(handle: HANDLE) -> Result<Facts> {
    use windows_sys::Win32::Storage::FileSystem::{
        FileAttributeTagInfo, GetFileInformationByHandle, GetFileInformationByHandleEx,
        BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_ATTRIBUTE_TAG_INFO,
    };
    // SAFETY: 全フィールドが整数の構造体（ゼロは有効な値）。`handle` は有効。
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: `handle` は有効、`info` は書き込み先。
    if unsafe { GetFileInformationByHandle(handle, &mut info) } == 0 {
        return Err(api("GetFileInformationByHandle", last_error()));
    }
    let is_reparse = info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0;
    let mut reparse_tag = 0;
    if is_reparse {
        let mut tag = FILE_ATTRIBUTE_TAG_INFO {
            FileAttributes: 0,
            ReparseTag: 0,
        };
        // SAFETY: `handle` は有効、`tag` は大きさの合う書き込み先。
        let ok = unsafe {
            GetFileInformationByHandleEx(
                handle,
                FileAttributeTagInfo,
                (&mut tag as *mut FILE_ATTRIBUTE_TAG_INFO).cast(),
                std::mem::size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
            )
        };
        // tag はログで見分けるためだけ。読めなくても reparse point として断る（0 のまま）。
        if ok != 0 {
            reparse_tag = tag.ReparseTag;
        }
    }
    Ok(Facts {
        is_dir: info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0,
        is_reparse,
        reparse_tag,
        links: info.nNumberOfLinks,
    })
}

/// ファイルとして扱ってよいか（純関数）: reparse point でない・フォルダでない・リンク数 1。
/// 合わなければ理由（見る順もこの順）。
pub(crate) fn file_refusal(facts: &Facts) -> Option<Refusal> {
    if facts.is_reparse {
        return Some(Refusal::ReparsePoint {
            tag: facts.reparse_tag,
        });
    }
    if facts.is_dir {
        return Some(Refusal::NotAFile);
    }
    if facts.links != 1 {
        return Some(Refusal::HardLinked { links: facts.links });
    }
    None
}

/// フォルダとして扱ってよいか（純関数）: reparse point でない・フォルダ。
pub(crate) fn dir_refusal(facts: &Facts) -> Option<Refusal> {
    if facts.is_reparse {
        return Some(Refusal::ReparsePoint {
            tag: facts.reparse_tag,
        });
    }
    if !facts.is_dir {
        return Some(Refusal::NotADirectory);
    }
    None
}

/// 開いたハンドルを `std::fs::File` へ移す（閉じる責任も移る）。
pub(crate) fn into_file(handle: OwnedHandle) -> std::fs::File {
    use std::os::windows::io::FromRawHandle;
    // SAFETY: `into_raw` で所有を手放した有効なハンドル。以後は `File` だけが閉じる。
    unsafe { std::fs::File::from_raw_handle(handle.into_raw() as _) }
}

/// `ERROR_INVALID_PARAMETER`・`ERROR_NOT_SUPPORTED`・`ERROR_INVALID_FUNCTION`（POSIX の付け替え・
/// 削除を知らないファイルシステム・古い OS）。
fn is_unsupported_class(os_error: u32) -> bool {
    use windows_sys::Win32::Foundation::{
        ERROR_INVALID_FUNCTION, ERROR_INVALID_PARAMETER, ERROR_NOT_SUPPORTED,
    };
    os_error == ERROR_INVALID_PARAMETER
        || os_error == ERROR_NOT_SUPPORTED
        || os_error == ERROR_INVALID_FUNCTION
}

fn io_error(op: IoOp, os_error: u32) -> SafeRootError {
    SafeRootError::io(op, &std::io::Error::from_raw_os_error(os_error as i32))
}

/// `handle`（`DELETE` を持つファイル）の名前を、握ったフォルダ `dir`（`handle` の今の親）の中の
/// `name` へ付け替え、既にあれば置き換える（`NtSetInformationFile` の `FileRenameInformationEx`、
/// `REPLACE_IF_EXISTS | POSIX_SEMANTICS`、`RootDirectory` = `dir`、名前 1 要素）。宛先は握った
/// ハンドルを基準に決まるので、親のパスを解決し直さない。
///
/// **Win32 の `SetFileInformationByHandle(FileRenameInfo)` は使わない**: 【実測】名前 1 要素・
/// `RootDirectory` なしで渡すと（`FILE_RENAME_INFO` にも `RootDirectory` はあり、それを渡した
/// 場合は測っていない。測ったのは `RootDirectory` なしだけ）、kernelbase が名前を**今のプロセスのカレントディレクトリ**基準の
/// パスに直してしまい（Windows 11 26300、2026-10-18。カレントが別のボリュームだと
/// `ERROR_NOT_SAME_DEVICE`(17)、同じボリュームなら木の外のカレントへ移る）、ハンドル基準に
/// ならない。ntdll を直接呼び、`RootDirectory` を明示する。
///
/// 【実測】宛先が空の普通のフォルダなら `ERROR_ACCESS_DENIED` で失敗するが、**ジャンクション（・
/// ディレクトリのシンボリックリンク）の宛先は置き換えてしまう**（Fable B-6 の「失敗する」は成り立た
/// ない）。そのため呼び出し側（`ManagedDir::write_atomic`）が付け替えの前に宛先を属性だけで開いて
/// 確かめ、フォルダ・リンクなら断る（テスト `write_atomic_refuses_a_directory_or_junction_destination`）。
/// 宛先がファイルのシンボリックリンク・ハードリンクなら、**名前だけ**が置き換わり、リンクの先・
/// 別名の中身は変わらない。POSIX の付け替えを知らないファイルシステムでは
/// `FileRenameInformation`（`ReplaceIfExists`）でやり直す。
pub(crate) fn rename_replace(handle: HANDLE, dir: HANDLE, name: &str) -> Result<()> {
    use windows_sys::Wdk::Storage::FileSystem::{
        FileRenameInformation, FileRenameInformationEx, NtSetInformationFile,
        FILE_RENAME_INFORMATION, FILE_RENAME_POSIX_SEMANTICS, FILE_RENAME_REPLACE_IF_EXISTS,
    };
    use windows_sys::Win32::Foundation::RtlNtStatusToDosError;
    use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

    if !crate::guard::is_plain_child_name(name) {
        return Err(io_error(IoOp::RenameFile, 123)); // ERROR_INVALID_NAME
    }
    let wide: Vec<u16> = name.encode_utf16().collect();
    let name_bytes = wide.len() * 2;
    let total = std::mem::size_of::<FILE_RENAME_INFORMATION>() + name_bytes;
    let attempt = |class, flags: u32| -> std::result::Result<(), u32> {
        // 8 バイト境界のバッファ（`FILE_RENAME_INFORMATION` はハンドルを含む）。
        let mut buffer = vec![0u64; total.div_ceil(8)];
        let info = buffer.as_mut_ptr() as *mut FILE_RENAME_INFORMATION;
        // SAFETY: `buffer` は構造体と名前の分の大きさがあり、8 バイト境界。`FileName` は可変長で、
        // その位置から `name_bytes` バイト書く（領域の中）。
        unsafe {
            (*info).Anonymous.Flags = flags;
            (*info).RootDirectory = dir;
            (*info).FileNameLength = name_bytes as u32;
            std::ptr::copy_nonoverlapping(
                wide.as_ptr(),
                std::ptr::addr_of_mut!((*info).FileName).cast::<u16>(),
                wide.len(),
            );
        }
        // SAFETY: 全フィールドが整数・ポインタの構造体（ゼロは有効な値）。
        let mut status_block: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };
        // SAFETY: `handle` は DELETE を持つ有効なハンドル、`dir` は開いている親のハンドル。
        // `buffer` は `total` バイト以上。
        let status = unsafe {
            NtSetInformationFile(
                handle,
                &mut status_block,
                buffer.as_ptr().cast(),
                total as u32,
                class,
            )
        };
        if status < 0 {
            // SAFETY: NTSTATUS を Win32 のエラーに写すだけ。
            Err(unsafe { RtlNtStatusToDosError(status) })
        } else {
            Ok(())
        }
    };
    match attempt(
        FileRenameInformationEx,
        FILE_RENAME_REPLACE_IF_EXISTS | FILE_RENAME_POSIX_SEMANTICS,
    ) {
        Ok(()) => Ok(()),
        Err(e) if is_unsupported_class(e) => {
            // `ReplaceIfExists` は共用体の先頭の bool（1 = 置き換える）。
            attempt(FileRenameInformation, 1).map_err(|e| io_error(IoOp::RenameFile, e))
        }
        Err(e) => Err(io_error(IoOp::RenameFile, e)),
    }
}

/// `handle`（`DELETE` を持つ）の物を消す印を付ける（`FileDispositionInfoEx`、
/// `DELETE | POSIX_SEMANTICS`。閉じたときに名前が消える。ほかの主体が開いていても名前は
/// すぐ消える）。POSIX の削除を知らないファイルシステムでは `FileDispositionInfo` でやり直す。
pub(crate) fn delete_posix(handle: HANDLE) -> Result<()> {
    use windows_sys::Win32::Storage::FileSystem::{
        FileDispositionInfoEx, SetFileInformationByHandle, FILE_DISPOSITION_FLAG_DELETE,
        FILE_DISPOSITION_FLAG_POSIX_SEMANTICS, FILE_DISPOSITION_INFO_EX,
    };
    let info = FILE_DISPOSITION_INFO_EX {
        Flags: FILE_DISPOSITION_FLAG_DELETE | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS,
    };
    // SAFETY: `handle` は DELETE を持つ有効なハンドル。`info` は呼び出しの間生きている。
    let ok = unsafe {
        SetFileInformationByHandle(
            handle,
            FileDispositionInfoEx,
            (&info as *const FILE_DISPOSITION_INFO_EX).cast(),
            std::mem::size_of::<FILE_DISPOSITION_INFO_EX>() as u32,
        )
    };
    if ok != 0 {
        return Ok(());
    }
    let e = last_error();
    if is_unsupported_class(e) {
        return crate::acl::imp::delete_by_handle(handle);
    }
    Err(io_error(IoOp::RemoveFile, e))
}

/// `SafeRootError` を `io::Error` へ写す（[`crate::nofollow`] は `io::Result` を返すため）。
/// OS のエラー番号があればそれを保つ。
pub(crate) fn to_io(error: SafeRootError) -> std::io::Error {
    match error {
        SafeRootError::AclApi { os_error, .. } => {
            std::io::Error::from_raw_os_error(os_error as i32)
        }
        SafeRootError::Io {
            os_error: Some(code),
            ..
        } => std::io::Error::from_raw_os_error(code),
        SafeRootError::Io { kind, .. } => std::io::Error::from(kind),
        other => std::io::Error::other(other.to_string()),
    }
}

/// ハンドルの本当のパス（[`crate::acl`] の `final_path_of`。`\\?\` を外した DOS 形式）。
pub(crate) fn final_path(handle: HANDLE) -> Result<PathBuf> {
    crate::acl::imp::final_path_of(handle)
}
