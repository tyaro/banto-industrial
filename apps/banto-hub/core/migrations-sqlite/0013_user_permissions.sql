-- Per-user permissions, separate from the role (Issue #697,
-- docs/design/user-permissions-plan.md §4). A row means "this account holds
-- this permission"; which permission keys exist is registered by the app at
-- startup (the permission catalog), not stored here. A key the app no longer
-- registers may stay in this table and is treated as not held.
-- `granted_by` is the username of the admin who granted it (the current
-- state's origin; the audit log keeps the history). Rows go away with the
-- account (ON DELETE CASCADE; the app's pools enable foreign keys).
CREATE TABLE user_permissions (
    user_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    permission TEXT    NOT NULL,
    granted_by TEXT    NOT NULL,
    granted_at TEXT    NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (user_id, permission)
);
CREATE INDEX idx_user_permissions_permission ON user_permissions (permission);
