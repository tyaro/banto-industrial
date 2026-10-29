//! 書き込み監査 (docs/tag-server-design.md §6-3「log-before-write」)。
//! `hub_write_audit` テーブル (`migrations-sqlite/0103_hub_write_audit.sql`) の
//! 唯一のアクセス経路。relay-wright の `engine/write_audit.rs` と同じ
//! **log-before-write** 規律を、hub の書き込みエンドポイント
//! (`crate::rest` の `POST /api/v1/values/{tag}`) 向けに読み替えたもの。
//!
//! ## log-before-write (§6-3、relay-wright と同じ規律)
//!
//! 実際の物理書き込みは2段階で記録する: [`insert_pending`] が
//! `action = 'write'`・`result = 'failed'`(まだ確定していない、が
//! 「試みられた」ことは残る安全側の暫定状態)の行を **`broker.write`
//! を呼ぶ前に** 挿入し、[`set_result`] がその行を `broker.write` の
//! 結果に応じて `ok`/`failed`/`uncertain` に更新する。途中でプロセスが
//! 死んでも(電源断・パニック等)「試みられたが成功が未確認」という記録が
//! 残る - これが安全側の解釈(relay-wright の同モジュール doc comment
//! と同じ理由付け: 「試みたのに記録がない」よりずっと安全)。暫定行の
//! 値は `failed` のまま(#898 でも変えていない)。
//!
//! ## `failed` と `uncertain` の区別 (#898、2026-10-28 オーナー決定)
//!
//! 確定した失敗の行は 2 つに分ける:
//!
//! - `failed`: **確かに効いていない**。送る前に断った(範囲外・アドレス
//!   不正・ドライバの事前の拒否)、PLC が完全な応答でエラーを返した
//!   (SLMP の終了コード・Modbus の例外)、セッションが無く何も送って
//!   いない(`BrokerError::Disconnected` など)。
//! - `uncertain`: **送ったあとで結果が分からなくなった**。送信後の
//!   タイムアウト・切断・応答の不一致(`BrokerError::ConnectionFailed`。
//!   FC22 の ACK のマスクの不一致もここに来る)、broker からの応答が
//!   欠けた。PLC 側では効いている可能性がある。
//!
//! どちらに入れるかは `crate::write_path` が broker の結果から決める
//! (分類の表は同モジュールの doc comment「監査の `result` の分類」)。
//! `uncertain` でも自動では再送しない(broker の約束のまま)。
//!
//! 一方、**物理書き込みを一切試みない抑制**(受付 off による
//! `suppressed_disabled`、レート制限超過による `rate_limit_tripped`/
//! `suppressed_rate_limited`)は最初から確定結果を持つので
//! [`insert_row`] 1回で完結する(pending 経由にする理由がない -
//! そもそも broker を呼んでいない)。
//!
//! ## 定義上の拒否 `rejected` (#711、2026-10-29 オーナー決定)
//!
//! 書込み不可(`not_writable`)・catalog に無い(`not_found`)・スコープ外
//! (`missing_write_scope`)・タグ無効(`tag_disabled`)の拒否も、
//! `action = 'write'`・`result = 'rejected'` の行として残す(不正な試みや
//! 外部システムの設定誤りを追えるように)。理由のコードは `detail` に入る。
//! catalog に無いタグは `tag_id` を引けないので 0 を入れ、要求された名前を
//! `external_name_snapshot` に残す。物理書き込みは試みないので
//! [`WriteAuditService::record_rejection`] で 1 回の挿入で完結する。
//!
//! 監査が拒否で埋まらないよう、**同じ(API キー, 外部名, 理由)は 60 秒
//! ([`REJECTION_WINDOW`])の窓に 1 行**にまとめる。窓の最初の拒否で行を挿入し、
//! 同じ窓の 2 件目以降はその行の `detail` を UPDATE して合計を書く
//! (例: `not_writable（60 秒の窓で計 13 件）`。1 件のときはコードだけ)。
//! 件数は常に DB の行にあるので、拒否が止まっても、記憶の組を上限
//! ([`REJECTION_THROTTLE_MAX_ENTRIES`] 組)で捨てても、再起動しても失われない。
//! 窓が明けた次の拒否は新しい行を挿入して窓を始め直す。挿入に失敗した組は
//! 記憶に残さない。
//!
//! **キーごとの行の上限**(#945 のレビュー): 1 つの API キーが窓の中で別々の行に
//! できる(外部名, 理由)の組は [`REJECTION_ROWS_PER_KEY`] 組まで。超えた拒否は、
//! そのキーと理由ごとの**まとめの行**(`external_name_snapshot = '*'`、
//! `tag_id = 0`、例: `not_found（60 秒の窓で、上限を超えたほかのタグ名で計 7 件）`)
//! に数え、窓の中では同じく UPDATE する。毎回違うタグ名を送っても、1 つのキーが
//! 60 秒に作る行はおよそ「上限 + 理由の数(4)」に収まる。キーの数は発行済みの
//! キーの数で決まり(拒否の監査は認証を通った要求だけ)、記憶は組・キーとも
//! [`REJECTION_THROTTLE_MAX_ENTRIES`] まで(超えて捨てたキーは予算を数え直すので、
//! 1000 を超えるキーが同時に拒否を浴びると上の行数を超えうる)。
//!
//! **背景のキュー**(#945 のレビュー): [`WriteAuditService::record_rejections`] は
//! 拒否を上限つきのキュー([`REJECTION_QUEUE_CAPACITY`] メッセージ)へ `try_send`
//! で渡してすぐ返す(403・404 を DB の待ちで遅らせない。同期の送信なので、要求の
//! future が切断で捨てられても渡し終えた分は欠けない)。記憶と書込みは 1 つの
//! ワーカーのタスクだけが持つので、ロックは要らない(件数は順に数える)。キューが
//! 一杯なら捨てて件数を数え、ワーカーが次に書くときに**あふれの行**(`api_key_id = 0`・
//! キー名とプレフィックスは空・`external_name_snapshot = '*'`・`tag_id = 0`・
//! `detail = 'queue_overflow（N 件を記録できなかった）'`)を 1 行残す(どのキーの
//! 拒否かは数えない)。ワーカーは最初に使うときに呼んだ側のランタイムで起動する。
//! 監査の書込みに失敗しても応答は変えず、ワーカーがログ(`eprintln!`)に残す。
//!
//! **送る前のまとめとキューの予算**(#945 のレビュー: 大きなバッチの拒否で、
//! キューがメモリを抱えないように):
//!
//! - 送る前に、同じ(キー, 外部名, 理由)を件数つきの 1 エントリにまとめる。ワーカーは
//!   件数ぶんを一度に数える(DB の操作は 1 エントリにつき 1 回)。同じタグを 10 万件
//!   並べたバッチも 1 エントリ・1 回の UPDATE になる。
//! - 1 メッセージの別々のエントリは [`REJECTION_MESSAGE_MAX_TAGS`](32)まで。超えた分は
//!   (キー, 理由)ごとのまとめのエントリ(まとめの行に数える)にし、まとめのエントリも
//!   [`REJECTION_MESSAGE_MAX_SUMMARIES`](32)までで、超えた件数はあふれに足す。上限が
//!   キーごとの行の上限より大きいので、1 キーの要求ならワーカーの結果はほぼ変わらない
//!   (窓の中ですでに行のあるタグ名が 33 番目以降に来たときだけ、まとめの行に数える)。
//! - 外部名とキー名は [`REJECTION_NAME_MAX_CHARS`](256 文字)までに切り詰め、末尾を
//!   `…` にして監査に残す。先頭が同じ長い名前どうしは、切り詰めた後で同じ組として
//!   数えうる。
//! - キュー全体で抱える(まとめた後の)エントリは [`REJECTION_QUEUE_MAX_ENTRIES`]
//!   (16384)まで。送るときに足し、ワーカーが処理したら引く。超えるメッセージは送らず、
//!   件数をあふれに足す。
//!
//! **メモリの見積もり**: 1 エントリの構造体は 128 バイト以下(試験で確かめる。いまは
//! およそ 80 バイト)、外部名は 256 文字(UTF-8 で最大 1023 バイト)まで、キー名と
//! プレフィックスはメッセージの中でキーごとに共有する。キューが抱えるのは、最悪でも
//! 16384 × (128 + 1023) バイト ≒ 19 MB に、メッセージ(4096 まで)ごとのキー名・
//! プレフィックス(各 1023 バイトまで)の約 8 MB を足した、およそ 27 MB。ASCII の
//! 短い名前なら数 MB。まとめる前の拒否の `Vec` は要求ごとの一時のもので、要求の
//! 本文の上限(REST のバッチは axum の既定の 2 MB)で抑えられる。
//!
//! **終了時**: 正常な終了では、`RunningHub::shutdown_and_report` が書込みの入口を
//! 止めたあと、設定 DB の pool を閉じる前に [`WriteAuditService::flush`] でキューを
//! 書き切る(上限 `WRITE_AUDIT_FLUSH_TIMEOUT` = 5 秒。超えたらログに出して先へ進む)。
//! サービスの複製がすべて捨てられたときも、ワーカーはキューの残りを書き切ってから
//! 終わる。ランタイムが先に止まったときや上限を超えたときは、キューに残った分と
//! あふれの件数を数えてログに出し、捨てる(書き切らない)。
//!
//! ## スナップショット列 (`api_key_name_snapshot`/`api_key_prefix_snapshot`/`external_name_snapshot`)
//!
//! API キーは失効(`revoked_at`)されても行として残るが、失効後に削除
//! できる(#713、2026-10-29)。タグも削除・リネームされうる。プレフィックスは
//! 認証のときの値を写す(#940 のレビュー: 書込みの途中でキーが削除されても
//! `NULL` にならない。0108 より前の行は `NULL`)。監査行は「その時点で
//! 何が起きたか」を後から名前解決なしに読めるよう、キー名・外部名を
//! **書き込み時点の値としてスナップショット**する(`crate::audit`の
//! `rule_name_snapshot` と同じ設計判断)。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use banto_core::{BantoError, ListParams, ListResult};
use banto_storage::ColumnMap;
use serde::Serialize;
use sqlx::{QueryBuilder, Sqlite, SqlitePool};
use tokio::sync::mpsc;
use utoipa::ToSchema;

