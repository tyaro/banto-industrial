//! DB Source の実行計画（docs/banto-hub-external-db-design.md §4.2「実行
//! モデル」・§4.5「稼働中の設定変更」）。
//!
//! [`crate::computed::build_plan`] と全く同じ立ち位置の**純関数**
//! [`build_plan`] が、レジストリのスナップショット1つから「どの接続の
//! どのグループを何 ms ごとにどの SQL で回し、その結果のどの列をどの
//! タグへ流すか」を組み立てる。副作用は一切持たず、
//! [`crate::db_source::DbSourceEngine::commit`] に渡すまで何も起きない -
//! `crate::hub::CollectorManager::rebuild`/`commit_catalog` が catalog /
//! `Collector` / 演算タグ plan の入れ替えと**同じ all-or-nothing の1ステップ**
//! として commit するため（設計 §4.3(a)「変更の影響半径 = 触ったものだけ」、
//! `hub.rs` の `computed` フィールド doc comment 参照）。
//!
//! ## なぜ `TagMap` ではなくレジストリのスナップショットから作るのか
//!
//! 演算タグの plan は `TagMap`（catalog）だけで組める - 必要なのは外部名と
//! 式だけだからである。DB Source はそうはいかない: 接続の資格情報
//! （`host`/`port`/`database`/`username`/`password`）とグループの `query_sql`
//! は catalog に載っていない（載せてはいけない - catalog は外部 API に
//! そのまま出る）。よって [`build_plan`] は
//! [`banto_collect::RegistrySnapshot`] を受け取り、`tag_key`
//! （`"tag:{id}"`）だけを `crate::hub` と同じ規約で自前に組み立てる。
//!
//! ## パスワードの扱い（R2b-1e、docs/banto-hub-connection-settings-design.md §5.2・§5.4）
//!
//! [`DbConnectionPlan`] は平文のパスワードを**持たない**。持つのは封印した行
//! （[`crate::secrets::SealedSecret`]、参照・方式・blob）だけで、ポーリングタスクが接続を
//! 張る直前に開く（`crate::db_source::task`、平文は `Zeroizing`）。計画を組む時点で一度
//! 開けることを確かめ（平文はすぐ捨てる）、読めない接続は [`DbSourcePlan::unreadable`] に
//! 回す（タスクを起こさない = 再試行でも繋ぎに行かない）。checklist §5「平文の資格情報は、
//! それを持つ層の外に出さない」。`Debug` は手書きで、封印した値も `"<sealed>"` に置き換える
//! （`plain-v1` の blob は平文そのもの。`SealedSecret` の `Debug` も blob を出さないが、
//! 計画の `Debug` は参照も出さない）。

use std::fmt;

use banto_collect::{ExclusionReason, RegistrySnapshot};
use banto_driver_api::DriverRegistry;
use banto_tags::{DB_TAG_KIND, POSTGRES_PROTOCOL};

use crate::secrets::{SealedSecret, UnreadableReason};

/// 1回の commit で engine が受け取る計画一式。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DbSourcePlan {
    /// 実際にポーリングタスクを走らせる接続（有効・かつ回すグループが1つ
    /// 以上あるもの）。
    pub connections: Vec<DbConnectionPlan>,
    /// レジストリに存在するが**無効**な `postgres` 接続の
    /// `(id, name)`。タスクは起動しないが、状態表示で `disabled` として
    /// 見せるために持ち回る（[`crate::db_source::status`]）。
    pub disabled: Vec<(i64, String)>,
    /// R2b-1d（docs/banto-hub-connection-settings-design.md §5.4・§5.5）: 有効で回す
    /// グループもあるが、**パスワードを読めない**接続（秘密値の行が無い・この PC で
    /// 開けない）。タスクを起動せず（再試行でも繋ぎに行かない）、状態表示はその接続だけ
    /// `error`「パスワード: 読めません」、配下のタグは Bad（[`crate::db_source::DbSourceEngine`]
    /// が書く）。空のパスワードで繋がない（`trust` 認証の DB なら別の資格で繋がって
    /// しまう、checklist §5「エラーを空に潰さない」）。
    pub unreadable: Vec<UnreadableDbConnection>,
    /// PR #777 のオーナーレビュー P2（設計 §4.4・§3.4）: 有効で回すグループもあるが、
    /// レジストリで確かめると使えない接続（`postgres` のドライバが無い・`settings` が
    /// 検証を通らない・設定の版がドライバより新しい。`banto_collect::driver_exclusion`、
    /// PLC の収集と同じ判断）。[`Self::unreadable`] と同じく、タスクを起動せず、状態表示は
    /// その接続だけ `error`（理由の文言）、配下のタグは Bad。
    pub excluded: Vec<ExcludedDbConnection>,
}

