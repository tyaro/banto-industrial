-- tyaro/banto-industrial#437 (ADR-0019): an audit write that cannot reach
-- the database (an error or a hang past the timeout) is spooled to a file
-- and flushed into this table later. Every write that goes through the
-- spool carries the same `pending_id` (a UUIDv4 generated when the entry was
-- recorded) on both the original INSERT and the flush, and both use
-- `ON CONFLICT (pending_id) DO NOTHING`, so an INSERT that completed after
-- its timeout and the later flush leave exactly one row.
--
-- NULL for every row written without the spool (`try_record`, `record`
-- without a spool, and every row written before this migration). NULLs
-- never conflict with each other in a UNIQUE index (SQLite and PostgreSQL).
ALTER TABLE audit_log ADD COLUMN pending_id TEXT;

CREATE UNIQUE INDEX idx_audit_log_pending_id ON audit_log(pending_id);
