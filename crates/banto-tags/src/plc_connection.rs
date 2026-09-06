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
//!
//! ## `"postgres"` (S1, docs/banto-hub-external-db-design.md §4.1・§6-4,
//! 2026-09-06 オーナー決定「案A: 既存3階層の流用」)
//!
//! A fourth protocol joins the other three in migration
//! `0014_plc_connections_allow_postgres.sql`: `"postgres"` names a
//! connection to an external PostgreSQL database, backing the future DB
//! Source (#228)/DB Sink (#229) rather than a PLC. Unlike `"virtual"`, a
//! `"postgres"` row *does* dial something real (`host`/`port` stay required,
//! same rule as `"modbus-tcp"`/`"slmp"`) - what it never does is join the PLC
//! collection pipeline: `banto_collect::build_config_from` excludes it from
//! collection entirely (same treatment as `"virtual"`, see that crate's own
//! doc comment), and `banto_broker`'s `DRIVERS` table has no entry for it
//! either, so nothing ever tries to open a broker session against it.
//!
//! Three columns exist only for this protocol: [`PlcConnection::database`]/
//! [`PlcConnection::username`]/[`PlcConnection::password`] (§2.2's "v1 は
//! MQTT と同じ平文" decision - plaintext storage, no encryption/keyring in
//! this version). [`validate_plc_connection_input`] enforces the two
//! directions of one rule: `database`/`username` are required (non-empty,
//! trimmed, capped) when `protocol == `[`POSTGRES_PROTOCOL`], and all three
//! columns must be empty/`None` for every other protocol - a `"modbus-tcp"`
//! row can never carry a stray DB password.
//!
//! `unit_id`/`word_order`/`simulation` are meaningless for a DB connection
//! (they are wire-protocol/PLC concepts) - but instead of `"virtual"`'s
//! reject-outright stance for `simulation`, a `"postgres"`
//! [`PlcConnectionInput`] simply has these three fields **silently
//! normalized** to their column defaults before validation/write
//! ([`normalize_postgres_input`]), regardless of what the caller sent. This
//! is the more lenient of the two treatments this module uses for
//! protocol-inapplicable fields (contrast `"virtual"`'s `host`/`port`
//! relaxation, which still validates the caller's value when one is given,
//! and `simulation`'s outright rejection for `"virtual"`) - chosen because a
//! generic UI/MCP caller that always sends its modbus-tcp defaults
//! (`unitId: 1`, `wordOrder: "low_high"`) for every protocol should not need
//! postgres-specific branching just to avoid a validation error.
//!
//! **In this slice (S1a), no [`crate::collection_group::CollectionGroup`]
//! may be created under a `"postgres"` connection** -
//! `crate::collection_group::CollectionGroupService::create`/`create_tx`/
//! `update`/`update_tx` reject it with a `FieldError` on `plcConnectionId`
//! (S2, the DB Source polling engine itself, lifts this once `query_sql`
//! and `tag_kind = "db"` exist to give such a group somewhere to put its
//! results).

use banto_core::{BantoError, FieldError, ListParams, ListResult};
use banto_storage::ColumnMap;
use serde::{Deserialize, Serialize};
use sqlx::{QueryBuilder, Sqlite, SqliteConnection, SqlitePool};

use crate::support::{
    map_write_error, max_length_message, range_message, required_message, NAME_ALREADY_USED,
};

/// Protocols accepted in `plc_connections.protocol` today. Mirrors the SQL
/// `CHECK` - as widened by `migrations/0004_plc_connections_allow_slmp.sql`,
/// not `0001`'s original - and is kept in Rust too so
/// [`validate_plc_connection_input`] produces a friendly `FieldError` instead
/// of surfacing the raw SQLite CHECK constraint violation. The two must be
/// changed together; `every_allowed_protocol_is_accepted_by_the_sql_check` is
/// the tripwire if they drift.
pub const ALLOWED_PROTOCOLS: &[&str] = &["modbus-tcp", "slmp", "virtual", "postgres"];

/// Values accepted in `plc_connections.word_order` (P3-b, 監査指摘
/// 2026-08-12). Mirrors the SQL `CHECK` added by
/// `migrations/0010_plc_connections_add_word_order.sql`, same
/// two-copies-of-one-rule relationship [`ALLOWED_PROTOCOLS`] has with its own
/// `CHECK` - see that constant's doc comment for why both exist and
/// `every_allowed_word_order_is_accepted_by_the_sql_check` for the tripwire
/// that keeps them in sync.
///
/// The two strings are the wire form of `banto_plc::decode::WordOrder`'s two
/// variants - not re-exported as that enum directly, because `banto-tags` has
/// no dependency on `banto-plc` (this crate is the registry, not a protocol
/// client) and because `protocol` already established the "TEXT + CHECK, not
/// a Rust enum" convention for exactly this kind of small closed vocabulary
/// (this module's doc comment). [`crate::plc_connection`]'s only job is to
/// store and validate the string; `banto-broker`
/// (`SessionDirectory::ensure_connection`) is what actually parses it into a
/// `WordOrder` when building an `SlmpConfig`.
pub const ALLOWED_WORD_ORDERS: &[&str] = &["low_high", "high_low"];

/// The one non-wire protocol (T6-2, this module's doc comment). Used both by
/// [`validate_plc_connection_input`] (relaxed host/port) and by
/// [`crate::tag::TagService`]'s `calc`/`mem` placement check.
pub const VIRTUAL_PROTOCOL: &str = "virtual";

/// The DB Source/Sink protocol (S1, this module's doc comment "`\"postgres\"`"
/// section). Used by [`validate_plc_connection_input`] (required
/// `database`/`username`, normalized `unit_id`/`word_order`/`simulation`),
/// [`PlcConnection::is_db_source`], and by
/// `crate::collection_group::CollectionGroupService`'s group-placement guard.
pub const POSTGRES_PROTOCOL: &str = "postgres";

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
// S1 (this module's doc comment, "`\"postgres\"`" section): generous but
// bounded caps for `database`/`username` - PostgreSQL identifiers themselves
// cap at 63 bytes, but `database`/`username` here are the *client-side*
// connection parameters (sqlx quotes them, they need not be bare
// identifiers), so the cap is a plain anti-abuse bound, not a PostgreSQL
// grammar limit. Matches [`MAX_NAME_LEN`]'s role for `name`.
const MAX_DATABASE_LEN: usize = 128;
const MAX_USERNAME_LEN: usize = 128;

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

/// P3-b: a `PlcConnectionInput` missing `wordOrder` (an old client, or a
/// direct Rust construction predating this field) builds with MELSEC's own
/// low-word-first order - the same value migration `0010`'s column default
/// takes, and the same value `banto_plc::slmp::SlmpConfig::default()` already
/// used before this field existed, so an old client is unaffected.
fn default_word_order() -> String {
    "low_high".to_string()
}

