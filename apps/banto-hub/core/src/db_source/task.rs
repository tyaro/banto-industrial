//! 1 DB 接続 = 1 tokio task のポーリング本体（docs/banto-hub-external-db-design.md
//! §4.2「実行モデル」・§4.3「Quality の変換」）。
//!
//! ## 構造
//!
//! [`run_connection`] は「接続する → 各グループの SQL を describe する →
//! 最小デッドライン方式で回す → 接続レベルの失敗で抜ける → バックオフして
//! 最初へ戻る」という**終わらないループ**。停止は
//! [`crate::db_source::DbSourceEngine`] が `JoinHandle::abort` で行う
//! （`crate::runtime::RunningHub::shutdown` の順序 - モジュール doc 参照）。
//!
//! グループのスケジューリングは「次に期限が来るグループまで眠る」最小
//! デッドライン方式で、`banto-collect` の収集タスクと考え方は同じだが
//! 実装は共有しない（§4.2 の明示: あちらは「1ソケット・アドレス範囲の
//! read_batch」前提で、「SELECT 1本 → N 列 → N タグ」には合わない）。
//! 1本の遅いクエリが他グループの周期を崩さないよう、クエリごとに
//! `min(period_ms * 0.8, 30s)` のタイムアウトを掛ける。
//!
//! ## Quality の状態機械（§4.3 の表）
//!
//! | 事象                       | 書く値   | quality | 単位     |
//! | -------------------------- | -------- | ------- | -------- |
//! | 正常に列が取れた           | 変換値   | Good    | タグ     |
//! | 列が NULL                  | None     | Bad     | タグ     |
//! | 列が無い / 型が未対応      | None     | Bad     | タグ     |
//! | 0 行                       | None     | Bad     | グループ |
//! | クエリエラー・タイムアウト | 直前値   | Stale   | グループ |
//! | 同上が2周期連続            | None     | Bad     | グループ |
//! | 接続断（バックオフ中）     | None     | Bad     | 接続     |
//!
//! 「1回の失敗で値を捨てない」（Stale を1周期だけ挟む）は収集キャッシュの
//! `STALE_PERIOD_FACTOR = 2.5` に合わせた運用感（§4.3 末尾）。Stale を書く
//! ときは**直前の `ptime_ms` も据え置く** - 値が実際に読めた時刻を後ろへ
//! ずらさないため（`crate::hub::effective_sample` は保存された値をそのまま
//! 返すので、ここが唯一の決定点）。
//!
//! ## ログの抑制
//!
//! `warn` は**初回失敗時とバックオフ段階が変わったときだけ**（§4.2 末尾・
//! MQTT publisher と同じ抑制）。復帰時は `info` 相当を1行。毎ティック
//! 出さない。

use std::sync::Arc;
use std::time::{Duration, Instant};

use banto_collect::Quality;
use banto_tstore::Clock;
use sqlx::postgres::{PgPool, PgPoolOptions};
use sqlx::{Executor, Row, SqlSafeStr};

use crate::computed::ServerTagStore;
use crate::diag_log::DiagLog;

use super::plan::{resolve_group, DbConnectionPlan, DbGroupPlan, DbTagPlan, ResolvedGroup};
use super::status::{DbConnectionState, DbConnectionStatus, DbSourceStatusStore};
use super::{pg_connect_options, sanitize_postgres_error};

/// 接続プールの最大接続数（§4.2「最大接続数は小さく固定、例 2」）。
/// 1接続タスクは同時に1本のクエリしか投げないので 1 でも足りるが、
/// 障害からの復帰時に古い接続が閉じ切る前に新しい接続を張れるよう 2 にする。
const POOL_MAX_CONNECTIONS: u32 = 2;

/// プールから接続を取り出すまでの上限（S1a の接続テストと同じ5秒）。
const POOL_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);

/// 1クエリのタイムアウト上限（§4.2「`min(period_ms * 0.8, 30s)`」）。
const MAX_QUERY_TIMEOUT: Duration = Duration::from_secs(30);

/// 再接続バックオフ（§4.2「1s → 30s 上限、`banto-broker` の `backoff_delay`
/// と同じ定数」）。`banto_broker::BackoffConfig::default()` の
/// `base = 1s` / `cap = 30s` / `factor = 2` と**同じ数値**をここに書き写して
/// ある - `banto_broker::backoff_delay` は非公開（`pub` でない）関数であり、
/// 公開のためだけにあちらの API を広げるより、値を複製して由来を doc comment
/// で結ぶ方がクレート境界として素直だと判断した（DB 接続は broker の
/// セッション機構を一切通らないので、実装を共有する意味も無い）。
const BACKOFF_BASE: Duration = Duration::from_secs(1);
const BACKOFF_CAP: Duration = Duration::from_secs(30);
const BACKOFF_FACTOR: u32 = 2;

/// `attempt` 回目（1 始まり）の再接続までの待ち時間。
/// `banto_broker::backoff_delay` と同じ形（`base * factor^(attempt-1)`、
/// `cap` で頭打ち）。
pub(crate) fn backoff_delay(attempt: u32) -> Duration {
    if attempt == 0 {
        return Duration::ZERO;
    }
    let growth = (BACKOFF_FACTOR as u64)
        .checked_pow(attempt - 1)
        .unwrap_or(u64::MAX);
    let ms = (BACKOFF_BASE.as_millis() as u64).saturating_mul(growth);
    Duration::from_millis(ms).min(BACKOFF_CAP)
}

/// describe 再試行の最短間隔（レビュー対応: 「groups never re-describe
/// after a describe failure or after degrading to Bad」）。周期が極端に
/// 短いグループ（100ms 等）で describe を毎ティック投げ続けないための
/// 下限 - `query_timeout` が `period_ms` を下げ方向に丸めるのと対称に、
/// こちらは上げ方向に丸める。
const DESCRIBE_RETRY_MIN: Duration = Duration::from_secs(5);

