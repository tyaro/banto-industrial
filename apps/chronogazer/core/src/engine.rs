//! 収集エンジンの起動と終了（#392 段階 B の B1-1、2026-10-12。B1-2 でデスクトップも
//! 乗り換えた）。
//!
//! ChronoGazer の「DB を開き、サービスを組み、Hub と収集を動かし、REST を
//! 待ち受ける」組み立ては、これまでデスクトップ（`src-tauri` の `setup()` と
//! `shutdown_app_state`）と開発用の `banto-serve`（`bin/banto-serve.rs` の
//! `main`）に**別々に**書かれていた（約 300 行の重複。順序の違い・失敗の
//! 扱いの違い・`banto-serve` が `HubService::shutdown` と `pool.close` を
//! 呼ばない、という食い違いがあった）。段階 B では host が 3 つ
//! （デスクトップ／headless の `chronogazer-server`／Windows サービス）に増える
//! ので、組み立てをこのモジュールの [`EngineRuntime`] 1 か所に寄せる
//! （banto-hub の `HubRuntime` と同じ型。計画は
//! docs/chronogazer-service-design.md §3.1(a)）。
//!
//! # 起動の順序（[`EngineRuntime::open`] → [`OpenedEngine::start`]）
//!
//! 1. **A1 の排他**（[`crate::instance_lock::acquire_for_db`]）。**DB に触れる
//!    前**。取れなければ DB・リストア予約・監査の保留のどれにも触れずに
//!    [`EngineStartError::InstanceLock`] を返す。
//! 2. **リストア予約の適用**（`BackupService::apply_pending_restore_at_startup`、
//!    pool を作る前でなければならない）。失敗しても起動は止めない（今までどおり）。
//! 3. **`init_db`**。旧形式の DB・開けない DB は [`EngineStartError::Database`]。
//!
//!    ここまでが [`EngineRuntime::open`]。**デスクトップは DB を開けてから
//!    窓を作る**（旧形式の DB のときに空の画面を先に出さない）ので、B1-2 では
//!    `open` と `start` の間で窓を作る。そのために 2 段に分けてある。
//! 4. サービス（users・settings・backup・レジストリ 3 つ・表示グループ・監査）、
//!    監査の保留の起動時の流し込み、`restore_applied` の記録、監査ログの剪定、
//!    （`banto-serve` だけ）閲覧公開の種まき。
//! 5. **Hub**: `HubService::new`（失敗は [`EngineStartError::Hub`]）→
//!    `attach_registry`（デスクトップと同じく常に。REST のルーターも同じ pool を
//!    差し込むが 2 回目は無視される）。
//! 6. 収集サービスを組み（保存設定 `data.dir` を読めないとき、
//!    [`EngineConfig::store_config_required`] なら Hub を止めて pool を閉じてから
//!    [`EngineStartError::StoreConfig`]、そうでなければ既定値で続行）、
//!    [`EngineConfig::attach_hub`] なら `attach_hub`
//!    （**自動開始より前**。後から繋いでも走り出した収集には効かない）。
//! 7. **listener の bind**（ポートの確保だけ。まだ要求を受けない）。先に
//!    常時の loopback の listener（[`OpenedEngine::with_loopback`]、
//!    `chronogazer-server` だけ。#392 B1-5）を必須で、次に LAN の listener。
//!    loopback か [`EngineListen::Required`] で bind できなければ、ここで Hub を止めて
//!    pool を閉じてから [`EngineStartError::Listen`] を返す - **収集を始める前に
//!    失敗する**ので、失敗して戻る経路に収集・Hub の張り直し・保持期間の掃除の
//!    タスクが残らない（計画書の順序「resume/autostart/retention → listener」
//!    から、bind だけを前に出した。serve は下の 9.）。
//! 8. `hub.resume()`・`collect.autostart()`（どちらも spawn して投げっぱなし。
//!    失敗しても起動は止めず、理由は状態に残る）、保持期間の掃除
//!    （[`crate::retention`]）。
//! 9. bind 済みの listener で serve を始め、[`ServerSlot`] に置く（loopback は
//!    入れ替えない別の slot、LAN は入れ替えられる slot）。
//! 10. **監査の保留の定期の流し込み**。起動の失敗しうる手順がすべて済んでから
//!     起こす（先に起こすと失敗して戻る経路にタスクが残る。banto-hub #557）。
//!
//! # 終了の順序（[`RunningEngine::shutdown`]）
//!
//! B1-1 より前のデスクトップの `shutdown_app_state` と同じ（「消費者を先に止め、
//! 書き手はその後、みんなが書き込む先は最後」。B1-2 からデスクトップの
//! `shutdown_app_state` はこれを呼ぶだけ）:
//!
//! 1. **収集の取り消せない終了の印**（`CollectorService::begin_shutdown`）。
//!    予算の外で、待たずに立てる。以降の開始（まだ止めていない REST・遅れて
//!    動いた自動開始）は断られ、下の 4. の `stop()` が終端の操作になる。
//! 2. 背景タスク: 監査の保留の定期の流し込みを abort し、保持期間の掃除に
//!    止まる合図を送る。流し込みは途中で止めても失われない（ファイルは行が
//!    入ってから消す）。これも予算の外（どちらも待たない）。**掃除の終わりを
//!    待つのは 6.**（下）。
//! 3. **listener**（LAN の [`ServerSlot`] と常時の loopback の slot から取り出して
//!    `stop`）。ハンドラーは同じ pool を使うので pool より前。**slot の lock の取得から
//!    停止までを待つのは [`listener_stop_cap`]（予算の 4 分の 1、最大 1 秒）まで**
//!    （#598 のオーナーレビュー P1。lock の取得を含めたのは同じ PR の Copilot の指摘で、
//!    デスクトップの `server_apply` が適用の間ずっと lock を持つため）: banto の
//!    `RunningServer::stop` は開いている要求の完了を最大 5 秒待つので、本文を送り
//!    切らない要求 1 本で、コンソールを閉じたときの予算（約 3.5 秒）を使い切って
//!    4. の最終 flush を飛ばしていた。上限を超えたら待つのをやめて 4. へ進む（停止
//!    そのものは裏のタスクで続き、banto の 5 秒で中断される。
//!    [`ShutdownReport::listener_stop_abandoned`]）。**順序の約束（消費者 → 書き手 →
//!    生産者 → DB）は変わらない**: 新しい接続は停止の合図で受け付けなくなり、残った
//!    要求が収集を始めることは 1. の取り消せない印が断る。残った要求が DB を触っても
//!    pool はまだ開いている（7. で閉じる。閉じた後に届いた要求は DB のエラーになる
//!    だけで、記録は失わない）。デスクトップ（5 秒）・Ctrl-C（15 秒）の経路も同じ。
//!    待つのをやめた停止のタスクは、lock が空き次第そのとき slot にあるもの（適用が
//!    置いた新しい listener を含む）を止めるので、終了の後に listener を残さない。
//!    `server_apply` の側を「停止の間 lock を持たない」形に変えなかったのは、適用の間
//!    lock を持つことが、並行する適用の直列化と、状態の表示が停止〜再起動の隙間を
//!    見ないための約束（banto #287・#294 の形）で、崩すと途中の状態が見えるため。
//! 4. **収集の停止**（最終 flush）。消費者（LAN）の後・Hub と DB の前。
//! 5. **Hub の購読の停止**（`HubService::shutdown`）。Hub は Hub 経由の接続の
//!    値の出どころ（収集にとっての生産者）なので、収集より後（#383 段階3 P3）。
//! 6. **保持期間の掃除の終わりを待つ**（#597、2026-10-13。#598 で収集の停止の後へ
//!    移した）。掃除の途中なら、その掃除が終わってから止まる。掃除は設定 DB を
//!    読むので pool より前で、abort はしない（接続の確立の途中で落とすと、その
//!    接続が pool の外に残り、7. の後で WAL を本体へ書き戻す。理由は
//!    [`crate::retention`] の「止め方」）。#597 の約束は「終了の後に DB に触れない」
//!    （= pool を閉じる前に掃除が終わっている）なので、待つ位置が pool を閉じる
//!    直前でも守れる。**収集の停止より後に置くのは**、掃除の待ち（設定の読み取り・
//!    次の確認までの待ちを含む）が最終 flush の予算を食わないようにするため
//!    （#598 のオーナーレビュー P1 の方針。コンソールを閉じたときは約 3.5 秒しか
//!    無い）。**掃除と収集の停止が重なっても壊れない**: 削除
//!    （[`CollectorService::prune_data_files`]）と停止はどちらも収集の
//!    `writer_gate` を取ってから書き手に触れるので直列になる（削除が書き手を
//!    掴んだまま停止が close に進むことも、close の途中で書き手なしの削除が走る
//!    ことも無い。`collect` の `Published::writer` の doc）。ただし**削除の最中に
//!    終了が来たら、4. はその削除が終わるまで待つ**（ゲートの直列化。削除そのものは
//!    同期のファイル削除で短い。これは待つ位置を動かしても変わらない残りの待ち）。
//!    予算が切れてこの待ちが落ちても、掃除のタスクは終わりが取れるまで追い続ける:
//!    もう一度 `shutdown` を呼ぶと、まだ走っている掃除の終わりを（その予算の中で）
//!    待ち、終わっていなければ [`ShutdownStep::RetentionStopped`] を報告せず、
//!    **pool も閉じずに**時間切れを返す（PR #597 のレビュー。以前は handle を
//!    取り出していたので、2 回目は掃除の途中でも素通りして pool を閉じていた）。
//! 7. **SQLite の pool を閉じる**（`-wal`/`-shm` を畳む）。
//!
//! 残りの危険: 監査の保留の定期の流し込み（banto の `spawn_spool_flusher`）は
//! 止める口が abort しかないので abort のままにしてある。30 秒ごとの流し込みの
//! 最中に終了が重なると、保持期間の掃除と同じく pool の外の接続が 7. の後で
//! DB を書き戻しうる。banto 側に止める合図を足すのが筋（ここでは直していない）。
//!
//! 3.〜7. は呼び出し側が渡す予算（デスクトップは 5 秒、`banto-serve` は 15 秒）
//! の中で行い、切れたら途中で切り上げて [`ShutdownReport::timed_out`] を立てる
//! （終了で固まるより後始末の一部を失う方がよい。理由はデスクトップの
//! `EXIT_CLEANUP_BUDGET` の doc）。どこまで済んだかは
//! [`ShutdownReport::completed`] に順に残る。
//!
//! 時間切れで返った後も保持期間の掃除が走っていることがある（6. の待ちを
//! 落としただけで、掃除そのものは止めない）。その掃除のタスクは **A1 の排他
//! （の `Arc`）を持っている**ので、[`RunningEngine`] を drop しても掃除が
//! 終わるまで排他は返らず、別のプロセスが同じ DB を開いている間に裏で掃除が
//! DB に触れることはない（PR #597 のレビュー）。同じプロセスで同じ DB を
//! 開き直す経路（起動時の対処の窓など）も、それまでは排他を取れない。
//!
//! **A1 の排他は `shutdown` では返さない**。[`RunningEngine`] を drop したとき
//! （普通はプロセスの終わり）に返る。終了の途中で別のプロセスが同じ DB を
//! 開き始めないようにするため。
//!
//! # host ごとの差（[`EngineConfig`]）
//!
//! - KeyStore（デスクトップ = OS キーリング、`banto-serve` =
//!   `UnavailableKeyStore` か `BANTO_HUB_MEMORY_KEYS=1` の `MemoryKeyStore`、
//!   `chronogazer-server` = DPAPI のファイル（`crate::server_host::open_key_store`））と
//!   `attach_hub` の有無。
//! - 待ち受け（[`EngineListen`]、`chronogazer-server` は加えて常時の loopback）と
//!   `allow_setup`（`POST /api/auth/setup` を開けるのは `banto-serve` と、
//!   `chronogazer-server` の**実際に loopback のアドレスに bind できた listener だけ**。
//!   [`OpenedEngine::with_loopback_only_setup`]）。
//! - ログの接頭辞（`banto` / `banto-serve`）と、起動時の `restore_applied` の
//!   監査の `origin`（`tauri` / `rest`）。今までの各 host の文言・値を保つため。
//! - 保存設定を読めないときに起動を止めるか（`store_config_required`。
//!   デスクトップ = 止める、`banto-serve` = 既定値で続行。どちらも B1-1 より前からの
//!   各 host の扱い。`chronogazer-server` はデスクトップと同じく止める）。
//!
//! `chronogazer-server`（#392 B1-5）の組み立ては [`crate::server_host::start`]
//! （`open` → 設定から待ち受けの形 → `with_loopback`（`0.0.0.0` なら
//! `with_loopback_fallback` も）・`with_listen`・`with_loopback_only_setup` → `start` →
//! `run\endpoint.json`）。
//!
//! **このモジュールに入れないもの**（host 固有）: 窓・トレイ・自動ログイン・
//! OS キーリング・Acrylic・イベントの webview への転送・単一インスタンスの
//! プラグイン。これらはデスクトップのシェル（`src-tauri`）に残る。
//!
//! # デスクトップの呼び方（B1-2、2026-10-12 実装。同日 #392 の起動時の対処で更新）
//!
//! ```text
//! setup() → launch_desktop(prelude なし)（対処の窓で対処を選んだ後も同じ関数でやり直す）:
//!   opened = EngineRuntime::open(cfg{ host: Desktop, key_store: Keyring,
//!                                     attach_hub: true, listen: Disabled,
//!                                     store_config_required: true, .. })
//!   prelude: 保存先の書き換え（startup_recovery::apply_store_dir、失敗は opened.close()）
//!            または対処の監査の予約（opened.with_recovery_note）
//!   events の webview への転送を opened.events() に張る   // start より前に購読する
//!   LAN の自動起動の判定（lan_autostart_allowed）を opened.settings() で読み
//!     （読めなければ opened.close() して Err）、
//!     opened.with_listen(EngineListen::BestEffort(cfg) または Disabled)
//!   engine = opened.start()
//!   自動ログイン（engine.services() の users/audit を使う）
//!   AppState { services: engine.services().clone(), server: engine.server_slot() }、
//!   エンジンを管理状態に置く（プロセスの終わりまで drop しない）
//!   create_main_window(); hide_to_tray_on_close(); Acrylic; tray::install()
//! 失敗（すべて Err、panic しない）:
//!   Err(InstanceLock) / Err(Database(Legacy)) -> 今までのエラーの窓
//!   それ以外 -> 対処の窓（crate::startup_recovery の classify / plan）
//! shutdown_app_state(): engine.shutdown(予算) → ExitCleanupReport へ写す
//!   （トレイの終了と RunEvent::Exit で 1 回だけ。banto_desktop_tray::ExitCleanupOnce）
//! server_apply: engine.server_slot().lock() を入れ替える
//! ```
//!
//! メインの窓は B1-2 では `open` と `start` の間に作っていたが、#392 の起動時の
//! 対処で `start` の後（`AppState` を manage した後）に移した（`start` の失敗で
//! メインの画面を出してから対処の窓に切り替えないため）。
//!
//! B1-1 より前のデスクトップとの差（どれも観測できる挙動を変えないと判断した。
//! 理由は docs/chronogazer-service-design.md §4 の B1-2 の実装メモ）:
//! (a) 自動ログイン（監査に `login` を書く）が Hub と収集の組み立ての後になる
//! （どちらも spawn して投げっぱなしで、Tauri のコマンドはまだ受けない）、
//! (b) LAN の listener の bind が自動開始の spawn より前になる（serve は後。上の 7.）、
//! (c) 起動時にリストアを適用したときに標準出力へ 1 行出す（リリースビルドの
//! デスクトップは標準出力が見えない）、(d) 保持期間の掃除を終了で止める
//! （以前はプロセスの終わりまで放っておいた。B1-2 では abort していたが、
//! 2026-10-13 から止める合図を送って予算の中で終わりを待つ。掃除の途中なら
//! それを終えてから止まるので、終了がその分だけ延びうる）。

