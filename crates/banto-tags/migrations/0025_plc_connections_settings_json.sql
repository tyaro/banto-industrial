-- R2b-1d（docs/banto-hub-connection-settings-design.md §3.1・§3.2・§3.3・§6.2、
-- 2026-10-21 オーナー決定 Q1〜Q15）: plc_connections の固定列（host・port・
-- unit_id・word_order・cpu_type・database・username・password）を JSON の
-- `settings`（NOT NULL のオブジェクト）へ移し、`settings_version` を足し、
-- postgres の平文のパスワードを秘密の表 `plc_connection_secrets` へ退避する。
-- **戻せない移行**（新しい DB は 0001〜0024 しか知らない migrator では開けない。
-- 設計 §1.1・§6.4）。純粋な SQL なので ChronoGazer の DB にもそのまま当たる（§6.7。
-- ChronoGazer の行は modbus-tcp・slmp・hub だけで、秘密値は無い）。
--
-- ## 新しい列（§3.1）
--
-- `id`・`name`・`protocol`・`enabled`・`simulation`・`settings`・`settings_version`
-- だけ。`protocol` の `CHECK (protocol IN (...))` は撤去し、DriverId の文法
-- （`[a-z0-9][a-z0-9-]*`、長さ 1〜64）だけを残す。何が存在するかの正は Hub の
-- レジストリ（§4）、ChronoGazer は banto-tags の旧規則（`ALLOWED_PROTOCOLS`、
-- R2b-7 まで）。`word_order`・`cpu_type` の `CHECK` も撤去する（値の検証は
-- アプリの層とドライバ）。`settings` は `json_valid` かつオブジェクト。
--
-- ## プロトコルごとの写し方（§3.2。`CASE protocol`）
--
-- | protocol   | settings                                                         |
-- | ---------- | ---------------------------------------------------------------- |
-- | modbus-tcp | host・port・unit_id・word_order                                  |
-- | slmp       | host・port・word_order・cpu_type                                 |
-- | postgres   | host・port・database・username・password（参照、空・NULL は無し）|
-- | virtual    | {}                                                               |
-- | hub        | {}                                                               |
-- | push       | 今の settings をそのまま（NULL は {}）                           |
-- | その他     | {}（今の CHECK の下では存在しない）                              |
--
-- プロトコルが読まない列（SLMP の unit_id、Modbus の cpu_type、virtual・hub・push
-- の host ほか）は捨てる。捨てた値で既定と違うものは Hub の起動時の preflight が
-- 報告する（§6.3、Q9。移行は止めない）。postgres の database・username が NULL の
-- 行（手で書き換えたもの）は、`json_patch('{}', ...)` で NULL のキーを置かない
-- （RFC 7396 の merge patch は NULL のメンバーを足さない）。`push` の settings が
-- JSON のオブジェクトでない行（手で書き換えたもの）は新しい表の `CHECK` に落ち、
-- この移行ごと失敗する（黙って設定を捨てない。preflight の写しで分かる）。
--
-- ## 秘密値（§3.3・§5）
--
-- postgres の非空のパスワードごとに、先に参照（128 ビットの乱数の小文字 16 進
-- 32 文字）を `_m0025_refs` で決め、`settings.password = {"$secret": ref}` と、
-- `plc_connection_secrets` の行 `(ref, id, 'password', 'plain-v1',
-- CAST(password AS BLOB))` を書く。`plain-v1` は平文そのもので、守りは今の列と
-- 同じ（Windows の Hub は後の起動で `dpapi-machine-v1` に封印し直す、§6.5・R2b-1e）。
-- 秘密の表には FK を張らない（Q15。hub_sink_groups と同じ慣行。張ると、後で
-- plc_connections を作り直すときに退避する子の表が 1 つ増える）。削除の全入口が
-- 同じトランザクションの中で行を消す（banto-tags の `PlcConnectionService`）。
--
-- ## 手順（0023 と同じ park-and-restore）
--
-- 0004 の header の 3 つの前提（sqlx が 1 トランザクション・FOREIGN KEY 強制下で
-- 流す / 参照されている表の DROP は失敗する / 参照されている表の RENAME は子の
-- FK 定義ごと引きずる）はそのまま効いているので、子孫（collection_groups →
-- tags）の行を一時表へ退避して消し、plc_connections_new を作って中身を写し、
-- 旧表を DROP して新表を空いた名前へ RENAME し、退避した行を戻す。
-- collection_groups は 0022 の後の 7 列、tags は 0018 の後の 19 列（0023 と同じ
-- 列リスト）。
--
-- ## トリガ 6 本を先に外し、戻した後に同じ定義で作り直す
--
-- 0020 の 2 本（`tags_hub_external_name_unique_insert` / `_update`）と 0024 の
-- 4 本（`tags_push_key_unique_insert` / `_update`・
-- `collection_groups_push_key_unique_move`・
-- `plc_connections_push_key_unique_protocol`）。どれも本体が `plc_connections` を
-- 参照する。最後の 1 本は `plc_connections` 自身のトリガで表の DROP と一緒に
-- 消えるが、作り直しの対で読めるように明示して外す。定義は 0023（0020 の
-- 作り直し）と 0024 の文と一字一句同じ（テストが `sqlite_master.sql` を比べる）。
--
-- **後の migration で `tags`・`collection_groups`・`plc_connections` のどれかを
-- 作り直すときも、この 6 本を先に外し、行を戻した後に同じ定義で作り直すこと。**
--
-- ## 索引と AUTOINCREMENT
--
-- `idx_plc_connections_single_hub`（0019 の部分一意索引、`hub` は高々 1 行）は
-- 表ごと消えるので同じ定義で作り直す。collection_groups / tags は DROP しない
-- （DELETE して元の id で戻すだけ）ので、その索引と採番位置は動かない。
--
-- **`sqlite_sequence` の位置（大きい方）を新表へ写す**（0023 と同じ書き方、
-- ingest-design §5.1）。引き継がないと、削除した接続の id が新しい接続に再び
-- 付き、古い `ingest:#{id}` の API キーがそれに通ってしまう。**後の migration で
-- plc_connections を作り直すときも、同じ書き方で引き継ぎ、同じテストを足すこと。**
--
-- 検証は `banto_tags::plc_connection` 内の
-- `migration_0025_moves_columns_to_settings_and_secrets_on_a_populated_database`
-- （実際の migrator で 0024 まで流した populated なデータベースに、実ファイルを
-- `include_str!` で読み込み、sqlx と同じく 1 接続・1 トランザクションで適用する）。

