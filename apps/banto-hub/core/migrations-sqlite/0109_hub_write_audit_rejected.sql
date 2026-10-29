-- #711（2026-10-29 オーナー決定）: `hub_write_audit.result` に `rejected`
-- （書込み不可・catalog に無い・スコープ外・タグ無効の定義上の拒否）を足す。
-- 値の意味は `crate::write_audit` のモジュール doc「定義上の拒否」参照。
--
-- 0107 と同じ手順で、CHECK を変えるため表を作り直して行を写す。0108 で足した
-- `api_key_prefix_snapshot` も写す。`tag_id` は NOT NULL のままで、catalog に
-- 無いタグの拒否は 0 を入れる（要求された名前は `external_name_snapshot`）。
CREATE TABLE hub_write_audit_new (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  ts TEXT NOT NULL DEFAULT (datetime('now')),
  api_key_id INTEGER NOT NULL,
  api_key_name_snapshot TEXT NOT NULL,
  tag_id INTEGER NOT NULL,
  external_name_snapshot TEXT NOT NULL,
  value_requested REAL,
  action TEXT NOT NULL CHECK (action IN ('write', 'rate_limit_tripped')),
  result TEXT NOT NULL CHECK (
    result IN ('ok', 'failed', 'uncertain', 'suppressed_disabled', 'suppressed_rate_limited', 'rejected')
  ),
  detail TEXT,
  value_requested_text TEXT,
  api_key_prefix_snapshot TEXT
);

INSERT INTO hub_write_audit_new (
  id, ts, api_key_id, api_key_name_snapshot, tag_id, external_name_snapshot,
  value_requested, action, result, detail, value_requested_text,
  api_key_prefix_snapshot
)
SELECT
  id, ts, api_key_id, api_key_name_snapshot, tag_id, external_name_snapshot,
  value_requested, action, result, detail, value_requested_text,
  api_key_prefix_snapshot
FROM hub_write_audit;

DROP TABLE hub_write_audit;

ALTER TABLE hub_write_audit_new RENAME TO hub_write_audit;

CREATE INDEX idx_hub_write_audit_ts ON hub_write_audit(ts);
CREATE INDEX idx_hub_write_audit_tag ON hub_write_audit(tag_id);
