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
//! `external_name_snapshot` に残す。物理書き込みは試みないので pending を経ず、
//! 下の「拒否の表」に件数を足すだけで完結する([`WriteAuditService::record_rejection`])。
//! タグが重複したバッチ(`DuplicateTagInBatch`)は 4 つの理由の外なので残らない
//! (#945 の総点検 P3-6)。
//!
//! ### 拒否の表と周期の書き手 (#945 の総点検、2026-10-30 オーナー決定)
//!
//! 監査が拒否で埋まらないよう、**同じ(API キー, 外部名, 理由)は 60 秒
//! ([`REJECTION_WINDOW`])の窓に 1 行**にまとめる(例: `not_writable（60 秒の窓で計 13 件）`。
//! 1 件のときはコードだけ)。窓が明けた次の拒否は新しい行で窓を始め直す。
//!
//! - **要求の経路**([`WriteAuditService::record_rejections`]): async ではなく、
//!   `std::sync::Mutex` の表を 1 回だけ lock して件数を足すだけ(DB にも async の
//!   ロックにも触れない)。403・404 の応答は変えず、監査で遅らせない。バッチは
//!   1 回の lock で全件を足し、呼び手は `tag_map()` を呼び出しごとに 1 回だけ取る。
//! - **書き手**([`WriteAuditService::spawn_rejection_writer`]、`HubRuntime::start` が
//!   起動): [`REJECTION_WRITE_PERIOD`](2 秒)ごとに、表を lock して書き終えていない組の
//!   写しを取り、lock を放してから 1 つのトランザクションで書く。新しい窓は INSERT
//!   (`ts` は窓の最初の拒否の時刻)、行のある窓は `detail` を UPDATE(0 行なら INSERT
//!   し直す)。1 周期は [`REJECTION_WRITE_MAX_PAIRS`](256)組まで、残りは次の周期へ。
//!   DB の操作は「書き終えていない組の数 ÷ 周期」で頭打ちになり、要求の頻度とは
//!   切り離される(同じ組を 1 万回足しても、1 周期の UPDATE は 1 回)。失敗したら
//!   表を変えずに次の周期で書き直す(件数を失わない。ログは失敗が続く間の最初の
//!   1 回と、戻ったときだけ)。窓が明けて書き終えた組は表から消す。
//! - **件数の遅れと失う範囲**: 件数は周期ぶん(最大 2 秒と 1 回の書込みの時間)遅れて
//!   DB に入る。プロセスが落ちたとき(電源断・強制終了)は、最後の周期のあとに足した
//!   分を失う。正常な終了では書き切る(下の「終了」)。
//! - **書き手の死**: 書き手のタスクが panic したら、見張りのタスクがログに出して
//!   起こし直す(待ちは 2 秒から倍々で 60 秒まで)。件数は表に残っているので、起こし
//!   直した書き手が書く。見張りごと止まった(ランタイムの終了など)ときは、要求の経路が
//!   書き手の最後の周期の時刻を見て、[`REJECTION_WRITER_STALE`](30 秒)を過ぎていれば
//!   ログに 1 回出す。
//!
//! **表の大きさの上限**(メモリを抱えないように):
//!
//! - 組は [`REJECTION_TABLE_MAX_PAIRS`](1000)まで。満ちたら窓の明けた組を外す
//!   (書き終えていない組は「明けた組」の置き場([`REJECTION_TABLE_MAX_CLOSED`]、1000)へ
//!   移して書き切る)。
//! - **キーごとの行の上限**: 1 つの API キーが窓の中で別々の行にできる(外部名, 理由)の
//!   組は [`REJECTION_ROWS_PER_KEY`](20)まで。超えた拒否は、そのキーと理由ごとの
//!   **まとめの行**(`external_name_snapshot = '*'`、`tag_id = 0`、例:
//!   `not_found（60 秒の窓で、上限を超えたほかのタグ名で計 7 件）`)に数える。キーの予算の
//!   窓と組ごとの窓は別々に始まるので、窓の境界をまたぐと 1 つのキーの行は最大で
//!   およそ 2 倍(上限の 2 倍 + 理由の数)になる(P3-4)。予算の記憶もキー 1000 個まで
//!   (超えて捨てたキーは予算を数え直す)。
//! - 新しい組もまとめの組も表に入らなければ、件数を**あふれ**に足し、書き手が
//!   **あふれの行**(`api_key_id = 0`・キー名とプレフィックスは空・
//!   `external_name_snapshot = '*'`・`tag_id = 0`・
//!   `detail = 'overflow（N 件を記録できなかった）'`)を 1 行残す(どのキーの拒否かは
//!   数えない)。置き場が満ちたときの、書いていない件数もここに足す。
//! - 外部名とキー名は [`REJECTION_NAME_MAX_CHARS`](256 文字)までに切り詰め、末尾を `…`
//!   にして監査に残す。先頭が同じ長い名前どうしは、切り詰めた後で同じ組として数えうる。
//! - **メモリの見積もり**: 組 1 つは構造体と名前(外部名は UTF-8 で最大 1023 バイト。
//!   キー名とプレフィックスは呼び出しごとに `Arc<str>` で共有)で、最悪およそ 1.2 KB。
//!   開いた組と置き場を合わせて 2000 組、およそ 2.5 MB に収まる。
//!
//! **終了**: `RunningHub::shutdown_and_report` が書込みの入口を止めたあと、設定 DB の
//! pool を閉じる前に [`RejectionWriter::shutdown`] で書き手を止め(書いている周期は
//! 終わらせる)、表の残りをすべて書き切る(上限 `WRITE_AUDIT_FLUSH_TIMEOUT` = 5 秒)。
//! 時間切れなら、書けなかった件数を 1 行のログにまとめて先へ進む。
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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use banto_core::{BantoError, ListParams, ListResult};
use banto_storage::ColumnMap;
use serde::Serialize;
use sqlx::{QueryBuilder, Sqlite, SqlitePool};
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

/// #945 の総点検: 拒否の表に持つ組(窓)の上限。満ちたら、窓が明けた組を表から外して
/// 場所を空ける(書き終えていない組は「明けた組」の置き場へ移すので件数は失われない)。
/// それでも空かなければ、(キー, 理由)ごとのまとめの組に足し、それも作れなければ
/// あふれの件数に足す。キーごとの行の予算([`REJECTION_ROWS_PER_KEY`])の記憶も同じ
/// 上限で持つ。
pub const REJECTION_TABLE_MAX_PAIRS: usize = 1000;

/// #945 の総点検: 窓が明けたが、まだ DB に書き終えていない組の置き場の上限(DB の
/// 書込みが続けて失敗しているときだけ溜まる)。超えた組の書いていない件数はあふれに足す。
pub const REJECTION_TABLE_MAX_CLOSED: usize = 1000;