/// 1グループが再 describe を試みてよい最短間隔 - `max(period, 5s)`。
fn describe_retry_interval(period_ms: u64) -> Duration {
    Duration::from_millis(period_ms).max(DESCRIBE_RETRY_MIN)
}

/// 1グループのクエリタイムアウト（§4.2）。
pub(crate) fn query_timeout(period_ms: u64) -> Duration {
    // `period_ms * 0.8` を整数で（`* 4 / 5`）。周期が極端に短い設定
    // （100ms → 80ms）でも設計どおり周期未満を守る - 遅い DB に対して
    // 100ms 周期を選ぶこと自体が構成側の判断であり、ここで勝手に伸ばすと
    // 「1本の遅いクエリが接続全体の周期を崩さない」という §4.2 の要求を
    // 破ってしまう。
    Duration::from_millis(period_ms.saturating_mul(4) / 5).min(MAX_QUERY_TIMEOUT)
}

/// 失敗の粒度（§4.3 の「グループ単位」と「接続単位」の分かれ目）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PgFailure {
    /// サーバーへ届いていない・セッションが壊れた - 接続単位で扱い、
    /// 配下の全タグを Bad にしてバックオフへ入る。
    Connection(String),
    /// サーバーは応答したが文が失敗した（構文エラー・権限・存在しない表）
    /// - グループ単位で扱い、他のグループには波及させない。
    Statement(String),
}

impl PgFailure {
    pub(crate) fn message(&self) -> &str {
        match self {
            PgFailure::Connection(message) | PgFailure::Statement(message) => message,
        }
    }
}

/// `sqlx::Error` を [`PgFailure`] へ分類する。**未知のバリアントは
/// `Statement` 側**に倒す - 誤って `Connection` に倒すと、実際には健全な
/// プールを毎回捨てて再接続し続けることになり、他の（正常な）グループまで
/// 巻き込むため。
pub(crate) fn classify(err: &sqlx::Error) -> PgFailure {
    let message = sanitize_postgres_error(err);
    match err {
        sqlx::Error::Io(_)
        | sqlx::Error::Tls(_)
        | sqlx::Error::Protocol(_)
        | sqlx::Error::PoolTimedOut
        | sqlx::Error::PoolClosed
        | sqlx::Error::Configuration(_) => PgFailure::Connection(message),
        _ => PgFailure::Statement(message),
    }
}

/// [`run_connection`] が使う共有物（`crate::db_source::DbSourceEngine` が
/// タスク生成時に clone して渡す）。
#[derive(Clone)]
pub(crate) struct TaskDeps {
    pub store: Arc<ServerTagStore>,
    pub status: Arc<DbSourceStatusStore>,
    pub clock: Arc<dyn Clock>,
    pub log: DiagLog,
}

/// 1グループの走行時状態。
struct GroupRuntime {
    plan: DbGroupPlan,
    resolved: ResolvedGroup,
    next_due: Instant,
    /// 連続したグループ単位の失敗回数（§4.3「2周期連続で失敗したら Bad」）。
    consecutive_failures: u32,
    /// 「2行以上」の `warn` をこの接続世代で既に出したか（§6-8「先頭行採用
    /// ＋warn 1回」）。
    warned_multi_row: bool,
    /// 次に `describe_group` を再試行してよい時刻（レビュー対応）。`None`
    /// は「まだ一度もこの世代で再試行していない」-
    /// `consecutive_failures` が初めて 2 に達したティックでは即座に
    /// 再試行してよい、という意味で使う（`wrapper_sql: None` のグループは
    /// [`poll_forever`] の起動時 describe が失敗した時点で `Some` を積む -
    /// そちらは「直前の describe からまだ間隔が空いていない」の意味）。
    next_describe_retry: Option<Instant>,
    /// 直近の再 describe 失敗メッセージ（`Statement` のみ）。同じ理由で
    /// 何度も `status.last_error` を書き換えない・`warn` を出さないための
    /// 抑制（このモジュール doc comment「ログの抑制」節と同じ考え方）。
    last_describe_error: Option<String>,
}

/// このモジュール doc comment の「構造」節そのもの。**戻らない**。
pub(crate) async fn run_connection(plan: DbConnectionPlan, deps: TaskDeps) {
    let mut attempt: u32 = 0;
    let mut last_delay: Option<Duration> = None;

    loop {
        match connect(&plan).await {
            Ok(pool) => {
                if attempt > 0 {
                    deps.log.line(&format!(
                        "banto-hub: DB Source: 接続 {} へ再接続しました",
                        plan.connection_name
                    ));
                }
                attempt = 0;
                last_delay = None;
                deps.status.update_connection(plan.connection_id, |status| {
                    status.state = DbConnectionState::Connected;
                    status.consecutive_failures = 0;
                    status.last_error = None;
                });

                let failure = poll_forever(&pool, &plan, &deps).await;
                // プールは接続断で捨てる（次のループで張り直す）。
                pool.close().await;
                mark_all_bad(&plan, &deps);
                attempt = attempt.saturating_add(1);
                deps.status.update_connection(plan.connection_id, |status| {
                    // レビュー対応: `Err` 分岐（下）と同じ
                    // `record_connection_failure` を通す - 接続が張れた後の
                    // 切断でも `consecutive_failures` を `attempt` と揃える
                    // （以前はここだけ 0 のまま取り残されていた）。
                    record_connection_failure(
                        status,
                        DbConnectionState::Backoff,
                        attempt,
                        failure.clone(),
                    );
                });
                deps.log.err_line(&format!(
                    "banto-hub: [WARN] DB Source: 接続 {} が切断されました: {failure}",
                    plan.connection_name
                ));
            }
            Err(failure) => {
                mark_all_bad(&plan, &deps);
                let rejected = matches!(failure, PgFailure::Statement(_));
                let message = failure.message().to_string();
                attempt = attempt.saturating_add(1);
                deps.status.update_connection(plan.connection_id, |status| {
                    // サーバーが応答した上で拒否した（認証失敗・DB 名違い）
                    // なら `error` - 再試行はするが構成を直さない限り直らない
                    // ことを状態画面で区別できるようにする
                    // （`DbConnectionState::Error` の doc comment）。
                    let state = if rejected {
                        DbConnectionState::Error
                    } else {
                        DbConnectionState::Backoff
                    };
                    record_connection_failure(status, state, attempt, message.clone());
                });
            }
        }

        let delay = backoff_delay(attempt);
        // §4.2 末尾: 初回失敗時とバックオフ段階が変わったときだけ warn。
        if last_delay != Some(delay) {
            deps.log.err_line(&format!(
                "banto-hub: [WARN] DB Source: 接続 {} への再接続を {} 秒後に再試行します（{} 回目の失敗）",
                plan.connection_name,
                delay.as_secs_f64(),
                attempt
            ));
            last_delay = Some(delay);
        }
        tokio::time::sleep(delay).await;
    }
}

