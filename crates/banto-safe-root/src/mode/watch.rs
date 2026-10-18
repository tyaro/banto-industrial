//! 木のセキュリティの変化の見張り（#623 PR-3、設計 §2.9 (c)。Windows だけ）。
//!
//! 締め終わった後、握ったルートと**同じオブジェクト**を見張り用にもう 1 本開き
//! （`FILE_LIST_DIRECTORY`、overlapped）、`ReadDirectoryChangesW(FILE_NOTIFY_CHANGE_SECURITY,
//! 部分木)` を待つ。運用モードの間にサービス自身は記述子を変えないので、通知が来たら
//! 呼ぶ側は改ざんとみなす（エンジンを止め・締めたまま終了コード 5）。通知のあふれ
//! （バッファに入り切らず名前が返らない）も同じ扱い（[`WatchEvent::Overflow`]）。
//!
//! 見張りは**従**（設計 §2.9）: 古いハンドルの `SetKernelObjectSecurity` による変更は、
//! そのハンドルを閉じるまで通知されない（実測）。主は印のファイルと、パスで渡す直前・定期の
//! `recheck`。見張りが捕まえるのは管理者・GPO・外のソフトによる（パスの）変更と、閉じた後の
//! 遅れた通知。
//!
//! # 開き方
//!
//! 見張り用のハンドルは**パスで**開くが（`OpenFileById` は overlapped の指定ができない）、
//! 開いた後に 128 ビットのファイル ID を握ったルートと比べ、違えば閉じて断る（その間にパスが
//! ジャンクションにすり替えられていても、`OPEN_REPARSE_POINT` で開くのでリンクそのものが開き、
//! ID が違う）。見張るのは SYSTEM・Administrators だけが書ける締まった木なので、見張り用の
//! ハンドルから木へ書くことは無い（権利は列挙だけ）。
//!
//! # 止め方
//!
//! `ReadDirectoryChangesW` は overlapped で出し、[`SecurityWatch::wait`] は I/O の完了の
//! イベントと止める合図のイベント（[`WatchStop`]）を一緒に待つ。止める合図が来たら
//! `CancelIoEx` で取り消し、完了を待ってから戻る（カーネルがバッファに書き終える前に
//! バッファを解放しない）。drop でも同じ。バッファと `OVERLAPPED` は `Box` に置き、
//! [`SecurityWatch`] の値が動いても住所が変わらないようにする。

use std::time::Duration;

use crate::acl::imp::{api, file_id_of, last_error, OwnedHandle};
use crate::acl::PinnedDir;
use crate::error::{IoOp, Refusal, Result, SafeRootError};

use std::os::windows::ffi::OsStrExt;

use windows_sys::Win32::Foundation::{
    ERROR_IO_PENDING, ERROR_NOTIFY_ENUM_DIR, ERROR_OPERATION_ABORTED, HANDLE, INVALID_HANDLE_VALUE,
    WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, ReadDirectoryChangesW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_FLAG_OVERLAPPED, FILE_LIST_DIRECTORY, FILE_NOTIFY_CHANGE_SECURITY,
    FILE_NOTIFY_INFORMATION, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, ResetEvent, SetEvent, WaitForMultipleObjects, INFINITE,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

/// 通知のバッファ（`u32` の単位。64 KiB）。
const BUFFER_UNITS: usize = 16 * 1024;

/// [`SecurityWatch::wait`] の結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchEvent {
    /// 木の中で記述子が変わった（変わった名前。ルートからの相対。空のことは無い）。
    Changed(Vec<String>),
    /// 通知があふれた（名前は分からないが、何かが変わった）。
    Overflow,
    /// 時間内に何も来なかった（要求は出したまま）。
    Timeout,
    /// [`WatchStop::stop`] で止められた（以後は使わない）。
    Stopped,
}

/// 見張りを止める合図（別のスレッドから。`Clone`）。
#[derive(Clone)]
pub struct WatchStop(std::sync::Arc<OwnedHandle>);

impl WatchStop {
    /// 止める（何度呼んでもよい）。
    pub fn stop(&self) {
        // SAFETY: 有効な手動リセットのイベント。
        unsafe {
            SetEvent(self.0.raw());
        }
    }
}

