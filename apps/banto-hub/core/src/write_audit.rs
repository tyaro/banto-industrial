//! 書き込み監査 (docs/tag-server-design.md §6-3「log-before-write」)。
//! `hub_write_audit` テーブル (`db.rs::apply_app_schema` の冪等 DDL) の
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
//! 結果に応じて `ok`/`failed` に更新する。途中でプロセスが死んでも
//! (電源断・パニック等)「試みられたが成功が未確認」という記録が
//! 残る - これが安全側の解釈(relay-wright の同モジュール doc comment
//! と同じ理由付け: 「試みたのに記録がない」よりずっと安全)。
//!
//! 一方、**物理書き込みを一切試みない抑制**(受付 off による
//! `suppressed_disabled`、レート制限超過による `rate_limit_tripped`/
//! `suppressed_rate_limited`)は最初から確定結果を持つので
//! [`insert_row`] 1回で完結する(pending 経由にする理由がない -
//! そもそも broker を呼んでいない)。
//!
//! ## スナップショット列 (`api_key_name_snapshot`/`external_name_snapshot`)
//!
//! API キーは失効(`revoked_at`)されても行として残るが名前が変わる
//! ことはない一方、タグは削除・リネームされうる。監査行は「その時点で
//! 何が起きたか」を後から名前解決なしに読めるよう、キー名・外部名を
//! **書き込み時点の値としてスナップショット**する(`crate::audit`の
//! `rule_name_snapshot` と同じ設計判断)。

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
    /// 物理書き込みを試みたが broker がエラーを返した。
    Failed,
    /// 書き込み受付(§6-6)が off だったため、物理書き込みを一切試みず抑制した。
    SuppressedDisabled,
    /// レート制限(§6-4)を超過するため、物理書き込みを一切試みず抑制した。
    SuppressedRateLimited,
}

impl WriteAuditResult {
    pub fn as_str(self) -> &'static str {
        match self {
            WriteAuditResult::Ok => "ok",
            WriteAuditResult::Failed => "failed",
            WriteAuditResult::SuppressedDisabled => "suppressed_disabled",
            WriteAuditResult::SuppressedRateLimited => "suppressed_rate_limited",
        }
    }
}

/// 1行分のフィールド(自動採番の `id`/`ts` を除く)。
/// [`WriteAuditRow::new`] + `with_*` で組み立てる。
#[derive(Debug, Clone)]
pub struct WriteAuditRow {
    pub api_key_id: i64,
    pub api_key_name_snapshot: String,
    pub tag_id: i64,
    pub external_name_snapshot: String,
    pub value_requested: Option<f64>,
    pub action: WriteAuditAction,
    pub result: WriteAuditResult,
    pub detail: Option<String>,
}

impl WriteAuditRow {
    pub fn new(
        api_key_id: i64,
        api_key_name_snapshot: impl Into<String>,
        tag_id: i64,
        external_name_snapshot: impl Into<String>,
        action: WriteAuditAction,
        result: WriteAuditResult,
    ) -> Self {
        Self {
            api_key_id,
            api_key_name_snapshot: api_key_name_snapshot.into(),
            tag_id,
            external_name_snapshot: external_name_snapshot.into(),
            value_requested: None,
            action,
            result,
            detail: None,
        }
    }

    pub fn with_value_requested(mut self, value: f64) -> Self {
        self.value_requested = Some(value);
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
    pub tag_id: i64,
    pub external_name_snapshot: String,
    pub value_requested: Option<f64>,
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
        .column("tagId", "tag_id")
        .column("externalNameSnapshot", "external_name_snapshot")
        .column("valueRequested", "value_requested")
        .column("action", "action")
        .column("result", "result")
        .column("detail", "detail")
}

/// 書き込み監査サービス(`crate::audit::AuditLogService` と同じ形の
/// 薄いラッパー)。`Clone` は安価(`SqlitePool` は `Arc` バックド)。
#[derive(Clone)]
pub struct WriteAuditService {
    pool: SqlitePool,
}

impl WriteAuditService {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// 完結した1行をそのまま挿入する(抑制系: `suppressed_disabled`/
    /// `rate_limit_tripped`)。新しい `id` を返す。
    pub async fn insert_row(&self, row: &WriteAuditRow) -> Result<i64, BantoError> {
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO hub_write_audit (\
                api_key_id, api_key_name_snapshot, tag_id, external_name_snapshot, \
                value_requested, action, result, detail\
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
        )
        .bind(row.api_key_id)
        .bind(&row.api_key_name_snapshot)
        .bind(row.tag_id)
        .bind(&row.external_name_snapshot)
        .bind(row.value_requested)
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
    /// (`broker.write` 成功なら `Ok`、それ以外は `Failed`)。`detail` は
    /// 失敗理由の説明文(T8-2、docs/tag-server-design.md §6.1、
    /// 2026-08-06: 例えば T8 の RMW 確認読み不一致
    /// `PlcWriteError::BitWriteVerificationFailed` の
    /// 「書き戻し競合の可能性があります」)- `crate::write_path::execute_write`
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
            "SELECT id, ts, api_key_id, api_key_name_snapshot, tag_id, external_name_snapshot, \
             value_requested, action, result, detail FROM hub_write_audit",
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
