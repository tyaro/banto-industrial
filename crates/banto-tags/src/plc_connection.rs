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
//!
//! ## `"virtual"` (T6-2, docs/tag-server-design.md §4.2/§4.3(a))
//!
//! A third protocol joined the other two in migration
//! `0007_plc_connections_allow_virtual.sql`: `"virtual"` names a connection
//! that speaks no wire protocol at all - it exists purely to give computed/
//! internal tags (design §4.2's `tag_kind = "computed"`/`"internal"`) a place
//! in the existing 3-tier `connection → group → tag` structure, so their
//! external name's reserved first segment (`calc`/`mem`) is realized as an
//! ordinary connection row rather than inventing a parallel namespace
//! mechanism. `banto-hub` auto-provisions exactly two such rows at startup
//! (named [`CALC_CONNECTION_NAME`]/[`MEM_CONNECTION_NAME`], created if
//! missing) - the registry's own `UNIQUE` constraint on `name` is what
//! prevents a user from ever creating a second connection also named `calc`
//! or `mem` (design's "予約は registry の UNIQUE 制約が自然に担保" - no
//! separate reservation table or special-cased CRUD guard needed). Which
//! `tag_kind` may live under a `"virtual"` connection - and specifically
//! under `calc` vs `mem` - is enforced by
//! [`crate::tag::TagService`] (it, not this module, can join a tag's group
//! back to its connection - see that service's own doc comment for why the
//! check lives there).
//!
//! `"virtual"` rows never reach a socket (`banto_collect::build_config`
//! excludes them from collection entirely; see that crate's own doc
//! comment), so [`validate_plc_connection_input`] relaxes `host`/`port` for
//! them: `host` may be empty (there is no host to dial) and `port` may be
//! `0` (there is no port to dial either) - both values that plc/slmp
//! connections still reject. No SQL-level relaxation was needed for this
//! (`host`/`port` were always plain `NOT NULL` columns with no `CHECK`, see
//! migration `0007`'s own header), so this is purely an application-layer
//! rule.
//!
//! ## `simulation` (T9-1, docs/ux-plan.md §1, 2026-08-06 オーナー決定)
//!
//! Migration `0008_plc_connections_add_simulation.sql` adds a `simulation`
//! column (`INTEGER NOT NULL DEFAULT 0`), surfaced as [`PlcConnection::simulation`]/
//! [`PlcConnectionInput::simulation`]. This is a per-connection flag
//! *independent of* `protocol` - the owner explicitly rejected a
//! `protocol = "simulation"` alternative because the whole point is
//! "開発→実機の切り替えがチェックボックス1つ" (flip one checkbox, keep every
//! other setting - name, groups, tags - untouched). When set,
//! `banto_collect` (not this crate) substitutes an in-process simulator's
//! loopback address for the connection's real `host`/`port` at collection
//! time; this crate only stores and validates the flag.
//!
//! **Interaction with `"virtual"` (this module's doc comment above)**:
//! [`validate_plc_connection_input`] rejects `simulation = true` combined
//! with `protocol = "virtual"` as a `FieldError` on `simulation`, rather than
//! silently ignoring the flag. A `"virtual"` connection dials nothing at all
//! (no socket, ever - see above), so "simulate this connection" is not
//! merely redundant for it, it is a category error: there is no real
//! connection for a simulator to stand in for. Rejecting outright (instead
//! of the alternative considered - silently treating `simulation` as a
//! no-op for `"virtual"` rows) follows this module's existing precedent for
//! `"virtual"`-specific illegal states (compare `update`/`delete`'s explicit
//! `FieldError`s for editing/deleting a reserved connection): a clear
//! validation error at write time is more honest than a flag that is
//! silently truthy in the database but never observed by anything.

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
pub const ALLOWED_PROTOCOLS: &[&str] = &["modbus-tcp", "slmp", "virtual"];

