-- H2（tyaro/banto-industrial#1053 の H の後半、2026-11-07）: PLC の時計合わせの
-- 接続ごとの設定（`crate::clock_sync` のモジュール doc 参照）。Hub の運用の
-- 時刻表なので banto-tags のレジストリ（`plc_connections`）には列を足さず、
-- この app のテーブルとして持つ（他の Hub 専用テーブルと同じ `hub_` 接頭辞）。
-- 設定の変更は保留キュー（pending_changes）を通さず、REST の PUT で即時に
-- 保存する（二重のゲートのもう片方、ゲストの設定 `clock_set` は接続の設定に
-- あり、従来どおり保留キューを通る）。
--
-- `connection_id` は `plc_connections(id)` を指すが FOREIGN KEY は張らない
-- （`hub_retained_values`・`hub_sink_groups` と同じ慣行。`plc_connections` は
-- banto-tags のマイグレーションが作り、この app のマイグレーションより後に
-- 流れる）。接続の id は AUTOINCREMENT で再利用されないので、接続を消した後に
-- 行が残っても別の接続に効くことはない（適用済みのカタログに無い接続は
-- 実行しない）。
--
-- 行が無い = 無効（GET は既定の「無効・毎時 5 分」を返す）。時刻表は
-- 「毎時 m 分」（`hourly`、`hour` は NULL）か「毎日 hh:mm」（`daily`）だけで、
-- cron 式は持たない。時刻は Hub の PC の現地時刻。daily の `hour IS NOT NULL` は
-- 要る（SQLite の CHECK は NULL を偽と見なさないので、`hour BETWEEN 0 AND 23`
-- だけでは NULL の時が通る）。
CREATE TABLE hub_clock_sync (
  connection_id INTEGER PRIMARY KEY,
  enabled INTEGER NOT NULL DEFAULT 0 CHECK (enabled IN (0, 1)),
  schedule_kind TEXT NOT NULL CHECK (schedule_kind IN ('hourly', 'daily')),
  hour INTEGER,
  minute INTEGER NOT NULL CHECK (minute BETWEEN 0 AND 59),
  updated_at TEXT NOT NULL DEFAULT (datetime('now')),
  CHECK (
    (schedule_kind = 'hourly' AND hour IS NULL)
    OR (schedule_kind = 'daily' AND hour IS NOT NULL AND hour BETWEEN 0 AND 23)
  )
);
