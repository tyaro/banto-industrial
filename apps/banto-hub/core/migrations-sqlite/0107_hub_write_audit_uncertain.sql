-- #898（2026-10-28 オーナー決定）: `hub_write_audit.result` に `uncertain`
-- （送ったあとで結果が分からなくなった書込み）を足す。値の意味は
-- `crate::write_audit` のモジュール doc「`failed` と `uncertain` の区別」参照。
--
-- SQLite は CHECK を ALTER で変えられないので、表を作り直して行を写す。
-- 0103 を書き換えないのは、適用済みの DB の `sqlx` の checksum を変えない
-- ため（`crate::db` のモジュール doc「今後の列追加は新しい番号のファイルで
-- 足す」）。`hub_write_audit` を参照する FOREIGN KEY は無いので、単純な
-- 作り直しで足りる。`id` をそのまま写すので、AUTOINCREMENT の続き番号は
-- 写した最大値から続く（RENAME で `sqlite_sequence` の名前も付け替わる）。
-- 作り直し後の `sqlite_sequence` は写した行の MAX(id) から再開するので、末尾で
-- 消した行の id は再利用され得る。監査行を消す処理は無いので今は害が無い。
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
    result IN ('ok', 'failed', 'uncertain', 'suppressed_disabled', 'suppressed_rate_limited')
  ),
  detail TEXT,
  value_requested_text TEXT
);

INSERT INTO hub_write_audit_new (
  id, ts, api_key_id, api_key_name_snapshot, tag_id, external_name_snapshot,
  value_requested, action, result, detail, value_requested_text
)
SELECT
  id, ts, api_key_id, api_key_name_snapshot, tag_id, external_name_snapshot,
  value_requested, action, result, detail, value_requested_text
FROM hub_write_audit;

DROP TABLE hub_write_audit;

ALTER TABLE hub_write_audit_new RENAME TO hub_write_audit;

CREATE INDEX idx_hub_write_audit_ts ON hub_write_audit(ts);
CREATE INDEX idx_hub_write_audit_tag ON hub_write_audit(tag_id);