DROP TRIGGER tags_hub_external_name_unique_insert;
DROP TRIGGER tags_hub_external_name_unique_update;
DROP TRIGGER tags_push_key_unique_insert;
DROP TRIGGER tags_push_key_unique_update;
DROP TRIGGER collection_groups_push_key_unique_move;
DROP TRIGGER plc_connections_push_key_unique_protocol;

-- 子孫を退避する（深い方から）。
CREATE TEMPORARY TABLE _m0025_tags AS SELECT * FROM tags;
CREATE TEMPORARY TABLE _m0025_collection_groups AS SELECT * FROM collection_groups;
DELETE FROM tags;
DELETE FROM collection_groups;

-- 秘密の参照を先に決める（1 行 1 回だけ乱数を引き、settings と秘密の表で同じ値を使う）。
CREATE TEMPORARY TABLE _m0025_refs AS
SELECT id, lower(hex(randomblob(16))) AS ref
  FROM plc_connections
 WHERE protocol = 'postgres' AND password IS NOT NULL AND password <> '';

CREATE TABLE plc_connections_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE,
    -- 何が存在するかの正はレジストリ（§4）。SQL は DriverId の文法だけを見る（Q14）。
    protocol TEXT NOT NULL
        CHECK (length(protocol) BETWEEN 1 AND 64
               AND protocol NOT GLOB '*[^a-z0-9-]*'
               AND substr(protocol, 1, 1) <> '-'),
    enabled INTEGER NOT NULL DEFAULT 1,
    simulation INTEGER NOT NULL DEFAULT 0,
    settings TEXT NOT NULL DEFAULT '{}'
        CHECK (json_valid(settings) AND json_type(settings) = 'object'),
    settings_version INTEGER NOT NULL DEFAULT 1 CHECK (settings_version >= 1)
);

INSERT INTO plc_connections_new (
    id, name, protocol, enabled, simulation, settings, settings_version
)
SELECT
    c.id, c.name, c.protocol, c.enabled, c.simulation,
    CASE c.protocol
        WHEN 'modbus-tcp' THEN json_object(
            'host', c.host, 'port', c.port, 'unit_id', c.unit_id, 'word_order', c.word_order)
        WHEN 'slmp' THEN json_object(
            'host', c.host, 'port', c.port, 'word_order', c.word_order, 'cpu_type', c.cpu_type)
        WHEN 'postgres' THEN CASE
            WHEN r.ref IS NULL THEN json_patch('{}', json_object(
                'host', c.host, 'port', c.port, 'database', c.database, 'username', c.username))
            ELSE json_set(
                json_patch('{}', json_object(
                    'host', c.host, 'port', c.port, 'database', c.database,
                    'username', c.username)),
                '$.password', json_object('$secret', r.ref))
            END
        WHEN 'virtual' THEN '{}'
        WHEN 'hub' THEN '{}'
        WHEN 'push' THEN COALESCE(c.settings, '{}')
        ELSE '{}'
    END,
    1
FROM plc_connections c
LEFT JOIN _m0025_refs r ON r.id = c.id;

-- 秘密の表（§3.1）。FK は張らない（Q15）。
CREATE TABLE plc_connection_secrets (
    ref TEXT PRIMARY KEY CHECK (length(ref) = 32 AND ref NOT GLOB '*[^0-9a-f]*'),
    connection_id INTEGER NOT NULL,
    key TEXT NOT NULL,
    scheme TEXT NOT NULL CHECK (scheme IN ('plain-v1', 'dpapi-machine-v1')),
    blob BLOB NOT NULL,
    UNIQUE (connection_id, key)
);

