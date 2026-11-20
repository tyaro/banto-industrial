//! 下のログのパネルの取得の口（`GET /api/log-feed`、#1253）。
//!
//! 設計は docs/hub/banto-hub-ui-improvement-proposal.md §下のログのパネル（2026-11-21
//! オーナー決定）。3 種の記録を 1 つの口で混ぜて返す:
//!
//! - **接続**（`source: "connection"`）: `collect_events` の `plc_connected`・
//!   `plc_disconnected`・`plc_reconnected`（#344 の意味）と、受け入れの接続の切断
//!   （`audit_log` の `origin = 'ingest' AND action = 'ingest_disconnected'`）。
//! - **書き込み**（`source: "write"`）: `hub_write_audit` の行（暫定の `failed` から
//!   確定した行まで）。
//! - **操作・認証の監査**（`source: "operation"`）: `audit_log` の `origin` が
//!   `mcp`・`rest`・`grpc` の行（ログイン・ログアウト・拒否も含む。絞らない）。
//!
//! ## 見せる範囲（サーバー側で絞る）
//!
//! 画面のセッションだけで使う（[`super::require_session`]。API キー `bh_` は照合で
//! 落ちて 401）。ロールの最低は付けず、ハンドラの中で identity のロールを見る:
//!
//! - 管理者: 3 種と、その詳細（監査の生の `detail` を含む）。
//! - それ以外（viewer・editor など管理者未満すべて）: 接続だけ。受け入れの切断は
//!   時刻・接続名・切断の区分（`transport`・`reason`）だけを返し、API キーの名前・ID・
//!   接続元（`peer`）・監査の生の `detail` は返さない。書き込みと操作の監査は、行も
//!   件数も先頭の位置も返さない（下の「続きの位置」）。
//! - 管理者未満には `audit_log.id` を出さない（2026-11-21 オーナーのレビュー、#1267）。
//!   `audit_log` は操作の監査と受け入れの切断で番号を分け合うので、番号の飛びから、間に
//!   管理者向けの操作の監査が何件あるかがわかる。受け入れの切断の行は数字の `id` を
//!   項目ごと出さず、`key` は `i:` にプロセスの鍵での HMAC-SHA256 を切り詰めた
//!   base64url（22 文字）を付けたもの（行は変わらないので、画面の重複の取り除きに
//!   使えれば足りる。Hub を起動し直すと同じ行でも別の `key` になる）。管理者には
//!   `id`・`a:<id>` を出す。`collect_events` の `id`・`c:<id>` は誰にでも出す（接続の
//!   記録は `/api/v1/events` でも読める番号で、監査の表とは別の番号）。
//!
//! ## 続きの位置と表示の順を分ける
//!
//! 表示は発生の時刻（`ts_ms`）の順だが、続きの位置は記録ごとの「変わった順」で持つ。
//! 発生の時刻を位置にすると、後から入る行（`audit_log` は保留（`crate::audit_spool`）
//! から元の時刻のまま後で入る）や後から変わる行（`hub_write_audit` は暫定の `failed` を
//! 同じ `id` で確定させる）を取りこぼすため。
//!
//! - `collect_events.id`・`audit_log.id`（挿入の順。受け入れの切断と操作の監査は同じ
//!   表なので 1 つの位置）・`hub_write_audit.change_seq`（0112 のトリガが挿入と
//!   `result`・`detail` の更新のたびに進める）。
//! - 読む前に、同じ読み取りのトランザクションの中で各記録の先頭（最大の位置）を
//!   決め、その先頭までを読む。先頭の後に入った行は次の回に取る（読み取りと先頭の間に
//!   入った行を飛ばさない）。上限に当たらなければ位置は先頭まで進める（見せない行を
//!   次の回に読み直さない）。
//! - 管理者未満の `a` は受け入れの切断の行だけの先頭（部分索引
//!   `idx_audit_log_ingest_disconnected`）で、`w` は 0 のまま（書き込みの表を読まない）。
//!
//! ## カーソルは封をした不透明な文字列
//!
//! カーソルの中身（3 つの位置と見せる範囲）は、プロセスの起動時に OS の乱数で作った
//! 鍵（[`FeedKeys`]）の ChaCha20-Poly1305 で封をし、`nonce ‖ 暗号文 ‖ tag` を
//! base64url（パディング無し、72 文字）にして返す。位置は `audit_log.id` そのもの
//! なので、管理者未満に読める形で返すと上の「番号を出さない」が崩れる。管理者にも
//! 同じ封をする（形から範囲がわからないように）。
//!
//! - 形が違う（base64url でない・長さが違う）: 400（`kind: "bad_request"`）。
//! - 封が開かない（Hub の再起動で鍵が替わった・改ざん。両者は区別できない）、または
//!   封の中の範囲が今の利用者の範囲と違う（途中でロールが変わった）: 409
//!   （`kind: "conflict"`、`current.code: "cursor_expired"`）。画面は `cursor` 無しの
//!   初回から読み直す。
//!
//! ## 初回と続き
//!
//! `cursor` 無しの初回は、各記録の先頭までの行から**発生の時刻の降順**（同じ時刻は
//! `id` の降順）で最大 `limit` 件ずつを取り、混ぜて `limit` 件に切る。続きの位置は
//! 各記録の先頭（切り捨てた古い行は返さない）。位置の降順で選ぶと、時刻の古い行が
//! 後から確定・後から挿入されて新しい位置を占めたとき、時刻の新しい行が初回の候補
//! から落ち、カーソルは先頭まで進むので次の回にも出ない（2026-11-21 オーナーの
//! レビュー、#1267）。`audit_log.ts`・`hub_write_audit.ts` の TEXT は書き込み側が
//! すべて `YYYY-MM-DD HH:MM:SS`（UTC）で書く（列の既定の `datetime('now')`、banto の
//! 保留の流し込みの `iso_datetime_from_system_time`、拒否の行の
//! `datetime(?, 'unixepoch')`）ので、文字列の順が時刻の順になり、`ts` の索引
//! （`idx_audit_log_ts`・`idx_hub_write_audit_ts`・`idx_collect_events_ts`）で
//! `ORDER BY ts DESC` を読める。
//!
//! `cursor` 付きは、各記録で位置がカーソルより大きい行を位置の昇順で最大 `limit` 件
//! ずつ取り、どれかで上限に当たったら `more: true`（その記録の位置は返した最後の行
//! まで）。どちらも `entries` は発生の時刻の降順に並べて返す。画面は受け取った行を
//! 時刻の位置に差し込み、書き込みの行は同じ `key` の行を置き換える。

