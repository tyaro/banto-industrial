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
//!   二重取得の失敗を検証できる）。**非 Windows は開発・CI 用**で、特権の
//!   サービス（root）として一般利用者が書ける場所を扱う運用は想定しない。
//!   診断ファイルの確かめが最後の要素のリンクとハードリンクだけで、途中の
//!   ディレクトリのリンクまで見ないのはこの前提による（`diag_file`）。
//!
//! # 診断ファイルはリンクを辿らない（#623 PR-2）
//!
//! 診断ファイルはデータのルート（ChronoGazer の `<root>`、banto-hub の
//! `profiles\<id>`。どちらもインストール利用者が「変更」を持つ）に置かれる。
//! 以前は取得後にパスのまま `create + truncate` で開いていたため、利用者が
//! そのパスを外のファイルへのシンボリックリンク・ハードリンクにしておく、または
//! 途中のフォルダをジャンクションにしておくと、**LocalSystem のサービスが外の
//! ファイルを 0 バイトに切り詰めて書いた**（任意ファイルの破壊。失敗は握り
//! つぶすので気付かれない）。今は書き・読みとも `diag_file` が辿らずに開き、
//! 開いたハンドルで「reparse point でない・通常のファイル・リンク数 1・本当の
//! パスが渡されたパスと同じ」を確かめてから、書くときだけ `set_len(0)` する。
//! 合わなければ書かない（Windows は排他の実体ではないので致命的にせず、理由を
//! [`InstanceLockGuard::diagnostics_warning`] で呼び出し側へ返す。この crate は
//! ログを出さない - アプリが自分のログに 1 行出す。非 Windows は flock の対象
//! なので `Io`）・読みは
//! [`OwnerInfoError::Corrupt`]。この crate は診断ファイルを消さない（ガードの
//! `Drop` は mutex のハンドル・fd を閉じるだけ。例外は、Windows でこの呼び出しが
//! 新しく作ったファイルが確かめに合わなかったときで、それはハンドルで消す）。
//!
//! 渡されたパスの途中にジャンクション・マウントポイント・`subst` があると、
//! 正当な構成でも本当のパスと食い違って書かれない。呼び出し側は**管理者が
//! 決める部分（ルート）だけ**を `canonicalize` してから、利用者が「変更」を
//! 持つ部分を連結して渡すこと（利用者の部分まで解くと、植えられたリンクを
//! 解いてしまい確かめが無意味になる）。
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

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use thiserror::Error;

mod diag_file;

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
    // 診断ファイルを書かなかった・作ったファイルを消せなかった理由（Windows だけ。
    // 取得の成否には関与しない）。呼び出し側がアプリのログに出す。
    diagnostics_warning: Option<String>,
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

    /// 取得は成功したが、診断ファイルを書かなかった（リンク・ハードリンク・外を
    /// 指していた・開けなかった等）、または確かめに合わず作ったファイルを消せな
    /// かったときの理由。無ければ `None`。**この crate はログを出さない**ので、
    /// 呼び出し側がアプリのログに 1 行出すこと。排他そのものには影響しない
    /// （Windows は mutex が取れれば成功）。非 Windows は診断ファイルが flock の
    /// 対象なので、書けなければ取得が `Io` で失敗し、常に `None`。
    pub fn diagnostics_warning(&self) -> Option<&str> {
        self.diagnostics_warning.as_deref()
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
    // `write_owner_info` が明示的に `set_len(0)` する。リンクは辿らない
    // （`O_NOFOLLOW` + fd で通常のファイル・リンク数 1 を確かめる。crate doc
    // 「診断ファイルはリンクを辿らない」）。ここでは flock の対象そのものなので、
    // 合わなければ `Io`。
    let mut file = match diag_file::open_for_lock(lock_path)? {
        diag_file::Opened::File(file) => file,
        diag_file::Opened::Refused(refusal) => {
            return Err(InstanceLockError::Io(std::io::Error::other(
                refusal.to_string(),
            )));
        }
    };

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
        diagnostics_warning: None,
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
    // 致命的にしない。リンクを辿らずに開き、ハンドルで確かめてから書く
    // （crate doc「診断ファイルはリンクを辿らない」）。
    let diagnostics_warning = write_diagnostics_windows(lock_path, host_kind);

    Ok(InstanceLockGuard {
        lock_file_path: lock_path.to_path_buf(),
        diagnostics_warning,
        _mutex: WindowsMutexHandle(handle),
    })
}

