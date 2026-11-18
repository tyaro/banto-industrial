-- Optimistic locking for users (Issue #651, follow-up of #638): a row version
-- the account update and delete paths compare against the one the client last
-- read. Every existing row starts at 1; each display-name/role update bumps it
-- by one. Password changes and admin resets only move `auth_epoch` and leave
-- the version alone. A request without a version keeps the old
-- last-write-wins behavior.
ALTER TABLE users ADD COLUMN version INTEGER NOT NULL DEFAULT 1;
