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
//! - **名前 → SID の文字列**: `lookup_account_sid_string`（Windows だけ、#626 PR-A）は
//!   アカウント名を `LookupAccountNameW` で引き、文字列形式の SID（純関数
//!   [`sid_bytes_to_string`]）と、種類が `SidTypeUser` か（[`AccountSid::is_user`]）を
//!   返す。利用者のアカウントとして受け入れるかの判断は呼ぶ側（banto-hub の
//!   `grant-profile-acl` は `banto_secret_store` の `InstallUserSid::parse` と
//!   `check_user_account` で確かめる）。
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
    lookup_account_sid, lookup_account_sid_string,
};

/// `lookup_account_sid_string`（Windows だけ）の結果（#626 PR-A）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountSid {
    /// 文字列形式の SID（`S-1-5-21-…`。`ConvertSidToStringSidW` と同じ書式）。
    pub sid: String,
    /// `LookupAccountNameW` の種類が `SidTypeUser`（利用者のアカウント）か。グループ・
    /// well-known（Everyone など）・ドメイン・別名は `false`。
    pub is_user: bool,
}

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

/// [`grant_service_operators`] の結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceAclOutcome {
    /// このとき Operators の ACE を足した（Operators の ACE は今は無く、消えた ACE も無い）。
    Added,
    /// Operators の別の権利の ACE を置き換えた（消えた ACE が Operators のものだった）。
    Replaced,
    /// Operators の ACE は既にあったが、DACL の並びが正規の順序でなかったので整え直した
    /// （Operators の ACE は増えも減りもしていない）。
    Reordered,
    /// 既に同じ ACE があった（何も書いていない）。
    AlreadyPresent,
}

/// DACL の SDDL（`D:` の後の制御フラグ `P`・`AI`・`AR` などは読み飛ばす）から ACE の
/// 並びを取り出す（純関数）。条件つきの ACE の入れ子の括弧も 1 つの ACE として数える。
/// 形が読めなければ `None`。
fn dacl_aces(sddl: &str) -> Option<Vec<&str>> {
    let rest = sddl.strip_prefix("D:")?;
    // 制御フラグの部分には括弧が無い。ACE が無ければ空。
    let start = rest.find('(').unwrap_or(rest.len());
    let mut aces = Vec::new();
    let (mut depth, mut from) = (0usize, 0usize);
    for (i, c) in rest[start..].char_indices() {
        match c {
            '(' => {
                if depth == 0 {
                    from = start + i;
                }
                depth += 1;
            }
            ')' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    aces.push(&rest[from..start + i + 1]);
                }
            }
            // ACE の外に余計な文字（`S:` など）があれば読めないとみなす。
            _ if depth == 0 => return None,
            _ => {}
        }
    }
    (depth == 0).then_some(aces)
}

/// ACE の文字列（`(A;;CCLCRPWP;;;<SID>)`）の trustee（6 番目のフィールド）を取り出す
/// （純関数）。条件つきの ACE は後ろにまだフィールドが続くので、6 番目だけを見る。
fn ace_trustee(ace: &str) -> Option<&str> {
    ace.strip_prefix('(')?.split(';').nth(5).map(|t| {
        // 条件つきの ACE で `)` が付いたまま終わる形（フィールドが 6 つで閉じる）に備える。
        t.strip_suffix(')').unwrap_or(t)
    })
}

