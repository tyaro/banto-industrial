//! [`super::MachineEnvironment`] の Windows の実装（HKLM の読み取りと既知のフォルダ。
//! 読むだけで何も変えない）。chronogazer-elev の `win` から移した（#626 PR-A）。

use std::path::PathBuf;

use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_SUCCESS};
use windows_sys::Win32::Storage::FileSystem::GetDriveTypeW;
use windows_sys::Win32::System::Com::CoTaskMemFree;
use windows_sys::Win32::System::Registry::{
    RegGetValueW, HKEY_LOCAL_MACHINE, RRF_NOEXPAND, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ,
};
use windows_sys::Win32::UI::Shell::{
    FOLDERID_LocalAppData, FOLDERID_ProgramData, FOLDERID_ProgramFiles,
    FOLDERID_ProgramFilesCommon, FOLDERID_ProgramFilesCommonX86, FOLDERID_ProgramFilesX86,
    FOLDERID_Public, FOLDERID_System, FOLDERID_SystemX86, FOLDERID_UserProfiles, FOLDERID_Windows,
    SHGetKnownFolderPath,
};

use super::{DriveKind, KnownFolders, MachineEnvironment, MACHINE_ENVIRONMENT_KEY};

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// マシンの環境を HKLM と既知のフォルダから読む（このプロセスの環境変数は読まない）。
#[derive(Debug, Clone, Copy, Default)]
pub struct RegistryMachineEnvironment;

impl MachineEnvironment for RegistryMachineEnvironment {
    fn machine_variable(&self, name: &str) -> Result<Option<String>, String> {
        read_hklm_string(MACHINE_ENVIRONMENT_KEY, name)
    }

    fn program_data(&self) -> Result<PathBuf, String> {
        known_folder(&FOLDERID_ProgramData)
    }

    fn known_folders(&self) -> Result<KnownFolders, String> {
        // どれか 1 つでも取れなければ止める（断る一覧が欠けたまま進まない）。
        let get = |ids: &[&windows_sys::core::GUID]| -> Result<Vec<PathBuf>, String> {
            ids.iter().map(|id| known_folder(id)).collect()
        };
        Ok(KnownFolders {
            forbidden_inside: get(&[
                &FOLDERID_Windows,
                &FOLDERID_System,
                &FOLDERID_SystemX86,
                &FOLDERID_ProgramFiles,
                &FOLDERID_ProgramFilesX86,
                &FOLDERID_ProgramFilesCommon,
                &FOLDERID_ProgramFilesCommonX86,
                &FOLDERID_Public,
            ])?,
            forbidden_itself: get(&[&FOLDERID_ProgramData, &FOLDERID_UserProfiles])?,
        })
    }
}

/// マシンの Program Files と Program Files (x86)（取れたものだけ。既知のフォルダから。
/// このプロセスの環境変数は読まない）。
pub fn program_files_dirs() -> Vec<PathBuf> {
    [&FOLDERID_ProgramFiles, &FOLDERID_ProgramFilesX86]
        .into_iter()
        .filter_map(|id| known_folder(id).ok())
        .collect()
}

/// 今の利用者の `%LOCALAPPDATA%`（`FOLDERID_LocalAppData`。このプロセスの環境変数は読まない。
/// #1121 PR1a）。プロセスのトークンの利用者のもの（偽装していないスレッドから呼ぶ）。
pub fn local_app_data() -> Result<PathBuf, String> {
    known_folder(&FOLDERID_LocalAppData)
}

/// `GetDriveTypeW` の値（`Win32_System_WindowsProgramming` の定数。feature を足さずに済むよう
/// ここに置く）。
const DRIVE_REMOVABLE: u32 = 2;
const DRIVE_FIXED: u32 = 3;
const DRIVE_REMOTE: u32 = 4;

/// パスの先頭の解釈（[`classify_path_prefix`] の結果）。
#[derive(Debug, PartialEq, Eq)]
enum PathPrefix {
    /// UNC（OS に尋ねずネットワーク）。
    Network,
    /// ドライブ文字（`X:`）で始まる。
    Drive(char),
}