use std::str::FromStr;
use std::sync::Arc;

use base64::Engine as _;
use ring::rand::{SecureRandom, SystemRandom};
use ring::{aead, hmac};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{Sqlite, SqlitePool, Transaction};
use utoipa::ToSchema;

use super::{
    get, middleware, require_session, ApiError, AuthGate, AuthState, AuthenticatedSession,
    BantoError, Extension, Json, OperationKind, Query, Role, Router, State,
};

/// 1 回の応答で、各記録から取る件数の既定。
const DEFAULT_LIMIT: i64 = 200;
/// 同じく上限（1 未満・上限超えはクランプする）。
const MAX_LIMIT: i64 = 500;

/// 接続の記録として出す `collect_events.kind`（#344 の意味。切断の始まりと、最初の
/// 良好な読み取りでの復帰）。
const CONNECTION_KINDS_SQL: &str = "('plc_connected', 'plc_disconnected', 'plc_reconnected')";

/// `audit_log` のうち受け入れの切断の行。0112 の部分索引の条件と同じ綴りにする
/// （SQLite は WHERE がこの条件を含むときだけ部分索引を使う）。
const INGEST_DISCONNECTED_SQL: &str = "origin = 'ingest' AND action = 'ingest_disconnected'";

/// `audit_log` のうち操作・認証の監査の行。
const OPERATION_ORIGINS_SQL: &str = "origin IN ('mcp', 'rest', 'grpc')";

/// `audit_log.ts`・`hub_write_audit.ts`（TEXT、`datetime('now')` の
/// `YYYY-MM-DD HH:MM:SS` で UTC）を epoch ms にする式。`julianday` は UTC として読み、
/// 小数の秒や `T`・`Z` の形も読む。読めない値は 0（行は落とさない）。
fn text_ts_ms(column: &str) -> String {
    format!("COALESCE(CAST(ROUND((julianday({column}) - 2440587.5) * 86400000.0) AS INTEGER), 0)")
}

#[derive(Clone)]
pub(super) struct LogFeedState {
    pool: SqlitePool,
    keys: Arc<FeedKeys>,
}

/// カーソルの封の付加データ（形を変えたら版を上げる。古い版の封は開かない）。
const CURSOR_AAD: &[u8] = b"banto-hub log-feed cursor v1";
/// 封の中身の版（中身の並びを変えたら上げる）。
const CURSOR_VERSION: u8 = 1;
/// 封の中身の長さ（版 1 バイト + 範囲 1 バイト + 位置 3 つ × 8 バイト。封をすると 54 バイトで、
/// base64url の 72 文字にちょうど収まる）。
const CURSOR_PLAIN_LEN: usize = 2 + 8 * 3;
/// 封をしたカーソルのバイト長（nonce ‖ 暗号文 ‖ tag）。
const CURSOR_SEALED_LEN: usize = aead::NONCE_LEN + CURSOR_PLAIN_LEN + 16;
/// 管理者未満の受け入れの切断の行の `key` に使う HMAC の長さ（バイト。base64url で 22 文字）。
const ROW_KEY_MAC_LEN: usize = 16;

/// プロセスごとの鍵（モジュール doc「カーソルは封をした不透明な文字列」）。DB にも
/// ファイルにも残さない（Hub を起動し直すと前のカーソルは開かず、画面は読み直す）。
pub(super) struct FeedKeys {
    cursor: aead::LessSafeKey,
    row: hmac::Key,
    rng: SystemRandom,
}

/// カーソルを開けなかった理由。
#[derive(Debug, PartialEq, Eq)]
enum CursorError {
    /// 形が違う（400）。
    Malformed,
    /// 封が開かない・範囲が違う（409、画面は初回から読み直す）。
    Expired,
}

