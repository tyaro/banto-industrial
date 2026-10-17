//! Win32 を呼ぶ実装（Windows だけ）。呼び出しの順序・引数・「既に存在する」と
//! みなす戻り値は、banto-hub-core の `service_operators.rs`・
//! `service_elevated.rs` から移す前のまま（#392 B3-3a、挙動は不変）。

use windows_sys::Win32::Foundation::{GetLastError, LocalFree, ERROR_NONE_MAPPED, HLOCAL};
use windows_sys::Win32::NetworkManagement::NetManagement::{
    NetLocalGroupAdd, NetLocalGroupAddMembers, LOCALGROUP_INFO_1, LOCALGROUP_MEMBERS_INFO_0,
    LOCALGROUP_MEMBERS_INFO_3,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSecurityDescriptorToStringSecurityDescriptorW, ConvertStringSidToSidW, SetEntriesInAclW,
    EXPLICIT_ACCESS_W, NO_MULTIPLE_TRUSTEE, SDDL_REVISION_1, SET_ACCESS, TRUSTEE_IS_GROUP,
    TRUSTEE_IS_SID, TRUSTEE_W,
};
use windows_sys::Win32::Security::{
    CheckTokenMembership, GetSecurityDescriptorDacl, InitializeSecurityDescriptor,
    LookupAccountNameW, SetSecurityDescriptorDacl, SidTypeUser, ACL, PSECURITY_DESCRIPTOR, PSID,
    SECURITY_DESCRIPTOR, SID_NAME_USE,
};
use windows_sys::Win32::System::Services::{
    CloseServiceHandle, OpenSCManagerW, OpenServiceW, QueryServiceObjectSecurity,
    SetServiceObjectSecurity, SC_HANDLE, SC_MANAGER_CONNECT,
};

use crate::{
    classify_group_add_status, classify_member_add_status, is_valid_sid_string, AccountSid,
    ElevError, GroupOutcome, MemberOutcome, OperatorsGroup, ServiceAclOutcome,
    OPERATORS_SERVICE_ACCESS_MASK,
};

/// `DACL_SECURITY_INFORMATION`（MSDN で安定して文書化された値。移す前の
/// banto-hub と同じく数値で持つ）。
const DACL_SECURITY_INFORMATION: u32 = 0x0000_0004;
/// `SECURITY_DESCRIPTOR_REVISION`。
const SECURITY_DESCRIPTOR_REVISION: u32 = 1;
/// サービスの DACL を読む・書き換えるための標準のアクセス権。
const READ_CONTROL: u32 = 0x0002_0000;
const WRITE_DAC: u32 = 0x0004_0000;

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn last_error() -> u32 {
    // SAFETY: 直前の Win32 呼び出しの失敗の理由を読むだけ。
    unsafe { GetLastError() }
}

/// `LocalAlloc` で確保された領域を drop で `LocalFree` する。
struct LocalBox(*mut core::ffi::c_void);

impl Drop for LocalBox {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: Win32 API（`SetEntriesInAclW`・`ConvertStringSidToSidW` 等）が
            // LocalAlloc で返した領域。ここ以外では解放しない。
            unsafe { LocalFree(self.0 as HLOCAL) };
        }
    }
}

/// アカウント名（利用者・ローカルグループのどちらでもよい）からこのマシンの
/// SID（バイト列）を引く。名前に一致するアカウントが無ければ `Ok(None)`
/// （`ERROR_NONE_MAPPED`）、それ以外の失敗は [`ElevError::LookupAccountName`]。
///
/// `LookupAccountNameW` の 2 段の呼び出し（長さ 0 で必要な大きさを聞く →
/// その大きさのバッファで引く）。banto-hub の `profile_acl` も SYSTEM・
/// Administrators・profile の owner の SID をこれで引く。
pub fn lookup_account_sid(name: &str) -> Result<Option<Vec<u8>>, ElevError> {
    Ok(lookup_account(name)?.map(|(sid, _)| sid))
}

