//! タグモニタ (feature/tag-monitor): the monitor read / manual-write surface,
//! implemented as further methods on [`EngineControl`] so both wiring paths
//! (the Tauri commands and the REST routes) get it for free through the
//! existing [`crate::engine::SharedEngineControl`] slot.
//!
//! ## The one hard constraint: one SLMP session per connected port
//!
//! The real R08ENCPU accepts only ONE concurrent SLMP TCP connection **per
//! port** (a second connect to a port that already has a live session times
//! out - verified on hardware 2026-08-07; a second port opened via the CPU's
//! own parameters carries its own simultaneous session fine), so this module
//! NEVER opens its own `SlmpClient` against a connection the engine broker
//! already manages. Every monitor read and every manual write goes through
//! the engine broker's per-connection task, reached via the
//! [`banto_broker::SessionDirectory`] carried inside
//! [`EngineControl`]. A connection the engine has no session for yet (created
//! after engine start, or on an engine built with a connection subset) gets
//! one spawned ON DEMAND and kept - see `SessionDirectory::ensure_connection`.
//!
//! A session's host/port are captured when its task spawns, so EDITING a
//! connection's host/port does not re-dial an already-spawned session - the
//! same "compiled at engine start" semantics every other connection consumer
//! has. An engine reload (or app restart) picks up the new coordinates.
//!
//! ## Deliberately relaxed safety (debug app - user's explicit choice)
//!
//! Manual writes have NO arm gate, NO rate limiter, and NO dry-run
//! interception: this is a debug tool and the user explicitly relaxed those
//! for this screen to keep it snappy. What remains is the audit trail - every
//! manual write attempt (including one that fails validation or the wire)
//! inserts a `write_audit_log` row with `action = 'manual_write'`, the
//! caller's username, and the target's address info in the detail JSON, so
//! the trail doubles as debug history. The wire encoder
//! (`banto-plc-write/src/encode.rs`) still enforces per-type range / SJIS
//! checks, so a nonsense value is rejected before any wire traffic.
//!
//! ## H2 (2026-08-08 オーナー決定, `docs/improvement-plan.md` H2 — B 案):
//! manual writes are opt-in, off by default
//!
//! The bypass above is real and permanent (this remains a debug tool), but it
//! used to be reachable unconditionally by any `editor`, which read as
//! "disarm stops writes" even though it never did for this screen. Manual
//! writes are now ADDITIONALLY gated by
//! [`crate::settings::SettingsService::monitor_config`]'s
//! `manual_write_enabled` flag (default `false`).
//! [`EngineControl::monitor_write`] is the one function that ever calls
//! `broker.write` for a manual write (reached from every real caller via
//! [`EngineControl::monitor_tag_write`]), so it checks the flag FIRST, before
//! touching `write_audit_log` or the wire, and rejects with
//! [`MANUAL_WRITE_DISABLED_MESSAGE`] when it is off. Toggling the setting is
//! Admin-only and audited as a `settings_change` (see `crate::rest`'s
//! `/api/monitor/config` and `src-tauri`'s `monitor_config_apply`). RBAC
//! (`editor`+ for the write itself) is still checked first, unchanged, by the
//! wiring layer before it ever calls in here (invariant: this module never
//! re-checks role).
//!
//! A REJECTED attempt gets NO `write_audit_log` row at all: that table's
//! `action`/`result` CHECK constraints are fixed (an existing on-disk DB
//! cannot widen them, `migrations/0014_write_audit_log_manual_write.sql`) and
//! neither value space has a "denied by settings" member, so this module
//! deliberately does not try to force the new case into either enum. Instead
//! the REST/Tauri wiring layers, which detect the rejection via
//! [`is_manual_write_disabled`], record it to the general `audit_log` the
//! same way an RBAC denial is recorded (`action: "denied"`, `resource:
//! "monitor"`). That recording deliberately happens at the wiring layer and
//! not here: `audit_log` rows carry `origin` (`"rest"` vs `"tauri"`), and
//! this module, reached identically from both paths, has no way to know
//! which one is calling - writing that row from inside `EngineControl` would
//! either have to guess `origin` or drop the field's contract for this one
//! entry. Both wiring layers reach the SAME check (a single call to
//! [`is_manual_write_disabled`] on the error `monitor_tag_write`/
//! `monitor_write` already returned), so there is exactly one place per path
//! that could miss it and no way for the two to double up: each records at
//! most once, from its own already-terminal error.
//!
//! ## Wire-shape layer
//!
//! [`EngineControl::monitor_group_read`] / [`EngineControl::monitor_tag_write`]
//! speak TAG-level vocabulary (tag ids, engineering values, display rounding)
//! so the frontend stays dumb: scaling (`banto_tags::scale_raw`) and the
//! tag's `decimals` are applied to reads - the monitor shows the same
//! engineering values the rule engine compares against - and a written value
//! is parsed per the tag's data type and UNSCALED (`banto_tags::unscale`,
//! engineering → raw) before encoding. The lower-level
//! [`EngineControl::monitor_read`] / [`EngineControl::monitor_write`] carry
//! raw batch requests for callers (tests) that already resolved the wire
//! shape.
//!
//! ## SLMP-only is THIS module's own product-scope decision, not a broker
//! limitation
//!
//! `banto-broker` gained a `"modbus-tcp"` driver (Issue #131, 2026-09-01) and
//! serves Modbus reads/writes through `SessionDirectory::ensure_connection`
//! just fine - the broker itself no longer rejects a non-SLMP connection.
//! Every public entry point below (`monitor_read`, `monitor_write`,
//! `monitor_group_read`, and transitively `monitor_tag_write`) therefore
//! calls [`require_slmp`] itself, mirroring `engine::mod`'s own
//! `SLMP_PROTOCOL` filter on `Engine::start`'s managed-connection set. This
//! exists purely because relay-wright's タグモニタ - a MANUAL WRITE surface -
//! has never been reviewed or designed for anything but SLMP; extending it to
//! Modbus is a separate, unreviewed product decision that Issue #131's
//! banto-hub-only scope does not authorize. A reader wondering "why does the
//! monitor reject Modbus" should stop here, not go looking at the broker (it
//! already supports Modbus and will not explain this rejection).

