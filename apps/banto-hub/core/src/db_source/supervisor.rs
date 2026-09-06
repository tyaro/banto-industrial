//! 1 DB 接続タスクの supervisor（docs/banto-hub-external-db-design.md §4.8・
//! §6-17、2026-09-06 オーナー決定「DB task の異常終了は supervisor で
//! バックオフ付き再生成する」）。
//!
//! ## なぜ要るのか
//!
//! S2 の初版は 1 接続 = `tokio::spawn(run_connection(..))` の1本きりで、
//! その task が panic するとその接続は**次の設定変更まで死んだまま**に
//! なっていた（`run_connection` は終わらないループなので、正常終了も
//! 同じく異常事態である）。PLC 側は `banto-broker` のセッション監督が
//! 同じ役目を果たしているので、DB 側にも対称の仕組みを置く。
//!
//! ## 構造
//!
//! [`supervise`] は「内側の task を spawn する → その `JoinHandle` を
//! await する → 異常終了ならバックオフして作り直す」という外側の task。
//! 内側の future は**呼び出し側が渡すファクトリ**（`impl Fn() -> Fut`）
//! から毎回作り直す - こうしてあるので、
//! `crate::db_source::task::run_connection` に**テスト用の仕掛けを一切
//! 入れずに**、「N 回 panic してから park する」偽の future で supervisor
//! 自体を単体テストできる（このファイル末尾の `tests`）。
//!
//! ## 取り消し（abort を再生成と取り違えない）
//!
//! 収集停止・plan 差し替え・シャットダウンによる停止は「異常終了」では
//! ないので、再生成してはいけない。そのため停止は2系統ある:
//!
//! - **cancel チャネル**（`tokio::sync::watch<bool>`）: [`supervise`] は
//!   内側の join もバックオフの sleep も `tokio::select!` で cancel と
//!   一緒に待つ。cancel を受けたら内側を `abort` して**join してから**
//!   戻るので、呼び出し側が supervisor を join し終えた時点で内側の task
//!   （＝ `PgPool`）は確実に消えている。
//!   [`crate::db_source::DbSourceEngine::stop`]/`shutdown` はこちらを使う。
//! - **supervisor 自身の `abort`**: [`crate::db_source::DbSourceEngine::commit`]
//!   は同期メソッドなので join できず、`JoinHandle::abort` で落とす。
//!   このとき内側を取り残さないよう、内側の `JoinHandle` は
//!   [`AbortOnDrop`] ガードに入れてある - supervisor の future が
//!   （await 点で取り消されて）drop されると、そのガードの `Drop` が
//!   内側を `abort` する。
//!
//! **どちらの経路でも「内側の task が supervisor より長生きすることは
//! ない」**: 内側の `JoinHandle` を持っているのは supervisor だけで、
//! それはガードの中にしか無い。

use std::future::Future;
use std::time::Duration;

use tokio::sync::watch;
use tokio::task::{JoinError, JoinHandle};
// **`std::time::Instant` ではなく `tokio::time::Instant`**: 「内側が
// どれだけ動いたか」（[`next_restart_count`] の判定）は tokio の時計で
// 測る - そうしておけば `#[tokio::test(start_paused = true)]` の仮想時計
// でバックオフと reset の両方を実時間ゼロで検証できる（このモジュール
// 末尾の `tests`）。`crate::db_source::task` が `std::time::Instant` を
// 使っているのはポーリングのデッドライン計算で、あちらは実 DB が要る
// ので仮想時計の恩恵が無いという違い。
use tokio::time::Instant;

use super::status::{DbConnectionState, DbSourceStatusStore};
use crate::diag_log::DiagLog;

use std::sync::Arc;

/// 内側の task が**この時間以上**動いてから死んだら、連続再起動回数を
/// 1 に戻す（§4.8「バックオフ付きで再 spawn」の運用感 - 半日に1回落ちる
/// task に 30 秒の待ちを引きずらせない）。
const RESTART_COUNT_RESET_AFTER: Duration = Duration::from_secs(60);

