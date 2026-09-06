//! 1 sink group の実行時状態と、行を作る 2 つのプロデューサ
//! （設計 §5.2 の `mode`: `interval` / `on_change`）。
//!
//! ## モードの意味
//!
//! - `interval`: `intervalMs` ごとに**全対象タグの最新値**を採り、
//!   1 タグ 1 行を積む。値が変わっていなくても積む（定周期ロガー）。
//!   初期値は SDK が Live 到達時に REST スナップショットで埋めるので、
//!   静止した環境でも 1 周期目から行ができる。
//! - `on_change`: 値または quality が**変わったタグだけ**を積む。
//!   `intervalMs` は最短発行間隔（スロットル）で、1 タグはこの間隔に
//!   1 行を超えて出ない。スロットル中に起きた変化は捨てず、次の評価
//!   （SDK の新しいフレーム、または `intervalMs` のタイマー）で最新値
//!   1 行として出る（[`OnChangeGate`]）。
//!
//! ## `storeBad`
//!
//! 既定（false）では quality が `good` 以外の行を積まない（設計 §5.2）。
//! true なら `bad`/`stale` もそのまま積む - 行の quality 列に wire の
//! 文字列がそのまま入るので、DB 側で切り分けられる。
//!
//! ## プロデューサは abort で止める
//!
//! 両プロデューサとも「値を読む → キューに積む」だけで、`await` を
//! 挟んだ状態遷移を持たない。キューの排他は `std::sync::Mutex` で
//! ロック中に `await` しないため、`JoinHandle::abort` で任意の時点に
//! 止めても壊れる中間状態が無い（設定変更・停止で実際にそうする）。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::{watch, Notify};

use crate::hub_api::SinkConfigGroup;
use crate::queue::{Row, RowQueue};
use crate::sql::Table;
use crate::status::GroupState;
use crate::values::ValueView;

/// テーブル検査の再試行間隔の既定（設計 §5.3「30 秒ごとに再検査」）。
/// テストは [`crate::run::SidecarOptions`] で短くできる。
pub const DEFAULT_TABLE_RECHECK: Duration = Duration::from_secs(30);

/// `hub_sink_groups.mode`（`banto_hub_core::sink::ALLOWED_SINK_MODES`）。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SinkMode {
    Interval,
    OnChange,
}

impl SinkMode {
    /// 未知の値は `None` - 呼び出し側はそのグループを `error` にする
    /// （新しい Hub が増やしたモードを黙って `interval` として動かすと、
    /// 意図しない量の行を書いてしまう）。
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "interval" => Some(Self::Interval),
            "on_change" => Some(Self::OnChange),
            _ => None,
        }
    }
}

/// 対象タグ 1 件（安定 ID の 3 つ目と、採取時点の外部名）。
#[derive(Clone, Debug, PartialEq)]
pub struct TagRef {
    pub tag_id: i64,
    pub external_name: String,
}

/// グループの可変状態（状態 push とテーブル検査の管理）。
#[derive(Debug)]
struct GroupStatusInner {
    state: GroupState,
    last_flush_at: Option<i64>,
    last_error: Option<String>,
    /// テーブル検査に通っているか。INSERT が失敗したら false に戻し、
    /// 次の flush で必ず検査し直す（設計 §5.3「起動時と再接続後」）。
    table_ready: bool,
    /// 次にテーブル検査を試してよい時刻。
    next_check_at: Option<Instant>,
    /// 「テーブルが無い」の推奨 DDL 案内を 1 回だけ出すためのフラグ
    /// （設計 §5.5「warn は初回と状態が変わったときだけ」）。
    ddl_hint_logged: bool,
}

/// 1 sink group の実行時オブジェクト。プロデューサ（積む側）と
/// flusher（出す側）と状態 push（読む側）が `Arc` で共有する。
#[derive(Debug)]
pub struct GroupRuntime {
    pub id: i64,
    pub name: String,
    pub db_connection_id: i64,
    pub mode: SinkMode,
    pub interval: Duration,
    pub store_bad: bool,
    pub tags: Vec<TagRef>,
    pub table: Table,
    queue: Mutex<RowQueue>,
    status: Mutex<GroupStatusInner>,
}

