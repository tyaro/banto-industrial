//! シェルの配線（#392 段階 B の B2-4c、`docs/chronogazer-service-design.md` §3.2・
//! 2026-10-14 オーナー決定 O1〜O10）。
//!
//! 起動のたびに、SCM の照会と health の probe の結果から
//! `chronogazer_core::shell_launch::decide` で 3 つの画面の 1 つを選び（判断は
//! B2-4a の純関数）、ここはその配線だけを持つ:
//!
//! - **Desktop**（[`ShellView::Desktop`]）: 今までどおりプロセスの中でエンジンを動かす
//!   （[`crate::launch_desktop`]）。起動に失敗したときも今までのエラーの窓・対処の窓
//!   （fallback にはしない）。
//! - **Service**（[`ShellView::Service`]）: サービスの画面
//!   `http://127.0.0.1:<port>/` を窓（label [`SERVICE_WINDOW_LABEL`]）で開く。窓の設定は
//!   tauri.conf.json の `main` を写し、URL を最初から外部（`WebviewUrl::External`）に
//!   する（ローカルの SPA を一度も読まない）。`AppState` は manage しない（コマンドを
//!   使わない）・capabilities は変えない（`remote.urls` を付けない。capabilities の
//!   対象は `main` だけで、この窓には何の権限も無い）。窓の設定の `transparent: true`
//!   は外す（サービスの画面は Acrylic を当てないので、透明のままだと背景が抜ける）。
//!   移動は同じ origin（`http://127.0.0.1:<port>`）の中だけ許す。× はトレイへ隠す
//!   （O5）。旧データの案内（O10）もここで出す。
//! - **Fallback**（[`ShellView::Fallback`]）: どちらもせず、SCM の状態と health の
//!   理由と対処を静的ページ（`static/service-fallback.html`、label
//!   [`FALLBACK_WINDOW_LABEL`]）で見せる。起動に失敗したときの対処の窓と同じく
//!   コマンドを呼ばない: 表示は初期化スクリプトで渡し、ボタンは
//!   `/__shell/{retry,open-logs,quit}` への移動を `on_navigation` で受け取って
//!   取り消す（[`fallback_navigation`]）。× はトレイへ隠す（O3 の自動の接続を続ける
//!   ため。**判断**: O5 を fallback の窓にも広げた）。
//!
//! 窓の種類は常に 1 つ（Service と Fallback の窓、Desktop のメインの窓が同時に
//! 並ばない）。移るときは**新しい窓を先に作ってから古い窓を消す**（最後の窓を
//! 消すとアプリが終わるため）。新しい窓は古い窓が見えていたときだけ見せる
//! （トレイに隠したまま切り替わったときに窓を勝手に出さない。利用者が「再試行」を
//! 押した Desktop への移りは常に見せる）。
//!
//! # 移り変わり（[`on_tick`]・[`on_retry`]、純関数）
//!
//! | 今の画面 | 起きたこと                                             | 次                          |
//! | -------- | ------------------------------------------------------ | --------------------------- |
//! | Service  | 見張り（5 秒ごと）: `Running` かつ同じ port で Healthy | そのまま（不健康の回数を 0 に） |
//! | Service  | 見張り: `Running` でない（停止・遷移中・未登録など）   | Fallback（O2）              |
//! | Service  | 見張り: `Running` だが不健康・照会に失敗、2 回続いた   | Fallback（O2）              |
//! | Fallback | 見張り: `Running` かつ Healthy                         | Service（O3、自動でつなぐ） |
//! | Fallback | 見張り: それ以外                                       | Fallback（表示を更新）      |
//! | Fallback | 「再試行」: `decide` が Service                        | Service                     |
//! | Fallback | 「再試行」: `decide` が Desktop（停止・未登録・照会の失敗） | Desktop（O3）          |
//! | Fallback | 「再試行」: `decide` が Fallback                       | Fallback（「まだ接続できません」） |
//! | Desktop  | （見張らない。Desktop から他へは移らない）             | -                           |
//!
//! **約束**: 見張りは**決して Desktop にしない**（O2・O3。[`Step`] に Desktop があっても
//! [`on_tick`] は返さない - 表で総当たりにテストする）。Desktop へ移るのは利用者が
//! 「再試行」を押したときだけで、そのときも `decide` の約束（遷移中・`Running` なら
//! Desktop にしない）を通る。見張りの照会の失敗は「不健康」に数える（`decide` の O1 の
//! 「照会の失敗は Desktop」は**起動と再試行のときだけ**）。サービスが別の port で
//! Healthy になった（再起動して port が変わった）ときも不健康に数え、Fallback を経て
//! 新しい port へつなぎ直す。
//!
//! # 並行（[`crate::lifecycle`]、Tauri に依存しない部分）
//!
//! 「移ってよいか・終わってよいか・何をどれだけ待つか」の判断は、すべてライフ
//! サイクルの状態機械（[`crate::lifecycle`]、1 つのロックと 1 つの表）が持つ。ここは
//! 表の判断に従って窓を作る・消すだけで、その場で別の印を読んで確かめ直さない。
//! 表の行のうちシェルに関わるもの:
//!
//! - **移り変わりは 1 つずつ**: 移る処理（見張りの Service ↔ Fallback、「再試行」、
//!   対処の窓の起動のやり直し）は札（[`TransitionGuard`]）を持って行う。札があれば
//!   「再試行」の二度押し・見張りの適用は何もしない。
//! - **飛行中の観測が新しい状態を巻き戻さない**: 見張りは観測の前に画面の世代
//!   （generation）を控え、適用するときに世代が変わっていたら捨てる
//!   （`Lifecycle::begin_transition_at`）。
//! - **終了との競合**: 終了の後始末（[`crate::exit_cleanup`]）は最初に扉を閉じ
//!   （`Lifecycle::begin_exit`。以後の移り変わり・エンジンを置くことを断る）、表が
//!   「待つ」と決めたときは走っている移り変わりの終わりを上限まで待ってから
//!   （`Lifecycle::wait_idle`）、エンジンを探す。「再試行」の [`crate::launch_desktop`] が
//!   扉が閉じた後に起動し終えたら、エンジンを置かずにその場で止める
//!   （`Lifecycle::admit`。banto-hub の `discard_started_hub` と同じ考え）。エンジンを
//!   置くこと（`DesktopEngine` の manage）と扉の確認は同じロックの中で行うので、「置いた
//!   のに後始末が見落とす」「後始末が終わった後に置く」のどちらも起きない。
//! - **待ちの上限は終了の起点で分ける**（PR #608 の全体監査 F2・F3、
//!   `crate::lifecycle::exit_budgets`）: トレイ・Fallback の「終了」（自発的な終了。専用の
//!   スレッドで待つので画面は塞がない）は、移り変わり（「再試行」の Desktop の起動）を
//!   長く（60 秒）待つ - 短く切ると、起動し終えたエンジンを止めずにプロセスが終わりうる。
//!   OS のシャットダウン（`RunEvent::Exit`、メインスレッド）は、移り変わりの待ちと
//!   後始末で **1 つの 5 秒の期限**を分け合う（OS の猶予を超えない）。さらにエンジンが
//!   既に置かれていれば移り変わりを待たない（窓を作る移り変わりはメインスレッドを
//!   待つので終わらない。エンジンは見つかるので後始末で止められる）。エンジンは
//!   `start()` の直後に置く（`crate::launch_desktop`）ので、「収集が動いているのに
//!   後始末から見えない」区間は `start()` の中だけ。
//! - **DB の復旧の途中は終了させない**（PR #608 のオーナーレビュー P2）: 対処の窓の
//!   「作り直す」「バックアップから戻す」は DB の付随ファイルと本体を別々に名前を
//!   変え、失敗したら戻す（`chronogazer_core::startup_recovery`）。途中でプロセスが
//!   終わると、現用の側と `.broken` の側にファイルが分かれた復旧が残りうる。そこで
//!   対処を受け付けた時点から起動のやり直しの終わりまで、**復旧の札**
//!   （`Lifecycle::begin_recovery`）を持つ。トレイの「終了」は復旧中なら**断る**
//!   （押した時点でも、確認の答えの時点でも。ダイアログで「復旧の途中です。終わってから
//!   終了してください」と知らせ、確認の段階を戻す）。**判断**: 5 秒で打ち切って進むのでも
//!   完了まで黙って保留するのでもなく断る - 打ち切りは今回の問題そのもので、保留は
//!   利用者から見ると「終了を押したのに何も起きない」になり、復旧が長い（大きな DB の
//!   名前の変更が他のソフトに掴まれて再試行している）と理由が分からないため。終了が
//!   確定した後は新しい復旧を始めない（対処の窓に「終了処理中」と出す）。確定と
//!   復旧の札は同じロックの中で見るので、「確かめた後に復旧が始まる」隙は無い。
//!   窓の × は対処の窓の処理中が防ぐ（同じ表の行）。OS のシャットダウン
//!   （`RunEvent::Exit`）は断れないので、移り変わりと同じく予算まで待ってから進む
//!   （既知の制限。設計書の B2-4c の実装メモ）。
//! - **見張りは 1 つだけ**: `setup()` で Service・Fallback に決まったときに 1 回だけ
//!   起こし、Desktop になる・終了に入ると抜ける。SCM の照会・probe（最大 1.5 秒）は
//!   tokio のワーカーを塞がないよう `spawn_blocking` で行う。SCM が `Running` でない
//!   間は probe しない（`needs_probe`）。
//!
//! # デバッグビルドだけの確認の口（O7）
//!
//! 実機で「Running → 接続」を確かめるため、**デバッグビルドだけ**次の環境変数を読む
//! （[`test_hooks_from`]。リリースビルドでは読む処理ごと入らない）:
//!
//! - [`TEST_SERVICE_NAME_ENV`]（`CHRONOGAZER_SHELL_TEST_SERVICE_NAME`）: SCM で照会する
//!   サービス名を差し替える（既にある別のサービスの `Running` を借りる）。
//! - [`TEST_ACCEPT_HOST_KIND_ENV`]（`CHRONOGAZER_SHELL_TEST_ACCEPT_HOST_KIND`）= `server`:
//!   `chronogazer-server` のコンソール版（`hostKind == "server"`）を Healthy に数える。
//!
//! 正式な受入は B3-1（本物のサービス）で行う。

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use banto_service_control::ScmState;
use chronogazer_core::instance_lock::ChronoGazerHost;
use chronogazer_core::paths::DataLayout;
use chronogazer_core::service_probe::{probe_service_as, PROBE_TIMEOUT};
use chronogazer_core::shell_launch::{
    decide, needs_probe, LaunchDecision, ScmObservation, ServiceHealth,
};
use serde::Serialize;
use tauri::utils::config::WindowConfig;
use tauri::{AppHandle, Manager, Url, WebviewUrl};

