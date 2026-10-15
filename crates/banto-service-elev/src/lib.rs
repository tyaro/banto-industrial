//! 製品ごとの昇格ヘルパー（banto-hub の `banto-hub-elev.exe`、ChronoGazer の
//! `chronogazer-elev.exe`）が使う **Win32 の部品**を集めた共有 crate
//! （#392 B3-3a、docs/chronogazer-service-design.md §1.2・§4 の B3-3a の実装メモ）。
//!
//! ## 設計意図
//!
//! - **昇格の境界はこの crate に持たせない**。受け付けるアクションの一覧・
//!   引数の検証・手順の順序・昇格する exe とそのマニフェスト
//!   （`requireAdministrator`）・利用者に見える文言は、製品ごとの elev が持つ
//!   （2026-10-16 オーナー決定: elev の exe は製品ごと、昇格の境界は製品ごとに
//!   監査する）。ここにあるのは「何をどの名前で」を引数で受ける Win32 の
//!   呼び出しだけで、製品名・グループ名・サービス名を 1 つも持たない。
//! - **ローカルグループ**: [`OperatorsGroup`]（名前と説明文）を製品が定数で持ち、
//!   [`ensure_local_group`]（無ければ作る。冪等）・[`add_member_sid`]／
//!   [`add_member_name`]（既にメンバーなら何もしない。冪等）に渡す。
//!   `add_member_name` は banto-hub の互換のため（banto-hub は利用者を名前で
//!   受ける）。新しく使う側は SID で渡す（名前の解決の揺れが無い）。
//! - **サービスの DACL**: [`grant_service_operators`] は対象サービスの
//!   **既存の DACL に** Operators への ACE を 1 つ足す（丸ごと置き換えない。
//!   `QueryServiceObjectSecurity` → `SetEntriesInAclW`（`SET_ACCESS`）→
//!   `SetServiceObjectSecurity`）。`SET_ACCESS` は同じ trustee の既存の ACE を
//!   置き換えるので、何度実行しても ACE は 1 つのまま（冪等）。付ける権利は
//!   [`OPERATORS_SERVICE_ACCESS_MASK`]（`CC`/`LC`/`RP`/`WP` = 設定と状態の照会・
//!   開始・停止）だけで、`DC`（設定の変更）・`SD`（削除）・`WD`（DACL の変更）・
//!   `WO`（所有者の変更）は**意図して含めない**（Operators に `DC` を渡すと
//!   実行ファイルのパスを書き換えて LocalSystem で任意の exe を動かせる）。
//!   SDDL の表現は純関数 [`operators_ace_sddl`]。
//! - **トークンのメンバーシップ**: [`is_current_process_member`]（グループが
//!   未作成なら `Ok(false)`、安全側）と [`is_current_process_admin`]。
//!   どちらも `CheckTokenMembership` で、UAC で昇格していない split token
//!   では Administrators は deny-only になり `false` が返る（既知の性質。
//!   banto-hub の `service_operators` のモジュール doc 参照）。
//! - **昇格して起動する**: [`launch::run_elevated`]（`ShellExecuteExW` の
//!   verb `runas` → 終わりを待つ → 終了コード）。引数は**英数字と `-` だけの
//!   トークン**に限る（SID `S-1-5-21-…` もこの形）- 引用符・空白・メタ文字を
//!   通さないので、コマンドラインの組み立てで解釈が割れる余地が無い。
//! - SID は文字列（`&str`）で受ける。`banto-secret-store` の `InstallUserSid` には
//!   依存しない（この crate は「利用者のアカウントの SID か」という製品の
//!   判断を持たない）。形の確かめは [`is_valid_sid_string`]。
//!
//! ## Windows 以外
//!
//! Win32 を呼ぶ関数（グループ・DACL・runas）は `#[cfg(windows)]` だけで公開する
//! （呼ぶ側の elev の `main` も Windows だけ）。純関数・定数・エラー型と
//! [`is_current_process_member`]／[`is_current_process_admin`]（Windows 以外は
//! 常に `Ok(false)`。banto-hub のシェルが全 OS で呼ぶ）は全 OS で使える。
//! 再公開も `#[cfg(windows)]` で包む（Windows 以外で空の `pub use` になると
//! `unused_imports` で Linux の clippy が落ちる - B3-2a で一度踏んだ）。
//!
//! ## 履歴
//!
//! banto-hub-core の `service_operators.rs`（T17-2 スライス 1、メンバーシップの
//! 判定と `LookupAccountNameW`）と `service_elevated.rs`（同スライス 2、
//! グループの作成・メンバーの追加・サービス DACL への ACE）の Win32 の部分、
//! banto-hub-shell の `host_switch_ipc.rs`（runas の起動）から、#392 B3-3a で
//! 抽出した。挙動は変えていない: Win32 の呼び出しの順序・引数・「既に存在する」
//! とみなす戻り値（`NERR_GroupExists`・`ERROR_ALIAS_EXISTS`、
//! `NERR_UserInGroup`・`ERROR_MEMBER_IN_ALIAS`）は移す前のまま。banto-hub は
//! この crate のエラー（文言は製品名を含まない中立な形）を自分の従来の文言の
//! エラーへ写す（banto-hub-core の `service_elevated::map_elev_error` と
//! golden テスト）。足したのは SID で渡すメンバーの追加（[`add_member_sid`]、
//! ChronoGazer の B3-3b が使う）と、結果を値で返すこと（[`GroupOutcome`]・
//! [`MemberOutcome`]）だけ。