/// The one non-wire protocol (T6-2, this module's doc comment). Used both by
/// [`validate_plc_connection_input`] (relaxed host/port) and by
/// [`crate::tag::TagService`]'s `calc`/`mem` placement check.
pub const VIRTUAL_PROTOCOL: &str = "virtual";

/// The reserved connection name for computed tags (design §4.2's `calc`
/// external-name segment). `banto-hub` auto-provisions a `"virtual"`-protocol
/// row with this exact name at startup; [`crate::tag::TagService`] requires
/// every `tag_kind = "computed"` tag's group to live under it.
pub const CALC_CONNECTION_NAME: &str = "calc";

/// The reserved connection name for internal tags (design §4.2's `mem`
/// external-name segment) - the `internal`-tag sibling of
/// [`CALC_CONNECTION_NAME`].
pub const MEM_CONNECTION_NAME: &str = "mem";

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

/// T9-1: a `PlcConnectionInput` missing `simulation` (an old client, or a
/// direct Rust construction predating this field) builds as "not simulated" -
/// the same backward-compatible stance migration `0008`'s column default
/// takes (this module's doc comment, "simulation" section).
fn default_simulation() -> bool {
    false
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
    /// T9-1 (this module's doc comment, "simulation" section).
    pub simulation: bool,
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
    /// T9-1 (this module's doc comment, "simulation" section).
    #[serde(default = "default_simulation")]
    pub simulation: bool,
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

    // T6-2 (this module's doc comment "virtual"): a virtual connection dials
    // nothing, so `host`/`port` are meaningless - both checks are skipped for
    // it (host may be empty, port may be 0/anything), while every other
    // protocol keeps the original required-host / 1..=65535-port rules.
    let is_virtual = input.protocol == VIRTUAL_PROTOCOL;

    if !is_virtual && input.host.trim().is_empty() {
        errors.push(FieldError {
            field: "host".to_string(),
            message: required_message(),
        });
    }

    if !is_virtual && !(MIN_PORT..=MAX_PORT).contains(&input.port) {
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

    // T9-1 (this module's doc comment, "simulation" section): a "virtual"
    // connection dials nothing, so "simulate this connection" is a category
    // error, not a redundant-but-harmless flag - reject rather than silently
    // ignore it.
    if input.simulation && is_virtual {
        errors.push(FieldError {
            field: "simulation".to_string(),
            message: "予約接続（calc/mem）はシミュレーションモードにできません".to_string(),
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
        .column("simulation", "simulation")
}

const RESOURCE: &str = "plc_connections";
const COLUMNS: &str = "id, name, protocol, host, port, unit_id, enabled, simulation";

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
            "INSERT INTO plc_connections (name, protocol, host, port, unit_id, enabled, simulation) \
             VALUES (?, ?, ?, ?, ?, ?, ?) RETURNING {COLUMNS}"
        ))
        .bind(input.name.trim())
        .bind(&input.protocol)
        .bind(input.host.trim())
        .bind(input.port)
        .bind(input.unit_id)
        .bind(input.enabled)
        .bind(input.simulation)
        .fetch_one(&self.pool)
        .await
        .map_err(|err| map_write_error(err, "name", "", ""))
    }

    /// **T6-2 addition**: a `"virtual"`-protocol connection cannot be edited
    /// either (same reservation as [`Self::delete`] - the admin UI shows
    /// `calc`/`mem` as read-only rows, this is the API-layer enforcement of
    /// that "編集・削除不可" decision, this module's doc comment).
    pub async fn update(
        &self,
        id: i64,
        input: PlcConnectionInput,
    ) -> Result<PlcConnection, BantoError> {
        validate_plc_connection_input(&input)?;

        let existing_protocol: Option<String> =
            sqlx::query_scalar("SELECT protocol FROM plc_connections WHERE id = ?")
                .bind(id)
                .fetch_optional(&self.pool)
                .await
                .map_err(banto_storage::storage_error)?;
        if existing_protocol.as_deref() == Some(VIRTUAL_PROTOCOL) {
            return Err(BantoError::Validation {
                field_errors: vec![FieldError {
                    field: "id".to_string(),
                    message: "予約接続（calc/mem）は編集できません".to_string(),
                }],
            });
        }

        sqlx::query_as::<_, PlcConnection>(&format!(
            "UPDATE plc_connections SET name = ?, protocol = ?, host = ?, port = ?, unit_id = ?, enabled = ?, simulation = ? \
             WHERE id = ? RETURNING {COLUMNS}"
        ))
        .bind(input.name.trim())
        .bind(&input.protocol)
        .bind(input.host.trim())
        .bind(input.port)
        .bind(input.unit_id)
        .bind(input.enabled)
        .bind(input.simulation)
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
    ///
    /// **T6-2 addition**: a `"virtual"`-protocol connection ([`CALC_CONNECTION_NAME`]/
    /// [`MEM_CONNECTION_NAME`], this module's doc comment) can never be
    /// deleted through this method, regardless of whether any group
    /// currently references it - unlike the in-use guard below, which only
    /// bites once something is attached. Without this, an operator could
    /// delete an empty `calc`/`mem` row (e.g. right after `banto-hub`
    /// auto-provisions it, before any computed/internal tag exists) and the
    /// reserved namespace would be gone until the next process restart
    /// re-provisions it - this rejects that path outright rather than
    /// relying on self-healing at the next boot (design test plan #6:
    /// "通常 CRUD で calc の削除が拒否されるか...実装した保護レベルをテスト
    /// で固定").
    pub async fn delete(&self, id: i64) -> Result<(), BantoError> {
        let protocol: Option<String> =
            sqlx::query_scalar("SELECT protocol FROM plc_connections WHERE id = ?")
                .bind(id)
                .fetch_optional(&self.pool)
                .await
                .map_err(banto_storage::storage_error)?;
        if protocol.as_deref() == Some(VIRTUAL_PROTOCOL) {
            return Err(BantoError::Validation {
                field_errors: vec![FieldError {
                    field: "id".to_string(),
                    message: "予約接続（calc/mem）は削除できません".to_string(),
                }],
            });
        }

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
            simulation: false,
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

    // --- T6-2: "virtual" protocol (migration 0007) ------------------------

    /// The application-layer half of the "virtual" relaxation (this module's
    /// doc comment): empty `host` and `port = 0` are accepted for a
    /// `"virtual"` connection, where every other protocol would reject both.
    #[tokio::test]
    async fn virtual_connection_accepts_empty_host_and_zero_port() {
        let svc = service().await;
        let created = svc
            .create(PlcConnectionInput {
                name: CALC_CONNECTION_NAME.to_string(),
                protocol: VIRTUAL_PROTOCOL.to_string(),
                host: String::new(),
                port: 0,
                unit_id: 1,
                enabled: true,
                simulation: false,
            })
            .await
            .expect("a virtual connection should accept empty host / port 0");
        assert_eq!(created.host, "");
        assert_eq!(created.port, 0);
    }

    /// A non-virtual protocol must keep rejecting empty host / port 0 - the
    /// relaxation is specific to `"virtual"`, not a general loosening.
    #[tokio::test]
    async fn a_non_virtual_connection_still_rejects_empty_host_and_zero_port() {
        let svc = service().await;
        let mut input = sample_input("X");
        input.host = String::new();
        input.port = 0;
        let err = svc.create(input).await.unwrap_err();
        match err {
            BantoError::Validation { field_errors } => {
                let fields: Vec<&str> = field_errors.iter().map(|e| e.field.as_str()).collect();
                assert!(fields.contains(&"host"));
                assert!(fields.contains(&"port"));
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    /// T6-2 test plan #6: a `"virtual"` connection cannot be deleted through
    /// the normal CRUD path, even when it has zero groups attached (unlike
    /// the generic in-use guard, which only bites once something
    /// references it).
    #[tokio::test]
    async fn delete_refuses_a_virtual_connection_even_with_no_groups_attached() {
        let svc = service().await;
        let calc = svc
            .create(PlcConnectionInput {
                name: CALC_CONNECTION_NAME.to_string(),
                protocol: VIRTUAL_PROTOCOL.to_string(),
                host: String::new(),
                port: 0,
                unit_id: 1,
                enabled: true,
                simulation: false,
            })
            .await
            .unwrap();

        let err = svc.delete(calc.id).await.unwrap_err();
        match err {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "id");
                assert!(field_errors[0].message.contains("予約接続"));
            }
            other => panic!("expected Validation, got {other:?}"),
        }
        // Still there.
        svc.get(calc.id).await.expect("calc should survive");
    }

    /// The API-layer twin: a `"virtual"` connection also cannot be edited.
    #[tokio::test]
    async fn update_refuses_a_virtual_connection() {
        let svc = service().await;
        let mem = svc
            .create(PlcConnectionInput {
                name: MEM_CONNECTION_NAME.to_string(),
                protocol: VIRTUAL_PROTOCOL.to_string(),
                host: String::new(),
                port: 0,
                unit_id: 1,
                enabled: true,
                simulation: false,
            })
            .await
            .unwrap();

        let err = svc
            .update(mem.id, sample_input("renamed"))
            .await
            .unwrap_err();
        match err {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "id");
                assert!(field_errors[0].message.contains("予約接続"));
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    /// A non-virtual connection is unaffected by the new guards (delete
    /// still only cares about in-use groups, update still succeeds).
    #[tokio::test]
    async fn delete_and_update_are_unaffected_for_non_virtual_connections() {
        let svc = service().await;
        let conn = svc.create(sample_input("Ordinary")).await.unwrap();
        svc.update(conn.id, sample_input("Renamed"))
            .await
            .expect("update should still work for a non-virtual connection");
        svc.delete(conn.id)
            .await
            .expect("delete should still work for a non-virtual connection");
    }

    /// Migration 0007 rebuilds `plc_connections` again (SQLite cannot `ALTER`
    /// a `CHECK`) - the direct sibling of
    /// `migration_0004_preserves_rows_and_foreign_keys_on_a_populated_database`,
    /// but against the FULL post-0006 `tags` shape (string_length/writable/
    /// tag_kind/expression/retain all present) - a stale column list here
    /// would silently truncate every existing tag row, exactly the risk this
    /// migration's own header warns about.
    #[tokio::test]
    async fn migration_0007_preserves_rows_and_foreign_keys_on_a_populated_database() {
        use sqlx::{Acquire, Executor};

        let pool = banto_storage::connect_sqlite_memory()
            .await
            .expect("connect_sqlite_memory");
        let mut conn = pool.acquire().await.expect("acquire one pinned connection");

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
            (
                "0006",
                include_str!("../migrations/0006_tags_writable_kind.sql"),
            ),
        ] {
            conn.execute(sql)
                .await
                .unwrap_or_else(|e| panic!("pre-0007 migration {label} failed: {e}"));
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
            "INSERT INTO tags (\
                id, name, collection_group_id, address, data_type, string_length, \
                raw_lo, raw_hi, eng_lo, eng_hi, unit, decimals, threshold_h, enabled, \
                writable, tag_kind, expression, retain\
             ) VALUES (\
                9, 'T1', 4, 'D100', 'i16', NULL, \
                0, 100, 0, 50, 'degC', 2, 45, 1, \
                1, 'plc', NULL, 0\
             )",
        )
        .await
        .expect("seed tag");

        let migration = include_str!("../migrations/0007_plc_connections_allow_virtual.sql");
        let mut tx = conn.begin().await.expect("begin, as the migrator does");
        tx.execute(migration).await.expect("0007 should apply");
        tx.commit().await.expect("0007 should commit");

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
                "slmp".to_string(),
                "192.168.1.10".to_string(),
                5007,
                3,
                false
            )
        );

        #[allow(clippy::type_complexity)]
        let tag: (i64, String, i64, String, bool, String, Option<String>, bool) = sqlx::query_as(
            "SELECT id, name, collection_group_id, address, writable, tag_kind, expression, \
             retain FROM tags",
        )
        .fetch_one(&mut *conn)
        .await
        .expect("the seeded tag should survive with every T2/T6 column intact");
        assert_eq!(
            tag,
            (
                9,
                "T1".to_string(),
                4,
                "D100".to_string(),
                true,
                "plc".to_string(),
                None,
                false
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

        // The point of the whole exercise: 'virtual' (with empty host/port 0)
        // is now insertable.
        sqlx::query(
            "INSERT INTO plc_connections (name, protocol, host, port) \
             VALUES ('calc', 'virtual', '', 0)",
        )
        .execute(&mut *conn)
        .await
        .expect("virtual should be accepted after the rebuild");
        assert!(sqlx::query(
            "INSERT INTO plc_connections (name, protocol, host, port) \
             VALUES ('Nope', 'ethernet-ip', '192.168.1.30', 44818)",
        )
        .execute(&mut *conn)
        .await
        .is_err());
    }

    // --- T9-1: "simulation" column (migration 0008) ------------------------

    /// A `PlcConnectionInput` built with `simulation: false` (this file's
    /// `sample_input`) round-trips as `false` - the baseline every other test
    /// in this module already exercises implicitly, stated explicitly here as
    /// the counterpart to `simulation_flag_round_trips_through_update`.
    #[tokio::test]
    async fn simulation_defaults_to_false_and_round_trips() {
        let svc = service().await;
        let created = svc.create(sample_input("Sim1")).await.unwrap();
        assert!(!created.simulation);

        let fetched = svc.get(created.id).await.unwrap();
        assert!(!fetched.simulation);
    }

    /// `simulation: true` is accepted for an ordinary (non-`"virtual"`)
    /// connection and persists through both `create` and a later `update`.
    #[tokio::test]
    async fn simulation_flag_round_trips_through_update() {
        let svc = service().await;
        let mut input = sample_input("Sim2");
        input.simulation = true;
        let created = svc.create(input).await.unwrap();
        assert!(created.simulation);
        assert!(svc.get(created.id).await.unwrap().simulation);

        let mut off = sample_input("Sim2");
        off.simulation = false;
        let updated = svc.update(created.id, off).await.unwrap();
        assert!(!updated.simulation);
    }

    /// This module's doc comment ("simulation" section): `simulation = true`
    /// combined with `protocol = "virtual"` is a category error (a virtual
    /// connection dials nothing for a simulator to stand in for) and is
    /// rejected as a `FieldError` on `simulation`, not silently accepted.
    #[tokio::test]
    async fn create_rejects_simulation_on_a_virtual_connection() {
        let svc = service().await;
        let err = svc
            .create(PlcConnectionInput {
                name: CALC_CONNECTION_NAME.to_string(),
                protocol: VIRTUAL_PROTOCOL.to_string(),
                host: String::new(),
                port: 0,
                unit_id: 1,
                enabled: true,
                simulation: true,
            })
            .await
            .unwrap_err();
        match err {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "simulation");
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    /// The reverse combination (`simulation: false`, `protocol: "virtual"`)
    /// stays accepted - this is exactly `virtual_connection_accepts_empty_host_and_zero_port`
    /// above, restated to pin down that the new check is specific to
    /// `simulation = true`, not a general tightening of virtual-connection
    /// validation.
    #[tokio::test]
    async fn a_non_simulated_virtual_connection_is_still_accepted() {
        let svc = service().await;
        let created = svc
            .create(PlcConnectionInput {
                name: MEM_CONNECTION_NAME.to_string(),
                protocol: VIRTUAL_PROTOCOL.to_string(),
                host: String::new(),
                port: 0,
                unit_id: 1,
                enabled: true,
                simulation: false,
            })
            .await
            .expect("simulation: false must not be affected by the new check");
        assert!(!created.simulation);
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
