-- PLC connection endpoint (recorder-requirements.md §1 "対象環境").
-- v1 only ever writes protocol = 'modbus-tcp' (chosen first for
-- debuggability, plan.md §3 I2 decision); 'protocol' is a TEXT + CHECK
-- rather than a fixed enum column so adding 'slmp' (MELSEC MC protocol,
-- the eventual primary target) later only needs a migration.
CREATE TABLE plc_connections (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE,
    protocol TEXT NOT NULL DEFAULT 'modbus-tcp' CHECK (protocol IN ('modbus-tcp')),
    host TEXT NOT NULL,
    port INTEGER NOT NULL,
    -- Modbus slave/unit id. Default 1 per docs/plan.md I1 spec.
    unit_id INTEGER NOT NULL DEFAULT 1,
    enabled INTEGER NOT NULL DEFAULT 1
);
