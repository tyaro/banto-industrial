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
//! 間隔待ちのいずれからでも即座に抜ける。渡した `timeout` はタスクの
//! join と `pool.close()` の合計の期限で、応答しない DB が返却時の
//! ping で許可を握ったままでも、その期限で戻る。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use sqlx::postgres::{PgConnectOptions, PgConnection, PgPool, PgPoolOptions, PgSslMode};
use sqlx::Error as SqlxError;
use sqlx::{pool::PoolConnection, Connection, Postgres};
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

/// One table check or one complete batch (acquire, BEGIN, INSERT, COMMIT).
pub(crate) const DB_OPERATION_TIMEOUT: Duration = Duration::from_secs(30);

/// Check a delayed group again during drain without spinning or delaying stop.
const DRAIN_IDLE_POLL: Duration = Duration::from_millis(50);

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
    /// Fixed in production; injectable for deterministic timeout tests.
    pub operation_timeout: Duration,
}

/// 接続の設定から `PgPool` を作る。**接続はここでは張らない**
/// （`connect_lazy_with`）- Hub より先に DB が上がっている保証は無く、
/// 起動時に繋がらないことは異常ではないため（設計 §5.6「Hub 未起動なら
/// 待つ」と同じ姿勢）。
pub fn build_pool(connection: &SinkConfigConnection) -> Result<PgPool, String> {
    let options = pg_connect_options(connection)?;
    Ok(PgPoolOptions::new()
        .max_connections(POOL_MAX_CONNECTIONS)
        .acquire_timeout(POOL_ACQUIRE_TIMEOUT)
        .connect_lazy_with(options))
}

fn pg_connect_options(connection: &SinkConfigConnection) -> Result<PgConnectOptions, String> {
    let port = u16::try_from(connection.port)
        .map_err(|_| format!("ポート番号 {} は 1〜65535 の範囲外です", connection.port))?;
    if port == 0 {
        return Err("ポート番号 0 は使えません".to_string());
    }
    Ok(PgConnectOptions::new()
        .host(&connection.host)
        .port(port)
        .database(connection.database.as_deref().unwrap_or(""))
        .username(connection.username.as_deref().unwrap_or(""))
        .password(connection.password.as_deref().unwrap_or(""))
        .application_name(APPLICATION_NAME)
        // 設計 §4.2「sslmode prefer」: TLS が使えれば使い、使えなければ
        // 平文へフォールバックする（v1 は閉域 LAN 前提 §2.2）。DB Source の
        // `pg_connect_options` と同じ選択。
        .ssl_mode(PgSslMode::Prefer)
        // Startup parameter scoped to this Sink connection; no ALTER ROLE/DB.
        .options([(
            "statement_timeout",
            format!("{}ms", DB_OPERATION_TIMEOUT.as_millis()),
        )]))
}

/// [`FlusherHandle::stop`] の結果。OS シャットダウンの停止ログで、
/// 「完了」と「打ち切り」を分けるために使う（#589）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub enum FlusherStopOutcome {
    /// タスクが期限内に終わり、プールも閉じた。
    Completed,
    /// 期限内に終わらず abort した、またはプールを閉じ切れなかった。
    CutShort,
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

    /// 停止して join し、プールも閉じる。`timeout` は join と
    /// [`PgPool::close`] の**合計**の絶対期限。
    ///
    /// sqlx 0.9 は接続をプールへ戻すときに ping し、その間セマフォの許可を
    /// 握ったままにする。`close` はその許可が全部戻るまで待つので、応答
    /// しない DB では `close().await` が終わらない。期限を超えたらタスクを
    /// abort し、閉じ切れていないプールは待たずに捨てて戻る。COMMIT の
    /// 応答が失われた行は再送で重複し得る。呼び出した側は
    /// 未送信行の計上と Stopped の報告を続けられる。
    ///
    /// この future が途中で drop されたとき（運転ループが停止合図で設定
    /// 反映の `stop` を打ち切ったとき）は [`Drop`] がタスクを abort する。
    /// `JoinHandle` の drop は detach するだけで、abort しない。
    ///
    /// 期限内に join とプールの close が両方済んだら
    /// [`FlusherStopOutcome::Completed`]、どちらかを打ち切ったら
    /// [`FlusherStopOutcome::CutShort`]。
    pub async fn stop(mut self, timeout: Duration) -> FlusherStopOutcome {
        self.stop.notify_one();
        let deadline = tokio::time::Instant::now() + timeout;

        let join_limit = deadline.saturating_duration_since(tokio::time::Instant::now());
        let joined = !join_limit.is_zero()
            && tokio::time::timeout(join_limit, &mut self.task)
                .await
                .is_ok();
        if !joined {
            log_err_line(&format!(
                "banto-hub-sink: warn: 接続 id={} の flusher が {}秒で停止しませんでした（強制終了します）",
                self.connection_id,
                timeout.as_secs_f64()
            ));
            self.task.abort();
            let join_limit = deadline.saturating_duration_since(tokio::time::Instant::now());
            if !join_limit.is_zero() {
                let _ = tokio::time::timeout(join_limit, &mut self.task).await;
            }
        }

        let close_limit = deadline.saturating_duration_since(tokio::time::Instant::now());
        let closed = !close_limit.is_zero()
            && tokio::time::timeout(close_limit, self.pool.close())
                .await
                .is_ok();
        if !closed {
            log_err_line(&format!(
                "banto-hub-sink: warn: 接続 id={} のプールを {}秒以内に閉じられませんでした（待たずに停止を続けます）",
                self.connection_id,
                timeout.as_secs_f64()
            ));
        }
        if joined && closed {
            FlusherStopOutcome::Completed
        } else {
            FlusherStopOutcome::CutShort
        }
    }
}