/// 設定が壊れている（サイドカー側では直せない）グループの理由。
/// いずれも `error` 状態 + `lastError` として Hub へ push する。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GroupSetupError {
    UnknownMode(String),
    InvalidTableName(String),
    MissingConnection(i64),
    NoTags,
}

impl GroupSetupError {
    pub fn message(&self) -> String {
        match self {
            Self::UnknownMode(mode) => format!(
                "未知の mode です: {mode}（このサイドカーは interval / on_change のみ扱えます。Hub と sink のバージョンを揃えてください）"
            ),
            Self::InvalidTableName(name) => format!(
                "テーブル名 {name} が識別子として不正です（schema.table 形式、英数字と _ のみ）"
            ),
            Self::MissingConnection(id) => format!(
                "参照している DB 接続 (id={id}) が設定に含まれていません（接続が削除された、または postgres 以外になっています）"
            ),
            Self::NoTags => {
                "対象タグが 1 つもありません（タグが削除された可能性があります）".to_string()
            }
        }
    }
}

impl GroupRuntime {
    /// Hub の設定 1 件から実行時オブジェクトを作る。`dropped` は同じ id の
    /// 旧世代から引き継ぐ累計値（[`RowQueue::with_dropped`]）。
    pub fn new(
        group: &SinkConfigGroup,
        queue_max_rows: usize,
        carried_dropped: u64,
    ) -> Result<Self, GroupSetupError> {
        let mode = SinkMode::parse(&group.mode)
            .ok_or_else(|| GroupSetupError::UnknownMode(group.mode.clone()))?;
        let table = Table::new(&group.table_name)
            .ok_or_else(|| GroupSetupError::InvalidTableName(group.table_name.clone()))?;
        if group.tags.is_empty() {
            return Err(GroupSetupError::NoTags);
        }
        let tags = group
            .tags
            .iter()
            .map(|tag| TagRef {
                tag_id: tag.tag_id,
                external_name: tag.external_name.clone(),
            })
            .collect();
        Ok(Self {
            id: group.id,
            name: group.name.clone(),
            db_connection_id: group.db_connection_id,
            mode,
            // `interval_ms` は Hub 側で 100〜3,600,000 に検証済み。
            // 0 以下は構造上来ないが、来ても 100ms で下支えする。
            interval: Duration::from_millis(group.interval_ms.max(100) as u64),
            store_bad: group.store_bad,
            tags,
            table,
            queue: Mutex::new(RowQueue::with_dropped(queue_max_rows, carried_dropped)),
            status: Mutex::new(GroupStatusInner {
                state: GroupState::Running,
                last_flush_at: None,
                last_error: None,
                table_ready: false,
                next_check_at: None,
                ddl_hint_logged: false,
            }),
        })
    }

    fn queue(&self) -> std::sync::MutexGuard<'_, RowQueue> {
        self.queue.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn status(&self) -> std::sync::MutexGuard<'_, GroupStatusInner> {
        self.status.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// 行を積む（プロデューサ専用）。
    pub fn push_rows(&self, rows: Vec<Row>) -> usize {
        let mut queue = self.queue();
        for row in rows {
            queue.push(row);
        }
        queue.len()
    }

    pub fn queued(&self) -> usize {
        self.queue().len()
    }

    pub fn dropped(&self) -> u64 {
        self.queue().dropped()
    }

    pub fn peek_batch(&self, limit: usize) -> Vec<Row> {
        self.queue().peek_batch(limit)
    }

    pub fn commit_batch(&self, count: usize) {
        self.queue().commit(count);
    }

    /// 停止時に残った行を捨てて `dropped` に計上する（設計 §5.6）。
    pub fn discard_queue(&self) -> usize {
        self.queue().discard_all()
    }

    // --- 状態遷移（すべて「変化したか」を返す - ログの抑制に使う） -------

