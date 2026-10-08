-- #533（2026-10-08 オーナー決定「しきい値は使う側（記録計・SCADA）が持つ設定とし、
-- Hub は持たない」）: `tags` からしきい値の列 `threshold_h` / `threshold_hh` /
-- `threshold_l` / `threshold_ll` を外す。
--
-- しきい値はデータ点（タグ定義）の性質ではない。ChronoGazer は #532 で記録計の
-- 側の表（chronogazer の `recorder_tag_settings`）へ移り、収集には
-- `banto_collect::build_config_lenient_with_thresholds` で渡す。banto-hub は
-- しきい値を持たず、警報を判定しない（Hub はデータを集めて配る役に徹する）。
-- この列を読む者も書く者も、もう居ない。
--
-- ## 値は移さない
--
-- 列に残っていた値は**捨てる**。ChronoGazer は #532 の時点で列を読まなくなって
-- おり（0102 の header「既存の値（tags.threshold_*）は移さない」）、banto-hub
-- では判定そのものをやめる。既存の DB は壊してよい（アルファ版、オーナー決定）
-- ので、移行先も無い。
--
-- ## 手順（`ALTER TABLE ... DROP COLUMN` を使わない理由）
--
-- 同梱の SQLite（3.35 以降）は `DROP COLUMN` を持つが、このクレートの `tags` の
-- 作り直しは 0005 / 0011 / 0015 / 0016 で一貫して「新テーブル作成 → データコピー
-- → DROP → RENAME → 索引再作成」で行ってきた。同じ手順にそろえることで、
-- 列の並び・CHECK・FOREIGN KEY・UNIQUE・索引を 1 か所（下の `tags_new`）で
-- 読めるようにする。`tags` は今も他テーブルから参照されない葉テーブルのまま
-- （トリガもビューも無い）なので、0016 と同じく park-and-restore は不要。
--
-- 列構成は 0016 の全23列から 4 列を除いた全19列で、**残す列の物理的な順序は
-- 0016 のまま**にする（`string_encoding` が末尾にあるのも 0013 由来の実際の順序。
-- `SELECT *` を使う消費者のために 0015 / 0016 が保った並びを変えない）。
-- ここで古い列リストに戻すと既存タグ行を静かに切り詰めてしまう点は、
-- 0007/0011/0014/0015/0016 の header の警告と同じ。
--
-- CHECK・FOREIGN KEY・UNIQUE は 0016 の定義をそのまま引き継ぐ（変更は列を
-- 外すことだけ）。
--
-- 検証は `banto_tags::tag` 内の
-- `migration_0018_drops_threshold_columns_and_preserves_rows_on_a_populated_database`
-- で行う（0016 に倣い、populated なデータベースに対して実ファイルを
-- `include_str!` で読み込んで適用する）。

CREATE TABLE tags_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    collection_group_id INTEGER NOT NULL REFERENCES collection_groups(id) ON DELETE RESTRICT,
    address TEXT NOT NULL,
    data_type TEXT NOT NULL CHECK (data_type IN ('bit', 'i16', 'u16', 'i32', 'u32', 'f32', 'i64', 'u64', 'f64', 'string')),
    string_length INTEGER CHECK (string_length IS NULL OR string_length BETWEEN 1 AND 128),
    raw_lo REAL,
    raw_hi REAL,
    eng_lo REAL,
    eng_hi REAL,
    unit TEXT,
    decimals INTEGER NOT NULL DEFAULT 0 CHECK (decimals BETWEEN 0 AND 6),
    -- 本マイグレーションの唯一の変更点: ここにあった threshold_h / threshold_hh /
    -- threshold_l / threshold_ll の 4 列を外した。
    enabled INTEGER NOT NULL DEFAULT 1,
    writable INTEGER NOT NULL DEFAULT 0,
    tag_kind TEXT NOT NULL DEFAULT 'plc'
        CHECK (tag_kind IN ('plc', 'computed', 'internal', 'db')),
    expression TEXT,
    retain INTEGER NOT NULL DEFAULT 0,
    revision INTEGER NOT NULL DEFAULT 1,
    string_encoding TEXT NOT NULL DEFAULT 'utf8'
        CHECK (string_encoding IN ('utf8', 'shift_jis')),
    UNIQUE (collection_group_id, name)
);

INSERT INTO tags_new (
    id, name, collection_group_id, address, data_type, string_length,
    raw_lo, raw_hi, eng_lo, eng_hi, unit, decimals, enabled,
    writable, tag_kind, expression, retain, revision, string_encoding
)
SELECT
    id, name, collection_group_id, address, data_type, string_length,
    raw_lo, raw_hi, eng_lo, eng_hi, unit, decimals, enabled,
    writable, tag_kind, expression, retain, revision, string_encoding
FROM tags;

-- AUTOINCREMENT の採番位置を引き継ぐ。コピーだけだと `tags_new` の採番は
-- 「残っている行の最大 id」から始まり、末尾で消したタグの id が別のタグに
-- 再利用される。ChronoGazer の `recorder_tag_settings` / `display_group_pens` は
-- `tags.id` を FK 無しで指していて、「AUTOINCREMENT なので再利用されない」ことを
-- 前提にしている（0102 の header）。`DROP TABLE tags` で `tags` の行は
-- `sqlite_sequence` から消え、`RENAME` で `tags_new` の行が `tags` になるので、
-- その前に旧い位置（大きい方）を `tags_new` に写す。
INSERT INTO sqlite_sequence (name, seq)
SELECT 'tags_new', seq FROM sqlite_sequence
 WHERE name = 'tags'
   AND NOT EXISTS (SELECT 1 FROM sqlite_sequence WHERE name = 'tags_new');
UPDATE sqlite_sequence
   SET seq = (SELECT MAX(seq) FROM sqlite_sequence WHERE name IN ('tags', 'tags_new'))
 WHERE name = 'tags_new';

DROP TABLE tags;
ALTER TABLE tags_new RENAME TO tags;

-- 0003 由来の索引はテーブル再構築で失われるので、0016 と同じく元の名前で
-- 再作成する（`tags` にトリガは無い）。
CREATE INDEX idx_tags_collection_group_id ON tags (collection_group_id);
