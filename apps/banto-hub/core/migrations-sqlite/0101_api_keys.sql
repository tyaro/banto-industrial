-- T0-2 (docs/tag-server-design.md §5.6): /api/v1/* の機械クライアント認証用
-- API キー。列の意味は `crate::api_keys` のモジュール doc 参照（特に
-- `last_used_at`・`expires_at` は epoch ミリ秒の 10 進文字列で、
-- `created_at`/`revoked_at` の ISO 日時文字列とは形式が違う）。
-- 失効は物理削除でなく `revoked_at` を立てるだけ（履歴を残す方針）。
-- `tripped_at` は T2-4（§6-4「レート制限ブレーカ」）の解除可能なトリップ
-- 状態、`expires_at` は H10 ①（docs/improvement-plan.md）の任意の有効期限
-- （NULL = 無期限）。どちらも以前は後追いの ADD COLUMN だったものを、
-- I1（2026-10-02）で最終形の列として CREATE TABLE に含めた。
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