/// [`supervise`] が状態表・ログへ書くために要るもの。
pub(crate) struct SupervisorCtx {
    pub connection_id: i64,
    pub connection_name: String,
    pub status: Arc<DbSourceStatusStore>,
    pub log: DiagLog,
}

/// 内側の task がどう終わったか。`Cancelled` 以外はすべて**異常**で、
/// 再生成の対象になる（`run_connection` は戻らない契約なので、
/// `Ok(())` すら異常である - このモジュール doc comment「なぜ要るのか」）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InnerExit {
    /// panic した（`JoinError::is_panic`）。文字列はペイロードから
    /// 取り出したメッセージ（このクレートのリテラルだけで、資格情報は
    /// 含まない - [`DbConnectionStatus::last_restart_reason`] の doc
    /// comment）。
    ///
    /// [`DbConnectionStatus::last_restart_reason`]:
    ///     super::status::DbConnectionStatus::last_restart_reason
    Panicked(String),
    /// 戻ってはいけないのに戻った。
    ReturnedUnexpectedly,
    /// abort された = 意図した停止。再生成しない。
    Cancelled,
}

impl InnerExit {
    /// 状態表の `last_restart_reason` とログに出す1行分の理由。
    /// [`InnerExit::Cancelled`] は再生成しないので理由も持たない。
    pub(crate) fn reason(&self) -> Option<String> {
        match self {
            InnerExit::Panicked(message) => Some(format!("task が panic しました: {message}")),
            InnerExit::ReturnedUnexpectedly => {
                Some("task が予期せず終了しました（本来は戻らないループ）".to_string())
            }
            InnerExit::Cancelled => None,
        }
    }
}

/// `JoinHandle` の結果を [`InnerExit`] へ分類する純関数。
pub(crate) fn classify_inner_exit(joined: Result<(), JoinError>) -> InnerExit {
    match joined {
        Ok(()) => InnerExit::ReturnedUnexpectedly,
        Err(err) if err.is_cancelled() => InnerExit::Cancelled,
        Err(err) => {
            // `is_panic()` が残る唯一のケース。ペイロードは `&str`/`String`
            // のときだけ取り出す（それ以外は型の名前すら出さない）。
            let message = match err.try_into_panic() {
                Ok(payload) => panic_message(&*payload),
                // 防御的分岐: `is_cancelled` でも `is_panic` でもない
                // `JoinError` は tokio に存在しない。
                Err(err) => err.to_string(),
            };
            InnerExit::Panicked(message)
        }
    }
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&'static str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "詳細不明".to_string()
    }
}

/// 次の連続再起動回数（= バックオフ段数）を返す純関数。`ran_for` が
/// [`RESTART_COUNT_RESET_AFTER`] 以上なら「一度は安定して動いていた」と
/// みなして 1 から数え直す。
pub(crate) fn next_restart_count(previous: u32, ran_for: Duration) -> u32 {
    if ran_for >= RESTART_COUNT_RESET_AFTER {
        1
    } else {
        previous.saturating_add(1)
    }
}

