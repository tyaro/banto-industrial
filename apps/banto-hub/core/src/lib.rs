//! banto-hub の domain/service 層 (docs/tag-server-design.md, 特に §3〜§5)。
//!
//! banto-hub は「タグサーバー」— I1（banto-tags）のタグレジストリと
//! I3b（banto-collect）の収集エンジンを1プロセスに束ね、REST でタグ空間
//! （現在値キャッシュ）を外部公開する、**Tauri を使わないヘッドレス axum
//! サーバー**（設計 §3.1）。ChronoGazer / relay-wright とは別プロセスで
//! 24/365 稼働する — 収集の寿命を UI プロセスから切り離すのが狙い
//! （設計 §0）。
//!
//! ## この crate が新規実装しないもの（設計 §3.2 の核心）
//!
//! 「タグ空間」に相当するものは `banto_collect::CurrentValuesHandle`
//! （現在値 + 品質のキャッシュ）が既にそのものであり、この crate は
//! 既存の I 系資産（banto-tags / banto-plc / banto-tstore / banto-collect /
//! banto_server）を束ね直すだけで成立する。banto-hub 自身が新しく持つのは
//! 「外部名 `{connection}.{group}.{tag}` ↔ 内部 `tag_key` のマッピング」
//! （[`hub::TagMap`]）と「レジストリ変更を検知して Collector を作り直す
//! 手順」（[`hub::CollectorManager`]）の2つだけ。
//!
//! ## モジュール構成（apps/chronogazer/core の構造を踏襲）
//!
//! - [`db`]: SQLite 起動 - この app 自身のスキーマ（settings/users/
//!   audit_log）→ `banto_tags::migrate` → `banto_collect::migrate` の順で
//!   1つの共有プールに適用する
//! - [`settings`]: hub 用設定（`server.bind`/`server.port`/`data.dir`/
//!   `retention.days`）。hub は常時サーバーなので ChronoGazer の
//!   `server.enabled` トグルは持たない
//! - [`users`] / [`audit`]: chronogazer からほぼそのまま流用した
//!   ローカルアカウント（RBAC: admin/editor/viewer）と監査ログ
//! - [`assets`]: 管理 UI 静的ファイルの埋め込み枠（`embed-ui` feature）。
//!   T0 では中身（フロントエンド）は作らない — 枠だけ用意する
//! - [`events`]: 管理 UI の SSE 用 `banto_server::ServerEvent` チャンネル。
//!   `banto_collect::CollectEvent` とは別物（そちらは `/api/v1/events` が
//!   `collect_events` テーブルを直接読む）
//! - [`hub`]: 中核。[`hub::CollectorManager`] が Collector のライフサイクル
//!   （起動時1回 + レジストリ書き込み後の全体再構築、設計 §4.3 の
//!   「T0 は全体再構築で開始してよい」に従う）と revision カウンタを持ち、
//!   [`hub::TagMap`] が外部名 catalog のスナップショットを保持する
//! - [`rest`]: `api_router` - 管理系（auth/users/audit/I1 CRUD/api-keys、
//!   CSRF 必須）と `/api/v1/*` タグ空間 API（機械クライアント向け、CSRF
//!   不要、設計 §5.1/§5.6。API キー + セッション bearer の併用認証、
//!   `/api/v1/openapi.json` は認証不要）の2系統
//! - [`api_keys`]: `/api/v1/*` 用のスコープ付き API キー基盤（設計
//!   §5.6、T0-2）。発行・一覧・失効の service 層と、キー生成/ハッシュ/
//!   スコープ構文検証の純関数群
//!
//! T0 のスコープ外（設計冒頭の指示どおり実装しない）: WebSocket、MQTT、
//! gRPC、書き込み経路（`write:` スコープの受理・保存は T0-2 で実装済みだが
//! 検証・使用は T2）、管理 UI フロントエンドの中身、演算タグ、接続単位の
//! 部分再構成。

pub mod api_keys;
pub mod assets;
pub mod audit;
pub mod db;
pub mod events;
pub mod hub;
pub mod rest;
pub mod settings;
pub mod users;
