//! [`super::Mode::Guarded`] の Windows の実装（モジュール doc「何を守るか」）。開くのは
//! [`crate::acl`] の `pin_dir`・`PinnedDir::{open_child_dir, create_child_dir}`・`child_relative`、
//! 属性・名前の付け替え・削除は [`crate::handle_io`]。パスで開き直さない。

use std::fs::File;
use std::path::Path;

use super::{
    nonce_seed, owner_allowed, root_form_problem, temp_name, Entry, EntryKind, ManagedDir,
    ManagedRefusal, ManagedSpec, RootRefusal, RootSpec, TEMP_TRIES,
};
use crate::acl::imp::{child_relative, enumerate_dir, ChildHow};
use crate::acl::{pin_dir, DirAccess, InstallUserSid, PinnedDir};
use crate::error::{IoOp, Refusal, Result, SafeRootError};
use crate::guard;
use crate::handle_io::{self, dir_refusal, file_refusal};

pub(super) fn open_root(spec: &RootSpec) -> std::result::Result<ManagedDir, RootRefusal> {
    if let Some(problem) = root_form_problem(&spec.expected) {
        return Err(RootRefusal::InvalidForm(problem));
    }
    guard::reject_well_known_root(&spec.expected, &spec.known).map_err(RootRefusal::WellKnown)?;
    let pinned = match pin_dir(&spec.expected).map_err(RootRefusal::Io)? {
        Some(pinned) => pinned,
        None if !spec.create => return Err(RootRefusal::Absent),
        None => create_root(spec)?,
    };
    let facts = handle_io::facts(pinned.raw_handle()).map_err(RootRefusal::Io)?;
    if let Some(refusal) = dir_refusal(&facts) {
        return Err(RootRefusal::Refused(refusal));
    }
    // 期待のパスは、在ることが分かってから長い名前に直す（無いパスの 8.3 は直せない）。
    let expected = crate::nofollow::normalize_expected(&spec.expected);
    guard::check_real_path(pinned.final_path(), &expected, &spec.known)
        .map_err(RootRefusal::RealPath)?;
    if !owner_allowed(&pinned.info().owner, &spec.owners) {
        return Err(RootRefusal::ForeignOwner(pinned.info().owner.clone()));
    }
    Ok(ManagedDir::guarded(pinned, spec.expected.clone()))
}

/// 無いルートを、親を辿らずに開いて確かめてから、親のハンドルを基準に作る（#626 PR-B の
/// elev と同じ順。作るのと同時に [`RootSpec::create_access`] を付ける）。
fn create_root(spec: &RootSpec) -> std::result::Result<PinnedDir, RootRefusal> {
    let (Some(parent_path), Some(name)) = (
        spec.expected.parent(),
        spec.expected.file_name().and_then(|n| n.to_str()),
    ) else {
        return Err(RootRefusal::InvalidForm(guard::RootFormProblem::Empty));
    };
    let parent = pin_dir(parent_path)
        .map_err(RootRefusal::Io)?
        .ok_or(RootRefusal::ParentAbsent)?;
    guard::check_root_parent(
        parent.final_path(),
        &crate::nofollow::normalize_expected(parent_path),
        parent.info(),
        &spec.known,
    )
    .map_err(RootRefusal::Parent)?;
    if let Some(created) = parent
        .create_child_dir(name, spec.create_access.as_ref())
        .map_err(RootRefusal::Io)?
    {
        return Ok(created);
    }
    // 確かめてから作るまでの間に誰かが作った。パスで開き直さず、親のハンドルを基準に開く。
    parent
        .open_child_dir(name)
        .map_err(RootRefusal::Io)?
        .ok_or(RootRefusal::Absent)
}