/// アカウント名から SID を引き、**文字列形式**（`S-1-5-21-…`）と、その種類が
/// 利用者のアカウント（`SidTypeUser`）かを返す（#626 PR-A。banto-hub の
/// `grant-profile-acl` が、名前で受けた利用者を SID にしてグループ・well-known を
/// 断るため）。無ければ `Ok(None)`、それ以外の失敗は [`ElevError::LookupAccountName`]。
///
/// 文字列化は純関数 [`crate::sid_bytes_to_string`]（`ConvertSidToStringSidW` と同じ
/// 書式）。`LookupAccountNameW` が返した SID の形が壊れていることは無いはずだが、
/// 万一そうなら `ERROR_INVALID_SID`（1337）の [`ElevError::LookupAccountName`]。
/// 利用者のアカウントかを最終的に確かめるのは呼ぶ側（`banto_secret_store` の
/// `InstallUserSid::parse` と `check_user_account`）。
pub fn lookup_account_sid_string(name: &str) -> Result<Option<AccountSid>, ElevError> {
    /// `ERROR_INVALID_SID`。
    const ERROR_INVALID_SID: u32 = 1337;
    let Some((sid, kind)) = lookup_account(name)? else {
        return Ok(None);
    };
    let sid = crate::sid_bytes_to_string(&sid).ok_or_else(|| ElevError::LookupAccountName {
        name: name.to_string(),
        os_error: ERROR_INVALID_SID,
    })?;
    Ok(Some(AccountSid {
        sid,
        is_user: kind == SidTypeUser,
    }))
}

/// `LookupAccountNameW` の 2 段の呼び出しの本体（SID のバイト列と種類）。
fn lookup_account(name: &str) -> Result<Option<(Vec<u8>, SID_NAME_USE)>, ElevError> {
    let wide_name = to_wide(name);

    let mut sid_size: u32 = 0;
    let mut domain_size: u32 = 0;
    let mut sid_name_use: SID_NAME_USE = 0;

    // 1 回目: 大きさの問い合わせ。戻り値の成否は見ず `GetLastError` で分ける。
    // SAFETY: バッファは null・長さ 0。`wide_name` は NUL 終端。
    unsafe {
        LookupAccountNameW(
            std::ptr::null(),
            wide_name.as_ptr(),
            std::ptr::null_mut(),
            &mut sid_size,
            std::ptr::null_mut(),
            &mut domain_size,
            &mut sid_name_use,
        );
    }
    let first_error = last_error();
    if first_error == ERROR_NONE_MAPPED {
        return Ok(None);
    }
    if sid_size == 0 {
        return Err(ElevError::LookupAccountName {
            name: name.to_string(),
            os_error: first_error,
        });
    }

    let mut sid_buf = vec![0u8; sid_size as usize];
    let mut domain_buf = vec![0u16; domain_size.max(1) as usize];

    // 2 回目: 1 回目が報告した大きさちょうどのバッファで引く。
    // SAFETY: バッファはそれぞれ報告された長さで確保した。
    let ok = unsafe {
        LookupAccountNameW(
            std::ptr::null(),
            wide_name.as_ptr(),
            sid_buf.as_mut_ptr() as PSID,
            &mut sid_size,
            domain_buf.as_mut_ptr(),
            &mut domain_size,
            &mut sid_name_use,
        )
    };
    if ok == 0 {
        let os_error = last_error();
        if os_error == ERROR_NONE_MAPPED {
            return Ok(None);
        }
        return Err(ElevError::LookupAccountName {
            name: name.to_string(),
            os_error,
        });
    }

    Ok(Some((sid_buf, sid_name_use)))
}

/// [`crate::is_current_process_member`] の本体。
pub(crate) fn is_current_process_member(group_name: &str) -> Result<bool, ElevError> {
    let mut sid = match lookup_account_sid(group_name)? {
        Some(sid) => sid,
        // グループが無い - 安全側で false。
        None => return Ok(false),
    };

    let mut is_member: i32 = 0;
    // SAFETY: `sid` は `LookupAccountNameW` が書いた有効な SID で、このスコープで
    // 生きている。`hToken = NULL` は呼び出しスレッドの偽装トークン、無ければ
    // プロセスの主トークンを使う既定の挙動。
    let ok = unsafe {
        CheckTokenMembership(
            std::ptr::null_mut(),
            sid.as_mut_ptr() as PSID,
            &mut is_member,
        )
    };
    if ok == 0 {
        return Err(ElevError::CheckTokenMembership {
            os_error: last_error(),
        });
    }
    Ok(is_member != 0)
}

