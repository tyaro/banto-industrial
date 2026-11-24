//! この PC からの接続を通す grant `localDesktop`（#392 段階 B の B2-1、H8、2026-10-14。
//! 設計は docs/chronogazer/chronogazer-service-design.md §3.2・§4 の B2-1・§5 の決定 8）。
//!
//! サービス（B3）では画面（シェル）とエンジンが別のプロセスになり、シェルはエンジンの
//! loopback の listener へ HTTP で入る。そのときにログイン画面を挟まずに入れる口が
//! この grant（banto ADR-0017 の 3 種類目。前例は閲覧公開 `publicViewer` と banto-hub の
//! 試運転 `commissioning`）。
//!
//! # 発行の条件（レビューはここから）
//!
//! 1. **listener**: `chronogazer-server` の**常時の listener**（`127.0.0.1`、または
//!    `0.0.0.0` にまとめた 1 本）のルーターだけ（[`LocalAccess::always_on_listener`]。
//!    `crate::engine::OpenedEngine::with_loopback_local_access`）。**本当の境界は下の
//!    2.・3. の要求ごとの確認**で、`0.0.0.0` の 1 本でも LAN からの要求は peer が LAN
//!    なので閉じる（2026-10-14 オーナー決定。最初は loopback に bind できた listener
//!    だけにしていた）。listener の条件は多層の防御として「LAN 専用の listener（LAN の
//!    slot）では常に閉じる」だけを残す。デスクトップの LAN サーバーと `banto-serve`
//!    も閉じたまま（デスクトップの画面は Tauri の経路で入り、LAN サーバーは LAN の
//!    ブラウザーのためのもの。`banto-serve` は開発・E2E 用で、サービスの代わりにしない）。
//!    閉じている listener でも kind は登録し、条件を常に偽にする（`404` ではなく `403`、
//!    `status.grants.localDesktop` は `false`）。
//! 2. **peer が loopback**（banto の `GrantSpec::require_loopback_peer`。peer 不明は拒否）。
//! 3. **Host が loopback**（`127.0.0.1`・`[::1]`・`localhost`、ポートは任意。DNS rebinding
//!    対策）。banto の grant は peer しか見ないので、[`local_access_middleware`] が
//!    `GET /api/auth/status` と `POST /api/auth/grant/*` の前で
//!    `banto_server::request_is_loopback_local`（peer と Host の両方）を確かめ、偽なら
//!    要求から `ConnectInfo` を外す。banto の判定（status と発行で共有の 1 つの関数）は
//!    peer 不明として扱うので、**status と発行が食い違わない**まま両方が閉じる
//!    （`publicViewer` は peer を見ないので影響しない）。banto 本体の変更（上流の PR）は
//!    要らない。
//! 4. **権限**（banto v8、2026-11-24 オーナー決定 = docs/banto-v8-upgrade-plan.md §2 の 3）:
//!    「未初期化（アカウント 0 件）」は無くなった（新しい DB はログインなしで始まる）。
//!    起動時に**ログインなし**（`auth.disabled = true`）なら、そのモードのロール
//!    （`auth.disabled_role`、既定 admin。デスクトップのログインなし = 組み込みの `admin` と
//!    同じ扱い）。条件は「いまもログインなしで同じロール」、またはログイン必須へ切り替えた
//!    後は「いまの設定 `auth.local_grant` が起動時のロールと同じ」をリクエストのたびに読む。
//!    起動時に**ログイン必須**なら設定 `auth.local_grant`（`off`/`viewer`/`editor`/`admin`、
//!    **既定 off**、決定 8）の値。条件は「いまもログイン必須 かつ いまの設定が起動時の値と
//!    同じ」。peer と Host の loopback の条件（1.〜3.）は変えない（LAN 側へ権限を広げない）。
//! 5. **予約名のアカウントが無い**（#600 の Copilot レビュー。ログインなしの間も見る）: 合成の
//!    identity と同じ名前（[`RESERVED_USERNAMES`]）のアカウントがあると、監査で発行した
//!    セッションの操作と区別できないので、条件を偽にする（status も `false`）。発行の
//!    要求には、別の名前のアカウントに移るよう伝える `409` を返す（[`has_reserved_accounts`]）。
//!
//! # ロールの変更は再起動で反映（決定 8）
//!
//! banto の `GrantSpec.identity` は**登録時に固定**（発行口はクライアントから identity も
//! role も受け取らない。ADR-0017 の「昇格の経路が無い」性質）。ルーターは起動時に 1 回
//! 組むので、発行する role は起動時の値に決まる。設定を変えたときは:
//!
//! - **閉じる方向（`off` にする・別の role にする）はすぐ効く**: 条件が「いまの設定 ==
//!   起動時の値」を読むので、違えばその場で発行をやめ、設定の保存（REST の
//!   `PUT /api/settings/auth`）の直後に発行済みのトークンを失効させる（ADR-0017 の
//!   順序: 閉じた条件を保存してから失効）。
//! - **開く方向（新しい role で発行する）は次の起動から**。応答の
//!   `localGrantRestartRequired` で知らせる。
//! - ログインなしで起動した後にログイン必須へ切り替えたときは、設定が起動時のロールと
//!   同じならそのまま入れる（開く方向でも再起動は要らない）。ほかのロールで入れるのは
//!   次の起動から（それまでは閉じる。既定 off なら変わらない）。
//!
//! # 初回セットアップは無い（banto v8、ADR-0034 §5）
//!
//! `POST /api/auth/setup` と、それに掛けていた Host の確認・未初期化の間の admin の
//! トークンの失効・初回セットアップでの `localGrant` の選択・「未初期化の間に grant から
//! 最初のアカウントを作らせない」断りは、上流が初回セットアップを無くしたので消した。
//! アカウントはログインなしの間にユーザー管理で作り、ログイン必須への切り替えは
//! banto の `SettingsService::set_auth_config`（管理者のアカウントが要る）が守る。
//!
//! # 後処理は応答から切り離す（#600 の監査 F1・F2・F4）
//!
//! hyper 1.x は応答を待つ間に相手の FIN を読むと service の future を捨てる。発行とその
//! 監査、設定の REST の適用（`crate::server_admin`）は
//! [`run_detached`] で別のタスクにして、接続を切られても最後まで完結させる。
//!
//! # 監査
//!
//! 発行（banto の `grant_router` は監査しない - キオスクの再読み込みで埋もれるため）に
//! 成功したら `login`（`resource: auth`、`detail: { via: "localGrant" }`、actor は合成の
//! `localDesktop` と発行した role、`origin: rest`）を残す。kind（percent decode した
//! パス）と role（起動時の形）は発行の前に決め、発行されたトークンを読み直さない
//! ので、発行の直後に FIFO で追い出されても発行と監査は 1 対 1（#601）。
//! デスクトップの自動ログインの `detail: { via: "autologin" }` と同じ作法。admin 相当になりうる口なので、発行の数は
//! `max_sessions`（[`LOCAL_GRANT_MAX_SESSIONS`]）で小さく抑え、発行のたびに記録する。
//! その後の操作は普通のセッションと同じく `actor_username: "localDesktop"` で記録される
//! （identity の名前は kind と同じ。banto-hub の試運転の `commissioning` と同じ綴りの
//! 作法。ログイン不要モードの合成のセッション `local` と監査で取り違えないよう、
//! 2026-10-14 オーナー決定で `local` から変えた）。
//!
//! # 守れないもの
//!
//! - **同じ PC の reverse proxy**: peer が常にプロキシになり、Host も上流のアドレスに
//!   書き換えられることが多いので、この確認は役に立たない。サービスの前に置かない
//!   （banto ADR-0017 §6・設計書 §6 と同じ運用条件）。
//! - 同じ PC の他の利用者・他のアプリ: loopback から入れる者は誰でも発行を受けられる。
//!   そのため初期化後の既定は `off`（初回セットアップの選択は、それを承知で選ぶ口）。