/// 診断ファイルへ書く（Windows）。確かめに合わない・開けないときは書かずに
/// 理由を返すだけ（排他は mutex が持っている。ログは呼び出し側）。
#[cfg(windows)]
fn write_diagnostics_windows(lock_path: &Path, host_kind: &str) -> Option<String> {
    let path = lock_path.display();
    match diag_file::open_for_write(lock_path) {
        Ok((diag_file::Opened::File(mut file), warning)) => {
            match write_owner_info(&mut file, host_kind) {
                Ok(()) => warning,
                Err(err) => Some(format!("診断ファイル {path} を書けませんでした: {err}")),
            }
        }
        Ok((diag_file::Opened::Refused(refusal), warning)) => {
            let mut msg = format!("診断ファイル {path} を書きません: {refusal}");
            if let Some(warning) = warning {
                msg.push_str(&format!("（{warning}）"));
            }
            Some(msg)
        }
        Err(err) => Some(format!("診断ファイル {path} を開けませんでした: {err}")),
    }
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

/// [`try_hold_named_mutex`] が取った名前付き mutex（drop で閉じる。全部のハンドルが
/// 閉じれば mutex は消える）。
#[cfg(windows)]
pub struct NamedMutexGuard {
    _mutex: WindowsMutexHandle,
}

/// **診断ファイルを書かずに**、名前付き mutex だけを取る（Windows だけ。#626 PR-B）。
/// [`try_acquire`] と同じ`CreateMutexW`（既定のセキュリティ記述子・初期所有者あり）で、
/// 取れれば`Some`、既にある（`ERROR_ALREADY_EXISTS`、またはアクセス拒否 = 権限の強い
/// ほかの作り手が持っている）なら`None`。それ以外の失敗は`Err`。
///
/// 昇格ヘルパー（banto-hub-elev の`grant-profile-acl`）が、profile の書き直しの間だけ
/// Hub と同じ mutex を持つために使う。持っている間は Hub の [`try_acquire`] が
/// [`InstanceLockError::AlreadyHeld`]（または拒否）で起動に失敗する。診断ファイルを
/// 書かないのは、確かめる前の profile のパスへ昇格したプロセスが書かないため。
#[cfg(windows)]
pub fn try_hold_named_mutex(name: &str) -> Result<Option<NamedMutexGuard>, std::io::Error> {
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, SetLastError, ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS,
    };
    use windows_sys::Win32::System::Threading::CreateMutexW;

    let wide_name: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: `wide_name` は NUL 終端。null の属性は既定のセキュリティ記述子
    // （[`try_acquire`] と同じ）。`SetLastError(0)` も同じ理由。
    let handle = unsafe {
        SetLastError(0);
        CreateMutexW(std::ptr::null(), 1, wide_name.as_ptr())
    };
    if handle.is_null() {
        // SAFETY: 直前の呼び出しのスレッドのエラー。
        let last_error = unsafe { GetLastError() };
        if last_error == ERROR_ACCESS_DENIED {
            return Ok(None);
        }
        return Err(std::io::Error::from_raw_os_error(last_error as i32));
    }
    // SAFETY: 直前の呼び出しのスレッドのエラー。
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        // SAFETY: `CreateMutexW`が返した有効なハンドル。
        unsafe {
            CloseHandle(handle);
        }
        return Ok(None);
    }
    Ok(Some(NamedMutexGuard {
        _mutex: WindowsMutexHandle(handle),
    }))
}

