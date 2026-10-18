//! `chronogazer-server` の Windows サービス（#392 B3-2b、2026-10-15。#623 PR-3 で
//! 設定モード ⇔ 運用モードの ACL の付け替えを当てはめた - 設計は
//! docs/chronogazer-service-design.md §5 と #623 の PR-1 の設計 §3）。**このファイル
//! 全体が Windows だけ**（`chronogazer-server.rs` が `#[cfg(windows)]` で読み込む）。
//!
//! - [`run_service_dispatcher`]（`run-service`）: 共有ランナー
//!   `banto_service_control::run_service` に [`ServerProgram`] を渡す。SCM の外で
//!   実行されたら製品共通の案内 2 行（`dispatch_failure_lines`）を標準エラー出力に
//!   出して終了コード 1。本体はまだ作っていないので、データのルートの下にも何も
//!   作らない。
//! - [`install`]・[`uninstall`]: 共有の手順（`install_service`・`uninstall_service`）
//!   と、インストール利用者の SID の記録（D1・D2）・ルートの用意（D3）。`install` の
//!   本体（`server_service::install_with`）・文言・終了コードは
//!   `chronogazer_core::server_service`。
//!
//! # 2 つの約束（設計 §3）
//!
//! 1. **締め終わるまでエンジンを動かさない**。緩い間に LocalSystem が書くのは、辿らずに
//!    開いて確かめたハンドル（A1 の診断ファイル・日次ログ）だけで、パスで書く（banto・
//!    SQLite・tstore に渡す）ことはしない。
//! 2. **緩め始めたら新しく開かず、ログにも書かない**。書くのは `relax_tree` が握った
//!    ルートから木を歩いて ID で開いたハンドルへの記述子だけ。
//!
//! # 起動（運用モードへ。[`ServerProgram::run_inner`]、設計 §3.1 の 13 段）
//!
//! 1. Redirection Guard を有効にする（結果を覚え、ログを開いた後に 1 行）。
//! 2. 開始の引数を見る（`ServiceContext::start_args`、信頼できない入力）: 空ならこの順序、
//!    `["relax-only"]` ちょうどなら緩めるだけの開始（下）、それ以外は**何も変えずに**
//!    終了コード 3（`server_service::parse_start_args`）。
//! 3. 付け替える相手を決める（D1 の記録 → `ModeAccess::for_users`。ルートに触る前）。
//! 4. ルートを辿らずに開いて握る（`open_root`、`Mode::Guarded`。無ければ**設定モードの形**で
//!    作る。相手が決まらないなら作らない）。
//! 5. A1 の排他（握ったルートの中に診断ファイルを書く `acquire_for_db_in`）。取れなければ 2。
//! 6. `logs\` を握り（無ければ設定モードの形で作る）、日次ログを辿らずに開いて標準出力・
//!    標準エラー出力を向け直す。開けなければ 3。「運用モードへの切り替えを開始」を書く。
//! 7. 相手が決まらなければ `NoReader` を書いて 4（何も変えていない）。
//! 8. 1 周目（`scan_tree`。断ったら何も変えずに 4、一覧をログに）。
//! 9. 2 周目（`close_tree`。止まったら `relax_tree` で緩め直して 4、緩め直せなければ 5）。
//!    木を歩くのは専用のスレッド（スタックを明示）で、tokio のワーカーでは歩かない。
//! 10. 残りの管理するフォルダを**運用モードの形**で開き、各フォルダ（`logs` も）に印を置く
//!     （改ざんを見つけたら緩め直して 5、I/O なら緩め直して 3）。
//! 11. セキュリティの変化の見張り（`SecurityWatch`）と定期の確かめ直し（`recheck`、60 秒）を
//!     始める。どちらかが落ちたら、エンジンを止め・DB を閉じ・**締めたまま**終了コード 5。
//! 12. 今の起動（`start::start_server`: DB → Hub のキー → エンジン）。DB のパスは
//!     `reopen_path_checked`、`run\endpoint.json` は握った `run\` にハンドル基準、Hub のキーは
//!     `FileSecretStore::open_in`。動き始めたら Running を報告し、完了の行（件数・所要時間）。
//! 13. 4〜12 の間は START_PENDING の checkpoint を進める（`report_start_progress`、1 秒ごと）。
//!     この間 SCM は STOP を 1061 で断る。
//!
//! # 停止（SCM の STOP = 設定モードへ。設計 §3.2）
//!
//! 新しいバックアップの受け付けを止めて進行中の物の完了を待つ（打ち切らない。待つ間は
//! `report_stop_progress`、30 分で進めるのをやめる）→ エンジンを止め DB を閉じる
//! （`StartedServer::shutdown`）→ 見張りを止める → listener の停止を裏に残していたら終わる
//! のを待つ（最大 `LISTENER_SETTLE_WAIT`。終わらなければ緩めずに 5）→ 最後の行「緩め始めます」
//! → ログを閉じ、向け直しも外す（NUL へ）→ `relax_tree`（進み具合で `report_stop_progress`）
//! → ハンドルを閉じ A1 を返す → 0（後始末が不完全なら 1）、緩められなければ 5。後始末が
//! 時間切れなら緩めずに 5。
//!
//! PRESHUTDOWN（OS の終了）・改ざんで止まるとき・異常終了は**締めたまま**（運用モードは
//! 再起動をまたいで続く。モード = ACL の状態）。
//!
//! # `relax-only`（設計 §3.3）
//!
//! ルートを握る（無ければ 0）→ A1（取れなければ 2）→ `logs` を開ければログ → 相手
//! （決まらなければ 5）→ 最後の行 → ログを閉じる → `relax_tree`（`report_start_progress`）→
//! **Running を報告せずに** 0／5。締める・エンジンを動かす道には入らない。
//!
//! # テスト
//!
//! ACL の付け替えの部品（[`AclOps`]）を差し替え、`Mode::Plain` のルート（どの OS でも
//! `std::fs`）の上で**順序と段ごとの終了コード**を偽の失敗の注入で総当たりにする。本物の
//! 締める・緩めるは `banto-safe-root` の Windows の実物テスト（CI の管理者）。
//!
//! **コンソールの Ctrl のハンドラーは登録しない**（`chronogazer-server.rs` の
//! モジュール doc「Windows サービス」）。止める合図は SCM からだけ来る。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use banto_hub_bootstrap::KeyStore;
use banto_safe_root::guard::KnownFolders;
use banto_safe_root::mode::{
    AbortReason, CloseAbort, MarkerError, MarkerOutcome, Progress, RecheckFailure, RelaxReport,
    ScanRefusal, ScanReport, StopSignal as TreeStop,
};
use banto_safe_root::{
    enable_redirection_guard, open_root, DirAccess, InstallUserSid, ManagedDir, ManagedSpec, Mode,
    ModeAccess, RedirectionGuard, RootRefusal, RootSpec, SafeRoot, SafeRootError,
};
use banto_service_control::{
    dispatch_failure_lines, print_console_lines, read_service_parameter, run_service,
    uninstall_service, ServiceContext, ServiceExit, ServiceProgram, StopSignal,
};
use chronogazer_core::instance_lock::{acquire_for_db_in, ChronoGazerHost, InstanceLockFailure};
use chronogazer_core::key_store::open_key_store_in;
use chronogazer_core::paths::{resolve_data_root_from_env, DataLayout, DB_FILE_NAME};
use chronogazer_core::server_host::{
    self, LockedServer, StartFailure, StartedServer, EXIT_ALREADY_RUNNING, EXIT_LEFT_TIGHT,
    EXIT_NOT_OPERATIONAL, EXIT_OK, EXIT_START_FAILED, SERVER_LABEL,
};
use chronogazer_core::server_service::{
    acl_changed_line, after_close_abort, after_marker_error, backup_wait_line, close_abort_line,
    exit_after_relax_back, exit_after_scan_refusal, external_data_dir_line, install_result_lines,
    install_with, listener_settled_line, listener_still_open_keeps_tight_line,
    mode_switch_begin_line, mode_switch_done_line, no_reader_line, parse_start_args,
    redirection_guard_line, relax_allowed_after_shutdown, relax_begin_line, relax_only_begin_line,
    scan_refused_lines, service_exit_for, service_flush_wait, service_running_line,
    service_starting_line, service_stop_budget, service_stop_done_line, service_stop_hint_line,
    service_stop_reason_line, service_stopped_line, shutdown_incomplete_keeps_tight_line,
    stop_progress_allowed, stop_signal_label, tampered_line, uninstall_lines,
    unknown_start_arg_line, StartArgs, INSTALL_USER_SID_VALUE, SERVICE_EXPECTED_STOP,
    SERVICE_LOG_FLUSH_WAIT,
};
use chronogazer_core::shell_launch::SERVICE_NAME;