use banto_core::{BantoError, FieldError};
use banto_plc::{Address, BatchReadRequest, BatchReadResult, PlcValue, TagValue};
use banto_plc_write::{BatchWriteRequest, StringWriteRequest, WriteRequest};
use banto_tags::{scale_raw, unscale, PlcConnection, PlcConnectionService, Scaling};
use serde::Serialize;
use serde_json::json;

use super::poller::ResolvedSource;
use super::rule_engine::WireShape;
use super::write_audit::{insert_row, set_result, AuditAction, AuditResult, AuditRow};
use super::{wire_shape, EngineControl};
use crate::settings::SettingsService;

/// The audit label used for every manual write's `rule_name_snapshot` (the
/// column is NOT NULL; non-rule actions carry a short label - same convention
/// as arm/disarm rows using their action name).
const MANUAL_WRITE_LABEL: &str = "手動書き込み";

/// The exact rejection [`EngineControl::monitor_write`] returns when manual
/// writes are disabled by settings (H2, module doc). Kept as a constant
/// (rather than formatted ad hoc at the call site) so [`is_manual_write_disabled`]
/// can match on it exactly - the REST/Tauri wiring layers use that to decide
/// whether a failed write should additionally be recorded to the general
/// `audit_log` as a denial (module doc explains why that recording happens
/// there and not in this module).
const MANUAL_WRITE_DISABLED_MESSAGE: &str =
    "手動書き込みは設定で無効です(設定画面から有効化できます)";

/// True if `err` is exactly the [`MANUAL_WRITE_DISABLED_MESSAGE`] rejection -
/// i.e. this specific `monitor_tag_write`/`monitor_write` call failed because
/// H2's settings gate is closed, as opposed to any other reason (RBAC is
/// checked earlier by the caller and never reaches this far; a broker/wire
/// error also comes back as `BantoError::Other` but with a different message).
/// See this module's doc comment for why the REST/Tauri wiring layers - not
/// this module - are the ones that call this to decide whether to audit a
/// denial.
pub fn is_manual_write_disabled(err: &BantoError) -> bool {
    matches!(err, BantoError::Other(message) if message == MANUAL_WRITE_DISABLED_MESSAGE)
}

