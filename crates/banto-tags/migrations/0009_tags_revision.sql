-- T18-1 TAG-UX-C (docs/banto-hub-desktop-plan.md §9.4「revision / ETag で
-- 後勝ち上書きを防ぎ、競合時は差分を表示する」): 楽観的ロック用の行
-- バージョン。新規行は 1 から始まり、`banto_tags::tag::TagService::update`/
-- `update_tx` が成功する度に必ず +1 される（`expected_revision` を指定
-- しない呼び出しでも増分する - 「チェックしない」と「増やさない」は別）。
--
-- 0006 と同じ理由でプレーンな `ADD COLUMN` 1本にしている: `tags` は既に
-- 再構築を要する `CHECK` 変更を持たず、`revision` 自体にも `CHECK` は要らない
-- （`enabled`/`writable`/`retain` と同じ単純な整数カウンタで、許容範囲の
-- 制約は無い）。既存行は `DEFAULT 1` を得るので後方互換 - relay-wright 等の
-- 既存クライアントは `expectedRevision` を送らない限りこの列の存在を意識
-- する必要がない。

ALTER TABLE tags ADD COLUMN revision INTEGER NOT NULL DEFAULT 1;
