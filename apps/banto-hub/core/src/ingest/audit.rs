//! 送信口の監査（R2a-2c、docs/banto-hub-ingest-design.md §8）。REST と R2a-2d の
//! WebSocket（`super::ws`）が同じ関数で記録する。
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
//! | 認証の失敗 | `denied` | 失効・期限切れ・トリップのキー（`resource: "api_keys"`）、`missing_ingest_scope`・`unknown_connection`（`resource: "ingest"`）。存在しないキーは記録しない。**キーごとに 1 時間に 100 行**、超えた分は数だけを 1 行（[`KeyDenialThrottle`]） |
//! | 未知のキー | `ingest_unknown_key` | (接続, キー) ごとに 1 時間に 1 回。1 時間に 100 キーを超えた分は数だけを 1 行（[`UnknownKeyThrottle`]） |
//! | 上限の超過 | `ingest_limit_exceeded` / `ingest_limit_cleared` | 超過の始まりと終わりの端だけ（`super::limits`）。**接続ごとに 1 時間に 20 組**、超えた分は組の数だけを 1 行（[`LimitEdgeThrottle`]） |
//! | WS の接続 | `ingest_connected`（`result: "ok"`） | WS を開いたとき（R2a-2d。REST は記録しない） |
//! | WS の切断 | `ingest_disconnected`（`result: "ok"`） | WS を閉じたとき。`reason` と受け付けた件数（R2a-2d） |
//!
//! ## 監査の行の間引き（2026-10-21 オーナー決定、§8）
//!
//! 3 つとも同じ型 [`WindowedAuditThrottle`] を使う: 1 時間の固定の窓（窓は最初の
//! 記録したい事象で始まる）で、窓ごとに N 行まで個別に記録し、超えた分は数だけを
//! 数える。窓が明けた後の**最初の観測**で、抑えた数を 1 行（`suppressed`）にする。
//! 観測は記録したい事象に限らない（キーの成功した要求・接続の通ったメッセージも
//! 観測）ので、悪い振る舞いが止んでも抑えた数は行方不明にならない（R2a-2c の
//! レビュー P2 の教訓）。抑えた行は 1 回だけ出し、窓なしに戻る。
//!
//! | 間引き | 単位 | N / 時間 | 観測の点 | 抑えた行 |
//! | --- | --- | --- | --- | --- |
//! | [`UnknownKeyThrottle`] | 接続（slot） | 100 キー | 値を反映したメッセージ（`IngestHub::throttle_unknown_keys`） | `ingest_unknown_key` `{suppressed, window_start_ms}` |
//! | [`KeyDenialThrottle`] | API キーの id（`IngestHub` に 1 つ） | 100 行 | 認証の層でキーを照合できた全要求と、層の後の `unknown_connection`（`IngestHub::throttle_key_denial`） | `denied`・`resource: "api_keys"`・`{reason: "suppressed", api_key_id, api_key_name, suppressed, window_start_ms}` |
//! | [`LimitEdgeThrottle`] | 接続（`ConnectionLimiter`） | 20 組 | 上限の判定を受けた全メッセージ（`IngestHub::admit`） | `ingest_limit_exceeded` `{suppressed, window_start_ms}`（`kind`・`limit` なし） |
//!
//! 監査は R2a-2d の WebSocket も同じ `IngestHub` の口を通すので、間引きも同じに掛かる。
//!
//! 記録は `tokio::spawn` で切り離し、ハンドラはその終わりを待つ
//! （implementation-checklist §5「handler の中で `await` をまたぐ副作用は接続の
//! 切断で捨てられる」- 送り手が応答を待たずに切っても監査の行は残る）。

use std::collections::{HashMap, HashSet};

use serde_json::{json, Map, Value};

use crate::audit::{AuditEntry, AuditLogService};

use super::limits::LimitEdge;
use super::{ApplyRefusal, IngestHub};

/// 監査の `origin`。
pub const ORIGIN: &str = "ingest";
/// 監査の `resource`（キーそのものの拒否以外）。
pub const RESOURCE: &str = "ingest";

