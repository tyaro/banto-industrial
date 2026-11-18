-- 外部 DB 連携 S2（docs/banto-hub-external-db-design.md §4.1・§4.2・§6-4、
-- 2026-09-06 オーナー決定「案A: 既存 3 階層の流用」）: DB Source 本体が要求
-- する2つのスキーマ変更を1本にまとめる。
--
-- 1. `collection_groups.query_sql TEXT`（NULL 許容）: 「1 グループ = 1 SELECT」
--    （§4.2）のその SELECT 文そのもの。`protocol = 'postgres'` の接続配下の
--    グループでは必須、それ以外の接続配下では常に NULL - どちらの向きも
--    application 層（`collection_group.rs` の `validate_query_sql`）が強制
--    する。SQL 文の中身の検証（先頭 SELECT/WITH・`;` を含まない・長さ）も
--    ベストエフォートで同じ場所にあり、**本当の防御は DB 側の read-only
--    ユーザー**（§4.2）。0012（`default_writable`）/0013（`string_encoding`）
--    と同じくプレーンな `ADD COLUMN` 1本で足りる: 制約を一切足していないので、
--    SQLite の「ADD COLUMN で CHECK を足せるのは新規列自身のみを参照する場合
--    に限る」という制約に抵触しようがない。
--
-- 2. `tags.tag_kind` の CHECK に `'db'` を追加: DB の結果列1つを現在値として
--    公開するタグ（§4.1「タグ: `tag_kind = "db"`、`address` に**結果列名**」）。
--    配置制約（`db` タグは `postgres` 接続配下にのみ、`plc` タグは `postgres`
--    接続配下に置けない）は SQL ではなく
--    `tag.rs::validate_tag_kind_placement` が担う - `computed`/`internal` の
--    `calc`/`mem` 配置制約と全く同じ理由（接続への JOIN が要るため CHECK では
--    書けない、そちらの doc comment 参照）。
--
--    SQLite は CHECK を ALTER できないので `tags` のテーブル再構築が要る -
--    0011（`UNIQUE(collection_group_id, name)` への緩和）と**同じ**
--    「DROP → RENAME」で足りる: `tags` は今も他テーブルから参照されない葉
--    テーブルのままなので、0007/0014 のような park-and-restore は不要
--    （0005 の header が確立した3つの前提 - sqlx 自身が適用するのと同じ
--    「1コネクション・1トランザクション・FOREIGN KEY 強制下」で流す/参照先
--    テーブルへの DROP は失敗する/参照先テーブルの RENAME は子の FK 定義ごと
--    引き継がれる - はそのまま効いている）。
--
--    列構成は 0013 時点の全列（0009 の revision、0011 の
--    UNIQUE(collection_group_id, name)、0013 の string_encoding を含む）を
--    **物理的な列順そのまま**にコピーする - 0007/0011/0014 の header が警告
--    するとおり、ここで古い列リストに戻すと既存タグ行を静かに切り詰めて
--    しまう。`string_encoding` が `revision` の後ろに来ているのは 0013 が
--    `ALTER TABLE ... ADD COLUMN` で足した結果の実際の順序で、`SELECT *` を
--    使う消費者が居た場合にも列順が変わらないようにするための意図的な選択。
--
-- 検証は `banto_tags::tag` 内の
-- `migration_0015_preserves_rows_and_foreign_keys_on_a_populated_database`
-- で行う（0005/0007/0011/0014 に倣い、populated なデータベースに対して実
-- ファイルを `include_str!` で読み込んで適用する）。

ALTER TABLE collection_groups ADD COLUMN query_sql TEXT;

CREATE TABLE tags_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    collection_group_id INTEGER NOT NULL REFERENCES collection_groups(id) ON DELETE RESTRICT,
    address TEXT NOT NULL,
    data_type TEXT NOT NULL CHECK (data_type IN ('bit', 'i16', 'u16', 'i32', 'u32', 'f32', 'string')),
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
    -- 本マイグレーションの主目的: 'db' の追加（0006 由来の3種 + S2）。
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
-- 属する）ので、0005/0007/0011 と同じく元の名前で再作成する。
CREATE INDEX idx_tags_collection_group_id ON tags (collection_group_id);
