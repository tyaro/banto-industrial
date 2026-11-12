//! migration `0025` の前に DB を守る部品（R2b-1d、docs/banto-hub-connection-settings-design.md
//! §6.4・§6.6・§6.7）。アプリに依存しないので、Hub と ChronoGazer が同じものを呼ぶ。
//!
//! migration `0025`（接続設定を `settings` の JSON と秘密の表へ移す）は**戻せない**
//! （0025 を当てた DB は 0001〜0024 の migrator では開けない）。そこで `banto_tags::migrate`
//! の前に、アプリは次を行う:
//!
//! 1. [`remove_leftovers`]（毎回の起動）: 前回の起動の残り物（作りかけのバックアップ・Hub の
//!    予行の写し。パスワードの平文を含みうる）を消す。消せなければ起動を止める。
//! 2. [`pending_0025`]: 0025 が未適用か（`_sqlx_migrations_banto_tags`）。
//! 3. [`backup_before_0025`]: パスワードを除いた `.bak` を作る（§6.4）。
//!
//! # 置き場（[`Place`]、#623 PR3b）
//!
//! DB の横（profile の `config/`・ChronoGazer のサービスのルートの直下）に作る一時ファイルは、
//! 置き場によって扱いを分ける:
//!
//! - [`Place::Path`]（デスクトップ・コンソール・テスト・`BANTO_DB` が profile の外）: 今までどおり
//!   パス（`std::fs`）で作る・消す・付け替える。
//! - [`Place::Managed`]（サービス。DB を `ManagedDir::pin_sqlite(_checked)` で握って開いたとき）:
//!   握ったフォルダ（`banto_safe_root::ManagedDir`）のハンドル基準で、名前を辿らずに扱う。
//!   一時ファイルは `ManagedDir::pin_file(_checked)` で空のまま作って**握ったまま**
//!   `VACUUM INTO` の宛先にし（握っている間は他者が名前を変更・削除・置換できない）、閉じて
//!   握りを離してから、消すのは `ManagedDir::remove`、`.bak` への付け替えは
//!   `ManagedDir::rename_no_replace(_checked)`（置き換えない）。一覧は `ManagedDir::list`（握った
//!   ハンドルの列挙）。`expected`（運用モードの形）が `Some` なら `_checked` 版を使い、フォルダと
//!   作ったファイルの記述子がその形かも確かめる。握りを離してから付け替える・消すまでの間は
//!   塞がらない（運用モードの ACL で利用者がこの木に名前を作れないことに頼る。safe-root の
//!   `rename_no_replace` の doc の残留）。
//!
//! 写し（作りかけのバックアップ・予行の写し）は `journal_mode = MEMORY` で開く（[`copy_options`]）。
//! `-journal` を作らないので、握っていない付随の名前が生まれない（落ちたときの写しの整合は
//! 要らない。作りかけは次の起動で消す）。
//!
//! # バックアップ（§6.4）
//!
//! 作りかけの名前 `<DB>.before-0025-<UTC>.bak.partial`（既にある `.bak`・`.bak.partial` の名前は
//! 使わず、同じ秒なら `-2`・`-3`… を足す）を空で用意して `VACUUM INTO` し、[`copy_options`] で
//! 開いて `plc_connections.password` を NULL に、`pending_changes` 表があれば（Hub だけが持つ）
//! そのペイロードから `password` を外し、`VACUUM` して閉じてから（[`Place::Managed`] は握りを
//! 離して）`.bak` へ付け替える。平文を含みうるのは `.partial` の名前のものだけで、途中で
//! 失敗すれば消し（[`TempFile`]）、プロセスごと落ちれば次の起動の [`remove_leftovers`] で
//! 消える（§6.6）。完成した `.bak` は消さない。
//!
//! # 置かないもの
//!
//! 予行・preflight（レジストリが要る）、清掃待ちの印（Hub の `settings` 表）は Hub の
//! `upgrade.rs`。ChronoGazer の行は `modbus-tcp` / `slmp` / `hub` だけで 0025 が断れない
//! ので、予行は要らない（§6.7）。

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use banto_safe_root::{DirAccess, ManagedDir, PinnedName, SafeRootError};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode};
use sqlx::{Connection, SqliteConnection, SqlitePool};

/// 戻せない移行の版（`migrations/0025_plc_connections_settings_json.sql`）。
pub const MIGRATION_0025_VERSION: i64 = 25;

/// Hub の予行の写しの名前の頭（config/ の中、`{prefix}{nonce}.sqlite3`）。写しを作るのは Hub
/// だけだが、残り物の掃除はこの名前の型も消す（どのアプリの config/ にあっても平文を含みうる）。
pub const DRYRUN_PREFIX: &str = ".banto-hub-0025-dryrun-";

/// バックアップの名前の中ほど（`<DB のファイル名>.before-0025-<UTC>.bak`）。
pub const BACKUP_INFIX: &str = ".before-0025-";
/// 完成したバックアップの末尾。
pub const BACKUP_SUFFIX: &str = ".bak";
/// 作りかけのバックアップの末尾（平文を含みうる。毎回の起動で消す）。
pub const PARTIAL_SUFFIX: &str = ".bak.partial";

/// SQLite が DB のファイルの横に作りうる付随ファイルの末尾。
const SIDE_FILES: [&str; 3] = ["-journal", "-wal", "-shm"];

