//! Collection group (recorder-requirements.md §3.1: "収集周期はタグ毎ではなく
//! 収集グループ毎"): the unit of periodic PLC bulk read. Every
//! [`crate::tag::Tag`] belongs to exactly one group, and a group's
//! `period_ms` (one of [`ALLOWED_PERIOD_MS`]) is how often the collection
//! engine (I3) reads every tag in it in one PLC round-trip.

use banto_core::{BantoError, FieldError, ListParams, ListResult};
use banto_storage::ColumnMap;
use serde::{Deserialize, Serialize};
use sqlx::{QueryBuilder, Sqlite, SqlitePool};

use crate::support::{map_write_error, max_length_message, required_message};

/// Selectable collection periods, milliseconds (recorder-requirements.md
/// §3.1: "標準 1s / 選択肢 100ms・200ms・500ms・2s・5s・10s・1min") - mirrors
/// the SQL `CHECK` in `migrations/0002_collection_groups.sql`.
pub const ALLOWED_PERIOD_MS: &[i64] = &[100, 200, 500, 1_000, 2_000, 5_000, 10_000, 60_000];

const MAX_NAME_LEN: usize = 100;

fn default_enabled() -> bool {
    true
}

/// A row of the `collection_groups` table, wire-shaped (camelCase) for a
/// future settings grid (recorder-requirements.md §6 "グループ設定" screen).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct CollectionGroup {
    pub id: i64,
    pub name: String,
    pub plc_connection_id: i64,
    pub period_ms: i64,
    pub enabled: bool,
}

