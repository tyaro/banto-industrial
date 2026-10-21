//! banto-tags: I1 タグレジストリ (docs/plan.md I1, docs/recorder-requirements.md
//! §2 "用語", §3.1 "タグ・収集", §3.2 "表示グループ").
//!
//! Three-tier entity model consumed by the collection engine (I3) and any
//! grid UI built on top of it (`banto_storage::list_query` today; a real
//! grid lands with the ChronoGazer app, R系):
//!
//! - [`PlcConnection`]: one data source. Started as one PLC endpoint
//!   (`"modbus-tcp"` first for debuggability, plan.md §3's I2 decision, then
//!   `"slmp"`); the vocabulary is now `"modbus-tcp"`/`"slmp"`/`"virtual"`/
//!   `"postgres"`/`"hub"`/`"push"` ([`ALLOWED_PROTOCOLS`]). `"hub"` (#383 段階3 P2,
//!   2026-10-10 オーナー決定 H1) is banto-hub as a source: at most one per
//!   registry, no endpoint of its own (ChronoGazer's `hub.record` owns it),
//!   and its tags' `address` is the Hub tag's external name - see
//!   [`plc_connection`]'s "`\"hub\"`" section. `"push"` (R2a-2a,
//!   docs/banto-hub-ingest-design.md §3) is a connection banto-hub *receives*
//!   values on: no endpoint either, a JSON `settings` column of its own, and
//!   tags whose `address` is an opaque sender-chosen key - the pure rules
//!   (limits, settings, keys, the bridge's load estimate) live in [`push`]
//! - [`CollectionGroup`]: the unit of periodic PLC bulk read
//!   (recorder-requirements.md §3.1: "収集周期はタグ毎ではなく収集グループ毎")
//! - [`Tag`]: one collection point - address + data type + scaling + unit +
//!   decimals (§2, §3.2). H/HH/L/LL thresholds are **not** part of a tag
//!   (#533, 2026-10-08 owner decision: しきい値は使う側（記録計・SCADA）の
//!   設定) - migration 0018 dropped the `threshold_*` columns
//!
//! - [`settings_json`] (R2b-1c, docs/banto-hub-connection-settings-design.md
//!   §3.5・§4.2・§4.3): a connection's JSON `settings` -> the driver API's
//!   `Settings` ([`settings_to_api`]), the flat-columns <-> `settings` adapter
//!   (`settings_json::legacy`) and validation through a `DriverRegistry`
//!   ([`validate_settings_with_registry`], [`PlcConnectionService::with_registry`]).
//!   Since R2b-1d (migration `0025`) the save path stores `settings` for every protocol
//!   and the flat fields of [`PlcConnection`] are a read-only projection of it.
//! - [`connection_secret`] (R2b-1d, design §3.3・§5.3): the sealed secret rows
//!   (`plc_connection_secrets`) a connection's `settings` refers to with
//!   `{"$secret": "<ref>"}`, written and deleted in the connection's own transaction.
//!   This crate never seals or opens a secret (banto-hub's `secrets` module does) and
//!   refuses a plaintext value under a secret key ([`check_settings_shape`]).
//! - [`upgrade`] (R2b-1d, design §6.4・§6.7): the app-independent pieces of the run-up to
//!   the irreversible migration `0025` - leftover cleanup, "is `0025` pending", and the
//!   password-free backup (`upgrade::backup_before_0025`) - shared by banto-hub and
//!   ChronoGazer, which call them before [`migrate`].
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
pub mod connection_secret;
pub mod plc_connection;
pub mod push;
pub mod scaling;
pub mod settings_json;
pub mod tag;
pub mod upgrade;

// Crate-internal helpers shared by the three entity modules above - not
// part of the public API (see `support.rs`'s doc comment).
mod support;