/// 移行の前後処理の失敗。どれも起動を止める。`Display` が利用者向けの説明（Hub の
/// `UpgradeError` は同じ文言の変種へ写す）。
#[derive(Debug, thiserror::Error)]
pub enum UpgradeError {
    /// 前の起動の残り物（予行の写し・作りかけのバックアップ。平文を含みうる）を消せない。
    /// [`Place::Managed`] の失敗（`SafeRootError`。フォルダ・reparse point で塞がれている
    /// など）は `error` の中身に包む（`kind` は safe-root の I/O の失敗ならその種類、ほかは
    /// `Other`）。
    #[error(
        "前回の起動で残ったファイル {path} を消せません（パスワードの平文を含みうるため、\
         消せるまで起動しません。手で消してから起動し直してください）: {error}"
    )]
    LeftoverNotRemoved {
        path: PathBuf,
        error: std::io::Error,
    },
    /// バックアップに失敗した。DB は変わっていない（作りかけは次の起動で消える）。
    #[error(
        "移行の前のバックアップ {path} を作れませんでした（DB は変更していません）: {message}"
    )]
    Backup { path: PathBuf, message: String },
    /// 移行の前後処理の DB の読み書きに失敗した。
    #[error("移行の準備で DB の読み書きに失敗しました: {0}")]
    Storage(String),
}

fn storage(err: impl std::fmt::Display) -> UpgradeError {
    UpgradeError::Storage(err.to_string())
}

/// safe-root の失敗を `std::io::Error` に包む（[`UpgradeError::LeftoverNotRemoved`] と
/// [`TempFile`] の結果の型を、パスの経路と揃える）。
fn safe_root_io(err: SafeRootError) -> std::io::Error {
    let kind = match &err {
        SafeRootError::Io { kind, .. } => *kind,
        _ => std::io::ErrorKind::Other,
    };
    std::io::Error::new(kind, err)
}

/// DB のファイルの置き場（profile の `config/`）。
pub fn config_dir_of(db_path: &Path) -> PathBuf {
    match db_path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

fn db_file_name(db_path: &Path) -> String {
    db_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "banto.sqlite3".to_string())
}

// ---- 置き場（#623 PR3b） ------------------------------------------------------------------

/// DB と、その横に作る一時ファイル・バックアップの置き場（モジュール doc「置き場」）。
#[derive(Clone, Copy)]
pub enum Place<'a> {
    /// DB のパス。横のファイルはパス（`std::fs`）で扱う。
    Path(&'a Path),
    /// 握ったフォルダ `dir` の直下の DB `db_name`。横のファイルは `dir` のハンドル基準で
    /// 扱う。`expected`（運用モードの形）が `Some` なら `ManagedDir` の `_checked` 版を使う。
    Managed {
        dir: &'a ManagedDir,
        db_name: &'a str,
        expected: Option<&'a DirAccess>,
    },
}

impl<'a> From<&'a Path> for Place<'a> {
    fn from(path: &'a Path) -> Self {
        Self::Path(path)
    }
}

impl<'a> From<&'a PathBuf> for Place<'a> {
    fn from(path: &'a PathBuf) -> Self {
        Self::Path(path.as_path())
    }
}

impl<'a> Place<'a> {
    /// [`Place::Managed`] を作る。
    pub fn managed(dir: &'a ManagedDir, db_name: &'a str, expected: Option<&'a DirAccess>) -> Self {
        Self::Managed {
            dir,
            db_name,
            expected,
        }
    }

    /// 置き場のフォルダのパス（表示用。[`Place::Managed`] は `ManagedDir::final_path`）。
    pub fn dir_path(&self) -> PathBuf {
        match self {
            Self::Path(path) => config_dir_of(path),
            Self::Managed { dir, .. } => dir.final_path().to_path_buf(),
        }
    }

    /// DB のファイル名。
    pub fn db_name(&self) -> String {
        match self {
            Self::Path(path) => db_file_name(path),
            Self::Managed { db_name, .. } => (*db_name).to_string(),
        }
    }

    /// DB のパス（表示用。[`Place::Managed`] は `ManagedDir::final_path` + 名前）。
    pub fn db_path(&self) -> PathBuf {
        match self {
            Self::Path(path) => path.to_path_buf(),
            Self::Managed { dir, db_name, .. } => dir.final_path().join(db_name),
        }
    }

    /// 置き場の中の名前 `name` のパス（表示用・パスの経路の実体）。
    fn child(&self, name: &str) -> PathBuf {
        self.dir_path().join(name)
    }

    /// 置き場の中の名前の集合（作りかけ・完成の名前を選ぶため）。[`Place::Managed`] は握った
    /// ハンドルの一覧。パスの経路は `None`（名前ごとに `exists` で見る）。
    fn names(&self) -> Result<Option<HashSet<String>>, SafeRootError> {
        match self {
            Self::Path(_) => Ok(None),
            Self::Managed { dir, .. } => Ok(Some(
                dir.list()?.into_iter().map(|entry| entry.name).collect(),
            )),
        }
    }
}

/// `name` が前の起動の残り物か（Hub の予行の写しと、`db_name` の作りかけのバックアップ、と
/// その付随ファイル）。完成した `.bak` は残り物ではない。
pub fn is_leftover(name: &str, db_name: &str) -> bool {
    if name.starts_with(DRYRUN_PREFIX) {
        return true;
    }
    let backup_prefix = format!("{db_name}{BACKUP_INFIX}");
    if !name.starts_with(&backup_prefix) {
        return false;
    }
    name.ends_with(PARTIAL_SUFFIX)
        || SIDE_FILES
            .iter()
            .any(|side| name.ends_with(&format!("{PARTIAL_SUFFIX}{side}")))
}

