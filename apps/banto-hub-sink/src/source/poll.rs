//! 1 つの source の DB への問い合わせ（docs/banto-hub-ingest-design.md §10.3 の「読み取り」
//! 「品質」）。Hub 内の DB Source（`banto_hub_core::db_source::task`）と**同じ形**で回す:
//!
//! 1. `PgPool`（最大 2）を張る（[`banto_db_source::pg_connect_options`]、`sslmode=prefer`）。
//! 2. 各クエリの SQL を 1 回だけ describe し（[`banto_db_source::describe_wrapper_sql`]・
//!    [`banto_db_source::describe_columns`]）、欲しい列を解決して（[`banto_db_source::
//!    resolve_columns`]）、`float8` に揃えるラッパー SQL を作る（未対応型・列なしは Bad）。
//! 3. クエリごとに最小デッドライン方式で回す。1 本のクエリには
//!    [`banto_db_source::query_timeout`]（`min(period × 0.8, 30s)`）。
//! 4. 結果の先頭行の列を `columns` で `key` に写し、**その周期の全キー**を 1 つの周期として
//!    [`PollEvent::Cycle`] で送り手（[`super::session`]）へ渡す。送り手が件数・バイト数で分け
//!    （[`super::split`]）、速さを抑えて（[`super::budget`]）Hub へ送る。
//! 5. 接続の失敗（[`banto_db_source::classify`] が `Connection`）で抜け、**全キー `bad` を
//!    渡してから**プールを閉じ、1 秒 → 30 秒のバックオフで 1. へ戻る。
//!
//! ## 品質の梯子（共有 crate の [`GroupLadder`]・[`Verdict`] をそのまま使う、§12 Q12）
//!
//! | 事象                       | 送る                                          |
//! | -------------------------- | --------------------------------------------- |
//! | 正常に列が取れた           | `q: good`・値                                  |
//! | 列が NULL・非有限値        | `q: bad`                                       |
//! | 列が無い / 型が未対応      | `q: bad`（毎周期）                             |
//! | 0 行                       | 全キー `q: bad`                                |
//! | クエリの失敗 1 回目        | 直前値で `q: stale`（直前値が無ければ `bad`）  |
//! | クエリの失敗 2 回目から    | `q: bad`                                       |
//! | DB の接続断                | 全キー `q: bad`（送ってからバックオフ）        |
//!
//! 直前値（Stale で送る値）は列ごとに持ち、Bad を送ったら捨てる - Hub 内の Source が
//! Bad を書くと `ServerTagStore` の値が消え、次の失敗の Stale が直前値なしで Bad に
//! なるのと同じ（[`Verdict::settle`]）。失敗の粒度（接続か文か）も共有 crate の
//! [`classify`]（Hub と同じ分類）。§10.4 の一致はこの共有で構造的に保つ。
//!
//! 同じ列を複数のキーへ送ってよい（`settings.bridge` の検証は列の重複を許す）。ラッパー
//! SQL には**列を 1 回だけ**射影して、キーへは添字で写す（PostgreSQL の射影は 1,664 列
//! まで。1 周期に 1,000 件を超えるクエリ（§10.4 の 9）でも 1 本の SQL で読めるように）。
//!
//! ## 止め方（implementation-checklist §5「DB を触る背景タスクを `abort()` で止めない」）
//!
//! 止めるのは合図（`watch`）で、**眠り（周期の待ち・バックオフ）の間だけ**合図で抜ける。
//! 接続・describe・クエリの途中では止めず、それぞれの上限（接続 5 秒、describe 30 秒、
//! クエリ [`query_timeout`]）で終わるのを待ってから抜け、プールを閉じる。止める側
//! （[`super::session`]）は `JoinHandle` を手放さずに終わりを待つ。
//!
//! ## 資格情報
//!
//! エラーの文言は [`banto_db_source::sanitize_postgres_error`]（[`classify`] の中）を通した
//! もので接続文字列・パスワードを含まない。さらに [`crate::status::Redactor`]（パスワードと
//! API キーを登録済み）を通してからログと `status` に出す（多層防御）。

use std::time::Duration;

use banto_db_source::{
    cell_verdict, classify, describe_columns, describe_retry_interval, describe_wrapper_sql,
    pg_connect_options, query_timeout, resolve_columns, ColumnBadReason, GroupLadder, PgFailure,
    ResolvedColumns, Verdict,
};
use sqlx::postgres::{PgPool, PgPoolOptions};
use sqlx::{Executor, Row, SqlSafeStr};
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;