use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use banto_core::{BantoError, ErrorBody};
use banto_server::{
    request_is_loopback_local, AuthState, GrantCondition, GrantKind, GrantSpec, Identity,
};

use tokio::sync::Mutex as AsyncMutex;

use crate::audit::{AuditEntry, AuditLogService};
use crate::settings::SettingsService;
use crate::users::{Role, UsersService};

/// grant の kind（URL `POST /api/auth/grant/localDesktop`・`status.grants.localDesktop`・
/// `identity.kind`・画面の `SessionKind` で同じ文字列）。
pub const LOCAL_DESKTOP_GRANT_KIND: &str = "localDesktop";

/// grant のセッションの合成の identity の `id`（kind と同じ。決定 8 の「合成の `local`」を、
/// ログイン不要モードの `local` と見分けるため 2026-10-14 オーナー決定で変えた）。
pub const LOCAL_GRANT_ACTOR_ID: &str = "localDesktop";

/// 初期化後の権限の設定キー（`auth.` の名前空間 - デスクトップの汎用の
/// `settings_set` は書けない。書くのは REST の `PUT /api/settings/auth` だけ）。
pub const LOCAL_GRANT_SETTING_KEY: &str = "auth.local_grant";

/// 種別ごとの発行の上限（ADR-0017 §5「admin 相当のトークンの数は少ないほどよい」。
/// banto-hub の試運転と同じ 16）。シェルの窓・タブの数として十分に大きく、超えたら
/// 最古が失効する。
pub const LOCAL_GRANT_MAX_SESSIONS: usize = 16;

/// banto v8（ADR-0034 §4）: 組み込みの `admin` に加えて予約する名前（[`RESERVED_USERNAMES`]）。
/// `UsersService::with_reserved_usernames` に渡すと、banto のサービスが作成を断り、
/// 起動時の検査（`UsersService::check_startup`）が同じ名前のアカウントのある DB を止める。
pub fn reserved_usernames() -> crate::users::ReservedUsernames {
    crate::users::ReservedUsernames::new(RESERVED_USERNAMES)
}

