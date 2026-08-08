-- Persisted collection events (recorder-requirements.md §3.5 "イベント・出力":
-- しきい値の超過/復帰・PLC断/復旧・収集開始/停止 を時刻付きで記録). This is
-- the durable half of the two-output event story (the other is the live
-- tokio::broadcast channel for the UI); a later Events screen (§6) reads
-- this table back.
--
-- Applied via banto_collect::migrate with `CREATE TABLE IF NOT EXISTS`
-- (idempotent, hand-run) rather than sqlx::migrate!: the ChronoGazer app
-- shares one SQLite database across banto-tags' migrator and this crate, and
-- a second independent sqlx migrator would collide on the shared
-- `_sqlx_migrations` table. See lib.rs's `migrate` doc comment.
CREATE TABLE IF NOT EXISTS collect_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    -- UTC epoch milliseconds (the collection PC's clock, per §4 "時刻").
    ts INTEGER NOT NULL,
    -- Event kind, snake_case: collection_started | collection_stopped |
    -- plc_connected | plc_disconnected | plc_reconnected | threshold_entered
    -- | threshold_cleared | clock_regression_entered |
    -- clock_regression_cleared | append_failure_entered |
    -- append_failure_cleared. No CHECK constraint deliberately - see
    -- `EventKind::as_str` (src/event.rs) for the authoritative Rust-side
    -- vocabulary; the last four kinds were added for H4 (2026-08-08 owner
    -- decision, docs/improvement-plan.md) without a schema change (this
    -- column was already an unconstrained TEXT NOT NULL).
    kind TEXT NOT NULL,
    -- Which connection (stable "conn:<id>" key), NULL for the collector-wide
    -- collection_started/collection_stopped events.
    connection_key TEXT,
    -- Which tag (stable "tag:<id>" key), set only for threshold_* events.
    tag_key TEXT,
    -- Threshold level for threshold_* events: 'H' | 'HH' | 'L' | 'LL'.
    level TEXT,
    -- The scaled value that crossed the threshold (threshold_* events only).
    value REAL,
    -- Free-text detail, e.g. a plc_disconnected reason string.
    detail TEXT
);

CREATE INDEX IF NOT EXISTS idx_collect_events_ts ON collect_events (ts);
