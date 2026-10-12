//! 起動に失敗したときの「エラーの窓と対処の選択」（#392、2026-10-12 オーナー決定）。
//!
//! 起動を止める失敗で **panic で落とさず**、エラーの窓を出して利用者に対処を
//! 選ばせる。**どの選択肢でも今の DB や記録を消さない**（名前を変えて残す・
//! 書き換えるのは設定の 1 項目だけ）。
//!
//! | 起きたこと（[`FailureKind`]） | 窓（[`WindowPlan`]） |
//! | --- | --- |
//! | 既に起動中（A1）・旧形式の DB | 今までのエラーの窓のまま（[`WindowPlan::ExistingNotice`]） |
//! | 保存先（`data.dir`）を読めない | ① 保存先のフォルダを選び直す ② 既定の場所に戻して起動する ③ 終了 |
//! | 保存先に書き込めない（USB の取り外し・権限・ドライブが無い） | もう一度確かめる・① ・② ・③ |
//! | DB が壊れている・開けない | ① バックアップから戻す ② 新しく作り直す ③ 終了 |
//! | 新しい版の ChronoGazer が使った DB・それ以外 | ③ 終了だけ（理由を出す） |
//!
//! ① ② のうち、作り直す・戻す・既定に戻すは確認を 1 回はさむ
//! （[`RecoveryChoice::needs_confirmation`]。確認は窓の側で行う）。
//!
//! このモジュールは **Tauri に依存しない**: 判断（失敗 → 窓と選択肢）は純関数
//! （[`classify`]・[`plan`]）、対処はファイルと設定 DB だけを触る関数
//! （[`set_aside_broken_db`]・[`restore_from_backup`]・[`apply_store_dir`]）。
//! 後の B2 でサービスに接続するシェルも同じものを使う（窓を出すのは各シェル）。
//!
//! # 分類の根拠（[`classify`]）
//!
//! - `EngineStartError::Database(InitDbError::Storage)`・`Hub`（`installation_id`
//!   の読み書き = 設定 DB）・保存設定の読み取りの失敗は、どれも「設定 DB を
//!   読めない」。SQLite の文言で分ける（[`classify_storage_message`]）:
//!   `file is not a database`・`malformed`・`corrupt` → 壊れている、
//!   `was previously applied but is missing`（sqlx の `VersionMissing` = この版が
//!   知らない migration が当たっている）→ 新しい版の DB、それ以外（権限・
//!   他のソフトが使用中・ディスク）→ 開けない。
//! - **新しい版の DB は「終了」だけ**にした: 作り直し・戻しは、壊れていない
//!   （新しい版でなら読める）DB を脇へ退けることになるため。その版で開くよう
//!   案内する。
//! - 保存設定の読み取りの失敗は、読めないのが保存先（`data.dir`）そのもの
//!   （`EngineStartError::StoreConfig::data_dir_unreadable`）で DB 自体は壊れて
//!   いないときだけ「保存先」の窓。DB が壊れている文言なら DB の窓、保持期間
//!   など別の項目が読めないなら「開けない」の DB の窓（設定 DB の中身が読めない）。
//! - 保存先に書き込めない（`EngineStartError::StoreDirUnwritable`、デスクトップは
//!   起動時に解決済みの `data.dir` へ試し書きする - `EngineConfig::store_dir_check_required`）
//!   は「書き込めない」の保存先の窓。値を読めない窓の選択肢に**「もう一度確かめる」**
//!   を足す（USB を差し直した・権限を直した後に、何も変えずに起動をやり直す）。
//! - DB が開けない理由が権限・他のソフトのロックでも「作り直す」は出す（オーナー決定
//!   2026-10-12: ロック中なら名前の変更が失敗して理由が出るだけで、何も消さない）。
//!
//! # 対処（どれも消さない）
//!
//! - **作り直す**（[`set_aside_broken_db`]）: `chronogazer.sqlite3` を
//!   `chronogazer.sqlite3.broken-<日時>` へ名前を変えて残す。`-wal`・`-shm`・
//!   `-journal` があれば**先に**同じ名前＋接尾辞へ移す（本体だけ移して古い
//!   `-wal` が元の名前に残ると、作り直した空の DB に古い WAL が当たりうる）。
//!   途中で失敗したら移したものを元に戻す。日時はローカル時刻の
//!   `YYYYMMDD-HHMMSS`（[`broken_stamp`]）、同じ名前があれば `-1`、`-2`…。
//!   Windows では pool を閉じた直後もハンドルが少し残ることがある
//!   （`crate::test_support` のモジュール doc）ので、名前の変更は短く再試行する。
//! - **バックアップから戻す**（[`restore_from_backup`]）: 今のリストアと
//!   **同じ仕組み・同じ安全策**（`BackupService::stage_restore_from_file` で
//!   整合性検査と必須テーブルを確かめて予約 → `apply_pending_restore_at_startup`
//!   がもう一度検査して適用、`restore_applied` の監査）。適用は次の `open` に任せず
//!   **対処の中で行い、適用されたことを確かめる**（#594 のレビュー P1: 通常の起動は
//!   適用の失敗を記録して続けるので、退けた後の `init_db` が空の DB を作ってしまう）。
//!   適用されなければ予約を取り消し、退けた DB を元の名前に戻して理由を返す。
//!   予約の後に、壊れた DB を作り直しと同じ
//!   形で `.broken-<日時>` へ退ける: リストアの適用は今の DB の本体だけを
//!   `pre-restore-*` に写して `-wal`・`-shm` を消すので、壊れた DB の WAL に
//!   残った記録を失いうるため（「消さない」）。退けられなければ予約を取り消す。
//!   バックアップの一覧と予約は DB のパスだけで決まる（`backups/<DB 名>/`）が、
//!   `BackupService` は SQLite のハンドルを要求する（SQLite かどうかの確認だけに
//!   使う）ので、壊れた DB の代わりにメモリ上の空の pool を渡す（[`list_backups`]）。
//! - **保存先を選び直す・既定に戻す**（[`apply_store_dir`]）: 設定 DB の
//!   `data.dir` だけを書き換える（読めない値を上書きする）。書く前にその
//!   フォルダに書き込めることを確かめる（作れない・書けないなら理由を返し、
//!   設定は変えない）。既定に戻すと、それまでの記録は元の場所に残るが画面からは
//!   見えなくなる（窓の確認の文言で先に伝える）。
//!
//! # やり直しの方式
//!
//! 対処の後は **プロセスを再起動せず、同じプロセスで `EngineRuntime::open` /
//! `start` をやり直す**（デスクトップの `src-tauri` の `startup` モジュール）。
//! 失敗して戻った `open`/`start` は A1 の排他と pool を返している
//! （`init_db` は失敗の経路でも pool を閉じてから返す、`start` の失敗の経路も
//! 同じ。`OpenedEngine::close` は pool を閉じるのを待つ）。再起動
//! （`AppHandle::restart`）にしなかった理由は、古いプロセスが終わりきる前に
//! 新しいプロセスが単一インスタンスのプラグイン・A1 の排他にぶつかりうること
//! と、選んだ対処を新しいプロセスへ渡す経路が要ること。
//!
//! # A1 の排他（#594 のオーナーレビュー P1）
//!
//! 作り直す・戻すは、ファイルを変える**前に** [`lock_for_recovery`] でエンジンと同じ
//! 排他を取り（取れなければ何も変えずに理由を返す）、[`RecoveryLock`] として
//! `EngineRuntime::open_with_recovery_lock` へ途切れずに引き継ぐ。窓を出している間
//! ずっとは持たない（その間は何も変えていないので不要で、持つと同じ DB を使う
//! `banto-serve` などを利用者が確かめて止める対処の邪魔になる）。
//!
//! # 監査
//!
//! 選ばれた対処は、DB を開けた後の `OpenedEngine::start` が `startup_recovery`
//! として記録する（[`RecoveryNote`]。DB を開けない間は書けない）。
//! バックアップから戻したときは、加えて今までどおり `restore_applied` が残る。