/// プールを張り、各グループの SQL を1回だけ describe して実行形へ解決する
/// までを行う。describe が**文の失敗**で落ちたグループは
/// 「SQL を投げない（全タグ Bad）」形で保持し、接続そのものは生かす。
async fn connect(plan: &DbConnectionPlan) -> Result<PgPool, PgFailure> {
    let options = pg_connect_options(
        &plan.host,
        plan.port,
        &plan.database,
        &plan.username,
        &plan.password,
    );
    PgPoolOptions::new()
        .max_connections(POOL_MAX_CONNECTIONS)
        .acquire_timeout(POOL_ACQUIRE_TIMEOUT)
        .connect_with(options)
        .await
        .map_err(|err| classify(&err))
}

/// 接続レベルの失敗が起きるまで回り続け、その理由を返す。
async fn poll_forever(pool: &PgPool, plan: &DbConnectionPlan, deps: &TaskDeps) -> String {
    let now = Instant::now();
    let mut groups: Vec<GroupRuntime> = Vec::with_capacity(plan.groups.len());
    for group in &plan.groups {
        let mut next_describe_retry = None;
        let mut last_describe_error = None;
        let resolved = match describe_group(pool, group).await {
            Ok(resolved) => resolved,
            Err(PgFailure::Connection(message)) => return message,
            Err(PgFailure::Statement(message)) => {
                // 文が壊れている（構文エラー・存在しない表・権限）。
                // 他のグループには波及させず、このグループだけ「SQL を
                // 投げない」形にしてティックごとに Bad を書く。起動時の
                // describe をこの瞬間の1回に数え、次に試せるのは
                // `describe_retry_interval` 後（レビュー対応 - このティック
                // で即座に再試行しない）。
                next_describe_retry = Some(now + describe_retry_interval(group.period_ms));
                last_describe_error = Some(message.clone());
                deps.status
                    .update_group(plan.connection_id, group.group_id, |status| {
                        status.last_error = Some(message.clone());
                    });
                deps.log.err_line(&format!(
                    "banto-hub: [WARN] DB Source: {}.{} の SQL を解析できません: {message}",
                    plan.connection_name, group.group_name
                ));
                ResolvedGroup {
                    wrapper_sql: None,
                    value_tags: Vec::new(),
                    bad_tags: group
                        .tags
                        .iter()
                        .map(|tag| {
                            (
                                tag.clone(),
                                super::plan::TagBadReason::StatementNotPrepared(message.clone()),
                            )
                        })
                        .collect(),
                }
            }
        };

        // §4.3「列が無い / 型変換不能 → タグ単位 Bad（起動時と設定変更時に
        // `warn`）」: プラン世代ごとに1回だけ出す（毎ティックは出さない）。
        for (tag, reason) in &resolved.bad_tags {
            deps.log.err_line(&format!(
                "banto-hub: [WARN] DB Source: タグ {} は値を取得できません: {}",
                tag.external_name,
                reason.message()
            ));
        }

        groups.push(GroupRuntime {
            plan: group.clone(),
            resolved,
            next_due: now,
            consecutive_failures: 0,
            warned_multi_row: false,
            next_describe_retry,
            last_describe_error,
        });
    }

    loop {
        // 最小デッドライン方式（このモジュール doc comment「構造」節）。
        let Some(index) = groups
            .iter()
            .enumerate()
            .min_by_key(|(_, group)| group.next_due)
            .map(|(index, _)| index)
        else {
            // 回すグループが1本も無い接続はそもそもタスクを起動しない
            // （`plan::build_plan`）ので到達しない防御的分岐。
            return "実行するグループがありません".to_string();
        };

        let due = groups[index].next_due;
        let now = Instant::now();
        if due > now {
            tokio::time::sleep(due - now).await;
        }

        if let Err(message) = run_group_tick(pool, plan, &mut groups[index], deps).await {
            return message;
        }

        let period = Duration::from_millis(groups[index].plan.period_ms);
        let mut next = groups[index].next_due + period;
        let now = Instant::now();
        if next <= now {
            // 実行が周期より長引いた場合、遅れを取り戻そうと連続実行しない
            // （`tokio::time::MissedTickBehavior::Delay` と同じ考え方 -
            // `crate::runtime` の 250ms 評価ループが選んでいるのと同じ）。
            next = now + period;
        }
        groups[index].next_due = next;
    }
}

/// `describe` 再試行のレート制限を判定する純関数（ユニットテスト用に
/// [`maybe_redescribe`] の呼び出し可否からも分離してある - 実 DB なしで
/// 「間隔内は再試行しない・間隔を過ぎたら再試行する」を確認できる）。
/// `None`（まだこの世代で一度も再試行していない）は常に「試してよい」。
fn describe_retry_due(next_describe_retry: Option<Instant>, now: Instant) -> bool {
    match next_describe_retry {
        None => true,
        Some(due) => now >= due,
    }
}

