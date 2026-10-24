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
//! `banto_collect` (not this crate) runs the connection against the value
//! layer's simulated device (`banto_driver_engine::sim`, no socket; until
//! R2b-5 an in-process wire simulator's loopback address replaced the real
//! `host`/`port`) at collection time; this crate only stores and validates
//! the flag.
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
//! doc comment), and the registry holds it as a host-provided protocol (not a
//! session-type driver), so `banto_broker` never opens a session against it.
//!
//! Three settings exist only for this protocol: [`PlcConnection::database`]/
//! [`PlcConnection::username`] and the password. [`validate_plc_connection_input`]
//! enforces the two directions of one rule: `database`/`username` are required
//! (non-empty, trimmed, capped) when `protocol == `[`POSTGRES_PROTOCOL`], and they
//! and a password must be empty/absent for every other protocol - a
//! `"modbus-tcp"` row can never carry a stray DB password.
//!
//! The password was a plaintext column until R2b-1d (§2.2's "v1 は MQTT と同じ平文").
//! Since migration `0025` (docs/banto-hub-connection-settings-design.md §3.3・§5) it is
//! a sealed row of `plc_connection_secrets` that `settings.password` refers to with
//! `{"$secret": "<ref>"}`; this crate receives it already sealed
//! ([`PlcConnectionInput::secrets`]) and never sees the plaintext
//! ([`crate::connection_secret`]).
//!
//! ## R2b-1d: `settings` for every protocol (migration `0025`)
//!
//! The table now has only `id`・`name`・`protocol`・`enabled`・`simulation`・`settings`・
//! `settings_version`; every per-protocol value lives in the JSON `settings`
//! (design §3.2's table) and the flat fields of [`PlcConnection`] are a read-only
//! projection of it (`settings_json::legacy::flat_view`). The SQL `CHECK` on
//! `protocol` is only the `DriverId` grammar now - [`ALLOWED_PROTOCOLS`] (and the
//! hand-written validation) is the old rule set a service without a registry
//! ([`PlcConnectionService::new`], ChronoGazer, R2b-7) still enforces; a service
//! with one ([`PlcConnectionService::with_registry`], banto-hub) adds the driver's
//! validation after it. Each write runs in one transaction that also writes/deletes
//! the connection's sealed secret rows (design §5.3).
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
//! generic UI/MCP caller that always sends its own defaults (`unitId: 1`,
//! some `wordOrder`) for every protocol should not need postgres-specific
//! branching just to avoid a validation error.
//!
//! ## `word_order` is a Modbus setting too (#325, 2026-09-08 オーナー決定)
//!
//! Migration `0010` added `word_order` **for SLMP** and said as much: its
//! header calls the column "modbus-tcp/virtual 接続では無意味な列". For
//! `"virtual"` that is still true; for `"modbus-tcp"` it was simply wrong,
//! and the mistake was load-bearing. Modbus multi-word values (`u32`/`f32`,
//! and since #325 the 64-bit types) have exactly the same high-word/low-word
//! ambiguity SLMP does, and the two code paths that build a Modbus client
//! disagreed about it: `banto_collect::config::modbus_config_for` (the
//! collection poller) ignored the column and hard-coded
//! `WordOrder::default()` = `HighLow`, while `banto_broker`'s own
//! `modbus_config_for` (read-on-demand and writes) read the column - which,
//! because `banto-hub`'s connection form only ever showed the word-order
//! control for `"slmp"`, was always the column default `"low_high"`. The
//! same tag therefore decoded one way into history and the opposite way on a
//! read-now/write.
//!
//! The owner's decision is `"high_low"` (the Modbus/IEEE convention) for
//! Modbus: that keeps the collection path's actual behaviour - and hence the
//! interpretation of every already-recorded history sample - untouched, and
//! moves the broker onto it. Three changes implement it, of which this
//! module owns the last two:
//!
//! 1. the collection path starts reading the column (`banto-collect`);
//! 2. migration `0017_modbus_word_order_high_low.sql` back-fills every
//!    existing `"modbus-tcp"` row to `"high_low"`, so step 1 does not
//!    silently flip every deployed connection (that migration's header has
//!    the full reasoning);
//! 3. a **new** `"modbus-tcp"` connection now defaults to `"high_low"`
//!    instead of `"low_high"` - see [`default_word_order_for`] and
//!    [`normalize_word_order_input`] for how a protocol-dependent default is
//!    applied to a field serde cannot default protocol-dependently.
//!
//! **In this slice (S1a), no [`crate::collection_group::CollectionGroup`]
//! may be created under a `"postgres"` connection** -
//! `crate::collection_group::CollectionGroupService::create`/`create_tx`/
//! `update`/`update_tx` reject it with a `FieldError` on `plcConnectionId`
//! (S2, the DB Source polling engine itself, lifts this once `query_sql`
//! and `tag_kind = "db"` exist to give such a group somewhere to put its
//! results).
//!
//! ## `"hub"` (#383 段階3 P2, 2026-10-10 オーナー決定 H1/H5)
//!
//! A fifth protocol joins in migration `0019_plc_connections_allow_hub.sql`:
//! `"hub"` ([`HUB_PROTOCOL`]) names the banto-hub tag server as a *source* -
//! ChronoGazer records tags it receives from the Hub alongside the ones it
//! reads from PLCs directly. Hub-sourced tags are ordinary `tags` rows under
//! groups under this connection, so pens, history and exclusions all key off
//! the same tag ID; a hub tag's `address` is the Hub tag's **external name**
//! (`接続名.グループ名.タグ名` - identity by name, scada-design.md §9.6), see
//! [`crate::tag::validate_hub_tag_address`] for the exact rule.
//!
//! What this connection row does **not** carry is the Hub's endpoint or key:
//! there is exactly one Hub, and ChronoGazer's `hub.record` setting already
//! owns both (H1). So, like `"postgres"`, a `"hub"` [`PlcConnectionInput`] has
//! every wire/PLC field **silently normalized** ([`normalize_hub_input`]):
//! `host` → `""`, `port` → `0`, `unit_id`/`word_order` → column defaults,
//! `simulation` → `false`. Normalizing rather than rejecting is the
//! `"postgres"` reasoning above - a generic caller that always sends its own
//! defaults should not need hub-specific branching - and storing `""`/`0`
//! (rather than whatever was sent) keeps a stale-looking host from sitting in
//! the row while the real endpoint lives elsewhere. `database`/`username`/
//! `password` stay forbidden, same as every non-`"postgres"` protocol.
//!
//! **At most one `"hub"` row per registry.** There is one Hub, so a second
//! `"hub"` connection could only ever be a duplicate source for the same
//! external names. Migration `0019` enforces this with a partial unique index
//! (`idx_plc_connections_single_hub`), which is race-free without a
//! read-then-write transaction; [`map_connection_write_error`] turns the
//! violation into a `FieldError` on `protocol`.
//!
//! **Switching a connection's protocol to or from `"hub"` is refused while it
//! still has collection groups** (`update`/`update_tx`). A hub tag's address
//! is an external name and a PLC tag's is a device address; flipping the
//! protocol underneath existing tags would leave every one of them with an
//! address its new connection cannot interpret. The same boundary applies to
//! moving a group that has tags between a `"hub"` and a non-`"hub"`
//! connection (`crate::collection_group::CollectionGroupService`).
//!
//! **Writes through the Hub are out of scope (H5)** - a hub tag can never be
//! `writable` ([`crate::tag::TagService`]'s placement check rejects it).
//!
//! The collection side does not sample `"hub"` connections yet (P3 adds the
//! Hub sampler to `banto-collect`); until then `banto_collect`'s lenient
//! build leaves them out with an explicit reason. banto-hub itself never
//! accepts a `"hub"` connection (a Hub does not subscribe to itself) -
//! `apps/banto-hub/core/src/rest.rs::reject_hub_connection_protocol`.
//!
//! ## `"push"` (R2a-2a, docs/banto-hub-ingest-design.md §3, 2026-10-21 オーナー決定)
//!
//! A sixth protocol joins in migration `0023_plc_connections_allow_push.sql`:
//! `"push"` ([`PUSH_PROTOCOL`]) names a connection whose values an external
//! sender (a DB bridge, a script) **pushes into** banto-hub - nothing is
//! dialed or subscribed to. Like `"hub"`, it has no endpoint of its own, so
//! [`normalize_push_input`] silently normalizes every wire/PLC field (`host`
//! → `""`, `port` → `0`, `unit_id`/`word_order` → column defaults,
//! `simulation` → `false`; `cpu_type` is left to the create/update rule and
//! is inert for `"push"`). `database`/`username`/`password` stay forbidden
//! (a bridge refers to an existing `"postgres"` connection by id instead,
//! §10.2). There is no per-registry limit (no partial unique index - one
//! connection per sender, §9.5-1).
//!
//! The same migration adds the `settings` column ([`PlcConnection::settings`]),
//! a JSON object that only `"push"` used at first (`stale_timeout_ms` and the
//! bridge section, §3.2; R2b-1d widened it to every protocol). Its rules are
//! the pure [`crate::push::validate_push_settings`] - every write method of
//! [`PlcConnectionService`] runs it - plus one registry lookup this module
//! does itself: `bridge.db_connection_id` must name a `"postgres"`
//! connection. Any other protocol must leave `settings` empty.
//!
//! **`settings` on update is "omitted keeps"** (the [`PlcConnectionInput::cpu_type`]
//! reasoning: an update from a caller that does not know the field - today
//! every REST/MCP payload - must not silently wipe a bridge's configuration).
//! An explicit object replaces it; switching to a non-`"push"` protocol
//! drops it (that protocol's `settings` come from its flat fields).
//!
//! A `"push"` tag's `address` is the sender's opaque key, unique per
//! connection - see `crate::tag`'s placement rules and migration `0024`.
//! banto-hub does not refuse `"push"` at its REST boundary: until R2a-1c
//! wires the receiver, the Hub's preflight (the strict
//! `banto_collect::build_config_from`) refuses an *enabled* `"push"`
//! connection with `pushSourceUnavailable`, and ChronoGazer's own protocol
//! allow-list never offered it.

use std::sync::Arc;

use banto_core::{BantoError, FieldError, ListParams, ListResult};
use banto_driver_api::DriverRegistry;
use banto_storage::ColumnMap;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sqlx::{QueryBuilder, Sqlite, SqliteConnection, SqlitePool};

use crate::connection_secret::{
    builtin_secret_keys, check_settings_shape, delete_all_secret_rows, delete_secret_rows_except,
    insert_secret_rows, load_all_secret_rows, load_secret_rows_of, secret_ref_lookup,
    secret_ref_of, secret_ref_value, SecretChange, SecretChanges, SecretRefLookup, StoredSecretRow,
    PASSWORD_KEY,
};
use crate::push::{validate_push_settings, PushSettings};
use crate::settings_json::legacy::{settings_from_flat, SLMP_PROTOCOL};
use crate::settings_json::validate_settings_with_registry;
use crate::support::{
    map_write_error, max_length_message, range_message, required_message, NAME_ALREADY_USED,
};
use crate::tag::{apply_address_rewrites, reparse_tags, ReparseScope};

mod settings_input;
pub use settings_input::ConnectionSettingsInput;
use settings_input::TypedSettings;

/// The **legacy** protocol rule list (R2b-1f, 2026-10-22). It is no longer the
/// Hub's source of truth: since migration `0025` the SQL `CHECK` on
/// `plc_connections.protocol` is only the `DriverId` grammar, and the Hub's
/// vocabulary is its driver registry
/// ([`PlcConnectionService::with_registry`], design
/// `banto-hub-connection-settings-design.md` §4). This list stays, unchanged,
/// as the rule of a service built without a registry
/// ([`PlcConnectionService::new`]: ChronoGazer and tests) and of the old flat
/// validation step (1) that [`validate_plc_connection_input`] still runs
/// first, until R2b-7 retires both (refactor-plan; Q6). Until then, adding a
/// protocol to the Hub means registering a driver **and** widening this list:
/// step (1) runs first on every service, registry or not, and refuses any
/// protocol missing here before the registry's validation is reached (so a
/// registry-only protocol would fail every REST/MCP create and update). After
/// R2b-7 registering the driver alone is enough. It originally mirrored the SQL
/// `CHECK` (as widened by `migrations/0004_plc_connections_allow_slmp.sql`
/// and later ones); `every_allowed_protocol_is_accepted_by_the_sql_check`
/// still inserts every entry through the service.
///
/// The six entries are, in order, [`MODBUS_PROTOCOL`], `"slmp"`,
/// [`VIRTUAL_PROTOCOL`], [`POSTGRES_PROTOCOL`], [`HUB_PROTOCOL`] (widened by
/// `migrations/0019_plc_connections_allow_hub.sql`) and [`PUSH_PROTOCOL`]
/// (widened by `migrations/0023_plc_connections_allow_push.sql`) - spelled out
/// as literals here (rather than built from those constants) so this list
/// still reads as the vocabulary itself, exactly like the SQL `CHECK` it
/// mirrors; `the_named_protocol_constants_are_all_in_allowed_protocols` is the
/// tripwire that keeps the two spellings from drifting.
///
/// This is the *registry's* vocabulary, not "what every app accepts": an app
/// narrows it at its own boundary (ChronoGazer accepts `"modbus-tcp"`/`"slmp"`
/// only, banto-hub refuses `"hub"`).
pub const ALLOWED_PROTOCOLS: &[&str] =
    &["modbus-tcp", "slmp", "virtual", "postgres", "hub", "push"];

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
/// store and validate the string; the consumers are what parse it into a
/// `WordOrder` - `banto-broker` (`SessionDirectory::ensure_connection`) when
/// building an `SlmpConfig`/`ModbusConfig`, and `banto_collect::config` when
/// building the collection poller's own client configs.
///
/// **Both values are meaningful for `"slmp"` *and* `"modbus-tcp"`** (#325,
/// 2026-09-08 - this module's doc comment, "`word_order` is a Modbus setting
/// too"); only `"virtual"`/`"postgres"` rows carry a value nothing reads.
pub const ALLOWED_WORD_ORDERS: &[&str] = &["low_high", "high_low"];

/// Low word first (MELSEC's own convention: `D0` holds the low half of a
/// 32-bit value, `D1` the high half) - migration `0010`'s column default, and
/// the default a new non-Modbus connection still gets
/// ([`default_word_order_for`]).
pub const WORD_ORDER_LOW_HIGH: &str = "low_high";

/// High word first - the Modbus/IEEE convention, and since 2026-09-08 the
/// default for a new [`MODBUS_PROTOCOL`] connection (this module's doc
/// comment, "`word_order` is a Modbus setting too"; migration `0017` for the
/// matching back-fill of existing rows).
pub const WORD_ORDER_HIGH_LOW: &str = "high_low";

/// #605: MELSEC-Q（QJ71E71 など）。4 バイトのデバイス番地、サブコマンド
/// `0x0000`/`0x0001`。`banto_plc::SlmpCpu::Q` に対応する。
pub const CPU_TYPE_Q: &str = "Q";

/// #605: iQ-R。6 バイトのデバイス番地、サブコマンド `0x0002`/`0x0003`。
/// migration `0021` の列既定であり、この列を足す前の
/// `SlmpConfig::default().cpu`（`SlmpCpu::R`）と同じ。
pub const CPU_TYPE_R: &str = "R";

/// #605: `plc_connections.cpu_type` が取りうる値。migration `0021` の
/// `CHECK` と二重管理する（[`ALLOWED_WORD_ORDERS`] と同じ「TEXT + CHECK」）。
/// L シリーズはフレームが Q 互換だが、今回の接続単位の選択は Q/R だけ
/// （他局・マルチ CPU と同じく範囲外）。
pub const ALLOWED_CPU_TYPES: &[&str] = &[CPU_TYPE_Q, CPU_TYPE_R];

/// Modbus TCP - the first protocol this registry ever spoke (plan.md §3's I2
/// decision) and the first entry of [`ALLOWED_PROTOCOLS`]. Named as a
/// constant (rather than left as a bare literal at its use sites) because
/// #325 gave it a second job beyond connection validation: the 64-bit data
/// types ([`crate::tag::MODBUS_ONLY_DATA_TYPES`]) may only be used by tags
/// whose group sits under a connection with this protocol, a rule
/// [`crate::tag::TagService`] enforces the same way it enforces the
/// `calc`/`mem`/`postgres` placement rules (see
/// [`VIRTUAL_PROTOCOL`]/[`POSTGRES_PROTOCOL`] below).
pub const MODBUS_PROTOCOL: &str = "modbus-tcp";

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

/// The banto-hub-as-a-source protocol (#383 段階3 P2, this module's doc
/// comment "`\"hub\"`" section). Used by [`validate_plc_connection_input`]/
/// [`normalize_hub_input`] (no host/port of its own), [`PlcConnection::is_hub_source`],
/// the at-most-one / protocol-switch guards in [`PlcConnectionService`], and
/// [`crate::tag::TagService`]'s hub-tag placement rules.
pub const HUB_PROTOCOL: &str = "hub";

/// The push-ingest protocol (docs/banto-hub-ingest-design.md §3, this
/// module's doc comment "`\"push\"`" section): a connection whose values are
/// pushed *into* banto-hub by an external sender rather than dialed or
/// subscribed to. Added to [`ALLOWED_PROTOCOLS`] and the SQL `CHECK` by
/// migration `0023` (R2a-2a; the constant itself and `banto_collect::config`'s
/// planning came first, in R2a-1b). Used by [`normalize_push_input`],
/// [`validate_plc_connection_input`] (no endpoint, `settings` allowed), and
/// `crate::tag`'s push-tag placement rules.
pub const PUSH_PROTOCOL: &str = "push";

/// Field-error message for a second `"hub"` connection (this module's doc
/// comment, "`\"hub\"`" section - at most one per registry).
const SECOND_HUB_MESSAGE: &str = "Hub 接続は 1 つまでです（既に登録されています）";

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

/// Migration `0010`'s column default, [`WORD_ORDER_LOW_HIGH`]. Still what a
/// `"postgres"` row is normalized to ([`normalize_postgres_input`]) - for
/// that protocol the column is inert, so "whatever the schema would have
/// written" is the right value to store.
///
/// This is **no longer** the default a deserialized payload gets: since
/// 2026-09-08 that default depends on `protocol`
/// ([`default_word_order_for`]).
fn default_word_order() -> String {
    WORD_ORDER_LOW_HIGH.to_string()
}

/// #325 (2026-09-08 オーナー決定, this module's doc comment "`word_order` is
/// a Modbus setting too"): the `word_order` a **new** connection gets when
/// its payload does not name one. Protocol-dependent, which is the whole
/// difficulty - `"modbus-tcp"` wants the Modbus/IEEE convention
/// ([`WORD_ORDER_HIGH_LOW`]) while `"slmp"` keeps MELSEC's
/// ([`WORD_ORDER_LOW_HIGH`], also migration `0010`'s column default, so
/// `"slmp"` behaviour is unchanged).
///
/// `"virtual"`/`"postgres"` fall in the `else` arm and get
/// [`WORD_ORDER_LOW_HIGH`]; neither ever reads the column, and
/// [`normalize_postgres_input`] overwrites it for `"postgres"` anyway.
pub(crate) fn default_word_order_for(protocol: &str) -> String {
    if protocol == MODBUS_PROTOCOL {
        WORD_ORDER_HIGH_LOW.to_string()
    } else {
        WORD_ORDER_LOW_HIGH.to_string()
    }
}

/// Apply [`default_word_order_for`] to a payload that left `wordOrder`
/// unspecified.
///
/// **Why a normalization step rather than `#[serde(default = ..)]`**: serde's
/// field defaults are computed per field, with no access to the rest of the
/// struct, so "default that depends on `protocol`" is not expressible there
/// at all. The same problem `normalize_postgres_input` already solves for
/// `unit_id`/`word_order`/`simulation`, solved the same way - which is why
/// both run from one [`normalize_plc_connection_input`] entry point, this one
/// first so `"postgres"` still wins.
///
/// **How "unspecified" is represented**: [`PlcConnectionInput::word_order`]
/// is `#[serde(default)]`, i.e. the empty string, and *empty means
/// unspecified* (see that field's doc comment for why this sentinel rather
/// than `Option<String>`). An empty `word_order` therefore never reaches
/// [`validate_plc_connection_input`] - by the time validation runs it has
/// been replaced by a real member of [`ALLOWED_WORD_ORDERS`].
fn normalize_word_order_input(mut input: PlcConnectionInput) -> PlcConnectionInput {
    if input.word_order.is_empty() {
        input.word_order = default_word_order_for(&input.protocol);
    }
    input
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

/// #383 段階3 P2 (this module's doc comment, "`\"hub\"`" section): a `"hub"`
/// connection has no endpoint of its own (ChronoGazer's `hub.record` owns
/// it) and no wire/PLC settings, so `host`/`port`/`unit_id`/`word_order`/
/// `simulation` are silently replaced - `host` with `""`, `port` with `0`,
/// the rest with their column defaults - the same "ignore, don't reject"
/// treatment [`normalize_postgres_input`] gives a `"postgres"` row.
///
/// A no-op for every other protocol (returns `input` unchanged).
fn normalize_hub_input(mut input: PlcConnectionInput) -> PlcConnectionInput {
    if input.protocol == HUB_PROTOCOL {
        input.host = String::new();
        input.port = 0;
        input.unit_id = default_unit_id();
        input.word_order = default_word_order();
        input.simulation = false;
    }
    input
}

/// R2a-2a (this module's doc comment, "`\"push\"`" section): a `"push"`
/// connection is dialed by nobody - the sender connects to banto-hub - so its
/// wire/PLC fields get exactly [`normalize_hub_input`]'s treatment.
///
/// A no-op for every other protocol (returns `input` unchanged).
fn normalize_push_input(mut input: PlcConnectionInput) -> PlcConnectionInput {
    if input.protocol == PUSH_PROTOCOL {
        input.host = String::new();
        input.port = 0;
        input.unit_id = default_unit_id();
        input.word_order = default_word_order();
        input.simulation = false;
    }
    input
}

/// The single normalization entry point every [`PlcConnectionService`] write
/// method runs a payload through before [`validate_plc_connection_input`].
///
/// Order matters: [`normalize_word_order_input`] first (it only fills in an
/// *unspecified* `word_order`), then [`normalize_postgres_input`] /
/// [`normalize_hub_input`] / [`normalize_push_input`] (which override
/// `word_order` unconditionally for their protocol), so a
/// `"postgres"`/`"hub"`/`"push"` row still ends up carrying the plain column
/// default no matter what - or whether - the caller sent.
pub(crate) fn normalize_plc_connection_input(input: PlcConnectionInput) -> PlcConnectionInput {
    normalize_push_input(normalize_hub_input(normalize_postgres_input(
        normalize_word_order_input(input),
    )))
}

/// Whether `protocol` has no simulation of its own, i.e. its `simulation`
/// column is always normalized to `false` (`"virtual"` is rejected instead,
/// `"postgres"`/`"hub"`/`"push"` are normalized - this module's doc comment). Used by
/// [`PlcConnectionService::update_preserving_simulation`] so a "keep the
/// stored value" update still stores `false` for these.
fn has_no_simulation(protocol: &str) -> bool {
    protocol == POSTGRES_PROTOCOL
        || protocol == VIRTUAL_PROTOCOL
        || protocol == HUB_PROTOCOL
        || protocol == PUSH_PROTOCOL
}

/// [`map_write_error`] for `plc_connections`, plus the one constraint only
/// this table has: the partial unique index `idx_plc_connections_single_hub`
/// (migration `0019`, at most one `"hub"` row). SQLite reports a partial
/// unique index on `protocol` as `UNIQUE constraint failed:
/// plc_connections.protocol`, which is told apart from the `name` UNIQUE by
/// that column name; anything else falls through to the shared mapping.
fn map_connection_write_error(err: sqlx::Error) -> BantoError {
    if let Some(db_err) = err.as_database_error() {
        if db_err.is_unique_violation() && db_err.message().contains("plc_connections.protocol") {
            return BantoError::Validation {
                field_errors: vec![FieldError {
                    field: "protocol".to_string(),
                    message: SECOND_HUB_MESSAGE.to_string(),
                }],
            };
        }
    }
    map_write_error(err, "name", NAME_ALREADY_USED, "", "")
}

/// #383 段階3 P2 (this module's doc comment, "`\"hub\"`" section): refuse
/// switching a connection's protocol across the `"hub"` boundary while it
/// still has collection groups. `existing_protocol` is the row's current
/// protocol (`None` = no such row - the `UPDATE` reports that as not-found).
/// `group_count` is only consulted when the boundary is actually crossed.
fn hub_protocol_switch_error(
    existing_protocol: Option<&str>,
    new_protocol: &str,
    group_count: i64,
) -> Option<BantoError> {
    if !crosses_hub_boundary(existing_protocol, new_protocol) || group_count == 0 {
        return None;
    }
    Some(BantoError::Validation {
        field_errors: vec![FieldError {
            field: "protocol".to_string(),
            message: format!(
                "この接続には収集グループが{group_count}件あるため、Hub 接続と他のプロトコルの間で\
                 変更できません（Hub 経由のタグのアドレスは外部名で、PLC のアドレスとは\
                 互換がありません）"
            ),
        }],
    })
}

/// `SELECT COUNT(*)` of the groups under connection `id` - the input
/// [`hub_protocol_switch_error`] needs, only queried when the protocol
/// boundary is crossed.
const GROUP_COUNT_SQL: &str = "SELECT COUNT(*) FROM collection_groups WHERE plc_connection_id = ?";

fn crosses_hub_boundary(existing_protocol: Option<&str>, new_protocol: &str) -> bool {
    existing_protocol
        .is_some_and(|existing| (existing == HUB_PROTOCOL) != (new_protocol == HUB_PROTOCOL))
}

/// R2a-2a (PR #753 review P2): whether an update switches a connection's
/// protocol into or out of `"push"`.
fn crosses_push_boundary(existing_protocol: Option<&str>, new_protocol: &str) -> bool {
    existing_protocol
        .is_some_and(|existing| (existing == PUSH_PROTOCOL) != (new_protocol == PUSH_PROTOCOL))
}

/// `SELECT COUNT(*)` of the tags under connection `id` (through its groups) -
/// the input [`push_protocol_switch_error`] needs, only queried when the
/// `"push"` boundary is crossed.
const TAG_COUNT_SQL: &str = "SELECT COUNT(*) FROM tags t \
     JOIN collection_groups g ON g.id = t.collection_group_id \
     WHERE g.plc_connection_id = ?";

/// R2a-2a (PR #753 review P2): refuse switching a connection's protocol into
/// or out of `"push"` while it still has tags. A `"push"` tag's `address` is
/// the sender's key, unique per connection, and its tags follow
/// `crate::tag`'s push rules (no `string`/`writable`/scaling); a PLC tag's
/// `address` is a device address with none of those rules. Flipping the
/// protocol under existing tags would carry them across without the tag-level
/// checks (duplicate keys, a writable or string tag under `"push"`), so the
/// switch is refused outright - the same stance [`hub_protocol_switch_error`]
/// takes, counted in tags rather than groups (an empty group carries nothing
/// across). Migration `0024`'s `plc_connections_push_key_unique_protocol`
/// trigger is the database-side backstop for the uniqueness half.
fn push_protocol_switch_error(
    existing_protocol: Option<&str>,
    new_protocol: &str,
    tag_count: i64,
) -> Option<BantoError> {
    if !crosses_push_boundary(existing_protocol, new_protocol) || tag_count == 0 {
        return None;
    }
    Some(BantoError::Validation {
        field_errors: vec![FieldError {
            field: "protocol".to_string(),
            message: format!(
                "この接続にはタグが{tag_count}件あるため、push 接続と他のプロトコルの間で\
                 変更できません（push のタグのアドレスは送り手が決めるキーで、PLC のアドレスとは\
                 規則が違います）"
            ),
        }],
    })
}

/// The parsed settings of a payload that is about to *write* them (`"push"`
/// with an explicit object). `None` when nothing new is written. Call after
/// [`validate_plc_connection_input`] passed (so the parse cannot fail).
fn settings_to_write(input: &PlcConnectionInput) -> Option<PushSettings> {
    if input.protocol != PUSH_PROTOCOL {
        return None;
    }
    input
        .settings
        .as_ref()
        .and_then(|value| validate_push_settings(value).ok())
}

/// `SELECT protocol` of the connection a bridge's `db_connection_id` names.
const BRIDGE_DB_CONNECTION_SQL: &str = "SELECT protocol FROM plc_connections WHERE id = ?";

/// R2a-2a (docs/banto-hub-ingest-design.md §3.2・§10.2): a bridge's
/// `db_connection_id` must name an existing `"postgres"` connection. The one
/// `settings` rule that needs the registry, so it runs here rather than in
/// the pure `validate_push_settings`; `protocol` is the referenced row's
/// protocol (`None` = no such row).
fn bridge_db_connection_error(db_connection_id: i64, protocol: Option<&str>) -> Option<BantoError> {
    let message = match protocol {
        Some(POSTGRES_PROTOCOL) => return None,
        None => format!("db_connection_id {db_connection_id} の接続が見つかりません"),
        Some(other) => format!(
            "db_connection_id は postgres 接続を指してください（id {db_connection_id} は {other} 接続です）"
        ),
    };
    Some(BantoError::Validation {
        field_errors: vec![FieldError {
            field: "settings.bridge.db_connection_id".to_string(),
            message,
        }],
    })
}

/// The refusal for a bridge whose `db_connection_id` is the connection being
/// written itself (PR #753 review P2). Only a `"push"` row can carry
/// `settings`, so after the write that row is `"push"`, never `"postgres"` -
/// reading its *pre-update* protocol (e.g. a `postgres` → `push` update)
/// would wrongly accept it.
fn bridge_self_reference_error(db_connection_id: i64) -> BantoError {
    BantoError::Validation {
        field_errors: vec![FieldError {
            field: "settings.bridge.db_connection_id".to_string(),
            message: format!(
                "db_connection_id {db_connection_id} はこの接続自身です（push 接続は postgres \
                 接続を指してください）"
            ),
        }],
    }
}

/// Run [`bridge_db_connection_error`] for a payload whose settings are being
/// written, through `executor` (the pool, or the caller's transaction so it
/// sees that transaction's own writes). `updated_id` is the row an update is
/// about to overwrite (`None` on create): a reference to it is judged against
/// its post-update state ([`bridge_self_reference_error`]), not the row as
/// it stands.
async fn check_bridge_db_connection<'c, E>(
    executor: E,
    input: &PlcConnectionInput,
    updated_id: Option<i64>,
) -> Result<(), BantoError>
where
    E: sqlx::Executor<'c, Database = Sqlite>,
{
    let Some(bridge) = settings_to_write(input).and_then(|settings| settings.bridge) else {
        return Ok(());
    };
    if updated_id == Some(bridge.db_connection_id) {
        return Err(bridge_self_reference_error(bridge.db_connection_id));
    }
    let protocol: Option<String> = sqlx::query_scalar(BRIDGE_DB_CONNECTION_SQL)
        .bind(bridge.db_connection_id)
        .fetch_optional(executor)
        .await
        .map_err(banto_storage::storage_error)?;
    match bridge_db_connection_error(bridge.db_connection_id, protocol.as_deref()) {
        Some(err) => Err(err),
        None => Ok(()),
    }
}

