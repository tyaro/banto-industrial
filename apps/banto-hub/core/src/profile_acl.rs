//! profile ACL 追加スライス（docs/banto-hub-desktop-plan.md §11、
//! docs/banto-hub-t16-design.md §3「LocalSystem 作成 profile の ACL」既知
//! gap の解消）: LocalSystem（Windows サービス）が先に作成した
//! `%ProgramData%\BantoHub\profiles\<profile-id>\`配下のファイルへ、
//! profile owner（対話 Windows ユーザー）が書き込めるよう明示的な DACL を
//! 付与する。
//!
//! ## 背景（実機で確認した不具合）
//!
//! LocalSystem で動く `BantoHub` サービスが profile ディレクトリ・DB を
//! 先に作成すると、既定の継承 ACL は SYSTEM/Administrators のみが変更でき
//! `Users`（対話ユーザー）には書き込みを与えない - このため後から
//! Desktop/shell（対話ユーザー権限）で同じ profile を開こうとすると
//! SQLite が `attempt to write a readonly database` で失敗する
//! （docs/banto-hub-t16-design.md §3 実機メモ、本モジュールが解消する
//! 対象そのもの）。
//!
//! ## 権限方針（desktop-plan §11 のとおり - `Users` 全体には絶対に付与しない）
//!
//! - **SYSTEM** / **Administrators**: Full Control（`FILE_ALL_ACCESS`）。
//!   既に親ディレクトリからの継承で持っているのが通常想定だが、この
//!   モジュールは明示 ACE としても付与し直す - 継承チェーンだけに依存
//!   しきらないための保険（[`windows_impl`]モジュール doc「ACL 設計」節
//!   参照）。
//! - **profile owner**（`owner_account_name`、通常は対話 Windows
//!   ユーザー）: 「変更（Modify）」相当
//!   （`FILE_GENERIC_READ | FILE_GENERIC_WRITE | FILE_GENERIC_EXECUTE |
//!   DELETE`）。フルコントロールは付与しない - owner は自分の profile を
//!   直接読み書きできれば十分で、ACL 自体の変更（`WRITE_DAC`）や所有権
//!   変更（`WRITE_OWNER`）までは与えない。
//! - **`BantoHub Operators`**: このモジュールは一切 ACE を追加しない
//!   （[`crate::service_elevated`]のサービス DACL とは別レイヤ -
//!   Operators は SCM の日常操作権限のみで、profile ファイルへの書き込み
//!   権限はグループメンバーシップからは得られない、実装指示のとおり）。
//! - **`Users`グループ**: 一切 ACE を追加しない
//!   （desktop-plan §11「Users 全体へ権限を与えない」の直接反映）。
//!
//! ## 継承（新規ファイルの自動修復）
//!
//! `profile_dir`自体へ設定する ACE には`OBJECT_INHERIT_ACE |
//! CONTAINER_INHERIT_ACE`を付与する - これにより、後から LocalSystem
//! サービスが profile 配下に新規作成するファイル・サブディレクトリも
//! 自動的に同じ owner 書き込み権限を継承する（「サービスを再起動する
//! たびに ACL が壊れる」を防ぐのが目的）。
//!
//! ## 既存ファイルの修復（実機バグそのものへの対処）
//!
//! 継承は「今後作成されるファイル」にしか効かない。既にサービスが
//! 作成済みの `config/*.sqlite3` 等（実機バグの実体）を直すため、
//! [`grant_profile_owner_acl`]は`profile_dir`だけでなく、その配下に既に
//! 存在する全ファイル・全ディレクトリへ再帰的に同じ ACE を適用する
//! （[`windows_impl::apply_acl_recursive`]）。
//!
//! ## マージ方針（既存 SYSTEM/Administrators ACE を壊さない）
//!
//! [`crate::service_elevated::windows_impl::grant_service_acl_with_service`]
//! と同じパターン - `GetNamedSecurityInfoW`で既存 DACL を取得し、
//! `SetEntriesInAclW`へ`oldacl`として渡すことで、指定した3トラスティ
//! （SYSTEM/Administrators/owner）以外の既存 ACE をそのまま残す。
//! `grfAccessMode = SET_ACCESS`は「同じトラスティの既存エントリを置き
//! 換える」動作なので、再実行しても owner の権限が増え続けない（冪等）。
//!
//! ## SDDL 相当
//!
//! ```text
//! (A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;0x1301BF;;;<owner-SID>)
//! ```
//!
//! - `SY` = SYSTEM、`BA` = Administrators（BUILTIN\Administrators）
//! - `FA` = `FILE_ALL_ACCESS`
//! - `0x1301BF` = `FILE_GENERIC_READ | FILE_GENERIC_WRITE |
//!   FILE_GENERIC_EXECUTE | DELETE`（NTFS の「変更」既定値と数値が一致する
//!   - [`windows_impl::OWNER_MODIFY_ACCESS_MASK`]参照）
//! - `OICI` = `OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE`
//!
//! ## 呼び出し元
//!
//! [`crate::service_elevated::ElevatedAction::GrantProfileAcl`]
//! （`grant-profile-acl`、UAC helper 経由 - ACL 変更は UAC 昇格を要求する
//! desktop-plan §11 の方針どおり）が唯一の呼び出し元。
//!
//! ## 非 Windows ビルド
//!
//! banto-hub は Windows 専用製品だが、このワークスペース自体は非 Windows
//! でも`cargo check --workspace`が通る必要がある（他の`service_*`モジュール
//! と同じ事情）。[`grant_profile_owner_acl`]の非 Windows 版は常に
//! [`ProfileAclError::UnsupportedPlatform`]を返す - Windows ACL という
//! 概念自体が存在しないため、黙って`Ok(())`にはしない（誤って「成功した」
//! と誤解させないため）。