/// 再 describe が成功したとき、その結果を採用してよいかを判定する純関数
/// （レビュー対応）。`wrapper_sql: None` から復帰する場合は無条件に採用、
/// 既に `Some` だったグループ（2周期連続失敗のグループ）では describe の
/// 結果が実際に変わったとき（列・型が変わった）だけ - 変わっていなければ
/// 黙って既存の `resolved` のまま次のクエリへ進む。
fn should_adopt_redescribed(current: &ResolvedGroup, candidate: &ResolvedGroup) -> bool {
    current.wrapper_sql.is_none() || candidate != current
}

async fn describe_group(pool: &PgPool, group: &DbGroupPlan) -> Result<ResolvedGroup, PgFailure> {
    let sql = super::convert::describe_wrapper_sql(&group.query_sql);
    // AssertSqlSafe: 文の本体は運用者が登録した `query_sql`（`banto_tags` が
    // 「先頭 SELECT/WITH・単文」までベストエフォート検証済み）で、外から
    // 与えられた値を文字列連結しているわけではない。**本当の防御は DB 側の
    // read-only ユーザー**（設計 §4.2）。
    let described = pool
        .describe(sqlx::AssertSqlSafe(sql).into_sql_str())
        .await
        .map_err(|err| classify(&err))?;
    Ok(resolve_group(
        group,
        &super::convert::describe_columns(&described),
    ))
}

/// [`apply_redescribe_result`] が呼び出し元へ返す「何をすべきか」。
/// DB I/O を持たない純粋なデータなので、ユニットテストが
/// [`apply_redescribe_result`] を直接叩いて実 DB なしに検証できる。
#[derive(Debug, PartialEq)]
enum RedescribeEffect {
    /// 接続レベルの失敗 - 呼び出し元は `poll_forever` を抜ける。
    ConnectionFailure(String),
    /// 何もしない（レート制限内で呼ばれることは無いが、Statement 失敗の
    /// メッセージが前回と同じ・または describe の結果が変わらなかった）。
    NoChange,
    /// Statement 失敗で、メッセージが前回から変わった - `status.last_error`
    /// だけ書き換える（「no repeated warn spam」なので `warn` は出さない）。
    ErrorMessageChanged(String),
    /// `wrapper_sql: None` から復帰、または `Some` のまま describe の結果が
    /// 変わった（列・型が変わった）- resolved は既に差し替え済みで、ここに
    /// 積んだログを呼び出し側が実際に出す。
    Recovered {
        info_log: String,
        bad_tag_warnings: Vec<String>,
    },
}

/// [`maybe_redescribe`] から DB I/O を除いた、状態遷移そのものを持つ純関数
/// （レビュー対応）。`group` を書き換え、呼び出し側が行うべきログ出力・
/// 状態ストア更新を [`RedescribeEffect`] として返す。
fn apply_redescribe_result(
    group: &mut GroupRuntime,
    connection_name: &str,
    outcome: Result<ResolvedGroup, PgFailure>,
) -> RedescribeEffect {
    match outcome {
        Err(PgFailure::Connection(message)) => RedescribeEffect::ConnectionFailure(message),
        Err(PgFailure::Statement(message)) => {
            // 「no repeated warn spam」: `warn` はここでは一切出さない -
            // 初回の describe 失敗（`poll_forever` の起動時）または直近の
            // クエリ失敗（`on_group_failure`）で既に知らせている。
            // `status.last_error` もメッセージが変わったときだけ書く。
            if group.last_describe_error.as_deref() == Some(message.as_str()) {
                RedescribeEffect::NoChange
            } else {
                group.last_describe_error = Some(message.clone());
                RedescribeEffect::ErrorMessageChanged(message)
            }
        }
        Ok(resolved) => {
            // `wrapper_sql: None` から復帰する場合は無条件に、`Some` の
            // ままだった場合（2周期連続失敗のグループ）は describe の
            // 結果が実際に変わったとき（列・型が変わった）だけ差し替える
            // - 変わっていなければ黙って既存の `resolved` のまま次の
            // クエリへ進む（このモジュールのレビュー対応節）。
            if !should_adopt_redescribed(&group.resolved, &resolved) {
                return RedescribeEffect::NoChange;
            }
            group.resolved = resolved;
            group.consecutive_failures = 0;
            group.last_describe_error = None;
            let info_log = format!(
                "banto-hub: DB Source: {}.{} の SQL を再解析して復帰しました",
                connection_name, group.plan.group_name
            );
            let bad_tag_warnings = group
                .resolved
                .bad_tags
                .iter()
                .map(|(tag, reason)| {
                    format!(
                        "banto-hub: [WARN] DB Source: タグ {} は値を取得できません: {}",
                        tag.external_name,
                        reason.message()
                    )
                })
                .collect();
            RedescribeEffect::Recovered {
                info_log,
                bad_tag_warnings,
            }
        }
    }
}

/// レビュー対応: `describe` に失敗したまま・または2周期以上 Bad の梯子に
/// 乗ったままのグループを、レート制限（`describe_retry_interval`）付きで
/// 再 describe する。呼び出し前に `next_describe_retry` を次の間隔へ
/// 進めておく - 成功・失敗いずれでも「次に試せるのは1間隔後」という
/// レート制限を守るため。`Err` は**接続レベル**の失敗（`run_group_tick`
/// 経由で [`poll_forever`] を抜ける）。
async fn maybe_redescribe(
    pool: &PgPool,
    plan: &DbConnectionPlan,
    group: &mut GroupRuntime,
    deps: &TaskDeps,
    now: Instant,
) -> Result<(), String> {
    group.next_describe_retry = Some(now + describe_retry_interval(group.plan.period_ms));
    let outcome = describe_group(pool, &group.plan).await;

    match apply_redescribe_result(group, &plan.connection_name, outcome) {
        RedescribeEffect::ConnectionFailure(message) => Err(message),
        RedescribeEffect::NoChange => Ok(()),
        RedescribeEffect::ErrorMessageChanged(message) => {
            deps.status
                .update_group(plan.connection_id, group.plan.group_id, |status| {
                    status.last_error = Some(message);
                });
            Ok(())
        }
        RedescribeEffect::Recovered {
            info_log,
            bad_tag_warnings,
        } => {
            deps.status
                .update_group(plan.connection_id, group.plan.group_id, |status| {
                    status.last_error = None;
                });
            deps.log.line(&info_log);
            for warning in &bad_tag_warnings {
                deps.log.err_line(warning);
            }
            Ok(())
        }
    }
}

