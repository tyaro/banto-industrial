//! DB 接続ごとの `PgPool` と flusher タスク（設計 §5.3・§5.4）。
//!
//! ## 1 接続 = 1 flusher
//!
//! 同じ DB 接続を使う sink group は 1 本の flusher が順に処理する。
//! 接続数ぶんしか同時に走らないので、グループを増やしても DB へのセッション
//! 数は増えない（プールは `max_connections(2)` - DB Source と同じ理由:
//! 1 本しか同時に使わないが、障害復帰時に古い接続が閉じ切る前に新しい
//! 接続を張れるように 2）。
//!
//! ## 1 周回でやること
//!
//! 1. `flush_interval_ms` の経過、または `batch_size` に達したプロデューサ
//!    からの起床（[`Notify`]）で目を覚ます。
//! 2. その接続のグループを id 順に見る。テーブル検査（`SELECT ... LIMIT 0`）
//!    に通っていなければ先に検査する（設計 §5.3。**DDL は発行しない** -
//!    失敗したらそのグループを `error` にして 30 秒後に再検査）。
//! 3. キューの先頭から `batch_size` 行ずつ multi-row INSERT。
//!    **commit してからキューを進める**（at-least-once）。
//!
//! ## 失敗の扱い（2 段階）
//!
//! - **文レベル**（テーブル/列が無い・型が合わない・権限が無い）:
//!   そのグループだけ `error`。他のグループは同じ周回で処理を続ける
//!   （設計 §5.3「他グループには影響しない」）。
//! - **接続レベル**（DB 停止・ネットワーク断・プール枯渇）: その接続の
//!   全グループを `backoff` にして、指数バックオフ（1s → 30s、
//!   `banto_hub_core::db_source::task` と**同じ定数**）で待ってから
//!   周回をやり直す。行はキューに残る（bounded なので溢れれば最古から
//!   捨てられる - [`crate::queue`]）。
//!
//! ## 停止
//!
//! [`FlusherHandle::request_drain`] で「間隔を待たず、空になるまで
//! 流し続ける」モードへ入る（設計 §5.6 の停止時 flush）。
//! [`FlusherHandle::stop`] は周回の切れ目・バックオフの待ち・
//! 間隔待ちのいずれからでも即座に抜ける。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions, PgSslMode};
use sqlx::Error as SqlxError;
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use crate::group::{now_ms, GroupRegistry, GroupRuntime};
use crate::hub_api::SinkConfigConnection;
use crate::log::{log_err_line, log_line};
use crate::queue::Row;
use crate::sql::{recommended_ddl, Table};
use crate::status::{classify_scope, is_undefined_table, sanitize_db_error, ErrorScope, Redactor};

/// 接続プールの最大接続数（設計 §4.2「最大接続数は小さく固定、例 2」を
/// Sink 側でも踏襲）。
const POOL_MAX_CONNECTIONS: u32 = 2;

/// プールから接続を取り出すまでの上限（S1a の接続テスト・DB Source と同じ
/// 5 秒）。
const POOL_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);

/// `pg_stat_activity` で見分けるための固定名（実装指示）。DB Source は
/// 接続名を後ろに付けるが、Sink は 1 プロセス 1 用途なので固定文字列。
const APPLICATION_NAME: &str = "banto-hub-sink";

/// 再接続バックオフ（設計 §5.4「Source と同じ定数」）。
/// `apps/banto-hub/core/src/db_source/task.rs` の `BACKOFF_BASE`/
/// `BACKOFF_CAP`/`BACKOFF_FACTOR` を**そのまま**写したもの - 由来は
/// `banto_broker::BackoffConfig::default()`（base 1s / cap 30s / factor 2）。
const BACKOFF_BASE: Duration = Duration::from_secs(1);
const BACKOFF_CAP: Duration = Duration::from_secs(30);
const BACKOFF_FACTOR: u32 = 2;

/// `attempt` 回目（1 始まり）の再試行までの待ち時間。
/// `banto_hub_core::db_source::task::backoff_delay` と同じ形
/// （`base * factor^(attempt-1)`、`cap` で頭打ち）。
pub fn backoff_delay(attempt: u32) -> Duration {
    if attempt == 0 {
        return Duration::ZERO;
    }
    let growth = (BACKOFF_FACTOR as u64)
        .checked_pow(attempt - 1)
        .unwrap_or(u64::MAX);
    let ms = (BACKOFF_BASE.as_millis() as u64).saturating_mul(growth);
    Duration::from_millis(ms).min(BACKOFF_CAP)
}

