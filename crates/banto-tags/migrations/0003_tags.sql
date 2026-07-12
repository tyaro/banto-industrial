-- Tag: one collection point (recorder-requirements.md §2 "用語" - name +
-- PLC address + data type + scaling + unit + decimals). `address` is
-- protocol-dependent free text (v1 assumes Modbus-style "40001" etc.) -
-- format validation is deliberately deferred to I2 (the PLC client crate
-- that actually knows the protocol's addressing rules); this crate only
-- enforces non-empty.
--
-- Scaling (raw_lo/raw_hi/eng_lo/eng_hi) is all-NULL ("no scaling") or
-- all-set - enforced by banto_tags::scaling::Scaling::from_parts at the
-- application layer, not by a SQL CHECK (SQLite CHECK constraints can
-- express "all or none" but the resulting error would not carry a
-- human-readable field-level message the way BantoError::Validation does).
--
-- Deleting a collection_groups row that still has tags is rejected at the
-- application layer (banto_tags::collection_group::CollectionGroupService::delete)
-- before reaching SQL; ON DELETE RESTRICT here is the same defense-in-depth
-- backstop as 0002's plc_connection_id FK.
CREATE TABLE tags (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE,
    collection_group_id INTEGER NOT NULL REFERENCES collection_groups(id) ON DELETE RESTRICT,
    address TEXT NOT NULL,
    data_type TEXT NOT NULL CHECK (data_type IN ('bit', 'i16', 'u16', 'i32', 'u32', 'f32')),
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

CREATE INDEX idx_tags_collection_group_id ON tags (collection_group_id);
