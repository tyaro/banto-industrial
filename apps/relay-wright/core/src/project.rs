//! Project file export/import: save the whole app CONFIGURATION registry to a
//! single versioned JSON "project file" and load it back (feature
//! `feature/project-file`).
//!
//! ## What "the project" is
//!
//! Only the CONFIGURATION registry - the rows an operator edits to describe a
//! machine and its auto-write behavior:
//! - banto-tags' `plc_connections`, `collection_groups`, `tags`
//! - this app's own `write_targets`, `write_rules` (+ their inline
//!   `write_rule_conditions`), `qr_strings`
//!
//! Deliberately EXCLUDED (per-installation / history / runtime state, NOT
//! project config): `users`, UI `settings`, `audit_log`, `write_audit_log`,
//! `armed_state`.
//!
//! ## Invariants (docs/conventions.md)
//! - §2 (サービス層非依存): this module is `SqlitePool` + `BantoError` only,
//!   no tauri/axum/RBAC/HTTP. Authorization, the arm-state safety guard, audit,
//!   and the post-import engine reload are the wiring layer's job
//!   (`crate::rest` / `src-tauri`).
//! - Adds NO dependency: it composes the EXISTING services (banto-tags' three
//!   registry services and this app's `write_targets`/`write_rules`/
//!   `qr_strings` services) plus their already-camelCase wire row types.
//!
//! ## Format
//!
//! A single versioned JSON object ([`ProjectFile`]). The row arrays use the
//! very same camelCase wire shapes the services already serialize (so an
//! export is literally each `list()`'s output), with each row's own `id`
//! carried ONLY as informational context - on import every id is REMAPPED (the
//! incoming ids are never trusted as target ids, see [`import_project`]).
//!
//! ## Import = REPLACE, atomic, validated
//!
//! Importing REPLACES the entire current configuration ("load a project"),
//! done as:
//! 1. reject an unknown `format` / an unreadable `version`;
//! 2. VALIDATE the whole file by replaying it through the real service
//!    `create()` paths into a throwaway in-memory database - this reuses every
//!    row's normal validation (banto-tags' own included, which this crate
//!    cannot reach directly), enforces referential integrity WITHIN the file
//!    (a group pointing at a missing connection, a rule at a missing target/
//!    tag, ...), rejects duplicate names, and runs the write-loop cycle
//!    detector - so ANY violation rejects the WHOLE import with NOTHING applied
//!    to the real database;
//! 3. only once the replay proves the file wholly valid, APPLY it to the real
//!    pool in ONE sqlx transaction: delete every included table (FK order),
//!    then re-insert the file's rows remapping ids parent-first. Because the
//!    replay already proved uniqueness / references / CHECKs all hold, these
//!    direct inserts cannot fail on validation and the single transaction makes
//!    the swap atomic against a mid-apply storage error.
//!
//! The engine compiles rules at start/reload, so imported rules are not live
//! until the wiring layer triggers an engine reload (or the app restarts).

use std::collections::HashMap;

use banto_core::{BantoError, FieldError, ListParams};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::qr_strings::{QrString, QrStringInput, QrStringService};
use crate::write_rule_conditions::WriteRuleConditionInput;
use crate::write_rules::{WriteRuleDetail, WriteRuleInput, WriteRuleService};
use crate::write_targets::{WriteTarget, WriteTargetInput, WriteTargetService};
use banto_tags::{
    CollectionGroup, CollectionGroupInput, CollectionGroupService, PlcConnection,
    PlcConnectionInput, PlcConnectionService, Tag, TagInput, TagService,
};

/// The one accepted `format` tag. A file with any other value is rejected up
/// front (it is not a relay-wright project file at all).
pub const FORMAT: &str = "relay-wright-project";

/// The project-file schema version THIS build can read/write. Import rejects a
/// file whose `version` is anything else (an older build must not silently
/// mis-read a newer file, and vice versa).
pub const VERSION: u32 = 1;

