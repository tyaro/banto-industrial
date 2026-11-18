-- T2-4 (docs/tag-server-design.md §6-6): 書き込み受付フラグの永続値
-- （`crate::write_control` のモジュール doc 参照）。id=1 の単一行。
-- 2026-09-09 オーナー決定 (#340) で既定は「書き込み可」なので seed は
-- `enabled_persisted = 1`。起動時にライブフラグへそのまま復元される。
CREATE TABLE write_control_state (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  enabled_persisted INTEGER NOT NULL DEFAULT 1,
  last_changed_at TEXT,
  last_changed_by TEXT
);

INSERT INTO write_control_state (id, enabled_persisted) VALUES (1, 1);
