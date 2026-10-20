//! T5-1（docs/tag-server-design.md §8「常駐」・docs/t5-handoff.md §3）:
//! banto-hub を Windows サービスとして常駐実行するための SCM 連携。
//! `windows-service`クレート（本実装）を使う - #392 B3-1 以降は直接ではなく、
//! 共有 crate `banto-service-control` のランナー経由（下の「共有ランナー」）。
//!
//! **このファイル全体が Windows 専用** - `bin/banto-hub.rs`側で
//! `#[cfg(windows)] mod win_service;`としてしか読み込まれないので、
//! 非 Windows（このワークスペースの CI 含む）ビルドにはこのファイルの
//! コードは一切含まれない。
//!
//! ## 3つのエントリポイント
//!
//! - [`install`][]: サービス登録（`bin/banto-hub.rs`の`install`サブコマンド、
//!   人間が管理者権限の PowerShell から直接叩く想定）。T17-2 スライス2で
//!   本体を`banto_hub_core::service_install::install`へ移設し、ここは薄い
//!   委譲になった（下記参照）。
//! - [`uninstall`][]: サービス登録解除（同`uninstall`サブコマンド、同じく
//!   `banto_hub_core::service_install::uninstall`への委譲）
//! - [`run_service_dispatcher`][]: SCM がサービス開始時に呼ぶ内部
//!   エントリポイント（同`run-service`サブコマンド - installで登録した
//!   起動引数そのもの。人間が直接叩く想定ではない）
//!
//! ## 共有ランナー（#392 B3-1、2026-10-15）
//!
//! SCM とのやりとり（`define_windows_service!`・ディスパッチャ・制御ハンドラ・
//! STOP と PRESHUTDOWN・Running → StopPending（heartbeat）→ Stopped の報告・
//! 本体の panic）は共有 crate `banto-service-control` のランナーへ移した
//! （banto-hub-sink と同じもの。ChronoGazer も B3-2 で使う）。このファイルに
//! 残るのは本体[`HubProgram`]（ログの開き方、Hub の起動と停止、終了コード
//! 1/2/3）だけ。ログを開いてから制御ハンドラを登録する順序、Running を
//! 報告する時機（Configured 収集が Running になった後）、終了コードは変えて
//! いない。#330 の`StoppedGate`は消した: 本体は`Stopped`を報告する口を
//! 持たず、終了コードを**返す**ので、起動失敗の後に`Win32(0)`で上書きする
//! 経路そのものが無くなった（共有 crate の`run_program_with`のテストが
//! 「起動失敗は`Stopped(n)`を 1 回だけ・Running なし」を固定する）。
//! 差は、本体の panic で`Stopped`（`ServiceSpecific(1)`）を報告するように
//! なったこと（オーナー決定 D2）と、手動で`run-service`を実行したときの
//! 案内の文言が sink と共通になったこと（同 D3、標準エラー出力だけ・
//! 終了コード 1 は不変）。
//!
//! ## サービス名・表示名・起動種別
//!
//! サービス名 `BantoHub`・表示名「banto-hub タグサーバー」。
//!
//! **T17-4（P4「Demand 化」、2026-08-10、
//! docs/banto-hub-t17-design.md §1「P4」・§11）以降、新規インストールの
//! 既定起動種別は手動開始（`ServiceStartType::OnDemand`）**
//! （`banto_hub_core::service_install::install`参照）- OS 再起動だけで
//! banto-hub の収集が始まらないようにするための決定で、`Start-Service`
//! または管理 UI からの明示操作でのみサービスが開始する。サービスが
//! 実際に開始したときにサービス本体（[`HubProgram`]）が即座に
//! `Configured` 収集を開始する挙動自体は変えていない。
//!
//! 自動開始を有効にしたい場合（`banto_hub_core::service_manager::
//! WindowsServiceManager::set_auto_start(true)`、管理 UI 等からの明示
//! 操作経由）は、従来どおり**遅延**自動開始
//! （`set_delayed_auto_start(true)`）を組み合わせる - banto-hub は起動
//! 直後に TCP bind と（設定次第で）LAN 上の PLC への接続を試みるため、
//! OS 起動直後・ネットワークスタック初期化がまだ終わっていないタイミング
//! で起動が競合する事故を避ける（docs/tag-server-design.md §8 常駐の
//! 判断）。`OnDemand`には遅延自動開始の概念が無い（Windows API 仕様）ため
//! `install`ではこの呼び出しを行わない。
//!
//! ## 設定モード ⇔ 運用モード（#661 661-2、2026-10-19）
//!
//! docs/banto-hub-t17-design.md §1 の「2026-10-19 オーナー決定（#661）」と、ChronoGazer の
//! `chronogazer_server/service.rs`（#623 PR-3）を手本にした。サービスが使う profile
//! （`BANTO_HUB_PROFILE`）の木だけを締める（`{root}` と `profiles\` は開いて本当のパスを
//! 確かめるだけ）。判定・文言の純関数は `banto_hub_core::profile_mode`、戻す相手の HKLM の
//! 記録は `banto_hub_core::profile_owners`。
//!
//! **2 つの約束**: 締め終わるまで Hub を動かさない（緩い間に LocalSystem が書くのは、辿らずに
//! 開いたハンドルの診断ファイルとサービスログだけ）。緩め始めたら新しく開かず、ログにも
//! 書かない。
//!
//! 起動（[`HubProgram::run_inner`]）:
//! 1. Redirection Guard。
//! 2. 開始の引数（`relax-only` ちょうどなら [`HubProgram::relax_only`]、知らない引数は何も
//!    変えずに 3）。
//! 3. 戻す相手を決める（`profile_mode::plan_owners`: 設定モードの形なら DACL から採り、
//!    運用モードの形なら HKLM の記録を使う）。
//! 4. profile を辿らずに開いて握る（`Mode::Guarded`、作らない）→ 5. profile の排他（取れなければ
//!    2）→ 6. `logs` を設定モードの形で開いてサービスログを始め、「運用モードへの切り替えを
//!    始めます」。
//! 7. 相手が 0 人なら何も変えずに 4、締まっているのに記録が使えなければ何も変えずに 5。
//! 8. 1 周目（`scan_tree`。断ったら何も変えずに 4）→ 設定モードの形からなら、ここで HKLM に
//!    相手を記録する（書けなければ何も変えずに 3）。
//! 9. 2 周目（`close_tree`。止まったら `relax_tree` で緩め直して 4、緩め直せなければ 5）。
//! 10. 管理するフォルダ（`config`・`data`・`logs`・`data\audit-spool`。監査の保留も必須）を運用
//!     モードの形で開き、印を置く（改ざんなら緩め直して 5、I/O なら緩め直して 3）。続けて
//!     見張り（`SecurityWatch`）と定期の確かめ直し（`recheck`、60 秒）を立てる（立てられなければ
//!     緩め直して 3）。運用中にどちらかが落ちたら、Hub を止め・設定 DB を閉じ・**締めたまま** 5
//!     （締める前から開かれていた `WRITE_DAC` のハンドルでの書き戻しに備える。#697 レビュー）。
//! 11. Hub を起動（`HubRuntime::start_with_profile_dirs`。排他はこの本体が持つので
//!     `skip_profile_lock`。設定 DB のパスは運用モードの形を確かめてから渡す）→ Configured の
//!     収集 → Running →「切り替えを終えました（所要 N 秒）」。`BANTO_DB`・`BANTO_HUB_DATA` が
//!     profile の外なら 1 行ずつ出す（締めない。既知の制限）。起動に失敗したら Hub を止めて
//!     から緩め直す（終了コードは 1〜3、緩め直せなければ 5）。
//!
//! STOP（設定モードへ）: Hub を止めて設定 DB を閉じる（`shutdown_and_report`。閉じ切れなければ
//! 緩めずに 5）→ tokio ランタイムを Stopped の報告の前に終える（上限 [`RUNTIME_SHUTDOWN_WAIT`]。
//! 終わらなければ緩めずに 5）→ 最後の行「緩め始めます」→ サービスログを閉じる → `relax_tree`
//! → 0／5。**PRESHUTDOWN（OS の終了）・異常終了では緩めない**（運用モードは再起動をまたいで
//! 続き、次の開始は記録の相手で締め直して動く）。
//!
//! `relax-only`: profile を握る（無ければ 0）→ 排他（2）→ `logs` を開ければログ → HKLM の記録で
//! `relax_tree` → **Running を報告せずに** 0／5（記録が無いのに締まっていれば緩めずに 5）。
//!
//! テストは ACL の部品（[`AclOps`]）を偽物にし、`Mode::Plain` の一時 profile の上で、呼ばれた
//! 順とログの行を 1 つの列にして比べる（ChronoGazer の `FakeAcl` と同じ方式）。

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use banto_hub_core::controller::{CollectionState, RunMode};
use banto_hub_core::hub_log;
use banto_hub_core::profile_lock::{
    open_profile_dirs_checked, try_acquire_profile_exclusive, HubHostKind, ProfileLockError,
    DIR_AUDIT_SPOOL, DIR_CONFIG, DIR_DATA, DIR_LOGS,
};
use banto_hub_core::profile_mode::{
    acl_changed_line, after_close_abort, after_marker_error, check_stored_owners, close_abort_line,
    db_not_closed_keeps_tight_line, exit_after_relax_back, exit_after_scan_refusal,
    mode_switch_begin_line, mode_switch_done_line, no_owners_line, owners_from_dacl_line,
    owners_from_record_line, owners_record_failed_line, parse_start_args, plan_owners,
    plan_relax_only, preshutdown_keeps_tight_line, record_unusable_line, redirection_guard_line,
    relax_allowed_after_shutdown, relax_back_begin_line, relax_begin_line, relax_only_begin_line,
    relax_only_left_tight_line, relax_only_nothing_line, runtime_still_busy_keeps_tight_line,
    scan_refused_lines, tampered_line, unknown_start_arg_line, OwnersPlan, RelaxOnlyPlan,
    StartArgs,
};
use banto_hub_core::profile_owners::owners_location;
use banto_hub_core::profile_paths::{
    build_hub_config_from_env, external_path_line, is_outside_profile,
    resolve_profile_paths_from_env, ExternalPath, ProfilePaths,
};
use banto_hub_core::runtime::{HubConfig, HubRuntime, HubStartError, RunningHub};
use banto_hub_core::service_manager::{
    dispatch_failure_lines, run_service, OverrunWatch, ServiceContext, ServiceExit, ServiceProgram,
    StopSignal, HUB_EXIT_LEFT_TIGHT, HUB_EXIT_PROFILE_LOCK_HELD,
    HUB_EXIT_RUNTIME_OR_COLLECT_FAILED, HUB_EXIT_START_FAILED,
};
use banto_safe_root::guard::{AccountKind, KnownFolders};
use banto_safe_root::mode::{
    AbortReason, CloseAbort, MarkerError, MarkerOutcome, Progress, RecheckFailure, RelaxReport,
    RootObservation, ScanRefusal, ScanReport, StopSignal as TreeStop, WatchEvent,
};
use banto_safe_root::{
    enable_redirection_guard, open_root, DirAccess, InstallUserSid, ManagedDir, ManagedSpec, Mode,
    ModeAccess, RedirectionGuard, RootSpec, SafeRoot, SafeRootError,
};
// T17-0（docs/banto-hub-t17-design.md §3「T17-0」）: サービス名・起動引数は
// `banto_hub_core::service_manager`（`WindowsServiceManager`が実 SCM 再登録
// 時に使う値と同じもの）を単一のソースとして再利用する - このファイルは
// 以前`SERVICE_NAME`/`RUN_SERVICE_ARG`を自前で定義していたが、値そのものは
// 1バイトも変えていない（`pub use`での再公開なので、このファイル内・
// `bin/banto-hub.rs`からの`win_service::SERVICE_NAME`/`RUN_SERVICE_ARG`参照は
// 変更不要）。
pub use banto_hub_core::service_manager::{RUN_SERVICE_ARG, SERVICE_NAME};