/// 毎回の起動の最初（§6.3 の 4・§6.4）: 置き場の残り物を消す。消したパスを返す。
/// 1 つでも消せなければ [`UpgradeError::LeftoverNotRemoved`]（起動を止める）。
///
/// [`Place::Managed`] は握ったハンドルの一覧から [`is_leftover`] の名前を選び、
/// `ManagedDir::remove`（開いた後のハンドルで確かめて消す）で消す。`ManagedDir::remove_where`
/// は使わない（フォルダ・reparse point を黙って飛ばすので、残り物の名前を塞がれても起動が
/// 進んでしまう）。名前を塞ぐフォルダ・リンクは `remove` が断る（Plain はパスの経路と同じく
/// `remove_file` が失敗する）ので、それも起動を止める。
pub fn remove_leftovers<'a>(place: impl Into<Place<'a>>) -> Result<Vec<PathBuf>, UpgradeError> {
    let place = place.into();
    let db_name = place.db_name();
    let mut removed = match place {
        Place::Path(db_path) => remove_leftovers_by_path(db_path, &db_name)?,
        Place::Managed { dir, .. } => {
            let entries = dir.list().map_err(|err| UpgradeError::LeftoverNotRemoved {
                path: place.dir_path(),
                error: safe_root_io(err),
            })?;
            let mut removed = Vec::new();
            for entry in entries {
                if !is_leftover(&entry.name, &db_name) {
                    continue;
                }
                let path = place.child(&entry.name);
                match dir.remove(&entry.name) {
                    Ok(true) => removed.push(path),
                    Ok(false) => {} // 一覧の後に消えた。
                    Err(err) => {
                        return Err(UpgradeError::LeftoverNotRemoved {
                            path,
                            error: safe_root_io(err),
                        })
                    }
                }
            }
            removed
        }
    };
    removed.sort();
    Ok(removed)
}

fn remove_leftovers_by_path(db_path: &Path, db_name: &str) -> Result<Vec<PathBuf>, UpgradeError> {
    let dir = config_dir_of(db_path);
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(UpgradeError::LeftoverNotRemoved { path: dir, error }),
    };
    let mut removed = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| UpgradeError::LeftoverNotRemoved {
            path: dir.clone(),
            error,
        })?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !is_leftover(&name, db_name) {
            continue;
        }
        let path = entry.path();
        match std::fs::remove_file(&path) {
            Ok(()) => removed.push(path),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(UpgradeError::LeftoverNotRemoved { path, error }),
        }
    }
    Ok(removed)
}

/// 0025 が未適用か。banto-tags の記録表が無い（新規の DB。migrate が全部を作るので、写す
/// ものもバックアップするものも無い）なら `false`。
pub async fn pending_0025(pool: &SqlitePool) -> Result<bool, UpgradeError> {
    let has_table: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?")
            .bind(crate::MIGRATIONS_TABLE)
            .fetch_one(pool)
            .await
            .map_err(storage)?;
    if has_table == 0 {
        return Ok(false);
    }
    // AssertSqlSafe: 補間するのはこの crate の定数の表の名前だけ。
    let applied: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT COUNT(*) FROM {} WHERE version = ?",
        crate::MIGRATIONS_TABLE
    )))
    .bind(MIGRATION_0025_VERSION)
    .fetch_one(pool)
    .await
    .map_err(storage)?;
    Ok(applied == 0)
}

/// 0025 の直前（0024）までの未適用の migration だけを当てる（Hub の予行の写しに、0025 の前の
/// 列を読む前に当てる。alpha.32 以前の DB には `settings`（0023）・`cpu_type`（0021）などが
/// まだ無い）。本物の DB には当てない（予行が通るまで本物は変えない、§6.3）。
pub async fn migrate_before_0025(pool: &SqlitePool) -> Result<(), UpgradeError> {
    crate::migrate_to(pool, MIGRATION_0025_VERSION - 1)
        .await
        .map_err(storage)
}

/// バックアップを取る価値のある中身があるか（0025 が未適用の DB で、接続・収集グループ・
/// タグのどれかに 1 行でもある）。空のままの DB（記録表だけ作って落ちた起動など）には
/// `.bak` を作らない。0025 が未適用であることは呼び出し側が [`pending_0025`] で確かめる。
pub async fn has_rows_to_protect(pool: &SqlitePool) -> Result<bool, UpgradeError> {
    for table in ["plc_connections", "collection_groups", "tags"] {
        if !table_exists(pool, table).await.map_err(storage)? {
            continue;
        }
        // AssertSqlSafe: 補間するのは上の固定の表の名前だけ。
        let any: Option<i64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT 1 FROM {table} LIMIT 1"
        )))
        .fetch_optional(pool)
        .await
        .map_err(storage)?;
        if any.is_some() {
            return Ok(true);
        }
    }
    Ok(false)
}

async fn table_exists<'e, E>(executor: E, name: &str) -> Result<bool, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
{
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?")
            .bind(name)
            .fetch_one(executor)
            .await?;
    Ok(count > 0)
}

async fn column_exists<'e, E>(executor: E, table: &str, column: &str) -> Result<bool, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
{
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pragma_table_info(?) WHERE name = ?")
        .bind(table)
        .bind(column)
        .fetch_one(executor)
        .await?;
    Ok(count > 0)
}

// ---- 一時ファイルの後始末 ----------------------------------------------------------------

