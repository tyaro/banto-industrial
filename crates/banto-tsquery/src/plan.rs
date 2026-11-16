//! [`FilePlan`]: one data file's connection plus everything
//! `decimate.rs`/`aggregate.rs` need to build a `GROUP BY`/aggregate SQL
//! query directly against its `samples_<n>` table for one `group_key`.
//!
//! ## Why this crate opens its own connections instead of going through `TsReader`
//!
//! `banto_tstore::TsReader` is deliberately minimal ("I4の土台。範囲クエリ・
//! 間引きはI4でやるのでここは最小限" - its own module doc) and its
//! `SqlitePool` field is private, so there is no way to run a custom
//! `SELECT ... GROUP BY ...` through it. `read_decimated`/`aggregate` need
//! exactly that (server-side `MIN`/`MAX`/`COUNT`/`AVG` per bin, per this
//! crate's central design principle: never pull raw rows into Rust for a
//! large range - see `decimate.rs`'s module doc). The alternative would be
//! adding a "give me a raw connection" escape hatch to `banto-tstore`
//! itself, which is out of this task's scope (a completed, reviewed crate)
//! and would leak this crate's SQL-building concerns into it. So this module
//! re-opens each candidate file read-only and re-reads its
//! `tstore_groups`/`tstore_columns` tables directly - a small, self-
//! contained duplication of what `banto-tstore/src/schema.rs::read_file_meta`
//! does, scoped to exactly the one `group_key` a query cares about (not the
//! whole file's metadata, unlike `TsReader::open`).
//!
//! `read_range` (`raw.rs`) does *not* go through this module - it delegates
//! to `TsReader::read_range` directly, since that call is already exactly
//! the "give me every row for this group in this range" operation it needs,
//! with no custom SQL required.
//!
//! ## Uninitialized files are skipped, not errored
//!
//! [`plan_file`] calls [`is_uninitialized`] before querying `tstore_groups`
//! at all: a file whose writer has not yet committed its schema-creation
//! transaction (`banto_tstore::TstoreError::Uninitialized`'s doc comment)
//! returns `Ok(None)` here - the same "this file contributes nothing"
//! outcome as a file that simply never described `group_key`. `catalog.rs`
//! (which shares [`open_file`] but does not build a [`FilePlan`]) calls
//! [`is_uninitialized`] directly for the same reason.
//!
//! ## SQL-identifier safety
//!
//! `table_name`/`column_name` values read back from `tstore_groups`/
//! `tstore_columns` are, in a genuine `banto-tstore` file, always exactly
//! `samples_<n>`/`c<i>` (`schema.rs`'s module doc: "programmatically-derived,
//! always-safe... regardless of what characters a key contains" - because
//! `group_key`/`tag_key` are only ever bound as values, never spliced into
//! identifiers). This module trusts that invariant for *writing* files, but
//! - since it reads files it did not itself just write, from disk, in an
//!   industrial product where a file could in principle be corrupted or
//!   replaced - re-validates the shape ([`is_safe_table_name`]/
//!   [`is_safe_column_name`]) before
//!   splicing either string into a SQL statement, and hard-errors
//!   ([`TsQueryError::UnsafeIdentifier`]) rather than silently coercing or
//!   skipping if it does not match. Defense in depth, not a normal-path
//!   check: no real `banto-tstore` file should ever trip it.

use std::path::Path;

use banto_tstore::{DataFileInfo, DataPlace, ReadOnlyFile};
use sqlx::{Row, SqlitePool};

use crate::error::TsQueryError;

/// One file's plan for a single `group_key` query: an open read-only
/// connection, the group's physical table name, its configured collection
/// period, and - for the caller's requested `tag_keys`, in that exact order
/// - which physical column (if any) holds each one in *this* file.
pub(crate) struct FilePlan {
    /// The open read-only file (a pinned one on a [`DataPlace::Managed`] -
    /// #623 PR7; see [`open_file`]).
    pub(crate) file: ReadOnlyFile,
    pub(crate) table_name: String,
    pub(crate) period_ms: i64,
    /// Same length/order as the caller's `tag_keys`; `None` at index `i`
    /// means `tag_keys[i]` is not part of this file's frozen schema for this
    /// group (design: "ファイル跨ぎは tag_key マッチ... 古いファイルに存在
    /// しないタグは gap").
    pub(crate) columns: Vec<Option<String>>,
}