/// `hub_write_audit.action` の値 (`db.rs` の SQL `CHECK` と対応)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteAuditAction {
    /// 実際の物理書き込みの試行(成功/失敗いずれも log-before-write を通る)。
    Write,
    /// §6-4 のレート制限ブレーカがトリップしたこと自体の記録
    /// (物理書き込みは試みない)。
    RateLimitTripped,
}

impl WriteAuditAction {
    pub fn as_str(self) -> &'static str {
        match self {
            WriteAuditAction::Write => "write",
            WriteAuditAction::RateLimitTripped => "rate_limit_tripped",
        }
    }
}

/// `hub_write_audit.result` の値 (`db.rs` の SQL `CHECK` と対応)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteAuditResult {
    /// 物理書き込みが成功した。
    Ok,
    /// 物理書き込みが確かに効いていない(送る前に断った・PLC がエラーを
    /// 返した・セッションが無く送っていない)。log-before-write の暫定行も
    /// この値(モジュール doc)。
    Failed,
    /// #898: 送ったあとで結果が分からなくなった(送信後のタイムアウト・
    /// 切断・応答の不一致)。PLC 側では効いている可能性がある。自動では
    /// 再送しない。
    Uncertain,
    /// 書き込み受付(§6-6)が off だったため、物理書き込みを一切試みず抑制した。
    SuppressedDisabled,
    /// レート制限(§6-4)を超過するため、物理書き込みを一切試みず抑制した。
    SuppressedRateLimited,
    /// #711(2026-10-29 オーナー決定): 定義上の拒否(書込み不可・catalog に
    /// 無い・スコープ外・タグ無効)。理由のコードは `detail` に入る
    /// ([`WriteRejectionReason`])。`action` は `write` のまま。
    Rejected,
}

impl WriteAuditResult {
    pub fn as_str(self) -> &'static str {
        match self {
            WriteAuditResult::Ok => "ok",
            WriteAuditResult::Failed => "failed",
            WriteAuditResult::Uncertain => "uncertain",
            WriteAuditResult::SuppressedDisabled => "suppressed_disabled",
            WriteAuditResult::SuppressedRateLimited => "suppressed_rate_limited",
            WriteAuditResult::Rejected => "rejected",
        }
    }
}

/// 1行分のフィールド(自動採番の `id`/`ts` を除く)。
/// [`WriteAuditRow::new`] + `with_*` で組み立てる。
#[derive(Debug, Clone)]
pub struct WriteAuditRow {
    pub api_key_id: i64,
    pub api_key_name_snapshot: String,
    /// 認証のときのキーのプレフィックス（`ApiKeyContext::prefix`）。名前と
    /// 同じ出どころで、書込みの途中でキーが削除されても `NULL` にならない。
    pub api_key_prefix_snapshot: String,
    pub tag_id: i64,
    pub external_name_snapshot: String,
    pub value_requested: Option<f64>,
    /// T20 宿題#1: `value_requested`(REAL)には入らない文字列書き込みの
    /// テキストをここに残す(`with_value_requested_text` 参照)。
    pub value_requested_text: Option<String>,
    pub action: WriteAuditAction,
    pub result: WriteAuditResult,
    pub detail: Option<String>,
}

impl WriteAuditRow {
    pub fn new(
        api_key_id: i64,
        api_key_name_snapshot: impl Into<String>,
        api_key_prefix_snapshot: impl Into<String>,
        tag_id: i64,
        external_name_snapshot: impl Into<String>,
        action: WriteAuditAction,
        result: WriteAuditResult,
    ) -> Self {
        Self {
            api_key_id,
            api_key_name_snapshot: api_key_name_snapshot.into(),
            api_key_prefix_snapshot: api_key_prefix_snapshot.into(),
            tag_id,
            external_name_snapshot: external_name_snapshot.into(),
            value_requested: None,
            value_requested_text: None,
            action,
            result,
            detail: None,
        }
    }

    pub fn with_value_requested(mut self, value: f64) -> Self {
        self.value_requested = Some(value);
        self
    }

    /// T20 ①a(docs/banto-hub-t20-design.md §3.1): the `Option`-taking twin of
    /// [`Self::with_value_requested`], for callers whose value may be a
    /// string - `value_requested` is a `REAL` column that cannot hold text,
    /// so a string write's audit row leaves it `NULL` (`None`) rather than a
    /// misleading `0.0` (`crate::write_path::RequestedValue::as_audit_value`/
    /// `ConvertedValue::as_audit_value` are the two callers that decide
    /// `None` vs `Some`).
    pub fn with_value_requested_opt(mut self, value: Option<f64>) -> Self {
        self.value_requested = value;
        self
    }

    /// T20 宿題#1: `value_requested`(REAL)に入らない文字列書き込みの
    /// テキストを残す([`Self::with_value_requested_opt`]の隣。
    /// `crate::write_path::RequestedValue::as_audit_text`/
    /// `ConvertedValue::as_audit_text` が呼び出し元)。
    pub fn with_value_requested_text(mut self, value: Option<String>) -> Self {
        self.value_requested_text = value;
        self
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

/// `GET`/`POST /api/write-audit/list` の1行分(wire は camelCase、
/// `crate::audit::AuditLogEntry` と同じ規約)。
#[derive(Debug, Clone, PartialEq, Serialize, sqlx::FromRow, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WriteAuditEntry {
    pub id: i64,
    pub ts: String,
    pub api_key_id: i64,
    pub api_key_name_snapshot: String,
    /// #713（2026-10-29 オーナー決定）: 書込み時点のキーのプレフィックス
    /// （失効したキーを削除した後も記録から読めるようにする。0108 より前の
    /// 行は `NULL`）。
    pub api_key_prefix_snapshot: Option<String>,
    pub tag_id: i64,
    pub external_name_snapshot: String,
    pub value_requested: Option<f64>,
    pub value_requested_text: Option<String>,
    pub action: String,
    pub result: String,
    pub detail: Option<String>,
}

fn column_map() -> ColumnMap {
    ColumnMap::new()
        .column("id", "id")
        .column("ts", "ts")
        .column("apiKeyId", "api_key_id")
        .column("apiKeyNameSnapshot", "api_key_name_snapshot")
        .column("apiKeyPrefixSnapshot", "api_key_prefix_snapshot")
        .column("tagId", "tag_id")
        .column("externalNameSnapshot", "external_name_snapshot")
        .column("valueRequested", "value_requested")
        .column("valueRequestedText", "value_requested_text")
        .column("action", "action")
        .column("result", "result")
        .column("detail", "detail")
}

/// #711: 定義上の拒否の理由(`result = 'rejected'` の行の `detail` の先頭に
/// 入るコード。REST の応答の `error` と同じ綴り)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WriteRejectionReason {
    /// `writable == false`(`db` タグを含む)。
    NotWritable,
    /// catalog に無い外部名。
    NotFound,
    /// `write:{タグ}` スコープが無い。
    MissingWriteScope,
    /// 実効 enabled が false。
    TagDisabled,
}

impl WriteRejectionReason {
    pub fn as_str(self) -> &'static str {
        match self {
            WriteRejectionReason::NotWritable => "not_writable",
            WriteRejectionReason::NotFound => "not_found",
            WriteRejectionReason::MissingWriteScope => "missing_write_scope",
            WriteRejectionReason::TagDisabled => "tag_disabled",
        }
    }
}

/// #711: 同じ(キー, タグ, 理由)の拒否を 1 行にまとめる窓。
pub const REJECTION_WINDOW: Duration = Duration::from_secs(60);

/// #711: まとめの記憶に持つ組の上限。超えたら窓の明けた組、それでも足りなければ
/// 最も古い窓の組を捨てる。捨てても、件数はすでに DB の行の `detail` にあるので
/// 失われない(捨てた組の次の拒否は、新しい行を挿入して窓を始め直す)。
/// キーごとの行の予算([`REJECTION_ROWS_PER_KEY`])の記憶も同じ上限で持つ。
pub const REJECTION_THROTTLE_MAX_ENTRIES: usize = 1000;