use std::path::Path;

use thiserror::Error;

/// [`grant_profile_owner_acl`]の失敗モード。
#[derive(Debug, Error)]
pub enum ProfileAclError {
    /// `profile_dir`の作成・列挙等の I/O エラー。
    #[error("banto-hub: profile ACL 適用の I/O に失敗しました: {0}")]
    Io(#[from] std::io::Error),
    /// `owner_account_name`が Windows アカウントとして解決できなかった。
    #[error("banto-hub: profile owner アカウント '{0}' が見つかりません")]
    OwnerAccountNotFound(String),
    /// `SYSTEM`/`Administrators`（本来常に解決できるはずの well-known
    /// アカウント）の SID 解決に失敗した - 通常は起こらない想定外エラー。
    #[error("banto-hub: 組み込みアカウント '{0}' の SID 解決に失敗しました")]
    WellKnownAccountNotFound(String),
    /// [`crate::service_operators::OperatorsError`]（`lookup_account_sid`の
    /// 再利用元）をそのまま透過する。
    #[error(transparent)]
    Operators(#[from] crate::service_operators::OperatorsError),
    /// `GetNamedSecurityInfoW`（既存 DACL の取得）の失敗。
    #[error(
        "banto-hub: '{path}' の既存 ACL 取得に失敗しました (GetNamedSecurityInfoW, os error {os_error})"
    )]
    GetNamedSecurityInfoFailed { path: String, os_error: u32 },
    /// `SetEntriesInAclW`（ACE のマージ）の失敗。
    #[error("banto-hub: ACL への ACE 追加に失敗しました (SetEntriesInAclW, status {0})")]
    SetEntriesInAclFailed(u32),
    /// `SetNamedSecurityInfoW`（新 DACL の適用）の失敗。
    #[error(
        "banto-hub: '{path}' への ACL 適用に失敗しました (SetNamedSecurityInfoW, os error {os_error})"
    )]
    SetNamedSecurityInfoFailed { path: String, os_error: u32 },
    /// 非 Windows ビルドでの呼び出し（モジュール doc「非 Windows ビルド」
    /// 節参照）。
    #[error("banto-hub: profile ACL 設定は Windows 専用です")]
    UnsupportedPlatform,
}