/// 監査の間引きの窓（1 時間、[`WindowedAuditThrottle`]）。
pub const AUDIT_WINDOW_MS: i64 = 3_600_000;
/// 未知のキーの監査の窓（[`AUDIT_WINDOW_MS`] と同じ）。
pub const UNKNOWN_KEY_WINDOW_MS: i64 = AUDIT_WINDOW_MS;
/// 1 つの窓で個別に記録する未知のキーの数（超えた分は数だけ）。
pub const UNKNOWN_KEYS_PER_WINDOW: usize = 100;
/// 1 つの窓で個別に記録する、1 つの API キーの `denied` の行の数（超えた分は数だけ）。
pub const KEY_DENIALS_PER_WINDOW: u64 = 100;
/// [`KeyDenialThrottle`] が状態を持つキーの数の上限。照合できたキー（DB にある・
/// 管理者が発行したもの）だけを持つので本来は届かない。届いたら、持っていないキーの
/// 行は間引かずに記録する（監査を落とさない側に倒す。メモリは頭打ち）。
pub const MAX_TRACKED_KEYS: usize = 1_024;
/// 1 つの窓で個別に記録する、1 つの接続の上限の超過の組（`ingest_limit_exceeded` と
/// 対の `ingest_limit_cleared` で 1 組）の数（超えた分は組の数だけ）。
pub const LIMIT_EPISODES_PER_WINDOW: u64 = 20;
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

    /// 接続の項目だけの `detail`（キー・接続元・経路は載せない）。抑えた数の行用。
    /// 抑えた分は複数のキー・接続元にまたがり、窓明けの観測をした要求のものではない
    /// ので載せない。接続名の控えは接続の性質なので残す。
    fn connection_detail(&self, extra: Value) -> Value {
        let mut map = Map::new();
        map.insert("connection_id".into(), json!(self.connection_id));
        map.insert(
            "connection_name_snapshot".into(),
            json!(self.connection_name),
        );
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
    /// 監査の `result`（拒否・未知のキー・上限は `denied`、WS の接続・切断は `ok`）。
    pub result: &'static str,
}

impl IngestAuditRow {
    /// `missing_ingest_scope`・`unknown_connection` の拒否（§6 の表、`resource: "ingest"`）。
    pub fn denied(ctx: &IngestAuditContext, reason: &'static str) -> Self {
        Self {
            action: "denied",
            resource: RESOURCE,
            entity_id: ctx.connection_id.to_string(),
            detail: ctx.detail(json!({ "reason": reason })),
            result: "denied",
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
            result: "denied",
        }
    }

    /// 未知のキー（[`UnknownKeyThrottle`] が返した 1 つ）。
    pub fn unknown_key(ctx: &IngestAuditContext, record: &UnknownKeyRecord) -> Self {
        // 抑えた数の行は接続の項目だけ（`key_denials_suppressed` と同じ理由: 抑えた行は
        // 複数のキー・接続元にまたがり、窓明けの観測をした要求のものではないので載せない）。
        let detail = match record {
            UnknownKeyRecord::Key(key) => ctx.detail(json!({ "key": key })),
            UnknownKeyRecord::Suppressed {
                count,
                window_start_ms,
            } => ctx.connection_detail(
                json!({ "suppressed": count, "window_start_ms": window_start_ms }),
            ),
        };
        Self {
            action: "ingest_unknown_key",
            resource: RESOURCE,
            entity_id: ctx.connection_id.to_string(),
            detail,
            result: "denied",
        }
    }

    /// [`KeyDenialThrottle`] が抑えた `denied` の数（窓が明けた後に 1 行）。キーごとの
    /// 間引きで、抑えた行は複数の接続・理由にまたがるので `resource: "api_keys"`・
    /// `entity_id` はキーの id（失効・期限切れ・トリップの行と同じ）。経路・接続元は
    /// 載せない（抑えた行のものではなく、窓明けの観測をした要求のものになるため）。
    pub fn key_denials_suppressed(record: &KeyDenialSuppressed) -> Self {
        Self {
            action: "denied",
            resource: "api_keys",
            entity_id: record.api_key_id.to_string(),
            detail: json!({
                "reason": "suppressed",
                "api_key_id": record.api_key_id,
                "api_key_name": record.api_key_name,
                "suppressed": record.suppressed.count,
                "window_start_ms": record.suppressed.window_start_ms,
            }),
            result: "denied",
        }
    }

    /// 上限の端（`super::limits::LimitEdge`）。抑えた組の数（[`LimitEdge::Suppressed`]）は
    /// `ingest_limit_exceeded` に `suppressed`・`window_start_ms` を載せた行（`kind`・
    /// `limit` は無い。種類をまたいで数えるため）。
    pub fn limit_edge(ctx: &IngestAuditContext, edge: &LimitEdge) -> Self {
        // 抑えた数の行は接続の項目だけ（`key_denials_suppressed` と同じ理由: 抑えた組は
        // 複数のキー・接続元にまたがり、窓明けの観測をした要求のものではないので載せない）。
        let (action, detail) = match edge {
            LimitEdge::Exceeded { kind, limit } => (
                "ingest_limit_exceeded",
                ctx.detail(json!({ "kind": kind.as_str(), "limit": limit })),
            ),
            LimitEdge::Cleared { kind, rejected } => (
                "ingest_limit_cleared",
                ctx.detail(json!({ "kind": kind.as_str(), "rejected": rejected })),
            ),
            LimitEdge::Suppressed {
                episodes,
                window_start_ms,
            } => (
                "ingest_limit_exceeded",
                ctx.connection_detail(
                    json!({ "suppressed": episodes, "window_start_ms": window_start_ms }),
                ),
            ),
        };
        Self {
            action,
            resource: RESOURCE,
            entity_id: ctx.connection_id.to_string(),
            detail,
            result: "denied",
        }
    }

