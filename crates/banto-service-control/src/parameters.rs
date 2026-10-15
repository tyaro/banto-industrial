//! サービスごとの設定値（`HKLM\SYSTEM\CurrentControlSet\Services\<service>\Parameters`
//! の REG_SZ）の読み書き（#392 B3-2a）。
//!
//! 登録時（`install`）に記録し、サービス本体が起動時に読む値のため。最初の利用者は
//! ChronoGazer のインストール利用者の SID（B3-2b、2026-10-15 オーナー決定 D1:
//! `Parameters\InstallUserSid`）。値の名前と意味は製品が持ち、この crate は
//! 読み書きの手順だけを持つ（文言は製品名を含まない）。
//!
//! - `Services\<service>` のキーは SCM が登録で作るもので、ここでは作らない。
//!   書くときにサービスのキーが無ければ [`ServiceManagerError::NotFound`]
//!   （登録されていないサービスの名前で `Services` の下にキーを作らない）。
//!   `Parameters` のキーは無ければ作る。登録の解除（`DeleteService`）でサービスの
//!   キーごと消える。
//! - `HKLM\SYSTEM\CurrentControlSet\Services` は Administrators と SYSTEM だけが書ける。
//!   書けなければ [`ServiceManagerError::AccessDenied`]。
//! - 読むときは、サービスのキー・`Parameters`・値のどれが無くても `Ok(None)`
//!   （呼び出し側が「記録が無い」として扱う）。REG_SZ 以外の型は `Other`。
//! - サービス名と値の名前は空・`\`・NUL を断る（`Services` の外のキーを指させない）。
//!
//! テストは HKLM に書かない（GitHub の Windows のランナーは管理者で、本当に書けて
//! しまう）。手順は根のキーとパスを受ける内部の関数にあり、テストは HKCU の
//! 一時的なキーで確かめる。HKLM へは読み取り（無いサービス）だけ。

use crate::ServiceManagerError;

/// サービスのキーの、`HKLM` からのパス。
// 使うのは Windows の実装とテストだけ。
#[cfg_attr(not(windows), allow(dead_code))]
const SERVICES_KEY: &str = r"SYSTEM\CurrentControlSet\Services";
/// 設定値を置くサブキーの名前。
// 使うのは Windows の実装とテストだけ。
#[cfg_attr(not(windows), allow(dead_code))]
const PARAMETERS_SUBKEY: &str = "Parameters";

/// `Services\<service>` のパス（`root` は `Services` のキーのパス）。名前が使えなければ
/// `Other`。
// 使うのは Windows の実装とテストだけ。
#[cfg_attr(not(windows), allow(dead_code))]
fn service_key_path(root: &str, service: &str) -> Result<String, ServiceManagerError> {
    validate_name("サービス名", service)?;
    Ok(format!(r"{root}\{service}"))
}

// 使うのは Windows の実装とテストだけ。
#[cfg_attr(not(windows), allow(dead_code))]
fn validate_name(what: &str, name: &str) -> Result<(), ServiceManagerError> {
    if name.is_empty() || name.contains('\\') || name.contains('\0') {
        return Err(ServiceManagerError::Other(format!(
            "{what}が不正です（空・「\\」・NUL を含む）: {name:?}"
        )));
    }
    Ok(())
}

/// `HKLM\SYSTEM\CurrentControlSet\Services\<service>\Parameters` の `name` に
/// REG_SZ の `value` を書く（モジュール doc）。管理者・SYSTEM だけが書ける。
#[cfg(windows)]
pub fn write_service_parameter(
    service: &str,
    name: &str,
    value: &str,
) -> Result<(), ServiceManagerError> {
    use windows_sys::Win32::System::Registry::HKEY_LOCAL_MACHINE;
    registry::write_parameter_in(HKEY_LOCAL_MACHINE, SERVICES_KEY, service, name, value)
}

/// `HKLM\SYSTEM\CurrentControlSet\Services\<service>\Parameters` の `name`（REG_SZ）を
/// 読む。サービス・`Parameters`・値のどれかが無ければ `Ok(None)`（モジュール doc）。
#[cfg(windows)]
pub fn read_service_parameter(
    service: &str,
    name: &str,
) -> Result<Option<String>, ServiceManagerError> {
    use windows_sys::Win32::System::Registry::HKEY_LOCAL_MACHINE;
    registry::read_parameter_in(HKEY_LOCAL_MACHINE, SERVICES_KEY, service, name)
}

