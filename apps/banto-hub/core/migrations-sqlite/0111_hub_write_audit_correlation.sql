-- #1201: 単票の書込みの監査に、呼び出し側が付けた突き合わせ用の ID と、名乗った
-- 操作者を足す（docs/hub/tag-server-design.md §6.6）。
--
-- - `correlation_id`: 呼び出し側の要求に付けた ID（1〜64 文字、`[A-Za-z0-9._:-]`）。
--   監査の `id` とは別に、呼び出し側が自分の記録と突き合わせるために使う。
-- - `operator_claim`: `on_behalf_of`（1〜128 文字）。**自己申告で、認証された事実ではない**。
--   認証された主体は `api_key_*` の列。
--
-- どちらも NULL 可（0111 より前の行・バッチの書込み・申告の無い要求は NULL）。
-- 0107・0109 と違い CHECK を変えないので、表は作り直さず列を足すだけ。
ALTER TABLE hub_write_audit ADD COLUMN correlation_id TEXT;
ALTER TABLE hub_write_audit ADD COLUMN operator_claim TEXT;
