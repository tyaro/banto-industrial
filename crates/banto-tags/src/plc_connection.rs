//! PLC connection (recorder-requirements.md §1 "対象環境"): one PLC endpoint
//! that a [`crate::collection_group::CollectionGroup`] reads from.
//!
//! `protocol` is `TEXT` + `CHECK` rather than a Rust enum precisely so that
//! adding a protocol is a migration plus a widened [`ALLOWED_PROTOCOLS`], not
//! a schema type change - and I2a is that prediction coming true: `"slmp"`
//! (MELSEC MC protocol, the eventual primary target -
//! `banto_plc::slmp::SlmpClient`) joined `"modbus-tcp"` (chosen first for
//! debuggability, plan.md §3's I2 decision) in migration
//! `0004_plc_connections_allow_slmp.sql`. That migration is worth reading
//! before adding a third: SQLite cannot `ALTER` a `CHECK`, so widening it means
//! rebuilding the table, and the ordering constraints there are not obvious.
//!
//! Which protocol a row names determines how its tags' `address` text is
//! parsed - `banto_plc::Address::parse` for `"modbus-tcp"` (`"40001"`),
//! `banto_plc::Address::parse_slmp` for `"slmp"` (`"D100"`). The two notations
//! do not overlap, so a mismatch surfaces as a per-tag error rather than a
//! misdirected read.
//!
//! [`PlcConnection::unit_id`] is Modbus-specific (a slave id inherited from
//! RTU gateways). SLMP has no equivalent single byte - its station addressing
//! is the network/PC/IO/area access route in `banto_plc::slmp::SlmpConfig` - so
//! `"slmp"` rows simply carry the column's default.

use banto_core::{BantoError, FieldError, ListParams, ListResult};
use banto_storage::ColumnMap;
use serde::{Deserialize, Serialize};
use sqlx::{QueryBuilder, Sqlite, SqlitePool};

use crate::support::{map_write_error, max_length_message, range_message, required_message};

/// Protocols accepted in `plc_connections.protocol` today. Mirrors the SQL
/// `CHECK` - as widened by `migrations/0004_plc_connections_allow_slmp.sql`,
/// not `0001`'s original - and is kept in Rust too so
/// [`validate_plc_connection_input`] produces a friendly `FieldError` instead
/// of surfacing the raw SQLite CHECK constraint violation. The two must be
/// changed together; `every_allowed_protocol_is_accepted_by_the_sql_check` is
/// the tripwire if they drift.
pub const ALLOWED_PROTOCOLS: &[&str] = &["modbus-tcp", "slmp"];

const MAX_NAME_LEN: usize = 100;
const MIN_PORT: i64 = 1;
const MAX_PORT: i64 = 65535;
// Modbus unit/slave id valid range (0 = broadcast, 1..247 = addressable
// slaves - RTU/TCP gateways sometimes also accept up to 255).
const MIN_UNIT_ID: i64 = 0;
const MAX_UNIT_ID: i64 = 255;

fn default_protocol() -> String {
    "modbus-tcp".to_string()
}

fn default_unit_id() -> i64 {
    1
}

fn default_enabled() -> bool {
    true
}

/// A row of the `plc_connections` table, wire-shaped (camelCase) for a
/// future settings grid (docs/recorder-requirements.md §6 "タグ設定"
/// screen: "PLC 接続設定含む").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct PlcConnection {
    pub id: i64,
    pub name: String,
    pub protocol: String,
    pub host: String,
    pub port: i64,
    pub unit_id: i64,
    pub enabled: bool,
}

