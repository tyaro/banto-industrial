-- #383 段階3 P4（2026-10-11、Codex の指摘）: Hub 経由のタグの外部名は
-- レジストリ全体で一意（同じ Hub のタグを 2 本のタグにしない - 記録・しきい値・
-- ペンが別々のタグ ID に割れる）。
--
-- `tags` は接続ではなくグループを参照するので、「`hub` 接続の下のタグだけで
-- `address` が一意」は UNIQUE 索引でも部分索引でも表せない（部分索引の WHERE は
-- 同じ行の列しか見られない）。表の作り直しで接続の種別を `tags` に持たせる案は、
-- 種別とグループの所属を二重に持つことになり、0019 の付け替えの規則と食い違う
-- 余地を作るので採らない。代わりにトリガで、書き込みの**文そのもの**の中で断る:
--
-- * アプリ層（`src/tag.rs` の `validate_tag_kind_placement[_tx]`）が書き込みの前に
--   同じ規則を調べて、行ごと・項目ごとの読みやすいエラーを返す（バッチは全行分）。
-- * このトリガは、調べてから書くまでの間に別の書き込みが割り込んだときの最後の砦
--   （素のプールの `create`/`update` は検査と書き込みが別の文）。断ったときの文言は
--   `src/tag.rs` の `HUB_EXTERNAL_NAME_TRIGGER_MESSAGE` と同じで、`map_tag_write_error`
--   が `address` の検証エラーに載せ替える。
--
-- 対象は `hub` 接続の下の `plc` タグだけ（banto-hub は `hub` 接続を作らないので
-- 何もしない）。既存の行は検査しない（トリガは既存の重複を消さない）。
--
-- **後の migration で `tags` を作り直す（DROP → RENAME、park-and-restore）ときは、
-- このトリガも作り直すこと**（表を DROP するとトリガも消える。退避した行を戻す
-- 間はトリガが無い方がよいので、戻した後に作る）。
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
