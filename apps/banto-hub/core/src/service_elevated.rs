//! T17-2 スライス2（docs/banto-hub-t17-design.md §3「T17-2」・P3、
//! docs/banto-hub-desktop-plan.md §8.3）: UAC 昇格ヘルパー
//! `banto-hub-elev.exe`（`src/bin/banto-hub-elev.rs`）が呼ぶ実装本体。
//!
//! [`crate::service_operators`]（slice 1）はローカルグループ
//! `BantoHub Operators`への**メンバーシップ判定**だけを行い、グループ自体の
//! 作成・サービス ACL 付与は「次スライスへ引き継ぐ」としていた。このモジュール
//! がその引き継ぎ分を実装する。
//!
//! ## 固定アクション（[`ElevatedAction`]）
//!
//! `banto-hub-elev.exe <action> [args...]`が受け付けるのは次の7種類のみ -
//! フリーフォームのコマンド文字列は一切受け付けない（UAC 昇格した管理者
//! プロセスに任意コマンドを渡させないためのセキュリティ境界、実装指示の
//! 「固定アクション」要求そのもの）。
//!
//! | action              | 処理内容                                                              |
//! |----------------------|------------------------------------------------------------------------|
//! | `setup-operators`    | ローカルグループ`BantoHub Operators`を（無ければ）作成し、指定ユーザー（省略時は現在の対話ユーザー）をメンバーに追加する。冪等 |
//! | `grant-service-acl`  | `BantoHub`サービスの DACL に`BantoHub Operators`への限定 ACE を追加する（下記「SDDL」節） |
//! | `grant-profile-acl`  | profile ディレクトリ（`[username] [profile-id]`、両方省略時は現在の対話ユーザー・既定 profile）へ owner 用 DACL を付与する（[`crate::profile_acl`]、下記「profile ACL」節） |
//! | `service-install`    | [`crate::service_install::install`]（`banto-hub.exe`本体を対象）→`setup-operators`→`grant-service-acl`→`grant-profile-acl`（既定ユーザー・既定 profile）の順で実行 |
//! | `service-uninstall`  | [`crate::service_install::uninstall`]をそのまま呼ぶ                    |
//! | `autostart-enable`   | `WindowsServiceManager::set_auto_start(true)`                          |
//! | `autostart-disable`  | `WindowsServiceManager::set_auto_start(false)`                         |
//!
//! ## profile ACL（`grant-profile-acl`が付与する権限）
//!
//! desktop-plan §11「データプロファイルと移行」の権限方針そのもの -
//! ACL 変更自体が UAC を要求するため、この固定アクションを新設した
//! （P3 に「グループ変更、profile owner 追加、ACL 変更は UAC を必要と
//! する」と明記済み）。実際の DACL 組み立ては[`crate::profile_acl`]が行う。
//! このモジュールが担うのは「`[username] [profile-id]`引数の省略時
//! デフォルト解決（現在の対話ユーザー／
//! [`crate::profile_paths::DEFAULT_PROFILE_ID`]）→ root 解決
//! （[`crate::profile_paths::resolve_hub_root`]、env
//! `BANTO_HUB_ROOT`/`ProgramData`/`XDG_DATA_HOME`/`HOME`）→
//! [`crate::profile_paths::resolve_profile_paths`]で`profile_dir`を得る」
//! までの配線だけである（[`windows_impl::grant_profile_acl`]参照）。
//!
//! ## SDDL（`grant-service-acl`が付与する権限）
//!
//! `BantoHub`サービスオブジェクトの**既存 DACL に追記する形**（丸ごと置換
//! しない - `QueryServiceObjectSecurity`→`GetSecurityDescriptorDacl`→
//! `SetEntriesInAclW`でマージ→`SetServiceObjectSecurity`、
//! [`windows_impl::grant_service_acl`]参照）で、`BantoHub Operators`グループの
//! SID に対して次の ACE のみを追加する:
//!
//! ```text
//! (A;;CCLCRPWP;;;<BantoHub-Operators-SID>)
//! ```
//!
//! - `CC` = `SERVICE_QUERY_CONFIG`
//! - `LC` = `SERVICE_QUERY_STATUS`
//! - `RP` = `SERVICE_START`
//! - `WP` = `SERVICE_STOP`
//!
//! （[`OPERATORS_SERVICE_ACL_SDDL_RIGHTS`]・[`OPERATORS_SERVICE_ACCESS_MASK`]
//! 参照）。**意図的に含めないもの**: `DC`(`SERVICE_CHANGE_CONFIG`)・
//! `SD`(`DELETE`)・`WD`(`WRITE_DAC`)・`WO`(`WRITE_OWNER`) - Operators は
//! サービスの起動/停止/状態照会のみを委任され、設定変更・削除・ACL 自体の
//! 変更・所有権変更はできない（実装指示の要求そのもの）。
//!
//! `SC_MANAGER_CONNECT`は SCM オブジェクト自体への接続権であり、個々の
//! サービスオブジェクトの DACL に含める性質のものではない（認証済み
//! ユーザーには既定で許可されている - `windows-service`クレートも
//! `ServiceManagerAccess::CONNECT`を無条件に要求できるのはこのため）ため、
//! このモジュールではサービス DACL への追加対象にしない。
//!
//! ## エラーハンドリング
//!
//! Win32 API 呼び出しの失敗は[`ElevatedError`]（thiserror、日本語メッセージ）
//! にマッピングする。[`crate::service_operators::OperatorsError`]・
//! [`crate::service_manager::ServiceManagerError`]は`#[from]`で透過的に
//! 包む。
//!
//! ## 非 Windows ビルド
//!
//! banto-hub は Windows 専用製品だが、このワークスペース自体は非 Windows
//! でも`cargo check --workspace`が通る必要がある（`service_operators.rs`等と
//! 同じ事情）。[`ElevatedAction`]の enum・parse・[`ElevatedError`]は
//! プラットフォーム非依存だが、実際に Win32 API を呼ぶ関数
//! （[`setup_operators`]・[`grant_service_acl`]・[`run`]等）は
//! `#[cfg(windows)]`のみで提供する - 呼び出し元は`bin/banto-hub-elev.rs`の
//! `#[cfg(windows)] fn main()`のみで、非 Windows 側の`main`はそもそも
//! これらを呼ばない。

use thiserror::Error;

/// `banto-hub-elev.exe <action>`が受け付ける固定アクション。
///
/// フリーフォームの文字列は受け付けない（[`ElevatedAction::parse`]は既知の
/// 6種類以外`None`を返す）- モジュール doc の「固定アクション」節参照。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElevatedAction {
    SetupOperators,
    GrantServiceAcl,
    GrantProfileAcl,
    ServiceInstall,
    ServiceUninstall,
    AutostartEnable,
    AutostartDisable,
}

