//! パスの版の小さな部品: ファイル 1 つを**リンクを辿らずに**開いて確かめる（#623 PR-1b。
//! もとは `banto-instance-lock` の診断ファイルの中身（#623 PR-2）。設計 §1.4・§4.6）。
//!
//! 握ったフォルダ（`ManagedDir`）を持たない呼び出し元（コンソール・テスト・パスで受ける
//! 互換の口）のための部品で、渡されたパスのファイルを次の順で扱う:
//!
//! - `FILE_FLAG_OPEN_REPARSE_POINT` で開く（最後の要素がリンクなら、リンクそのものが開く）。
//!   [`OpenHow::Write`] は新規を `CREATE_NEW`、既にあれば `OPEN_EXISTING` で開き直す
//!   （`OPEN_ALWAYS`/`CREATE_ALWAYS` のように開く時点で切り詰めない - 切り詰めは確かめた後に
//!   呼ぶ側が行う）。
//! - 開いた**ハンドル**で、reparse point でない・フォルダでない・リンク数が 1・本当のパス
//!   （`GetFinalPathNameByHandleW`）が渡されたパス（[`normalize_expected`] で 8.3 の短い名前を
//!   長い名前に直したもの）と同じ（[`crate::guard::paths_match`]）、を確かめる。最後の確かめで、
//!   途中の要素がジャンクション・シンボリックリンクにされていて外へ出た場合を捕まえる。
//! - 合わなければ [`Verified::Refused`]（ハンドルは閉じてある）。**この呼び出しで作った**
//!   ファイルが合わなければ、ハンドルで消す（外に空のファイルを残さない）。消せなかったときは
//!   [`LeftBehind`] で呼ぶ側へ返す（この crate はログを出さない）。
//!
//! 渡されたパスの途中にジャンクション・マウントポイント・`subst`・ネットワークドライブがあると、
//! 本当のパスが渡されたパスと食い違うので断られる。呼ぶ側は**管理者が決める部分だけ**を
//! 解いてから、利用者が触れる部分を連結して渡す（利用者の部分まで解くと、植えられたリンクを
//! 解いてしまい確かめが無意味になる）。`GetFinalPathNameByHandleW` の `VOLUME_NAME_DOS` は
//! ドライブ文字の無いボリューム（フォルダにだけマウント）で失敗するので、そこでは開けない
//! （既知の制限。Fable B-4-5）。
//!
//! 非 Windows は [`open_file_verified`] が `io::ErrorKind::Unsupported` を返す（黙って成功に
//! しない。`banto-instance-lock` の非 Windows は自前の `O_NOFOLLOW` の版を使う）。

use std::fs::File;
use std::path::{Path, PathBuf};

use crate::error::Refusal;

/// [`open_file_verified`] の開き方。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpenHow {
    /// 書くために開く。無ければ作り（`CREATE_NEW`）、あれば開き直す（`OPEN_EXISTING`）。
    /// **中身は切り詰めない**（確かめた後に呼ぶ側が `set_len(0)` する）。権利は書き込みと属性の
    /// 読み取り（作ったときだけ `DELETE` も - 合わなければハンドルで消すため）。
    Write,
    /// 読むために開く（既にあるものだけ）。
    Read,
}

/// この呼び出しで作ったファイルが確かめに合わず、消そうとして消せなかった（外に空のファイルが
/// 残っている可能性がある）。
#[derive(Debug)]
pub struct LeftBehind {
    /// 作った物の本当のパス（読めなかったときは `None`）。
    pub actual: Option<PathBuf>,
    /// 消せなかった理由。
    pub error: std::io::Error,
}

/// [`open_file_verified`] の結果。
#[derive(Debug)]
pub enum Verified {
    /// 確かめに通った。`created` はこの呼び出しで作ったか（[`OpenHow::Write`] だけ）。
    File { file: File, created: bool },
    /// 確かめに合わなかった（ハンドルは閉じてある）。`left_behind` は、この呼び出しで作った
    /// ファイルを消せなかったとき。
    Refused {
        refusal: Refusal,
        left_behind: Option<LeftBehind>,
    },
}

/// 渡されたパスを、本当のパス（`GetFinalPathNameByHandleW`）と比べられる形にする: 絶対パスに
/// して、8.3 の短い名前を長い名前に直し（`GetLongPathNameW`）、先頭の `\\?\`（UNC は
/// `\\?\UNC\` → `\\`）を外す。直せない（無いパスなど）ときは絶対パスのまま（比べて食い違えば
/// 断る側に倒れる）。
///
/// **期待値を長い名前にするだけで、本当のパスを求める関数ではない**（Fable B-4-6）。
/// `GetLongPathNameW` は名前の要素ごとに引くので、途中のジャンクションを**辿って**その先の
/// 名前で長くする - ジャンクションの名前そのものは残るので、本当のパスとは食い違う。
/// 場所の確かめは必ずハンドルの本当のパスと比べる。非 Windows は `std::path::absolute` だけ。
pub fn normalize_expected(path: &Path) -> PathBuf {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    imp::long_path(&absolute)
        .map(|long| strip_verbatim(&long))
        .unwrap_or_else(|| strip_verbatim(&absolute))
}

/// 先頭の `\\?\`（UNC は `\\?\UNC\` → `\\`）を外す（純関数。`final_path_of` と同じ形にする）。
fn strip_verbatim(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        path.to_path_buf()
    }
}

