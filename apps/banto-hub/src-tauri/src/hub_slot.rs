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
//! # ホスト切替そのものも数える（#587 のレビュー、2026-10-12）
//!
//! 上の札は「Hub の起動・停止」しか数えないので、切替のスレッドが**まだ Hub に
//! 触っていない間**（SCM の `stop`/`start` を呼ぶ前後・状態の待ち）は終了から
//! 見えなかった。`run_host_switch` の「閉じていないか」の確認の後・最初の
//! `engine.step` の前に終了が閉じると、終了は空で落ち着いたと見てすぐ戻り、
//! その後で切替が `ServiceManager::stop` を呼んでデスクトップの Hub を起動する
//! 前にプロセスが終わりうる（サービスもアプリも止まる）。
//!
//! そこで切替全体を**別の数**（[`SwitchTicket`]、`switches`）で数える:
//!
//! - **登録**（[`HubSlot::begin_switch`]）: 切替のスレッドを spawn する**前に**、
//!   閉じた印の確認と同じロックの中で数を 1 増やす。閉じていれば `None` で、
//!   切替を始めない。
//! - **確定**（[`run_switch`]）: スレッドが最初の SCM 操作（`engine.step`）の
//!   **直前に**閉じた印をもう一度見る。閉じていれば SCM を一切呼ばずに終える。
//!   閉じていなければ、そこから先は**打ち切らずに完走させる**（終端の状態まで
//!   進め、結果の適用まで行う）。終了は札が返るのを同じ期限で待つ。
//! - **なぜ確定の後は打ち切らないか**: SCM を呼んだ後に途中で止めると、どちらの
//!   向きでも「サービスもアプリも止まる」状態に落ちうる（Desktop→Service は
//!   デスクトップの Hub を止めた後・サービスを `start` する前、Service→Desktop
//!   はサービスを `stop` した後）。Desktop→Service は完走すればサービスが
//!   収集を引き継ぐ。Service→Desktop は完走しても、終了の開始後はデスクトップの
//!   Hub を起動しない（[`HubSlot::begin_start`] が断る）ので、**サービスは止まり
//!   アプリも終わる** - 利用者が求めたサービスの停止の結果で、終了がサービスを
//!   開始し直すことはしない（シェルの終了は SCM を呼ばない境界）。
//! - **確定と閉じるの間**: 確定の確認とその後の `step` は 1 つのロックで
//!   囲めない（`step` は SCM の呼び出しやデスクトップの Hub の `shutdown` で
//!   長く掛かる）。確定の確認の直後に閉じられた場合は「確定の後」として扱う -
//!   終了は切替の札を待つので、SCM を呼んだ切替を見捨てることは無い。
//! - 切替の数は [`HubSlot::is_idle`]（切替の `is_stopped`）には**入れない**。
//!   入れると、Desktop→Service の切替が自分の札のせいで「デスクトップは
//!   止まった」に永遠に到達しない。終了の待ち（[`HubSlot::wait_idle_until`]）
//!   だけが両方を数える。
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
    /// 登録済みで、まだ終わっていないホスト切替の数（[`SwitchTicket`]）。
    switches: usize,
    closed: bool,
}

impl<H> SlotState<H> {
    /// 終了が待つものが無い（Hub の起動・停止も、ホスト切替も進行中でない）。
    fn settled(&self) -> bool {
        self.in_flight == 0 && self.switches == 0
    }
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
        if state.settled() {
            self.slot.idle.notify_all();
        }
    }
}

/// 登録済みのホスト切替が進行中であることを示す札（この module の doc
/// 「ホスト切替そのものも数える」）。切替のスレッドが、最初の SCM 操作の前から
/// 結果の適用の後まで持ち、drop で返す。
#[must_use = "札を落とすと、進行中の切替を終了が待たなくなる"]
pub(crate) struct SwitchTicket<H> {
    slot: Arc<HubSlot<H>>,
}

impl<H> Drop for SwitchTicket<H> {
    fn drop(&mut self) {
        let mut state = self.slot.lock();
        state.switches -= 1;
        if state.settled() {
            self.slot.idle.notify_all();
        }
    }
}

