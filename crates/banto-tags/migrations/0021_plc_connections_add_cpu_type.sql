-- #605: `plc_connections.cpu_type` を追加し、SLMP の CPU 種別（Q シリーズ /
-- iQ-R）を接続単位で指定できるようにする。これまで broker・収集・接続テストが
-- `SlmpConfig { ..Default::default() }` の CPU（`SlmpCpu::R`）固定でフレームを
-- 組んでいた。Q シリーズ（QJ71E71-100 の 4E バイナリ）はデバイス番地が 4 バイト、
-- サブコマンドが 0x0000/0x0001 で、R の 6 バイト・0x0002/0x0003 とは別形式のため、
-- 接続できても全タグが Bad になる。
--
-- 既定 'R' は `SlmpConfig::default().cpu`（`SlmpCpu::R`）と一致させる。既存行と、
-- この列を知らないクライアントが作る行は、今までと同じ R フレームのままになる。
-- SLMP 以外のプロトコルでは収集も broker もこの列を読まない（`word_order` が
-- virtual 行で無意味なまま必須列になっているのと同じ扱い）。
--
-- `word_order`（migration 0010）と同じ理由でプレーンな ADD COLUMN にする。
-- 追加する CHECK は新規列自身しか参照しないので、SQLite の
-- 「ADD COLUMN の CHECK は新規列のみを参照する場合に限る」に収まる。

ALTER TABLE plc_connections
    ADD COLUMN cpu_type TEXT NOT NULL DEFAULT 'R'
    CHECK (cpu_type IN ('Q', 'R'));
