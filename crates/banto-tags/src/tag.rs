//! Tag: one collection point (recorder-requirements.md §2 "用語" - "収集点。
//! 名前 + PLC アドレス + データ型 + スケーリング + 単位 + 小数桁"). Every tag
//! belongs to exactly one [`crate::collection_group::CollectionGroup`],
//! which is what actually drives *when* it gets read from the PLC (§3.1).

use banto_core::{BantoError, FieldError, ListParams, ListResult};
use banto_storage::ColumnMap;
use serde::{Deserialize, Serialize};
use sqlx::{QueryBuilder, Sqlite, SqlitePool};

use crate::scaling::Scaling;
use crate::support::{map_write_error, max_length_message, range_message, required_message};

/// Data types accepted in `tags.data_type` (recorder-requirements.md §3.1:
/// "データ型（ビット/16bit/32bit 符号有無/実数）") - mirrors the SQL `CHECK`
/// in `migrations/0003_tags.sql`.
pub const ALLOWED_DATA_TYPES: &[&str] = &["bit", "i16", "u16", "i32", "u32", "f32"];

const MAX_NAME_LEN: usize = 100;
const MIN_DECIMALS: i64 = 0;
const MAX_DECIMALS: i64 = 6;

fn default_decimals() -> i64 {
    0
}

fn default_enabled() -> bool {
    true
}

/// A row of the `tags` table, wire-shaped (camelCase) for a future settings
/// grid (recorder-requirements.md §6 "タグ設定" screen).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct Tag {
    pub id: i64,
    pub name: String,
    pub collection_group_id: i64,
    pub address: String,
    pub data_type: String,
    pub raw_lo: Option<f64>,
    pub raw_hi: Option<f64>,
    pub eng_lo: Option<f64>,
    pub eng_hi: Option<f64>,
    pub unit: Option<String>,
    pub decimals: i64,
    pub threshold_h: Option<f64>,
    pub threshold_hh: Option<f64>,
    pub threshold_l: Option<f64>,
    pub threshold_ll: Option<f64>,
    pub enabled: bool,
}

