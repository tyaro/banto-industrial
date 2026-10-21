//! ポーリングの失敗の分類と時間の決まり（docs/banto-hub-external-db-design.md §4.2・
//! §4.3、docs/banto-hub-ingest-design.md §10.3）。
//!
//! 元は `banto-hub-core` の `db_source/task.rs` の `PgFailure`・`classify`・
//! `query_timeout`・`describe_retry_interval`。R2a-3b で DB ブリッジ
//! （`banto-hub-sink` の `source/`）も同じ判断を使うためにここへ移した。
//! **失敗の粒度（接続か文か）は梯子の入り口を決める**: 接続の失敗は全タグ Bad
//! （[`crate::GroupLadder::connection_lost`]）、文の失敗は 1 回目 Stale
//! （[`crate::GroupLadder::on_query_failure`]）。この分類が Hub 内の Source と
//! ブリッジでずれると、§10.4 の 5（クエリの失敗）と 6（DB の停止）の並びが
//! 食い違うので、梯子と同じ crate に置く。振る舞いは移す前と同じ。

use std::time::Duration;

use sqlx::Error as SqlxError;

use crate::pg::sanitize_postgres_error;

/// 1 クエリのタイムアウトの上限（§4.2「`min(period_ms * 0.8, 30s)`」）。
pub const MAX_QUERY_TIMEOUT: Duration = Duration::from_secs(30);

/// describe の再試行の最短間隔。周期が極端に短いグループ（100ms 等）で describe を
/// 毎ティック投げ続けないための下限。
pub const DESCRIBE_RETRY_MIN: Duration = Duration::from_secs(5);

/// 失敗の粒度（§4.3 の「グループ単位」と「接続単位」の分かれ目）。中の文言は
/// [`sanitize_postgres_error`] を通したもの（資格情報を含まない）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PgFailure {
    /// サーバーへ届いていない・セッションが壊れた - 接続単位で扱い、
    /// 配下の全タグを Bad にしてバックオフへ入る。
    Connection(String),
    /// サーバーは応答したが文が失敗した（構文エラー・権限・存在しない表）
    /// - グループ単位で扱い、他のグループには波及させない。
    Statement(String),
}

impl PgFailure {
    pub fn message(&self) -> &str {
        match self {
            PgFailure::Connection(message) | PgFailure::Statement(message) => message,
        }
    }
}

/// `sqlx::Error` を [`PgFailure`] へ分類する。**未知のバリアントは `Statement`
/// 側**に倒す - 誤って `Connection` に倒すと、実際には健全なプールを毎回捨てて
/// 再接続し続けることになり、他の（正常な）グループまで巻き込むため。
pub fn classify(err: &SqlxError) -> PgFailure {
    let message = sanitize_postgres_error(err);
    match err {
        SqlxError::Io(_)
        | SqlxError::Tls(_)
        | SqlxError::Protocol(_)
        | SqlxError::PoolTimedOut
        | SqlxError::PoolClosed
        | SqlxError::Configuration(_) => PgFailure::Connection(message),
        _ => PgFailure::Statement(message),
    }
}

/// 1 グループ（ブリッジでは 1 クエリ）のクエリのタイムアウト（§4.2）。
/// `period_ms * 0.8` を整数で（`* 4 / 5`）、[`MAX_QUERY_TIMEOUT`] で頭打ち。
/// 周期が極端に短い設定（100ms → 80ms）でも周期未満を守る - 遅い DB に対して
/// 短い周期を選ぶのは構成側の判断であり、ここで伸ばすと「1 本の遅いクエリが
/// 接続全体の周期を崩さない」（§4.2）を破る。
pub fn query_timeout(period_ms: u64) -> Duration {
    Duration::from_millis(period_ms.saturating_mul(4) / 5).min(MAX_QUERY_TIMEOUT)
}

/// 1 グループが再 describe を試みてよい最短間隔 - `max(period, 5s)`。
pub fn describe_retry_interval(period_ms: u64) -> Duration {
    Duration::from_millis(period_ms).max(DESCRIBE_RETRY_MIN)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_timeout_is_four_fifths_of_the_period_capped_at_30s() {
        assert_eq!(query_timeout(100), Duration::from_millis(80));
        assert_eq!(query_timeout(1_000), Duration::from_millis(800));
        assert_eq!(query_timeout(60_000), MAX_QUERY_TIMEOUT);
    }

    #[test]
    fn describe_retry_interval_is_at_least_five_seconds() {
        assert_eq!(describe_retry_interval(100), DESCRIBE_RETRY_MIN);
        assert_eq!(
            describe_retry_interval(10_000),
            Duration::from_millis(10_000)
        );
    }

    /// 接続側に倒すのは「サーバーへ届かない」ものだけ。それ以外（未知を含む）は文。
    #[test]
    fn classify_splits_connection_failures_from_statement_failures() {
        assert!(matches!(
            classify(&SqlxError::PoolTimedOut),
            PgFailure::Connection(_)
        ));
        assert!(matches!(
            classify(&SqlxError::PoolClosed),
            PgFailure::Connection(_)
        ));
        assert!(matches!(
            classify(&SqlxError::Io(std::io::Error::new(
                std::io::ErrorKind::ConnectionRefused,
                "refused"
            ))),
            PgFailure::Connection(_)
        ));
        assert!(matches!(
            classify(&SqlxError::RowNotFound),
            PgFailure::Statement(_)
        ));
        assert!(matches!(
            classify(&SqlxError::ColumnNotFound("x".into())),
            PgFailure::Statement(_)
        ));
    }
}