/// `profile_dir`（`{root}/profiles/<profile-id>/`）へ、`owner_account_name`
/// が変更できる ACE を再帰的に付与する（非 Windows 版、モジュール doc
/// 「非 Windows ビルド」節参照）。
#[cfg(not(windows))]
pub fn grant_profile_owner_acl(
    _profile_dir: &Path,
    _owner_account_name: &str,
) -> Result<(), ProfileAclError> {
    Err(ProfileAclError::UnsupportedPlatform)
}

/// `profile_dir`（`{root}/profiles/<profile-id>/`）へ、`owner_account_name`
/// が変更できる ACE を再帰的に付与する（Windows 版）。
///
/// 1. `profile_dir`が存在しなければ作成する（`create_dir_all`）。
/// 2. `owner_account_name`・`SYSTEM`・`Administrators`の SID を解決する。
/// 3. `profile_dir`自身と、その配下に既に存在する全ファイル・
///    全ディレクトリへ、モジュール doc「権限方針」節の3 ACE を
///    [`windows_impl::apply_acl_to_path`]で適用する
///    （[`windows_impl::apply_acl_recursive`]）。
#[cfg(windows)]
pub fn grant_profile_owner_acl(
    profile_dir: &Path,
    owner_account_name: &str,
) -> Result<(), ProfileAclError> {
    std::fs::create_dir_all(profile_dir)?;
    windows_impl::grant_profile_owner_acl(profile_dir, owner_account_name)
}

#[cfg(windows)]
pub(crate) mod windows_impl {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::{
        GetNamedSecurityInfoW, SetEntriesInAclW, SetNamedSecurityInfoW, EXPLICIT_ACCESS_W,
        NO_MULTIPLE_TRUSTEE, SET_ACCESS, SE_FILE_OBJECT, TRUSTEE_IS_ALIAS, TRUSTEE_IS_SID,
        TRUSTEE_IS_USER, TRUSTEE_IS_WELL_KNOWN_GROUP, TRUSTEE_W,
    };
    use windows_sys::Win32::Security::{
        ACL, CONTAINER_INHERIT_ACE, DACL_SECURITY_INFORMATION, OBJECT_INHERIT_ACE,
        PSECURITY_DESCRIPTOR,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        DELETE, FILE_ALL_ACCESS, FILE_GENERIC_EXECUTE, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
    };

    use super::ProfileAclError;
    use crate::service_operators::windows_impl::lookup_account_sid;

    /// owner に付与するアクセス権 - NTFS の「変更(Modify)」既定値そのもの
    /// （モジュール doc「SDDL 相当」節・`0x1301BF`参照）。フルコントロール
    /// （`WRITE_DAC`/`WRITE_OWNER`込み）は意図的に含めない。
    pub const OWNER_MODIFY_ACCESS_MASK: u32 =
        FILE_GENERIC_READ | FILE_GENERIC_WRITE | FILE_GENERIC_EXECUTE | DELETE;

    /// 新規作成ファイル・サブディレクトリへ同じ ACE を伝播させる継承フラグ
    /// （モジュール doc「継承」節参照）。
    const INHERIT_FILES_AND_DIRS: u32 = OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE;

    pub(super) fn grant_profile_owner_acl(
        profile_dir: &Path,
        owner_account_name: &str,
    ) -> Result<(), ProfileAclError> {
        let owner_sid = lookup_account_sid(owner_account_name)?
            .ok_or_else(|| ProfileAclError::OwnerAccountNotFound(owner_account_name.to_string()))?;
        // `SYSTEM`/`Administrators`はどのローカルマシンにも存在する
        // well-known アカウント - `lookup_account_sid`は名前ベースの
        // `LookupAccountNameW`をそのまま呼ぶので、この2つの英語名でも
        // ローカライズ版 Windows で問題なく解決できる
        // （`service_operators.rs`の`OPERATORS_GROUP_NAME`解決と同じ前提）。
        let system_sid = lookup_account_sid("SYSTEM")?
            .ok_or_else(|| ProfileAclError::WellKnownAccountNotFound("SYSTEM".to_string()))?;
        let admin_sid = lookup_account_sid("Administrators")?.ok_or_else(|| {
            ProfileAclError::WellKnownAccountNotFound("Administrators".to_string())
        })?;

        apply_acl_recursive(profile_dir, &owner_sid, &system_sid, &admin_sid)
    }

