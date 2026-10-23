//! ランプ波つきのワイヤのシミュレータ(`simulator` feature)。
//!
//! R2b-5c(docs/banto-hub-driver-sim-design.md §6): 本番の
//! `simulation = true` の接続は値の層(`banto_driver_engine::sim` の
//! `SimDevice`)へ移り、`banto-collect` がワイヤのシミュレータを立てることは
//! なくなった。ここに残るのは、**ワイヤの先で値が動いていること**が要る所
//! (開発用 PLC `dev_plc`、ChronoGazer の「対照の実機」、値が動くことを見る
//! 統合テスト)のための、ランプ波の生成タスクつきの起動口だけ。
//!
//! ## 値生成
//!
//! 先頭 [`RAMP_ADDRESS_COUNT`] 個の番地を [`RAMP_PERIOD_MS`] ごとに書く。
//! ワード(Modbus の holding/input register、SLMP の `D`)は `u16` で
//! ラップするランプ、ビット(Modbus の coil/discrete input、SLMP の `M`)は
//! ワードの偶奇のトグル。外からワイヤ経由で書かれた番地は各シミュレータの
//! held 判定で `set_*` が no-op になるので、ランプは書かれた番地だけ止まり
//! 隣は動き続ける(各シミュレータの module doc「held set」)。
//!
//! ハンドルを [`RampHandle::stop`] するか drop すると、ランプのタスクが止まる。
//! drop ではランプのタスクを abort し、シミュレータの待受・接続タスクも
//! `abort_tasks` で止める(`stop` はランプを join してから止める)。

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::modbus::simulator::Simulator as ModbusSimulator;
use crate::slmp::simulator::Simulator as SlmpSimulator;
use crate::SlmpDevice;

/// 値生成の周期。
pub const RAMP_PERIOD_MS: u64 = 100;

/// ランプ波/トグルの対象アドレス数。
pub const RAMP_ADDRESS_COUNT: u16 = 16;

/// 立てるシミュレータのプロトコル。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RampProtocol {
    ModbusTcp,
    Slmp,
}

enum Inner {
    Modbus(Arc<ModbusSimulator>),
    Slmp(Arc<SlmpSimulator>),
}

/// 起動中の 1 シミュレータとそのランプのタスク。
pub struct RampHandle {
    addr: SocketAddr,
    ramp_stop: watch::Sender<bool>,
    ramp_task: Option<JoinHandle<()>>,
    inner: Option<Inner>,
}

impl RampHandle {
    /// 待受の loopback アドレス。
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// ランプのタスクを止めて join してから、シミュレータ自体を止める。
    pub async fn stop(mut self) {
        let _ = self.ramp_stop.send(true);
        if let Some(task) = self.ramp_task.take() {
            let _ = task.await;
        }
        match self.inner.take() {
            Some(Inner::Modbus(sim)) => Arc::try_unwrap(sim)
                .unwrap_or_else(|_| unreachable!("ramp task already joined - sole reference"))
                .stop(),
            Some(Inner::Slmp(sim)) => Arc::try_unwrap(sim)
                .unwrap_or_else(|_| unreachable!("ramp task already joined - sole reference"))
                .stop(),
            None => {}
        }
    }
}

impl Drop for RampHandle {
    fn drop(&mut self) {
        // `stop` を通らない drop でも、ランプのタスクと待受を止める
        // (`stop` で `inner` を取った後の drop は何もしない)。
        let _ = self.ramp_stop.send(true);
        if let Some(task) = &self.ramp_task {
            task.abort();
        }
        match &self.inner {
            Some(Inner::Modbus(sim)) => sim.abort_tasks(),
            Some(Inner::Slmp(sim)) => sim.abort_tasks(),
            None => {}
        }
    }
}

/// `protocol` のシミュレータを loopback の空きポートで起動し、ランプを回す。
/// bind の失敗は資源枯渇程度なので panic。
pub async fn start(protocol: RampProtocol) -> RampHandle {
    start_on(
        protocol,
        "127.0.0.1:0".parse().expect("valid loopback address"),
    )
    .await
    .expect("bind loopback listener")
}