impl FeedKeys {
    /// OS の乱数で鍵を作る。乱数を取れない OS では Hub の安全の前提（API キー・
    /// セッションの生成）も崩れているので、起動を止める。
    fn generate() -> Self {
        let rng = SystemRandom::new();
        let mut bytes = [0u8; 32];
        rng.fill(&mut bytes)
            .expect("システム乱数生成器の呼び出しに失敗しました");
        let cursor = aead::LessSafeKey::new(
            aead::UnboundKey::new(&aead::CHACHA20_POLY1305, &bytes)
                .expect("ChaCha20-Poly1305 の鍵は 32 バイト"),
        );
        let row = hmac::Key::generate(hmac::HMAC_SHA256, &rng)
            .expect("システム乱数生成器の呼び出しに失敗しました");
        FeedKeys { cursor, row, rng }
    }

    fn seal(&self, scope: FeedScope, cursor: FeedCursor) -> String {
        let mut nonce = [0u8; aead::NONCE_LEN];
        self.rng
            .fill(&mut nonce)
            .expect("システム乱数生成器の呼び出しに失敗しました");
        let mut buf = Vec::with_capacity(CURSOR_SEALED_LEN);
        buf.extend_from_slice(&nonce);
        buf.push(CURSOR_VERSION);
        buf.push(scope.code());
        for position in [cursor.collect, cursor.audit, cursor.write] {
            buf.extend_from_slice(&position.to_be_bytes());
        }
        let mut body = buf.split_off(aead::NONCE_LEN);
        self.cursor
            .seal_in_place_append_tag(
                aead::Nonce::assume_unique_for_key(nonce),
                aead::Aad::from(CURSOR_AAD),
                &mut body,
            )
            .expect("封の長さは決まっている");
        buf.extend_from_slice(&body);
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf)
    }

    fn open(&self, text: &str, scope: FeedScope) -> Result<FeedCursor, CursorError> {
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(text)
            .map_err(|_| CursorError::Malformed)?;
        if bytes.len() != CURSOR_SEALED_LEN {
            return Err(CursorError::Malformed);
        }
        let (nonce, body) = bytes.split_at(aead::NONCE_LEN);
        let nonce =
            aead::Nonce::try_assume_unique_for_key(nonce).map_err(|_| CursorError::Malformed)?;
        let mut body = body.to_vec();
        let plain = self
            .cursor
            .open_in_place(nonce, aead::Aad::from(CURSOR_AAD), &mut body)
            .map_err(|_| CursorError::Expired)?;
        if plain.len() != CURSOR_PLAIN_LEN || plain[0] != CURSOR_VERSION || plain[1] != scope.code()
        {
            return Err(CursorError::Expired);
        }
        let position = |n: usize| {
            let start = 2 + 8 * n;
            i64::from_be_bytes(plain[start..start + 8].try_into().expect("8 バイト"))
        };
        Ok(FeedCursor {
            collect: position(0),
            audit: position(1),
            write: position(2),
        })
    }

    /// 管理者未満に返す受け入れの切断の行の `key`（`i:` + HMAC の切り詰め）。
    fn ingest_row_key(&self, audit_id: i64) -> String {
        let mut data = b"audit_log:".to_vec();
        data.extend_from_slice(&audit_id.to_be_bytes());
        let tag = hmac::sign(&self.row, &data);
        format!(
            "i:{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(&tag.as_ref()[..ROW_KEY_MAC_LEN])
        )
    }
}

/// `GET /api/log-feed` の問い合わせ。
#[derive(Debug, Deserialize)]
pub(super) struct LogFeedQuery {
    cursor: Option<String>,
    limit: Option<i64>,
}

/// `GET /api/log-feed` の応答。
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct LogFeedResponse {
    /// 発生の時刻（`ts_ms`）の降順。画面は時刻の位置に差し込む（後から入る行は、前に
    /// 受け取った行より古い時刻のことがある）。
    entries: Vec<LogFeedEntry>,
    /// 次の回にそのまま渡す続きの位置（不透明な文字列）。
    cursor: String,
    /// どれかの記録で上限に当たった（続きをすぐ読めば、まだ行がある）。
    more: bool,
}

/// 1 行分。`source` で種類を分ける。
#[derive(Debug, Serialize, ToSchema)]
#[serde(tag = "source", rename_all = "snake_case")]
pub(super) enum LogFeedEntry {
    Connection(ConnectionLogEntry),
    Write(WriteLogEntry),
    Operation(OperationLogEntry),
}

impl LogFeedEntry {
    fn ts_ms(&self) -> i64 {
        match self {
            LogFeedEntry::Connection(entry) => entry.ts_ms,
            LogFeedEntry::Write(entry) => entry.ts_ms,
            LogFeedEntry::Operation(entry) => entry.ts_ms,
        }
    }

    fn key(&self) -> &str {
        match self {
            LogFeedEntry::Connection(entry) => &entry.key,
            LogFeedEntry::Write(entry) => &entry.key,
            LogFeedEntry::Operation(entry) => &entry.key,
        }
    }
}