use crate::start::{self, StartHooks, StartSpec, Started};
use crate::{Flush, LineSink, OpenedLog};

/// データのルートを決める（本番は `start::resolve_layout`。テストは一時ルート）。
type LayoutSource = Box<dyn Fn() -> Result<DataLayout, i32> + Send + Sync>;
/// 記録したインストール利用者の SID を読む（本番は `Parameters`。テストは偽物 -
/// テストは HKLM を読み書きしない）。
type InstallUserReader = Box<dyn Fn() -> Result<Option<String>, String> + Send + Sync>;
/// 握った `logs\` に日次ログを開いて向け直す（本番は `crate::open_log_in`。テストは
/// テストのプロセスの標準ハンドルを差し替えない偽物）。
type LogOpener = Box<dyn Fn(Arc<ManagedDir>) -> Result<OpenedLog, String> + Send + Sync>;
/// ルートにしてはいけない既知のフォルダ（本番はマシンの既知のフォルダ。テストは空）。
type KnownFoldersSource = Box<dyn Fn() -> Result<KnownFolders, String> + Send + Sync>;
/// Redirection Guard を有効にする（本番は `enable_redirection_guard`。テストは呼ばない -
/// テストのプロセス全体に効いてしまうため）。
type RedirectionGuardSource = Box<dyn Fn() -> RedirectionGuard + Send + Sync>;

/// START_PENDING の報告の wait hint（設計 §3.8）。
const START_WAIT_HINT: Duration = Duration::from_secs(10);
/// 進み具合で checkpoint を進める間隔（1 秒ごと。設計 §3.8）。
const PROGRESS_INTERVAL: Duration = Duration::from_secs(1);
/// STOP で、裏に残した listener の停止（`ShutdownReport::listener_stop_abandoned`）が終わるのを
/// 待つ上限（オーナーレビュー P1）。banto の graceful 停止（最大 5 秒）に、適用が slot の lock を
/// 持っている分の余裕を足した。これを過ぎても終わらなければ緩めずに 5。
const LISTENER_SETTLE_WAIT: Duration = Duration::from_secs(10);
/// 木を歩くスレッドのスタック（`MAX_DEPTH` 64 段 × 4 フレームで足りる大きさ。#681 の S-5）。
const WALK_STACK: usize = 16 * 1024 * 1024;
/// 定期の確かめ直しの間隔（設計 §2.9 (d) の案 60 秒）。
const RECHECK_INTERVAL: Duration = Duration::from_secs(60);
/// 止める合図・改ざんの印を見る間隔（運用中の待ち）。
const WAIT_POLL: Duration = Duration::from_millis(200);
/// バックアップの完了を待つ間の刻み（STOP_PENDING の checkpoint を進める間隔）。
const BACKUP_WAIT_TICK: Duration = Duration::from_secs(5);
/// バックアップの完了を待っている旨をログに書く間隔。
const BACKUP_WAIT_LOG: Duration = Duration::from_secs(60);

/// 管理するフォルダの key（`open_managed` の spec）。
const LOGS_KEY: &str = "logs";
const RUN_KEY: &str = "run";
const HUB_KEYS_KEY: &str = "hub-keys";

/// 日次ログの置き場（締める前に開く。設計 §3.1 の 6）。
const LOGS_SPEC: ManagedSpec = ManagedSpec {
    key: LOGS_KEY,
    parent: None,
    name: "logs",
    create: true,
};

/// 締めた後に開く残りの管理するフォルダ（設計 §6.1。`data.dir` が外部でも `data` は握る -
/// 無ければ作って締めた形で置くだけで、外部の置き場には触れない）。
const MANAGED_SPECS: [ManagedSpec; 8] = [
    ManagedSpec {
        key: "data",
        parent: None,
        name: "data",
        create: true,
    },
    ManagedSpec {
        key: RUN_KEY,
        parent: None,
        name: "run",
        create: true,
    },
    ManagedSpec {
        key: "backups",
        parent: None,
        name: "backups",
        create: true,
    },
    ManagedSpec {
        key: "backups-db",
        parent: Some("backups"),
        name: DB_FILE_NAME,
        create: true,
    },
    ManagedSpec {
        key: "audit-spool",
        parent: None,
        name: "audit-spool",
        create: true,
    },
    ManagedSpec {
        key: "audit-spool-db",
        parent: Some("audit-spool"),
        name: DB_FILE_NAME,
        create: true,
    },
    ManagedSpec {
        key: "secrets",
        parent: None,
        name: "secrets",
        create: true,
    },
    ManagedSpec {
        key: HUB_KEYS_KEY,
        parent: Some("secrets"),
        name: "hub-keys",
        create: true,
    },
];

/// `run-service`: SCM のディスパッチャを起動して [`ServerProgram`] を動かす。SCM の外
/// （人が手で実行した）なら案内の 2 行を出して 1。
pub(crate) fn run_service_dispatcher() -> i32 {
    match run_service(ServerProgram::production()) {
        Ok(()) => server_host::EXIT_OK,
        Err(err) => {
            for line in dispatch_failure_lines(SERVER_LABEL, &err) {
                eprintln!("{line}");
            }
            1
        }
    }
}

// --- ACL の付け替えの部品（差し替え口） ------------------------------------------------

/// セキュリティの変化の見張り（本番は `banto_safe_root::mode::SecurityWatch`）。
pub(crate) trait SecurityWatcher: Send {
    /// 次の変化を `timeout` まで待つ。
    fn wait(&mut self, timeout: Duration) -> Result<banto_safe_root::mode::WatchEvent, String>;
    /// 別のスレッドから止める口。
    fn stopper(&self) -> Box<dyn Fn() + Send + Sync>;
}