use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::AsyncWriteExt;

use banto_core::BantoError;
use serde::Serialize;

use crate::audit::AuditEntry;
use crate::backup::{AppliedRestoreInfo, BackupInfo, BackupService};
use crate::collect::resolve_data_dir;
use crate::db::{Db, InitDbError};
use crate::engine::EngineStartError;
use crate::instance_lock::{acquire_for_db, ChronoGazerHost, InstanceGuard};
use crate::settings::{default_data_dir, set_data_dir, SettingsService};

/// 起動の失敗の分類（[`classify`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum FailureKind {
    /// 同じ DB を別のプロセスが使用中など（A1）。
    AlreadyRunning,
    /// 旧形式の DB（2026-10-02 のスキーマ整理より前）。
    LegacyDatabase,
    /// 保存先（`data.dir`）の値を読めない。
    StoreDirUnreadable,
    /// 保存先（解決済みの `data.dir`）に書き込めない（2026-10-12 オーナー決定で追加）。
    StoreDirUnwritable,
    /// DB が壊れている（SQLite が「DB ではない」「壊れている」と言う）。
    DatabaseCorrupt,
    /// DB を開けない・設定を読めない（権限・他のソフトが使用中・ディスクなど）。
    DatabaseUnopenable,
    /// この版が知らない migration が当たっている（新しい版の ChronoGazer が使った DB）。
    DatabaseFromNewerVersion,
    /// それ以外（終了だけ）。
    Other,
}