/// #945 のレビュー: 1 つの API キーが、[`REJECTION_WINDOW`] の中で別々の行として
/// 残せる(外部名, 理由)の組の数。超えた拒否は、そのキーと理由ごとの
/// まとめの行(外部名 `*`、`tag_id` 0)に数える。毎回違うタグ名を送っても、
/// 1 つのキーが窓の中で挿入できる行は、およそ「この上限 + 理由の数(4)」に収まる。
pub const REJECTION_ROWS_PER_KEY: usize = 20;

/// #945 のレビュー: 拒否の監査を背景のワーカーへ渡すキューの容量(メッセージの数。
/// バッチの拒否は 1 メッセージ)。一杯のときは捨てて、あふれの件数を数える。
pub const REJECTION_QUEUE_CAPACITY: usize = 4096;

/// #945 のレビュー: キュー全体で抱える(まとめた後の)エントリの予算。送るときに
/// 足し、ワーカーが処理したら引く。超えるメッセージは送らず、件数をあふれに足す。
/// 抱えるメモリの見積もりはモジュールの doc comment の「背景のキュー」。
pub const REJECTION_QUEUE_MAX_ENTRIES: usize = 16384;

/// #945 のレビュー: 1 つのメッセージに入れる、別々の(キー, 外部名, 理由)の
/// エントリの上限。超えた分は(キー, 理由)ごとのまとめのエントリ
/// (`RejectionTarget::OtherTags`)の件数にする。キーごとの行の上限
/// ([`REJECTION_ROWS_PER_KEY`])より大きいので、1 つのキーの要求ならワーカーの結果は
/// 変わらない(窓の中ですでに行のあるタグ名が 33 番目以降に来たときだけ、その行では
/// なくまとめの行に数える)。
pub const REJECTION_MESSAGE_MAX_TAGS: usize = 32;

/// #945 のレビュー: 1 つのメッセージに入れる、まとめのエントリ((キー, 理由)ごと)の
/// 上限。超えた分はあふれの件数に足す(呼び出しは要求ごとに 1 つのキーなので、
/// ふつうは理由の数(4)を超えない)。
pub const REJECTION_MESSAGE_MAX_SUMMARIES: usize = 32;

/// #945 のレビュー: キューに抱える外部名・キー名の長さの上限(文字数)。超えた名前は
/// 先頭の `上限 - 1` 文字に `…` を付けて切り詰め、監査にもその形で残す。切り詰めた
/// 名前どうしは、まとめのキーとして同じ組になりうる(先頭が同じ長い名前は 1 行に
/// 数える)。
pub const REJECTION_NAME_MAX_CHARS: usize = 256;

/// まとめの行・あふれの行の外部名。
pub const REJECTION_SUMMARY_NAME: &str = "*";

/// #711: 行をまとめる相手。タグごとの行か、キーごとの上限を超えた分のまとめの行か。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum RejectionTarget {
    Tag(String),
    OtherTags,
}

/// #711: 組ごとの現在の窓。
struct RejectionWindow {
    /// 窓の最初の拒否の時刻(窓はここから [`REJECTION_WINDOW`])。
    start: Instant,
    /// 窓の最初の拒否で挿入した行の id(以降の拒否はこの行を UPDATE する)。
    row_id: i64,
    /// 窓の中の拒否の合計(最初の 1 件を含む)。
    count: u32,
}

/// #945 のレビュー: キーごとの、窓の中で挿入したタグごとの行の数。
struct KeyBudget {
    start: Instant,
    rows: usize,
}

type RejectionKey = (i64, RejectionTarget, WriteRejectionReason);

/// 上限を超えないよう、窓の明けたもの、それでも足りなければ最も古いものを捨てる。
fn make_room<K: Clone + Eq + std::hash::Hash, V>(
    map: &mut HashMap<K, V>,
    now: Instant,
    start: impl Fn(&V) -> Instant,
) {
    if map.len() < REJECTION_THROTTLE_MAX_ENTRIES {
        return;
    }
    map.retain(|_, v| now.saturating_duration_since(start(v)) < REJECTION_WINDOW);
    if map.len() >= REJECTION_THROTTLE_MAX_ENTRIES {
        if let Some(oldest) = map
            .iter()
            .min_by_key(|(_, v)| start(v))
            .map(|(k, _)| k.clone())
        {
            map.remove(&oldest);
        }
    }
}

fn rejection_detail(reason: WriteRejectionReason, count: u32) -> String {
    if count > 1 {
        format!(
            "{}（{} 秒の窓で計 {} 件）",
            reason.as_str(),
            REJECTION_WINDOW.as_secs(),
            count
        )
    } else {
        reason.as_str().to_string()
    }
}

fn rejection_summary_detail(reason: WriteRejectionReason, count: u32) -> String {
    format!(
        "{}（{} 秒の窓で、上限を超えたほかのタグ名で計 {} 件）",
        reason.as_str(),
        REJECTION_WINDOW.as_secs(),
        count
    )
}

fn queue_overflow_detail(count: u64) -> String {
    format!("queue_overflow（{count} 件を記録できなかった）")
}

/// [`WriteAuditService::record_rejections`] に渡す拒否 1 件分。
#[derive(Debug, Clone)]
pub struct RejectionEntry {
    pub api_key_id: i64,
    pub api_key_name: String,
    pub api_key_prefix: String,
    pub tag_id: i64,
    pub external_name: String,
    pub reason: WriteRejectionReason,
}

/// #945 のレビュー: キューに入れる、まとめた後のエントリ。同じ
/// (キー, 外部名, 理由)を `count` 件にまとめ、キー名とプレフィックスはキーごとに
/// 共有する。
#[derive(Debug, Clone)]
struct QueuedRejection {
    api_key_id: i64,
    api_key_name: Arc<str>,
    api_key_prefix: Arc<str>,
    tag_id: i64,
    target: RejectionTarget,
    reason: WriteRejectionReason,
    count: u32,
}

/// 名前を [`REJECTION_NAME_MAX_CHARS`] 文字までに切り詰める(超えたら末尾を `…`)。
fn truncate_name(mut s: String) -> String {
    if s.char_indices().nth(REJECTION_NAME_MAX_CHARS).is_some() {
        let keep = s
            .char_indices()
            .nth(REJECTION_NAME_MAX_CHARS - 1)
            .map_or(s.len(), |(i, _)| i);
        s.truncate(keep);
        s.push('…');
    }
    s
}

/// #945 のレビュー: 送る前のまとめ。同じ(キー, 外部名, 理由)を 1 件にまとめ、
/// 別々のエントリは [`REJECTION_MESSAGE_MAX_TAGS`] まで、超えた分は(キー, 理由)ごとの
/// まとめのエントリ([`REJECTION_MESSAGE_MAX_SUMMARIES`] まで)に数える。それも超えた
/// 件数は 2 つ目の値で返す(あふれに足す)。
fn coalesce_rejections(entries: Vec<RejectionEntry>) -> (Vec<QueuedRejection>, u64) {
    let mut out: Vec<QueuedRejection> = Vec::new();
    let mut tag_index: HashMap<(i64, WriteRejectionReason, String), usize> = HashMap::new();
    let mut summary_index: HashMap<(i64, WriteRejectionReason), usize> = HashMap::new();
    let mut names: HashMap<i64, (Arc<str>, Arc<str>)> = HashMap::new();
    let mut tags = 0usize;
    let mut dropped: u64 = 0;
    for e in entries {
        let reason = e.reason;
        let key_id = e.api_key_id;
        let k = (key_id, reason, truncate_name(e.external_name));
        if let Some(&i) = tag_index.get(&k) {
            out[i].count = out[i].count.saturating_add(1);
            continue;
        }
        if tags >= REJECTION_MESSAGE_MAX_TAGS {
            if let Some(&i) = summary_index.get(&(key_id, reason)) {
                out[i].count = out[i].count.saturating_add(1);
                continue;
            }
            if summary_index.len() >= REJECTION_MESSAGE_MAX_SUMMARIES {
                dropped += 1;
                continue;
            }
        }
        let (name, prefix) = names
            .entry(key_id)
            .or_insert_with(|| {
                (
                    Arc::from(truncate_name(e.api_key_name)),
                    Arc::from(truncate_name(e.api_key_prefix)),
                )
            })
            .clone();
        let i = out.len();
        if tags < REJECTION_MESSAGE_MAX_TAGS {
            out.push(QueuedRejection {
                api_key_id: key_id,
                api_key_name: name,
                api_key_prefix: prefix,
                tag_id: e.tag_id,
                target: RejectionTarget::Tag(k.2.clone()),
                reason,
                count: 1,
            });
            tag_index.insert(k, i);
            tags += 1;
        } else {
            out.push(QueuedRejection {
                api_key_id: key_id,
                api_key_name: name,
                api_key_prefix: prefix,
                tag_id: 0,
                target: RejectionTarget::OtherTags,
                reason,
                count: 1,
            });
            summary_index.insert((key_id, reason), i);
        }
    }
    (out, dropped)
}

fn queued_total(entries: &[QueuedRejection]) -> u64 {
    entries.iter().map(|e| u64::from(e.count)).sum()
}