/// ACL の付け替えの部品（本番は `banto-safe-root` の `PinnedDir`。テストは偽物で、
/// `Mode::Plain` のルートの上で順序と失敗の注入だけ）。どれもログを出さない。
pub(crate) trait AclOps: Send + Sync {
    /// ルートの握り方（本番 `Guarded`、テスト `Plain`）。
    fn root_mode(&self) -> Mode;
    /// 1 周目（設計 §2.3）。
    fn scan_tree(
        &self,
        root: &SafeRoot,
        stop: &dyn Fn() -> TreeStop,
        progress: &mut dyn FnMut(&Progress),
    ) -> Result<ScanReport, ScanRefusal>;
    /// 2 周目（設計 §2.4）。
    fn close_tree(
        &self,
        root: &SafeRoot,
        mode: &ModeAccess,
        stop: &dyn Fn() -> TreeStop,
        progress: &mut dyn FnMut(&Progress),
    ) -> Result<Progress, CloseAbort>;
    /// 緩める（設計 §2.5）。
    fn relax_tree(
        &self,
        root: &SafeRoot,
        mode: &ModeAccess,
        progress: &mut dyn FnMut(&Progress),
    ) -> Result<RelaxReport, SafeRootError>;
    /// 印のファイル（設計 §2.9 (a)）。
    fn ensure_marker(&self, dir: &ManagedDir) -> Result<MarkerOutcome, MarkerError>;
    /// 確かめ直し（設計 §2.9 (d)）。
    fn recheck(&self, dir: &ManagedDir, expected: &DirAccess) -> Result<(), RecheckFailure>;
    /// 見張り（設計 §2.9 (c)）。`None` なら見張らない（テスト）。
    fn watch(&self, root: &SafeRoot) -> Result<Option<Box<dyn SecurityWatcher>>, String>;
}

/// 本番: `banto-safe-root` の部品そのもの。
struct RealAcl;

impl SecurityWatcher for banto_safe_root::mode::SecurityWatch {
    fn wait(&mut self, timeout: Duration) -> Result<banto_safe_root::mode::WatchEvent, String> {
        banto_safe_root::mode::SecurityWatch::wait(self, timeout).map_err(|err| err.to_string())
    }

    fn stopper(&self) -> Box<dyn Fn() + Send + Sync> {
        let stop = banto_safe_root::mode::SecurityWatch::stopper(self);
        Box::new(move || stop.stop())
    }
}

impl AclOps for RealAcl {
    fn root_mode(&self) -> Mode {
        Mode::Guarded
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
        let pinned = root.pinned().ok_or("ルートを握っていません")?;
        let watch = pinned.watch_security().map_err(|err| err.to_string())?;
        Ok(Some(Box::new(watch)))
    }
}

// --- 本体 ------------------------------------------------------------------------------

/// ChronoGazer のサービス本体（共有ランナーに渡す）。SCM とのやりとり（制御ハンドラ、
/// Running → StopPending → Stopped の報告、panic）は共有ランナーが持つ。
pub(crate) struct ServerProgram {
    layout: LayoutSource,
    read_install_user: InstallUserReader,
    open_log: LogOpener,
    known_folders: KnownFoldersSource,
    redirection_guard: RedirectionGuardSource,
    acl: Arc<dyn AclOps>,
    recheck_interval: Duration,
    /// ログを開いた後の、向け直しの後始末（[`ServerProgram::flush`]）。
    flush: Mutex<Option<Flush>>,
    /// ログを開いた後の、パイプを通さない記録先（[`ServiceProgram::log_error`]）。
    record: Mutex<Option<LineSink>>,
    /// テスト専用: STOP の後始末の結果を**時間切れ**に偽る（Codex の指摘 P1 の「緩めない」
    /// 経路を、本物のエンジンの予算を使い切らずに踏むため）。
    #[cfg(test)]
    fake_stop_timed_out: AtomicBool,
    /// テスト専用: STOP の後始末の結果を「listener の停止を待ち切れず裏に残した」に偽る
    /// （オーナーレビュー P1）。本物のエンジンは残していないので、`fake_listener_stops_unsettled`
    /// を立てなければ「後から終わったのを確かめた」になり緩める。
    #[cfg(test)]
    fake_listener_stop_abandoned: AtomicBool,
    /// テスト専用: 裏に残した listener の停止が待っても終わらなかったことに偽る。
    #[cfg(test)]
    fake_listener_stops_unsettled: AtomicBool,
}

/// [`ServerProgram::new`] に渡す部品の組（本番とテストで差し替える物）。
pub(crate) struct ProgramParts {
    pub layout: LayoutSource,
    pub read_install_user: InstallUserReader,
    pub open_log: LogOpener,
    pub known_folders: KnownFoldersSource,
    pub redirection_guard: RedirectionGuardSource,
    pub acl: Arc<dyn AclOps>,
    pub recheck_interval: Duration,
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

/// 運用中の見張りと定期の確かめ直し（設計 §2.9 (c)(d)）。drop で止める。
struct Watchers {
    stop_watch: Option<Box<dyn Fn() + Send + Sync>>,
    stop_recheck: Option<mpsc::Sender<()>>,
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

/// 握った物（ルート・管理するフォルダ）。終わりまで持ち、`relax_tree` の後に閉じる。
struct Held {
    root: Arc<SafeRoot>,
    logs: Arc<ManagedDir>,
    run: Arc<ManagedDir>,
    rest: Arc<banto_safe_root::ManagedSet>,
}

impl ServerProgram {
    fn production() -> Self {
        Self::new(ProgramParts {
            layout: Box::new(start::resolve_layout),
            read_install_user: Box::new(|| {
                read_service_parameter(SERVICE_NAME, INSTALL_USER_SID_VALUE)
                    .map_err(|err| err.to_string())
            }),
            open_log: Box::new(crate::open_log_in),
            known_folders: Box::new(|| {
                use banto_safe_root::guard::MachineEnvironment;
                banto_safe_root::guard::win::RegistryMachineEnvironment.known_folders()
            }),
            redirection_guard: Box::new(enable_redirection_guard),
            acl: Arc::new(RealAcl),
            recheck_interval: RECHECK_INTERVAL,
        })
    }

    fn new(parts: ProgramParts) -> Self {
        Self {
            layout: parts.layout,
            read_install_user: parts.read_install_user,
            open_log: parts.open_log,
            known_folders: parts.known_folders,
            redirection_guard: parts.redirection_guard,
            acl: parts.acl,
            recheck_interval: parts.recheck_interval,
            flush: Mutex::new(None),
            record: Mutex::new(None),
            #[cfg(test)]
            fake_stop_timed_out: AtomicBool::new(false),
            #[cfg(test)]
            fake_listener_stop_abandoned: AtomicBool::new(false),
            #[cfg(test)]
            fake_listener_stops_unsettled: AtomicBool::new(false),
        }
    }

    /// ログの書き出しを `wait` まで待つ（ログを開いていなければ何もしない）。
    fn flush(&self, wait: Duration) {
        if let Some(flush) = self
            .flush
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
        {
            flush(wait);
        }
    }

    /// 1 行を日次ログへ（開いていれば `write_through`、開いていなければ標準出力）。
    fn say(&self, line: &str) {
        let record = self
            .record
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        match record {
            Some(record) => record(line),
            None => println!("{line}"),
        }
    }