/// 窓で選べる対処。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RecoveryChoice {
    /// もう一度確かめる（USB を差し直した・権限を直した後に、そのまま起動をやり直す）。
    RetryStoreCheck,
    /// 保存先のフォルダを選び直す（フォルダ選択ダイアログ）。
    ChooseStoreDir,
    /// 既定の場所に戻して起動する。
    ResetStoreDir,
    /// バックアップから戻す（一覧から選ぶ）。
    RestoreBackup,
    /// 新しく作り直す（今の DB は `.broken-<日時>` へ名前を変えて残す）。
    RecreateDatabase,
    /// 終了する。
    Quit,
}

impl RecoveryChoice {
    /// 窓から Rust へ選択を渡すときの名前（監査の `detail.choice` にも使う）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RetryStoreCheck => "retryStoreCheck",
            Self::ChooseStoreDir => "chooseStoreDir",
            Self::ResetStoreDir => "resetStoreDir",
            Self::RestoreBackup => "restoreBackup",
            Self::RecreateDatabase => "recreateDatabase",
            Self::Quit => "quit",
        }
    }

    /// [`RecoveryChoice::as_str`] の逆。知らない名前は `None`。
    pub fn parse(name: &str) -> Option<Self> {
        [
            Self::RetryStoreCheck,
            Self::ChooseStoreDir,
            Self::ResetStoreDir,
            Self::RestoreBackup,
            Self::RecreateDatabase,
            Self::Quit,
        ]
        .into_iter()
        .find(|choice| choice.as_str() == name)
    }

    /// 実行の前に確認を 1 回はさむか（オーナー決定: 作り直す・戻す・既定に戻す）。
    /// フォルダを選び直すのはダイアログで選ぶこと自体が確認、終了は確認しない。
    pub fn needs_confirmation(self) -> bool {
        matches!(
            self,
            Self::ResetStoreDir | Self::RestoreBackup | Self::RecreateDatabase
        )
    }
}

/// 失敗に対して出す窓。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowPlan {
    /// 今までのエラーの窓（既に起動中・旧形式の DB。閉じると終了）。
    ExistingNotice,
    /// 対処を選ぶ窓（並びは窓に出す順。`Quit` は必ず最後に入る）。
    Choices(Vec<RecoveryChoice>),
}

/// 失敗の分類 → 窓と選択肢（モジュール doc の表）。
pub fn plan(kind: FailureKind) -> WindowPlan {
    use RecoveryChoice::*;
    match kind {
        FailureKind::AlreadyRunning | FailureKind::LegacyDatabase => WindowPlan::ExistingNotice,
        FailureKind::StoreDirUnreadable => {
            WindowPlan::Choices(vec![ChooseStoreDir, ResetStoreDir, Quit])
        }
        FailureKind::StoreDirUnwritable => {
            WindowPlan::Choices(vec![RetryStoreCheck, ChooseStoreDir, ResetStoreDir, Quit])
        }
        FailureKind::DatabaseCorrupt | FailureKind::DatabaseUnopenable => {
            WindowPlan::Choices(vec![RestoreBackup, RecreateDatabase, Quit])
        }
        FailureKind::DatabaseFromNewerVersion | FailureKind::Other => {
            WindowPlan::Choices(vec![Quit])
        }
    }
}

/// 起動の失敗の分類（モジュール doc「分類の根拠」）。
pub fn classify(err: &EngineStartError) -> FailureKind {
    match err {
        EngineStartError::InstanceLock(_) => FailureKind::AlreadyRunning,
        EngineStartError::Database(InitDbError::Legacy(_)) => FailureKind::LegacyDatabase,
        EngineStartError::Database(InitDbError::Storage(err)) => {
            classify_storage_message(&err.to_string())
        }
        EngineStartError::Hub(err) => classify_settings_error(err),
        EngineStartError::StoreConfig {
            source,
            data_dir_unreadable,
        } => match classify_settings_error(source) {
            FailureKind::DatabaseCorrupt => FailureKind::DatabaseCorrupt,
            _ if *data_dir_unreadable => FailureKind::StoreDirUnreadable,
            other => other,
        },
        EngineStartError::StoreDirUnwritable { .. } => FailureKind::StoreDirUnwritable,
        EngineStartError::Listen { .. } => FailureKind::Other,
    }
}