    /// WS を開いた（`ingest_connected`、R2a-2d）。REST は記録しない（要求ごとになる）。
    pub fn connected(ctx: &IngestAuditContext) -> Self {
        Self {
            action: "ingest_connected",
            resource: RESOURCE,
            entity_id: ctx.connection_id.to_string(),
            detail: ctx.detail(json!({})),
            result: "ok",
        }
    }

    /// WS を閉じた（`ingest_disconnected`、R2a-2d）。`reason` は §8 の綴り
    /// （`client_closed` / `superseded` / `revoked` / `connection_changed` /
    /// `connection_removed` / `backpressure` / `message_too_big`）、`revoked` のときは
    /// `revoked_reason`（`api_key_revoked` 等）。`accepted` はこの WS で受け付けた件数。
    pub fn disconnected(
        ctx: &IngestAuditContext,
        reason: &'static str,
        revoked_reason: Option<&'static str>,
        accepted: u64,
    ) -> Self {
        let mut extra = json!({ "reason": reason, "accepted": accepted });
        if let (Some(revoked), Value::Object(map)) = (revoked_reason, &mut extra) {
            map.insert("revoked_reason".into(), json!(revoked));
        }
        Self {
            action: "ingest_disconnected",
            resource: RESOURCE,
            entity_id: ctx.connection_id.to_string(),
            detail: ctx.detail(extra),
            result: "ok",
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
                    result: row.result,
                })
                .await;
        }
    });
    if let Err(err) = task.await {
        eprintln!("banto-hub: 取り込みの監査の記録が異常終了しました: {err}");
    }
}

/// 認証の層の後で `apply` / `admit` が接続を見失った（commit と競った）ときの監査
/// （§6 の表の `unknown_connection` と同じ行。名前の控えは無い）。キーごとの間引き
/// （[`IngestHub::throttle_key_denial`]）を通す。REST と WS（R2a-2d）が共有する。
/// `connection_disabled`・`stale_epoch` は記録しない（認証の層の 7 と同じ）。
pub fn refusal_rows(
    ingest: &IngestHub,
    ctx: &IngestAuditContext,
    refusal: ApplyRefusal,
    now_ms: i64,
) -> Vec<IngestAuditRow> {
    match refusal {
        ApplyRefusal::UnknownConnection => {
            let ctx = IngestAuditContext {
                connection_name: None,
                ..ctx.clone()
            };
            let row = IngestAuditRow::denied(&ctx, refusal.as_str());
            ingest.throttle_key_denial(ctx.api_key_id, &ctx.api_key_name, Some(row), now_ms)
        }
        ApplyRefusal::ConnectionDisabled | ApplyRefusal::StaleEpoch { .. } => Vec::new(),
    }
}

/// 窓で抑えた数（窓が明けた後の最初の観測で 1 行にする）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Suppressed {
    pub count: u64,
    /// 抑えた窓の始まり。
    pub window_start_ms: i64,
}

/// [`WindowedAuditThrottle::admit`] の結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThrottleVerdict {
    /// 前の窓で抑えた数（窓が明けていて、抑えたものがあったときだけ。先に記録する）。
    pub flushed: Option<Suppressed>,
    /// この事象を個別に記録するか（`false` なら数だけ数えた）。
    pub record: bool,
}

/// 監査の行の間引きの共通の型（モジュールの doc「監査の行の間引き」）。1 時間の固定の
/// 窓・窓ごとに `per_window` 行まで・超えた分は数だけ・窓が明けた後の最初の観測で
/// 抑えた数を 1 回だけ返す。持つのは数だけ（大きさは一定）。
///
/// - [`Self::admit`] は記録したい事象 1 つ。窓が無ければ始める。
/// - [`Self::flush_expired`] は記録したい事象でない観測（成功した要求など）。窓が
///   明けていれば閉じて「窓なし」に戻し、抑えた数を返す。新しい窓は始めない。
///
/// 時計が戻ったら（`now_ms` が窓の始まりより前）窓は明けていないとみなす。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowedAuditThrottle {
    per_window: u64,
    window_start_ms: Option<i64>,
    recorded: u64,
    suppressed: u64,
}

impl WindowedAuditThrottle {
    pub const fn new(per_window: u64) -> Self {
        Self {
            per_window,
            window_start_ms: None,
            recorded: 0,
            suppressed: 0,
        }
    }

