-- Write rule: one conditional-write behavior (plan
-- `luminous-discovering-goblet.md`, W1). A rule's condition set (1..N rows
-- in `write_rule_conditions`, AND-combined only - no free-form expression
-- language, matching recorder-requirements.md §7's existing SCADA-avoidance
-- policy) drives edge-detected writes to exactly one `write_targets` row.
--
-- `write_target_id` is a real SQL FOREIGN KEY: unlike `plc_connection_id`
-- below and in `write_targets`, `write_targets` is owned by THIS app's own
-- schema (applied in the same `apply_app_schema` call in core/src/db.rs,
-- not a separate migrator), so a normal in-lineage constraint applies.
-- ON DELETE RESTRICT mirrors banto-tags' own convention for a similar
-- child-holds-a-parent-id relationship (e.g. tags -> collection_groups).
--
-- `write_source_tag_id` (nullable: only meaningful when
-- write_value_mode = 'copy_from_source') is, like `write_targets.
-- plc_connection_id`, a plain unconstrained INTEGER referencing a
-- banto-tags-owned `tags` row across the migrator-lineage boundary - see
-- 0005_write_targets.sql's doc comment for why no FOREIGN KEY is used here.
CREATE TABLE IF NOT EXISTS write_rules (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE,
    enabled INTEGER NOT NULL DEFAULT 0,
    edge_mode TEXT NOT NULL CHECK (edge_mode IN ('rising', 'falling', 'change')),
    cooldown_ms INTEGER,
    write_target_id INTEGER NOT NULL REFERENCES write_targets(id) ON DELETE RESTRICT,
    write_value_mode TEXT NOT NULL CHECK (write_value_mode IN ('constant', 'copy_from_source')),
    write_constant_value REAL,
    write_source_tag_id INTEGER
);

CREATE INDEX IF NOT EXISTS idx_write_rules_write_target_id ON write_rules (write_target_id);
