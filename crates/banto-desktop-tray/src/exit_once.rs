//! 終了の後始末を**プロセスで 1 回だけ**走らせ、2 つ目以降の呼び出しには
//! **先行の後始末の完了を待たせる**入れ物（#586）。
//!
//! # なぜ要るか
//!
//! トレイ常駐のアプリには、後始末（収集の停止・最終 flush）へ入る口が 2 つある:
//!
//! - トレイの「終了」（確認の後、`AppHandle::exit` の**前**に後始末を待つ）。
//! - `tauri::RunEvent::Exit`。`exit` の後に必ず来るほか、**OS のサインアウト・
//!   シャットダウン**でも来る（tao は `WM_ENDSESSION` を受けるとイベントループを
//!   畳んで `Exit` を呼び、戻った直後に `std::process::exit` する）。
//!
//! 2 つが重なったとき（トレイの「終了」の後始末の最中に OS の終了が来た）に
//! 必要なのは、**後始末を二重に走らせない**ことと、**2 つ目が先行の完了を待つ**
//! ことの両方。2 つ目が「もう始まっている」だけを見てすぐ戻ると、`Exit` から
//! 戻った直後にプロセスが終わり、先行の後始末（最終 flush）が途中で切れる
//! （#586 受入条件 1）。
//!
//! # 予算（時間の上限）
//!
//! 待つ時間には必ず上限を付ける（受入条件 2）。OS の終了経路で無限に待つと、
//! Windows は「このアプリがシャットダウンを妨げています」を出して利用者を
//! 待たせ、最後は強制終了する。上限は呼び出しごとに渡し、
//!
//! - **最初の呼び出し**: 後始末の期限（`deadline = 今 + budget`）を決める。期限は
//!   後始末の本体（`work`）にも渡すので、本体の中の待ち（ロックの取得・
//!   進行中の停止の完了待ち）も同じ期限で打ち切れる。
//! - **2 つ目以降**: `min(先行の期限, 今 + 自分の budget)` まで待つ。トレイの
//!   「終了」が長めの予算で始めた後始末に、OS の終了が短い予算で合流しても、
//!   OS の側は自分の予算を超えて待たない。
//!
//! 本体は**専用のスレッド**で走らせ、呼び出し元はその完了を条件変数で待つ。
//! 呼び出し元のスレッドで本体を直接走らせないのは、本体が期限を守らずに
//! 固まった（ロックが返らない・下位の停止が応答しない）ときでも、呼び出し元は
//! 期限で戻れるようにするため。打ち切られても本体のスレッドは続く（直後に
//! プロセスが終わる）。
//!
//! 期限までに終わらなかったことは [`ExitCleanupOutcome::TimedOut`] で返し、
//! 正常に終わった [`ExitCleanupOutcome::Finished`] と区別する。記録（ログ）は
//! 呼び出し元が行う。
//!
//! # 持たないもの
//!
//! 後始末の中身（何をどの順で止めるか）はアプリごとに違うので、ここでは
//! `work` として受け取るだけ。Tauri にも依存しない（`std` だけ）ので、
//! Tauri を起動せずにスレッドで並行を組んでテストできる。

use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// [`ExitCleanupOnce::run_or_wait`] の結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExitCleanupOutcome<R> {
    /// 後始末が終わった（この呼び出しが始めたものでも、先行のものの完了を
    /// 待ったものでも同じ）。中身は後始末の本体が返した値。
    Finished(R),
    /// 予算内に終わらなかった - **これ以上待たずに戻った**。後始末の本体は
    /// 裏で続いている。正常終了と区別して記録すること。
    TimedOut,
}

enum Phase<R> {
    NotStarted,
    Running { deadline: Instant },
    Finished(R),
}

struct Inner<R> {
    phase: Mutex<Phase<R>>,
    finished: Condvar,
}

impl<R> Inner<R> {
    /// 中身は `Phase` 1 つで、ロック中に panic しても半端な値は残らない
    /// （書き換えは代入 1 回）ので、毒化は無視して中身を使う。
    fn lock(&self) -> MutexGuard<'_, Phase<R>> {
        self.phase
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// 終了の後始末を 1 回に限り、2 つ目以降には完了を待たせる（この module の doc）。
///
/// アプリの管理状態（`app.manage(..)`）として 1 つ持つ。
pub struct ExitCleanupOnce<R> {
    inner: Arc<Inner<R>>,
}

impl<R> Default for ExitCleanupOnce<R> {
    fn default() -> Self {
        Self::new()
    }
}

impl<R> ExitCleanupOnce<R> {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                phase: Mutex::new(Phase::NotStarted),
                finished: Condvar::new(),
            }),
        }
    }

    /// 後始末がもう始まっている（終わったものを含む）か。表示・テスト用。
    pub fn has_started(&self) -> bool {
        !matches!(*self.inner.lock(), Phase::NotStarted)
    }
}

