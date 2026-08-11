-- P3-b（監査指摘, 2026-08-12）: `plc_connections.word_order` を追加し、SLMP
-- 接続のワード順（32bit値の上位/下位ワードの並び）を接続単位で指定できるように
-- する。これまで `crates/banto-broker/src/lib.rs`（SessionDirectory::ensure_connection）
-- が `SlmpConfig { host, port, ..SlmpConfig::default() }` と組み立てており、
-- `SlmpConfig::word_order` は常に既定の `WordOrder::LowHigh` 固定だった -
-- ワード順が異なる機種（`WordOrder::HighLow` を要する機種）につなぐと u32/f32
-- 等の多ワード型の値が静かに化ける、という監査 P3-b の指摘そのもの。
--
-- 値は `banto_plc::decode::WordOrder` の2バリアントをスネークケースの文字列に
-- 対応させたもの: 'low_high'（既定・MELSEC の標準、D0=下位/D1=上位）/
-- 'high_low'（Modbus/IEEE 慣習）。`protocol` と同じ「TEXT + CHECK」方針
-- （`plc_connection.rs` モジュール冒頭のコメント参照）で、Rust 側の
-- `ALLOWED_WORD_ORDERS` と二重管理する。
--
-- `simulation`（migration 0008）と同じ理由でプレーンな `ADD COLUMN` 1本にして
-- いる: 追加する CHECK は新規列自身しか参照しないため、SQLite の
-- 「ADD COLUMN で CHECK 制約を追加できるのは新規列のみを参照する場合に限る」
-- という制約に収まり、0004/0007 のようなテーブル再構築は不要。
--
-- 既定 'low_high' は既存の `SlmpConfig::default().word_order`
-- (`WordOrder::LowHigh`) と一致させてあるので、既存の全 plc_connections 行 -
-- そして word_order を一度も知らない relay-wright/chronogazer のデータベース
-- も - 「今と同じ動作」のまま `banto_tags::migrate` を通る（後方互換）。
-- modbus-tcp/virtual 接続では無意味な列だが、`unit_id` が SLMP 側で
-- 無意味なまま必須列になっているのと同じ扱い（`plc_connection.rs` モジュール
-- コメントの `unit_id` の注記参照）。

ALTER TABLE plc_connections
    ADD COLUMN word_order TEXT NOT NULL DEFAULT 'low_high'
    CHECK (word_order IN ('low_high', 'high_low'));