/// Open one listed data file read-only through its [`DataPlace`] (#623 PR7):
/// [`DataPlace::open_readonly`] by the file's own name. A
/// [`DataPlace::Path`] opens by path exactly as `banto-tstore`'s
/// `connect_readonly` (same options this module always mirrored); a
/// [`DataPlace::Managed`] pins the file and its `-wal`/`-shm` first and
/// refuses (without opening) a name that is a link/reparse point. Close the
/// result with [`ReadOnlyFile::close`] (pool first, then the pins).
pub(crate) async fn open_file(
    place: &DataPlace,
    file: &DataFileInfo,
) -> Result<ReadOnlyFile, TsQueryError> {
    Ok(place.open_readonly(&file_name(file)?).await?)
}

/// The listed file's own name (one path element) - what
/// [`DataPlace::open_readonly`] takes.
pub(crate) fn file_name(file: &DataFileInfo) -> Result<String, TsQueryError> {
    file.path
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .ok_or_else(|| {
            TsQueryError::Storage(format!("ファイル名を読めません: {}", file.path.display()))
        })
}

/// `true` if `pool` has no `banto-tstore` schema at all yet - no
/// `tstore_meta` table, meaning the writer's schema-creation transaction
/// (`banto-tstore/src/schema.rs::create_schema`) has not committed (or this
/// is a genuinely foreign file; either way this crate treats it identically
/// - see `banto_tstore::TstoreError::Uninitialized`'s doc comment).
///
/// Mirrors `banto-tstore/src/schema.rs::read_file_meta`'s own
/// `sqlite_master` probe for the identical condition - duplicated here
/// (rather than reused) because this module deliberately does not route
/// file opens through `banto_tstore::TsReader` (see this module's doc
/// comment "Why this crate opens its own connections instead of going
/// through TsReader"), so there is no `TstoreError` to match on at this call
/// site the way `raw.rs` can.
pub(crate) async fn is_uninitialized(pool: &SqlitePool) -> Result<bool, sqlx::Error> {
    let row =
        sqlx::query("SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'tstore_meta'")
            .fetch_optional(pool)
            .await?;
    Ok(row.is_none())
}

pub(crate) fn is_safe_table_name(name: &str) -> bool {
    name.strip_prefix("samples_")
        .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
}

pub(crate) fn is_safe_column_name(name: &str) -> bool {
    name.strip_prefix('c')
        .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
}

/// Build a [`FilePlan`] for `group_key` in the file at `path`, or `Ok(None)`
/// if this file simply does not describe `group_key` at all (not an error -
/// a normal consequence of a group being added/renamed after this file was
/// created; the caller treats it the same as "this file contributes nothing
/// to this group's query").
pub(crate) async fn plan_file(
    place: &DataPlace,
    info: &DataFileInfo,
    group_key: &str,
    tag_keys: &[String],
) -> Result<Option<FilePlan>, TsQueryError> {
    let path = info.path.as_path();
    let file = open_file(place, info).await?;
    match plan_opened(&file, path, group_key, tag_keys).await {
        Ok(Some((table_name, period_ms, columns))) => Ok(Some(FilePlan {
            file,
            table_name,
            period_ms,
            columns,
        })),
        Ok(None) => {
            file.close().await;
            Ok(None)
        }
        Err(error) => {
            file.close().await;
            Err(error)
        }
    }
}

type PlanParts = (String, i64, Vec<Option<String>>);

