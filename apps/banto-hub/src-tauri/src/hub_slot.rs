//! デスクトップホストの Hub（[`banto_hub_core::runtime::RunningHub`]）の置き場所と、
//! その起動・停止の「進行中」の数え方（#586）。
//!
//! # なぜ `Option<RunningHub>` だけでは足りないか
//!
//! シェルの終了（トレイの「アプリを終了」・OS のサインアウト／シャットダウン）が
//! 「Hub が置かれていない（`None`）なら止めるものは無い」と判断すると、次の
//! 2 つを取りこぼす（#586 受入条件 3、オーナーのレビュー）:
//!
//! - **進行中の停止**: ホスト切替（Desktop→Service）の
//!   `ShellDesktopControl::request_stop` は Hub を**取り出してから**別スレッドで
//!   `shutdown` を待つ。取り出した瞬間から置き場所は `None` だが、最終 flush は
//!   まだ終わっていない。終了がそれを待たずにプロセスを畳むと、flush が途中で
//!   切れる。
//! - **進行中の起動**: 切替結果の適用（Service→Desktop）や「再試行」は
//!   `HubRuntime::start` で**新しい Hub を起動しうる**。終了の後にそれが
//!   置き場所へ入ると、誰も止めないままプロセスが終わる（収集の停止・最終
//!   flush を通らない）。
//!
//! そこで、置き場所と同じロックの下で、
//!
//! - **進行中の数**（[`InFlight`] の札の数）: 起動を始める・停止のために
//!   取り出すときに札を 1 枚取り、終わったら返す（drop）。
//! - **閉じた印**（`closed`）: 終了の開始で立て、**誰も下ろさない**。
//!   立った後は新しい起動を始めさせず（[`HubSlot::begin_start`] が `None`）、
//!   起動し終えた Hub も置かせない（[`HubSlot::install`] が突き返す。突き返された
//!   側は自分で `shutdown` する）。
//!
//! を持つ。終了は「閉じる → 置かれている Hub を取り出して止める → 進行中の札が
//! 0 になるのを期限まで待つ」（[`run_hub_exit`]）で、切替の途中の停止も起動も
//! 同じ終了の期限で待つ。
//!
//! # 1 回に限るのは終了だけ
//!
//! ホスト切替の停止・起動は何度でも起きる（デスクトップ ↔ サービスを
//! 往復できる）ので、ここでは回数を制限しない。プロセスで 1 回に限るのは
//! 終了の後始末だけで、それは `banto_desktop_tray::ExitCleanupOnce` が持つ。
//!
//! # 型を Hub に固定しない理由
//!
//! `RunningHub` は実際に DB・ポート・PLC 接続を持つので、テストで作るのが重い。
//! 置き場所の状態機械は Hub の中身に関係しないので `H` で受け、テストでは
//! 軽い値で並行を組む（判断を純関数・小さな状態機械に出す、の規律）。

use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Instant;

struct SlotState<H> {
    hub: Option<H>,
    in_flight: usize,
    closed: bool,
}

/// デスクトップホストの Hub の置き場所（この module の doc）。
pub(crate) struct HubSlot<H> {
    state: Mutex<SlotState<H>>,
    idle: Condvar,
}

/// 起動・停止が進行中であることを示す札。drop で返す（進行中の数を 1 減らし、
/// [`HubSlot::wait_idle_until`] を起こす）。
#[must_use = "札を落とすと、進行中の起動・停止を終了が待たなくなる"]
pub(crate) struct InFlight<H> {
    slot: Arc<HubSlot<H>>,
}

impl<H> Drop for InFlight<H> {
    fn drop(&mut self) {
        let mut state = self.slot.lock();
        state.in_flight -= 1;
        if state.in_flight == 0 {
            self.slot.idle.notify_all();
        }
    }
}

/// [`HubSlot::install`] が突き返した Hub と、その札。呼び出し元は Hub を
/// `shutdown` してから札を落とす（終了はそれを待つ）。
pub(crate) struct Rejected<H> {
    pub(crate) hub: H,
    pub(crate) ticket: InFlight<H>,
}

