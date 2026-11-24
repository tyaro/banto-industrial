-- M14: audit log (spec docs/roadmap.md M14). No FK to `users` - actor is
-- recorded as a username SNAPSHOT (`actor_username`) so entries remain
-- readable after the account that made them is deleted (spec: "ユーザー
-- 削除後も読める").
CREATE TABLE audit_log (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  ts TEXT NOT NULL DEFAULT (datetime('now')),
  actor_username TEXT,          -- NULL = unauthenticated (e.g. a login failure before any session exists)
  actor_role TEXT,               -- role AT THE TIME of the action, not looked up later
  action TEXT NOT NULL,          -- 'create'|'update'|'delete'|'login'|'login_failed'|'logout'|'setup'|'password_reset'|'settings_change'|'denied' etc.
  resource TEXT NOT NULL,        -- 'items'|'users'|'settings'|'auth' etc.
  entity_id TEXT,                -- target id as text, NULL if not applicable
  detail TEXT,                   -- JSON summary (changed field names, new role, etc.) - never full field values
  origin TEXT NOT NULL,           -- 'rest'|'tauri'
  result TEXT NOT NULL DEFAULT 'ok' -- 'ok'|'denied'|'failed'
);

CREATE INDEX idx_audit_log_ts ON audit_log(ts);
CREATE INDEX idx_audit_log_actor ON audit_log(actor_username);
CREATE INDEX idx_audit_log_resource ON audit_log(resource, entity_id);
