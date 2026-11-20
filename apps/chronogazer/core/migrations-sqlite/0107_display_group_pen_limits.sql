-- #1207・#1231 PR 7a（docs/chronogazer/trend-design.md §5）: 表示グループのペンの管理線の属性。
-- limit_lines = 1 で管理線（HH・H・L・LL）を引く。NULL・0 = 引かない（既定は無効、2026-11-20 オーナー決定）。
-- limit_hh・limit_h・limit_l・limit_ll は表示用の上書きの値で、NULL = タグのしきい値
-- （tag_thresholds の HH・H・L・LL）を使う。値は表示だけに使い、しきい値のイベントは変えない。
-- 上書きの値は limit_lines が無効でも保存する（種別を問わず保存し、効くのはトレンドだけ）。
--
-- 検証（有限の数・LL <= L <= H <= HH）は `src/display_groups.rs` の `validate_shape` 1 か所で行う。
-- ここの CHECK はその取りこぼしを防ぐ最後の砦（SQLite の CHECK は NULL を通すので、設定した
-- 値どうしだけが比べられる）。
ALTER TABLE display_group_pens
  ADD COLUMN limit_lines INTEGER CHECK (limit_lines IS NULL OR limit_lines IN (0, 1));
ALTER TABLE display_group_pens
  ADD COLUMN limit_hh REAL;
ALTER TABLE display_group_pens
  ADD COLUMN limit_h REAL CHECK (limit_h IS NULL OR limit_hh IS NULL OR limit_h <= limit_hh);
ALTER TABLE display_group_pens
  ADD COLUMN limit_l REAL CHECK (
    limit_l IS NULL
    OR ((limit_h IS NULL OR limit_l <= limit_h) AND (limit_hh IS NULL OR limit_l <= limit_hh))
  );
ALTER TABLE display_group_pens
  ADD COLUMN limit_ll REAL CHECK (
    limit_ll IS NULL
    OR ((limit_l IS NULL OR limit_ll <= limit_l)
        AND (limit_h IS NULL OR limit_ll <= limit_h)
        AND (limit_hh IS NULL OR limit_ll <= limit_hh))
  );
