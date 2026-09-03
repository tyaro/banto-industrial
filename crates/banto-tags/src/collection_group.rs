//! Collection group (recorder-requirements.md §3.1: "収集周期はタグ毎ではなく
//! 収集グループ毎"): the unit of periodic PLC bulk read. Every
//! [`crate::tag::Tag`] belongs to exactly one group, and a group's
//! `period_ms` (one of [`ALLOWED_PERIOD_MS`]) is how often the collection
//! engine (I3) reads every tag in it in one PLC round-trip.

use banto_core::{BantoError, FieldError, ListParams, ListResult};
use banto_storage::ColumnMap;
use serde::{Deserialize, Serialize};
use sqlx::{QueryBuilder, Sqlite, SqliteConnection, SqlitePool};

use crate::support::{map_write_error, max_length_message, required_message, NAME_ALREADY_USED};

/// Selectable collection periods, milliseconds (recorder-requirements.md
/// §3.1: "標準 1s / 選択肢 100ms・200ms・500ms・2s・5s・10s・1min") - mirrors
/// the SQL `CHECK` in `migrations/0002_collection_groups.sql`.
pub const ALLOWED_PERIOD_MS: &[i64] = &[100, 200, 500, 1_000, 2_000, 5_000, 10_000, 60_000];

const MAX_NAME_LEN: usize = 100;

fn default_enabled() -> bool {
    true
}

/// T19 S1-b（UX-34、docs/banto-hub-t19-design.md §2・§3.3、2026-09-02
/// オーナー決定）: `default_writable` の既定値。UX-34 の全体方針「既定
/// ON」に合わせ、`CollectionGroupInput` に列を省略した既存クライアント
/// （relay-wright/chronogazer にはこの列自体が無いので該当しないが、将来
/// 追従が遅れた banto-hub クライアントを想定した後方互換）は「既定 ON」の
/// グループとして作成される - `migrations/0012_collection_groups_add_
/// default_writable.sql` の列既定値 `1` と揃えてある。
fn default_writable_true() -> bool {
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
    /// T19 S1-b（UX-34）: このグループへ新規タグを登録するときの
    /// `writable` チェックボックスの初期値（banto-hub の UI 向け既定値で
    /// あり、`tags.writable` 自体の検証ルールとは無関係 - `tag.rs::
    /// validate_tag_input` の computed タグ拒否・Modbus 読み取り専用領域
    /// 拒否はこの値に関係なくそのまま効く）。relay-wright/chronogazer の
    /// 収集・書き込み動作はこの列を一切参照しない（`migrations/0012_
    /// collection_groups_add_default_writable.sql` 参照）。
    pub default_writable: bool,
}

/// Create/update payload.
#[derive(Debug, Clone, Deserialize)]
pub struct CollectionGroupInput {
    pub name: String,
    pub plc_connection_id: i64,
    pub period_ms: i64,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// T19 S1-b（UX-34）: 省略時は `default_writable_true`（既定 ON、
    /// migration 0012 の列既定値と同じ）。
    #[serde(default = "default_writable_true")]
    pub default_writable: bool,
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
        .column("defaultWritable", "default_writable")
}