pub use collection_group::{
    CollectionGroup, CollectionGroupCascadeOutcome, CollectionGroupInput, CollectionGroupService,
    ALLOWED_PERIOD_MS,
};
pub use connection_secret::{
    builtin_secret_keys, check_settings_shape, SealedSecretRow, SecretChange, SecretChanges,
    StoredSecretRow, PASSWORD_KEY, SCHEME_DPAPI_MACHINE_V1, SCHEME_PLAIN_V1, SECRET_REF_LEN,
};
pub use plc_connection::{
    PlcConnection, PlcConnectionCascadeOutcome, PlcConnectionInput, PlcConnectionService,
    ALLOWED_CPU_TYPES, ALLOWED_PROTOCOLS, ALLOWED_WORD_ORDERS, CALC_CONNECTION_NAME, CPU_TYPE_Q,
    CPU_TYPE_R, HUB_PROTOCOL, MEM_CONNECTION_NAME, MODBUS_PROTOCOL, POSTGRES_PROTOCOL,
    PUSH_PROTOCOL, VIRTUAL_PROTOCOL, WORD_ORDER_HIGH_LOW, WORD_ORDER_LOW_HIGH,
};
pub use push::{
    bridge_load, validate_push_key, validate_push_settings, BridgeColumn, BridgeLoad, BridgeQuery,
    PushBridge, PushLimits, PushSettings, QueryLoad, BRIDGE_KIND_DB, DEFAULT_STALE_TIMEOUT_MS,
    MAX_PUSH_KEY_LEN, MAX_STALE_TIMEOUT_MS, MIN_STALE_TIMEOUT_MS, PUSH_LIMITS,
};
pub use scaling::{scale_raw, unscale, Scaling};
pub use settings_json::{settings_to_api, validate_settings_with_registry};
pub use tag::{
    validate_hub_tag_address, BatchTagDeleteError, BatchTagDeleteOutcome, BatchTagError,
    BatchTagOutcome, BatchTagUpdateError, BatchTagUpdateOutcome, GroupTagCount, Tag, TagInput,
    TagService, TagUpdateError, ALLOWED_DATA_TYPES, ALLOWED_STRING_ENCODINGS, ALLOWED_TAG_KINDS,
    COMPUTED_TAG_KIND, DB_TAG_KIND, INTERNAL_TAG_KIND, MAX_DB_COLUMN_NAME_LEN,
    MAX_HUB_EXTERNAL_NAME_LEN, MODBUS_ONLY_DATA_TYPES, NUMERIC_DATA_TYPES, PLC_TAG_KIND,
    STRING_DATA_TYPE,
};

use banto_core::BantoError;
use sqlx::SqlitePool;

/// This crate's migration bookkeeping table. Not the default
/// `_sqlx_migrations`: the consuming app runs its OWN `sqlx::migrate!` on the
/// same pool, and two migrators sharing one bookkeeping table collide on
/// overlapping version numbers (`MigrateError::VersionMismatch`,
/// docs/r1a-readme-gaps.md). sqlx 0.9's `Migrator::dangerous_set_table_name`
/// lets each migrator keep its own table (schema cleanup, 2026-10-02). The `dangerous_`
/// part - a database migrated under the old shared table loses track of what
/// was applied - is accepted because existing alpha databases may be thrown
/// away (2026-10-02 owner decision); the apps refuse such a database before
/// calling [`migrate`] (`apps/*/core/src/db.rs`).
pub const MIGRATIONS_TABLE: &str = "_sqlx_migrations_banto_tags";

/// Run this crate's embedded migrations against `pool`. Idempotent (`sqlx`'s
/// migrator tracks applied versions in [`MIGRATIONS_TABLE`]), so it is safe
/// to call on every app startup, same as banto's `db.rs::init_db`.
pub async fn migrate(pool: &SqlitePool) -> Result<(), BantoError> {
    let mut migrator = sqlx::migrate!("./migrations");
    migrator.dangerous_set_table_name(MIGRATIONS_TABLE);
    migrator
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
                // R2b-1d (migration 0025): the sealed secrets a connection's
                // `settings` refers to.
                "plc_connection_secrets".to_string(),
                "plc_connections".to_string(),
                "tags".to_string(),
            ]
        );
    }

    /// Schema cleanup (2026-10-02): applied versions are recorded in this crate's own
    /// [`MIGRATIONS_TABLE`], never in the shared default `_sqlx_migrations`
    /// that the consuming app's migrator would otherwise collide on.
    #[tokio::test]
    async fn migrate_records_into_its_own_bookkeeping_table() {
        let pool = banto_storage::connect_sqlite_memory()
            .await
            .expect("connect_sqlite_memory");
        migrate(&pool).await.expect("migrate should succeed");

        let tables: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name LIKE '\\_sqlx%' \
             ESCAPE '\\' ORDER BY name",
        )
        .fetch_all(&pool)
        .await
        .expect("query tables");
        assert_eq!(tables, vec![MIGRATIONS_TABLE.to_string()]);

        let applied: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT COUNT(*) FROM {MIGRATIONS_TABLE}"
        )))
        .fetch_one(&pool)
        .await
        .expect("count applied");
        let embedded = sqlx::migrate!("./migrations").iter().count();
        assert_eq!(applied as usize, embedded);
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