/// flusher の調整値（[`crate::config`] 由来 + テスト用の再検査間隔）。
#[derive(Clone, Copy, Debug)]
pub struct FlushOptions {
    pub flush_interval: Duration,
    pub batch_size: usize,
    pub table_recheck: Duration,
}

/// 接続の設定から `PgPool` を作る。**接続はここでは張らない**
/// （`connect_lazy_with`）- Hub より先に DB が上がっている保証は無く、
/// 起動時に繋がらないことは異常ではないため（設計 §5.6「Hub 未起動なら
/// 待つ」と同じ姿勢）。
pub fn build_pool(connection: &SinkConfigConnection) -> Result<PgPool, String> {
    let port = u16::try_from(connection.port)
        .map_err(|_| format!("ポート番号 {} は 1〜65535 の範囲外です", connection.port))?;
    if port == 0 {
        return Err("ポート番号 0 は使えません".to_string());
    }
    let options = PgConnectOptions::new()
        .host(&connection.host)
        .port(port)
        .database(connection.database.as_deref().unwrap_or(""))
        .username(connection.username.as_deref().unwrap_or(""))
        .password(connection.password.as_deref().unwrap_or(""))
        .application_name(APPLICATION_NAME)
        // 設計 §4.2「sslmode prefer」: TLS が使えれば使い、使えなければ
        // 平文へフォールバックする（v1 は閉域 LAN 前提 §2.2）。DB Source の
        // `pg_connect_options` と同じ選択。
        .ssl_mode(PgSslMode::Prefer);
    Ok(PgPoolOptions::new()
        .max_connections(POOL_MAX_CONNECTIONS)
        .acquire_timeout(POOL_ACQUIRE_TIMEOUT)
        .connect_lazy_with(options))
}

/// 走っている flusher 1 本への操作口。
pub struct FlusherHandle {
    pub connection_id: i64,
    /// 差分判定のために保持している「今その flusher が繋いでいる先」。
    pub connection: SinkConfigConnection,
    pool: PgPool,
    wake: Arc<Notify>,
    stop: Arc<Notify>,
    draining: Arc<AtomicBool>,
    task: JoinHandle<()>,
}

impl FlusherHandle {
    /// プロデューサが `batch_size` に達したときに叩く起床口。
    pub fn waker(&self) -> Arc<Notify> {
        self.wake.clone()
    }

    /// 「間隔を待たずに空になるまで流す」モードへ（停止処理の第 1 段）。
    pub fn request_drain(&self) {
        self.draining.store(true, Ordering::Relaxed);
        self.wake.notify_one();
    }

    /// 停止して join する。`timeout` を超えたら abort する（DB が固まって
    /// いる場合に停止処理ごと止まらないため - トランザクションは
    /// commit 前なので abort しても二重書き込みにはならない）。
    pub async fn stop(self, timeout: Duration) {
        self.stop.notify_one();
        match tokio::time::timeout(timeout, self.task).await {
            Ok(_) => {}
            Err(_) => {
                log_err_line(&format!(
                    "banto-hub-sink: warn: 接続 id={} の flusher が {}秒で停止しませんでした（強制終了します）",
                    self.connection_id,
                    timeout.as_secs_f64()
                ));
            }
        }
        self.pool.close().await;
    }
}

/// flusher を起動する。
pub fn spawn_flusher(
    connection: SinkConfigConnection,
    pool: PgPool,
    groups: Arc<GroupRegistry>,
    options: FlushOptions,
    redactor: Redactor,
) -> FlusherHandle {
    let wake = Arc::new(Notify::new());
    let stop = Arc::new(Notify::new());
    let draining = Arc::new(AtomicBool::new(false));
    let worker = Flusher {
        connection_id: connection.id,
        pool: pool.clone(),
        groups,
        options,
        redactor,
        wake: wake.clone(),
        stop: stop.clone(),
        draining: draining.clone(),
    };
    let task = tokio::spawn(worker.run());
    FlusherHandle {
        connection_id: connection.id,
        connection,
        pool,
        wake,
        stop,
        draining,
        task,
    }
}

struct Flusher {
    connection_id: i64,
    pool: PgPool,
    groups: Arc<GroupRegistry>,
    options: FlushOptions,
    redactor: Redactor,
    wake: Arc<Notify>,
    stop: Arc<Notify>,
    draining: Arc<AtomicBool>,
}

