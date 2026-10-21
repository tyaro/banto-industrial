//! 送信口の監査（R2a-2c、docs/banto-hub-ingest-design.md §8）。REST と R2a-2d の
//! WebSocket が同じ関数で記録する。
//!
//! `AuditLogService::record`（DB が応答しないときは保留ファイルへ退避、
//! `crate::audit_spool`）を使う。`origin` は `"ingest"`、`resource` は `"ingest"`
//! （失効・期限切れ・トリップのキーの `denied` だけは今の `/api/v1/*` と同じ
//! `"api_keys"`）、`entity_id` は接続 id（パスの id。削除済みでも記録する）。
//!
//! **値そのものは載せない**（§8・§11 の監査の見どころ）。`detail` に載るのは
//! キーの id と名前・接続 id と（分かれば）その時点の名前の控え・接続元・経路
//! （`rest` / `ws`）と、事象ごとの項目（理由・未知のキーの名前・上限の種類と値・
//! 断った数）だけ。本文・`v`・`q`・`t` は載せない。
//!
//! | 事象 | `action` | いつ・どれだけ |
//! | --- | --- | --- |
//! | 認証の失敗 | `denied` | 失効・期限切れ・トリップのキー（`resource: "api_keys"`）、`missing_ingest_scope`・`unknown_connection`（`resource: "ingest"`）。存在しないキーは記録しない |
//! | 未知のキー | `ingest_unknown_key` | (接続, キー) ごとに 1 時間に 1 回。1 時間に 100 キーを超えた分は数だけを 1 行（[`UnknownKeyThrottle`]） |
//! | 上限の超過 | `ingest_limit_exceeded` / `ingest_limit_cleared` | 超過の始まりと終わりの端だけ（`super::limits`） |
//!
//! 記録は `tokio::spawn` で切り離し、ハンドラはその終わりを待つ
//! （implementation-checklist §5「handler の中で `await` をまたぐ副作用は接続の
//! 切断で捨てられる」- 送り手が応答を待たずに切っても監査の行は残る）。

use std::collections::HashSet;

use serde_json::{json, Map, Value};

use crate::audit::{AuditEntry, AuditLogService};

use super::limits::LimitEdge;

/// 監査の `origin`。
pub const ORIGIN: &str = "ingest";
/// 監査の `resource`（キーそのものの拒否以外）。
pub const RESOURCE: &str = "ingest";

/// 未知のキーの監査の窓（1 時間）。
pub const UNKNOWN_KEY_WINDOW_MS: i64 = 3_600_000;
/// 1 つの窓で個別に記録する未知のキーの数（超えた分は数だけ）。
pub const UNKNOWN_KEYS_PER_WINDOW: usize = 100;
/// 監査に載せる未知のキーの長さ（文字数）。登録できるキーの上限と同じ 256 文字で
/// 切る（送り手が 1 MiB のキーを送っても監査の行とメモリは膨らまない）。
pub const UNKNOWN_KEY_MAX_CHARS: usize = 256;

/// 送信の経路（監査の `transport`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Rest,
    Ws,
}

impl Transport {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rest => "rest",
            Self::Ws => "ws",
        }
    }
}

/// 誰が・どの接続へ・どこから（監査の `detail` の共通の項目）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestAuditContext {
    pub api_key_id: i64,
    pub api_key_name: String,
    /// パスの id（正規形を通ったもの）。
    pub connection_id: i64,
    /// その時点の接続名の控え（改名・削除の後も読めるように）。接続が無い
    /// （削除済み）・まだ明かしてよい段でない（スコープの確認の前）なら `None`。
    pub connection_name: Option<String>,
    /// 接続元（`IP:port`）。分からなければ `None`。
    pub peer: Option<String>,
    pub transport: Transport,
}

impl IngestAuditContext {
    fn detail(&self, extra: Value) -> Value {
        let mut map = Map::new();
        map.insert("api_key_id".into(), json!(self.api_key_id));
        map.insert("api_key_name".into(), json!(self.api_key_name));
        map.insert("connection_id".into(), json!(self.connection_id));
        map.insert(
            "connection_name_snapshot".into(),
            json!(self.connection_name),
        );
        map.insert("peer".into(), json!(self.peer));
        map.insert("transport".into(), json!(self.transport.as_str()));
        if let Value::Object(extra) = extra {
            map.extend(extra);
        }
        Value::Object(map)
    }
}