/// ローカルグループ `group` を無ければ作る（`NetLocalGroupAdd`、level 1 =
/// 名前と説明文）。既にあれば何も変えず [`GroupOutcome::Existed`]（説明文も
/// 書き換えない）。管理者として動くプロセスから呼ぶ。
pub fn ensure_local_group(group: &OperatorsGroup) -> Result<GroupOutcome, ElevError> {
    let mut name_wide = to_wide(group.name);
    let mut comment_wide = to_wide(group.comment);
    let info = LOCALGROUP_INFO_1 {
        lgrpi1_name: name_wide.as_mut_ptr(),
        lgrpi1_comment: comment_wide.as_mut_ptr(),
    };
    let mut parm_err: u32 = 0;
    // SAFETY: `info` はこのスコープで生きている `name_wide`／`comment_wide` だけを
    // 指す。level 1 は `LOCALGROUP_INFO_1`。
    let status = unsafe {
        NetLocalGroupAdd(
            std::ptr::null(),
            1,
            &info as *const LOCALGROUP_INFO_1 as *const u8,
            &mut parm_err,
        )
    };
    classify_group_add_status(status).ok_or_else(|| ElevError::CreateGroup {
        group: group.name.to_string(),
        status,
    })
}

/// 利用者を名前で `group` に足す（`NetLocalGroupAddMembers`、level 3 =
/// `DOMAIN\name` か `name`。SID の解決は API の中）。既にメンバーなら
/// [`MemberOutcome::AlreadyMember`]。banto-hub の互換のための口で、新しく
/// 使う側は [`add_member_sid`] を使う。
pub fn add_member_name(
    group: &OperatorsGroup,
    user_name: &str,
) -> Result<MemberOutcome, ElevError> {
    let group_wide = to_wide(group.name);
    let mut member_wide = to_wide(user_name);
    let info = LOCALGROUP_MEMBERS_INFO_3 {
        lgrmi3_domainandname: member_wide.as_mut_ptr(),
    };
    // SAFETY: `group_wide`／`member_wide` はこのスコープで生きている。level 3 は
    // `LOCALGROUP_MEMBERS_INFO_3`、件数は 1。
    let status = unsafe {
        NetLocalGroupAddMembers(
            std::ptr::null(),
            group_wide.as_ptr(),
            3,
            &info as *const LOCALGROUP_MEMBERS_INFO_3 as *const u8,
            1,
        )
    };
    classify_member_add_status(status).ok_or_else(|| ElevError::AddMember {
        member: user_name.to_string(),
        group: group.name.to_string(),
        status,
    })
}

/// SID（文字列）で `group` に足す（`NetLocalGroupAddMembers`、level 0 = SID）。
/// 形を [`is_valid_sid_string`] で確かめてから `ConvertStringSidToSidW` に渡す。
/// 既にメンバーなら [`MemberOutcome::AlreadyMember`]。その SID が利用者の
/// アカウントか・どの利用者を足してよいかは呼ぶ側（製品の elev）が決める。
pub fn add_member_sid(group: &OperatorsGroup, sid: &str) -> Result<MemberOutcome, ElevError> {
    if !is_valid_sid_string(sid) {
        return Err(ElevError::InvalidSid(sid.to_string()));
    }
    let text = to_wide(sid);
    let mut psid: PSID = std::ptr::null_mut();
    // SAFETY: `text` は NUL 終端。成功すれば `psid` は LocalAlloc の領域（LocalBox が解放）。
    if unsafe { ConvertStringSidToSidW(text.as_ptr(), &mut psid) } == 0 {
        return Err(ElevError::ConvertSid {
            sid: sid.to_string(),
            os_error: last_error(),
        });
    }
    let psid = LocalBox(psid);
    let group_wide = to_wide(group.name);
    let info = LOCALGROUP_MEMBERS_INFO_0 { lgrmi0_sid: psid.0 };
    // SAFETY: `group_wide` と `psid` はこのスコープで生きている。level 0 は
    // `LOCALGROUP_MEMBERS_INFO_0`、件数は 1。
    let status = unsafe {
        NetLocalGroupAddMembers(
            std::ptr::null(),
            group_wide.as_ptr(),
            0,
            &info as *const LOCALGROUP_MEMBERS_INFO_0 as *const u8,
            1,
        )
    };
    classify_member_add_status(status).ok_or_else(|| ElevError::AddMember {
        member: sid.to_string(),
        group: group.name.to_string(),
        status,
    })
}

