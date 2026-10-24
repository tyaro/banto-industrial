//! [`BrokerSimRegistry`]: the simulation mode of a broker-read connection
//! (T9-2, docs/ux-plan.md §1, 2026-08-06 オーナー決定) - what its broker
//! session should actually be: the real driver dialing the connection's own
//! settings, or (`simulation = true`) the value layer's in-process
//! [`SimDevice`] (R2b-5b, docs/banto-hub-driver-sim-design.md §4).
//!
//! Moved here from banto-hub's `broker_glue.rs` in R2b-3a-3
//! (docs/banto-hub-driver-io-design.md §8): ChronoGazer reads through the
//! broker too since then, and the registry has no banto-hub-specific
//! dependency (it needs the connection row and the driver registry's secret
//! keys for the fingerprint), so both apps share this one implementation.
//! banto-hub re-exports it from `broker_glue`.
//!
//! Until R2b-5b this started a wire-protocol simulator on the loopback and
//! swapped `host` / `port`; the session now reads and writes a table of values
//! and no socket exists, so every broker-managed protocol (a Session or Frame
//! driver in the registry) can be simulated, strings and addresses past the
//! old simulator's 16 included.
//!
//! ## Why it runs before `ensure_connection`
//!
//! `banto_broker::SessionDirectory::ensure_connection_with_settings` reuses a
//! session by connection id alone - it does not look at the settings (or at
//! simulation on / off) again. So the target has to be known *before* the
//! session is ensured: the caller calls [`BrokerSimRegistry::resolve`] for
//! every broker-read connection first, and when it answers `changed`, removes
//! the old session before ensuring a new one for the returned
//! [`DialTarget`].
//!
//! This type was once called `SlmpSimRegistry` (SLMP-only at T9-2; renamed
//! in #339 once it served every broker-managed protocol).

use std::collections::HashMap;

use banto_broker::{driver_secret_keys, real_dial_fingerprint, SettingsFingerprint, SimDevice};
use banto_driver_api::{DriverRegistry, Settings};
use banto_tags::PlcConnection;

/// What a connection's broker session is made of (design §4.2): the real
/// driver dialing `Settings`, or the value layer's [`SimDevice`] (the
/// `Settings` are still the connection's own - `validate_config` runs on them
/// either way, so a simulation that the real device would refuse is refused).
#[derive(Clone)]
pub enum DialTarget {
    Real(Settings),
    Sim(Settings, SimDevice),
}

impl DialTarget {
    /// The connection's settings (never altered by the simulation).
    pub fn settings(&self) -> &Settings {
        match self {
            DialTarget::Real(s) | DialTarget::Sim(s, _) => s,
        }
    }

    /// Whether this is the simulator.
    pub fn is_simulated(&self) -> bool {
        matches!(self, DialTarget::Sim(..))
    }
}

/// The settings fingerprint of a session of `conn` made for `target`
/// ([`SettingsFingerprint::of_connection`], or its simulated form, with the
/// driver's secret keys): the same rule as the broker's own handle, so a
/// stamped plan (`CollectorConfig::set_broker_dial_target`) and a session
/// agree - including the simulation mark.
pub fn dial_fingerprint(
    conn: &PlcConnection,
    target: &DialTarget,
    registry: &DriverRegistry,
) -> SettingsFingerprint {
    match target {
        DialTarget::Sim(settings, device) => SettingsFingerprint::of_connection_simulated(
            &conn.protocol,
            Some(conn),
            settings,
            &driver_secret_keys(registry, &conn.protocol),
            device.id(),
        ),
        DialTarget::Real(settings) => {
            real_dial_fingerprint(registry, &conn.protocol, Some(conn), settings)
        }
    }
}

/// A connection's [`SimDevice`] and the protocol it was made for: a device is
/// reused only by the same protocol (design §4.2).
struct SimEntry {
    protocol: String,
    device: SimDevice,
}

