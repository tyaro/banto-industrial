//! `pg_connect_options` が課すサーバー側の `statement_timeout`
//! （[`banto_db_source::SERVER_STATEMENT_TIMEOUT`] = 30 秒）の実 PostgreSQL での確認。
//!
//! 動機は「クライアントが future を捨てたあとも、サーバーは文を走らせ続ける」
//! こと（`pg.rs` の定数の doc comment）。ここでは接続を**必ず**
//! `banto_db_source::pg_connect_options` で作り（Hub・ブリッジと同じ組み立て）、
//! テストごとに一意の `application_name` を付けて並行実行でも互いの
//! `pg_stat_activity` を見ないようにする。実際の 30 秒を待つので 1 本が 30〜40
//! 秒かかる（テストは並行に走る）。
//!
//! `BANTO_TEST_PG_URL`（例 `postgres://postgres:postgres@127.0.0.1:5432/postgres`）
//! が無ければ `skipped` を出して成功する（他の PostgreSQL テストと同じ）。CI は
//! `Rust (db-source / PostgreSQL)` ジョブの `cargo test -p banto-db-source` で走る。
//!
//! - `server_side_statement_timeout_is_set_on_a_pool`: `SHOW statement_timeout` = `30s`。
//! - `a_long_statement_is_cancelled_by_the_server`: クライアントの時間切れなしで
//!   `pg_sleep(40)` が約 30 秒で `57014` になり、`classify` は `Statement`。
//! - `a_dropped_client_future_stops_on_the_server_by_the_timeout`: 本題。
//!   future を捨てたあとサーバーの `active` な文が 30 秒の上限で消える。
//! - `describe_waiting_for_a_lock_is_cancelled_by_the_server`: ロック待ちの
//!   `describe`（Parse）も `57014`。

use std::time::{Duration, Instant};

use banto_db_source::{classify, pg_connect_options, PgFailure, SERVER_STATEMENT_TIMEOUT};
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};
use sqlx::{Connection, Executor, PgConnection, Row, SqlSafeStr};

struct PgTarget {
    host: String,
    port: u16,
    database: String,
    username: String,
    password: String,
}

fn pg_target() -> Option<PgTarget> {
    let url = std::env::var("BANTO_TEST_PG_URL").ok()?;
    let options: PgConnectOptions = url
        .parse()
        .expect("BANTO_TEST_PG_URL should be a valid postgres:// URL");
    // `PgConnectOptions` はパスワードを読み出せない（書き込み専用）ので URL から取る。
    let password = url
        .split_once("://")
        .and_then(|(_, rest)| rest.split_once('@'))
        .and_then(|(userinfo, _)| userinfo.split_once(':'))
        .map(|(_, password)| password.to_string())
        .unwrap_or_default();
    Some(PgTarget {
        host: options.get_host().to_string(),
        port: options.get_port(),
        database: options.get_database().unwrap_or("postgres").to_string(),
        username: options.get_username().to_string(),
        password,
    })
}

macro_rules! require_pg {
    () => {
        match pg_target() {
            Some(target) => target,
            None => {
                eprintln!("skipped: BANTO_TEST_PG_URL unset");
                return;
            }
        }
    };
}

impl PgTarget {
    /// 被試験体の接続オプション。必ず共有の組み立て点を通す。
    fn options(&self, application_name: &str) -> PgConnectOptions {
        pg_connect_options(
            &self.host,
            self.port,
            &self.database,
            &self.username,
            &self.password,
            application_name,
        )
    }

    async fn pool(&self, application_name: &str) -> PgPool {
        PgPoolOptions::new()
            .max_connections(2)
            .acquire_timeout(Duration::from_secs(5))
            .connect_with(self.options(application_name))
            .await
            .expect("connect to BANTO_TEST_PG_URL")
    }

