//! グループ単位の運転状態と、Hub へ push する前のエラー文言の無害化
//! （設計 §5.2・§5.5）。
//!
//! ## `lastError` に何を載せるか
//!
//! 2 段構えで秘密を落とす:
//!
//! 1. [`sanitize_db_error`]: `sqlx::Error` を「短いカテゴリ + DB 自身の
//!    メッセージ」へ縮める。`banto_hub_core::db_source::
//!    sanitize_postgres_error`（S1a/S2）と**同じ分類・同じ文言**で、
//!    Hub の状態画面に DB Source と Sink のエラーが並んだときに読み方が
//!    変わらないようにしてある。PostgreSQL 自身がエラーメッセージに
//!    クライアントの送ったパスワードを含めることはない。
//! 2. [`Redactor`]: それでも念のため、既知の秘密文字列（API キー・全
//!    接続のパスワード）を機械的に伏せる。1 の分類漏れ（将来の sqlx の
//!    新しいバリアントなど）に対する多層防御で、通常は何もしない。
//!
//! さらに [`Redactor::apply`] は長さも切り詰める - `lastError` は Hub の
//! メモリに載って状態 API に出るだけの表示用文字列で、長大なスタック
//! ダンプを運ぶ場所ではない。

use sqlx::Error as SqlxError;

use crate::hub_api::SinkGroupStatusPush;

/// `lastError` の最大長（表示用。UTF-8 の途中で切らない）。
const MAX_ERROR_LEN: usize = 500;

/// グループの運転状態（Hub 側 `banto_hub_core::sink::
/// ALLOWED_SINK_GROUP_STATES` と同じ 4 値）。
///
/// `Disabled` は**このサイドカーからは push されない** -
/// `GET /api/sink/config` が有効なグループしか返さない（Hub 側
/// `SinkGroupService::list_enabled`）ため、無効化されたグループは
/// そもそも実行時状態を持たず、次の push の全量スナップショットから
/// 消えることで Hub 側の表示からも落ちる。値としては Hub の語彙に
/// 合わせて定義だけしてある。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GroupState {
    /// 正常（テーブル検査が通り、INSERT が失敗していない）。
    Running,
    /// DB へ接続できず/書けずバックオフ中。行はキューに残っている。
    Backoff,
    /// テーブル検査に失敗、設定が不正、参照する接続が無い等。
    /// 30 秒ごとに再検査する。
    Error,
    /// （このサイドカーは push しない - 上記参照）
    Disabled,
}

impl GroupState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Backoff => "backoff",
            Self::Error => "error",
            Self::Disabled => "disabled",
        }
    }
}

/// 秘密文字列の伏せ字化（このモジュールの doc comment参照）。
#[derive(Clone, Debug, Default)]
pub struct Redactor {
    secrets: Vec<String>,
}

impl Redactor {
    pub fn new() -> Self {
        Self::default()
    }

    /// 伏せる対象を足す。短すぎる文字列（3 文字以下）は無視する -
    /// 例えばパスワードが `"a"` だと本文が伏せ字だらけになり、かえって
    /// 障害の切り分けができなくなるため。
    pub fn add_secret(&mut self, secret: &str) {
        let secret = secret.trim();
        if secret.len() > 3 && !self.secrets.iter().any(|known| known == secret) {
            self.secrets.push(secret.to_string());
        }
    }

    /// 伏せ字化 + 長さの切り詰め。
    pub fn apply(&self, text: &str) -> String {
        let mut out = text.to_string();
        for secret in &self.secrets {
            if out.contains(secret.as_str()) {
                out = out.replace(secret.as_str(), "<redacted>");
            }
        }
        truncate_chars(&out, MAX_ERROR_LEN)
    }
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max_chars).collect();
    out.push('…');
    out
}

/// `sqlx::Error` を表示用の短い文言へ（`banto_hub_core::db_source::
/// sanitize_postgres_error` と同じ分類）。
pub fn sanitize_db_error(err: &SqlxError) -> String {
    match err {
        SqlxError::Database(db_err) => format!("データベースエラー: {}", db_err.message()),
        SqlxError::Io(io_err) => {
            format!("接続エラー(ポートが閉じている、または到達できません): {io_err}")
        }
        SqlxError::Tls(tls_err) => format!("TLS接続に失敗しました: {tls_err}"),
        SqlxError::Configuration(_) => "接続設定が不正です。".to_string(),
        SqlxError::PoolTimedOut => "接続プールがタイムアウトしました。".to_string(),
        SqlxError::PoolClosed => "接続プールが閉じられています。".to_string(),
        other => format!("接続に失敗しました: {other}"),
    }
}

/// テーブルが存在しないことによる失敗か（設計 §5.3: このときだけ推奨
/// DDL を 1 回 `warn` で案内する）。PostgreSQL の SQLSTATE `42P01`
/// (`undefined_table`) で判定する - 文言のパースはロケールで壊れるため
/// 使わない（H9 で SLMP のエラー文言パースを構造化マッチへ置き換えた
/// のと同じ判断）。
pub fn is_undefined_table(err: &SqlxError) -> bool {
    match err {
        SqlxError::Database(db_err) => db_err.code().as_deref() == Some("42P01"),
        _ => false,
    }
}

