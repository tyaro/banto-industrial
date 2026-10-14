//! この PC からの接続を通す grant `localDesktop`（#392 段階 B の B2-1、H8、2026-10-14。
//! 設計は docs/chronogazer-service-design.md §3.2・§4 の B2-1・§5 の決定 8）。
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
//! 4. **権限**: 起動時に未初期化（アカウント 0 件）なら **admin**、ただし条件は
//!    「いま未初期化」をリクエストのたびに読むので、初期化された時点で閉じる。初期化
//!    済みで起動したら設定 `auth.local_grant`（`off`/`viewer`/`editor`/`admin`、
//!    **既定 off**、決定 8）の値。条件は「いま初期化済み かつ いまの設定が起動時の
//!    値と同じ」をリクエストのたびに読む。
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
//! - 未初期化で起動した後に初期化された（最初の管理者を作った）ときも、設定の値で
//!   入れるのは次の起動から（それまでは閉じる。既定 off なら変わらない）。
//!
//! # 未初期化の間と最初の管理者の作成（`POST /api/auth/setup`）
//!
//! - setup は置き換えずに**併存**させ、grant と同じく常時の listener の、この PC からの
//!   要求（peer と Host の両方が loopback）だけで開く（`0.0.0.0` にまとめた 1 本でも、
//!   LAN からの要求は閉じる。2026-10-14 オーナー決定）。grant の admin は「最初のアカウントを作るまでの間、この PC から
//!   設定を始められる」ためのもので、最初のアカウントは setup（必ず admin）で作る。
//! - setup にも同じ **Host の確認**をかける（[`local_access_middleware`]。B1-5 の既知の
//!   制限「LAN に出していない間は同じ PC の任意のプロセスが最初の管理者を作れる」のうち、
//!   ブラウザーで開いた外のページ（DNS rebinding）から作られる経路を塞ぐ。同じ PC の
//!   プロセスが作れるのはデスクトップの初回セットアップと同じ範囲で、grant の未初期化の
//!   admin と同じ性質 - 受け入れる）。
//! - 未初期化の間に grant の admin で `POST /api/users` を呼ぶと断る（`400`）。最初の
//!   アカウントを editor・viewer で作ると、初期化された時点で grant が閉じ、admin が
//!   誰もいない状態に閉じ込められるため。
//! - setup が成功して初期化されたら、未初期化の間に発行した admin のトークンを失効させる
//!   （「未初期化の間だけ admin」。条件はアカウントの行で閉じているので、順序は ADR-0017
//!   どおり）。成否は応答の本文の `success` で決め（上流は失敗でも `200` を返す）、DB は
//!   読み直さない。読めない本文は失効へ倒す（#600 の監査 F1、[`setup_may_have_initialized`]）。
//!
//! # 後処理は応答から切り離す（#600 の監査 F1・F2・F4）
//!
//! hyper 1.x は応答を待つ間に相手の FIN を読むと service の future を捨てる。発行とその
//! 監査、setup とその後の失効、設定の REST の適用（`crate::server_admin`）は
//! [`run_detached`] で別のタスクにして、接続を切られても最後まで完結させる。
//!
//! # 監査
//!
//! 発行（banto の `grant_router` は監査しない - キオスクの再読み込みで埋もれるため）に
//! 成功したら `login`（`resource: auth`、`detail: { via: "localGrant" }`、actor は合成の
//! `localDesktop` と発行した role、`origin: rest`）を残す。デスクトップの自動ログインの
//! `detail: { via: "autologin" }` と同じ作法。admin 相当になりうる口なので、発行の数は
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
//!   そのため初期化後の既定は `off`。

use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use banto_core::{BantoError, ErrorBody};
use banto_server::routes::bearer_token;
use banto_server::{
    request_is_loopback_local, AuthState, GrantCondition, GrantKind, GrantSpec, Identity,
};

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

/// 既にある予約名のアカウント（起動時の警告用。[`RESERVED_USERNAMES`] の前に作られた
/// DB。自動では直さない）。
pub fn reserved_accounts<'a>(usernames: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    usernames
        .into_iter()
        .filter(|name| is_reserved_username(name))
        .map(str::to_owned)
        .collect()
}

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
    /// 起動時に未初期化。admin で発行し、初期化されたら閉じる。
    Uninitialized,
    /// 起動時に初期化済み。この設定の値で発行する（`Off` なら発行しない）。
    Initialized(LocalGrantSetting),
}