/// サービス `service_name` の既存の DACL に、`group` への ACE
/// （[`crate::OPERATORS_SERVICE_ACCESS_MASK`]、SDDL では
/// [`crate::operators_ace_sddl`]）を足す。冪等（`SET_ACCESS` は同じ trustee の
/// 既存の ACE を置き換える）。
///
/// 順序: グループの SID を引く（無ければ [`ElevError::GroupNotFound`]、SCM には
/// まだ触らない）→ `OpenSCManagerW(CONNECT)` → `OpenServiceW(READ_CONTROL |
/// WRITE_DAC)` → `QueryServiceObjectSecurity`（2 段）→
/// `GetSecurityDescriptorDacl` → `SetEntriesInAclW` → 絶対形式の記述子に
/// 載せて `SetServiceObjectSecurity`。管理者として動くプロセスから呼ぶ。
///
/// 足した結果の DACL が今の DACL と同じ（= 既に同じ ACE があった）なら、書かずに
/// [`ServiceAclOutcome::AlreadyPresent`] を返す（`SET_ACCESS` が同じ内容に置き換えるだけ
/// なので、書いても結果は変わらない。出力の「足しました」を嘘にしないため。#628）。
pub fn grant_service_operators(
    service_name: &str,
    group: &OperatorsGroup,
) -> Result<ServiceAclOutcome, ElevError> {
    let mut operators_sid = lookup_account_sid(group.name)?
        .ok_or_else(|| ElevError::GroupNotFound(group.name.to_string()))?;

    // 結果の出し分け（消えた ACE が Operators のものか）に使う文字列形式。
    let sid_text = crate::sid_bytes_to_string(&operators_sid).ok_or_else(|| {
        ElevError::LookupAccountName {
            name: group.name.to_string(),
            // ERROR_INVALID_SID
            os_error: 1337,
        }
    })?;

    let scm = open_scm()?;
    let result = grant_with_scm(
        scm,
        service_name,
        operators_sid.as_mut_ptr() as PSID,
        &sid_text,
    );
    // SAFETY: `scm` は `open_scm` が返した有効なハンドル。
    unsafe { CloseServiceHandle(scm) };
    result
}

fn open_scm() -> Result<SC_HANDLE, ElevError> {
    // SAFETY: 引数はすべて null（ローカルマシン・既定のデータベース）。
    let scm = unsafe { OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT) };
    if scm.is_null() {
        return Err(ElevError::OpenScm(last_error()));
    }
    Ok(scm)
}

fn grant_with_scm(
    scm: SC_HANDLE,
    service_name: &str,
    operators_sid: PSID,
    sid_text: &str,
) -> Result<ServiceAclOutcome, ElevError> {
    let service_wide = to_wide(service_name);
    // SAFETY: `scm` は呼び出し元が開いた有効なハンドル。
    let service = unsafe { OpenServiceW(scm, service_wide.as_ptr(), READ_CONTROL | WRITE_DAC) };
    if service.is_null() {
        return Err(ElevError::OpenService {
            service: service_name.to_string(),
            os_error: last_error(),
        });
    }

    let result = grant_with_service(service, service_name, operators_sid, sid_text);
    // SAFETY: `service` は直前に `OpenServiceW` が返した有効なハンドル。
    unsafe { CloseServiceHandle(service) };
    result
}

