//! 握ったルートと管理するフォルダの中を、**辿らずに**扱う I/O（#623 PR-1b。設計 §4・§5）。
//!
//! # 何を守るか
//!
//! LocalSystem のサービスが、利用者が触れるかもしれない木（ChronoGazer の
//! `%ProgramData%\ChronoGazer`、banto-hub の `profiles\<id>`）の中へ書くとき、パスのまま開くと
//! 利用者が植えたジャンクション・シンボリックリンク・ハードリンクを辿って**木の外**へ書く。
//! ここでは次の順で扱う（[`Mode::Guarded`]、Windows）:
//!
//! 1. [`open_root`]: ルートを `FILE_FLAG_OPEN_REPARSE_POINT` で辿らずに開いて**握る**
//!    （`PinnedDir`。削除なし共有なので、握っている間は名前の変更・削除 = ジャンクションへの
//!    すり替えができない）。開いた**ハンドルから**、reparse point でないフォルダ・本当のパスが
//!    期待と同じ（8.3 は長い名前に直して比べる）・既知のフォルダに当たらない・所有者が
//!    SYSTEM・Administrators・指定した利用者のどれか、を確かめる。無ければ（`create`）、親を
//!    辿らずに開いて確かめ（[`guard::check_root_parent`]）、親のハンドルを基準に作る。
//!    ルートの**形（ACL）は条件にしない**（設計 §2.10。モードの ACL は PR-1c）。
//! 2. [`SafeRoot::open_managed`]: 管理するフォルダを 1 段 1 spec（[`ManagedSpec`]）で、
//!    **親の握ったハンドルを基準に**名前 1 要素ずつ開いて握る（無ければ作る）。祖先を後から
//!    すり替えられても外へ出ない。spec の誤り（重複・知らない親・名前）は何も開く前に断る。
//! 3. [`ManagedDir`] の操作: 名前 1 要素を `NtCreateFile` の `RootDirectory`（握ったハンドル）で
//!    相対に、`FILE_OPEN_REPARSE_POINT` で辿らずに開き、**開いた後にハンドルで**属性を読み直して
//!    から使う。消す・名前を付け替える・書き出すのも開いたハンドルで行う（パスを解決し直さない）。
//!
//! 【測定・NTFS】握っている間は、**ルートの祖先**の名前の変更も断られる（部分木の中に開いた
//! ハンドルがあるフォルダの名前の変更・削除を NTFS が断る。API の契約ではないので、テスト
//! `an_ancestor_of_a_held_root_cannot_be_renamed` で固定した。ReFS は未測。Fable B-4-1）。
//!
//! # 守れないこと（[`ReopenPath`] の doc も）
//!
//! SQLite・tstore・banto の BackupService は、渡したパスで後から開き直し、付随のファイル
//! （`-wal`・`-shm`・`-journal`）を自分でパスから作る。[`ManagedDir::reopen_path`] は確かめた
//! 時点のことしか保証しない。そこを守るのは運用モードのルート全体の ACL（PR-1c・PR-3/5）と、
//! 途中のジャンクションへの保険の Redirection Guard（[`crate::redirection`]）。
//!
//! # 対象の OS
//!
//! `FileRenameInfoEx`・`FileDispositionInfoEx` の POSIX の付け替え・削除は Windows 10 1607 以降
//! （1809 以降を推奨。それより古い・対応しないファイルシステムでは POSIX でない版で
//! やり直す）。ハードリンクの mitigation（作るのにリンク先への書き込み権が要る）は 1903 以降、
//! Redirection Guard は Windows 11 22H2 以降・Server 2022 の更新以降（設計 §2.8）。
//!
//! # [`Mode::Plain`]
//!
//! デスクトップ・コンソール・非 Windows 向けに、同じ API を `std::fs` で実装する（デスクトップの
//! 挙動を変えない）。**リンクの確かめはしない**（辿る）。[`SafeRoot::final_path`] は
//! `std::fs::canonicalize` の結果（排他の scope と揃える。Fable C-3）。非 Windows で
//! [`Mode::Guarded`] を求めると [`RootRefusal::UnsupportedPlatform`]。

