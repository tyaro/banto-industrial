-- v8.0.0 baseline (ADR-0034 §2/§9, plan §4.1/§4.4): per-owner UI settings
-- and the built-in `admin` row.
--
-- `ui_settings.owner` names who the value belongs to: `acct:<users.id>` for
-- an account, `grant:<kind>` for a grant session, `local` for the desktop's
-- no-login session. Account rows also carry `user_id` so the foreign key
-- removes them with the account (and refuses a write that arrives after the
-- delete); the CHECK keeps the two columns from disagreeing.
-- `revision` is a random string the service layer writes on every change
-- (not a counter: a restored backup must not make an old revision match).
CREATE TABLE ui_settings (
    owner      TEXT NOT NULL,
    key        TEXT NOT NULL,
    value      TEXT NOT NULL,
    revision   TEXT NOT NULL,
    user_id    INTEGER REFERENCES users(id) ON DELETE CASCADE,
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (owner, key),
    CONSTRAINT ui_settings_owner_matches_user CHECK (
        (user_id IS NULL AND owner NOT LIKE 'acct:%')
        OR (user_id IS NOT NULL AND owner = 'acct:' || CAST(user_id AS TEXT))
    )
);

CREATE INDEX idx_ui_settings_user_id ON ui_settings (user_id);

-- The built-in `admin` (ADR-0034 §2): no password, no account_uid, never
-- logs in. Inserted here, after 0004/0007/0012 added role/auth_epoch/version.
-- Exactly one such row exists (0003's partial unique index).
INSERT INTO users (username, display_name, role, kind) VALUES ('admin', 'admin', 'admin', 'builtin');