/// One display-ready monitor reading (camelCase on the wire, both paths).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MonitorValue {
    pub tag_id: i64,
    pub tag_name: String,
    pub address: String,
    pub data_type: String,
    pub unit: Option<String>,
    /// The display value: a number for numeric tags (scaling + `decimals`
    /// rounding applied; bit tags read as `0`/`1`), a string for string tags,
    /// `null` when `quality` is `"bad"`.
    pub value: Option<serde_json::Value>,
    /// `"good"` or `"bad"` - per-tag, so one dead address never hides its
    /// batch-mates.
    pub quality: String,
    /// Why `quality` is `"bad"`, when it is.
    pub error: Option<String>,
}

impl MonitorValue {
    fn bad(row: &TagRow, error: String) -> Self {
        Self {
            tag_id: row.id,
            tag_name: row.name.clone(),
            address: row.address.clone(),
            data_type: row.data_type.clone(),
            unit: row.unit.clone(),
            value: None,
            quality: "bad".to_string(),
            error: Some(error),
        }
    }

    fn good(row: &TagRow, value: serde_json::Value) -> Self {
        Self {
            tag_id: row.id,
            tag_name: row.name.clone(),
            address: row.address.clone(),
            data_type: row.data_type.clone(),
            unit: row.unit.clone(),
            value: Some(value),
            quality: "good".to_string(),
            error: None,
        }
    }
}

/// One `tags` row as the monitor loads it (subset of `banto_tags::Tag`).
#[derive(Debug, Clone, sqlx::FromRow)]
struct TagRow {
    id: i64,
    name: String,
    address: String,
    data_type: String,
    string_length: Option<i64>,
    raw_lo: Option<f64>,
    raw_hi: Option<f64>,
    eng_lo: Option<f64>,
    eng_hi: Option<f64>,
    unit: Option<String>,
    decimals: i64,
}

impl TagRow {
    /// Same all-or-nothing collapse as `banto_tags::Tag::scaling`.
    fn scaling(&self) -> Option<Scaling> {
        match (self.raw_lo, self.raw_hi, self.eng_lo, self.eng_hi) {
            (Some(raw_lo), Some(raw_hi), Some(eng_lo), Some(eng_hi)) => Some(Scaling {
                raw_lo,
                raw_hi,
                eng_lo,
                eng_hi,
            }),
            _ => None,
        }
    }
}

/// Round to `decimals` places for display - the monitor's twin of the trend/
/// grid display rounding, applied server-side so the frontend stays dumb.
fn round_decimals(value: f64, decimals: i64) -> f64 {
    let factor = 10f64.powi(decimals.clamp(0, 6) as i32);
    (value * factor).round() / factor
}

fn validation_error(message: String) -> BantoError {
    BantoError::Validation {
        field_errors: vec![FieldError {
            field: "value".to_string(),
            message,
        }],
    }
}

/// relay-wright's own SLMP-only product-scope gate for the tag monitor -
/// mirrors `engine::mod`'s `SLMP_PROTOCOL` filter on `Engine::start`'s
/// managed-connection set (the `let keep = c.protocol == SLMP_PROTOCOL;`
/// line). This is NOT a broker limitation - `banto-broker` gained a
/// `"modbus-tcp"` driver in Issue #131 (2026-09-01) and serves Modbus reads/
/// writes through `SessionDirectory::ensure_connection` just fine today. This
/// check exists purely because relay-wright itself has never been reviewed or
/// designed for anything but SLMP (the engine's own rule evaluation already
/// skips non-SLMP connections outright); extending the tag monitor - a MANUAL
/// WRITE surface - to Modbus is a separate, unreviewed product decision that
/// Issue #131's banto-hub-only scope does not authorize. If relay-wright ever
/// does add Modbus support, this is the gate (and `engine::mod`'s matching
/// one) to remove together - do not let one drift ahead of the other.
fn require_slmp(connection: &PlcConnection) -> Result<(), BantoError> {
    if connection.protocol != super::SLMP_PROTOCOL {
        return Err(BantoError::Other(format!(
            "タグモニタは SLMP 接続のみ対応です(接続 {} は protocol={}, relay-wright は現時点で他プロトコルに未対応)",
            connection.id, connection.protocol
        )));
    }
    Ok(())
}

