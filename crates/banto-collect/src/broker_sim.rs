//! [`BrokerSimRegistry`]: the simulation mode of a broker-read connection
//! (T9-2, docs/ux-plan.md §1, 2026-08-06 オーナー決定) - the settings its
//! broker session should actually dial, with `host`/`port` replaced by an
//! in-process simulator's when `simulation = true`.
//!
//! Moved here from banto-hub's `broker_glue.rs` in R2b-3a-3
//! (docs/banto-hub-driver-io-design.md §8): ChronoGazer reads through the
//! broker too since then, and the registry has no banto-hub-specific
//! dependency (it needs the connection row, the driver registry's secret keys
//! for the fingerprint, and [`crate::simulation`]), so both apps share this
//! one implementation. banto-hub re-exports it from `broker_glue`.
//!
//! ## Why it runs before `ensure_connection`
//!
//! `banto_broker::SessionDirectory::ensure_connection_with_settings` reuses a
//! session by connection id alone - it does not look at the settings again.
//! So the simulator's address has to be known *before* the session is
//! ensured: the caller calls [`BrokerSimRegistry::resolve`] for every
//! broker-read connection first, and when it answers `changed`, removes the
//! old session before ensuring a new one with the returned settings.
//!
//! This type was once called `SlmpSimRegistry` (SLMP-only at T9-2; renamed
//! in #339 once it served every broker-managed protocol).

use std::collections::HashMap;

use banto_broker::SettingsFingerprint;
use banto_driver_api::{keys, DriverRegistry, Settings};
use banto_tags::PlcConnection;

use crate::config::Protocol;
use crate::simulation::{self, SimulatorHandle};

/// `protocol` のドライバの秘密のキー（無ければ空）。
pub(crate) fn secret_keys_of(registry: &DriverRegistry, protocol: &str) -> Vec<String> {
    registry
        .get(protocol)
        .map(|entry| entry.base().secret_keys())
        .unwrap_or_default()
}

/// The settings fingerprint of a session of `conn` dialed with `settings`
/// ([`SettingsFingerprint::of_connection`] with the driver's secret keys):
/// the same rule as the broker's own handle, so a stamped plan
/// (`CollectorConfig::set_broker_dial_target`) and a session agree.
pub fn dial_fingerprint(
    conn: &PlcConnection,
    settings: &Settings,
    registry: &DriverRegistry,
) -> SettingsFingerprint {
    SettingsFingerprint::of_connection(
        &conn.protocol,
        Some(conn),
        settings,
        &secret_keys_of(registry, &conn.protocol),
    )
}

/// See this module's doc. Built empty and grown by [`Self::resolve`]; held
/// across reconfigurations next to the broker sessions it feeds, and shut
/// down once, *after* those sessions ([`Self::shutdown`]).
pub struct BrokerSimRegistry {
    /// `simulation = true` として現在起動中のシミュレータ - 接続 id ごとに
    /// 高々1個。[`Self::resolve`]が simulation フラグの on/off に応じて
    /// 起動・停止する。
    simulators: tokio::sync::Mutex<HashMap<i64, SimulatorHandle>>,
    /// 各接続 id について直近に解決した設定の指紋
    /// （[`SettingsFingerprint`]、R2b-3a-2。#605 では host/port と SLMP の CPU
    /// 種別だった）- [`Self::resolve`]が「ダイヤルの設定が変わったか」を判定する
    /// ための唯一の情報源。エントリなし(`None`)は「初回」を意味し、常に
    /// "changed" 扱いになる。同じ IP・ポートで CPU を R→Q に変えても、Modbus の
    /// ユニット ID・ワード順を変えても、ここが違えば `changed` になり、呼び出し側が
    /// 古い broker セッションを捨てる（プロトコルに依らない）。
    last_target: tokio::sync::Mutex<HashMap<i64, SettingsFingerprint>>,
}