/// Create/update payload. `protocol`/`unit_id`/`enabled` default (spec:
/// "'modbus-tcp' 固定で開始"; "既定1") when omitted from a deserialized
/// payload - constructing one directly in Rust (e.g. from tests) must still
/// set every field explicitly since `#[serde(default = ..)]` only applies
/// to `Deserialize`.
#[derive(Debug, Clone, Deserialize)]
pub struct PlcConnectionInput {
    pub name: String,
    #[serde(default = "default_protocol")]
    pub protocol: String,
    pub host: String,
    pub port: i64,
    #[serde(default = "default_unit_id")]
    pub unit_id: i64,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

/// Validate a [`PlcConnectionInput`]: `name`/`host` trimmed non-empty (name
/// additionally capped at `MAX_NAME_LEN`), `protocol` in [`ALLOWED_PROTOCOLS`],
/// `port` in `1..=65535`, `unit_id` in `0..=255`. Returns every violation,
/// not just the first (mirrors `items::validate_item_input` in the banto
/// template repo).
fn validate_plc_connection_input(input: &PlcConnectionInput) -> Result<(), BantoError> {
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

    if !ALLOWED_PROTOCOLS.contains(&input.protocol.as_str()) {
        errors.push(FieldError {
            field: "protocol".to_string(),
            message: format!(
                "対応プロトコルは {} のいずれかです",
                ALLOWED_PROTOCOLS.join(", ")
            ),
        });
    }

    if input.host.trim().is_empty() {
        errors.push(FieldError {
            field: "host".to_string(),
            message: required_message(),
        });
    }

    if !(MIN_PORT..=MAX_PORT).contains(&input.port) {
        errors.push(FieldError {
            field: "port".to_string(),
            message: range_message(MIN_PORT, MAX_PORT),
        });
    }

    if !(MIN_UNIT_ID..=MAX_UNIT_ID).contains(&input.unit_id) {
        errors.push(FieldError {
            field: "unitId".to_string(),
            message: range_message(MIN_UNIT_ID, MAX_UNIT_ID),
        });
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(BantoError::Validation {
            field_errors: errors,
        })
    }
}

fn column_map() -> ColumnMap {
    ColumnMap::new()
        .column("id", "id")
        .column("name", "name")
        .column("protocol", "protocol")
        .column("host", "host")
        .column("port", "port")
        .column("unitId", "unit_id")
        .column("enabled", "enabled")
}

const RESOURCE: &str = "plc_connections";
const COLUMNS: &str = "id, name, protocol, host, port, unit_id, enabled";

/// Service layer for the `plc_connections` resource. `Clone` is cheap
/// (`SqlitePool` is `Arc`-backed), matching the pattern of every resource
/// service in the banto template repo.
#[derive(Clone)]
pub struct PlcConnectionService {
    pool: SqlitePool,
}

impl PlcConnectionService {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn list(&self, params: ListParams) -> Result<ListResult<PlcConnection>, BantoError> {
        let columns = column_map();

        let mut rows_builder: QueryBuilder<'_, Sqlite> =
            QueryBuilder::new(format!("SELECT {COLUMNS} FROM plc_connections"));
        banto_storage::list_query::sqlite::apply_list_params(&mut rows_builder, &columns, &params)?;
        let rows: Vec<PlcConnection> = rows_builder
            .build_query_as::<PlcConnection>()
            .fetch_all(&self.pool)
            .await
            .map_err(banto_storage::storage_error)?;

        let mut count_builder: QueryBuilder<'_, Sqlite> =
            QueryBuilder::new("SELECT COUNT(*) FROM plc_connections");
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

    pub async fn get(&self, id: i64) -> Result<PlcConnection, BantoError> {
        sqlx::query_as::<_, PlcConnection>(&format!(
            "SELECT {COLUMNS} FROM plc_connections WHERE id = ?"
        ))
        .bind(id)
        .fetch_one(&self.pool)
        .await
        .map_err(|err| banto_storage::not_found(err, RESOURCE, id.to_string()))
    }

    pub async fn create(&self, input: PlcConnectionInput) -> Result<PlcConnection, BantoError> {
        validate_plc_connection_input(&input)?;
        sqlx::query_as::<_, PlcConnection>(&format!(
            "INSERT INTO plc_connections (name, protocol, host, port, unit_id, enabled) \
             VALUES (?, ?, ?, ?, ?, ?) RETURNING {COLUMNS}"
        ))
        .bind(input.name.trim())
        .bind(&input.protocol)
        .bind(input.host.trim())
        .bind(input.port)
        .bind(input.unit_id)
        .bind(input.enabled)
        .fetch_one(&self.pool)
        .await
        .map_err(|err| map_write_error(err, "name", "", ""))
    }