/// 完結した 1 行を挿入して新しい `id` を返す。
async fn insert_row_in(pool: &SqlitePool, row: &WriteAuditRow) -> Result<i64, BantoError> {
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO hub_write_audit (\
            api_key_id, api_key_name_snapshot, api_key_prefix_snapshot, tag_id, \
            external_name_snapshot, value_requested, value_requested_text, action, \
            result, detail\
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
         RETURNING id",
    )
    .bind(row.api_key_id)
    .bind(&row.api_key_name_snapshot)
    .bind(&row.api_key_prefix_snapshot)
    .bind(row.tag_id)
    .bind(&row.external_name_snapshot)
    .bind(row.value_requested)
    .bind(&row.value_requested_text)
    .bind(row.action.as_str())
    .bind(row.result.as_str())
    .bind(&row.detail)
    .fetch_one(pool)
    .await
    .map_err(banto_storage::storage_error)?;
    Ok(id)
}

/// #711 / #945 のレビュー: 拒否の行をまとめる記憶と書込み。背景のワーカーだけが
/// 持つ(ロックは要らない)。件数は行の `detail` に書き込み済みなので、再起動や
/// 組の破棄で件数が失われることはない。
struct RejectionWorker {
    pool: SqlitePool,
    windows: HashMap<RejectionKey, RejectionWindow>,
    budgets: HashMap<i64, KeyBudget>,
}

impl RejectionWorker {
    fn new(pool: SqlitePool) -> Self {
        Self {
            pool,
            windows: HashMap::new(),
            budgets: HashMap::new(),
        }
    }

    /// 窓の中なら件数(`add`)を足して UPDATE する(件数がいくつでも 1 回)。窓が無い・
    /// 明けたなら `false`(明けた組は記憶から消す)。
    async fn bump(&mut self, key: &RejectionKey, now: Instant, add: u32) -> bool {
        let Some(w) = self.windows.get_mut(key) else {
            return false;
        };
        if now.saturating_duration_since(w.start) >= REJECTION_WINDOW {
            // 窓が明けた: 前の行はそのまま残し、新しい行で窓を始め直す。
            self.windows.remove(key);
            return false;
        }
        w.count = w.count.saturating_add(add);
        let reason = key.2;
        let detail = match key.1 {
            RejectionTarget::Tag(_) => rejection_detail(reason, w.count),
            RejectionTarget::OtherTags => rejection_summary_detail(reason, w.count),
        };
        let result = sqlx::query("UPDATE hub_write_audit SET detail = ? WHERE id = ?")
            .bind(&detail)
            .bind(w.row_id)
            .execute(&self.pool)
            .await;
        if let Err(err) = result {
            eprintln!(
                "banto-hub: 書き込み監査(rejected: {})の件数の更新に失敗しました: {err}",
                reason.as_str()
            );
        }
        true
    }

    /// 新しい窓の最初の行を挿入して記憶する。挿入に失敗した組は記憶に残さない
    /// (次の拒否で挿入をやり直す)。成功したら `true`。`count` は行の `detail` に
    /// 書いた件数。
    async fn insert_first(
        &mut self,
        key: RejectionKey,
        now: Instant,
        row: WriteAuditRow,
        count: u32,
    ) -> bool {
        match insert_row_in(&self.pool, &row).await {
            Ok(row_id) => {
                make_room(&mut self.windows, now, |w| w.start);
                self.windows.insert(
                    key,
                    RejectionWindow {
                        start: now,
                        row_id,
                        count,
                    },
                );
                true
            }
            Err(err) => {
                eprintln!(
                    "banto-hub: 書き込み監査(rejected: {})の記録に失敗しました: {err}",
                    key.2.as_str()
                );
                false
            }
        }
    }

    /// まとめたエントリ 1 件(`count` 件の拒否)を残す(時刻を注入できる形。試験は
    /// これを直接呼ぶ)。件数がいくつでも、DB の操作は 1 回(UPDATE か INSERT)。
    async fn record_at(&mut self, now: Instant, e: &QueuedRejection) {
        let count = e.count.max(1);
        if let RejectionTarget::Tag(name) = &e.target {
            let tag_key: RejectionKey = (e.api_key_id, e.target.clone(), e.reason);
            if self.bump(&tag_key, now, count).await {
                return;
            }
            // 新しい行が要る。キーごとの予算の中ならタグごとの行にする。
            let budget = self.budgets.get(&e.api_key_id);
            let fresh =
                budget.is_none_or(|b| now.saturating_duration_since(b.start) >= REJECTION_WINDOW);
            let used = if fresh {
                0
            } else {
                budget.map_or(0, |b| b.rows)
            };
            if used < REJECTION_ROWS_PER_KEY {
                let row = WriteAuditRow::new(
                    e.api_key_id,
                    &*e.api_key_name,
                    &*e.api_key_prefix,
                    e.tag_id,
                    name.as_str(),
                    WriteAuditAction::Write,
                    WriteAuditResult::Rejected,
                )
                .with_detail(rejection_detail(e.reason, count));
                if self.insert_first(tag_key, now, row, count).await {
                    if fresh {
                        make_room(&mut self.budgets, now, |b| b.start);
                        self.budgets.insert(
                            e.api_key_id,
                            KeyBudget {
                                start: now,
                                rows: 1,
                            },
                        );
                    } else if let Some(b) = self.budgets.get_mut(&e.api_key_id) {
                        b.rows += 1;
                    }
                }
                return;
            }
        }
        // 予算を超えた(または送る前にまとめのエントリになった): キーと理由ごとの
        // まとめの行に数える。
        let summary_key: RejectionKey = (e.api_key_id, RejectionTarget::OtherTags, e.reason);
        if self.bump(&summary_key, now, count).await {
            return;
        }
        let row = WriteAuditRow::new(
            e.api_key_id,
            &*e.api_key_name,
            &*e.api_key_prefix,
            0,
            REJECTION_SUMMARY_NAME,
            WriteAuditAction::Write,
            WriteAuditResult::Rejected,
        )
        .with_detail(rejection_summary_detail(e.reason, count));
        self.insert_first(summary_key, now, row, count).await;
    }

    /// キューのあふれの件数があれば、まとめの行として 1 行残して 0 に戻す。
    /// 挿入に失敗したら件数を戻す(次の機会にもう一度試す)。
    async fn write_overflow(&mut self, overflow: &AtomicU64) {
        let n = overflow.swap(0, Ordering::Relaxed);
        if n == 0 {
            return;
        }
        let row = WriteAuditRow::new(
            0,
            "",
            "",
            0,
            REJECTION_SUMMARY_NAME,
            WriteAuditAction::Write,
            WriteAuditResult::Rejected,
        )
        .with_detail(queue_overflow_detail(n));
        if let Err(err) = insert_row_in(&self.pool, &row).await {
            overflow.fetch_add(n, Ordering::Relaxed);
            eprintln!(
                "banto-hub: 書き込み監査(rejected)のあふれ {n} 件の記録に失敗しました: {err}"
            );
        }
    }
}

enum RejectionMsg {
    /// 受け取った時刻(キューで待った分は窓をずらさない)と、まとめた後の拒否。
    Rejections(Instant, Vec<QueuedRejection>),
    /// それより前に渡した分を書き終えたら応答する。
    Flush(tokio::sync::oneshot::Sender<()>),
}

/// ワーカーのタスクが(ランタイムの終了で)捨てられたとき、キューに残った分を
/// 数えてログに出す。
struct QueueDrain {
    rx: mpsc::Receiver<RejectionMsg>,
    overflow: Arc<AtomicU64>,
    pending: Arc<AtomicUsize>,
}

impl Drop for QueueDrain {
    fn drop(&mut self) {
        let mut left: u64 = self.overflow.swap(0, Ordering::Relaxed);
        while let Ok(msg) = self.rx.try_recv() {
            if let RejectionMsg::Rejections(_, entries) = msg {
                left += queued_total(&entries);
                self.pending.fetch_sub(entries.len(), Ordering::Relaxed);
            }
        }
        if left > 0 {
            eprintln!("banto-hub: 書き込み監査(rejected)の {left} 件を記録できずに終了しました");
        }
    }
}

async fn run_rejection_worker(mut worker: RejectionWorker, mut q: QueueDrain) {
    while let Some(msg) = q.rx.recv().await {
        worker.write_overflow(&q.overflow).await;
        match msg {
            RejectionMsg::Rejections(at, entries) => {
                for e in &entries {
                    worker.record_at(at, e).await;
                }
                // 処理し終えたので予算を返す。
                q.pending.fetch_sub(entries.len(), Ordering::Relaxed);
            }
            RejectionMsg::Flush(ack) => {
                let _ = ack.send(());
            }
        }
        worker.write_overflow(&q.overflow).await;
    }
    // すべての送り手が捨てられた: 残りは書き切った。
    worker.write_overflow(&q.overflow).await;
}

/// #945 のレビュー: 拒否の監査のキュー。ワーカーは最初に使うときに、呼んだ側の
/// tokio のランタイムで起動する(`WriteAuditService::new` はランタイムの外でも
/// 呼べる)。
struct RejectionQueue {
    pool: SqlitePool,
    capacity: usize,
    /// 抱えてよい(まとめた後の)エントリの数([`REJECTION_QUEUE_MAX_ENTRIES`])。
    max_entries: usize,
    tx: OnceLock<mpsc::Sender<RejectionMsg>>,
    overflow: Arc<AtomicU64>,
    /// いまキューに抱えている(まだ処理し終えていない)エントリの数。
    pending: Arc<AtomicUsize>,
}