    /// 管理用の接続。**statement_timeout を課さない**よう共有の組み立て点を通さず、
    /// `application_name` だけ別に付ける（被試験体の接続と見分ける）。
    async fn admin(&self, application_name: &str) -> PgConnection {
        let options = PgConnectOptions::new()
            .host(&self.host)
            .port(self.port)
            .database(&self.database)
            .username(&self.username)
            .password(&self.password)
            .application_name(application_name);
        PgConnection::connect_with(&options)
            .await
            .expect("connect admin to BANTO_TEST_PG_URL")
    }
}

/// 一意な名前（テストの並行実行・再実行で衝突させない）。
fn unique(label: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("banto_stmt_to_{label}_{}_{nanos}", std::process::id())
}

fn sqlstate(err: &sqlx::Error) -> Option<String> {
    match err {
        sqlx::Error::Database(db) => db.code().map(|c| c.to_string()),
        _ => None,
    }
}

#[tokio::test]
async fn server_side_statement_timeout_is_set_on_a_pool() {
    let target = require_pg!();
    let pool = target.pool(&unique("show")).await;
    let value: String = sqlx::query_scalar("SHOW statement_timeout")
        .fetch_one(&pool)
        .await
        .expect("SHOW statement_timeout");
    assert_eq!(SERVER_STATEMENT_TIMEOUT, Duration::from_secs(30));
    assert_eq!(value, "30s");
    // プールの 2 本目の接続にも同じ設定が載る。
    let (a, b) = tokio::join!(
        sqlx::query_scalar::<_, String>("SELECT current_setting('statement_timeout')")
            .fetch_one(&pool),
        sqlx::query_scalar::<_, String>("SELECT current_setting('statement_timeout')")
            .fetch_one(&pool),
    );
    assert_eq!(a.unwrap(), "30s");
    assert_eq!(b.unwrap(), "30s");
}

#[tokio::test]
async fn a_long_statement_is_cancelled_by_the_server() {
    let target = require_pg!();
    let pool = target.pool(&unique("sleep")).await;
    let started = Instant::now();
    let err = sqlx::query("SELECT pg_sleep(40)")
        .execute(&pool)
        .await
        .expect_err("pg_sleep(40) must be cancelled by the server");
    let elapsed = started.elapsed();
    eprintln!("pg_sleep(40) cancelled after {elapsed:?}: {err}");
    assert_eq!(sqlstate(&err).as_deref(), Some("57014"), "err: {err}");
    assert!(
        elapsed >= Duration::from_secs(29) && elapsed < Duration::from_secs(36),
        "cancelled after {elapsed:?}"
    );
    assert!(
        matches!(classify(&err), PgFailure::Statement(_)),
        "classify: {:?}",
        classify(&err)
    );
    // 取り消されたあとも接続は使える（プールを捨てる理由にならない）。
    let one: i32 = sqlx::query_scalar("SELECT 1")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(one, 1);
}

/// `pg_stat_activity` の「この application_name の、pg_sleep を実行中の
/// セッション」の数。
async fn active_sleepers(admin: &mut PgConnection, application_name: &str) -> i64 {
    sqlx::query(
        "SELECT count(*) FROM pg_stat_activity \
         WHERE application_name = $1 AND state = 'active' AND query LIKE '%pg_sleep%' \
         AND pid <> pg_backend_pid()",
    )
    .bind(application_name)
    .fetch_one(admin)
    .await
    .expect("query pg_stat_activity")
    .get::<i64, _>(0)
}