/// 内側の `JoinHandle` を必ず `abort` するガード（このモジュール doc
/// comment「取り消し」節）。supervisor の future が await 点で取り消されて
/// も、内側の task がプロセスに残らないことをここで担保する。
struct AbortOnDrop(JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// このモジュール doc comment の「構造」節そのもの。**cancel を受けるまで
/// 戻らない**（内側が何度死んでも作り直す）。
///
/// `factory` は「内側の task にする future」を毎回新しく作る - 呼び出し側
/// （[`crate::db_source::DbSourceEngine`]）は
/// `move || run_connection(plan.clone(), deps.clone())` を渡す。
pub(crate) async fn supervise<F, Fut>(
    factory: F,
    ctx: SupervisorCtx,
    mut cancel: watch::Receiver<bool>,
) where
    F: Fn() -> Fut + Send,
    Fut: Future<Output = ()> + Send + 'static,
{
    let mut consecutive_restarts: u32 = 0;

    loop {
        let mut guard = AbortOnDrop(tokio::spawn(factory()));
        let started = Instant::now();

        let joined = {
            let inner = &mut guard.0;
            tokio::select! {
                biased;
                _ = cancel.changed() => None,
                joined = inner => Some(joined),
            }
        };

        let Some(joined) = joined else {
            // 取り消し（収集停止 / plan 差し替え / シャットダウン）:
            // 内側を止め、**join してから**戻る - 呼び出し側が supervisor を
            // join し終えた時点で `PgPool` が確実に落ちているようにする
            // （このモジュール doc comment「取り消し」節）。
            guard.0.abort();
            let _ = (&mut guard.0).await;
            return;
        };
        // 内側は既に終わっているのでガードの `abort` は無害な no-op。
        drop(guard);

        let exit = classify_inner_exit(joined);
        let Some(reason) = exit.reason() else {
            // `InnerExit::Cancelled`: 我々以外にこの `JoinHandle` を持つ者は
            // 居ないので、通常は到達しない（ランタイム自体の停止時のみ）。
            // 意図した停止として扱い、再生成しない。
            return;
        };

        consecutive_restarts = next_restart_count(consecutive_restarts, started.elapsed());
        let delay = super::task::backoff_delay(consecutive_restarts);

        ctx.status.update_connection(ctx.connection_id, |status| {
            status.restarts = status.restarts.saturating_add(1);
            status.last_restart_reason = Some(reason.clone());
            // task が死んでいる間は接続もタスクも実在しないので、それまで
            // `connected` のままだった状態表示を `backoff` に落とす（さもな
            // いと `GET /api/status` がバックオフ待ちの間も `connected` を
            // 報告してしまう - Copilot 指摘、PR #303）。内側が再接続に成功
            // すれば `run_connection` が `Connected` へ進め、`last_error` も
            // 消す（`crate::db_source::task`）。`consecutive_failures` は
            // 「DB へ繋がらない」という別の失敗種別のカウンタなので、ここ
            // では触らない - タスクの panic/異常終了と DB 接続失敗は別物。
            status.state = DbConnectionState::Backoff;
            status.last_error = Some(reason.clone());
        });
        // §4.8「状態 API に `restarts` 回数と最後の理由を出す」。ログは
        // 1回の異常終了につき1行だけ（`crate::db_source::task` の
        // 「ログの抑制」節と同じ抑制の考え方 - こちらは事象自体が稀なので
        // 段階変化の判定は要らない）。
        ctx.log.err_line(&format!(
            "banto-hub: [WARN] DB Source: 接続 {} のタスクが異常終了しました（{reason}）。{} 秒後に再起動します（連続 {consecutive_restarts} 回目）",
            ctx.connection_name,
            delay.as_secs_f64(),
        ));

        let cancelled = tokio::select! {
            biased;
            _ = cancel.changed() => true,
            _ = tokio::time::sleep(delay) => false,
        };
        if cancelled {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    fn ctx(status: Arc<DbSourceStatusStore>) -> SupervisorCtx {
        SupervisorCtx {
            connection_id: 1,
            connection_name: "erp".to_string(),
            status,
            log: DiagLog::new(|_| {}, |_| {}),
        }
    }

    /// 状態表に接続 1 の行を1つ持つストア（`update_connection` は存在
    /// しない id を静かに無視するので、行が無いと `restarts` を観測でき
    /// ない）。
    fn seeded_status() -> Arc<DbSourceStatusStore> {
        use crate::db_source::plan::{DbConnectionPlan, DbSourcePlan};
        let store = Arc::new(DbSourceStatusStore::new());
        store.apply_plan(
            &DbSourcePlan {
                connections: vec![DbConnectionPlan {
                    connection_id: 1,
                    connection_name: "erp".to_string(),
                    host: "127.0.0.1".to_string(),
                    port: 1,
                    database: "d".to_string(),
                    username: "u".to_string(),
                    password: "p".to_string(),
                    groups: Vec::new(),
                }],
                disabled: Vec::new(),
            },
            &std::collections::HashSet::new(),
            true,
        );
        store
    }

    fn restarts(status: &DbSourceStatusStore) -> u32 {
        status.snapshot()[0].restarts
    }

    fn state(status: &DbSourceStatusStore) -> DbConnectionState {
        status.snapshot()[0].state
    }

    /// §6-17「supervisor でバックオフ付き再生成」: 内側が panic するたび
    /// 作り直され、待ち時間は `backoff_delay`（1s → 2s → 4s …）どおり。
    ///
    /// `start_paused` の仮想時計なので実時間は待たない - 全 task が
    /// タイマー待ちになった時点で tokio が時計を進める。内側の panic は
    /// テスト出力に panic メッセージを出すが、テスト自体は成功する
    /// （`tokio::spawn` した task の panic は `JoinHandle` へ伝わるだけで
    /// プロセスを落とさない）。
    #[tokio::test(start_paused = true)]
    async fn a_panicking_task_is_respawned_with_the_documented_backoff() {
        let status = seeded_status();
        // 落ちる前は `connected` だったことにする - Copilot 指摘（PR #303）の
        // 再現条件そのもの: バックオフに入ってもここが `connected` のまま
        // だと `GET /api/status` が「task も DB セッションも無いのに
        // connected」と嘘をつく。
        status.update_connection(1, |s| s.state = DbConnectionState::Connected);
        let calls = Arc::new(AtomicU32::new(0));
        let (cancel_tx, cancel_rx) = watch::channel(false);

        let factory_calls = calls.clone();
        let handle = tokio::spawn(supervise(
            move || {
                let calls = factory_calls.clone();
                async move {
                    let nth = calls.fetch_add(1, Ordering::SeqCst) + 1;
                    if nth <= 3 {
                        panic!("boom #{nth}");
                    }
                    std::future::pending::<()>().await;
                }
            },
            ctx(status.clone()),
            cancel_rx,
        ));

        // t=0.5s: 1回目が panic し、1秒のバックオフ中。
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(restarts(&status), 1);
        // バックオフ待機中は `backoff` へ落ち、`last_error` に再起動理由が
        // 出る - task も DB セッションも実在しないので `connected` を報告
        // し続けてはいけない（Copilot 指摘、PR #303）。内側がまだ一度も
        // 繋ぎ直せていないので `Connected` はここでは絶対に出ない。
        assert_eq!(state(&status), DbConnectionState::Backoff);
        let last_error = status.snapshot()[0]
            .last_error
            .clone()
            .expect("backoff must record the restart reason as last_error");
        assert!(last_error.contains("boom #1"), "{last_error}");
        assert_eq!(
            Some(last_error),
            status.snapshot()[0].last_restart_reason.clone(),
            "last_error mirrors last_restart_reason while backing off"
        );

        // t=2.5s: 1s 後に2回目が走って panic し、いまは 2 秒のバックオフ中。
        tokio::time::sleep(Duration::from_millis(2_000)).await;
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(restarts(&status), 2);

        // t=6.5s: 3s に3回目 → panic → 4 秒のバックオフ中。
        tokio::time::sleep(Duration::from_millis(4_000)).await;
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        assert_eq!(restarts(&status), 3);

        // t=7.5s: 4回目が走り、今度は park するので以後増えない。
        tokio::time::sleep(Duration::from_millis(1_000)).await;
        assert_eq!(calls.load(Ordering::SeqCst), 4);
        tokio::time::sleep(Duration::from_secs(120)).await;
        assert_eq!(calls.load(Ordering::SeqCst), 4);
        assert_eq!(restarts(&status), 3);

        let reason = status.snapshot()[0]
            .last_restart_reason
            .clone()
            .expect("a restart must record its reason");
        assert!(reason.contains("panic"), "{reason}");
        assert!(reason.contains("boom #3"), "{reason}");
        // task の再起動と DB 接続の成否は別カウンタ - ここでは一度も DB へ
        // 接続を試みていない（偽の future が panic するだけ）ので、
        // `consecutive_failures` は触られていないはず。
        assert_eq!(status.snapshot()[0].consecutive_failures, 0);
        // 4回目は park する = 内側がまだ「繋がった」を報告していないので、
        // supervisor が `state` を `Connected` に進めることは一切ない
        // （`run_connection` が実際に繋がったときだけ `Connected` にする -
        // このモジュール doc comment、supervisor 自身は状態を Connected に
        // 進める権限を持たない）。
        assert_eq!(state(&status), DbConnectionState::Backoff);

        cancel_tx.send_replace(true);
        handle
            .await
            .expect("the supervisor exits cleanly on cancel");
    }

    /// §4.8「abort による正常停止は再生成しない」。supervisor 自身を
    /// `abort` した場合も、[`AbortOnDrop`] が内側を道連れにする
    /// （このモジュール doc comment「取り消し」節）。
    #[tokio::test(start_paused = true)]
    async fn aborting_the_supervisor_kills_the_inner_task_and_does_not_respawn() {
        let status = seeded_status();
        let calls = Arc::new(AtomicU32::new(0));
        let inner_dropped = Arc::new(AtomicBool::new(false));
        let (_cancel_tx, cancel_rx) = watch::channel(false);

        /// 内側の future が drop された（= abort が効いた）ことを観測する
        /// ための小道具。
        struct DropFlag(Arc<AtomicBool>);
        impl Drop for DropFlag {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        let factory_calls = calls.clone();
        let factory_flag = inner_dropped.clone();
        let handle = tokio::spawn(supervise(
            move || {
                let calls = factory_calls.clone();
                let flag = factory_flag.clone();
                async move {
                    let _guard = DropFlag(flag);
                    calls.fetch_add(1, Ordering::SeqCst);
                    std::future::pending::<()>().await;
                }
            },
            ctx(status.clone()),
            cancel_rx,
        ));

        // 内側が1回起動するまで待つ。
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(!inner_dropped.load(Ordering::SeqCst));

        handle.abort();
        let joined = handle.await;
        assert!(joined.unwrap_err().is_cancelled());

        // abort は「次にスケジュールされたとき」に効くので、内側が実際に
        // drop されるまで少しだけ待つ（仮想時計なので実時間は進まない）。
        for _ in 0..50 {
            if inner_dropped.load(Ordering::SeqCst) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            inner_dropped.load(Ordering::SeqCst),
            "the inner task must not outlive its supervisor"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1, "abort must not respawn");
        assert_eq!(restarts(&status), 0, "abort is not a restart");
    }

    /// cancel チャネル経由の停止も再生成しない。cancel を受けた
    /// supervisor は内側を join してから戻るので、`handle.await` が
    /// 返った時点で内側は確実に消えている（このモジュール doc comment
    /// 「取り消し」節）。
    #[tokio::test(start_paused = true)]
    async fn cancelling_the_supervisor_joins_the_inner_task_before_returning() {
        let status = seeded_status();
        let inner_dropped = Arc::new(AtomicBool::new(false));
        let (cancel_tx, cancel_rx) = watch::channel(false);

        struct DropFlag(Arc<AtomicBool>);
        impl Drop for DropFlag {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        let factory_flag = inner_dropped.clone();
        let handle = tokio::spawn(supervise(
            move || {
                let flag = factory_flag.clone();
                async move {
                    let _guard = DropFlag(flag);
                    std::future::pending::<()>().await;
                }
            },
            ctx(status.clone()),
            cancel_rx,
        ));
        tokio::time::sleep(Duration::from_millis(100)).await;

        cancel_tx.send_replace(true);
        handle
            .await
            .expect("the supervisor exits cleanly on cancel");
        assert!(
            inner_dropped.load(Ordering::SeqCst),
            "the inner task must be joined before the supervisor returns"
        );
        assert_eq!(restarts(&status), 0);
    }

    /// §4.8: 一定時間（[`RESTART_COUNT_RESET_AFTER`]）以上動いてから死んだ
    /// task は「連続」扱いしない - バックオフは 1 秒から数え直す。
    #[tokio::test(start_paused = true)]
    async fn a_long_lived_task_resets_the_consecutive_restart_count() {
        let status = seeded_status();
        let calls = Arc::new(AtomicU32::new(0));
        let (cancel_tx, cancel_rx) = watch::channel(false);

        let factory_calls = calls.clone();
        let handle = tokio::spawn(supervise(
            move || {
                let calls = factory_calls.clone();
                async move {
                    let nth = calls.fetch_add(1, Ordering::SeqCst) + 1;
                    if nth == 1 {
                        // 十分に長く動いてから死ぬ（reset の閾値超え）。
                        tokio::time::sleep(Duration::from_secs(120)).await;
                        panic!("late boom");
                    }
                    std::future::pending::<()>().await;
                }
            },
            ctx(status.clone()),
            cancel_rx,
        ));

        // t=120.5s: 1回目が死んだ直後。連続回数は 1 に戻るのでバックオフは
        // 1 秒（2 秒ではない）。
        tokio::time::sleep(Duration::from_millis(120_500)).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(restarts(&status), 1);

        // t=121.5s: 1 秒後に作り直されている。
        tokio::time::sleep(Duration::from_millis(1_000)).await;
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        cancel_tx.send_replace(true);
        handle
            .await
            .expect("the supervisor exits cleanly on cancel");
    }

    /// [`next_restart_count`] の純関数としての契約。
    #[test]
    fn next_restart_count_resets_only_after_a_long_run() {
        assert_eq!(next_restart_count(0, Duration::from_millis(1)), 1);
        assert_eq!(next_restart_count(1, Duration::from_secs(59)), 2);
        assert_eq!(next_restart_count(5, Duration::from_secs(59)), 6);
        assert_eq!(next_restart_count(5, RESTART_COUNT_RESET_AFTER), 1);
        assert_eq!(next_restart_count(5, Duration::from_secs(3_600)), 1);
        assert_eq!(next_restart_count(u32::MAX, Duration::ZERO), u32::MAX);
    }

    /// [`classify_inner_exit`]: panic / 正常終了 / abort の3分岐。
    #[tokio::test]
    async fn classify_inner_exit_separates_panic_from_cancel_and_normal_return() {
        let panicked = tokio::spawn(async { panic!("kaboom") }).await;
        match classify_inner_exit(panicked) {
            InnerExit::Panicked(message) => assert_eq!(message, "kaboom"),
            other => panic!("expected Panicked, got {other:?}"),
        }

        let handle = tokio::spawn(std::future::pending::<()>());
        handle.abort();
        assert_eq!(classify_inner_exit(handle.await), InnerExit::Cancelled);

        let returned = tokio::spawn(async {}).await;
        assert_eq!(
            classify_inner_exit(returned),
            InnerExit::ReturnedUnexpectedly
        );

        // 理由の文言（`Cancelled` だけは理由を持たない = 再生成しない）。
        assert!(InnerExit::Panicked("x".to_string())
            .reason()
            .expect("panic has a reason")
            .contains("panic"));
        assert!(InnerExit::ReturnedUnexpectedly.reason().is_some());
        assert_eq!(InnerExit::Cancelled.reason(), None);
    }
}
