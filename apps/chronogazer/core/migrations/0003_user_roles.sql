-- M10: RBAC roles for the `users` table (spec docs/roadmap.md M10).
-- Existing accounts (i.e. whatever first-run `setup_first_user` created)
-- default to 'admin' so nobody is locked out of their own instance by this
-- migration.
ALTER TABLE users ADD COLUMN role TEXT NOT NULL DEFAULT 'admin' CHECK (role IN ('admin','editor','viewer'));