/// `close_pool` が true ならプールも閉じる（捨てた future の接続が破棄される
/// 場面。sink が停止時にプールを閉じる）、false なら生かす（Hub の
/// クライアント時間切れ）。
async fn dropped_future_case(target: &PgTarget, label: &str, close_pool: bool) {
    let app = unique(label);
    let pool = target.pool(&app).await;
    let mut admin = target.admin(&unique("admin")).await;

    let started = Instant::now();
    let timed_out = tokio::time::timeout(
        Duration::from_secs(1),
        sqlx::query("SELECT pg_sleep(60)").execute(&pool),
    )
    .await;
    assert!(
        timed_out.is_err(),
        "the client-side timeout must fire first"
    );

    // 捨てた直後の様子（サーバーがまだ走っているか）を観測する。クライアントが
    // 見捨てた時点でサーバーは走っている（これが無いと「消えた」が意味を持たない）。
    let mut seen_active = active_sleepers(&mut admin, &app).await > 0;
    assert!(
        seen_active,
        "the abandoned statement should still be active right after the drop"
    );
    let mut gone_at = None;
    let deadline = started + Duration::from_secs(40);
    // `pool.close()` は await しない: 捨てた future の接続が残っているとき、
    // close は文が終わるまで（サーバーが取り消すまで）戻らない。環境によっては
    // 戻らないままのことがあったので（実測）、閉じる操作は裏で走らせ、サーバー側の
    // 結果（pg_stat_activity）だけを見る。戻ったかどうかは最後にログへ出す。
    let closer = close_pool.then(|| {
        let pool = pool.clone();
        tokio::spawn(async move {
            pool.close().await;
            Instant::now()
        })
    });
    while Instant::now() < deadline {
        let active = active_sleepers(&mut admin, &app).await;
        if active > 0 {
            seen_active = true;
        } else if seen_active {
            gone_at = Some(started.elapsed());
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    eprintln!(
        "dropped future (close_pool={close_pool}): seen_active={seen_active}, gone_at={gone_at:?}"
    );
    if let Some(closer) = closer {
        match tokio::time::timeout(Duration::from_secs(5), closer).await {
            Ok(Ok(at)) => eprintln!("pool.close() returned at {:?}", at - started),
            _ => eprintln!("pool.close() had not returned 5 s after the statement was gone"),
        }
    }
    assert!(
        gone_at.is_some(),
        "the abandoned pg_sleep must be gone from pg_stat_activity within 40 s \
         (seen_active={seen_active})"
    );
}

#[tokio::test]
async fn a_dropped_client_future_stops_on_the_server_by_the_timeout() {
    let target = require_pg!();
    // プールを生かしたまま（Hub のクライアント時間切れ）と、閉じる場合の両方。
    tokio::join!(
        dropped_future_case(&target, "dropkeep", false),
        dropped_future_case(&target, "dropclose", true),
    );
}

#[tokio::test]
async fn describe_waiting_for_a_lock_is_cancelled_by_the_server() {
    let target = require_pg!();
    let table = unique("locked");
    let pool = target.pool(&unique("describe")).await;
    let mut admin = target.admin(&unique("admin")).await;

    admin
        .execute(sqlx::AssertSqlSafe(format!("CREATE TABLE {table} (a int)")))
        .await
        .expect("create table");
    admin.execute("BEGIN").await.expect("begin");
    admin
        .execute(sqlx::AssertSqlSafe(format!(
            "LOCK TABLE {table} IN ACCESS EXCLUSIVE MODE"
        )))
        .await
        .expect("lock table");

    let started = Instant::now();
    let statement = sqlx::AssertSqlSafe(format!("SELECT * FROM {table}")).into_sql_str();
    let result = (&pool).describe(statement).await;
    let elapsed = started.elapsed();

    // 後始末を先に（アサートで落ちても表を残さない）。
    let _ = admin.execute("ROLLBACK").await;
    let _ = admin
        .execute(sqlx::AssertSqlSafe(format!("DROP TABLE IF EXISTS {table}")))
        .await;

    let err = result.expect_err("describe must wait for the lock and be cancelled");
    eprintln!("describe under ACCESS EXCLUSIVE cancelled after {elapsed:?}: {err}");
    assert_eq!(sqlstate(&err).as_deref(), Some("57014"), "err: {err}");
    assert!(
        elapsed >= Duration::from_secs(29) && elapsed < Duration::from_secs(36),
        "cancelled after {elapsed:?}"
    );
    assert!(matches!(classify(&err), PgFailure::Statement(_)));
}