/// `path` のファイルを**リンクを辿らずに**開いて確かめる（モジュール doc）。開くこと自体の
/// 失敗（無い・拒否など）は `Err`。確かめに合わなければ `Ok(Verified::Refused)`。
pub fn open_file_verified(path: &Path, how: OpenHow) -> std::io::Result<Verified> {
    imp::open_file_verified(path, how)
}

#[cfg(not(windows))]
mod imp {
    use super::{OpenHow, Verified};
    use std::path::{Path, PathBuf};

    pub(super) fn long_path(_path: &Path) -> Option<PathBuf> {
        None
    }

    pub(super) fn open_file_verified(_path: &Path, _how: OpenHow) -> std::io::Result<Verified> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "この OS ではリンクを辿らない確かめを使えません",
        ))
    }
}

#[cfg(windows)]
mod imp {
    use super::{LeftBehind, OpenHow, Verified};
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::path::{Path, PathBuf};

    use windows_sys::Win32::Foundation::{
        ERROR_ALREADY_EXISTS, ERROR_FILE_EXISTS, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, GetLongPathNameW, CREATE_NEW, DELETE, FILE_ATTRIBUTE_NORMAL,
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_READ_ATTRIBUTES,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };

    use crate::acl::imp::OwnedHandle;
    use crate::error::Refusal;
    use crate::handle_io::{self, to_io};

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    pub(super) fn long_path(path: &Path) -> Option<PathBuf> {
        let wide = wide(path);
        let mut buf = vec![0u16; 512];
        loop {
            // SAFETY: `wide` は NUL 終端、`buf` は書ける長さ `buf.len()` の領域。
            let len = unsafe { GetLongPathNameW(wide.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) }
                as usize;
            if len == 0 {
                return None;
            }
            if len < buf.len() {
                buf.truncate(len);
                return Some(PathBuf::from(std::ffi::OsString::from_wide(&buf)));
            }
            // 足りないときは終端を含む必要な長さが返る。
            buf.resize(len + 1, 0);
        }
    }

    /// `FILE_FLAG_OPEN_REPARSE_POINT` で開く（最後の要素がリンクでも辿らない）。共有は
    /// `std::fs::OpenOptions` の既定と同じ（読み・書き・削除）。`FILE_FLAG_BACKUP_SEMANTICS` を
    /// 付けないので、フォルダは開けない（拒否で失敗する）。
    fn open_raw(path: &Path, access: u32, disposition: u32) -> std::io::Result<OwnedHandle> {
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
            return Err(std::io::Error::last_os_error());
        }
        Ok(OwnedHandle(handle))
    }

    /// 開いたハンドルを確かめる（モジュール doc の順）。合わなければ理由を返す。
    fn verify(handle: &OwnedHandle, path: &Path) -> std::io::Result<Option<Refusal>> {
        let facts = handle_io::facts(handle.raw()).map_err(to_io)?;
        if let Some(refusal) = handle_io::file_refusal(&facts) {
            return Ok(Some(refusal));
        }
        let actual = handle_io::final_path(handle.raw()).map_err(to_io)?;
        if !crate::guard::paths_match(&actual, &super::normalize_expected(path)) {
            return Ok(Some(Refusal::Elsewhere { actual }));
        }
        Ok(None)
    }

    /// この呼び出しで作ったファイルを、ハンドルで消す。消せなければ本当のパスつきで返す。
    fn delete_created(handle: &OwnedHandle) -> Option<LeftBehind> {
        let error = crate::acl::imp::delete_by_handle(handle.raw()).err()?;
        Some(LeftBehind {
            actual: handle_io::final_path(handle.raw()).ok(),
            error: to_io(error),
        })
    }

    pub(super) fn open_file_verified(path: &Path, how: OpenHow) -> std::io::Result<Verified> {
        let (handle, created) = match how {
            OpenHow::Read => (open_raw(path, FILE_GENERIC_READ, OPEN_EXISTING)?, false),
            OpenHow::Write => {
                const WRITE: u32 = FILE_GENERIC_WRITE | FILE_READ_ATTRIBUTES;
                match open_raw(path, WRITE | DELETE, CREATE_NEW) {
                    Ok(handle) => (handle, true),
                    Err(err)
                        if err.raw_os_error() == Some(ERROR_FILE_EXISTS as i32)
                            || err.raw_os_error() == Some(ERROR_ALREADY_EXISTS as i32) =>
                    {
                        (open_raw(path, WRITE, OPEN_EXISTING)?, false)
                    }
                    Err(err) => return Err(err),
                }
            }
        };
        let refusal = match verify(&handle, path) {
            Ok(None) => {
                return Ok(Verified::File {
                    file: handle_io::into_file(handle),
                    created,
                })
            }
            Ok(Some(refusal)) => refusal,
            Err(err) => {
                if created {
                    if let Some(left) = delete_created(&handle) {
                        // 確かめ自体が失敗した経路。消せなかったことも理由に足す。
                        let actual = left
                            .actual
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|| "(不明)".to_string());
                        return Err(std::io::Error::new(
                            err.kind(),
                            format!(
                                "{err}（作ったファイルを消せませんでした。空のファイルが残っている可能性があります。実際の場所: {actual}: {}）",
                                left.error
                            ),
                        ));
                    }
                }
                return Err(err);
            }
        };
        // 作ったのが外（途中のリンクの先）なら、空のファイルを残さない。
        let left_behind = if created {
            delete_created(&handle)
        } else {
            None
        };
        Ok(Verified::Refused {
            refusal,
            left_behind,
        })
    }
}

#[cfg(test)]
mod tests;
