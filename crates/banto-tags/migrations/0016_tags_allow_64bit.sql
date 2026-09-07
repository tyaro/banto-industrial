-- #325（2026-09-08 オーナー決定「i64/u64/f64 の3型を追加する」）: `tags` の
-- `data_type` CHECK に 64bit 型 `'i64','u64','f64'` を追加する。
--
-- 変更はこの1点だけ。列は1つも増えず、他の CHECK も FOREIGN KEY も
-- UNIQUE も 0015 時点のままである - にもかかわらずテーブル再構築が要るのは、
-- SQLite が CHECK を `ALTER` できないため（0005 の header が確立し、0011 /
-- 0015 が踏襲した理由そのもの）。したがって手順も 0011 / 0015 と**同じ**
-- 「新テーブル作成 → データコピー → DROP → RENAME → 索引再作成」で足りる:
-- `tags` は今も他テーブルから参照されない葉テーブルのままなので、0007 /
-- 0014 のような park-and-restore は不要（0005 の header が確立した3つの前提
-- - sqlx 自身が適用するのと同じ「1コネクション・1トランザクション・
-- FOREIGN KEY 強制下」で流す/参照先テーブルへの DROP は失敗する/参照先
-- テーブルの RENAME は子の FK 定義ごと引き継がれる - はそのまま効いている）。
--
-- 列構成は 0015 時点の全23列を**物理的な列順そのまま**にコピーする -
-- 0007/0011/0014/0015 の header が警告するとおり、ここで古い列リストに戻すと
-- 既存タグ行を静かに切り詰めてしまう。0015 以降 `tags` に触る migration は
-- 無いので、0015 の `tags_new` 定義がそのまま出発点になる（`string_encoding`
-- が `revision` の後ろに来ているのは 0013 の `ADD COLUMN` の結果の実際の順序
-- で、`SELECT *` を使う消費者のために 0015 が意図的に保った並び - ここでも
-- 変えない）。
--
-- **64bit 型を Modbus 接続配下に限る規則は SQL では書かない**（オーナー決定
-- 2026-09-08 の2点目）。その判定にはタグ →`collection_groups`
-- →`plc_connections` の JOIN が要り、SQLite の CHECK では表現できないので、
-- `calc`/`mem`/`postgres` の `tag_kind` 配置制約と全く同じ理由・同じ場所 -
-- `tag.rs::validate_tag_kind_placement`（判定本体は `placement_verdict`）-
-- が担う。この CHECK が答えるのは「行として well-formed か」だけである。
--
-- 検証は `banto_tags::tag` 内の
-- `migration_0016_preserves_rows_and_widens_data_type_on_a_populated_database`
-- で行う（0005/0007/0011/0014/0015 に倣い、populated なデータベースに対して
-- 実ファイルを `include_str!` で読み込んで適用する）。

CREATE TABLE tags_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    collection_group_id INTEGER NOT NULL REFERENCES collection_groups(id) ON DELETE RESTRICT,
    address TEXT NOT NULL,
    -- 本マイグレーションの唯一の変更点: 'i64','u64','f64' の追加
    -- （0005 由来の7種 + #325）。
    data_type TEXT NOT NULL CHECK (data_type IN ('bit', 'i16', 'u16', 'i32', 'u32', 'f32', 'i64', 'u64', 'f64', 'string')),
    string_length INTEGER CHECK (string_length IS NULL OR string_length BETWEEN 1 AND 128),
    raw_lo REAL,
    raw_hi REAL,
    eng_lo REAL,
    eng_hi REAL,
    unit TEXT,
    decimals INTEGER NOT NULL DEFAULT 0 CHECK (decimals BETWEEN 0 AND 6),
    threshold_h REAL,
    threshold_hh REAL,
    threshold_l REAL,
    threshold_ll REAL,
    enabled INTEGER NOT NULL DEFAULT 1,
    writable INTEGER NOT NULL DEFAULT 0,
    -- 0015 の主目的（'db' の追加）をそのまま引き継ぐ。
    tag_kind TEXT NOT NULL DEFAULT 'plc'
        CHECK (tag_kind IN ('plc', 'computed', 'internal', 'db')),
    expression TEXT,
    retain INTEGER NOT NULL DEFAULT 0,
    revision INTEGER NOT NULL DEFAULT 1,
    string_encoding TEXT NOT NULL DEFAULT 'utf8'
        CHECK (string_encoding IN ('utf8', 'shift_jis')),
    -- 0011 の主目的（グループ内一意）をそのまま引き継ぐ。
    UNIQUE (collection_group_id, name)
);

INSERT INTO tags_new (
    id, name, collection_group_id, address, data_type, string_length,
    raw_lo, raw_hi, eng_lo, eng_hi, unit, decimals,
    threshold_h, threshold_hh, threshold_l, threshold_ll, enabled,
    writable, tag_kind, expression, retain, revision, string_encoding
)
SELECT
    id, name, collection_group_id, address, data_type, string_length,
    raw_lo, raw_hi, eng_lo, eng_hi, unit, decimals,
    threshold_h, threshold_hh, threshold_l, threshold_ll, enabled,
    writable, tag_kind, expression, retain, revision, string_encoding
FROM tags;

DROP TABLE tags;
ALTER TABLE tags_new RENAME TO tags;

-- 0003 由来の索引はテーブル再構築で失われる（索引は DROP されたテーブルに
-- 属する）ので、0005/0007/0011/0015 と同じく元の名前で再作成する。`tags` に
-- はトリガが1つも無いので（0001-0015 のどこにも CREATE TRIGGER は無い）、
-- 再作成が要るのはこの索引だけ。
CREATE INDEX idx_tags_collection_group_id ON tags (collection_group_id);
