//! 画面の無い本番用の host `chronogazer-server`（#392 段階 B の B1-5、2026-10-13。
//! 設計は docs/chronogazer-service-design.md §3.1(b)・§4 の B1-5）の、OS に
//! 依存しない部分。
//!
//! `src/bin/chronogazer-server.rs` は「引数・環境変数を読む → ログを開く →
//! コンソールの Ctrl を受ける → ここの [`start`] → 止める合図を待つ →
//! [`StartedServer::shutdown`]」を組むだけで、判断（待ち受けの形・ルートの検証・
//! 起動の失敗の案内・ログのファイル名と保持）はここに純関数で置き、全分岐を
//! どの OS でもテストする（`tests/chronogazer_server.rs` はできた bin を
//! プロセスとして動かす）。
//!
//! # データ（決定 7・H9）
//!
//! ルートは [`crate::paths`]（既定 `%ProgramData%\ChronoGazer\`、`CHRONOGAZER_ROOT`
//! で上書き）。**`%APPDATA%` は読まない・移さない**。旧データ（デスクトップの
//! `%APPDATA%\dev.tyaro.chronogazer\`）の案内もしない（決定 3 は「利用者として動く
//! シェルとインストーラ」の担当。この bin はサービス（LocalSystem）と同じ形で
//! 動き、ログに 1 行出しても、どの利用者の旧データかが分からず誤解を招く -
//! サービスでは `%APPDATA%` が `systemprofile` を指す）。
//!
//! **相対パスのルートは拒否する**（[`validate_root`]）。相対の基準はプロセスの
//! 作業ディレクトリで、サービス（B3）では `System32`、コンソールでは起動した
//! 場所ごとに変わる - 同じ設定のつもりで別の DB を作り始めるより、起動時に
//! 止めて直してもらう方がよい。
//!
//! # 待ち受け（[`ListenPlan`]、決定 9）
//!
//! - **loopback の listener は常に開く**（`127.0.0.1:<server.port>`、既定 8721）。
//!   シェル（B2）の probe と、同じ PC の画面のため。開けなければ起動を止める。
//! - `server.enabled` で、かつ認証モードとの組み合わせが許される
//!   （[`crate::settings::lan_autostart_allowed`]、デスクトップと同じ判定）ときだけ、
//!   LAN 用を `<server.bind>:<server.port>` に足す。LAN は開けなくても起動を
//!   続ける（収集を止めない。デスクトップの LAN の自動起動と同じ扱い）。
//! - LAN の bind が loopback を含む（`0.0.0.0`）なら 1 本にまとめる（同じポートに
//!   ワイルドカードと特定のアドレスを別々の socket で bind すると、Windows では
//!   後の方が `WSAEADDRINUSE` になる）。bind が loopback そのもの（`127.0.0.1` など・
//!   `localhost`）なら LAN の分は無い（loopback の 1 本だけ）。それ以外（特定の
//!   LAN の IP）は 2 本。
//! - 1 本にまとめた `0.0.0.0` を開けない（別のプロセスが `<LAN の IP>:port` だけを
//!   持っている等）ときは、`127.0.0.1:port` に落として起動を続け、LAN を開けなかった
//!   理由を残す（[`ListenPlan::loopback_fallback`]、#598 のレビュー。「loopback は常に、
//!   LAN は best-effort」の約束をまとめた場合にも守る）。
//! - `POST /api/auth/setup`（最初の管理者の作成）は、常時の listener への**この PC からの
//!   要求**（peer と Host の両方が loopback）だけで開ける（`OpenedEngine::
//!   with_loopback_local_access`）。デスクトップの初回セットアップ（その PC の前の人が
//!   最初の管理者になる）と同じ範囲。`0.0.0.0` にまとめた 1 本でも、LAN からの要求は
//!   peer が LAN なので閉じる（2026-10-14 オーナー決定で、B1-5 の「loopback に bind
//!   できた listener だけ」から要求ごとの確認に変えた。以前は `0.0.0.0` のときこの PC
//!   からも作れなかった）。LAN 専用の listener と、後から LAN の listener を入れ替える
//!   経路（決定 9）のルーターでは常に閉じる。
//! - **この PC からの接続（`localDesktop` の grant、#392 B2-1、[`crate::local_grant`]）**も
//!   同じく常時の listener への、この PC からの要求だけで開く（未初期化の間は admin、初期化後は設定 `auth.local_grant`、
//!   既定 off）。setup は置き換えずに併存させ、どちらにも **Host の確認**（peer と Host の
//!   両方が loopback。DNS rebinding 対策）をかける。B1-5 の既知の制限「LAN に出していない
//!   間は同じ PC の任意のプロセスが最初の管理者を作れる」は、デスクトップの初回セット
//!   アップと同じ範囲として受け入れ（grant の未初期化の admin も同じ性質）、ブラウザーで
//!   開いた外のページから作られる経路だけを Host の確認で塞いだ。
//! - LAN の公開設定は REST（`PUT /api/settings/server`、[`crate::server_admin`]）から
//!   変えられ、LAN の listener はその場で入れ替わる（決定 9）。常時の listener の形が
//!   変わる設定（ポート・`0.0.0.0`）は保存だけで、次の起動から。
//!
//! # 起動の失敗（[`StartFailure`]）
//!
//! デスクトップの対処の窓と同じ分類（[`crate::startup_recovery::classify`]）で
//! 理由を分け、**対処はコンソールに文で示すだけで自動では行わない**
//! （[`console_guidance`]）。終了コードは A1 の使用中 = 2、それ以外 = 3
//! （B3 のサービスの終了コードと同じ。[`EXIT_ALREADY_RUNNING`]）。
//!
//! # KeyStore（決定 4・H10）
//!
//! Hub の API キーは `banto_secret_store::FileSecretStore<DpapiMachine>` で
//! `<root>\secrets\hub-keys\` に置く（[`open_key_store`]）。ディレクトリの ACL は
//! SYSTEM・Administrators にフル、加えて**このプロセスを動かしている利用者**
//! （コンソールを開いた人）に「変更」。LocalSystem で動いている（`psexec -s` など。
//! B3 のサービスと同じ）ときは利用者の ACE を付けない。理由は [`open_key_store`]。
//! 開けない（ACL が違う・別の利用者が先に作った）ときは起動を止めず、Hub の
//! 操作のたびにその理由で失敗する KeyStore を渡す（[`FailedKeyStore`] - PLC の
//! 収集を止めない。保存先の中身には触れないので fail closed のまま）。
//! `attach_hub` は常に。`BANTO_HUB_MEMORY_KEYS` は読まない（`banto-serve` 専用）。
//!
//! # ログ（[`ServerLog`]）
//!
//! `<root>\logs\chronogazer-server-YYYY-MM-DD.log`（ローカルの日付）。各行の先頭に
//! `[YYYY-MM-DD HH:MM:SS]`。日付が変わったら次のファイルへ移り、保持日数
//! （設定 `log.retention_days`、既定 [`DEFAULT_LOG_RETENTION_DAYS`] 日、`0` 以下は
//! 無制限）より古い**自分の名前の形のファイルだけ**消す。保持日数は DB を開いた後に
//! 読むので、それまでは消さない（既定値で先に消すと、長く残す設定の利用者のログを
//! 消してしまう）。標準出力・標準エラー出力をこのファイルへ向け直すのは bin の
//! 担当（Windows の `SetStdHandle`。core と依存 crate が `eprintln!` を直接使う）。
//!
//! 古いファイルの削除は [`ServerLog`] の中では行わず、[`LogPrune`] として呼び出し側へ
//! 返す（#598 の Copilot の指摘）。ディレクトリの列挙と削除には上限が無い（UNC の
//! ルート・大きなフォルダ）ので、ログの `Mutex` を持ったまま行うと、終了の最後の
//! 行の記録と書き出し（同じ `Mutex` を取る）を待たせ、close 系の 4 秒の予算を
//! 超えうる。呼び出し側はロックを手放してから別のスレッドで走らせる
//! （[`LogPrune::spawn`]）。消すのは今日の分より古い自分の名前の形のファイルだけで、
//! 今書いているファイルには触れないので、削除の途中でプロセスが終わってもログは
//! 壊れない（消し残しは次の機会に消える）。

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use banto_hub_bootstrap::KeyStore;
use banto_safe_root::ManagedDir;
use banto_server::ServerConfig;
use banto_tstore::{Clock, LocalDate};

