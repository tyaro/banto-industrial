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
//! # 守れないこと（[`ReopenPath`]・[`PinnedName`] の doc も）
//!
//! SQLite・tstore・banto の BackupService は、渡したパスで後から開き直し、付随のファイル
//! （`-wal`・`-shm`・`-journal`）を自分でパスから作る。[`ManagedDir::reopen_path`] は確かめた
//! 時点のことしか保証しない（確かめてから相手が開くまでの間の、名前の変更・削除・リンクへの
//! 置換が残る）。[`ManagedDir::pin_file`] は名前を辿らずに開いて確かめ、**削除なしの共有で
//! 握り続ける**ので、握っている名前についてはその間が塞がる（[`PinnedName`] が生きている間、
//! 他者はその名前を変更・削除・置換できない）。残るのは、
//!
//! - **握っていない名前**: 相手が後から作る付随のファイル（握らなかった `-journal`、SQLite の
//!   一時ファイル・`VACUUM` の作業ファイル・予期しない名前）は [`ManagedDir::reopen_path`] と
//!   同じく確かめた時点まで。
//! - **握る前の窓**: 握る時点で既に `DELETE` の権利で開いている他者のハンドルがあれば、握るのが
//!   共有違反で失敗する（それは塞げないので失敗として返す）。
//! - **同じファイルへの別名**: 握った後に、別の場所に同じファイルへのハードリンクを足すことは
//!   止めない（ファイルへの書き込み権が要る。1903 以降の mitigation）。足した別名は握って
//!   いないので改名・削除できる（【実測】）が、握った名前は変わらず、外へは書かない。
//! - **握ったファイルのその場の reparse 化**: 握るハンドルの共有は読み書きを許す（SQLite が
//!   書く）ので、そのファイルへの書き込み権（とシンボリックリンクの特権）を持つ他者は、握った
//!   空のファイルを `FSCTL_SET_REPARSE_POINT` でその場でシンボリックリンクなどに変えられ、
//!   相手が後でパスで開くとそれを辿りうる。名前を握っても防げない（Codex P2）。防ぐのは
//!   ファイルの記述子で、運用モードの呼び出し側は [`ManagedDir::pin_file_checked`] を使う。
//!   運用モードに締める前から書き込みの権利で開いていた他者の古いハンドルは記述子では防げない
//!   （PR-3 の既知の制限と同じ残留リスク）。
//! - **SQLite が自分で消す名前**: 握った名前は SQLite も消せない。WAL の最後の接続が閉じるとき
//!   `-wal`・`-shm` を消しに行き、共有違反を数回待ってやり直してから諦める（失敗は無視され、
//!   `close` 1 回に約 3 秒。`SQLITE_FCNTL_PERSIST_WAL` で消しに行かなくなる。`win_tests` の
//!   `sqlite_with_pins` の実測）。`journal_mode=DELETE` の `-journal` は握らない。
//!   SQLite の DB は `sqlite` feature の `ManagedDir::pin_sqlite`（`crate::sqlite`。#623 PR2a）が、
//!   本体・`-wal`・`-shm` を握り、接続ごとに `SQLITE_FCNTL_PERSIST_WAL` を立てて開く
//!   （`-journal` は握らず作らず、在ればリンクでないかを確かめた時点まで）。使っているのは
//!   banto-hub のサービスの設定 DB だけ（ChronoGazer のサービスは次の PR。sidecar・0025 の `.bak`・
//!   tstore・banto の BackupService・監査の保留は [`ManagedDir::reopen_path`] などパスのまま）。
//!
//! そこを守るのは運用モードのルート全体の ACL（PR-1c・PR-3/5）と、途中のジャンクションへの
//! 保険の Redirection Guard（[`crate::redirection`]）。
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
pub(crate) mod win_tests;