impl Drop for FlusherHandle {
    fn drop(&mut self) {
        // `stop` が最後まで走ったあとの abort は、すでに終わったタスクに
        // 対する no-op。途中で future が捨てられたときは、detach せずに
        // ここで止める。
        self.task.abort();
    }
}

/// 応答しない PostgreSQL に接続を 1 本出した状態の flusher。
/// 停止合図で接続を返し、返却時の ping（sqlx 0.9）が応答を待つところまで
/// 進めてからタスクを終わらせる。`pool.close()` はその ping が許可を
/// 握っている間は戻らない。
#[cfg(test)]
pub(crate) async fn flusher_on_blackhole_pool(connection_id: i64) -> (FlusherHandle, TestServer) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let (ping_tx, ping_rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        let Ok((mut stream, _)) = listener.accept().await else {
            return;
        };
        if read_startup(&mut stream).await.is_err() {
            return;
        }
        if stream.write_all(&postgres_ready_for_query()).await.is_err() {
            return;
        }
        let mut buf = [0_u8; 1024];
        let mut saw_ping = false;
        let mut ping_tx = Some(ping_tx);
        loop {
            match stream.read(&mut buf).await {
                Ok(0) | Err(_) => return,
                Ok(_) if !saw_ping => {
                    saw_ping = true;
                    if let Some(tx) = ping_tx.take() {
                        let _ = tx.send(());
                    }
                }
                Ok(_) => {}
            }
        }
    });

    let options = PgConnectOptions::new()
        .host("127.0.0.1")
        .port(port)
        .username("app")
        .database("erp")
        .password("pw")
        .ssl_mode(PgSslMode::Disable);
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(3))
        .test_before_acquire(false)
        .connect_lazy_with(options);
    let conn = pool
        .acquire()
        .await
        .expect("startup handshake with the blackhole server");

    let stop = Arc::new(Notify::new());
    let stop_for_task = stop.clone();
    let task = tokio::spawn(async move {
        stop_for_task.notified().await;
        drop(conn);
        tokio::time::timeout(Duration::from_secs(2), ping_rx)
            .await
            .expect("return-to-pool ping did not reach the server")
            .expect("ping channel closed");
    });

    let handle = FlusherHandle {
        connection_id,
        connection: SinkConfigConnection {
            id: connection_id,
            name: "erp-db".to_string(),
            host: "127.0.0.1".to_string(),
            port: i64::from(port),
            database: Some("erp".to_string()),
            username: Some("app".to_string()),
            password: Some("pw".to_string()),
        },
        pool,
        wake: Arc::new(Notify::new()),
        stop,
        draining: Arc::new(AtomicBool::new(false)),
        task,
    };
    (handle, TestServer { task: server })
}

