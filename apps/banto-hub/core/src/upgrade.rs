//! 起動時の DB の移行の前後処理（R2b-1d、docs/banto-hub-connection-settings-design.md
//! §6.1 の 2-b・§6.3・§6.4・§6.6、§5.3 の最後の行）。
//!
//! migration `0025`（`banto-tags`、接続設定を `settings` の JSON と秘密の表へ移す）は
//! **戻せない**（0025 を当てた DB は 0001〜0024 の migrator では開けない）。そこで Hub は
//! `banto_tags::migrate` の前に、このモジュールで次を行う（[`prepare_0025`]、
//! `crate::db::init_db_with_registry` が呼ぶ）。
//!
//! # 起動時の順序（§6.1 の 2-a〜2-c を、この PR で決めた並びで）
//!
//! ```text
//! 2-a  接続・旧形式の拒否・Hub 自身の migrator（crate::db）
//! 2-b  1. 残り物の掃除（毎回の起動）: config/ の `.banto-hub-0025-dryrun-*` と
//!         `<DB>.before-0025-*.bak.partial`（と -journal ほか）を消す。消せなければ
//!         起動を止めてパスを出す（UpgradeError::LeftoverNotRemoved）
//!      2. 0025 が未適用か（`_sqlx_migrations_banto_tags`）。適用済み・新規の DB なら
//!         ここで終わり
//!      3. 写しでの予行と preflight（§6.3）: 通らなければ起動を断る
//!         （UpgradeError::Preflight → HubStartError::ConnectionSettingsPreflight）。
//!         DB は一切変わらない
//!      4. バックアップ（§6.4）: パスワードを除いて VACUUM した `.bak`
//!      5. 清掃待ちの印 `secrets.vacuum_pending = true`（§6.5 の注。VACUUM そのものは
//!         R2b-1e）
//! 2-c  banto_tags::migrate（0025 を含む）→ banto_collect::migrate（crate::db）
//!      → 秘密の行と参照の整合の確認（[`check_secret_integrity`]、crate::runtime が
//!         毎回の起動で呼ぶ）
//! ```
//!
//! **予行をバックアップより先にした理由**（§6.1 の図はバックアップ → 予行の順に書いて
//! いるが、どちらも「本物の migrate の前」で、決めるのは R2b-1d に任されていた）:
//!
//! - preflight で断る起動（旧版で直すまで何度でも起こりうる）のたびに、使われない
//!   `.bak` が増えない。バックアップは「これから本当に移行する」ときだけ作る。
//! - バックアップが本物の migrate の**直前**になる（§6.4「移行の直前に」）。予行に
//!   時間がかかる大きな DB でも、バックアップと移行の間に他の書き込みが入る隙が無い
//!   （どちらも起動の途中で、まだ誰も DB に書かない）。
//! - 予行が DB に触らないこと（写しだけ）は順序に依らない。
//!
//! # 予行と preflight（§6.3）
//!
//! `VACUUM INTO` で config/ の中（ACL は DB と同じ）に写しを作り、写しに
//! `banto_tags::migrate` を当て、**変換の正は SQL の 1 つだけ**（Rust で写し方を書き直さ
//! ない）のまま、移行後の全接続をレジストリで確かめる
//! （`banto_collect::driver_exclusion`: ドライバの有無・設定の版・`validate_config`・
//! 保存の層の形の検査）。
//!
//! | 結果 | 何が |
//! | ---- | ---- |
//! | **断る**（移行しない） | ドライバが無い行（`hub` を除く）・検証に通らない行・版が新しすぎる行・秘密の行の数が「旧表の非空のパスワードの数」と違う・参照と行が食い違う・写しへの migrate が失敗した（例: `push` の `settings` が JSON のオブジェクトでない行。分かれば行を出す） |
//! | **報告だけ**（移行する） | 捨てる値（§3.2: プロトコルが読まない列で既定と違ったもの）・`hub` の行（移行後は §4.4 の `driverUnavailable` で外れる。今も厳格版の計画で収集できない） |
//!
//! 写しは成功・失敗・パニックのどれでも消す（[`TempFile`] の `Drop`。消せなければ起動を
//! 止めてパスを出す。写しにはパスワードの平文がある）。
//!
//! # バックアップ（§6.4）
//!
//! 作りかけの名前 `<DB>.before-0025-<UTC>.bak.partial` に `VACUUM INTO` し、
//! `journal_mode = DELETE` で開いて `plc_connections.password` を NULL に、
//! `pending_changes.payload` から（状態を問わず）`password` を外し、`VACUUM` して閉じて
//! から `.bak` へ `rename` する。平文を含みうるのは `.partial` の名前のものだけで、
//! 途中で落ちれば次の起動の掃除（2-b の 1）で消える（§6.6）。完成した `.bak` は消さない。
//!
//! # しないこと
//!
//! - Windows での `plain-v1` → `dpapi-machine-v1` の封印し直し・未適用キューの封印・
//!   `VACUUM` と清掃待ちの印を消すこと（§6.5、R2b-1e）。ここは印を**立てる**だけ。
//! - ChronoGazer の DB の起動（§6.7）。掃除・未適用の検出・バックアップの部品は
//!   `banto_tags::upgrade` に置き、ここはそれを呼ぶ（予行と preflight はレジストリを持つ Hub だけ）。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use banto_core::{BantoError, ListParams};
use banto_driver_api::DriverRegistry;
use banto_tags::upgrade::{copy_options, vacuum_into, TempFile};
use banto_tags::{PlcConnection, PlcConnectionService, HUB_PROTOCOL};
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::SqlitePool;

