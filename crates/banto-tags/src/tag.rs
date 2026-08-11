//! Tag: one collection point (recorder-requirements.md §2 "用語" - "収集点。
//! 名前 + PLC アドレス + データ型 + スケーリング + 単位 + 小数桁"). Every tag
//! belongs to exactly one [`crate::collection_group::CollectionGroup`],
//! which is what actually drives *when* it gets read from the PLC (§3.1).

use std::collections::{HashMap, HashSet};

use banto_core::{BantoError, FieldError, ListParams, ListResult};
use banto_storage::ColumnMap;
use serde::{Deserialize, Serialize};
use sqlx::{QueryBuilder, Sqlite, SqliteConnection, SqlitePool};

use crate::plc_connection::{CALC_CONNECTION_NAME, MEM_CONNECTION_NAME, VIRTUAL_PROTOCOL};
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
/// `migrations/0006_tags_writable_kind.sql`. All three are accepted by
/// [`validate_tag_input`] as of T6-2 (design §6 item 9's "computed/internal
/// の受理は T6 で解禁" is now in effect) - see [`PLC_TAG_KIND`]/
/// [`COMPUTED_TAG_KIND`]/[`INTERNAL_TAG_KIND`] for each species' own rules.
pub const ALLOWED_TAG_KINDS: &[&str] = &["plc", "computed", "internal"];

/// A collection-driven tag (design §4.2's table): value comes from the
/// collection task, `address` is required, `expression` must be absent.
/// Placement: any connection EXCEPT the reserved `calc`/`mem` virtual ones
/// (T6-2 - see [`validate_tag_kind_placement`]).
pub const PLC_TAG_KIND: &str = "plc";

/// A computed tag (design §4.2's table, T6-2): value comes from evaluating
/// `expression` (`banto_expr::compile`, wired at the hub layer -
/// `apps/banto-hub/core/src/computed.rs`), so `address` must be absent and
/// `expression` is required. Placement: the tag's group must live under the
/// reserved `"virtual"` connection named
/// [`crate::plc_connection::CALC_CONNECTION_NAME`] (`"calc"`) - enforced by
/// [`validate_tag_kind_placement`], not the SQL schema (see that function's
/// doc comment for why the check needs a connection join and therefore
/// cannot live in [`validate_tag_input`] alone).
pub const COMPUTED_TAG_KIND: &str = "computed";