/// 診断ファイルから所有者情報を読む（読めなければ `None`）。排他の正当性には
/// 関与しない読み取り専用ヘルパー（banto-hub の health プローブも使う）。
///
/// 「無い」「アクセス拒否」「壊れている」を区別したい呼び出し側は
/// [`read_owner_info_detailed`] を使う（#626 PR-C。この関数は区別を捨てる）。
pub fn read_owner_info(lock_path: &Path) -> Option<OwnerInfo> {
    read_owner_info_detailed(lock_path).ok()
}

/// [`read_owner_info_detailed`] が読めなかった理由。エラーを 1 つの「読めない」に
/// 潰さず、呼び出し側が扱いを変えられるようにする（#626 PR-C）。
#[derive(Debug, Error)]
pub enum OwnerInfoError {
    /// ファイルが無い（`NotFound`）。まだ誰も取得していない、または消えた。
    #[error("診断ファイルがありません")]
    Missing,
    /// アクセス拒否（`PermissionDenied`）。この利用者のトークンでは開けない
    /// （profile ディレクトリの ACL を保護した場合に、所有者以外の利用者が
    /// ここへ来る）。**ファイルが在る確かな証拠ではない**: 途中のディレクトリの
    /// 検索権限が無くても同じ種別になる。ただし、開けなかった対象を辿らずに
    /// 調べられて通常のファイルでなければ（ディレクトリ・シンボリックリンク等。
    /// Windows の `File::open` はディレクトリを `PermissionDenied` で断る）
    /// ここへは来ず [`OwnerInfoError::Corrupt`] になる。中身は分からない。
    #[error("診断ファイルへのアクセスが拒否されました: {0}")]
    Denied(std::io::Error),
    /// 開けて読めたが JSON として解釈できない、または UTF-8 でない。リンク
    /// （reparse point）・ハードリンクの別名がある・本当のパスが渡されたパスと
    /// 違う（途中にリンク）ものも、辿って外を読まずにここにする（#623 PR-2）。
    #[error("診断ファイルの中身が壊れています: {0}")]
    Corrupt(String),
    /// 上のいずれでもない I/O エラー（共有違反・デバイスエラー等）。
    #[error("診断ファイルを読めませんでした: {0}")]
    Io(std::io::Error),
}

/// [`read_owner_info`] の区別つき版。`NotFound`/`PermissionDenied` は
/// `File::open` の失敗だけから決める（Windows の `ERROR_ACCESS_DENIED` は
/// `PermissionDenied` に写る）。読み取り中の `InvalidData`（UTF-8 でない）と
/// JSON の失敗は [`OwnerInfoError::Corrupt`]。リンクは辿らない（crate doc
/// 「診断ファイルはリンクを辿らない」。書くときと同じ確かめ）。
pub fn read_owner_info_detailed(lock_path: &Path) -> Result<OwnerInfo, OwnerInfoError> {
    use std::io::ErrorKind;
    let opened = diag_file::open_for_read(lock_path).map_err(|e| match e.kind() {
        ErrorKind::NotFound => OwnerInfoError::Missing,
        // Windows の `File::open` は `FILE_FLAG_BACKUP_SEMANTICS` を付けないので、
        // ディレクトリも `PermissionDenied` になる。辿らない metadata で形を見て、
        // 通常のファイルでないものは「拒否」ではなく「診断ファイルの形が壊れて
        // いる」（`Corrupt`。中身を読めた壊れ方と同じ、呼び出し側は安全側）にする。
        // metadata も取れないとき（拒否）は、拒否のままにする。
        ErrorKind::PermissionDenied => match std::fs::symlink_metadata(lock_path) {
            Ok(meta) if !meta.is_file() => {
                OwnerInfoError::Corrupt("診断ファイルが通常のファイルではありません".to_string())
            }
            _ => OwnerInfoError::Denied(e),
        },
        _ => OwnerInfoError::Io(e),
    })?;
    let mut file = match opened {
        diag_file::Opened::File(file) => file,
        diag_file::Opened::Refused(refusal) => {
            return Err(OwnerInfoError::Corrupt(refusal.to_string()));
        }
    };
    let mut contents = String::new();
    file.read_to_string(&mut contents)
        .map_err(|e| match e.kind() {
            ErrorKind::InvalidData => OwnerInfoError::Corrupt(e.to_string()),
            ErrorKind::PermissionDenied => OwnerInfoError::Denied(e),
            _ => OwnerInfoError::Io(e),
        })?;
    serde_json::from_str(&contents).map_err(|e| OwnerInfoError::Corrupt(e.to_string()))
}

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

