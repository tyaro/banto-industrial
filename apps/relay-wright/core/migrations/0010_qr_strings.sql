-- QR文字列リスト（デバッグ支援）: タッチパネル（HMI）のQRリーダーに読ませる
-- 文字列を登録しておき、QRコード画面（/qr-codes）で SVG として並べて表示する
-- ためのテーブル。PLC 書き込み経路とは完全に独立したデバッグ用ユーティリティ
-- で、他テーブルへの参照は持たない。
--
-- `sort_order` は画面上の並び順（リストで並べられる要件）。並び替えは
-- `PUT /api/qr-strings/reorder`（Tauri: `qr_strings_reorder`）が ids 配列の
-- 添字で一括更新する。新規作成時は MAX(sort_order)+1 で末尾に追加される。
--
-- NOTE: このファイルは他の migrations/*.sql と同様「スキーマのドキュメント」
-- であり、`sqlx::migrate!` からは実行されない。実体は
-- `core/src/db.rs::apply_app_schema` の冪等DDL（両者を手で同期すること —
-- db.rs のモジュールコメント参照）。
CREATE TABLE IF NOT EXISTS qr_strings (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    -- 表示名（任意）。QRタイルの下に添えるラベル。
    label TEXT NOT NULL DEFAULT '',
    -- QRコードにする文字列（必須・非空。長さ上限はサービス層で検証）。
    text TEXT NOT NULL,
    sort_order INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_qr_strings_sort_order ON qr_strings (sort_order);
