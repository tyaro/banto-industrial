//! DB Source（docs/banto-hub-external-db-design.md #228・§4）の実装置き場。
//!
//! **本スライス（S1a）ではここに接続テストしか無い。** S1 の受け入れ条件
//! （§7 スライス表「接続テストが `SELECT 1` と列一覧を返す」の PostgreSQL
//! 側実装）を満たすため、`protocol = "postgres"` の
//! [`banto_tags::PlcConnection`] に対して「接続 → `SELECT version()` を
//! 1回実行 → 切断」だけを行う [`test_connection`] を置く。S2（クエリ
//! グループのポーリングエンジン本体、Quality 変換、バックオフ）はこの
//! モジュールに追加される予定で、S1a のうちからこのモジュールをその
//! 置き場として確保しておく（`apps/banto-collect`が PLC 収集の置き場で
//! あるのと同じ役割分担）。
//!
//! `apps/banto-hub/core/src/rest.rs`の`POST /api/plc-connections/{id}/test`
//! ハンドラと`crate::mcp`の`test_saved_connection`ツールの両方がここを呼ぶ
//! （二重実装しない - 既存の`run_plc_connection_test`が REST/MCP 間で
//! 疎通確認ロジックを共有しているのと同じ規律）。
//!
//! # 資格情報の扱い
//!
//! [`test_connection`]が返す[`DbConnectionTestOutcome::error`]には**平文
//! パスワードも接続文字列全体も絶対に含めない**（design §4.6・§2.2）。
//! `sqlx::Error`をそのまま`{err}`で文字列化せず、[`sanitize_postgres_error`]
//! でカテゴリ + DB 自身のエラーメッセージ（パスワードを含まないことが
//! 保証されている - PostgreSQL 自体がエラーメッセージにクライアントの
//! 送信したパスワードを含めることはない）だけを取り出す。

use std::time::Duration;

use banto_tags::PlcConnection;
use serde::Serialize;
use sqlx::postgres::{PgConnectOptions, PgSslMode};
use sqlx::{Connection, Error as SqlxError};

/// S1a の接続テストに使うタイムアウト（design §4.6「5秒」）。接続・
/// クエリの両方を合わせてこの時間内に完了しなければタイムアウト扱いにする
/// （`apps/banto-hub/core/src/rest.rs`の`PLC_TEST_TIMEOUT`が PLC 側で
/// 同じ役割を果たすのと同型 - あちらは 3 秒固定、DB は初回接続に TLS
/// ネゴシエーションが乗る分だけ長めにしてある）。
const DB_TEST_TIMEOUT: Duration = Duration::from_secs(5);

/// [`test_connection`]の結果。REST は`200 {"ok": true, "serverVersion":
/// "..."}`または`200 {"ok": false, "error": "..."}`としてそのまま返す
/// （design §4.6）。`ok: false`は疎通確認の失敗を表すだけで、REST 層は
/// これを異常系（4xx/5xx）ではなく通常応答として扱う -
/// `PlcConnectionTestResponse`（PLC 側の接続テスト）と同じ判断。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DbConnectionTestOutcome {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl DbConnectionTestOutcome {
    fn ok(server_version: String) -> Self {
        Self {
            ok: true,
            server_version: Some(server_version),
            error: None,
        }
    }

    fn failed(message: String) -> Self {
        Self {
            ok: false,
            server_version: None,
            error: Some(message),
        }
    }
}

