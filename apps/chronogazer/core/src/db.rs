//! Database bootstrap for the chronogazer app (spec §12): connect, refuse a
//! database in the pre-2026-10-02 format, then apply this app's own migrations
//! (`migrations-sqlite/`), then `banto_tags::migrate` (the PLC connection/
//! collection group/tag registry tables), then `banto_collect::migrate`
//! (`collect_events`, #383 段階2b) against the SAME pool - ChronoGazer shares
//! one SQLite database across the app's own tables (settings/users/
//! audit_log) and every I-series crate's tables (plan.md §5: "single
//! app-data file"), so this is the one place that bootstraps the whole
//! schema.
//!
//! ## スキーマの出所（DB スキーマの整理、2026-10-02 オーナー決定）
//!
//! この app 自身のテーブルは banto の admin-template と**同じ形**にする。
//! `migrations-sqlite/` の `0002`〜`0008`・`0010`・`0012`・`0013` の 9 本は、当面
//! `banto apps/admin-template/core/migrations-sqlite/<同じファイル名>`
//! を **byte 等価**でコピーしたもの（`0002`〜`0007` は v2.1.1、`0008` は
//! v6.5.0 = 監査の保留の `audit_log.pending_id`、banto ADR-0019。ChronoGazer は
//! まだ保留を有効にしていないが、banto-hub と同じ集合に揃えておく - #437。
//! `0012` は v7.4.0 = users の楽観ロック `users.version`（banto#651）。
//! `0013` は v7.7.0 = ユーザーごとの権限の表 `user_permissions`（banto#697。上流の
//! `UsersService` の一覧・更新がこの表を読むので写す）。上流の
//! `0009`・`0011`（items の索引・version）はこの app が items を使わないので写さない。
//! `0010` = 監査の記録単位の生成列（上流の監査一覧が recordResource/recordId で絞れる列。
//! 無いとその指定で SQL エラーになるので写す）。
//! banto 側には手を入れない。番号の飛び - `0001_items`・`0006_attachments` が
//! 無い - は上流の番号をそのまま残しているため）。`0008` は `0101`・`0102` を
//! 適用済みの既存 DB には「若い番号が後から来る」形になるが、`sqlx` 0.9 の
//! `Migrator::run` は適用済みに無い版を流すだけで順序の逆転を拒まない（テスト
//! `an_existing_db_without_0008_gets_it_on_the_next_startup`）。ファイルの中身を書き換えないこと: 上流と食い違うと「banto に寄せる」
//! 土台にならず、`sqlx` の checksum も変わる。上流を上げるときは同じ名前で
//! コピーし直す。
//!
//! **この app 固有の migration は `0101_*` から振る**（#393 の
//! `0101_display_groups.sql` = 表示グループが最初。banto-hub 固有のテーブルと
//! 同じ決まり - plan.md §5 の 2026-10-02 の決定）。上流が 0008 以降を足した
//! ときに同じ番号を取り合わないため。こちらは上流のコピーではないので、
//! 必要なら新しい番号で足す（既存のファイルは書き換えない）。
//!
//! ## なぜ `sqlx::migrate!` に戻せたか
//!
//! 以前はこの app の分を手書きの冪等 DDL で作っていた。`sqlx` 0.8 の
//! migration 記録テーブル（`_sqlx_migrations`）は DB に 1 つで名前を変えられず、
//! 同じ pool で `banto_tags::migrate` の `sqlx::migrate!` と番号が衝突した
//! （`MigrateError::VersionMismatch`、docs/archive/r1a-readme-gaps.md）ため。
//! `sqlx` 0.9 には `Migrator::dangerous_set_table_name` があるので、記録
//! テーブルを migrator ごとに分けて衝突を無くした:
//!
//! - この app: [`MIGRATIONS_TABLE`]（既定の名前 `_sqlx_migrations`）
//! - `banto_tags::migrate`: `_sqlx_migrations_banto_tags`
//! - `banto_collect::migrate`: 記録テーブルを使わない（冪等 DDL のまま）
//!
//! この app の分が既定の名前なのは、banto v8 のリストアの検査（`banto_admin_services::backup`
//! の `check_migration_history`）が候補の履歴を固定の名前 `_sqlx_migrations` から読むため
//! （banto v8 への追従、docs/banto-v8-upgrade-plan.md §2。v8 より前は
//! [`PRE_V8_MIGRATIONS_TABLE`] に記録していた）。`banto_tags` は別の表なので、リストアの
//! 検査には app の分だけが見える（[`sqlite_restore_manifest`] も app の分だけ）。
//!
//! `dangerous_` が警告しているのは「既存 DB の記録を見失い、適用済みの
//! migration を流し直そうとする」こと。**既存の DB は壊してよい**（アルファ版。
//! 2026-10-02 オーナー決定。ChronoGazer は未リリースなので v8 への追従でも移行しない、
//! 2026-11-25 オーナー決定）ので受け入れた。その代わり、古い形式の DB は
//! 黙って流さずに起動を拒否する（次節）。
//!
//! ## 旧形式の DB の拒否
//!
//! [`init_db`] は接続直後・migrate の前に、次の DB を旧形式とみなし、
//! [`InitDbError::Legacy`] を返す（自動移行はしない）:
//!
//! - banto v8 より前（[`LegacyKind::BeforeBantoV8`]）: v8 より前の記録の表
//!   [`PRE_V8_MIGRATIONS_TABLE`] がある、または記録の表の版 3 に banto v7 の
//!   `0003_users.sql` の checksum がある;
//! - 2026-10-02 のスキーマ整理より前（[`LegacyKind::BeforeSchemaReset`]）: 記録の表が
//!   無いのに `users` か `settings` がある、または記録の表に `banto_tags` の版 1 がある
//!   （整理の前は `banto_tags` が共有の `_sqlx_migrations` に記録していた）。
//!
//! そのまま流すと `CREATE TABLE users` が「既にある」で落ち、何が起きたか
//! 分からないため。新規の空ファイルと新しい形式の DB は通る。呼び出し側
//! はメッセージを stderr に出して終了する（`bin/banto-serve.rs`）。`src-tauri`
//! はリリースビルドで stderr が見えないので、加えてメインの窓の代わりにエラー
//! 専用の窓を開き、閉じると終了する。作り直しの手順は `apps/chronogazer/README.md` 参照。
//!
//! リストアの予約は `init_db` より前に適用される（`crate::engine` の起動の順序の 2.）ので、
//! `init_db` だけで断ると、旧形式の DB の上に予約が先に当たる・旧形式の予約が黙って消される
//! （banto は検査に通らない予約を起動時に消す）。そこで起動は予約の適用の**前に**
//! [`refuse_legacy_profile`] で、今の DB と予約のファイルを読み取り専用で確かめ、どちらかが
//! 旧形式なら何も変えずに止まる（docs/banto-v8-upgrade-plan.md §5 の 2）。

use banto_core::BantoError;
use sqlx::SqlitePool;
use std::fmt;
use std::path::{Path, PathBuf};

/// The one SQLite pool type every service in this crate is built over,
/// re-exported so downstream crates (notably `src-tauri`, whose invariant is
/// to add NO new dependencies of its own) can name it - e.g. to hold the
/// pool in their own app state so it can be `close()`d on exit - without
/// taking a direct `sqlx` dependency. Same precedent (and same wording) as
/// `relay_wright_core::db::DbPool`.
pub type DbPool = SqlitePool;

/// banto のサービス（`banto_admin_services` の users/settings/audit/backup。
/// I2a で自前のコピーから置き換えた）が受け取る、バックエンドを問わない
/// 接続ハンドル。ChronoGazer は SQLite 固定なので、いつも
/// `Db::Sqlite(pool.clone())` で [`DbPool`] から組む（`sqlx` の pool は
/// `Arc` なので、同じ pool を共有する）。[`DbPool`] と同じ理由で re-export
/// する（`src-tauri` は `banto-storage` に直接依存しない）。
pub use banto_storage::Db;