/// ログ行と案内の先頭に付ける製品名。
const LABEL: &str = "banto-hub";

/// [`HubStartError`]から、SCM へ報告する非0 exit code とサービスログへ
/// 書く診断メッセージを組み立てる純関数（I/O・SCM 呼び出しを一切行わない
/// ので単体テストできる - 2026-09-07 I4 実機検証で見つかったバグの修正）。
///
/// **観測されたバグ**: サービス本体は以前、`HubRuntime::start`が
/// 失敗した場合に`panic!`していた。`HubStartError::ProfileLock(AlreadyHeld)`は
/// 「シェルが既に profile ロックを握っている状態で Operator が`sc start BantoHub`
/// した」という日常的に起こり得る操作であり、プログラミングバグではない。
/// この関数はその`panic!`分岐の代わりに、本体がログを書いて終了コードを返す
/// （共有ランナーが`Stopped`を報告する）ための値を作る。終了コードの定数は
/// `banto_hub_core::service_manager`（`HUB_EXIT_*`、#661 661-2 で lib へ移した）。
fn describe_start_failure(err: &HubStartError) -> (u32, String) {
    if let HubStartError::ProfileLock(ProfileLockError::AlreadyHeld { profile_id, owner }) = err {
        let owner_desc = owner
            .as_ref()
            .map(|info| format!("pid={} host_kind={}", info.pid, info.host_kind))
            .unwrap_or_else(|| "unknown".to_string());
        return (
            HUB_EXIT_PROFILE_LOCK_HELD,
            format!(
                "banto-hub: 起動に失敗しました - profile '{profile_id}' は既に別プロセスが使用中です（owner: {owner_desc}）"
            ),
        );
    }
    (
        HUB_EXIT_START_FAILED,
        format!("banto-hub: 起動に失敗しました: {err}"),
    )
}

/// 停止に入ったときのログ。通常の STOP と OS の PRESHUTDOWN を区別する。
fn hub_stop_reason_line(signal: StopSignal) -> String {
    match signal {
        StopSignal::ServiceStop => {
            "banto-hub: 停止要求を受けました（SCM の STOP）。最終 flush を行います".to_string()
        }
        StopSignal::OsPreshutdown => {
            "banto-hub: OS のシャットダウンを受けました（PRESHUTDOWN）。最終 flush を行います"
                .to_string()
        }
    }
}

/// 最終 flush を含む停止が予算内に終わったときのログ。
fn hub_stop_done_line(signal: StopSignal) -> String {
    match signal {
        StopSignal::ServiceStop => {
            "banto-hub: 最終 flush を含む停止が完了しました（SCM の STOP）".to_string()
        }
        StopSignal::OsPreshutdown => {
            "banto-hub: 最終 flush を含む停止が完了しました（PRESHUTDOWN）".to_string()
        }
    }
}

/// OS シャットダウンの作業予算を超えても停止が終わっていないときのログ。
/// プロセスは止めない（収集の停止を abort しない）。SCM が
/// PRESHUTDOWN の待ち時間（30 秒）でプロセスを終わらせうる、という警告。
fn hub_stop_overrun_line() -> &'static str {
    "banto-hub: warn: OS シャットダウンの予算内に最終 flush が終わりませんでした（SCM がプロセスを終了させる可能性があります）"
}

/// 設定 DB を閉じ終えたときの行（STOP で緩める前の順序の目印）。
const DB_CLOSED_LINE: &str = "banto-hub: 設定 DB を閉じました";
/// Hub の tokio ランタイムを終えたときの行（同上）。
const RUNTIME_FINISHED_LINE: &str =
    "banto-hub: Hub の非同期ランタイムを終えました（残りのタスクはありません）";