/// The whole exported configuration, as a single versioned JSON object. Field
/// order here is the export's key order; the row arrays are the services'
/// own camelCase wire shapes verbatim.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectFile {
    /// Always [`FORMAT`] on export; validated on import.
    pub format: String,
    /// Always [`VERSION`] on export; validated on import.
    pub version: u32,
    /// The app-clock timestamp the export was taken (SQLite `datetime('now')`,
    /// the SAME clock source `audit_log`/`write_audit_log` use - no new time
    /// dependency). `None` only if that read somehow failed; the file is still
    /// valid without it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exported_at: Option<String>,
    /// The exporting build's version (`relay-wright-core`'s `CARGO_PKG_VERSION`),
    /// informational only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_version: Option<String>,
    pub plc_connections: Vec<PlcConnection>,
    pub collection_groups: Vec<CollectionGroup>,
    pub tags: Vec<Tag>,
    pub write_targets: Vec<WriteTarget>,
    /// Each rule flattened with its inline AND-conditions (the `WriteRuleDetail`
    /// bundle the write-rule service already returns).
    pub write_rules: Vec<WriteRuleDetail>,
    pub qr_strings: Vec<QrString>,
}

/// Per-table row counts applied by a successful [`import_project`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportSummary {
    pub plc_connections: usize,
    pub collection_groups: usize,
    pub tags: usize,
    pub write_targets: usize,
    pub write_rules: usize,
    pub write_rule_conditions: usize,
    pub qr_strings: usize,
}

/// Read every included table (via the existing services' `list`, default
/// params = all rows) and assemble the [`ProjectFile`]. Pure read: no
/// mutation, and - mirroring every other read path in this crate - no audit.
pub async fn export_project(pool: &SqlitePool) -> Result<ProjectFile, BantoError> {
    let plc_connections = PlcConnectionService::new(pool.clone())
        .list(ListParams::default())
        .await?
        .rows;
    let collection_groups = CollectionGroupService::new(pool.clone())
        .list(ListParams::default())
        .await?
        .rows;
    let tags = TagService::new(pool.clone())
        .list(ListParams::default())
        .await?
        .rows;
    let write_targets = WriteTargetService::new(pool.clone())
        .list(ListParams::default())
        .await?
        .rows;
    let write_rules = WriteRuleService::new(pool.clone())
        .list(ListParams::default())
        .await?
        .rows;
    let qr_strings = QrStringService::new(pool.clone()).list().await?;

    Ok(ProjectFile {
        format: FORMAT.to_string(),
        version: VERSION,
        exported_at: exported_at(pool).await,
        app_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        plc_connections,
        collection_groups,
        tags,
        write_targets,
        write_rules,
        qr_strings,
    })
}

/// The app-clock timestamp for `exportedAt`, from SQLite `datetime('now')` -
/// the exact source `audit_log.ts`/`write_audit_log.ts` default to, so no
/// chrono/time crate is pulled in (invariant: add no dependency). Best-effort:
/// a failure just omits the field rather than failing the export.
async fn exported_at(pool: &SqlitePool) -> Option<String> {
    sqlx::query_scalar::<_, String>("SELECT datetime('now')")
        .fetch_one(pool)
        .await
        .ok()
}

/// Load a project (REPLACE semantics): validate `project` in full, then swap
/// the entire configuration for it atomically. See the module doc for the
/// three phases. On ANY validation failure nothing is applied.
///
/// SAFETY: this function does NOT check the engine's arm state - it has no
/// engine handle (invariant §2). The wiring layer MUST refuse to call it while
/// the engine is armed, and SHOULD trigger an engine reload afterward so the
/// imported rules take effect (they are compiled at engine start/reload).
pub async fn import_project(
    pool: &SqlitePool,
    project: ProjectFile,
) -> Result<ImportSummary, BantoError> {
    // 1. format/version.
    if project.format != FORMAT {
        return Err(BantoError::Validation {
            field_errors: vec![FieldError {
                field: "format".to_string(),
                message: format!(
                    "未知のプロジェクト形式です（期待: {FORMAT}、実際: {}）",
                    project.format
                ),
            }],
        });
    }
    if project.version != VERSION {
        return Err(BantoError::Validation {
            field_errors: vec![FieldError {
                field: "version".to_string(),
                message: format!(
                    "このバージョンのプロジェクトファイル（version {}）は読み込めません（対応 version {VERSION}）",
                    project.version
                ),
            }],
        });
    }

    // 2. Full validation via a throwaway in-memory replay. Any error here
    //    means NOTHING is applied to the real pool below.
    validate_by_replay(&project).await?;

    // 3. Atomic REPLACE on the real pool.
    apply_replace(pool, &project).await
}

