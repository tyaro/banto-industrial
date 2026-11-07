//! REST・取り込みの handler が切り離した副作用のタスクの数え（Hub に 1 つ、
//! [`crate::hub::CollectorManager::detached`]）。
//!
//! ## なぜ要るか
//!
//! hyper 1.x は応答を待つ間に相手の FIN を読むと、service の future（handler）を
//! 捨てる。そこで保存・失効・監査のように完結させたい副作用は `tokio::spawn` で
//! 切り離し、handler はその `JoinHandle` を待つ（docs/implementation-checklist.md §5
//! 「handler の中で `await` をまたぐ副作用は、接続の切断で捨てられる」）。ただし
//! 切り離したタスクは誰も持っていないので、クライアントが切れた後に Hub を止めると、
//! 監査の書き込みが終了処理の `pool.close()` と競って行が失われうる。
//!
//! ## 約束
//!
//! - 終了の冒頭（`RunningHub::shutdown_and_report` の `begin_shutdown` の直後）で
//!   [`DetachedTasks::close`] する。以後の [`DetachedTasks::spawn`] は spawn せずに
//!   `None` を返し、handler は何も保存・監査せずに 503（`crate::rest` の
//!   `shutting_down_response`）を返す。
//! - 起きてしまった事象の監査（取り込みの `denied` など）と、書込みの停止・再開
//!   （`WriteControl::set_enabled_detached`。終了中に停止を 503 にしない）は、断ると
//!   監査や停止が欠けるので、[`DetachedTasks::spawn_always`] で閉じた後も数える。
//! - 終了は HTTP サーバーを止めた後（新しい handler は来ない）・pool を閉じる前に、
//!   [`DetachedTasks::wait`] で上限つきで待つ。待ちきれなくても **abort しない**
//!   （DB を触るタスクを `abort()` で止めない、同 §5）。数はログに出す。
//! - タスクの終わり（panic でも）で数を減らす。0 になれば待ち手を起こす。
//!
//! 使う側: API キーの変更・OPC UA 設定の PUT・時計合わせの PUT（保存）と POST（手動
//! 実行、`crate::rest` の clock-sync）（`spawn`。終了中は 503）、取り込みの監査
//! （`crate::ingest::audit::record`）・取り込みの WS の接続ごとの監査の記録のタスク
//! （`crate::ingest::audit::IngestAuditWriter`。WS が開いている間ずっと数える。終了は冒頭で
//! WS を閉じさせる）と書込みの
//! 停止・再開（REST・MCP の `WriteControl::set_enabled_detached`）（`spawn_always`）。

use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tokio::sync::Notify;
use tokio::task::JoinHandle;

/// 切り離したタスクの数と受付。複製は同じ数えを指す（`Arc`）。
#[derive(Clone, Default)]
pub struct DetachedTasks(Arc<Inner>);

#[derive(Default)]
struct Inner {
    state: Mutex<State>,
    idle: Notify,
}

#[derive(Default)]
struct State {
    closed: bool,
    count: usize,
}

impl Inner {
    /// 中身は数と印だけなので、panic で毒が付いても読み書きを続ける。
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// タスクの終わり（panic で future が捨てられても）で数を減らし、0 なら待ち手を起こす。
struct Guard(Arc<Inner>);

impl Drop for Guard {
    fn drop(&mut self) {
        let mut state = self.0.state();
        state.count -= 1;
        if state.count == 0 {
            self.0.idle.notify_waiters();
        }
    }
}

impl DetachedTasks {
    pub fn new() -> Self {
        Self::default()
    }

    /// 数えて spawn する。受付を閉じた後（[`Self::close`]）は spawn せずに `None`
    /// （呼び出し側は何もせずに 503）。
    pub fn spawn<F>(&self, future: F) -> Option<JoinHandle<F::Output>>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        {
            let mut state = self.0.state();
            if state.closed {
                return None;
            }
            state.count += 1;
        }
        Some(self.spawn_counted(future))
    }