/// #945 のレビュー: 1 つの API キーが、[`REJECTION_WINDOW`] の中で別々の行として
/// 残せる(外部名, 理由)の組の数。超えた拒否は、そのキーと理由ごとの
/// まとめの行(外部名 `*`、`tag_id` 0)に数える。キーの予算の窓とタグの組の窓は
/// 別々に始まるので、窓の境界をまたぐと 1 つのキーの行は最大でおよそ 2 倍になる。
pub const REJECTION_ROWS_PER_KEY: usize = 20;

/// #945 のレビュー: 表に持つ外部名・キー名の長さの上限(文字数)。超えた名前は
/// 先頭の `上限 - 1` 文字に `…` を付けて切り詰め、監査にもその形で残す。切り詰めた
/// 名前どうしは、まとめのキーとして同じ組になりうる(先頭が同じ長い名前は 1 行に
/// 数える)。
pub const REJECTION_NAME_MAX_CHARS: usize = 256;

/// まとめの行・あふれの行の外部名。
pub const REJECTION_SUMMARY_NAME: &str = "*";

/// #945 の総点検: 拒否の表を DB へ書く周期。件数はこの周期ぶん遅れて DB に入る。
pub const REJECTION_WRITE_PERIOD: Duration = Duration::from_secs(2);

/// #945 の総点検: 1 周期(1 つのトランザクション)で書く組の上限。残りは次の周期へ回す。
pub const REJECTION_WRITE_MAX_PAIRS: usize = 256;

/// #945 の総点検: 書き手の最後の周期からこれだけ経っても次の周期が来なければ、
/// 拒否を足す側(要求の経路)が「書き手が止まっている」とログに 1 回出す。
pub const REJECTION_WRITER_STALE: Duration = Duration::from_secs(30);

/// 書き手のタスクが panic したあと起こし直すまでの待ち(最初)。続けて panic する
/// たびに倍にし、[`REJECTION_WRITER_RESTART_MAX`] で頭打ちにする。
const REJECTION_WRITER_RESTART_MIN: Duration = Duration::from_secs(2);
const REJECTION_WRITER_RESTART_MAX: Duration = Duration::from_secs(60);

/// #711: 行をまとめる相手。タグごとの行か、キーごとの上限を超えた分のまとめの行か。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum RejectionTarget {
    Tag(String),
    OtherTags,
}

type PairKey = (i64, RejectionTarget, WriteRejectionReason);

/// #945 の総点検: 組の 1 つの窓(表の 1 行ぶん)。
#[derive(Debug, Clone)]
struct PairWindow {
    /// 窓を見分ける通し番号(書き手が書いている間に窓が明けて置き場へ移っても、
    /// 書けた結果を正しい窓へ戻すため)。
    generation: u64,
    /// 窓の最初の拒否の時刻(窓はここから [`REJECTION_WINDOW`])。
    start: Instant,
    /// 窓の最初の拒否の時計の時刻(行の `ts` に入れる)。
    first_at: SystemTime,
    /// 窓の中の拒否の合計。
    count: u64,
    /// DB の行に書き終えた件数(`count` と違えば書き直しが要る)。
    written: u64,
    /// 挿入した行の id(まだ挿入していなければ `None`)。
    row_id: Option<i64>,
    key_name: Arc<str>,
    key_prefix: Arc<str>,
    tag_id: i64,
}

impl PairWindow {
    fn dirty(&self) -> bool {
        self.row_id.is_none() || self.written != self.count
    }

    fn expired(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.start) >= REJECTION_WINDOW
    }
}

/// #945 のレビュー: キーごとの、窓の中で作ったタグごとの組の数。
#[derive(Debug, Clone, Copy)]
struct KeyBudget {
    start: Instant,
    rows: usize,
}

/// #945 の総点検: 拒否の表。要求の経路は `std::sync::Mutex` を 1 回取って件数を足す
/// だけ(DB にも async のロックにも触れない)。書き手が周期ごとに書き出す。
#[derive(Debug, Default)]
struct RejectionTable {
    /// いま開いている窓。
    open: HashMap<PairKey, PairWindow>,
    /// 窓が明けたが、まだ書き終えていない組(書き終えたら消す)。
    closed: Vec<(PairKey, PairWindow)>,
    budgets: HashMap<i64, KeyBudget>,
    /// 表に入らなかった件数(あふれの行に書く)。
    overflow: u64,
    next_generation: u64,
}

/// 要求の経路で、呼び出しごとに 1 回だけ作るキー名とプレフィックス。
struct KeyNames<'a> {
    key: RejectingKey<'a>,
    shared: Option<(Arc<str>, Arc<str>)>,
}

impl KeyNames<'_> {
    fn get(&mut self) -> (Arc<str>, Arc<str>) {
        let key = self.key;
        self.shared
            .get_or_insert_with(|| {
                (
                    Arc::from(truncate_name(key.name).as_ref()),
                    Arc::from(truncate_name(key.prefix).as_ref()),
                )
            })
            .clone()
    }
}

impl RejectionTable {
    /// 開いた窓に `n` 件を足す。窓が無ければ `false`。明けていたら置き場へ移して `false`。
    fn bump(&mut self, key: &PairKey, now: Instant, n: u64) -> bool {
        let Some(w) = self.open.get_mut(key) else {
            return false;
        };
        if w.expired(now) {
            self.retire(key);
            return false;
        }
        w.count = w.count.saturating_add(n);
        true
    }

    /// 開いた窓を表から外す。書き終えていなければ置き場へ移す(置き場が満ちていれば、
    /// 書いていない件数をあふれに足す)。
    fn retire(&mut self, key: &PairKey) {
        let Some(w) = self.open.remove(key) else {
            return;
        };
        if !w.dirty() {
            return;
        }
        if self.closed.len() < REJECTION_TABLE_MAX_CLOSED {
            self.closed.push((key.clone(), w));
        } else {
            let unwritten = if w.row_id.is_some() {
                w.count.saturating_sub(w.written)
            } else {
                w.count
            };
            self.overflow = self.overflow.saturating_add(unwritten);
        }
    }

    /// 新しい組を入れる場所があるか。満ちていたら、窓が明けた組を外して空ける。
    fn make_room(&mut self, now: Instant) -> bool {
        if self.open.len() < REJECTION_TABLE_MAX_PAIRS {
            return true;
        }
        let expired: Vec<PairKey> = self
            .open
            .iter()
            .filter(|(_, w)| w.expired(now))
            .map(|(k, _)| k.clone())
            .collect();
        for k in &expired {
            self.retire(k);
        }
        self.open.len() < REJECTION_TABLE_MAX_PAIRS
    }