/// この app の migration 記録テーブルの名前（モジュール doc「なぜ
/// `sqlx::migrate!` に戻せたか」）。banto v8 のリストアの検査が読む既定の名前。
pub const MIGRATIONS_TABLE: &str = "_sqlx_migrations";

/// banto v8 への追従より前に、この app の記録に使っていた表の名前。これがある DB は
/// v8 より前の版のもの（[`LegacyKind::BeforeBantoV8`]）。
pub const PRE_V8_MIGRATIONS_TABLE: &str = "_sqlx_migrations_chronogazer";

/// `banto_tags` の最初の migration の版。2026-10-02 のスキーマ整理より前の DB は、これを
/// 共有の `_sqlx_migrations` に記録していた（この app の migration に版 1 は無い）。
const PRE_RESET_BANTO_TAGS_FIRST_VERSION: i64 = 1;

/// 旧形式（2026-10-02 のスキーマ整理より前）の DB を開こうとした（モジュール doc「旧形式の DB の
/// 拒否」）。`Display` がそのまま利用者向けの説明になる。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyDatabase {
    /// 拒否した DB ファイルのパス。
    pub path: PathBuf,
    /// どの旧い形式か（利用者向けの説明を分ける）。
    pub kind: LegacyKind,
}

/// [`LegacyDatabase`] の種類。どちらも自動の移行は無く、DB を作り直す。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyKind {
    /// 2026-10-02 のスキーマ整理より前（この app の記録テーブルが無いのに
    /// `users` か `settings` がある）。
    BeforeSchemaReset,
    /// banto v8.0.0 より前の版で作った DB（ADR-0034 §9・
    /// docs/banto-v8-upgrade-plan.md §2 の 1）。v8 より前の記録の表
    /// （[`PRE_V8_MIGRATIONS_TABLE`]）がある、または記録の表に banto v7 の
    /// `0003_users.sql` の checksum（[`PRE_V8_USERS_CHECKSUM`]）が残っている。
    BeforeBantoV8,
}

/// banto v8.0.0 が書き換えた migration のうち、版の順で最初のもの（`0003_users.sql`）。
const PRE_V8_USERS_MIGRATION_VERSION: i64 = 3;

/// banto v7.x（v7.9.0〜v7.11.0 で同じ本文）の `0003_users.sql` の checksum（sqlx: ファイルの
/// SHA-384、小文字の 16 進）。上流 `apps/admin-template/core/src/db.rs` の
/// `V7_USERS_CHECKSUM_SQLITE` と同じ値。この app の記録テーブルの版 3 にこれが残っている DB は、
/// v8 より前の版で作った DB と確定できる（それ以外の食い違いは「判別できない」で、
/// sqlx の `VersionMismatch` のまま止まる。DB は変わらない）。
pub const PRE_V8_USERS_CHECKSUM: &str =
    "25f4a5973321cc81959ce2f2e211df45acdfa0d0e9db30db155dd3ac62a36b00b82edbf393ac250b4867e2a30e1b1240";

impl fmt::Display for LegacyDatabase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            LegacyKind::BeforeSchemaReset => {
                write!(
                    f,
                    "この DB は旧形式です。{} を削除または退避して起動し直してください\
                     （アルファ版のため自動移行はありません）",
                    self.path.display()
                )
            }
            LegacyKind::BeforeBantoV8 => write!(
                f,
                "この DB は v8.0.0 より前の版のものです。読めません。DB を作り直してください。\
                 {} を削除または退避して起動し直してください（データの移行はありません）",
                self.path.display()
            ),
        }
    }
}

impl std::error::Error for LegacyDatabase {}

/// [`init_db`] の失敗。旧形式の DB（[`InitDbError::Legacy`]）と、それ以外の
/// 接続・migration の失敗（[`InitDbError::Storage`]）を呼び出し側で分けられる
/// ようにする。
#[derive(Debug)]
pub enum InitDbError {
    /// 旧形式の DB。起動を拒否する。
    Legacy(LegacyDatabase),
    /// 接続または migration の失敗。
    Storage(BantoError),
}

impl fmt::Display for InitDbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Legacy(legacy) => legacy.fmt(f),
            Self::Storage(err) => err.fmt(f),
        }
    }
}

impl std::error::Error for InitDbError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Legacy(legacy) => Some(legacy),
            Self::Storage(err) => Some(err),
        }
    }
}

impl From<BantoError> for InitDbError {
    fn from(err: BantoError) -> Self {
        Self::Storage(err)
    }
}

/// `BantoError` で受ける呼び出し側のため。旧形式は説明文ごと
/// `BantoError::Other` に入れる。
impl From<InitDbError> for BantoError {
    fn from(err: InitDbError) -> Self {
        match err {
            InitDbError::Legacy(legacy) => BantoError::Other(legacy.to_string()),
            InitDbError::Storage(err) => err,
        }
    }
}

/// Connect to the SQLite database at `path`, refuse it if it is in the
/// pre-2026-10-02 format, and apply the full schema (this app's own, then
/// `banto_tags`'s, then `banto_collect`'s). Used by the `src-tauri` adapter
/// with a path under the app's data directory.
pub async fn init_db(path: impl AsRef<Path>) -> Result<SqlitePool, InitDbError> {
    let path = path.as_ref();
    let pool = banto_storage::connect_sqlite(path).await?;
    init_db_on(pool, banto_tags::upgrade::Place::Path(path)).await
}

/// [`init_db`] の、握った組（[`banto_safe_root::sqlite::PinnedSqlite`]）で開く版（#623 PR2b。
/// サービスが握ったルートの直下の設定 DB）。開き方は `banto_storage::connect_sqlite` と同じ
/// 設定に、握っているときは接続ごとの `SQLITE_FCNTL_PERSIST_WAL` と `journal_size_limit` を
/// 足したもの（`PinnedSqlite::connect`）。手順と失敗は [`init_db`] と同じ。`pinned` は返った
/// pool を閉じ終えるまで生かすこと（呼ぶ側の責任）。`Err` のときは開いた接続をすべて閉じ
/// 終えてから返す（`connect` の失敗は pool を返さず、その probe の接続は
/// `PinnedSqlite::connect` が閉じる。[`init_db`] と同じ後半の失敗は pool を閉じてから返る）
/// ので、`pinned` はすぐ離してよい。
///
/// `dir` は `pinned` を握ったフォルダ（サービスのルート）、`expected` はその運用モードの形
/// （`DbPin::InRoot` の `expected`）。0025 の前の残り物の掃除と作りかけのバックアップは、
/// パスで開かずに `dir` のハンドル基準で扱う（`banto_tags::upgrade::Place::Managed`、
/// #623 PR3b。`expected` が `Some` なら `_checked` 版）。
pub async fn init_db_pinned(
    pinned: &banto_safe_root::sqlite::PinnedSqlite,
    dir: &banto_safe_root::ManagedDir,
    expected: Option<&banto_safe_root::DirAccess>,
) -> Result<SqlitePool, InitDbError> {
    let db_name = pinned
        .path()
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            BantoError::Storage(format!(
                "設定 DB のファイル名を読めません: {}",
                pinned.path().display()
            ))
        })?;
    let pool = pinned
        .connect()
        .await
        .map_err(banto_storage::storage_error)?;
    init_db_on(
        pool,
        banto_tags::upgrade::Place::managed(dir, db_name, expected),
    )
    .await
}