/// `pid` のプロセスが今も生きているか（#392 B1-4。`run/endpoint.json` の
/// 古さの判定用）。**生きていると言い切れないときは `true`** を返す（権限が
/// 足りず調べられない・OS が答えない）。古いファイルを「生きている」と誤って
/// 読むほうが、生きているサービスのファイルを「無い」と読むより害が小さい
/// （前者は接続を試みて失敗するだけ、後者は二重起動の判断を誤らせうる）。
/// pid は再利用されるので、`true` は「そのファイルを書いたプロセスが生きている」
/// の証明ではない（同じ pid の別プロセスでも `true`）。`pid == 0` は `false`。
pub fn process_is_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    process_is_alive_impl(pid)
}

#[cfg(windows)]
fn process_is_alive_impl(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_INVALID_PARAMETER, WAIT_OBJECT_0,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
    };
    // SAFETY: 返ったハンドルは下で必ず閉じる。
    unsafe {
        let handle = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
        if handle.is_null() {
            // 存在しない pid は ERROR_INVALID_PARAMETER。権限不足（SYSTEM 所有の
            // サービスを一般利用者が調べる等）は「生きている」側に倒す。
            return GetLastError() != ERROR_INVALID_PARAMETER;
        }
        // 終了済みでもハンドルが残っていれば開けるので、待機で見る。終了コードを
        // STILL_ACTIVE（259）と比べる方法は、259 で終わったプロセスを永遠に
        // 生きていると読むので使わない。
        let wait = WaitForSingleObject(handle, 0);
        CloseHandle(handle);
        // 終了済み（シグナル状態）だけが死んでいる。タイムアウト・失敗は生きている側。
        wait != WAIT_OBJECT_0
    }
}

