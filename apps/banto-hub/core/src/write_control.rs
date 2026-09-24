//! 書き込み受付状態 (docs/tag-server-design.md §6-6)。[`WriteControl`] は
//! 「この hub プロセスはいま `/api/v1/values/{tag}` への書き込みを受け付けて
//! よいか」を持つ、読み取りがロックフリーの薄いフラグ保持者。
//!
//! ## ルール: 既定は有効、再起動では永続値を復元する
//!
//! (2026-09-09 オーナー決定, #340 - 旧ルール「起動時は必ず disabled」を撤回)
//!
//! 永続値の seed は `write_control_state.enabled_persisted = 1`
//! (`db.rs::apply_app_schema` 参照、既定で書き込み可)。banto-hub は
//! relay-wright のようなルールエンジンを持たない、外部クライアントの明示
//! 要求を1回転送するだけのパススルーであり、再起動後に条件が揃えば自律的に
//! 書き込みを再開するという危険が存在しない。書き込みの可否は per-tag
//! `writable` (既定 false) と API キーの `write` スコープが実質担っており、
//! このグローバルトグルは運用者が手で書き込みを止める**非常停止スイッチ**に
//! 徹する。手動で止めた状態はプロセス再起動を跨いで保持される。
//!
//! ## 停止の状態は 2 か所に記録する (#433、2026-09-24)
//!
//! #340 のままでは、停止を DB に保存できなかったとき (DB が完全に落ちて
//! いる等) に再起動すると、最後に保存された値 (有効) に**黙って戻った**。
//! これを塞ぐため、状態を DB (`write_control_state`) と、データ
//! ディレクトリ内の小さな状態ファイル ([`STATE_FILE_NAME`]、
//! [`state_file_path`]) の 2 か所に書く:
//!
//! - **起動**: 「どちらか一方でも停止なら停止」で起動する
//!   ([`decide_startup`]、純関数。総当たりの表はテスト参照)。DB を読めない・
//!   ファイルが壊れている・読めない場合も停止側に倒す。ファイルが無い
//!   (#433 より前からの環境・初回起動) だけは DB の値に従う。食い違いは
//!   [`StartupDecision::warning`] としてログと管理画面 (状態画面の
//!   「書き込み受付」) に出す。
//! - **停止** ([`WriteControl::set_enabled`] の `false`): ライブフラグを
//!   **ロックを取る前に**落とし (書き込みは即座に止まる)、状態ファイル → DB
//!   の順に保存する。どちらか一方に保存できれば「停止した」(再起動しても
//!   停止のまま) とし、保存できなかった側は [`WriteControlChange::warning`]
//!   で応答・ログ・監査に出す。両方失敗したときだけ失敗とする。
//! - **再開** (`true`): 両方の保存に成功したときだけライブフラグを立てる。
//!   片方でも失敗したらライブフラグには触れずにエラーを返す。
//!
//! ### ロック順序 (一方向に固定)
//!
//! `op_lock` (tokio の非同期ロック。保存の一連を直列化) → `stop_generation`
//! (std の同期ロック。ライブフラグの切り替えだけを囲む短い区間)。逆順に
//! 取る経路は無い: 停止は `stop_generation` を取って世代を進めライブフラグを
//! 落とし、**それを離してから** `op_lock` を待つ。再開は `op_lock` の中で
//! 開始時の世代を覚え、保存が済んだら `stop_generation` を取って世代が
//! 変わっていないときだけライブフラグを立てる。これで:
//!
//! - 再開の保存が DB 待ちで詰まっていても、停止はライブフラグを即座に
//!   落とせる (停止が再開の後ろに並ばない)。
//! - 再開の保存中に停止が割り込んだら、その再開はライブフラグを立てない
//!   ([`WriteControlChange::interrupted_by_stop`])。割り込んだ停止は再開の
//!   後で保存するので、最後に残る永続値も停止になる。
//!
//! ### 停止の DB 保存は 5 秒で打ち切る (#433 監査、2026-09-24)
//!
//! DB がエラーを返さずに固まると、停止の応答が DB を待って返らない
//! (ライブフラグと状態ファイルは先に済んでいても)。そこで**停止の DB 保存
//! だけ**に [`STOP_DB_SAVE_TIMEOUT`] (5 秒) の制限を付け、超えたら
//! 「DB に保存できなかった (タイムアウト)」として扱う
//! ([`WriteControlChange::db_timed_out`])。状態ファイルに保存できていれば
//! 停止は成功 (200)。
//!
//! - 打ち切っても DB への書き込みは**取り消さない** (取り消しても SQLite の
//!   接続側で実行され得るので、完了を追えなくなるだけ)。書き込みは別タスクで
//!   続け、その完了を `lagging_stop_db` に覚える。中身は「停止」なので、遅れて
//!   完了しても安全側。
//! - **次の再開は、遅れている停止の DB 書き込みの完了を待ってから**自分の
//!   保存を始める (`op_lock` の中で待つ)。これで「再開の『有効』を、遅れて
//!   届いた停止の『停止』が上書きする」順序の逆転が起きない。
//! - 遅れて完了したら、その停止より新しい停止・再開が記録されていない
//!   ときに限り、注意書きをその結果で更新する (両方に保存できたなら消える)。
//!   古い結果が新しい表示を巻き戻さないよう、記録の通し番号で比べる。
//! - **再開には制限を付けない**: 打ち切った書き込みが後から「有効」を DB に
//!   残すと、失敗を返したのに再起動で有効になり得るため。
//!
//! REST の停止は、#431 のセッション照合 (`crate::rest` の
//! `STOP_SESSION_CHECK_TIMEOUT`、同じ 5 秒) を通ってから来るので、DB が
//! 固まっているときの REST の停止の応答は最長でおよそ 5 + 5 秒になる。
//!
//! ### ファイルの書き込み中に落ちた場合
//!
//! 一時ファイル (`<名前>.tmp`) に書いて `sync_all` し、`rename` で置き換える
//! (Unix では親ディレクトリも `sync_all`)。途中で落ちても、状態ファイルは
//! 前の内容か新しい内容のどちらかで、半端な内容は残らない。残った一時
//! ファイルは読まず、次の書き込みで上書きする。
//!
//! [`WriteControl::was_enabled_before_restart`] は「起動時に復元した値」を
//! 指す名称として残す (外部クライアント Thermal Monitor が
//! `GET /api/v1/status` の `write_was_enabled_before_restart` を読んでいる
//! ため、フィールド名・REST フィールド名とも互換のため変更しない)。