/// 接続の行（`collect_events` の PLC の接続・切断・復帰と、受け入れの切断）。
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct ConnectionLogEntry {
    /// 元の表の `id`（`collect_events.id` か `audit_log.id`。表が違うと重なりうる）。
    /// 管理者未満への受け入れの切断の行には項目ごと無い（`audit_log.id` を出さない）。
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<i64>,
    /// 画面の行の鍵。全種類で一意。`c:<collect_events.id>`、受け入れの切断は管理者に
    /// `a:<audit_log.id>`、管理者未満に `i:<22 文字>`（HMAC。Hub の起動ごとに替わる）。
    key: String,
    /// 発生の時刻（epoch ms、UTC）。
    ts_ms: i64,
    /// `plc_connected`・`plc_disconnected`・`plc_reconnected`・`ingest_disconnected`。
    kind: String,
    /// 接続の id（`conn:<id>`・受け入れの接続の id）。分からなければ null。
    connection_id: Option<i64>,
    /// 接続名（受け入れの切断は記録の時点の控え、無ければ今の名前。PLC は今の名前）。
    /// 消えた接続は null。
    connection_name: Option<String>,
    /// PLC の切断の理由など（`collect_events.detail`）。受け入れの切断は null。
    detail: Option<String>,
    /// 受け入れの切断の経路（例: `ws`）。PLC の行は null。
    transport: Option<String>,
    /// 受け入れの切断の区分（`client_closed`・`superseded`・`revoked` など、決まった綴り）。
    reason: Option<String>,
    /// 管理者だけ: 受け入れの切断の監査の生の `detail`（API キーの名前・ID、接続元など）。
    /// 管理者未満の応答には項目ごと無い。
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<Object>)]
    audit_detail: Option<Value>,
}

/// 書き込みの監査の行（管理者だけ）。
///
/// **同じ `id`（`key`）の行が、後から更新版として届く**: 書き込みの前に暫定の
/// `result: "failed"` で入り、結果が出ると同じ行が `ok`・`failed`・`uncertain` に
/// 確定する（log-before-write）。拒否の行も数が `detail` に書き足される。画面は同じ
/// `key` の行を置き換える（暫定のまま失敗と出し続けない）。
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct WriteLogEntry {
    /// `hub_write_audit.id`。
    id: i64,
    /// 画面の行の鍵（`w:<id>`）。更新版も同じ鍵。
    key: String,
    /// 発生の時刻（epoch ms、UTC。書き込みの要求の時刻で、確定しても変わらない）。
    ts_ms: i64,
    /// 変更の番号（続きの位置。大きいほど新しい版）。
    change_seq: i64,
    /// `write`・`rate_limit_tripped`。
    action: String,
    /// `ok`・`failed`・`uncertain`・`rejected`・`suppressed_disabled`・`suppressed_rate_limited`。
    result: String,
    tag_id: i64,
    /// 書き込みの時点のタグの外部名。
    tag_name: String,
    value: Option<f64>,
    /// 文字列の値（数値でない書き込み）。
    value_text: Option<String>,
    /// 失敗の理由・拒否の数など。
    detail: Option<String>,
    api_key_id: i64,
    /// 書き込みの時点の API キーの名前。
    api_key_name: String,
    api_key_prefix: Option<String>,
    correlation_id: Option<String>,
    /// 呼び出し側の自己申告の操作者（Hub は確かめていない）。
    operator_claim: Option<String>,
}

/// 操作・認証の監査の行（管理者だけ）。
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct OperationLogEntry {
    /// `audit_log.id`。
    id: i64,
    /// 画面の行の鍵（`a:<id>`）。
    key: String,
    /// 発生の時刻（epoch ms、UTC。保留から後で入った行も元の時刻）。
    ts_ms: i64,
    /// `mcp`・`rest`・`grpc`。
    origin: String,
    /// 誰が（未認証のログイン失敗などは null）。
    actor_username: Option<String>,
    actor_role: Option<String>,
    /// `create`・`update`・`delete`・`login`・`login_failed`・`logout`・`denied` など。
    action: String,
    resource: String,
    entity_id: Option<String>,
    /// `ok`・`denied`・`failed`。
    result: String,
    /// 監査の `detail`（JSON。JSON でなければ文字列）。
    #[schema(value_type = Option<Object>)]
    detail: Option<Value>,
}

/// 見せる範囲（モジュール doc「見せる範囲」）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FeedScope {
    Admin,
    ConnectionsOnly,
}

impl FeedScope {
    /// カーソルの封の中の範囲の印。
    fn code(self) -> u8 {
        match self {
            FeedScope::Admin => 1,
            FeedScope::ConnectionsOnly => 2,
        }
    }

    fn of(session: Option<&AuthenticatedSession>) -> Self {
        let admin = session
            .and_then(|session| Role::from_str(&session.identity.role).ok())
            .is_some_and(|role| role.at_least(Role::Admin));
        if admin {
            FeedScope::Admin
        } else {
            FeedScope::ConnectionsOnly
        }
    }
}

