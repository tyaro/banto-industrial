//! T9-1 (docs/ux-plan.md §1, 2026-08-06 オーナー決定): in-process シミュレータの
//! 起動・値生成・ライフサイクル管理 -「接続単位のシミュレーションモード」の
//! 収集層側の実体。`simulation = true` の接続は、実 PLC の代わりにここで
//! 起動する `banto_plc` の Modbus/SLMP シミュレータ(実 TCP・実バイト列、
//! `banto-plc/src/{modbus,slmp}/simulator.rs`)へ接続する - ux-plan.md §1の
//! 決定どおり、収集エンジン・クライアントのコードパス自体(`crate::task`)は
//! 一切変更しない。シミュレータは実ソケットを持つ本物の TCP サーバーなので、
//! `crate::task::default_client_factory`が組み立てる`ModbusTcpClient`/
//! `SlmpClient`は、相手が実機かシミュレータか区別できない(区別する必要も
//! ない)。
//!
//! ## ライフサイクルは接続タスクと同じ(オーナー決定)
//!
//! シミュレータは `crate::collector::Collector` が `tasks`(接続毎の収集
//! タスク)と同じ寿命で管理する - 接続タスクを spawn する直前に起動し、
//! 停止/入れ替え(`apply_config` の removed/replaced 分類、または
//! `Collector::stop`)の際にタスクと一緒に止める。`crate::config::ConnectionPlan::simulation`
//! を診断対象の `PartialEq` に含めたことで、simulation フラグの on/off
//! 切り替え自体が「この接続は変わった」という T7-1 の通常の再構成経路に
//! そのまま乗る(`crate::collector`に専用の分岐は不要 - 通常の
//! stop-then-spawn の中でシミュレータの stop/start が一緒に起こるだけ)。
//!
//! ## 値生成: ランプ波(ux-plan.md §1)
//!
//! アドレス毎に単調増加(`u16` ラップアラウンド)する値を [`RAMP_PERIOD_MS`]
//! 間隔で書き込み続ける([`apps/banto-hub/core/tests/soak.rs`]の`run_soak`が
//! ソーク走行中にレジスタを単調増加させ続ける手法と同じ)。固定値では
//! on_change 系(WS/MQTT)購読が発火せず開発用途に不十分、というのが
//! ux-plan の判断。周期は接続の実際の収集 period_ms に厳密に追従させる
//! 必要はない(ux-plan:「厳密でなくてよい」)ため、[`RAMP_PERIOD_MS`] 固定
//! (100ms - 既存ソーク`apps/banto-hub/core/tests/soak.rs`の収集周期と同じ値
//! を踏襲し、典型的な収集グループの周期より確実に速く回るようにした)を
//! 採用した。
//!
//! ワードデバイス(Modbus holding/input register・SLMP `D`)はランプ波、
//! ビットデバイス(Modbus coil/discrete input・SLMP `M`)はトグルとした -
//! データ型に応じた妥当な生成(ux-plan.md §1「ビットデバイスはトグル、
//! ワードデバイスはランプ」)。[`RAMP_ADDRESS_COUNT`]個のアドレス/デバイス
//! 番号だけ面倒をみる - 典型的な接続のタグ数を十分カバーしつつ、無限に
//! 生成し続けるコストを抑える境界(「凝りすぎない」という ux-plan の指示
//! どおりの最小実装。文字列タグ用の固定文字列生成は非対応 - S1 の文字列
//! タグはこの最小実装のスコープ外とした。将来必要になれば`set_string`で
//! 追加できる)。
//!
//! ## SLMP + banto-hub の broker 経路について(T9-2 で実装済み)
//!
//! T9-1 の時点でこのモジュールがカバーしていたのは(1)Modbus 接続
//! (常に直接クライアント、broker を経由しない)と(2)banto-hub の broker を
//! 経由しない SLMP 接続(このクレート単体・将来の非 hub コンシューマ)のみで、
//! hub の broker 経由 SLMP 接続(`HubSessions::ensure_connection`)は
//! `CollectorManager::rebuild`の中で`Collector::apply_config`より*前*に
//! セッションを確立するため、この`Collector`が起動するシミュレータの
//! アドレスをまだ知りえない、という申し送りが残っていた。
//!
//! T9-2 で解消済み: `apps/banto-hub/core/src/broker_glue.rs`の
//! `SlmpSimRegistry`が、この`start`/[`SimulatorHandle`]をそのまま再利用して
//! `ensure_connection`より前にシミュレータを起動・アドレス解決する - 詳細は
//! `SlmpSimRegistry`自身の doc comment（および同ファイルの module doc
//! 「T9-1/T9-2 note」節）を参照。

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use banto_plc::modbus::simulator::Simulator as ModbusSimulator;
use banto_plc::slmp::simulator::Simulator as SlmpSimulator;
use banto_plc::SlmpDevice;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::config::Protocol;

