-- Collection group: the unit of periodic PLC bulk read (recorder-
-- requirements.md §3.1 - "収集周期はタグ毎ではなく収集グループ毎"). Deleting
-- a plc_connections row that still has collection_groups pointing at it is
-- rejected at the application layer with a human-readable count
-- (banto_tags::plc_connection::PlcConnectionService::delete) before it ever
-- reaches SQL; ON DELETE RESTRICT here is a defense-in-depth backstop for
-- callers that bypass the service layer.
CREATE TABLE collection_groups (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE,
    plc_connection_id INTEGER NOT NULL REFERENCES plc_connections(id) ON DELETE RESTRICT,
    -- Selectable periods per recorder-requirements.md §3.1: 100/200/500ms,
    -- 1/2/5/10s, 1min.
    period_ms INTEGER NOT NULL CHECK (period_ms IN (100, 200, 500, 1000, 2000, 5000, 10000, 60000)),
    enabled INTEGER NOT NULL DEFAULT 1
);

CREATE INDEX idx_collection_groups_plc_connection_id ON collection_groups (plc_connection_id);
