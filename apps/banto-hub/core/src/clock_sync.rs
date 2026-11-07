//! H2（tyaro/banto-industrial#1053 の H の後半、2026-11-07）: PLC の時計合わせの
//! Hub 側 - 接続ごとの設定と時刻表、定期の実行、手動の実行、監査、直近の結果。
//!
//! ## 役割の分け方（ゲストの PLAN.md「H の設計」「H の Hub との IF」）
//!
//! どのデバイスにどう書くか・機種ごとの違い・成否の見分け方はゲスト
//! （`clock-v1`、SLMP では SD210–SD216 と SM210）が持つ。Hub が持つのは
//! 接続先によらない部分: 時刻の元（この PC の現地時刻）、実行の契機（時刻表と
//! 手動）、許可（editor と接続ごとの `enabled`）、監査、直近の結果。
//! H1 で broker まで通した [`BrokerHandle::set_clock`] を呼ぶだけで、手順の
//! 上限（engine の `MAX_CLOCK_STEPS = 8`）と時間切れ（broker の
//! `clock_job_limit`）は下の層のものを使う。
//!
//! ## 二重のゲート
//!
//! 1. ゲストの設定 `clock_set`（既定は無効）。接続の設定なので保留キューを通る。
//!    無効なら送る前に `rejected`。
//! 2. この Hub の接続ごとの設定 `enabled`（既定は無効、`hub_clock_sync` 表）。
//!    **定期の実行だけ**を開け閉めする。手動の実行（`POST`）は editor の明示の
//!    操作なので `enabled` を問わず出せる（ゲートの 1 と、運用書込みの loader
//!    で読み込んだゲストだけが時計合わせを出すことは残る）。
//!
//! 時計合わせを出すのは、署名付きで書込みを明示的に選んだ運用書込みの loader の
//! ゲストだけ（H1、`banto-driver-wasm`）。それ以外の接続は `Ok(None)` で、
//! 結果は `notOffered`（何も送っていない）になる。
//!
//! ## 保存と即時反映
//!
//! 設定は `hub_clock_sync`（migration `0110`）に保存し、保留キュー
//! （pending_changes）を通さず PUT で即時に反映する。接続の設定（ドライバの
//! settings・カタログ）ではなく Hub の運用の時刻表だから。接続を消しても行は
//! 残る（孤児。接続の id は再利用されず、適用済みのカタログに無い接続は実行
//! しないので害はない）。設定パッケージ（export/import）には含めない。
//!
//! ## 時刻表とスケジューラ
//!
//! 時刻表は「毎時 m 分」（[`ClockSchedule::Hourly`]）か「毎日 hh:mm」
//! （[`ClockSchedule::Daily`]）だけ（cron は持たない）。時刻はこの PC の現地時刻
//! （`chrono::Local`）で、夏時間は考えない（ゲストの PLAN.md の作者決定）。
//!
//! スケジューラ（[`spawn_scheduler`]）は Hub の runtime で 1 本の tokio タスク。
//! [`SCHEDULER_POLL`] ごとに現地時刻を読み、**分（naive の `YYYY-MM-DD HH:MM`）が
//! 前に評価した分と違うときだけ**評価する（分の境目まで sleep しないので、
//! 時計の飛びで寝過ごさない）。評価は純関数 [`due_connections`]:
//!
//! - 有効な行のうち、**今の分**が時刻表に一致し、適用済みのカタログにあって
//!   有効な接続だけを出す。
//! - **追いつかない**: 今の分しか見ないので、一致した分のうちに評価できなかった
//!   回（Hub の停止中・PC のスリープ・時計の飛び越し）は後から出さない。
//! - **二重に出さない**: 接続ごとに「定期で出した分」の集合をメモリに持ち、集合に
//!   ある分では 2 回目を出さない（夏時間の巻き戻しや時計の手直しで、いくつか前の
//!   naive の分がもう一度来ても出さない）。出すと決めた時点で印を付けるので、実行中の
//!   回とも重ならない。集合は評価のたびに今の分の前後 26 時間（[`FIRED_KEEP`]）より
//!   離れた分を捨てる。夏時間の巻き戻しと日ごとの時刻表を覆いつつ、PC の時計が大きく
//!   ずれていて直されたときに時刻表が永久に止まらないようにするため。
//! - **実行を待たない**: 評価はループの中で行い、各回の実行は `JoinSet` に spawn
//!   する。1 回の時計合わせが長引いても（`clock_job_limit` は応答の時間切れに比例し、
//!   T = 5 s なら 81 s）次の分の評価と別の接続の回は遅れない。止める合図の後は
//!   新しい回を出さず、残る回を全部待ってから終わる（abort しない）。
//!
//! 今の時刻の取得は差し替えられる（[`ClockSync::set_local_now`]）。試験は実時間を
//! 待たずに分を進める。
//!
//! ## 実行（定期と手動で共通の [`ClockSync::run`]）
//!
//! - 書込み可能なフル `BrokerHandle` は非ダイヤルの peek
//!   （`HubSessions::write_handle_for`）で取る。新しくダイヤルしない。セッションが
//!   無い・切断中なら送らずに `unavailable`。
//! - 接続ごとの実行中の印で、同じ接続の時計合わせを重ねない（2 つ目は送らずに
//!   `busy`）。
//! - `CivilTime` は `set_clock` を呼ぶ**直前**に現地時刻から作る（秒は切り捨て、
//!   曜日は渡さない）。
//! - 時間切れは broker の `clock_job_limit` が持つ。Hub は外側に別の時間切れを
//!   足さない（足すと、捨てた後に broker のキューの中で送られる）。**自動で再送
//!   しない**（次の回は時刻表どおり）。
//! - 毎回（`busy`・`unavailable` も）監査し、直近の結果（メモリ、接続ごとに 1 件）を
//!   更新する。
//!
//! 結果の写像は純関数 [`classify`]（`Ok(Some(Ok))` → `ok`、`Unsupported` →
//! `unsupported`、`Rejected` → `rejected`、`Device` → `device`（終了コードを
//! `code`）、`VerificationFailed` → `verificationFailed`、送る前の broker の失敗
//! （`Disconnected` など）→ `unavailable`、送った後かもしれない失敗
//! （`ConnectionFailed`・`TaskGone`）→ `unknown`、`Ok(None)` → `notOffered`）。
//!
//! ## 監査
//!
//! `audit_log` に `resource = "plc_connection_clock_sync"`・`entity_id = 接続 id`。
//! 実行は `action = "clock_sync"`、`detail = {trigger, time, result, code, message}`
//! （`time` は渡した現地時刻 `YYYY-MM-DDTHH:MM:SS`、送らなかったときは `null`）。
//! 設定の変更は `action = "update"`、`detail = {enabled, schedule}`。`result` 列は
//! 成功だけ `ok`、ほかは `failed`。手動は `origin = "rest"` と操作者、定期は
//! `actor` なし・`origin = "schedule"`（`audit_log.origin` に CHECK は無い。既存の
//! 値は `rest`・`mcp`・`grpc`・`elev`）。
//!
//! ## 危険（設定の説明・OpenAPI にも書く）
//!
//! PLC 自身の SNTP・PLC のプログラム・GOT の時刻同期などが同じ時計を合わせて
//! いると互いに上書きする。毎回 PLC のイベント履歴に「時計設定」（イベント
//! コード 24000）が残るので、間隔を短くしすぎない。