use crate::endpoint::{
    clear_endpoint, clear_endpoint_in, write_endpoint, write_endpoint_in, EndpointClear,
    EndpointInfo,
};
use crate::engine::{
    BackupGate, EngineConfig, EngineListen, EngineRuntime, EngineStartError, LockedEngine,
    OpenedEngine, RunningEngine, ShutdownReport,
};
use crate::instance_lock::{ChronoGazerHost, InstanceGuard};
use crate::paths::{DataLayout, DirAccessSource};
use crate::settings::{lan_autostart_allowed, AuthSettings, ServerSettings, SettingsService};
use crate::startup_recovery::{
    classify, classify_settings_error, plan, FailureKind, RecoveryChoice, WindowPlan,
};

/// ログ・標準エラー出力の接頭辞。
pub const SERVER_LABEL: &str = "chronogazer-server";

/// Ctrl-C・Ctrl-Break からの終了の予算（設計書 §3.1(b)）。
pub const INTERRUPT_BUDGET: Duration = Duration::from_secs(15);

/// コンソールのウィンドウを閉じた（`CTRL_CLOSE_EVENT`）・ログオフ・シャットダウンの
/// とき、後始末の完了を待つ上限。OS はハンドラーから戻るのを約 5 秒しか待たない
/// （過ぎるとプロセスを終わらせる）ので、ログの書き出しの分を残して 4 秒。
pub const CLOSE_WAIT: Duration = Duration::from_secs(4);

/// [`CLOSE_WAIT`] の中で、`run\endpoint.json` を消す・ログを書き出す分として
/// エンジンの終了の予算から差し引く余裕。
pub const CLOSE_MARGIN: Duration = Duration::from_millis(500);

/// 終了コード: 正常に止まった。
pub const EXIT_OK: i32 = 0;
/// 終了コード: 止まったが、後始末が予算内に終わらなかった・最終 flush に失敗した。
pub const EXIT_SHUTDOWN_INCOMPLETE: i32 = 1;
/// 終了コード: 同じデータを別のプロセスが使用中（A1。B3 のサービスと同じ値）。
pub const EXIT_ALREADY_RUNNING: i32 = 2;
/// 終了コード: それ以外の起動の失敗（B3 のサービスと同じ値）。
pub const EXIT_START_FAILED: i32 = 3;
/// 終了コード（サービスだけ。#623 PR-3、設計 §3.7）: 運用モードにできなかった - **何も変えずに
/// 断った・締めかけて緩め直した**（木にリンク／ハードリンク／EFS／開けない・使用中の子／
/// 深すぎる／印の名前を塞ぐフォルダがある、読み取りを付ける相手を決められない `NoReader`）。
/// 木は設定モードのまま。理由の細目は日次ログ。
pub const EXIT_NOT_OPERATIONAL: i32 = 4;
/// 終了コード（サービスだけ。#623 PR-3、設計 §3.7）: 締めたまま止まった、またはフォルダが
/// 外から変えられた（緩められなかった／印を置いた後の改ざん `Tampered`／運用中の記述子の
/// 変化 `AclChanged`／`relax-only` で相手を決められない）。デスクトップは `relax-only` の開始か
/// 昇格ヘルパーの修復を案内する。
pub const EXIT_LEFT_TIGHT: i32 = 5;
/// 終了コード: 引数が違う。
pub const EXIT_USAGE: i32 = 64;

/// ログの保持日数の設定キー。
pub const LOG_RETENTION_KEY: &str = "log.retention_days";
/// ログの保持日数の既定（設計書 §3.1(b) の初期案）。
pub const DEFAULT_LOG_RETENTION_DAYS: u32 = 30;
/// ログのファイル名の接頭辞（`chronogazer-server-YYYY-MM-DD.log`）。
pub const LOG_FILE_PREFIX: &str = "chronogazer-server-";
/// ログのファイル名の拡張子。
pub const LOG_FILE_SUFFIX: &str = ".log";
/// デスクトップ（シェル）が設定モードの間に書くログのファイル名の接頭辞
/// （`chronogazer-desktop-YYYY-MM-DD.log`。#623 PR-4、2026-10-18 オーナー決定 (3)）。
/// 書くのは `crate::mode_switch::append_desktop_log`（モードの切り替えの行）と、デスクトップの
/// 診断のログ（[`ServerLog::open_desktop`] + `crate::stdio_log`。2026-10-24 オーナー決定「案 A」）。
/// 古いファイルはサービスのログと同じ
/// 保持日数で [`LogPrune`] が消す（[`parse_log_file_date`] が両方の形を受ける）。
pub const DESKTOP_LOG_FILE_PREFIX: &str = "chronogazer-desktop-";