/// `text` の先頭を解釈する純粋な関数（OS に尋ねない。`/` は `\` と同じに扱い、接頭辞の
/// 大文字小文字は区別しない）。`\\?\UNC\` は UNC、`\\?\X:` は接頭辞を外してドライブ文字、
/// `\\?\` の後がドライブ文字でないもの（`\\?\Volume{..}\` など）と `\\.\` は推測せず
/// `Err`、ほかの `\\` は UNC、`X:` はそのままドライブ文字。
fn classify_path_prefix(text: &str) -> Result<PathPrefix, String> {
    let norm: String = text.replace('/', "\\");
    let drive_of = |rest: &str| -> Option<char> {
        let mut chars = rest.chars();
        match (chars.next(), chars.next()) {
            (Some(letter), Some(':')) if letter.is_ascii_alphabetic() => Some(letter),
            _ => None,
        }
    };
    let starts = |prefix: &str| {
        norm.get(..prefix.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
    };
    if starts("\\\\?\\UNC\\") {
        return Ok(PathPrefix::Network);
    }
    if starts("\\\\?\\") {
        return drive_of(&norm[4..]).map(PathPrefix::Drive).ok_or_else(|| {
            format!("パス（{text}）の \\\\?\\ の後がドライブ文字でないため種類を判定できません")
        });
    }
    if starts("\\\\.\\") {
        return Err(format!(
            "パス（{text}）はデバイス名前空間（\\\\.\\）で始まるため種類を判定できません"
        ));
    }
    if starts("\\\\") {
        return Ok(PathPrefix::Network);
    }
    drive_of(&norm)
        .map(PathPrefix::Drive)
        .ok_or_else(|| format!("パス（{text}）がドライブ文字で始まりません"))
}

/// `path`（本当のパス。`PinnedDir::final_path` の形）が載っているドライブの種類（#1121 PR1a。
/// 読むだけ）。先頭は [`classify_path_prefix`] で解釈する: UNC（`\\server\..`・
/// `\\?\UNC\..`）は OS に尋ねずに [`DriveKind::Network`]、`X:\..` と `\\?\X:\..`（verbatim の
/// DOS 形式）はドライブ文字の `GetDriveTypeW`。`\\?\Volume{..}` や `\\.\` など判定できない形と、
/// ドライブ文字で始まらないパスは理由を返す。
pub fn drive_kind(path: &std::path::Path) -> Result<DriveKind, String> {
    let text = path.to_string_lossy();
    let letter = match classify_path_prefix(&text)? {
        PathPrefix::Network => return Ok(DriveKind::Network),
        PathPrefix::Drive(letter) => letter,
    };
    let root = wide(&format!("{letter}:\\"));
    // SAFETY: `root` は NUL 終端の `X:\`。
    let kind = unsafe { GetDriveTypeW(root.as_ptr()) };
    Ok(match kind {
        DRIVE_FIXED => DriveKind::Fixed,
        DRIVE_REMOTE => DriveKind::Network,
        DRIVE_REMOVABLE => DriveKind::Removable,
        other => DriveKind::Other(other),
    })
}

/// `HKLM\<key>` の `name`（REG_SZ か REG_EXPAND_SZ。**展開しない**生の値）。無ければ
/// `Ok(None)`。
fn read_hklm_string(key: &str, name: &str) -> Result<Option<String>, String> {
    let key = wide(key);
    let name = wide(name);
    let flags = RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | RRF_NOEXPAND;
    let mut buffer: Vec<u16> = vec![0; 260];
    for _ in 0..4 {
        let mut bytes = (buffer.len() * 2) as u32;
        // SAFETY: `key`・`name` は NUL 終端、`buffer` は `bytes` バイト書ける。
        let status = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                key.as_ptr(),
                name.as_ptr(),
                flags,
                std::ptr::null_mut(),
                buffer.as_mut_ptr().cast(),
                &mut bytes,
            )
        };
        match status {
            ERROR_SUCCESS => {
                let len = (bytes as usize / 2).min(buffer.len());
                let text = &buffer[..len];
                let end = text.iter().position(|&c| c == 0).unwrap_or(text.len());
                return Ok(Some(String::from_utf16_lossy(&text[..end])));
            }
            ERROR_FILE_NOT_FOUND => return Ok(None),
            ERROR_MORE_DATA => {
                buffer = vec![0; (bytes as usize).div_ceil(2) + 1];
            }
            other => return Err(std::io::Error::from_raw_os_error(other as i32).to_string()),
        }
    }
    Err("値が読み取りの間に伸び続けました".to_string())
}

