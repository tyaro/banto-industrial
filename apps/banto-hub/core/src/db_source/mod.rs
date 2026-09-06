//! DB Source（docs/banto-hub-external-db-design.md #228・§4）: 外部
//! PostgreSQL の SELECT 結果を、PLC タグと区別のつかない現在値としてタグ
//! 空間へ載せる仕組み。
//!
//! ## 構成（S2）
//!
//! - [`plan`]: レジストリのスナップショット → 実行計画（純関数。
//!   `crate::computed::build_plan` と同じ立ち位置）。
//! - [`convert`]: 「列 → f64」の変換戦略（describe した型に応じたキャストを
//!   生成 SQL に埋め込む）。
//! - [`task`]: 1接続 = 1 tokio task のポーリング本体（最小デッドライン方式・
//!   Quality の状態機械・再接続バックオフ）。
//! - [`status`]: プロセス内メモリの運転状態（`GET /api/status` の
//!   `dbSource` 節）。
//! - 本ファイル: それらを束ねる [`DbSourceEngine`]（`crate::hub::CollectorManager`
//!   が `crate::computed::ComputedEngine` と全く同じ扱いで所有する）と、
//!   S1a から居る接続テスト [`test_connection`]。
//!
//! ## エンジンのライフサイクル
//!
//! `crate::hub::CollectorManager` が1つ構築して保持し、`rebuild` /
//! `commit_catalog` が catalog・`Collector`・演算 plan の入れ替えと**同じ
//! all-or-nothing の1ステップ**として [`DbSourceEngine::commit`] を呼ぶ
//! （設計 §4.5「Source の task は `commit_catalog` の完了を受けて配下の
//! グループ集合を再読込する」）。タスクの停止は
//! `crate::runtime::RunningHub::shutdown` が
//! [`DbSourceEngine::shutdown`] を呼ぶ1点だけ（そのモジュール doc comment
//! の「シャットダウン順序」節参照 - `ServerTagStore` と `CollectorManager`
//! を畳む**前**に止める）。
//!
//! ## 資格情報の扱い
//!
//! [`test_connection`]が返す[`DbConnectionTestOutcome::error`]にも、
//! [`status::DbConnectionStatus::last_error`]にも、[`plan::DbConnectionPlan`]の
//! `Debug`にも、**平文パスワードも接続文字列全体も絶対に含めない**
//! （design §4.6・§2.2）。`sqlx::Error`をそのまま`{err}`で文字列化せず、
//! [`sanitize_postgres_error`]でカテゴリ + DB 自身のエラーメッセージ
//! （パスワードを含まないことが保証されている - PostgreSQL 自体がエラー
//! メッセージにクライアントの送信したパスワードを含めることはない）だけを
//! 取り出す。

pub mod convert;
pub mod plan;
pub mod status;
pub mod task;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use banto_tags::PlcConnection;
use banto_tstore::Clock;
use serde::Serialize;
use sqlx::postgres::{PgConnectOptions, PgSslMode};
use sqlx::{Connection, Error as SqlxError};
use tokio::task::JoinHandle;

use crate::computed::ServerTagStore;
use crate::diag_log::DiagLog;

pub use plan::{build_plan, DbSourcePlan};
pub use status::{DbConnectionState, DbConnectionStatus, DbGroupStatus, DbSourceStatusStore};

/// S1a の接続テストに使うタイムアウト（design §4.6「5秒」）。接続・
/// クエリの両方を合わせてこの時間内に完了しなければタイムアウト扱いにする
/// （`apps/banto-hub/core/src/rest.rs`の`PLC_TEST_TIMEOUT`が PLC 側で
/// 同じ役割を果たすのと同型 - あちらは 3 秒固定、DB は初回接続に TLS
/// ネゴシエーションが乗る分だけ長めにしてある）。
const DB_TEST_TIMEOUT: Duration = Duration::from_secs(5);

/// PostgreSQL 接続オプションの唯一の組み立て点 - [`test_connection`]
/// （単発の疎通確認）と [`task`]（ポーリングタスクの `PgPool`）が同じ
/// 設定で繋ぐことを保証する。特に `sslmode` の選択（design §4.2
/// 「sslmode prefer」: TLS が使えれば使う、使えなければ平文にフォール
/// バック - v1 は閉域 LAN 前提（§2.2）で、PostgreSQL 側の証明書運用を
/// 必須にしない）は片方だけ変わると挙動が食い違うので、ここ1箇所に置く。
pub(crate) fn pg_connect_options(
    host: &str,
    port: u16,
    database: &str,
    username: &str,
    password: &str,
) -> PgConnectOptions {
    PgConnectOptions::new()
        .host(host)
        .port(port)
        .database(database)
        .username(username)
        .password(password)
        .ssl_mode(PgSslMode::Prefer)
}