impl<H> HubSlot<H> {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(SlotState {
                hub: None,
                in_flight: 0,
                closed: false,
            }),
            idle: Condvar::new(),
        })
    }

    /// 中身は `Option`・数・印だけで、書き換えの途中で panic しても終了を
    /// 止める理由にはならないので、毒化は無視して中身を使う。
    fn lock(&self) -> MutexGuard<'_, SlotState<H>> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn ticket(self: &Arc<Self>, state: &mut SlotState<H>) -> InFlight<H> {
        state.in_flight += 1;
        InFlight {
            slot: Arc::clone(self),
        }
    }

    /// 新しい Hub の起動を始めてよいか。閉じていれば（終了が始まっていれば）
    /// `None` - 起動しない。よければ札を返すので、起動し終えたら
    /// [`Self::install`] に渡す（起動に失敗したら札を落とすだけ）。
    pub(crate) fn begin_start(self: &Arc<Self>) -> Option<InFlight<H>> {
        let mut state = self.lock();
        if state.closed {
            return None;
        }
        Some(self.ticket(&mut state))
    }

    /// 起動し終えた Hub を置く。閉じている、または既に別の Hub が置かれて
    /// いるときは置かずに突き返す（呼び出し元が `shutdown` する）。
    ///
    /// 札はロックを放してから落とす（`Drop` が同じロックを取るため）。
    pub(crate) fn install(&self, hub: H, ticket: InFlight<H>) -> Result<(), Rejected<H>> {
        {
            let mut state = self.lock();
            if state.closed || state.hub.is_some() {
                drop(state);
                return Err(Rejected { hub, ticket });
            }
            state.hub = Some(hub);
        }
        drop(ticket);
        Ok(())
    }

    /// 停止のために Hub を取り出す。取り出すのと同じロックの中で札を取るので、
    /// 「置き場所は空なのに、まだ止めている最中」の間も終了から見える。
    /// 閉じていても取り出せる（停止は終了の後でも妨げない）。
    pub(crate) fn take_for_stop(self: &Arc<Self>) -> Option<(H, InFlight<H>)> {
        let mut state = self.lock();
        let hub = state.hub.take()?;
        let ticket = self.ticket(&mut state);
        Some((hub, ticket))
    }

    /// 閉じた印を立てる（終了の開始。冪等、下ろす口は無い）。
    pub(crate) fn close(&self) {
        self.lock().closed = true;
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.lock().closed
    }

    /// Hub が置かれておらず、進行中の起動・停止も無い（= このプロセスの
    /// デスクトップの Hub は止まりきっている）。ホスト切替の
    /// `DesktopHostControl::is_stopped` が使う - `None` を見ただけでは、
    /// 取り出された Hub の `shutdown` がまだ終わっていないことがある。
    pub(crate) fn is_idle(&self) -> bool {
        let state = self.lock();
        state.hub.is_none() && state.in_flight == 0
    }

    /// 進行中の札が 0 になるのを `deadline` まで待つ。0 になれば `true`、期限が
    /// 来れば `false`。
    pub(crate) fn wait_idle_until(&self, deadline: Instant) -> bool {
        let mut state = self.lock();
        loop {
            if state.in_flight == 0 {
                return true;
            }
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            state = self
                .idle
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .0;
        }
    }
}

/// [`run_hub_exit`] の結果（終了の後始末の本体が返す値）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HubExitReport {
    /// 置き場所にあった Hub をこの後始末が止めた（`false` はサービス接続中・
    /// fallback、または切替が取り出し済みだった）。
    pub(crate) stopped_hub: bool,
    /// 進行中の起動・停止がすべて期限内に終わった。`false` なら期限で
    /// 待つのをやめた（正常終了と区別して記録する）。
    pub(crate) settled: bool,
}