use crate::flush::backoff_delay;
use crate::hub_api::{BridgeDbConfig, BridgeQueryConfig};
use crate::log::{log_err_line, log_line};
use crate::source::wire::ValueItem;
use crate::status::Redactor;

/// プールの最大接続数（Hub 内の Source と同じ 2、§10.3）。
const POOL_MAX_CONNECTIONS: u32 = 2;

/// プールから接続を取り出すまで（接続の確立を含む）の上限（Hub 内の Source と同じ 5 秒）。
const POOL_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);

/// describe の上限。Hub 内の Source は上限を掛けないが、ブリッジは止める合図を
/// describe の途中で受けない（このモジュールの doc「止め方」）ので、応答しない DB で
/// 止まり続けないよう掛ける。超えたら接続の失敗として張り直す。
const DESCRIBE_TIMEOUT: Duration = Duration::from_secs(30);

/// `pg_stat_activity` の `application_name` の接頭辞（DBA が Hub 内の Source
/// （`banto-hub-db-source {接続名}`）・Sink（`banto-hub-sink`）と見分けるため）。
pub const APPLICATION_NAME_PREFIX: &str = "banto-hub-sink-bridge";

/// 問い合わせのタスクから送り手（[`super::session`]）へ渡すもの。
#[derive(Debug, Clone, PartialEq)]
pub enum PollEvent {
    /// クエリ `query`（`queries` の添字）の 1 周期の全キー。
    Cycle { query: usize, items: Vec<ValueItem> },
    /// DB 側の状態の文言（秘密を伏せたもの）。`None` は「問題なし」。変わったときだけ。
    Status(Option<String>),
}

/// 1 クエリの実行形（設定から作る。列は重複を除いて 1 回ずつ）。
#[derive(Debug, Clone, PartialEq)]
pub struct QueryPlan {
    pub name: String,
    pub period_ms: u64,
    pub sql: String,
    /// 射影する列（重複なし、最初に出た順）。
    pub columns: Vec<String>,
    /// 送るキーと、その値を取る列（`columns` の添字）。設定の `columns` の順。
    pub keys: Vec<(String, usize)>,
}

impl QueryPlan {
    pub fn from_config(query: &BridgeQueryConfig) -> Self {
        let mut columns: Vec<String> = Vec::new();
        let mut keys = Vec::with_capacity(query.columns.len());
        for mapping in &query.columns {
            let index = match columns.iter().position(|c| *c == mapping.column) {
                Some(index) => index,
                None => {
                    columns.push(mapping.column.clone());
                    columns.len() - 1
                }
            };
            keys.push((mapping.key.clone(), index));
        }
        Self {
            name: query.name.clone(),
            // 設定は `collection_groups.period_ms` と同じ集合（100〜60000）に検証済み。
            period_ms: u64::try_from(query.period_ms).unwrap_or(1_000).max(100),
            sql: query.sql.clone(),
            columns,
            keys,
        }
    }

    /// 全キーを `bad` にした 1 周期。
    pub fn all_bad(&self) -> Vec<ValueItem> {
        self.keys
            .iter()
            .map(|(key, _)| ValueItem::bad(key))
            .collect()
    }
}

/// 1 クエリの梯子の状態（純粋。DB を触らない - 表のテストに掛ける）。
#[derive(Debug, Clone)]
pub struct QueryState {
    pub plan: QueryPlan,
    resolved: ResolvedColumns,
    ladder: GroupLadder,
    /// 列ごとの直前値（Stale で送る値）。Bad を送ったら `None`。
    previous: Vec<Option<f64>>,
}

impl QueryState {
    /// describe の結果から作る（列の解決は共有 crate の [`resolve_columns`]）。
    pub fn resolved(plan: QueryPlan, described: &[(String, String)]) -> Self {
        let wanted: Vec<&str> = plan.columns.iter().map(String::as_str).collect();
        let resolved = resolve_columns(&plan.sql, &wanted, described);
        let previous = vec![None; plan.columns.len()];
        Self {
            plan,
            resolved,
            ladder: GroupLadder::new(),
            previous,
        }
    }