/// ルートの木のセキュリティの変化の見張り（モジュール doc）。
pub struct SecurityWatch {
    handle: OwnedHandle,
    io_event: OwnedHandle,
    stop: WatchStop,
    buffer: Box<[u32]>,
    overlapped: Box<OVERLAPPED>,
    pending: bool,
    stopped: bool,
}

// SAFETY: 中身はカーネルのハンドルとヒープのバッファで、スレッドに結び付かない。I/O が
// 出ている間はバッファと OVERLAPPED の住所が変わらない（`Box`）。
unsafe impl Send for SecurityWatch {}

fn create_event() -> Result<OwnedHandle> {
    // SAFETY: 既定の属性・手動リセット・非シグナル・名前なし。
    let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
    if event.is_null() {
        return Err(api("CreateEventW", last_error()));
    }
    Ok(OwnedHandle(event))
}

impl PinnedDir {
    /// この（握った）フォルダを木のルートとして、セキュリティの変化を見張る
    /// [`SecurityWatch`] を作る（モジュール doc）。
    pub fn watch_security(&self) -> Result<SecurityWatch> {
        let wide: Vec<u16> = self
            .final_path()
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: `wide` は NUL 終端。辿らず（OPEN_REPARSE_POINT）、フォルダを開き
        // （BACKUP_SEMANTICS）、overlapped で `ReadDirectoryChangesW` を出せるようにする。
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                FILE_LIST_DIRECTORY,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_OVERLAPPED,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(SafeRootError::io(
                IoOp::InspectDir,
                &std::io::Error::from_raw_os_error(last_error() as i32),
            ));
        }
        let handle = OwnedHandle(handle);
        // 握ったルートと同じオブジェクトか（パスで開いたので確かめる）。
        if file_id_of(handle.raw())? != file_id_of(self.raw_handle())? {
            return Err(SafeRootError::Refused {
                path: self.final_path().to_path_buf(),
                refusal: Refusal::Elsewhere {
                    actual: crate::handle_io::final_path(handle.raw())
                        .unwrap_or_else(|_| self.final_path().to_path_buf()),
                },
            });
        }
        let io_event = create_event()?;
        let stop_event = create_event()?;
        // SAFETY: 全フィールドが整数・ポインタの構造体（ゼロは有効な初期値）。
        let mut overlapped: Box<OVERLAPPED> = Box::new(unsafe { std::mem::zeroed() });
        overlapped.hEvent = io_event.raw();
        Ok(SecurityWatch {
            handle,
            io_event,
            stop: WatchStop(std::sync::Arc::new(stop_event)),
            buffer: vec![0u32; BUFFER_UNITS].into_boxed_slice(),
            overlapped,
            pending: false,
            stopped: false,
        })
    }
}

impl SecurityWatch {
    /// 止める合図の口（別のスレッドへ渡す）。
    pub fn stopper(&self) -> WatchStop {
        self.stop.clone()
    }

    /// 要求がまだ出ていなければ出す。
    fn issue(&mut self) -> Result<()> {
        if self.pending {
            return Ok(());
        }
        // SAFETY: 有効なイベント。
        unsafe {
            ResetEvent(self.io_event.raw());
        }
        let mut returned = 0u32;
        // SAFETY: `handle` は FILE_LIST_DIRECTORY・overlapped で開いた。`buffer`・`overlapped` は
        // `Box` で、完了（`GetOverlappedResult`）まで解放しない（drop でも取り消して待つ）。
        let ok = unsafe {
            ReadDirectoryChangesW(
                self.handle.raw(),
                self.buffer.as_mut_ptr().cast(),
                (self.buffer.len() * 4) as u32,
                1,
                FILE_NOTIFY_CHANGE_SECURITY,
                &mut returned,
                &mut *self.overlapped,
                None,
            )
        };
        if ok == 0 {
            let os_error = last_error();
            if os_error != ERROR_IO_PENDING {
                return Err(api("ReadDirectoryChangesW", os_error));
            }
        }
        self.pending = true;
        Ok(())
    }

