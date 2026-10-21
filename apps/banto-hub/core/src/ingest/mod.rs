//! 取り込み口 `push`（R2a、docs/banto-hub-ingest-design.md）: 外の送り手（DB
//! ブリッジ・スクリプト）が Hub へ値を**送り込む**接続の、Hub 側の置き場
//! （§12 Q5 = (a)「core の `ingest/`」）。
//!
//! `push` は R2a では `DriverKind::Session` にせず、Hub 側の
//! `banto_collect::ExternalSource` として作る（§2、Q1）。このモジュールに揃う
//! もの（予定を含む）:
//!
//! - [`driver`]（R2a-2a）: 静的な部分 [`driver::PushDriver`]（`DriverBase`: 識別・
//!   設定の検証・キーの検証）。**レジストリには登録しない**（R2b-1 で host が
//!   [`driver::push_driver`] を登録する）。R2a の正は今までどおり
//!   `banto_tags::ALLOWED_PROTOCOLS` と SQL の `CHECK`。
//! - `IngestHub`（R2a-1c）: 受信側の保持値・Live の判定・commit・epoch。
//! - 送信口（R2a-2c の REST、R2a-2d の WebSocket）と、その認証・上限・監査。
//!
//! 規則そのもの（キーの文法、`settings` の検証、上限の定数 `PUSH_LIMITS`、ブリッジの
//! 量の見積もり `bridge_load`）は `banto_tags::push` にあり、ここは包むだけ
//! （タグの検証・接続の検証と同じものを使うため）。

pub mod driver;
