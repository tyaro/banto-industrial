//! SQLite の DB を、本体・`-wal`・`-shm` の名前を**握ったまま**開く口（#623 PR2a。
//! `sqlite` feature）。
//!
//! # 何を守るか
//!
//! SQLite はパスで開き直し、`-wal`・`-shm` を自分でパスから作る。[`ManagedDir::reopen_path`]
//! で確かめてから渡すと、確かめてから SQLite が開くまでの間に、名前を変える・消す・リンクに
//! 置き換える窓が残る（[`crate::root`] の「守れないこと」）。ここでは先に 3 つの名前を
//! [`ManagedDir::pin_file`]（運用モードは [`ManagedDir::pin_file_checked`]）で**作って握り**、
//! 握ったままパスを sqlx に渡す（[`PinnedSqlite`]）。握っている間、3 つの名前は他者から
//! 変更・削除・置換できない（[`PinnedName`] の契約）。
//!
//! # `SQLITE_FCNTL_PERSIST_WAL`
//!
//! 握った名前は SQLite も消せない。WAL の最後の接続が閉じるとき SQLite は `-wal`・`-shm` を
//! 消しに行き、共有違反を数回待ってやり直してから諦める（`close` 1 回に約 3 秒。【実測】
//! `root::win_tests` の `sqlite_with_pins`）。そこで、握っているときは接続ごとに
//! `SQLITE_FCNTL_PERSIST_WAL` を立てる（[`set_persist_wal`]。[`PinnedSqlite::pool_options`] の
//! `after_connect`）。立てられなければ（`SQLITE_OK` でない）その接続を失敗にする（黙って
//! 3 秒待つ形へ戻さない）。消しに行かないので、閉じた後もチェックポイント済みの `-wal`
//! （`journal_size_limit` が 0 以上なので、閉じるとき SQLite が 0 バイトまで切り詰める。
//! [`JOURNAL_SIZE_LIMIT`]）と `-shm` が残る。次に開くときは SQLite がそのまま使う。
//!
//! # `-journal`
//!
//! WAL では `-journal` を使わないので、**握らず、作らない**。ただし以前の
//! [`ManagedDir::reopen_path`] の確かめ（在るならリンク・reparse point・フォルダでないこと）は
//! 落とさない（[`ManagedDir::pin_sqlite`] が 3 つを握った後に確かめる）。握らないので、確かめた
//! 後に他者が `-journal` を作る・差し替える窓は残る（`journal_mode` を WAL から変えない限り
//! SQLite は開かない。WAL へ切り替える前の古い DB の hot journal だけが読まれうる）。SQLite の
//! 一時ファイル（`etilqs_*`）は TEMP に作り、この木には作らない。
//!
//! # 握る期間
//!
//! [`PinnedSqlite`] は、最初の接続より前から、[`sqlx::SqlitePool::close`] が返った後まで
//! 生かす（呼ぶ側の責任。pool は握りを持たない）。先に握りを落とすと、残った接続が開いて
//! いる間はその接続自身のハンドル（SQLite は削除なしの共有で開く。【実測】握らずに開いた
//! Hub の稼働中も 3 つの名前は改名・削除できなかった）が名前を守るが、pool が
//! 新しい接続を開くときは窓が戻る。
//!
//! # [`crate::root::Mode::Plain`]
//!
//! 何も握らず（[`PinnedName::is_held`] が偽）、`SQLITE_FCNTL_PERSIST_WAL` も立てない（閉じると
//! SQLite がいつもどおり `-wal`・`-shm` を消す）。3 つの名前は `pin_file` と同じく無ければ空で
//! 作る（空の `-wal`・`-shm` は SQLite がそのまま使う）。

use std::ffi::c_int;
use std::path::Path;

use sqlx::sqlite::{
    SqliteConnectOptions, SqliteConnection, SqliteJournalMode, SqlitePool, SqlitePoolOptions,
};
use sqlx::{ConnectOptions, Connection};