// --- ルート -------------------------------------------------------------------

/// データのルートを使えるか（モジュール doc「データ」）。相対パス（Windows では
/// ドライブ無しの `\foo` も含む）は理由を返す。
pub fn validate_root(root: &Path) -> Result<(), String> {
    if root.as_os_str().is_empty() {
        return Err("データのルートが空です".to_string());
    }
    if !root.is_absolute() {
        return Err(format!(
            "データのルート（{}）が絶対パスではありません。{} には絶対パス（例: D:\\ChronoGazer）を指定してください。\
             相対パスは起動した場所（サービスでは C:\\Windows\\System32）を基準にするため使えません。",
            root.display(),
            crate::paths::ROOT_ENV
        ));
    }
    // `.`・`..`（と点だけの名前）を含むパスは、OS が作るときに正規化して別の場所を
    // 指しうる（例 `…\ChronoGazer\child\..`: 親を先に普通の継承の ACL で作ってしまい、
    // 保護された DACL が付かない。PR #604 のレビュー）。副作用なしで断る。
    if crate::paths::has_dot_segment(root) {
        return Err(format!(
            "データのルート（{}）に「.」や「..」が含まれています。{} には「.」「..」を含まない絶対パス（例: D:\\ChronoGazer）を指定してください。",
            root.display(),
            crate::paths::ROOT_ENV
        ));
    }
    Ok(())
}

// --- 待ち受け -----------------------------------------------------------------

/// 待ち受けの形（モジュール doc「待ち受け」）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListenPlan {
    /// 常に開く listener。普通は `127.0.0.1`、LAN と 1 本にまとめたときは
    /// `0.0.0.0`。開けなければ起動を止める。
    pub loopback: ServerConfig,
    /// `loopback` を開けなかったときの代わり（`loopback` を `0.0.0.0` にまとめた
    /// ときだけ `127.0.0.1:port`）。
    pub loopback_fallback: Option<ServerConfig>,
    /// 足す LAN の listener（開けなくても起動を続ける）。
    pub lan: Option<ServerConfig>,
}

impl ListenPlan {
    /// 計画の上で LAN から届く listener があるか（実際に開けたかは
    /// [`StartedServer::exposes_lan`]）。
    pub fn exposes_lan(&self) -> bool {
        self.lan.is_some() || !bind_is_loopback(&self.loopback.bind)
    }
}

const LOOPBACK_BIND: &str = "127.0.0.1";

/// `bind` が loopback そのものか（`127.0.0.0/8`・`localhost`）。
fn bind_is_loopback(bind: &str) -> bool {
    let bind = bind.trim();
    if bind.eq_ignore_ascii_case("localhost") {
        return true;
    }
    matches!(bind.parse::<IpAddr>(), Ok(IpAddr::V4(v4)) if v4.is_loopback())
}

/// `bind` が IPv4 の全アドレス（`0.0.0.0`。loopback を含む）か。IPv6 は当面
/// 対象外（2026-09-29 オーナー決定）なので `::` は「特定のアドレス」と同じ扱い。
fn bind_is_ipv4_wildcard(bind: &str) -> bool {
    matches!(bind.trim().parse::<IpAddr>(), Ok(IpAddr::V4(v4)) if v4.is_unspecified())
}

/// 保存済みの設定から待ち受けの形を決める（純関数）。`lan_allowed` は
/// [`lan_autostart_allowed`]（`server.enabled` と認証モードの組み合わせ）。
pub fn listen_plan(server: &ServerSettings, lan_allowed: bool) -> ListenPlan {
    let port = server.port;
    let loopback = ServerConfig {
        bind: LOOPBACK_BIND.to_string(),
        port,
    };
    if !lan_allowed {
        return ListenPlan {
            loopback,
            loopback_fallback: None,
            lan: None,
        };
    }
    if bind_is_loopback(&server.bind) {
        return ListenPlan {
            loopback,
            loopback_fallback: None,
            lan: None,
        };
    }
    if bind_is_ipv4_wildcard(&server.bind) {
        return ListenPlan {
            loopback: ServerConfig {
                bind: server.bind.trim().to_string(),
                port,
            },
            loopback_fallback: Some(loopback),
            lan: None,
        };
    }
    ListenPlan {
        loopback,
        loopback_fallback: None,
        lan: Some(ServerConfig {
            bind: server.bind.clone(),
            port,
        }),
    }
}

/// 設定を読んで [`listen_plan`] を決める（[`lan_autostart_allowed`] を通す）。
pub fn listen_plan_from_settings(auth: &AuthSettings, server: &ServerSettings) -> ListenPlan {
    listen_plan(server, lan_autostart_allowed(auth, server))
}

// --- 起動の失敗 ---------------------------------------------------------------

/// 起動の失敗（コンソールに出して終了コードで返す）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartFailure {
    pub kind: FailureKind,
    /// 理由（利用者向け）。
    pub message: String,
    /// 対処の案内（[`console_guidance`]）。
    pub guidance: Vec<String>,
}

impl StartFailure {
    pub fn exit_code(&self) -> i32 {
        exit_code_for(self.kind)
    }
}

/// 起動の失敗の分類 → 終了コード。
pub fn exit_code_for(kind: FailureKind) -> i32 {
    match kind {
        FailureKind::AlreadyRunning => EXIT_ALREADY_RUNNING,
        _ => EXIT_START_FAILED,
    }
}