    /// ログを開いた直後の登録（`start_server` の `log_opened` と同じ）。
    fn attach_log(&self, flush: Flush, record: Option<LineSink>) {
        *self.flush.lock().unwrap_or_else(|p| p.into_inner()) = Some(flush);
        *self.record.lock().unwrap_or_else(|p| p.into_inner()) = record;
    }

    /// **緩め始める前の最後の仕事**（設計 §3.2 の 5）: 書き出しを待ち（向け直しの残りを流しきり）、日次ログを閉じ（以後 `write_line` は
    /// 何もしない）、標準出力・標準エラー出力の向け直しを外して NUL へ向ける。
    /// これ以後、この本体は `say` も `println!` もしない（しても NUL へ行く）。
    fn close_log(&self, log: Option<&Arc<Mutex<server_host::ServerLog>>>, wait: Duration) {
        // 先に向け直しを流しきる（`println!` の残りを `ServerLog` がまだ開いている間に書かせる）。
        self.flush(wait);
        if let Some(log) = log {
            if let Ok(mut log) = log.lock() {
                log.close();
            }
        }
        *self.record.lock().unwrap_or_else(|p| p.into_inner()) = None;
        if let Err(err) = crate::console::redirect_stdio_to_nul() {
            // ここでは何も書けない（書いてはいけない）。標準ハンドルは元に戻っており、
            // Session 0 ではどこにも届かない。
            let _ = err;
        }
    }

    /// 1 周目・2 周目・緩めるを**専用のスレッド**（スタックを明示）で走らせる（#681 の S-5）。
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

    /// 締めた分を緩め直す（2 周目が止まった・印で改ざんを見つけた・管理するフォルダを開けない）。
    /// ログは閉じてから呼ぶ（約束 2）。戻り値は緩め直せたか。
    fn relax_back(&self, ctx: &ServiceContext, root: &SafeRoot, mode: &ModeAccess) -> bool {
        let mut last = Instant::now();
        let mut progress = |_: &Progress| {
            if last.elapsed() >= PROGRESS_INTERVAL {
                last = Instant::now();
                ctx.report_start_progress(START_WAIT_HINT);
            }
        };
        self.on_walk_thread("chronogazer-relax-back", || {
            self.acl.relax_tree(root, mode, &mut progress).is_ok()
        })
    }

    fn run_inner(&self, ctx: &ServiceContext) -> ServiceExit {
        ctx.set_expected_stop(SERVICE_EXPECTED_STOP);
        // 1. Redirection Guard（結果はログを開いた後に 1 行）。
        let redirection = (self.redirection_guard)();

        // 2. 開始の引数（信頼できない入力。決まった値との完全一致だけ）。
        match parse_start_args(ctx.start_args()) {
            StartArgs::Normal => {}
            StartArgs::RelaxOnly => return self.relax_only(ctx),
            StartArgs::Unknown(arg) => {
                eprintln!("{}", unknown_start_arg_line(&arg));
                return service_exit_for(EXIT_START_FAILED);
            }
        }

        // 3. 付け替える相手（D1）。決まらない理由は、ログを開いてから書く。
        let (mode, reader_problem) = self.mode_access();

        // 4. ルート（辿らずに開いて握る。無ければ設定モードの形で作る。相手が無ければ作らない）。
        let layout = match (self.layout)() {
            Ok(layout) => layout,
            Err(code) => return service_exit_for(code),
        };
        let root = match self.open_root(&layout, mode.as_ref(), mode.is_some()) {
            Ok(root) => Arc::new(root),
            // 相手が決まらず、ルートも無い: 何も作れないのでログも無く終了コードだけ（設計 §3.1 の 7）。
            Err(RootOpenError::Refused(RootRefusal::Absent)) if mode.is_none() => {
                eprintln!("{}", no_reader_line());
                return service_exit_for(EXIT_NOT_OPERATIONAL);
            }
            Err(error) => {
                eprintln!(
                    "{SERVER_LABEL}: データのルート（{}）を使えません: {error}",
                    layout.root().display()
                );
                return service_exit_for(EXIT_START_FAILED);
            }
        };

        // 5. A1 の排他（握ったルートの中に診断ファイル）。取れなければ何も開かずに 2。
        let instance_lock = match acquire_for_db_in(root.as_dir(), ChronoGazerHost::Service) {
            Ok(guard) => guard,
            Err(failure) => {
                eprintln!("{SERVER_LABEL}: 起動できませんでした: {failure}");
                return service_exit_for(match failure {
                    InstanceLockFailure::AlreadyRunning { .. } => EXIT_ALREADY_RUNNING,
                    _ => EXIT_START_FAILED,
                });
            }
        };
        ctx.report_start_progress(START_WAIT_HINT);

        // ランタイム（Stopped の報告の後で、残りのタスクを待たずに落とす）。
        let runtime = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => ctx.hold_until_stopped(BackgroundRuntime(Some(runtime))),
            Err(err) => {
                eprintln!("{SERVER_LABEL}: 非同期ランタイムを作れませんでした: {err}");
                return service_exit_for(EXIT_START_FAILED);
            }
        };

        // 6. 日次ログ（`logs\` を設定モードの形で握り、辿らずに開く）。開けなければ 3。
        let logs = match self.open_logs(&root, mode.as_ref().map(ModeAccess::setting), true) {
            Ok(Some(logs)) => logs,
            Ok(None) => unreachable!("create: true なら Absent は返らない"),
            Err(reason) => {
                eprintln!("{SERVER_LABEL}: ログの置き場（logs）を開けませんでした: {reason}");
                return service_exit_for(EXIT_START_FAILED);
            }
        };
        let (log, flush, record) = match (self.open_log)(logs.clone()) {
            Ok(opened) => opened,
            Err(reason) => {
                eprintln!("{SERVER_LABEL}: {reason}");
                return service_exit_for(EXIT_START_FAILED);
            }
        };
        self.attach_log(flush, record);
        let switch_started = Instant::now();
        self.say(&service_starting_line());
        self.say(&redirection_guard_line(redirection));
        self.say(&mode_switch_begin_line());

        // 7. 相手が決まらない: 何も変えずに 4。
        let Some(mode) = mode else {
            if let Some(problem) = reader_problem {
                self.say(&format!("{SERVER_LABEL}: {problem}"));
            }
            self.say(&no_reader_line());
            return self.finish_without_engine(EXIT_NOT_OPERATIONAL, log.as_ref());
        };

        // 8. 1 周目（読むだけ。断ったら何も変えずに 4）。
        let stop = || tree_stop(ctx);
        let progress = Throttle::new(ctx);
        let scanned = self.on_walk_thread("chronogazer-scan", || {
            self.acl.scan_tree(&root, &stop, &mut progress.callback())
        });
        match scanned {
            Ok(report) => self.say(&format!(
                "{SERVER_LABEL}: データのフォルダを確かめました（{} 件。締められない物はありません）",
                report.examined
            )),
            Err(refusal) => {
                match &refusal {
                    ScanRefusal::Problems(problems) => {
                        for line in scan_refused_lines(problems) {
                            self.say(&line);
                        }
                    }
                    ScanRefusal::Io(error) => self.say(&format!(
                        "{SERVER_LABEL}: データのフォルダを確かめられませんでした（終了コード {EXIT_START_FAILED}。何も変えていません）: {error}"
                    )),
                    ScanRefusal::Stopped(signal) => self.say(&format!(
                        "{SERVER_LABEL}: 確かめている途中で止める合図（{signal:?}）を受けたため、何も変えずに起動を中止します"
                    )),
                }
                return self.finish_without_engine(exit_after_scan_refusal(&refusal), log.as_ref());
            }
        }