/// [`DbSourcePlan`] の中で、タスクを起こさず配下のタグを Bad にする接続 1 件
/// （[`DbSourcePlan::unreadable`] と [`DbSourcePlan::excluded`] をまとめて読む形。
/// [`DbSourcePlan::blocked`]）。
#[derive(Debug, Clone, Copy)]
pub struct BlockedDbConnection<'a> {
    pub connection_id: i64,
    pub connection_name: &'a str,
    pub groups: &'a [DbGroupPlan],
    /// パスワードを読めないので外した接続ならその理由（[`DbSourcePlan::unreadable`]）。
    /// レジストリで外した接続（[`DbSourcePlan::excluded`]）は `None`。
    pub password_unreadable: Option<UnreadableReason>,
}

impl DbSourcePlan {
    /// タスクを起こさない（配下のタグを Bad にし、状態を `error` にする）接続を、
    /// 状態の画面の文言と一緒に並べる（パスワードを読めない接続、次にレジストリで外した接続）。
    pub fn blocked(&self) -> impl Iterator<Item = (BlockedDbConnection<'_>, String)> + '_ {
        let unreadable = self.unreadable.iter().map(|conn| {
            (
                BlockedDbConnection {
                    connection_id: conn.connection_id,
                    connection_name: &conn.connection_name,
                    groups: &conn.groups,
                    password_unreadable: Some(conn.reason),
                },
                unreadable_password_message(conn.reason),
            )
        });
        let excluded = self.excluded.iter().map(|conn| {
            (
                BlockedDbConnection {
                    connection_id: conn.connection_id,
                    connection_name: &conn.connection_name,
                    groups: &conn.groups,
                    password_unreadable: None,
                },
                conn.reason.message(),
            )
        });
        unreadable.chain(excluded)
    }
}

/// PR #777 のオーナーレビュー P2: レジストリで確かめると使えないので起動しない接続 1 件
/// （[`DbSourcePlan::excluded`]）。`groups` は配下のタグへ Bad を書くため。
#[derive(Debug, Clone, PartialEq)]
pub struct ExcludedDbConnection {
    pub connection_id: i64,
    pub connection_name: String,
    pub reason: ExclusionReason,
    pub groups: Vec<DbGroupPlan>,
}

impl DbSourcePlan {
    pub fn is_empty(&self) -> bool {
        self.connections.is_empty()
            && self.disabled.is_empty()
            && self.unreadable.is_empty()
            && self.excluded.is_empty()
    }
}

/// R2b-1d: パスワードを読めないので起動しない接続 1 件（[`DbSourcePlan::unreadable`]）。
/// パスワードも参照も持たない。`groups` は配下のタグへ Bad を書くため。
#[derive(Debug, Clone, PartialEq)]
pub struct UnreadableDbConnection {
    pub connection_id: i64,
    pub connection_name: String,
    pub reason: UnreadableReason,
    pub groups: Vec<DbGroupPlan>,
}

/// 設計 §5.5: パスワードを読めない接続の文言の本体（ドロワーの表示と同じ、R2b-1e）。
pub const UNREADABLE_PASSWORD_MESSAGE: &str =
    "パスワード: 読めません（この PC で再入力してください）";

/// パスワードを読めない接続の理由の文言（状態の画面の `dbSource` 節の `lastError`・接続の
/// テストの `error`）: [`UNREADABLE_PASSWORD_MESSAGE`] に理由の綴り（`missing` /
/// `undecryptable` / `invalid`）を添える。状態の画面は理由を別の欄
/// （`passwordUnreadableReason`）にも出す。秘密値を含まない。
pub fn unreadable_password_message(reason: UnreadableReason) -> String {
    format!("{UNREADABLE_PASSWORD_MESSAGE}（理由: {}）", reason.as_str())
}

/// 1接続分（= tokio task 1本、`PgPool` 1つ）。
#[derive(Clone, PartialEq)]
pub struct DbConnectionPlan {
    pub connection_id: i64,
    pub connection_name: String,
    pub host: String,
    pub port: u16,
    pub database: String,
    pub username: String,
    /// 封印したパスワード（R2b-1e）。`None` はパスワード無し（postgres のパスワードは
    /// 任意）。平文はタスクが接続を張る直前に開く（このモジュールの doc comment
    /// 「パスワードの扱い」節）。`Debug` には出さない。
    pub password: Option<SealedSecret>,
    pub groups: Vec<DbGroupPlan>,
}

impl fmt::Debug for DbConnectionPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DbConnectionPlan")
            .field("connection_id", &self.connection_id)
            .field("connection_name", &self.connection_name)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("database", &self.database)
            .field("username", &self.username)
            .field(
                "password",
                &self
                    .password
                    .as_ref()
                    .map(|_| "<sealed>")
                    .unwrap_or("<none>"),
            )
            .field("groups", &self.groups)
            .finish()
    }
}

