//! Write rule: one condition→action rule (plan `luminous-discovering-goblet.md`,
//! W1/W2), backed by the `write_rules` table (`migrations/0006_write_rules.sql`)
//! plus its 1..N AND-combined [`crate::write_rule_conditions`] child rows.
//!
//! Conditions have no independent CRUD - a rule is always created, read, and
//! updated together with its full condition set (the rule form's inline
//! 1..N rows), so this is an AGGREGATE service: [`WriteRuleService`] persists
//! a rule and its conditions atomically (one transaction), and every read
//! returns the rule with its conditions ([`WriteRuleDetail`]).
//!
//! ## Invariants (docs/conventions.md)
//! - §2 (サービス層非依存): `Clone` + `SqlitePool` + `BantoError` only, no
//!   tauri/axum/RBAC/HTTP. Authz + audit are the wiring layer's job.
//! - SQL columns reached only through the [`column_map`] whitelist for list
//!   filter/sort.
//!
//! ## Cross-lineage references (validated here, no SQL FOREIGN KEY)
//! `write_source_tag_id` and each condition's `source_tag_id` reference
//! banto-tags-owned `tags` rows across the migrator boundary (see
//! `0005_write_targets.sql`'s doc comment); `write_target_id` is an
//! in-lineage FK but is still validated here so the message is friendly and
//! deterministic rather than a raw SQLite constraint error.
//!
//! ## Write-loop cycle detection (the one non-CRUD bit, plan W2)
//! On create/update of an ENABLED rule, [`WriteRuleService`] walks the
//! source→target graph over PLC devices `(plc_connection_id, address)` across
//! all enabled rules and rejects a save that would close a write-feedback
//! loop (rule A reads device X and writes Y while rule B reads Y and writes X
//! → cycle). See [`WriteRuleService::check_no_write_cycle`].

use std::collections::{HashMap, HashSet};

use banto_core::{BantoError, FieldError, ListParams, ListResult};
use banto_storage::ColumnMap;
use serde::{Deserialize, Serialize};
use sqlx::{QueryBuilder, Sqlite, SqlitePool};

use crate::support::{map_write_error, max_length_message, required_message, sjis_text_error};
use crate::write_rule_conditions::{
    validate_condition_input, SourceTagKind, WriteRuleCondition, WriteRuleConditionInput,
};

const MAX_NAME_LEN: usize = 100;

/// Edge-detection modes accepted in `write_rules.edge_mode` (mirrors the SQL
/// CHECK in 0006; change both together).
pub const ALLOWED_EDGE_MODES: &[&str] = &["rising", "falling", "change"];

/// Write-value modes accepted in `write_rules.write_value_mode` (mirrors the
/// SQL CHECK in 0006; change both together).
pub const ALLOWED_WRITE_VALUE_MODES: &[&str] = &["constant", "copy_from_source"];

/// A row of the `write_rules` table, wire-shaped (camelCase). Serialized flat
/// and re-used (via `#[serde(flatten)]`) inside [`WriteRuleDetail`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct WriteRule {
    pub id: i64,
    pub name: String,
    pub enabled: bool,
    pub edge_mode: String,
    pub cooldown_ms: Option<i64>,
    pub write_target_id: i64,
    pub write_value_mode: String,
    pub write_constant_value: Option<f64>,
    /// The constant for a STRING write target (S2 文字列タグ) - exactly one
    /// of `write_constant_value`/`write_constant_text` is set for a
    /// constant-mode rule, decided by the target's data type.
    pub write_constant_text: Option<String>,
    pub write_source_tag_id: Option<i64>,
}

/// A rule plus its AND-combined conditions - the shape every read/write on
/// this aggregate returns. `Deserialize` (alongside `Serialize`) so the
/// project export/import (`crate::project`) can round-trip this exact wire
/// bundle back through a project file - the `flatten`ed `WriteRule` and the
/// `WriteRuleCondition` rows are both already `Deserialize`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WriteRuleDetail {
    #[serde(flatten)]
    pub rule: WriteRule,
    pub conditions: Vec<WriteRuleCondition>,
}

/// Create/update payload: the rule fields plus its full condition set.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteRuleInput {
    pub name: String,
    #[serde(default)]
    pub enabled: bool,
    pub edge_mode: String,
    #[serde(default)]
    pub cooldown_ms: Option<i64>,
    pub write_target_id: i64,
    pub write_value_mode: String,
    #[serde(default)]
    pub write_constant_value: Option<f64>,
    #[serde(default)]
    pub write_constant_text: Option<String>,
    #[serde(default)]
    pub write_source_tag_id: Option<i64>,
    #[serde(default)]
    pub conditions: Vec<WriteRuleConditionInput>,
}

/// A PLC device identity: `(plc_connection_id, normalized address)`. Two rules
/// referencing the same device (whether one reads it and the other writes it)
/// meet at the same node of the write-loop graph.
type Device = (i64, String);

/// A write target's or source tag's `(data_type, string_length)` as resolved
/// at save time (S2 文字列タグ) - the inputs to [`kind_of`].
type TypeMeta = (String, Option<i64>);

/// Collapse a resolved [`TypeMeta`] to the shape validation cares about. A
/// string row whose `string_length` is somehow NULL (impossible for a row
/// that passed its own registry validation - defensive only) gets length 0,
/// which every non-empty comparand/constant then fails against loudly rather
/// than silently passing.
fn kind_of(meta: &TypeMeta) -> SourceTagKind {
    if meta.0 == banto_tags::STRING_DATA_TYPE {
        SourceTagKind::Str {
            length: meta.1.unwrap_or(0),
        }
    } else {
        SourceTagKind::Numeric
    }
}

fn column_map() -> ColumnMap {
    ColumnMap::new()
        .column("id", "id")
        .column("name", "name")
        .column("enabled", "enabled")
        .column("edgeMode", "edge_mode")
        .column("cooldownMs", "cooldown_ms")
        .column("writeTargetId", "write_target_id")
        .column("writeValueMode", "write_value_mode")
        .column("writeConstantValue", "write_constant_value")
        .column("writeConstantText", "write_constant_text")
        .column("writeSourceTagId", "write_source_tag_id")
}

const RESOURCE: &str = "write_rules";
const COLUMNS: &str = "id, name, enabled, edge_mode, cooldown_ms, write_target_id, \
     write_value_mode, write_constant_value, write_constant_text, write_source_tag_id";
const TARGET_FK_MESSAGE: &str = "指定された書き込み先が見つかりません";

/// Aggregate service for the `write_rules` resource and its condition rows.
#[derive(Clone)]
pub struct WriteRuleService {
    pool: SqlitePool,
}

impl WriteRuleService {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    // --- existence/type checks (cross-lineage refs, no SQL FK) --------------

