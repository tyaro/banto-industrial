-- #1253: 下のログのパネル（`GET /api/log-feed`、`crate::rest::log_feed`）が、
-- `hub_write_audit` の「変わった順」を続きの位置にできるよう、変更の番号
-- `change_seq` を足す（docs/hub/banto-hub-ui-improvement-proposal.md
-- §下のログのパネル）。
--
-- 書き込みの監査は log-before-write で、先に暫定の `failed` を挿入し、結果が出たら
-- 同じ `id` の `result`・`detail` を更新する（`WriteAuditService::set_result`）。拒否の
-- 行も数を `detail` へ書き足す。`id` や発生の時刻を位置にすると、この更新を
-- 取りこぼすので、挿入と `result`・`detail` の更新のたびに表の中で単調に増える
-- 番号を行に持たせる。
--
-- ## トリガで入れる理由（Rust の SQL で入れない）
--
-- 挿入・更新の経路が 4 つ以上ある（`insert_row_in`・`insert_rejected_row`・拒否の数の
-- `UPDATE ... SET detail`・`set_result`）うえ、試験は表へ直に `INSERT` する。Rust の側で
-- 入れると、経路を足すたびに入れ忘れが起き、入れ忘れた行は feed から消える（気づけない）。
-- トリガなら、どの経路の書き込みでも番号が進み、既存の書き込みの経路の SQL を変えずに済む。
--
-- 表を作り直す migration（0107・0109 の型）は `DROP TABLE` でトリガも消す。そのときは
-- 作り直しの中で `change_seq` の列と値を写し、下の 2 つのトリガと索引を作り直すこと。
-- 消えたままだと、`crate::db` の試験 `the_change_seq_triggers_survive_on_the_latest_schema`
-- （最新の schema でトリガが有り、挿入と更新で番号が進むことを見る）が落ちる。
--
-- ## 再帰しないこと
--
-- 更新のトリガは `AFTER UPDATE OF result, detail` で、中の `UPDATE` は `change_seq`
-- だけを変えるので、自分を再び起こさない（SQLite の `UPDATE OF` は SET に挙げた列で
-- 判定する。`recursive_triggers` も既定で無効）。挿入のトリガの中の `UPDATE` も
-- `change_seq` だけなので、更新のトリガを起こさない。
--
-- 書き手は SQLite で 1 つなので、`MAX(change_seq) + 1` は並行で重ならない。重なれば
-- 一意の索引が断る（黙って同じ番号にならない）。
ALTER TABLE hub_write_audit ADD COLUMN change_seq INTEGER;

-- 既存の行は挿入の順（`id`）で埋める。以後の番号は `MAX(change_seq) + 1` から続く。
UPDATE hub_write_audit SET change_seq = id;

CREATE UNIQUE INDEX idx_hub_write_audit_change_seq ON hub_write_audit(change_seq);

CREATE TRIGGER trg_hub_write_audit_change_seq_insert
AFTER INSERT ON hub_write_audit
BEGIN
  UPDATE hub_write_audit
     SET change_seq = (SELECT COALESCE(MAX(change_seq), 0) + 1 FROM hub_write_audit)
   WHERE id = NEW.id;
END;

CREATE TRIGGER trg_hub_write_audit_change_seq_update
AFTER UPDATE OF result, detail ON hub_write_audit
BEGIN
  UPDATE hub_write_audit
     SET change_seq = (SELECT COALESCE(MAX(change_seq), 0) + 1 FROM hub_write_audit)
   WHERE id = NEW.id;
END;

-- 受け入れの切断（`audit_log` の `origin = 'ingest' AND action = 'ingest_disconnected'`）
-- だけを読む口（管理者でない利用者の feed）のための部分索引。操作の監査の行が多くても、
-- 続きの位置からの範囲と先頭（`MAX(id)`）を索引だけで引ける。
CREATE INDEX idx_audit_log_ingest_disconnected ON audit_log(id)
  WHERE origin = 'ingest' AND action = 'ingest_disconnected';
