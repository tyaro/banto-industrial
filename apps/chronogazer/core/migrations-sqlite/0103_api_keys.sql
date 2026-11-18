-- #1161 PR4（docs/chronogazer-agent-api-design.md §3.1）: AI エージェントなどの
-- 機械クライアントが `/api/*` を使うための API キー。列は banto-hub の
-- `0101_api_keys.sql` と同じ（発行・照合の SQL は共有 crate `banto-api-keys` が
-- 一つだけ持つため）。列の意味は共有 crate の doc を参照（`last_used_at`・
-- `expires_at` は epoch ミリ秒の 10 進文字列、`created_at`/`revoked_at` は
-- `datetime('now')` の文字列）。失効は `revoked_at` を立てるだけで、失効した
-- キーだけ削除できる。`tripped_at`（一時停止）は ChronoGazer では使わないが、
-- 共有の SQL を一つにするため列は持つ。
CREATE TABLE api_keys (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  name TEXT NOT NULL UNIQUE,
  prefix TEXT NOT NULL UNIQUE,
  key_hash TEXT NOT NULL,
  scopes TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT (datetime('now')),
  last_used_at TEXT,
  revoked_at TEXT,
  tripped_at TEXT,
  expires_at TEXT
);