/// Create/update payload.
#[derive(Debug, Clone, Deserialize)]
pub struct CollectionGroupInput {
    pub name: String,
    pub plc_connection_id: i64,
    pub period_ms: i64,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

/// Validate a [`CollectionGroupInput`]: `name` trimmed non-empty and capped
/// at `MAX_NAME_LEN`, `period_ms` in [`ALLOWED_PERIOD_MS`]. `plc_connection_id`
/// referring to a real row is enforced by the `FOREIGN KEY` at write time
/// (see [`crate::support::map_write_error`]), not here - a plain existence
/// check here would be a second round trip for something the database
/// constraint already guarantees atomically.
fn validate_collection_group_input(input: &CollectionGroupInput) -> Result<(), BantoError> {
    let mut errors: Vec<FieldError> = Vec::new();

    let trimmed_name = input.name.trim();
    if trimmed_name.is_empty() {
        errors.push(FieldError {
            field: "name".to_string(),
            message: required_message(),
        });
    } else if trimmed_name.chars().count() > MAX_NAME_LEN {
        errors.push(FieldError {
            field: "name".to_string(),
            message: max_length_message(MAX_NAME_LEN),
        });
    }

    if !ALLOWED_PERIOD_MS.contains(&input.period_ms) {
        let choices = ALLOWED_PERIOD_MS
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        errors.push(FieldError {
            field: "periodMs".to_string(),
            message: format!("収集周期は次のいずれかを選択してください: {choices}"),
        });
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(BantoError::Validation {
            field_errors: errors,
        })
    }
}

fn column_map() -> ColumnMap {
    ColumnMap::new()
        .column("id", "id")
        .column("name", "name")
        .column("plcConnectionId", "plc_connection_id")
        .column("periodMs", "period_ms")
        .column("enabled", "enabled")
}

const RESOURCE: &str = "collection_groups";
const COLUMNS: &str = "id, name, plc_connection_id, period_ms, enabled";
const FK_MESSAGE: &str = "指定されたPLC接続が見つかりません";

/// Service layer for the `collection_groups` resource.
#[derive(Clone)]
pub struct CollectionGroupService {
    pool: SqlitePool,
}

impl CollectionGroupService {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn list(
        &self,
        params: ListParams,
    ) -> Result<ListResult<CollectionGroup>, BantoError> {
        let columns = column_map();

        let mut rows_builder: QueryBuilder<'_, Sqlite> =
            QueryBuilder::new(format!("SELECT {COLUMNS} FROM collection_groups"));
        banto_storage::list_query::sqlite::apply_list_params(&mut rows_builder, &columns, &params)?;
        let rows: Vec<CollectionGroup> = rows_builder
            .build_query_as::<CollectionGroup>()
            .fetch_all(&self.pool)
            .await
            .map_err(banto_storage::storage_error)?;

        let mut count_builder: QueryBuilder<'_, Sqlite> =
            QueryBuilder::new("SELECT COUNT(*) FROM collection_groups");
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

    pub async fn get(&self, id: i64) -> Result<CollectionGroup, BantoError> {
        sqlx::query_as::<_, CollectionGroup>(&format!(
            "SELECT {COLUMNS} FROM collection_groups WHERE id = ?"
        ))
        .bind(id)
        .fetch_one(&self.pool)
        .await
        .map_err(|err| banto_storage::not_found(err, RESOURCE, id.to_string()))
    }

    pub async fn create(&self, input: CollectionGroupInput) -> Result<CollectionGroup, BantoError> {
        validate_collection_group_input(&input)?;
        sqlx::query_as::<_, CollectionGroup>(&format!(
            "INSERT INTO collection_groups (name, plc_connection_id, period_ms, enabled) \
             VALUES (?, ?, ?, ?) RETURNING {COLUMNS}"
        ))
        .bind(input.name.trim())
        .bind(input.plc_connection_id)
        .bind(input.period_ms)
        .bind(input.enabled)
        .fetch_one(&self.pool)
        .await
        .map_err(|err| map_write_error(err, "name", "plcConnectionId", FK_MESSAGE))
    }

    pub async fn update(
        &self,
        id: i64,
        input: CollectionGroupInput,
    ) -> Result<CollectionGroup, BantoError> {
        validate_collection_group_input(&input)?;
        sqlx::query_as::<_, CollectionGroup>(&format!(
            "UPDATE collection_groups SET name = ?, plc_connection_id = ?, period_ms = ?, enabled = ? \
             WHERE id = ? RETURNING {COLUMNS}"
        ))
        .bind(input.name.trim())
        .bind(input.plc_connection_id)
        .bind(input.period_ms)
        .bind(input.enabled)
        .bind(id)
        .fetch_one(&self.pool)
        .await
        .map_err(|err| match err {
            sqlx::Error::RowNotFound => BantoError::NotFound {
                resource: RESOURCE.to_string(),
                id: id.to_string(),
            },
            other => map_write_error(other, "name", "plcConnectionId", FK_MESSAGE),
        })
    }

    /// Delete, refusing when any [`crate::tag::Tag`] still references this
    /// group (docs/plan.md I1 spec: same "在籍タグ/グループ数を数えて
    /// Validation エラー" rule as
    /// [`crate::plc_connection::PlcConnectionService::delete`]).
    pub async fn delete(&self, id: i64) -> Result<(), BantoError> {
        let tag_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM tags WHERE collection_group_id = ?")
                .bind(id)
                .fetch_one(&self.pool)
                .await
                .map_err(banto_storage::storage_error)?;
        if tag_count > 0 {
            return Err(BantoError::Validation {
                field_errors: vec![FieldError {
                    field: "id".to_string(),
                    message: format!(
                        "このグループに属するタグが{tag_count}件あるため削除できません"
                    ),
                }],
            });
        }

        let result = sqlx::query("DELETE FROM collection_groups WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(banto_storage::storage_error)?;
        if result.rows_affected() == 0 {
            return Err(BantoError::NotFound {
                resource: RESOURCE.to_string(),
                id: id.to_string(),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrate;
    use crate::plc_connection::{PlcConnectionInput, PlcConnectionService};
    use banto_core::{FilterOp, FilterState, Pagination, SortDirection, SortState};
    use serde_json::json;

    async fn setup() -> (PlcConnectionService, CollectionGroupService, i64) {
        let pool = banto_storage::connect_sqlite_memory()
            .await
            .expect("connect_sqlite_memory");
        migrate(&pool).await.expect("migrate");
        let plc_svc = PlcConnectionService::new(pool.clone());
        let group_svc = CollectionGroupService::new(pool);
        let conn = plc_svc
            .create(PlcConnectionInput {
                name: "PLC1".to_string(),
                protocol: "modbus-tcp".to_string(),
                host: "10.0.0.1".to_string(),
                port: 502,
                unit_id: 1,
                enabled: true,
            })
            .await
            .unwrap();
        (plc_svc, group_svc, conn.id)
    }

    fn sample_input(name: &str, plc_connection_id: i64) -> CollectionGroupInput {
        CollectionGroupInput {
            name: name.to_string(),
            plc_connection_id,
            period_ms: 1_000,
            enabled: true,
        }
    }

    #[tokio::test]
    async fn create_then_get_round_trips() {
        let (_plc_svc, svc, conn_id) = setup().await;
        let created = svc
            .create(sample_input("Group1", conn_id))
            .await
            .expect("create should succeed");
        assert_eq!(created.name, "Group1");
        assert_eq!(created.plc_connection_id, conn_id);
        assert_eq!(created.period_ms, 1_000);
        assert!(created.enabled);

        let fetched = svc.get(created.id).await.expect("get should succeed");
        assert_eq!(fetched, created);
    }

    #[tokio::test]
    async fn create_rejects_empty_name() {
        let (_plc_svc, svc, conn_id) = setup().await;
        let mut input = sample_input("X", conn_id);
        input.name = "  ".to_string();
        let err = svc.create(input).await.unwrap_err();
        match err {
            BantoError::Validation { field_errors } => assert_eq!(field_errors[0].field, "name"),
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_rejects_name_over_max_len() {
        let (_plc_svc, svc, conn_id) = setup().await;
        let mut input = sample_input("X", conn_id);
        input.name = "あ".repeat(MAX_NAME_LEN + 1);
        let err = svc.create(input).await.unwrap_err();
        match err {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "name");
                assert_eq!(field_errors[0].message, max_length_message(MAX_NAME_LEN));
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_accepts_every_allowed_period() {
        let (_plc_svc, svc, conn_id) = setup().await;
        for (i, period) in ALLOWED_PERIOD_MS.iter().enumerate() {
            let mut input = sample_input(&format!("G{i}"), conn_id);
            input.period_ms = *period;
            svc.create(input)
                .await
                .unwrap_or_else(|e| panic!("period {period} should be accepted: {e:?}"));
        }
    }

    #[tokio::test]
    async fn create_rejects_disallowed_period() {
        let (_plc_svc, svc, conn_id) = setup().await;
        let mut input = sample_input("X", conn_id);
        input.period_ms = 750;
        let err = svc.create(input).await.unwrap_err();
        match err {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "periodMs")
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_rejects_duplicate_name() {
        let (_plc_svc, svc, conn_id) = setup().await;
        svc.create(sample_input("Dup", conn_id)).await.unwrap();
        let err = svc.create(sample_input("Dup", conn_id)).await.unwrap_err();
        match err {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "name");
                assert_eq!(field_errors[0].message, "既に使用されています");
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_rejects_missing_plc_connection_with_friendly_message() {
        let (_plc_svc, svc, _conn_id) = setup().await;
        let err = svc.create(sample_input("X", 999)).await.unwrap_err();
        match err {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "plcConnectionId");
                assert_eq!(field_errors[0].message, FK_MESSAGE);
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn update_changes_fields() {
        let (_plc_svc, svc, conn_id) = setup().await;
        let created = svc.create(sample_input("Before", conn_id)).await.unwrap();
        let mut input = sample_input("After", conn_id);
        input.period_ms = 5_000;
        let updated = svc.update(created.id, input).await.expect("update ok");
        assert_eq!(updated.name, "After");
        assert_eq!(updated.period_ms, 5_000);
    }

    #[tokio::test]
    async fn update_missing_id_is_not_found() {
        let (_plc_svc, svc, conn_id) = setup().await;
        let err = svc
            .update(999, sample_input("X", conn_id))
            .await
            .unwrap_err();
        assert!(
            matches!(err, BantoError::NotFound { resource, id } if resource == "collection_groups" && id == "999")
        );
    }

    #[tokio::test]
    async fn delete_then_get_is_not_found() {
        let (_plc_svc, svc, conn_id) = setup().await;
        let created = svc.create(sample_input("Doomed", conn_id)).await.unwrap();
        svc.delete(created.id).await.expect("delete should succeed");
        let err = svc.get(created.id).await.unwrap_err();
        assert!(matches!(err, BantoError::NotFound { .. }));
    }

    #[tokio::test]
    async fn delete_missing_id_is_not_found() {
        let (_plc_svc, svc, _conn_id) = setup().await;
        let err = svc.delete(999).await.unwrap_err();
        assert!(matches!(err, BantoError::NotFound { .. }));
    }

    #[tokio::test]
    async fn delete_refuses_when_a_tag_references_it() {
        let (_plc_svc, svc, conn_id) = setup().await;
        let group = svc.create(sample_input("InUse", conn_id)).await.unwrap();

        sqlx::query(
            "INSERT INTO tags (name, collection_group_id, address, data_type, decimals, enabled) \
             VALUES ('T1', ?, '40001', 'i16', 0, 1)",
        )
        .bind(group.id)
        .execute(&svc.pool)
        .await
        .unwrap();

        let err = svc.delete(group.id).await.unwrap_err();
        match err {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "id");
                assert!(field_errors[0].message.contains('1'));
            }
            other => panic!("expected Validation, got {other:?}"),
        }
        svc.get(group.id).await.expect("group should survive");
    }

    #[tokio::test]
    async fn list_filters_sorts_and_paginates_with_total_count() {
        let (_plc_svc, svc, conn_id) = setup().await;
        for (name, period) in [("A", 100), ("B", 1_000), ("C", 5_000)] {
            let mut input = sample_input(name, conn_id);
            input.period_ms = period;
            svc.create(input).await.unwrap();
        }

        let result = svc
            .list(ListParams {
                sort: vec![SortState {
                    field: "periodMs".to_string(),
                    direction: SortDirection::Desc,
                }],
                filters: vec![FilterState {
                    field: "periodMs".to_string(),
                    op: FilterOp::Gte,
                    value: json!(1_000),
                }],
                pagination: Some(Pagination {
                    offset: 0,
                    limit: 1,
                }),
            })
            .await
            .expect("list should succeed");

        assert_eq!(result.total_count, 2);
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0].name, "C");
    }
}