pub(super) fn open_managed_one(
    parent: &PinnedDir,
    parent_display: &Path,
    spec: &ManagedSpec,
    owners: &[InstallUserSid],
    create_access: Option<&DirAccess>,
) -> std::result::Result<ManagedDir, ManagedRefusal> {
    let opened = match parent
        .open_child_dir(spec.name)
        .map_err(ManagedRefusal::Io)?
    {
        Some(dir) => dir,
        None if !spec.create => return Err(ManagedRefusal::Absent),
        None => match parent
            .create_child_dir(spec.name, create_access)
            .map_err(ManagedRefusal::Io)?
        {
            Some(dir) => dir,
            // 確かめてから作るまでの間に誰かが作った。親のハンドルを基準に開き直す。
            None => parent
                .open_child_dir(spec.name)
                .map_err(ManagedRefusal::Io)?
                .ok_or(ManagedRefusal::Absent)?,
        },
    };
    let facts = handle_io::facts(opened.raw_handle()).map_err(ManagedRefusal::Io)?;
    if let Some(refusal) = dir_refusal(&facts) {
        return Err(ManagedRefusal::Refused(refusal));
    }
    // 親のハンドルを基準に開いたので外には出ないはずだが、本当のパスでも確かめる（多重の防御）。
    let expected = parent.final_path().join(spec.name);
    if !guard::paths_match(opened.final_path(), &expected) {
        return Err(ManagedRefusal::Refused(Refusal::Elsewhere {
            actual: opened.final_path().to_path_buf(),
        }));
    }
    if !owner_allowed(&opened.info().owner, owners) {
        return Err(ManagedRefusal::ForeignOwner(opened.info().owner.clone()));
    }
    Ok(ManagedDir::guarded(opened, parent_display.join(spec.name)))
}

fn refused(display: &Path, refusal: Refusal) -> SafeRootError {
    SafeRootError::Refused {
        path: display.to_path_buf(),
        refusal,
    }
}

/// 開いたハンドルをファイルとして確かめ（[`file_refusal`]）、通れば `File` にする。合わなければ
/// ハンドルを閉じて断る。
fn verified_file(handle: crate::acl::imp::OwnedHandle, display: &Path) -> Result<File> {
    let facts = handle_io::facts(handle.raw())?;
    if let Some(refusal) = file_refusal(&facts) {
        return Err(refused(display, refusal));
    }
    Ok(handle_io::into_file(handle))
}

pub(super) fn create_new(dir: &PinnedDir, name: &str) -> Result<Option<File>> {
    // 作ったばかりの物（`FILE_CREATE`・`FILE_OPEN_REPARSE_POINT`）は普通のファイルで、リンク数 1。
    Ok(child_relative(dir, name, ChildHow::CreateFile(None))?.map(handle_io::into_file))
}

pub(super) fn open_append(
    dir: &PinnedDir,
    display: &Path,
    name: &str,
    create: bool,
) -> Result<File> {
    match child_relative(dir, name, ChildHow::OpenAppend { create })? {
        Some(handle) => verified_file(handle, display),
        None => Err(SafeRootError::io(
            IoOp::OpenFile,
            &std::io::Error::from(std::io::ErrorKind::NotFound),
        )),
    }
}

pub(super) fn open_read(dir: &PinnedDir, display: &Path, name: &str) -> Result<Option<File>> {
    child_relative(dir, name, ChildHow::OpenFile)?
        .map(|handle| verified_file(handle, display))
        .transpose()
}

pub(super) fn write_atomic(
    dir: &PinnedDir,
    display: &Path,
    name: &str,
    contents: &[u8],
) -> Result<()> {
    // 宛先がフォルダ（ジャンクション・ディレクトリのシンボリックリンクを含む）なら、何も作らずに
    // 断る。【実測】`FileRenameInformationEx`（`REPLACE_IF_EXISTS | POSIX_SEMANTICS`）は、普通の
    // 空のフォルダは置き換えない（`ERROR_ACCESS_DENIED`）が、**ジャンクションは置き換えてしまう**
    // （リンクの名前が消えて一時ファイルの中身になる。リンクの先は辿らない。Windows 11 26300、
    // 2026-10-18）。Fable B-6 の「付け替えは失敗する」は成り立たないので、付け替えに任せずに先に
    // 確かめる。確かめてから付け替えるまでの間に差し替えられても、置き換わるのはリンクの名前
    // だけ（先へは書かない）。宛先が**ファイルの** reparse point（WOF の `compact` など）の
    // ときは、この確かめを通り、ファイルのシンボリックリンク・ハードリンクと同じく名前ごと
    // 一時ファイルの中身に置き換わる（リンク先へは書かない。意図どおり）。
    if let Some(handle) = child_relative(dir, name, ChildHow::OpenAttributes)? {
        let facts = handle_io::facts(handle.raw())?;
        if facts.is_dir {
            let refusal = if facts.is_reparse {
                Refusal::ReparsePoint {
                    tag: facts.reparse_tag,
                }
            } else {
                Refusal::NotAFile
            };
            return Err(refused(display, refusal));
        }
    }
    let seed = nonce_seed();
    let mut created = None;
    for step in 0..TEMP_TRIES {
        let temp = temp_name(name, seed.wrapping_add(step)).ok_or_else(super::invalid_name)?;
        if let Some(handle) = child_relative(dir, &temp, ChildHow::CreateFile(None))? {
            created = Some(handle);
            break;
        }
        // 同じ名前が既にある（衝突）。nonce を進めて試し直す。
    }
    let Some(handle) = created else {
        return Err(SafeRootError::io(
            IoOp::CreateFile,
            &std::io::Error::from(std::io::ErrorKind::AlreadyExists),
        ));
    };
    let result = crate::acl::imp::write_all_and_flush(handle.raw(), contents)
        .and_then(|()| handle_io::rename_replace(handle.raw(), dir.raw_handle(), name));
    if result.is_err() {
        // 付け替えられなかった一時ファイルは、作ったハンドルで消す（名前で消さない）。消す失敗で
        // 元の理由を上書きしない。
        let _ = crate::acl::imp::delete_by_handle(handle.raw());
    }
    drop(handle);
    result
}

