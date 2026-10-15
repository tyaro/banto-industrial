//! FFI の入口へ本体を渡す受け渡し場所（[`super`] のモジュール doc
//! 「1 プロセスに 1 つ」）。`define_windows_service!` が作る入口は関数
//! ポインタなので本体を引数で渡せず、型を消した本体を static に置く。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use super::{lock, DispatchError};

pub(super) type ServiceBody = Box<dyn FnOnce(Vec<std::ffi::OsString>) + Send>;

/// FFI の入口へ本体を渡す受け渡し場所（モジュール doc「1 プロセスに 1 つ」）。
#[cfg_attr(not(windows), allow(dead_code))]
pub(super) struct Handoff {
    started: AtomicBool,
    body: Mutex<Option<ServiceBody>>,
}

#[cfg_attr(not(windows), allow(dead_code))]
impl Handoff {
    pub(super) const fn new() -> Self {
        Self {
            started: AtomicBool::new(false),
            body: Mutex::new(None),
        }
    }

    /// 本体を置く。2 回目は（1 回目が失敗していても）`AlreadyStarted`。
    pub(super) fn claim(&self, body: ServiceBody) -> Result<(), DispatchError> {
        if self.started.swap(true, Ordering::SeqCst) {
            return Err(DispatchError::AlreadyStarted);
        }
        *lock(&self.body) = Some(body);
        Ok(())
    }

    /// 入口が本体を取り出す（1 回だけ）。
    pub(super) fn take(&self) -> Option<ServiceBody> {
        lock(&self.body).take()
    }

    /// ディスパッチャが起動できなかった。置いた本体を捨てる。
    pub(super) fn abandon(&self) {
        drop(self.take());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_handoff_is_claimed_once_per_process() {
        let handoff = Handoff::new();
        assert!(handoff.claim(Box::new(|_| {})).is_ok());
        assert!(handoff.take().is_some());
        assert!(handoff.take().is_none(), "取り出しは 1 回だけ");
        assert_eq!(
            handoff.claim(Box::new(|_| {})),
            Err(DispatchError::AlreadyStarted)
        );

        let failed = Handoff::new();
        assert!(failed.claim(Box::new(|_| {})).is_ok());
        failed.abandon();
        assert!(failed.take().is_none(), "失敗したら本体を捨てる");
        assert_eq!(
            failed.claim(Box::new(|_| {})),
            Err(DispatchError::AlreadyStarted),
            "ディスパッチャの起動は 1 回だけ"
        );
    }
}