#[cfg(windows)]
mod registry {
    use std::ffi::OsStr;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};

    use windows_sys::Win32::Foundation::{
        ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_PATH_NOT_FOUND,
        ERROR_SUCCESS, WIN32_ERROR,
    };
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegGetValueW, RegOpenKeyExW, RegSetKeyValueW, HKEY, KEY_CREATE_SUB_KEY,
        KEY_SET_VALUE, REG_SZ, RRF_RT_REG_SZ,
    };

    use super::{service_key_path, validate_name, PARAMETERS_SUBKEY};
    use crate::ServiceManagerError;

    fn wide(text: &str) -> Vec<u16> {
        OsStr::new(text).encode_wide().chain(Some(0)).collect()
    }

    fn os_error(code: WIN32_ERROR) -> String {
        std::io::Error::from_raw_os_error(code as i32).to_string()
    }

    fn is_missing(code: WIN32_ERROR) -> bool {
        code == ERROR_FILE_NOT_FOUND || code == ERROR_PATH_NOT_FOUND
    }

    /// 開いたキー（drop で閉じる）。
    struct Key(HKEY);

    impl Drop for Key {
        fn drop(&mut self) {
            // SAFETY: `RegOpenKeyExW` が返した、まだ閉じていないハンドル。
            unsafe {
                RegCloseKey(self.0);
            }
        }
    }

    /// [`super::write_service_parameter`] の本体（`root` の下の `services_path` を
    /// `Services` のキーとして扱う。テストは HKCU の一時的なキーを渡す）。
    pub(super) fn write_parameter_in(
        root: HKEY,
        services_path: &str,
        service: &str,
        name: &str,
        value: &str,
    ) -> Result<(), ServiceManagerError> {
        let service_path = service_key_path(services_path, service)?;
        validate_name("値の名前", name)?;
        if value.contains('\0') {
            return Err(ServiceManagerError::Other(
                "値に NUL を含めることはできません".to_string(),
            ));
        }

        // サービスのキーは作らない（開くだけ）。`Parameters` は `RegSetKeyValueW` が
        // 無ければ作る。
        let service_path_w = wide(&service_path);
        let mut key: HKEY = std::ptr::null_mut();
        // SAFETY: パスは NUL 終端の UTF-16 で、呼び出しが返るまで生きている。
        // `key` は成功したときだけ書かれ、`Key` が閉じる。
        let status = unsafe {
            RegOpenKeyExW(
                root,
                service_path_w.as_ptr(),
                0,
                KEY_CREATE_SUB_KEY | KEY_SET_VALUE,
                &mut key,
            )
        };
        match status {
            ERROR_SUCCESS => {}
            code if is_missing(code) => {
                return Err(ServiceManagerError::NotFound(service.to_string()))
            }
            ERROR_ACCESS_DENIED => return Err(ServiceManagerError::AccessDenied),
            code => {
                return Err(ServiceManagerError::Other(format!(
                    "サービスのレジストリのキーを開けませんでした: {}",
                    os_error(code)
                )))
            }
        }
        let key = Key(key);

        let subkey = wide(PARAMETERS_SUBKEY);
        let name_w = wide(name);
        let data = wide(value);
        let bytes = u32::try_from(data.len() * 2)
            .map_err(|_| ServiceManagerError::Other("値が長すぎます".to_string()))?;
        // SAFETY: 各文字列は NUL 終端の UTF-16 で、呼び出しが返るまで生きている。
        // `data` の長さ（バイト）は終端の NUL を含む（REG_SZ の決まり）。
        let status = unsafe {
            RegSetKeyValueW(
                key.0,
                subkey.as_ptr(),
                name_w.as_ptr(),
                REG_SZ,
                data.as_ptr().cast(),
                bytes,
            )
        };
        match status {
            ERROR_SUCCESS => Ok(()),
            ERROR_ACCESS_DENIED => Err(ServiceManagerError::AccessDenied),
            code => Err(ServiceManagerError::Other(format!(
                "サービスの設定値を書けませんでした: {}",
                os_error(code)
            ))),
        }
    }

    /// [`super::read_service_parameter`] の本体（[`write_parameter_in`] と同じ形）。
    pub(super) fn read_parameter_in(
        root: HKEY,
        services_path: &str,
        service: &str,
        name: &str,
    ) -> Result<Option<String>, ServiceManagerError> {
        let service_path = service_key_path(services_path, service)?;
        validate_name("値の名前", name)?;
        let path = wide(&format!(r"{service_path}\{PARAMETERS_SUBKEY}"));
        let name_w = wide(name);

        // 大きさを聞いてから読む。間で書き換わって大きくなったら（`ERROR_MORE_DATA`）
        // 読み直す。
        let mut capacity_bytes = 0u32;
        // SAFETY: データの置き場を渡さず、大きさだけを聞く。
        let status = unsafe {
            RegGetValueW(
                root,
                path.as_ptr(),
                name_w.as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut capacity_bytes,
            )
        };
        if let Some(result) = read_status(status) {
            return result;
        }
        for _ in 0..4 {
            let mut buffer = vec![0u16; (capacity_bytes as usize).div_ceil(2).max(1)];
            let mut size = u32::try_from(buffer.len() * 2).unwrap_or(u32::MAX);
            // SAFETY: `buffer` は `size` バイトの置き場で、呼び出しが返るまで生きている。
            let status = unsafe {
                RegGetValueW(
                    root,
                    path.as_ptr(),
                    name_w.as_ptr(),
                    RRF_RT_REG_SZ,
                    std::ptr::null_mut(),
                    buffer.as_mut_ptr().cast(),
                    &mut size,
                )
            };
            if status == ERROR_MORE_DATA {
                capacity_bytes = size.max(capacity_bytes.saturating_mul(2));
                continue;
            }
            if let Some(result) = read_status(status) {
                return result;
            }
            // `size` は終端の NUL を含むバイト数（`RRF_RT_REG_SZ` は終端を保証する）。
            let units = (size as usize / 2).min(buffer.len());
            let text = &buffer[..units];
            let text = match text.iter().position(|&unit| unit == 0) {
                Some(end) => &text[..end],
                None => text,
            };
            return std::ffi::OsString::from_wide(text)
                .into_string()
                .map(Some)
                .map_err(|_| {
                    ServiceManagerError::Other(
                        "サービスの設定値が Unicode の文字列として読めません".to_string(),
                    )
                });
        }
        Err(ServiceManagerError::Other(
            "サービスの設定値を読む間に値が変わり続けました".to_string(),
        ))
    }

    /// 読み取りの状態: 続けて読むなら `None`、結果が決まったら `Some`。
    fn read_status(status: WIN32_ERROR) -> Option<Result<Option<String>, ServiceManagerError>> {
        match status {
            ERROR_SUCCESS | ERROR_MORE_DATA => None,
            code if is_missing(code) => Some(Ok(None)),
            ERROR_ACCESS_DENIED => Some(Err(ServiceManagerError::AccessDenied)),
            code => Some(Err(ServiceManagerError::Other(format!(
                "サービスの設定値を読めませんでした: {}",
                os_error(code)
            )))),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use windows_sys::Win32::System::Registry::{
            RegDeleteTreeW, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE,
        };

        /// HKCU の下の一時的な `Services` 相当のキー（drop で消す）。
        struct TempServices {
            base: String,
        }

        impl TempServices {
            fn new(tag: &str) -> Self {
                let nanos = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos();
                Self {
                    base: format!(
                        r"Software\banto-service-control-test-{tag}-{}-{nanos}",
                        std::process::id()
                    ),
                }
            }

            fn services(&self) -> String {
                format!(r"{}\Services", self.base)
            }

            /// SCM の登録の代わりに、サービスのキーを作る（値を 1 つ置く）。
            fn register(&self, service: &str) {
                let path = wide(&format!(r"{}\{service}", self.services()));
                let name = wide("Type");
                let data = 16u32.to_le_bytes();
                // SAFETY: テストの HKCU の一時的なキー。各値は呼び出しの間生きている。
                let status = unsafe {
                    RegSetKeyValueW(
                        HKEY_CURRENT_USER,
                        path.as_ptr(),
                        name.as_ptr(),
                        windows_sys::Win32::System::Registry::REG_DWORD,
                        data.as_ptr().cast(),
                        4,
                    )
                };
                assert_eq!(status, ERROR_SUCCESS, "{}", os_error(status));
            }

            fn key_exists(&self, relative: &str) -> bool {
                let path = wide(&format!(r"{}\{relative}", self.services()));
                let mut key: HKEY = std::ptr::null_mut();
                // SAFETY: 読むだけで開き、成功したら閉じる。
                let status = unsafe {
                    RegOpenKeyExW(
                        HKEY_CURRENT_USER,
                        path.as_ptr(),
                        0,
                        windows_sys::Win32::System::Registry::KEY_READ,
                        &mut key,
                    )
                };
                if status == ERROR_SUCCESS {
                    drop(Key(key));
                    true
                } else {
                    false
                }
            }
        }

        impl Drop for TempServices {
            fn drop(&mut self) {
                let path = wide(&self.base);
                // SAFETY: テストが作った HKCU の一時的なキーだけを消す。
                unsafe {
                    RegDeleteTreeW(HKEY_CURRENT_USER, path.as_ptr());
                }
            }
        }

        #[test]
        fn a_written_value_reads_back_and_can_be_overwritten() {
            let temp = TempServices::new("roundtrip");
            temp.register("Svc");
            let services = temp.services();
            assert_eq!(
                read_parameter_in(HKEY_CURRENT_USER, &services, "Svc", "InstallUserSid").unwrap(),
                None,
                "まだ書いていない"
            );
            write_parameter_in(
                HKEY_CURRENT_USER,
                &services,
                "Svc",
                "InstallUserSid",
                "S-1-5-21-1-2-3-1001",
            )
            .unwrap();
            assert!(temp.key_exists(r"Svc\Parameters"));
            assert_eq!(
                read_parameter_in(HKEY_CURRENT_USER, &services, "Svc", "InstallUserSid").unwrap(),
                Some("S-1-5-21-1-2-3-1001".to_string())
            );
            // 上書きと、日本語・長い値。
            let long = format!("値-{}", "x".repeat(4000));
            write_parameter_in(HKEY_CURRENT_USER, &services, "Svc", "InstallUserSid", &long)
                .unwrap();
            assert_eq!(
                read_parameter_in(HKEY_CURRENT_USER, &services, "Svc", "InstallUserSid").unwrap(),
                Some(long)
            );
            // 空の値も書ける・読める。
            write_parameter_in(HKEY_CURRENT_USER, &services, "Svc", "Empty", "").unwrap();
            assert_eq!(
                read_parameter_in(HKEY_CURRENT_USER, &services, "Svc", "Empty").unwrap(),
                Some(String::new())
            );
        }

        /// 書くときにサービスのキーが無ければ `NotFound` で、キーを作らない。読むときは
        /// サービス・`Parameters`・値のどれが無くても `None`。
        #[test]
        fn a_missing_service_is_not_created_and_reads_as_none() {
            let temp = TempServices::new("missing");
            let services = temp.services();
            assert!(matches!(
                write_parameter_in(HKEY_CURRENT_USER, &services, "Nope", "X", "v"),
                Err(ServiceManagerError::NotFound(name)) if name == "Nope"
            ));
            assert!(!temp.key_exists("Nope"), "無いサービスのキーを作らない");
            assert_eq!(
                read_parameter_in(HKEY_CURRENT_USER, &services, "Nope", "X").unwrap(),
                None
            );
            temp.register("Svc");
            assert_eq!(
                read_parameter_in(HKEY_CURRENT_USER, &services, "Svc", "X").unwrap(),
                None,
                "Parameters が無い"
            );
        }

        /// REG_SZ 以外の値は `Other`（`None` と区別する）。
        #[test]
        fn a_value_of_another_type_is_an_error() {
            let temp = TempServices::new("type");
            temp.register("Svc");
            // `Type`（REG_DWORD）をサービスのキーに置いたので、`Parameters` に同じ形で置く。
            let path = wide(&format!(r"{}\Svc\Parameters", temp.services()));
            let name = wide("Dword");
            let data = 1u32.to_le_bytes();
            // SAFETY: テストの HKCU の一時的なキー。
            let status = unsafe {
                RegSetKeyValueW(
                    HKEY_CURRENT_USER,
                    path.as_ptr(),
                    name.as_ptr(),
                    windows_sys::Win32::System::Registry::REG_DWORD,
                    data.as_ptr().cast(),
                    4,
                )
            };
            assert_eq!(status, ERROR_SUCCESS);
            assert!(matches!(
                read_parameter_in(HKEY_CURRENT_USER, &temp.services(), "Svc", "Dword"),
                Err(ServiceManagerError::Other(_))
            ));
        }

        /// HKLM は読むだけ（無いサービスは `None`）。書かない（モジュール doc）。
        #[test]
        fn reading_a_missing_service_under_hklm_is_none() {
            let service = format!("banto-service-control-test-missing-{}", std::process::id());
            assert_eq!(
                super::super::read_service_parameter(&service, "X").unwrap(),
                None
            );
            assert_eq!(
                read_parameter_in(
                    HKEY_LOCAL_MACHINE,
                    super::super::SERVICES_KEY,
                    &service,
                    "X"
                )
                .unwrap(),
                None
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 名前に `\`・NUL・空を許さない（`Services` の外のキーを指させない）。
    #[test]
    fn names_that_could_escape_the_service_key_are_refused() {
        assert_eq!(
            service_key_path(SERVICES_KEY, "ChronoGazer").unwrap(),
            r"SYSTEM\CurrentControlSet\Services\ChronoGazer"
        );
        for bad in ["", r"..\Other", r"A\B", "A\0B"] {
            assert!(
                matches!(
                    service_key_path(SERVICES_KEY, bad),
                    Err(ServiceManagerError::Other(_))
                ),
                "{bad:?}"
            );
            assert!(validate_name("値の名前", bad).is_err(), "{bad:?}");
        }
        assert!(validate_name("値の名前", "InstallUserSid").is_ok());
    }
}
