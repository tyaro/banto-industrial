-- T9-1 (docs/ux-plan.md §1, 2026-08-06 オーナー決定「接続単位のシミュレーション
-- モード」): add `plc_connections.simulation` - per-connection opt-in to run
-- against an in-process simulator (banto-collect) instead of the real
-- host/port, independent of `protocol` (owner's explicit rejection of a
-- `protocol = 'simulation'` alternative - see this crate's
-- `plc_connection.rs` module doc for the full rationale: "開発→実機の切り替え
-- がチェックボックス1つ" must not require recreating the connection row).
--
-- Plain `ADD COLUMN`, no table rebuild - unlike 0004/0007 (which had to widen
-- `protocol`'s CHECK, something SQLite cannot ALTER), `simulation` needs no
-- CHECK of its own (a boolean flag like the existing `enabled` column, which
-- also carries none). Default `0` (false) is backward compatible: every
-- existing row - and every ChronoGazer/relay-wright database that has never
-- heard of simulation mode - picks this up as "not simulated", i.e. no
-- behavior change, the next time `banto_tags::migrate` runs at startup (same
-- pattern as every prior additive migration in this crate).

ALTER TABLE plc_connections ADD COLUMN simulation INTEGER NOT NULL DEFAULT 0;