use crate::hub_log::{log_err_line, log_line};

// 名前の型と工程は `banto_tags::upgrade`（ChronoGazer と共有。R2b-1d、設計 §6.7）。
pub use banto_tags::upgrade::{
    config_dir_of, BackupStep, BACKUP_INFIX, BACKUP_SUFFIX, DRYRUN_PREFIX, MIGRATION_0025_VERSION,
    PARTIAL_SUFFIX,
};

/// 予行の写しの末尾（`{DRYRUN_PREFIX}{nonce}{DRYRUN_SUFFIX}`）。
const DRYRUN_SUFFIX: &str = ".sqlite3";

/// 清掃待ちの印（Hub の `settings` 表、§6.5 の 2）。R2b-1d は立てるだけ。
pub const VACUUM_PENDING_KEY: &str = "secrets.vacuum_pending";

/// preflight の 1 行（断った行・報告だけの行）。`errors` は人が読む文言で、秘密値を
/// 含まない（秘密のキーは参照しか持たず、参照は検証に渡さない）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreflightRow {
    pub id: i64,
    pub name: String,
    pub protocol: String,
    pub errors: Vec<String>,
}

/// 予行と preflight の結果（§6.3 の 5）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PreflightReport {
    /// 断る行（1 件でもあれば移行しない）。
    pub refused: Vec<PreflightRow>,
    /// 行に結び付かない断る理由（秘密の行の数の食い違い・写しへの migrate の失敗）。
    pub problems: Vec<String>,
    /// 報告だけの行（捨てる値・`hub` の行）。移行は進む。
    pub notes: Vec<PreflightRow>,
}

impl PreflightReport {
    /// 断るか（断る行か行に結び付かない理由が 1 つでもある）。
    pub fn is_refused(&self) -> bool {
        !self.refused.is_empty() || !self.problems.is_empty()
    }
}

/// preflight で断ったときの起動の失敗の中身（`HubStartError::ConnectionSettingsPreflight`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreflightRefusal {
    pub db_path: PathBuf,
    pub rows: Vec<PreflightRow>,
    pub problems: Vec<String>,
}

impl std::fmt::Display for PreflightRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "接続設定の移行（R2b-1、migration 0025）の予行で問題が見つかったため、移行せずに\
             起動を中止しました。DB（{}）は変更していません。旧版（v0.2.0-alpha.33 など）の \
             banto-hub でこれらの接続を直すか削除してから、この版で起動し直してください\
             （手順は docs/banto-hub-operations.md）。",
            self.db_path.display()
        )?;
        for row in &self.rows {
            write!(
                f,
                "\n  - 接続 #{} {}（{}）: {}",
                row.id,
                row.name,
                row.protocol,
                row.errors.join(" / ")
            )?;
        }
        for problem in &self.problems {
            write!(f, "\n  - {problem}")?;
        }
        Ok(())
    }
}