impl ElevatedAction {
    pub const SETUP_OPERATORS: &'static str = "setup-operators";
    pub const GRANT_SERVICE_ACL: &'static str = "grant-service-acl";
    pub const GRANT_PROFILE_ACL: &'static str = "grant-profile-acl";
    pub const SERVICE_INSTALL: &'static str = "service-install";
    pub const SERVICE_UNINSTALL: &'static str = "service-uninstall";
    pub const AUTOSTART_ENABLE: &'static str = "autostart-enable";
    pub const AUTOSTART_DISABLE: &'static str = "autostart-disable";

    /// [`ElevatedAction::as_str`]が返しうる全文字列 - CLI のヘルプ表示・
    /// エラーメッセージ用（`bin/banto-hub-elev.rs`参照）。
    pub const ALL_NAMES: [&'static str; 7] = [
        Self::SETUP_OPERATORS,
        Self::GRANT_SERVICE_ACL,
        Self::GRANT_PROFILE_ACL,
        Self::SERVICE_INSTALL,
        Self::SERVICE_UNINSTALL,
        Self::AUTOSTART_ENABLE,
        Self::AUTOSTART_DISABLE,
    ];

    /// 大文字小文字を区別する完全一致のみ受け付ける（セキュリティ境界 -
    /// 曖昧な正規化をすると「一見それらしい別文字列」を誤って受理しかねない）。
    /// 一致しない場合は`None`（呼び出し元は「不明な action」として拒否する）。
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            Self::SETUP_OPERATORS => Some(Self::SetupOperators),
            Self::GRANT_SERVICE_ACL => Some(Self::GrantServiceAcl),
            Self::GRANT_PROFILE_ACL => Some(Self::GrantProfileAcl),
            Self::SERVICE_INSTALL => Some(Self::ServiceInstall),
            Self::SERVICE_UNINSTALL => Some(Self::ServiceUninstall),
            Self::AUTOSTART_ENABLE => Some(Self::AutostartEnable),
            Self::AUTOSTART_DISABLE => Some(Self::AutostartDisable),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SetupOperators => Self::SETUP_OPERATORS,
            Self::GrantServiceAcl => Self::GRANT_SERVICE_ACL,
            Self::GrantProfileAcl => Self::GRANT_PROFILE_ACL,
            Self::ServiceInstall => Self::SERVICE_INSTALL,
            Self::ServiceUninstall => Self::SERVICE_UNINSTALL,
            Self::AutostartEnable => Self::AUTOSTART_ENABLE,
            Self::AutostartDisable => Self::AUTOSTART_DISABLE,
        }
    }
}

impl std::fmt::Display for ElevatedAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// [`ElevatedAction::GrantServiceAcl`]が付与する Win32 サービスアクセス権の
/// ビットマスク（`SERVICE_QUERY_CONFIG | SERVICE_QUERY_STATUS |
/// SERVICE_START | SERVICE_STOP`の数値そのもの、モジュール doc「SDDL」節
/// 参照）。windows-sys の対応する定数へ依存を増やさず、
/// クロスプラットフォームでテストできるようこのモジュール冒頭に平文の
/// 数値で持つ - 実際の値が windows-sys の定数と一致することは
/// `service_acl_access_mask_matches_win32_constants`（`#[cfg(windows)]`）が
/// 固定する。
pub const OPERATORS_SERVICE_ACCESS_MASK: u32 = 0x0001 | 0x0004 | 0x0010 | 0x0020;

/// 上記アクセス権を SDDL のサービス用アクセス権文字列で表したもの
/// （モジュール doc「SDDL」節参照、`CC`+`LC`+`RP`+`WP`）。
pub const OPERATORS_SERVICE_ACL_SDDL_RIGHTS: &str = "CCLCRPWP";

/// [`ElevatedAction::GrantServiceAcl`]が実際に追加する ACE を表す SDDL
/// テンプレート - `{sid}`の部分は実行時に解決した`BantoHub Operators`の
/// SID（`S-1-5-21-...`形式）に置き換わる。**このモジュールは実際には
/// `ConvertStringSecurityDescriptorToSecurityDescriptorW`でこの文字列を
/// パースするのではなく**、[`windows_impl::grant_service_acl`]が
/// `EXPLICIT_ACCESS_W`+`SetEntriesInAclW`で同じ内容の ACE を直接組み立てる
/// （SID は実行時解決が必要なため、静的な SDDL 文字列に埋め込めない -
/// モジュール doc参照）。この定数はドキュメント・監査用の表現。
pub const OPERATORS_SERVICE_ACL_SDDL_TEMPLATE: &str = "(A;;CCLCRPWP;;;{sid})";