    /// flush 成功。
    pub fn mark_flushed(&self, now_ms: i64) -> bool {
        let mut status = self.status();
        let changed = status.state != GroupState::Running;
        status.state = GroupState::Running;
        status.last_flush_at = Some(now_ms);
        status.last_error = None;
        changed
    }

    /// 接続レベルの失敗（バックオフ中）。
    pub fn mark_backoff(&self, error: String) -> bool {
        let mut status = self.status();
        let changed = status.state != GroupState::Backoff;
        status.state = GroupState::Backoff;
        status.last_error = Some(error);
        status.table_ready = false;
        changed
    }

    /// グループレベルの失敗（テーブル検査・SQL エラー・設定不正）。
    pub fn mark_error(&self, error: String, recheck_after: Duration) -> bool {
        let mut status = self.status();
        let changed = status.state != GroupState::Error;
        status.state = GroupState::Error;
        status.last_error = Some(error);
        status.table_ready = false;
        status.next_check_at = Some(Instant::now() + recheck_after);
        changed
    }

    /// テーブル検査に通った。
    pub fn mark_table_ready(&self) -> bool {
        let mut status = self.status();
        let changed = status.state != GroupState::Running;
        status.table_ready = true;
        status.state = GroupState::Running;
        status.last_error = None;
        status.next_check_at = None;
        changed
    }

    pub fn table_ready(&self) -> bool {
        self.status().table_ready
    }

    /// 今テーブル検査を試してよいか（`error` の再検査間隔）。
    pub fn may_check_table(&self, now: Instant) -> bool {
        match self.status().next_check_at {
            Some(at) => now >= at,
            None => true,
        }
    }

    /// 推奨 DDL の案内をまだ出していなければ true を返し、以後 false
    /// （1 回だけ `warn` する - 設計 §5.3・§5.5）。
    pub fn take_ddl_hint_slot(&self) -> bool {
        let mut status = self.status();
        if status.ddl_hint_logged {
            return false;
        }
        status.ddl_hint_logged = true;
        true
    }

    pub fn state(&self) -> GroupState {
        self.status().state
    }

    pub fn last_flush_at(&self) -> Option<i64> {
        self.status().last_flush_at
    }

    pub fn last_error(&self) -> Option<String> {
        self.status().last_error.clone()
    }
}

/// 実行中のグループ一覧。**flusher（接続ごと）・状態 push（全件）・
/// 設定の差分適用**の 3 者が共有する唯一の登録簿。flusher が毎周回
/// [`Self::for_connection`] で読み直すので、グループの追加・削除で
/// flusher を作り直す必要が無い（接続そのものが変わったときだけ作り直す
/// - [`crate::run`] の差分適用参照）。
#[derive(Debug, Default)]
pub struct GroupRegistry {
    groups: Mutex<HashMap<i64, Arc<GroupRuntime>>>,
}

impl GroupRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<i64, Arc<GroupRuntime>>> {
        self.groups.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn insert(&self, group: Arc<GroupRuntime>) {
        self.lock().insert(group.id, group);
    }

    pub fn remove(&self, id: i64) -> Option<Arc<GroupRuntime>> {
        self.lock().remove(&id)
    }

    pub fn get(&self, id: i64) -> Option<Arc<GroupRuntime>> {
        self.lock().get(&id).cloned()
    }

    /// id 昇順（flush の順序と状態 push の順序を安定させる）。
    pub fn all(&self) -> Vec<Arc<GroupRuntime>> {
        let mut groups: Vec<Arc<GroupRuntime>> = self.lock().values().cloned().collect();
        groups.sort_by_key(|group| group.id);
        groups
    }

    pub fn for_connection(&self, db_connection_id: i64) -> Vec<Arc<GroupRuntime>> {
        let mut groups: Vec<Arc<GroupRuntime>> = self
            .lock()
            .values()
            .filter(|group| group.db_connection_id == db_connection_id)
            .cloned()
            .collect();
        groups.sort_by_key(|group| group.id);
        groups
    }
}

// --- 行の生成（純粋関数・状態機械） -----------------------------------------