async fn plan_opened(
    file: &ReadOnlyFile,
    path: &Path,
    group_key: &str,
    tag_keys: &[String],
) -> Result<Option<PlanParts>, TsQueryError> {
    let pool = file.pool();

    if is_uninitialized(pool)
        .await
        .map_err(|e| incompatible(path, e))?
    {
        // Writer has not yet committed this file's schema-creation
        // transaction (raced) - treat it exactly like "this file does not
        // describe group_key at all" below, not as an error.
        return Ok(None);
    }

    let group_row =
        sqlx::query("SELECT table_name, period_ms FROM tstore_groups WHERE group_key = ? LIMIT 1")
            .bind(group_key)
            .fetch_optional(pool)
            .await
            .map_err(|e| incompatible(path, e))?;

    let Some(group_row) = group_row else {
        return Ok(None);
    };
    let table_name: String = group_row.get("table_name");
    let period_ms: i64 = group_row.get("period_ms");

    if !is_safe_table_name(&table_name) {
        return Err(TsQueryError::UnsafeIdentifier {
            path: path.to_path_buf(),
            identifier: table_name,
        });
    }

    let column_rows = sqlx::query(
        "SELECT column_name, tag_key FROM tstore_columns WHERE group_key = ? ORDER BY column_index ASC",
    )
    .bind(group_key)
    .fetch_all(pool)
    .await
    .map_err(|e| incompatible(path, e))?;

    let mut by_tag_key: std::collections::HashMap<String, String> =
        std::collections::HashMap::with_capacity(column_rows.len());
    for row in column_rows {
        let column_name: String = row.get("column_name");
        let tag_key: String = row.get("tag_key");
        if !is_safe_column_name(&column_name) {
            return Err(TsQueryError::UnsafeIdentifier {
                path: path.to_path_buf(),
                identifier: column_name,
            });
        }
        by_tag_key.insert(tag_key, column_name);
    }

    let columns = tag_keys
        .iter()
        .map(|tag_key| by_tag_key.get(tag_key).cloned())
        .collect();

    Ok(Some((table_name, period_ms, columns)))
}

pub(crate) fn incompatible(path: &Path, err: sqlx::Error) -> TsQueryError {
    TsQueryError::IncompatibleFile {
        path: path.to_path_buf(),
        message: err.to_string(),
    }
}

/// Build one [`FilePlan`] per candidate file that actually describes
/// `group_key`, skipping (not erroring on) files that do not.
pub(crate) async fn plan_files(
    place: &DataPlace,
    files: &[DataFileInfo],
    group_key: &str,
    tag_keys: &[String],
) -> Result<Vec<FilePlan>, TsQueryError> {
    let mut plans = Vec::with_capacity(files.len());
    for file in files {
        match plan_file(place, file, group_key, tag_keys).await {
            Ok(Some(plan)) => plans.push(plan),
            Ok(None) => {}
            Err(error) => {
                close_plans(plans).await;
                return Err(error);
            }
        }
    }
    Ok(plans)
}

/// Close every plan's file (pool first, then its pins - [`ReadOnlyFile::close`]).
/// Plans dropped without this (an error mid-query, a caller's timeout) still
/// release their pins right after their pool (field order).
pub(crate) async fn close_plans(plans: Vec<FilePlan>) {
    for plan in plans {
        plan.file.close().await;
    }
}

impl FilePlan {
    pub(crate) fn pool(&self) -> &SqlitePool {
        self.file.pool()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_table_names() {
        assert!(is_safe_table_name("samples_1"));
        assert!(is_safe_table_name("samples_42"));
        assert!(!is_safe_table_name("samples_"));
        assert!(!is_safe_table_name("samples_1; DROP TABLE x"));
        assert!(!is_safe_table_name("other_table"));
    }

    #[test]
    fn safe_column_names() {
        assert!(is_safe_column_name("c1"));
        assert!(is_safe_column_name("c42"));
        assert!(!is_safe_column_name("c"));
        assert!(!is_safe_column_name("cx"));
        assert!(!is_safe_column_name("ptime"));
    }
}