use crate::lifecycle::{Lifecycle, PollSnapshot, TransitionGuard};
use crate::tray::TrayMode;

/// サービスの画面の窓の label。capabilities（`main` だけ）の対象外。
pub(crate) const SERVICE_WINDOW_LABEL: &str = "service";

/// Fallback の窓の label（capabilities の対象外 = コマンドを呼べない）。
pub(crate) const FALLBACK_WINDOW_LABEL: &str = "service-fallback";

/// Fallback の窓のページ。
const FALLBACK_PAGE: &str = "service-fallback.html";

/// Fallback の窓が操作を知らせるときに移ろうとするパスの頭。
const ACTION_PATH_PREFIX: &str = "/__shell/";

/// 見張りの間隔（O2）。
pub(crate) const MONITOR_INTERVAL: Duration = Duration::from_secs(5);

/// Service の画面で、続けて何回不健康なら Fallback へ移るか（O2）。
pub(crate) const UNHEALTHY_LIMIT: u32 = 2;

/// O7: SCM で照会するサービス名を差し替える環境変数（デバッグビルドだけ）。
pub(crate) const TEST_SERVICE_NAME_ENV: &str = "CHRONOGAZER_SHELL_TEST_SERVICE_NAME";

/// O7: `server` なら `chronogazer-server` のコンソール版を Healthy に数える環境変数
/// （デバッグビルドだけ）。
pub(crate) const TEST_ACCEPT_HOST_KIND_ENV: &str = "CHRONOGAZER_SHELL_TEST_ACCEPT_HOST_KIND";

// ---------------------------------------------------------------------------
// O7 の確認の口
// ---------------------------------------------------------------------------

/// デバッグビルドだけの確認の口の値（モジュール doc「デバッグビルドだけの確認の口」）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ShellTestHooks {
    /// SCM で照会するサービス名（`None` なら本番の `ChronoGazer`）。
    service_name: Option<String>,
    /// `chronogazer-server` のコンソール版を Healthy に数える。
    accept_server_host: bool,
}

impl ShellTestHooks {
    /// 照会するサービス名。
    pub(crate) fn service_name(&self) -> &str {
        self.service_name
            .as_deref()
            .unwrap_or(chronogazer_core::shell_launch::SERVICE_NAME)
    }

    /// probe が Healthy に数える host の種類（本番は `service`）。
    pub(crate) fn expected_host(&self) -> ChronoGazerHost {
        if self.accept_server_host {
            ChronoGazerHost::Server
        } else {
            ChronoGazerHost::Service
        }
    }

    /// 口が使われているときに標準エラー出力へ出す行（使われていなければ空）。
    fn log_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        if let Some(name) = &self.service_name {
            lines.push(format!(
                "banto: [デバッグビルドの確認の口] SCM で照会するサービス名を「{name}」にします（{TEST_SERVICE_NAME_ENV}）"
            ));
        }
        if self.accept_server_host {
            lines.push(format!(
                "banto: [デバッグビルドの確認の口] chronogazer-server のコンソール版（hostKind=server）を接続先として受け入れます（{TEST_ACCEPT_HOST_KIND_ENV}）"
            ));
        }
        lines
    }
}

/// 確認の口を環境変数の読み方から組み立てる（純関数）。`debug_build` が偽なら
/// 何も読まずに既定（本番の値）を返す - リリースビルドは [`test_hooks`] が
/// `false` を渡す（読む処理ごと入らない）。値が空・知らない値は無視する。
pub(crate) fn test_hooks_from(
    debug_build: bool,
    var: impl Fn(&str) -> Option<String>,
) -> ShellTestHooks {
    if !debug_build {
        return ShellTestHooks::default();
    }
    let service_name = var(TEST_SERVICE_NAME_ENV)
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty());
    let accept_server_host = var(TEST_ACCEPT_HOST_KIND_ENV)
        .map(|kind| kind.trim() == ChronoGazerHost::Server.as_str())
        .unwrap_or(false);
    ShellTestHooks {
        service_name,
        accept_server_host,
    }
}

/// 本番の確認の口。リリースビルドでは環境変数を読まない。
fn test_hooks() -> ShellTestHooks {
    #[cfg(debug_assertions)]
    {
        test_hooks_from(true, |name| std::env::var(name).ok())
    }
    #[cfg(not(debug_assertions))]
    {
        test_hooks_from(false, |_| None)
    }
}

// ---------------------------------------------------------------------------
// 画面と移り変わり（純関数）
// ---------------------------------------------------------------------------

/// Fallback になった理由（文言を選ぶため）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FallbackCause {
    /// 起動の判定で（サービスが遷移中・不健康）。
    AtStartup,
    /// Service の画面の見張りで（「サービスが止まりました」、O2）。
    ServiceLost,
}

/// Fallback の画面に出す状態。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FallbackState {
    /// SCM の観測（見張りでは照会の失敗もありうる）。
    pub(crate) scm: ScmObservation,
    /// probe の結果（`Running` でないときは `None`）。
    pub(crate) health: Option<ServiceHealth>,
    pub(crate) cause: FallbackCause,
}

/// シェルの画面。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ShellView {
    /// プロセスの中でエンジンを動かす（起動に失敗してエラーの窓・対処の窓を出して
    /// いるときも含む）。
    Desktop,
    /// サービスの画面へつないでいる。
    Service { port: u16 },
    /// どちらもせず状態を見せている。
    Fallback(FallbackState),
}

/// 起動の判定から最初の画面を決める。
pub(crate) fn initial_view(decision: &LaunchDecision) -> ShellView {
    match decision {
        LaunchDecision::Service { port } => ShellView::Service { port: *port },
        LaunchDecision::Desktop { .. } => ShellView::Desktop,
        LaunchDecision::Fallback(info) => ShellView::Fallback(FallbackState {
            scm: ScmObservation::State(info.scm.clone()),
            health: info.health.clone(),
            cause: FallbackCause::AtStartup,
        }),
    }
}

/// 画面の移り変わり。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Step {
    /// 何もしない。
    Stay,
    /// Fallback のまま、表示を更新する。
    Refresh(FallbackState),
    /// サービスへつなぐ。
    ToService { port: u16 },
    /// Fallback へ移る。
    ToFallback(FallbackState),
    /// プロセスの中でエンジンを動かす（[`on_retry`] だけが返す）。
    ToDesktop,
}

