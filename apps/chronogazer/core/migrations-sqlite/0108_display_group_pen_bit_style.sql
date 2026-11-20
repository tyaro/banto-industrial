-- #1207・#1231 PR 7c（docs/chronogazer/trend-design.md §5）: 表示グループのペンの bit の表しの属性。
-- bit_style は 'step'（段差）| 'band'（帯。タイムチャート）で、NULL = 段差。帯は描画域の下から
-- 自動で積む（縦の位置は持たない、2026-11-20 オーナー決定）。bit_band_height は帯の高さ（px、8〜32）で、
-- NULL = 16。種別・タグの型を問わず保存し、効くのはトレンドの bit のタグだけ。
--
-- 検証は `src/display_groups.rs` の `validate_shape` 1 か所で行う。ここの CHECK はその取りこぼしを
-- 防ぐ最後の砦（SQLite の CHECK は NULL を通す）。
ALTER TABLE display_group_pens
  ADD COLUMN bit_style TEXT CHECK (bit_style IS NULL OR bit_style IN ('step', 'band'));
ALTER TABLE display_group_pens
  ADD COLUMN bit_band_height INTEGER CHECK (bit_band_height IS NULL OR bit_band_height BETWEEN 8 AND 32);