/// 1グループ分（= 1 SELECT、`period_ms` ごとに1回）。
#[derive(Debug, Clone, PartialEq)]
pub struct DbGroupPlan {
    pub group_id: i64,
    pub group_name: String,
    pub period_ms: u64,
    pub query_sql: String,
    pub tags: Vec<DbTagPlan>,
}

/// 1タグ分（= 結果列1つ）。
#[derive(Debug, Clone, PartialEq)]
pub struct DbTagPlan {
    /// `crate::hub::TagEntry::tag_key` と同じ `"tag:{id}"` -
    /// [`crate::computed::ServerTagStore`] のキー。
    pub tag_key: String,
    /// 診断ログ用の外部名（`{connection}.{group}.{tag}`）。
    pub external_name: String,
    /// 結果列名（`db` タグの `address`、大文字小文字そのまま）。
    pub column: String,
}

/// 1タグが値を取れない確定的な理由（describe の時点で分かるもの）。
/// 毎ティック Bad を書きつつ、`warn` はプラン単位で1回だけ出す
/// （§4.3 の表「列が無い / 型変換不能 → タグ単位 Bad（起動時と設定変更時に
/// `warn`）」）。
///
/// R2a-3a: 実体は共有 crate の [`banto_db_source::ColumnBadReason`]
/// （`MissingColumn` / `UnsupportedColumnType` / `StatementNotPrepared`）。
/// Hub では「列 = タグ」なので従来の名前で再輸出している。
pub use banto_db_source::ColumnBadReason as TagBadReason;

/// describe の結果とグループの計画を突き合わせた、1グループ分の実行形
/// （[`crate::db_source::task`] が毎ティック使う）。
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedGroup {
    /// 実際に実行する生成済み SQL（[`super::convert::build_wrapper_sql`]）。
    /// 値の取れる列が1つも無いグループでは `None`（SELECT を投げても
    /// 意味が無いので、そのグループはティックごとに全タグ Bad を書くだけ
    /// になる）。
    pub wrapper_sql: Option<String>,
    /// `wrapper_sql` の射影順に並んだタグ - `c{index}` の `index` が
    /// そのままこの `Vec` の添字。
    pub value_tags: Vec<DbTagPlan>,
    /// 値が取れないタグとその理由。
    pub bad_tags: Vec<(DbTagPlan, TagBadReason)>,
}

/// `describe` が返した「列名 → PostgreSQL 型名」に対して、グループの
/// タグを解決する（純関数 - 実 DB を触らないのでユニットテストできる）。
///
/// R2a-3a: 判断の本体は共有 crate の [`banto_db_source::resolve_columns`]
/// （Hub の型に依存しない、列名の並びと添字の入出力）にあり、ここは
/// `group.tags` の列名を渡して、返ってきた添字をタグへ写し戻すだけの
/// アダプタ。
pub fn resolve_group(group: &DbGroupPlan, described: &[(String, String)]) -> ResolvedGroup {
    let wanted: Vec<&str> = group.tags.iter().map(|tag| tag.column.as_str()).collect();
    let resolved = banto_db_source::resolve_columns(&group.query_sql, &wanted, described);

    ResolvedGroup {
        wrapper_sql: resolved.wrapper_sql,
        value_tags: resolved
            .value_columns
            .into_iter()
            .map(|index| group.tags[index].clone())
            .collect(),
        bad_tags: resolved
            .bad_columns
            .into_iter()
            .map(|(index, reason)| (group.tags[index].clone(), reason))
            .collect(),
    }
}

