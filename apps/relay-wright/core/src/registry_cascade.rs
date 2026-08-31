//! Cascade delete for the tag registry (feature/easy-delete): delete a PLC
//! connection together with its collection groups and tags, or a collection
//! group together with its tags, in ONE transaction.
//!
//! banto-tags' own `PlcConnectionService::delete`/`CollectionGroupService::delete`
//! deliberately REFUSE while children exist ("使用中の…削除は拒否" - shared
//! registry semantics ChronoGazer relies on), and banto-tags must not be
//! modified from this app (invariant). relay-wright is a DEBUG tool where
//! wiping a whole connection's registration in one confirmed step is the
//! desired UX, so the cascade lives HERE, in this app's wiring layer, as
//! direct SQL over the same shared pool (`crate::db` bootstraps both schema
//! halves onto one database). Deletion needs no per-row validation - the rows
//! are simply removed, in FK order (tags → collection_groups →
//! plc_connections; both links are `ON DELETE RESTRICT`).
//!
//! Each cascade has a **preview** twin returning the would-be counts WITHOUT
//! deleting, so the UI can show them in its confirm dialog. The preview also
//! counts affected WRITE-side references - `write_targets` rows on the doomed
//! connection and `write_rules` whose conditions/copy-source reference the
//! doomed tags - because those become unresolvable after the cascade (the
//! engine already drops unresolvable rules with a log line at compile time -
//! no crash, but the user should be warned). The cascade itself deliberately
//! does NOT delete write targets/rules: they may be re-pointed at a new
//! registration instead. Note the engine compiles rules at start/reload, so a
//! cascade - like every other registry edit - does not affect already-compiled
//! rules until the next engine rebuild.

use banto_core::BantoError;
use serde::Serialize;
use sqlx::SqlitePool;

const CONNECTION_RESOURCE: &str = "plc_connections";
const GROUP_RESOURCE: &str = "collection_groups";

/// Would-be effects of cascade-deleting a PLC connection: rows that WILL be
/// deleted (`groups`/`tags`) plus write-side references that will merely be
/// left dangling (`write_targets`/`write_rules` - warned about, not deleted).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionCascadePreview {
    pub groups: i64,
    pub tags: i64,
    pub write_targets: i64,
    pub write_rules: i64,
}

/// Would-be effects of cascade-deleting a collection group: `tags` will be
/// deleted; `write_rules` referencing those tags will be left dangling
/// (warned about, not deleted). The group's connection - and therefore its
/// `write_targets` - is unaffected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupCascadePreview {
    pub tags: i64,
    pub write_rules: i64,
}

/// What a connection cascade actually deleted (besides the connection row
/// itself), from the DELETEs' own `rows_affected` inside the transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionCascadeSummary {
    pub groups: u64,
    pub tags: u64,
}

/// What a group cascade actually deleted (besides the group row itself).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupCascadeSummary {
    pub tags: u64,
}

fn not_found(resource: &str, id: i64) -> BantoError {
    BantoError::NotFound {
        resource: resource.to_string(),
        id: id.to_string(),
    }
}

/// Distinct `write_rules` referencing any tag in `doomed_tags_sql` (a
/// parenthesized SELECT of tag ids taking exactly one `?` bind) - either as a
/// condition's `source_tag_id` or as the rule's copy-source
/// (`write_source_tag_id`). These rules are counted for the preview warning
/// only, never deleted here.
async fn count_rules_referencing_tags(
    pool: &SqlitePool,
    doomed_tags_sql: &str,
    id: i64,
) -> Result<i64, BantoError> {
    // AssertSqlSafe: `doomed_tags_sql` は呼び出し元(下記2箇所)で
    // TAGS_OF_CONNECTION/TAGS_OF_GROUP のどちらかの固定文字列定数しか渡さない
    // - 外部入力は一切混入しない（可変な id は `?` プレースホルダでバインド）。
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT COUNT(*) FROM write_rules \
         WHERE write_source_tag_id IN {doomed_tags_sql} \
            OR id IN (SELECT write_rule_id FROM write_rule_conditions \
                      WHERE source_tag_id IN {doomed_tags_sql})"
    )))
    .bind(id)
    .bind(id)
    .fetch_one(pool)
    .await
    .map_err(banto_storage::storage_error)
}