/// 見張りの 1 回（モジュール doc の表）。`streak` は Service の画面で続けて不健康
/// だった回数で、戻り値の 2 つ目が次の値。**Desktop は決して返さない**。`health` は
/// SCM が `Running` のときだけ見る（`needs_probe`）。
pub(crate) fn on_tick(
    view: &ShellView,
    streak: u32,
    scm: &ScmObservation,
    health: Option<&ServiceHealth>,
) -> (Step, u32) {
    let health = if needs_probe(scm) { health } else { None };
    match view {
        ShellView::Desktop => (Step::Stay, 0),
        ShellView::Service { port } => {
            let running = matches!(scm, ScmObservation::State(ScmState::Running));
            let unhealthy_but_maybe_transient =
                running || matches!(scm, ScmObservation::QueryFailed(_));
            if running
                && matches!(health, Some(ServiceHealth::Healthy { port: p, .. }) if p == port)
            {
                return (Step::Stay, 0);
            }
            let fallback = FallbackState {
                scm: scm.clone(),
                health: health.cloned(),
                cause: FallbackCause::ServiceLost,
            };
            if !unhealthy_but_maybe_transient {
                // 止まった・遷移中・未登録: すぐに Fallback（O2）。
                return (Step::ToFallback(fallback), 0);
            }
            let streak = streak.saturating_add(1);
            if streak >= UNHEALTHY_LIMIT {
                (Step::ToFallback(fallback), 0)
            } else {
                (Step::Stay, streak)
            }
        }
        ShellView::Fallback(current) => {
            if let (
                ScmObservation::State(ScmState::Running),
                Some(ServiceHealth::Healthy { port, .. }),
            ) = (scm, health)
            {
                // O3: 自動でつなぐ（Desktop へは自動では移らない）。
                return (Step::ToService { port: *port }, 0);
            }
            let next = FallbackState {
                scm: scm.clone(),
                health: health.cloned(),
                cause: current.cause,
            };
            if &next == current {
                (Step::Stay, 0)
            } else {
                (Step::Refresh(next), 0)
            }
        }
    }
}

/// Fallback の「再試行」（O3）。起動と同じ `decide`（遷移中・`Running` なら Desktop に
/// しない、照会の失敗は Desktop - O1）で決める。Fallback のままなら必ず
/// [`Step::Refresh`]（「まだ接続できません」を出すため）。
pub(crate) fn on_retry(
    cause: FallbackCause,
    scm: &ScmObservation,
    health: Option<&ServiceHealth>,
) -> Step {
    match decide(scm, health) {
        LaunchDecision::Service { port } => Step::ToService { port },
        LaunchDecision::Desktop { .. } => Step::ToDesktop,
        LaunchDecision::Fallback(info) => Step::Refresh(FallbackState {
            scm: ScmObservation::State(info.scm),
            health: info.health,
            cause,
        }),
    }
}

/// トレイの表示の態（[`crate::tray::menu_spec`]）。
pub(crate) fn tray_mode(view: &ShellView) -> TrayMode {
    match view {
        ShellView::Desktop => TrayMode::Desktop,
        ShellView::Service { .. } => TrayMode::Service,
        ShellView::Fallback(state) => TrayMode::Fallback {
            scm: describe_scm(&state.scm),
        },
    }
}

// ---------------------------------------------------------------------------
// 文言（純関数）
// ---------------------------------------------------------------------------

/// SCM の状態の短い文言（トレイの「サービス: <状態>（接続できません）」と窓）。
pub(crate) fn describe_scm(scm: &ScmObservation) -> String {
    match scm {
        ScmObservation::NotWindows => "対象外（Windows ではありません）".to_string(),
        ScmObservation::QueryFailed(_) => "状態を確認できません".to_string(),
        ScmObservation::State(state) => match state {
            ScmState::NotInstalled => "未登録".to_string(),
            ScmState::Stopped => "停止".to_string(),
            ScmState::StartPending => "開始中".to_string(),
            ScmState::StopPending => "停止中".to_string(),
            ScmState::Running => "実行中".to_string(),
            ScmState::Other(raw) => format!("その他（{raw}）"),
        },
    }
}

/// SCM の状態の詳しい文言（窓。照会の失敗は理由も出す）。
fn describe_scm_detail(scm: &ScmObservation) -> String {
    match scm {
        ScmObservation::QueryFailed(reason) => format!("状態を確認できません: {reason}"),
        other => describe_scm(other),
    }
}

/// 接続先のファイル（文言の中の呼び方）。
const ENDPOINT_FILE_LABEL: &str = "データの置き場所の run\\endpoint.json";

/// 接続先が見つからないときの、権限の可能性（最後に添える。#612: データの置き場所の
/// 権限を絞った後（B3-3）のほかの利用者の場合で、まず疑う原因ではない）。
const ENDPOINT_PERMISSION_HINT: &str =
    "ほかの利用者として使っているときは、データの置き場所を読む権限が無いこともあります。";

/// health の probe の結果の文言（窓）。`None` は probe していない（SCM が `Running` で
/// ない。理由は `scm` で言い分け、照会の失敗・未登録を「実行中ではない」と断定しない）。
/// `EndpointMissing` は `scm` に合わせて原因の並べ方を変え、権限の可能性は最後に回す
/// （#612。コンソール版を止めた・サービスの起動や停止の途中のほうがずっと多い）。
pub(crate) fn describe_health(scm: &ScmObservation, health: Option<&ServiceHealth>) -> String {
    match health {
        None => match scm {
            ScmObservation::QueryFailed(_) => {
                "サービスの状態を確認できないため、接続は確かめていません。".to_string()
            }
            ScmObservation::State(ScmState::NotInstalled) => {
                "サービスが登録されていないため、接続は確かめていません。".to_string()
            }
            ScmObservation::NotWindows => {
                "Windows ではないため、接続は確かめていません。".to_string()
            }
            ScmObservation::State(ScmState::Running) => {
                "接続はまだ確かめていません。".to_string()
            }
            ScmObservation::State(_) => {
                "サービスが実行中ではないため、接続は確かめていません。".to_string()
            }
        },
        Some(ServiceHealth::Healthy { port, version }) => {
            format!("接続できます（ポート {port}、版 {version}）。")
        }
        Some(ServiceHealth::EndpointMissing) => match scm {
            ScmObservation::State(ScmState::Running) => format!(
                "サービスの接続先（{ENDPOINT_FILE_LABEL}）が見つからないか、書いたプロセスが終わっています。\
                 サービスが起動か停止の途中のことが多く、そのときは少し待つと自動でつながります。\
                 続くときは、サービスが別のデータの置き場所を使っているか、接続先を書けなかった\
                 （理由はサービスのログ）可能性があります。{ENDPOINT_PERMISSION_HINT}"
            ),
            ScmObservation::State(ScmState::StartPending) => format!(
                "サービスが開始の途中のため、接続先（{ENDPOINT_FILE_LABEL}）はまだありません。"
            ),
            ScmObservation::State(ScmState::StopPending | ScmState::Stopped) => format!(
                "サービスが止まっている（停止の途中を含む）ため、接続先（{ENDPOINT_FILE_LABEL}）はありません。"
            ),
            _ => format!(
                "サービスの接続先（{ENDPOINT_FILE_LABEL}）が見つからないか、書いたプロセスが終わっています。\
                 {ENDPOINT_PERMISSION_HINT}"
            ),
        },
        Some(ServiceHealth::Unreachable { port }) => {
            format!("サービスのポート {port} に接続できません。")
        }
        Some(ServiceHealth::NotChronoGazer { port }) => {
            format!("ポート {port} で応答したのは ChronoGazer のサービスではありません。")
        }
        Some(ServiceHealth::WrongHostKind { port, kind }) => format!(
            "ポート {port} で応答した ChronoGazer はサービスではありません（種類: {kind}）。"
        ),
        Some(ServiceHealth::WrongScope { port }) => format!(
            "ポート {port} の ChronoGazer のサービスは、このデータの置き場所とは別のデータを使っています。"
        ),
        Some(ServiceHealth::ProbeFailed(reason)) => {
            format!("サービスへの接続の確認に失敗しました: {reason}")
        }
    }
}

/// Fallback の窓に渡す表示（`window.__CHRONOGAZER_SHELL_FALLBACK__`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FallbackPageView {
    pub(crate) title: String,
    pub(crate) message: String,
    pub(crate) scm: String,
    pub(crate) health: String,
    pub(crate) steps: Vec<String>,
    pub(crate) logs_path: String,
    pub(crate) notice: Option<String>,
    pub(crate) busy: bool,
}

impl FallbackPageView {
    /// ページを読む前に走らせる JS。JSON はそのまま JS の式として有効で、ページ側は
    /// `textContent` で入れる（パスがどんな文字でも HTML として解釈されない）。
    fn initialization_script(&self) -> String {
        let json = serde_json::to_string(self).expect("FallbackPageView serializes");
        format!("window.__CHRONOGAZER_SHELL_FALLBACK__ = {json};")
    }

