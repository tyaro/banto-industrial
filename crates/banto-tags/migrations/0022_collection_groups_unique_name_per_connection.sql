-- 2026-10-19 オーナー決定: 収集グループ名の一意性を「全グループで一意」から
-- 「接続内で一意」へ緩和する（`name TEXT NOT NULL UNIQUE` →
-- `UNIQUE (plc_connection_id, name)`）。接続名（`plc_connections.name`）は
-- 今もグローバルに一意のまま。
--
-- 経緯は 0011（タグ名を収集グループ内一意へ緩和）と同じ: 同型構成の装置を
-- 複数台つなぐと、どの接続にも同じグループ名（例: `fast`）を付けたくなるが、
-- 全体一意の制約が 2 台目の登録を「既に使用されています」で弾いていた。
--
-- 外部名 `{connection}.{group}.{tag}`（`apps/banto-hub/core/src/hub.rs` の
-- `build_catalog_from`）が今もグローバルに一意である理由: 同じ名前の 2 つの
-- グループは必ず異なる接続に属し、接続名はグローバルに一意なので、外部名の
-- 先頭の要素が既に異なる。ただし名前に `.` を含められるので、文字列としての
-- 衝突（`a.b` + `c` と `a` + `b.c`）は以前から hub 側（#635）が別に扱っている -
-- この変更はその判定の前提（接続名・グループ名・タグ名の組で区別する）を
-- 変えない。
--
-- ## 手順（0019 と同じ park-and-restore）
--
-- SQLite は列定義の `UNIQUE` を ALTER で外せないので collection_groups を
-- 作り直す。0004 の header が確立した 3 つの前提（sqlx が 1 トランザクション・
-- FOREIGN KEY 強制下で流す / 参照されている表の DROP は失敗する / 参照されて
-- いる表の RENAME は子の FK 定義ごと引きずる）はそのまま効いているので、
-- 子（tags）の行を一時表へ退避して消し、collection_groups_new を作って中身を
-- 写し、旧表を DROP して新表を空いた名前へ RENAME し、退避した行を戻す。
--
-- 列は**現在の全列を物理的な順序のまま**: 0002 の 5 列、0012 の
-- default_writable、0015 の query_sql の 7 列。tags は 0018 の後の 19 列
-- （0019 と同じ列リスト）。古い列リストに戻すと既存行を静かに切り詰める
-- （0007/0011/0014/0015/0016/0018/0019 の header の警告と同じ）。
--
-- ## トリガ（0020）を先に外し、戻した後に作り直す
--
-- `tags` には 0020 のトリガ 2 本があり、本体が `collection_groups` を参照する。
-- 0020 の header のとおり、退避した行を戻す間はトリガが無い方がよい（既存の
-- 重複はトリガが検査しない約束なので、戻す INSERT で断られては困る）。また
-- 参照先の表を DROP → RENAME する間に、トリガの本体が存在しない表を指す
-- 状態を作らない。よって最初に DROP し、tags の行を戻した後に 0020 と同じ
-- 定義で作り直す。
--
-- ## 索引と AUTOINCREMENT
--
-- `idx_collection_groups_plc_connection_id`（0002）は表ごと消えるので元の名前で
-- 作り直す（新しい複合 UNIQUE の自動索引が同じ先頭列を持つので冗長ではあるが、
-- 名前で索引を確かめる既存のテストと文書のために残す）。collection_groups の
-- 採番位置は 0018/0019 と同じ書き方で引き継ぐ（グループ ID を FK 無しで指す
-- 表がある - banto-hub の sink 設定・pending changes、ChronoGazer の表示グループ
-- ほか）。tags は DROP しない（DELETE して元の id で戻すだけ）ので採番位置も
-- 索引 `idx_tags_collection_group_id` も動かない。ビューはどの表にも無い。
--
-- 検証は `banto_tags::collection_group` 内の
-- `migration_0022_*` のテスト（実際の migrator で 0021 まで流した populated な
-- データベースに対して [`crate::migrate`] を流す）。

DROP TRIGGER tags_hub_external_name_unique_insert;
DROP TRIGGER tags_hub_external_name_unique_update;

