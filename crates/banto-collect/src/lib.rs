//! banto-collect: I3b 収集エンジン (docs/plan.md I3b, docs/recorder-requirements.md
//! §3.1 "タグ・収集", §3.5 "イベント・出力", §4 "非機能要件").
//!
//! Takes a snapshot of the tag registry's enabled configuration, opens a
//! `banto-tstore` file under a data directory, and drives one concurrent
//! collection task per PLC connection - reading every group on its period,
//! writing scaled samples (with NULL gaps for missed/failed reads), and
//! supplying two live views the UI consumes: a current-value cache and an
//! event stream. It runs independently of the UI (recorder-requirements.md
//! §4: a UI crash or browser disconnect never touches collection) and stays
//! up 24/365, folding a PLC drop into Bad-quality rows + a disconnect event
//! and auto-reconnecting with backoff, rather than ever tearing the loop down.
//!
//! ## Responsibility boundary (司令塔決定)
//!
//! This crate *reads* the registry to build a [`CollectorConfig`]
//! ([`build_config`]) but does **not** watch for configuration changes or
//! decide when to restart - that is the calling app's job (the future
//! ChronoGazer app). A `CollectorConfig` is a point-in-time snapshot; on a
//! definition change, the app rebuilds it and starts a fresh [`Collector`].
//!
//! ## Shape
//!
//! - [`config`]: [`CollectorConfig`] + [`build_config`] - the registry ->
//!   snapshot step (I1 -> I3b bridge), plus the derived `banto-tstore`
//!   [`StoreConfig`](banto_tstore::StoreConfig).
//! - [`collector`]: [`Collector`] - lifecycle (`start`/`stop`), the UI's read
//!   handles (`current_values`/`status`/`subscribe_events`), and (T7-1,
//!   docs/tag-server-design.md §4.3) [`Collector::apply_config`] - online
//!   partial reconfiguration that touches only the connections that actually
//!   changed. See `collector.rs`'s module doc for the safety derivation.
//! - [`current`]: [`CurrentValuesHandle`]/[`CurrentSample`]/[`Quality`] - the
//!   latest-value cache with read-time Stale derivation.
//! - [`event`]: [`CollectEvent`]/[`EventKind`]/[`EventSink`] - the two-output
//!   (live broadcast + durable `collect_events` table) event delivery.
//! - [`error`]: [`CollectError`] - config/lifecycle failures (the hot loop
//!   never surfaces errors this way; it uses quality flags and events).
//! - `task` (private): the per-connection collection loop - the crate's core
//!   concurrency design (one task per connection, in-task min-deadline
//!   scheduler, non-blocking reconnect). [`BackoffConfig`] and
//!   [`ConnectionStatus`] are its public surface, plus (T2-2,
//!   docs/tag-server-design.md §6-5) the [`ClientFactory`]/[`ClientSpec`]/
//!   [`ClientProtocol`] client-construction injection seam and its
//!   [`default_client_factory`].
//!
//! ## Why `migrate` is not `sqlx::migrate!`
//!
//! The ChronoGazer app shares one SQLite database across I1's tables and this
//! crate's `collect_events`. `banto-tags` applies its schema via
//! `sqlx::migrate!`. With sqlx 0.8 every migrator recorded applied versions in
//! one shared `_sqlx_migrations` bookkeeping table, so a second independent
//! `sqlx::migrate!` set against the same database would collide there
//! (overlapping version numbers, mismatched checksums). So this crate applies
//! its one table with an idempotent `CREATE TABLE IF NOT EXISTS` instead - the
//! one deliberate deviation from "banto-tags の migrate 方式に倣う" (the design
//! note calling for that method predates noticing the shared-migrator
//! collision).
//!
//! Since the 2026-10-02 schema cleanup (sqlx 0.9) each migrator keeps its own bookkeeping
//! table (`Migrator::dangerous_set_table_name`: `banto_tags::MIGRATIONS_TABLE`,
//! the apps' `db::MIGRATIONS_TABLE`), so the collision no longer forces this.
//! The single idempotent table is kept as-is anyway: it has nothing to
//! migrate yet, and keeping it out of any bookkeeping table means it never
//! participates in the apps' "is this an old-format database?" check.

pub mod collector;
pub mod config;
pub mod current;
pub mod error;
pub mod event;
// T9-1/T9-2 (docs/ux-plan.md §1): in-process simulator lifecycle for
// `simulation = true` connections. Started life as a pure implementation
// detail of `crate::collector::Collector`; T9-2 (apps/banto-hub/core's
// broker-routed SLMP path - see this module's own doc comment, "SLMP +
// banto-hub の broker 経路について") needs to start/reuse/stop a simulator
// itself, so `start`/`SimulatorHandle` are now public. Everything else in
// this module (the ramp/toggle generation tasks, the Modbus/SLMP-specific
// startup helpers) stays crate-private.
pub mod simulation;
mod task;

