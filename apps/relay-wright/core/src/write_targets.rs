//! Write target: one PLC device this app may write a value to (plan
//! `luminous-discovering-goblet.md`, W1/W2). Structurally symmetric with
//! banto-tags' [`banto_tags::Tag`] (`crates/banto-tags/src/tag.rs`) - the
//! same `name`/`plc_connection_id`/`address`/`data_type`/optional-scaling/
//! `unit`/`decimals`/`enabled` shape - minus the H/HH/L/LL thresholds (a
//! write target has no alarm thresholds), backed by the `write_targets`
//! table (`migrations/0005_write_targets.sql`).
//!
//! ## Invariants (docs/conventions.md)
//! - §2 (サービス層非依存): this service is `Clone` + `SqlitePool` +
//!   `BantoError` only - no tauri/axum/RBAC/HTTP. Authorization and audit are
//!   added by the wiring layer (`crate::rest` / `src-tauri`).
//! - SQL columns are only ever reached through the [`column_map`] whitelist
//!   (list filter/sort), never string-interpolated from caller input.
//!
//! `plc_connection_id` is validated against the banto-tags-owned
//! `plc_connections` table at the service layer (there is no SQL FOREIGN KEY
//! across the two migrator lineages - see the migration's own doc comment),
//! the same precedent banto-tags' `Tag` follows for `collection_group_id`,
//! except the referenced table lives in a different crate's migration set.

use banto_core::{BantoError, FieldError, ListParams, ListResult};
use banto_storage::ColumnMap;
use serde::{Deserialize, Serialize};
use sqlx::{QueryBuilder, Sqlite, SqlitePool};

use crate::support::{map_write_error, max_length_message, range_message, required_message};

const MAX_NAME_LEN: usize = 100;
const MIN_DECIMALS: i64 = 0;
const MAX_DECIMALS: i64 = 6;

fn default_decimals() -> i64 {
    0
}

fn default_enabled() -> bool {
    true
}

/// A row of the `write_targets` table, wire-shaped (camelCase) for the W2
/// registry grid.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct WriteTarget {
    pub id: i64,
    pub name: String,
    pub plc_connection_id: i64,
    pub address: String,
    pub data_type: String,
    pub raw_lo: Option<f64>,
    pub raw_hi: Option<f64>,
    pub eng_lo: Option<f64>,
    pub eng_hi: Option<f64>,
    pub unit: Option<String>,
    pub decimals: i64,
    pub enabled: bool,
}

impl WriteTarget {
    /// The write target's scaling, if any (all-four-set or all-none, enforced
    /// at create/update time via [`banto_tags::Scaling::from_parts`], exactly
    /// as `Tag::scaling` does).
    pub fn scaling(&self) -> Option<banto_tags::Scaling> {
        match (self.raw_lo, self.raw_hi, self.eng_lo, self.eng_hi) {
            (Some(raw_lo), Some(raw_hi), Some(eng_lo), Some(eng_hi)) => Some(banto_tags::Scaling {
                raw_lo,
                raw_hi,
                eng_lo,
                eng_hi,
            }),
            _ => None,
        }
    }
}

/// Create/update payload.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteTargetInput {
    pub name: String,
    pub plc_connection_id: i64,
    pub address: String,
    pub data_type: String,
    #[serde(default)]
    pub raw_lo: Option<f64>,
    #[serde(default)]
    pub raw_hi: Option<f64>,
    #[serde(default)]
    pub eng_lo: Option<f64>,
    #[serde(default)]
    pub eng_hi: Option<f64>,
    #[serde(default)]
    pub unit: Option<String>,
    #[serde(default = "default_decimals")]
    pub decimals: i64,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

/// Normalized (trimmed) string fields extracted from a [`WriteTargetInput`],
/// so create/update bind exactly what validation already checked.
struct Normalized {
    name: String,
    address: String,
    unit: Option<String>,
}