/// 通常の STOP で Hub の停止にかかる見込み。`ServiceStatusReporter` の
/// heartbeat は、これを 5 秒の刻みで割った回数まで StopPending の
/// checkpoint を進め、その後は進めない（停止が固まったら、`wait_hint` の
/// 30 秒の後に SCM が応答なしを報告できるように）。収集停止は打ち切らない
/// ので、これを超えても停止そのものは続く。
const HUB_EXPECTED_STOP: Duration = Duration::from_secs(60);
/// START_PENDING の報告の wait hint。
const START_WAIT_HINT: Duration = Duration::from_secs(10);
/// 進み具合で checkpoint を進める間隔。
const PROGRESS_INTERVAL: Duration = Duration::from_secs(1);
/// STOP で、緩める前に Hub の tokio ランタイムを終えるのを待つ上限。非同期のタスクは
/// 終えるときに全部捨てるので、待つのは実質 `spawn_blocking` の残り。過ぎたら緩めずに 5。
const RUNTIME_SHUTDOWN_WAIT: Duration = Duration::from_secs(10);
/// 運用中の定期の確かめ直しの間隔（ChronoGazer と同じ 60 秒）。
const RECHECK_INTERVAL: Duration = Duration::from_secs(60);
/// 止める合図と改ざんの印を待つ刻み。
const WAIT_POLL: Duration = Duration::from_millis(200);
/// 木を歩くスレッドのスタック（ChronoGazer と同じ。深い木で溢れない大きさ）。
const WALK_STACK: usize = 16 * 1024 * 1024;

/// 締める前に開く `logs`（設定モードの形で作る）。
const LOGS_SPEC: ManagedSpec = ManagedSpec {
    key: DIR_LOGS,
    parent: None,
    name: "logs",
    create: true,
};

/// `bin/banto-hub.rs`の`install`サブコマンド用引数リテラル。
pub const INSTALL_ARG: &str = "install";
/// `bin/banto-hub.rs`の`uninstall`サブコマンド用引数リテラル。
pub const UNINSTALL_ARG: &str = "uninstall";

/// Windows サービスとして登録する（管理者権限が必要 - 失敗時はその旨を
/// 案内して終了する）。本体は`banto_hub_core::service_install::install`
/// （このファイルのモジュール doc上部の T17-2 コメント参照）- ここは
/// `banto-hub.exe install`から見た薄い委譲で、挙動は移設前と同一
/// （`None`を渡すので`std::env::current_exe()`で自分自身を登録対象にする、
/// 従来どおりの経路）。
pub fn install() {
    banto_hub_core::service_install::install(None);
}

/// サービス登録を解除する（管理者権限が必要）。本体は
/// `banto_hub_core::service_install::uninstall`（上記`install`と同じ理由）。
pub fn uninstall() {
    banto_hub_core::service_install::uninstall();
}

/// SCM がサービス開始時に呼ぶ内部エントリポイント（`run-service`
/// サブコマンド）。本体は[`HubProgram`]で、共有ランナー
/// （`banto_service_control::run_service`）に渡す。人間が直接叩いても
/// ディスパッチャの起動がエラーを返すだけ（SCM 経由以外での呼び出しは
/// 想定されていない - Windows の制約）で、製品共通の案内（#392 B3-1 の
/// オーナー決定 D3）を標準エラー出力に出して終了コード 1。
pub fn run_service_dispatcher() {
    if let Err(err) = run_service(HubProgram::production()) {
        for line in dispatch_failure_lines(LABEL, &err) {
            eprintln!("{line}");
        }
        std::process::exit(1);
    }
}

// --- 差し替え口 ------------------------------------------------------------------------

/// profile のパスと Hub の設定（本番は環境変数。テストは一時 profile）。
type ConfigSource = Box<dyn Fn() -> (ProfilePaths, HubConfig) + Send + Sync>;
/// HKLM の戻す相手の記録を読む（本番は `profile_owners::read_owners`）。
type OwnersReader = Box<dyn Fn(&str) -> Result<Option<Vec<String>>, String> + Send + Sync>;
/// HKLM に戻す相手を記録する（本番は `profile_owners::write_owners`）。
type OwnersWriter = Box<dyn Fn(&str, &[InstallUserSid]) -> Result<(), String> + Send + Sync>;
/// ルートにしてはいけない既知のフォルダ。
type KnownFoldersSource = Box<dyn Fn() -> Result<KnownFolders, String> + Send + Sync>;
/// Redirection Guard を有効にする（テストは呼ばない - テストのプロセス全体に効くため）。
type RedirectionGuardSource = Box<dyn Fn() -> RedirectionGuard + Send + Sync>;

/// サービスログ（本番は `hub_log` のサービスログ。テストは行を集める偽物）。
pub(crate) trait ServiceLog: Send + Sync {
    /// 握った `logs\` の中にサービスログを辿らずに開く。
    fn open(&self, logs: &ManagedDir) -> Result<(), String>;
    /// 1 行（開いていなければ標準出力だけ）。
    fn line(&self, line: &str);
    /// エラーの 1 行。
    fn error(&self, line: &str);
    /// ディスクへ出す。
    fn sync(&self);
    /// ディスクへ出してから閉じる（以後ファイルへは書かない）。
    fn close(&self);
}

/// 本番: `hub_log` のサービスログ。
struct HubServiceLog;

impl ServiceLog for HubServiceLog {
    fn open(&self, logs: &ManagedDir) -> Result<(), String> {
        hub_log::enable_service_log_file_in(logs).map_err(|err| err.to_string())
    }

    fn line(&self, line: &str) {
        hub_log::log_line(line);
    }

    fn error(&self, line: &str) {
        hub_log::log_err_line(line);
    }

    fn sync(&self) {
        hub_log::sync_service_log();
    }

    fn close(&self) {
        hub_log::close_service_log();
    }
}

/// ACL の付け替えの部品（本番は `banto-safe-root`。テストは偽物で、`Mode::Plain` の
/// profile の上で順序と失敗の注入だけ）。どれもログを出さない。
pub(crate) trait AclOps: Send + Sync {
    /// profile の握り方（本番 `Guarded`、テスト `Plain`）。
    fn root_mode(&self) -> Mode;
    /// profile を読むだけで開いて記述子を読む（形と、戻す相手を採るため）。
    fn observe(&self, profile_dir: &Path) -> Result<RootObservation, String>;
    /// SID が利用者のアカウントか。
    fn account_kind(&self, sid: &InstallUserSid) -> AccountKind;
    /// 1 周目。
    fn scan_tree(
        &self,
        root: &SafeRoot,
        stop: &dyn Fn() -> TreeStop,
        progress: &mut dyn FnMut(&Progress),
    ) -> Result<ScanReport, ScanRefusal>;
    /// 2 周目。
    fn close_tree(
        &self,
        root: &SafeRoot,
        mode: &ModeAccess,
        stop: &dyn Fn() -> TreeStop,
        progress: &mut dyn FnMut(&Progress),
    ) -> Result<Progress, CloseAbort>;
    /// 緩める（下から、profile 自身を最後に）。
    fn relax_tree(
        &self,
        root: &SafeRoot,
        mode: &ModeAccess,
        progress: &mut dyn FnMut(&Progress),
    ) -> Result<RelaxReport, SafeRootError>;
    /// 印のファイル。
    fn ensure_marker(&self, dir: &ManagedDir) -> Result<MarkerOutcome, MarkerError>;
    /// 確かめ直し（握ったフォルダが運用モードの形のままか）。
    fn recheck(&self, dir: &ManagedDir, expected: &DirAccess) -> Result<(), RecheckFailure>;
    /// セキュリティの変化の見張り。`None` なら見張らない（テスト）。
    fn watch(&self, root: &SafeRoot) -> Result<Option<Box<dyn SecurityWatcher>>, String>;
}