use std::future::Future;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as SyncMutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use banto_core::BantoError;
use serde_json::{json, Value};
use sqlx::SqlitePool;
use tokio::sync::Mutex as AsyncMutex;
use tokio::task::JoinHandle;

use crate::hub_log::{log_err_line, log_line};

/// データディレクトリ内の状態ファイル名 (#433)。tstore の剪定
/// (`banto_tstore::prune_files`) は `YYYYMMDD-NNN.sqlite3` の形のファイル
/// しか触らないので、同じディレクトリに置いても消されない。
pub const STATE_FILE_NAME: &str = "write-control-state.json";

/// 停止の DB 保存の制限時間 (このモジュール doc「停止の DB 保存は 5 秒で
/// 打ち切る」)。#431 のセッション照合の制限 (`crate::rest` の
/// `STOP_SESSION_CHECK_TIMEOUT`) と同じ 5 秒。再開には使わない。
pub const STOP_DB_SAVE_TIMEOUT: Duration = Duration::from_secs(5);

/// 状態ファイルの形式番号。これ以外の値は「壊れている」として停止側に倒す。
const STATE_FILE_FORMAT: u64 = 1;

/// `data_dir` (実効値。`BANTO_HUB_DATA` 等の上書き適用後) の中の状態ファイル。
pub fn state_file_path(data_dir: &Path) -> PathBuf {
    data_dir.join(STATE_FILE_NAME)
}

// --- 起動時の判定 (純関数) ----------------------------------------------------

/// 起動時に DB (`write_control_state`) から読んだ状態。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DbStartupState {
    Enabled,
    Disabled,
    /// 読めなかった (理由)。
    Unreadable(String),
}

/// 起動時に状態ファイルから読んだ状態。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileStartupState {
    /// ファイルが無い (#433 より前からの環境、または初回起動)。
    Absent,
    Enabled,
    Disabled,
    /// 読めない・壊れている (理由。パスを含む)。
    Corrupt(String),
}

/// 起動時にライブフラグをどうするか。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartupDecision {
    pub enabled: bool,
    /// DB と状態ファイルの食い違い・読めなかったことの説明。無ければ `None`。
    pub warning: Option<String>,
}

/// 起動時の判定 (#433)。**どちらか一方でも停止なら停止**。DB を読めない・
/// ファイルが壊れているときも停止。ファイルが無いときだけ DB に従う。
/// 総当たりの表は `tests::startup_decision_table` を参照。
pub fn decide_startup(db: &DbStartupState, file: &FileStartupState) -> StartupDecision {
    let db_allows = matches!(db, DbStartupState::Enabled);
    let file_allows = matches!(file, FileStartupState::Absent | FileStartupState::Enabled);
    let enabled = db_allows && file_allows;

    let mut problems: Vec<String> = Vec::new();
    if let DbStartupState::Unreadable(reason) = db {
        problems.push(format!("DB から状態を読めませんでした（{reason}）"));
    }
    if let FileStartupState::Corrupt(reason) = file {
        problems.push(format!("状態ファイルを読めませんでした（{reason}）"));
    }
    match (db, file) {
        (DbStartupState::Enabled, FileStartupState::Disabled) => problems.push(
            "DB は「有効」、状態ファイルは「停止」でした（前回の停止を DB に保存できなかった可能性があります）"
                .to_string(),
        ),
        (DbStartupState::Disabled, FileStartupState::Enabled) => problems.push(
            "DB は「停止」、状態ファイルは「有効」でした（前回の停止を状態ファイルに保存できなかった可能性があります）"
                .to_string(),
        ),
        _ => {}
    }

    let warning = if problems.is_empty() {
        None
    } else {
        Some(format!(
            "{}。安全のため書き込み受付を停止で起動しました。状態を確かめてから再開してください。",
            problems.join("／")
        ))
    };
    StartupDecision { enabled, warning }
}

/// 起動時に DB と状態ファイルを読み、[`decide_startup`] で判定する。
pub async fn load_startup_decision(pool: &SqlitePool, state_file: &Path) -> StartupDecision {
    let db = match load_persisted_enabled(pool).await {
        Ok(true) => DbStartupState::Enabled,
        Ok(false) => DbStartupState::Disabled,
        Err(err) => DbStartupState::Unreadable(err.to_string()),
    };
    let file = read_state_file(state_file).await;
    decide_startup(&db, &file)
}

/// 状態ファイルを読む。無ければ [`FileStartupState::Absent`]、読めない・
/// 形式が違うなら [`FileStartupState::Corrupt`]。
pub async fn read_state_file(path: &Path) -> FileStartupState {
    let owned = path.to_path_buf();
    match tokio::task::spawn_blocking(move || read_state_file_blocking(&owned)).await {
        Ok(state) => state,
        Err(err) => FileStartupState::Corrupt(format!("{}: {err}", path.display())),
    }
}

fn read_state_file_blocking(path: &Path) -> FileStartupState {
    match std::fs::read(path) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => FileStartupState::Absent,
        Err(err) => FileStartupState::Corrupt(format!("{}: {err}", path.display())),
        Ok(bytes) => match parse_state_file(&bytes) {
            Ok(true) => FileStartupState::Enabled,
            Ok(false) => FileStartupState::Disabled,
            Err(reason) => FileStartupState::Corrupt(format!("{}: {reason}", path.display())),
        },
    }
}

fn parse_state_file(bytes: &[u8]) -> Result<bool, String> {
    let value: Value = serde_json::from_slice(bytes).map_err(|err| err.to_string())?;
    if value.get("format").and_then(Value::as_u64) != Some(STATE_FILE_FORMAT) {
        return Err("形式番号が違います".to_string());
    }
    value
        .get("writeEnabled")
        .and_then(Value::as_bool)
        .ok_or_else(|| "writeEnabled がありません".to_string())
}