    /// `root`自身と、その配下に既に存在する全ファイル・全ディレクトリへ
    /// 再帰的に同じ ACE を適用する（モジュール doc「既存ファイルの修復」
    /// 節参照 - 実機バグ（LocalSystem 作成済みファイルが readonly）を
    /// 直すのに必須）。
    pub(super) fn apply_acl_recursive(
        root: &Path,
        owner_sid: &[u8],
        system_sid: &[u8],
        admin_sid: &[u8],
    ) -> Result<(), ProfileAclError> {
        apply_acl_to_path(root, owner_sid, system_sid, admin_sid)?;

        let Ok(entries) = std::fs::read_dir(root) else {
            // ルート自体は直前の`apply_acl_to_path`が成功しているので
            // 存在するはずだが、列挙自体の失敗（一時的な競合等）は致命的
            // にしない - 既存ファイルの一部が直らないだけで、profile_dir
            // 自体と今後の新規ファイル（継承）は既に直っている。
            return Ok(());
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
            if is_dir {
                apply_acl_recursive(&path, owner_sid, system_sid, admin_sid)?;
            } else {
                apply_acl_to_path(&path, owner_sid, system_sid, admin_sid)?;
            }
        }
        Ok(())
    }

    /// `path`（ファイルまたはディレクトリ）1件へ、モジュール doc
    /// 「権限方針」節の3 ACE（SYSTEM Full / Administrators Full / owner
    /// Modify、いずれも`SET_ACCESS`）を「マージ方針」節のとおり既存 DACL を
    /// 壊さず適用する。ディレクトリ・ファイルのどちらに対しても同じ
    /// 継承フラグを付ける - ファイル自体には継承の効果は無いが、Win32 API
    /// 上は無害（`SetEntriesInAclW`はオブジェクト種別を見ずに ACE を
    /// 組み立てるだけ）。
    pub(super) fn apply_acl_to_path(
        path: &Path,
        owner_sid: &[u8],
        system_sid: &[u8],
        admin_sid: &[u8],
    ) -> Result<(), ProfileAclError> {
        // 再帰呼び出しの都度クローンする - SID は高々数十バイトで、
        // `TRUSTEE_W::ptstrName`が要求する`*mut u16`（実体は SID バイト列
        // へのポインタ、Win32 API の慣例どおり）を得るには可変バッファが
        // 必要なため（`service_elevated.rs`の`grant_service_acl_with_service`
        // と同じ理由）。
        let mut owner_sid = owner_sid.to_vec();
        let mut system_sid = system_sid.to_vec();
        let mut admin_sid = admin_sid.to_vec();

        let wide_path: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        let mut old_dacl: *mut ACL = std::ptr::null_mut();
        let mut sd_ptr: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: `wide_path`はこの呼び出しが終わるまでスコープ内で生存する
        // NUL 終端 UTF-16 文字列。DACL 以外（owner/group/SACL）は取得しない
        // - `service_elevated.rs::grant_service_acl_with_service`の
        // `QueryServiceObjectSecurity`呼び出しと同じ「DACL だけ触る」方針。
        let query_status = unsafe {
            GetNamedSecurityInfoW(
                wide_path.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut old_dacl,
                std::ptr::null_mut(),
                &mut sd_ptr,
            )
        };
        if query_status != 0 {
            return Err(ProfileAclError::GetNamedSecurityInfoFailed {
                path: path.display().to_string(),
                os_error: query_status,
            });
        }

        let free_queried_sd = || {
            if !sd_ptr.is_null() {
                // SAFETY: `sd_ptr`は直前の`GetNamedSecurityInfoW`が
                // `LocalAlloc`で確保したセキュリティ記述子（MSDN の契約
                // どおり`LocalFree`で解放する）。
                unsafe {
                    LocalFree(sd_ptr as _);
                }
            }
        };

        let system_trustee = TRUSTEE_W {
            pMultipleTrustee: std::ptr::null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_WELL_KNOWN_GROUP,
            ptstrName: system_sid.as_mut_ptr() as *mut u16,
        };
        let admin_trustee = TRUSTEE_W {
            pMultipleTrustee: std::ptr::null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            // BUILTIN\Administrators はローカルエイリアス
            // （`service_elevated.rs`が`BantoHub Operators`に使う
            // `TRUSTEE_IS_GROUP`と区別 - こちらは`TRUSTEE_IS_ALIAS`が
            // より正確だが、`CheckTokenMembership`等の実処理では
            // `TrusteeType`は情報用途のみで挙動に影響しない）。
            TrusteeType: TRUSTEE_IS_ALIAS,
            ptstrName: admin_sid.as_mut_ptr() as *mut u16,
        };
        let owner_trustee = TRUSTEE_W {
            pMultipleTrustee: std::ptr::null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_USER,
            ptstrName: owner_sid.as_mut_ptr() as *mut u16,
        };

        let entries = [
            EXPLICIT_ACCESS_W {
                grfAccessPermissions: FILE_ALL_ACCESS,
                grfAccessMode: SET_ACCESS,
                grfInheritance: INHERIT_FILES_AND_DIRS,
                Trustee: system_trustee,
            },
            EXPLICIT_ACCESS_W {
                grfAccessPermissions: FILE_ALL_ACCESS,
                grfAccessMode: SET_ACCESS,
                grfInheritance: INHERIT_FILES_AND_DIRS,
                Trustee: admin_trustee,
            },
            EXPLICIT_ACCESS_W {
                grfAccessPermissions: OWNER_MODIFY_ACCESS_MASK,
                grfAccessMode: SET_ACCESS,
                grfInheritance: INHERIT_FILES_AND_DIRS,
                Trustee: owner_trustee,
            },
        ];

        let mut new_acl: *mut ACL = std::ptr::null_mut();
        // SAFETY: `old_dacl`は直前に取得した有効な（または未設定なら null
        // の）ACL、`entries`はこのスコープで生存している。
        let merge_status = unsafe {
            SetEntriesInAclW(
                entries.len() as u32,
                entries.as_ptr(),
                old_dacl,
                &mut new_acl,
            )
        };
        if merge_status != 0 {
            free_queried_sd();
            return Err(ProfileAclError::SetEntriesInAclFailed(merge_status));
        }

        // SAFETY: `wide_path`は生存中、`new_acl`は直前に構築した有効な
        // ACL。owner/group/SACL は変更しない（DACL のみ）。
        let apply_status = unsafe {
            SetNamedSecurityInfoW(
                wide_path.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                new_acl,
                std::ptr::null(),
            )
        };

        free_queried_sd();
        // SAFETY: `new_acl`は`SetEntriesInAclW`が`LocalAlloc`で確保した
        // メモリ（MSDN の契約どおり`LocalFree`で解放する）。
        unsafe {
            LocalFree(new_acl as _);
        }

        if apply_status != 0 {
            return Err(ProfileAclError::SetNamedSecurityInfoFailed {
                path: path.display().to_string(),
                os_error: apply_status,
            });
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(windows))]
    #[test]
    fn non_windows_returns_unsupported_platform() {
        let result = grant_profile_owner_acl(Path::new("/tmp/does-not-matter"), "someone");
        assert!(matches!(result, Err(ProfileAclError::UnsupportedPlatform)));
    }