use std::collections::{BTreeSet, HashMap, HashSet};
use std::future::Future;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use banto_broker::{BrokerConnectionStatus, BrokerError, ClockOutcome};
use banto_core::BantoError;
use banto_driver_api::{CivilTime, ItemError};
use chrono::{Datelike, NaiveDateTime, Timelike};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{Row, SqlitePool};
use tokio::sync::watch;
use tokio::task::{JoinHandle, JoinSet};
use utoipa::ToSchema;

use crate::audit::{AuditEntry, AuditLogService};
use crate::broker_glue::HubSessions;
use crate::hub::CollectorManager;

/// 監査の `resource`。
pub const AUDIT_RESOURCE: &str = "plc_connection_clock_sync";
/// 実行の監査の `action`。
pub const AUDIT_ACTION_RUN: &str = "clock_sync";
/// 定期の実行の監査の `origin`（`actor` なし）。
pub const SCHEDULE_ORIGIN: &str = "schedule";
/// スケジューラが現地時刻を読む間隔。分の境目まで sleep しない（時計の飛び）。
pub const SCHEDULER_POLL: Duration = Duration::from_secs(1);

/// この PC の現地時刻を返す関数（試験で差し替える）。
pub type LocalNow = Arc<dyn Fn() -> NaiveDateTime + Send + Sync>;

/// 既定の現地時刻（`chrono::Local`）。
pub fn system_local_now() -> NaiveDateTime {
    chrono::Local::now().naive_local()
}

/// 時刻表（cron は持たない）。JSON は `{"kind":"hourly","minute":m}` と
/// `{"kind":"daily","hour":h,"minute":m}`。範囲は [`Self::validate`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum ClockSchedule {
    /// 毎時 `minute` 分。
    Hourly {
        #[schema(maximum = 59)]
        minute: u8,
    },
    /// 毎日 `hour`:`minute`。
    Daily {
        #[schema(maximum = 23)]
        hour: u8,
        #[schema(maximum = 59)]
        minute: u8,
    },
}

impl ClockSchedule {
    /// 範囲（分 0–59、時 0–23）。
    pub fn validate(&self) -> Result<(), String> {
        let (hour, minute) = match *self {
            ClockSchedule::Hourly { minute } => (None, minute),
            ClockSchedule::Daily { hour, minute } => (Some(hour), minute),
        };
        if minute > 59 {
            return Err(format!(
                "schedule.minute は 0〜59 で指定してください（{minute}）"
            ));
        }
        if let Some(hour) = hour.filter(|hour| *hour > 23) {
            return Err(format!(
                "schedule.hour は 0〜23 で指定してください（{hour}）"
            ));
        }
        Ok(())
    }

    /// 現地の分 `minute`（秒は見ない）がこの時刻表に一致するか。
    pub fn matches(&self, minute: NaiveDateTime) -> bool {
        match *self {
            ClockSchedule::Hourly { minute: m } => minute.minute() == u32::from(m),
            ClockSchedule::Daily { hour, minute: m } => {
                minute.hour() == u32::from(hour) && minute.minute() == u32::from(m)
            }
        }
    }
}

/// 接続ごとの設定（REST の PUT の本文・GET の `settings`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClockSyncSettings {
    /// 定期の実行を出すか（既定は無効）。手動の実行はこれを問わない。
    pub enabled: bool,
    pub schedule: ClockSchedule,
}

impl Default for ClockSyncSettings {
    /// 行が無いときの値: 無効・毎時 5 分（ゲストの PLAN.md の例）。
    fn default() -> Self {
        ClockSyncSettings {
            enabled: false,
            schedule: ClockSchedule::Hourly { minute: 5 },
        }
    }
}

/// PUT の本文を読む（未知のキー・型の違い・範囲外はどれも `Err`、REST は 400）。
pub fn parse_settings(body: &[u8]) -> Result<ClockSyncSettings, String> {
    let settings: ClockSyncSettings = serde_json::from_slice(body)
        .map_err(|err| format!("時計合わせの設定の形が不正です: {err}"))?;
    settings.schedule.validate()?;
    Ok(settings)
}

/// 実行の契機。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum ClockTrigger {
    /// 時刻表による定期の実行。
    Schedule,
    /// REST の `POST /api/plc-connections/{id}/clock-sync`。
    Manual,
}

/// 1 回の結果（JSON の `result`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum ClockResult {
    /// 合わせた。
    Ok,
    /// ドライバ・機種が扱わない（標準 Q など）。何も変えていない。
    Unsupported,
    /// ドライバが送る前に断った（ゲストの `clock_set` が無効、SM210 が残っている
    /// など）。
    Rejected,
    /// 機器の終了コード（`code`）。
    Device,
    /// 確かめられなかった（SM210 が ON のまま）。時計が変わったかは分からない。
    VerificationFailed,
    /// 送らなかった（セッションが無い・切断中、broker が送る前に断った）。
    Unavailable,
    /// 結果不明（送った後かもしれない致命・時間切れ）。接続は再接続される。
    /// 自動で再送しない。
    Unknown,
    /// このドライバ・loader は時計合わせを出さない。何も送っていない。
    NotOffered,
    /// 同じ接続の時計合わせが実行中。送らなかった。
    Busy,
}

impl ClockResult {
    /// 監査の `result` 列（成功だけ `ok`）。
    pub fn audit_result(self) -> &'static str {
        if self == ClockResult::Ok {
            "ok"
        } else {
            "failed"
        }
    }
}

/// 1 回の実行の記録（直近の結果・POST の応答・監査の `detail`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClockSyncRecord {
    /// 実行した時刻（UTC の RFC3339、秒まで）。
    pub at: String,
    pub trigger: ClockTrigger,
    /// 渡した現地時刻（`YYYY-MM-DDTHH:MM:SS`）。送らなかったときは `None`。
    pub time: Option<String>,
    pub result: ClockResult,
    /// `device` の終了コード。
    pub code: Option<u32>,
    /// 人が読む文。
    pub message: String,
}

