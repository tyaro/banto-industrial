//! 保存先ディレクトリの DACL（決定 4、計画書 §3.1(c)・§3.3）。
//!
//! # 期待する形
//!
//! 継承しない（保護された）DACL に、明示の許可 ACE だけがちょうど次のとおり並ぶ:
//!
//! | SID                                    | アクセス                        | 継承フラグ |
//! | -------------------------------------- | ------------------------------- | ---------- |
//! | SYSTEM（`S-1-5-18`）                   | フル（`FILE_ALL_ACCESS`）       | OI CI      |
//! | Administrators（`S-1-5-32-544`）       | フル                            | OI CI      |
//! | インストール利用者（指定したときだけ） | 変更（`0x1301BF`、ACL の変更・所有権の取得は含まない） | OI CI |
//!
//! SDDL では `D:P(A;OICI;FA;;;S-1-5-18)(A;OICI;FA;;;S-1-5-32-544)(A;OICI;0x1301bf;;;<SID>)`。
//! Users・Everyone・Authenticated Users・Operators のグループには何も付けない。
//! 中のファイルは OI で同じ ACE を継承する（一時ファイルも作った時点で継承し、
//! 同じディレクトリの中の置換なのでそのまま本体になる）。
//!
//! 所有者は SYSTEM・Administrators・インストール利用者のどれかであることも
//! 確かめる（所有者は DACL に依らず ACL を書き換えられるため。例えば
//! `%ProgramData%` の下で一般の利用者が先に同じ名前のフォルダを作り、同じ形の
//! DACL を付けておく、という乗っ取りを拒む）。
//!
//! # 作るとき・既にあるとき（既存のディレクトリの ACL が違う場合の扱い）
//!
//! - **無ければ**: `CreateDirectoryW` に上の記述子を渡し、作るのと同時に DACL を
//!   付ける（作ってから付けるまでの間に、親から継承した広い権限で見える時間を
//!   作らない）。インストール利用者の SID は、作る前に `LookupAccountSidW` で
//!   **利用者のアカウント（`SidTypeUser`）であること**を確かめる（Domain Users の
//!   ような S-1-5-21 のグループを渡されても広げない）。解決できない SID も拒む
//!   （fail closed。Microsoft Entra ID の利用者 `S-1-12-1-…` を含む - 下の
//!   [`InstallUserSid`] の doc）。
//! - **既にあれば**: DACL・所有者を読み戻して上の形と**完全に一致する**ことを
//!   確かめ、違えば [`SecretStoreError::AclMismatch`] で失敗する（fail closed）。
//!   **自動では直さない**（黙って広げることも狭めることもしない。違う形には、
//!   管理者が意図して変えた・別の製品が使っている・乗っ取りの準備、のどれも
//!   ありうるため）。直すのは [`reset_dir_acl`] の明示的な呼び出しだけで、
//!   B3-3 の昇格ヘルパ（管理者の操作）から使う想定。
//! - [`crate::FileSecretStore`] は開くときだけでなく、**読む・書く・消すたびに**
//!   この確認をする（途中で ACL が変えられたら、それ以降は使わない）。
//!
//! ファイル 1 つ 1 つの DACL は確かめない（ディレクトリから継承したものの
//! はず。書き込みはファイルを丸ごと置き換えるので、書くたびに継承した形に戻る）。
//!
//! # 非 Windows
//!
//! DACL の概念が無いので、作る・確かめる・直すはどれも
//! [`SecretStoreError::UnsupportedPlatform`] を返す（黙って成功にしない）。
//! [`InstallUserSid`] の文字列の検査と [`check_dacl`] の判定は純関数で、全 OS で
//! テストする。

use crate::error::{AclMismatch, Result, SecretStoreError};

/// SYSTEM。
pub const SYSTEM_SID: &str = "S-1-5-18";
/// BUILTIN\Administrators。
pub const ADMINISTRATORS_SID: &str = "S-1-5-32-544";
/// `FILE_ALL_ACCESS`。
pub const FULL_ACCESS_MASK: u32 = 0x001F_01FF;
/// NTFS の「変更」= `FILE_GENERIC_READ | FILE_GENERIC_WRITE |
/// FILE_GENERIC_EXECUTE | DELETE`（`WRITE_DAC`・`WRITE_OWNER` を含まない。
/// banto-hub の `profile_acl::OWNER_MODIFY_ACCESS_MASK` と同じ値）。
pub const MODIFY_ACCESS_MASK: u32 = 0x0013_01BF;
/// `OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE`。
pub const INHERIT_FLAGS: u8 = 0x03;
/// `INHERITED_ACE`。
const INHERITED_ACE_FLAG: u8 = 0x10;
/// `ACCESS_ALLOWED_ACE_TYPE`。
pub const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;