/// 1グループ1周期分。`Err` は**接続レベル**の失敗（呼び出し元が
/// [`poll_forever`] を抜けて再接続へ入る）。
async fn run_group_tick(
    pool: &PgPool,
    plan: &DbConnectionPlan,
    group: &mut GroupRuntime,
    deps: &TaskDeps,
) -> Result<(), String> {
    // レビュー対応: SQL を投げていない（`wrapper_sql: None`）グループ、
    // および Bad の梯子に乗って2周期以上経つグループは、クエリの前に
    // レート制限付きで再 describe する - `describe` 失敗のまま・古い
    // wrapper のまま固定されないようにする（このモジュール doc comment
    // 「構造」節・PR レビュー指摘）。
    let needs_redescribe = group.resolved.wrapper_sql.is_none() || group.consecutive_failures >= 2;
    if needs_redescribe {
        let now = Instant::now();
        if describe_retry_due(group.next_describe_retry, now) {
            maybe_redescribe(pool, plan, group, deps, now).await?;
        }
    }

    let Some(sql) = group.resolved.wrapper_sql.clone() else {
        // 値の取れる列が1つも無いグループ - SQL は投げず、Bad だけ書く。
        let now_ms = deps.clock.now_ms();
        write_bad_tags(&group.resolved.bad_tags, deps, now_ms);
        return Ok(());
    };

    let timeout = query_timeout(group.plan.period_ms);
    // AssertSqlSafe: `describe_group` と同じ理由（生成側は
    // `convert::build_wrapper_sql` で、列名は必ず `quote_ident` を通る）。
    let query = sqlx::query(sqlx::AssertSqlSafe(sql)).fetch_all(pool);
    let outcome = match tokio::time::timeout(timeout, query).await {
        Err(_elapsed) => Err(PgFailure::Statement(format!(
            "クエリがタイムアウトしました({:.1}秒)",
            timeout.as_secs_f64()
        ))),
        Ok(Ok(rows)) => Ok(rows),
        Ok(Err(err)) => Err(classify(&err)),
    };

    let rows = match outcome {
        Ok(rows) => rows,
        Err(PgFailure::Connection(message)) => return Err(message),
        Err(PgFailure::Statement(message)) => {
            on_group_failure(plan, group, deps, message);
            return Ok(());
        }
    };

    let now_ms = deps.clock.now_ms();
    let row_count = rows.len() as i64;

    if rows.is_empty() {
        // §4.3「0 行 → グループ単位 Bad」。クエリ自体は成功しているので
        // Stale の梯子（2周期）には乗せず、直ちに Bad にする。
        write_bad_tags(&group.resolved.bad_tags, deps, now_ms);
        for tag in &group.resolved.value_tags {
            deps.store.set(&tag.tag_key, None, Quality::Bad, now_ms);
        }
        group.consecutive_failures = 0;
        deps.status
            .update_group(plan.connection_id, group.plan.group_id, |status| {
                status.row_count_last = Some(0);
                status.last_error = Some("結果が 0 行です".to_string());
            });
        deps.status.update_connection(plan.connection_id, |status| {
            status.last_poll_at = Some(now_ms);
        });
        return Ok(());
    }

    if row_count >= 2 && !group.warned_multi_row {
        // §6-8「先頭行採用＋warn 1回」。
        group.warned_multi_row = true;
        deps.log.err_line(&format!(
            "banto-hub: [WARN] DB Source: {}.{} の SQL が複数行を返しています - 先頭行を採用します",
            plan.connection_name, group.plan.group_name
        ));
    }

    let row = &rows[0];
    for (index, tag) in group.resolved.value_tags.iter().enumerate() {
        let value: Option<f64> = row.try_get(index).unwrap_or(None);
        // 非有限値（`'NaN'::float8` など）は JSON にも載せられないため Bad
        // に倒す - 「欠測を隠さない」（設計 §4）の一環。
        let (value, quality) = match value {
            Some(x) if x.is_finite() => (Some(x), Quality::Good),
            _ => (None, Quality::Bad),
        };
        deps.store.set(&tag.tag_key, value, quality, now_ms);
    }
    write_bad_tags(&group.resolved.bad_tags, deps, now_ms);

    group.consecutive_failures = 0;
    deps.status
        .update_group(plan.connection_id, group.plan.group_id, |status| {
            status.last_ok_at = Some(now_ms);
            status.last_error = None;
            status.row_count_last = Some(row_count);
        });
    deps.status.update_connection(plan.connection_id, |status| {
        status.last_poll_at = Some(now_ms);
    });
    Ok(())
}

/// §4.3「クエリエラー・タイムアウト → 直前値 + Stale、2周期連続で Bad」。
fn on_group_failure(
    plan: &DbConnectionPlan,
    group: &mut GroupRuntime,
    deps: &TaskDeps,
    message: String,
) {
    group.consecutive_failures = group.consecutive_failures.saturating_add(1);
    let now_ms = deps.clock.now_ms();

    if group.consecutive_failures == 1 {
        // 初回だけ warn（このモジュール doc comment「ログの抑制」節）。
        deps.log.err_line(&format!(
            "banto-hub: [WARN] DB Source: {}.{} のクエリが失敗しました: {message}",
            plan.connection_name, group.plan.group_name
        ));
    }

    let degrade_to_bad = group.consecutive_failures >= 2;
    for tag in group
        .resolved
        .value_tags
        .iter()
        .chain(group.resolved.bad_tags.iter().map(|(tag, _)| tag))
    {
        if degrade_to_bad {
            deps.store.set(&tag.tag_key, None, Quality::Bad, now_ms);
        } else {
            write_stale(&deps.store, tag, now_ms);
        }
    }

    deps.status
        .update_group(plan.connection_id, group.plan.group_id, |status| {
            status.last_error = Some(message.clone());
        });
    deps.status.update_connection(plan.connection_id, |status| {
        status.last_poll_at = Some(now_ms);
    });
}