/// 終了の後始末の本体（[`HubSlot`] の部分）。呼び出し元は先に
/// [`HubSlot::close`] を立ててから、これを `ExitCleanupOnce` の本体として渡す。
///
/// 1. 置かれている Hub を取り出して `shutdown` で止める（札を持ったまま）。
/// 2. 進行中の起動・停止（切替のスレッドが持つ札）が 0 になるのを `deadline`
///    まで待つ。閉じているので、進行中の起動は置けずに突き返され、起動した
///    側が自分で止めてから札を返す。
///
/// 閉じるのはここではなく呼び出し元（待たずに、すぐ立てる）。ここで初めて
/// 閉じると、後始末のスレッドが動き出すまでの間に切替が Hub を置けてしまう。
pub(crate) fn run_hub_exit<H>(
    slot: &Arc<HubSlot<H>>,
    deadline: Instant,
    shutdown: impl FnOnce(H),
) -> HubExitReport {
    let stopped_hub = match slot.take_for_stop() {
        Some((hub, ticket)) => {
            shutdown(hub);
            drop(ticket);
            true
        }
        None => false,
    };
    let settled = slot.wait_idle_until(deadline);
    HubExitReport {
        stopped_hub,
        settled,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use banto_desktop_tray::{ExitCleanupOnce, ExitCleanupOutcome};
    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
    use std::sync::mpsc;
    use std::time::Duration;

    const LONG: Duration = Duration::from_secs(10);

    /// テスト用の Hub。止められた回数を数える。
    #[derive(Debug)]
    struct FakeHub(u32);

    /// シェルの終了と同じ順序: 閉じてから、1 回に限った後始末の本体として
    /// [`run_hub_exit`] を渡す（`lib.rs` の `exit_cleanup`）。
    fn exit(
        once: &ExitCleanupOnce<HubExitReport>,
        slot: &Arc<HubSlot<FakeHub>>,
        budget: Duration,
        stops: &Arc<AtomicUsize>,
    ) -> ExitCleanupOutcome<HubExitReport> {
        slot.close();
        let slot = Arc::clone(slot);
        let stops = Arc::clone(stops);
        once.run_or_wait(budget, move |deadline| {
            run_hub_exit(&slot, deadline, |_hub| {
                stops.fetch_add(1, SeqCst);
            })
        })
    }

    fn slot_with_hub() -> Arc<HubSlot<FakeHub>> {
        let slot = HubSlot::new();
        let ticket = slot.begin_start().expect("open slot accepts a start");
        assert!(slot.install(FakeHub(1), ticket).is_ok());
        slot
    }

    /// 通常のトレイ終了（デスクトップホスト）: 置かれている Hub を 1 回止め、
    /// 進行中のものは無いので期限内に落ち着く。
    #[test]
    fn a_plain_exit_stops_the_installed_hub_once() {
        let slot = slot_with_hub();
        let stops = Arc::new(AtomicUsize::new(0));
        let once = ExitCleanupOnce::new();
        assert_eq!(
            exit(&once, &slot, LONG, &stops),
            ExitCleanupOutcome::Finished(HubExitReport {
                stopped_hub: true,
                settled: true,
            })
        );
        assert_eq!(stops.load(SeqCst), 1);
        assert!(slot.is_idle());
    }

    /// サービス接続中・fallback（このプロセスに Hub が無い）: 止めるものは
    /// 無く、すぐ落ち着く。サービスへは何もしない（この型はサービスを知らない。
    /// SCM の停止を発行しない境界は `lib.rs` の `exit_cleanup` の doc）。
    #[test]
    fn an_exit_without_a_hub_has_nothing_to_stop() {
        let slot: Arc<HubSlot<FakeHub>> = HubSlot::new();
        let stops = Arc::new(AtomicUsize::new(0));
        let once = ExitCleanupOnce::new();
        assert_eq!(
            exit(&once, &slot, LONG, &stops),
            ExitCleanupOutcome::Finished(HubExitReport {
                stopped_hub: false,
                settled: true,
            })
        );
        assert_eq!(stops.load(SeqCst), 0);
    }

    /// トレイ終了と OS 終了の重複: 1 つ目の後始末の `shutdown` の最中に
    /// 2 つ目（OS の `RunEvent::Exit`）が来ても、Hub は 1 回しか止めず、
    /// 2 つ目は 1 つ目の完了を待ってから同じ結果を受け取る。
    #[test]
    fn tray_quit_and_os_exit_overlap_stop_the_hub_once_and_both_wait() {
        let slot = slot_with_hub();
        let stops = Arc::new(AtomicUsize::new(0));
        let once = Arc::new(ExitCleanupOnce::new());
        let (in_shutdown_tx, in_shutdown_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();

        let tray = {
            let once = Arc::clone(&once);
            let slot = Arc::clone(&slot);
            let stops = Arc::clone(&stops);
            std::thread::spawn(move || {
                slot.close();
                let inner_slot = Arc::clone(&slot);
                once.run_or_wait(LONG, move |deadline| {
                    run_hub_exit(&inner_slot, deadline, |_hub| {
                        stops.fetch_add(1, SeqCst);
                        in_shutdown_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                    })
                })
            })
        };
        in_shutdown_rx.recv().unwrap();

        let (os_tx, os_rx) = mpsc::channel();
        let os = {
            let once = Arc::clone(&once);
            let slot = Arc::clone(&slot);
            let stops = Arc::clone(&stops);
            std::thread::spawn(move || {
                let outcome = exit(&once, &slot, LONG, &stops);
                os_tx.send(()).unwrap();
                outcome
            })
        };
        // 1 つ目の `shutdown` を放すまで、OS の側は戻らない。
        assert!(os_rx.recv_timeout(Duration::from_millis(200)).is_err());
        release_tx.send(()).unwrap();

        let expected = ExitCleanupOutcome::Finished(HubExitReport {
            stopped_hub: true,
            settled: true,
        });
        assert_eq!(tray.join().unwrap(), expected);
        assert_eq!(os.join().unwrap(), expected);
        assert_eq!(stops.load(SeqCst), 1);
    }

    /// 切替中の終了（Desktop→Service の途中）: 切替が Hub を取り出して止めて
    /// いる最中は、置き場所は空でも終了は**その停止の完了を待つ**。
    #[test]
    fn an_exit_during_a_switch_waits_for_the_in_flight_stop() {
        let slot = slot_with_hub();
        // 切替（`ShellDesktopControl::request_stop`）が取り出した。
        let (hub, switch_ticket) = slot.take_for_stop().expect("hub installed");
        assert!(!slot.is_idle(), "a taken hub is still being stopped");

        let stops = Arc::new(AtomicUsize::new(0));
        let once = Arc::new(ExitCleanupOnce::new());
        let (done_tx, done_rx) = mpsc::channel();
        let exiting = {
            let once = Arc::clone(&once);
            let slot = Arc::clone(&slot);
            let stops = Arc::clone(&stops);
            std::thread::spawn(move || {
                let outcome = exit(&once, &slot, LONG, &stops);
                done_tx.send(()).unwrap();
                outcome
            })
        };
        // 切替の停止が終わるまで、終了は戻らない。
        assert!(done_rx.recv_timeout(Duration::from_millis(200)).is_err());
        // 切替のスレッドが `shutdown` を終えて札を返す。
        assert_eq!(hub.0, 1);
        drop(switch_ticket);

        assert_eq!(
            exiting.join().unwrap(),
            ExitCleanupOutcome::Finished(HubExitReport {
                stopped_hub: false,
                settled: true,
            })
        );
        // 終了自身は止めていない（切替が止めた）。
        assert_eq!(stops.load(SeqCst), 0);
        assert!(slot.is_idle());
    }

    /// 切替中の終了（Service→Desktop の途中）: 終了の前に始まった起動は、
    /// 起動し終えても置けずに突き返され、起動した側が止める。終了はそれを待つ。
    /// 終了の後の起動は始めさせない。
    #[test]
    fn a_start_in_flight_at_exit_is_rejected_and_stopped_by_its_starter() {
        let slot: Arc<HubSlot<FakeHub>> = HubSlot::new();
        let start_ticket = slot.begin_start().expect("open slot accepts a start");

        let stops = Arc::new(AtomicUsize::new(0));
        let once = Arc::new(ExitCleanupOnce::new());
        let (done_tx, done_rx) = mpsc::channel();
        let exiting = {
            let once = Arc::clone(&once);
            let slot = Arc::clone(&slot);
            let stops = Arc::clone(&stops);
            std::thread::spawn(move || {
                let outcome = exit(&once, &slot, LONG, &stops);
                done_tx.send(()).unwrap();
                outcome
            })
        };
        assert!(done_rx.recv_timeout(Duration::from_millis(200)).is_err());
        // 終了は閉じてから後始末を始める（`exit`）ので、始まっていれば閉じている。
        let give_up = Instant::now() + LONG;
        while !once.has_started() {
            assert!(Instant::now() < give_up, "the exit never started");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(slot.is_closed());

        // 新しい起動は始められない。
        assert!(slot.begin_start().is_none());
        // 進行中だった起動は置けない - 突き返された Hub を起動した側が止める。
        let rejected = match slot.install(FakeHub(2), start_ticket) {
            Ok(()) => panic!("a closed slot must not accept a new hub"),
            Err(rejected) => rejected,
        };
        // 起動した側が突き返された Hub を止め終えて、札を返す。
        assert_eq!(rejected.hub.0, 2);
        drop(rejected.ticket);

        assert_eq!(
            exiting.join().unwrap(),
            ExitCleanupOutcome::Finished(HubExitReport {
                stopped_hub: false,
                settled: true,
            })
        );
        assert!(slot.is_idle());
    }

    /// 予算超過: 進行中の停止が期限までに終わらなければ、待つのをやめて
    /// `TimedOut`（呼び出し元の待ち）・`settled: false`（本体の待ち）になる -
    /// どちらも正常終了の値とは別。
    #[test]
    fn an_exit_over_budget_is_reported_and_does_not_hang() {
        let slot = slot_with_hub();
        let (_hub, switch_ticket) = slot.take_for_stop().expect("hub installed");
        let stops = Arc::new(AtomicUsize::new(0));
        let once = ExitCleanupOnce::new();
        let started = Instant::now();
        assert_eq!(
            exit(&once, &slot, Duration::from_millis(50), &stops),
            ExitCleanupOutcome::TimedOut
        );
        assert!(started.elapsed() < LONG);

        // 本体の側も期限で待つのをやめる（札は返していない）。
        assert!(!slot.wait_idle_until(Instant::now()));
        drop(switch_ticket);
        assert!(slot.wait_idle_until(Instant::now() + LONG));
    }

    /// 閉じる前のホスト切替は何度でもできる（停止・起動を 1 回に制限しない）。
    #[test]
    fn switches_before_exit_are_not_limited() {
        let slot: Arc<HubSlot<FakeHub>> = HubSlot::new();
        for round in 0..3 {
            let ticket = slot.begin_start().expect("open slot accepts a start");
            assert!(slot.install(FakeHub(round), ticket).is_ok());
            let (hub, ticket) = slot.take_for_stop().expect("installed");
            assert_eq!(hub.0, round);
            drop(ticket);
            assert!(slot.is_idle());
        }
        assert!(!slot.is_closed());
    }

    /// 置き場所が埋まっているときに別の Hub を置こうとしたら、上書きせずに
    /// 突き返す（上書きすると前の Hub を止めずに捨てる）。
    #[test]
    fn an_occupied_slot_rejects_a_second_hub() {
        let slot = slot_with_hub();
        let ticket = slot.begin_start().expect("open slot accepts a start");
        let rejected = match slot.install(FakeHub(2), ticket) {
            Ok(()) => panic!("an occupied slot must not be overwritten"),
            Err(rejected) => rejected,
        };
        assert_eq!(rejected.hub.0, 2);
        drop(rejected.ticket);
        let (hub, _ticket) = slot.take_for_stop().expect("first hub stays");
        assert_eq!(hub.0, 1);
    }
}