/// 起動の失敗の対処の案内（コンソール用の文。デスクトップの対処の窓の選択肢
/// [`plan`] を、利用者が手で行う手順に言い換える。**どれも自動では行わない**）。
pub fn console_guidance(kind: FailureKind, layout: &DataLayout) -> Vec<String> {
    let db = layout.db_path();
    let mut lines = Vec::new();
    match kind {
        FailureKind::AlreadyRunning => lines.push(
            "同じデータを使っている ChronoGazer（上の表示）を終了してから、もう一度起動してください。"
                .to_string(),
        ),
        FailureKind::LegacyDatabase => lines.push(
            "chronogazer-server は旧形式の DB を移行しません。上の案内に従ってください。".to_string(),
        ),
        FailureKind::DatabaseFromNewerVersion => lines.push(
            "新しい版の ChronoGazer で使った DB です。その版の ChronoGazer で起動してください（DB には触れていません）。"
                .to_string(),
        ),
        _ => {}
    }
    if let WindowPlan::Choices(choices) = plan(kind) {
        for choice in choices {
            let line = match choice {
                RecoveryChoice::RetryStoreCheck => Some(
                    "保存先を使えるようにしてから（ドライブの接続・フォルダの権限）、もう一度起動してください。"
                        .to_string(),
                ),
                RecoveryChoice::ChooseStoreDir | RecoveryChoice::ResetStoreDir => None,
                RecoveryChoice::RestoreBackup => Some(format!(
                    "バックアップから戻す: chronogazer-server を止めたまま、{} の中のバックアップを {} へ写してください（今の DB と隣の -wal・-shm は別の名前に変えて残してから）。",
                    layout.backups_dir().display(),
                    db.display()
                )),
                RecoveryChoice::RecreateDatabase => Some(format!(
                    "作り直す: {} と隣の -wal・-shm を別の名前に変えてから起動すると、空の設定 DB で起動します（時系列データ {} は残ります）。",
                    db.display(),
                    layout.data_dir().display()
                )),
                RecoveryChoice::Quit => None,
            };
            if let Some(line) = line {
                lines.push(line);
            }
        }
        if choices_need_store_setting(kind) {
            lines.push(
                "保存先の設定（data.dir）を変える手段は chronogazer-server にはまだありません。保存先を使えるようにするか、上の手順で DB を作り直してください。"
                    .to_string(),
            );
        }
    }
    lines.push(format!("データのルート: {}", layout.root().display()));
    lines
}

fn choices_need_store_setting(kind: FailureKind) -> bool {
    matches!(
        kind,
        FailureKind::StoreDirUnreadable | FailureKind::StoreDirUnwritable
    )
}

fn failure_from_engine(err: &EngineStartError, layout: &DataLayout) -> StartFailure {
    let kind = classify(err);
    StartFailure {
        kind,
        message: err.to_string(),
        guidance: console_guidance(kind, layout),
    }
}

// --- KeyStore -----------------------------------------------------------------

// #392 B2-4a: Hub の API キーの保存先を開く関数と、開けなかったときの代わりは
// デスクトップ（B2-4b）と共有するため `crate::key_store` へ移した。挙動は同じ。
pub use crate::key_store::{
    open_key_store, open_key_store_from, open_key_store_with, FailedKeyStore,
};

// --- 起動と終了 ---------------------------------------------------------------

/// [`start`] に渡すもの。
pub struct ServerOptions {
    pub layout: DataLayout,
    pub key_store: Arc<dyn KeyStore>,
    /// A1 の診断ファイル・`GET /api/host`・`run\endpoint.json` に出す host の種類
    /// （コンソールは [`ChronoGazerHost::Server`]、サービスは
    /// [`ChronoGazerHost::Service`]。#392 B3-2a）。
    pub host: ChronoGazerHost,
}

/// 動いている `chronogazer-server`。
pub struct StartedServer {
    engine: RunningEngine,
    endpoint_file: PathBuf,
    /// `run\endpoint.json` を握ったフォルダ基準で消す（サービス。#623 PR-3）。
    run_dir: Option<Arc<ManagedDir>>,
    plan: ListenPlan,
    log_retention_days: Option<u32>,
    endpoint_error: Option<String>,
}

/// [`StartedServer::shutdown`] の結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerShutdownSummary {
    pub report: ShutdownReport,
    /// `run\endpoint.json` の後始末の結果（#599）。
    pub endpoint: EndpointClear,
    /// `run\endpoint.json` の場所（[`Self::endpoint_diagnostic`] の表示用）。
    pub endpoint_file: PathBuf,
}

impl ServerShutdownSummary {
    /// `run\endpoint.json` を残したことを知らせる 1 行（消せなかった・読めなかった・
    /// 壊れていた。[`EndpointClear::diagnostic`]）。終了の記録で 1 回だけ出す。
    pub fn endpoint_diagnostic(&self) -> Option<String> {
        self.endpoint
            .diagnostic(&self.endpoint_file)
            .map(|line| format!("{SERVER_LABEL}: {line}"))
    }

    /// 終了コード（後始末が予算内に終わり、最終 flush が通れば 0）。
    ///
    /// `run\endpoint.json` を消せなかったことは**終了コードに入れない**（#599）。
    /// [`EXIT_SHUTDOWN_INCOMPLETE`] は「収集の後始末（予算・最終 flush）が終わらなかった」
    /// の意味で、サービスでは SCM へ異常の終わり（`server_service::service_exit_for`）
    /// として報告される。残ったファイルは書いたプロセス（自分）が生きていないので読む側
    /// （`endpoint::read_live_endpoint`）が古いものとして読み飛ばし、次の起動が置き換えを
    /// 試みる。データには影響しない。原因が属性・ACL のように続くものなら次の起動の
    /// 置き換えも失敗し（起動時に「書けなかった」の行が出る）、シェルはつなげないので、
    /// 知らせるログの 1 行（[`Self::endpoint_diagnostic`]）に手で消す案内を入れる。
    pub fn exit_code(&self) -> i32 {
        if self.report.timed_out || self.report.collect_stop_error.is_some() {
            EXIT_SHUTDOWN_INCOMPLETE
        } else {
            EXIT_OK
        }
    }
}

/// 保持日数の設定値の読み方（`0` 以下は無制限 = `None`、読めない・数でないなら既定）。
pub fn parse_log_retention(raw: Option<&str>) -> Option<u32> {
    match raw.map(str::trim) {
        None | Some("") => Some(DEFAULT_LOG_RETENTION_DAYS),
        Some(text) => match text.parse::<i64>() {
            Ok(days) if days <= 0 => None,
            Ok(days) => Some(u32::try_from(days).unwrap_or(u32::MAX)),
            Err(_) => Some(DEFAULT_LOG_RETENTION_DAYS),
        },
    }
}

/// 起動（[`open`] → [`OpenedServer::start`]。間に何も挟まないテスト用）。
pub async fn start(options: ServerOptions) -> Result<StartedServer, StartFailure> {
    let ServerOptions {
        layout,
        key_store,
        host,
    } = options;
    open(layout, host).await?.start(key_store).await
}

