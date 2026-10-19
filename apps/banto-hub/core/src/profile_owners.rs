//! 運用モードの profile を緩めるときに「変更」を戻す相手（owner の組）の記録（#661 661-2、
//! docs/banto-hub-t17-design.md §1 の「2026-10-19 オーナー決定（#661）」の「戻す相手の記録」）。
//!
//! ## 設計意図
//!
//! - **置き場は HKLM**: `HKLM\SYSTEM\CurrentControlSet\Services\BantoHub\Parameters\Profiles\<id>`
//!   の `Owners`（REG_MULTI_SZ、利用者の SID を 1 つずつ）。締めた後の profile の DACL からは
//!   読めない（利用者は「読み取りと実行」だけになる）ので、締める前に決めた組をここに置き、
//!   緩めるとき（STOP・`relax-only`・締めたまま落ちた後の再開）はここから読む。profile の中の
//!   ファイルや設定 DB には置かない（設定モードの利用者が書き換えられる）。`Services` の下は
//!   Administrators と SYSTEM だけが書ける。
//! - **書くのはサービス（設定モードの形から締める直前）と昇格ヘルパー（661-3 の
//!   `grant-profile-acl`）だけ**。どちらも同じ関数（[`write_owners`]）を使う。
//!   `Services\BantoHub` のキー自体は作らない（登録されていないサービスの名前で `Services`
//!   の下にキーを作らない。`banto_service_control::write_service_parameter` と同じ作法）。
//! - **読むときの確かめ**は呼ぶ側（`crate::profile_mode::check_stored_owners`）: 空でない・
//!   どれも利用者の SID の形（`InstallUserSid::parse`）であること。ここは読み書きの手順と、
//!   REG_MULTI_SZ の形の変換（純関数、全 OS でテストする）だけを持つ。
//!
//! ## テスト
//!
//! HKLM には書かない（GitHub の Windows のランナーは管理者で、本当に書けてしまう）。
//! 手順は根のキーとパスを受ける内部の関数にあり、テストは HKCU の一時的なキーで確かめる
//! （`banto_service_control` の `Parameters` のテストと同じ形）。

use crate::profile_paths::validate_profile_id;
#[cfg(windows)]
use banto_safe_root::InstallUserSid;

/// 記録の値の名前。
pub const OWNERS_VALUE: &str = "Owners";

/// `Services` のキーの、`HKLM` からのパス。
// 使うのは Windows の実装とテストだけ。
#[cfg_attr(not(windows), allow(dead_code))]
const SERVICES_KEY: &str = r"SYSTEM\CurrentControlSet\Services";

/// `Services\BantoHub` の下の、profile ごとの記録のキーの相対パス
/// （`Parameters\Profiles\<id>`）。profile-id は [`validate_profile_id`] に通る物だけ
/// （`\`・`..` を含まない）。
pub fn owners_subkey(profile_id: &str) -> Result<String, String> {
    validate_profile_id(profile_id).map_err(|err| format!("profile id が不正です: {err}"))?;
    // NUL は UTF-16 にしたときにパスを途中で切る（別のキーを指す）。
    if profile_id.contains('\0') {
        return Err("profile id が不正です（NUL を含む）".to_string());
    }
    Ok(format!(r"Parameters\Profiles\{profile_id}"))
}

/// 記録の場所の表示（ログ・案内用）。
pub fn owners_location(profile_id: &str) -> String {
    format!(
        r"HKLM\{SERVICES_KEY}\{}\Parameters\Profiles\{profile_id}\{OWNERS_VALUE}",
        crate::service_manager::SERVICE_NAME
    )
}

/// SID の組を REG_MULTI_SZ の中身（UTF-16、各文字列の NUL と最後の空の文字列の NUL）にする。
/// 空の組は空の文字列 1 つ（NUL 2 つ）。
pub fn encode_multi_sz<S: AsRef<str>>(items: &[S]) -> Vec<u16> {
    let mut out = Vec::new();
    for item in items {
        out.extend(item.as_ref().encode_utf16());
        out.push(0);
    }
    if items.is_empty() {
        out.push(0);
    }
    out.push(0);
    out
}

/// REG_MULTI_SZ の中身を文字列の列にする（最初の空の文字列で終わる。末尾の NUL が欠けて
/// いても読めた分を返す）。
pub fn decode_multi_sz(data: &[u16]) -> Vec<String> {
    data.split(|unit| *unit == 0)
        .take_while(|part| !part.is_empty())
        .map(String::from_utf16_lossy)
        .collect()
}

