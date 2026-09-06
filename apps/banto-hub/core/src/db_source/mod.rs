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
//! - [`supervisor`]: その task を監督する外側の task（S2b・§4.8・§6-17。
//!   panic / 予期せぬ終了をバックオフ付きで作り直す）。
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
//! グループ集合を再読込する」）。
//!
//! **S2b（§4.8・§6-16、2026-09-06 オーナー決定）: task が走るのは収集が
//! Running のときだけ。** エンジンは「最後に commit された計画」と「いま
//! 収集が Running か」の2つを持ち、実際に task が居るのは両方が揃って
//! いるときだけである。
//!
//! | 呼ばれるもの                      | 誰が呼ぶ                                              | 何をする                                                       |
//! | --------------------------------- | ----------------------------------------------------- | -------------------------------------------------------------- |
//! | [`DbSourceEngine::commit`]        | `CollectorManager::rebuild`/`commit_catalog`          | 計画を保存。Running なら接続単位の差分で task を作り直す       |
//! | [`DbSourceEngine::start`]         | `crate::controller::CollectionController::start_locked` | 保存済みの計画から task を起動する                             |
//! | [`DbSourceEngine::stop`]          | `crate::controller::CollectionController::stop_locked`  | task を止め、計画上の全 `db` タグへ Bad を書く                 |
//! | [`DbSourceEngine::shutdown`]      | `crate::runtime::RunningHub::shutdown`                | プロセス終了時に task を止める（値は書かない）                 |
//!
//! 収集停止時に**明示的に Bad を書く**のがこの設計の要点:
//! `crate::hub::effective_sample` は `!enabled`（タグ/接続が構成上無効）
//! しか見ないので、「構成上は有効だが収集を止めている」を表現できない。
//! 書かないと、読み手は停止前の最後の Good をいつまでも見続けてしまう。
//!
//! 停止順序について: [`DbSourceEngine::shutdown`] は
//! `crate::runtime::RunningHub::shutdown` の1点からだけ呼ぶ（そのモジュール
//! doc comment の「シャットダウン順序」節参照 - `ServerTagStore` と
//! `CollectorManager` を畳む**前**に止める）。`stop` も同じ理由で
//! `CollectorManager::stop` より前に呼ぶ。
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
pub mod supervisor;
pub mod task;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use banto_tags::PlcConnection;
use banto_tstore::Clock;
use serde::Serialize;
use sqlx::postgres::{PgConnectOptions, PgSslMode};
use sqlx::{Connection, Error as SqlxError};
use tokio::sync::watch;
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

/// S2b: PostgreSQL の `application_name` の接頭辞。DBA が
/// `pg_stat_activity` を見たときに「この接続は banto-hub の DB Source が
/// 張っている」と分かるようにする（統合テストが「収集停止中は本当に1本も
/// 繋いでいない」を裏取りするのにも使う）。
pub(crate) const APPLICATION_NAME_PREFIX: &str = "banto-hub-db-source";

/// `pg_connect_options` に渡す `application_name`。接頭辞に**接続名**を
/// 足すのは、DB 側で「どの Hub 接続のセッションか」を1行で見分けられる
/// ようにするため（PostgreSQL は 63 バイトで黙って切り詰めるが、識別の
/// 用途には十分）。
pub(crate) fn application_name(connection_name: &str) -> String {
    format!("{APPLICATION_NAME_PREFIX} {connection_name}")
}

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
    connection_name: &str,
) -> PgConnectOptions {
    PgConnectOptions::new()
        .host(host)
        .port(port)
        .database(database)
        .username(username)
        .password(password)
        .application_name(&application_name(connection_name))
        .ssl_mode(PgSslMode::Prefer)
}

/// DB Source のエンジン本体（このモジュールの doc comment「エンジンの
/// ライフサイクル」節）。`crate::computed::ComputedEngine` と同じく
/// `Arc` で共有され、[`Self::commit`] が唯一の書き込み口。
///
/// **並行呼び出しは想定していない**: [`Self::start`]・[`Self::stop`]・
/// [`Self::commit`] は互いに排他される前提で書かれており、特に
/// [`Self::stop`] の（内側 task の）join と Bad 書き込みは、その途中に
/// 別スレッドから [`Self::start`] が割り込まないことに依存している。
/// banto-hub では `crate::controller::CollectionController` が自分の
/// ロック（`start_locked`/`stop_locked`）越しにしか `start`/`stop` を
/// 呼ばず、`commit` は `crate::hub::CollectorManager::commit_catalog`
/// （および同じロック配下の rebuild 経路）経由でしか呼ばないので、この
/// 前提はこの2つの呼び出し元だけで担保されている - 将来もう1つ呼び出し元
/// を追加するときは、この直列化を崩さないこと。
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
    /// S2b（§4.8・§6-16）: 収集が Running か。`false` の間は
    /// [`EngineState::tasks`] は必ず空で、DB へは1本も繋がない。
    running: bool,
    /// S2b: **最後に commit された計画**。収集が停止中でも保持し、
    /// [`DbSourceEngine::start`] がここからタスクを起こす。
    plan: DbSourcePlan,
    /// 現在走っている supervisor タスク: 接続 id →
    /// （その世代の計画, [`ConnectionTask`]）。計画も一緒に持つのは
    /// [`DbSourceEngine::commit`] の差分判定（「設定が変わったか」）のため。
    tasks: HashMap<i64, (plan::DbConnectionPlan, ConnectionTask)>,
}

