//! banto-tstore: I3a 時系列ストレージ (docs/plan.md I3, docs/recorder-requirements.md
//! §3.4 "保存・保持", §8 "時系列スキーマ" 未決事項の解決).
//!
//! **日次ファイル + スキーマ凍結**方式（2026-07-12 決定）: 1つのデータ
//! ファイル（SQLite）は作成された瞬間にスキーマが確定し、以後は
//! 変更されない。収集グループ/タグの構成が変わったら既存ファイルを
//! `ALTER` するのではなく、新しい連番ファイルへローテーションする
//! （[`config`] モジュールの `StoreConfig` がそのスナップショット）。
//!
//! ## この crate が意図的に依存しないもの
//!
//! **`banto-tags`（タグレジストリ）**: [`config::StoreConfig`] は
//! `banto_tags::Tag`/`CollectionGroup` を直接受け取らない、完全に独立した型。
//! 各データファイルは自分の `tstore_meta`/`tstore_groups`/`tstore_columns`
//! テーブルだけでタグ↔列の対応を説明できる（自己記述的。詳細は
//! `schema.rs` のモジュールドキュメント参照）ので、後続の I4（クエリ層）は
//! レジストリ DB に接続しなくてもファイル単体を解釈できる。`StoreConfig` を
//! `banto_tags` の行から組み立てるのは I3b（収集エンジン）の責務。
//!
//! **`banto-core`/`banto-storage`**: `banto-tags`/`banto-plc` と違い、この
//! crate は CRUD エンティティでも Tauri/REST 境界を跨ぐ資源でもない低レベルな
//! ストレージエンジンなので、`banto_core::BantoError` ではなく自前の
//! [`error::TstoreError`] を持つ（`banto-plc::PlcError` と同じ判断。詳細は
//! `error.rs` のモジュールドキュメント参照）。
//!
//! ## モジュール構成
//!
//! [`config`][]: [`config::StoreConfig`]/[`config::GroupConfig`]/
//! [`config::TagColumn`] - ファイル生成に使う構成スナップショット。
//!
//! [`date`][]: [`date::LocalDate`] - ローカル暦日と UTC epoch ms の
//! 相互変換（純粋な整数演算、依存クレートなし）。
//!
//! [`clock`][]: [`clock::Clock`] trait - ローテーション判定の「今」を注入
//! 可能にする（本番用 [`clock::SystemClock`]、テスト用
//! [`clock::ManualClock`]）。
//!
//! [`schema`][]: ファイル名の生成/解析、テーブル DDL、メタデータの読み書き -
//! [`writer::TsWriter`]/[`reader::TsReader`] 共通の土台。
//!
//! [`meta`][]: 読み戻したメタデータの形（[`meta::GroupMeta`]/
//! [`meta::ColumnMeta`]）。
//!
//! [`writer`][]: [`writer::TsWriter`] - 追記・バッファリング・自動
//! ローテーション。
//!
//! [`reader`][]: [`reader::TsReader`] - 1ファイル単位の最小限の読み出し
//! （範囲クエリ・間引きは I4 の仕事）。
//!
//! [`files`][]: [`files::list_data_files`]/[`files::prune_files`] -
//! データディレクトリ単位の列挙・保持期限削除。
//!
//! [`error`][]: [`error::TstoreError`]。

pub mod clock;
pub mod config;
pub mod date;
pub mod error;
pub mod files;
pub mod meta;
pub mod reader;
pub mod schema;
pub mod writer;

pub use clock::{Clock, ManualClock, SystemClock};
pub use config::{GroupConfig, StoreConfig, TagColumn};
pub use date::LocalDate;
pub use error::TstoreError;
pub use files::{list_data_files, prune_files, DataFileInfo, PruneReport};
pub use meta::{ColumnMeta, GroupMeta};
pub use reader::{Sample, TsReader};
pub use writer::{TsWriter, WriterOptions};