/// アカウント名として使えない名前（#600 のレビュー、2026-10-14）: 資格情報なしの合成の
/// セッションの identity と同じ名前。監査の行は `actor_username`/`actor_role` しか
/// 持たないので、同じ名前のアカウントがあると「誰が操作したか」を区別できない。
/// `local` = ログイン不要モード（デスクトップ）、`localDesktop` = この PC からの接続、
/// `public` = 閲覧公開（banto の `PUBLIC_VIEWER_ID`）。上流（banto）の検証は長さだけ
/// なので、ChronoGazer の経路（REST の setup・`POST /api/users`、デスクトップの
/// `auth_setup`・`users_create`）で banto のサービスを呼ぶ前に断る。比べ方は前後の空白を
/// 除き（banto も除いて保存する）、大文字小文字を区別しない。アカウント名の変更の口は
/// 無い（`PUT /api/users/{id}` は表示名と role だけ）。
pub const RESERVED_USERNAMES: [&str; 3] = [
    "local",
    LOCAL_GRANT_ACTOR_ID,
    banto_server::PUBLIC_VIEWER_ID,
];

/// `username` が予約名か（[`RESERVED_USERNAMES`]）。
pub fn is_reserved_username(username: &str) -> bool {
    let name = username.trim();
    RESERVED_USERNAMES
        .iter()
        .any(|reserved| reserved.eq_ignore_ascii_case(name))
}

/// 予約名なら、banto のアカウント名の検証と同じ形（`422`、欄 `username`）の誤りを返す
/// （画面は既存の欄の誤りとして出す）。
pub fn check_username_not_reserved(username: &str) -> Result<(), BantoError> {
    if is_reserved_username(username) {
        return Err(BantoError::Validation {
            field_errors: vec![banto_core::FieldError {
                field: "username".to_string(),
                message: format!(
                    "「{}」は内部で使う名前のため、アカウント名にできません",
                    username.trim()
                ),
            }],
        });
    }
    Ok(())
}

/// 既にある予約名のアカウント（起動時の警告と [`has_reserved_accounts`] 用。
/// [`RESERVED_USERNAMES`] の前に作られた DB。自動では直さない）。
pub fn reserved_accounts<'a>(usernames: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    usernames
        .into_iter()
        .filter(|name| is_reserved_username(name))
        .map(str::to_owned)
        .collect()
}

/// 予約名のアカウントが 1 つでもあるか（#600 の Copilot レビュー、fail closed）。あれば
/// 初期化後の `localDesktop` の発行を断る（[`local_desktop_grant_spec`] の条件 - status と
/// 発行で共有 - と、[`local_access_middleware`] の説明つきの `409`）: 発行したセッションの
/// 操作は `actor_username: "localDesktop"` で監査に残るので、同じ名前のアカウントが
/// あると、どちらの操作か区別できない。
pub async fn has_reserved_accounts(users: &UsersService) -> Result<bool, BantoError> {
    let accounts = users.list_users().await?;
    Ok(!reserved_accounts(accounts.iter().map(|account| account.username.as_str())).is_empty())
}

/// 予約名のアカウントがあるときの、初期化後の発行を断る理由（[`has_reserved_accounts`]）。
const RESERVED_ACCOUNT_REFUSAL: &str = "この PC からの接続（localDesktop）は、内部で使う名前（local・localDesktop・public）のアカウントがあるため発行できません（監査ログで区別できないため）。別の名前の管理者のアカウントを作ってそちらに移り、その名前のアカウントを削除してください";

/// [`LOCAL_DESKTOP_GRANT_KIND`] を `GrantKind` として返す。
pub fn local_desktop_grant_kind() -> GrantKind {
    GrantKind::new(LOCAL_DESKTOP_GRANT_KIND).expect("`localDesktop` は grant kind の文法に合う")
}

// --- 設定 `auth.local_grant` --------------------------------------------------

/// 設定 `auth.local_grant` の値。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalGrantSetting {
    /// 発行しない（既定）。
    Off,
    /// この role で発行する。
    Role(Role),
}

impl LocalGrantSetting {
    /// 保存の形（`off`/`viewer`/`editor`/`admin`）。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Role(role) => role.as_str(),
        }
    }

    /// 保存されている値の読み方: 未設定・空・読めない値は **`Off`**（fail closed。
    /// ログイン不要モードの `auth.disabled_role` が読めない値を admin に倒すのとは
    /// 逆 - こちらは「認証なしで誰に何を渡すか」なので、迷ったら渡さない）。
    pub fn parse_stored(raw: Option<&str>) -> Self {
        match raw.map(str::trim) {
            Some(text) if !text.is_empty() => Self::parse(text).unwrap_or(Self::Off),
            _ => Self::Off,
        }
    }

    /// 受け付ける入力（REST の `PUT`）の読み方。知らない値は `Err`。
    pub fn parse(text: &str) -> Result<Self, BantoError> {
        if text == "off" {
            return Ok(Self::Off);
        }
        Role::from_str(text).map(Self::Role).map_err(|_| {
            BantoError::BadRequest(format!(
                "localGrant は off・viewer・editor・admin のどれかにしてください（{text}）"
            ))
        })
    }
}

