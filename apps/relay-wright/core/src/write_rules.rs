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

use crate::support::{map_write_error, max_length_message, required_message};
use crate::write_rule_conditions::{
    validate_condition_input, WriteRuleCondition, WriteRuleConditionInput,
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
    pub write_source_tag_id: Option<i64>,
}

/// A rule plus its AND-combined conditions - the shape every read/write on
/// this aggregate returns.
#[derive(Debug, Clone, PartialEq, Serialize)]
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
    pub write_source_tag_id: Option<i64>,
    #[serde(default)]
    pub conditions: Vec<WriteRuleConditionInput>,
}

/// A PLC device identity: `(plc_connection_id, normalized address)`. Two rules
/// referencing the same device (whether one reads it and the other writes it)
/// meet at the same node of the write-loop graph.
type Device = (i64, String);

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
        .column("writeSourceTagId", "write_source_tag_id")
}

const RESOURCE: &str = "write_rules";
const COLUMNS: &str = "id, name, enabled, edge_mode, cooldown_ms, write_target_id, \
     write_value_mode, write_constant_value, write_source_tag_id";
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

    // --- existence checks (cross-lineage refs, no SQL FK) -------------------

    async fn write_target_exists(&self, id: i64) -> Result<bool, BantoError> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM write_targets WHERE id = ?")
            .bind(id)
            .fetch_one(&self.pool)
            .await
            .map_err(banto_storage::storage_error)?;
        Ok(count > 0)
    }

    async fn tag_exists(&self, id: i64) -> Result<bool, BantoError> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tags WHERE id = ?")
            .bind(id)
            .fetch_one(&self.pool)
            .await
            .map_err(banto_storage::storage_error)?;
        Ok(count > 0)
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

        // Write value mode + its dependent field.
        match input.write_value_mode.as_str() {
            "constant" => {
                if input.write_constant_value.is_none() {
                    errors.push(FieldError {
                        field: "writeConstantValue".to_string(),
                        message: "定数書き込みには書き込む値が必要です".to_string(),
                    });
                }
            }
            "copy_from_source" => match input.write_source_tag_id {
                None => errors.push(FieldError {
                    field: "writeSourceTagId".to_string(),
                    message: "ソース値のコピーには参照元タグが必要です".to_string(),
                }),
                Some(tag_id) => {
                    if !self.tag_exists(tag_id).await? {
                        errors.push(FieldError {
                            field: "writeSourceTagId".to_string(),
                            message: "指定された参照元タグが見つかりません".to_string(),
                        });
                    }
                }
            },
            _ => errors.push(FieldError {
                field: "writeValueMode".to_string(),
                message: format!(
                    "対応書き込みモードは {} のいずれかです",
                    ALLOWED_WRITE_VALUE_MODES.join(", ")
                ),
            }),
        }

        if !self.write_target_exists(input.write_target_id).await? {
            errors.push(FieldError {
                field: "writeTargetId".to_string(),
                message: TARGET_FK_MESSAGE.to_string(),
            });
        }

        // Conditions: at least one, each valid, each source tag existing.
        if input.conditions.is_empty() {
            errors.push(FieldError {
                field: "conditions".to_string(),
                message: "条件を1つ以上設定してください".to_string(),
            });
        }
        for (i, condition) in input.conditions.iter().enumerate() {
            errors.extend(validate_condition_input(condition, i));
            if !self.tag_exists(condition.source_tag_id).await? {
                errors.push(FieldError {
                    field: format!("conditions.{i}.sourceTagId"),
                    message: "指定されたソースタグが見つかりません".to_string(),
                });
            }
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
            let mut builder: QueryBuilder<'_, Sqlite> = QueryBuilder::new(
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

        let mut rows_builder: QueryBuilder<'_, Sqlite> =
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

        let mut count_builder: QueryBuilder<'_, Sqlite> =
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
            "SELECT id, write_rule_id, source_tag_id, operator, threshold_value, threshold_value_2 \
             FROM write_rule_conditions WHERE write_rule_id = ? ORDER BY id",
        )
        .bind(rule_id)
        .fetch_all(&self.pool)
        .await
        .map_err(banto_storage::storage_error)
    }

    pub async fn get(&self, id: i64) -> Result<WriteRuleDetail, BantoError> {
        let rule = sqlx::query_as::<_, WriteRule>(&format!(
            "SELECT {COLUMNS} FROM write_rules WHERE id = ?"
        ))
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

        let rule = sqlx::query_as::<_, WriteRule>(&format!(
            "INSERT INTO write_rules (\
                name, enabled, edge_mode, cooldown_ms, write_target_id, \
                write_value_mode, write_constant_value, write_source_tag_id\
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?) RETURNING {COLUMNS}"
        ))
        .bind(input.name.trim())
        .bind(input.enabled)
        .bind(&input.edge_mode)
        .bind(input.cooldown_ms)
        .bind(input.write_target_id)
        .bind(&input.write_value_mode)
        .bind(normalized.constant_value)
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

        let rule = sqlx::query_as::<_, WriteRule>(&format!(
            "UPDATE write_rules SET \
                name = ?, enabled = ?, edge_mode = ?, cooldown_ms = ?, write_target_id = ?, \
                write_value_mode = ?, write_constant_value = ?, write_source_tag_id = ? \
             WHERE id = ? RETURNING {COLUMNS}"
        ))
        .bind(input.name.trim())
        .bind(input.enabled)
        .bind(&input.edge_mode)
        .bind(input.cooldown_ms)
        .bind(input.write_target_id)
        .bind(&input.write_value_mode)
        .bind(normalized.constant_value)
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

/// Normalized value-mode-dependent fields: only the column that matches the
/// selected `write_value_mode` is persisted; the other is forced to NULL so a
/// stale constant/source-tag never lingers after a mode switch.
struct Normalized {
    constant_value: Option<f64>,
    source_tag_id: Option<i64>,
}

fn normalize(input: &WriteRuleInput) -> Normalized {
    match input.write_value_mode.as_str() {
        "constant" => Normalized {
            constant_value: input.write_constant_value,
            source_tag_id: None,
        },
        "copy_from_source" => Normalized {
            constant_value: None,
            source_tag_id: input.write_source_tag_id,
        },
        // Unreachable for a validated input; keep both to be safe.
        _ => Normalized {
            constant_value: input.write_constant_value,
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
                (write_rule_id, source_tag_id, operator, threshold_value, threshold_value_2) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(rule_id)
        .bind(condition.source_tag_id)
        .bind(&condition.operator)
        .bind(condition.threshold_value)
        .bind(upper)
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
            write_source_tag_id: None,
            conditions: vec![WriteRuleConditionInput {
                source_tag_id,
                operator: "gt".to_string(),
                threshold_value: 10.0,
                threshold_value_2: None,
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
            threshold_value: 0.0,
            threshold_value_2: Some(5.0),
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
        input.conditions[0].threshold_value = 99.0;
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
            threshold_value: 10.0,
            threshold_value_2: None,
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
            write_source_tag_id: Some(from_tag),
            conditions: vec![WriteRuleConditionInput {
                source_tag_id: cond_tag,
                operator: "gt".to_string(),
                threshold_value: 10.0,
                threshold_value_2: None,
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
}
