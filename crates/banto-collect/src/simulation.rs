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
//!
//! ## T15-2: all-simulation 開始前のカバレッジプリフライト(docs/banto-hub-desktop-plan.md §9.7)
//!
//! [`classify_plc_tag`]は、1タグの(protocol, address, data_type)がこの
//! モジュールの値生成ウィンドウ([`RAMP_ADDRESS_COUNT`]個のアドレス/デバイス
//! 番号、上の「値生成」節参照)に収まるかを判定する純粋関数 -
//! `apps/banto-hub/core/src/hub.rs`の`CollectorManager::simulation_coverage_report`
//! (T15-2)がカタログの全タグに対して呼び、all-simulation
//! (`RunMode::AllSimulation`)開始前に「対応 N タグ / 未対応 M タグ」を運用者
//! へ提示する(プラン §9.7 のモックアップどおり)。ウィンドウ外のタグは
//! シミュレータ配下でも常に Good/0 で変化しないままなので、それを事前に
//! 一覧できることが目的 - **`start(AllSimulation)`自体はこの T15-2 では
//! 一切ブロックしない**(プランの決定どおり、表示のみ)。
//!
//! アドレスのパースは`crate::config::build_request`が構成ビルド時に使うのと
//! 同じ`banto_plc::Address::{parse,parse_slmp}`を再利用する - カバレッジ
//! 判定が構成ビルドのアドレス解釈からズレて「未対応と表示されたのに実際は
//! 収集できている(またはその逆)」という不整合を防ぐため。

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use banto_plc::modbus::simulator::Simulator as ModbusSimulator;
use banto_plc::slmp::simulator::Simulator as SlmpSimulator;
use banto_plc::{Address, DataType, SlmpDevice};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::config::Protocol;

/// 値生成の周期(モジュール doc comment「値生成」参照)。
pub(crate) const RAMP_PERIOD_MS: u64 = 100;

/// ランプ波/トグルの対象アドレス数(モジュール doc comment「値生成」参照)。
/// T15-2: [`classify_plc_tag`]がこのウィンドウ判定に使うため`pub`
/// (`apps/banto-hub/core/src/hub.rs`は数値を再計算せずこの定数を直接参照する)。
pub const RAMP_ADDRESS_COUNT: u16 = 16;

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

// --- T15-2: all-simulation プリフライトのカバレッジ判定 --------------------

/// [`classify_plc_tag`]の結果 - 1タグが現行シミュレータの値生成ウィンドウ
/// (このモジュールの doc comment「T15-2」節参照)に収まるか。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SimulationCoverage {
    /// このモジュールが起動するシミュレータが値を生成し続ける
    /// (ランプ波/トグル) - all-simulation 下で変化する値が読める。
    Supported,
    /// シミュレータ配下でも常に Good/0 のまま変化しない(またはアドレス/
    /// データ型自体の解釈に失敗した) - `reason`は運用者に見せる日本語の
    /// 理由文。
    Unsupported { reason: String },
}

fn unsupported(reason: impl Into<String>) -> SimulationCoverage {
    SimulationCoverage::Unsupported {
        reason: reason.into(),
    }
}

/// `bit`指定アドレス(`"40001.3"`/`"D100.5"`、T8 notation)は`data_type=bit`
/// のタグでのみ意味を持つ - `crate::config::build_request`が構成ビルド時に
/// 課す規則と同一(そちらは`CollectError::Config`で構成ビルドそのものを
/// 拒否するので、通常はこの分岐に到達するタグ自体が収集構成に存在しないが、
/// カタログは構成ビルドの成否と無関係に全タグを載せるため、防御的にここでも
/// 判定する)。
fn is_bit_mismatch(bit: Option<u8>, data_type: DataType) -> bool {
    bit.is_some() && data_type != DataType::Bit
}

/// 1タグの(`protocol`, `address`, `data_type`)がシミュレータのカバレッジに
/// 入るかを分類する。`protocol`は`banto_tags::PlcConnection::protocol`
/// (`"modbus-tcp"` / `"slmp"`)、`address`/`data_type`は`banto_tags::Tag`の
/// 同名カラムそのもの - `apps/banto-hub/core/src/hub.rs`の
/// `CollectorManager::simulation_coverage_report`(T15-2)がカタログの各タグ
/// にそのまま渡す。
///
/// パーサーは`crate::config::build_request`と同じ`banto_plc::Address::{parse,
/// parse_slmp}`(このモジュールの doc comment「T15-2」節参照) - 分類が構成
/// ビルドのアドレス解釈からズレないようにするため。アドレス/データ型が
/// 解析できない場合も(構成ビルドなら`CollectError::Config`になるところを)
/// 「未対応」として理由文とともに報告する - プリフライトは構成ビルド前に
/// 呼ばれることもあるため、パニックせず常に何らかの分類を返す。
pub fn classify_plc_tag(protocol: &str, address: &str, data_type: &str) -> SimulationCoverage {
    // S1 文字列タグ: `crate::config::build_config_from`がタグごと収集対象
    // から外す(このモジュールの doc comment「値生成」節: 文字列タグ用の
    // 固定文字列生成は非対応)のと同じ判定。
    if data_type == banto_tags::STRING_DATA_TYPE {
        return unsupported("文字列タグは現行シミュレータのランプ生成の対象外です");
    }
    let Some(parsed_type) = DataType::parse(data_type) else {
        return unsupported(format!("データ型 {data_type} は未対応です"));
    };

    match protocol {
        "modbus-tcp" => classify_modbus_tag(address, parsed_type),
        "slmp" => classify_slmp_tag(address, parsed_type),
        other => unsupported(format!("未対応のプロトコルです: {other}")),
    }
}