/// 保存済みの設定を読む（[`LocalGrantSetting::parse_stored`]）。
pub async fn read_setting(settings: &SettingsService) -> Result<LocalGrantSetting, BantoError> {
    Ok(LocalGrantSetting::parse_stored(
        settings.get(LOCAL_GRANT_SETTING_KEY).await?.as_deref(),
    ))
}

/// 設定を保存する（失効は呼び出し側が保存の**後**に行う - ADR-0017 の順序）。
pub async fn write_setting(
    settings: &SettingsService,
    value: LocalGrantSetting,
) -> Result<(), BantoError> {
    settings.set(LOCAL_GRANT_SETTING_KEY, value.as_str()).await
}

// --- 起動時に決まる発行の形 ----------------------------------------------------

/// 起動時に決まる、このエンジンが発行する形（エンジン全体で 1 つ。listener ごとの
/// 開け閉めは [`LocalAccess`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalGrantStart {
    /// この host は発行しない（デスクトップ - 画面は Tauri の経路で入る - と
    /// `banto-serve`）。
    Unsupported,
    /// 起動時にログインなし（banto v8 の新しい DB の既定、`auth.disabled = true`）。
    /// そのモードのロール（`auth.disabled_role`、既定 admin）で発行する。ログインなしの
    /// 間はデスクトップと同じく組み込みの `admin` として扱う（docs/banto-v8-upgrade-plan.md
    /// §2 の 3）。ログイン必須へ切り替えた後は、設定 `auth.local_grant` がこのロールの
    /// ときだけ開いたまま（それ以外は閉じる）。
    NoLogin(Role),
    /// 起動時にログイン必須。この設定の値で発行する（`Off` なら発行しない）。
    Initialized(LocalGrantSetting),
}

impl LocalGrantStart {
    /// 起動時に決める（`chronogazer-server`）。読めなければ発行しない形
    /// （`Initialized(Off)`）に倒し、理由をログに出す（fail closed）。
    pub async fn resolve(settings: &SettingsService, label: &str) -> Self {
        // banto v8（ADR-0034 §5）: 「未初期化（アカウント 0 件）」は無くなった。新しい DB は
        // ログインなし（`auth.disabled = true`）で始まる。認証モードは既定で埋めずに読む。
        let auth = match settings.auth_config().await {
            Ok(auth) => auth,
            Err(err) => {
                eprintln!(
                    "{label}: 認証モードを読めなかったため、この PC からの接続（localDesktop）は発行しません: {err}"
                );
                return Self::Initialized(LocalGrantSetting::Off);
            }
        };
        if auth.disabled {
            return Self::NoLogin(auth.disabled_role);
        }
        match read_setting(settings).await {
            Ok(setting) => Self::Initialized(setting),
            Err(err) => {
                eprintln!(
                    "{label}: 設定 {LOCAL_GRANT_SETTING_KEY} を読めなかったため、この PC からの接続（localDesktop）は発行しません: {err}"
                );
                Self::Initialized(LocalGrantSetting::Off)
            }
        }
    }

    /// 発行するセッションの role（発行しない形では `None`）。
    pub fn issued_role(&self) -> Option<Role> {
        match self {
            Self::Unsupported | Self::Initialized(LocalGrantSetting::Off) => None,
            Self::NoLogin(role) => Some(*role),
            Self::Initialized(LocalGrantSetting::Role(role)) => Some(*role),
        }
    }

    /// 保存済みの設定 `saved` で発行するには再起動が要るか（モジュール doc「ロールの
    /// 変更は再起動で反映」）。`login_required_now` はいまログイン必須か。
    pub fn restart_required(&self, saved: LocalGrantSetting, login_required_now: bool) -> bool {
        match self {
            Self::Unsupported => false,
            // 起動時のロールと同じ設定なら、ログイン必須に切り替えた後もそのまま入れる
            // （条件は [`local_desktop_grant_spec`]）。
            Self::NoLogin(started) => {
                login_required_now
                    && saved != LocalGrantSetting::Off
                    && saved != LocalGrantSetting::Role(*started)
            }
            // `off` にする（閉じる）のはすぐ効くので再起動は要らない（#600 のレビュー）。
            Self::Initialized(started) => *started != saved && saved != LocalGrantSetting::Off,
        }
    }
}

/// listener ごとの開け閉め（ルーターを組むときに渡す）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalAccess {
    /// エンジンの起動時に決まった発行の形。
    pub start: LocalGrantStart,
    /// このルーターが `chronogazer-server` の常時の listener（`127.0.0.1` か、`0.0.0.0`
    /// にまとめた 1 本）のものか。`false`（LAN 専用の listener など）なら grant は常に
    /// 閉じる。`true` でも、開くのは要求ごとに peer と Host の両方が loopback のとき
    /// だけ（モジュール doc）。
    pub always_on_listener: bool,
}