pub use collector::{ApplyReport, Collector, CollectorOptions};
pub use config::{
    build_config, build_config_from, build_config_lenient_from,
    build_config_lenient_with_thresholds, check_tag_address, config_exclusions,
    connections_with_collected_groups, CollectorConfig, ConfigExclusion, ExclusionReason,
    ExclusionUnit, Protocol, RegistrySnapshot, TagAddressField, TagAddressIssue, Thresholds,
};
pub use current::{
    CurrentReading, CurrentSample, CurrentValuesHandle, Quality, STALE_PERIOD_FACTOR,
};
pub use error::CollectError;
pub use event::{
    CollectEvent, EventKind, EventSink, ThresholdLevel, DEFAULT_EVENT_CHANNEL_CAPACITY,
};
pub use task::{
    default_client_factory, BackoffConfig, ClientFactory, ClientProtocol, ClientSpec,
    ConnectionStatus,
};

use sqlx::SqlitePool;

/// Create this crate's `collect_events` table if it does not already exist.
/// Idempotent (safe to call on every app startup, same contract as
/// `banto_tags::migrate`), and applied statement-by-statement from the
/// embedded `migrations/0001_collect_events.sql` with `CREATE TABLE/INDEX IF
/// NOT EXISTS` - *not* via `sqlx::migrate!` (see this crate's module doc for
/// why). The consuming app calls this once at startup, after
/// `banto_tags::migrate`, against the same shared pool.
pub async fn migrate(pool: &SqlitePool) -> Result<(), CollectError> {
    const DDL: &str = include_str!("../migrations/0001_collect_events.sql");
    // `raw_sql` runs the whole script - multiple statements and SQL comments -
    // in one call, which `sqlx::query` (single prepared statement) cannot.
    sqlx::raw_sql(DDL)
        .execute(pool)
        .await
        .map_err(|err| CollectError::Migrate(err.to_string()))?;
    add_limit_value_column(pool).await
}

/// #532 (2026-10-08 owner decision): `threshold_*` events record the limit
/// they were judged against, in `collect_events.limit_value`.
///
/// SQLite has no `ADD COLUMN IF NOT EXISTS`, and this table is created by the
/// idempotent DDL above rather than a migrator with bookkeeping, so the
/// column is added here only when `pragma_table_info` does not list it yet: a
/// database created before #532 gets it on its next startup, a new one gets it
/// right after the `CREATE TABLE`, and a second run is a no-op. The embedded
/// `0001_collect_events.sql` is left as-is (it is the original table shape;
/// this function is the one place that extends it). The column is nullable
/// and old rows read as `NULL` ("limit not recorded"). banto-hub shares this
/// table and gets the column too; its events also carry the limit (it still
/// judges against the tag columns until #533).
async fn add_limit_value_column(pool: &SqlitePool) -> Result<(), CollectError> {
    let present: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('collect_events') WHERE name = 'limit_value'",
    )
    .fetch_one(pool)
    .await
    .map_err(|err| CollectError::Migrate(err.to_string()))?;
    if present == 0 {
        sqlx::query("ALTER TABLE collect_events ADD COLUMN limit_value REAL")
            .execute(pool)
            .await
            .map_err(|err| CollectError::Migrate(err.to_string()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn migrate_creates_collect_events_and_is_idempotent() {
        let pool = banto_storage::connect_sqlite_memory().await.unwrap();
        migrate(&pool).await.expect("first migrate");
        migrate(&pool).await.expect("second migrate is a no-op");

        // Table exists and is empty.
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM collect_events")
            .fetch_one(&pool)
            .await
            .expect("collect_events should exist");
        assert_eq!(count, 0);
    }

    /// #532: a database whose `collect_events` predates `limit_value` gets
    /// the column on the next `migrate` (existing rows keep `NULL`), and a
    /// fresh one has it too. Running twice stays a no-op.
    #[tokio::test]
    async fn migrate_adds_limit_value_to_a_table_created_before_it() {
        let pool = banto_storage::connect_sqlite_memory().await.unwrap();
        sqlx::raw_sql(include_str!("../migrations/0001_collect_events.sql"))
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO collect_events (ts, kind) VALUES (1, 'collection_started')")
            .execute(&pool)
            .await
            .unwrap();

        migrate(&pool).await.expect("migrate adds the column");
        migrate(&pool).await.expect("second migrate is a no-op");

        let old: Option<f64> = sqlx::query_scalar("SELECT limit_value FROM collect_events")
            .fetch_one(&pool)
            .await
            .expect("limit_value should exist");
        assert_eq!(old, None);

        let fresh = banto_storage::connect_sqlite_memory().await.unwrap();
        migrate(&fresh).await.unwrap();
        let present: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pragma_table_info('collect_events') WHERE name = 'limit_value'",
        )
        .fetch_one(&fresh)
        .await
        .unwrap();
        assert_eq!(present, 1);
    }

    #[tokio::test]
    async fn migrate_coexists_with_banto_tags_migrate() {
        // The whole point of the CREATE-TABLE-IF-NOT-EXISTS approach: both
        // schemas apply to one shared database without a migrator collision.
        let pool = banto_storage::connect_sqlite_memory().await.unwrap();
        banto_tags::migrate(&pool).await.expect("tags migrate");
        migrate(&pool).await.expect("collect migrate");

        let tables: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM sqlite_master WHERE type='table' AND name IN \
             ('tags', 'collection_groups', 'plc_connections', 'collect_events') ORDER BY name",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            tables,
            vec![
                "collect_events".to_string(),
                "collection_groups".to_string(),
                "plc_connections".to_string(),
                "tags".to_string(),
            ]
        );
    }
}