/// A1 の排他を取って DB を開いた `chronogazer-server`（[`open`]）。ルートの下に
/// 何かを作る処理（ログ・Hub のキーの保存先・`run\endpoint.json`）は、これを得た後
/// （= 排他を持っている間）に行う（#598 の Copilot の指摘: 先に作ると、同じルートで
/// 2 人目が起動したときに、排他で弾かれる側が `secrets\hub-keys\` を自分の ACL で
/// 作ってしまい、動いている側の Hub のキーの保存先が使えなくなりうる）。drop すると
/// 排他と pool を返す。
pub struct OpenedServer {
    opened: OpenedEngine,
    layout: DataLayout,
    host: ChronoGazerHost,
    run_dir: Option<Arc<ManagedDir>>,
}

/// [`lock`] と [`LockedServer::open`] を続けて行う（間に何も挟まないテスト用。
/// ルートの ACL の利用者は今のプロセスの利用者）。
pub async fn open(layout: DataLayout, host: ChronoGazerHost) -> Result<OpenedServer, StartFailure> {
    lock(layout, host, &DirAccessSource::CurrentProcess)?
        .open()
        .await
}

/// A1 の排他だけを取った `chronogazer-server`（[`lock`]）。bin はこの後に日次ログを
/// 開いてから [`LockedServer::open`]（リストア予約の適用・`init_db`）へ進む
/// （#598 のオーナーレビュー P2: DB を開けない理由・対処や、リストア予約の警告を
/// 日次ログに残す）。drop すると排他を返す。
pub struct LockedServer {
    locked: LockedEngine,
    layout: DataLayout,
    host: ChronoGazerHost,
    /// `run\endpoint.json` を握ったフォルダ基準で書く・消す（サービス。#623 PR-3）。
    run_dir: Option<Arc<ManagedDir>>,
}

/// 握ったルートの中で既に取った A1 の排他と、確かめてから作ったパス（DB は
/// `ManagedDir::reopen_path_checked`、`data_base` はルートの本当のパス）で
/// [`LockedServer`] を組む（サービス。#623 PR-3、設計 §3.1 の 5・12）。ルートは作らず、
/// 排他も取り直さない。`run_dir` は `run\endpoint.json` を書く・消す握ったフォルダ。
pub fn lock_held(
    layout: DataLayout,
    host: ChronoGazerHost,
    instance_lock: InstanceGuard,
    db_path: PathBuf,
    data_base: PathBuf,
    run_dir: Arc<ManagedDir>,
) -> LockedServer {
    let locked = EngineRuntime::lock_with(engine_config(data_base, db_path, host), instance_lock);
    LockedServer {
        locked,
        layout,
        host,
        run_dir: Some(run_dir),
    }
}

/// `chronogazer-server` のエンジンの設定（[`lock`]・[`lock_held`] で同じ）。
fn engine_config(data_base: PathBuf, db_path: PathBuf, host: ChronoGazerHost) -> EngineConfig {
    EngineConfig {
        drivers: EngineConfig::builtin_drivers(),
        data_base,
        db_path,
        // 本物は排他を取った後に `OpenedServer::start` が渡す（`with_key_store`）。
        key_store: Arc::new(FailedKeyStore::new(
            "Hub の API キーの保存先をまだ開いていません",
        )),
        attach_hub: true,
        host,
        listen: EngineListen::Disabled,
        allow_setup: false,
        seed_viewer_public: false,
        log_label: SERVER_LABEL,
        audit_origin: "rest",
        // 本番と同じ（デスクトップと同じく、保存設定を読めない・保存先に書けない
        // ときは起動を止める）。
        store_config_required: true,
        store_dir_check_required: true,
    }
}

impl LockedServer {
    pub fn layout(&self) -> &DataLayout {
        &self.layout
    }

    /// リストア予約の適用 → `init_db`。
    pub async fn open(self) -> Result<OpenedServer, StartFailure> {
        let LockedServer {
            locked,
            layout,
            host,
            run_dir,
        } = self;
        let opened = locked
            .open()
            .await
            .map_err(|err| failure_from_engine(&err, &layout))?;
        Ok(OpenedServer {
            opened,
            layout,
            host,
            run_dir,
        })
    }
}

/// 起動の最初の段: ルートを作り（ルートそのものだけ）、A1 の排他を取る（モジュール
/// doc）。排他を取れなければ、ルートの下には何も作らない（DB も）。
///
/// `host` は A1 の診断ファイル・`GET /api/host`・`run\endpoint.json` に出す種類、
/// `access` はルートを作るときの ACL の利用者の欄（#392 B3-2a。コンソールは
/// [`ChronoGazerHost::Server`] と [`DirAccessSource::CurrentProcess`]、サービスは
/// [`ChronoGazerHost::Service`] とインストール利用者の SID）。
pub fn lock(
    layout: DataLayout,
    host: ChronoGazerHost,
    access: &DirAccessSource,
) -> Result<LockedServer, StartFailure> {
    // 初回（`%ProgramData%\ChronoGazer` がまだ無い）はルートを作る。A1 の排他の
    // 診断ファイルと DB はその直下に置く。#392 B2-4a（O6）: 無いときだけ、保護された
    // DACL（SYSTEM・Administrators にフル、`access` の利用者に「変更」、Users なし）
    // で作る。既にあるルートには触らない（`crate::paths::create_root_dir`）。
    if let Err(err) = access.create_root_dir(&layout) {
        return Err(StartFailure {
            kind: FailureKind::Other,
            message: format!(
                "データのルート（{}）を作れませんでした: {err}",
                layout.root().display()
            ),
            guidance: console_guidance(FailureKind::Other, &layout),
        });
    }
    let db_path = layout.db_path();
    let locked = EngineRuntime::lock(engine_config(layout.root().to_path_buf(), db_path, host))
        .map_err(|err| failure_from_engine(&err, &layout))?;
    Ok(LockedServer {
        locked,
        layout,
        host,
        run_dir: None,
    })
}

impl OpenedServer {
    pub fn layout(&self) -> &DataLayout {
        &self.layout
    }

    /// 開いたまま閉じる（排他と pool を返す）。
    pub async fn close(self) {
        self.opened.close().await;
    }

    /// 設定 DB（bin のデバッグビルドだけのテスト用のポート `CHRONOGAZER_TEST_PORT` が
    /// 待ち受けを決める前に書く。#392 B2-2）。
    pub fn settings(&self) -> SettingsService {
        self.opened.settings()
    }