/// [`init_db`]・[`init_db_pinned`] の、pool を開いた後の手順（旧形式の拒否 → migration）。
async fn init_db_on(
    pool: SqlitePool,
    place: banto_tags::upgrade::Place<'_>,
) -> Result<SqlitePool, InitDbError> {
    // 失敗して戻る経路では必ず pool を閉じてから返す（#392 の起動時の対処:
    // 戻った直後に DB のファイルの名前を変えることがあり、Windows では開いた
    // ままのハンドルが名前の変更を拒む）。
    let legacy = match is_legacy_schema(&pool).await {
        Ok(legacy) => legacy,
        Err(err) => {
            pool.close().await;
            return Err(err.into());
        }
    };
    if let Some(kind) = legacy {
        pool.close().await;
        return Err(InitDbError::Legacy(LegacyDatabase {
            path: place.db_path(),
            kind,
        }));
    }
    // R2b-1d（docs/hub/banto-hub-connection-settings-design.md §6.7）: 戻せない migration 0025
    // （`banto_tags::migrate` に入っている）の前に、残り物の掃除とバックアップ。
    let prepared = match run_app_migrations(&pool).await {
        Ok(()) => prepare_0025(&pool, place).await,
        Err(err) => Err(err),
    };
    if let Err(err) = prepared {
        pool.close().await;
        return Err(err.into());
    }
    if let Err(err) = run_shared_migrations(&pool).await {
        pool.close().await;
        return Err(err.into());
    }
    Ok(pool)
}

/// 0025 の前処理（設計 §6.7）。ChronoGazer の行は `modbus-tcp` / `slmp` / `hub` だけで 0025 が
/// 断れず、秘密値も無いので、Hub のような予行と preflight は要らない（`banto_tags::migrate`
/// が失敗しても sqlx がトランザクションを巻き戻す）。
///
/// 1. 毎回: 前回の起動の作りかけのバックアップ（パスワードの平文を含みうる）を消す。
/// 2. 0025 が未適用で中身のある DB だけ、パスワードを除いたバックアップ
///    （`<DB>.before-0025-<UTC>.bak`）を作り、パスを標準エラーに出す。新規の DB・適用済みの
///    DB・空のままの DB には作らない。
///
/// 失敗は [`InitDbError::Storage`] に包む（他の DB 初期化の失敗と同じ経路:
/// `EngineStartError::Database` → `startup_recovery::classify` の
/// `DatabaseUnopenable`）。
///
/// 置き場（`banto_tags::upgrade::Place`）はパスか、サービスの握ったルート（#623 PR3b）。
async fn prepare_0025(
    pool: &SqlitePool,
    place: banto_tags::upgrade::Place<'_>,
) -> Result<(), BantoError> {
    use banto_tags::upgrade;
    let to_err = |err: upgrade::UpgradeError| BantoError::Storage(err.to_string());
    for leftover in upgrade::remove_leftovers(place).map_err(to_err)? {
        eprintln!(
            "banto: 前回の起動で残った移行の一時ファイルを消しました: {}",
            leftover.display()
        );
    }
    if upgrade::pending_0025(pool).await.map_err(to_err)?
        && upgrade::has_rows_to_protect(pool).await.map_err(to_err)?
    {
        let backup = upgrade::backup_before_0025(pool, place)
            .await
            .map_err(to_err)?;
        eprintln!(
            "banto: 接続設定の移行（migration 0025）の前に、DB のバックアップを作りました\
             （postgres 接続のパスワードは含みません。戻すときは入れ直してください）: {}",
            backup.display()
        );
    }
    Ok(())
}

/// Same as [`init_db`] but against a private in-memory database. Used by
/// tests so each test gets an isolated, fully-migrated database (always
/// empty, so the old-format check is not needed).
pub async fn init_db_memory() -> Result<SqlitePool, BantoError> {
    let pool = banto_storage::connect_sqlite_memory().await?;
    run_migrations(&pool).await?;
    Ok(pool)
}

/// Same as [`init_db_memory`], `pub(crate)` for `rest.rs`'s test module -
/// kept as a separate name (rather than just reusing `init_db_memory`
/// directly) since several call sites already spell it this way.
#[cfg(test)]
pub(crate) async fn migrate_memory() -> Result<SqlitePool, BantoError> {
    let pool = banto_storage::connect_sqlite_memory().await?;
    run_migrations(&pool).await?;
    Ok(pool)
}

/// 旧形式の DB か（モジュール doc「旧形式の DB の拒否」）。読むだけ。
async fn is_legacy_schema(pool: &SqlitePool) -> Result<Option<LegacyKind>, BantoError> {
    let mut conn = pool.acquire().await.map_err(banto_storage::storage_error)?;
    legacy_kind_of(&mut conn).await
}

/// [`is_legacy_schema`] の本体（読み取り専用の接続でも使う。[`refuse_legacy_profile`]）。
async fn legacy_kind_of(
    conn: &mut sqlx::SqliteConnection,
) -> Result<Option<LegacyKind>, BantoError> {
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name IN (?, ?, 'users', 'settings')",
    )
    .bind(MIGRATIONS_TABLE)
    .bind(PRE_V8_MIGRATIONS_TABLE)
    .fetch_all(&mut *conn)
    .await
    .map_err(banto_storage::storage_error)?;
    if tables.iter().any(|name| name == PRE_V8_MIGRATIONS_TABLE) {
        return Ok(Some(LegacyKind::BeforeBantoV8));
    }
    if !tables.iter().any(|name| name == MIGRATIONS_TABLE) {
        return Ok((!tables.is_empty()).then_some(LegacyKind::BeforeSchemaReset));
    }
    // 表の名前は定数なので、`format!` に外の値は入らない。
    let rows: Vec<(i64, Vec<u8>)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT version, checksum FROM {MIGRATIONS_TABLE} WHERE version IN (?, ?)"
    )))
    .bind(PRE_RESET_BANTO_TAGS_FIRST_VERSION)
    .bind(PRE_V8_USERS_MIGRATION_VERSION)
    .fetch_all(&mut *conn)
    .await
    .map_err(banto_storage::storage_error)?;
    if rows
        .iter()
        .any(|(version, _)| *version == PRE_RESET_BANTO_TAGS_FIRST_VERSION)
    {
        return Ok(Some(LegacyKind::BeforeSchemaReset));
    }
    // banto v8（ADR-0034 §9）: 新しい migration を当てる前に、v8 より前の版の DB を
    // 名前の付いた理由で断る（sqlx の `VersionMismatch` の文言では分からないため）。
    let v7_users = rows.iter().any(|(version, checksum)| {
        *version == PRE_V8_USERS_MIGRATION_VERSION && hex_of(checksum) == PRE_V8_USERS_CHECKSUM
    });
    Ok(v7_users.then_some(LegacyKind::BeforeBantoV8))
}

fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// 起動の最初（リストアの予約の適用と `init_db` の前）に、今の DB と予約のファイルが旧形式で
/// ないかを確かめる（モジュール doc「旧形式の DB の拒否」の最後の段落。
/// docs/banto-v8-upgrade-plan.md §5 の 2）。どちらかが旧形式なら、何も変えずに
/// [`InitDbError::Legacy`]（`path` は旧形式だったファイル）を返す。
///
/// ファイルは `immutable` の読み取り専用で開く（書き込み・ロック・`-wal`・`-shm` を作らない。
/// バイトは変わらない）。そのため `-wal` にだけある変更は見えない: そこで見逃した今の DB は、
/// この後の [`init_db`] の判定が断る（予約は適用された後になる。v7 の DB は閉じるときに
/// `-wal` を本体へ畳むので、実際に起きるのは前の版が落ちたままの DB だけ）。
/// 無いファイルは飛ばす。開けない・読めないファイルも飛ばす（旧形式とは言えない。壊れた
/// DB は今までの経路 - 予約は banto の検査、今の DB は `init_db` - が扱う）。
pub async fn refuse_legacy_profile(db_path: &Path) -> Result<(), InitDbError> {
    let pending = crate::backup::BackupService::pending_restore_file(db_path).ok();
    for path in [Some(db_path.to_path_buf()), pending].into_iter().flatten() {
        if let Some(kind) = legacy_kind_of_file(&path).await {
            return Err(InitDbError::Legacy(LegacyDatabase { path, kind }));
        }
    }
    Ok(())
}

