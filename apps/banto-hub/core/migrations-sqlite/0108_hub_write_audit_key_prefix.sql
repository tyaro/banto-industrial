-- #713（2026-10-29 オーナー決定）: 失効した API キーを削除できるようにする。
-- 削除の後も書込み監査から「どのキーか」が読めるよう、名前のスナップショット
-- （`api_key_name_snapshot`）に並べてプレフィックスのスナップショットを持つ。
-- 値は書込み監査を挿入するときに `api_keys.prefix` から写す
-- （`crate::write_audit::WriteAuditService::insert_row`）。この列を足す前の
-- 行は NULL のまま（使用者がいない前提で、移行はしない）。
ALTER TABLE hub_write_audit ADD COLUMN api_key_prefix_snapshot TEXT;