/// 状態ファイルを原子的に書く (一時ファイル → `sync_all` → `rename`)。
/// このモジュール doc「ファイルの書き込み中に落ちた場合」参照。
pub async fn write_state_file(
    path: &Path,
    enabled: bool,
    actor: Option<&str>,
) -> Result<(), String> {
    let owned = path.to_path_buf();
    let actor = actor.map(str::to_string);
    tokio::task::spawn_blocking(move || {
        write_state_file_blocking(&owned, enabled, actor.as_deref())
            .map_err(|err| format!("{}: {err}", owned.display()))
    })
    .await
    .map_err(|err| format!("{}: {err}", path.display()))?
}

fn write_state_file_blocking(
    path: &Path,
    enabled: bool,
    actor: Option<&str>,
) -> std::io::Result<()> {
    let dir = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "親ディレクトリがありません",
        )
    })?;
    std::fs::create_dir_all(dir)?;
    let changed_at_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let body = serde_json::to_vec_pretty(&json!({
        "format": STATE_FILE_FORMAT,
        "writeEnabled": enabled,
        "changedAtUnixMs": changed_at_ms,
        "changedBy": actor,
    }))
    .map_err(std::io::Error::other)?;

    let mut tmp_name = path.as_os_str().to_owned();
    tmp_name.push(".tmp");
    let tmp = PathBuf::from(tmp_name);
    let result = (|| {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(&body)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result?;
    #[cfg(unix)]
    {
        if let Ok(dir_handle) = std::fs::File::open(dir) {
            let _ = dir_handle.sync_all();
        }
    }
    Ok(())
}

// --- ライブフラグと停止・再開 ---------------------------------------------------

/// ライブの「書き込み受付中か」フラグ + 起動時に復元した値 + 停止・再開の
/// 保存 (#433)。`Arc` で共有する前提。
#[derive(Debug)]
pub struct WriteControl {
    /// ライブの「/api/v1/values/{tag} への書き込みを受け付けるか」フラグ。
    /// 読み取りはロックを取らない。
    enabled: AtomicBool,
    /// 起動時に復元した値 (= 構築時のライブフラグの初期値)。
    was_enabled_before_restart: bool,
    /// 状態ファイル。`None` は状態ファイルを持たない構成 ([`Self::new`]、
    /// 他機能のテスト用) - そのときは DB だけに保存する (#433 より前の挙動)。
    state_file: Option<PathBuf>,
    /// 停止の世代。停止のたびに進む。ライブフラグの切り替えはこのロックの
    /// 中で行う (このモジュール doc「ロック順序」)。
    stop_generation: SyncMutex<u64>,
    /// 停止・再開の保存の一連を直列化する (このモジュール doc「ロック順序」)。
    op_lock: AsyncMutex<()>,
    /// いまの保存状態の注意書き (起動時の食い違い、または直近の停止・再開の
    /// 保存の失敗)。両方に保存できた停止・再開で消える。遅れて完了した停止の
    /// DB 書き込みも更新するので `Arc` で共有する。
    persistence_warning: Arc<SyncMutex<WarningSlot>>,
    /// 制限時間を超えてまだ終わっていない、停止の DB 書き込み。次の再開は
    /// これの完了を待つ (このモジュール doc「停止の DB 保存は 5 秒で打ち切る」)。
    lagging_stop_db: SyncMutex<Vec<JoinHandle<()>>>,
}

/// 注意書きと、それを記録した通し番号 (遅れて届く結果が新しい表示を巻き戻さ
/// ないため)。
#[derive(Debug, Default)]
struct WarningSlot {
    seq: u64,
    warning: Option<String>,
}

/// 停止・再開 1 回の結果 ([`WriteControl::set_enabled`])。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteControlChange {
    /// 要求した値 (`true` = 再開、`false` = 停止)。
    pub requested_enabled: bool,
    /// DB への保存の結果。
    pub db: Result<(), String>,
    /// 状態ファイルへの保存の結果。`None` は状態ファイルを持たない構成。
    pub file: Option<Result<(), String>>,
    /// 再開の保存中に停止が割り込んだため、再開しなかった。
    pub interrupted_by_stop: bool,
    /// 停止の DB 保存が [`STOP_DB_SAVE_TIMEOUT`] を超えたため打ち切った
    /// (`db` は `Err`)。書き込み自体は続いていて、遅れて完了し得る。
    pub db_timed_out: bool,
}

impl WriteControlChange {
    pub fn db_saved(&self) -> bool {
        self.db.is_ok()
    }

    /// 状態ファイルに保存できたか (`None` = 状態ファイルを持たない構成)。
    pub fn file_saved(&self) -> Option<bool> {
        self.file.as_ref().map(Result::is_ok)
    }

    pub fn db_error(&self) -> Option<&str> {
        self.db.as_ref().err().map(String::as_str)
    }

    pub fn file_error(&self) -> Option<&str> {
        self.file
            .as_ref()
            .and_then(|r| r.as_ref().err())
            .map(String::as_str)
    }

    /// 要求どおりになり、再起動後もそのまま残るか。
    /// - 停止: どちらか一方に保存できた。
    /// - 再開: 両方に保存でき (状態ファイルを持たない構成では DB だけ)、
    ///   停止に割り込まれなかった。
    pub fn succeeded(&self) -> bool {
        if self.requested_enabled {
            self.db_saved() && self.file_saved().unwrap_or(true) && !self.interrupted_by_stop
        } else {
            self.db_saved() || self.file_saved() == Some(true)
        }
    }

    /// 両方に保存できたか (状態ファイルを持たない構成では DB だけ)。
    pub fn fully_persisted(&self) -> bool {
        self.db_saved() && self.file_saved().unwrap_or(true)
    }

