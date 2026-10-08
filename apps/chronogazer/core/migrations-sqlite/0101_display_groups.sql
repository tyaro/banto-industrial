-- #393（#524 の段階 1）: 表示グループ（recorder-requirements.md §2・§3.2・
-- §3.7.3 (1)）。収集グループ（banto-tags の `collection_groups`、PLC 一括読み出し
-- の単位）とは別物で、表示の単位。ChronoGazer 固有のテーブル。
--
-- 番号について: 0002〜0007 は banto の admin-template の migration を byte 等価で
-- コピーしたもの（src/db.rs のモジュール doc）。上流が将来 0008 以降を足したとき
-- 同じ番号を取り合わないよう、この app 固有の migration は 0101 から振る
-- （banto-hub 固有のテーブルと同じ決まり。plan.md §5 の 2026-10-02 の決定）。
--
-- 検証（上限 16 グループ・8 ペン、表示種別ごとの属性の閉じた集合、タグの存在、
-- 名前の重複）は `src/display_groups.rs` の `validate_display_group` 1 か所で行う。
-- ここの CHECK / UNIQUE はその取りこぼしを防ぐ最後の砦。
CREATE TABLE display_groups (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  name TEXT NOT NULL UNIQUE,
  sort_order INTEGER NOT NULL DEFAULT 0,
  -- 4 種固定（§3.2）。種別はデータで増やせない（§3.7.7）。
  kind TEXT NOT NULL CHECK (kind IN ('trend', 'digital', 'bar', 'gauge')),
  -- 種別ごとの表示属性（JSON。閉じた項目の集合で、検証済みの形だけを書く）。
  attributes TEXT NOT NULL DEFAULT '{}',
  -- 楽観ロック用の版。作成で 1、更新のたびに +1。
  revision INTEGER NOT NULL DEFAULT 1,
  created_at TEXT NOT NULL DEFAULT (datetime('now')),
  updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX idx_display_groups_sort ON display_groups(sort_order, id);

-- ペン: グループ内のタグ参照 + 表示属性（§2）。1 グループ最大 8（position 0..7）。
--
-- `tag_id` は banto-tags の `tags(id)` を指すが、**FK は張らない**。banto-tags の
-- migration は `tags` を作り直す（DROP / RENAME）ことがあり、この app のテーブル
-- から RESTRICT の FK を張ると、ペンが 1 本でもある DB で上流の migration が
-- 落ちるため。参照されているタグの削除の拒否（§3.7.9 の 8）は
-- `display_groups::DisplayGroupService::delete_tag_unless_referenced` が
-- `BEGIN IMMEDIATE` の中で「参照元の確認 → 削除」を行って担保する。
CREATE TABLE display_group_pens (
  group_id INTEGER NOT NULL REFERENCES display_groups(id) ON DELETE CASCADE,
  position INTEGER NOT NULL CHECK (position BETWEEN 0 AND 7),
  tag_id INTEGER NOT NULL,
  -- banto チャートの系列色の枠（1..8、`--banto-chart-N`）。NULL = 既定
  -- （ペンの位置 + 1 の枠）。
  color_slot INTEGER CHECK (color_slot IS NULL OR color_slot BETWEEN 1 AND 8),
  PRIMARY KEY (group_id, position),
  UNIQUE (group_id, tag_id)
);

CREATE INDEX idx_display_group_pens_tag ON display_group_pens(tag_id);