/// 1 周回の結果（バックオフの判断に使う）。
enum RoundOutcome {
    /// 何事もなく終わった（残りが無いか、グループ単位のエラーのみ）。
    Idle,
    /// 接続レベルの失敗 - バックオフする。
    ConnectionFailed,
}

impl Flusher {
    async fn run(self) {
        let mut attempt: u32 = 0;
        loop {
            if !self.draining.load(Ordering::Relaxed) {
                tokio::select! {
                    _ = self.stop.notified() => return,
                    _ = self.wake.notified() => {}
                    _ = tokio::time::sleep(self.options.flush_interval) => {}
                }
            } else if self.all_queues_empty() {
                // drain 指示済みでキューも空 - 停止要求を待つだけ。
                self.stop.notified().await;
                return;
            }

            match self.round().await {
                RoundOutcome::Idle => attempt = 0,
                RoundOutcome::ConnectionFailed => {
                    attempt = attempt.saturating_add(1);
                    let delay = backoff_delay(attempt);
                    tokio::select! {
                        _ = self.stop.notified() => return,
                        _ = tokio::time::sleep(delay) => {}
                    }
                }
            }
        }
    }

    fn all_queues_empty(&self) -> bool {
        self.groups
            .for_connection(self.connection_id)
            .iter()
            .all(|group| group.queued() == 0)
    }

    async fn round(&self) -> RoundOutcome {
        for group in self.groups.for_connection(self.connection_id) {
            if !group.table_ready() {
                if !group.may_check_table(std::time::Instant::now()) {
                    continue;
                }
                match self.check_table(&group).await {
                    Ok(()) => {}
                    Err(ErrorScope::Connection) => return RoundOutcome::ConnectionFailed,
                    Err(ErrorScope::Group) => continue,
                }
            }
            match self.flush_group(&group).await {
                Ok(()) => {}
                Err(ErrorScope::Connection) => return RoundOutcome::ConnectionFailed,
                Err(ErrorScope::Group) => continue,
            }
        }
        RoundOutcome::Idle
    }

    /// 設計 §5.3 のテーブル検査。失敗したらそのグループを `error` にし、
    /// 「テーブルが無い」場合だけ推奨 DDL を 1 回 `warn` で案内する。
    async fn check_table(&self, group: &Arc<GroupRuntime>) -> Result<(), ErrorScope> {
        let sql = group.table.check_sql();
        match sqlx::query(sqlx::AssertSqlSafe(sql))
            .fetch_all(&self.pool)
            .await
        {
            Ok(_) => {
                if group.mark_table_ready() {
                    log_line(&format!(
                        "banto-hub-sink: グループ {} (id={}) の保存先 {} を確認しました",
                        group.name,
                        group.id,
                        group.table.raw()
                    ));
                }
                Ok(())
            }
            Err(err) => {
                let scope = classify_scope(&err);
                let message = self.redactor.apply(&sanitize_db_error(&err));
                match scope {
                    ErrorScope::Connection => {
                        self.fail_connection(&message);
                    }
                    ErrorScope::Group => {
                        if group.mark_error(message.clone(), self.options.table_recheck) {
                            log_err_line(&format!(
                                "banto-hub-sink: warn: グループ {} (id={}) の保存先 {} を検査できません: {message}",
                                group.name,
                                group.id,
                                group.table.raw()
                            ));
                        }
                        if is_undefined_table(&err) && group.take_ddl_hint_slot() {
                            log_err_line(&format!(
                                "banto-hub-sink: warn: テーブルが存在しません。DB 管理者に次の DDL を依頼してください（サイドカーと Hub は DDL を発行しません）: {}",
                                recommended_ddl(group.table.raw())
                            ));
                        }
                    }
                }
                Err(scope)
            }
        }
    }