use thiserror::Error;

pub mod launch;

#[cfg(windows)]
mod win;

#[cfg(windows)]
pub use win::{
    add_member_name, add_member_sid, ensure_local_group, grant_service_operators,
    lookup_account_sid,
};

/// 製品ごとのローカルグループ（例: banto-hub の `BantoHub Operators`）。
/// 名前と説明文は製品が定数で持つ（`const` で作れるように `&'static str`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OperatorsGroup {
    /// グループ名。`LookupAccountNameW` は指定した文字列そのものを名前として
    /// 引くので、ローカライズされた OS でも同じ英語名で作れば解決できる。
    pub name: &'static str,
    /// `NetLocalGroupAdd` に渡す説明文（`lusrmgr.msc` に出る）。
    pub comment: &'static str,
}

/// [`ensure_local_group`] の結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupOutcome {
    /// このとき作った。
    Created,
    /// 既にあった（何も変えていない）。
    Existed,
}

/// [`add_member_sid`]／[`add_member_name`] の結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberOutcome {
    /// このとき追加した。
    Added,
    /// 既にメンバーだった（何も変えていない）。
    AlreadyMember,
}

/// Operators に付けるサービスのアクセス権（`SERVICE_QUERY_CONFIG(0x1) |
/// SERVICE_QUERY_STATUS(0x4) | SERVICE_START(0x10) | SERVICE_STOP(0x20)`）。
/// 全 OS でテストできるよう数値で持つ（windows-sys の定数と一致することは
/// Windows のテストが固定する）。
pub const OPERATORS_SERVICE_ACCESS_MASK: u32 = 0x0001 | 0x0004 | 0x0010 | 0x0020;

/// [`OPERATORS_SERVICE_ACCESS_MASK`] を SDDL のサービスのアクセス権で
/// 表したもの（`CC` + `LC` + `RP` + `WP`）。
pub const OPERATORS_SERVICE_SDDL_RIGHTS: &str = "CCLCRPWP";

/// Windows の組み込みグループ `BUILTIN\Administrators` の参照名
/// （[`is_current_process_admin`]。`LookupAccountNameW` はロケールに関わらず
/// この英語名で解決できる）。
pub const ADMINISTRATORS_GROUP_NAME: &str = "Administrators";

// `NetLocalGroupAdd`／`NetLocalGroupAddMembers` の戻り値のうち「成功」と
// 「既に存在する」を意味するもの（MSDN で文書化された値。windows-sys の定数と
// 一致することは Windows のテストが固定する）。実機（T17-2）で、2 回目の
// グループの作成に `NERR_GroupExists` ではなく `ERROR_ALIAS_EXISTS` が返る
// 環境を確かめたので、両方を「既にある」とみなす。
const NERR_SUCCESS: u32 = 0;
const NERR_GROUP_EXISTS: u32 = 2223;
const ERROR_ALIAS_EXISTS: u32 = 1379;
const NERR_USER_IN_GROUP: u32 = 2236;
const ERROR_MEMBER_IN_ALIAS: u32 = 1378;

/// `NetLocalGroupAdd` の戻り値を分類する（純関数）。`None` は失敗。
pub fn classify_group_add_status(status: u32) -> Option<GroupOutcome> {
    match status {
        NERR_SUCCESS => Some(GroupOutcome::Created),
        NERR_GROUP_EXISTS | ERROR_ALIAS_EXISTS => Some(GroupOutcome::Existed),
        _ => None,
    }
}

