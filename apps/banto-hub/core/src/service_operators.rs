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
//!
//! ## T16-2 第二スライスで追加した [`is_current_process_admin`]
//!
//! desktop-plan §8.3「通常画面のサービス操作は Hub Admin と Windows の
//! 専用ローカルグループ `BantoHub Operators`（**または Windows 管理者**）
//! を双方要求する」・§16.3 の意図どおり、`BantoHub Operators`
//! メンバーシップだけでなく Windows ローカル Administrators グループの
//! メンバーシップでもサービス操作系トレイ項目を表示できるようにする
//! （T16-2 第一スライスの既知の gap - `docs/banto-hub-t16-design.md` §5
//! 「サービス操作系トレイ項目が Operators のみに限定されており、
//! ローカル管理者が誤って隠される」）。
//!
//! 実装は[`is_current_process_operator`]と全く同じ`CheckTokenMembership`
//! 経路を、グループ名だけ`"Administrators"`（`BUILTIN\Administrators`、
//! ローカライズに依存しない英語名で`LookupAccountNameW`が解決できる -
//! [`OPERATORS_GROUP_NAME`]と同じ理由）に変えて再利用するだけ - 新規
//! Win32 API 面は増やさない。上記「既知の未検証事項」の split-token
//! 限界がそのまま当てはまる点も同じ: **非昇格**プロセスでは通常の
//! Administrators メンバーであっても`false`になりうる（UAC のフィルタが
//! 管理者グループを対象とするため）。そのため、これは「常に管理者を
//! 正しく検出できる」機能ではなく、「シェルを明示的に管理者として
//! 実行した（昇格した）場合に `BantoHub Operators` 未登録でも操作可能に
//! する」効果に限られる、低リスクな追加チェックとして扱う - 昇格していない
//! 通常の管理者アカウントは従来どおり`BantoHub Operators`への登録が必要
//! （desktop-plan 同節「管理者は常に UAC 経路へ」の方針とも矛盾しない）。
//!
//! ## #392 B3-3a: Win32 の部分は共有 crate へ
//!
//! `LookupAccountNameW`・`CheckTokenMembership`の呼び出しは、ChronoGazer の
//! elev（B3-3b）と共有するため共有 crate `banto-service-elev`
//! （[`banto_service_elev::is_current_process_member`]・
//! [`banto_service_elev::is_current_process_admin`]・
//! `banto_service_elev::lookup_account_sid`）へ移した。呼び出しの順序・
//! 「未作成なら`Ok(false)`」・非 Windows で`Ok(false)`は移す前のまま。
//! このモジュールに残したのは banto-hub の知識（グループ名
//! [`OPERATORS_GROUP_NAME`]と説明文[`OPERATORS_GROUP`]）と、共有 crate の
//! 中立な文言のエラーを従来の文言の[`OperatorsError`]へ写すこと
//! （`OperatorsError::from_elev`）だけ。

use thiserror::Error;

/// desktop-plan §8.3 / 本設計 P3 で決定したローカルグループ名。
/// グループ自体の作成は slice 2（UAC helper）が行う - このスライスでは
/// 名前の参照のみ。
pub const OPERATORS_GROUP_NAME: &str = "BantoHub Operators";

/// Windows 組み込みローカルグループ`BUILTIN\Administrators`の参照名
/// （[`is_current_process_admin`]、T16-2 第二スライス）。`LookupAccountNameW`
/// は英語名でロケールに関わらず解決できる（[`OPERATORS_GROUP_NAME`]と
/// 同じ理由）。
pub const ADMINISTRATORS_GROUP_NAME: &str = banto_service_elev::ADMINISTRATORS_GROUP_NAME;

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
    /// 共有 crate（`banto-service-elev`）のその他のエラー。このモジュールの
    /// 経路（メンバーシップの判定・SID の解決）からは返らない（#392 B3-3a で
    /// 写しを網羅にするための変種）。
    #[error(transparent)]
    Elev(banto_service_elev::ElevError),
}

/// `BantoHub Operators`グループの名前と説明文（`setup-operators`が
/// [`banto_service_elev::ensure_local_group`]に渡す。説明文は #392 B3-3a で
/// `service_elevated.rs`から移した - 文言は変えていない）。
pub const OPERATORS_GROUP: banto_service_elev::OperatorsGroup = banto_service_elev::OperatorsGroup {
    name: OPERATORS_GROUP_NAME,
    comment: "banto-hub のサービス操作（開始/停止/状態照会）を委任されたユーザーのグループ（banto-hub-elev.exe が自動作成）",
};