/// [`classify_plc_tag`]の Modbus 経路。[`modbus_ramp_task`]が実際に書き込む
/// 範囲(4テーブル全て、オフセット`0..RAMP_ADDRESS_COUNT`)と一致させる。
fn classify_modbus_tag(address: &str, data_type: DataType) -> SimulationCoverage {
    let parsed = match Address::parse(address) {
        Ok(addr) => addr,
        Err(err) => return unsupported(format!("アドレスの解析に失敗しました: {err}")),
    };
    // `Address::parse`は常に`ModbusRef`を返す(I2a: parse/parse_slmpは互いの
    // 記法を受け付けない)ので`None`分岐は理論上到達しないが、`as_modbus_ref`
    // の契約どおり網羅的に扱う。
    let Some((area, offset, bit)) = parsed.as_modbus_ref() else {
        return unsupported("Modbus アドレスではありません");
    };
    if is_bit_mismatch(bit, data_type) {
        return unsupported(format!(
            "ビット指定アドレスは data_type=bit のタグでのみ対応します(現在のデータ型: {data_type})"
        ));
    }
    if offset < RAMP_ADDRESS_COUNT {
        SimulationCoverage::Supported
    } else {
        unsupported(format!(
            "現行シミュレータは{area}のオフセット 0..{RAMP_ADDRESS_COUNT} のみ値を生成します\
             (このタグのオフセット: {offset})"
        ))
    }
}

/// [`classify_plc_tag`]の SLMP 経路。[`slmp_ramp_task`]が実際に書き込む
/// 範囲(`D`/`M`のみ、デバイス番号`0..RAMP_ADDRESS_COUNT`)と一致させる。
fn classify_slmp_tag(address: &str, data_type: DataType) -> SimulationCoverage {
    let parsed = match Address::parse_slmp(address) {
        Ok(addr) => addr,
        Err(err) => return unsupported(format!("アドレスの解析に失敗しました: {err}")),
    };
    let Some((device, number, bit)) = parsed.as_slmp() else {
        return unsupported("SLMP アドレスではありません");
    };
    if is_bit_mismatch(bit, data_type) {
        return unsupported(format!(
            "ビット指定アドレスは data_type=bit のタグでのみ対応します(現在のデータ型: {data_type})"
        ));
    }
    match device {
        SlmpDevice::D | SlmpDevice::M => {
            if number < u32::from(RAMP_ADDRESS_COUNT) {
                SimulationCoverage::Supported
            } else {
                unsupported(format!(
                    "現行シミュレータはデバイス {} の番号 0..{RAMP_ADDRESS_COUNT} のみ値を生成します\
                     (このタグの番号: {number})",
                    device.mnemonic()
                ))
            }
        }
        other => unsupported(format!(
            "デバイス {} は現行シミュレータの値生成対象外です(D/M のみ対応)",
            other.mnemonic()
        )),
    }
}

#[cfg(test)]
mod capability_tests {
    use super::*;

    #[test]
    fn modbus_holding_register_within_window_is_supported() {
        assert_eq!(
            classify_plc_tag("modbus-tcp", "40001", "u16"),
            SimulationCoverage::Supported
        );
    }

    #[test]
    fn modbus_holding_register_at_the_last_in_window_offset_is_supported() {
        // 40016 -> offset 15、RAMP_ADDRESS_COUNT(16)の境界(0..16の最後)。
        assert_eq!(
            classify_plc_tag("modbus-tcp", "40016", "u16"),
            SimulationCoverage::Supported
        );
    }

    #[test]
    fn modbus_holding_register_just_past_the_window_is_unsupported() {
        // 40017 -> offset 16、ウィンドウ外の最初の1個。
        assert!(matches!(
            classify_plc_tag("modbus-tcp", "40017", "u16"),
            SimulationCoverage::Unsupported { .. }
        ));
    }