    /// 出ている要求の完了を待って結果を取る（`wait` が真なら完了まで待つ）。戻るのは
    /// 書かれたバイト数（取り消し・あふれは `Err` の OS の番号）。
    fn finish(&mut self, wait: bool) -> std::result::Result<u32, u32> {
        let mut bytes = 0u32;
        // SAFETY: `overlapped` は `issue` で渡した物。
        let ok = unsafe {
            GetOverlappedResult(
                self.handle.raw(),
                &*self.overlapped,
                &mut bytes,
                i32::from(wait),
            )
        };
        self.pending = false;
        if ok == 0 {
            return Err(last_error());
        }
        Ok(bytes)
    }

    /// 取り消して完了を待つ（止めるとき・drop）。
    fn cancel(&mut self) {
        if !self.pending {
            return;
        }
        // SAFETY: 出ている要求の `overlapped`。取り消せなくても（既に完了）、完了は待てる。
        unsafe {
            CancelIoEx(self.handle.raw(), &*self.overlapped);
        }
        let _ = self.finish(true);
    }

    /// 変わった名前を取り出す（`bytes` は `FILE_NOTIFY_INFORMATION` の並びの長さ）。
    fn names(&self, bytes: u32) -> Vec<String> {
        let mut names = Vec::new();
        if bytes == 0 {
            return names;
        }
        let base = self.buffer.as_ptr() as *const u8;
        let limit = (bytes as usize).min(self.buffer.len() * 4);
        let mut offset = 0usize;
        loop {
            if offset + std::mem::size_of::<FILE_NOTIFY_INFORMATION>() > limit {
                break;
            }
            // SAFETY: `offset` はバッファの中で、`NextEntryOffset` で進めた境界。構造体の先頭は
            // 4 バイト境界（OS が書く）。
            let info = unsafe { &*(base.add(offset) as *const FILE_NOTIFY_INFORMATION) };
            let len = (info.FileNameLength as usize / 2).min(
                (limit - offset - std::mem::offset_of!(FILE_NOTIFY_INFORMATION, FileName)) / 2,
            );
            // SAFETY: 名前は `FileNameLength` バイト続き、上で `limit` に収めた。
            let name = unsafe { std::slice::from_raw_parts(info.FileName.as_ptr(), len) };
            names.push(String::from_utf16_lossy(name));
            if info.NextEntryOffset == 0 {
                break;
            }
            offset += info.NextEntryOffset as usize;
        }
        names
    }

    /// 次の変化を `timeout` まで待つ（モジュール doc）。止められた後は常に
    /// [`WatchEvent::Stopped`]。
    pub fn wait(&mut self, timeout: Duration) -> Result<WatchEvent> {
        if self.stopped {
            return Ok(WatchEvent::Stopped);
        }
        self.issue()?;
        let handles: [HANDLE; 2] = [self.stop.0.raw(), self.io_event.raw()];
        let millis = u32::try_from(timeout.as_millis()).unwrap_or(INFINITE - 1);
        // SAFETY: どちらも有効なイベント。
        let waited = unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, millis) };
        if waited == WAIT_OBJECT_0 {
            self.stopped = true;
            self.cancel();
            return Ok(WatchEvent::Stopped);
        }
        if waited == WAIT_TIMEOUT {
            return Ok(WatchEvent::Timeout);
        }
        if waited != WAIT_OBJECT_0 + 1 {
            return Err(api("WaitForMultipleObjects", last_error()));
        }
        match self.finish(false) {
            Ok(0) => Ok(WatchEvent::Overflow),
            Ok(bytes) => {
                let names = self.names(bytes);
                if names.is_empty() {
                    Ok(WatchEvent::Overflow)
                } else {
                    Ok(WatchEvent::Changed(names))
                }
            }
            Err(ERROR_NOTIFY_ENUM_DIR) => Ok(WatchEvent::Overflow),
            Err(ERROR_OPERATION_ABORTED) => {
                self.stopped = true;
                Ok(WatchEvent::Stopped)
            }
            Err(os_error) => Err(api("GetOverlappedResult(ReadDirectoryChangesW)", os_error)),
        }
    }
}

impl Drop for SecurityWatch {
    fn drop(&mut self) {
        // 取り消して完了を待ってから、`OwnedHandle` の drop がハンドルを閉じる。
        self.cancel();
    }
}