    fn render_script(&self) -> String {
        let json = serde_json::to_string(self).expect("FallbackPageView serializes");
        format!(
            "window.__chronogazerShellFallback && window.__chronogazerShellFallback.render({json});"
        )
    }
}

/// Fallback の窓の表示を組む（純関数）。
pub(crate) fn fallback_page_view(
    state: &FallbackState,
    logs_dir: &Path,
    notice: Option<String>,
    busy: bool,
) -> FallbackPageView {
    let title = match state.cause {
        FallbackCause::ServiceLost => "サービスが止まりました",
        FallbackCause::AtStartup => "記録エンジン（サービス）に接続できません",
    };
    let message = match (state.cause, &state.scm) {
        (FallbackCause::ServiceLost, _) => {
            "ChronoGazer の記録エンジン（サービス）に接続できなくなったため、サービスの画面を閉じました。"
        }
        (_, ScmObservation::State(ScmState::StartPending)) => {
            "ChronoGazer の記録エンジン（サービス）が開始の途中です。"
        }
        (_, ScmObservation::State(ScmState::StopPending)) => {
            "ChronoGazer の記録エンジン（サービス）が停止の途中です。"
        }
        (_, ScmObservation::State(ScmState::Running)) => {
            "ChronoGazer の記録エンジン（サービス）は実行中ですが、接続を確かめられませんでした。"
        }
        _ => "ChronoGazer の記録エンジン（サービス）は、接続できる状態ではありません。",
    };
    FallbackPageView {
        title: title.to_string(),
        message: message.to_string(),
        scm: describe_scm_detail(&state.scm),
        health: describe_health(&state.scm, state.health.as_ref()),
        steps: vec![
            format!(
                "サービスが実行中になり接続を確かめられると、自動でつなぎます（{} 秒ごとに確かめています）。",
                MONITOR_INTERVAL.as_secs()
            ),
            concat!(
                "サービスを使わずにこの PC の中で記録するときは、サービスを止めてから「再試行」を押してください。",
                "サービスが止まっていれば、この PC の中で記録エンジンを起動します",
                "（サービスが実行中・開始や停止の途中の間は、同じデータを二重に開かないよう起動しません）。"
            )
            .to_string(),
            "原因はサービスのログ（下のフォルダ）で確かめられます。".to_string(),
        ],
        logs_path: logs_dir.display().to_string(),
        notice,
        busy,
    }
}

/// 「再試行」で Fallback のままだったときの知らせ。
fn still_unavailable_notice(state: &FallbackState) -> String {
    format!(
        "まだ接続できません（サービス: {}）。",
        describe_scm(&state.scm)
    )
}

// ---------------------------------------------------------------------------
// 窓の URL と移動（純関数）
// ---------------------------------------------------------------------------

/// Fallback の窓のボタン。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShellAction {
    Retry,
    OpenLogs,
    Quit,
}

/// Fallback の窓が移ろうとした先の扱い。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FallbackNavigation {
    /// Fallback のページそのもの（読み込みを許す）。
    Page,
    /// ボタン（移動は取り消す）。
    Action(ShellAction),
    /// それ以外（取り消す）。
    Deny,
}

/// Fallback の窓の移動の判定（純関数）。
pub(crate) fn fallback_navigation(url: &Url) -> FallbackNavigation {
    let path = url.path();
    if let Some(name) = path.strip_prefix(ACTION_PATH_PREFIX) {
        return match name {
            "retry" => FallbackNavigation::Action(ShellAction::Retry),
            "open-logs" => FallbackNavigation::Action(ShellAction::OpenLogs),
            "quit" => FallbackNavigation::Action(ShellAction::Quit),
            _ => FallbackNavigation::Deny,
        };
    }
    if path == format!("/{FALLBACK_PAGE}") {
        FallbackNavigation::Page
    } else {
        FallbackNavigation::Deny
    }
}

/// サービスの画面の URL（loopback の IPv4 固定。IPv6 は対象外）。
pub(crate) fn service_url(port: u16) -> Url {
    Url::parse(&format!("http://127.0.0.1:{port}/")).expect("loopback URL parses")
}

/// サービスの画面の窓で許す移動（同じ origin の中だけ）。
pub(crate) fn service_navigation_allowed(url: &Url, port: u16) -> bool {
    url.scheme() == "http"
        && url.host_str() == Some("127.0.0.1")
        && url.port_or_known_default() == Some(port)
}

/// サービスの画面の窓の設定（`main` の設定を写して、label・URL・透明・見せるかを
/// 差し替える。純関数）。
pub(crate) fn service_window_config(base: &WindowConfig, port: u16, visible: bool) -> WindowConfig {
    let mut config = base.clone();
    config.label = SERVICE_WINDOW_LABEL.to_string();
    config.url = WebviewUrl::External(service_url(port));
    config.transparent = false;
    config.visible = visible;
    config.create = false;
    config
}

// ---------------------------------------------------------------------------
// Tauri の配線
// ---------------------------------------------------------------------------

/// シェルの状態（`setup()` が manage する）。画面・札・終了の判断は
/// [`crate::lifecycle::Lifecycle`]（`run()` が manage する）が持つ。
pub(crate) struct ShellState {
    pub(crate) layout: DataLayout,
    hooks: ShellTestHooks,
}

/// ライフサイクルの状態機械（`run()` が manage する。無ければ `None`）。
pub(crate) fn lifecycle(app: &AppHandle) -> Option<Arc<Lifecycle>> {
    app.try_state::<Arc<Lifecycle>>()
        .map(|lifecycle| Arc::clone(&lifecycle))
}

/// 画面からトレイの態を決める（画面が決まる前は Desktop）。
pub(crate) fn tray_mode_of(view: Option<&ShellView>) -> TrayMode {
    view.map(tray_mode).unwrap_or(TrayMode::Desktop)
}

/// 今のトレイの態（画面が決まっていなければ Desktop）。
pub(crate) fn current_tray_mode(app: &AppHandle) -> TrayMode {
    tray_mode_of(
        lifecycle(app)
            .and_then(|lifecycle| lifecycle.view())
            .as_ref(),
    )
}

/// `launch_desktop` が続けてよいか（終了の扉が閉じていれば偽）。
pub(crate) fn launch_may_proceed(app: &AppHandle) -> bool {
    lifecycle(app)
        .map(|lifecycle| lifecycle.launch_may_proceed())
        .unwrap_or(true)
}

/// SCM を照会する（Windows だけ。それ以外は [`ScmObservation::NotWindows`]）。
fn observe_scm(hooks: &ShellTestHooks) -> ScmObservation {
    #[cfg(windows)]
    {
        use banto_service_control::WindowsServiceManager;
        use chronogazer_core::shell_launch::{SERVICE_DISPLAY_NAME, SERVICE_NAME};
        // `for_service` は `&'static str` を取る。確認の口の名前は起動のたびに 1 回
        // 決まり、プロセスの終わりまで使うので、そのときだけ 1 つ確保したまま持つ。
        static NAME: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();
        let name: &'static str = match hooks.service_name() {
            SERVICE_NAME => SERVICE_NAME,
            custom => NAME.get_or_init(|| Box::leak(custom.to_string().into_boxed_str())),
        };
        let manager = WindowsServiceManager::for_service(
            name,
            SERVICE_DISPLAY_NAME,
            std::env::current_exe().unwrap_or_default(),
            Vec::new(),
        );
        chronogazer_core::shell_launch::observe_scm(&manager)
    }
    #[cfg(not(windows))]
    {
        let _ = hooks;
        ScmObservation::NotWindows
    }
}

/// SCM を照会し、`Running` なら probe する（同期。最大 [`PROBE_TIMEOUT`] + 照会）。
fn observe(layout: &DataLayout, hooks: &ShellTestHooks) -> (ScmObservation, Option<ServiceHealth>) {
    let scm = observe_scm(hooks);
    let health =
        needs_probe(&scm).then(|| probe_service_as(layout, hooks.expected_host(), PROBE_TIMEOUT));
    (scm, health)
}

/// [`observe`] を tokio のワーカーの外で行う。
async fn observe_async(app: &AppHandle) -> Option<(ScmObservation, Option<ServiceHealth>)> {
    let (layout, hooks) = {
        let shell = app.try_state::<ShellState>()?;
        (shell.layout.clone(), shell.hooks.clone())
    };
    match tauri::async_runtime::spawn_blocking(move || observe(&layout, &hooks)).await {
        Ok(observed) => Some(observed),
        Err(err) => {
            eprintln!("banto: サービスの状態の確認に失敗しました: {err}");
            None
        }
    }
}