impl BrokerSimRegistry {
    /// 空のレジストリで開始する。
    pub fn new() -> Self {
        Self {
            simulators: tokio::sync::Mutex::new(HashMap::new()),
            last_target: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    /// `conn`(broker が読む接続)が実際にダイヤルすべき設定（`Settings`）を解決する
    /// （R2b-3a-2、docs/banto-hub-driver-io-design.md §4.4）。土台は接続の
    /// `settings` を `banto_tags::settings_to_api` で直したもの（秘密の参照は
    /// 入らない - 秘密は broker がダイヤルの直前にだけ開く）。
    ///
    /// - `conn.simulation == true` の場合: `conn.id`用のシミュレータが
    ///   既に起動中ならそれを再利用し、なければ`conn.protocol`に応じた
    ///   シミュレータ種別（`conn.protocol == "modbus-tcp"`なら
    ///   [`Protocol::ModbusTcp`]、それ以外は [`Protocol::Slmp`]）を
    ///   [`simulation::start`] へ渡して新規に起動・登録し、設定の `host`/`port` を
    ///   そのループバックのアドレスに差し替える（#131、2026-09-01: この protocol 分岐が
    ///   無いと、Modbus 接続の simulation=true が SLMP シミュレータへ誤って差し替わって
    ///   しまう。シミュレータの選び方は R2b-5 まで名前で選ぶ）。
    /// - `conn.simulation == false` の場合: `conn.id`用のシミュレータが
    ///   まだ追跡されていれば取り除いて停止する(もう不要なので)。設定はそのまま。
    ///
    /// いずれの場合も、返す設定の指紋（[`dial_fingerprint`]）を `last_target` に
    /// 記録されている直前の値(`conn.id`が未記録なら常に「変わった」扱い)と比較して
    /// `changed`を求め、`last_target`を更新してから`(設定, changed)`を返す。
    ///
    /// ## `changed`が重要な理由
    ///
    /// broker の `ensure_connection_with_settings` は接続 id だけで既存セッションを
    /// 再利用するので、呼び出し側は `changed == true` のたびに、ensure の*前*に
    /// その接続のセッションを取り除かなければならない - そうしなければ古い
    /// セッション(シミュレーションを切ったあとの実ホスト、前回のシミュレータ、
    /// 前の CPU 種別など)へ黙って接続し続けてしまう。さらに、動いている収集
    /// タスクは古いハンドルを握っているので、呼び出し側は
    /// `CollectorConfig::set_broker_dial_target` に同じ指紋を渡し、
    /// `Collector::apply_config` がその接続を「replaced」に分類してタスクごと
    /// 作り直すように仕向ける。
    ///
    /// 無関係な編集（同じ接続の別グループへのタグ追加など）では設定が変わらない
    /// ので `changed` は `false` のまま。秘密値を差し替える（新しい参照で封印し直す）と
    /// 指紋が変わり `changed` になる。
    pub async fn resolve(
        &self,
        conn: &PlcConnection,
        registry: &DriverRegistry,
    ) -> (Settings, bool) {
        let mut settings = banto_tags::settings_to_api(&conn.settings);
        let mut simulators = self.simulators.lock().await;
        if conn.simulation {
            let addr = match simulators.get(&conn.id) {
                Some(handle) => handle.addr(),
                None => {
                    // #131 (2026-09-01): select the simulator type from the
                    // connection's own protocol rather than hardcoding SLMP.
                    // Kept until R2b-5.
                    let protocol = if conn.protocol == "modbus-tcp" {
                        Protocol::ModbusTcp
                    } else {
                        Protocol::Slmp
                    };
                    let handle = simulation::start(protocol).await;
                    let addr = handle.addr();
                    simulators.insert(conn.id, handle);
                    addr
                }
            };
            settings.insert(keys::HOST, addr.ip().to_string());
            settings.insert(keys::PORT, addr.port().to_string());
        } else if let Some(handle) = simulators.remove(&conn.id) {
            handle.stop().await;
        }
        drop(simulators);

        let fingerprint = dial_fingerprint(conn, &settings, registry);
        let mut last_target = self.last_target.lock().await;
        let changed = last_target.get(&conn.id) != Some(&fingerprint);
        last_target.insert(conn.id, fingerprint);
        (settings, changed)
    }

    /// `connection_id`用に追跡しているシミュレータ(あれば)を停止・忘却し、
    /// `last_target`のエントリも忘れる。broker が読む接続の集合から外れた接続
    /// (削除・無効化・プロトコル変更)の掃除から呼ばれる - **対応する収集タスクが
    /// 既に停止した後にのみ**呼び出すこと。
    pub async fn remove(&self, connection_id: i64) {
        if let Some(handle) = self.simulators.lock().await.remove(&connection_id) {
            handle.stop().await;
        }
        self.last_target.lock().await.remove(&connection_id);
    }

    /// 追跡中の全シミュレータを停止する。broker セッションを止めた*後*に
    /// 一度だけ呼ぶこと(シミュレータは、それをダイヤルする broker セッション
    /// より長生きしなければならない)。
    pub async fn shutdown(&self) {
        let mut simulators = self.simulators.lock().await;
        for (_, handle) in simulators.drain() {
            handle.stop().await;
        }
    }
}

impl Default for BrokerSimRegistry {
    fn default() -> Self {
        Self::new()
    }
}
