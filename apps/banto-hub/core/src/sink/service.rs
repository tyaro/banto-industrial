//! `hub_sink_groups`/`hub_sink_group_tags` の CRUD（`crate::sink`のモジュール
//! doc comment参照）。`banto_tags`の各サービス（`PlcConnectionService`等）と
//! 同じ形（`pool`を1個持つ`Clone`可能なサービス構造体）だが、`create`/
//! `update`/`delete` は下記「**`BEGIN IMMEDIATE`**」の説明通りトランザクション
//! を開始する。また、この2テーブルは Hub 専用（レジストリ側 `banto_tags` の
//! 管轄外）なので `apps/banto-hub/core`側に置く - 他の Hub 専用テーブルのサービス
//! （`crate::write_audit`・`crate::pending_changes`）と同じ配置。
//!
//! **`BEGIN IMMEDIATE`（2026-09-07 追記、CI flake 修正）**: `create`/
//! `update`/`delete` は`self.pool.begin_with("BEGIN IMMEDIATE")`で
//! トランザクションを開始する - 素の`self.pool.begin()`（SQLite の既定
//! `BEGIN DEFERRED`）は書き込みロックの取得を最初の書き込み文まで遅らせる。
//! `update`はその書き込み文の前に検証用`SELECT`を複数回（既存行取得・
//! 名前重複チェック・接続の存在/`protocol`チェック・タグ存在チェックを
//! タグ数ぶんループ）挟むため、その間に別コネクションがコミットすると、
//! このトランザクションが確立した読み取りスナップショットが古くなり、
//! `UPDATE`実行時に`SQLITE_BUSY`（"database is locked"、code 5）を
//! *待たずに即座に*返す（WAL モードで「読み取りスナップショット確立後に
//! 別コネクションが書き込みをコミットした」ケースは、SQLite がビジー
//! ハンドラを呼ばず即時 `SQLITE_BUSY` を返す仕様のため、
//! `busy_timeout`（既定 5秒、`banto_storage::sqlite::connect`/
//! `connect_memory`）が長くても解決しない）。`BEGIN IMMEDIATE`は
//! トランザクション開始時点で書き込みロックを取得するため、「後から
//! アップグレードしようとして失敗する」窓自体を無くす（先に開始した側が
//! 完了するまで、後続の`BEGIN IMMEDIATE`は`busy_timeout`の通常の待ち合わせ
//! でブロックされるだけになる）。`banto_tags`の各サービスの素の`create`/
//! `update`（`create_tx`/`update_tx`ではない方）がこの問題を踏まなかった
//! のは、検証用`SELECT`と書き込み文をそれぞれ独立した単発オートコミットと
//! して`&self.pool`に投げており、複数の`SELECT`を1つの明示トランザクション
//! に束ねていなかったため - このモジュールは`hub_sink_groups`と
//! `hub_sink_group_tags`をまたぐ複数文を原子的に更新する必要があるため
//! 同じ形にはできず、`BEGIN IMMEDIATE`で解決する。

use banto_core::{BantoError, FieldError};
use serde::{Deserialize, Serialize};
use sqlx::{SqliteConnection, SqlitePool};

use banto_tags::POSTGRES_PROTOCOL;

const RESOURCE: &str = "sink_groups";
const MAX_NAME_LEN: usize = 64;
const MIN_INTERVAL_MS: i64 = 100;
const MAX_INTERVAL_MS: i64 = 3_600_000;
const MAX_TABLE_IDENTIFIER_LEN: usize = 63;

/// `hub_sink_groups.mode`（設計 §5.2「MQTT と同じ語彙」）。SQL の `CHECK` と
/// 二重管理になる関係は `banto_tags::plc_connection::ALLOWED_PROTOCOLS` と
/// 同じ - 変更する際は `crate::db`の`apply_app_schema`のCHECKも合わせる。
pub const ALLOWED_SINK_MODES: &[&str] = &["interval", "on_change"];

fn default_enabled() -> bool {
    true
}

/// `hub_sink_groups`の1行 + 対象タグ id 集合（`hub_sink_group_tags`）。
/// `sqlx::FromRow`は導出しない - `tag_ids`は別テーブルなので単一行の
/// `SELECT`では埋まらない。行取得は`hub_sink_groups`単独の列だけを持つ
/// 内部専用の[`SinkGroupRow`]（`FromRow`導出）が担い、
/// [`SinkGroupRow::into_group`]が`tag_ids`を添えてこの型へ変換する。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SinkGroup {
    pub id: i64,
    pub name: String,
    pub db_connection_id: i64,
    pub mode: String,
    pub interval_ms: i64,
    pub table_name: String,
    pub store_bad: bool,
    pub enabled: bool,
    pub tag_ids: Vec<i64>,
}

