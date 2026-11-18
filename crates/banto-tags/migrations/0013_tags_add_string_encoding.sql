-- T20 ①a（文字列タグへの書き込み、docs/banto-hub-t20-design.md §3.1、
-- 2026-09-04 オーナー決定「文字コードは既定 UTF-8、タグ単位で Shift-JIS も
-- 選択可」）: `tags.string_encoding` を追加し、`data_type = 'string'` タグが
-- ワイヤへ書き込まれる際のエンコーディングをタグ単位で選べるようにする。
--
-- 既定は `'utf8'`（案A、書き込みは write_path 経由 - 記録計の read/cache
-- には触れない。文字列 read は本スライスの対象外＝①b）。既存の string タグ
-- （0005 以降に登録済みのもの）は全て `utf8` へ backfill される -
-- `banto-plc-write` は S1 導入当初から Shift-JIS 固定だったため、この
-- backfill は「今まで書き込みに使われていなかった文字列タグ」にのみ影響し
-- (write_path 側で string タグへの書き込みは gate 7 が本スライスまで一律
-- 拒否していた - `apps/banto-hub/core/src/write_path.rs`のconvert_value)、
-- 実害は無い。Shift-JIS のまま使いたい既存タグは登録後にこの列を更新すれば
-- よい。
--
-- 0012（`collection_groups.default_writable`）と同じ理由でプレーンな
-- `ADD COLUMN` 1本にしている: 追加する CHECK は新規列自身しか参照しないため、
-- SQLite の「ADD COLUMN で CHECK 制約を追加できるのは新規列自身のみを参照
-- する場合に限る」という制約に収まり、0004/0005 のようなテーブル再構築は
-- 不要。

ALTER TABLE tags
    ADD COLUMN string_encoding TEXT NOT NULL DEFAULT 'utf8'
    CHECK (string_encoding IN ('utf8', 'shift_jis'));