/// 起動時の移行の前後処理の失敗。どれも起動を止める。`Display` が利用者向けの説明。
#[derive(Debug, thiserror::Error)]
pub enum UpgradeError {
    /// 前の起動の残り物（予行の写し・作りかけのバックアップ。平文を含みうる）を消せない。
    #[error(
        "前回の起動で残ったファイル {path} を消せません（パスワードの平文を含みうるため、\
         消せるまで起動しません。手で消してから起動し直してください）: {error}"
    )]
    LeftoverNotRemoved {
        path: PathBuf,
        error: std::io::Error,
    },
    /// 予行の写しを消せない（平文を含む）。
    #[error(
        "移行の予行の写し {path} を消せません（パスワードの平文を含むため、消せるまで起動\
         しません。手で消してから起動し直してください）: {error}"
    )]
    DryRunCopyNotRemoved {
        path: PathBuf,
        error: std::io::Error,
    },
    /// 予行そのもの（写しの作成・読み取り）に失敗した。DB は変わっていない。
    #[error("移行の予行に失敗しました（DB は変更していません）: {0}")]
    DryRun(String),
    /// バックアップに失敗した。DB は変わっていない（作りかけは次の起動で消える）。
    #[error(
        "移行の前のバックアップ {path} を作れませんでした（DB は変更していません）: {message}"
    )]
    Backup { path: PathBuf, message: String },
    /// preflight が断った（§6.3）。
    #[error("{0}")]
    Preflight(PreflightRefusal),
    /// 0025 が未適用なのに、レジストリを持たない入口（昇格ヘルパーなど）で開いた。
    /// 予行もバックアップもせずに戻せない移行を当てないため、開かない。
    #[error(
        "この DB（{path}）は接続設定の移行（R2b-1、migration 0025）がまだです。先に \
         banto-hub（サービス・デスクトップ・コンソールのどれか）を一度起動して移行してから、\
         もう一度実行してください"
    )]
    NeedsHubStartup { path: PathBuf },
    /// 移行の前後処理の DB の読み書きに失敗した。
    #[error("移行の準備で DB の読み書きに失敗しました: {0}")]
    Storage(String),
}

fn storage(err: impl std::fmt::Display) -> UpgradeError {
    UpgradeError::Storage(err.to_string())
}

/// `banto_tags::upgrade` の失敗は同じ文言の変種へ写す（Hub の起動の失敗の型は変えない）。
impl From<banto_tags::upgrade::UpgradeError> for UpgradeError {
    fn from(err: banto_tags::upgrade::UpgradeError) -> Self {
        use banto_tags::upgrade::UpgradeError as Tags;
        match err {
            Tags::LeftoverNotRemoved { path, error } => Self::LeftoverNotRemoved { path, error },
            Tags::Backup { path, message } => Self::Backup { path, message },
            Tags::Storage(message) => Self::Storage(message),
        }
    }
}

/// 2-b の 1（§6.3 の 4・§6.4、毎回の起動）: config/ の残り物を消す。消したパスを返す。
/// 1 つでも消せなければ [`UpgradeError::LeftoverNotRemoved`]（起動を止める）。
pub fn remove_leftovers(db_path: &Path) -> Result<Vec<PathBuf>, UpgradeError> {
    Ok(banto_tags::upgrade::remove_leftovers(db_path)?)
}

/// 0025 が未適用か（2-b の 2）。banto-tags の記録表が無い（新規の DB。migrate が全部を
/// 作るので、写すものもバックアップするものも無い）なら `false`。
pub async fn pending_0025(pool: &SqlitePool) -> Result<bool, UpgradeError> {
    Ok(banto_tags::upgrade::pending_0025(pool).await?)
}