/// `path` の DB を変えずに開いて [`legacy_kind_of`]。無い・開けない・読めないなら `None`。
async fn legacy_kind_of_file(path: &Path) -> Option<LegacyKind> {
    use sqlx::sqlite::SqliteConnectOptions;
    use sqlx::{ConnectOptions, Connection};
    if !tokio::fs::try_exists(path).await.unwrap_or(false) {
        return None;
    }
    let mut conn = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(false)
        .read_only(true)
        .immutable(true)
        .connect()
        .await
        .ok()?;
    let kind = legacy_kind_of(&mut conn).await.ok().flatten();
    let _ = conn.close().await;
    kind
}

async fn run_migrations(pool: &SqlitePool) -> Result<(), BantoError> {
    run_app_migrations(pool).await?;
    run_shared_migrations(pool).await
}

/// この app 自身の migrator（`banto_tags::migrate` の前。`init_db` が間に 0025 の前処理を挟む）。
/// 起動時の検査（banto v8、ADR-0034 §2・§4・§5・§6・§9。上流の `startup::check` の
/// デスクトップの形に当たる）。migration の後に呼ぶ。満たさない DB は名前と対処を書いた
/// エラーで起動を止める（既定で埋めない・直さない）:
///
/// - 組み込みの `admin` の行がちょうど 1 つ、予約名（`admin` と
///   [`crate::local_grant::reserved_usernames`]）と同じ名前のアカウントが無い;
/// - 認証モード（`auth.disabled`・`auth.disabled_role`）を読める;
/// - ログイン必須なら、ログインできる管理者のアカウントがある。
///
/// 返すのは組み込みの行の id（`AuthState::with_builtin_admin_id` と、デスクトップの
/// コマンドの監査の主体に渡す）。
pub async fn startup_check(db: &Db) -> Result<i64, BantoError> {
    let settings = crate::settings::SettingsService::new(db.clone());
    let users = crate::users::UsersService::new(db.clone())
        .with_reserved_usernames(crate::local_grant::reserved_usernames());
    let found = users.check_startup().await?;
    let auth = settings.auth_config().await?;
    if !auth.disabled && !users.has_admin().await? {
        return Err(BantoError::Other(
            "ログイン必須の設定なのに、ログインできる管理者のアカウントがありません。\
             この DB は直接書き換えられた可能性があります。バックアップから戻すか、DB を作り直してください"
                .to_string(),
        ));
    }
    Ok(found.builtin_admin_id)
}

/// banto v8.0.0 の基準の最後の migration（`0014_ui_settings`、ADR-0034 §9）。リストアの
/// 候補はここまでを全部当ててある必要がある（この app の 0101 以降は後の起動が当てる）。
pub const BASELINE_MIGRATION_VERSION: i64 = 14;

/// リストアの検査に渡す、この app の migration の一覧と起動時の検査（banto v8、ADR-0034 §9）。
/// `BackupService::with_migrations` と `apply_pending_restore_at_startup` に渡す。
///
/// banto は候補の履歴を `_sqlx_migrations` から読む（この app の記録の表
/// [`MIGRATIONS_TABLE`]。`banto_tags` の分は別の表なので一覧に入れない）。履歴が合った
/// 候補には、起動と同じ検査（[`startup_check`]）を写しの上で掛ける
/// （`CandidateCheck`。上流 admin-template の `startup::sqlite_restore_manifest` と同じ形）:
/// 置き換えた後で起動できない候補を、置き換える前に断る。
pub fn sqlite_restore_manifest() -> crate::backup::MigrationManifest {
    sqlite_migration_manifest()
        .with_candidate_check(crate::backup::CandidateCheck::new(check_restore_candidate))
}

/// [`sqlite_restore_manifest`] の、履歴の一覧だけ（起動時の検査なし）。
fn sqlite_migration_manifest() -> crate::backup::MigrationManifest {
    crate::backup::MigrationManifest::new(
        sqlx::migrate!("./migrations-sqlite")
            .iter()
            .filter(|migration| !migration.migration_type.is_down_migration())
            .map(|migration| (migration.version, migration.checksum.to_vec())),
        BASELINE_MIGRATION_VERSION,
    )
}

/// リストアの候補を、次の起動が見る形で確かめる（上流 admin-template の
/// `startup::check_restore_candidate` と同じ方針）: 候補の**写し**に起動と同じ準備
/// （migration。候補は基準より後の migration を欠いていてよい）を当ててから
/// [`startup_check`]。候補そのものは書き込みで開かず、写しは結果に関わらず消す。
///
/// - `Rejected`: [`startup_check`] が断った（候補の中身の問題。組み込みの行・予約名・
///   認証モード・管理者）。
/// - `Err`: 確かめきれなかった（写しを作れない・開けない・migration が通らない・読めない）。
///   候補の問題とは限らないので判定にしない（予約は残り、今回は適用しない）。
///
/// 0025 の前処理（[`prepare_0025`]）は当てない: 写しの隣にバックアップを作るだけで、
/// 起動できるかの判定は変わらない。
async fn check_restore_candidate(
    candidate: PathBuf,
) -> Result<crate::backup::CandidateVerdict, BantoError> {
    let copy = restore_check_copy_path(&candidate);
    let result = match tokio::fs::copy(&candidate, &copy).await {
        Ok(_) => check_restore_copy(&copy).await,
        Err(err) => Err(BantoError::Other(format!(
            "リストア候補の検査用コピーを作れませんでした: {err}"
        ))),
    };
    for path in [
        copy.clone(),
        sidecar_of(&copy, "-wal"),
        sidecar_of(&copy, "-shm"),
    ] {
        let _ = tokio::fs::remove_file(path).await;
    }
    result
}

async fn check_restore_copy(copy: &Path) -> Result<crate::backup::CandidateVerdict, BantoError> {
    let pool = banto_storage::connect_sqlite(copy).await?;
    let result = match run_migrations(&pool).await {
        Ok(()) => match startup_check(&Db::Sqlite(pool.clone())).await {
            Ok(_) => Ok(crate::backup::CandidateVerdict::Accepted),
            // 読めなかった（判定ではない）。
            Err(err @ BantoError::Storage(_)) => Err(err),
            Err(err) => Ok(crate::backup::CandidateVerdict::Rejected(err.to_string())),
        },
        Err(err) => Err(BantoError::Other(format!(
            "リストア候補の検査用コピーで、起動の準備（migration）を終えられませんでした: {err}"
        ))),
    };
    // 消す前に閉じる（Windows は開いた SQLite のファイルを消させない）。
    pool.close().await;
    result
}

fn restore_check_copy_path(candidate: &Path) -> PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    let name = format!(".restore-check-{}-{nonce}.tmp", std::process::id());
    match candidate.parent() {
        Some(dir) => dir.join(name),
        None => PathBuf::from(name),
    }
}

fn sidecar_of(path: &Path, suffix: &str) -> PathBuf {
    PathBuf::from(format!("{}{suffix}", path.display()))
}

async fn run_app_migrations(pool: &SqlitePool) -> Result<(), BantoError> {
    // この app 自身の分（admin-template のコピー）。記録テーブルを分ける理由は
    // モジュール doc 参照。
    let mut migrator = sqlx::migrate!("./migrations-sqlite");
    migrator.dangerous_set_table_name(MIGRATIONS_TABLE);
    migrator
        .run(pool)
        .await
        .map_err(|err| BantoError::Storage(err.to_string()))?;
    Ok(())
}