/// 1接続分の走行中 supervisor（`crate::db_source::supervisor` のモジュール
/// doc comment「取り消し」節）。`cancel` を送ってから `handle` を join
/// すると、supervisor が内側の `run_connection`（＝ `PgPool`）を join して
/// から戻るので、join 完了時点で DB セッションは確実に閉じている。
struct ConnectionTask {
    cancel: watch::Sender<bool>,
    handle: JoinHandle<()>,
}

impl ConnectionTask {
    /// join できない同期の文脈（[`DbSourceEngine::commit`]）用の即時停止。
    /// cancel も送っておくのは、`abort` が効く前に supervisor が走った
    /// 場合に綺麗な経路（内側を join してから戻る）を通れるようにするため。
    /// `abort` が先に効いた場合は
    /// `crate::db_source::supervisor` の `AbortOnDrop` ガードが内側を
    /// 道連れにする。
    fn cancel_and_abort(&self) {
        self.cancel.send_replace(true);
        self.handle.abort();
    }
}

/// [`DbSourceEngine::stop`]/`shutdown` が supervisor の join を諦めるまで
/// の上限。内側が abort に応じない（await 点を持たない）ことは
/// `run_connection` の構造上ありえないが、シャットダウンが永久に止まる
/// 事故だけは避ける - 超えたら `abort` へ切り替える。
const STOP_JOIN_TIMEOUT: Duration = Duration::from_secs(5);

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
    ///
    /// **S2b（§4.8・§6-16）**: 収集が Running でない間は「計画を覚える
    /// だけ」で、タスクは1本も起こさない（DB へも繋がない）。次の
    /// [`Self::start`] が保存済みの計画から起こす。停止中の commit では
    /// タグへ値を書かない - 停止中の `db` タグを Bad にするのは
    /// [`Self::stop`] の責務で、そちらが一度書けば以後誰も上書きしない
    /// （タスクが居ないのだから）。
    pub fn commit(&self, plan: DbSourcePlan) {
        let mut state = self.state.lock().expect("DbSourceEngine lock poisoned");
        let log = self
            .log
            .lock()
            .expect("DbSourceEngine log lock poisoned")
            .clone();

        state.plan = plan.clone();

        if !state.running {
            // 停止中: 走っているタスクは（定義上）無い。状態表だけ新しい
            // 計画に合わせて作り直す - すべて `stopped` で見える。
            debug_assert!(state.tasks.is_empty());
            self.status.apply_plan(&plan, &HashSet::new(), false);
            return;
        }

        let wanted: HashMap<i64, plan::DbConnectionPlan> = plan
            .connections
            .iter()
            .map(|conn| (conn.connection_id, conn.clone()))
            .collect();

        // 1) 計画から消えた／設定が変わった接続のタスクを止める。
        let mut preserved: HashSet<i64> = HashSet::new();
        state
            .tasks
            .retain(|id, (running_plan, task)| match wanted.get(id) {
                Some(new_plan) if new_plan == running_plan => {
                    preserved.insert(*id);
                    true
                }
                _ => {
                    task.cancel_and_abort();
                    false
                }
            });

        // 2) 状態表は「残す接続以外」を作り直す。
        self.status.apply_plan(&plan, &preserved, true);

        // 3) 新規／再起動対象を spawn する。
        for conn in plan.connections {
            if preserved.contains(&conn.connection_id) {
                continue;
            }
            let connection_id = conn.connection_id;
            let task = self.spawn_connection(conn.clone(), &log);
            state.tasks.insert(connection_id, (conn, task));
        }
    }

    /// S2b（§4.8・§6-16）: 収集開始。保存済みの計画から接続タスクを
    /// 起こす。既に Running なら何もしない（`start` は冪等 -
    /// `crate::controller::CollectionController::start` 自身と同じ契約）。
    ///
    /// 同期メソッドだが `tokio::spawn` を使うのでランタイムの中から
    /// 呼ぶこと（唯一の呼び出し元 `CollectionController::start_locked` は
    /// `async fn`）。
    pub fn start(&self) {
        let mut state = self.state.lock().expect("DbSourceEngine lock poisoned");
        if state.running {
            return;
        }
        let log = self
            .log
            .lock()
            .expect("DbSourceEngine log lock poisoned")
            .clone();
        state.running = true;

        let plan = state.plan.clone();
        self.status.apply_plan(&plan, &HashSet::new(), true);
        for conn in plan.connections {
            let connection_id = conn.connection_id;
            let task = self.spawn_connection(conn.clone(), &log);
            state.tasks.insert(connection_id, (conn, task));
        }
    }

    /// S2b（§4.8・§6-16）: 収集停止。タスクを止めて join し、**計画上の
    /// すべての `db` タグへ Bad を書く**（このモジュール doc comment
    /// 「エンジンのライフサイクル」節 - `effective_sample` は「停止中」を
    /// 表現できないので、ここで明示的に書かないと読み手は停止前の最後の
    /// Good を見続けてしまう）。状態表は全接続 `stopped` になる。
    ///
    /// 既に停止中なら何もしない（冪等）。
    pub async fn stop(&self) {
        let (tasks, plan) = {
            let mut state = self.state.lock().expect("DbSourceEngine lock poisoned");
            if !state.running {
                return;
            }
            state.running = false;
            let tasks: Vec<ConnectionTask> =
                state.tasks.drain().map(|(_, (_, task))| task).collect();
            (tasks, state.plan.clone())
        };

        join_tasks(tasks).await;

        // タスクが完全に止まった**後**で書く - 逆順だと、止まりきる前の
        // 最後の1周期が Bad を Good で上書きしうる。
        let now_ms = self.clock.now_ms();
        for conn in &plan.connections {
            task::mark_connection_bad(conn, &self.store, now_ms);
        }
        self.status.apply_plan(&plan, &HashSet::new(), false);
    }

    /// 全タスクを止めて join する（`crate::runtime::RunningHub::shutdown`
    /// の1点からだけ呼ぶ）。[`Self::stop`] と違って**タグへは何も書かない**。
    /// プロセスが終わるところなので読み手が居らず、畳んだ後のタグ空間を
    /// 触らない方が安全だから（`crate::runtime` の「シャットダウン順序」節）。
    pub async fn shutdown(&self) {
        let tasks: Vec<ConnectionTask> = {
            let mut state = self.state.lock().expect("DbSourceEngine lock poisoned");
            state.running = false;
            state.tasks.drain().map(|(_, (_, task))| task).collect()
        };
        join_tasks(tasks).await;
    }

    /// 1接続の supervisor を起こす（§6-17）。`run_connection` そのものでは
    /// なく `crate::db_source::supervisor::supervise` を spawn し、内側の
    /// future はファクトリから毎回作り直す - panic しても次の設定変更まで
    /// 死んだままにならない。
    fn spawn_connection(&self, conn: plan::DbConnectionPlan, log: &DiagLog) -> ConnectionTask {
        let deps = task::TaskDeps {
            store: self.store.clone(),
            status: self.status.clone(),
            clock: self.clock.clone(),
            log: log.clone(),
        };
        let ctx = supervisor::SupervisorCtx {
            connection_id: conn.connection_id,
            connection_name: conn.connection_name.clone(),
            status: self.status.clone(),
            log: log.clone(),
        };
        let (cancel, cancel_rx) = watch::channel(false);
        let handle = tokio::spawn(supervisor::supervise(
            move || task::run_connection(conn.clone(), deps.clone()),
            ctx,
            cancel_rx,
        ));
        ConnectionTask { cancel, handle }
    }
}