/// 2-b の 2〜5 をまとめた入口（`crate::db::init_db_with_registry` が Hub の migrator の
/// 後・`banto_tags::migrate` の前に呼ぶ）。掃除（2-b の 1）は呼び出し側が先に行う。
///
/// - 0025 が適用済み・新規の DB: 何もしない（`Ok(None)`）。
/// - 未適用で `registry` が無い（昇格ヘルパーなど）: [`UpgradeError::NeedsHubStartup`]。
/// - 未適用: 予行と preflight → 断れば [`UpgradeError::Preflight`]（DB は変わらない）→
///   バックアップ → 清掃待ちの印。報告だけの行・バックアップのパス・予行の時間はログに出す。
pub async fn prepare_0025(
    pool: &SqlitePool,
    db_path: &Path,
    registry: Option<&Arc<DriverRegistry>>,
) -> Result<Option<UpgradeOutcome>, UpgradeError> {
    if !pending_0025(pool).await? {
        return Ok(None);
    }
    let Some(registry) = registry else {
        return Err(UpgradeError::NeedsHubStartup {
            path: db_path.to_path_buf(),
        });
    };
    log_line(&format!(
        "banto-hub: 接続設定の移行（R2b-1、migration 0025）の前に、写しで予行します: {}",
        db_path.display()
    ));
    let started = Instant::now();
    let report = dry_run_and_preflight(pool, db_path, registry).await?;
    let dry_run_elapsed = started.elapsed();
    for note in &report.notes {
        log_line(&format!(
            "banto-hub: 移行の報告: 接続 #{} {}（{}）: {}",
            note.id,
            note.name,
            note.protocol,
            note.errors.join(" / ")
        ));
    }
    if report.is_refused() {
        return Err(UpgradeError::Preflight(PreflightRefusal {
            db_path: db_path.to_path_buf(),
            rows: report.refused,
            problems: report.problems,
        }));
    }
    log_line(&format!(
        "banto-hub: 移行の予行が通りました（{} ms）",
        dry_run_elapsed.as_millis()
    ));
    let backup = backup_before_0025(pool, db_path).await?;
    log_line(&format!(
        "banto-hub: 移行の前のバックアップを作りました（postgres 接続のパスワードは含みません。\
         戻すときは入れ直してください）: {}",
        backup.display()
    ));
    set_vacuum_pending(pool).await?;
    Ok(Some(UpgradeOutcome {
        backup,
        report,
        dry_run_elapsed,
    }))
}

/// [`prepare_0025`] が移行の準備をしたときの結果（テストと診断のため）。
#[derive(Debug, Clone)]
pub struct UpgradeOutcome {
    pub backup: PathBuf,
    pub report: PreflightReport,
    pub dry_run_elapsed: Duration,
}

/// 2-b の 5（§6.5 の注）: 0025 が DROP する旧表の平文が空きページと WAL に残るので、
/// migrate の**前**に清掃待ちの印を立てる（0025 は banto-tags の migrator の
/// トランザクションなので同じトランザクションにできない。先に立てておけば migrate の
/// 直後に落ちても印は残る）。印を見て `VACUUM` し、消すのは R2b-1e。
pub async fn set_vacuum_pending(pool: &SqlitePool) -> Result<(), UpgradeError> {
    sqlx::query(
        "INSERT INTO settings (key, value) VALUES (?, 'true') \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind(VACUUM_PENDING_KEY)
    .execute(pool)
    .await
    .map_err(storage)?;
    Ok(())
}

// ---- 一時ファイルの後始末は banto_tags::upgrade::TempFile ---------------------------------

fn random_hex(bytes: usize) -> Result<String, UpgradeError> {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).map_err(|_| storage("乱数を作れませんでした"))?;
    Ok(hex::encode(buf))
}

// ---- 予行と preflight（§6.3） --------------------------------------------------------------

/// 移行の前の旧表の 1 行（平らな列。パスワードは読まない）。
#[derive(Debug, Clone, sqlx::FromRow)]
struct OldRow {
    id: i64,
    host: Option<String>,
    port: Option<i64>,
    unit_id: Option<i64>,
    word_order: Option<String>,
    cpu_type: Option<String>,
    database: Option<String>,
    username: Option<String>,
}

/// 写しで予行し、preflight の結果を返す（§6.3）。写しは成功・失敗・パニックのどれでも
/// 消す。消せなければ [`UpgradeError::DryRunCopyNotRemoved`]。DB 本体には触らない
/// （`VACUUM INTO` は読み取りだけ）。
pub async fn dry_run_and_preflight(
    pool: &SqlitePool,
    db_path: &Path,
    registry: &Arc<DriverRegistry>,
) -> Result<PreflightReport, UpgradeError> {
    let copy_path =
        config_dir_of(db_path).join(format!("{DRYRUN_PREFIX}{}{DRYRUN_SUFFIX}", random_hex(8)?));
    let copy = TempFile::new(copy_path.clone());
    let result = dry_run_on_copy(pool, &copy_path, registry).await;
    copy.finish()
        .map_err(|(path, error)| UpgradeError::DryRunCopyNotRemoved { path, error })?;
    result
}