/// `banto_tags::migrate`（0025 を含む）→ `banto_collect::migrate`。
async fn run_shared_migrations(pool: &SqlitePool) -> Result<(), BantoError> {
    // I1 (docs/plan.md): banto-tags owns its own migrations/ directory and
    // is applied here, right after this app's own schema, so every caller
    // of init_db/init_db_memory gets the full schema in one call. It records
    // into its own `_sqlx_migrations_banto_tags`, so it no longer collides
    // with this app's migrator.
    banto_tags::migrate(pool).await?;
    // I3b / #383 段階2b（R1-C）: `banto-collect` の `collect_events` テーブル。
    // `crate::collect::CollectorService` が持つ `EventSink` の**永続側**の
    // 出力先で、live broadcast と対になっている。`EventSink::emit` は
    // insert の失敗を握り潰す（収集を止めないため）ので、ここで作っておかないと
    // イベントは黙って消える - `banto_collect::migrate` の doc が
    // 「consuming app が `banto_tags::migrate` の後に 1 回呼ぶ」と定めている
    // のがまさにこの位置で、`apps/banto-hub/core/src/db.rs` も同じ順序で
    // 呼んでいる。`sqlx::migrate!` ではなく冪等 DDL なので記録テーブルは無い。
    banto_collect::migrate(pool)
        .await
        .map_err(|err| BantoError::Other(err.to_string()))?;
    Ok(())
}

