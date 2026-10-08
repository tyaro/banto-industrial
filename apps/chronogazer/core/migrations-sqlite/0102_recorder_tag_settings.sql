-- #532（2026-10-08 オーナー決定）: 記録計の側のタグごとの設定
-- （recorder-requirements.md §3.1・§3.7.3 の (0b)）。しきい値 H / HH / L / LL は
-- データ点（タグ定義）の性質ではなく、使う側（記録計・SCADA）が持つ設定なので、
-- banto-tags の `tags` の列ではなくこのアプリの表に持つ。
--
-- - **既定は設定なし = 行が無い**。行があっても 4 つとも NULL なら設定なし
--   （画面で全部消したとき。版を続けて進めるため、行は消さずに残す）。
-- - 検証（大小関係 LL <= L <= H <= HH、対象タグの存在、文字列タグには付けない）は
--   `src/tag_thresholds.rs` の 1 か所で行う（banto-tags の `validate_thresholds` と
--   同じ規則・同じフィールド名）。
-- - `revision` は楽観ロック用の版。作成で 1、更新のたびに +1。行が無いタグは
--   版 0 として扱う（サービスの doc）。
--
-- ## 点の指し方（タグ ID）と Hub 経由のタグ（#383）
--
-- 今は banto-tags の `tags(id)` で点を指す（SLMP / Modbus のタグ）。`tags` への FK は
-- 張らない - 理由は 0101 の `display_group_pens.tag_id` と同じ（banto-tags の
-- migration は `tags` を作り直すことがあり、この app から FK を張ると上流の
-- migration が落ちる）。タグの削除と一緒にこの行を消すのは
-- `DisplayGroupService::delete_tag_unless_referenced` の `BEGIN IMMEDIATE` の中。
-- `tags.id` は AUTOINCREMENT なので、消したタグの ID が別のタグに再利用されて
-- 古い設定が付くことは無い。
--
-- Hub 経由のタグ（#383。今は `tags` の行が無く、Hub の external name で購読して
-- いる）は、合流の段で次のどちらかで同じ表に載せる:
-- 1. Hub 経由の点も `tags` の行（Hub 接続配下の収集グループ）として持つなら、
--    そのまま `tag_id` で指せる（表の変更なし）。
-- 2. 行を持たない形のままなら、新しい migration で点の種類の列（例: `source`
--    = 'tag' / 'hub'）と外部名の列を足し、一意の鍵を (source, 点の鍵) に広げる
--    （既存の行は source = 'tag'）。収集・イベントの鍵（`tag:<id>`）も同じ規則で
--    種類ごとに分ける。
-- どちらでも、しきい値の列・検証・版・監査はこの表とサービスのまま使える。
--
-- ## 既存の値（tags.threshold_*）は移さない
--
-- この app の migration は `banto_tags::migrate` より**前**に流れる
-- （`src/db.rs` の `run_migrations`）。新しい DB では、この時点で `tags` がまだ
-- 無いので、`INSERT ... SELECT FROM tags` は書けない（一度だけ流れる migration が
-- 新規の DB で必ず落ちる）。既存の DB は壊してよい（アルファ版、オーナー決定）
-- ので、移行はしない。必要なら画面で入れ直す（#532 の PR に記録）。
CREATE TABLE recorder_tag_settings (
  tag_id INTEGER PRIMARY KEY,
  threshold_ll REAL,
  threshold_l REAL,
  threshold_h REAL,
  threshold_hh REAL,
  revision INTEGER NOT NULL DEFAULT 1,
  updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