/// 記録する 1 行（所有する形。`tokio::spawn` へ渡すため）。
#[derive(Debug, Clone, PartialEq)]
pub struct IngestAuditRow {
    pub action: &'static str,
    pub resource: &'static str,
    pub entity_id: String,
    pub detail: Value,
}

impl IngestAuditRow {
    /// `missing_ingest_scope`・`unknown_connection` の拒否（§6 の表、`resource: "ingest"`）。
    pub fn denied(ctx: &IngestAuditContext, reason: &'static str) -> Self {
        Self {
            action: "denied",
            resource: RESOURCE,
            entity_id: ctx.connection_id.to_string(),
            detail: ctx.detail(json!({ "reason": reason })),
        }
    }

    /// 失効・期限切れ・トリップのキーの拒否（`resource: "api_keys"`、`entity_id` は
    /// キーの id。今の `/api/v1/*` の行と同じ項目に経路と接続元を足す）。
    pub fn key_denied(
        key_id: i64,
        key_name: &str,
        reason: &str,
        path: &str,
        peer: Option<&str>,
        transport: Transport,
    ) -> Self {
        Self {
            action: "denied",
            resource: "api_keys",
            entity_id: key_id.to_string(),
            detail: json!({
                "reason": reason,
                "name": key_name,
                "method": match transport { Transport::Rest => "POST", Transport::Ws => "GET" },
                "path": path,
                "peer": peer,
                "transport": transport.as_str(),
            }),
        }
    }

    /// 未知のキー（[`UnknownKeyThrottle`] が返した 1 つ）。
    pub fn unknown_key(ctx: &IngestAuditContext, record: &UnknownKeyRecord) -> Self {
        let extra = match record {
            UnknownKeyRecord::Key(key) => json!({ "key": key }),
            UnknownKeyRecord::Suppressed {
                count,
                window_start_ms,
            } => json!({ "suppressed": count, "window_start_ms": window_start_ms }),
        };
        Self {
            action: "ingest_unknown_key",
            resource: RESOURCE,
            entity_id: ctx.connection_id.to_string(),
            detail: ctx.detail(extra),
        }
    }

    /// 上限の端（`super::limits::LimitEdge`）。
    pub fn limit_edge(ctx: &IngestAuditContext, edge: &LimitEdge) -> Self {
        let (action, extra) = match edge {
            LimitEdge::Exceeded { kind, limit } => (
                "ingest_limit_exceeded",
                json!({ "kind": kind.as_str(), "limit": limit }),
            ),
            LimitEdge::Cleared { kind, rejected } => (
                "ingest_limit_cleared",
                json!({ "kind": kind.as_str(), "rejected": rejected }),
            ),
        };
        Self {
            action,
            resource: RESOURCE,
            entity_id: ctx.connection_id.to_string(),
            detail: ctx.detail(extra),
        }
    }
}

/// 行を記録する。`tokio::spawn` で切り離して、その終わりを待つ（モジュールの doc）。
/// 記録の失敗は `AuditLogService` が保留・ログに回すので、ここでは返さない。
pub async fn record(audit: &AuditLogService, rows: Vec<IngestAuditRow>) {
    if rows.is_empty() {
        return;
    }
    let audit = audit.clone();
    let task = tokio::spawn(async move {
        for row in rows {
            audit
                .record(AuditEntry {
                    actor_username: None,
                    actor_role: None,
                    action: row.action,
                    resource: row.resource,
                    entity_id: Some(&row.entity_id),
                    detail: Some(row.detail),
                    origin: ORIGIN,
                    result: "denied",
                })
                .await;
        }
    });
    if let Err(err) = task.await {
        eprintln!("banto-hub: 取り込みの監査の記録が異常終了しました: {err}");
    }
}

/// [`UnknownKeyThrottle::observe`] が返す、記録すべき 1 つ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnknownKeyRecord {
    /// そのキーを記録する（[`UNKNOWN_KEY_MAX_CHARS`] で切った名前）。
    Key(String),
    /// 前の窓で個別に記録しなかった未知のキーの数（窓が明けた後の最初の観測で 1 行）。
    Suppressed { count: u64, window_start_ms: i64 },
}

