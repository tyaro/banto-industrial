-- TAG-P0-3（2026-08-11 方針改定）: 運転中編集の pending queue。`payload` は
-- 提案変更の JSON 文字列。`base_fingerprint` は適用時のステイル検出用の
-- スナップショット（`crate::rest::compute_pending_base_fingerprint` 参照。
-- `plc_connections`/`collection_groups` の update/delete だけが値を持つ）。
CREATE TABLE pending_changes (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  state TEXT NOT NULL CHECK (state IN ('pending','applying','applied','canceled','failed')),
  source TEXT NOT NULL,
  payload TEXT NOT NULL,
  base_configured_revision INTEGER NOT NULL,
  requested_by_username TEXT,
  requested_by_role TEXT,
  failure_reason TEXT,
  created_at TEXT NOT NULL DEFAULT (datetime('now')),
  updated_at TEXT NOT NULL DEFAULT (datetime('now')),
  base_fingerprint TEXT
);

CREATE INDEX idx_pending_changes_state_created_at ON pending_changes(state, created_at);
CREATE INDEX idx_pending_changes_created_at ON pending_changes(created_at);