/// 設定 DB の読み書きの失敗（Hub の初期化・起動時の設定の読み取り）の分類。
/// 壊れている文言なら [`FailureKind::DatabaseCorrupt`]、それ以外は
/// [`FailureKind::DatabaseUnopenable`]（DB はもう開けているので「新しい版」は
/// ここでは出ない）。
pub fn classify_settings_error(err: &BantoError) -> FailureKind {
    match classify_storage_message(&err.to_string()) {
        FailureKind::DatabaseCorrupt => FailureKind::DatabaseCorrupt,
        _ => FailureKind::DatabaseUnopenable,
    }
}

/// `init_db` の接続・migration の失敗の文言の分類（モジュール doc）。
pub fn classify_storage_message(message: &str) -> FailureKind {
    let lower = message.to_ascii_lowercase();
    if lower.contains("file is not a database")
        || lower.contains("malformed")
        || lower.contains("corrupt")
    {
        FailureKind::DatabaseCorrupt
    } else if lower.contains("was previously applied but is missing") {
        FailureKind::DatabaseFromNewerVersion
    } else {
        FailureKind::DatabaseUnopenable
    }
}

/// 対処に失敗した理由（利用者向けの日本語。窓にそのまま出す）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryError(pub String);

impl std::fmt::Display for RecoveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for RecoveryError {}

/// 選ばれた対処の監査の記録（`OpenedEngine::with_recovery_note`）。
#[derive(Debug, Clone, PartialEq)]
pub struct RecoveryNote {
    pub choice: RecoveryChoice,
    /// 監査の `detail`（`choice` は自動で入る）。
    pub detail: serde_json::Map<String, serde_json::Value>,
}

impl RecoveryNote {
    pub fn new(choice: RecoveryChoice) -> Self {
        Self {
            choice,
            detail: serde_json::Map::new(),
        }
    }

    pub fn with(mut self, key: &str, value: impl Into<serde_json::Value>) -> Self {
        self.detail.insert(key.to_string(), value.into());
        self
    }

    /// 監査の 1 行（`startup_recovery`、操作した人は無し = 起動時の窓）。
    pub fn audit_entry(&self, origin: &'static str) -> AuditEntry<'_> {
        let mut detail = self.detail.clone();
        detail.insert("choice".to_string(), self.choice.as_str().into());
        AuditEntry {
            actor_username: None,
            actor_role: None,
            action: "startup_recovery",
            resource: "startup",
            entity_id: None,
            detail: Some(serde_json::Value::Object(detail)),
            origin,
            result: "ok",
        }
    }
}

/// `.broken-<日時>` の日時（ローカル時刻の `YYYYMMDD-HHMMSS`）。`epoch_ms` は UTC の
/// エポックミリ秒、`utc_offset_ms` はローカル時刻のずれ（JST = +9 時間）。
pub fn broken_stamp(epoch_ms: i64, utc_offset_ms: i64) -> String {
    const DAY_MS: i64 = 86_400_000;
    let date = banto_tstore::LocalDate::from_epoch_ms(epoch_ms, utc_offset_ms);
    let secs_of_day = (epoch_ms + utc_offset_ms).rem_euclid(DAY_MS) / 1000;
    format!(
        "{}-{:02}{:02}{:02}",
        date.to_yyyymmdd(),
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60
    )
}

/// 今の時刻の [`broken_stamp`]。
pub fn broken_stamp_now() -> String {
    use banto_tstore::{Clock, SystemClock};
    let clock = SystemClock;
    broken_stamp(clock.now_ms(), clock.utc_offset_ms())
}

/// DB と一緒に動かす付随ファイルの接尾辞（SQLite の命名）。
const SIDECAR_SUFFIXES: [&str; 3] = ["-wal", "-shm", "-journal"];

/// [`set_aside_broken_db`] の結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetAside {
    /// 名前を変えた DB 本体（元の DB が無ければ `None`）。
    pub broken_db: Option<PathBuf>,
    /// 名前を変えたファイル（元 → 先、移した順）。
    pub moved: Vec<(PathBuf, PathBuf)>,
}

impl SetAside {
    /// 名前を変えた DB 本体のファイル名（監査・窓の文言用）。
    pub fn broken_file_name(&self) -> Option<String> {
        self.broken_db
            .as_ref()
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
    }
}