/// `setup()` から: ルートの配置を決め、SCM と health を見て、3 つの画面の 1 つで
/// 始める。
pub(crate) fn start(app: &AppHandle) {
    // データのルートの配置（作らない）。使えないルートは「終了」だけの窓。
    let layout = crate::startup::layout_from_env();
    if let Err(detail) = crate::startup::check_root(&layout) {
        crate::startup::show_fatal(app, detail);
        return;
    }
    let hooks = test_hooks();
    for line in hooks.log_lines() {
        eprintln!("{line}");
    }
    let (scm, health) = observe(&layout, &hooks);
    let decision = decide(&scm, health.as_ref());
    eprintln!(
        "banto: 起動の判定: サービス={}、判定={}",
        describe_scm_detail(&scm),
        match &decision {
            LaunchDecision::Service { port } => format!("サービスへつなぐ（ポート {port}）"),
            LaunchDecision::Desktop { .. } => "この PC の中で記録エンジンを起動".to_string(),
            LaunchDecision::Fallback(_) => "接続できない状態を表示".to_string(),
        }
    );
    let view = initial_view(&decision);
    if let Some(lifecycle) = lifecycle(app) {
        lifecycle.start(view.clone());
    }
    app.manage(ShellState {
        layout: layout.clone(),
        hooks,
    });

    match view {
        ShellView::Desktop => start_desktop(app, layout),
        ShellView::Service { port } => {
            let tray_ok = install_tray_logged(app);
            if let Err(err) = open_service_window(app, port, true, tray_ok) {
                crate::startup::show_fatal(app, format!("画面を作れませんでした: {err}"));
                return;
            }
            // #612: 見張りの自動のつなぎ直し（`switch_to_service`）と同じ 1 行を、窓を
            // 作れた後に 1 回だけ（作れなければ出さない）。画面の読み込みや認証の成功は
            // 意味しない（つなぎ直しの行と同じ意味）。
            eprintln!("{}", connected_to_service_line(port));
            crate::legacy_notice::show_once(app, &layout);
            tauri::async_runtime::spawn(monitor(app.clone()));
        }
        ShellView::Fallback(state) => {
            let tray_ok = install_tray_logged(app);
            let page = fallback_page_view(&state, &layout.logs_dir(), None, false);
            if let Err(err) = open_fallback_window(app, &page, true, tray_ok) {
                crate::startup::show_fatal(app, format!("画面を作れませんでした: {err}"));
                return;
            }
            tauri::async_runtime::spawn(monitor(app.clone()));
        }
    }
}

/// 起動の判定が Desktop のとき（今までの起動）。
fn start_desktop(app: &AppHandle, layout: DataLayout) {
    let paths = match crate::startup::LaunchPaths::prepare(layout) {
        Ok(paths) => paths,
        Err(detail) => {
            crate::startup::show_fatal(app, detail);
            return;
        }
    };
    // `setup()` のメインスレッド（runtime の外）なので `block_on` してよい。
    tauri::async_runtime::block_on(async {
        if let Err(err) = crate::launch_desktop(app, &paths, crate::startup::Prelude::None).await {
            crate::startup::show_failure(app, &paths, err).await;
        }
    });
}

/// トレイを作る（失敗は記録して `false`。窓の × はそのとき隠さずに閉じる - 戻す
/// 手段が無くなるため）。
fn install_tray_logged(app: &AppHandle) -> bool {
    match crate::tray::install(app) {
        Ok(()) => true,
        Err(err) => {
            eprintln!("banto: トレイを作れませんでした（窓の × で終了します）: {err}");
            false
        }
    }
}

/// サービスの画面の窓を作る。
fn open_service_window(
    app: &AppHandle,
    port: u16,
    visible: bool,
    hide_on_close: bool,
) -> tauri::Result<()> {
    let base = app
        .config()
        .app
        .windows
        .iter()
        .find(|window| window.label == crate::MAIN_WINDOW_LABEL)
        .cloned()
        .ok_or(tauri::Error::WindowNotFound)?;
    let config = service_window_config(&base, port, visible);
    let window = tauri::WebviewWindowBuilder::from_config(app, &config)?
        .on_navigation(move |url| service_navigation_allowed(url, port))
        .build()?;
    if hide_on_close {
        banto_desktop_tray::hide_to_tray_on_close(&window, crate::tray::SERVICE_TRAY_HINT);
    }
    Ok(())
}

/// Fallback の窓を作る（既にあれば描き直す）。
fn open_fallback_window(
    app: &AppHandle,
    page: &FallbackPageView,
    visible: bool,
    hide_on_close: bool,
) -> tauri::Result<()> {
    if let Some(window) = app.get_webview_window(FALLBACK_WINDOW_LABEL) {
        window.eval(page.render_script())?;
        if visible {
            let _ = window.show();
        }
        return Ok(());
    }
    let navigation_app = app.clone();
    let window = tauri::WebviewWindowBuilder::new(
        app,
        FALLBACK_WINDOW_LABEL,
        WebviewUrl::App(FALLBACK_PAGE.into()),
    )
    .title("ChronoGazer")
    .inner_size(680.0, 560.0)
    .resizable(true)
    .center()
    .visible(visible)
    .initialization_script(page.initialization_script())
    .on_navigation(move |url| match fallback_navigation(url) {
        FallbackNavigation::Page => true,
        FallbackNavigation::Action(action) => {
            handle_action(&navigation_app, action);
            false
        }
        FallbackNavigation::Deny => false,
    })
    .build()?;
    if hide_on_close {
        banto_desktop_tray::hide_to_tray_on_close(&window, crate::tray::FALLBACK_TRAY_HINT);
    }
    Ok(())
}

/// Fallback の窓を、今の画面の状態で描き直す（Fallback でなければ何もしない）。
fn rerender_fallback(app: &AppHandle, notice: Option<String>, busy: bool) {
    let Some(shell) = app.try_state::<ShellState>() else {
        return;
    };
    let Some(ShellView::Fallback(state)) = lifecycle(app).and_then(|lifecycle| lifecycle.view())
    else {
        return;
    };
    let page = fallback_page_view(&state, &shell.layout.logs_dir(), notice, busy);
    if let Some(window) = app.get_webview_window(FALLBACK_WINDOW_LABEL) {
        if let Err(err) = window.eval(page.render_script()) {
            eprintln!("banto: 接続できない状態の窓を更新できませんでした: {err}");
        }
    }
}

/// 窓が見えているか（無ければ偽）。
fn window_visible(app: &AppHandle, label: &str) -> bool {
    app.get_webview_window(label)
        .and_then(|window| window.is_visible().ok())
        .unwrap_or(false)
}

fn destroy_window(app: &AppHandle, label: &str) {
    if let Some(window) = app.get_webview_window(label) {
        if let Err(err) = window.destroy() {
            eprintln!("banto: 窓（{label}）を閉じられませんでした: {err}");
        }
    }
}

/// Fallback の窓のボタン。
fn handle_action(app: &AppHandle, action: ShellAction) {
    match action {
        ShellAction::Retry => request_retry(app),
        ShellAction::OpenLogs => open_logs(app),
        ShellAction::Quit => crate::tray::request_quit(app),
    }
}

/// ログのフォルダ（`<root>\logs`、`chronogazer-server` が書く）を開く。無ければ
/// 窓に知らせる。
fn open_logs(app: &AppHandle) {
    let Some(shell) = app.try_state::<ShellState>() else {
        return;
    };
    let logs = shell.layout.logs_dir();
    let notice = if !logs.is_dir() {
        Some(format!(
            "ログのフォルダ（{}）がありません（サービスがまだ一度も起動していないか、この利用者に読む権限がありません）。",
            logs.display()
        ))
    } else if open_folder(&logs) {
        None
    } else {
        Some(format!(
            "ログのフォルダを開けませんでした。手で開いてください: {}",
            logs.display()
        ))
    };
    if notice.is_some() {
        rerender_fallback(app, notice, false);
    }
}

/// フォルダをエクスプローラーで開く（Windows だけ。開けたら `true`）。
fn open_folder(path: &Path) -> bool {
    #[cfg(windows)]
    {
        std::process::Command::new("explorer")
            .arg(path)
            .spawn()
            .is_ok()
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        false
    }
}

/// 「再試行」（Fallback の窓のボタン・トレイ）。
pub(crate) fn request_retry(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move { retry(app).await });
}