//! # テスト
//!
//! 起動の失敗が `Result` で返ること（排他の競合では DB に触れない・DB を開けない・
//! 待ち受けを開けない）と、終了の順序。順序は各手順の**直前の実際の状態**
//! （印・listener・収集・Hub・pool）をテスト専用の記録点で採り、シミュレータ
//! 相手に本当に収集している状態から確かめる（手順を入れ替えると落ちる）。

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use banto_core::BantoError;
use banto_hub_bootstrap::KeyStore;
use banto_server::{
    bind as bind_listener, static_router, with_security_headers, AuthState, BoundServer,
    RunningServer, ServerConfig, ServerEvent,
};
use tokio::sync::{broadcast, Mutex as AsyncMutex, MutexGuard as AsyncMutexGuard};
use tokio::task::JoinHandle;

use crate::assets::FrontendAssets;
use crate::audit::{AuditEntry, AuditLogService};
use crate::audit_spool::{build_audit_service, flush_at_startup};
use crate::backup::{AppliedRestoreInfo, BackupService};
use crate::collect::{resolve_data_dir, CollectorService};
use crate::db::{init_db, Db, DbPool, InitDbError};
use crate::display_groups::DisplayGroupService;
use crate::events::event_channel;
use crate::host_info::{host_router, HostInfo};
use crate::hub::HubService;
use crate::instance_lock::{acquire_for_db, ChronoGazerHost, InstanceGuard, InstanceLockFailure};
use crate::rest::{api_router, user_auth_state};
use crate::retention::RetentionTask;
use crate::settings::{data_dir_value_unreadable, store_config, ServerSettings, SettingsService};
use crate::startup_recovery::{check_writable_dir, RecoveryLock, RecoveryNote};
use crate::users::UsersService;
use crate::{CollectionGroupService, PlcConnectionService, TagService};

/// LAN（REST・SSE・静的 UI）の待ち受けをどうするか。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineListen {
    /// 起動時には待ち受けない（[`ServerSlot`] は空。後から入れられる）。
    Disabled,
    /// 待ち受けが必須。bind できなければ起動を失敗にする
    /// （[`EngineStartError::Listen`]）。`banto-serve`。
    Required(ServerConfig),
    /// 待ち受けを試み、bind できなければログに出して空のまま起動を続ける
    /// （[`RunningEngine::listen_error`]）。デスクトップの LAN の自動起動。
    BestEffort(ServerConfig),
}

/// [`EngineRuntime`] の起動に要るもの（host ごとの差はすべてここに出す）。
pub struct EngineConfig {
    /// 設定 DB のファイル。A1 の排他の単位でもある。
    pub db_path: PathBuf,
    /// `data.dir` の相対パスの基準（[`resolve_data_dir`]）。デスクトップは
    /// アプリのデータディレクトリ、`banto-serve` は DB の置き場
    /// （[`EngineConfig::db_dir_of`]）。
    pub data_base: PathBuf,
    /// Hub の API キーの置き場（`HubService::new` に渡す）。
    pub key_store: Arc<dyn KeyStore>,
    /// 収集に Hub を繋ぐか（`CollectorService::attach_hub`）。キーを保存できる
    /// host だけ `true`（繋いでもキーが無ければ hub 接続は購読できない）。
    pub attach_hub: bool,
    /// A1 の診断ファイルに書く host の種別。
    pub host: ChronoGazerHost,
    /// LAN の待ち受け。
    pub listen: EngineListen,
    /// `POST /api/auth/setup` を開けるか（`banto-serve` の `BANTO_ALLOW_SETUP`）。
    pub allow_setup: bool,
    /// 起動時に保存済みの設定 `server.viewer_public` を `true` にする
    /// （`banto-serve` の `BANTO_VIEWER_PUBLIC`。開発・E2E 用）。
    pub seed_viewer_public: bool,
    /// ログ（標準出力・標準エラー出力）の接頭辞（`banto` / `banto-serve`）。
    pub log_label: &'static str,
    /// 起動時の `restore_applied` の監査の `origin`（`tauri` / `rest`）。
    pub audit_origin: &'static str,
    /// 時系列データの保存設定（`data.dir` など、[`store_config`]）を読めなかった
    /// ときに起動を止めるか（B1-2、2026-10-12）。`false`（`banto-serve`）は既定値で
    /// 続行する。`true`（デスクトップ）は Hub を止めて pool を閉じてから
    /// [`EngineStartError::StoreConfig`] を返す - 利用者が `data.dir` を別の場所に
    /// 設定していると、既定値で続けたときに時系列データを**違う場所へ**書き始めて
    /// しまうため（デスクトップは B1-1 より前から、ここで起動を止めていた）。
    pub store_config_required: bool,
    /// 起動時に、解決済みの保存先（`data.dir`）に書き込めるかを確かめ、書けなければ
    /// 起動を止めるか（#392、2026-10-12 オーナー決定）。`true`（デスクトップ）は
    /// Hub を止めて pool を閉じてから [`EngineStartError::StoreDirUnwritable`] を返す
    /// （USB の取り外し・権限・ドライブが無いなどで、収集の開始で初めてエラーに
    /// なるのを起動時の対処の窓に前倒しする）。確かめ方は
    /// [`crate::startup_recovery::check_writable_dir`]（無ければフォルダを作る - 収集の
    /// 開始も作るので同じ扱い - 一時ファイルを書いて消す。既存の中身には触れない）。
    /// `false`（`banto-serve`）は今までどおり確かめない（開発・E2E 用。書けなければ
    /// 収集の開始がエラーを状態に残す）。
    pub store_dir_check_required: bool,
}

impl EngineConfig {
    /// DB ファイルの置き場（`data.dir` の基準などに使う）。ファイル名だけの
    /// パス（`parent()` が空）は作業ディレクトリ（`"."`）。
    pub fn db_dir_of(db_path: &Path) -> PathBuf {
        match db_path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
            _ => PathBuf::from("."),
        }
    }
}

/// [`EngineRuntime::open`] / [`OpenedEngine::start`] の失敗。
#[derive(Debug)]
pub enum EngineStartError {
    /// 同じ DB を別のプロセスが使用中など（A1）。DB には触れていない。
    InstanceLock(InstanceLockFailure),
    /// DB を開けない・旧形式の DB（`InitDbError::Legacy`）。
    Database(InitDbError),
    /// `HubService::new`（`installation_id` の読み書き）の失敗。
    Hub(BantoError),
    /// 時系列データの保存設定を読めなかった（[`EngineConfig::store_config_required`]
    /// のときだけ）。`data_dir_unreadable` は、読めなかったのが保存先
    /// （`data.dir`）そのものか（起動時の対処の窓の分類に使う。
    /// [`crate::startup_recovery::classify`]）。
    StoreConfig {
        source: BantoError,
        data_dir_unreadable: bool,
    },
    /// 解決済みの保存先に書き込めない（[`EngineConfig::store_dir_check_required`]
    /// のときだけ）。`reason` は利用者向けの理由。
    StoreDirUnwritable { dir: PathBuf, reason: String },
    /// [`EngineListen::Required`] の待ち受けを開けなかった。
    Listen {
        config: ServerConfig,
        source: BantoError,
    },
}