/// `NetLocalGroupAddMembers` の戻り値を分類する（純関数）。`None` は失敗
/// （典型例: 利用者が存在しない `NERR_UserNotFound`）。
pub fn classify_member_add_status(status: u32) -> Option<MemberOutcome> {
    match status {
        NERR_SUCCESS => Some(MemberOutcome::Added),
        NERR_USER_IN_GROUP | ERROR_MEMBER_IN_ALIAS => Some(MemberOutcome::AlreadyMember),
        _ => None,
    }
}

/// 文字列が SID の形（`S-1-<authority>-<sub>[-<sub>…]`）かを確かめる（純関数）。
///
/// 確かめるのは形だけ: 版は `1`、識別子機関は 48 ビットに収まる 10 進、
/// 下位機関は 1〜15 個でそれぞれ 32 ビットに収まる 10 進（符号・空・先頭の
/// 空白は不可）。それが利用者か・グループか・このマシンで解決できるかは
/// 見ない（製品の判断）。SDDL や runas の引数に埋め込む前の、区切り文字・
/// メタ文字を通さないための確かめ。
pub fn is_valid_sid_string(s: &str) -> bool {
    let mut parts = s.split('-');
    if parts.next() != Some("S") || parts.next() != Some("1") {
        return false;
    }
    let Some(authority) = parts.next() else {
        return false;
    };
    if !is_decimal_within(authority, (1u64 << 48) - 1) {
        return false;
    }
    let mut sub_count = 0usize;
    for sub in parts {
        if !is_decimal_within(sub, u64::from(u32::MAX)) {
            return false;
        }
        sub_count += 1;
    }
    (1..=15).contains(&sub_count)
}

fn is_decimal_within(s: &str, max: u64) -> bool {
    // 20 桁を超える数字列は u64 にも収まらない（parse の前に弾く）。
    !s.is_empty()
        && s.len() <= 20
        && s.bytes().all(|b| b.is_ascii_digit())
        && s.parse::<u64>().is_ok_and(|v| v <= max)
}

/// Operators に付ける ACE を SDDL で表す（純関数）: `(A;;CCLCRPWP;;;<sid>)`。
///
/// [`grant_service_operators`] は SDDL を解釈せず `EXPLICIT_ACCESS_W` で同じ
/// 内容の ACE を組み立てる（グループの SID は実行時に解決するため）。これは
/// 監査・案内（`sc sdshow` の期待値）・テスト用の表現。SID の形でない文字列は
/// [`ElevError::InvalidSid`]（SDDL の区切り `)`・`;` を埋め込ませない）。
pub fn operators_ace_sddl(sid: &str) -> Result<String, ElevError> {
    if !is_valid_sid_string(sid) {
        return Err(ElevError::InvalidSid(sid.to_string()));
    }
    Ok(format!("(A;;{OPERATORS_SERVICE_SDDL_RIGHTS};;;{sid})"))
}

/// この crate の操作の失敗（文言は製品名を含まない中立な形。製品は自分の
/// 文言へ写す - banto-hub は `service_elevated::map_elev_error`）。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ElevError {
    /// `LookupAccountNameW` が「名前に一致するアカウントが無い」以外の理由で
    /// 失敗した（無いときは `Ok(None)`／`Ok(false)`／[`ElevError::GroupNotFound`]）。
    #[error(
        "アカウント '{name}' の SID の解決に失敗しました (LookupAccountNameW, os error {os_error})"
    )]
    LookupAccountName { name: String, os_error: u32 },
    /// `CheckTokenMembership` の呼び出し自体が失敗した。
    #[error("プロセスのトークンのメンバーシップの確認に失敗しました (CheckTokenMembership, os error {os_error})")]
    CheckTokenMembership { os_error: u32 },
    /// SID の形でない文字列を渡された（[`is_valid_sid_string`]）。
    #[error("SID の形ではありません: '{0}'")]
    InvalidSid(String),
    /// `ConvertStringSidToSidW` の失敗（形は通ったが OS が受け付けなかった）。
    #[error("SID '{sid}' を変換できませんでした (ConvertStringSidToSidW, os error {os_error})")]
    ConvertSid { sid: String, os_error: u32 },
    /// `NetLocalGroupAdd` が「既にある」以外の理由で失敗した。
    #[error("ローカルグループ '{group}' の作成に失敗しました (NetLocalGroupAdd, status {status})")]
    CreateGroup { group: String, status: u32 },
    /// `NetLocalGroupAddMembers` が「既にメンバー」以外の理由で失敗した。
    /// `member` は渡された名前か SID。
    #[error("'{member}' をグループ '{group}' に追加できませんでした (NetLocalGroupAddMembers, status {status})")]
    AddMember {
        member: String,
        group: String,
        status: u32,
    },
    /// ACE を付ける対象のグループがまだ無い（先にグループを作る）。
    #[error("ローカルグループ '{0}' がまだありません")]
    GroupNotFound(String),
    /// `OpenSCManagerW` の失敗。
    #[error("Service Control Manager への接続に失敗しました (os error {0})")]
    OpenScm(u32),
    /// `OpenServiceW` の失敗。
    #[error("サービス '{service}' を開けませんでした (os error {os_error})")]
    OpenService { service: String, os_error: u32 },
    /// `QueryServiceObjectSecurity` の失敗。
    #[error("サービス '{service}' のセキュリティ記述子を読めませんでした (QueryServiceObjectSecurity, os error {os_error})")]
    QuerySecurity { service: String, os_error: u32 },
    /// `GetSecurityDescriptorDacl` の失敗。
    #[error("既存の DACL を取り出せませんでした (GetSecurityDescriptorDacl, os error {0})")]
    GetDacl(u32),
    /// `SetEntriesInAclW` の失敗（戻り値そのものが Win32 のエラー）。
    #[error("ACL への ACE の追加に失敗しました (SetEntriesInAclW, status {0})")]
    SetEntriesInAcl(u32),
    /// `InitializeSecurityDescriptor` の失敗。
    #[error(
        "セキュリティ記述子の初期化に失敗しました (InitializeSecurityDescriptor, os error {0})"
    )]
    InitializeSecurityDescriptor(u32),
    /// `SetSecurityDescriptorDacl` の失敗。
    #[error("DACL の設定に失敗しました (SetSecurityDescriptorDacl, os error {0})")]
    SetSecurityDescriptorDacl(u32),
    /// `SetServiceObjectSecurity` の失敗。
    #[error("サービス '{service}' にセキュリティ記述子を適用できませんでした (SetServiceObjectSecurity, os error {os_error})")]
    SetServiceSecurity { service: String, os_error: u32 },
}