/// 未知のキーの監査の間引き（§8、接続ごと）。1 時間の固定の窓で、窓の中で初めて見た
/// キーを 100 個まで記録し、それを超えた分（同じ窓で既に記録したキーは数えない）は
/// 数だけを数え、窓が明けた後の最初の観測で 1 行にする（その観測が未知のキーを
/// 含まない場合も同じ。正常な送信に戻った後の最初の要求でも 1 行出る）。持つのは窓の中の 100 個の
/// キー（各 256 文字まで）だけなので、送り手が未知のキーを何種類送ってもメモリと
/// 監査の行は頭打ちになる（1 時間に高々 101 行、§11 のオーナーの監査 (3)）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UnknownKeyThrottle {
    window_start_ms: Option<i64>,
    seen: HashSet<String>,
    suppressed: u64,
}

impl UnknownKeyThrottle {
    /// 1 メッセージで断った未知のキーを観測し、記録すべきものを返す。`keys` が空でも
    /// 窓が明けていれば抑えた数の `Suppressed` を 1 回だけ返し、窓なしに戻す。
    pub fn observe<'a>(
        &mut self,
        keys: impl IntoIterator<Item = &'a str>,
        now_ms: i64,
    ) -> Vec<UnknownKeyRecord> {
        let mut out = Vec::new();
        let mut keys = keys.into_iter().peekable();
        let no_keys = keys.peek().is_none();
        let expired = self
            .window_start_ms
            .is_none_or(|start| now_ms.saturating_sub(start) >= UNKNOWN_KEY_WINDOW_MS);
        if expired {
            // 窓が明けていたら、未知のキーが来たかどうかによらず `Suppressed` を 1 回だけ出す
            // （送り手が未知のキーを送るのをやめても、抑えた数が行方不明にならない）。
            if let (Some(start), true) = (self.window_start_ms, self.suppressed > 0) {
                out.push(UnknownKeyRecord::Suppressed {
                    count: self.suppressed,
                    window_start_ms: start,
                });
            }
            if no_keys {
                // 未知のキーの無い観測では新しい窓を始めず「窓なし」に戻す。
                self.window_start_ms = None;
                self.seen.clear();
                self.suppressed = 0;
                return out;
            }
            self.window_start_ms = Some(now_ms);
            self.seen.clear();
            self.suppressed = 0;
        }
        if no_keys {
            return out;
        }
        for key in keys {
            let key = truncate_chars(key, UNKNOWN_KEY_MAX_CHARS);
            if self.seen.contains(key) {
                continue;
            }
            if self.seen.len() < UNKNOWN_KEYS_PER_WINDOW {
                self.seen.insert(key.to_string());
                out.push(UnknownKeyRecord::Key(key.to_string()));
            } else {
                self.suppressed += 1;
            }
        }
        out
    }
}

