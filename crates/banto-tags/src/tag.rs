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
/// "データ型（ビット/16bit/32bit 符号有無/実数）", plus S1's `"string"` for
/// MELSEC string devices - the relay-wright plan's 文字列タグ) - mirrors the
/// SQL `CHECK` in `migrations/0005_tags_allow_string.sql` (originally
/// `0003_tags.sql`).
///
/// `"string"` is registry vocabulary only as far as the recorder pipeline is
/// concerned: `banto-collect` skips string tags entirely (its tstore schema is
/// frozen numeric-only), and `banto_plc::DataType::parse("string")` returns
/// `None` on purpose. The consumer is relay-wright's engine (S2), which reads
/// string tags through `banto_plc`'s batch API using [`Tag::string_length`].
pub const ALLOWED_DATA_TYPES: &[&str] = &["bit", "i16", "u16", "i32", "u32", "f32", "string"];

/// The numeric/bit subset of [`ALLOWED_DATA_TYPES`] - i.e. the pre-S1 list.
/// For consumers whose own schema is numeric-only and must NOT widen with the
/// tag registry: relay-wright's `write_targets` validation (its SQL `CHECK`
/// has no `'string'`; string write targets are S2 work) and any similar
/// resource that borrowed the tag vocabulary. `allowed_is_numeric_plus_string`
/// below pins the relationship so the two lists cannot drift apart silently.
pub const NUMERIC_DATA_TYPES: &[&str] = &["bit", "i16", "u16", "i32", "u32", "f32"];

/// The one data type with a mandatory companion column (`string_length`) and
/// no scaling/threshold story. Kept as a named constant so the validation
/// below and any consumer reads as prose.
pub const STRING_DATA_TYPE: &str = "string";

const MAX_NAME_LEN: usize = 100;
const MIN_DECIMALS: i64 = 0;
const MAX_DECIMALS: i64 = 6;
/// `string_length` bounds, in 16-bit words (2 SJIS bytes per word, so 128
/// words = 256 bytes). Mirrors the SQL `CHECK` in
/// `migrations/0005_tags_allow_string.sql`. The upper bound also stays well
/// inside `banto-plc`'s single-bulk-read word cap (480), so a registry-legal
/// string always fits one wire read.
const MIN_STRING_LENGTH: i64 = 1;
const MAX_STRING_LENGTH: i64 = 128;

fn default_decimals() -> i64 {
    0
}

fn default_enabled() -> bool {
    true
}

fn default_tag_kind() -> String {
    PLC_TAG_KIND.to_string()
}

/// Full `tag_kind` vocabulary from design §4.2's table (`plc` / `computed` /
/// `internal`) - mirrors the SQL `CHECK` added in
/// `migrations/0006_tags_writable_kind.sql`. Not every value here is accepted
/// by [`validate_tag_input`] yet; see [`PLC_TAG_KIND`]'s doc comment for the
/// T2-vs-T6 staging (design §6 item 9, 2026-08-05 decision).
pub const ALLOWED_TAG_KINDS: &[&str] = &["plc", "computed", "internal"];