/// An internal tag (design §4.2's table, T6-2): value comes from client
/// writes and is held entirely in the hub's tag space (never sent to a PLC).
/// `address` must be absent like `computed`, but `expression` must ALSO be
/// absent (an internal tag's value is not derived - it is written).
/// Placement: the tag's group must live under the reserved `"virtual"`
/// connection named [`crate::plc_connection::MEM_CONNECTION_NAME`] (`"mem"`)
/// - same enforcement point as [`COMPUTED_TAG_KIND`].
pub const INTERNAL_TAG_KIND: &str = "internal";

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
    /// T18-1 (docs/banto-hub-desktop-plan.md §9.4 TAG-UX-C 4点目「revision /
    /// ETag で後勝ち上書きを防ぐ」): 楽観的ロック用の行バージョン。
    /// `migrations/0009_tags_revision.sql` により新規行は `1` から始まり、
    /// [`TagService::update`]/[`TagService::update_tx`] が成功する度に必ず
    /// +1 される（`expected_revision` を指定しない呼び出しも例外ではない -
    /// 「チェックしない」と「増やさない」は別の話）。呼び出し側は編集画面を
    /// 開いた時点で取得したこの値を [`TagInput::expected_revision`] として
    /// 送り返すことで、他セッションが先に更新した行を黙って上書きしない
    /// ようにできる（差分表示 UI は本 PR のスコープ外 -
    /// [`TagUpdateError::RevisionConflict`] 参照）。
    pub revision: i64,
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
    /// T18-1 (docs/banto-hub-desktop-plan.md §9.4 TAG-UX-C 4点目): 更新時の
    /// 楽観ロック用。create では無視。`None` なら revision チェック無し
    /// （relay-wright 等の既存クライアント互換）だが revision は +1 する。
    #[serde(default)]
    pub expected_revision: Option<i64>,
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
    // migrations/0003_tags.sql). This applies to `plc` tags only (T6-2):
    // `computed`/`internal` tags have no PLC address at all (design §4.2's
    // table - "address なし") and must instead leave it blank, checked in the
    // `tag_kind`-specific block below.
    let trimmed_address = input.address.trim();
    if input.tag_kind == PLC_TAG_KIND && trimmed_address.is_empty() {
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

    // `tag_kind` itself (design §4.2's table). All three species are accepted
    // as of T6-2 (design §6 item 9's staging is now lifted) - an outright
    // unknown value still gets the generic "must be one of" message.
    if !ALLOWED_TAG_KINDS.contains(&input.tag_kind.as_str()) {
        errors.push(FieldError {
            field: "tagKind".to_string(),
            message: format!("tagKind は {} のいずれかです", ALLOWED_TAG_KINDS.join(", ")),
        });
    }

    // design §4.2's table, per species (checked regardless of whether
    // `tag_kind` itself just failed above, so a bad payload sees every
    // problem in one response):
    // - `plc`: `address` required (checked above), `expression` forbidden -
    //   its value always comes from the collection task, never a formula.
    // - `computed`: `address` forbidden (there is no PLC address - the
    //   external name's `calc` prefix is a virtual connection, not a real
    //   one), `expression` required - its value always comes from evaluating
    //   the formula (banto-expr, wired at the hub layer, T6-2).
    // - `internal`: `address` forbidden (same reasoning as `computed` - the
    //   `mem` prefix is virtual too), `expression` ALSO forbidden - unlike
    //   `computed`, an internal tag's value is written by a client, not
    //   derived from a formula.
    match input.tag_kind.as_str() {
        PLC_TAG_KIND => {
            if input.expression.is_some() {
                errors.push(FieldError {
                    field: "expression".to_string(),
                    message: "plc タグには expression を設定できません".to_string(),
                });
            }
        }
        COMPUTED_TAG_KIND => {
            if !trimmed_address.is_empty() {
                errors.push(FieldError {
                    field: "address".to_string(),
                    message: "computed タグには address を設定できません".to_string(),
                });
            }
            match input.expression.as_deref().map(str::trim) {
                None | Some("") => errors.push(FieldError {
                    field: "expression".to_string(),
                    message: required_message(),
                }),
                Some(_) => {}
            }
            if input.data_type == STRING_DATA_TYPE {
                errors.push(FieldError {
                    field: "dataType".to_string(),
                    message: "string 型は computed タグに設定できません".to_string(),
                });
            }
            // design §4.2's table: a computed tag's value is always decided
            // by its expression, never accepted from a client write - forcing
            // `writable == false` here (rather than special-casing tag_kind
            // in the write path) is what makes the write path's existing
            // gate 2 (`writable == false` -> 403) already the "computed タグ
            // への書き込みは常に403" rule from the T6-2 implementation
            // instructions, with no extra branch needed there.
            if input.writable {
                errors.push(FieldError {
                    field: "writable".to_string(),
                    message: "computed タグは writable にできません（値は式が決まります）"
                        .to_string(),
                });
            }
        }
        INTERNAL_TAG_KIND => {
            if !trimmed_address.is_empty() {
                errors.push(FieldError {
                    field: "address".to_string(),
                    message: "internal タグには address を設定できません".to_string(),
                });
            }
            if input.expression.is_some() {
                errors.push(FieldError {
                    field: "expression".to_string(),
                    message: "internal タグには expression を設定できません".to_string(),
                });
            }
            if input.data_type == STRING_DATA_TYPE {
                errors.push(FieldError {
                    field: "dataType".to_string(),
                    message: "string 型は internal タグに設定できません".to_string(),
                });
            }
        }
        // Unknown tag_kind: already reported above; no species-specific rule
        // applies.
        _ => {}
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

/// Cross-table placement check (T6-2, design §4.2's reserved `calc`/`mem`
/// namespace): `tag_kind` alone (checked in [`validate_tag_input`], which has
/// no database access) cannot tell whether a tag's group sits under the
/// reserved `"virtual"` connections - that requires joining
/// `collection_groups` to `plc_connections`, i.e. a query. This function is
/// therefore a separate, `async`, pool-taking step run by
/// [`TagService::create`]/[`TagService::update`] AFTER
/// [`validate_tag_input`] passes.
///
/// **Why here and not in `apps/banto-hub` (hub layer)**: the T6-2
/// implementation instructions leave the placement open between `TagService`
/// and the hub layer, recommending `TagService` "レジストリの整合性制約として
/// 全アプリ共通であるべき" - `banto-tags` is shared by ChronoGazer /
/// relay-wright / banto-hub alike, and "a `computed` tag must live under
/// `calc`, an `internal` tag under `mem`, a `plc` tag under neither" is a
/// registry-integrity invariant (same category as the existing "収集グループ
/// は必ず実在する PLC 接続を指す" `FOREIGN KEY` check), not a hub-specific
/// business rule - every consumer of this crate should get it enforced
/// uniformly rather than only banto-hub bothering to check.
///
/// A `collection_group_id` that does not resolve to any row is reported by
/// the `FOREIGN KEY` failure at INSERT/UPDATE time
/// ([`crate::support::map_write_error`]) instead - this function returns
/// `Ok(())` for that case rather than manufacturing a second, duplicate
/// error for the same underlying problem.
async fn validate_tag_kind_placement(
    pool: &SqlitePool,
    collection_group_id: i64,
    tag_kind: &str,
) -> Result<(), BantoError> {
    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT pc.name, pc.protocol FROM collection_groups cg \
         JOIN plc_connections pc ON pc.id = cg.plc_connection_id \
         WHERE cg.id = ?",
    )
    .bind(collection_group_id)
    .fetch_optional(pool)
    .await
    .map_err(banto_storage::storage_error)?;

    let Some((conn_name, protocol)) = row else {
        return Ok(());
    };
    let is_virtual = protocol == VIRTUAL_PROTOCOL;

    let placement_error = |message: String| -> Result<(), BantoError> {
        Err(BantoError::Validation {
            field_errors: vec![FieldError {
                field: "tagKind".to_string(),
                message,
            }],
        })
    };

    match tag_kind {
        PLC_TAG_KIND if is_virtual => {
            placement_error("plc タグは予約接続（calc/mem）配下に作成できません".to_string())
        }
        COMPUTED_TAG_KIND if !is_virtual || conn_name != CALC_CONNECTION_NAME => placement_error(
            format!("computed タグは予約接続 {CALC_CONNECTION_NAME} 配下にのみ作成できます"),
        ),
        INTERNAL_TAG_KIND if !is_virtual || conn_name != MEM_CONNECTION_NAME => placement_error(
            format!("internal タグは予約接続 {MEM_CONNECTION_NAME} 配下にのみ作成できます"),
        ),
        // Unknown tag_kind is already rejected by validate_tag_input; no
        // placement rule to apply.
        _ => Ok(()),
    }
}

async fn validate_tag_kind_placement_tx(
    connection: &mut SqliteConnection,
    collection_group_id: i64,
    tag_kind: &str,
) -> Result<(), BantoError> {
    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT pc.name, pc.protocol FROM collection_groups cg \
         JOIN plc_connections pc ON pc.id = cg.plc_connection_id \
         WHERE cg.id = ?",
    )
    .bind(collection_group_id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(banto_storage::storage_error)?;
    let Some((conn_name, protocol)) = row else {
        return Ok(());
    };
    let is_virtual = protocol == VIRTUAL_PROTOCOL;
    let placement_error = |message: String| -> Result<(), BantoError> {
        Err(BantoError::Validation {
            field_errors: vec![FieldError {
                field: "tagKind".to_string(),
                message,
            }],
        })
    };
    match tag_kind {
        PLC_TAG_KIND if is_virtual => {
            placement_error("plc タグは予約接続（calc/mem）配下に作成できません".to_string())
        }
        COMPUTED_TAG_KIND if !is_virtual || conn_name != CALC_CONNECTION_NAME => placement_error(
            format!("computed タグは予約接続 {CALC_CONNECTION_NAME} 配下にのみ作成できます"),
        ),
        INTERNAL_TAG_KIND if !is_virtual || conn_name != MEM_CONNECTION_NAME => placement_error(
            format!("internal タグは予約接続 {MEM_CONNECTION_NAME} 配下にのみ作成できます"),
        ),
        _ => Ok(()),
    }
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
        .column("revision", "revision")
}

const RESOURCE: &str = "tags";
const COLUMNS: &str = "id, name, collection_group_id, address, data_type, string_length, \
     raw_lo, raw_hi, eng_lo, eng_hi, unit, decimals, \
     threshold_h, threshold_hh, threshold_l, threshold_ll, enabled, \
     writable, tag_kind, expression, retain, revision";
const FK_MESSAGE: &str = "指定された収集グループが見つかりません";

/// Shared by [`TagService::create`] and [`TagService::create_batch`] (T11-1)
/// so the two INSERT statements cannot drift apart - both bind the exact
/// same 20 columns in the exact same order (see either call site).
fn insert_tag_sql() -> String {
    format!(
        "INSERT INTO tags (\
            name, collection_group_id, address, data_type, string_length, \
            raw_lo, raw_hi, eng_lo, eng_hi, unit, decimals, \
            threshold_h, threshold_hh, threshold_l, threshold_ll, enabled, \
            writable, tag_kind, expression, retain\
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
         RETURNING {COLUMNS}"
    )
}

/// Shared by [`TagService::update`] and [`TagService::update_tx`] (T18-1) so
/// the two UPDATE statements cannot drift apart - both bind the exact same
/// 20 columns in the exact same order (mirrors [`insert_tag_sql`]'s doc
/// comment), and both always advance `revision` regardless of whether the
/// caller opted into the optimistic-lock check.
///
/// `with_expected_revision` selects the `WHERE` clause: `true` adds
/// `AND revision = ?` (the caller must bind the expected revision as the
/// LAST parameter, after `id`) for [`TagInput::expected_revision`]'s `Some`
/// case; `false` keeps the pre-T18-1 unconditional `WHERE id = ?` for the
/// `None` case (relay-wright and any other client that never adopts the
/// lock).
fn update_tag_sql(with_expected_revision: bool) -> String {
    let where_clause = if with_expected_revision {
        "WHERE id = ? AND revision = ?"
    } else {
        "WHERE id = ?"
    };
    format!(
        "UPDATE tags SET \
            name = ?, collection_group_id = ?, address = ?, data_type = ?, \
            string_length = ?, raw_lo = ?, raw_hi = ?, eng_lo = ?, eng_hi = ?, unit = ?, decimals = ?, \
            threshold_h = ?, threshold_hh = ?, threshold_l = ?, threshold_ll = ?, enabled = ?, \
            writable = ?, tag_kind = ?, expression = ?, retain = ?, revision = revision + 1 \
         {where_clause} RETURNING {COLUMNS}"
    )
}

/// Fetch a tag row by id on a caller-owned connection (as opposed to
/// [`TagService::get`], which always goes through the pool) - used by
/// [`TagService::update_tx`] to tell apart "no such tag" from "the tag
/// exists but `expected_revision` was stale" after a
/// [`update_tag_sql`]-with-lock `UPDATE` returns zero rows.
async fn fetch_tag_row(
    connection: &mut SqliteConnection,
    id: i64,
) -> Result<Option<Tag>, sqlx::Error> {
    sqlx::query_as::<_, Tag>(&format!("SELECT {COLUMNS} FROM tags WHERE id = ?"))
        .bind(id)
        .fetch_optional(&mut *connection)
        .await
}

/// A single row's worth of field errors within a [`TagService::create_batch`]
/// call (T11-1, docs/ux-plan.md §3: "行番号/インデックス付きのエラー一覧").
/// `index` is the row's 0-based position in the request's `tags` array, so a
/// client (the continuous-registration preview, and later T11-2's CSV
/// import) can point back at exactly the offending row/line.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchTagError {
    pub index: usize,
    pub field_errors: Vec<FieldError>,
}