fn truncate_chars(text: &str, max: usize) -> &str {
    match text.char_indices().nth(max) {
        Some((end, _)) => &text[..end],
        None => text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::limits::LimitKind;

    fn ctx() -> IngestAuditContext {
        IngestAuditContext {
            api_key_id: 7,
            api_key_name: "bridge".into(),
            connection_id: 12,
            connection_name: Some("Bridge1".into()),
            peer: Some("127.0.0.1:5000".into()),
            transport: Transport::Rest,
        }
    }

    /// (接続, キー) ごとに窓の中で 1 回。100 キーを超えた分は数だけを数え、窓が明けた
    /// 後の最初の観測で `Suppressed` を 1 行。同じ窓で記録済みのキーは数えない。
    #[test]
    fn unknown_keys_are_recorded_once_per_window_and_capped() {
        let mut throttle = UnknownKeyThrottle::default();
        assert!(throttle.observe(std::iter::empty(), 0).is_empty());
        assert_eq!(
            throttle.observe(["a", "a", "b"], 0),
            vec![
                UnknownKeyRecord::Key("a".into()),
                UnknownKeyRecord::Key("b".into())
            ]
        );
        assert!(throttle.observe(["a", "b"], 1_000).is_empty());
        let many: Vec<String> = (0..200).map(|n| format!("k{n}")).collect();
        let records = throttle.observe(many.iter().map(String::as_str), 2_000);
        assert_eq!(records.len(), 98);
        // 既に記録した 2 つ（a・b）と 98 個で 100 個。残り 102 個は数だけ。
        assert!(throttle.observe(["a", "k0"], 3_000).is_empty());
        assert!(throttle
            .observe(["k199"], UNKNOWN_KEY_WINDOW_MS - 1)
            .is_empty());
        assert_eq!(
            throttle.observe(["a"], UNKNOWN_KEY_WINDOW_MS),
            vec![
                UnknownKeyRecord::Suppressed {
                    count: 103,
                    window_start_ms: 0
                },
                UnknownKeyRecord::Key("a".into())
            ]
        );
        // 新しい窓で抑えたものが無ければ `Suppressed` は出ない。
        assert_eq!(
            throttle.observe(["z"], 3 * UNKNOWN_KEY_WINDOW_MS),
            vec![UnknownKeyRecord::Key("z".into())]
        );
    }

    /// 回帰: 送り手が未知のキーを送るのをやめても（空の観測だけが続いても）、窓が明けた
    /// 最初の観測で `Suppressed` が 1 回だけ出る。
    #[test]
    fn suppressed_is_flushed_by_empty_observation_after_window() {
        let mut throttle = UnknownKeyThrottle::default();
        let many: Vec<String> = (0..101).map(|n| format!("k{n}")).collect();
        let records = throttle.observe(many.iter().map(String::as_str), 0);
        assert_eq!(records.len(), 100);
        assert!(records
            .iter()
            .all(|record| matches!(record, UnknownKeyRecord::Key(_))));
        assert_eq!(
            throttle.observe(std::iter::empty(), UNKNOWN_KEY_WINDOW_MS),
            vec![UnknownKeyRecord::Suppressed {
                count: 1,
                window_start_ms: 0
            }]
        );
        assert!(throttle
            .observe(std::iter::empty(), 2 * UNKNOWN_KEY_WINDOW_MS)
            .is_empty());
    }

    /// 長いキーは 256 文字で切る（文字の境界で）。切った後の名前で重複を数える。
    #[test]
    fn long_unknown_keys_are_truncated() {
        let mut throttle = UnknownKeyThrottle::default();
        let long = "あ".repeat(300);
        let other = format!("{}x", "あ".repeat(256));
        assert_eq!(
            throttle.observe([long.as_str(), other.as_str()], 0),
            vec![UnknownKeyRecord::Key("あ".repeat(256))]
        );
    }

    /// 監査の行に値（`v`・`q`・`t`）が載らない。共通の項目と事象ごとの項目だけ。
    #[test]
    fn rows_carry_identities_but_no_values() {
        let rows = [
            IngestAuditRow::denied(&ctx(), "missing_ingest_scope"),
            IngestAuditRow::unknown_key(&ctx(), &UnknownKeyRecord::Key("line1.tmp".into())),
            IngestAuditRow::limit_edge(
                &ctx(),
                &LimitEdge::Exceeded {
                    kind: LimitKind::ItemRate,
                    limit: 5_000,
                },
            ),
            IngestAuditRow::limit_edge(
                &ctx(),
                &LimitEdge::Cleared {
                    kind: LimitKind::MessageRate,
                    rejected: 3,
                },
            ),
        ];
        let allowed = [
            "api_key_id",
            "api_key_name",
            "connection_id",
            "connection_name_snapshot",
            "peer",
            "transport",
            "reason",
            "key",
            "kind",
            "limit",
            "rejected",
        ];
        for row in &rows {
            assert_eq!(row.entity_id, "12");
            assert_eq!(row.resource, "ingest");
            let detail = row.detail.as_object().unwrap();
            for key in detail.keys() {
                assert!(allowed.contains(&key.as_str()), "{key} in {row:?}");
            }
            assert_eq!(detail["connection_name_snapshot"], "Bridge1");
            assert_eq!(detail["transport"], "rest");
        }
        assert_eq!(rows[0].action, "denied");
        assert_eq!(rows[1].action, "ingest_unknown_key");
        assert_eq!(rows[2].action, "ingest_limit_exceeded");
        assert_eq!(rows[2].detail["kind"], "items_per_sec");
        assert_eq!(rows[3].action, "ingest_limit_cleared");
        assert_eq!(rows[3].detail["rejected"], 3);
    }
}
