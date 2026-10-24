//! サービスで起きる古いログの削除の**全部**を通す口（#623 PR-3 の後追い。Codex の P1・
//! オーナーレビューの P2）。
//!
//! 削除は起動時の `ServerLog::set_retention` の 1 件だけでなく、サービスが日付をまたいで
//! 動くと `ServerLog::write_line` の日付の切り替えでも起きる（向け直しのポンプ
//! `chronogazer_core::stdio_log` の `pump` と `StdioRedirect::write_through`）。削除は握った `logs\` の子を名前で
//! 開いて消すので（`ManagedDir::remove_where`）、STOP で ACL を緩め始めた後に重なると
//! 約束 2「緩め始めたら新しく開かない」に反する。そこでサービスの中の削除はすべて
//! [`LogPrunes::spawn`] で走らせ、STOP は
//!
//! 1. [`LogPrunes::seal`] で以後の削除を**走らせずに捨てる**ようにし（停止の最後の行や
//!    向け直しの残りの書き出しが日付の切り替えを起こしても、新しい削除は始まらない。
//!    消し損ねた古いファイルは次の起動の `set_retention` が消す）、
//! 2. [`LogPrunes::wait_idle`] で走っている全部の終わりを上限つきで待ち（終わらなければ
//!    緩めずに締めたまま 5）、
//! 3. 最後の行 → `close_log`（以後 `write_line` は何もしない）の後に、もう一度
//!    [`LogPrunes::wait_idle`] で残りが無いことを確かめてから緩める。
//!
//! コンソールの経路（`crate::spawn_log_prune`）は追跡しない: コンソールの終了は ACL を
//! 緩めず（緩める木が無い）、削除はプロセスの終わりで打ち切られてよいため。

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// 削除の 1 回分（本番は `LogPrune::run`。テストは偽物）。
pub(crate) type PruneJob = Box<dyn FnOnce() + Send>;

/// テスト専用: 各削除の直前に呼ぶ処理（削除を保留して STOP と重ねるため）。
#[cfg(test)]
pub(crate) type BeforeRun = Arc<dyn Fn() + Send + Sync>;

#[derive(Default)]
struct State {
    /// [`LogPrunes::seal`] の後か（以後の削除は走らせない）。
    sealed: bool,
    /// 走っている削除の数。
    running: usize,
}

/// 追跡する削除の口（モジュール doc）。
#[derive(Default)]
pub(crate) struct LogPrunes {
    state: Mutex<State>,
    idle: Condvar,
    #[cfg(test)]
    before_run: Mutex<Option<BeforeRun>>,
}

/// 走っている数を、削除の終わり（panic を含む）で 1 つ減らす。
struct Running(Arc<LogPrunes>);

impl Drop for Running {
    fn drop(&mut self) {
        let mut state = self.0.lock();
        state.running -= 1;
        if state.running == 0 {
            self.0.idle.notify_all();
        }
    }
}

impl LogPrunes {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// `job` を別のスレッドで走らせ、終わりを追跡する。封じた後（[`Self::seal`]）と
    /// スレッドを作れないときは走らせない（今回は消さない）。走らせたら `true`。
    pub(crate) fn spawn(self: &Arc<Self>, job: PruneJob) -> bool {
        {
            let mut state = self.lock();
            if state.sealed {
                return false;
            }
            state.running += 1;
        }
        let running = Running(Arc::clone(self));
        #[cfg(test)]
        let before = self
            .before_run
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        std::thread::Builder::new()
            .name("chronogazer-log-prune".to_string())
            .spawn(move || {
                let _running = running;
                #[cfg(test)]
                if let Some(before) = before {
                    before();
                }
                job();
            })
            // 作れなければ閉包ごと（`running` も）落ち、数は戻る。
            .is_ok()
    }

    /// 以後の [`Self::spawn`] を走らせずに捨てる（STOP で緩める前）。
    pub(crate) fn seal(&self) {
        self.lock().sealed = true;
    }

    /// 走っている削除が無くなるのを `cap` まで待つ。無くなれば `true`、終わらなければ
    /// `false`（呼ぶ側は緩めずに終わる）。待つ間は `PROGRESS_INTERVAL`（`tick_every`）
    /// ごとに `tick`（STOP_PENDING の checkpoint）。
    pub(crate) fn wait_idle(
        &self,
        cap: Duration,
        tick_every: Duration,
        tick: &mut dyn FnMut(),
    ) -> bool {
        let deadline = Instant::now() + cap;
        let mut last_tick = Instant::now();
        let mut state = self.lock();
        while state.running > 0 {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            if now.duration_since(last_tick) >= tick_every {
                last_tick = now;
                drop(state);
                tick();
                state = self.lock();
                continue;
            }
            let slice = (deadline - now).min(tick_every.saturating_sub(now - last_tick));
            state = self
                .idle
                .wait_timeout(state, slice.max(Duration::from_millis(1)))
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
        true
    }

    /// テスト専用: 各削除の直前に呼ぶ処理を差し替える。
    #[cfg(test)]
    pub(crate) fn set_before_run(&self, before: Option<BeforeRun>) {
        *self.before_run.lock().unwrap_or_else(|p| p.into_inner()) = before;
    }

    /// テスト専用: 走っている削除の数。
    #[cfg(test)]
    pub(crate) fn running(&self) -> usize {
        self.lock().running
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    const TICK: Duration = Duration::from_millis(50);

    #[test]
    fn wait_idle_waits_for_every_spawned_job() {
        let prunes = Arc::new(LogPrunes::default());
        let (tx, rx) = mpsc::channel::<()>();
        let rx = Arc::new(Mutex::new(rx));
        for _ in 0..2 {
            let rx = Arc::clone(&rx);
            assert!(prunes.spawn(Box::new(move || {
                let _ = rx.lock().unwrap().recv_timeout(Duration::from_secs(10));
            })));
        }
        assert!(!prunes.wait_idle(Duration::from_millis(200), TICK, &mut || {}));
        tx.send(()).unwrap();
        assert!(!prunes.wait_idle(Duration::from_millis(200), TICK, &mut || {}));
        tx.send(()).unwrap();
        assert!(prunes.wait_idle(Duration::from_secs(5), TICK, &mut || {}));
        assert_eq!(prunes.running(), 0);
    }

    #[test]
    fn a_sealed_tracker_drops_new_jobs_without_running_them() {
        let prunes = Arc::new(LogPrunes::default());
        prunes.seal();
        let ran = Arc::new(Mutex::new(false));
        let flag = Arc::clone(&ran);
        assert!(!prunes.spawn(Box::new(move || *flag.lock().unwrap() = true)));
        assert!(prunes.wait_idle(Duration::ZERO, TICK, &mut || {}));
        std::thread::sleep(Duration::from_millis(100));
        assert!(!*ran.lock().unwrap());
    }

    #[test]
    fn a_panicking_job_still_counts_as_finished() {
        let prunes = Arc::new(LogPrunes::default());
        assert!(prunes.spawn(Box::new(|| panic!("削除の panic（テスト）"))));
        assert!(prunes.wait_idle(Duration::from_secs(5), TICK, &mut || {}));
    }
}