    /// `OWNER_MODIFY_ACCESS_MASK`がモジュール doc「SDDL 相当」節の
    /// `0x1301BF`（NTFS「変更」既定値）と一致することを固定する - 実際の
    /// 数値は`windows_impl`（`#[cfg(windows)]`）内にしか無いため、非
    /// Windows でも検証できるようここでは Win32 の公開定数値を直接計算
    /// して比較する（`FILE_GENERIC_READ`=0x0012_0089・`FILE_GENERIC_WRITE`
    /// =0x0012_0116・`FILE_GENERIC_EXECUTE`=0x0012_00A0・`DELETE`=0x0001_0000、
    /// いずれも MSDN で安定して文書化された Win32 の値）。
    #[test]
    fn owner_modify_access_mask_matches_ntfs_modify_default() {
        const FILE_GENERIC_READ: u32 = 0x0012_0089;
        const FILE_GENERIC_WRITE: u32 = 0x0012_0116;
        const FILE_GENERIC_EXECUTE: u32 = 0x0012_00A0;
        const DELETE: u32 = 0x0001_0000;
        let expected = FILE_GENERIC_READ | FILE_GENERIC_WRITE | FILE_GENERIC_EXECUTE | DELETE;
        assert_eq!(expected, 0x1301BF, "この計算式自体が 0x1301BF と食い違う");
        #[cfg(windows)]
        assert_eq!(windows_impl::OWNER_MODIFY_ACCESS_MASK, expected);
    }

