//! ChronoGazer — Tauri entry point.
//!
//! Thin `tauri::command` adapters only (spec §10): all real logic lives in
//! `chronogazer-core` (`apps/chronogazer/core`) and `banto-server`
//! (`crates/banto-server`), neither of which has a `tauri` dependency, so
//! both are exercised by plain `cargo test` in environments (e.g. CI
//! containers without webkit2gtk) that cannot build this crate. This file
//! CANNOT be compiled in that same environment - keep changes here small,
//! mechanical, and easy to eyeball-verify against the crates it wires
//! together.
//!
//! M6 Phase B (spec §11) adds the embedded LAN server's lifecycle to this
//! crate: `AppState` gains the settings service, the app-wide
//! resource-change broadcast channel, the embedded server's own auth state,
//! and a slot for the currently-running server (if LAN access is enabled).
//! `setup()` forwards every broadcast event onto the webview via Tauri's own
//! event system (`banto://event`) - this is `TauriEventProvider`'s other
//! half (`packages/admin-core/src/events.ts`) - and auto-starts the server
//! if it was left enabled on a previous run.

mod keyring_store;

use banto_core::{BantoError, FieldError, ListParams, ListResult};
use banto_server::{
    lan_urls, start, static_router, AuthState, RunningServer, ServerConfig, ServerEvent,
};
use chronogazer_core::assets::FrontendAssets;
use chronogazer_core::audit::{AuditEntry, AuditLogEntry, AuditLogService};
use chronogazer_core::backup::{BackupInfo, BackupService, PendingRestoreInfo};
// #383 段階2b / R1-C: 収集サービス。C-2 で `collect_*` コマンドと起動時の
// 自動開始を足し、C-3a で読み出し 3 本（現在値・接続状態・イベント一覧）を
// 足した（監査の `resource` も床も REST と共有の定数）。
use chronogazer_core::collect::{
    resolve_data_dir, CollectEventRow, CollectOutcome, CollectorService, CollectorStateView,
    ConnectionStatusView, CurrentSampleView, EventPage, Readout, COLLECT_AUDIT_RESOURCE,
    COLLECT_OPERATION_ROLE, COLLECT_READ_ROLE,
};
use chronogazer_core::db::init_db;
use chronogazer_core::events::event_channel;
use chronogazer_core::hub::{HubService, HubSubscriptionView, HubView};
use chronogazer_core::rest::{
    api_router, audited_credential_verifier, CollectionGroupPayload, PlcConnectionPayload,
    PlcConnectionResponse, TagPayload,
};
use chronogazer_core::settings::{AuditSettings, AuthSettings, ServerSettings, SettingsService};
use chronogazer_core::users::{Role, UserIdentity, UserSummary, UsersService};
// #383 段階2a / R1-B: レジストリ3サービスと行型。`chronogazer_core::lib.rs`の
// re-export 経由（invariant: このクレートは banto-tags を直接 depend
// しない - `db::DbPool` と同じ理由）。
use chronogazer_core::{
    CollectionGroup, CollectionGroupService, PlcConnectionService, Tag, TagService,
};
use qrcode::render::svg;
use qrcode::QrCode;
use serde::Serialize;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Mutex;
use tauri::{Emitter, Manager, State};
use tokio::sync::{broadcast, Mutex as AsyncMutex};

/// App-wide state managed by Tauri (spec §10, §11).
struct AppState {
    /// The webview window's own session identity, set by `auth_login`/
    /// `auth_setup` and cleared by `auth_logout` - all called directly via
    /// `invoke()`, never through `/api/auth/login`. `Some` means logged in;
    /// carrying the full `UserIdentity` (not just a bool) lets
    /// `auth_change_password` recover the current `username` without a
    /// second round trip.
    auth: Mutex<Option<UserIdentity>>,
    /// The local credential store (spec §8.2): argon2id-hashed accounts in
    /// the same SQLite settings DB as `settings` below. Shared with
    /// `rest_auth`'s verifier closure so the webview session and the
    /// embedded-server session always check the same accounts.
    users: UsersService,
    /// App settings (spec §12.1), including the embedded-server config
    /// (spec §11.2/§11.4's enabled/bind/port).
    settings: SettingsService,
    /// App-wide resource-change/notice broadcast (spec §3.5): future
    /// mutating services (R1-B) feed this, and it is fanned out two ways -
    /// to the webview via the `banto://event` forwarding task spawned in
    /// `setup()`, and (only while the embedded server is running) to LAN
    /// browser clients via `GET /api/events` (`banto_server::sse_route`).
    events: broadcast::Sender<ServerEvent>,
    /// The embedded REST/SSE server's own bearer-token auth state
    /// (`banto_server::AuthState`). Deliberately a SEPARATE token space from
    /// `auth` above: the webview window never logs in through
    /// `/api/auth/login`, so a LAN browser client logging in does not
    /// implicitly authenticate the desktop window, and vice versa - each is
    /// its own session, over its own transport (both sessions ultimately
    /// check the same `users` credential store, though).
    rest_auth: AuthState,
    /// `Some` while LAN access is enabled and successfully bound; `None`
    /// otherwise (disabled, or a previous bind attempt failed - see
    /// `server_apply`).
    server: AsyncMutex<Option<RunningServer>>,
    /// Audit trail (spec M14): every mutating command below records a
    /// `create`/`update`/`delete`/`password_reset`/`settings_change`/
    /// `login`/`login_failed`/`logout`/`setup` entry here (`origin:
    /// "tauri"`) once it has already succeeded, and [`require_role`] records
    /// `denied` when an active session's role is too low. Shares the same
    /// pool as `users`/`settings` (all three are `Clone` handles onto
    /// the one on-disk SQLite DB, see `run()`'s `setup()`).
    audit: AuditLogService,
    /// Hub 接続（#332）: banto-hub への自動接続 bootstrap。OS キーリング
    /// （`keyring_store::KeyringKeyStore`）と設定 KV を
    /// `chronogazer_core::hub` が結び付けたもの。`Clone` ハンドル（内部は
    /// `Arc`）なので、`start_embedded_server` にも同じ実体を渡して LAN
    /// ブラウザの `/api/hub/*` とデスクトップの `hub_*` コマンドが同じ
    /// 設定・同じキーリングを見るようにする。
    hub: HubService,
    /// Backup/restore (spec M17): `VACUUM INTO` snapshots into `backups/`
    /// next to the DB file, plus the restore staging flow. Shares the same
    /// pool as `users`/`settings`/`audit` - only its `db_path` is
    /// unique to this service (needed to resolve `backups/` and
    /// `restore-pending.sqlite3`'s location, see `crate::backup`'s doc
    /// comment).
    backup: BackupService,
    /// #383 段階2a / R1-B: レジストリ3サービス（PLC接続/収集グループ/
    /// タグ）。`users`/`settings`/`audit` と同じ pool を共有する `Clone`
    /// ハンドル。`start_embedded_server` にも同じ実体を渡して、LAN
    /// ブラウザの `/api/plc-connections|collection-groups|tags/*` と
    /// デスクトップの `plc_connections_*`/`collection_groups_*`/`tags_*`
    /// コマンドが同じレジストリを見るようにする。
    plc_connections: PlcConnectionService,
    collection_groups: CollectionGroupService,
    tags: TagService,
    /// #383 段階2b / R1-C: 収集ランタイム。**起動時に自動開始**し
    /// （`setup()` が `CollectorService::autostart` を spawn する）、
    /// `collect_*` コマンドと LAN ブラウザの `/api/collect*` が同じ実体を
    /// 操作する。収集は「PLC から読んで tstore に書く」側なので、Tauri の
    /// managed state が drop されないこのアプリでは、[`shutdown_app_state`]
    /// が唯一の「止める場所」になる。
    collect: CollectorService,
    /// The one SQLite pool every service above is a `Clone` handle onto,
    /// kept here solely so [`shutdown_app_state`] can `close()` it when the
    /// app exits (flushing/removing the `-wal`/`-shm` sidecar files instead
    /// of leaving them for the next launch to recover). Named through
    /// `chronogazer_core`'s `DbPool` alias so this crate keeps its invariant
    /// of adding no dependencies of its own (no direct `sqlx`).
    pool: chronogazer_core::db::DbPool,
}

#[derive(Debug, Clone, Serialize)]
struct LoginResult {
    success: bool,
    error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct Identity {
    id: String,
    name: String,
    /// Spec M10 RBAC: the account's role, as its lowercase wire string (see
    /// `chronogazer_core::users::Role::as_str`) - kept a plain `String`
    /// here rather than `Role` itself so this wire type does not need
    /// `Role: Deserialize` for a command return value that is only ever
    /// serialized outbound.
    role: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AuthStatusResult {
    initialized: bool,
}

fn identity_from(user: &UserIdentity) -> Identity {
    // Convention shared with `chronogazer_core::rest` and `banto-serve`:
    // `Identity.id` is the account's `username` (not `UserIdentity.id`'s
    // numeric row id), so any layer holding only an `Identity` can still
    // recover "which account" for things like `change_password`.
    Identity {
        id: user.username.clone(),
        name: user.display_name.clone(),
        role: user.role.to_string(),
    }
}

/// Require an active webview session with at least role `min` (spec M10
/// RBAC), returning the caller's [`UserIdentity`] on success so callers that
/// also need "which account is this" (e.g. `users_delete`'s self-deletion
/// guard) do not have to re-lock `state.auth`. No session at all ->
/// `BantoError::Unauthorized` (401-equivalent); a session that exists but is
/// under-privileged -> `BantoError::Forbidden` (403-equivalent) - mirrors
/// `chronogazer_core::rest`'s `require_auth` then `require_role_at_least`
/// distinction on the REST side.
///
/// `resource` (spec M14) tags the audit entry recorded when an
/// AUTHENTICATED session's role is too low - mirrors REST's
/// `RoleGuard`/`require_role_at_least`. The no-session (`Unauthorized`) case
/// is deliberately NOT recorded, same reasoning as the REST side: it means
/// there is nothing resembling a real user to attribute a denial to, not a
/// meaningful RBAC decision.
///
/// `async` (unlike its pre-M14 form) only to `.await` that audit write -
/// every call site is already inside an `async fn` Tauri command. The
/// `state.auth` lock is dropped (via the `identity` clone below) BEFORE the
/// `.await`, since `std::sync::MutexGuard` is `!Send` and holding one across
/// an await point would make the command's future `!Send` (which `tauri`
/// requires).
async fn require_role(
    state: &AppState,
    min: Role,
    resource: &str,
) -> Result<UserIdentity, BantoError> {
    let current = state.auth.lock().expect("auth mutex poisoned").clone();
    match current {
        Some(identity) if identity.role.at_least(min) => Ok(identity),
        Some(identity) => {
            state
                .audit
                .record(AuditEntry {
                    actor_username: Some(&identity.username),
                    actor_role: Some(identity.role.as_str()),
                    action: "denied",
                    resource,
                    entity_id: None,
                    detail: None,
                    origin: "tauri",
                    result: "denied",
                })
                .await;
            Err(BantoError::Forbidden)
        }
        None => Err(BantoError::Unauthorized),
    }
}

/// Smoke-test command used by the frontend to verify the bridge.
#[tauri::command]
fn ping() -> &'static str {
    concat!("banto ", env!("CARGO_PKG_VERSION"))
}

/// `GET`-ish command: has an account been created yet (spec §3.3/§8.2)? The
/// login page calls this first to decide between the first-run setup form
/// and the normal login form.
#[tauri::command]
async fn auth_status(state: State<'_, AppState>) -> Result<AuthStatusResult, BantoError> {
    Ok(AuthStatusResult {
        initialized: state.users.is_initialized().await?,
    })
}

/// Create the very first account and log the webview session in as it
/// (spec §8.2). `BantoError::Validation` (bad username/short password)
/// propagates as `Err` so the frontend form store can field-map it;
/// "already initialized" (or any other non-validation failure) surfaces as
/// `Ok(LoginResult { success: false, .. })` instead, since that is an
/// expected/retryable outcome, not a form error.
#[tauri::command]
async fn auth_setup(
    state: State<'_, AppState>,
    username: String,
    password: String,
    display_name: String,
) -> Result<LoginResult, BantoError> {
    match state
        .users
        .setup_first_user(&username, &password, &display_name)
        .await
    {
        Ok(identity) => {
            state
                .audit
                .record(AuditEntry {
                    actor_username: Some(&identity.username),
                    actor_role: Some(identity.role.as_str()),
                    action: "setup",
                    resource: "auth",
                    entity_id: None,
                    detail: None,
                    origin: "tauri",
                    result: "ok",
                })
                .await;
            *state.auth.lock().expect("auth mutex poisoned") = Some(identity);
            Ok(LoginResult {
                success: true,
                error: None,
            })
        }
        Err(err @ BantoError::Validation { .. }) => Err(err),
        Err(other) => Ok(LoginResult {
            success: false,
            error: Some(other.to_string()),
        }),
    }
}

#[tauri::command]
async fn auth_login(
    state: State<'_, AppState>,
    username: String,
    password: String,
) -> Result<LoginResult, BantoError> {
    match state.users.verify(&username, &password).await? {
        Some(identity) => {
            state
                .audit
                .record(AuditEntry {
                    actor_username: Some(&identity.username),
                    actor_role: Some(identity.role.as_str()),
                    action: "login",
                    resource: "auth",
                    entity_id: None,
                    detail: None,
                    origin: "tauri",
                    result: "ok",
                })
                .await;
            *state.auth.lock().expect("auth mutex poisoned") = Some(identity);
            Ok(LoginResult {
                success: true,
                error: None,
            })
        }
        None => {
            state
                .audit
                .record(AuditEntry {
                    actor_username: Some(&username),
                    actor_role: None,
                    action: "login_failed",
                    resource: "auth",
                    entity_id: None,
                    detail: None,
                    origin: "tauri",
                    result: "failed",
                })
                .await;
            Ok(LoginResult {
                success: false,
                error: Some("ユーザー名またはパスワードが違います".to_string()),
            })
        }
    }
}

/// No-op while auth-disabled mode is on (spec M11): that mode has no login
/// screen to fall back to, so clearing `state.auth` here would strand the
/// webview with no session at all until the next app restart re-runs the
/// bootstrap in `run()`. Re-synthesizing the identity inline (instead of
/// just refusing to clear it) was considered and rejected as needlessly
/// complex for the same outcome - the simpler "logout does nothing in this
/// mode" reads clearly at the call site and matches auth-disabled mode's
/// framing as "this whole device is trusted, there is no session to log out
/// of". Spec M14: that no-op path deliberately records no `logout` entry
/// either - nothing actually changed.
#[tauri::command]
async fn auth_logout(state: State<'_, AppState>) -> Result<(), BantoError> {
    if state.settings.auth_config().await?.disabled {
        return Ok(());
    }
    let previous = state.auth.lock().expect("auth mutex poisoned").clone();
    *state.auth.lock().expect("auth mutex poisoned") = None;
    if let Some(identity) = previous {
        state
            .audit
            .record(AuditEntry {
                actor_username: Some(&identity.username),
                actor_role: Some(identity.role.as_str()),
                action: "logout",
                resource: "auth",
                entity_id: None,
                detail: None,
                origin: "tauri",
                result: "ok",
            })
            .await;
    }
    Ok(())
}

#[tauri::command]
fn auth_check(state: State<'_, AppState>) -> bool {
    state.auth.lock().expect("auth mutex poisoned").is_some()
}

#[tauri::command]
fn auth_identity(state: State<'_, AppState>) -> Option<Identity> {
    state
        .auth
        .lock()
        .expect("auth mutex poisoned")
        .as_ref()
        .map(identity_from)
}

/// Body of [`auth_change_password`], split out so the audit-recording
/// behavior (spec M14) is testable with a plain `&AppState` in this crate's
/// own `cargo test` - `tauri::State` cannot be constructed outside a running
/// tauri app, but it derefs to `&AppState`, so the command below is a
/// one-line adapter.
async fn change_own_password(
    state: &AppState,
    current_password: &str,
    new_password: &str,
) -> Result<(), BantoError> {
    let identity = {
        let guard = state.auth.lock().expect("auth mutex poisoned");
        match guard.as_ref() {
            Some(identity) => identity.clone(),
            None => return Err(BantoError::Unauthorized),
        }
    };
    state
        .users
        .change_password(&identity.username, current_password, new_password)
        .await?;
    // Spec M14: a self-service password change is a security event (it is
    // also what naturally invalidates an M11 autologin credential), so it IS
    // audited - actor and entity are both the caller. `detail` stays `None`:
    // neither the old nor the new password (nor any hash) may ever be
    // recorded.
    state
        .audit
        .record(AuditEntry {
            actor_username: Some(&identity.username),
            actor_role: Some(identity.role.as_str()),
            action: "password_change",
            resource: "users",
            entity_id: Some(&identity.id.to_string()),
            detail: None,
            origin: "tauri",
            result: "ok",
        })
        .await;
    Ok(())
}

/// Requires an active webview session (spec §8.2): looks up the logged-in
/// account's `username` from `state.auth` rather than taking it as a
/// parameter, so a caller cannot change a DIFFERENT account's password just
/// by naming it.
#[tauri::command]
async fn auth_change_password(
    state: State<'_, AppState>,
    current_password: String,
    new_password: String,
) -> Result<(), BantoError> {
    change_own_password(&state, &current_password, &new_password).await
}

// --- M11: auth-disabled mode + desktop autologin ---------------------------

/// Current auth-mode settings (spec M11): any authenticated role may read
/// this (it only feeds a settings-screen display), and it never carries the
/// autologin password - `AuthSettings` itself has no such field (see its doc
/// comment in `chronogazer_core::settings`).
#[tauri::command]
async fn auth_config_get(state: State<'_, AppState>) -> Result<AuthSettings, BantoError> {
    require_role(&state, Role::Viewer, "settings").await?;
    state.settings.auth_config().await
}

/// Toggle auth-disabled mode and its synthetic-identity role (spec M11).
///
/// Normally `admin`-only, like every other server/settings-mutating command
/// here. ESCAPE HATCH: while auth-disabled mode is CURRENTLY active, this
/// command is allowed regardless of the calling session's role. Reason: in
/// that mode the webview's only session is the synthetic identity `run()`'s
/// bootstrap manufactures from `disabled_role` (see that function) - if an
/// operator had configured `disabled_role` as something below `admin` (e.g.
/// `viewer`, for a kiosk), that synthetic session could never call this
/// command to turn auth back ON again, permanently locking the running app
/// out of re-enabling authentication short of editing the SQLite settings DB
/// by hand. Auth-disabled mode is already documented as "trust the whole
/// device" (spec M11), so not gating the one command that re-locks it down
/// behind a role that mode itself may have suppressed is consistent with
/// that trust model, not a weakening of it.
#[tauri::command]
async fn auth_config_apply(
    state: State<'_, AppState>,
    disabled: bool,
    disabled_role: String,
) -> Result<AuthSettings, BantoError> {
    let currently_disabled = state.settings.auth_config().await?.disabled;
    // Spec M14: the escape hatch means `require_role` may not run at all
    // (see this command's doc comment) - capture whatever actor identity
    // exists directly in that case, so the audit entry below still has one
    // when possible, instead of skipping the escape-hatch path's write
    // entirely.
    let actor = if currently_disabled {
        state.auth.lock().expect("auth mutex poisoned").clone()
    } else {
        Some(require_role(&state, Role::Admin, "settings").await?)
    };

    // An unrecognized role string falls back to `admin` (same convention as
    // `SettingsService::auth_config`'s own read-time fallback) rather than
    // failing the whole command - a bad value here must never leave the app
    // unable to determine ANY role for the synthetic identity.
    let role = Role::from_str(&disabled_role).unwrap_or(Role::Admin);

    let mut config = state.settings.auth_config().await?;
    config.disabled = disabled;
    config.disabled_role = role;
    state.settings.set_auth_config(&config).await?;
    state
        .audit
        .record(AuditEntry {
            actor_username: actor.as_ref().map(|i| i.username.as_str()),
            actor_role: actor.as_ref().map(|i| i.role.as_str()),
            action: "settings_change",
            resource: "settings",
            entity_id: None,
            detail: Some(serde_json::json!({ "authDisabled": disabled })),
            origin: "tauri",
            result: "ok",
        })
        .await;
    Ok(config)
}

/// Enable desktop autologin for `username` (spec M11): verifies the
/// credentials against the same `UsersService` a normal login would (so a
/// caller cannot register autologin for an account/password it does not
/// actually know), stores the password in the OS keyring (never in the
/// settings DB - see `keyring_store`), and flips the setting on. `admin`-only,
/// same floor as every other server/settings-mutating command.
#[tauri::command]
async fn autologin_enable(
    state: State<'_, AppState>,
    username: String,
    password: String,
) -> Result<(), BantoError> {
    let actor = require_role(&state, Role::Admin, "settings").await?;

    if state.users.verify(&username, &password).await?.is_none() {
        return Err(BantoError::Validation {
            field_errors: vec![FieldError {
                field: "password".to_string(),
                message: "ユーザー名またはパスワードが違います".to_string(),
            }],
        });
    }

    keyring_store::set_password(&username, &password)?;

    let mut config = state.settings.auth_config().await?;
    config.autologin_enabled = true;
    config.autologin_username = Some(username.clone());
    state.settings.set_auth_config(&config).await?;
    // Spec M14: the target `username` (never the password) is fine to
    // record - it identifies WHICH account autologin now applies to, no
    // different from `users_update`'s `role` detail.
    state
        .audit
        .record(AuditEntry {
            actor_username: Some(&actor.username),
            actor_role: Some(actor.role.as_str()),
            action: "settings_change",
            resource: "settings",
            entity_id: None,
            detail: Some(serde_json::json!({ "autologinEnabled": true, "username": username })),
            origin: "tauri",
            result: "ok",
        })
        .await;
    Ok(())
}

/// Disable desktop autologin (spec M11): removes the stored credential from
/// the OS keyring (best-effort - a keyring delete failure is logged, not
/// propagated, so the setting is still turned off even if the OS store is,
/// say, already gone) and clears the setting.
#[tauri::command]
async fn autologin_disable(state: State<'_, AppState>) -> Result<(), BantoError> {
    let actor = require_role(&state, Role::Admin, "settings").await?;

    let mut config = state.settings.auth_config().await?;
    if let Some(username) = config.autologin_username.take() {
        if let Err(err) = keyring_store::delete_password(&username) {
            eprintln!("banto: 自動ログインの資格情報のキーリング削除に失敗しました: {err}");
        }
    }
    config.autologin_enabled = false;
    state.settings.set_auth_config(&config).await?;
    state
        .audit
        .record(AuditEntry {
            actor_username: Some(&actor.username),
            actor_role: Some(actor.role.as_str()),
            action: "settings_change",
            resource: "settings",
            entity_id: None,
            detail: Some(serde_json::json!({ "autologinEnabled": false })),
            origin: "tauri",
            result: "ok",
        })
        .await;
    Ok(())
}

/// One LAN access URL plus its QR code, rendered as an inline SVG string
/// (spec §11.4).
#[derive(Debug, Clone, Serialize)]
struct QrSvgEntry {
    url: String,
    svg: String,
}

/// `server_status`/`server_apply`'s shared response shape - mirrors
/// `src/lib/banto/serverAdmin.ts::ServerStatus` field-for-field.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ServerStatusResult {
    enabled: bool,
    running: bool,
    bind: String,
    port: u16,
    urls: Vec<String>,
    qr_svgs: Vec<QrSvgEntry>,
}