use std::path::{Path, PathBuf};

use crate::acl::{DirAccess, InstallUserSid, ADMINISTRATORS_SID, SYSTEM_SID};
use crate::error::{IoOp, Refusal, Result, SafeRootError};
use crate::guard::{self, KnownFolders, RealPathRefusal, RootFormProblem, RootParentRefusal};

mod plain;
#[cfg(windows)]
mod win;

#[cfg(test)]
pub(crate) mod tests;
#[cfg(all(test, windows))]
mod win_tests;

/// 握り方（設計 §4.1）。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
    /// 辿らずに開いて握り、ハンドル基準で扱う（Windows のサービス）。
    Guarded,
    /// `std::fs` のまま（デスクトップ・コンソール・非 Windows）。リンクを辿る。
    Plain,
}

/// ルートの仕様（[`open_root`]）。
#[derive(Clone, Debug)]
pub struct RootSpec {
    /// 期待するルートのパス（絶対パス。8.3 の短い名前は長い名前に直して比べる）。
    pub expected: PathBuf,
    /// ルート（とその本当のパス）にしてはいけない既知のフォルダ（[`Mode::Guarded`] だけが使う）。
    pub known: KnownFolders,
    /// SYSTEM・Administrators の他に、ルートと管理するフォルダの**所有者として許す**利用者
    /// （設定モードでデスクトップが作った物の所有者 = インストール利用者。[`Mode::Guarded`]
    /// だけが使う）。
    pub owners: Vec<InstallUserSid>,
    /// ルート・管理するフォルダを**作るときに**付ける保護された DACL（作るのと同時に付ける）。
    /// `None` なら親から継承する。既にある物の ACL は変えない・確かめない（PR-1c の範囲）。
    pub create_access: Option<DirAccess>,
    /// 無ければ作るか。
    pub create: bool,
    /// 握り方。
    pub mode: Mode,
}

/// [`open_root`] が断った理由。断ったときは何も作っていない（ルートを作ったのは確かめの前の
/// 親の確かめに通った後だけ）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RootRefusal {
    /// 期待のパスの形が使えない（空・絶対でない・`.`/`..`）。
    #[error("ルートのパスの形が使えません（{0:?}）")]
    InvalidForm(RootFormProblem),
    /// 期待のパスが既知のフォルダ・ドライブのルート・UNC に当たる。
    #[error("ルートが既知のフォルダに当たります（{0:?}）")]
    WellKnown(guard::WellKnownRootRefusal),
    /// ルートが無く、作らない指定（`create: false`）。
    #[error("ルートがありません")]
    Absent,
    /// 作るために開いた親が無い。
    #[error("ルートの親のフォルダがありません")]
    ParentAbsent,
    /// 作るために開いた親が確かめに通らない。
    #[error("ルートの親のフォルダが使えません（{0:?}）")]
    Parent(RootParentRefusal),
    /// 開いたルートが reparse point・フォルダでない。
    #[error("ルートが使えません: {0}")]
    Refused(Refusal),
    /// 開いたルートの本当のパスが期待と違う・既知のフォルダに当たる。
    #[error("ルートの本当の場所が使えません（{0:?}）")]
    RealPath(RealPathRefusal),
    /// 所有者が SYSTEM・Administrators・[`RootSpec::owners`] のどれでもない。
    #[error("ルートの所有者が想定外です（{0}）")]
    ForeignOwner(String),
    /// この OS では [`Mode::Guarded`] を使えない。
    #[error("この OS ではリンクを辿らずにルートを握れません")]
    UnsupportedPlatform,
    /// 開く・作る・読むのに失敗した。
    #[error("ルートを開けませんでした: {0}")]
    Io(SafeRootError),
}