/// The one `tag_kind` this crate's service layer accepts as of T2 (design §6
/// item 9: "tag_kind は T2 時点で plc のみ受理し、computed/internal の受理は
/// T6 で解禁"). `"computed"`/`"internal"` are legal per the SQL `CHECK` (the
/// column already carries the full §4.2 vocabulary so T6 needs no further
/// migration) but [`validate_tag_input`] rejects them with a forward-looking
/// "T6 で対応予定" message, distinct from the generic "invalid value" message
/// a typo'd `tag_kind` gets - so a caller reaching for `computed`/`internal`
/// today is told this is a matter of timing, not a mistake.
pub const PLC_TAG_KIND: &str = "plc";

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
    /// Number of consecutive 16-bit word devices a `"string"` tag occupies
    /// (SJIS capacity = 2 bytes per word). `Some(1..=128)` exactly when
    /// `data_type == "string"`, `None` otherwise - enforced by
    /// [`validate_tag_input`], same all-or-nothing style as scaling.
    pub string_length: Option<i64>,
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
    /// Per-tag write opt-in (design §6 item 1: "per-tag opt-in"). Whether a
    /// `writable` tag can actually be *targeted* by a write (e.g. "is this
    /// tag's connection an SLMP connection under broker management") is not
    /// checked here - see this struct's module-level validation doc comment
    /// on [`validate_tag_input`] for why that question belongs to the write
    /// stack (T2-4), not the registry.
    pub writable: bool,
    /// One of [`ALLOWED_TAG_KINDS`] (design §4.2). T2's service layer only
    /// ever persists `"plc"` (see [`PLC_TAG_KIND`]) - the column carries the
    /// full T6 vocabulary already so no further migration is needed when
    /// `computed`/`internal` are unlocked.
    pub tag_kind: String,
    /// Computed-tag formula source (design §4.2, T6). Always `None` for a
    /// `tag_kind = "plc"` row - [`validate_tag_input`] enforces that a `plc`
    /// tag never carries an expression, since an address-driven tag's value
    /// is never derived from one.
    pub expression: Option<String>,
    /// Internal-tag "restore last value on restart" flag (design §4.2, T6).
    /// Accepted and persisted from T2 onward but not yet interpreted by any
    /// consumer - `tag_kind` staying `"plc"`-only until T6 means no row can
    /// reach the internal-tag code path that would read it.
    pub retain: bool,
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
    pub string_length: Option<i64>,
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
    /// `#[serde(default)]` (= `false`): an existing API client's payload
    /// (written before this field existed) still deserializes and creates a
    /// non-writable tag, exactly the pre-T2 behaviour (design §10-2:
    /// "既存の API クライアントのペイロードは無変更で通る").
    #[serde(default)]
    pub writable: bool,
    /// `#[serde(default = "default_tag_kind")]` (= `"plc"`) for the same
    /// reason as `writable` above - an existing payload with no `tagKind`
    /// field still creates the same `plc` tag it always did.
    #[serde(default = "default_tag_kind")]
    pub tag_kind: String,
    #[serde(default)]
    pub expression: Option<String>,
    #[serde(default)]
    pub retain: bool,
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
///
/// **`writable` gets no validation here beyond accepting the bool as-is.**
/// Design §6 item 1 makes `writable` a per-tag opt-in, and item 7 restricts
/// which *connections* can actually be targeted for a write ("writable に
/// できるのは SLMP 接続配下のタグのみ"). That second rule is deliberately NOT
/// enforced in this crate: `banto-tags` (I1) is the registry and knows a
/// tag's protocol only indirectly (via its `PlcConnection`'s `protocol`
/// column), but "is this protocol part of the write stack" is a question
/// about `banto-plc-write`/`banto-broker`'s capabilities, not about registry
/// integrity - a `writable` Modbus tag is a perfectly well-formed *row* (I1's
/// own invariants all hold), it is simply not usable for a write today. That
/// check belongs to the app layer that owns the write path (T2-4), which can
/// reject or warn on `writable` + non-SLMP without I1 having to know the
/// write stack's protocol coverage at all - keeping the two concerns (row
/// validity vs. write-path capability) from being entangled in one crate.
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

    // T2 accepts only `tag_kind = "plc"` (design §6 item 9, 2026-08-05
    // decision - see PLC_TAG_KIND's doc comment). A value from the wider §4.2
    // vocabulary gets a distinct "not yet" message from an outright-invalid
    // one, same "report everything, tell the caller exactly what's wrong"
    // spirit as every other field here.
    if input.tag_kind != PLC_TAG_KIND {
        if ALLOWED_TAG_KINDS.contains(&input.tag_kind.as_str()) {
            errors.push(FieldError {
                field: "tagKind".to_string(),
                message: "T6 で対応予定です（現時点は plc のみ受理します）".to_string(),
            });
        } else {
            errors.push(FieldError {
                field: "tagKind".to_string(),
                message: format!("tagKind は {} のいずれかです", ALLOWED_TAG_KINDS.join(", ")),
            });
        }
    }

    // design §4.2's table: a `plc` tag's value always comes from the
    // collection task, never a formula - `expression` only has meaning for
    // `computed` (T6). Checked regardless of whether `tag_kind` itself just
    // failed above, so a `plc` payload that also sets `expression` sees both
    // problems in one response.
    if input.tag_kind == PLC_TAG_KIND && input.expression.is_some() {
        errors.push(FieldError {
            field: "expression".to_string(),
            message: "plc タグには expression を設定できません".to_string(),
        });
    }

    if !(MIN_DECIMALS..=MAX_DECIMALS).contains(&input.decimals) {
        errors.push(FieldError {
            field: "decimals".to_string(),
            message: range_message(MIN_DECIMALS, MAX_DECIMALS),
        });
    }

    // S1 string tags: `string_length` is mandatory (1..=128 words) for
    // data_type "string" and forbidden otherwise, and a string tag has no
    // scaling/threshold story at all - a raw/eng mapping or an H/HH/L/LL
    // comparison over SJIS text is meaningless, so setting either is a field
    // error rather than silently ignored. The ordinary scaling/threshold
    // validation below is skipped for string tags so a violation surfaces as
    // the one intended message, not twice.
    let is_string = input.data_type == STRING_DATA_TYPE;
    if is_string {
        match input.string_length {
            None => errors.push(FieldError {
                field: "stringLength".to_string(),
                message: required_message(),
            }),
            Some(len) if !(MIN_STRING_LENGTH..=MAX_STRING_LENGTH).contains(&len) => {
                errors.push(FieldError {
                    field: "stringLength".to_string(),
                    message: range_message(MIN_STRING_LENGTH, MAX_STRING_LENGTH),
                })
            }
            Some(_) => {}
        }

        if input.raw_lo.is_some()
            || input.raw_hi.is_some()
            || input.eng_lo.is_some()
            || input.eng_hi.is_some()
        {
            errors.push(FieldError {
                field: "scaling".to_string(),
                message: "string 型ではスケーリングを設定できません".to_string(),
            });
        }

        for (field, value) in [
            ("thresholdH", input.threshold_h),
            ("thresholdHh", input.threshold_hh),
            ("thresholdL", input.threshold_l),
            ("thresholdLl", input.threshold_ll),
        ] {
            if value.is_some() {
                errors.push(FieldError {
                    field: field.to_string(),
                    message: "string 型ではしきい値を設定できません".to_string(),
                });
            }
        }
    } else if input.string_length.is_some() {
        errors.push(FieldError {
            field: "stringLength".to_string(),
            message: "string 型でのみ設定できます".to_string(),
        });
    }

    if !is_string {
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
        .column("stringLength", "string_length")
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
        .column("writable", "writable")
        .column("tagKind", "tag_kind")
        .column("expression", "expression")
        .column("retain", "retain")
}

