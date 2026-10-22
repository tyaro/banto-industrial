-- R2b-1e（docs/banto-hub-connection-settings-design.md §5.3・§6.5、PR #778 の
-- レビュー P2）: `plain-v1` の秘密値の行（blob = 平文そのもの）を消す・書き換える
-- 書き込みは、平文を SQLite の空きページと WAL に追い出す。その書き込みと**同じ
-- トランザクションで**「清掃が要る」を記録し、Hub の起動時の後処理（§6.5 の 2）が
-- それを見て `VACUUM` する。
--
-- 書き込みの入口ごとに記録させる（作成・更新の `Set`・`Clear`・プロトコルの変更・
-- 削除・一括削除・`*_tx`・Hub の起動時の整合の確認が消す参照の無い行・手で流した
-- SQL）のは漏れやすいので、表のトリガで拾う。トリガは文の中で動くので、記録は
-- 必ず書き込みと同じトランザクションに入り、書き込みが戻れば記録も戻る。
--
-- * `plc_connection_secrets_plain_deleted`: `scheme = 'plain-v1'` の行の DELETE。
-- * `plc_connection_secrets_plain_replaced`: `scheme = 'plain-v1'` の行の `blob` か
--   `scheme` を変える UPDATE（Hub の封印し直し `plain-v1` → `dpapi-machine-v1` も
--   これに当たる。Hub はそのとき自分の印も立てるので、二重になるだけで害は無い）。
--
-- `dpapi-machine-v1` の行（暗号文）の削除・書き換えは記録しない。秘密の行の書き込みは
-- INSERT と DELETE と UPDATE だけで、`INSERT OR REPLACE`（DELETE のトリガが
-- `recursive_triggers` の無い既定では動かない）は使わない。
--
-- 記録の表 `plc_connection_secrets_cleanup` は 1 行だけ（`id = 1`）で、`pending` は
-- 記録の回数（0 = 清掃は要らない）。Hub の `upgrade::vacuum_if_pending` が Hub の
-- `settings` 表の `secrets.vacuum_pending` と一緒に読み、`VACUUM` と WAL の切り詰めが
-- 済んだ後に、読んだときの値のままなら 0 に戻す（間に増えていれば残し、次の起動で
-- もう一度清掃する）。この crate は Hub に依存できないので、記録の置き場はこの crate の
-- 表にし、Hub の `settings` 表には書かない。
--
-- ChronoGazer の DB にも同じ表とトリガができるが、ChronoGazer は秘密値を持たない
-- （秘密の行が無い）のでトリガは動かず、清掃もしない（害は無い）。
--
-- 記録の行が無くても（手で消した DB）トリガの中で作り直す（`INSERT ... SELECT
-- ... WHERE NOT EXISTS`。衝突しないので、外側の文の衝突の扱いに左右されない）。
--
-- **後の migration で `plc_connection_secrets` を作り直す（DROP → RENAME）ときは、
-- この 2 本のトリガも同じ定義で作り直すこと**（表を DROP するとトリガも消える）。
-- 追加だけの migration（0025 と同じ R2b-1 の未リリースの束）で、戻せないのは 0025。

CREATE TABLE plc_connection_secrets_cleanup (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    pending INTEGER NOT NULL DEFAULT 0 CHECK (pending >= 0)
);

INSERT INTO plc_connection_secrets_cleanup (id, pending) VALUES (1, 0);

CREATE TRIGGER plc_connection_secrets_plain_deleted
AFTER DELETE ON plc_connection_secrets
WHEN OLD.scheme = 'plain-v1'
BEGIN
    INSERT INTO plc_connection_secrets_cleanup (id, pending)
        SELECT 1, 0
         WHERE NOT EXISTS (SELECT 1 FROM plc_connection_secrets_cleanup WHERE id = 1);
    UPDATE plc_connection_secrets_cleanup SET pending = pending + 1 WHERE id = 1;
END;

CREATE TRIGGER plc_connection_secrets_plain_replaced
AFTER UPDATE OF blob, scheme ON plc_connection_secrets
WHEN OLD.scheme = 'plain-v1'
 AND (NEW.scheme IS NOT OLD.scheme OR NEW.blob IS NOT OLD.blob)
BEGIN
    INSERT INTO plc_connection_secrets_cleanup (id, pending)
        SELECT 1, 0
         WHERE NOT EXISTS (SELECT 1 FROM plc_connection_secrets_cleanup WHERE id = 1);
    UPDATE plc_connection_secrets_cleanup SET pending = pending + 1 WHERE id = 1;
END;