/// DB Source のエンジン本体（このモジュールの doc comment「エンジンの
/// ライフサイクル」節）。`crate::computed::ComputedEngine` と同じく
/// `Arc` で共有され、[`Self::commit`] が唯一の書き込み口。
pub struct DbSourceEngine {
    store: Arc<ServerTagStore>,
    status: Arc<DbSourceStatusStore>,
    clock: Arc<dyn Clock>,
    /// 診断ログの宛先（`crate::diag_log` - 既定は素の `println!`/
    /// `eprintln!`）。`crate::hub::CollectorManager::with_diag_log` が
    /// 自分の `DiagLog` をここへも配線するので、Windows サービスモードでも
    /// DB Source の warn がサービスログファイルに残る。
    log: Mutex<DiagLog>,
    state: Mutex<EngineState>,
}

#[derive(Default)]
struct EngineState {
    /// 現在走っているタスク: 接続 id → （その世代の計画, `JoinHandle`）。
    /// 計画も一緒に持つのは [`DbSourceEngine::commit`] の差分判定
    /// （「設定が変わったか」）のため。
    tasks: HashMap<i64, (plan::DbConnectionPlan, JoinHandle<()>)>,
}

impl DbSourceEngine {
    pub fn new(store: Arc<ServerTagStore>, clock: Arc<dyn Clock>) -> Self {
        Self {
            store,
            status: Arc::new(DbSourceStatusStore::new()),
            clock,
            log: Mutex::new(DiagLog::default()),
            state: Mutex::new(EngineState::default()),
        }
    }

    /// `crate::hub::CollectorManager::with_diag_log` から配線する
    /// （`log` フィールドの doc comment 参照）。
    pub fn set_diag_log(&self, log: DiagLog) {
        *self.log.lock().expect("DbSourceEngine log lock poisoned") = log;
    }

    /// `GET /api/status` の `dbSource` 節が読むスナップショット。
    pub fn status_snapshot(&self) -> Vec<DbConnectionStatus> {
        self.status.snapshot()
    }

    /// 検証済みの計画を採用する（[`plan::build_plan`] が `Ok` を返した
    /// ときだけ呼ぶこと - all-or-nothing は呼び出し元
    /// `crate::hub::CollectorManager::rebuild`/`commit_catalog` が担保する。
    /// `crate::computed::ComputedEngine::commit` と全く同じ契約）。
    ///
    /// **v1 の差分方針: 接続単位で「設定が変わったら丸ごと再起動」。**
    /// 走行中タスクへグループ集合の差分を注入する（= タスクに mutable な
    /// 制御チャネルを持たせる）方式も採れるが、そのためには「describe
    /// 済みの解決結果をどのグループについて捨てるか」「スケジューラの
    /// デッドラインをどう引き継ぐか」を全部決める必要があり、v1 の
    /// 得るもの（再接続1回ぶんの空白）に対して状態が増えすぎる。
    /// [`plan::DbConnectionPlan`] が `PartialEq` なので「変わったか」の
    /// 判定は構造比較1回で済み、**変わっていない接続のタスクは触らない**
    /// （＝無関係な接続がポーリングを取りこぼさない）ことは保証される。
    /// これは `banto-collect` の `apply_config` が「変わった接続のタスク
    /// だけ差し替える」のと同じ粒度の判断で、粒度を接続より細かくして
    /// いないだけの違い。
    pub fn commit(&self, plan: DbSourcePlan) {
        let mut state = self.state.lock().expect("DbSourceEngine lock poisoned");
        let log = self
            .log
            .lock()
            .expect("DbSourceEngine log lock poisoned")
            .clone();

        let wanted: HashMap<i64, plan::DbConnectionPlan> = plan
            .connections
            .iter()
            .map(|conn| (conn.connection_id, conn.clone()))
            .collect();

        // 1) 計画から消えた／設定が変わった接続のタスクを止める。
        let mut preserved: HashSet<i64> = HashSet::new();
        state
            .tasks
            .retain(|id, (running_plan, handle)| match wanted.get(id) {
                Some(new_plan) if new_plan == running_plan => {
                    preserved.insert(*id);
                    true
                }
                _ => {
                    handle.abort();
                    false
                }
            });

        // 2) 状態表は「残す接続以外」を作り直す。
        self.status.apply_plan(&plan, &preserved);

        // 3) 新規／再起動対象を spawn する。
        for conn in plan.connections {
            if preserved.contains(&conn.connection_id) {
                continue;
            }
            let deps = task::TaskDeps {
                store: self.store.clone(),
                status: self.status.clone(),
                clock: self.clock.clone(),
                log: log.clone(),
            };
            let connection_id = conn.connection_id;
            let handle = tokio::spawn(task::run_connection(conn.clone(), deps));
            state.tasks.insert(connection_id, (conn, handle));
        }
    }