    fn insert_window(
        &mut self,
        key: PairKey,
        now: Instant,
        wall: SystemTime,
        names: &mut KeyNames<'_>,
        tag_id: i64,
        n: u64,
    ) {
        let (key_name, key_prefix) = names.get();
        self.next_generation += 1;
        self.open.insert(
            key,
            PairWindow {
                generation: self.next_generation,
                start: now,
                first_at: wall,
                count: n,
                written: 0,
                row_id: None,
                key_name,
                key_prefix,
                tag_id,
            },
        );
    }

    /// キーの予算の中で、新しいタグの組を作れるか(作ったら [`Self::spend_budget`])。
    fn budget_allows(&self, key_id: i64, now: Instant) -> bool {
        match self.budgets.get(&key_id) {
            Some(b) if now.saturating_duration_since(b.start) < REJECTION_WINDOW => {
                b.rows < REJECTION_ROWS_PER_KEY
            }
            _ => true,
        }
    }

    fn spend_budget(&mut self, key_id: i64, now: Instant) {
        if let Some(b) = self.budgets.get_mut(&key_id) {
            if now.saturating_duration_since(b.start) < REJECTION_WINDOW {
                b.rows += 1;
                return;
            }
        }
        if self.budgets.len() >= REJECTION_TABLE_MAX_PAIRS && !self.budgets.contains_key(&key_id) {
            self.budgets
                .retain(|_, b| now.saturating_duration_since(b.start) < REJECTION_WINDOW);
            if self.budgets.len() >= REJECTION_TABLE_MAX_PAIRS {
                // 捨てたキーは次に予算を数え直す(モジュールの doc comment)。
                if let Some(oldest) = self
                    .budgets
                    .iter()
                    .min_by_key(|(_, b)| b.start)
                    .map(|(k, _)| *k)
                {
                    self.budgets.remove(&oldest);
                }
            }
        }
        self.budgets.insert(
            key_id,
            KeyBudget {
                start: now,
                rows: 1,
            },
        );
    }

    /// 拒否 `n` 件を足す(要求の経路。DB には触れない)。
    #[allow(clippy::too_many_arguments)] // 時刻 2 つと呼び出しごとの名前を渡す内部の形
    fn add(
        &mut self,
        now: Instant,
        wall: SystemTime,
        names: &mut KeyNames<'_>,
        tag_id: i64,
        external_name: &str,
        reason: WriteRejectionReason,
        n: u64,
    ) {
        let key_id = names.key.id;
        let tag_key: PairKey = (
            key_id,
            RejectionTarget::Tag(truncate_name(external_name).into_owned()),
            reason,
        );
        if self.bump(&tag_key, now, n) {
            return;
        }
        if self.budget_allows(key_id, now) && self.make_room(now) {
            self.insert_window(tag_key, now, wall, names, tag_id, n);
            self.spend_budget(key_id, now);
            return;
        }
        let summary_key: PairKey = (key_id, RejectionTarget::OtherTags, reason);
        if self.bump(&summary_key, now, n) {
            return;
        }
        if self.make_room(now) {
            self.insert_window(summary_key, now, wall, names, 0, n);
            return;
        }
        self.overflow = self.overflow.saturating_add(n);
    }

    /// 書き手のための写し(置き場の組から先に、上限 `max` 組まで)。lock の中で呼ぶ。
    fn take_jobs(&self, max: usize) -> (Vec<WriteJob>, u64) {
        let closed = self
            .closed
            .iter()
            .filter(|(_, w)| w.dirty())
            .map(|(k, w)| WriteJob::of(k, w));
        let open = self
            .open
            .iter()
            .filter(|(_, w)| w.dirty())
            .map(|(k, w)| WriteJob::of(k, w));
        (closed.chain(open).take(max).collect(), self.overflow)
    }

    /// 書けた結果を戻す(`row_id` と書いた件数)。窓が明けて置き場へ移っていても
    /// 通し番号で見つける。
    fn apply(&mut self, done: &[(WriteJob, i64)], overflow_written: u64) {
        for (job, row_id) in done {
            let w = match self.open.get_mut(&job.key) {
                Some(w) if w.generation == job.generation => Some(w),
                _ => self
                    .closed
                    .iter_mut()
                    .find(|(_, w)| w.generation == job.generation)
                    .map(|(_, w)| w),
            };
            if let Some(w) = w {
                w.row_id = Some(*row_id);
                w.written = job.count;
            }
        }
        self.overflow = self.overflow.saturating_sub(overflow_written);
    }

    /// 窓が明けて書き終えた組と、明けたキーの予算を消す。
    fn collect_garbage(&mut self, now: Instant) {
        self.open.retain(|_, w| w.dirty() || !w.expired(now));
        self.closed.retain(|(_, w)| w.dirty());
        self.budgets
            .retain(|_, b| now.saturating_duration_since(b.start) < REJECTION_WINDOW);
    }

    /// まだ DB に書いていない件数(終了のログ・試験用)。
    fn unwritten(&self) -> u64 {
        let pending = |w: &PairWindow| {
            if w.row_id.is_some() {
                w.count.saturating_sub(w.written)
            } else {
                w.count
            }
        };
        let open: u64 = self.open.values().map(pending).sum();
        let closed: u64 = self.closed.iter().map(|(_, w)| pending(w)).sum();
        open + closed + self.overflow
    }

    fn dirty_pairs(&self) -> usize {
        self.open.values().filter(|w| w.dirty()).count()
            + self.closed.iter().filter(|(_, w)| w.dirty()).count()
    }
}

/// 書き手が 1 組を書くための写し。
#[derive(Debug, Clone)]
struct WriteJob {
    key: PairKey,
    generation: u64,
    first_at: SystemTime,
    count: u64,
    row_id: Option<i64>,
    key_name: Arc<str>,
    key_prefix: Arc<str>,
    tag_id: i64,
}

impl WriteJob {
    fn of(key: &PairKey, w: &PairWindow) -> Self {
        Self {
            key: key.clone(),
            generation: w.generation,
            first_at: w.first_at,
            count: w.count,
            row_id: w.row_id,
            key_name: w.key_name.clone(),
            key_prefix: w.key_prefix.clone(),
            tag_id: w.tag_id,
        }
    }

    fn external_name(&self) -> &str {
        match &self.key.1 {
            RejectionTarget::Tag(name) => name,
            RejectionTarget::OtherTags => REJECTION_SUMMARY_NAME,
        }
    }

    fn detail(&self) -> String {
        match self.key.1 {
            RejectionTarget::Tag(_) => rejection_detail(self.key.2, self.count),
            RejectionTarget::OtherTags => rejection_summary_detail(self.key.2, self.count),
        }
    }
}

