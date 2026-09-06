//! DB Source の運転状態（docs/banto-hub-external-db-design.md §4.2 の再接続・
//! §4.3 の Quality 表を、運用者が状態画面で追えるようにしたもの）。
//!
//! **プロセス内メモリのみ**（DB にも tstore にも書かない）: 状態は「今この
//! プロセスが何をしているか」であって構成でも履歴でもない -
//! `crate::write_control::WriteControl` / `crate::test_output::TestOutputControl`
//! と同じ非永続の扱い。`GET /api/v1/status`・`GET /api/status` が
//! [`DbSourceStatusStore::snapshot`] を読んで `db_source` / `dbSource` 節
//! として出す（`crate::rest`）。
//!
//! # 秘密を含まない
//!
//! [`DbConnectionStatus::last_error`] に入るのは
//! [`crate::db_source::sanitize_postgres_error`] を通した文字列だけで、
//! 平文パスワードも接続文字列も**絶対に**含まれない（S1a の接続テストが
//! 守っているのと同じ規律 - `crate::db_source` のモジュール doc comment
//! 「資格情報の扱い」節）。そのため状態節は admin 限定にしていない
//! （`GET /api/v1/status` は API キーでも読める）。

use std::collections::{BTreeMap, HashSet};
use std::sync::RwLock;

use super::plan::DbSourcePlan;

/// 1接続の運転状態（§4.2 の再接続モデルの外から見える面）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DbConnectionState {
    /// プールが張れていて、直近のポーリングが接続レベルでは成功している。
    Connected,
    /// 接続・トランスポート障害でバックオフ待機中（配下の全タグは Bad）。
    Backoff,
    /// レジストリ上で無効。タスクは起動していない（値は一切書かない -
    /// `crate::hub::effective_sample` の `!enabled` 規則が Bad を返す）。
    Disabled,
    /// サーバーには届いたが**拒否された**（認証失敗・DB 名違いなど、
    /// `sqlx::Error::Database`）。再試行はするが、構成を直さない限り直らない
    /// ので [`DbConnectionState::Backoff`] と区別して見せる。
    Error,
}

impl DbConnectionState {
    pub fn as_str(self) -> &'static str {
        match self {
            DbConnectionState::Connected => "connected",
            DbConnectionState::Backoff => "backoff",
            DbConnectionState::Disabled => "disabled",
            DbConnectionState::Error => "error",
        }
    }
}

/// 1接続分の状態スナップショット。
#[derive(Debug, Clone, PartialEq)]
pub struct DbConnectionStatus {
    pub connection_id: i64,
    pub connection_name: String,
    pub state: DbConnectionState,
    /// 直近にこの接続のいずれかのグループを実行し終えた時刻（epoch ms）。
    pub last_poll_at: Option<i64>,
    /// 直近の接続レベルの失敗（sanitize 済み）。
    pub last_error: Option<String>,
    /// 連続した接続失敗回数（バックオフ段数 - 成功で 0 に戻る）。
    pub consecutive_failures: u32,
    pub groups: Vec<DbGroupStatus>,
}

/// 1グループ分の状態スナップショット。
#[derive(Debug, Clone, PartialEq)]
pub struct DbGroupStatus {
    pub group_id: i64,
    pub group_name: String,
    /// 直近に「行が取れた」時刻（epoch ms）。0 行やエラーでは進まない。
    pub last_ok_at: Option<i64>,
    /// 直近のグループレベルの失敗（クエリエラー・タイムアウト・0 行）。
    pub last_error: Option<String>,
    /// 直近の実行で取れた行数。生成 SQL が `LIMIT 2` を付けるので値域は
    /// `0`/`1`/`2` で、`2` は「2 行以上」を意味する
    /// （[`crate::db_source::convert::build_wrapper_sql`] の doc comment）。
    pub row_count_last: Option<i64>,
}

/// [`DbConnectionStatus`] の共有ストア。`std::sync::RwLock` -
/// `crate::computed::ServerTagStore` と同じ理由（読み取りは同期・書き込みは
/// 短時間の非 await 区間）。
#[derive(Debug, Default)]
pub struct DbSourceStatusStore {
    connections: RwLock<BTreeMap<i64, DbConnectionStatus>>,
}