impl LocalAccess {
    /// 発行しないルーター（LAN の listener・デスクトップ・`banto-serve`・テスト）。
    pub fn closed() -> Self {
        Self {
            start: LocalGrantStart::Unsupported,
            always_on_listener: false,
        }
    }

    /// このルーターで grant が開きうるか（条件はリクエストのたびに別に読む）。
    pub fn may_issue(&self) -> bool {
        self.always_on_listener && self.start.issued_role().is_some()
    }
}

/// 合成の identity（`role` は起動時に決まる。発行しない形でも登録には要るので、
/// そのときは最も低い viewer を置く - 条件が常に偽なので発行されない）。
fn synthetic_identity(role: Role) -> Identity {
    Identity {
        id: LOCAL_GRANT_ACTOR_ID.to_string(),
        name: "この PC".to_string(),
        role: role.as_str().to_string(),
        // grant の固定 identity はアカウントではない（banto v8 C3）。
        account_id: None,
    }
}

/// `localDesktop` の grant の仕様（モジュール doc「発行の条件」）。
pub fn local_desktop_grant_spec(
    access: LocalAccess,
    users: UsersService,
    settings: SettingsService,
) -> GrantSpec {
    let role = access.start.issued_role();
    let enabled: GrantCondition = match (access.may_issue(), access.start) {
        // banto v8: ログインなしで起動したら、ログインなしのまま（同じロール）の間は開く。
        // ログイン必須に切り替えた後は、設定 `auth.local_grant` が起動時のロールと同じ
        // ときだけ開いたまま（モジュール doc の 4.）。どちらも予約名のアカウントがあれば
        // 閉じる（ログインなしの間もユーザー管理でアカウントを作れるため）。
        (true, LocalGrantStart::NoLogin(started)) => Arc::new(move || {
            let (users, settings) = (users.clone(), settings.clone());
            Box::pin(async move {
                let auth = settings.auth_config().await?;
                let open = if auth.disabled {
                    auth.disabled_role == started
                } else {
                    read_setting(&settings).await? == LocalGrantSetting::Role(started)
                };
                Ok(open && !has_reserved_accounts(&users).await?)
            })
        }),
        (true, LocalGrantStart::Initialized(started)) => Arc::new(move || {
            let (users, settings) = (users.clone(), settings.clone());
            Box::pin(async move {
                Ok(!settings.auth_config().await?.disabled
                    && read_setting(&settings).await? == started
                    && !has_reserved_accounts(&users).await?)
            })
        }),
        _ => Arc::new(|| Box::pin(async { Ok(false) })),
    };
    let mut spec = GrantSpec::new(
        local_desktop_grant_kind(),
        synthetic_identity(role.unwrap_or(Role::Viewer)),
        enabled,
    );
    spec.max_sessions = LOCAL_GRANT_MAX_SESSIONS;
    spec.require_loopback_peer = true;
    spec
}

// --- ミドルウェア ----------------------------------------------------------------

/// [`local_access_middleware`] の状態。
#[derive(Clone)]
pub struct LocalAccessState {
    pub access: LocalAccess,
    pub users: UsersService,
    pub auth: AuthState,
    pub audit: AuditLogService,
    /// 初回セットアップでの選択（モジュール doc）を保存する先。
    pub settings: SettingsService,
    /// `PUT /api/settings/auth` と同じ lock（`crate::rest::HostAccess::settings_lock`）。
    pub settings_lock: Arc<AsyncMutex<()>>,
}

const STATUS_PATH: &str = "/api/auth/status";
const GRANT_PREFIX: &str = "/api/auth/grant/";
const USERS_PATH: &str = "/api/users";
/// アカウントを作る本文の上限（名前・パスワード・表示名だけなので十分に大きい）。
const ACCOUNT_BODY_LIMIT: usize = 64 * 1024;

