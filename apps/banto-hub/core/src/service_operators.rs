//! T17-2 スライス1（docs/banto-hub-t17-design.md §3「T17-2」・P3、
//! docs/banto-hub-desktop-plan.md §8.3）: ローカルグループ
//! `BantoHub Operators` へのメンバーシップ判定。
//!
//! `host_switch`（T17-3、`docs/banto-hub-t17-design.md` §9「T17-2 スタブ」）
//! は `HostSwitchConfig::can_operate_service: bool` を UAC helper が来るまでの
//! 仮値（常に`false`）で受け取っていた - このモジュールはその判定を実際の
//! Windows API 呼び出しで置き換えるための関数を提供する。
//! `HostSwitchEngine::set_can_operate_service` へ結果を渡すのは呼び出し側
//! （T16-2 の native shell 配線）の責務であり、このモジュール自体は
//! `crate::host_switch` を参照・変更しない。
//!
//! ## このスライスで行ったこと
//!
//! - [`is_current_process_operator`]（`#[cfg(windows)]`）: 現在のプロセスの
//!   トークンが [`OPERATORS_GROUP_NAME`] ローカルグループのメンバーかどうかを
//!   判定する。
//!   1. `LookupAccountNameW` でグループ名からローカル SID を解決する
//!      （ローカルマシンのアカウント DB を引くだけなので、グループの
//!      表示名がローカライズされていても同じ英語名で作成しておけば動く -
//!      `LookupAccountNameW` は指定した文字列そのものを名前として検索する）。
//!   2. 解決できた SID を `CheckTokenMembership` に渡し、呼び出しスレッド
//!      （または呼び出しプロセスの主トークン、`hToken=NULL`時の既定挙動）に
//!      その SID が含まれるかを確認する。
//!
//! ## slice 2 での引き継ぎ（実装済み）
//!
//! - `BantoHub Operators` グループ**自体の作成**・メンバー追加
//!   （`NetLocalGroupAdd`/`NetLocalGroupAddMembers`）、対象サービスの
//!   Security Descriptor への ACE 付与（`SetServiceObjectSecurity`）、
//!   UAC helper 本体（`banto-hub-elev.exe`）は [`crate::service_elevated`]
//!   （slice 2）が実装した。このモジュールは変更していない -
//!   [`lookup_account_sid`][windows_impl::lookup_account_sid]（本来
//!   `lookup_group_sid`という名前だった）を`pub(crate)`に広げ、
//!   `service_elevated`がユーザー名/グループ名 SID 解決の両方に再利用できる
//!   ようにしただけ。
//!
//! グループが未作成の環境（`service_elevated::setup_operators`実行前の
//! 全環境を含む）では、[`is_current_process_operator`]は Windows API
//! エラーにはせず、安全側で `Ok(false)` を返す（関数のドキュメント参照）。
//!
//! ## 既知の未検証事項（要 Windows 実機、`docs/banto-hub-t17-design.md` §5）
//!
//! `CheckTokenMembership` は **Administrators のような管理者グループ**に
//! ついては、UAC 昇格前の split token（フィルタ済み標準トークン）で
//! deny-only エントリを偽の非メンバー判定として返すことが知られている。
//! `BantoHub Operators` は管理者グループではない一般のローカルグループ
//! なので、UAC のフィルタ処理では通常剥奪されない想定だが（フィルタは
//! 管理者相当のグループを対象とする）、この前提は Windows 実機での
//! 再確認が済んでいない。slice 2（UAC helper 実装・受け入れ時）で
//! 実機確認すること。

use thiserror::Error;

/// desktop-plan §8.3 / 本設計 P3 で決定したローカルグループ名。
/// グループ自体の作成は slice 2（UAC helper）が行う - このスライスでは
/// 名前の参照のみ。
pub const OPERATORS_GROUP_NAME: &str = "BantoHub Operators";