/// 管理するフォルダ 1 段（設計 §4.1。Fable B-2: 1 段 1 spec、親の key で木を組む）。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ManagedSpec {
    /// 呼ぶ側の名前（`"data"`・`"backups-db"` など。[`ManagedSet::get`] の key）。
    pub key: &'static str,
    /// 親の key（`None` ならルートの直下）。**先に並べた spec の key** でなければならない。
    pub parent: Option<&'static str>,
    /// 名前 1 要素（[`guard::is_plain_child_name`] に通ること）。製品が渡す。
    pub name: &'static str,
    /// 無ければ作るか（[`RootSpec::create_access`] を作るのと同時に付ける）。
    pub create: bool,
}

/// spec の並びの誤り（[`check_specs`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpecProblem {
    /// 同じ key が 2 回ある。
    DuplicateKey,
    /// 親の key が、それより前の spec に無い（知らない・後ろにある）。
    UnknownParent(&'static str),
    /// 名前が 1 要素の普通の名前でない。
    InvalidName,
}

/// 管理するフォルダを開けなかった理由。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManagedRefusal {
    /// spec の並びの誤り（何も開いていない）。
    Spec(SpecProblem),
    /// 無く、作らない指定。
    Absent,
    /// reparse point・フォルダでない・本当のパスが親の中でない。
    Refused(Refusal),
    /// 所有者が SYSTEM・Administrators・[`RootSpec::owners`] のどれでもない。
    ForeignOwner(String),
    /// 開く・作る・読むのに失敗した。
    Io(SafeRootError),
}

/// [`SafeRoot::open_managed`] が止まった spec と理由。止まったら 1 つも返さない（それまでに
/// 開いた物は閉じる。作った物は残る - 作り直しは冪等）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("管理するフォルダ「{key}」を開けませんでした（{reason:?}）")]
pub struct ManagedStop {
    pub key: &'static str,
    pub reason: ManagedRefusal,
}

/// [`ManagedSet::get`] に渡された key が spec に無い。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("管理するフォルダ「{0}」は開いていません")]
pub struct UnknownKey(pub String);

/// spec の並びを確かめる（純関数。何も開かない）: key の重複なし・親は前に並べた key・名前は
/// 1 要素の普通の名前。最初に見つけた誤りを返す。
pub fn check_specs(specs: &[ManagedSpec]) -> std::result::Result<(), ManagedStop> {
    for (index, spec) in specs.iter().enumerate() {
        let earlier = &specs[..index];
        let stop = |problem| ManagedStop {
            key: spec.key,
            reason: ManagedRefusal::Spec(problem),
        };
        if earlier.iter().any(|e| e.key == spec.key) {
            return Err(stop(SpecProblem::DuplicateKey));
        }
        if let Some(parent) = spec.parent {
            if !earlier.iter().any(|e| e.key == parent) {
                return Err(stop(SpecProblem::UnknownParent(parent)));
            }
        }
        if !guard::is_plain_child_name(spec.name) {
            return Err(stop(SpecProblem::InvalidName));
        }
    }
    Ok(())
}

/// 所有者として許すか（純関数）: SYSTEM・Administrators・`owners` のどれか。
pub fn owner_allowed(owner: &str, owners: &[InstallUserSid]) -> bool {
    owner == SYSTEM_SID || owner == ADMINISTRATORS_SID || owners.iter().any(|u| u.as_str() == owner)
}

/// [`ManagedDir::write_atomic`] の一時ファイルの名前（純関数）: `.{name}.tmp-{nonce:08x}`。
/// 要素の上限（UTF-16 で 255）を超える・普通の名前でないなら `None`（Fable C-5）。
pub fn temp_name(name: &str, nonce: u32) -> Option<String> {
    let temp = format!(".{name}.tmp-{nonce:08x}");
    (guard::is_plain_child_name(name)
        && guard::is_plain_child_name(&temp)
        && temp.encode_utf16().count() <= MAX_NAME_UNITS)
        .then_some(temp)
}

/// 名前 1 要素の上限（UTF-16 の単位。NTFS の要素の上限）。
const MAX_NAME_UNITS: usize = 255;