impl std::fmt::Display for EngineStartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InstanceLock(err) => err.fmt(f),
            // 旧形式の DB は、何をすればよいかを書いた文言をそのまま出す
            // （`crate::db` のモジュール doc「旧形式の DB の拒否」）。
            Self::Database(InitDbError::Legacy(legacy)) => legacy.fmt(f),
            Self::Database(err) => write!(f, "DB の初期化に失敗しました: {err}"),
            Self::Hub(err) => write!(f, "Hub 接続の初期化に失敗しました: {err}"),
            Self::StoreConfig { source, .. } => {
                write!(
                    f,
                    "時系列データの保存設定の読み取りに失敗しました: {source}"
                )
            }
            Self::StoreDirUnwritable { reason, .. } => f.write_str(reason),
            Self::Listen { config, source } => write!(
                f,
                "待ち受け（{}:{}）を開始できませんでした: {source}",
                config.bind, config.port
            ),
        }
    }
}

impl std::error::Error for EngineStartError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InstanceLock(err) => Some(err),
            Self::Database(err) => Some(err),
            Self::Hub(err)
            | Self::StoreConfig { source: err, .. }
            | Self::Listen { source: err, .. } => Some(err),
            Self::StoreDirUnwritable { .. } => None,
        }
    }
}

/// 入れ替えられる LAN の listener の入れ物。デスクトップの `server_apply`
/// （B1-2）は [`ServerSlot::lock`] を今の `AppState::server` と同じく使う
/// （止めて、新しいものを置く）。終了は [`RunningEngine::shutdown`] が
/// 取り出して止める。
#[derive(Default)]
pub struct ServerSlot {
    inner: AsyncMutex<Option<RunningServer>>,
}

impl ServerSlot {
    pub fn new(server: Option<RunningServer>) -> Self {
        Self {
            inner: AsyncMutex::new(server),
        }
    }

    /// 中身を直接扱う（入れ替え・状態の表示）。
    pub async fn lock(&self) -> AsyncMutexGuard<'_, Option<RunningServer>> {
        self.inner.lock().await
    }

    /// 走っている listener の実際のアドレス。
    pub async fn local_addr(&self) -> Option<SocketAddr> {
        self.inner
            .lock()
            .await
            .as_ref()
            .map(RunningServer::local_addr)
    }

    /// 取り出して止める。走っていれば `true`。
    pub async fn stop(&self) -> bool {
        // ロックは取り出すまで。停止（最長 5 秒の graceful）を待つ間は握らない。
        let running = self.inner.lock().await.take();
        match running {
            Some(running) => {
                running.stop().await;
                true
            }
            None => false,
        }
    }
}

/// 動いているエンジンの各サービスの `Clone` ハンドル（中身は `Arc`）。
/// B1-2 でデスクトップの `AppState` がそのまま受け取る。
#[derive(Clone)]
pub struct EngineServices {
    pub users: UsersService,
    pub settings: SettingsService,
    pub audit: AuditLogService,
    pub backup: BackupService,
    pub hub: HubService,
    pub plc_connections: PlcConnectionService,
    pub collection_groups: CollectionGroupService,
    pub tags: TagService,
    pub display_groups: DisplayGroupService,
    pub collect: CollectorService,
    /// LAN の REST の bearer トークンの状態（listener を入れ替えてもトークンが
    /// 続くよう、エンジンが 1 つ持つ）。
    pub rest_auth: AuthState,
    pub events: broadcast::Sender<ServerEvent>,
    /// 全サービスが共有する SQLite の pool（終了の最後に閉じる）。
    pub pool: DbPool,
    /// `GET /api/host` の応答（host 種別と排他の単位。#392 B1-4）。
    pub host: HostInfo,
}

impl EngineServices {
    /// LAN に出すルーター（REST・SSE・静的 UI、最外に `with_security_headers`
    /// - banto #500）。
    pub fn router(&self, allow_setup: bool) -> Router {
        with_security_headers(
            api_router(
                self.users.clone(),
                self.settings.clone(),
                self.audit.clone(),
                self.backup.clone(),
                self.hub.clone(),
                self.plc_connections.clone(),
                self.collection_groups.clone(),
                self.tags.clone(),
                self.collect.clone(),
                self.display_groups.clone(),
                self.rest_auth.clone(),
                self.events.clone(),
                allow_setup,
            )
            .merge(host_router(self.host.clone()))
            .merge(static_router::<FrontendAssets>()),
        )
    }
}

/// 起動の入口。
pub struct EngineRuntime;

impl EngineRuntime {
    /// [`EngineRuntime::open`] と [`OpenedEngine::start`] を続けて行う
    /// （間に何も挟まない host 用。`banto-serve`）。
    pub async fn start(cfg: EngineConfig) -> Result<RunningEngine, EngineStartError> {
        Self::open(cfg).await?.start().await
    }

    /// 起動の前半: A1 の排他 → リストア予約の適用 → `init_db`（モジュール doc の
    /// 1.〜3.）。排他を取れなければ DB には一切触れない。
    pub async fn open(cfg: EngineConfig) -> Result<OpenedEngine, EngineStartError> {
        Self::lock(cfg)?.open().await
    }

    /// 起動の最初の段だけ: A1 の排他を取る（DB・リストア予約・監査の保留のどれにも
    /// 触れない）。続きは [`LockedEngine::open`]。間に host の準備を挟みたいとき用
    /// （#598 のオーナーレビュー P2: `chronogazer-server` は排他を取った後・リストア
    /// 予約の適用と `init_db` の前にログを開き、DB を開けない理由やリストアの警告を
    /// 日次ログに残す）。
    pub fn lock(cfg: EngineConfig) -> Result<LockedEngine, EngineStartError> {
        let instance_lock =
            acquire_for_db(&cfg.db_path, cfg.host).map_err(EngineStartError::InstanceLock)?;
        Ok(LockedEngine { cfg, instance_lock })
    }

    /// [`EngineRuntime::open`] と同じだが、起動時の対処が**先に取った** A1 の排他
    /// （[`RecoveryLock`]）をそのまま引き継ぐ（取り直さない。復旧の変更から
    /// エンジンまで排他が途切れない - #594 のオーナーレビュー P1）。対処の中で
    /// 適用したリストアは `restore_applied` の監査に残る。`lock` の DB が `cfg` と
    /// 違えば（呼び出し側の誤り）、`lock` を返して普通に排他を取る。
    pub async fn open_with_recovery_lock(
        cfg: EngineConfig,
        lock: RecoveryLock,
    ) -> Result<OpenedEngine, EngineStartError> {
        if lock.db_path != cfg.db_path {
            debug_assert_eq!(lock.db_path, cfg.db_path, "別の DB の排他");
            drop(lock);
            return Self::open(cfg).await;
        }
        let RecoveryLock {
            guard,
            applied_restore,
            ..
        } = lock;
        Self::open_locked(cfg, guard, applied_restore).await
    }

    async fn open_locked(
        cfg: EngineConfig,
        instance_lock: InstanceGuard,
        preapplied_restore: Option<AppliedRestoreInfo>,
    ) -> Result<OpenedEngine, EngineStartError> {
        let label = cfg.log_label;

        // Spec M17: リストア予約は pool を作る前に適用する
        // （`BackupService::apply_pending_restore_at_startup` の doc）。失敗しても
        // 起動は止めない（今の DB はそのまま残る）。I2a（banto #280）: 予約も
        // バックアップも `<DB の親>/backups/<DB ファイル名>/`。それより前の共有領域
        // に残ったファイルは警告だけ（2026-10-04 オーナー決定）。
        // 起動時の対処で適用したリストアがあればそれ（そのときは予約は残っていない）。
        // 通常の起動では、ここでの適用の失敗は今までどおり記録して続ける（今の DB は
        // そのまま残る）。対処の窓の「バックアップから戻す」は適用を確かめてから
        // ここへ来る（`startup_recovery::restore_from_backup`）。
        let applied_restore = match preapplied_restore {
            Some(applied) => Some(applied),
            None => match BackupService::apply_pending_restore_at_startup(&cfg.db_path).await {
                Ok(applied) => applied,
                Err(err) => {
                    eprintln!("{label}: 起動時のリストア適用に失敗しました: {err}");
                    None
                }
            },
        };

        // DB スキーマの整理（2026-10-02）: 旧形式の DB は起動を拒否する（自動移行は
        // 無い）。何をすればよいかは `InitDbError::Legacy` の文言にある。
        let pool = init_db(&cfg.db_path)
            .await
            .map_err(EngineStartError::Database)?;

        Ok(OpenedEngine {
            cfg,
            instance_lock,
            pool,
            applied_restore,
            recovery_note: None,
            loopback: None,
            loopback_fallback: None,
            loopback_only_setup: false,
            events: event_channel(),
        })
    }
}

/// A1 の排他だけを取ったエンジン（[`EngineRuntime::lock`]）。drop すると排他を返す。
pub struct LockedEngine {
    cfg: EngineConfig,
    instance_lock: InstanceGuard,
}

impl LockedEngine {
    /// 起動の前半の残り: リストア予約の適用 → `init_db`（モジュール doc の 2.〜3.）。
    pub async fn open(self) -> Result<OpenedEngine, EngineStartError> {
        EngineRuntime::open_locked(self.cfg, self.instance_lock, None).await
    }
}

/// DB を開けた後・サービスを組む前のエンジン（[`EngineRuntime::open`]）。
/// デスクトップはここで窓を作る（モジュール doc「B1-2」）。drop すると pool の
/// ハンドルと A1 の排他を返す（何も起動していない）。
pub struct OpenedEngine {
    cfg: EngineConfig,
    instance_lock: InstanceGuard,
    pool: DbPool,
    applied_restore: Option<AppliedRestoreInfo>,
    /// 起動時の対処の窓で選ばれた対処（[`OpenedEngine::with_recovery_note`]）。
    recovery_note: Option<RecoveryNote>,
    /// 常時の loopback の listener（[`OpenedEngine::with_loopback`]）。
    loopback: Option<ServerConfig>,
    /// 常時の listener を開けなかったときの代わり（[`OpenedEngine::with_loopback_fallback`]）。
    loopback_fallback: Option<ServerConfig>,
    /// 最初の管理者の作成を loopback に bind できた listener だけに開ける
    /// （[`OpenedEngine::with_loopback_only_setup`]）。
    loopback_only_setup: bool,
    events: broadcast::Sender<ServerEvent>,
}

impl OpenedEngine {
    /// 設定 DB のファイル。
    pub fn db_path(&self) -> &Path {
        &self.cfg.db_path
    }

    /// 起動時の対処の窓で選ばれた対処を、`start` で監査に残す
    /// （`startup_recovery`。DB を開けない間は書けないので、開けた後に書く。
    /// [`crate::startup_recovery`] のモジュール doc「監査」）。
    pub fn with_recovery_note(mut self, note: RecoveryNote) -> Self {
        self.recovery_note = Some(note);
        self
    }

