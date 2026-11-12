//! 書き手の置き場（#623 PR4）: データファイルを置くフォルダを、**パス**で受けるか、
//! **握ったフォルダ**（[`ManagedDir`]）で受けるか。
//!
//! # なぜ握ったフォルダで受けるか
//!
//! LocalSystem のサービス（ChronoGazer の `run-service`）は、利用者が触れるかもしれない木の
//! 中に日付ファイルを書く。パスのまま開くと、利用者が植えたリンク（ジャンクション・
//! シンボリックリンク・ハードリンク）を辿って木の外へ書きうる。[`DataPlace::Managed`] では:
//!
//! - **列挙**は握ったハンドルの [`ManagedDir::list`]（ファイルで reparse point でないものだけを、
//!   パスでの列挙と同じ名前の規則 [`parse_data_file_info`] で選ぶ）。
//! - **開く**ファイルは本体・`-wal`・`-shm` を [`ManagedDir::pin_sqlite`]（`expected` があれば
//!   [`ManagedDir::pin_sqlite_checked`]）で、無ければ空で作って**削除なしの共有で握り**、握った
//!   まま [`PinnedSqlite::connect`]（握っているときは接続ごとに PERSIST_WAL）で pool を開く。
//!   握りは pool と一緒に [`WritableFile`] に持ち、**pool を閉じ終えた後に**離す
//!   （[`WritableFile::close`]。日付の切り替え・設定の不一致での付け替え・`close`）。握っている
//!   間、その 3 つの名前は他者から変更・削除・リンクへの置き換えができない。
//! - **保持期間の削除**は握ったハンドル基準の [`ManagedDir::remove`]（開いた後のハンドルで
//!   reparse point・フォルダでないことを確かめ直してから消す）。本体に加えて、PERSIST_WAL の
//!   ため閉じた後も残る `-wal`・`-shm` と、在れば `-journal` も消す（[`prune_managed_files_with`]）。
//!
//! # 開き方の違い
//!
//! パスの版（[`DataPlace::Path`]）は今までどおり `schema::connect_writable`（`create_if_missing`・
//! WAL・外部キー）。握った版は [`PinnedSqlite::connect_options`]（同じ 3 つに
//! `journal_size_limit` = 64 MiB を足したもの。PERSIST_WAL で閉じるとき `-wal` を 0 バイトに
//! 切り詰めるため）。ファイルの中身（スキーマ・WAL）は同じで、読み手（`TsReader`・
//! banto-tsquery の読み取り専用の open）はどちらで書いたファイルも同じに読む。
//!
//! # 守れないこと
//!
//! - **握っていない名前**: `-journal`（WAL では使わない。在ればリンクでないかを握る時点で
//!   確かめるだけ）と SQLite の一時ファイル。
//! - **握る前・閉じた後**: 開いていない日付のファイルは握らない（保持期間の削除は消す直前に
//!   開いたハンドルで確かめ直す）。
//! - **書き手を `close` せずに drop したとき**: pool は閉じ終えを待たずに捨てられ、握りも
//!   続けて離れる（`Inner` のフィールドの順。pool が後片付けで接続を閉じるまでの間は、
//!   接続自身のハンドルが名前を守る）。
//! - 残りは [`banto_safe_root::root`] の「守れないこと」と [`PinnedSqlite`] の doc と同じ。
//!
//! # 削除の途中の失敗
//!
//! 1 つの日付ファイルは「`-wal` → `-shm` → `-journal` → 本体」の順に消す。**本体を最後に
//! 消す**ので、付随のファイルのどれかが消せなければ（共有違反・リンクに置き換えられていた
//! など）その時点で `Err` を返し、本体は残る（次の削除でやり直す。本体は列挙の手がかり
//! なので、本体だけ消えて付随が取り残されることはない）。付随を先に消すので、本体の削除が
//! 失敗すると本体だけが残る（`-wal` に未反映の行があれば失われるが、消す対象の日付なので
//! 中身ごと消える予定のもの。PERSIST_WAL で閉じた後の `-wal` は反映済みで 0 バイト）。読み手が
//! 開いている日付は、SQLite が削除なしの共有で開くので消せずに `Err` になる。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use banto_safe_root::root::{EntryKind, ManagedDir};
use banto_safe_root::sqlite::{PinnedSqlite, JOURNAL_SUFFIX, SHM_SUFFIX, WAL_SUFFIX};
use banto_safe_root::DirAccess;
use sqlx::SqlitePool;