/// [`ManagedDir::write_atomic`] が一時ファイルの名前の衝突で nonce を進めて試す回数。
const TEMP_TRIES: u32 = 16;

/// 一時ファイルの nonce の始まり（プロセス ID・時刻・呼び出しの回数を混ぜる。予測されても
/// 害は「衝突して試し直す・上限で断る」だけ - 一時ファイルは `FILE_CREATE` で作り、既にある
/// 物は開かない）。
fn nonce_seed() -> u32 {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    #[cfg(test)]
    if let Some(seed) = TEST_NONCE_SEED.with(std::cell::Cell::get) {
        return seed;
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    std::process::id()
        .rotate_left(16)
        .wrapping_add(nanos)
        .wrapping_add(COUNTER.fetch_add(0x9E37_79B9, Ordering::Relaxed))
}

// テスト専用: 一時ファイルの nonce の始まりを決め打ちする（衝突の反証）。
#[cfg(test)]
thread_local! {
    pub(crate) static TEST_NONCE_SEED: std::cell::Cell<Option<u32>> =
        const { std::cell::Cell::new(None) };
}

// テスト専用: `remove_where` が一覧を取った後・消す前に呼ぶフック（名前を差し替える反証）。
#[cfg(test)]
pub(crate) type BeforeRemoveHook = Box<dyn Fn(&str)>;
#[cfg(test)]
thread_local! {
    pub(crate) static BEFORE_REMOVE: std::cell::RefCell<Option<BeforeRemoveHook>> =
        const { std::cell::RefCell::new(None) };
}

/// [`ManagedDir::list`] の 1 件（列挙の時点の属性。**判断には使わない** - 消す・開くときは
/// 開いた後にハンドルで読み直す）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub kind: EntryKind,
    /// reparse point なら tag（Plain は 0）。
    pub reparse_tag: Option<u32>,
    /// 大きさ（フォルダは 0 のことがある）。
    pub len: u64,
}

/// [`Entry`] の種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
}

/// [`ManagedDir::remove_where`] の対象の候補か（純関数）: ファイル・reparse point でない・
/// `pred` が名前を選ぶ。候補でも、消す前に開いたハンドルで属性を読み直す（[`ManagedDir::remove`]）。
pub fn is_removal_candidate(entry: &Entry, pred: &dyn Fn(&str) -> bool) -> bool {
    entry.kind == EntryKind::File && entry.reparse_tag.is_none() && pred(&entry.name)
}

/// [`ManagedDir::remove_where`] の結果。
#[derive(Debug, Default)]
pub struct RemoveReport {
    /// 消した名前。
    pub removed: Vec<String>,
    /// 開いた後の属性で断った名前（一覧の後に差し替えられた物など）。触っていない。
    pub refused: Vec<(String, Refusal)>,
    /// 消せなかった名前（共有違反など）。残りは続けて消す。
    pub failed: Vec<(String, SafeRootError)>,
}

/// パスで開き直す相手（SQLite・tstore・banto の BackupService）に渡すパス（設計 §4.4）。
///
/// **残留リスク**: `ReopenPath` は「確かめた時点で、このフォルダ自身が reparse point でなく
/// （握ったハンドルで読み直した属性）、このフォルダの中のこの名前と付随の名前がリンク（reparse
/// point・ハードリンク）でもフォルダでもなかった（無いのも可）」ことしか保証しない。相手は
/// このパスで後から開き直し、付随のファイルを自分でパスから作る。それでも外へ出ないのは、
/// **運用モードの間は利用者がこの木に名前を作れない**（ルート全体の ACL。PR-1c・PR-3/5）
/// からであって、この型の力ではない。フォルダの記述子が運用モードの形かの確かめ直し
/// （設計 §2.9 (d)）は PR-1c で足す。Redirection Guard は途中のジャンクションの保険
/// （ハードリンク・シンボリックリンクには効かない）。[`Mode::Plain`] は何も確かめない。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReopenPath {
    path: PathBuf,
    companions: Vec<PathBuf>,
}

