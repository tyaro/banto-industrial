-- #383 段階3 P2（2026-10-10 オーナー決定 H1「Hub を接続種別として表す」）:
-- plc_connections.protocol の CHECK に 'hub' を追加し、'hub' 行を
-- レジストリ全体で高々 1 行に制限する部分一意索引を足す。
--
-- 'hub' は banto-hub（タグサーバー）から値を受ける接続を表す。Hub 由来の
-- タグは普通の `tags` 行で、`address` は Hub 側のタグの**外部名**
-- （`接続名.グループ名.タグ名`、識別子は外部名 - scada-design.md §9.6）。
-- 接続先とキーは ChronoGazer の設定 `hub.record` が持ち、この行には持たない
-- （host/port/unit_id/word_order/simulation は application 層で既定値に
-- 正規化される - `src/plc_connection.rs` の "`\"hub\"`" 節）。Hub は 1 つ
-- なので 'hub' 行も 1 行まで（下の `idx_plc_connections_single_hub`）。
-- 外部名・データ型・writable の規則は `src/tag.rs` の配置検証が担う。
--
-- ## 手順（0014 と同じ park-and-restore）
--
-- SQLite は CHECK を ALTER できないので plc_connections を作り直す。
-- 0004 の header が確立した 3 つの前提（sqlx が 1 トランザクション・FOREIGN
-- KEY 強制下で流す / 参照されている表の DROP は失敗する / 参照されている
-- 表の RENAME は子の FK 定義ごと引きずる）はそのまま効いているので、
-- 0007/0014 と同じく子孫（collection_groups → tags）の行を一時表へ退避して
-- 消し、plc_connections_new を作って中身を写し、旧表を DROP して新表を
-- 空いた名前へ RENAME し、退避した行を戻す。
--
-- 退避・復元する列は**現在の全列**: collection_groups は 0012 の
-- default_writable と 0015 の query_sql を含む 7 列、tags は 0018 で
-- しきい値 4 列を外した後の 19 列。古い列リストに戻すと既存行を静かに
-- 切り詰める（0007/0011/0014/0015/0016/0018 の header の警告と同じ）。
--
-- ## AUTOINCREMENT の採番位置を引き継ぐ（0018 と同じ）
--
-- 0004/0007/0014 は plc_connections の採番位置を引き継がなかった（0004 の
-- header 末尾「harmless」）。しかし今は接続 ID を FK 無しで指す表がある
-- （banto-hub の sink 設定ほか）ので、0018 が tags に対して行ったのと同じく
-- `sqlite_sequence` の旧い位置（大きい方）を新表へ写す。`DROP TABLE
-- plc_connections` で旧表の行は `sqlite_sequence` から消え、`RENAME` で
-- 新表の行が `plc_connections` になる。
--
-- collection_groups / tags は DROP しない（DELETE して戻すだけ）ので、
-- 両者の `sqlite_sequence` の行は動かず、元の id で戻した行が採番位置を
-- 下げることもない。索引 `idx_collection_groups_plc_connection_id` /
-- `idx_tags_collection_group_id` も表ごと残るので作り直しは要らない。
-- plc_connections 自身の索引は `name` の UNIQUE（列定義に含まれる自動索引）
-- と、今回足す部分一意索引だけ。トリガ・ビューはどの表にも無い。
--
-- 検証は `banto_tags::plc_connection` 内の
-- `migration_0019_preserves_rows_sequence_and_foreign_keys_on_a_populated_database`
-- （populated なデータベースに対して実ファイルを `include_str!` で読み込み、
-- sqlx と同じく 1 接続・1 トランザクションで適用する）。

-- 子孫を退避する（深い方から）。
CREATE TEMPORARY TABLE _m0019_tags AS SELECT * FROM tags;
CREATE TEMPORARY TABLE _m0019_collection_groups AS SELECT * FROM collection_groups;
DELETE FROM tags;
DELETE FROM collection_groups;

-- CHECK を広げた plc_connections を作る（列構成・順序は 0014 のまま）。
CREATE TABLE plc_connections_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE,
    protocol TEXT NOT NULL DEFAULT 'modbus-tcp'
        CHECK (protocol IN ('modbus-tcp', 'slmp', 'virtual', 'postgres', 'hub')),
    host TEXT NOT NULL,
    port INTEGER NOT NULL,
    unit_id INTEGER NOT NULL DEFAULT 1,
    enabled INTEGER NOT NULL DEFAULT 1,
    simulation INTEGER NOT NULL DEFAULT 0,
    word_order TEXT NOT NULL DEFAULT 'low_high' CHECK (word_order IN ('low_high', 'high_low')),
    database TEXT,
    username TEXT,
    password TEXT
);

INSERT INTO plc_connections_new (
    id, name, protocol, host, port, unit_id, enabled, simulation, word_order,
    database, username, password
)
SELECT
    id, name, protocol, host, port, unit_id, enabled, simulation, word_order,
    database, username, password
FROM plc_connections;

-- 採番位置を引き継ぐ（0018 と同じ書き方）。
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

-- 'hub' 行は高々 1 行（Hub は 1 つ、H1）。部分一意索引なので他の
-- プロトコルの行は何行でも置ける。違反は application 層
-- （`src/plc_connection.rs` の `map_connection_write_error`）が
-- `protocol` の項目エラーに言い換える。
CREATE UNIQUE INDEX idx_plc_connections_single_hub
    ON plc_connections (protocol) WHERE protocol = 'hub';

-- 子孫を戻す（浅い方から）。現在の全列。
INSERT INTO collection_groups (
    id, name, plc_connection_id, period_ms, enabled, default_writable, query_sql
)
SELECT id, name, plc_connection_id, period_ms, enabled, default_writable, query_sql
FROM _m0019_collection_groups;
INSERT INTO tags (
    id, name, collection_group_id, address, data_type, string_length,
    raw_lo, raw_hi, eng_lo, eng_hi, unit, decimals, enabled,
    writable, tag_kind, expression, retain, revision, string_encoding
)
SELECT
    id, name, collection_group_id, address, data_type, string_length,
    raw_lo, raw_hi, eng_lo, eng_hi, unit, decimals, enabled,
    writable, tag_kind, expression, retain, revision, string_encoding
FROM _m0019_tags;

DROP TABLE _m0019_tags;
DROP TABLE _m0019_collection_groups;