fn rejection_detail(reason: WriteRejectionReason, count: u64) -> String {
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

fn rejection_summary_detail(reason: WriteRejectionReason, count: u64) -> String {
    format!(
        "{}（{} 秒の窓で、上限を超えたほかのタグ名で計 {} 件）",
        reason.as_str(),
        REJECTION_WINDOW.as_secs(),
        count
    )
}

fn overflow_detail(count: u64) -> String {
    format!("overflow（{count} 件を記録できなかった）")
}

/// 名前を [`REJECTION_NAME_MAX_CHARS`] 文字までに切り詰める(超えたら末尾を `…`)。
fn truncate_name(s: &str) -> std::borrow::Cow<'_, str> {
    match s.char_indices().nth(REJECTION_NAME_MAX_CHARS - 1) {
        Some((keep, _)) if s[keep..].chars().nth(1).is_some() => {
            let mut out = s[..keep].to_string();
            out.push('…');
            std::borrow::Cow::Owned(out)
        }
        _ => std::borrow::Cow::Borrowed(s),
    }
}

fn unix_seconds(t: SystemTime) -> i64 {
    match t.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_secs()).unwrap_or(i64::MAX),
        Err(e) => -i64::try_from(e.duration().as_secs()).unwrap_or(i64::MAX),
    }
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

/// 拒否の行を 1 行挿入する(`ts` は窓の最初の拒否の時刻)。
async fn insert_rejected_row(
    conn: &mut sqlx::SqliteConnection,
    job: &WriteJob,
    detail: &str,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "INSERT INTO hub_write_audit (\
            ts, api_key_id, api_key_name_snapshot, api_key_prefix_snapshot, tag_id, \
            external_name_snapshot, action, result, detail\
         ) VALUES (datetime(?, 'unixepoch'), ?, ?, ?, ?, ?, ?, ?, ?) \
         RETURNING id",
    )
    .bind(unix_seconds(job.first_at))
    .bind(job.key.0)
    .bind(&*job.key_name)
    .bind(&*job.key_prefix)
    .bind(job.tag_id)
    .bind(job.external_name())
    .bind(WriteAuditAction::Write.as_str())
    .bind(WriteAuditResult::Rejected.as_str())
    .bind(detail)
    .fetch_one(conn)
    .await
}

/// 1 周期で DB に書いた数([`WriteAuditService::write_pending`] の結果)。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RejectionWriteStats {
    /// 挿入した行(新しい窓、または UPDATE が 0 行だったので挿入し直した行)。
    pub inserted: usize,
    /// 件数を UPDATE した行。
    pub updated: usize,
    /// あふれの行を書いたら 1。
    pub overflow_rows: usize,
    /// 上限([`REJECTION_WRITE_MAX_PAIRS`])で次の周期に回した、書き終えていない組の数。
    pub remaining: usize,
}

/// 1 つのトランザクションで写しを書く。途中で失敗したら全体を巻き戻す(表は変えない
/// ので、次の周期で同じ組を書き直す)。
async fn write_jobs(
    pool: &SqlitePool,
    jobs: &[WriteJob],
    overflow: u64,
) -> Result<(Vec<i64>, RejectionWriteStats), sqlx::Error> {
    let mut stats = RejectionWriteStats::default();
    let mut tx = pool.begin().await?;
    let mut ids = Vec::with_capacity(jobs.len());
    for job in jobs {
        let detail = job.detail();
        if let Some(id) = job.row_id {
            let done = sqlx::query("UPDATE hub_write_audit SET detail = ? WHERE id = ?")
                .bind(&detail)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            if done.rows_affected() == 1 {
                stats.updated += 1;
                ids.push(id);
                continue;
            }
            // 行が消えていた(0 行): 件数を失わないよう挿入し直す。
        }
        let id = insert_rejected_row(&mut tx, job, &detail).await?;
        stats.inserted += 1;
        ids.push(id);
    }
    if overflow > 0 {
        sqlx::query(
            "INSERT INTO hub_write_audit (\
                api_key_id, api_key_name_snapshot, api_key_prefix_snapshot, tag_id, \
                external_name_snapshot, action, result, detail\
             ) VALUES (0, '', '', 0, ?, ?, ?, ?)",
        )
        .bind(REJECTION_SUMMARY_NAME)
        .bind(WriteAuditAction::Write.as_str())
        .bind(WriteAuditResult::Rejected.as_str())
        .bind(overflow_detail(overflow))
        .execute(&mut *tx)
        .await?;
        stats.overflow_rows = 1;
    }
    tx.commit().await?;
    Ok((ids, stats))
}

/// #945 の総点検: 拒否の表と書き手の様子(サービスの複製の間で共有する)。
struct RejectionShared {
    table: std::sync::Mutex<RejectionTable>,
    /// 書き手どうし(周期のタスク・終了の書き切り・試験)を 1 本ずつにする。
    write_lock: tokio::sync::Mutex<()>,
    /// 書き手の最後の周期の時刻(`base` からのミリ秒)。
    base: Instant,
    last_cycle_ms: AtomicU64,
    /// 周期の書き手を起動していて、まだ止めていない。
    writer_running: AtomicBool,
    /// 「書き手が止まっている」のログを出した(次の周期で下ろす)。
    stale_warned: AtomicBool,
    /// 書込みの失敗が続いている(最初の 1 回と、直ったときだけログに出す)。
    failing: AtomicBool,
    /// 試験用: 次の周期で書き手を panic させる。
    #[cfg(test)]
    panic_next: AtomicBool,
}

impl RejectionShared {
    fn table(&self) -> std::sync::MutexGuard<'_, RejectionTable> {
        // 表の操作は panic しない形だが、毒されても件数は使える(要求の経路を落とさない)。
        self.table
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn millis_since_base(&self, now: Instant) -> u64 {
        u64::try_from(now.saturating_duration_since(self.base).as_millis()).unwrap_or(u64::MAX)
    }

    /// 書き手が動いているはずなのに、最後の周期から [`REJECTION_WRITER_STALE`] を
    /// 過ぎていれば `true`(ログは 1 回だけ出す)。
    fn check_writer(&self, now: Instant) -> bool {
        if !self.writer_running.load(Ordering::Acquire) {
            return false;
        }
        let last = self.last_cycle_ms.load(Ordering::Acquire);
        let age = self.millis_since_base(now).saturating_sub(last);
        if age < u64::try_from(REJECTION_WRITER_STALE.as_millis()).unwrap_or(u64::MAX) {
            return false;
        }
        if !self.stale_warned.swap(true, Ordering::AcqRel) {
            eprintln!(
                "banto-hub: [WARN] 書き込み監査(rejected)の書き手が {} 秒より長く書いていません（拒否は表に溜めています）",
                age / 1000
            );
        }
        true
    }
}

