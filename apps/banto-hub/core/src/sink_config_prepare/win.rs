//! [`super::SinkConfigOps`] の Windows の実装（本番）。部品はすべて
//! `banto_secret_store::acl`（unsafe な Win32 をこの crate に複製しない）。

use std::path::{Path, PathBuf};

use banto_secret_store::acl::{FileSecurity, PinnedDir, PinnedDirInfo, PinnedFile, Recurse};
use banto_secret_store::{DirAccess, InstallUserSid, SecretStoreError};

use super::{
    secret_error_text, FileOpenError, SinkConfigOps, SinkDir, SinkFile, EXAMPLE_FILE,
    MAX_EXAMPLE_BYTES,
};
use crate::profile_grant::win::RegistryMachineEnvironment;
use crate::profile_grant::CreateHow;

/// 本番の操作。
pub struct WindowsSinkConfigOps;

impl SinkDir for PinnedDir {
    fn info(&self) -> PinnedDirInfo {
        PinnedDir::info(self).clone()
    }

    fn final_path(&self) -> PathBuf {
        PinnedDir::final_path(self).to_path_buf()
    }

    fn reset_service_only(&self) -> Result<(), String> {
        PinnedDir::reset_acl(self, &DirAccess::service_only(), Recurse::ObjectOnly)
            .map(|_| ())
            .map_err(|err| secret_error_text(&err))
    }
}

impl SinkFile for PinnedFile {
    fn security(&self) -> FileSecurity {
        PinnedFile::security(self).clone()
    }

    fn final_path(&self) -> PathBuf {
        PinnedFile::final_path(self).to_path_buf()
    }

    fn reset_acl(&self) -> Result<(), String> {
        PinnedFile::reset_acl(self)
            .map(|_| ())
            .map_err(|err| secret_error_text(&err))
    }

    fn delete(self) -> Result<(), String> {
        PinnedFile::delete(self).map_err(|err| secret_error_text(&err))
    }
}

/// `ERROR_SHARING_VIOLATION`。
const ERROR_SHARING_VIOLATION: i32 = 32;

/// exe の隣の雛形を、上限つきで読む。
pub fn read_example_beside(exe_dir: &Path) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let path = exe_dir.join(EXAMPLE_FILE);
    let file = std::fs::File::open(&path).map_err(|err| format!("{}: {err}", path.display()))?;
    let mut bytes = Vec::new();
    file.take(MAX_EXAMPLE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|err| format!("{}: {err}", path.display()))?;
    Ok(bytes)
}

impl SinkConfigOps for WindowsSinkConfigOps {
    type Env = RegistryMachineEnvironment;
    type Dir = PinnedDir;
    type File = PinnedFile;

    fn machine_env(&self) -> &RegistryMachineEnvironment {
        &RegistryMachineEnvironment
    }

    fn current_user_sid(&self) -> Result<InstallUserSid, String> {
        banto_secret_store::acl::current_user_sid().map_err(|err| err.to_string())
    }

    fn example_contents(&self) -> Result<Vec<u8>, String> {
        let dir = self
            .exe_dir()
            .ok_or_else(|| "実行ファイルの場所を取れませんでした".to_string())?;
        read_example_beside(&dir)
    }

    fn exe_dir(&self) -> Option<PathBuf> {
        std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(Path::to_path_buf))
    }

    fn exists(&self, path: &Path) -> Result<bool, String> {
        banto_secret_store::acl::inspect_file(path)
            .map(|found| found.is_some())
            .map_err(|err| err.to_string())
    }

    fn pin_dir(&self, dir: &Path) -> Result<Option<PinnedDir>, String> {
        banto_secret_store::acl::pin_dir(dir).map_err(|err| err.to_string())
    }

    fn create_child_dir(
        &self,
        parent: &PinnedDir,
        name: &str,
        how: CreateHow,
    ) -> Result<Option<PinnedDir>, String> {
        let service_only = DirAccess::service_only();
        let access = match how {
            CreateHow::Inherit => None,
            CreateHow::ServiceOnly => Some(&service_only),
        };
        parent
            .create_child_dir(name, access)
            .map_err(|err| err.to_string())
    }

    fn open_child_dir(&self, parent: &PinnedDir, name: &str) -> Result<Option<PinnedDir>, String> {
        parent.open_child_dir(name).map_err(|err| err.to_string())
    }

    fn open_child_file(
        &self,
        parent: &PinnedDir,
        name: &str,
    ) -> Result<Option<PinnedFile>, FileOpenError> {
        parent
            .open_child_file_for_update(name)
            .map_err(|err| match err {
                SecretStoreError::Io {
                    os_error: Some(ERROR_SHARING_VIOLATION),
                    ..
                } => FileOpenError::InUse,
                other => FileOpenError::Other(secret_error_text(&other)),
            })
    }

    fn create_child_file(
        &self,
        parent: &PinnedDir,
        name: &str,
        contents: &[u8],
    ) -> Result<bool, String> {
        parent
            .create_child_file(name, contents)
            .map(|made| made.is_some())
            .map_err(|err| secret_error_text(&err))
    }
}

#[cfg(test)]
mod tests;