/// セキュリティの変化の見張り（本番は `banto_safe_root::mode::SecurityWatch`。ChronoGazer の
/// `chronogazer_server/service.rs` と同じ差し替え口）。
pub(crate) trait SecurityWatcher: Send {
    /// 次の変化を `timeout` まで待つ。
    fn wait(&mut self, timeout: Duration) -> Result<WatchEvent, String>;
    /// 別のスレッドから止める口。
    fn stopper(&self) -> Box<dyn Fn() + Send + Sync>;
}

impl SecurityWatcher for banto_safe_root::mode::SecurityWatch {
    fn wait(&mut self, timeout: Duration) -> Result<WatchEvent, String> {
        banto_safe_root::mode::SecurityWatch::wait(self, timeout).map_err(|err| err.to_string())
    }

    fn stopper(&self) -> Box<dyn Fn() + Send + Sync> {
        let stop = banto_safe_root::mode::SecurityWatch::stopper(self);
        Box::new(move || stop.stop())
    }
}

/// 本番: `banto-safe-root` の部品そのもの。
struct RealAcl;

impl AclOps for RealAcl {
    fn root_mode(&self) -> Mode {
        Mode::Guarded
    }

    fn observe(&self, profile_dir: &Path) -> Result<RootObservation, String> {
        banto_safe_root::mode::observe_root(profile_dir).map_err(|err| err.to_string())
    }

    fn account_kind(&self, sid: &InstallUserSid) -> AccountKind {
        banto_safe_root::guard::account_kind(sid)
    }

    fn scan_tree(
        &self,
        root: &SafeRoot,
        stop: &dyn Fn() -> TreeStop,
        progress: &mut dyn FnMut(&Progress),
    ) -> Result<ScanReport, ScanRefusal> {
        root.pinned()
            .ok_or(ScanRefusal::Io(SafeRootError::UnsupportedPlatform))?
            .scan_tree(stop, progress)
    }

    fn close_tree(
        &self,
        root: &SafeRoot,
        mode: &ModeAccess,
        stop: &dyn Fn() -> TreeStop,
        progress: &mut dyn FnMut(&Progress),
    ) -> Result<Progress, CloseAbort> {
        root.pinned()
            .ok_or(CloseAbort {
                written: 0,
                reason: AbortReason::Io(SafeRootError::UnsupportedPlatform),
            })?
            .close_tree(mode, stop, progress)
    }

    fn relax_tree(
        &self,
        root: &SafeRoot,
        mode: &ModeAccess,
        progress: &mut dyn FnMut(&Progress),
    ) -> Result<RelaxReport, SafeRootError> {
        root.pinned()
            .ok_or(SafeRootError::UnsupportedPlatform)?
            .relax_tree(mode, progress)
    }

    fn ensure_marker(&self, dir: &ManagedDir) -> Result<MarkerOutcome, MarkerError> {
        dir.ensure_marker()
    }

    fn recheck(&self, dir: &ManagedDir, expected: &DirAccess) -> Result<(), RecheckFailure> {
        dir.recheck(expected)
    }

    fn watch(&self, root: &SafeRoot) -> Result<Option<Box<dyn SecurityWatcher>>, String> {
        let pinned = root.pinned().ok_or("profile を握っていません")?;
        let watch = pinned.watch_security().map_err(|err| err.to_string())?;
        Ok(Some(Box::new(watch)))
    }
}

/// 改ざん（見張り・定期の確かめ直し）の印。最初の 1 件だけ残す。
#[derive(Default)]
struct Tamper(Mutex<Option<String>>);

impl Tamper {
    fn set(&self, what: String) {
        let mut slot = self.0.lock().unwrap_or_else(|p| p.into_inner());
        if slot.is_none() {
            *slot = Some(what);
        }
    }

    fn get(&self) -> Option<String> {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }
}