impl EngineControl {
    /// Low-level monitor read: resolve `connection_id` in the registry,
    /// ensure its broker session (spawning one on demand - see the module
    /// doc), and read `requests` through it via a [`super::ReadOnlyHandle`].
    /// Non-SLMP connections are rejected by [`require_slmp`] - relay-wright's
    /// own product-scope gate (module doc's "SLMP-only" note), not a broker
    /// constraint (the broker itself accepts Modbus TCP as of Issue #131).
    pub async fn monitor_read(
        &self,
        connection_id: i64,
        requests: Vec<BatchReadRequest>,
    ) -> Result<Vec<BatchReadResult>, BantoError> {
        let connection = PlcConnectionService::new(self.pool.clone())
            .get(connection_id)
            .await?;
        require_slmp(&connection)?;
        let handle = self
            .sessions
            .ensure_connection(&connection)
            .map_err(|e| BantoError::Other(e.to_string()))?;
        handle
            .read_only()
            .read(requests)
            .await
            .map_err(|e| BantoError::Other(e.to_string()))
    }

    /// Low-level manual write: audit (log-before-write, `action =
    /// 'manual_write'`, provisionally `failed` until the wire confirms), then
    /// write `request` through the connection's broker session. NO arm gate,
    /// NO rate limiter, NO dry-run interception - the user's explicit
    /// relaxation for this debug screen (module doc). The audit row is left
    /// `failed` on any wire/session error, which is exactly the evidence a
    /// debug history wants.
    ///
    /// H2 (module doc): this is the single chokepoint every manual write
    /// passes through (`monitor_tag_write` - the real entry point for both
    /// REST and Tauri - calls here on a successfully-parsed write, and any
    /// lower-level caller that already has a resolved `BatchWriteRequest`
    /// necessarily calls here too), so the `manual_write_enabled` gate is
    /// checked FIRST, before anything else in this function: no
    /// `write_audit_log` row, no session lookup, no wire traffic when it is
    /// off. See [`is_manual_write_disabled`] for how callers detect this
    /// specific rejection.
    pub async fn monitor_write(
        &self,
        connection: &PlcConnection,
        request: BatchWriteRequest,
        actor: Option<&str>,
        source_tag_id: Option<i64>,
        detail: serde_json::Value,
    ) -> Result<(), BantoError> {
        if !SettingsService::new(self.pool.clone())
            .monitor_config()
            .await?
            .manual_write_enabled
        {
            return Err(BantoError::Other(MANUAL_WRITE_DISABLED_MESSAGE.to_string()));
        }

        // The numeric audit column carries the RAW value that goes to the
        // wire (a string write leaves it NULL and carries its text in the
        // detail JSON - same split as writer.rs).
        let written_f64: Option<f64> = match &request {
            BatchWriteRequest::Numeric(w) => Some(match w.value {
                TagValue::Bit(b) => {
                    if b {
                        1.0
                    } else {
                        0.0
                    }
                }
                TagValue::F64(v) => v,
            }),
            BatchWriteRequest::String(_) => None,
            // T8 (docs/tag-server-design.md §6.1): bit-in-word RMW writes.
            // `monitor_write` is a generic low-level entry point that
            // forwards whatever `BatchWriteRequest` it is handed to the
            // broker (this match only decides the audit row's numeric
            // column), so it is exhaustiveness-complete for the new variant
            // even though the tag monitor's manual-write UI
            // (`monitor_tag_write`'s `build()`, below) does not yet
            // construct one - wiring `.N` bit-in-word tags into relay-wright's
            // own UI is a separate slice, not part of T8-1's driver-layer
            // scope. Same 1.0/0.0 convention as `TagValue::Bit` above.
            BatchWriteRequest::BitInWord { value, .. } => Some(if *value { 1.0 } else { 0.0 }),
        };

        let row = AuditRow::new(
            AuditAction::ManualWrite,
            AuditResult::Failed,
            MANUAL_WRITE_LABEL,
        )
        .with_source(source_tag_id, None)
        .with_actor(actor)
        .with_detail(detail.to_string());
        let row = AuditRow {
            target_value_written: written_f64,
            ..row
        };
        // Log-before-write (invariant #5's convention): the row exists -
        // provisionally `failed` - before any wire traffic, so a crash
        // mid-write still leaves evidence a write was in flight.
        let audit_id = insert_row(&self.pool, &row).await?;

        // relay-wright's own SLMP-only gate (module doc): checked here, AFTER
        // the audit row above, so a rejected write to a non-SLMP connection
        // leaves the same `failed` audit trail it did back when the broker
        // itself rejected the connection at `ensure_connection` (this used to
        // be an implicit side effect of that broker-level rejection; now that
        // the broker accepts Modbus TCP too, `require_slmp` reproduces it
        // explicitly, at the same point in the sequence).
        require_slmp(connection)?;

        let handle = self
            .sessions
            .ensure_connection(connection)
            .map_err(|e| BantoError::Other(e.to_string()))?;
        let results = handle
            .write(vec![request])
            .await
            .map_err(|e| BantoError::Other(e.to_string()))?;
        match results.first() {
            Some(banto_plc_write::WriteResult::Ok) => {
                set_result(&self.pool, audit_id, AuditResult::Ok).await?;
                Ok(())
            }
            Some(banto_plc_write::WriteResult::Bad(e)) => Err(validation_error(e.to_string())),
            None => Err(BantoError::Other(
                "書き込み結果が返されませんでした".to_string(),
            )),
        }
    }