/// 試験用: banto v7 の版の ChronoGazer が作った DB（v8 への追従の前の形）を `path` に作る
/// （docs/banto-v8-upgrade-plan.md §5 の 2 の「v7 の実プロファイル」）。記録の表は
/// [`PRE_V8_MIGRATIONS_TABLE`]、`0003_users.sql`・`0005_audit_log.sql` は v7 の本文
/// （`tests/fixtures/banto-v7/`。main の写し）、v8 が足した 0014・0015 は無い。
/// 作業用の migration の置き場は `path` の隣に作る。WAL を畳んで閉じる。
#[cfg(test)]
pub(crate) async fn write_banto_v7_db(path: &Path) {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let set = path.with_extension("v7-migrations");
    std::fs::create_dir_all(&set).unwrap();
    for entry in std::fs::read_dir(manifest.join("migrations-sqlite")).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with("0014_") || name.starts_with("0015_") {
            continue;
        }
        let source = if name.starts_with("0003_") || name.starts_with("0005_") {
            manifest.join("tests/fixtures/banto-v7").join(&name)
        } else {
            entry.path()
        };
        std::fs::copy(source, set.join(&name)).unwrap();
    }
    let pool = banto_storage::connect_sqlite(path).await.unwrap();
    let mut app = sqlx::migrate::Migrator::new(set.as_path()).await.unwrap();
    app.dangerous_set_table_name(PRE_V8_MIGRATIONS_TABLE);
    app.run(&pool).await.unwrap();
    banto_tags::migrate(&pool).await.unwrap();
    banto_collect::migrate(&pool).await.unwrap();
    sqlx::query("INSERT INTO settings (key, value) VALUES ('probe', 'v7')")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
    std::fs::remove_dir_all(&set).unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// End-to-end proof that `init_db_memory` applies BOTH this app's own
    /// schema (`settings`/`users`/`audit_log`, including the `role` column)
    /// AND `banto_tags`'s (`plc_connections`/`collection_groups`/`tags`)
    /// against the same pool - the R1-A integration point this module
    /// exists to wire up.
    #[tokio::test]
    async fn init_db_memory_applies_both_this_apps_and_banto_tags_schema() {
        let pool = init_db_memory()
            .await
            .expect("init_db_memory should succeed");
        for table in [
            "settings",
            "users",
            "audit_log",
            "plc_connections",
            "collection_groups",
            "tags",
            // #383 段階2b / R1-C: `banto_collect::migrate` の分。
            "collect_events",
            // #393: この app 固有の migration（0101_display_groups.sql）。
            "display_groups",
            "display_group_pens",
            // #532: 記録計の側のタグごとの設定（0102_recorder_tag_settings.sql）。
            "recorder_tag_settings",
            // #1161 PR4: API キー（0103_api_keys.sql）。
            "api_keys",
            // #1207・#1231 PR 8a: 自由トレンド（0109_free_trends.sql）。
            "free_trends",
            "free_trend_pens",
        ] {
            let exists: Option<String> = sqlx::query_scalar(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?",
            )
            .bind(table)
            .fetch_optional(&pool)
            .await
            .unwrap();
            assert!(exists.is_some(), "expected table '{table}' to exist");
        }

        let has_role_column: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pragma_table_info('users') WHERE name = 'role'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(has_role_column, 1, "expected users.role to exist");
    }

    /// 2 回流しても失敗しない（どの migrator も自分の記録テーブルで適用済みを
    /// 覚えている。`banto_collect` は冪等 DDL）。
    #[tokio::test]
    async fn schema_application_is_idempotent_across_two_runs_on_the_same_db() {
        let pool = banto_storage::connect_sqlite_memory().await.unwrap();
        run_migrations(&pool).await.unwrap();
        run_migrations(&pool).await.unwrap(); // second run: must not error
    }

    /// スキーマ整理（2026-10-02）: 記録テーブルは migrator ごとに分かれる。banto v8 への追従から
    /// この app の分は既定の `_sqlx_migrations`（リストアの検査が読む）、`banto_tags` は自分の表。
    /// この app の分は上流 admin-template の写し + この app 固有。
    #[tokio::test]
    async fn migrations_are_recorded_in_per_migrator_tables() {
        let pool = init_db_memory().await.unwrap();
        let tables: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name LIKE '\\_sqlx%' \
             ESCAPE '\\' ORDER BY name",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            tables,
            vec![
                MIGRATIONS_TABLE.to_string(),
                "_sqlx_migrations_banto_tags".to_string(),
            ]
        );
        let versions: Vec<i64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT version FROM {MIGRATIONS_TABLE} ORDER BY version"
        )))
        .fetch_all(&pool)
        .await
        .unwrap();
        // 2〜15 は admin-template のコピー（9・11 は除く）、101 からはこの app 固有（#393 の表示グループ、
        // #532 の記録計の側のタグごとの設定、#1161 PR4 の API キー）。
        assert_eq!(
            versions,
            vec![2, 3, 4, 5, 7, 8, 10, 12, 13, 14, 15, 101, 102, 103, 104, 105, 106, 107, 108, 109]
        );
        let has_auth_epoch: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pragma_table_info('users') WHERE name = 'auth_epoch'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(has_auth_epoch, 1);
    }

    /// 新規の空ファイルは通り、2 回目の起動（同じファイル）も migrate が
    /// 冪等で、1 回目に書いた行が残る。
    #[tokio::test]
    async fn a_new_file_passes_and_the_second_startup_is_idempotent() {
        let dir = crate::test_support::TempDir::new();
        let db_path = dir.path().join("chronogazer.sqlite3");

        let pool = init_db(&db_path).await.expect("a new file must pass");
        sqlx::query("INSERT INTO settings (key, value) VALUES ('probe', 'kept')")
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;

        let pool = init_db(&db_path)
            .await
            .expect("the new-format DB must pass on the second startup");
        let value: String = sqlx::query_scalar("SELECT value FROM settings WHERE key = 'probe'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(value, "kept");
        pool.close().await;
    }

    /// #437: banto v6.5.0 の `0008_audit_log_pending_id.sql` は、`0101`・`0102`
    /// を適用済みの既存 DB にとって「番号が若い後から来た migration」になる。
    /// `sqlx` 0.9 はそれを拒まずに流すことを、0008 を除いた集合で作った DB を
    /// 今の集合で起動し直して確かめる（banto-hub の同名のテストと同じ）。
    #[tokio::test]
    async fn an_existing_db_without_0008_gets_it_on_the_next_startup() {
        let dir = crate::test_support::TempDir::new();
        let old_set = dir.path().join("migrations-before-0008");
        std::fs::create_dir(&old_set).unwrap();
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations-sqlite");
        for entry in std::fs::read_dir(&source).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name();
            if name.to_string_lossy().starts_with("0008_") {
                continue;
            }
            std::fs::copy(entry.path(), old_set.join(&name)).unwrap();
        }

        let db_path = dir.path().join("chronogazer.sqlite3");
        let pool = banto_storage::connect_sqlite(&db_path).await.unwrap();
        let mut old = sqlx::migrate::Migrator::new(old_set.as_path())
            .await
            .unwrap();
        old.dangerous_set_table_name(MIGRATIONS_TABLE);
        old.run(&pool).await.unwrap();
        let before: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pragma_table_info('audit_log') WHERE name = 'pending_id'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(before, 0, "the old set must not have 0008");
        pool.close().await;

        let pool = init_db(&db_path)
            .await
            .expect("an existing DB must take 0008 after 0101-0102");
        let versions: Vec<i64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT version FROM {MIGRATIONS_TABLE} ORDER BY version"
        )))
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            versions,
            vec![2, 3, 4, 5, 7, 8, 10, 12, 13, 14, 15, 101, 102, 103, 104, 105, 106, 107, 108, 109]
        );
        let after: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pragma_table_info('audit_log') WHERE name = 'pending_id'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(after, 1);
        pool.close().await;
    }

    /// banto v7.4.0 の `0012_users_version.sql`（users の楽観ロック、banto#651）は、
    /// 既存 DB には次の起動で入り、既存の行は version 1 から始まる。0010・0012 を除いた
    /// 集合で作った DB に users の行を入れ、今の集合で起動し直して確かめる。
    #[tokio::test]
    async fn an_existing_db_without_0010_and_0012_gets_them_on_the_next_startup() {
        let dir = crate::test_support::TempDir::new();
        let old_set = dir.path().join("migrations-before-0010-0012");
        std::fs::create_dir(&old_set).unwrap();
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations-sqlite");
        for entry in std::fs::read_dir(&source).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name();
            if name.to_string_lossy().starts_with("0010_")
                || name.to_string_lossy().starts_with("0012_")
            {
                continue;
            }
            std::fs::copy(entry.path(), old_set.join(&name)).unwrap();
        }

        let db_path = dir.path().join("chronogazer.sqlite3");
        let pool = banto_storage::connect_sqlite(&db_path).await.unwrap();
        let mut old = sqlx::migrate::Migrator::new(old_set.as_path())
            .await
            .unwrap();
        old.dangerous_set_table_name(MIGRATIONS_TABLE);
        old.run(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO users (username, password_hash, display_name, account_uid) VALUES ('probe', 'x', 'Probe', 'probe-uid')",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;

        let pool = init_db(&db_path)
            .await
            .expect("an existing DB must take 0010 and 0012 on the next startup");
        let version: i64 = sqlx::query_scalar("SELECT version FROM users WHERE username = 'probe'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(version, 1, "an existing row starts at version 1");
        sqlx::query("SELECT record_resource, record_id FROM audit_log")
            .fetch_all(&pool)
            .await
            .expect("0010 must add the generated audit_log columns");
        pool.close().await;
    }

    /// 旧形式（記録テーブルが無いのに `users` か `settings` がある）は
    /// [`InitDbError::Legacy`] で拒否され、DB には何も足されない。
    #[tokio::test]
    async fn a_legacy_database_is_refused_without_being_touched() {
        for legacy_table in [
            "CREATE TABLE users (id INTEGER PRIMARY KEY, username TEXT NOT NULL)",
            "CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
        ] {
            let dir = crate::test_support::TempDir::new();
            let db_path = dir.path().join("chronogazer.sqlite3");
            let pool = banto_storage::connect_sqlite(&db_path).await.unwrap();
            sqlx::query(legacy_table).execute(&pool).await.unwrap();
            pool.close().await;

            let err = init_db(&db_path)
                .await
                .expect_err("a legacy DB must be refused");
            match &err {
                InitDbError::Legacy(legacy) => assert_eq!(legacy.path, db_path),
                other => panic!("expected InitDbError::Legacy, got {other:?}"),
            }
            let message = err.to_string();
            assert!(message.contains("旧形式"), "{message}");
            assert!(
                message.contains(&db_path.display().to_string()),
                "{message}"
            );

            let pool = banto_storage::connect_sqlite(&db_path).await.unwrap();
            let tables: Vec<String> = sqlx::query_scalar(
                "SELECT name FROM sqlite_master WHERE type = 'table' \
                 AND name NOT LIKE 'sqlite_%' ORDER BY name",
            )
            .fetch_all(&pool)
            .await
            .unwrap();
            assert_eq!(tables.len(), 1, "nothing may be added: {tables:?}");
            pool.close().await;
        }
    }

    /// banto v8 への追従（docs/banto-v8-upgrade-plan.md §5 の 2）: v7 の版の DB（旧い記録の表・
    /// 旧い 0003/0005）は [`LegacyKind::BeforeBantoV8`] で断られ、ファイルは byte で変わらない。
    /// v7 の 0003 の checksum は [`PRE_V8_USERS_CHECKSUM`] と同じ。
    #[tokio::test]
    async fn a_banto_v7_database_is_refused_without_being_touched() {
        let dir = crate::test_support::TempDir::new();
        let db_path = dir.path().join("chronogazer.sqlite3");
        write_banto_v7_db(&db_path).await;
        let before = std::fs::read(&db_path).unwrap();

        let pool = banto_storage::connect_sqlite(&db_path).await.unwrap();
        let checksum: Vec<u8> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT checksum FROM {PRE_V8_MIGRATIONS_TABLE} WHERE version = 3"
        )))
        .fetch_one(&pool)
        .await
        .unwrap();
        pool.close().await;
        assert_eq!(hex_of(&checksum), PRE_V8_USERS_CHECKSUM);
        // 上の読み取りで本体は変わっていない（以降の比べる基準）。
        assert_eq!(std::fs::read(&db_path).unwrap(), before);

        match refuse_legacy_profile(&db_path).await {
            Err(InitDbError::Legacy(legacy)) => {
                assert_eq!(legacy.path, db_path);
                assert_eq!(legacy.kind, LegacyKind::BeforeBantoV8);
            }
            other => panic!("expected Legacy, got {other:?}"),
        }
        match init_db(&db_path).await {
            Err(InitDbError::Legacy(legacy)) => {
                assert_eq!(legacy.kind, LegacyKind::BeforeBantoV8);
                assert!(legacy.to_string().contains("v8.0.0 より前"), "{legacy}");
            }
            other => panic!("expected Legacy, got {other:?}"),
        }
        assert_eq!(
            std::fs::read(&db_path).unwrap(),
            before,
            "the v7 DB changed"
        );
    }

    /// 2026-10-02 のスキーマ整理より前の、`banto_tags` が共有の `_sqlx_migrations` に版 1 を
    /// 記録していた DB は [`LegacyKind::BeforeSchemaReset`]。新しい DB は旧形式ではない。
    #[tokio::test]
    async fn a_shared_history_with_banto_tags_version_1_is_before_the_schema_reset() {
        let pool = banto_storage::connect_sqlite_memory().await.unwrap();
        sqlx::raw_sql(
            "CREATE TABLE _sqlx_migrations (version BIGINT PRIMARY KEY, description TEXT NOT NULL, installed_on TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP, success BOOLEAN NOT NULL, checksum BLOB NOT NULL, execution_time BIGINT NOT NULL); INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) VALUES (1, 'plc connections', 1, x'00', 0); CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(
            is_legacy_schema(&pool).await.unwrap(),
            Some(LegacyKind::BeforeSchemaReset)
        );
        pool.close().await;

        let fresh = init_db_memory().await.unwrap();
        assert_eq!(is_legacy_schema(&fresh).await.unwrap(), None);
        fresh.close().await;
    }

    /// banto v8 の起動時の検査（`CandidateCheck`）: 履歴は合っていても起動できない候補
    /// （組み込みの `admin` の行が無い）は予約の前に断られ、今の DB は変わらない。
    #[tokio::test]
    async fn a_backup_that_could_not_start_is_refused_at_staging() {
        use crate::backup::BackupService;

        let dir = crate::test_support::TempDir::new();
        let db_path = dir.path().join("chronogazer.sqlite3");
        let pool = init_db(&db_path).await.expect("init_db");
        let backup = BackupService::new(db_path.clone(), Db::Sqlite(pool.clone()))
            .with_migrations(sqlite_restore_manifest());
        // 組み込みの行を消してからバックアップを取る（候補だけが壊れている）。
        sqlx::query("DELETE FROM users WHERE username = 'admin'")
            .execute(&pool)
            .await
            .unwrap();
        let created = backup.create().await.expect("create backup");
        let err = backup
            .stage_restore_from_file(&created.file_name)
            .await
            .expect_err("a candidate the app cannot start on must be refused");
        assert!(format!("{err:?}").contains("起動できなく"), "{err:?}");
        assert!(backup.pending_restore().await.is_none());
        // 検査用の写しは残らない。
        let scope = BackupService::pending_restore_file(&db_path)
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        let leftovers: Vec<String> = std::fs::read_dir(&scope)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".restore-check-"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
        pool.close().await;
    }

    /// バックアップと復元の往復を ChronoGazer の実際の migration の上で
    /// （I2a #498 のレビューの任意提案、I2b で追加）: `init_db` で開いた DB で
    /// バックアップを作る → 値を変える → そのバックアップの復元を予約する →
    /// プールを閉じる → 起動時の適用（`apply_pending_restore_at_startup`）→
    /// `init_db` でもう一度開く。復元した DB は新形式として通り（旧形式の
    /// 拒否に当たらない・migrate は冪等）、値はバックアップ時のものに戻り、
    /// 適用前の自動バックアップが残る。
    #[tokio::test]
    async fn backup_restore_round_trip_reopens_with_init_db() {
        use crate::backup::BackupService;

        let dir = crate::test_support::TempDir::new();
        let db_path = dir.path().join("chronogazer.sqlite3");
        let pool = init_db(&db_path).await.expect("init_db");
        sqlx::query("INSERT INTO settings (key, value) VALUES ('probe', 'at-backup')")
            .execute(&pool)
            .await
            .unwrap();
        let backup = BackupService::new(db_path.clone(), Db::Sqlite(pool.clone()))
            .with_migrations(sqlite_restore_manifest());
        let created = backup.create().await.expect("create backup");
        sqlx::query("UPDATE settings SET value = 'after-backup' WHERE key = 'probe'")
            .execute(&pool)
            .await
            .unwrap();
        backup
            .stage_restore_from_file(&created.file_name)
            .await
            .expect("stage restore");
        assert!(backup.pending_restore().await.is_some());
        pool.close().await;

        let applied =
            BackupService::apply_pending_restore_at_startup(&db_path, &sqlite_restore_manifest())
                .await
                .expect("apply at startup")
                .expect("a staged restore is applied");

        let pool = init_db(&db_path)
            .await
            .expect("the restored DB must pass init_db");
        let value: String = sqlx::query_scalar("SELECT value FROM settings WHERE key = 'probe'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(value, "at-backup");
        let backup = BackupService::new(db_path.clone(), Db::Sqlite(pool.clone()))
            .with_migrations(sqlite_restore_manifest());
        assert!(backup.pending_restore().await.is_none());
        let names: Vec<String> = backup
            .list()
            .await
            .expect("list")
            .into_iter()
            .map(|info| info.file_name)
            .collect();
        assert!(
            names.contains(&applied.pre_restore_backup_file_name),
            "{names:?}"
        );
        pool.close().await;
    }

    // ---- R2b-1d: 0025 の前のバックアップ（設計 §6.7） ----------------------------------

    const PG_PASSWORD: &str = "TOPSECRET-pg-pw-7f3a";

    fn file_names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn contains(haystack: &[u8], needle: &str) -> bool {
        haystack
            .windows(needle.len())
            .any(|window| window == needle.as_bytes())
    }

    /// 0025 より前（banto-tags 0001〜0024）の ChronoGazer の DB。`rows` なら
    /// `modbus-tcp` と、平文のパスワードを持つ `postgres` の行（古い表の列は生の SQL で入れる）。
    async fn db_before_0025(path: &Path, rows: bool) {
        let pool = banto_storage::connect_sqlite(path).await.unwrap();
        run_app_migrations(&pool).await.unwrap();
        let dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../crates/banto-tags/migrations");
        let mut tags = sqlx::migrate::Migrator::new(dir.as_path()).await.unwrap();
        tags.dangerous_set_table_name(banto_tags::MIGRATIONS_TABLE);
        tags.run_to(24, &pool).await.unwrap();
        if rows {
            sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
                "INSERT INTO plc_connections (id, name, protocol, host, port, unit_id, enabled, \
                 simulation, word_order, database, username, password, cpu_type, settings) \
                 VALUES (1, 'plc', 'modbus-tcp', '10.0.0.5', 502, 1, 1, 0, 'low_high', NULL, NULL, NULL, 'R', NULL), \
                 (2, 'pg', 'postgres', 'db.local', 5432, 1, 1, 0, 'low_high', 'plant', 'reader', '{PG_PASSWORD}', 'R', NULL)"
            )))
            .execute(&pool)
            .await
            .unwrap();
        }
        pool.close().await;
    }

    /// 0025 より前で行のある DB: 起動で `.bak` ができ（パスワードのバイト列は無く、行は
    /// 残る）、DB 本体は移行され、次の起動で前回の作りかけは消える（完成した `.bak` は残る）。
    #[tokio::test]
    async fn a_populated_db_before_0025_is_backed_up_without_passwords() {
        let dir = crate::test_support::TempDir::new();
        let db_path = dir.path().join("chronogazer.sqlite3");
        db_before_0025(&db_path, true).await;

        let pool = init_db(&db_path).await.expect("init_db");
        let applied: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT COUNT(*) FROM {} WHERE version = 25",
            banto_tags::MIGRATIONS_TABLE
        )))
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(applied, 1, "0025 is applied to the live DB");
        pool.close().await;

        let backups: Vec<String> = file_names(dir.path())
            .into_iter()
            .filter(|name| name.ends_with(".bak"))
            .collect();
        assert_eq!(backups.len(), 1, "{:?}", file_names(dir.path()));
        assert!(
            backups[0].starts_with("chronogazer.sqlite3.before-0025-"),
            "{backups:?}"
        );
        let bak = dir.path().join(&backups[0]);
        let bytes = std::fs::read(&bak).unwrap();
        assert!(
            !contains(&bytes, PG_PASSWORD),
            "the password is in the backup"
        );
        // The backup is the old schema with every row (passwords cleared).
        let opened = banto_storage::connect_sqlite(&bak).await.unwrap();
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM plc_connections")
            .fetch_one(&opened)
            .await
            .unwrap();
        assert_eq!(rows, 2);
        let passwords: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM plc_connections WHERE password IS NOT NULL")
                .fetch_one(&opened)
                .await
                .unwrap();
        assert_eq!(passwords, 0);
        opened.close().await;

        // A second start does not make another backup (0025 is applied).
        let pool = init_db(&db_path).await.expect("second init_db");
        pool.close().await;
        let again = file_names(dir.path())
            .into_iter()
            .filter(|name| name.ends_with(".bak"))
            .count();
        assert_eq!(again, 1);
    }

    /// 前回の起動の作りかけ（`.bak.partial` と付随ファイル、Hub の予行の写し）は、次の
    /// 起動で消える。完成した `.bak` と DB は残る。
    #[tokio::test]
    async fn partial_backups_left_by_a_crash_are_removed_on_the_next_init() {
        let dir = crate::test_support::TempDir::new();
        let db_path = dir.path().join("chronogazer.sqlite3");
        db_before_0025(&db_path, true).await;
        let partial = "chronogazer.sqlite3.before-0025-20261021T000000Z.bak.partial";
        let finished = "chronogazer.sqlite3.before-0025-20261020T000000Z.bak";
        std::fs::write(dir.path().join(partial), PG_PASSWORD).unwrap();
        std::fs::write(dir.path().join(format!("{partial}-journal")), PG_PASSWORD).unwrap();
        std::fs::write(dir.path().join(finished), b"finished").unwrap();

        let pool = init_db(&db_path).await.expect("init_db");
        pool.close().await;

        let names = file_names(dir.path());
        assert!(
            !names.iter().any(|name| name.contains(".bak.partial")),
            "{names:?}"
        );
        assert!(names.iter().any(|name| name == finished), "{names:?}");
    }

    /// 新規の DB・0025 の後の DB・行の無い 0025 より前の DB には、バックアップを作らない。
    #[tokio::test]
    async fn no_backup_for_a_fresh_migrated_or_empty_db() {
        let dir = crate::test_support::TempDir::new();
        let fresh = dir.path().join("fresh.sqlite3");
        init_db(&fresh).await.expect("fresh").close().await;
        init_db(&fresh).await.expect("migrated").close().await;

        let empty = dir.path().join("empty.sqlite3");
        db_before_0025(&empty, false).await;
        init_db(&empty).await.expect("empty").close().await;

        let names = file_names(dir.path());
        assert!(!names.iter().any(|name| name.contains(".bak")), "{names:?}");
    }

    /// #623 PR3b: 握った組で開く経路（[`init_db_pinned`]）を試す握り方（Plain はどの OS でも、
    /// Windows は Guarded も）。
    fn pin_modes() -> Vec<banto_safe_root::Mode> {
        let mut modes = vec![banto_safe_root::Mode::Plain];
        if cfg!(windows) {
            modes.push(banto_safe_root::Mode::Guarded);
        }
        modes
    }

    /// `dir`（既にあるフォルダ）を `mode` で握ったルート。
    fn root_in(dir: &Path, mode: banto_safe_root::Mode) -> banto_safe_root::SafeRoot {
        let owners = match mode {
            #[cfg(windows)]
            banto_safe_root::Mode::Guarded => {
                vec![banto_safe_root::acl::current_user_sid().expect("今の利用者の SID")]
            }
            _ => Vec::new(),
        };
        banto_safe_root::open_root(&banto_safe_root::RootSpec {
            expected: dir.to_path_buf(),
            known: banto_safe_root::guard::KnownFolders::default(),
            owners,
            create_access: None,
            create: false,
            mode,
        })
        .expect("open the root")
    }

    /// #623 PR3b: 握った組で開く（[`init_db_pinned`]）と、前回の作りかけは握ったフォルダの
    /// ハンドル基準で消え、`.bak` はパスワードを持たずにでき、作りかけ・`-journal` は残らない。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_pinned_init_backs_up_and_cleans_up_through_the_managed_dir() {
        let base = crate::test_support::TempDir::new();
        for mode in pin_modes() {
            let root_dir = base.path().join(format!("{mode:?}"));
            std::fs::create_dir(&root_dir).unwrap();
            let root = root_in(&root_dir, mode);
            let db_path = root_dir.join("chronogazer.sqlite3");
            db_before_0025(&db_path, true).await;
            let partial = "chronogazer.sqlite3.before-0025-20261021T000000Z.bak.partial";
            std::fs::write(root_dir.join(partial), PG_PASSWORD).unwrap();

            let pinned = root.as_dir().pin_sqlite("chronogazer.sqlite3").unwrap();
            let pool = init_db_pinned(&pinned, root.as_dir(), None)
                .await
                .expect("init_db_pinned");
            pool.close().await;
            drop(pinned);

            let names = file_names(&root_dir);
            assert!(
                !names.iter().any(|name| name.contains(".bak.partial")),
                "{names:?}"
            );
            assert!(
                !names
                    .iter()
                    .any(|name| name.contains(".before-0025-") && name.ends_with("-journal")),
                "{names:?}"
            );
            let backups: Vec<&String> = names.iter().filter(|n| n.ends_with(".bak")).collect();
            assert_eq!(backups.len(), 1, "{names:?}");
            let bytes = std::fs::read(root_dir.join(backups[0])).unwrap();
            assert!(
                !contains(&bytes, PG_PASSWORD),
                "the password is in the backup"
            );
        }
    }

    /// #623 PR3b: 握った組で開く経路でも、前回の作りかけの名前をディレクトリが塞いでいれば、
    /// バックアップも移行もせずに `InitDbError::Storage` で失敗する（黙って飛ばさない）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_pinned_init_stops_on_a_leftover_blocked_by_a_directory() {
        let base = crate::test_support::TempDir::new();
        for mode in pin_modes() {
            let root_dir = base.path().join(format!("{mode:?}"));
            std::fs::create_dir(&root_dir).unwrap();
            let root = root_in(&root_dir, mode);
            let db_path = root_dir.join("chronogazer.sqlite3");
            db_before_0025(&db_path, true).await;
            let stuck =
                root_dir.join("chronogazer.sqlite3.before-0025-20261021T000000Z.bak.partial");
            std::fs::create_dir(&stuck).unwrap();

            let pinned = root.as_dir().pin_sqlite("chronogazer.sqlite3").unwrap();
            match init_db_pinned(&pinned, root.as_dir(), None).await {
                Err(InitDbError::Storage(err)) => {
                    assert!(err.to_string().contains("bak.partial"), "{err}")
                }
                other => panic!("expected InitDbError::Storage, got {other:?}"),
            }
            drop(pinned);
            assert!(stuck.is_dir());
            assert!(
                !file_names(&root_dir).iter().any(|n| n.ends_with(".bak")),
                "no backup"
            );
        }
    }

    /// 前回の作りかけを消せない（`.bak.partial` の名前をディレクトリが塞ぐ）ときは、バックアップも
    /// 移行もせずに `InitDbError::Storage` で失敗する（他の DB 初期化の失敗と同じ経路）。
    #[tokio::test]
    async fn a_leftover_that_cannot_be_removed_fails_init_db_without_migrating() {
        let dir = crate::test_support::TempDir::new();
        let db_path = dir.path().join("chronogazer.sqlite3");
        db_before_0025(&db_path, true).await;
        // A leftover that cannot be deleted stops the start (here: a directory of that name).
        let stuck = dir
            .path()
            .join("chronogazer.sqlite3.before-0025-20261021T000000Z.bak.partial");
        std::fs::create_dir(&stuck).unwrap();

        match init_db(&db_path).await {
            Err(InitDbError::Storage(err)) => {
                assert!(err.to_string().contains("bak.partial"), "{err}")
            }
            other => panic!("expected InitDbError::Storage, got {other:?}"),
        }
        std::fs::remove_dir(&stuck).unwrap();
        let pool = banto_storage::connect_sqlite(&db_path).await.unwrap();
        let applied: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT COUNT(*) FROM {} WHERE version = 25",
            banto_tags::MIGRATIONS_TABLE
        )))
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(applied, 0, "0025 must not be applied");
        pool.close().await;
    }
}
