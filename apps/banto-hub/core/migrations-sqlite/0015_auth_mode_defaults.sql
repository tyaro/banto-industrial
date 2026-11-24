-- The auth mode of a new database, written explicitly (ADR-0034 §5, and the
-- 2026-11-22 addendum that moved these rows out of 0014): no login, with the
-- no-login session's role `admin`. The app never fills these two keys with a
-- default when it reads them - a database without them does not start - so
-- they are inserted here, once.
--
-- Rows that are already there are kept (ON CONFLICT DO NOTHING): a migration
-- never overrides a setting someone saved. A database that already holds
-- accounts and has no `auth.disabled` yet was created while a missing key
-- still meant "login required", so it stays login-required; only a database
-- without accounts starts in no-login mode. (`WHERE true` is what SQLite needs
-- to tell this ON CONFLICT from a join's ON.)
INSERT INTO settings (key, value)
SELECT 'auth.disabled',
       CASE WHEN EXISTS (SELECT 1 FROM users WHERE kind = 'account') THEN 'false' ELSE 'true' END
WHERE true
ON CONFLICT(key) DO NOTHING;

INSERT INTO settings (key, value) VALUES ('auth.disabled_role', 'admin')
ON CONFLICT(key) DO NOTHING;