        // 9. 2 周目（締める。止まったら緩め直して 4／5）。
        let closed = self.on_walk_thread("chronogazer-close", || {
            self.acl
                .close_tree(&root, &mode, &stop, &mut progress.callback())
        });
        let tightened = match closed {
            Ok(progress) => progress,
            Err(CloseAbort { written, reason }) => {
                self.say(&close_abort_line(written, &reason));
                let plan = after_close_abort(&reason);
                if !plan.relax_back {
                    self.say(&format!(
                        "{SERVER_LABEL}: OS の終了のため緩め直さずに終わります（締めかけのまま。次の起動か relax-only で戻ります）"
                    ));
                    return self.finish_without_engine(plan.exit_if_relaxed, log.as_ref());
                }
                self.say(&format!(
                    "{SERVER_LABEL}: 締めた分を緩め直します（結果は終了コードで: {} = 戻せた、{} = 戻せなかった）",
                    plan.exit_if_relaxed, plan.exit_if_left_tight
                ));
                self.close_log(log.as_ref(), SERVICE_LOG_FLUSH_WAIT);
                let relaxed = self.relax_back(ctx, &root, &mode);
                return service_exit_for(exit_after_relax_back(relaxed, plan.exit_if_relaxed));
            }
        };
        self.say(&format!(
            "{SERVER_LABEL}: データのフォルダを運用モードの形に締めました（確かめた物 {} 件・書いた物 {} 件）",
            tightened.examined, tightened.written
        ));

        // 10. 残りの管理するフォルダ（運用モードの形で作る）と印。
        let mut rest = match root.open_managed(&MANAGED_SPECS, Some(mode.operation())) {
            Ok(set) => set,
            Err(stop) => {
                self.say(&format!(
                    "{SERVER_LABEL}: 管理するフォルダを開けませんでした（締めた分を緩め直します。終了コード {EXIT_START_FAILED}、戻せなければ {EXIT_LEFT_TIGHT}）: {stop}"
                ));
                self.close_log(log.as_ref(), SERVICE_LOG_FLUSH_WAIT);
                let relaxed = self.relax_back(ctx, &root, &mode);
                return service_exit_for(exit_after_relax_back(relaxed, EXIT_START_FAILED));
            }
        };
        let run = Arc::new(rest.take(RUN_KEY).expect("spec にある"));
        let hub_keys = rest.take(HUB_KEYS_KEY).expect("spec にある");
        let rest = Arc::new(rest);
        {
            let mut marked: Vec<(&str, &ManagedDir)> = vec![
                (LOGS_KEY, logs.as_ref()),
                (RUN_KEY, run.as_ref()),
                (HUB_KEYS_KEY, &hub_keys),
            ];
            for key in rest.keys() {
                marked.push((key, rest.get(key).expect("keys から")));
            }
            for (key, dir) in marked {
                if let Err(error) = self.acl.ensure_marker(dir) {
                    let (tampered, exit) = after_marker_error(&error);
                    if tampered {
                        self.say(&tampered_line(key, &error));
                    } else {
                        self.say(&format!(
                            "{SERVER_LABEL}: フォルダ「{key}」に印を置けませんでした（終了コード {exit}）: {error}"
                        ));
                    }
                    self.say(&format!(
                        "{SERVER_LABEL}: 締めた分を緩め直します（戻せなければ終了コード {EXIT_LEFT_TIGHT}）"
                    ));
                    self.close_log(log.as_ref(), SERVICE_LOG_FLUSH_WAIT);
                    let relaxed = self.relax_back(ctx, &root, &mode);
                    return service_exit_for(exit_after_relax_back(relaxed, exit));
                }
            }
        }
        ctx.report_start_progress(START_WAIT_HINT);

        // DB のパス（確かめてから渡す）と、A1 を引き継いだ `LockedServer`。
        let db = match root.as_dir().reopen_path_checked(
            DB_FILE_NAME,
            &["-wal", "-shm", "-journal"],
            mode.operation(),
        ) {
            Ok(path) => path.into_path(),
            Err(error) => {
                self.say(&format!(
                    "{SERVER_LABEL}: 設定 DB のパスを確かめられませんでした（締めた分を緩め直します）: {error}"
                ));
                self.close_log(log.as_ref(), SERVICE_LOG_FLUSH_WAIT);
                let relaxed = self.relax_back(ctx, &root, &mode);
                return service_exit_for(exit_after_relax_back(relaxed, EXIT_START_FAILED));
            }
        };
        let locked = server_host::lock_held(
            layout.clone(),
            ChronoGazerHost::Service,
            instance_lock,
            db,
            root.final_path().to_path_buf(),
            run.clone(),
        );
        let key_store = open_key_store_in(hub_keys, mode.operation().clone());
        let held = Held {
            root: root.clone(),
            logs: logs.clone(),
            run: run.clone(),
            rest: rest.clone(),
        };

        // 11. 見張りと定期の確かめ直し。
        let tamper = Arc::new(Tamper::default());
        let mut watchers = match self.start_watchers(&held, &mode, tamper.clone()) {
            Ok(watchers) => watchers,
            Err(reason) => {
                self.say(&format!(
                    "{SERVER_LABEL}: セキュリティの見張りを始められませんでした（締めた分を緩め直します）: {reason}"
                ));
                self.close_log(log.as_ref(), SERVICE_LOG_FLUSH_WAIT);
                let relaxed = self.relax_back(ctx, &root, &mode);
                return service_exit_for(exit_after_relax_back(relaxed, EXIT_START_FAILED));
            }
        };

        // 12. 今の起動（DB → Hub のキー → エンジン）。
        let hooks = ServiceHooks {
            program: self,
            ctx,
            aborted: AtomicBool::new(false),
            locked: Mutex::new(Some(locked)),
            key_store: Mutex::new(Some(key_store)),
            opened_log: Mutex::new(Some((log.clone(), Box::new(|_| {}), None))),
        };
        let started = start::start_server(
            &runtime,
            StartSpec {
                layout: &layout,
                host: ChronoGazerHost::Service,
                access: &chronogazer_core::paths::DirAccessSource::Given(mode.operation().clone()),
            },
            &hooks,
        );
        let Started { server, .. } = match started {
            Ok(started) => started,
            Err(code) => {
                // 起動の失敗・中止: エンジンは動いていない。締めた分を緩め直す（止める合図の
                // 中止は失敗ではない = 0。PRESHUTDOWN は締めたまま）。
                watchers.stop();
                let aborted = hooks.aborted.load(Ordering::SeqCst);
                let preshutdown = ctx
                    .stop_notice()
                    .is_some_and(|notice| notice.signal == StopSignal::OsPreshutdown);
                let exit = if aborted { EXIT_OK } else { code };
                if preshutdown {
                    self.say(&format!(
                        "{SERVER_LABEL}: OS の終了のため緩め直さずに終わります（運用モードのまま）"
                    ));
                    self.flush(service_flush_wait(
                        &ctx.stop_notice().expect("preshutdown"),
                        Instant::now(),
                    ));
                    return service_exit_for(exit);
                }
                self.say(&format!(
                    "{SERVER_LABEL}: エンジンを動かさずに終わるため、締めた分を緩め直します（戻せなければ終了コード {EXIT_LEFT_TIGHT}）"
                ));
                self.close_log(log.as_ref(), SERVICE_LOG_FLUSH_WAIT);
                let relaxed = self.relax_back(ctx, &root, &mode);
                return service_exit_for(exit_after_relax_back(relaxed, exit));
            }
        };