    /// 応答・ログ・監査に出す説明。全部うまくいったときは `None`。
    pub fn warning(&self) -> Option<String> {
        if self.interrupted_by_stop {
            return Some("再開の保存中に停止が要求されたため、再開しませんでした。".to_string());
        }
        if self.fully_persisted() {
            return None;
        }
        let db_part = match &self.db {
            Ok(()) => "DB: 保存済み".to_string(),
            Err(_) if self.db_timed_out => format!(
                "DB: タイムアウト（{} 秒以内に応答がありませんでした。遅れて保存される場合があります）",
                STOP_DB_SAVE_TIMEOUT.as_secs()
            ),
            Err(err) => format!("DB: 失敗（{err}）"),
        };
        let file_part = match &self.file {
            None => None,
            Some(Ok(())) => Some("状態ファイル: 保存済み".to_string()),
            Some(Err(err)) => Some(format!("状態ファイル: 失敗（{err}）")),
        };
        let detail = match file_part {
            Some(file_part) => format!("{db_part}／{file_part}"),
            None => db_part,
        };
        Some(if self.requested_enabled {
            format!("再開を保存できなかったため、書き込み受付を有効にしませんでした（{detail}）。")
        } else if self.succeeded() {
            format!(
                "書き込み受付を停止しました。保存できなかった側があります（{detail}）。\
                 保存できた側が停止を記録しているので、再起動しても停止のまま起動します。"
            )
        } else {
            format!(
                "書き込み受付は停止しましたが、どこにも保存できませんでした（{detail}）。\
                 再起動すると最後に保存した状態に戻ります。"
            )
        })
    }
}

impl WriteControl {
    /// 状態ファイルを持たない構成で構築する (他機能のテスト用。停止・再開は
    /// DB だけに保存する = #433 より前の挙動)。本番の起動経路
    /// (`crate::runtime`) は [`Self::restore`] を使う。
    pub fn new(enabled: bool) -> Self {
        Self::build(enabled, None, None)
    }

    /// 起動時の判定 ([`load_startup_decision`]) から構築する。`state_file` は
    /// [`state_file_path`] で求めたパス。
    pub fn restore(decision: StartupDecision, state_file: PathBuf) -> Self {
        Self::build(decision.enabled, Some(state_file), decision.warning)
    }

    fn build(enabled: bool, state_file: Option<PathBuf>, warning: Option<String>) -> Self {
        Self {
            enabled: AtomicBool::new(enabled),
            was_enabled_before_restart: enabled,
            state_file,
            stop_generation: SyncMutex::new(0),
            op_lock: AsyncMutex::new(()),
            persistence_warning: Arc::new(SyncMutex::new(WarningSlot { seq: 0, warning })),
            lagging_stop_db: SyncMutex::new(Vec::new()),
        }
    }

    /// ライブフラグだけを立てる (保存しない。テスト用)。運用の再開は
    /// [`Self::set_enabled`] を使う。
    pub fn enable(&self) {
        let _generation = lock_ignoring_poison(&self.stop_generation);
        self.enabled.store(true, Ordering::SeqCst);
    }

    /// ライブフラグだけを落とし、停止の世代を進める (保存しない)。
    pub fn disable(&self) {
        let mut generation = lock_ignoring_poison(&self.stop_generation);
        *generation = generation.wrapping_add(1);
        self.enabled.store(false, Ordering::SeqCst);
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::SeqCst)
    }

    /// 起動時に復元した値 (`GET /api/v1/status` の
    /// `write_was_enabled_before_restart`)。以後の停止・再開では変わらない。
    pub fn was_enabled_before_restart(&self) -> bool {
        self.was_enabled_before_restart
    }

    /// 状態ファイルのパス (`None` = 状態ファイルを持たない構成)。
    pub fn state_file(&self) -> Option<&Path> {
        self.state_file.as_deref()
    }

    /// いまの保存状態の注意書き (管理画面の「書き込み受付」に出す)。
    pub fn persistence_warning(&self) -> Option<String> {
        lock_ignoring_poison(&self.persistence_warning)
            .warning
            .clone()
    }

    /// 停止 (`false`) / 再開 (`true`) し、DB と状態ファイルに保存する
    /// (このモジュール doc「停止の状態は 2 か所に記録する」)。
    pub async fn set_enabled(
        &self,
        pool: &SqlitePool,
        enabled: bool,
        actor: Option<&str>,
    ) -> WriteControlChange {
        let pool = pool.clone();
        let db_actor = actor.map(str::to_string);
        self.set_enabled_with(enabled, actor, async move {
            persist_enabled(&pool, enabled, db_actor.as_deref())
                .await
                .map_err(|err| err.to_string())
        })
        .await
    }

    /// [`Self::set_enabled`] の本体。DB への保存を future として受け取る
    /// (テストで DB の失敗・詰まりを差し込むため)。停止では、制限時間を
    /// 超えても書き込みを続けられるよう別タスクで走らせるので `Send + 'static`。
    pub async fn set_enabled_with<F>(
        &self,
        enabled: bool,
        actor: Option<&str>,
        persist_db: F,
    ) -> WriteControlChange
    where
        F: Future<Output = Result<(), String>> + Send + 'static,
    {
        if enabled {
            let _op = self.op_lock.lock().await;
            // 遅れている停止の DB 書き込みが終わるまで待つ (再開の「有効」を
            // 後から「停止」で上書きされないように)。再開に制限時間は無い。
            let lagging: Vec<JoinHandle<()>> =
                std::mem::take(&mut *lock_ignoring_poison(&self.lagging_stop_db));
            for handle in lagging {
                let _ = handle.await;
            }
            let started_generation = *lock_ignoring_poison(&self.stop_generation);
            let file = self.persist_file(true, actor).await;
            let db = persist_db.await;
            let mut change = WriteControlChange {
                requested_enabled: true,
                db,
                file,
                interrupted_by_stop: false,
                db_timed_out: false,
            };
            if change.fully_persisted() {
                let generation = lock_ignoring_poison(&self.stop_generation);
                if *generation == started_generation {
                    self.enabled.store(true, Ordering::SeqCst);
                } else {
                    change.interrupted_by_stop = true;
                }
            }
            self.record_outcome(&change);
            change
        } else {
            // ロックを取る前にライブフラグを落とす (書き込みは即座に止まる)。
            self.disable();
            let _op = self.op_lock.lock().await;
            let file = self.persist_file(false, actor).await;
            let mut task = tokio::spawn(persist_db);
            let (db, db_timed_out) =
                match tokio::time::timeout(STOP_DB_SAVE_TIMEOUT, &mut task).await {
                    Ok(joined) => (flatten_join(joined), false),
                    Err(_) => (
                        Err(format!(
                            "{} 秒以内に応答がありませんでした",
                            STOP_DB_SAVE_TIMEOUT.as_secs()
                        )),
                        true,
                    ),
                };
            let change = WriteControlChange {
                requested_enabled: false,
                db,
                file,
                interrupted_by_stop: false,
                db_timed_out,
            };
            let seq = self.record_outcome(&change);
            if db_timed_out {
                self.follow_lagging_stop_db(task, change.file.clone(), seq);
            }
            change
        }
    }

    /// 打ち切った停止の DB 書き込みを見届ける (`op_lock` の中で呼ぶ)。完了
    /// したら、この停止より新しい記録が無いときだけ注意書きを更新する。
    fn follow_lagging_stop_db(
        &self,
        task: JoinHandle<Result<(), String>>,
        file: Option<Result<(), String>>,
        seq: u64,
    ) {
        let slot = self.persistence_warning.clone();
        let follower = tokio::spawn(async move {
            let late = WriteControlChange {
                requested_enabled: false,
                db: flatten_join(task.await),
                file,
                interrupted_by_stop: false,
                db_timed_out: false,
            };
            match &late.db {
                Ok(()) => log_line("banto-hub: 書き込み受付の停止を、遅れて DB に保存しました"),
                Err(err) => log_err_line(&format!(
                    "banto-hub: 書き込み受付の停止を DB に保存できませんでした（遅れて失敗）: {err}"
                )),
            }
            let mut slot = lock_ignoring_poison(&slot);
            if slot.seq == seq {
                slot.warning = late.warning();
            }
        });
        lock_ignoring_poison(&self.lagging_stop_db).push(follower);
    }

    async fn persist_file(&self, enabled: bool, actor: Option<&str>) -> Option<Result<(), String>> {
        match &self.state_file {
            None => None,
            Some(path) => Some(write_state_file(path, enabled, actor).await),
        }
    }

    /// `op_lock` の中で呼ぶ。注意書きを更新し、失敗をログに出す。記録の
    /// 通し番号を返す。
    fn record_outcome(&self, change: &WriteControlChange) -> u64 {
        let mut slot = lock_ignoring_poison(&self.persistence_warning);
        if change.interrupted_by_stop {
            // 割り込んだ停止が、この後で自分の保存結果を記録する。
            log_err_line(
                "banto-hub: 書き込み受付の再開は、保存中に停止が要求されたため取りやめました",
            );
            return slot.seq;
        }
        let warning = change.warning();
        if let Some(message) = &warning {
            log_err_line(&format!("banto-hub: {message}"));
        }
        slot.seq = slot.seq.wrapping_add(1);
        slot.warning = warning;
        slot.seq
    }
}

