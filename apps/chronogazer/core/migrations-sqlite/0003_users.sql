-- v8.0.0 baseline rebuild (ADR-0034 §2/§9, conventions §11's one-time
-- exception): this file was rewritten in place. A database created by an
-- earlier version is refused at startup (sqlx checksum mismatch, see
-- `db::run_migrations`); there is no data migration.
--
-- `kind`: 'account' (a person; logs in) or 'builtin' (the one `admin` row
--   that 0014 inserts; never logs in). The CHECKs tie the shape to the kind:
--   a builtin row has neither a password hash nor an `account_uid`, an
--   account row has both - an INSERT that forgets `account_uid` fails instead
--   of leaving NULL behind. The partial unique index allows one builtin row.
-- `account_uid`: 128-bit random string the service layer generates when the
--   account is created (ADR-0034 §1). Never reused, not a secret.
CREATE TABLE users (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    username TEXT NOT NULL UNIQUE,
    password_hash TEXT,
    display_name TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    kind TEXT NOT NULL DEFAULT 'account' CHECK (kind IN ('account', 'builtin')),
    account_uid TEXT UNIQUE,
    CONSTRAINT users_builtin_has_no_password CHECK ((kind = 'builtin') = (password_hash IS NULL)),
    CONSTRAINT users_builtin_has_no_account_uid CHECK ((kind = 'builtin') = (account_uid IS NULL))
);

CREATE UNIQUE INDEX idx_users_single_builtin ON users(kind) WHERE kind = 'builtin';