async fn retry(app: AppHandle) {
    if app.try_state::<ShellState>().is_none() {
        return;
    }
    let Some(lifecycle) = lifecycle(&app) else {
        return;
    };
    // 二度押し・見張りの移り変わりの最中・終了の後は何もしない（窓が「確かめて
    // います」のまま残らないよう、今の状態で描き直す）。古いメニューから Fallback で
    // ない画面で押されたときも何もしない（`lifecycle` の表の「再試行」の行）。
    let (guard, current) = match lifecycle.begin_retry() {
        Ok(granted) => granted,
        Err(crate::lifecycle::Refusal::NotFallback) => return,
        Err(_) => {
            rerender_fallback(&app, None, false);
            return;
        }
    };
    rerender_fallback(&app, None, true);
    let Some((scm, health)) = observe_async(&app).await else {
        drop(guard);
        rerender_fallback(&app, None, false);
        return;
    };
    match on_retry(current.cause, &scm, health.as_ref()) {
        Step::ToService { port } => switch_to_service(&app, guard, port),
        Step::ToDesktop => switch_to_desktop(&app, guard).await,
        Step::Refresh(state) => {
            let notice = still_unavailable_notice(&state);
            guard.finish(ShellView::Fallback(state));
            rerender_fallback(&app, Some(notice), false);
            crate::tray::refresh(&app);
        }
        Step::Stay | Step::ToFallback(_) => {
            drop(guard);
            rerender_fallback(&app, None, false);
        }
    }
}

/// サービスの画面の窓を作れたときのログの 1 行（起動の判定・見張りの自動のつなぎ直し・
/// 「再試行」で共通。#612）。窓を作れたことだけを表し、画面の読み込みや認証の成功は
/// 意味しない。
pub(crate) fn connected_to_service_line(port: u16) -> String {
    format!("banto: サービスへつなぎました（ポート {port}）")
}

/// Fallback → Service（見張りの自動の接続・「再試行」）。
fn switch_to_service(app: &AppHandle, guard: TransitionGuard, port: u16) {
    let visible = window_visible(app, FALLBACK_WINDOW_LABEL);
    let hide_on_close = crate::tray::installed(app);
    if let Err(err) = open_service_window(app, port, visible, hide_on_close) {
        // 窓を作れない: Fallback のまま（次の見張りでもう一度試す）。
        eprintln!("banto: サービスの画面の窓を作れませんでした: {err}");
        drop(guard);
        rerender_fallback(app, None, false);
        return;
    }
    destroy_window(app, FALLBACK_WINDOW_LABEL);
    guard.finish(ShellView::Service { port });
    crate::tray::refresh(app);
    eprintln!("{}", connected_to_service_line(port));
    if let Some(shell) = app.try_state::<ShellState>() {
        crate::legacy_notice::show_once(app, &shell.layout);
    }
}

/// Service → Fallback（見張り、O2）。
fn switch_to_fallback(app: &AppHandle, guard: TransitionGuard, state: FallbackState) {
    let Some(shell) = app.try_state::<ShellState>() else {
        return;
    };
    let visible = window_visible(app, SERVICE_WINDOW_LABEL);
    let page = fallback_page_view(&state, &shell.layout.logs_dir(), None, false);
    let hide_on_close = crate::tray::installed(app);
    if let Err(err) = open_fallback_window(app, &page, visible, hide_on_close) {
        // 窓を作れない: Service のまま（次の見張りでもう一度試す）。
        eprintln!("banto: 接続できない状態の窓を作れませんでした: {err}");
        drop(guard);
        return;
    }
    destroy_window(app, SERVICE_WINDOW_LABEL);
    eprintln!(
        "banto: サービスに接続できなくなりました（サービス: {}）",
        describe_scm_detail(&state.scm)
    );
    guard.finish(ShellView::Fallback(state));
    crate::tray::refresh(app);
}

/// Fallback → Desktop（「再試行」だけ、O3）。起動に失敗したら今までのエラーの窓・
/// 対処の窓（fallback には戻さない）。
async fn switch_to_desktop(app: &AppHandle, guard: TransitionGuard) {
    let layout = match app.try_state::<ShellState>() {
        Some(shell) => shell.layout.clone(),
        None => return,
    };
    let paths = match crate::startup::LaunchPaths::prepare(layout) {
        Ok(paths) => paths,
        Err(detail) => {
            crate::startup::show_fatal(app, detail);
            destroy_window(app, FALLBACK_WINDOW_LABEL);
            guard.finish(ShellView::Desktop);
            crate::tray::refresh(app);
            return;
        }
    };
    match crate::launch_desktop(app, &paths, crate::startup::Prelude::None).await {
        Ok(()) => {
            destroy_window(app, FALLBACK_WINDOW_LABEL);
            guard.finish(ShellView::Desktop);
        }
        // 終了に入った: エンジンは `launch_desktop` が止めた。窓はそのまま（プロセスが
        // 終わる）。
        Err(crate::startup::LaunchError::Exiting) => {
            drop(guard);
            return;
        }
        Err(err) => {
            crate::startup::show_failure(app, &paths, err).await;
            destroy_window(app, FALLBACK_WINDOW_LABEL);
            guard.finish(ShellView::Desktop);
        }
    }
    crate::tray::refresh(app);
}