fn flatten_join(joined: Result<Result<(), String>, tokio::task::JoinError>) -> Result<(), String> {
    joined.unwrap_or_else(|err| Err(err.to_string()))
}

fn lock_ignoring_poison<T>(mutex: &SyncMutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

// --- DB (write_control_state) ----------------------------------------------

/// `write_control_state.enabled_persisted` (id=1 の単一行) を読む。
/// `db.rs::apply_app_schema` が起動時に必ず1行 seed するので
/// `fetch_one` で問題ない。
pub async fn load_persisted_enabled(pool: &SqlitePool) -> Result<bool, BantoError> {
    let enabled: i64 =
        sqlx::query_scalar("SELECT enabled_persisted FROM write_control_state WHERE id = 1")
            .fetch_one(pool)
            .await
            .map_err(banto_storage::storage_error)?;
    Ok(enabled != 0)
}

/// `write_control_state` の永続値を更新する (誰が・いつ変更したかも記録)。
/// 通常は [`WriteControl::set_enabled`] 経由で呼ぶ (状態ファイルと対で保存
/// するため)。
pub async fn persist_enabled(
    pool: &SqlitePool,
    enabled: bool,
    actor: Option<&str>,
) -> Result<(), BantoError> {
    sqlx::query(
        "UPDATE write_control_state \
         SET enabled_persisted = ?, last_changed_at = datetime('now'), last_changed_by = ? \
         WHERE id = 1",
    )
    .bind(enabled as i64)
    .bind(actor)
    .execute(pool)
    .await
    .map_err(banto_storage::storage_error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{init_db, migrate_memory};
    use std::sync::Arc;

    // --- 起動時の判定の総当たり (#433) ---------------------------------------

    /// DB: 有効/停止/読めない × ファイル: 無い/有効/停止/壊れている の 12 通り。
    /// 「どちらか一方でも停止（または読めない）なら停止」。ファイルが無い
    /// ときだけ DB に従う。食い違い・読めないときは注意書きが付く。
    #[test]
    fn startup_decision_table() {
        use DbStartupState as D;
        use FileStartupState as F;
        let db_unreadable = D::Unreadable("db down".to_string());
        let file_corrupt = F::Corrupt("broken".to_string());
        // (DB, ファイル, 有効で起動するか, 注意書きが付くか)
        let table: Vec<(D, F, bool, bool)> = vec![
            (D::Enabled, F::Absent, true, false),
            (D::Enabled, F::Enabled, true, false),
            (D::Enabled, F::Disabled, false, true),
            (D::Enabled, file_corrupt.clone(), false, true),
            (D::Disabled, F::Absent, false, false),
            (D::Disabled, F::Enabled, false, true),
            (D::Disabled, F::Disabled, false, false),
            (D::Disabled, file_corrupt.clone(), false, true),
            (db_unreadable.clone(), F::Absent, false, true),
            (db_unreadable.clone(), F::Enabled, false, true),
            (db_unreadable.clone(), F::Disabled, false, true),
            (db_unreadable.clone(), file_corrupt.clone(), false, true),
        ];
        for (db, file, enabled, warns) in table {
            let decision = decide_startup(&db, &file);
            assert_eq!(
                decision.enabled, enabled,
                "enabled for DB={db:?} file={file:?}"
            );
            assert_eq!(
                decision.warning.is_some(),
                warns,
                "warning for DB={db:?} file={file:?}: {:?}",
                decision.warning
            );
        }
    }

    #[test]
    fn startup_warning_names_both_problems() {
        let decision = decide_startup(
            &DbStartupState::Unreadable("db down".to_string()),
            &FileStartupState::Corrupt("broken".to_string()),
        );
        let warning = decision.warning.unwrap();
        assert!(warning.contains("db down"), "{warning}");
        assert!(warning.contains("broken"), "{warning}");
    }

    // --- 状態ファイル ---------------------------------------------------------

    #[tokio::test]
    async fn state_file_round_trips_and_reads_absent_and_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        let path = state_file_path(dir.path());
        assert_eq!(read_state_file(&path).await, FileStartupState::Absent);

        write_state_file(&path, false, Some("admin")).await.unwrap();
        assert_eq!(read_state_file(&path).await, FileStartupState::Disabled);
        write_state_file(&path, true, None).await.unwrap();
        assert_eq!(read_state_file(&path).await, FileStartupState::Enabled);

        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        assert!(
            !PathBuf::from(tmp).exists(),
            "no temporary file is left behind"
        );

        for broken in [
            &b""[..],
            b"{not json",
            br#"{"format":2,"writeEnabled":true}"#,
            br#"{"format":1}"#,
            br#"{"format":1,"writeEnabled":"yes"}"#,
        ] {
            std::fs::write(&path, broken).unwrap();
            assert!(
                matches!(read_state_file(&path).await, FileStartupState::Corrupt(_)),
                "{:?}",
                String::from_utf8_lossy(broken)
            );
        }
    }

    /// 途中で落ちて一時ファイルだけが残った状態は、状態ファイルの内容に
    /// 影響しない (読むのは本体だけ。次の書き込みで一時ファイルを上書きする)。
    #[tokio::test]
    async fn a_leftover_temporary_file_is_ignored_and_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = state_file_path(dir.path());
        write_state_file(&path, false, None).await.unwrap();
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        std::fs::write(&tmp, b"{\"format\":1,\"writeEnab").unwrap();

        assert_eq!(read_state_file(&path).await, FileStartupState::Disabled);
        write_state_file(&path, true, None).await.unwrap();
        assert_eq!(read_state_file(&path).await, FileStartupState::Enabled);
        assert!(!tmp.exists());
    }

    /// 状態ファイルの場所がディレクトリになっている (書けない・読めない)。
    fn unwritable_state_file(dir: &Path) -> PathBuf {
        let path = state_file_path(dir);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    // --- 停止・再開 -----------------------------------------------------------

    fn db_ok() -> std::future::Ready<Result<(), String>> {
        std::future::ready(Ok(()))
    }

    fn db_down() -> std::future::Ready<Result<(), String>> {
        std::future::ready(Err("database is down".to_string()))
    }

    #[test]
    fn constructs_with_live_flag_from_the_decision() {
        let dir = tempfile::tempdir().unwrap();
        let control = WriteControl::restore(
            StartupDecision {
                enabled: true,
                warning: None,
            },
            state_file_path(dir.path()),
        );
        assert!(control.is_enabled());
        assert!(control.was_enabled_before_restart());
        assert_eq!(control.persistence_warning(), None);

        let control = WriteControl::restore(
            StartupDecision {
                enabled: false,
                warning: Some("mismatch".to_string()),
            },
            state_file_path(dir.path()),
        );
        assert!(!control.is_enabled());
        assert!(!control.was_enabled_before_restart());
        assert_eq!(control.persistence_warning().as_deref(), Some("mismatch"));
    }

    #[test]
    fn enable_disable_round_trips() {
        let control = WriteControl::new(false);
        assert!(!control.is_enabled());
        control.enable();
        assert!(control.is_enabled());
        control.disable();
        assert!(!control.is_enabled());
    }

    #[tokio::test]
    async fn persisted_state_seeds_enabled_and_round_trips_through_persist() {
        let pool = migrate_memory().await.expect("migrate_memory");
        assert!(
            load_persisted_enabled(&pool).await.unwrap(),
            "write_control_state should seed enabled_persisted=1 (既定は書き込み可, #340)"
        );

        persist_enabled(&pool, false, Some("admin")).await.unwrap();
        assert!(!load_persisted_enabled(&pool).await.unwrap());

        let by: Option<String> =
            sqlx::query_scalar("SELECT last_changed_by FROM write_control_state WHERE id = 1")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(by.as_deref(), Some("admin"));
    }

    /// 通常の停止・再開の往復: 両方に保存され、再起動 (新しい判定と構築) を
    /// 跨いで保持される。注意書きは出ない。
    #[tokio::test]
    async fn normal_stop_and_resume_round_trip_across_restarts() {
        let dir = tempfile::tempdir().unwrap();
        let pool = init_db(dir.path().join("registry.sqlite3")).await.unwrap();
        let state_file = state_file_path(&dir.path().join("data"));

        let decision = load_startup_decision(&pool, &state_file).await;
        assert_eq!(
            decision,
            StartupDecision {
                enabled: true,
                warning: None
            },
            "fresh install (seed = enabled, no file) starts enabled"
        );
        let control = WriteControl::restore(decision, state_file.clone());

        let change = control.set_enabled(&pool, false, Some("admin")).await;
        assert!(change.succeeded() && change.fully_persisted(), "{change:?}");
        assert_eq!(change.warning(), None);
        assert!(!control.is_enabled());

        let decision = load_startup_decision(&pool, &state_file).await;
        assert_eq!(
            decision,
            StartupDecision {
                enabled: false,
                warning: None
            }
        );
        let control = WriteControl::restore(decision, state_file.clone());
        let change = control.set_enabled(&pool, true, Some("admin")).await;
        assert!(change.succeeded() && change.fully_persisted(), "{change:?}");
        assert!(control.is_enabled());

        let decision = load_startup_decision(&pool, &state_file).await;
        assert_eq!(
            decision,
            StartupDecision {
                enabled: true,
                warning: None
            }
        );
    }

    /// #433 の本題: 停止の保存が DB だけ失敗 → 再起動 → 停止のまま起動する
    /// (DB は「有効」のまま、状態ファイルが「停止」を覚えている)。
    #[tokio::test]
    async fn a_stop_that_only_reached_the_file_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let pool = init_db(dir.path().join("registry.sqlite3")).await.unwrap();
        let state_file = state_file_path(&dir.path().join("data"));
        let control = WriteControl::restore(
            load_startup_decision(&pool, &state_file).await,
            state_file.clone(),
        );
        assert!(control.is_enabled());

        let change = control
            .set_enabled_with(false, Some("admin"), db_down())
            .await;
        assert!(change.succeeded(), "a stop saved to the file is a stop");
        assert!(!change.fully_persisted());
        assert!(change.warning().unwrap().contains("database is down"));
        assert!(!control.is_enabled());
        assert!(control.persistence_warning().is_some());
        assert!(
            load_persisted_enabled(&pool).await.unwrap(),
            "precondition: the DB still says enabled"
        );

        // 再起動。
        let decision = load_startup_decision(&pool, &state_file).await;
        assert!(!decision.enabled, "the stop must survive the restart");
        assert!(decision.warning.is_some(), "the mismatch is reported");
        let control = WriteControl::restore(decision, state_file);
        assert!(!control.is_enabled());
        assert!(control.persistence_warning().is_some());
    }

    /// 停止の保存がファイルだけ失敗 → DB が停止を覚えているので停止のまま。
    #[tokio::test]
    async fn a_stop_that_only_reached_the_db_is_still_a_stop() {
        let dir = tempfile::tempdir().unwrap();
        let pool = init_db(dir.path().join("registry.sqlite3")).await.unwrap();
        let state_file = unwritable_state_file(dir.path());
        let control = WriteControl::restore(
            StartupDecision {
                enabled: true,
                warning: None,
            },
            state_file.clone(),
        );

        let change = control.set_enabled(&pool, false, Some("admin")).await;
        assert!(change.succeeded());
        assert!(change.file_error().is_some());
        assert!(!control.is_enabled());

        let decision = load_startup_decision(&pool, &state_file).await;
        assert!(!decision.enabled);
    }

    /// 両方に保存できなかった停止は失敗 (ライブフラグは止まったまま)。
    #[tokio::test]
    async fn a_stop_saved_nowhere_fails_but_writes_stay_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let control = WriteControl::restore(
            StartupDecision {
                enabled: true,
                warning: None,
            },
            unwritable_state_file(dir.path()),
        );
        let change = control.set_enabled_with(false, None, db_down()).await;
        assert!(!change.succeeded());
        assert!(!control.is_enabled());
        assert!(change
            .warning()
            .unwrap()
            .contains("どこにも保存できませんでした"));
    }

    /// 再開は、DB かファイルのどちらかに書けないと停止のまま。
    #[tokio::test]
    async fn resume_needs_both_saves() {
        // DB が失敗。
        let dir = tempfile::tempdir().unwrap();
        let state_file = state_file_path(dir.path());
        let control = WriteControl::restore(
            StartupDecision {
                enabled: false,
                warning: None,
            },
            state_file.clone(),
        );
        let change = control.set_enabled_with(true, None, db_down()).await;
        assert!(!change.succeeded());
        assert!(!control.is_enabled(), "DB failure keeps writes stopped");
        assert!(control.persistence_warning().is_some());

        // ファイルが失敗。
        let dir = tempfile::tempdir().unwrap();
        let control = WriteControl::restore(
            StartupDecision {
                enabled: false,
                warning: None,
            },
            unwritable_state_file(dir.path()),
        );
        let change = control.set_enabled_with(true, None, db_ok()).await;
        assert!(!change.succeeded());
        assert!(change.file_error().is_some());
        assert!(!control.is_enabled(), "file failure keeps writes stopped");

        // 両方成功で、注意書きも消える。
        let control = WriteControl::restore(
            StartupDecision {
                enabled: false,
                warning: Some("startup mismatch".to_string()),
            },
            state_file,
        );
        let change = control.set_enabled_with(true, None, db_ok()).await;
        assert!(change.succeeded());
        assert!(control.is_enabled());
        assert_eq!(control.persistence_warning(), None);
    }

    /// 再開の保存 (DB) が詰まっているあいだに停止が来たら: 停止はすぐに
    /// 効き、再開はライブフラグを立てず、最後に残る永続値も停止になる。
    /// (有効な状態から、再開が重ねて要求されて詰まっている場面で確かめる -
    /// ライブフラグが「すぐに落ちる」ことを観測できるように。)
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_stop_during_a_stuck_resume_wins() {
        let dir = tempfile::tempdir().unwrap();
        let state_file = state_file_path(dir.path());
        let control = Arc::new(WriteControl::restore(
            StartupDecision {
                enabled: true,
                warning: None,
            },
            state_file.clone(),
        ));

        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let resume = {
            let control = control.clone();
            let entered = entered.clone();
            let release = release.clone();
            tokio::spawn(async move {
                control
                    .set_enabled_with(true, None, async move {
                        entered.notify_one();
                        release.notified().await;
                        Ok(())
                    })
                    .await
            })
        };
        entered.notified().await;

        let stop = {
            let control = control.clone();
            tokio::spawn(async move { control.set_enabled_with(false, None, db_ok()).await })
        };
        // 停止は再開の保存を待たずにライブフラグを落とす。
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while control.is_enabled() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!stop.is_finished(), "the stop's save waits for the resume");

        release.notify_one();
        let resumed = resume.await.unwrap();
        assert!(resumed.interrupted_by_stop, "{resumed:?}");
        assert!(!resumed.succeeded());
        let stopped = stop.await.unwrap();
        assert!(stopped.succeeded());
        assert!(!control.is_enabled(), "the stop wins");
        assert_eq!(
            read_state_file(&state_file).await,
            FileStartupState::Disabled,
            "the last saved value is the stop"
        );
    }

    // --- 停止の DB 保存の制限時間 (#433 監査) ---------------------------------

    type Order = Arc<SyncMutex<Vec<&'static str>>>;

    /// DB への保存を `release` まで返さず、返るときに `label` を `order` に積む。
    async fn db_held_until(
        release: Arc<tokio::sync::Notify>,
        order: Order,
        label: &'static str,
        result: Result<(), String>,
    ) -> Result<(), String> {
        release.notified().await;
        order.lock().unwrap().push(label);
        result
    }

    async fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !done() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for: {what}"));
    }

    /// 固まる DB (返らない保存): 停止の応答は制限時間で返り、書き込みは
    /// 止まり、注意書きは「タイムアウト」と分かる形で、再起動で停止のまま
    /// 起動する。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_stop_with_a_hung_db_returns_within_the_limit_and_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let pool = init_db(dir.path().join("registry.sqlite3")).await.unwrap();
        let state_file = state_file_path(&dir.path().join("data"));
        let control = WriteControl::restore(
            load_startup_decision(&pool, &state_file).await,
            state_file.clone(),
        );
        assert!(control.is_enabled());

        let started = std::time::Instant::now();
        let change = tokio::time::timeout(
            STOP_DB_SAVE_TIMEOUT * 3,
            control.set_enabled_with(
                false,
                Some("admin"),
                std::future::pending::<Result<(), String>>(),
            ),
        )
        .await
        .expect("the stop must return within its DB save limit");
        let elapsed = started.elapsed();
        assert!(
            elapsed >= STOP_DB_SAVE_TIMEOUT && elapsed < STOP_DB_SAVE_TIMEOUT * 2,
            "{elapsed:?}"
        );
        assert!(change.db_timed_out, "{change:?}");
        assert!(change.succeeded(), "saved to the file, so the stop holds");
        assert!(!control.is_enabled());
        assert!(change.warning().unwrap().contains("タイムアウト"));
        assert!(control
            .persistence_warning()
            .unwrap()
            .contains("タイムアウト"));

        // 再起動 (DB はまだ「有効」、状態ファイルが「停止」)。
        let decision = load_startup_decision(&pool, &state_file).await;
        assert!(!decision.enabled, "the stop survives the restart");
        assert!(decision.warning.is_some());
    }

    /// 打ち切った停止の DB 書き込みが遅れて完了する場合: 次の再開はその完了を
    /// 待ってから保存する (「有効」を遅れた「停止」で上書きされない)。遅れて
    /// 完了したら注意書きも更新される。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_next_resume_waits_for_a_lagging_stop_db_write() {
        let dir = tempfile::tempdir().unwrap();
        let control = Arc::new(WriteControl::restore(
            StartupDecision {
                enabled: true,
                warning: None,
            },
            state_file_path(dir.path()),
        ));
        let order: Order = Arc::default();
        let release = Arc::new(tokio::sync::Notify::new());

        let stopped = control
            .set_enabled_with(
                false,
                None,
                db_held_until(release.clone(), order.clone(), "stop", Ok(())),
            )
            .await;
        assert!(stopped.db_timed_out && stopped.succeeded(), "{stopped:?}");

        let resume = {
            let control = control.clone();
            let order = order.clone();
            tokio::spawn(async move {
                control
                    .set_enabled_with(true, None, async move {
                        order.lock().unwrap().push("resume");
                        Ok(())
                    })
                    .await
            })
        };
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            order.lock().unwrap().is_empty(),
            "the resume must not save before the lagging stop finishes"
        );
        assert!(!resume.is_finished());
        assert!(!control.is_enabled());

        release.notify_one();
        let resumed = resume.await.unwrap();
        assert!(resumed.succeeded(), "{resumed:?}");
        assert_eq!(*order.lock().unwrap(), vec!["stop", "resume"]);
        assert!(control.is_enabled());
        assert_eq!(control.persistence_warning(), None);
    }

    /// 遅れて完了した停止の DB 書き込みは、注意書きを自分の結果で更新する
    /// (成功なら消える) が、その後に記録された新しい停止・再開の表示は巻き
    /// 戻さない。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_late_stop_db_result_updates_only_its_own_warning() {
        // 遅れて成功 → 注意書きが消える。
        let dir = tempfile::tempdir().unwrap();
        let control = WriteControl::restore(
            StartupDecision {
                enabled: true,
                warning: None,
            },
            state_file_path(dir.path()),
        );
        let order: Order = Arc::default();
        let release = Arc::new(tokio::sync::Notify::new());
        let change = control
            .set_enabled_with(
                false,
                None,
                db_held_until(release.clone(), order.clone(), "stop", Ok(())),
            )
            .await;
        assert!(change.db_timed_out);
        assert!(control.persistence_warning().is_some());
        release.notify_one();
        wait_until("the late success clears the warning", || {
            control.persistence_warning().is_none()
        })
        .await;

        // 遅れて失敗するが、その前に新しい停止が両方に保存された → 新しい
        // 表示 (注意なし) のまま。
        let dir = tempfile::tempdir().unwrap();
        let control = WriteControl::restore(
            StartupDecision {
                enabled: true,
                warning: None,
            },
            state_file_path(dir.path()),
        );
        let release = Arc::new(tokio::sync::Notify::new());
        let first = control
            .set_enabled_with(
                false,
                None,
                db_held_until(
                    release.clone(),
                    order.clone(),
                    "late failure",
                    Err("late failure".to_string()),
                ),
            )
            .await;
        assert!(first.db_timed_out);
        let second = control.set_enabled_with(false, None, db_ok()).await;
        assert!(second.fully_persisted());
        assert_eq!(control.persistence_warning(), None);
        release.notify_one();
        wait_until("the late failure has been observed", || {
            order.lock().unwrap().contains(&"late failure")
        })
        .await;
        let lagging: Vec<JoinHandle<()>> =
            std::mem::take(&mut *control.lagging_stop_db.lock().unwrap());
        for handle in lagging {
            handle.await.unwrap();
        }
        assert_eq!(
            control.persistence_warning(),
            None,
            "an older late result must not overwrite a newer outcome"
        );
    }
}