    /// `start` せずに閉じる: pool を閉じ（ファイルのハンドルを返す）、A1 の
    /// 排他を返す。drop でも両方返るが、pool の接続は裏で閉じるので、続けて
    /// DB のファイルを触る（名前を変える）前にはこちらを待つ。
    pub async fn close(self) {
        self.pool.close().await;
        drop(self.instance_lock);
    }
    /// 開いた pool（`start` の前に設定を読みたい host 用）。
    pub fn pool(&self) -> &DbPool {
        &self.pool
    }

    /// 開いた DB の設定（`start` の前に LAN の自動起動を判定したい host 用）。
    pub fn settings(&self) -> SettingsService {
        SettingsService::new(Db::Sqlite(self.pool.clone()))
    }

    /// 変更通知の放送（`start` より前に購読しておけば、取りこぼさない）。
    pub fn events(&self) -> &broadcast::Sender<ServerEvent> {
        &self.events
    }

    /// Hub の API キーの置き場を差し替える（#598 の Copilot の指摘、`chronogazer-server`）。
    /// 保存先（`secrets\hub-keys\`）の作成・ACL の付与は共有のルートの下に物を作るので、
    /// A1 の排他を取った後（`open` の後）に行い、ここで渡す。`start` が使う。
    pub fn with_key_store(mut self, key_store: Arc<dyn KeyStore>) -> Self {
        self.cfg.key_store = key_store;
        self
    }

    /// 待ち受けを差し替える（`start` の前に設定から決める host 用）。
    pub fn with_listen(mut self, listen: EngineListen) -> Self {
        self.cfg.listen = listen;
        self
    }

    /// 常時の loopback の listener を足す（#392 B1-5、`chronogazer-server`。
    /// 設計書 §3.1(b)「loopback の listener は常に開く」・決定 9）。**必須**: bind
    /// できなければ [`EngineListen::Required`] と同じく収集を始める前に
    /// [`EngineStartError::Listen`] で戻る。LAN の [`ServerSlot`] とは別の入れ物に
    /// 置き、LAN の入れ替え（決定 9）の対象にしない。LAN と重なる bind（`0.0.0.0`）
    /// を 1 本にまとめるのは呼び出し側（`chronogazer_core::server_host::ListenPlan`）。
    pub fn with_loopback(mut self, config: ServerConfig) -> Self {
        self.loopback = Some(config);
        self
    }

    /// 常時の listener（[`OpenedEngine::with_loopback`]）を開けなかったときに代わりに
    /// 開くもの（#598 のレビュー）。`chronogazer-server` は LAN の `0.0.0.0` を常時の
    /// listener に 1 本にまとめるので、別のプロセスが `<LAN の IP>:port` を持っている
    /// などでワイルドカードの bind が失敗しても、`127.0.0.1:port` に落として起動を
    /// 続ける（「loopback は常に、LAN は best-effort」）。落としたときの理由は
    /// [`RunningEngine::listen_error`] に残る。代わりも開けなければ `Err(Listen)`。
    pub fn with_loopback_fallback(mut self, config: ServerConfig) -> Self {
        self.loopback_fallback = Some(config);
        self
    }

    /// 最初の管理者の作成（`POST /api/auth/setup`）を、**実際に loopback のアドレスに
    /// bind できた listener のルーターだけ**に開ける（#392 B1-5、#598 のレビュー。
    /// `chronogazer-server`）。計画ではなく bind の結果で決めるので、LAN の bind が
    /// 失敗して loopback だけで動いていれば開き、`0.0.0.0` で LAN に出ていれば閉じる。
    /// LAN の listener と、後から LAN を入れ替えるときの [`RunningEngine::router`]
    /// （決定 9）は常に閉じる - LAN に出る listener で開けたままにしない。
    /// [`EngineConfig::allow_setup`]（`banto-serve`）が `true` ならそちらが優先（全部で開く）。
    pub fn with_loopback_only_setup(mut self) -> Self {
        self.loopback_only_setup = true;
        self
    }

    /// 起動の後半（モジュール doc の 4.〜10.）。
    pub async fn start(self) -> Result<RunningEngine, EngineStartError> {
        let OpenedEngine {
            cfg,
            instance_lock,
            pool,
            applied_restore,
            recovery_note,
            loopback,
            loopback_fallback,
            loopback_only_setup,
            events,
        } = self;
        let label = cfg.log_label;

        // I2a: users/settings/audit/backup は banto の `banto_admin_services`。
        // バックエンドを問わない `Db` ハンドルを受けるので同じ pool を包んで渡す。
        let db = Db::Sqlite(pool.clone());
        let users = UsersService::new(db.clone());
        let settings = SettingsService::new(db.clone());
        let backup = BackupService::new(cfg.db_path.clone(), db.clone());
        // #383 段階2a / R1-B: レジストリ 3 サービス（テーブルは `init_db` が呼ぶ
        // `banto_tags::migrate` で作成済み）。#393: 表示グループ。
        let plc_connections = PlcConnectionService::new(pool.clone());
        let collection_groups = CollectionGroupService::new(pool.clone());
        let tags = TagService::new(pool.clone());
        let display_groups = DisplayGroupService::new(pool.clone());

        // #437（banto ADR-0019）: 監査は保留つき（DB に書けない監査は DB の隣の
        // `audit-spool/` に退避）。マイグレーションは済んでいるので、前回の実行で
        // 残った保留をここで 1 回流し込む。定期の流し込みは最後（10.）。
        let audit = build_audit_service(db, &cfg.db_path);
        flush_at_startup(&audit, label).await;
        // LAN のセッション（spec §8.2）。`login`/`login_failed` を監査に書き、
        // 要求のたびにアカウントを確かめ直す（banto v1.7.0 #204）。
        let rest_auth = user_auth_state(users.clone(), audit.clone());

        // Spec M17: `restore_applied` は監査サービスができた今記録する
        // （適用そのものは pool より前に済んでいる）。
        if let Some(applied) = &applied_restore {
            audit
                .record(AuditEntry {
                    actor_username: None,
                    actor_role: None,
                    action: "restore_applied",
                    resource: "backups",
                    entity_id: None,
                    detail: Some(serde_json::json!({
                        "preRestoreBackupFileName": applied.pre_restore_backup_file_name,
                    })),
                    origin: cfg.audit_origin,
                    result: "ok",
                })
                .await;
            println!(
                "{label}: 起動時にリストアを適用しました（適用前の自動バックアップ: {}）",
                applied.pre_restore_backup_file_name
            );
        }
        // 起動時の対処の窓で選ばれた対処（作り直し・バックアップからの戻し・
        // 保存先の変更）。DB を開けない間は書けないので、開けた今記録する。
        // 「もう一度確かめる」だけは何も変えない対処なので、`start` の失敗しうる処理
        // （保存先の書き込み確認・listener の bind）がすべて済んだ後に記録する（下）。
        // 失敗して窓へ戻る再試行に成功の行を残さない（#594 のオーナーレビュー P2）。
        if let Some(note) = &recovery_note {
            if note.choice != crate::startup_recovery::RecoveryChoice::RetryStoreCheck {
                audit.record(note.audit_entry(cfg.audit_origin)).await;
            }
        }

        // 起動時の剪定（spec M14「起動時に 1 回 + list 実行時に軽く」）。失敗しても
        // 起動は止めない。
        match settings.audit_config().await {
            Ok(config) => {
                if let Err(err) = audit
                    .prune(config.retention_days, config.retention_rows)
                    .await
                {
                    eprintln!("{label}: 起動時の監査ログの剪定に失敗しました: {err}");
                }
            }
            Err(err) => eprintln!("{label}: 監査ログの保持設定の読み取りに失敗しました: {err}"),
        }

        // 閲覧公開の種まき（`banto-serve` の `BANTO_VIEWER_PUBLIC`、admin-template
        // v3.0.0 と同じ）。保存済みの設定を読み書きするので `enabled`/bind/port は
        // 残る。失敗しても起動は止めない。
        if cfg.seed_viewer_public {
            match settings.server_config().await {
                Ok(config) => {
                    let seeded = ServerSettings {
                        viewer_public: true,
                        ..config
                    };
                    if let Err(err) = settings.set_server_config(&seeded).await {
                        eprintln!("{label}: 閲覧公開設定の保存に失敗しました: {err}");
                    }
                }
                Err(err) => eprintln!("{label}: サーバー設定の読み取りに失敗しました: {err}"),
            }
        }

        // #332: Hub 接続。`installation_id` を設定から読む（無ければ作って保存する）。
        // 失敗は設定 DB が書けない状態なので起動の失敗にする（pool は閉じて返す）。
        let hub = match HubService::new(settings.clone(), cfg.key_store.clone()).await {
            Ok(hub) => hub,
            Err(err) => {
                pool.close().await;
                return Err(EngineStartError::Hub(err));
            }
        };
        // #383 段階3 P4: 購読の対象（登録済みの hub タグ）をレジストリから読む。
        hub.attach_registry(pool.clone());

        // #383 段階2b / R1-C: 収集。`data.dir` の相対パスは `data_base` 基準
        // （既定 `"./data"` をそのまま使うとプロセスの作業ディレクトリに書いて
        // しまう）。設定を読めなければ、`store_config_required` なら起動を止め
        // （Hub を止めて pool を閉じて返す）、そうでなければ既定値で続行する。
        let store_settings = match store_config(&settings).await {
            Ok(store_settings) => store_settings,
            Err(err) if cfg.store_config_required => {
                // 読めないのが保存先（`data.dir`）そのものかを、pool を閉じる前に
                // 確かめる（起動時の対処の窓が「保存先を選び直す」を出すか決める）。
                let data_dir_unreadable = data_dir_value_unreadable(&pool).await;
                hub.shutdown().await;
                pool.close().await;
                return Err(EngineStartError::StoreConfig {
                    source: err,
                    data_dir_unreadable,
                });
            }
            Err(err) => {
                eprintln!(
                    "{label}: 時系列データの保存設定の読み取りに失敗しました（既定値で続行します）: {err}"
                );
                Default::default()
            }
        };
        let collect_data_dir = resolve_data_dir(&cfg.data_base, &store_settings.data_dir);
        // #392: 保存先に書き込めるか（`store_dir_check_required` のときだけ）。
        if cfg.store_dir_check_required {
            if let Err(err) = check_writable_dir(&collect_data_dir).await {
                hub.shutdown().await;
                pool.close().await;
                return Err(EngineStartError::StoreDirUnwritable {
                    dir: collect_data_dir,
                    reason: err.0,
                });
            }
        }
        let collect = CollectorService::new(pool.clone(), collect_data_dir);
        if cfg.attach_hub {
            // 自動開始（下）より前に繋ぐ。
            collect.attach_hub(hub.clone());
        }

        // 7. listener の bind（ポートの確保だけ）。収集を始める前に失敗を決める
        // （モジュール doc の 7.）。常時の loopback（必須）が先。
        // 開けなければ代わり（`with_loopback_fallback`）を試し、理由を残す。
        let mut fallback_error = None;
        let bound_loopback = match &loopback {
            None => None,
            Some(config) => {
                let first = bind_listener(config.clone()).await;
                let result = match (first, &loopback_fallback) {
                    (Ok(bound), _) => Ok(bound),
                    (Err(err), Some(fallback)) => {
                        eprintln!(
                            "{label}: 待ち受け（{}:{}）を開けなかったため、{}:{} だけで続けます: {err}",
                            config.bind, config.port, fallback.bind, fallback.port
                        );
                        fallback_error = Some(err.to_string());
                        bind_listener(fallback.clone())
                            .await
                            .map_err(|source| (fallback.clone(), source))
                    }
                    (Err(source), None) => Err((config.clone(), source)),
                };
                match result {
                    Ok(bound) => Some(bound),
                    Err((config, source)) => {
                        hub.shutdown().await;
                        pool.close().await;
                        return Err(EngineStartError::Listen { config, source });
                    }
                }
            }
        };
        let (bound, listen_error) = match &cfg.listen {
            EngineListen::Disabled => (None, None),
            EngineListen::Required(config) => match bind_listener(config.clone()).await {
                Ok(bound) => (Some(bound), None),
                Err(source) => {
                    hub.shutdown().await;
                    pool.close().await;
                    return Err(EngineStartError::Listen {
                        config: config.clone(),
                        source,
                    });
                }
            },
            EngineListen::BestEffort(config) => match bind_listener(config.clone()).await {
                Ok(bound) => (Some(bound), None),
                Err(err) => {
                    // LAN が無くてもエンジンは動く（デスクトップの今の扱い）。
                    eprintln!("{label}: 起動時のLANアクセス開始に失敗しました: {err}");
                    (None, Some(err.to_string()))
                }
            },
        };

        // 8. 張り直しと自動開始は spawn して投げっぱなし（失敗しても起動は止めず、
        // 理由は状態に残る）。レジストリを後から編集しても自動では再起動しない
        // （反映は `restart`）。
        {
            let hub = hub.clone();
            tokio::spawn(async move { hub.resume().await });
        }
        {
            let collect = collect.clone();
            tokio::spawn(async move { collect.autostart().await });
        }
        // #538: 保持期間を過ぎた時系列データファイルの削除（起動時 + 日付が
        // 変わるたび）。
        // 掃除のタスクにも A1 の排他を持たせる（[`spawn_retention`]）。
        let instance_lock = Arc::new(instance_lock);
        let retention = {
            let (settings, collect) = (settings.clone(), collect.clone());
            spawn_retention(&instance_lock, move |stop| {
                crate::retention::run(settings, collect, stop)
            })
        };

        let services = EngineServices {
            users,
            settings,
            audit,
            backup,
            hub,
            plc_connections,
            collection_groups,
            tags,
            display_groups,
            collect,
            rest_auth,
            events,
            pool,
            host: HostInfo::new(cfg.host, instance_lock.scope()),
        };

        let listen_error = listen_error.or(fallback_error);

        // 9. serve を始める。最初の管理者の作成は `allow_setup`（全部）か、
        // `loopback_only_setup` なら実際に loopback のアドレスに bind できた常時の
        // listener だけ（LAN の listener と後の入れ替えは閉じる）。
        let lan_allow_setup = cfg.allow_setup;
        let loopback_allow_setup = |bound: &BoundServer| {
            cfg.allow_setup || (loopback_only_setup && bound.local_addr().ip().is_loopback())
        };
        let server = bound.map(|bound: BoundServer| bound.serve(services.router(lan_allow_setup)));
        let loopback_server = bound_loopback.map(|bound: BoundServer| {
            let allow = loopback_allow_setup(&bound);
            bound.serve(services.router(allow))
        });

        // 10. 監査の保留の定期の流し込み（失敗しうる手順がすべて済んだ後）。
        let spool_flusher = services.audit.spawn_spool_flusher();
        if let Some(note) = &recovery_note {
            if note.choice == crate::startup_recovery::RecoveryChoice::RetryStoreCheck {
                services
                    .audit
                    .record(note.audit_entry(cfg.audit_origin))
                    .await;
            }
        }

        Ok(RunningEngine {
            services,
            server: Arc::new(ServerSlot::new(server)),
            loopback: Arc::new(ServerSlot::new(loopback_server)),
            allow_setup: lan_allow_setup,
            log_label: label,
            listen_error,
            background: std::sync::Mutex::new(BackgroundTasks {
                spool_flusher,
                retention: Arc::new(retention),
            }),
            #[cfg(test)]
            observations: std::sync::Mutex::new(None),
            _instance_lock: instance_lock,
        })
    }
}