/// 名前の変更の再試行（Windows で pool を閉じた直後に残るハンドル向け）。
#[derive(Debug, Clone, Copy)]
struct RetryPolicy {
    attempts: u32,
    interval: Duration,
}

const RENAME_RETRY: RetryPolicy = RetryPolicy {
    attempts: 30,
    interval: Duration::from_millis(100),
};

/// 壊れた DB を `<DB 名>.broken-<stamp>` へ名前を変えて残す（モジュール doc
/// 「対処」）。DB が無ければ何もせずに `Ok`（`broken_db: None`）。
pub(crate) async fn set_aside_broken_db(
    db_path: &Path,
    stamp: &str,
) -> Result<SetAside, RecoveryError> {
    set_aside_with(db_path, stamp, RENAME_RETRY).await
}

async fn set_aside_with(
    db_path: &Path,
    stamp: &str,
    retry: RetryPolicy,
) -> Result<SetAside, RecoveryError> {
    let file_name = db_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .ok_or_else(|| RecoveryError(format!("DB のパスが不正です: {}", db_path.display())))?;
    let dir = db_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let sidecar = |base: &Path, suffix: &str| PathBuf::from(format!("{}{suffix}", base.display()));

    // 移すもの: 付随ファイルが先、本体が最後（モジュール doc）。
    let mut sources: Vec<(PathBuf, &str)> = SIDECAR_SUFFIXES
        .iter()
        .map(|suffix| (sidecar(db_path, suffix), *suffix))
        .filter(|(path, _)| path.exists())
        .collect();
    let main_exists = std::fs::symlink_metadata(db_path).is_ok();
    if main_exists {
        sources.push((db_path.to_path_buf(), ""));
    }
    if sources.is_empty() {
        return Ok(SetAside {
            broken_db: None,
            moved: Vec::new(),
        });
    }

    // 移し先: 本体・付随ファイルのどれも既に無い名前（`-1`、`-2`…）。
    let mut n = 0u32;
    let base = loop {
        let candidate = if n == 0 {
            dir.join(format!("{file_name}.broken-{stamp}"))
        } else {
            dir.join(format!("{file_name}.broken-{stamp}-{n}"))
        };
        let taken = std::fs::symlink_metadata(&candidate).is_ok()
            || SIDECAR_SUFFIXES
                .iter()
                .any(|suffix| std::fs::symlink_metadata(sidecar(&candidate, suffix)).is_ok());
        if !taken {
            break candidate;
        }
        n += 1;
    };

    let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new();
    for (from, suffix) in sources {
        let to = sidecar(&base, suffix);
        if let Err(err) = rename_with_retry(&from, &to, retry).await {
            // 移したものを元に戻す（逆順）。戻せなかったものは理由に書く。
            let mut not_restored = Vec::new();
            for (orig, moved_to) in moved.iter().rev() {
                if rename_with_retry(moved_to, orig, retry).await.is_err() {
                    not_restored.push(format!("{} → {}", moved_to.display(), orig.display()));
                }
            }
            let mut message = format!(
                "DB のファイルの名前を変えられませんでした（{}）: {err}。他のソフト（ウイルス対策・バックアップ・エクスプローラーのプレビューなど）が使用していないか、フォルダの権限を確認してください。",
                from.display()
            );
            if !not_restored.is_empty() {
                message.push_str(&format!(
                    " 次のファイルは元の名前に戻せませんでした（消してはいません）: {}",
                    not_restored.join(", ")
                ));
            }
            return Err(RecoveryError(message));
        }
        moved.push((from, to));
    }

    Ok(SetAside {
        broken_db: main_exists.then_some(base),
        moved,
    })
}

/// 名前の変更。Windows の共有違反・アクセス拒否（閉じた直後のハンドル）は
/// 短く再試行する。移し先が既にあれば失敗にする（上書きしない）。
async fn rename_with_retry(from: &Path, to: &Path, retry: RetryPolicy) -> std::io::Result<()> {
    let mut attempt = 0;
    loop {
        if std::fs::symlink_metadata(to).is_ok() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("移し先が既にあります: {}", to.display()),
            ));
        }
        match tokio::fs::rename(from, to).await {
            Ok(()) => return Ok(()),
            Err(err) if attempt + 1 < retry.attempts && is_transient(&err) => {
                attempt += 1;
                tokio::time::sleep(retry.interval).await;
            }
            Err(err) => return Err(err),
        }
    }
}