/// Collect EVERY field violation of a [`WriteTargetInput`] (not just the
/// first - mirrors `banto_tags::tag::validate_tag_input`), returning them
/// alongside the normalized string fields the caller binds on success. The
/// cross-lineage `plc_connection_id` existence check is done separately (it
/// needs the pool) and appended to this list by the caller.
fn collect_errors(input: &WriteTargetInput) -> (Vec<FieldError>, Normalized) {
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

    let trimmed_address = input.address.trim();
    if trimmed_address.is_empty() {
        errors.push(FieldError {
            field: "address".to_string(),
            message: required_message(),
        });
    }

    // Reuse banto-tags' canonical data-type list so the two never drift
    // (this app's SQL CHECK in 0005 is the same set).
    if !banto_tags::ALLOWED_DATA_TYPES.contains(&input.data_type.as_str()) {
        errors.push(FieldError {
            field: "dataType".to_string(),
            message: format!(
                "対応データ型は {} のいずれかです",
                banto_tags::ALLOWED_DATA_TYPES.join(", ")
            ),
        });
    }

    if !(MIN_DECIMALS..=MAX_DECIMALS).contains(&input.decimals) {
        errors.push(FieldError {
            field: "decimals".to_string(),
            message: range_message(MIN_DECIMALS, MAX_DECIMALS),
        });
    }

    if let Err(BantoError::Validation { field_errors }) = banto_tags::Scaling::from_parts(
        input.raw_lo,
        input.raw_hi,
        input.eng_lo,
        input.eng_hi,
        "scaling",
    ) {
        errors.extend(field_errors);
    }

    let unit = input
        .unit
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    (
        errors,
        Normalized {
            name: trimmed_name.to_string(),
            address: trimmed_address.to_string(),
            unit,
        },
    )
}

fn column_map() -> ColumnMap {
    ColumnMap::new()
        .column("id", "id")
        .column("name", "name")
        .column("plcConnectionId", "plc_connection_id")
        .column("address", "address")
        .column("dataType", "data_type")
        .column("rawLo", "raw_lo")
        .column("rawHi", "raw_hi")
        .column("engLo", "eng_lo")
        .column("engHi", "eng_hi")
        .column("unit", "unit")
        .column("decimals", "decimals")
        .column("enabled", "enabled")
}

const RESOURCE: &str = "write_targets";
const COLUMNS: &str = "id, name, plc_connection_id, address, data_type, \
     raw_lo, raw_hi, eng_lo, eng_hi, unit, decimals, enabled";
const PLC_FK_MESSAGE: &str = "指定されたPLC接続が見つかりません";

/// Service layer for the `write_targets` resource. Tauri/axum-independent
/// (invariant §2): only `SqlitePool` + `BantoError`.
#[derive(Clone)]
pub struct WriteTargetService {
    pool: SqlitePool,
}