/// 値生成の周期(モジュール doc comment「値生成」参照)。
pub(crate) const RAMP_PERIOD_MS: u64 = 100;

/// ランプ波/トグルの対象アドレス数(モジュール doc comment「値生成」参照)。
const RAMP_ADDRESS_COUNT: u16 = 16;

/// 起動中の1シミュレータインスタンス - 対応するランプ波生成タスクとその
/// 停止チャネルを束ねる。`crate::collector::Collector` が接続キー
/// (`"conn:{id}"`)ごとに1個保持する。T9-2: `apps/banto-hub/core/src/broker_glue.rs`
/// の`SlmpSimRegistry`も、broker 経由 SLMP 接続1本ごとに1個保持する
/// (`Collector`とは別の、独立したライフサイクル - このモジュールの
/// 「SLMP + banto-hub の broker 経路について」参照)ため`pub`。
pub struct SimulatorHandle {
    addr: SocketAddr,
    ramp_stop: watch::Sender<bool>,
    ramp_task: JoinHandle<()>,
    inner: SimulatorInner,
}

enum SimulatorInner {
    Modbus(Arc<ModbusSimulator>),
    Slmp(Arc<SlmpSimulator>),
}

impl SimulatorHandle {
    /// 接続タスクが実際に宛先とする loopback アドレス -
    /// `crate::collector::Collector`はこれで`ConnectionPlan`(のタスク用
    /// コピー)の host/port を上書きする。T9-2: `SlmpSimRegistry`も同様に、
    /// broker セッションの実際の宛先としてこれを使う。
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// ランプ波タスクを止めてからシミュレータ自体を停止する。呼び出し順は
    /// 固定: ランプタスクを join して`Arc`の外側クローンを確実に手放させて
    /// から`Arc::try_unwrap`する - このモジュール内で`Arc`を保持するのは
    /// [`SimulatorHandle`]自身とランプタスクの2箇所だけなので、join 後は
    /// 常に唯一の参照になる(`crate::collector`の writer ローテーション
    /// (`close_or_flush_writer`)と同種の「所有権を取り戻してから閉じる」
    /// パターンだが、そちらと違って参照元が構造的に2つしかないため
    /// フォールバック分岐は不要)。
    pub async fn stop(self) {
        let _ = self.ramp_stop.send(true);
        let _ = self.ramp_task.await;
        match self.inner {
            SimulatorInner::Modbus(sim) => Arc::try_unwrap(sim)
                .unwrap_or_else(|_| unreachable!("ramp task already joined - sole reference"))
                .stop(),
            SimulatorInner::Slmp(sim) => Arc::try_unwrap(sim)
                .unwrap_or_else(|_| unreachable!("ramp task already joined - sole reference"))
                .stop(),
        }
    }
}

/// `protocol` に応じたシミュレータを loopback の空きポートで起動し、ランプ波/
/// トグル生成タスクを spawn する。呼び出し元は返った [`SimulatorHandle::addr`]
/// を接続先として使う。`pub`: `crate::collector::Collector`自身に加え、T9-2の
/// `apps/banto-hub/core/src/broker_glue.rs`の`SlmpSimRegistry`もこれを直接
/// 呼ぶ(このモジュールの「SLMP + banto-hub の broker 経路について」参照)。
pub async fn start(protocol: Protocol) -> SimulatorHandle {
    match protocol {
        Protocol::ModbusTcp => start_modbus().await,
        Protocol::Slmp => start_slmp().await,
    }
}

async fn start_modbus() -> SimulatorHandle {
    let sim = Arc::new(ModbusSimulator::start().await);
    let addr = sim.addr;
    let (ramp_stop, stop_rx) = watch::channel(false);
    let ramp_task = tokio::spawn(modbus_ramp_task(sim.clone(), stop_rx));
    SimulatorHandle {
        addr,
        ramp_stop,
        ramp_task,
        inner: SimulatorInner::Modbus(sim),
    }
}

async fn start_slmp() -> SimulatorHandle {
    let sim = Arc::new(SlmpSimulator::start().await);
    let addr = sim.addr;
    let (ramp_stop, stop_rx) = watch::channel(false);
    let ramp_task = tokio::spawn(slmp_ramp_task(sim.clone(), stop_rx));
    SimulatorHandle {
        addr,
        ramp_stop,
        ramp_task,
        inner: SimulatorInner::Slmp(sim),
    }
}

/// Modbus のランプ波/トグル生成ループ。holding/input register の両方に同じ
/// 値を書く(読み取り側がどちらの function code を使うか build_config の
/// アドレス種別次第で決まるため、両方を面倒みるのが最小実装で確実 -
/// coil/discrete input も同様)。
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

/// SLMP のランプ波/トグル生成ループ。`D`(ワード)にランプ波、`M`(ビット)に
/// トグルを書く - このリポジトリの他のシミュレータ利用箇所
/// (`apps/banto-hub/core/tests/t7_partial_reconfig.rs`等)と同じデバイスの
/// 組み合わせ。
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