/// `snapshot` から DB Source の実行計画を組み立てる純関数
/// （[`crate::computed::build_plan`] と同じ契約 - `Err` は呼び出し元
/// `crate::hub::CollectorManager::rebuild`/`commit_catalog` が rebuild 全体の
/// 失敗として扱い、catalog も `Collector` も演算 plan も**古いまま**残る）。
///
/// 拾うもの / 落とすもの:
///
/// - `protocol != "postgres"` の接続: 対象外（PLC 側の世界）。
/// - `enabled == false` の `postgres` 接続: タスクを起動せず
///   [`DbSourcePlan::disabled`] に載せるだけ（§4.3 の表「グループ / 接続が
///   無効 → 既存規則」- 値は一切書かず、`effective_sample` の `!enabled`
///   規則が Bad を返す）。
/// - `enabled == false` のグループ・タグ: 同じ理由で単に飛ばす。
/// - 有効な `db` タグが1本も無いグループ: SELECT を投げる意味が無いので
///   飛ばす。同様に、回すグループが1つも無い接続はタスクを起動しない。
///
/// `Err` になるのはレジストリの不変条件が破れている場合だけ（`banto_tags`
/// の検証が通っていれば起こらない - 演算タグの `expression` 欠落と同じ
/// 「静かに無視しない」防御的分岐）:
///
/// - `postgres` 接続配下の有効なグループに `query_sql` が無い
/// - `postgres` 接続配下に `db` 以外の種別のタグがいる
/// - `port` が `1..=65535` に収まらない（レジストリで外した接続は除く）
///
/// PR #777 のオーナーレビュー P2（設計 §4.4・§3.4）: 有効な `postgres` 接続は、PLC の収集
/// （`banto_collect::build_config_from_with_registry`）と同じ
/// `banto_collect::driver_exclusion` で `registry` に照らす。ドライバが無い・`settings` が
/// 検証を通らない・設定の版が新しすぎる接続は [`DbSourcePlan::excluded`] に載せ、タスクを
/// 起こさない（配下のタグは Bad）。計画全体は落とさない（他の接続は動かす、#414 の
/// 「外して残りを動かす」）。
pub fn build_plan(
    snapshot: &RegistrySnapshot,
    registry: &DriverRegistry,
) -> Result<DbSourcePlan, String> {
    let mut connections = Vec::new();
    let mut disabled = Vec::new();
    let mut unreadable = Vec::new();
    let mut excluded = Vec::new();

    for conn in &snapshot.connections {
        if conn.protocol != POSTGRES_PROTOCOL {
            continue;
        }
        if !conn.enabled {
            disabled.push((conn.id, conn.name.clone()));
            continue;
        }

        let exclusion = banto_collect::driver_exclusion(conn, registry);
        // A connection the registry refuses is not parsed further (its port may be
        // exactly what the driver refused); it is listed below once its groups are known.
        let port = if exclusion.is_some() {
            0
        } else {
            u16::try_from(conn.port)
                .ok()
                .filter(|port| *port >= 1)
                .ok_or_else(|| {
                    format!(
                        "DB 接続 {} のポート番号が不正です（1〜65535）: {}",
                        conn.name, conn.port
                    )
                })?
        };

        let mut groups = Vec::new();
        for group in snapshot
            .groups
            .iter()
            .filter(|g| g.plc_connection_id == conn.id)
        {
            let query_sql = group
                .query_sql
                .as_deref()
                .map(str::trim)
                .filter(|sql| !sql.is_empty());
            // 無効なグループでも「SQL が無い」こと自体はレジストリの不変
            // 条件違反なので、有効/無効を問わず先に検査する - 無効にして
            // おけば壊れた行を隠せる、という抜け道を作らない。
            let Some(query_sql) = query_sql else {
                return Err(format!(
                    "DB 接続 {} のグループ {} に SQL (querySql) がありません",
                    conn.name, group.name
                ));
            };

            let mut tags = Vec::new();
            for tag in snapshot
                .tags
                .iter()
                .filter(|t| t.collection_group_id == group.id)
            {
                if tag.tag_kind != DB_TAG_KIND {
                    return Err(format!(
                        "DB 接続 {} のグループ {} に db 以外の種別のタグがあります: {} ({})",
                        conn.name, group.name, tag.name, tag.tag_kind
                    ));
                }
                if !tag.enabled {
                    continue;
                }
                tags.push(DbTagPlan {
                    tag_key: format!("tag:{}", tag.id),
                    external_name: format!("{}.{}.{}", conn.name, group.name, tag.name),
                    column: tag.address.clone(),
                });
            }

            if !group.enabled || tags.is_empty() {
                continue;
            }

            let period_ms = u64::try_from(group.period_ms).map_err(|_| {
                format!(
                    "DB 接続 {} のグループ {} の収集周期が不正です: {}",
                    conn.name, group.name, group.period_ms
                )
            })?;

            groups.push(DbGroupPlan {
                group_id: group.id,
                group_name: group.name.clone(),
                period_ms,
                query_sql: query_sql.to_string(),
                tags,
            });
        }

        if groups.is_empty() {
            continue;
        }

        if let Some(reason) = exclusion {
            excluded.push(ExcludedDbConnection {
                connection_id: conn.id,
                connection_name: conn.name.clone(),
                reason,
                groups,
            });
            continue;
        }

        // Design §5.4・§5.5 (R2b-1e): the plan holds the sealed row, not the plaintext - it
        // is opened right before connecting (`task::run_connection`). Here it is opened
        // once to check it is readable (the plaintext is dropped at once). No password set =
        // connect without one (a postgres password is optional). One that is set but cannot
        // be read is **not** collapsed to an empty password: the connection is not started
        // (no task, so no retry dials it either), its status reads「パスワード: 読めません」
        // and its tags read Bad.
        let password = match crate::secrets::sealed_connection_secret(
            &crate::secrets::ConnectionSecrets::platform_default(),
            conn,
            banto_tags::PASSWORD_KEY,
            &snapshot.secrets,
        ) {
            Ok(password) => password,
            Err(reason) => {
                unreadable.push(UnreadableDbConnection {
                    connection_id: conn.id,
                    connection_name: conn.name.clone(),
                    reason,
                    groups,
                });
                continue;
            }
        };

        connections.push(DbConnectionPlan {
            connection_id: conn.id,
            connection_name: conn.name.clone(),
            host: conn.host.clone(),
            port,
            database: conn.database.clone().unwrap_or_default(),
            username: conn.username.clone().unwrap_or_default(),
            password,
            groups,
        });
    }

    Ok(DbSourcePlan {
        connections,
        disabled,
        unreadable,
        excluded,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use banto_tags::{CollectionGroup, PlcConnection, Tag};

    fn pg_connection(id: i64, name: &str, enabled: bool) -> PlcConnection {
        // R2b-1d: the endpoint lives in `settings` (what the registry check validates),
        // the password is a reference to a sealed row ([`secret_rows`]).
        PlcConnection::from_settings(
            id,
            name.to_string(),
            POSTGRES_PROTOCOL.to_string(),
            enabled,
            false,
            serde_json::json!({
                "host": "10.0.0.50", "port": 5432, "database": "erp", "username": "reader",
                "password": { "$secret": PW_REF }
            })
            .as_object()
            .unwrap()
            .clone(),
            1,
        )
    }

    /// The Hub's registry in tests (`postgres` is the real host-provided entry).
    fn registry() -> std::sync::Arc<DriverRegistry> {
        crate::test_support::test_drivers().registry
    }

    const PW_REF: &str = "0123456789abcdef0123456789abcdef";

    /// The sealed (`plain-v1`) password row of connection 1 (what
    /// `RegistrySnapshot::load` reads from `plc_connection_secrets`).
    fn secret_rows() -> Vec<banto_tags::StoredSecretRow> {
        vec![banto_tags::StoredSecretRow {
            connection_id: 1,
            key: "password".to_string(),
            sealed: banto_tags::SealedSecretRow::new(PW_REF, "plain-v1", b"s3cret".to_vec()),
        }]
    }

    fn plc_connection(id: i64, name: &str) -> PlcConnection {
        PlcConnection {
            protocol: "slmp".to_string(),
            database: None,
            username: None,
            ..pg_connection(id, name, true)
        }
    }

    fn group(
        id: i64,
        name: &str,
        conn_id: i64,
        sql: Option<&str>,
        enabled: bool,
    ) -> CollectionGroup {
        CollectionGroup {
            id,
            name: name.to_string(),
            plc_connection_id: conn_id,
            period_ms: 1_000,
            enabled,
            default_writable: true,
            query_sql: sql.map(str::to_string),
        }
    }

    fn db_tag(id: i64, name: &str, group_id: i64, column: &str, enabled: bool) -> Tag {
        Tag {
            id,
            name: name.to_string(),
            collection_group_id: group_id,
            address: column.to_string(),
            data_type: "f32".to_string(),
            string_length: None,
            string_encoding: "utf8".to_string(),
            raw_lo: None,
            raw_hi: None,
            eng_lo: None,
            eng_hi: None,
            unit: None,
            decimals: 0,
            enabled,
            writable: false,
            tag_kind: DB_TAG_KIND.to_string(),
            expression: None,
            retain: false,
            revision: 1,
        }
    }

    #[test]
    fn build_plan_is_empty_without_any_postgres_connection() {
        let snapshot = RegistrySnapshot {
            connections: vec![plc_connection(1, "line1")],
            groups: vec![group(1, "fast", 1, None, true)],
            tags: vec![],
            secrets: secret_rows(),
        };
        assert!(build_plan(&snapshot, &registry()).unwrap().is_empty());
    }

    #[test]
    fn build_plan_collects_enabled_groups_and_tags() {
        let snapshot = RegistrySnapshot {
            connections: vec![pg_connection(1, "erp", true)],
            groups: vec![
                group(1, "q1", 1, Some("SELECT a, b FROM v1"), true),
                group(2, "q2", 1, Some("SELECT c FROM v2"), false),
            ],
            tags: vec![
                db_tag(10, "A", 1, "a", true),
                db_tag(11, "B", 1, "b", false),
                db_tag(12, "C", 2, "c", true),
            ],
            secrets: secret_rows(),
        };
        let plan = build_plan(&snapshot, &registry()).unwrap();
        assert_eq!(plan.connections.len(), 1);
        let conn = &plan.connections[0];
        assert_eq!(conn.port, 5432);
        assert_eq!(conn.database, "erp");
        // 無効なグループ q2 は落ちる。
        assert_eq!(conn.groups.len(), 1);
        assert_eq!(conn.groups[0].query_sql, "SELECT a, b FROM v1");
        // 無効なタグ B も落ちる。
        assert_eq!(conn.groups[0].tags.len(), 1);
        assert_eq!(conn.groups[0].tags[0].tag_key, "tag:10");
        assert_eq!(conn.groups[0].tags[0].external_name, "erp.q1.A");
        assert_eq!(conn.groups[0].tags[0].column, "a");
    }

    #[test]
    fn build_plan_lists_a_disabled_connection_without_starting_it() {
        let snapshot = RegistrySnapshot {
            connections: vec![pg_connection(1, "erp", false)],
            groups: vec![group(1, "q1", 1, Some("SELECT a FROM v1"), true)],
            tags: vec![db_tag(10, "A", 1, "a", true)],
            secrets: secret_rows(),
        };
        let plan = build_plan(&snapshot, &registry()).unwrap();
        assert!(plan.connections.is_empty());
        assert_eq!(plan.disabled, vec![(1, "erp".to_string())]);
    }

    #[test]
    fn build_plan_skips_a_group_with_no_enabled_tags() {
        let snapshot = RegistrySnapshot {
            connections: vec![pg_connection(1, "erp", true)],
            groups: vec![group(1, "q1", 1, Some("SELECT a FROM v1"), true)],
            tags: vec![db_tag(10, "A", 1, "a", false)],
            secrets: secret_rows(),
        };
        let plan = build_plan(&snapshot, &registry()).unwrap();
        assert!(plan.connections.is_empty(), "{plan:?}");
    }

    #[test]
    fn build_plan_rejects_a_group_without_sql() {
        let snapshot = RegistrySnapshot {
            connections: vec![pg_connection(1, "erp", true)],
            groups: vec![group(1, "q1", 1, None, true)],
            tags: vec![db_tag(10, "A", 1, "a", true)],
            secrets: secret_rows(),
        };
        let err = build_plan(&snapshot, &registry()).unwrap_err();
        assert!(err.contains("querySql"), "{err}");
    }

    #[test]
    fn build_plan_rejects_a_non_db_tag_under_a_postgres_group() {
        let mut tag = db_tag(10, "A", 1, "a", true);
        tag.tag_kind = banto_tags::PLC_TAG_KIND.to_string();
        let snapshot = RegistrySnapshot {
            connections: vec![pg_connection(1, "erp", true)],
            groups: vec![group(1, "q1", 1, Some("SELECT a FROM v1"), true)],
            tags: vec![tag],
            secrets: secret_rows(),
        };
        let err = build_plan(&snapshot, &registry()).unwrap_err();
        assert!(err.contains("db 以外"), "{err}");
    }

    #[test]
    fn build_plan_rejects_an_out_of_range_port() {
        let mut conn = pg_connection(1, "erp", true);
        conn.port = 70_000;
        let snapshot = RegistrySnapshot {
            connections: vec![conn],
            groups: vec![group(1, "q1", 1, Some("SELECT a FROM v1"), true)],
            tags: vec![db_tag(10, "A", 1, "a", true)],
            secrets: secret_rows(),
        };
        let err = build_plan(&snapshot, &registry()).unwrap_err();
        assert!(err.contains("ポート"), "{err}");
    }

    /// Design §5.4 (R2b-1d, R2b-1e): the plan holds the sealed password row (never the
    /// plaintext) after checking it opens; a connection with no password connects without
    /// one; one whose reference has no readable row is not started (listed as unreadable
    /// with its reason) - never dialled with an empty password.
    #[test]
    fn build_plan_holds_the_sealed_password_and_never_collapses_an_unreadable_one() {
        let snapshot = RegistrySnapshot {
            connections: vec![pg_connection(1, "erp", true)],
            groups: vec![group(1, "q1", 1, Some("SELECT a FROM v1"), true)],
            tags: vec![db_tag(10, "A", 1, "a", true)],
            secrets: secret_rows(),
        };
        let plan = build_plan(&snapshot, &registry()).unwrap();
        let sealed = plan.connections[0]
            .password
            .as_ref()
            .expect("the sealed row is held");
        assert_eq!(sealed.r#ref.as_str(), PW_REF);
        assert_eq!(
            crate::secrets::ConnectionSecrets::platform_default()
                .open("password", sealed)
                .unwrap()
                .as_str(),
            "s3cret",
            "opened only at connect time, from the sealed row"
        );

        let mut no_password = snapshot.clone();
        no_password.connections[0].settings.remove("password");
        no_password.secrets.clear();
        let plan = build_plan(&no_password, &registry()).unwrap();
        assert_eq!(plan.connections[0].password, None);

        for broken in [
            // The row is missing.
            Vec::new(),
            // The row belongs to another connection.
            vec![banto_tags::StoredSecretRow {
                connection_id: 2,
                ..secret_rows().remove(0)
            }],
            // The row's scheme cannot be opened here.
            vec![banto_tags::StoredSecretRow {
                sealed: banto_tags::SealedSecretRow::new(
                    PW_REF,
                    "dpapi-machine-v1",
                    b"blob".to_vec(),
                ),
                ..secret_rows().remove(0)
            }],
        ] {
            let mut unreadable = snapshot.clone();
            unreadable.secrets = broken;
            let plan = build_plan(&unreadable, &registry()).unwrap();
            assert!(plan.connections.is_empty(), "{plan:?}");
            assert!(plan.disabled.is_empty(), "{plan:?}");
            assert_eq!(plan.unreadable.len(), 1, "{plan:?}");
            assert_eq!(plan.unreadable[0].connection_id, 1);
            assert_eq!(plan.unreadable[0].groups[0].tags[0].tag_key, "tag:10");
            assert!(!format!("{plan:?}").contains("s3cret"));
        }
        // The reason is kept apart: a missing row vs. a row that cannot be opened.
        let mut missing = snapshot.clone();
        missing.secrets.clear();
        assert_eq!(
            build_plan(&missing, &registry()).unwrap().unreadable[0].reason,
            UnreadableReason::Missing
        );
        let mut undecryptable = snapshot.clone();
        undecryptable.secrets[0].sealed =
            banto_tags::SealedSecretRow::new(PW_REF, "dpapi-machine-v1", b"blob".to_vec());
        assert_eq!(
            build_plan(&undecryptable, &registry()).unwrap().unreadable[0].reason,
            UnreadableReason::Undecryptable
        );
        // A disabled connection with an unreadable password stays `disabled` (it would
        // not be dialled anyway).
        let mut disabled = missing.clone();
        disabled.connections[0].enabled = false;
        let plan = build_plan(&disabled, &registry()).unwrap();
        assert_eq!(plan.disabled, vec![(1, "erp".to_string())]);
        assert!(plan.unreadable.is_empty());
    }

    /// PR #777 のオーナーレビュー P2（設計 §4.4・§3.4）: DB Source も PLC の収集と同じ
    /// `driver_exclusion` で接続を確かめる。設定の版がドライバより新しい・`postgres` の
    /// ドライバが無い・`settings` が検証を通らない（範囲外のポート・形の違う秘密の参照）
    /// 接続は計画に載らず（タスクなし）、理由つきで `excluded` に出る（配下のタグを Bad に
    /// するためのグループを持つ）。計画全体は落ちず、他の接続は動く。
    #[test]
    fn build_plan_excludes_connections_the_registry_refuses() {
        // Connection 2 has no password (its plan needs no secret row).
        let mut mes = pg_connection(2, "mes", true);
        mes.settings.remove("password");
        let base = RegistrySnapshot {
            connections: vec![pg_connection(1, "erp", true), mes],
            groups: vec![
                group(1, "q1", 1, Some("SELECT a FROM v1"), true),
                group(2, "q2", 2, Some("SELECT b FROM v2"), true),
            ],
            tags: vec![db_tag(10, "A", 1, "a", true), db_tag(20, "B", 2, "b", true)],
            secrets: secret_rows(),
        };
        let only_first_excluded = |plan: &DbSourcePlan| {
            assert_eq!(plan.excluded.len(), 1, "{plan:?}");
            assert_eq!(plan.excluded[0].connection_id, 1);
            assert_eq!(plan.excluded[0].groups[0].tags[0].tag_key, "tag:10");
            let ids: Vec<i64> = plan.connections.iter().map(|c| c.connection_id).collect();
            assert_eq!(ids, vec![2], "the other connection still runs");
            assert!(plan.unreadable.is_empty(), "{plan:?}");
            let blocked: Vec<(i64, String)> = plan
                .blocked()
                .map(|(conn, message)| (conn.connection_id, message))
                .collect();
            assert_eq!(blocked.len(), 1);
            assert_eq!(blocked[0].0, 1);
            blocked[0].1.clone()
        };

        // settings_version newer than the postgres driver's (1).
        let mut too_new = base.clone();
        too_new.connections[0].settings_version = 2;
        let plan = build_plan(&too_new, &registry()).unwrap();
        assert_eq!(
            plan.excluded[0].reason,
            ExclusionReason::SettingsVersionTooNew {
                stored: 2,
                supported: 1
            }
        );
        let message = only_first_excluded(&plan);
        assert!(message.contains("版"), "{message}");

        // No postgres driver in the registry: every postgres connection is out.
        let empty = DriverRegistry::new();
        let plan = build_plan(&base, &empty).unwrap();
        assert!(plan.connections.is_empty(), "{plan:?}");
        assert_eq!(plan.excluded.len(), 2, "{plan:?}");
        for conn in &plan.excluded {
            assert_eq!(
                conn.reason,
                ExclusionReason::DriverUnavailable {
                    protocol: "postgres".to_string()
                }
            );
        }

        // Stored settings the driver refuses (a port out of range written by hand): left
        // out, not a failure of the whole plan.
        let mut bad_port = base.clone();
        bad_port.connections[0]
            .settings
            .insert("port".to_string(), serde_json::json!(70_000));
        let plan = build_plan(&bad_port, &registry()).unwrap();
        assert!(
            matches!(
                plan.excluded[0].reason,
                ExclusionReason::InvalidSettings { .. }
            ),
            "{plan:?}"
        );
        only_first_excluded(&plan);

        // A malformed secret reference (key present, not `{"$secret": "<32 hex>"}`):
        // left out - never planned with an empty password.
        let mut malformed = base.clone();
        malformed.connections[0].settings.insert(
            "password".to_string(),
            serde_json::json!({ "$secret": 123 }),
        );
        let plan = build_plan(&malformed, &registry()).unwrap();
        match &plan.excluded[0].reason {
            ExclusionReason::InvalidSettings { errors } => {
                assert!(
                    errors.iter().any(|e| e.starts_with("password")),
                    "{errors:?}"
                )
            }
            other => panic!("expected InvalidSettings, got {other:?}"),
        }
        only_first_excluded(&plan);

        // A disabled connection stays `disabled` (not dialled anyway).
        let mut disabled = too_new.clone();
        disabled.connections[0].enabled = false;
        let plan = build_plan(&disabled, &registry()).unwrap();
        assert_eq!(plan.disabled, vec![(1, "erp".to_string())]);
        assert!(plan.excluded.is_empty());
    }

    /// PR #777 のオーナーレビュー P2: 生の SQL で `settings.password` を形の違う参照
    /// （`{"$secret": 123}`）にした接続は、DB から読んだ計画でもタスクを持たず（空の
    /// パスワードで繋がない）、配下のタグを Bad にする側（`excluded`）に出る。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_malformed_reference_written_by_raw_sql_never_plans_a_task() {
        let pool = crate::db::init_db_memory().await.unwrap();
        let service = banto_tags::PlcConnectionService::with_registry(pool.clone(), registry());
        let secrets = crate::secrets::ConnectionSecrets::platform_default();
        let conn = service
            .create(banto_tags::PlcConnectionInput {
                name: "erp".to_string(),
                protocol: POSTGRES_PROTOCOL.to_string(),
                host: "10.0.0.50".to_string(),
                port: 5432,
                unit_id: 1,
                enabled: true,
                simulation: false,
                word_order: String::new(),
                cpu_type: None,
                database: Some("erp".to_string()),
                username: Some("reader".to_string()),
                secrets: crate::secrets::password_changes(&secrets, Some("s3cret")).unwrap(),
                settings: None,
            })
            .await
            .unwrap();
        let group = banto_tags::CollectionGroupService::new(pool.clone())
            .create(banto_tags::CollectionGroupInput {
                name: "q1".to_string(),
                plc_connection_id: conn.id,
                period_ms: 1_000,
                enabled: true,
                default_writable: false,
                query_sql: Some("SELECT a FROM v1".to_string()),
            })
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO tags (name, collection_group_id, address, data_type, tag_kind) \
             VALUES ('A', ?, 'a', 'f32', 'db')",
        )
        .bind(group.id)
        .execute(&pool)
        .await
        .unwrap();
        let snapshot = RegistrySnapshot::load(&pool).await.unwrap();
        assert_eq!(
            build_plan(&snapshot, &registry())
                .unwrap()
                .connections
                .len(),
            1
        );

        sqlx::query(
            "UPDATE plc_connections SET settings = json_set(settings, '$.password', \
             json('{\"$secret\":123}')) WHERE id = ?",
        )
        .bind(conn.id)
        .execute(&pool)
        .await
        .unwrap();
        let snapshot = RegistrySnapshot::load(&pool).await.unwrap();
        let plan = build_plan(&snapshot, &registry()).unwrap();
        assert!(plan.connections.is_empty(), "{plan:?}");
        let blocked: Vec<i64> = plan.blocked().map(|(c, _)| c.connection_id).collect();
        assert_eq!(blocked, vec![conn.id], "{plan:?}");
        assert_eq!(plan.excluded[0].groups[0].tags.len(), 1);
        assert!(!format!("{plan:?}").contains("s3cret"));
    }

    /// このモジュールの doc comment「平文パスワードの扱い」節。
    #[test]
    fn connection_plan_debug_redacts_the_password() {
        let snapshot = RegistrySnapshot {
            connections: vec![pg_connection(1, "erp", true)],
            groups: vec![group(1, "q1", 1, Some("SELECT a FROM v1"), true)],
            tags: vec![db_tag(10, "A", 1, "a", true)],
            secrets: secret_rows(),
        };
        let plan = build_plan(&snapshot, &registry()).unwrap();
        let rendered = format!("{plan:?}");
        assert!(
            !rendered.contains("s3cret"),
            "the plan's Debug must never carry the plaintext password: {rendered}"
        );
        assert!(!rendered.contains(PW_REF), "{rendered}");
        assert!(rendered.contains("<sealed>"), "{rendered}");
    }
}