impl RejectionQueue {
    fn sender(&self) -> Option<&mpsc::Sender<RejectionMsg>> {
        if let Some(tx) = self.tx.get() {
            return Some(tx);
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            eprintln!("banto-hub: 書き込み監査(rejected)のワーカーを tokio のランタイムの外では起動できません");
            return None;
        };
        Some(self.tx.get_or_init(|| {
            let (tx, rx) = mpsc::channel(self.capacity);
            let worker = RejectionWorker::new(self.pool.clone());
            let drain = QueueDrain {
                rx,
                overflow: self.overflow.clone(),
                pending: self.pending.clone(),
            };
            handle.spawn(run_rejection_worker(worker, drain));
            tx
        }))
    }

    /// エントリの予算を `n` だけ取る。予算を超えるなら取らずに `false`。
    fn reserve(&self, n: usize) -> bool {
        self.pending
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |cur| {
                cur.checked_add(n).filter(|&next| next <= self.max_entries)
            })
            .is_ok()
    }
}

/// 書き込み監査サービス(`crate::audit::AuditLogService` と同じ形の
/// 薄いラッパー)。`Clone` は安価(`SqlitePool` は `Arc` バックド)。
/// 拒否のキューとワーカー([`Self::record_rejections`])は複製の間で共有する。
#[derive(Clone)]
pub struct WriteAuditService {
    pool: SqlitePool,
    rejections: Arc<RejectionQueue>,
}

impl WriteAuditService {
    pub fn new(pool: SqlitePool) -> Self {
        Self::with_rejection_queue_limits(
            pool,
            REJECTION_QUEUE_CAPACITY,
            REJECTION_QUEUE_MAX_ENTRIES,
        )
    }

    fn with_rejection_queue_limits(pool: SqlitePool, capacity: usize, max_entries: usize) -> Self {
        Self {
            rejections: Arc::new(RejectionQueue {
                pool: pool.clone(),
                capacity,
                max_entries,
                tx: OnceLock::new(),
                overflow: Arc::new(AtomicU64::new(0)),
                pending: Arc::new(AtomicUsize::new(0)),
            }),
            pool,
        }
    }

    /// #711: 定義上の拒否を `result = 'rejected'` で残す(窓の中の同じ
    /// (キー, タグ, 理由)は 1 行にまとめる)。1 件だけの入口。複数件は
    /// [`Self::record_rejections`]。DB を待たずにすぐ返す。
    pub fn record_rejection(
        &self,
        api_key_id: i64,
        api_key_name: &str,
        api_key_prefix: &str,
        tag_id: i64,
        external_name: &str,
        reason: WriteRejectionReason,
    ) {
        self.record_rejections(vec![RejectionEntry {
            api_key_id,
            api_key_name: api_key_name.to_string(),
            api_key_prefix: api_key_prefix.to_string(),
            tag_id,
            external_name: external_name.to_string(),
            reason,
        }]);
    }

    /// 複数の拒否を背景のワーカーへ渡して、**DB を待たずにすぐ返す**(#945 の
    /// レビュー: 403・404 を監査の DB の待ちで遅らせない)。同期の `try_send` なので、
    /// 要求の future が捨てられても、渡し終えたものは失われない。
    ///
    /// 送る前に、同じ(キー, 外部名, 理由)を件数つきの 1 件にまとめ、別々のエントリを
    /// [`REJECTION_MESSAGE_MAX_TAGS`] まで、超えた分を(キー, 理由)ごとのまとめに
    /// する(名前は [`REJECTION_NAME_MAX_CHARS`] 文字まで)。キューが一杯
    /// ([`REJECTION_QUEUE_CAPACITY`] メッセージ)か、抱えるエントリが予算
    /// ([`REJECTION_QUEUE_MAX_ENTRIES`])を超えるなら送らずに、件数をあふれに足す
    /// (ワーカーが次に書くときに `queue_overflow` の行として残す)。記録の失敗は
    /// ワーカーがログに残す。
    pub fn record_rejections(&self, entries: Vec<RejectionEntry>) {
        if entries.is_empty() {
            return;
        }
        let Some(tx) = self.rejections.sender() else {
            return;
        };
        let (queued, dropped) = coalesce_rejections(entries);
        if dropped > 0 {
            self.rejections
                .overflow
                .fetch_add(dropped, Ordering::Relaxed);
        }
        if queued.is_empty() {
            return;
        }
        let n_entries = queued.len();
        let n = queued_total(&queued);
        if !self.rejections.reserve(n_entries) {
            self.rejections.overflow.fetch_add(n, Ordering::Relaxed);
            return;
        }
        if tx
            .try_send(RejectionMsg::Rejections(Instant::now(), queued))
            .is_err()
        {
            self.rejections
                .pending
                .fetch_sub(n_entries, Ordering::Relaxed);
            self.rejections.overflow.fetch_add(n, Ordering::Relaxed);
        }
    }

    /// それまでに渡した拒否をワーカーが書き終えるのを待つ(試験で、拒否の直後に
    /// 監査を読むため。終了の経路でも、pool を閉じる前に上限つきで呼ぶ)。
    pub async fn flush(&self) {
        let Some(tx) = self.rejections.sender() else {
            return;
        };
        let (ack, done) = tokio::sync::oneshot::channel();
        if tx.send(RejectionMsg::Flush(ack)).await.is_ok() {
            let _ = done.await;
        }
    }

    /// 完結した1行をそのまま挿入する(抑制系: `suppressed_disabled`/
    /// `rate_limit_tripped`)。新しい `id` を返す。
    pub async fn insert_row(&self, row: &WriteAuditRow) -> Result<i64, BantoError> {
        insert_row_in(&self.pool, row).await
    }

    /// log-before-write (§6-3): `broker.write` を呼ぶ**前**に
    /// `result = 'failed'` の暫定行を挿入する(このモジュールの doc
    /// comment 参照)。`row.action` は常に `Write` でなければならない
    /// (`debug_assert` で検査 - relay-wright の `insert_pending_fire` と
    /// 同じ規律)。
    pub async fn insert_pending(&self, row: &WriteAuditRow) -> Result<i64, BantoError> {
        debug_assert_eq!(row.action, WriteAuditAction::Write);
        let pending = WriteAuditRow {
            result: WriteAuditResult::Failed,
            ..row.clone()
        };
        self.insert_row(&pending).await
    }

    /// [`Self::insert_pending`] した行の `result` を確定させる
    /// (`broker.write` 成功なら `Ok`、確かに効いていなければ `Failed`、
    /// 送ったあとで結果が分からなければ `Uncertain` - #898)。`detail` は
    /// 失敗理由の説明文(T8-2、docs/tag-server-design.md §6.1、
    /// 2026-08-06: 例えば T8 の RMW 確認読み不一致
    /// `PlcWriteError::BitWriteVerificationFailed`
    /// (`ItemError::VerificationFailed`)の「書き戻し競合の可能性があります」)- `crate::write_path::execute_write`
    /// は成功時に `None`、失敗時に `WriteRejection::detail()` をそのまま渡す
    /// (REST/gRPC の応答本文と同じ文言を監査行にも残す)。
    pub async fn set_result(
        &self,
        audit_id: i64,
        result: WriteAuditResult,
        detail: Option<&str>,
    ) -> Result<(), BantoError> {
        sqlx::query("UPDATE hub_write_audit SET result = ?, detail = ? WHERE id = ?")
            .bind(result.as_str())
            .bind(detail)
            .bind(audit_id)
            .execute(&self.pool)
            .await
            .map_err(banto_storage::storage_error)?;
        Ok(())
    }