/// 失敗の影響範囲（設計 §5.3「合わなければそのグループを `error` にして
/// 止める（他グループには影響しない）」と §5.4「DB 停止・INSERT 失敗は
/// 指数バックオフ」の分かれ目）。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorScope {
    /// SQL 文レベルの失敗（テーブル/列が無い、型が合わない、権限が無い）。
    /// そのグループだけを `error` にし、接続のバックオフには入らない。
    Group,
    /// 接続レベルの失敗（DB 停止・ネットワーク断・プール枯渇）。
    /// その接続の全グループが `backoff` になる。
    Connection,
}

pub fn classify_scope(err: &SqlxError) -> ErrorScope {
    match err {
        SqlxError::Database(_) => ErrorScope::Group,
        SqlxError::Io(_)
        | SqlxError::Tls(_)
        | SqlxError::PoolTimedOut
        | SqlxError::PoolClosed
        | SqlxError::Configuration(_) => ErrorScope::Connection,
        // `Protocol`/`WorkerCrashed` 等は接続が壊れた側に寄せる（再接続
        // で直る見込みがある方へ倒す）。
        _ => ErrorScope::Connection,
    }
}

/// 状態 push 1 グループ分の組み立て（[`crate::hub_api`] の wire 型へ）。
pub fn push_entry(
    id: i64,
    state: GroupState,
    queued: usize,
    dropped: u64,
    last_flush_at: Option<i64>,
    last_error: Option<String>,
) -> SinkGroupStatusPush {
    SinkGroupStatusPush {
        id,
        state: state.as_str(),
        queued: queued as u64,
        dropped,
        last_flush_at,
        last_error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_states_use_the_hub_vocabulary() {
        assert_eq!(GroupState::Running.as_str(), "running");
        assert_eq!(GroupState::Backoff.as_str(), "backoff");
        assert_eq!(GroupState::Error.as_str(), "error");
        assert_eq!(GroupState::Disabled.as_str(), "disabled");
    }

    #[test]
    fn redactor_replaces_every_known_secret() {
        let mut redactor = Redactor::new();
        redactor.add_secret("s3cret-password");
        redactor.add_secret("bh_abcdef.ghijkl");
        let text = "接続に失敗しました: password=s3cret-password key=bh_abcdef.ghijkl";
        let out = redactor.apply(text);
        assert!(!out.contains("s3cret-password"), "{out}");
        assert!(!out.contains("bh_abcdef.ghijkl"), "{out}");
        assert_eq!(out.matches("<redacted>").count(), 2, "{out}");
    }

    #[test]
    fn redactor_ignores_secrets_too_short_to_be_useful() {
        let mut redactor = Redactor::new();
        redactor.add_secret("ab");
        redactor.add_secret("");
        assert_eq!(redactor.apply("abcabc"), "abcabc");
    }

    #[test]
    fn redactor_truncates_long_messages_on_a_char_boundary() {
        let redactor = Redactor::new();
        let long = "あ".repeat(1_000);
        let out = redactor.apply(&long);
        assert_eq!(out.chars().count(), MAX_ERROR_LEN + 1);
        assert!(out.ends_with('…'));
    }

    /// 状態 push の本文に秘密が載らないこと（実装指示のユニットテスト）。
    #[test]
    fn a_status_push_body_never_carries_a_secret() {
        let mut redactor = Redactor::new();
        redactor.add_secret("s3cret-password");
        redactor.add_secret("bh_sidecar.key");
        let raw = "データベースエラー: role \"app\" password s3cret-password rejected (key bh_sidecar.key)";
        let entry = push_entry(
            7,
            GroupState::Error,
            12,
            3,
            Some(1_700_000_000_000),
            Some(redactor.apply(raw)),
        );
        let body = crate::hub_api::SinkStatusPush {
            groups: vec![entry],
        };
        let json = serde_json::to_string(&body).expect("serialize");
        assert!(!json.contains("s3cret-password"), "{json}");
        assert!(!json.contains("bh_sidecar.key"), "{json}");
        assert!(json.contains("\"state\":\"error\""), "{json}");
        assert!(json.contains("\"queued\":12"), "{json}");
        assert!(json.contains("\"dropped\":3"), "{json}");
    }

    #[test]
    fn sanitize_maps_the_transport_variants_without_the_connection_string() {
        let err = SqlxError::PoolTimedOut;
        assert_eq!(
            sanitize_db_error(&err),
            "接続プールがタイムアウトしました。"
        );
        assert_eq!(classify_scope(&err), ErrorScope::Connection);
        assert!(!is_undefined_table(&err));

        let err = SqlxError::Io(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            "refused",
        ));
        assert!(sanitize_db_error(&err).starts_with("接続エラー"));
        assert_eq!(classify_scope(&err), ErrorScope::Connection);
    }
}