/// Create/update payload（`id`を持たない点だけが[`SinkGroup`]と違う - 他の
/// `*Input`型と同じ規約）。`storeBad`/`enabled`はブール値の使い勝手優先で
/// 既定値を持つ（`banto_tags::PlcConnectionInput::enabled`と同じ判断）が、
/// それ以外は省略不可 - MCP の `update_sink_group` はさらに厳しく
/// `require_all_fields`で全キーの**存在**を要求する（`crate::mcp`の
/// `UPDATE_CONNECTION_REQUIRED_FIELDS`等と同じ「PUT 置換で省略項目が
/// 黙って既定値に上書きされるのを防ぐ」規約 - このモジュールの doc comment
/// 参照）。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SinkGroupInput {
    pub name: String,
    pub db_connection_id: i64,
    pub mode: String,
    pub interval_ms: i64,
    pub table_name: String,
    #[serde(default)]
    pub store_bad: bool,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    pub tag_ids: Vec<i64>,
}

/// `hub_sink_groups`単独の列だけを持つ内部行型 - `sqlx::FromRow`で直接
/// クエリするための最小単位。[`SinkGroup`]へは呼び出し元が`tag_ids`を
/// 添えて変換する（[`SinkGroupRow::into_group`]）。
#[derive(sqlx::FromRow)]
struct SinkGroupRow {
    id: i64,
    name: String,
    db_connection_id: i64,
    mode: String,
    interval_ms: i64,
    table_name: String,
    store_bad: bool,
    enabled: bool,
}

impl SinkGroupRow {
    fn into_group(self, tag_ids: Vec<i64>) -> SinkGroup {
        SinkGroup {
            id: self.id,
            name: self.name,
            db_connection_id: self.db_connection_id,
            mode: self.mode,
            interval_ms: self.interval_ms,
            table_name: self.table_name,
            store_bad: self.store_bad,
            enabled: self.enabled,
            tag_ids,
        }
    }
}

const SINK_GROUP_COLUMNS: &str =
    "id, name, db_connection_id, mode, interval_ms, table_name, store_bad, enabled";

