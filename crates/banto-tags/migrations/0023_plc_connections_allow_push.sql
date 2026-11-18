-- R2a-2a（docs/banto-hub-ingest-design.md §3.1・§3.2・§5.1、2026-10-21 オーナー決定
-- Q6 = (a)「1 回の再構築で `push` と `settings` 列」）: plc_connections.protocol の
-- CHECK に 'push' を足し、同じ再構築で列 `settings`（JSON、NULL 可）を足す。
--
-- 'push' は、外の送り手（DB ブリッジ・スクリプト）が banto-hub へ値を**送り込む**
-- 接続を表す。Hub が繋ぎに行く先は無いので host/port などは application 層で
-- 既定値に正規化される（`src/plc_connection.rs` の "`\"push\"`" 節）。数に上限は
-- 無い（'hub' の部分一意索引のようなものは付けない - 1 接続 = 送り手 1 つ）。
-- タグの `address` は送り手が決める不透明なキーで、接続の中で一意（0024 のトリガ）。
--
-- `settings` は `push` 接続の設定（`stale_timeout_ms`・ブリッジの区画）の置き場。
-- `push` 以外では NULL のまま（application 層の `validate_push_settings` 側が断る。
-- R2b-1 がこの列を全プロトコルの設定の置き場に広げる）。中身の検証（未知のキー・
-- 大きさ 64 KiB・値の範囲・ブリッジの量の見積もり）は application 層が持ち、
-- SQL は `json_valid` だけを見る（壊れた JSON の行を作らない最後の砦）。
--
-- ## 手順（0019 と同じ park-and-restore）
--
-- SQLite は CHECK を ALTER できないので plc_connections を作り直す。0004 の
-- header が確立した 3 つの前提（sqlx が 1 トランザクション・FOREIGN KEY 強制下で
-- 流す / 参照されている表の DROP は失敗する / 参照されている表の RENAME は子の
-- FK 定義ごと引きずる）はそのまま効いているので、子孫（collection_groups →
-- tags）の行を一時表へ退避して消し、plc_connections_new を作って中身を写し、
-- 旧表を DROP して新表を空いた名前へ RENAME し、退避した行を戻す。
--
-- 写す列は**今の全列を物理的な順序のまま**: 0019 の 12 列と、0021 が ADD COLUMN で
-- 末尾に足した `cpu_type`（13 列目）。新しい `settings` はその後ろ。`cpu_type` を
-- 列リストから落とすと、全行が列既定の 'R' に戻り、Q シリーズの接続が黙って
-- R フレームで繋ぎに行く（0021 の header）。collection_groups は 0022 の後の
-- 7 列、tags は 0018 の後の 19 列（0019・0022 と同じ列リスト）。古い列リストに
-- 戻すと既存行を静かに切り詰める（0007/0011/0014/0015/0016/0018/0019/0022 の
-- header の警告と同じ）。
--
-- ## トリガ（0020）を先に外し、戻した後に作り直す（0022 と同じ）
--
-- `tags` の 0020 のトリガ 2 本は本体が `plc_connections` を参照する。参照先の表を
-- DROP → RENAME する間に、トリガの本体が存在しない表を指す状態を作らない
-- （SQLite 3.26 以降の RENAME は schema 全体を検査し、壊れたトリガがあると
-- 失敗する）。また退避した行を戻す INSERT で断られては困る（既存の重複は
-- トリガが検査しない約束、0020 の header）。よって最初に DROP し、tags の行を
-- 戻した後に 0020 と同じ定義で作り直す。
--
-- ## 索引と AUTOINCREMENT（0019 と同じ）
--
-- plc_connections の索引は `name` の UNIQUE（列定義に含まれる自動索引）と
-- 0019 の部分一意索引 `idx_plc_connections_single_hub` だけ。後者は表ごと消える
-- ので元の名前・定義で作り直す。collection_groups / tags は DROP しない（DELETE
-- して元の id で戻すだけ）ので、`idx_collection_groups_plc_connection_id`・
-- `idx_tags_collection_group_id`・0022 の複合 UNIQUE の自動索引はそのまま残り、
-- 両者の `sqlite_sequence` の行も動かない。ビューはどの表にも無い。
--
-- **`sqlite_sequence` の位置（大きい方）を新表へ写す**。`ingest:#{id}` の API
-- キーは接続を id で縛る（Q9 = (b)）ので、作り直しで高水位が残った行の最大 id に
-- 戻ると、削除した接続の id が新しい接続に再び付き、古いキーがそれに通って
-- しまう（ingest-design §5.1）。**後の migration（R2b-1 ほか）で plc_connections を
-- 作り直すときも、同じ書き方で引き継ぎ、同じテストを足すこと。**
--
-- 検証は `banto_tags::plc_connection` 内の
-- `migration_0023_and_0024_preserve_rows_sequence_triggers_and_indexes_on_a_populated_database`
-- （実際の migrator で 0022 まで流した populated なデータベースに、実ファイルを
-- `include_str!` で読み込み、sqlx と同じく 1 接続・1 トランザクションで適用する）。

