-- #1207・#1231 PR 8a（docs/chronogazer/trend-design.md §2・§3・§9）: 自由トレンド。登録されたタグから
-- 運転員がペンを選び、線の属性を変え、名前を付けて保存する。全員で共有する名前付きのもの
-- （利用者ごとにしない）。保存は全体で 200 まで（`free_trends::MAX_FREE_TRENDS`）、名前は重ならない。
--
-- 作った人は `created_by_kind`（'account' = アカウントのログイン、'localDesktop' = この PC からの
-- 接続の grant）と `created_by_user_id`（account のときの `users.id`、localDesktop のときは NULL）で
-- 判定する。`created_by` は表示用の当時の名前（アカウント名、または 'localDesktop'）で、判定には
-- 使わない。名前で比べると、(1) 予約名と同じ名前の古いアカウント（`local_grant::RESERVED_USERNAMES`）
-- が grant の作ったものを、(2) 消したアカウントと同じ名前で作り直したアカウントが前の人の作ったものを、
-- 自分の物として変えられてしまうため。`users.id` は AUTOINCREMENT なので再利用されない。
-- `created_by_user_id` に `users(id)` への FK は張らない: アカウントの削除（banto の `UsersService`、
-- 行を消す）が自由トレンドに妨げられないように、また作った人が消えても自由トレンドは残す
-- （消えたアカウントの物は editor 以上が変える）ため。
--
-- 検証は `src/free_trends.rs` の `validate_free_trend` 1 か所で行う（ペンの属性は表示グループと
-- 同じ `display_groups::validate_pen_style`）。ここの CHECK / UNIQUE はその取りこぼしを防ぐ最後の砦
-- （SQLite の CHECK は NULL を通す）。
CREATE TABLE free_trends (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  name TEXT NOT NULL UNIQUE CHECK (length(name) BETWEEN 1 AND 64),
  -- 表示グループのトレンドの `TREND_TIME_WINDOWS_SEC` から 1 つ（既定 600）。
  time_window_sec INTEGER NOT NULL DEFAULT 600
    CHECK (time_window_sec IN (60, 300, 600, 1800, 3600)),
  -- 楽観ロック用の版。作成で 1、更新のたびに +1。
  revision INTEGER NOT NULL DEFAULT 1,
  created_by_kind TEXT NOT NULL CHECK (created_by_kind IN ('account', 'localDesktop')),
  created_by_user_id INTEGER
    CHECK ((created_by_kind = 'account') = (created_by_user_id IS NOT NULL)),
  created_by TEXT NOT NULL
    CHECK (created_by_kind <> 'localDesktop' OR created_by = 'localDesktop'),
  created_at TEXT NOT NULL DEFAULT (datetime('now')),
  updated_by TEXT NOT NULL,
  updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- ペン: 1 つの自由トレンドに最大 16（position 0..15）。属性の列と CHECK は表示グループのペン
-- （`display_group_pens`、0104・0106〜0108）と同じ。`tag_id` に FK を張らない理由も同じ
-- （0101 の注記）。参照されているタグの削除の拒否は
-- `display_groups::DisplayGroupService::delete_tag_unless_referenced` が表示グループと一緒に
-- `BEGIN IMMEDIATE` の中で確かめる。
CREATE TABLE free_trend_pens (
  free_trend_id INTEGER NOT NULL REFERENCES free_trends(id) ON DELETE CASCADE,
  position INTEGER NOT NULL CHECK (position BETWEEN 0 AND 15),
  tag_id INTEGER NOT NULL,
  color_slot INTEGER CHECK (color_slot IS NULL OR color_slot BETWEEN 1 AND 16),
  label TEXT CHECK (label IS NULL OR length(label) BETWEEN 1 AND 64),
  unit TEXT CHECK (unit IS NULL OR length(unit) BETWEEN 1 AND 16),
  decimals INTEGER CHECK (decimals IS NULL OR decimals BETWEEN 0 AND 6),
  line_style TEXT
    CHECK (line_style IS NULL OR line_style IN ('solid', 'dashed', 'dashDot', 'dashDotDot', 'none')),
  line_width REAL CHECK (line_width IS NULL OR line_width BETWEEN 1 AND 4),
  marker TEXT
    CHECK (marker IS NULL OR marker IN ('none', 'circle', 'square', 'triangle', 'diamond')),
  marker_size INTEGER CHECK (marker_size IS NULL OR marker_size BETWEEN 2 AND 8),
  axis TEXT CHECK (axis IS NULL OR axis IN ('left', 'right')),
  y_min REAL,
  y_max REAL CHECK (y_max IS NULL OR y_min IS NULL OR y_min < y_max),
  limit_lines INTEGER CHECK (limit_lines IS NULL OR limit_lines IN (0, 1)),
  limit_hh REAL,
  limit_h REAL CHECK (limit_h IS NULL OR limit_hh IS NULL OR limit_h <= limit_hh),
  limit_l REAL CHECK (
    limit_l IS NULL
    OR ((limit_h IS NULL OR limit_l <= limit_h) AND (limit_hh IS NULL OR limit_l <= limit_hh))
  ),
  limit_ll REAL CHECK (
    limit_ll IS NULL
    OR ((limit_l IS NULL OR limit_ll <= limit_l)
        AND (limit_h IS NULL OR limit_ll <= limit_h)
        AND (limit_hh IS NULL OR limit_ll <= limit_hh))
  ),
  bit_style TEXT CHECK (bit_style IS NULL OR bit_style IN ('step', 'band')),
  bit_band_height INTEGER CHECK (bit_band_height IS NULL OR bit_band_height BETWEEN 8 AND 32),
  PRIMARY KEY (free_trend_id, position),
  UNIQUE (free_trend_id, tag_id)
);

CREATE INDEX idx_free_trend_pens_tag ON free_trend_pens(tag_id);