impl WriteTargetService {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Does the referenced banto-tags-owned `plc_connections` row exist?
    /// (Cross-lineage reference, validated here - see the module doc comment.)
    async fn plc_connection_exists(&self, id: i64) -> Result<bool, BantoError> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM plc_connections WHERE id = ?")
            .bind(id)
            .fetch_one(&self.pool)
            .await
            .map_err(banto_storage::storage_error)?;
        Ok(count > 0)
    }

    pub async fn list(&self, params: ListParams) -> Result<ListResult<WriteTarget>, BantoError> {
        let columns = column_map();

        let mut rows_builder: QueryBuilder<'_, Sqlite> =
            QueryBuilder::new(format!("SELECT {COLUMNS} FROM write_targets"));
        banto_storage::list_query::sqlite::apply_list_params(&mut rows_builder, &columns, &params)?;
        let rows: Vec<WriteTarget> = rows_builder
            .build_query_as::<WriteTarget>()
            .fetch_all(&self.pool)
            .await
            .map_err(banto_storage::storage_error)?;

        let mut count_builder: QueryBuilder<'_, Sqlite> =
            QueryBuilder::new("SELECT COUNT(*) FROM write_targets");
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

    pub async fn get(&self, id: i64) -> Result<WriteTarget, BantoError> {
        sqlx::query_as::<_, WriteTarget>(&format!(
            "SELECT {COLUMNS} FROM write_targets WHERE id = ?"
        ))
        .bind(id)
        .fetch_one(&self.pool)
        .await
        .map_err(|err| banto_storage::not_found(err, RESOURCE, id.to_string()))
    }

    pub async fn create(&self, input: WriteTargetInput) -> Result<WriteTarget, BantoError> {
        let (mut errors, normalized) = collect_errors(&input);
        if !self.plc_connection_exists(input.plc_connection_id).await? {
            errors.push(FieldError {
                field: "plcConnectionId".to_string(),
                message: PLC_FK_MESSAGE.to_string(),
            });
        }
        if !errors.is_empty() {
            return Err(BantoError::Validation {
                field_errors: errors,
            });
        }

        sqlx::query_as::<_, WriteTarget>(&format!(
            "INSERT INTO write_targets (\
                name, plc_connection_id, address, data_type, \
                raw_lo, raw_hi, eng_lo, eng_hi, unit, decimals, enabled\
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING {COLUMNS}"
        ))
        .bind(&normalized.name)
        .bind(input.plc_connection_id)
        .bind(&normalized.address)
        .bind(&input.data_type)
        .bind(input.raw_lo)
        .bind(input.raw_hi)
        .bind(input.eng_lo)
        .bind(input.eng_hi)
        .bind(&normalized.unit)
        .bind(input.decimals)
        .bind(input.enabled)
        .fetch_one(&self.pool)
        .await
        .map_err(|err| map_write_error(err, "name", "plcConnectionId", PLC_FK_MESSAGE))
    }

    pub async fn update(
        &self,
        id: i64,
        input: WriteTargetInput,
    ) -> Result<WriteTarget, BantoError> {
        let (mut errors, normalized) = collect_errors(&input);
        if !self.plc_connection_exists(input.plc_connection_id).await? {
            errors.push(FieldError {
                field: "plcConnectionId".to_string(),
                message: PLC_FK_MESSAGE.to_string(),
            });
        }
        if !errors.is_empty() {
            return Err(BantoError::Validation {
                field_errors: errors,
            });
        }

        sqlx::query_as::<_, WriteTarget>(&format!(
            "UPDATE write_targets SET \
                name = ?, plc_connection_id = ?, address = ?, data_type = ?, \
                raw_lo = ?, raw_hi = ?, eng_lo = ?, eng_hi = ?, unit = ?, decimals = ?, enabled = ? \
             WHERE id = ? RETURNING {COLUMNS}"
        ))
        .bind(&normalized.name)
        .bind(input.plc_connection_id)
        .bind(&normalized.address)
        .bind(&input.data_type)
        .bind(input.raw_lo)
        .bind(input.raw_hi)
        .bind(input.eng_lo)
        .bind(input.eng_hi)
        .bind(&normalized.unit)
        .bind(input.decimals)
        .bind(input.enabled)
        .bind(id)
        .fetch_one(&self.pool)
        .await
        .map_err(|err| match err {
            sqlx::Error::RowNotFound => BantoError::NotFound {
                resource: RESOURCE.to_string(),
                id: id.to_string(),
            },
            other => map_write_error(other, "name", "plcConnectionId", PLC_FK_MESSAGE),
        })
    }

    /// Delete, refusing when any [`crate::write_rules::WriteRule`] still
    /// targets this write target (`write_rules.write_target_id` is an
    /// in-lineage `ON DELETE RESTRICT` FK - see `0006_write_rules.sql`). The
    /// count is taken before the DELETE so the message can say exactly how
    /// many rules are in the way, the same friendly-guard pattern
    /// `banto_tags::plc_connection::PlcConnectionService::delete` uses.
    pub async fn delete(&self, id: i64) -> Result<(), BantoError> {
        let rule_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM write_rules WHERE write_target_id = ?")
                .bind(id)
                .fetch_one(&self.pool)
                .await
                .map_err(banto_storage::storage_error)?;
        if rule_count > 0 {
            return Err(BantoError::Validation {
                field_errors: vec![FieldError {
                    field: "id".to_string(),
                    message: format!(
                        "この書き込み先を使用している書き込みルールが{rule_count}件あるため削除できません"
                    ),
                }],
            });
        }

        let result = sqlx::query("DELETE FROM write_targets WHERE id = ?")
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
    use crate::db::init_db_memory;
    use banto_core::{FilterOp, FilterState, Pagination, SortDirection, SortState};
    use banto_tags::{PlcConnectionInput, PlcConnectionService};
    use serde_json::json;

    /// Fresh in-memory DB (this app's schema + banto-tags') plus one PLC
    /// connection to point write targets at.
    async fn setup() -> (WriteTargetService, i64) {
        let pool = init_db_memory().await.expect("init_db_memory");
        let plc = PlcConnectionService::new(pool.clone());
        let conn = plc
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
        (WriteTargetService::new(pool), conn.id)
    }

    fn sample(name: &str, plc_connection_id: i64) -> WriteTargetInput {
        WriteTargetInput {
            name: name.to_string(),
            plc_connection_id,
            address: "D100".to_string(),
            data_type: "i16".to_string(),
            raw_lo: None,
            raw_hi: None,
            eng_lo: None,
            eng_hi: None,
            unit: None,
            decimals: 0,
            enabled: true,
        }
    }

    #[tokio::test]
    async fn create_then_get_round_trips() {
        let (svc, plc) = setup().await;
        let created = svc.create(sample("WT1", plc)).await.expect("create");
        assert_eq!(created.name, "WT1");
        assert_eq!(created.plc_connection_id, plc);
        assert_eq!(created.address, "D100");
        assert_eq!(created.data_type, "i16");
        assert!(created.enabled);
        assert_eq!(created.scaling(), None);

        let fetched = svc.get(created.id).await.expect("get");
        assert_eq!(fetched, created);
    }

    #[tokio::test]
    async fn create_trims_name_address_and_unit() {
        let (svc, plc) = setup().await;
        let mut input = sample("  Padded  ", plc);
        input.address = "  D100  ".to_string();
        input.unit = Some("  degC  ".to_string());
        let created = svc.create(input).await.expect("create");
        assert_eq!(created.name, "Padded");
        assert_eq!(created.address, "D100");
        assert_eq!(created.unit.as_deref(), Some("degC"));
    }

    #[tokio::test]
    async fn create_normalizes_whitespace_unit_to_none() {
        let (svc, plc) = setup().await;
        let mut input = sample("X", plc);
        input.unit = Some("   ".to_string());
        assert_eq!(svc.create(input).await.unwrap().unit, None);
    }

    #[tokio::test]
    async fn get_missing_id_is_not_found() {
        let (svc, _plc) = setup().await;
        assert!(matches!(
            svc.get(999).await.unwrap_err(),
            BantoError::NotFound { .. }
        ));
    }

    #[tokio::test]
    async fn update_changes_fields() {
        let (svc, plc) = setup().await;
        let created = svc.create(sample("Before", plc)).await.unwrap();
        let mut input = sample("After", plc);
        input.data_type = "f32".to_string();
        input.decimals = 2;
        let updated = svc.update(created.id, input).await.expect("update");
        assert_eq!(updated.name, "After");
        assert_eq!(updated.data_type, "f32");
        assert_eq!(updated.decimals, 2);
    }

    #[tokio::test]
    async fn update_missing_id_is_not_found() {
        let (svc, plc) = setup().await;
        let err = svc.update(999, sample("X", plc)).await.unwrap_err();
        assert!(
            matches!(err, BantoError::NotFound { resource, id } if resource == "write_targets" && id == "999")
        );
    }

    #[tokio::test]
    async fn delete_then_get_is_not_found() {
        let (svc, plc) = setup().await;
        let created = svc.create(sample("Doomed", plc)).await.unwrap();
        svc.delete(created.id).await.expect("delete");
        assert!(matches!(
            svc.get(created.id).await.unwrap_err(),
            BantoError::NotFound { .. }
        ));
    }

    #[tokio::test]
    async fn delete_missing_id_is_not_found() {
        let (svc, _plc) = setup().await;
        assert!(matches!(
            svc.delete(999).await.unwrap_err(),
            BantoError::NotFound { .. }
        ));
    }

    // --- validation ---------------------------------------------------------

    #[tokio::test]
    async fn create_rejects_empty_name() {
        let (svc, plc) = setup().await;
        let mut input = sample("X", plc);
        input.name = "   ".to_string();
        match svc.create(input).await.unwrap_err() {
            BantoError::Validation { field_errors } => assert_eq!(field_errors[0].field, "name"),
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_rejects_name_over_max_len() {
        let (svc, plc) = setup().await;
        let mut input = sample("X", plc);
        input.name = "あ".repeat(MAX_NAME_LEN + 1);
        match svc.create(input).await.unwrap_err() {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "name");
                assert_eq!(field_errors[0].message, max_length_message(MAX_NAME_LEN));
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_rejects_empty_address() {
        let (svc, plc) = setup().await;
        let mut input = sample("X", plc);
        input.address = "  ".to_string();
        match svc.create(input).await.unwrap_err() {
            BantoError::Validation { field_errors } => assert_eq!(field_errors[0].field, "address"),
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_accepts_every_allowed_data_type() {
        let (svc, plc) = setup().await;
        for (i, dt) in banto_tags::ALLOWED_DATA_TYPES.iter().enumerate() {
            let mut input = sample(&format!("T{i}"), plc);
            input.data_type = dt.to_string();
            svc.create(input)
                .await
                .unwrap_or_else(|e| panic!("data_type {dt} should be accepted: {e:?}"));
        }
    }

    #[tokio::test]
    async fn create_rejects_unknown_data_type() {
        let (svc, plc) = setup().await;
        let mut input = sample("X", plc);
        input.data_type = "f64".to_string();
        match svc.create(input).await.unwrap_err() {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "dataType")
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_rejects_out_of_range_decimals() {
        let (svc, plc) = setup().await;
        let mut input = sample("X", plc);
        input.decimals = 7;
        match svc.create(input).await.unwrap_err() {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "decimals")
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_accepts_full_scaling() {
        let (svc, plc) = setup().await;
        let mut input = sample("X", plc);
        input.raw_lo = Some(0.0);
        input.raw_hi = Some(4095.0);
        input.eng_lo = Some(0.0);
        input.eng_hi = Some(100.0);
        let created = svc.create(input).await.expect("full scaling ok");
        assert_eq!(
            created.scaling(),
            Some(banto_tags::Scaling {
                raw_lo: 0.0,
                raw_hi: 4095.0,
                eng_lo: 0.0,
                eng_hi: 100.0,
            })
        );
    }

    #[tokio::test]
    async fn create_rejects_partial_scaling() {
        let (svc, plc) = setup().await;
        let mut input = sample("X", plc);
        input.raw_lo = Some(0.0);
        input.raw_hi = Some(4095.0);
        match svc.create(input).await.unwrap_err() {
            BantoError::Validation { field_errors } => assert_eq!(field_errors[0].field, "scaling"),
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_rejects_duplicate_name() {
        let (svc, plc) = setup().await;
        svc.create(sample("Dup", plc)).await.unwrap();
        match svc.create(sample("Dup", plc)).await.unwrap_err() {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "name");
                assert_eq!(field_errors[0].message, "既に使用されています");
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_rejects_missing_plc_connection_with_friendly_message() {
        let (svc, _plc) = setup().await;
        match svc.create(sample("X", 999)).await.unwrap_err() {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "plcConnectionId");
                assert_eq!(field_errors[0].message, PLC_FK_MESSAGE);
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn list_filters_sorts_and_paginates_with_total_count() {
        let (svc, plc) = setup().await;
        for (name, decimals) in [("A", 0), ("B", 1), ("C", 2)] {
            let mut input = sample(name, plc);
            input.decimals = decimals;
            svc.create(input).await.unwrap();
        }

        let result = svc
            .list(ListParams {
                sort: vec![SortState {
                    field: "decimals".to_string(),
                    direction: SortDirection::Desc,
                }],
                filters: vec![FilterState {
                    field: "decimals".to_string(),
                    op: FilterOp::Gte,
                    value: json!(1),
                }],
                pagination: Some(Pagination {
                    offset: 0,
                    limit: 1,
                }),
            })
            .await
            .expect("list");
        assert_eq!(result.total_count, 2);
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0].name, "C");
    }
}
