//! 診断ファイルを**リンクを辿らずに**開く（#623 PR-2）。
//!
//! 診断ファイルはデータのルート（ChronoGazer の `<root>`、banto-hub の
//! `profiles\<id>`）に置かれ、そこにはインストール利用者が「変更」を持つ。
//! LocalSystem のサービスがパスのまま `create + truncate` で開くと、利用者が事前に
//! 置いたシンボリックリンク・ハードリンク・途中のジャンクションを辿り、**外の任意の
//! ファイルを 0 バイトに切り詰めて書く**（失敗は握りつぶすので気付かれない）。
//! ここでは次の順で扱い、確かめに合わないものは書かず・読まずに捨てる:
//!
//! - **Windows**（確かめの本体は `banto_safe_root::nofollow::open_file_verified`。#623 PR-1b で
//!   共有 crate へ移した。ここは結果を診断ファイルの言葉に写すだけ）:
//!   `FILE_FLAG_OPEN_REPARSE_POINT` で開く（最後の要素がリンクなら、
//!   リンクそのものが開く）。新規は `CREATE_NEW`、既にあれば `OPEN_EXISTING` で
//!   開き直す（`OPEN_ALWAYS`/`CREATE_ALWAYS` のように開く時点で切り詰めない）。
//!   開いた**ハンドル**で、reparse point でない・ディレクトリでない・リンク数が 1
//!   （ハードリンクの別名が無い）・本当のパス（`GetFinalPathNameByHandleW`）が渡された
//!   パス（8.3 の短い名前は長い名前に直して比べる）と同じ、を確かめる。最後の確かめで、
//!   途中の要素がジャンクション・シンボリックリンクにされていて外へ出た場合を捕まえる。
//!   確かめた後にだけ `set_len(0)` して書く（ハンドルが指す実体は確かめた後に
//!   すり替えられない）。この呼び出しで作ったファイルが確かめに合わなければ、
//!   ハンドルで消す（外に空のファイルを残さない）。消せなかったときは警告の文字列を
//!   呼び出し側へ返す（`InstanceLockGuard::diagnostics_warning`。この crate は
//!   ログを出さない - 呼び出し側のアプリが自分のログに 1 行出す）。
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
    //! Windows の確かめは `banto_safe_root::nofollow::open_file_verified`（#623 PR-1b で
    //! この中身を共有 crate へ移した）。ここは結果を診断ファイルの言葉に写すだけ。
    use super::{Opened, Refusal};
    use std::io;
    use std::path::Path;

    use banto_safe_root::nofollow::{self, LeftBehind, OpenHow, Verified};

    impl From<banto_safe_root::Refusal> for Refusal {
        fn from(refusal: banto_safe_root::Refusal) -> Self {
            match refusal {
                banto_safe_root::Refusal::ReparsePoint { .. } => Refusal::ReparsePoint,
                banto_safe_root::Refusal::NotAFile | banto_safe_root::Refusal::NotADirectory => {
                    Refusal::NotARegularFile
                }
                banto_safe_root::Refusal::HardLinked { links } => Refusal::HardLinked {
                    links: u64::from(links),
                },
                banto_safe_root::Refusal::Elsewhere { actual } => Refusal::Elsewhere { actual },
            }
        }
    }

    /// 作ったが確かめに合わず、消せなかったときの警告（呼び出し側がアプリのログに出す）。
    fn left_behind_warning(path: &Path, left: LeftBehind) -> String {
        let actual = left
            .actual
            .map(|actual| actual.display().to_string())
            .unwrap_or_else(|| "(不明)".to_string());
        format!(
            "確かめに合わなかった診断ファイル {} を作ったが消せませんでした（空のファイルが残っている可能性があります。実際の場所: {actual}）: {}",
            path.display(),
            left.error
        )
    }

    /// 書くために開く。中身は**切り詰めない**（確かめた後に呼ぶ側が `set_len(0)`）。
    ///
    /// 2 つ目の値は、この呼び出しで作った確かめに合わないファイルを消せなかった
    /// ときの警告（呼び出し側がアプリのログに出す）。
    pub(crate) fn open_for_write(path: &Path) -> io::Result<(Opened, Option<String>)> {
        Ok(match nofollow::open_file_verified(path, OpenHow::Write)? {
            Verified::File { file, .. } => (Opened::File(file), None),
            Verified::Refused {
                refusal,
                left_behind,
            } => (
                Opened::Refused(refusal.into()),
                left_behind.map(|left| left_behind_warning(path, left)),
            ),
        })
    }

    /// 読むために開く（既にあるものだけ）。
    pub(crate) fn open_for_read(path: &Path) -> io::Result<Opened> {
        Ok(match nofollow::open_file_verified(path, OpenHow::Read)? {
            Verified::File { file, .. } => Opened::File(file),
            Verified::Refused { refusal, .. } => Opened::Refused(refusal.into()),
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
