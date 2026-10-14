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
//!   どおり）。
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
            Self::Initialized(started) => *started != saved,
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

/// `/api` 全体に掛ける前段（モジュール doc）:
///
/// - `GET /api/auth/status`・`POST /api/auth/grant/*`: peer と Host の両方が loopback で
///   なければ `ConnectInfo` を外す（banto の判定が peer 不明として閉じる）。
/// - `POST /api/auth/setup`: loopback の listener のルーターでは、peer と Host の両方が
///   loopback でなければ `403`（`banto-serve` の `BANTO_ALLOW_SETUP` は開発用なので
///   変えない）。成功して初期化されたら、未初期化の間の admin のトークンを失効させる。
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

    let response = next.run(req).await;

    let response = if is_grant_issue && response.status().is_success() {
        audit_issued_grant(&state, response).await
    } else {
        response
    };
    if is_setup && response.status().is_success() {
        revoke_bootstrap_grants(&state).await;
    }
    response
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

/// 最初の管理者ができたら、未初期化の間に発行した admin のトークンを失効させる。
async fn revoke_bootstrap_grants(state: &LocalAccessState) {
    if state.access.start != LocalGrantStart::Uninitialized {
        return;
    }
    if !matches!(state.users.is_initialized().await, Ok(true)) {
        return;
    }
    let revoked = state.auth.revoke_grant_tokens(&local_desktop_grant_kind());
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
        assert!(LocalGrantStart::Initialized(viewer).restart_required(Off, true));
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