fn grant_with_service(
    service: SC_HANDLE,
    service_name: &str,
    operators_sid: PSID,
    sid_text: &str,
) -> Result<ServiceAclOutcome, ElevError> {
    // 1 回目: 必要な大きさの問い合わせ。
    let mut needed: u32 = 0;
    // SAFETY: 長さ 0・null で呼ぶのは Win32 の想定どおり（FALSE を返し `needed` に
    // 必要な大きさを書く）。
    unsafe {
        QueryServiceObjectSecurity(
            service,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            0,
            &mut needed,
        )
    };
    if needed == 0 {
        return Err(ElevError::QuerySecurity {
            service: service_name.to_string(),
            os_error: last_error(),
        });
    }

    let mut sd_buf = vec![0u8; needed as usize];
    // SAFETY: `sd_buf` は直前の呼び出しが報告した大きさちょうど。
    let ok = unsafe {
        QueryServiceObjectSecurity(
            service,
            DACL_SECURITY_INFORMATION,
            sd_buf.as_mut_ptr() as PSECURITY_DESCRIPTOR,
            needed,
            &mut needed,
        )
    };
    if ok == 0 {
        return Err(ElevError::QuerySecurity {
            service: service_name.to_string(),
            os_error: last_error(),
        });
    }

    let merged = merged_dacl(sd_buf.as_mut_ptr() as PSECURITY_DESCRIPTOR, operators_sid)?;
    let mut new_sd = absolute_sd_with_dacl(&merged)?;

    // 足した結果が今と同じなら（既に同じ ACE がある）、書かない。
    let outcome = outcome_for(
        sd_buf.as_mut_ptr() as PSECURITY_DESCRIPTOR,
        &mut new_sd,
        sid_text,
    );
    if outcome == ServiceAclOutcome::AlreadyPresent {
        return Ok(outcome);
    }

    // SAFETY: `service` は有効なハンドル、`new_sd` は DACL を載せた有効な記述子で、
    // その DACL（`merged`）はこの呼び出しの後まで生きている。
    let ok = unsafe {
        SetServiceObjectSecurity(
            service,
            DACL_SECURITY_INFORMATION,
            &mut new_sd as *mut SECURITY_DESCRIPTOR as PSECURITY_DESCRIPTOR,
        )
    };
    if ok == 0 {
        return Err(ElevError::SetServiceSecurity {
            service: service_name.to_string(),
            os_error: last_error(),
        });
    }
    Ok(outcome)
}

/// 今の記述子（`current`。本番では `QueryServiceObjectSecurity` の自己相対形式）と、作り直した
/// 記述子（`new_sd`。絶対形式）の DACL を SDDL にして、[`crate::classify_dacl_change`] で
/// 結果を決める。SDDL に直せなければ `Added`（書く側に倒す）。本番とテストが同じ関数を通る。
fn outcome_for(
    current: PSECURITY_DESCRIPTOR,
    new_sd: &mut SECURITY_DESCRIPTOR,
    operators_sid: &str,
) -> ServiceAclOutcome {
    let merged = dacl_sddl_of(new_sd as *mut SECURITY_DESCRIPTOR as PSECURITY_DESCRIPTOR);
    match (dacl_sddl_of(current), merged) {
        (Some(current), Some(merged)) => {
            crate::classify_dacl_change(&current, &merged, operators_sid)
        }
        _ => ServiceAclOutcome::Added,
    }
}

/// 記述子 `sd` の DACL を SDDL の文字列にする（比べるため。直せなければ `None`）。
fn dacl_sddl_of(sd: PSECURITY_DESCRIPTOR) -> Option<String> {
    let mut out: *mut u16 = std::ptr::null_mut();
    let mut len: u32 = 0;
    // SAFETY: `sd` は呼び出し元が持つ有効な記述子。成功すれば `out` は LocalAlloc の文字列。
    let ok = unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            sd,
            SDDL_REVISION_1,
            DACL_SECURITY_INFORMATION,
            &mut out,
            &mut len,
        )
    };
    if ok == 0 {
        return None;
    }
    let out = LocalBox(out.cast());
    // SAFETY: NUL 終端の UTF-16。
    let text = unsafe {
        let p = out.0 as *const u16;
        let mut n = 0;
        while *p.add(n) != 0 {
            n += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(p, n))
    };
    Some(text)
}