/// Render `data` (a LAN access URL) as an inline SVG QR code (spec §11.4).
/// Falls back to an empty string on an encoding failure rather than
/// panicking - our inputs are short `http://host:port` strings well within
/// QR capacity, so this should not happen in practice, but this only feeds
/// a settings-screen `{@html}` display, not anything load-bearing.
fn qr_svg_for(data: &str) -> String {
    QrCode::new(data)
        .map(|code| code.render::<svg::Color>().min_dimensions(160, 160).build())
        .unwrap_or_default()
}

fn build_status(config: &ServerSettings, running: bool) -> ServerStatusResult {
    let urls = lan_urls(config.port);
    let qr_svgs = urls
        .iter()
        .map(|url| QrSvgEntry {
            url: url.clone(),
            svg: qr_svg_for(url),
        })
        .collect();
    ServerStatusResult {
        enabled: config.enabled,
        running,
        bind: config.bind.clone(),
        port: config.port,
        urls,
        qr_svgs,
    }
}

/// Build the full `/api/*` + static-asset router (spec §11.1) and start
/// listening. Shared by `setup()` (auto-start on launch if LAN access was
/// left enabled) and the `server_apply` command (spec §11.4's
/// 「保存して適用」button).
///
/// Deliberately never names the intermediate `axum::Router` type anywhere -
/// `axum` is not (and does not need to be) a direct dependency of this
/// crate purely to support this one function: Rust only requires a crate to
/// be listed in `[dependencies]` to *spell out* one of its types in source,
/// and the router value here only ever flows through an inferred `let`
/// binding on its way into `banto_server::start`.
// Same shape as `chronogazer_core::rest::api_router` (which this wraps)
// and for the same reason: distinct service handles, no natural struct to
// bundle them into for a single call site.
#[allow(clippy::too_many_arguments)]
async fn start_embedded_server(
    users: UsersService,
    settings: SettingsService,
    audit: AuditLogService,
    backup: BackupService,
    hub: HubService,
    // #383 段階2a / R1-B: レジストリ3サービス。
    plc_connections: PlcConnectionService,
    collection_groups: CollectionGroupService,
    tags: TagService,
    // #383 段階2b / R1-C（C-2）: 収集ランタイム。デスクトップの `collect_*`
    // コマンドと LAN ブラウザの `/api/collect*` が**同じ実体**を操作する
    // ように、`hub` と同じく `AppState` のハンドルをそのまま渡す。
    collect: CollectorService,
    auth: AuthState,
    events: broadcast::Sender<ServerEvent>,
    config: ServerConfig,
) -> Result<RunningServer, BantoError> {
    // `allow_setup: false` - the Tauri app's first-run setup goes through
    // the `auth_setup` command above (`invoke()`, no network involved), not
    // this REST endpoint. Only `banto-serve` (this repo's Tauri-free dev
    // vehicle) opts into `POST /api/auth/setup` via `BANTO_ALLOW_SETUP=1`.
    let router = api_router(
        users,
        settings,
        audit,
        backup,
        hub,
        plc_connections,
        collection_groups,
        tags,
        collect,
        auth,
        events,
        false,
    )
    .merge(static_router::<FrontendAssets>());
    start(config, router).await
}

/// `GET`-ish command: current persisted settings + live running state (spec
/// §11.4's status line). `admin`-only (spec M10: "サーバ制御系 = admin").
#[tauri::command]
async fn server_status(state: State<'_, AppState>) -> Result<ServerStatusResult, BantoError> {
    require_role(&state, Role::Admin, "settings").await?;
    let config = state.settings.server_config().await?;
    let running = state.server.lock().await.is_some();
    Ok(build_status(&config, running))
}

/// Persist new settings, stop whatever is currently running, and start a
/// fresh instance if `enabled` (spec §11.4's 「保存して適用」button).
/// Stop-then-maybe-start unconditionally (rather than diffing old vs. new
/// config) keeps this simple to reason about, at the cost of a
/// no-op restart when the caller "changes" settings to the same values -
/// an acceptable trade for a settings-screen action a user triggers
/// explicitly and infrequently.
#[tauri::command]
async fn server_apply(
    state: State<'_, AppState>,
    enabled: bool,
    bind: String,
    port: u16,
) -> Result<ServerStatusResult, BantoError> {
    let actor = require_role(&state, Role::Admin, "settings").await?;
    let config = ServerSettings {
        enabled,
        bind,
        port,
    };
    state.settings.set_server_config(&config).await?;

    if let Some(running) = state.server.lock().await.take() {
        running.stop().await;
    }

    let started = if config.enabled {
        Some(
            start_embedded_server(
                state.users.clone(),
                state.settings.clone(),
                state.audit.clone(),
                state.backup.clone(),
                state.hub.clone(),
                state.plc_connections.clone(),
                state.collection_groups.clone(),
                state.tags.clone(),
                state.collect.clone(),
                state.rest_auth.clone(),
                state.events.clone(),
                ServerConfig {
                    bind: config.bind.clone(),
                    port: config.port,
                },
            )
            .await?,
        )
    } else {
        None
    };

    let running = started.is_some();
    *state.server.lock().await = started;
    state
        .audit
        .record(AuditEntry {
            actor_username: Some(&actor.username),
            actor_role: Some(actor.role.as_str()),
            action: "settings_change",
            resource: "settings",
            entity_id: None,
            detail: Some(serde_json::json!({
                "serverEnabled": config.enabled,
                "bind": config.bind,
                "port": config.port,
            })),
            origin: "tauri",
            result: "ok",
        })
        .await;
    Ok(build_status(&config, running))
}

#[tauri::command]
async fn settings_get(
    state: State<'_, AppState>,
    key: String,
) -> Result<Option<String>, BantoError> {
    state.settings.get(&key).await
}

/// `admin`-only (spec M10): writing settings (which include the embedded
/// server's enable/bind/port via `server_apply` and, generically, anything
/// else stored through this key/value command) is a privileged action.
#[tauri::command]
async fn settings_set(
    state: State<'_, AppState>,
    key: String,
    value: String,
) -> Result<(), BantoError> {
    let actor = require_role(&state, Role::Admin, "settings").await?;
    state.settings.set(&key, &value).await?;
    // Spec M14: only the KEY is recorded, never the value - this is a
    // generic key/value store and the value could be anything, including
    // something sensitive a future setting might store here.
    state
        .audit
        .record(AuditEntry {
            actor_username: Some(&actor.username),
            actor_role: Some(actor.role.as_str()),
            action: "settings_change",
            resource: "settings",
            entity_id: None,
            detail: Some(serde_json::json!({ "key": key })),
            origin: "tauri",
            result: "ok",
        })
        .await;
    Ok(())
}

// --- M12: per-user UI settings + window vibrancy ----------------------------

/// Read one of the calling user's OWN UI settings (spec M12
/// SettingsProvider migration: theme mode/preset, dock layout). Any
/// authenticated role - unlike `settings_get`/`settings_set` these only ever
/// touch keys namespaced under the caller's own username
/// (`SettingsService::ui_get`'s `ui.{username}.{key}` scheme), so no
/// privilege is involved. In auth-disabled mode (spec M11) the synthetic
/// session's username is `"local"`, so all UI settings share that one
/// namespace - consistent with that mode's "the whole device is one trusted
/// user" framing.
#[tauri::command]
async fn ui_settings_get(
    state: State<'_, AppState>,
    key: String,
) -> Result<Option<String>, BantoError> {
    let identity = require_role(&state, Role::Viewer, "settings").await?;
    state.settings.ui_get(&identity.username, &key).await
}

/// Write one of the calling user's OWN UI settings (spec M12). Any
/// authenticated role - deliberately NOT `admin`-gated like `settings_set`,
/// see [`ui_settings_get`]'s doc comment. Spec M14: NOT audited, same
/// reasoning as the REST `/api/ui-settings/*` routes (see `rest.rs`'s module
/// doc comment) - this is each user's own theme/dock-layout preference, not
/// an admin-scoped "settings change".
#[tauri::command]
async fn ui_settings_set(
    state: State<'_, AppState>,
    key: String,
    value: String,
) -> Result<(), BantoError> {
    let identity = require_role(&state, Role::Viewer, "settings").await?;
    state
        .settings
        .ui_set(&identity.username, &key, &value)
        .await
}

/// Settings key for the desktop vibrancy toggle (spec M12): a GLOBAL
/// setting ("true"/"false", default off), not a per-user `ui.*` one - it
/// changes the physical window every user of this desktop install shares.
const KEY_DESKTOP_VIBRANCY: &str = "desktop.vibrancy";

/// `vibrancy_status`'s response shape (spec M12): the persisted toggle
/// state plus whether this build can apply it at all (`supported` is `false`
/// on non-Windows, letting the settings screen hide/disable the toggle
/// instead of showing one that can only error).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct VibrancyStatus {
    enabled: bool,
    supported: bool,
}