    /// キューが空になるか失敗するまで `batch_size` 単位で流す。
    async fn flush_group(&self, group: &Arc<GroupRuntime>) -> Result<(), ErrorScope> {
        loop {
            let batch = group.peek_batch(self.options.batch_size);
            if batch.is_empty() {
                return Ok(());
            }
            match insert_batch(&self.pool, &group.table, &batch).await {
                Ok(()) => {
                    // commit されてからキューを進める（at-least-once）。
                    group.commit_batch(batch.len());
                    if group.mark_flushed(now_ms()) {
                        log_line(&format!(
                            "banto-hub-sink: グループ {} (id={}) の保存を再開しました",
                            group.name, group.id
                        ));
                    }
                }
                Err(err) => {
                    let scope = classify_scope(&err);
                    let message = self.redactor.apply(&sanitize_db_error(&err));
                    match scope {
                        ErrorScope::Connection => self.fail_connection(&message),
                        ErrorScope::Group => {
                            if group.mark_error(message.clone(), self.options.table_recheck) {
                                log_err_line(&format!(
                                    "banto-hub-sink: warn: グループ {} (id={}) への INSERT に失敗しました（行はキューに残ります）: {message}",
                                    group.name, group.id
                                ));
                            }
                            if is_undefined_table(&err) && group.take_ddl_hint_slot() {
                                log_err_line(&format!(
                                    "banto-hub-sink: warn: テーブルが存在しません。DB 管理者に次の DDL を依頼してください: {}",
                                    recommended_ddl(group.table.raw())
                                ));
                            }
                        }
                    }
                    return Err(scope);
                }
            }
        }
    }

    /// 接続レベルの失敗 - この接続の全グループを `backoff` にする。
    /// `warn` は状態が変わったグループがあったときだけ 1 行
    /// （設計 §5.5 の抑制）。
    fn fail_connection(&self, message: &str) {
        let mut announced = false;
        for group in self.groups.for_connection(self.connection_id) {
            if group.mark_backoff(message.to_string()) && !announced {
                announced = true;
            }
        }
        if announced {
            log_err_line(&format!(
                "banto-hub-sink: warn: DB 接続 id={} が使えません（再試行します。行はキューに残ります）: {message}",
                self.connection_id
            ));
        }
    }
}

/// 1 バッチを 1 トランザクションで INSERT する（設計 §5.4）。
/// 値はすべて bind パラメータで、SQL には行数しか反映されない
/// （[`crate::sql`]）。
async fn insert_batch(pool: &PgPool, table: &Table, rows: &[Row]) -> Result<(), SqlxError> {
    debug_assert!(!rows.is_empty());
    let sql = table.insert_sql(rows.len());
    let mut tx = pool.begin().await?;
    let mut query = sqlx::query(sqlx::AssertSqlSafe(sql));
    for row in rows {
        query = query
            // `ts` は `to_timestamp($n)`（epoch 秒の double precision）で
            // timestamptz へ変換する - [`crate::sql`] のモジュール doc 参照。
            .bind(row.ts_ms as f64 / 1_000.0)
            .bind(row.tag_id)
            .bind(row.external_name.as_str())
            .bind(row.value)
            .bind(row.quality.as_str());
    }
    query.execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_matches_the_db_source_schedule() {
        assert_eq!(backoff_delay(0), Duration::ZERO);
        assert_eq!(backoff_delay(1), Duration::from_secs(1));
        assert_eq!(backoff_delay(2), Duration::from_secs(2));
        assert_eq!(backoff_delay(3), Duration::from_secs(4));
        assert_eq!(backoff_delay(4), Duration::from_secs(8));
        assert_eq!(backoff_delay(5), Duration::from_secs(16));
        // 上限 30 秒で頭打ち（1s → 30s、設計 §5.4）。
        assert_eq!(backoff_delay(6), Duration::from_secs(30));
        assert_eq!(backoff_delay(7), Duration::from_secs(30));
        assert_eq!(backoff_delay(u32::MAX), Duration::from_secs(30));
    }

    fn connection(port: i64) -> SinkConfigConnection {
        SinkConfigConnection {
            id: 1,
            name: "erp-db".to_string(),
            host: "127.0.0.1".to_string(),
            port,
            database: Some("erp".to_string()),
            username: Some("app".to_string()),
            password: Some("s3cret".to_string()),
        }
    }

    /// プールの構築はネットワーク I/O を伴わない（`connect_lazy_with`）
    /// ので、DB が無いテスト環境でも成功する。
    #[tokio::test]
    async fn building_a_pool_does_not_connect() {
        let pool = build_pool(&connection(5432)).expect("pool");
        assert_eq!(pool.size(), 0, "接続はまだ張らない");
        pool.close().await;
    }

    #[test]
    fn an_out_of_range_port_is_rejected_without_leaking_the_password() {
        let err = build_pool(&connection(70_000)).expect_err("out of range");
        assert!(err.contains("70000"), "{err}");
        assert!(!err.contains("s3cret"), "{err}");
        assert!(build_pool(&connection(0)).is_err());
        assert!(build_pool(&connection(-1)).is_err());
    }
}