    /// describe が文の失敗で落ちた（構文エラー・存在しない表・権限）。SQL を投げずに
    /// 全キー Bad を送り続け、[`describe_retry_interval`] ごとに describe し直す。
    pub fn unprepared(plan: QueryPlan, message: &str) -> Self {
        let bad_columns = (0..plan.columns.len())
            .map(|i| {
                (
                    i,
                    ColumnBadReason::StatementNotPrepared(message.to_string()),
                )
            })
            .collect();
        let previous = vec![None; plan.columns.len()];
        Self {
            plan,
            resolved: ResolvedColumns {
                wrapper_sql: None,
                value_columns: Vec::new(),
                bad_columns,
            },
            ladder: GroupLadder::new(),
            previous,
        }
    }

    /// 実行する SQL（値の取れる列が無ければ `None`）。
    pub fn wrapper_sql(&self) -> Option<&str> {
        self.resolved.wrapper_sql.as_deref()
    }

    /// 値の取れない列（列名と理由）。
    pub fn bad_columns(&self) -> impl Iterator<Item = (&str, &ColumnBadReason)> {
        self.resolved
            .bad_columns
            .iter()
            .map(|(i, reason)| (self.plan.columns[*i].as_str(), reason))
    }

    /// describe し直すべきか（SQL を投げていない、または Bad の梯子に乗っている）。
    pub fn needs_redescribe(&self) -> bool {
        self.resolved.wrapper_sql.is_none() || self.ladder.is_degraded()
    }

    /// describe し直した結果を採るか（Hub 内の Source の `should_adopt_redescribed` と同じ:
    /// SQL を投げていなかったなら必ず、そうでなければ列・型が変わったときだけ）。採ったら
    /// 梯子を最初に戻す。
    pub fn adopt_redescribed(&mut self, described: &[(String, String)]) -> bool {
        let wanted: Vec<&str> = self.plan.columns.iter().map(String::as_str).collect();
        let candidate = resolve_columns(&self.plan.sql, &wanted, described);
        if self.resolved.wrapper_sql.is_some() && candidate == self.resolved {
            return false;
        }
        self.resolved = candidate;
        self.ladder.reset();
        true
    }

    /// クエリが 1 行以上を返した。`cells` は先頭行の値（ラッパー SQL の射影順 = 値の
    /// 取れる列の順）。
    pub fn on_row(&mut self, cells: &[Option<f64>]) -> Vec<ValueItem> {
        self.ladder.on_success();
        let mut verdicts = vec![Verdict::Bad; self.plan.columns.len()];
        for (position, column) in self.resolved.value_columns.iter().enumerate() {
            verdicts[*column] = cell_verdict(cells.get(position).copied().flatten());
        }
        self.emit(&verdicts)
    }

    /// クエリは成功したが 0 行だった（梯子に乗せず全キー Bad）。
    pub fn on_zero_rows(&mut self) -> Vec<ValueItem> {
        let verdict = self.ladder.on_zero_rows();
        self.emit(&vec![verdict; self.plan.columns.len()])
    }

    /// クエリが文の失敗・タイムアウトで落ちた（1 回目は直前値で Stale、2 回目から Bad）。
    /// 2 つ目は初回の失敗か（ログの抑制）。
    pub fn on_query_failure(&mut self) -> (Vec<ValueItem>, bool) {
        let failure = self.ladder.on_query_failure();
        (
            self.emit(&vec![failure.verdict; self.plan.columns.len()]),
            failure.first,
        )
    }

    /// SQL を投げない（値の取れる列が無い・describe できない）周期: 全キー Bad。
    pub fn without_query(&mut self) -> Vec<ValueItem> {
        self.emit(&vec![Verdict::Bad; self.plan.columns.len()])
    }

    /// 列ごとの判断をキーへ写す（直前値の有無で Stale を確定し、直前値を更新する）。
    fn emit(&mut self, verdicts: &[Verdict]) -> Vec<ValueItem> {
        let settled: Vec<Verdict> = verdicts
            .iter()
            .enumerate()
            .map(|(column, verdict)| {
                let settled = verdict.settle(self.previous[column].is_some());
                match settled {
                    Verdict::Good(value) => self.previous[column] = Some(value),
                    Verdict::Bad => self.previous[column] = None,
                    Verdict::Stale => {}
                }
                settled
            })
            .collect();
        self.plan
            .keys
            .iter()
            .map(
                |(key, column)| match (settled[*column], self.previous[*column]) {
                    (Verdict::Good(value), _) => ValueItem::good(key, value),
                    (Verdict::Stale, Some(previous)) => ValueItem::stale(key, previous),
                    // `settle` が直前値の無い Stale を Bad にするので `(Stale, None)` は来ない。
                    (Verdict::Stale, None) | (Verdict::Bad, _) => ValueItem::bad(key),
                },
            )
            .collect()
    }
}