/// 握り方（設計 §4.1）。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
    /// 辿らずに開いて握り、ハンドル基準で扱う（Windows のサービス）。
    Guarded,
    /// `std::fs` のまま（デスクトップ・コンソール・非 Windows）。リンクを辿る。
    /// **サービス（LocalSystem）では使わない**（確かめをしない）。
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
    /// **ルートを**作るときに付ける保護された DACL（作るのと同時に付ける）。`None` なら親から
    /// 継承する。既にある物の ACL は変えない・確かめない（形は [`crate::mode`] の 2 周目が書く）。
    /// 管理するフォルダを作るときの記述子は、[`SafeRoot::open_managed`] に**呼ぶたびに**渡す
    /// （#623 PR-3: 締める前に開く `logs` は設定モードの形、締めた後に開く残りは運用モードの形）。
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
    /// 無ければ作るか（[`SafeRoot::open_managed`] の `create_access` を作るのと同時に付ける）。
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
/// 印のファイル（[`crate::mode::MARKER_NAME`] の綴り。前さばきで、本当の守りは
/// [`ManagedDir::remove`] がファイルの同一性で断ること）でない・`pred` が名前を選ぶ。候補でも、
/// 消す前に開いたハンドルで属性を読み直す（[`ManagedDir::remove`]）。
pub fn is_removal_candidate(entry: &Entry, pred: &dyn Fn(&str) -> bool) -> bool {
    entry.kind == EntryKind::File
        && entry.reparse_tag.is_none()
        && entry.name != crate::mode::MARKER_NAME
        && pred(&entry.name)
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
/// 確かめてから相手が開くまでの間を塞ぎたい名前は、[`ManagedDir::pin_file_checked`]
/// （[`PinnedName`]）で握ってから渡す。
///
/// **残留リスク**: `ReopenPath` は「確かめた時点で、このフォルダ自身が reparse point でなく
/// （握ったハンドルで読み直した属性）、このフォルダの中のこの名前と付随の名前がリンク（reparse
/// point・ハードリンク）でもフォルダでもなかった（無いのも可）」ことしか保証しない。相手は
/// このパスで後から開き直し、付随のファイルを自分でパスから作る。それでも外へ出ないのは、
/// **運用モードの間は利用者がこの木に名前を作れない**（ルート全体の ACL。PR-1c・PR-3/5）
/// からであって、この型の力ではない。フォルダの記述子が運用モードの形かの確かめ直し
/// （設計 §2.9 (d)）は [`ManagedDir::reopen_path_checked`]。Redirection Guard は途中の
/// ジャンクションの保険（ハードリンク・シンボリックリンクには効かない）。[`Mode::Plain`] は
/// 何も確かめない。
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

/// 名前 1 要素のファイルを**握ったまま**、パスで開き直す相手（SQLite の本体・`-wal`・`-shm`、
/// `VACUUM INTO` の宛先）に渡すためのガード（[`ManagedDir::pin_file_checked`]・
/// [`ManagedDir::pin_file`]、#623）。
///
/// **契約（[`Mode::Guarded`]）**: このガードが生きている間、[`PinnedName::path`] の名前は他者から
/// 変更・削除・置換（別のファイル・ハードリンク・シンボリックリンク・ジャンクションへのすり替え）
/// できない。握ったハンドルの共有に `FILE_SHARE_DELETE` を付けないので、`DELETE` の権利で開く
/// こと（名前の変更・削除・置き換えの前提）が共有違反で失敗する。握った時点で、それが
/// reparse point でもフォルダでもなく、リンク数 1 だったことを**開いた後のハンドルで**確かめて
/// いる。相手が同じパスで開き直すとき、得るのは握っている同じファイル。
///
/// **契約の外（Codex P2）**: 名前を握っても、**ファイルそのもの**をその場で reparse point に
/// 変えること（そのファイルへの書き込み権で開いた別のハンドルからの `FSCTL_SET_REPARSE_POINT`）
/// は止めない。握るハンドルの共有は書き込みを許すので（SQLite が書く）、握ったのが空のファイル
/// なら、書き込み権とシンボリックリンクの特権を持つ他者はそれをシンボリックリンクに変えられ、
/// 相手が後でパスで開くとそれを辿りうる。防ぐのはファイルの記述子で、
/// [`ManagedDir::pin_file_checked`] が握った後にハンドルで確かめる（他者に書き込み権が無い形
/// だけを通す）。[`ManagedDir::pin_file`]（確かめない版）で握った名前はこれを守らない。運用
/// モードに締める前から書き込みの権利で開いていた他者の古いハンドルは記述子では防げない
/// （PR-3 の既知の制限と同じ残留リスク）。
///
/// **相手への影響**: 共有は読み書きを許すので、読み書き共有で開く相手（SQLite）は開ける。
/// ただし相手もこの名前を消せない。【実測】WAL の最後の接続が閉じるとき SQLite は `-wal`・
/// `-shm` を消しに行き、共有違反を数回待ってやり直してから諦める（失敗は `close` の結果に
/// 出ず、`close` 1 回に約 3 秒かかる。チェックポイントは済んでいて行は失われない）。接続ごとに
/// `SQLITE_FCNTL_PERSIST_WAL` を立てれば消しに行かない（`-wal` は `journal_size_limit` で閉じるとき 0 バイトに
/// 切り詰められる）。`journal_mode=DELETE` の `-journal` のように、相手が毎回消す名前は握らない。
/// drop で閉じ、以後は普通に変更・削除できる。
///
/// [`Mode::Plain`] は何も握らない（[`PinnedName::is_held`] が偽。パスを返すだけ）。
#[derive(Debug)]
pub struct PinnedName {
    path: PathBuf,
    /// 握っているハンドル（[`Mode::Guarded`] だけ）。drop で閉じる。
    held: Option<std::fs::File>,
}

impl PinnedName {
    /// 握っている名前のパス（[`ManagedDir::final_path`] + 名前。渡された綴りではなく本当のパス
    /// 基準）。相手にはこのパスを渡す。
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 名前を削除なしの共有で握っているか（[`Mode::Guarded`]）。
    pub fn is_held(&self) -> bool {
        self.held.is_some()
    }
}

enum Inner {
    #[cfg(windows)]
    Guarded(crate::acl::PinnedDir),
    Plain,
}

/// 握ったルート・管理するフォルダ（設計 §4.3）。[`Mode::Guarded`] では辿らずに開いたハンドルを
/// 持ち続け、drop で閉じる。スレッドをまたいで使える（一覧は握ったハンドルごとに内部で直列化する）。
pub struct ManagedDir {
    inner: Inner,
    /// 表示用のパス（渡された綴り + 名前。開き直すのには使わない）。
    display: PathBuf,
    /// 本当のパス（Guarded: ハンドルの `GetFinalPathNameByHandleW`、Plain: `canonicalize`）。
    final_path: PathBuf,
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
        }
    }

    #[cfg(windows)]
    fn guarded(pinned: crate::acl::PinnedDir, display: PathBuf) -> Self {
        let final_path = pinned.final_path().to_path_buf();
        Self {
            inner: Inner::Guarded(pinned),
            display,
            final_path,
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
        // 印のファイル（#623 PR-1c）はサービス自身の掃除でも消さない（消えると古いハンドルで
        // フォルダを reparse point にできるようになる）。綴りはここで、同一性（綴り違いで同じ物を
        // 指す名前）は Guarded の `win::remove` が開いた後のファイル ID で断る（Fable M-1）。
        if name == crate::mode::MARKER_NAME {
            return Err(invalid_name());
        }
        match &self.inner {
            #[cfg(windows)]
            Inner::Guarded(pinned) => win::remove(pinned, &self.display_of(name), name),
            Inner::Plain => plain::remove(&self.final_path, name),
        }
    }

    /// 中の一覧（`.`・`..` を除く。順は OS のまま）。Guarded は握ったハンドルを列挙する
    /// （パスを使わない。その場でジャンクションにされても、列挙はこのフォルダそのものを返す）。
    pub fn list(&self) -> Result<Vec<Entry>> {
        // 列挙の位置はファイルオブジェクトに付くので、Guarded は `win::list` が握ったハンドルの
        // `PinnedDir::enum_guard` で直列化する（`reset_acl` の再帰とも重ならない。S-1）。
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

    /// 名前 1 要素のファイルを辿らずに開いて（`create` なら無ければ空のファイルを作って）
    /// **握り続ける**（[`PinnedName`] の契約）。`create` が偽で無ければ `Ok(None)`（`create` が
    /// 真なら `None` にならない）。用途: SQLite の本体・`-wal`・`-shm`（`create` 真で空のまま
    /// 渡してよい）、`VACUUM INTO` の宛先（空のファイル）。
    ///
    /// Guarded は (1) 握ったハンドルでこのフォルダ自身の属性を読み直し、reparse point なら断る
    /// （[`ManagedDir::reopen_path`] と同じ）、(2) 名前を握ったハンドルを基準に相対に
    /// `FILE_OPEN_REPARSE_POINT` で開き（作るときの記述子は親のフォルダから継承）、**開いた後に
    /// ハンドルで** reparse point でない・フォルダでない・リンク数 1 を確かめ、合わなければ
    /// 閉じて [`SafeRootError::Refused`]。共有は読み書き（削除なし）。握る時点で既に `DELETE` の
    /// 権利で開いている他者のハンドルがあると、共有違反（`ERROR_SHARING_VIOLATION`）の
    /// [`SafeRootError::Io`] で失敗する。握ったファイルがある間、このフォルダは空でないので
    /// その場でジャンクションにもされない。
    ///
    /// Plain は確かめず、握らない（`create` なら `std::fs` で無ければ作り、偽なら有無だけ見る。
    /// リンクを辿る）。印のファイル（[`crate::mode::MARKER_NAME`]）は綴りの大文字小文字を
    /// 問わず断る。文字列の比較は前さばきで、本当の守りは**開いたハンドルのファイル ID**
    /// （8.3 の短縮名など、綴りが違っても印の席に解決する名前は Guarded で断る）。
    ///
    /// **ファイルの記述子は確かめない**: そのファイルへの書き込み権を持つ他者は、握った空の
    /// ファイルをその場で reparse point に変えられる（[`PinnedName`] の「契約の外」）。運用モードの
    /// 呼び出し側は [`ManagedDir::pin_file_checked`] を使う。
    pub fn pin_file(&self, name: &str, create: bool) -> Result<Option<PinnedName>> {
        if !guard::is_plain_child_name(name) || name.eq_ignore_ascii_case(crate::mode::MARKER_NAME)
        {
            return Err(invalid_name());
        }
        let held = match &self.inner {
            #[cfg(windows)]
            Inner::Guarded(pinned) => match win::pin_file(pinned, &self.display, name, create)? {
                Some(file) => Some(file),
                None => return Ok(None),
            },
            Inner::Plain => {
                if !plain::pin_file(&self.final_path, name, create)? {
                    return Ok(None);
                }
                None
            }
        };
        Ok(Some(PinnedName {
            path: self.final_path.join(name),
            held,
        }))
    }

    /// [`ManagedDir::reopen_path`] に、握ったフォルダの**記述子の確かめ直し**を足したもの
    /// （設計 §2.9 (b)(d)。運用モードのサービスはこちらを使う）。Guarded は先に
    /// [`ManagedDir::recheck`]（reparse 属性と、記述子が `expected` = 運用モードの形か）を通し、
    /// 合わなければ [`crate::mode::RecheckFailure`] を [`SafeRootError::Refused`]（reparse）か
    /// [`SafeRootError::AclMismatch`]（形）で返す。Plain は確かめずに `reopen_path` と同じ。
    pub fn reopen_path_checked(
        &self,
        name: &str,
        companions: &[&str],
        expected: &DirAccess,
    ) -> Result<ReopenPath> {
        self.recheck_as_error(expected)?;
        self.reopen_path(name, companions)
    }

    /// [`ManagedDir::pin_file`] に、**記述子の確かめ**を足したもの（#623、Codex P2。運用モードの
    /// サービスはこちらを使う）。握った名前でも、そのファイルへの書き込み権（とシンボリック
    /// リンクの特権）を持つ他者は、握った空のファイルを `FSCTL_SET_REPARSE_POINT` で**その場で**
    /// reparse point に変えられる（共有に `FILE_SHARE_WRITE` を含むため。SQLite が書くので外せ
    /// ない）。それを防ぐのは記述子なので、ここで確かめる。
    ///
    /// Guarded は (1) [`ManagedDir::recheck`]（このフォルダの reparse 属性と、記述子が `expected`
    /// = 運用モードの形か。[`ManagedDir::reopen_path_checked`] と同じ失敗の返し方）、(2)
    /// [`ManagedDir::pin_file`] と同じく開いて握り、(3) **握ったハンドルで**ファイルの記述子
    /// （DACL と所有者）を読み、[`crate::acl::check_file_dacl`] で `expected` の形（作った直後の
    /// 継承のままか、締めた後の保護された形）か確かめ、合わなければ閉じて
    /// [`SafeRootError::AclMismatch`]。`create` で作ったばかりのファイルは (1) を通ったフォルダから
    /// 継承するので通る。Plain は確かめずに [`ManagedDir::pin_file`] と同じ。
    ///
    /// **残留リスク**: 運用モードに締める前から書き込みの権利でそのファイルを開いていた他者の
    /// 古いハンドルは、記述子を締めても閉じられないので防げない（PR-3 の既知の制限と同じ）。
    pub fn pin_file_checked(
        &self,
        name: &str,
        create: bool,
        expected: &DirAccess,
    ) -> Result<Option<PinnedName>> {
        if !guard::is_plain_child_name(name) || name.eq_ignore_ascii_case(crate::mode::MARKER_NAME)
        {
            return Err(invalid_name());
        }
        self.recheck_as_error(expected)?;
        match &self.inner {
            #[cfg(windows)]
            Inner::Guarded(pinned) => {
                Ok(
                    win::pin_file_checked(pinned, &self.display, name, create, expected)?.map(
                        |file| PinnedName {
                            path: self.final_path.join(name),
                            held: Some(file),
                        },
                    ),
                )
            }
            Inner::Plain => self.pin_file(name, create),
        }
    }

    /// 名前 1 要素のファイル `from` を、同じフォルダの中の `to` へ、**辿らずに開いて・置き換えずに**
    /// 付け替える（`from` が無ければ `Ok(false)`）。用途: 作りかけの名前で書き終えたファイルを
    /// 完成の名前に付け替える（0025 移行前の `.bak`、#623 PR3b）。
    ///
    /// Guarded は `from` を削除の権利で握ったフォルダ基準に開き、**開いた後のハンドルで**
    /// reparse point・フォルダ・リンク数 1 でないを確かめて [`SafeRootError::Refused`]（印の席も
    /// 断る）、付け替えも開いたハンドルで、宛先は握ったこのフォルダのハンドル基準に行う（パスを
    /// 解決し直さない）。**置き換えない**: `to` が既にあれば（普通のファイル・フォルダ・
    /// ジャンクション・シンボリックリンクのどれでも）何も変えずに [`SafeRootError::Io`] で失敗する
    /// （[`ManagedDir::write_atomic`] のように宛先を先に確かめる必要がないのもそのため）。
    /// `from` の中身は確かめない。印のファイル（[`crate::mode::MARKER_NAME`]）は綴りの大文字小文字を
    /// 問わず `from`・`to` のどちらでも断る。
    ///
    /// **残留**: 呼ぶ側が `from` を握っていた（[`PinnedName`]）なら、その握りを離してからこれを呼ぶ
    /// までの間は塞がらない（運用モードの ACL で利用者がこの木に名前を作れないことに頼る）。
    /// Plain は確かめず（`to` の有無だけ見て `std::fs::rename`）、確かめと付け替えの間の競合は残る。
    pub fn rename_no_replace(&self, from: &str, to: &str) -> Result<bool> {
        for name in [from, to] {
            if !guard::is_plain_child_name(name)
                || name.eq_ignore_ascii_case(crate::mode::MARKER_NAME)
            {
                return Err(invalid_name());
            }
        }
        match &self.inner {
            #[cfg(windows)]
            Inner::Guarded(pinned) => win::rename_no_replace(pinned, &self.display, from, to),
            Inner::Plain => plain::rename_no_replace(&self.final_path, from, to),
        }
    }

    /// [`ManagedDir::rename_no_replace`] に、握ったフォルダの記述子の確かめ直し
    /// （[`ManagedDir::recheck`]。[`ManagedDir::pin_file_checked`] と同じ失敗の返し方）を足したもの。
    pub fn rename_no_replace_checked(
        &self,
        from: &str,
        to: &str,
        expected: &DirAccess,
    ) -> Result<bool> {
        self.recheck_as_error(expected)?;
        self.rename_no_replace(from, to)
    }

    /// [`ManagedDir::recheck`] の失敗を [`SafeRootError`] にする（reparse・フォルダでないは
    /// [`SafeRootError::Refused`]、形は [`SafeRootError::AclMismatch`]）。
    fn recheck_as_error(&self, expected: &DirAccess) -> Result<()> {
        self.recheck(expected).map_err(|failure| match failure {
            crate::mode::RecheckFailure::ReparsePoint { tag } => SafeRootError::Refused {
                path: self.display.clone(),
                refusal: Refusal::ReparsePoint { tag },
            },
            crate::mode::RecheckFailure::NotADirectory => SafeRootError::Refused {
                path: self.display.clone(),
                refusal: Refusal::NotADirectory,
            },
            crate::mode::RecheckFailure::Acl(mismatch) => SafeRootError::AclMismatch(mismatch),
            crate::mode::RecheckFailure::Io(error) => error,
        })
    }

    /// 握ったフォルダの reparse 属性と記述子をハンドルで読み直し、`expected`（運用モードの形）に
    /// 通るか確かめる（`PinnedDir::recheck`、設計 §2.9 (b)(d)）。Plain は何も確かめず `Ok`。
    pub fn recheck(
        &self,
        expected: &DirAccess,
    ) -> std::result::Result<(), crate::mode::RecheckFailure> {
        match &self.inner {
            #[cfg(windows)]
            Inner::Guarded(pinned) => pinned.recheck(expected),
            Inner::Plain => {
                let _ = expected;
                Ok(())
            }
        }
    }

    /// 印のファイル（[`crate::mode::MARKER_NAME`]）を無ければ置き、置いた直後に握ったハンドルで
    /// 自分の reparse 属性を読み直す（`PinnedDir::ensure_marker`、設計 §2.9 (a)）。Plain は
    /// 置かない（[`crate::mode::MarkerOutcome::Skipped`]。デスクトップの挙動を変えない）。
    pub fn ensure_marker(
        &self,
    ) -> std::result::Result<crate::mode::MarkerOutcome, crate::mode::MarkerError> {
        match &self.inner {
            #[cfg(windows)]
            Inner::Guarded(pinned) => pinned.ensure_marker(),
            Inner::Plain => Ok(crate::mode::MarkerOutcome::Skipped),
        }
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

    /// 管理するフォルダを親から順に開いて握る（無ければ `create` の spec だけ、`create_access`
    /// の保護された DACL を作るのと同時に付けて作る。`None` なら親から継承）。spec の並びは
    /// 何も開く前に [`check_specs`] で確かめる。どれか 1 つでも開けない・確かめに通らないなら
    /// 止めて、1 つも返さない（設計 §4.1「常に起動を止める」）。
    ///
    /// `create_access` を**呼ぶたびに**渡すのは（#623 PR-3、設計 §4.7）、締める前に開く `logs` は
    /// 設定モードの形で作り（断ったとき「設定モードのまま」にする）、締めた後に開く残りは運用
    /// モードの形で作るため。1 つの値を両方に使うと、どちらかが食い違う。
    pub fn open_managed(
        &self,
        specs: &[ManagedSpec],
        create_access: Option<&DirAccess>,
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
            let opened = self
                .open_one(parent, spec, create_access)
                .map_err(|reason| ManagedStop {
                    key: spec.key,
                    reason,
                })?;
            dirs.push((spec.key, opened));
        }
        Ok(ManagedSet { dirs })
    }

    /// Open or create a dynamically configured relative child directory,
    /// keeping its final handle. Every component is opened from the preceding
    /// held handle with the same checks as `open_managed`, then checked against
    /// the operation-mode ACL. The caller must first validate path policy and
    /// establish the root's operation mode. Plain mode is only a compatibility
    /// fallback for simulation; it provides no non-following boundary.
    pub fn open_relative_operation_dir(
        &self,
        relative: &Path,
    ) -> std::result::Result<ManagedDir, ManagedRefusal> {
        let operation = self
            .dir
            .is_guarded()
            .then(|| DirAccess::with_readers(self.owners.clone()));
        let mut opened: Option<ManagedDir> = None;
        for component in relative.components() {
            let std::path::Component::Normal(name) = component else {
                return Err(ManagedRefusal::Spec(SpecProblem::InvalidName));
            };
            let name = name
                .to_str()
                .filter(|name| guard::is_plain_child_name(name))
                .ok_or(ManagedRefusal::Spec(SpecProblem::InvalidName))?;
            let parent = opened.as_ref().unwrap_or(&self.dir);
            let child = match &parent.inner {
                #[cfg(windows)]
                Inner::Guarded(pinned) => win::open_managed_child(
                    pinned,
                    &parent.display,
                    name,
                    true,
                    &self.owners,
                    operation.as_ref(),
                )?,
                Inner::Plain => plain::open_managed_child(parent, name, true)?,
            };
            if let Some(access) = &operation {
                child.recheck(access).map_err(|failure| match failure {
                    crate::mode::RecheckFailure::ReparsePoint { tag } => {
                        ManagedRefusal::Refused(Refusal::ReparsePoint { tag })
                    }
                    crate::mode::RecheckFailure::NotADirectory => {
                        ManagedRefusal::Refused(Refusal::NotADirectory)
                    }
                    crate::mode::RecheckFailure::Acl(mismatch) => {
                        ManagedRefusal::Io(SafeRootError::AclMismatch(mismatch))
                    }
                    crate::mode::RecheckFailure::Io(error) => ManagedRefusal::Io(error),
                })?;
            }
            opened = Some(child);
        }
        opened.ok_or(ManagedRefusal::Spec(SpecProblem::InvalidName))
    }

    fn open_one(
        &self,
        parent: &ManagedDir,
        spec: &ManagedSpec,
        create_access: Option<&DirAccess>,
    ) -> std::result::Result<ManagedDir, ManagedRefusal> {
        match &parent.inner {
            #[cfg(windows)]
            Inner::Guarded(pinned) => {
                win::open_managed_one(pinned, &parent.display, spec, &self.owners, create_access)
            }
            Inner::Plain => {
                let _ = create_access;
                plain::open_managed_one(parent, spec)
            }
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

    /// key の管理するフォルダを**取り出す**（組から外れる。握ったハンドルを別の持ち主 - 日次ログ・
    /// 秘密の保存先 - に渡すため。#623 PR-3）。無ければ [`UnknownKey`]。
    pub fn take(&mut self, key: &str) -> std::result::Result<ManagedDir, UnknownKey> {
        let index = self
            .dirs
            .iter()
            .position(|(k, _)| *k == key)
            .ok_or_else(|| UnknownKey(key.to_string()))?;
        Ok(self.dirs.remove(index).1)
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
