-- T6-2 (docs/tag-server-design.md §4.2「retain フラグで再起動時の最終値
-- 復元」): `retain = true` の内部タグの最終値。`tag_id` を主キーにする
-- （`crate::computed::ServerTagStore` のキー `"tag:{id}"` と同じ id）。
-- `tags(id)` への FOREIGN KEY は張らない: `tags` は banto-tags の
-- マイグレーションが作り、この app のマイグレーションより後に流れる。
-- タグ削除後に残った行は、ロード時に catalog に無ければ無視される。
CREATE TABLE hub_retained_values (
  tag_id INTEGER PRIMARY KEY,
  value REAL NOT NULL,
  ptime_ms INTEGER NOT NULL
);