impl DbSourceStatusStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// 接続 id 昇順の全接続状態（`BTreeMap` なので順序は安定）。
    pub fn snapshot(&self) -> Vec<DbConnectionStatus> {
        self.read().values().cloned().collect()
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, BTreeMap<i64, DbConnectionStatus>> {
        self.connections
            .read()
            .expect("DbSourceStatusStore lock poisoned (a writer panicked)")
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, BTreeMap<i64, DbConnectionStatus>> {
        self.connections
            .write()
            .expect("DbSourceStatusStore lock poisoned (a writer panicked)")
    }

    /// commit のたびに作り直す: 計画に無い接続の行は消え、新しく起動する
    /// 接続は [`DbConnectionState::Backoff`]（まだ繋いでいない = 値は Bad）、
    /// 無効な接続は [`DbConnectionState::Disabled`] から始まる。タスク側が
    /// 直後に接続を試み、成功すれば `connected` へ進む。
    ///
    /// `preserve` に入っている接続 id は**そのまま残す** -
    /// [`crate::db_source::DbSourceEngine::commit`] が「設定の変わっていない
    /// 接続のタスクは再起動しない」と判断した接続のこと。走り続けている
    /// タスクの `connected` を commit のたびに `backoff` へ巻き戻すと、
    /// 状態画面が実態と食い違う（そして次のポーリングまで直らない）。
    pub fn apply_plan(&self, plan: &DbSourcePlan, preserve: &HashSet<i64>) {
        let mut map = self.write();
        map.retain(|id, _| preserve.contains(id));
        for conn in &plan.connections {
            if preserve.contains(&conn.connection_id) {
                continue;
            }
            map.insert(
                conn.connection_id,
                DbConnectionStatus {
                    connection_id: conn.connection_id,
                    connection_name: conn.connection_name.clone(),
                    state: DbConnectionState::Backoff,
                    last_poll_at: None,
                    last_error: None,
                    consecutive_failures: 0,
                    groups: conn
                        .groups
                        .iter()
                        .map(|group| DbGroupStatus {
                            group_id: group.group_id,
                            group_name: group.group_name.clone(),
                            last_ok_at: None,
                            last_error: None,
                            row_count_last: None,
                        })
                        .collect(),
                },
            );
        }
        for (id, name) in &plan.disabled {
            if preserve.contains(id) {
                continue;
            }
            map.insert(
                *id,
                DbConnectionStatus {
                    connection_id: *id,
                    connection_name: name.clone(),
                    state: DbConnectionState::Disabled,
                    last_poll_at: None,
                    last_error: None,
                    consecutive_failures: 0,
                    groups: Vec::new(),
                },
            );
        }
    }

    /// 接続1件を書き換える（存在しない id は無視 - commit と走行中タスクの
    /// 極小レース時の防御的分岐。`crate::computed::ComputedEngine::evaluate_tick`
    /// が消えたタグを静かに飛ばすのと同じ立場）。
    pub fn update_connection(&self, connection_id: i64, f: impl FnOnce(&mut DbConnectionStatus)) {
        let mut map = self.write();
        if let Some(status) = map.get_mut(&connection_id) {
            f(status);
        }
    }

    /// グループ1件を書き換える（同上）。
    pub fn update_group(
        &self,
        connection_id: i64,
        group_id: i64,
        f: impl FnOnce(&mut DbGroupStatus),
    ) {
        let mut map = self.write();
        if let Some(status) = map.get_mut(&connection_id) {
            if let Some(group) = status.groups.iter_mut().find(|g| g.group_id == group_id) {
                f(group);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db_source::plan::{DbConnectionPlan, DbGroupPlan, DbTagPlan};

    fn plan() -> DbSourcePlan {
        DbSourcePlan {
            connections: vec![DbConnectionPlan {
                connection_id: 1,
                connection_name: "erp".to_string(),
                host: "127.0.0.1".to_string(),
                port: 5432,
                database: "erp".to_string(),
                username: "reader".to_string(),
                password: "pw".to_string(),
                groups: vec![DbGroupPlan {
                    group_id: 5,
                    group_name: "q1".to_string(),
                    period_ms: 1_000,
                    query_sql: "SELECT a FROM v1".to_string(),
                    tags: vec![DbTagPlan {
                        tag_key: "tag:10".to_string(),
                        external_name: "erp.q1.A".to_string(),
                        column: "a".to_string(),
                    }],
                }],
            }],
            disabled: vec![(2, "old-erp".to_string())],
        }
    }

    #[test]
    fn apply_plan_seeds_backoff_and_disabled_entries() {
        let store = DbSourceStatusStore::new();
        store.apply_plan(&plan(), &HashSet::new());
        let snapshot = store.snapshot();
        assert_eq!(snapshot.len(), 2);
        assert_eq!(snapshot[0].connection_id, 1);
        assert_eq!(snapshot[0].state, DbConnectionState::Backoff);
        assert_eq!(snapshot[0].groups.len(), 1);
        assert_eq!(snapshot[1].state, DbConnectionState::Disabled);
        assert!(snapshot[1].groups.is_empty());
    }

    #[test]
    fn apply_plan_replaces_the_previous_generation() {
        let store = DbSourceStatusStore::new();
        store.apply_plan(&plan(), &HashSet::new());
        store.apply_plan(&DbSourcePlan::default(), &HashSet::new());
        assert!(store.snapshot().is_empty());
    }

    /// `preserve` に入っている接続の行は commit をまたいでそのまま残る
    /// （[`DbSourceStatusStore::apply_plan`] の doc comment）。
    #[test]
    fn apply_plan_keeps_preserved_connections_untouched() {
        let store = DbSourceStatusStore::new();
        store.apply_plan(&plan(), &HashSet::new());
        store.update_connection(1, |status| status.state = DbConnectionState::Connected);
        store.apply_plan(&plan(), &HashSet::from([1]));
        let snapshot = store.snapshot();
        assert_eq!(snapshot[0].state, DbConnectionState::Connected);
    }

    #[test]
    fn updates_reach_the_named_connection_and_group_only() {
        let store = DbSourceStatusStore::new();
        store.apply_plan(&plan(), &HashSet::new());
        store.update_connection(1, |status| {
            status.state = DbConnectionState::Connected;
            status.last_poll_at = Some(1_000);
        });
        store.update_group(1, 5, |group| {
            group.last_ok_at = Some(1_000);
            group.row_count_last = Some(1);
        });
        // Unknown ids are silently ignored (defensive - see `update_connection`).
        store.update_connection(999, |status| status.state = DbConnectionState::Error);
        store.update_group(1, 999, |group| group.row_count_last = Some(42));

        let snapshot = store.snapshot();
        assert_eq!(snapshot[0].state, DbConnectionState::Connected);
        assert_eq!(snapshot[0].last_poll_at, Some(1_000));
        assert_eq!(snapshot[0].groups[0].row_count_last, Some(1));
    }
}
