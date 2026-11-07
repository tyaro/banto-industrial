//! H3（tyaro/banto-industrial#1053 の H の続き、2026-11-07）: PLC の時計の
//! 読取りの Hub 側 - 結果の分類、PLC の時刻と PC の時刻の差、REST の GET・MCP の
//! 使い回し。
//!
//! ## 役割の分け方（ゲストの PLAN.md「H3 の設計」）
//!
//! どのデバイスをどう読むか・機種ごとの違い（iQ-R 系は SD210–SD216 の BIN、
//! 標準 Q・L は SD210–SD213 の BCD）・SM213 と SM210 の確かめはゲスト
//! （`clock-read-v1`）が持つ。Hub は H3 の第 1 段で broker まで通した
//! `read_clock`（`ReadOnlyHandle` にもある。PLC に何も書かない）を呼び、結果を
//! 分類し、PC の現地時刻（[`crate::clock_sync::ClockSync`] と同じ差し替えられる
//! `now`）との差を付けるだけ。手順の上限（engine の `MAX_CLOCK_READ_STEPS = 4`）と
//! 時間切れ（broker の `clock_read_job_limit`）は下の層のもの。
//!
//! ## 使い道（オーナー決定 2026-11-07）
//!
//! 1. 表示: REST `GET /api/plc-connections/{id}/clock` と MCP `read_plc_clock`。
//!    どちらも [`crate::clock_sync::ClockSync::read_plc_clock`] を通り、接続ごとに
//!    直近の結果を [`CLOCK_READ_REUSE`] 使い回す（[`ClockReadCache`]）。監査には
//!    残さない（表示だけ）。Hub の `clockSync.enabled` は問わない。
//! 2. 時計合わせ（H2）の前後: `ClockSyncRecord` の `before`・`after` に載り、監査の
//!    `detail` に残る。こちらは使い回さず毎回新しく読み、結果が何であっても
//!    時計合わせは止めない。時計合わせを 1 回実行したら、その接続の使い回しを捨てる。
//!
//! ## 使い回し（[`ClockReadCache`]）
//!
//! - 接続ごとに 1 件、読み終えた時刻（`tokio::time::Instant`）から
//!   [`CLOCK_READ_REUSE`] のあいだ同じ記録を `cached: true` で返す。結果が `ok` で
//!   なくても使い回す（`rejected` の PLC に連打しないため）。ただし `unavailable`
//!   （何も送っていない）は使い回さない。
//! - 接続ごとの `tokio::sync::Mutex` で、同じ接続の同時の要求を 1 回の読取りに
//!   まとめる（後の要求は前の結果を `cached: true` で受ける）。
//! - 捨てる（[`ClockReadCache::invalidate`]）と世代を進める。読取りの途中で捨てられた
//!   ときは、その読取りの結果を保存しない（時計合わせの前の値を後から使い回さない）。
//!
//! ## 差（`offsetSeconds`）
//!
//! `plcTime − pcTime` の秒（PLC が進んでいれば正）。`pcTime` は **broker の応答を
//! 受けた直後**の PC の現地時刻を秒に切り捨てたもの。PLC の時刻も秒までなので、
//! 差には ±1 s の誤差がある（応答の往復の時間もここに入る）。

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use banto_broker::{BrokerError, ClockReadOutcome};
use banto_driver_api::{CivilTime, ItemError};
use chrono::{NaiveDate, NaiveDateTime, Timelike};
use serde::Serialize;
use tokio::time::Instant;
use utoipa::ToSchema;

use crate::clock_sync::civil_time_text;

/// REST の GET・MCP が直近の結果を使い回す長さ。
pub const CLOCK_READ_REUSE: Duration = Duration::from_secs(5);

