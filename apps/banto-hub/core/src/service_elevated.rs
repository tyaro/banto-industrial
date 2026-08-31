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
//! `banto-hub-elev.exe <action> [args...]`が受け付けるのは次の9種類のみ -
//! フリーフォームのコマンド文字列は一切受け付けない（UAC 昇格した管理者
//! プロセスに任意コマンドを渡させないためのセキュリティ境界、実装指示の
//! 「固定アクション」要求そのもの）。**2026-08-31 追加の2種類
//! （`reset-password`/`revert-to-commissioning`）も同じ境界を維持したまま
//! 追加した** - 引数は個数・意味を固定した位置引数のみで、パスワード本体は
//! 引数に一切含めない（下記「ロックダウン回復アクション」節参照）。
//!
//! | action                      | 処理内容                                                              |
//! |------------------------------|------------------------------------------------------------------------|
//! | `setup-operators`            | ローカルグループ`BantoHub Operators`を（無ければ）作成し、指定ユーザー（省略時は現在の対話ユーザー）をメンバーに追加する。冪等 |
//! | `grant-service-acl`          | `BantoHub`サービスの DACL に`BantoHub Operators`への限定 ACE を追加する（下記「SDDL」節） |
//! | `grant-profile-acl`          | profile ディレクトリ（`[username] [profile-id]`、両方省略時は現在の対話ユーザー・既定 profile）へ owner 用 DACL を付与する（[`crate::profile_acl`]、下記「profile ACL」節） |
//! | `service-install`            | [`crate::service_install::install`]（`banto-hub.exe`本体を対象）→`setup-operators`→`grant-service-acl`→`grant-profile-acl`（既定ユーザー・既定 profile）の順で実行 |
//! | `service-uninstall`          | [`crate::service_install::uninstall`]をそのまま呼ぶ                    |
//! | `autostart-enable`           | `WindowsServiceManager::set_auto_start(true)`                          |
//! | `autostart-disable`          | `WindowsServiceManager::set_auto_start(false)`                         |
//! | `reset-password`             | `<username> [profile-id]` - 対象 profile の DB を開き、指定ユーザーのパスワードを標準入力から読んだ新パスワードへ再設定する（下記「ロックダウン回復アクション」節） |
//! | `revert-to-commissioning`    | `[profile-id]` - 対象 profile をロックダウン済み→試運転モードへ戻す（[`crate::commissioning::revert_to_commissioning`]、下記「ロックダウン回復アクション」節） |
//!
//! ## ロックダウン回復アクション（`reset-password`/`revert-to-commissioning`、
//! 2026-08-31 オーナー決定・docs/tag-server-design.md §5.6 制約3/4）
//!
//! **背景**: §5.6 の「ロックダウンは UI・REST からは後戻りできない」設計は、
//! 裏を返すと「管理者パスワードを紛失した」「ロックダウンしたまま改造・
//! 再試運転が必要になった」場面で詰む経路が必要ということでもある。
//! この2アクションは、その回復経路を「ローカルの管理者権限を持つ人
//! （＝機械の前に立てる人）」に限定して提供する（UAC 昇格必須 = ネットワーク
//! 越しには実行できない、既存7種類と同じ前提）。
//!
//! **パスワードの受け渡し方法（コマンドライン引数にしない理由）**:
//! `reset-password`はコマンドライン引数として新パスワードを受け取らない -
//! Windows では他ユーザーも`tasklist /v`・`Get-Process -IncludeUserName`・
//! WMI 等でプロセスの起動コマンドラインを読める場合があり、また
//! シェルの入力履歴（PowerShell の `Get-History`/`ConsoleHost_history.txt`
//! 等）にも残ってしまう。代わりに**標準入力から対話的に読む**
//! （[`windows_impl::read_password`]、コンソールのエコーを一時的に止めて
//! 画面にも極力残さない・確認のため2回入力させ不一致ならエラー）ことで、
//! プロセス一覧・シェル履歴のどちらにも新パスワードの平文が残らない。
//! ユーザー名（`username`）は秘密ではないため、他アクション同様に通常の
//! 位置引数で受け取る。
//!
//! **`revert_to_commissioning`を elev 限定にする理由**: 認証を一切要求しない
//! 状態へ戻す操作であり、UI・REST から実行できてしまうと「ロックダウンは
//! 後戻りできない」という§5.6の安全設計そのものが崩れる。UAC 昇格
//! （ローカル管理者トークンの同意）を要求する`banto-hub-elev.exe`だけに
//! 経路を閉じることで、「ネットワーク越しに試運転モードへ戻される」
//! 攻撃を構造的に排除する。[`crate::commissioning::revert_to_commissioning`]
//! （実行中の Hub プロセスを持たない呼び出し元向けの自由関数版 - doc comment
//! 参照）を、対象 profile の DB パスへ直接開いた `SqlitePool` で呼ぶ。
//!
//! **監査ログ**（§5.6 制約4「試運転モードへの復帰は監査ログに記録する」）:
//! [`revert_to_commissioning_with_audit`]が、復帰の実行直後に
//! `crate::audit::AuditLogService`へ1行書き込む -
//! `actor_username`には**ローカル OS ユーザー名**
//! （[`windows_impl::current_user_name`]、UAC 昇格前後で実行アカウントは
//! 変わらない）を記録する。banto-hub のログインユーザー名ではない - この
//! 操作自体が「ログインを要求しない状態へ戻す」ものなので、アプリ側の
//! ログインアカウントという概念に頼れない（既存の`SYNTHETIC_ACTOR_ID`の
//! ような合成値でもなく、実際に操作した Windows アカウントを残すことに
//! 意味がある）。`origin`は既存の`"rest"`固定ではなく`"elev"`とした - REST
//! 経由ではない別プロセス・別経路からの操作であることが監査ログ上で
//! 区別できるようにするため（`crate::audit`モジュール doc の「origin は
//! transport-agnostic な`&str`」という設計に沿った拡張）。
//!
//! **確認プロンプト**: 既存7種類は非対話（確認なしで即実行）だが、この
//! 2アクションは実行前に何が起きるかを表示し、y/N の確認を求める
//! （[`windows_impl::confirm`]）- 既存の慣習からは外れるが、「パスワード
//! 再設定」「認証を外す」という取り返しにくい操作の性質上、実装指示が
//! 明示的に要求している。`revert-to-commissioning`はさらに「復帰後は
//! loopback バインドでないと次回起動できなくなる」（制約1が再び効く）
//! ことも確認プロンプトの前に警告表示する。
//!
//! **profile-id の扱い**: 両アクションとも末尾に任意の`[profile-id]`引数を
//! 取り、省略時は`grant-profile-acl`と同じく
//! [`crate::profile_paths::DEFAULT_PROFILE_ID`]を使う - 複数 profile
//! （desktop-plan §11の複数ライン運用）のどれを対象にするか明示できるよう
//! 既存の流儀を踏襲した。DB パスは
//! [`crate::profile_paths::resolve_profile_paths`]で解決し、DB ファイルが
//! 実在しない場合は「サイレントに新規空 DB を作ってしまう」
//! （`banto_storage::connect_sqlite`が`create_if_missing(true)`のため）事故を
//! 避けるために、開く前に存在確認する（[`ElevatedError::ProfileDbNotFound`]）。
//!
//! **中核処理の分離**: [`reset_user_password`]・
//! [`revert_to_commissioning_with_audit`]は`SqlitePool`を直接受け取る
//! 純粋な非同期関数で、`#[cfg(windows)]`を掛けていない -
//! Win32 コンソール入出力（パスワード入力・確認プロンプト）を一切含まない
//! ため、`crate::db::migrate_memory`を使って非 Windows でも
//! `cargo test`で検証できる（このモジュール doc「非 Windows ビルド」節と
//! 同じ考え方）。対話的な入出力・DB パス解決・UAC 前提の部分だけを
//! [`windows_impl`]・`run`の`#[cfg(windows)]`側に残した。
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
/// 9種類以外`None`を返す）- モジュール doc の「固定アクション」節参照。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElevatedAction {
    SetupOperators,
    GrantServiceAcl,
    GrantProfileAcl,
    ServiceInstall,
    ServiceUninstall,
    AutostartEnable,
    AutostartDisable,
    /// 2026-08-31 追加（モジュール doc「ロックダウン回復アクション」節）。
    ResetPassword,
    /// 2026-08-31 追加（同上）。
    RevertToCommissioning,
}

