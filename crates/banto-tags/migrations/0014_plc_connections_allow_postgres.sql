-- 外部 DB 連携 S1a（docs/banto-hub-external-db-design.md §4.1・§6-4、
-- 2026-09-06 オーナー決定「案A: 既存 3 階層の流用」）: plc_connections.protocol
-- の CHECK に 'postgres' を追加し、DB 接続専用の資格情報列
-- (`database`/`username`/`password`) を新設する。
--
-- 資格情報の平文保存は§2.2「v1 は MQTT と同じ平文（settings テーブルの
-- MQTT パスワードと同じ前提）」の決定に従う - 暗号化・OS keyring は別 issue。
-- 3列とも NULL 許容: 'modbus-tcp'/'slmp'/'virtual' の既存行は全て NULL の
-- ままで良く（Rust 側の validate_plc_connection_input が「postgres 以外は
-- 3列とも None/空でなければならない」を強制する - `plc_connection.rs`
-- 参照）、'postgres' 行は database/username が必須（application 層の検証、
-- 同上）で password は任意（後から設定する運用を許す）。
--
-- host/port は流用 - 'postgres' 接続でも実在するホスト/ポートを指す
-- （§4.1「host/port は流用」）。unit_id/word_order/simulation も列としては
-- 流用するが、'postgres' 接続では常に既定値を書き込み、意味を持たない
-- （application 層で正規化 - `unit_id` が SLMP で無意味なまま必須列に
-- なっているのと同じ扱い、`plc_connection.rs` モジュールコメント参照）。
--
-- SQLite cannot ALTER a CHECK constraint, so the table has to be rebuilt -
-- same fundamental dance as 0004/0007 (see 0004's header for the three
-- empirically-established constraints this must respect: runs inside a
-- transaction with foreign keys ENFORCED, DROP on a referenced table fails,
-- and renaming a referenced table drags children's foreign keys along).
-- `tags` now carries more columns than it did at 0007 time (revision from
-- 0009, the tags_new rebuild + UNIQUE(collection_group_id, name) from 0011,
-- string_encoding from 0013) and `collection_groups` carries
-- `default_writable` (0012) - the park-and-restore below copies the FULL
-- current shape of both, not 0007's narrower one - copying a stale column
-- list here would silently truncate every existing row.
--
-- Verified end to end against a populated database (mirroring 0004/0007's
-- own tests) by
-- `migration_0014_preserves_rows_and_foreign_keys_on_a_populated_database`
-- in `src/plc_connection.rs`.

-- Park the descendants, deepest first.
CREATE TEMPORARY TABLE _m0014_tags AS SELECT * FROM tags;
CREATE TEMPORARY TABLE _m0014_collection_groups AS SELECT * FROM collection_groups;
DELETE FROM tags;
DELETE FROM collection_groups;

-- Rebuild plc_connections with the widened CHECK and the new credential
-- columns.
CREATE TABLE plc_connections_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE,
    protocol TEXT NOT NULL DEFAULT 'modbus-tcp'
        CHECK (protocol IN ('modbus-tcp', 'slmp', 'virtual', 'postgres')),
    host TEXT NOT NULL,
    port INTEGER NOT NULL,
    unit_id INTEGER NOT NULL DEFAULT 1,
    enabled INTEGER NOT NULL DEFAULT 1,
    simulation INTEGER NOT NULL DEFAULT 0,
    word_order TEXT NOT NULL DEFAULT 'low_high' CHECK (word_order IN ('low_high', 'high_low')),
    -- S1（本マイグレーションの主目的）: 'postgres' 接続専用の資格情報。
    -- 3列とも NULL 許容 - 'postgres' 以外の行は常に NULL、'postgres' 行の
    -- 検証（database/username 必須・password 任意）は application 層
    -- （`validate_plc_connection_input`）が担う。
    database TEXT,
    username TEXT,
    password TEXT
);

INSERT INTO plc_connections_new (
    id, name, protocol, host, port, unit_id, enabled, simulation, word_order
)
SELECT
    id, name, protocol, host, port, unit_id, enabled, simulation, word_order
FROM plc_connections;

DROP TABLE plc_connections;
ALTER TABLE plc_connections_new RENAME TO plc_connections;

-- Put the descendants back, shallowest first - full current column shape
-- (post-0012 collection_groups, post-0013 tags).
INSERT INTO collection_groups (id, name, plc_connection_id, period_ms, enabled, default_writable)
SELECT id, name, plc_connection_id, period_ms, enabled, default_writable
FROM _m0014_collection_groups;
INSERT INTO tags (
    id, name, collection_group_id, address, data_type, string_length,
    raw_lo, raw_hi, eng_lo, eng_hi, unit, decimals,
    threshold_h, threshold_hh, threshold_l, threshold_ll, enabled,
    writable, tag_kind, expression, retain, revision, string_encoding
)
SELECT
    id, name, collection_group_id, address, data_type, string_length,
    raw_lo, raw_hi, eng_lo, eng_hi, unit, decimals,
    threshold_h, threshold_hh, threshold_l, threshold_ll, enabled,
    writable, tag_kind, expression, retain, revision, string_encoding
FROM _m0014_tags;

DROP TABLE _m0014_tags;
DROP TABLE _m0014_collection_groups;

-- 0002/0007 由来の索引はテーブル再構築で失われない（今回 DROP されるのは
-- plc_connections のみで、collection_groups/tags 自体は DROP されない
-- ため）が、念のため plc_connections を直接参照する索引がここまで
-- 存在しないことを確認済み（`idx_collection_groups_plc_connection_id` は
-- collection_groups 側の索引で、collection_groups を DROP していないため
-- 無傷で残る）。