/// 記述子 `sd` の DACL に Operators の ACE を `SET_ACCESS` で足した新しい ACL
/// （LocalAlloc の領域）。DACL が無い記述子なら、その ACE だけの ACL になる
/// （移す前と同じ）。記述子はメモリの上のものだけを読み、OS の何も変えない。
fn merged_dacl(sd: PSECURITY_DESCRIPTOR, operators_sid: PSID) -> Result<LocalBox, ElevError> {
    let mut dacl_present: i32 = 0;
    let mut dacl_ptr: *mut ACL = std::ptr::null_mut();
    let mut dacl_defaulted: i32 = 0;
    // SAFETY: `sd` は呼び出し元が持つ有効な記述子。
    let ok = unsafe {
        GetSecurityDescriptorDacl(sd, &mut dacl_present, &mut dacl_ptr, &mut dacl_defaulted)
    };
    if ok == 0 {
        return Err(ElevError::GetDacl(last_error()));
    }
    let old_dacl: *const ACL = if dacl_present != 0 {
        dacl_ptr as *const ACL
    } else {
        std::ptr::null()
    };

    let trustee = TRUSTEE_W {
        pMultipleTrustee: std::ptr::null_mut(),
        MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
        TrusteeForm: TRUSTEE_IS_SID,
        TrusteeType: TRUSTEE_IS_GROUP,
        // `TrusteeForm = TRUSTEE_IS_SID` のときは SID へのポインタをそのまま
        // キャストして渡す（MSDN `TRUSTEE` の契約）。
        ptstrName: operators_sid as *mut u16,
    };
    let explicit_access = EXPLICIT_ACCESS_W {
        grfAccessPermissions: OPERATORS_SERVICE_ACCESS_MASK,
        // 同じ trustee の既存のエントリを置き換える（加算しない）- 冪等。
        grfAccessMode: SET_ACCESS,
        // サービスは子を持たないので継承は無関係（NO_INHERITANCE = 0）。
        grfInheritance: 0,
        Trustee: trustee,
    };

    let mut new_acl: *mut ACL = std::ptr::null_mut();
    // SAFETY: `old_dacl` は `sd` の中の ACL（または null）、`explicit_access` は
    // このスコープで生きている。成功すれば `new_acl` は LocalAlloc の領域。
    let status = unsafe { SetEntriesInAclW(1, &explicit_access, old_dacl, &mut new_acl) };
    if status != 0 {
        return Err(ElevError::SetEntriesInAcl(status));
    }
    Ok(LocalBox(new_acl.cast()))
}

/// `acl` を DACL に持つ絶対形式の記述子。記述子は `acl` を指すだけなので、
/// 使い終わるまで `acl` を生かしておく。
fn absolute_sd_with_dacl(acl: &LocalBox) -> Result<SECURITY_DESCRIPTOR, ElevError> {
    let mut new_sd = SECURITY_DESCRIPTOR::default();
    // SAFETY: `new_sd` はこの関数のローカル変数。
    let ok = unsafe {
        InitializeSecurityDescriptor(
            &mut new_sd as *mut SECURITY_DESCRIPTOR as PSECURITY_DESCRIPTOR,
            SECURITY_DESCRIPTOR_REVISION,
        )
    };
    if ok == 0 {
        return Err(ElevError::InitializeSecurityDescriptor(last_error()));
    }
    // SAFETY: `acl.0` は `SetEntriesInAclW` が作った有効な ACL、`new_sd` は
    // 初期化済み。絶対形式の記述子は ACL へのポインタを持つだけ（自分自身は
    // 指さない）ので、戻り値として動かしてよい。
    let ok = unsafe {
        SetSecurityDescriptorDacl(
            &mut new_sd as *mut SECURITY_DESCRIPTOR as PSECURITY_DESCRIPTOR,
            1,
            acl.0 as *const ACL,
            0,
        )
    };
    if ok == 0 {
        return Err(ElevError::SetSecurityDescriptorDacl(last_error()));
    }
    Ok(new_sd)
}