/// [`start`] の、bind するアドレスを呼び出し側が決める版(`dev_plc` の固定
/// ポート)。固定ポートは使用中でありうるので bind の失敗は `io::Error`。
pub async fn start_on(protocol: RampProtocol, addr: SocketAddr) -> std::io::Result<RampHandle> {
    let (ramp_stop, stop_rx) = watch::channel(false);
    match protocol {
        RampProtocol::ModbusTcp => {
            let sim = Arc::new(ModbusSimulator::start_on(addr).await?);
            let addr = sim.addr;
            let task = tokio::spawn(modbus_ramp_task(sim.clone(), stop_rx));
            Ok(RampHandle {
                addr,
                ramp_stop,
                ramp_task: Some(task),
                inner: Some(Inner::Modbus(sim)),
            })
        }
        RampProtocol::Slmp => {
            let sim = Arc::new(SlmpSimulator::start_on(addr).await?);
            let addr = sim.addr;
            let task = tokio::spawn(slmp_ramp_task(sim.clone(), stop_rx));
            Ok(RampHandle {
                addr,
                ramp_stop,
                ramp_task: Some(task),
                inner: Some(Inner::Slmp(sim)),
            })
        }
    }
}

/// Modbus のランプ/トグル。holding/input register の両方に同じ値、coil/
/// discrete input にも同様(読み取り側がどの function code を使うかは番地の
/// 種別次第なので、全部面倒をみるのが最小で確実)。
async fn modbus_ramp_task(sim: Arc<ModbusSimulator>, mut stop_rx: watch::Receiver<bool>) {
    let mut tick: u16 = 0;
    loop {
        tokio::select! {
            _ = stop_rx.changed() => {
                if *stop_rx.borrow() {
                    return;
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(RAMP_PERIOD_MS)) => {
                tick = tick.wrapping_add(1);
                for offset in 0..RAMP_ADDRESS_COUNT {
                    let word = tick.wrapping_add(offset);
                    sim.set_holding_register(offset, word);
                    sim.set_input_register(offset, word);
                    let bit = word.is_multiple_of(2);
                    sim.set_coil(offset, bit);
                    sim.set_discrete_input(offset, bit);
                }
            }
        }
    }
}

/// SLMP のランプ/トグル。`D`(ワード)にランプ、`M`(ビット)にトグル。
async fn slmp_ramp_task(sim: Arc<SlmpSimulator>, mut stop_rx: watch::Receiver<bool>) {
    let mut tick: u16 = 0;
    loop {
        tokio::select! {
            _ = stop_rx.changed() => {
                if *stop_rx.borrow() {
                    return;
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(RAMP_PERIOD_MS)) => {
                tick = tick.wrapping_add(1);
                for offset in 0..u32::from(RAMP_ADDRESS_COUNT) {
                    let word = tick.wrapping_add(offset as u16);
                    sim.set_word(SlmpDevice::D, offset, word);
                    sim.set_bit(SlmpDevice::M, offset, word.is_multiple_of(2));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_modbus_ramp_moves_words_and_stops_when_told() {
        let sim = Arc::new(ModbusSimulator::start().await);
        let (stop_tx, stop_rx) = watch::channel(false);
        let task = tokio::spawn(modbus_ramp_task(sim.clone(), stop_rx));
        let first = sim.get_holding_register(0);
        let mut moved = false;
        for _ in 0..100 {
            tokio::time::sleep(Duration::from_millis(RAMP_PERIOD_MS)).await;
            if sim.get_holding_register(0) != first {
                moved = true;
                break;
            }
        }
        assert!(moved, "the ramp must change the first word");
        stop_tx.send(true).unwrap();
        task.await.unwrap();
        let frozen = sim.get_holding_register(0);
        tokio::time::sleep(Duration::from_millis(RAMP_PERIOD_MS * 3)).await;
        assert_eq!(sim.get_holding_register(0), frozen, "stopped ramp is quiet");
    }

    #[tokio::test]
    async fn a_dropped_handle_stops_the_ramp_task() {
        let handle = start(RampProtocol::Slmp).await;
        let task = {
            // `Drop` aborts the task; it must finish.
            let mut handle = handle;
            let task = handle.ramp_task.take().expect("task");
            drop(handle);
            task
        };
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("ramp task ends after the handle is dropped")
            .unwrap();
    }

    #[tokio::test]
    async fn a_dropped_handle_stops_the_listener() {
        let handle = start(RampProtocol::ModbusTcp).await;
        let addr = handle.addr();
        assert!(tokio::net::TcpStream::connect(addr).await.is_ok());
        drop(handle);
        let mut refused = false;
        for _ in 0..100 {
            if tokio::net::TcpStream::connect(addr).await.is_err() {
                refused = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(
            refused,
            "the listener must stop after the handle is dropped"
        );
    }
}
