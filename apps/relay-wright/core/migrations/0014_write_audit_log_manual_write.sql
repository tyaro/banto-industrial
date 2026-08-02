-- feature/tag-monitor (タグモニタ画面): widen `write_audit_log.action`'s CHECK
-- with 'manual_write' - the monitor's one-shot debug writes are audited under
-- that action, always carrying an `actor_username` (a human clicked the value
-- cell). Manual writes are deliberately NOT gated by arming/rate-limit/dry-run
-- (debug app; the user explicitly relaxed those for this screen) - the audit
-- row is the safety net that remains, so the trail doubles as debug history.
--
-- SQLite cannot ALTER a CHECK constraint, so an existing database is rebuilt.
-- `write_audit_log` is a LEAF table (nothing references it), so banto-tags'
-- 0005 leaf-rebuild pattern applies: copy every row explicitly (`ts` included,
-- so the DEFAULT never re-evaluates and audit history survives byte-for-byte),
-- drop, rename, recreate both indexes under their original names.
--
-- NOTE: like every file in this directory, this is schema DOCUMENTATION - the
-- executable source of truth is `db.rs::apply_app_schema` (fresh databases get
-- the widened CHECK in the CREATE TABLE) plus
-- `db.rs::upgrade_write_audit_log_for_manual_write` (existing pre-monitor
-- databases, detected by reading the stored CREATE TABLE text out of
-- sqlite_master - the CHECK change adds no column, so `pragma_table_info`
-- cannot see it).

CREATE TABLE write_audit_log_new (
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
        action IN ('rule_fire', 'arm', 'disarm', 'dry_run_toggle', 'rate_limit_tripped',
                   'manual_write')
    ),
    result TEXT NOT NULL CHECK (
        result IN (
            'ok', 'failed', 'suppressed_disarmed', 'suppressed_rate_limited',
            'suppressed_dry_run'
        )
    ),
    detail TEXT
);

INSERT INTO write_audit_log_new (
    id, ts, write_rule_id, rule_name_snapshot, source_tag_id, source_value_snapshot,
    write_target_id, target_value_written, actor_username, action, result, detail
)
SELECT
    id, ts, write_rule_id, rule_name_snapshot, source_tag_id, source_value_snapshot,
    write_target_id, target_value_written, actor_username, action, result, detail
FROM write_audit_log;

DROP TABLE write_audit_log;
ALTER TABLE write_audit_log_new RENAME TO write_audit_log;
CREATE INDEX idx_write_audit_log_ts ON write_audit_log (ts);
CREATE INDEX idx_write_audit_log_write_rule_id ON write_audit_log (write_rule_id);