    /// 窓が明けていれば閉じ（窓なしに戻す）、抑えた数があれば返す（1 回だけ）。
    pub fn flush_expired(&mut self, now_ms: i64) -> Option<Suppressed> {
        let start = self.window_start_ms?;
        if now_ms.saturating_sub(start) < AUDIT_WINDOW_MS {
            return None;
        }
        let count = std::mem::take(&mut self.suppressed);
        self.window_start_ms = None;
        self.recorded = 0;
        (count > 0).then_some(Suppressed {
            count,
            window_start_ms: start,
        })
    }

    /// 記録したい事象 1 つ: 明けた窓を閉じてから、窓の中で `per_window` 個目までなら
    /// 記録、超えたら数える。
    pub fn admit(&mut self, now_ms: i64) -> ThrottleVerdict {
        let flushed = self.flush_expired(now_ms);
        if self.window_start_ms.is_none() {
            self.window_start_ms = Some(now_ms);
        }
        let record = self.recorded < self.per_window;
        if record {
            self.recorded += 1;
        } else {
            self.suppressed += 1;
        }
        ThrottleVerdict { flushed, record }
    }

    /// 窓が無い（明けて閉じた・まだ始まっていない）。持っている数は 0 なので捨ててよい。
    pub fn is_idle(&self) -> bool {
        self.window_start_ms.is_none()
    }
}

/// [`KeyDenialThrottle`] が抑えた、1 つのキーの `denied` の数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyDenialSuppressed {
    pub api_key_id: i64,
    /// 最後に観測したときのキーの名前。
    pub api_key_name: String,
    pub suppressed: Suppressed,
}

/// [`KeyDenialThrottle::observe`] の結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyDenialVerdict {
    /// この拒否を個別に記録するか（拒否でない観測では `false`）。
    pub record: bool,
    /// 窓が明けて出す、抑えた数（このキーのものと、掃除で見つけた他のキーのもの）。
    /// 拒否の行より先に記録する。
    pub flushed: Vec<KeyDenialSuppressed>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct KeyDenialEntry {
    name: String,
    window: WindowedAuditThrottle,
}

/// 照合できた API キーの `denied` の行の間引き（2026-10-21 オーナー決定、§8）。キーの
/// id ごとに [`WindowedAuditThrottle`]（1 時間に [`KEY_DENIALS_PER_WINDOW`] 行）。
/// 対象は `missing_ingest_scope`・`unknown_connection`（認証の層と、層の後の競合）と、
/// 失効・期限切れ・トリップのキーの行。`IngestHub` に 1 つ置き、REST と WS が共有する。
///
/// メモリ: 持つのは照合できたキー（DB にあるもの）で、窓が動いているものだけ。
/// - 拒否でない観測（そのキーの成功した要求など）で、窓が明けていれば抑えた数を出して
///   そのキーの状態を捨てる。
/// - 1 時間に 1 回、どの観測でも全体を掃除する: 窓が明けたものは抑えた数を出して
///   捨てる（失効したキー・削除されたキーはもう成功しないので、ここで出す）。
/// - [`MAX_TRACKED_KEYS`] で頭打ち（届いたら持っていないキーの行は間引かずに記録）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeyDenialThrottle {
    entries: HashMap<i64, KeyDenialEntry>,
    last_sweep_ms: Option<i64>,
}

impl KeyDenialThrottle {
    /// キー `key_id` の観測。`denied` は記録したい `denied` の行があるか。
    pub fn observe(
        &mut self,
        key_id: i64,
        key_name: &str,
        denied: bool,
        now_ms: i64,
    ) -> KeyDenialVerdict {
        let mut flushed = Vec::new();
        self.sweep(now_ms, &mut flushed);
        let record = if denied {
            if self.entries.len() >= MAX_TRACKED_KEYS && !self.entries.contains_key(&key_id) {
                true
            } else {
                let entry = self
                    .entries
                    .entry(key_id)
                    .or_insert_with(|| KeyDenialEntry {
                        name: key_name.to_string(),
                        window: WindowedAuditThrottle::new(KEY_DENIALS_PER_WINDOW),
                    });
                if entry.name != key_name {
                    entry.name = key_name.to_string();
                }
                let verdict = entry.window.admit(now_ms);
                if let Some(suppressed) = verdict.flushed {
                    flushed.push(KeyDenialSuppressed {
                        api_key_id: key_id,
                        api_key_name: entry.name.clone(),
                        suppressed,
                    });
                }
                verdict.record
            }
        } else {
            if let Some(entry) = self.entries.get_mut(&key_id) {
                if let Some(suppressed) = entry.window.flush_expired(now_ms) {
                    flushed.push(KeyDenialSuppressed {
                        api_key_id: key_id,
                        api_key_name: key_name.to_string(),
                        suppressed,
                    });
                }
                if entry.window.is_idle() {
                    self.entries.remove(&key_id);
                }
            }
            false
        };
        KeyDenialVerdict { record, flushed }
    }