/// 問い合わせのタスクの入力。
#[derive(Debug, Clone)]
pub struct PollerSpec {
    /// ログと `application_name` 用（Hub の `push` 接続の名前）。
    pub source_name: String,
    pub db: BridgeDbConfig,
    pub queries: Vec<QueryPlan>,
}

impl PollerSpec {
    pub fn new(source_name: &str, db: &BridgeDbConfig, queries: &[BridgeQueryConfig]) -> Self {
        Self {
            source_name: source_name.to_string(),
            db: db.clone(),
            queries: queries.iter().map(QueryPlan::from_config).collect(),
        }
    }

    fn application_name(&self) -> String {
        format!("{APPLICATION_NAME_PREFIX} {}", self.source_name)
    }
}

/// 状態の文言をまとめる（接続の失敗があればそれだけ、無ければクエリごとの問題を `; ` で）。
#[derive(Debug, Default)]
struct StatusText {
    connection: Option<String>,
    queries: Vec<Option<String>>,
    last_sent: Option<Option<String>>,
}

impl StatusText {
    fn new(query_count: usize) -> Self {
        Self {
            connection: None,
            queries: vec![None; query_count],
            last_sent: None,
        }
    }

    fn compose(&self) -> Option<String> {
        if let Some(connection) = &self.connection {
            return Some(connection.clone());
        }
        let parts: Vec<&str> = self.queries.iter().flatten().map(String::as_str).collect();
        (!parts.is_empty()).then(|| parts.join("; "))
    }

    /// 変わっていれば送る文言を返す。
    fn take_change(&mut self, redactor: &Redactor) -> Option<Option<String>> {
        let now = self.compose().map(|text| redactor.apply(&text));
        if self.last_sent.as_ref() == Some(&now) {
            return None;
        }
        self.last_sent = Some(now.clone());
        Some(now)
    }
}

/// 送り手への出口。受け手が居なくなった（送り手が止まった）ら `false`。
struct Events<'a> {
    tx: &'a mpsc::Sender<PollEvent>,
    status: StatusText,
    redactor: &'a Redactor,
}

impl Events<'_> {
    async fn cycle(&self, query: usize, items: Vec<ValueItem>) -> bool {
        if items.is_empty() {
            return true;
        }
        self.tx
            .send(PollEvent::Cycle { query, items })
            .await
            .is_ok()
    }

    async fn flush_status(&mut self) -> bool {
        match self.status.take_change(self.redactor) {
            Some(text) => self.tx.send(PollEvent::Status(text)).await.is_ok(),
            None => true,
        }
    }

    async fn all_bad(&self, queries: &[QueryPlan]) -> bool {
        for (index, query) in queries.iter().enumerate() {
            if !self.cycle(index, query.all_bad()).await {
                return false;
            }
        }
        true
    }
}

/// 合図が来るまで `duration` 眠る。合図が来た（か送り手が居なくなった）ら `true`。
async fn sleep_or_stop(duration: Duration, stop: &mut watch::Receiver<bool>) -> bool {
    if *stop.borrow() {
        return true;
    }
    tokio::select! {
        _ = tokio::time::sleep(duration) => *stop.borrow(),
        changed = stop.changed() => changed.is_err() || *stop.borrow(),
    }
}

/// 問い合わせのタスクの本体。止める合図（`stop` が `true`・送り手が居なくなった）で
/// 戻る。戻る前にプールを閉じる。
pub async fn run_poller(
    spec: PollerSpec,
    tx: mpsc::Sender<PollEvent>,
    mut stop: watch::Receiver<bool>,
    redactor: Redactor,
) {
    let mut events = Events {
        tx: &tx,
        status: StatusText::new(spec.queries.len()),
        redactor: &redactor,
    };
    let mut attempt: u32 = 0;
    let mut last_delay: Option<Duration> = None;
    loop {
        if *stop.borrow() {
            return;
        }
        let failure = match connect(&spec).await {
            Ok(pool) => {
                if attempt > 0 {
                    log_line(&format!(
                        "banto-hub-sink: ブリッジ {}: DB に再接続しました",
                        spec.source_name
                    ));
                }
                attempt = 0;
                last_delay = None;
                events.status.connection = None;
                let outcome = poll_until_failure(&pool, &spec, &mut events, &mut stop).await;
                pool.close().await;
                match outcome {
                    PollOutcome::Stopped => return,
                    PollOutcome::ConnectionLost(message) => message,
                }
            }
            Err(failure) => failure.message().to_string(),
        };
        // §10.3「DB の接続断: 全キー bad を送ってからバックオフ」。
        if !events.all_bad(&spec.queries).await {
            return;
        }
        attempt = attempt.saturating_add(1);
        let message = redactor.apply(&failure);
        events.status.connection = Some(format!("DB に接続できません（{message}）"));
        if !events.flush_status().await {
            return;
        }
        let delay = backoff_delay(attempt);
        if last_delay != Some(delay) {
            log_err_line(&format!(
                "banto-hub-sink: warn: ブリッジ {}: DB に接続できません（{} 秒後に再試行します）: {message}",
                spec.source_name,
                delay.as_secs_f64()
            ));
            last_delay = Some(delay);
        }
        if sleep_or_stop(delay, &mut stop).await {
            return;
        }
    }
}