/// [`is_current_process_operator`] の失敗モード。
///
/// グループが未作成の場合は `Err` にせず `Ok(false)` を返す設計にしている
/// （モジュール doc 参照）ため、ここに列挙するのは Win32 API 自体が
/// 想定外の失敗をした場合のみ。
#[derive(Debug, Error)]
pub enum OperatorsError {
    /// `LookupAccountNameW` がグループ未検出以外の理由で失敗した。
    #[error(
        "banto-hub: ローカルグループ '{group}' の SID 解決に失敗しました \
         (LookupAccountNameW, os error {os_error})"
    )]
    LookupAccountName { group: String, os_error: u32 },
    /// `CheckTokenMembership` の呼び出し自体が失敗した
    /// （グループが見つからないケースはこの変種を使わない）。
    #[error(
        "banto-hub: プロセストークンのメンバーシップ確認に失敗しました \
         (CheckTokenMembership, os error {os_error})"
    )]
    CheckTokenMembership { os_error: u32 },
}

/// 現在のプロセスが [`OPERATORS_GROUP_NAME`] ローカルグループのメンバーかを
/// 判定する（非 Windows 版）。
///
/// banto-hub は Windows 専用製品だが、ワークスペース全体は非 Windows でも
/// `cargo check --workspace` が通る必要がある（`profile_lock.rs` 等と同じ
/// 事情）。非 Windows には Windows ローカルグループという概念が無いため、
/// 常に安全側の `Ok(false)`（操作不可）を返す。
#[cfg(not(windows))]
pub fn is_current_process_operator() -> Result<bool, OperatorsError> {
    Ok(false)
}

/// 現在のプロセスが [`OPERATORS_GROUP_NAME`] ローカルグループのメンバーかを
/// 判定する（Windows 版）。
///
/// グループがこのマシンにまだ作成されていない場合（このスライス時点の
/// 全環境を含む - slice 2 で UAC helper がインストール時に作成する）は
/// エラーにせず `Ok(false)` を返す - 「Operators 委任の日常操作はできないが
/// UAC 経由の管理者操作は妨げない」という安全側の既定に合わせるため
/// （呼び出し元は `false` を「委任された日常操作は不可、UAC へ回す」判断に
/// そのまま使える）。
#[cfg(windows)]
pub fn is_current_process_operator() -> Result<bool, OperatorsError> {
    windows_impl::is_current_process_operator()
}