/// Names of the `"push"` connections whose bridge reads from connection `id`
/// (`settings.bridge.db_connection_id`), oldest first.
const PUSH_BRIDGES_REFERENCING_SQL: &str = "SELECT name FROM plc_connections \
     WHERE protocol = 'push' AND settings IS NOT NULL \
     AND json_extract(settings, '$.bridge.db_connection_id') = ? ORDER BY id";

/// R2a-2e (docs/banto-hub-ingest-design.md §3.2): refuse to delete a
/// connection that a `"push"` bridge's `settings.bridge.db_connection_id`
/// still names - the same RESTRICT-style refusal banto-hub's
/// `reject_delete_if_referenced_by_sink_group` gives a sink group's
/// `db_connection_id`, as a `Validation` error on `id` that names the bridges
/// in the way. The reference is **by id** (§10.2, Q8): the bridge would
/// otherwise point at a deleted id (and, ids not being reused, never at
/// anything again) and its DB bridge could only fail. Every delete entry
/// ([`PlcConnectionService::delete`], [`PlcConnectionService::delete_tx`],
/// [`PlcConnectionService::cascade_delete_tx`]) runs this, so REST, MCP and
/// the pending-change apply all refuse alike. Runs through `executor` (the
/// caller's transaction when there is one) so it sees that transaction's own
/// writes.
async fn reject_delete_if_referenced_by_push_bridge<'c, E>(
    executor: E,
    id: i64,
) -> Result<(), BantoError>
where
    E: sqlx::Executor<'c, Database = Sqlite>,
{
    let names: Vec<String> = sqlx::query_scalar(PUSH_BRIDGES_REFERENCING_SQL)
        .bind(id)
        .fetch_all(executor)
        .await
        .map_err(banto_storage::storage_error)?;
    if names.is_empty() {
        return Ok(());
    }
    Err(BantoError::Validation {
        field_errors: vec![FieldError {
            field: "id".to_string(),
            message: format!(
                "この接続を DB ブリッジの読み取り先にしている push 接続が{}件あるため削除できません\
                 （{}）。先にその push 接続のブリッジの設定を外すか、別の接続に変えてください",
                names.len(),
                names.join("、")
            ),
        }],
    })
}

/// A row of the `plc_connections` table, wire-shaped (camelCase) for a
/// future settings grid (docs/recorder-requirements.md §6 "タグ設定"
/// screen: "PLC 接続設定含む").
///
/// ## R2b-1d: `settings` is the truth, the flat fields are a read-only projection
///
/// Since migration `0025` (docs/banto-hub-connection-settings-design.md §3.1・§3.2) the
/// table has only `id`・`name`・`protocol`・`enabled`・`simulation`・`settings`・
/// `settings_version`. The per-protocol endpoint/PLC settings live in [`Self::settings`]
/// (a JSON object, every protocol); [`Self::host`]・[`Self::port`]・[`Self::unit_id`]・
/// [`Self::word_order`]・[`Self::cpu_type`]・[`Self::database`]・[`Self::username`] are
/// **built from it** by `settings_json::legacy::flat_view` (in [`Self::from_settings`],
/// which `FromRow` calls) so the collection, the broker and ChronoGazer keep reading the
/// flat fields until R2b-7 (design §4.3・§7). Writing to them changes nothing in the
/// database. A key a protocol does not read, or that is absent, reads as today's default.
///
/// The postgres password is **not** a field any more: `settings.password` holds only a
/// reference `{"$secret": "<ref>"}` to a sealed row of `plc_connection_secrets`
/// (design §3.3, [`crate::connection_secret`]). [`Self::secret_ref`] reads the reference;
/// opening the sealed value is banto-hub's `secrets` module's job.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlcConnection {
    pub id: i64,
    pub name: String,
    pub protocol: String,
    /// Read-only projection of `settings.host` (`""` when absent or not read by the protocol).
    pub host: String,
    /// Read-only projection of `settings.port` (`0` when absent or not read by the protocol).
    pub port: i64,
    /// Read-only projection of `settings.unit_id` (`modbus-tcp` only; `1` otherwise).
    pub unit_id: i64,
    pub enabled: bool,
    /// T9-1 (this module's doc comment, "simulation" section). Still a column (a host
    /// concept, design §3.1).
    pub simulation: bool,
    /// P3-b (this module's doc comment, [`ALLOWED_WORD_ORDERS`]). Read-only projection
    /// of `settings.word_order` - meaningful for `"slmp"` **and `"modbus-tcp"`** (#325,
    /// 2026-09-08 - this module's doc comment, "`word_order` is a Modbus setting too");
    /// every other protocol reads as its default ([`default_word_order_for`]).
    pub word_order: String,
    /// #605: SLMP の CPU 種別（[`CPU_TYPE_Q`] / [`CPU_TYPE_R`]）。`settings.cpu_type` の
    /// 読み取りの写し（`slmp` だけ。ほかのプロトコルと無いときは [`CPU_TYPE_R`]）。
    pub cpu_type: String,
    /// S1: read-only projection of `settings.database` (`"postgres"` only, `None`
    /// otherwise).
    pub database: Option<String>,
    /// S1: read-only projection of `settings.username` (`"postgres"` only, `None`
    /// otherwise).
    pub username: Option<String>,
    /// R2b-1d: the connection's JSON settings (migration `0025`'s `settings` column,
    /// `NOT NULL`, always an object). Per-protocol keys are design §3.2's table; a
    /// `"push"` row's object is what [`crate::push::validate_push_settings`] accepted
    /// (R2a). Secret keys hold only references (`{"$secret": "<ref>"}`), never a value,
    /// so this may be serialized as is.
    #[serde(default)]
    pub settings: Map<String, Value>,
    /// R2b-1d (design §3.4): the driver's settings-shape version the row was written
    /// with. Always `1` in R2b-1.
    #[serde(default = "default_settings_version")]
    pub settings_version: i64,
}

fn default_settings_version() -> i64 {
    1
}

/// Hand-written instead of `#[derive(sqlx::FromRow)]` because of
/// [`PlcConnection::settings`]: the column is JSON text (the derive's `#[sqlx(json)]`
/// needs sqlx's `json` feature, which this workspace does not enable) and the flat
/// fields are projected from it ([`PlcConnection::from_settings`]). `COLUMNS` and
/// `banto_collect`'s `load_connection` SELECT must still name every column
/// (implementation-checklist §5, #605). A `settings` that is not a JSON object fails the
/// row (the column's `CHECK` makes that impossible for a row SQLite accepted, design §4.4).
impl<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow> for PlcConnection {
    fn from_row(row: &'r sqlx::sqlite::SqliteRow) -> Result<Self, sqlx::Error> {
        use sqlx::Row;
        let text: String = row.try_get("settings")?;
        let settings: Map<String, Value> =
            serde_json::from_str(&text).map_err(|err| sqlx::Error::ColumnDecode {
                index: "settings".to_string(),
                source: Box::new(err),
            })?;
        Ok(PlcConnection::from_settings(
            row.try_get("id")?,
            row.try_get("name")?,
            row.try_get("protocol")?,
            row.try_get("enabled")?,
            row.try_get("simulation")?,
            settings,
            row.try_get("settings_version")?,
        ))
    }
}

impl PlcConnection {
    /// Build a connection from its stored columns, projecting the flat fields from
    /// `settings` (`settings_json::legacy::flat_view`, design §4.3). `FromRow` uses this;
    /// so can a test or a caller that holds a `settings` object.
    pub fn from_settings(
        id: i64,
        name: String,
        protocol: String,
        enabled: bool,
        simulation: bool,
        settings: Map<String, Value>,
        settings_version: i64,
    ) -> Self {
        let flat = crate::settings_json::legacy::flat_view(&protocol, &settings);
        PlcConnection {
            id,
            name,
            protocol,
            host: flat.host,
            port: flat.port,
            unit_id: flat.unit_id,
            enabled,
            simulation,
            word_order: flat.word_order,
            cpu_type: flat.cpu_type,
            database: flat.database,
            username: flat.username,
            settings,
            settings_version,
        }
    }

    /// The secret reference stored under `key` (`settings[key] = {"$secret": ref}`), if
    /// any (design §3.3). `postgres`'s password is `secret_ref(`[`PASSWORD_KEY`]`)`.
    pub fn secret_ref(&self, key: &str) -> Option<&str> {
        secret_ref_of(&self.settings, key)
    }

    /// `settings[key]` を秘密の参照として読む（キーが無い・形の正しい参照・キーはあるが
    /// 形が違う、を分ける。PR #777 のオーナーレビュー P2）。秘密値を読む側はこちらを使い、
    /// 形の違う値を「値が無い」として扱わない。
    pub fn secret_ref_lookup(&self, key: &str) -> SecretRefLookup<'_> {
        secret_ref_lookup(&self.settings, key)
    }

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

    /// #383 段階3 P2 (this module's doc comment, "`\"hub\"`" section):
    /// whether this row is the banto-hub source connection
    /// (`protocol == `[`HUB_PROTOCOL`]) rather than a wire-protocol PLC
    /// connection - the `"hub"` sibling of [`Self::is_db_source`], used by
    /// `banto_collect::config` to keep it out of the PLC tasks.
    pub fn is_hub_source(&self) -> bool {
        self.protocol == HUB_PROTOCOL
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
    /// P3-b. See [`PlcConnection::word_order`]'s doc comment for what the
    /// value means.
    ///
    /// **The empty string means "unspecified"** (#325, 2026-09-08 - this
    /// module's doc comment, "`word_order` is a Modbus setting too"). Since
    /// the default now depends on `protocol` - `"high_low"` for
    /// [`MODBUS_PROTOCOL`], `"low_high"` for everything else - and serde
    /// cannot compute a field default from a sibling field, the `serde`
    /// default here is deliberately a *sentinel* (`String::default()`, i.e.
    /// `""`) rather than a real word order, and
    /// [`normalize_word_order_input`] turns it into the protocol's default
    /// before validation. A payload that *does* name a word order keeps it
    /// verbatim, so an operator who deliberately picks `"low_high"` for a
    /// Modbus connection gets `"low_high"` stored.
    ///
    /// **A `""` sentinel, not `Option<String>`**: making this field optional
    /// would be the tidier encoding of "unspecified", but the field is
    /// non-optional in ~100 direct Rust constructions of this struct across
    /// `banto-hub`, `relay-wright`, `banto-collect` and this crate's own
    /// tests, and this crate is shared by all of them - the type change alone
    /// would be a workspace-wide edit for a defaulting rule. `""` is
    /// unambiguous as a sentinel because it is not, and never was, a legal
    /// stored value: it is absent from [`ALLOWED_WORD_ORDERS`] *and* rejected
    /// by migration `0010`'s SQL `CHECK`.
    ///
    /// The one behaviour this costs: a payload with an explicit
    /// `"word_order": ""` used to be a `wordOrder` validation error and is
    /// now silently treated as omitted. Nothing legitimate sends that.
    ///
    /// A **direct Rust construction** (a test, an app's own DTO conversion)
    /// naturally spells out a real value and therefore counts as explicit;
    /// pass `String::new()` to ask for the protocol's default instead. This
    /// matters more than it looks, because every app in this workspace
    /// deserializes its *own* wire DTO and converts: `banto-hub`'s
    /// `rest::PlcConnectionPayload` and relay-wright's namesake both carry
    /// their own `#[serde(default)]` for `wordOrder` and hand this struct a
    /// fully-populated `String`. Until those defaults become the same
    /// sentinel, an omitted `wordOrder` on **their** wire is still filled in
    /// as `"low_high"` before it ever reaches this crate - the
    /// protocol-dependent default below only governs callers that construct
    /// or deserialize a `PlcConnectionInput` directly.
    #[serde(default)]
    pub word_order: String,
    /// #605. SLMP の CPU 種別。
    ///
    /// **作成と更新で省略の意味が違う**（パスワードの tri-state と同じ理由:
    /// この項目を知らないクライアントの更新で、保存済みの Q を黙って R に
    /// 戻さない）:
    ///
    /// - 作成: `None`（省略）と `Some("")` は [`CPU_TYPE_R`]。`Some("Q")` /
    ///   `Some("R")` はその値。それ以外は検証エラー。
    /// - 更新: `None` と `Some("")` は**既存行の値を保つ**（R2b-1d から: 同じ書き込みの
    ///   トランザクションの中で今の `settings.cpu_type` を読んで合わせる。今の行が `slmp`
    ///   でない・値が無いときは [`CPU_TYPE_R`]。設計 §4.3）。明示した `"Q"` / `"R"`
    ///   だけ置き換える。未知の値は検証エラーで、既定へのフォールバックは
    ///   しない。
    #[serde(default)]
    pub cpu_type: Option<String>,
    /// S1. See [`PlcConnection::database`]'s doc comment. Required
    /// (non-empty after trim) when `protocol == `[`POSTGRES_PROTOCOL`],
    /// forbidden (must be `None`/empty) otherwise.
    #[serde(default)]
    pub database: Option<String>,
    /// S1. See [`PlcConnection::username`]'s doc comment. Same
    /// required-for-postgres/forbidden-otherwise rule as [`Self::database`].
    #[serde(default)]
    pub username: Option<String>,
    /// R2b-1d (docs/banto-hub-connection-settings-design.md §5.3): what to do with each
    /// secret key of the connection, **already sealed** by the caller (banto-hub's
    /// `secrets` module) - this crate never receives a plaintext secret. A key that is
    /// absent means [`SecretChange::Keep`]. Replaces the plaintext `password` field.
    ///
    /// The REST/MCP tri-state of the old `password` maps onto it at the Hub's entrance:
    /// omitted / `null` → `Keep`, `""` → `Clear`, a value → `Set(sealed)`. Per call:
    ///
    /// - **create** ([`PlcConnectionService::create`]/[`PlcConnectionService::create_tx`]):
    ///   `Set` writes the sealed row and puts `{"$secret": ref}` under the key; `Keep`
    ///   and `Clear` both mean "no value yet" (the old create rule: `None` and `Some("")`
    ///   both stored no password).
    /// - **update** ([`PlcConnectionService::update`] and its siblings): `Keep` keeps the
    ///   stored reference and row - only when the new protocol still holds that key as a
    ///   secret; `Clear` removes reference and row; `Set` adds the new row, removes the old
    ///   one and switches the reference. A protocol change removes every reference and row
    ///   that is not a secret key of the new protocol. All in the write's transaction.
    ///
    /// `Set` is accepted only for the protocol's secret keys (the driver's
    /// `secret_keys()` with a registry, [`crate::connection_secret::builtin_secret_keys`]
    /// without: `postgres` → `password`); any other key is a field error on that key,
    /// with the old `password` wording ("postgres接続以外では指定できません").
    ///
    /// Not part of the wire form (`#[serde(skip)]`): a deserialized input always has no
    /// change, so a plaintext `password` in a JSON payload can never reach the database
    /// through this type.
    #[serde(skip)]
    pub secrets: SecretChanges,
    /// R2a-2a. See [`PlcConnection::settings`]'s doc comment. Allowed only
    /// when `protocol == `[`PUSH_PROTOCOL`] (validated by
    /// [`crate::push::validate_push_settings`]); must be `None` otherwise.
    ///
    /// **Like [`Self::cpu_type`], omission means different things on create
    /// and update**: create stores `{}` (the protocol's defaults); update keeps the
    /// stored object when this is `None` and the row already is `"push"` (read inside
    /// the write's transaction, design §4.3) and replaces it with an explicit object.
    /// Every other protocol's `settings` is built from the flat fields
    /// (`settings_json::legacy::settings_from_flat`, design §3.2), so switching a
    /// connection away from `"push"` drops its push settings.
    #[serde(default)]
    pub settings: Option<serde_json::Value>,
}