    #[test]
    fn modbus_coil_within_window_is_supported() {
        assert_eq!(
            classify_plc_tag("modbus-tcp", "00001", "bit"),
            SimulationCoverage::Supported
        );
    }

    #[test]
    fn modbus_discrete_input_within_window_is_supported() {
        assert_eq!(
            classify_plc_tag("modbus-tcp", "10001", "bit"),
            SimulationCoverage::Supported
        );
    }

    #[test]
    fn modbus_input_register_within_window_is_supported() {
        assert_eq!(
            classify_plc_tag("modbus-tcp", "30001", "u16"),
            SimulationCoverage::Supported
        );
    }

    #[test]
    fn modbus_bit_in_word_within_window_and_bit_type_is_supported() {
        // 40001.3 -> holding register offset 0、data_type=bit なので許容。
        assert_eq!(
            classify_plc_tag("modbus-tcp", "40001.3", "bit"),
            SimulationCoverage::Supported
        );
    }

    #[test]
    fn modbus_bit_in_word_with_non_bit_data_type_is_unsupported() {
        assert!(matches!(
            classify_plc_tag("modbus-tcp", "40001.3", "u16"),
            SimulationCoverage::Unsupported { .. }
        ));
    }

    #[test]
    fn modbus_unparseable_address_is_unsupported_with_a_reason() {
        match classify_plc_tag("modbus-tcp", "not-an-address", "u16") {
            SimulationCoverage::Unsupported { reason } => assert!(!reason.is_empty()),
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }

    #[test]
    fn string_tag_is_always_unsupported_regardless_of_protocol() {
        assert!(matches!(
            classify_plc_tag("modbus-tcp", "40001", "string"),
            SimulationCoverage::Unsupported { .. }
        ));
        assert!(matches!(
            classify_plc_tag("slmp", "D0", "string"),
            SimulationCoverage::Unsupported { .. }
        ));
    }

    #[test]
    fn unknown_data_type_is_unsupported() {
        assert!(matches!(
            classify_plc_tag("modbus-tcp", "40001", "f64"),
            SimulationCoverage::Unsupported { .. }
        ));
    }

    #[test]
    fn unknown_protocol_is_unsupported() {
        assert!(matches!(
            classify_plc_tag("foo-protocol", "40001", "u16"),
            SimulationCoverage::Unsupported { .. }
        ));
    }

    #[test]
    fn slmp_d_device_within_window_is_supported() {
        assert_eq!(
            classify_plc_tag("slmp", "D0", "u16"),
            SimulationCoverage::Supported
        );
        // D15 -> RAMP_ADDRESS_COUNT(16)の境界(0..16の最後)。
        assert_eq!(
            classify_plc_tag("slmp", "D15", "u16"),
            SimulationCoverage::Supported
        );
    }

    #[test]
    fn slmp_d_device_just_past_the_window_is_unsupported() {
        assert!(matches!(
            classify_plc_tag("slmp", "D16", "u16"),
            SimulationCoverage::Unsupported { .. }
        ));
    }

    #[test]
    fn slmp_m_device_within_window_is_supported() {
        assert_eq!(
            classify_plc_tag("slmp", "M0", "bit"),
            SimulationCoverage::Supported
        );
    }

    #[test]
    fn slmp_m_device_just_past_the_window_is_unsupported() {
        assert!(matches!(
            classify_plc_tag("slmp", "M16", "bit"),
            SimulationCoverage::Unsupported { .. }
        ));
    }

    #[test]
    fn slmp_word_device_other_than_d_is_unsupported() {
        assert!(matches!(
            classify_plc_tag("slmp", "W0", "u16"),
            SimulationCoverage::Unsupported { .. }
        ));
    }

    #[test]
    fn slmp_bit_device_other_than_m_is_unsupported() {
        assert!(matches!(
            classify_plc_tag("slmp", "X0", "bit"),
            SimulationCoverage::Unsupported { .. }
        ));
    }

    #[test]
    fn slmp_bit_in_word_within_window_and_bit_type_is_supported() {
        // D0.5 -> D デバイス番号0(ウィンドウ内)、data_type=bit なので許容。
        assert_eq!(
            classify_plc_tag("slmp", "D0.5", "bit"),
            SimulationCoverage::Supported
        );
    }

    #[test]
    fn slmp_bit_in_word_with_non_bit_data_type_is_unsupported() {
        assert!(matches!(
            classify_plc_tag("slmp", "D0.5", "u16"),
            SimulationCoverage::Unsupported { .. }
        ));
    }

    #[test]
    fn slmp_unparseable_address_is_unsupported_with_a_reason() {
        match classify_plc_tag("slmp", "not-an-address", "u16") {
            SimulationCoverage::Unsupported { reason } => assert!(!reason.is_empty()),
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }
}