/// 「直前値を残したまま Stale にする」（このモジュール doc comment の
/// Quality 表）。直前値そのものが無い（まだ一度も読めていない）場合は
/// Stale にする古い値が存在しないので `(None, Bad, now)` を書く - 値なしの
/// Stale は「古い値がある」という嘘になるため。
fn write_stale(store: &ServerTagStore, tag: &DbTagPlan, now_ms: i64) {
    match store.get(&tag.tag_key) {
        Some(previous) if previous.value.is_some() => {
            store.set(
                &tag.tag_key,
                previous.value,
                Quality::Stale,
                previous.ptime_ms,
            );
        }
        _ => store.set(&tag.tag_key, None, Quality::Bad, now_ms),
    }
}

fn write_bad_tags(
    bad_tags: &[(DbTagPlan, super::plan::TagBadReason)],
    deps: &TaskDeps,
    now_ms: i64,
) {
    for (tag, _) in bad_tags {
        deps.store.set(&tag.tag_key, None, Quality::Bad, now_ms);
    }
}

/// レビュー対応 (Fix 2): 接続レベルの失敗を状態ストアへ書く、たった1つの
/// 経路。「プールは張れたが `poll_forever` が接続断で抜けた」（`Ok(pool)`
/// 分岐）と「プールがそもそも張れなかった」（`Err` 分岐）の両方が同じ
/// 意味で `consecutive_failures` を更新するようにここへ集約した -
/// 以前は前者だけこの代入を欠いていて、`state != Connected` なのに
/// `consecutive_failures == 0` のまま（`attempt` は 1 以上）という
/// 矛盾した状態を書いていた。
fn record_connection_failure(
    status: &mut DbConnectionStatus,
    state: DbConnectionState,
    attempt: u32,
    message: String,
) {
    status.state = state;
    status.consecutive_failures = attempt;
    status.last_error = Some(message);
}

