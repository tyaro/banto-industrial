-- 2026-08-31 オーナー決定: タグ名の一意性を「全タグで一意」から「収集グループ
-- 内で一意」へ緩和する（`UNIQUE(name)` → `UNIQUE(collection_group_id, name)`）。
--
-- 経緯: 外部名は `{connection}.{group}.{tag}` の合成
-- (`apps/banto-hub/core/src/hub.rs::build_catalog_from`、design §1.1 の
-- FA-Server Unit/Folder/Tag 対応) であり、末端のタグ名まで全体一意にする
-- 必要はない。ところが従来の全体一意制約は、同型構成の装置を複数台つないで
-- 同じ PLC アドレス（例: 2台目も `D100`）をタグ名に使うという産業用途の実態
-- と合わず、2台目の登録が「既に使用されています」で弾かれていた。
--
-- 外部名がグループをまたいでも衝突しない理由は変わらない:
-- `collection_groups.name`/`plc_connections.name` は今も各々グローバルに
-- `UNIQUE` のまま（この変更で緩めるのは `tags.name` だけ）なので、同じ
-- `name` を持つ2つのタグは必ず異なるグループに属し、それらのグループ名
-- 自体が既に異なる - よって `{connection}.{group}.{tag}` は依然として
-- グローバルに一意（`crate::hub::TagMap` の doc comment参照）。
--
-- オーナーより「互換性は考慮しなくて問題ない」との明示的な指示があるため、
-- 既存データの移行は素通し（厳しい制約→緩い制約なので既存行は全て新しい
-- 制約を満たす）。API キーのスコープ判定（`api_keys.rs` の
-- `can_read_value`/`has_write_scope`）は最初から `external_name` ベースで
-- 素の `name` を見ていないため、この変更の影響を受けない。
--
-- SQLite は既存の列内 `UNIQUE` を `ALTER` で外せない（テーブル制約としての
-- 複合 `UNIQUE` に置き換えるにはそもそも列定義自体を書き換える必要がある）
-- ので、0005/0007 と同じテーブル再構築が要る。0005 の header が確立した
-- 3つの前提（sqlx 自身が適用するのと同じ「1コネクション・1トランザクション・
-- FOREIGN KEY 強制下」で流す/参照先テーブルへの DROP は失敗する/参照先
-- テーブルの RENAME は子の FK 定義ごと引き継がれる）に加え、`tags` は今も
-- 他テーブルから参照されない葉テーブルのままなので、0007 のような
-- park-and-restore は不要 - 0005 と全く同じ「DROP → RENAME」で足りる。
--
-- 列構成は 0009 時点の全列（0006 の writable/tag_kind/expression/retain、
-- 0009 の revision を含む）をそのままコピーする - 0007 の header が警告する
-- とおり、ここで古い列リストに戻すと既存タグ行を静かに切り詰めてしまう。
--
-- 検証は `banto_tags::tag` 内のテストで行う（0005/0007 に倣い、populated な
-- データベースに対して実ファイルを `include_str!` で読み込んで適用する）。

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
    tag_kind TEXT NOT NULL DEFAULT 'plc' CHECK (tag_kind IN ('plc', 'computed', 'internal')),
    expression TEXT,
    retain INTEGER NOT NULL DEFAULT 0,
    revision INTEGER NOT NULL DEFAULT 1,
    -- 本マイグレーションの主目的: 全体一意 → 収集グループ内一意。
    UNIQUE (collection_group_id, name)
);

INSERT INTO tags_new (
    id, name, collection_group_id, address, data_type, string_length,
    raw_lo, raw_hi, eng_lo, eng_hi, unit, decimals,
    threshold_h, threshold_hh, threshold_l, threshold_ll, enabled,
    writable, tag_kind, expression, retain, revision
)
SELECT
    id, name, collection_group_id, address, data_type, string_length,
    raw_lo, raw_hi, eng_lo, eng_hi, unit, decimals,
    threshold_h, threshold_hh, threshold_l, threshold_ll, enabled,
    writable, tag_kind, expression, retain, revision
FROM tags;

DROP TABLE tags;
ALTER TABLE tags_new RENAME TO tags;

-- 0003 由来の索引はテーブル再構築で失われる（索引は DROP されたテーブルに
-- 属する）ので、0005/0007 と同じく元の名前で再作成する。
CREATE INDEX idx_tags_collection_group_id ON tags (collection_group_id);