/// quality が保存対象か（`storeBad = false` なら Good のみ - 設計 §5.2）。
fn is_storable(quality: &str, store_bad: bool) -> bool {
    store_bad || quality == "good"
}

/// 値の `ptime` を行の `ts` にする。0 以下（未設定・異常）のときだけ
/// 採取時刻へフォールバックする（実装指示「ts は ptime、fallback: now」）。
fn row_ts(ptime_ms: i64, now_ms: i64) -> i64 {
    if ptime_ms > 0 {
        ptime_ms
    } else {
        now_ms
    }
}

/// `interval` モードの 1 周期分。購読が Live でなければ空
/// （[`ValueView::get`] が `None` を返す）。
pub fn sample_interval(
    view: &ValueView,
    tags: &[TagRef],
    store_bad: bool,
    now_ms: i64,
) -> Vec<Row> {
    let mut rows = Vec::with_capacity(tags.len());
    for tag in tags {
        let Some(sample) = view.get(&tag.external_name) else {
            continue;
        };
        if !is_storable(&sample.quality, store_bad) {
            continue;
        }
        rows.push(Row {
            ts_ms: row_ts(sample.ptime_ms, now_ms),
            tag_id: tag.tag_id,
            external_name: tag.external_name.clone(),
            value: sample.value,
            quality: sample.quality.clone(),
        });
    }
    rows
}

/// `on_change` モードの状態機械（変化検出 + 最短発行間隔）。
///
/// - 「変化」は `(value, quality)` の変化。`ptime` だけが進んだ場合
///   （再接続後の REST スナップショット等）は変化としない。
/// - スロットル中の変化は**捨てない**: 最後に発行した `(value, quality)`
///   を更新しないので、次の評価で最新値がそのまま出る。
#[derive(Debug, Default)]
pub struct OnChangeGate {
    /// tag_id → 最後に**発行した** (value のビット表現, quality)。
    last_emitted: HashMap<i64, (Option<u64>, String)>,
    /// tag_id → 最後に発行した時刻（epoch ミリ秒）。
    last_emitted_at: HashMap<i64, i64>,
}

impl OnChangeGate {
    pub fn new() -> Self {
        Self::default()
    }

    /// 発行してよければ true（そのとき内部状態を更新する）。
    pub fn admit(
        &mut self,
        tag_id: i64,
        value: Option<f64>,
        quality: &str,
        now_ms: i64,
        min_interval_ms: i64,
    ) -> bool {
        let key = (value.map(f64::to_bits), quality.to_string());
        if self.last_emitted.get(&tag_id) == Some(&key) {
            return false;
        }
        if let Some(previous) = self.last_emitted_at.get(&tag_id) {
            if now_ms.saturating_sub(*previous) < min_interval_ms {
                return false;
            }
        }
        self.last_emitted.insert(tag_id, key);
        self.last_emitted_at.insert(tag_id, now_ms);
        true
    }

    /// 設定変更でタグが減ったときに、消えたタグの記憶を落とす。
    pub fn retain_tags(&mut self, tags: &[TagRef]) {
        let keep: std::collections::HashSet<i64> = tags.iter().map(|tag| tag.tag_id).collect();
        self.last_emitted.retain(|id, _| keep.contains(id));
        self.last_emitted_at.retain(|id, _| keep.contains(id));
    }
}

/// `on_change` の 1 評価分。
pub fn sample_on_change(
    gate: &mut OnChangeGate,
    view: &ValueView,
    tags: &[TagRef],
    store_bad: bool,
    now_ms: i64,
    min_interval_ms: i64,
) -> Vec<Row> {
    let mut rows = Vec::new();
    for tag in tags {
        let Some(sample) = view.get(&tag.external_name) else {
            continue;
        };
        if !is_storable(&sample.quality, store_bad) {
            continue;
        }
        if !gate.admit(
            tag.tag_id,
            sample.value,
            &sample.quality,
            now_ms,
            min_interval_ms,
        ) {
            continue;
        }
        rows.push(Row {
            ts_ms: row_ts(sample.ptime_ms, now_ms),
            tag_id: tag.tag_id,
            external_name: tag.external_name.clone(),
            value: sample.value,
            quality: sample.quality.clone(),
        });
    }
    rows
}