    /// 起動の後半: 設定から待ち受けの形 → エンジン（`key_store` を使う）→
    /// `run\endpoint.json`。失敗は分類して [`StartFailure`]。
    pub async fn start(self, key_store: Arc<dyn KeyStore>) -> Result<StartedServer, StartFailure> {
        let OpenedServer {
            opened,
            layout,
            host,
            run_dir,
        } = self;
        let opened = opened.with_key_store(key_store);
        start_opened(opened, layout, host, run_dir).await
    }
}

async fn start_opened(
    opened: OpenedEngine,
    layout: DataLayout,
    host: ChronoGazerHost,
    run_dir: Option<Arc<ManagedDir>>,
) -> Result<StartedServer, StartFailure> {
    let settings = opened.settings();
    let read = async {
        let auth = settings.auth_config().await?;
        let server = settings.server_config().await?;
        let retention = settings.get(LOG_RETENTION_KEY).await?;
        Ok::<_, banto_core::BantoError>((auth, server, retention))
    };
    let (auth, server, retention) = match read.await {
        Ok(values) => values,
        Err(err) => {
            opened.close().await;
            let kind = classify_settings_error(&err);
            return Err(StartFailure {
                kind,
                message: format!("設定 DB を読めませんでした: {err}"),
                guidance: console_guidance(kind, &layout),
            });
        }
    };
    let plan = listen_plan_from_settings(&auth, &server);
    let lan = match &plan.lan {
        Some(config) => EngineListen::BestEffort(config.clone()),
        None => EngineListen::Disabled,
    };
    let mut opened = opened
        .with_loopback(plan.loopback.clone())
        .with_listen(lan)
        .with_loopback_local_access();
    if let Some(fallback) = &plan.loopback_fallback {
        opened = opened.with_loopback_fallback(fallback.clone());
    }
    let engine = opened
        .start()
        .await
        .map_err(|err| failure_from_engine(&err, &layout))?;

    let endpoint_file = match &run_dir {
        Some(run) => run.final_path().join(crate::endpoint::ENDPOINT_FILE_NAME),
        None => layout.endpoint_file(),
    };
    let endpoint_error = match engine.loopback_addr().await {
        Some(addr) => {
            let info = EndpointInfo::for_current_process(
                addr.port(),
                engine.services().host.scope.clone(),
                host.as_str(),
            );
            // サービスは握った `run\` の中にハンドル基準で書く（パスを解決し直さない。
            // #623 PR-3）。コンソールは今までどおりパス。
            let written = match &run_dir {
                Some(run) => write_endpoint_in(run, &info),
                None => write_endpoint(&endpoint_file, &info),
            };
            written
                .err()
                .map(|err| format!("{} を書けませんでした: {err}", endpoint_file.display()))
        }
        None => Some("loopback の listener がありません".to_string()),
    };

    Ok(StartedServer {
        engine,
        endpoint_file,
        run_dir,
        plan,
        log_retention_days: parse_log_retention(retention.as_deref()),
        endpoint_error,
    })
}

impl StartedServer {
    pub fn engine(&self) -> &RunningEngine {
        &self.engine
    }

    pub fn plan(&self) -> &ListenPlan {
        &self.plan
    }

    /// 実際に LAN から届く listener が開いているか（常時の listener が loopback 以外の
    /// アドレス（`0.0.0.0`）に bind できた、または LAN の listener が開いている）。
    /// 最初の管理者の作成とこの PC からの接続は、この値に関係なく常時の listener への
    /// この PC からの要求で開く（2026-10-14 オーナー決定。モジュール doc「待ち受け」）。
    pub async fn exposes_lan(&self) -> bool {
        let loopback_exposed = self
            .engine
            .loopback_addr()
            .await
            .is_some_and(|addr| !addr.ip().is_loopback());
        loopback_exposed || self.engine.server_slot().local_addr().await.is_some()
    }

    /// 設定のログの保持日数（`None` は無制限）。
    pub fn log_retention_days(&self) -> Option<u32> {
        self.log_retention_days
    }

    /// `run\endpoint.json` を書けなかった理由。
    pub fn endpoint_error(&self) -> Option<&str> {
        self.endpoint_error.as_deref()
    }

    pub fn endpoint_file(&self) -> &Path {
        &self.endpoint_file
    }

    /// バックアップの REST の受け付けの門（[`BackupGate`]。#623 PR-3、設計 §3.2 の 1）。
    pub fn backup_gate(&self) -> &Arc<BackupGate> {
        self.engine.backup_gate()
    }

    /// 新しいバックアップの受け付けを止め、**進行中のバックアップ（REST の作成・restore の
    /// 適用）が終わるのを待つ**（決定 1。打ち切らない。#623 PR-3、設計 §3.2 の 1）。待つ間は
    /// `tick` の間隔ごとに `on_wait(待った時間)` を呼ぶ（呼ぶ側が STOP_PENDING の checkpoint を
    /// 進め、1 分ごとにログを書く）。何も進行していなければすぐ戻る。
    pub async fn drain_backups(&self, tick: Duration, on_wait: &mut dyn FnMut(Duration)) {
        let gate = self.backup_gate();
        gate.close();
        let started = std::time::Instant::now();
        while gate.inflight() > 0 {
            if tokio::time::timeout(tick, gate.wait_idle()).await.is_err() {
                on_wait(started.elapsed());
            }
        }
    }

    /// 終了。**先に** `run\endpoint.json` を消し（自分の pid のものだけ）、それから
    /// エンジンを止める。どちらも A1 の排他を持ったまま（排他は `StartedServer` の
    /// drop で返る。B1-4 のレビューで残った「終了時の後始末を A1 の保持中に行う」）。
    /// 先に消すのは、後始末が予算を超えてプロセスが打ち切られても、止まりかけの
    /// プロセスを指すファイルを残さないため（シェルの probe は止まりかけの
    /// listener に繋がらない）。同時に 2 回走らせないのは呼び出し側
    /// （`banto_exit_once::ExitCleanupOnce`）。
    pub async fn shutdown(&self, budget: Duration) -> ServerShutdownSummary {
        // 消せなくても（#599）エンジンの停止・最終 flush へ進む。
        let endpoint = match &self.run_dir {
            Some(run) => clear_endpoint_in(run, std::process::id()),
            None => clear_endpoint(&self.endpoint_file, std::process::id()),
        };
        let report = self.engine.shutdown(budget).await;
        ServerShutdownSummary {
            report,
            endpoint,
            endpoint_file: self.endpoint_file.clone(),
        }
    }
}

// --- ログ ---------------------------------------------------------------------

