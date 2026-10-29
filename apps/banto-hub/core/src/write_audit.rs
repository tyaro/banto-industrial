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
//! 窓が明けた次の拒否は新しい行を挿入して窓を始め直す。記憶(窓の最初の行の
//! id と件数)はメモリ上だけで、判定から挿入/UPDATE までを `tokio::sync::Mutex`
//! のロックを持ったまま行う(並行の 2 件目が件数を失わない)。挿入に失敗した
//! 組は記憶に残さない。監査の書込みに失敗しても応答は変えず、ログだけ残す。
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
use std::sync::Arc;
use std::time::{Duration, Instant};

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

/// #711: まとめの記憶に持つ組の上限。超えたら窓の明けた組、それでも足りなければ
/// 最も古い窓の組を捨てる。捨てても、件数はすでに DB の行の `detail` にあるので
/// 失われない(捨てた組の次の拒否は、新しい行を挿入して窓を始め直す)。
pub const REJECTION_THROTTLE_MAX_ENTRIES: usize = 1000;

/// #711: 組ごとの現在の窓。
struct RejectionWindow {
    /// 窓の最初の拒否の時刻(窓はここから [`REJECTION_WINDOW`])。
    start: Instant,
    /// 窓の最初の拒否で挿入した行の id(以降の拒否はこの行を UPDATE する)。
    row_id: i64,
    /// 窓の中の拒否の合計(最初の 1 件を含む)。
    count: u32,
}

type RejectionKey = (i64, String, WriteRejectionReason);

/// #711: 拒否の行をまとめる記憶(メモリ上のみ)。件数は行の `detail` に
/// 書き込み済みなので、再起動や組の破棄で件数が失われることはない。
#[derive(Default)]
struct RejectionThrottle {
    map: HashMap<RejectionKey, RejectionWindow>,
}