/// #711 / #945 の総点検: 拒否を足すキー(認証のときのキーの値)。
#[derive(Debug, Clone, Copy)]
pub struct RejectingKey<'a> {
    pub id: i64,
    pub name: &'a str,
    pub prefix: &'a str,
}

/// #945 の総点検: 周期の書き手のタスク([`WriteAuditService::spawn_rejection_writer`])。
/// [`Self::shutdown`] で止めて、残りを書き切る。
pub struct RejectionWriter {
    svc: WriteAuditService,
    stop: tokio::sync::watch::Sender<bool>,
    join: tokio::task::JoinHandle<()>,
}

impl RejectionWriter {
    /// 書き手を止め(書いている周期は終わらせる)、表に残ったものをすべて書き切る。
    /// DB が失敗し続けると返らないので、呼び手が上限時間で切る(切られたら
    /// [`WriteAuditService::unwritten_rejections`] で残りを数えてログに出す)。
    pub async fn shutdown(self) {
        let _ = self.stop.send(true);
        let _ = self.join.await;
        loop {
            match self.svc.write_pending().await {
                Ok(_) if self.svc.unwritten_rejections() == 0 => return,
                Ok(_) => {}
                Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
    }
}

/// 周期の書き手の本体。止めるよう言われるまで周期ごとに書く。
async fn run_rejection_writer(
    svc: WriteAuditService,
    period: Duration,
    mut stop: tokio::sync::watch::Receiver<bool>,
) {
    let mut tick = tokio::time::interval(period);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            biased;
            _ = stop.wait_for(|s| *s) => return,
            _ = tick.tick() => {}
        }
        #[cfg(test)]
        if svc.rejections.panic_next.swap(false, Ordering::AcqRel) {
            panic!("試験: 書き手を panic させる");
        }
        // 失敗はこの中でログに出す(表は残るので次の周期で書き直す)。
        let _ = svc.write_pending().await;
    }
}

/// 書き手を見張り、panic したらログに出して起こし直す(表の件数は残っているので、
/// 起こし直した書き手が書く)。止めるよう言われたら終わる。
async fn supervise_rejection_writer(
    svc: WriteAuditService,
    period: Duration,
    restart_min: Duration,
    mut stop: tokio::sync::watch::Receiver<bool>,
) {
    let mut delay = restart_min;
    loop {
        let inner = tokio::spawn(run_rejection_writer(svc.clone(), period, stop.clone()));
        match inner.await {
            Ok(()) => break,
            Err(err) => {
                if *stop.borrow() {
                    break;
                }
                let left = svc.unwritten_rejections();
                eprintln!(
                    "banto-hub: [WARN] 書き込み監査(rejected)の書き手が止まりました（{err}）。{} 秒後に起こし直します（未書込み {left} 件は表に残っています）",
                    delay.as_secs_f32()
                );
                tokio::select! {
                    _ = stop.wait_for(|s| *s) => break,
                    _ = tokio::time::sleep(delay) => {}
                }
                delay = (delay * 2).min(REJECTION_WRITER_RESTART_MAX);
            }
        }
    }
    svc.rejections
        .writer_running
        .store(false, Ordering::Release);
}

/// 書き込み監査サービス(`crate::audit::AuditLogService` と同じ形の
/// 薄いラッパー)。`Clone` は安価(`SqlitePool` は `Arc` バックド)。
/// 拒否の表([`Self::record_rejections`])は複製の間で共有する。
#[derive(Clone)]
pub struct WriteAuditService {
    pool: SqlitePool,
    rejections: Arc<RejectionShared>,
}

impl WriteAuditService {
    pub fn new(pool: SqlitePool) -> Self {
        let base = Instant::now();
        Self {
            rejections: Arc::new(RejectionShared {
                table: std::sync::Mutex::new(RejectionTable::default()),
                write_lock: tokio::sync::Mutex::new(()),
                base,
                last_cycle_ms: AtomicU64::new(0),
                writer_running: AtomicBool::new(false),
                stale_warned: AtomicBool::new(false),
                failing: AtomicBool::new(false),
                #[cfg(test)]
                panic_next: AtomicBool::new(false),
            }),
            pool,
        }
    }

    /// #711: 定義上の拒否を 1 件足す(窓の中の同じ(キー, タグ, 理由)は 1 行に
    /// まとまる)。複数件は [`Self::record_rejections`]。async ではなく、DB にも触れない。
    pub fn record_rejection(
        &self,
        key: RejectingKey<'_>,
        tag_id: i64,
        external_name: &str,
        reason: WriteRejectionReason,
    ) {
        self.record_rejections(key, [(tag_id, external_name, reason)]);
    }

    /// #945 の総点検: 1 つのキーの拒否(`(tag_id, 外部名, 理由)`)を、表を 1 回だけ
    /// lock して足す。**async ではなく、DB にも async のロックにも触れない**(403・404 を
    /// 監査で遅らせない。要求の future が捨てられても、足し終えた分は欠けない)。
    /// 表に入らない分はまとめの組・あふれの件数に足す(モジュールの doc comment)。
    /// DB へは周期の書き手([`Self::spawn_rejection_writer`])が書く。
    pub fn record_rejections<'n>(
        &self,
        key: RejectingKey<'_>,
        entries: impl IntoIterator<Item = (i64, &'n str, WriteRejectionReason)>,
    ) {
        let now = Instant::now();
        self.record_rejections_at(now, SystemTime::now(), key, entries);
        self.rejections.check_writer(now);
    }

    fn record_rejections_at<'n>(
        &self,
        now: Instant,
        wall: SystemTime,
        key: RejectingKey<'_>,
        entries: impl IntoIterator<Item = (i64, &'n str, WriteRejectionReason)>,
    ) {
        let mut names = KeyNames { key, shared: None };
        let mut table = self.rejections.table();
        for (tag_id, name, reason) in entries {
            table.add(now, wall, &mut names, tag_id, name, reason, 1);
        }
    }

    /// まだ DB に書いていない拒否の件数(あふれを含む)。
    pub fn unwritten_rejections(&self) -> u64 {
        self.rejections.table().unwritten()
    }

    /// #945 の総点検: 周期の書き手を起動する([`REJECTION_WRITE_PERIOD`] ごとに
    /// [`Self::write_pending`])。`HubRuntime::start` が 1 回だけ呼ぶ。panic したら
    /// ログに出して起こし直す。
    pub fn spawn_rejection_writer(&self) -> RejectionWriter {
        self.spawn_rejection_writer_with(REJECTION_WRITE_PERIOD, REJECTION_WRITER_RESTART_MIN)
    }