/// Result of [`TagService::create_batch`] - see that method's doc comment for
/// the all-or-nothing contract each variant implies.
#[derive(Debug)]
pub enum BatchTagOutcome {
    /// At least one row failed validation (including duplicate-name
    /// detection). Nothing was written - not even the rows that were
    /// individually fine (design: "1件でもエラーがあれば全体を拒否").
    Invalid(Vec<BatchTagError>),
    /// Every row validated. `tags` carries the persisted rows in request
    /// order for a real apply (`dry_run: false`), or is `None` for
    /// `dry_run: true` (validation-only - nothing was written).
    Valid {
        count: usize,
        tags: Option<Vec<Tag>>,
    },
}

/// [`TagService::update_tx`]'s error type (T18-1, docs/banto-hub-desktop-plan.md
/// §9.4 TAG-UX-C 4点目). A superset of `BantoError` that additionally
/// distinguishes the one new failure mode the optimistic-lock `WHERE id = ?
/// AND revision = ?` clause can produce: zero rows updated *because another
/// session already advanced the revision*, as opposed to zero rows updated
/// because the id does not exist at all. The hub REST layer needs this
/// distinction to answer with `409 Conflict` + the current row (so the admin
/// UI can refresh instead of silently overwriting) rather than a generic
/// `404`/`422`.
///
/// [`TagService::update`] (the pool-taking, pre-T18-1-shaped method that
/// relay-wright still calls) intentionally keeps its `Result<Tag, BantoError>`
/// signature instead of switching to this type - see that method's doc
/// comment for how it folds a conflict back into `BantoError`.
#[derive(Debug)]
pub enum TagUpdateError {
    /// Every other failure ([`validate_tag_input`]'s field errors,
    /// [`validate_tag_kind_placement_tx`]'s placement rule, the id simply
    /// not existing, a UNIQUE/FOREIGN KEY violation, ...) - unchanged from
    /// what [`TagService::update_tx`] returned before T18-1.
    Banto(BantoError),
    /// `WHERE id = ? AND revision = ?` matched zero rows, but the row
    /// itself still exists (a plain `SELECT ... WHERE id = ?` on the same
    /// connection found it) - i.e. another session updated it first. Carries
    /// that current row so the caller can hand it back to the client
    /// (differencing which fields actually changed is explicitly out of
    /// scope for this PR - see docs/banto-hub-desktop-plan.md §9.4's
    /// implementation note). Boxed - `Tag` is large enough that
    /// `clippy::large_enum_variant` flags an unboxed `Tag` here (it would
    /// roughly triple this enum's size over the `Banto` variant).
    RevisionConflict(Box<Tag>),
}