        // Running を報告してから、上限の無い処理（古いログの削除・案内）へ。
        let log_prune = log.as_ref().and_then(|log| {
            log.lock()
                .ok()
                .and_then(|mut log| log.set_retention(server.log_retention_days()))
        });
        if ctx.report_running() {
            self.say(&service_running_line());
        }
        self.say(&mode_switch_done_line(&tightened, switch_started.elapsed()));
        if let Some(external) = external_data_dir(&server, &root) {
            self.say(&external_data_dir_line(&external));
        }
        if let Some(prune) = log_prune {
            crate::spawn_log_prune(prune);
        }
        if ctx.stop_notice().is_none() && tamper.get().is_none() {
            runtime.block_on(crate::announce(&server, &service_stop_hint_line()));
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

        match notice {
            // 改ざん: エンジンを止め、DB を閉じ、**締めたまま** 5。
            None => {
                watchers.stop();
                let what = tamper.get().unwrap_or_default();
                self.say(&acl_changed_line(&what));
                let summary = runtime.block_on(server.shutdown(server_host::INTERRUPT_BUDGET));
                self.report_shutdown(&summary, None);
                let exit = service_exit_for(EXIT_LEFT_TIGHT);
                self.say(&service_stopped_line(exit));
                self.flush(SERVICE_LOG_FLUSH_WAIT);
                drop(held);
                exit
            }
            // PRESHUTDOWN: 今の流れのまま締めたまま。
            Some(notice) if notice.signal == StopSignal::OsPreshutdown => {
                watchers.stop();
                let budget = service_stop_budget(&notice, Instant::now());
                self.say(&service_stop_reason_line(notice.signal, budget));
                let summary = runtime.block_on(server.shutdown(budget));
                self.report_shutdown(&summary, Some(notice.signal));
                let exit = service_exit_for(summary.exit_code());
                self.say(&service_stopped_line(exit));
                self.flush(service_flush_wait(&notice, Instant::now()));
                drop(held);
                exit
            }
            // STOP = 設定モードへ（設計 §3.2）。
            Some(notice) => {
                let budget = service_stop_budget(&notice, Instant::now());
                self.say(&service_stop_reason_line(notice.signal, budget));
                let stop_started = Instant::now();
                // 1. 新しいバックアップの受け付けを止め、進行中の完了を待つ（打ち切らない）。
                let mut last_log = Duration::ZERO;
                runtime.block_on(server.drain_backups(BACKUP_WAIT_TICK, &mut |elapsed| {
                    if stop_progress_allowed(stop_started.elapsed()) {
                        ctx.report_stop_progress();
                    }
                    if elapsed.saturating_sub(last_log) >= BACKUP_WAIT_LOG {
                        last_log = elapsed;
                        self.say(&backup_wait_line(elapsed));
                    }
                }));
                // 2〜3. エンジンを止め、DB を閉じる（`run\endpoint.json` も消す）。
                #[cfg(not(test))]
                let summary = runtime.block_on(server.shutdown(budget));
                #[cfg(test)]
                let summary = {
                    let mut summary = runtime.block_on(server.shutdown(budget));
                    if self.fake_stop_timed_out.load(Ordering::SeqCst) {
                        summary.report.timed_out = true;
                    }
                    if self.fake_listener_stop_abandoned.load(Ordering::SeqCst) {
                        summary.report.listener_stop_abandoned = true;
                    }
                    summary
                };
                self.report_shutdown(&summary, Some(notice.signal));
                // 4. 見張りと定期の確かめ直しを止める。
                watchers.stop();
                // 3b. listener の停止を上限（1 秒）で待ち切れず裏に残していたら、終わったのを
                // 確かめる（オーナーレビュー P1）: listener の graceful 停止は最大 5 秒で、
                // その間は STOP の前から開いていた要求がまだ届く。閉じた pool に当たった
                // 認証は監査の退避ファイルを利用者の木の下へパスで書くので、要求が残り
                // うるうちは緩めない。待つ間も STOP_PENDING の checkpoint は進める。
                let listener_stops_settled =
                    if summary.report.listener_stop_abandoned && !summary.report.timed_out {
                        let settle_started = Instant::now();
                        let settled = loop {
                            let settled = runtime.block_on(
                                server
                                    .engine()
                                    .await_abandoned_listener_stops(PROGRESS_INTERVAL),
                            );
                            if settled || settle_started.elapsed() >= LISTENER_SETTLE_WAIT {
                                break settled;
                            }
                            if stop_progress_allowed(stop_started.elapsed()) {
                                ctx.report_stop_progress();
                            }
                        };
                        #[cfg(test)]
                        let settled =
                            settled && !self.fake_listener_stops_unsettled.load(Ordering::SeqCst);
                        if settled {
                            self.say(&listener_settled_line());
                        }
                        settled
                    } else {
                        false
                    };
                // 後始末が予算内に終わらなかった（pool を閉じていない・listener を止める
                // タスクが残っている）、または裏に残した listener の停止が終わったのを確かめ
                // られなかったなら**緩めない**（Codex の指摘 P1・オーナーレビュー P1: エンジンの
                // 資源や要求が残ったまま利用者の書き込みを戻すと、約束 2 と同時アクセスの防ぎ
                // に反する）。締めたまま 5 で終わり、デスクトップの案内（relax-only）で戻す。
                if !relax_allowed_after_shutdown(&summary.report, listener_stops_settled) {
                    if summary.report.timed_out {
                        self.say(&shutdown_incomplete_keeps_tight_line());
                    } else {
                        self.say(&listener_still_open_keeps_tight_line(LISTENER_SETTLE_WAIT));
                    }
                    let exit = service_exit_for(EXIT_LEFT_TIGHT);
                    self.say(&service_stopped_line(exit));
                    self.flush(SERVICE_LOG_FLUSH_WAIT);
                    drop(server);
                    drop(held);
                    return exit;
                }
                // 5. 最後の行 → ログを閉じ、向け直しを外す。
                self.say(&relax_begin_line());
                self.close_log(log.as_ref(), SERVICE_LOG_FLUSH_WAIT);
                // 6. 緩める（ここから先は新しく開かず、ログにも書かない）。
                let mut last = Instant::now();
                let mut progress = |_: &Progress| {
                    if last.elapsed() >= PROGRESS_INTERVAL
                        && stop_progress_allowed(stop_started.elapsed())
                    {
                        last = Instant::now();
                        ctx.report_stop_progress();
                    }
                };
                let relaxed = self.on_walk_thread("chronogazer-relax", || {
                    self.acl.relax_tree(&root, &mode, &mut progress).is_ok()
                });
                // 7. 握った物を閉じ、A1 を返す（`server` が持つ）。
                drop(server);
                drop(held);
                service_exit_for(exit_after_relax_back(relaxed, summary.exit_code()))
            }
        }
    }