/// 1 回の読取りの結果（JSON の `result`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum ClockReadResult {
    /// 読めた（`plcTime`・`offsetSeconds` がある）。
    Ok,
    /// ドライバ・機種が扱わない（Q25PRH・FX5 など）。何も送っていない。
    Unsupported,
    /// ドライバが断った（SM213 が OFF で SD210 以降が時計でない、SM210 が ON で
    /// 時計合わせが残っている、など）。
    Rejected,
    /// 機器の終了コード（`code`）。
    Device,
    /// 読んだ値が時計として正しくない（暦にない日付、BCD でない桁）。
    VerificationFailed,
    /// 送らなかった（セッションが無い・切断中、broker が送る前に断った）。
    Unavailable,
    /// 結果不明（送った後かもしれない致命・時間切れ）。読取りは PLC に何も
    /// 書かないので時計には影響しない。接続は再接続される。
    Unknown,
    /// このドライバは時計の読取りを出さない。何も送っていない。
    NotOffered,
}

/// 1 回の読取りの記録（REST の GET・MCP の応答、時計合わせの `before`・`after`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClockReading {
    /// 読んだ時刻（UTC の RFC3339、秒まで）。
    pub at: String,
    pub result: ClockReadResult,
    /// `device` の終了コード。
    pub code: Option<u32>,
    /// 人が読む文。
    pub message: String,
    /// PLC の現地時刻（`YYYY-MM-DDTHH:MM:SS`）。`ok` のときだけ。
    pub plc_time: Option<String>,
    /// broker の応答を受けた直後（送らなかったときは送らないと決めたとき）の
    /// Hub の PC の現地時刻を秒に切り捨てたもの（`YYYY-MM-DDTHH:MM:SS`）。
    pub pc_time: String,
    /// `plcTime − pcTime` の秒（PLC が進んでいれば正）。`ok` のときだけ。両方とも
    /// 秒に切り捨てなので ±1 s の誤差がある。
    pub offset_seconds: Option<i64>,
}

/// REST の GET・MCP の応答: [`ClockReading`] に `cached` を足したもの。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClockReadView {
    #[serde(flatten)]
    pub reading: ClockReading,
    /// 直近の結果を使い回した（この要求では PLC に送っていない）。
    pub cached: bool,
}

/// broker の結果を `(result, code, message, PLC の時刻)` に写す（純関数、表で試験する）。
pub fn classify_read(
    outcome: &ClockReadOutcome,
) -> (ClockReadResult, Option<u32>, String, Option<CivilTime>) {
    match outcome {
        Ok(Some(Ok(time))) => (
            ClockReadResult::Ok,
            None,
            "PLC の時計を読みました".into(),
            Some(*time),
        ),
        Ok(Some(Err(err))) => match err {
            ItemError::Unsupported(_) => (ClockReadResult::Unsupported, None, err.to_string(), None),
            ItemError::Rejected(_) => (ClockReadResult::Rejected, None, err.to_string(), None),
            ItemError::Device(device) => (
                ClockReadResult::Device,
                Some(device.code),
                err.to_string(),
                None,
            ),
            // `ItemError` の文（「書込み後の確認読み」）は読取りに合わないので自前の文に。
            ItemError::VerificationFailed(message) => (
                ClockReadResult::VerificationFailed,
                None,
                format!("PLC から読んだ値が時計として正しくありません: {message}"),
                None,
            ),
            // 将来の分類（`non_exhaustive`）は結果不明として扱う。
            other => (ClockReadResult::Unknown, None, other.to_string(), None),
        },
        Ok(None) => (
            ClockReadResult::NotOffered,
            None,
            "この接続のドライバは時計の読取りを出しません（何も送っていません）".into(),
            None,
        ),
        Err(
            err @ (BrokerError::Disconnected { .. }
            | BrokerError::UnsupportedProtocol { .. }
            | BrokerError::InvalidSettings { .. }),
        ) => (ClockReadResult::Unavailable, None, err.to_string(), None),
        Err(err @ (BrokerError::ConnectionFailed { .. } | BrokerError::TaskGone { .. })) => (
            ClockReadResult::Unknown,
            None,
            format!(
                "結果不明（読取りは PLC に何も書かないので時計には影響しません。接続は再接続されます）: {err}"
            ),
            None,
        ),
    }
}