/// Apply or clear the Acrylic effect on `window` (Windows only, spec M12).
/// The `(18, 18, 18, 125)` tint keeps the blur legibly dark in both theme
/// modes without fully occluding the backdrop.
#[cfg(target_os = "windows")]
fn set_window_vibrancy(window: &tauri::WebviewWindow, enabled: bool) -> Result<(), BantoError> {
    let result = if enabled {
        window_vibrancy::apply_acrylic(window, Some((18, 18, 18, 125)))
    } else {
        window_vibrancy::clear_acrylic(window)
    };
    result.map_err(|err| {
        BantoError::Other(format!(
            "ウィンドウのAcrylic効果の適用に失敗しました: {err}"
        ))
    })
}

/// Toggle real window translucency (Windows Acrylic) for the main window
/// and persist the choice (spec M12). `admin`-only, same floor as
/// `settings_set` (this writes a global setting). The setting is only
/// persisted AFTER the effect applied successfully - a machine that cannot
/// apply Acrylic (e.g. an old Windows 10 build) keeps its stored value
/// unchanged instead of persisting a state the window does not reflect.
/// Returns the applied state.
///
/// Non-Windows builds always fail with a clear message (Windows のみ, spec
/// M12/docs/roadmap.md §6) - the frontend avoids ever calling this there by
/// checking `vibrancy_status().supported` first.
#[tauri::command]
async fn vibrancy_apply(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    enabled: bool,
) -> Result<bool, BantoError> {
    let actor = require_role(&state, Role::Admin, "settings").await?;

    #[cfg(target_os = "windows")]
    {
        let window = app
            .get_webview_window("main")
            .ok_or_else(|| BantoError::Other("メインウィンドウが見つかりません".to_string()))?;
        set_window_vibrancy(&window, enabled)?;
        state
            .settings
            .set(KEY_DESKTOP_VIBRANCY, if enabled { "true" } else { "false" })
            .await?;
        state
            .audit
            .record(AuditEntry {
                actor_username: Some(&actor.username),
                actor_role: Some(actor.role.as_str()),
                action: "settings_change",
                resource: "settings",
                entity_id: None,
                detail: Some(serde_json::json!({ "vibrancyEnabled": enabled })),
                origin: "tauri",
                result: "ok",
            })
            .await;
        Ok(enabled)
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = (app, enabled, actor); // parameters only used on Windows
        Err(BantoError::Other(
            "この機能はWindowsでのみ利用できます".to_string(),
        ))
    }
}

/// Current vibrancy state (spec M12): any authenticated role (it only feeds
/// the settings screen's toggle display). Never errors on non-Windows -
/// `supported: false` (with `enabled: false`, regardless of any stored
/// value) is the signal the frontend uses to hide the toggle.
#[tauri::command]
async fn vibrancy_status(state: State<'_, AppState>) -> Result<VibrancyStatus, BantoError> {
    require_role(&state, Role::Viewer, "settings").await?;
    let supported = cfg!(target_os = "windows");
    let enabled = supported
        && state
            .settings
            .get(KEY_DESKTOP_VIBRANCY)
            .await?
            .map(|value| value == "true")
            .unwrap_or(false);
    Ok(VibrancyStatus { enabled, supported })
}

/// Wire shape returned by `users_create` (spec M10): everything
/// `UserIdentity` carries, `Serialize`d for the Tauri command boundary
/// (`UserIdentity` itself is not `Serialize` - see its doc comment in
/// `chronogazer_core::users`). No `createdAt` (unlike [`UserSummary`],
/// which `users_list`/`users_update` return): `UsersService::create_user`
/// does not read it back from the DB, only the row it just inserted.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct UserIdentityResult {
    id: i64,
    username: String,
    display_name: String,
    role: Role,
}

impl From<UserIdentity> for UserIdentityResult {
    fn from(identity: UserIdentity) -> Self {
        Self {
            id: identity.id,
            username: identity.username,
            display_name: identity.display_name,
            role: identity.role,
        }
    }
}

/// `admin`-only (spec M10): the user-management screen's account list.
#[tauri::command]
async fn users_list(state: State<'_, AppState>) -> Result<Vec<UserSummary>, BantoError> {
    require_role(&state, Role::Admin, "users").await?;
    state.users.list_users().await
}

/// `admin`-only (spec M10): create an additional account.
#[tauri::command]
async fn users_create(
    state: State<'_, AppState>,
    username: String,
    password: String,
    display_name: String,
    role: Role,
) -> Result<UserIdentityResult, BantoError> {
    let actor = require_role(&state, Role::Admin, "users").await?;
    let identity = state
        .users
        .create_user(&username, &password, &display_name, role)
        .await?;
    state
        .audit
        .record(AuditEntry {
            actor_username: Some(&actor.username),
            actor_role: Some(actor.role.as_str()),
            action: "create",
            resource: "users",
            entity_id: Some(&identity.id.to_string()),
            detail: Some(
                serde_json::json!({ "username": identity.username, "role": identity.role }),
            ),
            origin: "tauri",
            result: "ok",
        })
        .await;
    Ok(identity.into())
}

/// `admin`-only (spec M10): update an account's display name/role. Refuses
/// to demote the last remaining `admin` (`UsersService::update_user`'s
/// guard).
#[tauri::command]
async fn users_update(
    state: State<'_, AppState>,
    id: i64,
    display_name: String,
    role: Role,
) -> Result<UserSummary, BantoError> {
    let actor = require_role(&state, Role::Admin, "users").await?;
    let updated = state.users.update_user(id, &display_name, role).await?;
    state
        .audit
        .record(AuditEntry {
            actor_username: Some(&actor.username),
            actor_role: Some(actor.role.as_str()),
            action: "update",
            resource: "users",
            entity_id: Some(&id.to_string()),
            detail: Some(serde_json::json!({ "role": updated.role })),
            origin: "tauri",
            result: "ok",
        })
        .await;
    Ok(updated)
}

/// `admin`-only (spec M10): reset another account's password without
/// knowing its current one (unlike self-service `auth_change_password`).
#[tauri::command]
async fn users_reset_password(
    state: State<'_, AppState>,
    id: i64,
    new_password: String,
) -> Result<(), BantoError> {
    let actor = require_role(&state, Role::Admin, "users").await?;
    state.users.reset_password(id, &new_password).await?;
    state
        .audit
        .record(AuditEntry {
            actor_username: Some(&actor.username),
            actor_role: Some(actor.role.as_str()),
            action: "password_reset",
            resource: "users",
            entity_id: Some(&id.to_string()),
            detail: None,
            origin: "tauri",
            result: "ok",
        })
        .await;
    Ok(())
}

/// `admin`-only (spec M10): delete an account. Refuses to delete the last
/// remaining `admin` or the caller's own account
/// (`UsersService::delete_user`'s guards) - the acting admin's id comes
/// from the session `require_role` just verified, not from an argument, so
/// a caller cannot spoof a different acting user.
#[tauri::command]
async fn users_delete(state: State<'_, AppState>, id: i64) -> Result<(), BantoError> {
    let acting = require_role(&state, Role::Admin, "users").await?;
    state.users.delete_user(id, acting.id).await?;
    state
        .audit
        .record(AuditEntry {
            actor_username: Some(&acting.username),
            actor_role: Some(acting.role.as_str()),
            action: "delete",
            resource: "users",
            entity_id: Some(&id.to_string()),
            detail: None,
            origin: "tauri",
            result: "ok",
        })
        .await;
    Ok(())
}

// --- #383 段階2a / R1-B: レジストリ CRUD（PLC接続・収集グループ・タグ） ----
//
// REST 側（`chronogazer_core::rest::tag_registry_router`）と同じ floor split
// （R0 §3.6: viewer-read / editor-write）・同じ監査内容（`origin: "tauri"`
// のみ異なる）。手本は relay-wright の `src-tauri/src/lib.rs` 同名セクション。

/// #383 段階2a: chronogazer は `"modbus-tcp"`/`"slmp"` だけを受け付ける。
/// `chronogazer_core::rest::reject_disallowed_connection_protocol`の doc
/// comment と同じ理由。REST 側と Tauri 側の両方から呼ぶ - 片方だけに書くと
/// もう片方の経路から `"virtual"`/`"postgres"` 接続を作れてしまう
/// （invariant: 両経路対称）。
fn reject_disallowed_connection_protocol(protocol: &str) -> Result<(), BantoError> {
    const ALLOWED: [&str; 2] = ["modbus-tcp", "slmp"];
    if ALLOWED.contains(&protocol) {
        return Ok(());
    }
    Err(BantoError::Validation {
        field_errors: vec![FieldError {
            field: "protocol".to_string(),
            message: format!(
                "chronogazer が対応するプロトコルは {} のいずれかです",
                ALLOWED.join(", ")
            ),
        }],
    })
}

/// `viewer`+ (R0 §3.6): list PLC connections.
#[tauri::command]
async fn plc_connections_list(
    state: State<'_, AppState>,
) -> Result<Vec<PlcConnectionResponse>, BantoError> {
    require_role(&state, Role::Viewer, "plc_connections").await?;
    Ok(state
        .plc_connections
        .list(ListParams::default())
        .await?
        .rows
        .into_iter()
        .map(PlcConnectionResponse::from)
        .collect())
}

/// `viewer`+ (R0 §3.6): fetch one PLC connection.
#[tauri::command]
async fn plc_connections_get(
    state: State<'_, AppState>,
    id: i64,
) -> Result<PlcConnectionResponse, BantoError> {
    require_role(&state, Role::Viewer, "plc_connections").await?;
    Ok(PlcConnectionResponse::from(
        state.plc_connections.get(id).await?,
    ))
}

/// `editor`+ (R0 §3.6): create a PLC connection.
#[tauri::command]
async fn plc_connections_create(
    state: State<'_, AppState>,
    input: PlcConnectionPayload,
) -> Result<PlcConnectionResponse, BantoError> {
    let actor = require_role(&state, Role::Editor, "plc_connections").await?;
    reject_disallowed_connection_protocol(&input.protocol)?;
    let created = state.plc_connections.create(input.into()).await?;
    state
        .audit
        .record(AuditEntry {
            actor_username: Some(&actor.username),
            actor_role: Some(actor.role.as_str()),
            action: "create",
            resource: "plc_connections",
            entity_id: Some(&created.id.to_string()),
            detail: Some(serde_json::json!({ "name": created.name, "enabled": created.enabled })),
            origin: "tauri",
            result: "ok",
        })
        .await;
    Ok(PlcConnectionResponse::from(created))
}

/// `editor`+ (R0 §3.6): update a PLC connection.
#[tauri::command]
async fn plc_connections_update(
    state: State<'_, AppState>,
    id: i64,
    input: PlcConnectionPayload,
) -> Result<PlcConnectionResponse, BantoError> {
    let actor = require_role(&state, Role::Editor, "plc_connections").await?;
    reject_disallowed_connection_protocol(&input.protocol)?;
    let updated = state.plc_connections.update(id, input.into()).await?;
    state
        .audit
        .record(AuditEntry {
            actor_username: Some(&actor.username),
            actor_role: Some(actor.role.as_str()),
            action: "update",
            resource: "plc_connections",
            entity_id: Some(&id.to_string()),
            detail: Some(serde_json::json!({ "name": updated.name, "enabled": updated.enabled })),
            origin: "tauri",
            result: "ok",
        })
        .await;
    Ok(PlcConnectionResponse::from(updated))
}

/// `editor`+ (R0 §3.6): delete a PLC connection. Refuses (friendly
/// Validation error) when a collection group still references it
/// (`PlcConnectionService::delete`'s guard).
#[tauri::command]
async fn plc_connections_delete(state: State<'_, AppState>, id: i64) -> Result<(), BantoError> {
    let actor = require_role(&state, Role::Editor, "plc_connections").await?;
    state.plc_connections.delete(id).await?;
    state
        .audit
        .record(AuditEntry {
            actor_username: Some(&actor.username),
            actor_role: Some(actor.role.as_str()),
            action: "delete",
            resource: "plc_connections",
            entity_id: Some(&id.to_string()),
            detail: None,
            origin: "tauri",
            result: "ok",
        })
        .await;
    Ok(())
}

/// `viewer`+ (R0 §3.6): list collection groups.
#[tauri::command]
async fn collection_groups_list(
    state: State<'_, AppState>,
) -> Result<Vec<CollectionGroup>, BantoError> {
    require_role(&state, Role::Viewer, "collection_groups").await?;
    Ok(state
        .collection_groups
        .list(ListParams::default())
        .await?
        .rows)
}

/// `viewer`+ (R0 §3.6): fetch one collection group.
#[tauri::command]
async fn collection_groups_get(
    state: State<'_, AppState>,
    id: i64,
) -> Result<CollectionGroup, BantoError> {
    require_role(&state, Role::Viewer, "collection_groups").await?;
    state.collection_groups.get(id).await
}

/// `editor`+ (R0 §3.6): create a collection group.
#[tauri::command]
async fn collection_groups_create(
    state: State<'_, AppState>,
    input: CollectionGroupPayload,
) -> Result<CollectionGroup, BantoError> {
    let actor = require_role(&state, Role::Editor, "collection_groups").await?;
    let created = state.collection_groups.create(input.into()).await?;
    state
        .audit
        .record(AuditEntry {
            actor_username: Some(&actor.username),
            actor_role: Some(actor.role.as_str()),
            action: "create",
            resource: "collection_groups",
            entity_id: Some(&created.id.to_string()),
            detail: Some(serde_json::json!({ "name": created.name, "enabled": created.enabled })),
            origin: "tauri",
            result: "ok",
        })
        .await;
    Ok(created)
}

/// `editor`+ (R0 §3.6): update a collection group.
#[tauri::command]
async fn collection_groups_update(
    state: State<'_, AppState>,
    id: i64,
    input: CollectionGroupPayload,
) -> Result<CollectionGroup, BantoError> {
    let actor = require_role(&state, Role::Editor, "collection_groups").await?;
    let updated = state.collection_groups.update(id, input.into()).await?;
    state
        .audit
        .record(AuditEntry {
            actor_username: Some(&actor.username),
            actor_role: Some(actor.role.as_str()),
            action: "update",
            resource: "collection_groups",
            entity_id: Some(&id.to_string()),
            detail: Some(serde_json::json!({ "name": updated.name, "enabled": updated.enabled })),
            origin: "tauri",
            result: "ok",
        })
        .await;
    Ok(updated)
}

/// `editor`+ (R0 §3.6): delete a collection group. Refuses (friendly
/// Validation error) when a tag still references it
/// (`CollectionGroupService::delete`'s guard).
#[tauri::command]
async fn collection_groups_delete(state: State<'_, AppState>, id: i64) -> Result<(), BantoError> {
    let actor = require_role(&state, Role::Editor, "collection_groups").await?;
    state.collection_groups.delete(id).await?;
    state
        .audit
        .record(AuditEntry {
            actor_username: Some(&actor.username),
            actor_role: Some(actor.role.as_str()),
            action: "delete",
            resource: "collection_groups",
            entity_id: Some(&id.to_string()),
            detail: None,
            origin: "tauri",
            result: "ok",
        })
        .await;
    Ok(())
}

/// `viewer`+ (R0 §3.6): list tags.
#[tauri::command]
async fn tags_list(state: State<'_, AppState>) -> Result<Vec<Tag>, BantoError> {
    require_role(&state, Role::Viewer, "tags").await?;
    Ok(state.tags.list(ListParams::default()).await?.rows)
}

/// `viewer`+ (R0 §3.6): fetch one tag.
#[tauri::command]
async fn tags_get(state: State<'_, AppState>, id: i64) -> Result<Tag, BantoError> {
    require_role(&state, Role::Viewer, "tags").await?;
    state.tags.get(id).await
}

/// `editor`+ (R0 §3.6): create a tag.
#[tauri::command]
async fn tags_create(state: State<'_, AppState>, input: TagPayload) -> Result<Tag, BantoError> {
    let actor = require_role(&state, Role::Editor, "tags").await?;
    let created = state.tags.create(input.into()).await?;
    state
        .audit
        .record(AuditEntry {
            actor_username: Some(&actor.username),
            actor_role: Some(actor.role.as_str()),
            action: "create",
            resource: "tags",
            entity_id: Some(&created.id.to_string()),
            detail: Some(serde_json::json!({ "name": created.name, "enabled": created.enabled })),
            origin: "tauri",
            result: "ok",
        })
        .await;
    Ok(created)
}