impl<R: Clone + Send + 'static> ExitCleanupOnce<R> {
    /// 後始末がまだなら `work` を専用のスレッドで始め、どちらにしても
    /// 完了を**期限まで**待つ（この module の doc「予算」）。
    ///
    /// - `budget`: この呼び出しが待つ上限。最初の呼び出しなら、後始末の期限
    ///   （`work` に渡す `deadline`）にもなる。
    /// - `work`: 後始末の本体。最初の呼び出しの分だけが実行され、2 つ目以降に
    ///   渡したものは使われずに捨てられる。中で待つものがあれば、渡された
    ///   `deadline` で打ち切ること。
    ///
    /// `work` が panic したときは完了の印が付かないので、待つ側は期限で
    /// [`ExitCleanupOutcome::TimedOut`] を受け取る（終了は止まらない）。
    pub fn run_or_wait<F>(&self, budget: Duration, work: F) -> ExitCleanupOutcome<R>
    where
        F: FnOnce(Instant) -> R + Send + 'static,
    {
        let now = Instant::now();
        let wait_until = {
            let mut phase = self.inner.lock();
            match &*phase {
                Phase::Finished(result) => return ExitCleanupOutcome::Finished(result.clone()),
                Phase::Running { deadline } => (*deadline).min(now + budget),
                Phase::NotStarted => {
                    let deadline = now + budget;
                    *phase = Phase::Running { deadline };
                    let inner = Arc::clone(&self.inner);
                    // ロックを持ったままスレッドを作るだけ（本体はロックの外で
                    // 走る）。本体の完了は `finished` で知らせる。
                    std::thread::spawn(move || {
                        let result = work(deadline);
                        *inner.lock() = Phase::Finished(result);
                        inner.finished.notify_all();
                    });
                    deadline
                }
            }
        };
        self.wait_until(wait_until)
    }

    fn wait_until(&self, until: Instant) -> ExitCleanupOutcome<R> {
        let mut phase = self.inner.lock();
        loop {
            if let Phase::Finished(result) = &*phase {
                return ExitCleanupOutcome::Finished(result.clone());
            }
            let now = Instant::now();
            if now >= until {
                return ExitCleanupOutcome::TimedOut;
            }
            phase = self
                .inner
                .finished
                .wait_timeout(phase, until - now)
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
    use std::sync::mpsc;
    use std::sync::Barrier;

    const LONG: Duration = Duration::from_secs(10);

    /// 1 つ目の呼び出しが後始末を走らせ、完了を待って結果を返す。
    #[test]
    fn first_call_runs_the_work_and_waits_for_it() {
        let once = ExitCleanupOnce::new();
        assert!(!once.has_started());
        let outcome = once.run_or_wait(LONG, |_deadline| 7);
        assert_eq!(outcome, ExitCleanupOutcome::Finished(7));
        assert!(once.has_started());
    }

    /// 終わった後の呼び出しは、本体を走らせずに同じ結果を返す。
    #[test]
    fn calls_after_completion_return_the_same_result_without_rerunning() {
        let runs = Arc::new(AtomicUsize::new(0));
        let once = ExitCleanupOnce::new();
        for _ in 0..3 {
            let runs = Arc::clone(&runs);
            let outcome = once.run_or_wait(LONG, move |_| runs.fetch_add(1, SeqCst) + 1);
            assert_eq!(outcome, ExitCleanupOutcome::Finished(1));
        }
        assert_eq!(runs.load(SeqCst), 1);
    }

    /// 受入条件 1: 本体の最中に来た 2 つ目の呼び出しは、すぐ戻らず、先行の
    /// 完了を待ってから同じ結果を受け取る。本体は 1 回しか走らない。
    ///
    /// 決定的にするため、本体は「始まった」を知らせてから `release` を待つ。
    /// 2 つ目は本体が始まった後に呼び、`release` するまで戻らないことを
    /// 確かめてから放す。
    #[test]
    fn a_second_call_during_the_work_waits_for_its_completion() {
        let once = Arc::new(ExitCleanupOnce::new());
        let runs = Arc::new(AtomicUsize::new(0));
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();

        let first = {
            let once = Arc::clone(&once);
            let runs = Arc::clone(&runs);
            std::thread::spawn(move || {
                once.run_or_wait(LONG, move |_| {
                    runs.fetch_add(1, SeqCst);
                    started_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    "flushed"
                })
            })
        };
        started_rx.recv().unwrap();

        let (second_tx, second_rx) = mpsc::channel();
        let second = {
            let once = Arc::clone(&once);
            let runs = Arc::clone(&runs);
            std::thread::spawn(move || {
                let outcome = once.run_or_wait(LONG, move |_| {
                    runs.fetch_add(1, SeqCst);
                    "second work must not run"
                });
                second_tx.send(outcome.clone()).unwrap();
                outcome
            })
        };
        // 本体を放すまで 2 つ目は戻らない（戻ってしまう実装なら、ここで
        // 受け取れてしまう）。
        assert!(second_rx.recv_timeout(Duration::from_millis(200)).is_err());

        release_tx.send(()).unwrap();
        assert_eq!(
            first.join().unwrap(),
            ExitCleanupOutcome::Finished("flushed")
        );
        assert_eq!(
            second.join().unwrap(),
            ExitCleanupOutcome::Finished("flushed")
        );
        assert_eq!(runs.load(SeqCst), 1);
    }

    /// 2 つの呼び出しが同時に始まっても、本体は 1 回だけ走る（Barrier で
    /// 揃えて同時に入れる）。
    #[test]
    fn simultaneous_calls_run_the_work_once() {
        let once = Arc::new(ExitCleanupOnce::new());
        let runs = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(Barrier::new(2));
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let once = Arc::clone(&once);
                let runs = Arc::clone(&runs);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    once.run_or_wait(LONG, move |_| {
                        runs.fetch_add(1, SeqCst);
                    })
                })
            })
            .collect();
        for handle in handles {
            assert_eq!(handle.join().unwrap(), ExitCleanupOutcome::Finished(()));
        }
        assert_eq!(runs.load(SeqCst), 1);
    }

    /// 受入条件 2: 期限までに終わらなければ `TimedOut` で戻る（`Finished` とは
    /// 別の値）。本体はその後に終わってもよく、終わった後の呼び出しは結果を
    /// 受け取る。
    #[test]
    fn the_budget_bounds_the_wait_and_is_reported_as_timed_out() {
        let once = ExitCleanupOnce::new();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let (done_tx, done_rx) = mpsc::channel();
        let started = Instant::now();
        let outcome = once.run_or_wait(Duration::from_millis(50), move |_| {
            release_rx.recv().unwrap();
            done_tx.send(()).unwrap();
            1
        });
        assert_eq!(outcome, ExitCleanupOutcome::TimedOut);
        assert!(started.elapsed() < LONG);

        // 期限を過ぎた後の呼び出しは、本体が終わるまでは待たずに `TimedOut`。
        assert_eq!(once.run_or_wait(LONG, |_| 2), ExitCleanupOutcome::TimedOut);

        release_tx.send(()).unwrap();
        done_rx.recv().unwrap();
        // 本体の完了の記録は `done_tx` の送信の直後なので、記録されるまで
        // 読み直す（上限付き）。本体は 2 回目を走らせない（結果は 1 のまま）。
        let give_up = Instant::now() + LONG;
        loop {
            match once.run_or_wait(LONG, |_| 2) {
                ExitCleanupOutcome::Finished(value) => {
                    assert_eq!(value, 1);
                    break;
                }
                ExitCleanupOutcome::TimedOut => {
                    assert!(
                        Instant::now() < give_up,
                        "the work never recorded its result"
                    );
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        }
    }

    /// 2 つ目以降は `min(先行の期限, 今 + 自分の予算)` まで待つ。長い予算で
    /// 始まった後始末に短い予算で合流しても、自分の予算で戻る。
    #[test]
    fn a_later_caller_does_not_wait_beyond_its_own_budget() {
        let once = Arc::new(ExitCleanupOnce::new());
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let first = {
            let once = Arc::clone(&once);
            std::thread::spawn(move || {
                once.run_or_wait(LONG, move |_| {
                    started_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                })
            })
        };
        started_rx.recv().unwrap();
        let second = once.run_or_wait(Duration::from_millis(50), |_| ());
        assert_eq!(second, ExitCleanupOutcome::TimedOut);
        release_tx.send(()).unwrap();
        assert_eq!(first.join().unwrap(), ExitCleanupOutcome::Finished(()));
    }

    /// 本体には最初の呼び出しの期限が渡り、2 つ目以降の予算では延びない。
    #[test]
    fn the_work_receives_the_first_callers_deadline() {
        let once = ExitCleanupOnce::new();
        let before = Instant::now();
        let outcome = once.run_or_wait(Duration::from_secs(3), move |deadline| deadline);
        let ExitCleanupOutcome::Finished(deadline) = outcome else {
            panic!("expected Finished");
        };
        assert!(deadline >= before + Duration::from_secs(3));
        assert!(deadline <= Instant::now() + Duration::from_secs(3));
    }
}
