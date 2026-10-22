//! 起動時の DB の移行の前後処理（R2b-1d・R2b-1e、docs/banto-hub-connection-settings-design.md
//! §6.1 の 2-b・§6.3・§6.4・§6.5・§6.6、§5.3 の最後の行）。
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
//!      5. 清掃待ちの印 `secrets.vacuum_pending = true`（§6.5 の注）
//! 2-c  banto_tags::migrate（0025 を含む）→ banto_collect::migrate（crate::db）
//! 3    秘密値の後処理（[`secrets_upkeep`]、R2b-1e、crate::runtime が毎回の起動で呼ぶ）
//!      → 秘密の行と参照の整合の確認（[`check_secret_integrity`]、毎回の起動）
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
//! `VACUUM INTO` で config/ の中（ACL は DB と同じ）に写しを作り、写しをまず 0024 まで
//! 進めて（`banto_tags::upgrade::migrate_before_0025`。alpha.32 以前の DB には 0021・0023 の
//! 列がまだ無い）移行の前の値を読み、それから `banto_tags::migrate`（0025）を当て、
//! **変換の正は SQL の 1 つだけ**（Rust で写し方を書き直さない）のまま、移行後の全接続をレジストリで確かめる
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
//! # 秘密値の後処理（§6.5・§6.6、R2b-1e、毎回の起動・冪等）
//!
//! [`secrets_upkeep`] = [`rewrite_secrets`]（工程 1）→ [`vacuum_if_pending`]（工程 2）。
//!
//! 1. `BEGIN IMMEDIATE` の 1 トランザクションで（[`rewrite_secrets`]）:
//!    - (a) protector があるとき（Windows の DPAPI）、`plc_connection_secrets` の `plain-v1`
//!      の行を開いて**同じ参照のまま**封印し直す（entropy は参照とキー）。
//!    - (b) 未適用（`pending`・`applying`・`failed`）の `plc_connections.create` / `.update` の
//!      ペイロードの平文の `password` を封印した `sealedPassword` に替える。
//!    - (c) 同じ 3 つの状態の `plain-v1` の `sealedPassword` を、protector があれば封印し直す。
//!    - (d) 履歴（`applied`・`canceled`）のペイロードから `password` と `sealedPassword` の
//!      両方を外して `passwordSet` に替える。
//!    - (e) 1 つでも書き換えたら、**同じトランザクションで**清掃待ちの印を立てる。何も
//!      書き換えなければ何も書かない（印も立てない）。
//!
//!    1 件の封印し直し・封印の失敗（DPAPI の失敗・壊れた値）は起動を止めない: その行は
//!    そのまま残し（平文の守りは今と同じ）、[`UpkeepReport::failures`] に数える。状態の画面は
//!    [`plaintext_remaining`] で「移行待ちの秘密値が平文で残っています」を出す。
//! 2. 印があるときだけ、トランザクションの外で（[`vacuum_if_pending`]）
//!    `PRAGMA wal_checkpoint(TRUNCATE)` → `VACUUM` → `PRAGMA wal_checkpoint(TRUNCATE)`
//!    （WAL の DB では `VACUUM` の結果はまず WAL に入り、本体の古いページは次の checkpoint まで
//!    残るので、後ろの checkpoint で本体を書き換えて切り詰める）→ 印を消す。どれかが失敗
//!    したり checkpoint が終わり切らなければ印を残し、次の起動でやり直す（§6.6）。
//!
//! 工程 1 の途中で落ちればトランザクションごと戻り、次の起動でやり直す。工程 1 の commit の
//! 後・工程 2 の前に落ちても、印が同じトランザクションで立っているので次の起動で
//! `VACUUM` だけやり直す（印は「清掃がまだ」を表す。完了の印ではない、§6.5 の 2）。
//!
//! 工程 1 の外で `plain-v1` の秘密の行が消える・書き換わる書き込み（封印し直しに失敗して
//! 残った行の、操作者によるパスワードの入れ替え・消去、プロトコルの変更、接続の削除・
//! 一括削除、[`check_secret_integrity`] が消す参照の無い行。#778 のレビュー P2）は、
//! banto-tags の migration `0026` のトリガが同じトランザクションで
//! `plc_connection_secrets_cleanup` に数える。工程 2 はこれも印として読み、`VACUUM` の後に
//! 0 に戻す。トリガは OS を問わないので、非 Windows（全部 `plain-v1`、§5.8）でもパスワードを
//! 入れ替えた次の起動で `VACUUM` する（古いパスワードを空きページに残さない。入れ替えは
//! まれなので起動の重さは問題にしない）。
//!
//! # しないこと
//!
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