use crate::acl::DirAccess;
use crate::error::Result;
use crate::root::{ManagedDir, PinnedName};

/// `-wal` の大きさの上限（バイト、64 MiB）。SQLite は `journal_size_limit` が 0 以上なら、
/// PERSIST_WAL で閉じるとき `-wal` を 0 バイトまで切り詰め（`sqlite3WalClose` の
/// `walLimitSize(pWal, 0)`）、稼働中はチェックポイントで WAL を先頭から使い直すときにこの上限まで
/// 切り詰める。立てないと PERSIST_WAL の `-wal` は切り詰められず、大きな書き込みの後の大きさのまま
/// 残る。Plain でも同じ（Plain は閉じると SQLite が `-wal` を消す）。
pub const JOURNAL_SIZE_LIMIT: i64 = 64 * 1024 * 1024;

/// WAL のファイルの接尾辞。
pub const WAL_SUFFIX: &str = "-wal";
/// 共有メモリのファイルの接尾辞。
pub const SHM_SUFFIX: &str = "-shm";
/// ロールバックの journal の接尾辞（握らず作らない。在れば確かめる）。
pub const JOURNAL_SUFFIX: &str = "-journal";

/// 本体・`-wal`・`-shm` を握った組（[`ManagedDir::pin_sqlite`]・
/// [`ManagedDir::pin_sqlite_checked`]）。drop で 3 つとも閉じる。
#[derive(Debug)]
pub struct PinnedSqlite {
    db: PinnedName,
    wal: PinnedName,
    shm: PinnedName,
}

impl ManagedDir {
    /// SQLite の DB `name` の本体・`-wal`・`-shm` を、無ければ空で作って握る
    /// （[`ManagedDir::pin_file`] を 3 回。ファイルの記述子は確かめない）。続けて `-journal` を
    /// [`ManagedDir::reopen_path`] と同じく確かめる（在るならリンク・reparse point・フォルダで
    /// ないこと。握らず作らない）。運用モードの呼び出し側は [`ManagedDir::pin_sqlite_checked`]。
    pub fn pin_sqlite(&self, name: &str) -> Result<PinnedSqlite> {
        self.pin_sqlite_with(name, None)
    }

    /// [`ManagedDir::pin_sqlite`] の、[`ManagedDir::pin_file_checked`]（このフォルダと各ファイルの
    /// 記述子が `expected` の形か）で握る版。`-journal` の確かめは
    /// [`ManagedDir::reopen_path_checked`]。
    pub fn pin_sqlite_checked(&self, name: &str, expected: &DirAccess) -> Result<PinnedSqlite> {
        self.pin_sqlite_with(name, Some(expected))
    }

    fn pin_sqlite_with(&self, name: &str, expected: Option<&DirAccess>) -> Result<PinnedSqlite> {
        let pin = |name: &str| -> Result<PinnedName> {
            let pinned = match expected {
                Some(expected) => self.pin_file_checked(name, true, expected)?,
                None => self.pin_file(name, true)?,
            };
            Ok(pinned.expect("create が真なら None にならない"))
        };
        let db = pin(name)?;
        let wal = pin(&format!("{name}{WAL_SUFFIX}"))?;
        let shm = pin(&format!("{name}{SHM_SUFFIX}"))?;
        match expected {
            Some(expected) => self.reopen_path_checked(name, &[JOURNAL_SUFFIX], expected)?,
            None => self.reopen_path(name, &[JOURNAL_SUFFIX])?,
        };
        Ok(PinnedSqlite { db, wal, shm })
    }
}

impl PinnedSqlite {
    /// 本体のパス（[`ManagedDir::final_path`] + 名前）。sqlx にはこのパスを渡す。
    pub fn path(&self) -> &Path {
        self.db.path()
    }

    /// `-wal` のパス。
    pub fn wal_path(&self) -> &Path {
        self.wal.path()
    }

    /// `-shm` のパス。
    pub fn shm_path(&self) -> &Path {
        self.shm.path()
    }