/// `Services\BantoHub\Parameters\Profiles\<id>` の `Owners` を読む。サービス・キー・値の
/// どれかが無ければ `Ok(None)`。REG_MULTI_SZ 以外の型・読めないときは `Err`（理由）。
#[cfg(windows)]
pub fn read_owners(profile_id: &str) -> Result<Option<Vec<String>>, String> {
    use windows_sys::Win32::System::Registry::HKEY_LOCAL_MACHINE;
    registry::read_owners_in(
        HKEY_LOCAL_MACHINE,
        SERVICES_KEY,
        crate::service_manager::SERVICE_NAME,
        profile_id,
    )
}

/// `owners` を記録する（上書き）。`Services\BantoHub` のキーが無ければ断る（作らない）。
/// 空の組は書かない（呼ぶ側は 0 人なら締めずに断る。空で上書きしない）。
#[cfg(windows)]
pub fn write_owners(profile_id: &str, owners: &[InstallUserSid]) -> Result<(), String> {
    use windows_sys::Win32::System::Registry::HKEY_LOCAL_MACHINE;
    registry::write_owners_in(
        HKEY_LOCAL_MACHINE,
        SERVICES_KEY,
        crate::service_manager::SERVICE_NAME,
        profile_id,
        owners,
    )
}

#[cfg(windows)]
mod registry {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    use banto_safe_root::InstallUserSid;
    use windows_sys::Win32::Foundation::{
        ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_PATH_NOT_FOUND,
        ERROR_SUCCESS, WIN32_ERROR,
    };
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegGetValueW, RegOpenKeyExW, RegSetValueExW, HKEY,
        KEY_CREATE_SUB_KEY, KEY_SET_VALUE, REG_MULTI_SZ, REG_OPTION_NON_VOLATILE,
        RRF_RT_REG_MULTI_SZ,
    };

    use super::{decode_multi_sz, encode_multi_sz, owners_subkey, OWNERS_VALUE};

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
            // SAFETY: `RegOpenKeyExW`・`RegCreateKeyExW` が返した、まだ閉じていないハンドル。
            unsafe {
                RegCloseKey(self.0);
            }
        }
    }

    /// [`super::read_owners`] の本体（`root` の下の `services_path` を `Services` のキーとして
    /// 扱う。テストは HKCU の一時的なキーを渡す）。
    pub(super) fn read_owners_in(
        root: HKEY,
        services_path: &str,
        service: &str,
        profile_id: &str,
    ) -> Result<Option<Vec<String>>, String> {
        let path = wide(&format!(
            r"{services_path}\{service}\{}",
            owners_subkey(profile_id)?
        ));
        let name = wide(OWNERS_VALUE);
        let mut bytes: u32 = 0;
        // SAFETY: パス・名前は NUL 終端の UTF-16 で、呼び出しが返るまで生きている。データの
        // 置き場を渡さずに大きさだけを問う。
        let status = unsafe {
            RegGetValueW(
                root,
                path.as_ptr(),
                name.as_ptr(),
                RRF_RT_REG_MULTI_SZ,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut bytes,
            )
        };
        match status {
            ERROR_SUCCESS => {}
            code if is_missing(code) => return Ok(None),
            code => {
                return Err(format!(
                    "戻す相手の記録を読めませんでした: {}",
                    os_error(code)
                ))
            }
        }
        // 読む間に大きくなっていたら、もう一度だけ大きさを問い直す。
        for _ in 0..2 {
            let mut buffer = vec![0u16; (bytes as usize).div_ceil(2).max(1)];
            let mut got = u32::try_from(buffer.len() * 2).map_err(|_| "記録が大きすぎます")?;
            // SAFETY: `buffer` は `got` バイトの書ける置き場で、呼び出しが返るまで生きている。
            let status = unsafe {
                RegGetValueW(
                    root,
                    path.as_ptr(),
                    name.as_ptr(),
                    RRF_RT_REG_MULTI_SZ,
                    std::ptr::null_mut(),
                    buffer.as_mut_ptr().cast(),
                    &mut got,
                )
            };
            match status {
                ERROR_SUCCESS => {
                    buffer.truncate((got as usize) / 2);
                    return Ok(Some(decode_multi_sz(&buffer)));
                }
                ERROR_MORE_DATA => bytes = got,
                code if is_missing(code) => return Ok(None),
                code => {
                    return Err(format!(
                        "戻す相手の記録を読めませんでした: {}",
                        os_error(code)
                    ))
                }
            }
        }
        Err("戻す相手の記録を読めませんでした（読む間に変わり続けました）".to_string())
    }

    /// [`super::write_owners`] の本体（[`read_owners_in`] と同じ形）。
    pub(super) fn write_owners_in(
        root: HKEY,
        services_path: &str,
        service: &str,
        profile_id: &str,
        owners: &[InstallUserSid],
    ) -> Result<(), String> {
        if owners.is_empty() {
            return Err("戻す相手が 0 人の記録は書きません".to_string());
        }
        let subkey = wide(&owners_subkey(profile_id)?);
        if service.is_empty() || service.contains('\\') || service.contains('\0') {
            return Err(format!("サービス名が不正です: {service:?}"));
        }
        // サービスのキーは作らない（開くだけ）。
        let service_path = wide(&format!(r"{services_path}\{service}"));
        let mut service_key: HKEY = std::ptr::null_mut();
        // SAFETY: パスは NUL 終端の UTF-16 で、呼び出しが返るまで生きている。`service_key` は
        // 成功したときだけ書かれ、`Key` が閉じる。
        let status = unsafe {
            RegOpenKeyExW(
                root,
                service_path.as_ptr(),
                0,
                KEY_CREATE_SUB_KEY,
                &mut service_key,
            )
        };
        match status {
            ERROR_SUCCESS => {}
            code if is_missing(code) => {
                return Err(format!("サービス '{service}' が登録されていません"))
            }
            ERROR_ACCESS_DENIED => return Err("記録を書く権限がありません".to_string()),
            code => {
                return Err(format!(
                    "サービスのレジストリのキーを開けませんでした: {}",
                    os_error(code)
                ))
            }
        }
        let service_key = Key(service_key);

        let mut key: HKEY = std::ptr::null_mut();
        // SAFETY: `subkey` は NUL 終端の UTF-16。`key` は成功したときだけ書かれ、`Key` が閉じる。
        let status = unsafe {
            RegCreateKeyExW(
                service_key.0,
                subkey.as_ptr(),
                0,
                std::ptr::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE,
                std::ptr::null(),
                &mut key,
                std::ptr::null_mut(),
            )
        };
        match status {
            ERROR_SUCCESS => {}
            ERROR_ACCESS_DENIED => return Err("記録を書く権限がありません".to_string()),
            code => return Err(format!("記録のキーを作れませんでした: {}", os_error(code))),
        }
        let key = Key(key);

        let items: Vec<&str> = owners.iter().map(InstallUserSid::as_str).collect();
        let data = encode_multi_sz(&items);
        let bytes = u32::try_from(data.len() * 2).map_err(|_| "記録が大きすぎます")?;
        let name = wide(OWNERS_VALUE);
        // SAFETY: `name`・`data` は呼び出しが返るまで生きている。`bytes` は `data` の大きさ。
        let status = unsafe {
            RegSetValueExW(
                key.0,
                name.as_ptr(),
                0,
                REG_MULTI_SZ,
                data.as_ptr().cast(),
                bytes,
            )
        };
        match status {
            ERROR_SUCCESS => Ok(()),
            ERROR_ACCESS_DENIED => Err("記録を書く権限がありません".to_string()),
            code => Err(format!(
                "戻す相手の記録を書けませんでした: {}",
                os_error(code)
            )),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use windows_sys::Win32::System::Registry::{RegDeleteTreeW, HKEY_CURRENT_USER};

        const USER_A: &str = "S-1-5-21-1111111111-2222222222-3333333333-1001";
        const USER_B: &str = "S-1-5-21-1111111111-2222222222-3333333333-1002";

        /// HKCU の下の一時的な `Services` 相当のキー（drop で消す）。
        struct TempServices(String);

        impl TempServices {
            fn new(tag: &str) -> Self {
                let path = format!(
                    r"Software\banto-hub-test\profile-owners-{tag}-{}-{}",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_nanos()
                );
                Self(path)
            }

            /// `Services\<service>` を作る（SCM の登録の代わり）。
            fn register(&self, service: &str) {
                let path = wide(&format!(r"{}\{service}", self.0));
                let mut key: HKEY = std::ptr::null_mut();
                // SAFETY: テストの一時的なキー。
                let status = unsafe {
                    RegCreateKeyExW(
                        HKEY_CURRENT_USER,
                        path.as_ptr(),
                        0,
                        std::ptr::null(),
                        REG_OPTION_NON_VOLATILE,
                        KEY_SET_VALUE,
                        std::ptr::null(),
                        &mut key,
                        std::ptr::null_mut(),
                    )
                };
                assert_eq!(status, ERROR_SUCCESS);
                drop(Key(key));
            }
        }

        impl Drop for TempServices {
            fn drop(&mut self) {
                let path = wide(&self.0);
                // SAFETY: テストの一時的なキー。
                unsafe {
                    RegDeleteTreeW(HKEY_CURRENT_USER, path.as_ptr());
                }
            }
        }

        fn sid(s: &str) -> InstallUserSid {
            InstallUserSid::parse(s).unwrap()
        }

        #[test]
        fn writes_and_reads_back_the_owner_set() {
            let services = TempServices::new("roundtrip");
            services.register("BantoHub");
            assert_eq!(
                read_owners_in(HKEY_CURRENT_USER, &services.0, "BantoHub", "default"),
                Ok(None),
                "記録が無ければ None"
            );
            write_owners_in(
                HKEY_CURRENT_USER,
                &services.0,
                "BantoHub",
                "default",
                &[sid(USER_A), sid(USER_B)],
            )
            .unwrap();
            assert_eq!(
                read_owners_in(HKEY_CURRENT_USER, &services.0, "BantoHub", "default"),
                Ok(Some(vec![USER_A.to_string(), USER_B.to_string()]))
            );
            // 上書き。
            write_owners_in(
                HKEY_CURRENT_USER,
                &services.0,
                "BantoHub",
                "default",
                &[sid(USER_B)],
            )
            .unwrap();
            assert_eq!(
                read_owners_in(HKEY_CURRENT_USER, &services.0, "BantoHub", "default"),
                Ok(Some(vec![USER_B.to_string()]))
            );
        }

        #[test]
        fn does_not_create_the_service_key_or_write_an_empty_set() {
            let services = TempServices::new("unregistered");
            let err = write_owners_in(
                HKEY_CURRENT_USER,
                &services.0,
                "BantoHub",
                "default",
                &[sid(USER_A)],
            )
            .unwrap_err();
            assert!(err.contains("登録されていません"), "{err}");
            services.register("BantoHub");
            let err = write_owners_in(HKEY_CURRENT_USER, &services.0, "BantoHub", "default", &[])
                .unwrap_err();
            assert!(err.contains("0 人"), "{err}");
            assert_eq!(
                read_owners_in(HKEY_CURRENT_USER, &services.0, "BantoHub", "default"),
                Ok(None)
            );
        }

        #[test]
        fn refuses_a_profile_id_that_could_leave_the_key() {
            let services = TempServices::new("bad-id");
            services.register("BantoHub");
            assert!(read_owners_in(HKEY_CURRENT_USER, &services.0, "BantoHub", r"..\x").is_err());
            assert!(write_owners_in(
                HKEY_CURRENT_USER,
                &services.0,
                "BantoHub",
                r"a\b",
                &[sid(USER_A)]
            )
            .is_err());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multi_sz_round_trips_and_stops_at_the_first_empty_string() {
        let items = ["S-1-5-21-1-2-3-1001", "S-1-5-21-1-2-3-1002"];
        let data = encode_multi_sz(&items);
        assert_eq!(data.iter().filter(|u| **u == 0).count(), 3);
        assert_eq!(decode_multi_sz(&data), items);
        let empty: [&str; 0] = [];
        assert_eq!(encode_multi_sz(&empty), vec![0, 0]);
        assert!(decode_multi_sz(&encode_multi_sz(&empty)).is_empty());
        // 末尾の NUL が欠けていても読めた分。
        let truncated: Vec<u16> = "S-1-5-21-1-2-3-1001".encode_utf16().collect();
        assert_eq!(decode_multi_sz(&truncated), ["S-1-5-21-1-2-3-1001"]);
        // 空の文字列の後ろは読まない。
        let mut data = encode_multi_sz(&["a"]);
        data.extend("b".encode_utf16());
        data.push(0);
        assert_eq!(decode_multi_sz(&data), ["a"]);
    }

    #[test]
    fn subkey_and_location_use_only_a_valid_profile_id() {
        assert_eq!(
            owners_subkey("default").unwrap(),
            r"Parameters\Profiles\default"
        );
        assert!(owners_subkey(r"..\x").is_err());
        assert!(owners_subkey("").is_err());
        assert!(
            owners_location("default").ends_with(r"BantoHub\Parameters\Profiles\default\Owners")
        );
    }
}