/// 登録済みの切替を、確定してから走らせる（この module の doc「ホスト切替
/// そのものも数える」）。
///
/// - 最初に閉じた印を見る（確定）。閉じていれば `drive` を**呼ばずに** `None`
///   （SCM を一切呼ばない）。
/// - 閉じていなければ `drive`（切替の駆動と結果の適用）を最後まで走らせて
///   `Some`。途中で閉じられても打ち切らない - 終了は札が返るのを待つ。
///
/// 札は `drive` が終わってから返す。
pub(crate) fn run_switch<H, R>(ticket: SwitchTicket<H>, drive: impl FnOnce() -> R) -> Option<R> {
    if ticket.slot.is_closed() {
        return None;
    }
    let result = drive();
    drop(ticket);
    Some(result)
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
                switches: 0,
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

    /// ホスト切替を終了の追跡に登録する（spawn する**前に**呼ぶ）。閉じていれば
    /// （終了が始まっていれば）`None` で、切替を始めない。登録は閉じた印の確認と
    /// 同じロックの中なので、「確認した後・登録する前に閉じられて、終了が切替を
    /// 知らずに戻る」隙間は無い。札は [`run_switch`] に渡す。
    pub(crate) fn begin_switch(self: &Arc<Self>) -> Option<SwitchTicket<H>> {
        let mut state = self.lock();
        if state.closed {
            return None;
        }
        state.switches += 1;
        Some(SwitchTicket {
            slot: Arc::clone(self),
        })
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
    /// 進行中のホスト切替（[`SwitchTicket`]）は数えない（この module の doc）。
    pub(crate) fn is_idle(&self) -> bool {
        let state = self.lock();
        state.hub.is_none() && state.in_flight == 0
    }

    /// 進行中の札（Hub の起動・停止と、登録済みのホスト切替の両方）が 0 になるのを
    /// `deadline` まで待つ。0 になれば `true`、期限が来れば `false`。
    pub(crate) fn wait_idle_until(&self, deadline: Instant) -> bool {
        let mut state = self.lock();
        loop {
            if state.settled() {
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
    /// 進行中の起動・停止・ホスト切替がすべて期限内に終わった。`false` なら期限で
    /// 待つのをやめた（正常終了と区別して記録する）。
    pub(crate) settled: bool,
}

/// 終了の後始末の本体（[`HubSlot`] の部分）。呼び出し元は先に
/// [`HubSlot::close`] を立ててから、これを `ExitCleanupOnce` の本体として渡す。
///
/// 1. 置かれている Hub を取り出して `shutdown` で止める（札を持ったまま）。
/// 2. 進行中の起動・停止と登録済みのホスト切替（切替のスレッドが持つ札）が
///    0 になるのを `deadline` まで待つ。閉じているので、進行中の起動は置けずに突き返され、起動した
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
    use std::sync::{mpsc, Barrier};
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
        once.run_or_wait((), budget, move |deadline| {
            run_hub_exit(&slot, deadline, |_hub| {
                stops.fetch_add(1, SeqCst);
            })
        })
        .outcome()
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
                once.run_or_wait((), LONG, move |deadline| {
                    run_hub_exit(&inner_slot, deadline, |_hub| {
                        stops.fetch_add(1, SeqCst);
                        in_shutdown_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                    })
                })
                .outcome()
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
        // 本体の待ち（`wait_idle_until`）と外側の待ち（`run_or_wait`）は同じ期限
        // なので、どちらが先に期限を見るかで `TimedOut` と
        // `Finished(settled: false)` のどちらにもなりうる（#587 のオーナー補足）。
        // 両方とも「正常完了ではない」ことと、上限内に戻ることだけを確かめる。
        let outcome = exit(&once, &slot, Duration::from_millis(50), &stops);
        assert!(
            matches!(
                outcome,
                ExitCleanupOutcome::TimedOut
                    | ExitCleanupOutcome::Finished(HubExitReport { settled: false, .. })
            ),
            "an exit over budget must not look like a clean finish: {outcome:?}"
        );
        assert!(started.elapsed() < LONG);

        // 本体の側も期限で待つのをやめる（札は返していない）。
        assert!(!slot.wait_idle_until(Instant::now()));
        drop(switch_ticket);
        assert!(slot.wait_idle_until(Instant::now() + LONG));
    }

    /// #587: 登録の後・最初の SCM 操作（`engine.step`）の前に終了が閉じた場合、
    /// 切替は SCM を一切呼ばずに終わり、終了はその札が返るのを待つ。
    ///
    /// `run_host_switch` の流れをそのまま組む: 呼び出し元のスレッドで登録
    /// （`begin_switch`）→ 切替のスレッドを起こす → Barrier で止めておき、
    /// その間に終了を始めて閉じる → 放すと `run_switch` が確定で断る。
    #[test]
    fn a_switch_closed_before_its_first_step_issues_no_scm_call() {
        let slot: Arc<HubSlot<FakeHub>> = HubSlot::new();
        let ticket = slot.begin_switch().expect("open slot accepts a switch");
        let scm_calls = Arc::new(AtomicUsize::new(0));
        let before_step = Arc::new(Barrier::new(2));
        let worker = {
            let scm_calls = Arc::clone(&scm_calls);
            let before_step = Arc::clone(&before_step);
            std::thread::spawn(move || {
                // `run_host_switch` の確認の後・最初の `step` の前。
                before_step.wait();
                run_switch(ticket, || {
                    scm_calls.fetch_add(1, SeqCst);
                })
            })
        };

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
        let give_up = Instant::now() + LONG;
        while !once.has_started() {
            assert!(Instant::now() < give_up, "the exit never started");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(slot.is_closed());
        // 切替の札が残っている間、終了は戻らない（登録の前は空に見えて戻っていた）。
        assert!(done_rx.recv_timeout(Duration::from_millis(200)).is_err());

        before_step.wait();
        assert_eq!(worker.join().unwrap(), None);
        assert_eq!(scm_calls.load(SeqCst), 0);
        assert_eq!(
            exiting.join().unwrap(),
            ExitCleanupOutcome::Finished(HubExitReport {
                stopped_hub: false,
                settled: true,
            })
        );
    }

    /// #587: 確定した後（最初の SCM 操作の最中）に終了が閉じた場合、切替は
    /// 打ち切らずに完走し、終了はその完了を待つ。
    #[test]
    fn a_switch_closed_during_its_steps_runs_to_completion_and_the_exit_waits() {
        let slot: Arc<HubSlot<FakeHub>> = HubSlot::new();
        let ticket = slot.begin_switch().expect("open slot accepts a switch");
        let (in_step_tx, in_step_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let worker = std::thread::spawn(move || {
            run_switch(ticket, move || {
                in_step_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                "completed"
            })
        });
        in_step_rx.recv().unwrap();

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
        release_tx.send(()).unwrap();

        assert_eq!(worker.join().unwrap(), Some("completed"));
        assert_eq!(
            exiting.join().unwrap(),
            ExitCleanupOutcome::Finished(HubExitReport {
                stopped_hub: false,
                settled: true,
            })
        );
    }

    /// 終了の開始後は切替を登録させない（spawn もしない）。
    #[test]
    fn a_closed_slot_refuses_to_register_a_switch() {
        let slot: Arc<HubSlot<FakeHub>> = HubSlot::new();
        slot.close();
        assert!(slot.begin_switch().is_none());
        assert!(slot.wait_idle_until(Instant::now()));
    }

    /// 切替の札は `is_idle`（切替の `is_stopped`）に入れない: Desktop→Service の
    /// 切替は自分の札を持ったまま「デスクトップは止まった」に到達できる。
    /// 終了の待ちだけが切替の札を数える。
    #[test]
    fn a_registered_switch_does_not_block_its_own_is_stopped() {
        let slot = slot_with_hub();
        let switch = slot.begin_switch().expect("open slot accepts a switch");
        let (hub, stop_ticket) = slot.take_for_stop().expect("hub installed");
        assert!(!slot.is_idle());
        assert_eq!(hub.0, 1);
        drop(stop_ticket);
        assert!(
            slot.is_idle(),
            "the switch's own ticket must not count here"
        );
        assert!(
            !slot.wait_idle_until(Instant::now()),
            "the exit still waits"
        );
        drop(switch);
        assert!(slot.wait_idle_until(Instant::now()));
    }

    /// 閉じる前のホスト切替は何度でもできる（停止・起動を 1 回に制限しない）。
    #[test]
    fn switches_before_exit_are_not_limited() {
        let slot: Arc<HubSlot<FakeHub>> = HubSlot::new();
        for round in 0..3 {
            let switch = slot.begin_switch().expect("open slot accepts a switch");
            let ran = run_switch(switch, || {
                let ticket = slot.begin_start().expect("open slot accepts a start");
                assert!(slot.install(FakeHub(round), ticket).is_ok());
                let (hub, ticket) = slot.take_for_stop().expect("installed");
                assert_eq!(hub.0, round);
                drop(ticket);
            });
            assert_eq!(ran, Some(()));
            assert!(slot.is_idle());
            assert!(slot.wait_idle_until(Instant::now()));
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