/// 保持期間の掃除を spawn し、A1 の排他（の `Arc`）を掃除のタスクに持たせる。
/// 終了が時間切れで返り [`RunningEngine`] が drop されても、掃除が終わる
/// まで排他を返さない（別のプロセスが同じ DB を開いている間に、裏で掃除が
/// DB に触れない。PR #597 のレビュー）。
fn spawn_retention<F, Fut>(instance_lock: &Arc<InstanceGuard>, make: F) -> RetentionTask
where
    F: FnOnce(tokio::sync::watch::Receiver<bool>) -> Fut,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    crate::retention::spawn_holding(instance_lock.clone(), make)
}

/// 終了で止める背景タスク。監査の保留の流し込みは abort、保持期間の掃除は
/// 止める合図を送って終わりを待つ（モジュール doc「終了の順序」の 2.・3.）。
/// 掃除は終わりを取れるまで**取り出さない**: 終了が時間切れで手順 3. の
/// 待ちを落としても、もう一度の `shutdown` がまだ走っている掃除を待つ
/// （[`crate::retention`] の「止め方」、PR #597 のレビュー）。
struct BackgroundTasks {
    spool_flusher: Option<JoinHandle<()>>,
    retention: Arc<RetentionTask>,
}

/// [`RunningEngine::shutdown`] の手順（[`ShutdownReport::completed`] に、
/// 済んだ順に入る）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownStep {
    /// 収集の取り消せない終了の印を立てた。
    Marked,
    /// 監査の保留の定期の流し込みを abort し、保持期間の掃除に止まる合図を
    /// 送った（どちらも待たない）。
    BackgroundTasksStopRequested,
    /// listener（LAN と常時の loopback）を止めた（走っていなければ何もしない。
    /// 上限で待つのをやめたときも、待つのをやめた時点で済んだものとする）。
    ListenerStopped,
    /// 収集を止めた（最終 flush。失敗は [`ShutdownReport::collect_stop_error`]）。
    CollectStopped,
    /// Hub の購読を止めた。
    HubShutdown,
    /// 保持期間の掃除が終わった（掃除の途中なら終わってから）。収集の停止の後
    /// （モジュール doc の 6.）。
    RetentionStopped,
    /// SQLite の pool を閉じた。
    PoolClosed,
}

/// [`RunningEngine::shutdown`] の結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShutdownReport {
    /// 済んだ手順（順に）。予算が切れたらそこで途切れる。
    pub completed: Vec<ShutdownStep>,
    /// `CollectorService::stop` の失敗（最終 flush）の文言。
    pub collect_stop_error: Option<String>,
    /// 予算以内に終わらず、途中で切り上げた。
    pub timed_out: bool,
    /// listener の停止を [`listener_stop_cap`] で待ちきれず、裏に残して次へ進んだ
    /// （開いたままの要求があった）。
    pub listener_stop_abandoned: bool,
    /// 渡された予算。
    pub budget: Duration,
}

/// 終了で listener の停止を待つ上限（モジュール doc「終了の順序」の 3.）。予算の
/// 4 分の 1、最大 1 秒。残りを最終 flush（収集の停止）以降に回す。
pub fn listener_stop_cap(budget: Duration) -> Duration {
    (budget / 4).min(Duration::from_secs(1))
}

/// 動いているエンジン（[`OpenedEngine::start`]）。
pub struct RunningEngine {
    services: EngineServices,
    server: Arc<ServerSlot>,
    /// 常時の loopback の listener（[`OpenedEngine::with_loopback`]）。入れ替えない。
    loopback: Arc<ServerSlot>,
    allow_setup: bool,
    log_label: &'static str,
    listen_error: Option<String>,
    background: std::sync::Mutex<BackgroundTasks>,
    /// テスト専用: `Some` なら、終了の各手順の直前の状態を記録する。
    #[cfg(test)]
    observations: std::sync::Mutex<Option<Vec<ShutdownObservation>>>,
    /// A1 の排他。drop で返る（モジュール doc）。
    /// 保持期間の掃除のタスクも同じ `Arc` を持つので、排他が返るのは両方が
    /// いなくなったとき（[`spawn_retention`]）。
    _instance_lock: Arc<InstanceGuard>,
}

impl RunningEngine {
    /// 各サービスのハンドル。
    pub fn services(&self) -> &EngineServices {
        &self.services
    }

    /// LAN の listener の入れ物（デスクトップの `server_apply` が入れ替える）。
    pub fn server_slot(&self) -> &Arc<ServerSlot> {
        &self.server
    }

    /// 常時の loopback の listener の実際のアドレス（[`OpenedEngine::with_loopback`]
    /// を使っていなければ `None`）。`run\endpoint.json` の `port` に使う。
    pub async fn loopback_addr(&self) -> Option<SocketAddr> {
        self.loopback.local_addr().await
    }

    /// LAN に出すルーター（`allow_setup` は起動時の設定のまま）。listener を
    /// 入れ替えるときに使う。
    pub fn router(&self) -> Router {
        self.services.router(self.allow_setup)
    }

    /// [`EngineListen::BestEffort`] の待ち受けを開けなかったとき、または常時の
    /// listener を代わり（[`OpenedEngine::with_loopback_fallback`]）に落としたときの理由。
    pub fn listen_error(&self) -> Option<&str> {
        self.listen_error.as_deref()
    }