-- 子を退避する。
CREATE TEMPORARY TABLE _m0022_tags AS SELECT * FROM tags;
DELETE FROM tags;

CREATE TABLE collection_groups_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    plc_connection_id INTEGER NOT NULL REFERENCES plc_connections(id) ON DELETE RESTRICT,
    period_ms INTEGER NOT NULL CHECK (period_ms IN (100, 200, 500, 1000, 2000, 5000, 10000, 60000)),
    enabled INTEGER NOT NULL DEFAULT 1,
    default_writable INTEGER NOT NULL DEFAULT 1 CHECK (default_writable IN (0, 1)),
    query_sql TEXT,
    -- 本マイグレーションの主目的: 全体一意 → 接続内一意。
    UNIQUE (plc_connection_id, name)
);

INSERT INTO collection_groups_new (
    id, name, plc_connection_id, period_ms, enabled, default_writable, query_sql
)
SELECT id, name, plc_connection_id, period_ms, enabled, default_writable, query_sql
FROM collection_groups;

-- 採番位置を引き継ぐ（0018/0019 と同じ書き方）。
INSERT INTO sqlite_sequence (name, seq)
SELECT 'collection_groups_new', seq FROM sqlite_sequence
 WHERE name = 'collection_groups'
   AND NOT EXISTS (SELECT 1 FROM sqlite_sequence WHERE name = 'collection_groups_new');
UPDATE sqlite_sequence
   SET seq = (SELECT MAX(seq) FROM sqlite_sequence
               WHERE name IN ('collection_groups', 'collection_groups_new'))
 WHERE name = 'collection_groups_new';

DROP TABLE collection_groups;
ALTER TABLE collection_groups_new RENAME TO collection_groups;

CREATE INDEX idx_collection_groups_plc_connection_id ON collection_groups (plc_connection_id);

-- 子を戻す。現在の全列。
INSERT INTO tags (
    id, name, collection_group_id, address, data_type, string_length,
    raw_lo, raw_hi, eng_lo, eng_hi, unit, decimals, enabled,
    writable, tag_kind, expression, retain, revision, string_encoding
)
SELECT
    id, name, collection_group_id, address, data_type, string_length,
    raw_lo, raw_hi, eng_lo, eng_hi, unit, decimals, enabled,
    writable, tag_kind, expression, retain, revision, string_encoding
FROM _m0022_tags;

DROP TABLE _m0022_tags;

-- 0020 のトリガを同じ定義で作り直す。**後の migration で `tags` または
-- `collection_groups` を作り直すときも、このトリガを作り直すこと。**
CREATE TRIGGER tags_hub_external_name_unique_insert
BEFORE INSERT ON tags
WHEN NEW.tag_kind = 'plc'
    AND EXISTS (
        SELECT 1 FROM collection_groups g
        JOIN plc_connections c ON c.id = g.plc_connection_id
        WHERE g.id = NEW.collection_group_id AND c.protocol = 'hub'
    )
    AND EXISTS (
        SELECT 1 FROM tags t
        JOIN collection_groups g ON g.id = t.collection_group_id
        JOIN plc_connections c ON c.id = g.plc_connection_id
        WHERE c.protocol = 'hub' AND t.address = NEW.address
    )
BEGIN
    SELECT RAISE(ABORT, 'banto_tags: hub external name already registered');
END;

CREATE TRIGGER tags_hub_external_name_unique_update
BEFORE UPDATE OF address, collection_group_id, tag_kind ON tags
WHEN NEW.tag_kind = 'plc'
    AND EXISTS (
        SELECT 1 FROM collection_groups g
        JOIN plc_connections c ON c.id = g.plc_connection_id
        WHERE g.id = NEW.collection_group_id AND c.protocol = 'hub'
    )
    AND EXISTS (
        SELECT 1 FROM tags t
        JOIN collection_groups g ON g.id = t.collection_group_id
        JOIN plc_connections c ON c.id = g.plc_connection_id
        WHERE c.protocol = 'hub' AND t.address = NEW.address AND t.id <> NEW.id
    )
BEGIN
    SELECT RAISE(ABORT, 'banto_tags: hub external name already registered');
END;
