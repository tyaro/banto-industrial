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
-- **後の migration で `tags`・`collection_groups`・`plc_connections` のどれかを
-- 作り直す（DROP → RENAME、park-and-restore）ときは、0020 のトリガと一緒に
-- このトリガも先に外し、行を戻した後に作り直すこと**（0022・0023 の header）。
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
