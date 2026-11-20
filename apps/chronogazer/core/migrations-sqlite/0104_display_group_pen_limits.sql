-- #1207・#1231 PR 5a（docs/chronogazer/trend-design.md §6・§9 Q4）: 1 グループのペンを
-- 8 → 16 本、色の枠を 1..8 → 1..16 に広げる。`0101_display_groups.sql` の CHECK
-- （position 0..7・color_slot 1..8）は SQLite では書き換えられないので、テーブルを
-- 作り直す（0101 は適用済みの DB のチェックサムがあるので触らない）。
-- `display_group_pens` を参照するテーブル・外部キーは無い（`tag_id` に FK を張らない
-- 理由は 0101 の注記）ので、作り直しで巻き込まれるものは無い。色の枠 1..8 は banto の
-- `--banto-chart-N`、9..16 は industrial のテーマの `--bi-chart-N`。
CREATE TABLE display_group_pens_new (
  group_id INTEGER NOT NULL REFERENCES display_groups(id) ON DELETE CASCADE,
  position INTEGER NOT NULL CHECK (position BETWEEN 0 AND 15),
  tag_id INTEGER NOT NULL,
  color_slot INTEGER CHECK (color_slot IS NULL OR color_slot BETWEEN 1 AND 16),
  PRIMARY KEY (group_id, position),
  UNIQUE (group_id, tag_id)
);

INSERT INTO display_group_pens_new (group_id, position, tag_id, color_slot)
  SELECT group_id, position, tag_id, color_slot FROM display_group_pens;

DROP TABLE display_group_pens;
ALTER TABLE display_group_pens_new RENAME TO display_group_pens;

CREATE INDEX idx_display_group_pens_tag ON display_group_pens(tag_id);