    /// The monitor's per-group read (wire-shape layer, module doc): resolve
    /// the group's ENABLED tags, batch-read them over the group's connection
    /// in one broker round trip, and return display-ready values (scaling +
    /// `decimals` applied to numerics, bit as 0/1, string as text). A tag
    /// whose address/type will not resolve, a per-tag `Bad` read, and a
    /// whole-connection failure (session down) all degrade to per-tag
    /// `quality: "bad"` entries - the monitor keeps rendering. Only a
    /// missing group/connection or a non-SLMP connection is a call-level
    /// error - the latter via [`require_slmp`], relay-wright's own
    /// product-scope gate (module doc), not a broker-level rejection.
    pub async fn monitor_group_read(
        &self,
        collection_group_id: i64,
    ) -> Result<Vec<MonitorValue>, BantoError> {
        let connection_id: Option<i64> =
            sqlx::query_scalar("SELECT plc_connection_id FROM collection_groups WHERE id = ?")
                .bind(collection_group_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(banto_storage::storage_error)?;
        let Some(connection_id) = connection_id else {
            return Err(BantoError::NotFound {
                resource: "collection_groups".to_string(),
                id: collection_group_id.to_string(),
            });
        };
        let connection = PlcConnectionService::new(self.pool.clone())
            .get(connection_id)
            .await?;
        require_slmp(&connection)?;

        let rows: Vec<TagRow> = sqlx::query_as(
            "SELECT id, name, address, data_type, string_length, \
                    raw_lo, raw_hi, eng_lo, eng_hi, unit, decimals \
             FROM tags WHERE collection_group_id = ? AND enabled = 1 ORDER BY id",
        )
        .bind(collection_group_id)
        .fetch_all(&self.pool)
        .await
        .map_err(banto_storage::storage_error)?;

        // Resolve each tag to a wire request; unresolvable ones become `bad`
        // entries immediately (never dropped - the monitor should show them).
        let mut values: Vec<Option<MonitorValue>> = Vec::with_capacity(rows.len());
        let mut readable: Vec<(usize, &TagRow, BatchReadRequest)> = Vec::new();
        for (index, row) in rows.iter().enumerate() {
            let shape = wire_shape(&row.data_type, row.string_length, "monitor tag");
            match (Address::parse_slmp(&row.address), shape) {
                (Ok(address), Some(shape)) => {
                    let source = ResolvedSource {
                        tag_id: row.id,
                        connection_id,
                        address,
                        shape,
                    };
                    readable.push((index, row, source.to_request()));
                    values.push(None);
                }
                (Err(e), _) => {
                    values.push(Some(MonitorValue::bad(
                        row,
                        format!("アドレスを解析できません: {e}"),
                    )));
                }
                (_, None) => {
                    values.push(Some(MonitorValue::bad(
                        row,
                        "データ型/文字列長を解決できません".to_string(),
                    )));
                }
            }
        }

        if !readable.is_empty() {
            let requests: Vec<BatchReadRequest> = readable.iter().map(|(_, _, req)| *req).collect();
            let handle = self
                .sessions
                .ensure_connection(&connection)
                .map_err(|e| BantoError::Other(e.to_string()))?;
            match handle.read_only().read(requests).await {
                Ok(results) => {
                    // The broker preserves request order in its results.
                    for ((index, row, _), result) in readable.iter().zip(results) {
                        values[*index] = Some(match result {
                            BatchReadResult::Value(value) => display_value(row, value),
                            BatchReadResult::Bad(e) => MonitorValue::bad(row, e.to_string()),
                        });
                    }
                }
                Err(e) => {
                    // Whole connection down/failed this cycle: every readable
                    // tag is bad with the session-level reason.
                    let message = e.to_string();
                    for (index, row, _) in &readable {
                        values[*index] = Some(MonitorValue::bad(row, message.clone()));
                    }
                }
            }
        }

        Ok(values
            .into_iter()
            .map(|v| v.expect("every tag row produced a MonitorValue"))
            .collect())
    }

    /// The monitor's manual tag write (wire-shape layer, module doc): look up
    /// the tag, parse `input` per its data type (bit: 0/1/true/false;
    /// numeric: engineering value, UNSCALED to raw and - for integer types
    /// with scaling - rounded to the nearest raw count; string: Shift-JIS,
    /// validated against `2 × string_length` bytes), then write through
    /// [`Self::monitor_write`]. Every attempt that resolves to a real tag is
    /// audited (`action = 'manual_write'`), including validation failures
    /// (result `failed`, the reason in the detail JSON) - debug history.
    pub async fn monitor_tag_write(
        &self,
        tag_id: i64,
        input: &str,
        actor: Option<&str>,
    ) -> Result<(), BantoError> {
        type WriteTagRow = (
            String,
            String,
            String,
            Option<i64>,
            Option<f64>,
            Option<f64>,
            Option<f64>,
            Option<f64>,
            i64,
        );
        let row: Option<WriteTagRow> = sqlx::query_as(
            "SELECT t.name, t.address, t.data_type, t.string_length, \
                    t.raw_lo, t.raw_hi, t.eng_lo, t.eng_hi, cg.plc_connection_id \
             FROM tags t JOIN collection_groups cg ON t.collection_group_id = cg.id \
             WHERE t.id = ?",
        )
        .bind(tag_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(banto_storage::storage_error)?;
        let Some((
            name,
            address_raw,
            data_type,
            string_length,
            raw_lo,
            raw_hi,
            eng_lo,
            eng_hi,
            connection_id,
        )) = row
        else {
            return Err(BantoError::NotFound {
                resource: "tags".to_string(),
                id: tag_id.to_string(),
            });
        };
        let scaling = match (raw_lo, raw_hi, eng_lo, eng_hi) {
            (Some(raw_lo), Some(raw_hi), Some(eng_lo), Some(eng_hi)) => Some(Scaling {
                raw_lo,
                raw_hi,
                eng_lo,
                eng_hi,
            }),
            _ => None,
        };

        let connection = PlcConnectionService::new(self.pool.clone())
            .get(connection_id)
            .await?;

        let input = input.trim();
        let mut detail = json!({
            "connectionId": connection.id,
            "connectionName": connection.name,
            "tagName": name,
            "address": address_raw,
            "dataType": data_type,
            "input": input,
        });

        // Resolve + parse. A failure here is audited as a failed manual write
        // (with the reason) and surfaced as a field-level validation error.
        let build = || -> Result<BatchWriteRequest, String> {
            let address = Address::parse_slmp(&address_raw)
                .map_err(|e| format!("アドレスを解析できません: {e}"))?;
            let shape = wire_shape(&data_type, string_length, "monitor tag")
                .ok_or_else(|| "データ型/文字列長を解決できません".to_string())?;
            match shape {
                WireShape::Numeric(banto_plc::DataType::Bit) => {
                    let bit = match input.to_ascii_lowercase().as_str() {
                        "0" | "false" | "off" => false,
                        "1" | "true" | "on" => true,
                        _ => {
                            return Err("bit タグには 0/1（または true/false）を入力してください"
                                .to_string())
                        }
                    };
                    Ok(BatchWriteRequest::Numeric(WriteRequest {
                        address,
                        data_type: banto_plc::DataType::Bit,
                        value: TagValue::Bit(bit),
                    }))
                }
                WireShape::Numeric(data_type) => {
                    let eng: f64 = input
                        .parse()
                        .map_err(|_| format!("数値として解析できません: {input}"))?;
                    if !eng.is_finite() {
                        return Err("有限の数値を入力してください".to_string());
                    }
                    // Engineering → raw (the tag's scaling, if any), then -
                    // only when a scaling was applied - snap to the nearest
                    // representable raw count for integer wire types (the
                    // encoder rejects fractional raw values; a raw value the
                    // user typed directly is deliberately NOT rounded, so a
                    // typo like 3.5 into a u16 errors instead of writing 4).
                    let mut raw = match &scaling {
                        Some(s) => {
                            let raw = unscale(eng, s);
                            if !raw.is_finite() {
                                return Err(
                                    "スケーリングの工業値スパンが0のため書き込めません".to_string()
                                );
                            }
                            raw
                        }
                        None => eng,
                    };
                    if scaling.is_some()
                        && matches!(
                            data_type,
                            banto_plc::DataType::I16
                                | banto_plc::DataType::U16
                                | banto_plc::DataType::I32
                                | banto_plc::DataType::U32
                        )
                    {
                        raw = raw.round();
                    }
                    Ok(BatchWriteRequest::Numeric(WriteRequest {
                        address,
                        data_type,
                        value: TagValue::F64(raw),
                    }))
                }
                WireShape::Str { words } => {
                    if let Some(message) = crate::support::sjis_text_error(input, words as i64) {
                        return Err(message);
                    }
                    Ok(BatchWriteRequest::String(StringWriteRequest {
                        address,
                        words,
                        value: input.to_string(),
                        // T20 ①a: kept hardcoded Shift-JIS - see
                        // `crate::engine::writer::build_request`'s identical
                        // comment for why relay-wright never adopts the new
                        // per-tag `string_encoding` concept.
                        encoding: banto_plc_write::StringEncoding::ShiftJis,
                    }))
                }
            }
        };

        match build() {
            Ok(request) => {
                if let BatchWriteRequest::Numeric(w) = &request {
                    if let TagValue::F64(raw) = w.value {
                        detail["rawValue"] = json!(raw);
                    }
                }
                self.monitor_write(&connection, request, actor, Some(tag_id), detail)
                    .await
            }
            Err(message) => {
                detail["error"] = json!(message);
                let row = AuditRow::new(
                    AuditAction::ManualWrite,
                    AuditResult::Failed,
                    MANUAL_WRITE_LABEL,
                )
                .with_source(Some(tag_id), None)
                .with_actor(actor)
                .with_detail(detail.to_string());
                insert_row(&self.pool, &row).await?;
                Err(validation_error(message))
            }
        }
    }
}

/// Turn one wire reading into its display-ready [`MonitorValue`]: bit → 0/1,
/// numeric → scaling + `decimals` rounding, string → text.
fn display_value(row: &TagRow, value: PlcValue) -> MonitorValue {
    match value {
        PlcValue::Bit(b) => MonitorValue::good(row, json!(if b { 1 } else { 0 })),
        PlcValue::F64(raw) => {
            let eng = match row.scaling() {
                Some(s) => scale_raw(raw, &s),
                None => raw,
            };
            let rounded = round_decimals(eng, row.decimals);
            if rounded.is_finite() {
                MonitorValue::good(row, json!(rounded))
            } else {
                MonitorValue::bad(row, "値が有限ではありません".to_string())
            }
        }
        PlcValue::Str(s) => MonitorValue::good(row, json!(s)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_decimals_rounds_half_away_and_clamps() {
        assert_eq!(round_decimals(1.25, 1), 1.3);
        assert_eq!(round_decimals(1.24, 1), 1.2);
        assert_eq!(round_decimals(1.5, 0), 2.0);
        // Out-of-range decimals clamp to the schema's 0..=6 rather than
        // producing a wild factor.
        assert_eq!(round_decimals(1.23456789, 99), 1.234568);
    }
}