impl ClockSyncRecord {
    fn detail(&self) -> Value {
        json!({
            "trigger": self.trigger,
            "time": self.time,
            "result": self.result,
            "code": self.code,
            "message": self.message,
        })
    }
}

/// broker の結果を `(result, code, message)` に写す（純関数、表で試験する）。
pub fn classify(outcome: &ClockOutcome) -> (ClockResult, Option<u32>, String) {
    match outcome {
        Ok(Some(Ok(()))) => (ClockResult::Ok, None, "PLC の時計を合わせました".into()),
        Ok(Some(Err(err))) => match err {
            ItemError::Unsupported(_) => (ClockResult::Unsupported, None, err.to_string()),
            ItemError::Rejected(_) => (ClockResult::Rejected, None, err.to_string()),
            ItemError::Device(device) => (ClockResult::Device, Some(device.code), err.to_string()),
            ItemError::VerificationFailed(_) => {
                (ClockResult::VerificationFailed, None, err.to_string())
            }
            // 将来の分類（`non_exhaustive`）は結果不明として扱う。
            other => (ClockResult::Unknown, None, other.to_string()),
        },
        Ok(None) => (
            ClockResult::NotOffered,
            None,
            "この接続のドライバは時計合わせを出しません（何も送っていません）".into(),
        ),
        Err(
            err @ (BrokerError::Disconnected { .. }
            | BrokerError::UnsupportedProtocol { .. }
            | BrokerError::InvalidSettings { .. }),
        ) => (ClockResult::Unavailable, None, err.to_string()),
        Err(err @ (BrokerError::ConnectionFailed { .. } | BrokerError::TaskGone { .. })) => (
            ClockResult::Unknown,
            None,
            format!("結果不明（時計が変わったかは分かりません。自動では再送しません）: {err}"),
        ),
    }
}

/// `minute` を分に切り詰める（秒・ナノ秒を 0）。
pub fn truncate_to_minute(time: NaiveDateTime) -> NaiveDateTime {
    time.with_second(0)
        .and_then(|t| t.with_nanosecond(0))
        .unwrap_or(time)
}

/// 現地時刻を `CivilTime` に（秒は切り捨て、曜日は渡さない）。
pub fn civil_time(now: NaiveDateTime) -> CivilTime {
    // `NaiveDateTime` の年は chrono の範囲で、u16 に収まらない年は
    // ドライバが暦の検査で断る（0 にして送らせる）。
    let year = u16::try_from(now.year()).unwrap_or(0);
    // 月・日・時・分・秒は chrono が範囲を保証する（1–12 など）。
    CivilTime::new(
        year,
        now.month() as u8,
        now.day() as u8,
        now.hour() as u8,
        now.minute() as u8,
        now.second() as u8,
    )
}

/// 監査・応答に載せる渡した時刻の文字列（`YYYY-MM-DDTHH:MM:SS`）。
pub fn civil_time_text(time: &CivilTime) -> String {
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        time.year, time.month, time.day, time.hour, time.minute, time.second
    )
}

fn utc_now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// 定期の評価（純関数）: `rows`（`hub_clock_sync` の有効な行）のうち、今の分
/// `minute` が時刻表に一致し、`runnable`（適用済みのカタログにある有効な接続）に
/// 含まれ、`fired`（接続ごとの出した分の集合）に無いものの id を返し、`fired` に
/// 入れる。今の分しか見ないので、過ぎた回は出さない（追いつかない）。評価のたびに、
/// 今の分から [`FIRED_KEEP`] より離れた分を `fired` から捨てる。
pub fn due_connections(
    rows: &[(i64, ClockSchedule)],
    runnable: &HashSet<i64>,
    minute: NaiveDateTime,
    fired: &mut FiredMinutes,
) -> Vec<i64> {
    let minute = truncate_to_minute(minute);
    fired.retain(|_, minutes| {
        minutes.retain(|m| (*m - minute).abs() <= FIRED_KEEP);
        !minutes.is_empty()
    });
    let mut due = Vec::new();
    for (id, schedule) in rows {
        if !runnable.contains(id) || !schedule.matches(minute) {
            continue;
        }
        if fired.entry(*id).or_default().insert(minute) {
            due.push(*id);
        }
    }
    fired.retain(|_, minutes| !minutes.is_empty());
    due
}

/// 接続ごとの、定期で出した分（naive の現地時刻、分に切り捨て）の集合。
pub type FiredMinutes = HashMap<i64, BTreeSet<NaiveDateTime>>;

/// 出した分を覚えておく幅（今の分の前後）。夏時間の巻き戻しと日ごとの時刻表を
/// 覆う 24 時間に余裕を足した 26 時間。これより離れた分は捨てるので、PC の時計が
/// 大きくずれていて直されても、その時刻表が永久に止まることはない。
pub const FIRED_KEEP: chrono::TimeDelta = chrono::TimeDelta::hours(26);

/// 手動の実行の操作者（監査の `actor_username`・`actor_role`）。
#[derive(Debug, Clone, Copy)]
pub struct ClockActor<'a> {
    pub username: &'a str,
    pub role: &'a str,
}

/// 実行中の印を外すガード（future が捨てられても外す）。
struct RunningGuard<'a> {
    running: &'a Mutex<HashSet<i64>>,
    id: i64,
}

impl Drop for RunningGuard<'_> {
    fn drop(&mut self) {
        self.running
            .lock()
            .expect("clock sync running lock poisoned")
            .remove(&self.id);
    }
}

/// 時計合わせの状態（`CollectorManager` が 1 つ持つ。[`CollectorManager::clock_sync`]）。
/// 設定は DB、直近の結果・実行中の印・定期で最後に出した分はメモリだけ。
pub struct ClockSync {
    pool: SqlitePool,
    now: RwLock<LocalNow>,
    last: Mutex<HashMap<i64, ClockSyncRecord>>,
    running: Mutex<HashSet<i64>>,
    fired: Mutex<FiredMinutes>,
}

impl ClockSync {
    pub fn new(pool: SqlitePool) -> Self {
        ClockSync {
            pool,
            now: RwLock::new(Arc::new(system_local_now)),
            last: Mutex::new(HashMap::new()),
            running: Mutex::new(HashSet::new()),
            fired: Mutex::new(HashMap::new()),
        }
    }

    /// 現地時刻の元を差し替える（試験用。製品は [`system_local_now`] のまま）。
    pub fn set_local_now(&self, now: LocalNow) {
        *self.now.write().expect("clock sync now lock poisoned") = now;
    }

    /// 今の現地時刻。
    pub fn local_now(&self) -> NaiveDateTime {
        let now = self
            .now
            .read()
            .expect("clock sync now lock poisoned")
            .clone();
        now()
    }

