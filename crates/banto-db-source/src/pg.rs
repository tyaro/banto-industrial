//! PostgreSQL への接続オプションの唯一の組み立て点と、エラーの文言の整形
//! （docs/banto-hub-external-db-design.md §4.2・§4.6・§2.2）。
//!
//! 元は `banto-hub-core` の `db_source/mod.rs`。Hub の接続テスト・ポーリング
//! タスクと DB ブリッジ（R2a-3b）が同じ設定で繋ぎ、同じ文言で失敗を返す
//! ための移設で、振る舞いは変えていない。
//!
//! ## 資格情報の扱い
//!
//! [`sanitize_postgres_error`] の戻り値には、**平文パスワードも接続文字列
//! 全体も絶対に含めない**（design §4.6・§2.2）。`sqlx::Error` をそのまま
//! `{err}` で文字列化せず、カテゴリ + DB 自身のエラーメッセージ（パスワードを
//! 含まないことが保証されている - PostgreSQL 自体がエラーメッセージに
//! クライアントの送信したパスワードを含めることはない）だけを取り出す。

use std::time::Duration;

use sqlx::postgres::{PgConnectOptions, PgSslMode};
use sqlx::Error as SqlxError;

/// 接続ごとにサーバー側へ課す `statement_timeout`（[`pg_connect_options`] が
/// 起動パケットの `options` で渡す）。クライアント側の上限
/// （[`crate::poll::MAX_QUERY_TIMEOUT`] = `query_timeout` の頭打ち、sink の
/// `DESCRIBE_TIMEOUT`）は `tokio::time::timeout` で future を捨てるだけで、
/// サーバー側の実行は止めない。時間切れ・停止時のプールの破棄（PR #765、
/// ingest-design §10.3 の実装の注 (11)）のあとも、サーバーは切断に気づくまで
/// 文を走らせ続ける（`pg_sleep` や長い集計の最中は
/// `client_connection_check_interval` が既定の 0 で気づかない）。
/// その「捨てたあとの走りっぱなし」にサーバー側の上限を付けるための値。
///
/// **クライアント側の上限と同じ 30 秒**にしてある（短くしない）。external-db
/// §4.2 の実効上限 `min(周期 × 0.8, 30 秒)` を変えないため。クライアントの
/// タイマーは送信の前に始まるので、通常は先にクライアントが切れる。サーバーが
/// 先に切れたときは SQLSTATE `57014`（`query_canceled`）の
/// `sqlx::Error::Database` で、[`crate::poll::classify`] が
/// [`crate::poll::PgFailure::Statement`] にする = クライアントの時間切れと
/// 梯子の同じ段。
pub const SERVER_STATEMENT_TIMEOUT: Duration = crate::poll::MAX_QUERY_TIMEOUT;

/// PostgreSQL 接続オプションの唯一の組み立て点 - Hub の接続テスト（単発の
/// 疎通確認）・ポーリングタスクの `PgPool`・DB ブリッジが同じ設定で繋ぐこと
/// を保証する。特に `sslmode` の選択（design §4.2「sslmode prefer」: TLS が
/// 使えれば使う、使えなければ平文にフォールバック - v1 は閉域 LAN 前提
/// （§2.2）で、PostgreSQL 側の証明書運用を必須にしない）は片方だけ変わると
/// 挙動が食い違うので、ここ1箇所に置く。
///
/// ## サーバー側の `statement_timeout`
///
/// 起動パケットの `options`（`-c statement_timeout=30000ms`）で
/// [`SERVER_STATEMENT_TIMEOUT`] を課す。理由は定数の doc comment。
///
/// - 簡易・拡張どちらのプロトコルにも効くので、`describe`（prepare = Parse /
///   Describe）にも効く。
/// - ロック待ちの時間も `statement_timeout` に数えられる。こちらの文は読み取り
///   専用の SELECT で `ACCESS SHARE` しか取らず、`lock_timeout` を足しても
///   `statement_timeout` より短くしない限り意味がないので、`lock_timeout` は
///   設けない。
/// - 接続の確立・認証には効かない（そちらはクライアント側の
///   `acquire_timeout` / 接続の上限が受け持つ）。
/// - 既知のトレードオフ: 起動時の設定は `ALTER ROLE` / `ALTER DATABASE` の
///   設定より優先されるので、DBA がロールに設定した、より厳しい値も 30 秒に
///   上書きされる。また PgBouncer など一部のプーラーは、
///   `ignore_startup_parameters = options` が無いと `options` を起動パラメータ
///   として拒否する。その設定で接続が通っても、プーラーは `options` を捨てる
///   ので**サーバー側の 30 秒の上限は掛からない**（接続を通すことと、上限が
///   バックエンドに掛かることは別）。プーラー経由では `SHOW statement_timeout`
///   で確かめる。
///
/// `application_name` は呼び出し側が決める（DBA が `pg_stat_activity` で
/// 「どのプロセスのどの接続か」を見分けるため。Hub は
/// `banto-hub-db-source <接続名>`、ブリッジは別の接頭辞を使う）。
pub fn pg_connect_options(
    host: &str,
    port: u16,
    database: &str,
    username: &str,
    password: &str,
    application_name: &str,
) -> PgConnectOptions {
    PgConnectOptions::new()
        .host(host)
        .port(port)
        .database(database)
        .username(username)
        .password(password)
        .application_name(application_name)
        .ssl_mode(PgSslMode::Prefer)
        .options([(
            "statement_timeout",
            format!("{}ms", SERVER_STATEMENT_TIMEOUT.as_millis()),
        )])
}

/// `sqlx::Error`を「短いカテゴリ + DB 自身のメッセージ」へ変換する。この
/// モジュールの doc comment「資格情報の扱い」節参照 -
/// パスワード・接続文字列全体を含めない。
pub fn sanitize_postgres_error(err: &SqlxError) -> String {
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

    /// 起動パケットの `options` に `statement_timeout`（30 秒 = ミリ秒で）が
    /// 載る（実 PostgreSQL なしで見られる部分。サーバーが受け取ることは
    /// `tests/statement_timeout.rs`）。
    #[test]
    fn connect_options_carry_the_server_statement_timeout() {
        let options = pg_connect_options("localhost", 5432, "db", "user", "pw", "app");
        assert_eq!(SERVER_STATEMENT_TIMEOUT, Duration::from_secs(30));
        let carried = options
            .get_options()
            .expect("options should be set for the startup packet");
        assert!(
            carried.contains("statement_timeout=30000ms"),
            "unexpected options: {carried}"
        );
    }
}