/// See this module's doc. Built empty and grown by [`Self::resolve`]; held
/// across reconfigurations next to the broker sessions it feeds, and shut
/// down once, *after* those sessions ([`Self::shutdown`]).
pub struct BrokerSimRegistry {
    /// `simulation = true` の接続 id ごとの値の表（高々1個）。
    /// [`Self::resolve`] が simulation フラグの on/off に応じて作る・捨てる。
    devices: tokio::sync::Mutex<HashMap<i64, SimEntry>>,
    /// 各接続 id について直近に解決した対象の指紋
    /// （[`SettingsFingerprint`]、R2b-3a-2。#605 では host/port と SLMP の CPU
    /// 種別だった）- [`Self::resolve`]が「対象が変わったか」を判定する
    /// ための唯一の情報源。エントリなし(`None`)は「初回」を意味し、常に
    /// "changed" 扱いになる。同じ IP・ポートで CPU を R→Q に変えても、Modbus の
    /// ユニット ID・ワード順を変えても、シミュレーションを切り替えても、ここが
    /// 違えば `changed` になり、呼び出し側が古い broker セッションを捨てる
    /// （プロトコルに依らない）。
    last_target: tokio::sync::Mutex<HashMap<i64, SettingsFingerprint>>,
}

impl BrokerSimRegistry {
    /// 空のレジストリで開始する。
    pub fn new() -> Self {
        Self {
            devices: tokio::sync::Mutex::new(HashMap::new()),
            last_target: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    /// `conn`(broker が読む接続)の broker セッションの対象（[`DialTarget`]）を
    /// 解決する（R2b-3a-2、docs/banto-hub-driver-io-design.md §4.4。R2b-5b で
    /// 値の層へ）。土台は接続の `settings` を `banto_tags::settings_to_api` で
    /// 直したもの（秘密の参照は入らない - 秘密は broker がダイヤルの直前に
    /// だけ開く）。
    ///
    /// - `conn.simulation == true`: `conn.id` 用の [`SimDevice`] を作る（無ければ）
    ///   か再利用して `Sim(settings, device)`。**再利用は同じ `protocol` のとき
    ///   だけ**: `protocol` が変わっていれば古い表（held）を捨てて作り直す
    ///   （前のドライバの held が、正規形の重なる新しいドライバのタグへ漏れない
    ///   ように）。ホストもポートも差し替えない（ソケットは開かない）。
    /// - `conn.simulation == false`: 持っていた [`SimDevice`] を捨てて
    ///   `Real(settings)`。
    ///
    /// いずれの場合も、対象の指紋（[`dial_fingerprint`]、シミュレーションの印つき）を
    /// `last_target` に記録されている直前の値(`conn.id`が未記録なら常に「変わった」扱い)
    /// と比較して`changed`を求め、`last_target`を更新してから返す。
    ///
    /// ## `changed`が重要な理由
    ///
    /// broker の `ensure_connection_with_settings` は接続 id だけで既存セッションを
    /// 再利用するので、呼び出し側は `changed == true` のたびに、ensure の*前*に
    /// その接続のセッションを取り除かなければならない - そうしなければ古い
    /// セッション(シミュレーションを切ったあとの表、前の CPU 種別など)へ黙って
    /// 接続し続けてしまう。さらに、動いている収集タスクは古いハンドルを握って
    /// いるので、呼び出し側は `CollectorConfig::set_broker_dial_target` に同じ
    /// 指紋を渡し、`Collector::apply_config` がその接続を「replaced」に分類して
    /// タスクごと作り直すように仕向ける。
    ///
    /// 無関係な編集（同じ接続の別グループへのタグ追加など）では対象が変わらない
    /// ので `changed` は `false` のまま。秘密値を差し替える（新しい参照で封印し直す）と
    /// 指紋が変わり `changed` になる。
    pub async fn resolve(
        &self,
        conn: &PlcConnection,
        registry: &DriverRegistry,
    ) -> (DialTarget, bool) {
        let settings = banto_tags::settings_to_api(&conn.settings);
        let target = {
            let mut devices = self.devices.lock().await;
            if conn.simulation {
                let reusable = devices
                    .get(&conn.id)
                    .is_some_and(|e| e.protocol == conn.protocol);
                if !reusable {
                    devices.insert(
                        conn.id,
                        SimEntry {
                            protocol: conn.protocol.clone(),
                            device: SimDevice::new(),
                        },
                    );
                }
                let device = devices[&conn.id].device.clone();
                DialTarget::Sim(settings, device)
            } else {
                devices.remove(&conn.id);
                DialTarget::Real(settings)
            }
        };

        let fingerprint = dial_fingerprint(conn, &target, registry);
        let mut last_target = self.last_target.lock().await;
        let changed = last_target.get(&conn.id) != Some(&fingerprint);
        last_target.insert(conn.id, fingerprint);
        (target, changed)
    }

    /// `connection_id` の値の表(あれば)を返す - テストと診断の覗き窓
    /// （[`SimDevice::get`] で書いた値を読み戻せる）。
    pub async fn device(&self, connection_id: i64) -> Option<SimDevice> {
        self.devices
            .lock()
            .await
            .get(&connection_id)
            .map(|e| e.device.clone())
    }

    /// `connection_id`用に追跡している値の表(あれば)を忘れ、
    /// `last_target`のエントリも忘れる。broker が読む接続の集合から外れた接続
    /// (削除・無効化・プロトコル変更)の掃除から呼ばれる - **対応する収集タスクが
    /// 既に停止し、セッションを止めた後にのみ**呼び出すこと。
    pub async fn remove(&self, connection_id: i64) {
        self.devices.lock().await.remove(&connection_id);
        self.last_target.lock().await.remove(&connection_id);
    }

    /// 追跡中の全ての値の表を忘れる。broker セッションを止めた*後*に
    /// 一度だけ呼ぶこと(表は、それを読み書きする broker セッションより
    /// 長生きしなければならない)。
    pub async fn shutdown(&self) {
        self.devices.lock().await.clear();
    }
}

impl Default for BrokerSimRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use banto_driver_api::{keys, DataType, TagKind, Value};
    use std::sync::Arc;

    fn conn(protocol: &str, simulation: bool) -> PlcConnection {
        PlcConnection::from_settings(
            7,
            "c".to_string(),
            protocol.to_string(),
            true,
            simulation,
            serde_json::json!({"host": "192.0.2.1", "port": 5000})
                .as_object()
                .unwrap()
                .clone(),
            1,
        )
    }

    /// R2b-5b: a simulated connection is a value table, never a socket - the
    /// settings keep the connection's own host/port, and any broker-managed
    /// protocol is simulated (no per-protocol simulator list any more).
    #[tokio::test]
    async fn a_simulation_keeps_the_settings_and_makes_a_device() {
        let registry = banto_drivers::builtin().registry;
        let sims = BrokerSimRegistry::new();
        for protocol in ["modbus-tcp", "slmp", "fake-session"] {
            let (target, _) = sims.resolve(&conn(protocol, true), &registry).await;
            assert!(target.is_simulated());
            assert_eq!(target.settings().get(keys::HOST), Some("192.0.2.1"));
            assert!(sims.device(7).await.is_some());
        }
        let (target, changed) = sims.resolve(&conn("fake-session", false), &registry).await;
        assert!(changed);
        assert!(!target.is_simulated());
        assert!(sims.device(7).await.is_none());
        sims.shutdown().await;
    }

    /// The simulation mark is part of the fingerprint: on -> off -> on is a
    /// change every time, and the registry's value equals the broker handle's
    /// formula for the same target (design §12).
    #[tokio::test]
    async fn the_simulation_mark_changes_the_fingerprint() {
        let registry = banto_drivers::builtin().registry;
        let sims = BrokerSimRegistry::new();
        let (_, c1) = sims.resolve(&conn("modbus-tcp", false), &registry).await;
        let (_, c2) = sims.resolve(&conn("modbus-tcp", false), &registry).await;
        let (_, c3) = sims.resolve(&conn("modbus-tcp", true), &registry).await;
        let (_, c4) = sims.resolve(&conn("modbus-tcp", true), &registry).await;
        let (_, c5) = sims.resolve(&conn("modbus-tcp", false), &registry).await;
        assert_eq!((c1, c2, c3, c4, c5), (true, false, true, false, true));

        let c = conn("modbus-tcp", true);
        let (target, _) = sims.resolve(&c, &registry).await;
        let keys = driver_secret_keys(&registry, "modbus-tcp");
        assert_eq!(
            dial_fingerprint(&c, &target, &registry),
            SettingsFingerprint::of_connection_simulated(
                "modbus-tcp",
                Some(&c),
                target.settings(),
                &keys,
                match &target {
                    DialTarget::Sim(_, d) => d.id(),
                    _ => unreachable!(),
                }
            )
        );
        assert_ne!(
            dial_fingerprint(&c, &target, &registry),
            dial_fingerprint(&c, &DialTarget::Real(target.settings().clone()), &registry)
        );
    }

    /// #807 P2: the fingerprint carries the table's generation. Reusing the
    /// table keeps it; a rebuilt table (off -> on, or after `remove`) differs
    /// even with identical settings.
    #[tokio::test]
    async fn a_rebuilt_table_has_another_fingerprint() {
        let registry = banto_drivers::builtin().registry;
        let sims = BrokerSimRegistry::new();
        let c = conn("modbus-tcp", true);
        let (t1, _) = sims.resolve(&c, &registry).await;
        let f1 = dial_fingerprint(&c, &t1, &registry);
        let (t2, changed) = sims.resolve(&c, &registry).await;
        assert!(!changed);
        assert_eq!(f1, dial_fingerprint(&c, &t2, &registry));

        sims.resolve(&conn("modbus-tcp", false), &registry).await;
        let (t3, _) = sims.resolve(&c, &registry).await;
        assert_ne!(f1, dial_fingerprint(&c, &t3, &registry));

        sims.remove(c.id).await;
        let (t4, changed) = sims.resolve(&c, &registry).await;
        assert!(changed);
        assert_ne!(
            dial_fingerprint(&c, &t3, &registry),
            dial_fingerprint(&c, &t4, &registry)
        );
    }

    /// Design §4.2: a device is reused by the same protocol only - a protocol
    /// change drops its held values (they must not leak into the new
    /// driver's tags).
    #[tokio::test]
    async fn changing_the_protocol_drops_the_held_values() {
        let registry = banto_drivers::builtin().registry;
        let sims = BrokerSimRegistry::new();
        let (target, _) = sims.resolve(&conn("modbus-tcp", true), &registry).await;
        let DialTarget::Sim(_, first) = target else {
            panic!("simulated")
        };
        first.set("D100", TagKind::Numeric(DataType::I16), Value::F64(5.0));
        assert_eq!(first.held_count(), 1);

        // Same protocol again: the same table.
        let (again, changed) = sims.resolve(&conn("modbus-tcp", true), &registry).await;
        assert!(!changed);
        let DialTarget::Sim(_, same) = again else {
            panic!("simulated")
        };
        assert_eq!(same.held_count(), 1);

        // Another protocol: a fresh table.
        let (other, changed) = sims.resolve(&conn("slmp", true), &registry).await;
        assert!(changed);
        let DialTarget::Sim(_, fresh) = other else {
            panic!("simulated")
        };
        assert_eq!(fresh.held_count(), 0);
        assert_eq!(sims.device(7).await.unwrap().held_count(), 0);
    }

    /// Design §12: the fingerprint the registry stamps on a plan
    /// (`dial_fingerprint`) is the one the broker's handle carries, for the
    /// real and the simulated session alike - a mismatch would keep a
    /// collection task on a stale handle ("replaced" never fires). The
    /// simulated session also takes the registry check and `validate_config`
    /// of the real one.
    #[tokio::test]
    async fn the_stamped_fingerprint_is_the_handles_own() {
        let registry = Arc::new(banto_drivers::builtin().registry);
        let supervisor = banto_broker::BrokerSupervisor::spawn_with(
            &[],
            registry.clone(),
            banto_broker::BrokerOptions::default(),
        )
        .unwrap();
        let directory = supervisor.directory();
        let sims = BrokerSimRegistry::new();

        let sim_conn = conn("modbus-tcp", true);
        let (target, _) = sims.resolve(&sim_conn, &registry).await;
        let DialTarget::Sim(settings, device) = target.clone() else {
            panic!("simulated")
        };
        let handle = directory
            .ensure_simulated_connection(&sim_conn, settings, device)
            .unwrap();
        assert_eq!(
            handle.settings_fingerprint(),
            Some(dial_fingerprint(&sim_conn, &target, &registry))
        );
        directory.remove(sim_conn.id);

        let real_conn = conn("modbus-tcp", false);
        let (target, _) = sims.resolve(&real_conn, &registry).await;
        let DialTarget::Real(settings) = target.clone() else {
            panic!("real")
        };
        let handle = directory
            .ensure_connection_with_settings(&real_conn, settings)
            .unwrap();
        assert_eq!(
            handle.settings_fingerprint(),
            Some(dial_fingerprint(&real_conn, &target, &registry))
        );
        directory.remove(real_conn.id);

        // The same checks as the real session's.
        let unknown = conn("no-such-protocol", true);
        assert!(matches!(
            directory.ensure_simulated_connection(
                &unknown,
                banto_tags::settings_to_api(&unknown.settings),
                SimDevice::new()
            ),
            Err(banto_broker::BrokerError::UnsupportedProtocol { .. })
        ));
        let mut bad = conn("modbus-tcp", true);
        bad.settings
            .insert("host".to_string(), serde_json::json!(""));
        assert!(matches!(
            directory.ensure_simulated_connection(
                &bad,
                banto_tags::settings_to_api(&bad.settings),
                SimDevice::new()
            ),
            Err(banto_broker::BrokerError::InvalidSettings { .. })
        ));
        supervisor.shutdown().await;
    }
}