/// ログのファイル名（`chronogazer-server-YYYY-MM-DD.log`）。
pub fn log_file_name(date: LocalDate) -> String {
    dated_log_file_name(LOG_FILE_PREFIX, date)
}

/// デスクトップのログのファイル名（`chronogazer-desktop-YYYY-MM-DD.log`。#623 PR-4）。
pub fn desktop_log_file_name(date: LocalDate) -> String {
    dated_log_file_name(DESKTOP_LOG_FILE_PREFIX, date)
}

fn dated_log_file_name(prefix: &str, date: LocalDate) -> String {
    format!(
        "{prefix}{:04}-{:02}-{:02}{LOG_FILE_SUFFIX}",
        date.year, date.month, date.day
    )
}

/// [`log_file_name`]・[`desktop_log_file_name`] の逆。形がちょうど一致するものだけ（それ以外は
/// `None` で、消す対象にしない）。
pub fn parse_log_file_date(name: &str) -> Option<LocalDate> {
    let rest = name
        .strip_prefix(LOG_FILE_PREFIX)
        .or_else(|| name.strip_prefix(DESKTOP_LOG_FILE_PREFIX))?;
    let middle = rest.strip_suffix(LOG_FILE_SUFFIX)?;
    let bytes = middle.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    let compact = format!("{}{}{}", &middle[0..4], &middle[5..7], &middle[8..10]);
    let date = LocalDate::parse_yyyymmdd(&compact)?;
    // 2026-02-30 のような暦に無い日付を受けない（通日へ変換して戻すと同じ日に
    // なること。`parse_yyyymmdd` は範囲を確かめない）。
    let normalized = LocalDate::from_epoch_ms(date.to_days_since_epoch() * 86_400_000, 0);
    (normalized == date).then_some(date)
}

/// 消すログのファイル名（`today` を含めて `retention_days` 日分を残す）。純関数。
pub fn log_files_to_prune<'a>(
    names: impl IntoIterator<Item = &'a str>,
    today: LocalDate,
    retention_days: u32,
) -> Vec<String> {
    let oldest_kept = today.to_days_since_epoch() - i64::from(retention_days.max(1)) + 1;
    names
        .into_iter()
        .filter(|name| {
            parse_log_file_date(name).is_some_and(|date| date.to_days_since_epoch() < oldest_kept)
        })
        .map(str::to_string)
        .collect()
}

/// 古いログファイルの削除の 1 回分（モジュール doc「ログ」）。ログの `Mutex` の外で
/// 走らせる。
#[must_use = "ログの Mutex を手放してから run か spawn で消す"]
pub struct LogPrune {
    dir: LogDir,
    today: LocalDate,
    retention_days: u32,
}

impl PartialEq for LogPrune {
    fn eq(&self, other: &Self) -> bool {
        self.dir.path() == other.dir.path()
            && self.today == other.today
            && self.retention_days == other.retention_days
    }
}

impl Eq for LogPrune {}

impl std::fmt::Debug for LogPrune {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LogPrune")
            .field("dir", &self.dir.path())
            .field("today", &self.today)
            .field("retention_days", &self.retention_days)
            .finish()
    }
}

impl LogPrune {
    /// このスレッドで消す（列挙と削除に上限は無い。失敗は捨てる）。握った版は
    /// `ManagedDir::remove_where`（名前の形で選び、開いた後の属性で確かめ、印のファイルは
    /// 消さない）。
    pub fn run(self) {
        let (today, retention_days) = (self.today, self.retention_days);
        let is_old =
            move |name: &str| !log_files_to_prune([name], today, retention_days).is_empty();
        match &self.dir {
            LogDir::Managed(dir) => {
                let _ = dir.remove_where(&is_old);
            }
            LogDir::Path(dir) => {
                let Ok(entries) = std::fs::read_dir(dir) else {
                    return;
                };
                let names: Vec<String> = entries
                    .filter_map(Result::ok)
                    .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
                    .filter_map(|entry| entry.file_name().into_string().ok())
                    .collect();
                for name in names.iter().filter(|name| is_old(name)) {
                    let _ = std::fs::remove_file(dir.join(name));
                }
            }
        }
    }

    /// 別のスレッドで消す（待たない。スレッドを作れなければ今回は消さない）。
    /// プロセスの終わりでは待たれずに打ち切られる（今書いているファイルには
    /// 触れないので、ログは壊れない）。
    pub fn spawn(self) {
        let _ = std::thread::Builder::new()
            .name("chronogazer-log-prune".to_string())
            .spawn(move || self.run());
    }
}

/// ログの置き場（#623 PR-3）: パス（コンソール）か、握ったフォルダ（サービス。日次ファイルを
/// `ManagedDir::open_append` で辿らずに開き、古いファイルは `remove_where` で消す）。
#[derive(Clone)]
enum LogDir {
    Path(PathBuf),
    Managed(Arc<ManagedDir>),
}

impl LogDir {
    fn path(&self) -> &Path {
        match self {
            Self::Path(path) => path,
            Self::Managed(dir) => dir.final_path(),
        }
    }

    fn open_append(&self, name: &str) -> std::io::Result<File> {
        match self {
            Self::Path(dir) => OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join(name)),
            Self::Managed(dir) => dir.open_append(name, true).map_err(std::io::Error::other),
        }
    }
}

/// 日付ごとのログファイル（モジュール doc「ログ」）。書き込みの失敗は捨てる
/// （ログが書けないことで収集を止めない）。ファイル名の接頭辞は `chronogazer-server`
/// （[`ServerLog::open`]・[`ServerLog::open_in`]）か、デスクトップの
/// `chronogazer-desktop-`（[`ServerLog::open_desktop`]。2026-10-24 オーナー決定「案 A」）。
pub struct ServerLog {
    dir: LogDir,
    /// ファイル名の接頭辞（[`LOG_FILE_PREFIX`] か [`DESKTOP_LOG_FILE_PREFIX`]）。
    prefix: &'static str,
    clock: Arc<dyn Clock>,
    current: Option<(LocalDate, File)>,
    /// `None` の間は消さない（保持日数をまだ読んでいない・無制限）。
    retention_days: Option<u32>,
    /// [`ServerLog::close`] の後（サービスが ACL を緩め始めた後は書かない。#623 PR-3）。
    closed: bool,
}