/// 現在時刻（epoch ミリ秒）。`banto_tstore::Clock` を持ち込まないための
/// 最小の代用（サイドカーは時計を注入するテストを持たない - 時間に
/// 依存する判定はすべて純粋関数側に切り出してある）。
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|dur| dur.as_millis() as i64)
        .unwrap_or(0)
}

// --- プロデューサ本体（タスク） ---------------------------------------------

/// `interval` モード: 周期ごとに全タグを採る。
pub async fn run_interval_producer(
    group: Arc<GroupRuntime>,
    rx: watch::Receiver<Arc<ValueView>>,
    wake: Arc<Notify>,
    batch_size: usize,
) {
    let mut ticker = tokio::time::interval(group.interval);
    // 遅れを取り戻すためにまとめて発火させない（周期ロガーとして、
    // 1 周期に 1 回だけ採る方が意味に合う）。
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        let view = rx.borrow().clone();
        let rows = sample_interval(&view, &group.tags, group.store_bad, now_ms());
        if rows.is_empty() {
            continue;
        }
        let queued = group.push_rows(rows);
        if queued >= batch_size {
            wake.notify_one();
        }
    }
}

/// `on_change` モード: SDK のフレームごと、および最短発行間隔ごとに
/// 評価する（スロットルで持ち越した変化を取りこぼさないため - この
/// モジュールの doc comment参照）。
pub async fn run_on_change_producer(
    group: Arc<GroupRuntime>,
    mut rx: watch::Receiver<Arc<ValueView>>,
    wake: Arc<Notify>,
    batch_size: usize,
) {
    let mut gate = OnChangeGate::new();
    let min_interval_ms = group.interval.as_millis() as i64;
    loop {
        let view = rx.borrow_and_update().clone();
        let rows = sample_on_change(
            &mut gate,
            &view,
            &group.tags,
            group.store_bad,
            now_ms(),
            min_interval_ms,
        );
        if !rows.is_empty() {
            let queued = group.push_rows(rows);
            if queued >= batch_size {
                wake.notify_one();
            }
        }
        tokio::select! {
            changed = rx.changed() => {
                if changed.is_err() {
                    return;
                }
            }
            _ = tokio::time::sleep(group.interval) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub_api::SinkConfigTag;
    use crate::values::ValueSample;

    fn tags() -> Vec<TagRef> {
        vec![
            TagRef {
                tag_id: 11,
                external_name: "line1.fast.temp01".to_string(),
            },
            TagRef {
                tag_id: 12,
                external_name: "line1.fast.temp02".to_string(),
            },
        ]
    }

    fn view(entries: &[(&str, Option<f64>, &str, i64)]) -> ValueView {
        ValueView {
            live: true,
            values: entries
                .iter()
                .map(|(name, value, quality, ptime)| {
                    (
                        (*name).to_string(),
                        ValueSample {
                            value: *value,
                            quality: (*quality).to_string(),
                            ptime_ms: *ptime,
                        },
                    )
                })
                .collect(),
        }
    }

    fn config_group(mode: &str, table: &str) -> SinkConfigGroup {
        SinkConfigGroup {
            id: 7,
            name: "line1-log".to_string(),
            db_connection_id: 3,
            mode: mode.to_string(),
            interval_ms: 1_000,
            table_name: table.to_string(),
            store_bad: false,
            tags: vec![SinkConfigTag {
                connection_id: 1,
                group_id: 2,
                tag_id: 11,
                external_name: "line1.fast.temp01".to_string(),
            }],
        }
    }

    // --- interval サンプラー ------------------------------------------------

    #[test]
    fn interval_sampling_takes_one_row_per_tag_from_the_latest_snapshot() {
        let view = view(&[
            ("line1.fast.temp01", Some(1.5), "good", 100),
            ("line1.fast.temp02", Some(2.5), "good", 200),
        ]);
        let rows = sample_interval(&view, &tags(), false, 9_999);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].tag_id, 11);
        assert_eq!(rows[0].value, Some(1.5));
        assert_eq!(rows[0].ts_ms, 100);
        assert_eq!(rows[0].external_name, "line1.fast.temp01");
        assert_eq!(rows[1].ts_ms, 200);
    }

    #[test]
    fn interval_sampling_skips_non_good_rows_unless_store_bad() {
        let view = view(&[
            ("line1.fast.temp01", Some(1.5), "bad", 100),
            ("line1.fast.temp02", Some(2.5), "stale", 200),
        ]);
        assert!(sample_interval(&view, &tags(), false, 0).is_empty());

        let rows = sample_interval(&view, &tags(), true, 0);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].quality, "bad");
        assert_eq!(rows[1].quality, "stale");
    }

    #[test]
    fn interval_sampling_produces_nothing_while_the_subscription_is_not_live() {
        let mut view = view(&[("line1.fast.temp01", Some(1.5), "good", 100)]);
        view.live = false;
        assert!(sample_interval(&view, &tags(), true, 0).is_empty());
    }

    #[test]
    fn a_tag_missing_from_the_snapshot_is_skipped_not_faked() {
        let view = view(&[("line1.fast.temp01", Some(1.5), "good", 100)]);
        let rows = sample_interval(&view, &tags(), false, 0);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].tag_id, 11);
    }

    #[test]
    fn ts_falls_back_to_now_when_the_value_has_no_ptime() {
        let view = view(&[("line1.fast.temp01", Some(1.5), "good", 0)]);
        let rows = sample_interval(&view, &tags(), false, 4_242);
        assert_eq!(rows[0].ts_ms, 4_242);
    }

    // --- on_change のスロットル ---------------------------------------------

    #[test]
    fn on_change_emits_the_first_observation_then_only_real_changes() {
        let mut gate = OnChangeGate::new();
        let tags = tags();
        let first = view(&[("line1.fast.temp01", Some(1.0), "good", 10)]);
        assert_eq!(
            sample_on_change(&mut gate, &first, &tags, false, 0, 1_000).len(),
            1
        );
        // 同じ値・同じ quality（ptime だけ進む）→ 発行しない。
        let same = view(&[("line1.fast.temp01", Some(1.0), "good", 20)]);
        assert!(sample_on_change(&mut gate, &same, &tags, false, 5_000, 1_000).is_empty());
        // quality だけの変化も「変化」。
        let quality_change = view(&[("line1.fast.temp01", Some(1.0), "stale", 30)]);
        assert_eq!(
            sample_on_change(&mut gate, &quality_change, &tags, true, 10_000, 1_000).len(),
            1
        );
    }

    #[test]
    fn on_change_throttles_to_at_most_one_row_per_interval_and_keeps_the_change_pending() {
        let mut gate = OnChangeGate::new();
        let tags = tags();
        let v1 = view(&[("line1.fast.temp01", Some(1.0), "good", 10)]);
        assert_eq!(
            sample_on_change(&mut gate, &v1, &tags, false, 1_000, 1_000).len(),
            1
        );
        // 100ms 後の変化はスロットルで出ない。
        let v2 = view(&[("line1.fast.temp01", Some(2.0), "good", 20)]);
        assert!(sample_on_change(&mut gate, &v2, &tags, false, 1_100, 1_000).is_empty());
        // さらに 900ms 後（前回発行から 1,000ms）に再評価すると、
        // **持ち越した最新値**が 1 行だけ出る。
        assert_eq!(
            sample_on_change(&mut gate, &v2, &tags, false, 2_000, 1_000)[0].value,
            Some(2.0)
        );
    }

    #[test]
    fn on_change_tracks_each_tag_independently() {
        let mut gate = OnChangeGate::new();
        let tags = tags();
        let both = view(&[
            ("line1.fast.temp01", Some(1.0), "good", 10),
            ("line1.fast.temp02", Some(2.0), "good", 10),
        ]);
        assert_eq!(
            sample_on_change(&mut gate, &both, &tags, false, 0, 1_000).len(),
            2
        );
        let one_changed = view(&[
            ("line1.fast.temp01", Some(1.0), "good", 10),
            ("line1.fast.temp02", Some(9.0), "good", 30),
        ]);
        let rows = sample_on_change(&mut gate, &one_changed, &tags, false, 5_000, 1_000);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].tag_id, 12);
    }

    #[test]
    fn on_change_treats_null_and_zero_as_different_values() {
        let mut gate = OnChangeGate::new();
        assert!(gate.admit(1, None, "good", 0, 0));
        assert!(gate.admit(1, Some(0.0), "good", 1, 0));
        assert!(!gate.admit(1, Some(0.0), "good", 2, 0));
        assert!(gate.admit(1, None, "good", 3, 0));
    }

    #[test]
    fn retain_tags_forgets_removed_tags() {
        let mut gate = OnChangeGate::new();
        assert!(gate.admit(11, Some(1.0), "good", 0, 0));
        assert!(gate.admit(12, Some(1.0), "good", 0, 0));
        gate.retain_tags(&tags()[..1]);
        // 11 は覚えたまま（同じ値なら出ない）、12 は忘れている（出る）。
        assert!(!gate.admit(11, Some(1.0), "good", 10, 0));
        assert!(gate.admit(12, Some(1.0), "good", 10, 0));
    }

    // --- GroupRuntime -------------------------------------------------------

    #[test]
    fn group_runtime_rejects_configurations_it_cannot_execute() {
        assert_eq!(
            GroupRuntime::new(&config_group("weekly", "t"), 10, 0).unwrap_err(),
            GroupSetupError::UnknownMode("weekly".to_string())
        );
        assert_eq!(
            GroupRuntime::new(&config_group("interval", "bad-table"), 10, 0).unwrap_err(),
            GroupSetupError::InvalidTableName("bad-table".to_string())
        );
        let mut no_tags = config_group("interval", "t");
        no_tags.tags.clear();
        assert_eq!(
            GroupRuntime::new(&no_tags, 10, 0).unwrap_err(),
            GroupSetupError::NoTags
        );
    }

    #[test]
    fn group_state_transitions_report_whether_they_changed() {
        let group =
            GroupRuntime::new(&config_group("interval", "public.t"), 10, 4).expect("valid config");
        assert_eq!(group.state(), GroupState::Running);
        assert_eq!(group.dropped(), 4, "dropped は旧世代から引き継ぐ");
        assert!(!group.table_ready());

        assert!(group.mark_error("boom".to_string(), Duration::from_secs(30)));
        assert_eq!(group.state(), GroupState::Error);
        assert_eq!(group.last_error().as_deref(), Some("boom"));
        // 同じ状態への遷移は「変化なし」（ログを毎回出さないため）。
        assert!(!group.mark_error("boom again".to_string(), Duration::from_secs(30)));

        assert!(group.mark_table_ready());
        assert!(group.table_ready());
        assert_eq!(group.last_error(), None);

        assert!(!group.mark_flushed(1_000));
        assert_eq!(group.last_flush_at(), Some(1_000));

        assert!(group.mark_backoff("db down".to_string()));
        assert!(!group.table_ready(), "バックオフ後は必ず再検査する");
    }

    #[test]
    fn the_ddl_hint_slot_is_handed_out_once() {
        let group = GroupRuntime::new(&config_group("interval", "t"), 10, 0).expect("valid config");
        assert!(group.take_ddl_hint_slot());
        assert!(!group.take_ddl_hint_slot());
    }

    #[test]
    fn table_recheck_is_gated_until_the_interval_elapses() {
        let group = GroupRuntime::new(&config_group("interval", "t"), 10, 0).expect("valid config");
        assert!(group.may_check_table(Instant::now()));
        group.mark_error("missing".to_string(), Duration::from_secs(30));
        assert!(!group.may_check_table(Instant::now()));
        assert!(group.may_check_table(Instant::now() + Duration::from_secs(31)));
    }
}