/// 今のプロセスのトークンが、名前 `group_name` のローカルグループのメンバーか
/// （`LookupAccountNameW` → `CheckTokenMembership(NULL, …)`）。
///
/// グループがこのマシンに無いときは `Err` にせず `Ok(false)`（安全側 -
/// 「委任された日常の操作はできない、UAC へ回す」に使える）。Windows 以外は
/// 常に `Ok(false)`（ローカルグループという概念が無い）。
pub fn is_current_process_member(group_name: &str) -> Result<bool, ElevError> {
    #[cfg(windows)]
    {
        win::is_current_process_member(group_name)
    }
    #[cfg(not(windows))]
    {
        let _ = group_name;
        Ok(false)
    }
}

/// 今のプロセスのトークンが `BUILTIN\Administrators` のメンバーか
/// （[`is_current_process_member`] に [`ADMINISTRATORS_GROUP_NAME`] を渡すだけ）。
/// UAC で昇格していないプロセスでは、管理者の利用者でも `false` になりうる
/// （split token の deny-only）。Windows 以外は常に `Ok(false)`。
pub fn is_current_process_admin() -> Result<bool, ElevError> {
    is_current_process_member(ADMINISTRATORS_GROUP_NAME)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_mask_is_query_config_query_status_start_stop() {
        assert_eq!(OPERATORS_SERVICE_ACCESS_MASK, 0x35);
        assert_eq!(OPERATORS_SERVICE_SDDL_RIGHTS, "CCLCRPWP");
    }

    #[test]
    fn operators_ace_sddl_formats_the_allow_ace() {
        let sid = "S-1-5-21-1111111111-2222222222-3333333333-1001";
        assert_eq!(
            operators_ace_sddl(sid).unwrap(),
            "(A;;CCLCRPWP;;;S-1-5-21-1111111111-2222222222-3333333333-1001)"
        );
    }

    #[test]
    fn operators_ace_sddl_never_contains_dangerous_rights() {
        // DC（設定の変更）・SD（削除）・WD（DACL の変更）・WO（所有者の変更）。
        let ace = operators_ace_sddl("S-1-5-21-1-2-3-1001").unwrap();
        let rights = ace.split(';').nth(2).unwrap();
        for forbidden in ["DC", "SD", "WD", "WO", "GA", "GW", "FA"] {
            assert!(
                !rights.contains(forbidden),
                "ACE の権利に '{forbidden}' が入っている: {ace}"
            );
        }
    }

    #[test]
    fn operators_ace_sddl_rejects_strings_that_are_not_sids() {
        for bad in [
            "",
            "S-1-5-21-1-2)(A;;FA;;;WD",
            "S-1-5-21-1-2;",
            "BantoHub Operators",
            "WD",
        ] {
            assert_eq!(
                operators_ace_sddl(bad),
                Err(ElevError::InvalidSid(bad.to_string())),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn sid_shape_accepts_well_formed_sids() {
        for good in [
            "S-1-5-18",
            "S-1-5-32-544",
            "S-1-5-21-1111111111-2222222222-3333333333-1001",
            "S-1-12-1-1234567890-1234567890-1234567890-1234567890",
            "S-1-5-21-4294967295-1-2-3",
            "S-1-281474976710655-1",
            "S-1-5-1-2-3-4-5-6-7-8-9-10-11-12-13-14-15",
        ] {
            assert!(is_valid_sid_string(good), "{good}");
        }
    }

    #[test]
    fn sid_shape_rejects_malformed_strings() {
        for bad in [
            "",
            "S",
            "S-1",
            "S-1-5",
            "s-1-5-18",
            "S-2-5-18",
            "S-1--18",
            "S-1-5-",
            "S-1-5--18",
            "S-1-5-+18",
            "S-1-5- 18",
            "S-1-5-18 ",
            "S-1-5-4294967296",
            "S-1-281474976710656-1",
            "S-1-5-1-2-3-4-5-6-7-8-9-10-11-12-13-14-15-16",
            "S-1-5-99999999999999999999999",
            "S-1-5-18\"",
            "S-1-5-18;",
            "S-1-5-１８",
        ] {
            assert!(!is_valid_sid_string(bad), "{bad:?}");
        }
    }

    #[test]
    fn group_add_status_classification() {
        assert_eq!(classify_group_add_status(0), Some(GroupOutcome::Created));
        assert_eq!(classify_group_add_status(2223), Some(GroupOutcome::Existed));
        assert_eq!(classify_group_add_status(1379), Some(GroupOutcome::Existed));
        // ERROR_ACCESS_DENIED・NERR_UserInGroup（グループの作成では「既に」ではない）。
        assert_eq!(classify_group_add_status(5), None);
        assert_eq!(classify_group_add_status(2236), None);
    }

    #[test]
    fn member_add_status_classification() {
        assert_eq!(classify_member_add_status(0), Some(MemberOutcome::Added));
        assert_eq!(
            classify_member_add_status(2236),
            Some(MemberOutcome::AlreadyMember)
        );
        assert_eq!(
            classify_member_add_status(1378),
            Some(MemberOutcome::AlreadyMember)
        );
        // NERR_UserNotFound(2221)・ERROR_NO_SUCH_MEMBER(1387)・NERR_GroupExists。
        assert_eq!(classify_member_add_status(2221), None);
        assert_eq!(classify_member_add_status(1387), None);
        assert_eq!(classify_member_add_status(2223), None);
    }

    #[test]
    fn administrators_group_name_is_the_builtin_english_name() {
        assert_eq!(ADMINISTRATORS_GROUP_NAME, "Administrators");
    }

    /// Administrators は組み込みなので必ずあり、`Err` にならない（読むだけ。
    /// グループもトークンも変えない）。
    #[test]
    fn admin_membership_check_returns_ok() {
        let result = is_current_process_admin();
        assert!(result.is_ok(), "{result:?}");
    }

    /// 無いグループは `Ok(false)`（安全側）。読むだけ。
    #[test]
    fn missing_group_is_not_a_member() {
        assert_eq!(
            is_current_process_member("banto-service-elev test group that does not exist"),
            Ok(false)
        );
    }

    #[cfg(windows)]
    #[test]
    fn numeric_constants_match_windows_sys() {
        use windows_sys::Win32::Foundation::{ERROR_ALIAS_EXISTS as A, ERROR_MEMBER_IN_ALIAS as M};
        use windows_sys::Win32::NetworkManagement::NetManagement::{
            NERR_GroupExists, NERR_Success, NERR_UserInGroup,
        };
        use windows_sys::Win32::System::Services::{
            SERVICE_QUERY_CONFIG, SERVICE_QUERY_STATUS, SERVICE_START, SERVICE_STOP,
        };
        assert_eq!(NERR_SUCCESS, NERR_Success);
        assert_eq!(NERR_GROUP_EXISTS, NERR_GroupExists);
        assert_eq!(NERR_USER_IN_GROUP, NERR_UserInGroup);
        assert_eq!(ERROR_ALIAS_EXISTS, A);
        assert_eq!(ERROR_MEMBER_IN_ALIAS, M);
        assert_eq!(
            OPERATORS_SERVICE_ACCESS_MASK,
            SERVICE_QUERY_CONFIG | SERVICE_QUERY_STATUS | SERVICE_START | SERVICE_STOP
        );
    }
}