/// 待てば通りうる失敗か（Windows の ERROR_ACCESS_DENIED = 5・
/// ERROR_SHARING_VIOLATION = 32・ERROR_LOCK_VIOLATION = 33）。
fn is_transient(err: &std::io::Error) -> bool {
    err.kind() == std::io::ErrorKind::PermissionDenied
        || matches!(err.raw_os_error(), Some(5 | 32 | 33))
}

/// `db_path` のバックアップの一覧（新しい順。`BackupService::list` と同じ）。
/// 一覧はファイルだけで決まるので、壊れた DB を開かずに読める（モジュール doc）。
pub async fn list_backups(db_path: &Path) -> Result<Vec<BackupInfo>, BantoError> {
    let pool = banto_storage::connect_sqlite_memory().await?;
    let result = BackupService::new(db_path.to_path_buf(), Db::Sqlite(pool.clone()))
        .list()
        .await;
    pool.close().await;
    result
}

/// 復旧の操作（DB・付随ファイル・リストアの予約を変える）の間ずっと持つ A1 の排他
/// （#594 のオーナーレビュー P1）。[`lock_for_recovery`] で**変更の前に**取り、
/// `EngineRuntime::open_with_recovery_lock` へ途切れずに引き継ぐ（その後は
/// エンジンが持つ）。
pub struct RecoveryLock {
    pub(crate) guard: InstanceGuard,
    pub(crate) db_path: PathBuf,
    /// 復旧の中で適用したリストア（`restore_applied` の監査に使う）。
    pub(crate) applied_restore: Option<AppliedRestoreInfo>,
}

impl std::fmt::Debug for RecoveryLock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecoveryLock")
            .field("db_path", &self.db_path)
            .field("applied_restore", &self.applied_restore)
            .finish()
    }
}

/// 復旧の操作の前に、エンジンと同じ A1 の排他を取る。取れなければ（`banto-serve`
/// や別の ChronoGazer が同じ DB を使用中）何も変えずに理由を返す。
pub fn lock_for_recovery(db_path: &Path) -> Result<RecoveryLock, RecoveryError> {
    let guard = acquire_for_db(db_path, ChronoGazerHost::Desktop).map_err(|failure| {
        RecoveryError(format!(
            "{failure}（対処は行っていません。ファイルは何も変えていません。）"
        ))
    })?;
    Ok(RecoveryLock {
        guard,
        db_path: db_path.to_path_buf(),
        applied_restore: None,
    })
}

/// 排他を持ったまま済ませた対処（[`recreate_database`]・[`restore_from_backup`]）。
#[derive(Debug)]
pub struct Prepared {
    /// `EngineRuntime::open_with_recovery_lock` へ渡す排他。
    pub lock: RecoveryLock,
    pub set_aside: SetAside,
}

/// 新しく作り直す: 排他を取ってから（取れなければ何も変えない）壊れた DB を退ける。
pub async fn recreate_database(db_path: &Path, stamp: &str) -> Result<Prepared, RecoveryError> {
    let lock = lock_for_recovery(db_path)?;
    let set_aside = set_aside_broken_db(db_path, stamp).await?;
    Ok(Prepared { lock, set_aside })
}

/// 退けたファイルを元の名前に戻す（逆順）。
async fn undo_set_aside(set_aside: &SetAside) -> Result<(), RecoveryError> {
    let mut failed = Vec::new();
    for (orig, moved_to) in set_aside.moved.iter().rev() {
        if let Err(err) = rename_with_retry(moved_to, orig, RENAME_RETRY).await {
            failed.push(format!(
                "{} → {}: {err}",
                moved_to.display(),
                orig.display()
            ));
        }
    }
    if failed.is_empty() {
        Ok(())
    } else {
        Err(RecoveryError(format!(
            "退けたファイルを元の名前に戻せませんでした（消してはいません）: {}",
            failed.join(", ")
        )))
    }
}

/// バックアップから戻す（モジュール doc「対処」）: 排他を取る → 予約（検査つき）→
/// 壊れた DB を退ける → **ここで適用し、実際に適用されたことを確かめる**
/// （#594 のレビュー P1: 適用に失敗したまま `init_db` が空の DB を作り、戻せたかの
/// ように続けない）。適用されなければ予約を取り消し、退けた DB を元の名前に戻して
/// 理由を返す（何も消さない）。適用は今のリストアと同じ
/// `BackupService::apply_pending_restore_at_startup`。
pub async fn restore_from_backup(
    db_path: &Path,
    backup_file_name: &str,
    stamp: &str,
) -> Result<Prepared, RecoveryError> {
    restore_with(db_path, backup_file_name, stamp, |path| async move {
        BackupService::apply_pending_restore_at_startup(&path).await
    })
    .await
}