#[cfg(test)]
mod tests {
    //! どれもメモリの上の記述子・ACL だけを扱う（ローカルグループ・サービス・
    //! レジストリ・ファイルには触らない。GitHub の Windows のランナーは管理者
    //! なので、実際に作る・書き換える経路はここでは動かさない）。

    use super::*;
    use windows_sys::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;

    const OPERATORS: &str = "S-1-5-21-1111111111-2222222222-3333333333-1001";

    /// サービスの既定に近い DACL（SYSTEM・Administrators・対話ログオンの利用者）。
    const EXISTING: &str =
        "D:(A;;CCLCSWRPWPDTLOCRRC;;;SY)(A;;CCDCLCSWRPWPDTLOCRSDRCWDWO;;;BA)(A;;CCLCSWLOCRRC;;;IU)";

    fn sd_from_sddl(sddl: &str) -> LocalBox {
        let text = to_wide(sddl);
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: `text` は NUL 終端。成功すれば `sd` は LocalAlloc の領域。
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                text.as_ptr(),
                SDDL_REVISION_1,
                &mut sd,
                std::ptr::null_mut(),
            )
        };
        assert_ne!(ok, 0, "SDDL を記述子にできない: {sddl}");
        LocalBox(sd)
    }

    fn sid_from_str(sid: &str) -> LocalBox {
        let text = to_wide(sid);
        let mut psid: PSID = std::ptr::null_mut();
        // SAFETY: `text` は NUL 終端。
        let ok = unsafe { ConvertStringSidToSidW(text.as_ptr(), &mut psid) };
        assert_ne!(ok, 0);
        LocalBox(psid)
    }

    fn dacl_sddl(sd: &mut SECURITY_DESCRIPTOR) -> String {
        dacl_sddl_of(sd as *mut SECURITY_DESCRIPTOR as PSECURITY_DESCRIPTOR).expect("SDDL にできる")
    }

    /// 本番と同じ `merged_dacl` → `absolute_sd_with_dacl` を通した DACL の SDDL。
    fn merge(existing: &str, sid: &str) -> String {
        let sd = sd_from_sddl(existing);
        let sid = sid_from_str(sid);
        let acl = merged_dacl(sd.0, sid.0).expect("merge");
        let mut new_sd = absolute_sd_with_dacl(&acl).expect("absolute sd");
        dacl_sddl(&mut new_sd)
    }

    #[test]
    fn grant_appends_the_operators_ace_and_keeps_existing_aces() {
        let merged = merge(EXISTING, OPERATORS);
        let ace = crate::operators_ace_sddl(OPERATORS).unwrap();
        assert_eq!(merged.matches(&ace).count(), 1, "{merged}");
        for kept in [
            "(A;;CCLCSWRPWPDTLOCRRC;;;SY)",
            "(A;;CCDCLCSWRPWPDTLOCRSDRCWDWO;;;BA)",
            "(A;;CCLCSWLOCRRC;;;IU)",
        ] {
            assert!(merged.contains(kept), "{kept} が消えた: {merged}");
        }
    }

    /// 本番と同じ比べ方（自己相対形式の今の記述子 × 作り直した絶対形式の記述子）で結果を
    /// 決める。`existing` は SDDL（`D:P(...)` などの制御フラグ付きも可）。
    fn outcome(existing: &str, sid: &str) -> ServiceAclOutcome {
        let sd = sd_from_sddl(existing);
        let sid_str = sid;
        let sid = sid_from_str(sid);
        let acl = merged_dacl(sd.0, sid.0).expect("merge");
        let mut new_sd = absolute_sd_with_dacl(&acl).expect("absolute sd");
        outcome_for(sd.0, &mut new_sd, sid_str)
    }

    #[test]
    fn outcome_is_decided_the_way_production_compares() {
        // (b) 無い → Added。
        assert_eq!(outcome(EXISTING, OPERATORS), ServiceAclOutcome::Added);
        // (a) 既にある → AlreadyPresent（書き込みを省く）。
        let with_ours = merge(EXISTING, OPERATORS);
        assert_eq!(
            outcome(&with_ours, OPERATORS),
            ServiceAclOutcome::AlreadyPresent
        );
        // (c) 同じ相手に別の権利 → Replaced。
        let wider = format!("{EXISTING}(A;;CCDCLCRPWP;;;{OPERATORS})");
        assert_eq!(outcome(&wider, OPERATORS), ServiceAclOutcome::Replaced);
        // (e) 既にあるが並びが正規でない（許可の後ろに拒否）→ Reordered（足していない）。
        // `SetEntriesInAclW` は拒否を許可より前に並べ直すので、書く側に倒れる。
        let ours_ace = crate::operators_ace_sddl(OPERATORS).unwrap();
        let non_canonical = format!("{with_ours}(D;;CCLC;;;S-1-5-21-1-2-3-3003)");
        assert_eq!(
            outcome(&non_canonical, OPERATORS),
            ServiceAclOutcome::Reordered,
            "{non_canonical} / {ours_ace}"
        );
        // (d) 元の記述子に制御フラグが付いていても、既にあれば AlreadyPresent、無ければ Added。
        for flags in ["P", "AI", "PAI"] {
            let existing_with = format!("D:{flags}{}", with_ours.trim_start_matches("D:"));
            assert_eq!(
                outcome(&existing_with, OPERATORS),
                ServiceAclOutcome::AlreadyPresent,
                "{flags}: {existing_with}"
            );
            let existing_without = format!("D:{flags}{}", EXISTING.trim_start_matches("D:"));
            assert_eq!(
                outcome(&existing_without, OPERATORS),
                ServiceAclOutcome::Added,
                "{flags}"
            );
        }
    }

    #[test]
    fn grant_is_idempotent() {
        let once = merge(EXISTING, OPERATORS);
        let twice = merge(&once, OPERATORS);
        assert_eq!(once, twice);
    }

    /// 同じ trustee に別の権利（例えば手で付けた `DC` 込み）があっても、
    /// `SET_ACCESS` で既定の 4 つに置き換わる（増え続けない・広がらない）。
    #[test]
    fn grant_replaces_a_wider_ace_for_the_same_group() {
        let wider = format!("{EXISTING}(A;;CCDCLCRPWP;;;{OPERATORS})");
        let merged = merge(&wider, OPERATORS);
        assert!(
            merged.contains(&crate::operators_ace_sddl(OPERATORS).unwrap()),
            "{merged}"
        );
        assert!(
            !merged.contains(&format!("(A;;CCDCLCRPWP;;;{OPERATORS})")),
            "{merged}"
        );
        assert_eq!(merged.matches(OPERATORS).count(), 1, "{merged}");
    }

    /// 他のグループの ACE には触らない。
    #[test]
    fn grant_leaves_other_trustees_alone() {
        let other = "S-1-5-21-1111111111-2222222222-3333333333-1002";
        let with_other = format!("{EXISTING}(A;;CCLC;;;{other})");
        let merged = merge(&with_other, OPERATORS);
        assert!(merged.contains(&format!("(A;;CCLC;;;{other})")), "{merged}");
    }

    #[test]
    fn add_member_sid_rejects_a_malformed_sid_before_any_win32_call() {
        let group = OperatorsGroup {
            name: "banto-service-elev test group that does not exist",
            comment: "",
        };
        assert_eq!(
            add_member_sid(&group, "S-1-5-21-1)(A;;FA;;;WD"),
            Err(ElevError::InvalidSid("S-1-5-21-1)(A;;FA;;;WD".to_string()))
        );
    }

    /// 無い名前は `Ok(None)`（読むだけ）。
    #[test]
    fn lookup_of_a_missing_name_is_none() {
        assert_eq!(
            lookup_account_sid("banto-service-elev test group that does not exist"),
            Ok(None)
        );
    }
}