impl RejectionThrottle {
    /// 新しい組を入れる前に、上限を超えないよう組を捨てる。
    fn make_room(&mut self, now: Instant) {
        if self.map.len() < REJECTION_THROTTLE_MAX_ENTRIES {
            return;
        }
        self.map
            .retain(|_, w| now.saturating_duration_since(w.start) < REJECTION_WINDOW);
        if self.map.len() >= REJECTION_THROTTLE_MAX_ENTRIES {
            if let Some(oldest) = self
                .map
                .iter()
                .min_by_key(|(_, w)| w.start)
                .map(|(k, _)| k.clone())
            {
                self.map.remove(&oldest);
            }
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

/// 書き込み監査サービス(`crate::audit::AuditLogService` と同じ形の
/// 薄いラッパー)。`Clone` は安価(`SqlitePool` は `Arc` バックド)。
/// 拒否のまとめの記憶([`Self::record_rejection`])は複製の間で共有する。
#[derive(Clone)]
pub struct WriteAuditService {
    pool: SqlitePool,
    rejections: Arc<tokio::sync::Mutex<RejectionThrottle>>,
}

impl WriteAuditService {
    pub fn new(pool: SqlitePool) -> Self {
        Self {
            pool,
            rejections: Arc::new(tokio::sync::Mutex::new(RejectionThrottle::default())),
        }
    }

    /// #711: 定義上の拒否を `result = 'rejected'` で残す(窓の中の同じ
    /// (キー, タグ, 理由)は 1 行にまとめる)。時刻は [`Instant::now`]。
    /// 失敗は応答を変えないよう、エラーはログに残して握りつぶす。
    pub async fn record_rejection(
        &self,
        api_key_id: i64,
        api_key_name: &str,
        tag_id: i64,
        external_name: &str,
        reason: WriteRejectionReason,
    ) {
        self.record_rejection_at(
            Instant::now(),
            api_key_id,
            api_key_name,
            tag_id,
            external_name,
            reason,
        )
        .await;
    }

    /// [`Self::record_rejection`] の時刻を注入できる形(試験用)。
    pub async fn record_rejection_at(
        &self,
        now: Instant,
        api_key_id: i64,
        api_key_name: &str,
        tag_id: i64,
        external_name: &str,
        reason: WriteRejectionReason,
    ) {
        let key: RejectionKey = (api_key_id, external_name.to_string(), reason);
        // 判定から挿入/UPDATE までロックを持ったまま行う。挿入が終わる前に同じ組の
        // 2 件目が来ても、行 id が決まってから数えるので件数が失われない(拒否の
        // 監査の書込みだけが直列になる)。
        let mut throttle = self.rejections.lock().await;
        if let Some(w) = throttle.map.get_mut(&key) {
            if now.saturating_duration_since(w.start) < REJECTION_WINDOW {
                w.count = w.count.saturating_add(1);
                let detail = rejection_detail(reason, w.count);
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
                return;
            }
            // 窓が明けた: 前の行はそのまま残し、新しい行で窓を始め直す。
            throttle.map.remove(&key);
        }
        let row = WriteAuditRow::new(
            api_key_id,
            api_key_name,
            tag_id,
            external_name,
            WriteAuditAction::Write,
            WriteAuditResult::Rejected,
        )
        .with_detail(reason.as_str());
        match self.insert_row(&row).await {
            Ok(row_id) => {
                throttle.make_room(now);
                throttle.map.insert(
                    key,
                    RejectionWindow {
                        start: now,
                        row_id,
                        count: 1,
                    },
                );
            }
            // 失敗した組は記憶に残さない(次の拒否で挿入をやり直す)。
            Err(err) => eprintln!(
                "banto-hub: 書き込み監査(rejected: {})の記録に失敗しました: {err}",
                reason.as_str()
            ),
        }
    }

    /// 完結した1行をそのまま挿入する(抑制系: `suppressed_disabled`/
    /// `rate_limit_tripped`)。新しい `id` を返す。
    pub async fn insert_row(&self, row: &WriteAuditRow) -> Result<i64, BantoError> {
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
        .fetch_one(&self.pool)
        .await
        .map_err(banto_storage::storage_error)?;
        Ok(id)
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

    async fn service() -> WriteAuditService {
        WriteAuditService::new(migrate_memory().await.expect("migrate_memory"))
    }

    async fn rows(svc: &WriteAuditService) -> Vec<WriteAuditEntry> {
        let mut rows = svc.list(ListParams::default()).await.unwrap().rows;
        rows.sort_by_key(|r| r.id);
        rows
    }

    #[tokio::test]
    async fn repeated_rejection_inside_the_window_is_one_row_with_the_total() {
        let svc = service().await;
        let t0 = Instant::now();
        let r = WriteRejectionReason::NotWritable;
        for i in 0..13 {
            svc.record_rejection_at(t0 + Duration::from_secs(i), 1, "k", 5, "a.b", r)
                .await;
        }
        // 拒否が止まっても、何も呼ばずに読んで件数が残っている。
        let got = rows(&svc).await;
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].result, "rejected");
        assert_eq!(got[0].action, "write");
        assert_eq!(
            got[0].detail.as_deref(),
            Some("not_writable（60 秒の窓で計 13 件）")
        );
    }

    #[tokio::test]
    async fn a_single_rejection_has_only_the_code() {
        let svc = service().await;
        let r = WriteRejectionReason::NotFound;
        svc.record_rejection_at(Instant::now(), 1, "k", 0, "x", r)
            .await;
        assert_eq!(rows(&svc).await[0].detail.as_deref(), Some("not_found"));
    }

    #[tokio::test]
    async fn a_new_window_is_a_new_row_and_keeps_the_old_total() {
        let svc = service().await;
        let t0 = Instant::now();
        let r = WriteRejectionReason::NotWritable;
        for i in 0..5 {
            svc.record_rejection_at(t0 + Duration::from_secs(i), 1, "k", 5, "a.b", r)
                .await;
        }
        svc.record_rejection_at(t0 + REJECTION_WINDOW, 1, "k", 5, "a.b", r)
            .await;
        svc.record_rejection_at(
            t0 + REJECTION_WINDOW + Duration::from_secs(1),
            1,
            "k",
            5,
            "a.b",
            r,
        )
        .await;
        let got = rows(&svc).await;
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
        let svc = service().await;
        let t0 = Instant::now();
        let r = WriteRejectionReason::NotFound;
        // 最初の組に 3 件。
        for _ in 0..3 {
            svc.record_rejection_at(t0, 1, "k", 0, "first", r).await;
        }
        // 上限を超える数の別の組で、最初の組を記憶から捨てさせる。
        for i in 0..REJECTION_THROTTLE_MAX_ENTRIES {
            svc.record_rejection_at(
                t0 + Duration::from_millis(1 + i as u64),
                1,
                "k",
                0,
                &format!("tag{i}"),
                r,
            )
            .await;
        }
        assert!(svc.rejections.lock().await.map.len() <= REJECTION_THROTTLE_MAX_ENTRIES);
        let got = svc.list(ListParams::default()).await.unwrap().rows;
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
        let svc = service().await;
        let t0 = Instant::now();
        let r = WriteRejectionReason::NotWritable;
        svc.record_rejection_at(t0, 1, "k", 5, "a.b", r).await;
        svc.record_rejection_at(t0, 2, "k2", 5, "a.b", r).await;
        svc.record_rejection_at(t0, 1, "k", 6, "a.c", r).await;
        svc.record_rejection_at(
            t0,
            1,
            "k",
            5,
            "a.b",
            WriteRejectionReason::MissingWriteScope,
        )
        .await;
        assert_eq!(rows(&svc).await.len(), 4);
    }
}
