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

use sqlx::postgres::{PgConnectOptions, PgSslMode};
use sqlx::Error as SqlxError;

/// PostgreSQL 接続オプションの唯一の組み立て点 - Hub の接続テスト（単発の
/// 疎通確認）・ポーリングタスクの `PgPool`・DB ブリッジが同じ設定で繋ぐこと
/// を保証する。特に `sslmode` の選択（design §4.2「sslmode prefer」: TLS が
/// 使えれば使う、使えなければ平文にフォールバック - v1 は閉域 LAN 前提
/// （§2.2）で、PostgreSQL 側の証明書運用を必須にしない）は片方だけ変わると
/// 挙動が食い違うので、ここ1箇所に置く。
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
