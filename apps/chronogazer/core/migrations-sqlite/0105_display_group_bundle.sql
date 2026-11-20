-- #1207・#1231 PR 5b（docs/chronogazer/trend-design.md §9 Q5）: 表示グループの「束」。
-- 束 = 名前だけの 1 階層の入れ物で、グループの任意の属性（NULL = 束なし）。同じ名前の
-- 束を持つグループが 1 つの束を作る（束のテーブルは持たない。2026-11-20 オーナー決定）。
-- 既存の行は NULL のまま（束なし）で、画面は今までどおり平らなタブ 1 列になる。
--
-- 検証（前後の空白の除去・空は NULL・50 文字以内）は `src/display_groups.rs` の
-- `validate_shape` 1 か所で行う。ここの CHECK はその取りこぼしを防ぐ最後の砦。
ALTER TABLE display_groups
  ADD COLUMN bundle TEXT CHECK (bundle IS NULL OR length(bundle) BETWEEN 1 AND 50);