/// `editor`+ (R0 §3.6): update a tag.
#[tauri::command]
async fn tags_update(
    state: State<'_, AppState>,
    id: i64,
    input: TagPayload,
) -> Result<Tag, BantoError> {
    let actor = require_role(&state, Role::Editor, "tags").await?;
    let updated = state.tags.update(id, input.into()).await?;
    state
        .audit
        .record(AuditEntry {
            actor_username: Some(&actor.username),
            actor_role: Some(actor.role.as_str()),
            action: "update",
            resource: "tags",
            entity_id: Some(&id.to_string()),
            detail: Some(serde_json::json!({ "name": updated.name, "enabled": updated.enabled })),
            origin: "tauri",
            result: "ok",
        })
        .await;
    Ok(updated)
}

/// `editor`+ (R0 §3.6): delete a tag.
#[tauri::command]
async fn tags_delete(state: State<'_, AppState>, id: i64) -> Result<(), BantoError> {
    let actor = require_role(&state, Role::Editor, "tags").await?;
    state.tags.delete(id).await?;
    state
        .audit
        .record(AuditEntry {
            actor_username: Some(&actor.username),
            actor_role: Some(actor.role.as_str()),
            action: "delete",
            resource: "tags",
            entity_id: Some(&id.to_string()),
            detail: None,
            origin: "tauri",
            result: "ok",
        })
        .await;
    Ok(())
}

/// `admin`-only (spec M14): the audit-log viewer's filtered/sorted/
/// paginated read. Also opportunistically prunes first - same reasoning as
/// `chronogazer_core::rest::audit_log_list` (see that function's doc
/// comment).
#[tauri::command]
async fn audit_log_list(
    state: State<'_, AppState>,
    params: ListParams,
) -> Result<ListResult<AuditLogEntry>, BantoError> {
    require_role(&state, Role::Admin, "audit_log").await?;
    if let Ok(config) = state.settings.audit_config().await {
        let _ = state
            .audit
            .prune(config.retention_days, config.retention_rows)
            .await;
    }
    state.audit.list(params).await
}

/// Current audit-log retention policy (spec M14 Phase B). Any authenticated
/// role may read this (same rationale as `auth_config_get`: it only feeds a
/// settings-screen display) - only `audit_config_apply` below is
/// `admin`-only.
#[tauri::command]
async fn audit_config_get(state: State<'_, AppState>) -> Result<AuditSettings, BantoError> {
    require_role(&state, Role::Viewer, "settings").await?;
    state.settings.audit_config().await
}

/// `admin`-only (spec M14 Phase B): persist a new retention policy. `None`
/// on either field means unlimited on that dimension
/// (`SettingsService::set_audit_config`/`normalize_retention`) - the
/// pruning itself still only runs opportunistically from `audit_log_list`/
/// `crate::rest::audit_log_list`, not from this command.
#[tauri::command]
async fn audit_config_apply(
    state: State<'_, AppState>,
    retention_days: Option<i64>,
    retention_rows: Option<i64>,
) -> Result<AuditSettings, BantoError> {
    let actor = require_role(&state, Role::Admin, "settings").await?;
    let config = AuditSettings {
        retention_days,
        retention_rows,
    };
    state.settings.set_audit_config(&config).await?;
    state
        .audit
        .record(AuditEntry {
            actor_username: Some(&actor.username),
            actor_role: Some(actor.role.as_str()),
            action: "settings_change",
            resource: "settings",
            entity_id: None,
            detail: Some(serde_json::json!({
                "retentionDays": config.retention_days,
                "retentionRows": config.retention_rows,
            })),
            origin: "tauri",
            result: "ok",
        })
        .await;
    // Re-read rather than echo `config` back directly: `set_audit_config`/
    // `audit_config` round-trip a non-positive value as `None` (spec:
    // "0以下は「無制限」" - see `normalize_retention`), so if the caller
    // passed e.g. `Some(0)` the echoed struct would show `Some(0)` while a
    // subsequent `audit_config_get` would show `None` for the same field.
    // Re-reading keeps this command's response identical to what every
    // other reader of the setting sees.
    state.settings.audit_config().await
}

// --- M17: SQLite backup/restore ---------------------------------------------

/// Body of [`backups_create`], split out the same way [`change_own_password`]
/// is (spec M14 pattern) so the audit-recording behavior is testable with a
/// plain `&AppState` in this crate's own `cargo test` - `tauri::State`
/// cannot be constructed outside a running tauri app.
async fn backups_create_body(state: &AppState) -> Result<BackupInfo, BantoError> {
    let actor = require_role(state, Role::Admin, "backups").await?;
    let info = state.backup.create().await?;
    state
        .audit
        .record(AuditEntry {
            actor_username: Some(&actor.username),
            actor_role: Some(actor.role.as_str()),
            action: "backup",
            resource: "backups",
            entity_id: Some(&info.file_name),
            detail: Some(serde_json::json!({ "sizeBytes": info.size_bytes })),
            origin: "tauri",
            result: "ok",
        })
        .await;
    Ok(info)
}

/// `admin`-only (spec M17): create a new backup (`VACUUM INTO`).
#[tauri::command]
async fn backups_create(state: State<'_, AppState>) -> Result<BackupInfo, BantoError> {
    backups_create_body(&state).await
}

/// `admin`-only (spec M17): list existing backups, newest first. Read-only,
/// so - like `backups_pending`/`server_status` - not audited.
#[tauri::command]
async fn backups_list(state: State<'_, AppState>) -> Result<Vec<BackupInfo>, BantoError> {
    require_role(&state, Role::Admin, "backups").await?;
    state.backup.list().await
}

/// `backups_open_folder`'s response shape (spec M17): `path` is always the
/// resolved `backups/` directory; `opened` tells the frontend whether an
/// actual file-explorer window was launched, so it can show a fallback
/// message (e.g. "このOSでは非対応です。手動で開いてください: {path}") on
/// platforms this command deliberately does not attempt to support.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct OpenFolderResult {
    opened: bool,
    path: String,
}

/// `admin`-only (spec M17): open the `backups/` directory in the OS file
/// explorer. **Windows-only** by design (spec: "非Windowsはエラーでなく
/// no-op + その旨返す") - every other platform this workspace targets
/// (macOS/Linux, spec §6) gets `opened: false` instead of an `Err`, since
/// "please go look at a folder" is not worth failing the command over; the
/// frontend is expected to show `path` as a fallback instead. Not audited -
/// this only opens a window, it does not touch any data.
#[tauri::command]
async fn backups_open_folder(state: State<'_, AppState>) -> Result<OpenFolderResult, BantoError> {
    require_role(&state, Role::Admin, "backups").await?;
    let path = state.backup.backups_dir_display();

    #[cfg(target_os = "windows")]
    {
        // Best-effort: `explorer` returning a non-zero exit status (e.g. the
        // directory does not exist yet because no backup has ever been
        // created) is still reported as `opened: false` rather than an
        // `Err` - same non-fatal framing as every other OS in this command.
        let opened = std::process::Command::new("explorer")
            .arg(&path)
            .spawn()
            .is_ok();
        Ok(OpenFolderResult { opened, path })
    }

    #[cfg(not(target_os = "windows"))]
    {
        Ok(OpenFolderResult {
            opened: false,
            path,
        })
    }
}

/// Body of [`backups_stage_restore`] (spec M14 split-function pattern, see
/// [`backups_create_body`]).
async fn backups_stage_restore_body(state: &AppState, file_name: &str) -> Result<(), BantoError> {
    let actor = require_role(state, Role::Admin, "backups").await?;
    state.backup.stage_restore_from_file(file_name).await?;
    state
        .audit
        .record(AuditEntry {
            actor_username: Some(&actor.username),
            actor_role: Some(actor.role.as_str()),
            action: "restore_staged",
            resource: "backups",
            entity_id: None,
            detail: Some(serde_json::json!({ "source": "existing", "fileName": file_name })),
            origin: "tauri",
            result: "ok",
        })
        .await;
    Ok(())
}

/// `admin`-only (spec M17): stage a restore from an existing backup already
/// in `backups/`.
#[tauri::command]
async fn backups_stage_restore(
    state: State<'_, AppState>,
    file_name: String,
) -> Result<(), BantoError> {
    backups_stage_restore_body(&state, &file_name).await
}

/// `admin`-only (spec M17): the currently-staged restore, if any. Read-only,
/// not audited.
#[tauri::command]
async fn backups_pending(
    state: State<'_, AppState>,
) -> Result<Option<PendingRestoreInfo>, BantoError> {
    require_role(&state, Role::Admin, "backups").await?;
    Ok(state.backup.pending_restore().await)
}

/// Body of [`backups_cancel_restore`] (spec M14 split-function pattern).
async fn backups_cancel_restore_body(state: &AppState) -> Result<(), BantoError> {
    let actor = require_role(state, Role::Admin, "backups").await?;
    state.backup.cancel_pending_restore().await?;
    state
        .audit
        .record(AuditEntry {
            actor_username: Some(&actor.username),
            actor_role: Some(actor.role.as_str()),
            action: "restore_cancelled",
            resource: "backups",
            entity_id: None,
            detail: None,
            origin: "tauri",
            result: "ok",
        })
        .await;
    Ok(())
}

/// `admin`-only (spec M17): cancel a staged restore.
#[tauri::command]
async fn backups_cancel_restore(state: State<'_, AppState>) -> Result<(), BantoError> {
    backups_cancel_restore_body(&state).await
}

// --- #332: Hub 接続 ----------------------------------------------------------

/// Role check shared by every `hub_*` command (`admin`-only, same floor as
/// the other settings/server commands).
///
/// The DENIAL audit `resource` is **`"hub"`**, matching the REST side's
/// `hub_router` (`RoleGuard { resource: "hub", .. }` in
/// `chronogazer_core::rest`). Before this, the Tauri commands passed
/// `"settings"`, so the same rejected operation was recorded under two
/// different resources depending on which transport the caller used, and a
/// "show me every denial against hub" filter silently missed the Tauri half
/// (`users`/`backups`/`audit_log` have always agreed across both).
///
/// The SUCCESS entries below deliberately stay
/// `action: "settings_change"` / `resource: "settings"` on BOTH transports:
/// they already agree with each other, and the Hub endpoint/key/selection
/// genuinely are part of this app's settings, so re-tagging them would
/// change how existing audit logs read (a `settings` history would suddenly
/// lose its Hub entries). The asymmetry - denials under `hub`, successes
/// under `settings` - is therefore intentional and matches REST exactly.
async fn require_hub_admin(state: &AppState) -> Result<UserIdentity, BantoError> {
    require_role(state, Role::Admin, "hub").await
}

/// `GET`-ish command: 保存済み設定での Hub 接続状態（6 状態）とタグ一覧。
/// `admin` 限定（他のサーバー/設定系コマンドと同じ下限）。
///
/// **キーの発行は行わない** - 設定画面を開いただけで banto-hub にキーが
/// 増えないようにするため（発行は `hub_connect` だけ）。
#[tauri::command]
async fn hub_status(state: State<'_, AppState>) -> Result<HubView, BantoError> {
    require_hub_admin(&state).await?;
    state.hub.status().await
}

/// 接続（保存済みキーがあれば再利用、無ければ試運転中の Hub にのみ `read`
/// スコープのキーを自己発行）。`admin` 限定。
#[tauri::command]
async fn hub_connect(state: State<'_, AppState>, endpoint: String) -> Result<HubView, BantoError> {
    let actor = require_hub_admin(&state).await?;
    let view = state.hub.connect(&endpoint).await?;
    // 監査 detail に入れてよいのは接続先と結果の状態まで。平文キーはどの
    // 経路にも出さない。
    state
        .audit
        .record(AuditEntry {
            actor_username: Some(&actor.username),
            actor_role: Some(actor.role.as_str()),
            action: "settings_change",
            resource: "settings",
            entity_id: None,
            detail: Some(
                serde_json::json!({ "hubEndpoint": endpoint, "hubStatus": view.status.as_str() }),
            ),
            origin: "tauri",
            result: "ok",
        })
        .await;
    Ok(view)
}

/// 購読の状態だけ（#383 段階1）。`admin` 限定。
///
/// **ネットワークを叩かない**（メモリ上の `watch` を読むだけ）ので、設定
/// 画面はこれをポーリングしてよい。`hub_status` は catalog を毎回取り直す
/// のでポーリングには使わない。
#[tauri::command]
async fn hub_subscription(state: State<'_, AppState>) -> Result<HubSubscriptionView, BantoError> {
    require_hub_admin(&state).await?;
    Ok(state.hub.subscription().await)
}

/// タグ一覧の再取得。`admin` 限定。
#[tauri::command]
async fn hub_refresh_catalog(state: State<'_, AppState>) -> Result<HubView, BantoError> {
    require_hub_admin(&state).await?;
    state.hub.refresh_catalog().await
}

/// 選択タグの保存。空配列も正当な入力。`admin` 限定。
#[tauri::command]
async fn hub_set_selected_tags(
    state: State<'_, AppState>,
    tags: Vec<String>,
) -> Result<(), BantoError> {
    let actor = require_hub_admin(&state).await?;
    let count = tags.len();
    state.hub.set_selected_tags(tags).await?;
    state
        .audit
        .record(AuditEntry {
            actor_username: Some(&actor.username),
            actor_role: Some(actor.role.as_str()),
            action: "settings_change",
            resource: "settings",
            entity_id: None,
            detail: Some(serde_json::json!({ "hubSelectedTagCount": count })),
            origin: "tauri",
            result: "ok",
        })
        .await;
    Ok(())
}

/// ロックダウン済み Hub 向けの手動連携: 管理者が発行した API キーを採用
/// する。`admin` 限定。
///
/// `key` は**ログにも監査 detail にも出さない**（`autologin_enable` が
/// パスワードを出さないのと同じ扱い）。平文は
/// `keyring_store::KeyringKeyStore` 経由で OS キーリングにだけ入る。
#[tauri::command]
async fn hub_adopt_manual_key(
    state: State<'_, AppState>,
    endpoint: String,
    key: String,
) -> Result<HubView, BantoError> {
    let actor = require_hub_admin(&state).await?;
    let view = state.hub.adopt_manual_key(&endpoint, key).await?;
    state
        .audit
        .record(AuditEntry {
            actor_username: Some(&actor.username),
            actor_role: Some(actor.role.as_str()),
            action: "settings_change",
            resource: "settings",
            entity_id: None,
            detail: Some(serde_json::json!({
                "hubEndpoint": endpoint,
                "hubKeyAdopted": true,
                "hubStatus": view.status.as_str(),
            })),
            origin: "tauri",
            result: "ok",
        })
        .await;
    Ok(view)
}

/// 切断: ローカルの設定とキーリングだけを消す。**Hub 側のキーは失効させ
/// ない**（他のインストールを巻き込まないため - crate 側 `disconnect` の
/// doc comment参照）。`admin` 限定。
#[tauri::command]
async fn hub_disconnect(state: State<'_, AppState>) -> Result<HubView, BantoError> {
    let actor = require_hub_admin(&state).await?;
    let view = state.hub.disconnect().await?;
    state
        .audit
        .record(AuditEntry {
            actor_username: Some(&actor.username),
            actor_role: Some(actor.role.as_str()),
            action: "settings_change",
            resource: "settings",
            entity_id: None,
            detail: Some(serde_json::json!({ "hubDisconnected": true })),
            origin: "tauri",
            result: "ok",
        })
        .await;
    Ok(view)
}

// --- #383 段階2b / R1-C（C-2）: 収集の操作 -----------------------------------