/// Validate a [`PlcConnectionInput`]: `name`/`host` trimmed non-empty (name
/// additionally capped at `MAX_NAME_LEN`), `protocol` in [`ALLOWED_PROTOCOLS`],
/// `port` in `1..=65535`, `unit_id` in `0..=255`, `word_order` in
/// [`ALLOWED_WORD_ORDERS`]. Returns every violation, not just the first
/// (mirrors `items::validate_item_input` in the banto template repo).
///
/// Every caller runs the input through [`normalize_plc_connection_input`]
/// first, so `word_order` is never the `""` "unspecified" sentinel by the
/// time it is checked against [`ALLOWED_WORD_ORDERS`] here
/// ([`PlcConnectionInput::word_order`]'s doc comment).
///
/// S1 (this module's doc comment, "`\"postgres\"`" section) adds one more
/// pair of rules, applied **after** the caller has already run the input
/// through [`normalize_postgres_input`] (so `unit_id`/`word_order`/
/// `simulation` never fail their own checks above for a `"postgres"` row):
/// `database`/`username` are required (trimmed non-empty, capped) when
/// `protocol == `[`POSTGRES_PROTOCOL`], and `database`/`username` must be
/// empty/`None` for every other protocol. R2b-1d: a sealed secret
/// ([`PlcConnectionInput::secrets`]'s `Set`) is accepted only for one of
/// `secret_keys` (the protocol's secret keys - `password` for postgres).
///
/// This is step (1) of design §4.2 (the old flat rules, R2b-7 removes it). With a
/// registry, steps (2)-(4) run only when this passes
/// ([`PlcConnectionService::with_registry`]).
fn validate_plc_connection_input(
    input: &PlcConnectionInput,
    secret_keys: &[String],
) -> Result<(), BantoError> {
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
    //
    // #383 段階3 P2: a "hub" connection has no endpoint of its own either
    // (this module's doc comment, "`\"hub\"`" section) - its host/port are
    // normalized to ""/0 before validation, so the same two checks are
    // skipped for it.
    //
    // R2a-2a: likewise "push" (this module's doc comment, "`\"push\"`"
    // section) - the sender dials banto-hub, not the other way round.
    let is_virtual = input.protocol == VIRTUAL_PROTOCOL;
    let has_no_endpoint =
        is_virtual || input.protocol == HUB_PROTOCOL || input.protocol == PUSH_PROTOCOL;

    if !has_no_endpoint && input.host.trim().is_empty() {
        errors.push(FieldError {
            field: "host".to_string(),
            message: required_message(),
        });
    }

    if !has_no_endpoint && !(MIN_PORT..=MAX_PORT).contains(&input.port) {
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

    // P3-b: validated for every protocol, not just the two that read it
    // ("slmp" and - since #325/2026-09-08 - "modbus-tcp") - same stance
    // `unit_id` already takes (validated even though SLMP never reads it).
    // Keeping it unconditional avoids a second implicit "which values are
    // legal" rule that only applies sometimes; a virtual/postgres row simply
    // carries whatever value it was given (normally the default) and nothing
    // ever reads it.
    if !ALLOWED_WORD_ORDERS.contains(&input.word_order.as_str()) {
        errors.push(FieldError {
            field: "wordOrder".to_string(),
            message: format!(
                "ワード順は {} のいずれかです",
                ALLOWED_WORD_ORDERS.join(", ")
            ),
        });
    }

    // #605: 空文字と None は「未指定」（作成は R、更新は既存を保つ）なので
    // ここでは弾かない。明示された未知の値だけ拒否する。
    if let Some(cpu_type) = input.cpu_type.as_deref() {
        if !cpu_type.is_empty() && !ALLOWED_CPU_TYPES.contains(&cpu_type) {
            errors.push(FieldError {
                field: "cpuType".to_string(),
                message: format!("CPU種別は {} のいずれかです", ALLOWED_CPU_TYPES.join(", ")),
            });
        }
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
    // else. The password has no format rule of its own (R2b-1d: it arrives
    // sealed, `PlcConnectionInput::secrets`); setting one outside postgres is
    // refused below, by the protocol's secret keys.
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
    }

    // R2b-1d (design §5.3): a sealed value may only be set for one of the protocol's
    // secret keys. The `password` wording is the old plaintext column's (S1).
    for (key, change) in &input.secrets {
        if matches!(change, SecretChange::Set(_)) && !secret_keys.iter().any(|k| k == key) {
            errors.push(FieldError {
                field: key.clone(),
                message: if key == PASSWORD_KEY {
                    "postgres接続以外では指定できません".to_string()
                } else {
                    format!("{} 接続では {key} を指定できません", input.protocol)
                },
            });
        }
    }

    // R2a-2a (this module's doc comment, "`\"push\"`" section): `settings`
    // belongs to "push" only (R2b-1 widens it); its content rules are the
    // pure `validate_push_settings`, reported with their `settings.*` paths.
    if let Some(settings) = &input.settings {
        if input.protocol == PUSH_PROTOCOL {
            if let Err(settings_errors) = validate_push_settings(settings) {
                errors.extend(settings_errors);
            }
        } else {
            errors.push(FieldError {
                field: "settings".to_string(),
                message: "settings は push 接続でのみ指定できます".to_string(),
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
    // R2b-1d (design §7): only the columns that still exist. `host`・`port`・`unitId`・
    // `wordOrder`・`cpuType`・`database`・`username` moved into `settings` (no REST
    // surface filters or sorts connections - banto-hub and ChronoGazer both list with
    // `ListParams::default()`).
    ColumnMap::new()
        .column("id", "id")
        .column("name", "name")
        .column("protocol", "protocol")
        .column("enabled", "enabled")
        .column("simulation", "simulation")
    // `settings` (a JSON object) is not listed - nothing filters or sorts by it, a
    // text match against JSON would be meaningless, and it holds secret references.
}

const RESOURCE: &str = "plc_connections";
const COLUMNS: &str = "id, name, protocol, enabled, simulation, settings, settings_version";

/// `SELECT protocol, settings` of the row an update is about to overwrite.
const EXISTING_ROW_SQL: &str = "SELECT protocol, settings FROM plc_connections WHERE id = ?";

/// The refusal for editing a reserved `"virtual"` connection.
fn reserved_edit_error() -> BantoError {
    BantoError::Validation {
        field_errors: vec![FieldError {
            field: "id".to_string(),
            message: "予約接続（calc/mem）は編集できません".to_string(),
        }],
    }
}

/// The refusal for deleting a reserved `"virtual"` connection.
fn reserved_delete_error() -> BantoError {
    BantoError::Validation {
        field_errors: vec![FieldError {
            field: "id".to_string(),
            message: "予約接続（calc/mem）は削除できません".to_string(),
        }],
    }
}

fn validation(field_errors: Vec<FieldError>) -> Result<(), BantoError> {
    if field_errors.is_empty() {
        Ok(())
    } else {
        Err(BantoError::Validation { field_errors })
    }
}

/// Service layer for the `plc_connections` resource. `Clone` is cheap
/// (`SqlitePool` is `Arc`-backed), matching the pattern of every resource
/// service in the banto template repo.
///
/// ## R2b-1d: one transaction per write (design §4.3・§5.3)
///
/// A write now touches two tables - the connection row (whose `settings` is merged with
/// the stored one for the "omitted keeps" fields) and its sealed secret rows
/// (`plc_connection_secrets`, [`crate::connection_secret`]) - so every write runs in one
/// transaction: the `*_tx` methods inside the caller's, the others open their own
/// `BEGIN IMMEDIATE` (they read before they write, checklist §5). The merge reads the
/// stored row **inside** that transaction; nothing is read outside and written back.
#[derive(Clone)]
pub struct PlcConnectionService {
    pool: SqlitePool,
    /// R2b-1d（docs/banto-hub-connection-settings-design.md §4.2）: [`Self::with_registry`]
    /// で渡されたレジストリ。あるときは書き込みが旧い検証 (1) の後に (2)〜(4)（平らな形 →
    /// `settings` → `registry.get` → `validate_config`）を走らせ、秘密のキーと
    /// `settings_version` をドライバから引く。
    registry: Option<Arc<DriverRegistry>>,
}

impl PlcConnectionService {
    /// 旧規則（[`ALLOWED_PROTOCOLS`] と手書きの検証、秘密のキーは
    /// [`crate::connection_secret::builtin_secret_keys`]）のサービス。ChronoGazer と
    /// `banto-tags` のテストが使う（設計 §4.2、Q6）。
    pub fn new(pool: SqlitePool) -> Self {
        Self {
            pool,
            registry: None,
        }
    }

    /// レジストリを正にするサービス（Hub、設計 §4.2）。書き込みは旧い検証 (1) が通った
    /// ときだけ (2) 平らな形を `settings` に写し（「省略は保つ」を合わせた最終の形）→ (3)
    /// `registry.get(protocol)` が無ければ `protocol` の項目エラー → (4) `settings_to_api` →
    /// `validate_config` を走らせる。**(1) が断ったら (1) のエラーだけを返す**。秘密のキーは
    /// ドライバの `secret_keys()`、`settings_version` はドライバの記述子から引く。
    pub fn with_registry(pool: SqlitePool, registry: Arc<DriverRegistry>) -> Self {
        Self {
            pool,
            registry: Some(registry),
        }
    }

    /// [`Self::with_registry`] で渡されたレジストリ。[`Self::new`] では `None`。
    pub fn registry(&self) -> Option<&Arc<DriverRegistry>> {
        self.registry.as_ref()
    }

    /// The secret keys of `protocol`: the driver's `secret_keys()` with a registry (none
    /// for an unregistered protocol - step (3) refuses it anyway), the built-in table
    /// without (design §3.3).
    pub fn secret_keys(&self, protocol: &str) -> Vec<String> {
        match &self.registry {
            Some(registry) => registry
                .get(protocol)
                .map(|entry| entry.base().secret_keys())
                .unwrap_or_default(),
            None => builtin_secret_keys(protocol)
                .iter()
                .map(|key| key.to_string())
                .collect(),
        }
    }

    /// The `settings_version` a write stores (design §3.4): the driver's with a
    /// registry, `1` without (or for an unregistered protocol).
    fn settings_version(&self, protocol: &str) -> i64 {
        self.registry
            .as_ref()
            .and_then(|registry| registry.get(protocol))
            .map_or(1, |entry| i64::from(entry.descriptor().settings_version))
    }

    /// Steps (2)-(4) of design §4.2 plus the storage-layer checks, on the **final**
    /// `settings` a write is about to store (after the "omitted keeps" merge and the
    /// secret references). Called only after step (1) passed.
    ///
    /// With a registry: the driver's validation first; only if that passes, the shape
    /// check. Without: the shape check. The shape check ([`check_settings_shape`]) refuses a
    /// plaintext (or any non-reference) value under a secret key - the last line that keeps
    /// a plaintext secret out of the database whichever entrance forgot to seal it (design
    /// §3.3・§8-1) - and enforces design §3.2's JSON types for the built-in protocols so
    /// that `flat_view` reads back exactly what was validated (§10's R2b-1d note).
    fn validate_settings(
        &self,
        protocol: &str,
        settings: &Map<String, Value>,
        secret_keys: &[String],
    ) -> Result<(), BantoError> {
        if let Some(registry) = &self.registry {
            validation(validate_settings_with_registry(
                registry, protocol, settings,
            ))?;
        }
        validation(check_settings_shape(protocol, settings, secret_keys))
    }

    /// [`Self::validate_settings`] for a write's final `settings`. With `typed`
    /// ([`ConnectionSettingsInput`], R2b-6b) a required secret field that ends up with no
    /// reference is refused first, and every field error is named by its JSON key
    /// (`unit_id`, not the flat form's `unitId`; docs/banto-hub-driver-ui-design.md §4.2).
    fn validate_final_settings(
        &self,
        protocol: &str,
        settings: &Map<String, Value>,
        secret_keys: &[String],
        typed: Option<&TypedSettings>,
    ) -> Result<(), BantoError> {
        let Some(typed) = typed else {
            return self.validate_settings(protocol, settings, secret_keys);
        };
        validation(settings_input::missing_required_secrets(
            settings,
            &typed.required_secrets,
        ))?;
        self.validate_settings(protocol, settings, secret_keys)
            .map_err(settings_input::with_json_keys)
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

    /// The sealed secret rows of connection `id` (design §5.4: the readers open them
    /// in banto-hub's `secrets` module, this crate does not). Empty when it has none.
    pub async fn secret_rows(&self, id: i64) -> Result<Vec<StoredSecretRow>, BantoError> {
        load_secret_rows_of(&self.pool, id)
            .await
            .map_err(banto_storage::storage_error)
    }

    /// Every sealed secret row (all connections).
    pub async fn all_secret_rows(&self) -> Result<Vec<StoredSecretRow>, BantoError> {
        load_all_secret_rows(&self.pool)
            .await
            .map_err(banto_storage::storage_error)
    }

    /// Connection `id` and its sealed secret rows, read in **one** read transaction (one
    /// snapshot). PR #777 owner review P2: reading them with two separate statements lets
    /// a password update commit in between (new reference + new row, old row deleted), so
    /// the connection read first refers to a row that is already gone and a healthy
    /// connection reads as `Missing` - `GET /api/sink/config` would drop it and the sidecar
    /// would rebuild its groups and discard their unsent rows. Readers that open a secret
    /// (sink config, connection test) use this instead of [`Self::get`] +
    /// [`Self::secret_rows`].
    pub async fn get_with_secret_rows(
        &self,
        id: i64,
    ) -> Result<(PlcConnection, Vec<StoredSecretRow>), BantoError> {
        self.get_with_secret_rows_between(id, || async {}).await
    }

    /// [`Self::get_with_secret_rows`] with `between` awaited after the connection read and
    /// before the secret rows read (the tests commit an update there).
    async fn get_with_secret_rows_between<F, Fut>(
        &self,
        id: i64,
        between: F,
    ) -> Result<(PlcConnection, Vec<StoredSecretRow>), BantoError>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        // A deferred `BEGIN`: SQLite takes the read snapshot at the first SELECT and keeps
        // it to the end of the transaction (WAL: writers are not blocked meanwhile).
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(banto_storage::storage_error)?;
        // AssertSqlSafe: 補間されるのは COLUMNS 定数のみ。
        let conn = sqlx::query_as::<_, PlcConnection>(sqlx::AssertSqlSafe(format!(
            "SELECT {COLUMNS} FROM plc_connections WHERE id = ?"
        )))
        .bind(id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|err| banto_storage::not_found(err, RESOURCE, id.to_string()))?;
        between().await;
        let rows = load_secret_rows_of(&mut *tx, id)
            .await
            .map_err(banto_storage::storage_error)?;
        tx.rollback().await.map_err(banto_storage::storage_error)?;
        Ok((conn, rows))
    }

    /// Every connection (by id) and every sealed secret row, read in **one** read
    /// transaction (see [`Self::get_with_secret_rows`] for why). Used by
    /// `GET /api/ingest/bridge-config`.
    pub async fn list_with_secret_rows(
        &self,
    ) -> Result<(Vec<PlcConnection>, Vec<StoredSecretRow>), BantoError> {
        self.list_with_secret_rows_between(|| async {}).await
    }

    async fn list_with_secret_rows_between<F, Fut>(
        &self,
        between: F,
    ) -> Result<(Vec<PlcConnection>, Vec<StoredSecretRow>), BantoError>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(banto_storage::storage_error)?;
        // AssertSqlSafe: 補間されるのは COLUMNS 定数のみ。
        let connections = sqlx::query_as::<_, PlcConnection>(sqlx::AssertSqlSafe(format!(
            "SELECT {COLUMNS} FROM plc_connections ORDER BY id"
        )))
        .fetch_all(&mut *tx)
        .await
        .map_err(banto_storage::storage_error)?;
        between().await;
        let rows = load_all_secret_rows(&mut *tx)
            .await
            .map_err(banto_storage::storage_error)?;
        tx.rollback().await.map_err(banto_storage::storage_error)?;
        Ok((connections, rows))
    }

    /// Create, in its own `BEGIN IMMEDIATE` transaction (the row and its sealed secrets
    /// commit together). See [`Self::create_tx`].
    pub async fn create(&self, input: PlcConnectionInput) -> Result<PlcConnection, BantoError> {
        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(banto_storage::storage_error)?;
        let created = self.create_tx(&mut tx, input).await?;
        tx.commit().await.map_err(banto_storage::storage_error)?;
        Ok(created)
    }

    /// Transaction-compatible counterpart of [`Self::create`]. The caller
    /// owns the transaction and may run additional registry preflight queries
    /// before deciding to commit or roll it back. The connection row and its
    /// sealed secret rows ([`PlcConnectionInput::secrets`]'s `Set`) are written
    /// through `connection`, so a rollback leaves neither.
    pub async fn create_tx(
        &self,
        connection: &mut SqliteConnection,
        input: PlcConnectionInput,
    ) -> Result<PlcConnection, BantoError> {
        self.create_in(connection, input, None).await
    }

    /// Shared body of every create. `typed` is `None` for the flat input (the old rules
    /// (1), then `settings` built from the flat fields) and `Some` for a driver's
    /// `settings` already built by [`crate::settings_from_schema`] (R2b-6b,
    /// [`ConnectionSettingsInput`]: the flat fields of `input` are placeholders and its
    /// common fields were checked by the caller).
    async fn create_in(
        &self,
        connection: &mut SqliteConnection,
        input: PlcConnectionInput,
        typed: Option<TypedSettings>,
    ) -> Result<PlcConnection, BantoError> {
        let input = normalize_plc_connection_input(input);
        let secret_keys = self.secret_keys(&input.protocol);
        let mut settings = match &typed {
            Some(typed) => typed.settings.clone(),
            None => {
                // (1) the old flat rules; with a registry, (2)-(4) only when they pass.
                validate_plc_connection_input(&input, &secret_keys)?;
                check_bridge_db_connection(&mut *connection, &input, None).await?;
                let mut settings = settings_from_flat(&input);
                // #605: an omitted cpu_type creates as R (stored explicitly, like
                // migration 0025).
                if input.protocol == SLMP_PROTOCOL && !settings.contains_key("cpu_type") {
                    settings.insert("cpu_type".into(), CPU_TYPE_R.into());
                }
                settings
            }
        };
        let mut new_rows = Vec::new();
        for key in &secret_keys {
            if let Some(SecretChange::Set(row)) = input.secrets.get(key) {
                settings.insert(key.clone(), secret_ref_value(&row.r#ref));
                new_rows.push((key.clone(), row.clone()));
            }
        }
        self.validate_final_settings(&input.protocol, &settings, &secret_keys, typed.as_ref())?;

        // AssertSqlSafe: get() と同じ理由 - COLUMNS 定数のみを埋め込む固定
        // 文字列。値はすべてプレースホルダでバインドする。
        let created = sqlx::query_as::<_, PlcConnection>(sqlx::AssertSqlSafe(format!(
            "INSERT INTO plc_connections (name, protocol, enabled, simulation, settings, settings_version) \
             VALUES (?, ?, ?, ?, ?, ?) RETURNING {COLUMNS}"
        )))
        .bind(input.name.trim())
        .bind(&input.protocol)
        .bind(input.enabled)
        .bind(input.simulation)
        .bind(Value::Object(settings).to_string())
        .bind(self.settings_version(&input.protocol))
        .fetch_one(&mut *connection)
        .await
        .map_err(map_connection_write_error)?;
        insert_secret_rows(&mut *connection, created.id, &new_rows)
            .await
            .map_err(banto_storage::storage_error)?;
        Ok(created)
    }

    /// **T6-2 addition**: a `"virtual"`-protocol connection cannot be edited
    /// either (same reservation as [`Self::delete`] - the admin UI shows
    /// `calc`/`mem` as read-only rows, this is the API-layer enforcement of
    /// that "編集・削除不可" decision, this module's doc comment).
    ///
    /// R2b-1d: runs in its own `BEGIN IMMEDIATE` transaction ([`Self::update_tx`]).
    pub async fn update(
        &self,
        id: i64,
        input: PlcConnectionInput,
    ) -> Result<PlcConnection, BantoError> {
        let simulation = input.simulation;
        self.update_own_tx(id, input, Some(simulation)).await
    }

    /// Like [`Self::update`], but **leaves the stored `simulation` column as
    /// it is at the moment of the `UPDATE`** (`input.simulation` is ignored).
    ///
    /// chronogazer #413/#417: an update that does not mention `simulation`
    /// must not change it. Reading the current value first and writing it
    /// back would race a concurrent explicit toggle (the read-then-write
    /// window overwrites the newer value), so the preservation happens in
    /// the `UPDATE` statement itself (`simulation = COALESCE(NULL,
    /// simulation)`) - there is no separate read of `simulation` at all.
    ///
    /// The protocol normalizations still win: a `"postgres"`/`"hub"` (or
    /// `"virtual"`) payload stores `simulation = false` exactly as
    /// [`Self::update`] would, because those protocols have no meaningful
    /// simulation (this module's doc comment). Added as a separate method so
    /// [`Self::update`]'s behaviour (banto-hub, relay-wright) is unchanged.
    pub async fn update_preserving_simulation(
        &self,
        id: i64,
        input: PlcConnectionInput,
    ) -> Result<PlcConnection, BantoError> {
        self.update_own_tx(id, input, None).await
    }

    async fn update_own_tx(
        &self,
        id: i64,
        input: PlcConnectionInput,
        simulation: Option<bool>,
    ) -> Result<PlcConnection, BantoError> {
        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(banto_storage::storage_error)?;
        let updated = self.update_in(&mut tx, id, input, simulation, None).await?;
        tx.commit().await.map_err(banto_storage::storage_error)?;
        Ok(updated)
    }

    /// Transaction-compatible counterpart of [`Self::update`].
    pub async fn update_tx(
        &self,
        connection: &mut SqliteConnection,
        id: i64,
        input: PlcConnectionInput,
    ) -> Result<PlcConnection, BantoError> {
        let simulation = input.simulation;
        self.update_in(connection, id, input, Some(simulation), None)
            .await
    }

    /// Shared body of every update (`simulation = Some(value)`: store `value`;
    /// `None`: keep the stored value at `UPDATE` time).
    ///
    /// The "omitted keeps" fields are merged with the stored row **inside** this
    /// transaction (design §4.3, #417 owner review P2): `cpu_type` (an `slmp` row's
    /// stored value when the input omits it), `settings` (a `"push"` row's stored object
    /// when the input omits it) and each secret key's [`SecretChange::Keep`]. The secret
    /// rows are then brought in line with the new `settings` (design §5.3): rows whose
    /// reference is not kept are deleted, `Set` rows inserted.
    ///
    /// `typed` as in [`Self::create_in`]: `Some` replaces the flat rules and the flat
    /// "omitted keeps" merges (`cpu_type`, push `settings`) with the driver's `settings`
    /// as given (only a secret key's [`SecretChange::Keep`] still keeps).
    async fn update_in(
        &self,
        connection: &mut SqliteConnection,
        id: i64,
        mut input: PlcConnectionInput,
        simulation: Option<bool>,
        typed: Option<TypedSettings>,
    ) -> Result<PlcConnection, BantoError> {
        // Validation/normalization see `false` when the caller keeps the
        // stored value - `false` is valid for every protocol, and the kept
        // value was already valid when it was stored.
        input.simulation = simulation.unwrap_or(false);
        let input = normalize_plc_connection_input(input);
        let secret_keys = self.secret_keys(&input.protocol);
        if typed.is_none() {
            validate_plc_connection_input(&input, &secret_keys)?;
            check_bridge_db_connection(&mut *connection, &input, Some(id)).await?;
        }
        // `None` binds SQL NULL -> `COALESCE(NULL, simulation)` keeps the
        // column. Protocols without a meaningful simulation always store the
        // normalized value, same as a plain `update`.
        let simulation_to_store = match simulation {
            None if !has_no_simulation(&input.protocol) => None,
            _ => Some(input.simulation),
        };

        let existing: Option<(String, String)> = sqlx::query_as(EXISTING_ROW_SQL)
            .bind(id)
            .fetch_optional(&mut *connection)
            .await
            .map_err(banto_storage::storage_error)?;
        let existing_protocol = existing.as_ref().map(|(protocol, _)| protocol.as_str());
        if existing_protocol == Some(VIRTUAL_PROTOCOL) {
            return Err(reserved_edit_error());
        }
        if crosses_hub_boundary(existing_protocol, &input.protocol) {
            let group_count: i64 = sqlx::query_scalar(GROUP_COUNT_SQL)
                .bind(id)
                .fetch_one(&mut *connection)
                .await
                .map_err(banto_storage::storage_error)?;
            if let Some(err) =
                hub_protocol_switch_error(existing_protocol, &input.protocol, group_count)
            {
                return Err(err);
            }
        }
        if crosses_push_boundary(existing_protocol, &input.protocol) {
            let tag_count: i64 = sqlx::query_scalar(TAG_COUNT_SQL)
                .bind(id)
                .fetch_one(&mut *connection)
                .await
                .map_err(banto_storage::storage_error)?;
            if let Some(err) =
                push_protocol_switch_error(existing_protocol, &input.protocol, tag_count)
            {
                return Err(err);
            }
        }
        // R2b-2b (docs/banto-hub-driver-address-design.md §3.3, Q2): with a registry a
        // protocol change hands every tag of this connection (disabled and `string`
        // ones too) to the new driver in this same transaction. One that does not pass
        // refuses the change - nothing written - and the others are rewritten to the new
        // driver's canonical form after the UPDATE below.
        let mut address_rewrites = Vec::new();
        if let (Some(registry), Some(old_protocol)) = (&self.registry, existing_protocol) {
            if old_protocol != input.protocol {
                let new_protocol = &input.protocol;
                address_rewrites = reparse_tags(
                    &mut *connection,
                    registry,
                    ReparseScope::Connection(id),
                    input.name.trim(),
                    new_protocol,
                    (
                        "protocol",
                        &format!("プロトコルを {new_protocol} に変更すると、この接続の下の"),
                        &format!(
                            "これらのタグが残っている間は、この接続のままプロトコルを変えることは\
                             できません。{new_protocol} の PLC接続と収集グループを別に作り、各タグの\
                             「収集グループ」と「デバイスアドレス」を同時に変えて移してください\
                             （不要なタグは削除してもかまいません）"
                        ),
                    ),
                )
                .await?;
            }
        }
        let existing_settings: Map<String, Value> = match &existing {
            Some((_, text)) => serde_json::from_str(text).map_err(|err| {
                BantoError::Storage(format!(
                    "plc_connections.settings of id {id} is not a JSON object: {err}"
                ))
            })?,
            None => Map::new(),
        };
        let old_secret_keys = existing_protocol
            .map(|protocol| self.secret_keys(protocol))
            .unwrap_or_default();

        let mut settings = match &typed {
            Some(typed) => typed.settings.clone(),
            None => {
                let mut settings = settings_from_flat(&input);
                // #605: an omitted cpu_type keeps the stored one (an slmp row's), else R.
                if input.protocol == SLMP_PROTOCOL && !settings.contains_key("cpu_type") {
                    let kept = (existing_protocol == Some(SLMP_PROTOCOL))
                        .then(|| existing_settings.get("cpu_type"))
                        .flatten()
                        .filter(|value| value.is_string())
                        .cloned();
                    settings.insert("cpu_type".into(), kept.unwrap_or_else(|| CPU_TYPE_R.into()));
                }
                // R2a-2a: an omitted push `settings` keeps the stored object (only when the
                // row already is push - another protocol's settings are not push settings).
                if input.protocol == PUSH_PROTOCOL
                    && input.settings.is_none()
                    && existing_protocol == Some(PUSH_PROTOCOL)
                {
                    settings = existing_settings.clone();
                }
                settings
            }
        };

        let mut kept_refs = Vec::new();
        let mut new_rows = Vec::new();
        for key in &secret_keys {
            match input.secrets.get(key).unwrap_or(&SecretChange::Keep) {
                SecretChange::Keep => {
                    if old_secret_keys.contains(key) {
                        if let Some(r#ref) = secret_ref_of(&existing_settings, key) {
                            settings.insert(key.clone(), secret_ref_value(r#ref));
                            kept_refs.push(r#ref.to_string());
                        }
                    }
                }
                SecretChange::Clear => {
                    settings.remove(key);
                }
                SecretChange::Set(row) => {
                    settings.insert(key.clone(), secret_ref_value(&row.r#ref));
                    new_rows.push((key.clone(), row.clone()));
                }
            }
        }
        self.validate_final_settings(&input.protocol, &settings, &secret_keys, typed.as_ref())?;

        // AssertSqlSafe: get() と同じ理由 - COLUMNS 定数のみを埋め込む固定
        // 文字列。値はすべてプレースホルダでバインドする。
        let updated = sqlx::query_as::<_, PlcConnection>(sqlx::AssertSqlSafe(format!(
            "UPDATE plc_connections SET name = ?, protocol = ?, enabled = ?, \
             simulation = COALESCE(?, simulation), settings = ?, settings_version = ? \
             WHERE id = ? RETURNING {COLUMNS}"
        )))
        .bind(input.name.trim())
        .bind(&input.protocol)
        .bind(input.enabled)
        .bind(simulation_to_store)
        .bind(Value::Object(settings).to_string())
        .bind(self.settings_version(&input.protocol))
        .bind(id)
        .fetch_one(&mut *connection)
        .await
        .map_err(|err| match err {
            sqlx::Error::RowNotFound => BantoError::NotFound {
                resource: RESOURCE.to_string(),
                id: id.to_string(),
            },
            other => map_connection_write_error(other),
        })?;
        // Design §5.3: the old row of a `Set`/`Clear`, and every row whose key the new
        // protocol does not hold as a secret, goes; the `Set` rows come in.
        delete_secret_rows_except(&mut *connection, id, &kept_refs)
            .await
            .map_err(banto_storage::storage_error)?;
        insert_secret_rows(&mut *connection, id, &new_rows)
            .await
            .map_err(banto_storage::storage_error)?;
        apply_address_rewrites(&mut *connection, &address_rewrites).await?;
        Ok(updated)
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
    ///
    /// R2b-1d: runs in its own `BEGIN IMMEDIATE` transaction ([`Self::delete_tx`]) so
    /// the connection's sealed secret rows go with it (design §5.3).
    pub async fn delete(&self, id: i64) -> Result<(), BantoError> {
        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(banto_storage::storage_error)?;
        self.delete_tx(&mut tx, id).await?;
        tx.commit().await.map_err(banto_storage::storage_error)?;
        Ok(())
    }

    /// Transaction-compatible counterpart of [`Self::delete`]. Removes the
    /// connection's sealed secret rows in the same transaction (design §5.3).
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
            return Err(reserved_delete_error());
        }
        reject_delete_if_referenced_by_push_bridge(&mut *connection, id).await?;
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
        delete_all_secret_rows(&mut *connection, id)
            .await
            .map_err(banto_storage::storage_error)?;
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
            return Err(reserved_delete_error());
        }
        // R2a-2e: DB ブリッジの読み取り先は、配下ごと消すカスケードでも消さない
        // （参照先が消えるとブリッジは二度と読めない。§3.2）。
        reject_delete_if_referenced_by_push_bridge(&mut *connection, id).await?;

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
        // R2b-1d (design §5.3): the connection's sealed secrets go in the same transaction.
        delete_all_secret_rows(&mut *connection, id)
            .await
            .map_err(banto_storage::storage_error)?;

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
        // 先に SELECT するので BEGIN IMMEDIATE（checklist §5 / `update_checked` と同じ理由）。
        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
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
    use crate::connection_secret::SealedSecretRow;
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
            cpu_type: None,
            database: None,
            username: None,
            secrets: Default::default(),
            settings: None,
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
        assert_eq!(created.cpu_type, CPU_TYPE_R);
    }

    /// #605: 作成の省略は R。更新の省略は既存の Q を保つ。未知の値は拒否し、
    /// 行は Q のまま。空文字の更新も省略と同じ（R に戻さない）。
    ///
    /// R2b-1d: `cpu_type` は `slmp` の `settings` にだけある（設計 §3.2）ので、`slmp` の
    /// 接続で確かめる。省略の保持は書き込みのトランザクションの中で今の `settings` を
    /// 読んで合わせる（§4.3）。`modbus-tcp` の行は `cpu_type` を持たず、常に R と読める。
    #[tokio::test]
    async fn cpu_type_omitted_on_update_preserves_q_and_unknown_is_rejected() {
        let svc = service().await;
        let slmp = |name: &str| {
            let mut input = sample_input(name);
            input.protocol = "slmp".to_string();
            input
        };
        let created = svc.create(slmp("cpu-line")).await.unwrap();
        assert_eq!(created.settings.get("cpu_type"), Some(&json!("R")));
        assert_eq!(created.cpu_type, "R");

        let mut q = slmp("cpu-line");
        q.cpu_type = Some("Q".to_string());
        let updated = svc.update(created.id, q).await.unwrap();
        assert_eq!(updated.cpu_type, "Q");

        let mut omit = slmp("cpu-line");
        omit.host = "192.168.1.11".to_string();
        omit.cpu_type = None;
        let kept = svc.update(created.id, omit).await.unwrap();
        assert_eq!(kept.host, "192.168.1.11");
        assert_eq!(kept.cpu_type, "Q");

        let mut blank = slmp("cpu-line");
        blank.cpu_type = Some(String::new());
        let kept_blank = svc.update(created.id, blank).await.unwrap();
        assert_eq!(kept_blank.cpu_type, "Q");

        let mut bad = slmp("cpu-line");
        bad.cpu_type = Some("L".to_string());
        match svc.update(created.id, bad).await.unwrap_err() {
            BantoError::Validation { field_errors } => {
                assert!(
                    field_errors.iter().any(|e| e.field == "cpuType"),
                    "{field_errors:?}"
                );
            }
            other => panic!("expected validation, got {other:?}"),
        }
        assert_eq!(svc.get(created.id).await.unwrap().cpu_type, "Q");

        // update_tx keeps it too (the merge reads inside the caller's transaction).
        let mut tx = svc.pool.begin().await.unwrap();
        let kept_tx = svc
            .update_tx(&mut tx, created.id, slmp("cpu-line"))
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(kept_tx.cpu_type, "Q");

        // A modbus row has no cpu_type (design §3.2); switching it to slmp without
        // naming one gives R, and a modbus payload's cpu_type is not stored.
        let mut modbus = sample_input("modbus-line");
        modbus.cpu_type = Some("Q".to_string());
        let modbus = svc.create(modbus).await.unwrap();
        assert_eq!(modbus.cpu_type, "R");
        assert!(!modbus.settings.contains_key("cpu_type"));
        let switched = svc.update(modbus.id, slmp("modbus-line")).await.unwrap();
        assert_eq!(switched.cpu_type, "R");
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

    /// chronogazer #417: [`PlcConnectionService::update_preserving_simulation`]
    /// keeps the column **as stored at `UPDATE` time**, whatever
    /// `input.simulation` says, while every other field is replaced. The
    /// "stale" `input.simulation` below stands in for a value a caller read
    /// before a concurrent explicit toggle: the explicit toggle wins.
    ///
    /// Regression check: binding `input.simulation` instead of NULL in the
    /// preserving path makes the first `assert!` fail.
    #[tokio::test]
    async fn update_preserving_simulation_keeps_the_value_stored_at_update_time() {
        let svc = service().await;
        let created = svc.create(sample_input("Keeper")).await.unwrap();
        assert!(!created.simulation);

        // A concurrent explicit toggle lands first...
        let mut on = sample_input("Keeper");
        on.simulation = true;
        assert!(svc.update(created.id, on).await.unwrap().simulation);

        // ...then an update that did not mention simulation (carrying a
        // stale `false`) must not undo it.
        let mut stale = sample_input("Keeper renamed");
        stale.simulation = false;
        stale.port = 503;
        let kept = svc
            .update_preserving_simulation(created.id, stale)
            .await
            .expect("update_preserving_simulation");
        assert!(kept.simulation, "stale value overwrote the explicit toggle");
        assert_eq!(kept.name, "Keeper renamed", "other fields are replaced");
        assert_eq!(kept.port, 503);

        // The other direction: an explicit `false` is kept against a stale `true`.
        let mut off = sample_input("Keeper renamed");
        off.simulation = false;
        assert!(!svc.update(created.id, off).await.unwrap().simulation);
        let mut stale_true = sample_input("Keeper renamed");
        stale_true.simulation = true;
        assert!(
            !svc.update_preserving_simulation(created.id, stale_true)
                .await
                .unwrap()
                .simulation
        );

        // Missing id: the same NotFound as `update`.
        let missing = svc
            .update_preserving_simulation(999_999, sample_input("nobody"))
            .await;
        assert!(
            matches!(missing, Err(BantoError::NotFound { .. })),
            "{missing:?}"
        );
    }

    /// The protocol normalization still wins in the preserving path: a
    /// `"postgres"` payload stores `simulation = false` like `update` does.
    #[tokio::test]
    async fn update_preserving_simulation_still_normalizes_postgres_to_false() {
        let svc = service().await;
        let mut sim = sample_input("To postgres");
        sim.simulation = true;
        let created = svc.create(sim).await.unwrap();
        assert!(created.simulation);

        let updated = svc
            .update_preserving_simulation(created.id, postgres_input("To postgres"))
            .await
            .expect("update to postgres");
        assert_eq!(updated.protocol, POSTGRES_PROTOCOL);
        assert!(!updated.simulation);
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

    /// The named protocol constants ([`MODBUS_PROTOCOL`]/[`VIRTUAL_PROTOCOL`]/
    /// [`POSTGRES_PROTOCOL`]) are spellings of entries that also appear as
    /// literals inside [`ALLOWED_PROTOCOLS`] (see that constant's doc
    /// comment). This pins the relationship so a rename of either copy cannot
    /// pass silently.
    #[test]
    fn the_named_protocol_constants_are_all_in_allowed_protocols() {
        for protocol in [
            MODBUS_PROTOCOL,
            VIRTUAL_PROTOCOL,
            POSTGRES_PROTOCOL,
            HUB_PROTOCOL,
            PUSH_PROTOCOL,
        ] {
            assert!(
                ALLOWED_PROTOCOLS.contains(&protocol),
                "{protocol:?} is a named constant but missing from ALLOWED_PROTOCOLS"
            );
        }
        assert_eq!(ALLOWED_PROTOCOLS[0], MODBUS_PROTOCOL);
    }

    /// R2b-1d (migration 0025, design Q14): the SQL `CHECK` on `protocol` checks only the
    /// DriverId grammar (1-64 of `[a-z0-9-]`, not starting with `-`); what exists is the
    /// registry's call. Bypasses the service layer deliberately - that is the only way to
    /// ask the schema directly what it allows - and inserts only the columns 0025 keeps,
    /// so a rejection here is the `protocol` CHECK and nothing else (PR #777 Codex P2:
    /// the old version inserted `host`/`port`, which 0025 removed, so every case failed
    /// on "no such column" and the test passed even with a broken CHECK).
    #[tokio::test]
    async fn the_sql_check_on_protocol_follows_the_driver_id_grammar() {
        let pool = banto_storage::connect_sqlite_memory()
            .await
            .expect("connect_sqlite_memory");
        migrate(&pool).await.expect("migrate");

        let insert = |name: String, protocol: String| {
            let pool = pool.clone();
            async move {
                sqlx::query(
                    "INSERT INTO plc_connections (name, protocol, enabled, simulation, settings, \
                     settings_version) VALUES (?, ?, 1, 0, '{}', 1)",
                )
                .bind(name)
                .bind(protocol)
                .execute(&pool)
                .await
            }
        };

        let longest = "a".repeat(64);
        for (i, protocol) in [
            "modbus-tcp",
            "ethernet-ip",
            "opc-ua",
            "a",
            "0",
            "x-1-",
            longest.as_str(),
        ]
        .into_iter()
        .enumerate()
        {
            insert(format!("ok-{i}"), protocol.to_string())
                .await
                .unwrap_or_else(|err| panic!("the CHECK refused the valid id {protocol:?}: {err}"));
        }

        let too_long = "a".repeat(65);
        for (i, protocol) in [
            "",
            too_long.as_str(),
            "MODBUS-TCP",
            "Slmp",
            "modbus_tcp",
            "-modbus",
            "push ",
            " push",
            "op c",
            "modbüs",
            "プロトコル",
        ]
        .into_iter()
        .enumerate()
        {
            let err = insert(format!("bad-{i}"), protocol.to_string())
                .await
                .expect_err(&format!("the CHECK accepted the malformed id {protocol:?}"));
            let text = err.to_string();
            assert!(
                text.contains("CHECK constraint failed"),
                "{protocol:?} must fail on the CHECK, not on something else: {text}"
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
                cpu_type: None,
                database: None,
                username: None,
                secrets: Default::default(),
                settings: None,
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
                cpu_type: None,
                database: None,
                username: None,
                secrets: Default::default(),
                settings: None,
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
                cpu_type: None,
                database: None,
                username: None,
                secrets: Default::default(),
                settings: None,
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
        let input = with_password(input, REF_A, "secret");
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
            cpu_type: None,
            database: Some("appdb".to_string()),
            username: Some("appuser".to_string()),
            secrets: Default::default(),
            settings: None,
        }
    }

    /// R2b-1d test helpers: a `plain-v1` sealed row (the blob is the plaintext itself)
    /// under a fixed, well-formed reference.
    fn sealed(r#ref: &str, plain: &str) -> SealedSecretRow {
        SealedSecretRow {
            r#ref: r#ref.to_string(),
            scheme: crate::SCHEME_PLAIN_V1.to_string(),
            blob: zeroize::Zeroizing::new(plain.as_bytes().to_vec()),
        }
    }

    const REF_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const REF_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const REF_C: &str = "cccccccccccccccccccccccccccccccc";

    fn with_password(
        mut input: PlcConnectionInput,
        r#ref: &str,
        plain: &str,
    ) -> PlcConnectionInput {
        input.secrets.insert(
            PASSWORD_KEY.to_string(),
            SecretChange::Set(sealed(r#ref, plain)),
        );
        input
    }

    fn with_secret_change(
        mut input: PlcConnectionInput,
        change: SecretChange,
    ) -> PlcConnectionInput {
        input.secrets.insert(PASSWORD_KEY.to_string(), change);
        input
    }

    /// The plaintext of connection `id`'s password as stored, checking that the
    /// reference in `settings` and the secret rows agree (one row per reference, no
    /// stray row). `None` = no password (no reference and no row).
    async fn stored_password(svc: &PlcConnectionService, id: i64) -> Option<String> {
        let conn = svc.get(id).await.unwrap();
        let rows = svc.secret_rows(id).await.unwrap();
        match conn.secret_ref(PASSWORD_KEY) {
            None => {
                assert!(rows.is_empty(), "rows without a reference: {rows:?}");
                None
            }
            Some(r#ref) => {
                assert_eq!(rows.len(), 1, "{rows:?}");
                assert_eq!(rows[0].sealed.r#ref, r#ref);
                assert_eq!(rows[0].key, PASSWORD_KEY);
                assert_eq!(rows[0].sealed.scheme, crate::SCHEME_PLAIN_V1);
                Some(String::from_utf8(rows[0].sealed.blob.to_vec()).unwrap())
            }
        }
    }

    /// Every secret row in the database, as `(ref, connection_id, key)`.
    async fn all_secret_rows(svc: &PlcConnectionService) -> Vec<(String, i64, String)> {
        sqlx::query_as("SELECT ref, connection_id, key FROM plc_connection_secrets ORDER BY ref")
            .fetch_all(&svc.pool)
            .await
            .unwrap()
    }

    /// PR #777 owner review P2: the connection and its secret rows are read in one
    /// snapshot. A password update that commits between the two reads (new reference +
    /// new row, old row deleted) must not make the connection read first refer to a row
    /// the second read no longer sees (`Missing` for a healthy connection). The update is
    /// committed from another pool connection while the reader holds its transaction (a
    /// file DB in WAL, like the apps').
    #[tokio::test]
    async fn connection_and_secret_rows_are_read_in_one_snapshot_across_a_password_update() {
        let dir = tempfile::tempdir().unwrap();
        let pool = banto_storage::connect_sqlite(dir.path().join("snapshot.sqlite3"))
            .await
            .expect("connect_sqlite");
        migrate(&pool).await.expect("migrate");
        let svc = PlcConnectionService::new(pool.clone());
        let id = svc
            .create(with_password(postgres_input("Pg"), REF_A, "old"))
            .await
            .unwrap()
            .id;

        let writer = svc.clone();
        let (conn, rows) = svc
            .get_with_secret_rows_between(id, || async move {
                writer
                    .update(id, with_password(postgres_input("Pg"), REF_B, "new"))
                    .await
                    .expect("the update commits while the reader holds its snapshot");
            })
            .await
            .unwrap();
        assert_eq!(conn.secret_ref(PASSWORD_KEY), Some(REF_A));
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(
            rows[0].sealed.r#ref, REF_A,
            "the row the connection refers to"
        );

        let writer = svc.clone();
        let (connections, rows) = svc
            .list_with_secret_rows_between(|| async move {
                writer
                    .update(id, with_password(postgres_input("Pg"), REF_C, "newer"))
                    .await
                    .expect("the update commits while the reader holds its snapshot");
            })
            .await
            .unwrap();
        let conn = connections.iter().find(|c| c.id == id).unwrap();
        assert_eq!(conn.secret_ref(PASSWORD_KEY), Some(REF_B));
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(
            rows[0].sealed.r#ref, REF_B,
            "the row the connection refers to"
        );

        // A read after the update sees the new pair.
        let (conn, rows) = svc.get_with_secret_rows(id).await.unwrap();
        assert_eq!(conn.secret_ref(PASSWORD_KEY), Some(REF_C));
        assert_eq!(rows[0].sealed.r#ref, REF_C);
        let (connections, rows) = svc.list_with_secret_rows().await.unwrap();
        assert_eq!(connections.len(), 1);
        assert_eq!(rows[0].sealed.r#ref, REF_C);
        assert!(matches!(
            svc.get_with_secret_rows(id + 100).await,
            Err(BantoError::NotFound { .. })
        ));
        pool.close().await;
    }

    /// A `"postgres"` connection created without a password stores none -
    /// no reference in `settings` and no secret row (`Keep` and `Clear` alike on
    /// create, the old `None`/`Some("")` rule).
    #[tokio::test]
    async fn create_postgres_without_password_stores_none() {
        let svc = service().await;
        let created = svc
            .create(postgres_input("PgNoPassword"))
            .await
            .expect("postgres without a password should be accepted");
        assert_eq!(created.database.as_deref(), Some("appdb"));
        assert_eq!(created.username.as_deref(), Some("appuser"));
        assert_eq!(created.secret_ref(PASSWORD_KEY), None);
        assert_eq!(stored_password(&svc, created.id).await, None);
        let cleared = svc
            .create(with_secret_change(
                postgres_input("PgCleared"),
                SecretChange::Clear,
            ))
            .await
            .unwrap();
        assert_eq!(stored_password(&svc, cleared.id).await, None);
        assert!(all_secret_rows(&svc).await.is_empty());
    }

    /// A `"postgres"` connection created with a sealed password stores the row and
    /// a reference to it; `settings` never carries the value.
    #[tokio::test]
    async fn create_postgres_with_password_stores_it() {
        let svc = service().await;
        let input = with_password(postgres_input("PgWithPassword"), REF_A, "s3cret");
        let created = svc.create(input).await.expect("create should succeed");
        assert_eq!(created.secret_ref(PASSWORD_KEY), Some(REF_A));
        assert_eq!(
            created.settings,
            json!({
                "host": "10.0.0.50", "port": 5432, "database": "appdb",
                "username": "appuser", "password": {"$secret": REF_A}
            })
            .as_object()
            .unwrap()
            .clone()
        );
        assert!(!Value::Object(created.settings.clone())
            .to_string()
            .contains("s3cret"));
        assert_eq!(
            stored_password(&svc, created.id).await.as_deref(),
            Some("s3cret")
        );
    }

    /// Update's tri-state (design §5.3): `Keep` (no entry) keeps whatever is
    /// already stored - reference and row.
    #[tokio::test]
    async fn update_with_password_none_keeps_the_existing_password() {
        let svc = service().await;
        let created = svc
            .create(with_password(postgres_input("PgKeep"), REF_A, "original"))
            .await
            .unwrap();

        let updated = svc
            .update(created.id, postgres_input("PgKeep"))
            .await
            .unwrap();
        assert_eq!(updated.secret_ref(PASSWORD_KEY), Some(REF_A));
        assert_eq!(
            stored_password(&svc, created.id).await.as_deref(),
            Some("original")
        );
        let explicit_keep = with_secret_change(postgres_input("PgKeep"), SecretChange::Keep);
        svc.update(created.id, explicit_keep).await.unwrap();
        assert_eq!(
            stored_password(&svc, created.id).await.as_deref(),
            Some("original")
        );
    }

    /// `Clear` removes a previously-stored password - reference and row.
    #[tokio::test]
    async fn update_with_password_empty_string_clears_it() {
        let svc = service().await;
        let created = svc
            .create(with_password(postgres_input("PgClear"), REF_A, "original"))
            .await
            .unwrap();

        let update_input = with_secret_change(postgres_input("PgClear"), SecretChange::Clear);
        let updated = svc.update(created.id, update_input).await.unwrap();
        assert_eq!(updated.secret_ref(PASSWORD_KEY), None);
        assert_eq!(stored_password(&svc, created.id).await, None);
        assert!(all_secret_rows(&svc).await.is_empty());
    }

    /// `Set` replaces the stored password: a new row under a new reference, the
    /// old row gone, `settings` pointing at the new one.
    #[tokio::test]
    async fn update_with_password_some_replaces_it() {
        let svc = service().await;
        let created = svc
            .create(with_password(
                postgres_input("PgReplace"),
                REF_A,
                "original",
            ))
            .await
            .unwrap();

        let update_input = with_password(postgres_input("PgReplace"), REF_B, "replacement");
        let updated = svc.update(created.id, update_input).await.unwrap();
        assert_eq!(updated.secret_ref(PASSWORD_KEY), Some(REF_B));
        assert_eq!(
            stored_password(&svc, created.id).await.as_deref(),
            Some("replacement")
        );
        assert_eq!(
            all_secret_rows(&svc).await,
            [(REF_B.to_string(), created.id, PASSWORD_KEY.to_string())]
        );
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
                cpu_type: None,
                database: None,
                username: None,
                secrets: Default::default(),
                settings: None,
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
                cpu_type: None,
                database: None,
                username: None,
                secrets: Default::default(),
                settings: None,
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

        // R2b-1d: a row whose `settings` has no `word_order` (migration 0025 always
        // writes one; a hand-edited row may not) reads as the protocol's default.
        sqlx::query(
            "INSERT INTO plc_connections (name, protocol, settings) \
             VALUES ('Legacy', 'slmp', '{\"host\":\"192.168.1.20\",\"port\":5007}')",
        )
        .execute(&pool)
        .await
        .expect("insert without word_order");

        let svc = PlcConnectionService::new(pool);
        let rows = svc
            .list(ListParams::default())
            .await
            .expect("list should succeed");
        assert_eq!(rows.rows[0].word_order, "low_high");
    }

    // --- #325 (2026-09-08): Modbus defaults to "high_low" ------------------

    /// A `PlcConnectionInput` that leaves `wordOrder` out deserializes with
    /// the `""` sentinel, *not* with a real word order - the mechanism the
    /// protocol-dependent default is built on
    /// ([`PlcConnectionInput::word_order`]'s doc comment). Asserted at the
    /// serde layer specifically, because every behavioural test below would
    /// still pass if the sentinel silently became `"low_high"` again for
    /// non-Modbus protocols.
    #[test]
    fn an_omitted_word_order_deserializes_to_the_unspecified_sentinel() {
        let input: PlcConnectionInput = serde_json::from_value(json!({
            "name": "Line1",
            "protocol": "modbus-tcp",
            "host": "192.168.1.10",
            "port": 502,
        }))
        .expect("a payload without wordOrder should still deserialize");
        assert_eq!(input.word_order, "");
    }

    /// The headline of #325's registry half: a **new** `"modbus-tcp"`
    /// connection whose payload does not name a word order is stored as
    /// `"high_low"`, the Modbus/IEEE convention the collection path was
    /// already using in practice (this module's doc comment, "`word_order` is
    /// a Modbus setting too"). Driven through `serde` rather than by setting
    /// `String::new()` by hand, so it exercises the same path a REST/MCP
    /// payload takes.
    #[tokio::test]
    async fn a_new_modbus_connection_defaults_to_high_low() {
        let svc = service().await;
        let input: PlcConnectionInput = serde_json::from_value(json!({
            "name": "Modbus default",
            "protocol": "modbus-tcp",
            "host": "192.168.1.10",
            "port": 502,
        }))
        .expect("deserialize");
        let created = svc.create(input).await.expect("create should succeed");
        assert_eq!(created.word_order, WORD_ORDER_HIGH_LOW);
        assert_eq!(
            svc.get(created.id).await.unwrap().word_order,
            WORD_ORDER_HIGH_LOW
        );
    }

    /// The other half of the same rule: `"slmp"` is untouched by #325 and
    /// still defaults to MELSEC's own `"low_high"` (migration `0010`'s column
    /// default), so no existing SLMP client changes behaviour.
    #[tokio::test]
    async fn a_new_slmp_connection_still_defaults_to_low_high() {
        let svc = service().await;
        let input: PlcConnectionInput = serde_json::from_value(json!({
            "name": "SLMP default",
            "protocol": "slmp",
            "host": "192.168.1.20",
            "port": 5007,
        }))
        .expect("deserialize");
        let created = svc.create(input).await.expect("create should succeed");
        assert_eq!(created.word_order, WORD_ORDER_LOW_HIGH);
    }

    /// The default must not become a *policy*: an operator who deliberately
    /// picks `"low_high"` for a Modbus connection (a gateway that presents
    /// MELSEC-order registers, say) gets `"low_high"` stored, on `create` and
    /// on a later `update` alike. This is the distinction the `""` sentinel
    /// exists to make - "omitted" and "explicitly low_high" are different
    /// payloads and must stay different.
    #[tokio::test]
    async fn an_explicit_low_high_is_kept_on_a_modbus_connection() {
        let svc = service().await;
        let input: PlcConnectionInput = serde_json::from_value(json!({
            "name": "Modbus explicit",
            "protocol": "modbus-tcp",
            "host": "192.168.1.10",
            "port": 502,
            "word_order": "low_high",
        }))
        .expect("deserialize");
        let created = svc.create(input).await.expect("create should succeed");
        assert_eq!(created.word_order, WORD_ORDER_LOW_HIGH);

        let update: PlcConnectionInput = serde_json::from_value(json!({
            "name": "Modbus explicit",
            "protocol": "modbus-tcp",
            "host": "192.168.1.10",
            "port": 502,
            "word_order": "low_high",
        }))
        .expect("deserialize");
        let updated = svc
            .update(created.id, update)
            .await
            .expect("update should succeed");
        assert_eq!(updated.word_order, WORD_ORDER_LOW_HIGH);
        assert_eq!(
            svc.get(created.id).await.unwrap().word_order,
            WORD_ORDER_LOW_HIGH
        );
    }

    /// `update` is a full replacement (`PUT`-shaped, not a PATCH - see
    /// `PlcConnectionInput::password`'s doc comment for the one field that
    /// deviates), so an update payload that omits `wordOrder` gets the same
    /// protocol-dependent default a create would: a Modbus row switches back
    /// to `"high_low"`.
    #[tokio::test]
    async fn an_omitted_word_order_on_update_falls_back_to_the_protocol_default() {
        let svc = service().await;
        let mut input = sample_input("Modbus update");
        input.word_order = WORD_ORDER_LOW_HIGH.to_string();
        let created = svc.create(input).await.expect("create should succeed");
        assert_eq!(created.word_order, WORD_ORDER_LOW_HIGH);

        let update: PlcConnectionInput = serde_json::from_value(json!({
            "name": "Modbus update",
            "protocol": "modbus-tcp",
            "host": "192.168.1.10",
            "port": 502,
        }))
        .expect("deserialize");
        let updated = svc
            .update(created.id, update)
            .await
            .expect("update should succeed");
        assert_eq!(updated.word_order, WORD_ORDER_HIGH_LOW);
    }

    /// Switching an existing connection's protocol re-decides the default
    /// too, since the default is a function of the payload's `protocol` and
    /// nothing else - `update_can_switch_a_connection_between_protocols`'s
    /// `word_order` counterpart.
    #[tokio::test]
    async fn the_default_follows_the_protocol_across_an_update() {
        let svc = service().await;
        let created = svc
            .create(
                serde_json::from_value(json!({
                    "name": "Switcher",
                    "protocol": "modbus-tcp",
                    "host": "192.168.1.10",
                    "port": 502,
                }))
                .expect("deserialize"),
            )
            .await
            .expect("create should succeed");
        assert_eq!(created.word_order, WORD_ORDER_HIGH_LOW);

        let updated = svc
            .update(
                created.id,
                serde_json::from_value(json!({
                    "name": "Switcher",
                    "protocol": "slmp",
                    "host": "192.168.1.10",
                    "port": 5007,
                }))
                .expect("deserialize"),
            )
            .await
            .expect("update should succeed");
        assert_eq!(updated.word_order, WORD_ORDER_LOW_HIGH);
    }

    /// `"postgres"` keeps winning: `normalize_postgres_input` runs *after*
    /// the word-order default (see `normalize_plc_connection_input`), so a DB
    /// connection carries the plain column default no matter what the caller
    /// sent - including nothing at all.
    #[tokio::test]
    async fn a_postgres_connection_ignores_the_word_order_default() {
        let svc = service().await;
        for word_order in [None, Some("high_low"), Some("low_high")] {
            let mut payload = json!({
                "name": format!("db-{word_order:?}"),
                "protocol": "postgres",
                "host": "db.example.com",
                "port": 5432,
                "database": "appdb",
                "username": "appuser",
            });
            if let Some(word_order) = word_order {
                payload["word_order"] = json!(word_order);
            }
            let created = svc
                .create(serde_json::from_value(payload).expect("deserialize"))
                .await
                .expect("create should succeed");
            assert_eq!(created.word_order, WORD_ORDER_LOW_HIGH);
        }
    }

    /// [`WORD_ORDER_LOW_HIGH`]/[`WORD_ORDER_HIGH_LOW`] are spellings of the
    /// entries that also appear as literals inside [`ALLOWED_WORD_ORDERS`] -
    /// the `word_order` twin of
    /// `the_named_protocol_constants_are_all_in_allowed_protocols`.
    #[test]
    fn the_named_word_order_constants_are_all_in_allowed_word_orders() {
        for word_order in [WORD_ORDER_LOW_HIGH, WORD_ORDER_HIGH_LOW] {
            assert!(
                ALLOWED_WORD_ORDERS.contains(&word_order),
                "{word_order:?} is a named constant but missing from ALLOWED_WORD_ORDERS"
            );
        }
        assert_eq!(ALLOWED_WORD_ORDERS[0], WORD_ORDER_LOW_HIGH);
        // The sentinel must never collide with a real value.
        assert!(!ALLOWED_WORD_ORDERS.contains(&""));
    }

    // --- migration 0017 (backfill: modbus-tcp -> 'high_low') ---------------

    /// Migration `0017` is a plain `UPDATE`, not a table rebuild, so this is
    /// not a "did the rebuild drop a column" test like
    /// `migration_0014_preserves_rows_and_foreign_keys_on_a_populated_database`
    /// - it is a "did the `WHERE` clause hit exactly the right rows" test.
    /// The recipe is the same one `tag.rs`'s
    /// `migration_0016_preserves_rows_and_widens_data_type_on_a_populated_database`
    /// uses: run the whole 0001-0016 chain by hand (a faithful stand-in for a
    /// deployed pre-0017 database), seed rows, then apply 0017 the way sqlx's
    /// `Migrate::apply` does - the whole file as one multi-statement
    /// `execute`, on a single pinned connection, inside one transaction.
    ///
    /// The seeded rows are the three cases the migration's header calls out:
    /// a `"modbus-tcp"` row at the old default `'low_high'` (the row the
    /// back-fill exists for - every deployed Modbus connection looks like
    /// this, because `banto-hub`'s form never offered the control), an
    /// `"slmp"` row at `'low_high'` (correct as-is, must not move), and a
    /// `"virtual"` row (the column is inert for it, must not move either).
    /// A second `"modbus-tcp"` row already at `'high_low'` is seeded too, to
    /// pin that the migration is a no-op for it rather than accidentally
    /// inverting.
    #[tokio::test]
    async fn migration_0017_backfills_only_modbus_rows_to_high_low() {
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
            (
                "0014",
                include_str!("../migrations/0014_plc_connections_allow_postgres.sql"),
            ),
            (
                "0015",
                include_str!("../migrations/0015_db_source_query_sql_and_tag_kind.sql"),
            ),
            (
                "0016",
                include_str!("../migrations/0016_tags_allow_64bit.sql"),
            ),
        ] {
            conn.execute(sql)
                .await
                .unwrap_or_else(|e| panic!("pre-0017 migration {label} failed: {e}"));
        }

        conn.execute(
            "INSERT INTO plc_connections (id, name, protocol, host, port, word_order) VALUES \
             (1, 'Modbus legacy', 'modbus-tcp', '192.168.1.10', 502, 'low_high'), \
             (2, 'Modbus already correct', 'modbus-tcp', '192.168.1.11', 502, 'high_low'), \
             (3, 'MELSEC', 'slmp', '192.168.1.20', 5007, 'low_high'), \
             (4, 'calc', 'virtual', '', 0, 'low_high'), \
             (5, 'ext db', 'postgres', 'db.example.com', 5432, 'low_high')",
        )
        .await
        .expect("seed connections");
        conn.execute(
            "UPDATE plc_connections SET database = 'appdb', username = 'appuser' WHERE id = 5",
        )
        .await
        .expect("seed postgres credentials");

        let migration = include_str!("../migrations/0017_modbus_word_order_high_low.sql");
        let mut tx = conn.begin().await.expect("begin, as the migrator does");
        tx.execute(migration).await.expect("0017 should apply");
        tx.commit().await.expect("0017 should commit");

        let rows: Vec<(i64, String, String)> =
            sqlx::query_as("SELECT id, protocol, word_order FROM plc_connections ORDER BY id")
                .fetch_all(&mut *conn)
                .await
                .expect("read back every seeded connection");
        assert_eq!(
            rows,
            vec![
                (1, "modbus-tcp".to_string(), "high_low".to_string()),
                (2, "modbus-tcp".to_string(), "high_low".to_string()),
                (3, "slmp".to_string(), "low_high".to_string()),
                (4, "virtual".to_string(), "low_high".to_string()),
                (5, "postgres".to_string(), "low_high".to_string()),
            ]
        );
    }

    /// The back-fill must be a one-shot historical correction, not a standing
    /// rule: once `migrate` has run, an operator may deliberately set a
    /// Modbus connection back to `"low_high"`, and re-running `migrate` (i.e.
    /// the next process start - `migrate_is_idempotent` at the crate root
    /// covers the "does not error" half) must leave that choice alone.
    /// sqlx's bookkeeping (`crate::MIGRATIONS_TABLE`) is what guarantees this; this
    /// test pins the consequence rather than the mechanism.
    #[tokio::test]
    async fn rerunning_migrate_does_not_reapply_the_0017_backfill() {
        let pool = banto_storage::connect_sqlite_memory()
            .await
            .expect("connect_sqlite_memory");
        migrate(&pool).await.expect("migrate");

        let svc = PlcConnectionService::new(pool.clone());
        let mut input = sample_input("Deliberate low_high");
        input.word_order = WORD_ORDER_LOW_HIGH.to_string();
        let created = svc.create(input).await.expect("create should succeed");
        assert_eq!(created.word_order, WORD_ORDER_LOW_HIGH);

        migrate(&pool).await.expect("migrate should be idempotent");

        assert_eq!(
            svc.get(created.id).await.unwrap().word_order,
            WORD_ORDER_LOW_HIGH
        );
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
                cpu_type: None,
                database: None,
                username: None,
                secrets: Default::default(),
                settings: None,
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

        // R2b-1d: `port` moved into `settings` and is no longer a list column
        // (design §7) - sort/filter by `name` instead.
        let result = svc
            .list(ListParams {
                sort: vec![SortState {
                    field: "name".to_string(),
                    direction: SortDirection::Desc,
                }],
                filters: vec![FilterState {
                    field: "name".to_string(),
                    op: FilterOp::Gte,
                    value: json!("B"),
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

    // --- #383 段階3 P2: "hub" (this module's doc comment, "`\"hub\"`" section).

    fn hub_input(name: &str) -> PlcConnectionInput {
        let mut input = sample_input(name);
        input.protocol = HUB_PROTOCOL.to_string();
        input
    }

    fn only_field_error(err: BantoError) -> FieldError {
        match err {
            BantoError::Validation { mut field_errors } => {
                assert_eq!(field_errors.len(), 1, "{field_errors:?}");
                field_errors.remove(0)
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    async fn add_group(svc: &PlcConnectionService, connection_id: i64, name: &str) {
        sqlx::query(
            "INSERT INTO collection_groups (name, plc_connection_id, period_ms, enabled) \
             VALUES (?, ?, 1000, 1)",
        )
        .bind(name)
        .bind(connection_id)
        .execute(&svc.pool)
        .await
        .unwrap();
    }

    /// A hub connection has no endpoint or wire settings of its own: whatever
    /// the caller sent for host/port/unitId/wordOrder/simulation is replaced
    /// by ""/0/the column defaults/false, on create and on update.
    #[tokio::test]
    async fn a_hub_connection_normalizes_every_wire_field() {
        let svc = service().await;
        let mut input = hub_input("Hub");
        input.host = "10.0.0.5".to_string();
        input.port = 8722;
        input.unit_id = 9;
        input.word_order = WORD_ORDER_HIGH_LOW.to_string();
        input.simulation = true;
        let created = svc.create(input.clone()).await.expect("hub create");
        assert_eq!(created.protocol, HUB_PROTOCOL);
        assert_eq!(created.host, "");
        assert_eq!(created.port, 0);
        assert_eq!(created.unit_id, 1);
        assert_eq!(created.word_order, WORD_ORDER_LOW_HIGH);
        assert!(!created.simulation);
        assert!(created.is_hub_source());
        assert!(!created.is_db_source());

        let updated = svc.update(created.id, input.clone()).await.expect("update");
        assert_eq!((updated.host.as_str(), updated.port), ("", 0));
        assert!(!updated.simulation);

        let preserved = svc
            .update_preserving_simulation(created.id, input)
            .await
            .expect("update_preserving_simulation");
        assert!(!preserved.simulation);
    }

    /// Unlike every PLC protocol, a hub connection is valid with an empty host
    /// and port 0 (it dials nothing itself).
    #[tokio::test]
    async fn a_hub_connection_accepts_empty_host_and_zero_port() {
        let svc = service().await;
        let mut input = hub_input("Hub");
        input.host = String::new();
        input.port = 0;
        svc.create(input).await.expect("hub needs no host/port");
    }

    #[tokio::test]
    async fn a_hub_connection_rejects_database_credentials() {
        let svc = service().await;
        let mut input = hub_input("Hub");
        input.database = Some("db".to_string());
        input.username = Some("user".to_string());
        let input = with_password(input, REF_A, "pw");
        match svc.create(input).await.unwrap_err() {
            BantoError::Validation { field_errors } => {
                let fields: Vec<&str> = field_errors.iter().map(|e| e.field.as_str()).collect();
                assert_eq!(fields, vec!["database", "username", "password"]);
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    /// At most one hub connection per registry (one Hub, H1): a second one is
    /// a `protocol` field error, whether it arrives as a create or as an
    /// update that switches another connection to "hub" - on both the pool
    /// and the transaction paths.
    #[tokio::test]
    async fn only_one_hub_connection_is_allowed() {
        let svc = service().await;
        svc.create(hub_input("Hub")).await.expect("first hub");

        let err = only_field_error(svc.create(hub_input("Hub2")).await.unwrap_err());
        assert_eq!(err.field, "protocol");
        assert_eq!(err.message, SECOND_HUB_MESSAGE);

        let mut tx = svc.pool.begin().await.unwrap();
        let err = only_field_error(svc.create_tx(&mut tx, hub_input("Hub3")).await.unwrap_err());
        assert_eq!(err.field, "protocol");
        drop(tx);

        let plc = svc.create(sample_input("PLC")).await.unwrap();
        let err = only_field_error(svc.update(plc.id, hub_input("PLC")).await.unwrap_err());
        assert_eq!(err.field, "protocol");
        assert_eq!(err.message, SECOND_HUB_MESSAGE);
        let mut tx = svc.pool.begin().await.unwrap();
        let err = only_field_error(
            svc.update_tx(&mut tx, plc.id, hub_input("PLC"))
                .await
                .unwrap_err(),
        );
        assert_eq!(err.field, "protocol");
        drop(tx);

        // Other protocols are unaffected by the partial index, and a name
        // clash is still reported on `name`, not mistaken for the hub limit.
        svc.create(sample_input("PLC2"))
            .await
            .expect("second modbus");
        let err = only_field_error(svc.create(sample_input("PLC2")).await.unwrap_err());
        assert_eq!(err.field, "name");
    }

    /// Once the only hub connection is gone, a new one can be created.
    #[tokio::test]
    async fn a_hub_connection_can_be_recreated_after_delete() {
        let svc = service().await;
        let hub = svc.create(hub_input("Hub")).await.unwrap();
        svc.delete(hub.id).await.unwrap();
        svc.create(hub_input("Hub")).await.expect("recreate");
    }

    /// Switching a connection across the "hub" boundary is refused while it
    /// has collection groups (their tags' addresses would stop making sense),
    /// and allowed while it has none - in both directions, on both paths.
    #[tokio::test]
    async fn switching_to_or_from_hub_is_refused_while_groups_exist() {
        let svc = service().await;
        let plc = svc.create(sample_input("PLC")).await.unwrap();
        add_group(&svc, plc.id, "G-plc").await;

        let err = only_field_error(svc.update(plc.id, hub_input("PLC")).await.unwrap_err());
        assert_eq!(err.field, "protocol");
        assert!(err.message.contains("1件"), "{}", err.message);
        let mut tx = svc.pool.begin().await.unwrap();
        let err = only_field_error(
            svc.update_tx(&mut tx, plc.id, hub_input("PLC"))
                .await
                .unwrap_err(),
        );
        assert_eq!(err.field, "protocol");
        drop(tx);

        let hub = svc.create(hub_input("Hub")).await.unwrap();
        add_group(&svc, hub.id, "G-hub").await;
        let mut to_slmp = sample_input("Hub");
        to_slmp.protocol = "slmp".to_string();
        let err = only_field_error(svc.update(hub.id, to_slmp.clone()).await.unwrap_err());
        assert_eq!(err.field, "protocol");
        let mut tx = svc.pool.begin().await.unwrap();
        let err = only_field_error(
            svc.update_tx(&mut tx, hub.id, to_slmp.clone())
                .await
                .unwrap_err(),
        );
        assert_eq!(err.field, "protocol");
        drop(tx);

        // Edits that stay on the same side of the boundary are unaffected.
        let mut renamed = hub_input("Hub renamed");
        renamed.enabled = false;
        svc.update(hub.id, renamed).await.expect("rename hub");
        let mut to_slmp_plc = sample_input("PLC");
        to_slmp_plc.protocol = "slmp".to_string();
        svc.update(plc.id, to_slmp_plc)
            .await
            .expect("modbus -> slmp");

        // With no groups, the switch is allowed.
        sqlx::query("DELETE FROM collection_groups WHERE plc_connection_id = ?")
            .bind(hub.id)
            .execute(&svc.pool)
            .await
            .unwrap();
        svc.delete(hub.id).await.unwrap();
        let empty = svc.create(sample_input("Empty")).await.unwrap();
        let switched = svc
            .update(empty.id, hub_input("Empty"))
            .await
            .expect("an empty connection may become the hub");
        assert_eq!(switched.protocol, HUB_PROTOCOL);
        assert_eq!((switched.host.as_str(), switched.port), ("", 0));
    }

    #[test]
    fn hub_protocol_switch_error_table() {
        let cases: &[(Option<&str>, &str, i64, bool)] = &[
            (Some("modbus-tcp"), "hub", 1, true),
            (Some("hub"), "slmp", 2, true),
            (Some("modbus-tcp"), "hub", 0, false),
            (Some("hub"), "hub", 5, false),
            (Some("modbus-tcp"), "slmp", 5, false),
            (None, "hub", 5, false),
        ];
        for &(existing, new, groups, refused) in cases {
            assert_eq!(
                hub_protocol_switch_error(existing, new, groups).is_some(),
                refused,
                "{existing:?} -> {new} with {groups} groups"
            );
        }
    }

    /// Migration 0019 rebuilds `plc_connections` against the full post-0018
    /// schema (0014's credential columns, 0015's `query_sql`, 0018's 19 tag
    /// columns). Applied the way sqlx applies it (whole file, one pinned
    /// connection, one transaction) to a populated database: every row comes
    /// back intact, foreign keys and the children's indexes still hold, every
    /// AUTOINCREMENT high-water mark survives (a freed connection id is not
    /// reused), "hub" is accepted once, a second "hub" and unknown protocols
    /// are not.
    #[tokio::test]
    async fn migration_0019_preserves_rows_sequence_and_foreign_keys_on_a_populated_database() {
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
            (
                "0014",
                include_str!("../migrations/0014_plc_connections_allow_postgres.sql"),
            ),
            (
                "0015",
                include_str!("../migrations/0015_db_source_query_sql_and_tag_kind.sql"),
            ),
            (
                "0016",
                include_str!("../migrations/0016_tags_allow_64bit.sql"),
            ),
            (
                "0017",
                include_str!("../migrations/0017_modbus_word_order_high_low.sql"),
            ),
            (
                "0018",
                include_str!("../migrations/0018_tags_drop_thresholds.sql"),
            ),
        ] {
            conn.execute(sql)
                .await
                .unwrap_or_else(|e| panic!("pre-0019 migration {label} failed: {e}"));
        }

        // Connection 9 is created and deleted so the high-water mark (9) is
        // above every surviving id (7, 8) - the case a plain copy would lose.
        conn.execute(
            "INSERT INTO plc_connections \
             (id, name, protocol, host, port, unit_id, enabled, simulation, word_order) \
             VALUES (7, 'Line1 PLC', 'slmp', '192.168.1.10', 5007, 3, 0, 1, 'high_low')",
        )
        .await
        .expect("seed slmp connection");
        conn.execute(
            "INSERT INTO plc_connections \
             (id, name, protocol, host, port, database, username, password) \
             VALUES (8, 'ERP DB', 'postgres', '10.0.0.50', 5432, 'erp', 'reader', 'secret')",
        )
        .await
        .expect("seed postgres connection");
        conn.execute(
            "INSERT INTO plc_connections (id, name, protocol, host, port) \
             VALUES (9, 'Gone', 'modbus-tcp', '1.2.3.4', 502)",
        )
        .await
        .expect("seed doomed connection");
        conn.execute("DELETE FROM plc_connections WHERE id = 9")
            .await
            .expect("delete doomed connection");
        conn.execute(
            "INSERT INTO collection_groups \
             (id, name, plc_connection_id, period_ms, enabled, default_writable) \
             VALUES (4, 'G1', 7, 1000, 1, 0)",
        )
        .await
        .expect("seed plc group");
        conn.execute(
            "INSERT INTO collection_groups \
             (id, name, plc_connection_id, period_ms, enabled, default_writable, query_sql) \
             VALUES (5, 'Q1', 8, 5000, 1, 1, 'SELECT 1 AS v')",
        )
        .await
        .expect("seed db group");
        conn.execute(
            "INSERT INTO tags (\
                id, name, collection_group_id, address, data_type, string_length, \
                raw_lo, raw_hi, eng_lo, eng_hi, unit, decimals, enabled, \
                writable, tag_kind, expression, retain, revision, string_encoding\
             ) VALUES (\
                11, 'T1', 4, 'D100', 'i16', NULL, \
                0, 100, 0, 50, 'degC', 2, 1, \
                1, 'plc', NULL, 0, 3, 'shift_jis'\
             )",
        )
        .await
        .expect("seed plc tag");
        conn.execute(
            "INSERT INTO tags (id, name, collection_group_id, address, data_type, tag_kind, writable) \
             VALUES (12, 'V', 5, 'v', 'f64', 'db', 0)",
        )
        .await
        .expect("seed db tag");
        let sequences_before: Vec<(String, i64)> =
            sqlx::query_as("SELECT name, seq FROM sqlite_sequence ORDER BY name")
                .fetch_all(&mut *conn)
                .await
                .unwrap();
        assert!(sequences_before.contains(&("plc_connections".to_string(), 9)));

        let migration = include_str!("../migrations/0019_plc_connections_allow_hub.sql");
        let mut tx = conn.begin().await.expect("begin, as the migrator does");
        tx.execute(migration).await.expect("0019 should apply");
        tx.commit().await.expect("0019 should commit");

        #[allow(clippy::type_complexity)]
        let connections: Vec<(
            i64,
            String,
            String,
            String,
            i64,
            i64,
            bool,
            bool,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
        )> = sqlx::query_as(
            "SELECT id, name, protocol, host, port, unit_id, enabled, simulation, word_order, \
                 database, username, password FROM plc_connections ORDER BY id",
        )
        .fetch_all(&mut *conn)
        .await
        .unwrap();
        assert_eq!(
            connections,
            vec![
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
                    None,
                    None,
                    None,
                ),
                (
                    8,
                    "ERP DB".to_string(),
                    "postgres".to_string(),
                    "10.0.0.50".to_string(),
                    5432,
                    1,
                    true,
                    false,
                    "low_high".to_string(),
                    Some("erp".to_string()),
                    Some("reader".to_string()),
                    Some("secret".to_string()),
                ),
            ]
        );

        #[allow(clippy::type_complexity)]
        let groups: Vec<(i64, String, i64, i64, bool, bool, Option<String>)> = sqlx::query_as(
            "SELECT id, name, plc_connection_id, period_ms, enabled, default_writable, query_sql \
             FROM collection_groups ORDER BY id",
        )
        .fetch_all(&mut *conn)
        .await
        .unwrap();
        assert_eq!(
            groups,
            vec![
                (4, "G1".to_string(), 7, 1000, true, false, None),
                (
                    5,
                    "Q1".to_string(),
                    8,
                    5000,
                    true,
                    true,
                    Some("SELECT 1 AS v".to_string())
                ),
            ]
        );

        #[allow(clippy::type_complexity)]
        let tags: Vec<(
            i64,
            String,
            i64,
            String,
            String,
            Option<f64>,
            Option<String>,
            i64,
            bool,
            String,
            i64,
            String,
        )> = sqlx::query_as(
            "SELECT id, name, collection_group_id, address, data_type, eng_hi, unit, decimals, \
                 writable, tag_kind, revision, string_encoding FROM tags ORDER BY id",
        )
        .fetch_all(&mut *conn)
        .await
        .unwrap();
        assert_eq!(
            tags,
            vec![
                (
                    11,
                    "T1".to_string(),
                    4,
                    "D100".to_string(),
                    "i16".to_string(),
                    Some(50.0),
                    Some("degC".to_string()),
                    2,
                    true,
                    "plc".to_string(),
                    3,
                    "shift_jis".to_string(),
                ),
                (
                    12,
                    "V".to_string(),
                    5,
                    "v".to_string(),
                    "f64".to_string(),
                    None,
                    None,
                    0,
                    false,
                    "db".to_string(),
                    1,
                    "utf8".to_string(),
                ),
            ]
        );

        // Every AUTOINCREMENT high-water mark is exactly what it was.
        let sequences_after: Vec<(String, i64)> =
            sqlx::query_as("SELECT name, seq FROM sqlite_sequence ORDER BY name")
                .fetch_all(&mut *conn)
                .await
                .unwrap();
        assert_eq!(sequences_after, sequences_before);

        let violations: Vec<(String,)> = sqlx::query_as("PRAGMA foreign_key_check")
            .fetch_all(&mut *conn)
            .await
            .unwrap();
        assert!(
            violations.is_empty(),
            "dangling foreign keys: {violations:?}"
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
        // The children's indexes survive (their tables were never dropped),
        // and the new partial unique index exists.
        let indexes: Vec<(String,)> = sqlx::query_as(
            "SELECT name FROM sqlite_master WHERE type = 'index' AND name LIKE 'idx_%' ORDER BY name",
        )
        .fetch_all(&mut *conn)
        .await
        .unwrap();
        assert_eq!(
            indexes,
            vec![
                ("idx_collection_groups_plc_connection_id".to_string(),),
                ("idx_plc_connections_single_hub".to_string(),),
                ("idx_tags_collection_group_id".to_string(),),
            ]
        );

        // A freed id is not reused: the next connection gets 10, not 9.
        let next_id: i64 = sqlx::query_scalar(
            "INSERT INTO plc_connections (name, protocol, host, port) \
             VALUES ('Hub', 'hub', '', 0) RETURNING id",
        )
        .fetch_one(&mut *conn)
        .await
        .expect("hub should be accepted after the rebuild");
        assert_eq!(next_id, 10);
        assert!(
            sqlx::query(
                "INSERT INTO plc_connections (name, protocol, host, port) \
                 VALUES ('Hub2', 'hub', '', 0)",
            )
            .execute(&mut *conn)
            .await
            .is_err(),
            "a second hub row must be refused by the partial unique index"
        );
        assert!(
            sqlx::query(
                "INSERT INTO plc_connections (name, protocol, host, port) \
                 VALUES ('Nope', 'opc-ua', '1.2.3.4', 4840)",
            )
            .execute(&mut *conn)
            .await
            .is_err(),
            "an unknown protocol must still be refused"
        );
    }

    // ---- R2a-2a: "push" connections and `settings` ----------------------

    fn push_input(name: &str) -> PlcConnectionInput {
        PlcConnectionInput {
            name: name.to_string(),
            protocol: PUSH_PROTOCOL.to_string(),
            host: "ignored.example".to_string(),
            port: 502,
            unit_id: 7,
            enabled: true,
            simulation: true,
            word_order: "high_low".to_string(),
            cpu_type: None,
            database: None,
            username: None,
            secrets: Default::default(),
            settings: None,
        }
    }

    fn bridge_settings(db_connection_id: i64) -> serde_json::Value {
        json!({
            "stale_timeout_ms": 5000,
            "bridge": { "kind": "db", "db_connection_id": db_connection_id, "queries": [
                { "name": "line1", "period_ms": 1000, "sql": "SELECT temp FROM line1_now",
                  "columns": [ { "column": "temp", "key": "line1.temp" } ] } ] }
        })
    }

    /// Like `"hub"`: every wire/PLC field is normalized (not rejected), and
    /// there is no per-registry limit.
    #[tokio::test]
    async fn a_push_connection_normalizes_every_wire_field() {
        let svc = service().await;
        let created = svc
            .create(push_input("Bridge1"))
            .await
            .expect("create push");
        assert_eq!(created.protocol, PUSH_PROTOCOL);
        assert_eq!(created.host, "");
        assert_eq!(created.port, 0);
        assert_eq!(created.unit_id, 1);
        assert_eq!(created.word_order, WORD_ORDER_LOW_HIGH);
        assert!(!created.simulation);
        assert!(created.settings.is_empty());
        svc.create(push_input("Bridge2"))
            .await
            .expect("push connections have no per-registry limit");

        let mut with_db = push_input("Bridge3");
        with_db.database = Some("erp".to_string());
        let with_db = with_password(with_db, REF_A, "s");
        let fields: Vec<String> = only_fields(svc.create(with_db).await.unwrap_err());
        assert_eq!(fields, ["database", "password"]);

        // update_preserving_simulation still stores false for push.
        let updated = svc
            .update_preserving_simulation(created.id, push_input("Bridge1"))
            .await
            .unwrap();
        assert!(!updated.simulation);
    }

    fn only_fields(err: BantoError) -> Vec<String> {
        match err {
            BantoError::Validation { field_errors } => {
                field_errors.into_iter().map(|e| e.field).collect()
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    /// `settings` round-trips for push (through every read path), is refused
    /// on every other protocol, and its content goes through
    /// `validate_push_settings` with `settings.*` field paths.
    #[tokio::test]
    async fn push_settings_round_trip_and_are_refused_elsewhere() {
        let svc = service().await;
        let db = svc.create(postgres_input("ERP")).await.unwrap();
        let mut input = push_input("Bridge1");
        input.settings = Some(bridge_settings(db.id));
        let created = svc.create(input).await.expect("push with settings");
        assert_eq!(
            Value::Object(created.settings.clone()),
            bridge_settings(db.id)
        );
        assert_eq!(
            svc.get(created.id).await.unwrap().settings,
            created.settings
        );
        let listed = svc.list(ListParams::default()).await.unwrap();
        assert_eq!(
            listed
                .rows
                .iter()
                .find(|c| c.id == created.id)
                .unwrap()
                .settings,
            created.settings
        );

        // Non-push: refused, even an empty object.
        for protocol in ["modbus-tcp", "slmp", "hub"] {
            let mut input = sample_input(&format!("x-{protocol}"));
            input.protocol = protocol.to_string();
            input.settings = Some(json!({}));
            assert_eq!(
                only_fields(svc.create(input).await.unwrap_err()),
                ["settings"],
                "{protocol}"
            );
        }
        let mut pg = postgres_input("ERP2");
        pg.settings = Some(json!({}));
        assert_eq!(only_fields(svc.create(pg).await.unwrap_err()), ["settings"]);

        // Content errors keep their paths; several are reported together.
        let mut bad = push_input("Bad");
        bad.settings = Some(json!({ "stale_timeout_ms": 500, "extra": 1 }));
        let mut fields = only_fields(svc.create(bad).await.unwrap_err());
        fields.sort();
        assert_eq!(fields, ["settings", "settings.stale_timeout_ms"]);

        // The 64 KiB limit applies on the service path too.
        let mut huge = push_input("Huge");
        huge.settings = Some(json!({ "bridge": { "kind": "db", "db_connection_id": db.id,
            "queries": [ { "name": "q", "period_ms": 60000, "sql": "SELECT 1",
                           "columns": [], "pad": "x".repeat(70_000) } ] } }));
        assert_eq!(
            only_fields(svc.create(huge).await.unwrap_err()),
            ["settings"]
        );
    }

    /// `bridge.db_connection_id` must name an existing postgres connection -
    /// checked against the registry on create, create_tx, update, update_tx.
    #[tokio::test]
    async fn a_bridge_must_reference_a_postgres_connection() {
        let svc = service().await;
        let plc = svc.create(sample_input("PLC")).await.unwrap();
        let db = svc.create(postgres_input("ERP")).await.unwrap();
        let expect_ref_error = |err: BantoError| {
            assert_eq!(only_fields(err), ["settings.bridge.db_connection_id"]);
        };

        let mut to_plc = push_input("B1");
        to_plc.settings = Some(bridge_settings(plc.id));
        expect_ref_error(svc.create(to_plc.clone()).await.unwrap_err());
        let mut missing = push_input("B1");
        missing.settings = Some(bridge_settings(9_999));
        expect_ref_error(svc.create(missing).await.unwrap_err());
        let mut tx = svc.pool.begin().await.unwrap();
        expect_ref_error(svc.create_tx(&mut tx, to_plc.clone()).await.unwrap_err());
        drop(tx);

        let created = svc.create(push_input("B1")).await.unwrap();
        expect_ref_error(svc.update(created.id, to_plc.clone()).await.unwrap_err());
        let mut tx = svc.pool.begin().await.unwrap();
        expect_ref_error(
            svc.update_tx(&mut tx, created.id, to_plc)
                .await
                .unwrap_err(),
        );
        drop(tx);

        let mut ok = push_input("B1");
        ok.settings = Some(bridge_settings(db.id));
        assert_eq!(
            Value::Object(svc.update(created.id, ok).await.unwrap().settings),
            bridge_settings(db.id)
        );
    }

    /// R2a-2e (docs/banto-hub-ingest-design.md §3.2): a postgres connection
    /// a push bridge reads from cannot be deleted through any delete entry
    /// (`delete`, `delete_tx`, `cascade_delete_tx`); the message names the
    /// bridge. Once the bridge no longer names it (settings replaced), the
    /// delete goes through. An unrelated postgres connection is unaffected.
    #[tokio::test]
    async fn a_postgres_connection_a_bridge_reads_from_cannot_be_deleted() {
        let svc = service().await;
        let db = svc.create(postgres_input("ERP")).await.unwrap();
        let other = svc.create(postgres_input("ERP-other")).await.unwrap();
        let mut bridge = push_input("Bridge1");
        bridge.settings = Some(bridge_settings(db.id));
        let bridge = svc.create(bridge).await.unwrap();

        let expect_refused = |err: BantoError| {
            let BantoError::Validation { field_errors } = err else {
                panic!("expected a validation error, got {err:?}");
            };
            assert_eq!(field_errors.len(), 1);
            assert_eq!(field_errors[0].field, "id");
            assert!(
                field_errors[0].message.contains("Bridge1"),
                "{}",
                field_errors[0].message
            );
        };
        expect_refused(svc.delete(db.id).await.unwrap_err());
        let mut tx = svc.pool.begin().await.unwrap();
        expect_refused(svc.delete_tx(&mut tx, db.id).await.unwrap_err());
        expect_refused(svc.cascade_delete_tx(&mut tx, db.id).await.unwrap_err());
        drop(tx);
        expect_refused(svc.cascade_delete(db.id).await.unwrap_err());
        assert!(svc.get(db.id).await.is_ok(), "the row must survive");

        // Unreferenced postgres connections delete as before.
        svc.delete(other.id).await.unwrap();

        // Dropping the bridge from the push connection's settings frees it.
        let mut no_bridge = push_input("Bridge1");
        no_bridge.settings = Some(json!({ "stale_timeout_ms": 5000 }));
        svc.update(bridge.id, no_bridge).await.unwrap();
        svc.cascade_delete(db.id).await.unwrap();
        assert!(svc.get(db.id).await.is_err());
    }

    /// Update semantics (`PlcConnectionInput::settings`): omitted keeps the
    /// stored value (an unaware caller cannot wipe a bridge), an explicit
    /// object replaces it, and leaving "push" stores NULL.
    #[tokio::test]
    async fn push_settings_on_update_omitted_keeps_explicit_replaces_other_protocol_clears() {
        let svc = service().await;
        let mut input = push_input("Bridge1");
        input.settings = Some(json!({ "stale_timeout_ms": 2000 }));
        let created = svc.create(input).await.unwrap();

        let kept = svc
            .update(created.id, push_input("Bridge1 renamed"))
            .await
            .unwrap();
        assert_eq!(kept.name, "Bridge1 renamed");
        assert_eq!(
            Value::Object(kept.settings),
            json!({ "stale_timeout_ms": 2000 })
        );
        let mut tx = svc.pool.begin().await.unwrap();
        let kept_tx = svc
            .update_tx(&mut tx, created.id, push_input("Bridge1"))
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(
            Value::Object(kept_tx.settings),
            json!({ "stale_timeout_ms": 2000 })
        );

        let mut replace = push_input("Bridge1");
        replace.settings = Some(json!({}));
        assert!(svc
            .update(created.id, replace)
            .await
            .unwrap()
            .settings
            .is_empty());

        let cleared = svc
            .update(created.id, sample_input("Now modbus"))
            .await
            .unwrap();
        assert_eq!(cleared.protocol, "modbus-tcp");
        assert_eq!(
            Value::Object(cleared.settings),
            json!({"host": "192.168.1.10", "port": 502, "unit_id": 1, "word_order": "low_high"})
        );
    }

    /// The SQL side of `settings` (migration 0025): `json_valid` and "an object"
    /// refuse broken JSON and non-objects even when the service is bypassed, and the
    /// column is NOT NULL (an omitted column defaults to `{}`).
    #[tokio::test]
    async fn the_settings_column_refuses_invalid_json() {
        let svc = service().await;
        let insert = |name: &'static str, settings: Option<&'static str>| {
            sqlx::query(
                "INSERT INTO plc_connections (name, protocol, settings) VALUES (?, 'push', ?)",
            )
            .bind(name)
            .bind(settings)
            .execute(&svc.pool)
        };
        assert!(insert("a", Some("{not json")).await.is_err());
        assert!(insert("b", Some("[1]")).await.is_err());
        assert!(insert("c", Some("null")).await.is_err());
        assert!(insert("d", None).await.is_err(), "NOT NULL");
        insert("e", Some(r#"{"stale_timeout_ms":1000}"#))
            .await
            .unwrap();
        sqlx::query("INSERT INTO plc_connections (name, protocol) VALUES ('f', 'push')")
            .execute(&svc.pool)
            .await
            .unwrap();
        let defaulted: String =
            sqlx::query_scalar("SELECT settings FROM plc_connections WHERE name = 'f'")
                .fetch_one(&svc.pool)
                .await
                .unwrap();
        assert_eq!(defaulted, "{}");
    }

    #[test]
    fn bridge_db_connection_error_table() {
        assert!(bridge_db_connection_error(3, Some(POSTGRES_PROTOCOL)).is_none());
        for protocol in [None, Some("modbus-tcp"), Some("push"), Some("hub")] {
            let err = bridge_db_connection_error(3, protocol).expect("refused");
            assert_eq!(only_fields(err), ["settings.bridge.db_connection_id"]);
        }
    }

    /// `idx_*` indexes and every trigger, as `(type, name)` sorted.
    async fn schema_objects(conn: &mut SqliteConnection) -> Vec<(String, String)> {
        sqlx::query_as(
            "SELECT type, name FROM sqlite_master \
             WHERE (type = 'index' AND name LIKE 'idx_%') OR type = 'trigger' \
             ORDER BY type, name",
        )
        .fetch_all(conn)
        .await
        .unwrap()
    }

    /// Migrations 0023 (rebuild `plc_connections`: CHECK + `settings`) and
    /// 0024 (push-key triggers), applied the way sqlx applies a migration
    /// (whole file, one pinned connection, one transaction) to a database the
    /// real migrator brought up to 0022 and that has rows in every table.
    ///
    /// Checks (docs/banto-hub-ingest-design.md §11, R2a-2a): every column of
    /// every row survives - `cpu_type` included - and `settings` is NULL;
    /// `sqlite_sequence` is unchanged for every table; **no id is reused**
    /// (a non-max id and the max id were deleted before 0023, and the next
    /// connection's id is above the old max, §5.1); FKs hold in both
    /// directions; every index (0019's partial unique index included) and the
    /// 0020 triggers exist and still fire; and after 0024 its two triggers
    /// exist too.
    #[tokio::test]
    async fn migration_0023_and_0024_preserve_rows_sequence_triggers_and_indexes_on_a_populated_database(
    ) {
        use sqlx::{Acquire, Executor};

        let pool = banto_storage::connect_sqlite_memory()
            .await
            .expect("connect_sqlite_memory");
        let mut migrator = sqlx::migrate!("./migrations");
        migrator.dangerous_set_table_name(crate::MIGRATIONS_TABLE);
        migrator
            .run_to(22, &pool)
            .await
            .expect("migrate up to 0022");
        let mut conn = pool.acquire().await.expect("acquire one pinned connection");

        for sql in [
            // id 3 (not the max) and id 6 (the max) are deleted below.
            "INSERT INTO plc_connections (id, name, protocol, host, port, unit_id, enabled, \
             simulation, word_order, cpu_type) \
             VALUES (1, 'Line1', 'slmp', '192.168.1.10', 5007, 3, 0, 1, 'high_low', 'Q')",
            "INSERT INTO plc_connections (id, name, protocol, host, port, database, username, \
             password) VALUES (2, 'ERP', 'postgres', '10.0.0.50', 5432, 'erp', 'reader', 's')",
            "INSERT INTO plc_connections (id, name, protocol, host, port) \
             VALUES (3, 'GoneMid', 'modbus-tcp', '1.2.3.4', 502)",
            "INSERT INTO plc_connections (id, name, protocol, host, port, word_order) \
             VALUES (4, 'Hub', 'hub', '', 0, 'low_high')",
            "INSERT INTO plc_connections (id, name, protocol, host, port) \
             VALUES (5, 'M1', 'modbus-tcp', '1.2.3.5', 502)",
            "INSERT INTO plc_connections (id, name, protocol, host, port) \
             VALUES (6, 'GoneMax', 'modbus-tcp', '1.2.3.6', 502)",
            "DELETE FROM plc_connections WHERE id IN (3, 6)",
            "INSERT INTO collection_groups (id, name, plc_connection_id, period_ms, enabled, \
             default_writable) VALUES (4, 'G1', 1, 2000, 0, 0)",
            "INSERT INTO collection_groups (id, name, plc_connection_id, period_ms, enabled, \
             default_writable, query_sql) VALUES (5, 'Q1', 2, 5000, 1, 1, 'SELECT 1 AS v')",
            "INSERT INTO collection_groups (id, name, plc_connection_id, period_ms) \
             VALUES (6, 'H', 4, 1000)",
            "INSERT INTO collection_groups (id, name, plc_connection_id, period_ms) \
             VALUES (7, 'G1', 5, 1000)",
            "INSERT INTO tags (id, name, collection_group_id, address, data_type, string_length, \
             raw_lo, raw_hi, eng_lo, eng_hi, unit, decimals, enabled, writable, tag_kind, \
             expression, retain, revision, string_encoding) VALUES (11, 'T1', 4, 'D100', \
             'string', 8, 0, 100, 0, 50, 'degC', 2, 0, 1, 'plc', NULL, 1, 3, 'shift_jis')",
            "INSERT INTO tags (id, name, collection_group_id, address, data_type, tag_kind) \
             VALUES (12, 'V', 5, 'v', 'f64', 'db')",
            "INSERT INTO tags (id, name, collection_group_id, address, data_type) \
             VALUES (13, 't', 6, 'L1.fast.t', 'f64')",
            "INSERT INTO tags (id, name, collection_group_id, address, data_type) \
             VALUES (14, 'u', 7, '40001', 'u16')",
        ] {
            conn.execute(sql)
                .await
                .unwrap_or_else(|e| panic!("seed failed ({sql}): {e}"));
        }

        const CONN_ROW_SQL: &str = "SELECT quote(id) || '|' || quote(name) || '|' || \
             quote(protocol) || '|' || quote(host) || '|' || quote(port) || '|' || \
             quote(unit_id) || '|' || quote(enabled) || '|' || quote(simulation) || '|' || \
             quote(word_order) || '|' || quote(database) || '|' || quote(username) || '|' || \
             quote(password) || '|' || quote(cpu_type) FROM plc_connections ORDER BY id";
        const GROUP_ROW_SQL: &str = "SELECT quote(id) || '|' || quote(name) || '|' || \
             quote(plc_connection_id) || '|' || quote(period_ms) || '|' || quote(enabled) || \
             '|' || quote(default_writable) || '|' || quote(query_sql) \
             FROM collection_groups ORDER BY id";
        const TAG_ROW_SQL: &str = "SELECT quote(id) || '|' || quote(name) || '|' || \
             quote(collection_group_id) || '|' || quote(address) || '|' || quote(data_type) || \
             '|' || quote(string_length) || '|' || quote(raw_lo) || '|' || quote(raw_hi) || \
             '|' || quote(eng_lo) || '|' || quote(eng_hi) || '|' || quote(unit) || '|' || \
             quote(decimals) || '|' || quote(enabled) || '|' || quote(writable) || '|' || \
             quote(tag_kind) || '|' || quote(expression) || '|' || quote(retain) || '|' || \
             quote(revision) || '|' || quote(string_encoding) FROM tags ORDER BY id";
        const SEQ_SQL: &str = "SELECT name, seq FROM sqlite_sequence ORDER BY name";

        let rows = |sql: &'static str| sqlx::query_scalar::<_, String>(sql);
        let conns_before = rows(CONN_ROW_SQL).fetch_all(&mut *conn).await.unwrap();
        let groups_before = rows(GROUP_ROW_SQL).fetch_all(&mut *conn).await.unwrap();
        let tags_before = rows(TAG_ROW_SQL).fetch_all(&mut *conn).await.unwrap();
        let seq_before: Vec<(String, i64)> =
            sqlx::query_as(SEQ_SQL).fetch_all(&mut *conn).await.unwrap();
        assert_eq!(conns_before.len(), 4);
        assert!(conns_before[0].ends_with("|'Q'"), "{conns_before:?}");
        assert!(seq_before.contains(&("plc_connections".to_string(), 6)));

        let mut tx = conn.begin().await.expect("begin, as the migrator does");
        tx.execute(include_str!(
            "../migrations/0023_plc_connections_allow_push.sql"
        ))
        .await
        .expect("0023 should apply");
        tx.commit().await.expect("0023 should commit");

        // Every row, column for column (cpu_type 'Q' included).
        assert_eq!(
            rows(CONN_ROW_SQL).fetch_all(&mut *conn).await.unwrap(),
            conns_before
        );
        assert_eq!(
            rows(GROUP_ROW_SQL).fetch_all(&mut *conn).await.unwrap(),
            groups_before
        );
        assert_eq!(
            rows(TAG_ROW_SQL).fetch_all(&mut *conn).await.unwrap(),
            tags_before
        );
        let null_settings: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM plc_connections WHERE settings IS NULL")
                .fetch_one(&mut *conn)
                .await
                .unwrap();
        assert_eq!(null_settings, 4);
        let columns: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM pragma_table_info('plc_connections') ORDER BY cid",
        )
        .fetch_all(&mut *conn)
        .await
        .unwrap();
        assert_eq!(
            columns,
            [
                "id",
                "name",
                "protocol",
                "host",
                "port",
                "unit_id",
                "enabled",
                "simulation",
                "word_order",
                "database",
                "username",
                "password",
                "cpu_type",
                "settings"
            ]
        );
        let autoincrement: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'plc_connections' \
             AND sql LIKE '%id INTEGER PRIMARY KEY AUTOINCREMENT%'",
        )
        .fetch_one(&mut *conn)
        .await
        .unwrap();
        assert_eq!(autoincrement, 1);

        // Every AUTOINCREMENT high-water mark is exactly what it was.
        let seq_after: Vec<(String, i64)> =
            sqlx::query_as(SEQ_SQL).fetch_all(&mut *conn).await.unwrap();
        assert_eq!(seq_after, seq_before);

        let violations: Vec<(String,)> = sqlx::query_as("PRAGMA foreign_key_check")
            .fetch_all(&mut *conn)
            .await
            .unwrap();
        assert!(violations.is_empty(), "dangling FKs: {violations:?}");
        assert!(
            conn.execute(
                "INSERT INTO collection_groups (name, plc_connection_id, period_ms) \
                 VALUES ('orphan', 999, 1000)"
            )
            .await
            .is_err(),
            "group -> connection FK"
        );
        assert!(
            conn.execute("DELETE FROM plc_connections WHERE id = 1")
                .await
                .is_err(),
            "a connection with groups cannot be deleted (ON DELETE RESTRICT)"
        );

        let expected_0023 = vec![
            ("index", "idx_collection_groups_plc_connection_id"),
            ("index", "idx_plc_connections_single_hub"),
            ("index", "idx_tags_collection_group_id"),
            ("trigger", "tags_hub_external_name_unique_insert"),
            ("trigger", "tags_hub_external_name_unique_update"),
        ];
        let to_owned = |v: &[(&str, &str)]| -> Vec<(String, String)> {
            v.iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect()
        };
        assert_eq!(schema_objects(&mut conn).await, to_owned(&expected_0023));
        // 0022's composite UNIQUE (an automatic index) survives: same group
        // name under one connection is still refused.
        assert!(
            conn.execute(
                "INSERT INTO collection_groups (name, plc_connection_id, period_ms) \
                 VALUES ('G1', 1, 1000)"
            )
            .await
            .is_err(),
            "UNIQUE(plc_connection_id, name)"
        );
        // The hub partial unique index still holds, and the 0020 trigger still fires.
        assert!(
            conn.execute(
                "INSERT INTO plc_connections (name, protocol, host, port) VALUES ('Hub2', 'hub', '', 0)"
            )
            .await
            .is_err(),
            "a second hub row must be refused by the partial unique index"
        );
        let err = conn
            .execute(
                "INSERT INTO tags (name, collection_group_id, address, data_type) \
                 VALUES ('t2', 6, 'L1.fast.t', 'f64')",
            )
            .await
            .unwrap_err();
        assert!(
            err.as_database_error()
                .is_some_and(|e| e.message().contains("hub external name already registered")),
            "{err:?}"
        );

        // No id is reused: the next connection gets 7, above the deleted max (6).
        let next_id: i64 = sqlx::query_scalar(
            "INSERT INTO plc_connections (name, protocol, host, port) \
             VALUES ('Bridge1', 'push', '', 0) RETURNING id",
        )
        .fetch_one(&mut *conn)
        .await
        .expect("push should be accepted after 0023");
        assert_eq!(next_id, 7);
        assert!(
            conn.execute(
                "INSERT INTO plc_connections (name, protocol, host, port) \
                 VALUES ('Nope', 'opc-ua', '1.2.3.4', 4840)"
            )
            .await
            .is_err(),
            "an unknown protocol must still be refused"
        );

        // 0024: the push-key triggers, on top of the populated database.
        let mut tx = conn.begin().await.expect("begin, as the migrator does");
        tx.execute(include_str!("../migrations/0024_tags_push_key_unique.sql"))
            .await
            .expect("0024 should apply");
        tx.commit().await.expect("0024 should commit");
        let mut expected_0024 = expected_0023.clone();
        expected_0024.push(("trigger", "collection_groups_push_key_unique_move"));
        expected_0024.push(("trigger", "plc_connections_push_key_unique_protocol"));
        expected_0024.push(("trigger", "tags_push_key_unique_insert"));
        expected_0024.push(("trigger", "tags_push_key_unique_update"));
        expected_0024.sort();
        assert_eq!(schema_objects(&mut conn).await, to_owned(&expected_0024));
        assert_eq!(
            rows(TAG_ROW_SQL).fetch_all(&mut *conn).await.unwrap(),
            tags_before
        );
        assert_eq!(
            sqlx::query_as::<_, (String, i64)>(SEQ_SQL)
                .fetch_all(&mut *conn)
                .await
                .unwrap()
                .into_iter()
                .filter(|(name, _)| name != "plc_connections")
                .collect::<Vec<_>>(),
            seq_before
                .iter()
                .filter(|(name, _)| name != "plc_connections")
                .cloned()
                .collect::<Vec<_>>()
        );
        // The push triggers fire per connection.
        conn.execute(
            "INSERT INTO collection_groups (id, name, plc_connection_id, period_ms) \
             VALUES (8, 'P', 7, 1000)",
        )
        .await
        .unwrap();
        conn.execute(
            "INSERT INTO tags (name, collection_group_id, address, data_type) \
             VALUES ('k1', 8, 'line1.temp', 'f64')",
        )
        .await
        .unwrap();
        let err = conn
            .execute(
                "INSERT INTO tags (name, collection_group_id, address, data_type) \
                 VALUES ('k2', 8, 'line1.temp', 'f64')",
            )
            .await
            .unwrap_err();
        assert!(
            err.as_database_error()
                .is_some_and(|e| e.message().contains("push key already registered")),
            "{err:?}"
        );
        // The same text outside a push connection is not a push key.
        conn.execute(
            "INSERT INTO tags (name, collection_group_id, address, data_type) \
             VALUES ('k3', 7, 'line1.temp', 'f64')",
        )
        .await
        .expect("a modbus tag may use the same text");
    }

    // ---- PR #753 review P2: parent changes and bridge self-reference --------

    /// A connection created through the service, with one group holding a tag
    /// at each of `addresses` (inserted directly - the tag rules are not what
    /// these tests exercise). Returns `(connection id, group id)`.
    async fn connection_with_tags(
        svc: &PlcConnectionService,
        input: PlcConnectionInput,
        addresses: &[&str],
    ) -> (i64, i64) {
        let conn = svc.create(input).await.expect("create connection");
        let group: i64 = sqlx::query_scalar(
            "INSERT INTO collection_groups (name, plc_connection_id, period_ms) \
             VALUES (?, ?, 1000) RETURNING id",
        )
        .bind(format!("{}-g", conn.name))
        .bind(conn.id)
        .fetch_one(&svc.pool)
        .await
        .unwrap();
        for (i, address) in addresses.iter().enumerate() {
            sqlx::query(
                "INSERT INTO tags (name, collection_group_id, address, data_type) \
                 VALUES (?, ?, ?, 'f64')",
            )
            .bind(format!("t{i}"))
            .bind(group)
            .bind(*address)
            .execute(&svc.pool)
            .await
            .unwrap();
        }
        (conn.id, group)
    }

    fn assert_push_key_trigger(result: Result<sqlx::sqlite::SqliteQueryResult, sqlx::Error>) {
        let err = result.expect_err("the 0024 trigger should refuse");
        assert!(
            err.as_database_error()
                .is_some_and(|e| e.message().contains("push key already registered")),
            "{err:?}"
        );
    }

    /// DB side (migration 0024's parent-change triggers), bypassing the
    /// services: moving a group into a push connection where a key already
    /// exists, moving a group that itself holds a duplicate into push, and
    /// switching a connection with duplicate addresses to push are all
    /// refused; the same changes without a duplicate pass.
    #[tokio::test]
    async fn the_0024_parent_change_triggers_refuse_duplicate_push_keys() {
        let svc = service().await;
        let (a, _ga) = connection_with_tags(&svc, push_input("A"), &["temperature"]).await;
        let (_b, gb) = connection_with_tags(&svc, push_input("B"), &["temperature"]).await;
        let (_m, gm) = connection_with_tags(&svc, sample_input("M"), &["D100", "D100"]).await;
        let (_c, gc) = connection_with_tags(&svc, push_input("C"), &["pressure"]).await;
        let move_group = |group: i64, to: i64| {
            sqlx::query("UPDATE collection_groups SET plc_connection_id = ? WHERE id = ?")
                .bind(to)
                .bind(group)
                .execute(&svc.pool)
        };

        // The reviewer's repro: B's "temperature" moved under A.
        assert_push_key_trigger(move_group(gb, a).await);
        // A group carrying its own duplicate into push.
        assert_push_key_trigger(move_group(gm, a).await);
        // No duplicate: C's "pressure" may join A.
        move_group(gc, a).await.expect("distinct keys may move");
        // Rewriting the same connection id is not a move.
        move_group(gb, _b).await.expect("same connection");

        // A Modbus connection with duplicate addresses cannot become push...
        let (m2, _) = connection_with_tags(&svc, sample_input("M2"), &["40001", "40001"]).await;
        assert_push_key_trigger(
            sqlx::query("UPDATE plc_connections SET protocol = 'push' WHERE id = ?")
                .bind(m2)
                .execute(&svc.pool)
                .await,
        );
        // ...one with distinct addresses can (at the DB level).
        let (m3, _) = connection_with_tags(&svc, sample_input("M3"), &["40001", "40002"]).await;
        sqlx::query("UPDATE plc_connections SET protocol = 'push' WHERE id = ?")
            .bind(m3)
            .execute(&svc.pool)
            .await
            .expect("distinct addresses");
    }

    /// Service side: switching a connection that has tags into or out of
    /// push is refused on `protocol` (update and update_tx), so neither
    /// duplicate keys nor tags breaking `push_tag_errors` (writable, string,
    /// scaling) are carried across; a connection with only empty groups may
    /// switch.
    #[tokio::test]
    async fn switching_to_or_from_push_is_refused_while_tags_exist() {
        let svc = service().await;
        let (m, _) = connection_with_tags(&svc, sample_input("M"), &["40001", "40001"]).await;
        sqlx::query("UPDATE tags SET writable = 1")
            .execute(&svc.pool)
            .await
            .unwrap();
        assert_eq!(
            only_fields(svc.update(m, push_input("M")).await.unwrap_err()),
            ["protocol"]
        );
        let mut tx = svc.pool.begin().await.unwrap();
        assert_eq!(
            only_fields(
                svc.update_tx(&mut tx, m, push_input("M"))
                    .await
                    .unwrap_err()
            ),
            ["protocol"]
        );
        drop(tx);

        let (p, _) = connection_with_tags(&svc, push_input("P"), &["k"]).await;
        assert_eq!(
            only_fields(svc.update(p, sample_input("P")).await.unwrap_err()),
            ["protocol"]
        );
        let protocol: String =
            sqlx::query_scalar("SELECT protocol FROM plc_connections WHERE id = ?")
                .bind(m)
                .fetch_one(&svc.pool)
                .await
                .unwrap();
        assert_eq!(protocol, "modbus-tcp");

        // Only an empty group: nothing is carried across.
        let (e, _) = connection_with_tags(&svc, sample_input("E"), &[]).await;
        assert_eq!(
            svc.update(e, push_input("E")).await.unwrap().protocol,
            PUSH_PROTOCOL
        );
        assert_eq!(
            svc.update(e, sample_input("E")).await.unwrap().protocol,
            "modbus-tcp"
        );
    }

    #[test]
    fn push_protocol_switch_error_table() {
        let cases: &[(Option<&str>, &str, i64, bool)] = &[
            (Some("modbus-tcp"), "push", 1, true),
            (Some("push"), "slmp", 2, true),
            (Some("postgres"), "push", 1, true),
            (Some("modbus-tcp"), "push", 0, false),
            (Some("push"), "push", 5, false),
            (Some("modbus-tcp"), "slmp", 5, false),
            (None, "push", 5, false),
        ];
        for &(existing, new, tags, refused) in cases {
            assert_eq!(
                push_protocol_switch_error(existing, new, tags).is_some(),
                refused,
                "{existing:?} -> {new} with {tags} tags"
            );
        }
    }

    /// PR #753 review P2: a postgres connection X updated to push with a
    /// bridge pointing at X itself is refused (the check must judge X's
    /// post-update protocol, push, not the stored postgres) - on update and
    /// update_tx - and nothing is written.
    #[tokio::test]
    async fn a_postgres_to_push_update_cannot_reference_itself() {
        let svc = service().await;
        let x = svc.create(postgres_input("X")).await.unwrap();
        let mut input = push_input("X");
        input.settings = Some(bridge_settings(x.id));

        let err = svc.update(x.id, input.clone()).await.unwrap_err();
        assert_eq!(only_fields(err), ["settings.bridge.db_connection_id"]);
        let mut tx = svc.pool.begin().await.unwrap();
        let err = svc
            .update_tx(&mut tx, x.id, input.clone())
            .await
            .unwrap_err();
        assert_eq!(only_fields(err), ["settings.bridge.db_connection_id"]);
        drop(tx);
        assert_eq!(svc.get(x.id).await.unwrap(), x);

        // A push connection cannot point at itself either, and another
        // postgres connection is still fine.
        let p = svc.create(push_input("P")).await.unwrap();
        let mut own = push_input("P");
        own.settings = Some(bridge_settings(p.id));
        assert_eq!(
            only_fields(svc.update(p.id, own).await.unwrap_err()),
            ["settings.bridge.db_connection_id"]
        );
        let y = svc.create(postgres_input("Y")).await.unwrap();
        input.settings = Some(bridge_settings(y.id));
        assert_eq!(
            Value::Object(svc.update(x.id, input).await.unwrap().settings),
            bridge_settings(y.id)
        );
    }

    // ---- R2b-1d: migration 0025 (docs/banto-hub-connection-settings-design.md §6.2・§9) ----

    /// Every trigger and `idx_*` index, with its full SQL text, sorted.
    async fn trigger_and_index_sql(conn: &mut SqliteConnection) -> Vec<(String, String, String)> {
        sqlx::query_as(
            "SELECT type, name, sql FROM sqlite_master \
             WHERE type = 'trigger' OR (type = 'index' AND name LIKE 'idx_%') \
             ORDER BY type, name",
        )
        .fetch_all(conn)
        .await
        .unwrap()
    }

    /// Migration 0025, applied the way sqlx applies a migration (whole file, one pinned
    /// connection, one transaction) to a database the real migrator brought up to 0024 and
    /// that holds every protocol variant of design §9: `modbus-tcp` (`low_high` /
    /// `high_low`, `unit_id` 0 and 255, a stray `cpu_type = 'Q'`), `slmp` (`Q` / `R`, a
    /// non-default `unit_id`), `postgres` (password set / empty / NULL), `virtual`
    /// (`calc` / `mem`), `push` (no settings / `stale_timeout_ms` only / a bridge) and
    /// `hub`; a non-max and the max id deleted beforehand.
    ///
    /// Checks: each row's `settings` is exactly design §3.2's mapping (dropped columns
    /// gone), the flat projection reads back the old columns except the dropped ones,
    /// `settings_version = 1`; the one non-empty password became a `plain-v1` secret row
    /// whose reference is in `settings` (and nowhere else holds the plaintext);
    /// `sqlite_sequence` is unchanged and a new connection does not reuse a deleted id;
    /// the 6 triggers and the index have byte-identical SQL and still fire; groups and
    /// tags are untouched; FKs and integrity hold; the new `protocol` CHECK is the
    /// DriverId grammar.
    #[tokio::test]
    async fn migration_0025_moves_columns_to_settings_and_secrets_on_a_populated_database() {
        use sqlx::{Acquire, Executor};

        let pool = banto_storage::connect_sqlite_memory()
            .await
            .expect("connect_sqlite_memory");
        let mut migrator = sqlx::migrate!("./migrations");
        migrator.dangerous_set_table_name(crate::MIGRATIONS_TABLE);
        migrator
            .run_to(24, &pool)
            .await
            .expect("migrate up to 0024");
        let mut conn = pool.acquire().await.expect("acquire one pinned connection");

        const BRIDGE: &str = r#"{"stale_timeout_ms":5000,"bridge":{"kind":"db","db_connection_id":5,"queries":[{"name":"q","period_ms":1000,"sql":"SELECT 1 AS v","columns":[{"column":"v","key":"k"}]}]}}"#;
        let seed = [
            // (id, name, protocol, host, port, unit_id, enabled, simulation, word_order,
            //  database, username, password, cpu_type, settings)
            "INSERT INTO plc_connections VALUES (1, 'M-lh', 'modbus-tcp', '10.0.0.1', 502, 0, \
             1, 1, 'low_high', NULL, NULL, NULL, 'Q', NULL)"
                .to_string(),
            "INSERT INTO plc_connections VALUES (2, 'M-hl', 'modbus-tcp', '10.0.0.2', 1502, 255, \
             0, 0, 'high_low', NULL, NULL, NULL, 'R', NULL)"
                .to_string(),
            "INSERT INTO plc_connections VALUES (3, 'S-Q', 'slmp', '10.0.0.3', 5007, 3, \
             1, 0, 'low_high', NULL, NULL, NULL, 'Q', NULL)"
                .to_string(),
            "INSERT INTO plc_connections VALUES (4, 'S-R', 'slmp', '10.0.0.4', 5008, 1, \
             0, 1, 'high_low', NULL, NULL, NULL, 'R', NULL)"
                .to_string(),
            "INSERT INTO plc_connections VALUES (5, 'PG-pw', 'postgres', '10.0.0.50', 5432, 1, \
             1, 0, 'low_high', 'erp', 'reader', 's3cret-pw', 'R', NULL)"
                .to_string(),
            "INSERT INTO plc_connections VALUES (6, 'PG-empty', 'postgres', '10.0.0.51', 5433, 1, \
             1, 0, 'low_high', 'erp2', 'reader2', '', 'R', NULL)"
                .to_string(),
            "INSERT INTO plc_connections VALUES (7, 'PG-null', 'postgres', '10.0.0.52', 5434, 1, \
             1, 0, 'low_high', 'erp3', 'reader3', NULL, 'R', NULL)"
                .to_string(),
            "INSERT INTO plc_connections VALUES (8, 'calc', 'virtual', '', 0, 1, \
             1, 0, 'low_high', NULL, NULL, NULL, 'R', NULL)"
                .to_string(),
            "INSERT INTO plc_connections VALUES (9, 'mem', 'virtual', 'stray-host', 7, 1, \
             1, 0, 'low_high', NULL, NULL, NULL, 'R', NULL)"
                .to_string(),
            "INSERT INTO plc_connections VALUES (10, 'P-none', 'push', '', 0, 1, \
             1, 0, 'low_high', NULL, NULL, NULL, 'R', NULL)"
                .to_string(),
            "INSERT INTO plc_connections VALUES (11, 'P-stale', 'push', '', 0, 1, \
             1, 0, 'low_high', NULL, NULL, NULL, 'R', '{\"stale_timeout_ms\":5000}')"
                .to_string(),
            format!(
                "INSERT INTO plc_connections VALUES (12, 'P-bridge', 'push', '', 0, 1, \
                 1, 0, 'low_high', NULL, NULL, NULL, 'R', '{BRIDGE}')"
            ),
            "INSERT INTO plc_connections VALUES (13, 'Hub', 'hub', '', 0, 1, \
             1, 0, 'low_high', NULL, NULL, NULL, 'R', NULL)"
                .to_string(),
            "INSERT INTO plc_connections VALUES (14, 'GoneMid', 'modbus-tcp', '1.2.3.4', 502, 1, \
             1, 0, 'high_low', NULL, NULL, NULL, 'R', NULL)"
                .to_string(),
            "INSERT INTO plc_connections VALUES (15, 'GoneMax', 'modbus-tcp', '1.2.3.5', 502, 1, \
             1, 0, 'high_low', NULL, NULL, NULL, 'R', NULL)"
                .to_string(),
            "DELETE FROM plc_connections WHERE id IN (14, 15)".to_string(),
            "INSERT INTO collection_groups (id, name, plc_connection_id, period_ms, enabled, \
             default_writable) VALUES (4, 'G1', 3, 2000, 0, 0)"
                .to_string(),
            "INSERT INTO collection_groups (id, name, plc_connection_id, period_ms, enabled, \
             default_writable, query_sql) VALUES (5, 'Q1', 5, 5000, 1, 1, 'SELECT 1 AS v')"
                .to_string(),
            "INSERT INTO collection_groups (id, name, plc_connection_id, period_ms) \
             VALUES (6, 'H', 13, 1000)"
                .to_string(),
            "INSERT INTO collection_groups (id, name, plc_connection_id, period_ms) \
             VALUES (7, 'P', 11, 1000)"
                .to_string(),
            "INSERT INTO tags (id, name, collection_group_id, address, data_type, string_length, \
             raw_lo, raw_hi, eng_lo, eng_hi, unit, decimals, enabled, writable, tag_kind, \
             expression, retain, revision, string_encoding) VALUES (11, 'T1', 4, 'D100', \
             'string', 8, 0, 100, 0, 50, 'degC', 2, 0, 1, 'plc', NULL, 1, 3, 'shift_jis')"
                .to_string(),
            "INSERT INTO tags (id, name, collection_group_id, address, data_type, tag_kind) \
             VALUES (12, 'V', 5, 'v', 'f64', 'db')"
                .to_string(),
            "INSERT INTO tags (id, name, collection_group_id, address, data_type) \
             VALUES (13, 't', 6, 'L1.fast.t', 'f64')"
                .to_string(),
            "INSERT INTO tags (id, name, collection_group_id, address, data_type) \
             VALUES (14, 'k', 7, 'line1.temp', 'f64')"
                .to_string(),
        ];
        for sql in &seed {
            sqlx::raw_sql(sqlx::AssertSqlSafe(sql.clone()))
                .execute(&mut *conn)
                .await
                .unwrap_or_else(|e| panic!("seed failed ({sql}): {e}"));
        }

        const GROUP_ROW_SQL: &str = "SELECT quote(id) || '|' || quote(name) || '|' || \
             quote(plc_connection_id) || '|' || quote(period_ms) || '|' || quote(enabled) || \
             '|' || quote(default_writable) || '|' || quote(query_sql) \
             FROM collection_groups ORDER BY id";
        const TAG_ROW_SQL: &str = "SELECT quote(id) || '|' || quote(name) || '|' || \
             quote(collection_group_id) || '|' || quote(address) || '|' || quote(data_type) || \
             '|' || quote(string_length) || '|' || quote(raw_lo) || '|' || quote(raw_hi) || \
             '|' || quote(eng_lo) || '|' || quote(eng_hi) || '|' || quote(unit) || '|' || \
             quote(decimals) || '|' || quote(enabled) || '|' || quote(writable) || '|' || \
             quote(tag_kind) || '|' || quote(expression) || '|' || quote(retain) || '|' || \
             quote(revision) || '|' || quote(string_encoding) FROM tags ORDER BY id";
        const SEQ_SQL: &str = "SELECT name, seq FROM sqlite_sequence ORDER BY name";

        let rows = |sql: &'static str| sqlx::query_scalar::<_, String>(sql);
        let groups_before = rows(GROUP_ROW_SQL).fetch_all(&mut *conn).await.unwrap();
        let tags_before = rows(TAG_ROW_SQL).fetch_all(&mut *conn).await.unwrap();
        let seq_before: Vec<(String, i64)> =
            sqlx::query_as(SEQ_SQL).fetch_all(&mut *conn).await.unwrap();
        assert!(seq_before.contains(&("plc_connections".to_string(), 15)));
        let schema_before = trigger_and_index_sql(&mut conn).await;
        assert_eq!(
            schema_before
                .iter()
                .filter(|(kind, _, _)| kind == "trigger")
                .count(),
            6,
            "{schema_before:?}"
        );

        let mut tx = conn.begin().await.expect("begin, as the migrator does");
        tx.execute(include_str!(
            "../migrations/0025_plc_connections_settings_json.sql"
        ))
        .await
        .expect("0025 should apply");
        tx.commit().await.expect("0025 should commit");

        // The new shape (design §3.1).
        let columns: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM pragma_table_info('plc_connections') ORDER BY cid",
        )
        .fetch_all(&mut *conn)
        .await
        .unwrap();
        assert_eq!(
            columns,
            [
                "id",
                "name",
                "protocol",
                "enabled",
                "simulation",
                "settings",
                "settings_version"
            ]
        );
        let autoincrement: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'plc_connections' \
             AND sql LIKE '%id INTEGER PRIMARY KEY AUTOINCREMENT%'",
        )
        .fetch_one(&mut *conn)
        .await
        .unwrap();
        assert_eq!(autoincrement, 1);

        // Every row's settings, exactly design §3.2.
        let stored: Vec<(i64, String, String, bool, bool, String, i64)> = sqlx::query_as(
            "SELECT id, name, protocol, enabled, simulation, settings, settings_version \
             FROM plc_connections ORDER BY id",
        )
        .fetch_all(&mut *conn)
        .await
        .unwrap();
        let secret: (String, i64, String, String, Vec<u8>) = sqlx::query_as(
            "SELECT ref, connection_id, key, scheme, blob FROM plc_connection_secrets",
        )
        .fetch_one(&mut *conn)
        .await
        .expect("exactly one secret row (the non-empty password)");
        let secret_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM plc_connection_secrets")
            .fetch_one(&mut *conn)
            .await
            .unwrap();
        assert_eq!(secret_count, 1);
        let (secret_ref, secret_conn, secret_key, scheme, blob) = secret;
        assert!(
            crate::connection_secret::is_valid_secret_ref(&secret_ref),
            "{secret_ref}"
        );
        assert_eq!(
            (secret_conn, secret_key.as_str(), scheme.as_str()),
            (5, "password", "plain-v1")
        );
        assert_eq!(blob, b"s3cret-pw");

        let bridge: Value = serde_json::from_str(BRIDGE).unwrap();
        let expected: Vec<(i64, &str, &str, bool, bool, Value)> = vec![
            (
                1,
                "M-lh",
                "modbus-tcp",
                true,
                true,
                json!({"host": "10.0.0.1", "port": 502, "unit_id": 0, "word_order": "low_high"}),
            ),
            (
                2,
                "M-hl",
                "modbus-tcp",
                false,
                false,
                json!({"host": "10.0.0.2", "port": 1502, "unit_id": 255, "word_order": "high_low"}),
            ),
            (
                3,
                "S-Q",
                "slmp",
                true,
                false,
                json!({"host": "10.0.0.3", "port": 5007, "word_order": "low_high", "cpu_type": "Q"}),
            ),
            (
                4,
                "S-R",
                "slmp",
                false,
                true,
                json!({"host": "10.0.0.4", "port": 5008, "word_order": "high_low", "cpu_type": "R"}),
            ),
            (
                5,
                "PG-pw",
                "postgres",
                true,
                false,
                json!({"host": "10.0.0.50", "port": 5432, "database": "erp", "username": "reader",
                       "password": {"$secret": secret_ref}}),
            ),
            (
                6,
                "PG-empty",
                "postgres",
                true,
                false,
                json!({"host": "10.0.0.51", "port": 5433, "database": "erp2", "username": "reader2"}),
            ),
            (
                7,
                "PG-null",
                "postgres",
                true,
                false,
                json!({"host": "10.0.0.52", "port": 5434, "database": "erp3", "username": "reader3"}),
            ),
            (8, "calc", "virtual", true, false, json!({})),
            (9, "mem", "virtual", true, false, json!({})),
            (10, "P-none", "push", true, false, json!({})),
            (
                11,
                "P-stale",
                "push",
                true,
                false,
                json!({"stale_timeout_ms": 5000}),
            ),
            (12, "P-bridge", "push", true, false, bridge),
            (13, "Hub", "hub", true, false, json!({})),
        ];
        assert_eq!(stored.len(), expected.len());
        for (row, want) in stored.iter().zip(&expected) {
            let (id, name, protocol, enabled, simulation, settings, version) = row;
            let settings: Value = serde_json::from_str(settings).unwrap();
            assert_eq!(
                (*id, name.as_str(), protocol.as_str(), *enabled, *simulation),
                (want.0, want.1, want.2, want.3, want.4),
                "row {id}"
            );
            assert_eq!(settings, want.5, "row {id} ({name})");
            assert_eq!(*version, 1, "row {id}");
        }
        // The plaintext survives only in the secret row's blob.
        let plaintext_in_settings: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM plc_connections WHERE instr(settings, 's3cret-pw') > 0",
        )
        .fetch_one(&mut *conn)
        .await
        .unwrap();
        assert_eq!(plaintext_in_settings, 0);

        // The flat projection reads the old columns back, except design §3.2's dropped
        // ones (S-Q's unit_id 3 → 1, M-lh's cpu_type Q → R, mem's host/port → ""/0).
        let svc = PlcConnectionService::new(pool.clone());
        let listed = svc.list(ListParams::default()).await.unwrap().rows;
        type Flat<'a> = (
            i64,
            &'a str,
            i64,
            i64,
            &'a str,
            &'a str,
            Option<&'a str>,
            Option<&'a str>,
        );
        let flat: Vec<Flat<'_>> = listed
            .iter()
            .map(|c| {
                (
                    c.id,
                    c.host.as_str(),
                    c.port,
                    c.unit_id,
                    c.word_order.as_str(),
                    c.cpu_type.as_str(),
                    c.database.as_deref(),
                    c.username.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            flat,
            vec![
                (1, "10.0.0.1", 502, 0, "low_high", "R", None, None),
                (2, "10.0.0.2", 1502, 255, "high_low", "R", None, None),
                (3, "10.0.0.3", 5007, 1, "low_high", "Q", None, None),
                (4, "10.0.0.4", 5008, 1, "high_low", "R", None, None),
                (
                    5,
                    "10.0.0.50",
                    5432,
                    1,
                    "low_high",
                    "R",
                    Some("erp"),
                    Some("reader")
                ),
                (
                    6,
                    "10.0.0.51",
                    5433,
                    1,
                    "low_high",
                    "R",
                    Some("erp2"),
                    Some("reader2")
                ),
                (
                    7,
                    "10.0.0.52",
                    5434,
                    1,
                    "low_high",
                    "R",
                    Some("erp3"),
                    Some("reader3")
                ),
                (8, "", 0, 1, "low_high", "R", None, None),
                (9, "", 0, 1, "low_high", "R", None, None),
                (10, "", 0, 1, "low_high", "R", None, None),
                (11, "", 0, 1, "low_high", "R", None, None),
                (12, "", 0, 1, "low_high", "R", None, None),
                (13, "", 0, 1, "low_high", "R", None, None),
            ]
        );
        assert_eq!(
            listed[4].secret_ref(PASSWORD_KEY),
            Some(secret_ref.as_str())
        );
        assert_eq!(
            svc.secret_rows(5).await.unwrap()[0].sealed.blob.as_slice(),
            b"s3cret-pw"
        );

        // Children untouched; every AUTOINCREMENT high-water mark unchanged.
        assert_eq!(
            rows(GROUP_ROW_SQL).fetch_all(&mut *conn).await.unwrap(),
            groups_before
        );
        assert_eq!(
            rows(TAG_ROW_SQL).fetch_all(&mut *conn).await.unwrap(),
            tags_before
        );
        let seq_after: Vec<(String, i64)> =
            sqlx::query_as(SEQ_SQL).fetch_all(&mut *conn).await.unwrap();
        assert_eq!(seq_after, seq_before);

        // The 6 triggers and the index: byte-identical SQL.
        assert_eq!(trigger_and_index_sql(&mut conn).await, schema_before);

        let violations: Vec<(String,)> = sqlx::query_as("PRAGMA foreign_key_check")
            .fetch_all(&mut *conn)
            .await
            .unwrap();
        assert!(violations.is_empty(), "dangling FKs: {violations:?}");
        let integrity: String = sqlx::query_scalar("PRAGMA integrity_check")
            .fetch_one(&mut *conn)
            .await
            .unwrap();
        assert_eq!(integrity, "ok");
        assert!(
            conn.execute("DELETE FROM plc_connections WHERE id = 3")
                .await
                .is_err(),
            "a connection with groups cannot be deleted (ON DELETE RESTRICT)"
        );
        assert!(
            conn.execute(
                "INSERT INTO collection_groups (name, plc_connection_id, period_ms) \
                 VALUES ('orphan', 999, 1000)"
            )
            .await
            .is_err(),
            "group -> connection FK"
        );

        // The triggers still fire: hub external name, push key, protocol switch.
        let expect_trigger = |result: Result<sqlx::sqlite::SqliteQueryResult, sqlx::Error>,
                              message: &str| {
            let err = result.expect_err("the trigger should refuse");
            assert!(
                err.as_database_error()
                    .is_some_and(|e| e.message().contains(message)),
                "{err:?}"
            );
        };
        expect_trigger(
            conn.execute(
                "INSERT INTO tags (name, collection_group_id, address, data_type) \
                 VALUES ('t2', 6, 'L1.fast.t', 'f64')",
            )
            .await,
            "hub external name already registered",
        );
        expect_trigger(
            conn.execute(
                "INSERT INTO tags (name, collection_group_id, address, data_type) \
                 VALUES ('k2', 7, 'line1.temp', 'f64')",
            )
            .await,
            "push key already registered",
        );
        conn.execute(
            "INSERT INTO tags (name, collection_group_id, address, data_type) \
             VALUES ('d2', 4, 'D100', 'u16')",
        )
        .await
        .expect("a duplicate address under slmp is not a push key");
        expect_trigger(
            conn.execute("UPDATE plc_connections SET protocol = 'push' WHERE id = 3")
                .await,
            "push key already registered",
        );
        // The hub partial unique index still holds.
        assert!(conn
            .execute("INSERT INTO plc_connections (name, protocol) VALUES ('Hub2', 'hub')")
            .await
            .is_err());

        // No id is reused: the next connection gets 16, above the deleted max (15).
        let next_id: i64 = sqlx::query_scalar(
            "INSERT INTO plc_connections (name, protocol) VALUES ('New', 'push') RETURNING id",
        )
        .fetch_one(&mut *conn)
        .await
        .unwrap();
        assert_eq!(next_id, 16);

        // The protocol CHECK is the DriverId grammar (Q14), not a list.
        conn.execute("INSERT INTO plc_connections (name, protocol) VALUES ('Opc', 'opc-ua')")
            .await
            .expect("a well-formed driver id is the registry's business, not SQL's");
        for bad in ["Opc-UA", "-x", "", "a b", "x_y", &"a".repeat(65)] {
            assert!(
                sqlx::query("INSERT INTO plc_connections (name, protocol) VALUES (?, ?)")
                    .bind(format!("bad-{bad}"))
                    .bind(bad)
                    .execute(&mut *conn)
                    .await
                    .is_err(),
                "{bad:?}"
            );
        }
        sqlx::query("INSERT INTO plc_connections (name, protocol) VALUES ('Max', ?)")
            .bind("a".repeat(64))
            .execute(&mut *conn)
            .await
            .expect("64 characters is the limit");
        // The secret table's CHECKs.
        for (r#ref, scheme) in [
            ("ABCDEF0123456789abcdef0123456789", "plain-v1"),
            ("abc", "plain-v1"),
            ("0123456789abcdef0123456789abcdef", "rot13"),
        ] {
            assert!(
                sqlx::query(
                    "INSERT INTO plc_connection_secrets (ref, connection_id, key, scheme, blob) \
                     VALUES (?, 1, 'k', ?, x'00')"
                )
                .bind(r#ref)
                .bind(scheme)
                .execute(&mut *conn)
                .await
                .is_err(),
                "{ref} {scheme}"
            );
        }
    }

    /// Design §1.1 要確認 / §9 「戻せないこと」: a database migrated through 0025 cannot be
    /// opened by a migrator that knows only 0001-0024 (an older Hub/ChronoGazer) - sqlx
    /// refuses an applied version missing from its source (`ignore_missing` is `false`),
    /// so an old build fails at startup instead of reading a table it does not understand.
    #[tokio::test]
    async fn a_database_migrated_through_0025_is_refused_by_a_0024_migrator() {
        let pool = banto_storage::connect_sqlite_memory()
            .await
            .expect("connect_sqlite_memory");
        migrate(&pool).await.expect("migrate through 0025");

        let mut old = sqlx::migrate!("./migrations");
        old.dangerous_set_table_name(crate::MIGRATIONS_TABLE);
        old.migrations = std::borrow::Cow::Owned(
            old.migrations
                .iter()
                .filter(|m| m.version <= 24)
                .cloned()
                .collect(),
        );
        assert_eq!(old.migrations.len(), 24);
        assert!(!old.ignore_missing);
        match old.run(&pool).await {
            Err(sqlx::migrate::MigrateError::VersionMissing(25)) => {}
            other => panic!("expected VersionMissing(25), got {other:?}"),
        }
    }

    // ---- R2b-1d: the secret lifecycle (design §5.3) and the storage-layer checks ----

    /// Protocol change (postgres → modbus-tcp) removes the reference and the row in the
    /// update's transaction; changing back with `Keep` does not resurrect anything.
    #[tokio::test]
    async fn a_protocol_change_removes_the_secret_reference_and_row() {
        let svc = service().await;
        let pg = svc
            .create(with_password(postgres_input("PG"), REF_A, "pw-a"))
            .await
            .unwrap();
        let other = svc
            .create(with_password(postgres_input("PG2"), REF_B, "pw-b"))
            .await
            .unwrap();
        let switched = svc.update(pg.id, sample_input("PG")).await.unwrap();
        assert_eq!(switched.protocol, "modbus-tcp");
        assert_eq!(switched.secret_ref(PASSWORD_KEY), None);
        assert!(svc.secret_rows(pg.id).await.unwrap().is_empty());
        // The other connection's secret is untouched.
        assert_eq!(
            all_secret_rows(&svc).await,
            [(REF_B.to_string(), other.id, PASSWORD_KEY.to_string())]
        );
        let back = svc.update(pg.id, postgres_input("PG")).await.unwrap();
        assert_eq!(stored_password(&svc, back.id).await, None);

        // Same through update_tx and update_preserving_simulation.
        let mut tx = svc.pool.begin().await.unwrap();
        svc.update_tx(&mut tx, other.id, sample_input("PG2"))
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert!(all_secret_rows(&svc).await.is_empty());
        let c = svc
            .create(with_password(postgres_input("PG3"), REF_C, "pw-c"))
            .await
            .unwrap();
        svc.update_preserving_simulation(c.id, sample_input("PG3"))
            .await
            .unwrap();
        assert!(all_secret_rows(&svc).await.is_empty());
    }

    /// Delete, delete_tx, cascade_delete and cascade_delete_tx all remove the
    /// connection's secret rows with it - and only its own.
    #[tokio::test]
    async fn every_delete_entry_removes_the_connections_secret_rows() {
        let svc = service().await;
        let keep = svc
            .create(with_password(postgres_input("Keep"), REF_A, "keep"))
            .await
            .unwrap();
        let refs = [
            "11111111111111111111111111111111",
            "22222222222222222222222222222222",
            "33333333333333333333333333333333",
            "44444444444444444444444444444444",
        ];
        let mut ids = Vec::new();
        for (i, r#ref) in refs.iter().enumerate() {
            let created = svc
                .create(with_password(postgres_input(&format!("D{i}")), r#ref, "x"))
                .await
                .unwrap();
            ids.push(created.id);
        }
        // A group (and a db tag) under the cascade targets.
        for id in [ids[2], ids[3]] {
            sqlx::query(
                "INSERT INTO collection_groups (name, plc_connection_id, period_ms, query_sql) \
                 VALUES ('g', ?, 1000, 'SELECT 1 AS v')",
            )
            .bind(id)
            .execute(&svc.pool)
            .await
            .unwrap();
        }
        svc.delete(ids[0]).await.unwrap();
        let mut tx = svc.pool.begin().await.unwrap();
        svc.delete_tx(&mut tx, ids[1]).await.unwrap();
        tx.commit().await.unwrap();
        svc.cascade_delete(ids[2]).await.unwrap();
        let mut tx = svc.pool.begin().await.unwrap();
        svc.cascade_delete_tx(&mut tx, ids[3]).await.unwrap();
        tx.commit().await.unwrap();
        assert_eq!(
            all_secret_rows(&svc).await,
            [(REF_A.to_string(), keep.id, PASSWORD_KEY.to_string())]
        );

        // A rolled-back delete leaves the row and its secret.
        let mut tx = svc.pool.begin().await.unwrap();
        svc.delete_tx(&mut tx, keep.id).await.unwrap();
        drop(tx);
        assert_eq!(
            stored_password(&svc, keep.id).await.as_deref(),
            Some("keep")
        );
    }

    /// A failed write leaves nothing: a secret insert that fails (its reference is
    /// already used by another connection) rolls back the connection row on create
    /// and keeps the old password on update; a caller's rolled-back transaction leaves
    /// neither the row nor the secret.
    #[tokio::test]
    async fn a_failed_write_leaves_neither_the_row_nor_the_secret() {
        let svc = service().await;
        let first = svc
            .create(with_password(postgres_input("First"), REF_A, "first"))
            .await
            .unwrap();

        // Create: the secret insert collides -> no connection row either.
        let err = svc
            .create(with_password(postgres_input("Dup"), REF_A, "dup"))
            .await
            .unwrap_err();
        assert!(matches!(err, BantoError::Storage(_)), "{err:?}");
        let names: Vec<String> = sqlx::query_scalar("SELECT name FROM plc_connections")
            .fetch_all(&svc.pool)
            .await
            .unwrap();
        assert_eq!(names, ["First"]);

        // Update: the collision keeps the old reference and row.
        let second = svc
            .create(with_password(postgres_input("Second"), REF_B, "second"))
            .await
            .unwrap();
        svc.update(
            second.id,
            with_password(postgres_input("Second renamed"), REF_A, "new"),
        )
        .await
        .unwrap_err();
        let unchanged = svc.get(second.id).await.unwrap();
        assert_eq!(unchanged.name, "Second");
        assert_eq!(
            stored_password(&svc, second.id).await.as_deref(),
            Some("second")
        );
        assert_eq!(
            stored_password(&svc, first.id).await.as_deref(),
            Some("first")
        );

        // The caller's transaction rolls back: nothing of create_tx / update_tx stays.
        let mut tx = svc.pool.begin().await.unwrap();
        let created = svc
            .create_tx(
                &mut tx,
                with_password(postgres_input("Gone"), REF_C, "gone"),
            )
            .await
            .unwrap();
        svc.update_tx(
            &mut tx,
            first.id,
            with_secret_change(postgres_input("First"), SecretChange::Clear),
        )
        .await
        .unwrap();
        drop(tx);
        assert!(svc.get(created.id).await.is_err());
        assert_eq!(
            all_secret_rows(&svc).await,
            [
                (REF_A.to_string(), first.id, PASSWORD_KEY.to_string()),
                (REF_B.to_string(), second.id, PASSWORD_KEY.to_string()),
            ]
        );
    }

    /// Design §3.3・§8-1: the storage layer refuses a plaintext value under a secret
    /// key whatever the entrance. The flat path never puts one there, so this drives the
    /// last line directly with a registry whose `push` driver declares `stale_timeout_ms`
    /// a secret key: the old validation (1) and the driver (4) both pass the plain number,
    /// and only the storage check refuses it - on create and update, writing nothing.
    #[tokio::test]
    async fn a_plaintext_value_under_a_secret_key_is_refused_at_storage() {
        let svc = registry_service(&[("push", &["stale_timeout_ms"], 1)]).await;
        let mut input = push_input("P");
        input.settings = Some(json!({"stale_timeout_ms": 5000}));
        let err = svc.create(input.clone()).await.unwrap_err();
        assert_eq!(only_fields(err), ["stale_timeout_ms"]);
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM plc_connections")
            .fetch_one(&svc.pool)
            .await
            .unwrap();
        assert_eq!(count, 0);

        let created = svc.create(push_input("P")).await.unwrap();
        let err = svc.update(created.id, input).await.unwrap_err();
        assert_eq!(only_fields(err), ["stale_timeout_ms"]);
        assert!(svc.get(created.id).await.unwrap().settings.is_empty());
    }

    /// A test driver for [`registry_service`]: accepts everything, declares the given
    /// secret keys and settings version; refuses `port = 1` so a registry error is
    /// observable.
    struct TestDriver {
        descriptor: banto_driver_api::DriverDescriptor,
        secret_keys: Vec<String>,
    }

    impl banto_driver_api::DriverBase for TestDriver {
        fn descriptor(&self) -> &banto_driver_api::DriverDescriptor {
            &self.descriptor
        }
        fn validate_config(
            &self,
            settings: &banto_driver_api::Settings,
        ) -> Vec<banto_driver_api::FieldError> {
            if settings.get("port") == Some("1") {
                vec![banto_driver_api::FieldError::new("port", "driver says no")]
            } else {
                Vec::new()
            }
        }
        fn secret_keys(&self) -> Vec<String> {
            self.secret_keys.clone()
        }
        fn parse_address(
            &self,
            _text: &str,
            _kind: &banto_driver_api::TagKind,
        ) -> Result<banto_driver_api::DriverAddress, banto_driver_api::AddressError> {
            Err(banto_driver_api::AddressError::new("none"))
        }
        fn offset_address(
            &self,
            canonical: &str,
            _kind: &banto_driver_api::TagKind,
            _n: i64,
        ) -> Result<String, banto_driver_api::AddressError> {
            Ok(canonical.to_owned())
        }
        fn address_help(&self) -> String {
            String::new()
        }
    }

    async fn registry_service(drivers: &[(&str, &[&str], u32)]) -> PlcConnectionService {
        let mut registry = DriverRegistry::new();
        for (id, secret_keys, version) in drivers {
            let descriptor = banto_driver_api::DriverDescriptor::new(
                banto_driver_api::DriverId::new(id).unwrap(),
                "0.0.0",
                *id,
                banto_driver_api::Capabilities::READ_ONLY,
            )
            .with_settings_version(*version);
            registry
                .register(banto_driver_api::DriverEntry::host_provided(Arc::new(
                    TestDriver {
                        descriptor,
                        secret_keys: secret_keys.iter().map(|k| k.to_string()).collect(),
                    },
                )))
                .unwrap();
        }
        let pool = banto_storage::connect_sqlite_memory()
            .await
            .expect("connect_sqlite_memory");
        migrate(&pool).await.expect("migrate");
        PlcConnectionService::with_registry(pool, Arc::new(registry))
    }

    /// Design §4.2 with a registry: (1) first - when it refuses, only its errors come
    /// back (the driver is not consulted); when it passes, the driver's errors on the
    /// final `settings` come back in the REST spelling; an unregistered protocol is a
    /// `protocol` error. Secret keys and `settings_version` come from the driver.
    #[tokio::test]
    async fn with_a_registry_the_old_rules_run_first_then_the_driver() {
        let svc = registry_service(&[("modbus-tcp", &[], 1), ("postgres", &["password"], 3)]).await;

        // (1) refuses (port 0 and unit 300): only the old errors, no driver error.
        let mut bad = sample_input("Bad");
        bad.port = 0;
        bad.unit_id = 300;
        let mut fields = only_fields(svc.create(bad).await.unwrap_err());
        fields.sort();
        assert_eq!(fields, ["port", "unitId"]);

        // (1) passes, the driver refuses port 1.
        let mut port1 = sample_input("Port1");
        port1.port = 1;
        let err = svc.create(port1).await.unwrap_err();
        let BantoError::Validation { field_errors } = err else {
            panic!("expected validation");
        };
        assert_eq!(field_errors.len(), 1);
        assert_eq!(
            (
                field_errors[0].field.as_str(),
                field_errors[0].message.as_str()
            ),
            ("port", "driver says no")
        );

        // An unregistered protocol (slmp is in ALLOWED_PROTOCOLS, not in this registry).
        let mut slmp = sample_input("Slmp");
        slmp.protocol = "slmp".to_string();
        assert_eq!(
            only_fields(svc.create(slmp).await.unwrap_err()),
            ["protocol"]
        );

        // settings_version and secret keys from the driver.
        let pg = svc
            .create(with_password(postgres_input("PG"), REF_A, "pw"))
            .await
            .unwrap();
        assert_eq!(pg.settings_version, 3);
        assert_eq!(pg.secret_ref(PASSWORD_KEY), Some(REF_A));
        let modbus = svc.create(sample_input("M")).await.unwrap();
        assert_eq!(modbus.settings_version, 1);
        // A secret set on a protocol whose driver holds no secret key is refused by (1).
        assert_eq!(
            only_fields(
                svc.create(with_password(sample_input("M2"), REF_B, "x"))
                    .await
                    .unwrap_err()
            ),
            ["password"]
        );
    }

    /// §10's R2b-1d note: what the write path stores reads back unchanged through the
    /// flat projection - every value the old validation accepts for a built-in protocol
    /// round-trips (`create` → `get`) through `settings` and `flat_view`.
    #[tokio::test]
    async fn validated_settings_read_back_identically_through_the_flat_view() {
        let svc = service().await;
        let cases: Vec<PlcConnectionInput> = vec![
            {
                let mut i = sample_input("m");
                i.host = " h ".to_string();
                i.port = 65535;
                i.unit_id = 0;
                i.word_order = "high_low".to_string();
                i
            },
            {
                let mut i = sample_input("s");
                i.protocol = "slmp".to_string();
                i.port = 1;
                i.cpu_type = Some("Q".to_string());
                i
            },
            {
                let mut i = postgres_input("p");
                i.database = Some(" d ".to_string());
                i.username = Some("u".repeat(128));
                i
            },
        ];
        for input in cases {
            let created = svc.create(input.clone()).await.unwrap();
            let again = svc.get(created.id).await.unwrap();
            assert_eq!(again, created);
            let normalized = normalize_plc_connection_input(input);
            let flat = crate::settings_json::legacy::flat_view(&again.protocol, &again.settings);
            assert_eq!(flat.host, normalized.host.trim(), "{}", again.name);
            assert_eq!(flat.port, normalized.port, "{}", again.name);
            if again.protocol == MODBUS_PROTOCOL {
                assert_eq!(flat.unit_id, normalized.unit_id);
            }
            if again.protocol != POSTGRES_PROTOCOL {
                assert_eq!(flat.word_order, normalized.word_order);
            }
            if again.protocol == "slmp" {
                assert_eq!(Some(flat.cpu_type.clone()), normalized.cpu_type);
            }
            if again.protocol == POSTGRES_PROTOCOL {
                assert_eq!(
                    flat.database.as_deref(),
                    normalized.database.as_deref().map(str::trim)
                );
                assert_eq!(flat.username, normalized.username);
            }
        }
    }

    // ---- R2b-1e: migration 0026, the plain-v1 cleanup count (design §6.5, #778 P2) ----

    async fn cleanup_count(svc: &PlcConnectionService) -> i64 {
        crate::connection_secret::plain_secret_cleanup_pending(&svc.pool)
            .await
            .unwrap()
    }

    /// Every path that deletes or rewrites a `plain-v1` secret row counts it in the
    /// writing transaction (the 0026 triggers): `Set` (the old row goes), `Clear`, a
    /// protocol change, delete, cascade delete, and raw SQL (the Hub's reseal and its
    /// integrity check's orphan delete). A rolled-back write leaves no count; deleting a
    /// `dpapi-machine-v1` row (ciphertext) counts nothing; clearing only clears the value
    /// it read.
    #[tokio::test]
    async fn deleting_or_rewriting_a_plain_secret_row_counts_a_cleanup() {
        let svc = service().await;
        assert_eq!(
            cleanup_count(&svc).await,
            0,
            "a fresh database needs no cleanup"
        );
        let pg = svc
            .create(with_password(postgres_input("PG"), REF_A, "pw-a"))
            .await
            .unwrap();
        assert_eq!(cleanup_count(&svc).await, 0, "an insert counts nothing");

        // Set: the old row is deleted.
        svc.update(pg.id, with_password(postgres_input("PG"), REF_B, "pw-b"))
            .await
            .unwrap();
        assert_eq!(cleanup_count(&svc).await, 1);
        // Keep: nothing deleted.
        svc.update(pg.id, postgres_input("PG")).await.unwrap();
        assert_eq!(cleanup_count(&svc).await, 1);
        // Clear.
        svc.update(
            pg.id,
            with_secret_change(postgres_input("PG"), SecretChange::Clear),
        )
        .await
        .unwrap();
        assert_eq!(cleanup_count(&svc).await, 2);
        // Protocol change.
        svc.update(pg.id, with_password(postgres_input("PG"), REF_A, "pw-a"))
            .await
            .unwrap();
        svc.update(pg.id, sample_input("PG")).await.unwrap();
        assert_eq!(cleanup_count(&svc).await, 3);
        // Delete and cascade delete.
        let d = svc
            .create(with_password(postgres_input("Del"), REF_B, "pw-b"))
            .await
            .unwrap();
        svc.delete(d.id).await.unwrap();
        assert_eq!(cleanup_count(&svc).await, 4);
        let c = svc
            .create(with_password(postgres_input("Cascade"), REF_C, "pw-c"))
            .await
            .unwrap();
        svc.cascade_delete(c.id).await.unwrap();
        assert_eq!(cleanup_count(&svc).await, 5);

        // A rolled-back write leaves no count (the trigger runs inside it).
        let e = svc
            .create(with_password(postgres_input("Rollback"), REF_A, "pw-r"))
            .await
            .unwrap();
        let mut tx = svc.pool.begin().await.unwrap();
        svc.delete_tx(&mut tx, e.id).await.unwrap();
        assert_eq!(
            crate::connection_secret::plain_secret_cleanup_pending(&mut *tx)
                .await
                .unwrap(),
            6
        );
        drop(tx);
        assert_eq!(cleanup_count(&svc).await, 5, "rolled back with the delete");

        // Raw SQL: the reseal to dpapi (a rewrite of a plain-v1 row) counts; deleting the
        // dpapi row afterwards does not; an UPDATE that changes neither blob nor scheme
        // does not.
        sqlx::query(
            "UPDATE plc_connection_secrets SET connection_id = connection_id WHERE ref = ?",
        )
        .bind(REF_A)
        .execute(&svc.pool)
        .await
        .unwrap();
        sqlx::query("UPDATE plc_connection_secrets SET blob = blob WHERE ref = ?")
            .bind(REF_A)
            .execute(&svc.pool)
            .await
            .unwrap();
        assert_eq!(cleanup_count(&svc).await, 5);
        sqlx::query(
            "UPDATE plc_connection_secrets SET scheme = 'dpapi-machine-v1', blob = X'00' \
             WHERE ref = ?",
        )
        .bind(REF_A)
        .execute(&svc.pool)
        .await
        .unwrap();
        assert_eq!(cleanup_count(&svc).await, 6);
        svc.delete(e.id).await.unwrap();
        assert_eq!(
            cleanup_count(&svc).await,
            6,
            "a dpapi row's delete counts nothing"
        );

        // Clearing: only the value read; a count added in between survives.
        assert!(
            !crate::connection_secret::clear_plain_secret_cleanup(&svc.pool, 5)
                .await
                .unwrap()
        );
        assert_eq!(cleanup_count(&svc).await, 6);
        assert!(
            crate::connection_secret::clear_plain_secret_cleanup(&svc.pool, 6)
                .await
                .unwrap()
        );
        assert_eq!(cleanup_count(&svc).await, 0);

        // A database whose record row was removed by hand: the trigger recreates it.
        sqlx::query("DELETE FROM plc_connection_secrets_cleanup")
            .execute(&svc.pool)
            .await
            .unwrap();
        assert_eq!(cleanup_count(&svc).await, 0);
        let f = svc
            .create(with_password(postgres_input("Again"), REF_B, "pw-f"))
            .await
            .unwrap();
        svc.delete(f.id).await.unwrap();
        assert_eq!(cleanup_count(&svc).await, 1);
    }
}