async fn dry_run_on_copy(
    pool: &SqlitePool,
    copy_path: &Path,
    registry: &Arc<DriverRegistry>,
) -> Result<PreflightReport, UpgradeError> {
    vacuum_into(pool, copy_path)
        .await
        .map_err(UpgradeError::DryRun)?;
    // 1 本だけの接続のプール（banto_tags::migrate がプールを取るため）。返る前に必ず閉じる
    // （閉じないと Windows では写しを消せない）。
    let copy_pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(copy_options(copy_path))
        .await
        .map_err(|err| UpgradeError::DryRun(err.to_string()))?;
    let result = preflight_copy(&copy_pool, registry).await;
    copy_pool.close().await;
    result
}

async fn preflight_copy(
    copy: &SqlitePool,
    registry: &Arc<DriverRegistry>,
) -> Result<PreflightReport, UpgradeError> {
    let dry = |err: sqlx::Error| UpgradeError::DryRun(err.to_string());
    // 移行の前の写しから読むもの: 旧表の行・非空のパスワードの数・オブジェクトでない
    // push の settings（0025 の CHECK に落ちる行）。
    let old_rows: Vec<OldRow> = sqlx::query_as(
        "SELECT id, host, port, unit_id, word_order, cpu_type, database, username \
         FROM plc_connections ORDER BY id",
    )
    .fetch_all(copy)
    .await
    .map_err(dry)?;
    let expected_secrets: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM plc_connections WHERE protocol = 'postgres' \
         AND password IS NOT NULL AND password <> ''",
    )
    .fetch_one(copy)
    .await
    .map_err(dry)?;
    let non_object_push: Vec<(i64, String, String)> = sqlx::query_as(
        "SELECT id, name, protocol FROM plc_connections WHERE protocol = 'push' \
         AND settings IS NOT NULL \
         AND (json_valid(settings) = 0 OR json_type(settings) <> 'object') ORDER BY id",
    )
    .fetch_all(copy)
    .await
    .map_err(dry)?;

    let mut report = PreflightReport::default();
    if let Err(err) = banto_tags::migrate(copy).await {
        report.refused = non_object_push
            .into_iter()
            .map(|(id, name, protocol)| PreflightRow {
                id,
                name,
                protocol,
                errors: vec!["push の settings が JSON のオブジェクトではありません".to_string()],
            })
            .collect();
        report.problems.push(format!(
            "移行（migration 0025）を写しに当てると失敗しました: {err}"
        ));
        return Ok(report);
    }

    let service = PlcConnectionService::with_registry(copy.clone(), registry.clone());
    let connections = service
        .list(ListParams::default())
        .await
        .map_err(|err| UpgradeError::DryRun(err.to_string()))?
        .rows;
    let secret_rows = service
        .all_secret_rows()
        .await
        .map_err(|err| UpgradeError::DryRun(err.to_string()))?;

    let old_by_id: HashMap<i64, &OldRow> = old_rows.iter().map(|row| (row.id, row)).collect();
    for conn in &connections {
        if conn.protocol == HUB_PROTOCOL {
            report.notes.push(row_of(
                conn,
                vec![
                    "hub 接続（ChronoGazer が使う種別）は banto-hub では収集しません。移行後は\
                     「ドライバがありません」で外れます（今も収集できません）"
                        .to_string(),
                ],
            ));
        } else if let Some(reason) = banto_collect::driver_exclusion(conn, registry) {
            let errors = match reason {
                banto_collect::ExclusionReason::InvalidSettings { errors } => errors,
                other => vec![other.message()],
            };
            report.refused.push(row_of(conn, errors));
        }
        if let Some(old) = old_by_id.get(&conn.id) {
            let dropped = dropped_values(old, conn);
            if !dropped.is_empty() {
                report.notes.push(row_of(conn, dropped));
            }
        }
    }

    // 秘密の行の数が「旧表の非空のパスワードの数」と一致し、参照と行が 1 対 1 で合うこと。
    let actual = secret_rows.len() as i64;
    if actual != expected_secrets {
        report.problems.push(format!(
            "移行後の秘密値の行の数（{actual}）が、移行前の postgres 接続の非空のパスワードの数\
             （{expected_secrets}）と一致しません"
        ));
    }
    let integrity = integrity_of(
        connections
            .iter()
            .map(|conn| (conn.id, conn.name.clone(), conn.settings.clone())),
        &secret_rows,
    );
    for (id, name, key) in &integrity.missing {
        report.problems.push(format!(
            "移行後の接続 #{id} {name} の {key} の参照に秘密値の行がありません"
        ));
    }
    for orphan in &integrity.orphans {
        report.problems.push(format!(
            "移行後の秘密値の行（接続 #{}・{}）を参照する接続がありません",
            orphan.connection_id, orphan.key
        ));
    }
    Ok(report)
}