/// A bare table/schema identifier segment: an ASCII letter or `_` followed by
/// ASCII letters/digits/`_`, capped at 63 bytes - the same shape
/// `crates/banto-tags/src/tag.rs`'s `is_db_column_identifier` uses for a `db`
/// tag's column name (this module's doc comment: no quoted identifiers, so
/// the sidecar's generated `INSERT`/`SELECT ... LIMIT 0` builder never needs
/// to handle quoting corner cases).
fn is_valid_table_identifier_segment(segment: &str) -> bool {
    if segment.is_empty() || segment.len() > MAX_TABLE_IDENTIFIER_LEN {
        return false;
    }
    let mut chars = segment.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `table_name`（設計 §5.2「スキーマ修飾 `schema.table` を許可」）: `.`で
/// 区切って最大2セグメント、各セグメントが
/// [`is_valid_table_identifier_segment`]を満たすこと。
fn is_valid_table_name(table_name: &str) -> bool {
    let segments: Vec<&str> = table_name.split('.').collect();
    match segments.as_slice() {
        [table] => is_valid_table_identifier_segment(table),
        [schema, table] => {
            is_valid_table_identifier_segment(schema) && is_valid_table_identifier_segment(table)
        }
        _ => false,
    }
}

/// [`SinkGroupService::create`]/[`SinkGroupService::update`]共通の検証。
/// `existing_id`は update 時の自己参照を名前の一意性チェックから除外する
/// ための`Some(id)`（create は`None`）。フィールド形式の同期チェックを
/// すべて集めてから、DB参照が要る非同期チェック（接続の存在・
/// `protocol == "postgres"`・タグの存在）を追加する - 1件でもあれば
/// `Vec<FieldError>`全件を返す（`banto_tags::plc_connection`の
/// `validate_plc_connection_input`と同じ「最初の1件で打ち切らない」規約）。
async fn validate_sink_group_input(
    conn: &mut SqliteConnection,
    input: &SinkGroupInput,
    existing_id: Option<i64>,
) -> Result<(), BantoError> {
    let mut errors = Vec::new();

    let trimmed_name = input.name.trim();
    if trimmed_name.is_empty() {
        errors.push(FieldError {
            field: "name".to_string(),
            message: "名前を入力してください".to_string(),
        });
    } else if trimmed_name.chars().count() > MAX_NAME_LEN {
        errors.push(FieldError {
            field: "name".to_string(),
            message: format!("名前は{MAX_NAME_LEN}文字以内で入力してください"),
        });
    }

    if !ALLOWED_SINK_MODES.contains(&input.mode.as_str()) {
        errors.push(FieldError {
            field: "mode".to_string(),
            message: format!(
                "mode は次のいずれかを指定してください: {}",
                ALLOWED_SINK_MODES.join(", ")
            ),
        });
    }

    if !(MIN_INTERVAL_MS..=MAX_INTERVAL_MS).contains(&input.interval_ms) {
        errors.push(FieldError {
            field: "intervalMs".to_string(),
            message: format!(
                "intervalMs は{MIN_INTERVAL_MS}〜{MAX_INTERVAL_MS}の範囲で指定してください"
            ),
        });
    }

    if !is_valid_table_name(&input.table_name) {
        errors.push(FieldError {
            field: "tableName".to_string(),
            message: "tableName は英字/アンダースコアで始まる識別子（省略可能な1回のスキーマ修飾 `schema.table` を含む）を指定してください。引用符は使用できません".to_string(),
        });
    }

    if input.tag_ids.is_empty() {
        errors.push(FieldError {
            field: "tagIds".to_string(),
            message: "対象タグを1件以上指定してください".to_string(),
        });
    } else {
        let mut seen = std::collections::HashSet::new();
        for tag_id in &input.tag_ids {
            if !seen.insert(*tag_id) {
                errors.push(FieldError {
                    field: "tagIds".to_string(),
                    message: format!("タグ id {tag_id} が重複しています"),
                });
                break;
            }
        }
    }

    // ここまでは DB を触らない同期チェック。1件でもあれば、この先の
    // DB 参照チェックを待たずにまとめて返す方が呼び出し元に親切だが、
    // `validate_plc_connection_input`（既存の慣行）は同期/非同期チェックを
    // 両方走らせてから合算する - このモジュールもそれに揃える。

    if trimmed_name.chars().count() > 0 && trimmed_name.chars().count() <= MAX_NAME_LEN {
        let name_conflict: Option<i64> =
            sqlx::query_scalar("SELECT id FROM hub_sink_groups WHERE name = ? AND id != ?")
                .bind(trimmed_name)
                .bind(existing_id.unwrap_or(-1))
                .fetch_optional(&mut *conn)
                .await
                .map_err(banto_storage::storage_error)?;
        if name_conflict.is_some() {
            errors.push(FieldError {
                field: "name".to_string(),
                message: "その名前は既に使用されています".to_string(),
            });
        }
    }

    let connection_protocol: Option<String> =
        sqlx::query_scalar("SELECT protocol FROM plc_connections WHERE id = ?")
            .bind(input.db_connection_id)
            .fetch_optional(&mut *conn)
            .await
            .map_err(banto_storage::storage_error)?;
    match connection_protocol.as_deref() {
        None => errors.push(FieldError {
            field: "dbConnectionId".to_string(),
            message: "指定された接続が見つかりません".to_string(),
        }),
        Some(protocol) if protocol != POSTGRES_PROTOCOL => errors.push(FieldError {
            field: "dbConnectionId".to_string(),
            message: "dbConnectionId は protocol が postgres の接続を指定してください".to_string(),
        }),
        Some(_) => {}
    }

    for tag_id in &input.tag_ids {
        let exists: Option<i64> = sqlx::query_scalar("SELECT id FROM tags WHERE id = ?")
            .bind(tag_id)
            .fetch_optional(&mut *conn)
            .await
            .map_err(banto_storage::storage_error)?;
        if exists.is_none() {
            errors.push(FieldError {
                field: "tagIds".to_string(),
                message: format!("タグ id {tag_id} が見つかりません"),
            });
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(BantoError::Validation {
            field_errors: errors,
        })
    }
}

/// [`SinkGroupService::create`]/[`update`](SinkGroupService::update)の後半
/// 共通処理: `hub_sink_group_tags`を対象タグ集合で置き換える（update は
/// 既存行を全削除してから作り直す - タグ数もグループ数もごく小規模な設定
/// エンティティなので、差分更新より単純な「全削除して作り直す」を選んだ）。
async fn replace_tag_memberships(
    conn: &mut SqliteConnection,
    sink_group_id: i64,
    tag_ids: &[i64],
) -> Result<(), BantoError> {
    sqlx::query("DELETE FROM hub_sink_group_tags WHERE sink_group_id = ?")
        .bind(sink_group_id)
        .execute(&mut *conn)
        .await
        .map_err(banto_storage::storage_error)?;
    for tag_id in tag_ids {
        sqlx::query("INSERT INTO hub_sink_group_tags (sink_group_id, tag_id) VALUES (?, ?)")
            .bind(sink_group_id)
            .bind(tag_id)
            .execute(&mut *conn)
            .await
            .map_err(banto_storage::storage_error)?;
    }
    Ok(())
}

async fn fetch_tag_ids(
    conn: &mut SqliteConnection,
    sink_group_id: i64,
) -> Result<Vec<i64>, BantoError> {
    sqlx::query_scalar::<_, i64>(
        "SELECT tag_id FROM hub_sink_group_tags WHERE sink_group_id = ? ORDER BY tag_id",
    )
    .bind(sink_group_id)
    .fetch_all(&mut *conn)
    .await
    .map_err(banto_storage::storage_error)
}

async fn fetch_group_row(conn: &mut SqliteConnection, id: i64) -> Result<SinkGroupRow, BantoError> {
    // AssertSqlSafe: 埋め込むのは本ファイル内の固定列リスト定数のみ。値は
    // すべてプレースホルダでバインドする（`banto_tags`の各サービスと同じ
    // 慣行）。
    sqlx::query_as::<_, SinkGroupRow>(sqlx::AssertSqlSafe(format!(
        "SELECT {SINK_GROUP_COLUMNS} FROM hub_sink_groups WHERE id = ?"
    )))
    .bind(id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(banto_storage::storage_error)?
    .ok_or_else(|| BantoError::NotFound {
        resource: RESOURCE.to_string(),
        id: id.to_string(),
    })
}

async fn get_in_tx(conn: &mut SqliteConnection, id: i64) -> Result<SinkGroup, BantoError> {
    let row = fetch_group_row(conn, id).await?;
    let tag_ids = fetch_tag_ids(conn, id).await?;
    Ok(row.into_group(tag_ids))
}

/// Service layer for `hub_sink_groups`/`hub_sink_group_tags`（`crate::sink`の
/// モジュール doc comment参照）。
#[derive(Clone)]
pub struct SinkGroupService {
    pool: SqlitePool,
}

impl SinkGroupService {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// 全件、名前順（設定エンティティとして件数が少ない前提 - ページングは
    /// 持たない。`crate::api_keys::ApiKeysService::list`と同じ判断）。
    pub async fn list(&self) -> Result<Vec<SinkGroup>, BantoError> {
        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(banto_storage::storage_error)?;
        // AssertSqlSafe: fetch_group_row と同じ理由。
        let rows: Vec<SinkGroupRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {SINK_GROUP_COLUMNS} FROM hub_sink_groups ORDER BY name"
        )))
        .fetch_all(&mut *conn)
        .await
        .map_err(banto_storage::storage_error)?;
        let mut groups = Vec::with_capacity(rows.len());
        for row in rows {
            let tag_ids = fetch_tag_ids(&mut conn, row.id).await?;
            groups.push(row.into_group(tag_ids));
        }
        Ok(groups)
    }

    /// [`Self::list`]のうち`enabled`な行だけ（`GET /api/sink/config`専用 -
    /// 設計 §5.2「有効な logger_groups」）。
    pub async fn list_enabled(&self) -> Result<Vec<SinkGroup>, BantoError> {
        Ok(self
            .list()
            .await?
            .into_iter()
            .filter(|group| group.enabled)
            .collect())
    }

    pub async fn get(&self, id: i64) -> Result<SinkGroup, BantoError> {
        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(banto_storage::storage_error)?;
        get_in_tx(&mut conn, id).await
    }

    pub async fn create(&self, input: SinkGroupInput) -> Result<SinkGroup, BantoError> {
        // `BEGIN IMMEDIATE`: このモジュールの doc comment「BEGIN IMMEDIATE」
        // 参照 - 書き込み文の前に検証用`SELECT`を複数回挟むため、素の
        // `BEGIN DEFERRED`だと WAL のスナップショット競合で
        // 即時`SQLITE_BUSY`になり得る。
        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(banto_storage::storage_error)?;
        validate_sink_group_input(&mut tx, &input, None).await?;
        let name = input.name.trim();
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO hub_sink_groups \
             (name, db_connection_id, mode, interval_ms, table_name, store_bad, enabled) \
             VALUES (?, ?, ?, ?, ?, ?, ?) RETURNING id",
        )
        .bind(name)
        .bind(input.db_connection_id)
        .bind(&input.mode)
        .bind(input.interval_ms)
        .bind(&input.table_name)
        .bind(input.store_bad)
        .bind(input.enabled)
        .fetch_one(&mut *tx)
        .await
        .map_err(banto_storage::storage_error)?;
        replace_tag_memberships(&mut tx, id, &input.tag_ids).await?;
        let created = get_in_tx(&mut tx, id).await?;
        tx.commit().await.map_err(banto_storage::storage_error)?;
        Ok(created)
    }

    pub async fn update(&self, id: i64, input: SinkGroupInput) -> Result<SinkGroup, BantoError> {
        // `BEGIN IMMEDIATE`: このモジュールの doc comment「BEGIN IMMEDIATE」
        // 参照 - CI で観測された "database is locked" flake の修正
        // （2026-09-07）。存在確認・名前重複・接続/タグ存在チェックと
        // いった複数回の`SELECT`を書き込み文の前に挟むため、素の
        // `BEGIN DEFERRED`だと、その間に他コネクションがコミットした
        // ときに読み取りスナップショットが古くなり、`UPDATE`実行時に
        // `busy_timeout`で待たされず即座に`SQLITE_BUSY`を返しうる。
        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(banto_storage::storage_error)?;
        // 存在確認を先に行う - 見つからなければ検証より先に404を返す
        // （`banto_tags`の各`update`と同じ順序）。
        fetch_group_row(&mut tx, id).await?;
        validate_sink_group_input(&mut tx, &input, Some(id)).await?;
        let name = input.name.trim();
        sqlx::query(
            "UPDATE hub_sink_groups SET \
             name = ?, db_connection_id = ?, mode = ?, interval_ms = ?, table_name = ?, \
             store_bad = ?, enabled = ?, updated_at = datetime('now') \
             WHERE id = ?",
        )
        .bind(name)
        .bind(input.db_connection_id)
        .bind(&input.mode)
        .bind(input.interval_ms)
        .bind(&input.table_name)
        .bind(input.store_bad)
        .bind(input.enabled)
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(banto_storage::storage_error)?;
        replace_tag_memberships(&mut tx, id, &input.tag_ids).await?;
        let updated = get_in_tx(&mut tx, id).await?;
        tx.commit().await.map_err(banto_storage::storage_error)?;
        Ok(updated)
    }

    pub async fn delete(&self, id: i64) -> Result<(), BantoError> {
        // `BEGIN IMMEDIATE`: `create`/`update`と揃える（このモジュールの
        // doc comment「BEGIN IMMEDIATE」参照）- `delete`自体は書き込み文が
        // 1つだけで同じ危険は薄いが、素の`BEGIN DEFERRED`のままにする理由も
        // ないため同じ形にする。
        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(banto_storage::storage_error)?;
        // `hub_sink_group_tags`は`ON DELETE CASCADE`（`crate::db`参照）なので
        // 明示的な削除は不要。
        let result = sqlx::query("DELETE FROM hub_sink_groups WHERE id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(banto_storage::storage_error)?;
        if result.rows_affected() == 0 {
            return Err(BantoError::NotFound {
                resource: RESOURCE.to_string(),
                id: id.to_string(),
            });
        }
        tx.commit().await.map_err(banto_storage::storage_error)?;
        Ok(())
    }
}