INSERT INTO plc_connection_secrets (ref, connection_id, key, scheme, blob)
SELECT r.ref, c.id, 'password', 'plain-v1', CAST(c.password AS BLOB)
  FROM _m0025_refs r
  JOIN plc_connections c ON c.id = r.id;

-- 採番位置を引き継ぐ（0018/0019/0022/0023 と同じ書き方）。
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
FROM _m0025_collection_groups;
INSERT INTO tags (
    id, name, collection_group_id, address, data_type, string_length,
    raw_lo, raw_hi, eng_lo, eng_hi, unit, decimals, enabled,
    writable, tag_kind, expression, retain, revision, string_encoding
)
SELECT
    id, name, collection_group_id, address, data_type, string_length,
    raw_lo, raw_hi, eng_lo, eng_hi, unit, decimals, enabled,
    writable, tag_kind, expression, retain, revision, string_encoding
FROM _m0025_tags;

DROP TABLE _m0025_tags;
DROP TABLE _m0025_collection_groups;
DROP TABLE _m0025_refs;

-- トリガ 6 本を同じ定義で作り直す（0020 の 2 本は 0023 の作り直しと、0024 の
-- 4 本は 0024 と一字一句同じ）。**後の migration で `tags`・
-- `collection_groups`・`plc_connections` のどれかを作り直すときも、この 6 本を
-- 作り直すこと。**
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

CREATE TRIGGER tags_push_key_unique_insert
BEFORE INSERT ON tags
WHEN NEW.tag_kind = 'plc'
    AND EXISTS (
        SELECT 1 FROM collection_groups g
        JOIN plc_connections c ON c.id = g.plc_connection_id
        WHERE g.id = NEW.collection_group_id AND c.protocol = 'push'
    )
    AND EXISTS (
        SELECT 1 FROM tags t
        JOIN collection_groups g ON g.id = t.collection_group_id
        WHERE t.tag_kind = 'plc' AND t.address = NEW.address
            AND g.plc_connection_id = (
                SELECT g2.plc_connection_id FROM collection_groups g2
                WHERE g2.id = NEW.collection_group_id
            )
    )
BEGIN
    SELECT RAISE(ABORT, 'banto_tags: push key already registered');
END;

CREATE TRIGGER tags_push_key_unique_update
BEFORE UPDATE OF address, collection_group_id, tag_kind ON tags
WHEN NEW.tag_kind = 'plc'
    AND EXISTS (
        SELECT 1 FROM collection_groups g
        JOIN plc_connections c ON c.id = g.plc_connection_id
        WHERE g.id = NEW.collection_group_id AND c.protocol = 'push'
    )
    AND EXISTS (
        SELECT 1 FROM tags t
        JOIN collection_groups g ON g.id = t.collection_group_id
        WHERE t.tag_kind = 'plc' AND t.address = NEW.address AND t.id <> NEW.id
            AND g.plc_connection_id = (
                SELECT g2.plc_connection_id FROM collection_groups g2
                WHERE g2.id = NEW.collection_group_id
            )
    )
BEGIN
    SELECT RAISE(ABORT, 'banto_tags: push key already registered');
END;

CREATE TRIGGER collection_groups_push_key_unique_move
BEFORE UPDATE OF plc_connection_id ON collection_groups
WHEN NEW.plc_connection_id IS NOT OLD.plc_connection_id
    AND EXISTS (
        SELECT 1 FROM plc_connections c
        WHERE c.id = NEW.plc_connection_id AND c.protocol = 'push'
    )
    AND EXISTS (
        SELECT 1 FROM tags t
        JOIN tags o ON o.address = t.address AND o.id <> t.id AND o.tag_kind = 'plc'
        JOIN collection_groups og ON og.id = o.collection_group_id
        WHERE t.collection_group_id = NEW.id AND t.tag_kind = 'plc'
            AND (og.id = NEW.id OR og.plc_connection_id = NEW.plc_connection_id)
    )
BEGIN
    SELECT RAISE(ABORT, 'banto_tags: push key already registered');
END;

CREATE TRIGGER plc_connections_push_key_unique_protocol
BEFORE UPDATE OF protocol ON plc_connections
WHEN NEW.protocol = 'push' AND OLD.protocol <> 'push'
    AND EXISTS (
        SELECT 1 FROM tags t
        JOIN collection_groups g ON g.id = t.collection_group_id
        JOIN tags o ON o.address = t.address AND o.id <> t.id AND o.tag_kind = 'plc'
        JOIN collection_groups og ON og.id = o.collection_group_id
        WHERE t.tag_kind = 'plc' AND g.plc_connection_id = NEW.id
            AND og.plc_connection_id = NEW.id
    )
BEGIN
    SELECT RAISE(ABORT, 'banto_tags: push key already registered');
END;
