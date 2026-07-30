-- Write target: one PLC device this app may write a value to (plan
-- `luminous-discovering-goblet.md`, W1 - symmetric with banto-tags'
-- `tags` table, crates/banto-tags/migrations/0003_tags.sql). A target may
-- point at a DIFFERENT plc_connection than the rule's source tag(s) -
-- reading and writing PLCs are not required to be the same device.
--
-- `plc_connection_id` is a plain unconstrained INTEGER, NOT a SQL FOREIGN
-- KEY: `plc_connections` is owned by banto-tags' own migration lineage
-- (crates/banto-tags/migrations/0001_plc_connections.sql), applied via its
-- own `sqlx::migrate!` against this same database (see core/src/db.rs's
-- module doc comment). A real FOREIGN KEY across two independent
-- `sqlx::migrate!` sources is fine at the SQL level, but validating the
-- referenced id is left to the application layer (W2 CRUD), the same
-- precedent `banto-collect`'s `collect_events.connection_key` set for
-- referencing a banto-tags-owned row from a different app/crate's own
-- table set - see crates/banto-collect/migrations/0001_collect_events.sql.
--
-- Scaling (raw_lo/raw_hi/eng_lo/eng_hi) and column naming mirror
-- banto-tags' `tags` table field-for-field (see that migration's own doc
-- comment for the "all-NULL or all-set" convention, enforced at the
-- application layer in W2, not by a SQL CHECK here either).
CREATE TABLE IF NOT EXISTS write_targets (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE,
    plc_connection_id INTEGER NOT NULL,
    address TEXT NOT NULL,
    data_type TEXT NOT NULL CHECK (data_type IN ('bit', 'i16', 'u16', 'i32', 'u32', 'f32')),
    raw_lo REAL,
    raw_hi REAL,
    eng_lo REAL,
    eng_hi REAL,
    unit TEXT,
    decimals INTEGER NOT NULL DEFAULT 0 CHECK (decimals BETWEEN 0 AND 6),
    enabled INTEGER NOT NULL DEFAULT 1
);

CREATE INDEX IF NOT EXISTS idx_write_targets_plc_connection_id ON write_targets (plc_connection_id);