#[cfg(test)]
async fn read_startup(stream: &mut tokio::net::TcpStream) -> std::io::Result<()> {
    use tokio::io::AsyncReadExt;
    use tokio::io::AsyncWriteExt;

    let mut len_buf = [0_u8; 4];
    stream.read_exact(&mut len_buf).await?;
    let mut len = i32::from_be_bytes(len_buf);
    // sslmode が Prefer でも Disable でも、先頭が SSLRequest なら 'N' を返す。
    if len == 8 {
        let mut code_buf = [0_u8; 4];
        stream.read_exact(&mut code_buf).await?;
        if i32::from_be_bytes(code_buf) == 80877103 {
            stream.write_all(b"N").await?;
            stream.read_exact(&mut len_buf).await?;
            len = i32::from_be_bytes(len_buf);
        }
    }
    let rest = usize::try_from(len).unwrap_or(0).saturating_sub(4);
    if rest > 0 && rest < 8192 {
        let mut body = vec![0_u8; rest];
        stream.read_exact(&mut body).await?;
    }
    Ok(())
}

#[cfg(test)]
fn postgres_ready_for_query() -> Vec<u8> {
    fn message(tag: u8, body: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(1 + 4 + body.len());
        out.push(tag);
        out.extend_from_slice(&(4 + body.len() as i32).to_be_bytes());
        out.extend_from_slice(body);
        out
    }
    fn cstring_pair(name: &str, value: &str) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(name.as_bytes());
        body.push(0);
        body.extend_from_slice(value.as_bytes());
        body.push(0);
        message(b'S', &body)
    }
    let mut out = message(b'R', &0_i32.to_be_bytes());
    out.extend(cstring_pair("server_version", "15.0"));
    out.extend(cstring_pair("client_encoding", "UTF8"));
    out.extend(cstring_pair("DateStyle", "ISO, MDY"));
    out.extend(cstring_pair("integer_datetimes", "on"));
    out.extend(message(b'K', &[0, 0, 0, 1, 0, 0, 0, 2]));
    out.extend(message(b'Z', b"I"));
    out
}

/// テスト用サーバタスク。drop で abort する。
#[cfg(test)]
pub(crate) struct TestServer {
    task: tokio::task::JoinHandle<()>,
}

