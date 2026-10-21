-- R2a-2a（docs/banto-hub-ingest-design.md §3.1・§3.3）: `push` 接続の下の `plc`
-- タグの `address`（送り手が決めるキー）は**接続ごとに**一意。1 つのキーが 2 本の
-- タグに割れると、送られた値がどちらに入ったか分からない。接続が違えば同じ
-- キーを使える（`hub` の外部名がレジストリ全体で一意なのとはここが違う）。
--
-- 形は 0020（Hub の外部名）と同じ: `tags` は接続ではなくグループを参照するので、
-- UNIQUE 索引でも部分索引でも「同じ `push` 接続の下だけで一意」は表せない。
-- アプリ層（`src/tag.rs` の `validate_tag_kind_placement[_tx]`）が書き込みの前に
-- 同じ規則を調べて読みやすいエラーを返し（バッチは全行分）、このトリガは
-- 調べてから書くまでの間に割り込まれたときの最後の砦。断ったときの文言は
-- `src/tag.rs` の `PUSH_KEY_TRIGGER_MESSAGE` と同じで、`map_tag_write_error` が
-- `address` の検証エラーに載せ替える。
--
-- 0020 との違いは条件だけ: `c.protocol = 'hub'` を `'push'` に替え、既存の行の
-- 側を「新しい行と同じ `plc_connection_id`」に絞る。キーは大文字小文字を区別し、
-- そのまま比べる（`=` は BINARY 照合。保存前に前後の空白は除いてある）。
-- 既存の行は検査しない（トリガは既存の重複を消さない。0023 までに `push` 接続は
-- 作れなかったので、既存の重複も無い）。
--
-- ## 親が変わる経路（PR #753 のレビュー P2）
--
-- タグの行を書かずに、タグの属する接続が変わる経路が 2 つある: グループの
-- `plc_connection_id` の更新（別の接続へ移す）と、接続の `protocol` の更新
-- （`push` へ変える）。どちらも上の 2 本（`tags` の INSERT/UPDATE）では発火しない
-- ので、同じ規則を親の側でも見る 2 本を足す:
--
-- * `collection_groups_push_key_unique_move`: グループを `push` 接続へ移すとき、
--   移すグループの `plc` タグのキーが、移動先の接続の他のグループのタグ、または
--   同じグループの別のタグと重なれば断る。
-- * `plc_connections_push_key_unique_protocol`: 接続を `push` 以外から `push` へ
--   変えるとき、その接続の `plc` タグの中でキーが重なれば断る。
--
-- アプリ層（`src/collection_group.rs` の `push_group_move_error`、
-- `src/plc_connection.rs` の `push_protocol_switch_error`）は、タグを持つグループの
-- `push` へ・からの移動と、タグを持つ接続の `push` へ・からの変更をそもそも断る
-- （`string`・書き込み可・スケーリングといった `push_tag_errors` の規則も持ち込ませない
-- ため）。この 2 本はその検査を通らない書き込みに対する一意性の最後の砦。
--
-- **後の migration で `tags`・`collection_groups`・`plc_connections` のどれかを
-- 作り直す（DROP → RENAME、park-and-restore）ときは、0020 のトリガと一緒に
-- このファイルのトリガも先に外し、行を戻した後に作り直すこと**（0022・0023 の
-- header）。`collection_groups`・`plc_connections` を DROP するとその表のトリガも
-- 消える。
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