impl ReopenPath {
    /// 本体のパス（[`ManagedDir::final_path`] + 名前。渡された綴りではなく本当のパス基準）。
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 付随の名前のパス（渡した接尾辞の順）。
    pub fn companions(&self) -> &[PathBuf] {
        &self.companions
    }

    /// 本体のパスを取り出す。
    pub fn into_path(self) -> PathBuf {
        self.path
    }
}

enum Inner {
    #[cfg(windows)]
    Guarded(crate::acl::PinnedDir),
    Plain,
}

/// 握ったルート・管理するフォルダ（設計 §4.3）。[`Mode::Guarded`] では辿らずに開いたハンドルを
/// 持ち続け、drop で閉じる。スレッドをまたいで使える（一覧は内部で直列化する）。
pub struct ManagedDir {
    inner: Inner,
    /// 表示用のパス（渡された綴り + 名前。開き直すのには使わない）。
    display: PathBuf,
    /// 本当のパス（Guarded: ハンドルの `GetFinalPathNameByHandleW`、Plain: `canonicalize`）。
    final_path: PathBuf,
    /// 列挙の位置はファイルオブジェクトに付くので、同じハンドルの一覧を直列化する。
    list_lock: std::sync::Mutex<()>,
}

impl std::fmt::Debug for ManagedDir {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManagedDir")
            .field("display", &self.display)
            .field("final_path", &self.final_path)
            .field("guarded", &self.is_guarded())
            .finish_non_exhaustive()
    }
}

impl ManagedDir {
    fn plain(display: PathBuf, final_path: PathBuf) -> Self {
        Self {
            inner: Inner::Plain,
            display,
            final_path,
            list_lock: std::sync::Mutex::new(()),
        }
    }

    #[cfg(windows)]
    fn guarded(pinned: crate::acl::PinnedDir, display: PathBuf) -> Self {
        let final_path = pinned.final_path().to_path_buf();
        Self {
            inner: Inner::Guarded(pinned),
            display,
            final_path,
            list_lock: std::sync::Mutex::new(()),
        }
    }

    /// 表示用のパス（渡された綴り + 名前）。
    pub fn path(&self) -> &Path {
        &self.display
    }

    /// 本当のパス（Guarded: ハンドルの本当のパス、Plain: `canonicalize` の結果）。
    pub fn final_path(&self) -> &Path {
        &self.final_path
    }

    /// 辿らずに握っているか（[`Mode::Guarded`]）。
    pub fn is_guarded(&self) -> bool {
        match self.inner {
            #[cfg(windows)]
            Inner::Guarded(_) => true,
            Inner::Plain => false,
        }
    }

    /// 握っているハンドル（[`Mode::Guarded`] だけ。PR-1c のモードの ACL の部品が使う）。
    #[cfg(windows)]
    pub fn pinned(&self) -> Option<&crate::acl::PinnedDir> {
        match &self.inner {
            Inner::Guarded(pinned) => Some(pinned),
            Inner::Plain => None,
        }
    }

    // 使うのは Windows の Guarded の操作だけ（理由の表示用のパス）。
    #[cfg_attr(not(windows), allow(dead_code))]
    fn display_of(&self, name: &str) -> PathBuf {
        self.display.join(name)
    }

    /// 名前 1 要素の**新しい**ファイルを作って、書き込みで開く（無ければ作る、既にあれば
    /// `Ok(None)` で何も開かない）。Guarded は握ったハンドルを基準に `FILE_CREATE` で作り、
    /// 記述子は親のフォルダから継承する。共有は読み取りだけ。用途: 書き込みの試し（probe）。
    pub fn create_new(&self, name: &str) -> Result<Option<std::fs::File>> {
        match &self.inner {
            #[cfg(windows)]
            Inner::Guarded(pinned) => win::create_new(pinned, name),
            Inner::Plain => plain::create_new(&self.final_path, name),
        }
    }