    fn spawn_rejection_writer_with(
        &self,
        period: Duration,
        restart_min: Duration,
    ) -> RejectionWriter {
        let (stop, rx) = tokio::sync::watch::channel(false);
        let shared = &self.rejections;
        shared
            .last_cycle_ms
            .store(shared.millis_since_base(Instant::now()), Ordering::Release);
        shared.writer_running.store(true, Ordering::Release);
        let join = tokio::spawn(supervise_rejection_writer(
            self.clone(),
            period,
            restart_min,
            rx,
        ));
        RejectionWriter {
            svc: self.clone(),
            stop,
            join,
        }
    }

    /// #945 の総点検: 表の書き終えていない組を、今すぐ 1 周期ぶん書く(周期の書き手が
    /// 呼ぶ。試験では拒否の直後に監査を読むために呼ぶ)。1 つのトランザクションで最大
    /// [`REJECTION_WRITE_MAX_PAIRS`] 組。新しい窓は INSERT(`ts` は窓の最初の拒否の時刻)、
    /// 行のある窓は UPDATE(0 行なら INSERT し直す)。失敗したら表を変えずに返す
    /// (次の周期で書き直す)。失敗は続いている間の最初の 1 回だけログに出す。
    pub async fn write_pending(&self) -> Result<RejectionWriteStats, BantoError> {
        self.write_pending_at(Instant::now()).await
    }

    async fn write_pending_at(&self, now: Instant) -> Result<RejectionWriteStats, BantoError> {
        let shared = &self.rejections;
        let _writing = shared.write_lock.lock().await;
        let (jobs, overflow) = shared.table().take_jobs(REJECTION_WRITE_MAX_PAIRS);
        let result = if jobs.is_empty() && overflow == 0 {
            Ok((Vec::new(), RejectionWriteStats::default()))
        } else {
            write_jobs(&self.pool, &jobs, overflow).await
        };
        let outcome = match result {
            Ok((ids, mut stats)) => {
                let mut table = shared.table();
                let done: Vec<(WriteJob, i64)> = jobs.into_iter().zip(ids).collect();
                table.apply(&done, if stats.overflow_rows > 0 { overflow } else { 0 });
                table.collect_garbage(now);
                stats.remaining = table.dirty_pairs();
                drop(table);
                if shared.failing.swap(false, Ordering::AcqRel) {
                    eprintln!("banto-hub: 書き込み監査(rejected)の書込みが戻りました");
                }
                Ok(stats)
            }
            Err(err) => {
                if !shared.failing.swap(true, Ordering::AcqRel) {
                    eprintln!(
                        "banto-hub: [WARN] 書き込み監査(rejected)の書込みに失敗しました（件数は表に残し、次の周期で書き直します）: {err}"
                    );
                }
                Err(banto_storage::storage_error(err))
            }
        };
        shared
            .last_cycle_ms
            .store(shared.millis_since_base(Instant::now()), Ordering::Release);
        shared.stale_warned.store(false, Ordering::Release);
        outcome
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

    const KEY: RejectingKey<'static> = RejectingKey {
        id: 1,
        name: "mes",
        prefix: "bk_abc",
    };

    async fn service() -> WriteAuditService {
        WriteAuditService::new(migrate_memory().await.expect("migrate_memory"))
    }

    async fn rows(svc: &WriteAuditService) -> Vec<WriteAuditEntry> {
        let mut rows = svc.list(ListParams::default()).await.unwrap().rows;
        rows.sort_by_key(|r| r.id);
        rows
    }

    /// 行の `detail` から件数を読む(コードだけなら 1)。
    fn count_of(detail: &str) -> u64 {
        match detail.split("計 ").nth(1) {
            Some(rest) => rest.split(' ').next().unwrap().parse().unwrap(),
            None => 1,
        }
    }

    fn add(svc: &WriteAuditService, now: Instant, name: &str, reason: WriteRejectionReason) {
        svc.record_rejections_at(now, SystemTime::now(), KEY, [(7, name, reason)]);
    }

    #[test]
    fn the_request_path_is_a_plain_function_that_does_not_return_a_future() {
        // 戻り値が `()`(future ではない)ことを型で確かめる。
        let _single: fn(&WriteAuditService, RejectingKey<'_>, i64, &str, WriteRejectionReason) =
            WriteAuditService::record_rejection;
        type Batch<'n> = Vec<(i64, &'n str, WriteRejectionReason)>;
        let _batch: fn(&WriteAuditService, RejectingKey<'_>, Batch<'_>) =
            |svc, key, entries| svc.record_rejections(key, entries);
    }

    #[tokio::test]
    async fn adding_works_with_a_closed_pool_because_it_never_touches_the_db() {
        let svc = service().await;
        svc.pool.close().await;
        svc.record_rejection(KEY, 7, "line.t", WriteRejectionReason::NotWritable);
        assert_eq!(svc.unwritten_rejections(), 1);
    }