// T17-2 スライス2（`service_elevated.rs`）が`lookup_account_sid`を
// `BantoHub Operators`グループだけでなく指定ユーザー名の SID 解決にも
// 再利用するため`pub(crate)`にした - スライス1時点では`is_current_process_
// operator`専用のプライベートヘルパーだったが、`LookupAccountNameW`自体は
// ユーザー・ローカルグループのどちらの名前解決にも使える汎用 API なので
// （関数doc参照）、実装を複製せずここを再利用する。
#[cfg(windows)]
pub(crate) mod windows_impl {
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_NONE_MAPPED};
    use windows_sys::Win32::Security::{
        CheckTokenMembership, LookupAccountNameW, PSID, SID_NAME_USE,
    };

    use super::{OperatorsError, OPERATORS_GROUP_NAME};

    pub(super) fn is_current_process_operator() -> Result<bool, OperatorsError> {
        let mut sid = match lookup_account_sid(OPERATORS_GROUP_NAME)? {
            Some(sid) => sid,
            // グループ未作成 - モジュール doc・関数 doc の「安全側で false」節。
            None => return Ok(false),
        };

        let mut is_member: i32 = 0;
        // SAFETY: `sid`はこのスコープで生存しており、`lookup_group_sid`が
        // `LookupAccountNameW`から書き込ませたバイト列そのもの（有効な
        // `SID`構造体のレイアウト）。`hToken=NULL`は呼び出しスレッドの
        // 偽装トークン、無ければプロセス主トークンを複製して使う既定挙動
        // （MSDN `CheckTokenMembership` 参照）。
        let ok = unsafe {
            CheckTokenMembership(
                std::ptr::null_mut(),
                sid.as_mut_ptr() as PSID,
                &mut is_member,
            )
        };
        if ok == 0 {
            let os_error = unsafe { GetLastError() };
            return Err(OperatorsError::CheckTokenMembership { os_error });
        }
        Ok(is_member != 0)
    }

    /// アカウント名（ユーザー名またはローカルグループ名）からこのマシン
    /// 上の SID を解決する。
    ///
    /// `LookupAccountNameW`はユーザーだけでなくローカルグループ（エイリアス、
    /// `SidTypeAlias`）の名前解決にも使える汎用 API - `service_elevated.rs`
    /// （T17-2 スライス2）が`BantoHub Operators`グループと指定ユーザー名の
    /// 両方の SID 解決にこの関数を再利用する。`ERROR_NONE_MAPPED`
    /// （名前がどのアカウントにも一致しない = 未作成/未存在）は`Ok(None)`
    /// として区別し、それ以外の失敗のみ`Err`にする - 呼び出し元
    /// （[`is_current_process_operator`]はこれを「未作成なら false」に、
    /// `service_elevated`は「アカウント未検出」エラーにそれぞれ変換する）。
    pub(crate) fn lookup_account_sid(name: &str) -> Result<Option<Vec<u8>>, OperatorsError> {
        let wide_name: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();

        let mut sid_size: u32 = 0;
        let mut domain_size: u32 = 0;
        let mut sid_name_use: SID_NAME_USE = 0;

        // 1回目: サイズ問い合わせ。バッファ長 0 で呼び、必要な `sid_size`/
        // `domain_size`を`ERROR_INSUFFICIENT_BUFFER`と共に受け取る
        // `LookupAccountNameW`の標準的な2段呼び出しパターン
        // （戻り値の成否そのものは見ず、`GetLastError`で分岐する）。
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
        let first_error = unsafe { GetLastError() };
        if first_error == ERROR_NONE_MAPPED {
            return Ok(None);
        }
        if sid_size == 0 {
            return Err(OperatorsError::LookupAccountName {
                group: name.to_string(),
                os_error: first_error,
            });
        }

        let mut sid_buf = vec![0u8; sid_size as usize];
        let mut domain_buf = vec![0u16; domain_size.max(1) as usize];

        // 2回目: 実際の SID・ドメイン名バッファを渡して解決する。
        // SAFETY: `sid_buf`/`domain_buf`は直前に確保した1回目の呼び出しが
        // 報告したサイズちょうどのバッファで、`sid_size`/`domain_size`へ
        // 実際の書き込みサイズが上書きされる（呼び出し規約どおり）。
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
            let os_error = unsafe { GetLastError() };
            if os_error == ERROR_NONE_MAPPED {
                return Ok(None);
            }
            return Err(OperatorsError::LookupAccountName {
                group: name.to_string(),
                os_error,
            });
        }

        Ok(Some(sid_buf))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operators_group_name_matches_design_decision() {
        // docs/banto-hub-desktop-plan.md §8.3 / docs/banto-hub-t17-design.md
        // P3 で確定した表記そのもの - 表記揺れがあるとインストーラ（slice 2）
        // が作るグループ名と実行時の判定がずれる。
        assert_eq!(OPERATORS_GROUP_NAME, "BantoHub Operators");
    }

    /// このスライス時点では `BantoHub Operators` グループを作成する経路が
    /// 無い（モジュール doc「行っていないこと」節）ため、CI・開発機の
    /// いずれでもグループは未作成のはず - 安全側の `Ok(false)` を返すことを
    /// 固定する。Windows 実機で slice 2 導入前に実行しても green になる。
    #[cfg(windows)]
    #[test]
    fn returns_false_when_group_does_not_exist_yet() {
        let result = is_current_process_operator();
        assert!(
            matches!(result, Ok(false)),
            "expected Ok(false), got {result:?}"
        );
    }

    /// 実際に `BantoHub Operators` を作成しメンバーを追加した Windows 実機
    /// でのみ意味のある確認（CI や開発機では未作成のため `#[ignore]`）。
    /// slice 2（UAC helper でグループ作成）導入後、手動でメンバー登録した
    /// 環境で `cargo test -p banto-hub-core --lib service_operators -- --ignored`
    /// を実行して確認する。
    #[cfg(windows)]
    #[test]
    #[ignore = "BantoHub Operators への実メンバーシップが必要 - Windows 実機で手動実行"]
    fn membership_check_compiles_and_runs_on_real_machine() {
        let result = is_current_process_operator();
        assert!(result.is_ok(), "expected Ok(_), got {result:?}");
    }
}
