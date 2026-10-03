-- T2-4 (docs/tag-server-design.md §6-3「log-before-write」): 書き込み監査
-- ログ。列の意味・2 段挿入のパターンは `crate::write_audit` のモジュール
-- doc 参照。`action`/`result` の許容値は同モジュールの
-- `WriteAuditAction`/`WriteAuditResult` と一致させる。
-- `value_requested_text` は T20-HW1: `value_requested` は REAL 専用なので、
-- 文字列書き込みの値をこちらに残す（以前は後追いの ADD COLUMN）。
CREATE TABLE hub_write_audit (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  ts TEXT NOT NULL DEFAULT (datetime('now')),
  api_key_id INTEGER NOT NULL,
  api_key_name_snapshot TEXT NOT NULL,
  tag_id INTEGER NOT NULL,
  external_name_snapshot TEXT NOT NULL,
  value_requested REAL,
  action TEXT NOT NULL CHECK (action IN ('write', 'rate_limit_tripped')),
  result TEXT NOT NULL CHECK (
    result IN ('ok', 'failed', 'suppressed_disabled', 'suppressed_rate_limited')
  ),
  detail TEXT,
  value_requested_text TEXT
);

CREATE INDEX idx_hub_write_audit_ts ON hub_write_audit(ts);
CREATE INDEX idx_hub_write_audit_tag ON hub_write_audit(tag_id);