    /// フィルタ/ソート/ページングつきの読み取り(`crate::audit::AuditLogService::list`
    /// と同じ `banto_storage::list_query` パターン)。
    /// `crate::rest` の `POST /api/write-audit/list`(admin 限定)から呼ぶ。
    pub async fn list(
        &self,
        params: ListParams,
    ) -> Result<ListResult<WriteAuditEntry>, BantoError> {
        let columns = column_map();

        let mut rows_builder: QueryBuilder<Sqlite> = QueryBuilder::new(
            "SELECT id, ts, api_key_id, api_key_name_snapshot, api_key_prefix_snapshot, tag_id, external_name_snapshot, \
             value_requested, value_requested_text, action, result, detail FROM hub_write_audit",
        );
        banto_storage::list_query::sqlite::apply_list_params(&mut rows_builder, &columns, &params)?;
        let rows: Vec<WriteAuditEntry> = rows_builder
            .build_query_as::<WriteAuditEntry>()
            .fetch_all(&self.pool)
            .await
            .map_err(banto_storage::storage_error)?;

        let mut count_builder: QueryBuilder<Sqlite> =
            QueryBuilder::new("SELECT COUNT(*) FROM hub_write_audit");
        banto_storage::list_query::sqlite::append_where(
            &mut count_builder,
            &columns,
            &params.filters,
        )?;
        let total_count: i64 = count_builder
            .build_query_scalar()
            .fetch_one(&self.pool)
            .await
            .map_err(banto_storage::storage_error)?;

        Ok(ListResult {
            rows,
            total_count: total_count as u64,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrate_memory;

    async fn service() -> WriteAuditService {
        let pool = migrate_memory().await.expect("migrate_memory");
        WriteAuditService::new(pool)
    }

    fn sample_row() -> WriteAuditRow {
        WriteAuditRow::new(
            1,
            "mes-gateway",
            "bk_abc123",
            10,
            "line1.fast.temp01",
            WriteAuditAction::Write,
            WriteAuditResult::Ok,
        )
        .with_value_requested(42.5)
    }

    #[tokio::test]
    async fn insert_row_then_list_round_trips() {
        let svc = service().await;
        svc.insert_row(&sample_row()).await.unwrap();

        let result = svc.list(ListParams::default()).await.unwrap();
        assert_eq!(result.total_count, 1);
        let row = &result.rows[0];
        assert_eq!(row.api_key_id, 1);
        assert_eq!(row.api_key_name_snapshot, "mes-gateway");
        assert_eq!(row.tag_id, 10);
        assert_eq!(row.external_name_snapshot, "line1.fast.temp01");
        assert_eq!(row.value_requested, Some(42.5));
        assert_eq!(row.action, "write");
        assert_eq!(row.result, "ok");
    }

    /// #713: キーを削除した後も、監査行から名前とプレフィックスが読める。
    /// 監査行は**キーを削除した後に**挿入する（認証のときのプレフィックスを
    /// 持たせる。#940 のレビュー: 書込みの途中でキーが削除されても NULL にならない）。
    #[tokio::test]
    async fn name_and_prefix_stay_readable_after_the_key_is_deleted() {
        let pool = migrate_memory().await.expect("migrate_memory");
        let keys = crate::api_keys::ApiKeysService::new(pool.clone());
        let issued = keys
            .issue("gone-soon", vec!["write:c.g.t".into()], None)
            .await
            .unwrap();
        let svc = WriteAuditService::new(pool);

        keys.revoke(issued.id).await.unwrap();
        assert!(matches!(
            keys.delete(issued.id).await.unwrap(),
            crate::api_keys::ApiKeyDeleteOutcome::Deleted(_)
        ));

        let row = WriteAuditRow::new(
            issued.id,
            "gone-soon",
            issued.prefix.clone(),
            10,
            "line1.fast.temp01",
            WriteAuditAction::Write,
            WriteAuditResult::Ok,
        );
        svc.insert_row(&row).await.unwrap();

        let listed = svc.list(ListParams::default()).await.unwrap();
        assert_eq!(listed.rows[0].api_key_name_snapshot, "gone-soon");
        assert_eq!(
            listed.rows[0].api_key_prefix_snapshot.as_deref(),
            Some(issued.prefix.as_str())
        );
    }

    #[tokio::test]
    async fn pending_write_starts_failed_then_flips_ok() {
        let svc = service().await;
        let row = sample_row();
        let id = svc.insert_pending(&row).await.unwrap();

        let before: String = sqlx::query_scalar("SELECT result FROM hub_write_audit WHERE id = ?")
            .bind(id)
            .fetch_one(&svc.pool)
            .await
            .unwrap();
        assert_eq!(before, "failed", "pending row is provisionally failed");

        svc.set_result(id, WriteAuditResult::Ok, None)
            .await
            .unwrap();
        let after: String = sqlx::query_scalar("SELECT result FROM hub_write_audit WHERE id = ?")
            .bind(id)
            .fetch_one(&svc.pool)
            .await
            .unwrap();
        assert_eq!(after, "ok");
    }

    /// T8-2 (docs/tag-server-design.md §6.1, 2026-08-06): `set_result` also
    /// records a failure detail, so a T8 RMW confirmation-read mismatch's
    /// 「書き戻し競合の可能性があります」ends up on the confirmed audit row,
    /// not just the pending one this test's sibling
    /// (`pending_write_left_failed_if_never_confirmed`) leaves blank.
    #[tokio::test]
    async fn set_result_records_a_failure_detail() {
        let svc = service().await;
        let id = svc.insert_pending(&sample_row()).await.unwrap();

        svc.set_result(
            id,
            WriteAuditResult::Failed,
            Some("ビット書き込みの確認読みで不一致を検出しました。書き戻し競合の可能性があります"),
        )
        .await
        .unwrap();

        let result = svc.list(ListParams::default()).await.unwrap();
        assert_eq!(result.rows[0].result, "failed");
        assert_eq!(
            result.rows[0].detail.as_deref(),
            Some("ビット書き込みの確認読みで不一致を検出しました。書き戻し競合の可能性があります")
        );
    }

    /// #898: 送ったあとで結果が分からない書込みは `uncertain` で確定でき、
    /// 一覧で `failed` と別に絞り込める。
    #[tokio::test]
    async fn uncertain_is_kept_apart_from_failed_in_the_list() {
        let svc = service().await;
        let failed = svc.insert_pending(&sample_row()).await.unwrap();
        svc.set_result(failed, WriteAuditResult::Failed, Some("PLC のエラー"))
            .await
            .unwrap();
        let uncertain = svc.insert_pending(&sample_row()).await.unwrap();
        svc.set_result(
            uncertain,
            WriteAuditResult::Uncertain,
            Some("応答タイムアウト"),
        )
        .await
        .unwrap();

        let only = |value: &str| ListParams {
            filters: vec![banto_core::FilterState {
                field: "result".to_string(),
                op: banto_core::FilterOp::Eq,
                value: serde_json::json!(value),
            }],
            ..Default::default()
        };
        let listed = svc.list(only("uncertain")).await.unwrap();
        assert_eq!(listed.total_count, 1);
        assert_eq!(listed.rows[0].id, uncertain);
        assert_eq!(listed.rows[0].detail.as_deref(), Some("応答タイムアウト"));
        let listed = svc.list(only("failed")).await.unwrap();
        assert_eq!(listed.total_count, 1);
        assert_eq!(listed.rows[0].id, failed);
    }

    #[tokio::test]
    async fn pending_write_left_failed_if_never_confirmed() {
        // Simulates "the process died mid-write": insert_pending runs, but
        // set_result never does. The row must stay 'failed' - the safe
        // interpretation (this module's doc comment).
        let svc = service().await;
        let id = svc.insert_pending(&sample_row()).await.unwrap();
        let result: String = sqlx::query_scalar("SELECT result FROM hub_write_audit WHERE id = ?")
            .bind(id)
            .fetch_one(&svc.pool)
            .await
            .unwrap();
        assert_eq!(result, "failed");
    }

    #[tokio::test]
    async fn suppressed_rows_do_not_go_through_pending() {
        let svc = service().await;
        let row = WriteAuditRow::new(
            2,
            "writer-only",
            "bk_def456",
            10,
            "line1.fast.temp01",
            WriteAuditAction::RateLimitTripped,
            WriteAuditResult::SuppressedRateLimited,
        )
        .with_detail("rate limit exceeded; key tripped");
        svc.insert_row(&row).await.unwrap();

        let result = svc.list(ListParams::default()).await.unwrap();
        assert_eq!(result.rows[0].action, "rate_limit_tripped");
        assert_eq!(result.rows[0].result, "suppressed_rate_limited");
        assert_eq!(
            result.rows[0].detail.as_deref(),
            Some("rate limit exceeded; key tripped")
        );
    }

    /// T20 宿題#1: 文字列書き込み(`hub_write_audit.value_requested` は
    /// `REAL` 専用のため NULL のまま)でも、専用列 `value_requested_text` に
    /// 実際のテキストが残ることを確認する(`RequestedValue::Str`/
    /// `ConvertedValue::Str` の呼び出し元 `crate::write_path` が組み立てる
    /// 行を模した形 - `with_value_requested_opt(None)` + テキストを渡す
    /// `with_value_requested_text`)。
    #[tokio::test]
    async fn string_write_leaves_value_requested_null_but_records_text() {
        let svc = service().await;
        let row = WriteAuditRow::new(
            1,
            "mes-gateway",
            "bk_abc123",
            10,
            "line1.recipe_name",
            WriteAuditAction::Write,
            WriteAuditResult::Ok,
        )
        .with_value_requested_opt(None)
        .with_value_requested_text(Some("書いた文字列".to_string()));
        svc.insert_row(&row).await.unwrap();

        let result = svc.list(ListParams::default()).await.unwrap();
        assert_eq!(result.total_count, 1);
        let stored = &result.rows[0];
        assert_eq!(stored.value_requested, None);
        assert_eq!(stored.value_requested_text.as_deref(), Some("書いた文字列"));
    }

    #[tokio::test]
    async fn list_filters_by_tag_id() {
        let svc = service().await;
        svc.insert_row(&sample_row()).await.unwrap();
        let mut other = sample_row();
        other.tag_id = 20;
        other.external_name_snapshot = "line1.fast.temp02".to_string();
        svc.insert_row(&other).await.unwrap();

        let result = svc
            .list(ListParams {
                filters: vec![banto_core::FilterState {
                    field: "tagId".to_string(),
                    op: banto_core::FilterOp::Eq,
                    value: serde_json::json!(20),
                }],
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(result.total_count, 1);
        assert_eq!(result.rows[0].tag_id, 20);
    }
}

#[cfg(test)]
mod rejection_tests {
    use super::*;
    use crate::db::migrate_memory;

    /// ワーカーの処理を直接呼ぶ試験の組(時刻を注入する)。
    struct Rec {
        svc: WriteAuditService,
        w: RejectionWorker,
    }

    impl Rec {
        #[allow(clippy::too_many_arguments)] // 時刻の注入で 1 つ増える試験用の形
        async fn record_rejection_at(
            &mut self,
            now: Instant,
            api_key_id: i64,
            api_key_name: &str,
            api_key_prefix: &str,
            tag_id: i64,
            external_name: &str,
            reason: WriteRejectionReason,
        ) {
            let e = RejectionEntry {
                api_key_id,
                api_key_name: api_key_name.to_string(),
                api_key_prefix: api_key_prefix.to_string(),
                tag_id,
                external_name: external_name.to_string(),
                reason,
            };
            let (queued, _) = coalesce_rejections(vec![e]);
            self.w.record_at(now, &queued[0]).await;
        }
    }

    async fn service() -> Rec {
        let pool = migrate_memory().await.expect("migrate_memory");
        Rec {
            svc: WriteAuditService::new(pool.clone()),
            w: RejectionWorker::new(pool),
        }
    }

    async fn rows(svc: &WriteAuditService) -> Vec<WriteAuditEntry> {
        let mut rows = svc.list(ListParams::default()).await.unwrap().rows;
        rows.sort_by_key(|r| r.id);
        rows
    }

    #[tokio::test]
    async fn repeated_rejection_inside_the_window_is_one_row_with_the_total() {
        let mut svc = service().await;
        let t0 = Instant::now();
        let r = WriteRejectionReason::NotWritable;
        for i in 0..13 {
            svc.record_rejection_at(t0 + Duration::from_secs(i), 1, "k", "bk_k", 5, "a.b", r)
                .await;
        }
        // 拒否が止まっても、何も呼ばずに読んで件数が残っている。
        let got = rows(&svc.svc).await;
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].result, "rejected");
        assert_eq!(got[0].api_key_prefix_snapshot.as_deref(), Some("bk_k"));
        assert_eq!(got[0].action, "write");
        assert_eq!(
            got[0].detail.as_deref(),
            Some("not_writable（60 秒の窓で計 13 件）")
        );
    }

    #[tokio::test]
    async fn a_single_rejection_has_only_the_code() {
        let mut svc = service().await;
        let r = WriteRejectionReason::NotFound;
        svc.record_rejection_at(Instant::now(), 1, "k", "bk_k", 0, "x", r)
            .await;
        assert_eq!(rows(&svc.svc).await[0].detail.as_deref(), Some("not_found"));
    }

    #[tokio::test]
    async fn a_new_window_is_a_new_row_and_keeps_the_old_total() {
        let mut svc = service().await;
        let t0 = Instant::now();
        let r = WriteRejectionReason::NotWritable;
        for i in 0..5 {
            svc.record_rejection_at(t0 + Duration::from_secs(i), 1, "k", "bk_k", 5, "a.b", r)
                .await;
        }
        svc.record_rejection_at(t0 + REJECTION_WINDOW, 1, "k", "bk_k", 5, "a.b", r)
            .await;
        svc.record_rejection_at(
            t0 + REJECTION_WINDOW + Duration::from_secs(1),
            1,
            "k",
            "bk_k",
            5,
            "a.b",
            r,
        )
        .await;
        let got = rows(&svc.svc).await;
        assert_eq!(got.len(), 2);
        assert_eq!(
            got[0].detail.as_deref(),
            Some("not_writable（60 秒の窓で計 5 件）")
        );
        assert_eq!(
            got[1].detail.as_deref(),
            Some("not_writable（60 秒の窓で計 2 件）")
        );
    }

    #[tokio::test]
    async fn counts_survive_when_the_memory_drops_a_pair_at_the_cap() {
        let mut svc = service().await;
        let t0 = Instant::now();
        let r = WriteRejectionReason::NotFound;
        // 最初の組に 3 件。
        for _ in 0..3 {
            svc.record_rejection_at(t0, 1, "k", "bk_k", 0, "first", r)
                .await;
        }
        // 上限を超える数の別の組(キーごとの行の上限に掛からないよう、別々のキー)で、
        // 最初の組を記憶から捨てさせる。
        for i in 0..REJECTION_THROTTLE_MAX_ENTRIES {
            svc.record_rejection_at(
                t0 + Duration::from_millis(1 + i as u64),
                2 + i as i64,
                "k",
                "bk_k",
                0,
                &format!("tag{i}"),
                r,
            )
            .await;
        }
        assert!(svc.w.windows.len() <= REJECTION_THROTTLE_MAX_ENTRIES);
        assert!(svc.w.budgets.len() <= REJECTION_THROTTLE_MAX_ENTRIES);
        let got = svc.svc.list(ListParams::default()).await.unwrap().rows;
        let first = got
            .iter()
            .find(|e| e.external_name_snapshot == "first")
            .unwrap();
        assert_eq!(
            first.detail.as_deref(),
            Some("not_found（60 秒の窓で計 3 件）")
        );
    }

    #[tokio::test]
    async fn different_key_tag_or_reason_are_recorded_separately() {
        let mut svc = service().await;
        let t0 = Instant::now();
        let r = WriteRejectionReason::NotWritable;
        svc.record_rejection_at(t0, 1, "k", "bk_k", 5, "a.b", r)
            .await;
        svc.record_rejection_at(t0, 2, "k2", "bk_k", 5, "a.b", r)
            .await;
        svc.record_rejection_at(t0, 1, "k", "bk_k", 6, "a.c", r)
            .await;
        svc.record_rejection_at(
            t0,
            1,
            "k",
            "bk_k",
            5,
            "a.b",
            WriteRejectionReason::MissingWriteScope,
        )
        .await;
        assert_eq!(rows(&svc.svc).await.len(), 4);
    }

    #[tokio::test]
    async fn one_key_with_many_tag_names_stops_at_the_cap_plus_a_summary_row() {
        let mut svc = service().await;
        let t0 = Instant::now();
        let r = WriteRejectionReason::MissingWriteScope;
        let total = REJECTION_ROWS_PER_KEY + 30;
        for i in 0..total {
            svc.record_rejection_at(
                t0 + Duration::from_millis(i as u64),
                1,
                "k",
                "bk_k",
                0,
                &format!("tag{i}"),
                r,
            )
            .await;
        }
        // 上限の中のタグ名を繰り返しても、そのタグの行に数える(まとめには入らない)。
        svc.record_rejection_at(t0 + Duration::from_secs(1), 1, "k", "bk_k", 0, "tag0", r)
            .await;
        let got = rows(&svc.svc).await;
        assert_eq!(got.len(), REJECTION_ROWS_PER_KEY + 1);
        let summary: Vec<_> = got
            .iter()
            .filter(|e| e.external_name_snapshot == REJECTION_SUMMARY_NAME)
            .collect();
        assert_eq!(summary.len(), 1);
        assert_eq!(summary[0].tag_id, 0);
        assert_eq!(summary[0].api_key_id, 1);
        assert_eq!(
            summary[0].detail.as_deref(),
            Some("missing_write_scope（60 秒の窓で、上限を超えたほかのタグ名で計 30 件）")
        );
        assert_eq!(
            got[0].detail.as_deref(),
            Some("missing_write_scope（60 秒の窓で計 2 件）")
        );
        // ほかのキーは別の予算。
        svc.record_rejection_at(t0, 2, "k2", "bk_k2", 0, "tagx", r)
            .await;
        assert_eq!(rows(&svc.svc).await.len(), REJECTION_ROWS_PER_KEY + 2);
        // 窓が明けたら、同じキーでもタグごとの行に戻る。
        svc.record_rejection_at(t0 + REJECTION_WINDOW, 1, "k", "bk_k", 0, "late", r)
            .await;
        let got = rows(&svc.svc).await;
        assert_eq!(got.len(), REJECTION_ROWS_PER_KEY + 3);
        assert_eq!(got.last().unwrap().external_name_snapshot, "late");
    }

    #[tokio::test]
    async fn record_rejection_returns_before_the_worker_writes() {
        let pool = migrate_memory().await.expect("migrate_memory");
        let svc = WriteAuditService::new(pool.clone());
        let r = WriteRejectionReason::NotFound;
        // DB の接続をふさいだままでも、呼び出し(同期。await しない)は戻る。
        let held = pool.acquire().await.expect("acquire");
        svc.record_rejection(1, "k", "bk_k", 0, "x", r);
        svc.record_rejections(vec![
            RejectionEntry {
                api_key_id: 1,
                api_key_name: "k".into(),
                api_key_prefix: "bk_k".into(),
                tag_id: 0,
                external_name: "x".into(),
                reason: r,
            };
            2
        ]);
        // ワーカーを走らせても、接続を待つだけで呼び出し側は止まらない。
        tokio::task::yield_now().await;
        drop(held);
        svc.flush().await;
        let got = rows(&svc).await;
        assert_eq!(got.len(), 1);
        assert_eq!(
            got[0].detail.as_deref(),
            Some("not_found（60 秒の窓で計 3 件）")
        );
    }

    #[tokio::test]
    async fn queue_overflow_is_left_as_a_summary_row() {
        let pool = migrate_memory().await.expect("migrate_memory");
        let svc =
            WriteAuditService::with_rejection_queue_limits(pool, 2, REJECTION_QUEUE_MAX_ENTRIES);
        let r = WriteRejectionReason::NotWritable;
        // ワーカーが走る前に容量 2 を超えて渡す: 2 件はキューへ、3 件はあふれ。
        for i in 0..5 {
            svc.record_rejection(1, "k", "bk_k", 5, &format!("t{i}"), r);
        }
        svc.flush().await;
        let got = rows(&svc).await;
        assert_eq!(got.len(), 3);
        let overflow: Vec<_> = got
            .iter()
            .filter(|e| e.detail.as_deref() == Some("queue_overflow（3 件を記録できなかった）"))
            .collect();
        assert_eq!(overflow.len(), 1);
        assert_eq!(overflow[0].api_key_id, 0);
        assert_eq!(overflow[0].tag_id, 0);
        assert_eq!(overflow[0].external_name_snapshot, REJECTION_SUMMARY_NAME);
        assert_eq!(overflow[0].result, "rejected");
        // 件数は 0 に戻っている(次の flush で行が増えない)。
        svc.flush().await;
        assert_eq!(rows(&svc).await.len(), 3);
    }

    fn entry(name: &str, reason: WriteRejectionReason) -> RejectionEntry {
        RejectionEntry {
            api_key_id: 1,
            api_key_name: "k".into(),
            api_key_prefix: "bk_k".into(),
            tag_id: 0,
            external_name: name.into(),
            reason,
        }
    }

    #[tokio::test]
    async fn the_same_tag_100k_times_is_one_entry_and_one_row() {
        let r = WriteRejectionReason::MissingWriteScope;
        let entries = vec![entry("x", r); 100_000];
        // 送る前に 1 エントリ(件数 10 万)にまとまる。
        let (queued, dropped) = coalesce_rejections(entries.clone());
        assert_eq!(dropped, 0);
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].count, 100_000);
        let pool = migrate_memory().await.expect("migrate_memory");
        let svc = WriteAuditService::new(pool);
        svc.record_rejections(entries);
        svc.flush().await;
        let got = rows(&svc).await;
        assert_eq!(got.len(), 1);
        assert_eq!(
            got[0].detail.as_deref(),
            Some("missing_write_scope（60 秒の窓で計 100000 件）")
        );
        assert_eq!(svc.rejections.pending.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn a_batch_of_1000_tag_names_is_capped_per_message() {
        let r = WriteRejectionReason::MissingWriteScope;
        let entries: Vec<_> = (0..1000).map(|i| entry(&format!("t{i}"), r)).collect();
        let (queued, dropped) = coalesce_rejections(entries.clone());
        assert_eq!(dropped, 0);
        assert_eq!(queued.len(), REJECTION_MESSAGE_MAX_TAGS + 1);
        let summary: Vec<_> = queued
            .iter()
            .filter(|q| q.target == RejectionTarget::OtherTags)
            .collect();
        assert_eq!(summary.len(), 1);
        assert_eq!(summary[0].count as usize, 1000 - REJECTION_MESSAGE_MAX_TAGS);
        assert_eq!(queued_total(&queued), 1000);

        let pool = migrate_memory().await.expect("migrate_memory");
        let svc = WriteAuditService::new(pool);
        svc.record_rejections(entries);
        svc.flush().await;
        let got = rows(&svc).await;
        // キーごとの行の上限 + まとめの行 1 つ。
        assert_eq!(got.len(), REJECTION_ROWS_PER_KEY + 1);
        let over = 1000 - REJECTION_ROWS_PER_KEY;
        let summary_row: Vec<_> = got
            .iter()
            .filter(|e| e.external_name_snapshot == REJECTION_SUMMARY_NAME)
            .collect();
        assert_eq!(summary_row.len(), 1);
        assert_eq!(
            summary_row[0].detail.clone().unwrap(),
            format!("missing_write_scope（60 秒の窓で、上限を超えたほかのタグ名で計 {over} 件）")
        );
        // タグごとの行は 1 件ずつ: 合計は 1000 件。
        let tag_rows = got
            .iter()
            .filter(|e| e.detail.as_deref() == Some("missing_write_scope"))
            .count();
        assert_eq!(tag_rows + over, 1000);
    }

    #[tokio::test]
    async fn summaries_beyond_the_cap_go_to_the_overflow() {
        // 多数のキー(呼び出しはふつう 1 キーだが)の、まとめのエントリの上限。
        let r = WriteRejectionReason::NotFound;
        let mut entries: Vec<_> = (0..REJECTION_MESSAGE_MAX_TAGS)
            .map(|i| entry(&format!("t{i}"), r))
            .collect();
        for key in 0..(REJECTION_MESSAGE_MAX_SUMMARIES as i64 + 3) {
            let mut e = entry("other", r);
            e.api_key_id = 100 + key;
            entries.push(e.clone());
            entries.push(e);
        }
        let (queued, dropped) = coalesce_rejections(entries);
        assert_eq!(
            queued.len(),
            REJECTION_MESSAGE_MAX_TAGS + REJECTION_MESSAGE_MAX_SUMMARIES
        );
        assert_eq!(dropped, 6);
    }

    #[tokio::test]
    async fn a_message_over_the_queue_budget_is_not_sent_and_counted_as_overflow() {
        let pool = migrate_memory().await.expect("migrate_memory");
        let budget = REJECTION_MESSAGE_MAX_TAGS + 8;
        let svc =
            WriteAuditService::with_rejection_queue_limits(pool, REJECTION_QUEUE_CAPACITY, budget);
        let r = WriteRejectionReason::NotWritable;
        // ワーカーが走る前に渡す(同期の呼び出しで await しない)。
        // 1 つ目: 32 エントリ + まとめ 1 = 33 エントリ(予算 40 の中)。
        let first: Vec<_> = (0..100).map(|i| entry(&format!("a{i}"), r)).collect();
        svc.record_rejections(first);
        assert_eq!(
            svc.rejections.pending.load(Ordering::Relaxed),
            REJECTION_MESSAGE_MAX_TAGS + 1
        );
        // 2 つ目: 10 エントリ(計 43 で予算を超える)は送らず、10 件をあふれに。
        let second: Vec<_> = (0..10).map(|i| entry(&format!("b{i}"), r)).collect();
        svc.record_rejections(second);
        assert_eq!(svc.rejections.overflow.load(Ordering::Relaxed), 10);
        assert_eq!(
            svc.rejections.pending.load(Ordering::Relaxed),
            REJECTION_MESSAGE_MAX_TAGS + 1
        );
        svc.flush().await;
        assert_eq!(svc.rejections.pending.load(Ordering::Relaxed), 0);
        let got = rows(&svc).await;
        assert!(got
            .iter()
            .all(|e| !e.external_name_snapshot.starts_with('b')));
        let overflow: Vec<_> = got
            .iter()
            .filter(|e| e.detail.as_deref() == Some("queue_overflow（10 件を記録できなかった）"))
            .collect();
        assert_eq!(overflow.len(), 1);
        // 1 つ目は記録されている(タグごとの行 + まとめの行)。
        assert_eq!(got.len(), REJECTION_ROWS_PER_KEY + 1 + 1);
        // 予算が戻ったので、次は送れる(キーの行の上限を超えているので、まとめの行に
        // 数える: 100 - 20 = 80 件 + 1 件)。
        svc.record_rejection(1, "k", "bk_k", 0, "c", r);
        svc.flush().await;
        let summary = rows(&svc)
            .await
            .into_iter()
            .find(|e| e.api_key_id == 1 && e.external_name_snapshot == REJECTION_SUMMARY_NAME)
            .unwrap();
        assert_eq!(
            summary.detail.as_deref(),
            Some("not_writable（60 秒の窓で、上限を超えたほかのタグ名で計 81 件）")
        );
    }

    #[tokio::test]
    async fn a_long_external_name_is_truncated_with_a_mark() {
        let exact = "あ".repeat(REJECTION_NAME_MAX_CHARS);
        assert_eq!(truncate_name(exact.clone()), exact);
        let long = "あ".repeat(REJECTION_NAME_MAX_CHARS + 50);
        let cut = truncate_name(long.clone());
        assert_eq!(cut.chars().count(), REJECTION_NAME_MAX_CHARS);
        assert!(cut.ends_with('…'));
        assert!(long.starts_with(cut.trim_end_matches('…')));

        let pool = migrate_memory().await.expect("migrate_memory");
        let svc = WriteAuditService::new(pool);
        let r = WriteRejectionReason::NotFound;
        // 先頭が同じ長い名前どうしは、切り詰めた後で同じ組になる。
        let other = format!("{}い", "あ".repeat(REJECTION_NAME_MAX_CHARS + 10));
        svc.record_rejections(vec![entry(&long, r), entry(&other, r)]);
        svc.flush().await;
        let got = rows(&svc).await;
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].external_name_snapshot, cut);
        assert_eq!(
            got[0].detail.as_deref(),
            Some("not_found（60 秒の窓で計 2 件）")
        );
    }

    #[test]
    fn a_queued_entry_stays_small_for_the_memory_estimate() {
        // モジュールの doc comment のメモリの見積もり(1 エントリの構造体 128 バイト以下)。
        assert!(std::mem::size_of::<QueuedRejection>() <= 128);
    }
}
