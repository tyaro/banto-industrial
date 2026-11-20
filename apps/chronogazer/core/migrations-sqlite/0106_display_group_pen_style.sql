-- #1207・#1231 PR 6a（docs/chronogazer/trend-design.md §5）: 表示グループのペンの属性の語彙。
-- すべて任意で、NULL = 既定（凡例の名前・単位・小数の桁はタグの設定、線は実線 1.5 px、
-- マーカーなし（大きさ 4）、縦軸は左・工学値レンジ）。既存の行は NULL のままで、
-- 画面は今までどおりに描く。管理線・bit の表し方は PR 7 で足す。
--
-- 検証（前後の空白の除去・空は NULL・文字数・列挙・範囲・0.5 刻み）は
-- `src/display_groups.rs` の `validate_shape` 1 か所で行う。ここの CHECK はその取りこぼしを
-- 防ぐ最後の砦（SQLite の CHECK は NULL を通すので、NULL = 既定はそのまま入る）。
ALTER TABLE display_group_pens
  ADD COLUMN label TEXT CHECK (label IS NULL OR length(label) BETWEEN 1 AND 64);
ALTER TABLE display_group_pens
  ADD COLUMN unit TEXT CHECK (unit IS NULL OR length(unit) BETWEEN 1 AND 16);
ALTER TABLE display_group_pens
  ADD COLUMN decimals INTEGER CHECK (decimals IS NULL OR decimals BETWEEN 0 AND 6);
ALTER TABLE display_group_pens
  ADD COLUMN line_style TEXT
  CHECK (line_style IS NULL OR line_style IN ('solid', 'dashed', 'dashDot', 'dashDotDot', 'none'));
ALTER TABLE display_group_pens
  ADD COLUMN line_width REAL CHECK (line_width IS NULL OR line_width BETWEEN 1 AND 4);
ALTER TABLE display_group_pens
  ADD COLUMN marker TEXT
  CHECK (marker IS NULL OR marker IN ('none', 'circle', 'square', 'triangle', 'diamond'));
ALTER TABLE display_group_pens
  ADD COLUMN marker_size INTEGER CHECK (marker_size IS NULL OR marker_size BETWEEN 2 AND 8);
ALTER TABLE display_group_pens
  ADD COLUMN axis TEXT CHECK (axis IS NULL OR axis IN ('left', 'right'));
ALTER TABLE display_group_pens
  ADD COLUMN y_min REAL;
ALTER TABLE display_group_pens
  ADD COLUMN y_max REAL CHECK (y_max IS NULL OR y_min IS NULL OR y_min < y_max);