/// 現在のプロセスが [`OPERATORS_GROUP_NAME`] ローカルグループのメンバーかを
/// 判定する。
///
/// グループがこのマシンにまだ作成されていない場合はエラーにせず
/// `Ok(false)`を返す - 「Operators 委任の日常操作はできないが UAC 経由の
/// 管理者操作は妨げない」という安全側の既定に合わせるため（呼び出し元は
/// `false`を「委任された日常操作は不可、UAC へ回す」判断にそのまま使える）。
///
/// 非 Windows（banto-hub は Windows 専用製品だが、ワークスペース全体は
/// 非 Windows でも`cargo check --workspace`が通る必要がある）では、Windows
/// ローカルグループという概念が無いため常に安全側の`Ok(false)`（操作不可）。
///
/// #392 B3-3a で Win32 の呼び出し（`LookupAccountNameW`→
/// `CheckTokenMembership`）は[`banto_service_elev::is_current_process_member`]
/// へ移した（挙動は不変）。ここはエラーを従来の文言へ写すだけ。
pub fn is_current_process_operator() -> Result<bool, OperatorsError> {
    banto_service_elev::is_current_process_member(OPERATORS_GROUP_NAME)
        .map_err(OperatorsError::from_elev)
}

/// 現在のプロセスが Windows ローカル Administrators グループのメンバーかを
/// 判定する（T16-2 第二スライス - モジュール doc「T16-2 第二スライスで追加した
/// is_current_process_admin」節参照）。
///
/// [`is_current_process_operator`]と同じ`CheckTokenMembership`経路を
/// グループ名だけ変えて再利用する（[`banto_service_elev::is_current_process_admin`]
/// は[`ADMINISTRATORS_GROUP_NAME`]と同じ英語名を引く）- Administrators は
/// 未作成になることがない組み込みグループだが、万一見つからなくても同じ
/// 「安全側で`false`」規約に従う。非 Windows は常に`Ok(false)`。
pub fn is_current_process_admin() -> Result<bool, OperatorsError> {
    banto_service_elev::is_current_process_admin().map_err(OperatorsError::from_elev)
}

impl OperatorsError {
    /// 共有 crate のエラー（中立な文言）を従来の banto-hub の文言の変種へ写す
    /// （#392 B3-3a）。メンバーシップの判定・SID の解決が返しうるのは
    /// `LookupAccountName`と`CheckTokenMembership`だけで、残りは
    /// [`OperatorsError::Elev`]（このモジュールの経路からは返らない）。
    pub(crate) fn from_elev(err: banto_service_elev::ElevError) -> Self {
        use banto_service_elev::ElevError;
        match err {
            ElevError::LookupAccountName { name, os_error } => OperatorsError::LookupAccountName {
                group: name,
                os_error,
            },
            ElevError::CheckTokenMembership { os_error } => {
                OperatorsError::CheckTokenMembership { os_error }
            }
            other => OperatorsError::Elev(other),
        }
    }
}

// T17-2 スライス2（`service_elevated.rs`）・profile ACL（`profile_acl.rs`）が
// アカウント名からの SID の解決に使う。#392 B3-3a で本体は
// [`banto_service_elev::lookup_account_sid`]へ移し、ここはエラーを従来の
// 文言へ写す薄い包みだけにした。
#[cfg(windows)]
pub(crate) mod windows_impl {
    use super::OperatorsError;

    /// アカウント名（ユーザー名またはローカルグループ名）からこのマシン
    /// 上の SID を解決する。名前がどのアカウントにも一致しない（未作成/
    /// 未存在）ときは`Ok(None)`、それ以外の失敗だけ`Err`。
    pub(crate) fn lookup_account_sid(name: &str) -> Result<Option<Vec<u8>>, OperatorsError> {
        banto_service_elev::lookup_account_sid(name).map_err(OperatorsError::from_elev)
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

    #[test]
    fn administrators_group_name_is_the_builtin_english_name() {
        // T16-2 第二スライス: `LookupAccountNameW`がロケールに関わらず
        // 解決できる英語表記（モジュール doc「T16-2 第二スライスで追加した
        // is_current_process_admin」節参照）。
        assert_eq!(ADMINISTRATORS_GROUP_NAME, "Administrators");
    }

    /// Administrators は OS 組み込みグループなので常に存在し、
    /// `is_current_process_admin`は`Err`にならないことを固定する
    /// （`is_current_process_operator`版と同じ契約）。
    #[cfg(windows)]
    #[test]
    fn admin_membership_check_returns_ok_without_api_error() {
        let result = is_current_process_admin();
        assert!(result.is_ok(), "expected Ok(_), got {result:?}");
    }

    /// グループ未作成なら`Ok(false)`、作成済みなら`Ok(true|false)`
    /// （メンバーかどうか）のいずれかで、`Err`にはしないことを固定する。
    /// 以前は「グループは未作成のはず」と`Ok(false)`を要求していたが、
    /// T17-2 実機検証後の開発機には`BantoHub Operators`が残ることがあり、
    /// その前提は成立しなくなった（実装済みの「未作成なら false」契約自体は
    /// `lookup_account_sid`の`Ok(None)`経路で別途カバーされる）。
    #[cfg(windows)]
    #[test]
    fn membership_check_returns_ok_without_api_error() {
        let result = is_current_process_operator();
        assert!(result.is_ok(), "expected Ok(_), got {result:?}");
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
