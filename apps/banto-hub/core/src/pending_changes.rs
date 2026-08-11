//! TAG-P0-3（2026-08-11 方針改定）: 運転中編集の提案変更を保持する
//! pending queue の service 層。
//!
//! このモジュールは「反映エンジン」ではなく「保存・状態遷移・取得」の責務に
//! 限定する。実行構成への適用（preflight + apply_run）や controller 連携は
//! `crate::rest`/`crate::controller` 側が後続スライスで担う。

use banto_core::BantoError;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PendingChangeState {
    Pending,
    Applying,
    Applied,
    Canceled,
    Failed,
}

impl PendingChangeState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Applying => "applying",
            Self::Applied => "applied",
            Self::Canceled => "canceled",
            Self::Failed => "failed",
        }
    }

    fn parse(value: &str) -> Result<Self, BantoError> {
        match value {
            "pending" => Ok(Self::Pending),
            "applying" => Ok(Self::Applying),
            "applied" => Ok(Self::Applied),
            "canceled" => Ok(Self::Canceled),
            "failed" => Ok(Self::Failed),
            other => Err(BantoError::Other(format!(
                "不明な pending change 状態です: {other}"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingChange {
    pub id: i64,
    pub state: PendingChangeState,
    pub source: String,
    pub payload: serde_json::Value,
    pub base_configured_revision: i64,
    pub created_at: String,
    pub updated_at: String,
    pub requested_by_username: Option<String>,
    pub requested_by_role: Option<String>,
    pub failure_reason: Option<String>,
}

type PendingRow = (
    i64,
    String,
    String,
    String,
    i64,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
);

fn row_to_pending(row: PendingRow) -> Result<PendingChange, BantoError> {
    let (
        id,
        state,
        source,
        payload,
        base_configured_revision,
        created_at,
        updated_at,
        requested_by_username,
        requested_by_role,
        failure_reason,
    ) = row;
    Ok(PendingChange {
        id,
        state: PendingChangeState::parse(&state)?,
        source,
        payload: serde_json::from_str(&payload).map_err(|err| {
            BantoError::Other(format!("payload のデシリアライズに失敗しました: {err}"))
        })?,
        base_configured_revision,
        created_at,
        updated_at,
        requested_by_username,
        requested_by_role,
        failure_reason,
    })
}

#[derive(Clone)]
pub struct PendingChangesService {
    pool: SqlitePool,
}

impl PendingChangesService {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn create_pending(
        &self,
        source: &str,
        payload: &serde_json::Value,
        base_configured_revision: i64,
        requested_by_username: Option<&str>,
        requested_by_role: Option<&str>,
    ) -> Result<PendingChange, BantoError> {
        let payload_json = serde_json::to_string(payload).map_err(|err| {
            BantoError::Other(format!("payload のシリアライズに失敗しました: {err}"))
        })?;
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO pending_changes (
               state,
               source,
               payload,
               base_configured_revision,
               requested_by_username,
               requested_by_role
             ) VALUES (?, ?, ?, ?, ?, ?) RETURNING id",
        )
        .bind(PendingChangeState::Pending.as_str())
        .bind(source)
        .bind(payload_json)
        .bind(base_configured_revision)
        .bind(requested_by_username)
        .bind(requested_by_role)
        .fetch_one(&self.pool)
        .await
        .map_err(banto_storage::storage_error)?;

        self.get(id).await
    }

    pub async fn get(&self, id: i64) -> Result<PendingChange, BantoError> {
        let row: Option<PendingRow> = sqlx::query_as(
            "SELECT
               id,
               state,
               source,
               payload,
               base_configured_revision,
               created_at,
               updated_at,
               requested_by_username,
               requested_by_role,
               failure_reason
             FROM pending_changes
             WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(banto_storage::storage_error)?;

        match row {
            Some(row) => row_to_pending(row),
            None => Err(BantoError::NotFound {
                resource: "pending_changes".to_string(),
                id: id.to_string(),
            }),
        }
    }

    pub async fn list(&self, limit: i64) -> Result<Vec<PendingChange>, BantoError> {
        let limit = limit.clamp(1, 1000);
        let rows: Vec<PendingRow> = sqlx::query_as(
            "SELECT
               id,
               state,
               source,
               payload,
               base_configured_revision,
               created_at,
               updated_at,
               requested_by_username,
               requested_by_role,
               failure_reason
             FROM pending_changes
             ORDER BY id DESC
             LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(banto_storage::storage_error)?;

        rows.into_iter().map(row_to_pending).collect()
    }

    pub async fn cancel_pending(&self, id: i64) -> Result<PendingChange, BantoError> {
        let row: Option<PendingRow> = sqlx::query_as(
            "UPDATE pending_changes
             SET
               state = ?,
               updated_at = datetime('now')
             WHERE id = ? AND state = ?
             RETURNING
               id,
               state,
               source,
               payload,
               base_configured_revision,
               created_at,
               updated_at,
               requested_by_username,
               requested_by_role,
               failure_reason",
        )
        .bind(PendingChangeState::Canceled.as_str())
        .bind(id)
        .bind(PendingChangeState::Pending.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(banto_storage::storage_error)?;

        if let Some(row) = row {
            return row_to_pending(row);
        }

        let current = self.get(id).await?;
        if current.state == PendingChangeState::Canceled {
            return Ok(current);
        }
        Err(BantoError::Other(
            "pending 状態以外の提案はキャンセルできません".to_string(),
        ))
    }

    pub async fn start_applying(&self, id: i64) -> Result<PendingChange, BantoError> {
        let row: Option<PendingRow> = sqlx::query_as(
            "UPDATE pending_changes
             SET
               state = ?,
               failure_reason = NULL,
               updated_at = datetime('now')
             WHERE id = ? AND state = ?
             RETURNING
               id,
               state,
               source,
               payload,
               base_configured_revision,
               created_at,
               updated_at,
               requested_by_username,
               requested_by_role,
               failure_reason",
        )
        .bind(PendingChangeState::Applying.as_str())
        .bind(id)
        .bind(PendingChangeState::Pending.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(banto_storage::storage_error)?;

        if let Some(row) = row {
            return row_to_pending(row);
        }

        let current = self.get(id).await?;
        Err(BantoError::Other(format!(
            "pending 状態以外の提案は適用開始できません(state={})",
            current.state.as_str()
        )))
    }

    pub async fn mark_applied(&self, id: i64) -> Result<PendingChange, BantoError> {
        let row: Option<PendingRow> = sqlx::query_as(
            "UPDATE pending_changes
             SET
               state = ?,
               failure_reason = NULL,
               updated_at = datetime('now')
             WHERE id = ? AND state = ?
             RETURNING
               id,
               state,
               source,
               payload,
               base_configured_revision,
               created_at,
               updated_at,
               requested_by_username,
               requested_by_role,
               failure_reason",
        )
        .bind(PendingChangeState::Applied.as_str())
        .bind(id)
        .bind(PendingChangeState::Applying.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(banto_storage::storage_error)?;

        if let Some(row) = row {
            return row_to_pending(row);
        }

        let current = self.get(id).await?;
        Err(BantoError::Other(format!(
            "applying 状態以外の提案は適用完了にできません(state={})",
            current.state.as_str()
        )))
    }

    pub async fn mark_failed(&self, id: i64, reason: &str) -> Result<PendingChange, BantoError> {
        let row: Option<PendingRow> = sqlx::query_as(
            "UPDATE pending_changes
             SET
               state = ?,
               failure_reason = ?,
               updated_at = datetime('now')
             WHERE id = ? AND state = ?
             RETURNING
               id,
               state,
               source,
               payload,
               base_configured_revision,
               created_at,
               updated_at,
               requested_by_username,
               requested_by_role,
               failure_reason",
        )
        .bind(PendingChangeState::Failed.as_str())
        .bind(reason)
        .bind(id)
        .bind(PendingChangeState::Applying.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(banto_storage::storage_error)?;

        if let Some(row) = row {
            return row_to_pending(row);
        }

        let current = self.get(id).await?;
        Err(BantoError::Other(format!(
            "applying 状態以外の提案は失敗扱いにできません(state={})",
            current.state.as_str()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrate_memory;

    async fn service() -> PendingChangesService {
        let pool = migrate_memory().await.expect("migrate_memory");
        PendingChangesService::new(pool)
    }

    #[tokio::test]
    async fn create_and_get_round_trip() {
        let svc = service().await;
        let created = svc
            .create_pending(
                "tags.create",
                &serde_json::json!({ "name": "temp01" }),
                42,
                Some("admin"),
                Some("admin"),
            )
            .await
            .unwrap();
        assert_eq!(created.state, PendingChangeState::Pending);
        assert_eq!(created.source, "tags.create");
        assert_eq!(created.base_configured_revision, 42);

        let fetched = svc.get(created.id).await.unwrap();
        assert_eq!(fetched.id, created.id);
        assert_eq!(fetched.payload["name"], "temp01");
    }

    #[tokio::test]
    async fn list_returns_latest_first() {
        let svc = service().await;
        let first = svc
            .create_pending(
                "tags.create",
                &serde_json::json!({ "name": "a" }),
                1,
                None,
                None,
            )
            .await
            .unwrap();
        let second = svc
            .create_pending(
                "tags.create",
                &serde_json::json!({ "name": "b" }),
                2,
                None,
                None,
            )
            .await
            .unwrap();

        let listed = svc.list(100).await.unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].id, second.id);
        assert_eq!(listed[1].id, first.id);
    }

    #[tokio::test]
    async fn cancel_is_idempotent_for_already_canceled_rows() {
        let svc = service().await;
        let created = svc
            .create_pending(
                "tags.update",
                &serde_json::json!({ "id": 1 }),
                3,
                None,
                None,
            )
            .await
            .unwrap();

        let canceled = svc.cancel_pending(created.id).await.unwrap();
        assert_eq!(canceled.state, PendingChangeState::Canceled);

        let canceled_again = svc.cancel_pending(created.id).await.unwrap();
        assert_eq!(canceled_again.state, PendingChangeState::Canceled);
    }

    #[tokio::test]
    async fn applying_state_transitions_are_strict() {
        let svc = service().await;
        let created = svc
            .create_pending(
                "tags.create",
                &serde_json::json!({ "name": "strict" }),
                5,
                None,
                None,
            )
            .await
            .unwrap();

        let applying = svc.start_applying(created.id).await.unwrap();
        assert_eq!(applying.state, PendingChangeState::Applying);

        let failed = svc
            .mark_failed(created.id, "simulated failure")
            .await
            .unwrap();
        assert_eq!(failed.state, PendingChangeState::Failed);
        assert_eq!(failed.failure_reason.as_deref(), Some("simulated failure"));

        let err = svc.start_applying(created.id).await.unwrap_err();
        assert!(err
            .to_string()
            .contains("pending 状態以外の提案は適用開始できません"));
    }
}