    pub async fn update(
        &self,
        id: i64,
        input: PlcConnectionInput,
    ) -> Result<PlcConnection, BantoError> {
        validate_plc_connection_input(&input)?;
        sqlx::query_as::<_, PlcConnection>(&format!(
            "UPDATE plc_connections SET name = ?, protocol = ?, host = ?, port = ?, unit_id = ?, enabled = ? \
             WHERE id = ? RETURNING {COLUMNS}"
        ))
        .bind(input.name.trim())
        .bind(&input.protocol)
        .bind(input.host.trim())
        .bind(input.port)
        .bind(input.unit_id)
        .bind(input.enabled)
        .bind(id)
        .fetch_one(&self.pool)
        .await
        .map_err(|err| match err {
            sqlx::Error::RowNotFound => BantoError::NotFound {
                resource: RESOURCE.to_string(),
                id: id.to_string(),
            },
            other => map_write_error(other, "name", "", ""),
        })
    }

    /// Delete, refusing when any [`crate::collection_group::CollectionGroup`]
    /// still references this connection (docs/plan.md I1 spec: "使用中の
    /// PlcConnection ... の削除は拒否。在籍タグ/グループ数を数えて Validation
    /// エラー"). The count is taken in the same call, before the DELETE, so
    /// the error message can say exactly how many groups are in the way
    /// rather than just repeating the opaque FOREIGN KEY constraint failure
    /// `ON DELETE RESTRICT` would otherwise surface.
    pub async fn delete(&self, id: i64) -> Result<(), BantoError> {
        let group_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM collection_groups WHERE plc_connection_id = ?",
        )
        .bind(id)
        .fetch_one(&self.pool)
        .await
        .map_err(banto_storage::storage_error)?;
        if group_count > 0 {
            return Err(BantoError::Validation {
                field_errors: vec![FieldError {
                    field: "id".to_string(),
                    message: format!(
                        "この接続を使用している収集グループが{group_count}件あるため削除できません"
                    ),
                }],
            });
        }

        let result = sqlx::query("DELETE FROM plc_connections WHERE id = ?")
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
    use crate::migrate;
    use banto_core::{FilterOp, FilterState, Pagination, SortDirection, SortState};
    use serde_json::json;

    async fn service() -> PlcConnectionService {
        let pool = banto_storage::connect_sqlite_memory()
            .await
            .expect("connect_sqlite_memory");
        migrate(&pool).await.expect("migrate");
        PlcConnectionService::new(pool)
    }

    fn sample_input(name: &str) -> PlcConnectionInput {
        PlcConnectionInput {
            name: name.to_string(),
            protocol: "modbus-tcp".to_string(),
            host: "192.168.1.10".to_string(),
            port: 502,
            unit_id: 1,
            enabled: true,
        }
    }

    #[tokio::test]
    async fn create_then_get_round_trips() {
        let svc = service().await;
        let created = svc
            .create(sample_input("Line1 PLC"))
            .await
            .expect("create should succeed");
        assert_eq!(created.name, "Line1 PLC");
        assert_eq!(created.protocol, "modbus-tcp");
        assert_eq!(created.host, "192.168.1.10");
        assert_eq!(created.port, 502);
        assert_eq!(created.unit_id, 1);
        assert!(created.enabled);

        let fetched = svc.get(created.id).await.expect("get should succeed");
        assert_eq!(fetched, created);
    }

    #[tokio::test]
    async fn create_trims_name_and_host() {
        let svc = service().await;
        let mut input = sample_input("  Padded  ");
        input.host = "  10.0.0.1  ".to_string();
        let created = svc.create(input).await.expect("create should succeed");
        assert_eq!(created.name, "Padded");
        assert_eq!(created.host, "10.0.0.1");
    }