/// 運用中の見張りと定期の確かめ直し。drop で止める（緩める前に必ず止める）。
struct Watchers {
    stop_watch: Option<Box<dyn Fn() + Send + Sync>>,
    stop_recheck: Option<std::sync::mpsc::Sender<()>>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl Watchers {
    fn stop(&mut self) {
        if let Some(stop) = self.stop_watch.take() {
            stop();
        }
        self.stop_recheck.take();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

impl Drop for Watchers {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Hub を動かす tokio ランタイム。**Stopped の報告の前に終える**（`hold_until_stopped` は
/// 使わない）: STOP では [`Self::shutdown_within`] で終わりを待ち、それ以外の道（PRESHUTDOWN・
/// 起動の失敗・panic）では drop で残りのタスクを待たずに落とす（`shutdown_background`）。
struct EngineRuntime(Option<tokio::runtime::Runtime>);

impl EngineRuntime {
    fn new() -> std::io::Result<Self> {
        tokio::runtime::Runtime::new().map(|runtime| Self(Some(runtime)))
    }

    fn block_on<F: std::future::Future>(&self, future: F) -> F::Output {
        self.0
            .as_ref()
            .expect("ランタイムを終える前だけ使う")
            .block_on(future)
    }

    /// ランタイムを終わらせ、`cap` まで待つ（別のスレッドで drop する。tokio の `Runtime`
    /// の drop は非同期のタスクを全部捨て、`spawn_blocking` の分が終わるまで待つ）。戻れば
    /// `true`（もうこのランタイムで動く物は無い）。待つ間は `PROGRESS_INTERVAL` ごとに `tick`。
    fn shutdown_within(&mut self, cap: Duration, tick: &mut dyn FnMut()) -> bool {
        let Some(runtime) = self.0.take() else {
            return true;
        };
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        let spawned = std::thread::Builder::new()
            .name("banto-hub-runtime-end".to_string())
            .spawn(move || {
                drop(runtime);
                let _ = done_tx.send(());
            });
        if spawned.is_err() {
            return false;
        }
        let deadline = Instant::now() + cap;
        loop {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            match done_rx.recv_timeout((deadline - now).min(PROGRESS_INTERVAL)) {
                Ok(()) => return true,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return false,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => tick(),
            }
        }
    }
}

impl Drop for EngineRuntime {
    fn drop(&mut self) {
        if let Some(runtime) = self.0.take() {
            runtime.shutdown_background();
        }
    }
}

/// Hub のサービス本体（共有ランナーに渡す）。SCM とのやりとり（制御ハンドラ、Running →
/// StopPending → Stopped の報告、panic）は共有ランナーが持ち、ここは段の順序と終了コード。
pub(crate) struct HubProgram {
    config: ConfigSource,
    read_owners: OwnersReader,
    write_owners: OwnersWriter,
    known_folders: KnownFoldersSource,
    redirection_guard: RedirectionGuardSource,
    acl: Arc<dyn AclOps>,
    log: Arc<dyn ServiceLog>,
    runtime_shutdown_wait: Duration,
    /// 運用中の定期の確かめ直しの間隔（本番は [`RECHECK_INTERVAL`]。テストは短くする）。
    recheck_interval: Duration,
    /// テスト専用: `shutdown_and_report` の `db_closed` を `false` に偽る。
    #[cfg(test)]
    fake_db_not_closed: std::sync::atomic::AtomicBool,
}

/// [`HubProgram::new`] に渡す部品の組（本番とテストで差し替える物）。
pub(crate) struct ProgramParts {
    pub config: ConfigSource,
    pub read_owners: OwnersReader,
    pub write_owners: OwnersWriter,
    pub known_folders: KnownFoldersSource,
    pub redirection_guard: RedirectionGuardSource,
    pub acl: Arc<dyn AclOps>,
    pub log: Arc<dyn ServiceLog>,
}

/// 運用中に見張る物（握った profile と、運用モードの形で開いた管理するフォルダ）。
struct WatchedDirs {
    root: Arc<SafeRoot>,
    dirs: [(&'static str, Arc<ManagedDir>); 4],
}

/// 締めた後の Hub が動いている間に持つ物。
struct Engine {
    runtime: EngineRuntime,
    hub: Option<RunningHub>,
}

impl HubProgram {
    fn production() -> Self {
        Self::new(ProgramParts {
            config: Box::new(|| {
                let paths = resolve_profile_paths_from_env();
                let config = build_hub_config_from_env(HubHostKind::Service, crate::hub_drivers());
                (paths, config)
            }),
            read_owners: Box::new(banto_hub_core::profile_owners::read_owners),
            write_owners: Box::new(banto_hub_core::profile_owners::write_owners),
            known_folders: Box::new(|| {
                use banto_safe_root::guard::MachineEnvironment;
                banto_safe_root::guard::win::RegistryMachineEnvironment.known_folders()
            }),
            redirection_guard: Box::new(enable_redirection_guard),
            acl: Arc::new(RealAcl),
            log: Arc::new(HubServiceLog),
        })
    }

    fn new(parts: ProgramParts) -> Self {
        Self {
            config: parts.config,
            read_owners: parts.read_owners,
            write_owners: parts.write_owners,
            known_folders: parts.known_folders,
            redirection_guard: parts.redirection_guard,
            acl: parts.acl,
            log: parts.log,
            runtime_shutdown_wait: RUNTIME_SHUTDOWN_WAIT,
            recheck_interval: RECHECK_INTERVAL,
            #[cfg(test)]
            fake_db_not_closed: std::sync::atomic::AtomicBool::new(false),
        }
    }

    fn say(&self, line: &str) {
        self.log.line(line);
    }

    /// 1 周目・2 周目・緩めるを**専用のスレッド**（スタックを明示）で走らせる。
    fn on_walk_thread<T: Send>(&self, name: &str, work: impl FnOnce() -> T + Send) -> T {
        std::thread::scope(|scope| {
            std::thread::Builder::new()
                .name(name.to_string())
                .stack_size(WALK_STACK)
                .spawn_scoped(scope, work)
                .map(|handle| handle.join().expect("木を歩くスレッドが panic した"))
                .unwrap_or_else(|_| panic!("木を歩くスレッドを作れない（{name}）"))
        })
    }

    /// 緩める（ログは閉じてから呼ぶ）。`stopping` なら STOP_PENDING、そうでなければ
    /// START_PENDING の checkpoint を進める。戻り値は緩められたか。
    fn relax(
        &self,
        ctx: &ServiceContext,
        root: &SafeRoot,
        mode: &ModeAccess,
        stopping: bool,
    ) -> bool {
        let mut last = Instant::now();
        let mut progress = |_: &Progress| {
            if last.elapsed() >= PROGRESS_INTERVAL {
                last = Instant::now();
                if stopping {
                    ctx.report_stop_progress();
                } else {
                    ctx.report_start_progress(START_WAIT_HINT);
                }
            }
        };
        self.on_walk_thread("banto-hub-relax", || {
            self.acl.relax_tree(root, mode, &mut progress).is_ok()
        })
    }

    /// 締めた分を緩め直して終わる（起動の途中で止まったとき）。最後の行 → ログを閉じる →
    /// 緩める。緩め直せたら `if_relaxed`、できなければ 5。
    fn relax_back(
        &self,
        ctx: &ServiceContext,
        root: &SafeRoot,
        mode: &ModeAccess,
        why: &str,
        if_relaxed: u32,
    ) -> u32 {
        self.say(&relax_back_begin_line(why, if_relaxed));
        self.log.close();
        let relaxed = self.relax(ctx, root, mode, false);
        exit_after_relax_back(relaxed, if_relaxed)
    }

    /// 締める前に終わる（何も変えていない、または緩めない）。
    fn finish_without_relax(&self, code: u32) -> u32 {
        self.say(&format!(
            "{LABEL}: Windows サービスを停止しました（終了コード {code}）"
        ));
        self.log.close();
        code
    }

    /// profile を辿らずに開いて握る（作らない。`{root}`・`profiles\` は本当のパスを確かめる
    /// だけ）。`owners` は所有者として許す利用者。
    fn open_profile(
        &self,
        paths: &ProfilePaths,
        owners: &[InstallUserSid],
    ) -> Result<SafeRoot, String> {
        let known = (self.known_folders)()
            .map_err(|reason| format!("既知のフォルダの場所を取れませんでした: {reason}"))?;
        open_root(&RootSpec {
            expected: paths.profile_dir.clone(),
            known,
            owners: owners.to_vec(),
            create_access: None,
            create: false,
            mode: self.acl.root_mode(),
        })
        .map_err(|refusal| refusal.to_string())
    }

    /// `logs\` を握る（`create` なら無ければ `access` の形で作る）。無くて作らないなら `Ok(None)`。
    fn open_logs(
        &self,
        root: &SafeRoot,
        access: Option<&DirAccess>,
        create: bool,
    ) -> Result<Option<Arc<ManagedDir>>, String> {
        let spec = ManagedSpec {
            create,
            ..LOGS_SPEC
        };
        match root.open_managed(&[spec], access) {
            Ok(mut set) => Ok(Some(Arc::new(set.take(DIR_LOGS).expect("spec にある")))),
            Err(stop) if matches!(stop.reason, banto_safe_root::root::ManagedRefusal::Absent) => {
                Ok(None)
            }
            Err(stop) => Err(stop.to_string()),
        }
    }

    /// Hub を止め、設定 DB を閉じ、ランタイムを終える。緩めてよければ `Ok`、だめなら
    /// 締めたまま終わる終了コード（5）。`stopping` なら STOP_PENDING の checkpoint を進める。
    fn stop_engine(
        &self,
        ctx: &ServiceContext,
        mut engine: Engine,
        stopping: bool,
    ) -> Result<(), u32> {
        let db_closed = match engine.hub.take() {
            Some(hub) => {
                let report = engine.runtime.block_on(hub.shutdown_and_report());
                #[cfg(test)]
                let report = banto_hub_core::runtime::HubShutdownReport {
                    db_closed: report.db_closed
                        && !self
                            .fake_db_not_closed
                            .load(std::sync::atomic::Ordering::SeqCst),
                };
                report.db_closed
            }
            None => true,
        };
        if db_closed {
            self.say(DB_CLOSED_LINE);
        }
        let mut tick = || {
            if stopping {
                ctx.report_stop_progress();
            } else {
                ctx.report_start_progress(START_WAIT_HINT);
            }
        };
        // 設定 DB を閉じ切れなくても、ランタイムは終える（Stopped の報告の前に）。
        let runtime_finished = engine
            .runtime
            .shutdown_within(self.runtime_shutdown_wait, &mut tick);
        if runtime_finished {
            self.say(RUNTIME_FINISHED_LINE);
        }
        if relax_allowed_after_shutdown(db_closed, runtime_finished) {
            return Ok(());
        }
        if !db_closed {
            self.say(&db_not_closed_keeps_tight_line());
        } else {
            self.say(&runtime_still_busy_keeps_tight_line(
                self.runtime_shutdown_wait,
            ));
        }
        Err(HUB_EXIT_LEFT_TIGHT)
    }

    /// 起動の途中（締めた後）で Hub を動かさずに終わる: Hub を止めてから緩め直す
    /// （PRESHUTDOWN が来ていれば緩めない）。
    fn abandon_start(
        &self,
        ctx: &ServiceContext,
        engine: Engine,
        root: &SafeRoot,
        mode: &ModeAccess,
        code: u32,
    ) -> u32 {
        if let Err(left_tight) = self.stop_engine(ctx, engine, false) {
            return self.finish_without_relax(left_tight);
        }
        if is_preshutdown(ctx) {
            self.say(&preshutdown_keeps_tight_line());
            return self.finish_without_relax(code);
        }
        self.relax_back(ctx, root, mode, "Hub を動かさずに終わるため", code)
    }

    fn run_inner(&self, ctx: &ServiceContext) -> u32 {
        ctx.set_expected_stop(HUB_EXPECTED_STOP);
        // 1. Redirection Guard（結果はログを開いた後に 1 行）。
        let redirection = (self.redirection_guard)();

        // 2. 開始の引数（信頼できない入力。決まった値との完全一致だけ）。
        match parse_start_args(ctx.start_args()) {
            StartArgs::Normal => {}
            StartArgs::RelaxOnly => return self.relax_only(ctx),
            StartArgs::Unknown(arg) => {
                self.log.error(&unknown_start_arg_line(&arg));
                return HUB_EXIT_START_FAILED;
            }
        }

        // 3. 戻す相手（設定モードの形なら DACL から採る・運用モードの形なら記録を使う）。
        let (paths, config) = (self.config)();
        let observation = match self.acl.observe(&paths.profile_dir) {
            Ok(observation) => observation,
            Err(reason) => {
                self.log.error(&format!(
                    "{LABEL}: profile のフォルダ（{}）を確かめられませんでした（何も変えていません）: {reason}",
                    paths.profile_dir.display()
                ));
                return HUB_EXIT_START_FAILED;
            }
        };
        let stored = check_stored_owners((self.read_owners)(&paths.profile_id));
        let plan = plan_owners(&observation, stored, |sid| self.acl.account_kind(sid));
        let location = owners_location(&paths.profile_id);
        let refusal_line = |plan: &OwnersPlan| match plan {
            OwnersPlan::NoOwners(reason) => Some(no_owners_line(reason)),
            OwnersPlan::RecordUnusable(reason) => Some(record_unusable_line(reason, &location)),
            OwnersPlan::FromDacl(_) | OwnersPlan::FromRecord(_) => None,
        };

        // 4. profile（辿らずに開いて握る。作らない）。
        let root = match self.open_profile(&paths, plan.owners()) {
            Ok(root) => Arc::new(root),
            Err(reason) => {
                // 相手が決まらないなら、開けない理由より先にそちらを返す（何も変えていない）。
                if let (Some(code), Some(line)) = (plan.refusal_exit(), refusal_line(&plan)) {
                    self.log.error(&line);
                    return code;
                }
                self.log.error(&format!(
                    "{LABEL}: profile のフォルダ（{}）を使えません（何も変えていません）: {reason}",
                    paths.profile_dir.display()
                ));
                return HUB_EXIT_START_FAILED;
            }
        };

        // 5. profile の排他（取れなければ何も開かずに 2）。
        let exclusive = match try_acquire_profile_exclusive(&paths, HubHostKind::Service, &root) {
            Ok(exclusive) => exclusive,
            Err(err) => {
                let code = match err {
                    ProfileLockError::AlreadyHeld { .. } => HUB_EXIT_PROFILE_LOCK_HELD,
                    _ => HUB_EXIT_START_FAILED,
                };
                self.log.error(&format!(
                    "{LABEL}: 起動に失敗しました（何も変えていません）: {err}"
                ));
                return code;
            }
        };
        ctx.report_start_progress(START_WAIT_HINT);

        // 6. サービスログ（`logs\` を設定モードの形で握る。相手が決まらないなら作らない）。
        let mode = ModeAccess::for_users(plan.owners().to_vec());
        let logs = match self.open_logs(
            &root,
            mode.as_ref().map(ModeAccess::setting),
            mode.is_some(),
        ) {
            Ok(logs) => logs,
            Err(reason) => {
                if let (Some(code), Some(line)) = (plan.refusal_exit(), refusal_line(&plan)) {
                    self.log.error(&line);
                    return code;
                }
                self.log.error(&format!(
                    "{LABEL}: ログの置き場（logs）を開けませんでした（何も変えていません）: {reason}"
                ));
                return HUB_EXIT_START_FAILED;
            }
        };
        if let Some(logs) = &logs {
            if let Err(reason) = self.log.open(logs) {
                self.log.error(&format!(
                    "{LABEL}: サービスログを開けませんでした（何も変えていません）: {reason}"
                ));
                if plan.refusal_exit().is_none() {
                    return HUB_EXIT_START_FAILED;
                }
            }
        }
        let switch_started = Instant::now();
        self.say(&redirection_guard_line(redirection));
        self.say(&mode_switch_begin_line());

        // 7. 相手が決まらない: 何も変えずに 4（0 人）／5（締まっているのに記録が使えない）。
        let (Some(mode), None) = (mode, plan.refusal_exit()) else {
            let line = refusal_line(&plan).expect("決まらないときは行がある");
            self.say(&line);
            let code = plan.refusal_exit().expect("決まらない");
            drop(exclusive);
            return self.finish_without_relax(code);
        };
        match &plan {
            OwnersPlan::FromDacl(owners) => {
                self.say(&owners_from_dacl_line(owners.len(), &location))
            }
            OwnersPlan::FromRecord(owners) => {
                self.say(&owners_from_record_line(owners.len(), &location))
            }
            _ => {}
        }

        // 8. 1 周目（読むだけ。断ったら何も変えずに 4。締まった形から再開していたなら、
        // 何も変えていなくても締まったままなので 5）。
        let from_tight = matches!(plan, OwnersPlan::FromRecord(_));
        let stop = || tree_stop(ctx);
        let progress = Throttle::new(ctx);
        let scanned = self.on_walk_thread("banto-hub-scan", || {
            self.acl.scan_tree(&root, &stop, &mut progress.callback())
        });
        match scanned {
            Ok(report) => self.say(&format!(
                "{LABEL}: profile を確かめました（{} 件。締められない物はありません）",
                report.examined
            )),
            Err(refusal) => {
                let code = exit_after_scan_refusal(&refusal, from_tight);
                let state = if from_tight {
                    "何も変えていません。profile は締まったまま＝運用モードの形のままです"
                } else {
                    "何も変えていません"
                };
                match &refusal {
                    ScanRefusal::Problems(problems) => {
                        for line in scan_refused_lines(problems, from_tight) {
                            self.say(&line);
                        }
                    }
                    ScanRefusal::Io(error) => self.say(&format!(
                        "{LABEL}: profile を確かめられませんでした（終了コード {code}。{state}）: {error}"
                    )),
                    ScanRefusal::Stopped(signal) => self.say(&format!(
                        "{LABEL}: 確かめている途中で止める合図（{signal:?}）を受けたため、起動を中止します（終了コード {code}。{state}）"
                    )),
                }
                return self.finish_without_relax(code);
            }
        }

        // 8b. 設定モードの形から締めるときだけ、締める直前に相手を HKLM に記録する（運用
        // モードの形からの再開では書かない = 記録を空や採り直しで上書きしない）。
        if let OwnersPlan::FromDacl(owners) = &plan {
            if let Err(error) = (self.write_owners)(&paths.profile_id, owners) {
                self.say(&owners_record_failed_line(&error));
                return self.finish_without_relax(HUB_EXIT_START_FAILED);
            }
        }

        // 9. 2 周目（締める。止まったら緩め直して 4／5）。
        let closed = self.on_walk_thread("banto-hub-close", || {
            self.acl
                .close_tree(&root, &mode, &stop, &mut progress.callback())
        });
        let tightened = match closed {
            Ok(progress) => progress,
            Err(CloseAbort { written, reason }) => {
                self.say(&close_abort_line(written, &reason));
                let plan = after_close_abort(&reason);
                if !plan.relax_back {
                    self.say(&preshutdown_keeps_tight_line());
                    return self.finish_without_relax(plan.exit_if_relaxed);
                }
                return self.relax_back(
                    ctx,
                    &root,
                    &mode,
                    "運用モードにできませんでした",
                    plan.exit_if_relaxed,
                );
            }
        };
        self.say(&format!(
            "{LABEL}: profile を運用モードの形に締めました（確かめた物 {} 件・書いた物 {} 件）",
            tightened.examined, tightened.written
        ));

        // 10. 管理するフォルダ（運用モードの形で作る。監査の保留も必須）と印。
        let dirs = match open_profile_dirs_checked(root.clone(), mode.operation()) {
            Ok(dirs) => dirs,
            Err(error) => {
                return self.relax_back(
                    ctx,
                    &root,
                    &mode,
                    &format!("管理するフォルダを開けませんでした: {error}"),
                    HUB_EXIT_START_FAILED,
                );
            }
        };
        let spool = dirs
            .audit_spool
            .clone()
            .expect("open_profile_dirs_checked は監査の保留を必ず開く");
        let marked: [(&str, &ManagedDir); 4] = [
            (DIR_CONFIG, dirs.config.as_ref()),
            (DIR_DATA, dirs.data.as_ref()),
            (DIR_LOGS, dirs.logs.as_ref()),
            (DIR_AUDIT_SPOOL, spool.as_ref()),
        ];
        for (key, dir) in marked {
            if let Err(error) = self.acl.ensure_marker(dir) {
                let (tampered, exit) = after_marker_error(&error);
                let why = if tampered {
                    tampered_line(key, &error)
                } else {
                    format!("フォルダ「{key}」に印を置けませんでした: {error}")
                };
                return self.relax_back(ctx, &root, &mode, &why, exit);
            }
        }
        ctx.report_start_progress(START_WAIT_HINT);

        // 11. Hub を起動（排他はこの本体が持つ。外のパスは 1 行ずつ）。
        let mut config = config;
        config.skip_profile_lock = true;
        let external = external_lines(&paths, &config, root.final_path());
        let runtime = match EngineRuntime::new() {
            Ok(runtime) => runtime,
            Err(err) => {
                return self.relax_back(
                    ctx,
                    &root,
                    &mode,
                    &format!("tokio ランタイムの構築に失敗しました: {err}"),
                    HUB_EXIT_RUNTIME_OR_COLLECT_FAILED,
                );
            }
        };
        let mut engine = Engine { runtime, hub: None };

        // 11a. 見張りと定期の確かめ直し（Hub を動かす前に立てる。締める前から開かれていた
        // `WRITE_DAC` のハンドルで書き戻されても、運用中に気づいて締めたまま止まる）。
        let watched = WatchedDirs {
            root: root.clone(),
            dirs: [
                (DIR_CONFIG, dirs.config.clone()),
                (DIR_DATA, dirs.data.clone()),
                (DIR_LOGS, dirs.logs.clone()),
                (DIR_AUDIT_SPOOL, spool.clone()),
            ],
        };
        let tamper = Arc::new(Tamper::default());
        let mut watchers = match self.start_watchers(&watched, mode.operation(), tamper.clone()) {
            Ok(watchers) => watchers,
            Err(reason) => {
                drop(engine);
                return self.relax_back(
                    ctx,
                    &root,
                    &mode,
                    &format!("セキュリティの見張りを始められませんでした: {reason}"),
                    HUB_EXIT_START_FAILED,
                );
            }
        };

        let started = engine
            .runtime
            .block_on(HubRuntime::start_with_profile_dirs(config, dirs));
        let hub = match started {
            Ok(hub) => hub,
            Err(err) => {
                let (code, message) = describe_start_failure(&err);
                self.say(&message);
                watchers.stop();
                return self.abandon_start(ctx, engine, &root, &mode, code);
            }
        };
        let controller = hub.controller();
        engine.hub = Some(hub);
        // 終了の受付（#392 A2 レビュー）の印を立てるのは `shutdown_and_report` だけで、それは
        // この開始より後に呼ばれるため、ここで `Err` になる経路は今は無い。
        let collect = match engine
            .runtime
            .block_on(controller.start(RunMode::Configured))
        {
            Ok(status) if status.state == CollectionState::Running => None,
            Ok(status) => Some(format!("{:?}", status.last_error)),
            Err(refused) => Some(refused.to_string()),
        };
        if let Some(reason) = collect {
            self.say(&format!(
                "{LABEL}: Configured 収集の開始に失敗しました: {reason}"
            ));
            watchers.stop();
            return self.abandon_start(
                ctx,
                engine,
                &root,
                &mode,
                HUB_EXIT_RUNTIME_OR_COLLECT_FAILED,
            );
        }
        // 起動中に STOP / PRESHUTDOWN が来ていたら Running を報告しない（そのまま停止へ）。
        if ctx.report_running() {
            self.say("banto-hub: Windows サービスとして起動しました");
        }
        self.say(&mode_switch_done_line(&tightened, switch_started.elapsed()));
        for line in external {
            self.say(&line);
        }

        // 止める合図（SCM）か改ざんの印を待つ。
        let notice = loop {
            if let Some(notice) = ctx.stop_notice() {
                break Some(notice);
            }
            if tamper.get().is_some() {
                break None;
            }
            std::thread::sleep(WAIT_POLL);
        };
        watchers.stop();
        let Some(notice) = notice else {
            // 改ざん: Hub を止め、設定 DB を閉じ、**緩めずに** 5（閉じ切れなくても 5）。
            self.say(&acl_changed_line(&tamper.get().unwrap_or_default()));
            // 停止に入っていないので STOP_PENDING の checkpoint は進まない（`report_stop_progress`
            // は `false` を返すだけ）。
            let _ = self.stop_engine(ctx, engine, true);
            drop(exclusive);
            return self.finish_without_relax(HUB_EXIT_LEFT_TIGHT);
        };
        self.say(&hub_stop_reason_line(notice.signal));
        self.log.sync();

        if notice.signal == StopSignal::OsPreshutdown {
            // PRESHUTDOWN: 締めたまま（運用モードは再起動をまたいで続く）。収集の停止は
            // 打ち切らず、作業予算を超えたら警告を 1 行書いてログをディスクへ出す。
            let overrun = notice.os_work_deadline().map(|deadline| {
                let log = self.log.clone();
                OverrunWatch::spawn(deadline, move || {
                    log.error(hub_stop_overrun_line());
                    log.sync();
                })
            });
            if let Some(hub) = engine.hub.take() {
                let _ = engine.runtime.block_on(hub.shutdown_and_report());
            }
            if let Some(watch) = overrun {
                watch.finish();
            }
            self.say(&hub_stop_done_line(notice.signal));
            self.say(&preshutdown_keeps_tight_line());
            // ランタイムは残りのタスクを待たずに落とす（Stopped の報告の前）。
            drop(engine);
            drop(exclusive);
            return self.finish_without_relax(0);
        }

        // STOP = 設定モードへ: Hub を止めて設定 DB を閉じる → ランタイムを終える → 最後の行 →
        // ログを閉じる → 緩める。
        if let Err(left_tight) = self.stop_engine(ctx, engine, true) {
            drop(exclusive);
            return self.finish_without_relax(left_tight);
        }
        self.say(&hub_stop_done_line(notice.signal));
        self.say(&relax_begin_line());
        self.log.close();
        let relaxed = self.relax(ctx, &root, &mode, true);
        // 握った profile と排他は緩め終えてから放す。
        drop(root);
        drop(exclusive);
        exit_after_relax_back(relaxed, 0)
    }

    /// 見張り（`SecurityWatch`）と定期の確かめ直しを始める（ChronoGazer の
    /// `start_watchers` と同じ形）。変化・確かめ直しの失敗は `tamper` に最初の 1 件を残す。
    fn start_watchers(
        &self,
        watched: &WatchedDirs,
        expected: &DirAccess,
        tamper: Arc<Tamper>,
    ) -> Result<Watchers, String> {
        let mut watchers = Watchers {
            stop_watch: None,
            stop_recheck: None,
            threads: Vec::new(),
        };
        if let Some(mut watch) = self.acl.watch(&watched.root)? {
            watchers.stop_watch = Some(watch.stopper());
            let tamper = tamper.clone();
            let thread = std::thread::Builder::new()
                .name("banto-hub-acl-watch".to_string())
                .spawn(move || loop {
                    match watch.wait(Duration::from_secs(1)) {
                        Ok(WatchEvent::Timeout) => continue,
                        Ok(WatchEvent::Stopped) => return,
                        Ok(WatchEvent::Changed(names)) => {
                            tamper.set(format!(
                                "見張りが変化を見つけました（{}）",
                                names.join(", ")
                            ));
                            return;
                        }
                        Ok(WatchEvent::Overflow) => {
                            tamper.set("見張りの通知があふれました（名前は不明）".to_string());
                            return;
                        }
                        Err(err) => {
                            tamper.set(format!("見張りが失敗しました: {err}"));
                            return;
                        }
                    }
                })
                .map_err(|err| err.to_string())?;
            watchers.threads.push(thread);
        }
        // 定期の確かめ直し（握った profile と管理するフォルダ）。
        let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
        watchers.stop_recheck = Some(stop_tx);
        let interval = self.recheck_interval;
        let expected = expected.clone();
        let root = watched.root.clone();
        let dirs = watched.dirs.clone();
        let acl = self.acl.clone();
        let thread = std::thread::Builder::new()
            .name("banto-hub-acl-recheck".to_string())
            .spawn(move || loop {
                match stop_rx.recv_timeout(interval) {
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    _ => return,
                }
                let targets = std::iter::once(("profile", root.as_dir()))
                    .chain(dirs.iter().map(|(key, dir)| (*key, dir.as_ref())));
                for (key, dir) in targets {
                    if let Err(failure) = acl.recheck(dir, &expected) {
                        tamper.set(format!("定期の確かめ直しで「{key}」が {failure}"));
                        return;
                    }
                }
            })
            .map_err(|err| err.to_string())?;
        watchers.threads.push(thread);
        Ok(watchers)
    }

    /// 緩めるだけの開始（`relax-only`）。Running を報告しない。DACL からは採らず、HKLM の
    /// 記録だけを使う。
    fn relax_only(&self, ctx: &ServiceContext) -> u32 {
        let (paths, _config) = (self.config)();
        let location = owners_location(&paths.profile_id);
        let observation = match self.acl.observe(&paths.profile_dir) {
            Ok(observation) => observation,
            Err(reason) => {
                self.log.error(&format!(
                    "{LABEL}: relax-only: profile のフォルダ（{}）を確かめられませんでした: {reason}",
                    paths.profile_dir.display()
                ));
                return HUB_EXIT_START_FAILED;
            }
        };
        let stored = check_stored_owners((self.read_owners)(&paths.profile_id));
        let plan = plan_relax_only(&observation, stored);
        let owners = match &plan {
            RelaxOnlyPlan::Relax(owners) => owners.clone(),
            RelaxOnlyPlan::NothingToRelax | RelaxOnlyPlan::LeftTight(_) => Vec::new(),
        };
        let root = match self.open_profile(&paths, &owners) {
            Ok(root) => root,
            Err(reason) => {
                return match &plan {
                    RelaxOnlyPlan::NothingToRelax => 0,
                    RelaxOnlyPlan::LeftTight(why) => {
                        self.log.error(&relax_only_left_tight_line(why, &location));
                        HUB_EXIT_LEFT_TIGHT
                    }
                    RelaxOnlyPlan::Relax(_) => {
                        self.log.error(&format!(
                            "{LABEL}: relax-only: profile のフォルダ（{}）を使えません: {reason}",
                            paths.profile_dir.display()
                        ));
                        HUB_EXIT_START_FAILED
                    }
                };
            }
        };
        // 排他（デスクトップの Hub が動いている間には緩めない）。
        let exclusive = match try_acquire_profile_exclusive(&paths, HubHostKind::Service, &root) {
            Ok(exclusive) => exclusive,
            Err(err) => {
                self.log
                    .error(&format!("{LABEL}: relax-only を始められません: {err}"));
                return match err {
                    ProfileLockError::AlreadyHeld { .. } => HUB_EXIT_PROFILE_LOCK_HELD,
                    _ => HUB_EXIT_START_FAILED,
                };
            }
        };
        ctx.report_start_progress(START_WAIT_HINT);
        // `logs` は作らない。開ければログに書く（開けなければログなしで続ける）。
        if let Ok(Some(logs)) = self.open_logs(&root, None, false) {
            let _ = self.log.open(&logs);
        }
        let exit = match plan {
            RelaxOnlyPlan::NothingToRelax => {
                self.say(&relax_only_nothing_line());
                self.finish_without_relax(0)
            }
            RelaxOnlyPlan::LeftTight(why) => {
                self.say(&relax_only_left_tight_line(&why, &location));
                self.finish_without_relax(HUB_EXIT_LEFT_TIGHT)
            }
            RelaxOnlyPlan::Relax(owners) => {
                let mode = ModeAccess::for_users(owners).expect("記録は空でない");
                self.say(&relax_only_begin_line());
                self.log.close();
                let relaxed = self.relax(ctx, &root, &mode, false);
                exit_after_relax_back(relaxed, 0)
            }
        };
        drop(root);
        drop(exclusive);
        exit
    }
}

/// `BANTO_DB`・`BANTO_HUB_DATA` が profile の外なら、その旨の行（締めない。#659 と合わせて
/// 決める既知の制限）。
fn external_lines(paths: &ProfilePaths, config: &HubConfig, profile_final: &Path) -> Vec<String> {
    let mut lines = Vec::new();
    let db = Path::new(&config.db_path);
    if is_outside_profile(db, &paths.profile_dir, profile_final) {
        lines.push(external_path_line(ExternalPath::Db, db));
    }
    if let Some(data) = &config.data_dir_override {
        if is_outside_profile(data, &paths.profile_dir, profile_final) {
            lines.push(external_path_line(ExternalPath::DataDir, data));
        }
    }
    lines
}

/// 起動の途中で OS の終了（PRESHUTDOWN）を受けていたか。
fn is_preshutdown(ctx: &ServiceContext) -> bool {
    ctx.stop_notice()
        .is_some_and(|notice| notice.signal == StopSignal::OsPreshutdown)
}

/// SCM の止める合図を木を歩く部品の合図に写す。
fn tree_stop(ctx: &ServiceContext) -> TreeStop {
    match ctx.stop_notice().map(|notice| notice.signal) {
        None => TreeStop::None,
        Some(StopSignal::ServiceStop) => TreeStop::Stop,
        Some(StopSignal::OsPreshutdown) => TreeStop::Preshutdown,
    }
}

/// 進み具合のコールバックを間引いて START_PENDING の checkpoint を進める（1 秒ごと）。
struct Throttle<'a> {
    ctx: &'a ServiceContext,
    last: Mutex<Instant>,
}

impl<'a> Throttle<'a> {
    fn new(ctx: &'a ServiceContext) -> Self {
        Self {
            ctx,
            last: Mutex::new(Instant::now()),
        }
    }

    fn callback(&self) -> impl FnMut(&Progress) + '_ {
        move |_| {
            let mut last = self.last.lock().unwrap_or_else(|p| p.into_inner());
            if last.elapsed() >= PROGRESS_INTERVAL {
                *last = Instant::now();
                self.ctx.report_start_progress(START_WAIT_HINT);
            }
        }
    }
}

impl ServiceProgram for HubProgram {
    fn service_name(&self) -> &str {
        SERVICE_NAME
    }

    fn label(&self) -> &str {
        LABEL
    }

    // `open_log` は使わない（#661 661-2）: サービスログは profile を辿らずに開いて握り、
    // 排他を取った後（`run_inner` の 6）に、握った `logs\` の中に開く。それより前の失敗は
    // 標準エラー出力だけ（サービスでは届かない）で、終了コードで区別する。

    fn log_error(&self, line: &str) {
        self.log.error(line);
    }

    fn run(&self, ctx: &ServiceContext) -> ServiceExit {
        match self.run_inner(ctx) {
            0 => ServiceExit::Success,
            code => ServiceExit::Specific(code),
        }
    }
}

#[cfg(test)]
#[path = "win_service_tests.rs"]
mod tests;