    /// 1 時間に 1 回、窓が明けたキーの抑えた数を出して捨てる。
    fn sweep(&mut self, now_ms: i64, flushed: &mut Vec<KeyDenialSuppressed>) {
        if self
            .last_sweep_ms
            .is_some_and(|last| now_ms.saturating_sub(last) < AUDIT_WINDOW_MS)
        {
            return;
        }
        self.last_sweep_ms = Some(now_ms);
        self.entries.retain(|&key_id, entry| {
            if let Some(suppressed) = entry.window.flush_expired(now_ms) {
                flushed.push(KeyDenialSuppressed {
                    api_key_id: key_id,
                    api_key_name: entry.name.clone(),
                    suppressed,
                });
            }
            !entry.window.is_idle()
        });
    }

    /// 状態を持っているキーの数（テスト用）。
    pub fn tracked(&self) -> usize {
        self.entries.len()
    }
}

/// 上限の超過の端の行の間引き（2026-10-21 オーナー決定、§8）。接続ごとに
/// [`WindowedAuditThrottle`]（1 時間に [`LIMIT_EPISODES_PER_WINDOW`] 組）。
/// `super::limits::ConnectionLimiter` が持ち、端の状態（超過中か・断った数）は
/// 今までどおり limiter が持つ - ここが間引くのは**行を書くこと**だけ。
///
/// - `Exceeded` を 1 組として数える。記録したら種類ごとに「書いた」を覚え、対の
///   `Cleared` は書いた `Exceeded` の後のものだけ記録する（数えない）。抑えた
///   `Exceeded` の対の `Cleared` は書かない（組ごと抑える）。
/// - 判定を受けた全メッセージが観測。窓が明けていれば [`LimitEdge::Suppressed`]（抑えた
///   組の数）を、そのメッセージの端より先に 1 回だけ返す。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LimitEdgeThrottle {
    window: WindowedAuditThrottle,
    /// 種類ごと（`LimitKind::index`）に、今の超過の `Exceeded` を書いたか。
    written: [bool; 3],
}

impl Default for LimitEdgeThrottle {
    fn default() -> Self {
        Self {
            window: WindowedAuditThrottle::new(LIMIT_EPISODES_PER_WINDOW),
            written: [false; 3],
        }
    }
}