/// `/api` 全体に掛ける前段（モジュール doc）:
///
/// - `GET /api/auth/status`・`POST /api/auth/grant/*`: peer と Host の両方が loopback で
///   なければ `ConnectInfo` を外す（banto の判定が peer 不明として閉じる）。
/// - `POST /api/users`: 予約名（[`RESERVED_USERNAMES`]）のアカウントは作らせない。
///   （banto v8 で初回セットアップ `POST /api/auth/setup` は無くなった。）
/// - `POST /api/auth/grant/localDesktop` の成功: 監査に `login`（`via: localGrant`）。
pub async fn local_access_middleware(
    State(state): State<LocalAccessState>,
    mut req: Request,
    next: Next,
) -> Response {
    let path = req.uri().path().to_string();
    let is_post = req.method() == Method::POST;
    let local = request_is_loopback_local(&req);

    if (path == STATUS_PATH || path.starts_with(GRANT_PREFIX)) && !local {
        req.extensions_mut().remove::<ConnectInfo<SocketAddr>>();
    }

    // アカウントを作る経路では予約名を断る（[`RESERVED_USERNAMES`]）。
    if is_post && path == USERS_PATH {
        let (parts, body) = req.into_parts();
        let bytes = match axum::body::to_bytes(body, ACCOUNT_BODY_LIMIT).await {
            Ok(bytes) => bytes,
            Err(_) => {
                return banto_server::ApiError(BantoError::BadRequest(
                    "本文が大きすぎるか、読めません".to_string(),
                ))
                .into_response()
            }
        };
        // JSON として読める本文は object だけを通す（#600 の監査 F3）: 上流の
        // `SetupRequest`・`CreateUserRequest` は普通の `derive(Deserialize)` なので serde の
        // 配列の表現（`["localDesktop", "pw", "x"]`）も受理し、`username` の欄だけを見ると
        // 予約名の検査を素通りする。JSON として読めない本文も `400` で断る（fail closed。
        // #600 のオーナーレビュー P2）: `serde_json::Value` の読み手には再帰の上限（128）が
        // あるが、上流は知らない欄を `IgnoredAny` で読み飛ばすのでその上限に当たらない。
        // 知らない欄を深い入れ子にした本文はここでは読めず、上流では読めるので、上流に
        // 任せると予約名の検査を素通りしていた。
        match serde_json::from_slice::<serde_json::Value>(&bytes) {
            Ok(serde_json::Value::Object(object)) => {
                if let Some(username) = object.get("username").and_then(|value| value.as_str()) {
                    if let Err(err) = check_username_not_reserved(username) {
                        return banto_server::ApiError(err).into_response();
                    }
                }
            }
            Ok(_) => {
                return banto_server::ApiError(BantoError::BadRequest(
                    "本文は JSON の object（{\"username\": ...} の形）で送ってください".to_string(),
                ))
                .into_response()
            }
            Err(_) => {
                return banto_server::ApiError(BantoError::BadRequest(
                    "本文を JSON として読めません".to_string(),
                ))
                .into_response()
            }
        }
        req = Request::from_parts(parts, axum::body::Body::from(bytes));
    }

    // 発行の監査は、生のパスの文字列ではなく **percent decode した kind** で判断する
    // （#600 のオーナーレビュー P2）: banto の発行口は `Path<String>` で kind を
    // percent decode するので、`/api/auth/grant/local%44esktop` でも `localDesktop` が
    // 発行される。生の文字列と比べると、その別表記で発行の監査が抜けていた。
    // （peer と Host の確認は接頭辞 `/api/auth/grant/` で掛けるので別表記でも素通り
    // しない。axum のルーティングは decode しない生のパスで行うので、接頭辞・status・
    // setup・users を別表記にした要求はどのルートにも当たらない。）
    // 監査に使う kind と role は**発行の前に**決める（#601、[`local_desktop_issue_role`]）:
    // 以前は応答のトークンをもう一度 `authenticate` してセッションの kind と role を
    // 読んでいたので、同時の発行で FIFO の上限を超えて先に追い出されたトークンでは
    // `login` が欠けていた。
    let is_grant_issue = is_post && path.starts_with(GRANT_PREFIX);
    let issued_role = if is_grant_issue {
        local_desktop_issue_role(state.access, &path[GRANT_PREFIX.len()..])
    } else {
        None
    };

    // 予約名のアカウントがあれば、初期化後の `localDesktop` の発行を説明つきの `409` で
    // 断る（#600 の Copilot レビュー、[`has_reserved_accounts`]）。断る本体は条件の側
    // （status と発行で共有）にあり、ここは理由を伝えるだけ。この PC からの要求だけに
    // 答える（LAN の peer には条件の `403` のまま、アカウントの名前の有無を漏らさない）。
    // kind は banto と同じく percent decode して比べる（別表記でも同じ説明を返す）。判定は
    // 監査と同じ `issued_role`（[`local_desktop_issue_role`]: 発行の POST・この listener が
    // 発行しうる・decode した kind が `localDesktop`）を使い、1 か所にまとめる（PR #649 の
    // オーナー Nit 1。挙動は同じ - `may_issue` は起動時の role があることを含む）。
    // 未初期化で起動した後の初期化済み（#392 B2-2 で、設定が `admin` なら開く）も同じ
    // なので、起動時の形は問わない（未初期化の間はアカウントが無く、常に偽）。
    if local && issued_role.is_some() {
        if let Ok(true) = has_reserved_accounts(&state.users).await {
            return (
                StatusCode::CONFLICT,
                Json(ErrorBody::Other {
                    message: RESERVED_ACCOUNT_REFUSAL.to_string(),
                }),
            )
                .into_response();
        }
    }

    if !is_grant_issue {
        return next.run(req).await;
    }
    // 発行（とその監査）・setup（とその後の失効）は、応答の future から切り離して最後まで
    // 走らせる（#600 の監査 F1・F4。[`run_detached`]）: hyper 1.x は応答を待つ間に
    // 相手の FIN を読むと service の future を捨てるので、`.await` をまたぐ後処理は
    // 接続を切られると飛ぶ（発行したのに監査が無い・初期化したのに admin のトークンが
    // 残る）。上流の処理（発行・setup の commit）ごと切り離すので、どの await 点で
    // 切られても、上流の副作用と後処理は揃って完結する。
    let detached = async move {
        let response = next.run(req).await;
        if let Some(role) = issued_role.filter(|_| response.status().is_success()) {
            record_grant_login(&state, role.as_str()).await;
        }
        response
    };
    match run_detached("localDesktop の発行", detached).await {
        Ok(response) => response,
        Err(failed) => failed.into_response(),
    }
}