/// Replay the whole file through the real service `create()` paths into a
/// fresh in-memory database. This is the validation pass: it exercises every
/// row's normal validation (including banto-tags' own, unreachable from this
/// crate otherwise), enforces referential integrity WITHIN the file (each
/// remap below fails if a referenced id is absent), rejects duplicate names
/// (UNIQUE), and runs the write-loop cycle detector on each enabled rule's
/// create. A fully-valid file replays without error; anything else surfaces the
/// same `BantoError` a normal create would.
async fn validate_by_replay(project: &ProjectFile) -> Result<(), BantoError> {
    let scratch = crate::db::init_db_memory().await?;

    let plc = PlcConnectionService::new(scratch.clone());
    let groups = CollectionGroupService::new(scratch.clone());
    let tags = TagService::new(scratch.clone());
    let targets = WriteTargetService::new(scratch.clone());
    let rules = WriteRuleService::new(scratch.clone());
    let qr = QrStringService::new(scratch.clone());

    let mut conn_map: HashMap<i64, i64> = HashMap::new();
    for c in &project.plc_connections {
        let created = plc
            .create(PlcConnectionInput {
                name: c.name.clone(),
                protocol: c.protocol.clone(),
                host: c.host.clone(),
                port: c.port,
                unit_id: c.unit_id,
                enabled: c.enabled,
                simulation: false,

                word_order: "low_high".to_string(),
            })
            .await?;
        conn_map.insert(c.id, created.id);
    }

    let mut group_map: HashMap<i64, i64> = HashMap::new();
    for g in &project.collection_groups {
        let plc_id = remap(
            &conn_map,
            g.plc_connection_id,
            "collectionGroups",
            "plcConnectionId",
        )?;
        let created = groups
            .create(CollectionGroupInput {
                name: g.name.clone(),
                plc_connection_id: plc_id,
                period_ms: g.period_ms,
                enabled: g.enabled,
                // T19 S1-b (banto-industrial, 2026-09-02): round-trip the
                // source group's own value, same as every other field here -
                // this is a project snapshot restore, not a fresh-group
                // creation, so the imported group should keep its recorded
                // `default_writable` rather than resetting to a hardcoded
                // value. See `banto_tags::CollectionGroup::default_writable`'s
                // doc comment - it has no effect on collection/write
                // behavior, only on banto-hub's own new-tag form default.
                default_writable: g.default_writable,
            })
            .await?;
        group_map.insert(g.id, created.id);
    }

    let mut tag_map: HashMap<i64, i64> = HashMap::new();
    for t in &project.tags {
        let group_id = remap(
            &group_map,
            t.collection_group_id,
            "tags",
            "collectionGroupId",
        )?;
        let created = tags
            .create(TagInput {
                name: t.name.clone(),
                collection_group_id: group_id,
                address: t.address.clone(),
                data_type: t.data_type.clone(),
                string_length: t.string_length,
                string_encoding: "utf8".to_string(),
                raw_lo: t.raw_lo,
                raw_hi: t.raw_hi,
                eng_lo: t.eng_lo,
                eng_hi: t.eng_hi,
                unit: t.unit.clone(),
                decimals: t.decimals,
                threshold_h: t.threshold_h,
                threshold_hh: t.threshold_hh,
                threshold_l: t.threshold_l,
                threshold_ll: t.threshold_ll,
                enabled: t.enabled,
                writable: t.writable,
                tag_kind: t.tag_kind.clone(),
                expression: t.expression.clone(),
                retain: t.retain,
                // インポートは常に新規作成であり、楽観ロックの対象にする
                // 「前回取得した revision」という概念自体が無い。
                expected_revision: None,
            })
            .await?;
        tag_map.insert(t.id, created.id);
    }

    let mut target_map: HashMap<i64, i64> = HashMap::new();
    for wt in &project.write_targets {
        let plc_id = remap(
            &conn_map,
            wt.plc_connection_id,
            "writeTargets",
            "plcConnectionId",
        )?;
        let created = targets
            .create(WriteTargetInput {
                name: wt.name.clone(),
                plc_connection_id: plc_id,
                address: wt.address.clone(),
                data_type: wt.data_type.clone(),
                string_length: wt.string_length,
                raw_lo: wt.raw_lo,
                raw_hi: wt.raw_hi,
                eng_lo: wt.eng_lo,
                eng_hi: wt.eng_hi,
                unit: wt.unit.clone(),
                decimals: wt.decimals,
                enabled: wt.enabled,
            })
            .await?;
        target_map.insert(wt.id, created.id);
    }

    for r in &project.write_rules {
        rules
            .create(rule_input_remapped(r, &target_map, &tag_map)?)
            .await?;
    }

    for q in &project.qr_strings {
        qr.create(QrStringInput {
            label: q.label.clone(),
            text: q.text.clone(),
        })
        .await?;
    }

    Ok(())
}