/// Role check shared by the three **変更**コマンド
/// （`collect_start`/`collect_stop`/`collect_restart`）。**`editor` 以上**
/// （`admin` ではない）。読み取りだけは [`require_collect_reader`]（`viewer`
/// 以上）で、床が 2 段に分かれているのは `chronogazer_core::rest` の
/// `collect_router` とまったく同じ - どちらの床も**同じ定数**
/// （[`COLLECT_OPERATION_ROLE`] / [`COLLECT_READ_ROLE`]）から来ているので、
/// 経路によって割れようがない。
///
/// **根拠**: `docs/recorder-requirements.md` §3.6「収集の開始/停止は editor
/// 以上」と、そこに追記された **2026-09-18 のオーナー決定** - 「banto-hub は
/// 同種の制御（自動書き込みエンジンの arm/disarm 等）を admin 限定にして
/// いるが、chronogazer は別プロダクトであり R0 本文のこの決定を継承する。
/// **banto-hub の先例に引きずられて admin 限定へ変更しない**」。すぐ上の
/// [`require_hub_admin`] が `admin` なのは **Hub の接続設定**（キーの発行・
/// 保存）だからで、収集の起動停止とは別の話。
///
/// 拒否の `resource` は [`COLLECT_AUDIT_RESOURCE`]、つまり **REST と同じ
/// 定数**。`hub_*` では REST=`"hub"` / Tauri=`"settings"` と割れていて
/// （#397 P2-D）「hub への拒否だけ抽出する」フィルタが片方を取りこぼした
/// ので、ここは最初から 1 つの定数を両方から参照して割れようがなくして
/// ある。**成功側の `resource` も同じ `"collect"`** - `hub_*` に残っている
/// 「拒否は hub・成功は settings」という非対称を新しい資源に持ち込まない。
/// **床を 2 段に分けても `resource` は分けない。**
///
/// 7 つの `hub_*` が [`require_hub_admin`] を通るのと同じく、**3 つの変更
/// コマンドは全部ここを通る** - 「変更操作は editor 以上」が 1 か所で決まり、
/// 次に操作を足しても床が散らない。
async fn require_collect_editor(state: &AppState) -> Result<UserIdentity, BantoError> {
    require_role(state, COLLECT_OPERATION_ROLE, COLLECT_AUDIT_RESOURCE).await
}

/// Role check for the **読み取り**コマンド（`collect_status`）。**`viewer`
/// 以上**（2026-09-20 オーナー決定、#407 レビュー。当初は変更と同じ `editor`
/// にしていた）。
///
/// **根拠**: R0 §3.6 の viewer は「**閲覧のみ**」であって「何も見えない」では
/// ない。収集が動いているかどうかは**監視画面（C-3 / R1-D）の基本情報**で、
/// この床で公開する [`CollectorStateView`] には機微な情報が何も入っていない
/// （接続先もキーもファイルパスも無い）ので、viewer に見せて困るものが無い。
/// **内部の `CollectorState` をそのまま返さないのがその前提**（#407 レビュー
/// P2-2）- 内部の型は `StartFailed { reason }` を持っていて、その理由には
/// 接続先や `data.dir` の絶対パスが載りうる。
/// [`require_hub_admin`] が読み取りまで `admin` なのは **Hub の接続先と
/// キーの情報**を扱うからで、**収集の稼働状態はそれとは性質が違う** -
/// 先例に引きずられない、という 2026-09-18 の決定と同じ考え方。
///
/// 拒否の `resource` は変更側と同じ [`COLLECT_AUDIT_RESOURCE`]。
async fn require_collect_reader(state: &AppState) -> Result<UserIdentity, BantoError> {
    require_role(state, COLLECT_READ_ROLE, COLLECT_AUDIT_RESOURCE).await
}

/// 成功した収集操作を監査に 1 本記録する（`collect_*` 共通）。`detail` は
/// **結果の状態と `pending` だけ** - 接続先・ファイルパス・
/// `CollectorState::StartFailed` の理由といった内部の詳細は入れない
/// （`CollectorState::as_str` の doc）。`chronogazer_core::rest` の
/// `record_collect_operation` と対で、`origin` だけが違う。
///
/// **ここへ来るのは「確かに受け付けた操作」だけ**（#407 レビュー P2-1）:
/// 混雑でキューに入れられなかった依頼は `CollectorService` がエラーを返し、
/// 呼び出し側の `?` がここへ届く前に抜けるので、**未受付が `result: "ok"` /
/// `pending: true` で残ることはない**。
async fn record_collect_operation(
    state: &AppState,
    actor: &UserIdentity,
    action: &'static str,
    outcome: &CollectOutcome,
) {
    state
        .audit
        .record(AuditEntry {
            actor_username: Some(&actor.username),
            actor_role: Some(actor.role.as_str()),
            action,
            resource: COLLECT_AUDIT_RESOURCE,
            entity_id: None,
            detail: Some(serde_json::json!({
                "collectState": outcome.status.as_str(),
                "pending": outcome.pending,
            })),
            origin: "tauri",
            result: "ok",
        })
        .await;
}

/// Body of [`collect_status`]（spec M14 split-function pattern: コマンド本体を
/// 合成 [`AppState`] に対して単体テストできるようにする）。
///
/// 返すのは**公開用の [`CollectorStateView`]**（#407 レビュー P2-2）。変換は
/// `CollectorService::state_view` の 1 箇所だけで、`chronogazer_core::rest` の
/// `collect_status_handler` も**同じものを通る** - 経路によって公開する情報が
/// 割れないようにするため（`COLLECT_AUDIT_RESOURCE` や床の定数と同じ作法）。
async fn collect_status_body(state: &AppState) -> Result<CollectorStateView, BantoError> {
    require_collect_reader(state).await?;
    Ok(state.collect.state_view())
}

/// `GET`-ish command: 収集の現在の状態。**`viewer` 以上**
/// （[`require_collect_reader`] - 変更操作より 1 段低い床。理由はそちらの doc）。
///
/// **ネットワークもディスクも DB も触らない**（メモリ上の状態を読むだけ）
/// ので、画面はこれをポーリングしてよい。読み取りなので監査しない。
///
/// **起動失敗の理由はここからは見えない**（`startFailed` としか答えない）。
/// 理由が利用者へ伝わるのは [`collect_start`] の**戻り値のエラー**で、
/// 画面を再読み込みすると分からなくなるという制約が残る - 詳しくは
/// `chronogazer_core::collect` のモジュール doc
/// 「理由はどこで利用者に伝わるか」。
#[tauri::command]
async fn collect_status(state: State<'_, AppState>) -> Result<CollectorStateView, BantoError> {
    collect_status_body(&state).await
}

/// 収集の開始。`editor` 以上。
///
/// **`CollectOutcome::pending` は「失敗」ではない** - 上限
/// （`chronogazer_core::collect::COLLECT_OPERATION_TIMEOUT`）で待つのを
/// やめただけで、開始処理はアプリ側で続いている。画面は「失敗しました」では
/// なく「まだ終わっていません」と出し、続きは `collect_status` で追うこと
/// （#400 で確立した言い分けと同じ）。
///
/// **その裏返し**（#407 レビュー P2-1）: 依頼がライフサイクルタスクのキューに
/// 入らなかったときは `pending` ではなく**エラー**が返る（「処理が混み合って
/// います…この操作は実行されていません」）。`pending: true` は「後から必ず
/// 実行される」という約束なので、混雑の言い換えに使わない。**開始に失敗した
/// ときの理由もこの戻り値のエラーで伝わる** - `collect_status` は
/// `startFailed` としか答えない（`chronogazer_core::collect` のモジュール doc
/// 「理由はどこで利用者に伝わるか」）。
/// Body of [`collect_start`]（spec M14 split-function pattern）。
async fn collect_start_body(state: &AppState) -> Result<CollectOutcome, BantoError> {
    let actor = require_collect_editor(state).await?;
    let outcome = state.collect.start().await?;
    record_collect_operation(state, &actor, "start", &outcome).await;
    Ok(outcome)
}

#[tauri::command]
async fn collect_start(state: State<'_, AppState>) -> Result<CollectOutcome, BantoError> {
    collect_start_body(&state).await
}

/// 収集の停止。`editor` 以上。走っていなければ何もしない（冪等）。
/// Body of [`collect_stop`]（spec M14 split-function pattern）。
async fn collect_stop_body(state: &AppState) -> Result<CollectOutcome, BantoError> {
    let actor = require_collect_editor(state).await?;
    let outcome = state.collect.stop().await?;
    record_collect_operation(state, &actor, "stop", &outcome).await;
    Ok(outcome)
}

#[tauri::command]
async fn collect_stop(state: State<'_, AppState>) -> Result<CollectOutcome, BantoError> {
    collect_stop_body(&state).await
}

/// 収集の再起動。`editor` 以上。**レジストリ（PLC接続・収集グループ・タグ）の
/// 変更を収集へ反映する唯一の口**で、CRUD コマンドが自動でこれを呼ぶことは
/// しない - 理由は `chronogazer_core::collect` のモジュール doc
/// 「起動時の自動開始と、「収集を再起動」だけが反映の口であること」。
/// Body of [`collect_restart`]（spec M14 split-function pattern）。
async fn collect_restart_body(state: &AppState) -> Result<CollectOutcome, BantoError> {
    let actor = require_collect_editor(state).await?;
    let outcome = state.collect.restart().await?;
    record_collect_operation(state, &actor, "restart", &outcome).await;
    Ok(outcome)
}

#[tauri::command]
async fn collect_restart(state: State<'_, AppState>) -> Result<CollectOutcome, BantoError> {
    collect_restart_body(&state).await
}

// --- #383 段階2b / R1-C（C-3a）: 収集の読み出し ------------------------------
//
// 3 本とも **`viewer` 以上**（[`require_collect_reader`] - 状態の読み取りと
// 同じ床・同じ定数）で、**監査しない**（読み取りは監査しない、という全体の
// 規約）。返す型は 3 本とも `Readout` で、「**走っていない**」「**読めな
// かった**」「**読めて 0 件**」を別の値にする - 空のオブジェクトを返して
// 3 つを潰さないため（`chronogazer_core::collect` のモジュール doc
// 「読み出しの 3 つの口」）。**これらは `crate::rest` の
// `/api/collect/values|connections|events` と同じサービスの同じメソッドを
// 通る双子**なので、経路によって形も床も割れない。

/// Body of [`collect_values`]（spec M14 split-function pattern）。
async fn collect_values_body(
    state: &AppState,
) -> Result<Readout<HashMap<String, CurrentSampleView>>, BantoError> {
    require_collect_reader(state).await?;
    Ok(state.collect.values())
}

/// `GET`-ish command: タグごとの現在値。**`viewer` 以上**。
///
/// **キューもディスクも DB も触らない**（葉に公開してある現在値ハンドルの
/// `snapshot()` だけ）ので、画面はこれをポーリングしてよい - 遅い
/// `collect_start` の後ろで待たされない（#400 で潰した「画面が固まる」型を
/// 再発させないための形。`chronogazer_core::collect` のモジュール doc
/// 「現在値はキューから外して葉に公開する」）。
///
/// **品質（`good`/`bad`/`stale`）と時刻を必ず載せる** - 値だけでは「通信
/// エラーで古い値を出し続けている」のか「今読めた値」なのかを画面が言えない。
#[tauri::command]
async fn collect_values(
    state: State<'_, AppState>,
) -> Result<Readout<HashMap<String, CurrentSampleView>>, BantoError> {
    collect_values_body(&state).await
}

/// Body of [`collect_connections`]（spec M14 split-function pattern）。
async fn collect_connections_body(
    state: &AppState,
) -> Result<Readout<HashMap<String, ConnectionStatusView>>, BantoError> {
    require_collect_reader(state).await?;
    Ok(state.collect.connections().await)
}

/// `GET`-ish command: 接続ごとの状態。**`viewer` 以上**。
///
/// **3 つの結末を返しうる唯一の口**（走っていない / 読めなかった / 読めて
/// 0 件）。こちらはライフサイクルタスクのキューを通る（`banto-collect` が
/// 状態のハンドルを外に出していない）ので、遅い操作の最中は
/// `chronogazer_core::collect::COLLECT_READ_TIMEOUT` で打ち切って
/// `unavailable` を返す - 画面は「失敗しました」ではなく「**今は読めません
/// でした**」と出すこと。
#[tauri::command]
async fn collect_connections(
    state: State<'_, AppState>,
) -> Result<Readout<HashMap<String, ConnectionStatusView>>, BantoError> {
    collect_connections_body(&state).await
}

/// Body of [`collect_events_list`]（spec M14 split-function pattern）。
async fn collect_events_list_body(
    state: &AppState,
    offset: Option<u64>,
    limit: Option<u64>,
) -> Result<Readout<ListResult<CollectEventRow>>, BantoError> {
    require_collect_reader(state).await?;
    Ok(state.collect.events(EventPage::new(offset, limit)).await)
}

/// `GET`-ish command: `collect_events` の 1 ページ（**新しい順**）。
/// **`viewer` 以上**。
///
/// 総件数は `ListResult::totalCount` で返す - [`audit_log_list`]（監査ログ
/// 一覧）と同じ型・同じ綴りで、既定の取得件数も同じ 50
/// （docs/r1-plan.md の R1-C「`collect_events` のイベント一覧ページ
/// （banto 監査ログページの流儀）」）。
///
/// **`detail` 列は返さない**（`chronogazer_core::collect::CollectEventRow` の
/// doc）: 自由文で、切断理由（接続先を含みうる）や書き込みエラー（ファイル
/// パスを含みうる）が入っている列なので、型でも SQL でも落としてある。
///
/// **収集が止まっていても読める**（`notRunning` を返さない）- 過去の記録で
/// あって、走っているエンジンの覗き窓ではないため。
#[tauri::command]
async fn collect_events_list(
    state: State<'_, AppState>,
    offset: Option<u64>,
    limit: Option<u64>,
) -> Result<Readout<ListResult<CollectEventRow>>, BantoError> {
    collect_events_list_body(&state, offset, limit).await
}

/// How long [`shutdown_app_state`] is allowed to take in total before the
/// app gives up and exits anyway (#383 R1-C's prerequisite).
///
/// **Why a budget at all**: every step below can, in principle, wait on
/// something outside this process - the LAN server's graceful shutdown waits
/// for in-flight requests (a LAN browser's long-lived `GET /api/events` SSE
/// stream is exactly such a connection), and `SqlitePool::close` waits for
/// every checked-out connection to come back. A window closing must never
/// leave a process the user cannot see hanging around; **freezing on exit is
/// worse than losing some of the cleanup**, because the OS tears the sockets
/// and the file handles down for us anyway (SQLite recovers a `-wal` on the
/// next launch by design).
///
/// **Why 5 seconds**: everything here is local (a WS close frame to a Hub on
/// the same PC/LAN, an in-process axum shutdown, an SQLite close) and
/// normally completes in milliseconds. 5 seconds is far enough above that to
/// never cut a healthy shutdown short, and short enough that a user who
/// closed the window does not notice.
const EXIT_CLEANUP_BUDGET: std::time::Duration = std::time::Duration::from_secs(5);