    /// 受付を閉じた後も数えて spawn する。起きてしまった事象の監査（断ると監査の行が
    /// 欠けるもの）に使う。終了の待ちより後に来たものは待たれないが、それでも
    /// 書き込みは試みる（失敗は `AuditLogService` が保留・ログに回す）。
    pub fn spawn_always<F>(&self, future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.0.state().count += 1;
        self.spawn_counted(future)
    }

    /// 数えた後の spawn。ガードは spawn の前に作る（spawn が panic しても数を戻す）。
    fn spawn_counted<F>(&self, future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let guard = Guard(self.0.clone());
        tokio::spawn(async move {
            let _guard = guard;
            future.await
        })
    }

    /// 受付を閉じる（終了の冒頭。以後の [`Self::spawn`] は `None`）。戻さない。
    pub fn close(&self) {
        self.0.state().closed = true;
    }

    /// 受付を閉じているか。
    pub fn is_closed(&self) -> bool {
        self.0.state().closed
    }

    /// 終わっていないタスクの数（終了の警告のログ用）。
    pub fn pending(&self) -> usize {
        self.0.state().count
    }

    /// 数えたタスクが全部終わるのを `limit` まで待つ。待ちきれなければ `false`
    /// （タスクは止めない）。
    pub async fn wait(&self, limit: Duration) -> bool {
        let inner = self.0.clone();
        tokio::time::timeout(limit, async move {
            loop {
                // 数を読む前に待ちを登録する（読んだ後・待つ前の 0 を取りこぼさない）。
                let notified = inner.idle.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if inner.state().count == 0 {
                    return;
                }
                notified.await;
            }
        })
        .await
        .is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn nothing_pending_is_waited_for_at_once() {
        let detached = DetachedTasks::new();
        assert_eq!(detached.pending(), 0);
        assert!(detached.wait(Duration::from_millis(10)).await);
    }

    /// 走っているタスクは待ちが待ち、受付を閉じた後は spawn しない。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn held_tasks_are_waited_for_and_refused_after_close() {
        let detached = DetachedTasks::new();
        let release = Arc::new(Notify::new());
        let held = release.clone();
        let task = detached
            .spawn(async move { held.notified().await })
            .expect("accepting");
        detached.close();
        assert!(detached.is_closed());
        assert!(detached.spawn(async {}).is_none(), "closed");
        assert_eq!(detached.pending(), 1);
        assert!(!detached.wait(Duration::from_millis(100)).await);
        release.notify_one();
        assert!(detached.wait(Duration::from_secs(5)).await);
        task.await.unwrap();
        assert_eq!(detached.pending(), 0);
    }

    /// panic したタスクも数を減らす（待ちは戻る）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_panicking_task_still_counts_down() {
        let detached = DetachedTasks::new();
        let task = detached
            .spawn(async { panic!("detached task panics") })
            .expect("accepting");
        assert!(task.await.unwrap_err().is_panic());
        assert!(detached.wait(Duration::from_secs(5)).await);
        assert_eq!(detached.pending(), 0);
    }

    /// `spawn_always` は閉じた後も数え、待ちはその終わりを待つ。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn spawn_always_is_counted_after_close() {
        let detached = DetachedTasks::new();
        detached.close();
        let release = Arc::new(Notify::new());
        let held = release.clone();
        let task = detached.spawn_always(async move {
            held.notified().await;
            7
        });
        assert_eq!(detached.pending(), 1);
        assert!(!detached.wait(Duration::from_millis(100)).await);
        release.notify_one();
        assert!(detached.wait(Duration::from_secs(5)).await);
        assert_eq!(task.await.unwrap(), 7);
    }

    /// 複製は同じ数えを指す。
    #[tokio::test]
    async fn clones_share_the_count() {
        let detached = DetachedTasks::new();
        let clone = detached.clone();
        clone.close();
        assert!(detached.is_closed());
        assert!(detached.spawn(async {}).is_none());
    }
}
