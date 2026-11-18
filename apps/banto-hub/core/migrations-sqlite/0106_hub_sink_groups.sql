-- 外部 DB 連携 S4（docs/banto-hub-external-db-design.md §5.2）: DB Sink の
-- 設定。Hub 専用の設定なので banto-tags のレジストリには乗せず、この app の
-- テーブルとして持つ（名前は他の Hub 専用テーブルと同じ `hub_` 接頭辞。
-- `crate::sink` のモジュール doc 参照）。
--
-- `db_connection_id` は `plc_connections(id)` を、`hub_sink_group_tags.tag_id`
-- は `tags(id)` を指すが FOREIGN KEY は張らない（`hub_retained_values` と
-- 同じ慣行。存在確認・削除拒否・タグ削除時の掃除は `crate::sink::service`
-- が担う）。`sink_group_id` は同じ app のテーブルなので FK（ON DELETE
-- CASCADE）を張る。
CREATE TABLE hub_sink_groups (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  name TEXT NOT NULL UNIQUE,
  db_connection_id INTEGER NOT NULL,
  mode TEXT NOT NULL CHECK (mode IN ('interval', 'on_change')),
  interval_ms INTEGER NOT NULL,
  table_name TEXT NOT NULL,
  store_bad INTEGER NOT NULL DEFAULT 0,
  enabled INTEGER NOT NULL DEFAULT 1,
  created_at TEXT NOT NULL DEFAULT (datetime('now')),
  updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX idx_hub_sink_groups_db_connection_id ON hub_sink_groups(db_connection_id);

CREATE TABLE hub_sink_group_tags (
  sink_group_id INTEGER NOT NULL REFERENCES hub_sink_groups(id) ON DELETE CASCADE,
  tag_id INTEGER NOT NULL,
  PRIMARY KEY (sink_group_id, tag_id)
);

CREATE INDEX idx_hub_sink_group_tags_tag_id ON hub_sink_group_tags(tag_id);