    /// 名前 1 要素のファイルを追記で開く（`create` なら無ければ作る。偽で無ければ
    /// `io::ErrorKind::NotFound`）。Guarded は**開いた後にハンドルで**、reparse point でない・
    /// フォルダでない・リンク数 1 を確かめ、合わなければ [`SafeRootError::Refused`]。権利は
    /// 追記だけ（書き込みは必ず末尾）。用途: 日次ログ。
    pub fn open_append(&self, name: &str, create: bool) -> Result<std::fs::File> {
        match &self.inner {
            #[cfg(windows)]
            Inner::Guarded(pinned) => {
                win::open_append(pinned, &self.display_of(name), name, create)
            }
            Inner::Plain => plain::open_append(&self.final_path, name, create),
        }
    }

    /// 名前 1 要素のファイルを読み取りで開く（無ければ `Ok(None)`）。Guarded は開いた後に
    /// ハンドルで [`ManagedDir::open_append`] と同じ確かめをする。共有は読み取りだけ（書き込みで
    /// 開いている主体がいれば共有違反で失敗する）。
    pub fn open_read(&self, name: &str) -> Result<Option<std::fs::File>> {
        match &self.inner {
            #[cfg(windows)]
            Inner::Guarded(pinned) => win::open_read(pinned, &self.display_of(name), name),
            Inner::Plain => plain::open_read(&self.final_path, name),
        }
    }

    /// 名前 1 要素のファイルを、中身ごと**原子的に**置き換える（設計 §4.3）。
    ///
    /// 一時ファイル `.{name}.tmp-{nonce}`（[`temp_name`]）を握ったハンドルを基準に `FILE_CREATE`
    /// で作り（名前が衝突したら nonce を進めて最大 16 回）、書いて `FlushFileBuffers` し、
    /// **一時ファイルのハンドルで** `FileRenameInformationEx`（`REPLACE_IF_EXISTS |
    /// POSIX_SEMANTICS`、`RootDirectory` = 握ったこのフォルダのハンドル。ntdll を直接呼ぶ - Win32 の
    /// 口は名前をカレントディレクトリ基準に直すため）で `name` へ付け替える。Guarded は、宛先が
    /// フォルダ（ジャンクション・ディレクトリのシンボリックリンクを含む）なら**何も作らずに**
    /// [`SafeRootError::Refused`] で断る（【実測】POSIX の付け替えはジャンクションを置き換えて
    /// しまうので、付け替えの失敗に任せない。Fable B-6 の補正）。付け替え・書き込みが失敗した
    /// ときは、一時ファイルを作ったハンドルで消してから失敗を返す。宛先がファイルのシンボリック
    /// リンク・ハードリンクなら名前だけが置き換わり、リンクの先・別名の中身は変わらない。
    /// `name` が長すぎて一時名が要素の上限を超えるなら、何もせずに断る（Fable C-5）。
    pub fn write_atomic(&self, name: &str, contents: &[u8]) -> Result<()> {
        if temp_name(name, 0).is_none() {
            return Err(invalid_name());
        }
        match &self.inner {
            #[cfg(windows)]
            Inner::Guarded(pinned) => {
                win::write_atomic(pinned, &self.display_of(name), name, contents)
            }
            Inner::Plain => plain::write_atomic(&self.final_path, name, contents),
        }
    }

    /// 名前 1 要素のファイルを消す（無ければ `Ok(false)`）。Guarded は名前で相対に開いた**後に
    /// ハンドルで**属性を読み直し、reparse point・フォルダなら消さずに
    /// [`SafeRootError::Refused`]（一覧の属性は信じない。Fable B-5）。消すのは開いたハンドルで
    /// （`FileDispositionInfoEx`、POSIX）。
    pub fn remove(&self, name: &str) -> Result<bool> {
        match &self.inner {
            #[cfg(windows)]
            Inner::Guarded(pinned) => win::remove(pinned, &self.display_of(name), name),
            Inner::Plain => plain::remove(&self.final_path, name),
        }
    }

