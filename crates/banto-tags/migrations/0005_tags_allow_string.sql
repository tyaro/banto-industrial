-- Widen tags.data_type's CHECK to accept 'string' (MELSEC string devices, S1 of
-- the relay-wright 文字列タグ plan) and add the companion string_length column:
-- the number of consecutive 16-bit word devices the string occupies (SJIS
-- capacity = 2 bytes per word). NULL for every non-string tag; 1..=128 for
-- string tags - the cross-column "required exactly when data_type='string'"
-- rule is enforced at the application layer (banto_tags::tag::validate_tag_input),
-- same reasoning as 0003's scaling columns: a SQL CHECK could express it but
-- its error would not carry a field-level message the way
-- BantoError::Validation does. The range CHECK below is defense-in-depth only,
-- mirroring 0003's decimals CHECK.
--
-- SQLite cannot ALTER a CHECK constraint, so the table has to be rebuilt -
-- same fundamental dance as 0004 (see that file's header for the three
-- empirically-established constraints: this runs inside a transaction with
-- foreign keys ENFORCED, DROP on a referenced table fails, and renaming a
-- referenced table drags children's foreign keys along). But `tags` is a LEAF
-- table - nothing references it - so none of 0004's park-and-restore is needed
-- here:
--
-- - `DROP TABLE tags` performs an implicit `DELETE FROM tags`, which only
--   deletes *child* rows of collection_groups; ON DELETE RESTRICT restricts
--   deleting the parent, never the child, so it passes with rows present.
-- - `ALTER TABLE tags_new RENAME TO tags` rewrites references to `tags_new`
--   in other tables' schemas - there are none - and keeps tags_new's own
--   REFERENCES collection_groups(id) clause intact.
--
-- If a future table ever references `tags`, this migration stays valid (it
-- has already run) but its *pattern* stops being reusable - the next rebuild
-- would need 0004's full parking dance.
--
-- Same accepted behaviour change as 0004: the rebuilt table's AUTOINCREMENT
-- high-water mark is re-seeded from the copied rows' maximum id, so an id
-- freed by an earlier DELETE can be reused after this migration. Harmless for
-- the same reason.
--
-- Verified end to end against a populated database, run the way sqlx itself
-- runs it (one connection, one transaction), by
-- `migration_0005_preserves_rows_and_foreign_keys_on_a_populated_database` in
-- src/tag.rs.

CREATE TABLE tags_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE,
    collection_group_id INTEGER NOT NULL REFERENCES collection_groups(id) ON DELETE RESTRICT,
    address TEXT NOT NULL,
    data_type TEXT NOT NULL CHECK (data_type IN ('bit', 'i16', 'u16', 'i32', 'u32', 'f32', 'string')),
    string_length INTEGER CHECK (string_length IS NULL OR string_length BETWEEN 1 AND 128),
    raw_lo REAL,
    raw_hi REAL,
    eng_lo REAL,
    eng_hi REAL,
    unit TEXT,
    decimals INTEGER NOT NULL DEFAULT 0 CHECK (decimals BETWEEN 0 AND 6),
    threshold_h REAL,
    threshold_hh REAL,
    threshold_l REAL,
    threshold_ll REAL,
    enabled INTEGER NOT NULL DEFAULT 1
);

INSERT INTO tags_new (
    id, name, collection_group_id, address, data_type,
    raw_lo, raw_hi, eng_lo, eng_hi, unit, decimals,
    threshold_h, threshold_hh, threshold_l, threshold_ll, enabled
)
SELECT
    id, name, collection_group_id, address, data_type,
    raw_lo, raw_hi, eng_lo, eng_hi, unit, decimals,
    threshold_h, threshold_hh, threshold_l, threshold_ll, enabled
FROM tags;

DROP TABLE tags;
ALTER TABLE tags_new RENAME TO tags;

-- 0003's index does not survive the rebuild (indexes belong to the dropped
-- table), so it is recreated here under its original name.
CREATE INDEX idx_tags_collection_group_id ON tags (collection_group_id);