/// 平文を含みうる一時ファイル（Hub の予行の写し・作りかけのバックアップ）。`Drop` で本体と
/// 付随ファイルを消す（パニックの経路でも）。[`TempFile::finish`] は消して結果を返す
/// （消せなければ起動を止める）。[`TempFile::keep`] は消さずに手放す（完成した
/// バックアップ、テストの「落ちた」の再現）。
///
/// [`Place::Managed`] で作ったもの（[`TempFile::create`]）は、空のファイルを作って**握った
/// まま**持つ（`PinnedName`）。相手（SQLite）には [`TempFile::path`] を渡す。消す・付け替える・
/// 手放すときは、先に握りを離す（握った名前は自分でも消せない）。消すのは握ったフォルダの
/// `ManagedDir::remove`。相手の接続は、それより前に閉じておくこと（呼ぶ側の責任。閉じて
/// いないと Windows では消せない）。
pub struct TempFile<'a> {
    inner: TempInner<'a>,
    armed: bool,
}

enum TempInner<'a> {
    Path(PathBuf),
    Managed {
        dir: &'a ManagedDir,
        expected: Option<&'a DirAccess>,
        name: String,
        /// 握り。離した後は `None`。
        pin: Option<PinnedName>,
        /// 表示・相手に渡すパス（`PinnedName::path` と同じ）。
        path: PathBuf,
    },
}

impl TempFile<'static> {
    /// パスの一時ファイル（まだ作らない。`VACUUM INTO` などの相手が作る）。
    pub fn new(path: PathBuf) -> Self {
        Self {
            inner: TempInner::Path(path),
            armed: true,
        }
    }
}

impl<'a> TempFile<'a> {
    /// 置き場 `place` の中の名前 `name` の一時ファイルを用意する。[`Place::Path`] はパスを
    /// 決めるだけ（[`TempFile::new`]）。[`Place::Managed`] は `ManagedDir::pin_file(_checked)`
    /// で空のファイルを作って握る（既にあればそれを握る。`VACUUM INTO` は空でない宛先を断る）。
    pub fn create(place: Place<'a>, name: &str) -> Result<Self, SafeRootError> {
        match place {
            Place::Path(_) => Ok(TempFile::new(place.child(name))),
            Place::Managed { dir, expected, .. } => {
                let pin = match expected {
                    Some(expected) => dir.pin_file_checked(name, true, expected)?,
                    None => dir.pin_file(name, true)?,
                }
                // `create` が真なら `None` にならない（safe-root の契約）。念のため失敗にする。
                .ok_or(SafeRootError::Io {
                    op: banto_safe_root::IoOp::CreateFile,
                    kind: std::io::ErrorKind::NotFound,
                    os_error: None,
                })?;
                Ok(Self {
                    inner: TempInner::Managed {
                        dir,
                        expected,
                        name: name.to_string(),
                        path: pin.path().to_path_buf(),
                        pin: Some(pin),
                    },
                    armed: true,
                })
            }
        }
    }

    /// 相手（SQLite）に渡すパス。
    pub fn path(&self) -> &Path {
        match &self.inner {
            TempInner::Path(path) => path,
            TempInner::Managed { path, .. } => path,
        }
    }

    /// 握りを離す（[`Place::Managed`] だけ。2 回目以降は何もしない）。
    fn release(&mut self) {
        if let TempInner::Managed { pin, .. } = &mut self.inner {
            drop(pin.take());
        }
    }

    fn remove_all(&mut self) -> Result<(), (PathBuf, std::io::Error)> {
        self.release();
        let mut result = Ok(());
        let mut record = |path: PathBuf, error: std::io::Error| {
            if result.is_ok() {
                result = Err((path, error));
            }
        };
        match &self.inner {
            TempInner::Path(path) => {
                let mut paths = vec![path.clone()];
                for side in SIDE_FILES {
                    let mut name = path.as_os_str().to_owned();
                    name.push(side);
                    paths.push(PathBuf::from(name));
                }
                for candidate in paths {
                    match std::fs::remove_file(&candidate) {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => record(candidate, error),
                    }
                }
            }
            TempInner::Managed { dir, name, .. } => {
                let mut names = vec![name.clone()];
                names.extend(SIDE_FILES.iter().map(|side| format!("{name}{side}")));
                for candidate in names {
                    if let Err(err) = dir.remove(&candidate) {
                        record(dir.final_path().join(&candidate), safe_root_io(err));
                    }
                }
            }
        }
        result
    }

    /// 同じ置き場の `final_name` へ付け替える（完成したバックアップ）。[`Place::Managed`] は
    /// 握りを離してから `ManagedDir::rename_no_replace(_checked)`（辿らず・置き換えない。
    /// 宛先が既にあれば失敗）。パスの経路は `std::fs::rename`。失敗しても `self` は消す側の
    /// まま（落とせば `Drop` が消す）。成功したら付け替えた先のパスを返す（`self` は手放して
    /// よい）。
    fn rename_to(&mut self, final_name: &str) -> Result<PathBuf, String> {
        self.release();
        match &self.inner {
            TempInner::Path(path) => {
                let final_path = path.with_file_name(final_name);
                std::fs::rename(path, &final_path).map_err(|err| err.to_string())?;
                Ok(final_path)
            }
            TempInner::Managed {
                dir,
                expected,
                name,
                ..
            } => {
                let renamed = match expected {
                    Some(expected) => dir.rename_no_replace_checked(name, final_name, expected),
                    None => dir.rename_no_replace(name, final_name),
                }
                .map_err(|err| err.to_string())?;
                if !renamed {
                    return Err(format!("作りかけの {name} が見つかりません"));
                }
                Ok(dir.final_path().join(final_name))
            }
        }
    }