enum PollOutcome {
    Stopped,
    ConnectionLost(String),
}

async fn connect(spec: &PollerSpec) -> Result<PgPool, PgFailure> {
    let port = u16::try_from(spec.db.port)
        .ok()
        .filter(|port| *port != 0)
        .ok_or_else(|| {
            PgFailure::Connection(format!(
                "ポート番号 {} は 1〜65535 の範囲外です",
                spec.db.port
            ))
        })?;
    let options = pg_connect_options(
        &spec.db.host,
        port,
        spec.db.database.as_deref().unwrap_or(""),
        spec.db.username.as_deref().unwrap_or(""),
        spec.db.password.as_deref().unwrap_or(""),
        &spec.application_name(),
    );
    PgPoolOptions::new()
        .max_connections(POOL_MAX_CONNECTIONS)
        .acquire_timeout(POOL_ACQUIRE_TIMEOUT)
        .connect_with(options)
        .await
        .map_err(|err| classify(&err))
}

async fn describe(pool: &PgPool, sql: &str) -> Result<Vec<(String, String)>, PgFailure> {
    // AssertSqlSafe: 文の本体は Hub の管理者が登録した SQL（`banto_tags` が「先頭
    // SELECT/WITH・単文」まで検証済み）で、外から来た値を連結しているわけではない。
    // 本当の防御は DB 側の読み取り専用のユーザー（external-db §4.2）。Hub 内の Source と同じ。
    let statement = sqlx::AssertSqlSafe(describe_wrapper_sql(sql)).into_sql_str();
    match tokio::time::timeout(DESCRIBE_TIMEOUT, pool.describe(statement)).await {
        Err(_) => Err(PgFailure::Connection(format!(
            "SQL の解析が {} 秒以内に終わりませんでした",
            DESCRIBE_TIMEOUT.as_secs()
        ))),
        Ok(Ok(described)) => Ok(describe_columns(&described)),
        Ok(Err(err)) => Err(classify(&err)),
    }
}

/// 1 クエリの走行時状態。
struct QueryRuntime {
    state: QueryState,
    next_due: Instant,
    next_describe_retry: Option<Instant>,
    warned_multi_row: bool,
    warned_failure: bool,
}

