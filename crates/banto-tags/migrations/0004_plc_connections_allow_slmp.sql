-- Widen plc_connections.protocol's CHECK to accept 'slmp' (MELSEC MC protocol)
-- alongside 'modbus-tcp'. This is what migration 0001's own comment anticipated
-- ("adding 'slmp' ... later only needs a migration"), landing with I2a's
-- banto-plc::slmp read client.
--
-- SQLite cannot ALTER a CHECK constraint, so the table has to be rebuilt. The
-- order below is not the textbook 12-step rebuild, and the difference matters,
-- so here is why - all three constraints below were established empirically
-- (see the test named at the bottom):
--
-- 1. **This runs inside a transaction, and cannot opt out.** sqlx parses a
--    leading `-- no-transaction` directive, but its SQLite driver ignores it:
--    `Migrate::apply` in sqlx-sqlite 0.8 unconditionally opens a transaction so
--    the migration and its bookkeeping row commit together. Therefore
--    `PRAGMA foreign_keys = OFF` - the usual way to make a table rebuild
--    painless - is unavailable: SQLite silently ignores that pragma inside a
--    transaction. Anything here must work with foreign keys *enforced*.
--
-- 2. **`DROP TABLE` on a table with children fails.** With foreign keys on, DROP
--    performs an implicit `DELETE FROM`, which trips collection_groups'
--    `ON DELETE RESTRICT` the moment any collection group exists. So the table
--    that gets dropped must be one nothing references.
--    (`PRAGMA defer_foreign_keys = ON` is not a way around this: it lets the
--    DROP through, then fails at COMMIT, because the deferred violation it
--    recorded is never cancelled out by a later insert into a differently-named
--    table.)
--
-- 3. **A rename drags children's foreign keys with it.** Modern SQLite rewrites
--    references to a renamed table in every other table's schema, and
--    `PRAGMA legacy_alter_table = ON` does not reliably suppress that here. So
--    renaming plc_connections out of the way would re-point collection_groups at
--    the old table, with no supported way to point it back.
--
-- Constraint 3 fixes which table may be renamed: only the *new* one, into the
-- vacated name (nothing references `plc_connections_new`, so that rename
-- rewrites nothing, and collection_groups' existing
-- `REFERENCES plc_connections(id)` simply resolves to the rebuilt table).
-- Constraint 2 then requires plc_connections to have no referencing *rows* by
-- the time it is dropped.
--
-- Hence the shape below: park the descendant rows in temporary tables and
-- delete them, rebuild plc_connections, rename it into place, then put the
-- parked rows back. `tags` is parked too, one level down, for the same reason -
-- it references collection_groups with `ON DELETE RESTRICT`, so those rows
-- cannot be deleted while tags' rows point at them.
--
-- Nothing about tags or collection_groups *changes*: both are round-tripped
-- column for column. But a future table referencing either one will have to be
-- added to this dance, which is the standing cost of SQLite having no
-- ALTER CONSTRAINT.
--
-- Verified end to end against a populated database, run the way sqlx itself runs
-- it (one connection, one transaction), by
-- `migration_0004_preserves_rows_and_foreign_keys_on_a_populated_database` in
-- src/plc_connection.rs. That is the test to look at before changing any of the
-- above.
--
-- One accepted behaviour change: the rebuilt table's AUTOINCREMENT high-water
-- mark is re-seeded from the copied rows' maximum id rather than carried over,
-- so an id freed by an earlier DELETE can be reused after this migration.
-- Harmless (an id is only ever referenced while its row exists) and not worth
-- hand-editing sqlite_sequence for.

-- Park the descendants, deepest first.
CREATE TEMPORARY TABLE _m0004_tags AS SELECT * FROM tags;
CREATE TEMPORARY TABLE _m0004_collection_groups AS SELECT * FROM collection_groups;
DELETE FROM tags;
DELETE FROM collection_groups;

-- Rebuild plc_connections with the widened CHECK.
CREATE TABLE plc_connections_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE,
    protocol TEXT NOT NULL DEFAULT 'modbus-tcp' CHECK (protocol IN ('modbus-tcp', 'slmp')),
    host TEXT NOT NULL,
    port INTEGER NOT NULL,
    -- Modbus slave/unit id (docs/plan.md I1 spec: 既定1). Unused by SLMP, whose
    -- station addressing is the network/PC/IO/area access route in
    -- banto_plc::slmp::SlmpConfig rather than a single byte; kept required with
    -- its default so 'slmp' rows simply carry the default and no reader has to
    -- treat the column as optional.
    unit_id INTEGER NOT NULL DEFAULT 1,
    enabled INTEGER NOT NULL DEFAULT 1
);

INSERT INTO plc_connections_new (id, name, protocol, host, port, unit_id, enabled)
SELECT id, name, protocol, host, port, unit_id, enabled FROM plc_connections;

DROP TABLE plc_connections;
ALTER TABLE plc_connections_new RENAME TO plc_connections;

-- Put the descendants back, shallowest first.
INSERT INTO collection_groups (id, name, plc_connection_id, period_ms, enabled)
SELECT id, name, plc_connection_id, period_ms, enabled FROM _m0004_collection_groups;
INSERT INTO tags (
    id, name, collection_group_id, address, data_type,
    raw_lo, raw_hi, eng_lo, eng_hi, unit, decimals,
    threshold_h, threshold_hh, threshold_l, threshold_ll, enabled
)
SELECT
    id, name, collection_group_id, address, data_type,
    raw_lo, raw_hi, eng_lo, eng_hi, unit, decimals,
    threshold_h, threshold_hh, threshold_l, threshold_ll, enabled
FROM _m0004_tags;

DROP TABLE _m0004_tags;
DROP TABLE _m0004_collection_groups;