async fn restore_with<F, Fut>(
    db_path: &Path,
    backup_file_name: &str,
    stamp: &str,
    apply: F,
) -> Result<Prepared, RecoveryError>
where
    F: FnOnce(PathBuf) -> Fut,
    Fut: std::future::Future<Output = Result<Option<AppliedRestoreInfo>, BantoError>>,
{
    let mut lock = lock_for_recovery(db_path)?;
    let pool = banto_storage::connect_sqlite_memory()
        .await
        .map_err(|err| RecoveryError(format!("リストアの準備に失敗しました: {err}")))?;
    let backup = BackupService::new(db_path.to_path_buf(), Db::Sqlite(pool.clone()));
    let result = async {
        backup
            .stage_restore_from_file(backup_file_name)
            .await
            .map_err(|err| {
                RecoveryError(format!(
                    "選んだバックアップ（{backup_file_name}）から戻せません: {}",
                    describe(&err)
                ))
            })?;
        let set_aside = match set_aside_broken_db(db_path, stamp).await {
            Ok(set_aside) => set_aside,
            Err(err) => {
                if let Err(cancel) = backup.cancel_pending_restore().await {
                    return Err(RecoveryError(format!(
                        "{err} また、リストアの予約を取り消せませんでした（次の起動で適用されます）: {cancel}"
                    )));
                }
                return Err(err);
            }
        };
        let reason = match apply(db_path.to_path_buf()).await {
            Ok(Some(applied)) => return Ok((set_aside, applied)),
            Ok(None) => "予約したバックアップが検査に通らず、適用されませんでした".to_string(),
            Err(err) => describe(&err),
        };
        // 適用されなかった: 予約を取り消し、退けた DB を元に戻す（空の DB で続けない）。
        let mut message =
            format!("バックアップ（{backup_file_name}）を適用できませんでした: {reason}。");
        if let Err(cancel) = backup.cancel_pending_restore().await {
            message.push_str(&format!(
                " リストアの予約を取り消せませんでした（次の起動で適用されることがあります）: {cancel}。"
            ));
        }
        if std::fs::symlink_metadata(db_path).is_ok() {
            // 適用の途中で何かが元の名前に置かれた: 上書きしないよう戻さない。
            message.push_str(&format!(
                " 元のデータベースは {} に残しています（消していません）。",
                set_aside
                    .broken_file_name()
                    .unwrap_or_else(|| "退けた名前".to_string())
            ));
        } else {
            match undo_set_aside(&set_aside).await {
                Ok(()) => message
                    .push_str(" 元のデータベースは元の名前に戻しました（何も変えていません）。"),
                Err(err) => message.push_str(&format!(" {err}")),
            }
        }
        Err(RecoveryError(message))
    }
    .await;
    pool.close().await;
    let (set_aside, applied) = result?;
    lock.applied_restore = Some(applied);
    Ok(Prepared { lock, set_aside })
}

/// 保存先の選び直し（[`apply_store_dir`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreDirChoice {
    /// 選んだフォルダ（絶対パス）。
    Folder(PathBuf),
    /// 既定の場所（`./data`、アプリのデータディレクトリ基準）。
    Default,
}

/// 保存先（`data.dir`）を書き換える（モジュール doc「対処」）。書く前にフォルダに
/// 書き込めることを確かめ、だめなら設定を変えずに理由を返す。`data_base` は
/// 相対パスの基準（`EngineConfig::data_base`）。
pub async fn apply_store_dir(
    settings: &SettingsService,
    data_base: &Path,
    choice: &StoreDirChoice,
) -> Result<RecoveryNote, RecoveryError> {
    let (value, resolved, recovery_choice) = match choice {
        StoreDirChoice::Folder(path) => {
            if !path.is_absolute() {
                return Err(RecoveryError(format!(
                    "保存先には絶対パスのフォルダを選んでください: {}",
                    path.display()
                )));
            }
            (
                path.display().to_string(),
                path.clone(),
                RecoveryChoice::ChooseStoreDir,
            )
        }
        StoreDirChoice::Default => (
            default_data_dir().to_string(),
            without_cur_dir(&resolve_data_dir(data_base, default_data_dir())),
            RecoveryChoice::ResetStoreDir,
        ),
    };
    check_writable_dir(&resolved).await?;
    set_data_dir(settings, &value).await.map_err(|err| {
        RecoveryError(format!(
            "保存先の設定を書き込めませんでした（設定 DB に書き込めません）: {err}"
        ))
    })?;
    Ok(RecoveryNote::new(recovery_choice)
        .with("dataDir", value)
        .with("resolvedDataDir", resolved.display().to_string()))
}

