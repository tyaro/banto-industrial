-- Extend `tags` with 4 columns in a single migration (docs/tag-server-design.md
-- §10-2, decided 2026-08-04): `writable` (per-tag write opt-in, §6 item 1) and
-- `tag_kind` / `expression` / `retain` (tag species - plc/computed/internal,
-- §4.2, needed starting T6). All four ship together even though
-- `tag_kind`/`expression`/`retain` stay unused until T6 - the owner's
-- rationale (§10-2) is that splitting this into a T2 migration and a later T6
-- migration would gain nothing: both are additive, default-valued
-- `ADD COLUMN`s (backward compatible - ChronoGazer / relay-wright pick them
-- up transparently the next time they call `banto_tags::migrate` at startup,
-- same as every prior migration in this crate), so there is no forward-compat
-- reason to defer the T6 columns, and doing all four now avoids a second
-- SQLite migration against the same table later.
--
-- Unlike migrations 0004/0005, this one is a plain `ADD COLUMN` set - no
-- table rebuild needed. `tags` already has no `CHECK` that needs re-forming,
-- and SQLite's `ALTER TABLE ... ADD COLUMN` accepts a `CHECK` constraint on
-- the new column itself as long as the column's own default value satisfies
-- it (true below: `tag_kind`'s default `'plc'` is in its own allow-list).
--
-- `tag_kind`'s CHECK constraint lists the FULL vocabulary from design §4.2
-- (`'plc' | 'computed' | 'internal'`) even though the application-layer
-- validation (`banto_tags::tag::validate_tag_input`) accepts only `'plc'`
-- until T6 (design §6 item 9, 2026-08-05 decision: "tag_kind は T2 時点で
-- plc のみ受理し、computed/internal の受理は T6 で解禁") - the SQL CHECK's
-- job is defense-in-depth against a value outside the whole future
-- vocabulary, not a stand-in for the narrower T2-scoped application rule
-- (same division of labor as this crate's other CHECK constraints: SQL is
-- the coarse backstop, `validate_tag_input` carries the field-level message).
--
-- `expression` / `retain` get no CHECK: `expression` is free-form text (the
-- expression-language grammar, §10-12, is validated at the application layer
-- once T6 defines it - this column is just a NULL-able TEXT payload until
-- then), and `retain` is a plain boolean flag like `enabled`/`writable`
-- (this crate's existing boolean columns carry no CHECK either).

ALTER TABLE tags ADD COLUMN writable INTEGER NOT NULL DEFAULT 0;
ALTER TABLE tags ADD COLUMN tag_kind TEXT NOT NULL DEFAULT 'plc'
    CHECK (tag_kind IN ('plc', 'computed', 'internal'));
ALTER TABLE tags ADD COLUMN expression TEXT;
ALTER TABLE tags ADD COLUMN retain INTEGER NOT NULL DEFAULT 0;