/// `conn`（`protocol == "postgres"`である前提 - 呼び出し側が
/// [`banto_tags::PlcConnection::is_db_source`]で確認する）へ接続し、
/// `SELECT version()`を1回実行して切断する。接続・クエリを合わせて
/// [`DB_TEST_TIMEOUT`]（5秒）でタイムアウトする。
///
/// 呼び出し側の責務（このモジュールの doc comment参照）: `protocol`が
/// `"postgres"`かどうかの判定・認可（REST の`require_editor`/MCP の
/// `require_admin_scope`）はここでは行わない - 疎通確認そのものだけを担う。
pub async fn test_connection(conn: &PlcConnection) -> DbConnectionTestOutcome {
    let port = if conn.port < 1 || conn.port > 65535 {
        return DbConnectionTestOutcome::failed(
            "ポート番号が不正です(1〜65535の範囲で指定してください)。".to_string(),
        );
    } else {
        conn.port as u16
    };

    let options = PgConnectOptions::new()
        .host(&conn.host)
        .port(port)
        .database(conn.database.as_deref().unwrap_or(""))
        .username(conn.username.as_deref().unwrap_or(""))
        .password(conn.password.as_deref().unwrap_or(""))
        // design §4.2「sslmode prefer」: TLS が使えれば使う、使えなければ
        // 平文にフォールバックする - v1 は閉域 LAN 前提（§2.2）で、
        // PostgreSQL 側の証明書運用を必須にしない。
        .ssl_mode(PgSslMode::Prefer);

    match tokio::time::timeout(DB_TEST_TIMEOUT, run_test(options)).await {
        Ok(outcome) => outcome,
        Err(_elapsed) => DbConnectionTestOutcome::failed(format!(
            "接続タイムアウトです({}秒)。ホスト/ポート、ネットワーク到達性を確認してください。",
            DB_TEST_TIMEOUT.as_secs()
        )),
    }
}

async fn run_test(options: PgConnectOptions) -> DbConnectionTestOutcome {
    let mut pg_conn = match sqlx::postgres::PgConnection::connect_with(&options).await {
        Ok(pg_conn) => pg_conn,
        Err(err) => return DbConnectionTestOutcome::failed(sanitize_postgres_error(&err)),
    };

    let version: Result<(String,), SqlxError> = sqlx::query_as("SELECT version()")
        .fetch_one(&mut pg_conn)
        .await;
    // ベストエフォートで閉じる - 失敗しても結果には影響しない
    // (`test_modbus_connection`の`client.disconnect()`と同じ扱い)。
    let _ = pg_conn.close().await;

    match version {
        Ok((server_version,)) => DbConnectionTestOutcome::ok(server_version),
        Err(err) => DbConnectionTestOutcome::failed(sanitize_postgres_error(&err)),
    }
}