    /// 消して結果を返す。
    pub fn finish(mut self) -> Result<(), (PathBuf, std::io::Error)> {
        self.armed = false;
        self.remove_all()
    }

    /// 消さずに手放す（握りは離す）。
    pub fn keep(mut self) {
        self.armed = false;
        self.release();
    }
}

impl Drop for TempFile<'_> {
    fn drop(&mut self) {
        if self.armed {
            if let Err((path, error)) = self.remove_all() {
                eprintln!(
                    "banto-tags: [WARN] 一時ファイル {} を消せませんでした（次の起動で消します）: {error}",
                    path.display()
                );
            }
        }
    }
}

fn path_text(path: &Path) -> Result<&str, String> {
    path.to_str()
        .ok_or_else(|| format!("パスが UTF-8 ではありません: {}", path.display()))
}

/// `VACUUM INTO` で `pool` の DB の一貫した写しを `dest` に作る（`dest` は無いか、空のファイル。
/// [`Place::Managed`] では [`TempFile::create`] が握った空のファイル）。
pub async fn vacuum_into(pool: &SqlitePool, dest: &Path) -> Result<(), String> {
    sqlx::query("VACUUM INTO ?")
        .bind(path_text(dest)?)
        .execute(pool)
        .await
        .map(|_| ())
        .map_err(|err| err.to_string())
}

/// 写し（予行・バックアップ）を開く設定: 作らない・ジャーナルはメモリの中（`-journal`・WAL の
/// 付随の名前を作らない。握っていない名前を生まないため。#623 PR3b）・FK を強制（0025 の
/// park-and-restore の前提）。写しは落ちたら捨てるので、ジャーナルの耐久性は要らない。
pub fn copy_options(path: &Path) -> SqliteConnectOptions {
    SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(false)
        .journal_mode(SqliteJournalMode::Memory)
        .foreign_keys(true)
}

// ---- バックアップ（§6.4） -----------------------------------------------------------------

/// バックアップの工程（テストで「その工程の直後に落ちた」を再現する注入点）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackupStep {
    /// `VACUUM INTO` の後（まだ平文を含む）。
    AfterVacuumInto,
    /// パスワードを除いた後（空きページに平文が残りうる）。
    AfterStrip,
    /// `VACUUM` の後（閉じる前）。
    AfterVacuum,
    /// 閉じた後・`rename` の前。
    BeforeRename,
}

/// 移行の直前のバックアップ（§6.4）。完成した `.bak` のパスを返す。
pub async fn backup_before_0025<'a>(
    pool: &SqlitePool,
    place: impl Into<Place<'a>>,
) -> Result<PathBuf, UpgradeError> {
    backup_with(pool, place, None).await
}

/// [`backup_before_0025`] の本体。`crash_after` の工程に来たら、作りかけを**残したまま**
/// 失敗で返る（プロセスが落ちたのと同じ。[`Place::Managed`] では握りは離す。テストだけが使う）。
#[doc(hidden)]
pub async fn backup_with<'a>(
    pool: &SqlitePool,
    place: impl Into<Place<'a>>,
    crash_after: Option<BackupStep>,
) -> Result<PathBuf, UpgradeError> {
    let place = place.into();
    let db_name = place.db_name();
    let stamp: String = sqlx::query_scalar("SELECT strftime('%Y%m%dT%H%M%SZ', 'now')")
        .fetch_one(pool)
        .await
        .map_err(storage)?;
    let fail_at = |path: PathBuf| {
        move |message: String| UpgradeError::Backup {
            path: path.clone(),
            message,
        }
    };
    // 同じ秒に 2 回作る（移行が失敗して起動し直した）ときは連番を足す。既にある `.bak`・
    // `.bak.partial` の名前は使わない（上書き・再利用しない）。Managed の有無は握ったハンドルの
    // 一覧で見る。
    let names = place
        .names()
        .map_err(|err| fail_at(place.dir_path())(err.to_string()))?;
    let taken = |name: &str| match &names {
        Some(names) => names.contains(name),
        None => place.child(name).exists(),
    };
    let mut base = format!("{db_name}{BACKUP_INFIX}{stamp}");
    let mut n = 1;
    while taken(&format!("{base}{BACKUP_SUFFIX}")) || taken(&format!("{base}{PARTIAL_SUFFIX}")) {
        n += 1;
        base = format!("{db_name}{BACKUP_INFIX}{stamp}-{n}");
    }
    let partial_name = format!("{base}{PARTIAL_SUFFIX}");
    let final_name = format!("{base}{BACKUP_SUFFIX}");
    let fail = fail_at(place.child(&final_name));

    let mut partial =
        TempFile::create(place, &partial_name).map_err(|err| fail(err.to_string()))?;
    vacuum_into(pool, partial.path()).await.map_err(&fail)?;
    if crash_after == Some(BackupStep::AfterVacuumInto) {
        partial.keep();
        return Err(fail("（テスト）VACUUM INTO の後で落ちた".to_string()));
    }

    let mut conn = SqliteConnection::connect_with(&copy_options(partial.path()))
        .await
        .map_err(|err| fail(err.to_string()))?;
    let stripped = strip_secrets(&mut conn, crash_after).await;
    let closed = conn.close().await;
    match stripped {
        Ok(()) => {}
        Err(StripError::Crash(message)) => {
            partial.keep();
            return Err(fail(message));
        }
        Err(StripError::Sql(err)) => return Err(fail(err.to_string())),
    }
    closed.map_err(|err| fail(err.to_string()))?;
    if crash_after == Some(BackupStep::BeforeRename) {
        partial.keep();
        return Err(fail("（テスト）rename の前で落ちた".to_string()));
    }

    let final_path = partial.rename_to(&final_name).map_err(&fail)?;
    partial.keep();
    Ok(final_path)
}