/// 続きの位置（モジュール doc「続きの位置と表示の順を分ける」）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct FeedCursor {
    collect: i64,
    audit: i64,
    write: i64,
}

/// 1 つの記録から読んだ結果。
struct Fetched {
    entries: Vec<LogFeedEntry>,
    /// 次の続きの位置。
    position: i64,
    /// 上限に当たった。
    more: bool,
}

/// 読み方（初回か、続きか）。
#[derive(Debug, Clone, Copy)]
enum Window {
    /// 先頭から新しい側へ `limit` 件（初回）。
    Newest,
    /// `after` より大きい位置を昇順で `limit` 件（続き）。
    After(i64),
}

impl Window {
    /// 先頭 `head` までの範囲の WHERE（`column` は位置の列）と ORDER BY。初回は
    /// 発生の時刻の降順（同じ時刻は `id` の降順）で選ぶ（モジュール doc「初回と続き」）。
    /// `ts` は索引の列そのもの（`ts` の索引は `(ts, rowid)` の順なので、`id` が rowid の
    /// 表では `ts DESC, id DESC` まで索引の順で読める）。
    fn clauses(self, column: &str, ts: &str, id: &str) -> (String, String) {
        match self {
            Window::Newest => (
                format!("{column} <= ?"),
                format!("ORDER BY {ts} DESC, {id} DESC"),
            ),
            Window::After(_) => (
                format!("{column} > ? AND {column} <= ?"),
                format!("ORDER BY {column} ASC"),
            ),
        }
    }

    /// 読んだ行（続きは位置の昇順、初回は時刻の降順）から、次の位置と `more` を決める。`rows` は
    /// `limit + 1` 件まで読んだもの（1 件多ければ上限に当たった）。
    fn settle<T>(self, mut rows: Vec<(i64, T)>, head: i64, limit: i64) -> (Vec<T>, i64, bool) {
        let limit = limit as usize;
        match self {
            // 初回: 時刻の新しい側の `limit` 件だけ。古い側の残りは返さないので、位置は先頭。
            Window::Newest => {
                rows.truncate(limit);
                (rows.into_iter().map(|(_, row)| row).collect(), head, false)
            }
            Window::After(_) => {
                if rows.len() > limit {
                    rows.truncate(limit);
                    let last = rows.last().map(|(position, _)| *position).unwrap_or(head);
                    (rows.into_iter().map(|(_, row)| row).collect(), last, true)
                } else {
                    (rows.into_iter().map(|(_, row)| row).collect(), head, false)
                }
            }
        }
    }
}

fn storage(err: sqlx::Error) -> ApiError {
    ApiError(banto_storage::storage_error(err))
}

/// `detail` の値が決まった綴り（英小文字と `_` だけ、短い）なら返す。受け入れの切断の
/// `transport`・`reason` を管理者未満へ返すときに使い、自由な文（接続元・鍵の情報が
/// 混じりうる）は返さない。
fn plain_token(value: Option<&Value>) -> Option<String> {
    let text = value?.as_str()?;
    let ok = !text.is_empty()
        && text.len() <= 40
        && text.bytes().all(|b| b.is_ascii_lowercase() || b == b'_');
    ok.then(|| text.to_string())
}

fn parse_detail(detail: Option<String>) -> Option<Value> {
    detail.map(|text| serde_json::from_str(&text).unwrap_or(Value::String(text)))
}

/// 文はこのモジュールの定数だけで組み、外から来た値は bind する（`AssertSqlSafe` の根拠。
/// 下の 3 つの `fetch_*` も同じ）。
async fn head(tx: &mut Transaction<'_, Sqlite>, sql: String) -> Result<i64, ApiError> {
    sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(sql))
        .fetch_one(&mut **tx)
        .await
        .map_err(storage)
}

async fn fetch_collect(
    tx: &mut Transaction<'_, Sqlite>,
    window: Window,
    limit: i64,
) -> Result<Fetched, ApiError> {
    // 接続の種類に限らず表の先頭（見せない種類はしきい値などで、件数を隠す対象ではない）。
    // 種類で絞った先頭にすると、接続の行が無い間は毎回、表の全体を読み直す。
    let head = head(
        tx,
        "SELECT COALESCE(MAX(id), 0) FROM collect_events".to_string(),
    )
    .await?;
    let (range, order) = window.clauses("e.id", "e.ts", "e.id");
    let sql = format!(
        "SELECT e.id, e.ts, e.kind, e.connection_key, p.name, e.detail \
         FROM collect_events e \
         LEFT JOIN plc_connections p \
           ON e.connection_key LIKE 'conn:%' \
          AND p.id = CAST(substr(e.connection_key, 6) AS INTEGER) \
         WHERE e.kind IN {CONNECTION_KINDS_SQL} AND {range} {order} LIMIT ?"
    );
    let mut query = sqlx::query_as::<
        _,
        (
            i64,
            i64,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
        ),
    >(sqlx::AssertSqlSafe(sql));
    if let Window::After(after) = window {
        query = query.bind(after);
    }
    let rows = query
        .bind(head)
        .bind(limit + 1)
        .fetch_all(&mut **tx)
        .await
        .map_err(storage)?;
    let rows = rows
        .into_iter()
        .map(|(id, ts, kind, connection_key, name, detail)| {
            let connection_id = connection_key
                .as_deref()
                .and_then(|key| key.strip_prefix("conn:"))
                .and_then(|id| id.parse::<i64>().ok());
            (
                id,
                LogFeedEntry::Connection(ConnectionLogEntry {
                    id: Some(id),
                    key: format!("c:{id}"),
                    ts_ms: ts,
                    kind,
                    connection_id,
                    connection_name: name,
                    detail,
                    transport: None,
                    reason: None,
                    audit_detail: None,
                }),
            )
        })
        .collect();
    let (entries, position, more) = window.settle(rows, head, limit);
    Ok(Fetched {
        entries,
        position,
        more,
    })
}