/// 清掃待ちの印（Hub の `settings` 表、§6.5 の 2）。立てるのは [`set_vacuum_pending`]
/// （0025 の前）と [`rewrite_secrets`]（書き換えと同じトランザクション）、消すのは
/// [`vacuum_if_pending`]（`VACUUM` と checkpoint が済んだ後）。
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
/// 直後に落ちても印は残る）。印を見て `VACUUM` し、消すのは [`vacuum_if_pending`]。
pub async fn set_vacuum_pending(pool: &SqlitePool) -> Result<(), UpgradeError> {
    let mut conn = pool.acquire().await.map_err(storage)?;
    crate::pending_changes::set_vacuum_pending_in(&mut conn)
        .await
        .map_err(|err| storage(err.to_string()))?;
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
    // まず写しを 0024 まで進める（alpha.32 以前の DB には `settings`（0023）・`cpu_type`
    // （0021）などの列がまだ無く、下の SELECT が読めない）。0024 までは戻せる移行で、当てる
    // のは写しだけ（本物は予行が通ってから banto_tags::migrate で全部を当てる）。
    let mut report = PreflightReport::default();
    if let Err(err) = banto_tags::upgrade::migrate_before_0025(copy).await {
        report.problems.push(format!(
            "移行（migration 0024 まで）を写しに当てると失敗しました: {err}"
        ));
        return Ok(report);
    }
    // 移行の前（0024 の形）の写しから読むもの: 旧表の行・非空のパスワードの数・オブジェクトでない
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
        connections.iter().map(|conn| {
            (
                conn.id,
                conn.name.clone(),
                conn.protocol.clone(),
                conn.settings.clone(),
            )
        }),
        &secret_rows,
    );
    for (id, name, key) in &integrity.missing {
        report.problems.push(format!(
            "移行後の接続 #{id} {name} の {key} の参照に秘密値の行がありません"
        ));
    }
    for (id, name, key) in &integrity.malformed {
        report.problems.push(format!(
            "移行後の接続 #{id} {name} の {key} が秘密の参照の形になっていません"
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
    /// キーはあるが形の正しい参照でない `(接続 id, 接続名, キー)`（PR #777 のオーナー
    /// レビュー P2）。その接続のその値は `Unreadable { Invalid }`（読む側が
    /// `secrets::open_connection_secret` で同じ判断をする）。値は持たない。
    pub malformed: Vec<(i64, String, String)>,
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
///
/// 形の違う参照（`malformed`）: 組み込みのプロトコルの秘密のキー（`postgres` の
/// `password`）にある、形の正しい参照でない値と、どのキーでも `$secret` を持つのに形の
/// 正しい参照でない値。
fn integrity_of(
    connections: impl IntoIterator<
        Item = (
            i64,
            String,
            String,
            serde_json::Map<String, serde_json::Value>,
        ),
    >,
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
    for (id, name, protocol, settings) in connections {
        let secret_keys = banto_tags::builtin_secret_keys(&protocol);
        let mut keys: Vec<&String> = settings.keys().collect();
        keys.sort();
        for key in keys {
            use banto_tags::SecretRefLookup;
            let r#ref = match banto_tags::connection_secret::secret_ref_lookup(&settings, key) {
                SecretRefLookup::Ref(r#ref) => r#ref,
                SecretRefLookup::Malformed => {
                    let claimed = settings
                        .get(key)
                        .is_some_and(banto_tags::connection_secret::looks_like_secret_ref);
                    if claimed || secret_keys.contains(&key.as_str()) {
                        report.malformed.push((id, name.clone(), key.clone()));
                    }
                    // A row under the string it names is kept (not an orphan): the value is
                    // reported, not repaired here, and deleting the row would lose it.
                    if let Some(r#ref) = settings
                        .get(key)
                        .and_then(|value| value.get(banto_tags::settings_json::SECRET_REF_KEY))
                        .and_then(serde_json::Value::as_str)
                    {
                        wanted.insert((id, key.clone(), r#ref.to_string()));
                    }
                    continue;
                }
                SecretRefLookup::Absent => continue,
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
/// - キーはあるが形の正しい参照でない → 報告だけ（読む側で `Unreadable { Invalid }`）。
/// - 行があって参照が無い → 消す（同じ `BEGIN IMMEDIATE` のトランザクションで読んで消す。
///   トランザクションの外で読んで書き戻さない）。
///
/// ログは呼び出し側（`crate::runtime`）が出す（[`log_integrity`]）。
pub async fn check_secret_integrity(pool: &SqlitePool) -> Result<IntegrityReport, BantoError> {
    let mut tx = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(banto_storage::storage_error)?;
    let connections: Vec<(i64, String, String, String)> =
        sqlx::query_as("SELECT id, name, protocol, settings FROM plc_connections ORDER BY id")
            .fetch_all(&mut *tx)
            .await
            .map_err(banto_storage::storage_error)?;
    let rows = banto_tags::connection_secret::load_all_secret_rows(&mut *tx)
        .await
        .map_err(banto_storage::storage_error)?;
    let parsed = connections
        .into_iter()
        .map(|(id, name, protocol, settings)| {
            let map = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&settings)
                .unwrap_or_default();
            (id, name, protocol, map)
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
    for (id, name, key) in &report.malformed {
        log_err_line(&format!(
            "banto-hub: [WARN] 接続 {name}（#{id}）の {key} が秘密の参照の形になっていません。\
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

// ---- 秘密値の後処理（§6.5・§6.6、R2b-1e） ------------------------------------------------

/// 未適用キューの状態（封印する・封印し直す側）。
const UNAPPLIED_STATES: [&str; 3] = ["pending", "applying", "failed"];
/// 履歴の状態（パスワードを外す側）。
const HISTORY_STATES: [&str; 2] = ["applied", "canceled"];

/// 後処理の工程 1 で、封印し直し・封印に失敗した 1 件。値・blob は持たない。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpkeepFailure {
    /// `"plc_connection_secrets"`（`id` は接続 id）か `"pending_changes"`（`id` はキューの id）。
    pub table: &'static str,
    pub id: i64,
    /// 失敗の理由（`crate::secrets::SecretError` の文言など。秘密値を含まない）。
    pub error: String,
}

/// 秘密値の後処理の結果（[`secrets_upkeep`]）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UpkeepReport {
    /// (a) `plain-v1` から封印し直した秘密の行の数。
    pub resealed_rows: usize,
    /// (b) 平文の `password` を封印したキューの行の数。
    pub sealed_queue: usize,
    /// (c) `plain-v1` の `sealedPassword` を封印し直したキューの行の数。
    pub resealed_queue: usize,
    /// (d) `password` / `sealedPassword` を外した履歴の行の数。
    pub stripped_history: usize,
    /// 封印し直し・封印に失敗して、そのまま残した行（起動は止めない）。
    pub failures: Vec<UpkeepFailure>,
    /// 工程 1 の DB の失敗（トランザクションごと戻った。次の起動でやり直す）。
    pub rewrite_error: Option<String>,
    /// 工程 2 で `VACUUM` を済ませて印を消した。
    pub vacuumed: bool,
    /// 工程 2 の失敗（印は残る。次の起動でやり直す）。
    pub vacuum_error: Option<String>,
}

impl UpkeepReport {
    /// 工程 1 で何か書き換えたか（= 同じトランザクションで清掃待ちの印を立てたか）。
    pub fn rewrote(&self) -> bool {
        self.resealed_rows + self.sealed_queue + self.resealed_queue + self.stripped_history > 0
    }
}

/// 秘密値の後処理（§6.5、`crate::runtime` が毎回の起動の手順 3 で呼ぶ。冪等）。工程 1
/// （[`rewrite_secrets`]）→ 工程 2（[`vacuum_if_pending`]）。どの失敗も起動を止めない:
/// 結果（[`UpkeepReport`]）に載せて返し、呼び出し側がログに出す（[`log_upkeep`]）。工程 1 が
/// DB の失敗で戻っても、前の起動から残った印があれば工程 2 は行う。
pub async fn secrets_upkeep(
    pool: &SqlitePool,
    secrets: &crate::secrets::ConnectionSecrets,
) -> UpkeepReport {
    let mut report = match rewrite_secrets(pool, secrets).await {
        Ok(report) => report,
        Err(err) => UpkeepReport {
            rewrite_error: Some(err.to_string()),
            ..UpkeepReport::default()
        },
    };
    match vacuum_if_pending(pool).await {
        Ok(vacuumed) => report.vacuumed = vacuumed,
        Err(err) => report.vacuum_error = Some(err.to_string()),
    }
    report
}

/// 工程 1（§6.5 の 1、このモジュールの doc の (a)〜(e)）。`BEGIN IMMEDIATE` の 1 トランザク
/// ションで書き換え、何か書き換えたら同じトランザクションで清掃待ちの印を立てる。書き換える
/// ものが無ければ何も書かない。
pub async fn rewrite_secrets(
    pool: &SqlitePool,
    secrets: &crate::secrets::ConnectionSecrets,
) -> Result<UpkeepReport, BantoError> {
    rewrite_secrets_inner(pool, secrets, false).await
}

/// テスト用（§9 の冪等）: 工程 1 の書き換えと印までを済ませてから、commit の**前**に失敗
/// する（プロセスが落ちたのと同じ。トランザクションは戻る）。
#[cfg(test)]
pub(crate) async fn rewrite_secrets_failing_before_commit(
    pool: &SqlitePool,
    secrets: &crate::secrets::ConnectionSecrets,
) -> Result<UpkeepReport, BantoError> {
    rewrite_secrets_inner(pool, secrets, true).await
}

async fn rewrite_secrets_inner(
    pool: &SqlitePool,
    secrets: &crate::secrets::ConnectionSecrets,
    fail_before_commit: bool,
) -> Result<UpkeepReport, BantoError> {
    use crate::pending_changes::{
        reseal_queued_password, seal_queued_password, strip_queued_password,
    };
    use crate::secrets::{SealedSecret, SecretScheme};

    let storage = banto_storage::storage_error;
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await.map_err(storage)?;
    let mut report = UpkeepReport::default();

    // (a) plain-v1 の秘密の行を、protector があれば同じ参照のまま封印し直す。
    if secrets.seal_scheme() != SecretScheme::PlainV1 {
        let rows: Vec<banto_tags::StoredSecretRow> = sqlx::query_as(
            "SELECT ref, connection_id, key, scheme, blob FROM plc_connection_secrets \
             WHERE scheme = ? ORDER BY connection_id, key",
        )
        .bind(SecretScheme::PlainV1.as_str())
        .fetch_all(&mut *tx)
        .await
        .map_err(storage)?;
        for row in rows {
            let resealed = SealedSecret::from_row(&row.sealed)
                .and_then(|sealed| secrets.reseal_plain(&row.key, &sealed));
            match resealed {
                Ok(Some(resealed)) => {
                    sqlx::query(
                        "UPDATE plc_connection_secrets SET scheme = ?, blob = ? WHERE ref = ?",
                    )
                    .bind(resealed.scheme.as_str())
                    .bind(resealed.blob.as_slice())
                    .bind(resealed.r#ref.as_str())
                    .execute(&mut *tx)
                    .await
                    .map_err(storage)?;
                    report.resealed_rows += 1;
                }
                Ok(None) => {}
                Err(err) => report.failures.push(UpkeepFailure {
                    table: "plc_connection_secrets",
                    id: row.connection_id,
                    error: err.to_string(),
                }),
            }
        }
    }

    // (b)〜(d) 接続の作成・更新のキューの行。
    let queue: Vec<(i64, String, String, String)> = sqlx::query_as(
        "SELECT id, state, source, payload FROM pending_changes \
         WHERE source IN (?, ?) ORDER BY id",
    )
    .bind(crate::pending_changes::CONNECTION_PASSWORD_SOURCES[0])
    .bind(crate::pending_changes::CONNECTION_PASSWORD_SOURCES[1])
    .fetch_all(&mut *tx)
    .await
    .map_err(storage)?;
    for (id, state, source, text) in queue {
        let Ok(mut payload) = serde_json::from_str::<serde_json::Value>(&text) else {
            report.failures.push(UpkeepFailure {
                table: "pending_changes",
                id,
                error: "ペイロードが JSON として読めません".to_string(),
            });
            continue;
        };
        let mut changed = false;
        if UNAPPLIED_STATES.contains(&state.as_str()) {
            // (b) 平文の password → sealedPassword（Windows は DPAPI、他は plain-v1）。
            match seal_queued_password(secrets, &source, &mut payload) {
                Ok(true) => {
                    report.sealed_queue += 1;
                    changed = true;
                }
                Ok(false) => {}
                Err(err) => report.failures.push(UpkeepFailure {
                    table: "pending_changes",
                    id,
                    error: err.to_string(),
                }),
            }
            // (c) plain-v1 の sealedPassword を封印し直す（protector があるときだけ）。
            match reseal_queued_password(secrets, &source, &mut payload) {
                Ok(true) => {
                    report.resealed_queue += 1;
                    changed = true;
                }
                Ok(false) => {}
                Err(err) => report.failures.push(UpkeepFailure {
                    table: "pending_changes",
                    id,
                    error: err.to_string(),
                }),
            }
        } else if HISTORY_STATES.contains(&state.as_str())
            && strip_queued_password(&source, &mut payload)
        {
            // (d) 履歴から password と sealedPassword の両方を外す。
            report.stripped_history += 1;
            changed = true;
        }
        if changed {
            let text = serde_json::to_string(&payload)
                .map_err(|err| BantoError::Other(format!("payload のシリアライズ: {err}")))?;
            sqlx::query("UPDATE pending_changes SET payload = ? WHERE id = ?")
                .bind(text)
                .bind(id)
                .execute(&mut *tx)
                .await
                .map_err(storage)?;
        }
    }

    // (e) 平文を空きページへ追い出しうる書き込みと同じトランザクションで印を立てる。
    if report.rewrote() {
        crate::pending_changes::set_vacuum_pending_in(&mut tx).await?;
    }
    if fail_before_commit {
        drop(tx);
        return Err(BantoError::Other(
            "テストで注入した失敗（後処理の commit の前）".to_string(),
        ));
    }
    tx.commit().await.map_err(storage)?;
    Ok(report)
}

/// 工程 2（§6.5 の 2）: 清掃待ちの印（Hub の `secrets.vacuum_pending`、または banto-tags の
/// `plain-v1` の行の消去・書き換えの記録 `plc_connection_secrets_cleanup`、migration 0026）が
/// あるときだけ、`PRAGMA wal_checkpoint(TRUNCATE)` →
/// `VACUUM` → `PRAGMA wal_checkpoint(TRUNCATE)` を 1 本の接続で行い、済んだら印を消す。
/// 印が無ければ何もせず `Ok(false)`。後ろの checkpoint が終わり切らない（他の接続が読んで
/// いる）・どれかが失敗したら印を残して `Err`（次の起動でやり直す）。
pub async fn vacuum_if_pending(pool: &SqlitePool) -> Result<bool, BantoError> {
    let storage = banto_storage::storage_error;
    let mut conn = pool.acquire().await.map_err(storage)?;
    let mark: Option<String> = sqlx::query_scalar("SELECT value FROM settings WHERE key = ?")
        .bind(VACUUM_PENDING_KEY)
        .fetch_optional(&mut *conn)
        .await
        .map_err(storage)?;
    // banto-tags の記録（migration 0026 のトリガ）: `plain-v1` の秘密の行を消した・書き換えた
    // 書き込みが、同じトランザクションで数えたもの（操作者のパスワードの入れ替え・消去・
    // プロトコルの変更・接続の削除・整合の確認が消す参照の無い行。#778 のレビュー P2）。
    let tags_cleanup = banto_tags::connection_secret::plain_secret_cleanup_pending(&mut *conn)
        .await
        .map_err(storage)?;
    if mark.as_deref() != Some("true") && tags_cleanup == 0 {
        return Ok(false);
    }
    // 前の checkpoint: WAL に残った古いフレーム（平文を含みうる）を本体へ移して切り詰める。
    // 終わり切らなくても、後ろの checkpoint が終われば WAL は空になる。
    wal_checkpoint_truncate(&mut conn).await?;
    sqlx::query("VACUUM")
        .execute(&mut *conn)
        .await
        .map_err(storage)?;
    // 後ろの checkpoint: WAL の DB では VACUUM の結果は WAL に入り、本体の空きページの古い
    // 中身は checkpoint まで残る。ここで本体を書き換えて切り詰める。
    if !wal_checkpoint_truncate(&mut conn).await? {
        return Err(BantoError::Other(
            "VACUUM の後の WAL の切り詰めが終わりませんでした（他の接続が DB を読んでいます）"
                .to_string(),
        ));
    }
    sqlx::query("DELETE FROM settings WHERE key = ?")
        .bind(VACUUM_PENDING_KEY)
        .execute(&mut *conn)
        .await
        .map_err(storage)?;
    // 読んだときの値のままのときだけ 0 に戻す（間に数えた書き込みは次の起動で清掃する）。
    if tags_cleanup > 0 {
        banto_tags::connection_secret::clear_plain_secret_cleanup(&mut *conn, tags_cleanup)
            .await
            .map_err(storage)?;
    }
    Ok(true)
}

/// `PRAGMA wal_checkpoint(TRUNCATE)`。終わり切ったら `true`（WAL でない DB も `true`）。
async fn wal_checkpoint_truncate(
    conn: &mut sqlx::pool::PoolConnection<sqlx::Sqlite>,
) -> Result<bool, BantoError> {
    let (busy, _log, _checkpointed): (i64, i64, i64) =
        sqlx::query_as("PRAGMA wal_checkpoint(TRUNCATE)")
            .fetch_one(&mut **conn)
            .await
            .map_err(banto_storage::storage_error)?;
    Ok(busy == 0)
}

/// 状態の画面の「移行待ちの秘密値が平文で残っています」（§6.5 の最後・§8 の 9）の件数:
/// protector があるのに（Windows）`plain-v1` のままの秘密の行と、未適用キューの平文の
/// `password` か `plain-v1` の `sealedPassword` を持つ行。protector が無い（Windows 以外）
/// なら平文は既定なので 0（そちらは「秘密値を平文で保存しています」の警告で出す）。
pub async fn plaintext_remaining(
    pool: &SqlitePool,
    secrets: &crate::secrets::ConnectionSecrets,
) -> Result<u64, BantoError> {
    use crate::secrets::SecretScheme;
    if secrets.seal_scheme() == SecretScheme::PlainV1 {
        return Ok(0);
    }
    let storage = banto_storage::storage_error;
    let rows: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM plc_connection_secrets WHERE scheme = ?")
            .bind(SecretScheme::PlainV1.as_str())
            .fetch_one(pool)
            .await
            .map_err(storage)?;
    let queue: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pending_changes \
         WHERE source IN (?, ?) AND state IN ('pending', 'applying', 'failed') \
         AND json_valid(payload) \
         AND ((json_type(payload, '$.input.password') = 'text' \
               AND json_extract(payload, '$.input.password') <> '') \
              OR json_extract(payload, '$.input.sealedPassword.scheme') = ?)",
    )
    .bind(crate::pending_changes::CONNECTION_PASSWORD_SOURCES[0])
    .bind(crate::pending_changes::CONNECTION_PASSWORD_SOURCES[1])
    .bind(SecretScheme::PlainV1.as_str())
    .fetch_one(pool)
    .await
    .map_err(storage)?;
    Ok(u64::try_from(rows + queue).unwrap_or(0))
}

/// [`secrets_upkeep`] の結果をログに出す（値・参照・blob は出さない）。
pub fn log_upkeep(report: &UpkeepReport) {
    if report.rewrote() {
        log_line(&format!(
            "banto-hub: 秘密値の後処理: 封印し直した秘密の行 {} 件、未適用キューの封印 {} 件・\
             封印し直し {} 件、履歴の行から外した値 {} 件",
            report.resealed_rows,
            report.sealed_queue,
            report.resealed_queue,
            report.stripped_history
        ));
    }
    for failure in &report.failures {
        log_err_line(&format!(
            "banto-hub: [WARN] 秘密値の後処理: {} の #{} を封印し直せませんでした（平文のまま\
             残します。次の起動でもう一度試します）: {}",
            failure.table, failure.id, failure.error
        ));
    }
    if let Some(err) = &report.rewrite_error {
        log_err_line(&format!(
            "banto-hub: [WARN] 秘密値の後処理に失敗しました（何も変えていません。次の起動で\
             もう一度試します）: {err}"
        ));
    }
    if report.vacuumed {
        log_line("banto-hub: 秘密値の後処理: DB の空きページと WAL を清掃しました（VACUUM）");
    }
    if let Some(err) = &report.vacuum_error {
        log_err_line(&format!(
            "banto-hub: [WARN] DB の清掃（VACUUM）に失敗しました。次の起動でもう一度試します: {err}"
        ));
    }
}

#[cfg(test)]
mod tests;
