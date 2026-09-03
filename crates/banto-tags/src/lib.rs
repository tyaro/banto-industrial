//! banto-tags: I1 タグレジストリ (docs/plan.md I1, docs/recorder-requirements.md
//! §2 "用語", §3.1 "タグ・収集", §3.2 "表示グループ").
//!
//! Three-tier entity model consumed by the collection engine (I3) and any
//! grid UI built on top of it (`banto_storage::list_query` today; a real
//! grid lands with the ChronoGazer app, R系):
//!
//! - [`PlcConnection`]: one PLC endpoint. v1 only writes `protocol =
//!   "modbus-tcp"` (Modbus TCP first for debuggability, plan.md §3's I2
//!   decision); the column stays open for `"slmp"` (MELSEC MC protocol)
//!   later
//! - [`CollectionGroup`]: the unit of periodic PLC bulk read
//!   (recorder-requirements.md §3.1: "収集周期はタグ毎ではなく収集グループ毎")
//! - [`Tag`]: one collection point - address + data type + scaling + unit +
//!   decimals + H/HH/L/LL thresholds (§2, §3.2)
//!
//! This crate is also the first real-world proof of consuming `banto-core`/
//! `banto-storage` via a git tag reference rather than a workspace path
//! (banto's `docs/publishing.md`) - see `Cargo.toml`.
//!
//! Migrations are embedded in this crate (`migrations/`) via `sqlx::migrate!`
//! and applied by [`migrate`] - the consuming app calls this once at
//! startup, mirroring `apps/admin-template/core/src/db.rs`'s pattern in the
//! banto template repo, but packaged for reuse instead of being app-local.
//! `banto-tags` never opens its own database connection: v1's ChronoGazer
//! app shares one SQLite database across I1/I2/I3 tables (a single app-data
//! file, plan.md §5), so connecting is the consuming app's job.

pub mod collection_group;
pub mod plc_connection;
pub mod scaling;
pub mod tag;

// Crate-internal helpers shared by the three entity modules above - not
// part of the public API (see `support.rs`'s doc comment).
mod support;

pub use collection_group::{
    CollectionGroup, CollectionGroupCascadeOutcome, CollectionGroupInput, CollectionGroupService,
    ALLOWED_PERIOD_MS,
};
pub use plc_connection::{
    PlcConnection, PlcConnectionCascadeOutcome, PlcConnectionInput, PlcConnectionService,
    ALLOWED_PROTOCOLS, CALC_CONNECTION_NAME, MEM_CONNECTION_NAME, VIRTUAL_PROTOCOL,
};
pub use scaling::{scale_raw, unscale, Scaling};
pub use tag::{
    BatchTagDeleteError, BatchTagDeleteOutcome, BatchTagError, BatchTagOutcome,
    BatchTagUpdateError, BatchTagUpdateOutcome, GroupTagCount, Tag, TagInput, TagService,
    TagUpdateError, ALLOWED_DATA_TYPES, ALLOWED_TAG_KINDS, COMPUTED_TAG_KIND, INTERNAL_TAG_KIND,
    NUMERIC_DATA_TYPES, PLC_TAG_KIND, STRING_DATA_TYPE,
};

use banto_core::BantoError;
use sqlx::SqlitePool;

/// Run this crate's embedded migrations against `pool`. Idempotent (`sqlx`'s
/// migrator tracks applied versions in its own bookkeeping table), so it is
/// safe to call on every app startup, same as banto's `db.rs::init_db`.
pub async fn migrate(pool: &SqlitePool) -> Result<(), BantoError> {
    sqlx::migrate!("./migrations")
        .run(pool)
        .await
        .map_err(|err| BantoError::Storage(err.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// End-to-end proof that the embedded migrations actually create all
    /// three tables in the expected order (each entity module's own tests
    /// exercise this too via `migrate`, but this is the one test that
    /// checks the schema shape directly rather than through a service).
    #[tokio::test]
    async fn migrate_creates_all_three_tables() {
        let pool = banto_storage::connect_sqlite_memory()
            .await
            .expect("connect_sqlite_memory");
        migrate(&pool).await.expect("migrate should succeed");

        let tables: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' \
             AND name NOT LIKE '_sqlx_%' ORDER BY name",
        )
        .fetch_all(&pool)
        .await
        .expect("query tables");

        assert_eq!(
            tables,
            vec![
                "collection_groups".to_string(),
                "plc_connections".to_string(),
                "tags".to_string(),
            ]
        );
    }

    /// Calling `migrate` twice on the same pool must not error (spec: apps
    /// call this on every startup).
    #[tokio::test]
    async fn migrate_is_idempotent() {
        let pool = banto_storage::connect_sqlite_memory()
            .await
            .expect("connect_sqlite_memory");
        migrate(&pool).await.expect("first migrate");
        migrate(&pool)
            .await
            .expect("second migrate should be a no-op, not an error");
    }
}