async fn fetch_audit(
    tx: &mut Transaction<'_, Sqlite>,
    keys: &FeedKeys,
    scope: FeedScope,
    window: Window,
    limit: i64,
) -> Result<Fetched, ApiError> {
    let (head_sql, filter) = match scope {
        // 管理者は表の先頭（見せない origin の行を次の回に読み直さない）。
        FeedScope::Admin => (
            "SELECT COALESCE(MAX(id), 0) FROM audit_log".to_string(),
            format!("(({OPERATION_ORIGINS_SQL}) OR ({INGEST_DISCONNECTED_SQL}))"),
        ),
        // 管理者未満は受け入れの切断の行だけの先頭（操作の監査の件数を表さない）。
        FeedScope::ConnectionsOnly => (
            format!("SELECT COALESCE(MAX(id), 0) FROM audit_log WHERE {INGEST_DISCONNECTED_SQL}"),
            INGEST_DISCONNECTED_SQL.to_string(),
        ),
    };
    let head = head(tx, head_sql).await?;
    let (range, order) = window.clauses("id", "ts", "id");
    let ts_ms = text_ts_ms("ts");
    let sql = format!(
        "SELECT id, {ts_ms}, actor_username, actor_role, action, resource, entity_id, detail, \
                origin, result, \
                (SELECT p.name FROM plc_connections p \
                  WHERE a.origin = 'ingest' AND p.id = CAST(a.entity_id AS INTEGER)) \
         FROM audit_log a WHERE {filter} AND {range} {order} LIMIT ?"
    );
    #[allow(clippy::type_complexity)]
    let mut query = sqlx::query_as::<
        _,
        (
            i64,
            i64,
            Option<String>,
            Option<String>,
            String,
            String,
            Option<String>,
            Option<String>,
            String,
            String,
            Option<String>,
        ),
    >(sqlx::AssertSqlSafe(sql));
    if let Window::After(after) = window {
        query = query.bind(after);
    }
    let rows = query
        .bind(head)
        .bind(limit + 1)
        .fetch_all(&mut **tx)
        .await
        .map_err(storage)?;
    let mut entries = Vec::with_capacity(rows.len());
    for (
        id,
        ts_ms,
        actor_username,
        actor_role,
        action,
        resource,
        entity_id,
        detail,
        origin,
        result,
        current_name,
    ) in rows
    {
        let entry = if origin == "ingest" {
            let detail = parse_detail(detail);
            let field = |name: &str| detail.as_ref().and_then(|d| d.get(name));
            let connection_name = field("connection_name_snapshot")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or(current_name);
            let (shown_id, key) = match scope {
                FeedScope::Admin => (Some(id), format!("a:{id}")),
                FeedScope::ConnectionsOnly => (None, keys.ingest_row_key(id)),
            };
            LogFeedEntry::Connection(ConnectionLogEntry {
                id: shown_id,
                key,
                ts_ms,
                kind: action,
                connection_id: entity_id.as_deref().and_then(|id| id.parse().ok()),
                connection_name,
                detail: None,
                transport: plain_token(field("transport")),
                reason: plain_token(field("reason")),
                audit_detail: match scope {
                    FeedScope::Admin => detail,
                    FeedScope::ConnectionsOnly => None,
                },
            })
        } else {
            // 管理者未満の WHERE は受け入れの切断だけなので、ここへは来ない。念のため
            // 範囲の外の行は返さない（位置は進める）。
            if scope != FeedScope::Admin {
                continue;
            }
            LogFeedEntry::Operation(OperationLogEntry {
                id,
                key: format!("a:{id}"),
                ts_ms,
                origin,
                actor_username,
                actor_role,
                action,
                resource,
                entity_id,
                result,
                detail: parse_detail(detail),
            })
        };
        entries.push((id, entry));
    }
    let (entries, position, more) = window.settle(entries, head, limit);
    Ok(Fetched {
        entries,
        position,
        more,
    })
}

