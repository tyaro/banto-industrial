-- Write rule condition: one AND-combined threshold test within a
-- `write_rules` row (plan `luminous-discovering-goblet.md`, W1). A rule
-- with N condition rows requires ALL N to hold before its edge detector
-- (W3) considers the rule's condition "true" - there is no OR/free-form
-- expression support (recorder-requirements.md §7).
--
-- `write_rule_id` is an in-lineage FOREIGN KEY (see 0006_write_rules.sql's
-- doc comment) with ON DELETE CASCADE: a condition row has no independent
-- meaning once its parent rule is gone, unlike `write_audit_log` below
-- (which snapshots enough of a rule's identity to survive the rule's
-- deletion, by design).
--
-- `source_tag_id` is a plain unconstrained INTEGER referencing a
-- banto-tags-owned `tags` row, same reasoning as `write_rules.
-- write_source_tag_id` (see 0005_write_targets.sql's doc comment).
--
-- `threshold_value_2` is only used by the `between` operator (the second
-- bound); every other operator leaves it NULL.
CREATE TABLE IF NOT EXISTS write_rule_conditions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    write_rule_id INTEGER NOT NULL REFERENCES write_rules(id) ON DELETE CASCADE,
    source_tag_id INTEGER NOT NULL,
    operator TEXT NOT NULL CHECK (
        operator IN ('eq', 'neq', 'gt', 'gte', 'lt', 'lte', 'between', 'bit_is')
    ),
    threshold_value REAL NOT NULL,
    threshold_value_2 REAL
);

CREATE INDEX IF NOT EXISTS idx_write_rule_conditions_write_rule_id
    ON write_rule_conditions (write_rule_id);