enum StripError {
    Sql(sqlx::Error),
    Crash(String),
}

impl From<sqlx::Error> for StripError {
    fn from(err: sqlx::Error) -> Self {
        Self::Sql(err)
    }
}

/// 作りかけのバックアップからパスワードを除いて `VACUUM` する（§6.4）。
async fn strip_secrets(
    conn: &mut SqliteConnection,
    crash_after: Option<BackupStep>,
) -> Result<(), StripError> {
    // `password` は 0014（postgres）から。それより古い DB（列が無い）には除くものが無い。
    if column_exists(&mut *conn, "plc_connections", "password").await? {
        sqlx::query("UPDATE plc_connections SET password = NULL WHERE password IS NOT NULL")
            .execute(&mut *conn)
            .await?;
    }
    // キューのペイロード（状態を問わず）から password を外す。`pending_changes` は Hub だけの
    // 表（ChronoGazer には無い）。読めない JSON の行は中身ごと落とす（バックアップの中だけ。
    // 平文を含みうるものを残さない）。
    let has_queue = table_exists(&mut *conn, "pending_changes").await?;
    if has_queue {
        sqlx::query("UPDATE pending_changes SET payload = '{}' WHERE json_valid(payload) = 0")
            .execute(&mut *conn)
            .await?;
        sqlx::query(
            "UPDATE pending_changes \
             SET payload = json_remove(payload, '$.input.password', '$.password') \
             WHERE json_type(payload, '$.input.password') IS NOT NULL \
                OR json_type(payload, '$.password') IS NOT NULL",
        )
        .execute(&mut *conn)
        .await?;
    }
    if crash_after == Some(BackupStep::AfterStrip) {
        return Err(StripError::Crash(
            "（テスト）パスワードを除いた後で落ちた".to_string(),
        ));
    }
    sqlx::query("VACUUM").execute(&mut *conn).await?;
    if crash_after == Some(BackupStep::AfterVacuum) {
        return Err(StripError::Crash(
            "（テスト）VACUUM の後で落ちた".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASSWORD: &str = "TOPSECRET-pw-7f3a";

    fn names(dir: &Path) -> Vec<String> {
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

    /// banto-tags 0001〜0024 だけの DB（`pending_changes` は Hub の表なので、`queue` のときだけ
    /// 作って平文のパスワードを持つ行を入れる）。
    async fn db_before_0025(dir: &Path, queue: bool) -> (PathBuf, SqlitePool) {
        let path = dir.join("app.sqlite3");
        let pool = banto_storage::connect_sqlite(&path).await.unwrap();
        let migrations = Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
        let mut migrator = sqlx::migrate::Migrator::new(migrations.as_path())
            .await
            .unwrap();
        migrator.dangerous_set_table_name(crate::MIGRATIONS_TABLE);
        migrator.run_to(24, &pool).await.unwrap();
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "INSERT INTO plc_connections (id, name, protocol, host, port, unit_id, enabled, \
             simulation, word_order, database, username, password, cpu_type, settings) \
             VALUES (1, 'pg', 'postgres', 'db.local', 5432, 1, 1, 0, 'low_high', 'plant', \
             'reader', '{PASSWORD}', 'R', NULL)"
        )))
        .execute(&pool)
        .await
        .unwrap();
        if queue {
            sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
                "CREATE TABLE pending_changes (id INTEGER PRIMARY KEY, payload TEXT NOT NULL); \
                 INSERT INTO pending_changes (payload) VALUES \
                 ('{{\"input\":{{\"name\":\"q\",\"password\":\"{PASSWORD}\"}}}}'), ('not json {PASSWORD}')"
            )))
            .execute(&pool)
            .await
            .unwrap();
        }
        (path, pool)
    }

    #[test]
    fn leftover_names_cover_dry_runs_and_partial_backups_only() {
        let db = "app.sqlite3";
        for name in [
            ".banto-hub-0025-dryrun-0011223344556677.sqlite3",
            ".banto-hub-0025-dryrun-0011223344556677.sqlite3-journal",
            "app.sqlite3.before-0025-20261021T190331Z.bak.partial",
            "app.sqlite3.before-0025-20261021T190331Z.bak.partial-journal",
            "app.sqlite3.before-0025-20261021T190331Z-2.bak.partial",
        ] {
            assert!(is_leftover(name, db), "{name}");
        }
        for name in [
            "app.sqlite3",
            "app.sqlite3-wal",
            "app.sqlite3.before-0025-20261021T190331Z.bak",
            "other.sqlite3.before-0025-20261021T190331Z.bak.partial",
            "notes.bak.partial",
        ] {
            assert!(!is_leftover(name, db), "{name}");
        }
    }

    #[test]
    fn remove_leftovers_deletes_only_leftovers_and_stops_on_one_it_cannot_delete() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("app.sqlite3");
        for name in [
            "app.sqlite3",
            "app.sqlite3.before-0025-20261021T000000Z.bak",
            "app.sqlite3.before-0025-20261021T000001Z.bak.partial",
            "app.sqlite3.before-0025-20261021T000001Z.bak.partial-journal",
        ] {
            std::fs::write(dir.path().join(name), b"x").unwrap();
        }
        assert_eq!(remove_leftovers(&db).unwrap().len(), 2);
        assert_eq!(
            names(dir.path()),
            vec![
                "app.sqlite3".to_string(),
                "app.sqlite3.before-0025-20261021T000000Z.bak".to_string()
            ]
        );
        let stuck = dir
            .path()
            .join("app.sqlite3.before-0025-20261021T000002Z.bak.partial");
        std::fs::create_dir(&stuck).unwrap();
        match remove_leftovers(&db) {
            Err(UpgradeError::LeftoverNotRemoved { path, .. }) => assert_eq!(path, stuck),
            other => panic!("expected LeftoverNotRemoved, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn pending_and_rows_to_protect_follow_the_migration_state() {
        let dir = tempfile::tempdir().unwrap();
        let fresh = banto_storage::connect_sqlite(dir.path().join("fresh.sqlite3"))
            .await
            .unwrap();
        assert!(
            !pending_0025(&fresh).await.unwrap(),
            "a new DB has no record table"
        );
        assert!(!has_rows_to_protect(&fresh).await.unwrap());
        fresh.close().await;

        let (_, pool) = db_before_0025(dir.path(), false).await;
        assert!(pending_0025(&pool).await.unwrap());
        assert!(has_rows_to_protect(&pool).await.unwrap());
        crate::migrate(&pool).await.unwrap();
        assert!(!pending_0025(&pool).await.unwrap());
        pool.close().await;
    }

    /// 完成したバックアップにパスワードの平文は無い（接続の列も、あれば Hub のキューも、空き
    /// ページにも）。`pending_changes` が無い DB（ChronoGazer）でも作れる。
    #[tokio::test]
    async fn the_backup_holds_no_password_bytes_with_or_without_a_queue() {
        for queue in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let (path, pool) = db_before_0025(dir.path(), queue).await;
            let backup = backup_before_0025(&pool, &path).await.expect("backup");
            let bytes = std::fs::read(&backup).unwrap();
            assert!(!contains(&bytes, PASSWORD), "queue={queue}");
            assert!(
                !names(dir.path())
                    .iter()
                    .any(|n| n.ends_with(PARTIAL_SUFFIX)),
                "{:?}",
                names(dir.path())
            );
            let opened = banto_storage::connect_sqlite(&backup).await.unwrap();
            let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM plc_connections")
                .fetch_one(&opened)
                .await
                .unwrap();
            assert_eq!(rows, 1);
            opened.close().await;
            // The live DB is untouched.
            let live: Option<String> =
                sqlx::query_scalar("SELECT password FROM plc_connections WHERE id = 1")
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(live.as_deref(), Some(PASSWORD));
            pool.close().await;
        }
    }

    /// どの工程の直後に落ちても、残るのは `.partial` の名前のものだけで、次の掃除が消す。
    #[tokio::test]
    async fn a_crash_between_backup_steps_leaves_only_a_partial() {
        for step in [
            BackupStep::AfterVacuumInto,
            BackupStep::AfterStrip,
            BackupStep::AfterVacuum,
            BackupStep::BeforeRename,
        ] {
            let dir = tempfile::tempdir().unwrap();
            let (path, pool) = db_before_0025(dir.path(), true).await;
            let err = backup_with(&pool, &path, Some(step))
                .await
                .expect_err("the injected crash");
            assert!(matches!(err, UpgradeError::Backup { .. }), "{err:?}");
            assert!(
                !names(dir.path()).iter().any(|n| n.ends_with(BACKUP_SUFFIX)),
                "{step:?}: {:?}",
                names(dir.path())
            );
            assert_eq!(remove_leftovers(&path).unwrap().len(), 1, "{step:?}");
            let backup = backup_before_0025(&pool, &path).await.unwrap();
            assert!(
                !contains(&std::fs::read(backup).unwrap(), PASSWORD),
                "{step:?}"
            );
            pool.close().await;
        }
    }

    // ---- 握った置き場（#623 PR3b） -----------------------------------------------------------

    fn root_of(dir: &Path, mode: banto_safe_root::Mode) -> banto_safe_root::SafeRoot {
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

    fn modes() -> Vec<banto_safe_root::Mode> {
        let mut modes = vec![banto_safe_root::Mode::Plain];
        if cfg!(windows) {
            modes.push(banto_safe_root::Mode::Guarded);
        }
        modes
    }

    /// 作りかけ・その付随（`-journal` など）の名前。
    fn partial_names(dir: &Path) -> Vec<String> {
        names(dir)
            .into_iter()
            .filter(|n| n.contains(PARTIAL_SUFFIX) || n.contains(DRYRUN_PREFIX))
            .collect()
    }

    /// 握った置き場でも、バックアップは `.bak` を作り、パスワードの平文を持たず、作りかけと
    /// `-journal` を残さない（Plain はどの OS でも、Windows は Guarded でも）。
    #[tokio::test]
    async fn a_managed_backup_makes_a_bak_and_leaves_no_partial_or_journal() {
        for mode in modes() {
            let dir = tempfile::tempdir().unwrap();
            let (_, pool) = db_before_0025(dir.path(), true).await;
            let root = root_of(dir.path(), mode);
            let place = Place::managed(root.as_dir(), "app.sqlite3", None);
            let backup = backup_before_0025(&pool, place).await.expect("backup");
            assert!(
                backup.to_string_lossy().ends_with(BACKUP_SUFFIX),
                "{mode:?}: {backup:?}"
            );
            assert!(
                !contains(&std::fs::read(&backup).unwrap(), PASSWORD),
                "{mode:?}"
            );
            assert!(
                partial_names(dir.path()).is_empty(),
                "{mode:?}: {:?}",
                names(dir.path())
            );
            assert!(
                !names(dir.path()).iter().any(|n| n.ends_with("-journal")),
                "{mode:?}: {:?}",
                names(dir.path())
            );
            // 同じ秒の 2 本目は連番を足し、1 本目を上書きしない。
            let second = backup_before_0025(&pool, place).await.expect("second");
            assert_ne!(second, backup, "{mode:?}");
            assert!(backup.exists() && second.exists(), "{mode:?}");
            pool.close().await;
        }
    }

    /// 握った置き場でも、どの工程の直後に落ちても残るのは `.partial` の名前のものだけで、
    /// 次の掃除（握ったハンドルの一覧から消す）が消す。
    #[tokio::test]
    async fn a_managed_crash_between_backup_steps_is_removed_by_the_next_start() {
        for mode in modes() {
            for step in [
                BackupStep::AfterVacuumInto,
                BackupStep::AfterStrip,
                BackupStep::AfterVacuum,
                BackupStep::BeforeRename,
            ] {
                let dir = tempfile::tempdir().unwrap();
                let (_, pool) = db_before_0025(dir.path(), true).await;
                let root = root_of(dir.path(), mode);
                let place = Place::managed(root.as_dir(), "app.sqlite3", None);
                let err = backup_with(&pool, place, Some(step))
                    .await
                    .expect_err("the injected crash");
                assert!(matches!(err, UpgradeError::Backup { .. }), "{err:?}");
                assert_eq!(
                    partial_names(dir.path()).len(),
                    1,
                    "{mode:?} {step:?}: {:?}",
                    names(dir.path())
                );
                assert!(
                    !names(dir.path()).iter().any(|n| n.ends_with(BACKUP_SUFFIX)),
                    "{mode:?} {step:?}"
                );
                assert_eq!(
                    remove_leftovers(place).unwrap().len(),
                    1,
                    "{mode:?} {step:?}"
                );
                assert!(partial_names(dir.path()).is_empty(), "{mode:?} {step:?}");
                let backup = backup_before_0025(&pool, place).await.unwrap();
                assert!(
                    !contains(&std::fs::read(backup).unwrap(), PASSWORD),
                    "{mode:?} {step:?}"
                );
                pool.close().await;
            }
        }
    }

    /// 握った置き場でも、残り物の名前をフォルダが塞いでいたら起動を止める（黙って飛ばさない）。
    /// 完成した `.bak` と DB は消さない。
    #[test]
    fn a_managed_leftover_name_blocked_by_a_directory_stops_the_start() {
        for mode in modes() {
            let dir = tempfile::tempdir().unwrap();
            for name in [
                "app.sqlite3",
                "app.sqlite3.before-0025-20261021T000000Z.bak",
                "app.sqlite3.before-0025-20261021T000001Z.bak.partial",
                ".banto-hub-0025-dryrun-00.sqlite3-journal",
            ] {
                std::fs::write(dir.path().join(name), b"x").unwrap();
            }
            let root = root_of(dir.path(), mode);
            let place = Place::managed(root.as_dir(), "app.sqlite3", None);
            assert_eq!(remove_leftovers(place).unwrap().len(), 2, "{mode:?}");
            assert_eq!(
                names(dir.path()),
                vec![
                    "app.sqlite3".to_string(),
                    "app.sqlite3.before-0025-20261021T000000Z.bak".to_string()
                ],
                "{mode:?}"
            );
            let stuck = "app.sqlite3.before-0025-20261021T000002Z.bak.partial";
            std::fs::create_dir(dir.path().join(stuck)).unwrap();
            match remove_leftovers(place) {
                Err(UpgradeError::LeftoverNotRemoved { path, .. }) => {
                    assert_eq!(path.file_name().unwrap(), stuck, "{mode:?}")
                }
                other => panic!("{mode:?}: expected LeftoverNotRemoved, got {other:?}"),
            }
            assert!(dir.path().join(stuck).is_dir(), "{mode:?}");
        }
    }

    /// Windows の Guarded: 握っている間、作りかけの名前は外から改名も削除もできない。
    /// 離した後（`finish`）は握ったフォルダのハンドル基準で消える。
    #[cfg(windows)]
    #[test]
    fn a_guarded_partial_cannot_be_renamed_or_removed_while_held() {
        let dir = tempfile::tempdir().unwrap();
        let root = root_of(dir.path(), banto_safe_root::Mode::Guarded);
        let place = Place::managed(root.as_dir(), "app.sqlite3", None);
        let name = "app.sqlite3.before-0025-20261021T000000Z.bak.partial";
        let partial = TempFile::create(place, name).expect("pin the partial");
        let path = dir.path().join(name);
        assert!(path.is_file());
        assert!(
            std::fs::rename(&path, dir.path().join("moved")).is_err(),
            "握っている名前を改名できた"
        );
        assert!(
            std::fs::remove_file(&path).is_err(),
            "握っている名前を消せた"
        );
        partial.finish().expect("remove after release");
        assert!(!path.exists());
    }
}