impl Tag {
    /// Convenience accessor for the collection scaling, if any (spec: "全
    /// NULL=スケーリングなし" - a persisted `Tag` row is always in that
    /// all-or-nothing state since [`TagService::create`]/[`TagService::update`]
    /// only ever write rows that already passed [`Scaling::from_parts`]).
    /// Callers doing per-sample scaling (I3's collection engine) build a
    /// [`Scaling`] once per tag via this method rather than re-validating
    /// the four columns on every reading.
    pub fn scaling(&self) -> Option<Scaling> {
        match (self.raw_lo, self.raw_hi, self.eng_lo, self.eng_hi) {
            (Some(raw_lo), Some(raw_hi), Some(eng_lo), Some(eng_hi)) => Some(Scaling {
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
pub struct TagInput {
    pub name: String,
    pub collection_group_id: i64,
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
    #[serde(default)]
    pub threshold_h: Option<f64>,
    #[serde(default)]
    pub threshold_hh: Option<f64>,
    #[serde(default)]
    pub threshold_l: Option<f64>,
    #[serde(default)]
    pub threshold_ll: Option<f64>,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

/// Normalized, validated fields extracted from a [`TagInput`]: trimmed
/// strings and the whitespace-only-unit-becomes-`None` normalization, so
/// [`TagService::create`]/[`TagService::update`] bind exactly what
/// [`validate_tag_input`] already checked instead of re-deriving it.
struct ValidatedTag {
    name: String,
    address: String,
    unit: Option<String>,
}

/// Check `ll <= l <= h <= hh`, comparing only the thresholds that are
/// actually set (spec: "しきい値の順序... 設定されているものだけ比較").
/// Filtering to the set values while preserving position order and then
/// checking only *consecutive* pairs in that filtered list is equivalent to
/// checking every pair: the four positions form a fixed chain, so if the
/// filtered sequence is non-decreasing, every omitted comparison involving a
/// `None` is vacuously satisfied, and if some pair in the full chain would
/// have been violated, at least one consecutive pair in the filtered
/// sequence must be too (order violations cannot "hide" between two set
/// values with only unset values between them).
fn validate_thresholds(
    ll: Option<f64>,
    l: Option<f64>,
    h: Option<f64>,
    hh: Option<f64>,
) -> Result<(), BantoError> {
    let entries: [(&str, Option<f64>); 4] = [
        ("thresholdLl", ll),
        ("thresholdL", l),
        ("thresholdH", h),
        ("thresholdHh", hh),
    ];
    let set: Vec<(&str, f64)> = entries
        .into_iter()
        .filter_map(|(name, v)| v.map(|v| (name, v)))
        .collect();

    for pair in set.windows(2) {
        let (prev_field, prev_value) = pair[0];
        let (field, value) = pair[1];
        if prev_value > value {
            return Err(BantoError::Validation {
                field_errors: vec![FieldError {
                    field: field.to_string(),
                    message: format!("{prev_field} 以上の値にしてください"),
                }],
            });
        }
    }
    Ok(())
}

/// Validate a [`TagInput`], collecting every violation (mirrors
/// `items::validate_item_input`'s "report everything, not just the first"
/// convention in the banto template repo). On success, returns the trimmed
/// `name`/`address` and normalized `unit` for the caller to bind directly.
fn validate_tag_input(input: &TagInput) -> Result<ValidatedTag, BantoError> {
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

    // Address format is protocol-dependent and deliberately left to I2 -
    // only non-empty is enforced here (see this module's doc comment and
    // migrations/0003_tags.sql).
    let trimmed_address = input.address.trim();
    if trimmed_address.is_empty() {
        errors.push(FieldError {
            field: "address".to_string(),
            message: required_message(),
        });
    }

    if !ALLOWED_DATA_TYPES.contains(&input.data_type.as_str()) {
        errors.push(FieldError {
            field: "dataType".to_string(),
            message: format!(
                "対応データ型は {} のいずれかです",
                ALLOWED_DATA_TYPES.join(", ")
            ),
        });
    }

    if !(MIN_DECIMALS..=MAX_DECIMALS).contains(&input.decimals) {
        errors.push(FieldError {
            field: "decimals".to_string(),
            message: range_message(MIN_DECIMALS, MAX_DECIMALS),
        });
    }

    if let Err(BantoError::Validation { field_errors }) = Scaling::from_parts(
        input.raw_lo,
        input.raw_hi,
        input.eng_lo,
        input.eng_hi,
        "scaling",
    ) {
        errors.extend(field_errors);
    }

    if let Err(BantoError::Validation { field_errors }) = validate_thresholds(
        input.threshold_ll,
        input.threshold_l,
        input.threshold_h,
        input.threshold_hh,
    ) {
        errors.extend(field_errors);
    }

    if !errors.is_empty() {
        return Err(BantoError::Validation {
            field_errors: errors,
        });
    }

    // A whitespace-only unit is treated the same as an absent one - it is a
    // free-text display field (recorder-requirements.md §2), not something
    // worth a hard validation error over.
    let unit = input
        .unit
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    Ok(ValidatedTag {
        name: trimmed_name.to_string(),
        address: trimmed_address.to_string(),
        unit,
    })
}

fn column_map() -> ColumnMap {
    ColumnMap::new()
        .column("id", "id")
        .column("name", "name")
        .column("collectionGroupId", "collection_group_id")
        .column("address", "address")
        .column("dataType", "data_type")
        .column("rawLo", "raw_lo")
        .column("rawHi", "raw_hi")
        .column("engLo", "eng_lo")
        .column("engHi", "eng_hi")
        .column("unit", "unit")
        .column("decimals", "decimals")
        .column("thresholdH", "threshold_h")
        .column("thresholdHh", "threshold_hh")
        .column("thresholdL", "threshold_l")
        .column("thresholdLl", "threshold_ll")
        .column("enabled", "enabled")
}

const RESOURCE: &str = "tags";
const COLUMNS: &str = "id, name, collection_group_id, address, data_type, \
     raw_lo, raw_hi, eng_lo, eng_hi, unit, decimals, \
     threshold_h, threshold_hh, threshold_l, threshold_ll, enabled";
const FK_MESSAGE: &str = "指定された収集グループが見つかりません";

/// Service layer for the `tags` resource. No delete guard is needed here
/// (unlike [`crate::plc_connection::PlcConnectionService::delete`] /
/// [`crate::collection_group::CollectionGroupService::delete`]): nothing in
/// this crate references a `tags` row by id.
#[derive(Clone)]
pub struct TagService {
    pool: SqlitePool,
}

impl TagService {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn list(&self, params: ListParams) -> Result<ListResult<Tag>, BantoError> {
        let columns = column_map();

        let mut rows_builder: QueryBuilder<'_, Sqlite> =
            QueryBuilder::new(format!("SELECT {COLUMNS} FROM tags"));
        banto_storage::list_query::sqlite::apply_list_params(&mut rows_builder, &columns, &params)?;
        let rows: Vec<Tag> = rows_builder
            .build_query_as::<Tag>()
            .fetch_all(&self.pool)
            .await
            .map_err(banto_storage::storage_error)?;

        let mut count_builder: QueryBuilder<'_, Sqlite> =
            QueryBuilder::new("SELECT COUNT(*) FROM tags");
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

    pub async fn get(&self, id: i64) -> Result<Tag, BantoError> {
        sqlx::query_as::<_, Tag>(&format!("SELECT {COLUMNS} FROM tags WHERE id = ?"))
            .bind(id)
            .fetch_one(&self.pool)
            .await
            .map_err(|err| banto_storage::not_found(err, RESOURCE, id.to_string()))
    }

    pub async fn create(&self, input: TagInput) -> Result<Tag, BantoError> {
        let validated = validate_tag_input(&input)?;
        sqlx::query_as::<_, Tag>(&format!(
            "INSERT INTO tags (\
                name, collection_group_id, address, data_type, \
                raw_lo, raw_hi, eng_lo, eng_hi, unit, decimals, \
                threshold_h, threshold_hh, threshold_l, threshold_ll, enabled\
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING {COLUMNS}"
        ))
        .bind(&validated.name)
        .bind(input.collection_group_id)
        .bind(&validated.address)
        .bind(&input.data_type)
        .bind(input.raw_lo)
        .bind(input.raw_hi)
        .bind(input.eng_lo)
        .bind(input.eng_hi)
        .bind(&validated.unit)
        .bind(input.decimals)
        .bind(input.threshold_h)
        .bind(input.threshold_hh)
        .bind(input.threshold_l)
        .bind(input.threshold_ll)
        .bind(input.enabled)
        .fetch_one(&self.pool)
        .await
        .map_err(|err| map_write_error(err, "name", "collectionGroupId", FK_MESSAGE))
    }

    pub async fn update(&self, id: i64, input: TagInput) -> Result<Tag, BantoError> {
        let validated = validate_tag_input(&input)?;
        sqlx::query_as::<_, Tag>(&format!(
            "UPDATE tags SET \
                name = ?, collection_group_id = ?, address = ?, data_type = ?, \
                raw_lo = ?, raw_hi = ?, eng_lo = ?, eng_hi = ?, unit = ?, decimals = ?, \
                threshold_h = ?, threshold_hh = ?, threshold_l = ?, threshold_ll = ?, enabled = ? \
             WHERE id = ? RETURNING {COLUMNS}"
        ))
        .bind(&validated.name)
        .bind(input.collection_group_id)
        .bind(&validated.address)
        .bind(&input.data_type)
        .bind(input.raw_lo)
        .bind(input.raw_hi)
        .bind(input.eng_lo)
        .bind(input.eng_hi)
        .bind(&validated.unit)
        .bind(input.decimals)
        .bind(input.threshold_h)
        .bind(input.threshold_hh)
        .bind(input.threshold_l)
        .bind(input.threshold_ll)
        .bind(input.enabled)
        .bind(id)
        .fetch_one(&self.pool)
        .await
        .map_err(|err| match err {
            sqlx::Error::RowNotFound => BantoError::NotFound {
                resource: RESOURCE.to_string(),
                id: id.to_string(),
            },
            other => map_write_error(other, "name", "collectionGroupId", FK_MESSAGE),
        })
    }

    pub async fn delete(&self, id: i64) -> Result<(), BantoError> {
        let result = sqlx::query("DELETE FROM tags WHERE id = ?")
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
    use crate::collection_group::{CollectionGroupInput, CollectionGroupService};
    use crate::migrate;
    use crate::plc_connection::{PlcConnectionInput, PlcConnectionService};
    use banto_core::{FilterOp, FilterState, Pagination, SortDirection, SortState};
    use serde_json::json;

    async fn setup() -> (TagService, i64) {
        let pool = banto_storage::connect_sqlite_memory()
            .await
            .expect("connect_sqlite_memory");
        migrate(&pool).await.expect("migrate");

        let plc_svc = PlcConnectionService::new(pool.clone());
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

        let group_svc = CollectionGroupService::new(pool.clone());
        let group = group_svc
            .create(CollectionGroupInput {
                name: "Group1".to_string(),
                plc_connection_id: conn.id,
                period_ms: 1_000,
                enabled: true,
            })
            .await
            .unwrap();

        (TagService::new(pool), group.id)
    }

    fn sample_input(name: &str, collection_group_id: i64) -> TagInput {
        TagInput {
            name: name.to_string(),
            collection_group_id,
            address: "40001".to_string(),
            data_type: "i16".to_string(),
            raw_lo: None,
            raw_hi: None,
            eng_lo: None,
            eng_hi: None,
            unit: None,
            decimals: 0,
            threshold_h: None,
            threshold_hh: None,
            threshold_l: None,
            threshold_ll: None,
            enabled: true,
        }
    }

    // --- CRUD round trip -----------------------------------------------

    #[tokio::test]
    async fn create_then_get_round_trips() {
        let (svc, group_id) = setup().await;
        let created = svc
            .create(sample_input("Tag1", group_id))
            .await
            .expect("create should succeed");
        assert_eq!(created.name, "Tag1");
        assert_eq!(created.collection_group_id, group_id);
        assert_eq!(created.address, "40001");
        assert_eq!(created.data_type, "i16");
        assert_eq!(created.decimals, 0);
        assert!(created.enabled);
        assert_eq!(created.scaling(), None);

        let fetched = svc.get(created.id).await.expect("get should succeed");
        assert_eq!(fetched, created);
    }

    #[tokio::test]
    async fn create_trims_name_and_address() {
        let (svc, group_id) = setup().await;
        let mut input = sample_input("  Padded  ", group_id);
        input.address = "  40001  ".to_string();
        let created = svc.create(input).await.expect("create should succeed");
        assert_eq!(created.name, "Padded");
        assert_eq!(created.address, "40001");
    }

    #[tokio::test]
    async fn create_normalizes_whitespace_only_unit_to_none() {
        let (svc, group_id) = setup().await;
        let mut input = sample_input("X", group_id);
        input.unit = Some("   ".to_string());
        let created = svc.create(input).await.unwrap();
        assert_eq!(created.unit, None);
    }

    #[tokio::test]
    async fn create_trims_unit() {
        let (svc, group_id) = setup().await;
        let mut input = sample_input("X", group_id);
        input.unit = Some("  degC  ".to_string());
        let created = svc.create(input).await.unwrap();
        assert_eq!(created.unit.as_deref(), Some("degC"));
    }

    #[tokio::test]
    async fn get_missing_id_is_not_found() {
        let (svc, _group_id) = setup().await;
        let err = svc.get(999).await.unwrap_err();
        assert!(matches!(err, BantoError::NotFound { .. }));
    }

    #[tokio::test]
    async fn update_changes_fields() {
        let (svc, group_id) = setup().await;
        let created = svc.create(sample_input("Before", group_id)).await.unwrap();
        let mut input = sample_input("After", group_id);
        input.data_type = "f32".to_string();
        input.decimals = 2;
        let updated = svc.update(created.id, input).await.expect("update ok");
        assert_eq!(updated.name, "After");
        assert_eq!(updated.data_type, "f32");
        assert_eq!(updated.decimals, 2);
    }

    #[tokio::test]
    async fn update_missing_id_is_not_found() {
        let (svc, group_id) = setup().await;
        let err = svc
            .update(999, sample_input("X", group_id))
            .await
            .unwrap_err();
        assert!(
            matches!(err, BantoError::NotFound { resource, id } if resource == "tags" && id == "999")
        );
    }

    #[tokio::test]
    async fn delete_then_get_is_not_found() {
        let (svc, group_id) = setup().await;
        let created = svc.create(sample_input("Doomed", group_id)).await.unwrap();
        svc.delete(created.id).await.expect("delete should succeed");
        let err = svc.get(created.id).await.unwrap_err();
        assert!(matches!(err, BantoError::NotFound { .. }));
    }

    #[tokio::test]
    async fn delete_missing_id_is_not_found() {
        let (svc, _group_id) = setup().await;
        let err = svc.delete(999).await.unwrap_err();
        assert!(matches!(err, BantoError::NotFound { .. }));
    }

    // --- validation: name / address / data_type / decimals -------------

    #[tokio::test]
    async fn create_rejects_empty_name() {
        let (svc, group_id) = setup().await;
        let mut input = sample_input("X", group_id);
        input.name = "   ".to_string();
        let err = svc.create(input).await.unwrap_err();
        match err {
            BantoError::Validation { field_errors } => assert_eq!(field_errors[0].field, "name"),
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_rejects_name_over_max_len() {
        let (svc, group_id) = setup().await;
        let mut input = sample_input("X", group_id);
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
    async fn create_rejects_empty_address() {
        let (svc, group_id) = setup().await;
        let mut input = sample_input("X", group_id);
        input.address = "   ".to_string();
        let err = svc.create(input).await.unwrap_err();
        match err {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "address")
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_accepts_every_allowed_data_type() {
        let (svc, group_id) = setup().await;
        for (i, data_type) in ALLOWED_DATA_TYPES.iter().enumerate() {
            let mut input = sample_input(&format!("T{i}"), group_id);
            input.data_type = data_type.to_string();
            svc.create(input)
                .await
                .unwrap_or_else(|e| panic!("data_type {data_type} should be accepted: {e:?}"));
        }
    }

    #[tokio::test]
    async fn create_rejects_unknown_data_type() {
        let (svc, group_id) = setup().await;
        let mut input = sample_input("X", group_id);
        input.data_type = "f64".to_string();
        let err = svc.create(input).await.unwrap_err();
        match err {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "dataType")
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_accepts_decimals_boundaries() {
        let (svc, group_id) = setup().await;
        for (i, decimals) in [MIN_DECIMALS, MAX_DECIMALS].into_iter().enumerate() {
            let mut input = sample_input(&format!("D{i}"), group_id);
            input.decimals = decimals;
            svc.create(input).await.expect("boundary decimals ok");
        }
    }

    #[tokio::test]
    async fn create_rejects_out_of_range_decimals() {
        let (svc, group_id) = setup().await;
        let mut input = sample_input("X", group_id);
        input.decimals = 7;
        let err = svc.create(input).await.unwrap_err();
        match err {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "decimals")
            }
            other => panic!("expected Validation, got {other:?}"),
        }

        let mut input2 = sample_input("Y", group_id);
        input2.decimals = -1;
        let err2 = svc.create(input2).await.unwrap_err();
        assert!(matches!(err2, BantoError::Validation { .. }));
    }

    #[tokio::test]
    async fn create_rejects_duplicate_name() {
        let (svc, group_id) = setup().await;
        svc.create(sample_input("Dup", group_id)).await.unwrap();
        let err = svc.create(sample_input("Dup", group_id)).await.unwrap_err();
        match err {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "name");
                assert_eq!(field_errors[0].message, "既に使用されています");
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_rejects_missing_collection_group_with_friendly_message() {
        let (svc, _group_id) = setup().await;
        let err = svc.create(sample_input("X", 999)).await.unwrap_err();
        match err {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "collectionGroupId");
                assert_eq!(field_errors[0].message, FK_MESSAGE);
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    // --- validation: scaling --------------------------------------------

    #[tokio::test]
    async fn create_accepts_full_scaling() {
        let (svc, group_id) = setup().await;
        let mut input = sample_input("X", group_id);
        input.raw_lo = Some(0.0);
        input.raw_hi = Some(4095.0);
        input.eng_lo = Some(0.0);
        input.eng_hi = Some(100.0);
        let created = svc.create(input).await.expect("full scaling should be ok");
        assert_eq!(
            created.scaling(),
            Some(Scaling {
                raw_lo: 0.0,
                raw_hi: 4095.0,
                eng_lo: 0.0,
                eng_hi: 100.0,
            })
        );
    }

    #[tokio::test]
    async fn create_rejects_partial_scaling() {
        let (svc, group_id) = setup().await;
        let mut input = sample_input("X", group_id);
        input.raw_lo = Some(0.0);
        input.raw_hi = Some(4095.0);
        // eng_lo/eng_hi left None: partial.
        let err = svc.create(input).await.unwrap_err();
        match err {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "scaling")
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_rejects_degenerate_raw_range() {
        let (svc, group_id) = setup().await;
        let mut input = sample_input("X", group_id);
        input.raw_lo = Some(10.0);
        input.raw_hi = Some(10.0);
        input.eng_lo = Some(0.0);
        input.eng_hi = Some(100.0);
        let err = svc.create(input).await.unwrap_err();
        match err {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "scaling")
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    // --- validation: thresholds ------------------------------------------

    #[tokio::test]
    async fn create_accepts_fully_ordered_thresholds() {
        let (svc, group_id) = setup().await;
        let mut input = sample_input("X", group_id);
        input.threshold_ll = Some(0.0);
        input.threshold_l = Some(10.0);
        input.threshold_h = Some(90.0);
        input.threshold_hh = Some(100.0);
        svc.create(input).await.expect("ordered thresholds ok");
    }

    #[tokio::test]
    async fn create_accepts_partial_thresholds_in_order() {
        let (svc, group_id) = setup().await;
        // Only LL and H set; must still be compared (LL <= H).
        let mut input = sample_input("X", group_id);
        input.threshold_ll = Some(0.0);
        input.threshold_h = Some(90.0);
        svc.create(input)
            .await
            .expect("partial ordered thresholds ok");
    }

    #[tokio::test]
    async fn create_rejects_adjacent_threshold_violation() {
        let (svc, group_id) = setup().await;
        let mut input = sample_input("X", group_id);
        input.threshold_l = Some(50.0);
        input.threshold_h = Some(40.0); // H < L
        let err = svc.create(input).await.unwrap_err();
        match err {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "thresholdH")
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    /// LL and H are both set (and violate LL <= H) while L is left unset -
    /// proves the check does not just compare adjacent SQL columns but
    /// every consecutive pair *among the values that are actually set*.
    #[tokio::test]
    async fn create_rejects_non_adjacent_threshold_violation_across_a_gap() {
        let (svc, group_id) = setup().await;
        let mut input = sample_input("X", group_id);
        input.threshold_ll = Some(50.0);
        input.threshold_h = Some(10.0); // LL > H, with L unset in between
        let err = svc.create(input).await.unwrap_err();
        match err {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "thresholdH")
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_accepts_equal_adjacent_thresholds() {
        let (svc, group_id) = setup().await;
        let mut input = sample_input("X", group_id);
        input.threshold_l = Some(50.0);
        input.threshold_h = Some(50.0); // equal is allowed (<=)
        svc.create(input).await.expect("equal thresholds ok");
    }

    #[tokio::test]
    async fn create_accepts_a_single_threshold() {
        let (svc, group_id) = setup().await;
        let mut input = sample_input("X", group_id);
        input.threshold_h = Some(80.0);
        svc.create(input).await.expect("single threshold ok");
    }

    // --- list -------------------------------------------------------------

    #[tokio::test]
    async fn list_filters_sorts_and_paginates_with_total_count() {
        let (svc, group_id) = setup().await;
        for (name, decimals) in [("A", 0), ("B", 1), ("C", 2)] {
            let mut input = sample_input(name, group_id);
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
            .expect("list should succeed");

        assert_eq!(result.total_count, 2);
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0].name, "C");
    }
}
