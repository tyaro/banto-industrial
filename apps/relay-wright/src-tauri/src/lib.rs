//! RelayWright — Tauri entry point.
//!
//! Thin `tauri::command` adapters only (spec §10): all real logic lives in
//! `relay-wright-core` (`apps/relay-wright/core`) and `banto-server`
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
use qrcode::render::svg;
use qrcode::QrCode;
use relay_wright_core::assets::FrontendAssets;
use relay_wright_core::audit::{AuditEntry, AuditLogEntry, AuditLogService};
use relay_wright_core::backup::{BackupInfo, BackupService, PendingRestoreInfo};
use relay_wright_core::db::init_db;
use relay_wright_core::engine::{
    Engine, EngineConfig, EngineControl, EngineStatus, SharedEngineControl,
};
use relay_wright_core::events::event_channel;
use relay_wright_core::rest::{api_router, audited_credential_verifier};
use relay_wright_core::settings::{AuditSettings, AuthSettings, ServerSettings, SettingsService};
use relay_wright_core::users::{Role, UserIdentity, UserSummary, UsersService};
use relay_wright_core::write_audit_query::WriteAuditLogService;
use relay_wright_core::write_rules::{WriteRuleDetail, WriteRuleInput, WriteRuleService};
use relay_wright_core::write_targets::{WriteTarget, WriteTargetInput, WriteTargetService};
use serde::Serialize;
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
    /// Backup/restore (spec M17): `VACUUM INTO` snapshots into `backups/`
    /// next to the DB file, plus the restore staging flow. Shares the same
    /// pool as `users`/`settings`/`audit` - only its `db_path` is
    /// unique to this service (needed to resolve `backups/` and
    /// `restore-pending.sqlite3`'s location, see `crate::backup`'s doc
    /// comment).
    backup: BackupService,
    /// Write-target registry (plan W2): the PLC devices this app may write
    /// to. Same shared pool; viewer-read / editor-write, audited on both
    /// paths (this Tauri path and `crate::rest`'s REST path).
    write_targets: WriteTargetService,
    /// Write-rule registry + its inline conditions (plan W2), with the
    /// write-loop cycle-detection guard on save. Same shared pool.
    write_rules: WriteRuleService,
    /// The one shared SQLite pool (same on-disk DB as every service above).
    /// Held directly - not just via the service handles - so `engine_reload`
    /// can hand it to `Engine::start_from_db` to rebuild the engine from the
    /// current connections/rules (plan W3-B2). Named via `relay_wright_core`'s
    /// [`relay_wright_core::db::DbPool`] alias so this crate keeps its
    /// no-new-dependencies invariant (no direct `sqlx` dependency).
    pool: relay_wright_core::db::DbPool,
    /// The running auto-write engine (plan W3-B2): owns the poller/writer
    /// tasks and the PLC broker. `Option` so `engine_reload` (and app exit)
    /// can `.take()` it to call `Engine::shutdown`, which consumes it; `None`
    /// only in the (in practice unreachable) case that the engine failed to
    /// start at launch. Behind an async mutex so a reload holds the slot
    /// exclusively across its teardown+rebuild await points.
    engine: AsyncMutex<Option<Engine>>,
    /// The engine's arm/disarm/dry-run control handle (plan W3-B2), in a
    /// SHARED swappable slot ([`SharedEngineControl`]) whose Arc is also cloned
    /// into the embedded REST server's router state - so both wiring paths act
    /// on the SAME control and `engine_reload` (which swaps this slot) is seen
    /// by both (invariant §1 dual-path symmetry). The four engine commands
    /// clone the current control out from under the lock and call it;
    /// arm/disarm/set_dry_run already persist + write a `write_audit_log` row
    /// with the passed actor, so this layer adds only authorization + actor
    /// resolution - never a second audit.
    engine_control: SharedEngineControl,
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
    /// `relay_wright_core::users::Role::as_str`) - kept a plain `String`
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
    // Convention shared with `relay_wright_core::rest` and `relay-wright-serve`:
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
/// `relay_wright_core::rest`'s `require_auth` then `require_role_at_least`
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
/// comment in `relay_wright_core::settings`).
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
// Same shape as `relay_wright_core::rest::api_router` (which this wraps)
// and for the same reason: distinct service handles, no natural struct to
// bundle them into for a single call site.
#[allow(clippy::too_many_arguments)]
async fn start_embedded_server(
    users: UsersService,
    settings: SettingsService,
    audit: AuditLogService,
    backup: BackupService,
    write_targets: WriteTargetService,
    write_rules: WriteRuleService,
    write_audit_log: WriteAuditLogService,
    engine_control: SharedEngineControl,
    auth: AuthState,
    events: broadcast::Sender<ServerEvent>,
    config: ServerConfig,
) -> Result<RunningServer, BantoError> {
    // `allow_setup: false` - the Tauri app's first-run setup goes through
    // the `auth_setup` command above (`invoke()`, no network involved), not
    // this REST endpoint. Only `relay-wright-serve` (this repo's Tauri-free dev
    // vehicle) opts into `POST /api/auth/setup` via `BANTO_ALLOW_SETUP=1`.
    // `engine_control` is the SAME shared slot the Tauri engine commands use
    // (invariant §1 dual-path symmetry) - see `AppState::engine_control`.
    let router = api_router(
        users,
        settings,
        audit,
        backup,
        write_targets,
        write_rules,
        write_audit_log,
        engine_control,
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
                state.write_targets.clone(),
                state.write_rules.clone(),
                state.engine_control.clone(),
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
/// `relay_wright_core::users`). No `createdAt` (unlike [`UserSummary`],
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

/// `admin`-only (spec M14): the audit-log viewer's filtered/sorted/
/// paginated read. Also opportunistically prunes first - same reasoning as
/// `relay_wright_core::rest::audit_log_list` (see that function's doc
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

// --- W2: write registry/rule CRUD -------------------------------------------
//
// The Tauri half of the dual-path (invariant §1 両経路対称): every mutation
// below applies the SAME authorization floor (editor-write / viewer-read) and
// the SAME audit entry (`origin: "tauri"`) as `crate::rest`'s REST handlers.
// Reads use `require_role(Viewer, ..)` (any authenticated role); writes use
// `require_role(Editor, ..)` and `record` a create/update/delete entry once
// the underlying service call has already succeeded, exactly as the users
// commands do. Each write body is split into a `*_body(&AppState, ..)` helper
// so the audit behavior is testable with a plain `&AppState` in this crate's
// own `cargo test` (see `change_own_password`'s precedent).

/// `viewer`+ (spec M10): list write targets. Reads are never audited.
#[tauri::command]
async fn write_targets_list(state: State<'_, AppState>) -> Result<Vec<WriteTarget>, BantoError> {
    require_role(&state, Role::Viewer, "write_targets").await?;
    Ok(state.write_targets.list(ListParams::default()).await?.rows)
}

/// `viewer`+ (spec M10): fetch one write target.
#[tauri::command]
async fn write_targets_get(state: State<'_, AppState>, id: i64) -> Result<WriteTarget, BantoError> {
    require_role(&state, Role::Viewer, "write_targets").await?;
    state.write_targets.get(id).await
}

async fn write_targets_create_body(
    state: &AppState,
    input: WriteTargetInput,
) -> Result<WriteTarget, BantoError> {
    let actor = require_role(state, Role::Editor, "write_targets").await?;
    let created = state.write_targets.create(input).await?;
    state
        .audit
        .record(AuditEntry {
            actor_username: Some(&actor.username),
            actor_role: Some(actor.role.as_str()),
            action: "create",
            resource: "write_targets",
            entity_id: Some(&created.id.to_string()),
            detail: Some(serde_json::json!({ "name": created.name })),
            origin: "tauri",
            result: "ok",
        })
        .await;
    Ok(created)
}

/// `editor`+ (spec M10): create a write target.
#[tauri::command]
async fn write_targets_create(
    state: State<'_, AppState>,
    input: WriteTargetInput,
) -> Result<WriteTarget, BantoError> {
    write_targets_create_body(&state, input).await
}

async fn write_targets_update_body(
    state: &AppState,
    id: i64,
    input: WriteTargetInput,
) -> Result<WriteTarget, BantoError> {
    let actor = require_role(state, Role::Editor, "write_targets").await?;
    let updated = state.write_targets.update(id, input).await?;
    state
        .audit
        .record(AuditEntry {
            actor_username: Some(&actor.username),
            actor_role: Some(actor.role.as_str()),
            action: "update",
            resource: "write_targets",
            entity_id: Some(&id.to_string()),
            detail: Some(serde_json::json!({ "name": updated.name })),
            origin: "tauri",
            result: "ok",
        })
        .await;
    Ok(updated)
}

/// `editor`+ (spec M10): update a write target.
#[tauri::command]
async fn write_targets_update(
    state: State<'_, AppState>,
    id: i64,
    input: WriteTargetInput,
) -> Result<WriteTarget, BantoError> {
    write_targets_update_body(&state, id, input).await
}

async fn write_targets_delete_body(state: &AppState, id: i64) -> Result<(), BantoError> {
    let actor = require_role(state, Role::Editor, "write_targets").await?;
    state.write_targets.delete(id).await?;
    state
        .audit
        .record(AuditEntry {
            actor_username: Some(&actor.username),
            actor_role: Some(actor.role.as_str()),
            action: "delete",
            resource: "write_targets",
            entity_id: Some(&id.to_string()),
            detail: None,
            origin: "tauri",
            result: "ok",
        })
        .await;
    Ok(())
}

/// `editor`+ (spec M10): delete a write target. Refuses when a rule still
/// targets it (`WriteTargetService::delete`'s guard).
#[tauri::command]
async fn write_targets_delete(state: State<'_, AppState>, id: i64) -> Result<(), BantoError> {
    write_targets_delete_body(&state, id).await
}

/// `viewer`+ (spec M10): list write rules (each with its conditions).
#[tauri::command]
async fn write_rules_list(state: State<'_, AppState>) -> Result<Vec<WriteRuleDetail>, BantoError> {
    require_role(&state, Role::Viewer, "write_rules").await?;
    Ok(state.write_rules.list(ListParams::default()).await?.rows)
}

/// `viewer`+ (spec M10): fetch one write rule with its conditions.
#[tauri::command]
async fn write_rules_get(
    state: State<'_, AppState>,
    id: i64,
) -> Result<WriteRuleDetail, BantoError> {
    require_role(&state, Role::Viewer, "write_rules").await?;
    state.write_rules.get(id).await
}

async fn write_rules_create_body(
    state: &AppState,
    input: WriteRuleInput,
) -> Result<WriteRuleDetail, BantoError> {
    let actor = require_role(state, Role::Editor, "write_rules").await?;
    let created = state.write_rules.create(input).await?;
    state
        .audit
        .record(AuditEntry {
            actor_username: Some(&actor.username),
            actor_role: Some(actor.role.as_str()),
            action: "create",
            resource: "write_rules",
            entity_id: Some(&created.rule.id.to_string()),
            detail: Some(
                serde_json::json!({ "name": created.rule.name, "enabled": created.rule.enabled }),
            ),
            origin: "tauri",
            result: "ok",
        })
        .await;
    Ok(created)
}

/// `editor`+ (spec M10): create a write rule (with the write-loop cycle guard).
#[tauri::command]
async fn write_rules_create(
    state: State<'_, AppState>,
    input: WriteRuleInput,
) -> Result<WriteRuleDetail, BantoError> {
    write_rules_create_body(&state, input).await
}

async fn write_rules_update_body(
    state: &AppState,
    id: i64,
    input: WriteRuleInput,
) -> Result<WriteRuleDetail, BantoError> {
    let actor = require_role(state, Role::Editor, "write_rules").await?;
    let updated = state.write_rules.update(id, input).await?;
    state
        .audit
        .record(AuditEntry {
            actor_username: Some(&actor.username),
            actor_role: Some(actor.role.as_str()),
            action: "update",
            resource: "write_rules",
            entity_id: Some(&id.to_string()),
            detail: Some(
                serde_json::json!({ "name": updated.rule.name, "enabled": updated.rule.enabled }),
            ),
            origin: "tauri",
            result: "ok",
        })
        .await;
    Ok(updated)
}

/// `editor`+ (spec M10): update a write rule (with the write-loop cycle guard).
#[tauri::command]
async fn write_rules_update(
    state: State<'_, AppState>,
    id: i64,
    input: WriteRuleInput,
) -> Result<WriteRuleDetail, BantoError> {
    write_rules_update_body(&state, id, input).await
}

async fn write_rules_delete_body(state: &AppState, id: i64) -> Result<(), BantoError> {
    let actor = require_role(state, Role::Editor, "write_rules").await?;
    state.write_rules.delete(id).await?;
    state
        .audit
        .record(AuditEntry {
            actor_username: Some(&actor.username),
            actor_role: Some(actor.role.as_str()),
            action: "delete",
            resource: "write_rules",
            entity_id: Some(&id.to_string()),
            detail: None,
            origin: "tauri",
            result: "ok",
        })
        .await;
    Ok(())
}

/// `editor`+ (spec M10): delete a write rule (its conditions cascade).
#[tauri::command]
async fn write_rules_delete(state: State<'_, AppState>, id: i64) -> Result<(), BantoError> {
    write_rules_delete_body(&state, id).await
}

// --- W3-B2: auto-write engine control ---------------------------------------
//
// The Tauri half of the engine's dual-path (invariant §1 両経路対称): the same
// authorization floors and the same audit as `crate::rest`'s `/api/engine/*`
// routes. arm/disarm require `admin` (arming enables LIVE physical writes to
// industrial equipment - the strongest gate, per plan W3-B2's safety notes);
// dry-run toggle requires `editor`; `engine_status` is a `viewer`+ read.
//
// The arm/disarm/dry-run AUDIT is written INSIDE `EngineControl` (to the
// `write_audit_log` table, with the actor resolved below), so this layer must
// NOT `state.audit.record` a second entry - it adds ONLY the RBAC gate and the
// actor username. A role DENIAL is still recorded by `require_role` to the
// M14 audit log, exactly as every other command here.

/// Clone the current [`EngineControl`] out from under the lock so the actual
/// arm/disarm/dry-run/status call does not hold the `AppState` engine lock
/// across its own await points (and so it cannot deadlock a concurrent
/// `engine_reload`, which is the only path that swaps the handle). `None` -
/// the engine failed to start at launch - is surfaced as a plain error rather
/// than panicking.
async fn current_engine_control(state: &AppState) -> Result<EngineControl, BantoError> {
    state
        .engine_control
        .lock()
        .await
        .clone()
        .ok_or_else(|| BantoError::Other("自動書き込みエンジンが起動していません".to_string()))
}

/// Body of [`engine_arm`], split out so the role gate + state flip is testable
/// with a plain `&AppState` (spec M14 split-function pattern).
async fn engine_arm_body(state: &AppState) -> Result<(), BantoError> {
    let actor = require_role(state, Role::Admin, "engine").await?;
    current_engine_control(state)
        .await?
        .arm(Some(&actor.username))
        .await
}

/// `admin`-only (plan W3-B2): arm the engine (enable live physical writes).
#[tauri::command]
async fn engine_arm(state: State<'_, AppState>) -> Result<(), BantoError> {
    engine_arm_body(&state).await
}

/// Body of [`engine_disarm`] (see [`engine_arm_body`]).
async fn engine_disarm_body(state: &AppState) -> Result<(), BantoError> {
    let actor = require_role(state, Role::Admin, "engine").await?;
    current_engine_control(state)
        .await?
        .disarm(Some(&actor.username))
        .await
}

/// `admin`-only (plan W3-B2): disarm the engine (suppress all physical
/// writes). Disarm is safety-positive and could defensibly be `editor`, but it
/// is kept at `admin` to match arm and keep the two paths' RBAC table
/// symmetric (invariant §1) - one role governs the arm/disarm pair.
#[tauri::command]
async fn engine_disarm(state: State<'_, AppState>) -> Result<(), BantoError> {
    engine_disarm_body(&state).await
}

/// Body of [`engine_set_dry_run`] (see [`engine_arm_body`]).
async fn engine_set_dry_run_body(state: &AppState, on: bool) -> Result<(), BantoError> {
    let actor = require_role(state, Role::Editor, "engine").await?;
    current_engine_control(state)
        .await?
        .set_dry_run(on, Some(&actor.username))
        .await
}

/// `editor`+ (plan W3-B2): turn dry-run on/off (evaluate + audit would-be
/// writes, but never touch the PLC). Lower floor than arm/disarm because
/// dry-run can only make the engine SAFER, never enable a physical write.
#[tauri::command]
async fn engine_set_dry_run(state: State<'_, AppState>, on: bool) -> Result<(), BantoError> {
    engine_set_dry_run_body(&state, on).await
}

/// `viewer`+ (plan W3-B2): read the engine's arm/dry-run snapshot. Read-only,
/// so not audited (same convention as every other read command here).
#[tauri::command]
async fn engine_status(state: State<'_, AppState>) -> Result<EngineStatus, BantoError> {
    require_role(&state, Role::Viewer, "engine").await?;
    Ok(current_engine_control(&state).await?.status())
}

/// Body of [`engine_reload`] (see [`engine_arm_body`]).
async fn engine_reload_body(state: &AppState) -> Result<EngineStatus, BantoError> {
    let actor = require_role(state, Role::Admin, "engine").await?;
    // Serialize the whole rebuild on the engine slot: hold it for the entire
    // disarm -> shutdown -> rebuild -> store sequence so two concurrent
    // reloads (or a reload racing app-exit shutdown) cannot interleave.
    let mut engine_slot = state.engine.lock().await;
    // Disarm the outgoing engine first (safety-positive; audited by the
    // control itself). Best-effort - a disarm error must not block the
    // teardown that follows.
    if let Some(control) = state.engine_control.lock().await.clone() {
        let _ = control.disarm(Some(&actor.username)).await;
    }
    if let Some(old) = engine_slot.take() {
        old.shutdown().await;
    }
    // Rebuild from the CURRENT DB (enabled connections + rules). The rebuilt
    // engine always starts DISARMED - never auto-re-arm (invariant §1).
    let (engine, control) =
        Engine::start_from_db(state.pool.clone(), EngineConfig::default()).await?;
    let status = control.status();
    *state.engine_control.lock().await = Some(control);
    *engine_slot = Some(engine);
    Ok(status)
}

/// `admin`-only (plan W3-B2): rebuild the engine from the current DB so rule
/// edits made via the W2 CRUD take effect (rules are compiled once at
/// `Engine::start`). Disarms and tears down the running engine, then starts a
/// fresh one from the enabled connections/rules - the rebuilt engine is always
/// **disarmed**. Returns the new (disarmed) status.
#[tauri::command]
async fn engine_reload(state: State<'_, AppState>) -> Result<EngineStatus, BantoError> {
    engine_reload_body(&state).await
}

pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let data_dir = app.path().app_data_dir().expect("resolve app data dir");
            std::fs::create_dir_all(&data_dir).expect("create app data dir");
            let db_path = data_dir.join("relay-wright.sqlite3");

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
            let write_targets = WriteTargetService::new(pool.clone());
            let write_rules = WriteRuleService::new(pool.clone());
            // Cloned (not moved) into `audit` so the pool stays available for
            // the W3-B2 auto-write engine start below and `AppState.pool`
            // (which `engine_reload` needs to rebuild the engine from the DB).
            let audit = AuditLogService::new(pool.clone());
            // Records `login`/`login_failed` audit entries (spec M14) from
            // inside the verifier itself - see
            // `relay_wright_core::rest::audited_credential_verifier`'s doc
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

            // W3-B2: build and START the auto-write engine from the current DB
            // (enabled SLMP connections + enabled rules), BEFORE the LAN server
            // auto-start below - the embedded server shares this engine's
            // control slot (invariant §1 dual-path symmetry). It starts
            // DISARMED (invariant §1 - startup never auto-arms) and returns
            // promptly even if a PLC is unreachable (the broker reconnects/backs
            // off in its own tasks). Zero connections/rules -> a clean idle
            // engine. A start failure is NON-FATAL: the desktop app still runs
            // (with no engine until the next restart or `engine_reload`); the
            // engine commands/routes then surface a clear "not started" error.
            let (initial_engine, initial_engine_control) =
                match tauri::async_runtime::block_on(Engine::start_from_db(
                    pool.clone(),
                    EngineConfig::default(),
                )) {
                    Ok((engine, control)) => (Some(engine), Some(control)),
                    Err(err) => {
                        eprintln!(
                            "banto: 自動書き込みエンジンの起動に失敗しました（アプリは続行します）: {err}"
                        );
                        (None, None)
                    }
                };
            // The shared, swappable control slot. Its Arc is cloned into the
            // embedded REST server (below and in `server_apply`) so both wiring
            // paths act on the SAME engine, and `engine_reload` (which swaps the
            // inner control) is seen by both automatically.
            let engine_control: SharedEngineControl =
                std::sync::Arc::new(AsyncMutex::new(initial_engine_control));

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
                    write_targets.clone(),
                    write_rules.clone(),
                    engine_control.clone(),
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
                write_targets,
                write_rules,
                pool,
                engine: AsyncMutex::new(initial_engine),
                engine_control,
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
            write_targets_list,
            write_targets_get,
            write_targets_create,
            write_targets_update,
            write_targets_delete,
            write_rules_list,
            write_rules_get,
            write_rules_create,
            write_rules_update,
            write_rules_delete,
            engine_arm,
            engine_disarm,
            engine_set_dry_run,
            engine_status,
            engine_reload,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            // W3-B2: stop the auto-write engine cleanly when the app's event
            // loop is exiting - flips the broker/poller/writer shutdown signal
            // and awaits the tasks (the W3-A watch-signal design guarantees
            // this returns promptly, never hangs). Mirrors relying on the
            // process teardown for the LAN server, but done explicitly here so
            // the engine's live PLC sockets close cleanly. Best-effort: if the
            // engine never started (`None`), there is nothing to stop.
            if let tauri::RunEvent::Exit = event {
                if let Some(state) = app_handle.try_state::<AppState>() {
                    let engine = tauri::async_runtime::block_on(state.engine.lock()).take();
                    if let Some(engine) = engine {
                        tauri::async_runtime::block_on(engine.shutdown());
                    }
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A minimal [`AppState`] over an in-memory DB, no running server, and a
    /// dummy REST verifier - just enough state to exercise command bodies
    /// (like [`change_own_password`]) that only touch the service handles.
    async fn app_state() -> AppState {
        let pool = relay_wright_core::db::init_db_memory()
            .await
            .expect("init_db_memory");
        let events = event_channel();
        AppState {
            auth: Mutex::new(None),
            users: UsersService::new(pool.clone()),
            settings: SettingsService::new(pool.clone()),
            events,
            rest_auth: AuthState::new(|_u: String, _p: String| {
                Box::pin(async { None::<banto_server::Identity> })
            }),
            server: AsyncMutex::new(None),
            audit: AuditLogService::new(pool.clone()),
            backup: BackupService::new(
                PathBuf::from("unused-in-tests").join("relay-wright.sqlite3"),
                pool.clone(),
            ),
            write_targets: WriteTargetService::new(pool.clone()),
            write_rules: WriteRuleService::new(pool.clone()),
            pool,
            // Engine-less: the command-body tests that use this helper only
            // exercise the RBAC gate, which rejects before ever touching the
            // control (see the engine denial tests).
            engine: AsyncMutex::new(None),
            engine_control: std::sync::Arc::new(AsyncMutex::new(None)),
        }
    }

    /// Like [`app_state`], but backed by a REAL on-disk db in a fresh temp
    /// directory rather than `:memory:` - required for the M17 backup tests
    /// below, since `BackupService::create`'s `VACUUM INTO` silently writes
    /// nothing when its source pool is `:memory:` (see
    /// `relay_wright_core::backup`'s test module doc comment for the
    /// empirically-verified reason). The returned `TempDir` guard must be
    /// kept alive by the caller for as long as `AppState` is still in use.
    async fn app_state_with_tempdir() -> (AppState, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("relay-wright.sqlite3");
        let pool = relay_wright_core::db::init_db(&db_path)
            .await
            .expect("init_db");
        let events = event_channel();
        let state = AppState {
            auth: Mutex::new(None),
            users: UsersService::new(pool.clone()),
            settings: SettingsService::new(pool.clone()),
            events,
            rest_auth: AuthState::new(|_u: String, _p: String| {
                Box::pin(async { None::<banto_server::Identity> })
            }),
            server: AsyncMutex::new(None),
            audit: AuditLogService::new(pool.clone()),
            backup: BackupService::new(db_path, pool.clone()),
            write_targets: WriteTargetService::new(pool.clone()),
            write_rules: WriteRuleService::new(pool.clone()),
            pool,
            engine: AsyncMutex::new(None),
            engine_control: std::sync::Arc::new(AsyncMutex::new(None)),
        };
        (state, dir)
    }

    /// Spec M14: the Tauri-side self-service password change must be
    /// recorded as `password_change` (actor = entity = the caller), and the
    /// entry's `detail` must never carry the password.
    #[tokio::test]
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
    #[tokio::test]
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
    #[tokio::test]
    async fn backups_create_records_a_backup_audit_entry() {
        let (state, _dir) = app_state_with_tempdir().await;
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
    #[tokio::test]
    async fn viewer_cannot_create_backups() {
        let (state, _dir) = app_state_with_tempdir().await;
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
    #[tokio::test]
    async fn stage_restore_then_pending_reports_it() {
        let (state, _dir) = app_state_with_tempdir().await;
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

    // --- W2: write registry dual-path symmetry (Tauri half) -----------------

    /// The Tauri half of the W2 create+audit symmetry (its REST twin is
    /// `relay_wright_core::rest`'s `rest_editor_can_create_write_target_and_it_is_audited`):
    /// an `editor` session can create a write target via the command body,
    /// and it is recorded to the SAME audit log with `origin: "tauri"`.
    /// Like [`app_state`], but returns the shared pool too, so a test can
    /// seed cross-lineage rows (e.g. a `plc_connections` row a write target
    /// must reference) that no command exposes a way to create.
    async fn app_state_with_pool() -> (AppState, sqlx::SqlitePool) {
        let pool = relay_wright_core::db::init_db_memory()
            .await
            .expect("init_db_memory");
        let events = event_channel();
        let state = AppState {
            auth: Mutex::new(None),
            users: UsersService::new(pool.clone()),
            settings: SettingsService::new(pool.clone()),
            events,
            rest_auth: AuthState::new(|_u: String, _p: String| {
                Box::pin(async { None::<banto_server::Identity> })
            }),
            server: AsyncMutex::new(None),
            audit: AuditLogService::new(pool.clone()),
            backup: BackupService::new(
                PathBuf::from("unused-in-tests").join("relay-wright.sqlite3"),
                pool.clone(),
            ),
            write_targets: WriteTargetService::new(pool.clone()),
            write_rules: WriteRuleService::new(pool.clone()),
            pool: pool.clone(),
            engine: AsyncMutex::new(None),
            engine_control: std::sync::Arc::new(AsyncMutex::new(None)),
        };
        (state, pool)
    }

    /// Like [`app_state`], but with a REAL (idle) engine started over the
    /// in-memory pool - zero connections/rules, so the poller/writer tasks run
    /// but do nothing. Enough to exercise the permitted arm/dry-run flip and
    /// the `write_audit_log` row `EngineControl` writes. Returns the shared
    /// pool too so a test can query that audit table directly.
    async fn app_state_with_engine() -> (AppState, sqlx::SqlitePool) {
        let pool = relay_wright_core::db::init_db_memory()
            .await
            .expect("init_db_memory");
        let events = event_channel();
        let (engine, control) = Engine::start(pool.clone(), Vec::new(), EngineConfig::default())
            .await
            .expect("idle engine start");
        let state = AppState {
            auth: Mutex::new(None),
            users: UsersService::new(pool.clone()),
            settings: SettingsService::new(pool.clone()),
            events,
            rest_auth: AuthState::new(|_u: String, _p: String| {
                Box::pin(async { None::<banto_server::Identity> })
            }),
            server: AsyncMutex::new(None),
            audit: AuditLogService::new(pool.clone()),
            backup: BackupService::new(
                PathBuf::from("unused-in-tests").join("relay-wright.sqlite3"),
                pool.clone(),
            ),
            write_targets: WriteTargetService::new(pool.clone()),
            write_rules: WriteRuleService::new(pool.clone()),
            pool: pool.clone(),
            engine: AsyncMutex::new(Some(engine)),
            engine_control: std::sync::Arc::new(AsyncMutex::new(Some(control))),
        };
        (state, pool)
    }

    #[tokio::test]
    async fn write_targets_create_is_recorded_with_tauri_origin() {
        use banto_tags::{PlcConnectionInput, PlcConnectionService};

        let (state, pool) = app_state_with_pool().await;
        // A PLC connection for the target to reference (validated at the
        // service layer since it is a cross-lineage reference).
        let conn = PlcConnectionService::new(pool)
            .create(PlcConnectionInput {
                name: "PLC1".to_string(),
                protocol: "modbus-tcp".to_string(),
                host: "10.0.0.1".to_string(),
                port: 502,
                unit_id: 1,
                enabled: true,
            })
            .await
            .expect("seed plc connection");

        let editor = state
            .users
            .create_user("editor", "password123", "編集者", Role::Editor)
            .await
            .expect("create editor");
        *state.auth.lock().expect("auth mutex poisoned") = Some(editor);

        let created = write_targets_create_body(
            &state,
            WriteTargetInput {
                name: "WT1".to_string(),
                plc_connection_id: conn.id,
                address: "D100".to_string(),
                data_type: "i16".to_string(),
                raw_lo: None,
                raw_hi: None,
                eng_lo: None,
                eng_hi: None,
                unit: None,
                decimals: 0,
                enabled: true,
            },
        )
        .await
        .expect("editor create should succeed");
        assert_eq!(created.name, "WT1");

        let entries = state.audit.list(ListParams::default()).await.unwrap();
        let entry = entries
            .rows
            .iter()
            .find(|r| r.action == "create" && r.resource == "write_targets")
            .expect("expected a write_targets create audit entry");
        assert_eq!(entry.origin, "tauri");
        assert_eq!(entry.result, "ok");
        assert_eq!(entry.actor_username.as_deref(), Some("editor"));
    }

    /// A `viewer` session is denied (and the denial audited) when creating a
    /// write target - the Tauri twin of the REST `require_editor` denial.
    #[tokio::test]
    async fn write_targets_create_denies_viewer_and_audits_it() {
        let state = app_state().await;
        let viewer = state
            .users
            .create_user("viewer", "password123", "閲覧者", Role::Viewer)
            .await
            .expect("create viewer");
        *state.auth.lock().expect("auth mutex poisoned") = Some(viewer);

        let err = write_targets_create_body(
            &state,
            WriteTargetInput {
                name: "WT1".to_string(),
                plc_connection_id: 1,
                address: "D100".to_string(),
                data_type: "i16".to_string(),
                raw_lo: None,
                raw_hi: None,
                eng_lo: None,
                eng_hi: None,
                unit: None,
                decimals: 0,
                enabled: true,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, BantoError::Forbidden));

        let entries = state.audit.list(ListParams::default()).await.unwrap();
        assert!(
            entries
                .rows
                .iter()
                .any(|r| r.action == "denied" && r.resource == "write_targets"),
            "expected a denied entry, got {:?}",
            entries.rows
        );
    }

    // --- W3-B2: engine control dual-path (Tauri half) -----------------------

    async fn write_audit_count(pool: &sqlx::SqlitePool, action: &str, result: &str) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM write_audit_log WHERE action = ? AND result = ?")
            .bind(action)
            .bind(result)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// arm/disarm require `admin`: an `editor` (one rung below) is denied, the
    /// engine is NOT armed, and the denial is recorded to the M14 audit log
    /// with `resource: "engine"` - the Tauri twin of the REST `/api/engine/arm`
    /// admin gate.
    #[tokio::test]
    async fn engine_arm_denies_editor_and_audits_it() {
        let (state, pool) = app_state_with_engine().await;
        let editor = state
            .users
            .create_user("editor", "password123", "編集者", Role::Editor)
            .await
            .expect("create editor");
        *state.auth.lock().expect("auth mutex poisoned") = Some(editor);

        let err = engine_arm_body(&state).await.unwrap_err();
        assert!(matches!(err, BantoError::Forbidden));

        // Nothing armed, nothing written to the engine's own audit table.
        assert!(!current_engine_control(&state).await.unwrap().is_armed());
        assert_eq!(write_audit_count(&pool, "arm", "ok").await, 0);

        // The RBAC denial IS recorded to the M14 audit log.
        let entries = state.audit.list(ListParams::default()).await.unwrap();
        assert!(
            entries
                .rows
                .iter()
                .any(|r| r.action == "denied" && r.resource == "engine"),
            "expected a denied engine entry, got {:?}",
            entries.rows
        );
    }

    /// An `admin` can arm: the state flips to armed AND `EngineControl` writes
    /// exactly one `arm`/`ok` row to `write_audit_log` with the acting actor -
    /// and this layer does NOT double-audit (no second entry).
    #[tokio::test]
    async fn engine_admin_can_arm_and_it_flips_state_and_audits() {
        let (state, pool) = app_state_with_engine().await;
        let admin = state
            .users
            .create_user("admin", "password123", "管理者", Role::Admin)
            .await
            .expect("create admin");
        *state.auth.lock().expect("auth mutex poisoned") = Some(admin);

        engine_arm_body(&state)
            .await
            .expect("admin arm should succeed");

        assert!(current_engine_control(&state).await.unwrap().is_armed());
        assert_eq!(
            write_audit_count(&pool, "arm", "ok").await,
            1,
            "exactly one arm row (no double-audit from the command layer)"
        );

        // And `engine_status`'s snapshot reflects the flip.
        let status = current_engine_control(&state).await.unwrap().status();
        assert!(status.armed);
        assert!(!status.dry_run);
    }

    /// dry-run toggle requires only `editor` (safety-positive): an editor can
    /// enable it, the snapshot reflects it, and a `dry_run_toggle`/`ok` row is
    /// written by `EngineControl`.
    #[tokio::test]
    async fn engine_editor_can_toggle_dry_run() {
        let (state, pool) = app_state_with_engine().await;
        let editor = state
            .users
            .create_user("editor", "password123", "編集者", Role::Editor)
            .await
            .expect("create editor");
        *state.auth.lock().expect("auth mutex poisoned") = Some(editor);

        engine_set_dry_run_body(&state, true)
            .await
            .expect("editor dry-run toggle should succeed");

        assert!(current_engine_control(&state).await.unwrap().is_dry_run());
        assert_eq!(write_audit_count(&pool, "dry_run_toggle", "ok").await, 1);
    }

    /// `engine_status` is a `viewer`+ read: a viewer can read the (disarmed)
    /// snapshot, and it records nothing.
    #[tokio::test]
    async fn engine_status_permits_viewer_and_is_not_audited() {
        let (state, _pool) = app_state_with_engine().await;
        let viewer = state
            .users
            .create_user("viewer", "password123", "閲覧者", Role::Viewer)
            .await
            .expect("create viewer");
        *state.auth.lock().expect("auth mutex poisoned") = Some(viewer);

        let status = current_engine_control(&state).await.unwrap().status();
        require_role(&state, Role::Viewer, "engine")
            .await
            .expect("viewer may read status");
        assert!(!status.armed);

        let entries = state.audit.list(ListParams::default()).await.unwrap();
        assert!(
            entries.rows.iter().all(|r| r.resource != "engine"),
            "a status read must not record any engine audit entry, got {:?}",
            entries.rows
        );
    }
}