impl LocalGrantStart {
    /// 起動時に決める（`chronogazer-server`）。読めなければ発行しない形
    /// （`Initialized(Off)`）に倒し、理由をログに出す（fail closed）。
    pub async fn resolve(users: &UsersService, settings: &SettingsService, label: &str) -> Self {
        let initialized = match users.is_initialized().await {
            Ok(initialized) => initialized,
            Err(err) => {
                eprintln!(
                    "{label}: アカウントの有無を読めなかったため、この PC からの接続（localDesktop）は発行しません: {err}"
                );
                return Self::Initialized(LocalGrantSetting::Off);
            }
        };
        if !initialized {
            return Self::Uninitialized;
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
            Self::Uninitialized => Some(Role::Admin),
            Self::Initialized(LocalGrantSetting::Role(role)) => Some(*role),
        }
    }

    /// 保存済みの設定 `saved` で発行するには再起動が要るか（モジュール doc「ロールの
    /// 変更は再起動で反映」）。`initialized_now` はいまアカウントがあるか。
    pub fn restart_required(&self, saved: LocalGrantSetting, initialized_now: bool) -> bool {
        match self {
            Self::Unsupported => false,
            Self::Uninitialized => initialized_now && saved != LocalGrantSetting::Off,
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
        (true, LocalGrantStart::Uninitialized) => Arc::new(move || {
            let users = users.clone();
            Box::pin(async move { Ok(!users.is_initialized().await?) })
        }),
        (true, LocalGrantStart::Initialized(started)) => Arc::new(move || {
            let (users, settings) = (users.clone(), settings.clone());
            Box::pin(async move {
                Ok(users.is_initialized().await? && read_setting(&settings).await? == started)
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
}

const STATUS_PATH: &str = "/api/auth/status";
const GRANT_PREFIX: &str = "/api/auth/grant/";
const SETUP_PATH: &str = "/api/auth/setup";
const USERS_PATH: &str = "/api/users";
/// アカウントを作る本文の上限（名前・パスワード・表示名だけなので十分に大きい）。
const ACCOUNT_BODY_LIMIT: usize = 64 * 1024;

/// `/api` 全体に掛ける前段（モジュール doc）:
///
/// - `GET /api/auth/status`・`POST /api/auth/grant/*`: peer と Host の両方が loopback で
///   なければ `ConnectInfo` を外す（banto の判定が peer 不明として閉じる）。
/// - `POST /api/auth/setup`: loopback の listener のルーターでは、peer と Host の両方が
///   loopback でなければ `403`（`banto-serve` の `BANTO_ALLOW_SETUP` は開発用なので
///   変えない）。成功して初期化されたら、未初期化の間の admin のトークンを失効させる
///   （[`finish_setup`]）。
/// - `POST /api/users`: 未初期化の間に `localDesktop` のセッションから来たら `400`。
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

    let is_setup = is_post && path == SETUP_PATH;
    if is_setup && state.access.always_on_listener && !local {
        return forbidden_with(
            "最初の管理者の作成は、この PC の http://127.0.0.1:<port>/ からだけ行えます",
        );
    }

    // アカウントを作る経路では予約名を断る（[`RESERVED_USERNAMES`]）。
    if is_post && (path == SETUP_PATH || path == USERS_PATH) {
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
        // 予約名の検査を素通りする。JSON として読めない本文は従来どおり上流に任せる
        // （上流が型・Content-Type で断る）。
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
            Err(_) => {}
        }
        req = Request::from_parts(parts, axum::body::Body::from(bytes));
    }

    if is_post && path == USERS_PATH {
        // `&Request` を `.await` の向こうへ持たない（本文が `Sync` でないので future が
        // `Send` でなくなる）。トークンだけを取り出して渡す。
        let token = bearer_token(req.headers()).map(str::to_owned);
        if let Some(refusal) = refuse_first_account_by_grant(&state, token).await {
            return refusal;
        }
    }

    // 発行の監査は、生のパスの kind ではなく**発行されたトークンのセッションの kind**で
    // 判断する（#600 のオーナーレビュー P2）: banto の発行口は `Path<String>` で kind を
    // percent decode するので、`/api/auth/grant/local%44esktop` でも `localDesktop` が
    // 発行される。生の文字列と比べると、その別表記で発行の監査が抜けていた。
    // （peer と Host の確認は接頭辞 `/api/auth/grant/` で掛けるので別表記でも素通り
    // しない。axum のルーティングは decode しない生のパスで行うので、接頭辞・status・
    // setup・users を別表記にした要求はどのルートにも当たらない。）
    let is_grant_issue = is_post && path.starts_with(GRANT_PREFIX);

    if !(is_grant_issue || is_setup) {
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
        let response = if is_grant_issue && response.status().is_success() {
            audit_issued_grant(&state, response).await
        } else {
            response
        };
        if is_setup {
            finish_setup(&state, response).await
        } else {
            response
        }
    };
    match run_detached("localDesktop の発行・最初の管理者の作成", detached).await {
        Ok(response) => response,
        Err(failed) => failed.into_response(),
    }
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

fn forbidden_with(message: &str) -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(ErrorBody::Other {
            message: message.to_string(),
        }),
    )
        .into_response()
}

/// 未初期化の間に `localDesktop` のセッションが最初のアカウントを作ろうとしたら断る
/// （モジュール doc「未初期化の間と最初の管理者の作成」）。
async fn refuse_first_account_by_grant(
    state: &LocalAccessState,
    token: Option<String>,
) -> Option<Response> {
    if state.access.start != LocalGrantStart::Uninitialized {
        return None;
    }
    let token = token?;
    let session = state.auth.authenticate(&token).await.ok().flatten()?;
    if session.grant.as_ref().map(GrantKind::as_str) != Some(LOCAL_DESKTOP_GRANT_KIND) {
        return None;
    }
    match state.users.is_initialized().await {
        Ok(true) => None,
        Ok(false) => Some(
            (
                StatusCode::BAD_REQUEST,
                Json(ErrorBody::BadRequest {
                    message: "最初のアカウントは初回セットアップ（管理者）で作ってください。管理者が誰もいない状態を防ぐため、この PC からの接続ではまだアカウントを追加できません".to_string(),
                }),
            )
                .into_response(),
        ),
        Err(err) => Some(banto_server::ApiError(err).into_response()),
    }
}

/// grant の発行の応答の上限（`{success, token}` だけなので十分に小さい）。
const GRANT_RESPONSE_LIMIT: usize = 64 * 1024;

/// 発行の成功の応答から発行されたトークンを読み、そのセッションの kind が
/// `localDesktop` なら監査に残す。応答はそのまま組み直して返す。
async fn audit_issued_grant(state: &LocalAccessState, response: Response) -> Response {
    let (parts, body) = response.into_parts();
    let bytes = match axum::body::to_bytes(body, GRANT_RESPONSE_LIMIT).await {
        Ok(bytes) => bytes,
        Err(err) => {
            // 読めなければトークンを渡せない（発行は成り立たない）。理由だけ残す。
            eprintln!("grant の発行の応答を読めませんでした: {err}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorBody::Other {
                    message: "grant の発行の応答を読めませんでした".to_string(),
                }),
            )
                .into_response();
        }
    };
    let token = serde_json::from_slice::<serde_json::Value>(&bytes)
        .ok()
        .and_then(|value| value["token"].as_str().map(str::to_owned));
    if let Some(token) = token {
        if let Ok(Some(session)) = state.auth.authenticate(&token).await {
            if session.grant.as_ref().map(GrantKind::as_str) == Some(LOCAL_DESKTOP_GRANT_KIND) {
                record_grant_login(state, &session.identity.role).await;
            }
        }
    }
    Response::from_parts(parts, axum::body::Body::from(bytes))
}

/// 発行の成功を監査に残す（モジュール doc「監査」）。`role` は発行したセッションのもの。
async fn record_grant_login(state: &LocalAccessState, role: &str) {
    state
        .audit
        .record(AuditEntry {
            actor_username: Some(LOCAL_GRANT_ACTOR_ID),
            actor_role: Some(role),
            action: "login",
            resource: "auth",
            entity_id: None,
            detail: Some(serde_json::json!({ "via": "localGrant" })),
            origin: "rest",
            result: "ok",
        })
        .await;
}

/// setup の応答の本文の上限（`{success, token, ...}` だけなので十分に小さい）。
const SETUP_RESPONSE_LIMIT: usize = 64 * 1024;

/// setup の応答から、最初の管理者ができた**かもしれない**か（#600 の監査 F1）。`body` は
/// 応答の本文（読めなかったら `None`）。
///
/// 上流（banto の `auth_setup_handler`）は、成功で `200 {success: true}`、既に初期化
/// 済み・保存の失敗でも `200 {success: false}` を返す。HTTP の状態は setup の成否では
/// ないので本文で決める: 2xx 以外と、本文が `success: false` を言うときだけ「できて
/// いない」。2xx で本文が読めない・解釈できないときは「できたかもしれない」として失効へ
/// 倒す（fail closed。失効しすぎても、未初期化のままなら grant をもう一度受け取れる）。
pub(crate) fn setup_may_have_initialized(status: StatusCode, body: Option<&[u8]>) -> bool {
    if !status.is_success() {
        return false;
    }
    let said_failure = body
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(bytes).ok())
        .is_some_and(|value| value.get("success") == Some(&serde_json::Value::Bool(false)));
    !said_failure
}