/// `plc_connections`の削除ハンドラ（`crate::rest`の即時削除・pending apply
/// の両経路）が`cascade_delete_tx`を呼ぶ**前**に呼ぶ参照チェック
/// （`crate::sink`のモジュール doc comment「FK を張らない理由」参照）。
/// 1件でも参照する sink group があれば`BantoError::Validation`（422）で
/// 拒否する - `banto_tags::PlcConnectionService::delete`が収集グループの
/// 参照を数えて拒否するのと同じ形（RESTRICT 相当）。
pub async fn reject_delete_if_referenced_by_sink_group(
    conn: &mut SqliteConnection,
    connection_id: i64,
) -> Result<(), BantoError> {
    let referencing_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM hub_sink_groups WHERE db_connection_id = ?")
            .bind(connection_id)
            .fetch_one(&mut *conn)
            .await
            .map_err(banto_storage::storage_error)?;
    if referencing_count > 0 {
        return Err(BantoError::Validation {
            field_errors: vec![FieldError {
                field: "id".to_string(),
                message: format!(
                    "この接続を使用している sink group が{referencing_count}件あるため削除できません"
                ),
            }],
        });
    }
    Ok(())
}

/// タグ削除（`crate::rest`の`tags_delete`/`tags_batch_delete`）が呼ぶ
/// クリーンアップ - 対応する`hub_sink_group_tags`行を能動的に消す
/// （`crate::sink`のモジュール doc comment「FK を張らない理由」参照。
/// カスケード削除経路では呼ばない孤児許容の対象外にする、最も単純で
/// 頻度の高い削除経路）。
pub async fn remove_tag_from_all_sink_groups(
    conn: &mut SqliteConnection,
    tag_id: i64,
) -> Result<(), BantoError> {
    sqlx::query("DELETE FROM hub_sink_group_tags WHERE tag_id = ?")
        .bind(tag_id)
        .execute(&mut *conn)
        .await
        .map_err(banto_storage::storage_error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrate_memory;

    async fn seed_postgres_connection(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO plc_connections (name, protocol, host, port, database, username) \
             VALUES ('pg1', 'postgres', 'localhost', 5432, 'db1', 'user1') RETURNING id",
        )
        .fetch_one(pool)
        .await
        .expect("insert postgres connection")
    }

    async fn seed_modbus_connection(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO plc_connections (name, protocol, host, port) \
             VALUES ('plc1', 'modbus-tcp', '127.0.0.1', 502) RETURNING id",
        )
        .fetch_one(pool)
        .await
        .expect("insert modbus connection")
    }

    async fn seed_tag(pool: &SqlitePool, connection_id: i64, name: &str) -> i64 {
        let group_id: i64 = sqlx::query_scalar(
            "INSERT INTO collection_groups (name, plc_connection_id, period_ms) \
             VALUES (?, ?, 1000) RETURNING id",
        )
        .bind(format!("group-{name}"))
        .bind(connection_id)
        .fetch_one(pool)
        .await
        .expect("insert group");
        sqlx::query_scalar(
            "INSERT INTO tags (name, collection_group_id, address, data_type) \
             VALUES (?, ?, '40001', 'i16') RETURNING id",
        )
        .bind(name)
        .bind(group_id)
        .fetch_one(pool)
        .await
        .expect("insert tag")
    }

    fn sample_input(db_connection_id: i64, tag_ids: Vec<i64>) -> SinkGroupInput {
        SinkGroupInput {
            name: "line1-log".to_string(),
            db_connection_id,
            mode: "interval".to_string(),
            interval_ms: 1000,
            table_name: "public.tag_history".to_string(),
            store_bad: false,
            enabled: true,
            tag_ids,
        }
    }

    #[tokio::test]
    async fn create_then_get_round_trips() {
        let pool = migrate_memory().await.unwrap();
        let conn_id = seed_postgres_connection(&pool).await;
        let tag_id = seed_tag(&pool, conn_id, "temp01").await;
        let service = SinkGroupService::new(pool);

        let created = service
            .create(sample_input(conn_id, vec![tag_id]))
            .await
            .expect("create");
        assert_eq!(created.name, "line1-log");
        assert_eq!(created.tag_ids, vec![tag_id]);

        let fetched = service.get(created.id).await.expect("get");
        assert_eq!(fetched, created);
    }

    #[tokio::test]
    async fn create_rejects_non_postgres_connection() {
        let pool = migrate_memory().await.unwrap();
        let conn_id = seed_modbus_connection(&pool).await;
        let tag_id = seed_tag(&pool, conn_id, "temp01").await;
        let service = SinkGroupService::new(pool);

        let err = service
            .create(sample_input(conn_id, vec![tag_id]))
            .await
            .expect_err("modbus-tcp connection must be rejected");
        match err {
            BantoError::Validation { field_errors } => {
                assert!(field_errors.iter().any(|e| e.field == "dbConnectionId"));
            }
            other => panic!("expected validation error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_rejects_unknown_connection() {
        let pool = migrate_memory().await.unwrap();
        let conn_id = seed_postgres_connection(&pool).await;
        let tag_id = seed_tag(&pool, conn_id, "temp01").await;
        let service = SinkGroupService::new(pool);

        let err = service
            .create(sample_input(9999, vec![tag_id]))
            .await
            .expect_err("unknown connection must be rejected");
        match err {
            BantoError::Validation { field_errors } => {
                assert!(field_errors.iter().any(|e| e.field == "dbConnectionId"));
            }
            other => panic!("expected validation error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_rejects_unknown_tag() {
        let pool = migrate_memory().await.unwrap();
        let conn_id = seed_postgres_connection(&pool).await;
        let service = SinkGroupService::new(pool);

        let err = service
            .create(sample_input(conn_id, vec![9999]))
            .await
            .expect_err("unknown tag must be rejected");
        match err {
            BantoError::Validation { field_errors } => {
                assert!(field_errors.iter().any(|e| e.field == "tagIds"));
            }
            other => panic!("expected validation error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_rejects_empty_tag_ids() {
        let pool = migrate_memory().await.unwrap();
        let conn_id = seed_postgres_connection(&pool).await;
        let service = SinkGroupService::new(pool);

        let err = service
            .create(sample_input(conn_id, vec![]))
            .await
            .expect_err("empty tag_ids must be rejected");
        match err {
            BantoError::Validation { field_errors } => {
                assert!(field_errors.iter().any(|e| e.field == "tagIds"));
            }
            other => panic!("expected validation error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_rejects_duplicate_tag_ids() {
        let pool = migrate_memory().await.unwrap();
        let conn_id = seed_postgres_connection(&pool).await;
        let tag_id = seed_tag(&pool, conn_id, "temp01").await;
        let service = SinkGroupService::new(pool);

        let err = service
            .create(sample_input(conn_id, vec![tag_id, tag_id]))
            .await
            .expect_err("duplicate tag_ids must be rejected");
        match err {
            BantoError::Validation { field_errors } => {
                assert!(field_errors.iter().any(|e| e.field == "tagIds"));
            }
            other => panic!("expected validation error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_rejects_bad_mode() {
        let pool = migrate_memory().await.unwrap();
        let conn_id = seed_postgres_connection(&pool).await;
        let tag_id = seed_tag(&pool, conn_id, "temp01").await;
        let service = SinkGroupService::new(pool);
        let mut input = sample_input(conn_id, vec![tag_id]);
        input.mode = "hourly".to_string();

        let err = service
            .create(input)
            .await
            .expect_err("unknown mode must be rejected");
        match err {
            BantoError::Validation { field_errors } => {
                assert!(field_errors.iter().any(|e| e.field == "mode"));
            }
            other => panic!("expected validation error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_rejects_out_of_range_interval_ms() {
        let pool = migrate_memory().await.unwrap();
        let conn_id = seed_postgres_connection(&pool).await;
        let tag_id = seed_tag(&pool, conn_id, "temp01").await;
        let service = SinkGroupService::new(pool);
        let mut input = sample_input(conn_id, vec![tag_id]);
        input.interval_ms = 50;

        let err = service
            .create(input)
            .await
            .expect_err("too-small interval_ms must be rejected");
        match err {
            BantoError::Validation { field_errors } => {
                assert!(field_errors.iter().any(|e| e.field == "intervalMs"));
            }
            other => panic!("expected validation error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_rejects_invalid_table_name() {
        let pool = migrate_memory().await.unwrap();
        let conn_id = seed_postgres_connection(&pool).await;
        let tag_id = seed_tag(&pool, conn_id, "temp01").await;
        let service = SinkGroupService::new(pool);
        let mut input = sample_input(conn_id, vec![tag_id]);
        input.table_name = "\"quoted table\"".to_string();

        let err = service
            .create(input)
            .await
            .expect_err("quoted table_name must be rejected");
        match err {
            BantoError::Validation { field_errors } => {
                let table_name_error = field_errors.iter().find(|e| e.field == "tableName");
                assert!(table_name_error.is_some(), "expected tableName field error");
                // エラーメッセージに連続する空白（ダブルスペース）がないことを確認
                assert!(
                    !table_name_error.unwrap().message.contains("  "),
                    "error message should not contain double spaces"
                );
            }
            other => panic!("expected validation error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_accepts_schema_qualified_table_name() {
        let pool = migrate_memory().await.unwrap();
        let conn_id = seed_postgres_connection(&pool).await;
        let tag_id = seed_tag(&pool, conn_id, "temp01").await;
        let service = SinkGroupService::new(pool);
        let mut input = sample_input(conn_id, vec![tag_id]);
        input.table_name = "reporting.tag_history".to_string();

        let created = service.create(input).await.expect("schema-qualified ok");
        assert_eq!(created.table_name, "reporting.tag_history");
    }

    #[tokio::test]
    async fn create_rejects_duplicate_name() {
        let pool = migrate_memory().await.unwrap();
        let conn_id = seed_postgres_connection(&pool).await;
        let tag_id = seed_tag(&pool, conn_id, "temp01").await;
        let service = SinkGroupService::new(pool);
        service
            .create(sample_input(conn_id, vec![tag_id]))
            .await
            .expect("first create");

        let err = service
            .create(sample_input(conn_id, vec![tag_id]))
            .await
            .expect_err("duplicate name must be rejected");
        match err {
            BantoError::Validation { field_errors } => {
                assert!(field_errors.iter().any(|e| e.field == "name"));
            }
            other => panic!("expected validation error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn update_can_keep_its_own_name() {
        let pool = migrate_memory().await.unwrap();
        let conn_id = seed_postgres_connection(&pool).await;
        let tag_id = seed_tag(&pool, conn_id, "temp01").await;
        let service = SinkGroupService::new(pool);
        let created = service
            .create(sample_input(conn_id, vec![tag_id]))
            .await
            .expect("create");

        let mut input = sample_input(conn_id, vec![tag_id]);
        input.interval_ms = 2000;
        let updated = service
            .update(created.id, input)
            .await
            .expect("update with unchanged name must succeed");
        assert_eq!(updated.interval_ms, 2000);
    }

    #[tokio::test]
    async fn update_replaces_tag_membership() {
        let pool = migrate_memory().await.unwrap();
        let conn_id = seed_postgres_connection(&pool).await;
        let tag1 = seed_tag(&pool, conn_id, "temp01").await;
        let tag2 = seed_tag(&pool, conn_id, "temp02").await;
        let service = SinkGroupService::new(pool);
        let created = service
            .create(sample_input(conn_id, vec![tag1]))
            .await
            .expect("create");

        let input = sample_input(conn_id, vec![tag2]);
        let updated = service.update(created.id, input).await.expect("update");
        assert_eq!(updated.tag_ids, vec![tag2]);
    }

    #[tokio::test]
    async fn update_missing_id_is_not_found() {
        let pool = migrate_memory().await.unwrap();
        let conn_id = seed_postgres_connection(&pool).await;
        let tag_id = seed_tag(&pool, conn_id, "temp01").await;
        let service = SinkGroupService::new(pool);

        let err = service
            .update(9999, sample_input(conn_id, vec![tag_id]))
            .await
            .expect_err("missing id must 404");
        assert!(matches!(err, BantoError::NotFound { .. }));
    }

    #[tokio::test]
    async fn delete_then_get_is_not_found() {
        let pool = migrate_memory().await.unwrap();
        let conn_id = seed_postgres_connection(&pool).await;
        let tag_id = seed_tag(&pool, conn_id, "temp01").await;
        let service = SinkGroupService::new(pool);
        let created = service
            .create(sample_input(conn_id, vec![tag_id]))
            .await
            .expect("create");

        service.delete(created.id).await.expect("delete");
        let err = service.get(created.id).await.expect_err("must 404");
        assert!(matches!(err, BantoError::NotFound { .. }));
    }

    #[tokio::test]
    async fn delete_missing_id_is_not_found() {
        let pool = migrate_memory().await.unwrap();
        let service = SinkGroupService::new(pool);
        let err = service.delete(9999).await.expect_err("must 404");
        assert!(matches!(err, BantoError::NotFound { .. }));
    }

    #[tokio::test]
    async fn list_enabled_excludes_disabled_groups() {
        let pool = migrate_memory().await.unwrap();
        let conn_id = seed_postgres_connection(&pool).await;
        let tag_id = seed_tag(&pool, conn_id, "temp01").await;
        let service = SinkGroupService::new(pool);
        let mut disabled_input = sample_input(conn_id, vec![tag_id]);
        disabled_input.name = "disabled-group".to_string();
        disabled_input.enabled = false;
        service
            .create(disabled_input)
            .await
            .expect("create disabled");
        service
            .create(sample_input(conn_id, vec![tag_id]))
            .await
            .expect("create enabled");

        let enabled = service.list_enabled().await.expect("list_enabled");
        assert_eq!(enabled.len(), 1);
        assert_eq!(enabled[0].name, "line1-log");
    }

    #[tokio::test]
    async fn reject_delete_if_referenced_by_sink_group_blocks_the_referenced_connection() {
        let pool = migrate_memory().await.unwrap();
        let conn_id = seed_postgres_connection(&pool).await;
        let tag_id = seed_tag(&pool, conn_id, "temp01").await;
        let service = SinkGroupService::new(pool.clone());
        service
            .create(sample_input(conn_id, vec![tag_id]))
            .await
            .expect("create");

        let mut conn = pool.acquire().await.unwrap();
        let err = reject_delete_if_referenced_by_sink_group(&mut conn, conn_id)
            .await
            .expect_err("referenced connection must be rejected");
        assert!(matches!(err, BantoError::Validation { .. }));
    }

    #[tokio::test]
    async fn reject_delete_if_referenced_by_sink_group_allows_unreferenced_connection() {
        let pool = migrate_memory().await.unwrap();
        let conn_id = seed_postgres_connection(&pool).await;
        let mut conn = pool.acquire().await.unwrap();
        reject_delete_if_referenced_by_sink_group(&mut conn, conn_id)
            .await
            .expect("unreferenced connection must be allowed");
    }

    #[tokio::test]
    async fn remove_tag_from_all_sink_groups_clears_membership() {
        let pool = migrate_memory().await.unwrap();
        let conn_id = seed_postgres_connection(&pool).await;
        let tag1 = seed_tag(&pool, conn_id, "temp01").await;
        let tag2 = seed_tag(&pool, conn_id, "temp02").await;
        let service = SinkGroupService::new(pool.clone());
        let created = service
            .create(sample_input(conn_id, vec![tag1, tag2]))
            .await
            .expect("create");

        let mut conn = pool.acquire().await.unwrap();
        remove_tag_from_all_sink_groups(&mut conn, tag1)
            .await
            .expect("remove tag1");
        drop(conn);

        let fetched = service.get(created.id).await.expect("get");
        assert_eq!(fetched.tag_ids, vec![tag2]);
    }
}