async fn poll_until_failure(
    pool: &PgPool,
    spec: &PollerSpec,
    events: &mut Events<'_>,
    stop: &mut watch::Receiver<bool>,
) -> PollOutcome {
    let now = Instant::now();
    let mut queries = Vec::with_capacity(spec.queries.len());
    for (index, plan) in spec.queries.iter().enumerate() {
        let (state, next_describe_retry) = match describe(pool, &plan.sql).await {
            Ok(described) => {
                events.status.queries[index] = None;
                (QueryState::resolved(plan.clone(), &described), None)
            }
            Err(PgFailure::Connection(message)) => return PollOutcome::ConnectionLost(message),
            Err(PgFailure::Statement(message)) => {
                let message = events.redactor.apply(&message);
                log_err_line(&format!(
                    "banto-hub-sink: warn: ブリッジ {}.{}: SQL を解析できません: {message}",
                    spec.source_name, plan.name
                ));
                events.status.queries[index] =
                    Some(format!("{}: SQL を実行できません（{message}）", plan.name));
                (
                    QueryState::unprepared(plan.clone(), &message),
                    Some(now + describe_retry_interval(plan.period_ms)),
                )
            }
        };
        for (column, reason) in state.bad_columns() {
            if !matches!(reason, ColumnBadReason::StatementNotPrepared(_)) {
                log_err_line(&format!(
                    "banto-hub-sink: warn: ブリッジ {}.{}: 列 {column} は値を取得できません: {}",
                    spec.source_name,
                    plan.name,
                    reason.message()
                ));
            }
        }
        queries.push(QueryRuntime {
            state,
            // §10.3「describe からやり直し、すぐ最初の問い合わせ」。
            next_due: now,
            next_describe_retry,
            warned_multi_row: false,
            warned_failure: false,
        });
    }
    if !events.flush_status().await {
        return PollOutcome::Stopped;
    }

    loop {
        let Some(index) = queries
            .iter()
            .enumerate()
            .min_by_key(|(_, q)| q.next_due)
            .map(|(i, _)| i)
        else {
            // クエリの無い source は起動しない（[`super::plan`]）ので到達しない。止める合図を待つ。
            while !sleep_or_stop(Duration::from_secs(3600), stop).await {}
            return PollOutcome::Stopped;
        };
        let due = queries[index].next_due;
        let now = Instant::now();
        if due > now && sleep_or_stop(due - now, stop).await {
            return PollOutcome::Stopped;
        }
        if *stop.borrow() {
            return PollOutcome::Stopped;
        }

        let query = &mut queries[index];
        match run_tick(pool, spec, index, query, events).await {
            TickOutcome::Continue => {}
            TickOutcome::ConnectionLost(message) => return PollOutcome::ConnectionLost(message),
            TickOutcome::Stopped => return PollOutcome::Stopped,
        }
        if !events.flush_status().await {
            return PollOutcome::Stopped;
        }

        let period = Duration::from_millis(query.state.plan.period_ms);
        let mut next = query.next_due + period;
        let now = Instant::now();
        if next <= now {
            // 周期より長引いたら、遅れを取り戻そうと連続で投げない（Hub 内の Source と同じ）。
            next = now + period;
        }
        query.next_due = next;
    }
}

enum TickOutcome {
    Continue,
    ConnectionLost(String),
    Stopped,
}

async fn run_tick(
    pool: &PgPool,
    spec: &PollerSpec,
    index: usize,
    query: &mut QueryRuntime,
    events: &mut Events<'_>,
) -> TickOutcome {
    let name = query.state.plan.name.clone();
    // describe し直す（Hub 内の Source と同じ: SQL を投げていない・Bad の梯子に乗った
    // クエリを、`max(period, 5s)` ごとに）。
    if query.state.needs_redescribe() {
        let now = Instant::now();
        if query.next_describe_retry.is_none_or(|due| now >= due) {
            query.next_describe_retry =
                Some(now + describe_retry_interval(query.state.plan.period_ms));
            match describe(pool, &query.state.plan.sql).await {
                Err(PgFailure::Connection(message)) => return TickOutcome::ConnectionLost(message),
                Err(PgFailure::Statement(message)) => {
                    let message = events.redactor.apply(&message);
                    if query.state.wrapper_sql().is_none() {
                        events.status.queries[index] =
                            Some(format!("{name}: SQL を実行できません（{message}）"));
                    }
                }
                Ok(described) => {
                    if query.state.adopt_redescribed(&described) {
                        log_line(&format!(
                            "banto-hub-sink: ブリッジ {}.{name}: SQL を再解析しました",
                            spec.source_name
                        ));
                        events.status.queries[index] = None;
                    }
                }
            }
        }
    }

    let Some(sql) = query.state.wrapper_sql().map(str::to_owned) else {
        let items = query.state.without_query();
        return cycle_outcome(events.cycle(index, items).await);
    };

    let timeout = query_timeout(query.state.plan.period_ms);
    // AssertSqlSafe: `describe` と同じ理由（列名は共有 crate の `quote_ident` を必ず通る）。
    let fetch = sqlx::query(sqlx::AssertSqlSafe(sql)).fetch_all(pool);
    let outcome = match tokio::time::timeout(timeout, fetch).await {
        Err(_) => Err(PgFailure::Statement(format!(
            "クエリがタイムアウトしました({:.1}秒)",
            timeout.as_secs_f64()
        ))),
        Ok(Ok(rows)) => Ok(rows),
        Ok(Err(err)) => Err(classify(&err)),
    };
    let rows = match outcome {
        Ok(rows) => rows,
        Err(PgFailure::Connection(message)) => return TickOutcome::ConnectionLost(message),
        Err(PgFailure::Statement(message)) => {
            let message = events.redactor.apply(&message);
            let (items, first) = query.state.on_query_failure();
            if first && !query.warned_failure {
                query.warned_failure = true;
                log_err_line(&format!(
                    "banto-hub-sink: warn: ブリッジ {}.{name}: クエリが失敗しました: {message}",
                    spec.source_name
                ));
            }
            events.status.queries[index] =
                Some(format!("{name}: クエリが失敗しました（{message}）"));
            return cycle_outcome(events.cycle(index, items).await);
        }
    };
    query.warned_failure = false;

    if rows.is_empty() {
        let items = query.state.on_zero_rows();
        events.status.queries[index] = Some(format!("{name}: 結果が 0 行です"));
        return cycle_outcome(events.cycle(index, items).await);
    }
    if rows.len() >= 2 && !query.warned_multi_row {
        query.warned_multi_row = true;
        log_err_line(&format!(
            "banto-hub-sink: warn: ブリッジ {}.{name}: SQL が複数行を返しています - 先頭行を採用します",
            spec.source_name
        ));
    }
    let row = &rows[0];
    let cells: Vec<Option<f64>> = (0..query.state.resolved.value_columns.len())
        .map(|position| row.try_get::<Option<f64>, _>(position).unwrap_or(None))
        .collect();
    let items = query.state.on_row(&cells);
    events.status.queries[index] = query
        .state
        .bad_columns()
        .next()
        .map(|(column, reason)| format!("{name}: 列 {column}: {}", reason.message()));
    cycle_outcome(events.cycle(index, items).await)
}