/// サービスの今の DACL（`current`）と、Operators の ACE を足して作り直した DACL
/// （`merged`）の SDDL を比べて、結果を決める（純関数。#628・#658）。`operators_sid` は
/// Operators の SID の文字列（`S-1-5-21-…`）。
///
/// - 比べるのは **ACE の並び**だけ。制御フラグ（`D:P`・`D:AI` など）は無視する。作り直した
///   記述子は制御フラグを持たないので、今の記述子に付いていると文字列全体では毎回食い
///   違い、「既にあっても毎回足した」と読めてしまうため。
/// - [`ServiceAclOutcome::AlreadyPresent`]（書き込みを省く）にするのは、ACE の並びが
///   **順序まで**同じときだけ。並びが違えば（順序だけの違いも）書く側に倒す。
/// - 今の ACE のうち作り直した側に無いもの（消えた ACE）が **Operators の SID のもの**を
///   含めば [`ServiceAclOutcome::Replaced`]。ほかの相手の ACE が並べ直された・まとめられた・
///   拒否の ACE が消えた、などでは `Replaced` にしない（Operators について嘘になるため）。
/// - Operators の ACE が今の DACL に既にあり（消えてもいない）、並びだけが違うなら
///   [`ServiceAclOutcome::Reordered`]（足していない）。無ければ [`ServiceAclOutcome::Added`]。
/// - どちらかの SDDL が読めなければ `Added`（これまでどおり書く）。
pub fn classify_dacl_change(current: &str, merged: &str, operators_sid: &str) -> ServiceAclOutcome {
    let (Some(current), Some(merged)) = (dacl_aces(current), dacl_aces(merged)) else {
        return ServiceAclOutcome::Added;
    };
    if current == merged {
        return ServiceAclOutcome::AlreadyPresent;
    }
    let is_operators =
        |ace: &str| ace_trustee(ace).is_some_and(|t| t.eq_ignore_ascii_case(operators_sid));
    // 今の ACE のうち、作り直した側に（多重集合として）残らなかったもの。
    let mut remaining = merged;
    let mut removed = Vec::new();
    for ace in &current {
        match remaining.iter().position(|m| m == ace) {
            Some(i) => {
                remaining.remove(i);
            }
            None => removed.push(*ace),
        }
    }
    if removed.iter().any(|ace| is_operators(ace)) {
        ServiceAclOutcome::Replaced
    } else if current.iter().any(|ace| is_operators(ace)) {
        ServiceAclOutcome::Reordered
    } else {
        ServiceAclOutcome::Added
    }
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

/// SID のバイト列（`SID` 構造体: 版・下位機関の数・6 バイトの識別子機関（ビッグエンディアン）・
/// 下位機関（各 4 バイト、リトルエンディアン））を文字列形式（`S-1-5-21-…`）にする（純関数、
/// #626 PR-A）。識別子機関が 32 ビットに収まらなければ `ConvertSidToStringSidW` と同じく
/// `0x` の 12 桁の 16 進。形が壊れていれば（短い・版が 1 でない・下位機関が 15 を超える）
/// `None`。後ろの余ったバイトは見ない（`LookupAccountNameW` のバッファは大きめでありうる）。
pub fn sid_bytes_to_string(bytes: &[u8]) -> Option<String> {
    let (&revision, rest) = bytes.split_first()?;
    let (&count, rest) = rest.split_first()?;
    if revision != 1 || count > 15 {
        return None;
    }
    let authority = rest.get(..6)?;
    let subs = rest.get(6..6 + 4 * usize::from(count))?;
    let authority = authority
        .iter()
        .fold(0u64, |acc, &b| (acc << 8) | u64::from(b));
    let mut text = if authority >> 32 == 0 {
        format!("S-1-{authority}")
    } else {
        format!("S-1-0x{authority:012X}")
    };
    for chunk in subs.as_chunks::<4>().0 {
        text.push_str(&format!("-{}", u32::from_le_bytes(*chunk)));
    }
    Some(text)
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
    fn classify_dacl_change_decides_by_the_ace_sequence_only() {
        use super::{classify_dacl_change as classify, ServiceAclOutcome as O};
        const SID: &str = "S-1-5-21-1-2-3-1001";
        let c = |current: &str, merged: &str| classify(current, merged, SID);
        let base = "(A;;CCLCSWRPWPDTLOCRRC;;;SY)(A;;CCDCLCSWRPWPDTLOCRSDRCWDWO;;;BA)";
        let ours = "(A;;CCLCRPWP;;;S-1-5-21-1-2-3-1001)";
        let theirs = "(A;;CCDCLCRPWP;;;S-1-5-21-1-2-3-1001)";
        assert_eq!(
            c(&format!("D:{base}{ours}"), &format!("D:{base}{ours}")),
            O::AlreadyPresent
        );
        assert_eq!(
            c(&format!("D:{base}"), &format!("D:{base}{ours}")),
            O::Added
        );
        assert_eq!(
            c(&format!("D:{base}{theirs}"), &format!("D:{base}{ours}")),
            O::Replaced
        );
        // 制御フラグは無視する（今にだけ付いていても既にあるとみなす）。
        for flags in ["P", "AI", "PAI", "ARPAI"] {
            assert_eq!(
                c(
                    &format!("D:{flags}{base}{ours}"),
                    &format!("D:{base}{ours}")
                ),
                O::AlreadyPresent,
                "{flags}"
            );
            assert_eq!(
                c(&format!("D:{flags}{base}"), &format!("D:{base}{ours}")),
                O::Added,
                "{flags}"
            );
        }
        // Operators の ACE は既にあり、並びだけが違う → 足していない（Reordered）。
        assert_eq!(
            c(&format!("D:{ours}{base}"), &format!("D:{base}{ours}")),
            O::Reordered
        );
        // 読めなければ書く側に倒す。
        assert_eq!(c("S:(AU;SA;FA;;;WD)", &format!("D:{base}{ours}")), O::Added);
        assert_eq!(
            c(&format!("D:{base}("), &format!("D:{base}{ours}")),
            O::Added
        );
        // 条件つきの ACE の入れ子の括弧は 1 つの ACE。
        let cond = "(XA;;FX;;;S-1-1-0;(@User.Level>=3))";
        assert_eq!(
            c(&format!("D:{cond}{ours}"), &format!("D:{cond}{ours}")),
            O::AlreadyPresent
        );
    }

    /// #658: `Replaced` は、消えた ACE が Operators のものだったときだけ。
    #[test]
    fn classify_dacl_change_replaced_only_when_an_operators_ace_vanished() {
        use super::{classify_dacl_change as classify, ServiceAclOutcome as O};
        const SID: &str = "S-1-5-21-1-2-3-1001";
        let ours = "(A;;CCLCRPWP;;;S-1-5-21-1-2-3-1001)";
        let sy = "(A;;CCLCSWRPWPDTLOCRRC;;;SY)";
        let ba = "(A;;CCDCLCSWRPWPDTLOCRSDRCWDWO;;;BA)";
        let other = "(A;;CCLCRPWP;;;S-1-5-21-1-2-3-2002)";
        let other_wide = "(A;;CCDCLCRPWP;;;S-1-5-21-1-2-3-2002)";
        let deny = "(D;;CCLC;;;S-1-5-21-1-2-3-3003)";
        // Operators の別の権利が消えた（大文字小文字の違う SID でも同じ相手）→ Replaced。
        assert_eq!(
            classify(
                "D:(A;;CCDCLCRPWP;;;S-1-5-21-1-2-3-1001)",
                &format!("D:{ours}"),
                SID
            ),
            O::Replaced
        );
        assert_eq!(
            classify(
                &format!("D:{sy}(A;;CCDCLCRPWP;;;s-1-5-21-1-2-3-1001)"),
                &format!("D:{sy}{ours}"),
                "S-1-5-21-1-2-3-1001"
            ),
            O::Replaced
        );
        // ほかの相手の ACE がまとめられた・置き換わった（Operators の ACE は消えていない）→ Replaced にしない。
        assert_eq!(
            classify(
                &format!("D:{sy}{other_wide}"),
                &format!("D:{sy}{other}{ours}"),
                SID
            ),
            O::Added
        );
        // 拒否の ACE が消えた → Replaced にしない（Operators の ACE が無ければ Added）。
        assert_eq!(
            classify(
                &format!("D:{deny}{sy}{ba}"),
                &format!("D:{sy}{ba}{ours}"),
                SID
            ),
            O::Added
        );
        // Operators の ACE が既にあって、ほかの ACE が並べ直されただけ → Reordered。
        assert_eq!(
            classify(
                &format!("D:{ba}{sy}{ours}"),
                &format!("D:{sy}{ba}{ours}"),
                SID
            ),
            O::Reordered
        );
    }

    #[test]
    fn ace_trustee_is_the_sixth_field() {
        assert_eq!(
            ace_trustee("(A;;CCLCRPWP;;;S-1-5-21-1)"),
            Some("S-1-5-21-1")
        );
        assert_eq!(
            ace_trustee("(XA;;FX;;;S-1-1-0;(@User.Level>=3))"),
            Some("S-1-1-0")
        );
        assert_eq!(ace_trustee("(A;;CCLC)"), None);
        assert_eq!(ace_trustee("A;;;;;SY"), None);
    }

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
    fn sid_bytes_format_like_convert_sid_to_string_sid() {
        // S-1-5-18（SYSTEM）。
        assert_eq!(
            sid_bytes_to_string(&[1, 1, 0, 0, 0, 0, 0, 5, 18, 0, 0, 0]).as_deref(),
            Some("S-1-5-18")
        );
        // S-1-5-32-544（Administrators）。後ろの余りは見ない。
        assert_eq!(
            sid_bytes_to_string(&[1, 2, 0, 0, 0, 0, 0, 5, 32, 0, 0, 0, 0x20, 2, 0, 0, 9, 9])
                .as_deref(),
            Some("S-1-5-32-544")
        );
        // S-1-5-21-1111111111-2222222222-3333333333-1001。
        let mut bytes = vec![1u8, 5, 0, 0, 0, 0, 0, 5];
        for sub in [21u32, 1_111_111_111, 2_222_222_222, 3_333_333_333, 1001] {
            bytes.extend_from_slice(&sub.to_le_bytes());
        }
        assert_eq!(
            sid_bytes_to_string(&bytes).as_deref(),
            Some("S-1-5-21-1111111111-2222222222-3333333333-1001")
        );
        // S-1-1-0（Everyone）・下位機関が 0 個。
        assert_eq!(
            sid_bytes_to_string(&[1, 1, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0]).as_deref(),
            Some("S-1-1-0")
        );
        assert_eq!(
            sid_bytes_to_string(&[1, 0, 0, 0, 0, 0, 0, 16]).as_deref(),
            Some("S-1-16")
        );
        // 32 ビットを超える識別子機関は 16 進。
        assert_eq!(
            sid_bytes_to_string(&[1, 0, 0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC]).as_deref(),
            Some("S-1-0x123456789ABC")
        );
        // 壊れた形。
        for bad in [
            &[][..],
            &[1][..],
            &[2, 1, 0, 0, 0, 0, 0, 5, 18, 0, 0, 0][..],
            &[1, 16, 0, 0, 0, 0, 0, 5][..],
            &[1, 1, 0, 0, 0, 0, 0, 5, 18, 0, 0][..],
            &[1, 0, 0, 0, 0][..],
        ] {
            assert_eq!(sid_bytes_to_string(bad), None, "{bad:?}");
        }
    }

    /// 読むだけ: 名前 → SID の文字列と種類。組み込みのグループ・well-known は利用者では
    /// なく、無い名前は `None`（どれも本物のアカウントを変えない）。
    #[cfg(windows)]
    #[test]
    fn lookup_account_sid_string_resolves_names_and_kinds() {
        assert_eq!(
            lookup_account_sid_string("Administrators"),
            Ok(Some(AccountSid {
                sid: "S-1-5-32-544".to_string(),
                is_user: false,
            }))
        );
        let everyone = lookup_account_sid_string("Everyone").unwrap().unwrap();
        assert_eq!(everyone.sid, "S-1-1-0");
        assert!(!everyone.is_user);
        assert_eq!(
            lookup_account_sid_string("banto-service-elev test account that does not exist"),
            Ok(None)
        );
        // 今の利用者（USERDOMAIN\USERNAME）は利用者のアカウント。
        if let (Ok(domain), Ok(user)) = (std::env::var("USERDOMAIN"), std::env::var("USERNAME")) {
            let me = lookup_account_sid_string(&format!(r"{domain}\{user}"))
                .unwrap()
                .expect("今の利用者");
            assert!(me.is_user, "{me:?}");
            assert!(is_valid_sid_string(&me.sid), "{me:?}");
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