/// `time` を秒に切り捨てる（ナノ秒を 0）。
pub fn truncate_to_second(time: NaiveDateTime) -> NaiveDateTime {
    time.with_nanosecond(0).unwrap_or(time)
}

/// `plc − pc`（秒）。`pc` は秒に切り捨てて比べる。PLC の時刻が暦にない
/// （ゲストが確かめるので普通は無い）ときは `None`。
pub fn offset_seconds(plc: &CivilTime, pc: NaiveDateTime) -> Option<i64> {
    let plc = NaiveDate::from_ymd_opt(i32::from(plc.year), plc.month.into(), plc.day.into())?
        .and_hms_opt(plc.hour.into(), plc.minute.into(), plc.second.into())?;
    Some((plc - truncate_to_second(pc)).num_seconds())
}

/// `NaiveDateTime` を `YYYY-MM-DDTHH:MM:SS`（秒に切り捨て）に。
pub fn local_time_text(time: NaiveDateTime) -> String {
    truncate_to_second(time)
        .format("%Y-%m-%dT%H:%M:%S")
        .to_string()
}

fn utc_now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// broker の結果と応答を受けた直後の PC の現地時刻 `pc` から記録を作る。
pub fn reading_from(outcome: &ClockReadOutcome, pc: NaiveDateTime) -> ClockReading {
    let (result, code, message, plc) = classify_read(outcome);
    ClockReading {
        at: utc_now_rfc3339(),
        result,
        code,
        message,
        plc_time: plc.as_ref().map(civil_time_text),
        pc_time: local_time_text(pc),
        offset_seconds: plc.as_ref().and_then(|plc| offset_seconds(plc, pc)),
    }
}

/// 送らなかった記録（`unavailable`）。`pc` は送らないと決めたときの現地時刻。
pub fn unavailable(message: &str, pc: NaiveDateTime) -> ClockReading {
    ClockReading {
        at: utc_now_rfc3339(),
        result: ClockReadResult::Unavailable,
        code: None,
        message: message.to_string(),
        plc_time: None,
        pc_time: local_time_text(pc),
        offset_seconds: None,
    }
}

/// 接続ごとの使い回しの枠。
#[derive(Default)]
struct Slot {
    /// 同じ接続の要求を 1 つずつ通す（同時の要求を 1 回の読取りにまとめる）。
    gate: tokio::sync::Mutex<()>,
    /// 直近の結果と読み終えた時刻。
    last: Mutex<Option<(Instant, ClockReading)>>,
    /// [`ClockReadCache::invalidate`] で進む世代。
    epoch: AtomicU64,
}

/// REST の GET・MCP の使い回し（モジュール doc の「使い回し」）。
#[derive(Default)]
pub struct ClockReadCache {
    slots: Mutex<HashMap<i64, Arc<Slot>>>,
}

impl ClockReadCache {
    pub fn new() -> Self {
        Self::default()
    }

    fn slot(&self, connection_id: i64) -> Arc<Slot> {
        self.slots
            .lock()
            .expect("clock read cache lock poisoned")
            .entry(connection_id)
            .or_default()
            .clone()
    }

    /// 接続 `connection_id` の直近の結果が [`CLOCK_READ_REUSE`] 以内ならそれを
    /// `(記録, true)` で、無ければ `read` で読んで `(記録, false)` を返す。同じ接続の
    /// 同時の要求は 1 つずつ通るので、後の要求は前の結果を使い回す。`unavailable` は
    /// 保存しない。
    pub async fn get_or_read<F, Fut>(&self, connection_id: i64, read: F) -> (ClockReading, bool)
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = ClockReading>,
    {
        let slot = self.slot(connection_id);
        let _gate = slot.gate.lock().await;
        if let Some((at, reading)) = slot
            .last
            .lock()
            .expect("clock read slot lock poisoned")
            .as_ref()
        {
            if at.elapsed() < CLOCK_READ_REUSE {
                return (reading.clone(), true);
            }
        }
        let epoch = slot.epoch.load(Ordering::SeqCst);
        let reading = read().await;
        let mut last = slot.last.lock().expect("clock read slot lock poisoned");
        if reading.result == ClockReadResult::Unavailable {
            *last = None;
        } else if slot.epoch.load(Ordering::SeqCst) == epoch {
            *last = Some((Instant::now(), reading.clone()));
        }
        (reading, false)
    }