use crate::date::LocalDate;
use crate::error::TstoreError;
use crate::files::{
    list_data_files, parse_data_file_info, plan_prune_candidates, DataFileInfo, PruneReport,
};
use crate::schema;

/// 書き手の置き場（モジュールの doc）。`TsWriter::open`（`&Path`）は [`DataPlace::Path`]。
#[derive(Clone, Debug)]
pub enum DataPlace {
    /// パスで開く（デスクトップ・Hub・テスト。今までの挙動）。
    Path(PathBuf),
    /// 握ったフォルダの中で、名前 1 要素ずつ辿らずに扱う（サービス）。
    Managed {
        dir: Arc<ManagedDir>,
        /// 握るファイルとこのフォルダの記述子の形（運用モードの形）。`Some` なら
        /// [`ManagedDir::pin_sqlite_checked`] で握る。`None` は確かめずに握る。
        expected: Option<DirAccess>,
    },
}

impl DataPlace {
    /// 表示用のパス（Managed は握ったハンドルの本当のパス）。開き直すのには使わない。
    pub fn path(&self) -> &Path {
        match self {
            DataPlace::Path(path) => path,
            DataPlace::Managed { dir, .. } => dir.final_path(),
        }
    }

    /// 書き手を開く前の準備。Path はフォルダを作る（今までどおり）。Managed は既に在る
    /// （握れている）ので何もしない。
    pub(crate) fn prepare(&self) -> Result<(), TstoreError> {
        match self {
            // Tiny, one-shot blocking call (directory creation, not the hot
            // append path) - not worth pulling in tokio's `fs` feature for
            // (same reasoning as `files.rs`'s module doc for why that module
            // stays fully synchronous).
            DataPlace::Path(path) => Ok(std::fs::create_dir_all(path)?),
            DataPlace::Managed { .. } => Ok(()),
        }
    }

    /// 認識できるデータファイルを `(date, seq)` の昇順で（[`list_data_files`] と同じ規則）。
    /// Managed は握ったハンドルの一覧から、ファイルで reparse point でないものだけを選ぶ
    /// （`path` は握ったハンドルの本当のパス + 名前）。
    pub fn list_data_files(&self) -> Result<Vec<DataFileInfo>, TstoreError> {
        match self {
            DataPlace::Path(path) => list_data_files(path),
            DataPlace::Managed { dir, .. } => list_managed(dir),
        }
    }

    /// `date` の最も大きい連番のファイル（[`crate::files`] の `latest_file_for_date` と同じ）。
    pub(crate) fn latest_file_for_date(
        &self,
        date: LocalDate,
    ) -> Result<Option<DataFileInfo>, TstoreError> {
        match self {
            DataPlace::Path(path) => crate::files::latest_file_for_date(path, date),
            DataPlace::Managed { dir, .. } => Ok(list_managed(dir)?
                .into_iter()
                .filter(|f| f.date == date)
                .max_by_key(|f| f.seq)),
        }
    }

    /// 保持期間の予行（何も消さない。[`crate::files::plan_prune`] と同じ判定に、`pinned` の
    /// 日付は消さないを足したもの）。
    pub fn plan_prune(
        &self,
        retention_days: u32,
        today: LocalDate,
        pinned: &[LocalDate],
    ) -> Result<PruneReport, TstoreError> {
        match self {
            DataPlace::Path(path) => {
                crate::files::plan_prune_excluding(path, retention_days, today, pinned)
            }
            DataPlace::Managed { dir, .. } => Ok(plan_prune_candidates(
                list_managed(dir)?,
                retention_days,
                today,
                pinned,
            )),
        }
    }

    /// 保持期間を過ぎたファイルを消す（`pinned` の日付は消さない）。Path は今までどおり本体
    /// だけを `std::fs::remove_file` で。Managed は [`prune_managed_files_with`]（付随のファイル
    /// も消す。失敗の扱いはモジュールの doc）。
    pub fn prune_files(
        &self,
        retention_days: u32,
        today: LocalDate,
        pinned: &[LocalDate],
    ) -> Result<PruneReport, TstoreError> {
        match self {
            DataPlace::Path(path) => {
                crate::files::prune_files_excluding(path, retention_days, today, pinned)
            }
            DataPlace::Managed { dir, .. } => {
                prune_managed_files_with(dir, retention_days, today, pinned, &|_| {})
            }
        }
    }