    #[tokio::test]
    async fn create_rejects_empty_name() {
        let svc = service().await;
        let mut input = sample_input("   ");
        input.name = "   ".to_string();
        let err = svc.create(input).await.unwrap_err();
        match err {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "name");
                assert_eq!(field_errors[0].message, "必須項目です");
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    /// Used `"slmp"` as its example of a not-yet-allowed value until I2a made
    /// `"slmp"` valid; `"ethernet-ip"` stands in now. Deliberately a protocol
    /// that plausibly *could* be added one day (rather than a nonsense string),
    /// so this keeps testing the real failure mode - a protocol nobody has
    /// implemented yet - and will need the same flip if EtherNet/IP ever lands.
    #[tokio::test]
    async fn create_rejects_unknown_protocol() {
        let svc = service().await;
        let mut input = sample_input("X");
        input.protocol = "ethernet-ip".to_string();
        let err = svc.create(input).await.unwrap_err();
        match err {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "protocol");
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    /// I2a: `"slmp"` is now a real protocol (`banto_plc::slmp::SlmpClient`), so
    /// it must survive both the Rust validation *and* the SQL `CHECK` - which
    /// only a round trip through the database can prove, since the two are
    /// separate declarations of the same rule.
    #[tokio::test]
    async fn create_accepts_slmp() {
        let svc = service().await;
        let mut input = sample_input("MELSEC Line2");
        input.protocol = "slmp".to_string();
        input.port = 5007;
        let created = svc
            .create(input)
            .await
            .expect("slmp should be accepted since migration 0004");
        assert_eq!(created.protocol, "slmp");

        let fetched = svc.get(created.id).await.expect("get should succeed");
        assert_eq!(fetched.protocol, "slmp");
    }

    #[tokio::test]
    async fn update_can_switch_a_connection_between_protocols() {
        let svc = service().await;
        let created = svc.create(sample_input("Switcher")).await.unwrap();
        assert_eq!(created.protocol, "modbus-tcp");

        let mut input = sample_input("Switcher");
        input.protocol = "slmp".to_string();
        let updated = svc.update(created.id, input).await.expect("update to slmp");
        assert_eq!(updated.protocol, "slmp");

        let mut back = sample_input("Switcher");
        back.protocol = "modbus-tcp".to_string();
        let updated = svc
            .update(created.id, back)
            .await
            .expect("update back to modbus-tcp");
        assert_eq!(updated.protocol, "modbus-tcp");
    }

    /// [`ALLOWED_PROTOCOLS`] and the SQL `CHECK` are two hand-written copies of
    /// one rule. This is the tripwire for them drifting: it inserts every
    /// allowed protocol through the service (so both copies are exercised), and
    /// fails if the Rust list has grown past what the schema accepts.
    #[tokio::test]
    async fn every_allowed_protocol_is_accepted_by_the_sql_check() {
        let svc = service().await;
        for (i, protocol) in ALLOWED_PROTOCOLS.iter().enumerate() {
            let mut input = sample_input(&format!("conn{i}"));
            input.protocol = (*protocol).to_string();
            let created = svc.create(input).await.unwrap_or_else(|e| {
                panic!("{protocol} is in ALLOWED_PROTOCOLS but the SQL CHECK rejected it: {e:?}")
            });
            assert_eq!(&created.protocol, protocol);
        }
    }

    /// The reverse direction: a protocol the SQL `CHECK` would accept must not
    /// be missing from [`ALLOWED_PROTOCOLS`], or callers would get SQLite's raw
    /// constraint-violation text instead of a field-level message. Bypasses the
    /// service layer deliberately - that is the only way to ask the schema
    /// directly what it allows.
    #[tokio::test]
    async fn the_sql_check_accepts_nothing_beyond_allowed_protocols() {
        let pool = banto_storage::connect_sqlite_memory()
            .await
            .expect("connect_sqlite_memory");
        migrate(&pool).await.expect("migrate");

        for protocol in ["ethernet-ip", "opc-ua", "", "MODBUS-TCP", "SLMP"] {
            let result = sqlx::query(
                "INSERT INTO plc_connections (name, protocol, host, port) VALUES (?, ?, '1.2.3.4', 502)",
            )
            .bind(protocol)
            .bind(protocol)
            .execute(&pool)
            .await;
            assert!(
                result.is_err(),
                "the SQL CHECK accepted {protocol:?}, which is not in ALLOWED_PROTOCOLS"
            );
        }
    }

    /// Migration 0004 rebuilds `plc_connections` (SQLite cannot `ALTER` a
    /// `CHECK`), and every other test in this crate only ever runs it against an
    /// *empty* database - where a broken rebuild passes unnoticed, because with
    /// no rows there are no foreign keys to violate. This is the test that
    /// actually exercises it: a hand-built copy of the pre-0004 schema, seeded
    /// with a `plc_connections` row, a `collection_groups` row referencing it,
    /// and a `tags` row referencing *that*, so both `ON DELETE RESTRICT` links
    /// the migration has to work around are live.
    ///
    /// Faithfulness matters more than convenience here, since 0004's whole shape
    /// is dictated by the environment sqlx runs it in (see the migration's
    /// header). So it is applied the way `Migrate::apply` in sqlx-sqlite applies
    /// it: the entire file, as one multi-statement `execute`, on a single pinned
    /// connection, inside one transaction. Running it statement-by-statement off
    /// the pool would be *easier* and would prove nothing - a `SqlitePool` hands
    /// out different connections per call, so connection-scoped state would not
    /// carry over.
    ///
    /// The SQL comes from `include_str!` rather than being restated, so this
    /// cannot drift into passing against a stale copy.
    #[tokio::test]
    async fn migration_0004_preserves_rows_and_foreign_keys_on_a_populated_database() {
        use sqlx::{Acquire, Executor};

        let pool = banto_storage::connect_sqlite_memory()
            .await
            .expect("connect_sqlite_memory");
        let mut conn = pool.acquire().await.expect("acquire one pinned connection");

        // The schema as of 0003, i.e. what a deployed v1 database looks like.
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
        ] {
            conn.execute(sql)
                .await
                .unwrap_or_else(|e| panic!("pre-0004 migration {label} failed: {e}"));
        }

        // Non-default values throughout, so a column dropped or transposed by
        // the rebuild shows up as a mismatch rather than coinciding with a
        // default.
        conn.execute(
            "INSERT INTO plc_connections (id, name, protocol, host, port, unit_id, enabled) \
             VALUES (7, 'Line1 PLC', 'modbus-tcp', '192.168.1.10', 502, 3, 0)",
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
             VALUES (9, 'T1', 4, '40001', 'i16', 0, 100, 0, 50, 'degC', 2, 45, 1)",
        )
        .await
        .expect("seed tag");

        let migration = include_str!("../migrations/0004_plc_connections_allow_slmp.sql");
        let mut tx = conn.begin().await.expect("begin, as the migrator does");
        tx.execute(migration).await.expect("0004 should apply");
        tx.commit().await.expect("0004 should commit");

        // Every column of the existing connection survived, values and all.
        let row: (i64, String, String, String, i64, i64, bool) = sqlx::query_as(
            "SELECT id, name, protocol, host, port, unit_id, enabled FROM plc_connections",
        )
        .fetch_one(&mut *conn)
        .await
        .expect("the seeded connection should have been copied across");
        assert_eq!(
            row,
            (
                7,
                "Line1 PLC".to_string(),
                "modbus-tcp".to_string(),
                "192.168.1.10".to_string(),
                502,
                3,
                false
            )
        );

        // Both descendant rows are back, unchanged, with their foreign keys
        // resolving - this is what 0004's park-and-restore ordering protects.
        let group: (i64, String, i64, i64, bool) = sqlx::query_as(
            "SELECT id, name, plc_connection_id, period_ms, enabled FROM collection_groups",
        )
        .fetch_one(&mut *conn)
        .await
        .expect("the collection group should survive");
        assert_eq!(group, (4, "G1".to_string(), 7, 1000, true));

        let tag: (i64, String, i64, String, String, Option<f64>, i64) = sqlx::query_as(
            "SELECT id, name, collection_group_id, address, data_type, threshold_h, decimals \
             FROM tags",
        )
        .fetch_one(&mut *conn)
        .await
        .expect("the tag should survive");
        assert_eq!(
            tag,
            (
                9,
                "T1".to_string(),
                4,
                "40001".to_string(),
                "i16".to_string(),
                Some(45.0),
                2
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
                "INSERT INTO collection_groups (name, plc_connection_id, period_ms) \
                 VALUES ('orphan', 999, 1000)",
            )
            .execute(&mut *conn)
            .await
            .is_err(),
            "foreign keys should still be enforced after the migration"
        );

        // The point of the whole exercise: 'slmp' is now insertable, and
        // nothing else new is.
        sqlx::query(
            "INSERT INTO plc_connections (name, protocol, host, port) \
             VALUES ('MELSEC', 'slmp', '192.168.1.20', 5007)",
        )
        .execute(&mut *conn)
        .await
        .expect("slmp should be accepted after the rebuild");
        assert!(sqlx::query(
            "INSERT INTO plc_connections (name, protocol, host, port) \
             VALUES ('Nope', 'ethernet-ip', '192.168.1.30', 44818)",
        )
        .execute(&mut *conn)
        .await
        .is_err());
    }

    #[tokio::test]
    async fn create_rejects_out_of_range_port() {
        let svc = service().await;
        let mut input = sample_input("X");
        input.port = 0;
        let err = svc.create(input).await.unwrap_err();
        assert!(matches!(err, BantoError::Validation { .. }));

        let mut input2 = sample_input("Y");
        input2.port = 70000;
        let err2 = svc.create(input2).await.unwrap_err();
        match err2 {
            BantoError::Validation { field_errors } => assert_eq!(field_errors[0].field, "port"),
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_rejects_out_of_range_unit_id() {
        let svc = service().await;
        let mut input = sample_input("X");
        input.unit_id = 256;
        let err = svc.create(input).await.unwrap_err();
        match err {
            BantoError::Validation { field_errors } => assert_eq!(field_errors[0].field, "unitId"),
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_rejects_duplicate_name_with_friendly_message() {
        let svc = service().await;
        svc.create(sample_input("Dup")).await.unwrap();
        let err = svc.create(sample_input("Dup")).await.unwrap_err();
        match err {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors.len(), 1);
                assert_eq!(field_errors[0].field, "name");
                assert_eq!(field_errors[0].message, "既に使用されています");
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn update_changes_fields() {
        let svc = service().await;
        let created = svc.create(sample_input("Before")).await.unwrap();
        let mut input = sample_input("After");
        input.port = 503;
        let updated = svc
            .update(created.id, input)
            .await
            .expect("update should succeed");
        assert_eq!(updated.name, "After");
        assert_eq!(updated.port, 503);
    }

    #[tokio::test]
    async fn update_missing_id_is_not_found() {
        let svc = service().await;
        let err = svc.update(999, sample_input("X")).await.unwrap_err();
        assert!(
            matches!(err, BantoError::NotFound { resource, id } if resource == "plc_connections" && id == "999")
        );
    }

    #[tokio::test]
    async fn get_missing_id_is_not_found() {
        let svc = service().await;
        let err = svc.get(999).await.unwrap_err();
        assert!(matches!(err, BantoError::NotFound { .. }));
    }

    #[tokio::test]
    async fn delete_then_get_is_not_found() {
        let svc = service().await;
        let created = svc.create(sample_input("Doomed")).await.unwrap();
        svc.delete(created.id).await.expect("delete should succeed");
        let err = svc.get(created.id).await.unwrap_err();
        assert!(matches!(err, BantoError::NotFound { .. }));
    }

    #[tokio::test]
    async fn delete_missing_id_is_not_found() {
        let svc = service().await;
        let err = svc.delete(999).await.unwrap_err();
        assert!(matches!(err, BantoError::NotFound { .. }));
    }

    #[tokio::test]
    async fn delete_refuses_when_a_collection_group_references_it() {
        let svc = service().await;
        let conn = svc.create(sample_input("InUse")).await.unwrap();

        sqlx::query(
            "INSERT INTO collection_groups (name, plc_connection_id, period_ms, enabled) \
             VALUES ('G1', ?, 1000, 1)",
        )
        .bind(conn.id)
        .execute(&svc.pool)
        .await
        .unwrap();

        let err = svc.delete(conn.id).await.unwrap_err();
        match err {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "id");
                assert!(field_errors[0].message.contains('1'));
            }
            other => panic!("expected Validation, got {other:?}"),
        }

        // The row must still be there after the rejected delete.
        svc.get(conn.id).await.expect("connection should survive");
    }

    #[tokio::test]
    async fn list_filters_sorts_and_paginates_with_total_count() {
        let svc = service().await;
        for (name, port) in [("A", 501), ("B", 502), ("C", 503)] {
            let mut input = sample_input(name);
            input.port = port;
            svc.create(input).await.unwrap();
        }

        let result = svc
            .list(ListParams {
                sort: vec![SortState {
                    field: "port".to_string(),
                    direction: SortDirection::Desc,
                }],
                filters: vec![FilterState {
                    field: "port".to_string(),
                    op: FilterOp::Gte,
                    value: json!(502),
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