impl ServerLog {
    /// `dir`（`<root>\logs`）を作り、今日のファイルを開く。
    pub fn open(dir: impl Into<PathBuf>, clock: Arc<dyn Clock>) -> std::io::Result<Self> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir)?;
        Self::start(LogDir::Path(dir), LOG_FILE_PREFIX, clock)
    }

    /// デスクトップの診断のログ（`dir` の `chronogazer-desktop-YYYY-MM-DD.log`。2026-10-24
    /// オーナー決定「案 A」）。開き方は [`ServerLog::open`] と同じ（パス。デスクトップは利用者の
    /// 権限で動き、設定モードの間だけ書く - `banto_safe_root::Mode::Plain` と同じ扱い）。
    /// 同じファイルには `crate::mode_switch::append_desktop_log` も追記する（同じ行の形）。
    pub fn open_desktop(dir: impl Into<PathBuf>, clock: Arc<dyn Clock>) -> std::io::Result<Self> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir)?;
        Self::start(LogDir::Path(dir), DESKTOP_LOG_FILE_PREFIX, clock)
    }

    /// [`ServerLog::open_desktop`] を OS の時計で（デスクトップが使う）。
    pub fn open_desktop_now(dir: impl Into<PathBuf>) -> std::io::Result<Self> {
        Self::open_desktop(dir, Arc::new(banto_tstore::SystemClock))
    }

    /// 握ったフォルダ（`<root>\logs` を `SafeRoot::open_managed` で握った物）に今日のファイルを
    /// **辿らずに**開く（`ManagedDir::open_append`: reparse point でない・ファイル・リンク数 1 を
    /// ハンドルで確かめる。サービス。#623 PR-3、設計 §3.1 の 6）。日跨ぎの新しいファイルも
    /// 同じく開き、古いファイルの削除は `remove_where`（印のファイルは消さない）。
    pub fn open_in(dir: Arc<ManagedDir>, clock: Arc<dyn Clock>) -> std::io::Result<Self> {
        Self::start(LogDir::Managed(dir), LOG_FILE_PREFIX, clock)
    }

    fn start(dir: LogDir, prefix: &'static str, clock: Arc<dyn Clock>) -> std::io::Result<Self> {
        let mut log = Self {
            dir,
            prefix,
            clock,
            current: None,
            retention_days: None,
            closed: false,
        };
        let today = log.today();
        log.current = Some((today, log.open_file(today)?));
        Ok(log)
    }

    fn today(&self) -> LocalDate {
        LocalDate::from_epoch_ms(self.clock.now_ms(), self.clock.utc_offset_ms())
    }

    fn open_file(&self, date: LocalDate) -> std::io::Result<File> {
        self.dir
            .open_append(&dated_log_file_name(self.prefix, date))
    }

    /// 今書いているファイル。
    pub fn current_path(&self) -> PathBuf {
        let date = self
            .current
            .as_ref()
            .map(|(date, _)| *date)
            .unwrap_or_else(|| self.today());
        self.dir.path().join(dated_log_file_name(self.prefix, date))
    }

    pub fn dir(&self) -> &Path {
        self.dir.path()
    }

    /// 書くのをやめ、今のファイルを閉じる（サービスが ACL を緩め始める前。以後の
    /// [`ServerLog::write_line`] は何もしない・新しいファイルも開かない。#623 PR-3、
    /// 設計 §3.2 の 5）。
    pub fn close(&mut self) {
        self.flush();
        self.current = None;
        self.closed = true;
    }

    /// [`ServerLog::close`] の後か。
    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// 1 行書く（先頭に時刻）。日付が変わっていれば次のファイルへ移り、古いものの
    /// 削除を返す（ロックを手放してから走らせる。モジュール doc「ログ」）。
    pub fn write_line(&mut self, line: &str) -> Option<LogPrune> {
        if self.closed {
            return None;
        }
        let now_ms = self.clock.now_ms();
        let offset_ms = self.clock.utc_offset_ms();
        let today = LocalDate::from_epoch_ms(now_ms, offset_ms);
        let rotate = !matches!(&self.current, Some((date, _)) if *date == today);
        let mut prune = None;
        if rotate {
            // 開けなければ前のファイルに書き続ける（次の行でまた試す）。
            if let Ok(file) = self.open_file(today) {
                self.current = Some((today, file));
                prune = self.prune(today);
            }
        }
        if let Some((_, file)) = self.current.as_mut() {
            // 1 行を組み立ててから 1 回で書く（`writeln!` は時刻・本文・改行を別々に書く）。
            // デスクトップでは `crate::mode_switch::append_desktop_log` が同じファイルへ別の
            // ハンドルで追記するので、追記モードが守るのは 1 回の書き込みの単位だけ（#825 の
            // レビュー P2）。
            let record = format!("[{}] {line}\n", format_timestamp(now_ms, offset_ms));
            let _ = file.write_all(record.as_bytes());
        }
        prune
    }

    /// 保持日数を設定し（`None` = 無制限）、古いものの削除を返す（ロックを手放して
    /// から走らせる。モジュール doc「ログ」）。
    pub fn set_retention(&mut self, retention_days: Option<u32>) -> Option<LogPrune> {
        self.retention_days = retention_days;
        let today = self.today();
        self.prune(today)
    }

    /// 削除の 1 回分（ファイルシステムには触れない）。保持日数を知らないなら `None`。
    fn prune(&self, today: LocalDate) -> Option<LogPrune> {
        if self.closed {
            return None;
        }
        self.retention_days.map(|retention_days| LogPrune {
            dir: self.dir.clone(),
            today,
            retention_days,
        })
    }

    /// ディスクへ書き出す（終了の前）。
    pub fn flush(&mut self) {
        if let Some((_, file)) = self.current.as_mut() {
            let _ = file.flush();
            let _ = file.sync_all();
        }
    }
}

/// `[YYYY-MM-DD HH:MM:SS]` の中身（ローカル時刻。banto-hub の `hub_log` と同じ形）。
pub fn format_timestamp(epoch_ms: i64, utc_offset_ms: i64) -> String {
    let date = LocalDate::from_epoch_ms(epoch_ms, utc_offset_ms);
    const MS_PER_DAY: i64 = 86_400_000;
    let local_ms_of_day = (epoch_ms + utc_offset_ms).rem_euclid(MS_PER_DAY);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        date.year,
        date.month,
        date.day,
        local_ms_of_day / 3_600_000,
        (local_ms_of_day / 60_000) % 60,
        (local_ms_of_day / 1_000) % 60
    )
}

#[cfg(test)]
mod tests;