/// setup の後処理（モジュール doc「未初期化の間と最初の管理者の作成」）: 最初の管理者が
/// できた（かもしれない）なら、未初期化の間に発行した admin のトークンを失効させる。
///
/// #600 の監査 F1: DB を読み直さない（setup は commit 済みなのに、追加の
/// `is_initialized()` だけが一時的に失敗すると失効が飛び、admin のトークンが寿命まで
/// 残っていた）。判断は応答の本文だけ（[`setup_may_have_initialized`]）で、失効は同期で
/// すぐ行い、監査の記録はその後（[`local_access_middleware`] の切り離したタスクの中）。
pub(crate) async fn finish_setup(state: &LocalAccessState, response: Response) -> Response {
    if state.access.start != LocalGrantStart::Uninitialized || !response.status().is_success() {
        return response;
    }
    let (parts, body) = response.into_parts();
    let bytes = axum::body::to_bytes(body, SETUP_RESPONSE_LIMIT).await;
    let revoked = if setup_may_have_initialized(parts.status, bytes.as_deref().ok()) {
        state.auth.revoke_grant_tokens(&local_desktop_grant_kind())
    } else {
        0
    };
    if revoked > 0 {
        state
            .audit
            .record(AuditEntry {
                actor_username: None,
                actor_role: None,
                action: "revoke_grants",
                resource: "auth",
                entity_id: None,
                detail: Some(serde_json::json!({
                    "kind": LOCAL_DESKTOP_GRANT_KIND,
                    "reason": "initialized",
                    "revokedGrants": revoked,
                })),
                origin: "rest",
                result: "ok",
            })
            .await;
    }
    match bytes {
        Ok(bytes) => Response::from_parts(parts, axum::body::Body::from(bytes)),
        Err(err) => {
            eprintln!("最初の管理者の作成の応答を読めませんでした: {err}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorBody::Other {
                    message: "最初の管理者の作成の応答を読めませんでした。ログイン画面から入り直してください".to_string(),
                }),
            )
                .into_response()
        }
    }
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

    /// #600 の監査 F1: setup の成否は HTTP の状態ではなく本文の `success` で決め、
    /// 読めない・解釈できない 2xx は失効へ倒す（fail closed）。
    #[test]
    fn setup_success_is_read_from_the_body_and_fails_closed() {
        let ok = StatusCode::OK;
        let cases: [(StatusCode, Option<&[u8]>, bool); 9] = [
            (ok, Some(br#"{"success":true,"token":"t"}"#), true),
            (ok, Some(br#"{"success":false,"error":"x"}"#), false),
            (ok, Some(b"not json"), true),
            (ok, None, true),
            (ok, Some(b"{}"), true),
            (ok, Some(br#"{"success":"false"}"#), true),
            (ok, Some(b"[false]"), true),
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                Some(br#"{"success":true}"#),
                false,
            ),
            (StatusCode::FORBIDDEN, None, false),
        ];
        for (status, body, expected) in cases {
            assert_eq!(
                setup_may_have_initialized(status, body),
                expected,
                "{status} {:?}",
                body.map(String::from_utf8_lossy)
            );
        }
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
    fn issued_role_follows_the_start() {
        assert_eq!(LocalGrantStart::Unsupported.issued_role(), None);
        assert_eq!(
            LocalGrantStart::Uninitialized.issued_role(),
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
        assert!(!LocalGrantStart::Uninitialized.restart_required(viewer, false));
        assert!(!LocalGrantStart::Uninitialized.restart_required(Off, true));
        assert!(LocalGrantStart::Uninitialized.restart_required(viewer, true));
        assert!(!LocalGrantStart::Initialized(viewer).restart_required(viewer, true));
        // 閉じる方向（`off`）はすぐ効く。
        assert!(!LocalGrantStart::Initialized(viewer).restart_required(Off, true));
        assert!(LocalGrantStart::Initialized(viewer).restart_required(R(Role::Admin), true));
        assert!(LocalGrantStart::Initialized(Off).restart_required(viewer, true));
    }

    #[test]
    fn only_the_always_on_listener_with_a_role_may_issue() {
        let open = LocalAccess {
            start: LocalGrantStart::Uninitialized,
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