    /// 書き込みで開く（無ければ作る）。Managed は 3 つの名前を握ってから開く。
    pub(crate) async fn open_writable(&self, name: &str) -> Result<WritableFile, TstoreError> {
        match self {
            DataPlace::Path(path) => Ok(WritableFile {
                pool: schema::connect_writable(&path.join(name)).await?,
                pin: None,
            }),
            DataPlace::Managed { dir, expected } => {
                let pin = match expected {
                    Some(expected) => dir.pin_sqlite_checked(name, expected),
                    None => dir.pin_sqlite(name),
                }
                .map_err(|error| {
                    TstoreError::Storage(format!(
                        "データファイルを握れません（{}）: {error}",
                        dir.final_path().join(name).display()
                    ))
                })?;
                // 失敗したら pool は無く、握りはここで離れる。
                let pool = pin.connect().await?;
                Ok(WritableFile {
                    pool,
                    pin: Some(pin),
                })
            }
        }
    }
}

impl From<PathBuf> for DataPlace {
    fn from(path: PathBuf) -> Self {
        DataPlace::Path(path)
    }
}

/// 開いた書き込み先: pool と、Managed ならその握り（[`PinnedSqlite`]）。握りは pool を
/// 閉じ終えるまで生かす（[`WritableFile::close`]）。
pub(crate) struct WritableFile {
    pub(crate) pool: SqlitePool,
    pub(crate) pin: Option<PinnedSqlite>,
}

impl WritableFile {
    /// pool を閉じ終えてから握りを離す。
    pub(crate) async fn close(self) {
        self.pool.close().await;
        drop(self.pin);
    }
}

fn list_managed(dir: &ManagedDir) -> Result<Vec<DataFileInfo>, TstoreError> {
    let mut files: Vec<DataFileInfo> = dir
        .list()
        .map_err(|error| {
            TstoreError::Storage(format!(
                "データフォルダを列挙できません（{}）: {error}",
                dir.final_path().display()
            ))
        })?
        .into_iter()
        .filter(|entry| entry.kind == EntryKind::File && entry.reparse_tag.is_none())
        .filter_map(|entry| parse_data_file_info(dir.final_path(), &entry.name))
        .collect();
    files.sort_by_key(|f| (f.date, f.seq));
    Ok(files)
}

/// 握ったフォルダの保持期間の削除（[`DataPlace::prune_files`] の Managed）。`before_remove` は
/// 各日付ファイルの**本体**を消す直前に名前で呼ぶ（テストで、一覧の後に名前を差し替える
/// ため。本番は何もしない関数を渡す）。
///
/// 1 件ごとに `-wal` → `-shm` → `-journal` → 本体の順に [`ManagedDir::remove`] で消す（付随は
/// 無ければ飛ばす）。どれかが消せない（共有違反・開いた後の属性で断った・I/O の失敗）か、
/// 本体が一覧の後に消えていたら、その時点で `Err`（それまでに消した日付はそのまま。本体を
/// 最後に消す理由はモジュールの doc の「削除の途中の失敗」）。
#[doc(hidden)]
pub fn prune_managed_files_with(
    dir: &ManagedDir,
    retention_days: u32,
    today: LocalDate,
    pinned: &[LocalDate],
    before_remove: &dyn Fn(&str),
) -> Result<PruneReport, TstoreError> {
    let plan = plan_prune_candidates(list_managed(dir)?, retention_days, today, pinned);
    let remove = |name: &str| {
        dir.remove(name).map_err(|error| {
            TstoreError::Storage(format!(
                "保持期間の削除に失敗しました（{}）: {error}",
                dir.final_path().join(name).display()
            ))
        })
    };
    for path in &plan.deleted {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .expect("一覧から選んだ名前は UTF-8");
        for suffix in [WAL_SUFFIX, SHM_SUFFIX, JOURNAL_SUFFIX] {
            remove(&format!("{name}{suffix}"))?;
        }
        before_remove(name);
        if !remove(name)? {
            return Err(TstoreError::Storage(format!(
                "保持期間の削除対象が消えました: {}",
                path.display()
            )));
        }
    }
    Ok(plan)
}

#[cfg(test)]
mod tests;
