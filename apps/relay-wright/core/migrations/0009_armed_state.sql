-- Global armed state: whether conditional auto-write is currently allowed to
-- actually write to a PLC (plan `luminous-discovering-goblet.md`, W1's
-- safety design / W3's engine). Single-row table (`CHECK(id = 1)`, same
-- "one settings row" shape as a key/value table collapsed to its simplest
-- form) so there is exactly one global arm/disarm switch in W1 - the plan
-- does not call for per-rule or per-connection arming.
--
-- *** SAFETY: this table PERSISTS the last-known armed state for audit/UI
-- history display ("was this armed before the last restart?") ONLY. ***
-- W3's engine MUST initialize its IN-MEMORY armed flag to `false`
-- (disarmed) on every process start, REGARDLESS of `armed_persisted`'s
-- stored value - the plan is explicit that automatic write behavior must
-- never resume just because the app restarted (e.g. after a crash, a
-- Windows update reboot, or a power cycle) while a previous session had
-- left it armed. Do NOT read `armed_persisted` as the initial in-memory
-- state in W3 - that would silently defeat this entire safety rule. See
-- `write_audit_log`'s `'arm'`/`'disarm'` actions for how transitions are
-- recorded going forward.
--
-- `INSERT OR IGNORE` in core/src/db.rs seeds the single row (id = 1,
-- disarmed) so W3 can always assume exactly one row exists rather than
-- handling an empty-table case.
CREATE TABLE IF NOT EXISTS armed_state (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    armed_persisted INTEGER NOT NULL DEFAULT 0,
    last_changed_at TEXT,
    last_changed_by TEXT
);