/// §4.3「接続断（バックオフ中）→ 接続単位で全タグ Bad」。
fn mark_all_bad(plan: &DbConnectionPlan, deps: &TaskDeps) {
    let now_ms = deps.clock.now_ms();
    for group in &plan.groups {
        for tag in &group.tags {
            deps.store.set(&tag.tag_key, None, Quality::Bad, now_ms);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db_source::plan::{DbGroupPlan, ResolvedGroup, TagBadReason};
    use crate::db_source::status::DbSourceStatusStore;
    use banto_tstore::SystemClock;

    fn tag(n: i64) -> DbTagPlan {
        DbTagPlan {
            tag_key: format!("tag:{n}"),
            external_name: format!("erp.q1.T{n}"),
            column: format!("c{n}"),
        }
    }

    fn deps() -> TaskDeps {
        TaskDeps {
            store: Arc::new(ServerTagStore::new()),
            status: Arc::new(DbSourceStatusStore::new()),
            clock: Arc::new(SystemClock),
            log: DiagLog::new(|_| {}, |_| {}),
        }
    }

    fn connection_plan() -> DbConnectionPlan {
        DbConnectionPlan {
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
                query_sql: "SELECT c1 FROM v1".to_string(),
                tags: vec![tag(1)],
            }],
        }
    }

    fn group_runtime() -> GroupRuntime {
        GroupRuntime {
            plan: connection_plan().groups[0].clone(),
            resolved: ResolvedGroup {
                wrapper_sql: Some("SELECT 1".to_string()),
                value_tags: vec![tag(1)],
                bad_tags: Vec::new(),
            },
            next_due: Instant::now(),
            consecutive_failures: 0,
            warned_multi_row: false,
            next_describe_retry: None,
            last_describe_error: None,
        }
    }

    /// §4.2「1s → 30s 上限」（`banto_broker::backoff_delay` と同じ数値）。
    #[test]
    fn backoff_delay_grows_from_one_second_and_caps_at_thirty() {
        assert_eq!(backoff_delay(0), Duration::ZERO);
        assert_eq!(backoff_delay(1), Duration::from_secs(1));
        assert_eq!(backoff_delay(2), Duration::from_secs(2));
        assert_eq!(backoff_delay(3), Duration::from_secs(4));
        assert_eq!(backoff_delay(4), Duration::from_secs(8));
        assert_eq!(backoff_delay(5), Duration::from_secs(16));
        assert_eq!(backoff_delay(6), Duration::from_secs(30));
        assert_eq!(backoff_delay(100), Duration::from_secs(30));
        // 巨大な attempt でもオーバーフローしない。
        assert_eq!(backoff_delay(u32::MAX), Duration::from_secs(30));
    }

    /// §4.2「`min(period_ms * 0.8, 30s)`」。
    #[test]
    fn query_timeout_is_eighty_percent_of_the_period_capped_at_thirty_seconds() {
        assert_eq!(query_timeout(100), Duration::from_millis(80));
        assert_eq!(query_timeout(1_000), Duration::from_millis(800));
        assert_eq!(query_timeout(10_000), Duration::from_millis(8_000));
        // 60s * 0.8 = 48s > 30s cap.
        assert_eq!(query_timeout(60_000), Duration::from_secs(30));
    }

    #[test]
    fn classify_separates_transport_failures_from_statement_failures() {
        let io = sqlx::Error::Io(std::io::Error::other("boom"));
        assert!(matches!(classify(&io), PgFailure::Connection(_)));
        assert!(matches!(
            classify(&sqlx::Error::PoolTimedOut),
            PgFailure::Connection(_)
        ));
        assert!(matches!(
            classify(&sqlx::Error::PoolClosed),
            PgFailure::Connection(_)
        ));
        // 未知/その他は Statement 側へ倒す（この関数の doc comment）。
        assert!(matches!(
            classify(&sqlx::Error::RowNotFound),
            PgFailure::Statement(_)
        ));
    }

    /// §4.3: 1回目の失敗は「直前値 + Stale」、2回目で Bad。
    #[test]
    fn group_failure_goes_stale_first_then_bad_on_the_second_consecutive_failure() {
        let deps = deps();
        let plan = connection_plan();
        deps.status.apply_plan(
            &crate::db_source::plan::DbSourcePlan {
                connections: vec![plan.clone()],
                disabled: Vec::new(),
            },
            &std::collections::HashSet::new(),
        );
        let mut group = group_runtime();

        // 直前に読めていた値。
        deps.store.set("tag:1", Some(12.5), Quality::Good, 1_000);

        on_group_failure(&plan, &mut group, &deps, "boom".to_string());
        let sample = deps.store.get("tag:1").expect("still present");
        assert_eq!(sample.value, Some(12.5), "1回目は直前値を残す");
        assert_eq!(sample.quality, Quality::Stale);
        assert_eq!(sample.ptime_ms, 1_000, "Stale は時刻も据え置く");

        on_group_failure(&plan, &mut group, &deps, "boom".to_string());
        let sample = deps.store.get("tag:1").expect("still present");
        assert_eq!(sample.value, None, "2周期連続で Bad へ落とす");
        assert_eq!(sample.quality, Quality::Bad);

        assert_eq!(group.consecutive_failures, 2);
        let status = deps.status.snapshot();
        assert_eq!(status[0].groups[0].last_error.as_deref(), Some("boom"));
    }

    /// 直前値が無いタグは Stale ではなく Bad（`write_stale` の doc comment）。
    #[test]
    fn a_tag_that_never_had_a_value_goes_straight_to_bad() {
        let deps = deps();
        let plan = connection_plan();
        let mut group = group_runtime();
        on_group_failure(&plan, &mut group, &deps, "boom".to_string());
        let sample = deps.store.get("tag:1").expect("written");
        assert_eq!(sample.value, None);
        assert_eq!(sample.quality, Quality::Bad);
    }

    /// §4.3「接続断 → 接続単位で全タグ Bad」。
    #[test]
    fn mark_all_bad_covers_every_tag_of_every_group() {
        let deps = deps();
        let plan = connection_plan();
        deps.store.set("tag:1", Some(1.0), Quality::Good, 1_000);
        mark_all_bad(&plan, &deps);
        let sample = deps.store.get("tag:1").expect("written");
        assert_eq!(sample.value, None);
        assert_eq!(sample.quality, Quality::Bad);
    }

    /// Fix 2 のレビュー対応: 「プールは張れたが `poll_forever` が接続断で
    /// 抜けた」場合も「プールがそもそも張れなかった」場合も、
    /// `record_connection_failure` を通せば `consecutive_failures` が
    /// `attempt` と一致する - 以前は前者の経路（`run_connection` の
    /// `Ok(pool)` 分岐）だけこの代入を欠き、`state == Backoff` なのに
    /// `consecutive_failures == 0` のまま（`attempt` は 1 以上）という
    /// 矛盾した状態を書いていた。
    #[test]
    fn record_connection_failure_keeps_consecutive_failures_in_step_with_state() {
        let mut status = DbConnectionStatus {
            connection_id: 1,
            connection_name: "erp".to_string(),
            state: DbConnectionState::Connected,
            last_poll_at: Some(1_000),
            last_error: None,
            consecutive_failures: 0,
            groups: Vec::new(),
        };

        // 「プールは張れたが poll_forever が接続断で抜けた」経路
        // （`run_connection` の `Ok(pool)` 分岐）を模す。
        record_connection_failure(
            &mut status,
            DbConnectionState::Backoff,
            1,
            "切断されました".to_string(),
        );
        assert_eq!(status.state, DbConnectionState::Backoff);
        assert_eq!(
            status.consecutive_failures, 1,
            "attempt と揃っているはず（以前は 0 のまま取り残されていた）"
        );
        assert_eq!(status.last_error.as_deref(), Some("切断されました"));

        // 続けて再接続そのものが失敗する経路（`Err` 分岐）でも同じ関数を
        // 通るので、段数がそのまま積み上がる。
        record_connection_failure(
            &mut status,
            DbConnectionState::Backoff,
            2,
            "接続できません".to_string(),
        );
        assert_eq!(status.consecutive_failures, 2);
        assert_eq!(status.last_error.as_deref(), Some("接続できません"));
    }

    /// 「列が無い / 型が未対応」のタグは毎ティック Bad が書かれる。
    #[test]
    fn bad_tags_are_written_bad_every_tick() {
        let deps = deps();
        let bad = vec![(tag(2), TagBadReason::MissingColumn)];
        write_bad_tags(&bad, &deps, 5_000);
        let sample = deps.store.get("tag:2").expect("written");
        assert_eq!(sample.value, None);
        assert_eq!(sample.quality, Quality::Bad);
        assert_eq!(sample.ptime_ms, 5_000);
    }

    // --- レビュー対応: describe の再試行 -----------------------------------

    /// `describe_retry_interval` は `max(period, 5s)`（このモジュール doc
    /// comment の `DESCRIBE_RETRY_MIN`）。
    #[test]
    fn describe_retry_interval_is_at_least_five_seconds() {
        assert_eq!(describe_retry_interval(1_000), Duration::from_secs(5));
        assert_eq!(describe_retry_interval(5_000), Duration::from_secs(5));
        assert_eq!(describe_retry_interval(10_000), Duration::from_secs(10));
    }

    /// レート制限そのもの: 間隔内は再試行しない・間隔を過ぎたら再試行する。
    #[test]
    fn describe_retry_due_respects_the_rate_limit() {
        let now = Instant::now();
        // まだ一度も再試行していない世代は常に「試してよい」。
        assert!(describe_retry_due(None, now));

        let due = now + Duration::from_secs(5);
        assert!(!describe_retry_due(Some(due), now), "before the interval");
        assert!(
            describe_retry_due(Some(due), due),
            "exactly at the deadline"
        );
        assert!(
            describe_retry_due(Some(due), due + Duration::from_millis(1)),
            "after the interval"
        );
    }

    /// `should_adopt_redescribed`: `wrapper_sql: None` からの復帰は無条件、
    /// `Some` のままなら結果が変わったときだけ採用する。
    #[test]
    fn should_adopt_redescribed_covers_recovery_and_drift_only() {
        let broken = ResolvedGroup {
            wrapper_sql: None,
            value_tags: Vec::new(),
            bad_tags: vec![(
                tag(1),
                TagBadReason::StatementNotPrepared("boom".to_string()),
            )],
        };
        let healthy = ResolvedGroup {
            wrapper_sql: Some("SELECT 1".to_string()),
            value_tags: vec![tag(1)],
            bad_tags: Vec::new(),
        };
        // 復帰: None → Some は無条件で採用。
        assert!(should_adopt_redescribed(&broken, &healthy));
        // 同一の結果なら採用しない（黙って既存のまま）。
        assert!(!should_adopt_redescribed(&healthy, &healthy.clone()));
        // 列が変わった（同じ SQL でも型が変わった等）なら採用する。
        let drifted = ResolvedGroup {
            wrapper_sql: Some("SELECT 1, 2".to_string()),
            ..healthy.clone()
        };
        assert!(should_adopt_redescribed(&healthy, &drifted));
    }

    /// §4.3 拡張: `wrapper_sql: None` のグループが再 describe に成功すると
    /// 復帰する - `resolved` が差し替わり、`last_describe_error` が消え、
    /// `consecutive_failures` が 0 に戻り、`Recovered` を返す。
    #[test]
    fn apply_redescribe_result_recovers_a_group_whose_statement_was_broken() {
        let mut group = group_runtime();
        group.resolved = ResolvedGroup {
            wrapper_sql: None,
            value_tags: Vec::new(),
            bad_tags: vec![(
                tag(1),
                TagBadReason::StatementNotPrepared("relation does not exist".to_string()),
            )],
        };
        group.consecutive_failures = 0;
        group.last_describe_error = Some("relation does not exist".to_string());

        let recovered = ResolvedGroup {
            wrapper_sql: Some("SELECT (\"c1\")::float8 AS c0".to_string()),
            value_tags: vec![tag(1)],
            bad_tags: Vec::new(),
        };
        let effect = apply_redescribe_result(&mut group, "erp", Ok(recovered.clone()));

        assert_eq!(group.resolved, recovered);
        assert_eq!(group.consecutive_failures, 0);
        assert_eq!(group.last_describe_error, None);
        match effect {
            RedescribeEffect::Recovered {
                info_log,
                bad_tag_warnings,
            } => {
                assert!(info_log.contains("再解析して復帰"), "{info_log}");
                assert!(bad_tag_warnings.is_empty());
            }
            other => panic!("expected Recovered, got {other:?}"),
        }
    }

    /// 2周期連続で失敗したグループが再 describe しても結果が変わらなければ
    /// 黙って既存の `resolved` のまま（`should_adopt_redescribed` の doc
    /// comment）- 古い prepared wrapper のまま固定される、というレビュー
    /// 指摘の裏側（変わらない限りは差し替えない）を確認する。
    #[test]
    fn apply_redescribe_result_keeps_the_old_wrapper_when_describe_is_unchanged() {
        let mut group = group_runtime();
        group.consecutive_failures = 2;
        let before = group.resolved.clone();

        let same = before.clone();
        let effect = apply_redescribe_result(&mut group, "erp", Ok(same));

        assert_eq!(
            group.resolved, before,
            "unchanged describe keeps the wrapper"
        );
        assert_eq!(
            group.consecutive_failures, 2,
            "no reset without an actual change"
        );
        assert_eq!(effect, RedescribeEffect::NoChange);
    }

    /// Statement 失敗が続くとき: 同じメッセージなら `NoChange`
    /// （「no repeated warn spam」）、メッセージが変われば
    /// `ErrorMessageChanged` を1回だけ返す。
    #[test]
    fn apply_redescribe_result_suppresses_repeated_identical_statement_errors() {
        let mut group = group_runtime();
        group.resolved.wrapper_sql = None;
        group.last_describe_error = Some("boom".to_string());

        let unchanged = apply_redescribe_result(
            &mut group,
            "erp",
            Err(PgFailure::Statement("boom".to_string())),
        );
        assert_eq!(unchanged, RedescribeEffect::NoChange);

        let changed = apply_redescribe_result(
            &mut group,
            "erp",
            Err(PgFailure::Statement(
                "still broken, differently".to_string(),
            )),
        );
        assert_eq!(
            changed,
            RedescribeEffect::ErrorMessageChanged("still broken, differently".to_string())
        );
        assert_eq!(
            group.last_describe_error.as_deref(),
            Some("still broken, differently")
        );
    }

    /// 接続レベルの失敗はそのまま伝播する（`poll_forever` を抜ける契約）。
    #[test]
    fn apply_redescribe_result_propagates_connection_failures() {
        let mut group = group_runtime();
        let effect = apply_redescribe_result(
            &mut group,
            "erp",
            Err(PgFailure::Connection("connection reset".to_string())),
        );
        assert_eq!(
            effect,
            RedescribeEffect::ConnectionFailure("connection reset".to_string())
        );
    }
}