    /// 後始末の結果の行（#599 の endpoint・最終の書き出しの失敗・完了）。
    fn report_shutdown(
        &self,
        summary: &server_host::ServerShutdownSummary,
        signal: Option<StopSignal>,
    ) {
        if let Some(line) = summary.endpoint_diagnostic() {
            self.say(&line);
        }
        if let Some(err) = &summary.report.collect_stop_error {
            self.say(&format!(
                "{SERVER_LABEL}: 最終の書き出しに失敗しました: {err}"
            ));
        }
        // 予算切れはエンジンが自分で 1 行出している。
        if !summary.report.timed_out {
            if let Some(signal) = signal {
                self.say(&service_stop_done_line(signal));
            }
        }
    }

    /// エンジンを動かさずに終わる（何も変えていない: 1 周目の前・1 周目で断った）。ログの
    /// 書き出しを待ってから終了コードを返す。
    fn finish_without_engine(
        &self,
        code: i32,
        log: Option<&Arc<Mutex<server_host::ServerLog>>>,
    ) -> ServiceExit {
        let exit = service_exit_for(code);
        self.say(&service_stopped_line(exit));
        let _ = log;
        self.flush(SERVICE_LOG_FLUSH_WAIT);
        exit
    }

    /// D1 の記録から付け替える相手を決める（`ModeAccess::for_users`）。決まらない理由も返す。
    fn mode_access(&self) -> (Option<ModeAccess>, Option<String>) {
        match (self.read_install_user)() {
            Ok(Some(raw)) => match InstallUserSid::parse(&raw) {
                Ok(sid) => (ModeAccess::for_users([sid]), None),
                Err(_) => (
                    None,
                    Some(format!(
                        "インストール利用者の記録が利用者のアカウントの SID ではありません（{raw:?}）"
                    )),
                ),
            },
            Ok(None) => (None, Some("インストール利用者の記録がありません".to_string())),
            Err(err) => (
                None,
                Some(format!("インストール利用者の記録を読めませんでした: {err}")),
            ),
        }
    }

    /// ルートを握る（設計 §3.1 の 4）。`create` なら無ければ設定モードの形で作る。
    fn open_root(
        &self,
        layout: &DataLayout,
        mode: Option<&ModeAccess>,
        create: bool,
    ) -> Result<SafeRoot, RootOpenError> {
        let known = (self.known_folders)().map_err(RootOpenError::KnownFolders)?;
        open_root(&RootSpec {
            expected: layout.root().to_path_buf(),
            known,
            owners: mode.map(|m| m.users().to_vec()).unwrap_or_default(),
            create_access: mode.map(|m| m.setting().clone()),
            create,
            mode: self.acl.root_mode(),
        })
        .map_err(RootOpenError::Refused)
    }

    /// `logs\` を握る（`create` なら無ければ `access` の形で作る）。無くて作らないなら `Ok(None)`、
    /// 開けなければ理由。
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
            Ok(mut set) => Ok(Some(Arc::new(set.take(LOGS_KEY).expect("spec にある")))),
            Err(stop) if matches!(stop.reason, banto_safe_root::root::ManagedRefusal::Absent) => {
                Ok(None)
            }
            Err(stop) => Err(stop.to_string()),
        }
    }

    /// 見張り（`SecurityWatch`）と定期の確かめ直しを始める（設計 §2.9 (c)(d)）。
    fn start_watchers(
        &self,
        held: &Held,
        mode: &ModeAccess,
        tamper: Arc<Tamper>,
    ) -> Result<Watchers, String> {
        let mut watchers = Watchers {
            stop_watch: None,
            stop_recheck: None,
            threads: Vec::new(),
        };
        if let Some(mut watch) = self.acl.watch(&held.root)? {
            watchers.stop_watch = Some(watch.stopper());
            let tamper = tamper.clone();
            let thread = std::thread::Builder::new()
                .name("chronogazer-acl-watch".to_string())
                .spawn(move || loop {
                    use banto_safe_root::mode::WatchEvent;
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
        // 定期の確かめ直し（握ったフォルダとルート）。`hub-keys` は保存先が使うたびに確かめる。
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        watchers.stop_recheck = Some(stop_tx);
        let interval = self.recheck_interval;
        let expected = mode.operation().clone();
        let (root, logs, run, rest) = (
            held.root.clone(),
            held.logs.clone(),
            held.run.clone(),
            held.rest.clone(),
        );
        let acl = self.acl.clone();
        let thread = std::thread::Builder::new()
            .name("chronogazer-acl-recheck".to_string())
            .spawn(move || loop {
                match stop_rx.recv_timeout(interval) {
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    _ => return,
                }
                let mut targets: Vec<(&str, &ManagedDir)> = vec![
                    ("root", root.as_dir()),
                    (LOGS_KEY, logs.as_ref()),
                    (RUN_KEY, run.as_ref()),
                ];
                for key in rest.keys() {
                    targets.push((key, rest.get(key).expect("keys から")));
                }
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

    /// 緩めるだけの開始（設計 §3.3「`relax-only` の配線と手順」）。Running を報告しない。
    fn relax_only(&self, ctx: &ServiceContext) -> ServiceExit {
        let (mode, reader_problem) = self.mode_access();
        let layout = match (self.layout)() {
            Ok(layout) => layout,
            Err(code) => return service_exit_for(code),
        };
        // ルートが無ければ緩める物が無い。
        let root = match self.open_root(&layout, mode.as_ref(), false) {
            Ok(root) => root,
            Err(RootOpenError::Refused(RootRefusal::Absent)) => return ServiceExit::Success,
            Err(error) => {
                eprintln!(
                    "{SERVER_LABEL}: データのルート（{}）を使えません: {error}",
                    layout.root().display()
                );
                return service_exit_for(EXIT_START_FAILED);
            }
        };
        // A1（デスクトップのエンジンが動いている間には緩めない）。
        let instance_lock = match acquire_for_db_in(root.as_dir(), ChronoGazerHost::Service) {
            Ok(guard) => guard,
            Err(failure) => {
                eprintln!("{SERVER_LABEL}: relax-only を始められません: {failure}");
                return service_exit_for(match failure {
                    InstanceLockFailure::AlreadyRunning { .. } => EXIT_ALREADY_RUNNING,
                    _ => EXIT_START_FAILED,
                });
            }
        };
        ctx.report_start_progress(START_WAIT_HINT);
        // `logs` は作らない。開ければログに書く（開けなければログなしで続ける）。
        let log = match self.open_logs(&root, None, false) {
            Ok(Some(logs)) => match (self.open_log)(logs) {
                Ok((log, flush, record)) => {
                    self.attach_log(flush, record);
                    log
                }
                Err(_) => None,
            },
            _ => None,
        };
        self.say(&service_starting_line());
        let Some(mode) = mode else {
            if let Some(problem) = reader_problem {
                self.say(&format!("{SERVER_LABEL}: {problem}"));
            }
            self.say(&format!(
                "{SERVER_LABEL}: relax-only: 付け替える相手（インストール利用者）を決められないため、緩めずに終わります（終了コード {EXIT_LEFT_TIGHT}）。chronogazer-elev.exe grant-data-acl で戻してください"
            ));
            let exit = service_exit_for(EXIT_LEFT_TIGHT);
            self.flush(SERVICE_LOG_FLUSH_WAIT);
            drop(instance_lock);
            return exit;
        };
        self.say(&relax_only_begin_line());
        self.close_log(log.as_ref(), SERVICE_LOG_FLUSH_WAIT);
        let relaxed = self.relax_back(ctx, &root, &mode);
        drop(root);
        drop(instance_lock);
        // Running を報告せずに終わる（START_PENDING から STOPPED）。
        service_exit_for(exit_after_relax_back(relaxed, EXIT_OK))
    }
}

/// ルートを握れなかった理由。
#[derive(Debug)]
enum RootOpenError {
    /// 既知のフォルダの場所を取れない（ルートを決めずに止める）。
    KnownFolders(String),
    /// `open_root` が断った。
    Refused(RootRefusal),
}

impl std::fmt::Display for RootOpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::KnownFolders(reason) => {
                write!(f, "既知のフォルダの場所を取れませんでした: {reason}")
            }
            Self::Refused(refusal) => write!(f, "{refusal}"),
        }
    }
}

/// SCM の止める合図を木を歩く部品の合図に写す（締めている間は SCM からは来ない。口として配線）。
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

/// `data.dir` が外部（ルートの外）なら、その場所（起動ログの 1 行用。#659）。
fn external_data_dir(server: &StartedServer, root: &SafeRoot) -> Option<PathBuf> {
    let data_dir = server.engine().services().collect.data_dir().to_path_buf();
    if data_dir.starts_with(root.final_path()) || data_dir.starts_with(root.path()) {
        None
    } else {
        Some(data_dir)
    }
}

impl ServiceProgram for ServerProgram {
    fn service_name(&self) -> &str {
        SERVICE_NAME
    }

    fn label(&self) -> &str {
        SERVER_LABEL
    }

    // `open_log` は既定（何もしない）: ログは A1 の排他を取った後に開く（B1-5）。

    fn log_error(&self, line: &str) {
        let record = self
            .record
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        match record {
            Some(record) => record(line),
            None => eprintln!("{line}"),
        }
    }

    fn run(&self, ctx: &ServiceContext) -> ServiceExit {
        self.run_inner(ctx)
    }
}

/// サービスの起動の順序の差し込み（`start::start_server`）。A1・ログは本体が先に済ませて
/// いるので、ここは**既に済んだ物を渡す**だけ。
struct ServiceHooks<'a> {
    program: &'a ServerProgram,
    ctx: &'a ServiceContext,
    /// 段の間で止める合図を見つけた（起動の中止。失敗ではない）。
    aborted: AtomicBool,
    /// 既に A1 を取って組んだ `LockedServer`。
    locked: Mutex<Option<LockedServer>>,
    /// 握った `hub-keys` で開いた Hub のキーの保存先（開けなければ起動を止める）。
    key_store: Mutex<Option<Result<Arc<dyn KeyStore>, String>>>,
    /// 既に開いた日次ログ（向け直しも済み）。
    opened_log: Mutex<Option<OpenedLog>>,
}

impl StartHooks for ServiceHooks<'_> {
    fn stop_requested(&self) -> Option<&'static str> {
        let notice = self.ctx.stop_notice()?;
        // 合図を返すと `start_server` は必ず起動を中止する。
        self.aborted.store(true, Ordering::SeqCst);
        Some(stop_signal_label(notice.signal))
    }