pub(super) fn remove(dir: &PinnedDir, display: &Path, name: &str) -> Result<bool> {
    let Some(handle) = child_relative(dir, name, ChildHow::OpenForDelete)? else {
        return Ok(false);
    };
    // 一覧の属性は信じず、開いた後のハンドルで読み直す（Fable B-5）。ハードリンクは消して
    // よい（消えるのはこの名前だけで、別名の中身は変わらない）。
    let facts = handle_io::facts(handle.raw())?;
    if facts.is_reparse {
        return Err(refused(
            display,
            Refusal::ReparsePoint {
                tag: facts.reparse_tag,
            },
        ));
    }
    if facts.is_dir {
        return Err(refused(display, Refusal::NotAFile));
    }
    // 開いた物が印の席（OS が `MARKER_NAME` に解決する物）なら、綴りが違っても消さない
    // （`.BANTO-SAFE-ROOT` は大文字小文字を区別しないフォルダでは同じ物。Fable M-1）。
    if let crate::acl::imp::MarkerProbe::Found(slot) =
        crate::acl::imp::probe_marker(dir.raw_handle())?
    {
        if slot.id == crate::acl::imp::file_id_of(handle.raw())? {
            return Err(super::invalid_name());
        }
    }
    handle_io::delete_posix(handle.raw())?;
    drop(handle); // 閉じたときに消える。
    Ok(true)
}

pub(super) fn list(dir: &PinnedDir) -> Result<Vec<Entry>> {
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
    };
    let mut entries = Vec::new();
    // 同じハンドルの列挙（`reset_acl` の再帰を含む）と重ならないようにする。
    let _serial = dir.enum_guard();
    enumerate_dir(dir.raw_handle(), &mut |raw| {
        entries.push(Entry {
            name: raw.name,
            kind: if raw.attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
                EntryKind::Dir
            } else {
                EntryKind::File
            },
            reparse_tag: (raw.attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0)
                .then_some(raw.reparse_tag),
            len: raw.len,
        });
        Ok(())
    })
    .map_err(|e| match e {
        SafeRootError::AclApi { os_error, .. } => SafeRootError::io(
            IoOp::ListDir,
            &std::io::Error::from_raw_os_error(os_error as i32),
        ),
        other => other,
    })?;
    Ok(entries)
}

pub(super) fn check_reopen(dir: &PinnedDir, display: &Path, names: &[String]) -> Result<()> {
    // (1) 握ったフォルダ自身（同じオブジェクトなので、その場で reparse point にされたら
    // 握ったハンドルでも属性の変化が見える。設計 §2.9 (b)）。
    let own = handle_io::facts(dir.raw_handle())?;
    if let Some(refusal) = dir_refusal(&own) {
        return Err(refused(display, refusal));
    }
    // (2) 本体と付随の名前（無いのは可）。
    for name in names {
        if let Some(handle) = child_relative(dir, name, ChildHow::OpenAttributes)? {
            let facts = handle_io::facts(handle.raw())?;
            if let Some(refusal) = file_refusal(&facts) {
                return Err(refused(&display.join(name), refusal));
            }
        }
    }
    Ok(())
}