    /// 中の一覧（`.`・`..` を除く。順は OS のまま）。Guarded は握ったハンドルを列挙する
    /// （パスを使わない。その場でジャンクションにされても、列挙はこのフォルダそのものを返す）。
    pub fn list(&self) -> Result<Vec<Entry>> {
        let _serial = self
            .list_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match &self.inner {
            #[cfg(windows)]
            Inner::Guarded(pinned) => win::list(pinned),
            Inner::Plain => plain::list(&self.final_path),
        }
    }

    /// 一覧から [`is_removal_candidate`] が選んだ名前を [`ManagedDir::remove`] で消す（保持期間の
    /// 掃除）。開いた後の属性で断った名前・消せなかった名前は報告して、残りを続ける。
    pub fn remove_where(&self, pred: &dyn Fn(&str) -> bool) -> Result<RemoveReport> {
        let mut report = RemoveReport::default();
        for entry in self.list()? {
            if !is_removal_candidate(&entry, pred) {
                continue;
            }
            #[cfg(test)]
            BEFORE_REMOVE.with(|hook| {
                if let Some(hook) = hook.borrow().as_ref() {
                    hook(&entry.name);
                }
            });
            match self.remove(&entry.name) {
                Ok(true) => report.removed.push(entry.name),
                Ok(false) => {} // 一覧の後に消えた。
                Err(SafeRootError::Refused { refusal, .. }) => {
                    report.refused.push((entry.name, refusal));
                }
                Err(error) => report.failed.push((entry.name, error)),
            }
        }
        Ok(report)
    }

    /// パスで開き直す相手に渡すパスを、確かめてから作る（設計 §4.4・§5。残留リスクは
    /// [`ReopenPath`] の doc）。Guarded は (1) **握ったハンドルで**このフォルダ自身の属性を
    /// 読み直し、reparse point なら断る（締める前の古いハンドルでその場でジャンクションに
    /// された場合。設計 §2.9 (b)）、(2) `name` と `name + 接尾辞`（`"-wal"` など）を相対に
    /// 属性だけで開き、reparse point・フォルダ・ハードリンクなら断る（無いのは可）。返すパスは
    /// [`ManagedDir::final_path`] 基準（渡された綴りではない。Fable B-8-2）。
    pub fn reopen_path(&self, name: &str, companions: &[&str]) -> Result<ReopenPath> {
        let mut names = vec![name.to_string()];
        names.extend(companions.iter().map(|suffix| format!("{name}{suffix}")));
        if names.iter().any(|n| !guard::is_plain_child_name(n)) {
            return Err(invalid_name());
        }
        match &self.inner {
            #[cfg(windows)]
            Inner::Guarded(pinned) => win::check_reopen(pinned, &self.display, &names)?,
            Inner::Plain => {}
        }
        let mut paths = names.iter().map(|n| self.final_path.join(n));
        let path = paths.next().expect("本体の名前がある");
        Ok(ReopenPath {
            path,
            companions: paths.collect(),
        })
    }
}

/// 名前が 1 要素の普通の名前でない（`ERROR_INVALID_NAME` の `io` の失敗）。
fn invalid_name() -> SafeRootError {
    SafeRootError::io(
        IoOp::OpenFile,
        &std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "名前が 1 要素の普通の名前ではありません",
        ),
    )
}

/// 握ったルート（設計 §4.2）。プロセスの終わりまで持つ。
#[derive(Debug)]
pub struct SafeRoot {
    dir: ManagedDir,
    // 読むのは Windows の Guarded の `open_managed` だけ（非 Windows は Plain しか無い）。
    #[cfg_attr(not(windows), allow(dead_code))]
    owners: Vec<InstallUserSid>,
    #[cfg_attr(not(windows), allow(dead_code))]
    create_access: Option<DirAccess>,
}

impl SafeRoot {
    /// 渡されたパス（表示用）。
    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    /// 本当のパス（Guarded: ハンドルの本当のパス、Plain: `canonicalize` の結果）。
    pub fn final_path(&self) -> &Path {
        self.dir.final_path()
    }