    /// 全タスクを止めて join する（`crate::runtime::RunningHub::shutdown`
    /// の1点からだけ呼ぶ）。`abort` してから `await` するので、
    /// `JoinError::Cancelled` は正常終了として捨てる。
    pub async fn shutdown(&self) {
        let handles: Vec<JoinHandle<()>> = {
            let mut state = self.state.lock().expect("DbSourceEngine lock poisoned");
            state
                .tasks
                .drain()
                .map(|(_, (_, handle))| {
                    handle.abort();
                    handle
                })
                .collect()
        };
        for handle in handles {
            let _ = handle.await;
        }
    }
}

/// [`test_connection`]の結果。REST は`200 {"ok": true, "serverVersion":
/// "..."}`または`200 {"ok": false, "error": "..."}`としてそのまま返す
/// （design §4.6）。`ok: false`は疎通確認の失敗を表すだけで、REST 層は
/// これを異常系（4xx/5xx）ではなく通常応答として扱う -
/// `PlcConnectionTestResponse`（PLC 側の接続テスト）と同じ判断。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DbConnectionTestOutcome {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl DbConnectionTestOutcome {
    fn ok(server_version: String) -> Self {
        Self {
            ok: true,
            server_version: Some(server_version),
            error: None,
        }
    }

    fn failed(message: String) -> Self {
        Self {
            ok: false,
            server_version: None,
            error: Some(message),
        }
    }
}

/// `conn`（`protocol == "postgres"`である前提 - 呼び出し側が
/// [`banto_tags::PlcConnection::is_db_source`]で確認する）へ接続し、
/// `SELECT version()`を1回実行して切断する。接続・クエリを合わせて
/// [`DB_TEST_TIMEOUT`]（5秒）でタイムアウトする。
///
/// 呼び出し側の責務（このモジュールの doc comment参照）: `protocol`が
/// `"postgres"`かどうかの判定・認可（REST の`require_editor`/MCP の
/// `require_admin_scope`）はここでは行わない - 疎通確認そのものだけを担う。
pub async fn test_connection(conn: &PlcConnection) -> DbConnectionTestOutcome {
    let port = if conn.port < 1 || conn.port > 65535 {
        return DbConnectionTestOutcome::failed(
            "ポート番号が不正です(1〜65535の範囲で指定してください)。".to_string(),
        );
    } else {
        conn.port as u16
    };

    let options = pg_connect_options(
        &conn.host,
        port,
        conn.database.as_deref().unwrap_or(""),
        conn.username.as_deref().unwrap_or(""),
        conn.password.as_deref().unwrap_or(""),
    );

    match tokio::time::timeout(DB_TEST_TIMEOUT, run_test(options)).await {
        Ok(outcome) => outcome,
        Err(_elapsed) => DbConnectionTestOutcome::failed(format!(
            "接続タイムアウトです({}秒)。ホスト/ポート、ネットワーク到達性を確認してください。",
            DB_TEST_TIMEOUT.as_secs()
        )),
    }
}

async fn run_test(options: PgConnectOptions) -> DbConnectionTestOutcome {
    let mut pg_conn = match sqlx::postgres::PgConnection::connect_with(&options).await {
        Ok(pg_conn) => pg_conn,
        Err(err) => return DbConnectionTestOutcome::failed(sanitize_postgres_error(&err)),
    };

    let version: Result<(String,), SqlxError> = sqlx::query_as("SELECT version()")
        .fetch_one(&mut pg_conn)
        .await;
    // ベストエフォートで閉じる - 失敗しても結果には影響しない
    // (`test_modbus_connection`の`client.disconnect()`と同じ扱い)。
    let _ = pg_conn.close().await;

    match version {
        Ok((server_version,)) => DbConnectionTestOutcome::ok(server_version),
        Err(err) => DbConnectionTestOutcome::failed(sanitize_postgres_error(&err)),
    }
}