/// `sqlx::Error`を「短いカテゴリ + DB 自身のメッセージ」へ変換する。この
/// モジュールの doc comment「資格情報の扱い」節参照 -
/// パスワード・接続文字列全体を含めない。
fn sanitize_postgres_error(err: &SqlxError) -> String {
    match err {
        SqlxError::Database(db_err) => {
            format!("データベースエラー: {}", db_err.message())
        }
        SqlxError::Io(io_err) => {
            format!("接続エラー(ポートが閉じている、または到達できません): {io_err}")
        }
        SqlxError::Tls(tls_err) => format!("TLS接続に失敗しました: {tls_err}"),
        SqlxError::Configuration(_) => "接続設定が不正です。".to_string(),
        SqlxError::PoolTimedOut => "接続プールがタイムアウトしました。".to_string(),
        SqlxError::PoolClosed => "接続プールが閉じられています。".to_string(),
        // 他のバリアント（Protocol/RowNotFound 等）はこの用途では通常
        // 発生しない防御的ケース。`Display`実装自体が接続文字列や
        // パスワードを含まないことは sqlx のソース上保証されている
        // (これらは全て「サーバーから返った」情報かクライアント側の
        // 状態を記述するものであり、渡した認証情報を含まない)。
        other => format!("接続に失敗しました: {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_conn() -> PlcConnection {
        PlcConnection {
            id: 1,
            name: "Test DB".to_string(),
            protocol: "postgres".to_string(),
            host: "127.0.0.1".to_string(),
            port: 1,
            unit_id: 1,
            enabled: true,
            simulation: false,
            word_order: "low_high".to_string(),
            database: Some("appdb".to_string()),
            username: Some("appuser".to_string()),
            password: Some("s3cret-password".to_string()),
        }
    }

    /// A closed port fails fast (well within [`DB_TEST_TIMEOUT`]) with
    /// `ok: false`, and - the whole point of this test - the error text
    /// never contains the plaintext password.
    #[tokio::test]
    async fn test_connection_against_a_closed_port_fails_without_leaking_the_password() {
        let conn = base_conn();
        let outcome = test_connection(&conn).await;
        assert!(!outcome.ok);
        assert!(outcome.server_version.is_none());
        let error = outcome.error.expect("a failed test should carry an error");
        assert!(
            !error.contains("s3cret-password"),
            "error text must never contain the plaintext password: {error}"
        );
        assert!(
            !error.contains("appuser") || !error.contains("s3cret"),
            "sanity: this assertion just documents that usernames MAY appear \
             (the server's own message), passwords never do: {error}"
        );
    }

    /// An invalid port number is rejected before any connection attempt,
    /// with a friendly message (mirrors `test_modbus_connection`'s own
    /// port-range guard).
    #[tokio::test]
    async fn test_connection_rejects_an_out_of_range_port() {
        let mut conn = base_conn();

        // Test port > 65535
        conn.port = 70_000;
        let outcome = test_connection(&conn).await;
        assert!(!outcome.ok);
        assert!(outcome.error.expect("error").contains("1〜65535"));

        // Test port = 0
        conn.port = 0;
        let outcome = test_connection(&conn).await;
        assert!(!outcome.ok);
        assert!(outcome.error.expect("error").contains("1〜65535"));
    }

    /// Integration test against a real PostgreSQL - only runs when
    /// `BANTO_TEST_PG_URL` is set (e.g.
    /// `postgres://postgres:postgres@127.0.0.1:5432/postgres`), otherwise
    /// skips with an `eprintln!` (same convention the task instructions
    /// specify, matching this workspace's other real-machine-gated tests).
    #[tokio::test]
    async fn test_connection_against_a_real_postgresql_returns_the_server_version() {
        let Ok(url) = std::env::var("BANTO_TEST_PG_URL") else {
            eprintln!("skipped: BANTO_TEST_PG_URL unset");
            return;
        };
        let options: PgConnectOptions = url
            .parse()
            .expect("BANTO_TEST_PG_URL should be a valid postgres:// URL");
        let conn = PlcConnection {
            id: 1,
            name: "Real PG".to_string(),
            protocol: "postgres".to_string(),
            host: options.get_host().to_string(),
            port: options.get_port() as i64,
            unit_id: 1,
            enabled: true,
            simulation: false,
            word_order: "low_high".to_string(),
            database: options.get_database().map(|s| s.to_string()),
            username: Some(options.get_username().to_string()),
            password: url_password(&url),
        };

        let outcome = test_connection(&conn).await;
        assert!(outcome.ok, "expected ok, got: {outcome:?}");
        let server_version = outcome
            .server_version
            .expect("a successful test should carry the server version");
        eprintln!("BANTO_TEST_PG_URL SELECT version() -> {server_version}");
        assert!(
            server_version.to_uppercase().contains("POSTGRESQL"),
            "unexpected server_version: {server_version}"
        );
    }

    /// Tiny helper for the real-PostgreSQL test above:
    /// `sqlx::postgres::PgConnectOptions` deliberately does not expose the
    /// password back out (by design - it is a write-only builder field), so
    /// this test extracts it from the URL string directly instead.
    #[cfg(test)]
    fn url_password(url: &str) -> Option<String> {
        let after_scheme = url.split_once("://")?.1;
        let userinfo = after_scheme.split_once('@')?.0;
        let password = userinfo.split_once(':')?.1;
        Some(password.to_string())
    }
}
