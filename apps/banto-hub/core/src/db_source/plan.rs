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
//! ## 平文パスワードの扱い
//!
//! [`DbConnectionPlan`] は §2.2 の決定どおり平文パスワードを保持する。
//! そのため [`DbConnectionPlan`] の `Debug` は**手書き**で、`password` を
//! `"<redacted>"` に置き換える - `#[derive(Debug)]` のままだと、plan を
//! そのままログや `panic!` メッセージに出した瞬間にパスワードが漏れる
//! （S1a のレビューで `PlcConnection::password` について同じ指摘が2箇所
//! 出ている）。

use std::collections::HashMap;
use std::fmt;

use banto_collect::RegistrySnapshot;
use banto_tags::{DB_TAG_KIND, POSTGRES_PROTOCOL};

use super::convert::ColumnKind;

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
}

impl DbSourcePlan {
    pub fn is_empty(&self) -> bool {
        self.connections.is_empty() && self.disabled.is_empty()
    }
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
    /// 平文（§2.2）。このモジュールの doc comment「平文パスワードの扱い」
    /// 節のとおり `Debug` には出さない。
    pub password: String,
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
            .field("password", &"<redacted>")
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TagBadReason {
    /// SELECT の結果にその名前の列が無い。
    MissingColumn,
    /// 列はあるが v1 が扱えない型（`text`/`bytea`/配列/`json`…、§4.4）。
    UnsupportedColumnType(String),
    /// グループの SQL 自体を describe（prepare）できなかった - 構文エラー・
    /// 存在しない表・権限不足など。列単位ではなくグループ全体の問題だが、
    /// 値を書く単位はタグなので同じ列挙で扱う（`crate::db_source::task` の
    /// `poll_forever` 参照）。
    StatementNotPrepared(String),
}

impl TagBadReason {
    pub fn message(&self) -> String {
        match self {
            TagBadReason::MissingColumn => "SELECT の結果にこの列がありません".to_string(),
            TagBadReason::UnsupportedColumnType(type_name) => {
                format!("列の型 {type_name} は v1 では扱えません（数値・bool・timestamp のみ）")
            }
            TagBadReason::StatementNotPrepared(detail) => {
                format!("グループの SQL を実行できません: {detail}")
            }
        }
    }
}

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
pub fn resolve_group(group: &DbGroupPlan, described: &[(String, String)]) -> ResolvedGroup {
    // §4.1「大文字小文字は DB 側の規則に委ねずそのまま比較」: 完全一致の
    // マップを作る。同名列が2つある SELECT（`SELECT a, a FROM t`）では
    // 先に現れた方が勝つ - どちらを採るかは本質的に恣意的で、先頭優先は
    // 「先頭行を採る」§4.2 と同じ一貫した選択。
    let mut by_name: HashMap<&str, &str> = HashMap::with_capacity(described.len());
    for (name, type_name) in described {
        by_name.entry(name.as_str()).or_insert(type_name.as_str());
    }

    let mut columns = Vec::new();
    let mut value_tags = Vec::new();
    let mut bad_tags = Vec::new();
    for tag in &group.tags {
        match by_name.get(tag.column.as_str()) {
            None => bad_tags.push((tag.clone(), TagBadReason::MissingColumn)),
            Some(type_name) => match ColumnKind::classify(type_name) {
                None => bad_tags.push((
                    tag.clone(),
                    TagBadReason::UnsupportedColumnType((*type_name).to_string()),
                )),
                Some(kind) => {
                    columns.push(super::convert::MappedColumn {
                        column: tag.column.clone(),
                        kind,
                    });
                    value_tags.push(tag.clone());
                }
            },
        }
    }

    let wrapper_sql = (!columns.is_empty())
        .then(|| super::convert::build_wrapper_sql(&group.query_sql, &columns));

    ResolvedGroup {
        wrapper_sql,
        value_tags,
        bad_tags,
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
/// - `port` が `1..=65535` に収まらない
pub fn build_plan(snapshot: &RegistrySnapshot) -> Result<DbSourcePlan, String> {
    let mut connections = Vec::new();
    let mut disabled = Vec::new();

    for conn in &snapshot.connections {
        if conn.protocol != POSTGRES_PROTOCOL {
            continue;
        }
        if !conn.enabled {
            disabled.push((conn.id, conn.name.clone()));
            continue;
        }

        let port = u16::try_from(conn.port)
            .ok()
            .filter(|port| *port >= 1)
            .ok_or_else(|| {
                format!(
                    "DB 接続 {} のポート番号が不正です（1〜65535）: {}",
                    conn.name, conn.port
                )
            })?;

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

        connections.push(DbConnectionPlan {
            connection_id: conn.id,
            connection_name: conn.name.clone(),
            host: conn.host.clone(),
            port,
            database: conn.database.clone().unwrap_or_default(),
            username: conn.username.clone().unwrap_or_default(),
            password: conn.password.clone().unwrap_or_default(),
            groups,
        });
    }

    Ok(DbSourcePlan {
        connections,
        disabled,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use banto_tags::{CollectionGroup, PlcConnection, Tag};

    fn pg_connection(id: i64, name: &str, enabled: bool) -> PlcConnection {
        PlcConnection {
            id,
            name: name.to_string(),
            protocol: POSTGRES_PROTOCOL.to_string(),
            host: "10.0.0.50".to_string(),
            port: 5432,
            unit_id: 1,
            enabled,
            simulation: false,
            word_order: "low_high".to_string(),
            database: Some("erp".to_string()),
            username: Some("reader".to_string()),
            password: Some("s3cret".to_string()),
        }
    }

    fn plc_connection(id: i64, name: &str) -> PlcConnection {
        PlcConnection {
            protocol: "slmp".to_string(),
            database: None,
            username: None,
            password: None,
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
            threshold_h: None,
            threshold_hh: None,
            threshold_l: None,
            threshold_ll: None,
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
        };
        assert!(build_plan(&snapshot).unwrap().is_empty());
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
        };
        let plan = build_plan(&snapshot).unwrap();
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
        };
        let plan = build_plan(&snapshot).unwrap();
        assert!(plan.connections.is_empty());
        assert_eq!(plan.disabled, vec![(1, "erp".to_string())]);
    }

    #[test]
    fn build_plan_skips_a_group_with_no_enabled_tags() {
        let snapshot = RegistrySnapshot {
            connections: vec![pg_connection(1, "erp", true)],
            groups: vec![group(1, "q1", 1, Some("SELECT a FROM v1"), true)],
            tags: vec![db_tag(10, "A", 1, "a", false)],
        };
        let plan = build_plan(&snapshot).unwrap();
        assert!(plan.connections.is_empty(), "{plan:?}");
    }

    #[test]
    fn build_plan_rejects_a_group_without_sql() {
        let snapshot = RegistrySnapshot {
            connections: vec![pg_connection(1, "erp", true)],
            groups: vec![group(1, "q1", 1, None, true)],
            tags: vec![db_tag(10, "A", 1, "a", true)],
        };
        let err = build_plan(&snapshot).unwrap_err();
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
        };
        let err = build_plan(&snapshot).unwrap_err();
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
        };
        let err = build_plan(&snapshot).unwrap_err();
        assert!(err.contains("ポート"), "{err}");
    }

    /// このモジュールの doc comment「平文パスワードの扱い」節。
    #[test]
    fn connection_plan_debug_redacts_the_password() {
        let snapshot = RegistrySnapshot {
            connections: vec![pg_connection(1, "erp", true)],
            groups: vec![group(1, "q1", 1, Some("SELECT a FROM v1"), true)],
            tags: vec![db_tag(10, "A", 1, "a", true)],
        };
        let plan = build_plan(&snapshot).unwrap();
        let rendered = format!("{plan:?}");
        assert!(
            !rendered.contains("s3cret"),
            "the plan's Debug must never carry the plaintext password: {rendered}"
        );
        assert!(rendered.contains("<redacted>"), "{rendered}");
    }

    // --- resolve_group -----------------------------------------------------

    fn one_group(columns: &[&str]) -> DbGroupPlan {
        DbGroupPlan {
            group_id: 1,
            group_name: "q1".to_string(),
            period_ms: 1_000,
            query_sql: "SELECT * FROM v1".to_string(),
            tags: columns
                .iter()
                .enumerate()
                .map(|(i, column)| DbTagPlan {
                    tag_key: format!("tag:{}", i + 1),
                    external_name: format!("erp.q1.T{i}"),
                    column: (*column).to_string(),
                })
                .collect(),
        }
    }

    #[test]
    fn resolve_group_splits_value_tags_from_bad_ones() {
        let group = one_group(&["a", "flag", "ts", "note", "missing"]);
        let described = vec![
            ("a".to_string(), "INT4".to_string()),
            ("flag".to_string(), "BOOL".to_string()),
            ("ts".to_string(), "TIMESTAMPTZ".to_string()),
            ("note".to_string(), "TEXT".to_string()),
        ];
        let resolved = resolve_group(&group, &described);
        assert_eq!(
            resolved
                .value_tags
                .iter()
                .map(|t| t.column.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "flag", "ts"]
        );
        assert_eq!(resolved.bad_tags.len(), 2);
        assert_eq!(
            resolved.bad_tags[0].1,
            TagBadReason::UnsupportedColumnType("TEXT".to_string())
        );
        assert_eq!(resolved.bad_tags[1].1, TagBadReason::MissingColumn);
        let sql = resolved.wrapper_sql.expect("three value columns");
        assert!(sql.contains("(\"a\")::float8 AS c0"), "{sql}");
        assert!(sql.contains("(\"flag\")::int4::float8 AS c1"), "{sql}");
    }

    /// 列名の比較は大文字小文字を区別する（§4.1）。
    #[test]
    fn resolve_group_compares_column_names_case_sensitively() {
        let group = one_group(&["Rate"]);
        let described = vec![("rate".to_string(), "INT4".to_string())];
        let resolved = resolve_group(&group, &described);
        assert!(resolved.wrapper_sql.is_none());
        assert_eq!(resolved.bad_tags[0].1, TagBadReason::MissingColumn);
    }

    #[test]
    fn resolve_group_with_no_usable_column_emits_no_sql() {
        let group = one_group(&["note"]);
        let described = vec![("note".to_string(), "TEXT".to_string())];
        let resolved = resolve_group(&group, &described);
        assert!(resolved.wrapper_sql.is_none());
        assert!(resolved.value_tags.is_empty());
        assert_eq!(resolved.bad_tags.len(), 1);
    }
}
