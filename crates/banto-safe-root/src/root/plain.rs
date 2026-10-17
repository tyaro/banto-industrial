//! [`super::Mode::Plain`] の実装（`std::fs` のまま。リンクを辿る。デスクトップ・コンソール・
//! 非 Windows の挙動を変えないため）。

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use super::{
    nonce_seed, temp_name, Entry, EntryKind, ManagedDir, ManagedRefusal, ManagedSpec, RootRefusal,
    RootSpec, TEMP_TRIES,
};
use crate::error::{IoOp, Result, SafeRootError};

fn io(op: IoOp) -> impl Fn(std::io::Error) -> SafeRootError {
    move |e| SafeRootError::io(op, &e)
}

/// 名前 1 要素の確かめ（Plain でも、名前から区切り・`..` を通さない）。
fn check_name(name: &str) -> Result<()> {
    if crate::guard::is_plain_child_name(name) {
        Ok(())
    } else {
        Err(super::invalid_name())
    }
}

pub(super) fn open_root(spec: &RootSpec) -> std::result::Result<ManagedDir, RootRefusal> {
    let root_io = |op| move |e: std::io::Error| RootRefusal::Io(SafeRootError::io(op, &e));
    match std::fs::metadata(&spec.expected) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => return Err(RootRefusal::Refused(crate::error::Refusal::NotADirectory)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if !spec.create {
                return Err(RootRefusal::Absent);
            }
            std::fs::create_dir_all(&spec.expected).map_err(root_io(IoOp::CreateDir))?;
        }
        Err(e) => return Err(root_io(IoOp::InspectDir)(e)),
    }
    let final_path = std::fs::canonicalize(&spec.expected).map_err(root_io(IoOp::InspectDir))?;
    Ok(ManagedDir::plain(spec.expected.clone(), final_path))
}

pub(super) fn open_managed_one(
    parent: &ManagedDir,
    spec: &ManagedSpec,
) -> std::result::Result<ManagedDir, ManagedRefusal> {
    let path = parent.final_path.join(spec.name);
    let refuse_io = |op| move |e: std::io::Error| ManagedRefusal::Io(SafeRootError::io(op, &e));
    match std::fs::metadata(&path) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => {
            return Err(ManagedRefusal::Refused(
                crate::error::Refusal::NotADirectory,
            ))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if !spec.create {
                return Err(ManagedRefusal::Absent);
            }
            match std::fs::create_dir(&path) {
                Ok(()) => {}
                // 確かめてから作るまでの間に誰かが作った（作り直しは冪等）。
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(refuse_io(IoOp::CreateDir)(e)),
            }
        }
        Err(e) => return Err(refuse_io(IoOp::InspectDir)(e)),
    }
    let final_path = std::fs::canonicalize(&path).map_err(refuse_io(IoOp::InspectDir))?;
    Ok(ManagedDir::plain(
        parent.display.join(spec.name),
        final_path,
    ))
}

pub(super) fn create_new(dir: &Path, name: &str) -> Result<Option<File>> {
    check_name(name)?;
    match OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(dir.join(name))
    {
        Ok(file) => Ok(Some(file)),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(None),
        Err(e) => Err(io(IoOp::CreateFile)(e)),
    }
}

pub(super) fn open_append(dir: &Path, name: &str, create: bool) -> Result<File> {
    check_name(name)?;
    OpenOptions::new()
        .append(true)
        .create(create)
        .open(dir.join(name))
        .map_err(io(if create {
            IoOp::CreateFile
        } else {
            IoOp::OpenFile
        }))
}

pub(super) fn open_read(dir: &Path, name: &str) -> Result<Option<File>> {
    check_name(name)?;
    match File::open(dir.join(name)) {
        Ok(file) => Ok(Some(file)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io(IoOp::OpenFile)(e)),
    }
}

pub(super) fn write_atomic(dir: &Path, name: &str, contents: &[u8]) -> Result<()> {
    check_name(name)?;
    let seed = nonce_seed();
    let mut created: Option<(PathBuf, File)> = None;
    for step in 0..TEMP_TRIES {
        let temp = temp_name(name, seed.wrapping_add(step)).ok_or_else(super::invalid_name)?;
        let path = dir.join(&temp);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => {
                created = Some((path, file));
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(io(IoOp::CreateFile)(e)),
        }
    }
    let Some((temp_path, mut file)) = created else {
        return Err(io(IoOp::CreateFile)(std::io::Error::from(
            std::io::ErrorKind::AlreadyExists,
        )));
    };
    let written = file
        .write_all(contents)
        .and_then(|()| file.sync_all())
        .map_err(io(IoOp::WriteFile));
    drop(file);
    let result = written
        .and_then(|()| std::fs::rename(&temp_path, dir.join(name)).map_err(io(IoOp::RenameFile)));
    if result.is_err() {
        let _ = std::fs::remove_file(&temp_path);
    }
    result
}

pub(super) fn remove(dir: &Path, name: &str) -> Result<bool> {
    check_name(name)?;
    match std::fs::remove_file(dir.join(name)) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(io(IoOp::RemoveFile)(e)),
    }
}

pub(super) fn list(dir: &Path) -> Result<Vec<Entry>> {
    let mut entries = Vec::new();
    for item in std::fs::read_dir(dir).map_err(io(IoOp::ListDir))? {
        let item = item.map_err(io(IoOp::ListDir))?;
        let Ok(name) = item.file_name().into_string() else {
            continue; // UTF-8 でない名前は扱わない（名前で選ぶ相手が無い）。
        };
        let meta = std::fs::symlink_metadata(item.path()).map_err(io(IoOp::ListDir))?;
        entries.push(Entry {
            name,
            kind: if meta.is_dir() {
                EntryKind::Dir
            } else {
                EntryKind::File
            },
            reparse_tag: meta.file_type().is_symlink().then_some(0),
            len: meta.len(),
        });
    }
    Ok(entries)
}