    /// 接続 `connection_id` の使い回しを捨てる（時計合わせの後）。読取りの途中なら、
    /// その結果は保存されない。
    pub fn invalidate(&self, connection_id: i64) {
        let slot = self.slot(connection_id);
        slot.epoch.fetch_add(1, Ordering::SeqCst);
        *slot.last.lock().expect("clock read slot lock poisoned") = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use banto_driver_api::DeviceError;
    use serde_json::json;

    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, mo, d)
            .unwrap()
            .and_hms_opt(h, mi, s)
            .unwrap()
    }

    #[test]
    fn every_outcome_maps_to_its_result() {
        let time = CivilTime::new(2026, 11, 7, 13, 4, 59);
        let cases: Vec<(
            ClockReadOutcome,
            ClockReadResult,
            Option<u32>,
            Option<CivilTime>,
        )> = vec![
            (Ok(Some(Ok(time))), ClockReadResult::Ok, None, Some(time)),
            (
                Ok(Some(Err(ItemError::Unsupported("Q25PRH".into())))),
                ClockReadResult::Unsupported,
                None,
                None,
            ),
            (
                Ok(Some(Err(ItemError::Rejected("SM213 is OFF".into())))),
                ClockReadResult::Rejected,
                None,
                None,
            ),
            (
                Ok(Some(Err(ItemError::Device(DeviceError::new(
                    0xc059, "end",
                ))))),
                ClockReadResult::Device,
                Some(0xc059),
                None,
            ),
            (
                Ok(Some(Err(ItemError::VerificationFailed(
                    "SD210 is not BCD".into(),
                )))),
                ClockReadResult::VerificationFailed,
                None,
                None,
            ),
            (Ok(None), ClockReadResult::NotOffered, None, None),
            (
                Err(BrokerError::Disconnected { connection_id: 1 }),
                ClockReadResult::Unavailable,
                None,
                None,
            ),
            (
                Err(BrokerError::UnsupportedProtocol {
                    connection_id: 1,
                    protocol: "x".into(),
                }),
                ClockReadResult::Unavailable,
                None,
                None,
            ),
            (
                Err(BrokerError::InvalidSettings {
                    connection_id: 1,
                    reason: "x".into(),
                }),
                ClockReadResult::Unavailable,
                None,
                None,
            ),
            (
                Err(BrokerError::ConnectionFailed {
                    connection_id: 1,
                    reason: "timeout".into(),
                }),
                ClockReadResult::Unknown,
                None,
                None,
            ),
            (
                Err(BrokerError::TaskGone { connection_id: 1 }),
                ClockReadResult::Unknown,
                None,
                None,
            ),
        ];
        for (outcome, result, code, plc) in cases {
            let (got, got_code, message, got_plc) = classify_read(&outcome);
            assert_eq!((got, got_code, got_plc), (result, code, plc), "{outcome:?}");
            assert!(!message.is_empty());
        }
        let (_, _, message, _) = classify_read(&Err(BrokerError::ConnectionFailed {
            connection_id: 1,
            reason: "timeout".into(),
        }));
        assert!(message.contains("時計には影響しません"), "{message}");
        assert!(message.contains("再接続"), "{message}");
        let (_, _, message, _) = classify_read(&Ok(Some(Err(ItemError::VerificationFailed(
            "SD210 is not BCD".into(),
        )))));
        assert!(!message.contains("書込み"), "{message}");
        assert!(message.contains("SD210 is not BCD"), "{message}");
        assert_eq!(
            serde_json::to_value(ClockReadResult::VerificationFailed).unwrap(),
            json!("verificationFailed")
        );
        assert_eq!(
            serde_json::to_value(ClockReadResult::NotOffered).unwrap(),
            json!("notOffered")
        );
    }

    #[test]
    fn the_offset_is_plc_minus_pc_in_seconds() {
        let plc = CivilTime::new(2026, 11, 7, 13, 5, 2);
        // PLC が 3 秒進んでいる（PC の端数は切り捨てる）。
        let pc = at(2026, 11, 7, 13, 4, 59)
            .with_nanosecond(999_000_000)
            .unwrap();
        assert_eq!(offset_seconds(&plc, pc), Some(3));
        // PLC が遅れている。
        assert_eq!(offset_seconds(&plc, at(2026, 11, 7, 14, 5, 2)), Some(-3600));
        // 日付（年）をまたぐ: PLC は 2027-01-01 00:00:01、PC は 2026-12-31 23:59:58。
        let new_year = CivilTime::new(2027, 1, 1, 0, 0, 1);
        assert_eq!(
            offset_seconds(&new_year, at(2026, 12, 31, 23, 59, 58)),
            Some(3)
        );
        assert_eq!(offset_seconds(&new_year, at(2027, 1, 1, 0, 0, 3)), Some(-2));
        // 暦にない PLC の時刻は差を出さない。
        assert_eq!(
            offset_seconds(&CivilTime::new(2026, 2, 30, 0, 0, 0), pc),
            None
        );
    }

    #[test]
    fn a_reading_carries_both_times_and_the_offset_only_when_ok() {
        let pc = at(2026, 11, 7, 13, 4, 59)
            .with_nanosecond(500_000_000)
            .unwrap();
        let ok = reading_from(&Ok(Some(Ok(CivilTime::new(2026, 11, 7, 13, 5, 9)))), pc);
        assert_eq!(ok.result, ClockReadResult::Ok);
        assert_eq!(ok.plc_time.as_deref(), Some("2026-11-07T13:05:09"));
        assert_eq!(ok.pc_time, "2026-11-07T13:04:59");
        assert_eq!(ok.offset_seconds, Some(10));
        let json = serde_json::to_value(ClockReadView {
            reading: ok,
            cached: true,
        })
        .unwrap();
        for key in [
            "at",
            "result",
            "code",
            "message",
            "plcTime",
            "pcTime",
            "offsetSeconds",
            "cached",
        ] {
            assert!(json.get(key).is_some(), "{key}: {json}");
        }
        assert_eq!(json["cached"], true);

        let rejected = reading_from(
            &Ok(Some(Err(ItemError::Rejected("SM213 is OFF".into())))),
            pc,
        );
        assert_eq!(rejected.plc_time, None);
        assert_eq!(rejected.offset_seconds, None);
        assert_eq!(rejected.pc_time, "2026-11-07T13:04:59");
    }

    fn reading(result: ClockReadResult, message: &str) -> ClockReading {
        ClockReading {
            at: "2026-11-07T04:04:59Z".into(),
            result,
            code: None,
            message: message.into(),
            plc_time: None,
            pc_time: "2026-11-07T13:04:59".into(),
            offset_seconds: None,
        }
    }

    /// 5 秒以内は読まずに使い回す（結果が ok でなくても）。過ぎたら読む。
    #[tokio::test(start_paused = true)]
    async fn a_reading_is_reused_for_five_seconds() {
        let cache = ClockReadCache::new();
        let reads = Mutex::new(0u32);
        let read = || async {
            *reads.lock().unwrap() += 1;
            reading(ClockReadResult::Rejected, "SM213 is OFF")
        };
        let (first, cached) = cache.get_or_read(1, read).await;
        assert!(!cached);
        tokio::time::advance(CLOCK_READ_REUSE - Duration::from_millis(1)).await;
        let (again, cached) = cache.get_or_read(1, read).await;
        assert!(cached);
        assert_eq!(again, first);
        assert_eq!(*reads.lock().unwrap(), 1);
        // 別の接続は別の枠。
        assert!(!cache.get_or_read(2, read).await.1);
        assert_eq!(*reads.lock().unwrap(), 2);
        tokio::time::advance(Duration::from_millis(1)).await;
        assert!(!cache.get_or_read(1, read).await.1, "5 s passed");
        assert_eq!(*reads.lock().unwrap(), 3);
    }

    /// `unavailable`（何も送っていない）は使い回さない。
    #[tokio::test(start_paused = true)]
    async fn unavailable_is_not_reused() {
        let cache = ClockReadCache::new();
        let reads = Mutex::new(0u32);
        let read = || async {
            *reads.lock().unwrap() += 1;
            reading(ClockReadResult::Unavailable, "切断中")
        };
        assert!(!cache.get_or_read(1, read).await.1);
        assert!(!cache.get_or_read(1, read).await.1);
        assert_eq!(*reads.lock().unwrap(), 2);
    }

    /// 捨てたら次は読む。読取りの途中で捨てられたら、その結果は保存しない。
    #[tokio::test(start_paused = true)]
    async fn invalidate_drops_the_reading_and_one_in_flight() {
        let cache = Arc::new(ClockReadCache::new());
        let reads = Arc::new(Mutex::new(0u32));
        let counted = || {
            let reads = reads.clone();
            move || async move {
                *reads.lock().unwrap() += 1;
                reading(ClockReadResult::Ok, "ok")
            }
        };
        assert!(!cache.get_or_read(1, counted()).await.1);
        cache.invalidate(1);
        assert!(!cache.get_or_read(1, counted()).await.1, "dropped");
        assert_eq!(*reads.lock().unwrap(), 2);

        // 読取りの途中で捨てる。
        cache.invalidate(1);
        let release = Arc::new(tokio::sync::Notify::new());
        let entered = Arc::new(tokio::sync::Notify::new());
        let slow = {
            let (cache, release, entered) = (cache.clone(), release.clone(), entered.clone());
            tokio::spawn(async move {
                cache
                    .get_or_read(1, || async move {
                        entered.notify_one();
                        release.notified().await;
                        reading(ClockReadResult::Ok, "before the clock set")
                    })
                    .await
            })
        };
        entered.notified().await;
        cache.invalidate(1);
        release.notify_one();
        let (got, cached) = slow.await.unwrap();
        assert!(!cached);
        assert_eq!(got.message, "before the clock set");
        assert!(
            !cache.get_or_read(1, counted()).await.1,
            "the in-flight reading was not kept"
        );
        assert_eq!(*reads.lock().unwrap(), 3);
    }

    /// 同じ接続の同時の要求は 1 回の読取りにまとまり、後の要求は `cached: true`。
    #[tokio::test(start_paused = true)]
    async fn concurrent_requests_share_one_read() {
        let cache = Arc::new(ClockReadCache::new());
        let reads = Arc::new(Mutex::new(0u32));
        let read = || {
            let reads = reads.clone();
            move || async move {
                *reads.lock().unwrap() += 1;
                tokio::time::sleep(Duration::from_secs(1)).await;
                reading(ClockReadResult::Ok, "ok")
            }
        };
        let (a, b, c) = tokio::join!(
            cache.get_or_read(1, read()),
            cache.get_or_read(1, read()),
            cache.get_or_read(1, read()),
        );
        assert_eq!(*reads.lock().unwrap(), 1);
        let mut cached: Vec<bool> = [&a, &b, &c].iter().map(|(_, cached)| *cached).collect();
        cached.sort();
        assert_eq!(cached, vec![false, true, true]);
        assert_eq!(a.0, b.0);
        assert_eq!(b.0, c.0);
    }
}