impl From<BantoError> for TagUpdateError {
    fn from(err: BantoError) -> Self {
        Self::Banto(err)
    }
}

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
        validate_tag_kind_placement(&self.pool, input.collection_group_id, &input.tag_kind).await?;
        sqlx::query_as::<_, Tag>(&insert_tag_sql())
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

    /// Transaction-compatible counterpart of [`Self::create`].
    pub async fn create_tx(
        &self,
        connection: &mut SqliteConnection,
        input: TagInput,
    ) -> Result<Tag, BantoError> {
        let validated = validate_tag_input(&input)?;
        validate_tag_kind_placement_tx(connection, input.collection_group_id, &input.tag_kind)
            .await?;
        sqlx::query_as::<_, Tag>(&insert_tag_sql())
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
            .fetch_one(&mut *connection)
            .await
            .map_err(|err| map_write_error(err, "name", "collectionGroupId", FK_MESSAGE))
    }

    /// **T18-1 optimistic lock** (docs/banto-hub-desktop-plan.md §9.4
    /// TAG-UX-C 4点目): when `input.expected_revision` is `Some(r)`, the
    /// `UPDATE` only touches the row if it is still at revision `r`
    /// (`WHERE id = ? AND revision = ?`) - a stale caller (one that read the
    /// row before another session updated it) gets zero rows back instead of
    /// silently clobbering the newer write. `None` keeps the pre-T18-1
    /// unconditional `WHERE id = ?` for compatibility with clients that
    /// never adopted the lock (relay-wright today) - either way `revision`
    /// itself always advances by 1 on a successful update.
    ///
    /// This method's signature stays `Result<Tag, BantoError>` (unlike
    /// [`Self::update_tx`]) so relay-wright's existing call site keeps
    /// compiling and behaving as before: a revision conflict here is folded
    /// into a plain [`BantoError::Other`] with a Japanese "reload and retry"
    /// message rather than the richer [`TagUpdateError::RevisionConflict`]
    /// (which needs a caller that can actually show the current row - the
    /// hub REST layer, via [`Self::update_tx`]).
    pub async fn update(&self, id: i64, input: TagInput) -> Result<Tag, BantoError> {
        let validated = validate_tag_input(&input)?;
        validate_tag_kind_placement(&self.pool, input.collection_group_id, &input.tag_kind).await?;
        let expected_revision = input.expected_revision;
        let sql = update_tag_sql(expected_revision.is_some());
        let mut query = sqlx::query_as::<_, Tag>(&sql)
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
            .bind(id);
        if let Some(revision) = expected_revision {
            query = query.bind(revision);
        }
        match query.fetch_one(&self.pool).await {
            Ok(tag) => Ok(tag),
            Err(sqlx::Error::RowNotFound) => {
                if expected_revision.is_some() {
                    // Distinguish "no such tag" from "stale revision" the
                    // same way update_tx does, just without a connection to
                    // reuse - see that method's doc comment.
                    let current = sqlx::query_as::<_, Tag>(&format!(
                        "SELECT {COLUMNS} FROM tags WHERE id = ?"
                    ))
                    .bind(id)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(banto_storage::storage_error)?;
                    match current {
                        Some(_) => Err(BantoError::Other(
                            "他のクライアントがこのタグを更新済みです。再読込してから保存してください。"
                                .to_string(),
                        )),
                        None => Err(BantoError::NotFound {
                            resource: RESOURCE.to_string(),
                            id: id.to_string(),
                        }),
                    }
                } else {
                    Err(BantoError::NotFound {
                        resource: RESOURCE.to_string(),
                        id: id.to_string(),
                    })
                }
            }
            Err(other) => Err(map_write_error(
                other,
                "name",
                "collectionGroupId",
                FK_MESSAGE,
            )),
        }
    }

    /// Transaction-compatible counterpart of [`Self::update`] - and the
    /// richer-error twin the hub REST layer (`tags_update`) actually calls.
    /// Same optimistic-lock semantics as `update` (see that method's doc
    /// comment for the WHERE-clause split on `expected_revision`), but on a
    /// stale revision this returns [`TagUpdateError::RevisionConflict`]
    /// carrying the tag's current row (re-fetched on the SAME `connection`,
    /// so it reflects whatever the caller's transaction can see) instead of
    /// folding the conflict into a generic error - the hub's REST handler
    /// rolls back and answers `409` with that row so the admin UI can offer
    /// "reload and retry" instead of silently overwriting.
    pub async fn update_tx(
        &self,
        connection: &mut SqliteConnection,
        id: i64,
        input: TagInput,
    ) -> Result<Tag, TagUpdateError> {
        let validated = validate_tag_input(&input)?;
        validate_tag_kind_placement_tx(connection, input.collection_group_id, &input.tag_kind)
            .await?;
        let expected_revision = input.expected_revision;
        let sql = update_tag_sql(expected_revision.is_some());
        let mut query = sqlx::query_as::<_, Tag>(&sql)
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
            .bind(id);
        if let Some(revision) = expected_revision {
            query = query.bind(revision);
        }
        match query.fetch_one(&mut *connection).await {
            Ok(tag) => Ok(tag),
            Err(sqlx::Error::RowNotFound) => {
                if expected_revision.is_some() {
                    let current = fetch_tag_row(connection, id)
                        .await
                        .map_err(banto_storage::storage_error)?;
                    match current {
                        Some(tag) => Err(TagUpdateError::RevisionConflict(Box::new(tag))),
                        None => Err(TagUpdateError::Banto(BantoError::NotFound {
                            resource: RESOURCE.to_string(),
                            id: id.to_string(),
                        })),
                    }
                } else {
                    Err(TagUpdateError::Banto(BantoError::NotFound {
                        resource: RESOURCE.to_string(),
                        id: id.to_string(),
                    }))
                }
            }
            Err(other) => Err(TagUpdateError::Banto(map_write_error(
                other,
                "name",
                "collectionGroupId",
                FK_MESSAGE,
            ))),
        }
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

    /// Transaction-compatible counterpart of [`Self::delete`].
    pub async fn delete_tx(
        &self,
        connection: &mut SqliteConnection,
        id: i64,
    ) -> Result<(), BantoError> {
        let result = sqlx::query("DELETE FROM tags WHERE id = ?")
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

    /// Validate and insert a batch using a caller-owned SQLite transaction.
    /// Unlike [`Self::create_batch`], this method never starts or commits a
    /// transaction itself: the caller can inspect the resulting registry on
    /// the same connection and then commit or roll back the whole proposal.
    pub async fn create_batch_tx(
        &self,
        connection: &mut SqliteConnection,
        inputs: &[TagInput],
    ) -> Result<BatchTagOutcome, BantoError> {
        let mut row_errors: Vec<Vec<FieldError>> = vec![Vec::new(); inputs.len()];
        let mut validated: Vec<Option<ValidatedTag>> = Vec::with_capacity(inputs.len());

        for (index, input) in inputs.iter().enumerate() {
            match validate_tag_input(input) {
                Ok(value) => validated.push(Some(value)),
                Err(BantoError::Validation { field_errors }) => {
                    row_errors[index].extend(field_errors);
                    validated.push(None);
                }
                Err(other) => return Err(other),
            }
            match validate_tag_kind_placement_tx(
                connection,
                input.collection_group_id,
                &input.tag_kind,
            )
            .await
            {
                Ok(()) => {}
                Err(BantoError::Validation { field_errors }) => {
                    row_errors[index].extend(field_errors)
                }
                Err(other) => return Err(other),
            }
        }

        let mut first_seen: HashMap<&str, usize> = HashMap::new();
        let mut batch_dupe_indices = Vec::new();
        for (index, value) in validated.iter().enumerate() {
            let Some(value) = value else { continue };
            match first_seen.get(value.name.as_str()) {
                Some(&first) => {
                    if !batch_dupe_indices.contains(&first) {
                        batch_dupe_indices.push(first);
                    }
                    batch_dupe_indices.push(index);
                }
                None => {
                    first_seen.insert(&value.name, index);
                }
            }
        }
        for index in batch_dupe_indices {
            row_errors[index].push(FieldError {
                field: "name".to_string(),
                message: "リクエスト内の他の行と名前が重複しています".to_string(),
            });
        }

        let candidate_names: Vec<&str> = validated
            .iter()
            .filter_map(|value| value.as_ref().map(|value| value.name.as_str()))
            .collect();
        if !candidate_names.is_empty() {
            let mut qb: QueryBuilder<'_, Sqlite> =
                QueryBuilder::new("SELECT name FROM tags WHERE name IN (");
            let mut separated = qb.separated(", ");
            for name in &candidate_names {
                separated.push_bind(*name);
            }
            qb.push(")");
            let existing: Vec<(String,)> = qb
                .build_query_as()
                .fetch_all(&mut *connection)
                .await
                .map_err(banto_storage::storage_error)?;
            let existing_names: HashSet<String> =
                existing.into_iter().map(|(name,)| name).collect();
            for (index, value) in validated.iter().enumerate() {
                let Some(value) = value else { continue };
                if existing_names.contains(&value.name) {
                    row_errors[index].push(FieldError {
                        field: "name".to_string(),
                        message: "既に使用されています".to_string(),
                    });
                }
            }
        }

        let errors: Vec<BatchTagError> = row_errors
            .into_iter()
            .enumerate()
            .filter(|(_, field_errors)| !field_errors.is_empty())
            .map(|(index, field_errors)| BatchTagError {
                index,
                field_errors,
            })
            .collect();
        if !errors.is_empty() {
            return Ok(BatchTagOutcome::Invalid(errors));
        }

        let mut created = Vec::with_capacity(inputs.len());
        let sql = insert_tag_sql();
        for (input, value) in inputs.iter().zip(validated.iter()) {
            let value = value
                .as_ref()
                .expect("all batch rows were validated before insertion");
            let row = sqlx::query_as::<_, Tag>(&sql)
                .bind(&value.name)
                .bind(input.collection_group_id)
                .bind(&value.address)
                .bind(&input.data_type)
                .bind(input.string_length)
                .bind(input.raw_lo)
                .bind(input.raw_hi)
                .bind(input.eng_lo)
                .bind(input.eng_hi)
                .bind(&value.unit)
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
                .fetch_one(&mut *connection)
                .await
                .map_err(|err| map_write_error(err, "name", "collectionGroupId", FK_MESSAGE))?;
            created.push(row);
        }

        Ok(BatchTagOutcome::Valid {
            count: created.len(),
            tags: Some(created),
        })
    }

    /// Bulk create - T11-1 (docs/ux-plan.md §3): the shared foundation both
    /// continuous registration and CSV import (T11-2) build on. Calling
    /// [`TagService::create`] `n` times would (a) be all-or-nothing only at
    /// the row level, leaving a partially-applied registry on a mid-batch
    /// failure, and (b) force the caller to rebuild the collector once per
    /// row (design: "1件ずつ POST を繰り返すと T7 の部分再構成がタグ追加の
    /// たびに走る（100点で100回）"). This method instead:
    ///
    /// 1. Validates every row ([`validate_tag_input`] +
    ///    [`validate_tag_kind_placement`]), collecting **every** row's
    ///    errors rather than stopping at the first (design: "行番号付き
    ///    エラー一覧").
    /// 2. Detects duplicate `name`s, both within the batch itself and
    ///    against already-persisted tags (design: "重複名(リクエスト内・
    ///    既存タグとの両方)も検証で検出").
    /// 3. If step 1 or 2 found anything, returns [`BatchTagOutcome::Invalid`]
    ///    and writes nothing at all, `dry_run` or not (design: "1件でも
    ///    エラーがあれば全体を拒否") - the caller does not need to re-check
    ///    this itself.
    /// 4. Otherwise, if `dry_run`, returns [`BatchTagOutcome::Valid`] with
    ///    `tags: None` - nothing was written, the caller only wanted to know
    ///    the batch *would* succeed (the continuous-registration preview,
    ///    and later T11-2's CSV dry-run step).
    /// 5. Otherwise, inserts every row in **one** `sqlx` transaction (design:
    ///    "単一トランザクションで全件 INSERT") and returns the persisted
    ///    rows in request order.
    ///
    /// This method never touches the collector - same as every other
    /// `TagService` method, rebuilding is the caller's job. The intended
    /// caller (`apps/banto-hub/core/src/rest.rs`'s `POST /api/tags/batch`)
    /// calls [`crate::plc_connection::PlcConnectionService`]'s sibling hub
    /// type (`CollectorManager::rebuild`) exactly once after a
    /// `Valid { tags: Some(_), .. }` result, never per row.
    ///
    /// A DB-level failure surfacing only at INSERT time (e.g. a `name` that
    /// became a duplicate, or a `collection_group_id` that stopped existing,
    /// between this method's own validation queries and the transaction
    /// below) is the one case this method cannot report per-row: it rolls
    /// the transaction back (nothing is written, all-or-nothing still holds)
    /// but surfaces as a plain `Err(BantoError::Validation)` with a single,
    /// unindexed `FieldError` rather than a `BatchTagOutcome::Invalid` entry.
    /// This is an accepted, intentionally-unhandled race window (judgment
    /// call, T11-1, 2026-08-07): closing it would require holding a
    /// transaction open across the whole validation pass, which would
    /// serialize every batch/CRUD write against every other one for the
    /// duration of a potentially large batch's validation queries - worse in
    /// practice than the rare surprise of a generic error message on an
    /// actual concurrent conflict.
    pub async fn create_batch(
        &self,
        inputs: Vec<TagInput>,
        dry_run: bool,
    ) -> Result<BatchTagOutcome, BantoError> {
        let mut row_errors: Vec<Vec<FieldError>> = vec![Vec::new(); inputs.len()];
        let mut validated: Vec<Option<ValidatedTag>> = Vec::with_capacity(inputs.len());

        for (index, input) in inputs.iter().enumerate() {
            match validate_tag_input(input) {
                Ok(v) => validated.push(Some(v)),
                Err(BantoError::Validation { field_errors }) => {
                    row_errors[index].extend(field_errors);
                    validated.push(None);
                }
                Err(other) => return Err(other),
            }
            match validate_tag_kind_placement(
                &self.pool,
                input.collection_group_id,
                &input.tag_kind,
            )
            .await
            {
                Ok(()) => {}
                Err(BantoError::Validation { field_errors }) => {
                    row_errors[index].extend(field_errors)
                }
                Err(other) => return Err(other),
            }
        }

        // Intra-batch duplicates: every index sharing a name with another
        // index gets flagged, not just the "later" occurrence - a preview/
        // CSV row list is easier to fix when every offending line is marked.
        let mut first_seen: HashMap<&str, usize> = HashMap::new();
        let mut batch_dupe_indices: Vec<usize> = Vec::new();
        for (index, v) in validated.iter().enumerate() {
            let Some(v) = v else { continue };
            match first_seen.get(v.name.as_str()) {
                Some(&first) => {
                    if !batch_dupe_indices.contains(&first) {
                        batch_dupe_indices.push(first);
                    }
                    batch_dupe_indices.push(index);
                }
                None => {
                    first_seen.insert(&v.name, index);
                }
            }
        }
        for index in batch_dupe_indices {
            row_errors[index].push(FieldError {
                field: "name".to_string(),
                message: "リクエスト内の他の行と名前が重複しています".to_string(),
            });
        }

        // Duplicates against already-persisted tags: one query covering
        // every syntactically-valid name in the batch.
        let candidate_names: Vec<&str> = validated
            .iter()
            .filter_map(|v| v.as_ref().map(|v| v.name.as_str()))
            .collect();
        if !candidate_names.is_empty() {
            let mut qb: QueryBuilder<'_, Sqlite> =
                QueryBuilder::new("SELECT name FROM tags WHERE name IN (");
            let mut separated = qb.separated(", ");
            for name in &candidate_names {
                separated.push_bind(*name);
            }
            qb.push(")");
            let existing: Vec<(String,)> = qb
                .build_query_as()
                .fetch_all(&self.pool)
                .await
                .map_err(banto_storage::storage_error)?;
            let existing_names: HashSet<String> = existing.into_iter().map(|(n,)| n).collect();
            for (index, v) in validated.iter().enumerate() {
                let Some(v) = v else { continue };
                if existing_names.contains(&v.name) {
                    row_errors[index].push(FieldError {
                        field: "name".to_string(),
                        message: "既に使用されています".to_string(),
                    });
                }
            }
        }

        let errors: Vec<BatchTagError> = row_errors
            .into_iter()
            .enumerate()
            .filter(|(_, field_errors)| !field_errors.is_empty())
            .map(|(index, field_errors)| BatchTagError {
                index,
                field_errors,
            })
            .collect();
        if !errors.is_empty() {
            return Ok(BatchTagOutcome::Invalid(errors));
        }

        if dry_run {
            return Ok(BatchTagOutcome::Valid {
                count: inputs.len(),
                tags: None,
            });
        }

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(banto_storage::storage_error)?;
        let mut created = Vec::with_capacity(inputs.len());
        let sql = insert_tag_sql();
        for (input, validated) in inputs.iter().zip(validated.iter()) {
            let validated = validated.as_ref().expect(
                "every row validated Ok in the pass above (errors would have returned already)",
            );
            let row = sqlx::query_as::<_, Tag>(&sql)
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
                .fetch_one(&mut *tx)
                .await
                .map_err(|err| map_write_error(err, "name", "collectionGroupId", FK_MESSAGE))?;
            created.push(row);
        }
        tx.commit().await.map_err(banto_storage::storage_error)?;

        Ok(BatchTagOutcome::Valid {
            count: created.len(),
            tags: Some(created),
        })
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
                simulation: false,

                word_order: "low_high".to_string(),
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
            expected_revision: None,
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

    // --- T18-1: revision optimistic lock ---------------------------------

    #[tokio::test]
    async fn create_sets_revision_to_one() {
        let (svc, group_id) = setup().await;
        let created = svc.create(sample_input("Rev1", group_id)).await.unwrap();
        assert_eq!(created.revision, 1);
    }

    #[tokio::test]
    async fn update_without_expected_revision_still_advances_it() {
        let (svc, group_id) = setup().await;
        let created = svc.create(sample_input("Rev2", group_id)).await.unwrap();
        assert_eq!(created.revision, 1);

        let mut input = sample_input("Rev2b", group_id);
        input.expected_revision = None;
        let updated = svc
            .update(created.id, input)
            .await
            .expect("update without expected_revision should still succeed");
        assert_eq!(updated.revision, 2);
    }

    #[tokio::test]
    async fn update_with_matching_expected_revision_succeeds_and_advances_it() {
        let (svc, group_id) = setup().await;
        let created = svc.create(sample_input("Rev3", group_id)).await.unwrap();

        let mut input = sample_input("Rev3b", group_id);
        input.expected_revision = Some(created.revision);
        let updated = svc
            .update(created.id, input)
            .await
            .expect("update with the current revision should succeed");
        assert_eq!(updated.revision, 2);
    }

    /// [`TagService::update`] (the pool-taking, relay-wright-compatible
    /// method) folds a stale `expected_revision` into a plain
    /// `BantoError::Other` - see that method's doc comment for why it does
    /// not return [`TagUpdateError`].
    #[tokio::test]
    async fn update_with_stale_expected_revision_is_rejected_as_other() {
        let (svc, group_id) = setup().await;
        let created = svc.create(sample_input("Rev4", group_id)).await.unwrap();

        // Another session updates first, advancing revision to 2.
        let mut first = sample_input("Rev4-updated", group_id);
        first.expected_revision = Some(created.revision);
        svc.update(created.id, first)
            .await
            .expect("first update should succeed");

        // A caller still holding the original (now stale) revision=1 is
        // rejected instead of silently overwriting the row above.
        let mut stale = sample_input("Rev4-stale", group_id);
        stale.expected_revision = Some(created.revision);
        let err = svc.update(created.id, stale).await.unwrap_err();
        match err {
            BantoError::Other(message) => assert!(
                message.contains("再読込"),
                "expected a reload-and-retry message, got {message:?}"
            ),
            other => panic!("expected BantoError::Other, got {other:?}"),
        }

        // The row itself must reflect the first (successful) update, not
        // the rejected stale one - "他セッション更新を黙って上書きしない".
        let current = svc.get(created.id).await.unwrap();
        assert_eq!(current.name, "Rev4-updated");
        assert_eq!(current.revision, 2);
    }

    #[tokio::test]
    async fn update_tx_with_matching_expected_revision_succeeds_and_advances_it() {
        let (svc, group_id) = setup().await;
        let created = svc.create(sample_input("Rev5", group_id)).await.unwrap();

        let mut input = sample_input("Rev5b", group_id);
        input.expected_revision = Some(created.revision);
        let mut tx = svc.pool.begin().await.expect("begin tx");
        let updated = svc
            .update_tx(&mut tx, created.id, input)
            .await
            .expect("update_tx with the current revision should succeed");
        assert_eq!(updated.revision, 2);
        tx.commit().await.expect("commit");
    }

    /// The richer [`TagUpdateError::RevisionConflict`] path (hub's
    /// `update_tx`, unlike the pool `update` above): a stale
    /// `expected_revision` returns the tag's CURRENT row so the caller can
    /// hand it back to the client instead of just a generic error.
    #[tokio::test]
    async fn update_tx_with_stale_expected_revision_returns_revision_conflict_with_current_tag() {
        let (svc, group_id) = setup().await;
        let created = svc.create(sample_input("Rev6", group_id)).await.unwrap();

        let mut first = sample_input("Rev6-updated", group_id);
        first.expected_revision = Some(created.revision);
        let after_first = svc
            .update(created.id, first)
            .await
            .expect("first update should succeed");

        let mut stale = sample_input("Rev6-stale", group_id);
        stale.expected_revision = Some(created.revision); // the original (now stale) revision=1
        let mut tx = svc.pool.begin().await.expect("begin tx");
        let err = svc.update_tx(&mut tx, created.id, stale).await.unwrap_err();
        match err {
            TagUpdateError::RevisionConflict(current) => {
                assert_eq!(current.id, created.id);
                assert_eq!(current.name, "Rev6-updated");
                assert_eq!(current.revision, after_first.revision);
            }
            other => panic!("expected RevisionConflict, got {other:?}"),
        }
        tx.rollback().await.expect("rollback");
    }

    /// A deleted id with `expected_revision` set must still report plain
    /// `NotFound` (there is no "current row" to conflict against), not
    /// `RevisionConflict`.
    #[tokio::test]
    async fn update_tx_with_expected_revision_on_a_deleted_id_is_not_found() {
        let (svc, group_id) = setup().await;
        let created = svc.create(sample_input("Rev7", group_id)).await.unwrap();
        svc.delete(created.id).await.expect("delete should succeed");

        let mut input = sample_input("Rev7-gone", group_id);
        input.expected_revision = Some(created.revision);
        let mut tx = svc.pool.begin().await.expect("begin tx");
        let err = svc.update_tx(&mut tx, created.id, input).await.unwrap_err();
        assert!(matches!(
            err,
            TagUpdateError::Banto(BantoError::NotFound { resource, id })
                if resource == "tags" && id == created.id.to_string()
        ));
        tx.rollback().await.expect("rollback");
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

    /// Test helper (T6-2): create a `"virtual"`-protocol connection named
    /// `conn_name` (e.g. [`CALC_CONNECTION_NAME`]/[`MEM_CONNECTION_NAME`])
    /// plus one collection group under it, and return the group's id - the
    /// shape [`validate_tag_kind_placement`] requires for `computed`/
    /// `internal` tags. Mirrors what `banto-hub` auto-provisions at startup
    /// (T6-2 architecture decision), but built directly here since this
    /// crate's tests do not depend on the hub layer.
    async fn virtual_group(pool: &sqlx::SqlitePool, conn_name: &str) -> i64 {
        let plc_svc = PlcConnectionService::new(pool.clone());
        let conn = plc_svc
            .create(PlcConnectionInput {
                name: conn_name.to_string(),
                protocol: VIRTUAL_PROTOCOL.to_string(),
                host: String::new(),
                port: 0,
                unit_id: 1,
                enabled: true,
                simulation: false,

                word_order: "low_high".to_string(),
            })
            .await
            .expect("virtual connection should be creatable");
        let group_svc = CollectionGroupService::new(pool.clone());
        let group = group_svc
            .create(CollectionGroupInput {
                name: format!("{conn_name}-group"),
                plc_connection_id: conn.id,
                period_ms: 1_000,
                enabled: true,
            })
            .await
            .expect("group under a virtual connection should be creatable");
        group.id
    }

    /// design §4.2's reserved namespace (T6-2): a `computed` tag must live
    /// under the `calc` virtual connection, and must supply `expression`
    /// with a blank `address`.
    #[tokio::test]
    async fn create_accepts_a_computed_tag_under_calc() {
        let (svc, _plc_group_id) = setup().await;
        let calc_group = virtual_group(&svc.pool, CALC_CONNECTION_NAME).await;

        let mut input = sample_input("avg", calc_group);
        input.tag_kind = COMPUTED_TAG_KIND.to_string();
        input.address = String::new();
        input.expression = Some("(a + b) / 2".to_string());
        let created = svc
            .create(input)
            .await
            .expect("a computed tag under calc should be accepted");
        assert_eq!(created.tag_kind, "computed");
        assert_eq!(created.address, "");
        assert_eq!(created.expression.as_deref(), Some("(a + b) / 2"));
    }

    /// The `internal` sibling: must live under `mem`, forbids `expression`,
    /// and may set `retain`.
    #[tokio::test]
    async fn create_accepts_an_internal_tag_under_mem() {
        let (svc, _plc_group_id) = setup().await;
        let mem_group = virtual_group(&svc.pool, MEM_CONNECTION_NAME).await;

        let mut input = sample_input("setpoint1", mem_group);
        input.tag_kind = INTERNAL_TAG_KIND.to_string();
        input.address = String::new();
        input.writable = true;
        input.retain = true;
        let created = svc
            .create(input)
            .await
            .expect("an internal tag under mem should be accepted");
        assert_eq!(created.tag_kind, "internal");
        assert_eq!(created.address, "");
        assert_eq!(created.expression, None);
        assert!(created.retain);
        assert!(created.writable);
    }

    /// A `computed`/`internal` tag whose group sits under an ordinary (non-
    /// virtual) connection is rejected - the reserved namespace is not
    /// optional.
    #[tokio::test]
    async fn create_rejects_computed_and_internal_outside_their_reserved_connection() {
        let (svc, plc_group_id) = setup().await;
        for kind in [COMPUTED_TAG_KIND, INTERNAL_TAG_KIND] {
            let mut input = sample_input(&format!("K-{kind}"), plc_group_id);
            input.tag_kind = kind.to_string();
            input.address = String::new();
            if kind == COMPUTED_TAG_KIND {
                input.expression = Some("1 + 1".to_string());
            }
            let err = svc.create(input).await.unwrap_err();
            match err {
                BantoError::Validation { field_errors } => {
                    assert_eq!(field_errors[0].field, "tagKind", "kind={kind}");
                    assert!(
                        field_errors[0].message.contains("予約接続"),
                        "kind={kind} message={:?}",
                        field_errors[0].message
                    );
                }
                other => panic!("expected Validation for kind={kind}, got {other:?}"),
            }
        }
    }

    /// The reverse direction: a `plc` tag cannot be placed under either
    /// reserved virtual connection.
    #[tokio::test]
    async fn create_rejects_a_plc_tag_under_a_virtual_connection() {
        let (svc, _plc_group_id) = setup().await;
        let calc_group = virtual_group(&svc.pool, CALC_CONNECTION_NAME).await;
        let input = sample_input("X", calc_group);
        let err = svc.create(input).await.unwrap_err();
        match err {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "tagKind");
                assert!(field_errors[0].message.contains("予約接続"));
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    /// `computed`/`internal` reject a non-empty `address` even when placed
    /// correctly under `calc`/`mem` - design §4.2's table ("address なし").
    #[tokio::test]
    async fn create_rejects_a_non_empty_address_on_computed_and_internal() {
        let (svc, _plc_group_id) = setup().await;
        let calc_group = virtual_group(&svc.pool, CALC_CONNECTION_NAME).await;
        let mut input = sample_input("X", calc_group);
        input.tag_kind = COMPUTED_TAG_KIND.to_string();
        input.expression = Some("1 + 1".to_string());
        // sample_input's default address ("40001") is left as-is.
        let err = svc.create(input).await.unwrap_err();
        match err {
            BantoError::Validation { field_errors } => {
                assert!(field_errors.iter().any(|e| e.field == "address"));
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    /// `computed` requires `expression`; omitting it (even with everything
    /// else correct) is rejected.
    #[tokio::test]
    async fn create_rejects_a_computed_tag_without_an_expression() {
        let (svc, _plc_group_id) = setup().await;
        let calc_group = virtual_group(&svc.pool, CALC_CONNECTION_NAME).await;
        let mut input = sample_input("X", calc_group);
        input.tag_kind = COMPUTED_TAG_KIND.to_string();
        input.address = String::new();
        let err = svc.create(input).await.unwrap_err();
        match err {
            BantoError::Validation { field_errors } => {
                assert!(field_errors.iter().any(|e| e.field == "expression"));
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    /// `internal` forbids `expression` even under the correct `mem` group.
    #[tokio::test]
    async fn create_rejects_an_internal_tag_with_an_expression() {
        let (svc, _plc_group_id) = setup().await;
        let mem_group = virtual_group(&svc.pool, MEM_CONNECTION_NAME).await;
        let mut input = sample_input("X", mem_group);
        input.tag_kind = INTERNAL_TAG_KIND.to_string();
        input.address = String::new();
        input.expression = Some("1 + 1".to_string());
        let err = svc.create(input).await.unwrap_err();
        match err {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "expression");
                assert_eq!(
                    field_errors[0].message,
                    "internal タグには expression を設定できません"
                );
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    /// design §4.2's table: a computed tag's value is always the expression
    /// result, never a client write - `writable` is forced false at
    /// registration so the write path's existing "writable == false -> 403"
    /// gate already covers "computed タグへの書き込みは常に403" with no
    /// special-casing in the write path itself.
    #[tokio::test]
    async fn create_rejects_a_writable_computed_tag() {
        let (svc, _plc_group_id) = setup().await;
        let calc_group = virtual_group(&svc.pool, CALC_CONNECTION_NAME).await;
        let mut input = sample_input("X", calc_group);
        input.tag_kind = COMPUTED_TAG_KIND.to_string();
        input.address = String::new();
        input.expression = Some("1 + 1".to_string());
        input.writable = true;
        let err = svc.create(input).await.unwrap_err();
        match err {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "writable");
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    /// `string` data type makes no sense for a computed/internal tag (their
    /// value is always numeric - banto-expr's own type system, and
    /// `ServerTagStore`'s value slot, are both `f64`-only).
    #[tokio::test]
    async fn create_rejects_string_data_type_on_computed_and_internal() {
        let (svc, _plc_group_id) = setup().await;
        let calc_group = virtual_group(&svc.pool, CALC_CONNECTION_NAME).await;
        let mut computed = sample_input("X", calc_group);
        computed.tag_kind = COMPUTED_TAG_KIND.to_string();
        computed.address = String::new();
        computed.expression = Some("1 + 1".to_string());
        computed.data_type = STRING_DATA_TYPE.to_string();
        let err = svc.create(computed).await.unwrap_err();
        match err {
            BantoError::Validation { field_errors } => {
                assert!(field_errors.iter().any(|e| e.field == "dataType"));
            }
            other => panic!("expected Validation for computed, got {other:?}"),
        }

        let mem_group = virtual_group(&svc.pool, MEM_CONNECTION_NAME).await;
        let mut internal = sample_input("Y", mem_group);
        internal.tag_kind = INTERNAL_TAG_KIND.to_string();
        internal.address = String::new();
        internal.data_type = STRING_DATA_TYPE.to_string();
        let err = svc.create(internal).await.unwrap_err();
        match err {
            BantoError::Validation { field_errors } => {
                assert!(field_errors.iter().any(|e| e.field == "dataType"));
            }
            other => panic!("expected Validation for internal, got {other:?}"),
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

    // --- create_batch (T11-1, docs/ux-plan.md §3) --------------------------

    #[tokio::test]
    async fn create_batch_persists_every_row_in_request_order() {
        let (svc, group_id) = setup().await;
        let inputs = vec![
            sample_input("Batch1", group_id),
            sample_input("Batch2", group_id),
            sample_input("Batch3", group_id),
        ];
        let outcome = svc
            .create_batch(inputs, false)
            .await
            .expect("create_batch should succeed");
        match outcome {
            BatchTagOutcome::Valid { count, tags } => {
                assert_eq!(count, 3);
                let tags = tags.expect("a non-dry-run apply returns the created rows");
                assert_eq!(
                    tags.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
                    vec!["Batch1", "Batch2", "Batch3"]
                );
            }
            other => panic!("expected Valid, got {other:?}"),
        }

        let all = svc.list(ListParams::default()).await.unwrap();
        assert_eq!(all.total_count, 3);
    }

    #[tokio::test]
    async fn create_batch_rejects_everything_when_one_row_is_invalid() {
        let (svc, group_id) = setup().await;
        let mut bad = sample_input("Bad", group_id);
        bad.data_type = "f64".to_string(); // not in ALLOWED_DATA_TYPES
        let inputs = vec![
            sample_input("Good1", group_id),
            bad,
            sample_input("Good2", group_id),
        ];

        let outcome = svc
            .create_batch(inputs, false)
            .await
            .expect("create_batch should not error - it reports invalid rows instead");
        match outcome {
            BatchTagOutcome::Invalid(errors) => {
                assert_eq!(errors.len(), 1, "{errors:?}");
                assert_eq!(errors[0].index, 1);
                assert!(errors[0].field_errors.iter().any(|e| e.field == "dataType"));
            }
            other => panic!("expected Invalid, got {other:?}"),
        }

        // All-or-nothing: not even the two valid rows were written.
        let all = svc.list(ListParams::default()).await.unwrap();
        assert_eq!(all.total_count, 0);
    }

    #[tokio::test]
    async fn create_batch_dry_run_validates_without_writing() {
        let (svc, group_id) = setup().await;
        let inputs = vec![
            sample_input("Preview1", group_id),
            sample_input("Preview2", group_id),
        ];

        let outcome = svc
            .create_batch(inputs, true)
            .await
            .expect("create_batch should succeed");
        match outcome {
            BatchTagOutcome::Valid { count, tags } => {
                assert_eq!(count, 2);
                assert!(tags.is_none(), "dry run must not report created rows");
            }
            other => panic!("expected Valid, got {other:?}"),
        }

        let all = svc.list(ListParams::default()).await.unwrap();
        assert_eq!(all.total_count, 0, "dry run must not write anything");
    }

    #[tokio::test]
    async fn create_batch_flags_every_index_sharing_a_duplicate_name_within_the_request() {
        let (svc, group_id) = setup().await;
        let inputs = vec![
            sample_input("Dup", group_id),
            sample_input("Unique", group_id),
            sample_input("Dup", group_id),
        ];

        let outcome = svc.create_batch(inputs, true).await.unwrap();
        match outcome {
            BatchTagOutcome::Invalid(errors) => {
                let indices: Vec<usize> = errors.iter().map(|e| e.index).collect();
                assert_eq!(indices, vec![0, 2], "{errors:?}");
                for err in &errors {
                    assert!(err.field_errors.iter().any(|e| e.field == "name"));
                }
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_batch_flags_a_name_already_used_by_an_existing_tag() {
        let (svc, group_id) = setup().await;
        svc.create(sample_input("Existing", group_id))
            .await
            .unwrap();

        let outcome = svc
            .create_batch(vec![sample_input("Existing", group_id)], false)
            .await
            .unwrap();
        match outcome {
            BatchTagOutcome::Invalid(errors) => {
                assert_eq!(errors.len(), 1);
                assert_eq!(errors[0].index, 0);
                assert_eq!(errors[0].field_errors[0].field, "name");
                assert_eq!(errors[0].field_errors[0].message, "既に使用されています");
            }
            other => panic!("expected Invalid, got {other:?}"),
        }

        // Still just the one pre-existing row.
        let all = svc.list(ListParams::default()).await.unwrap();
        assert_eq!(all.total_count, 1);
    }

    #[tokio::test]
    async fn create_batch_with_an_empty_vec_succeeds_trivially() {
        let (svc, _group_id) = setup().await;
        let outcome = svc.create_batch(Vec::new(), false).await.unwrap();
        match outcome {
            BatchTagOutcome::Valid { count, tags } => {
                assert_eq!(count, 0);
                assert_eq!(tags, Some(Vec::new()));
            }
            other => panic!("expected Valid, got {other:?}"),
        }
    }
}