/// パスの 1 区間の percent decode（banto の発行口の `Path<String>` と同じ読み方の
/// 範囲で十分: `%XX` を 1 バイトにし、UTF-8 として読めなければ `None`）。
fn percent_decode(segment: &str) -> Option<String> {
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = segment.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// `work` を応答の future から切り離して最後まで走らせ、その結果を待つ（#600 の監査
/// F1・F2・F4）。
///
/// hyper 1.x（`half_close: false`）は応答を待っている間に相手の FIN を読むと接続を閉じ、
/// service の future（= handler）を捨てる。handler の中で `.await` をまたぐ副作用
/// （listener の入れ替え・保存・失効・監査）は、そこで途中のまま捨てられる。`work` は
/// `tokio::spawn` した別のタスクで走るので、呼び出し側（handler）が捨てられても
/// `JoinHandle` の drop はタスクを止めず、最後まで完結する。応答の外形は変えない
/// （handler は結果をそのまま返す）。
///
/// タスクが panic したら（`JoinError`）[`DetachedTaskFailed`]（応答は `500`）を返す:
/// 副作用がどこまで済んだかは分からないので、呼び出し側には「状態を読み直す」ように
/// 伝え、理由はログに残す。
///
/// banto 自身の書き込みの handler にも同じ形の処理（`.await` をまたぐ書き込み → 監査）があり、
/// 切断で途中で止まる可能性がある（banto 側では再現を確かめていない）ので、共通の口を
/// tyaro/banto#793 で提案した。入ったら置き換える。
pub(crate) async fn run_detached<F>(label: &str, work: F) -> Result<F::Output, DetachedTaskFailed>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    tokio::spawn(work).await.map_err(|err| {
        eprintln!("{label}: 処理のタスクが途中で終わりました: {err}");
        DetachedTaskFailed
    })
}

/// [`run_detached`] のタスクが最後まで走らなかった（panic）。応答は `500`。
#[derive(Debug)]
pub(crate) struct DetachedTaskFailed;

impl IntoResponse for DetachedTaskFailed {
    fn into_response(self) -> Response {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorBody::Other {
                message: "処理が途中で止まりました。状態を読み直してから、もう一度試してください"
                    .to_string(),
            }),
        )
            .into_response()
    }
}

/// `POST /api/auth/grant/<segment>` が `localDesktop` の発行なら、発行するセッションの
/// role（それ以外は `None`）。kind は banto の発行口（`Path<String>`）と同じく percent
/// decode して比べ、role はこの listener の起動時の形（[`LocalGrantStart::issued_role`]
/// = 合成の identity に登録した role）から決める。どちらも発行の時点で決まっていて、
/// 発行されたトークンを読み直さない（#601。発行の成功の後に FIFO で追い出されても、
/// 失効しても、発行の監査は 1 対 1）。
fn local_desktop_issue_role(access: LocalAccess, segment: &str) -> Option<Role> {
    if !access.may_issue() || percent_decode(segment).as_deref() != Some(LOCAL_DESKTOP_GRANT_KIND) {
        return None;
    }
    access.start.issued_role()
}