    /// 接続 `connection_id` の設定（行が無ければ既定）。
    pub async fn settings(&self, connection_id: i64) -> Result<ClockSyncSettings, BantoError> {
        let row = sqlx::query(
            "SELECT enabled, schedule_kind, hour, minute FROM hub_clock_sync WHERE connection_id = ?",
        )
        .bind(connection_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(banto_storage::storage_error)?;
        match row {
            None => Ok(ClockSyncSettings::default()),
            Some(row) => {
                let enabled: i64 = row.get("enabled");
                Ok(ClockSyncSettings {
                    enabled: enabled != 0,
                    schedule: schedule_from_row(&row)?,
                })
            }
        }
    }

    /// 設定を保存する（無ければ作る）。`settings` は検証済みであること。
    pub async fn save_settings(
        &self,
        connection_id: i64,
        settings: &ClockSyncSettings,
    ) -> Result<(), BantoError> {
        let (kind, hour, minute) = match settings.schedule {
            ClockSchedule::Hourly { minute } => ("hourly", None, minute),
            ClockSchedule::Daily { hour, minute } => ("daily", Some(i64::from(hour)), minute),
        };
        sqlx::query(
            "INSERT INTO hub_clock_sync (connection_id, enabled, schedule_kind, hour, minute) \
             VALUES (?, ?, ?, ?, ?) \
             ON CONFLICT (connection_id) DO UPDATE SET enabled = excluded.enabled, \
             schedule_kind = excluded.schedule_kind, hour = excluded.hour, \
             minute = excluded.minute, updated_at = datetime('now')",
        )
        .bind(connection_id)
        .bind(i64::from(settings.enabled))
        .bind(kind)
        .bind(hour)
        .bind(i64::from(minute))
        .execute(&self.pool)
        .await
        .map_err(banto_storage::storage_error)?;
        Ok(())
    }

    /// 有効な行（定期の評価の入力）。
    pub async fn enabled_schedules(&self) -> Result<Vec<(i64, ClockSchedule)>, BantoError> {
        let rows = sqlx::query(
            "SELECT connection_id, schedule_kind, hour, minute FROM hub_clock_sync \
             WHERE enabled = 1 ORDER BY connection_id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(banto_storage::storage_error)?;
        rows.iter()
            .map(|row| Ok((row.get::<i64, _>("connection_id"), schedule_from_row(row)?)))
            .collect()
    }

    /// 接続 `connection_id` の直近の結果（メモリだけ。Hub の再起動で消える）。
    pub fn last(&self, connection_id: i64) -> Option<ClockSyncRecord> {
        self.last
            .lock()
            .expect("clock sync last lock poisoned")
            .get(&connection_id)
            .cloned()
    }

    /// 1 回実行する（定期と手動で共通）。毎回、監査と直近の結果を更新する。
    /// `actor` は手動のときの操作者（定期は `None`）。
    pub async fn run(
        &self,
        sessions: &HubSessions,
        audit: &AuditLogService,
        connection_id: i64,
        trigger: ClockTrigger,
        actor: Option<ClockActor<'_>>,
    ) -> ClockSyncRecord {
        let record = self.run_inner(sessions, connection_id, trigger).await;
        self.last
            .lock()
            .expect("clock sync last lock poisoned")
            .insert(connection_id, record.clone());
        let entity_id = connection_id.to_string();
        audit
            .record(AuditEntry {
                actor_username: actor.map(|a| a.username),
                actor_role: actor.map(|a| a.role),
                action: AUDIT_ACTION_RUN,
                resource: AUDIT_RESOURCE,
                entity_id: Some(&entity_id),
                detail: Some(record.detail()),
                origin: match trigger {
                    ClockTrigger::Schedule => SCHEDULE_ORIGIN,
                    ClockTrigger::Manual => "rest",
                },
                result: record.result.audit_result(),
            })
            .await;
        record
    }

    async fn run_inner(
        &self,
        sessions: &HubSessions,
        connection_id: i64,
        trigger: ClockTrigger,
    ) -> ClockSyncRecord {
        let not_sent = |result: ClockResult, message: &str| ClockSyncRecord {
            at: utc_now_rfc3339(),
            trigger,
            time: None,
            result,
            code: None,
            message: message.to_string(),
        };
        let inserted = self
            .running
            .lock()
            .expect("clock sync running lock poisoned")
            .insert(connection_id);
        if !inserted {
            return not_sent(
                ClockResult::Busy,
                "この接続の時計合わせが実行中です（送っていません）",
            );
        }
        let _guard = RunningGuard {
            running: &self.running,
            id: connection_id,
        };
        // 非ダイヤルの peek（新しくダイヤルしない。`HubSessions::write_handle_for`）。
        let Some(handle) = sessions.write_handle_for(connection_id) else {
            return not_sent(
                ClockResult::Unavailable,
                "この接続の broker セッションがありません（収集が止まっているか、接続が無効です。送っていません）",
            );
        };
        if *handle.status_watch().borrow() != BrokerConnectionStatus::Connected {
            return not_sent(
                ClockResult::Unavailable,
                "この接続は切断中です（送っていません）",
            );
        }
        // 呼ぶ直前に現地時刻から作る（秒は切り捨て、曜日は渡さない）。
        let time = civil_time(self.local_now());
        let outcome = handle.set_clock(time).await;
        let (result, code, message) = classify(&outcome);
        let sent = !matches!(result, ClockResult::Unavailable | ClockResult::NotOffered);
        ClockSyncRecord {
            at: utc_now_rfc3339(),
            trigger,
            // 渡した時刻。送らなかった（broker が送る前に断った・時計合わせを出さない）
            // ときは載せない。
            time: sent.then(|| civil_time_text(&time)),
            result,
            code,
            message,
        }
    }

    /// 定期の 1 回の評価: 今の分 `minute` に出す接続を [`due_connections`] で決め、
    /// 印を付けて id を返す（実行はしない。スケジューラが接続ごとに [`Self::run`] を
    /// 別のタスクで出す）。DB を読めなければ何も出さずにログに残す。
    pub async fn due_now(&self, manager: &CollectorManager, minute: NaiveDateTime) -> Vec<i64> {
        let rows = match self.enabled_schedules().await {
            Ok(rows) => rows,
            Err(err) => {
                eprintln!("banto-hub: 時計合わせの時刻表を読めませんでした: {err}");
                return Vec::new();
            }
        };
        if rows.is_empty() {
            return Vec::new();
        }
        let (_, applied) = manager.diagnostic_registry_snapshot();
        let runnable: HashSet<i64> = applied
            .connections
            .iter()
            .filter(|c| c.enabled)
            .map(|c| c.id)
            .collect();
        let mut fired = self.fired.lock().expect("clock sync fired lock poisoned");
        due_connections(&rows, &runnable, minute, &mut fired)
    }
}

fn schedule_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<ClockSchedule, BantoError> {
    let kind: String = row.get("schedule_kind");
    let hour: Option<i64> = row.get("hour");
    let minute: i64 = row.get("minute");
    let byte = |value: i64| {
        u8::try_from(value)
            .map_err(|_| BantoError::Other(format!("hub_clock_sync の値が範囲外です: {value}")))
    };
    let schedule = match (kind.as_str(), hour) {
        ("hourly", None) => ClockSchedule::Hourly {
            minute: byte(minute)?,
        },
        ("daily", Some(hour)) => ClockSchedule::Daily {
            hour: byte(hour)?,
            minute: byte(minute)?,
        },
        _ => {
            return Err(BantoError::Other(format!(
                "hub_clock_sync の時刻表が不正です: {kind}"
            )))
        }
    };
    schedule.validate().map_err(BantoError::Other)?;
    Ok(schedule)
}

/// GET・PUT・MCP の応答 `{"settings": {...}, "last": 直近の結果 | null}`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClockSyncView {
    pub settings: ClockSyncSettings,
    /// 直近の結果（メモリだけ。Hub の再起動で消える）。まだ無ければ `null`。
    pub last: Option<ClockSyncRecord>,
}

impl ClockSync {
    /// 接続 `connection_id` の設定と直近の結果。
    pub async fn view(&self, connection_id: i64) -> Result<ClockSyncView, BantoError> {
        Ok(ClockSyncView {
            settings: self.settings(connection_id).await?,
            last: self.last(connection_id),
        })
    }
}

/// 動いているスケジューラ（[`spawn_scheduler`]）。止めるのは合図
/// （[`Self::signal_stop`]）で、`abort` しない（DB を触る背景タスク、
/// docs/implementation-checklist.md §5）。実行中の回は終わりまで走る（上限は
/// broker の `clock_job_limit`。broker を止めればすぐ終わる）。タスクは合図の後、
/// 走っている回を全部待ってから終わる。
pub struct ClockSyncScheduler {
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
}

impl ClockSyncScheduler {
    /// 止める合図を出す（待たない）。以後、新しい回は出さない。
    pub fn signal_stop(&self) {
        let _ = self.stop.send(true);
    }