    #[tokio::test]
    async fn the_same_pair_100k_times_is_one_row_with_the_total() {
        let svc = service().await;
        let r = WriteRejectionReason::NotWritable;
        svc.record_rejections(KEY, std::iter::repeat_n((7, "line.t", r), 100_000));
        let stats = svc.write_pending().await.unwrap();
        assert_eq!((stats.inserted, stats.updated), (1, 0));
        let got = rows(&svc).await;
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].result, "rejected");
        assert_eq!(got[0].action, "write");
        assert_eq!(got[0].api_key_name_snapshot, "mes");
        assert_eq!(got[0].api_key_prefix_snapshot.as_deref(), Some("bk_abc"));
        assert_eq!(got[0].tag_id, 7);
        assert_eq!(
            got[0].detail.as_deref(),
            Some("not_writable（60 秒の窓で計 100000 件）")
        );
        assert_eq!(svc.unwritten_rejections(), 0);
    }

    #[tokio::test]
    async fn a_single_rejection_has_only_the_code() {
        let svc = service().await;
        svc.record_rejection(KEY, 0, "nope", WriteRejectionReason::NotFound);
        svc.write_pending().await.unwrap();
        let got = rows(&svc).await;
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].detail.as_deref(), Some("not_found"));
    }

    #[tokio::test]
    async fn one_cycle_updates_a_dirty_pair_once_however_many_were_added() {
        let svc = service().await;
        let r = WriteRejectionReason::TagDisabled;
        svc.record_rejection(KEY, 7, "line.t", r);
        svc.write_pending().await.unwrap();
        for _ in 0..10_000 {
            svc.record_rejection(KEY, 7, "line.t", r);
        }
        let stats = svc.write_pending().await.unwrap();
        assert_eq!((stats.inserted, stats.updated), (0, 1));
        // 書き終えたら、次の周期は何もしない。
        let idle = svc.write_pending().await.unwrap();
        assert_eq!(idle, RejectionWriteStats::default());
        let got = rows(&svc).await;
        assert_eq!(got.len(), 1);
        assert_eq!(count_of(got[0].detail.as_deref().unwrap()), 10_001);
    }

    #[tokio::test]
    async fn one_cycle_writes_at_most_the_cap_and_leaves_the_rest_for_the_next() {
        let svc = service().await;
        // キーごとの予算(20)に当たらないよう、キーを変えて組を増やす。
        for k in 0..300 {
            let key = RejectingKey {
                id: 100 + k,
                name: "k",
                prefix: "p",
            };
            svc.record_rejection(key, 0, "x", WriteRejectionReason::NotFound);
        }
        let first = svc.write_pending().await.unwrap();
        assert_eq!(first.inserted, REJECTION_WRITE_MAX_PAIRS);
        assert_eq!(first.remaining, 300 - REJECTION_WRITE_MAX_PAIRS);
        let second = svc.write_pending().await.unwrap();
        assert_eq!(second.inserted, 300 - REJECTION_WRITE_MAX_PAIRS);
        assert_eq!(second.remaining, 0);
        assert_eq!(rows(&svc).await.len(), 300);
    }

    #[tokio::test]
    async fn a_thousand_tag_names_fit_in_the_per_key_rows_plus_a_summary() {
        let svc = service().await;
        let r = WriteRejectionReason::NotFound;
        let names: Vec<String> = (0..1000).map(|i| format!("no_such_{i}")).collect();
        svc.record_rejections(KEY, names.iter().map(|n| (0, n.as_str(), r)));
        svc.write_pending().await.unwrap();
        let got = rows(&svc).await;
        assert_eq!(got.len(), REJECTION_ROWS_PER_KEY + 1);
        let summary: Vec<_> = got
            .iter()
            .filter(|row| row.external_name_snapshot == REJECTION_SUMMARY_NAME)
            .collect();
        assert_eq!(summary.len(), 1);
        assert_eq!(
            summary[0].detail.as_deref(),
            Some("not_found（60 秒の窓で、上限を超えたほかのタグ名で計 980 件）")
        );
        let total: u64 = got
            .iter()
            .map(|row| count_of(row.detail.as_deref().unwrap()))
            .sum();
        assert_eq!(total, 1000);
    }

    #[tokio::test]
    async fn the_row_ts_is_the_time_of_the_first_rejection() {
        let svc = service().await;
        // 2001-09-09 01:46:40 UTC
        let first_at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
        svc.record_rejections_at(
            Instant::now(),
            first_at,
            KEY,
            [(7, "line.t", WriteRejectionReason::NotWritable)],
        );
        svc.record_rejection(KEY, 7, "line.t", WriteRejectionReason::NotWritable);
        svc.write_pending().await.unwrap();
        let got = rows(&svc).await;
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].ts, "2001-09-09 01:46:40");
    }

    #[tokio::test]
    async fn an_update_of_zero_rows_inserts_the_row_again() {
        let svc = service().await;
        let r = WriteRejectionReason::NotWritable;
        add(&svc, Instant::now(), "line.t", r);
        svc.write_pending().await.unwrap();
        sqlx::query("DELETE FROM hub_write_audit")
            .execute(&svc.pool)
            .await
            .unwrap();
        add(&svc, Instant::now(), "line.t", r);
        let stats = svc.write_pending().await.unwrap();
        assert_eq!((stats.inserted, stats.updated), (1, 0));
        let got = rows(&svc).await;
        assert_eq!(got.len(), 1);
        assert_eq!(
            got[0].detail.as_deref(),
            Some("not_writable（60 秒の窓で計 2 件）")
        );
    }

    #[tokio::test]
    async fn a_failed_write_keeps_the_counts_for_the_next_cycle() {
        let svc = service().await;
        let r = WriteRejectionReason::NotWritable;
        add(&svc, Instant::now(), "a", r);
        svc.write_pending().await.unwrap();
        for stmt in [
            "CREATE TRIGGER fail_ins BEFORE INSERT ON hub_write_audit \
             BEGIN SELECT RAISE(ABORT, 'test'); END",
            "CREATE TRIGGER fail_upd BEFORE UPDATE ON hub_write_audit \
             BEGIN SELECT RAISE(ABORT, 'test'); END",
        ] {
            sqlx::query(stmt).execute(&svc.pool).await.unwrap();
        }
        add(&svc, Instant::now(), "a", r);
        add(&svc, Instant::now(), "b", r);
        assert!(svc.write_pending().await.is_err());
        assert!(svc.write_pending().await.is_err());
        // トランザクションごと巻き戻るので、半端な行は残らない。
        assert_eq!(rows(&svc).await.len(), 1);
        assert_eq!(svc.unwritten_rejections(), 2);
        for stmt in ["DROP TRIGGER fail_ins", "DROP TRIGGER fail_upd"] {
            sqlx::query(stmt).execute(&svc.pool).await.unwrap();
        }
        let stats = svc.write_pending().await.unwrap();
        assert_eq!((stats.inserted, stats.updated), (1, 1));
        let got = rows(&svc).await;
        assert_eq!(got.len(), 2);
        assert_eq!(
            got[0].detail.as_deref(),
            Some("not_writable（60 秒の窓で計 2 件）")
        );
        assert_eq!(got[1].external_name_snapshot, "b");
        assert_eq!(svc.unwritten_rejections(), 0);
    }

    #[tokio::test]
    async fn a_new_window_is_a_new_row_and_keeps_the_old_total() {
        let svc = service().await;
        let r = WriteRejectionReason::NotWritable;
        let t0 = Instant::now();
        for _ in 0..3 {
            add(&svc, t0, "line.t", r);
        }
        svc.write_pending_at(t0).await.unwrap();
        let t1 = t0 + REJECTION_WINDOW + Duration::from_secs(1);
        add(&svc, t1, "line.t", r);
        svc.write_pending_at(t1).await.unwrap();
        let got = rows(&svc).await;
        assert_eq!(got.len(), 2);
        assert_eq!(
            got[0].detail.as_deref(),
            Some("not_writable（60 秒の窓で計 3 件）")
        );
        assert_eq!(got[1].detail.as_deref(), Some("not_writable"));
    }

    #[tokio::test]
    async fn a_window_closed_before_it_was_written_is_still_written() {
        let svc = service().await;
        let r = WriteRejectionReason::NotWritable;
        let t0 = Instant::now();
        add(&svc, t0, "line.t", r);
        add(&svc, t0, "line.t", r);
        // 書く前に窓が明けて、同じ組の次の拒否が来た。
        let t1 = t0 + REJECTION_WINDOW + Duration::from_secs(1);
        add(&svc, t1, "line.t", r);
        svc.write_pending_at(t1).await.unwrap();
        let got = rows(&svc).await;
        assert_eq!(got.len(), 2);
        let total: u64 = got
            .iter()
            .map(|row| count_of(row.detail.as_deref().unwrap()))
            .sum();
        assert_eq!(total, 3);
        assert_eq!(svc.unwritten_rejections(), 0);
    }

    #[tokio::test]
    async fn written_pairs_are_dropped_after_the_window() {
        let svc = service().await;
        let t0 = Instant::now();
        add(&svc, t0, "line.t", WriteRejectionReason::NotWritable);
        svc.write_pending_at(t0).await.unwrap();
        assert_eq!(svc.rejections.table().open.len(), 1);
        svc.write_pending_at(t0 + REJECTION_WINDOW).await.unwrap();
        let table = svc.rejections.table();
        assert!(table.open.is_empty());
        assert!(table.budgets.is_empty());
    }

    #[tokio::test]
    async fn rejections_that_do_not_fit_are_left_as_an_overflow_row() {
        let svc = service().await;
        let t0 = Instant::now();
        // 表を組で満たす(キーごとに 1 組)。
        for k in 0..REJECTION_TABLE_MAX_PAIRS {
            let key = RejectingKey {
                id: 1000 + k as i64,
                name: "k",
                prefix: "p",
            };
            svc.record_rejections_at(
                t0,
                SystemTime::now(),
                key,
                [(0, "x", WriteRejectionReason::NotFound)],
            );
        }
        // 新しいキーは、組もまとめの組も作れないので、あふれに数える。
        for _ in 0..5 {
            add(&svc, t0, "y", WriteRejectionReason::NotFound);
        }
        assert_eq!(
            svc.unwritten_rejections(),
            REJECTION_TABLE_MAX_PAIRS as u64 + 5
        );
        while svc.write_pending_at(t0).await.unwrap().remaining > 0 {}
        let overflow: Vec<_> = rows(&svc)
            .await
            .into_iter()
            .filter(|row| row.api_key_id == 0)
            .collect();
        assert_eq!(overflow.len(), 1);
        assert_eq!(
            overflow[0].detail.as_deref(),
            Some("overflow（5 件を記録できなかった）")
        );
        assert_eq!(overflow[0].external_name_snapshot, REJECTION_SUMMARY_NAME);
        assert_eq!(svc.unwritten_rejections(), 0);
    }

    #[tokio::test]
    async fn a_long_external_name_is_truncated_with_a_mark() {
        let svc = service().await;
        let long = "あ".repeat(REJECTION_NAME_MAX_CHARS + 10);
        let other = format!("{}い", "あ".repeat(REJECTION_NAME_MAX_CHARS + 10));
        let r = WriteRejectionReason::NotFound;
        svc.record_rejections(KEY, [(0, long.as_str(), r), (0, other.as_str(), r)]);
        svc.write_pending().await.unwrap();
        let got = rows(&svc).await;
        assert_eq!(got.len(), 1);
        let cut = &got[0].external_name_snapshot;
        assert_eq!(cut.chars().count(), REJECTION_NAME_MAX_CHARS);
        assert!(cut.ends_with('…'));
        assert_eq!(
            got[0].detail.as_deref(),
            Some("not_found（60 秒の窓で計 2 件）")
        );
        // 上限ちょうどの名前は切り詰めない。
        let exact = "a".repeat(REJECTION_NAME_MAX_CHARS);
        assert_eq!(truncate_name(&exact), exact.as_str());
    }

    #[tokio::test]
    async fn different_key_tag_or_reason_are_recorded_separately() {
        let svc = service().await;
        let other_key = RejectingKey {
            id: 2,
            name: "other",
            prefix: "bk_x",
        };
        svc.record_rejection(KEY, 7, "a", WriteRejectionReason::NotWritable);
        svc.record_rejection(KEY, 7, "a", WriteRejectionReason::TagDisabled);
        svc.record_rejection(KEY, 8, "b", WriteRejectionReason::NotWritable);
        svc.record_rejection(other_key, 7, "a", WriteRejectionReason::NotWritable);
        svc.write_pending().await.unwrap();
        assert_eq!(rows(&svc).await.len(), 4);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_periodic_writer_writes_and_its_shutdown_writes_everything() {
        let svc = service().await;
        let writer = svc.spawn_rejection_writer_with(Duration::from_millis(20), Duration::ZERO);
        svc.record_rejection(KEY, 7, "a", WriteRejectionReason::NotWritable);
        let deadline = Instant::now() + Duration::from_secs(5);
        while rows(&svc).await.is_empty() {
            assert!(Instant::now() < deadline, "周期の書き手が書かない");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        svc.record_rejection(KEY, 7, "b", WriteRejectionReason::NotWritable);
        writer.shutdown().await;
        assert_eq!(rows(&svc).await.len(), 2);
        assert!(!svc.rejections.writer_running.load(Ordering::Acquire));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_panicked_writer_is_restarted_and_keeps_the_counts() {
        let svc = service().await;
        svc.rejections.panic_next.store(true, Ordering::Release);
        let writer =
            svc.spawn_rejection_writer_with(Duration::from_millis(20), Duration::from_millis(20));
        svc.record_rejection(KEY, 7, "a", WriteRejectionReason::NotWritable);
        let deadline = Instant::now() + Duration::from_secs(5);
        while rows(&svc).await.is_empty() {
            assert!(Instant::now() < deadline, "起こし直した書き手が書かない");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(!svc.rejections.panic_next.load(Ordering::Acquire));
        assert!(svc.rejections.writer_running.load(Ordering::Acquire));
        writer.shutdown().await;
    }

    #[tokio::test]
    async fn the_request_path_notices_a_writer_that_stopped_writing() {
        let svc = service().await;
        let now = Instant::now();
        // 書き手を起動していなければ(試験の組み立てなど)、気にしない。
        assert!(!svc.rejections.check_writer(now + Duration::from_secs(3600)));
        svc.rejections.writer_running.store(true, Ordering::Release);
        assert!(!svc.rejections.check_writer(now));
        let late = now + REJECTION_WRITER_STALE + Duration::from_secs(1);
        assert!(svc.rejections.check_writer(late));
        assert!(svc.rejections.stale_warned.load(Ordering::Acquire));
        // 書き手が 1 周期まわれば、印は下りる。
        svc.write_pending().await.unwrap();
        assert!(!svc.rejections.stale_warned.load(Ordering::Acquire));
    }
}