/// 発行の成功を監査に残す（モジュール doc「監査」）。`role` は発行したセッションのもの
/// （[`local_desktop_issue_role`]）。
async fn record_grant_login(state: &LocalAccessState, role: &str) {
    state
        .audit
        .record(AuditEntry {
            actor_username: Some(LOCAL_GRANT_ACTOR_ID),
            actor_id: None,
            actor_role: Some(role),
            actor_kind: Some(&banto_admin_services::audit::grant_actor_kind(
                LOCAL_DESKTOP_GRANT_KIND,
            )),
            action: "login",
            resource: "auth",
            entity_id: None,
            detail: Some(serde_json::json!({ "via": "localGrant" })),
            origin: "rest",
            result: "ok",
        })
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_setting_is_off_unless_it_names_a_role() {
        assert_eq!(
            LocalGrantSetting::parse_stored(None),
            LocalGrantSetting::Off
        );
        assert_eq!(
            LocalGrantSetting::parse_stored(Some("")),
            LocalGrantSetting::Off
        );
        assert_eq!(
            LocalGrantSetting::parse_stored(Some("off")),
            LocalGrantSetting::Off
        );
        assert_eq!(
            LocalGrantSetting::parse_stored(Some("superuser")),
            LocalGrantSetting::Off
        );
        for role in [Role::Viewer, Role::Editor, Role::Admin] {
            assert_eq!(
                LocalGrantSetting::parse_stored(Some(role.as_str())),
                LocalGrantSetting::Role(role)
            );
            assert_eq!(
                LocalGrantSetting::parse(LocalGrantSetting::Role(role).as_str()).unwrap(),
                LocalGrantSetting::Role(role)
            );
        }
        assert!(LocalGrantSetting::parse("root").is_err());
    }

    #[test]
    fn synthetic_identities_are_reserved_usernames() {
        for name in [
            "local",
            "localDesktop",
            "public",
            " Local ",
            "LOCALDESKTOP",
            "localdesktop",
            "Public",
        ] {
            assert!(is_reserved_username(name), "{name}");
            assert!(check_username_not_reserved(name).is_err(), "{name}");
        }
        for name in ["admin", "localuser", "local2", "publicist", "operator"] {
            assert!(!is_reserved_username(name), "{name}");
            assert!(check_username_not_reserved(name).is_ok(), "{name}");
        }
        assert_eq!(
            reserved_accounts(["admin", "Local", "op", "localDesktop"]),
            vec!["Local".to_string(), "localDesktop".to_string()]
        );
    }

    /// #600 の監査: 切り離したタスクが panic したら `500` を返す（呼び出し側は panic
    /// しない）。終わればその結果を返す。
    #[tokio::test]
    async fn run_detached_turns_a_panic_into_500() {
        assert_eq!(run_detached("test", async { 7 }).await.ok(), Some(7));
        let failed = run_detached("test", async {
            panic!("boom");
        })
        .await
        .expect_err("panic を結果として返した");
        assert_eq!(
            failed.into_response().status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[test]
    fn percent_decode_reads_the_kind_like_banto() {
        assert_eq!(
            percent_decode("local%44esktop").as_deref(),
            Some("localDesktop")
        );
        assert_eq!(
            percent_decode("%6c%6F%63alDesktop").as_deref(),
            Some("localDesktop")
        );
        assert_eq!(
            percent_decode("publicViewer").as_deref(),
            Some("publicViewer")
        );
        assert_eq!(percent_decode("bad%4"), None);
        assert_eq!(percent_decode("bad%zz"), None);
        assert_eq!(percent_decode("%ff"), None);
    }

    #[test]
    fn issued_role_follows_the_start() {
        assert_eq!(LocalGrantStart::Unsupported.issued_role(), None);
        assert_eq!(
            LocalGrantStart::NoLogin(Role::Admin).issued_role(),
            Some(Role::Admin)
        );
        assert_eq!(
            LocalGrantStart::Initialized(LocalGrantSetting::Off).issued_role(),
            None
        );
        assert_eq!(
            LocalGrantStart::Initialized(LocalGrantSetting::Role(Role::Editor)).issued_role(),
            Some(Role::Editor)
        );
    }

    #[test]
    fn restart_is_required_only_to_open_or_change_the_role() {
        use LocalGrantSetting::{Off, Role as R};
        let viewer = R(Role::Viewer);
        assert!(!LocalGrantStart::Unsupported.restart_required(viewer, true));
        assert!(!LocalGrantStart::NoLogin(Role::Admin).restart_required(viewer, false));
        assert!(!LocalGrantStart::NoLogin(Role::Admin).restart_required(Off, true));
        assert!(LocalGrantStart::NoLogin(Role::Admin).restart_required(viewer, true));
        assert!(!LocalGrantStart::Initialized(viewer).restart_required(viewer, true));
        // 閉じる方向（`off`）はすぐ効く。
        assert!(!LocalGrantStart::Initialized(viewer).restart_required(Off, true));
        assert!(LocalGrantStart::Initialized(viewer).restart_required(R(Role::Admin), true));
        assert!(LocalGrantStart::Initialized(Off).restart_required(viewer, true));
        // ログインなしで起動した後の `admin` は、起動時の role と同じなので再起動なしで入れる
        // （banto v8、docs/banto-v8-upgrade-plan.md §2 の 3）。
        assert!(!LocalGrantStart::NoLogin(Role::Admin).restart_required(R(Role::Admin), true));
        assert!(LocalGrantStart::NoLogin(Role::Admin).restart_required(R(Role::Editor), true));
    }

    #[test]
    fn only_the_always_on_listener_with_a_role_may_issue() {
        let open = LocalAccess {
            start: LocalGrantStart::NoLogin(Role::Admin),
            always_on_listener: true,
        };
        assert!(open.may_issue());
        assert!(!LocalAccess {
            always_on_listener: false,
            ..open
        }
        .may_issue());
        assert!(!LocalAccess {
            start: LocalGrantStart::Initialized(LocalGrantSetting::Off),
            ..open
        }
        .may_issue());
        assert!(!LocalAccess::closed().may_issue());
    }
}