/// このモジュールの操作の失敗モード。
#[derive(Debug, Error)]
pub enum ElevatedError {
    /// CLI 引数の個数・組み合わせが不正（`bin/banto-hub-elev.rs`からの
    /// 呼び出しで発生しうる - `run`が action ごとの引数個数を検証する）。
    #[error("banto-hub-elev: 引数が不正です: {0}")]
    InvalidArgs(String),
    /// `std::env::current_exe()`（自身の実行ファイルパス取得）の失敗。
    #[error("banto-hub-elev: 自身の実行ファイルパスの取得に失敗しました: {0}")]
    CurrentExePathFailed(String),
    /// `GetUserNameW`（現在の対話ユーザー名取得）の失敗。
    #[error(
        "banto-hub-elev: 現在の対話ユーザー名の取得に失敗しました (GetUserNameW, os error {0})"
    )]
    CurrentUserLookupFailed(u32),
    /// `NetLocalGroupAdd`が`NERR_GroupExists`(2223)・`ERROR_ALIAS_EXISTS`
    /// (1379)以外の理由で失敗した - どちらも「グループが既に存在する」を
    /// 意味する Win32 の戻り値だが、実機検証で環境によって後者が返る
    /// ケースを確認したため両方を成功として扱う
    /// （`create_group_if_missing`参照）。
    #[error(
        "banto-hub-elev: ローカルグループ '{group}' の作成に失敗しました (NetLocalGroupAdd, status {status})"
    )]
    CreateGroupFailed { group: String, status: u32 },
    /// `NetLocalGroupAddMembers`が`NERR_UserInGroup`(2236)・
    /// `ERROR_MEMBER_IN_ALIAS`(1378)以外の理由で失敗した（典型例: 指定
    /// ユーザーが存在しない = `NERR_UserNotFound`）。どちらも「既に
    /// メンバーである」を意味する Win32 の戻り値 -
    /// `add_member_if_missing`参照。
    #[error(
        "banto-hub-elev: ユーザー '{user}' をグループ '{group}' に追加できませんでした (NetLocalGroupAddMembers, status {status})"
    )]
    AddMemberFailed {
        user: String,
        group: String,
        status: u32,
    },
    /// `grant-service-acl`実行時点で`BantoHub Operators`グループが未作成
    /// （`setup-operators`を先に実行する必要がある）。
    #[error(
        "banto-hub-elev: グループ '{0}' がまだ作成されていません。先に setup-operators を実行してください"
    )]
    OperatorsGroupNotFound(String),
    /// `OpenSCManagerW`の失敗。
    #[error("banto-hub-elev: Service Control Manager への接続に失敗しました (os error {0})")]
    OpenScmFailed(u32),
    /// `OpenServiceW`の失敗。
    #[error("banto-hub-elev: サービス '{service}' のオープンに失敗しました (os error {os_error})")]
    OpenServiceFailed { service: String, os_error: u32 },
    /// `QueryServiceObjectSecurity`の失敗。
    #[error(
        "banto-hub-elev: サービス '{service}' のセキュリティ記述子取得に失敗しました (QueryServiceObjectSecurity, os error {os_error})"
    )]
    QuerySecurityFailed { service: String, os_error: u32 },
    /// `GetSecurityDescriptorDacl`の失敗。
    #[error(
        "banto-hub-elev: 既存 DACL の取得に失敗しました (GetSecurityDescriptorDacl, os error {0})"
    )]
    GetDaclFailed(u32),
    /// `SetEntriesInAclW`の失敗（戻り値そのものが`WIN32_ERROR`）。
    #[error("banto-hub-elev: ACL への ACE 追加に失敗しました (SetEntriesInAclW, status {0})")]
    SetEntriesInAclFailed(u32),
    /// `InitializeSecurityDescriptor`の失敗。
    #[error(
        "banto-hub-elev: セキュリティ記述子の初期化に失敗しました (InitializeSecurityDescriptor, os error {0})"
    )]
    InitializeSecurityDescriptorFailed(u32),
    /// `SetSecurityDescriptorDacl`の失敗。
    #[error("banto-hub-elev: DACL の設定に失敗しました (SetSecurityDescriptorDacl, os error {0})")]
    SetSecurityDescriptorDaclFailed(u32),
    /// `SetServiceObjectSecurity`の失敗。
    #[error(
        "banto-hub-elev: サービス '{service}' へのセキュリティ記述子適用に失敗しました (SetServiceObjectSecurity, os error {os_error})"
    )]
    SetServiceSecurityFailed { service: String, os_error: u32 },
    /// [`crate::service_operators`]側のエラーをそのまま透過する
    /// （`lookup_account_sid`の再利用元、モジュール doc参照）。
    #[error(transparent)]
    Operators(#[from] crate::service_operators::OperatorsError),
    /// [`crate::service_manager`]側のエラーをそのまま透過する
    /// （`autostart-enable`/`autostart-disable`が使う）。
    #[error(transparent)]
    ServiceManager(#[from] crate::service_manager::ServiceManagerError),
    /// `grant-profile-acl`の`[profile-id]`引数が不正
    /// （[`crate::profile_paths::validate_profile_id`]が拒否する文字列）。
    #[error(transparent)]
    ProfileId(#[from] crate::profile_paths::ProfileIdError),
    /// [`crate::profile_acl`]側のエラーをそのまま透過する
    /// （`grant-profile-acl`本体、モジュール doc「profile ACL」節参照）。
    #[error(transparent)]
    ProfileAcl(#[from] crate::profile_acl::ProfileAclError),
}

/// [`ElevatedAction::SetupOperators`]の本体（`user`省略時は現在の対話
/// ユーザー、[`windows_impl::current_user_name`]参照）。冪等 - グループ・
/// メンバーシップが既に存在していてもエラーにしない。
#[cfg(windows)]
pub fn setup_operators(user: Option<&str>) -> Result<(), ElevatedError> {
    windows_impl::setup_operators(user)
}

/// [`ElevatedAction::GrantServiceAcl`]の本体（モジュール doc「SDDL」節参照）。
#[cfg(windows)]
pub fn grant_service_acl() -> Result<(), ElevatedError> {
    windows_impl::grant_service_acl()
}

/// [`ElevatedAction::GrantProfileAcl`]の本体（`user`/`profile_id`省略時は
/// それぞれ現在の対話ユーザー／[`crate::profile_paths::DEFAULT_PROFILE_ID`]、
/// モジュール doc「profile ACL」節参照）。
#[cfg(windows)]
pub fn grant_profile_acl(
    user: Option<&str>,
    profile_id: Option<&str>,
) -> Result<(), ElevatedError> {
    windows_impl::grant_profile_acl(user, profile_id)
}

/// `banto-hub-elev.exe`と同じディレクトリにあるはずの`banto-hub.exe`の
/// パスを組み立てる（`service-install`/`autostart-enable`/
/// `autostart-disable`が「登録対象の実行ファイル」として使う - 両バイナリは
/// 同じ`cargo build`出力先ディレクトリにインストールされる前提。
/// `service_install.rs`のモジュール doc・`service_manager.rs`の
/// `WindowsServiceManager`構造体 doc「`set_auto_start`の制約」参照）。
#[cfg(windows)]
fn sibling_banto_hub_exe_path() -> Result<std::path::PathBuf, ElevatedError> {
    let current_exe = std::env::current_exe()
        .map_err(|err| ElevatedError::CurrentExePathFailed(err.to_string()))?;
    let dir = current_exe.parent().ok_or_else(|| {
        ElevatedError::CurrentExePathFailed(
            "実行ファイルの親ディレクトリを取得できませんでした".to_string(),
        )
    })?;
    Ok(dir.join("banto-hub.exe"))
}

/// [`ElevatedAction::ServiceInstall`]の本体 - `banto-hub.exe`を対象に
/// [`crate::service_install::install`]を呼んだ後、`setup-operators`→
/// `grant-service-acl`→`grant-profile-acl`（既定ユーザー・既定 profile）を
/// 続けて実行する（実装指示の順序どおり）。`grant-profile-acl`を新規
/// インストール時点で流しておくことで、初回起動が Desktop/Service
/// どちらであっても「LocalSystem 作成 profile が readonly になる」実機
/// バグ（docs/banto-hub-t16-design.md §3 実機メモ、`crate::profile_acl`
/// モジュール doc参照）を未然に防ぐ。
///
/// `service_install::install`自体は失敗時に`eprintln!`案内の上で
/// `std::process::exit(1)`する（`service_install.rs`のモジュール doc
/// 「挙動は一切変えていない」参照）- そのため、このファイルの`Result`は
/// 実質的に「install 成功後の setup-operators/grant-service-acl/
/// grant-profile-acl の失敗」だけを表す。
#[cfg(windows)]
fn service_install_and_delegate() -> Result<(), ElevatedError> {
    let exe_path = sibling_banto_hub_exe_path()?;
    crate::service_install::install(Some(&exe_path));
    setup_operators(None)?;
    grant_service_acl()?;
    grant_profile_acl(None, None)?;
    Ok(())
}

/// [`ElevatedAction::ServiceUninstall`]の本体 -
/// [`crate::service_install::uninstall`]への単純な委譲
/// （`BantoHub Operators`グループ自体・サービス以外の ACL 変更は
/// このスライスのスコープ外 - 実装指示「service-uninstall — reuse
/// win_service::uninstall」のとおり）。
#[cfg(windows)]
fn service_uninstall_delegate() -> Result<(), ElevatedError> {
    crate::service_install::uninstall();
    Ok(())
}

/// [`ElevatedAction::AutostartEnable`]/[`ElevatedAction::AutostartDisable`]の
/// 本体。`WindowsServiceManager::set_auto_start`（`service_manager.rs`）を
/// 呼ぶだけの薄いラッパー - 再登録に使う実行ファイルパスは
/// [`sibling_banto_hub_exe_path`]で組み立てる。
#[cfg(windows)]
fn set_autostart(enabled: bool) -> Result<(), ElevatedError> {
    use crate::service_manager::ServiceManager as _;
    let exe_path = sibling_banto_hub_exe_path()?;
    let manager = crate::service_manager::WindowsServiceManager::new(exe_path);
    manager.set_auto_start(enabled)?;
    Ok(())
}

/// `bin/banto-hub-elev.rs`から呼ばれる唯一のエントリポイント - パース済みの
/// [`ElevatedAction`]と残りの CLI 引数を受け取り、action ごとの引数個数を
/// 検証した上でディスパッチする。
///
/// `args`は action 自体を除いた残りの引数（`setup-operators`は0〜1個の
/// ユーザー名、`grant-profile-acl`は0〜2個の`[username] [profile-id]`
/// （位置引数、`profile-id`だけを指定して`username`を省略することは
/// できない）を受け付け、他の4アクションは追加引数を受け付けない -
/// モジュール doc の action 表参照）。
#[cfg(windows)]
pub fn run(action: ElevatedAction, args: &[String]) -> Result<(), ElevatedError> {
    match action {
        ElevatedAction::SetupOperators => {
            if args.len() > 1 {
                return Err(too_many_args(action, args.len(), "0〜1"));
            }
            setup_operators(args.first().map(String::as_str))
        }
        ElevatedAction::GrantServiceAcl => {
            reject_extra_args(action, args)?;
            grant_service_acl()
        }
        ElevatedAction::GrantProfileAcl => {
            if args.len() > 2 {
                return Err(too_many_args(action, args.len(), "0〜2"));
            }
            let user = args.first().map(String::as_str);
            let profile_id = args.get(1).map(String::as_str);
            grant_profile_acl(user, profile_id)
        }
        ElevatedAction::ServiceInstall => {
            reject_extra_args(action, args)?;
            service_install_and_delegate()
        }
        ElevatedAction::ServiceUninstall => {
            reject_extra_args(action, args)?;
            service_uninstall_delegate()
        }
        ElevatedAction::AutostartEnable => {
            reject_extra_args(action, args)?;
            set_autostart(true)
        }
        ElevatedAction::AutostartDisable => {
            reject_extra_args(action, args)?;
            set_autostart(false)
        }
    }
}

#[cfg(windows)]
fn reject_extra_args(action: ElevatedAction, args: &[String]) -> Result<(), ElevatedError> {
    if args.is_empty() {
        Ok(())
    } else {
        Err(too_many_args(action, args.len(), "0"))
    }
}

#[cfg(windows)]
fn too_many_args(action: ElevatedAction, got: usize, expected: &str) -> ElevatedError {
    ElevatedError::InvalidArgs(format!(
        "'{action}' の引数は{expected}個ですが{got}個受け取りました"
    ))
}

/// Win32 API を直接叩く実装本体（`#[cfg(windows)]`）。上位の
/// [`setup_operators`]/[`grant_service_acl`]から薄く呼ばれるだけで、
/// 公開関数からは直接見えない実装詳細。
#[cfg(windows)]
mod windows_impl {
    use windows_sys::Win32::Foundation::{
        GetLastError, LocalFree, ERROR_ALIAS_EXISTS, ERROR_INSUFFICIENT_BUFFER,
        ERROR_MEMBER_IN_ALIAS, HLOCAL,
    };
    use windows_sys::Win32::NetworkManagement::NetManagement::{
        NERR_GroupExists, NERR_Success, NERR_UserInGroup, NetLocalGroupAdd,
        NetLocalGroupAddMembers, LOCALGROUP_INFO_1, LOCALGROUP_MEMBERS_INFO_3,
    };
    use windows_sys::Win32::Security::Authorization::{
        SetEntriesInAclW, EXPLICIT_ACCESS_W, NO_MULTIPLE_TRUSTEE, SET_ACCESS, TRUSTEE_IS_GROUP,
        TRUSTEE_IS_SID, TRUSTEE_W,
    };
    use windows_sys::Win32::Security::{
        GetSecurityDescriptorDacl, InitializeSecurityDescriptor, SetSecurityDescriptorDacl, ACL,
        PSECURITY_DESCRIPTOR, SECURITY_DESCRIPTOR,
    };
    use windows_sys::Win32::System::Services::{
        CloseServiceHandle, OpenSCManagerW, OpenServiceW, QueryServiceObjectSecurity,
        SetServiceObjectSecurity, SC_HANDLE, SC_MANAGER_CONNECT,
    };
    use windows_sys::Win32::System::WindowsProgramming::GetUserNameW;

    use super::{ElevatedError, OPERATORS_SERVICE_ACCESS_MASK};
    use crate::service_manager::SERVICE_NAME;
    use crate::service_operators::{windows_impl::lookup_account_sid, OPERATORS_GROUP_NAME};

    /// `DACL_SECURITY_INFORMATION`（`windows-sys`は`Win32_System_Services`
    /// 側からもこの値を再エクスポートしていないため、値そのもの
    /// （MSDN で安定して文書化された定数）をここで定義する -
    /// `service_install.rs`等の既存 doc が採る「見つからない定数は
    /// ハードコードする」方針と同じ）。
    const DACL_SECURITY_INFORMATION: u32 = 0x0000_0004;
    /// `SECURITY_DESCRIPTOR_REVISION`（同上の理由でハードコード。値は
    /// `windows-sys`の`Win32::System::SystemServices`側にも同じ`1`が
    /// 定義されている）。
    const SECURITY_DESCRIPTOR_REVISION: u32 = 1;
    /// サービスオブジェクトの DACL を読む・書き換えるための標準アクセス権
    /// （`STANDARD_RIGHTS_READ`/`_WRITE`相当。数値はどの securable object
    /// にも共通の Win32 標準値 - MSDN `ACCESS_MASK`参照）。
    const READ_CONTROL: u32 = 0x0002_0000;
    const WRITE_DAC: u32 = 0x0004_0000;

    fn to_wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// 現在のプロセスの実行アカウント名を`GetUserNameW`で取得する。
    ///
    /// UAC 昇格後の管理者プロセスであっても、（「別のユーザーとして実行」
    /// ではなく通常の同意プロンプト経由で昇格した限り）実行アカウント自体は
    /// 昇格前と同じユーザーのまま（トークンの種類が変わるだけ）なので、
    /// `banto-hub-elev.exe`を起動した対話ユーザーの名前として扱える -
    /// `setup-operators`の`user`省略時のデフォルトに使う
    /// （モジュール doc・`setup_operators`関数 doc参照）。
    ///
    /// **既知の未検証事項**: 「別のユーザーとして実行」（RunAs 別ユーザー）
    /// 経由で起動された場合はこの前提が崩れる - Windows 実機での確認が
    /// 済んでいない（`service_operators.rs`の同種の注記と同じ位置づけ）。
    pub(super) fn current_user_name() -> Result<String, ElevatedError> {
        let mut buf = vec![0u16; 256];
        loop {
            let mut len = buf.len() as u32;
            // SAFETY: `buf`はちょうど`len`要素の有効なバッファ。失敗時
            // `len`にはヌル終端込みの必要サイズが書き戻される
            // （`GetUserNameW`の契約）。
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
            return Err(ElevatedError::CurrentUserLookupFailed(os_error));
        }
    }

    /// [`super::setup_operators`]の本体。
    pub(super) fn setup_operators(user: Option<&str>) -> Result<(), ElevatedError> {
        let user_owned;
        let user_name: &str = match user {
            Some(name) => name,
            None => {
                user_owned = current_user_name()?;
                &user_owned
            }
        };

        create_group_if_missing()?;
        add_member_if_missing(user_name)?;
        Ok(())
    }

    fn create_group_if_missing() -> Result<(), ElevatedError> {
        let mut name_wide = to_wide(OPERATORS_GROUP_NAME);
        let mut comment_wide = to_wide(
            "banto-hub のサービス操作（開始/停止/状態照会）を委任されたユーザーのグループ（banto-hub-elev.exe が自動作成）",
        );
        let info = LOCALGROUP_INFO_1 {
            lgrpi1_name: name_wide.as_mut_ptr(),
            lgrpi1_comment: comment_wide.as_mut_ptr(),
        };
        let mut parm_err: u32 = 0;
        // SAFETY: `info`は`name_wide`/`comment_wide`（このスコープで生存）
        // へのポインタのみを持つ。`level=1`は`LOCALGROUP_INFO_1`に対応する
        // （NetLocalGroupAdd の契約）。
        let status = unsafe {
            NetLocalGroupAdd(
                std::ptr::null(),
                1,
                &info as *const LOCALGROUP_INFO_1 as *const u8,
                &mut parm_err,
            )
        };
        // `NERR_Success`/`NERR_GroupExists`は windows-sys 側の定数名が
        // 大文字始まりでない（`Win32 ヘッダー名をそのまま踏襲）ため、
        // パターンマッチにすると`non_upper_case_globals`警告になる
        // （`clippy -D warnings`で失敗する）- `if`の等値比較にして回避する。
        //
        // `ERROR_ALIAS_EXISTS`(1379)も`NERR_GroupExists`(2223)と同じく
        // 「グループが既に存在する」を意味する - T17-2 実機検証で、2回目の
        // `setup-operators`実行時に`NetLocalGroupAdd`が`NERR_GroupExists`
        // ではなく`ERROR_ALIAS_EXISTS`を返すケースを確認したため、両方を
        // 冪等な成功として扱う（`ElevatedError::CreateGroupFailed`doc参照）。
        if status == NERR_Success || status == NERR_GroupExists || status == ERROR_ALIAS_EXISTS {
            // 既に存在するグループも冪等性のため成功として扱う
            // （モジュール doc「setup-operators」節参照）。
            return Ok(());
        }
        Err(ElevatedError::CreateGroupFailed {
            group: OPERATORS_GROUP_NAME.to_string(),
            status,
        })
    }

    fn add_member_if_missing(user_name: &str) -> Result<(), ElevatedError> {
        let group_wide = to_wide(OPERATORS_GROUP_NAME);
        let mut member_wide = to_wide(user_name);
        let info = LOCALGROUP_MEMBERS_INFO_3 {
            lgrmi3_domainandname: member_wide.as_mut_ptr(),
        };
        // SAFETY: `group_wide`/`member_wide`はこのスコープで生存している。
        // `level=3`は`LOCALGROUP_MEMBERS_INFO_3`（名前ベース、SID解決は
        // API内部で行われる）に対応する。
        let status = unsafe {
            NetLocalGroupAddMembers(
                std::ptr::null(),
                group_wide.as_ptr(),
                3,
                &info as *const LOCALGROUP_MEMBERS_INFO_3 as *const u8,
                1,
            )
        };
        // 上の`create_group_if_missing`と同じ理由で`if`の等値比較にする。
        // `ERROR_MEMBER_IN_ALIAS`(1378)も`NERR_UserInGroup`(2236)と同じく
        // 「既にメンバーである」を意味する（MSDN 上、`NetLocalGroupAddMembers`
        // はどちらの値も返しうる - `create_group_if_missing`の
        // `ERROR_ALIAS_EXISTS`と対になる注記、`ElevatedError::AddMemberFailed`
        // doc参照）。
        if status == NERR_Success || status == NERR_UserInGroup || status == ERROR_MEMBER_IN_ALIAS {
            // 既にメンバーの場合も冪等性のため成功として扱う。
            return Ok(());
        }
        Err(ElevatedError::AddMemberFailed {
            user: user_name.to_string(),
            group: OPERATORS_GROUP_NAME.to_string(),
            status,
        })
    }

    /// [`super::grant_service_acl`]の本体（モジュール doc「SDDL」節参照）。
    pub(super) fn grant_service_acl() -> Result<(), ElevatedError> {
        let mut operators_sid = lookup_account_sid(OPERATORS_GROUP_NAME)?.ok_or_else(|| {
            ElevatedError::OperatorsGroupNotFound(OPERATORS_GROUP_NAME.to_string())
        })?;

        let scm = open_scm()?;
        let result = grant_service_acl_with_scm(scm, &mut operators_sid);
        // SAFETY: `scm`は`open_scm`が返した有効なハンドル。
        unsafe { CloseServiceHandle(scm) };
        result
    }

    /// [`super::grant_profile_acl`]の本体（モジュール doc「profile ACL」節
    /// 参照）。`user`/`profile_id`省略時のデフォルト解決だけをここで行い、
    /// 実際の DACL 組み立ては[`crate::profile_acl::grant_profile_owner_acl`]
    /// （`crate::service_operators::windows_impl::lookup_account_sid`を
    /// 内部で再利用する別モジュール）へ委譲する。
    pub(super) fn grant_profile_acl(
        user: Option<&str>,
        profile_id: Option<&str>,
    ) -> Result<(), ElevatedError> {
        let user_owned;
        let user_name: &str = match user {
            Some(name) => name,
            None => {
                user_owned = current_user_name()?;
                &user_owned
            }
        };
        let profile_id = profile_id.unwrap_or(crate::profile_paths::DEFAULT_PROFILE_ID);

        // root 解決は`crate::profile_paths::resolve_profile_paths_from_env`
        // と同じ4つの env（`BANTO_HUB_ROOT`/`ProgramData`/`XDG_DATA_HOME`/
        // `HOME`）を読む - `profile-id`だけはこの関数の引数を正とする
        // （`BANTO_HUB_PROFILE`は読まない。「省略時は既定 profile」という
        // モジュール doc の記述どおり固定的に振る舞わせるため）。
        let root = crate::profile_paths::resolve_hub_root(
            std::env::var("BANTO_HUB_ROOT").ok().as_deref(),
            std::env::var("ProgramData").ok().as_deref(),
            std::env::var("XDG_DATA_HOME").ok().as_deref(),
            std::env::var("HOME").ok().as_deref(),
        );
        let paths = crate::profile_paths::resolve_profile_paths(&root, profile_id)?;

        crate::profile_acl::grant_profile_owner_acl(&paths.profile_dir, user_name)?;
        Ok(())
    }

    fn open_scm() -> Result<SC_HANDLE, ElevatedError> {
        // SAFETY: 引数はすべて null（ローカルマシン・既定データベース）。
        let scm = unsafe { OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT) };
        if scm.is_null() {
            let os_error = unsafe { GetLastError() };
            return Err(ElevatedError::OpenScmFailed(os_error));
        }
        Ok(scm)
    }

    fn grant_service_acl_with_scm(
        scm: SC_HANDLE,
        operators_sid: &mut [u8],
    ) -> Result<(), ElevatedError> {
        let service_wide = to_wide(SERVICE_NAME);
        // SAFETY: `scm`は呼び出し元が確保した有効なハンドル。
        let service = unsafe { OpenServiceW(scm, service_wide.as_ptr(), READ_CONTROL | WRITE_DAC) };
        if service.is_null() {
            let os_error = unsafe { GetLastError() };
            return Err(ElevatedError::OpenServiceFailed {
                service: SERVICE_NAME.to_string(),
                os_error,
            });
        }

        let result = grant_service_acl_with_service(service, operators_sid);
        // SAFETY: `service`は直前に`OpenServiceW`が返した有効なハンドル。
        unsafe { CloseServiceHandle(service) };
        result
    }

    fn grant_service_acl_with_service(
        service: SC_HANDLE,
        operators_sid: &mut [u8],
    ) -> Result<(), ElevatedError> {
        // 1回目: 必要バッファサイズの問い合わせ（`QueryServiceObjectSecurity`
        // の標準的な2段呼び出しパターン - `service_operators.rs`の
        // `LookupAccountNameW`と同じ考え方）。
        let mut needed: u32 = 0;
        // SAFETY: バッファ長 0・null ポインタで呼ぶのは Win32 の想定どおり
        // （FALSE を返し`needed`へ必要サイズを書き込む）。
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
            let os_error = unsafe { GetLastError() };
            return Err(ElevatedError::QuerySecurityFailed {
                service: SERVICE_NAME.to_string(),
                os_error,
            });
        }

        let mut sd_buf = vec![0u8; needed as usize];
        // SAFETY: `sd_buf`は直前の呼び出しが報告したサイズちょうどの
        // バッファ。
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
            let os_error = unsafe { GetLastError() };
            return Err(ElevatedError::QuerySecurityFailed {
                service: SERVICE_NAME.to_string(),
                os_error,
            });
        }

        let mut dacl_present: i32 = 0;
        let mut dacl_ptr: *mut ACL = std::ptr::null_mut();
        let mut dacl_defaulted: i32 = 0;
        // SAFETY: `sd_buf`は直前に`QueryServiceObjectSecurity`が書き込んだ
        // 有効なセキュリティ記述子。
        let ok = unsafe {
            GetSecurityDescriptorDacl(
                sd_buf.as_mut_ptr() as PSECURITY_DESCRIPTOR,
                &mut dacl_present,
                &mut dacl_ptr,
                &mut dacl_defaulted,
            )
        };
        if ok == 0 {
            let os_error = unsafe { GetLastError() };
            return Err(ElevatedError::GetDaclFailed(os_error));
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
            // Win32 API の慣例どおり、`ptstrName`フィールドへ SID への
            // ポインタをそのままキャストして渡す
            // （`TrusteeForm=TRUSTEE_IS_SID`の場合の契約 - MSDN
            // `TRUSTEE`構造体参照）。
            ptstrName: operators_sid.as_mut_ptr() as *mut u16,
        };

        let explicit_access = EXPLICIT_ACCESS_W {
            grfAccessPermissions: OPERATORS_SERVICE_ACCESS_MASK,
            // `SET_ACCESS`: 同じ trustee の既存エントリがあれば置き換える
            // （`GRANT_ACCESS`のような加算マージではない）- 再実行しても
            // 権限が増え続けない冪等性を保つため。
            grfAccessMode: SET_ACCESS,
            // サービスオブジェクトは子オブジェクトを持たないため継承は
            // 無関係（`NO_INHERITANCE`は数値上`0` - `windows-sys`に定数が
            // 存在しないため直接`0`を使う）。
            grfInheritance: 0,
            Trustee: trustee,
        };

        let mut new_acl: *mut ACL = std::ptr::null_mut();
        // SAFETY: `old_dacl`は上で取得した有効な（または未設定なら null の）
        // ACL、`explicit_access`はこのスコープで生存している。
        let status = unsafe { SetEntriesInAclW(1, &explicit_access, old_dacl, &mut new_acl) };
        if status != 0 {
            return Err(ElevatedError::SetEntriesInAclFailed(status));
        }

        let cleanup_new_acl = || {
            // SAFETY: `new_acl`は`SetEntriesInAclW`が`LocalAlloc`で確保した
            // メモリ（MSDN の契約どおり`LocalFree`で解放する）。
            unsafe { LocalFree(new_acl as HLOCAL) };
        };

        let mut new_sd = SECURITY_DESCRIPTOR::default();
        // SAFETY: `new_sd`はこのスコープの固定サイズローカル変数。
        let ok = unsafe {
            InitializeSecurityDescriptor(
                &mut new_sd as *mut SECURITY_DESCRIPTOR as PSECURITY_DESCRIPTOR,
                SECURITY_DESCRIPTOR_REVISION,
            )
        };
        if ok == 0 {
            let os_error = unsafe { GetLastError() };
            cleanup_new_acl();
            return Err(ElevatedError::InitializeSecurityDescriptorFailed(os_error));
        }

        // SAFETY: `new_acl`は直前に構築した有効な ACL、`new_sd`は
        // 初期化済みのセキュリティ記述子。
        let ok = unsafe {
            SetSecurityDescriptorDacl(
                &mut new_sd as *mut SECURITY_DESCRIPTOR as PSECURITY_DESCRIPTOR,
                1,
                new_acl,
                0,
            )
        };
        if ok == 0 {
            let os_error = unsafe { GetLastError() };
            cleanup_new_acl();
            return Err(ElevatedError::SetSecurityDescriptorDaclFailed(os_error));
        }

        // SAFETY: `service`は呼び出し元が確保した有効なハンドル、`new_sd`は
        // 直前に DACL を設定済みの有効なセキュリティ記述子。
        let ok = unsafe {
            SetServiceObjectSecurity(
                service,
                DACL_SECURITY_INFORMATION,
                &mut new_sd as *mut SECURITY_DESCRIPTOR as PSECURITY_DESCRIPTOR,
            )
        };
        cleanup_new_acl();
        if ok == 0 {
            let os_error = unsafe { GetLastError() };
            return Err(ElevatedError::SetServiceSecurityFailed {
                service: SERVICE_NAME.to_string(),
                os_error,
            });
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_accepts_all_documented_actions() {
        assert_eq!(
            ElevatedAction::parse("setup-operators"),
            Some(ElevatedAction::SetupOperators)
        );
        assert_eq!(
            ElevatedAction::parse("grant-service-acl"),
            Some(ElevatedAction::GrantServiceAcl)
        );
        assert_eq!(
            ElevatedAction::parse("grant-profile-acl"),
            Some(ElevatedAction::GrantProfileAcl)
        );
        assert_eq!(
            ElevatedAction::parse("service-install"),
            Some(ElevatedAction::ServiceInstall)
        );
        assert_eq!(
            ElevatedAction::parse("service-uninstall"),
            Some(ElevatedAction::ServiceUninstall)
        );
        assert_eq!(
            ElevatedAction::parse("autostart-enable"),
            Some(ElevatedAction::AutostartEnable)
        );
        assert_eq!(
            ElevatedAction::parse("autostart-disable"),
            Some(ElevatedAction::AutostartDisable)
        );
    }

    #[test]
    fn parse_rejects_unknown_and_free_form_strings() {
        assert_eq!(ElevatedAction::parse(""), None);
        assert_eq!(ElevatedAction::parse("install"), None);
        assert_eq!(ElevatedAction::parse("uninstall"), None);
        // 大文字小文字違いも拒否する（セキュリティ境界 - parse doc参照）。
        assert_eq!(ElevatedAction::parse("Setup-Operators"), None);
        assert_eq!(ElevatedAction::parse("SETUP-OPERATORS"), None);
        // シェル経由の危険な入力例も、単なる「知らない文字列」として
        // 一律拒否されることを確認する。
        assert_eq!(ElevatedAction::parse("setup-operators; rm -rf /"), None);
    }

    #[test]
    fn as_str_round_trips_through_parse() {
        for &name in ElevatedAction::ALL_NAMES.iter() {
            let action = ElevatedAction::parse(name).expect("ALL_NAMES entries must parse");
            assert_eq!(action.as_str(), name);
            assert_eq!(action.to_string(), name);
        }
    }

    #[test]
    fn all_names_has_no_duplicates_and_matches_action_count() {
        let mut names: Vec<&str> = ElevatedAction::ALL_NAMES.to_vec();
        let original_len = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), original_len, "ALL_NAMES に重複がある");
        assert_eq!(original_len, 7, "固定アクションは7種類のはず");
    }

    #[test]
    fn operators_service_access_mask_matches_documented_rights() {
        // SERVICE_QUERY_CONFIG(0x1) | SERVICE_QUERY_STATUS(0x4) |
        // SERVICE_START(0x10) | SERVICE_STOP(0x20) = 0x35。
        assert_eq!(OPERATORS_SERVICE_ACCESS_MASK, 0x35);
    }

    #[test]
    fn sddl_rights_string_matches_access_mask_letters() {
        // CC=QUERY_CONFIG, LC=QUERY_STATUS, RP=START, WP=STOP
        // （モジュール doc「SDDL」節参照）。
        assert_eq!(OPERATORS_SERVICE_ACL_SDDL_RIGHTS, "CCLCRPWP");
        assert!(OPERATORS_SERVICE_ACL_SDDL_TEMPLATE.contains(OPERATORS_SERVICE_ACL_SDDL_RIGHTS));
        assert!(OPERATORS_SERVICE_ACL_SDDL_TEMPLATE.contains("{sid}"));
    }

    #[test]
    fn sddl_template_intentionally_excludes_dangerous_rights() {
        // DC(SERVICE_CHANGE_CONFIG)/SD(DELETE)/WD(WRITE_DAC)/WO(WRITE_OWNER)
        // を含めないことを明示的に固定する（実装指示の要求そのもの、
        // モジュール doc「SDDL」節参照）。
        for forbidden in ["DC", "SD", "WD", "WO"] {
            assert!(
                !OPERATORS_SERVICE_ACL_SDDL_TEMPLATE.contains(forbidden),
                "SDDL テンプレートに禁止権限 '{forbidden}' が含まれている"
            );
        }
    }

    /// `create_group_if_missing`/`add_member_if_missing`が「既に存在する」
    /// 判定に使う追加の Win32 エラーコードの実値を固定する - T17-2 実機
    /// 検証（2回目の`setup-operators`実行）で`NetLocalGroupAdd`が
    /// `NERR_GroupExists`(2223)ではなく`ERROR_ALIAS_EXISTS`(1379)を返す
    /// ケースを確認した際の回帰防止。windows-sys のクレートバージョン変更で
    /// これらの数値が変わった場合にビルドが失敗する
    /// （`#[cfg(windows)]`のみ - MSDN で安定して文書化された Win32 の
    /// エラーコードだが、非 Windows ビルドでは windows-sys 自体を使わない）。
    #[cfg(windows)]
    #[test]
    fn alias_exists_error_codes_match_win32_constants() {
        use windows_sys::Win32::Foundation::{ERROR_ALIAS_EXISTS, ERROR_MEMBER_IN_ALIAS};
        assert_eq!(
            ERROR_ALIAS_EXISTS, 1379,
            "ERROR_ALIAS_EXISTS の値が変わった"
        );
        assert_eq!(
            ERROR_MEMBER_IN_ALIAS, 1378,
            "ERROR_MEMBER_IN_ALIAS の値が変わった"
        );
    }

    #[cfg(windows)]
    #[test]
    fn service_acl_access_mask_matches_win32_constants() {
        use windows_sys::Win32::System::Services::{
            SERVICE_QUERY_CONFIG, SERVICE_QUERY_STATUS, SERVICE_START, SERVICE_STOP,
        };
        assert_eq!(
            OPERATORS_SERVICE_ACCESS_MASK,
            SERVICE_QUERY_CONFIG | SERVICE_QUERY_STATUS | SERVICE_START | SERVICE_STOP
        );
    }

    #[cfg(windows)]
    #[test]
    fn run_rejects_extra_args_for_no_arg_actions() {
        let extra = vec!["unexpected".to_string()];
        for action in [
            ElevatedAction::GrantServiceAcl,
            ElevatedAction::AutostartEnable,
            ElevatedAction::AutostartDisable,
        ] {
            let err = run(action, &extra).expect_err("extra args should be rejected");
            assert!(matches!(err, ElevatedError::InvalidArgs(_)));
        }
    }

    #[cfg(windows)]
    #[test]
    fn run_rejects_too_many_args_for_setup_operators() {
        let extra = vec!["user-a".to_string(), "user-b".to_string()];
        let err = run(ElevatedAction::SetupOperators, &extra)
            .expect_err("setup-operators only accepts 0-1 args");
        assert!(matches!(err, ElevatedError::InvalidArgs(_)));
    }

    #[cfg(windows)]
    #[test]
    fn run_rejects_too_many_args_for_grant_profile_acl() {
        let extra = vec![
            "user-a".to_string(),
            "profile-a".to_string(),
            "unexpected-third".to_string(),
        ];
        let err = run(ElevatedAction::GrantProfileAcl, &extra)
            .expect_err("grant-profile-acl only accepts 0-2 args");
        assert!(matches!(err, ElevatedError::InvalidArgs(_)));
    }

    /// 実際に`BantoHub Operators`グループを作成・メンバー追加する -
    /// 管理者権限が必要なため CI・通常の開発機では実行しない
    /// （`#[ignore]`）。Windows 実機で
    /// `cargo test -p banto-hub-core --lib service_elevated -- --ignored`
    /// を管理者権限のシェルから実行して確認する。
    ///
    /// 2回目の`setup_operators(None)`呼び出しが冪等な成功であることの
    /// 確認が、`create_group_if_missing`/`add_member_if_missing`の
    /// `ERROR_ALIAS_EXISTS`(1379)/`ERROR_MEMBER_IN_ALIAS`(1378)対応の
    /// 実質的な回帰テストになっている（T17-2 実機検証で発見した不具合 -
    /// `ElevatedError::CreateGroupFailed`doc参照）。
    #[cfg(windows)]
    #[test]
    #[ignore = "管理者権限と実マシンでのグループ作成が必要 - Windows 実機で手動実行"]
    fn setup_operators_creates_group_idempotently() {
        // 冪等性の確認がこのテストの本体。メンバーシップのトークン反映
        // （`is_current_process_operator`）はログオン後追加グループが
        // 既存トークンに載らない既知制約があるためここでは見ない
        // （`service_operators` モジュール doc・T17-2 実機検証メモ参照）。
        setup_operators(None).expect("first call should succeed");
        setup_operators(None).expect("second call should be a no-op success (idempotent)");
    }

    /// 実際に`BantoHub`サービスへ DACL を適用する - `BantoHub`サービスが
    /// 事前にインストール済みで、かつ管理者権限が必要なため`#[ignore]`。
    /// Windows 実機で`service-install`実行後に手動実行して確認する。
    #[cfg(windows)]
    #[test]
    #[ignore = "管理者権限・BantoHub サービスの事前インストールが必要 - Windows 実機で手動実行"]
    fn grant_service_acl_applies_without_error() {
        setup_operators(None).expect("setup-operators should succeed first");
        grant_service_acl().expect("grant-service-acl should succeed against an installed service");
    }

    /// 実際に`%ProgramData%\BantoHub\profiles\<profile-id>\`へ ACL を
    /// 適用する - `%ProgramData%`配下への書き込みには管理者権限が必要
    /// なため`#[ignore]`。Windows 実機で、LocalSystem サービス起動後の
    /// 実 profile ディレクトリに対して手動実行し、対話ユーザーで DB が
    /// 書き込めるようになることを確認する
    /// （docs/banto-hub-t16-design.md §3 実機メモの再現・解消確認）。
    #[cfg(windows)]
    #[test]
    #[ignore = "管理者権限・%ProgramData% への書き込みが必要 - Windows 実機で手動実行"]
    fn grant_profile_acl_applies_to_default_profile() {
        grant_profile_acl(None, None)
            .expect("grant-profile-acl should succeed for the current user / default profile");
    }
}