/// S1 (this module's doc comment, "`\"postgres\"`" section): a `"postgres"`
/// connection has no meaningful `unit_id`/`word_order`/`simulation` (all
/// three are wire-protocol/PLC concepts), so a payload's values for them are
/// silently replaced with the column defaults - not validated, not rejected,
/// just ignored. Called before [`validate_plc_connection_input`] by every
/// [`PlcConnectionService`] write method, so those three fields' ordinary
/// range/enum checks below never actually have anything to reject for a
/// `"postgres"` row (they always see the default, which is always valid).
///
/// A no-op for every other protocol (returns `input` unchanged).
fn normalize_postgres_input(mut input: PlcConnectionInput) -> PlcConnectionInput {
    if input.protocol == POSTGRES_PROTOCOL {
        input.unit_id = default_unit_id();
        input.word_order = default_word_order();
        input.simulation = false;
    }
    input
}

/// [`PlcConnectionService::create`]'s password rule (this crate's
/// `PlcConnectionInput::password` doc comment, "create" case): `None` and
/// `Some("")` both mean "no password yet" (stored as SQL `NULL`); any other
/// `Some(s)` is stored verbatim.
fn stored_password_for_create(password: &Option<String>) -> Option<String> {
    match password {
        Some(s) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

/// [`PlcConnectionService::update`]'s password rule (this crate's
/// `PlcConnectionInput::password` doc comment, "update" case): `None` keeps
/// `existing`, `Some("")` clears it, any other `Some(s)` replaces it.
fn stored_password_for_update(
    password: &Option<String>,
    existing: Option<String>,
) -> Option<String> {
    match password {
        None => existing,
        Some(s) if s.is_empty() => None,
        Some(s) => Some(s.clone()),
    }
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
    /// P3-b (this module's doc comment, [`ALLOWED_WORD_ORDERS`]). Meaningful
    /// for `"slmp"` connections only - `"modbus-tcp"`/`"virtual"` rows simply
    /// carry the column's default, the same treatment `unit_id` gets for
    /// SLMP (this module's doc comment, above).
    pub word_order: String,
    /// S1 (this module's doc comment, "`\"postgres\"`" section): the DB name
    /// a `"postgres"` connection targets. Always `None` for every other
    /// protocol.
    pub database: Option<String>,
    /// S1: the DB user a `"postgres"` connection authenticates as. Always
    /// `None` for every other protocol.
    pub username: Option<String>,
    /// S1: the DB password, stored **in plain text** (§2.2 of
    /// docs/banto-hub-external-db-design.md, 2026-09-06 決定「v1 は MQTT と
    /// 同じ平文」- same posture as `apps/banto-hub/core/src/settings.rs`'s
    /// MQTT password). Always `None` for every other protocol.
    ///
    /// **Never serialize this field to an external caller.** `PlcConnection`
    /// derives `Serialize` for internal use (config-package export, the
    /// registry's own round-trip), but every REST/MCP surface that reads a
    /// connection back (`apps/banto-hub/core/src/rest.rs`'s response DTO,
    /// `crate::mcp`'s `list_connections`/`create_connection` results) must
    /// redact this to a `passwordSet: bool` before it leaves the process -
    /// see this crate's `PlcConnectionService::update`/`update_tx` doc
    /// comment for the write-side semantics this field participates in.
    pub password: Option<String>,
}

impl PlcConnection {
    /// S1 (this module's doc comment, "`\"postgres\"`" section): whether this
    /// row is a DB Source/Sink connection (`protocol == `[`POSTGRES_PROTOCOL`])
    /// rather than a wire-protocol PLC connection. Downstream code that must
    /// keep DB connections out of a pipeline built for a socket - the PLC
    /// collection filter in `banto_collect::config::build_config_from`, the
    /// group-placement guard in
    /// `crate::collection_group::CollectionGroupService` - uses this instead
    /// of re-deriving the equality check inline.
    pub fn is_db_source(&self) -> bool {
        self.protocol == POSTGRES_PROTOCOL
    }
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
    /// P3-b. See [`PlcConnection::word_order`]'s doc comment.
    #[serde(default = "default_word_order")]
    pub word_order: String,
    /// S1. See [`PlcConnection::database`]'s doc comment. Required
    /// (non-empty after trim) when `protocol == `[`POSTGRES_PROTOCOL`],
    /// forbidden (must be `None`/empty) otherwise.
    #[serde(default)]
    pub database: Option<String>,
    /// S1. See [`PlcConnection::username`]'s doc comment. Same
    /// required-for-postgres/forbidden-otherwise rule as [`Self::database`].
    #[serde(default)]
    pub username: Option<String>,
    /// S1. See [`PlcConnection::password`]'s doc comment. Optional even for
    /// `protocol == `[`POSTGRES_PROTOCOL`] (a DB Source may be configured
    /// before its password is known), forbidden (must be `None`/empty)
    /// otherwise.
    ///
    /// **This field's meaning is not the same on every call** -
    /// [`PlcConnectionService::create`]/[`PlcConnectionService::create_tx`]
    /// treat it as a plain "the password to store" (`None`/`Some("")` both
    /// mean "no password yet"). [`PlcConnectionService::update`]/
    /// [`PlcConnectionService::update_tx`] treat it as a **tri-state PATCH**
    /// instead, because a bare `PUT`-style "always echo the current value"
    /// contract would force every editor UI/MCP caller to have already
    /// fetched (and therefore received) the plaintext password just to
    /// leave it unchanged - exactly the exposure [`PlcConnection::password`]'s
    /// own doc comment says to avoid:
    ///
    /// - `None` (omitted, or explicit JSON `null`): **keep** the row's
    ///   current password unchanged.
    /// - `Some("")` (empty string): **clear** the stored password (set the
    ///   column to `NULL`).
    /// - `Some(s)` for non-empty `s`: **replace** the stored password with
    ///   `s`.
    #[serde(default)]
    pub password: Option<String>,
}

/// Validate a [`PlcConnectionInput`]: `name`/`host` trimmed non-empty (name
/// additionally capped at `MAX_NAME_LEN`), `protocol` in [`ALLOWED_PROTOCOLS`],
/// `port` in `1..=65535`, `unit_id` in `0..=255`, `word_order` in
/// [`ALLOWED_WORD_ORDERS`]. Returns every violation, not just the first
/// (mirrors `items::validate_item_input` in the banto template repo).
///
/// S1 (this module's doc comment, "`\"postgres\"`" section) adds one more
/// pair of rules, applied **after** the caller has already run the input
/// through [`normalize_postgres_input`] (so `unit_id`/`word_order`/
/// `simulation` never fail their own checks above for a `"postgres"` row):
/// `database`/`username` are required (trimmed non-empty, capped) when
/// `protocol == `[`POSTGRES_PROTOCOL`], and `database`/`username`/`password`
/// must all be empty/`None` for every other protocol.
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

    // P3-b: validated for every protocol, not just "slmp" - same stance
    // `unit_id` already takes (validated even though SLMP never reads it).
    // Keeping it unconditional avoids a second implicit "which values are
    // legal" rule that only applies sometimes; a modbus-tcp/virtual row
    // simply carries whatever value it was given (normally the default) and
    // nothing ever reads it.
    if !ALLOWED_WORD_ORDERS.contains(&input.word_order.as_str()) {
        errors.push(FieldError {
            field: "wordOrder".to_string(),
            message: format!(
                "ワード順は {} のいずれかです",
                ALLOWED_WORD_ORDERS.join(", ")
            ),
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

    // S1 (this module's doc comment, "`\"postgres\"`" section): database/
    // username required (and capped) for postgres, forbidden for everyone
    // else. password has no format rule of its own (any string, including
    // empty, is acceptable for postgres - see `PlcConnectionInput::password`'s
    // doc comment) but is still forbidden outside postgres, same as the
    // other two.
    let is_postgres = input.protocol == POSTGRES_PROTOCOL;
    if is_postgres {
        let database = input.database.as_deref().unwrap_or("").trim();
        if database.is_empty() {
            errors.push(FieldError {
                field: "database".to_string(),
                message: required_message(),
            });
        } else if database.chars().count() > MAX_DATABASE_LEN {
            errors.push(FieldError {
                field: "database".to_string(),
                message: max_length_message(MAX_DATABASE_LEN),
            });
        }

        let username = input.username.as_deref().unwrap_or("").trim();
        if username.is_empty() {
            errors.push(FieldError {
                field: "username".to_string(),
                message: required_message(),
            });
        } else if username.chars().count() > MAX_USERNAME_LEN {
            errors.push(FieldError {
                field: "username".to_string(),
                message: max_length_message(MAX_USERNAME_LEN),
            });
        }
    } else {
        if input
            .database
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty())
        {
            errors.push(FieldError {
                field: "database".to_string(),
                message: "postgres接続以外では指定できません".to_string(),
            });
        }
        if input
            .username
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty())
        {
            errors.push(FieldError {
                field: "username".to_string(),
                message: "postgres接続以外では指定できません".to_string(),
            });
        }
        if input.password.as_deref().is_some_and(|s| !s.is_empty()) {
            errors.push(FieldError {
                field: "password".to_string(),
                message: "postgres接続以外では指定できません".to_string(),
            });
        }
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
        .column("wordOrder", "word_order")
        .column("database", "database")
        .column("username", "username")
    // S1: `password` is deliberately NOT listed here - filtering/sorting
    // a list by password value would let a caller confirm a guess about
    // its content through a side channel (an otherwise-plain `list`
    // parameter), which nothing else in this API needs to support.
}

const RESOURCE: &str = "plc_connections";
const COLUMNS: &str =
    "id, name, protocol, host, port, unit_id, enabled, simulation, word_order, database, username, password";

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

        let mut rows_builder: QueryBuilder<Sqlite> =
            QueryBuilder::new(format!("SELECT {COLUMNS} FROM plc_connections"));
        banto_storage::list_query::sqlite::apply_list_params(&mut rows_builder, &columns, &params)?;
        let rows: Vec<PlcConnection> = rows_builder
            .build_query_as::<PlcConnection>()
            .fetch_all(&self.pool)
            .await
            .map_err(banto_storage::storage_error)?;

        let mut count_builder: QueryBuilder<Sqlite> =
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
        // AssertSqlSafe: 補間されるのは COLUMNS 定数（本ファイル内の固定文字列）
        // のみで、外部入力は含まれない。id はプレースホルダでバインドする。
        sqlx::query_as::<_, PlcConnection>(sqlx::AssertSqlSafe(format!(
            "SELECT {COLUMNS} FROM plc_connections WHERE id = ?"
        )))
        .bind(id)
        .fetch_one(&self.pool)
        .await
        .map_err(|err| banto_storage::not_found(err, RESOURCE, id.to_string()))
    }

    pub async fn create(&self, input: PlcConnectionInput) -> Result<PlcConnection, BantoError> {
        let input = normalize_postgres_input(input);
        validate_plc_connection_input(&input)?;
        let is_postgres = input.protocol == POSTGRES_PROTOCOL;
        let stored_database =
            is_postgres.then(|| input.database.as_deref().unwrap_or("").trim().to_string());
        let stored_username =
            is_postgres.then(|| input.username.as_deref().unwrap_or("").trim().to_string());
        let stored_password = is_postgres
            .then(|| stored_password_for_create(&input.password))
            .flatten();
        // AssertSqlSafe: get() と同じ理由 - COLUMNS 定数のみを埋め込む固定
        // 文字列。値はすべてプレースホルダでバインドする。
        sqlx::query_as::<_, PlcConnection>(sqlx::AssertSqlSafe(format!(
            "INSERT INTO plc_connections (name, protocol, host, port, unit_id, enabled, simulation, word_order, database, username, password) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING {COLUMNS}"
        )))
        .bind(input.name.trim())
        .bind(&input.protocol)
        .bind(input.host.trim())
        .bind(input.port)
        .bind(input.unit_id)
        .bind(input.enabled)
        .bind(input.simulation)
        .bind(&input.word_order)
        .bind(stored_database)
        .bind(stored_username)
        .bind(stored_password)
        .fetch_one(&self.pool)
        .await
        .map_err(|err| map_write_error(err, "name", NAME_ALREADY_USED, "", ""))
    }

    /// Transaction-compatible counterpart of [`Self::create`]. The caller
    /// owns the transaction and may run additional registry preflight queries
    /// before deciding to commit or roll it back.
    pub async fn create_tx(
        &self,
        connection: &mut SqliteConnection,
        input: PlcConnectionInput,
    ) -> Result<PlcConnection, BantoError> {
        let input = normalize_postgres_input(input);
        validate_plc_connection_input(&input)?;
        let is_postgres = input.protocol == POSTGRES_PROTOCOL;
        let stored_database =
            is_postgres.then(|| input.database.as_deref().unwrap_or("").trim().to_string());
        let stored_username =
            is_postgres.then(|| input.username.as_deref().unwrap_or("").trim().to_string());
        let stored_password = is_postgres
            .then(|| stored_password_for_create(&input.password))
            .flatten();
        // AssertSqlSafe: get() と同じ理由 - COLUMNS 定数のみを埋め込む固定
        // 文字列。値はすべてプレースホルダでバインドする。
        sqlx::query_as::<_, PlcConnection>(sqlx::AssertSqlSafe(format!(
            "INSERT INTO plc_connections (name, protocol, host, port, unit_id, enabled, simulation, word_order, database, username, password) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING {COLUMNS}"
        )))
        .bind(input.name.trim())
        .bind(&input.protocol)
        .bind(input.host.trim())
        .bind(input.port)
        .bind(input.unit_id)
        .bind(input.enabled)
        .bind(input.simulation)
        .bind(&input.word_order)
        .bind(stored_database)
        .bind(stored_username)
        .bind(stored_password)
        .fetch_one(&mut *connection)
        .await
        .map_err(|err| map_write_error(err, "name", NAME_ALREADY_USED, "", ""))
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
        let input = normalize_postgres_input(input);
        validate_plc_connection_input(&input)?;

        let existing: Option<(String, Option<String>)> =
            sqlx::query_as("SELECT protocol, password FROM plc_connections WHERE id = ?")
                .bind(id)
                .fetch_optional(&self.pool)
                .await
                .map_err(banto_storage::storage_error)?;
        if existing.as_ref().map(|(protocol, _)| protocol.as_str()) == Some(VIRTUAL_PROTOCOL) {
            return Err(BantoError::Validation {
                field_errors: vec![FieldError {
                    field: "id".to_string(),
                    message: "予約接続（calc/mem）は編集できません".to_string(),
                }],
            });
        }
        let existing_password = existing.and_then(|(_, password)| password);

        let is_postgres = input.protocol == POSTGRES_PROTOCOL;
        let stored_database =
            is_postgres.then(|| input.database.as_deref().unwrap_or("").trim().to_string());
        let stored_username =
            is_postgres.then(|| input.username.as_deref().unwrap_or("").trim().to_string());
        let stored_password = if is_postgres {
            stored_password_for_update(&input.password, existing_password)
        } else {
            None
        };

        // AssertSqlSafe: get() と同じ理由 - COLUMNS 定数のみを埋め込む固定
        // 文字列。値はすべてプレースホルダでバインドする。
        sqlx::query_as::<_, PlcConnection>(sqlx::AssertSqlSafe(format!(
            "UPDATE plc_connections SET name = ?, protocol = ?, host = ?, port = ?, unit_id = ?, enabled = ?, simulation = ?, word_order = ?, database = ?, username = ?, password = ? \
             WHERE id = ? RETURNING {COLUMNS}"
        )))
        .bind(input.name.trim())
        .bind(&input.protocol)
        .bind(input.host.trim())
        .bind(input.port)
        .bind(input.unit_id)
        .bind(input.enabled)
        .bind(input.simulation)
        .bind(&input.word_order)
        .bind(stored_database)
        .bind(stored_username)
        .bind(stored_password)
        .bind(id)
        .fetch_one(&self.pool)
        .await
        .map_err(|err| match err {
            sqlx::Error::RowNotFound => BantoError::NotFound {
                resource: RESOURCE.to_string(),
                id: id.to_string(),
            },
            other => map_write_error(other, "name", NAME_ALREADY_USED, "", ""),
        })
    }

    /// Transaction-compatible counterpart of [`Self::update`].
    pub async fn update_tx(
        &self,
        connection: &mut SqliteConnection,
        id: i64,
        input: PlcConnectionInput,
    ) -> Result<PlcConnection, BantoError> {
        let input = normalize_postgres_input(input);
        validate_plc_connection_input(&input)?;
        let existing: Option<(String, Option<String>)> =
            sqlx::query_as("SELECT protocol, password FROM plc_connections WHERE id = ?")
                .bind(id)
                .fetch_optional(&mut *connection)
                .await
                .map_err(banto_storage::storage_error)?;
        if existing.as_ref().map(|(protocol, _)| protocol.as_str()) == Some(VIRTUAL_PROTOCOL) {
            return Err(BantoError::Validation {
                field_errors: vec![FieldError {
                    field: "id".to_string(),
                    message: "予約接続（calc/mem）は編集できません".to_string(),
                }],
            });
        }
        let existing_password = existing.and_then(|(_, password)| password);

        let is_postgres = input.protocol == POSTGRES_PROTOCOL;
        let stored_database =
            is_postgres.then(|| input.database.as_deref().unwrap_or("").trim().to_string());
        let stored_username =
            is_postgres.then(|| input.username.as_deref().unwrap_or("").trim().to_string());
        let stored_password = if is_postgres {
            stored_password_for_update(&input.password, existing_password)
        } else {
            None
        };

        // AssertSqlSafe: get() と同じ理由 - COLUMNS 定数のみを埋め込む固定
        // 文字列。値はすべてプレースホルダでバインドする。
        sqlx::query_as::<_, PlcConnection>(sqlx::AssertSqlSafe(format!(
            "UPDATE plc_connections SET name = ?, protocol = ?, host = ?, port = ?, unit_id = ?, enabled = ?, simulation = ?, word_order = ?, database = ?, username = ?, password = ? \
             WHERE id = ? RETURNING {COLUMNS}"
        )))
        .bind(input.name.trim())
        .bind(&input.protocol)
        .bind(input.host.trim())
        .bind(input.port)
        .bind(input.unit_id)
        .bind(input.enabled)
        .bind(input.simulation)
        .bind(&input.word_order)
        .bind(stored_database)
        .bind(stored_username)
        .bind(stored_password)
        .bind(id)
        .fetch_one(&mut *connection)
        .await
        .map_err(|err| match err {
            sqlx::Error::RowNotFound => BantoError::NotFound {
                resource: RESOURCE.to_string(),
                id: id.to_string(),
            },
            other => map_write_error(other, "name", NAME_ALREADY_USED, "", ""),
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

    /// Transaction-compatible counterpart of [`Self::delete`].
    pub async fn delete_tx(
        &self,
        connection: &mut SqliteConnection,
        id: i64,
    ) -> Result<(), BantoError> {
        let protocol: Option<String> =
            sqlx::query_scalar("SELECT protocol FROM plc_connections WHERE id = ?")
                .bind(id)
                .fetch_optional(&mut *connection)
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
        .fetch_one(&mut *connection)
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
            .execute(&mut *connection)
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

    /// T19 S2-b（UX-38、docs/banto-hub-t19-design.md §3.4・§7.5、2026-09-02
    /// オーナー決定「タグがあってもグループ・接続を削除可。定義のみ削除し、
    /// 履歴は残す」）: [`Self::delete`]/[`Self::delete_tx`] とは別の、
    /// カスケード削除の入口。この接続配下の全グループの全タグ →
    /// 全グループ → この接続、の順（FK 順）で削除する。
    ///
    /// **[`Self::delete`]/[`Self::delete_tx`] は変更していない**（意図的 -
    /// このモジュールの doc comment 冒頭の注意点参照）: `relay-wright` は
    /// このクレートの通常 `delete`/`delete_tx` が「子が居れば拒否する」
    /// ことを前提にした独自の一括削除 UI
    /// （`apps/relay-wright/core/src/registry_cascade.rs`、その doc comment
    /// 「banto-tags must not be modified from this app (invariant)」）を
    /// **別に**持っており、通常の DELETE ルートはそのままの安全側の挙動
    /// （拒否）を維持する設計になっている。banto-tags は relay-wright/
    /// banto-collect とも共有するクレートなので、既存の `delete`/`delete_tx`
    /// の意味をここで変えると、UX-38 は banto-hub だけの決定であるにも
    /// かかわらず relay-wright の「通常 DELETE は安全側」という契約を無断で
    /// 崩すことになる。UX-38 を要求する `banto-hub` 側の REST ハンドラ
    /// （`apps/banto-hub/core/src/rest.rs::plc_connections_delete`）だけを
    /// この新しいメソッドへ差し替える。
    ///
    /// **tstore の履歴は触らない**: このクレートはそもそも `banto-tstore`
    /// （日次ファイルの独立したストレージエンジン、`banto-tstore` の lib.rs
    /// doc comment「この crate が意図的に依存しないもの」節参照）に接続
    /// しない - `plc_connections`/`collection_groups`/`tags` の3テーブル
    /// しか触れないので、tstore の実データは構造的に無傷のまま残る
    /// （孤児となった履歴は UX-39 の保持期間で自然に消える想定 - §3.4）。
    ///
    /// 予約接続（`calc`/`mem`）はこのカスケード経路でも削除できない
    /// （[`Self::delete`] と同じ理由 - このモジュールの doc comment
    /// 「"virtual"」節、[`Self::delete`] 自身の doc comment参照）。
    pub async fn cascade_delete_tx(
        &self,
        connection: &mut SqliteConnection,
        id: i64,
    ) -> Result<PlcConnectionCascadeOutcome, BantoError> {
        let protocol: Option<String> =
            sqlx::query_scalar("SELECT protocol FROM plc_connections WHERE id = ?")
                .bind(id)
                .fetch_optional(&mut *connection)
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

        // FK 順: tags -> collection_groups -> plc_connections。
        let deleted_tags = sqlx::query(
            "DELETE FROM tags WHERE collection_group_id IN \
             (SELECT id FROM collection_groups WHERE plc_connection_id = ?)",
        )
        .bind(id)
        .execute(&mut *connection)
        .await
        .map_err(banto_storage::storage_error)?
        .rows_affected() as i64;

        let deleted_groups =
            sqlx::query("DELETE FROM collection_groups WHERE plc_connection_id = ?")
                .bind(id)
                .execute(&mut *connection)
                .await
                .map_err(banto_storage::storage_error)?
                .rows_affected() as i64;

        let result = sqlx::query("DELETE FROM plc_connections WHERE id = ?")
            .bind(id)
            .execute(&mut *connection)
            .await
            .map_err(banto_storage::storage_error)?;
        if result.rows_affected() == 0 {
            return Err(BantoError::NotFound {
                resource: RESOURCE.to_string(),
                id: id.to_string(),
            });
        }

        Ok(PlcConnectionCascadeOutcome {
            deleted_groups,
            deleted_tags,
        })
    }

    /// Non-transactional counterpart of [`Self::cascade_delete_tx`] - opens
    /// its own transaction so the three DELETEs stay atomic even when the
    /// caller has no transaction of its own (mirrors how [`Self::delete`]
    /// relates to [`Self::delete_tx`], except `delete`/`delete_tx` do not
    /// need a transaction since each is a single row-affecting statement).
    pub async fn cascade_delete(&self, id: i64) -> Result<PlcConnectionCascadeOutcome, BantoError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(banto_storage::storage_error)?;
        let outcome = self.cascade_delete_tx(&mut tx, id).await?;
        tx.commit().await.map_err(banto_storage::storage_error)?;
        Ok(outcome)
    }
}

/// What [`PlcConnectionService::cascade_delete`]/[`PlcConnectionService::cascade_delete_tx`]
/// removed besides the connection row itself - lets a caller (audit log,
/// tests) report exactly how much a single confirm removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlcConnectionCascadeOutcome {
    pub deleted_groups: i64,
    pub deleted_tags: i64,
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

            word_order: "low_high".to_string(),
            database: None,
            username: None,
            password: None,
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
            // S1: "postgres" additionally requires database/username (this
            // module's doc comment, "`\"postgres\"`" section) - fill them in
            // so this test still exercises the SQL CHECK specifically,
            // rather than failing validation for an unrelated reason.
            if *protocol == POSTGRES_PROTOCOL {
                input.database = Some("appdb".to_string());
                input.username = Some("appuser".to_string());
            }
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

                word_order: "low_high".to_string(),
                database: None,
                username: None,
                password: None,
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

                word_order: "low_high".to_string(),
                database: None,
                username: None,
                password: None,
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

                word_order: "low_high".to_string(),
                database: None,
                username: None,
                password: None,
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

    // --- S1: "postgres" protocol (migration 0014) ---------------------------

    /// Migration 0014 rebuilds `plc_connections` again (SQLite cannot `ALTER`
    /// a `CHECK`) - the direct sibling of
    /// `migration_0007_preserves_rows_and_foreign_keys_on_a_populated_database`,
    /// but against the full post-0013 schema (`collection_groups.default_writable`
    /// from 0012, `tags.revision`/the 0011 rebuild/`string_encoding` from
    /// 0013) - a stale column list here would silently truncate every
    /// existing row, exactly the risk 0007's own header warns about.
    #[tokio::test]
    async fn migration_0014_preserves_rows_and_foreign_keys_on_a_populated_database() {
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
            (
                "0007",
                include_str!("../migrations/0007_plc_connections_allow_virtual.sql"),
            ),
            (
                "0008",
                include_str!("../migrations/0008_plc_connections_add_simulation.sql"),
            ),
            ("0009", include_str!("../migrations/0009_tags_revision.sql")),
            (
                "0010",
                include_str!("../migrations/0010_plc_connections_add_word_order.sql"),
            ),
            (
                "0011",
                include_str!("../migrations/0011_tags_unique_name_per_group.sql"),
            ),
            (
                "0012",
                include_str!("../migrations/0012_collection_groups_add_default_writable.sql"),
            ),
            (
                "0013",
                include_str!("../migrations/0013_tags_add_string_encoding.sql"),
            ),
        ] {
            conn.execute(sql)
                .await
                .unwrap_or_else(|e| panic!("pre-0014 migration {label} failed: {e}"));
        }

        conn.execute(
            "INSERT INTO plc_connections \
             (id, name, protocol, host, port, unit_id, enabled, simulation, word_order) \
             VALUES (7, 'Line1 PLC', 'slmp', '192.168.1.10', 5007, 3, 0, 1, 'high_low')",
        )
        .await
        .expect("seed connection");
        conn.execute(
            "INSERT INTO collection_groups \
             (id, name, plc_connection_id, period_ms, enabled, default_writable) \
             VALUES (4, 'G1', 7, 1000, 1, 0)",
        )
        .await
        .expect("seed collection group");
        conn.execute(
            "INSERT INTO tags (\
                id, name, collection_group_id, address, data_type, string_length, \
                raw_lo, raw_hi, eng_lo, eng_hi, unit, decimals, threshold_h, enabled, \
                writable, tag_kind, expression, retain, revision, string_encoding\
             ) VALUES (\
                9, 'T1', 4, 'D100', 'i16', NULL, \
                0, 100, 0, 50, 'degC', 2, 45, 1, \
                1, 'plc', NULL, 0, 1, 'shift_jis'\
             )",
        )
        .await
        .expect("seed tag");

        let migration = include_str!("../migrations/0014_plc_connections_allow_postgres.sql");
        let mut tx = conn.begin().await.expect("begin, as the migrator does");
        tx.execute(migration).await.expect("0014 should apply");
        tx.commit().await.expect("0014 should commit");

        #[allow(clippy::type_complexity)]
        let row: (i64, String, String, String, i64, i64, bool, bool, String) = sqlx::query_as(
            "SELECT id, name, protocol, host, port, unit_id, enabled, simulation, word_order \
             FROM plc_connections",
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
                false,
                true,
                "high_low".to_string(),
            )
        );
        // The three new columns start out NULL for a pre-existing row.
        let creds: (Option<String>, Option<String>, Option<String>) =
            sqlx::query_as("SELECT database, username, password FROM plc_connections WHERE id = 7")
                .fetch_one(&mut *conn)
                .await
                .expect("credential columns should exist and be NULL");
        assert_eq!(creds, (None, None, None));

        let group: (i64, String, i64, i64, bool, bool) = sqlx::query_as(
            "SELECT id, name, plc_connection_id, period_ms, enabled, default_writable \
             FROM collection_groups",
        )
        .fetch_one(&mut *conn)
        .await
        .expect("the collection group should survive");
        assert_eq!(group, (4, "G1".to_string(), 7, 1000, true, false));

        #[allow(clippy::type_complexity)]
        let tag: (
            i64,
            String,
            i64,
            String,
            bool,
            String,
            Option<String>,
            bool,
            i64,
            String,
        ) = sqlx::query_as(
            "SELECT id, name, collection_group_id, address, writable, tag_kind, expression, \
                 retain, revision, string_encoding FROM tags",
        )
        .fetch_one(&mut *conn)
        .await
        .expect("the seeded tag should survive with every T2/T6/T18/T20 column intact");
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
                false,
                1,
                "shift_jis".to_string(),
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

        // The point of the whole exercise: 'postgres' (with credentials) is
        // now insertable, and nothing else new is.
        sqlx::query(
            "INSERT INTO plc_connections (name, protocol, host, port, database, username, password) \
             VALUES ('ERP DB', 'postgres', '10.0.0.50', 5432, 'erp', 'reader', 'secret')",
        )
        .execute(&mut *conn)
        .await
        .expect("postgres should be accepted after the rebuild");
        assert!(sqlx::query(
            "INSERT INTO plc_connections (name, protocol, host, port) \
             VALUES ('Nope', 'ethernet-ip', '192.168.1.30', 44818)",
        )
        .execute(&mut *conn)
        .await
        .is_err());
    }

    /// [`ALLOWED_PROTOCOLS`]/the SQL `CHECK` widened by migration 0014, plus
    /// this crate's own application-layer rule (this module's doc comment,
    /// "`\"postgres\"`" section): `database`/`username` required for
    /// `"postgres"`, forbidden for every other protocol.
    #[tokio::test]
    async fn create_rejects_postgres_without_database_or_username() {
        let svc = service().await;
        let mut input = sample_input("PgNoCreds");
        input.protocol = POSTGRES_PROTOCOL.to_string();
        let err = svc.create(input).await.unwrap_err();
        match err {
            BantoError::Validation { field_errors } => {
                let fields: Vec<&str> = field_errors.iter().map(|e| e.field.as_str()).collect();
                assert!(fields.contains(&"database"));
                assert!(fields.contains(&"username"));
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    /// The reverse direction: a non-postgres connection may not carry
    /// `database`/`username`/`password` - a `"modbus-tcp"` row can never
    /// smuggle in a stray DB credential.
    #[tokio::test]
    async fn create_rejects_database_username_password_on_a_non_postgres_connection() {
        let svc = service().await;
        let mut input = sample_input("NotPg");
        input.database = Some("somedb".to_string());
        input.username = Some("someone".to_string());
        input.password = Some("secret".to_string());
        let err = svc.create(input).await.unwrap_err();
        match err {
            BantoError::Validation { field_errors } => {
                let fields: Vec<&str> = field_errors.iter().map(|e| e.field.as_str()).collect();
                assert!(fields.contains(&"database"));
                assert!(fields.contains(&"username"));
                assert!(fields.contains(&"password"));
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    fn postgres_input(name: &str) -> PlcConnectionInput {
        PlcConnectionInput {
            name: name.to_string(),
            protocol: POSTGRES_PROTOCOL.to_string(),
            host: "10.0.0.50".to_string(),
            port: 5432,
            unit_id: 1,
            enabled: true,
            simulation: false,
            word_order: "low_high".to_string(),
            database: Some("appdb".to_string()),
            username: Some("appuser".to_string()),
            password: None,
        }
    }

    /// A `"postgres"` connection created with `password: None` stores no
    /// password (this crate's `PlcConnectionInput::password` doc comment,
    /// "create" case) - round-trips as `None`, not an empty string.
    #[tokio::test]
    async fn create_postgres_without_password_stores_none() {
        let svc = service().await;
        let created = svc
            .create(postgres_input("PgNoPassword"))
            .await
            .expect("postgres without a password should be accepted");
        assert_eq!(created.database.as_deref(), Some("appdb"));
        assert_eq!(created.username.as_deref(), Some("appuser"));
        assert_eq!(created.password, None);
    }

    /// A `"postgres"` connection created with `Some(s)` stores `s` verbatim.
    #[tokio::test]
    async fn create_postgres_with_password_stores_it() {
        let svc = service().await;
        let mut input = postgres_input("PgWithPassword");
        input.password = Some("s3cret".to_string());
        let created = svc.create(input).await.expect("create should succeed");
        assert_eq!(created.password.as_deref(), Some("s3cret"));
    }

    /// Update's tri-state PATCH semantics (this crate's
    /// `PlcConnectionInput::password` doc comment, "update" case): omitting
    /// `password` (`None`) keeps whatever is already stored.
    #[tokio::test]
    async fn update_with_password_none_keeps_the_existing_password() {
        let svc = service().await;
        let mut create_input = postgres_input("PgKeep");
        create_input.password = Some("original".to_string());
        let created = svc.create(create_input).await.unwrap();

        let mut update_input = postgres_input("PgKeep");
        update_input.password = None;
        let updated = svc.update(created.id, update_input).await.unwrap();
        assert_eq!(updated.password.as_deref(), Some("original"));
    }

    /// `Some("")` clears a previously-stored password.
    #[tokio::test]
    async fn update_with_password_empty_string_clears_it() {
        let svc = service().await;
        let mut create_input = postgres_input("PgClear");
        create_input.password = Some("original".to_string());
        let created = svc.create(create_input).await.unwrap();

        let mut update_input = postgres_input("PgClear");
        update_input.password = Some(String::new());
        let updated = svc.update(created.id, update_input).await.unwrap();
        assert_eq!(updated.password, None);
    }

    /// `Some(s)` for a non-empty `s` replaces the stored password.
    #[tokio::test]
    async fn update_with_password_some_replaces_it() {
        let svc = service().await;
        let mut create_input = postgres_input("PgReplace");
        create_input.password = Some("original".to_string());
        let created = svc.create(create_input).await.unwrap();

        let mut update_input = postgres_input("PgReplace");
        update_input.password = Some("replacement".to_string());
        let updated = svc.update(created.id, update_input).await.unwrap();
        assert_eq!(updated.password.as_deref(), Some("replacement"));
    }

    /// `unit_id`/`word_order`/`simulation` are silently normalized to their
    /// defaults for a `"postgres"` connection, regardless of what the caller
    /// sent (this module's doc comment, "`\"postgres\"`" section,
    /// [`normalize_postgres_input`]) - no validation error, just ignored.
    #[tokio::test]
    async fn postgres_connection_normalizes_unit_id_word_order_and_simulation() {
        let svc = service().await;
        let mut input = postgres_input("PgNormalize");
        input.unit_id = 200;
        input.word_order = "high_low".to_string();
        input.simulation = true;
        let created = svc.create(input).await.expect("create should succeed");
        assert_eq!(created.unit_id, 1);
        assert_eq!(created.word_order, "low_high");
        assert!(!created.simulation);
    }

    /// [`PlcConnection::is_db_source`]: true only for `"postgres"` rows.
    #[tokio::test]
    async fn is_db_source_is_true_only_for_postgres() {
        let svc = service().await;
        let pg = svc.create(postgres_input("PgIsDbSource")).await.unwrap();
        assert!(pg.is_db_source());

        let plc = svc.create(sample_input("NotDbSource")).await.unwrap();
        assert!(!plc.is_db_source());
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

                word_order: "low_high".to_string(),
                database: None,
                username: None,
                password: None,
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

                word_order: "low_high".to_string(),
                database: None,
                username: None,
                password: None,
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

    // --- P3-b: "word_order" column (migration 0010) ------------------------

    /// `sample_input`'s default (`"low_high"`, matching
    /// `default_word_order`/migration `0010`'s column default) round-trips
    /// through create/get - the baseline every other test in this module
    /// already exercises implicitly, stated explicitly as the counterpart to
    /// [`word_order_round_trips_through_update`].
    #[tokio::test]
    async fn word_order_defaults_to_low_high_and_round_trips() {
        let svc = service().await;
        let mut input = sample_input("WordOrder1");
        input.protocol = "slmp".to_string();
        let created = svc.create(input).await.unwrap();
        assert_eq!(created.word_order, "low_high");

        let fetched = svc.get(created.id).await.unwrap();
        assert_eq!(fetched.word_order, "low_high");
    }

    /// `word_order: "high_low"` is accepted and persists through both
    /// `create` and a later `update` - the direct analogue of
    /// `simulation_flag_round_trips_through_update`.
    #[tokio::test]
    async fn word_order_round_trips_through_update() {
        let svc = service().await;
        let mut input = sample_input("WordOrder2");
        input.protocol = "slmp".to_string();
        input.word_order = "high_low".to_string();
        let created = svc.create(input).await.unwrap();
        assert_eq!(created.word_order, "high_low");
        assert_eq!(svc.get(created.id).await.unwrap().word_order, "high_low");

        let mut back = sample_input("WordOrder2");
        back.protocol = "slmp".to_string();
        back.word_order = "low_high".to_string();
        let updated = svc.update(created.id, back).await.unwrap();
        assert_eq!(updated.word_order, "low_high");
    }

    /// An unrecognized `word_order` is rejected as a `FieldError` on
    /// `wordOrder`, the same shape [`create_rejects_unknown_protocol`] proves
    /// for `protocol`.
    #[tokio::test]
    async fn create_rejects_unknown_word_order() {
        let svc = service().await;
        let mut input = sample_input("X");
        input.word_order = "middle_endian".to_string();
        let err = svc.create(input).await.unwrap_err();
        match err {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "wordOrder");
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    /// [`ALLOWED_WORD_ORDERS`] and the SQL `CHECK` migration `0010` added are
    /// two hand-written copies of one rule - the `word_order` twin of
    /// `every_allowed_protocol_is_accepted_by_the_sql_check`.
    #[tokio::test]
    async fn every_allowed_word_order_is_accepted_by_the_sql_check() {
        let svc = service().await;
        for (i, word_order) in ALLOWED_WORD_ORDERS.iter().enumerate() {
            let mut input = sample_input(&format!("wo{i}"));
            input.protocol = "slmp".to_string();
            input.word_order = (*word_order).to_string();
            let created = svc.create(input).await.unwrap_or_else(|e| {
                panic!(
                    "{word_order} is in ALLOWED_WORD_ORDERS but the SQL CHECK rejected it: {e:?}"
                )
            });
            assert_eq!(&created.word_order, word_order);
        }
    }

    /// The reverse direction: a value the SQL `CHECK` would accept must not be
    /// missing from [`ALLOWED_WORD_ORDERS`] - the `word_order` twin of
    /// `the_sql_check_accepts_nothing_beyond_allowed_protocols`. Bypasses the
    /// service layer deliberately, the only way to ask the schema directly
    /// what it allows.
    #[tokio::test]
    async fn the_sql_check_accepts_nothing_beyond_allowed_word_orders() {
        let pool = banto_storage::connect_sqlite_memory()
            .await
            .expect("connect_sqlite_memory");
        migrate(&pool).await.expect("migrate");

        for word_order in ["middle_endian", "", "LOW_HIGH", "HighLow"] {
            let result = sqlx::query(
                "INSERT INTO plc_connections (name, protocol, host, port, word_order) \
                 VALUES ('X', 'slmp', '1.2.3.4', 5007, ?)",
            )
            .bind(word_order)
            .execute(&pool)
            .await;
            assert!(
                result.is_err(),
                "the SQL CHECK accepted word_order {word_order:?}, which is not in ALLOWED_WORD_ORDERS"
            );
        }
    }

    /// A `plc_connections` row created before migration `0010` (i.e. one
    /// created here through raw SQL that never mentions `word_order`) reads
    /// back as `"low_high"` - the column default matches
    /// `SlmpConfig::default().word_order`, so an upgraded database's existing
    /// SLMP connections keep behaving exactly as they did before this column
    /// existed (this migration's own header, "既存の全 plc_connections 行 ...
    /// 「今と同じ動作」のまま").
    #[tokio::test]
    async fn a_row_inserted_without_word_order_defaults_to_low_high() {
        let pool = banto_storage::connect_sqlite_memory()
            .await
            .expect("connect_sqlite_memory");
        migrate(&pool).await.expect("migrate");

        sqlx::query(
            "INSERT INTO plc_connections (name, protocol, host, port) \
             VALUES ('Legacy', 'slmp', '192.168.1.20', 5007)",
        )
        .execute(&pool)
        .await
        .expect("insert without word_order should use the column default");

        let svc = PlcConnectionService::new(pool);
        let rows = svc
            .list(ListParams::default())
            .await
            .expect("list should succeed");
        assert_eq!(rows.rows[0].word_order, "low_high");
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

    // --- T19 S2-b (UX-38, docs/banto-hub-t19-design.md §3.4/§7.5): cascade
    // delete - deliberately a NEW method (`cascade_delete`/`cascade_delete_tx`),
    // not a change to `delete`/`delete_tx` above. See `cascade_delete_tx`'s
    // own doc comment for why the guarded methods stay untouched (relay-wright
    // depends on their "refuse while children exist" behavior).

    /// The core UX-38 behavior: a connection with groups and tags can be
    /// removed in one call, and every descendant definition goes with it.
    #[tokio::test]
    async fn cascade_delete_removes_groups_and_tags_with_the_connection() {
        let svc = service().await;
        let conn = svc.create(sample_input("InUse")).await.unwrap();

        for group_name in ["G1", "G2"] {
            sqlx::query(
                "INSERT INTO collection_groups (name, plc_connection_id, period_ms, enabled) \
                 VALUES (?, ?, 1000, 1)",
            )
            .bind(group_name)
            .bind(conn.id)
            .execute(&svc.pool)
            .await
            .unwrap();
        }
        let group_ids: Vec<i64> = sqlx::query_scalar(
            "SELECT id FROM collection_groups WHERE plc_connection_id = ? ORDER BY id",
        )
        .bind(conn.id)
        .fetch_all(&svc.pool)
        .await
        .unwrap();
        for (i, group_id) in group_ids.iter().enumerate() {
            sqlx::query(
                "INSERT INTO tags (name, collection_group_id, address, data_type) \
                 VALUES (?, ?, '40001', 'i16')",
            )
            .bind(format!("T{i}"))
            .bind(group_id)
            .execute(&svc.pool)
            .await
            .unwrap();
        }

        let outcome = svc
            .cascade_delete(conn.id)
            .await
            .expect("cascade_delete should succeed even with children");
        assert_eq!(outcome.deleted_groups, 2);
        assert_eq!(outcome.deleted_tags, 2);

        assert!(matches!(
            svc.get(conn.id).await.unwrap_err(),
            BantoError::NotFound { .. }
        ));
        let remaining_groups: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM collection_groups WHERE plc_connection_id = ?",
        )
        .bind(conn.id)
        .fetch_one(&svc.pool)
        .await
        .unwrap();
        assert_eq!(remaining_groups, 0);
        let remaining_tags: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tags")
            .fetch_one(&svc.pool)
            .await
            .unwrap();
        assert_eq!(
            remaining_tags, 0,
            "the tags must be gone along with their groups"
        );
    }

    /// A childless connection behaves exactly like the old `delete`.
    #[tokio::test]
    async fn cascade_delete_with_no_children_behaves_like_a_plain_delete() {
        let svc = service().await;
        let conn = svc.create(sample_input("Lonely")).await.unwrap();
        let outcome = svc.cascade_delete(conn.id).await.unwrap();
        assert_eq!(outcome.deleted_groups, 0);
        assert_eq!(outcome.deleted_tags, 0);
        assert!(matches!(
            svc.get(conn.id).await.unwrap_err(),
            BantoError::NotFound { .. }
        ));
    }

    #[tokio::test]
    async fn cascade_delete_missing_id_is_not_found() {
        let svc = service().await;
        let err = svc.cascade_delete(999).await.unwrap_err();
        assert!(matches!(err, BantoError::NotFound { .. }));
    }

    /// The one invariant UX-38 must NOT touch: a reserved `calc`/`mem`
    /// connection stays undeletable even through the cascade path, and even
    /// when it does have groups/tags under it (unlike the plain in-use
    /// guard on non-virtual connections, this check fires unconditionally -
    /// same as [`delete_refuses_a_virtual_connection_even_with_no_groups_attached`]).
    #[tokio::test]
    async fn cascade_delete_still_refuses_a_virtual_connection_even_with_tags() {
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

                word_order: "low_high".to_string(),
                database: None,
                username: None,
                password: None,
            })
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO collection_groups (name, plc_connection_id, period_ms, enabled) \
             VALUES ('calc-group', ?, 1000, 1)",
        )
        .bind(calc.id)
        .execute(&svc.pool)
        .await
        .unwrap();
        let group_id: i64 =
            sqlx::query_scalar("SELECT id FROM collection_groups WHERE plc_connection_id = ?")
                .bind(calc.id)
                .fetch_one(&svc.pool)
                .await
                .unwrap();
        sqlx::query(
            "INSERT INTO tags (name, collection_group_id, address, data_type, tag_kind) \
             VALUES ('c1', ?, '', 'f32', 'computed')",
        )
        .bind(group_id)
        .execute(&svc.pool)
        .await
        .unwrap();

        let err = svc.cascade_delete(calc.id).await.unwrap_err();
        match err {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "id");
                assert!(field_errors[0].message.contains("予約接続"));
            }
            other => panic!("expected Validation, got {other:?}"),
        }
        // Nothing was touched: connection, group, and tag all survive.
        svc.get(calc.id).await.expect("calc should survive");
        let remaining_groups: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM collection_groups WHERE plc_connection_id = ?",
        )
        .bind(calc.id)
        .fetch_one(&svc.pool)
        .await
        .unwrap();
        assert_eq!(remaining_groups, 1);
        let remaining_tags: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tags")
            .fetch_one(&svc.pool)
            .await
            .unwrap();
        assert_eq!(remaining_tags, 1);
    }

    /// `cascade_delete_tx` must not commit anything on its own - it has to
    /// participate cleanly in a transaction the CALLER controls (this is how
    /// `apps/banto-hub/core/src/rest.rs::plc_connections_delete` uses it:
    /// begin -> `cascade_delete_tx` -> preflight -> commit, rolling the whole
    /// transaction back if preflight rejects the resulting registry state).
    /// This test proves the "both parent and children survive together" half
    /// of that contract by rolling back explicitly instead of committing.
    #[tokio::test]
    async fn cascade_delete_tx_rolls_back_together_with_the_callers_transaction() {
        let svc = service().await;
        let conn = svc.create(sample_input("Rollback")).await.unwrap();
        sqlx::query(
            "INSERT INTO collection_groups (name, plc_connection_id, period_ms, enabled) \
             VALUES ('G1', ?, 1000, 1)",
        )
        .bind(conn.id)
        .execute(&svc.pool)
        .await
        .unwrap();
        let group_id: i64 =
            sqlx::query_scalar("SELECT id FROM collection_groups WHERE plc_connection_id = ?")
                .bind(conn.id)
                .fetch_one(&svc.pool)
                .await
                .unwrap();
        sqlx::query(
            "INSERT INTO tags (name, collection_group_id, address, data_type) \
             VALUES ('T1', ?, '40001', 'i16')",
        )
        .bind(group_id)
        .execute(&svc.pool)
        .await
        .unwrap();

        let mut tx = svc.pool.begin().await.unwrap();
        let outcome = svc.cascade_delete_tx(&mut tx, conn.id).await.unwrap();
        assert_eq!(outcome.deleted_groups, 1);
        assert_eq!(outcome.deleted_tags, 1);
        tx.rollback().await.unwrap();

        // Rolled back: everything is exactly as before.
        svc.get(conn.id)
            .await
            .expect("connection should survive the rollback");
        let remaining_tags: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tags")
            .fetch_one(&svc.pool)
            .await
            .unwrap();
        assert_eq!(remaining_tags, 1);
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