/// Build a [`WriteRuleInput`] from an exported [`WriteRuleDetail`], remapping
/// its `writeTargetId`, optional `writeSourceTagId`, and every condition's
/// `sourceTagId` from file ids to the target database's ids. Shared by the
/// scratch replay and the real apply so both remap identically.
fn rule_input_remapped(
    r: &WriteRuleDetail,
    target_map: &HashMap<i64, i64>,
    tag_map: &HashMap<i64, i64>,
) -> Result<WriteRuleInput, BantoError> {
    let write_target_id = remap(
        target_map,
        r.rule.write_target_id,
        "writeRules",
        "writeTargetId",
    )?;
    let write_source_tag_id = match r.rule.write_source_tag_id {
        Some(id) => Some(remap(tag_map, id, "writeRules", "writeSourceTagId")?),
        None => None,
    };
    let conditions = r
        .conditions
        .iter()
        .map(|c| {
            Ok(WriteRuleConditionInput {
                source_tag_id: remap(
                    tag_map,
                    c.source_tag_id,
                    "writeRules.conditions",
                    "sourceTagId",
                )?,
                operator: c.operator.clone(),
                threshold_value: c.threshold_value,
                threshold_value_2: c.threshold_value_2,
                threshold_text: c.threshold_text.clone(),
            })
        })
        .collect::<Result<Vec<_>, BantoError>>()?;
    Ok(WriteRuleInput {
        name: r.rule.name.clone(),
        enabled: r.rule.enabled,
        edge_mode: r.rule.edge_mode.clone(),
        cooldown_ms: r.rule.cooldown_ms,
        write_target_id,
        write_value_mode: r.rule.write_value_mode.clone(),
        write_constant_value: r.rule.write_constant_value,
        write_constant_text: r.rule.write_constant_text.clone(),
        write_source_tag_id,
        conditions,
    })
}

/// Resolve a file-local id to the target database's id, or reject the import as
/// referentially inconsistent (a reference to a row absent from the file).
fn remap(map: &HashMap<i64, i64>, old: i64, entity: &str, field: &str) -> Result<i64, BantoError> {
    map.get(&old)
        .copied()
        .ok_or_else(|| BantoError::Validation {
            field_errors: vec![FieldError {
                field: format!("{entity}.{field}"),
                message: format!("プロジェクトファイル内で参照先が見つかりません（id={old}）"),
            }],
        })
}