const RESOURCE: &str = "collection_groups";
const COLUMNS: &str = "id, name, plc_connection_id, period_ms, enabled, default_writable";
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

        let mut rows_builder: QueryBuilder<Sqlite> =
            QueryBuilder::new(format!("SELECT {COLUMNS} FROM collection_groups"));
        banto_storage::list_query::sqlite::apply_list_params(&mut rows_builder, &columns, &params)?;
        let rows: Vec<CollectionGroup> = rows_builder
            .build_query_as::<CollectionGroup>()
            .fetch_all(&self.pool)
            .await
            .map_err(banto_storage::storage_error)?;

        let mut count_builder: QueryBuilder<Sqlite> =
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
        // AssertSqlSafe: 補間されるのは COLUMNS 定数（本ファイル内の固定文字列）
        // のみで、外部入力は含まれない。id はプレースホルダでバインドする。
        sqlx::query_as::<_, CollectionGroup>(sqlx::AssertSqlSafe(format!(
            "SELECT {COLUMNS} FROM collection_groups WHERE id = ?"
        )))
        .bind(id)
        .fetch_one(&self.pool)
        .await
        .map_err(|err| banto_storage::not_found(err, RESOURCE, id.to_string()))
    }

    pub async fn create(&self, input: CollectionGroupInput) -> Result<CollectionGroup, BantoError> {
        validate_collection_group_input(&input)?;
        // AssertSqlSafe: get() と同じ理由 - COLUMNS 定数のみを埋め込む固定
        // 文字列。値はすべてプレースホルダでバインドする。
        sqlx::query_as::<_, CollectionGroup>(sqlx::AssertSqlSafe(format!(
            "INSERT INTO collection_groups (name, plc_connection_id, period_ms, enabled, default_writable) \
             VALUES (?, ?, ?, ?, ?) RETURNING {COLUMNS}"
        )))
        .bind(input.name.trim())
        .bind(input.plc_connection_id)
        .bind(input.period_ms)
        .bind(input.enabled)
        .bind(input.default_writable)
        .fetch_one(&self.pool)
        .await
        .map_err(|err| {
            map_write_error(
                err,
                "name",
                NAME_ALREADY_USED,
                "plcConnectionId",
                FK_MESSAGE,
            )
        })
    }

    /// Transaction-compatible counterpart of [`Self::create`].
    pub async fn create_tx(
        &self,
        connection: &mut SqliteConnection,
        input: CollectionGroupInput,
    ) -> Result<CollectionGroup, BantoError> {
        validate_collection_group_input(&input)?;
        // AssertSqlSafe: get() と同じ理由 - COLUMNS 定数のみを埋め込む固定
        // 文字列。値はすべてプレースホルダでバインドする。
        sqlx::query_as::<_, CollectionGroup>(sqlx::AssertSqlSafe(format!(
            "INSERT INTO collection_groups (name, plc_connection_id, period_ms, enabled, default_writable) \
             VALUES (?, ?, ?, ?, ?) RETURNING {COLUMNS}"
        )))
        .bind(input.name.trim())
        .bind(input.plc_connection_id)
        .bind(input.period_ms)
        .bind(input.enabled)
        .bind(input.default_writable)
        .fetch_one(&mut *connection)
        .await
        .map_err(|err| {
            map_write_error(
                err,
                "name",
                NAME_ALREADY_USED,
                "plcConnectionId",
                FK_MESSAGE,
            )
        })
    }

    pub async fn update(
        &self,
        id: i64,
        input: CollectionGroupInput,
    ) -> Result<CollectionGroup, BantoError> {
        validate_collection_group_input(&input)?;
        // AssertSqlSafe: get() と同じ理由 - COLUMNS 定数のみを埋め込む固定
        // 文字列。値はすべてプレースホルダでバインドする。
        sqlx::query_as::<_, CollectionGroup>(sqlx::AssertSqlSafe(format!(
            "UPDATE collection_groups SET name = ?, plc_connection_id = ?, period_ms = ?, enabled = ?, default_writable = ? \
             WHERE id = ? RETURNING {COLUMNS}"
        )))
        .bind(input.name.trim())
        .bind(input.plc_connection_id)
        .bind(input.period_ms)
        .bind(input.enabled)
        .bind(input.default_writable)
        .bind(id)
        .fetch_one(&self.pool)
        .await
        .map_err(|err| match err {
            sqlx::Error::RowNotFound => BantoError::NotFound {
                resource: RESOURCE.to_string(),
                id: id.to_string(),
            },
            other => map_write_error(other, "name", NAME_ALREADY_USED, "plcConnectionId", FK_MESSAGE),
        })
    }

    /// Transaction-compatible counterpart of [`Self::update`].
    pub async fn update_tx(
        &self,
        connection: &mut SqliteConnection,
        id: i64,
        input: CollectionGroupInput,
    ) -> Result<CollectionGroup, BantoError> {
        validate_collection_group_input(&input)?;
        // AssertSqlSafe: get() と同じ理由 - COLUMNS 定数のみを埋め込む固定
        // 文字列。値はすべてプレースホルダでバインドする。
        sqlx::query_as::<_, CollectionGroup>(sqlx::AssertSqlSafe(format!(
            "UPDATE collection_groups SET name = ?, plc_connection_id = ?, period_ms = ?, enabled = ?, default_writable = ? \
             WHERE id = ? RETURNING {COLUMNS}"
        )))
        .bind(input.name.trim())
        .bind(input.plc_connection_id)
        .bind(input.period_ms)
        .bind(input.enabled)
        .bind(input.default_writable)
        .bind(id)
        .fetch_one(&mut *connection)
        .await
        .map_err(|err| match err {
            sqlx::Error::RowNotFound => BantoError::NotFound {
                resource: RESOURCE.to_string(),
                id: id.to_string(),
            },
            other => map_write_error(other, "name", NAME_ALREADY_USED, "plcConnectionId", FK_MESSAGE),
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

    /// Transaction-compatible counterpart of [`Self::delete`].
    pub async fn delete_tx(
        &self,
        connection: &mut SqliteConnection,
        id: i64,
    ) -> Result<(), BantoError> {
        let tag_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM tags WHERE collection_group_id = ?")
                .bind(id)
                .fetch_one(&mut *connection)
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
            .execute(&mut *connection)
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

    /// T19 S2-b（UX-38、docs/banto-hub-t19-design.md §3.4・§7.5）: カスケード
    /// 削除の入口 - [`crate::plc_connection::PlcConnectionService::cascade_delete_tx`]
    /// の doc comment がここでの設計判断（[`Self::delete`]/[`Self::delete_tx`]
    /// は変更せず新メソッドを足す理由 - relay-wright との契約）をそのまま
    /// 説明しているので、そちらを参照。このグループに属する全タグ →
    /// このグループ、の順（FK 順）で削除する。tstore の履歴には触れない
    /// （同じ理由 - このクレートは banto-tstore に依存しない）。
    pub async fn cascade_delete_tx(
        &self,
        connection: &mut SqliteConnection,
        id: i64,
    ) -> Result<CollectionGroupCascadeOutcome, BantoError> {
        let deleted_tags = sqlx::query("DELETE FROM tags WHERE collection_group_id = ?")
            .bind(id)
            .execute(&mut *connection)
            .await
            .map_err(banto_storage::storage_error)?
            .rows_affected() as i64;

        let result = sqlx::query("DELETE FROM collection_groups WHERE id = ?")
            .bind(id)
            .execute(&mut *connection)
            .await
            .map_err(banto_storage::storage_error)?;
        if result.rows_affected() == 0 {
            return Err(BantoError::NotFound {
                resource: RESOURCE.to_string(),
                id: id.to_string(),
            });
        }

        Ok(CollectionGroupCascadeOutcome { deleted_tags })
    }

    /// Non-transactional counterpart of [`Self::cascade_delete_tx`] - opens
    /// its own transaction so both DELETEs stay atomic (mirrors
    /// [`crate::plc_connection::PlcConnectionService::cascade_delete`]).
    pub async fn cascade_delete(
        &self,
        id: i64,
    ) -> Result<CollectionGroupCascadeOutcome, BantoError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(banto_storage::storage_error)?;
        let outcome = self.cascade_delete_tx(&mut tx, id).await?;
        tx.commit().await.map_err(banto_storage::storage_error)?;
        Ok(outcome)
    }
}

/// What [`CollectionGroupService::cascade_delete`]/[`CollectionGroupService::cascade_delete_tx`]
/// removed besides the group row itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectionGroupCascadeOutcome {
    pub deleted_tags: i64,
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
                simulation: false,

                word_order: "low_high".to_string(),
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
            default_writable: true,
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
        assert!(created.default_writable);

        let fetched = svc.get(created.id).await.expect("get should succeed");
        assert_eq!(fetched, created);
    }

    /// T19 S1-b（UX-34）: `default_writable` を省略した入力（後方互換 -
    /// 現行 banto-hub クライアントは常に明示的に送るが、JSON 側の
    /// `#[serde(default)]` を裏付けるテスト）は既定 `true` になる。
    #[tokio::test]
    async fn create_defaults_default_writable_to_true_when_json_omits_it() {
        let (_plc_svc, svc, conn_id) = setup().await;
        // `CollectionGroupInput` itself deserializes snake_case (no
        // `#[serde(rename_all = "camelCase")]` - only `CollectionGroup`, the
        // read model, is wire-shaped that way; the camelCase mirroring for
        // input happens one layer up, in `banto_hub_core::rest::
        // CollectionGroupPayload`). This test exercises this crate's own
        // `#[serde(default)]` directly, so it uses this type's own field
        // names.
        let input: CollectionGroupInput = serde_json::from_value(json!({
            "name": "NoDefaultWritableField",
            "plc_connection_id": conn_id,
            "period_ms": 1_000,
        }))
        .expect("deserialize should succeed without default_writable");
        let created = svc.create(input).await.expect("create should succeed");
        assert!(created.default_writable);
    }

    /// T19 S1-b（UX-34「収集グループ単位で既定値を変更できる」）:
    /// `default_writable: false` で作成・更新した値が往復する。
    #[tokio::test]
    async fn create_and_update_round_trip_default_writable_false() {
        let (_plc_svc, svc, conn_id) = setup().await;
        let mut input = sample_input("OptOut", conn_id);
        input.default_writable = false;
        let created = svc.create(input).await.expect("create should succeed");
        assert!(!created.default_writable);

        let mut update_input = sample_input("OptOut", conn_id);
        update_input.default_writable = true;
        let updated = svc
            .update(created.id, update_input)
            .await
            .expect("update should succeed");
        assert!(updated.default_writable);
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

    // --- T19 S2-b (UX-38, docs/banto-hub-t19-design.md §3.4/§7.5): cascade
    // delete - a NEW method, `delete`/`delete_tx` above are unchanged (see
    // `CollectionGroupService::cascade_delete_tx`'s doc comment, which points
    // at `PlcConnectionService::cascade_delete_tx`'s longer explanation of
    // why: relay-wright depends on the guarded methods refusing).

    /// The core UX-38 behavior for a group: tags go with it in one call.
    #[tokio::test]
    async fn cascade_delete_removes_tags_with_the_group() {
        let (_plc_svc, svc, conn_id) = setup().await;
        let group = svc.create(sample_input("InUse", conn_id)).await.unwrap();
        for name in ["T1", "T2"] {
            sqlx::query(
                "INSERT INTO tags (name, collection_group_id, address, data_type) \
                 VALUES (?, ?, '40001', 'i16')",
            )
            .bind(name)
            .bind(group.id)
            .execute(&svc.pool)
            .await
            .unwrap();
        }

        let outcome = svc
            .cascade_delete(group.id)
            .await
            .expect("cascade_delete should succeed even with tags");
        assert_eq!(outcome.deleted_tags, 2);

        assert!(matches!(
            svc.get(group.id).await.unwrap_err(),
            BantoError::NotFound { .. }
        ));
        let remaining_tags: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tags")
            .fetch_one(&svc.pool)
            .await
            .unwrap();
        assert_eq!(remaining_tags, 0);
    }

    /// A childless group behaves exactly like the old `delete`.
    #[tokio::test]
    async fn cascade_delete_with_no_tags_behaves_like_a_plain_delete() {
        let (_plc_svc, svc, conn_id) = setup().await;
        let group = svc.create(sample_input("Lonely", conn_id)).await.unwrap();
        let outcome = svc.cascade_delete(group.id).await.unwrap();
        assert_eq!(outcome.deleted_tags, 0);
        assert!(matches!(
            svc.get(group.id).await.unwrap_err(),
            BantoError::NotFound { .. }
        ));
    }

    #[tokio::test]
    async fn cascade_delete_missing_id_is_not_found() {
        let (_plc_svc, svc, _conn_id) = setup().await;
        let err = svc.cascade_delete(999).await.unwrap_err();
        assert!(matches!(err, BantoError::NotFound { .. }));
    }

    /// Same atomicity contract as
    /// `plc_connection::tests::cascade_delete_tx_rolls_back_together_with_the_callers_transaction`:
    /// `cascade_delete_tx` must not commit anything by itself, so a caller
    /// (`apps/banto-hub/core/src/rest.rs::collection_groups_delete`) that
    /// rolls its own transaction back after a failed preflight leaves both
    /// the group and its tags untouched.
    #[tokio::test]
    async fn cascade_delete_tx_rolls_back_together_with_the_callers_transaction() {
        let (_plc_svc, svc, conn_id) = setup().await;
        let group = svc.create(sample_input("Rollback", conn_id)).await.unwrap();
        sqlx::query(
            "INSERT INTO tags (name, collection_group_id, address, data_type) \
             VALUES ('T1', ?, '40001', 'i16')",
        )
        .bind(group.id)
        .execute(&svc.pool)
        .await
        .unwrap();

        let mut tx = svc.pool.begin().await.unwrap();
        let outcome = svc.cascade_delete_tx(&mut tx, group.id).await.unwrap();
        assert_eq!(outcome.deleted_tags, 1);
        tx.rollback().await.unwrap();

        svc.get(group.id)
            .await
            .expect("group should survive the rollback");
        let remaining_tags: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tags")
            .fetch_one(&svc.pool)
            .await
            .unwrap();
        assert_eq!(remaining_tags, 1);
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
