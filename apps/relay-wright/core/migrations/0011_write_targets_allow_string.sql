-- S2 文字列タグ (relay-wright string-tags plan): widen write_targets.data_type's
-- CHECK to accept 'string' (MELSEC string devices) and add the companion
-- string_length column - the number of consecutive 16-bit word devices the
-- string occupies (SJIS capacity = 2 bytes per word). NULL for every
-- non-string target; 1..=128 for string targets. The cross-column "required
-- exactly when data_type='string'" rule is enforced at the application layer
-- (relay_wright_core::write_targets), mirroring banto-tags' 0005; the range
-- CHECK below is defense-in-depth only.
--
-- NOTE (this app's migration discipline, see src/db.rs's module doc): the
-- migrations/*.sql files in this crate are schema DOCUMENTATION - they are not
-- executed by sqlx::migrate!. The executable source of truth is
-- `db.rs::apply_app_schema` (fresh databases) plus
-- `db.rs::upgrade_write_targets_for_string` (existing pre-S2 databases, the
-- statements below); update all of them together.
--
-- SQLite cannot ALTER a CHECK constraint, so the table has to be rebuilt.
-- Unlike banto-tags' 0005 (tags is a leaf), `write_targets` is REFERENCED:
-- `write_rules.write_target_id` carries ON DELETE RESTRICT. With foreign keys
-- enforced (banto-storage connects every pool with foreign_keys=ON), that
-- means banto-tags' 0004 park-and-restore dance applies (see that file's
-- header for the three empirically-established constraints):
--
-- - `DROP TABLE write_targets` performs an implicit `DELETE FROM`, which the
--   children's RESTRICT rejects the moment any write_rules row exists - so the
--   descendants' ROWS must be parked and deleted first.
-- - Renaming the OLD table out of the way instead is not an option: modern
--   SQLite rewrites references to a renamed table in every other table's
--   schema, so `write_rules` would end up referencing the parked name.
-- - Only the NEW table may be renamed, into the vacated name: nothing
--   references `write_targets_new`, so that rename rewrites nothing, and
--   write_rules' existing REFERENCES write_targets(id) resolves to the
--   rebuilt table.
--
-- `write_rule_conditions` is parked too, one level down - it cascades from
-- write_rules, and its rows would be silently lost by `DELETE FROM
-- write_rules` otherwise.
--
-- A database that needs this upgrade is pre-S2 by construction, so the parked
-- write_rules rows still have the OLD column set (no write_constant_text -
-- 0013 runs after this) and the explicit column lists below name exactly that
-- old shape.
--
-- Same accepted behaviour change as banto-tags' 0004/0005: the rebuilt
-- table's AUTOINCREMENT high-water mark is re-seeded from the copied rows'
-- maximum id. Harmless for the same reason.
--
-- Verified end to end against a populated database by
-- `s2_upgrade_preserves_rows_and_foreign_keys_on_a_populated_pre_s2_db` in
-- src/db.rs.

-- Park the descendants, deepest first.
CREATE TEMPORARY TABLE _u0011_write_rule_conditions AS SELECT * FROM write_rule_conditions;
CREATE TEMPORARY TABLE _u0011_write_rules AS SELECT * FROM write_rules;
DELETE FROM write_rule_conditions;
DELETE FROM write_rules;

-- Rebuild write_targets with the widened CHECK + string_length.
CREATE TABLE write_targets_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE,
    plc_connection_id INTEGER NOT NULL,
    address TEXT NOT NULL,
    data_type TEXT NOT NULL CHECK (data_type IN ('bit', 'i16', 'u16', 'i32', 'u32', 'f32', 'string')),
    string_length INTEGER CHECK (string_length IS NULL OR string_length BETWEEN 1 AND 128),
    raw_lo REAL,
    raw_hi REAL,
    eng_lo REAL,
    eng_hi REAL,
    unit TEXT,
    decimals INTEGER NOT NULL DEFAULT 0 CHECK (decimals BETWEEN 0 AND 6),
    enabled INTEGER NOT NULL DEFAULT 1
);

INSERT INTO write_targets_new (
    id, name, plc_connection_id, address, data_type,
    raw_lo, raw_hi, eng_lo, eng_hi, unit, decimals, enabled
)
SELECT
    id, name, plc_connection_id, address, data_type,
    raw_lo, raw_hi, eng_lo, eng_hi, unit, decimals, enabled
FROM write_targets;

DROP TABLE write_targets;
ALTER TABLE write_targets_new RENAME TO write_targets;

-- 0005's index does not survive the rebuild (indexes belong to the dropped
-- table), so it is recreated here under its original name.
CREATE INDEX idx_write_targets_plc_connection_id ON write_targets (plc_connection_id);

-- Put the descendants back, shallowest first (old column shape - see above).
INSERT INTO write_rules (
    id, name, enabled, edge_mode, cooldown_ms, write_target_id,
    write_value_mode, write_constant_value, write_source_tag_id
)
SELECT
    id, name, enabled, edge_mode, cooldown_ms, write_target_id,
    write_value_mode, write_constant_value, write_source_tag_id
FROM _u0011_write_rules;

INSERT INTO write_rule_conditions (
    id, write_rule_id, source_tag_id, operator, threshold_value, threshold_value_2
)
SELECT
    id, write_rule_id, source_tag_id, operator, threshold_value, threshold_value_2
FROM _u0011_write_rule_conditions;

DROP TABLE _u0011_write_rules;
DROP TABLE _u0011_write_rule_conditions;