const RESOURCE: &str = "tags";
const COLUMNS: &str = "id, name, collection_group_id, address, data_type, string_length, \
     raw_lo, raw_hi, eng_lo, eng_hi, unit, decimals, \
     threshold_h, threshold_hh, threshold_l, threshold_ll, enabled, \
     writable, tag_kind, expression, retain";
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
                name, collection_group_id, address, data_type, string_length, \
                raw_lo, raw_hi, eng_lo, eng_hi, unit, decimals, \
                threshold_h, threshold_hh, threshold_l, threshold_ll, enabled, \
                writable, tag_kind, expression, retain\
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             RETURNING {COLUMNS}"
        ))
        .bind(&validated.name)
        .bind(input.collection_group_id)
        .bind(&validated.address)
        .bind(&input.data_type)
        .bind(input.string_length)
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
        .bind(input.writable)
        .bind(&input.tag_kind)
        .bind(&input.expression)
        .bind(input.retain)
        .fetch_one(&self.pool)
        .await
        .map_err(|err| map_write_error(err, "name", "collectionGroupId", FK_MESSAGE))
    }

    pub async fn update(&self, id: i64, input: TagInput) -> Result<Tag, BantoError> {
        let validated = validate_tag_input(&input)?;
        sqlx::query_as::<_, Tag>(&format!(
            "UPDATE tags SET \
                name = ?, collection_group_id = ?, address = ?, data_type = ?, \
                string_length = ?, \
                raw_lo = ?, raw_hi = ?, eng_lo = ?, eng_hi = ?, unit = ?, decimals = ?, \
                threshold_h = ?, threshold_hh = ?, threshold_l = ?, threshold_ll = ?, enabled = ?, \
                writable = ?, tag_kind = ?, expression = ?, retain = ? \
             WHERE id = ? RETURNING {COLUMNS}"
        ))
        .bind(&validated.name)
        .bind(input.collection_group_id)
        .bind(&validated.address)
        .bind(&input.data_type)
        .bind(input.string_length)
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
        .bind(input.writable)
        .bind(&input.tag_kind)
        .bind(&input.expression)
        .bind(input.retain)
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
            string_length: None,
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
            writable: false,
            tag_kind: PLC_TAG_KIND.to_string(),
            expression: None,
            retain: false,
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
            // "string" is the one type with a mandatory companion column.
            input.string_length = (*data_type == STRING_DATA_TYPE).then_some(8);
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

    /// The relationship [`NUMERIC_DATA_TYPES`]'s doc comment promises:
    /// exactly [`ALLOWED_DATA_TYPES`] minus `"string"`, in the same order.
    #[test]
    fn allowed_is_numeric_plus_string() {
        let mut expected: Vec<&str> = NUMERIC_DATA_TYPES.to_vec();
        expected.push(STRING_DATA_TYPE);
        assert_eq!(ALLOWED_DATA_TYPES, expected.as_slice());
    }

    // --- validation: string tags (S1) -----------------------------------

    fn string_input(name: &str, group_id: i64, string_length: Option<i64>) -> TagInput {
        let mut input = sample_input(name, group_id);
        input.address = "D100".to_string();
        input.data_type = STRING_DATA_TYPE.to_string();
        input.string_length = string_length;
        input
    }

    #[tokio::test]
    async fn create_accepts_a_string_tag_and_round_trips_string_length() {
        let (svc, group_id) = setup().await;
        let created = svc
            .create(string_input("Recipe", group_id, Some(16)))
            .await
            .expect("string tag should be accepted");
        assert_eq!(created.data_type, "string");
        assert_eq!(created.string_length, Some(16));
        assert_eq!(created.scaling(), None);

        let fetched = svc.get(created.id).await.expect("get");
        assert_eq!(fetched, created);

        // Wire shape: camelCase like every other column.
        let json = serde_json::to_value(&fetched).expect("serialize");
        assert_eq!(json["stringLength"], json!(16));
        assert_eq!(json["dataType"], json!("string"));
    }

    #[tokio::test]
    async fn create_accepts_string_length_boundaries() {
        let (svc, group_id) = setup().await;
        for (i, len) in [MIN_STRING_LENGTH, MAX_STRING_LENGTH]
            .into_iter()
            .enumerate()
        {
            svc.create(string_input(&format!("S{i}"), group_id, Some(len)))
                .await
                .unwrap_or_else(|e| panic!("string_length {len} should be accepted: {e:?}"));
        }
    }

    #[tokio::test]
    async fn create_rejects_a_string_tag_without_string_length() {
        let (svc, group_id) = setup().await;
        let err = svc
            .create(string_input("S", group_id, None))
            .await
            .unwrap_err();
        match err {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "stringLength");
                assert_eq!(field_errors[0].message, required_message());
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_rejects_out_of_range_string_length() {
        let (svc, group_id) = setup().await;
        for len in [0, -1, MAX_STRING_LENGTH + 1] {
            let err = svc
                .create(string_input("S", group_id, Some(len)))
                .await
                .unwrap_err();
            match err {
                BantoError::Validation { field_errors } => {
                    assert_eq!(field_errors[0].field, "stringLength", "len={len}");
                    assert_eq!(
                        field_errors[0].message,
                        range_message(MIN_STRING_LENGTH, MAX_STRING_LENGTH)
                    );
                }
                other => panic!("expected Validation for len={len}, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn create_rejects_string_length_on_a_numeric_tag() {
        let (svc, group_id) = setup().await;
        let mut input = sample_input("X", group_id);
        input.string_length = Some(4);
        let err = svc.create(input).await.unwrap_err();
        match err {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "stringLength");
                assert_eq!(field_errors[0].message, "string 型でのみ設定できます");
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_rejects_scaling_on_a_string_tag() {
        let (svc, group_id) = setup().await;
        let mut input = string_input("S", group_id, Some(8));
        input.raw_lo = Some(0.0);
        input.raw_hi = Some(100.0);
        input.eng_lo = Some(0.0);
        input.eng_hi = Some(1.0);
        let err = svc.create(input).await.unwrap_err();
        match err {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "scaling");
                assert_eq!(
                    field_errors[0].message,
                    "string 型ではスケーリングを設定できません"
                );
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    /// Even a *partial* scaling (which for a numeric tag would produce the
    /// all-or-nothing message) is reported as the string-specific rejection -
    /// proves the ordinary scaling validation is skipped, not doubled up.
    #[tokio::test]
    async fn create_rejects_partial_scaling_on_a_string_tag_with_the_string_message() {
        let (svc, group_id) = setup().await;
        let mut input = string_input("S", group_id, Some(8));
        input.raw_lo = Some(0.0);
        let err = svc.create(input).await.unwrap_err();
        match err {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors.len(), 1);
                assert_eq!(field_errors[0].field, "scaling");
                assert_eq!(
                    field_errors[0].message,
                    "string 型ではスケーリングを設定できません"
                );
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_rejects_thresholds_on_a_string_tag_per_field() {
        let (svc, group_id) = setup().await;
        let mut input = string_input("S", group_id, Some(8));
        input.threshold_h = Some(10.0);
        input.threshold_ll = Some(0.0);
        let err = svc.create(input).await.unwrap_err();
        match err {
            BantoError::Validation { field_errors } => {
                let fields: Vec<&str> = field_errors.iter().map(|e| e.field.as_str()).collect();
                assert_eq!(fields, vec!["thresholdH", "thresholdLl"]);
                for e in &field_errors {
                    assert_eq!(e.message, "string 型ではしきい値を設定できません");
                }
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn update_can_change_a_numeric_tag_into_a_string_tag_and_back() {
        let (svc, group_id) = setup().await;
        let created = svc.create(sample_input("T", group_id)).await.unwrap();

        let updated = svc
            .update(created.id, string_input("T", group_id, Some(32)))
            .await
            .expect("update to string");
        assert_eq!(updated.data_type, "string");
        assert_eq!(updated.string_length, Some(32));

        let back = svc
            .update(created.id, sample_input("T", group_id))
            .await
            .expect("update back to numeric");
        assert_eq!(back.data_type, "i16");
        assert_eq!(back.string_length, None);
    }

    /// The 0005 SQL `CHECK` and [`ALLOWED_DATA_TYPES`] must agree in the
    /// rejection direction too: a type the Rust list rejects must also be
    /// rejected by the schema when the service layer is bypassed (mirrors
    /// `plc_connection.rs`'s CHECK symmetry tests).
    #[tokio::test]
    async fn the_sql_check_accepts_nothing_beyond_allowed_data_types() {
        let (svc, group_id) = setup().await;
        // Reach the pool through the service's own connection: create a row via
        // raw SQL to bypass validate_tag_input.
        let created = svc.create(sample_input("probe", group_id)).await.unwrap();
        let _ = created;
        for data_type in ["f64", "STRING", "str", ""] {
            let result = sqlx::query(
                "INSERT INTO tags (name, collection_group_id, address, data_type) \
                 VALUES (?, ?, '40001', ?)",
            )
            .bind(format!("raw-{data_type}"))
            .bind(group_id)
            .bind(data_type)
            .execute(&svc.pool)
            .await;
            assert!(
                result.is_err(),
                "the SQL CHECK accepted {data_type:?}, which is not in ALLOWED_DATA_TYPES"
            );
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

    // --- writable / tag_kind / expression / retain (T2-3, migration 0006) --

    /// The 4 new columns round-trip through create/get, including a
    /// `writable = true` tag - default() elsewhere in this test module
    /// (`sample_input`) always sets `writable: false`, so this is the one
    /// test that proves the opt-in flag itself persists.
    #[tokio::test]
    async fn create_then_get_round_trips_writable_and_kind_columns() {
        let (svc, group_id) = setup().await;
        let mut input = sample_input("X", group_id);
        input.writable = true;
        input.retain = true;
        let created = svc.create(input).await.expect("create should succeed");
        assert!(created.writable);
        assert_eq!(created.tag_kind, "plc");
        assert_eq!(created.expression, None);
        assert!(created.retain);

        let fetched = svc.get(created.id).await.expect("get should succeed");
        assert_eq!(fetched, created);

        // Wire shape: camelCase like every other column.
        let json = serde_json::to_value(&fetched).expect("serialize");
        assert_eq!(json["writable"], json!(true));
        assert_eq!(json["tagKind"], json!("plc"));
        assert_eq!(json["expression"], json!(null));
        assert_eq!(json["retain"], json!(true));
    }

    /// An existing API client's payload (no `writable`/`tagKind`/`expression`/
    /// `retain` fields at all) must still deserialize and create the exact
    /// pre-T2 tag - design §10-2's "既存の API クライアントのペイロードは
    /// 無変更で通る" backward-compatibility guarantee, exercised at the
    /// `TagInput` deserialization boundary rather than through the Rust
    /// struct literal every other test uses.
    #[tokio::test]
    async fn create_accepts_a_pre_t2_payload_missing_the_new_fields() {
        let (svc, group_id) = setup().await;
        // `TagInput` itself is snake_case on the wire (no `rename_all` - the
        // camelCase translation lives one layer up, in each app's own
        // `TagPayload`/`From<TagPayload> for TagInput`; see `rest.rs` in
        // banto-hub/relay-wright). This test targets `TagInput`'s own
        // deserialization boundary directly.
        let payload = json!({
            "name": "Legacy",
            "collection_group_id": group_id,
            "address": "40001",
            "data_type": "i16",
        });
        let input: TagInput = serde_json::from_value(payload).expect("legacy payload deserializes");
        let created = svc.create(input).await.expect("create should succeed");
        assert!(!created.writable);
        assert_eq!(created.tag_kind, "plc");
        assert_eq!(created.expression, None);
        assert!(!created.retain);
    }

    #[tokio::test]
    async fn create_accepts_tag_kind_plc() {
        let (svc, group_id) = setup().await;
        let mut input = sample_input("X", group_id);
        input.tag_kind = "plc".to_string();
        svc.create(input)
            .await
            .expect("tag_kind plc should be accepted in T2");
    }

    /// `computed`/`internal` are legal per [`ALLOWED_TAG_KINDS`] (and the SQL
    /// `CHECK`) but not yet accepted by the T2 service layer (design §6 item
    /// 9) - the rejection message must say "T6", not the generic
    /// invalid-value message [`create_rejects_an_unknown_tag_kind`] checks
    /// for.
    #[tokio::test]
    async fn create_rejects_computed_and_internal_with_a_t6_message() {
        let (svc, group_id) = setup().await;
        for kind in ["computed", "internal"] {
            let mut input = sample_input(&format!("K-{kind}"), group_id);
            input.tag_kind = kind.to_string();
            let err = svc.create(input).await.unwrap_err();
            match err {
                BantoError::Validation { field_errors } => {
                    assert_eq!(field_errors[0].field, "tagKind", "kind={kind}");
                    assert_eq!(
                        field_errors[0].message, "T6 で対応予定です（現時点は plc のみ受理します）",
                        "kind={kind}"
                    );
                }
                other => panic!("expected Validation for kind={kind}, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn create_rejects_an_unknown_tag_kind() {
        let (svc, group_id) = setup().await;
        let mut input = sample_input("X", group_id);
        input.tag_kind = "bogus".to_string();
        let err = svc.create(input).await.unwrap_err();
        match err {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "tagKind");
                assert_eq!(
                    field_errors[0].message,
                    format!("tagKind は {} のいずれかです", ALLOWED_TAG_KINDS.join(", "))
                );
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    /// design §4.2: a `plc` tag's value always comes from the collection
    /// task, never a formula - setting `expression` on a `plc` tag is
    /// rejected even though `tag_kind` itself is valid.
    #[tokio::test]
    async fn create_rejects_expression_on_a_plc_tag() {
        let (svc, group_id) = setup().await;
        let mut input = sample_input("X", group_id);
        input.expression = Some("a + b".to_string());
        let err = svc.create(input).await.unwrap_err();
        match err {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "expression");
                assert_eq!(
                    field_errors[0].message,
                    "plc タグには expression を設定できません"
                );
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    /// The SQL `CHECK` on `tag_kind` (migration 0006) must agree with
    /// [`ALLOWED_TAG_KINDS`] in the rejection direction, bypassing
    /// `validate_tag_input` entirely - same style as
    /// `the_sql_check_accepts_nothing_beyond_allowed_data_types` above.
    #[tokio::test]
    async fn the_sql_check_accepts_nothing_beyond_allowed_tag_kinds() {
        let (svc, group_id) = setup().await;
        for tag_kind in ["computed", "internal"] {
            let result = sqlx::query(
                "INSERT INTO tags (name, collection_group_id, address, data_type, tag_kind) \
                 VALUES (?, ?, '40001', 'i16', ?)",
            )
            .bind(format!("raw-{tag_kind}"))
            .bind(group_id)
            .bind(tag_kind)
            .execute(&svc.pool)
            .await;
            assert!(
                result.is_ok(),
                "the SQL CHECK should accept {tag_kind:?} (T6 vocabulary): {result:?}"
            );
        }
        for tag_kind in ["bogus", "PLC", ""] {
            let result = sqlx::query(
                "INSERT INTO tags (name, collection_group_id, address, data_type, tag_kind) \
                 VALUES (?, ?, '40001', 'i16', ?)",
            )
            .bind(format!("raw-{tag_kind}"))
            .bind(group_id)
            .bind(tag_kind)
            .execute(&svc.pool)
            .await;
            assert!(
                result.is_err(),
                "the SQL CHECK accepted {tag_kind:?}, which is not in ALLOWED_TAG_KINDS"
            );
        }
    }

    // --- migration 0005 (table rebuild) -----------------------------------

    /// Migration 0005 rebuilds `tags` (SQLite cannot `ALTER` a `CHECK`), and
    /// every other test in this crate only ever runs it against an *empty*
    /// database. This is the test that exercises it populated - the direct
    /// sibling of `plc_connection.rs`'s
    /// `migration_0004_preserves_rows_and_foreign_keys_on_a_populated_database`,
    /// and applied the same faithful way: the entire file as one
    /// multi-statement `execute`, on a single pinned connection, inside one
    /// transaction, exactly as sqlx-sqlite's `Migrate::apply` runs it. The SQL
    /// comes from `include_str!` so this cannot drift into passing against a
    /// stale copy.
    #[tokio::test]
    async fn migration_0005_preserves_rows_and_foreign_keys_on_a_populated_database() {
        use sqlx::{Acquire, Executor};

        let pool = banto_storage::connect_sqlite_memory()
            .await
            .expect("connect_sqlite_memory");
        let mut conn = pool.acquire().await.expect("acquire one pinned connection");

        // The schema as of 0004, i.e. what a deployed pre-string database
        // looks like.
        for (label, sql) in [
            (
                "0001",
                include_str!("../migrations/0001_plc_connections.sql"),
            ),
            (
                "0002",
                include_str!("../migrations/0002_collection_groups.sql"),
            ),
            ("0003", include_str!("../migrations/0003_tags.sql")),
            (
                "0004",
                include_str!("../migrations/0004_plc_connections_allow_slmp.sql"),
            ),
        ] {
            conn.execute(sql)
                .await
                .unwrap_or_else(|e| panic!("pre-0005 migration {label} failed: {e}"));
        }

        // Non-default values throughout, so a column dropped or transposed by
        // the rebuild shows up as a mismatch rather than coinciding with a
        // default.
        conn.execute(
            "INSERT INTO plc_connections (id, name, protocol, host, port, unit_id, enabled) \
             VALUES (7, 'Line1 PLC', 'slmp', '192.168.1.10', 5007, 3, 0)",
        )
        .await
        .expect("seed connection");
        conn.execute(
            "INSERT INTO collection_groups (id, name, plc_connection_id, period_ms, enabled) \
             VALUES (4, 'G1', 7, 1000, 1)",
        )
        .await
        .expect("seed collection group");
        conn.execute(
            "INSERT INTO tags (id, name, collection_group_id, address, data_type, \
             raw_lo, raw_hi, eng_lo, eng_hi, unit, decimals, threshold_h, enabled) \
             VALUES (9, 'T1', 4, 'D100', 'i16', 0, 100, 0, 50, 'degC', 2, 45, 1)",
        )
        .await
        .expect("seed tag");

        let migration = include_str!("../migrations/0005_tags_allow_string.sql");
        let mut tx = conn.begin().await.expect("begin, as the migrator does");
        tx.execute(migration).await.expect("0005 should apply");
        tx.commit().await.expect("0005 should commit");

        // Every column of the existing tag survived, values and all, with the
        // new column NULL.
        #[allow(clippy::type_complexity)]
        let tag: (
            i64,
            String,
            i64,
            String,
            String,
            Option<i64>,
            Option<f64>,
            Option<f64>,
            Option<String>,
            i64,
            Option<f64>,
            bool,
        ) = sqlx::query_as(
            "SELECT id, name, collection_group_id, address, data_type, string_length, \
             raw_lo, eng_hi, unit, decimals, threshold_h, enabled FROM tags",
        )
        .fetch_one(&mut *conn)
        .await
        .expect("the seeded tag should have been copied across");
        assert_eq!(
            tag,
            (
                9,
                "T1".to_string(),
                4,
                "D100".to_string(),
                "i16".to_string(),
                None,
                Some(0.0),
                Some(50.0),
                Some("degC".to_string()),
                2,
                Some(45.0),
                true
            )
        );

        let violations: Vec<(String,)> = sqlx::query_as("PRAGMA foreign_key_check")
            .fetch_all(&mut *conn)
            .await
            .expect("foreign_key_check");
        assert!(
            violations.is_empty(),
            "the rebuild left dangling foreign keys: {violations:?}"
        );

        // Foreign keys are still *enforced*, not merely currently consistent.
        assert!(
            sqlx::query(
                "INSERT INTO tags (name, collection_group_id, address, data_type) \
                 VALUES ('orphan', 999, 'D0', 'i16')",
            )
            .execute(&mut *conn)
            .await
            .is_err(),
            "foreign keys should still be enforced after the migration"
        );

        // The index survived under its original name.
        let index_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' \
             AND name = 'idx_tags_collection_group_id'",
        )
        .fetch_one(&mut *conn)
        .await
        .expect("index lookup");
        assert_eq!(index_count, 1);

        // The point of the whole exercise: 'string' (with a length) is now
        // insertable, and nothing else new is.
        sqlx::query(
            "INSERT INTO tags (name, collection_group_id, address, data_type, string_length) \
             VALUES ('Recipe', 4, 'D200', 'string', 16)",
        )
        .execute(&mut *conn)
        .await
        .expect("string should be accepted after the rebuild");
        assert!(sqlx::query(
            "INSERT INTO tags (name, collection_group_id, address, data_type) \
             VALUES ('Nope', 4, 'D300', 'f64')",
        )
        .execute(&mut *conn)
        .await
        .is_err());
        // The defensive range CHECK on string_length holds too.
        assert!(sqlx::query(
            "INSERT INTO tags (name, collection_group_id, address, data_type, string_length) \
             VALUES ('TooLong', 4, 'D400', 'string', 129)",
        )
        .execute(&mut *conn)
        .await
        .is_err());
    }

    // --- migration 0006 (plain ADD COLUMN) ---------------------------------

    /// Migration 0006's idempotency across app restarts is already covered by
    /// `migrate_is_idempotent` (crate root) - this test instead exercises the
    /// scenario that matters for a *deployed* database: applying 0006 against
    /// a database that already has real rows from 0001-0005 (i.e. an
    /// upgrade), the same "populated database" style as
    /// `migration_0005_preserves_rows_and_foreign_keys_on_a_populated_database`
    /// above. Unlike 0005, this migration is a plain `ADD COLUMN` (no table
    /// rebuild), so the existing row's id/values are trivially preserved by
    /// SQLite itself - what this test actually proves is that the new
    /// columns backfill to their documented defaults (`writable`/`retain` =
    /// false, `tag_kind` = `'plc'`, `expression` = NULL) on a row that predates
    /// them, and that the migration can be applied a second time consistent
    /// with `sqlx`'s own bookkeeping (via `migrate_is_idempotent`) without
    /// erroring on the already-added columns.
    #[tokio::test]
    async fn migration_0006_backfills_defaults_on_a_populated_database() {
        use sqlx::{Acquire, Executor};

        let pool = banto_storage::connect_sqlite_memory()
            .await
            .expect("connect_sqlite_memory");
        let mut conn = pool.acquire().await.expect("acquire one pinned connection");

        // The schema as of 0005, i.e. what a deployed pre-writable/kind
        // database looks like.
        for (label, sql) in [
            (
                "0001",
                include_str!("../migrations/0001_plc_connections.sql"),
            ),
            (
                "0002",
                include_str!("../migrations/0002_collection_groups.sql"),
            ),
            ("0003", include_str!("../migrations/0003_tags.sql")),
            (
                "0004",
                include_str!("../migrations/0004_plc_connections_allow_slmp.sql"),
            ),
            (
                "0005",
                include_str!("../migrations/0005_tags_allow_string.sql"),
            ),
        ] {
            conn.execute(sql)
                .await
                .unwrap_or_else(|e| panic!("pre-0006 migration {label} failed: {e}"));
        }

        conn.execute(
            "INSERT INTO plc_connections (id, name, protocol, host, port, unit_id, enabled) \
             VALUES (7, 'Line1 PLC', 'slmp', '192.168.1.10', 5007, 3, 0)",
        )
        .await
        .expect("seed connection");
        conn.execute(
            "INSERT INTO collection_groups (id, name, plc_connection_id, period_ms, enabled) \
             VALUES (4, 'G1', 7, 1000, 1)",
        )
        .await
        .expect("seed collection group");
        conn.execute(
            "INSERT INTO tags (id, name, collection_group_id, address, data_type, enabled) \
             VALUES (9, 'T1', 4, 'D100', 'i16', 1)",
        )
        .await
        .expect("seed tag");

        let migration = include_str!("../migrations/0006_tags_writable_kind.sql");
        let mut tx = conn.begin().await.expect("begin, as the migrator does");
        tx.execute(migration).await.expect("0006 should apply");
        tx.commit().await.expect("0006 should commit");

        #[allow(clippy::type_complexity)]
        let row: (i64, String, bool, String, Option<String>, bool) = sqlx::query_as(
            "SELECT id, name, writable, tag_kind, expression, retain FROM tags WHERE id = 9",
        )
        .fetch_one(&mut *conn)
        .await
        .expect("the pre-existing tag should have survived with backfilled defaults");
        assert_eq!(
            row,
            (9, "T1".to_string(), false, "plc".to_string(), None, false)
        );

        // The point of the exercise: the new columns are now writable and
        // enforce their own CHECK.
        sqlx::query(
            "INSERT INTO tags (name, collection_group_id, address, data_type, writable, \
             tag_kind, expression, retain) \
             VALUES ('T2', 4, 'D200', 'i16', 1, 'internal', NULL, 1)",
        )
        .execute(&mut *conn)
        .await
        .expect("a full new-shape row should be accepted after the migration");
        assert!(sqlx::query(
            "INSERT INTO tags (name, collection_group_id, address, data_type, tag_kind) \
             VALUES ('Bad', 4, 'D300', 'i16', 'bogus')",
        )
        .execute(&mut *conn)
        .await
        .is_err());
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
