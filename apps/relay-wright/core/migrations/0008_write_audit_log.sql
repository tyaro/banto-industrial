-- Write audit log: append-only record of every write-affecting event (plan
-- `luminous-discovering-goblet.md`, W1/W3's "log-before-write" safety
-- rule). Modeled directly on this app's own generic `audit_log` pattern
-- (core/src/audit.rs / migrations/0004_audit_log.sql, itself inherited
-- unchanged from the banto template via ChronoGazer - see core/src/db.rs's
-- module doc comment) but kept as a SEPARATE table, not shared with it -
-- this log is specifically about PLC writes (rule fires, arm/disarm,
-- rate-limit trips), reviewed and retained independently from the generic
-- admin audit trail.
--
-- `write_rule_id`/`source_tag_id`/`write_target_id` are all NULLABLE and
-- deliberately NOT foreign keys, even though `write_rule_id` COULD be an
-- in-lineage FK onto `write_rules` (unlike the cross-lineage columns
-- elsewhere in this app's schema - see 0005_write_targets.sql): an audit
-- entry must remain readable after its rule/target is deleted (W2), which
-- is exactly why `rule_name_snapshot` denormalizes the rule's name into
-- this row at write time - the same "snapshot, don't reference" principle
-- `chronogazer`'s own `audit_log.entity_id` (a plain TEXT, not a FK) used.
-- `source_tag_id` has the additional reason that `tags` is banto-tags-owned
-- (cross-lineage, same as elsewhere in this schema).
--
-- `action`/`result` enums match the plan's W1 spec exactly. `detail` is a
-- JSON-encoded TEXT summary, same convention as `audit_log.detail`
-- (core/src/audit.rs's module doc: no secrets, no raw PLC register dumps -
-- only what a human reviewing the log needs).
CREATE TABLE IF NOT EXISTS write_audit_log (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ts TEXT NOT NULL DEFAULT (datetime('now')),
    write_rule_id INTEGER,
    rule_name_snapshot TEXT NOT NULL,
    source_tag_id INTEGER,
    source_value_snapshot REAL,
    write_target_id INTEGER,
    target_value_written REAL,
    actor_username TEXT,
    action TEXT NOT NULL CHECK (
        action IN ('rule_fire', 'arm', 'disarm', 'dry_run_toggle', 'rate_limit_tripped')
    ),
    result TEXT NOT NULL CHECK (
        result IN (
            'ok', 'failed', 'suppressed_disarmed', 'suppressed_rate_limited', 'suppressed_dry_run'
        )
    ),
    detail TEXT
);

CREATE INDEX IF NOT EXISTS idx_write_audit_log_ts ON write_audit_log (ts);
CREATE INDEX IF NOT EXISTS idx_write_audit_log_write_rule_id ON write_audit_log (write_rule_id);