    /// ルートそのものを [`ManagedDir`] として扱う（DB・診断ファイルなど、ルート直下の名前）。
    pub fn as_dir(&self) -> &ManagedDir {
        &self.dir
    }

    /// 握っているハンドル（[`Mode::Guarded`] だけ。PR-1c の scan/close/relax が使う）。
    #[cfg(windows)]
    pub fn pinned(&self) -> Option<&crate::acl::PinnedDir> {
        self.dir.pinned()
    }

    /// 管理するフォルダを親から順に開いて握る（無ければ `create` の spec だけ作る）。spec の
    /// 並びは何も開く前に [`check_specs`] で確かめる。どれか 1 つでも開けない・確かめに通らない
    /// なら止めて、1 つも返さない（設計 §4.1「常に起動を止める」）。
    pub fn open_managed(
        &self,
        specs: &[ManagedSpec],
    ) -> std::result::Result<ManagedSet, ManagedStop> {
        check_specs(specs)?;
        let mut dirs: Vec<(&'static str, ManagedDir)> = Vec::with_capacity(specs.len());
        for spec in specs {
            let parent = match spec.parent {
                None => &self.dir,
                Some(key) => {
                    &dirs
                        .iter()
                        .find(|(k, _)| *k == key)
                        .expect("check_specs が親を確かめた")
                        .1
                }
            };
            let opened = self.open_one(parent, spec).map_err(|reason| ManagedStop {
                key: spec.key,
                reason,
            })?;
            dirs.push((spec.key, opened));
        }
        Ok(ManagedSet { dirs })
    }

    fn open_one(
        &self,
        parent: &ManagedDir,
        spec: &ManagedSpec,
    ) -> std::result::Result<ManagedDir, ManagedRefusal> {
        match &parent.inner {
            #[cfg(windows)]
            Inner::Guarded(pinned) => win::open_managed_one(
                pinned,
                &parent.display,
                spec,
                &self.owners,
                self.create_access.as_ref(),
            ),
            Inner::Plain => plain::open_managed_one(parent, spec),
        }
    }
}

/// [`SafeRoot::open_managed`] で開いた管理するフォルダの組。
#[derive(Debug)]
pub struct ManagedSet {
    dirs: Vec<(&'static str, ManagedDir)>,
}

impl ManagedSet {
    /// key の管理するフォルダ（Fable C-2）。
    pub fn get(&self, key: &str) -> std::result::Result<&ManagedDir, UnknownKey> {
        self.dirs
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, dir)| dir)
            .ok_or_else(|| UnknownKey(key.to_string()))
    }

    /// 開いた順の key。
    pub fn keys(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.dirs.iter().map(|(k, _)| *k)
    }
}

/// ルートを開いて握る（モジュール doc の 1）。
pub fn open_root(spec: &RootSpec) -> std::result::Result<SafeRoot, RootRefusal> {
    let dir = match spec.mode {
        Mode::Plain => plain::open_root(spec)?,
        #[cfg(windows)]
        Mode::Guarded => win::open_root(spec)?,
        #[cfg(not(windows))]
        Mode::Guarded => return Err(RootRefusal::UnsupportedPlatform),
    };
    Ok(SafeRoot {
        dir,
        owners: spec.owners.clone(),
        create_access: spec.create_access.clone(),
    })
}

/// 期待のパスの形（純関数。[`Mode::Guarded`] の入口）: 空でない・絶対パス・`.`/`..` なし。
// 使うのは Windows の Guarded だけ（非 Windows の Guarded は形を見る前に断る）。純関数のテストは全 OS。
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn root_form_problem(path: &Path) -> Option<RootFormProblem> {
    if path.as_os_str().is_empty() {
        Some(RootFormProblem::Empty)
    } else if !path.is_absolute() {
        Some(RootFormProblem::NotAbsolute)
    } else if crate::acl::has_dot_segment(path) {
        Some(RootFormProblem::DotSegment)
    } else {
        None
    }
}