/// The app's exit cleanup, split out of [`run`]'s `RunEvent::Exit` arm so it
/// can be unit-tested against a synthetic [`AppState`] - see
/// `exit_cleanup_closes_the_pool`, and [`run`] for what that test does NOT
/// cover.
///
/// **Order matters**, and it is "消費者を先に止め、書き手はその後、みんなが
/// 書き込む先は最後":
///
/// 1. **Hub subscriptions** - closes the WS generation and stops the
///    supervisor, so nothing keeps waking up to touch the settings DB or the
///    network while we are tearing the rest down.
/// 2. **The embedded LAN server** - stops accepting new requests and lets
///    in-flight ones finish. Its handlers use the same pool, so it has to be
///    down before step 4.
/// 3. **収集（#383 段階2b / R1-C）** - **消費者を全部止めた後、DB プールを
///    閉じる前**。収集は PLC から読んだ値を tstore と `collect_events` に
///    書く**書き手**なので、`banto-hub` の `RunningHub::shutdown`
///    （`apps/banto-hub/core/src/runtime.rs`）が採っているのと同じ方針、
///    すなわち「read-only な消費者（LAN サーバー・Hub 購読）を先に止め、
///    収集は依存が全部止まってから」に揃える。先に収集を止めてしまうと、まだ
///    生きている消費者が「値が止まった収集エンジン」を観測する時間帯が
///    できる。逆に DB プールより後ろへ回すと、最終 flush の書き込み先が
///    もう閉じている。
/// 4. **The SQLite pool** - closed last, once nothing can still use it.
///
/// Every step is best-effort: `HubService::shutdown` and `RunningServer::stop`
/// swallow their own failures, `CollectorService::stop` の失敗（最終 flush）は
/// ログに出すだけ、そして the whole sequence is capped by
/// [`EXIT_CLEANUP_BUDGET`].
///
/// **テストの範囲**（#396 と同じ書き方）: この関数の**中身**は
/// `exit_cleanup_closes_the_pool` が合成 [`AppState`] に対して実行して
/// いる（#383 R1-C で収集の停止もそこに足した）が、**`tauri::RunEvent::Exit`
/// から実際にここへ来る経路は自動テストしていない** - 理由と、それでも
/// そこを試そうとした場合に何が要るかは [`run`] の `RunEvent::Exit` 腕の
/// コメントに書いてある。
///
/// [`EXIT_CLEANUP_BUDGET`] は **5 秒のまま据え置いた**（#383 R1-C）。
/// 収集の停止は「接続タスクを join して tstore を最終 flush する」だけで、
/// どれもローカルのディスク操作。C-2 で起動時の自動開始が入ったので、
/// **ここが実際に予算を食うのは今からが初めて** - **実機で 5 秒に収まらない
/// ことが分かったら、勝手に増やさずオーナーに報告すること。**
///
/// 収集の停止は自分でも上限を持つ（`COLLECT_OPERATION_TIMEOUT` = 30 秒）が、
/// **この 5 秒の方が短いので、終了経路では必ずこちらが先に切る** - 窓を
/// 閉じた利用者を 30 秒待たせないため（意図どおり）。
///
/// 予算が切れたときに何が起きるか（#406 レビュー P2 の後）:
/// `CollectorService::stop` は**自分のライフサイクルタスクに依頼して待つ**
/// だけなので、打ち切られるのは**待つ側だけ**で、タスク側の停止処理
/// （接続タスクの join と最終 flush）は**そのまま続く**。直後にプロセスが
/// 終わるので実害は無く、失われうるのは最後の未 flush 分だけ - 詳しくは
/// `chronogazer_core::collect` のモジュール doc「終了フックからの停止」。
async fn shutdown_app_state(state: &AppState) {
    let cleanup = async {
        // #383 段階1 の購読世代 + 見張り。`shutdown()` は操作ロックを
        // 無条件には待たない（`connect` の 60 秒で終了が固まるため）-
        // 理由は `chronogazer_core::hub::HubService::shutdown` の doc。
        state.hub.shutdown().await;

        // `server_apply` が停止に使っているのと同じ経路（`take()` して
        // `RunningServer::stop()`）。`take()` しておくのは、ここで止めたものを
        // 残しておく意味が無いのと、二重に stop されないようにするため。
        if let Some(running) = state.server.lock().await.take() {
            running.stop().await;
        }

        // #383 段階2b / R1-C: 収集（書き手）は消費者の後・DB の前 - 順序の
        // 理由はこの関数の doc。走っていなければ `stop()` は何もしない
        // （冪等）。失敗しても終了は止めない（失われうるのは tstore の
        // 最後の未 flush 分だけで、固まる方が悪い）。
        //
        // `CollectOutcome::pending`（`stop()` 自身の上限
        // `COLLECT_OPERATION_TIMEOUT` = 30 秒）は**ここでは見ない**:
        // それより短い [`EXIT_CLEANUP_BUDGET`]（5 秒）が必ず先に切れて
        // 上のメッセージを出すので、この経路で `pending` が立つことはない。
        if let Err(err) = state.collect.stop().await {
            eprintln!("banto: 終了時の収集の停止に失敗しました: {err}");
        }

        // 最後に DB。`close()` は全接続が返るのを待ってからプールを閉じる
        // ので、`-wal`/`-shm` が畳まれた状態でプロセスが終わる。
        state.pool.close().await;
    };

    if tokio::time::timeout(EXIT_CLEANUP_BUDGET, cleanup)
        .await
        .is_err()
    {
        eprintln!(
            "banto: 終了時の後始末が{}秒以内に終わらなかったため、途中で切り上げて終了します。",
            EXIT_CLEANUP_BUDGET.as_secs()
        );
    }
}

pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let data_dir = app.path().app_data_dir().expect("resolve app data dir");
            std::fs::create_dir_all(&data_dir).expect("create app data dir");
            let db_path = data_dir.join("chronogazer.sqlite3");

            // Spec M17: apply any staged restore BEFORE `init_db`/the pool is
            // created - see `BackupService::apply_pending_restore_at_startup`'s
            // doc comment for why this must run first (no pool may exist yet
            // when a restore is applied). Best-effort at this top level: a
            // failure here must never prevent the desktop app from starting
            // at all - the current db (if any) is left untouched on error,
            // per that function's own per-step safety notes.
            let applied_restore = match tauri::async_runtime::block_on(
                BackupService::apply_pending_restore_at_startup(&db_path),
            ) {
                Ok(applied) => applied,
                Err(err) => {
                    eprintln!("banto: 起動時のリストア適用に失敗しました: {err}");
                    None
                }
            };

            // init_db takes a filesystem path (not a sqlite:// URL) so
            // Windows paths with drive letters/backslashes work unchanged.
            let pool =
                tauri::async_runtime::block_on(init_db(&db_path)).expect("init_db should succeed");

            let events = event_channel();
            let users = UsersService::new(pool.clone());
            let settings = SettingsService::new(pool.clone());
            let backup = BackupService::new(db_path.clone(), pool.clone());
            // #383 段階2a / R1-B: レジストリ3サービス。テーブルは
            // `init_db` が呼ぶ `banto_tags::migrate` で既に作成済み。
            let plc_connections = PlcConnectionService::new(pool.clone());
            let collection_groups = CollectionGroupService::new(pool.clone());
            let tags = TagService::new(pool.clone());
            // #383 段階2b / R1-C: 収集ランタイム。`data.dir` は設定から読み、
            // **相対パスはこのアプリのデータディレクトリ基準**で解決する
            // （既定 `"./data"` をそのまま使うと、プロセスの作業ディレクトリ
            // という当てにならない場所に時系列ファイルを作ってしまう）。
            // `retention.days` はここでは読まない - **収集はファイルを一切
            // 削除しない**（`chronogazer_core::settings::StoreSettings` の
            // doc 参照）。
            //
            // 開始は下の `autostart` まで待つ（`AppState` の他の材料が
            // 揃ってから、ランタイムの上で spawn したいため）。
            let store_settings = tauri::async_runtime::block_on(settings.store_config())
                .expect("store_config should succeed");
            let collect = CollectorService::new(
                pool.clone(),
                resolve_data_dir(&data_dir, &store_settings.data_dir),
            );
            let audit = AuditLogService::new(pool.clone());
            // Records `login`/`login_failed` audit entries (spec M14) from
            // inside the verifier itself - see
            // `chronogazer_core::rest::audited_credential_verifier`'s doc
            // comment. This is the embedded LAN server's OWN session
            // (`origin: "rest"`) - the webview's session goes through
            // `auth_login` below instead.
            let rest_auth = AuthState::new(audited_credential_verifier(users.clone(), audit.clone()));

            // Spec M17: record `restore_applied` now that a real
            // `AuditLogService` exists - `apply_pending_restore_at_startup`
            // itself cannot record this (it runs before any pool/audit
            // service exists at all). No caller identity exists at this
            // point either (nobody has logged in yet) - mirrors how the
            // auth-disabled bootstrap's synthetic `login` entry below has no
            // "real" actor either.
            if let Some(applied) = &applied_restore {
                tauri::async_runtime::block_on(audit.record(AuditEntry {
                    actor_username: None,
                    actor_role: None,
                    action: "restore_applied",
                    resource: "backups",
                    entity_id: None,
                    detail: Some(serde_json::json!({
                        "preRestoreBackupFileName": applied.pre_restore_backup_file_name,
                    })),
                    origin: "tauri",
                    result: "ok",
                }));
            }

            // Startup prune (spec M14: "アプリ起動時に1回 + list実行時に軽く" -
            // see `audit_log_list`'s doc comment for why no dedicated
            // background task is needed beyond this plus that opportunistic
            // prune). Best-effort: a prune failure must never block startup.
            match tauri::async_runtime::block_on(settings.audit_config()) {
                Ok(config) => {
                    if let Err(err) = tauri::async_runtime::block_on(
                        audit.prune(config.retention_days, config.retention_rows),
                    ) {
                        eprintln!("banto: 起動時の監査ログの剪定に失敗しました: {err}");
                    }
                }
                Err(err) => eprintln!("banto: 監査ログの保持設定の読み取りに失敗しました: {err}"),
            }

            // Forward every resource-change/notice event onto the webview
            // (spec §3.5's TauriEventProvider side: the webview has no
            // network, so it cannot use the SSE endpoint a LAN browser
            // client uses - `banto://event` is the in-process equivalent,
            // fed by the SAME broadcast channel the REST server's SSE route
            // fans out to browsers while running).
            let app_handle = app.handle().clone();
            let mut events_rx = events.subscribe();
            tauri::async_runtime::spawn(async move {
                loop {
                    match events_rx.recv().await {
                        Ok(event) => {
                            let _ = app_handle.emit("banto://event", event);
                        }
                        // A slow/absent listener fell behind: skip the gap
                        // rather than tearing down the forwarding task.
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            });

            // M11 bootstrap: decide the webview's starting session before
            // anything else, in priority order -
            //   1. auth-disabled mode ("ログイン不要モード") - a synthetic
            //      identity, no login screen at all.
            //   2. desktop autologin - verify a keyring-stored credential
            //      against `users`, same as a normal login.
            //   3. neither - the ordinary login screen (`auth: None`).
            let auth_config = tauri::async_runtime::block_on(settings.auth_config())
                .expect("auth_config should succeed");
            let initial_auth: Option<UserIdentity> = if auth_config.disabled {
                // `id: 0` is not a real `users` row - nothing here ever looks
                // it up by id (no change-password/self-deletion flows apply
                // to a synthetic session), so there is no real row to alias.
                let local_identity = UserIdentity {
                    id: 0,
                    username: "local".to_string(),
                    display_name: "ローカルユーザー".to_string(),
                    role: auth_config.disabled_role,
                };
                // Spec M14: auth-disabled mode still records a `login` for
                // its synthetic session, same as a normal login would - it
                // is still "someone" starting to use the app, just without a
                // credential check.
                tauri::async_runtime::block_on(audit.record(AuditEntry {
                    actor_username: Some(&local_identity.username),
                    actor_role: Some(local_identity.role.as_str()),
                    action: "login",
                    resource: "auth",
                    entity_id: None,
                    detail: Some(serde_json::json!({ "mode": "auth_disabled" })),
                    origin: "tauri",
                    result: "ok",
                }));
                Some(local_identity)
            } else if auth_config.autologin_enabled {
                match &auth_config.autologin_username {
                    Some(username) => match keyring_store::get_password(username) {
                        Ok(password) => {
                            match tauri::async_runtime::block_on(users.verify(username, &password))
                            {
                                Ok(Some(identity)) => {
                                    tauri::async_runtime::block_on(audit.record(AuditEntry {
                                        actor_username: Some(&identity.username),
                                        actor_role: Some(identity.role.as_str()),
                                        action: "login",
                                        resource: "auth",
                                        entity_id: None,
                                        detail: Some(serde_json::json!({ "via": "autologin" })),
                                        origin: "tauri",
                                        result: "ok",
                                    }));
                                    Some(identity)
                                }
                                Ok(None) => {
                                    // Credentials no longer valid (e.g. the
                                    // password was changed since autologin
                                    // was set up) - spec M11: do NOT
                                    // auto-disable the setting, just fall
                                    // through to the login screen.
                                    eprintln!(
                                        "banto: 自動ログインの資格情報が無効です（パスワード変更等）。ログイン画面を表示します。"
                                    );
                                    tauri::async_runtime::block_on(audit.record(AuditEntry {
                                        actor_username: Some(username),
                                        actor_role: None,
                                        action: "login_failed",
                                        resource: "auth",
                                        entity_id: None,
                                        detail: Some(serde_json::json!({ "via": "autologin" })),
                                        origin: "tauri",
                                        result: "failed",
                                    }));
                                    None
                                }
                                Err(err) => {
                                    eprintln!("banto: 自動ログインの検証に失敗しました: {err}");
                                    tauri::async_runtime::block_on(audit.record(AuditEntry {
                                        actor_username: Some(username),
                                        actor_role: None,
                                        action: "login_failed",
                                        resource: "auth",
                                        entity_id: None,
                                        detail: Some(serde_json::json!({ "via": "autologin" })),
                                        origin: "tauri",
                                        result: "failed",
                                    }));
                                    None
                                }
                            }
                        }
                        Err(err) => {
                            // Keyring entry missing / backend unavailable -
                            // safe degrade to the login screen (spec M11).
                            eprintln!("banto: 自動ログインの資格情報の取得に失敗しました: {err}");
                            None
                        }
                    },
                    None => None,
                }
            } else {
                None
            };

            // #332: Hub 接続。`HubService::new` は `installation_id` を設定
            // から読む（無ければ生成して保存する）ので非同期 - 他の起動時の
            // 読み取りと同じく `block_on` する。失敗は起動を止める（設定 DB
            // が書けない状態であり、他のサービスも同様に `expect` している）。
            let hub = tauri::async_runtime::block_on(HubService::new(
                settings.clone(),
                std::sync::Arc::new(keyring_store::KeyringKeyStore),
            ))
            .expect("HubService should initialize");

            // #383 段階1: 保存済みの接続と選択タグがあれば、設定画面を
            // 開かなくても購読を張り直す。Hub が落ちている・キーが無効に
            // なっている等はここでは起動を止める理由にならないので、
            // spawn して投げっぱなしにする（`resume` 自身が失敗を飲んで
            // ログに出す）。
            {
                let hub = hub.clone();
                tauri::async_runtime::spawn(async move { hub.resume().await });
            }

            // #383 段階2b / R1-C（C-2）: 収集の**起動時の自動開始**
            // （docs/r1-plan.md の R1-C「起動時に build_config → start」）。
            // すぐ上の `hub.resume()` と同じ形 - **spawn して投げっぱなし**に
            // するので、tstore を開くのに手間取っても起動画面が待たされず、
            // **失敗しても起動は止まらない**（理由は状態に残り、
            // `collect_status` から見える。収集対象 0 件は失敗ではない）。
            //
            // **レジストリを後から編集しても自動では再起動しない** - 反映は
            // 明示的な `collect_restart` だけ（理由は
            // `chronogazer_core::collect` のモジュール doc）。
            //
            // **注意**: このアプリと `banto-serve` を同時に起動して同じ
            // `data.dir` を指すと二重書き込みになる（防止機構は未実装 -
            // 同モジュール doc「同じ `data.dir` を 2 つのプロセスで
            // 開かないこと」）。
            {
                let collect = collect.clone();
                tauri::async_runtime::spawn(async move { collect.autostart().await });
            }

            // If LAN access was left enabled on a previous run, start the
            // server immediately (spec §11.4) - from here on, the settings
            // screen only needs to *change* state via `server_apply`.
            //
            // Spec M11 exclusivity is enforced at write-time
            // (`SettingsService::set_server_config`/`set_auth_config`), but a
            // hand-edited settings DB could still leave both
            // `auth.disabled` and `server.enabled` set to `true` at once - if
            // so, refuse to auto-start the (would-be unauthenticated) LAN
            // server rather than trust a state the app itself would never
            // have written, and leave the inconsistency for the user to
            // resolve from the settings screen (this does NOT rewrite either
            // setting).
            let server_config = tauri::async_runtime::block_on(settings.server_config())
                .expect("server_config should succeed");
            let inconsistent_auth_and_server = auth_config.disabled && server_config.enabled;
            if inconsistent_auth_and_server {
                eprintln!(
                    "banto: 認証無効モードとLANアクセスが同時に有効な不整合な設定を検出したため、LANサーバーの自動起動をスキップしました。設定画面でどちらかを無効にしてください。"
                );
            }
            let initial_server = if server_config.enabled && !inconsistent_auth_and_server {
                let runtime_config = ServerConfig {
                    bind: server_config.bind.clone(),
                    port: server_config.port,
                };
                match tauri::async_runtime::block_on(start_embedded_server(
                    users.clone(),
                    settings.clone(),
                    audit.clone(),
                    backup.clone(),
                    hub.clone(),
                    plc_connections.clone(),
                    collection_groups.clone(),
                    tags.clone(),
                    collect.clone(),
                    rest_auth.clone(),
                    events.clone(),
                    runtime_config,
                )) {
                    Ok(server) => Some(server),
                    Err(err) => {
                        // Non-fatal: the desktop app itself works fine with
                        // no LAN access; surface the failure (e.g. the
                        // persisted port now being in use) to the log only.
                        // The settings screen's `server_status` will report
                        // `running: false` and the user can pick a different
                        // port via `server_apply`.
                        eprintln!("banto: 起動時のLANアクセス開始に失敗しました: {err}");
                        None
                    }
                }
            } else {
                None
            };

            // M12: re-apply the persisted vibrancy (Windows Acrylic) choice
            // on launch. Best-effort by design - a failure (old Windows 10
            // build, missing window) must never block startup, so it is
            // logged and otherwise ignored; the settings screen's
            // `vibrancy_status`/`vibrancy_apply` remain the way to
            // observe/repair the state.
            #[cfg(target_os = "windows")]
            {
                let vibrancy_enabled = tauri::async_runtime::block_on(
                    settings.get(KEY_DESKTOP_VIBRANCY),
                )
                .unwrap_or_else(|err| {
                    eprintln!("banto: vibrancy設定の読み取りに失敗しました: {err}");
                    None
                })
                .map(|value| value == "true")
                .unwrap_or(false);
                if vibrancy_enabled {
                    match app.get_webview_window("main") {
                        Some(window) => {
                            if let Err(err) = set_window_vibrancy(&window, true) {
                                eprintln!(
                                    "banto: 起動時のウィンドウAcrylic効果の適用に失敗しました: {err}"
                                );
                            }
                        }
                        None => eprintln!(
                            "banto: メインウィンドウが見つからないため、起動時のAcrylic効果の適用をスキップしました"
                        ),
                    }
                }
            }

            app.manage(AppState {
                auth: Mutex::new(initial_auth),
                users,
                settings,
                events,
                rest_auth,
                server: AsyncMutex::new(initial_server),
                audit,
                backup,
                hub,
                plc_connections,
                collection_groups,
                tags,
                collect,
                pool,
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            ping,
            auth_status,
            auth_setup,
            auth_login,
            auth_logout,
            auth_check,
            auth_identity,
            auth_change_password,
            auth_config_get,
            auth_config_apply,
            autologin_enable,
            autologin_disable,
            server_status,
            server_apply,
            settings_get,
            settings_set,
            ui_settings_get,
            ui_settings_set,
            vibrancy_apply,
            vibrancy_status,
            users_list,
            users_create,
            users_update,
            users_reset_password,
            users_delete,
            audit_log_list,
            audit_config_get,
            audit_config_apply,
            backups_create,
            backups_list,
            backups_open_folder,
            backups_stage_restore,
            backups_pending,
            backups_cancel_restore,
            hub_status,
            hub_subscription,
            hub_connect,
            hub_refresh_catalog,
            hub_set_selected_tags,
            hub_adopt_manual_key,
            hub_disconnect,
            plc_connections_list,
            plc_connections_get,
            plc_connections_create,
            plc_connections_update,
            plc_connections_delete,
            collection_groups_list,
            collection_groups_get,
            collection_groups_create,
            collection_groups_update,
            collection_groups_delete,
            tags_list,
            tags_get,
            tags_create,
            tags_update,
            tags_delete,
            collect_status,
            collect_start,
            collect_stop,
            collect_restart,
            collect_values,
            collect_connections,
            collect_events_list,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            // #383 R1-C の前提: アプリの event loop が終わるときに、この
            // プロセスが持っている外向きの資源を明示的に閉じる。`AppState`
            // は Tauri の managed state なので**プロセス終了で Drop が
            // 走らない** - ここを通らない限り、Hub の購読 WS は close を
            // 送らずに切れ、LAN サーバーは graceful shutdown せず、SQLite の
            // `-wal`/`-shm` が残る。収集エンジンを同じプロセスに載せる
            // オーナー決定（2026-09-18、docs/recorder-requirements.md §4）の
            // 下では、**ここが唯一の「止める場所」**になる。
            //
            // Best effort: `try_state` が `None`（`setup()` が
            // `app.manage(..)` に到達する前に落ちた）なら閉じるものは無い。
            // 全体の上限は `shutdown_app_state` が持つ。
            //
            // **テストの範囲**（#396 のレビュー C で訂正）:
            //
            // * 後始末の中身は `shutdown_app_state` の単体テスト
            //   （`exit_cleanup_closes_the_pool`）で押さえてある。
            // * `tauri::RunEvent::Exit` **自体は構築できる**（`#[non_exhaustive]`
            //   が付いているのは enum であって `Exit` variant ではない。
            //   このクレートで `let e = tauri::RunEvent::Exit;` が通ることを
            //   実際に確かめた）。したがってイベントハンドラを切り出して
            //   `Exit` を渡すテストは**書ける**。書いていないのは費用対効果の
            //   判断: 切り出した関数は `AppHandle` を要求し、それを作るには
            //   `tauri::test` のモックランタイム（このクレートでは未有効）で
            //   アプリを組み立てる必要がある一方、確かめられるのは
            //   「`Exit` のときだけ `shutdown_app_state` を呼ぶ」という
            //   3 行の分岐だけになる。
            // * 「OS の終了 → Tauri のイベントループ → このコールバック」と
            //   いう**経路全体**は、どのみち通常の unit test では再現できない
            //   （このクレートは CI の Linux コンテナでビルドすらできない）。
            //   そこは実機確認の領域 - relay-wright の W3-B2 と同じ状況。
            if let tauri::RunEvent::Exit = event {
                if let Some(state) = app_handle.try_state::<AppState>() {
                    tauri::async_runtime::block_on(shutdown_app_state(&state));
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A retry-cleanup temp dir wrapper - see `chronogazer_core::backup`'s
    /// `crate::test_support` module doc (this crate's `AppState` tests hit
    /// the exact same Windows WAL-close-timing leak: measured, one
    /// `cargo test -p chronogazer --lib` run left directories behind, one
    /// per [`app_state_with_tempdir`] call). Plain `tempfile::TempDir`'s
    /// single, non-retrying `remove_dir_all` (which also silently swallows
    /// its error) cannot reliably clean up here. Requires a multi-thread
    /// tokio runtime with >= 2 workers, same reasoning as that module.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("tempdir").keep();
            Self(dir)
        }

        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    const TEMP_DIR_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(50);
    const TEMP_DIR_MAX_ATTEMPTS: u32 = 40;

    impl Drop for TempDir {
        fn drop(&mut self) {
            for attempt in 1..=TEMP_DIR_MAX_ATTEMPTS {
                match std::fs::remove_dir_all(&self.0) {
                    Ok(()) => return,
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => return,
                    Err(_) if attempt < TEMP_DIR_MAX_ATTEMPTS => {
                        std::thread::sleep(TEMP_DIR_RETRY_DELAY);
                    }
                    Err(err) => {
                        eprintln!(
                            "TempDir: giving up removing {:?} after {attempt} attempts: {err}",
                            self.0
                        );
                    }
                }
            }
        }
    }

    /// A minimal [`AppState`] over an in-memory DB, no running server, and a
    /// dummy REST verifier - just enough state to exercise command bodies
    /// (like [`change_own_password`]) that only touch the service handles.
    async fn app_state() -> AppState {
        let pool = chronogazer_core::db::init_db_memory()
            .await
            .expect("init_db_memory");
        let events = event_channel();
        let settings = SettingsService::new(pool.clone());
        // #332: tests never touch a real OS keyring - `UnavailableKeyStore`
        // reads as "no entry" and refuses to write, so no test can leave a
        // plaintext key in the developer's keychain.
        let hub = HubService::new(
            settings.clone(),
            std::sync::Arc::new(chronogazer_core::hub::UnavailableKeyStore),
        )
        .await
        .expect("HubService::new");
        AppState {
            auth: Mutex::new(None),
            users: UsersService::new(pool.clone()),
            settings,
            events,
            rest_auth: AuthState::new(|_u: String, _p: String| {
                Box::pin(async { None::<banto_server::Identity> })
            }),
            server: AsyncMutex::new(None),
            audit: AuditLogService::new(pool.clone()),
            backup: BackupService::new(
                PathBuf::from("unused-in-tests").join("chronogazer.sqlite3"),
                pool.clone(),
            ),
            hub,
            plc_connections: PlcConnectionService::new(pool.clone()),
            collection_groups: CollectionGroupService::new(pool.clone()),
            tags: TagService::new(pool.clone()),
            // 収集は起動しないので `data_dir` は使われない（`backup` の
            // `"unused-in-tests"` と同じ扱い）。
            collect: CollectorService::new(pool.clone(), PathBuf::from("unused-in-tests")),
            pool,
        }
    }

    /// Like [`app_state`], but backed by a REAL on-disk db in a fresh temp
    /// directory rather than `:memory:` - required for the M17 backup tests
    /// below, since `BackupService::create`'s `VACUUM INTO` silently writes
    /// nothing when its source pool is `:memory:` (see
    /// `chronogazer_core::backup`'s test module doc comment for the
    /// empirically-verified reason). The returned `TempDir` guard must be
    /// kept alive by the caller for as long as `AppState` is still in use.
    ///
    /// Returns `(dir, state)` - **dir first, state last** - not the more
    /// natural-looking `(state, dir)`. Every call site destructures this
    /// directly, and a tuple's bindings from one `let` pattern drop in
    /// *reverse* of how they're listed - `state` (which holds the pool)
    /// must be listed last so it drops before `dir`'s cleanup runs. Getting
    /// this order backwards silently leaks the temp dir on every run
    /// (measured before this fix).
    async fn app_state_with_tempdir() -> (TempDir, AppState) {
        let dir = TempDir::new();
        let db_path = dir.path().join("chronogazer.sqlite3");
        let pool = chronogazer_core::db::init_db(&db_path)
            .await
            .expect("init_db");
        let events = event_channel();
        let settings = SettingsService::new(pool.clone());
        let hub = HubService::new(
            settings.clone(),
            std::sync::Arc::new(chronogazer_core::hub::UnavailableKeyStore),
        )
        .await
        .expect("HubService::new");
        let state = AppState {
            auth: Mutex::new(None),
            users: UsersService::new(pool.clone()),
            settings,
            events,
            rest_auth: AuthState::new(|_u: String, _p: String| {
                Box::pin(async { None::<banto_server::Identity> })
            }),
            server: AsyncMutex::new(None),
            audit: AuditLogService::new(pool.clone()),
            backup: BackupService::new(db_path, pool.clone()),
            hub,
            plc_connections: PlcConnectionService::new(pool.clone()),
            collection_groups: CollectionGroupService::new(pool.clone()),
            tags: TagService::new(pool.clone()),
            collect: CollectorService::new(pool.clone(), dir.path().join("data")),
            pool,
        };
        (dir, state)
    }

    /// #383 R1-C's prerequisite: the exit cleanup closes the pool (and does
    /// not hang doing it). This covers the BODY of the exit hook only - see
    /// [`run`]'s comment for exactly what is and is not tested around it
    /// (corrected in #396's review C).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn exit_cleanup_closes_the_pool() {
        let state = app_state().await;
        assert!(!state.pool.is_closed(), "前提: 起動中はプールが開いている");

        // The budget is the outer bound; nothing here should take anywhere
        // near it, so a plain call is enough to catch a hang (the test would
        // never finish) as well as the wrong end state.
        shutdown_app_state(&state).await;

        assert!(state.pool.is_closed(), "DBプールが閉じている");
        assert_eq!(
            state.hub.subscription().await.state,
            "stopped",
            "購読は止まっている"
        );
        // #383 段階2b / R1-C: 収集も止まっている。この合成 `AppState` は
        // 自動開始を通らない（`autostart()` を呼ぶのは `run()` の `setup()`
        // だけ）ので、走っていない相手に `stop()` が「何もしない」で通る
        // ことの確認も兼ねている。
        assert_eq!(
            state.collect.state(),
            chronogazer_core::collect::CollectorState::Stopped,
            "収集は止まっている"
        );
    }

    /// Spec M14: the Tauri-side self-service password change must be
    /// recorded as `password_change` (actor = entity = the caller), and the
    /// entry's `detail` must never carry the password.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn change_own_password_is_recorded_as_password_change() {
        let state = app_state().await;
        let owner = state
            .users
            .setup_first_user("owner", "password123", "オーナー")
            .await
            .expect("setup_first_user");
        let owner_id = owner.id;
        *state.auth.lock().expect("auth mutex poisoned") = Some(owner);

        change_own_password(&state, "password123", "newpassword1")
            .await
            .expect("change_own_password should succeed");

        let result = state
            .audit
            .list(ListParams::default())
            .await
            .expect("audit list");
        let entry = result
            .rows
            .iter()
            .find(|r| r.action == "password_change")
            .unwrap_or_else(|| panic!("expected a password_change entry, got {:?}", result.rows));
        assert_eq!(entry.actor_username.as_deref(), Some("owner"));
        assert_eq!(entry.actor_role.as_deref(), Some("admin"));
        assert_eq!(entry.resource, "users");
        assert_eq!(
            entry.entity_id.as_deref(),
            Some(owner_id.to_string().as_str())
        );
        assert_eq!(entry.origin, "tauri");
        assert_eq!(entry.result, "ok");
        assert_eq!(entry.detail, None, "detail must never carry the password");
    }

    /// A FAILED password change (wrong current password) must record
    /// nothing - only the success path is a completed security event.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn failed_change_own_password_records_nothing() {
        let state = app_state().await;
        let owner = state
            .users
            .setup_first_user("owner", "password123", "オーナー")
            .await
            .expect("setup_first_user");
        *state.auth.lock().expect("auth mutex poisoned") = Some(owner);

        change_own_password(&state, "not-the-password", "newpassword1")
            .await
            .expect_err("wrong current password should fail");

        let result = state
            .audit
            .list(ListParams::default())
            .await
            .expect("audit list");
        assert!(
            result.rows.iter().all(|r| r.action != "password_change"),
            "a failed change must not be recorded as password_change: {:?}",
            result.rows
        );
    }

    // --- M17: SQLite backup/restore -------------------------------------------

    /// `admin` can create a backup, and it is recorded as `action: "backup"`
    /// with `entityId` = the created file name (spec M17).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn backups_create_records_a_backup_audit_entry() {
        let (_dir, state) = app_state_with_tempdir().await;
        let admin = state
            .users
            .create_user("admin", "password123", "管理者", Role::Admin)
            .await
            .expect("create_user");
        *state.auth.lock().expect("auth mutex poisoned") = Some(admin);

        let info = backups_create_body(&state)
            .await
            .expect("backups_create_body should succeed");
        assert!(info.file_name.starts_with("banto-"));
        assert!(info.size_bytes > 0);

        let listed = state.backup.list().await.expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].file_name, info.file_name);

        let audit = state
            .audit
            .list(ListParams::default())
            .await
            .expect("audit list");
        let entry = audit
            .rows
            .iter()
            .find(|r| r.action == "backup")
            .unwrap_or_else(|| panic!("expected a backup entry, got {:?}", audit.rows));
        assert_eq!(entry.actor_username.as_deref(), Some("admin"));
        assert_eq!(entry.resource, "backups");
        assert_eq!(entry.entity_id.as_deref(), Some(info.file_name.as_str()));
        assert_eq!(entry.origin, "tauri");
        assert_eq!(entry.result, "ok");
    }

    /// A `viewer` cannot create a backup (spec M17: "admin以外は全API 403"
    /// on the Tauri side too).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn viewer_cannot_create_backups() {
        let (_dir, state) = app_state_with_tempdir().await;
        let viewer = state
            .users
            .create_user("viewer", "password123", "閲覧者", Role::Viewer)
            .await
            .expect("create_user");
        *state.auth.lock().expect("auth mutex poisoned") = Some(viewer);

        let err = backups_create_body(&state).await.unwrap_err();
        assert!(matches!(err, BantoError::Forbidden));
        assert!(state.backup.list().await.unwrap().is_empty());
    }

    /// Stage a restore from an existing backup, then confirm it shows up as
    /// pending - the round trip `backups_create` -> `backups_stage_restore`
    /// -> `backups_pending` (spec M17), plus the `restore_staged` audit
    /// entry.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stage_restore_then_pending_reports_it() {
        let (_dir, state) = app_state_with_tempdir().await;
        let admin = state
            .users
            .create_user("admin", "password123", "管理者", Role::Admin)
            .await
            .expect("create_user");
        *state.auth.lock().expect("auth mutex poisoned") = Some(admin);

        let info = backups_create_body(&state).await.expect("create");
        assert!(state.backup.pending_restore().await.is_none());

        backups_stage_restore_body(&state, &info.file_name)
            .await
            .expect("stage_restore should succeed");

        let pending = state
            .backup
            .pending_restore()
            .await
            .expect("should now be pending");
        assert!(pending.size_bytes > 0);

        let audit = state
            .audit
            .list(ListParams::default())
            .await
            .expect("audit list");
        let entry = audit
            .rows
            .iter()
            .find(|r| r.action == "restore_staged")
            .unwrap_or_else(|| panic!("expected a restore_staged entry, got {:?}", audit.rows));
        assert_eq!(entry.actor_username.as_deref(), Some("admin"));
        assert_eq!(entry.resource, "backups");
        assert_eq!(entry.origin, "tauri");
        assert_eq!(entry.result, "ok");

        backups_cancel_restore_body(&state)
            .await
            .expect("cancel_restore should succeed");
        assert!(state.backup.pending_restore().await.is_none());

        let audit_after_cancel = state.audit.list(ListParams::default()).await.unwrap();
        assert!(
            audit_after_cancel
                .rows
                .iter()
                .any(|r| r.action == "restore_cancelled"),
            "expected a restore_cancelled entry, got {:?}",
            audit_after_cancel.rows
        );
    }

    // --- #332 Hub: denial audit resource ------------------------------------

    /// Review P2-D: a denied `hub_*` command must be recorded with
    /// `resource: "hub"`, the same tag the REST side's `hub_router`
    /// (`RoleGuard { resource: "hub", .. }`) uses - otherwise "every denial
    /// against hub" misses whichever transport disagrees. Every `hub_*`
    /// command goes through [`require_hub_admin`], so exercising that one
    /// function covers all seven call sites.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn denied_hub_command_is_recorded_under_the_hub_resource() {
        let state = app_state().await;
        let viewer = state
            .users
            .create_user("viewer", "password123", "閲覧者", Role::Viewer)
            .await
            .expect("create_user");
        *state.auth.lock().expect("auth mutex poisoned") = Some(viewer);

        let err = require_hub_admin(&state)
            .await
            .expect_err("a viewer must not pass the hub admin guard");
        assert!(matches!(err, BantoError::Forbidden));

        let audit = state
            .audit
            .list(ListParams::default())
            .await
            .expect("audit list");
        let entry = audit
            .rows
            .iter()
            .find(|r| r.action == "denied")
            .unwrap_or_else(|| panic!("expected a denied entry, got {:?}", audit.rows));
        assert_eq!(
            entry.resource, "hub",
            "denials must be tagged like REST's hub_router, not \"settings\""
        );
        assert_eq!(entry.actor_username.as_deref(), Some("viewer"));
        assert_eq!(entry.actor_role.as_deref(), Some("viewer"));
        assert_eq!(entry.origin, "tauri");
        assert_eq!(entry.result, "denied");
    }

    /// The other half of the intentional asymmetry (review P2-D): an `admin`
    /// passes the guard and NOTHING is recorded by it - the success entries
    /// the `hub_*` commands write themselves stay
    /// `action: "settings_change"` / `resource: "settings"` (unchanged, and
    /// already identical on the REST side).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn allowed_hub_command_records_no_denial() {
        let state = app_state().await;
        let admin = state
            .users
            .create_user("admin", "password123", "管理者", Role::Admin)
            .await
            .expect("create_user");
        *state.auth.lock().expect("auth mutex poisoned") = Some(admin);

        require_hub_admin(&state)
            .await
            .expect("an admin must pass the hub admin guard");

        let audit = state
            .audit
            .list(ListParams::default())
            .await
            .expect("audit list");
        assert!(
            audit.rows.iter().all(|r| r.action != "denied"),
            "a permitted call must not record a denial: {:?}",
            audit.rows
        );
    }

    // --- #383 段階2b / R1-C（C-2）: 収集の操作 -------------------------------

    /// 収集の床は **2 段**: 状態の**読み取りは `viewer` 以上**、
    /// **変更（開始・停止・再起動）は `editor` 以上**（`admin` ではない）。
    /// 2026-09-20 オーナー決定（#407 レビュー）と
    /// docs/recorder-requirements.md §3.6 + 2026-09-18 のオーナー決定が根拠
    /// （[`require_collect_reader`] / [`require_collect_editor`] の doc）。
    ///
    /// **これは `chronogazer_core::rest` 側の双子のテストと対**になっていて、
    /// 両方が同じことを主張する = **両経路の床が一致している**ことの固定。
    /// 床を分けても拒否は **`resource: "collect"`** のまま（#397 P2-D の
    /// 再発防止）。
    ///
    /// 3 つの変更コマンドは全部 [`require_collect_editor`]、読み取りは
    /// [`require_collect_reader`] を通るので、ここを押さえれば全部の
    /// 呼び出し口が決まる（`hub_*` と同じ構造）。
    ///
    /// 反証（回帰の検出）: `require_collect_reader` を
    /// `require_collect_editor` に戻すと viewer の `collect_status` が
    /// `Forbidden` になって落ちる。`require_collect_editor` の床を
    /// `COLLECT_READ_ROLE` に下げると viewer の `collect_start` が通って
    /// 落ちる。`resource` を `"settings"` 等に変えると綴りの `assert_eq!` が
    /// 落ちる。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn collect_reads_are_viewer_and_operations_are_editor_all_under_the_collect_resource() {
        let state = app_state().await;
        let viewer = state
            .users
            .create_user("viewer", "password123", "閲覧者", Role::Viewer)
            .await
            .expect("create_user");
        state
            .users
            .create_user("editor", "password123", "編集者", Role::Editor)
            .await
            .expect("create_user");

        *state.auth.lock().expect("auth mutex poisoned") = Some(viewer);
        // **viewer は状態を読める**（R0 §3.6 の「閲覧のみ」は「何も見えない」
        // ではない）。
        assert_eq!(
            collect_status_body(&state)
                .await
                .expect("a viewer must be able to read the collection state"),
            CollectorStateView::Stopped
        );
        // が、変更はできない。
        for outcome in [
            collect_start_body(&state).await,
            collect_stop_body(&state).await,
            collect_restart_body(&state).await,
        ] {
            assert!(matches!(
                outcome.expect_err("a viewer must not operate collection"),
                BantoError::Forbidden
            ));
        }

        let audit = state
            .audit
            .list(ListParams::default())
            .await
            .expect("audit list");
        let denials: Vec<_> = audit.rows.iter().filter(|r| r.action == "denied").collect();
        assert_eq!(
            denials.len(),
            3,
            "変更 3 件だけが拒否される（読み取りは通る）: {:?}",
            audit.rows
        );
        for entry in denials {
            assert_eq!(
                entry.resource, "collect",
                "床を分けても resource は分けない（REST 側と同じ綴り）"
            );
            assert_eq!(entry.actor_username.as_deref(), Some("viewer"));
            assert_eq!(entry.origin, "tauri");
            assert_eq!(entry.result, "denied");
        }

        // editor は変更も通る - `admin` へ引き上げていないこと。
        let editor = state
            .users
            .get_by_username("editor")
            .await
            .expect("get_by_username")
            .expect("editor exists");
        *state.auth.lock().expect("auth mutex poisoned") = Some(editor);
        require_collect_editor(&state)
            .await
            .expect("an editor must pass the collect operation guard");
    }

    /// **収集対象 0 件で `collect_start` を呼んでもエラーにならない**
    /// （`NoTargets` が返る）。あわせて、**成功時の監査**が
    /// 「操作者 + 結果の状態」で残ることと、`resource` が拒否と同じ
    /// `"collect"` であることを固定する。
    ///
    /// 反証（回帰の検出）: `collect_start_body` の
    /// `record_collect_operation` を消すと `expect` が落ちる。`resource` を
    /// `"settings"`（`hub_*` の成功側と同じ非対称）に変えても落ちる。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn collect_start_with_no_targets_is_ok_and_audited() {
        let state = app_state().await;
        let editor = state
            .users
            .create_user("editor", "password123", "編集者", Role::Editor)
            .await
            .expect("create_user");
        *state.auth.lock().expect("auth mutex poisoned") = Some(editor);

        let outcome = collect_start_body(&state)
            .await
            .expect("収集対象 0 件は「エラー」ではない");
        assert_eq!(
            outcome.status,
            chronogazer_core::collect::CollectorState::NoTargets
        );
        assert!(
            !outcome.pending,
            "上限に掛かっていないのに pending: {outcome:?}"
        );
        assert_eq!(
            collect_status_body(&state).await.expect("collect_status"),
            CollectorStateView::NoTargets
        );

        let audit = state
            .audit
            .list(ListParams::default())
            .await
            .expect("audit list");
        let entry = audit
            .rows
            .iter()
            .find(|r| r.action == "start")
            .unwrap_or_else(|| panic!("expected a start entry, got {:?}", audit.rows));
        assert_eq!(entry.resource, "collect");
        assert_eq!(entry.actor_username.as_deref(), Some("editor"));
        assert_eq!(entry.actor_role.as_deref(), Some("editor"));
        assert_eq!(entry.origin, "tauri");
        assert_eq!(entry.result, "ok");
        let detail: serde_json::Value =
            serde_json::from_str(entry.detail.as_deref().expect("detail が空"))
                .expect("detail は JSON");
        assert_eq!(detail["collectState"], "noTargets");
        assert_eq!(detail["pending"], false);
    }

    /// **#407 レビュー P2-2 の受入条件（Tauri 側）**: 起動に失敗した後、
    /// **viewer が `collect_status` を読んでも失敗理由が見えない**。
    ///
    /// `chronogazer_core::rest` の
    /// `a_viewer_reading_the_state_never_sees_why_the_start_failed` と
    /// **対になる双子のテスト**で、両方が同じことを主張する = **両経路が
    /// 同じ公開用の形を通している**ことの固定（床の定数・`resource` と
    /// 同じ作法）。
    ///
    /// 検証用の目印をタグのアドレスに埋めて `build_config` を失敗させる
    /// （`banto-tags` はアドレスの書式を検証しない）。`CollectError::Config`
    /// の文言はアドレスをそのまま含むので、**内部の `CollectorState` を
    /// 返していれば目印が必ず出てくる**。
    ///
    /// あわせて、**理由がどこで利用者に伝わるか**も押さえる - 起動を実行した
    /// editor には [`collect_start`] の**戻り値のエラー**として理由が届く
    /// （状態の読み取りから落とした分の埋め合わせが本当に存在すること）。
    ///
    /// 反証（回帰の検出）: `collect_status_body` を
    /// `Ok(state.collect.state())`（内部の型）に戻すと、`reason` キーと
    /// 目印の両方が JSON に出て 2 つの `assert!` が落ちる。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_viewer_reading_the_state_never_sees_why_the_start_failed() {
        const MARKER: &str = "CHRONOGAZER-LEAK-CANARY";
        let state = app_state().await;

        // 収集対象を 1 件だけ作る（アドレスが解釈できない = 起動が失敗する）。
        let conn = state
            .plc_connections
            .create(
                PlcConnectionPayload {
                    name: "plc1".to_string(),
                    protocol: "modbus-tcp".to_string(),
                    host: "192.168.11.200".to_string(),
                    port: 502,
                    unit_id: 1,
                    enabled: true,
                    word_order: String::new(),
                }
                .into(),
            )
            .await
            .expect("create plc connection");
        let group = state
            .collection_groups
            .create(
                CollectionGroupPayload {
                    name: "group1".to_string(),
                    plc_connection_id: conn.id,
                    period_ms: 1000,
                    enabled: true,
                }
                .into(),
            )
            .await
            .expect("create collection group");
        state
            .tags
            .create(
                TagPayload {
                    name: "tag1".to_string(),
                    collection_group_id: group.id,
                    address: MARKER.to_string(),
                    data_type: "i16".to_string(),
                    raw_lo: None,
                    raw_hi: None,
                    eng_lo: None,
                    eng_hi: None,
                    unit: None,
                    decimals: 0,
                    enabled: true,
                }
                .into(),
            )
            .await
            .expect("create tag");

        let editor = state
            .users
            .create_user("editor", "password123", "編集者", Role::Editor)
            .await
            .expect("create_user");
        let viewer = state
            .users
            .create_user("viewer", "password123", "閲覧者", Role::Viewer)
            .await
            .expect("create_user");

        // 起動した本人（editor）には、エラーとして理由が届く。
        *state.auth.lock().expect("auth mutex poisoned") = Some(editor);
        let err = collect_start_body(&state)
            .await
            .expect_err("前提が崩れている: 起動が失敗していない");
        assert!(
            err.to_string().contains(MARKER),
            "起動を実行した本人にも理由が伝わっていない（埋め合わせが無い）: {err}"
        );

        // viewer が状態を読む: 状態そのものは返るが、理由は返らない。
        *state.auth.lock().expect("auth mutex poisoned") = Some(viewer);
        let view = collect_status_body(&state)
            .await
            .expect("viewer は状態を読めること");
        assert_eq!(view, CollectorStateView::StartFailed);
        let json = serde_json::to_value(&view).expect("serialize");
        assert_eq!(json["state"], "startFailed");
        assert!(
            json.get("reason").is_none(),
            "`reason` フィールドがワイヤに出ている: {json}"
        );
        assert!(
            !json.to_string().contains(MARKER),
            "起動失敗の理由が viewer に漏れている: {json}"
        );
    }

    // --- #383 段階2b / R1-C（C-3a）: 収集の読み出し ---------------------------

    /// C-3a の 3 本（現在値・接続状態・イベント一覧）も**状態の読み取りと
    /// 同じ床**（`viewer` 以上 - [`require_collect_reader`]）で、**セッションが
    /// 無ければ拒否**。`chronogazer_core::rest` の
    /// `collect_readouts_are_viewer_readable_and_never_collapse_not_running_into_zero`
    /// と**対になる双子のテスト**で、両方が同じことを主張する = 両経路の床が
    /// 一致している。
    ///
    /// あわせて、**収集が走っていないときの 3 本の答え**も固定する:
    /// 現在値と接続状態は `notRunning`（**空のオブジェクトではない**）、
    /// イベント一覧は**それでも読める** `ready` で 0 件。
    ///
    /// **行の中身（新しい順・`totalCount`・`detail` を返さないこと）は
    /// `chronogazer_core` 側のテストが固定している** - 両経路とも
    /// `CollectorService::events` という**同じ 1 つのメソッド**を通るので、
    /// 経路によって形が割れようがない（`state_view` と同じ「変換は 1 箇所」の
    /// 作法）。ここで同じことを繰り返さないのは、このクレートが
    /// **依存を増やさない**invariant を持っていて、`collect_events` へ直接
    /// 行を入れる（= `sqlx` を名指しする）ことができないため。
    ///
    /// 反証（回帰の検出）: `collect_values_body` などの
    /// `require_collect_reader` を `require_collect_editor` に変えると viewer の
    /// 呼び出しが `Forbidden` になって落ちる。ガードを外すと未認証の
    /// `expect_err` が落ちる。走っていないときに空の `Ready` を返す実装に
    /// すると `notRunning` の `assert_eq!` が落ちる。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn collect_readouts_are_viewer_readable_and_never_collapse_not_running_into_zero() {
        let state = app_state().await;

        // セッションが無い = 未認証。3 本とも拒否される。
        collect_values_body(&state)
            .await
            .expect_err("未認証で現在値が読めている");
        collect_connections_body(&state)
            .await
            .expect_err("未認証で接続状態が読めている");
        collect_events_list_body(&state, None, None)
            .await
            .expect_err("未認証でイベント一覧が読めている");

        let viewer = state
            .users
            .create_user("viewer", "password123", "閲覧者", Role::Viewer)
            .await
            .expect("create_user");
        *state.auth.lock().expect("auth mutex poisoned") = Some(viewer);

        // viewer は 3 本とも読める。走っていないときの答えは
        // 「走っていない」であって「0 件」ではない。
        let values = collect_values_body(&state)
            .await
            .expect("viewer は現在値を読めること");
        assert_eq!(
            values,
            Readout::NotRunning,
            "「走っていない」を「0 件」に潰している: {values:?}"
        );
        let connections = collect_connections_body(&state)
            .await
            .expect("viewer は接続状態を読めること");
        assert_eq!(connections, Readout::NotRunning, "{connections:?}");

        // イベント一覧は走っていなくても読める（0 件という事実が返る）。
        let events = collect_events_list_body(&state, None, None)
            .await
            .expect("viewer はイベント一覧を読めること");
        assert_eq!(
            events.as_str(),
            "ready",
            "過去の記録の一覧を「走っていない」で隠している: {events:?}"
        );
        let page = events.data().expect("ready");
        assert!(page.rows.is_empty());
        assert_eq!(page.total_count, 0);
    }
}