async fn fetch_writes(
    tx: &mut Transaction<'_, Sqlite>,
    window: Window,
    limit: i64,
) -> Result<Fetched, ApiError> {
    let head = head(
        tx,
        "SELECT COALESCE(MAX(change_seq), 0) FROM hub_write_audit".to_string(),
    )
    .await?;
    let (range, order) = window.clauses("change_seq", "ts", "id");
    let ts_ms = text_ts_ms("ts");
    let sql = format!(
        "SELECT id, change_seq, {ts_ms}, action, result, tag_id, external_name_snapshot, \
                value_requested, value_requested_text, detail, api_key_id, \
                api_key_name_snapshot, api_key_prefix_snapshot, correlation_id, operator_claim \
         FROM hub_write_audit WHERE {range} {order} LIMIT ?"
    );
    #[allow(clippy::type_complexity)]
    let mut query = sqlx::query_as::<
        _,
        (
            i64,
            i64,
            i64,
            String,
            String,
            i64,
            String,
            Option<f64>,
            Option<String>,
            Option<String>,
            i64,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
        ),
    >(sqlx::AssertSqlSafe(sql));
    if let Window::After(after) = window {
        query = query.bind(after);
    }
    let rows = query
        .bind(head)
        .bind(limit + 1)
        .fetch_all(&mut **tx)
        .await
        .map_err(storage)?;
    let rows = rows
        .into_iter()
        .map(
            |(
                id,
                change_seq,
                ts_ms,
                action,
                result,
                tag_id,
                tag_name,
                value,
                value_text,
                detail,
                api_key_id,
                api_key_name,
                api_key_prefix,
                correlation_id,
                operator_claim,
            )| {
                (
                    change_seq,
                    LogFeedEntry::Write(WriteLogEntry {
                        id,
                        key: format!("w:{id}"),
                        ts_ms,
                        change_seq,
                        action,
                        result,
                        tag_id,
                        tag_name,
                        value,
                        value_text,
                        detail,
                        api_key_id,
                        api_key_name,
                        api_key_prefix,
                        correlation_id,
                        operator_claim,
                    }),
                )
            },
        )
        .collect();
    let (entries, position, more) = window.settle(rows, head, limit);
    Ok(Fetched {
        entries,
        position,
        more,
    })
}

/// 本体（ルーターから切り離して試験から呼べるようにしたもの）。
async fn read_feed(
    pool: &SqlitePool,
    keys: &FeedKeys,
    scope: FeedScope,
    cursor: Option<FeedCursor>,
    limit: i64,
) -> Result<LogFeedResponse, ApiError> {
    // 読み取りだけのトランザクション: 先頭を決めた時点と、その先頭までを読む時点の
    // 間に入った行で食い違わない（モジュール doc）。
    let mut tx = pool.begin().await.map_err(storage)?;
    let window = |position: i64| match cursor {
        None => Window::Newest,
        Some(_) => Window::After(position),
    };
    let given = cursor.unwrap_or_default();

    let collect = fetch_collect(&mut tx, window(given.collect), limit).await?;
    let audit = fetch_audit(&mut tx, keys, scope, window(given.audit), limit).await?;
    let writes = match scope {
        FeedScope::Admin => Some(fetch_writes(&mut tx, window(given.write), limit).await?),
        // 書き込みの表は読まない。`w` は受け取った値（管理者未満の封では常に 0）のまま。
        FeedScope::ConnectionsOnly => None,
    };
    tx.commit().await.map_err(storage)?;

    let next = FeedCursor {
        collect: collect.position,
        audit: audit.position,
        write: writes.as_ref().map_or(given.write, |w| w.position),
    };
    let more = collect.more || audit.more || writes.as_ref().is_some_and(|w| w.more);
    let mut entries: Vec<LogFeedEntry> = collect
        .entries
        .into_iter()
        .chain(audit.entries)
        .chain(writes.into_iter().flat_map(|w| w.entries))
        .collect();
    // 発生の時刻の降順。同じ時刻は鍵で決める（応答ごとに順が揺れない）。
    entries.sort_by(|a, b| b.ts_ms().cmp(&a.ts_ms()).then_with(|| b.key().cmp(a.key())));
    if cursor.is_none() {
        entries.truncate(limit as usize);
    }
    Ok(LogFeedResponse {
        entries,
        cursor: keys.seal(scope, next),
        more,
    })
}

/// `GET /api/log-feed` - 下のログのパネルの取得の口（モジュール doc）。
#[utoipa::path(
    get,
    path = "/api/log-feed",
    params(
        ("cursor" = Option<String>, Query, description = "前の応答の cursor（封をした不透明な文字列）。無ければ初回で、各記録の先頭までの行から発生の時刻の新しい側の最大 limit 件を混ぜて limit 件に切る"),
        ("limit" = Option<i64>, Query, description = "各記録から取る最大件数（既定 200、1〜500 にクランプ）"),
    ),
    responses(
        (status = 200, description = "接続・書き込み・操作の監査の行（発生の時刻の降順）と続きの位置。管理者未満は接続の行だけで、受け入れの切断は時刻・接続名・切断の区分だけ（監査の表の番号は出さず、key は i: の不透明な値）。書き込みの行は同じ key で更新版が届く", body = LogFeedResponse),
        (status = 400, description = "cursor の形が違う（base64url でない・長さが違う）"),
        (status = 401, description = "画面のセッションが無い（API キーでは使えない）"),
        (status = 409, description = "cursor が使えない（Hub の再起動で鍵が替わった・改ざん・ロールが変わって見せる範囲が違う）。kind は conflict、current.code は cursor_expired。cursor 無しの初回から読み直す"),
    ),
    tag = "log-feed",
)]
pub(super) async fn log_feed(
    State(state): State<LogFeedState>,
    session: Option<Extension<AuthenticatedSession>>,
    Query(query): Query<LogFeedQuery>,
) -> Result<Json<LogFeedResponse>, ApiError> {
    let scope = FeedScope::of(session.as_ref().map(|Extension(session)| session));
    let cursor = match query.cursor.as_deref() {
        None => None,
        Some(text) => Some(state.keys.open(text, scope).map_err(cursor_error)?),
    };
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    Ok(Json(
        read_feed(&state.pool, &state.keys, scope, cursor, limit).await?,
    ))
}