    /// 終了（モジュール doc「終了の順序」）。何度呼んでもよい（2 回目以降は
    /// 止めるものが残っていない。ただし前回が時間切れで保持期間の掃除が
    /// まだ終わっていなければ、それを待つ）が、同時に 2 回走らせないのは呼び出し側の
    /// 責任（デスクトップは `ExitCleanupOnce`）。
    pub async fn shutdown(&self, budget: Duration) -> ShutdownReport {
        let label = self.log_label;
        let services = &self.services;
        let mut completed = Vec::new();

        // 1. 取り消せない印（予算の外、待たない）。
        self.observe(ShutdownStep::Marked);
        let _ = services.collect.begin_shutdown();
        completed.push(ShutdownStep::Marked);

        // 2. 背景タスク（流し込みは abort、掃除は合図だけ。どちらも待たない）。
        self.observe(ShutdownStep::BackgroundTasksStopRequested);
        let retention = {
            let mut background = self
                .background
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(handle) = background.spool_flusher.take() {
                handle.abort();
            }
            background.retention.request_stop();
            background.retention.clone()
        };
        completed.push(ShutdownStep::BackgroundTasksStopRequested);

        let mut collect_stop_error = None;
        let mut listener_stop_abandoned = false;
        let cleanup = async {
            // 3. 消費者（LAN、続いて常時の loopback）。
            // 停止は裏のタスクで並べて始め、上限までだけ待つ（モジュール doc の 3.）。
            self.observe(ShutdownStep::ListenerStopped);
            // slot の lock の取得も上限の区間に含める（#598 の Copilot の指摘）: デスク
            // トップの `server_apply` は適用の間ずっと slot の lock を持ち、その中で古い
            // listener の graceful な停止（最大 5 秒）を待つので、lock を上限の外で待つと
            // 予算を使い切って最終 flush を飛ばしうる。上限で待つのをやめたタスクは、
            // lock が空き次第そのとき slot にあるもの（適用が置いた新しい listener を
            // 含む）を取り出して止める - 終了の後に listener を残さない。
            let mut stops = Vec::new();
            for slot in [&self.server, &self.loopback] {
                let slot = Arc::clone(slot);
                stops.push(tokio::spawn(async move {
                    let running = slot.inner.lock().await.take();
                    if let Some(running) = running {
                        running.stop().await;
                    }
                }));
            }
            let cap = listener_stop_cap(budget);
            let waited =
                tokio::time::timeout(cap, futures_util::future::join_all(stops.iter_mut())).await;
            if waited.is_err() {
                listener_stop_abandoned = true;
                eprintln!(
                    "{label}: 待ち受けの停止が{}ミリ秒で終わらなかった（開いたままの要求がある）ため、待たずに収集の停止へ進みます。",
                    cap.as_millis()
                );
            }
            completed.push(ShutdownStep::ListenerStopped);

            // 4. 書き手（収集）。走っていなければ何もしない（冪等）。失敗しても
            // 終了は止めない。
            self.observe(ShutdownStep::CollectStopped);
            if let Err(err) = services.collect.stop().await {
                eprintln!("{label}: 終了時の収集の停止に失敗しました: {err}");
                collect_stop_error = Some(err.to_string());
            }
            completed.push(ShutdownStep::CollectStopped);

            // 5. 生産者（Hub の購読）。
            self.observe(ShutdownStep::HubShutdown);
            services.hub.shutdown().await;
            completed.push(ShutdownStep::HubShutdown);

            // 6. 保持期間の掃除の終わり（abort しない。設定 DB を読むので pool より前。
            // 収集の停止の後に待つ - モジュール doc の 6.）。前回の終了が時間切れで
            // ここを落としていても、まだ走っている掃除を待つ（終わっていなければ
            // `RetentionStopped` を報告せず、pool も閉じない）。
            self.observe(ShutdownStep::RetentionStopped);
            retention.stop().await;
            completed.push(ShutdownStep::RetentionStopped);

            // 7. みんなが書き込む先（DB）。
            self.observe(ShutdownStep::PoolClosed);
            services.pool.close().await;
            completed.push(ShutdownStep::PoolClosed);
        };
        let timed_out = tokio::time::timeout(budget, cleanup).await.is_err();
        if timed_out {
            eprintln!(
                "{label}: 終了時の後始末が{}秒以内に終わらなかったため、途中で切り上げて終了します。",
                budget.as_secs()
            );
        }
        ShutdownReport {
            completed,
            collect_stop_error,
            timed_out,
            listener_stop_abandoned,
            budget,
        }
    }

    #[cfg(not(test))]
    fn observe(&self, _step: ShutdownStep) {}

    #[cfg(test)]
    fn observe(&self, step: ShutdownStep) {
        let mut slot = self
            .observations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(observations) = slot.as_mut() {
            observations.push(ShutdownObservation {
                before: step,
                marked: self.services.collect.shutdown_marked(),
                listener_open: [&self.server, &self.loopback].iter().any(|slot| {
                    slot.inner
                        .try_lock()
                        .map(|server| server.is_some())
                        .unwrap_or(true)
                }),
                collecting: self.services.collect.state().is_collecting(),
                hub_shutting_down: self.services.hub.is_shutting_down(),
                pool_closed: self.services.pool.is_closed(),
            });
        }
    }
}

/// テスト専用: 終了の手順 `before` の直前の実際の状態。
#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
struct ShutdownObservation {
    before: ShutdownStep,
    marked: bool,
    listener_open: bool,
    collecting: bool,
    hub_shutting_down: bool,
    pool_closed: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collect::{CollectEventRow, CollectorState, EventPage, Readout};
    use crate::hub::UnavailableKeyStore;
    use crate::test_support::TempDir;
    use banto_collect::simulation;
    use banto_collect::Protocol;
    use banto_tags::{CollectionGroupInput, PlcConnectionInput, TagInput};
    use std::time::Instant;

    fn config(dir: &Path, listen: EngineListen) -> EngineConfig {
        let db_path = dir.join("chronogazer.sqlite3");
        EngineConfig {
            data_base: EngineConfig::db_dir_of(&db_path),
            db_path,
            key_store: Arc::new(UnavailableKeyStore),
            attach_hub: false,
            host: ChronoGazerHost::Serve,
            listen,
            allow_setup: false,
            seed_viewer_public: false,
            log_label: "engine-test",
            audit_origin: "rest",
            store_config_required: false,
            store_dir_check_required: false,
        }
    }

    fn loopback_any_port() -> ServerConfig {
        ServerConfig {
            bind: "127.0.0.1".to_string(),
            port: 0,
        }
    }

    async fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !cond() {
            assert!(
                Instant::now() < deadline,
                "{what} が 20 秒以内に成り立たなかった"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// B1-4: 本番と同じ組み立ての listener が `GET /api/host` に未認証で答え、
    /// host 種別と A1 の排他の単位（DB のパスのハッシュ）を返す。DB のパスは出ない。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn api_host_answers_unauthenticated_with_kind_and_scope_but_no_path() {
        let dir = TempDir::new();
        let mut cfg = config(dir.path(), EngineListen::Required(loopback_any_port()));
        cfg.host = ChronoGazerHost::Server;
        let db_path = cfg.db_path.clone();
        let engine = EngineRuntime::start(cfg).await.expect("start");
        let addr = engine.server_slot().local_addr().await.expect("listening");

        let (expected_scope, _) = banto_instance_lock::path_scope_id(&db_path).unwrap();
        let client = reqwest::Client::new();
        let url = format!("http://{addr}/api/host");
        let response = client
            .get(&url)
            .header("X-Banto-Client", "banto")
            .send()
            .await
            .expect("send");
        assert_eq!(response.status(), 200);
        let json: serde_json::Value =
            serde_json::from_str(&response.text().await.expect("text")).expect("json");
        assert_eq!(json["product"], "chronogazer");
        assert_eq!(json["hostKind"], "server");
        assert_eq!(json["scope"], expected_scope.as_str());
        assert!(!json.to_string().contains(dir.path().to_str().unwrap()));

        // ヘッダが無ければ他の /api と同じく 403。
        let no_header = client.get(&url).send().await.expect("send");
        assert_eq!(no_header.status(), 403);

        engine.shutdown(Duration::from_secs(15)).await;
    }

    /// 排他の競合では `Err(InstanceLock)` を返し、DB・監査の保留・バックアップの
    /// どれにも触れない（ファイルもディレクトリも作らない）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn lock_conflict_is_an_error_and_does_not_touch_the_db() {
        let dir = TempDir::new();
        let cfg = config(dir.path(), EngineListen::Disabled);
        let db_path = cfg.db_path.clone();
        let _held = acquire_for_db(&db_path, ChronoGazerHost::Desktop).expect("先に取る");

        let err = match EngineRuntime::start(cfg).await {
            Ok(_) => panic!("同じ DB の 2 つ目の起動が通った"),
            Err(err) => err,
        };
        assert!(
            matches!(
                err,
                EngineStartError::InstanceLock(InstanceLockFailure::AlreadyRunning { .. })
            ),
            "{err:?}"
        );
        assert!(!db_path.exists(), "排他を取れないのに DB を作った");
        assert!(!dir.path().join("audit-spool").exists());
        assert!(!dir.path().join("backups").exists());
        assert!(!dir.path().join("data").exists());
    }

    /// DB を開けない（DB のパスがディレクトリ）ときは `Err(Database)`。
    /// 排他は返っている（もう一度取れる）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unopenable_db_is_an_error() {
        let dir = TempDir::new();
        let cfg = config(dir.path(), EngineListen::Disabled);
        let db_path = cfg.db_path.clone();
        std::fs::create_dir_all(&db_path).expect("DB のパスにディレクトリを置く");