#[cfg(not(windows))]
fn process_is_alive_impl(pid: u32) -> bool {
    // SAFETY: シグナル 0 は存在確認だけで何も送らない。
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if rc == 0 {
        return true;
    }
    // ESRCH = 存在しない。EPERM（他ユーザーのプロセス）は生きている。
    std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

/// 対象ファイルのパスから、排他の単位になる短い安定 ID（16 桁の小文字 hex）を
/// 作る。同じファイルを指すパス（相対・絶対・`..` 入り）は同じ ID になり、
/// 別のファイルは別の ID になる。
///
/// - 親ディレクトリを `create_dir_all` してから `canonicalize` し、ファイル名を
///   足す（対象ファイル自体はまだ無くてよい — 初回起動の DB）。
/// - Windows ではファイルシステムが大文字小文字を区別しないので、ハッシュの
///   前に小文字へそろえる。
/// - 既にあるファイルは全体を `canonicalize` する（シンボリックリンクの別名は
///   本体と同じ ID）。**ハードリンクの別名は検出しない**（同じ実体への別パス
///   なので、パスから作る ID は別になる。ハードリンク越しに DB を開く構成は
///   非対応）。ファイルの実体 ID（dev+ino 等）を使わないのは、初回起動
///   （ファイル作成前）に取った ID と作成後に取った ID が食い違うため。
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
    // 既にあるファイルは**全体**を `canonicalize` してシンボリックリンクを解く
    // （リンク経由の別名で開いても本体と同じ ID になる）。まだ無いファイル
    // （初回起動）は親 + ファイル名。通常のファイル（リンクでない）は前者も
    // 「正規化した親 + 実際のファイル名」になるので、作成の前に ID を作った
    // プロセスと後に作ったプロセスで ID が食い違わない。
    let canonical = if target.exists() {
        target.canonicalize()?
    } else {
        parent.canonicalize()?.join(file_name)
    };

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

    /// #626 PR-B: 診断ファイルを書かずに持つ mutex は、持っている間は [`try_acquire`]・
    /// もう一度の [`try_hold_named_mutex`]（別のスレッドからも）を断り、放せば取れる。
    #[cfg(windows)]
    #[test]
    fn a_held_named_mutex_blocks_acquire_until_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let name = unique_mutex_name("hold");
        let held = try_hold_named_mutex(&name).unwrap().expect("取れる");
        assert!(matches!(
            try_acquire(&name, &lock_path(&dir, "x.lock"), "hub"),
            Err(InstanceLockError::AlreadyHeld { .. })
        ));
        let other = name.clone();
        let from_thread =
            std::thread::spawn(move || try_hold_named_mutex(&other).unwrap().is_none())
                .join()
                .unwrap();
        assert!(from_thread, "別のスレッドからも取れない");
        drop(held);
        let again = try_hold_named_mutex(&name).unwrap();
        assert!(again.is_some(), "放せば取れる");
        drop(again);
        let guard = try_acquire(&name, &lock_path(&dir, "y.lock"), "hub").expect("Hub も取れる");
        drop(guard);
    }

    #[test]
    fn read_owner_info_detailed_tells_missing_corrupt_and_ok_apart() {
        let dir = tempfile::tempdir().unwrap();
        let missing = lock_path(&dir, "none.lock");
        assert!(matches!(
            read_owner_info_detailed(&missing),
            Err(OwnerInfoError::Missing)
        ));
        let corrupt = lock_path(&dir, "bad.lock");
        std::fs::write(&corrupt, "not json").unwrap();
        assert!(matches!(
            read_owner_info_detailed(&corrupt),
            Err(OwnerInfoError::Corrupt(_))
        ));
        let non_utf8 = lock_path(&dir, "bin.lock");
        std::fs::write(&non_utf8, [0xff, 0xfe, 0xfd]).unwrap();
        assert!(matches!(
            read_owner_info_detailed(&non_utf8),
            Err(OwnerInfoError::Corrupt(_))
        ));
        let ok = lock_path(&dir, "ok.lock");
        std::fs::write(
            &ok,
            r#"{"pid":7,"host_kind":"service","acquired_at_unix_ms":1}"#,
        )
        .unwrap();
        assert_eq!(read_owner_info_detailed(&ok).unwrap().pid, 7);
        // 従来の薄い版は区別を捨てて None のまま。
        assert!(read_owner_info(&missing).is_none());
        assert!(read_owner_info(&corrupt).is_none());
        assert!(read_owner_info(&ok).is_some());
    }

    /// 明示の拒否 ACE を付けたファイルは `Denied`（`Missing`/`Corrupt` ではない）。
    #[cfg(windows)]
    #[test]
    fn read_owner_info_detailed_reports_denied_for_a_deny_ace() {
        let dir = tempfile::tempdir().unwrap();
        let path = lock_path(&dir, "denied.lock");
        std::fs::write(
            &path,
            r#"{"pid":7,"host_kind":"service","acquired_at_unix_ms":1}"#,
        )
        .unwrap();
        // Everyone (S-1-1-0) に「データ読み取り」の明示拒否。所有者にも効く。
        let out = std::process::Command::new("icacls")
            .arg(&path)
            .args(["/deny", "*S-1-1-0:(RD)"])
            .output()
            .expect("icacls");
        assert!(out.status.success(), "icacls: {out:?}");
        let result = read_owner_info_detailed(&path);
        assert!(
            matches!(result, Err(OwnerInfoError::Denied(_))),
            "got {result:?}"
        );
        assert!(read_owner_info(&path).is_none());
    }

    /// ディレクトリは、ACL を変えなくても Windows の `File::open` では
    /// `PermissionDenied` になる。`Denied` にしてはならない。
    #[test]
    fn read_owner_info_detailed_does_not_call_a_directory_denied() {
        let dir = tempfile::tempdir().unwrap();
        let sub = lock_path(&dir, "dir.lock");
        std::fs::create_dir(&sub).unwrap();
        let result = read_owner_info_detailed(&sub);
        assert!(
            !matches!(result, Err(OwnerInfoError::Denied(_))),
            "got {result:?}"
        );
        assert!(result.is_err());
    }

    #[test]
    fn process_is_alive_sees_this_process_and_rejects_pid_zero() {
        assert!(process_is_alive(std::process::id()));
        assert!(!process_is_alive(0));
    }

    #[test]
    fn process_is_alive_is_false_for_a_finished_child() {
        let mut command = if cfg!(windows) {
            let mut c = std::process::Command::new("cmd");
            c.args(["/C", "exit 0"]);
            c
        } else {
            std::process::Command::new("true")
        };
        let mut child = command.spawn().expect("spawn");
        let pid = child.id();
        child.wait().expect("wait");
        // wait 済み（ハンドルは Child がまだ持つので Windows でも終了コードで判定される）。
        assert!(!process_is_alive(pid));
    }

    /// 終了コード 259（= STILL_ACTIVE と同じ値）で終わったプロセスも死んでいると
    /// 判定する（#596 のレビュー）。
    #[cfg(windows)]
    #[test]
    fn process_is_alive_is_false_for_a_child_that_exited_with_259() {
        let mut child = std::process::Command::new("cmd")
            .args(["/C", "exit 259"])
            .spawn()
            .expect("spawn");
        let pid = child.id();
        let status = child.wait().expect("wait");
        assert_eq!(status.code(), Some(259));
        assert!(!process_is_alive(pid));
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

    #[test]
    fn path_scope_id_is_the_same_before_and_after_the_file_is_created() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("fresh.sqlite3");
        let (before, _) = path_scope_id(&db).unwrap();
        std::fs::write(&db, b"x").unwrap();
        let (after, _) = path_scope_id(&db).unwrap();
        assert_eq!(before, after);
    }

    #[test]
    fn a_symlink_alias_of_an_existing_file_has_the_same_id() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("real.sqlite3");
        std::fs::write(&db, b"x").unwrap();
        let alias = dir.path().join("alias.sqlite3");
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_file(&db, &alias);
        #[cfg(not(windows))]
        let made = std::os::unix::fs::symlink(&db, &alias);
        if let Err(err) = made {
            eprintln!("skip: シンボリックリンクを作れません（権限?）: {err}");
            return;
        }
        let (real_id, real_path) = path_scope_id(&db).unwrap();
        let (alias_id, alias_path) = path_scope_id(&alias).unwrap();
        assert_eq!(real_id, alias_id);
        assert_eq!(real_path, alias_path);

        // 実際にロックしても衝突する（診断ファイルも本体の隣の 1 つ）。
        let name = format!(
            r"Global\BantoInstanceLockTest.sym.{}.{real_id}",
            std::process::id()
        );
        let _first = try_acquire(&name, &real_path.with_extension("lock"), "a").unwrap();
        assert!(matches!(
            try_acquire(&name, &alias_path.with_extension("lock"), "b"),
            Err(InstanceLockError::AlreadyHeld { .. })
        ));
    }

    /// ハードリンクは検出しない（非対応）。この挙動を固定して、変わったら気付く。
    #[test]
    fn a_hard_link_alias_is_not_detected() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("real.sqlite3");
        std::fs::write(&db, b"x").unwrap();
        let alias = dir.path().join("hard.sqlite3");
        std::fs::hard_link(&db, &alias).unwrap();
        assert_ne!(
            path_scope_id(&db).unwrap().0,
            path_scope_id(&alias).unwrap().0
        );
    }

    // ---- #623 PR-2: 診断ファイルはリンクを辿らない ----

    /// 外のファイル（守るべき中身）。中身は `OwnerInfo` として読める JSON にして、
    /// 読みが外を辿れば `Ok` で返ってしまう（＝反証で落ちる）形にする。
    const VICTIM: &str = r#"{"pid":4242,"host_kind":"victim","acquired_at_unix_ms":1}"#;

    fn victim_file(outside: &tempfile::TempDir, name: &str) -> PathBuf {
        let path = outside.path().join(name);
        std::fs::write(&path, VICTIM).unwrap();
        path
    }

    fn assert_victim_untouched(path: &Path) {
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            VICTIM,
            "外のファイルが書き換えられた: {}",
            path.display()
        );
    }

    /// 取得を試みる。Windows は mutex が排他なので成功する（診断ファイルだけ
    /// 書かない。理由は `diagnostics_warning()` で返る）。非 Windows は flock の対象
    /// そのものなので `Io` で断る。Windows では警告（理由）を返す。
    fn acquire_next_to_a_link(tag: &str, lock: &Path) -> Option<String> {
        let result = try_acquire(&unique_mutex_name(tag), lock, "svc");
        if cfg!(windows) {
            let guard = result.expect("Windows は mutex で取れる");
            let warning = guard.diagnostics_warning().map(str::to_string);
            assert!(warning.is_some(), "書かなかった理由が返る");
            warning
        } else {
            assert!(
                matches!(result, Err(InstanceLockError::Io(_))),
                "非 Windows はリンクを断る"
            );
            None
        }
    }

    /// ふつうに書けたときは警告が無い。
    #[test]
    fn a_normal_acquire_has_no_diagnostics_warning() {
        let dir = tempfile::tempdir().unwrap();
        let lock = lock_path(&dir, "ok.lock");
        let guard = try_acquire(&unique_mutex_name("normal"), &lock, "svc").unwrap();
        assert_eq!(guard.diagnostics_warning(), None);
    }

    /// ハードリンクは一般利用者でも作れる（権限が要らない）ので、どの環境でも必ず走る。
    #[test]
    fn a_hard_linked_lock_file_does_not_truncate_or_read_the_outside_file() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        // 同じボリュームでないとハードリンクは作れない（tempdir は同じ所の下）。
        let victim = victim_file(&outside, "victim.json");
        let lock = lock_path(&dir, "x.lock");
        std::fs::hard_link(&victim, &lock).unwrap();

        let warning = acquire_next_to_a_link("hard", &lock);
        if cfg!(windows) {
            let warning = warning.unwrap();
            assert!(warning.contains("ハードリンク"), "理由を含む: {warning}");
            assert!(warning.contains("x.lock"), "パスを含む: {warning}");
        }
        assert_victim_untouched(&victim);
        let read = read_owner_info_detailed(&lock);
        assert!(
            matches!(read, Err(OwnerInfoError::Corrupt(_))),
            "ハードリンク越しに外を読まない: {read:?}"
        );
    }

    /// 診断ファイルのパスが外のファイルへのシンボリックリンク。リンクの作成に権限が
    /// 要る環境（開発者モードでない非管理者）では、手元は skip、`CI` では落とす。
    #[test]
    fn a_symlinked_lock_file_does_not_truncate_or_read_the_outside_file() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let victim = victim_file(&outside, "victim.json");
        let lock = lock_path(&dir, "x.lock");
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_file(&victim, &lock);
        #[cfg(not(windows))]
        let made = std::os::unix::fs::symlink(&victim, &lock);
        if let Err(err) = made {
            if std::env::var_os("CI").is_some() {
                panic!("CI でシンボリックリンクを作れない: {err}");
            }
            eprintln!("skip: シンボリックリンクを作れません（権限?）: {err}");
            return;
        }

        let warning = acquire_next_to_a_link("sym", &lock);
        if cfg!(windows) {
            let warning = warning.unwrap();
            assert!(warning.contains("リンク"), "理由を含む: {warning}");
        }
        assert_victim_untouched(&victim);
        let read = read_owner_info_detailed(&lock);
        assert!(
            matches!(read, Err(OwnerInfoError::Corrupt(_))),
            "シンボリックリンク越しに外を読まない: {read:?}"
        );
        // 壊れた（先の無い）リンクでも、先にファイルを作らない。
        let dangling_target = outside.path().join("not-yet.json");
        let dangling = lock_path(&dir, "dangling.lock");
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&dangling_target, &dangling).unwrap();
        #[cfg(not(windows))]
        std::os::unix::fs::symlink(&dangling_target, &dangling).unwrap();
        let _ = acquire_next_to_a_link("dangling", &dangling);
        assert!(
            !dangling_target.exists(),
            "壊れたリンクの先にファイルを作った"
        );
    }

    /// 途中のフォルダが外へのジャンクション。ジャンクションは権限なしで作れるので
    /// 必ず走る。既にある外のファイルを書き換えず、無かった名前を外に作り残さない。
    #[cfg(windows)]
    #[test]
    fn a_lock_file_inside_a_junction_does_not_touch_the_outside() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let victim = victim_file(&outside, "x.lock");
        let junction = dir.path().join("j");
        let out = std::process::Command::new("cmd")
            .arg("/C")
            .arg("mklink")
            .arg("/J")
            .arg(&junction)
            .arg(outside.path())
            .output()
            .expect("mklink");
        assert!(out.status.success(), "mklink /J: {out:?}");

        let lock = junction.join("x.lock");
        let _guard =
            try_acquire(&unique_mutex_name("junction"), &lock, "svc").expect("mutex は取れる");
        let warning = _guard
            .diagnostics_warning()
            .expect("書かなかった理由が返る");
        assert!(warning.contains("本当の場所"), "理由を含む: {warning}");
        assert_victim_untouched(&victim);
        let read = read_owner_info_detailed(&lock);
        assert!(
            matches!(read, Err(OwnerInfoError::Corrupt(_))),
            "ジャンクション越しに外を読まない: {read:?}"
        );

        let fresh = junction.join("fresh.lock");
        let _guard2 =
            try_acquire(&unique_mutex_name("junction-new"), &fresh, "svc").expect("mutex は取れる");
        assert!(
            !outside.path().join("fresh.lock").exists(),
            "ジャンクションの先に診断ファイルを作り残した"
        );
    }

    /// 8.3 の短い名前を含むパス（GitHub の Windows のランナーの `TEMP` は
    /// `RUNNER~1` を含む）でも、本当のパスと食い違わずに書ける。短い名前が無い
    /// ボリューム（8.3 の生成が無効）では確かめられないので skip する。
    #[cfg(windows)]
    #[test]
    fn a_short_name_path_still_writes_the_diagnostics() {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        use windows_sys::Win32::Storage::FileSystem::GetShortPathNameW;

        let dir = tempfile::tempdir().unwrap();
        let long_dir = dir.path().join("a long directory name");
        std::fs::create_dir(&long_dir).unwrap();
        let wide: Vec<u16> = long_dir
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let mut buf = vec![0u16; 1024];
        // SAFETY: `wide` は NUL 終端、`buf` は書ける長さの領域。
        let len = unsafe { GetShortPathNameW(wide.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) }
            as usize;
        assert!(len > 0 && len < buf.len(), "GetShortPathNameW");
        let short_dir = PathBuf::from(std::ffi::OsString::from_wide(&buf[..len]));
        if short_dir == long_dir {
            eprintln!("skip: このボリュームは 8.3 の短い名前を作らない");
            return;
        }
        let lock = short_dir.join("x.lock");
        let _guard = try_acquire(&unique_mutex_name("short"), &lock, "svc").unwrap();
        let owner = read_owner_info_detailed(&lock).expect("短い名前のパスでも読める");
        assert_eq!(owner.host_kind, "svc");
    }

    /// 前回の診断ファイルがそのまま残っていれば、確かめて上書きする（既存開きの経路）。
    #[test]
    fn an_existing_plain_lock_file_is_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let lock = lock_path(&dir, "x.lock");
        std::fs::write(&lock, VICTIM).unwrap();
        let _guard = try_acquire(&unique_mutex_name("existing"), &lock, "svc").unwrap();
        let owner = read_owner_info_detailed(&lock).unwrap();
        assert_eq!(owner.host_kind, "svc");
        assert_eq!(owner.pid, std::process::id());
    }
}