    #[cfg(windows)]
    #[test]
    fn owner_modify_access_mask_excludes_dac_and_owner_rights() {
        // WRITE_DAC(0x00040000)/WRITE_OWNER(0x00080000) を含まないことを
        // 明示的に固定する（モジュール doc「権限方針」節の「フルコントロール
        // は付与しない」という要求そのもの）。
        const WRITE_DAC: u32 = 0x0004_0000;
        const WRITE_OWNER: u32 = 0x0008_0000;
        assert_eq!(windows_impl::OWNER_MODIFY_ACCESS_MASK & WRITE_DAC, 0);
        assert_eq!(windows_impl::OWNER_MODIFY_ACCESS_MASK & WRITE_OWNER, 0);
    }

    /// 実際に Windows ファイルシステム上へ ACL を適用する - 管理者権限
    /// （`SetNamedSecurityInfoW`で DACL を書き換えるには対象への
    /// `WRITE_DAC`が必要 - 通常ユーザーが自分の作成した一時ディレクトリに
    /// 対して持つ）で動作確認する。CI では現在の実行ユーザー名が Windows
    /// アカウントとして解決できることを前提にする。
    #[cfg(windows)]
    #[test]
    fn grant_profile_owner_acl_applies_to_new_and_existing_files() {
        let root = crate::test_support::TempDir::new("profile-acl-apply");
        let profile_dir = root.path().join("profiles").join("default");
        std::fs::create_dir_all(profile_dir.join("config")).expect("create config dir");
        std::fs::write(
            profile_dir.join("config").join("banto-hub.sqlite3"),
            b"pretend-sqlite-bytes",
        )
        .expect("create pretend db file");

        let current_user = service_elevated_current_user_name().expect("resolve current user name");

        grant_profile_owner_acl(&profile_dir, &current_user)
            .expect("granting ACL to a self-owned temp dir should succeed");
    }

    /// 上記テスト専用 - `service_elevated::windows_impl::current_user_name`
    /// は`pub(super)`のため直接使えない。同じ`GetUserNameW`呼び出しを
    /// テストコード側で複製する（本体側の可視性をテストのためだけに
    /// 緩めない）。
    #[cfg(windows)]
    fn service_elevated_current_user_name() -> Result<String, String> {
        use windows_sys::Win32::Foundation::{GetLastError, ERROR_INSUFFICIENT_BUFFER};
        use windows_sys::Win32::System::WindowsProgramming::GetUserNameW;

        let mut buf = vec![0u16; 256];
        loop {
            let mut len = buf.len() as u32;
            let ok = unsafe { GetUserNameW(buf.as_mut_ptr(), &mut len) };
            if ok != 0 {
                let end = (len.saturating_sub(1)) as usize;
                return Ok(String::from_utf16_lossy(&buf[..end.min(buf.len())]));
            }
            let os_error = unsafe { GetLastError() };
            if os_error == ERROR_INSUFFICIENT_BUFFER && (len as usize) > buf.len() {
                buf.resize(len as usize, 0);
                continue;
            }
            return Err(format!("GetUserNameW failed: {os_error}"));
        }
    }
}