/// `sqlx::Error`を「短いカテゴリ + DB 自身のメッセージ」へ変換する。この
/// モジュールの doc comment「資格情報の扱い」節参照 -
/// パスワード・接続文字列全体を含めない。
pub(crate) fn sanitize_postgres_error(err: &SqlxError) -> String {
    match err {
        SqlxError::Database(db_err) => {
            format!("データベースエラー: {}", db_err.message())
        }
        SqlxError::Io(io_err) => {
            format!("接続エラー(ポートが閉じている、または到達できません): {io_err}")
        }
        SqlxError::Tls(tls_err) => format!("TLS接続に失敗しました: {tls_err}"),
        SqlxError::Configuration(_) => "接続設定が不正です。".to_string(),
        SqlxError::PoolTimedOut => "接続プールがタイムアウトしました。".to_string(),
        SqlxError::PoolClosed => "接続プールが閉じられています。".to_string(),
        // 他のバリアント（Protocol/RowNotFound 等）はこの用途では通常
        // 発生しない防御的ケース。`Display`実装自体が接続文字列や
        // パスワードを含まないことは sqlx のソース上保証されている
        // (これらは全て「サーバーから返った」情報かクライアント側の
        // 状態を記述するものであり、渡した認証情報を含まない)。
        other => format!("接続に失敗しました: {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use banto_tstore::SystemClock;

    fn base_conn() -> PlcConnection {
        PlcConnection {
            id: 1,
            name: "Test DB".to_string(),
            protocol: "postgres".to_string(),
            host: "127.0.0.1".to_string(),
            port: 1,
            unit_id: 1,
            enabled: true,
            simulation: false,
            word_order: "low_high".to_string(),
            database: Some("appdb".to_string()),
            username: Some("appuser".to_string()),
            password: Some("s3cret-password".to_string()),
        }
    }

    /// A closed port fails fast (well within [`DB_TEST_TIMEOUT`]) with
    /// `ok: false`, and - the whole point of this test - the error text
    /// never contains the plaintext password.
    #[tokio::test]
    async fn test_connection_against_a_closed_port_fails_without_leaking_the_password() {
        let conn = base_conn();
        let outcome = test_connection(&conn).await;
        assert!(!outcome.ok);
        assert!(outcome.server_version.is_none());
        let error = outcome.error.expect("a failed test should carry an error");
        assert!(
            !error.contains("s3cret-password"),
            "error text must never contain the plaintext password: {error}"
        );
        assert!(
            !error.contains("appuser") || !error.contains("s3cret"),
            "sanity: this assertion just documents that usernames MAY appear \
             (the server's own message), passwords never do: {error}"
        );
    }

    /// An invalid port number is rejected before any connection attempt,
    /// with a friendly message (mirrors `test_modbus_connection`'s own
    /// port-range guard).
    #[tokio::test]
    async fn test_connection_rejects_an_out_of_range_port() {
        let mut conn = base_conn();

        // Test port > 65535
        conn.port = 70_000;
        let outcome = test_connection(&conn).await;
        assert!(!outcome.ok);
        assert!(outcome.error.expect("error").contains("1〜65535"));

        // Test port = 0
        conn.port = 0;
        let outcome = test_connection(&conn).await;
        assert!(!outcome.ok);
        assert!(outcome.error.expect("error").contains("1〜65535"));
    }

    /// Integration test against a real PostgreSQL - only runs when
    /// `BANTO_TEST_PG_URL` is set (e.g.
    /// `postgres://postgres:postgres@127.0.0.1:5432/postgres`), otherwise
    /// skips with an `eprintln!` (same convention the task instructions
    /// specify, matching this workspace's other real-machine-gated tests).
    #[tokio::test]
    async fn test_connection_against_a_real_postgresql_returns_the_server_version() {
        let Ok(url) = std::env::var("BANTO_TEST_PG_URL") else {
            eprintln!("skipped: BANTO_TEST_PG_URL unset");
            return;
        };
        let options: PgConnectOptions = url
            .parse()
            .expect("BANTO_TEST_PG_URL should be a valid postgres:// URL");
        let conn = PlcConnection {
            id: 1,
            name: "Real PG".to_string(),
            protocol: "postgres".to_string(),
            host: options.get_host().to_string(),
            port: options.get_port() as i64,
            unit_id: 1,
            enabled: true,
            simulation: false,
            word_order: "low_high".to_string(),
            database: options.get_database().map(|s| s.to_string()),
            username: Some(options.get_username().to_string()),
            password: url_password(&url),
        };

        let outcome = test_connection(&conn).await;
        assert!(outcome.ok, "expected ok, got: {outcome:?}");
        let server_version = outcome
            .server_version
            .expect("a successful test should carry the server version");
        eprintln!("BANTO_TEST_PG_URL SELECT version() -> {server_version}");
        assert!(
            server_version.to_uppercase().contains("POSTGRESQL"),
            "unexpected server_version: {server_version}"
        );
    }

    /// Tiny helper for the real-PostgreSQL test above:
    /// `sqlx::postgres::PgConnectOptions` deliberately does not expose the
    /// password back out (by design - it is a write-only builder field), so
    /// this test extracts it from the URL string directly instead.
    #[cfg(test)]
    fn url_password(url: &str) -> Option<String> {
        let after_scheme = url.split_once("://")?.1;
        let userinfo = after_scheme.split_once('@')?.0;
        let password = userinfo.split_once(':')?.1;
        Some(password.to_string())
    }

    fn engine() -> DbSourceEngine {
        DbSourceEngine::new(Arc::new(ServerTagStore::new()), Arc::new(SystemClock))
    }

    fn conn_plan(id: i64, sql: &str) -> plan::DbConnectionPlan {
        plan::DbConnectionPlan {
            connection_id: id,
            connection_name: format!("db{id}"),
            // ポート 1 は（このプロセスでは）誰も listen していないので、
            // タスクは接続に失敗してバックオフに入るだけ - commit の差分
            // 挙動を確かめるのに実 DB は要らない。
            host: "127.0.0.1".to_string(),
            port: 1,
            database: "d".to_string(),
            username: "u".to_string(),
            password: "p".to_string(),
            groups: vec![plan::DbGroupPlan {
                group_id: 100 + id,
                group_name: "q1".to_string(),
                period_ms: 1_000,
                query_sql: sql.to_string(),
                tags: vec![plan::DbTagPlan {
                    tag_key: format!("tag:{id}"),
                    external_name: format!("db{id}.q1.A"),
                    column: "a".to_string(),
                }],
            }],
        }
    }

    /// [`DbSourceEngine::commit`] の差分方針: 設定が同じ接続のタスクは
    /// **同一のまま**（再起動しない）、設定が変わった接続は差し替え、
    /// 計画から消えた接続は止める。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn commit_restarts_only_the_connections_whose_config_changed() {
        let engine = engine();
        engine.commit(DbSourcePlan {
            connections: vec![
                conn_plan(1, "SELECT a FROM v1"),
                conn_plan(2, "SELECT a FROM v2"),
            ],
            disabled: Vec::new(),
        });
        let ids_after_first: Vec<i64> = {
            let state = engine.state.lock().unwrap();
            let mut ids: Vec<i64> = state.tasks.keys().copied().collect();
            ids.sort_unstable();
            ids
        };
        assert_eq!(ids_after_first, vec![1, 2]);
        let handle_1_id = {
            let state = engine.state.lock().unwrap();
            state.tasks.get(&1).unwrap().1.id()
        };

        // 接続 1 は不変、接続 2 は SQL 変更、接続 3 は新規、接続 1 の
        // 状態行は残るはず。
        engine.commit(DbSourcePlan {
            connections: vec![
                conn_plan(1, "SELECT a FROM v1"),
                conn_plan(2, "SELECT a FROM v2_new"),
                conn_plan(3, "SELECT a FROM v3"),
            ],
            disabled: Vec::new(),
        });
        {
            let state = engine.state.lock().unwrap();
            let mut ids: Vec<i64> = state.tasks.keys().copied().collect();
            ids.sort_unstable();
            assert_eq!(ids, vec![1, 2, 3]);
            assert_eq!(
                state.tasks.get(&1).unwrap().1.id(),
                handle_1_id,
                "an unchanged connection's task must not be restarted"
            );
        }

        // 計画から消えた接続は止まる。
        engine.commit(DbSourcePlan {
            connections: vec![conn_plan(1, "SELECT a FROM v1")],
            disabled: vec![(2, "db2".to_string())],
        });
        {
            let state = engine.state.lock().unwrap();
            let ids: Vec<i64> = state.tasks.keys().copied().collect();
            assert_eq!(ids, vec![1]);
        }
        let status = engine.status_snapshot();
        assert_eq!(status.len(), 2);
        assert_eq!(status[1].connection_id, 2);
        assert_eq!(status[1].state, DbConnectionState::Disabled);

        engine.shutdown().await;
        assert!(engine.state.lock().unwrap().tasks.is_empty());
    }

    /// 空の計画は何も spawn しない（PLC しか無い通常の構成 - 既存テストの
    /// ほぼ全部がこれ）。
    #[tokio::test]
    async fn commit_with_an_empty_plan_spawns_nothing() {
        let engine = engine();
        engine.commit(DbSourcePlan::default());
        assert!(engine.state.lock().unwrap().tasks.is_empty());
        assert!(engine.status_snapshot().is_empty());
    }
}
