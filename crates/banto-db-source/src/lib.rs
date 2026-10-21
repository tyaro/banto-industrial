//! DB Source の共有部品（docs/banto-hub-ingest-design.md §10.1・§11 の R2a-3a）。
//!
//! Hub 内の PostgreSQL Source（`banto-hub-core` の `db_source`）と、R2a-3b で
//! 入る DB ブリッジ（`banto-hub-sink` の `source/`）が**同じ変換と同じ梯子**を
//! 使うための crate。§10.4 の「ブリッジと Hub 内の Source の一致」を、写しの
//! 突き合わせ（#334 / #605 と同じずれ方をする）ではなく、コードの共有で
//! 構造的に保つ（§12 Q12）。R2b-8 で `db_source` を消すときも、この crate は
//! 変わらない。
//!
//! ## 何を持つか
//!
//! | モジュール   | 内容                                                                                          |
//! | ------------ | --------------------------------------------------------------------------------------------- |
//! | [`convert`]  | 列型の分類 [`ColumnKind`]、`describe` 用・実行用の生成 SQL、識別子のクォート                  |
//! | [`resolve`]  | `describe` の結果と欲しい列名の突き合わせ [`resolve_columns`]（未対応型・列なしは Bad）       |
//! | [`ladder`]   | 品質の梯子の純関数の状態機械 [`GroupLadder`]・[`Verdict`]（Good / Stale / Bad の判断だけ）    |
//! | [`pg`]       | [`pg_connect_options`]（`sslmode=prefer`・サーバー側 `statement_timeout` = [`SERVER_STATEMENT_TIMEOUT`]）と [`sanitize_postgres_error`]（資格情報を出さない） |
//! | [`poll`]     | 失敗の粒度の分類 [`classify`]・[`PgFailure`]（接続か文か）とクエリのタイムアウト・再 describe の間隔（R2a-3b） |
//!
//! `validate_query_sql`（登録時の SQL の検証）はここへ**移さない**。`banto-tags`
//! にあり、Hub の登録・ブリッジの設定の取得の両方がそちらを呼ぶ。
//!
//! ## 中立に保つ
//!
//! この crate は Hub の型（`ServerTagStore`・`Quality`・`DbTagPlan` など）を
//! 知らない。値の書き込み・時刻・ログ・状態表示は呼び出し側の仕事で、ここは
//! 「何を書くべきか」の判断だけを返す。
//!
//! - 列の解決は、タグではなく**列名の並び**（添字）で受けて添字で返す。呼び出し側
//!   がタグ（Hub）や送信するキー（ブリッジ）へ写し戻す。
//! - 梯子の判断は [`Verdict`]（Good(値) / Stale / Bad）で返す。Stale は
//!   「直前値を残す」という意味だけで、直前値の有無は呼び出し側が
//!   [`Verdict::settle`] に渡す（直前値の無い Stale は Bad - §4.3 の境界）。
//!
//! 依存は `sqlx`（`postgres`）だけ。ランタイム・TLS の feature は Hub と sink が
//! それぞれ選ぶ。

pub mod convert;
pub mod ladder;
pub mod pg;
pub mod poll;
pub mod resolve;

pub use convert::{
    build_wrapper_sql, describe_columns, describe_wrapper_sql, quote_ident, ColumnKind,
    MappedColumn,
};
pub use ladder::{cell_verdict, GroupLadder, QueryFailure, Verdict};
pub use pg::{pg_connect_options, sanitize_postgres_error, SERVER_STATEMENT_TIMEOUT};
pub use poll::{classify, describe_retry_interval, query_timeout, PgFailure};
pub use resolve::{resolve_columns, ColumnBadReason, ResolvedColumns};