/// インストール利用者の SID（文字列形式、例 `S-1-5-21-…-1001`）。
///
/// 受け付けるのは、利用者のアカウントが入る 2 つの領域の SID だけ:
///
/// - `S-1-5-21-…`: ローカル・Active Directory のアカウント。
/// - `S-1-12-1-…`: Microsoft Entra ID（Azure AD）に参加した PC の Entra の利用者。
///
/// Everyone（`S-1-1-0`）・Authenticated Users（`S-1-5-11`）・
/// BUILTIN の各グループ（`S-1-5-32-…`）・SYSTEM などはこの時点で拒む。文字は
/// 数字と `-` だけに限る（SDDL に組み込むので、`)` や `;` で ACE を足されない）。
/// 同じ領域のグループ（Domain Users など）は文字列では見分けられないので、
/// Windows でディレクトリを作る・直すときに `LookupAccountSidW` で利用者の
/// アカウントであることを確かめる（`SidTypeUser` 以外は拒む）。
///
/// Entra の利用者の SID は、その利用者でサインインしたことのある PC では
/// `LookupAccountSidW` が解決し（`AzureAD\<名前>`、種類は `SidTypeUser`）、
/// そのまま使える。**解決できないとき（その PC で一度もサインインしていない
/// 利用者・名前の解決に Entra へ届かない等）は、グループと見分けられないので
/// 拒む（fail closed、[`SecretStoreError::InvalidUserSid`]）**。インストールは
/// その利用者自身のサインイン中に行う（B3-3 の昇格ヘルパは呼び出した利用者の
/// SID を渡す）ので、通常は解決できる。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstallUserSid(String);

impl InstallUserSid {
    pub fn parse(s: &str) -> Result<Self> {
        const PREFIXES: [&str; 2] = ["S-1-5-21-", "S-1-12-1-"];
        let Some(rest) = PREFIXES.iter().find_map(|prefix| s.strip_prefix(prefix)) else {
            return Err(SecretStoreError::InvalidUserSid(
                "利用者のアカウントの SID（S-1-5-21-… か S-1-12-1-…）ではありません",
            ));
        };
        let parts: Vec<&str> = rest.split('-').collect();
        let well_formed = s.len() <= 184
            && parts.len() >= 2
            && parts
                .iter()
                .all(|p| !p.is_empty() && p.len() <= 10 && p.bytes().all(|b| b.is_ascii_digit()));
        if !well_formed {
            return Err(SecretStoreError::InvalidUserSid("SID の形式ではありません"));
        }
        Ok(Self(s.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// 保存先ディレクトリに付ける権限の指定。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirAccess {
    install_user: Option<InstallUserSid>,
}

impl DirAccess {
    /// SYSTEM と Administrators だけ（サービスだけが使う構成）。
    pub fn service_only() -> Self {
        Self { install_user: None }
    }

    /// SYSTEM と Administrators に加えて、インストール利用者に「変更」（決定 4 -
    /// 昇格していないデスクトップの host が同じキーを読み書きするため）。
    pub fn with_install_user(sid: InstallUserSid) -> Self {
        Self {
            install_user: Some(sid),
        }
    }

    pub fn install_user(&self) -> Option<&InstallUserSid> {
        self.install_user.as_ref()
    }

    /// 期待する ACE（SID・アクセス）。
    fn expected_aces(&self) -> Vec<(String, u32)> {
        let mut aces = vec![
            (SYSTEM_SID.to_owned(), FULL_ACCESS_MASK),
            (ADMINISTRATORS_SID.to_owned(), FULL_ACCESS_MASK),
        ];
        if let Some(user) = &self.install_user {
            aces.push((user.0.clone(), MODIFY_ACCESS_MASK));
        }
        aces
    }

    /// 作るとき・直すときに使う SDDL（`D:P` = 保護された DACL）。
    pub fn sddl(&self) -> String {
        let mut sddl = format!("D:P(A;OICI;FA;;;{SYSTEM_SID})(A;OICI;FA;;;{ADMINISTRATORS_SID})");
        if let Some(user) = &self.install_user {
            sddl.push_str(&format!("(A;OICI;{MODIFY_ACCESS_MASK:#x};;;{})", user.0));
        }
        sddl
    }
}

/// 読み戻した ACE 1 つ。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObservedAce {
    pub ace_type: u8,
    pub flags: u8,
    pub mask: u32,
    /// 許可・拒否以外の種類の ACE では空。
    pub sid: String,
}

/// 読み戻したディレクトリのセキュリティ記述子（必要な部分だけ）。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObservedDacl {
    /// `SE_DACL_PROTECTED`。
    pub protected: bool,
    /// 所有者の SID。
    pub owner: String,
    /// `None` は null DACL（全員にフルアクセス）。
    pub aces: Option<Vec<ObservedAce>>,
}

/// 読み戻した記述子が `access` の期待どおりか（モジュール doc「期待する形」）。純関数。
pub fn check_dacl(
    observed: &ObservedDacl,
    access: &DirAccess,
) -> std::result::Result<(), AclMismatch> {
    let Some(aces) = &observed.aces else {
        return Err(AclMismatch::NoDacl);
    };
    if !observed.protected {
        return Err(AclMismatch::NotProtected);
    }
    let mut expected = access.expected_aces();
    for ace in aces {
        if ace.flags & INHERITED_ACE_FLAG != 0 {
            return Err(AclMismatch::InheritedAce {
                sid: ace.sid.clone(),
            });
        }
        let position = (ace.ace_type == ACCESS_ALLOWED_ACE_TYPE && ace.flags == INHERIT_FLAGS)
            .then(|| {
                expected
                    .iter()
                    .position(|(sid, mask)| *sid == ace.sid && *mask == ace.mask)
            })
            .flatten();
        match position {
            Some(i) => {
                expected.swap_remove(i);
            }
            None => {
                return Err(AclMismatch::UnexpectedAce {
                    ace_type: ace.ace_type,
                    flags: ace.flags,
                    mask: ace.mask,
                    sid: ace.sid.clone(),
                })
            }
        }
    }
    if let Some((sid, mask)) = expected.into_iter().next() {
        return Err(AclMismatch::MissingAce { sid, mask });
    }
    let owner_allowed = observed.owner == SYSTEM_SID
        || observed.owner == ADMINISTRATORS_SID
        || access
            .install_user
            .as_ref()
            .is_some_and(|user| user.0 == observed.owner);
    if !owner_allowed {
        return Err(AclMismatch::DisallowedOwner {
            sid: observed.owner.clone(),
        });
    }
    Ok(())
}

/// 無ければ期待の DACL で作り、あれば確かめる（モジュール doc「作るとき・
/// 既にあるとき」）。親ディレクトリは無ければ普通に作る（親の ACL は B3-3 の
/// 担当で、ここでは触らない）。
pub fn ensure_dir(dir: &std::path::Path, access: &DirAccess) -> Result<()> {
    imp::ensure_dir(dir, access)
}

/// 既にあるディレクトリを確かめる（作らない）。
pub fn verify_dir(dir: &std::path::Path, access: &DirAccess) -> Result<()> {
    imp::verify_dir(dir, access)
}

/// 既にあるディレクトリの DACL を、期待の形に**書き直す**（明示的な修復。
/// 管理者の操作から呼ぶ）。中のファイル・サブディレクトリへは OS が継承を
/// 伝える。**所有者は変えない**ので、所有者が許可外なら直した後も
/// [`verify_dir`] は失敗する（所有権を取り直すのは手作業）。
pub fn reset_dir_acl(dir: &std::path::Path, access: &DirAccess) -> Result<()> {
    imp::reset_dir_acl(dir, access)
}

/// ディレクトリの DACL と所有者を読み戻す（診断・テスト用）。
pub fn read_dir_dacl(dir: &std::path::Path) -> Result<ObservedDacl> {
    imp::read_dacl(dir)
}

/// 今のプロセスのトークンの利用者の SID（デスクトップ host が自分を
/// インストール利用者として渡すとき・テスト用）。
#[cfg(windows)]
pub fn current_user_sid() -> Result<InstallUserSid> {
    InstallUserSid::parse(&imp::current_user_sid_string()?)
}

#[cfg(not(windows))]
mod imp {
    use std::path::Path;