/// 既知のフォルダ（このプロセスの環境変数ではなく、OS の設定から）。
fn known_folder(id: &windows_sys::core::GUID) -> Result<PathBuf, String> {
    let mut path: windows_sys::core::PWSTR = std::ptr::null_mut();
    // SAFETY: `id` は有効な GUID。成功・失敗に関わらず `path` は CoTaskMemFree で解放する。
    let hr = unsafe { SHGetKnownFolderPath(id, 0, std::ptr::null_mut(), &mut path) };
    let result = if hr >= 0 && !path.is_null() {
        let mut len = 0;
        // SAFETY: 成功した SHGetKnownFolderPath は NUL 終端の文字列を返す。
        while unsafe { *path.add(len) } != 0 {
            len += 1;
        }
        // SAFETY: `len` 文字ぶんは上で読めた範囲。
        let slice = unsafe { std::slice::from_raw_parts(path, len) };
        Ok(PathBuf::from(String::from_utf16_lossy(slice)))
    } else {
        Err(format!(
            "SHGetKnownFolderPath が失敗しました (HRESULT {hr:#010x})"
        ))
    };
    // SAFETY: `path` は SHGetKnownFolderPath が確保した領域か null（null は何もしない）。
    unsafe { CoTaskMemFree(path as *const core::ffi::c_void) };
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 読むだけ: マシンの ProgramData・Program Files は既知のフォルダから取れ（環境変数を
    /// 見ない）、ProgramData の下の製品のフォルダは断られず、ProgramData そのものは断られる。
    #[test]
    fn known_folders_resolve() {
        let program_data = RegistryMachineEnvironment.program_data().unwrap();
        assert!(program_data.is_absolute(), "{program_data:?}");
        let dirs = program_files_dirs();
        assert_eq!(dirs.len(), 2, "{dirs:?}");
        assert!(dirs.iter().all(|d| d.is_absolute()));
        let known = RegistryMachineEnvironment.known_folders().unwrap();
        assert_eq!(known.forbidden_inside.len(), 8);
        assert_eq!(known.forbidden_itself.len(), 2);
        super::super::reject_well_known_root(&program_data.join("BantoHub"), &known).unwrap();
        assert!(super::super::reject_well_known_root(&program_data, &known).is_err());
    }

    /// 読むだけ: 今の利用者の `%LOCALAPPDATA%` は既知のフォルダから絶対パスで取れ、その
    /// ドライブの種類が読める。UNC は OS に尋ねずにネットワーク、ドライブ文字で始まらない
    /// パスは理由を返す。
    #[test]
    fn local_app_data_and_drive_kind_resolve() {
        let local = local_app_data().unwrap();
        assert!(local.is_absolute(), "{local:?}");
        drive_kind(&local).unwrap();
        assert_eq!(
            drive_kind(std::path::Path::new(r"\\server\share\dir")),
            Ok(DriveKind::Network)
        );
        assert!(drive_kind(std::path::Path::new(r"relative\dir")).is_err());
        // verbatim: DOS 形式は同じドライブと同じ種類、UNC 形式はネットワーク。
        assert_eq!(
            drive_kind(std::path::Path::new(r"\\?\C:\Windows")),
            drive_kind(std::path::Path::new(r"C:\Windows"))
        );
        for unc in [
            r"\\?\UNC\server\share\x",
            r"\\?\unc\server\share\x",
            "//?/UNC/server/share/x",
            r"\\server\share\x",
        ] {
            assert_eq!(
                drive_kind(std::path::Path::new(unc)),
                Ok(DriveKind::Network),
                "{unc}"
            );
        }
        for bad in [
            r"\\?\Volume{00000000-0000-0000-0000-000000000000}\",
            r"\\.\C:",
            "//./C:",
        ] {
            assert!(drive_kind(std::path::Path::new(bad)).is_err(), "{bad}");
        }
    }

    /// 純粋: 接頭辞の解釈の表。
    #[test]
    fn classify_path_prefix_table() {
        use PathPrefix::{Drive, Network};
        let ok = [
            (r"C:\x", Drive('C')),
            (r"\\?\C:\x", Drive('C')),
            ("//?/d:/x", Drive('d')),
            (r"\\?\UNC\s\h", Network),
            ("//?/unc/s/h", Network),
            (r"\\s\h", Network),
            ("//s/h", Network),
            (r"\\?\Z:", Drive('Z')),
        ];
        for (text, want) in ok {
            assert_eq!(classify_path_prefix(text), Ok(want), "{text}");
        }
        for bad in [
            r"\\?\Volume{1}\",
            r"\\?\",
            r"\\.\C:",
            r"\\.\UNC\s\h",
            "rel",
            "",
        ] {
            assert!(classify_path_prefix(bad).is_err(), "{bad}");
        }
    }

    /// 読むだけ: 無い値は `None`、ある値（`Path` は必ずある）は読める（`RRF_NOEXPAND` で
    /// REG_EXPAND_SZ も保存された文字のまま返る。`%…%` を展開しない）。
    #[test]
    fn hklm_missing_value_is_none_and_existing_reads() {
        assert_eq!(
            RegistryMachineEnvironment
                .machine_variable("BANTO_SECRET_STORE_TEST_VALUE_THAT_DOES_NOT_EXIST"),
            Ok(None)
        );
        let path = RegistryMachineEnvironment
            .machine_variable("Path")
            .unwrap()
            .expect("マシンの Path");
        assert!(!path.is_empty());
    }
}