        let err = match EngineRuntime::start(cfg).await {
            Ok(_) => panic!("ディレクトリを DB として開けた"),
            Err(err) => err,
        };
        assert!(
            matches!(err, EngineStartError::Database(InitDbError::Storage(_))),
            "{err:?}"
        );
        drop(
            acquire_for_db(&db_path, ChronoGazerHost::Serve)
                .expect("失敗した起動の後は排他が返っている"),
        );
    }

    /// 必須の待ち受けを開けないときは `Err(Listen)` を返し、排他と pool を返す
    /// （同じ DB でもう一度起動できる）。収集は始めていない。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn busy_required_port_is_an_error_and_releases_everything() {
        let dir = TempDir::new();
        let occupied = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = occupied.local_addr().expect("addr").port();
        let cfg = config(
            dir.path(),
            EngineListen::Required(ServerConfig {
                bind: "127.0.0.1".to_string(),
                port,
            }),
        );

        let err = match EngineRuntime::start(cfg).await {
            Ok(_) => panic!("使用中のポートで起動できた"),
            Err(err) => err,
        };
        assert!(
            matches!(&err, EngineStartError::Listen { config, .. } if config.port == port),
            "{err:?}"
        );

        // 同じ DB で、もう一度（今度は待ち受けなしで）起動できる。
        let engine = EngineRuntime::start(config(dir.path(), EngineListen::Disabled))
            .await
            .expect("失敗した起動の後に同じ DB で起動できる");
        let report = engine.shutdown(Duration::from_secs(10)).await;
        assert!(!report.timed_out);
    }

    /// 保存設定 `data.dir` を読めない DB（値を TEXT として読めない BLOB にする。
    /// Hub の `installation_id` と監査の設定は読める）で、
    /// `store_config_required` なら `Err(StoreConfig)` を返して排他と pool を返し、
    /// 収集のデータディレクトリを作らない。`false` なら既定値で起動を続ける
    /// （`banto-serve` の今までの扱い）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unreadable_store_config_stops_only_when_required() {
        let dir = TempDir::new();
        let opened = EngineRuntime::open(EngineConfig {
            store_config_required: true,
            ..config(dir.path(), EngineListen::Required(loopback_any_port()))
        })
        .await
        .expect("open");
        let db_path = dir.path().join("chronogazer.sqlite3");
        sqlx::query("INSERT INTO settings (key, value) VALUES ('data.dir', X'FF00FE')")
            .execute(opened.pool())
            .await
            .expect("data.dir を読めない値にする");
        let err = match opened.start().await {
            Ok(_) => panic!("保存設定を読めないのに起動した"),
            Err(err) => err,
        };
        assert!(
            matches!(
                err,
                EngineStartError::StoreConfig {
                    data_dir_unreadable: true,
                    ..
                }
            ),
            "{err:?}"
        );
        assert!(!dir.path().join("data").exists(), "収集の置き場を作った");
        drop(
            acquire_for_db(&db_path, ChronoGazerHost::Serve)
                .expect("失敗した起動の後は排他が返っている"),
        );

        // 必須でなければ既定値で続行する。
        let engine = EngineRuntime::start(config(dir.path(), EngineListen::Disabled))
            .await
            .expect("既定値で続行する");
        let report = engine.shutdown(Duration::from_secs(10)).await;
        assert!(!report.timed_out);
    }

    /// #392 B1-5: 常時の loopback と LAN の 2 本を同時に待ち受け、`/api/host` に
    /// どちらからも答え、終了で両方が閉じる。`loopback_addr` は loopback の方。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn loopback_and_lan_listeners_serve_together_and_both_stop() {
        let dir = TempDir::new();
        let engine = EngineRuntime::open(config(dir.path(), EngineListen::Disabled))
            .await
            .expect("open")
            .with_loopback(loopback_any_port())
            .with_listen(EngineListen::BestEffort(loopback_any_port()))
            .start()
            .await
            .expect("start");
        let loopback = engine.loopback_addr().await.expect("loopback");
        let lan = engine.server_slot().local_addr().await.expect("lan");
        assert_ne!(loopback, lan, "2 本の別々の listener");
        let client = reqwest::Client::new();
        for addr in [loopback, lan] {
            let response = client
                .get(format!("http://{addr}/api/host"))
                .header("X-Banto-Client", "banto")
                .send()
                .await
                .expect("send");
            assert_eq!(response.status(), 200, "{addr}");
        }
        let report = engine.shutdown(Duration::from_secs(10)).await;
        assert!(!report.timed_out);
        assert!(
            std::net::TcpStream::connect(loopback).is_err(),
            "loopback が残った"
        );
        assert!(std::net::TcpStream::connect(lan).is_err(), "LAN が残った");
        assert_eq!(engine.loopback_addr().await, None);
    }

    /// #598 の Copilot の指摘: デスクトップの `server_apply` のように LAN の slot の lock を
    /// 持ったまま止まっている者がいても、終了は上限で listener の待ちをやめて収集を止め、
    /// `collection_stopped` が残る。lock が空いたら、そのとき slot にある listener
    /// （適用が置いた新しいもの）も止める。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_held_server_slot_lock_does_not_skip_the_final_flush() {
        let dir = TempDir::new();
        let sim = simulation::start(Protocol::ModbusTcp).await;
        let cfg = config(dir.path(), EngineListen::Required(loopback_any_port()));
        let db_path = cfg.db_path.clone();
        let opened = EngineRuntime::open(cfg).await.expect("open");
        register_modbus(opened.pool(), sim.addr().port()).await;
        let engine = opened.start().await.expect("start");
        let collect = engine.services().collect.clone();
        wait_until("自動開始で収集が走ること", || {
            matches!(collect.state(), CollectorState::Running { .. })
        })
        .await;

        // 適用の途中: slot の lock を持ったまま、終了の予算より長く止まる。
        let slot = engine.server_slot().clone();
        let (locked_tx, locked_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        let applier = tokio::spawn(async move {
            let guard = slot.lock().await;
            let _ = locked_tx.send(());
            let _ = release_rx.await;
            drop(guard);
        });
        locked_rx.await.expect("lock を取った");

        let budget = Duration::from_secs(3);
        let report = engine.shutdown(budget).await;
        assert!(!report.timed_out, "{report:?}");
        assert!(report.listener_stop_abandoned);
        assert!(report.completed.contains(&ShutdownStep::CollectStopped));
        let lan = engine.server_slot().clone();

        // lock が空けば、裏の停止のタスクが残っていた listener を止める。
        let _ = release_tx.send(());
        applier.await.unwrap();
        wait_until("裏の停止で LAN の slot が空になること", || {
            lan.inner.try_lock().map(|s| s.is_none()).unwrap_or(false)
        })
        .await;

        drop(engine);
        let pool = init_db(&db_path).await.expect("開き直す");
        let reader = CollectorService::new(pool.clone(), dir.path().join("data"));
        let rows: Vec<CollectEventRow> = match reader.events(EventPage::default()).await {
            Readout::Ready { data } => data.rows,
            other => panic!("イベントが読めない: {}", other.as_str()),
        };
        assert_eq!(
            rows.first().map(|row| row.kind.as_str()),
            Some("collection_stopped")
        );
        pool.close().await;
    }

    /// listener の停止を待つ上限は予算の 4 分の 1、最大 1 秒（最終 flush の予算を残す）。
    #[test]
    fn listener_stop_cap_leaves_most_of_the_budget_to_the_final_flush() {
        assert_eq!(
            listener_stop_cap(Duration::from_millis(3_500)),
            Duration::from_millis(875)
        );
        assert_eq!(
            listener_stop_cap(Duration::from_secs(5)),
            Duration::from_secs(1)
        );
        assert_eq!(
            listener_stop_cap(Duration::from_secs(15)),
            Duration::from_secs(1)
        );
    }

    /// `POST /api/auth/setup` の HTTP の状態（最初の管理者の作成）。
    async fn post_setup(addr: SocketAddr) -> u16 {
        let body = serde_json::json!({
            "username": "admin",
            "password": "admin-pass-1",
            "displayName": "管理者",
        });
        reqwest::Client::new()
            .post(format!("http://{addr}/api/auth/setup"))
            .header("X-Banto-Client", "banto")
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .await
            .expect("send")
            .status()
            .as_u16()
    }

    /// bind できないアドレス（TEST-NET-3。この PC のアドレスではない）。
    fn unbindable() -> ServerConfig {
        ServerConfig {
            bind: "203.0.113.1".to_string(),
            port: 0,
        }
    }

    /// #598 のレビュー: 常時の listener（`0.0.0.0` にまとめたもの）を開けなくても、
    /// 代わりの `127.0.0.1` に落として起動を続け、理由を残す。落とした先は loopback
    /// なので最初の管理者の作成が開く。代わりが無ければ `Err(Listen)`（反証の対照）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_unbindable_primary_falls_back_to_loopback_and_keeps_running() {
        let dir = TempDir::new();
        let engine = EngineRuntime::open(config(dir.path(), EngineListen::Disabled))
            .await
            .expect("open")
            .with_loopback(unbindable())
            .with_loopback_fallback(loopback_any_port())
            .with_loopback_only_setup()
            .start()
            .await
            .expect("代わりの loopback で起動を続ける");
        let addr = engine.loopback_addr().await.expect("loopback");
        assert!(addr.ip().is_loopback(), "{addr}");
        assert!(engine.listen_error().is_some(), "落とした理由が残らない");
        assert_eq!(post_setup(addr).await, 200);
        assert!(!engine.shutdown(Duration::from_secs(10)).await.timed_out);
        drop(engine);

        // 代わりが無ければ起動を止める。
        let result = EngineRuntime::open(config(dir.path(), EngineListen::Disabled))
            .await
            .expect("open")
            .with_loopback(unbindable())
            .start()
            .await;
        assert!(matches!(result, Err(EngineStartError::Listen { .. })));
    }

    /// #598 のレビュー: `with_loopback_only_setup` は、loopback に bind できた常時の
    /// listener だけで最初の管理者の作成を開け、LAN の listener（と後の入れ替えの
    /// ルーター）は閉じる - アドレスが loopback でも LAN の slot は閉じる。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn setup_opens_only_on_the_loopback_listener() {
        let dir = TempDir::new();
        let engine = EngineRuntime::open(config(dir.path(), EngineListen::Disabled))
            .await
            .expect("open")
            .with_loopback(loopback_any_port())
            .with_listen(EngineListen::BestEffort(loopback_any_port()))
            .with_loopback_only_setup()
            .start()
            .await
            .expect("start");
        let lan = engine.server_slot().local_addr().await.expect("lan");
        let loopback = engine.loopback_addr().await.expect("loopback");
        assert_eq!(post_setup(lan).await, 403, "LAN の listener で開いている");
        assert!(!engine.allow_setup, "入れ替えのルーターで開いている");
        assert_eq!(post_setup(loopback).await, 200);
        assert!(!engine.shutdown(Duration::from_secs(10)).await.timed_out);
    }

    /// #392 B1-5: 常時の loopback は必須。ポートが塞がっていれば LAN が無くても
    /// `Err(Listen)` で戻り、排他と pool を返す（同じ DB でもう一度起動できる）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn busy_loopback_port_is_an_error_and_releases_everything() {
        let dir = TempDir::new();
        let occupied = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = occupied.local_addr().expect("addr").port();
        let result = EngineRuntime::open(config(dir.path(), EngineListen::Disabled))
            .await
            .expect("open")
            .with_loopback(ServerConfig {
                bind: "127.0.0.1".to_string(),
                port,
            })
            .start()
            .await;
        match result {
            Ok(_) => panic!("塞がった loopback のポートで起動できた"),
            Err(EngineStartError::Listen { config, .. }) => assert_eq!(config.port, port),
            Err(other) => panic!("{other:?}"),
        }
        let engine = EngineRuntime::start(config(dir.path(), EngineListen::Disabled))
            .await
            .expect("失敗した起動の後に同じ DB で起動できる");
        assert!(!engine.shutdown(Duration::from_secs(10)).await.timed_out);
    }

    /// 任意の待ち受けを開けないときは起動を続け、理由を残す。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn busy_best_effort_port_keeps_the_engine() {
        let dir = TempDir::new();
        let occupied = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = occupied.local_addr().expect("addr").port();
        let engine = EngineRuntime::start(config(
            dir.path(),
            EngineListen::BestEffort(ServerConfig {
                bind: "127.0.0.1".to_string(),
                port,
            }),
        ))
        .await
        .expect("任意の待ち受けの失敗では起動は止まらない");
        assert!(engine.listen_error().is_some());
        assert_eq!(engine.server_slot().local_addr().await, None);
        let report = engine.shutdown(Duration::from_secs(10)).await;
        assert!(!report.timed_out);
        assert!(engine.services().pool.is_closed());
    }

    /// 保持期間の掃除が終了の予算より長いとき: 1 回目の `shutdown` は時間切れで
    /// 掃除の待ちを落とすが、掃除のタスクは追い続ける。2 回目の `shutdown` は
    /// 掃除がまだ走っていれば `RetentionStopped` を報告せず（pool も閉じない）、
    /// 掃除を放せば待って最後まで終える（PR #597 のレビュー）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shutdown_again_waits_for_a_retention_sweep_that_outlived_the_budget() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let dir = TempDir::new();
        let engine = EngineRuntime::start(config(dir.path(), EngineListen::Disabled))
            .await
            .expect("start");

        // 掃除を「途中で止まっている掃除」（合図を見ずに `release` を待つ）に差し替える。
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let finished = Arc::new(AtomicBool::new(false));
        let stuck = {
            let (entered, release, finished) = (entered.clone(), release.clone(), finished.clone());
            Arc::new(RetentionTask::spawn_with(move |_stop| async move {
                entered.notify_one();
                release.notified().await;
                finished.store(true, Ordering::SeqCst);
            }))
        };
        let original = {
            let mut background = engine.background.lock().unwrap();
            std::mem::replace(&mut background.retention, stuck)
        };
        original.stop().await;
        tokio::time::timeout(Duration::from_secs(5), entered.notified())
            .await
            .expect("掃除が途中に入る");

        let short = Duration::from_millis(200);
        for attempt in 1..=2 {
            let report = engine.shutdown(short).await;
            assert!(report.timed_out, "{attempt} 回目が時間切れにならない");
            // 収集の停止までは進み（#598: 掃除の待ちは収集の停止の後）、掃除の
            // 終わりと pool を閉じる手順は報告しない。
            assert_eq!(
                report.completed,
                vec![
                    ShutdownStep::Marked,
                    ShutdownStep::BackgroundTasksStopRequested,
                    ShutdownStep::ListenerStopped,
                    ShutdownStep::CollectStopped,
                    ShutdownStep::HubShutdown,
                ],
                "{attempt} 回目: 掃除が終わっていないのに先へ進んだ"
            );
            assert!(!finished.load(Ordering::SeqCst));
            assert!(
                !engine.services().pool.is_closed(),
                "{attempt} 回目: 掃除の途中で pool を閉じた"
            );
        }

        // 掃除を放すと、次の `shutdown` は掃除の終わりを待ってから最後まで進む。
        release.notify_one();
        let report = engine.shutdown(Duration::from_secs(10)).await;
        assert!(!report.timed_out);
        assert!(report.completed.contains(&ShutdownStep::RetentionStopped));
        assert_eq!(report.completed.last(), Some(&ShutdownStep::PoolClosed));
        assert!(finished.load(Ordering::SeqCst), "掃除が最後まで走った");
        assert!(engine.services().pool.is_closed());
    }

    /// #598: 保持期間の掃除が終わらないまま（途中で止まっている）でも、コンソールを
    /// 閉じたときと同じ短い予算の終了で、収集の停止（最終 flush・`collection_stopped`）
    /// は行われる。掃除の終わりは収集の停止の後に待つので、時間切れでも pool は
    /// 閉じない（#597 の約束）。反証: 掃除の待ちを収集の停止の前に戻すと落ちる。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_long_retention_sweep_does_not_skip_the_final_flush() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let dir = TempDir::new();
        let sim = simulation::start(Protocol::ModbusTcp).await;
        let opened = EngineRuntime::open(config(dir.path(), EngineListen::Disabled))
            .await
            .expect("open");
        register_modbus(opened.pool(), sim.addr().port()).await;
        let engine = opened.start().await.expect("start");
        let collect = engine.services().collect.clone();
        wait_until("自動開始で収集が走ること", || {
            matches!(collect.state(), CollectorState::Running { .. })
        })
        .await;

        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let finished = Arc::new(AtomicBool::new(false));
        let stuck = {
            let (entered, release, finished) = (entered.clone(), release.clone(), finished.clone());
            Arc::new(RetentionTask::spawn_with(move |_stop| async move {
                entered.notify_one();
                release.notified().await;
                finished.store(true, Ordering::SeqCst);
            }))
        };
        let original = {
            let mut background = engine.background.lock().unwrap();
            std::mem::replace(&mut background.retention, stuck)
        };
        original.stop().await;
        tokio::time::timeout(Duration::from_secs(5), entered.notified())
            .await
            .expect("掃除が途中に入る");

        // コンソールを閉じたときのエンジンの予算（約 3.5 秒）。
        let report = engine.shutdown(Duration::from_millis(3_500)).await;
        assert!(
            report.timed_out,
            "掃除が終わっていないのに時間切れにならない"
        );
        assert!(
            report.completed.contains(&ShutdownStep::CollectStopped),
            "{report:?}"
        );
        assert!(!report.completed.contains(&ShutdownStep::RetentionStopped));
        assert!(
            !engine.services().pool.is_closed(),
            "掃除の途中で pool を閉じた"
        );
        let rows: Vec<CollectEventRow> = match collect.events(EventPage::default()).await {
            Readout::Ready { data } => data.rows,
            other => panic!("イベントが読めない: {}", other.as_str()),
        };
        assert_eq!(
            rows.first().map(|row| row.kind.as_str()),
            Some("collection_stopped")
        );

        release.notify_one();
        let report = engine.shutdown(Duration::from_secs(10)).await;
        assert!(!report.timed_out);
        assert_eq!(report.completed.last(), Some(&ShutdownStep::PoolClosed));
        assert!(finished.load(Ordering::SeqCst));
    }

    /// 終了が時間切れで返り、エンジンが drop されても、まだ走っている保持期間の
    /// 掃除が A1 の排他を持ち続ける（別のプロセスが同じ DB を開けない）。掃除が
    /// 終わると排他が返る（PR #597 のレビュー）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_retention_sweep_that_outlived_shutdown_keeps_the_instance_lock() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let dir = TempDir::new();
        let cfg = config(dir.path(), EngineListen::Disabled);
        let db_path = cfg.db_path.clone();
        let engine = EngineRuntime::start(cfg).await.expect("start");

        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let finished = Arc::new(AtomicBool::new(false));
        let stuck = {
            let (entered, release, finished) = (entered.clone(), release.clone(), finished.clone());
            Arc::new(spawn_retention(
                &engine._instance_lock,
                move |_stop| async move {
                    entered.notify_one();
                    release.notified().await;
                    finished.store(true, Ordering::SeqCst);
                },
            ))
        };
        let original = {
            let mut background = engine.background.lock().unwrap();
            std::mem::replace(&mut background.retention, stuck)
        };
        original.stop().await;
        tokio::time::timeout(Duration::from_secs(5), entered.notified())
            .await
            .expect("掃除が途中に入る");

        let report = engine.shutdown(Duration::from_millis(200)).await;
        assert!(report.timed_out);
        drop(engine);
        assert!(
            matches!(
                acquire_for_db(&db_path, ChronoGazerHost::Serve),
                Err(InstanceLockFailure::AlreadyRunning { .. })
            ),
            "掃除が走っているのに排他が返った"
        );

        release.notify_one();
        wait_until("掃除が終わって排他が返ること", || {
            acquire_for_db(&db_path, ChronoGazerHost::Serve).is_ok()
        })
        .await;
        assert!(finished.load(Ordering::SeqCst));
    }

    /// レジストリに「シミュレータの Modbus TCP 接続 1 / グループ 1 / タグ 1」を
    /// 入れる（`tests/collect_roundtrip.rs` の `register` と同じ形）。
    async fn register_modbus(pool: &DbPool, port: u16) {
        let conn = PlcConnectionService::new(pool.clone())
            .create(PlcConnectionInput {
                name: "engine-sim".to_string(),
                protocol: "modbus-tcp".to_string(),
                host: "127.0.0.1".to_string(),
                port: i64::from(port),
                unit_id: 1,
                enabled: true,
                simulation: false,
                word_order: String::new(),
                database: None,
                username: None,
                password: None,
            })
            .await
            .expect("接続");
        let group = CollectionGroupService::new(pool.clone())
            .create(CollectionGroupInput {
                name: "engine".to_string(),
                plc_connection_id: conn.id,
                period_ms: 100,
                enabled: true,
                default_writable: false,
                query_sql: None,
            })
            .await
            .expect("グループ");
        TagService::new(pool.clone())
            .create(TagInput {
                name: "ランプ".to_string(),
                collection_group_id: group.id,
                address: "40001".to_string(),
                data_type: "u16".to_string(),
                string_length: None,
                string_encoding: "utf8".to_string(),
                raw_lo: None,
                raw_hi: None,
                eng_lo: None,
                eng_hi: None,
                unit: None,
                decimals: 0,
                enabled: true,
                writable: false,
                tag_kind: "plc".to_string(),
                expression: None,
                retain: false,
                expected_revision: None,
            })
            .await
            .expect("タグ");
    }

    /// 終了の順序: 印 → 背景タスク → 保持期間の掃除の終わり → listener → 収集
    /// → Hub → pool。シミュレータ
    /// 相手に**本当に収集している**エンジンで、各手順の直前の実際の状態を見る。
    /// どの 2 つを入れ替えても、どこかの直前の状態が食い違って落ちる。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shutdown_runs_in_order_against_a_collecting_engine() {
        let dir = TempDir::new();
        let sim = simulation::start(Protocol::ModbusTcp).await;
        let cfg = config(dir.path(), EngineListen::Required(loopback_any_port()));
        let db_path = cfg.db_path.clone();

        let opened = EngineRuntime::open(cfg).await.expect("open");
        register_modbus(opened.pool(), sim.addr().port()).await;
        let engine = opened.start().await.expect("start");
        let addr = engine
            .server_slot()
            .local_addr()
            .await
            .expect("待ち受けている");
        std::net::TcpStream::connect(addr).expect("listener に繋がる");

        // 自動開始で収集が走る。
        let collect = engine.services().collect.clone();
        wait_until("自動開始で収集が走ること", || {
            matches!(collect.state(), CollectorState::Running { .. })
        })
        .await;

        *engine.observations.lock().unwrap() = Some(Vec::new());
        let report = engine.shutdown(Duration::from_secs(15)).await;
        let observations = engine.observations.lock().unwrap().take().unwrap();

        assert_eq!(
            report.completed,
            vec![
                ShutdownStep::Marked,
                ShutdownStep::BackgroundTasksStopRequested,
                ShutdownStep::ListenerStopped,
                ShutdownStep::CollectStopped,
                ShutdownStep::HubShutdown,
                ShutdownStep::RetentionStopped,
                ShutdownStep::PoolClosed,
            ]
        );
        assert!(!report.timed_out);
        assert_eq!(report.collect_stop_error, None);

        let obs = |before: ShutdownStep| ShutdownObservation {
            before,
            marked: false,
            listener_open: false,
            collecting: false,
            hub_shutting_down: false,
            pool_closed: false,
        };
        let expected = vec![
            // 何も止めていない。
            ShutdownObservation {
                listener_open: true,
                collecting: true,
                ..obs(ShutdownStep::Marked)
            },
            // 印だけが立っている。
            ShutdownObservation {
                marked: true,
                listener_open: true,
                collecting: true,
                ..obs(ShutdownStep::BackgroundTasksStopRequested)
            },
            ShutdownObservation {
                marked: true,
                listener_open: true,
                collecting: true,
                ..obs(ShutdownStep::ListenerStopped)
            },
            // listener は止まり、収集はまだ走っている。
            ShutdownObservation {
                marked: true,
                collecting: true,
                ..obs(ShutdownStep::CollectStopped)
            },
            // 収集が止まり、Hub はまだ止めていない。
            ShutdownObservation {
                marked: true,
                ..obs(ShutdownStep::HubShutdown)
            },
            // Hub が止まり、保持期間の掃除の終わりを待つ前（収集は止まっている）。
            ShutdownObservation {
                marked: true,
                hub_shutting_down: true,
                ..obs(ShutdownStep::RetentionStopped)
            },
            // 掃除が終わり、pool はまだ開いている。
            ShutdownObservation {
                marked: true,
                hub_shutting_down: true,
                ..obs(ShutdownStep::PoolClosed)
            },
        ];
        assert_eq!(observations, expected);
        assert!(engine.services().pool.is_closed());
        assert!(
            std::net::TcpStream::connect(addr).is_err(),
            "終了後も listener に繋がる"
        );
        // 終了の印の後は開始を断る。
        assert!(engine.services().collect.start().await.is_err());

        // 最終 flush が通った: 別に開き直した DB に `collection_stopped` が残る。
        drop(engine);
        let pool = init_db(&db_path).await.expect("開き直す");
        let reader = CollectorService::new(pool.clone(), dir.path().join("data"));
        let rows: Vec<CollectEventRow> = match reader.events(EventPage::default()).await {
            Readout::Ready { data } => data.rows,
            other => panic!("イベントが読めない: {}", other.as_str()),
        };
        assert_eq!(
            rows.first().map(|row| row.kind.as_str()),
            Some("collection_stopped"),
            "最新のイベントが collection_stopped でない: {:?}",
            rows.iter().map(|row| row.kind.as_str()).collect::<Vec<_>>()
        );
        pool.close().await;
    }
}