fn cycle_outcome(delivered: bool) -> TickOutcome {
    if delivered {
        TickOutcome::Continue
    } else {
        TickOutcome::Stopped
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub_api::BridgeColumnConfig;
    use crate::source::wire::WireQuality;

    fn plan(columns: &[(&str, &str)]) -> QueryPlan {
        QueryPlan::from_config(&BridgeQueryConfig {
            name: "line1".to_string(),
            period_ms: 1_000,
            sql: "SELECT a, b FROM t".to_string(),
            columns: columns
                .iter()
                .map(|(column, key)| BridgeColumnConfig {
                    column: column.to_string(),
                    key: key.to_string(),
                })
                .collect(),
        })
    }

    fn described(columns: &[(&str, &str)]) -> Vec<(String, String)> {
        columns
            .iter()
            .map(|(n, t)| (n.to_string(), t.to_string()))
            .collect()
    }

    fn qualities(items: &[ValueItem]) -> Vec<(String, Option<f64>, WireQuality)> {
        items.iter().map(|i| (i.key.clone(), i.v, i.q)).collect()
    }

    /// 同じ列を複数のキーへ: 射影は 1 回、キーは設定の順。
    #[test]
    fn duplicate_columns_are_projected_once() {
        let plan = plan(&[("a", "k1"), ("b", "k2"), ("a", "k3")]);
        assert_eq!(plan.columns, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(
            plan.keys,
            vec![
                ("k1".to_string(), 0),
                ("k2".to_string(), 1),
                ("k3".to_string(), 0)
            ]
        );
        let mut state = QueryState::resolved(plan, &described(&[("a", "INT4"), ("b", "FLOAT8")]));
        let sql = state.wrapper_sql().unwrap().to_string();
        assert_eq!(sql.matches("AS c").count(), 2, "{sql}");
        let items = state.on_row(&[Some(1.0), Some(2.5)]);
        assert_eq!(
            qualities(&items),
            vec![
                ("k1".into(), Some(1.0), WireQuality::Good),
                ("k2".into(), Some(2.5), WireQuality::Good),
                ("k3".into(), Some(1.0), WireQuality::Good),
            ]
        );
    }

    /// §11 の R2a-3b「梯子の写し」: Hub 内の Source の表（`db_source/task.rs`）と同じ並び。
    #[test]
    fn the_ladder_is_mapped_like_the_hub_db_source() {
        let mut state = QueryState::resolved(
            plan(&[("a", "ka"), ("b", "kb"), ("note", "kn"), ("gone", "kg")]),
            &described(&[("a", "INT4"), ("b", "BOOL"), ("note", "TEXT")]),
        );
        // 成功: 値は Good、NULL は Bad、未対応型・列なしは毎周期 Bad。
        assert_eq!(
            qualities(&state.on_row(&[Some(12.0), None])),
            vec![
                ("ka".into(), Some(12.0), WireQuality::Good),
                ("kb".into(), None, WireQuality::Bad),
                ("kn".into(), None, WireQuality::Bad),
                ("kg".into(), None, WireQuality::Bad),
            ]
        );
        assert_eq!(
            qualities(&state.on_row(&[Some(13.0), Some(1.0)]))[..2],
            [
                ("ka".into(), Some(13.0), WireQuality::Good),
                ("kb".into(), Some(1.0), WireQuality::Good),
            ]
        );
        // 失敗 1 回目: 直前値で Stale（値の取れない列は直前値が無いので Bad）。
        let (items, first) = state.on_query_failure();
        assert!(first);
        assert_eq!(
            qualities(&items),
            vec![
                ("ka".into(), Some(13.0), WireQuality::Stale),
                ("kb".into(), Some(1.0), WireQuality::Stale),
                ("kn".into(), None, WireQuality::Bad),
                ("kg".into(), None, WireQuality::Bad),
            ]
        );
        assert!(!state.needs_redescribe());
        // 2 回目から Bad（describe し直す対象になる）。
        let (items, first) = state.on_query_failure();
        assert!(!first);
        assert!(items
            .iter()
            .all(|i| i.q == WireQuality::Bad && i.v.is_none()));
        assert!(state.needs_redescribe());
        // 成功で戻る。
        let items = state.on_row(&[Some(14.0), Some(0.0)]);
        assert_eq!(items[0].q, WireQuality::Good);
        assert!(!state.needs_redescribe());
        // 0 行は梯子に乗せず即 Bad。直前値も捨てるので、次の失敗の Stale は直前値なしで Bad。
        assert!(state.on_zero_rows().iter().all(|i| i.q == WireQuality::Bad));
        let (items, _) = state.on_query_failure();
        assert!(items.iter().all(|i| i.q == WireQuality::Bad));
        // 非有限値は Bad。
        let items = state.on_row(&[Some(f64::NAN), Some(f64::INFINITY)]);
        assert_eq!(items[0].q, WireQuality::Bad);
        assert_eq!(items[1].q, WireQuality::Bad);
    }

    /// 直前値の無い Stale は Bad（まだ一度も読めていない）。
    #[test]
    fn a_first_failure_without_a_previous_value_is_bad() {
        let mut state = QueryState::resolved(plan(&[("a", "ka")]), &described(&[("a", "INT4")]));
        let (items, first) = state.on_query_failure();
        assert!(first);
        assert_eq!(
            qualities(&items),
            vec![("ka".into(), None, WireQuality::Bad)]
        );
    }

    /// describe できない SQL は全キー Bad、describe し直して読めたら採る。
    #[test]
    fn an_unprepared_query_sends_bad_until_it_can_be_described() {
        let mut state =
            QueryState::unprepared(plan(&[("a", "ka"), ("b", "kb")]), "relation does not exist");
        assert!(state.wrapper_sql().is_none());
        assert!(state.needs_redescribe());
        assert!(state
            .without_query()
            .iter()
            .all(|i| i.q == WireQuality::Bad));
        assert!(state
            .bad_columns()
            .all(|(_, r)| matches!(r, ColumnBadReason::StatementNotPrepared(_))));
        assert!(state.adopt_redescribed(&described(&[("a", "INT4"), ("b", "INT4")])));
        assert!(state.wrapper_sql().is_some());
        // 同じ describe の結果はもう採らない（SQL を投げている間）。
        assert!(!state.adopt_redescribed(&described(&[("a", "INT4"), ("b", "INT4")])));
        assert!(state.adopt_redescribed(&described(&[("a", "INT4"), ("b", "TEXT")])));
    }

    #[test]
    fn status_text_reports_changes_only_and_redacts() {
        let mut redactor = Redactor::new();
        redactor.add_secret("pw-secret");
        let mut status = StatusText::new(2);
        assert_eq!(status.take_change(&redactor), Some(None));
        assert_eq!(status.take_change(&redactor), None);
        status.queries[1] = Some("q2: クエリが失敗しました（pw-secret）".to_string());
        assert_eq!(
            status.take_change(&redactor),
            Some(Some("q2: クエリが失敗しました（<redacted>）".to_string()))
        );
        assert_eq!(status.take_change(&redactor), None);
        status.connection = Some("DB に接続できません".to_string());
        assert_eq!(
            status.take_change(&redactor),
            Some(Some("DB に接続できません".to_string()))
        );
    }
}