    fn lock(
        &self,
        _layout: DataLayout,
        _host: ChronoGazerHost,
        _access: &chronogazer_core::paths::DirAccessSource,
    ) -> Result<LockedServer, StartFailure> {
        Ok(self
            .locked
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
            .expect("start_server は 1 回だけ"))
    }

    fn open_log(&self, _layout: &DataLayout) -> Result<OpenedLog, String> {
        Ok(self
            .opened_log
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
            .expect("start_server は 1 回だけ"))
    }

    fn log_opened(&self, _flush: Flush, _record: Option<LineSink>) {
        // 本体が既に登録している（`attach_log`）。
        self.ctx.report_start_progress(START_WAIT_HINT);
    }

    fn key_store(
        &self,
        _layout: &DataLayout,
        _access: &chronogazer_core::paths::DirAccessSource,
    ) -> Result<(Arc<dyn KeyStore>, Option<String>), String> {
        self.key_store
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
            .expect("start_server は 1 回だけ")
            .map(|store| (store, None))
    }

    fn finish_start_failure(&self, code: i32) -> i32 {
        if self.aborted.load(Ordering::SeqCst) {
            self.program
                .say(&service_stopped_line(ServiceExit::Success));
        } else {
            self.program.say(&format!(
                "{SERVER_LABEL}: Windows サービスの起動に失敗しました（終了コード {code}）"
            ));
        }
        // 書き出しは呼ぶ側（本体）が緩め直しの前に `close_log` で待つ。
        code
    }

    fn before_open(&self) {
        self.ctx.report_start_progress(START_WAIT_HINT);
    }
}

/// tokio のランタイムを、drop のときに残りのタスクを待たずに落とす入れ物
/// （`shutdown_background`）。止めたエンジンの残りのタスクで、Stopped を報告した後の
/// プロセスの終わりを待たせない（コンソールの `mem::forget(runtime)` と同じ扱い）。
struct BackgroundRuntime(Option<tokio::runtime::Runtime>);

impl std::ops::Deref for BackgroundRuntime {
    type Target = tokio::runtime::Runtime;

    fn deref(&self) -> &Self::Target {
        self.0.as_ref().expect("drop の前だけ使う")
    }
}

impl Drop for BackgroundRuntime {
    fn drop(&mut self) {
        if let Some(runtime) = self.0.take() {
            runtime.shutdown_background();
        }
    }
}

// --- 登録・解除 ---------------------------------------------------------------

/// `install`: サービスを登録し、インストール利用者の SID を記録し（D1・D2）、ルートが
/// 無ければその SID で作る（D3）。終了コード（失敗は 1）を返す。本体は
/// `server_service::install_with`（#392 B3-3b で lib へ移した。`chronogazer-elev.exe
/// service-install` と共有）。ここで決めるのは、登録する実行ファイル（自分）・
/// データのルートと Program Files の場所（このプロセスの環境）だけで、出す行と
/// 終了コードは移す前と同じ。
pub(crate) fn install(install_user_sid: Option<InstallUserSid>) -> i32 {
    let executable = match std::env::current_exe() {
        Ok(path) => path,
        Err(err) => {
            eprintln!("{SERVER_LABEL}: 自身の実行ファイルパスの取得に失敗しました: {err}");
            return 1;
        }
    };
    let result = install_with(
        executable,
        install_user_sid,
        &DataLayout::from_env(),
        std::env::var_os("ProgramFiles").map(PathBuf::from),
    );
    let (lines, code) = install_result_lines(&result);
    print_console_lines(&lines);
    code
}

/// `uninstall`: 登録を解除する（`Parameters` もサービスのキーと一緒に消える）。データは
/// 残す。終了コード（失敗は 1）を返す。
pub(crate) fn uninstall() -> i32 {
    let result = uninstall_service(SERVICE_NAME);
    print_console_lines(&uninstall_lines(&result, &resolve_data_root_from_env()));
    i32::from(result.is_err())
}

#[cfg(test)]
#[path = "service/tests.rs"]
mod tests;