fn row_of(conn: &PlcConnection, errors: Vec<String>) -> PreflightRow {
    PreflightRow {
        id: conn.id,
        name: conn.name.clone(),
        protocol: conn.protocol.clone(),
        errors,
    }
}

/// §3.2 の「捨てる値」: 移行前の平らな列と、移行後の `settings` から作った平らな形
/// （REST が返す形）が違う欄。空文字と NULL は同じに扱う（読む経路が無い）。
fn dropped_values(old: &OldRow, new: &PlcConnection) -> Vec<String> {
    fn text(value: &Option<String>) -> &str {
        value.as_deref().unwrap_or("")
    }
    let mut dropped = Vec::new();
    let mut check = |field: &str, before: String, after: String| {
        if before != after {
            dropped.push(format!(
                "捨てる値: {field} {before:?} → {after:?}（このプロトコルは読みません）"
            ));
        }
    };
    check("host", text(&old.host).to_string(), new.host.clone());
    check(
        "port",
        old.port.unwrap_or_default().to_string(),
        new.port.to_string(),
    );
    check(
        "unitId",
        old.unit_id.unwrap_or(1).to_string(),
        new.unit_id.to_string(),
    );
    check(
        "wordOrder",
        text(&old.word_order).to_string(),
        new.word_order.clone(),
    );
    check(
        "cpuType",
        text(&old.cpu_type).to_string(),
        new.cpu_type.clone(),
    );
    check(
        "database",
        text(&old.database).to_string(),
        text(&new.database).to_string(),
    );
    check(
        "username",
        text(&old.username).to_string(),
        text(&new.username).to_string(),
    );
    dropped
}

// ---- バックアップ（§6.4） -----------------------------------------------------------------

/// 移行の直前のバックアップ（§6.4）。完成した `.bak` のパスを返す。手順は
/// `banto_tags::upgrade::backup_before_0025`（ChronoGazer と共有。`pending_changes` を持つ
/// Hub では、キューのペイロードの `password` も除く）。
pub async fn backup_before_0025(
    pool: &SqlitePool,
    db_path: &Path,
) -> Result<PathBuf, UpgradeError> {
    Ok(banto_tags::upgrade::backup_before_0025(pool, db_path).await?)
}

/// [`backup_before_0025`] の本体。`crash_after` の工程に来たら、作りかけを**残したまま**
/// 失敗で返る（プロセスが落ちたのと同じ。テストだけが使う）。
#[cfg(test)]
pub(crate) async fn backup_with(
    pool: &SqlitePool,
    db_path: &Path,
    crash_after: Option<BackupStep>,
) -> Result<PathBuf, UpgradeError> {
    Ok(banto_tags::upgrade::backup_with(pool, db_path, crash_after).await?)
}

// ---- 起動時の整合の確認（§5.3 の最後の行） ------------------------------------------------

/// 起動時の整合の確認の結果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IntegrityReport {
    /// 参照があって行が無い `(接続 id, 接続名, キー)`。その接続のその値は
    /// `Unreadable { Missing }`（読む側が `secrets::open_connection_secret` で同じ判断をする）。
    pub missing: Vec<(i64, String, String)>,
    /// 参照の無い行（消した）。
    pub orphans: Vec<OrphanSecret>,
}