/// パスから `.`（`./data` の `./`）を除く（窓・監査に出す形。同じ場所を指す）。
pub fn without_cur_dir(path: &Path) -> PathBuf {
    path.components()
        .filter(|component| !matches!(component, std::path::Component::CurDir))
        .collect()
}

/// 利用者向けの理由（検証の失敗は `validation failed` だけにせず、項目の文言を出す）。
fn describe(err: &BantoError) -> String {
    match err {
        BantoError::Validation { field_errors } if !field_errors.is_empty() => field_errors
            .iter()
            .map(|field| field.message.as_str())
            .collect::<Vec<_>>()
            .join(" / "),
        other => other.to_string(),
    }
}

/// フォルダを（無ければ）作り、小さなファイルを書いて消せることを確かめる。
///
/// 試し書きのファイル名は乱数の nonce を含み、`create_new(true)`（既にあれば失敗）で作る。
/// 既存のファイル（クラッシュで残った古い試し書き・PID の再利用・同じネットワークフォルダへ
/// 書く別の PC のもの）を切り詰めて壊さない。名前が衝突したら nonce を替えてやり直す。
pub async fn check_writable_dir(dir: &Path) -> Result<(), RecoveryError> {
    let fail = |what: &str, err: std::io::Error| {
        RecoveryError(format!(
            "保存先のフォルダ（{}）を{what}: {err}",
            dir.display()
        ))
    };
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(|err| fail("作れませんでした", err))?;
    for _ in 0..WRITE_CHECK_ATTEMPTS {
        let probe = write_check_path(dir).map_err(|err| {
            RecoveryError(format!(
                "保存先のフォルダ（{}）の書き込み確認で、試し書きの名前を作れませんでした: {err}",
                dir.display()
            ))
        })?;
        let opened = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&probe)
            .await;
        let file = match opened {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(fail("書き込みに使えません", err)),
        };
        // ここから先は自分が作ったファイルだけ。どの経路でも消す（失敗・取消しでも残さない）。
        let guard = WriteCheckFile(Some(probe));
        // 書き込み・flush の結果を一度保持し、**ファイルを閉じてから**消す（Windows では
        // 開いたままのファイルを消せない。`?` で早く戻ると guard が `file` より先に drop
        // されて、ディスクが一杯などの検出すべき失敗のたびに試し書きが残る）。
        // `write_probe` は `file` を値で受けて関数の終わりに閉じ、キャンセル時も内側の
        // future（= `file`）が guard より先に drop される。
        let written = write_probe(file).await;
        let removed = guard.remove().await;
        written.map_err(|err| fail("書き込みに使えません", err))?;
        return removed
            .map_err(|err| fail("書き込みに使えません（確認用のファイルを消せません）", err));
    }
    Err(fail(
        "書き込みに使えません",
        std::io::Error::from(std::io::ErrorKind::AlreadyExists),
    ))
}

/// 試し書き（書いて flush する）。`file` は戻る前に閉じる（成功・失敗とも）。
async fn write_probe(mut file: tokio::fs::File) -> std::io::Result<()> {
    file.write_all(b"ok").await?;
    file.flush().await
}

const WRITE_CHECK_ATTEMPTS: usize = 16;

/// 試し書きのパスを作る。OS の乱数源が使えないときは panic せず `Err`（窓に出せる）で返す。
/// 乱数源の失敗は注入できないため、この分岐の単体テストは省略（`?` で伝えるだけ）。
fn write_check_path(dir: &Path) -> Result<PathBuf, getrandom::Error> {
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes)?;
    Ok(dir.join(format!(
        ".chronogazer-write-check-{:016x}.tmp",
        u64::from_le_bytes(bytes)
    )))
}

/// 試し書きのファイルの後始末（正常時は `remove` で消して結果を返し、それ以外は Drop で消す）。
struct WriteCheckFile(Option<PathBuf>);

impl WriteCheckFile {
    async fn remove(mut self) -> std::io::Result<()> {
        match self.0.take() {
            Some(path) => tokio::fs::remove_file(&path).await,
            None => Ok(()),
        }
    }
}

impl Drop for WriteCheckFile {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests;