#[cfg(test)]
impl Drop for TestServer {
    fn drop(&mut self) {
        self.task.abort();
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
#[derive(Debug, PartialEq, Eq)]
enum RoundOutcome {
    /// 何事もなく終わった（残りが無いか、グループ単位のエラーのみ）。
    Idle,
    /// 接続レベルの失敗 - バックオフする。
    ConnectionFailed,
}

enum BoundedSqlError {
    Sqlx(SqlxError),
    TimedOut,
}

impl BoundedSqlError {
    fn scope(&self) -> ErrorScope {
        match self {
            Self::Sqlx(err) => classify_scope(err),
            Self::TimedOut => ErrorScope::Group,
        }
    }

    fn is_undefined_table(&self) -> bool {
        matches!(self, Self::Sqlx(err) if is_undefined_table(err))
    }
}

/// A completed operation can return its connection to the pool. On timeout,
/// SQLx error, or task abort, detach and drop the socket instead: transaction
/// drop only queues a rollback, and a subsequent pool return/ping may hang.
#[derive(Default)]
struct OperationConnection(Option<PoolConnection<Postgres>>);

impl OperationConnection {
    fn as_mut(&mut self) -> &mut PgConnection {
        self.0.as_mut().expect("acquired connection")
    }

    fn return_after_success(mut self) {
        drop(self.0.take());
    }
}

impl Drop for OperationConnection {
    fn drop(&mut self) {
        if let Some(connection) = self.0.take() {
            drop(connection.detach());
        }
    }
}

async fn check_table_bounded(
    pool: &PgPool,
    table: &Table,
    budget: Duration,
) -> Result<(), BoundedSqlError> {
    let sql = table.check_sql();
    let mut connection = OperationConnection::default();
    let result = tokio::time::timeout(budget, async {
        connection.0 = Some(pool.acquire().await?);
        sqlx::query(sqlx::AssertSqlSafe(sql))
            .fetch_all(connection.as_mut())
            .await?;
        Ok::<(), SqlxError>(())
    })
    .await;
    match result {
        Ok(Ok(())) => {
            connection.return_after_success();
            Ok(())
        }
        Ok(Err(err)) => Err(BoundedSqlError::Sqlx(err)),
        Err(_) => Err(BoundedSqlError::TimedOut),
    }
}

async fn insert_batch_bounded(
    pool: &PgPool,
    table: &Table,
    rows: &[Row],
    budget: Duration,
) -> Result<(), BoundedSqlError> {
    let mut connection = OperationConnection::default();
    let result = tokio::time::timeout(budget, async {
        connection.0 = Some(pool.acquire().await?);
        insert_batch(connection.as_mut(), table, rows).await
    })
    .await;
    match result {
        Ok(Ok(())) => {
            connection.return_after_success();
            Ok(())
        }
        Ok(Err(err)) => Err(BoundedSqlError::Sqlx(err)),
        Err(_) => Err(BoundedSqlError::TimedOut),
    }
}

impl Flusher {
    async fn run(self) {
        let mut attempt: u32 = 0;
        loop {
            let draining = self.draining.load(Ordering::Relaxed);
            let queued_before = if draining { self.queued_rows() } else { 0 };
            if !draining {
                tokio::select! {
                    _ = self.stop.notified() => return,
                    _ = self.wake.notified() => {}
                    _ = tokio::time::sleep(self.options.flush_interval) => {}
                }
            } else if queued_before == 0 {
                // drain 指示済みでキューも空 - 停止要求を待つだけ。
                self.stop.notified().await;
                return;
            } else {
                // Check stop even when a completed round needs no DB await.
                tokio::select! {
                    biased;
                    _ = self.stop.notified() => return,
                    _ = std::future::ready(()) => {}
                }
            }

            match self.round().await {
                RoundOutcome::Idle => {
                    attempt = 0;
                    // A timed-out group keeps its queue while waiting for the
                    // next table check. A shrinking queue keeps draining fast.
                    if draining && self.queued_rows() >= queued_before {
                        tokio::select! {
                            _ = self.stop.notified() => return,
                            _ = tokio::time::sleep(DRAIN_IDLE_POLL) => {}
                        }
                    }
                }
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

    fn queued_rows(&self) -> usize {
        self.groups
            .for_connection(self.connection_id)
            .iter()
            .map(|group| group.queued())
            .sum()
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
        match check_table_bounded(&self.pool, &group.table, self.options.operation_timeout).await {
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
                let scope = err.scope();
                let message = match &err {
                    BoundedSqlError::Sqlx(err) => self.redactor.apply(&sanitize_db_error(err)),
                    BoundedSqlError::TimedOut => format!(
                        "保存先検査が {:.1} 秒でタイムアウトしました",
                        self.options.operation_timeout.as_secs_f64()
                    ),
                };
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
                        if err.is_undefined_table() && group.take_ddl_hint_slot() {
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
            match insert_batch_bounded(
                &self.pool,
                &group.table,
                &batch,
                self.options.operation_timeout,
            )
            .await
            {
                Ok(()) => {
                    // commit されてからキューを進める（at-least-once）。
                    group.commit_batch(&batch);
                    if group.mark_flushed(now_ms()) {
                        log_line(&format!(
                            "banto-hub-sink: グループ {} (id={}) の保存を再開しました",
                            group.name, group.id
                        ));
                    }
                }
                Err(err) => {
                    let scope = err.scope();
                    let message = match &err {
                        BoundedSqlError::Sqlx(err) => self.redactor.apply(&sanitize_db_error(err)),
                        BoundedSqlError::TimedOut => format!(
                            "バッチ保存が {:.1} 秒でタイムアウトしました（保存結果は不明です。行は再送します）",
                            self.options.operation_timeout.as_secs_f64()
                        ),
                    };
                    match scope {
                        ErrorScope::Connection => self.fail_connection(&message),
                        ErrorScope::Group => {
                            if group.mark_error(message.clone(), self.options.table_recheck) {
                                log_err_line(&format!(
                                    "banto-hub-sink: warn: グループ {} (id={}) への INSERT に失敗しました（行はキューに残ります）: {message}",
                                    group.name, group.id
                                ));
                            }
                            if err.is_undefined_table() && group.take_ddl_hint_slot() {
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
async fn insert_batch(
    conn: &mut PgConnection,
    table: &Table,
    rows: &[Row],
) -> Result<(), SqlxError> {
    debug_assert!(!rows.is_empty());
    let sql = table.insert_sql(rows.len());
    let mut tx = conn.begin().await?;
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

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum StallAt {
        Never,
        TableCheck,
        Begin,
        Insert,
        Commit,
    }

    fn pg_message(tag: u8, body: &[u8]) -> Vec<u8> {
        let mut message = vec![tag];
        message.extend_from_slice(&(body.len() as i32 + 4).to_be_bytes());
        message.extend_from_slice(body);
        message
    }

    async fn read_pg_message(
        stream: &mut tokio::net::TcpStream,
    ) -> std::io::Result<Option<(u8, Vec<u8>)>> {
        use tokio::io::AsyncReadExt;
        let mut tag = [0];
        if stream.read(&mut tag).await? == 0 {
            return Ok(None);
        }
        let mut len = [0; 4];
        stream.read_exact(&mut len).await?;
        let body_len = i32::from_be_bytes(len) - 4;
        if !(0..=65_536).contains(&body_len) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "unexpected PostgreSQL frame length",
            ));
        }
        let mut body = vec![0; body_len as usize];
        stream.read_exact(&mut body).await?;
        Ok(Some((tag[0], body)))
    }

    async fn serve_mock_pg(
        mut stream: tokio::net::TcpStream,
        stall_at: StallAt,
        stall_once: Arc<AtomicBool>,
        reached: Arc<std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>>,
        inserts: Arc<std::sync::atomic::AtomicUsize>,
    ) -> std::io::Result<()> {
        use std::sync::atomic::Ordering;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        read_startup(&mut stream).await?;
        stream.write_all(&postgres_ready_for_query()).await?;
        let mut transaction_open = false;
        let mut pending_inserts = 0;
        let mut insert_statement = false;
        while let Some((tag, body)) = read_pg_message(&mut stream).await? {
            let stage = match tag {
                b'Q' if body.starts_with(b"BEGIN") => Some(StallAt::Begin),
                b'Q' if body.starts_with(b"COMMIT") => Some(StallAt::Commit),
                b'E' if insert_statement => Some(StallAt::Insert),
                b'E' => Some(StallAt::TableCheck),
                _ => None,
            };
            if stage == Some(stall_at) && stall_once.swap(false, Ordering::SeqCst) {
                if matches!(stage, Some(StallAt::Commit)) {
                    // The COMMIT took effect, but its response never reached SQLx.
                    inserts.fetch_add(pending_inserts, Ordering::SeqCst);
                }
                if let Some(tx) = reached.lock().expect("seen lock").take() {
                    let _ = tx.send(());
                }
                let mut discard = [0; 1024];
                while stream.read(&mut discard).await? != 0 {}
                return Ok(());
            }
            match tag {
                b'Q' => {
                    let (command, status) = if body.starts_with(b"BEGIN") {
                        transaction_open = true;
                        ("BEGIN", b'T')
                    } else if body.starts_with(b"COMMIT") {
                        transaction_open = false;
                        inserts.fetch_add(pending_inserts, Ordering::SeqCst);
                        pending_inserts = 0;
                        ("COMMIT", b'I')
                    } else {
                        transaction_open = false;
                        pending_inserts = 0;
                        ("ROLLBACK", b'I')
                    };
                    let mut command_body = command.as_bytes().to_vec();
                    command_body.push(0);
                    stream.write_all(&pg_message(b'C', &command_body)).await?;
                    stream.write_all(&pg_message(b'Z', &[status])).await?;
                }
                b'P' => {
                    insert_statement = body
                        .split(|byte| *byte == 0)
                        .nth(1)
                        .is_some_and(|sql| sql.starts_with(b"INSERT"));
                    stream.write_all(&pg_message(b'1', &[])).await?;
                }
                b'D' => {
                    if body.first() == Some(&b'S') {
                        let types: &[u32] = if insert_statement {
                            &[701, 20, 25, 701, 25]
                        } else {
                            &[]
                        };
                        let mut params = (types.len() as i16).to_be_bytes().to_vec();
                        for oid in types {
                            params.extend_from_slice(&oid.to_be_bytes());
                        }
                        stream.write_all(&pg_message(b't', &params)).await?;
                    }
                    stream.write_all(&pg_message(b'n', &[])).await?;
                }
                b'B' => stream.write_all(&pg_message(b'2', &[])).await?,
                b'E' => {
                    let command = if insert_statement {
                        pending_inserts += 1;
                        b"INSERT 0 1\0".as_slice()
                    } else {
                        b"SELECT 0\0".as_slice()
                    };
                    stream.write_all(&pg_message(b'C', command)).await?;
                }
                b'S' => {
                    let status = if transaction_open { b'T' } else { b'I' };
                    stream.write_all(&pg_message(b'Z', &[status])).await?;
                }
                b'C' => stream.write_all(&pg_message(b'3', &[])).await?,
                b'X' => return Ok(()),
                _ => {}
            }
        }
        Ok(())
    }

    async fn mock_pool(
        stall_at: StallAt,
    ) -> (
        PgPool,
        SinkConfigConnection,
        TestServer,
        tokio::sync::oneshot::Receiver<()>,
        Arc<std::sync::atomic::AtomicUsize>,
    ) {
        use tokio::net::TcpListener;
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
        let config = connection(listener.local_addr().expect("addr").port().into());
        let (tx, rx) = tokio::sync::oneshot::channel();
        let reached = Arc::new(std::sync::Mutex::new(Some(tx)));
        let stalled = Arc::new(AtomicBool::new(true));
        let inserts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let task = {
            let reached = reached.clone();
            let stalled = stalled.clone();
            let inserts = inserts.clone();
            tokio::spawn(async move {
                while let Ok((stream, _)) = listener.accept().await {
                    let reached = reached.clone();
                    let stalled = stalled.clone();
                    let inserts = inserts.clone();
                    tokio::spawn(async move {
                        let _ = serve_mock_pg(stream, stall_at, stalled, reached, inserts).await;
                    });
                }
            })
        };
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_millis(500))
            .test_before_acquire(false)
            .connect_lazy_with(pg_connect_options(&config).expect("options"));
        (pool, config, TestServer { task }, rx, inserts)
    }

    fn group(id: i64) -> Arc<GroupRuntime> {
        use crate::hub_api::{SinkConfigGroup, SinkConfigTag};
        let config = SinkConfigGroup {
            id,
            name: format!("sink-{id}"),
            db_connection_id: 1,
            mode: "interval".to_string(),
            interval_ms: 1_000,
            table_name: format!("sink_{id}"),
            store_bad: false,
            tags: vec![SinkConfigTag {
                connection_id: 10,
                group_id: 20,
                tag_id: id,
                external_name: format!("line.tag{id}"),
            }],
        };
        Arc::new(GroupRuntime::new(&config, 3, 0).expect("group"))
    }

    fn test_row(id: i64) -> Row {
        Row {
            ts_ms: 1_000,
            tag_id: id,
            external_name: format!("line.tag{id}"),
            value: Some(1.0),
            quality: "good".to_string(),
        }
    }

    fn test_options(operation_timeout: Duration) -> FlushOptions {
        FlushOptions {
            flush_interval: Duration::from_secs(60),
            batch_size: 2,
            table_recheck: Duration::from_secs(30),
            operation_timeout,
        }
    }

    fn test_flusher(pool: PgPool, groups: Arc<GroupRegistry>, budget: Duration) -> Flusher {
        Flusher {
            connection_id: 1,
            pool,
            groups,
            options: test_options(budget),
            redactor: Redactor::new(),
            wake: Arc::new(Notify::new()),
            stop: Arc::new(Notify::new()),
            draining: Arc::new(AtomicBool::new(false)),
        }
    }

    #[test]
    fn sink_connection_sets_only_its_own_statement_timeout() {
        let options = pg_connect_options(&connection(5432)).expect("options");
        assert_eq!(POOL_ACQUIRE_TIMEOUT, Duration::from_secs(5));
        assert_eq!(DB_OPERATION_TIMEOUT, Duration::from_secs(30));
        assert!(options
            .get_options()
            .expect("startup options")
            .contains("statement_timeout=30000ms"));
    }

    #[tokio::test]
    async fn timed_out_table_check_is_group_local_and_frees_the_pool_slot() {
        use crate::status::GroupState;
        let (pool, _connection, _server, reached, _inserts) = mock_pool(StallAt::TableCheck).await;
        let first = group(1);
        let second = group(2);
        let groups = Arc::new(GroupRegistry::new());
        groups.insert(first.clone());
        groups.insert(second.clone());
        let flusher = test_flusher(pool.clone(), groups, Duration::from_millis(800));

        assert_eq!(flusher.round().await, RoundOutcome::Idle);
        reached
            .await
            .expect("the first table check reached the mock DB");
        assert_eq!(first.state(), GroupState::Error);
        assert!(first
            .last_error()
            .expect("timeout status")
            .contains("タイムアウト"));
        assert!(!first.may_check_table(std::time::Instant::now()));
        assert!(second.table_ready(), "the following group must be checked");
        assert_eq!(pool.size(), 1, "only the replacement connection remains");
        drop(flusher);
        tokio::time::timeout(Duration::from_secs(2), pool.close())
            .await
            .expect("pool close");
    }

    #[tokio::test]
    async fn begin_insert_and_commit_timeouts_keep_rows_for_retry() {
        use crate::status::GroupState;
        for stage in [StallAt::Begin, StallAt::Insert, StallAt::Commit] {
            let (pool, _connection, _server, reached, inserts) = mock_pool(stage).await;
            let group = group(1);
            group.mark_table_ready();
            group.push_rows(vec![test_row(1)]);
            let groups = Arc::new(GroupRegistry::new());
            groups.insert(group.clone());
            let flusher = test_flusher(pool.clone(), groups, Duration::from_millis(800));

            assert_eq!(flusher.round().await, RoundOutcome::Idle, "{stage:?}");
            reached.await.expect("target operation reached the mock DB");
            assert_eq!(group.state(), GroupState::Error, "{stage:?}");
            assert_eq!(
                group.queued(),
                1,
                "uncertain result must not ACK: {stage:?}"
            );
            assert_eq!(pool.size(), 0, "timed-out connection detached: {stage:?}");

            group.mark_table_ready(); // simulate the scheduled recheck succeeding
            assert_eq!(
                flusher.round().await,
                RoundOutcome::Idle,
                "retry: {stage:?}"
            );
            assert_eq!(group.queued(), 0, "successful retry ACKs: {stage:?}");
            let expected = if stage == StallAt::Commit { 2 } else { 1 };
            assert_eq!(
                inserts.load(Ordering::SeqCst),
                expected,
                "only a lost COMMIT reply may duplicate persisted rows: {stage:?}"
            );
            drop(flusher);
            tokio::time::timeout(Duration::from_secs(2), pool.close())
                .await
                .expect("pool close");
        }
    }

    #[tokio::test]
    async fn stop_during_unknown_commit_keeps_the_existing_stop_budget() {
        let (pool, config, _server, reached, _inserts) = mock_pool(StallAt::Commit).await;
        let group = group(1);
        group.mark_table_ready();
        group.push_rows(vec![test_row(1)]);
        let groups = Arc::new(GroupRegistry::new());
        groups.insert(group.clone());
        let handle = spawn_flusher(
            config,
            pool,
            groups,
            test_options(Duration::from_secs(1)),
            Redactor::new(),
        );
        handle.request_drain();
        tokio::time::timeout(Duration::from_secs(2), reached)
            .await
            .expect("COMMIT reached the mock DB")
            .expect("COMMIT signal");
        let started = tokio::time::Instant::now();
        assert_eq!(
            handle.stop(Duration::from_millis(200)).await,
            FlusherStopOutcome::CutShort
        );
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(group.queued(), 1, "unknown COMMIT cannot ACK on stop");
    }

    async fn assert_stop_after_group_timeout(drain_first: bool) {
        use crate::status::GroupState;
        let (pool, config, _server, reached, _inserts) = mock_pool(StallAt::Commit).await;
        let group = group(1);
        group.mark_table_ready();
        group.push_rows(vec![test_row(1)]);
        let groups = Arc::new(GroupRegistry::new());
        groups.insert(group.clone());
        let handle = spawn_flusher(
            config,
            pool,
            groups,
            test_options(Duration::from_millis(100)),
            Redactor::new(),
        );
        if drain_first {
            handle.request_drain();
        } else {
            handle.waker().notify_one();
        }
        tokio::time::timeout(Duration::from_secs(2), reached)
            .await
            .expect("COMMIT reached the mock DB")
            .expect("COMMIT signal");
        tokio::time::timeout(Duration::from_secs(2), async {
            while group.state() != GroupState::Error {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("operation timeout becomes a group error");
        if !drain_first {
            handle.request_drain();
        }
        // On one worker, a synchronous drain spin prevents even this yield
        // and the independent heartbeat from running.
        tokio::task::yield_now().await;
        let heartbeat = tokio::spawn(async { 1 });
        assert_eq!(
            tokio::time::timeout(Duration::from_millis(500), heartbeat)
                .await
                .expect("other task gets CPU")
                .expect("heartbeat"),
            1
        );
        let started = tokio::time::Instant::now();
        assert_eq!(
            tokio::time::timeout(
                Duration::from_secs(1),
                handle.stop(Duration::from_millis(500))
            )
            .await
            .expect("stop budget is schedulable"),
            FlusherStopOutcome::Completed
        );
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(group.queued(), 1, "unknown COMMIT must remain unacked");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn timeout_then_drain_recheck_wait_yields_and_stops() {
        assert_stop_after_group_timeout(false).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn drain_during_timeout_recheck_wait_yields_and_stops() {
        assert_stop_after_group_timeout(true).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn successful_drain_flushes_all_batches() {
        let (pool, config, _server, _reached, inserts) = mock_pool(StallAt::Never).await;
        let group = group(1);
        group.mark_table_ready();
        group.push_rows(vec![test_row(1), test_row(2), test_row(3)]);
        let groups = Arc::new(GroupRegistry::new());
        groups.insert(group.clone());
        let mut options = test_options(Duration::from_secs(1));
        options.batch_size = 1;
        let handle = spawn_flusher(config, pool, groups, options, Redactor::new());
        handle.request_drain();
        tokio::time::timeout(Duration::from_secs(2), async {
            while group.queued() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("drain commits all batches");
        assert_eq!(inserts.load(Ordering::SeqCst), 3);
        assert_eq!(
            handle.stop(Duration::from_millis(500)).await,
            FlusherStopOutcome::Completed
        );
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

    /// flusher が止まらない場合、タイムアウト後に abort して join する。
    #[tokio::test]
    async fn stop_aborts_and_joins_a_task_that_does_not_respond() {
        // pending() は永遠に待つタスク - stop 信号を無視する。
        let task = tokio::spawn(std::future::pending::<()>());
        let handle = FlusherHandle {
            connection_id: 1,
            connection: connection(5432),
            pool: build_pool(&connection(5432)).expect("pool"),
            wake: Arc::new(Notify::new()),
            stop: Arc::new(Notify::new()),
            draining: Arc::new(AtomicBool::new(false)),
            task,
        };

        let start = tokio::time::Instant::now();
        let outcome = handle.stop(Duration::from_millis(50)).await;
        let elapsed = start.elapsed();
        assert_eq!(
            outcome,
            FlusherStopOutcome::CutShort,
            "abort したら打ち切り"
        );

        // タイムアウト時間は大幅に超えていないはず（abort + await は速い）。
        assert!(
            elapsed < Duration::from_secs(1),
            "stop は {}ms で返るべき（elapsed: {:?}）",
            50,
            elapsed
        );
    }

    /// `stop` の future を捨てても flusher タスクは detach されず abort される。
    /// 運転ループが設定反映の途中で停止合図を受けたときの引き渡し。
    #[tokio::test]
    async fn dropping_stop_aborts_the_flusher_task() {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let entered = Arc::new(Notify::new());
        let entered_for_task = entered.clone();
        let task = tokio::spawn(async move {
            struct Tell(Option<tokio::sync::oneshot::Sender<()>>);
            impl Drop for Tell {
                fn drop(&mut self) {
                    if let Some(tx) = self.0.take() {
                        let _ = tx.send(());
                    }
                }
            }
            let _tell = Tell(Some(tx));
            entered_for_task.notify_one();
            std::future::pending::<()>().await;
        });
        let handle = FlusherHandle {
            connection_id: 1,
            connection: connection(5432),
            pool: build_pool(&connection(5432)).expect("pool"),
            wake: Arc::new(Notify::new()),
            stop: Arc::new(Notify::new()),
            draining: Arc::new(AtomicBool::new(false)),
            task,
        };
        entered.notified().await;
        drop(handle.stop(Duration::from_secs(30)));
        tokio::time::timeout(Duration::from_secs(1), rx)
            .await
            .expect("タスクは abort されて drop される")
            .expect("oneshot");
    }

    /// 返却時の ping が戻らない DB でも、`stop` に渡した期限で
    /// `pool.close()` まで含めて戻る（期限の外で無期限に待たない）。
    #[tokio::test]
    async fn stop_returns_within_the_budget_when_pool_close_pings_a_blackhole() {
        let (handle, _server) = flusher_on_blackhole_pool(7).await;
        let budget = Duration::from_millis(400);
        let started = tokio::time::Instant::now();
        let outcome = tokio::time::timeout(Duration::from_secs(2), handle.stop(budget))
            .await
            .expect("応答しない DB の pool.close() が stop の期限の外で待っている");
        let elapsed = started.elapsed();
        assert_eq!(
            outcome,
            FlusherStopOutcome::CutShort,
            "プールを閉じ切れなかったら打ち切り"
        );
        assert!(
            elapsed >= Duration::from_millis(250),
            "ping が固まっているのに close が期限を待たずに戻った: {elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "stop が期限 {budget:?} を大きく超えた: {elapsed:?}"
        );
    }
}
