//! DB Sink（docs/banto-hub-external-db-design.md #229・§5）: タグ空間を外部
//! PostgreSQL へ記録する仕組みの **Hub 側**（S4）。エンジン本体（購読・
//! バッチ INSERT・キュー管理・テーブル検査）は Hub とは別プロセスの
//! サイドカー `banto-hub-sink`（S5、`apps/banto-hub-sink`・未実装）が持つ -
//! 本モジュールはそのサイドカーが読みに来る**設定**と、押し込んでくる
//! **運転状態**を Hub 側で保持するだけ（§5.1「設定・UI・監視は Hub が持つ」）。
//!
//! ## 構成
//!
//! - [`service`]: `hub_sink_groups`/`hub_sink_group_tags` の CRUD
//!   （[`service::SinkGroupService`]）と、`plc_connections` 削除ハンドラが
//!   使う参照チェック（[`service::reject_delete_if_referenced_by_sink_group`]）。
//! - [`status`]: サイドカーが `PUT /api/sink/status` で push する運転状態を
//!   プロセス内メモリに保持する（[`status::SinkStatusStore`]）。`GET
//!   /api/status`/`GET /api/v1/status` の `sink` 節がここを読む。
//!
//! ## エンティティの配置（設計 §5.2）
//!
//! DB 接続は #228（DB Source、`crate::db_source`）と `plc_connections`
//! （`protocol = "postgres"`）を共有する - 接続の登録・接続テスト・資格情報
//! の扱いを1つに保つため。sink 固有の設定は `hub_sink_groups`（1グループ =
//! 1保存先テーブル + 1周期/モード）と、その対象タグを持つ
//! `hub_sink_group_tags`（多対多）の2テーブルに新設する（`crate::db`の
//! `apply_app_schema`参照）。テーブル名は設計 §5.2 の呼称
//! （`logger_groups`/`logger_group_tags`）ではなく他の Hub 専用テーブル
//! （`hub_write_audit`/`hub_retained_values`）と同じ `hub_` 接頭辞に揃えた
//! （指示: S4 実装指示）。
//!
//! ## FK を張らない理由（`crate::db`の doc comment と同じ判断）
//!
//! `hub_sink_groups.db_connection_id`（→ `plc_connections.id`）、
//! `hub_sink_group_tags.tag_id`（→ `tags.id`）のいずれも SQL の
//! `FOREIGN KEY` を張らない。理由は `hub_retained_values.tag_id` と同じ
//! 慣行に揃えるため: この2テーブルは `apply_app_schema` の一部として
//! `banto_tags::migrate`（`plc_connections`/`tags` を作る側）より**先**に
//! 走る。参照整合性はサービス層で担保する:
//!
//! - `db_connection_id` の存在確認と `protocol == "postgres"` 検証は
//!   [`service::validate_sink_group_input`] が create/update の入口で行う。
//! - `plc_connections` の削除は、それを参照する sink group が1件でも
//!   あれば拒否する（RESTRICT 相当） -
//!   [`service::reject_delete_if_referenced_by_sink_group`] を
//!   `crate::rest` の接続削除ハンドラ（即時削除・pending apply の両方）が
//!   `cascade_delete_tx` の**前**に呼ぶ。
//! - `tag_id` の存在確認は create/update 時に行う。単体/一括のタグ削除
//!   （`crate::rest` の `tags_delete`/`tags_batch_delete`）はこのモジュールの
//!   [`service::remove_tag_from_all_sink_groups`] で対応する行を能動的に
//!   消すが、グループ/接続のカスケード削除（UX-38 の `cascade_delete_tx`
//!   経路）で消えるタグ id はここでは追いかけない - `hub_retained_values`
//!   と同じ「実害のない孤児」の扱い（読み出し側 - `GET /api/sink/config`
//!   や `list`/`get` の `tagIds` - は現在の catalog に存在しない `tag_id`
//!   をそのまま返す/読み飛ばすだけで、機能上の実害はない）。
//!
//! ## pending queue には載らない（§6-13）
//!
//! `hub_sink_groups` の CRUD は収集の稼働状態に関わらず常に即時適用する -
//! sink はタグ空間を消費するだけで PLC 収集パイプラインには一切関わらない
//! ため（`crate::rest` の `plc_connections`/`collection_groups` が持つ
//! `queue_pending_registry_change` 分岐をこのリソースは持たない）。変更の
//! 伝搬は `commit_catalog_and_notify`（catalog 再構築・収集 task の再同期）
//! ではなく、単純な SSE `ServerEvent::ResourceChanged { resource:
//! "sink_groups" }` の送出のみ - サイドカーはこれを将来の SSE 購読/定期
//! ポーリングで検知して `GET /api/sink/config` を取り直す（§5.2）。

pub mod service;
pub mod status;

pub use service::{
    reject_delete_if_referenced_by_sink_group, remove_tag_from_all_sink_groups, SinkGroup,
    SinkGroupInput, SinkGroupService, ALLOWED_SINK_MODES,
};
pub use status::{
    is_valid_sink_group_state, SinkGroupStatusPush, SinkStatusSnapshot, SinkStatusStore,
    ALLOWED_SINK_GROUP_STATES, SINK_SIDECAR_STALE_AFTER_MS,
};