/// Tags that belong (via their group) to connection `?`.
const TAGS_OF_CONNECTION: &str = "(SELECT t.id FROM tags t \
     JOIN collection_groups g ON t.collection_group_id = g.id \
     WHERE g.plc_connection_id = ?)";

/// Tags that belong directly to group `?`.
const TAGS_OF_GROUP: &str = "(SELECT id FROM tags WHERE collection_group_id = ?)";

/// Counts for the connection-cascade confirm dialog, without deleting
/// anything. `NotFound` when the connection does not exist.
pub async fn cascade_preview_plc_connection(
    pool: &SqlitePool,
    id: i64,
) -> Result<ConnectionCascadePreview, BantoError> {
    let exists: Option<i64> = sqlx::query_scalar("SELECT id FROM plc_connections WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await
        .map_err(banto_storage::storage_error)?;
    if exists.is_none() {
        return Err(not_found(CONNECTION_RESOURCE, id));
    }

    let groups: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM collection_groups WHERE plc_connection_id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .map_err(banto_storage::storage_error)?;
    // AssertSqlSafe: TAGS_OF_CONNECTION は本ファイル内の固定文字列定数
    // （外部入力は含まれない、可変な id は `?` プレースホルダでバインド）。
    let tags: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT COUNT(*) FROM {TAGS_OF_CONNECTION}"
    )))
    .bind(id)
    .fetch_one(pool)
    .await
    .map_err(banto_storage::storage_error)?;
    let write_targets: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM write_targets WHERE plc_connection_id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .map_err(banto_storage::storage_error)?;
    let write_rules = count_rules_referencing_tags(pool, TAGS_OF_CONNECTION, id).await?;

    Ok(ConnectionCascadePreview {
        groups,
        tags,
        write_targets,
        write_rules,
    })
}