    /// The write target's `(data_type, string_length)`, or `None` if the row
    /// does not exist. S2: type resolution replaces the old bare existence
    /// check because the constant/copy value fields' validity now depends on
    /// the TARGET's type (see [`Self::collect_errors`]).
    async fn target_meta(&self, id: i64) -> Result<Option<TypeMeta>, BantoError> {
        sqlx::query_as("SELECT data_type, string_length FROM write_targets WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(banto_storage::storage_error)
    }

    /// A source tag's `(data_type, string_length)` from banto-tags' `tags`
    /// table, or `None` if the row does not exist - resolved at save time
    /// exactly like [`Self::tag_device`] already resolves addresses for the
    /// write-cycle check.
    async fn tag_meta(&self, id: i64) -> Result<Option<TypeMeta>, BantoError> {
        sqlx::query_as("SELECT data_type, string_length FROM tags WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(banto_storage::storage_error)
    }

    // --- validation ---------------------------------------------------------

    /// Collect EVERY field violation of a [`WriteRuleInput`] except the
    /// write-cycle check (which is done afterward, only for enabled rules).
    async fn collect_errors(&self, input: &WriteRuleInput) -> Result<Vec<FieldError>, BantoError> {
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

        if !ALLOWED_EDGE_MODES.contains(&input.edge_mode.as_str()) {
            errors.push(FieldError {
                field: "edgeMode".to_string(),
                message: format!(
                    "対応エッジモードは {} のいずれかです",
                    ALLOWED_EDGE_MODES.join(", ")
                ),
            });
        }

        if let Some(cooldown) = input.cooldown_ms {
            if cooldown < 0 {
                errors.push(FieldError {
                    field: "cooldownMs".to_string(),
                    message: "クールダウンは0以上にしてください".to_string(),
                });
            }
        }

        // Resolve the target's type first: the constant/copy value fields'
        // validity depends on it (S2 文字列タグ).
        let target_meta = self.target_meta(input.write_target_id).await?;
        if target_meta.is_none() {
            errors.push(FieldError {
                field: "writeTargetId".to_string(),
                message: TARGET_FK_MESSAGE.to_string(),
            });
        }
        let target_kind = target_meta.as_ref().map(kind_of);

        // Write value mode + its dependent fields. The type-dependent checks
        // run only when the target resolved - an unresolved target already
        // produced its own error above, and guessing its type would only
        // stack a confusing second message on the same save.
        match input.write_value_mode.as_str() {
            "constant" => match target_kind {
                Some(SourceTagKind::Str { length }) => {
                    // A STRING target's constant lives in write_constant_text
                    // (validated against the TARGET's byte budget); a numeric
                    // constant on it is meaningless and rejected, not
                    // silently dropped.
                    if input.write_constant_value.is_some() {
                        errors.push(FieldError {
                            field: "writeConstantValue".to_string(),
                            message: "文字列書き込み先には数値定数は設定できません".to_string(),
                        });
                    }
                    match input.write_constant_text.as_deref() {
                        None | Some("") => errors.push(FieldError {
                            field: "writeConstantText".to_string(),
                            message: "定数書き込みには書き込む文字列が必要です".to_string(),
                        }),
                        Some(text) => {
                            if let Some(message) = sjis_text_error(text, length) {
                                errors.push(FieldError {
                                    field: "writeConstantText".to_string(),
                                    message,
                                });
                            }
                        }
                    }
                }
                Some(SourceTagKind::Numeric) => {
                    if input.write_constant_text.is_some() {
                        errors.push(FieldError {
                            field: "writeConstantText".to_string(),
                            message: "数値書き込み先には文字列定数は設定できません".to_string(),
                        });
                    }
                    if input.write_constant_value.is_none() {
                        errors.push(FieldError {
                            field: "writeConstantValue".to_string(),
                            message: "定数書き込みには書き込む値が必要です".to_string(),
                        });
                    }
                }
                None => {}
            },
            "copy_from_source" => match input.write_source_tag_id {
                None => errors.push(FieldError {
                    field: "writeSourceTagId".to_string(),
                    message: "ソース値のコピーには参照元タグが必要です".to_string(),
                }),
                Some(tag_id) => match self.tag_meta(tag_id).await? {
                    None => errors.push(FieldError {
                        field: "writeSourceTagId".to_string(),
                        message: "指定された参照元タグが見つかりません".to_string(),
                    }),
                    Some(source_meta) => {
                        // S2: string⇔numeric copies cannot be represented on
                        // the wire; string→string additionally needs the
                        // target span to hold the source's worst case
                        // (target length ≥ source length), else a full-length
                        // source value would fail at write time forever.
                        match (kind_of(&source_meta), target_kind) {
                            (
                                SourceTagKind::Str { length: src_len },
                                Some(SourceTagKind::Str { length: tgt_len }),
                            ) => {
                                if tgt_len < src_len {
                                    errors.push(FieldError {
                                        field: "writeSourceTagId".to_string(),
                                        message: format!(
                                            "コピー先の文字列長（{tgt_len}語）がコピー元（{src_len}語）より短いため、コピーできません"
                                        ),
                                    });
                                }
                            }
                            (SourceTagKind::Str { .. }, Some(SourceTagKind::Numeric)) => {
                                errors.push(FieldError {
                                    field: "writeSourceTagId".to_string(),
                                    message: "文字列タグの値を数値書き込み先へはコピーできません"
                                        .to_string(),
                                });
                            }
                            (SourceTagKind::Numeric, Some(SourceTagKind::Str { .. })) => {
                                errors.push(FieldError {
                                    field: "writeSourceTagId".to_string(),
                                    message: "数値タグの値を文字列書き込み先へはコピーできません"
                                        .to_string(),
                                });
                            }
                            // numeric→numeric (any width combo): allowed, as
                            // before. Unresolved target: covered above.
                            (SourceTagKind::Numeric, Some(SourceTagKind::Numeric)) | (_, None) => {}
                        }
                    }
                },
            },
            _ => errors.push(FieldError {
                field: "writeValueMode".to_string(),
                message: format!(
                    "対応書き込みモードは {} のいずれかです",
                    ALLOWED_WRITE_VALUE_MODES.join(", ")
                ),
            }),
        }

        // Conditions: at least one, each valid (against its source tag's
        // resolved type - see write_rule_conditions.rs), each source tag
        // existing.
        if input.conditions.is_empty() {
            errors.push(FieldError {
                field: "conditions".to_string(),
                message: "条件を1つ以上設定してください".to_string(),
            });
        }
        for (i, condition) in input.conditions.iter().enumerate() {
            let source_meta = self.tag_meta(condition.source_tag_id).await?;
            if source_meta.is_none() {
                errors.push(FieldError {
                    field: format!("conditions.{i}.sourceTagId"),
                    message: "指定されたソースタグが見つかりません".to_string(),
                });
            }
            errors.extend(validate_condition_input(
                condition,
                i,
                source_meta.as_ref().map(kind_of),
            ));
        }

        Ok(errors)
    }

    // --- device resolution + cycle detection --------------------------------

    async fn target_device(&self, write_target_id: i64) -> Result<Option<Device>, BantoError> {
        let row: Option<(i64, String)> =
            sqlx::query_as("SELECT plc_connection_id, address FROM write_targets WHERE id = ?")
                .bind(write_target_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(banto_storage::storage_error)?;
        Ok(row.map(|(conn, addr)| (conn, addr.trim().to_string())))
    }

    /// Resolve a source tag id to its PLC device via the banto-tags-owned
    /// `tags` → `collection_groups` join (a tag's `plc_connection_id` comes
    /// from its collection group, `crates/banto-tags/migrations`).
    async fn tag_device(&self, tag_id: i64) -> Result<Option<Device>, BantoError> {
        let row: Option<(i64, String)> = sqlx::query_as(
            "SELECT cg.plc_connection_id, t.address \
             FROM tags t JOIN collection_groups cg ON t.collection_group_id = cg.id \
             WHERE t.id = ?",
        )
        .bind(tag_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(banto_storage::storage_error)?;
        Ok(row.map(|(conn, addr)| (conn, addr.trim().to_string())))
    }

    /// The source devices + target device of one enabled rule, as needed to
    /// build the write-loop graph. Returns `None` for a rule whose references
    /// cannot be resolved (should not happen for a validated rule).
    async fn rule_devices(
        &self,
        write_target_id: i64,
        source_tag_ids: &[i64],
    ) -> Result<Option<(Vec<Device>, Device)>, BantoError> {
        let Some(target) = self.target_device(write_target_id).await? else {
            return Ok(None);
        };
        let mut sources = Vec::with_capacity(source_tag_ids.len());
        for &tag_id in source_tag_ids {
            match self.tag_device(tag_id).await? {
                Some(device) => sources.push(device),
                None => return Ok(None),
            }
        }
        Ok(Some((sources, target)))
    }

    /// Reject the save if enabling `input` would close a write-feedback loop.
    /// No-op when the rule is disabled (a disabled rule contributes no edges,
    /// so it can never create a cycle - and a disabled rule sitting in a path
    /// does not count either, since only enabled rules are loaded below).
    ///
    /// Model: nodes are PLC devices `(plc_connection_id, address)`; each
    /// enabled rule adds an edge from every one of its condition source
    /// devices to its single target device (information flows source→target).
    /// A cycle in this directed graph is a write-feedback loop. `input`
    /// introduces a cycle iff, for one of its source devices `S` and its
    /// target `T`, either `S == T` (self-loop) or `T` can already reach `S`
    /// through the other enabled rules' edges. `exclude_rule_id` is the row
    /// being updated (its OLD version in the DB is ignored so an in-place edit
    /// is judged against its NEW shape, not both).
    ///
    /// `write_source_tag_id` (copy_from_source's value source) is deliberately
    /// NOT an edge: a rule only *fires* when a condition device changes, so
    /// only condition→target edges can propagate a trigger. A loop that exists
    /// solely through the copy-value channel (A copied to B by one rule, B
    /// copied back to A by another) cannot re-trigger itself - every lap
    /// around it requires an independent condition edge, which this graph
    /// already models. Treating copy sources as edges would reject those
    /// bounded configurations without making anything safer (pinned by
    /// `copy_from_source_value_channel_alone_is_not_a_feedback_edge` below).
    async fn check_no_write_cycle(
        &self,
        input: &WriteRuleInput,
        exclude_rule_id: Option<i64>,
    ) -> Result<(), BantoError> {
        if !input.enabled {
            return Ok(());
        }

        // Candidate devices.
        let candidate_target = match self.target_device(input.write_target_id).await? {
            Some(device) => device,
            None => return Ok(()), // unresolved target: nothing to reason about
        };
        let mut candidate_sources: Vec<Device> = Vec::new();
        for condition in &input.conditions {
            if let Some(device) = self.tag_device(condition.source_tag_id).await? {
                candidate_sources.push(device);
            }
        }

        // Load the OTHER enabled rules and build the edge set. Edges are
        // labeled with the rule name so a detected cycle can name a culprit.
        let other_rules: Vec<(i64, String, i64)> = {
            let mut builder: QueryBuilder<Sqlite> = QueryBuilder::new(
                "SELECT id, name, write_target_id FROM write_rules WHERE enabled = 1",
            );
            if let Some(id) = exclude_rule_id {
                builder.push(" AND id <> ").push_bind(id);
            }
            builder
                .build_query_as::<(i64, String, i64)>()
                .fetch_all(&self.pool)
                .await
                .map_err(banto_storage::storage_error)?
        };

        // adjacency: device -> list of (next device, rule name)
        let mut adjacency: HashMap<Device, Vec<(Device, String)>> = HashMap::new();
        // Include the candidate's own edges too (harmless for a T→S search).
        for source in &candidate_sources {
            adjacency
                .entry(source.clone())
                .or_default()
                .push((candidate_target.clone(), input.name.trim().to_string()));
        }
        for (rule_id, rule_name, write_target_id) in &other_rules {
            let source_tag_ids: Vec<i64> = sqlx::query_scalar(
                "SELECT source_tag_id FROM write_rule_conditions WHERE write_rule_id = ?",
            )
            .bind(rule_id)
            .fetch_all(&self.pool)
            .await
            .map_err(banto_storage::storage_error)?;
            if let Some((sources, target)) =
                self.rule_devices(*write_target_id, &source_tag_ids).await?
            {
                for source in sources {
                    adjacency
                        .entry(source)
                        .or_default()
                        .push((target.clone(), rule_name.clone()));
                }
            }
        }

        // For each candidate source S and target T, look for a cycle.
        for source in &candidate_sources {
            if source == &candidate_target {
                return Err(cycle_error(input.name.trim(), input.name.trim()));
            }
            if let Some(path_rules) = find_path(&adjacency, &candidate_target, source) {
                let culprit = path_rules
                    .iter()
                    .find(|name| name.as_str() != input.name.trim())
                    .cloned()
                    .unwrap_or_else(|| input.name.trim().to_string());
                return Err(cycle_error(input.name.trim(), &culprit));
            }
        }

        Ok(())
    }

    // --- CRUD ---------------------------------------------------------------

    pub async fn list(
        &self,
        params: ListParams,
    ) -> Result<ListResult<WriteRuleDetail>, BantoError> {
        let columns = column_map();

        let mut rows_builder: QueryBuilder<Sqlite> =
            QueryBuilder::new(format!("SELECT {COLUMNS} FROM write_rules"));
        banto_storage::list_query::sqlite::apply_list_params(&mut rows_builder, &columns, &params)?;
        let rules: Vec<WriteRule> = rows_builder
            .build_query_as::<WriteRule>()
            .fetch_all(&self.pool)
            .await
            .map_err(banto_storage::storage_error)?;

        let mut details = Vec::with_capacity(rules.len());
        for rule in rules {
            let conditions = self.conditions_for(rule.id).await?;
            details.push(WriteRuleDetail { rule, conditions });
        }

        let mut count_builder: QueryBuilder<Sqlite> =
            QueryBuilder::new("SELECT COUNT(*) FROM write_rules");
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
            rows: details,
            total_count: total_count as u64,
        })
    }

    async fn conditions_for(&self, rule_id: i64) -> Result<Vec<WriteRuleCondition>, BantoError> {
        sqlx::query_as::<_, WriteRuleCondition>(
            "SELECT id, write_rule_id, source_tag_id, operator, threshold_value, \
                    threshold_value_2, threshold_text \
             FROM write_rule_conditions WHERE write_rule_id = ? ORDER BY id",
        )
        .bind(rule_id)
        .fetch_all(&self.pool)
        .await
        .map_err(banto_storage::storage_error)
    }

    pub async fn get(&self, id: i64) -> Result<WriteRuleDetail, BantoError> {
        // AssertSqlSafe: 補間されるのは COLUMNS 定数（本ファイル内の固定文字列）
        // のみで、外部入力は含まれない。id はプレースホルダでバインドする。
        let rule = sqlx::query_as::<_, WriteRule>(sqlx::AssertSqlSafe(format!(
            "SELECT {COLUMNS} FROM write_rules WHERE id = ?"
        )))
        .bind(id)
        .fetch_one(&self.pool)
        .await
        .map_err(|err| banto_storage::not_found(err, RESOURCE, id.to_string()))?;
        let conditions = self.conditions_for(rule.id).await?;
        Ok(WriteRuleDetail { rule, conditions })
    }

    pub async fn create(&self, input: WriteRuleInput) -> Result<WriteRuleDetail, BantoError> {
        let errors = self.collect_errors(&input).await?;
        if !errors.is_empty() {
            return Err(BantoError::Validation {
                field_errors: errors,
            });
        }
        self.check_no_write_cycle(&input, None).await?;

        let normalized = normalize(&input);
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(banto_storage::storage_error)?;

        // AssertSqlSafe: get() と同じ理由 - COLUMNS 定数のみを埋め込む固定
        // 文字列。値はすべてプレースホルダでバインドする。
        let rule = sqlx::query_as::<_, WriteRule>(sqlx::AssertSqlSafe(format!(
            "INSERT INTO write_rules (\
                name, enabled, edge_mode, cooldown_ms, write_target_id, \
                write_value_mode, write_constant_value, write_constant_text, write_source_tag_id\
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING {COLUMNS}"
        )))
        .bind(input.name.trim())
        .bind(input.enabled)
        .bind(&input.edge_mode)
        .bind(input.cooldown_ms)
        .bind(input.write_target_id)
        .bind(&input.write_value_mode)
        .bind(normalized.constant_value)
        .bind(&normalized.constant_text)
        .bind(normalized.source_tag_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|err| map_write_error(err, "name", "writeTargetId", TARGET_FK_MESSAGE))?;

        insert_conditions(&mut tx, rule.id, &input.conditions).await?;
        tx.commit().await.map_err(banto_storage::storage_error)?;

        let conditions = self.conditions_for(rule.id).await?;
        Ok(WriteRuleDetail { rule, conditions })
    }

    pub async fn update(
        &self,
        id: i64,
        input: WriteRuleInput,
    ) -> Result<WriteRuleDetail, BantoError> {
        let errors = self.collect_errors(&input).await?;
        if !errors.is_empty() {
            return Err(BantoError::Validation {
                field_errors: errors,
            });
        }
        self.check_no_write_cycle(&input, Some(id)).await?;

        let normalized = normalize(&input);
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(banto_storage::storage_error)?;

        // AssertSqlSafe: get() と同じ理由 - COLUMNS 定数のみを埋め込む固定
        // 文字列。値はすべてプレースホルダでバインドする。
        let rule = sqlx::query_as::<_, WriteRule>(sqlx::AssertSqlSafe(format!(
            "UPDATE write_rules SET \
                name = ?, enabled = ?, edge_mode = ?, cooldown_ms = ?, write_target_id = ?, \
                write_value_mode = ?, write_constant_value = ?, write_constant_text = ?, \
                write_source_tag_id = ? \
             WHERE id = ? RETURNING {COLUMNS}"
        )))
        .bind(input.name.trim())
        .bind(input.enabled)
        .bind(&input.edge_mode)
        .bind(input.cooldown_ms)
        .bind(input.write_target_id)
        .bind(&input.write_value_mode)
        .bind(normalized.constant_value)
        .bind(&normalized.constant_text)
        .bind(normalized.source_tag_id)
        .bind(id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|err| match err {
            sqlx::Error::RowNotFound => BantoError::NotFound {
                resource: RESOURCE.to_string(),
                id: id.to_string(),
            },
            other => map_write_error(other, "name", "writeTargetId", TARGET_FK_MESSAGE),
        })?;

        // Replace the whole condition set (child rows have no meaning apart
        // from their rule - `ON DELETE CASCADE` in 0007).
        sqlx::query("DELETE FROM write_rule_conditions WHERE write_rule_id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(banto_storage::storage_error)?;
        insert_conditions(&mut tx, id, &input.conditions).await?;
        tx.commit().await.map_err(banto_storage::storage_error)?;

        let conditions = self.conditions_for(rule.id).await?;
        Ok(WriteRuleDetail { rule, conditions })
    }

    pub async fn delete(&self, id: i64) -> Result<(), BantoError> {
        // Conditions cascade (0007); nothing references a rule by id in this
        // app's own schema (write_audit_log snapshots, not FK-references).
        let result = sqlx::query("DELETE FROM write_rules WHERE id = ?")
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

/// Normalized value-mode-dependent fields: only the columns that match the
/// selected `write_value_mode` are persisted; the others are forced to NULL
/// so a stale constant/source-tag never lingers after a mode switch. (Which
/// ONE of value/text is set in constant mode is enforced by validation
/// against the target's type - both cannot survive to here.)
struct Normalized {
    constant_value: Option<f64>,
    constant_text: Option<String>,
    source_tag_id: Option<i64>,
}

fn normalize(input: &WriteRuleInput) -> Normalized {
    match input.write_value_mode.as_str() {
        "constant" => Normalized {
            constant_value: input.write_constant_value,
            constant_text: input.write_constant_text.clone(),
            source_tag_id: None,
        },
        "copy_from_source" => Normalized {
            constant_value: None,
            constant_text: None,
            source_tag_id: input.write_source_tag_id,
        },
        // Unreachable for a validated input; keep all to be safe.
        _ => Normalized {
            constant_value: input.write_constant_value,
            constant_text: input.write_constant_text.clone(),
            source_tag_id: input.write_source_tag_id,
        },
    }
}

async fn insert_conditions(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    rule_id: i64,
    conditions: &[WriteRuleConditionInput],
) -> Result<(), BantoError> {
    for condition in conditions {
        // `between` is the only operator that keeps threshold_value_2.
        let upper = if condition.is_between() {
            condition.threshold_value_2
        } else {
            None
        };
        sqlx::query(
            "INSERT INTO write_rule_conditions \
                (write_rule_id, source_tag_id, operator, threshold_value, threshold_value_2, \
                 threshold_text) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(rule_id)
        .bind(condition.source_tag_id)
        .bind(&condition.operator)
        .bind(condition.threshold_value)
        .bind(upper)
        .bind(&condition.threshold_text)
        .execute(&mut **tx)
        .await
        .map_err(banto_storage::storage_error)?;
    }
    Ok(())
}

fn cycle_error(rule_name: &str, culprit: &str) -> BantoError {
    BantoError::Validation {
        field_errors: vec![FieldError {
            field: "enabled".to_string(),
            message: format!(
                "書き込みループを検出しました: ルール「{rule_name}」を有効にすると、ルール「{culprit}」との間で読み書きが循環します"
            ),
        }],
    }
}

/// Depth-first search for a path from `start` to `goal` over the device
/// graph, returning the rule names of the edges along one such path (used
/// only to name a culprit rule in the cycle error). `None` if `goal` is
/// unreachable from `start`.
fn find_path(
    adjacency: &HashMap<Device, Vec<(Device, String)>>,
    start: &Device,
    goal: &Device,
) -> Option<Vec<String>> {
    let mut visited: HashSet<Device> = HashSet::new();
    let mut stack: Vec<(Device, Vec<String>)> = vec![(start.clone(), Vec::new())];
    while let Some((device, rules)) = stack.pop() {
        if &device == goal {
            return Some(rules);
        }
        if !visited.insert(device.clone()) {
            continue;
        }
        if let Some(edges) = adjacency.get(&device) {
            for (next, rule_name) in edges {
                if next == goal {
                    let mut path = rules.clone();
                    path.push(rule_name.clone());
                    return Some(path);
                }
                if !visited.contains(next) {
                    let mut path = rules.clone();
                    path.push(rule_name.clone());
                    stack.push((next.clone(), path));
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::init_db_memory;
    use crate::write_targets::{WriteTargetInput, WriteTargetService};
    use banto_tags::{
        CollectionGroupInput, CollectionGroupService, PlcConnectionInput, PlcConnectionService,
        TagInput, TagService,
    };

    /// A fully wired test fixture: one PLC connection + one collection group,
    /// plus helpers to make source tags and write targets to reference.
    struct Fixture {
        pool: SqlitePool,
        rules: WriteRuleService,
        targets: WriteTargetService,
        tags: TagService,
        plc_id: i64,
        group_id: i64,
    }

    impl Fixture {
        async fn new() -> Self {
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
                    simulation: false,

                    word_order: "low_high".to_string(),
                })
                .await
                .unwrap();
            let groups = CollectionGroupService::new(pool.clone());
            let group = groups
                .create(CollectionGroupInput {
                    name: "G1".to_string(),
                    plc_connection_id: conn.id,
                    period_ms: 1000,
                    enabled: true,
                })
                .await
                .unwrap();
            Self {
                rules: WriteRuleService::new(pool.clone()),
                targets: WriteTargetService::new(pool.clone()),
                tags: TagService::new(pool.clone()),
                pool,
                plc_id: conn.id,
                group_id: group.id,
            }
        }

        /// A source tag at `address` on the fixture's single PLC.
        async fn tag(&self, name: &str, address: &str) -> i64 {
            self.tags
                .create(TagInput {
                    name: name.to_string(),
                    collection_group_id: self.group_id,
                    address: address.to_string(),
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
                    tag_kind: "plc".to_string(),
                    expression: None,
                    retain: false,
                    expected_revision: None,
                })
                .await
                .unwrap()
                .id
        }

        /// A write target at `address` on the fixture's single PLC.
        async fn target(&self, name: &str, address: &str) -> i64 {
            self.targets
                .create(WriteTargetInput {
                    name: name.to_string(),
                    plc_connection_id: self.plc_id,
                    address: address.to_string(),
                    data_type: "i16".to_string(),
                    string_length: None,
                    raw_lo: None,
                    raw_hi: None,
                    eng_lo: None,
                    eng_hi: None,
                    unit: None,
                    decimals: 0,
                    enabled: true,
                })
                .await
                .unwrap()
                .id
        }

        /// A STRING source tag (`length` words) at `address`.
        async fn string_tag(&self, name: &str, address: &str, length: i64) -> i64 {
            self.tags
                .create(TagInput {
                    name: name.to_string(),
                    collection_group_id: self.group_id,
                    address: address.to_string(),
                    data_type: "string".to_string(),
                    string_length: Some(length),
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
                    tag_kind: "plc".to_string(),
                    expression: None,
                    retain: false,
                    expected_revision: None,
                })
                .await
                .unwrap()
                .id
        }

        /// A STRING write target (`length` words) at `address`.
        async fn string_target(&self, name: &str, address: &str, length: i64) -> i64 {
            self.targets
                .create(WriteTargetInput {
                    name: name.to_string(),
                    plc_connection_id: self.plc_id,
                    address: address.to_string(),
                    data_type: "string".to_string(),
                    string_length: Some(length),
                    raw_lo: None,
                    raw_hi: None,
                    eng_lo: None,
                    eng_hi: None,
                    unit: None,
                    decimals: 0,
                    enabled: true,
                })
                .await
                .unwrap()
                .id
        }
    }

    fn rule_input(
        name: &str,
        enabled: bool,
        source_tag_id: i64,
        write_target_id: i64,
    ) -> WriteRuleInput {
        WriteRuleInput {
            name: name.to_string(),
            enabled,
            edge_mode: "rising".to_string(),
            cooldown_ms: None,
            write_target_id,
            write_value_mode: "constant".to_string(),
            write_constant_value: Some(1.0),
            write_constant_text: None,
            write_source_tag_id: None,
            conditions: vec![WriteRuleConditionInput {
                source_tag_id,
                operator: "gt".to_string(),
                threshold_value: Some(10.0),
                threshold_value_2: None,
                threshold_text: None,
            }],
        }
    }

    /// A rule whose single condition is `string source eq/neq text` and whose
    /// action writes `constant_text` to a (string) target.
    fn string_rule_input(
        name: &str,
        source_tag_id: i64,
        operator: &str,
        threshold_text: &str,
        write_target_id: i64,
        constant_text: &str,
    ) -> WriteRuleInput {
        WriteRuleInput {
            name: name.to_string(),
            enabled: false,
            edge_mode: "rising".to_string(),
            cooldown_ms: None,
            write_target_id,
            write_value_mode: "constant".to_string(),
            write_constant_value: None,
            write_constant_text: Some(constant_text.to_string()),
            write_source_tag_id: None,
            conditions: vec![WriteRuleConditionInput {
                source_tag_id,
                operator: operator.to_string(),
                threshold_value: None,
                threshold_value_2: None,
                threshold_text: Some(threshold_text.to_string()),
            }],
        }
    }

    // --- CRUD round trip ----------------------------------------------------

    #[tokio::test]
    async fn create_then_get_round_trips_with_conditions() {
        let f = Fixture::new().await;
        let tag = f.tag("SrcA", "D10").await;
        let target = f.target("TgtA", "D20").await;

        let created = f
            .rules
            .create(rule_input("R1", true, tag, target))
            .await
            .expect("create");
        assert_eq!(created.rule.name, "R1");
        assert!(created.rule.enabled);
        assert_eq!(created.conditions.len(), 1);
        assert_eq!(created.conditions[0].operator, "gt");
        assert_eq!(created.conditions[0].source_tag_id, tag);

        let fetched = f.rules.get(created.rule.id).await.expect("get");
        assert_eq!(fetched, created);
    }

    #[tokio::test]
    async fn update_replaces_conditions() {
        let f = Fixture::new().await;
        let tag_a = f.tag("SrcA", "D10").await;
        let tag_b = f.tag("SrcB", "D11").await;
        let target = f.target("TgtA", "D20").await;

        let created = f
            .rules
            .create(rule_input("R1", false, tag_a, target))
            .await
            .unwrap();

        let mut input = rule_input("R1", false, tag_b, target);
        input.conditions.push(WriteRuleConditionInput {
            source_tag_id: tag_a,
            operator: "between".to_string(),
            threshold_value: Some(0.0),
            threshold_value_2: Some(5.0),
            threshold_text: None,
        });
        let updated = f
            .rules
            .update(created.rule.id, input)
            .await
            .expect("update");
        assert_eq!(updated.conditions.len(), 2);
        // Old single condition is gone; the new set is exactly what we sent.
        assert_eq!(updated.conditions[0].source_tag_id, tag_b);
        assert_eq!(updated.conditions[1].operator, "between");
        assert_eq!(updated.conditions[1].threshold_value_2, Some(5.0));
    }

    #[tokio::test]
    async fn delete_cascades_conditions() {
        let f = Fixture::new().await;
        let tag = f.tag("SrcA", "D10").await;
        let target = f.target("TgtA", "D20").await;
        let created = f
            .rules
            .create(rule_input("R1", false, tag, target))
            .await
            .unwrap();

        f.rules.delete(created.rule.id).await.expect("delete");
        assert!(matches!(
            f.rules.get(created.rule.id).await.unwrap_err(),
            BantoError::NotFound { .. }
        ));
        let remaining: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM write_rule_conditions WHERE write_rule_id = ?",
        )
        .bind(created.rule.id)
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!(remaining, 0);
    }

    #[tokio::test]
    async fn update_missing_id_is_not_found() {
        let f = Fixture::new().await;
        let tag = f.tag("SrcA", "D10").await;
        let target = f.target("TgtA", "D20").await;
        let err = f
            .rules
            .update(999, rule_input("R1", false, tag, target))
            .await
            .unwrap_err();
        assert!(
            matches!(err, BantoError::NotFound { resource, id } if resource == "write_rules" && id == "999")
        );
    }

    // --- validation ---------------------------------------------------------

    #[tokio::test]
    async fn create_rejects_empty_conditions() {
        let f = Fixture::new().await;
        let target = f.target("TgtA", "D20").await;
        let mut input = rule_input("R1", false, 0, target);
        input.conditions.clear();
        match f.rules.create(input).await.unwrap_err() {
            BantoError::Validation { field_errors } => {
                assert!(field_errors.iter().any(|e| e.field == "conditions"));
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_rejects_unknown_edge_mode() {
        let f = Fixture::new().await;
        let tag = f.tag("SrcA", "D10").await;
        let target = f.target("TgtA", "D20").await;
        let mut input = rule_input("R1", false, tag, target);
        input.edge_mode = "sideways".to_string();
        match f.rules.create(input).await.unwrap_err() {
            BantoError::Validation { field_errors } => {
                assert!(field_errors.iter().any(|e| e.field == "edgeMode"));
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_rejects_constant_mode_without_value() {
        let f = Fixture::new().await;
        let tag = f.tag("SrcA", "D10").await;
        let target = f.target("TgtA", "D20").await;
        let mut input = rule_input("R1", false, tag, target);
        input.write_constant_value = None;
        match f.rules.create(input).await.unwrap_err() {
            BantoError::Validation { field_errors } => {
                assert!(field_errors.iter().any(|e| e.field == "writeConstantValue"));
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_copy_from_source_requires_existing_tag() {
        let f = Fixture::new().await;
        let tag = f.tag("SrcA", "D10").await;
        let target = f.target("TgtA", "D20").await;
        let mut input = rule_input("R1", false, tag, target);
        input.write_value_mode = "copy_from_source".to_string();
        input.write_constant_value = None;
        input.write_source_tag_id = Some(999);
        match f.rules.create(input).await.unwrap_err() {
            BantoError::Validation { field_errors } => {
                assert!(field_errors.iter().any(|e| e.field == "writeSourceTagId"));
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_rejects_missing_write_target() {
        let f = Fixture::new().await;
        let tag = f.tag("SrcA", "D10").await;
        match f
            .rules
            .create(rule_input("R1", false, tag, 999))
            .await
            .unwrap_err()
        {
            BantoError::Validation { field_errors } => {
                assert!(field_errors.iter().any(|e| e.field == "writeTargetId"));
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_rejects_missing_source_tag() {
        let f = Fixture::new().await;
        let target = f.target("TgtA", "D20").await;
        match f
            .rules
            .create(rule_input("R1", false, 999, target))
            .await
            .unwrap_err()
        {
            BantoError::Validation { field_errors } => {
                assert!(field_errors
                    .iter()
                    .any(|e| e.field == "conditions.0.sourceTagId"));
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_rejects_between_without_upper_bound() {
        let f = Fixture::new().await;
        let tag = f.tag("SrcA", "D10").await;
        let target = f.target("TgtA", "D20").await;
        let mut input = rule_input("R1", false, tag, target);
        input.conditions[0].operator = "between".to_string();
        input.conditions[0].threshold_value_2 = None;
        match f.rules.create(input).await.unwrap_err() {
            BantoError::Validation { field_errors } => {
                assert!(field_errors
                    .iter()
                    .any(|e| e.field == "conditions.0.thresholdValue2"));
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_rejects_duplicate_name() {
        let f = Fixture::new().await;
        let tag = f.tag("SrcA", "D10").await;
        let target = f.target("TgtA", "D20").await;
        f.rules
            .create(rule_input("Dup", false, tag, target))
            .await
            .unwrap();
        match f
            .rules
            .create(rule_input("Dup", false, tag, target))
            .await
            .unwrap_err()
        {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "name");
                assert_eq!(field_errors[0].message, "既に使用されています");
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    // --- cycle detection ----------------------------------------------------

    #[tokio::test]
    async fn no_cycle_between_independent_rules_is_allowed() {
        let f = Fixture::new().await;
        let x = f.tag("X", "D10").await;
        let y_tag = f.tag("Yr", "D11").await;
        let tgt_y = f.target("Y", "D20").await;
        let tgt_z = f.target("Z", "D21").await;

        // R1 reads X writes Y; R2 reads Y writes Z. No loop back to X.
        f.rules
            .create(rule_input("R1", true, x, tgt_y))
            .await
            .expect("R1");
        f.rules
            .create(rule_input("R2", true, y_tag, tgt_z))
            .await
            .expect("R2 should be allowed - no cycle");
    }

    #[tokio::test]
    async fn direct_two_rule_cycle_is_rejected() {
        let f = Fixture::new().await;
        // Device X: address D10 (both a source tag and a write target).
        let x_tag = f.tag("Xr", "D10").await;
        let y_tag = f.tag("Yr", "D20").await;
        let tgt_x = f.target("Xw", "D10").await;
        let tgt_y = f.target("Yw", "D20").await;

        // R1 reads X (D10) writes Y (D20).
        f.rules
            .create(rule_input("R1", true, x_tag, tgt_y))
            .await
            .expect("R1");
        // R2 reads Y (D20) writes X (D10) -> closes the loop.
        match f
            .rules
            .create(rule_input("R2", true, y_tag, tgt_x))
            .await
            .unwrap_err()
        {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "enabled");
                assert!(field_errors[0].message.contains("R1"));
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn three_rule_cycle_is_rejected() {
        let f = Fixture::new().await;
        let x_tag = f.tag("Xr", "D10").await;
        let y_tag = f.tag("Yr", "D20").await;
        let z_tag = f.tag("Zr", "D30").await;
        let tgt_x = f.target("Xw", "D10").await;
        let tgt_y = f.target("Yw", "D20").await;
        let tgt_z = f.target("Zw", "D30").await;

        f.rules
            .create(rule_input("R1", true, x_tag, tgt_y))
            .await
            .expect("R1: X->Y");
        f.rules
            .create(rule_input("R2", true, y_tag, tgt_z))
            .await
            .expect("R2: Y->Z");
        // R3 reads Z writes X -> closes X->Y->Z->X.
        match f
            .rules
            .create(rule_input("R3", true, z_tag, tgt_x))
            .await
            .unwrap_err()
        {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "enabled");
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn self_loop_is_rejected() {
        let f = Fixture::new().await;
        // Same device D10 both read and written by one rule.
        let x_tag = f.tag("Xr", "D10").await;
        let tgt_x = f.target("Xw", "D10").await;
        match f
            .rules
            .create(rule_input("Selfie", true, x_tag, tgt_x))
            .await
            .unwrap_err()
        {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "enabled");
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn cycle_only_among_enabled_rules_a_disabled_rule_in_the_path_does_not_count() {
        let f = Fixture::new().await;
        let x_tag = f.tag("Xr", "D10").await;
        let y_tag = f.tag("Yr", "D20").await;
        let tgt_x = f.target("Xw", "D10").await;
        let tgt_y = f.target("Yw", "D20").await;

        // R1 (DISABLED) reads X writes Y.
        f.rules
            .create(rule_input("R1", false, x_tag, tgt_y))
            .await
            .expect("R1 disabled");
        // R2 reads Y writes X: would cycle only if R1 counted, but R1 is off.
        f.rules
            .create(rule_input("R2", true, y_tag, tgt_x))
            .await
            .expect("R2 allowed since the closing rule R1 is disabled");
    }

    #[tokio::test]
    async fn enabling_the_second_rule_via_update_is_rejected() {
        let f = Fixture::new().await;
        let x_tag = f.tag("Xr", "D10").await;
        let y_tag = f.tag("Yr", "D20").await;
        let tgt_x = f.target("Xw", "D10").await;
        let tgt_y = f.target("Yw", "D20").await;

        f.rules
            .create(rule_input("R1", true, x_tag, tgt_y))
            .await
            .expect("R1");
        // Created disabled (no cycle yet), then enabling it via update closes
        // the loop and must be rejected.
        let r2 = f
            .rules
            .create(rule_input("R2", false, y_tag, tgt_x))
            .await
            .expect("R2 disabled ok");
        let mut input = rule_input("R2", true, y_tag, tgt_x);
        // keep same name
        input.name = "R2".to_string();
        match f.rules.update(r2.rule.id, input).await.unwrap_err() {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "enabled");
                assert!(field_errors[0].message.contains("R1"));
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn updating_an_enabled_rule_in_a_cycle_excludes_its_own_old_row() {
        // A rule already enabled and part of NO cycle can be edited (e.g.
        // its threshold) without its own OLD row being counted as a separate
        // conflicting rule.
        let f = Fixture::new().await;
        let x_tag = f.tag("Xr", "D10").await;
        let tgt_y = f.target("Yw", "D20").await;
        let created = f
            .rules
            .create(rule_input("R1", true, x_tag, tgt_y))
            .await
            .unwrap();

        let mut input = rule_input("R1", true, x_tag, tgt_y);
        input.conditions[0].threshold_value = Some(99.0);
        f.rules
            .update(created.rule.id, input)
            .await
            .expect("editing an enabled non-cyclic rule stays allowed");
    }

    #[tokio::test]
    async fn long_five_rule_chain_cycle_is_rejected() {
        // X→Y→Z→W→V, then V→X closes a five-hop loop: the DFS must find
        // cycles of arbitrary length, not just the 2/3-rule shapes.
        let f = Fixture::new().await;
        let devices = ["D10", "D20", "D30", "D40", "D50"];
        let mut tags = Vec::new();
        let mut targets = Vec::new();
        for (i, addr) in devices.iter().enumerate() {
            tags.push(f.tag(&format!("T{i}"), addr).await);
            targets.push(f.target(&format!("W{i}"), addr).await);
        }
        for i in 0..4 {
            f.rules
                .create(rule_input(&format!("R{i}"), true, tags[i], targets[i + 1]))
                .await
                .unwrap_or_else(|e| panic!("R{i} (link {i}->{}) should be allowed: {e:?}", i + 1));
        }
        // R4 reads V (D50) and writes X (D10) -> closes the loop.
        match f
            .rules
            .create(rule_input("R4", true, tags[4], targets[0]))
            .await
            .unwrap_err()
        {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "enabled");
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn any_one_of_several_conditions_can_close_a_cycle() {
        // A multi-condition rule contributes one edge PER condition source;
        // the cycle check must catch a loop through any of them, not just the
        // first.
        let f = Fixture::new().await;
        let x_tag = f.tag("Xr", "D10").await;
        let y_tag = f.tag("Yr", "D20").await;
        let unrelated = f.tag("Ur", "D90").await;
        let tgt_x = f.target("Xw", "D10").await;
        let tgt_y = f.target("Yw", "D20").await;

        f.rules
            .create(rule_input("R1", true, x_tag, tgt_y))
            .await
            .expect("R1: X->Y");

        // R2's FIRST condition is harmless; its SECOND reads Y and the target
        // writes X -> the loop closes through condition #2 only.
        let mut input = rule_input("R2", true, unrelated, tgt_x);
        input.conditions.push(WriteRuleConditionInput {
            source_tag_id: y_tag,
            operator: "gt".to_string(),
            threshold_value: Some(10.0),
            threshold_value_2: None,
            threshold_text: None,
        });
        match f.rules.create(input).await.unwrap_err() {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "enabled");
                assert!(field_errors[0].message.contains("R1"));
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn update_that_retargets_an_enabled_rule_into_a_cycle_is_rejected() {
        // Not just the enabled flag: an in-place edit that REDIRECTS an
        // enabled rule's target can also close a loop and must be rejected
        // (the update path re-runs the check against the NEW shape).
        let f = Fixture::new().await;
        let x_tag = f.tag("Xr", "D10").await;
        let y_tag = f.tag("Yr", "D20").await;
        let tgt_x = f.target("Xw", "D10").await;
        let tgt_y = f.target("Yw", "D20").await;
        let tgt_z = f.target("Zw", "D30").await;

        f.rules
            .create(rule_input("R1", true, x_tag, tgt_y))
            .await
            .expect("R1: X->Y");
        let r2 = f
            .rules
            .create(rule_input("R2", true, y_tag, tgt_z))
            .await
            .expect("R2: Y->Z, no cycle");

        // Retarget R2 from Z to X: Y->X + existing X->Y closes the loop.
        match f
            .rules
            .update(r2.rule.id, rule_input("R2", true, y_tag, tgt_x))
            .await
            .unwrap_err()
        {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "enabled");
                assert!(field_errors[0].message.contains("R1"));
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn same_address_on_a_different_connection_is_a_different_device() {
        // Device identity is (plc_connection_id, address): "D10" on PLC2 is
        // NOT the same node as "D10" on PLC1, so writing it closes no loop.
        let f = Fixture::new().await;
        let plc2 = PlcConnectionService::new(f.pool.clone())
            .create(PlcConnectionInput {
                name: "PLC2".to_string(),
                protocol: "modbus-tcp".to_string(),
                host: "10.0.0.2".to_string(),
                port: 502,
                unit_id: 1,
                enabled: true,
                simulation: false,

                word_order: "low_high".to_string(),
            })
            .await
            .unwrap();

        let x_tag = f.tag("Xr", "D10").await; // PLC1 D10
        let y_tag = f.tag("Yr", "D20").await; // PLC1 D20
        let tgt_y = f.target("Yw", "D20").await; // PLC1 D20
        let tgt_x2 = f
            .targets
            .create(WriteTargetInput {
                name: "Xw2".to_string(),
                plc_connection_id: plc2.id,
                address: "D10".to_string(), // same address, OTHER connection
                data_type: "i16".to_string(),
                string_length: None,
                raw_lo: None,
                raw_hi: None,
                eng_lo: None,
                eng_hi: None,
                unit: None,
                decimals: 0,
                enabled: true,
            })
            .await
            .unwrap()
            .id;

        f.rules
            .create(rule_input("R1", true, x_tag, tgt_y))
            .await
            .expect("R1: PLC1:D10 -> PLC1:D20");
        // R2 reads PLC1:D20 and writes PLC2:D10 - would be the two-rule cycle
        // if connections were ignored, but PLC2:D10 is a distinct node.
        f.rules
            .create(rule_input("R2", true, y_tag, tgt_x2))
            .await
            .expect(
                "R2 must be allowed - same address on another connection is a different device",
            );
    }

    #[tokio::test]
    async fn copy_from_source_value_channel_alone_is_not_a_feedback_edge() {
        // Pins the deliberate model choice documented on
        // `check_no_write_cycle`: copy_from_source's value source is not an
        // edge because it cannot cause a fire. Here values circulate A→B (R1)
        // and B→A (R2), but each rule triggers only on its own independent
        // condition device (C / D), so the loop cannot self-sustain and both
        // saves are allowed.
        let f = Fixture::new().await;
        let a_tag = f.tag("Ar", "D10").await;
        let b_tag = f.tag("Br", "D20").await;
        let c_tag = f.tag("Cr", "D40").await;
        let d_tag = f.tag("Dr", "D50").await;
        let tgt_a = f.target("Aw", "D10").await;
        let tgt_b = f.target("Bw", "D20").await;

        let copy_rule = |name: &str, cond_tag: i64, from_tag: i64, target: i64| WriteRuleInput {
            name: name.to_string(),
            enabled: true,
            edge_mode: "rising".to_string(),
            cooldown_ms: None,
            write_target_id: target,
            write_value_mode: "copy_from_source".to_string(),
            write_constant_value: None,
            write_constant_text: None,
            write_source_tag_id: Some(from_tag),
            conditions: vec![WriteRuleConditionInput {
                source_tag_id: cond_tag,
                operator: "gt".to_string(),
                threshold_value: Some(10.0),
                threshold_value_2: None,
                threshold_text: None,
            }],
        };

        f.rules
            .create(copy_rule("R1", c_tag, a_tag, tgt_b))
            .await
            .expect("R1: on C, copy A -> B");
        f.rules
            .create(copy_rule("R2", d_tag, b_tag, tgt_a))
            .await
            .expect("R2: on D, copy B -> A (copy-only loop is allowed by design)");
    }

    // --- S2 string rules ----------------------------------------------------

    /// Extract every violated field name from a Validation error.
    fn violated_fields(err: BantoError) -> Vec<String> {
        match err {
            BantoError::Validation { field_errors } => {
                field_errors.into_iter().map(|e| e.field).collect()
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn string_rule_round_trips_text_threshold_and_text_constant() {
        let f = Fixture::new().await;
        let src = f.string_tag("Sr", "D300", 4).await;
        let tgt = f.string_target("Sw", "D310", 4).await;

        let created = f
            .rules
            .create(string_rule_input("SR", src, "eq", "OK", tgt, "NG"))
            .await
            .expect("string rule should save");
        assert_eq!(created.rule.write_constant_value, None);
        assert_eq!(created.rule.write_constant_text.as_deref(), Some("NG"));
        assert_eq!(created.conditions.len(), 1);
        assert_eq!(created.conditions[0].operator, "eq");
        assert_eq!(created.conditions[0].threshold_value, None);
        assert_eq!(created.conditions[0].threshold_text.as_deref(), Some("OK"));

        let fetched = f.rules.get(created.rule.id).await.expect("get");
        assert_eq!(fetched, created);
    }

    #[tokio::test]
    async fn string_condition_rejects_ordering_operators() {
        let f = Fixture::new().await;
        let src = f.string_tag("Sr", "D300", 4).await;
        let tgt = f.string_target("Sw", "D310", 4).await;
        for op in ["gt", "gte", "lt", "lte", "between", "bit_is"] {
            let err = f
                .rules
                .create(string_rule_input(
                    &format!("SR-{op}"),
                    src,
                    op,
                    "OK",
                    tgt,
                    "NG",
                ))
                .await
                .unwrap_err();
            assert!(
                violated_fields(err).contains(&"conditions.0.operator".to_string()),
                "operator {op} must be rejected on a string source"
            );
        }
    }

    #[tokio::test]
    async fn string_condition_requires_text_and_rejects_numeric_threshold() {
        let f = Fixture::new().await;
        let src = f.string_tag("Sr", "D300", 4).await;
        let tgt = f.string_target("Sw", "D310", 4).await;

        // Missing text.
        let mut input = string_rule_input("SR1", src, "eq", "x", tgt, "NG");
        input.conditions[0].threshold_text = None;
        assert!(violated_fields(f.rules.create(input).await.unwrap_err())
            .contains(&"conditions.0.thresholdText".to_string()));

        // Empty text.
        let mut input = string_rule_input("SR2", src, "eq", "", tgt, "NG");
        input.conditions[0].threshold_text = Some(String::new());
        assert!(violated_fields(f.rules.create(input).await.unwrap_err())
            .contains(&"conditions.0.thresholdText".to_string()));

        // Numeric threshold on a string source.
        let mut input = string_rule_input("SR3", src, "eq", "OK", tgt, "NG");
        input.conditions[0].threshold_value = Some(1.0);
        assert!(violated_fields(f.rules.create(input).await.unwrap_err())
            .contains(&"conditions.0.thresholdValue".to_string()));
    }

    #[tokio::test]
    async fn string_condition_rejects_overlong_and_unencodable_text() {
        let f = Fixture::new().await;
        let src = f.string_tag("Sr", "D300", 2).await; // 2 words = 4 SJIS bytes
        let tgt = f.string_target("Sw", "D310", 4).await;

        // 5 ASCII bytes into a 4-byte budget.
        let err = f
            .rules
            .create(string_rule_input("SR1", src, "eq", "ABCDE", tgt, "NG"))
            .await
            .unwrap_err();
        assert!(violated_fields(err).contains(&"conditions.0.thresholdText".to_string()));

        // Not representable in Shift-JIS.
        let err = f
            .rules
            .create(string_rule_input("SR2", src, "eq", "😀", tgt, "NG"))
            .await
            .unwrap_err();
        assert!(violated_fields(err).contains(&"conditions.0.thresholdText".to_string()));
    }

    #[tokio::test]
    async fn numeric_condition_rejects_text_threshold() {
        let f = Fixture::new().await;
        let num = f.tag("Nr", "D10").await;
        let tgt = f.target("Nw", "D20").await;
        let mut input = rule_input("R1", false, num, tgt);
        input.conditions[0].threshold_text = Some("OK".to_string());
        assert!(violated_fields(f.rules.create(input).await.unwrap_err())
            .contains(&"conditions.0.thresholdText".to_string()));
    }

    #[tokio::test]
    async fn string_target_constant_requires_text_and_rejects_numeric_constant() {
        let f = Fixture::new().await;
        let src = f.string_tag("Sr", "D300", 4).await;
        let tgt = f.string_target("Sw", "D310", 2).await; // 2 words = 4 bytes

        // Numeric constant on a string target.
        let mut input = string_rule_input("SR1", src, "eq", "OK", tgt, "NG");
        input.write_constant_value = Some(1.0);
        assert!(violated_fields(f.rules.create(input).await.unwrap_err())
            .contains(&"writeConstantValue".to_string()));

        // Missing text constant.
        let mut input = string_rule_input("SR2", src, "eq", "OK", tgt, "NG");
        input.write_constant_text = None;
        assert!(violated_fields(f.rules.create(input).await.unwrap_err())
            .contains(&"writeConstantText".to_string()));

        // Over-long text constant vs the TARGET's budget (4 bytes).
        let err = f
            .rules
            .create(string_rule_input("SR3", src, "eq", "OK", tgt, "ABCDE"))
            .await
            .unwrap_err();
        assert!(violated_fields(err).contains(&"writeConstantText".to_string()));
    }

    #[tokio::test]
    async fn numeric_target_rejects_text_constant() {
        let f = Fixture::new().await;
        let num = f.tag("Nr", "D10").await;
        let tgt = f.target("Nw", "D20").await;
        let mut input = rule_input("R1", false, num, tgt);
        input.write_constant_text = Some("NG".to_string());
        assert!(violated_fields(f.rules.create(input).await.unwrap_err())
            .contains(&"writeConstantText".to_string()));
    }

    #[tokio::test]
    async fn copy_string_to_string_requires_target_at_least_source_length() {
        let f = Fixture::new().await;
        let cond = f.tag("Cr", "D40").await;
        let src4 = f.string_tag("Sr4", "D300", 4).await;
        let tgt2 = f.string_target("Sw2", "D310", 2).await;
        let tgt4 = f.string_target("Sw4", "D320", 4).await;

        let copy_rule = |name: &str, from: i64, target: i64| WriteRuleInput {
            name: name.to_string(),
            enabled: false,
            edge_mode: "rising".to_string(),
            cooldown_ms: None,
            write_target_id: target,
            write_value_mode: "copy_from_source".to_string(),
            write_constant_value: None,
            write_constant_text: None,
            write_source_tag_id: Some(from),
            conditions: vec![WriteRuleConditionInput {
                source_tag_id: cond,
                operator: "gt".to_string(),
                threshold_value: Some(10.0),
                threshold_value_2: None,
                threshold_text: None,
            }],
        };

        // Target shorter than source: rejected with the length message.
        match f
            .rules
            .create(copy_rule("C1", src4, tgt2))
            .await
            .unwrap_err()
        {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "writeSourceTagId");
                assert!(
                    field_errors[0].message.contains("2語"),
                    "message should name the lengths: {}",
                    field_errors[0].message
                );
            }
            other => panic!("expected Validation, got {other:?}"),
        }

        // Equal length: allowed.
        f.rules
            .create(copy_rule("C2", src4, tgt4))
            .await
            .expect("string→string copy with target length ≥ source length");
    }

    #[tokio::test]
    async fn copy_between_string_and_numeric_is_rejected_both_ways() {
        let f = Fixture::new().await;
        let cond = f.tag("Cr", "D40").await;
        let num_src = f.tag("Nr", "D10").await;
        let str_src = f.string_tag("Sr", "D300", 4).await;
        let num_tgt = f.target("Nw", "D20").await;
        let str_tgt = f.string_target("Sw", "D310", 4).await;

        let copy_rule = |name: &str, from: i64, target: i64| WriteRuleInput {
            name: name.to_string(),
            enabled: false,
            edge_mode: "rising".to_string(),
            cooldown_ms: None,
            write_target_id: target,
            write_value_mode: "copy_from_source".to_string(),
            write_constant_value: None,
            write_constant_text: None,
            write_source_tag_id: Some(from),
            conditions: vec![WriteRuleConditionInput {
                source_tag_id: cond,
                operator: "gt".to_string(),
                threshold_value: Some(10.0),
                threshold_value_2: None,
                threshold_text: None,
            }],
        };

        for (name, from, target) in [("S2N", str_src, num_tgt), ("N2S", num_src, str_tgt)] {
            let err = f
                .rules
                .create(copy_rule(name, from, target))
                .await
                .unwrap_err();
            assert!(
                violated_fields(err).contains(&"writeSourceTagId".to_string()),
                "{name}: string⇔numeric copy must be rejected"
            );
        }
    }
}
