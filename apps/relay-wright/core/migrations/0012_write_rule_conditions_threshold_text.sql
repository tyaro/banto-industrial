-- S2 文字列タグ: a condition on a STRING source tag compares text, not
-- numbers - its eq/neq comparand lives in the new nullable `threshold_text`
-- column and the numeric threshold columns stay NULL; a numeric condition is
-- exactly the reverse. Which side must be set depends on the source tag's
-- data type (banto-tags' `tags` table, a cross-migrator reference), so it is
-- enforced at the application layer
-- (relay_wright_core::write_rule_conditions/write_rules), not by SQL.
--
-- NOTE: like every file in this directory, this is schema DOCUMENTATION -
-- the executable source of truth is `db.rs::apply_app_schema` (fresh
-- databases) plus `db.rs::upgrade_write_rule_conditions_for_string`
-- (existing pre-S2 databases, the statements below). Update both together.
--
-- `threshold_value` also loses its NOT NULL here, and SQLite can neither
-- drop a NOT NULL nor alter it, so the table is rebuilt. It is a LEAF table
-- (nothing references write_rule_conditions), so banto-tags' 0005
-- leaf-rebuild pattern applies verbatim:
--
-- - `DROP TABLE write_rule_conditions` performs an implicit `DELETE FROM`,
--   which only deletes CHILD rows of write_rules; ON DELETE CASCADE
--   restricts nothing, so it passes with rows present.
-- - `ALTER TABLE ... RENAME TO write_rule_conditions` rewrites references to
--   `write_rule_conditions_new` in other tables' schemas - there are none -
--   and keeps its own REFERENCES write_rules(id) clause intact.
--
-- Verified against a populated database by
-- `s2_upgrade_preserves_rows_and_foreign_keys_on_a_populated_pre_s2_db` in
-- src/db.rs.

CREATE TABLE write_rule_conditions_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    write_rule_id INTEGER NOT NULL REFERENCES write_rules(id) ON DELETE CASCADE,
    source_tag_id INTEGER NOT NULL,
    operator TEXT NOT NULL CHECK (
        operator IN ('eq', 'neq', 'gt', 'gte', 'lt', 'lte', 'between', 'bit_is')
    ),
    threshold_value REAL,
    threshold_value_2 REAL,
    threshold_text TEXT
);

INSERT INTO write_rule_conditions_new (
    id, write_rule_id, source_tag_id, operator, threshold_value, threshold_value_2
)
SELECT
    id, write_rule_id, source_tag_id, operator, threshold_value, threshold_value_2
FROM write_rule_conditions;

DROP TABLE write_rule_conditions;
ALTER TABLE write_rule_conditions_new RENAME TO write_rule_conditions;

-- 0007's index does not survive the rebuild, so it is recreated here under
-- its original name.
CREATE INDEX idx_write_rule_conditions_write_rule_id
    ON write_rule_conditions (write_rule_id);