impl ElevatedAction {
    pub const SETUP_OPERATORS: &'static str = "setup-operators";
    pub const GRANT_SERVICE_ACL: &'static str = "grant-service-acl";
    pub const GRANT_PROFILE_ACL: &'static str = "grant-profile-acl";
    pub const SERVICE_INSTALL: &'static str = "service-install";
    pub const SERVICE_UNINSTALL: &'static str = "service-uninstall";
    pub const AUTOSTART_ENABLE: &'static str = "autostart-enable";
    pub const AUTOSTART_DISABLE: &'static str = "autostart-disable";
    pub const RESET_PASSWORD: &'static str = "reset-password";
    pub const REVERT_TO_COMMISSIONING: &'static str = "revert-to-commissioning";

    /// [`ElevatedAction::as_str`]が返しうる全文字列 - CLI のヘルプ表示・
    /// エラーメッセージ用（`bin/banto-hub-elev.rs`参照）。
    pub const ALL_NAMES: [&'static str; 9] = [
        Self::SETUP_OPERATORS,
        Self::GRANT_SERVICE_ACL,
        Self::GRANT_PROFILE_ACL,
        Self::SERVICE_INSTALL,
        Self::SERVICE_UNINSTALL,
        Self::AUTOSTART_ENABLE,
        Self::AUTOSTART_DISABLE,
        Self::RESET_PASSWORD,
        Self::REVERT_TO_COMMISSIONING,
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
            Self::RESET_PASSWORD => Some(Self::ResetPassword),
            Self::REVERT_TO_COMMISSIONING => Some(Self::RevertToCommissioning),
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
            Self::ResetPassword => Self::RESET_PASSWORD,
            Self::RevertToCommissioning => Self::REVERT_TO_COMMISSIONING,
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
    /// `reset-password`/`revert-to-commissioning`対象 profile の DB
    /// （`crate::db::init_db`）・アプリ層サービス（`UsersService`/
    /// `CommissioningService`/`AuditLogService`）から返るエラーをそのまま
    /// 透過する（モジュール doc「ロックダウン回復アクション」節参照）。
    #[error(transparent)]
    Db(#[from] banto_core::BantoError),
    /// `reset-password`の対象ユーザーが`users`テーブルに存在しない
    /// （[`crate::users::UsersService::get_by_username`]が`None`を返した）。
    /// パスワードを紛失した状況での操作なので、「どのユーザー名を
    /// 打ち間違えたか」がすぐ分かるよう明確なエラーにする。
    #[error("banto-hub-elev: ユーザー '{0}' は存在しません")]
    UserNotFound(String),
    /// 対象 profile の DB ファイルが存在しない
    /// （[`crate::profile_paths::resolve_profile_paths`]が返す`db_path`）。
    /// `banto_storage::connect_sqlite`は`create_if_missing(true)`なので、
    /// 存在確認せずに開くと「間違った profile-id を指定したのに気づかず
    /// 空の新規 DB を作ってしまい、直後の 'ユーザーが存在しない' エラーで
    /// 原因を誤解する」事故になりうる - それを避けるための事前チェック
    /// （モジュール doc「profile-id の扱い」節参照）。
    #[error(
        "banto-hub-elev: profile のデータベースが見つかりません: {0} \
         （profile-id の指定が正しいか確認してください）"
    )]
    ProfileDbNotFound(std::path::PathBuf),
    /// 確認プロンプト（[`windows_impl::confirm`]）でユーザーが承諾しなかった
    /// （`y`/`yes`以外を入力した）- パスワード再設定・試運転モード復帰は
    /// 取り返しにくい操作のため、実装指示により確認必須にしている
    /// （モジュール doc「確認プロンプト」節参照）。
    #[error("banto-hub-elev: 確認が得られなかったため中止しました")]
    ConfirmationDeclined,
    /// `reset-password`で2回入力した新パスワードが一致しなかった
    /// （[`windows_impl::read_password`]でエコーを止めて読むため、
    /// 誤入力に気づけるよう2回入力させて突き合わせている）。
    #[error("banto-hub-elev: 入力した新しいパスワードが一致しません")]
    PasswordMismatch,
    /// 標準入力からのパスワード読み取り（コンソールのエコー抑止含む）に
    /// 失敗した - パイプ経由の自動化等、対話コンソールでない標準入力を
    /// 想定していないため、失敗時は素直にエラーにする
    /// （モジュール doc「パスワードの受け渡し方法」節参照）。
    #[error("banto-hub-elev: 標準入力からの読み取りに失敗しました: {0}")]
    ConsoleIoFailed(String),
    /// DB 操作用の tokio ランタイム起動（`tokio::runtime::Runtime::new`、
    /// `bin/banto-hub.rs`と同じパターン）に失敗した。
    #[error("banto-hub-elev: 非同期ランタイムの起動に失敗しました: {0}")]
    RuntimeStartFailed(String),
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

// --- ロックダウン回復アクションの中核処理（Win32 非依存・非 Windows でも
// `cargo test`可能、モジュール doc「中核処理の分離」節参照） -------------

/// [`ElevatedAction::ResetPassword`]の中核処理。対象 profile の`users`
/// テーブルから`username`を引き、[`crate::users::UsersService::reset_password`]
/// （既存の`hash_password`をそのまま使う - 独自実装はしない）でパスワードを
/// 更新する。ユーザーが存在しなければ[`ElevatedError::UserNotFound`]。
///
/// コンソール入出力（パスワードの対話入力・確認プロンプト）を一切含まない
/// 薄いラッパーなので`#[cfg(windows)]`を掛けていない - `pool`は呼び出し元
/// （実プロセスでは[`reset_password_action`]、テストでは
/// `crate::db::migrate_memory`）が用意する。
pub async fn reset_user_password(
    pool: &sqlx::SqlitePool,
    username: &str,
    new_password: &str,
) -> Result<(), ElevatedError> {
    let users = crate::users::UsersService::new(pool.clone());
    let target = users
        .get_by_username(username)
        .await?
        .ok_or_else(|| ElevatedError::UserNotFound(username.to_string()))?;
    users.reset_password(target.id, new_password).await?;
    Ok(())
}

/// [`ElevatedAction::RevertToCommissioning`]の中核処理。
/// [`crate::commissioning::revert_to_commissioning`]（実行中の Hub
/// プロセスを持たない呼び出し元向けの自由関数版、`commissioning.rs`の
/// doc comment参照）を呼んだ直後に、監査ログへ1行記録する（§5.6 制約4
/// 「試運転モードへの復帰は監査ログに記録する」・モジュール doc
/// 「監査ログ」節）。
///
/// `actor`には呼び出し元が解決した**ローカル OS ユーザー名**を渡す
/// （banto-hub のログインユーザー名ではない - モジュール doc「監査ログ」
/// 節参照）。監査ログの書き込みが失敗した場合はこの関数自体もエラーを
/// 返す（`crate::audit::AuditLogService::record`の「失敗しても呼び出し元の
/// 操作は失敗させない」fire-and-forget方針ではなく`try_record`を使う） -
/// 「必ず記録する」という制約4の要求に対しては、書き込み失敗を握り潰して
/// 記録漏れに気づけなくなる方が、操作自体がエラーで止まるより危険だと
/// 判断したため。
///
/// コンソール入出力を含まないため`#[cfg(windows)]`を掛けていない -
/// [`reset_user_password`]と同じ理由（モジュール doc「中核処理の分離」節）。
pub async fn revert_to_commissioning_with_audit(
    pool: &sqlx::SqlitePool,
    actor: &str,
) -> Result<(), ElevatedError> {
    crate::commissioning::revert_to_commissioning(pool).await?;

    let audit = crate::audit::AuditLogService::new(pool.clone());
    audit
        .try_record(crate::audit::AuditEntry {
            actor_username: Some(actor),
            // アプリ側のロール概念に対応する値が無い（ログインを要求しない
            // 状態へ戻す操作そのものなので、ロールを引けるログイン
            // セッションが存在しない） - `None`のままにする
            // （`crate::audit`は`actor_role: None`を「未認証イベント」で
            // 許容済み、`login_failed`の既存例と同じ扱い）。
            actor_role: None,
            action: "commissioning_revert",
            resource: "commissioning",
            entity_id: None,
            detail: Some(serde_json::json!({
                "via": "banto-hub-elev.exe",
                "note": "ロックダウン済み→試運転モードへ復帰（要 loopback バインド）",
            })),
            // REST 経由の操作は`crate::rest`が一律`"rest"`を使うが、これは
            // 別プロセス・UAC 昇格経由の操作なので区別できる値にする
            // （モジュール doc「監査ログ」節）。
            origin: "elev",
            result: "ok",
        })
        .await?;
    Ok(())
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

/// `[profile-id]`（省略時は既定 profile）から対象 profile の
/// [`crate::profile_paths::ProfilePaths`]を解決する -
/// `reset-password`/`revert-to-commissioning`共通（`grant-profile-acl`の
/// root 解決ロジックと同じ4つの env、モジュール doc「profile-id の扱い」
/// 節参照）。DB ファイルが実在しない場合は
/// [`ElevatedError::ProfileDbNotFound`]で早期に拒否する。
#[cfg(windows)]
fn resolve_target_profile_paths(
    profile_id: Option<&str>,
) -> Result<crate::profile_paths::ProfilePaths, ElevatedError> {
    let profile_id = profile_id.unwrap_or(crate::profile_paths::DEFAULT_PROFILE_ID);
    let root = crate::profile_paths::resolve_hub_root(
        std::env::var("BANTO_HUB_ROOT").ok().as_deref(),
        std::env::var("ProgramData").ok().as_deref(),
        std::env::var("XDG_DATA_HOME").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
    );
    let paths = crate::profile_paths::resolve_profile_paths(&root, profile_id)?;
    if !paths.db_path.is_file() {
        return Err(ElevatedError::ProfileDbNotFound(paths.db_path));
    }
    Ok(paths)
}

/// DB を触る2アクション共通: 新しい`tokio::runtime::Runtime`を1回だけ立てて
/// `body`を`block_on`する（`bin/banto-hub.rs`の`main`と同じパターン -
/// このバイナリ自体は同期`fn main`のままで、DB 操作の瞬間だけランタイムを
/// 借りる）。
#[cfg(windows)]
fn block_on_db<F, T>(body: F) -> Result<T, ElevatedError>
where
    F: std::future::Future<Output = Result<T, ElevatedError>>,
{
    let runtime = tokio::runtime::Runtime::new()
        .map_err(|err| ElevatedError::RuntimeStartFailed(err.to_string()))?;
    runtime.block_on(body)
}

/// [`ElevatedAction::ResetPassword`]の本体。実行前に対象（profile・
/// ユーザー名）を表示して確認を求め、承諾されたら標準入力から新パスワードを
/// 2回読んで一致を確かめた上で[`reset_user_password`]を呼ぶ（モジュール doc
/// 「ロックダウン回復アクション」節参照）。
#[cfg(windows)]
fn reset_password_action(username: &str, profile_id: Option<&str>) -> Result<(), ElevatedError> {
    let paths = resolve_target_profile_paths(profile_id)?;

    println!(
        "banto-hub-elev: profile '{}' のユーザー '{username}' のパスワードを再設定します。",
        paths.profile_id
    );
    if !windows_impl::confirm("続行しますか？")? {
        return Err(ElevatedError::ConfirmationDeclined);
    }

    let password = windows_impl::read_password("新しいパスワード: ")?;
    let password_confirm = windows_impl::read_password("新しいパスワード（確認）: ")?;
    if password != password_confirm {
        return Err(ElevatedError::PasswordMismatch);
    }

    block_on_db(async move {
        let pool = crate::db::init_db(&paths.db_path).await?;
        reset_user_password(&pool, username, &password).await
    })
}

/// [`ElevatedAction::RevertToCommissioning`]の本体。実行前に「認証が不要に
/// なる」「loopback バインドでないと次回起動できなくなる」（§5.6 制約1が
/// 再び効く）ことを警告表示して確認を求め、承諾されたら
/// [`revert_to_commissioning_with_audit`]を呼ぶ（`actor`は
/// [`windows_impl::current_user_name`]で解決したローカル OS ユーザー名 -
/// モジュール doc「監査ログ」節参照）。
#[cfg(windows)]
fn revert_to_commissioning_action(profile_id: Option<&str>) -> Result<(), ElevatedError> {
    let paths = resolve_target_profile_paths(profile_id)?;

    println!(
        "banto-hub-elev: profile '{}' を試運転モード（未ロックダウン・認証なし）へ戻します。",
        paths.profile_id
    );
    println!(
        "banto-hub-elev: 警告: 復帰後は管理 UI / 管理 REST のログインが不要になります。\
         改造・再試運転以外の目的では実行しないでください。"
    );
    println!(
        "banto-hub-elev: 警告: 復帰後も server.bind / BANTO_BIND が loopback（127.0.0.1 等）\
         でないままだと、次回起動時に banto-hub は起動を拒否します\
         （docs/tag-server-design.md §5.6 制約1）。先に loopback へ変更するか、\
         復帰直後に変更してください。"
    );
    if !windows_impl::confirm("本当に試運転モードへ戻しますか？")? {
        return Err(ElevatedError::ConfirmationDeclined);
    }

    let actor = windows_impl::current_user_name()?;

    block_on_db(async move {
        let pool = crate::db::init_db(&paths.db_path).await?;
        revert_to_commissioning_with_audit(&pool, &actor).await
    })
}

/// `bin/banto-hub-elev.rs`から呼ばれる唯一のエントリポイント - パース済みの
/// [`ElevatedAction`]と残りの CLI 引数を受け取り、action ごとの引数個数を
/// 検証した上でディスパッチする。
///
/// `args`は action 自体を除いた残りの引数（`setup-operators`は0〜1個の
/// ユーザー名、`grant-profile-acl`は0〜2個の`[username] [profile-id]`
/// （位置引数、`profile-id`だけを指定して`username`を省略することは
/// できない）、`reset-password`は1〜2個の`<username> [profile-id]`
/// （`username`は省略不可 - モジュール doc「ロックダウン回復アクション」
/// 節参照。新パスワードは引数ではなく標準入力から読む）、
/// `revert-to-commissioning`は0〜1個の`[profile-id]`を受け付け、他の4
/// アクションは追加引数を受け付けない - モジュール doc の action 表参照）。
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
        ElevatedAction::ResetPassword => {
            if args.is_empty() || args.len() > 2 {
                return Err(ElevatedError::InvalidArgs(format!(
                    "'{action}' の引数は<username> [profile-id]（1〜2個）ですが{}個受け取りました",
                    args.len()
                )));
            }
            let username = args[0].as_str();
            let profile_id = args.get(1).map(String::as_str);
            reset_password_action(username, profile_id)
        }
        ElevatedAction::RevertToCommissioning => {
            if args.len() > 1 {
                return Err(too_many_args(action, args.len(), "0〜1"));
            }
            let profile_id = args.first().map(String::as_str);
            revert_to_commissioning_action(profile_id)
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
        ERROR_MEMBER_IN_ALIAS, HLOCAL, INVALID_HANDLE_VALUE,
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
    use windows_sys::Win32::System::Console::{
        GetConsoleMode, GetStdHandle, SetConsoleMode, ENABLE_ECHO_INPUT, STD_INPUT_HANDLE,
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

    /// `reset-password`/`revert-to-commissioning`共通の確認プロンプト
    /// （モジュール doc「確認プロンプト」節参照）。`y`/`yes`（大文字小文字
    /// 不問）のみ承諾とみなし、それ以外の入力・空入力は全て拒否
    /// （既定 No - 取り返しにくい操作なので、曖昧な入力を承諾扱いにしない）。
    pub(super) fn confirm(prompt: &str) -> Result<bool, ElevatedError> {
        use std::io::Write;
        eprint!("banto-hub-elev: {prompt} [y/N]: ");
        std::io::stderr()
            .flush()
            .map_err(|err| ElevatedError::ConsoleIoFailed(err.to_string()))?;

        let mut line = String::new();
        std::io::stdin()
            .read_line(&mut line)
            .map_err(|err| ElevatedError::ConsoleIoFailed(err.to_string()))?;

        let answer = line.trim().to_ascii_lowercase();
        Ok(answer == "y" || answer == "yes")
    }

    /// 標準入力から1行、コンソールのエコーを止めた状態で読む
    /// （`reset-password`の新パスワード入力専用 - モジュール doc
    /// 「パスワードの受け渡し方法」節参照）。パスワード自体はどの引数にも
    /// 含めない・ログにも残さない - この関数の戻り値は呼び出し元が
    /// `hash_password`に渡すだけで、それ以外の用途（表示・保存）に使っては
    /// ならない。
    ///
    /// エコー抑止に使うのは`SetConsoleMode`の`ENABLE_ECHO_INPUT`ビット
    /// クリアのみ（`ENABLE_LINE_INPUT`は残す - `std::io::Stdin::read_line`が
    /// 行バッファリングに依存しているため、これを外すと1文字ずつしか
    /// 読めなくなる）。標準入力が実コンソールでない（パイプ・リダイレクト）
    /// 場合は`GetConsoleMode`が失敗するので、その場合はエコー抑止を諦めて
    /// 警告を出した上でそのまま読む（`reset-password`は対話利用のみを
    /// 想定しているが、エコー抑止の失敗だけで操作全体を止めるほどの
    /// 重大度ではないと判断した - 「読めなくなる」より「画面に見えるが
    /// 読める」方を優先する）。
    pub(super) fn read_password(prompt: &str) -> Result<String, ElevatedError> {
        use std::io::Write;
        eprint!("banto-hub-elev: {prompt}");
        std::io::stderr()
            .flush()
            .map_err(|err| ElevatedError::ConsoleIoFailed(err.to_string()))?;

        // SAFETY: `STD_INPUT_HANDLE`は疑似ハンドル値であり、呼び出し自体に
        // 前提条件は無い（`GetStdHandle`の契約）。
        let handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
        let mut original_mode: u32 = 0;
        // SAFETY: `handle`が`INVALID_HANDLE_VALUE`でないことを確認してから
        // 渡す。標準入力が実コンソールでない場合はここが失敗しうる
        // （関数 doc参照）。
        let echo_suppressed = handle != INVALID_HANDLE_VALUE
            && unsafe { GetConsoleMode(handle, &mut original_mode) } != 0
            && unsafe { SetConsoleMode(handle, original_mode & !ENABLE_ECHO_INPUT) } != 0;
        if !echo_suppressed {
            eprintln!(
                "\nbanto-hub-elev: 警告: コンソールのエコー抑止に失敗しました。\
                 入力したパスワードが画面に表示されます"
            );
        }

        let mut line = String::new();
        let read_result = std::io::stdin().read_line(&mut line);

        if echo_suppressed {
            // SAFETY: `handle`は上で`GetConsoleMode`が成功した同じ有効な
            // ハンドル。
            unsafe { SetConsoleMode(handle, original_mode) };
            // エコー抑止中は Enter を押しても改行が画面に出ないため、ここで
            // 手動で改行して以降の出力とずれないようにする。
            eprintln!();
        }

        read_result.map_err(|err| ElevatedError::ConsoleIoFailed(err.to_string()))?;
        Ok(line.trim_end_matches(['\r', '\n']).to_string())
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
        assert_eq!(
            ElevatedAction::parse("reset-password"),
            Some(ElevatedAction::ResetPassword)
        );
        assert_eq!(
            ElevatedAction::parse("revert-to-commissioning"),
            Some(ElevatedAction::RevertToCommissioning)
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
        assert_eq!(original_len, 9, "固定アクションは9種類のはず");
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

    /// `reset-password`は`<username>`が必須 - 0個（省略）はエラー
    /// （引数バリデーションのみを検証するテストなので、標準入力の読み取り
    /// には到達しない前に弾かれることを確認する）。
    #[cfg(windows)]
    #[test]
    fn run_rejects_reset_password_without_username() {
        let err = run(ElevatedAction::ResetPassword, &[])
            .expect_err("reset-password requires a username");
        assert!(matches!(err, ElevatedError::InvalidArgs(_)));
    }

    #[cfg(windows)]
    #[test]
    fn run_rejects_too_many_args_for_reset_password() {
        let extra = vec![
            "user-a".to_string(),
            "profile-a".to_string(),
            "unexpected-third".to_string(),
        ];
        let err = run(ElevatedAction::ResetPassword, &extra)
            .expect_err("reset-password only accepts 1-2 args");
        assert!(matches!(err, ElevatedError::InvalidArgs(_)));
    }

    #[cfg(windows)]
    #[test]
    fn run_rejects_too_many_args_for_revert_to_commissioning() {
        let extra = vec!["profile-a".to_string(), "unexpected-second".to_string()];
        let err = run(ElevatedAction::RevertToCommissioning, &extra)
            .expect_err("revert-to-commissioning only accepts 0-1 args");
        assert!(matches!(err, ElevatedError::InvalidArgs(_)));
    }

    // --- reset_user_password / revert_to_commissioning_with_audit -------
    // Win32 コンソール入出力を含まない中核処理（このファイル冒頭「中核処理の
    // 分離」節参照）なので `#[cfg(windows)]` を掛けず、非 Windows でも
    // `cargo test` で検証できる。

    /// [`crate::users::UsersService::verify`]（内部で既存の
    /// `verify_password` を使う）が新パスワードでは通り、旧パスワードでは
    /// 通らなくなることを確認する - `verify_password`自体は`users.rs`内の
    /// 非`pub`関数のためここから直接は呼べない、`verify`を経由することで
    /// 実質的に同じ検証になる。
    #[tokio::test]
    async fn reset_user_password_lets_new_password_verify() {
        let pool = crate::db::migrate_memory().await.expect("migrate_memory");
        let users = crate::users::UsersService::new(pool.clone());
        users
            .setup_first_user("owner", "old-password-1", "オーナー")
            .await
            .expect("setup_first_user");

        reset_user_password(&pool, "owner", "new-password-1")
            .await
            .expect("reset_user_password should succeed for an existing user");

        assert!(
            users
                .verify("owner", "new-password-1")
                .await
                .expect("verify")
                .is_some(),
            "new password must verify"
        );
        assert!(
            users
                .verify("owner", "old-password-1")
                .await
                .expect("verify")
                .is_none(),
            "old password must no longer verify"
        );
    }

    #[tokio::test]
    async fn reset_user_password_errors_for_unknown_username() {
        let pool = crate::db::migrate_memory().await.expect("migrate_memory");

        let err = reset_user_password(&pool, "no-such-user", "irrelevant123")
            .await
            .expect_err("unknown username should fail");
        assert!(matches!(err, ElevatedError::UserNotFound(username) if username == "no-such-user"));
    }

    #[tokio::test]
    async fn revert_to_commissioning_with_audit_flips_locked_down_state_back() {
        let pool = crate::db::migrate_memory().await.expect("migrate_memory");
        let settings = crate::settings::SettingsService::new(pool.clone());
        let users = crate::users::UsersService::new(pool.clone());
        users
            .setup_first_user("owner", "password123", "オーナー")
            .await
            .expect("setup_first_user");
        let commissioning =
            crate::commissioning::CommissioningService::load(settings.clone(), users.clone())
                .await
                .expect("load");
        commissioning.lock_down().await.expect("lock_down");
        assert!(
            crate::commissioning::resolve_locked_down(&settings)
                .await
                .expect("resolve_locked_down"),
            "precondition: must be locked down before reverting"
        );

        revert_to_commissioning_with_audit(&pool, "DESKTOP\\owner")
            .await
            .expect("revert_to_commissioning_with_audit should succeed");

        assert!(
            !crate::commissioning::resolve_locked_down(&settings)
                .await
                .expect("resolve_locked_down"),
            "revert must flip the persisted flag back to commissioning mode"
        );
    }

    /// 設計 §5.6 制約4「試運転モードへの復帰は監査ログに記録する」の
    /// 回帰テスト - `actor_username`にはローカル OS ユーザー名がそのまま
    /// 入り、`origin`は REST 経由の`"rest"`とは区別できる`"elev"`になる
    /// ことを確認する（モジュール doc「監査ログ」節参照）。
    #[tokio::test]
    async fn revert_to_commissioning_with_audit_records_an_audit_entry() {
        let pool = crate::db::migrate_memory().await.expect("migrate_memory");

        revert_to_commissioning_with_audit(&pool, "DESKTOP\\owner")
            .await
            .expect("revert_to_commissioning_with_audit should succeed");

        let audit = crate::audit::AuditLogService::new(pool.clone());
        let result = audit
            .list(banto_core::ListParams::default())
            .await
            .expect("list");
        assert_eq!(result.total_count, 1);
        let entry = &result.rows[0];
        assert_eq!(entry.action, "commissioning_revert");
        assert_eq!(entry.resource, "commissioning");
        assert_eq!(entry.actor_username.as_deref(), Some("DESKTOP\\owner"));
        assert_eq!(entry.origin, "elev");
        assert_eq!(entry.result, "ok");
    }

    #[tokio::test]
    async fn revert_to_commissioning_with_audit_works_even_when_already_commissioning() {
        // すでに試運転モード（未ロックダウン）の profile へ誤って
        // 実行しても、`CommissioningService::revert_to_commissioning`と
        // 同じく冪等に成功する（settings への書き込み自体は無条件更新の
        // ため失敗しない）ことを確認する - 復帰操作は「何度実行しても
        // 安全側」であるべきという §5.6 の設計思想に沿う。
        let pool = crate::db::migrate_memory().await.expect("migrate_memory");

        revert_to_commissioning_with_audit(&pool, "DESKTOP\\owner")
            .await
            .expect("revert should be a harmless no-op when already commissioning");

        assert!(!crate::commissioning::resolve_locked_down(
            &crate::settings::SettingsService::new(pool.clone())
        )
        .await
        .expect("resolve_locked_down"));
    }

    /// 実際に対話コンソールへ確認プロンプトとパスワード入力を要求し、
    /// 実プロファイルの DB を開く - 管理者権限・実プロファイル・対話
    /// コンソールが必要なため`#[ignore]`（他の実機専用テストと同じ位置づけ）。
    /// Windows 実機で `banto-hub-elev.exe reset-password <username>` を
    /// 手動実行して確認する。
    #[cfg(windows)]
    #[test]
    #[ignore = "管理者権限・実プロファイル DB・対話コンソールが必要 - Windows 実機で手動実行"]
    fn reset_password_action_runs_end_to_end() {
        reset_password_action("owner", None).expect("manual verification only");
    }

    /// 同上（`revert-to-commissioning`版）。
    #[cfg(windows)]
    #[test]
    #[ignore = "管理者権限・実プロファイル DB・対話コンソールが必要 - Windows 実機で手動実行"]
    fn revert_to_commissioning_action_runs_end_to_end() {
        revert_to_commissioning_action(None).expect("manual verification only");
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