    use super::{DirAccess, ObservedDacl};
    use crate::error::{Result, SecretStoreError};

    pub(super) fn ensure_dir(_dir: &Path, _access: &DirAccess) -> Result<()> {
        Err(SecretStoreError::UnsupportedPlatform)
    }

    pub(super) fn verify_dir(_dir: &Path, _access: &DirAccess) -> Result<()> {
        Err(SecretStoreError::UnsupportedPlatform)
    }

    pub(super) fn reset_dir_acl(_dir: &Path, _access: &DirAccess) -> Result<()> {
        Err(SecretStoreError::UnsupportedPlatform)
    }

    pub(super) fn read_dacl(_dir: &Path) -> Result<ObservedDacl> {
        Err(SecretStoreError::UnsupportedPlatform)
    }
}

#[cfg(windows)]
mod imp {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, LocalFree, ERROR_ALREADY_EXISTS, HANDLE,
    };
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        ConvertStringSidToSidW, GetNamedSecurityInfoW, SetNamedSecurityInfoW, SDDL_REVISION_1,
        SE_FILE_OBJECT,
    };
    use windows_sys::Win32::Security::{
        GetAce, GetSecurityDescriptorControl, GetSecurityDescriptorDacl, GetTokenInformation,
        LookupAccountSidW, SidTypeUser, TokenUser, ACE_HEADER, ACL, DACL_SECURITY_INFORMATION,
        OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
        PSID, SECURITY_ATTRIBUTES, SE_DACL_PROTECTED, SID_NAME_USE, TOKEN_QUERY, TOKEN_USER,
    };
    use windows_sys::Win32::Storage::FileSystem::CreateDirectoryW;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    use super::{check_dacl, DirAccess, ObservedAce, ObservedDacl, ACCESS_ALLOWED_ACE_TYPE};
    use crate::error::{AclMismatch, IoOp, Result, SecretStoreError};

    /// `ACCESS_DENIED_ACE_TYPE`（許可と同じ並び: ヘッダ・マスク・SID）。
    const ACCESS_DENIED_ACE_TYPE: u8 = 1;

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    fn wide_str(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn last_error() -> u32 {
        // SAFETY: 直前の Win32 呼び出しの失敗の理由を読むだけ。
        unsafe { GetLastError() }
    }

    fn api(call: &'static str, os_error: u32) -> SecretStoreError {
        SecretStoreError::AclApi { call, os_error }
    }

    /// `LocalAlloc` で確保された領域を drop で `LocalFree` する。
    struct LocalBox(*mut core::ffi::c_void);

    impl Drop for LocalBox {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: 各 Win32 API が LocalAlloc で返した領域（それぞれの呼び出し元の
                // コメント）。ここ以外では解放しない。
                unsafe {
                    LocalFree(self.0 as _);
                }
            }
        }
    }

    /// SDDL から自己相対形式のセキュリティ記述子を作る。
    fn descriptor(access: &DirAccess) -> Result<LocalBox> {
        let sddl = wide_str(&access.sddl());
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: `sddl` は NUL 終端。成功すれば `sd` は LocalAlloc の領域（LocalBox が解放）。
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut sd,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(api(
                "ConvertStringSecurityDescriptorToSecurityDescriptorW",
                last_error(),
            ));
        }
        Ok(LocalBox(sd))
    }

    /// インストール利用者の SID が利用者のアカウントであること（グループではない）。
    fn ensure_user_account(access: &DirAccess) -> Result<()> {
        let Some(user) = access.install_user() else {
            return Ok(());
        };
        let text = wide_str(user.as_str());
        let mut sid: PSID = std::ptr::null_mut();
        // SAFETY: `text` は NUL 終端。成功すれば `sid` は LocalAlloc の領域。
        if unsafe { ConvertStringSidToSidW(text.as_ptr(), &mut sid) } == 0 {
            return Err(SecretStoreError::InvalidUserSid("SID の形式ではありません"));
        }
        let sid = LocalBox(sid);
        let mut name = [0u16; 256];
        let mut domain = [0u16; 256];
        let mut name_len = name.len() as u32;
        let mut domain_len = domain.len() as u32;
        let mut kind: SID_NAME_USE = 0;
        // SAFETY: バッファは長さとともに渡す。名前そのものは使わず、種類だけ見る。
        let ok = unsafe {
            LookupAccountSidW(
                std::ptr::null(),
                sid.0,
                name.as_mut_ptr(),
                &mut name_len,
                domain.as_mut_ptr(),
                &mut domain_len,
                &mut kind,
            )
        };
        if ok == 0 {
            return Err(SecretStoreError::InvalidUserSid(
                "この PC でアカウントを解決できません",
            ));
        }
        if kind != SidTypeUser {
            return Err(SecretStoreError::InvalidUserSid(
                "利用者のアカウントではありません（グループなど）",
            ));
        }
        Ok(())
    }

    fn inspect(dir: &Path) -> Result<bool> {
        match std::fs::symlink_metadata(dir) {
            Ok(meta) => {
                if meta.file_type().is_symlink() {
                    return Err(SecretStoreError::AclMismatch(AclMismatch::ReparsePoint));
                }
                if !meta.is_dir() {
                    return Err(SecretStoreError::AclMismatch(AclMismatch::NotADirectory));
                }
                Ok(true)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(SecretStoreError::io(IoOp::InspectDir, &e)),
        }
    }

    pub(super) fn ensure_dir(dir: &Path, access: &DirAccess) -> Result<()> {
        if inspect(dir)? {
            return verify_dir(dir, access);
        }
        ensure_user_account(access)?;
        if let Some(parent) = dir.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)
                .map_err(|e| SecretStoreError::io(IoOp::CreateDir, &e))?;
        }
        let sd = descriptor(access)?;
        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd.0,
            bInheritHandle: 0,
        };
        let path = wide(dir);
        // SAFETY: `path` は NUL 終端、`attributes` と記述子は呼び出しの間生きている。
        let ok = unsafe { CreateDirectoryW(path.as_ptr(), &attributes) };
        if ok == 0 {
            let os_error = last_error();
            if os_error != ERROR_ALREADY_EXISTS {
                return Err(SecretStoreError::io(
                    IoOp::CreateDir,
                    &std::io::Error::from_raw_os_error(os_error as i32),
                ));
            }
            // 確かめてから作るまでの間に誰かが作った: 作った側の形を確かめる。
        }
        verify_dir(dir, access)
    }

    pub(super) fn verify_dir(dir: &Path, access: &DirAccess) -> Result<()> {
        if !inspect(dir)? {
            return Err(SecretStoreError::io(
                IoOp::InspectDir,
                &std::io::Error::from(std::io::ErrorKind::NotFound),
            ));
        }
        let observed = read_dacl(dir)?;
        check_dacl(&observed, access).map_err(SecretStoreError::AclMismatch)
    }

    pub(super) fn reset_dir_acl(dir: &Path, access: &DirAccess) -> Result<()> {
        if !inspect(dir)? {
            return Err(SecretStoreError::io(
                IoOp::InspectDir,
                &std::io::Error::from(std::io::ErrorKind::NotFound),
            ));
        }
        ensure_user_account(access)?;
        let sd = descriptor(access)?;
        let mut present = 0;
        let mut defaulted = 0;
        let mut dacl: *mut ACL = std::ptr::null_mut();
        // SAFETY: `sd` は直前に作った有効な記述子。`dacl` はその中を指す（解放しない）。
        let ok =
            unsafe { GetSecurityDescriptorDacl(sd.0, &mut present, &mut dacl, &mut defaulted) };
        if ok == 0 || present == 0 || dacl.is_null() {
            return Err(api("GetSecurityDescriptorDacl", last_error()));
        }
        let path = wide(dir);
        // SAFETY: `path` は NUL 終端、`dacl` は `sd` が生きている間有効。
        // PROTECTED_DACL で親からの継承を切る。所有者・グループ・SACL は変えない。
        let status = unsafe {
            SetNamedSecurityInfoW(
                path.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                dacl,
                std::ptr::null(),
            )
        };
        if status != 0 {
            return Err(api("SetNamedSecurityInfoW", status));
        }
        Ok(())
    }

    fn sid_string(sid: PSID) -> Result<String> {
        let mut text: *mut u16 = std::ptr::null_mut();
        // SAFETY: `sid` は呼び出し元が有効な SID を指すことを保証する。成功すれば
        // `text` は LocalAlloc の NUL 終端文字列。
        if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
            return Err(api("ConvertSidToStringSidW", last_error()));
        }
        let text = LocalBox(text as _);
        let ptr = text.0 as *const u16;
        let mut len = 0;
        // SAFETY: NUL 終端まで読む。
        while unsafe { *ptr.add(len) } != 0 {
            len += 1;
        }
        // SAFETY: `len` 文字ぶんは上で読めた範囲。
        let slice = unsafe { std::slice::from_raw_parts(ptr, len) };
        Ok(String::from_utf16_lossy(slice))
    }

    pub(super) fn read_dacl(dir: &Path) -> Result<ObservedDacl> {
        let path = wide(dir);
        let mut owner: PSID = std::ptr::null_mut();
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: `path` は NUL 終端。成功すれば `sd` は LocalAlloc の領域で、
        // `owner`・`dacl` はその中を指す（`sd` を解放するまで有効）。
        let status = unsafe {
            GetNamedSecurityInfoW(
                path.as_ptr(),
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                &mut owner,
                std::ptr::null_mut(),
                &mut dacl,
                std::ptr::null_mut(),
                &mut sd,
            )
        };
        if status != 0 {
            return Err(api("GetNamedSecurityInfoW", status));
        }
        let sd = LocalBox(sd);

        let mut control: u16 = 0;
        let mut revision: u32 = 0;
        // SAFETY: `sd` は有効な記述子。
        if unsafe { GetSecurityDescriptorControl(sd.0, &mut control, &mut revision) } == 0 {
            return Err(api("GetSecurityDescriptorControl", last_error()));
        }
        let owner = sid_string(owner)?;

        let aces = if dacl.is_null() {
            None
        } else {
            // SAFETY: `dacl` は `sd` の中の有効な ACL。
            let count = unsafe { (*dacl).AceCount };
            let mut aces = Vec::with_capacity(count as usize);
            for index in 0..u32::from(count) {
                let mut ace: *mut core::ffi::c_void = std::ptr::null_mut();
                // SAFETY: index < AceCount。`ace` は ACL の中を指す。
                if unsafe { GetAce(dacl, index, &mut ace) } == 0 {
                    return Err(api("GetAce", last_error()));
                }
                // SAFETY: どの ACE も ACE_HEADER で始まる。
                let header = unsafe { *(ace as *const ACE_HEADER) };
                let observed = if header.AceType == ACCESS_ALLOWED_ACE_TYPE
                    || header.AceType == ACCESS_DENIED_ACE_TYPE
                {
                    // 許可・拒否の ACE は ACE_HEADER（4 バイト）・マスク（4 バイト）・
                    // SID の並び（ACCESS_ALLOWED_ACE / ACCESS_DENIED_ACE）。
                    // SAFETY: 種類を確かめた上で、構造体の並びどおりに読む。
                    let mask = unsafe { *((ace as *const u8).add(4) as *const u32) };
                    let sid = unsafe { (ace as *mut u8).add(8) } as PSID;
                    ObservedAce {
                        ace_type: header.AceType,
                        flags: header.AceFlags,
                        mask,
                        sid: sid_string(sid)?,
                    }
                } else {
                    ObservedAce {
                        ace_type: header.AceType,
                        flags: header.AceFlags,
                        mask: 0,
                        sid: String::new(),
                    }
                };
                aces.push(observed);
            }
            Some(aces)
        };
        Ok(ObservedDacl {
            protected: control & SE_DACL_PROTECTED != 0,
            owner,
            aces,
        })
    }

    pub(super) fn current_user_sid_string() -> Result<String> {
        let mut token: HANDLE = std::ptr::null_mut();
        // SAFETY: 今のプロセスの疑似ハンドル。成功すれば `token` を閉じる責任を持つ。
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(api("OpenProcessToken", last_error()));
        }
        struct Token(HANDLE);
        impl Drop for Token {
            fn drop(&mut self) {
                // SAFETY: OpenProcessToken が返したハンドル。
                unsafe {
                    CloseHandle(self.0);
                }
            }
        }
        let token = Token(token);
        let mut needed = 0u32;
        // SAFETY: 1 回目は長さを問い合わせるだけ（失敗が正常）。
        unsafe { GetTokenInformation(token.0, TokenUser, std::ptr::null_mut(), 0, &mut needed) };
        if needed == 0 {
            return Err(api("GetTokenInformation", last_error()));
        }
        // TOKEN_USER はポインタを含むので、8 バイト境界のバッファに読む。
        let mut buffer = vec![0u64; (needed as usize).div_ceil(8)];
        // SAFETY: `buffer` は `needed` バイト以上。
        if unsafe {
            GetTokenInformation(
                token.0,
                TokenUser,
                buffer.as_mut_ptr() as _,
                needed,
                &mut needed,
            )
        } == 0
        {
            return Err(api("GetTokenInformation", last_error()));
        }
        // SAFETY: 成功した GetTokenInformation(TokenUser) は先頭に TOKEN_USER を書く。
        // その SID は `buffer` の中を指す（`buffer` が生きている間有効）。
        let user = unsafe { &*(buffer.as_ptr() as *const TOKEN_USER) };
        sid_string(user.User.Sid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const USER: &str = "S-1-5-21-1111111111-2222222222-3333333333-1001";

    fn user() -> InstallUserSid {
        InstallUserSid::parse(USER).unwrap()
    }

    fn ace(sid: &str, mask: u32) -> ObservedAce {
        ObservedAce {
            ace_type: ACCESS_ALLOWED_ACE_TYPE,
            flags: INHERIT_FLAGS,
            mask,
            sid: sid.to_owned(),
        }
    }

    fn good(owner: &str) -> ObservedDacl {
        ObservedDacl {
            protected: true,
            owner: owner.to_owned(),
            aces: Some(vec![
                ace(SYSTEM_SID, FULL_ACCESS_MASK),
                ace(ADMINISTRATORS_SID, FULL_ACCESS_MASK),
                ace(USER, MODIFY_ACCESS_MASK),
            ]),
        }
    }

    #[test]
    fn sid_parsing_accepts_only_account_sids_without_sddl_metacharacters() {
        assert_eq!(user().as_str(), USER);
        // Microsoft Entra ID の利用者。
        let entra = "S-1-12-1-1234567890-1234567890-1234567890-1234567890";
        assert_eq!(InstallUserSid::parse(entra).unwrap().as_str(), entra);
        assert!(
            DirAccess::with_install_user(InstallUserSid::parse(entra).unwrap())
                .sddl()
                .ends_with(&format!("(A;OICI;0x1301bf;;;{entra})"))
        );
        for bad in [
            "",
            "S-1-1-0",
            "S-1-5-11",
            "S-1-5-32-545",
            "S-1-5-18",
            "S-1-5-21-",
            "S-1-5-21-1",
            "S-1-5-21-1--2",
            "S-1-5-21-1-2)(A;OICI;FA;;;WD",
            "S-1-5-21-1-2;",
            "s-1-5-21-1-2",
            "S-1-5-21-1-12345678901",
            // 利用者のアカウント以外の領域。
            "S-1-5-32-544",
            "S-1-5-32-547",
            "S-1-5-19",
            "S-1-5-20",
            "S-1-5-4",
            "S-1-5-80-1-2",
            "S-1-12-",
            "S-1-12-1-",
            "S-1-12-1-1",
            "S-1-12-2-1-2",
            "S-1-12-1-1-2)(A;;FA;;;WD",
        ] {
            assert!(
                matches!(
                    InstallUserSid::parse(bad),
                    Err(SecretStoreError::InvalidUserSid(_))
                ),
                "{bad:?} を受け付けた"
            );
        }
    }

    #[test]
    fn sddl_is_protected_and_lists_exactly_the_expected_aces() {
        assert_eq!(
            DirAccess::service_only().sddl(),
            "D:P(A;OICI;FA;;;S-1-5-18)(A;OICI;FA;;;S-1-5-32-544)"
        );
        assert_eq!(
            DirAccess::with_install_user(user()).sddl(),
            format!(
                "D:P(A;OICI;FA;;;S-1-5-18)(A;OICI;FA;;;S-1-5-32-544)(A;OICI;0x1301bf;;;{USER})"
            )
        );
    }

    #[test]
    fn check_accepts_the_exact_shape_in_any_order_and_any_allowed_owner() {
        let access = DirAccess::with_install_user(user());
        for owner in [SYSTEM_SID, ADMINISTRATORS_SID, USER] {
            assert_eq!(check_dacl(&good(owner), &access), Ok(()));
        }
        let mut reordered = good(SYSTEM_SID);
        reordered.aces.as_mut().unwrap().reverse();
        assert_eq!(check_dacl(&reordered, &access), Ok(()));

        let service = ObservedDacl {
            protected: true,
            owner: ADMINISTRATORS_SID.to_owned(),
            aces: Some(vec![
                ace(SYSTEM_SID, FULL_ACCESS_MASK),
                ace(ADMINISTRATORS_SID, FULL_ACCESS_MASK),
            ]),
        };
        assert_eq!(check_dacl(&service, &DirAccess::service_only()), Ok(()));
    }

    #[test]
    fn check_rejects_every_deviation() {
        let access = DirAccess::with_install_user(user());

        let mut null_dacl = good(SYSTEM_SID);
        null_dacl.aces = None;
        assert_eq!(check_dacl(&null_dacl, &access), Err(AclMismatch::NoDacl));

        let mut unprotected = good(SYSTEM_SID);
        unprotected.protected = false;
        assert_eq!(
            check_dacl(&unprotected, &access),
            Err(AclMismatch::NotProtected)
        );

        let mut inherited = good(SYSTEM_SID);
        inherited.aces.as_mut().unwrap()[0].flags |= 0x10;
        assert!(matches!(
            check_dacl(&inherited, &access),
            Err(AclMismatch::InheritedAce { .. })
        ));

        // Users・Everyone を足すと拒否。
        for extra in ["S-1-5-32-545", "S-1-1-0", "S-1-5-11"] {
            let mut widened = good(SYSTEM_SID);
            widened.aces.as_mut().unwrap().push(ace(extra, 0x0012_00A9));
            assert!(
                matches!(
                    check_dacl(&widened, &access),
                    Err(AclMismatch::UnexpectedAce { ref sid, .. }) if sid == extra
                ),
                "{extra}"
            );
        }

        // 利用者にフルコントロール（WRITE_DAC を含む）は広すぎる。
        let mut full_user = good(SYSTEM_SID);
        full_user.aces.as_mut().unwrap()[2].mask = FULL_ACCESS_MASK;
        assert!(matches!(
            check_dacl(&full_user, &access),
            Err(AclMismatch::UnexpectedAce { .. })
        ));

        // 拒否の ACE も想定外（形を完全に一致させる）。
        let mut deny = good(SYSTEM_SID);
        deny.aces.as_mut().unwrap().push(ObservedAce {
            ace_type: 1,
            ..ace("S-1-5-32-545", FULL_ACCESS_MASK)
        });
        assert!(matches!(
            check_dacl(&deny, &access),
            Err(AclMismatch::UnexpectedAce { ace_type: 1, .. })
        ));

        // 継承フラグが違う（OI だけ）。
        let mut flags = good(SYSTEM_SID);
        flags.aces.as_mut().unwrap()[1].flags = 0x01;
        assert!(matches!(
            check_dacl(&flags, &access),
            Err(AclMismatch::UnexpectedAce { .. })
        ));

        // 利用者の ACE が無い / 重複している。
        let mut missing = good(SYSTEM_SID);
        missing.aces.as_mut().unwrap().pop();
        assert_eq!(
            check_dacl(&missing, &access),
            Err(AclMismatch::MissingAce {
                sid: USER.to_owned(),
                mask: MODIFY_ACCESS_MASK
            })
        );
        let mut duplicated = good(SYSTEM_SID);
        duplicated
            .aces
            .as_mut()
            .unwrap()
            .push(ace(SYSTEM_SID, FULL_ACCESS_MASK));
        assert!(matches!(
            check_dacl(&duplicated, &access),
            Err(AclMismatch::UnexpectedAce { .. })
        ));

        // サービスだけの構成に利用者の ACE があれば広すぎる。
        assert!(matches!(
            check_dacl(&good(SYSTEM_SID), &DirAccess::service_only()),
            Err(AclMismatch::UnexpectedAce { ref sid, .. }) if sid == USER
        ));

        // 所有者が許可外（一般の利用者が先に作った）。
        let other = "S-1-5-21-1111111111-2222222222-3333333333-1002";
        assert_eq!(
            check_dacl(&good(other), &access),
            Err(AclMismatch::DisallowedOwner {
                sid: other.to_owned()
            })
        );
        // サービスだけの構成では、インストール利用者も所有者として許さない。
        let service_owned_by_user = ObservedDacl {
            protected: true,
            owner: USER.to_owned(),
            aces: Some(vec![
                ace(SYSTEM_SID, FULL_ACCESS_MASK),
                ace(ADMINISTRATORS_SID, FULL_ACCESS_MASK),
            ]),
        };
        assert!(matches!(
            check_dacl(&service_owned_by_user, &DirAccess::service_only()),
            Err(AclMismatch::DisallowedOwner { .. })
        ));
    }

    #[test]
    fn modify_mask_excludes_acl_and_owner_rights() {
        const WRITE_DAC: u32 = 0x0004_0000;
        const WRITE_OWNER: u32 = 0x0008_0000;
        assert_eq!(MODIFY_ACCESS_MASK & (WRITE_DAC | WRITE_OWNER), 0);
        assert_eq!(
            FULL_ACCESS_MASK & (WRITE_DAC | WRITE_OWNER),
            WRITE_DAC | WRITE_OWNER
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn non_windows_fails_closed() {
        let dir = std::path::Path::new("/tmp/banto-secret-store-unsupported");
        let access = DirAccess::service_only();
        assert_eq!(
            ensure_dir(dir, &access),
            Err(SecretStoreError::UnsupportedPlatform)
        );
        assert_eq!(
            verify_dir(dir, &access),
            Err(SecretStoreError::UnsupportedPlatform)
        );
        assert_eq!(
            reset_dir_acl(dir, &access),
            Err(SecretStoreError::UnsupportedPlatform)
        );
    }

    #[cfg(windows)]
    mod windows {
        use super::super::*;

        fn me() -> InstallUserSid {
            current_user_sid().expect("今の利用者の SID")
        }

        fn has_no_broad_groups(dacl: &ObservedDacl) {
            for ace in dacl.aces.as_ref().unwrap() {
                for broad in ["S-1-5-32-545", "S-1-1-0", "S-1-5-11", "S-1-5-4"] {
                    assert_ne!(ace.sid, broad, "{dacl:?}");
                }
            }
        }

        #[test]
        fn a_created_directory_reads_back_with_the_exact_protected_dacl() {
            let root = tempfile::tempdir().unwrap();
            let dir = root.path().join("secrets").join("hub-keys");
            let access = DirAccess::with_install_user(me());
            ensure_dir(&dir, &access).unwrap();

            let dacl = read_dir_dacl(&dir).unwrap();
            assert!(dacl.protected, "継承しない: {dacl:?}");
            let aces = dacl.aces.as_ref().unwrap();
            assert_eq!(aces.len(), 3, "{dacl:?}");
            for ace in aces {
                assert_eq!(ace.flags & 0x10, 0, "継承された ACE: {dacl:?}");
                assert_eq!(ace.ace_type, ACCESS_ALLOWED_ACE_TYPE);
                assert_eq!(ace.flags, INHERIT_FLAGS);
            }
            let find = |sid: &str| aces.iter().find(|a| a.sid == sid).map(|a| a.mask);
            assert_eq!(find(SYSTEM_SID), Some(FULL_ACCESS_MASK));
            assert_eq!(find(ADMINISTRATORS_SID), Some(FULL_ACCESS_MASK));
            assert_eq!(find(me().as_str()), Some(MODIFY_ACCESS_MASK));
            has_no_broad_groups(&dacl);

            // 2 回目は作らずに確かめるだけ（冪等）。
            ensure_dir(&dir, &access).unwrap();
            verify_dir(&dir, &access).unwrap();
        }

        #[test]
        fn a_service_only_directory_has_no_user_ace() {
            let root = tempfile::tempdir().unwrap();
            let dir = root.path().join("svc");
            let access = DirAccess::service_only();
            // 一般の利用者として動くテストでは、作った後の確認（所有者 = 自分）は
            // 失敗する（サービスだけの構成を一般の利用者が作るのは想定外）。
            // ここでは作られた DACL の形だけを確かめる。
            let _ = ensure_dir(&dir, &access);
            let dacl = read_dir_dacl(&dir).unwrap();
            assert!(dacl.protected);
            let mut sids: Vec<_> = dacl
                .aces
                .as_ref()
                .unwrap()
                .iter()
                .map(|a| (a.sid.clone(), a.mask))
                .collect();
            sids.sort();
            assert_eq!(
                sids,
                vec![
                    (SYSTEM_SID.to_owned(), FULL_ACCESS_MASK),
                    (ADMINISTRATORS_SID.to_owned(), FULL_ACCESS_MASK),
                ]
            );
            has_no_broad_groups(&dacl);
            // 片付け: 所有者は ACL を書き換えられるので、自分を足してから消す。
            reset_dir_acl(&dir, &DirAccess::with_install_user(me())).unwrap();
        }

        #[test]
        fn an_existing_directory_with_another_acl_is_refused_until_reset() {
            let root = tempfile::tempdir().unwrap();
            let dir = root.path().join("plain");
            std::fs::create_dir(&dir).unwrap();
            let access = DirAccess::with_install_user(me());

            let err = ensure_dir(&dir, &access).unwrap_err();
            assert!(
                matches!(
                    err,
                    SecretStoreError::AclMismatch(AclMismatch::NotProtected)
                ),
                "{err:?}"
            );
            // 黙って直さない。
            assert!(!read_dir_dacl(&dir).unwrap().protected);

            reset_dir_acl(&dir, &access).unwrap();
            verify_dir(&dir, &access).unwrap();
        }

        #[test]
        fn a_group_sid_is_refused_as_the_install_user() {
            // この PC の「None」/「Domain Users」等は S-1-5-21-…-513。
            let me = me();
            let machine = me.as_str().rsplit_once('-').unwrap().0;
            let group = InstallUserSid::parse(&format!("{machine}-513")).unwrap();
            let root = tempfile::tempdir().unwrap();
            let dir = root.path().join("g");
            let err = ensure_dir(&dir, &DirAccess::with_install_user(group)).unwrap_err();
            assert!(
                matches!(err, SecretStoreError::InvalidUserSid(_)),
                "{err:?}"
            );
            assert!(!dir.exists(), "拒んだときはフォルダを作らない");
        }
    }
}