    /// 合図を出し、タスクの終わりを `limit` まで待つ。待ちきれなければ `false`
    /// （handle は持ったまま返らないので、呼び出し側はログに残すだけでよい。
    /// タスクは合図を見て、走っている回を全部待ってから終わるので、`limit` が全体の
    /// 上限になる）。
    pub async fn stop_and_join(&mut self, limit: Duration) -> bool {
        self.signal_stop();
        tokio::time::timeout(limit, &mut self.task).await.is_ok()
    }
}

/// スケジューラを起こす（[`SCHEDULER_POLL`] ごとに分を見る）。
pub fn spawn_scheduler(
    manager: Arc<CollectorManager>,
    audit: AuditLogService,
) -> ClockSyncScheduler {
    spawn_scheduler_with(manager, audit, SCHEDULER_POLL)
}

/// [`spawn_scheduler`] の間隔を選べる版（試験用）。
pub fn spawn_scheduler_with(
    manager: Arc<CollectorManager>,
    audit: AuditLogService,
    poll: Duration,
) -> ClockSyncScheduler {
    let now = manager.clone();
    let due = manager.clone();
    spawn_loop(
        move || now.clock_sync().local_now(),
        move |minute| {
            let manager = due.clone();
            async move { manager.clock_sync().due_now(&manager, minute).await }
        },
        move |id| {
            let manager = manager.clone();
            let audit = audit.clone();
            async move {
                manager
                    .clock_sync()
                    .run(manager.sessions(), &audit, id, ClockTrigger::Schedule, None)
                    .await;
            }
        },
        poll,
    )
}

/// スケジューラの本体（broker を使わずに試験できるよう、時刻・評価・実行を受け取る）。
///
/// 評価（`due`、印を付けるところ）はループの中で待つが、各回の実行（`run`）は
/// [`JoinSet`] に spawn してループは待たない。1 回の時計合わせが長引いても
/// （`clock_job_limit` は応答の時間切れに比例し、分を超えうる）、次の分の評価と
/// 別の接続の回は遅れない（追いつかない設計なので、遅れると永久に飛ぶ）。同じ接続の
/// 重なりは実行中の印（`busy`）で防ぐ。終わった回は毎回 `try_join_next` で回収する。
/// 止める合図の後は新しい回を出さず、残る回を全部 join してから終わる（abort しない）。
fn spawn_loop<N, D, DF, R, RF>(now: N, due: D, run: R, poll: Duration) -> ClockSyncScheduler
where
    N: Fn() -> NaiveDateTime + Send + 'static,
    D: Fn(NaiveDateTime) -> DF + Send + 'static,
    DF: Future<Output = Vec<i64>> + Send,
    R: Fn(i64) -> RF + Send + 'static,
    RF: Future<Output = ()> + Send + 'static,
{
    let (stop, mut stop_rx) = watch::channel(false);
    let task = tokio::spawn(async move {
        let mut runs = JoinSet::new();
        let mut evaluated: Option<NaiveDateTime> = None;
        loop {
            while let Some(done) = runs.try_join_next() {
                report_run_end(done);
            }
            if *stop_rx.borrow_and_update() {
                break;
            }
            let minute = truncate_to_minute(now());
            if evaluated != Some(minute) {
                evaluated = Some(minute);
                let ids = due(minute).await;
                // 評価を待つ間に止める合図が来ていたら、新しい回は出さない。
                if *stop_rx.borrow() {
                    break;
                }
                for id in ids {
                    runs.spawn(run(id));
                }
            }
            tokio::select! {
                changed = stop_rx.changed() => {
                    if changed.is_err() {
                        break;
                    }
                }
                _ = tokio::time::sleep(poll) => {}
            }
        }
        while let Some(done) = runs.join_next().await {
            report_run_end(done);
        }
    });
    ClockSyncScheduler { stop, task }
}

fn report_run_end(done: Result<(), tokio::task::JoinError>) {
    if let Err(err) = done {
        eprintln!("banto-hub: 時計合わせの定期の回が異常終了しました: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use banto_driver_api::DeviceError;
    use chrono::NaiveDate;

    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, mo, d)
            .unwrap()
            .and_hms_opt(h, mi, s)
            .unwrap()
    }

    #[test]
    fn hourly_and_daily_match_at_the_boundaries() {
        let hourly0 = ClockSchedule::Hourly { minute: 0 };
        let hourly59 = ClockSchedule::Hourly { minute: 59 };
        assert!(hourly0.matches(at(2026, 11, 7, 0, 0, 0)));
        assert!(hourly0.matches(at(2026, 11, 7, 23, 0, 59)));
        assert!(!hourly0.matches(at(2026, 11, 7, 23, 1, 0)));
        assert!(hourly59.matches(at(2026, 11, 7, 13, 59, 30)));
        assert!(!hourly59.matches(at(2026, 11, 7, 13, 58, 59)));

        let midnight = ClockSchedule::Daily { hour: 0, minute: 0 };
        let last = ClockSchedule::Daily {
            hour: 23,
            minute: 59,
        };
        assert!(midnight.matches(at(2026, 11, 8, 0, 0, 5)));
        assert!(!midnight.matches(at(2026, 11, 8, 1, 0, 0)));
        assert!(!midnight.matches(at(2026, 11, 8, 0, 1, 0)));
        assert!(last.matches(at(2026, 12, 31, 23, 59, 59)));
        assert!(!last.matches(at(2026, 12, 31, 22, 59, 0)));
        assert!(!last.matches(at(2026, 12, 31, 23, 58, 0)));
    }

    #[test]
    fn settings_parse_rejects_unknown_keys_and_out_of_range() {
        let ok = parse_settings(
            br#"{"enabled":true,"schedule":{"kind":"daily","hour":23,"minute":59}}"#,
        )
        .unwrap();
        assert_eq!(
            ok,
            ClockSyncSettings {
                enabled: true,
                schedule: ClockSchedule::Daily {
                    hour: 23,
                    minute: 59
                }
            }
        );
        assert_eq!(
            parse_settings(br#"{"enabled":false,"schedule":{"kind":"hourly","minute":0}}"#)
                .unwrap()
                .schedule,
            ClockSchedule::Hourly { minute: 0 }
        );
        for bad in [
            &br#"{"enabled":true,"schedule":{"kind":"hourly","minute":60}}"#[..],
            br#"{"enabled":true,"schedule":{"kind":"daily","hour":24,"minute":0}}"#,
            br#"{"enabled":true,"schedule":{"kind":"daily","hour":-1,"minute":0}}"#,
            br#"{"enabled":true,"schedule":{"kind":"hourly","minute":5,"hour":1}}"#,
            br#"{"enabled":true,"schedule":{"kind":"daily","minute":5}}"#,
            br#"{"enabled":true,"schedule":{"kind":"cron","minute":5}}"#,
            br#"{"enabled":true,"schedule":{"kind":"hourly","minute":5},"extra":1}"#,
            br#"{"enabled":"yes","schedule":{"kind":"hourly","minute":5}}"#,
            br#"{"schedule":{"kind":"hourly","minute":5}}"#,
            br#"{"enabled":true}"#,
            br#"not json"#,
        ] {
            assert!(
                parse_settings(bad).is_err(),
                "{}",
                String::from_utf8_lossy(bad)
            );
        }
    }

    #[test]
    fn default_settings_are_disabled_hourly_at_five() {
        assert_eq!(
            serde_json::to_value(ClockSyncSettings::default()).unwrap(),
            json!({"enabled": false, "schedule": {"kind": "hourly", "minute": 5}})
        );
    }

    #[test]
    fn due_fires_once_per_minute_and_never_catches_up() {
        let rows = vec![
            (1, ClockSchedule::Hourly { minute: 5 }),
            (2, ClockSchedule::Daily { hour: 9, minute: 5 }),
            (3, ClockSchedule::Hourly { minute: 5 }),
        ];
        // 3 は適用済みのカタログに無い（または無効）。
        let runnable: HashSet<i64> = [1, 2].into();
        let mut fired = FiredMinutes::new();
        assert_eq!(
            due_connections(&rows, &runnable, at(2026, 11, 7, 9, 5, 0), &mut fired),
            vec![1, 2]
        );
        // 同じ分の 2 回目の評価（秒が進んだ）では出さない。
        assert!(
            due_connections(&rows, &runnable, at(2026, 11, 7, 9, 5, 59), &mut fired).is_empty()
        );
        // 次の分は時刻表に合わない。
        assert!(due_connections(&rows, &runnable, at(2026, 11, 7, 9, 6, 0), &mut fired).is_empty());
        // 10:05 は毎時の 1 だけ。
        assert_eq!(
            due_connections(&rows, &runnable, at(2026, 11, 7, 10, 5, 1), &mut fired),
            vec![1]
        );
        // 巻き戻し（夏時間・時計の手直し）で同じ naive の分が再び来ても出さない。
        assert!(
            due_connections(&rows, &runnable, at(2026, 11, 7, 10, 5, 30), &mut fired).is_empty()
        );
        // 分を飛び越した（11:04 の次に 11:06 を評価）なら 11:05 は後から出さない。
        assert!(
            due_connections(&rows, &runnable, at(2026, 11, 7, 11, 4, 0), &mut fired).is_empty()
        );
        assert!(
            due_connections(&rows, &runnable, at(2026, 11, 7, 11, 6, 0), &mut fired).is_empty()
        );
        // 翌日の 9:05 は両方。
        assert_eq!(
            due_connections(&rows, &runnable, at(2026, 11, 8, 9, 5, 0), &mut fired),
            vec![1, 2]
        );
    }

    /// 2 つ以上前の分へ巻き戻しても出さない。26 時間より離れた分は捨てる。
    #[test]
    fn rewinding_past_several_fired_minutes_does_not_fire_again() {
        let rows = vec![(1, ClockSchedule::Hourly { minute: 5 })];
        let runnable: HashSet<i64> = [1].into();
        let mut fired = FiredMinutes::new();
        let mut due = |time| due_connections(&rows, &runnable, time, &mut fired);
        assert_eq!(due(at(2026, 11, 7, 9, 5, 0)), vec![1]);
        assert_eq!(due(at(2026, 11, 7, 10, 5, 0)), vec![1]);
        assert!(due(at(2026, 11, 7, 9, 5, 0)).is_empty(), "09:05 again");
        // 時計が大きく進んでいて直された: 2099 年の分の後でも 2026 年の分は出る
        // （2099 年から見て 2026 年の分は 26 時間より前なので捨ててある）。
        assert_eq!(due(at(2099, 1, 1, 0, 5, 0)), vec![1]);
        assert_eq!(due(at(2026, 11, 7, 11, 5, 0)), vec![1]);
        assert_eq!(due(at(2026, 11, 7, 10, 5, 0)), vec![1], "2026 was dropped");
        assert!(due(at(2026, 11, 7, 11, 5, 0)).is_empty());
        // 26 時間ちょうどは覚えている、それより離れると捨てる。
        assert_eq!(due(at(2026, 11, 8, 13, 5, 0)), vec![1]);
        assert!(due(at(2026, 11, 7, 11, 5, 0)).is_empty(), "26 h is kept");
        assert_eq!(due(at(2026, 11, 8, 14, 5, 0)), vec![1]);
        assert_eq!(due(at(2026, 11, 7, 11, 5, 0)), vec![1], "27 h is dropped");
    }

    #[test]
    fn civil_time_truncates_seconds_fraction_and_formats() {
        let now = at(2026, 11, 7, 9, 5, 30)
            .with_nanosecond(999_000_000)
            .unwrap();
        let time = civil_time(now);
        assert_eq!(time, CivilTime::new(2026, 11, 7, 9, 5, 30));
        assert_eq!(civil_time_text(&time), "2026-11-07T09:05:30");
        assert_eq!(truncate_to_minute(now), at(2026, 11, 7, 9, 5, 0));
    }

    #[test]
    fn every_outcome_maps_to_its_result() {
        let cases: Vec<(ClockOutcome, ClockResult, Option<u32>)> = vec![
            (Ok(Some(Ok(()))), ClockResult::Ok, None),
            (
                Ok(Some(Err(ItemError::Unsupported("Q".into())))),
                ClockResult::Unsupported,
                None,
            ),
            (
                Ok(Some(Err(ItemError::Rejected("clock_set".into())))),
                ClockResult::Rejected,
                None,
            ),
            (
                Ok(Some(Err(ItemError::Device(DeviceError::new(
                    0xc059, "end",
                ))))),
                ClockResult::Device,
                Some(0xc059),
            ),
            (
                Ok(Some(Err(ItemError::VerificationFailed("SM210".into())))),
                ClockResult::VerificationFailed,
                None,
            ),
            (Ok(None), ClockResult::NotOffered, None),
            (
                Err(BrokerError::Disconnected { connection_id: 1 }),
                ClockResult::Unavailable,
                None,
            ),
            (
                Err(BrokerError::UnsupportedProtocol {
                    connection_id: 1,
                    protocol: "x".into(),
                }),
                ClockResult::Unavailable,
                None,
            ),
            (
                Err(BrokerError::InvalidSettings {
                    connection_id: 1,
                    reason: "x".into(),
                }),
                ClockResult::Unavailable,
                None,
            ),
            (
                Err(BrokerError::ConnectionFailed {
                    connection_id: 1,
                    reason: "timeout".into(),
                }),
                ClockResult::Unknown,
                None,
            ),
            (
                Err(BrokerError::TaskGone { connection_id: 1 }),
                ClockResult::Unknown,
                None,
            ),
        ];
        for (outcome, result, code) in cases {
            let (got, got_code, message) = classify(&outcome);
            assert_eq!((got, got_code), (result, code), "{outcome:?}");
            assert!(!message.is_empty());
        }
        assert_eq!(ClockResult::Ok.audit_result(), "ok");
        assert_eq!(ClockResult::Busy.audit_result(), "failed");
        assert_eq!(
            serde_json::to_value(ClockResult::VerificationFailed).unwrap(),
            json!("verificationFailed")
        );
        assert_eq!(
            serde_json::to_value(ClockResult::NotOffered).unwrap(),
            json!("notOffered")
        );
    }

    async fn pool() -> (SqlitePool, crate::test_support::TempDir) {
        let dir = crate::test_support::TempDir::new("clock-sync");
        let pool =
            crate::test_support::init_db_from_template(&dir.path().join("hub.sqlite3")).await;
        (pool, dir)
    }

    async fn audit_rows(pool: &SqlitePool) -> Vec<(Option<String>, String, String, String, Value)> {
        sqlx::query(
            "SELECT actor_username, action, origin, result, detail FROM audit_log \
             WHERE resource = 'plc_connection_clock_sync' ORDER BY id",
        )
        .fetch_all(pool)
        .await
        .unwrap()
        .iter()
        .map(|row| {
            (
                row.get("actor_username"),
                row.get("action"),
                row.get("origin"),
                row.get("result"),
                serde_json::from_str(&row.get::<String, _>("detail")).unwrap(),
            )
        })
        .collect()
    }

    #[tokio::test]
    async fn settings_round_trip_and_the_table_checks_hold() {
        let (pool, _dir) = pool().await;
        let sync = ClockSync::new(pool.clone());
        assert_eq!(
            sync.settings(7).await.unwrap(),
            ClockSyncSettings::default()
        );
        let daily = ClockSyncSettings {
            enabled: true,
            schedule: ClockSchedule::Daily {
                hour: 23,
                minute: 59,
            },
        };
        sync.save_settings(7, &daily).await.unwrap();
        assert_eq!(sync.settings(7).await.unwrap(), daily);
        let hourly = ClockSyncSettings {
            enabled: false,
            schedule: ClockSchedule::Hourly { minute: 0 },
        };
        sync.save_settings(7, &hourly).await.unwrap();
        assert_eq!(sync.settings(7).await.unwrap(), hourly);
        assert!(sync.enabled_schedules().await.unwrap().is_empty());
        sync.save_settings(8, &daily).await.unwrap();
        assert_eq!(
            sync.enabled_schedules().await.unwrap(),
            vec![(8, daily.schedule)]
        );
        // The CHECKs refuse what the Rust side never writes.
        for sql in [
            "INSERT INTO hub_clock_sync (connection_id, enabled, schedule_kind, hour, minute) VALUES (20, 1, 'hourly', 3, 5)",
            "INSERT INTO hub_clock_sync (connection_id, enabled, schedule_kind, hour, minute) VALUES (21, 1, 'daily', NULL, 5)",
            "INSERT INTO hub_clock_sync (connection_id, enabled, schedule_kind, hour, minute) VALUES (22, 1, 'daily', 24, 5)",
            "INSERT INTO hub_clock_sync (connection_id, enabled, schedule_kind, hour, minute) VALUES (23, 1, 'hourly', NULL, 60)",
            "INSERT INTO hub_clock_sync (connection_id, enabled, schedule_kind, hour, minute) VALUES (24, 2, 'hourly', NULL, 5)",
            "INSERT INTO hub_clock_sync (connection_id, enabled, schedule_kind, hour, minute) VALUES (25, 1, 'cron', NULL, 5)",
        ] {
            assert!(sqlx::query(sql).execute(&pool).await.is_err(), "{sql}");
        }
        pool.close().await;
    }

    /// 送らない分岐（セッションが無い・切断中・実行中・出さない）が監査と直近の
    /// 結果に残り、印は外れる。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn runs_without_a_live_session_are_unavailable_and_audited() {
        let (pool, _dir) = pool().await;
        let audit = AuditLogService::new(crate::db::Db::Sqlite(pool.clone()));
        let sync = ClockSync::new(pool.clone());
        let sessions = HubSessions::new(
            banto_broker::BackoffConfig::default(),
            crate::test_support::test_drivers(),
        );
        let record = sync
            .run(
                &sessions,
                &audit,
                3,
                ClockTrigger::Manual,
                Some(ClockActor {
                    username: "admin",
                    role: "admin",
                }),
            )
            .await;
        assert_eq!(record.result, ClockResult::Unavailable);
        assert_eq!(record.time, None);
        assert_eq!(sync.last(3), Some(record.clone()));

        // 実行中の印があれば busy（送らない）。
        sync.running.lock().unwrap().insert(4);
        let busy = sync
            .run(&sessions, &audit, 4, ClockTrigger::Schedule, None)
            .await;
        assert_eq!(busy.result, ClockResult::Busy);
        assert!(
            sync.running.lock().unwrap().contains(&4),
            "busy は他人の印を外さない"
        );
        sync.running.lock().unwrap().remove(&4);

        let rows = audit_rows(&pool).await;
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert_eq!(rows[0].0.as_deref(), Some("admin"));
        assert_eq!(
            (rows[0].1.as_str(), rows[0].2.as_str(), rows[0].3.as_str()),
            ("clock_sync", "rest", "failed")
        );
        assert_eq!(rows[0].4["result"], "unavailable");
        assert_eq!(rows[0].4["trigger"], "manual");
        assert_eq!(rows[0].4["time"], Value::Null);
        assert_eq!(rows[1].0, None);
        assert_eq!(rows[1].2, "schedule");
        assert_eq!(rows[1].4["result"], "busy");
        assert!(sync.running.lock().unwrap().is_empty());
        sessions.shutdown().await;
        pool.close().await;
    }

    /// 1 つ目の回が終わらない間も、次の分の評価で別の接続の回が出る。止める合図の
    /// 後は残る回を待ってから終わる（abort しない）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_long_run_does_not_hold_back_the_next_minute() {
        let now = Arc::new(Mutex::new(at(2026, 11, 7, 9, 5, 0)));
        let release = Arc::new(tokio::sync::Notify::new());
        let (started_tx, mut started) = tokio::sync::mpsc::unbounded_channel();
        let finished = Arc::new(Mutex::new(Vec::new()));
        let source = now.clone();
        let (release_run, finished_run) = (release.clone(), finished.clone());
        let mut scheduler = spawn_loop(
            move || *source.lock().unwrap(),
            |minute: NaiveDateTime| async move {
                match minute.minute() {
                    5 => vec![1],
                    6 => vec![2],
                    _ => Vec::new(),
                }
            },
            move |id| {
                let (release, finished, started) = (
                    release_run.clone(),
                    finished_run.clone(),
                    started_tx.clone(),
                );
                async move {
                    started.send(id).unwrap();
                    if id == 1 {
                        release.notified().await;
                    }
                    finished.lock().unwrap().push(id);
                }
            },
            Duration::from_millis(5),
        );
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), started.recv())
                .await
                .unwrap(),
            Some(1)
        );
        *now.lock().unwrap() = at(2026, 11, 7, 9, 6, 0);
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), started.recv())
                .await
                .unwrap(),
            Some(2),
            "not held back"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(*finished.lock().unwrap(), vec![2]);
        // 合図の後も 1 つ目の回は捨てない（終わるまでタスクは終わらない）。
        assert!(!scheduler.stop_and_join(Duration::from_millis(100)).await);
        release.notify_one();
        assert!(scheduler.stop_and_join(Duration::from_secs(5)).await);
        assert_eq!(*finished.lock().unwrap(), vec![2, 1]);
    }

    /// 評価（`due`）を待つ間に止める合図が来たら、その評価の回は出さない。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_stop_during_the_evaluation_starts_no_run() {
        let release = Arc::new(tokio::sync::Notify::new());
        let (entered_tx, mut entered) = tokio::sync::mpsc::unbounded_channel();
        let runs = Arc::new(Mutex::new(0u32));
        let release_due = release.clone();
        let runs_seen = runs.clone();
        let mut scheduler = spawn_loop(
            || at(2026, 11, 7, 9, 5, 0),
            move |_| {
                let (release, entered) = (release_due.clone(), entered_tx.clone());
                async move {
                    entered.send(()).unwrap();
                    release.notified().await;
                    vec![1]
                }
            },
            move |_| {
                let runs = runs_seen.clone();
                async move {
                    *runs.lock().unwrap() += 1;
                }
            },
            Duration::from_millis(5),
        );
        tokio::time::timeout(Duration::from_secs(5), entered.recv())
            .await
            .unwrap()
            .unwrap();
        scheduler.signal_stop();
        release.notify_one();
        assert!(scheduler.stop_and_join(Duration::from_secs(5)).await);
        assert_eq!(*runs.lock().unwrap(), 0, "no run after the stop signal");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_scheduler_evaluates_each_minute_once_and_stops_on_the_signal() {
        let (pool, _dir) = pool().await;
        let audit = AuditLogService::new(crate::db::Db::Sqlite(pool.clone()));
        let dir = tempfile::tempdir().unwrap();
        let manager = Arc::new(CollectorManager::new(
            pool.clone(),
            dir.path().join("data"),
            Arc::new(banto_tstore::SystemClock),
            banto_collect::CollectorOptions::default(),
            Arc::new(HubSessions::new(
                banto_broker::BackoffConfig::default(),
                crate::test_support::test_drivers(),
            )),
            Arc::new(crate::broker_glue::BrokerSimRegistry::new()),
            Arc::new(crate::computed::ComputedEngine::new(Arc::new(
                crate::computed::ServerTagStore::new(),
            ))),
        ));
        // 接続 5 は適用済みのカタログに無い（収集を始めていない）ので出さない。
        manager
            .clock_sync()
            .save_settings(
                5,
                &ClockSyncSettings {
                    enabled: true,
                    schedule: ClockSchedule::Hourly { minute: 5 },
                },
            )
            .await
            .unwrap();
        let now = Arc::new(Mutex::new(at(2026, 11, 7, 9, 5, 0)));
        let source = now.clone();
        manager
            .clock_sync()
            .set_local_now(Arc::new(move || *source.lock().unwrap()));
        let mut scheduler =
            spawn_scheduler_with(manager.clone(), audit.clone(), Duration::from_millis(5));
        tokio::time::sleep(Duration::from_millis(50)).await;
        *now.lock().unwrap() = at(2026, 11, 7, 10, 5, 0);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(scheduler.stop_and_join(Duration::from_secs(5)).await);
        assert!(audit_rows(&pool).await.is_empty(), "nothing runnable");
        assert!(manager.clock_sync().last(5).is_none());
        manager.sessions().shutdown().await;
        pool.close().await;
    }
}