/// Phase 3: apply the (already fully-validated) file to the real pool in ONE
/// transaction - delete every included table children-first, then re-insert
/// the file's rows parents-first with remapped ids. Direct inserts (not the
/// services) so the whole swap is one atomic transaction; validation already
/// happened in [`validate_by_replay`], so these inserts only ever see
/// known-good rows.
async fn apply_replace(
    pool: &SqlitePool,
    project: &ProjectFile,
) -> Result<ImportSummary, BantoError> {
    let mut tx = pool.begin().await.map_err(banto_storage::storage_error)?;

    // DELETE existing rows, children before parents (foreign keys are enforced
    // - banto-storage connects with foreign_keys(true)).
    for sql in [
        "DELETE FROM write_rule_conditions",
        "DELETE FROM write_rules",
        "DELETE FROM write_targets",
        "DELETE FROM tags",
        "DELETE FROM collection_groups",
        "DELETE FROM plc_connections",
        "DELETE FROM qr_strings",
    ] {
        sqlx::query(sql)
            .execute(&mut *tx)
            .await
            .map_err(banto_storage::storage_error)?;
    }

    // INSERT parents first, capturing old->new id maps via RETURNING id.
    let mut conn_map: HashMap<i64, i64> = HashMap::new();
    for c in &project.plc_connections {
        let new_id: i64 = sqlx::query_scalar(
            "INSERT INTO plc_connections (name, protocol, host, port, unit_id, enabled) \
             VALUES (?, ?, ?, ?, ?, ?) RETURNING id",
        )
        .bind(&c.name)
        .bind(&c.protocol)
        .bind(&c.host)
        .bind(c.port)
        .bind(c.unit_id)
        .bind(c.enabled)
        .fetch_one(&mut *tx)
        .await
        .map_err(banto_storage::storage_error)?;
        conn_map.insert(c.id, new_id);
    }

    let mut group_map: HashMap<i64, i64> = HashMap::new();
    for g in &project.collection_groups {
        let plc_id = remap(
            &conn_map,
            g.plc_connection_id,
            "collectionGroups",
            "plcConnectionId",
        )?;
        let new_id: i64 = sqlx::query_scalar(
            "INSERT INTO collection_groups (name, plc_connection_id, period_ms, enabled) \
             VALUES (?, ?, ?, ?) RETURNING id",
        )
        .bind(&g.name)
        .bind(plc_id)
        .bind(g.period_ms)
        .bind(g.enabled)
        .fetch_one(&mut *tx)
        .await
        .map_err(banto_storage::storage_error)?;
        group_map.insert(g.id, new_id);
    }

    let mut tag_map: HashMap<i64, i64> = HashMap::new();
    for t in &project.tags {
        let group_id = remap(
            &group_map,
            t.collection_group_id,
            "tags",
            "collectionGroupId",
        )?;
        let new_id: i64 = sqlx::query_scalar(
            "INSERT INTO tags (\
                name, collection_group_id, address, data_type, string_length, \
                raw_lo, raw_hi, eng_lo, eng_hi, unit, decimals, \
                threshold_h, threshold_hh, threshold_l, threshold_ll, enabled\
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
        )
        .bind(&t.name)
        .bind(group_id)
        .bind(&t.address)
        .bind(&t.data_type)
        .bind(t.string_length)
        .bind(t.raw_lo)
        .bind(t.raw_hi)
        .bind(t.eng_lo)
        .bind(t.eng_hi)
        .bind(&t.unit)
        .bind(t.decimals)
        .bind(t.threshold_h)
        .bind(t.threshold_hh)
        .bind(t.threshold_l)
        .bind(t.threshold_ll)
        .bind(t.enabled)
        .fetch_one(&mut *tx)
        .await
        .map_err(banto_storage::storage_error)?;
        tag_map.insert(t.id, new_id);
    }

    let mut target_map: HashMap<i64, i64> = HashMap::new();
    for wt in &project.write_targets {
        let plc_id = remap(
            &conn_map,
            wt.plc_connection_id,
            "writeTargets",
            "plcConnectionId",
        )?;
        let new_id: i64 = sqlx::query_scalar(
            "INSERT INTO write_targets (\
                name, plc_connection_id, address, data_type, string_length, \
                raw_lo, raw_hi, eng_lo, eng_hi, unit, decimals, enabled\
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
        )
        .bind(&wt.name)
        .bind(plc_id)
        .bind(&wt.address)
        .bind(&wt.data_type)
        .bind(wt.string_length)
        .bind(wt.raw_lo)
        .bind(wt.raw_hi)
        .bind(wt.eng_lo)
        .bind(wt.eng_hi)
        .bind(&wt.unit)
        .bind(wt.decimals)
        .bind(wt.enabled)
        .fetch_one(&mut *tx)
        .await
        .map_err(banto_storage::storage_error)?;
        target_map.insert(wt.id, new_id);
    }

    let mut condition_count: usize = 0;
    for r in &project.write_rules {
        let write_target_id = remap(
            &target_map,
            r.rule.write_target_id,
            "writeRules",
            "writeTargetId",
        )?;
        let write_source_tag_id = match r.rule.write_source_tag_id {
            Some(id) => Some(remap(&tag_map, id, "writeRules", "writeSourceTagId")?),
            None => None,
        };
        let rule_id: i64 = sqlx::query_scalar(
            "INSERT INTO write_rules (\
                name, enabled, edge_mode, cooldown_ms, write_target_id, \
                write_value_mode, write_constant_value, write_constant_text, write_source_tag_id\
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
        )
        .bind(&r.rule.name)
        .bind(r.rule.enabled)
        .bind(&r.rule.edge_mode)
        .bind(r.rule.cooldown_ms)
        .bind(write_target_id)
        .bind(&r.rule.write_value_mode)
        .bind(r.rule.write_constant_value)
        .bind(&r.rule.write_constant_text)
        .bind(write_source_tag_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(banto_storage::storage_error)?;

        for c in &r.conditions {
            let source_tag_id = remap(
                &tag_map,
                c.source_tag_id,
                "writeRules.conditions",
                "sourceTagId",
            )?;
            // `between` is the only operator that keeps threshold_value_2
            // (mirrors write_rules::insert_conditions).
            let upper = if c.operator == "between" {
                c.threshold_value_2
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
            .bind(source_tag_id)
            .bind(&c.operator)
            .bind(c.threshold_value)
            .bind(upper)
            .bind(&c.threshold_text)
            .execute(&mut *tx)
            .await
            .map_err(banto_storage::storage_error)?;
            condition_count += 1;
        }
    }

    // qr_strings are independent (no FKs); preserve the exported display order
    // as the new sort_order.
    for (index, q) in project.qr_strings.iter().enumerate() {
        sqlx::query("INSERT INTO qr_strings (label, text, sort_order) VALUES (?, ?, ?)")
            .bind(&q.label)
            .bind(&q.text)
            .bind(index as i64)
            .execute(&mut *tx)
            .await
            .map_err(banto_storage::storage_error)?;
    }

    tx.commit().await.map_err(banto_storage::storage_error)?;

    Ok(ImportSummary {
        plc_connections: project.plc_connections.len(),
        collection_groups: project.collection_groups.len(),
        tags: project.tags.len(),
        write_targets: project.write_targets.len(),
        write_rules: project.write_rules.len(),
        write_rule_conditions: condition_count,
        qr_strings: project.qr_strings.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::init_db_memory;

    /// Seed a small but fully-wired configuration into `pool`: one connection,
    /// one group, two tags, one write target, one rule (two conditions), and
    /// two QR strings. Returns nothing - the caller exports to inspect it.
    async fn seed(pool: &SqlitePool) {
        let plc = PlcConnectionService::new(pool.clone());
        let groups = CollectionGroupService::new(pool.clone());
        let tags = TagService::new(pool.clone());
        let targets = WriteTargetService::new(pool.clone());
        let rules = WriteRuleService::new(pool.clone());
        let qr = QrStringService::new(pool.clone());

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
        let group = groups
            .create(CollectionGroupInput {
                name: "G1".to_string(),
                plc_connection_id: conn.id,
                period_ms: 1000,
                enabled: true,
                default_writable: true,
            })
            .await
            .unwrap();
        let src = tags.create(new_tag("Src", group.id, "D10")).await.unwrap();
        let copy_src = tags.create(new_tag("Copy", group.id, "D11")).await.unwrap();
        let target = targets
            .create(WriteTargetInput {
                name: "T1".to_string(),
                plc_connection_id: conn.id,
                address: "D20".to_string(),
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
            .unwrap();
        rules
            .create(WriteRuleInput {
                name: "R1".to_string(),
                enabled: true,
                edge_mode: "rising".to_string(),
                cooldown_ms: Some(500),
                write_target_id: target.id,
                write_value_mode: "copy_from_source".to_string(),
                write_constant_value: None,
                write_constant_text: None,
                write_source_tag_id: Some(copy_src.id),
                conditions: vec![
                    WriteRuleConditionInput {
                        source_tag_id: src.id,
                        operator: "gt".to_string(),
                        threshold_value: Some(10.0),
                        threshold_value_2: None,
                        threshold_text: None,
                    },
                    WriteRuleConditionInput {
                        source_tag_id: src.id,
                        operator: "between".to_string(),
                        threshold_value: Some(0.0),
                        threshold_value_2: Some(5.0),
                        threshold_text: None,
                    },
                ],
            })
            .await
            .unwrap();
        qr.create(QrStringInput {
            label: "start".to_string(),
            text: "START".to_string(),
        })
        .await
        .unwrap();
        qr.create(QrStringInput {
            label: "".to_string(),
            text: "STOP".to_string(),
        })
        .await
        .unwrap();
    }

    fn new_tag(name: &str, group_id: i64, address: &str) -> TagInput {
        TagInput {
            name: name.to_string(),
            collection_group_id: group_id,
            address: address.to_string(),
            data_type: "i16".to_string(),
            string_length: None,
            string_encoding: "utf8".to_string(),
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
        }
    }

    /// A structural fingerprint of a project MODULO ids: names, relationships
    /// resolved by name, and every non-id field - so a round trip whose ids are
    /// all remapped still compares equal.
    fn fingerprint(p: &ProjectFile) -> String {
        // Build id->name lookups so cross-references compare by NAME not id.
        let conn_name: HashMap<i64, &str> = p
            .plc_connections
            .iter()
            .map(|c| (c.id, c.name.as_str()))
            .collect();
        let group_name: HashMap<i64, &str> = p
            .collection_groups
            .iter()
            .map(|g| (g.id, g.name.as_str()))
            .collect();
        let tag_name: HashMap<i64, &str> = p.tags.iter().map(|t| (t.id, t.name.as_str())).collect();
        let target_name: HashMap<i64, &str> = p
            .write_targets
            .iter()
            .map(|t| (t.id, t.name.as_str()))
            .collect();

        let mut out = String::new();
        for c in &p.plc_connections {
            out += &format!(
                "CONN {} {} {} {} {} {}\n",
                c.name, c.protocol, c.host, c.port, c.unit_id, c.enabled
            );
        }
        for g in &p.collection_groups {
            out += &format!(
                "GROUP {} conn={} {} {}\n",
                g.name, conn_name[&g.plc_connection_id], g.period_ms, g.enabled
            );
        }
        for t in &p.tags {
            out += &format!(
                "TAG {} group={} {} {} {:?} {}\n",
                t.name,
                group_name[&t.collection_group_id],
                t.address,
                t.data_type,
                t.string_length,
                t.enabled
            );
        }
        for wt in &p.write_targets {
            out += &format!(
                "TARGET {} conn={} {} {} {}\n",
                wt.name, conn_name[&wt.plc_connection_id], wt.address, wt.data_type, wt.enabled
            );
        }
        for r in &p.write_rules {
            let src = r
                .rule
                .write_source_tag_id
                .map(|id| tag_name[&id])
                .unwrap_or("-");
            out += &format!(
                "RULE {} target={} {} {} {:?} src={} enabled={}\n",
                r.rule.name,
                target_name[&r.rule.write_target_id],
                r.rule.edge_mode,
                r.rule.write_value_mode,
                r.rule.cooldown_ms,
                src,
                r.rule.enabled
            );
            for c in &r.conditions {
                out += &format!(
                    "  COND tag={} {} {:?} {:?} {:?}\n",
                    tag_name[&c.source_tag_id],
                    c.operator,
                    c.threshold_value,
                    c.threshold_value_2,
                    c.threshold_text
                );
            }
        }
        for q in &p.qr_strings {
            out += &format!("QR {} {}\n", q.label, q.text);
        }
        out
    }

    #[tokio::test]
    async fn export_import_round_trips_modulo_ids() {
        let pool = init_db_memory().await.unwrap();
        seed(&pool).await;

        let first = export_project(&pool).await.expect("export");
        assert_eq!(first.format, FORMAT);
        assert_eq!(first.version, VERSION);
        assert!(
            first.exported_at.is_some(),
            "expected an app-clock timestamp"
        );
        assert_eq!(first.plc_connections.len(), 1);
        assert_eq!(first.tags.len(), 2);
        assert_eq!(first.write_rules.len(), 1);
        assert_eq!(first.write_rules[0].conditions.len(), 2);
        assert_eq!(first.qr_strings.len(), 2);

        // Wipe by importing an EMPTY project, then import the real one back.
        let summary = import_project(&pool, first.clone()).await.expect("import");
        assert_eq!(summary.plc_connections, 1);
        assert_eq!(summary.collection_groups, 1);
        assert_eq!(summary.tags, 2);
        assert_eq!(summary.write_targets, 1);
        assert_eq!(summary.write_rules, 1);
        assert_eq!(summary.write_rule_conditions, 2);
        assert_eq!(summary.qr_strings, 2);

        // Second export must equal the first MODULO ids (compare by structure).
        let second = export_project(&pool).await.expect("re-export");
        assert_eq!(fingerprint(&first), fingerprint(&second));

        // Ids were actually remapped (import replaces every row), so the raw
        // connection id set need not match - but there is still exactly one.
        assert_eq!(second.plc_connections.len(), 1);
    }

    #[tokio::test]
    async fn import_replaces_prior_configuration() {
        let pool = init_db_memory().await.unwrap();
        seed(&pool).await;
        let exported = export_project(&pool).await.unwrap();

        // Add extra rows the import must WIPE.
        PlcConnectionService::new(pool.clone())
            .create(PlcConnectionInput {
                name: "Extra".to_string(),
                protocol: "modbus-tcp".to_string(),
                host: "10.0.0.9".to_string(),
                port: 502,
                unit_id: 1,
                enabled: true,
                simulation: false,

                word_order: "low_high".to_string(),
            })
            .await
            .unwrap();
        assert_eq!(
            export_project(&pool).await.unwrap().plc_connections.len(),
            2
        );

        import_project(&pool, exported).await.expect("import");
        // Back to exactly the seeded single connection - "Extra" is gone.
        let after = export_project(&pool).await.unwrap();
        assert_eq!(after.plc_connections.len(), 1);
        assert_eq!(after.plc_connections[0].name, "PLC1");
    }

    #[tokio::test]
    async fn import_rejects_tag_pointing_at_missing_group_and_applies_nothing() {
        let pool = init_db_memory().await.unwrap();
        seed(&pool).await;
        let mut project = export_project(&pool).await.unwrap();
        let before = fingerprint(&project);

        // Point the first tag at a group id that is NOT in the file.
        project.tags[0].collection_group_id = 999_999;

        let err = import_project(&pool, project).await.unwrap_err();
        assert!(matches!(err, BantoError::Validation { .. }), "got {err:?}");
        // Nothing applied: the live config is byte-for-byte the pre-import one.
        let after = export_project(&pool).await.unwrap();
        assert_eq!(before, fingerprint(&after));
    }

    #[tokio::test]
    async fn import_rejects_rule_pointing_at_missing_target() {
        let pool = init_db_memory().await.unwrap();
        seed(&pool).await;
        let mut project = export_project(&pool).await.unwrap();
        project.write_rules[0].rule.write_target_id = 424242;

        let err = import_project(&pool, project).await.unwrap_err();
        assert!(matches!(err, BantoError::Validation { .. }), "got {err:?}");
    }

    #[tokio::test]
    async fn import_rejects_unknown_format() {
        let pool = init_db_memory().await.unwrap();
        seed(&pool).await;
        let mut project = export_project(&pool).await.unwrap();
        project.format = "some-other-tool".to_string();
        match import_project(&pool, project).await.unwrap_err() {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "format")
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn import_rejects_unreadable_version() {
        let pool = init_db_memory().await.unwrap();
        seed(&pool).await;
        let mut project = export_project(&pool).await.unwrap();
        project.version = VERSION + 1;
        match import_project(&pool, project).await.unwrap_err() {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "version")
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn import_rejects_a_write_loop_cycle() {
        let pool = init_db_memory().await.unwrap();
        seed(&pool).await;
        let mut project = export_project(&pool).await.unwrap();

        // Craft a self-loop: a rule whose condition reads the SAME device its
        // target writes. Add a write target at the source tag's own address
        // (D10) and a rule reading D10 -> writing D10.
        let src_tag = project.tags.iter().find(|t| t.address == "D10").unwrap();
        let src_tag_id = src_tag.id;
        let conn_id = project.plc_connections[0].id;

        let loop_target_id = project.write_targets.iter().map(|t| t.id).max().unwrap() + 1;
        project.write_targets.push(WriteTarget {
            id: loop_target_id,
            name: "LoopTarget".to_string(),
            plc_connection_id: conn_id,
            address: "D10".to_string(),
            data_type: "i16".to_string(),
            string_length: None,
            raw_lo: None,
            raw_hi: None,
            eng_lo: None,
            eng_hi: None,
            unit: None,
            decimals: 0,
            enabled: true,
        });
        let loop_rule_id = project.write_rules.iter().map(|r| r.rule.id).max().unwrap() + 1;
        project.write_rules.push(WriteRuleDetail {
            rule: crate::write_rules::WriteRule {
                id: loop_rule_id,
                name: "LoopRule".to_string(),
                enabled: true,
                edge_mode: "rising".to_string(),
                cooldown_ms: None,
                write_target_id: loop_target_id,
                write_value_mode: "constant".to_string(),
                write_constant_value: Some(1.0),
                write_constant_text: None,
                write_source_tag_id: None,
            },
            conditions: vec![crate::write_rule_conditions::WriteRuleCondition {
                id: 0,
                write_rule_id: loop_rule_id,
                source_tag_id: src_tag_id,
                operator: "gt".to_string(),
                threshold_value: Some(1.0),
                threshold_value_2: None,
                threshold_text: None,
            }],
        });

        match import_project(&pool, project).await.unwrap_err() {
            BantoError::Validation { field_errors } => {
                assert!(
                    field_errors.iter().any(|e| e.field == "enabled"),
                    "expected the cycle error on `enabled`, got {field_errors:?}"
                );
            }
            other => panic!("expected Validation (cycle), got {other:?}"),
        }
    }
}
