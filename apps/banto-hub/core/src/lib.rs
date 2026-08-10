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
//! - [`computed`]: T6-2（設計 §4.2/§4.3(a)）。演算タグ・内部タグの評価
//!   エンジン（[`computed::ComputedEngine`]）とタグ空間ストア
//!   （[`computed::ServerTagStore`]）。`hub::CollectorManager::rebuild` が
//!   catalog/`Collector` と同じ all-or-nothing で式のコンパイル・DAG 検証
//!   結果を commit する
//! - [`broker_glue`]: T2-2（設計 §6-5）。SLMP 接続の収集読み取りを
//!   `banto-broker`（I6）経由にするアダプタ（[`broker_glue::BrokerReadClient`]）
//!   と、`CollectorManager` の外で生存するブローカーセッション directory
//!   （[`broker_glue::HubSessions`]）。Modbus 接続は現行の直接クライアントの
//!   まま
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
//!   `/api/v1/openapi.json` は認証不要）の2系統。`GET /api/v1/stream`
//!   （[`stream`] の WebSocket アップグレード）もこの認証層の下で公開する
//! - [`api_keys`]: `/api/v1/*` 用のスコープ付き API キー基盤（設計
//!   §5.6、T0-2）。発行・一覧・失効の service 層と、キー生成/ハッシュ/
//!   スコープ構文検証の純関数群
//! - [`stream`]: T1（設計 §5.2）。`/api/v1/stream` の WebSocket ハンドラ・
//!   購読の状態機械・250ms 評価ループ。`hub::CollectorManager` の
//!   `current_values`/`tag_map`/`subscribe_events`/`subscribe_revision` を
//!   読むだけの消費者で、収集エンジンには一切書き込まない
//! - [`write_control`]: T2-4（設計 §6-6）。書き込み受付の起動時
//!   disabled フラグ（relay-wright の arming 同型）
//! - [`write_rate`]: T2-4（設計 §6-4）。タグ毎 + 全体の2段書き込み
//!   レート制限（relay-wright の rate_limiter をタグ単位に読み替え）
//! - [`write_audit`]: T2-4（設計 §6-3）。`hub_write_audit` の
//!   log-before-write アクセス経路
//! - [`mqtt`]: T3（設計 §5.3）。外部 MQTT ブローカーへ接続するクライアント
//!   モードの発行機能（rumqttc）。[`hub::CollectorManager`] を読むだけの
//!   消費者（設計 §3.4「収集に背圧をかけない」） - `crate::stream`と同じ
//!   立ち位置
//! - [`test_output`]: T15-3（docs/banto-hub-desktop-plan.md §6.3）。現在の
//!   収集 run コンテキストにのみ opt-in する「テスト出力」フラグ
//!   （[`write_control::WriteControl`]と同型・非永続）。`crate::mqtt`の
//!   専用 test トピック・`crate::grpc`の`StreamValues(test_output=true)`
//!   がこれを読む
//! - [`runtime`]: T14-1（docs/banto-hub-t14-design.md §3「D1」）。
//!   composition root（[`runtime::HubRuntime::start`]/
//!   [`runtime::RunningHub::shutdown`]）- DB初期化〜各サービス構築〜
//!   axumサーバー起動〜シャットダウン順序を1箇所に持ち、`bin/banto-hub.rs`
//!   （コンソール）と `bin/banto_hub/win_service.rs`（Windows サービス）の
//!   両ホストから呼ばれる薄い composition root。以前は bin 側の
//!   `hub_run::run(shutdown)` だった
//! - [`hub_log`]: T14-1 でバイナリクレートからこの lib へ移設した出力
//!   ヘルパー（[`runtime`] が composition root としてここに来たため）。
//!   `println!`/`eprintln!` の薄いラッパーで、Windows サービスモードの
//!   間だけ同じ内容をファイルにもミラーする（[`hub_log`] のモジュール doc
//!   参照）
//! - [`service_manager`]: T17-0（docs/banto-hub-t17-design.md §3/§4）。
//!   SCM 状態取得＋start/stop/restart/autostart 操作のホスト非依存ロジック
//!   層（[`service_manager::ServiceManager`] trait）。テスト用の
//!   [`service_manager::MockServiceManager`]（常に利用可能）と実 SCM を
//!   叩く`WindowsServiceManager`（`#[cfg(windows)]`）の2実装を持つ。
//!   `bin/banto_hub/win_service.rs`の既存`install`/`uninstall`CLI 自体は
//!   このモジュールに置き換えていない（挙動不変、モジュール doc 参照）
//!
//! T0/T1 のスコープ外（設計冒頭の指示どおり実装しない）: gRPC、管理 UI
//! フロントエンドの中身の一部、演算タグ、接続単位の部分再構成。書き込み
//! 経路（`write:` スコープの受理・保存は T0-2 で実装済み、実際の書き込み
//! エンドポイントと安全ゲート一式は T2-4 で実装した - `crate::rest` の
//! `POST /api/v1/values/{tag}` と上記3モジュール）。MQTT publish は T3 で
//! 実装済み（[`mqtt`]）。

pub mod api_keys;
pub mod assets;
pub mod audit;
pub mod broker_glue;
pub mod computed;
pub mod controller;
pub mod db;
pub mod diag_log;
pub mod events;
pub mod grpc;
pub mod hub;
pub mod hub_log;
pub mod mqtt;
pub mod rest;
pub mod runtime;
pub mod service_manager;
pub mod settings;
pub mod stream;
pub mod subscribe_core;
pub mod test_output;
#[cfg(test)]
pub(crate) mod test_support;
pub mod users;
pub mod write_audit;
pub mod write_control;
pub mod write_path;
pub mod write_rate;

pub use controller::{
    CollectionController, CollectionState, CollectionStatus, RunContext, RunId, RunMode,
};