impl LimitEdgeThrottle {
    /// 1 メッセージの端 `edges`（limiter の生の端）から、記録する端を返す。
    pub fn filter(&mut self, edges: Vec<LimitEdge>, now_ms: i64) -> Vec<LimitEdge> {
        let mut out = Vec::with_capacity(edges.len() + 1);
        if let Some(suppressed) = self.window.flush_expired(now_ms) {
            out.push(LimitEdge::Suppressed {
                episodes: suppressed.count,
                window_start_ms: suppressed.window_start_ms,
            });
        }
        for edge in edges {
            match edge {
                LimitEdge::Exceeded { kind, .. } => {
                    let record = self.window.admit(now_ms).record;
                    self.written[kind.index()] = record;
                    if record {
                        out.push(edge);
                    }
                }
                LimitEdge::Cleared { kind, .. } => {
                    if std::mem::take(&mut self.written[kind.index()]) {
                        out.push(edge);
                    }
                }
                LimitEdge::Suppressed { .. } => out.push(edge),
            }
        }
        out
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
///
/// 窓と数は [`WindowedAuditThrottle`]（「初めて見たキー」が記録したい事象）。ここが
/// 足すのは窓の中で記録したキーの集合（同じキーを 2 回数えない）だけ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownKeyThrottle {
    window: WindowedAuditThrottle,
    /// 今の窓で記録したキー（窓が閉じたら空にする）。
    seen: HashSet<String>,
}

impl Default for UnknownKeyThrottle {
    fn default() -> Self {
        Self {
            window: WindowedAuditThrottle::new(UNKNOWN_KEYS_PER_WINDOW as u64),
            seen: HashSet::new(),
        }
    }
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
        // 窓が明けていたら、未知のキーが来たかどうかによらず `Suppressed` を 1 回だけ出す
        // （送り手が未知のキーを送るのをやめても、抑えた数が行方不明にならない）。
        if let Some(suppressed) = self.window.flush_expired(now_ms) {
            out.push(UnknownKeyRecord::Suppressed {
                count: suppressed.count,
                window_start_ms: suppressed.window_start_ms,
            });
        }
        if self.window.is_idle() {
            self.seen.clear();
        }
        for key in keys {
            let key = truncate_chars(key, UNKNOWN_KEY_MAX_CHARS);
            if self.seen.contains(key) {
                continue;
            }
            if self.window.admit(now_ms).record {
                self.seen.insert(key.to_string());
                out.push(UnknownKeyRecord::Key(key.to_string()));
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

    /// 共通の窓: N 個は記録、N+1 個目から数だけ。窓の中の観測では出さず、窓が明けた後の
    /// 最初の観測（記録したい事象でなくても）で抑えた数を 1 回だけ。窓なしに戻る。
    #[test]
    fn windowed_throttle_caps_and_flushes_once_on_any_observation() {
        let mut throttle = WindowedAuditThrottle::new(3);
        assert!(throttle.is_idle());
        assert_eq!(throttle.flush_expired(0), None);
        for n in 0..3 {
            assert_eq!(
                throttle.admit(10 + n),
                ThrottleVerdict {
                    flushed: None,
                    record: true
                }
            );
        }
        for n in 0..2 {
            assert_eq!(
                throttle.admit(20 + n),
                ThrottleVerdict {
                    flushed: None,
                    record: false
                }
            );
        }
        // 窓の中の観測では出さない（窓は最初の事象の 10 から）。
        assert_eq!(throttle.flush_expired(10 + AUDIT_WINDOW_MS - 1), None);
        assert!(!throttle.is_idle());
        assert_eq!(
            throttle.flush_expired(10 + AUDIT_WINDOW_MS),
            Some(Suppressed {
                count: 2,
                window_start_ms: 10
            })
        );
        assert!(throttle.is_idle());
        // 二度は出さない。
        assert_eq!(throttle.flush_expired(10 + 2 * AUDIT_WINDOW_MS), None);
        // 新しい窓は次の事象で始まり、また N 個まで記録する。
        let start = 5 * AUDIT_WINDOW_MS;
        assert!(throttle.admit(start).record);
        // 時計が戻っても窓は明けない。
        assert_eq!(throttle.flush_expired(0), None);
        // 事象で窓が明けたとき: 抑えた数（ここでは 0）は出さず、新しい窓で記録。
        assert_eq!(
            throttle.admit(start + AUDIT_WINDOW_MS),
            ThrottleVerdict {
                flushed: None,
                record: true
            }
        );
        // 抑えた後に事象で窓が明けたとき: 抑えた数を先に、この事象は記録。
        let mut throttle = WindowedAuditThrottle::new(1);
        assert!(throttle.admit(0).record);
        assert!(!throttle.admit(1).record);
        assert_eq!(
            throttle.admit(AUDIT_WINDOW_MS),
            ThrottleVerdict {
                flushed: Some(Suppressed {
                    count: 1,
                    window_start_ms: 0
                }),
                record: true
            }
        );
    }

    /// キーごとの `denied`: 100 行は記録、101 行目から数だけ（別のキーは別に数える）。
    /// 窓が明けた後の拒否でない観測（成功した要求）で抑えた数を 1 回だけ出し、その
    /// キーの状態を捨てる。
    #[test]
    fn key_denials_are_capped_per_key_and_flushed_by_a_success() {
        let mut throttle = KeyDenialThrottle::default();
        // 最初の観測で掃除の時刻が 0 になる（次の掃除は 1 時間後）。
        assert!(throttle.observe(1, "idle", false, 0).flushed.is_empty());
        for n in 0..100 {
            let verdict = throttle.observe(7, "bridge", true, 10 + n);
            assert!(verdict.record && verdict.flushed.is_empty(), "{n}");
        }
        for n in 0..5 {
            assert_eq!(
                throttle.observe(7, "bridge", true, 200 + n),
                KeyDenialVerdict {
                    record: false,
                    flushed: Vec::new()
                }
            );
        }
        // 別のキーは別の窓。
        assert!(throttle.observe(8, "other", true, 300).record);
        assert_eq!(throttle.tracked(), 2);
        // 窓の中の成功では何も出さず、状態も捨てない（ここで掃除も走るが、どの窓も
        // まだ明けていない）。
        assert!(throttle
            .observe(7, "bridge", false, AUDIT_WINDOW_MS)
            .flushed
            .is_empty());
        assert_eq!(throttle.tracked(), 2);
        // 窓が明けた後の成功（拒否でない観測）で 1 回だけ。名前は今のもの。
        let verdict = throttle.observe(7, "bridge2", false, 10 + AUDIT_WINDOW_MS);
        assert!(!verdict.record);
        assert_eq!(
            verdict.flushed,
            vec![KeyDenialSuppressed {
                api_key_id: 7,
                api_key_name: "bridge2".into(),
                suppressed: Suppressed {
                    count: 5,
                    window_start_ms: 10
                }
            }]
        );
        assert_eq!(throttle.tracked(), 1);
        assert!(throttle
            .observe(7, "bridge", false, 11 + AUDIT_WINDOW_MS)
            .flushed
            .is_empty());
        // 抑えたものの無いキー（8）は、窓が明けた後の観測で捨てる（何も出さない）。
        assert!(throttle
            .observe(8, "other", false, 300 + AUDIT_WINDOW_MS)
            .flushed
            .is_empty());
        assert_eq!(throttle.tracked(), 0);
    }

    /// 失効したキーはもう成功しないので、窓が明けた後の他のキーの観測の掃除で抑えた
    /// 数を出して捨てる（1 時間に 1 回の掃除）。
    #[test]
    fn key_denials_of_a_silent_key_are_flushed_by_the_sweep() {
        let mut throttle = KeyDenialThrottle::default();
        for n in 0..101 {
            throttle.observe(3, "revoked", true, n);
        }
        // 掃除は最初の観測（0）で済んだので、次は 1 時間後から。
        assert!(throttle
            .observe(9, "ok", false, AUDIT_WINDOW_MS - 1)
            .flushed
            .is_empty());
        let verdict = throttle.observe(9, "ok", false, AUDIT_WINDOW_MS);
        assert_eq!(
            verdict.flushed,
            vec![KeyDenialSuppressed {
                api_key_id: 3,
                api_key_name: "revoked".into(),
                suppressed: Suppressed {
                    count: 1,
                    window_start_ms: 0
                }
            }]
        );
        assert_eq!(throttle.tracked(), 0);
        assert!(throttle
            .observe(3, "revoked", false, 3 * AUDIT_WINDOW_MS)
            .flushed
            .is_empty());
    }

    /// 状態を持つキーの数は [`MAX_TRACKED_KEYS`] で頭打ち。届いたら持っていないキーの
    /// 行は間引かずに記録し（監査を落とさない）、状態は増やさない。
    #[test]
    fn key_denial_state_is_bounded() {
        let mut throttle = KeyDenialThrottle::default();
        for id in 0..MAX_TRACKED_KEYS as i64 {
            assert!(throttle.observe(id, "k", true, 0).record);
        }
        assert_eq!(throttle.tracked(), MAX_TRACKED_KEYS);
        let overflow = MAX_TRACKED_KEYS as i64 + 1;
        for _ in 0..150 {
            assert!(throttle.observe(overflow, "k", true, 1).record);
        }
        assert_eq!(throttle.tracked(), MAX_TRACKED_KEYS);
        // 窓が明けると掃除で全部捨てる（抑えたものは無い）。
        assert!(throttle
            .observe(overflow, "k", false, AUDIT_WINDOW_MS)
            .flushed
            .is_empty());
        assert_eq!(throttle.tracked(), 0);
    }

    fn exceeded(kind: LimitKind) -> LimitEdge {
        LimitEdge::Exceeded { kind, limit: 1 }
    }

    fn cleared(kind: LimitKind) -> LimitEdge {
        LimitEdge::Cleared { kind, rejected: 1 }
    }

    /// 上限の端: 20 組は記録し、21 組目の `Exceeded` と対の `Cleared` は書かない（組ごと
    /// 抑える）。窓が明けた後の、端の無いメッセージで抑えた組の数を 1 回だけ。窓を
    /// またいだ組の `Cleared` は、`Exceeded` を書いたなら書く。
    #[test]
    fn limit_edges_are_capped_per_pair_and_flushed_once() {
        let mut throttle = LimitEdgeThrottle::default();
        let kind = LimitKind::MessageRate;
        for n in 0..20 {
            assert_eq!(
                throttle.filter(vec![exceeded(kind)], n),
                vec![exceeded(kind)],
                "{n}"
            );
            assert_eq!(
                throttle.filter(vec![cleared(kind)], n),
                vec![cleared(kind)],
                "{n}"
            );
        }
        for n in 0..3 {
            assert!(throttle.filter(vec![exceeded(kind)], 100 + n).is_empty());
            assert!(throttle.filter(vec![cleared(kind)], 100 + n).is_empty());
        }
        // 抑えた組の後の観測（端なし）では、窓の中なら何も出ない。
        assert!(throttle.filter(Vec::new(), AUDIT_WINDOW_MS - 1).is_empty());
        assert_eq!(
            throttle.filter(Vec::new(), AUDIT_WINDOW_MS),
            vec![LimitEdge::Suppressed {
                episodes: 3,
                window_start_ms: 0
            }]
        );
        assert!(throttle.filter(Vec::new(), AUDIT_WINDOW_MS + 1).is_empty());

        // 窓をまたぐ組: 窓の終わりに書いた `Exceeded` の `Cleared` は次の窓でも書く
        // （数えない）。
        let mut throttle = LimitEdgeThrottle::default();
        for n in 0..19 {
            throttle.filter(vec![exceeded(kind)], n);
            throttle.filter(vec![cleared(kind)], n);
        }
        let other = LimitKind::TooManyItems;
        assert_eq!(
            throttle.filter(vec![exceeded(other)], 50),
            vec![exceeded(other)]
        );
        // 20 組を使い切ったので、別の種類の超過は抑える。
        assert!(throttle.filter(vec![exceeded(kind)], 60).is_empty());
        assert_eq!(
            throttle.filter(vec![cleared(other), cleared(kind)], AUDIT_WINDOW_MS),
            vec![
                LimitEdge::Suppressed {
                    episodes: 1,
                    window_start_ms: 0
                },
                cleared(other)
            ]
        );
    }

    /// 抑えた数の行の形（値は載らない）。
    #[test]
    fn suppressed_rows_have_documented_shapes() {
        let key_row = IngestAuditRow::key_denials_suppressed(&KeyDenialSuppressed {
            api_key_id: 7,
            api_key_name: "bridge".into(),
            suppressed: Suppressed {
                count: 5,
                window_start_ms: 1_000,
            },
        });
        assert_eq!(key_row.action, "denied");
        assert_eq!(key_row.resource, "api_keys");
        assert_eq!(key_row.entity_id, "7");
        assert_eq!(
            key_row.detail,
            json!({ "reason": "suppressed", "api_key_id": 7, "api_key_name": "bridge",
                    "suppressed": 5, "window_start_ms": 1_000 })
        );
        let edge_row = IngestAuditRow::limit_edge(
            &ctx(),
            &LimitEdge::Suppressed {
                episodes: 3,
                window_start_ms: 2_000,
            },
        );
        assert_eq!(edge_row.action, "ingest_limit_exceeded");
        assert_eq!(edge_row.resource, "ingest");
        assert_eq!(edge_row.detail["suppressed"], 3);
        assert_eq!(edge_row.detail["window_start_ms"], 2_000);
        assert!(edge_row.detail.get("kind").is_none());
        assert_eq!(edge_row.detail["connection_name_snapshot"], "Bridge1");
    }

    /// 抑えた数の行（上限の端・未知のキー）には、観測した要求のキー・接続元・経路が
    /// 載らない（接続の項目と `suppressed`・`window_start_ms` だけ）。通常の行は載る。
    #[test]
    fn suppressed_rows_omit_observer_key_and_peer() {
        let rows = [
            IngestAuditRow::limit_edge(
                &ctx(),
                &LimitEdge::Suppressed {
                    episodes: 3,
                    window_start_ms: 2_000,
                },
            ),
            IngestAuditRow::unknown_key(
                &ctx(),
                &UnknownKeyRecord::Suppressed {
                    count: 4,
                    window_start_ms: 3_000,
                },
            ),
        ];
        for row in &rows {
            let detail = row.detail.as_object().unwrap();
            for gone in ["api_key_id", "api_key_name", "peer", "transport"] {
                assert!(!detail.contains_key(gone), "{gone}: {detail:?}");
            }
            for kept in [
                "connection_id",
                "connection_name_snapshot",
                "suppressed",
                "window_start_ms",
            ] {
                assert!(detail.contains_key(kept), "{kept}: {detail:?}");
            }
            assert_eq!(detail["connection_id"], 12);
        }

        let normal = [
            IngestAuditRow::unknown_key(&ctx(), &UnknownKeyRecord::Key("k".into())),
            IngestAuditRow::limit_edge(
                &ctx(),
                &LimitEdge::Exceeded {
                    kind: LimitKind::ItemRate,
                    limit: 1,
                },
            ),
        ];
        for row in &normal {
            let detail = row.detail.as_object().unwrap();
            for kept in ["api_key_id", "api_key_name", "peer", "transport"] {
                assert!(detail.contains_key(kept), "{kept}: {detail:?}");
            }
        }
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
        for row in &rows {
            assert_eq!(row.result, "denied");
        }
    }

    /// WS の接続・切断（R2a-2d）は `result: "ok"`。切断は理由と受け付けた件数だけで、
    /// 値は載らない。`revoked` のときだけ理由文が付く。
    #[test]
    fn ws_session_rows_are_ok_and_carry_reason_and_count() {
        let ctx = IngestAuditContext {
            transport: Transport::Ws,
            ..ctx()
        };
        let connected = IngestAuditRow::connected(&ctx);
        assert_eq!(
            (connected.action, connected.result, connected.resource),
            ("ingest_connected", "ok", "ingest")
        );
        assert_eq!(connected.detail["transport"], "ws");
        let closed = IngestAuditRow::disconnected(&ctx, "superseded", None, 42);
        assert_eq!(
            (closed.action, closed.result),
            ("ingest_disconnected", "ok")
        );
        assert_eq!(closed.detail["reason"], "superseded");
        assert_eq!(closed.detail["accepted"], 42);
        assert!(closed.detail.get("revoked_reason").is_none());
        let revoked = IngestAuditRow::disconnected(&ctx, "revoked", Some("api_key_revoked"), 0);
        assert_eq!(revoked.detail["revoked_reason"], "api_key_revoked");
        assert_eq!(revoked.entity_id, "12");
    }
}