/// [`DbSourceEngine::stop`]/[`DbSourceEngine::shutdown`] 共通の停止手順:
/// 全 supervisor へ cancel を送ってから join する（先に全部へ送るので、
/// 接続数ぶんの停止が直列にならない）。join が [`STOP_JOIN_TIMEOUT`] を
/// 超えたら `abort` へ切り替える（その定数の doc comment）。
async fn join_tasks(tasks: Vec<ConnectionTask>) {
    for task in &tasks {
        task.cancel.send_replace(true);
    }
    for task in tasks {
        let mut handle = task.handle;
        if tokio::time::timeout(STOP_JOIN_TIMEOUT, &mut handle)
            .await
            .is_err()
        {
            handle.abort();
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
        &conn.name,
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
    ///
    /// S2b（§4.8・§6-16）: この差分方針が効くのは**収集が Running の
    /// ときだけ**なので、最初に [`DbSourceEngine::start`] を呼ぶ。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn commit_restarts_only_the_connections_whose_config_changed() {
        let engine = engine();
        engine.start();
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
            state.tasks.get(&1).unwrap().1.handle.id()
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
                state.tasks.get(&1).unwrap().1.handle.id(),
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
        engine.start();
        engine.commit(DbSourcePlan::default());
        assert!(engine.state.lock().unwrap().tasks.is_empty());
        assert!(engine.status_snapshot().is_empty());
    }

    // --- S2b: 収集 Running との連動（§4.8・§6-16） -------------------------

    /// 収集が Running でない間の `commit` は「計画を覚えるだけ」-
    /// タスクは1本も起きず（＝ DB へ繋がない）、状態表は `stopped`。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn commit_while_collection_is_stopped_spawns_nothing() {
        let engine = engine();
        engine.commit(DbSourcePlan {
            connections: vec![conn_plan(1, "SELECT a FROM v1")],
            disabled: vec![(2, "db2".to_string())],
        });

        assert!(
            engine.state.lock().unwrap().tasks.is_empty(),
            "a stopped engine must not open any DB connection"
        );
        let status = engine.status_snapshot();
        assert_eq!(status.len(), 2);
        assert_eq!(status[0].state, DbConnectionState::Stopped);
        assert!(status[0].last_error.is_none());
        // 無効な接続は収集状態に依らず `disabled`。
        assert_eq!(status[1].state, DbConnectionState::Disabled);
        // 計画は覚えている（次の `start` がここから起こす）。
        assert_eq!(engine.state.lock().unwrap().plan.connections.len(), 1);
    }

    /// 収集開始で、保存済みの計画からタスクが起きる。停止で止まり、
    /// **計画上の全 `db` タグへ Bad が書かれる**（§4.8・§6-16 -
    /// `effective_sample` は「停止中」を表現できないので、明示的に書かないと
    /// 読み手は停止前の最後の Good を見続けてしまう）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn start_spawns_from_the_stored_plan_and_stop_marks_every_tag_bad() {
        let store = Arc::new(ServerTagStore::new());
        let engine = DbSourceEngine::new(store.clone(), Arc::new(SystemClock));
        engine.commit(DbSourcePlan {
            connections: vec![conn_plan(1, "SELECT a FROM v1")],
            disabled: Vec::new(),
        });
        assert!(engine.state.lock().unwrap().tasks.is_empty());

        engine.start();
        {
            let state = engine.state.lock().unwrap();
            assert_eq!(state.tasks.keys().copied().collect::<Vec<_>>(), vec![1]);
        }
        assert_eq!(
            engine.status_snapshot()[0].state,
            DbConnectionState::Backoff,
            "a freshly started connection has not connected yet"
        );

        // 停止直前まで Good が読めていた状況を作る（タスクは繋がらない
        // ポート 1 を相手にしているので、値は自分で置く）。
        store.set("tag:1", Some(42.0), banto_collect::Quality::Good, 1_000);

        engine.stop().await;

        assert!(
            engine.state.lock().unwrap().tasks.is_empty(),
            "stop must abort and join every task"
        );
        let sample = store.get("tag:1").expect("the tag is still in the store");
        assert_eq!(sample.value, None, "stopped means Bad, not a stale Good");
        assert_eq!(sample.quality, banto_collect::Quality::Bad);
        assert_eq!(
            engine.status_snapshot()[0].state,
            DbConnectionState::Stopped
        );

        // 再開でまた起きる（`start`/`stop` は何度でも往復できる）。
        engine.start();
        assert_eq!(engine.state.lock().unwrap().tasks.len(), 1);
        engine.shutdown().await;
    }

    /// `start`/`stop` は冪等（`CollectionController::start`/`stop` 自身が
    /// 冪等なのと揃える - 二重呼び出しでタスクが二重に起きない）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn start_and_stop_are_idempotent() {
        let engine = engine();
        engine.commit(DbSourcePlan {
            connections: vec![conn_plan(1, "SELECT a FROM v1")],
            disabled: Vec::new(),
        });

        engine.start();
        engine.start();
        assert_eq!(engine.state.lock().unwrap().tasks.len(), 1);

        engine.stop().await;
        engine.stop().await;
        assert!(engine.state.lock().unwrap().tasks.is_empty());
    }

    /// `application_name` は接頭辞 + 接続名（S2b。統合テストが
    /// `pg_stat_activity` でこの値を数える）。
    #[test]
    fn application_name_carries_the_connection_name() {
        assert_eq!(
            application_name("erp"),
            format!("{APPLICATION_NAME_PREFIX} erp")
        );
    }
}
