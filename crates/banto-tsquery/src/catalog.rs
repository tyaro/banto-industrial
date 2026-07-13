//! [`catalog`]: [`crate::TsQuery::catalog`]'s implementation - what a UI's
//! period picker / group selector initializes from (recorder-requirements.md
//! §6 "ヒストリカル（期間指定トレンド + CSV 出力）") without needing the tag
//! registry (I1) at all, mirroring `banto-tstore`'s own self-description
//! principle.
//!
//! Unlike the other three query methods, this one is not range-scoped -
//! every recognized file in `data_dir` is opened (bounded by the retention
//! window, default 90 days: cheap, and this is an init-time call, not a hot
//! path). For each group/tag, later files (ascending `(date, seq)` order)
//! overwrite earlier ones' name/unit/decimals, so the reported metadata
//! reflects the most recent definition even though `tags` itself is the
//! *union* of every tag ever seen (a tag retired from a group's live config
//! still needs to be selectable for a historical range that predates its
//! removal).

use std::collections::HashMap;
use std::path::Path;

use banto_tstore::list_data_files;
use sqlx::Row;

use crate::error::TsQueryError;
use crate::plan::{incompatible, is_safe_column_name, is_safe_table_name, open_readonly};
use crate::types::{Catalog, GroupCatalogEntry, TagCatalogEntry};

pub(crate) async fn catalog(data_dir: &Path) -> Result<Catalog, TsQueryError> {
    let files = list_data_files(data_dir)?;
    let mut groups: HashMap<String, GroupCatalogEntry> = HashMap::new();

    for file in &files {
        let pool = open_readonly(&file.path).await?;

        let group_rows = sqlx::query("SELECT group_key, group_name, table_name FROM tstore_groups")
            .fetch_all(&pool)
            .await
            .map_err(|e| incompatible(&file.path, e))?;

        for group_row in group_rows {
            let group_key: String = group_row.try_get("group_key")?;
            let group_name: String = group_row.try_get("group_name")?;
            let table_name: String = group_row.try_get("table_name")?;
            if !is_safe_table_name(&table_name) {
                return Err(TsQueryError::UnsafeIdentifier {
                    path: file.path.clone(),
                    identifier: table_name,
                });
            }

            let column_rows = sqlx::query(
                "SELECT column_name, tag_key, tag_name, unit, decimals \
                 FROM tstore_columns WHERE group_key = ? ORDER BY column_index ASC",
            )
            .bind(&group_key)
            .fetch_all(&pool)
            .await
            .map_err(|e| incompatible(&file.path, e))?;

            let range_sql = format!("SELECT MIN(ptime), MAX(ptime) FROM {table_name}");
            let range_row = sqlx::query(&range_sql)
                .fetch_one(&pool)
                .await
                .map_err(|e| incompatible(&file.path, e))?;
            let min_ptime: Option<i64> = range_row.try_get(0)?;
            let max_ptime: Option<i64> = range_row.try_get(1)?;

            let entry = groups
                .entry(group_key.clone())
                .or_insert_with(|| GroupCatalogEntry {
                    group_key: group_key.clone(),
                    group_name: group_name.clone(),
                    tags: Vec::new(),
                    earliest_ms: None,
                    latest_ms: None,
                });
            // `files` is ascending by (date, seq) - later iterations always
            // win, so this ends up holding the most recent file's name.
            entry.group_name = group_name;

            for column_row in column_rows {
                let column_name: String = column_row.try_get("column_name")?;
                if !is_safe_column_name(&column_name) {
                    return Err(TsQueryError::UnsafeIdentifier {
                        path: file.path.clone(),
                        identifier: column_name,
                    });
                }
                let tag_key: String = column_row.try_get("tag_key")?;
                let decimals: i64 = column_row.try_get("decimals")?;
                let tag_entry = TagCatalogEntry {
                    tag_key: tag_key.clone(),
                    tag_name: column_row.try_get("tag_name")?,
                    unit: column_row.try_get("unit")?,
                    decimals: decimals as u8,
                };
                match entry.tags.iter_mut().find(|t| t.tag_key == tag_key) {
                    Some(existing) => *existing = tag_entry,
                    None => entry.tags.push(tag_entry),
                }
            }

            if let Some(min_ptime) = min_ptime {
                entry.earliest_ms = Some(entry.earliest_ms.map_or(min_ptime, |e| e.min(min_ptime)));
            }
            if let Some(max_ptime) = max_ptime {
                entry.latest_ms = Some(entry.latest_ms.map_or(max_ptime, |l| l.max(max_ptime)));
            }
        }
    }

    let mut groups: Vec<GroupCatalogEntry> = groups.into_values().collect();
    groups.sort_by(|a, b| a.group_key.cmp(&b.group_key));
    for group in &mut groups {
        group.tags.sort_by(|a, b| a.tag_key.cmp(&b.tag_key));
    }

    Ok(Catalog { groups })
}
