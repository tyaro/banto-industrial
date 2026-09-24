-- banto v1.7.0 #204: per-account "authentication epoch" for session
-- revocation. Every session records the (account id, auth_epoch) pair it was
-- issued under, and each request/command re-reads this row: a missing row
-- (the account was deleted) or a different epoch ends the session. The epoch
-- is incremented by a role change, a password change and an admin password
-- reset (`crate::users::UsersService`). Existing accounts start at 0;
-- sessions live only in process memory, so no issued session predates this
-- column. Documentation only - applied idempotently by
-- `crate::db::apply_app_schema` (see that module's doc comment).
ALTER TABLE users ADD COLUMN auth_epoch INTEGER NOT NULL DEFAULT 0;