/// 参照の無い秘密値の行（値は持たない）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrphanSecret {
    pub r#ref: String,
    pub connection_id: i64,
    pub key: String,
}

/// 参照と行の突き合わせ（純関数）。参照は `settings` のどのキーでも `{"$secret": ref}`
/// の形の値（レジストリの秘密のキーに限らない: 参照されている行は消さない側に倒す）。
/// 行が参照と合うのは、同じ接続・同じキー・同じ参照のときだけ。
fn integrity_of(
    connections: impl IntoIterator<Item = (i64, String, serde_json::Map<String, serde_json::Value>)>,
    rows: &[banto_tags::StoredSecretRow],
) -> IntegrityReport {
    let mut wanted: HashSet<(i64, String, String)> = HashSet::new();
    let mut report = IntegrityReport::default();
    let have: HashSet<(i64, &str, &str)> = rows
        .iter()
        .map(|row| {
            (
                row.connection_id,
                row.key.as_str(),
                row.sealed.r#ref.as_str(),
            )
        })
        .collect();
    for (id, name, settings) in connections {
        let mut keys: Vec<&String> = settings.keys().collect();
        keys.sort();
        for key in keys {
            let Some(r#ref) = banto_tags::connection_secret::secret_ref_of(&settings, key) else {
                continue;
            };
            if !have.contains(&(id, key.as_str(), r#ref)) {
                report.missing.push((id, name.clone(), key.clone()));
            }
            wanted.insert((id, key.clone(), r#ref.to_string()));
        }
    }
    for row in rows {
        let triple = (row.connection_id, row.key.clone(), row.sealed.r#ref.clone());
        if !wanted.contains(&triple) {
            report.orphans.push(OrphanSecret {
                r#ref: row.sealed.r#ref.clone(),
                connection_id: row.connection_id,
                key: row.key.clone(),
            });
        }
    }
    report
}

/// 起動時の整合の確認（§5.3 の最後の行、毎回の起動。FK を張らない代わり、Q15）。
///
/// - 参照があって行が無い → 報告だけ（その接続は読む側で `Unreadable { Missing }`。空の
///   パスワードにしない）。
/// - 行があって参照が無い → 消す（同じ `BEGIN IMMEDIATE` のトランザクションで読んで消す。
///   トランザクションの外で読んで書き戻さない）。
///
/// ログは呼び出し側（`crate::runtime`）が出す（[`log_integrity`]）。
pub async fn check_secret_integrity(pool: &SqlitePool) -> Result<IntegrityReport, BantoError> {
    let mut tx = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(banto_storage::storage_error)?;
    let connections: Vec<(i64, String, String)> =
        sqlx::query_as("SELECT id, name, settings FROM plc_connections ORDER BY id")
            .fetch_all(&mut *tx)
            .await
            .map_err(banto_storage::storage_error)?;
    let rows = banto_tags::connection_secret::load_all_secret_rows(&mut *tx)
        .await
        .map_err(banto_storage::storage_error)?;
    let parsed = connections.into_iter().map(|(id, name, settings)| {
        let map = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&settings)
            .unwrap_or_default();
        (id, name, map)
    });
    let report = integrity_of(parsed, &rows);
    for orphan in &report.orphans {
        sqlx::query("DELETE FROM plc_connection_secrets WHERE ref = ?")
            .bind(&orphan.r#ref)
            .execute(&mut *tx)
            .await
            .map_err(banto_storage::storage_error)?;
    }
    tx.commit().await.map_err(banto_storage::storage_error)?;
    Ok(report)
}

/// [`check_secret_integrity`] の結果をログに出す（1 件 1 行。値・参照は出さない）。
pub fn log_integrity(report: &IntegrityReport) {
    for (id, name, key) in &report.missing {
        log_err_line(&format!(
            "banto-hub: [WARN] 接続 {name}（#{id}）の {key} の秘密値の行がありません。\
             この接続の {key} は読めません（passwordState: unreadable）。接続の編集で入れ直して\
             ください"
        ));
    }
    for orphan in &report.orphans {
        log_line(&format!(
            "banto-hub: 参照する接続の無い秘密値の行（接続 #{}・{}）を消しました",
            orphan.connection_id, orphan.key
        ));
    }
}

#[cfg(test)]
mod tests;