/// 読み直しの応答の `current.code`（画面はこれを見て `cursor` 無しで読み直す）。
const CURSOR_EXPIRED_CODE: &str = "cursor_expired";

fn cursor_error(err: CursorError) -> ApiError {
    ApiError(match err {
        CursorError::Malformed => BantoError::BadRequest(
            "cursor の形が違います（前の応答の cursor をそのまま渡してください）".to_string(),
        ),
        CursorError::Expired => BantoError::Conflict {
            message: "cursor が使えません（Hub の再起動・ロールの変更など）。cursor 無しで読み直してください"
                .to_string(),
            current: Some(serde_json::json!({ "code": CURSOR_EXPIRED_CODE })),
        },
    })
}

/// `GET /api/log-feed`。画面のセッションだけ（[`require_session`]、ロールの最低は
/// 付けない。見せる範囲はハンドラが決める）。CSRF のヘッダは、呼び出し側が
/// `admin` のルーターへ merge して掛ける。カーソルの鍵はここで作る（ルーターは
/// プロセスで 1 回組むので、プロセスごとの鍵）。
pub(super) fn log_feed_router(pool: SqlitePool, auth: AuthState) -> Router {
    Router::new()
        .route("/api/log-feed", get(log_feed))
        .with_state(LogFeedState {
            pool,
            keys: Arc::new(FeedKeys::generate()),
        })
        .layer(middleware::from_fn_with_state(
            AuthGate {
                auth,
                operation: OperationKind::Normal,
            },
            require_session,
        ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sealed_cursor_round_trips_only_with_the_same_key_and_scope() {
        let keys = FeedKeys::generate();
        let cursor = FeedCursor {
            collect: 12,
            audit: 4_242_421,
            write: 345,
        };
        let sealed = keys.seal(FeedScope::ConnectionsOnly, cursor);
        assert_eq!(sealed.len(), 72, "{sealed}");
        assert!(
            sealed
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
            "{sealed}"
        );
        assert!(!sealed.contains("4242421"), "{sealed}");
        // 同じ中身でも封ごとに違う（nonce）。
        assert_ne!(sealed, keys.seal(FeedScope::ConnectionsOnly, cursor));
        assert_eq!(keys.open(&sealed, FeedScope::ConnectionsOnly), Ok(cursor));
        // 範囲が違う・鍵が違う・改ざんは読み直し。
        assert_eq!(
            keys.open(&sealed, FeedScope::Admin),
            Err(CursorError::Expired)
        );
        assert_eq!(
            FeedKeys::generate().open(&sealed, FeedScope::ConnectionsOnly),
            Err(CursorError::Expired)
        );
        let mut tampered = sealed.clone().into_bytes();
        tampered[30] = if tampered[30] == b'A' { b'B' } else { b'A' };
        let tampered = String::from_utf8(tampered).unwrap();
        assert_eq!(
            keys.open(&tampered, FeedScope::ConnectionsOnly),
            Err(CursorError::Expired)
        );
        // 形が違うのは 400。
        for bad in [
            "",
            "c12.a0.w345",
            "garbage",
            &sealed[..71],
            &format!("{sealed}A"),
        ] {
            assert_eq!(
                keys.open(bad, FeedScope::ConnectionsOnly),
                Err(CursorError::Malformed),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn ingest_row_keys_are_stable_per_key_and_hide_the_id() {
        let keys = FeedKeys::generate();
        let key = keys.ingest_row_key(4_242_421);
        assert_eq!(key, keys.ingest_row_key(4_242_421));
        assert_ne!(key, keys.ingest_row_key(4_242_422));
        assert_ne!(key, FeedKeys::generate().ingest_row_key(4_242_421));
        assert!(key.starts_with("i:") && key.len() == 24, "{key}");
        assert!(!key.contains("4242421"), "{key}");
    }

    #[test]
    fn only_plain_tokens_pass_to_non_admins() {
        assert_eq!(
            plain_token(Some(&Value::from("client_closed"))),
            Some("client_closed".to_string())
        );
        for bad in [
            Value::from("10.0.0.1:5000"),
            Value::from("key bk_abc revoked"),
            Value::from(""),
            Value::from(3),
        ] {
            assert_eq!(plain_token(Some(&bad)), None, "{bad}");
        }
    }
}