/// 見張り（モジュール doc「並行」）。`setup()` から 1 回だけ起こす。
async fn monitor(app: AppHandle) {
    loop {
        tokio::time::sleep(MONITOR_INTERVAL).await;
        if app.try_state::<ShellState>().is_none() {
            break;
        }
        let Some(gate) = lifecycle(&app) else {
            break;
        };
        let (view, generation, streak) = match gate.poll() {
            PollSnapshot::Stop => break,
            PollSnapshot::Busy => continue,
            PollSnapshot::Ready {
                view,
                generation,
                streak,
            } => (view, generation, streak),
        };
        let Some((scm, health)) = observe_async(&app).await else {
            continue;
        };
        let (step, streak) = on_tick(&view, streak, &scm, health.as_ref());
        match step {
            Step::Stay => gate.record_streak(generation, streak),
            Step::Refresh(state) => {
                if let Ok(guard) = gate.begin_transition_at(generation) {
                    guard.finish(ShellView::Fallback(state));
                    rerender_fallback(&app, None, false);
                    crate::tray::refresh(&app);
                }
            }
            Step::ToService { port } => {
                if let Ok(guard) = gate.begin_transition_at(generation) {
                    switch_to_service(&app, guard, port);
                }
            }
            Step::ToFallback(state) => {
                if let Ok(guard) = gate.begin_transition_at(generation) {
                    switch_to_fallback(&app, guard, state);
                }
            }
            // `on_tick` は返さない（O2・O3）。来ても何もしない。
            Step::ToDesktop => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn all_observations() -> Vec<ScmObservation> {
        vec![
            ScmObservation::NotWindows,
            ScmObservation::QueryFailed("サービス操作に失敗しました: x".to_string()),
            ScmObservation::State(ScmState::NotInstalled),
            ScmObservation::State(ScmState::Stopped),
            ScmObservation::State(ScmState::StartPending),
            ScmObservation::State(ScmState::StopPending),
            ScmObservation::State(ScmState::Running),
            ScmObservation::State(ScmState::Other("Paused".to_string())),
        ]
    }

    fn all_health() -> Vec<Option<ServiceHealth>> {
        vec![
            None,
            Some(ServiceHealth::Healthy {
                port: 8721,
                version: "0.1.0".to_string(),
            }),
            Some(ServiceHealth::Healthy {
                port: 9000,
                version: "0.1.0".to_string(),
            }),
            Some(ServiceHealth::EndpointMissing),
            Some(ServiceHealth::Unreachable { port: 8721 }),
            Some(ServiceHealth::NotChronoGazer { port: 8721 }),
            Some(ServiceHealth::WrongHostKind {
                port: 8721,
                kind: "server".to_string(),
            }),
            Some(ServiceHealth::WrongScope { port: 8721 }),
            Some(ServiceHealth::ProbeFailed("timed out".to_string())),
        ]
    }

    fn fallback(scm: ScmObservation, cause: FallbackCause) -> FallbackState {
        FallbackState {
            scm,
            health: None,
            cause,
        }
    }

    fn all_views() -> Vec<ShellView> {
        vec![
            ShellView::Desktop,
            ShellView::Service { port: 8721 },
            ShellView::Fallback(fallback(
                ScmObservation::State(ScmState::StartPending),
                FallbackCause::AtStartup,
            )),
            ShellView::Fallback(fallback(
                ScmObservation::State(ScmState::Stopped),
                FallbackCause::ServiceLost,
            )),
        ]
    }

    fn running() -> ScmObservation {
        ScmObservation::State(ScmState::Running)
    }

    fn healthy(port: u16) -> ServiceHealth {
        ServiceHealth::Healthy {
            port,
            version: "0.1.0".to_string(),
        }
    }

    /// 約束（O2・O3）: 見張りは、どの画面・どの観測・どの回数でも Desktop にしない。
    #[test]
    fn ticks_never_go_desktop() {
        for view in all_views() {
            for scm in all_observations() {
                for health in all_health() {
                    for streak in 0..=UNHEALTHY_LIMIT {
                        let (step, _) = on_tick(&view, streak, &scm, health.as_ref());
                        assert_ne!(
                            step,
                            Step::ToDesktop,
                            "view={view:?} scm={scm:?} health={health:?} streak={streak}"
                        );
                    }
                }
            }
        }
    }

    /// Service の画面の見張りの表（O2）。
    #[test]
    fn service_monitor_table() {
        let view = ShellView::Service { port: 8721 };
        for scm in all_observations() {
            for health in all_health() {
                for streak in 0..UNHEALTHY_LIMIT {
                    let (step, next) = on_tick(&view, streak, &scm, health.as_ref());
                    let is_running = scm == running();
                    let considered = if is_running { health.clone() } else { None };
                    let healthy_same_port = is_running && considered == Some(healthy(8721));
                    let transient = is_running || matches!(scm, ScmObservation::QueryFailed(_));
                    let lost = FallbackState {
                        scm: scm.clone(),
                        health: considered.clone(),
                        cause: FallbackCause::ServiceLost,
                    };
                    let expected = if healthy_same_port {
                        (Step::Stay, 0)
                    } else if !transient || streak + 1 >= UNHEALTHY_LIMIT {
                        (Step::ToFallback(lost), 0)
                    } else {
                        (Step::Stay, streak + 1)
                    };
                    assert_eq!(
                        (step, next),
                        expected,
                        "scm={scm:?} health={health:?} streak={streak}"
                    );
                }
            }
        }
    }

    /// 2 回続けて不健康で Fallback、間に健康が挟まれば数え直す。
    #[test]
    fn two_consecutive_unhealthy_ticks_fall_back() {
        let view = ShellView::Service { port: 8721 };
        let bad = ServiceHealth::Unreachable { port: 8721 };
        let (step, streak) = on_tick(&view, 0, &running(), Some(&bad));
        assert_eq!((step, streak), (Step::Stay, 1));
        let (step, streak) = on_tick(&view, streak, &running(), Some(&healthy(8721)));
        assert_eq!((step, streak), (Step::Stay, 0));
        let (_, streak) = on_tick(&view, streak, &running(), Some(&bad));
        let (step, _) = on_tick(&view, streak, &running(), Some(&bad));
        assert!(matches!(step, Step::ToFallback(_)), "{step:?}");
        // 止まったら 1 回目ですぐ。
        let (step, _) = on_tick(&view, 0, &ScmObservation::State(ScmState::Stopped), None);
        assert!(matches!(step, Step::ToFallback(_)), "{step:?}");
    }

    /// Fallback の見張りの表（O3）: `Running` かつ Healthy なら自動でつなぎ、それ以外は
    /// 表示を変えるだけ（変わらなければ何もしない）。
    #[test]
    fn fallback_monitor_table() {
        for view in all_views() {
            let ShellView::Fallback(current) = &view else {
                continue;
            };
            for scm in all_observations() {
                for health in all_health() {
                    let (step, next) = on_tick(&view, 1, &scm, health.as_ref());
                    assert_eq!(next, 0);
                    let considered = if scm == running() {
                        health.clone()
                    } else {
                        None
                    };
                    let expected = match (&scm, &considered) {
                        (s, Some(ServiceHealth::Healthy { port, .. })) if *s == running() => {
                            Step::ToService { port: *port }
                        }
                        _ => {
                            let state = FallbackState {
                                scm: scm.clone(),
                                health: considered.clone(),
                                cause: current.cause,
                            };
                            if &state == current {
                                Step::Stay
                            } else {
                                Step::Refresh(state)
                            }
                        }
                    };
                    assert_eq!(
                        step, expected,
                        "view={view:?} scm={scm:?} health={health:?}"
                    );
                }
            }
        }
    }

    /// Desktop は見張らない。
    #[test]
    fn desktop_view_stays() {
        for scm in all_observations() {
            for health in all_health() {
                assert_eq!(
                    on_tick(&ShellView::Desktop, 1, &scm, health.as_ref()),
                    (Step::Stay, 0)
                );
            }
        }
    }

    /// 「再試行」（O3）: Desktop へ移るのは `decide` が Desktop のとき（未登録・停止・
    /// 照会の失敗・Windows でない）だけ。遷移中・`Running` は Desktop にしない。
    #[test]
    fn retry_follows_decide() {
        for scm in all_observations() {
            for health in all_health() {
                let step = on_retry(FallbackCause::ServiceLost, &scm, health.as_ref());
                let desktop_allowed = matches!(
                    scm,
                    ScmObservation::NotWindows
                        | ScmObservation::QueryFailed(_)
                        | ScmObservation::State(ScmState::NotInstalled | ScmState::Stopped)
                );
                assert_eq!(
                    step == Step::ToDesktop,
                    desktop_allowed,
                    "scm={scm:?} health={health:?} -> {step:?}"
                );
                if scm == running() {
                    match &health {
                        Some(ServiceHealth::Healthy { port, .. }) => {
                            assert_eq!(step, Step::ToService { port: *port })
                        }
                        _ => assert!(matches!(step, Step::Refresh(_)), "{step:?}"),
                    }
                }
                if let Step::Refresh(state) = &step {
                    assert_eq!(state.cause, FallbackCause::ServiceLost);
                }
            }
        }
    }

    /// 最初の画面は判定のとおり（Fallback は起動の理由で）。
    #[test]
    fn initial_view_follows_decision() {
        assert_eq!(
            initial_view(&LaunchDecision::Service { port: 1 }),
            ShellView::Service { port: 1 }
        );
        assert_eq!(
            initial_view(&LaunchDecision::Desktop { scm: None }),
            ShellView::Desktop
        );
        let view = initial_view(&LaunchDecision::Fallback(
            chronogazer_core::shell_launch::FallbackInfo {
                scm: ScmState::StartPending,
                health: None,
            },
        ));
        assert_eq!(
            view,
            ShellView::Fallback(fallback(
                ScmObservation::State(ScmState::StartPending),
                FallbackCause::AtStartup
            ))
        );
    }

    /// O7: デバッグビルドだけ読む。値は前後の空白を落とし、空・知らない値は無視する。
    #[test]
    fn test_hooks_are_read_only_in_debug_builds() {
        let vars = |name: &str| match name {
            TEST_SERVICE_NAME_ENV => Some("  Spooler ".to_string()),
            TEST_ACCEPT_HOST_KIND_ENV => Some("server".to_string()),
            _ => None,
        };
        let debug = test_hooks_from(true, vars);
        assert_eq!(debug.service_name(), "Spooler");
        assert_eq!(debug.expected_host(), ChronoGazerHost::Server);
        assert_eq!(debug.log_lines().len(), 2);

        // リリースビルド（`debug_build = false`）は同じ環境でも本番の値。
        let release = test_hooks_from(false, vars);
        assert_eq!(release, ShellTestHooks::default());
        assert_eq!(release.service_name(), "ChronoGazer");
        assert_eq!(release.expected_host(), ChronoGazerHost::Service);
        assert!(release.log_lines().is_empty());

        // 空・知らない値。
        let odd = test_hooks_from(true, |name: &str| match name {
            TEST_SERVICE_NAME_ENV => Some("   ".to_string()),
            TEST_ACCEPT_HOST_KIND_ENV => Some("desktop".to_string()),
            _ => None,
        });
        assert_eq!(odd, ShellTestHooks::default());
    }

    /// 本番の `test_hooks` はビルドのプロファイルに従う: デバッグは口を読み、リリースは
    /// `#[cfg(not(debug_assertions))]` の側で何も読まない（`cargo test --release` で、
    /// 口の環境変数を置いたまま回して確かめる）。
    #[test]
    fn production_hooks_follow_build_profile() {
        // 今のビルドの期待値: デバッグなら環境変数から組み立てた値、リリースなら
        // 環境に関係なく既定（PR #608 の Copilot レビュー: どちらのプロファイルでも
        // テストがコンパイルでき、リリースの側の振る舞いも確かめる）。
        let expected = if cfg!(debug_assertions) {
            test_hooks_from(true, |name| std::env::var(name).ok())
        } else {
            ShellTestHooks::default()
        };
        assert_eq!(test_hooks(), expected);
    }

    /// Fallback の窓のボタンの受け付け: 知っている 3 つだけ。ページそのものは許し、
    /// それ以外は取り消す。
    #[test]
    fn fallback_navigation_accepts_only_known_actions() {
        let url = |s: &str| Url::parse(s).unwrap();
        for base in ["http://tauri.localhost", "http://localhost:1420"] {
            assert_eq!(
                fallback_navigation(&url(&format!("{base}/__shell/retry"))),
                FallbackNavigation::Action(ShellAction::Retry)
            );
            assert_eq!(
                fallback_navigation(&url(&format!("{base}/__shell/open-logs"))),
                FallbackNavigation::Action(ShellAction::OpenLogs)
            );
            assert_eq!(
                fallback_navigation(&url(&format!("{base}/__shell/quit?x=1"))),
                FallbackNavigation::Action(ShellAction::Quit)
            );
            for denied in [
                "/__shell/",
                "/__shell/retry/",
                "/__shell/start-service",
                "/__recovery/quit",
                "/",
                "/index.html",
            ] {
                assert_eq!(
                    fallback_navigation(&url(&format!("{base}{denied}"))),
                    FallbackNavigation::Deny,
                    "{denied}"
                );
            }
            assert_eq!(
                fallback_navigation(&url(&format!("{base}/service-fallback.html"))),
                FallbackNavigation::Page
            );
        }
    }

    /// サービスの画面の窓は同じ origin の中だけ移れる。
    #[test]
    fn service_window_stays_on_its_origin() {
        let url = |s: &str| Url::parse(s).unwrap();
        assert_eq!(service_url(8721).as_str(), "http://127.0.0.1:8721/");
        assert!(service_navigation_allowed(
            &url("http://127.0.0.1:8721/"),
            8721
        ));
        assert!(service_navigation_allowed(
            &url("http://127.0.0.1:8721/monitor?x=1#y"),
            8721
        ));
        for denied in [
            "http://127.0.0.1:8722/",
            "https://127.0.0.1:8721/",
            "http://localhost:8721/",
            "http://192.0.2.1:8721/",
            "http://example.com/",
            "http://tauri.localhost/index.html",
        ] {
            assert!(!service_navigation_allowed(&url(denied), 8721), "{denied}");
        }
    }

    /// 窓の設定は `main` を写し、label・URL・透明・見せるかだけ差し替える。
    #[test]
    fn service_window_config_overrides_only_what_it_must() {
        let base = WindowConfig {
            label: "main".to_string(),
            title: "ChronoGazer".to_string(),
            width: 1280.0,
            height: 800.0,
            transparent: true,
            ..WindowConfig::default()
        };
        let config = service_window_config(&base, 8721, false);
        assert_eq!(config.label, SERVICE_WINDOW_LABEL);
        assert_eq!(config.url, WebviewUrl::External(service_url(8721)));
        assert!(!config.transparent);
        assert!(!config.visible);
        assert_eq!(config.title, "ChronoGazer");
        assert_eq!((config.width, config.height), (1280.0, 800.0));
        assert!(service_window_config(&base, 8721, true).visible);
    }

    /// 文言: すべての SCM × health の理由に中身があり、port を名指す理由は port を含む。
    #[test]
    fn health_descriptions() {
        for scm in all_observations() {
            for health in all_health() {
                let text = describe_health(&scm, health.as_ref());
                assert!(!text.is_empty());
                if let Some(
                    ServiceHealth::Healthy { port, .. }
                    | ServiceHealth::Unreachable { port }
                    | ServiceHealth::NotChronoGazer { port }
                    | ServiceHealth::WrongHostKind { port, .. }
                    | ServiceHealth::WrongScope { port },
                ) = &health
                {
                    assert!(text.contains(&port.to_string()), "{text}");
                }
                if health == Some(ServiceHealth::EndpointMissing) {
                    assert!(text.contains("endpoint.json"), "{scm:?}: {text}");
                }
            }
        }
        assert!(describe_health(
            &running(),
            Some(&ServiceHealth::WrongHostKind {
                port: 1,
                kind: "server".to_string()
            })
        )
        .contains("server"));
        assert!(
            describe_health(&ScmObservation::State(ScmState::Stopped), None)
                .contains("確かめていません")
        );
    }

    /// #612: 接続先が見つからない（`EndpointMissing`）ときは、SCM の状態に合わせて原因を
    /// 並べ、権限の可能性は断定せず最後に回す（実行中ではまず起動・停止の途中を挙げる）。
    /// 照会の失敗・未登録で probe していないときは「実行中ではない」と断定しない。
    #[test]
    fn endpoint_missing_puts_the_permission_hint_last_and_never_asserts_it() {
        let missing = Some(&ServiceHealth::EndpointMissing);
        let text = describe_health(&running(), missing);
        let transient = text.find("起動か停止の途中").expect(&text);
        let permission = text.find("権限").expect(&text);
        assert!(transient < permission, "{text}");
        assert!(text.ends_with(ENDPOINT_PERMISSION_HINT), "{text}");
        assert!(!text.contains("権限がありません"), "断定している: {text}");

        for scm in [
            ScmObservation::State(ScmState::StartPending),
            ScmObservation::State(ScmState::StopPending),
            ScmObservation::State(ScmState::Stopped),
        ] {
            let text = describe_health(&scm, missing);
            assert!(!text.contains("権限"), "{scm:?}: {text}");
        }
        for scm in all_observations() {
            assert!(
                !describe_health(&scm, missing).contains("権限がありません"),
                "{scm:?}"
            );
        }

        let query_failed = describe_health(&ScmObservation::QueryFailed("x".to_string()), None);
        assert!(!query_failed.contains("実行中ではない"), "{query_failed}");
        let not_installed = describe_health(&ScmObservation::State(ScmState::NotInstalled), None);
        assert!(!not_installed.contains("実行中ではない"), "{not_installed}");
    }

    /// #612: 起動の判定でサービスの画面を開いたときも、見張りのつなぎ直しと同じ 1 行。
    #[test]
    fn the_connected_line_names_the_port() {
        assert_eq!(
            connected_to_service_line(8721),
            "banto: サービスへつなぎました（ポート 8721）"
        );
    }

    #[test]
    fn scm_descriptions() {
        assert_eq!(describe_scm(&running()), "実行中");
        assert_eq!(
            describe_scm(&ScmObservation::State(ScmState::StartPending)),
            "開始中"
        );
        assert_eq!(
            describe_scm(&ScmObservation::State(ScmState::Other("Paused".into()))),
            "その他（Paused）"
        );
        let failed = ScmObservation::QueryFailed("サービス操作の権限がありません".into());
        assert_eq!(describe_scm(&failed), "状態を確認できません");
        assert!(describe_scm_detail(&failed).contains("権限がありません"));
    }

    /// Fallback の窓の表示: 理由ごとの見出し、SCM と health、ログのフォルダ、再試行の
    /// 意味（止めてから押すと Desktop）、知らせ。
    #[test]
    fn fallback_page_view_texts() {
        let logs = PathBuf::from(r"C:\ProgramData\ChronoGazer\logs");
        let lost = FallbackState {
            scm: ScmObservation::State(ScmState::Stopped),
            health: None,
            cause: FallbackCause::ServiceLost,
        };
        let view = fallback_page_view(&lost, &logs, Some("x".into()), true);
        assert_eq!(view.title, "サービスが止まりました");
        assert_eq!(view.scm, "停止");
        assert!(view.logs_path.contains("logs"));
        assert_eq!(view.notice.as_deref(), Some("x"));
        assert!(view.busy);
        assert!(view.steps.iter().any(|s| s.contains("再試行")));
        assert!(view.steps.iter().any(|s| s.contains("5 秒")));

        let pending = FallbackState {
            scm: ScmObservation::State(ScmState::StartPending),
            health: None,
            cause: FallbackCause::AtStartup,
        };
        let view = fallback_page_view(&pending, &logs, None, false);
        assert!(view.message.contains("開始の途中"), "{}", view.message);
        assert_ne!(view.title, "サービスが止まりました");

        let unhealthy = FallbackState {
            scm: running(),
            health: Some(ServiceHealth::Unreachable { port: 8721 }),
            cause: FallbackCause::AtStartup,
        };
        let view = fallback_page_view(&unhealthy, &logs, None, false);
        assert!(view.message.contains("実行中"), "{}", view.message);
        assert!(view.health.contains("8721"), "{}", view.health);
        assert!(still_unavailable_notice(&unhealthy).contains("実行中"));

        // 初期化スクリプトと描き直しは JSON（textContent で入れる）を渡す。
        let script = view.initialization_script();
        assert!(script.starts_with("window.__CHRONOGAZER_SHELL_FALLBACK__ = {"));
        assert!(view
            .render_script()
            .contains("window.__chronogazerShellFallback.render({"));
    }

    /// トレイの態は画面から決まる。
    #[test]
    fn tray_mode_follows_view() {
        assert_eq!(tray_mode(&ShellView::Desktop), TrayMode::Desktop);
        assert_eq!(
            tray_mode(&ShellView::Service { port: 1 }),
            TrayMode::Service
        );
        assert_eq!(
            tray_mode(&ShellView::Fallback(fallback(
                ScmObservation::State(ScmState::StartPending),
                FallbackCause::AtStartup
            ))),
            TrayMode::Fallback {
                scm: "開始中".to_string()
            }
        );
    }
}