DROP TRIGGER tags_hub_external_name_unique_insert;
DROP TRIGGER tags_hub_external_name_unique_update;

-- 子孫を退避する（深い方から）。
CREATE TEMPORARY TABLE _m0023_tags AS SELECT * FROM tags;
CREATE TEMPORARY TABLE _m0023_collection_groups AS SELECT * FROM collection_groups;
DELETE FROM tags;
DELETE FROM collection_groups;

-- CHECK を広げ、settings を足した plc_connections を作る（既存の列の構成・順序は
-- 0021 の後のまま）。
CREATE TABLE plc_connections_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE,
    protocol TEXT NOT NULL DEFAULT 'modbus-tcp'
        CHECK (protocol IN ('modbus-tcp', 'slmp', 'virtual', 'postgres', 'hub', 'push')),
    host TEXT NOT NULL,
    port INTEGER NOT NULL,
    unit_id INTEGER NOT NULL DEFAULT 1,
    enabled INTEGER NOT NULL DEFAULT 1,
    simulation INTEGER NOT NULL DEFAULT 0,
    word_order TEXT NOT NULL DEFAULT 'low_high' CHECK (word_order IN ('low_high', 'high_low')),
    database TEXT,
    username TEXT,
    password TEXT,
    cpu_type TEXT NOT NULL DEFAULT 'R' CHECK (cpu_type IN ('Q', 'R')),
    settings TEXT CHECK (settings IS NULL OR json_valid(settings))
);

INSERT INTO plc_connections_new (
    id, name, protocol, host, port, unit_id, enabled, simulation, word_order,
    database, username, password, cpu_type
)
SELECT
    id, name, protocol, host, port, unit_id, enabled, simulation, word_order,
    database, username, password, cpu_type
FROM plc_connections;

-- 採番位置を引き継ぐ（0018/0019/0022 と同じ書き方）。
INSERT INTO sqlite_sequence (name, seq)
SELECT 'plc_connections_new', seq FROM sqlite_sequence
 WHERE name = 'plc_connections'
   AND NOT EXISTS (SELECT 1 FROM sqlite_sequence WHERE name = 'plc_connections_new');
UPDATE sqlite_sequence
   SET seq = (SELECT MAX(seq) FROM sqlite_sequence
               WHERE name IN ('plc_connections', 'plc_connections_new'))
 WHERE name = 'plc_connections_new';

DROP TABLE plc_connections;
ALTER TABLE plc_connections_new RENAME TO plc_connections;

-- 0019 の部分一意索引を同じ定義で作り直す（'hub' 行は高々 1 行）。
CREATE UNIQUE INDEX idx_plc_connections_single_hub
    ON plc_connections (protocol) WHERE protocol = 'hub';

-- 子孫を戻す（浅い方から）。現在の全列。
INSERT INTO collection_groups (
    id, name, plc_connection_id, period_ms, enabled, default_writable, query_sql
)
SELECT id, name, plc_connection_id, period_ms, enabled, default_writable, query_sql
FROM _m0023_collection_groups;
INSERT INTO tags (
    id, name, collection_group_id, address, data_type, string_length,
    raw_lo, raw_hi, eng_lo, eng_hi, unit, decimals, enabled,
    writable, tag_kind, expression, retain, revision, string_encoding
)
SELECT
    id, name, collection_group_id, address, data_type, string_length,
    raw_lo, raw_hi, eng_lo, eng_hi, unit, decimals, enabled,
    writable, tag_kind, expression, retain, revision, string_encoding
FROM _m0023_tags;

DROP TABLE _m0023_tags;
DROP TABLE _m0023_collection_groups;

-- 0020 のトリガを同じ定義で作り直す。**後の migration で `tags`・
-- `collection_groups`・`plc_connections` のどれかを作り直すときも、このトリガ
-- （と 0024 のトリガ）を作り直すこと。**
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