/// Counts for the group-cascade confirm dialog, without deleting anything.
/// `NotFound` when the group does not exist.
pub async fn cascade_preview_collection_group(
    pool: &SqlitePool,
    id: i64,
) -> Result<GroupCascadePreview, BantoError> {
    let exists: Option<i64> = sqlx::query_scalar("SELECT id FROM collection_groups WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await
        .map_err(banto_storage::storage_error)?;
    if exists.is_none() {
        return Err(not_found(GROUP_RESOURCE, id));
    }

    let tags: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tags WHERE collection_group_id = ?")
        .bind(id)
        .fetch_one(pool)
        .await
        .map_err(banto_storage::storage_error)?;
    let write_rules = count_rules_referencing_tags(pool, TAGS_OF_GROUP, id).await?;

    Ok(GroupCascadePreview { tags, write_rules })
}

/// Delete connection `id` and everything under it - its groups' tags, then
/// its groups, then the connection row - atomically, in one transaction and
/// in FK order. A connection with no children behaves exactly like a plain
/// delete (`{groups: 0, tags: 0}`). `NotFound` when the connection does not
/// exist (checked inside the same transaction).
pub async fn cascade_delete_plc_connection(
    pool: &SqlitePool,
    id: i64,
) -> Result<ConnectionCascadeSummary, BantoError> {
    let mut tx = pool.begin().await.map_err(banto_storage::storage_error)?;

    let exists: Option<i64> = sqlx::query_scalar("SELECT id FROM plc_connections WHERE id = ?")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(banto_storage::storage_error)?;
    if exists.is_none() {
        return Err(not_found(CONNECTION_RESOURCE, id));
    }

    // AssertSqlSafe: TAGS_OF_CONNECTION は本ファイル内の固定文字列定数
    // （外部入力は含まれない、可変な id は `?` プレースホルダでバインド）。
    let tags = sqlx::query(sqlx::AssertSqlSafe(format!(
        "DELETE FROM tags WHERE id IN {TAGS_OF_CONNECTION}"
    )))
    .bind(id)
    .execute(&mut *tx)
    .await
    .map_err(banto_storage::storage_error)?
    .rows_affected();
    let groups = sqlx::query("DELETE FROM collection_groups WHERE plc_connection_id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(banto_storage::storage_error)?
        .rows_affected();
    sqlx::query("DELETE FROM plc_connections WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(banto_storage::storage_error)?;

    tx.commit().await.map_err(banto_storage::storage_error)?;
    Ok(ConnectionCascadeSummary { groups, tags })
}

/// Delete group `id` and its tags atomically, in one transaction and in FK
/// order (tags first). A group with no tags behaves exactly like a plain
/// delete (`{tags: 0}`). `NotFound` when the group does not exist.
pub async fn cascade_delete_collection_group(
    pool: &SqlitePool,
    id: i64,
) -> Result<GroupCascadeSummary, BantoError> {
    let mut tx = pool.begin().await.map_err(banto_storage::storage_error)?;

    let exists: Option<i64> = sqlx::query_scalar("SELECT id FROM collection_groups WHERE id = ?")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(banto_storage::storage_error)?;
    if exists.is_none() {
        return Err(not_found(GROUP_RESOURCE, id));
    }

    let tags = sqlx::query("DELETE FROM tags WHERE collection_group_id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(banto_storage::storage_error)?
        .rows_affected();
    sqlx::query("DELETE FROM collection_groups WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(banto_storage::storage_error)?;

    tx.commit().await.map_err(banto_storage::storage_error)?;
    Ok(GroupCascadeSummary { tags })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::init_db_memory;
    use banto_tags::{
        CollectionGroupInput, CollectionGroupService, PlcConnectionInput, PlcConnectionService,
        TagInput, TagService,
    };

    /// Fully-migrated in-memory DB seeded with: connection C1 (2 groups, 3
    /// tags: G1={T1,T2}, G2={T3}) and connection C2 (1 group G3, 1 tag T4) -
    /// so every assertion can also check the OTHER connection's rows
    /// survived. Returns (pool, c1_id, c2_id, g1_id, g2_id, tag_ids).
    async fn seeded() -> (SqlitePool, i64, i64, i64, i64, Vec<i64>) {
        let pool = init_db_memory().await.expect("init_db_memory");
        let connections = PlcConnectionService::new(pool.clone());
        let groups = CollectionGroupService::new(pool.clone());
        let tags = TagService::new(pool.clone());

        let mut conn_ids = Vec::new();
        for name in ["C1", "C2"] {
            let conn = connections
                .create(PlcConnectionInput {
                    name: name.to_string(),
                    protocol: "slmp".to_string(),
                    host: "10.0.0.1".to_string(),
                    port: 5007,
                    unit_id: 1,
                    enabled: true,
                    simulation: false,

                    word_order: "low_high".to_string(),
                })
                .await
                .expect("seed connection");
            conn_ids.push(conn.id);
        }

        let mut group_ids = Vec::new();
        for (name, conn_id) in [
            ("G1", conn_ids[0]),
            ("G2", conn_ids[0]),
            ("G3", conn_ids[1]),
        ] {
            let group = groups
                .create(CollectionGroupInput {
                    name: name.to_string(),
                    plc_connection_id: conn_id,
                    period_ms: 1_000,
                    enabled: true,
                })
                .await
                .expect("seed group");
            group_ids.push(group.id);
        }

        let mut tag_ids = Vec::new();
        for (name, address, group_id) in [
            ("T1", "D100", group_ids[0]),
            ("T2", "D101", group_ids[0]),
            ("T3", "D102", group_ids[1]),
            ("T4", "D103", group_ids[2]),
        ] {
            let tag = tags
                .create(TagInput {
                    name: name.to_string(),
                    collection_group_id: group_id,
                    address: address.to_string(),
                    data_type: "i16".to_string(),
                    raw_lo: None,
                    raw_hi: None,
                    eng_lo: None,
                    eng_hi: None,
                    unit: None,
                    decimals: 0,
                    threshold_h: None,
                    threshold_hh: None,
                    threshold_l: None,
                    threshold_ll: None,
                    string_length: None,
                    enabled: true,
                    writable: false,
                    tag_kind: "plc".to_string(),
                    expression: None,
                    retain: false,
                    expected_revision: None,
                })
                .await
                .expect("seed tag");
            tag_ids.push(tag.id);
        }

        (
            pool,
            conn_ids[0],
            conn_ids[1],
            group_ids[0],
            group_ids[1],
            tag_ids,
        )
    }

    // AssertSqlSafe: テスト専用ヘルパー。呼び出し元はテスト内で採番した
    // 整数 id を `format!` で埋め込んだ SQL のみを渡す（外部入力は無い）。
    async fn count(pool: &SqlitePool, sql: &str) -> i64 {
        sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn connection_cascade_deletes_groups_tags_and_the_connection() {
        let (pool, c1, c2, _g1, _g2, _tags) = seeded().await;

        let summary = cascade_delete_plc_connection(&pool, c1)
            .await
            .expect("cascade should succeed");
        assert_eq!(summary, ConnectionCascadeSummary { groups: 2, tags: 3 });

        // C1's whole subtree is gone...
        assert_eq!(
            count(
                &pool,
                &format!("SELECT COUNT(*) FROM plc_connections WHERE id = {c1}")
            )
            .await,
            0
        );
        assert_eq!(
            count(
                &pool,
                &format!("SELECT COUNT(*) FROM collection_groups WHERE plc_connection_id = {c1}")
            )
            .await,
            0
        );
        // ...while C2's subtree survived untouched.
        assert_eq!(
            count(
                &pool,
                &format!("SELECT COUNT(*) FROM plc_connections WHERE id = {c2}")
            )
            .await,
            1
        );
        assert_eq!(
            count(&pool, "SELECT COUNT(*) FROM collection_groups").await,
            1
        );
        assert_eq!(count(&pool, "SELECT COUNT(*) FROM tags").await, 1);
        // Nothing dangling.
        let violations: Vec<(String,)> = sqlx::query_as("PRAGMA foreign_key_check")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert!(violations.is_empty(), "dangling FKs: {violations:?}");
    }

    #[tokio::test]
    async fn connection_cascade_without_children_behaves_like_plain_delete() {
        let (pool, _c1, c2, _g1, _g2, _tags) = seeded().await;
        // Give C2 no children by cascading its group first.
        let pre = cascade_delete_collection_group(
            &pool,
            count(
                &pool,
                &format!("SELECT id FROM collection_groups WHERE plc_connection_id = {c2}"),
            )
            .await,
        )
        .await
        .expect("clear C2's group");
        assert_eq!(pre, GroupCascadeSummary { tags: 1 });

        let summary = cascade_delete_plc_connection(&pool, c2)
            .await
            .expect("childless cascade should succeed");
        assert_eq!(summary, ConnectionCascadeSummary { groups: 0, tags: 0 });
        assert_eq!(
            count(
                &pool,
                &format!("SELECT COUNT(*) FROM plc_connections WHERE id = {c2}")
            )
            .await,
            0
        );
    }

    #[tokio::test]
    async fn group_cascade_deletes_its_tags_and_the_group_only() {
        let (pool, c1, _c2, g1, g2, _tags) = seeded().await;

        let summary = cascade_delete_collection_group(&pool, g1)
            .await
            .expect("group cascade should succeed");
        assert_eq!(summary, GroupCascadeSummary { tags: 2 });

        assert_eq!(
            count(
                &pool,
                &format!("SELECT COUNT(*) FROM collection_groups WHERE id = {g1}")
            )
            .await,
            0
        );
        // Sibling group, its tag, and the parent connection all survived.
        assert_eq!(
            count(
                &pool,
                &format!("SELECT COUNT(*) FROM collection_groups WHERE id = {g2}")
            )
            .await,
            1
        );
        assert_eq!(
            count(
                &pool,
                &format!("SELECT COUNT(*) FROM tags WHERE collection_group_id = {g2}")
            )
            .await,
            1
        );
        assert_eq!(
            count(
                &pool,
                &format!("SELECT COUNT(*) FROM plc_connections WHERE id = {c1}")
            )
            .await,
            1
        );
    }

    #[tokio::test]
    async fn previews_report_counts_without_deleting_anything() {
        let (pool, c1, _c2, g1, _g2, tags) = seeded().await;

        // Write-side references: 2 targets on C1, and 2 rules touching C1's
        // tags - one via a condition (T1), one via copy-source (T3). A third
        // rule references only T4 (C2's tag) and must NOT be counted.
        sqlx::query(
            "INSERT INTO write_targets (name, plc_connection_id, address, data_type) \
             VALUES ('WT1', ?, 'D200', 'u16'), ('WT2', ?, 'D201', 'u16')",
        )
        .bind(c1)
        .bind(c1)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO write_targets (id, name, plc_connection_id, address, data_type) \
             VALUES (99, 'WTX', 12345, 'D202', 'u16')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO write_rules (id, name, edge_mode, write_target_id, write_value_mode, write_constant_value) \
             VALUES (1, 'R1', 'rising', 99, 'constant', 1.0), \
                    (2, 'R2', 'rising', 99, 'copy_from_source', NULL), \
                    (3, 'R3', 'rising', 99, 'constant', 1.0)",
        )
        .execute(&pool)
        .await
        .unwrap();
        // R1: condition on T1 (in C1). R2: copy-source T3 (in C1). R3:
        // condition on T4 (in C2 - out of scope for C1's preview).
        sqlx::query(
            "INSERT INTO write_rule_conditions (write_rule_id, source_tag_id, operator, threshold_value) \
             VALUES (1, ?, 'gt', 10.0), (3, ?, 'gt', 10.0)",
        )
        .bind(tags[0])
        .bind(tags[3])
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("UPDATE write_rules SET write_source_tag_id = ? WHERE id = 2")
            .bind(tags[2])
            .execute(&pool)
            .await
            .unwrap();

        let preview = cascade_preview_plc_connection(&pool, c1)
            .await
            .expect("preview should succeed");
        assert_eq!(
            preview,
            ConnectionCascadePreview {
                groups: 2,
                tags: 3,
                write_targets: 2,
                write_rules: 2,
            }
        );

        // G1's preview: its 2 tags, and only R1 (condition on T1) - R2's
        // copy-source T3 lives in G2.
        let group_preview = cascade_preview_collection_group(&pool, g1)
            .await
            .expect("group preview should succeed");
        assert_eq!(
            group_preview,
            GroupCascadePreview {
                tags: 2,
                write_rules: 1,
            }
        );

        // Nothing was deleted by either preview.
        assert_eq!(
            count(&pool, "SELECT COUNT(*) FROM plc_connections").await,
            2
        );
        assert_eq!(
            count(&pool, "SELECT COUNT(*) FROM collection_groups").await,
            3
        );
        assert_eq!(count(&pool, "SELECT COUNT(*) FROM tags").await, 4);
        assert_eq!(count(&pool, "SELECT COUNT(*) FROM write_targets").await, 3);
        assert_eq!(count(&pool, "SELECT COUNT(*) FROM write_rules").await, 3);
    }

    #[tokio::test]
    async fn cascade_does_not_delete_write_targets_or_rules() {
        let (pool, c1, _c2, _g1, _g2, tags) = seeded().await;
        sqlx::query(
            "INSERT INTO write_targets (id, name, plc_connection_id, address, data_type) \
             VALUES (7, 'WT7', ?, 'D200', 'u16')",
        )
        .bind(c1)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO write_rules (id, name, edge_mode, write_target_id, write_value_mode, \
                write_constant_value, write_source_tag_id) \
             VALUES (1, 'R1', 'rising', 7, 'constant', 1.0, ?)",
        )
        .bind(tags[0])
        .execute(&pool)
        .await
        .unwrap();

        cascade_delete_plc_connection(&pool, c1)
            .await
            .expect("cascade should succeed");

        // The write-side rows survive (to be re-pointed by the user), even
        // though their references are now dangling by design.
        assert_eq!(count(&pool, "SELECT COUNT(*) FROM write_targets").await, 1);
        assert_eq!(count(&pool, "SELECT COUNT(*) FROM write_rules").await, 1);
    }

    #[tokio::test]
    async fn missing_ids_are_not_found_everywhere() {
        let (pool, _c1, _c2, _g1, _g2, _tags) = seeded().await;
        for err in [
            cascade_preview_plc_connection(&pool, 999)
                .await
                .unwrap_err(),
            cascade_delete_plc_connection(&pool, 999).await.unwrap_err(),
        ] {
            assert!(
                matches!(&err, BantoError::NotFound { resource, id }
                    if resource == "plc_connections" && id == "999"),
                "expected plc_connections NotFound, got {err:?}"
            );
        }
        for err in [
            cascade_preview_collection_group(&pool, 999)
                .await
                .unwrap_err(),
            cascade_delete_collection_group(&pool, 999)
                .await
                .unwrap_err(),
        ] {
            assert!(
                matches!(&err, BantoError::NotFound { resource, id }
                    if resource == "collection_groups" && id == "999"),
                "expected collection_groups NotFound, got {err:?}"
            );
        }
    }
}
