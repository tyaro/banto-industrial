//! OS の PRESHUTDOWN の作業予算を超えたときの警告の見張り（#392 B3-1）。
//! Hub の停止処理が使う（ChronoGazer も B3-2 で使える）。停止処理そのものは
//! 止めない。

use std::time::Instant;

/// OS の PRESHUTDOWN の作業予算（[`crate::StopNotice::os_work_deadline`]）を
/// 超えたら 1 回だけ `on_overrun` を呼ぶ見張り。停止処理そのものは止めない
/// （警告を残すだけ）。予算内に終わったら [`Self::finish`] で畳む。
pub struct OverrunWatch {
    finished: std::sync::mpsc::Sender<()>,
    thread: std::thread::JoinHandle<()>,
}

impl OverrunWatch {
    /// `deadline` を過ぎても [`Self::finish`] が呼ばれなければ `on_overrun`。
    pub fn spawn<F>(deadline: Instant, on_overrun: F) -> Self
    where
        F: FnOnce() + Send + 'static,
    {
        let (finished, rx) = std::sync::mpsc::channel::<()>();
        let thread = std::thread::spawn(move || {
            let wait = deadline.saturating_duration_since(Instant::now());
            if let Err(std::sync::mpsc::RecvTimeoutError::Timeout) = rx.recv_timeout(wait) {
                on_overrun();
            }
        });
        Self { finished, thread }
    }

    /// 停止処理が終わった。予算内なら `on_overrun` は呼ばれない。見張りの
    /// スレッドの終わりを待つ（呼ばれていたら、その書き込みの終わりも）。
    pub fn finish(self) {
        let _ = self.finished.send(());
        let _ = self.thread.join();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn overrun_watch_fires_only_past_the_deadline() {
        let fired = Arc::new(AtomicBool::new(false));
        let flag = fired.clone();
        let watch = OverrunWatch::spawn(Instant::now() + Duration::from_secs(60), move || {
            flag.store(true, Ordering::SeqCst)
        });
        watch.finish();
        assert!(!fired.load(Ordering::SeqCst), "予算内なら警告しない");

        let fired = Arc::new(AtomicBool::new(false));
        let flag = fired.clone();
        let watch = OverrunWatch::spawn(Instant::now(), move || flag.store(true, Ordering::SeqCst));
        // 期限は過ぎている: 見張りは finish を待たずに警告し、終わる。
        let deadline = Instant::now() + Duration::from_secs(5);
        while !fired.load(Ordering::SeqCst) {
            assert!(Instant::now() < deadline, "期限を過ぎても警告しない");
            std::thread::yield_now();
        }
        watch.finish();
    }
}