    /// 3 つの名前を削除なしの共有で握っているか（[`crate::root::Mode::Guarded`]）。
    pub fn is_held(&self) -> bool {
        self.db.is_held() && self.wal.is_held() && self.shm.is_held()
    }

    /// 開き方（banto-storage の `connect` と同じ: `create_if_missing`・WAL・外部キー。
    /// それに `journal_size_limit` = [`JOURNAL_SIZE_LIMIT`]。sqlx の `pragma` は接続を開くたびに流す）。
    pub fn connect_options(&self) -> SqliteConnectOptions {
        SqliteConnectOptions::new()
            .filename(self.path())
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .foreign_keys(true)
            .pragma("journal_size_limit", JOURNAL_SIZE_LIMIT.to_string())
    }

    /// pool の設定（既定の [`SqlitePoolOptions`]）。握っているときは `after_connect` で接続ごとに
    /// [`set_persist_wal`] を立てる（失敗はその接続の失敗）。
    pub fn pool_options(&self) -> SqlitePoolOptions {
        let options = SqlitePoolOptions::new();
        if self.is_held() {
            options.after_connect(|conn, _meta| Box::pin(set_persist_wal(conn)))
        } else {
            options
        }
    }

    /// [`PinnedSqlite::connect_options`] と [`PinnedSqlite::pool_options`] で pool を開く。
    /// 返った pool は握りを持たないので、この組は pool を閉じ終えるまで生かす（モジュールの
    /// doc の「握る期間」）。
    ///
    /// 握っているときは、pool の前に 1 本だけ開いて [`set_persist_wal`] を確かめ、閉じる。
    /// 立てられなければその失敗をすぐ返す。【実測・sqlx 0.9】pool の `after_connect` の失敗は
    /// `acquire_timeout`（既定 30 秒）まで繰り返しやり直され、最後は理由の無い
    /// [`sqlx::Error::PoolTimedOut`] になるため（`tests` の
    /// `a_failed_after_connect_only_times_out_the_pool`）。
    pub async fn connect(&self) -> std::result::Result<SqlitePool, sqlx::Error> {
        if self.is_held() {
            let mut probe = self.connect_options().connect().await?;
            let set = set_persist_wal(&mut probe).await;
            // 立てられなかったときは `close` が `-wal`・`-shm` を消しに行き数秒待つが、失敗の
            // 経路なので待つ。閉じる失敗は捨てる（理由は `set` の方を返す）。
            let _ = probe.close().await;
            set?;
        }
        self.pool_options()
            .connect_with(self.connect_options())
            .await
    }
}

/// 接続の `main` の DB に `SQLITE_FCNTL_PERSIST_WAL` を立てる（閉じるとき `-wal`・`-shm` を
/// 消しに行かない）。`SQLITE_OK` でなければ [`sqlx::Error::Configuration`]（DB を持たない
/// `:memory:` などでは `SQLITE_NOTFOUND`）。
pub async fn set_persist_wal(conn: &mut SqliteConnection) -> std::result::Result<(), sqlx::Error> {
    let mut handle = conn.lock_handle().await?;
    let mut on: c_int = 1;
    // SAFETY: ロックした接続の生のハンドル（ロックの間は他から使われない）。`"main"` は
    // NUL 終端。`on` は呼び出しの間生きる（PERSIST_WAL は `int*` を読み書きする）。
    let rc = unsafe {
        libsqlite3_sys::sqlite3_file_control(
            handle.as_raw_handle().as_ptr(),
            c"main".as_ptr(),
            libsqlite3_sys::SQLITE_FCNTL_PERSIST_WAL,
            (&mut on as *mut c_int).cast(),
        )
    };
    if rc != libsqlite3_sys::SQLITE_OK {
        return Err(sqlx::Error::Configuration(
            format!("SQLITE_FCNTL_PERSIST_WAL を立てられませんでした（rc = {rc}）").into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
