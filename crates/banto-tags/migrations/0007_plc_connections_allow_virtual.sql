-- Widen plc_connections.protocol's CHECK to accept 'virtual' alongside
-- 'modbus-tcp'/'slmp' (docs/tag-server-design.md §4.2/§4.3(a), T6-2,
-- 2026-08-05 決定: 演算タグ/内部タグの予約セグメント calc/mem は
-- protocol='virtual' の予約接続として実現する - see this crate's
-- `plc_connection.rs` module doc for the full rationale).
--
-- `host`/`port` need NO relaxation at the SQL level: both columns are
-- already only `NOT NULL` with no `CHECK` (0001's original shape) - an empty
-- `host` (`''`) and `port = 0` already satisfy `NOT NULL` today. The
-- relaxation for virtual connections (host may be empty, port may be 0) is
-- therefore entirely an application-layer change
-- (`validate_plc_connection_input` in `plc_connection.rs`), not a schema
-- change - this migration's only job is the `protocol` CHECK.
--
-- SQLite cannot ALTER a CHECK constraint, so the table has to be rebuilt -
-- same fundamental dance as 0004 (see that file's header for the three
-- empirically-established constraints this must respect: runs inside a
-- transaction with foreign keys ENFORCED, DROP on a referenced table fails,
-- and renaming a referenced table drags children's foreign keys along).
-- `tags` now carries more columns than it did at 0004 time (string_length
-- from 0005, writable/tag_kind/expression/retain from 0006), so the
-- park-and-restore below copies the FULL current shape, not 0004's narrower
-- one - copying a stale column list here would silently truncate every
-- existing tag row.
--
-- Verified end to end against a populated database (mirroring 0004/0005's
-- own tests) by
-- `migration_0007_preserves_rows_and_foreign_keys_on_a_populated_database`
-- in `src/plc_connection.rs`.

-- Park the descendants, deepest first.
CREATE TEMPORARY TABLE _m0007_tags AS SELECT * FROM tags;
CREATE TEMPORARY TABLE _m0007_collection_groups AS SELECT * FROM collection_groups;
DELETE FROM tags;
DELETE FROM collection_groups;

-- Rebuild plc_connections with the widened CHECK.
CREATE TABLE plc_connections_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE,
    protocol TEXT NOT NULL DEFAULT 'modbus-tcp' CHECK (protocol IN ('modbus-tcp', 'slmp', 'virtual')),
    host TEXT NOT NULL,
    port INTEGER NOT NULL,
    unit_id INTEGER NOT NULL DEFAULT 1,
    enabled INTEGER NOT NULL DEFAULT 1
);

INSERT INTO plc_connections_new (id, name, protocol, host, port, unit_id, enabled)
SELECT id, name, protocol, host, port, unit_id, enabled FROM plc_connections;

DROP TABLE plc_connections;
ALTER TABLE plc_connections_new RENAME TO plc_connections;

-- Put the descendants back, shallowest first - full current column shape
-- (post-0005/0006), not 0004's narrower one.
INSERT INTO collection_groups (id, name, plc_connection_id, period_ms, enabled)
SELECT id, name, plc_connection_id, period_ms, enabled FROM _m0007_collection_groups;
INSERT INTO tags (
    id, name, collection_group_id, address, data_type, string_length,
    raw_lo, raw_hi, eng_lo, eng_hi, unit, decimals,
    threshold_h, threshold_hh, threshold_l, threshold_ll, enabled,
    writable, tag_kind, expression, retain
)
SELECT
    id, name, collection_group_id, address, data_type, string_length,
    raw_lo, raw_hi, eng_lo, eng_hi, unit, decimals,
    threshold_h, threshold_hh, threshold_l, threshold_ll, enabled,
    writable, tag_kind, expression, retain
FROM _m0007_tags;

DROP TABLE _m0007_tags;
DROP TABLE _m0007_collection_groups;
