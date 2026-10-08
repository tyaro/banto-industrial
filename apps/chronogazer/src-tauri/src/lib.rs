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

use banto_core::{BantoError, FieldError, ListParams};
use banto_server::{
    bind as bind_listener, lan_urls_for_bind, static_router, with_security_headers, AuthState,
    BoundServer, GrantKind, RunningServer, ServerConfig, ServerEvent,
};
use chronogazer_core::assets::FrontendAssets;
use chronogazer_core::audit::{AuditEntry, AuditLogList, AuditLogService};
use chronogazer_core::backup::{BackupInfo, BackupService, PendingRestoreInfo};
// #383 段階2b / R1-C: 収集サービス。C-2 で `collect_*` コマンドと起動時の
// 自動開始を足し、C-3a で読み出し 3 本（現在値・接続状態・イベント一覧）を
// 足した（監査の `resource` も床も REST と共有の定数）。
use chronogazer_core::collect::{
    registry_exclusions, resolve_data_dir, validate_history_request, CollectEventList,
    CollectHistory, CollectOutcome, CollectorService, CollectorStateView, ConnectionView,
    CurrentSampleView, EventPage, ExclusionView, Readout, COLLECT_AUDIT_RESOURCE,
    COLLECT_OPERATION_ROLE, COLLECT_READ_ROLE,
};
use chronogazer_core::db::{init_db, Db, InitDbError};
// #393: 表示グループ。検証・保存は REST と同じ `DisplayGroupService`。
use chronogazer_core::display_groups::{
    audit_detail as display_group_audit_detail, DisplayGroup, DisplayGroupPayload,
    DisplayGroupService, AUDIT_RESOURCE as DISPLAY_GROUPS_RESOURCE,
};
use chronogazer_core::events::event_channel;
use chronogazer_core::hub::{HubService, HubSubscriptionView, HubView};
use chronogazer_core::rest::{
    api_router, plc_connection_audit_detail, update_plc_connection, user_auth_state,
    CollectionGroupPayload, PlcConnectionPayload, PlcConnectionResponse, TagPayload,
};
use chronogazer_core::settings::{
    auth_server_combination_allowed, store_config, AuditSettings, AuthSettings, ServerSettings,
    SettingsService,
};
use chronogazer_core::simulation::{simulation_coverage, SimulationCoverageEntry};
use chronogazer_core::tag_address::{
    ensure_group_move_keeps_tags_readable, ensure_protocol_change_keeps_tags_readable,
    ensure_tag_fits_its_connection, ensure_tag_update_fits_its_connection, update_tag_checked,
    TagPlacement,
};
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
use std::sync::{Arc, Mutex};
use tauri::{Emitter, Manager, State};
use tokio::sync::{broadcast, Mutex as AsyncMutex};

/// App-wide state managed by Tauri (spec §10, §11).
struct AppState {
    /// The webview window's own session identity, set by `auth_login`/
    /// `auth_setup` and cleared by `auth_logout` - all called directly via
    /// `invoke()`, never through `/api/auth/login`. `Some` means logged in;
    /// carrying the full `UserIdentity` (not just a bool) lets
    /// `auth_change_password` recover the current `username` without a
    /// second round trip. banto v1.7.0 #204: a [`DesktopSession`], so every
    /// place that establishes a session states which kind it is, and every
    /// read goes through [`current_session`].
    ///
    /// banto v2.0.0（banto #260, banto の docs/design/session-controller-design.md
    /// §5.3, I-11 - admin-template の同名フィールドを写したもの）: wrapped in
    /// an [`AuthSlot`] with a write sequence, so a command that `.await`s
    /// between reading the slot and writing it (login's argon2 verify,
    /// logout's settings read) only writes when nothing re-bound the slot
    /// meanwhile ([`cas_session`]).
    auth: Mutex<AuthSlot>,
    /// The slow `.await`s the session-changing commands make before they
    /// write [`AppState::auth`] (credential verification / first-user setup,
    /// the auth-mode read, and `auth_config_apply`'s save), injected so tests
    /// can hold a command at that point and fix the completion order (banto
    /// design §8.3). Production wraps `users`/`settings` below
    /// ([`AuthIo::production`]).
    auth_io: AuthIo,
    /// Serializes "read the auth-mode settings, decide, act on the decision"
    /// (banto PR #264 re-review P2): [`auth_config_apply_body`] holds it from
    /// its first settings read through the save and the session re-bind,
    /// [`logout_body`] holds it around its post-clear re-read and install,
    /// [`install_account_unless_auth_disabled`] around its mode check and
    /// install, and the autologin toggles around their read-modify-write of
    /// the same settings rows.
    ///
    /// LOCK ORDER: `auth_config_lock` -> [`AppState::auth`]. `auth` is a
    /// std `Mutex` that is never held across an `.await`, so nothing can
    /// hold it while waiting for this lock - the reverse order cannot occur.
    auth_config_lock: AsyncMutex<()>,
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
    /// Backup/restore (spec M17): `VACUUM INTO` snapshots into
    /// `backups/<DB file name>/` next to the DB file (banto #280: per DB
    /// file), plus the restore staging flow. banto's
    /// `banto_admin_services::backup::BackupService` since I2a. Shares the
    /// same pool as `users`/`settings`/`audit` - only its `db_path` is
    /// unique to this service (needed to resolve that directory and the
    /// `restore-pending.sqlite3` inside it, see that module's doc comment).
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

/// Result of `auth_login`/`auth_setup`. `superseded`/`seq` (banto #260,
/// design §5.3/I-19): `superseded` is `true` when the credentials were
/// valid but another command re-bound the session slot while this one was
/// verifying, so the session was NOT installed ([`cas_session`]); `seq` is
/// the slot's write sequence after this command, so the frontend provider
/// can tell whether the session changed without a second round trip.
#[derive(Debug, Clone, Serialize)]
struct LoginResult {
    success: bool,
    error: Option<String>,
    superseded: bool,
    seq: u64,
}

/// Result of `auth_logout` (banto #260): the slot's write sequence after the
/// command - unchanged when it did nothing (auth-disabled mode, or another
/// command re-bound the slot while the logout was reading settings).
#[derive(Debug, Clone, Serialize)]
struct LogoutResult {
    seq: u64,
}

/// Result of `auth_change_password` (banto #260): the slot's write sequence
/// after the command - advanced when the session was re-bound to the new
/// `auth_epoch`.
#[derive(Debug, Clone, Serialize)]
struct ChangePasswordResult {
    seq: u64,
}

/// Result of `auth_resolve` (banto #260, design §5.3): the whole session in
/// one round trip, tagged with which slot write it is about.
///
/// - `checked`: the slot's `seq` read before the first `.await` (the session
///   this answer validated);
/// - `current`: the `seq` after this call's own settle - `checked + 1` iff
///   this call cleared a revoked session, otherwise `checked`;
/// - `stale`: another command re-bound the slot while this call was reading
///   the store, so nothing was written and the answer is about nothing
///   current (the frontend provider rejects it and asks again).
#[derive(Debug, Clone, Serialize)]
struct AuthResolveResult {
    identity: Option<Identity>,
    /// `"account"` or `"local"` (auth-disabled mode's synthetic session);
    /// `None` when there is no session.
    kind: Option<&'static str>,
    checked: u64,
    current: u64,
    stale: bool,
}

/// The webview session slot (banto #260, design §5.3, I-11).
///
/// このファイルで「design §x」「I-n」「S-n」と書くのは banto リポジトリの
/// `docs/design/session-controller-design.md`（v2.0.0）の節・不変条件・シナリオ番号。
/// 実装は admin-template（`apps/admin-template/src-tauri/src/lib.rs`）の写し。
///
/// `seq` advances on every write that is MEANT to change the binding -
/// login/setup/config-apply installing a session, logout/settle clearing
/// one, `change_own_password` re-binding one - even when the value does not
/// change (a logout of `None` still advances it, so a login that started
/// before it can no longer install). It does NOT advance on a refresh of the
/// same binding (`settle_session` updating role/display name), nor on the
/// auth-disabled-mode logout no-op.
#[derive(Debug, Default)]
struct AuthSlot {
    session: Option<DesktopSession>,
    seq: u64,
}

impl AuthSlot {
    fn new(session: Option<DesktopSession>) -> Self {
        Self { session, seq: 0 }
    }
}

type AuthFuture<T> = futures_util::future::BoxFuture<'static, Result<T, BantoError>>;
/// `auth_login`'s credential check (design §8.3 `CredentialVerifier`).
type CredentialVerifier =
    Arc<dyn Fn(String, String) -> AuthFuture<Option<UserIdentity>> + Send + Sync>;
/// `auth_setup`'s first-account creation. Not in design §8.3's sketch (which
/// names only the verifier and the auth-mode read): `auth_setup` awaits
/// `setup_first_user`, not `verify`, so S-18/S-19 need their own hold point.
type FirstUserSetup = Arc<dyn Fn(String, String, String) -> AuthFuture<UserIdentity> + Send + Sync>;
/// `auth_logout`'s auth-mode read (design §8.3 `AuthModeSource`).
type AuthModeSource = Arc<dyn Fn() -> AuthFuture<AuthSettings> + Send + Sync>;
/// `auth_config_apply`'s settings save. Not in design §8.3's sketch: injected
/// so a test can hold an apply right AFTER it saved (banto PR #264 re-review P2).
type AuthConfigSave = Arc<dyn Fn(AuthSettings) -> AuthFuture<()> + Send + Sync>;

/// See [`AppState::auth_io`].
struct AuthIo {
    verify: CredentialVerifier,
    setup_first_user: FirstUserSetup,
    auth_mode: AuthModeSource,
    save_auth_config: AuthConfigSave,
}

impl AuthIo {
    fn production(users: &UsersService, settings: &SettingsService) -> Self {
        let verify_users = users.clone();
        let setup_users = users.clone();
        let save_settings = settings.clone();
        let settings = settings.clone();
        Self {
            verify: Arc::new(move |username, password| {
                let users = verify_users.clone();
                Box::pin(async move { users.verify(&username, &password).await })
            }),
            setup_first_user: Arc::new(move |username, password, display_name| {
                let users = setup_users.clone();
                Box::pin(async move {
                    users
                        .setup_first_user(&username, &password, &display_name)
                        .await
                })
            }),
            auth_mode: Arc::new(move || {
                let settings = settings.clone();
                Box::pin(async move { settings.auth_config().await })
            }),
            save_auth_config: Arc::new(move |config| {
                let settings = save_settings.clone();
                Box::pin(async move { settings.set_auth_config(&config).await })
            }),
        }
    }
}

/// Read the slot's `seq` (and session) under one lock. Session-changing
/// commands call this BEFORE their first `.await` (I-11).
fn read_slot(state: &AppState) -> (Option<DesktopSession>, u64) {
    let slot = state.auth.lock().expect("auth mutex poisoned");
    (slot.session.clone(), slot.seq)
}

/// Compare-and-set the session slot (design §5.3, I-7/I-11): under one lock,
/// write `next` and advance `seq` only when `seq` is still `expected_seq`.
/// Advances even when `next` equals the current value. Returns whether it
/// wrote, and the `seq` after the call.
fn cas_session(state: &AppState, expected_seq: u64, next: Option<DesktopSession>) -> (bool, u64) {
    let mut slot = state.auth.lock().expect("auth mutex poisoned");
    if slot.seq != expected_seq {
        return (false, slot.seq);
    }
    slot.session = next;
    slot.seq += 1;
    (true, slot.seq)
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

/// `UserIdentity.id` shown for the synthetic auth-disabled-mode ("local")
/// session (spec M11). Display only: whether a session is synthetic is the
/// [`DesktopSession`] variant, never this value (a `users` row could in
/// principle be inserted with id 0).
const LOCAL_SESSION_ID: i64 = 0;

/// The webview session (banto v1.7.0 #204, admin-template の同名の型を写した
/// もの). An enum rather than a bare `UserIdentity` so there is no way to
/// establish or read a session without saying whether it belongs to a
/// `users` account - and therefore how [`current_session`] re-validates it.
#[derive(Debug, Clone, PartialEq)]
enum DesktopSession {
    /// A `users` account (`auth_login`, `auth_setup`, autologin): bound to
    /// the row's `id` + `auth_epoch` it was established with and
    /// re-validated against that row on every command.
    Account(UserIdentity),
    /// The synthetic identity of auth-disabled mode (spec M11,
    /// 「認証なしのローカルモード」). There is no account behind it; it is
    /// valid exactly while auth-disabled mode is ON, with that mode's
    /// CURRENT role - re-checked on every command too. Turning the mode OFF
    /// ends it on the next command (the login screen takes over).
    AuthDisabledLocal(UserIdentity),
}

impl DesktopSession {
    fn identity(&self) -> &UserIdentity {
        match self {
            Self::Account(identity) | Self::AuthDisabledLocal(identity) => identity,
        }
    }
}

/// The webview session, re-validated on every command (banto v1.7.0 #204, the Tauri
/// twin of `banto_server::AuthState::authenticate`):
///
/// - no session -> `Ok(None)`;
/// - [`DesktopSession::Account`]: the `users` row is re-read. Account gone,
///   or a different row id (deleted and re-created under the same username)
///   or `auth_epoch` (role change, password change/reset - from EITHER
///   transport) than the session was established with -> the session is
///   cleared and `Ok(None)`; otherwise the account's CURRENT row (role
///   included), which is also written back to the cached session;
/// - [`DesktopSession::AuthDisabledLocal`]: auth-disabled mode is re-read.
///   Turned off -> cleared and `Ok(None)` (the login screen takes over);
///   otherwise the synthetic identity with the mode's CURRENT role.
///
/// A read failure is `Err` and leaves the session in place (a DB hiccup must
/// not log the user out), failing the command instead. The decision after
/// the read is [`settle_session`] (the session may have changed while the
/// read was in flight). The lock is never held across an `.await`.
///
/// banto #260: when the slot was re-bound while the read was in flight
/// ([`Settled::Stale`]), nothing is written and this command decides on the
/// CURRENT session's binding (valid iff `fresh` reports exactly it) - the
/// same answer the pre-#260 `settle_session` gave in that case, so ordinary
/// commands keep their behavior; only `auth_resolve` reports the staleness.
async fn current_session(state: &AppState) -> Result<Option<DesktopSession>, BantoError> {
    let (cached, seq_at_entry) = read_slot(state);
    let Some(cached) = cached else {
        return Ok(None);
    };
    let fresh = read_session_source(state, &cached).await?;
    Ok(match settle_session(state, &cached, fresh, seq_at_entry) {
        Settled::Settled { session, .. } => session,
        Settled::Stale { valid_now } => valid_now,
    })
}

/// What the store says NOW about the session `cached` stands for: the
/// `users` row for an account session, or - for the synthetic session - the
/// synthetic identity with the mode's current role while auth-disabled mode
/// is on (`None` once it is off).
async fn read_session_source(
    state: &AppState,
    cached: &DesktopSession,
) -> Result<Option<DesktopSession>, BantoError> {
    Ok(match cached {
        DesktopSession::Account(session) => state
            .users
            .get_by_username(&session.username)
            .await?
            .map(DesktopSession::Account),
        DesktopSession::AuthDisabledLocal(session) => {
            let config = state.settings.auth_config().await?;
            config.disabled.then(|| {
                DesktopSession::AuthDisabledLocal(UserIdentity {
                    role: config.disabled_role,
                    ..session.clone()
                })
            })
        }
    })
}

/// Do `a` and `b` carry the same session binding: the same kind and, for an
/// account, the same row id and `auth_epoch`?
fn same_binding(a: &DesktopSession, b: &DesktopSession) -> bool {
    match (a, b) {
        (DesktopSession::Account(a), DesktopSession::Account(b)) => {
            a.id == b.id && a.auth_epoch == b.auth_epoch
        }
        (DesktopSession::AuthDisabledLocal(_), DesktopSession::AuthDisabledLocal(_)) => true,
        _ => false,
    }
}

/// Outcome of [`settle_session`] (banto #260, design §5.3).
#[derive(Debug)]
enum Settled {
    /// The slot's `seq` moved since `seq_at_entry` (another command
    /// installed, cleared or re-bound the session while the store was being
    /// read). Nothing was written. `valid_now` is the CURRENT session (the
    /// slot's value, newer than `fresh`) if its own binding is exactly what
    /// `fresh` reports (e.g. a re-bind by `change_own_password` that `fresh`
    /// already reflects), for the
    /// ordinary commands' [`current_session`]; `auth_resolve` reports
    /// `stale` instead of using it.
    Stale { valid_now: Option<DesktopSession> },
    /// The slot was not re-bound meanwhile, and was settled from `fresh`.
    /// `seq_after == seq_before + 1` iff this call cleared the session.
    Settled {
        session: Option<DesktopSession>,
        seq_before: u64,
        seq_after: u64,
    },
}

/// Decide on `fresh` (read with no lock held) against the session as it is
/// NOW, under one lock:
///
/// - if the slot's `seq` is no longer `seq_at_entry` (read together with
///   `cached`, before the store read), the session was re-bound meanwhile:
///   write nothing and return [`Settled::Stale`] (banto #260, I-11/I-23);
/// - otherwise the slot still holds `cached`'s binding (only same-binding
///   refreshes, which do not move `seq`, can have happened). Valid iff that
///   binding is exactly what `fresh` reports: refreshed to `fresh` (current
///   role and name) WITHOUT advancing `seq`. Not valid (account gone,
///   re-keyed, `auth_epoch` advanced - role change included - or
///   auth-disabled mode turned off): cleared, advancing `seq`.
///
/// This never moves a session to the store's newest epoch on its behalf: a
/// session that was not itself re-bound still ends.
///
/// Why a refresh never writes an old answer back over a newer write: every
/// write to the slot that changes what the session is authorized as advances
/// `seq` - a (re-)bind, a clear, and (re-review of banto #266 P1, S-96) a role
/// change of the synthetic session by [`rebind_local_session`]. So an
/// unchanged `seq` means nothing was written since `cached`/`fresh` were
/// read, and `fresh` is at least as new as the slot. (An account's role
/// change advances its `auth_epoch`, so `fresh` is not the same binding and
/// the session is cleared instead.) The one thing a refresh can still put
/// back is an account's `display_name` (freshness audit of banto #266, P3-1): two
/// concurrent refreshes of the same binding do not move `seq`, so the one
/// that read the store first may write last. That is accepted: the name is
/// display only (authorization reads role/epoch, which a refresh cannot
/// rewind), and the next check refreshes it again; there is no version on
/// the name to compare.
fn settle_session(
    state: &AppState,
    cached: &DesktopSession,
    fresh: Option<DesktopSession>,
    seq_at_entry: u64,
) -> Settled {
    let mut slot = state.auth.lock().expect("auth mutex poisoned");
    let valid = match slot.session.as_ref() {
        Some(now) => fresh.filter(|fresh| same_binding(now, fresh)),
        None => None,
    };
    if slot.seq != seq_at_entry {
        // The slot was written after `fresh` was read, so the slot's own
        // value is the newer one: report IT (not `fresh`) when the binding
        // matches - e.g. a Local role changed by an apply after this read
        // (freshness audit of banto #266, P3-2, S-102).
        let valid_now = valid.and(slot.session.clone());
        return Settled::Stale { valid_now };
    }
    debug_assert!(slot
        .session
        .as_ref()
        .is_some_and(|now| same_binding(now, cached)));
    let seq_before = slot.seq;
    if valid.is_some() {
        slot.session = valid.clone();
    } else {
        slot.session = None;
        slot.seq += 1;
    }
    Settled::Settled {
        session: valid,
        seq_before,
        seq_after: slot.seq,
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
///
/// banto v1.7.0 #204: the session is re-validated against the `users` table
/// first ([`current_session`]) - the role checked (and the identity
/// returned) is the account's CURRENT one, and a deleted/re-keyed account's
/// session is ended (`Unauthorized`), exactly like REST's `require_auth` +
/// `RoleGuard`.
async fn require_role(
    state: &AppState,
    min: Role,
    resource: &str,
) -> Result<UserIdentity, BantoError> {
    let current = current_session(state)
        .await?
        .map(|session| session.identity().clone());
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

/// Record a successful, actor-attributed audit event from a Tauri command
/// (spec M14): `origin` is always `"tauri"` and `result` always `"ok"`.
/// banto v2.0.0 移行で admin-template の同名ヘルパーを写したもの - 認証の
/// コマンド（login/setup/logout、認証モードの切替に伴うセッションの付け替え、
/// autologin・settings_set）だけが使う。それ以外の既存コマンドは従来どおり
/// `AuditEntry` を手で組み立てている（この PR では書き換えない）。
async fn record_ok(
    audit: &AuditLogService,
    actor: &UserIdentity,
    action: &str,
    resource: &str,
    entity_id: Option<&str>,
    detail: Option<serde_json::Value>,
) {
    audit
        .record(AuditEntry {
            actor_username: Some(&actor.username),
            actor_role: Some(actor.role.as_str()),
            action,
            resource,
            entity_id,
            detail,
            origin: "tauri",
            result: "ok",
        })
        .await;
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
///
/// banto #260 (design §5.3, S-18/S-19): the session is installed only if no
/// other command re-bound the slot while the account was being created
/// ([`cas_session`] against the `seq` read before the first `.await`);
/// otherwise the account still exists but the result is `superseded`.
#[tauri::command]
async fn auth_setup(
    state: State<'_, AppState>,
    username: String,
    password: String,
    display_name: String,
) -> Result<LoginResult, BantoError> {
    setup_body(&state, username, password, display_name).await
}

/// Message for a `LoginResult { superseded: true }` (the credentials were
/// valid, but another session was established first).
const SUPERSEDED_LOGIN_MESSAGE: &str =
    "別のセッションが先に確定したため、このログインは適用されませんでした";

/// Body of [`auth_setup`] (testable with a plain `&AppState`, design §8.3).
/// Slot-clearing errors: none - every `Err` is returned before the slot is
/// written (the TS provider's revision relies on this, design I-19).
async fn setup_body(
    state: &AppState,
    username: String,
    password: String,
    display_name: String,
) -> Result<LoginResult, BantoError> {
    let (_, seq_at_entry) = read_slot(state);
    // Owner decision on banto #266 (S-95): in auth-disabled mode no account session
    // is ever installed. Checked on entry so no account is created for a
    // setup that could not sign in; checked again at the install below
    // (the mode can be switched on while the account is being created).
    if state.settings.auth_config().await?.disabled {
        return Ok(auth_disabled_login_result(state));
    }
    match (state.auth_io.setup_first_user)(username, password, display_name).await {
        Ok(identity) => {
            record_ok(&state.audit, &identity, "setup", "auth", None, None).await;
            install_account_unless_auth_disabled(state, seq_at_entry, identity).await
        }
        Err(err @ BantoError::Validation { .. }) => Err(err),
        Err(other) => Ok(LoginResult {
            success: false,
            error: Some(other.to_string()),
            superseded: false,
            seq: read_slot(state).1,
        }),
    }
}

/// `LoginResult.error` of a login/setup refused because auth-disabled mode is
/// on (S-95).
const AUTH_DISABLED_LOGIN_MESSAGE: &str =
    "ログイン不要モード中はアカウントでログインできません。設定で通常のログインに戻してください";

/// The refusal of a login/setup in auth-disabled mode (owner decision on
/// banto #266, S-95): `success: false`, not `superseded` (no other session won a
/// race - the mode forbids it), nothing written, the current `seq`. An `Ok`
/// result rather than an `Err`, so the TS provider observes the unchanged
/// `seq` and reports nothing (I-19), and the login screen shows `error`.
fn auth_disabled_login_result(state: &AppState) -> LoginResult {
    LoginResult {
        success: false,
        error: Some(AUTH_DISABLED_LOGIN_MESSAGE.to_string()),
        superseded: false,
        seq: read_slot(state).1,
    }
}

/// Install a verified account session only while auth-disabled mode is off
/// (owner decision on banto #266, S-95): so that
/// `auth.disabled == true <=> DesktopSession::AuthDisabledLocal` holds
/// always. Under `auth_config_lock` (taken AFTER the password check, never
/// held across it), the mode is read and, if it is off, the session is
/// installed with the `seq` compare-and-set ([`install_login`]); if it is on,
/// nothing is written. `auth_config_apply(true)` re-binds under the same lock
/// right after its save, so the two cannot interleave: an apply first leaves
/// Local (this refuses), a login first leaves Account (the apply then
/// replaces it). A failed mode read is an `Err` returned before any slot
/// write (I-19). Lock order: `auth_config_lock` -> `state.auth`.
async fn install_account_unless_auth_disabled(
    state: &AppState,
    seq_at_entry: u64,
    identity: UserIdentity,
) -> Result<LoginResult, BantoError> {
    let _auth_config = state.auth_config_lock.lock().await;
    if state.settings.auth_config().await?.disabled {
        return Ok(auth_disabled_login_result(state));
    }
    Ok(install_login(
        state,
        seq_at_entry,
        DesktopSession::Account(identity),
    ))
}

/// Install a verified login/setup session with [`cas_session`] and build the
/// command result: `superseded` when the slot was re-bound after
/// `seq_at_entry`.
fn install_login(state: &AppState, seq_at_entry: u64, session: DesktopSession) -> LoginResult {
    let (written, seq) = cas_session(state, seq_at_entry, Some(session));
    if written {
        LoginResult {
            success: true,
            error: None,
            superseded: false,
            seq,
        }
    } else {
        LoginResult {
            success: false,
            error: Some(SUPERSEDED_LOGIN_MESSAGE.to_string()),
            superseded: true,
            seq,
        }
    }
}

/// banto #260 (design §5.3, S-16): the session is installed only if no other
/// command (a logout, another login) re-bound the slot while the password
/// was being verified. The `login` audit entry is still recorded at
/// verification success, before the install (design §1.7, decision 7).
#[tauri::command]
async fn auth_login(
    state: State<'_, AppState>,
    username: String,
    password: String,
) -> Result<LoginResult, BantoError> {
    login_body(&state, username, password).await
}

/// Body of [`auth_login`] (testable with a plain `&AppState`, design §8.3).
/// Slot-clearing errors: none - every `Err` is returned before the slot is
/// written (the TS provider's revision relies on this, design I-19).
async fn login_body(
    state: &AppState,
    username: String,
    password: String,
) -> Result<LoginResult, BantoError> {
    let (_, seq_at_entry) = read_slot(state);
    // Owner decision on banto #266 (S-95): refused in auth-disabled mode - before
    // the (slow) password check, which then records nothing; and again at
    // the install, after the check (see
    // [`install_account_unless_auth_disabled`]).
    if state.settings.auth_config().await?.disabled {
        return Ok(auth_disabled_login_result(state));
    }
    match (state.auth_io.verify)(username.clone(), password).await? {
        Some(identity) => {
            // Recorded at verification success, before the install decides
            // (design §1.7, decision 7) - as for a superseded login.
            record_ok(&state.audit, &identity, "login", "auth", None, None).await;
            install_account_unless_auth_disabled(state, seq_at_entry, identity).await
        }
        None => {
            state
                .audit
                .record(AuditEntry {
                    // Bounded like the REST path (banto #278, admin-template
                    // v3.0.0 の `login_body` と同じ形): the name is
                    // caller-supplied and need not be a real account. The
                    // dummy argon2 verify for an over-long name happens inside
                    // `UsersService::verify` (`auth_io.verify`).
                    actor_username: Some(&chronogazer_core::users::bound_username_for_audit(
                        &username,
                    )),
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
                superseded: false,
                seq: read_slot(state).1,
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
///
/// banto #260 (design §5.3, S-17/S-67): the slot is cleared only if no other
/// command re-bound it while the auth mode was being read ([`cas_session`]
/// against the `seq` read before the first `.await`, advancing `seq` even
/// when there was no session). A logout overtaken by a login leaves that
/// login's session in place and records no `logout`. The auth-disabled
/// no-op does not advance `seq`. A logout that cleared re-reads the mode
/// and, if auth-disabled mode was switched on meanwhile, installs the
/// synthetic session (banto PR #264 review P1, see [`logout_body`]). Either way
/// the result carries the `seq` after the command.
#[tauri::command]
async fn auth_logout(state: State<'_, AppState>) -> Result<LogoutResult, BantoError> {
    logout_body(&state).await
}

/// Body of [`auth_logout`] (testable with a plain `&AppState`, design §8.3).
/// Slot-clearing errors: none - every `Err` is returned before the slot is
/// written (the TS provider's revision relies on this, design I-19).
async fn logout_body(state: &AppState) -> Result<LogoutResult, BantoError> {
    let (previous, seq_at_entry) = read_slot(state);
    if (state.auth_io.auth_mode)().await?.disabled {
        return Ok(LogoutResult {
            seq: read_slot(state).1,
        });
    }
    let (written, mut seq) = cas_session(state, seq_at_entry, None);
    if !written {
        return Ok(LogoutResult { seq });
    }
    // banto PR #264 review P1: the mode read above may be stale - auth-disabled
    // mode can have been switched on before this clear. (Since the owner
    // review of banto #266 P1 an apply re-binds the session right after its save,
    // which moves `seq` so this clear would not have been written; the
    // re-read is kept as the defense for any path that saves the mode
    // without re-binding.) Re-read the mode and, if it is now disabled, install
    // the synthetic session - but only while the slot is still exactly what
    // this clear left (`seq` unchanged and empty), so a later login or a
    // config-apply that already installed it is never overwritten and the
    // synthetic session is never installed twice ([`rebind_local_session`]
    // with this clear's `seq`: an unchanged `seq` means the slot is still
    // the empty one this clear left).
    //
    // A failed re-read is NOT an error of this logout: the clear has
    // happened (and an `Err` after a slot write would break the TS
    // provider's "errors are returned before the slot is written" contract,
    // design I-19), so the logout reports success and installs nothing. The
    // gap is then filled by the next `auth_config_apply`, or by `run()`'s
    // bootstrap on the next launch.
    //
    // banto PR #264 re-review P2: the re-read and the install run under
    // `auth_config_lock`, so an `apply(false)` cannot complete between the
    // value read here and the install based on it. The FIRST read (the
    // auth-disabled no-op decision above) stays outside the lock: it writes
    // nothing on its own, and a stale "enabled" answer there is exactly what
    // this re-read corrects; a stale "disabled" answer only makes this
    // logout a no-op that the frontend re-resolves.
    let rebind = {
        let _auth_config = state.auth_config_lock.lock().await;
        match (state.auth_io.auth_mode)().await {
            Ok(config) if config.disabled => {
                let (rebind, seq_now) =
                    rebind_local_session(state, config.disabled_role, Some(seq), false);
                seq = seq_now;
                rebind
            }
            Ok(_) => LocalRebind::Skipped,
            Err(err) => {
                eprintln!("banto: ログアウト後の認証モードの再読み込みに失敗しました: {err}");
                LocalRebind::Skipped
            }
        }
    };
    if let Some(session) = previous {
        record_ok(
            &state.audit,
            session.identity(),
            "logout",
            "auth",
            None,
            None,
        )
        .await;
    }
    // Only `Installed` can happen here: with `seq` unchanged since the clear,
    // the slot is empty.
    record_local_rebind(&state.audit, &rebind).await;
    Ok(LogoutResult { seq })
}

/// banto v1.7.0 #204: validated like every other command ([`current_session`]), so
/// a session ended by a change to its account reports "logged out" here
/// too, not just on the next guarded command.
#[tauri::command]
async fn auth_check(state: State<'_, AppState>) -> Result<bool, BantoError> {
    Ok(current_session(&state).await?.is_some())
}

/// banto v1.7.0 #204: the CURRENT identity (role/display name as stored now).
#[tauri::command]
async fn auth_identity(state: State<'_, AppState>) -> Result<Option<Identity>, BantoError> {
    Ok(current_session(&state)
        .await?
        .as_ref()
        .map(|session| identity_from(session.identity())))
}

/// banto #260 (design §5.3): the frontend provider's `resolve()` - the
/// session, its kind, and which slot write the answer is about, in one round
/// trip. Validated like [`auth_identity`] (the account row / auth-disabled
/// mode is re-read, so role and display name are current), but the `seq` is
/// read BEFORE the store read and the settle happens under one lock:
/// `current == checked + 1` iff this call cleared a revoked session, and a
/// slot re-bound meanwhile is reported as `stale` with nothing written.
#[tauri::command]
async fn auth_resolve(state: State<'_, AppState>) -> Result<AuthResolveResult, BantoError> {
    resolve_body(&state).await
}

fn session_kind(session: &DesktopSession) -> &'static str {
    match session {
        DesktopSession::Account(_) => "account",
        DesktopSession::AuthDisabledLocal(_) => "local",
    }
}

/// Body of [`auth_resolve`] (testable with a plain `&AppState`, design §8.3).
/// Its `Ok` answer carries any clear it made (`current`); an `Err` is
/// returned before the slot is written.
async fn resolve_body(state: &AppState) -> Result<AuthResolveResult, BantoError> {
    let (cached, seq_at_entry) = read_slot(state);
    let Some(cached) = cached else {
        return Ok(AuthResolveResult {
            identity: None,
            kind: None,
            checked: seq_at_entry,
            current: seq_at_entry,
            stale: false,
        });
    };
    let fresh = read_session_source(state, &cached).await?;
    Ok(match settle_session(state, &cached, fresh, seq_at_entry) {
        Settled::Stale { .. } => AuthResolveResult {
            identity: None,
            kind: None,
            checked: seq_at_entry,
            current: read_slot(state).1,
            stale: true,
        },
        Settled::Settled {
            session,
            seq_before,
            seq_after,
        } => AuthResolveResult {
            identity: session
                .as_ref()
                .map(|session| identity_from(session.identity())),
            kind: session.as_ref().map(session_kind),
            checked: seq_before,
            current: seq_after,
            stale: false,
        },
    })
}

/// Body of [`auth_change_password`], split out so the audit-recording
/// behavior (spec M14) is testable with a plain `&AppState` in this crate's
/// own `cargo test` - `tauri::State` cannot be constructed outside a running
/// tauri app, but it derefs to `&AppState`, so the command below is a
/// one-line adapter.
///
/// banto v1.7.0 #204: the session is validated first ([`current_session`]). The
/// change advances the account's `auth_epoch`, ending every session of it
/// (REST tokens on other devices included); this webview session is then
/// re-bound to the new epoch - it just proved the current password - unless
/// something else changed the account in between (then it ends too). Same
/// policy as REST's `/api/auth/change-password`.
///
/// banto #260 (design §5.3, I-11, S-68): the re-bind is a write that changes
/// the binding, so it advances the slot's `seq`; the result carries the
/// `seq` after the command (unchanged when the re-bind did not happen).
///
/// Slot-clearing errors: ONLY `BantoError::Unauthorized` can be returned
/// after the slot was written - the session check at the start
/// ([`current_session`]) clears a revoked session (advancing `seq`) and this
/// then fails with `Unauthorized`. Every other `Err` (`Forbidden`, the
/// `Validation` of a wrong current password, storage errors) is returned
/// with the slot unwritten by this command. The TS provider's revision
/// relies on this split (design I-19).
async fn change_own_password(
    state: &AppState,
    current_password: &str,
    new_password: &str,
) -> Result<ChangePasswordResult, BantoError> {
    let identity = match current_session(state).await? {
        Some(DesktopSession::Account(identity)) => identity,
        // The synthetic auth-disabled session owns no credentials: never let
        // it change a real account that happens to be named "local".
        Some(DesktopSession::AuthDisabledLocal(_)) => return Err(BantoError::Forbidden),
        None => return Err(BantoError::Unauthorized),
    };
    let new_epoch = state
        .users
        .change_password(&identity.username, current_password, new_password)
        .await?;
    let seq = {
        let mut slot = state.auth.lock().expect("auth mutex poisoned");
        if new_epoch == identity.auth_epoch + 1 {
            let mut rebound = false;
            if let Some(DesktopSession::Account(session)) = slot.session.as_mut() {
                if session.id == identity.id && session.auth_epoch == identity.auth_epoch {
                    session.auth_epoch = new_epoch;
                    rebound = true;
                }
            }
            if rebound {
                slot.seq += 1;
            }
        }
        slot.seq
    };
    // Spec M14: a self-service password change is a security event (it is
    // also what naturally invalidates an M11 autologin credential), so it IS
    // audited - actor and entity are both the caller. `detail` stays `None`:
    // neither the old nor the new password (nor any hash) may ever be
    // recorded.
    record_ok(
        &state.audit,
        &identity,
        "password_change",
        "users",
        Some(&identity.id.to_string()),
        None,
    )
    .await;
    Ok(ChangePasswordResult { seq })
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
) -> Result<ChangePasswordResult, BantoError> {
    change_own_password(&state, &current_password, &new_password).await
}

// --- M11: auth-disabled mode + desktop autologin ---------------------------

/// The synthetic identity of auth-disabled mode (spec M11). The ONE
/// definition shared by `run()`'s bootstrap and [`rebind_local_session`]
/// (used by [`auth_config_apply_body`] and [`logout_body`]), so they can
/// never drift apart. `id: 0` is not a
/// real `users` row - nothing ever looks a synthetic session up by id (no
/// change-password/self-deletion flows apply to it), so there is no real
/// row to alias.
fn local_identity(role: Role) -> UserIdentity {
    UserIdentity {
        id: LOCAL_SESSION_ID,
        username: "local".to_string(),
        display_name: "ローカルユーザー".to_string(),
        role,
        auth_epoch: 0,
    }
}

/// Spec M14: auth-disabled mode still records a `login` for its synthetic
/// session, same as a normal login would - it is still "someone" starting to
/// use the app, just without a credential check.
async fn record_local_login(audit: &AuditLogService, identity: &UserIdentity) {
    record_ok(
        audit,
        identity,
        "login",
        "auth",
        None,
        Some(serde_json::json!({ "mode": "auth_disabled" })),
    )
    .await;
}

/// What [`rebind_local_session`] did to the slot.
#[derive(Debug, Clone, PartialEq)]
enum LocalRebind {
    /// Nothing written: `expected_seq` was given and the slot had moved.
    Skipped,
    /// The slot was empty; the synthetic session was installed (`seq` + 1).
    Installed(UserIdentity),
    /// An account session was replaced by the synthetic one (`seq` + 1). The
    /// caller records the account's end and the synthetic `login`.
    Replaced {
        previous: UserIdentity,
        local: UserIdentity,
    },
    /// The synthetic session was there with another role: its role was
    /// changed (`seq` + 1 - an authorization-context change, re-review of
    /// banto #266 P1, S-96). No session audit (the `settings_change` records it).
    RoleChanged,
    /// The synthetic session was there with this role: nothing written,
    /// `seq` unchanged.
    Unchanged,
}

/// The auth-mode change's session rebind (banto #260 実装-3, owner review of
/// banto #266 P1, design §5.3): make the slot hold the auth-disabled synthetic
/// session for `role`, under ONE lock, so that
/// `auth.disabled == true <=> DesktopSession::AuthDisabledLocal <=>
/// auth_resolve kind == "local"` holds as soon as the mode is saved:
///
/// - `None -> Local` and `Account(A) -> Local`: written, `seq` + 1 (a
///   binding change: a login that read the old `seq` can no longer install
///   over it, S-94);
/// - `Local(r) -> Local(r')` with `r != r'`: the role is written and `seq`
///   advances (re-review of banto #266 P1, S-96). [`same_binding`] treats every
///   synthetic session as one binding, so `seq` is the only thing that tells
///   an in-flight [`settle_session`] that the slot was written after it read
///   the store; without the advance, an `auth_resolve` that had read the old
///   role would write it back over the new one;
/// - `Local(r) -> Local(r)`: nothing written, `seq` unchanged - UNLESS
///   `role_changed` (S-98): the caller's own change of the mode's role,
///   decided from the settings BEFORE its save, not from the slot. Between
///   the save and this call, an `auth_resolve`/`current_session` that does
///   not take `auth_config_lock` can read the new role from the store and
///   refresh the slot to it ([`settle_session`] writes the role but never
///   advances `seq` - advancing there would break "`current != checked` iff
///   this call cleared", I-23). The slot then already holds the new role,
///   yet an even older settle that read the OLD role still sees the same
///   `seq`; so the role change is advanced here regardless of the slot. A
///   settle's refresh writes the role without advancing `seq`; telling
///   in-flight settles about a role change is this forced advance's job.
///
/// With `expected_seq` (the logout's re-read), nothing is written unless
/// `seq` is still that value (the slot is then exactly what the logout's
/// clear left: empty). The guard never lives across an `.await` (banto PR #182 の
/// Copilot review). Returns the outcome and the `seq` after the call.
fn rebind_local_session(
    state: &AppState,
    role: Role,
    expected_seq: Option<u64>,
    role_changed: bool,
) -> (LocalRebind, u64) {
    let mut slot = state.auth.lock().expect("auth mutex poisoned");
    if expected_seq.is_some_and(|seq| seq != slot.seq) {
        return (LocalRebind::Skipped, slot.seq);
    }
    let (outcome, next) = match slot.session.as_ref() {
        Some(DesktopSession::AuthDisabledLocal(local)) if local.role == role && !role_changed => {
            return (LocalRebind::Unchanged, slot.seq);
        }
        Some(DesktopSession::AuthDisabledLocal(local)) => (
            LocalRebind::RoleChanged,
            UserIdentity {
                role,
                ..local.clone()
            },
        ),
        Some(DesktopSession::Account(previous)) => {
            let local = local_identity(role);
            (
                LocalRebind::Replaced {
                    previous: previous.clone(),
                    local: local.clone(),
                },
                local,
            )
        }
        None => {
            let local = local_identity(role);
            (LocalRebind::Installed(local.clone()), local)
        }
    };
    slot.session = Some(DesktopSession::AuthDisabledLocal(next));
    slot.seq += 1;
    (outcome, slot.seq)
}

/// Make the session follow a saved auth mode (`saved`), compared with the
/// mode before the save (`previous`) - synchronous, under `state.auth`'s
/// lock only (the caller holds `auth_config_lock`):
/// - mode on: [`rebind_local_session`], with `seq` forced to advance when
///   `(disabled, disabled_role)` changed in this save (S-98; freshness audit
///   of banto #266 P2-2: `false -> true` included, even if the slot already holds
///   Local);
/// - mode turned off (`true -> false`): [`end_local_session`] (S-99);
/// - mode off before and after: nothing.
fn apply_mode_to_session(
    state: &AppState,
    previous: &AuthSettings,
    saved: &AuthSettings,
) -> (LocalRebind, Option<UserIdentity>) {
    if saved.disabled {
        let changed =
            (previous.disabled, previous.disabled_role) != (saved.disabled, saved.disabled_role);
        (
            rebind_local_session(state, saved.disabled_role, None, changed).0,
            None,
        )
    } else if previous.disabled {
        (LocalRebind::Skipped, end_local_session(state))
    } else {
        (LocalRebind::Skipped, None)
    }
}

/// Audit [`apply_mode_to_session`]'s outcome (spec M14): the rebind's
/// entries ([`record_local_rebind`]) and an ended synthetic session's
/// `logout` with `detail: { "reason": "auth_enabled" }` (told apart from a
/// user's logout, whose `detail` is empty).
async fn record_mode_session_change(
    audit: &AuditLogService,
    rebind: &LocalRebind,
    ended_local: Option<UserIdentity>,
) {
    record_local_rebind(audit, rebind).await;
    if let Some(local) = ended_local {
        record_ok(
            audit,
            &local,
            "logout",
            "auth",
            None,
            Some(serde_json::json!({ "reason": "auth_enabled" })),
        )
        .await;
    }
}

/// End the auth-disabled synthetic session because the mode was turned off
/// (re-review of banto #266 P1, S-99): under ONE lock, `Local -> None`, advancing
/// `seq`, returning the ended identity (the caller records its end). Anything
/// else is left alone: `None` has nothing to end, and an `Account` is not
/// the mode's session (with `auth.disabled == true <=> AuthDisabledLocal`
/// holding, the slot cannot hold one while the mode was on; were it there,
/// the account's own re-validation decides its fate).
fn end_local_session(state: &AppState) -> Option<UserIdentity> {
    let mut slot = state.auth.lock().expect("auth mutex poisoned");
    let Some(DesktopSession::AuthDisabledLocal(local)) = slot.session.as_ref() else {
        return None;
    };
    let local = local.clone();
    slot.session = None;
    slot.seq += 1;
    Some(local)
}

/// Audit what [`rebind_local_session`] did (spec M14): the synthetic
/// `login` for an install; for a replaced account, that account's session
/// end as `logout` with `detail: { "reason": "auth_disabled" }` (not a
/// logout the user asked for - the detail tells the two apart) and then the
/// synthetic `login`. A role change records nothing here (the synthetic
/// session goes on; the apply's `settings_change` records the change).
async fn record_local_rebind(audit: &AuditLogService, outcome: &LocalRebind) {
    match outcome {
        LocalRebind::Installed(local) => record_local_login(audit, local).await,
        LocalRebind::Replaced { previous, local } => {
            record_ok(
                audit,
                previous,
                "logout",
                "auth",
                None,
                Some(serde_json::json!({ "reason": "auth_disabled" })),
            )
            .await;
            record_local_login(audit, local).await;
        }
        LocalRebind::Skipped | LocalRebind::RoleChanged | LocalRebind::Unchanged => {}
    }
}

/// Current auth-mode settings (spec M11): any authenticated role may read
/// this (it only feeds a settings-screen display), and it never carries the
/// autologin password - `AuthSettings` itself has no such field (see its doc
/// comment in `chronogazer_core::settings`).
#[tauri::command]
async fn auth_config_get(state: State<'_, AppState>) -> Result<AuthSettings, BantoError> {
    auth_config_get_body(&state).await
}

/// Body of [`auth_config_get`] (testable with a plain `&AppState`). Viewer
/// floor on purpose: a kiosk's `viewer` synthetic session needs it to show
/// the security screen it uses to turn the mode back off or raise its role
/// (the ESCAPE HATCH of [`auth_config_apply_body`]).
async fn auth_config_get_body(state: &AppState) -> Result<AuthSettings, BantoError> {
    require_role(state, Role::Viewer, "settings").await?;
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
///
/// admin-template にある「BOOTSTRAP WINDOW」（アカウント 0 件・セッション
/// なしでも呼べる初回画面の「ログインなしで使い始める」経路、banto
/// choiapp-feedback-2026-09 §5）は ChronoGazer には**移植していない**
/// （v2 移行 PR1a の範囲外。この経路の有無はオーナー判断で別に決める）。
/// したがってセッションなしの呼び出しは、従来どおり `Unauthorized`。
///
/// Enabling the mode re-binds the session to the same synthetic local
/// identity `run()`'s bootstrap would create on the next launch (and
/// records the same synthetic `login` entry), so the webview enters the app
/// without a restart - also when an account session was signed in: that
/// session is replaced (owner review of banto #266 P1, [`rebind_local_session`]),
/// so `auth.disabled == true` always goes with the synthetic session.
///
/// Body of [`auth_config_apply`] (spec M14 pattern) so its escape-hatch
/// authz (the actor may be `None`) + audit behavior is testable with a
/// plain `&AppState`.
async fn auth_config_apply_body(
    state: &AppState,
    disabled: bool,
    disabled_role: &str,
) -> Result<AuthSettings, BantoError> {
    // banto PR #264 re-review P2: held from the first settings read through the
    // save and the synthetic-session install below (see
    // `AppState::auth_config_lock`), so no other apply (or a logout's
    // re-read + install) can interleave with this decision.
    let _auth_config = state.auth_config_lock.lock().await;
    // Read through the injectable `auth_mode` (production: the same
    // `SettingsService::auth_config`) so tests can fix this command's order
    // against a concurrent logout (design §8.3).
    let currently_disabled = (state.auth_io.auth_mode)().await?.disabled;
    // Spec M14: the escape hatch (see this command's doc comment) means
    // `require_role` may not run at all - capture whatever actor identity
    // exists directly in that case, so the audit entry below still has one
    // when possible, instead of skipping the escape-hatch path's write
    // entirely.
    let actor = if currently_disabled {
        // Audit label only (the escape hatch does not authorize by session).
        state
            .auth
            .lock()
            .expect("auth mutex poisoned")
            .session
            .as_ref()
            .map(|session| session.identity().clone())
    } else {
        Some(require_role(state, Role::Admin, "settings").await?)
    };

    // An unrecognized role string falls back to `admin` (same convention as
    // `SettingsService::auth_config`'s own read-time fallback) rather than
    // failing the whole command - a bad value here must never leave the app
    // unable to determine ANY role for the synthetic identity.
    let role = Role::from_str(disabled_role).unwrap_or(Role::Admin);

    let mut config = state.settings.auth_config().await?;
    // The settings before this apply (read under `auth_config_lock`, so no
    // other apply changes them meanwhile): a change of the mode's role while
    // the mode stays on always advances `seq` at the rebind below (S-98).
    let previous = config.clone();
    config.disabled = disabled;
    config.disabled_role = role;
    if let Err(err) = (state.auth_io.save_auth_config)(config.clone()).await {
        // Freshness audit of banto #266 (P2-3, S-104): the save is one transaction
        // (`SettingsService::set_auth_config`), but should any save fail
        // part-way, the session must still follow what IS stored: re-read
        // it and re-bind/end from that, then report the error.
        if let Ok(stored) = state.settings.auth_config().await {
            let (rebind, ended_local) = apply_mode_to_session(state, &previous, &stored);
            record_mode_session_change(&state.audit, &rebind, ended_local).await;
        }
        return Err(err);
    }
    // Owner review of banto #266 P1 (design §5.3, S-94): turning the mode on
    // re-binds the session to the synthetic one RIGHT after the save, with
    // no `.await` in between (still under `auth_config_lock`), so
    // `auth.disabled == true` is never observable together with an account
    // session. None -> Local and Account -> Local advance `seq` (a login
    // that started before can no longer install over it); Local -> Local
    // advances `seq` only when the role changes (S-96).
    //
    // banto #260 (banto PR #264 review P1, kept): no "`seq` still the one read at
    // entry" condition - a logout that cleared the slot after this command
    // started would otherwise leave the mode with no session at all.
    // [`logout_body`] re-reads the mode after its clear for the other order;
    // both run under `auth_config_lock`, and the rebind is idempotent for
    // `Local`, so the synthetic session is never installed twice.
    //
    // Turning the mode OFF (re-review of banto #266 P1, S-99) ends the synthetic
    // session in the same step as the save (`Local -> None`, `seq` + 1), so
    // `auth.disabled == false` is never observable together with it - an
    // `auth_resolve` that had read `disabled = true` before the save then
    // settles `Stale` instead of confirming `local`. (This used to be left to
    // the next `auth_resolve`, on the grounds that the apply's answer carries
    // no `seq`; the frontend provider absorbs an unobserved `seq` advance
    // anyway - the catch-up of S-84.) Only on a `true -> false` change: a
    // re-save of `false` touches nothing.
    let (rebind, ended_local) = apply_mode_to_session(state, &previous, &config);
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

    // The session change is recorded after the settings change it follows
    // from: from the settings screen, the admin's session end (`logout`,
    // reason `auth_disabled`) and the synthetic `login`.
    record_mode_session_change(&state.audit, &rebind, ended_local).await;
    Ok(config)
}

#[tauri::command]
async fn auth_config_apply(
    state: State<'_, AppState>,
    disabled: bool,
    disabled_role: String,
) -> Result<AuthSettings, BantoError> {
    auth_config_apply_body(&state, disabled, &disabled_role).await
}

/// Enable desktop autologin for `username` (spec M11): verifies the
/// credentials against the same `UsersService` a normal login would (so a
/// caller cannot register autologin for an account/password it does not
/// actually know), stores the password in the OS keyring (never in the
/// settings DB - see `keyring_store`), and flips the setting on. `admin`-only,
/// same floor as every other server/settings-mutating command.
/// Body of [`autologin_enable`] (spec M14 pattern) so its authz + keyring +
/// audit behavior is testable with a plain `&AppState` (the test installs an
/// in-memory keyring so `keyring_store::set_password` does not hit the OS
/// store on a headless runner).
async fn autologin_enable_body(
    state: &AppState,
    username: &str,
    password: &str,
) -> Result<(), BantoError> {
    let actor = require_role(state, Role::Admin, "settings").await?;

    if state.users.verify(username, password).await?.is_none() {
        return Err(BantoError::Validation {
            field_errors: vec![FieldError {
                field: "password".to_string(),
                message: "ユーザー名またはパスワードが違います".to_string(),
            }],
        });
    }

    keyring_store::set_password(username, password)?;

    // The same settings row as `auth_config_apply`: its read-modify-write
    // must not interleave with an apply's (a lost update of `disabled`).
    let auth_config_guard = state.auth_config_lock.lock().await;
    let mut config = state.settings.auth_config().await?;
    config.autologin_enabled = true;
    config.autologin_username = Some(username.to_string());
    state.settings.set_auth_config(&config).await?;
    drop(auth_config_guard);
    // Spec M14: the target `username` (never the password) is fine to
    // record - it identifies WHICH account autologin now applies to, no
    // different from `users_update`'s `role` detail.
    record_ok(
        &state.audit,
        &actor,
        "settings_change",
        "settings",
        None,
        Some(serde_json::json!({ "autologinEnabled": true, "username": username })),
    )
    .await;
    Ok(())
}

#[tauri::command]
async fn autologin_enable(
    state: State<'_, AppState>,
    username: String,
    password: String,
) -> Result<(), BantoError> {
    autologin_enable_body(&state, &username, &password).await
}

/// Disable desktop autologin (spec M11): removes the stored credential from
/// the OS keyring (best-effort - a keyring delete failure is logged, not
/// propagated, so the setting is still turned off even if the OS store is,
/// say, already gone) and clears the setting.
/// Body of [`autologin_disable`] (spec M14 pattern) so its authz + audit
/// behavior is testable with a plain `&AppState`. No keyring is needed when
/// `autologin_username` is unset (the delete is skipped) and is best-effort
/// regardless.
async fn autologin_disable_body(state: &AppState) -> Result<(), BantoError> {
    let actor = require_role(state, Role::Admin, "settings").await?;

    // See `autologin_enable_body`: same row as `auth_config_apply`.
    let auth_config_guard = state.auth_config_lock.lock().await;
    let mut config = state.settings.auth_config().await?;
    if let Some(username) = config.autologin_username.take() {
        if let Err(err) = keyring_store::delete_password(&username) {
            eprintln!("banto: 自動ログインの資格情報のキーリング削除に失敗しました: {err}");
        }
    }
    config.autologin_enabled = false;
    state.settings.set_auth_config(&config).await?;
    drop(auth_config_guard);
    record_ok(
        &state.audit,
        &actor,
        "settings_change",
        "settings",
        None,
        Some(serde_json::json!({ "autologinEnabled": false })),
    )
    .await;
    Ok(())
}

#[tauri::command]
async fn autologin_disable(state: State<'_, AppState>) -> Result<(), BantoError> {
    autologin_disable_body(&state).await
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
    /// 閲覧公開 (ADR-0012; I2b, 2026-10-04 owner decision): whether LAN
    /// clients may obtain a synthetic `viewer` session without logging in.
    /// Persisted alongside the other server settings and therefore reported
    /// (and applied) here, even though it only ever has an effect on the LAN
    /// surface, not in this window (admin-template v3.0.0 と同じ).
    viewer_public: bool,
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

/// `running` is the embedded server's bound address while it runs. Its URLs
/// then come from that address (`chronogazer_core::listening_urls`, the same
/// as `banto-serve`'s startup log - PR1a オーナーレビュー P3); with no server
/// running they come from the saved `bind`/`port` (the settings screen only
/// offers IP choices for `bind`, so it parses).
fn build_status(
    config: &ServerSettings,
    running: Option<std::net::SocketAddr>,
) -> ServerStatusResult {
    let urls = match running {
        Some(addr) => chronogazer_core::listening_urls(addr),
        None => lan_urls_for_bind(&config.bind, config.port),
    };
    let qr_svgs = urls
        .iter()
        .map(|url| QrSvgEntry {
            url: url.clone(),
            svg: qr_svg_for(url),
        })
        .collect();
    ServerStatusResult {
        enabled: config.enabled,
        running: running.is_some(),
        bind: config.bind.clone(),
        port: config.port,
        viewer_public: config.viewer_public,
        urls,
        qr_svgs,
    }
}

/// Build the full `/api/*` + static-asset router (spec §11.1) and start
/// serving on an already-[`bind_listener`]-ed listener (infallible: the bind -
/// the only step that can fail - happened before, so callers can do work such
/// as saving settings between bind and serve; banto #294 review). Shared by
/// `setup()` (auto-start on launch if LAN access was left enabled) and
/// [`server_apply_body`] (spec §11.4's 「保存して適用」button).
///
/// I2b: admin-template v3.0.0 の `src-tauri/src/lib.rs::start_embedded_server`
/// の形（`BoundServer` を受けて `serve` する）に揃えた。ChronoGazer 固有の差は
/// 渡すサービス（Hub・レジストリ 3 つ・収集）だけ。`async` なのは、`serve` が
/// 中で tokio のタスクを起こすので、`setup()` からも `block_on` の中で呼ぶ
/// ため（admin-template と同じ）。
///
/// Deliberately never names the intermediate `axum::Router` type anywhere -
/// `axum` is not (and does not need to be) a direct dependency of this
/// crate purely to support this one function: Rust only requires a crate to
/// be listed in `[dependencies]` to *spell out* one of its types in source,
/// and the router value here only ever flows through an inferred `let`
/// binding on its way into `BoundServer::serve`.
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
    // #393: 表示グループ（同じ pool）。
    display_groups: DisplayGroupService,
    auth: AuthState,
    events: broadcast::Sender<ServerEvent>,
    bound: BoundServer,
) -> RunningServer {
    // `allow_setup: false` - the Tauri app's first-run setup goes through
    // the `auth_setup` command above (`invoke()`, no network involved), not
    // this REST endpoint. Only `banto-serve` (this repo's Tauri-free dev
    // vehicle) opts into `POST /api/auth/setup` via `BANTO_ALLOW_SETUP=1`.
    //
    // `with_security_headers`（banto #500）は admin-template と同じく**最後（最外）**
    // に掛け、静的 UI・`/api/*` の JSON・SSE のどれにも CSP などが付くようにする。
    let router = with_security_headers(
        api_router(
            users,
            settings,
            audit,
            backup,
            hub,
            plc_connections,
            collection_groups,
            tags,
            collect,
            display_groups,
            auth,
            events,
            false,
        )
        .merge(static_router::<FrontendAssets>()),
    );
    bound.serve(router)
}

/// [`start_embedded_server`] with every service handle taken from `state`
/// (the shared `rest_auth` included, so tokens outlive a restart).
async fn serve_embedded(state: &AppState, bound: BoundServer) -> RunningServer {
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
        display_groups_service(state),
        state.rest_auth.clone(),
        state.events.clone(),
        bound,
    )
    .await
}

/// Whether the LAN server may be auto-started on launch from the saved
/// settings (I2b; admin-template v3.0.0 の `setup()` の判定を純関数に出した
/// もの): LAN が有効で、認証モードとの組み合わせが banto の保存時の判定
/// （[`auth_server_combination_allowed`]、banto #288）でも許されるとき。
/// 「ログイン不要モード + LAN」は閲覧公開が ON のときだけ通る。手で書き換えた
/// DB などで許されない組み合わせが残っていたら、ログイン無しの LAN サーバーを
/// 信用して開けずに起動を見送る（どちらの設定も書き換えない）。
fn lan_autostart_allowed(auth: &AuthSettings, server: &ServerSettings) -> bool {
    server.enabled
        && auth_server_combination_allowed(auth.disabled, server.enabled, server.viewer_public)
}

/// `GET`-ish command: current persisted settings + live running state (spec
/// §11.4's status line). `admin`-only (spec M10: "サーバ制御系 = admin").
#[tauri::command]
async fn server_status(state: State<'_, AppState>) -> Result<ServerStatusResult, BantoError> {
    require_role(&state, Role::Admin, "settings").await?;
    let config = state.settings.server_config().await?;
    let running = state
        .server
        .lock()
        .await
        .as_ref()
        .map(|server| server.local_addr());
    Ok(build_status(&config, running))
}

/// Apply new LAN settings (spec §11.4's 「保存して適用」button). I2b: the
/// order, rollback and audit of admin-template v3.0.0's `server_apply`
/// (banto v2.1.0 #287 / #288 / #294) - before this, ChronoGazer saved first
/// and then stopped/started, so a port in use left the new values saved
/// while the old server was already gone.
///
/// `viewer_public` (閲覧公開, ADR-0012; 2026-10-04 owner decision to use it
/// in ChronoGazer too) is persisted like the other three fields. Whether LAN
/// clients may mint a synthetic `viewer` session is decided per request by
/// `POST /api/auth/grant/publicViewer`, whose condition re-reads the setting,
/// so it needs no restart. The auth-disabled/LAN exclusivity it relaxes is
/// validated in the service layer (`SettingsService::set_server_config`), so
/// an illegal combination is refused (`validate_server_config`) before
/// anything is stopped or started.
///
/// Order and failure contract (banto #287, #294 review): validate -> stop the
/// old server -> `bind` the new listener (a port in use surfaces here,
/// nothing saved yet) -> save the settings (one transaction) -> serve. The new
/// listener answers no request until the save is done, so nothing can observe
/// the new listener together with the old stored settings (e.g. mint a public
/// viewer token under a stale `viewer_public`). Serving a bound listener
/// cannot fail, so there is no state in which the settings are saved but the
/// server did not start. If the bind or the save fails, the listener is
/// dropped, nothing is saved, the previously running server is restarted from
/// the unchanged saved settings, and the command returns the error - so the
/// saved values and the live server never silently diverge. `enabled = false`
/// has no new listener (stop -> save). The attempt is audited as
/// `settings_change` / `result: "failed"` (`detail.saved: false`); a success
/// records the usual `ok` entry. The frontend re-reads `server_status` after
/// an error.
///
/// Lock order (#294 review): `state.server` -> `auth_config_lock` (see
/// `AppState::auth_config_lock`). The final validation + save + token
/// revocation run under `auth_config_lock` ([`save_server_config_locked`]),
/// so they are serialized with `auth_config_apply` and the other `auth.*`
/// writers; the lock is NOT held while the old server stops or while binding.
///
/// When the saved `viewer_public` is OFF after a successful apply, every
/// outstanding public viewer token is revoked (`rest_auth` is shared across
/// restarts, so they would otherwise outlive the setting); real login
/// sessions are untouched.
#[tauri::command]
async fn server_apply(
    state: State<'_, AppState>,
    enabled: bool,
    bind: String,
    port: u16,
    viewer_public: bool,
) -> Result<ServerStatusResult, BantoError> {
    server_apply_body(&state, enabled, bind, port, viewer_public).await
}

/// Body of [`server_apply`] over a plain `&AppState`, so the whole apply -
/// authorization, the bind -> save -> serve order, the rollback and the
/// audit - is testable without a Tauri `State` (admin-template v3.0.0 の
/// `server_apply_body` の写し。差は `build_status` に渡す稼働中のアドレス
/// （PR1a オーナーレビュー P3）と、サービスの組み立て（[`serve_embedded`]）
/// だけ）。
async fn server_apply_body(
    state: &AppState,
    enabled: bool,
    bind: String,
    port: u16,
    viewer_public: bool,
) -> Result<ServerStatusResult, BantoError> {
    let actor = require_role(state, Role::Admin, "settings").await?;
    let config = ServerSettings {
        enabled,
        bind,
        port,
        viewer_public,
    };

    // Held for the WHOLE apply: concurrent applies are serialized, and
    // `server_status` cannot observe the stopped-but-not-yet-restarted gap.
    let mut slot = state.server.lock().await;

    // The saved config is the one the rollback below restores, and what the
    // running listener (if any) was started from.
    let previous = state.settings.server_config().await?;
    // Refuse an illegal auth/LAN combination BEFORE anything is stopped.
    state.settings.validate_server_config(&config).await?;

    let was_running = slot.take();
    let had_running = was_running.is_some();
    if let Some(running) = was_running {
        running.stop().await;
    }

    // banto #287 / #294 review: bind -> save -> serve. The new settings are
    // saved only AFTER the new listener is bound (so a port in use fails
    // here, before anything is stored), and the listener only starts
    // answering requests AFTER the save. Without the last part, a request
    // (e.g. `POST /api/auth/grant/publicViewer`, which reads the SAVED
    // `viewer_public`) could hit the new listener while the old values were
    // still stored.
    let outcome: Result<Option<RunningServer>, BantoError> = async {
        let bound = if config.enabled {
            Some(
                bind_listener(ServerConfig {
                    bind: config.bind.clone(),
                    port: config.port,
                })
                .await?,
            )
        } else {
            None
        };
        // On error `bound` is dropped here, releasing the port.
        save_server_config_locked(state, &config).await?;
        Ok(match bound {
            Some(bound) => Some(serve_embedded(state, bound).await),
            None => None,
        })
    }
    .await;

    match outcome {
        Ok(started) => {
            let running = started.as_ref().map(|server| server.local_addr());
            *slot = started;
            record_ok(
                &state.audit,
                &actor,
                "settings_change",
                "settings",
                None,
                Some(serde_json::json!({
                    "serverEnabled": config.enabled,
                    "bind": config.bind,
                    "port": config.port,
                    "viewerPublic": config.viewer_public,
                })),
            )
            .await;
            Ok(build_status(&config, running))
        }
        Err(err) => {
            // Nothing was saved. Bring back the server that was running so
            // the live state matches the (unchanged) saved settings again.
            let mut restored = !had_running;
            if had_running {
                match bind_listener(ServerConfig {
                    bind: previous.bind.clone(),
                    port: previous.port,
                })
                .await
                {
                    Ok(bound) => {
                        *slot = Some(serve_embedded(state, bound).await);
                        restored = true;
                    }
                    Err(restore_err) => eprintln!(
                        "banto: LAN設定の適用に失敗し、旧設定でのサーバー再起動にも失敗しました: {restore_err}"
                    ),
                }
            }
            // The failed attempt is audited too (an applied change and a
            // refused/failed one both leave a record), with the same
            // `resource`/`action` as the success entry.
            let detail = serde_json::json!({
                "serverEnabled": config.enabled,
                "bind": config.bind,
                "port": config.port,
                "viewerPublic": config.viewer_public,
                "saved": false,
                "restoredPrevious": restored,
                "error": err.to_string(),
            });
            state
                .audit
                .record(AuditEntry {
                    actor_username: Some(&actor.username),
                    actor_role: Some(actor.role.as_str()),
                    action: "settings_change",
                    resource: "settings",
                    entity_id: None,
                    detail: Some(detail),
                    origin: "tauri",
                    result: "failed",
                })
                .await;
            Err(err)
        }
    }
}

/// Re-validate (inside `set_server_config`, which re-reads `auth.disabled`),
/// save the `server.*` settings and revoke public viewer tokens, all under
/// `auth_config_lock` so no `auth.*` write can interleave between the check
/// and the save (#294 review). Caller holds `state.server` (order: server ->
/// auth_config_lock). admin-template v3.0.0 の同名関数の写し。
async fn save_server_config_locked(
    state: &AppState,
    config: &ServerSettings,
) -> Result<(), BantoError> {
    let _auth_config = state.auth_config_lock.lock().await;
    state.settings.set_server_config(config).await?;
    if !config.viewer_public {
        // 閲覧公開 OFF: viewer-public grant tokens minted earlier (rest_auth
        // outlives server restarts) must not keep working. Order matters
        // (ADR-0017 §2): the closed condition is persisted above FIRST, then
        // the kind is revoked - an issuance that judged `true` before the
        // save is refused by the generation this advances.
        state
            .rest_auth
            .revoke_grant_tokens(&GrantKind::public_viewer());
    }
    Ok(())
}

/// `admin`-only (banto v2.0.0 移行で admin-template に揃えた): the settings
/// table holds privileged values (the embedded server's bind/port,
/// `auth.disabled`/`auth.disabled_role`, `auth.autologin.username`, audit
/// retention, the Hub connection, and every user's `ui.{username}.*`
/// namespace), so reading an arbitrary key is as privileged as writing one.
/// Without this guard an unauthenticated webview (the login screen is a real
/// webview whose console can `invoke`), or a kiosk's `viewer` synthetic
/// session, could read any of those. The frontend does not call this raw
/// command (UI settings go through the Viewer-gated, caller-scoped
/// `ui_settings_get`).
#[tauri::command]
async fn settings_get(
    state: State<'_, AppState>,
    key: String,
) -> Result<Option<String>, BantoError> {
    settings_get_body(&state, &key).await
}

/// Body of [`settings_get`] (testable with a plain `&AppState`).
async fn settings_get_body(state: &AppState, key: &str) -> Result<Option<String>, BantoError> {
    require_role(state, Role::Admin, "settings").await?;
    state.settings.get(key).await
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
    settings_set_body(&state, &actor, key, value).await
}

/// 組み込みサーバーの設定キー（`server.enabled`・`server.bind`・`server.port`・
/// `server.viewer_public`）か。banto の `SettingsService` には該当の判定関数が
/// 無い（キーの定数は非公開）ので、`auth.` の `is_auth_key` と同じく接頭辞で
/// 判定する。[`settings_set_body`] が拒否し、変更は [`server_apply`] だけが行う。
fn is_server_key(key: &str) -> bool {
    key.starts_with("server.")
}

/// Body of [`settings_set`]. banto v2.0.0 移行（freshness audit of banto
/// #266, P2-2, S-103 - admin-template の同名関数の写し）: keys in the
/// `auth.` namespace are refused - they are written only by
/// [`auth_config_apply`] (which re-binds the session under
/// `auth_config_lock`, keeping `auth.disabled == true <=> AuthDisabledLocal`)
/// and the `autologin_*` commands. A raw write would bypass both.
/// ChronoGazer 固有のキー（`data.dir`・`retention.days` など）はこれまでどおり
/// 書ける。
async fn settings_set_body(
    state: &AppState,
    actor: &UserIdentity,
    key: String,
    value: String,
) -> Result<(), BantoError> {
    if SettingsService::is_auth_key(&key) {
        return Err(BantoError::BadRequest(format!(
            "設定 {key} はこのコマンドでは変更できません。認証モードは auth_config_apply（自動ログインは autologin_enable/autologin_disable）で変更してください"
        )));
    }
    // I2b: `server.*`（LAN の有効化・バインド・ポート・閲覧公開）は
    // `server_apply` だけが変える。ここから書くと、認証モードとの組み合わせ検証
    // （`auth_server_combination_allowed`）と、閲覧公開 OFF の保存後の閲覧者
    // トークンの失効を通らず、保存と実際の状態がずれる（PR #499 レビュー）。
    // `auth.*` の拒否と同じ形。
    if is_server_key(&key) {
        return Err(BantoError::BadRequest(format!(
            "設定 {key} はこのコマンドでは変更できません。LAN 設定は接続の画面（server_apply）から変更してください"
        )));
    }
    state.settings.set(&key, &value).await?;
    // Spec M14: only the KEY is recorded, never the value - this is a
    // generic key/value store and the value could be anything, including
    // something sensitive a future setting might store here.
    record_ok(
        &state.audit,
        actor,
        "settings_change",
        "settings",
        None,
        Some(serde_json::json!({ "key": key })),
    )
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
    users_update_body(&state, id, &display_name, role).await
}

/// Body of [`users_update`] (a plain `&AppState` so the banto v1.7.0 #204
/// session-revocation tests can drive the Tauri path end to end - same
/// split as [`change_own_password`]).
async fn users_update_body(
    state: &AppState,
    id: i64,
    display_name: &str,
    role: Role,
) -> Result<UserSummary, BantoError> {
    let actor = require_role(state, Role::Admin, "users").await?;
    let updated = state.users.update_user(id, display_name, role).await?;
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
    users_reset_password_body(&state, id, &new_password).await
}

/// Body of [`users_reset_password`] (see [`users_update_body`]).
async fn users_reset_password_body(
    state: &AppState,
    id: i64,
    new_password: &str,
) -> Result<(), BantoError> {
    let actor = require_role(state, Role::Admin, "users").await?;
    state.users.reset_password(id, new_password).await?;
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
    users_delete_body(&state, id).await
}

/// Body of [`users_delete`] (see [`users_update_body`]).
async fn users_delete_body(state: &AppState, id: i64) -> Result<(), BantoError> {
    let acting = require_role(state, Role::Admin, "users").await?;
    state.users.delete_user(id, Some(acting.id)).await?;
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

/// Body of [`plc_connections_create`]（spec M14 split-function pattern -
/// `State` を組み立てずにテストから呼ぶため、#413）。
async fn plc_connections_create_body(
    state: &AppState,
    input: PlcConnectionPayload,
) -> Result<PlcConnectionResponse, BantoError> {
    let actor = require_role(state, Role::Editor, "plc_connections").await?;
    reject_disallowed_connection_protocol(&input.protocol)?;
    let created = state
        .plc_connections
        .create(input.into_create_input())
        .await?;
    state
        .audit
        .record(AuditEntry {
            actor_username: Some(&actor.username),
            actor_role: Some(actor.role.as_str()),
            action: "create",
            resource: "plc_connections",
            entity_id: Some(&created.id.to_string()),
            // REST と同じ helper（#413: `simulation` の切替も残す）。
            detail: Some(plc_connection_audit_detail(&created)),
            origin: "tauri",
            result: "ok",
        })
        .await;
    Ok(PlcConnectionResponse::from(created))
}

/// `editor`+ (R0 §3.6): create a PLC connection. `input.simulation`
/// （#413、作成での省略は `false`。更新での省略は既存の値を保つ -
/// `update_plc_connection`）は `chronogazer_core::rest::PlcConnectionPayload`
/// の doc のとおり REST と同じ扱い。
#[tauri::command]
async fn plc_connections_create(
    state: State<'_, AppState>,
    input: PlcConnectionPayload,
) -> Result<PlcConnectionResponse, BantoError> {
    plc_connections_create_body(&state, input).await
}

/// Body of [`plc_connections_update`]（#413、同上）。
async fn plc_connections_update_body(
    state: &AppState,
    id: i64,
    input: PlcConnectionPayload,
) -> Result<PlcConnectionResponse, BantoError> {
    let actor = require_role(state, Role::Editor, "plc_connections").await?;
    reject_disallowed_connection_protocol(&input.protocol)?;
    // #414 段階1: REST の `plc_connections_update` と同じ検査（両経路対称）。
    // 共通の更新（`update_plc_connection`）の**前に**呼ぶ。
    ensure_protocol_change_keeps_tags_readable(
        &state.plc_connections,
        &state.collection_groups,
        &state.tags,
        id,
        &input.protocol,
    )
    .await?;
    // #417 監査 P2: `simulation` の省略は既存行の値を保つ（REST と同じ関数）。
    let updated = update_plc_connection(&state.plc_connections, id, input).await?;
    state
        .audit
        .record(AuditEntry {
            actor_username: Some(&actor.username),
            actor_role: Some(actor.role.as_str()),
            action: "update",
            resource: "plc_connections",
            entity_id: Some(&id.to_string()),
            detail: Some(plc_connection_audit_detail(&updated)),
            origin: "tauri",
            result: "ok",
        })
        .await;
    Ok(PlcConnectionResponse::from(updated))
}

/// `editor`+ (R0 §3.6): update a PLC connection. `simulation` の切替は
/// 走っている収集には「収集を再起動」まで反映されない（C-2 の決定）。
#[tauri::command]
async fn plc_connections_update(
    state: State<'_, AppState>,
    id: i64,
    input: PlcConnectionPayload,
) -> Result<PlcConnectionResponse, BantoError> {
    plc_connections_update_body(&state, id, input).await
}

/// Body of [`simulation_coverage_list`]（#413）。
async fn simulation_coverage_list_body(
    state: &AppState,
) -> Result<Vec<SimulationCoverageEntry>, BantoError> {
    require_role(state, Role::Viewer, "tags").await?;
    simulation_coverage(
        &state.plc_connections,
        &state.collection_groups,
        &state.tags,
    )
    .await
}

/// `viewer`+（#413）: シミュレーション接続の配下のタグごとに、シミュレータが
/// 値を動かす番地かどうか。REST の `GET /api/simulation-coverage` と同じ
/// `chronogazer_core::simulation::simulation_coverage` を呼ぶ（判定は
/// `banto_collect::simulation::classify_plc_tag` の 1 か所）。読み取りなので
/// 監査しない。
#[tauri::command]
async fn simulation_coverage_list(
    state: State<'_, AppState>,
) -> Result<Vec<SimulationCoverageEntry>, BantoError> {
    simulation_coverage_list_body(&state).await
}

/// Body of [`config_exclusions_list`]（#414 段階2）。
async fn config_exclusions_list_body(state: &AppState) -> Result<Vec<ExclusionView>, BantoError> {
    require_role(state, Role::Viewer, "tags").await?;
    registry_exclusions(
        &state.plc_connections,
        &state.collection_groups,
        &state.tags,
    )
    .await
}

/// `viewer`+（#414 段階2）: 今のレジストリで収集を開始したら外される接続・
/// グループ・タグ（`/tags` の印）。REST の `GET /api/config-exclusions` と
/// 同じ `chronogazer_core::collect::registry_exclusions` を呼ぶ（判定は
/// 収集の開始と同じ `banto_collect::build_config_lenient_from`）。読み取り
/// なので監査しない。
#[tauri::command]
async fn config_exclusions_list(
    state: State<'_, AppState>,
) -> Result<Vec<ExclusionView>, BantoError> {
    config_exclusions_list_body(&state).await
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
    collection_groups_update_body(&state, id, input).await
}

/// Body of [`collection_groups_update`]（spec M14 split-function pattern -
/// #414 段階1 のテストがコマンドの本体を直接呼ぶため）。
async fn collection_groups_update_body(
    state: &AppState,
    id: i64,
    input: CollectionGroupPayload,
) -> Result<CollectionGroup, BantoError> {
    let actor = require_role(state, Role::Editor, "collection_groups").await?;
    // #414 段階1: REST の `collection_groups_update` と同じ検査（両経路対称）。
    ensure_group_move_keeps_tags_readable(
        &state.plc_connections,
        &state.collection_groups,
        &state.tags,
        id,
        input.plc_connection_id,
    )
    .await?;
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
    tags_create_body(&state, input).await
}

/// Body of [`tags_create`]（spec M14 split-function pattern - #414 段階1 の
/// テストがコマンドの本体を直接呼ぶため）。
async fn tags_create_body(state: &AppState, input: TagPayload) -> Result<Tag, BantoError> {
    let actor = require_role(state, Role::Editor, "tags").await?;
    // #414 段階1: REST の `tags_create` と同じ検査（両経路対称）。
    ensure_tag_fits_its_connection(
        &state.plc_connections,
        &state.collection_groups,
        input.collection_group_id,
        &input.address,
        &input.data_type,
    )
    .await?;
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
    tags_update_body(&state, id, input).await
}

/// Body of [`tags_update`]（spec M14 split-function pattern - #414 段階1 の
/// テストがコマンドの本体を直接呼ぶため）。
async fn tags_update_body(state: &AppState, id: i64, input: TagPayload) -> Result<Tag, BantoError> {
    let actor = require_role(state, Role::Editor, "tags").await?;
    // #414 段階1: REST の `tags_update` と同じ検査（両経路対称）。所属
    // グループを変える更新も、入力のグループから接続を辿るので同じ検査で拾う。
    // 読む場所を変えずに無効化するだけの更新は検査しない（#418 P2）。
    ensure_tag_update_fits_its_connection(
        &state.plc_connections,
        &state.collection_groups,
        &state.tags,
        id,
        TagPlacement {
            collection_group_id: input.collection_group_id,
            address: &input.address,
            data_type: &input.data_type,
            enabled: input.enabled,
        },
    )
    .await?;
    let updated = update_tag_checked(&state.tags, id, input.into()).await?;
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
    tags_delete_body(&state, id).await
}

/// Body of [`tags_delete`]（#393 のテストがコマンドの本体を直接呼ぶため）。
async fn tags_delete_body(state: &AppState, id: i64) -> Result<(), BantoError> {
    let actor = require_role(state, Role::Editor, "tags").await?;
    // #393（§3.7.9 の 8）: REST の `tags_delete` と同じく、表示グループの
    // ペンに割り当てられたタグは参照元を並べて削除を拒否する。
    display_groups_service(state)
        .delete_tag_unless_referenced(&state.tags, id)
        .await?;
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

// --- #393: 表示グループ ------------------------------------------------------

/// 表示グループのサービス。`AppState` の pool から都度組む（`Clone` の pool
/// ハンドルを包むだけなので軽い。`AppState` にフィールドを足さないのは、
/// テストの `AppState` の組み立てを増やさないため）。
fn display_groups_service(state: &AppState) -> DisplayGroupService {
    DisplayGroupService::new(state.pool.clone())
}

/// 書き込みの監査（REST の `record_write` と同じ中身、`origin: "tauri"`）。
async fn record_display_group_write(
    state: &AppState,
    actor: &UserIdentity,
    action: &str,
    entity_id: Option<&str>,
    detail: Option<serde_json::Value>,
) {
    record_ok(
        &state.audit,
        actor,
        action,
        DISPLAY_GROUPS_RESOURCE,
        entity_id,
        detail,
    )
    .await;
}

/// `viewer`+ (R0 §3.6): 表示グループの一覧（並び順）。
#[tauri::command]
async fn display_groups_list(state: State<'_, AppState>) -> Result<Vec<DisplayGroup>, BantoError> {
    display_groups_list_body(&state).await
}

async fn display_groups_list_body(state: &AppState) -> Result<Vec<DisplayGroup>, BantoError> {
    require_role(state, Role::Viewer, DISPLAY_GROUPS_RESOURCE).await?;
    display_groups_service(state).list().await
}

/// `viewer`+ (R0 §3.6): 表示グループを 1 つ。
#[tauri::command]
async fn display_groups_get(
    state: State<'_, AppState>,
    id: i64,
) -> Result<DisplayGroup, BantoError> {
    require_role(&state, Role::Viewer, DISPLAY_GROUPS_RESOURCE).await?;
    display_groups_service(&state).get(id).await
}

/// `editor`+ (R0 §3.6): 表示グループの作成。検証は REST と同じ
/// `validate_display_group`（サービスの中）。
#[tauri::command]
async fn display_groups_create(
    state: State<'_, AppState>,
    input: DisplayGroupPayload,
) -> Result<DisplayGroup, BantoError> {
    display_groups_create_body(&state, input).await
}

async fn display_groups_create_body(
    state: &AppState,
    input: DisplayGroupPayload,
) -> Result<DisplayGroup, BantoError> {
    let actor = require_role(state, Role::Editor, DISPLAY_GROUPS_RESOURCE).await?;
    let created = display_groups_service(state).create(&input).await?;
    record_display_group_write(
        state,
        &actor,
        "create",
        Some(&created.id.to_string()),
        Some(display_group_audit_detail(&created)),
    )
    .await;
    Ok(created)
}

/// `editor`+ (R0 §3.6): 表示グループの更新。版の食い違いは REST の `409` と
/// 同じ本文の検証エラー（`field_errors` の `expectedRevision`）。
#[tauri::command]
async fn display_groups_update(
    state: State<'_, AppState>,
    id: i64,
    input: DisplayGroupPayload,
) -> Result<DisplayGroup, BantoError> {
    display_groups_update_body(&state, id, input).await
}

async fn display_groups_update_body(
    state: &AppState,
    id: i64,
    input: DisplayGroupPayload,
) -> Result<DisplayGroup, BantoError> {
    let actor = require_role(state, Role::Editor, DISPLAY_GROUPS_RESOURCE).await?;
    let updated = display_groups_service(state).update(id, &input).await?;
    record_display_group_write(
        state,
        &actor,
        "update",
        Some(&id.to_string()),
        Some(display_group_audit_detail(&updated)),
    )
    .await;
    Ok(updated)
}

/// `editor`+ (R0 §3.6): 表示グループの削除。
#[tauri::command]
async fn display_groups_delete(state: State<'_, AppState>, id: i64) -> Result<(), BantoError> {
    display_groups_delete_body(&state, id).await
}

async fn display_groups_delete_body(state: &AppState, id: i64) -> Result<(), BantoError> {
    let actor = require_role(state, Role::Editor, DISPLAY_GROUPS_RESOURCE).await?;
    let deleted = display_groups_service(state).delete(id).await?;
    record_display_group_write(
        state,
        &actor,
        "delete",
        Some(&id.to_string()),
        Some(serde_json::json!({ "name": deleted.name })),
    )
    .await;
    Ok(())
}

/// `editor`+ (R0 §3.6): 表示グループの並べ替え（`ids` は全グループを新しい
/// 順で 1 回ずつ）。
#[tauri::command]
async fn display_groups_reorder(
    state: State<'_, AppState>,
    ids: Vec<i64>,
) -> Result<Vec<DisplayGroup>, BantoError> {
    display_groups_reorder_body(&state, ids).await
}

async fn display_groups_reorder_body(
    state: &AppState,
    ids: Vec<i64>,
) -> Result<Vec<DisplayGroup>, BantoError> {
    let actor = require_role(state, Role::Editor, DISPLAY_GROUPS_RESOURCE).await?;
    let groups = display_groups_service(state).reorder(&ids).await?;
    record_display_group_write(
        state,
        &actor,
        "reorder",
        None,
        Some(serde_json::json!({ "ids": ids })),
    )
    .await;
    Ok(groups)
}

/// Body of [`audit_log_list`], split out so the `asOfId`-gated prune
/// (#463) is testable with a plain `&AppState` in this crate's own
/// `cargo test` - same reasoning as [`change_own_password`] (`tauri::State`
/// cannot be constructed outside a running tauri app, but it derefs to
/// `&AppState`, so the command below is a one-line adapter).
///
/// `admin`-only (spec M14): the audit-log viewer's filtered/sorted/
/// paginated read. Also opportunistically prunes first - same reasoning as
/// `chronogazer_core::rest::audit_log_list` (see that function's doc
/// comment).
///
/// `asOfId`（任意、#410）はスナップショット境界 - REST の
/// `POST /api/audit-log/list?asOfId=` と同じ意味（
/// `chronogazer_core::audit::AuditLogService::list_as_of` の doc）。省略すると
/// 従来どおり全行が対象。床（`admin`）は変えていない。
///
/// **`asOfId` 付きの取得（世代の 2 ブロック目以降）では剪定しない**（#463、
/// `chronogazer_core::rest::audit_log_list` と同じ理由）。剪定は `asOfId`
/// なしの取得（世代の最初・「再読み込み」）と、起動時の剪定に任せる（ChronoGazer
/// に監査ログ専用の周期タスクは無い）。
async fn audit_log_list_body(
    state: &AppState,
    params: ListParams,
    as_of_id: Option<i64>,
) -> Result<AuditLogList, BantoError> {
    require_role(state, Role::Admin, "audit_log").await?;
    if as_of_id.is_none() {
        if let Ok(config) = state.settings.audit_config().await {
            let _ = state
                .audit
                .prune(config.retention_days, config.retention_rows)
                .await;
        }
    }
    state.audit.list_as_of(params, as_of_id).await
}

/// See [`audit_log_list_body`] for the actual logic and the doc comment
/// (this is a one-line adapter so that body is unit-testable outside tauri).
#[tauri::command]
async fn audit_log_list(
    state: State<'_, AppState>,
    params: ListParams,
    as_of_id: Option<i64>,
) -> Result<AuditLogList, BantoError> {
    audit_log_list_body(&state, params, as_of_id).await
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
/// resolved `backups/<DB file name>/` directory (banto #280); `opened`
/// tells the frontend whether an actual file-explorer window was launched,
/// so it can show a fallback
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
) -> Result<Readout<HashMap<String, ConnectionView>>, BantoError> {
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
) -> Result<Readout<HashMap<String, ConnectionView>>, BantoError> {
    collect_connections_body(&state).await
}

/// Body of [`collect_events_list`]（spec M14 split-function pattern）。
async fn collect_events_list_body(
    state: &AppState,
    offset: Option<u64>,
    limit: Option<u64>,
    as_of_id: Option<i64>,
) -> Result<Readout<CollectEventList>, BantoError> {
    require_collect_reader(state).await?;
    Ok(state
        .collect
        .events(EventPage::new(offset, limit).as_of(as_of_id))
        .await)
}

/// `GET`-ish command: `collect_events` の 1 ページ（**新しい順**）。
/// **`viewer` 以上**。
///
/// 総件数は `totalCount` で返す - [`audit_log_list`]（監査ログ
/// 一覧）と同じ綴りで、既定の取得件数も同じ 50
/// （docs/r1-plan.md の R1-C「`collect_events` のイベント一覧ページ
/// （banto 監査ログページの流儀）」）。
///
/// **`asOfId` は任意のスナップショット境界**（#409 レビュー P2-2。
/// `chronogazer_core::collect::EventPage` の doc）: 件数も行も
/// `id <= asOfId` で絞る。省略するとその時点の最大 `id` が境界になり、
/// 使った境界が応答の `asOfId` に入る。画面は 1 つの世代の最初の応答で
/// これを固定し、同じ世代の後続ブロックすべてに渡す - そうしないと、
/// ブロック取得の合間に足されたイベントで `offset` がずれ、境界で行が
/// 重複し末尾が漏れる。
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
    as_of_id: Option<i64>,
) -> Result<Readout<CollectEventList>, BantoError> {
    collect_events_list_body(&state, offset, limit, as_of_id).await
}

/// Body of [`collect_history`]（spec M14 split-function pattern）。
/// 床を先に見る（未認証・権限不足なら検証の誤りより先に拒否する - REST の
/// `RoleGuard` が先に効くのと同じ順）。
async fn collect_history_body(
    state: &AppState,
    tag_ids: Vec<i64>,
    from_ms: i64,
    to_ms: i64,
    bins: u64,
) -> Result<Readout<CollectHistory>, BantoError> {
    require_collect_reader(state).await?;
    let request = validate_history_request(&tag_ids, from_ms, to_ms, bins)?;
    Ok(state.collect.history(&request).await)
}

/// `GET`-ish command: タグごとの直近の履歴（R1-D の D-3a。間引いた最小・
/// 最大）。**`viewer` 以上**、監査しない。`chronogazer_core::rest` の
/// `GET /api/collect/history` と**同じ検証・同じメソッド**を通る双子。
///
/// **収集が止まっていても読める**（`notRunning` を返さない）。読めなかった
/// ときは `unavailable` で、理由（ファイルのパスを含みうる）は返さない。
/// 収集の操作キューは通らない。上限（タグ 8 本・期間 1 時間・`bins`）と
/// 割り切り（タグを別グループへ移す前の区間は欠測、など）は
/// `chronogazer_core::collect` の `history` モジュールの doc。
#[tauri::command]
async fn collect_history(
    state: State<'_, AppState>,
    tag_ids: Vec<i64>,
    from_ms: i64,
    to_ms: i64,
    bins: u64,
) -> Result<Readout<CollectHistory>, BantoError> {
    collect_history_body(&state, tag_ids, from_ms, to_ms, bins).await
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

/// tauri.conf.json の `app.windows` のうち label が `main` の窓を、その設定の
/// まま作る。設定側は `create: false`（Tauri が setup の前に自動で作らない）。
fn create_main_window(app: &tauri::App) -> tauri::Result<()> {
    let config = app
        .config()
        .app
        .windows
        .iter()
        .find(|window| window.label == "main")
        .cloned()
        .expect("tauri.conf.json defines the main window");
    tauri::WebviewWindowBuilder::from_config(app.handle(), &config)?.build()?;
    Ok(())
}

/// 旧形式の DB を拒否したときの窓の label。capabilities（`main` だけ）の
/// 対象外なので、この窓からはコマンドを呼べない。
const LEGACY_DB_WINDOW_LABEL: &str = "legacy-db";

/// 旧形式の DB を拒否したときに出す文（`static/legacy-db.html` が表示する）。
#[derive(Debug, Serialize, PartialEq, Eq)]
struct LegacyDbNotice {
    title: String,
    message: String,
    path: String,
    steps: String,
    closing: String,
}

impl LegacyDbNotice {
    fn new(legacy: &chronogazer_core::db::LegacyDatabase) -> Self {
        Self {
            title: "ChronoGazer を起動できません".to_string(),
            message: legacy.to_string(),
            path: legacy.path.display().to_string(),
            steps: concat!(
                "ChronoGazer を終了してから、上のファイルを（同じ場所に「-wal」「-shm」が",
                "付いたファイルがあれば一緒に）削除するか別の名前へ退避し、起動し直して",
                "ください。新しい DB はアカウントも接続・タグも空なので、初回セットアップ",
                "からやり直します。"
            )
            .to_string(),
            closing: "このウィンドウを閉じると ChronoGazer は終了します。".to_string(),
        }
    }

    /// ページを読む前に走らせる JS（`window.__CHRONOGAZER_LEGACY_DB__` に入れる）。
    /// JSON はそのまま JS の式として有効で、ページ側は `textContent` で入れる
    /// ので、パスにどんな文字が入っても HTML として解釈されない。
    fn initialization_script(&self) -> String {
        let json = serde_json::to_string(self).expect("LegacyDbNotice serializes");
        format!("window.__CHRONOGAZER_LEGACY_DB__ = {json};")
    }
}

/// 旧形式の DB を拒否したことを、メインの窓の代わりに小さな窓で見せる。
fn open_legacy_db_window(
    app: &tauri::App,
    legacy: &chronogazer_core::db::LegacyDatabase,
) -> tauri::Result<()> {
    let notice = LegacyDbNotice::new(legacy);
    tauri::WebviewWindowBuilder::new(
        app,
        LEGACY_DB_WINDOW_LABEL,
        tauri::WebviewUrl::App("legacy-db.html".into()),
    )
    .title(notice.title.clone())
    .inner_size(620.0, 360.0)
    .resizable(true)
    .center()
    .initialization_script(notice.initialization_script())
    .build()?;
    Ok(())
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
            //
            // I2a（banto #280）: 予約もバックアップも
            // `<データディレクトリ>/backups/chronogazer.sqlite3/` に置かれる。
            // それより前の共有領域（`backups/` 直下・`restore-pending.sqlite3`）
            // に残ったファイルは、この呼び出しが警告を出すだけで、移しも適用も
            // しない（2026-10-04 オーナー決定: banto の既定どおり）。
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
            //
            // DB スキーマの整理（2026-10-02）: 旧形式の DB は起動を拒否する（自動移行は無い）。
            // panic のメッセージで済ませず、何をすればよいかを出して終了する
            // （`chronogazer_core::db` のモジュール doc「旧形式の DB の拒否」、
            // 手順は apps/chronogazer/README.md）。リリースビルドは
            // `windows_subsystem = "windows"` で stderr が見えないので、メインの
            // 画面の代わりにエラー専用の窓を開く（[`open_legacy_db_window`]）。
            // stderr にも出す（開発ビルド用）。
            let pool = match tauri::async_runtime::block_on(init_db(&db_path)) {
                Ok(pool) => pool,
                Err(InitDbError::Legacy(legacy)) => {
                    eprintln!("banto: {legacy}");
                    if let Err(err) = open_legacy_db_window(app, &legacy) {
                        eprintln!("banto: エラー表示の窓を開けませんでした: {err}");
                        std::process::exit(1);
                    }
                    // `AppState` は manage しない。開くのはコマンドを呼ばない
                    // 静的ページだけで、閉じると最後の窓なのでプロセスが終わる
                    // （`RunEvent::Exit` の後始末は `try_state` で何もしない）。
                    return Ok(());
                }
                Err(err) => panic!("init_db should succeed: {err}"),
            };

            // メインの窓は tauri.conf.json で `create: false` にしてあり、DB を
            // 開けた後にだけここで作る（旧形式の DB のときに空の画面が先に
            // 出ないように）。設定は tauri.conf.json の定義をそのまま使う。
            create_main_window(app)?;

            let events = event_channel();
            let users = UsersService::new(Db::Sqlite(pool.clone()));
            let settings = SettingsService::new(Db::Sqlite(pool.clone()));
            let backup = BackupService::new(db_path.clone(), Db::Sqlite(pool.clone()));
            // #383 段階2a / R1-B: レジストリ3サービス。テーブルは
            // `init_db` が呼ぶ `banto_tags::migrate` で既に作成済み。
            let plc_connections = PlcConnectionService::new(pool.clone());
            let collection_groups = CollectionGroupService::new(pool.clone());
            let tags = TagService::new(pool.clone());
            // #383 段階2b / R1-C: 収集ランタイム。`data.dir` は設定から読み、
            // **相対パスはこのアプリのデータディレクトリ基準**で解決する
            // （既定 `"./data"` をそのまま使うと、プロセスの作業ディレクトリ
            // という当てにならない場所に時系列ファイルを作ってしまう）。
            // `retention.days` は収集では読まない - 古いファイルの削除は
            // `chronogazer_core::retention`（下の spawn。#538）が、掃除のたびに
            // 設定から読み直して行う。
            //
            // 開始は下の `autostart` まで待つ（`AppState` の他の材料が
            // 揃ってから、ランタイムの上で spawn したいため）。
            let store_settings = tauri::async_runtime::block_on(store_config(&settings))
                .expect("store_config should succeed");
            let collect_data_dir = resolve_data_dir(&data_dir, &store_settings.data_dir);
            let collect = CollectorService::new(pool.clone(), collect_data_dir.clone());
            let audit = AuditLogService::new(Db::Sqlite(pool.clone()));
            // Records `login`/`login_failed` audit entries (spec M14) from
            // inside the verifier itself - see
            // `chronogazer_core::rest::audited_credential_verifier`'s doc
            // comment. This is the embedded LAN server's OWN session
            // (`origin: "rest"`) - the webview's session goes through
            // `auth_login` below instead. banto v1.7.0 #204: also re-checks
            // the account on every request (`user_auth_state`), so a change
            // made from the webview's commands ends LAN sessions too.
            let rest_auth = user_auth_state(users.clone(), audit.clone());

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
            let initial_auth: Option<DesktopSession> = if auth_config.disabled {
                // The ONE definition of the synthetic identity and its
                // `login` entry, shared with `auth_config_apply_body` /
                // `logout_body` (banto v2.0.0 移行): `local_identity` /
                // `record_local_login`.
                let local_identity = local_identity(auth_config.disabled_role);
                tauri::async_runtime::block_on(record_local_login(&audit, &local_identity));
                Some(DesktopSession::AuthDisabledLocal(local_identity))
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
                                    Some(DesktopSession::Account(identity))
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

            // 保持期間を過ぎた時系列データファイルの削除（#538）。起動時に 1 回、
            // その後は日付が変わるたびに 1 回。失敗してもログに出すだけで収集は
            // 止めない。`data.dir` は収集と同じディレクトリを渡す。
            tauri::async_runtime::spawn(chronogazer_core::retention::run(
                settings.clone(),
                collect_data_dir,
            ));

            // If LAN access was left enabled on a previous run, start the
            // server immediately (spec §11.4) - from here on, the settings
            // screen only needs to *change* state via `server_apply`.
            //
            // Spec M11 exclusivity is enforced at write-time
            // (`SettingsService::set_server_config`/`set_auth_config`), but a
            // hand-edited settings DB could still leave both
            // `auth.disabled` and `server.enabled` set to `true` at once
            // without `server.viewer_public` (the one combination the guards
            // refuse, `auth_server_combination_allowed`, banto #288) - if so,
            // refuse to auto-start the (would-be unauthenticated) LAN server
            // rather than trust a state the app itself would never have
            // written, and leave the inconsistency for the user to resolve
            // from the settings screen (this does NOT rewrite either
            // setting). I2b: the decision is [`lan_autostart_allowed`].
            let server_config = tauri::async_runtime::block_on(settings.server_config())
                .expect("server_config should succeed");
            let autostart = lan_autostart_allowed(&auth_config, &server_config);
            if server_config.enabled && !autostart {
                eprintln!(
                    "banto: 認証無効モードとLANアクセスが閲覧公開なしで同時に有効な不整合な設定を検出したため、LANサーバーの自動起動をスキップしました。設定画面でどちらかを無効にするか、閲覧公開を有効にしてください。"
                );
            }
            let initial_server = if autostart {
                let runtime_config = ServerConfig {
                    bind: server_config.bind.clone(),
                    port: server_config.port,
                };
                match tauri::async_runtime::block_on(bind_listener(runtime_config)) {
                    Ok(bound) => Some(tauri::async_runtime::block_on(start_embedded_server(
                        users.clone(),
                        settings.clone(),
                        audit.clone(),
                        backup.clone(),
                        hub.clone(),
                        plc_connections.clone(),
                        collection_groups.clone(),
                        tags.clone(),
                        collect.clone(),
                        DisplayGroupService::new(pool.clone()),
                        rest_auth.clone(),
                        events.clone(),
                        bound,
                    ))),
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
                auth: Mutex::new(AuthSlot::new(initial_auth)),
                auth_io: AuthIo::production(&users, &settings),
                auth_config_lock: AsyncMutex::new(()),
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
            auth_resolve,
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
            display_groups_list,
            display_groups_get,
            display_groups_create,
            display_groups_update,
            display_groups_delete,
            display_groups_reorder,
            simulation_coverage_list,
            config_exclusions_list,
            collect_status,
            collect_start,
            collect_stop,
            collect_restart,
            collect_values,
            collect_connections,
            collect_events_list,
            collect_history,
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

    /// DB スキーマの整理（2026-10-02）: 旧形式の DB を拒否したときの窓に出す
    /// 文は、DB のパスと対処（削除または退避して起動し直す）を含み、ページへ
    /// 渡す JS は `window.__CHRONOGAZER_LEGACY_DB__ = <JSON>;` の形で、パスに
    /// 引用符や `</script>` が入っても JSON として読み戻せる。
    #[test]
    fn legacy_db_notice_carries_the_path_and_what_to_do() {
        let legacy = chronogazer_core::db::LegacyDatabase {
            path: std::path::PathBuf::from(r#"C:\Users\a "b"\</script>\chronogazer.sqlite3"#),
        };
        let notice = LegacyDbNotice::new(&legacy);
        assert_eq!(notice.path, legacy.path.display().to_string());
        assert!(notice.message.contains("旧形式"), "{}", notice.message);
        assert!(notice.message.contains(&notice.path), "{}", notice.message);
        assert!(
            notice.message.contains("削除または退避"),
            "{}",
            notice.message
        );
        assert!(notice.steps.contains("起動し直して"), "{}", notice.steps);
        assert!(notice.closing.contains("終了"), "{}", notice.closing);

        let script = notice.initialization_script();
        let json = script
            .strip_prefix("window.__CHRONOGAZER_LEGACY_DB__ = ")
            .and_then(|rest| rest.strip_suffix(';'))
            .expect("script shape");
        let parsed: serde_json::Value = serde_json::from_str(json).expect("valid JSON");
        assert_eq!(parsed["path"], notice.path);
        assert_eq!(parsed["message"], notice.message);
        assert_eq!(parsed["steps"], notice.steps);
    }

    /// エラー専用のページは Tauri のコマンドを呼ばない（そのとき `AppState`
    /// は manage されていない）、文は `textContent` で入れる、の 2 点を
    /// ページの中身で固定する。
    #[test]
    fn legacy_db_page_calls_no_commands_and_inserts_text_only() {
        let page = include_str!("../../static/legacy-db.html");
        assert!(page.contains("__CHRONOGAZER_LEGACY_DB__"));
        for forbidden in ["invoke", "__TAURI", "innerHTML"] {
            assert!(!page.contains(forbidden), "page must not use {forbidden}");
        }
    }

    /// メインの窓は tauri.conf.json で自動作成を止め、DB を開けた後に
    /// `create_main_window` で作る（旧形式の DB で空の画面が先に出ないように）。
    #[test]
    fn main_window_is_not_created_by_tauri_before_setup() {
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).expect("tauri.conf.json");
        let windows = conf["app"]["windows"].as_array().expect("windows");
        let main = windows
            .iter()
            .find(|window| window["label"] == "main")
            .expect("main window");
        assert_eq!(main["create"], false);
        assert_eq!(windows.len(), 1);
    }

    /// #505: 窓（WebView）の CSP は、組み込みサーバーの応答の CSP（banto の
    /// `CONTENT_SECURITY_POLICY`、#500）に Tauri の IPC（`connect-src` の
    /// `ipc: http://ipc.localhost`、`TAURI_IPC_CONNECT_SRC`）を足しただけのものに
    /// する（admin-template の `tauri.conf.json` と同じ形）。ChronoGazer の窓は
    /// Tauri のアセットを表示し、組み込みサーバーの画面へは移らないので、
    /// 組み込みサーバーの応答の CSP は厳格なまま（広げない）。banto が応答の CSP を変えたら、ここが落ちて窓の側を
    /// 揃え忘れないようにする。期待値は banto の `SecurityHeaders` で組み立てる
    /// （banto v5.0.0。文字列の置き換えはしない）。
    #[test]
    fn window_csp_is_the_served_csp_plus_tauri_ipc() -> Result<(), banto_server::InvalidCspSource> {
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).expect("tauri.conf.json");
        let window_csp = conf["app"]["security"]["csp"]
            .as_str()
            .expect("app.security.csp is set");

        let expected = banto_server::SecurityHeaders::new()
            .extra_connect_src(banto_server::TAURI_IPC_CONNECT_SRC)?
            .content_security_policy();
        assert_ne!(expected, banto_server::CONTENT_SECURITY_POLICY);
        assert_eq!(window_csp, expected);
        Ok(())
    }
    use std::path::PathBuf;

    impl AppState {
        /// Install `session` as if a session-changing command had written it
        /// (advancing `seq`, banto #260 I-11).
        fn set_session_for_test(&self, session: Option<DesktopSession>) {
            let mut slot = self.auth.lock().expect("auth mutex poisoned");
            slot.session = session;
            slot.seq += 1;
        }
    }

    /// A retry-cleanup temp dir wrapper - see `chronogazer_core`'s
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
        app_state_on(pool).await
    }

    /// [`app_state`] over a given pool. banto v1.7.0 #204: several
    /// `AppState`s on ONE pool stand for several desktop windows/processes
    /// sharing the same `users` table (each with its own webview session and
    /// its own REST token space).
    async fn app_state_on(pool: chronogazer_core::db::DbPool) -> AppState {
        let events = event_channel();
        let settings = SettingsService::new(Db::Sqlite(pool.clone()));
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
            auth: Mutex::new(AuthSlot::default()),
            auth_config_lock: AsyncMutex::new(()),
            auth_io: AuthIo::production(
                &UsersService::new(Db::Sqlite(pool.clone())),
                &SettingsService::new(Db::Sqlite(pool.clone())),
            ),
            users: UsersService::new(Db::Sqlite(pool.clone())),
            settings,
            events,
            // banto v1.7.0 #204: 本番と同じ照合付き（同じ `users`）。REST と
            // Tauri のセッションが同じアカウントの変更で終わることを、
            // 両経路をまたいで確かめるテストが使う。
            rest_auth: chronogazer_core::rest::user_auth_state(
                UsersService::new(Db::Sqlite(pool.clone())),
                AuditLogService::new(Db::Sqlite(pool.clone())),
            ),
            server: AsyncMutex::new(None),
            audit: AuditLogService::new(Db::Sqlite(pool.clone())),
            backup: BackupService::new(
                PathBuf::from("unused-in-tests").join("chronogazer.sqlite3"),
                Db::Sqlite(pool.clone()),
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
    /// `banto_admin_services::backup`'s test module doc comment for the
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
        let settings = SettingsService::new(Db::Sqlite(pool.clone()));
        let hub = HubService::new(
            settings.clone(),
            std::sync::Arc::new(chronogazer_core::hub::UnavailableKeyStore),
        )
        .await
        .expect("HubService::new");
        let state = AppState {
            auth: Mutex::new(AuthSlot::default()),
            auth_config_lock: AsyncMutex::new(()),
            auth_io: AuthIo::production(
                &UsersService::new(Db::Sqlite(pool.clone())),
                &SettingsService::new(Db::Sqlite(pool.clone())),
            ),
            users: UsersService::new(Db::Sqlite(pool.clone())),
            settings,
            events,
            // banto v1.7.0 #204: 本番と同じ照合付き（同じ `users`）。REST と
            // Tauri のセッションが同じアカウントの変更で終わることを、
            // 両経路をまたいで確かめるテストが使う。
            rest_auth: chronogazer_core::rest::user_auth_state(
                UsersService::new(Db::Sqlite(pool.clone())),
                AuditLogService::new(Db::Sqlite(pool.clone())),
            ),
            server: AsyncMutex::new(None),
            audit: AuditLogService::new(Db::Sqlite(pool.clone())),
            backup: BackupService::new(db_path, Db::Sqlite(pool.clone())),
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

    /// #463: `asOfId` 付きの取得（2 ブロック目以降）では剪定しない - 保持
    /// 件数の上限に張り付いた状態でも、読み込みの途中で行が消えて失効しない
    /// （`chronogazer_core::rest::audit_log_list` の同じ回帰テストと対の
    /// Tauri 側）。`tauri::State` はこのテストのように tauri アプリを起動
    /// せずには作れないので、[`audit_log_list_body`]（両方が委譲する共有の
    /// 層）を直接呼ぶ - `#[tauri::command]` の薄いラッパ自身は
    /// [`change_own_password`] と同じパターンでテストしない。`asOfId` なし
    /// （世代の最初・再読み込み）では従来どおり剪定する。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn audit_log_list_body_with_as_of_id_does_not_prune() {
        let state = app_state().await;
        let owner = state
            .users
            .setup_first_user("owner", "password123", "オーナー")
            .await
            .expect("setup_first_user");
        state.set_session_for_test(Some(DesktopSession::Account(owner)));

        for action in ["a", "b", "c", "d", "e"] {
            state
                .audit
                .try_record(AuditEntry {
                    actor_username: Some("owner"),
                    actor_role: Some("admin"),
                    action,
                    resource: "items",
                    entity_id: None,
                    detail: None,
                    origin: "tauri",
                    result: "ok",
                })
                .await
                .unwrap();
        }
        // #setup_first_user 自体は監査に残らないので、ここまでで 5 行。
        state
            .settings
            .set_audit_config(&AuditSettings {
                retention_days: None,
                retention_rows: Some(2),
            })
            .await
            .expect("set_audit_config");

        let count = || async {
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM audit_log")
                .fetch_one(&state.pool)
                .await
                .unwrap()
        };
        let before = count().await;
        assert_eq!(before, 5);
        let max_id: i64 = sqlx::query_scalar("SELECT MAX(id) FROM audit_log")
            .fetch_one(&state.pool)
            .await
            .unwrap();

        // `asOfId` 付き: 上限を超えていても消さない（件数も境界の集合のまま）。
        let pinned = audit_log_list_body(&state, ListParams::default(), Some(max_id))
            .await
            .expect("audit_log_list_body (asOfId 付き)");
        assert_eq!(pinned.total_count, before as u64, "{pinned:?}");
        assert_eq!(count().await, before, "asOfId 付きの取得で剪定された");

        // `asOfId` なし: 従来どおり剪定する。
        let fresh = audit_log_list_body(&state, ListParams::default(), None)
            .await
            .expect("audit_log_list_body (asOfId なし)");
        assert_eq!(fresh.total_count, 2);
        assert_eq!(count().await, 2, "asOfId なしの取得で剪定されない");
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
        state.set_session_for_test(Some(DesktopSession::Account(owner)));

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
        state.set_session_for_test(Some(DesktopSession::Account(owner)));

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
        state.set_session_for_test(Some(DesktopSession::Account(admin)));

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
        state.set_session_for_test(Some(DesktopSession::Account(viewer)));

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
        state.set_session_for_test(Some(DesktopSession::Account(admin)));

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
        state.set_session_for_test(Some(DesktopSession::Account(viewer)));

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
        state.set_session_for_test(Some(DesktopSession::Account(admin)));

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

        state.set_session_for_test(Some(DesktopSession::Account(viewer)));
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
        state.set_session_for_test(Some(DesktopSession::Account(editor)));
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
        state.set_session_for_test(Some(DesktopSession::Account(editor)));

        let outcome = collect_start_body(&state)
            .await
            .expect("収集対象 0 件は「エラー」ではない");
        assert_eq!(
            outcome.status,
            chronogazer_core::collect::CollectorState::NoTargets { exclusions: vec![] }
        );
        assert!(
            !outcome.pending,
            "上限に掛かっていないのに pending: {outcome:?}"
        );
        assert_eq!(
            collect_status_body(&state).await.expect("collect_status"),
            CollectorStateView::NoTargets { exclusions: vec![] }
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
    /// `data.dir` の場所に**ファイル**を置いて tstore の書き手を開けなくし
    /// （`Collector::start` が失敗する）、**editor に返った理由の文言そのもの**
    /// を目印にして、viewer の状態取得に出てこないことを確かめる（#414
    /// 段階2 で不正なアドレスのタグは「そのタグだけ外す」になり、起動の
    /// 失敗ではなくなったので、以前の「目印をアドレスに埋める」形から
    /// 変えた。REST 側の双子も同じ形）。**内部の `CollectorState` を
    /// 返していれば、同じ文言が必ず出てくる**。
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
        let dir = TempDir::new();
        let blocked_data_dir = dir.path().join(MARKER);
        std::fs::write(&blocked_data_dir, b"not a directory").expect("block data dir");
        let mut state = app_state().await;
        state.collect = CollectorService::new(state.pool.clone(), blocked_data_dir);

        // 収集対象を 1 件だけ作る（`data.dir` がファイル = 起動が失敗する）。
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
                    simulation: None,
                }
                .into_create_input(),
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
                    address: "40001".to_string(),
                    data_type: "i16".to_string(),
                    raw_lo: None,
                    raw_hi: None,
                    eng_lo: None,
                    eng_hi: None,
                    unit: None,
                    decimals: 0,
                    threshold_h: None,
                    threshold_hh: None,
                    threshold_l: None,
                    threshold_ll: None,
                    enabled: true,
                    expected_revision: None,
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
        state.set_session_for_test(Some(DesktopSession::Account(editor)));
        let err = collect_start_body(&state)
            .await
            .expect_err("前提が崩れている: 起動が失敗していない");
        let reason = err.to_string();
        assert!(
            reason.contains("時系列ストレージエラー"),
            "起動を実行した本人にも理由が伝わっていない（埋め合わせが無い）: {err}"
        );

        // viewer が状態を読む: 状態そのものは返るが、理由は返らない。
        state.set_session_for_test(Some(DesktopSession::Account(viewer)));
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
        let wire = json.to_string();
        assert!(
            !wire.contains(&reason) && !wire.contains(MARKER),
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
        collect_events_list_body(&state, None, None, None)
            .await
            .expect_err("未認証でイベント一覧が読めている");

        let viewer = state
            .users
            .create_user("viewer", "password123", "閲覧者", Role::Viewer)
            .await
            .expect("create_user");
        state.set_session_for_test(Some(DesktopSession::Account(viewer)));

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
        let events = collect_events_list_body(&state, None, None, None)
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

    /// R1-D の D-3a: 履歴も**読み取りと同じ床**（`viewer` 以上・セッションが
    /// 無ければ拒否）で、収集が走っていなくても `ready`。上限外は
    /// `Validation`。`chronogazer_core::rest` の
    /// `collect_history_is_viewer_readable_validated_and_ready_without_collection`
    /// と対になる双子のテスト（系列の中身は `chronogazer_core` 側が固定）。
    ///
    /// 反証: `collect_history_body` の `require_collect_reader` を外すと
    /// 未認証の `expect_err` が落ちる。`require_collect_editor` にすると viewer
    /// の呼び出しが `Forbidden` で落ちる。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn collect_history_is_viewer_readable_and_validated() {
        let state = app_state().await;

        collect_history_body(&state, vec![1], 0, 1000, 10)
            .await
            .expect_err("未認証で履歴が読めている");

        let viewer = state
            .users
            .create_user("viewer", "password123", "閲覧者", Role::Viewer)
            .await
            .expect("create_user");
        state.set_session_for_test(Some(DesktopSession::Account(viewer)));

        let history = collect_history_body(&state, vec![42], 0, 60_000, 60)
            .await
            .expect("viewer は履歴を読めること");
        let data = history.data().expect("走っていなくても ready");
        assert!(data.series.is_empty());
        assert_eq!(data.unknown_tag_ids, vec![42]);

        let err = collect_history_body(&state, vec![1], 0, 3_600_001, 10)
            .await
            .expect_err("1 時間を超える期間は拒否する");
        assert!(
            matches!(err, BantoError::Validation { .. }),
            "検証の誤りになっていない: {err:?}"
        );
    }

    // --- #413: 接続単位シミュレーション -------------------------------------

    fn sim_payload(name: &str, simulation: Option<bool>) -> PlcConnectionPayload {
        PlcConnectionPayload {
            name: name.to_string(),
            protocol: "modbus-tcp".to_string(),
            host: "192.168.11.200".to_string(),
            port: 502,
            unit_id: 1,
            enabled: true,
            word_order: String::new(),
            simulation,
        }
    }

    /// **Tauri 経路**で `simulation: true` が保存・返却され、監査の `detail`
    /// に切替が残る。viewer の変更は `Forbidden`。viewer は範囲外の判定を
    /// 読める。`chronogazer_core::rest` の
    /// `plc_connection_simulation_is_saved_returned_and_audited_over_rest`
    /// と**対になる双子のテスト**（両経路が同じことを主張する）。
    ///
    /// **省略した更新は既存の `true` を保ち**、明示の `false` で `false` に
    /// なる（#417 監査 P2。REST 側の双子のテストと同じ主張）。
    ///
    /// 反証（回帰の検出）: `plc_connections_create_body` の監査を
    /// `json!({ "name", "enabled" })`（#413 の前）に戻すと `detail["simulation"]`
    /// の `assert_eq!` が落ちる。`update_plc_connection` を通さず
    /// `None` を `false` にすると「省略した更新」の `assert!` が落ちる。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn plc_connection_simulation_is_saved_returned_and_audited_over_tauri() {
        let state = app_state().await;
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

        state.set_session_for_test(Some(DesktopSession::Account(editor)));
        let created = plc_connections_create_body(&state, sim_payload("sim-plc", Some(true)))
            .await
            .expect("editor は作れる");
        assert!(created.simulation, "作成の応答に出る");
        assert!(
            state
                .plc_connections
                .get(created.id)
                .await
                .expect("get")
                .simulation,
            "保存されている"
        );

        let kept = plc_connections_update_body(&state, created.id, sim_payload("renamed", None))
            .await
            .expect("editor は更新できる");
        assert_eq!(kept.name, "renamed", "他の項目は置き換わる");
        assert!(kept.simulation, "省略した更新で実機に戻った");

        let updated =
            plc_connections_update_body(&state, created.id, sim_payload("renamed", Some(false)))
                .await
                .expect("editor は切り替えられる");
        assert!(!updated.simulation);

        // 明示の切替（false）の後に届いた省略更新は、その切替を取り消さない
        // （UPDATE の時点の値を保つ。REST 側の双子と同じ主張）。
        let after_toggle =
            plc_connections_update_body(&state, created.id, sim_payload("renamed", None))
                .await
                .expect("editor は更新できる");
        assert!(
            !after_toggle.simulation,
            "省略更新が先行する明示の切替を取り消した"
        );

        let missing = plc_connections_update_body(&state, 999_999, sim_payload("x", None)).await;
        assert!(
            matches!(missing, Err(BantoError::NotFound { .. })),
            "存在しない id は従来どおり NotFound: {missing:?}"
        );

        let audit = state
            .audit
            .list(ListParams::default())
            .await
            .expect("audit list");
        // 並び順に依存しないよう、action ごとに simulation の値を集める。
        let simulations_of = |action: &str| -> Vec<serde_json::Value> {
            let mut values: Vec<serde_json::Value> = audit
                .rows
                .iter()
                .filter(|r| r.action == action && r.resource == "plc_connections")
                .map(|entry| {
                    assert_eq!(entry.origin, "tauri");
                    let detail: serde_json::Value =
                        serde_json::from_str(entry.detail.as_deref().expect("detail"))
                            .expect("JSON");
                    detail["simulation"].clone()
                })
                .collect();
            values.sort_by_key(|v| v.as_bool());
            values
        };
        assert_eq!(simulations_of("create"), vec![serde_json::json!(true)]);
        // 省略した更新（保たれた true）・明示の false・切替後の省略更新（false）。
        assert_eq!(
            simulations_of("update"),
            vec![
                serde_json::json!(false),
                serde_json::json!(false),
                serde_json::json!(true)
            ]
        );

        state.set_session_for_test(Some(DesktopSession::Account(viewer)));
        let denied =
            plc_connections_update_body(&state, created.id, sim_payload("renamed", Some(true)))
                .await;
        assert!(matches!(denied, Err(BantoError::Forbidden)), "{denied:?}");
        assert!(
            !state
                .plc_connections
                .get(created.id)
                .await
                .expect("get")
                .simulation,
            "拒否された切替は保存されない"
        );
        simulation_coverage_list_body(&state)
            .await
            .expect("viewer は範囲外の判定を読める");
    }

    /// #414 段階2: `config_exclusions_list` は viewer 以上で読め（REST の
    /// `GET /api/config-exclusions` と同じ床）、未ログインは拒否する。
    /// 中身（不正なタグが載ること・JSON の形）は同じ関数
    /// （`chronogazer_core::collect::registry_exclusions`）を叩く core 側の
    /// テストと REST の `config_exclusions_are_viewer_readable_and_carry_no_host`
    /// が固定している（このクレートは banto-tags に依存しないので、検証を
    /// くぐった行をここでは作れない）。
    #[tokio::test]
    async fn config_exclusions_list_is_viewer_readable() {
        let state = app_state().await;
        assert!(
            config_exclusions_list_body(&state).await.is_err(),
            "未ログインで読めてしまう"
        );

        let viewer = state
            .users
            .create_user("viewer", "password123", "閲覧者", Role::Viewer)
            .await
            .expect("create_user");
        state.set_session_for_test(Some(DesktopSession::Account(viewer)));
        assert_eq!(
            config_exclusions_list_body(&state)
                .await
                .expect("viewer は読める"),
            Vec::<ExclusionView>::new()
        );
    }

    // --- #414 段階1: タグのアドレス検査 -----------------------------------

    fn tag_payload(name: &str, collection_group_id: i64, address: &str) -> TagPayload {
        TagPayload {
            name: name.to_string(),
            collection_group_id,
            address: address.to_string(),
            data_type: "i16".to_string(),
            raw_lo: None,
            raw_hi: None,
            eng_lo: None,
            eng_hi: None,
            unit: None,
            decimals: 0,
            threshold_h: None,
            threshold_hh: None,
            threshold_l: None,
            threshold_ll: None,
            enabled: true,
            expected_revision: None,
        }
    }

    fn connection_payload(name: &str, protocol: &str) -> PlcConnectionPayload {
        PlcConnectionPayload {
            name: name.to_string(),
            protocol: protocol.to_string(),
            host: "192.168.11.200".to_string(),
            port: 502,
            unit_id: 1,
            enabled: true,
            word_order: String::new(),
            // #417: 省略 = 作成では false、更新では既存の値を保つ。
            simulation: None,
        }
    }

    fn only_field_error(err: BantoError) -> FieldError {
        match err {
            BantoError::Validation { mut field_errors } => {
                assert_eq!(field_errors.len(), 1, "{field_errors:?}");
                field_errors.remove(0)
            }
            other => panic!("検証エラーではない: {other:?}"),
        }
    }

    /// **#525（Tauri 経路）**: しきい値は保存され、古い `expectedRevision` の
    /// 更新は `expectedRevision` の検証エラー（REST の 409 と同じ中身）で拒否される。
    ///
    /// 反証: `tags_update_body` を `state.tags.update` に戻すと、最後の
    /// `is_revision_conflict` が落ちる（`Other` のまま返る）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn tag_commands_carry_thresholds_and_enforce_the_expected_revision() {
        let state = app_state().await;
        let editor = state
            .users
            .create_user("editor", "password123", "編集者", Role::Editor)
            .await
            .expect("create_user");
        state.set_session_for_test(Some(DesktopSession::Account(editor)));
        let conn = state
            .plc_connections
            .create(connection_payload("modbus", "modbus-tcp").into_create_input())
            .await
            .expect("create connection");
        let group = state
            .collection_groups
            .create(
                CollectionGroupPayload {
                    name: "g".to_string(),
                    plc_connection_id: conn.id,
                    period_ms: 1000,
                    enabled: true,
                }
                .into(),
            )
            .await
            .expect("create collection group");

        let mut payload = tag_payload("t", group.id, "40001");
        payload.threshold_h = Some(8.0);
        let tag = tags_create_body(&state, payload.clone())
            .await
            .expect("create");
        assert_eq!(tag.threshold_h, Some(8.0));

        // 大小関係が崩れる更新は拒否。
        let mut bad = payload.clone();
        bad.threshold_l = Some(9.0);
        bad.expected_revision = Some(tag.revision);
        assert!(matches!(
            tags_update_body(&state, tag.id, bad).await,
            Err(BantoError::Validation { .. })
        ));

        let mut ok = payload.clone();
        ok.threshold_h = Some(7.0);
        ok.expected_revision = Some(tag.revision);
        let updated = tags_update_body(&state, tag.id, ok).await.expect("update");
        assert_eq!(updated.threshold_h, Some(7.0));

        // 古い版での更新は食い違いとして拒否され、保存は変わらない。
        let mut stale = payload;
        stale.threshold_h = Some(8.5);
        stale.expected_revision = Some(tag.revision);
        let err = tags_update_body(&state, tag.id, stale)
            .await
            .expect_err("古い版の更新が通ってしまった");
        assert!(
            chronogazer_core::tag_address::is_revision_conflict(&err),
            "{err:?}"
        );
        assert_eq!(state.tags.get(tag.id).await.unwrap().threshold_h, Some(7.0));

        // 削除済みのタグへの古い版の更新は NotFound（競合でも汎用エラーでもない）。
        state.tags.delete(tag.id).await.expect("delete");
        let mut gone = tag_payload("t", group.id, "40001");
        gone.expected_revision = Some(tag.revision);
        let err = tags_update_body(&state, tag.id, gone)
            .await
            .expect_err("削除済みのタグが更新できてしまった");
        assert!(matches!(err, BantoError::NotFound { .. }), "{err:?}");
    }

    /// **#414 段階1（Tauri 経路）**: REST と同じ検査が、コマンドの本体にも
    /// 入っている（片方だけに書くと、もう片方の経路から通ってしまう）。
    /// タグの作成・更新（所属グループの変更を含む）と接続のプロトコル変更。
    ///
    /// 反証: 3 つの `*_body` から `ensure_*` の呼び出しを消すと、
    /// 3 つの `expect_err` が落ちる。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn tag_commands_refuse_an_address_the_connection_cannot_read() {
        let state = app_state().await;
        let editor = state
            .users
            .create_user("editor", "password123", "編集者", Role::Editor)
            .await
            .expect("create_user");
        state.set_session_for_test(Some(DesktopSession::Account(editor)));

        let modbus = state
            .plc_connections
            .create(connection_payload("modbus", "modbus-tcp").into_create_input())
            .await
            .expect("create modbus connection");
        let slmp = state
            .plc_connections
            .create(connection_payload("slmp", "slmp").into_create_input())
            .await
            .expect("create slmp connection");
        let mut group_ids = Vec::new();
        for (name, conn) in [("modbus-group", modbus.id), ("slmp-group", slmp.id)] {
            let group = state
                .collection_groups
                .create(
                    CollectionGroupPayload {
                        name: name.to_string(),
                        plc_connection_id: conn,
                        period_ms: 1000,
                        enabled: true,
                    }
                    .into(),
                )
                .await
                .expect("create collection group");
            group_ids.push(group.id);
        }
        let (modbus_group, slmp_group) = (group_ids[0], group_ids[1]);

        // 作成: Modbus の下の D3000 は `address` で拒否、40001 は通る。
        let err = tags_create_body(&state, tag_payload("melsec", modbus_group, "D3000"))
            .await
            .expect_err("Modbus の下の D3000 が保存できてしまった");
        assert_eq!(only_field_error(err).field, "address");
        let tag = tags_create_body(&state, tag_payload("modbus", modbus_group, "40001"))
            .await
            .expect("Modbus の下の 40001 は保存できること");

        // 更新で SLMP のグループへ移す: 40001 は SLMP では読めないので拒否。
        let err = tags_update_body(&state, tag.id, tag_payload("modbus", slmp_group, "40001"))
            .await
            .expect_err("SLMP のグループへ 40001 のまま移せてしまった");
        assert_eq!(only_field_error(err).field, "address");
        // アドレスも直して移すなら通る。
        tags_update_body(&state, tag.id, tag_payload("modbus", slmp_group, "D100"))
            .await
            .expect("アドレスを直して移すのは通ること");

        // 接続のプロトコル変更: SLMP の下に D100 があるので Modbus へは変えられない。
        let err =
            plc_connections_update_body(&state, slmp.id, connection_payload("slmp", "modbus-tcp"))
                .await
                .expect_err("配下のタグが読めなくなるプロトコル変更が通ってしまった");
        let field_error = only_field_error(err);
        assert_eq!(field_error.field, "protocol");
        assert!(
            field_error.message.contains("「modbus」（D100）"),
            "どのタグが妨げているかが分からない: {}",
            field_error.message
        );
        // プロトコルを変えない更新（名前の変更）は通る。
        plc_connections_update_body(&state, slmp.id, connection_payload("slmp-renamed", "slmp"))
            .await
            .expect("プロトコルを変えない更新は通ること");
    }

    /// **#414 段階1（Tauri 経路、グループの接続変更）**: 配下のタグが新しい
    /// 接続のプロトコルで読めなくなる移動は `plcConnectionId` で拒否され、
    /// 同じプロトコルの接続への移動は通り、存在しない接続は従来どおり
    /// `CollectionGroupService` 自身のエラー（「指定されたPLC接続が見つかり
    /// ません」）になる。
    ///
    /// 反証: `collection_groups_update_body` から
    /// `ensure_group_move_keeps_tags_readable` の呼び出しを消すと、最初の
    /// `expect_err` が落ちる。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn group_update_refuses_a_move_that_leaves_tags_unreadable() {
        let state = app_state().await;
        let editor = state
            .users
            .create_user("editor", "password123", "編集者", Role::Editor)
            .await
            .expect("create_user");
        state.set_session_for_test(Some(DesktopSession::Account(editor)));

        let mut ids = Vec::new();
        for (name, protocol) in [
            ("modbus", "modbus-tcp"),
            ("modbus2", "modbus-tcp"),
            ("slmp", "slmp"),
        ] {
            let conn = state
                .plc_connections
                .create(connection_payload(name, protocol).into_create_input())
                .await
                .expect("create connection");
            ids.push(conn.id);
        }
        let (modbus, modbus2, slmp) = (ids[0], ids[1], ids[2]);
        let group_payload = |plc_connection_id: i64| CollectionGroupPayload {
            name: "group".to_string(),
            plc_connection_id,
            period_ms: 1000,
            enabled: true,
        };
        let group = state
            .collection_groups
            .create(group_payload(modbus).into())
            .await
            .expect("create collection group");
        // 無効なタグも調べる対象。
        let mut disabled = tag_payload("modbus-tag", group.id, "40001");
        disabled.enabled = false;
        tags_create_body(&state, disabled)
            .await
            .expect("Modbus の下の 40001 は保存できること");

        // SLMP の接続へ移す: 40001 が読めなくなるので拒否、保存もされない。
        let err = collection_groups_update_body(&state, group.id, group_payload(slmp))
            .await
            .expect_err("タグが読めなくなるグループの移動が通ってしまった");
        let field_error = only_field_error(err);
        assert_eq!(field_error.field, "plcConnectionId");
        assert!(
            field_error.message.contains("「modbus-tag」（40001）")
                && field_error.message.contains("slmp"),
            "どのタグが妨げているかが分からない: {}",
            field_error.message
        );
        let unchanged = state
            .collection_groups
            .get(group.id)
            .await
            .expect("get group");
        assert_eq!(unchanged.plc_connection_id, modbus);

        // 同じプロトコルの接続への移動は通る。
        let moved = collection_groups_update_body(&state, group.id, group_payload(modbus2))
            .await
            .expect("同じプロトコルの接続への移動は通ること");
        assert_eq!(moved.plc_connection_id, modbus2);

        // 存在しない接続は従来どおり CollectionGroupService のエラー。
        let field_error = only_field_error(
            collection_groups_update_body(&state, group.id, group_payload(9999))
                .await
                .expect_err("存在しない接続への移動が通ってしまった"),
        );
        assert_eq!(field_error.field, "plcConnectionId");
        assert_eq!(field_error.message, "指定されたPLC接続が見つかりません");
    }

    /// **#418 オーナーレビュー P2 の回帰固定（Tauri 経路）**: 検査より前に
    /// 保存された不正タグを、読む場所を変えずに無効化するだけの更新は通り、
    /// 保存され、その後の `build_config` が成功して正常なタグだけが残る。
    /// アドレスを直さずに再有効化すると、引き続き拒否される。
    ///
    /// 反証: `tags_update_body` の `ensure_tag_update_fits_its_connection` を
    /// 無条件の `ensure_tag_fits_its_connection` に戻すと、無効化の
    /// `expect` が落ちる。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn disabling_a_legacy_bad_tag_is_allowed_and_reenabling_is_refused() {
        let state = app_state().await;
        let editor = state
            .users
            .create_user("editor", "password123", "編集者", Role::Editor)
            .await
            .expect("create_user");
        state.set_session_for_test(Some(DesktopSession::Account(editor)));

        let conn = state
            .plc_connections
            .create(connection_payload("modbus", "modbus-tcp").into_create_input())
            .await
            .expect("create connection");
        let group = state
            .collection_groups
            .create(
                CollectionGroupPayload {
                    name: "group".to_string(),
                    plc_connection_id: conn.id,
                    period_ms: 1000,
                    enabled: true,
                }
                .into(),
            )
            .await
            .expect("create collection group");
        // 正常なタグと、検査より前に保存された不正なタグ（既存データ）。
        state
            .tags
            .create(tag_payload("good", group.id, "40001").into())
            .await
            .expect("create good tag");
        let legacy = state
            .tags
            .create(tag_payload("legacy", group.id, "D3000").into())
            .await
            .expect("create legacy tag");
        assert!(
            banto_collect::build_config(&state.pool).await.is_err(),
            "前提が崩れている: 不正タグがあるのに build_config が通る"
        );

        // 無効化するだけの更新は通り、保存される。
        let mut disable = tag_payload("legacy", group.id, "D3000");
        disable.enabled = false;
        tags_update_body(&state, legacy.id, disable)
            .await
            .expect("無効化だけの更新が拒否された");
        assert!(!state.tags.get(legacy.id).await.unwrap().enabled);

        // その後の build_config は成功し、正常なタグだけが収集対象に残る。
        let config = banto_collect::build_config(&state.pool)
            .await
            .expect("不正タグを無効化した後は build_config が通ること");
        assert_eq!(config.tag_count(), 1);

        // アドレスを直さずに再有効化すると、引き続き拒否される。
        let err = tags_update_body(&state, legacy.id, tag_payload("legacy", group.id, "D3000"))
            .await
            .expect_err("アドレスを直さない再有効化が通った");
        assert_eq!(only_field_error(err).field, "address");
        assert!(!state.tags.get(legacy.id).await.unwrap().enabled);
    }

    // --- banto v1.7.0 #204: session revocation (Tauri + REST) ---------------

    /// What was changed about the target account.
    #[derive(Debug, Clone, Copy)]
    enum AccountChange {
        Delete,
        Demote,
        PasswordChange,
        PasswordReset,
    }

    /// Which transport made the change.
    #[derive(Debug, Clone, Copy)]
    enum Via {
        Tauri,
        Rest,
    }

    /// Everyone's sessions before the change. `admin`'s `rest_auth` is "the
    /// LAN server" every REST token below lives in; the other `AppState`s
    /// stand for other desktop windows/processes on the same DB.
    struct Sessions {
        admin: AppState,
        /// The target's desktop session that makes its own password change
        /// (`Via::Tauri` + `PasswordChange`); an ordinary target session
        /// otherwise.
        alice: AppState,
        /// Another desktop session of the target ("別の端末").
        alice_other: AppState,
        /// A bystander's desktop session.
        bob: AppState,
        router: axum::Router,
        admin_rest: String,
        alice_rest: String,
        alice_rest_remember: String,
        /// The target's REST session that makes its own password change
        /// (`Via::Rest` + `PasswordChange`); an ordinary one otherwise.
        alice_self_rest: String,
        bob_rest: String,
        alice_id: i64,
        _pool: chronogazer_core::db::DbPool,
    }

    async fn desktop_login(pool: &chronogazer_core::db::DbPool, username: &str) -> AppState {
        let state = app_state_on(pool.clone()).await;
        let identity = state
            .users
            .verify(username, "password123")
            .await
            .unwrap()
            .expect("verify");
        state.set_session_for_test(Some(DesktopSession::Account(identity)));
        state
    }

    async fn rest_login(auth: &AuthState, username: &str, remember: bool) -> String {
        match auth
            .login_rate_limited(None, username, "password123", remember)
            .await
        {
            banto_server::LoginOutcome::Success(token) => token,
            other => panic!("REST login for {username} failed: {other:?}"),
        }
    }

    fn router_for(state: &AppState) -> axum::Router {
        chronogazer_core::rest::api_router(
            state.users.clone(),
            state.settings.clone(),
            state.audit.clone(),
            state.backup.clone(),
            state.hub.clone(),
            state.plc_connections.clone(),
            state.collection_groups.clone(),
            state.tags.clone(),
            state.collect.clone(),
            display_groups_service(state),
            state.rest_auth.clone(),
            state.events.clone(),
            false,
        )
    }

    fn rest_request(
        method: &str,
        path: &str,
        token: &str,
        body: Option<serde_json::Value>,
    ) -> axum::http::Request<axum::body::Body> {
        let builder = axum::http::Request::builder()
            .method(method)
            .uri(path)
            .header("X-Banto-Client", "banto")
            .header("Authorization", format!("Bearer {token}"));
        match body {
            Some(body) => builder
                .header("content-type", "application/json")
                .body(axum::body::Body::from(body.to_string()))
                .unwrap(),
            None => builder.body(axum::body::Body::empty()).unwrap(),
        }
    }

    async fn rest_status(
        router: &axum::Router,
        request: axum::http::Request<axum::body::Body>,
    ) -> axum::http::StatusCode {
        use tower::ServiceExt;
        router.clone().oneshot(request).await.unwrap().status()
    }

    /// A protected route any role may read (viewer+).
    async fn rest_can_read(router: &axum::Router, token: &str) -> axum::http::StatusCode {
        rest_status(router, rest_request("GET", "/api/tags", token, None)).await
    }

    async fn sessions() -> Sessions {
        let pool = chronogazer_core::db::init_db_memory()
            .await
            .expect("init_db_memory");
        let users = UsersService::new(Db::Sqlite(pool.clone()));
        users
            .setup_first_user("admin", "password123", "管理者")
            .await
            .unwrap();
        let alice = users
            .create_user("alice", "password123", "Alice", Role::Editor)
            .await
            .unwrap();
        users
            .create_user("bob", "password123", "Bob", Role::Editor)
            .await
            .unwrap();

        let admin = desktop_login(&pool, "admin").await;
        let router = router_for(&admin);
        let admin_rest = rest_login(&admin.rest_auth, "admin", false).await;
        let alice_rest = rest_login(&admin.rest_auth, "alice", false).await;
        let alice_rest_remember = rest_login(&admin.rest_auth, "alice", true).await;
        let alice_self_rest = rest_login(&admin.rest_auth, "alice", false).await;
        let bob_rest = rest_login(&admin.rest_auth, "bob", true).await;
        Sessions {
            alice: desktop_login(&pool, "alice").await,
            alice_other: desktop_login(&pool, "alice").await,
            bob: desktop_login(&pool, "bob").await,
            admin,
            router,
            admin_rest,
            alice_rest,
            alice_rest_remember,
            alice_self_rest,
            bob_rest,
            alice_id: alice.id,
            _pool: pool,
        }
    }

    async fn apply_change(s: &Sessions, change: AccountChange, via: Via) {
        let id = s.alice_id;
        match (change, via) {
            (AccountChange::Delete, Via::Tauri) => {
                users_delete_body(&s.admin, id).await.unwrap();
            }
            (AccountChange::Demote, Via::Tauri) => {
                users_update_body(&s.admin, id, "Alice", Role::Viewer)
                    .await
                    .unwrap();
            }
            (AccountChange::PasswordReset, Via::Tauri) => {
                users_reset_password_body(&s.admin, id, "reset-password1")
                    .await
                    .unwrap();
            }
            (AccountChange::PasswordChange, Via::Tauri) => {
                change_own_password(&s.alice, "password123", "changed-password1")
                    .await
                    .unwrap();
            }
            (AccountChange::Delete, Via::Rest) => {
                let status = rest_status(
                    &s.router,
                    rest_request("DELETE", &format!("/api/users/{id}"), &s.admin_rest, None),
                )
                .await;
                assert_eq!(status, axum::http::StatusCode::NO_CONTENT);
            }
            (AccountChange::Demote, Via::Rest) => {
                let status = rest_status(
                    &s.router,
                    rest_request(
                        "PUT",
                        &format!("/api/users/{id}"),
                        &s.admin_rest,
                        Some(serde_json::json!({ "displayName": "Alice", "role": "viewer" })),
                    ),
                )
                .await;
                assert_eq!(status, axum::http::StatusCode::OK);
            }
            (AccountChange::PasswordReset, Via::Rest) => {
                let status = rest_status(
                    &s.router,
                    rest_request(
                        "POST",
                        &format!("/api/users/{id}/reset-password"),
                        &s.admin_rest,
                        Some(serde_json::json!({ "newPassword": "reset-password1" })),
                    ),
                )
                .await;
                assert_eq!(status, axum::http::StatusCode::OK);
            }
            (AccountChange::PasswordChange, Via::Rest) => {
                let status = rest_status(
                    &s.router,
                    rest_request(
                        "POST",
                        "/api/auth/change-password",
                        &s.alice_self_rest,
                        Some(serde_json::json!({
                            "currentPassword": "password123",
                            "newPassword": "changed-password1",
                        })),
                    ),
                )
                .await;
                assert_eq!(status, axum::http::StatusCode::OK);
            }
        }
    }

    /// Is the desktop session still accepted by a protected command?
    async fn desktop_allowed(state: &AppState) -> Result<(), BantoError> {
        require_role(state, Role::Viewer, "test").await.map(|_| ())
    }

    /// banto v1.7.0 #204 completion table: for each change (delete, demote,
    /// password change, password reset) made over EACH transport, every
    /// session the target held elsewhere - a normal REST token, a
    /// "Remember me" REST token, and other desktop sessions - is refused by
    /// protected routes/commands, while the bystander's and the acting
    /// admin's sessions are untouched, and the session that changed its OWN
    /// password keeps working.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn account_changes_end_the_accounts_other_sessions_on_both_transports() {
        use axum::http::StatusCode;
        for change in [
            AccountChange::Delete,
            AccountChange::Demote,
            AccountChange::PasswordChange,
            AccountChange::PasswordReset,
        ] {
            for via in [Via::Tauri, Via::Rest] {
                let case = format!("{change:?} via {via:?}");
                let s = sessions().await;
                // Precondition: every session works before the change.
                for token in [
                    &s.alice_rest,
                    &s.alice_rest_remember,
                    &s.alice_self_rest,
                    &s.bob_rest,
                ] {
                    assert_eq!(
                        rest_can_read(&s.router, token).await,
                        StatusCode::OK,
                        "{case}"
                    );
                }
                desktop_allowed(&s.alice).await.unwrap();
                desktop_allowed(&s.alice_other).await.unwrap();

                apply_change(&s, change, via).await;

                let self_change = matches!(change, AccountChange::PasswordChange);
                // The target's other sessions end - REST (normal and
                // Remember me) and desktop.
                assert_eq!(
                    rest_can_read(&s.router, &s.alice_rest).await,
                    StatusCode::UNAUTHORIZED,
                    "{case}: normal REST session"
                );
                assert_eq!(
                    rest_can_read(&s.router, &s.alice_rest_remember).await,
                    StatusCode::UNAUTHORIZED,
                    "{case}: Remember me REST session"
                );
                assert!(
                    matches!(
                        desktop_allowed(&s.alice_other).await,
                        Err(BantoError::Unauthorized)
                    ),
                    "{case}: other desktop session"
                );
                assert!(
                    s.alice_other.auth.lock().unwrap().session.is_none(),
                    "{case}: the ended desktop session is cleared"
                );
                // The session that changed its own password stays; for every
                // other change the target's sessions all end.
                let alice_self_rest = rest_can_read(&s.router, &s.alice_self_rest).await;
                let alice_desktop = desktop_allowed(&s.alice).await;
                match (change, via) {
                    (AccountChange::PasswordChange, Via::Rest) => {
                        assert_eq!(alice_self_rest, StatusCode::OK, "{case}: changer (REST)");
                        assert!(
                            matches!(alice_desktop, Err(BantoError::Unauthorized)),
                            "{case}"
                        );
                    }
                    (AccountChange::PasswordChange, Via::Tauri) => {
                        assert!(alice_desktop.is_ok(), "{case}: changer (desktop)");
                        assert_eq!(alice_self_rest, StatusCode::UNAUTHORIZED, "{case}");
                    }
                    _ => {
                        assert!(!self_change);
                        assert_eq!(alice_self_rest, StatusCode::UNAUTHORIZED, "{case}");
                        assert!(
                            matches!(alice_desktop, Err(BantoError::Unauthorized)),
                            "{case}"
                        );
                    }
                }
                // Untouched accounts keep their sessions.
                assert_eq!(
                    rest_can_read(&s.router, &s.bob_rest).await,
                    StatusCode::OK,
                    "{case}: bystander REST"
                );
                desktop_allowed(&s.bob)
                    .await
                    .unwrap_or_else(|err| panic!("{case}: bystander desktop: {err:?}"));
                assert_eq!(
                    rest_status(
                        &s.router,
                        rest_request("GET", "/api/users", &s.admin_rest, None)
                    )
                    .await,
                    StatusCode::OK,
                    "{case}: acting admin REST"
                );
                require_role(&s.admin, Role::Admin, "test")
                    .await
                    .unwrap_or_else(|err| panic!("{case}: acting admin desktop: {err:?}"));
            }
        }
    }

    /// #204: a demoted account that logs in again is authorized with its NEW
    /// role (the re-read row), on both transports - the fresh session is not
    /// ended, and an admin-only command/route is now refused as `Forbidden`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_fresh_session_after_a_demotion_uses_the_new_role() {
        let s = sessions().await;
        apply_change(&s, AccountChange::Demote, Via::Tauri).await;
        let again = desktop_login(&s._pool, "alice").await;
        assert_eq!(
            require_role(&again, Role::Viewer, "test")
                .await
                .unwrap()
                .role,
            Role::Viewer
        );
        assert!(matches!(
            require_role(&again, Role::Editor, "test").await,
            Err(BantoError::Forbidden)
        ));
        let token = rest_login(&s.admin.rest_auth, "alice", false).await;
        assert_eq!(
            rest_can_read(&s.router, &token).await,
            axum::http::StatusCode::OK
        );
    }

    /// #204: when the DB cannot answer the re-check, the command fails but
    /// the session is KEPT (a DB hiccup must not log anyone out), on both
    /// transports; it works again once the DB answers.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_failed_recheck_fails_the_request_but_keeps_the_session() {
        let s = sessions().await;
        sqlx_rename_users(&s._pool, "users", "users_away").await;

        let err = desktop_allowed(&s.alice).await.expect_err("DB is gone");
        assert!(
            !matches!(err, BantoError::Unauthorized),
            "a DB failure is not 'logged out': {err:?}"
        );
        assert!(
            s.alice.auth.lock().unwrap().session.is_some(),
            "session kept"
        );
        assert!(
            s.admin.rest_auth.authenticate(&s.alice_rest).await.is_err(),
            "REST re-check fails too"
        );
        assert_eq!(
            rest_can_read(&s.router, &s.alice_rest).await,
            axum::http::StatusCode::INTERNAL_SERVER_ERROR
        );

        sqlx_rename_users(&s._pool, "users_away", "users").await;
        desktop_allowed(&s.alice).await.expect("session resumes");
        assert_eq!(
            rest_can_read(&s.router, &s.alice_rest).await,
            axum::http::StatusCode::OK
        );
    }

    /// Make the `users` table unreadable (and readable again) - the
    /// simplest way to have the account store fail the re-check.
    async fn sqlx_rename_users(pool: &chronogazer_core::db::DbPool, from: &str, to: &str) {
        let sql = match (from, to) {
            ("users", "users_away") => "ALTER TABLE users RENAME TO users_away",
            ("users_away", "users") => "ALTER TABLE users_away RENAME TO users",
            _ => unreachable!(),
        };
        sqlx::query(sql).execute(pool).await.unwrap();
    }

    /// #204 + spec M11: the auth-disabled-mode ("認証なしのローカルモード")
    /// synthetic session is valid only while the mode is ON. Turning it OFF
    /// ends the session on the next command (the login screen takes over),
    /// and the synthetic session can never change a real account's password.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_auth_disabled_session_ends_when_the_mode_is_turned_off() {
        let state = app_state().await;
        let mut config = state.settings.auth_config().await.unwrap();
        config.disabled = true;
        config.disabled_role = Role::Editor;
        state.settings.set_auth_config(&config).await.unwrap();
        state.set_session_for_test(Some(DesktopSession::AuthDisabledLocal(UserIdentity {
            id: LOCAL_SESSION_ID,
            username: "local".to_string(),
            display_name: "ローカルユーザー".to_string(),
            role: Role::Editor,
            auth_epoch: 0,
        })));
        assert_eq!(
            require_role(&state, Role::Editor, "test")
                .await
                .unwrap()
                .role,
            Role::Editor
        );
        assert!(matches!(
            change_own_password(&state, "x", "new-password1").await,
            Err(BantoError::Forbidden)
        ));

        config.disabled = false;
        state.settings.set_auth_config(&config).await.unwrap();
        assert!(matches!(
            require_role(&state, Role::Viewer, "test").await,
            Err(BantoError::Unauthorized)
        ));
        assert!(
            state.auth.lock().unwrap().session.is_none(),
            "session cleared"
        );
    }

    // --- banto v2.0.0 移行: 認証コマンドの監査（admin-template から移植） ---

    /// [`auth_config_apply_body`]'s normal (non-escape-hatch) path records a
    /// `settings_change`/`settings` entry attributed to the admin, with an
    /// `{authDisabled}` detail (banto M-review 2026-08 M-5).
    #[tokio::test]
    async fn auth_config_apply_is_recorded_as_settings_change() {
        let state = app_state().await;
        let admin = state
            .users
            .setup_first_user("admin", "password123", "管理者")
            .await
            .expect("setup_first_user");
        state.set_session_for_test(Some(DesktopSession::Account(admin)));

        auth_config_apply_body(&state, true, "viewer")
            .await
            .expect("auth_config_apply should succeed");

        let audit = state
            .audit
            .list(ListParams::default())
            .await
            .expect("audit list");
        let entry = audit
            .rows
            .iter()
            .find(|r| r.action == "settings_change" && r.resource == "settings")
            .unwrap_or_else(|| panic!("expected a settings_change entry, got {:?}", audit.rows));
        assert_eq!(entry.actor_username.as_deref(), Some("admin"));
        assert_eq!(entry.actor_role.as_deref(), Some("admin"));
        assert_eq!(entry.entity_id, None);
        assert_eq!(entry.origin, "tauri");
        assert_eq!(entry.result, "ok");
        let detail: serde_json::Value =
            serde_json::from_str(entry.detail.as_deref().expect("detail present"))
                .expect("detail is json");
        assert_eq!(detail, serde_json::json!({ "authDisabled": true }));
    }

    /// [`autologin_enable_body`] records a `settings_change`/`settings` entry
    /// with an `{autologinEnabled,username}` detail (banto M-review 2026-08 M-5).
    /// Installs the in-memory keyring first so `keyring_store::set_password`
    /// does not hit the OS secret service on a headless CI runner (the Linux
    /// leg has no D-Bus/secret-service).
    #[tokio::test]
    async fn autologin_enable_is_recorded_as_settings_change() {
        keyring::set_default_credential_builder(keyring::mock::default_credential_builder());
        let state = app_state().await;
        let admin = state
            .users
            .setup_first_user("admin", "password123", "管理者")
            .await
            .expect("setup_first_user");
        state.set_session_for_test(Some(DesktopSession::Account(admin)));

        autologin_enable_body(&state, "admin", "password123")
            .await
            .expect("autologin_enable should succeed");

        let audit = state
            .audit
            .list(ListParams::default())
            .await
            .expect("audit list");
        let entry = audit
            .rows
            .iter()
            .find(|r| r.action == "settings_change" && r.resource == "settings")
            .unwrap_or_else(|| panic!("expected a settings_change entry, got {:?}", audit.rows));
        assert_eq!(entry.actor_username.as_deref(), Some("admin"));
        assert_eq!(entry.entity_id, None);
        assert_eq!(entry.result, "ok");
        let detail: serde_json::Value =
            serde_json::from_str(entry.detail.as_deref().expect("detail present"))
                .expect("detail is json");
        assert_eq!(
            detail,
            serde_json::json!({ "autologinEnabled": true, "username": "admin" })
        );
    }

    /// [`autologin_disable_body`] records a `settings_change`/`settings` entry
    /// with an `{autologinEnabled:false}` detail (banto M-review 2026-08 M-5). No
    /// keyring is touched: with `autologin_username` unset the delete is
    /// skipped.
    #[tokio::test]
    async fn autologin_disable_is_recorded_as_settings_change() {
        let state = app_state().await;
        let admin = state
            .users
            .setup_first_user("admin", "password123", "管理者")
            .await
            .expect("setup_first_user");
        state.set_session_for_test(Some(DesktopSession::Account(admin)));

        autologin_disable_body(&state)
            .await
            .expect("autologin_disable should succeed");

        let audit = state
            .audit
            .list(ListParams::default())
            .await
            .expect("audit list");
        let entry = audit
            .rows
            .iter()
            .find(|r| r.action == "settings_change" && r.resource == "settings")
            .unwrap_or_else(|| panic!("expected a settings_change entry, got {:?}", audit.rows));
        assert_eq!(entry.actor_username.as_deref(), Some("admin"));
        assert_eq!(entry.entity_id, None);
        assert_eq!(entry.result, "ok");
        let detail: serde_json::Value =
            serde_json::from_str(entry.detail.as_deref().expect("detail present"))
                .expect("detail is json");
        assert_eq!(detail, serde_json::json!({ "autologinEnabled": false }));
    }

    // --- banto #260: the session slot's seq and compare-and-set ------------
    //
    // docs/design/session-controller-design.md §4.3/§8.3: the command BODIES are
    // driven with their slow `.await` (verify / first-user setup / auth-mode
    // read) held at an injected gate, so the completion order is fixed by
    // the test, not by the scheduler.

    use tokio::sync::oneshot;

    const SLOT_PASSWORD: &str = "password123";

    /// The test's end of a one-shot gate: `entered` resolves once the held
    /// command reached the injected `.await`; sending on `release` lets it
    /// continue.
    struct Hold {
        entered: oneshot::Receiver<()>,
        release: oneshot::Sender<()>,
    }

    type GateSlot = Arc<Mutex<Option<(oneshot::Sender<()>, oneshot::Receiver<()>)>>>;

    fn gate() -> (GateSlot, Hold) {
        let (entered_tx, entered_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        (
            Arc::new(Mutex::new(Some((entered_tx, release_rx)))),
            Hold {
                entered: entered_rx,
                release: release_tx,
            },
        )
    }

    /// The FIRST caller through `gate` stops here until released; later
    /// callers pass straight through.
    async fn pass_gate(gate: &GateSlot) {
        let taken = gate.lock().expect("gate mutex").take();
        if let Some((entered, release)) = taken {
            let _ = entered.send(());
            let _ = release.await;
        }
    }

    /// Hold the first `auth_login` verification of `state`.
    fn hold_verify(state: &mut AppState) -> Hold {
        let (slot, hold) = gate();
        let users = state.users.clone();
        state.auth_io.verify = Arc::new(move |username, password| {
            let slot = slot.clone();
            let users = users.clone();
            Box::pin(async move {
                pass_gate(&slot).await;
                users.verify(&username, &password).await
            })
        });
        hold
    }

    /// Hold the first `auth_setup` account creation of `state`.
    fn hold_setup(state: &mut AppState) -> Hold {
        let (slot, hold) = gate();
        let users = state.users.clone();
        state.auth_io.setup_first_user = Arc::new(move |username, password, display_name| {
            let slot = slot.clone();
            let users = users.clone();
            Box::pin(async move {
                pass_gate(&slot).await;
                users
                    .setup_first_user(&username, &password, &display_name)
                    .await
            })
        });
        hold
    }

    /// Hold the first `auth_logout` auth-mode read of `state`.
    fn hold_auth_mode(state: &mut AppState) -> Hold {
        let (slot, hold) = gate();
        let settings = state.settings.clone();
        state.auth_io.auth_mode = Arc::new(move || {
            let slot = slot.clone();
            let settings = settings.clone();
            Box::pin(async move {
                pass_gate(&slot).await;
                settings.auth_config().await
            })
        });
        hold
    }

    /// Hold the `nth` (1-based) auth-mode read of `state` - whichever command
    /// makes it - either before the settings read (`after_read: false`, the
    /// held read sees what is stored when it is released) or after it
    /// (`true`, the held command carries the value read BEFORE the hold).
    fn hold_auth_mode_call(state: &mut AppState, nth: usize, after_read: bool) -> Hold {
        let (slot, hold) = gate();
        let settings = state.settings.clone();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        state.auth_io.auth_mode = Arc::new(move || {
            let (slot, settings, calls) = (slot.clone(), settings.clone(), calls.clone());
            Box::pin(async move {
                let this = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                if this != nth {
                    return settings.auth_config().await;
                }
                if after_read {
                    let config = settings.auth_config().await;
                    pass_gate(&slot).await;
                    config
                } else {
                    pass_gate(&slot).await;
                    settings.auth_config().await
                }
            })
        });
        hold
    }

    fn is_local(session: &Option<DesktopSession>) -> bool {
        matches!(session, Some(DesktopSession::AuthDisabledLocal(user)) if user.username == "local")
    }

    async fn audit_action_count(state: &AppState, action: &str) -> usize {
        state
            .audit
            .list(ListParams::default())
            .await
            .expect("audit list")
            .rows
            .iter()
            .filter(|row| row.action == action)
            .count()
    }

    /// Poll `$fut` until the command reaches its gate (it must not finish
    /// first).
    macro_rules! run_until_held {
        ($fut:expr, $hold:expr) => {{
            tokio::select! {
                biased;
                out = &mut $fut => panic!("finished before reaching the gate: {out:?}"),
                _ = &mut $hold.entered => {}
            }
        }};
    }

    fn is_account(session: &Option<DesktopSession>, username: &str) -> bool {
        matches!(session, Some(DesktopSession::Account(user)) if user.username == username)
    }

    /// admin-template のテストが「BOOTSTRAP WINDOW」（アカウント 0 件・
    /// セッションなしの `auth_config_apply`）で認証なしモードを入れている箇所の
    /// 置き換え。ChronoGazer にはその経路が無い（`auth_config_apply_body` の
    /// doc）ので、管理者アカウント `admin` を作ってそのセッションから入れる。
    /// 結果は同じ `Local(role)` だが、監査には `logout`（管理者の、reason
    /// `auth_disabled`。S-94 の付け替え）が 1 件増える。
    async fn enable_local_from_an_admin_session(state: &AppState, role: &str) {
        let admin = state
            .users
            .setup_first_user("admin", SLOT_PASSWORD, "Admin")
            .await
            .expect("setup_first_user");
        state.set_session_for_test(Some(DesktopSession::Account(admin)));
        auth_config_apply_body(state, true, role)
            .await
            .expect("enable auth-disabled mode from the admin's session");
        assert!(is_local(&read_slot(state).0));
    }

    /// S-16: a login whose verification is overtaken by a completed logout
    /// must not revive a session (design §1.3 order 1).
    #[tokio::test]
    async fn s16_a_slow_login_does_not_undo_a_completed_logout() {
        let mut state = app_state().await;
        state
            .users
            .setup_first_user("b", SLOT_PASSWORD, "B")
            .await
            .unwrap();
        let mut hold = hold_verify(&mut state);

        let login = login_body(&state, "b".to_string(), SLOT_PASSWORD.to_string());
        tokio::pin!(login);
        run_until_held!(login, hold);

        let logout = logout_body(&state).await.expect("logout");
        assert_eq!(logout.seq, 1, "a logout of no session still advances seq");
        hold.release.send(()).unwrap();
        let result = login.await.expect("login");

        assert!(!result.success);
        assert!(result.superseded);
        assert_eq!(result.seq, 1);
        assert_eq!(read_slot(&state), (None, 1));
        assert_eq!(audit_action_count(&state, "login").await, 1, "verified");
        assert_eq!(audit_action_count(&state, "logout").await, 0);
    }

    /// S-16 (wiring): the login must compare against the seq read BEFORE its
    /// first `.await` - any seq advance while it is held makes it superseded.
    #[tokio::test]
    async fn s16_login_reads_seq_before_its_first_await() {
        let mut state = app_state().await;
        state
            .users
            .setup_first_user("b", SLOT_PASSWORD, "B")
            .await
            .unwrap();
        let mut hold = hold_verify(&mut state);

        let login = login_body(&state, "b".to_string(), SLOT_PASSWORD.to_string());
        tokio::pin!(login);
        run_until_held!(login, hold);

        let (_, seq) = read_slot(&state);
        assert_eq!(cas_session(&state, seq, None), (true, seq + 1));
        hold.release.send(()).unwrap();
        let result = login.await.expect("login");

        assert!(result.superseded);
        assert_eq!(read_slot(&state), (None, seq + 1));
    }

    /// S-16 (control): the same login with nothing in between installs the
    /// session and advances seq; a failed login leaves the slot alone.
    #[tokio::test]
    async fn s16_an_uncontested_login_installs_and_advances_seq() {
        let state = app_state().await;
        state
            .users
            .setup_first_user("b", SLOT_PASSWORD, "B")
            .await
            .unwrap();

        let result = login_body(&state, "b".to_string(), SLOT_PASSWORD.to_string())
            .await
            .expect("login");

        assert!(result.success);
        assert!(!result.superseded);
        assert_eq!(result.seq, 1);
        let (session, seq) = read_slot(&state);
        assert_eq!(seq, 1);
        assert!(is_account(&session, "b"));

        let failed = login_body(&state, "b".to_string(), "wrong-password".to_string())
            .await
            .expect("login");
        assert!(!failed.success);
        assert!(!failed.superseded);
        assert_eq!(failed.seq, 1, "a failed login does not touch the slot");
    }

    /// S-17: a logout whose settings read is overtaken by a completed login
    /// must not clear that login's session, and records no `logout`.
    #[tokio::test]
    async fn s17_a_slow_logout_does_not_clear_a_later_login() {
        let mut state = app_state().await;
        state
            .users
            .setup_first_user("b", SLOT_PASSWORD, "B")
            .await
            .unwrap();
        let mut hold = hold_auth_mode(&mut state);

        let logout = logout_body(&state);
        tokio::pin!(logout);
        run_until_held!(logout, hold);

        let login = login_body(&state, "b".to_string(), SLOT_PASSWORD.to_string())
            .await
            .expect("login");
        assert!(login.success);
        hold.release.send(()).unwrap();
        let result = logout.await.expect("logout");

        assert_eq!(
            result.seq, login.seq,
            "the overtaken logout changes nothing"
        );
        let (session, seq) = read_slot(&state);
        assert_eq!(seq, login.seq);
        assert!(is_account(&session, "b"));
        assert_eq!(audit_action_count(&state, "logout").await, 0);
    }

    /// S-17 (wiring): the logout compares against the seq read before its
    /// first `.await` - a re-bind while it is held (even to the same
    /// account) keeps the session.
    #[tokio::test]
    async fn s17_logout_reads_seq_before_its_first_await() {
        let mut state = app_state().await;
        let a = state
            .users
            .setup_first_user("a", SLOT_PASSWORD, "A")
            .await
            .unwrap();
        state.set_session_for_test(Some(DesktopSession::Account(a.clone())));
        let mut hold = hold_auth_mode(&mut state);

        let logout = logout_body(&state);
        tokio::pin!(logout);
        run_until_held!(logout, hold);

        let (_, seq) = read_slot(&state);
        let rebound = Some(DesktopSession::Account(a));
        assert_eq!(cas_session(&state, seq, rebound.clone()), (true, seq + 1));
        hold.release.send(()).unwrap();
        let result = logout.await.expect("logout");

        assert_eq!(result.seq, seq + 1);
        assert_eq!(read_slot(&state), (rebound, seq + 1));
    }

    /// S-17 (control): an uncontested logout clears, advances seq, and is
    /// audited.
    #[tokio::test]
    async fn s17_an_uncontested_logout_clears_and_advances_seq() {
        let state = app_state().await;
        let a = state
            .users
            .setup_first_user("a", SLOT_PASSWORD, "A")
            .await
            .unwrap();
        state.set_session_for_test(Some(DesktopSession::Account(a)));

        let result = logout_body(&state).await.expect("logout");

        assert_eq!(result.seq, 2);
        assert_eq!(read_slot(&state), (None, 2));
        assert_eq!(audit_action_count(&state, "logout").await, 1);
    }

    /// S-18: `auth_setup` overtaken by a completed logout: the account is
    /// created, the session is not installed.
    #[tokio::test]
    async fn s18_a_slow_setup_does_not_undo_a_completed_logout() {
        let mut state = app_state().await;
        let mut hold = hold_setup(&mut state);

        let setup = setup_body(
            &state,
            "b".to_string(),
            SLOT_PASSWORD.to_string(),
            "B".to_string(),
        );
        tokio::pin!(setup);
        run_until_held!(setup, hold);

        logout_body(&state).await.expect("logout");
        hold.release.send(()).unwrap();
        let result = setup.await.expect("setup");

        assert!(!result.success);
        assert!(result.superseded);
        assert_eq!(read_slot(&state), (None, 1));
        assert!(state.users.is_initialized().await.unwrap());
        assert!(state.users.get_by_username("b").await.unwrap().is_some());
    }

    /// S-19: a logout overtaken by a completed `auth_setup` keeps the setup's
    /// session.
    #[tokio::test]
    async fn s19_a_slow_logout_does_not_clear_a_later_setup() {
        let mut state = app_state().await;
        let mut hold = hold_auth_mode(&mut state);

        let logout = logout_body(&state);
        tokio::pin!(logout);
        run_until_held!(logout, hold);

        let setup = setup_body(
            &state,
            "b".to_string(),
            SLOT_PASSWORD.to_string(),
            "B".to_string(),
        )
        .await
        .expect("setup");
        assert!(setup.success);
        hold.release.send(()).unwrap();
        let result = logout.await.expect("logout");

        assert_eq!(result.seq, setup.seq);
        assert!(is_account(&read_slot(&state).0, "b"));
    }

    /// S-54: re-resolving the same session - including after a display-name
    /// edit (a refresh of the same binding) - never advances seq.
    #[tokio::test]
    async fn s54_resolving_the_same_session_does_not_advance_seq() {
        let state = app_state().await;
        let a = state
            .users
            .setup_first_user("a", SLOT_PASSWORD, "A")
            .await
            .unwrap();
        let login = login_body(&state, "a".to_string(), SLOT_PASSWORD.to_string())
            .await
            .expect("login");

        for _ in 0..3 {
            let answer = resolve_body(&state).await.expect("resolve");
            assert!(!answer.stale);
            assert_eq!((answer.checked, answer.current), (login.seq, login.seq));
            assert_eq!(answer.kind, Some("account"));
            assert_eq!(answer.identity.expect("active").name, "A");
        }

        state
            .users
            .update_user(a.id, "A（改名）", Role::Admin)
            .await
            .unwrap();
        let answer = resolve_body(&state).await.expect("resolve");
        assert!(!answer.stale);
        assert_eq!((answer.checked, answer.current), (login.seq, login.seq));
        assert_eq!(answer.identity.expect("still active").name, "A（改名）");
        assert_eq!(read_slot(&state).1, login.seq);
    }

    /// S-64: a role change advances `auth_epoch` (ADR-0014), so the next
    /// resolve reports no session and advances seq (the clear) - unlike the
    /// display-name refresh of S-54.
    #[tokio::test]
    async fn s64_a_role_change_resolves_to_none_and_advances_seq() {
        let state = app_state().await;
        state
            .users
            .setup_first_user("owner", SLOT_PASSWORD, "オーナー")
            .await
            .unwrap();
        let a = state
            .users
            .create_user("a", SLOT_PASSWORD, "A", Role::Editor)
            .await
            .unwrap();
        let login = login_body(&state, "a".to_string(), SLOT_PASSWORD.to_string())
            .await
            .expect("login");
        assert!(resolve_body(&state).await.unwrap().identity.is_some());

        state
            .users
            .update_user(a.id, "A", Role::Viewer)
            .await
            .unwrap();
        let answer = resolve_body(&state).await.expect("resolve");

        assert!(!answer.stale);
        assert!(answer.identity.is_none(), "not the new role's session");
        assert_eq!(answer.kind, None);
        assert_eq!(answer.checked, login.seq);
        assert_eq!(answer.current, login.seq + 1, "this call cleared it");
        assert_eq!(read_slot(&state), (None, login.seq + 1));
    }

    /// S-77 (Rust half): a settle whose slot was re-bound after its entry
    /// read writes nothing and reports `Stale`; `auth_resolve` of no session
    /// answers `checked == current`.
    #[tokio::test]
    async fn s77_a_settle_overtaken_by_a_rebind_writes_nothing() {
        let state = app_state().await;
        let empty = resolve_body(&state).await.expect("resolve");
        assert!(empty.identity.is_none() && !empty.stale);
        assert_eq!((empty.checked, empty.current), (0, 0));

        let a = state
            .users
            .setup_first_user("a", SLOT_PASSWORD, "A")
            .await
            .unwrap();
        state.set_session_for_test(Some(DesktopSession::Account(a)));
        let (cached, seq_at_entry) = read_slot(&state);
        let cached = cached.unwrap();
        let fresh = read_session_source(&state, &cached).await.unwrap();
        // Another command re-binds the slot while the read was "in flight".
        assert!(cas_session(&state, seq_at_entry, None).0);

        assert!(matches!(
            settle_session(&state, &cached, fresh, seq_at_entry),
            Settled::Stale { valid_now: None }
        ));
        assert_eq!(read_slot(&state), (None, seq_at_entry + 1));
    }

    /// S-47 / S-67: auth-disabled mode's synthetic session resolves as
    /// `kind: "local"` with the mode's CURRENT role, and its logout is a
    /// no-op that does not advance seq.
    #[tokio::test]
    async fn s47_s67_the_auth_disabled_session_resolves_local_and_logout_keeps_seq() {
        let state = app_state().await;
        let set_mode = |role: Role| {
            let settings = state.settings.clone();
            async move {
                let config = settings.auth_config().await.unwrap();
                settings
                    .set_auth_config(&AuthSettings {
                        disabled: true,
                        disabled_role: role,
                        ..config
                    })
                    .await
                    .unwrap();
            }
        };
        set_mode(Role::Viewer).await;
        state.set_session_for_test(Some(DesktopSession::AuthDisabledLocal(UserIdentity {
            id: LOCAL_SESSION_ID,
            username: "local".to_string(),
            display_name: "ローカルユーザー".to_string(),
            role: Role::Viewer,
            auth_epoch: 0,
        })));
        let (_, seq) = read_slot(&state);

        let answer = resolve_body(&state).await.expect("resolve");
        assert_eq!(answer.kind, Some("local"));
        assert_eq!(answer.identity.expect("active").role, "viewer");
        set_mode(Role::Editor).await;
        let answer = resolve_body(&state).await.expect("resolve");
        assert_eq!(answer.kind, Some("local"));
        assert_eq!(answer.identity.expect("active").role, "editor");
        assert_eq!((answer.checked, answer.current), (seq, seq));

        let logout = logout_body(&state).await.expect("logout");
        assert_eq!(logout.seq, seq, "S-67: the no-op does not advance seq");
        assert!(read_slot(&state).0.is_some());
    }

    /// S-68: a self-service password change re-binds the session and
    /// advances seq; the result carries the new seq.
    #[tokio::test]
    async fn s68_change_password_rebinds_and_advances_seq() {
        let state = app_state().await;
        state
            .users
            .setup_first_user("a", SLOT_PASSWORD, "A")
            .await
            .unwrap();
        let login = login_body(&state, "a".to_string(), SLOT_PASSWORD.to_string())
            .await
            .expect("login");

        let result = change_own_password(&state, SLOT_PASSWORD, "newpassword1")
            .await
            .expect("change password");

        assert_eq!(result.seq, login.seq + 1);
        let (session, seq) = read_slot(&state);
        assert_eq!(seq, result.seq);
        assert!(matches!(session, Some(DesktopSession::Account(ref u)) if u.auth_epoch == 1));
        let answer = resolve_body(&state).await.expect("resolve");
        assert_eq!((answer.checked, answer.current), (result.seq, result.seq));
        assert!(answer.identity.is_some());
    }

    // admin-template の `drive_until!` は、ここでは移植しなかった
    // `s67_config_apply_installs_local_after_a_logout_advanced_seq` のほかに
    // `server_apply_is_serialized_with_auth_config_writes`（I2b）が使うので、
    // そちらのテスト群の先頭で定義している。

    /// Poll `$fut` for a while and assert it is still pending (waiting on
    /// `auth_config_lock`).
    macro_rules! assert_stays_pending {
        ($fut:expr) => {{
            for _ in 0..30 {
                tokio::select! {
                    biased;
                    out = &mut $fut => panic!("expected to wait, but finished: {out:?}"),
                    _ = tokio::task::yield_now() => {}
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }};
    }

    /// Hold the first `auth_config_apply` of `state` right AFTER its settings
    /// save (the apply still holds `auth_config_lock`).
    fn hold_after_save(state: &mut AppState) -> Hold {
        let (slot, hold) = gate();
        let settings = state.settings.clone();
        state.auth_io.save_auth_config = Arc::new(move |config| {
            let (slot, settings) = (slot.clone(), settings.clone());
            Box::pin(async move {
                settings.set_auth_config(&config).await?;
                pass_gate(&slot).await;
                Ok(())
            })
        });
        hold
    }

    /// Auth-mode reads for a logout that races an `apply(true)`: the FIRST
    /// read returns what is stored (`disabled = false`) and then stores
    /// `disabled = true` - an apply(true) that completed right after it -
    /// and the SECOND read (the logout's post-clear re-read, under
    /// `auth_config_lock`) is held before reading. Later reads pass.
    fn mode_turned_on_after_first_read_and_reread_held(state: &mut AppState) -> Hold {
        let (slot, hold) = gate();
        let settings = state.settings.clone();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        state.auth_io.auth_mode = Arc::new(move || {
            let (slot, settings, calls) = (slot.clone(), settings.clone(), calls.clone());
            Box::pin(async move {
                match calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) {
                    0 => {
                        let config = settings.auth_config().await?;
                        settings
                            .set_auth_config(&AuthSettings {
                                disabled: true,
                                ..config.clone()
                            })
                            .await?;
                        Ok(config)
                    }
                    1 => {
                        pass_gate(&slot).await;
                        settings.auth_config().await
                    }
                    _ => settings.auth_config().await,
                }
            })
        });
        hold
    }

    // admin-template の `s67_config_apply_installs_local_after_a_logout_advanced_seq`
    // は移植しない: 「セッションなし・アカウント 0 件の `auth_config_apply`」
    // （BOOTSTRAP WINDOW）から始まる順序で、ChronoGazer ではその apply が
    // `Unauthorized` になり到達できない。アカウントのセッションから始めても、
    // 先に完了した logout がそのセッションを消すので apply は `Unauthorized`
    // （正しい拒否）になり、同じ順序は作れない。

    /// S-67 / banto PR #264 review P1 (ii), updated by the owner review of banto #266
    /// P1: a logout reads the OLD `disabled = false` and is held;
    /// `auth_config_apply` then switches the mode on while A's session is
    /// still there - and re-binds it to the synthetic session right away
    /// (`seq` + 1). The logout resumes: its compare-and-set no longer holds,
    /// so it changes nothing. Disabled mode never ends with no session, nor
    /// with A's.
    #[tokio::test]
    async fn s67_a_logout_that_read_the_old_mode_is_overtaken_by_the_apply_rebind() {
        let mut state = app_state().await;
        state
            .users
            .setup_first_user("a", SLOT_PASSWORD, "A")
            .await
            .unwrap();
        let login = login_body(&state, "a".to_string(), SLOT_PASSWORD.to_string())
            .await
            .expect("login");
        assert!(login.success);
        // Call 1 = the logout's first mode read (held AFTER reading false).
        let mut hold = hold_auth_mode_call(&mut state, 1, true);

        let logout = logout_body(&state);
        tokio::pin!(logout);
        run_until_held!(logout, hold);

        let config = auth_config_apply_body(&state, true, "editor")
            .await
            .expect("apply");
        assert!(config.disabled);
        assert!(
            is_local(&read_slot(&state).0),
            "the apply re-bound A's session"
        );
        hold.release.send(()).unwrap();
        let result = logout.await.expect("logout");

        let (session, seq) = read_slot(&state);
        let Some(DesktopSession::AuthDisabledLocal(local)) = session else {
            panic!("expected the synthetic session: {session:?}")
        };
        assert_eq!(local.role, Role::Editor);
        assert_eq!(local.id, LOCAL_SESSION_ID);
        assert_eq!(
            seq,
            login.seq + 1,
            "the rebind only; the logout wrote nothing"
        );
        assert_eq!(result.seq, seq, "the logout reports the final seq");
        assert_eq!(
            audit_action_count(&state, "logout").await,
            1,
            "A's session end by the rebind (reason auth_disabled), none by the logout"
        );
        assert_eq!(
            audit_action_count(&state, "login").await,
            2,
            "A's login + one synthetic login"
        );
    }

    /// The audit rows for `action` (any order), as `(actor, detail)`.
    async fn audit_rows(state: &AppState, action: &str) -> Vec<(Option<String>, Option<String>)> {
        state
            .audit
            .list(ListParams::default())
            .await
            .expect("audit list")
            .rows
            .into_iter()
            .filter(|row| row.action == action)
            .map(|row| (row.actor_username, row.detail))
            .collect()
    }

    /// S-94 (owner review of banto #266 P1, design §5.3): an admin signed in with an
    /// account turns auth-disabled mode on from the settings screen. The
    /// apply re-binds the slot to the synthetic session with the new role in
    /// the same step as the save (`seq` + 1), and records the account's
    /// session end (`logout`, reason `auth_disabled`) and the synthetic
    /// `login`. `auth_resolve` then answers `kind: "local"` with that role -
    /// `auth.disabled == true` never goes with an account session.
    #[tokio::test]
    async fn s94_config_apply_true_rebinds_an_account_session_to_local() {
        let state = app_state().await;
        state
            .users
            .setup_first_user("admin", SLOT_PASSWORD, "Admin")
            .await
            .unwrap();
        let login = login_body(&state, "admin".to_string(), SLOT_PASSWORD.to_string())
            .await
            .expect("login");
        assert!(login.success);
        assert!(is_account(&read_slot(&state).0, "admin"));

        let config = auth_config_apply_body(&state, true, "viewer")
            .await
            .expect("apply");
        assert!(config.disabled);

        let (session, seq) = read_slot(&state);
        let Some(DesktopSession::AuthDisabledLocal(local)) = session else {
            panic!("expected the synthetic session: {session:?}")
        };
        assert_eq!(local.role, Role::Viewer);
        assert_eq!(seq, login.seq + 1, "Account -> Local advances seq once");

        let logouts = audit_rows(&state, "logout").await;
        assert_eq!(logouts.len(), 1);
        assert_eq!(logouts[0].0.as_deref(), Some("admin"));
        let detail: serde_json::Value =
            serde_json::from_str(logouts[0].1.as_deref().expect("detail")).unwrap();
        assert_eq!(detail, serde_json::json!({ "reason": "auth_disabled" }));
        let logins = audit_rows(&state, "login").await;
        assert_eq!(logins.len(), 2, "the admin's login + the synthetic login");
        assert!(
            logins
                .iter()
                .any(|(actor, detail)| actor.as_deref() == Some("local")
                    && detail
                        .as_deref()
                        .is_some_and(|d| d.contains("auth_disabled"))),
            "{logins:?}"
        );
        assert_eq!(audit_action_count(&state, "settings_change").await, 1);

        let answer = resolve_body(&state).await.expect("resolve");
        assert_eq!(answer.kind, Some("local"));
        assert_eq!(answer.identity.expect("active").role, "viewer");
        assert_eq!((answer.checked, answer.current), (seq, seq));
    }

    /// S-94/S-96: applying `disabled = true` again while the synthetic
    /// session is already there: with the same role nothing is written and
    /// `seq` does not move; with another role the role is written and `seq`
    /// advances (an authorization-context change, re-review of banto #266 P1). No
    /// session audit either way (the apply's `settings_change` records it).
    #[tokio::test]
    async fn s96_config_apply_true_again_keeps_seq_for_the_same_role_and_advances_it_for_another() {
        let state = app_state().await;
        enable_local_from_an_admin_session(&state, "admin").await;
        let (session, seq) = read_slot(&state);
        assert!(is_local(&session));
        let logins = audit_action_count(&state, "login").await;
        let logouts = audit_action_count(&state, "logout").await;

        auth_config_apply_body(&state, true, "admin")
            .await
            .expect("same apply");
        assert_eq!(
            read_slot(&state),
            (session, seq),
            "same role: nothing written"
        );

        auth_config_apply_body(&state, true, "editor")
            .await
            .expect("role change");
        let (session, seq_after) = read_slot(&state);
        assert_eq!(seq_after, seq + 1, "another role: seq advances");
        let Some(DesktopSession::AuthDisabledLocal(local)) = session else {
            panic!("{session:?}")
        };
        assert_eq!(local.role, Role::Editor);
        assert_eq!(audit_action_count(&state, "login").await, logins);
        assert_eq!(audit_action_count(&state, "logout").await, logouts);
        assert_eq!(audit_action_count(&state, "settings_change").await, 3);
    }

    /// S-96 (re-review of banto #266 P1): an `auth_resolve` of Local(admin) reads
    /// the slot and the store (role admin) and is held; the role is changed
    /// to viewer meanwhile; the held resolve then settles. The role change
    /// advanced `seq`, so the settle is `Stale` and writes nothing - it does
    /// not put admin back over viewer - and the next resolve answers viewer.
    /// The steps are `resolve_body`'s own, run one by one (as in S-77).
    #[tokio::test]
    async fn s96_a_resolve_that_read_the_old_local_role_does_not_write_it_back() {
        let state = app_state().await;
        enable_local_from_an_admin_session(&state, "admin").await;

        // The old resolve: slot and store read (admin), then held.
        let (cached, seq_at_entry) = read_slot(&state);
        let cached = cached.expect("Local(admin)");
        let fresh = read_session_source(&state, &cached).await.unwrap();
        assert!(matches!(
            &fresh,
            Some(DesktopSession::AuthDisabledLocal(local)) if local.role == Role::Admin
        ));

        auth_config_apply_body(&state, true, "viewer")
            .await
            .expect("role change");
        assert_eq!(read_slot(&state).1, seq_at_entry + 1);

        assert!(matches!(
            settle_session(&state, &cached, fresh, seq_at_entry),
            Settled::Stale { .. }
        ));
        let Some(DesktopSession::AuthDisabledLocal(local)) = read_slot(&state).0 else {
            panic!("expected Local")
        };
        assert_eq!(local.role, Role::Viewer, "admin was not written back");

        let answer = resolve_body(&state).await.expect("resolve");
        assert!(!answer.stale);
        assert_eq!(answer.kind, Some("local"));
        assert_eq!(answer.identity.expect("active").role, "viewer");
    }

    /// S-103 (freshness audit of banto #266, P2-2): the generic `settings_set`
    /// refuses the `auth.` keys (only `auth_config_apply` / `autologin_*`
    /// write them, with the session re-bind); other keys still work.
    #[tokio::test]
    async fn s103_settings_set_refuses_auth_keys() {
        let state = app_state().await;
        state
            .users
            .setup_first_user("admin", SLOT_PASSWORD, "Admin")
            .await
            .unwrap();
        let admin = state.users.get_by_username("admin").await.unwrap().unwrap();
        state.set_session_for_test(Some(DesktopSession::Account(admin.clone())));
        let before = read_slot(&state);
        for key in [
            "auth.disabled",
            "auth.disabled_role",
            "auth.autologin.enabled",
        ] {
            let result =
                settings_set_body(&state, &admin, key.to_string(), "true".to_string()).await;
            assert!(
                matches!(result, Err(BantoError::BadRequest(_))),
                "{key}: {result:?}"
            );
        }
        assert!(!state.settings.auth_config().await.unwrap().disabled);
        assert_eq!(read_slot(&state), before);
        settings_set_body(
            &state,
            &admin,
            "audit.retention_days".to_string(),
            "30".to_string(),
        )
        .await
        .expect("a non-auth key");
    }

    /// I2a（banto #278）: Tauri のログインの失敗も、REST と同じく監査ログの
    /// 名前を 32 文字に切り詰めて記録する（admin-template v3.0.0 の
    /// `login_body` と同じ形）。以前は送られてきた名前をそのまま書いていた。
    #[tokio::test]
    async fn i2a_an_oversized_failed_login_username_is_bounded_in_the_audit_log() {
        let state = app_state().await;
        state
            .users
            .setup_first_user("admin", SLOT_PASSWORD, "Admin")
            .await
            .unwrap();

        let result = login_body(&state, "y".repeat(10_000), "wrong-password".to_string())
            .await
            .expect("login");
        assert!(!result.success);

        let rows = audit_rows(&state, "login_failed").await;
        assert_eq!(rows.len(), 1, "{rows:?}");
        let recorded = rows[0].0.as_deref().expect("actor_username");
        assert_eq!(
            recorded.chars().count(),
            chronogazer_core::users::MAX_USERNAME_LEN
        );
        assert!(recorded.ends_with('…'), "{recorded}");
        // 上限以内の名前はそのまま（切り詰めは長すぎるときだけ）。
        login_body(&state, "admin".to_string(), "wrong-password".to_string())
            .await
            .expect("login");
        assert!(audit_rows(&state, "login_failed")
            .await
            .iter()
            .any(|(name, _)| name.as_deref() == Some("admin")));
    }

    /// I2b（PR #499 レビュー P2）: 汎用の `settings_set` は `server.*`
    /// （`enabled`・`bind`・`port`・`viewer_public`）を書けない。書けると `server_apply` の保存後の
    /// 失効と組み合わせ検証を通らず、OFF にしても発行済みの閲覧者のトークンが
    /// 生き残る。ON → 発行 → 汎用で OFF しようとすると拒否され、保存は ON の
    /// まま、トークンも有効なまま（状態が一致）。反証: `settings_set_body` の
    /// `is_server_key` の拒否を外すと、保存が変わって落ちる。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn settings_set_refuses_the_viewer_public_key() {
        let state = admin_app_state().await;
        let admin = state.users.get_by_username("admin").await.unwrap().unwrap();
        let port = free_port();
        apply(&state, true, port, true).await.expect("ON");
        let (status, body) = http_on(port, "POST", "/api/auth/grant/publicViewer", None)
            .await
            .expect("answers");
        assert_eq!(status, 200, "{body}");
        let body: serde_json::Value = serde_json::from_str(&body).unwrap();
        let viewer = body["token"].as_str().expect("token").to_string();

        let before = state.settings.server_config().await.unwrap();
        let attempts = [
            ("server.viewer_public", "false"),
            ("server.viewer_public", "true"),
            ("server.enabled", "false"),
            ("server.enabled", "true"),
            ("server.bind", "0.0.0.0"),
            ("server.port", "65000"),
        ];
        for (key, value) in attempts {
            let err = settings_set_body(&state, &admin, key.to_string(), value.to_string())
                .await
                .unwrap_err();
            assert!(matches!(err, BantoError::BadRequest(_)), "{key}: {err:?}");
        }

        let after = state.settings.server_config().await.unwrap();
        assert_eq!(after, before, "どのキーも保存されていない");
        assert!(after.viewer_public);
        assert!(
            state.rest_auth.verify(&viewer),
            "保存と同じくトークンも有効"
        );
        let (status, _) = http_on(port, "GET", "/api/tags", Some(&viewer))
            .await
            .unwrap();
        assert_eq!(status, 200);

        stop_running_server(&state).await;
    }

    /// I2a（banto #280）: Tauri のバックアップも DB ファイルごとの
    /// `<DB の親>/backups/<DB ファイル名>/` に作られ、「フォルダを開く」が
    /// 開く場所（`backups_dir_display`）もそこ。
    #[tokio::test]
    async fn i2a_backups_are_created_in_the_per_database_directory() {
        let (dir, state) = app_state_with_tempdir().await;
        let created = state.backup.create().await.expect("create");
        let scope = dir.path().join("backups").join("chronogazer.sqlite3");
        assert!(scope.join(&created.file_name).is_file());
        assert_eq!(
            state.backup.backups_dir_display(),
            scope.display().to_string()
        );
    }

    /// Replace `auth_config_apply`'s save with one that stores only
    /// `auth.disabled` and then fails (a save interrupted part-way).
    fn save_fails_after_the_mode(state: &mut AppState) {
        let settings = state.settings.clone();
        state.auth_io.save_auth_config = Arc::new(move |config| {
            let settings = settings.clone();
            Box::pin(async move {
                settings
                    .set(
                        "auth.disabled",
                        if config.disabled { "true" } else { "false" },
                    )
                    .await?;
                Err(BantoError::Storage("disk full (test)".to_string()))
            })
        });
    }

    /// S-104 (freshness audit of banto #266, P2-3): a save that stored the mode and
    /// then failed still leaves the session following what is stored -
    /// apply(true) over an account session re-binds it to Local; apply(false)
    /// over Local ends it - and the error is reported.
    #[tokio::test]
    async fn s104_a_save_failing_part_way_still_rebinds_the_session_to_what_is_stored() {
        let mut state = app_state().await;
        state
            .users
            .setup_first_user("admin", SLOT_PASSWORD, "Admin")
            .await
            .unwrap();
        state.set_session_for_test(Some(DesktopSession::Account(
            state.users.get_by_username("admin").await.unwrap().unwrap(),
        )));
        save_fails_after_the_mode(&mut state);
        let (_, seq) = read_slot(&state);

        let result = auth_config_apply_body(&state, true, "admin").await;
        assert!(matches!(result, Err(BantoError::Storage(_))), "{result:?}");
        assert!(state.settings.auth_config().await.unwrap().disabled);
        let (session, seq_on) = read_slot(&state);
        assert!(is_local(&session), "follows disabled = true: {session:?}");
        assert_eq!(seq_on, seq + 1);

        let result = auth_config_apply_body(&state, false, "admin").await;
        assert!(result.is_err());
        assert!(!state.settings.auth_config().await.unwrap().disabled);
        assert_eq!(
            read_slot(&state),
            (None, seq_on + 1),
            "follows disabled = false"
        );
    }

    /// S-102 (freshness audit of banto #266, P3-2): a check that read Local(admin)
    /// and is overtaken by apply(true, viewer) reports, for the ordinary
    /// commands, the slot's CURRENT session (viewer) - not its older read.
    #[tokio::test]
    async fn s102_a_stale_settle_reports_the_slot_value_not_the_older_read() {
        let state = app_state().await;
        enable_local_from_an_admin_session(&state, "admin").await;
        let (cached, seq_at_entry) = read_slot(&state);
        let cached = cached.expect("Local(admin)");
        let fresh = read_session_source(&state, &cached).await.unwrap();

        auth_config_apply_body(&state, true, "viewer")
            .await
            .expect("role change");

        let Settled::Stale {
            valid_now: Some(DesktopSession::AuthDisabledLocal(local)),
        } = settle_session(&state, &cached, fresh, seq_at_entry)
        else {
            panic!("expected Stale with the current Local session")
        };
        assert_eq!(local.role, Role::Viewer);
    }

    /// Hardening (freshness audit of banto #266, P2-2): `false -> true` advances
    /// `seq` even if the slot somehow already holds Local with that role.
    ///
    /// admin-template はこれを BOOTSTRAP WINDOW の `auth_config_apply` で
    /// 確かめるが、ChronoGazer ではモードがオフの間 `Local` のセッションは
    /// `require_role` の照合で消える（`Unauthorized`）ので、その apply には
    /// 到達できない。判定そのもの（apply が保存の直後に呼ぶ
    /// [`apply_mode_to_session`]）を直接確かめる。
    #[tokio::test]
    async fn s98_turning_the_mode_on_always_advances_seq() {
        let state = app_state().await;
        state.set_session_for_test(Some(DesktopSession::AuthDisabledLocal(local_identity(
            Role::Admin,
        ))));
        let (_, seq) = read_slot(&state);
        let previous = AuthSettings {
            disabled: false,
            disabled_role: Role::Admin,
            ..AuthSettings::default()
        };
        let saved = AuthSettings {
            disabled: true,
            ..previous.clone()
        };
        let (rebind, ended) = apply_mode_to_session(&state, &previous, &saved);
        assert_eq!(rebind, LocalRebind::RoleChanged);
        assert!(ended.is_none());
        assert_eq!(read_slot(&state).1, seq + 1);

        // Control: the same Local with nothing changed in the save keeps seq.
        let (rebind, _) = apply_mode_to_session(&state, &saved, &saved);
        assert_eq!(rebind, LocalRebind::Unchanged);
        assert_eq!(read_slot(&state).1, seq + 1);
    }

    /// S-98 (re-review of banto #266 P1): between `auth_config_apply`'s save and
    /// its rebind, another `auth_resolve` (B) reads the new role from the
    /// store and refreshes the slot to it - without advancing `seq` (a
    /// settle's refresh never does, I-23). The rebind then finds the new role
    /// already there, but the role DID change in this apply (decided from
    /// the settings before the save), so it still advances `seq`. An even
    /// older resolve (A) that had read the old role therefore settles
    /// `Stale` and does not write admin back.
    #[tokio::test]
    async fn s98_a_role_change_advances_seq_even_if_a_settle_refreshed_the_slot_first() {
        let mut state = app_state().await;
        // Local(admin): the mode on with role admin, and its synthetic session.
        let config = state.settings.auth_config().await.unwrap();
        state
            .settings
            .set_auth_config(&AuthSettings {
                disabled: true,
                disabled_role: Role::Admin,
                ..config
            })
            .await
            .unwrap();
        state.set_session_for_test(Some(DesktopSession::AuthDisabledLocal(local_identity(
            Role::Admin,
        ))));
        let mut hold = hold_after_save(&mut state);

        // Resolve A: slot and store read (admin), then held.
        let (cached_a, seq_n) = read_slot(&state);
        let cached_a = cached_a.expect("Local(admin)");
        let fresh_a = read_session_source(&state, &cached_a).await.unwrap();

        // apply(true, viewer): held after its save, before its rebind.
        let apply = auth_config_apply_body(&state, true, "viewer");
        tokio::pin!(apply);
        run_until_held!(apply, hold);

        // Resolve B runs whole: it reads viewer from the store and refreshes.
        let b = resolve_body(&state).await.expect("resolve B");
        assert_eq!(b.identity.expect("active").role, "viewer");
        assert_eq!((b.checked, b.current), (seq_n, seq_n), "a refresh: no seq");
        assert_eq!(read_slot(&state).1, seq_n);

        hold.release.send(()).unwrap();
        apply.await.expect("apply");
        assert_eq!(
            read_slot(&state).1,
            seq_n + 1,
            "the role change advanced seq anyway"
        );

        assert!(matches!(
            settle_session(&state, &cached_a, fresh_a, seq_n),
            Settled::Stale { .. }
        ));
        let Some(DesktopSession::AuthDisabledLocal(local)) = read_slot(&state).0 else {
            panic!("expected Local")
        };
        assert_eq!(local.role, Role::Viewer, "admin was not written back");
    }

    /// S-99 (re-review of banto #266 P1): an `auth_resolve` that read Local(admin)
    /// and `disabled = true` is held; `auth_config_apply(false)` then ends the
    /// synthetic session in the same step as its save (`None`, `seq` + 1) and
    /// records its end (`logout`, reason `auth_enabled`). The held resolve
    /// settles `Stale` (it does not confirm `local` under `disabled = false`),
    /// and the next resolve answers `none`.
    #[tokio::test]
    async fn s99_config_apply_false_ends_local_at_once_and_an_old_resolve_is_stale() {
        let state = app_state().await;
        enable_local_from_an_admin_session(&state, "admin").await;
        let (cached, seq_n) = read_slot(&state);
        let cached = cached.expect("Local(admin)");
        let fresh = read_session_source(&state, &cached).await.unwrap();
        assert!(fresh.is_some(), "read while the mode was on");

        auth_config_apply_body(&state, false, "admin")
            .await
            .expect("disable");
        assert_eq!(read_slot(&state), (None, seq_n + 1));
        // The admin's session end when the mode was turned on is the other
        // `logout` (reason `auth_disabled`, see
        // `enable_local_from_an_admin_session`); only Local's is asserted.
        let logouts: Vec<_> = audit_rows(&state, "logout")
            .await
            .into_iter()
            .filter(|(actor, _)| actor.as_deref() == Some("local"))
            .collect();
        assert_eq!(logouts.len(), 1);
        let detail: serde_json::Value =
            serde_json::from_str(logouts[0].1.as_deref().expect("detail")).unwrap();
        assert_eq!(detail, serde_json::json!({ "reason": "auth_enabled" }));

        assert!(matches!(
            settle_session(&state, &cached, fresh, seq_n),
            Settled::Stale { .. }
        ));
        assert_eq!(read_slot(&state), (None, seq_n + 1));
        let answer = resolve_body(&state).await.expect("resolve");
        assert!(answer.identity.is_none());
        assert!(!answer.stale);
        assert_eq!((answer.checked, answer.current), (seq_n + 1, seq_n + 1));
    }

    /// S-99: re-saving `disabled = false` (the mode already off) touches
    /// nothing: `seq` unchanged, no session audit.
    #[tokio::test]
    async fn s99_config_apply_false_again_is_a_no_op_for_the_session() {
        let state = app_state().await;
        state
            .users
            .setup_first_user("admin", SLOT_PASSWORD, "Admin")
            .await
            .unwrap();
        let login = login_body(&state, "admin".to_string(), SLOT_PASSWORD.to_string())
            .await
            .expect("login");
        assert!(login.success);
        let before = read_slot(&state);

        auth_config_apply_body(&state, false, "admin")
            .await
            .expect("re-save false");
        assert_eq!(read_slot(&state), before);
        assert!(is_account(&before.0, "admin"));
        assert_eq!(audit_action_count(&state, "logout").await, 0);
    }

    /// S-94/S-95: a login starts (reads `seq = N`) and is held in its
    /// password check; the mode is switched on meanwhile (the admin's account
    /// session re-bound to Local, `seq = N + 1`); the login then completes.
    /// Its install checks the mode under `auth_config_lock` and refuses - no
    /// account session in auth-disabled mode (and its compare-and-set would
    /// not hold either). The synthetic session stays; `seq` does not move.
    #[tokio::test]
    async fn s95_a_login_whose_check_was_overtaken_by_apply_true_is_refused() {
        let mut state = app_state().await;
        state
            .users
            .setup_first_user("admin", SLOT_PASSWORD, "Admin")
            .await
            .unwrap();
        state.set_session_for_test(Some(DesktopSession::Account(
            state.users.get_by_username("admin").await.unwrap().unwrap(),
        )));
        let (_, seq_before) = read_slot(&state);
        let mut hold = hold_verify(&mut state);

        let login = login_body(&state, "admin".to_string(), SLOT_PASSWORD.to_string());
        tokio::pin!(login);
        run_until_held!(login, hold);

        auth_config_apply_body(&state, true, "viewer")
            .await
            .expect("apply");
        assert_eq!(read_slot(&state).1, seq_before + 1);
        hold.release.send(()).unwrap();
        let result = login.await.expect("login");

        assert!(!result.success);
        assert!(!result.superseded, "refused by the mode, not a lost race");
        assert_eq!(result.error.as_deref(), Some(AUTH_DISABLED_LOGIN_MESSAGE));
        let (session, seq) = read_slot(&state);
        assert!(
            is_local(&session),
            "Local is not turned back into the account: {session:?}"
        );
        assert_eq!(seq, seq_before + 1);
        assert_eq!(result.seq, seq);
    }

    /// S-95: `auth_login` while auth-disabled mode is on is refused before the
    /// password check: the slot stays Local, `seq` does not move, and nothing
    /// is recorded (no credential was checked).
    #[tokio::test]
    async fn s95_login_in_auth_disabled_mode_is_refused_before_the_check() {
        let state = app_state().await;
        state
            .users
            .setup_first_user("admin", SLOT_PASSWORD, "Admin")
            .await
            .unwrap();
        state.set_session_for_test(Some(DesktopSession::Account(
            state.users.get_by_username("admin").await.unwrap().unwrap(),
        )));
        auth_config_apply_body(&state, true, "editor")
            .await
            .expect("apply");
        let before = read_slot(&state);
        let logins = audit_action_count(&state, "login").await;

        let result = login_body(&state, "admin".to_string(), SLOT_PASSWORD.to_string())
            .await
            .expect("login");

        assert!(!result.success);
        assert!(!result.superseded);
        assert_eq!(result.error.as_deref(), Some(AUTH_DISABLED_LOGIN_MESSAGE));
        assert_eq!(result.seq, before.1);
        assert_eq!(read_slot(&state), before);
        assert!(is_local(&before.0));
        assert_eq!(audit_action_count(&state, "login").await, logins);
        assert_eq!(audit_action_count(&state, "login_failed").await, 0);
    }

    /// S-95: `auth_setup` while auth-disabled mode is on (the first-run
    /// 「ログインなしで使い始める」 already chosen) is refused on entry and
    /// creates no account.
    #[tokio::test]
    async fn s95_setup_in_auth_disabled_mode_creates_no_account() {
        let state = app_state().await;
        // ChronoGazer has no bootstrap window: with no account yet, the mode
        // and its synthetic session are set up directly (as `run()`'s
        // bootstrap would on a launch with `auth.disabled = true`).
        let config = state.settings.auth_config().await.unwrap();
        state
            .settings
            .set_auth_config(&AuthSettings {
                disabled: true,
                ..config
            })
            .await
            .unwrap();
        state.set_session_for_test(Some(DesktopSession::AuthDisabledLocal(local_identity(
            Role::Admin,
        ))));
        let before = read_slot(&state);
        assert!(is_local(&before.0));

        let result = setup_body(
            &state,
            "b".to_string(),
            SLOT_PASSWORD.to_string(),
            "B".to_string(),
        )
        .await
        .expect("setup");

        assert!(!result.success);
        assert!(!result.superseded);
        assert_eq!(result.error.as_deref(), Some(AUTH_DISABLED_LOGIN_MESSAGE));
        assert_eq!(read_slot(&state), before);
        assert!(
            !state.users.is_initialized().await.unwrap(),
            "no account created"
        );
        assert_eq!(audit_action_count(&state, "setup").await, 0);
    }

    // admin-template の `s95_a_setup_overtaken_by_apply_true_keeps_the_account_but_installs_nothing`
    // は移植しない: 割り込む `auth_config_apply(true)` が BOOTSTRAP WINDOW
    // （アカウント 0 件・セッションなし）頼み。ChronoGazer ではアカウント 0 件の
    // 間にモードを入れる apply は、セッションが無いので `Unauthorized`、既に
    // モードがオンなら `setup` が入口で断る
    // （`s95_setup_in_auth_disabled_mode_creates_no_account`）ので、この順序は
    // 起きない。

    /// banto PR #264 review P1 / re-review P2: a logout whose first read saw
    /// `disabled = false` (and after which the mode was switched on) clears
    /// A and re-reads the mode under `auth_config_lock`; an
    /// `auth_config_apply(true)` started meanwhile waits for that lock. The
    /// logout installs the synthetic session, and the apply - seeing it -
    /// does not install again: one `seq` step, one synthetic `login`.
    #[tokio::test]
    async fn s67_config_apply_and_logout_install_local_only_once() {
        let mut state = app_state().await;
        state
            .users
            .setup_first_user("a", SLOT_PASSWORD, "A")
            .await
            .unwrap();
        let login = login_body(&state, "a".to_string(), SLOT_PASSWORD.to_string())
            .await
            .expect("login");
        let mut hold = mode_turned_on_after_first_read_and_reread_held(&mut state);

        let logout = logout_body(&state);
        tokio::pin!(logout);
        run_until_held!(logout, hold);
        assert_eq!(read_slot(&state), (None, login.seq + 1), "A is cleared");

        let apply = auth_config_apply_body(&state, true, "admin");
        tokio::pin!(apply);
        assert_stays_pending!(apply);
        hold.release.send(()).unwrap();
        let result = logout.await.expect("logout");
        assert!(is_local(&read_slot(&state).0), "the logout installed");
        assert_eq!(result.seq, login.seq + 2);
        apply.await.expect("apply");

        let (session, seq) = read_slot(&state);
        assert!(is_local(&session), "{session:?}");
        assert_eq!(seq, login.seq + 2, "installed once");
        assert_eq!(
            audit_action_count(&state, "login").await,
            2,
            "A's login + one synthetic login"
        );
    }

    /// banto PR #264 re-review P2 (a): `apply(true)` has saved and is still inside
    /// its decision (held after the save, `auth_config_lock` held) when an
    /// `apply(false)` starts. The second apply waits for the first to finish
    /// - it cannot save `false` between the first one's save and its install
    /// - so the install is based on the value still stored, and the final
    /// `disabled = false` is saved after it. Since S-99 that `apply(false)`
    /// ends the local session itself, in the same step as its save.
    #[tokio::test]
    async fn config_apply_true_then_false_are_serialized_by_the_auth_config_lock() {
        let mut state = app_state().await;
        // admin-template starts from no session (bootstrap window); here the
        // first apply is an admin's, so the slot starts at `Account`, seq 1.
        let admin = state
            .users
            .setup_first_user("admin", SLOT_PASSWORD, "Admin")
            .await
            .unwrap();
        state.set_session_for_test(Some(DesktopSession::Account(admin)));
        let mut hold = hold_after_save(&mut state);

        let apply_on = auth_config_apply_body(&state, true, "admin");
        tokio::pin!(apply_on);
        run_until_held!(apply_on, hold);

        let apply_off = auth_config_apply_body(&state, false, "admin");
        tokio::pin!(apply_off);
        assert_stays_pending!(apply_off);
        assert!(
            state.settings.auth_config().await.unwrap().disabled,
            "apply(false) is waiting: it has not saved"
        );
        let (session, seq) = read_slot(&state);
        assert!(
            is_account(&session, "admin"),
            "not re-bound yet: {session:?}"
        );
        assert_eq!(seq, 1);

        hold.release.send(()).unwrap();
        apply_on.await.expect("apply(true)");
        assert!(
            is_local(&read_slot(&state).0),
            "installed under disabled = true"
        );
        let config = apply_off.await.expect("apply(false)");

        assert!(!config.disabled);
        assert!(!state.settings.auth_config().await.unwrap().disabled);
        assert_eq!(read_slot(&state), (None, 3), "apply(false) ended it (S-99)");
        let answer = resolve_body(&state).await.expect("resolve");
        assert!(answer.identity.is_none());
        assert_eq!((answer.checked, answer.current), (3, 3));
        assert!(current_session(&state).await.unwrap().is_none());
    }

    /// banto PR #264 re-review P2 (b): a logout's post-clear re-read sees
    /// `disabled = true` and is held (under `auth_config_lock`); an
    /// `apply(false)` started then waits for the logout's install to finish
    /// before it saves. Final: `disabled = false`, and the local session the
    /// logout installed under `true` is ended by that `apply(false)` (S-99).
    #[tokio::test]
    async fn s67_a_logout_reread_and_a_later_apply_false_are_serialized() {
        let mut state = app_state().await;
        state
            .users
            .setup_first_user("a", SLOT_PASSWORD, "A")
            .await
            .unwrap();
        let login = login_body(&state, "a".to_string(), SLOT_PASSWORD.to_string())
            .await
            .expect("login");
        let mut hold = mode_turned_on_after_first_read_and_reread_held(&mut state);

        let logout = logout_body(&state);
        tokio::pin!(logout);
        run_until_held!(logout, hold);

        let apply_off = auth_config_apply_body(&state, false, "admin");
        tokio::pin!(apply_off);
        assert_stays_pending!(apply_off);
        assert!(
            state.settings.auth_config().await.unwrap().disabled,
            "apply(false) is waiting: it has not saved"
        );
        hold.release.send(()).unwrap();
        let result = logout.await.expect("logout");
        assert!(
            is_local(&read_slot(&state).0),
            "installed under disabled = true"
        );
        assert_eq!(result.seq, login.seq + 2);
        apply_off.await.expect("apply(false)");

        assert!(!state.settings.auth_config().await.unwrap().disabled);
        assert_eq!(
            read_slot(&state),
            (None, login.seq + 3),
            "apply(false) ended it (S-99)"
        );
        let answer = resolve_body(&state).await.expect("resolve");
        assert!(answer.identity.is_none());
        assert_eq!(answer.current, answer.checked);
    }

    /// banto PR #264 review P1: a logout whose re-read of the mode fails still
    /// succeeds (the clear has happened) and installs nothing.
    #[tokio::test]
    async fn s67_a_logout_whose_mode_reread_fails_still_succeeds() {
        let mut state = app_state().await;
        let a = state
            .users
            .setup_first_user("a", SLOT_PASSWORD, "A")
            .await
            .unwrap();
        state.set_session_for_test(Some(DesktopSession::Account(a)));
        let settings = state.settings.clone();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        state.auth_io.auth_mode = Arc::new(move || {
            let (settings, calls) = (settings.clone(), calls.clone());
            Box::pin(async move {
                if calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                    settings.auth_config().await
                } else {
                    Err(BantoError::Storage("injected".to_string()))
                }
            })
        });

        let result = logout_body(&state).await.expect("logout still succeeds");

        assert_eq!(result.seq, 2);
        assert_eq!(read_slot(&state), (None, 2));
        assert_eq!(audit_action_count(&state, "logout").await, 1);
    }

    /// Supplementary (design §8.3, not order-fixing): login and logout
    /// started together, both having read seq before either proceeds past
    /// its injected `.await`, end in exactly one of the two consistent
    /// states.
    #[tokio::test]
    async fn concurrent_login_and_logout_end_in_one_of_two_consistent_states() {
        for _ in 0..3 {
            let mut state = app_state().await;
            state
                .users
                .setup_first_user("b", SLOT_PASSWORD, "B")
                .await
                .unwrap();
            let barrier = Arc::new(tokio::sync::Barrier::new(2));
            let (verify_barrier, users) = (barrier.clone(), state.users.clone());
            state.auth_io.verify = Arc::new(move |username, password| {
                let (barrier, users) = (verify_barrier.clone(), users.clone());
                Box::pin(async move {
                    barrier.wait().await;
                    users.verify(&username, &password).await
                })
            });
            let (mode_barrier, settings) = (barrier.clone(), state.settings.clone());
            // Only the logout's FIRST mode read meets the login at the
            // barrier - a logout that cleared re-reads the mode (banto PR #264
            // review P1), and that second read has no partner.
            let first_read = Arc::new(std::sync::atomic::AtomicBool::new(true));
            state.auth_io.auth_mode = Arc::new(move || {
                let (barrier, settings, first_read) =
                    (mode_barrier.clone(), settings.clone(), first_read.clone());
                Box::pin(async move {
                    if first_read.swap(false, std::sync::atomic::Ordering::SeqCst) {
                        barrier.wait().await;
                    }
                    settings.auth_config().await
                })
            });

            let (login, logout) = tokio::join!(
                login_body(&state, "b".to_string(), SLOT_PASSWORD.to_string()),
                logout_body(&state)
            );
            let (login, logout) = (login.unwrap(), logout.unwrap());
            match read_slot(&state).0 {
                None => assert!(login.superseded, "logged out, so the login was superseded"),
                Some(_) => {
                    assert!(login.success);
                    assert_eq!(logout.seq, login.seq, "the logout was a no-op");
                }
            }
        }
    }

    /// S-55 (Rust half): a password change from a session that was revoked
    /// meanwhile clears the slot (advancing `seq`) and THEN fails with
    /// `Unauthorized` - the one error kind returned after a slot write, which
    /// the TS provider therefore treats as "the revision may have changed".
    #[tokio::test]
    async fn s55_change_password_on_a_revoked_session_advances_seq_then_fails_unauthorized() {
        let state = app_state().await;
        state
            .users
            .setup_first_user("owner", SLOT_PASSWORD, "オーナー")
            .await
            .unwrap();
        let a = state
            .users
            .create_user("a", SLOT_PASSWORD, "A", Role::Editor)
            .await
            .unwrap();
        let login = login_body(&state, "a".to_string(), SLOT_PASSWORD.to_string())
            .await
            .expect("login");
        state
            .users
            .update_user(a.id, "A", Role::Viewer)
            .await
            .unwrap();

        let result = change_own_password(&state, SLOT_PASSWORD, "newpassword1").await;

        assert!(
            matches!(result, Err(BantoError::Unauthorized)),
            "{result:?}"
        );
        assert_eq!(read_slot(&state), (None, login.seq + 1));
    }

    /// S-55 (Rust half): a wrong current password is a `Validation` error
    /// returned without touching the slot.
    #[tokio::test]
    async fn s55_change_password_with_a_wrong_current_password_leaves_seq() {
        let state = app_state().await;
        state
            .users
            .setup_first_user("a", SLOT_PASSWORD, "A")
            .await
            .unwrap();
        let login = login_body(&state, "a".to_string(), SLOT_PASSWORD.to_string())
            .await
            .expect("login");

        let result = change_own_password(&state, "wrong-password", "newpassword1").await;

        assert!(
            matches!(result, Err(BantoError::Validation { .. })),
            "{result:?}"
        );
        assert_eq!(read_slot(&state).1, login.seq);
        assert!(read_slot(&state).0.is_some());
    }

    // --- banto v2.0.0 移行（PR1a 監査の追補）---------------------------------

    /// オーナー決定（2026-10-02）: ログイン不要モードの `disabled_role` は残し、
    /// キオスク（`viewer`）から UI で admin に戻れること。ESCAPE HATCH
    /// （モードが今オンなら `auth_config_apply` は役割を問わず通る）を固定する:
    /// admin の Account → apply(true, viewer) で Local(viewer)・seq + 1 →
    /// **viewer のまま** apply(true, admin) が通って Local(admin)・seq + 1 →
    /// viewer に戻して apply(false) が通って Local → None・seq + 1。Local(viewer)
    /// は `auth_config_get` を読めて（画面表示に要る）、生の `settings_get` は
    /// `Forbidden`。
    #[tokio::test]
    async fn kiosk_viewer_can_raise_its_role_and_turn_the_mode_off() {
        let state = app_state().await;
        let admin = state
            .users
            .setup_first_user("admin", SLOT_PASSWORD, "Admin")
            .await
            .unwrap();
        state.set_session_for_test(Some(DesktopSession::Account(admin)));
        let (_, seq0) = read_slot(&state);

        let local_role = |state: &AppState| match read_slot(state).0 {
            Some(DesktopSession::AuthDisabledLocal(local)) => local.role,
            other => panic!("expected Local, got {other:?}"),
        };

        auth_config_apply_body(&state, true, "viewer")
            .await
            .expect("admin turns the kiosk mode on");
        assert_eq!(local_role(&state), Role::Viewer);
        assert_eq!(read_slot(&state).1, seq0 + 1);

        // The kiosk's viewer session can read the auth mode (the security
        // screen shows it) but not the raw settings table.
        let config = auth_config_get_body(&state)
            .await
            .expect("viewer reads auth_config");
        assert!(config.disabled);
        assert!(matches!(
            settings_get_body(&state, "auth.disabled").await,
            Err(BantoError::Forbidden)
        ));

        // Still viewer: the escape hatch lets it raise the role to admin.
        auth_config_apply_body(&state, true, "admin")
            .await
            .expect("viewer raises the role (escape hatch)");
        assert_eq!(local_role(&state), Role::Admin);
        assert_eq!(read_slot(&state).1, seq0 + 2);
        assert_eq!(
            settings_get_body(&state, "auth.disabled")
                .await
                .expect("admin reads settings")
                .as_deref(),
            Some("true")
        );

        // Back to viewer, then viewer turns the mode off.
        auth_config_apply_body(&state, true, "viewer")
            .await
            .expect("back to viewer");
        assert_eq!(local_role(&state), Role::Viewer);
        let (_, seq_viewer) = read_slot(&state);
        auth_config_apply_body(&state, false, "viewer")
            .await
            .expect("viewer turns the mode off (escape hatch)");
        assert!(!state.settings.auth_config().await.unwrap().disabled);
        assert_eq!(read_slot(&state), (None, seq_viewer + 1));

        // With the mode off and no session, the escape hatch is closed.
        assert!(matches!(
            auth_config_apply_body(&state, true, "admin").await,
            Err(BantoError::Unauthorized)
        ));
    }

    /// `apply_mode_to_session` from an empty slot (`false -> true`):
    /// `Installed`, seq + 1 (the shape of
    /// `s98_turning_the_mode_on_always_advances_seq`).
    #[tokio::test]
    async fn turning_the_mode_on_with_no_session_installs_local() {
        let state = app_state().await;
        let (_, seq) = read_slot(&state);
        let previous = AuthSettings {
            disabled: false,
            disabled_role: Role::Editor,
            ..AuthSettings::default()
        };
        let saved = AuthSettings {
            disabled: true,
            ..previous.clone()
        };
        let (rebind, ended) = apply_mode_to_session(&state, &previous, &saved);
        assert!(
            matches!(&rebind, LocalRebind::Installed(local) if local.role == Role::Editor),
            "{rebind:?}"
        );
        assert!(ended.is_none());
        let (session, seq_after) = read_slot(&state);
        assert!(is_local(&session));
        assert_eq!(seq_after, seq + 1);
    }

    /// admin-template の
    /// `a_desktop_session_rebound_while_its_check_was_in_flight_stays_valid`
    /// の移植（banto v1.7.0 #204 の review of #230 + banto #260）: 確認の
    /// 読み取りが飛行中に、このセッション自身のパスワード変更が rebind した
    /// 場合、その確認は `Stale { valid_now: Some(Account) }` で有効のまま。
    /// rebind されていない別ウィンドウの同じアカウントのセッションは、DB の
    /// 新しい epoch を代わりに採用されず終わる。
    #[tokio::test]
    async fn a_desktop_session_rebound_while_its_check_was_in_flight_stays_valid() {
        let pool = chronogazer_core::db::init_db_memory()
            .await
            .expect("init_db_memory");
        let changer = app_state_on(pool.clone()).await;
        let other = app_state_on(pool).await;
        changer
            .users
            .setup_first_user("target", SLOT_PASSWORD, "Target")
            .await
            .unwrap();
        for state in [&changer, &other] {
            let login = login_body(state, "target".to_string(), SLOT_PASSWORD.to_string())
                .await
                .expect("login");
            assert!(login.success);
        }
        let snapshot = |state: &AppState| {
            let (session, seq) = read_slot(state);
            (session.expect("a session"), seq)
        };
        let (changer_before, changer_seq) = snapshot(&changer);
        let (other_before, other_seq) = snapshot(&other);

        change_own_password(&changer, SLOT_PASSWORD, "newpassword1")
            .await
            .expect("password change");

        let fresh = read_session_source(&changer, &changer_before)
            .await
            .unwrap();
        match settle_session(&changer, &changer_before, fresh, changer_seq) {
            Settled::Stale {
                valid_now: Some(DesktopSession::Account(user)),
            } => assert_eq!(user.auth_epoch, 1),
            other => panic!("the re-bound session must stay valid, got {other:?}"),
        }
        assert!(require_role(&changer, Role::Admin, "users").await.is_ok());

        let fresh = read_session_source(&other, &other_before).await.unwrap();
        assert!(matches!(
            settle_session(&other, &other_before, fresh, other_seq),
            Settled::Settled { session: None, .. }
        ));
        assert!(other
            .auth
            .lock()
            .expect("auth mutex poisoned")
            .session
            .is_none());
    }

    // ----- `server_apply_body`（I2b: banto #287 / #288 / #294 review） -----
    //
    // admin-template v3.0.0 の同名のテスト群の写し（`free_port`・
    // `answers_http`・`port_is_free`・`drive_until!` も）に、ChronoGazer の
    // 組み込みサーバー（`chronogazer_core::rest::api_router`）を通した閲覧公開の
    // 往復を足したもの。すべて loopback 上で、ポートは OS に選ばせてから
    // 解放したものを使う。順序はポートの占有と `auth_config_lock` を握る
    // ことで決め、sleep には頼らない。

    /// Poll `$fut` until `$cond` holds (the future must not finish first).
    macro_rules! drive_until {
        ($fut:expr, $cond:expr) => {{
            let mut reached = false;
            for _ in 0..5_000 {
                if $cond {
                    reached = true;
                    break;
                }
                tokio::select! {
                    biased;
                    out = &mut $fut => panic!("finished early: {out:?}"),
                    _ = tokio::task::yield_now() => {}
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            assert!(reached, "the condition never held");
        }};
    }

    /// A currently free loopback port (picked by the OS, released again).
    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .expect("bind free port")
            .local_addr()
            .expect("local_addr")
            .port()
    }

    /// Whether `port` can be bound again, i.e. no listener holds it.
    fn port_is_free(port: u16) -> bool {
        std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
    }

    /// One HTTP/1.1 request to `127.0.0.1:port`; `None` when nothing
    /// answers, else (status, body).
    async fn http_on(
        port: u16,
        method: &str,
        path: &str,
        token: Option<&str>,
    ) -> Option<(u16, String)> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .ok()?;
        let mut request = format!(
            "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\
             X-Banto-Client: banto\r\nContent-Type: application/json\r\nContent-Length: 2\r\n"
        );
        if let Some(token) = token {
            request.push_str(&format!("Authorization: Bearer {token}\r\n"));
        }
        request.push_str("\r\n{}");
        stream.write_all(request.as_bytes()).await.ok()?;
        let mut response = Vec::new();
        let _ = stream.read_to_end(&mut response).await;
        let text = String::from_utf8_lossy(&response).into_owned();
        let status = text.split_whitespace().nth(1)?.parse().ok()?;
        let body = text
            .split_once("\r\n\r\n")
            .map(|(_, body)| body.to_string())
            .unwrap_or_default();
        Some((status, body))
    }

    /// Whether something answers HTTP on `127.0.0.1:port` (any status).
    async fn answers_http(port: u16) -> bool {
        http_on(port, "GET", "/api/auth/check", None)
            .await
            .is_some()
    }

    /// A state whose desktop session is the first (admin) account.
    async fn admin_app_state() -> AppState {
        let state = app_state().await;
        let admin = state
            .users
            .setup_first_user("admin", "password123", "管理者")
            .await
            .expect("setup_first_user");
        state.set_session_for_test(Some(DesktopSession::Account(admin)));
        state
    }

    /// Stop the server `server_apply_body` left running (if any).
    async fn stop_running_server(state: &AppState) {
        if let Some(running) = state.server.lock().await.take() {
            running.stop().await;
        }
    }

    async fn apply(
        state: &AppState,
        enabled: bool,
        port: u16,
        viewer_public: bool,
    ) -> Result<ServerStatusResult, BantoError> {
        server_apply_body(state, enabled, "127.0.0.1".to_string(), port, viewer_public).await
    }

    async fn settings_change_entries(state: &AppState) -> Vec<(String, serde_json::Value)> {
        state
            .audit
            .list(ListParams::default())
            .await
            .expect("audit list")
            .rows
            .into_iter()
            .filter(|r| r.action == "settings_change" && r.resource == "settings")
            .map(|r| {
                let detail = r
                    .detail
                    .map(|d| serde_json::from_str(&d).expect("detail json"))
                    .unwrap_or(serde_json::Value::Null);
                (r.result, detail)
            })
            .collect()
    }

    /// Normal apply: the server runs on the new port, the settings are saved
    /// (閲覧公開 included), the status reports it and the success is audited.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn server_apply_starts_saves_and_audits() {
        let state = admin_app_state().await;
        let port = free_port();

        let status = apply(&state, true, port, true).await.expect("apply");

        assert!(status.running);
        assert_eq!(status.port, port);
        assert!(status.viewer_public);
        let saved = state.settings.server_config().await.unwrap();
        assert_eq!(
            (
                saved.enabled,
                saved.bind.as_str(),
                saved.port,
                saved.viewer_public
            ),
            (true, "127.0.0.1", port, true)
        );
        assert!(state.server.lock().await.is_some());
        assert!(answers_http(port).await);
        let entries = settings_change_entries(&state).await;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, "ok");
        assert_eq!(entries[0].1["port"], port);
        assert_eq!(entries[0].1["viewerPublic"], true);

        stop_running_server(&state).await;
    }

    /// #287: the new port is taken by someone else. The apply fails, the
    /// saved settings stay on the old port, the old server is back and still
    /// answers (and `server_status` - what the screen re-reads after the
    /// error - says so), and the failure is audited as `saved: false` /
    /// `restoredPrevious: true`. 反証: 旧来の「保存 → 停止 → 起動」に戻すと、
    /// 保存値が新しいポートになり旧サーバーも止まったままになって落ちる。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn server_apply_port_in_use_restores_the_previous_server() {
        let state = admin_app_state().await;
        let port_a = free_port();
        apply(&state, true, port_a, false).await.expect("apply A");
        let occupied = std::net::TcpListener::bind("127.0.0.1:0").expect("occupy");
        let port_b = occupied.local_addr().unwrap().port();

        let result = apply(&state, true, port_b, true).await;

        assert!(result.is_err(), "{result:?}");
        let saved = state.settings.server_config().await.unwrap();
        assert_eq!(saved.port, port_a);
        assert!(!saved.viewer_public, "閲覧公開も保存されていない");
        assert!(state.server.lock().await.is_some());
        assert!(answers_http(port_a).await, "old server must be restored");
        let running = state
            .server
            .lock()
            .await
            .as_ref()
            .map(|server| server.local_addr());
        let shown = build_status(&saved, running);
        assert!(shown.running);
        assert_eq!(shown.port, port_a);
        let entries = settings_change_entries(&state).await;
        let failed: Vec<_> = entries.iter().filter(|(r, _)| r == "failed").collect();
        assert_eq!(failed.len(), 1, "{entries:?}");
        assert_eq!(failed[0].1["saved"], false);
        assert_eq!(failed[0].1["restoredPrevious"], true);
        assert_eq!(failed[0].1["port"], port_b);

        drop(occupied);
        stop_running_server(&state).await;
    }

    /// #287 with nothing running before: a port in use fails, nothing is
    /// saved, nothing runs, and `restoredPrevious` is `true` (there was
    /// nothing to restore).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn server_apply_port_in_use_from_stopped_saves_nothing() {
        let state = admin_app_state().await;
        let before = state.settings.server_config().await.unwrap();
        let occupied = std::net::TcpListener::bind("127.0.0.1:0").expect("occupy");
        let port = occupied.local_addr().unwrap().port();

        let result = apply(&state, true, port, false).await;

        assert!(result.is_err(), "{result:?}");
        assert_eq!(state.settings.server_config().await.unwrap(), before);
        assert!(state.server.lock().await.is_none());
        let entries = settings_change_entries(&state).await;
        assert_eq!(entries.len(), 1, "{entries:?}");
        assert_eq!(entries[0].0, "failed");
        assert_eq!(entries[0].1["restoredPrevious"], true);
        drop(occupied);
    }

    /// #288: with `auth.disabled` stored, enabling the LAN server without
    /// 閲覧公開 is refused BEFORE anything is stopped; with 閲覧公開 it works
    /// (「ログイン不要モード + LAN」は閲覧公開 ON のときだけ).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn server_apply_refuses_lan_without_viewer_public_when_auth_disabled() {
        let state = admin_app_state().await;
        let port_a = free_port();
        // Running with 閲覧公開 ON makes disabling auth legal.
        apply(&state, true, port_a, true).await.expect("apply A");
        let mut auth = state.settings.auth_config().await.unwrap();
        auth.disabled = true;
        state.settings.set_auth_config(&auth).await.expect("auth");
        let before = state.settings.server_config().await.unwrap();

        let result = apply(&state, true, free_port(), false).await;

        assert!(result.is_err(), "{result:?}");
        assert!(state.server.lock().await.is_some(), "nothing was stopped");
        assert!(answers_http(port_a).await);
        assert_eq!(state.settings.server_config().await.unwrap(), before);

        let status = apply(&state, true, port_a, true).await.expect("viewer ON");
        assert!(status.running);
        assert!(answers_http(port_a).await);

        stop_running_server(&state).await;
    }

    /// The other half of #288 on the desktop: turning ログイン不要モード on
    /// while LAN access is enabled is refused without 閲覧公開 and allowed
    /// with it (`auth_config_apply` → banto `set_auth_config`).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn auth_disabled_with_lan_requires_viewer_public() {
        let state = admin_app_state().await;
        let port = free_port();
        apply(&state, true, port, false).await.expect("LAN on");

        let refused = auth_config_apply_body(&state, true, "viewer").await;
        assert!(refused.is_err(), "{refused:?}");
        assert!(!state.settings.auth_config().await.unwrap().disabled);

        apply(&state, true, port, true).await.expect("閲覧公開 ON");
        let applied = auth_config_apply_body(&state, true, "viewer")
            .await
            .expect("閲覧公開 ON なら許される");
        assert!(applied.disabled);

        stop_running_server(&state).await;
    }

    /// #294 review: an `auth.disabled = true` save that lands while the apply
    /// is waiting for `auth_config_lock` (after its bind) is seen by the
    /// apply's final validation. The apply fails, the stored `server.*` is
    /// unchanged and the new port is released again.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn server_apply_is_serialized_with_auth_config_writes() {
        let state = admin_app_state().await;
        let before = state.settings.server_config().await.unwrap();
        let port = free_port();
        let guard = state.auth_config_lock.lock().await;

        let applying = apply(&state, true, port, false);
        tokio::pin!(applying);
        // The listener is bound => the apply is at its save, which needs the
        // lock we hold.
        drive_until!(applying, !port_is_free(port));
        assert_stays_pending!(applying);
        assert_eq!(state.settings.server_config().await.unwrap(), before);

        let mut auth = state.settings.auth_config().await.unwrap();
        auth.disabled = true;
        state.settings.set_auth_config(&auth).await.expect("auth");
        drop(guard);
        let result = applying.await;

        assert!(result.is_err(), "{result:?}");
        assert_eq!(state.settings.server_config().await.unwrap(), before);
        assert!(state.server.lock().await.is_none());
        assert!(port_is_free(port), "the new listener must be released");
    }

    /// 閲覧公開の往復を ChronoGazer の組み込みサーバーで: ON で適用すると
    /// 未ログインのクライアントが `POST /api/auth/grant/publicViewer` で
    /// 閲覧者のトークンを得て読める（書けない）。OFF で適用し直すと、その
    /// トークンは新しいサーバーでも `401` になり（ADR-0017: 保存の後の
    /// `revoke_grant_tokens`）、発行も `403`。本物のログインのセッションは
    /// 残る。反証: `save_server_config_locked` の失効を外すと、OFF 後も
    /// トークンが読めて落ちる。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn server_apply_viewer_public_round_trip_and_revocation() {
        let state = admin_app_state().await;
        let port = free_port();
        apply(&state, true, port, true).await.expect("ON");

        let (status, body) = http_on(port, "POST", "/api/auth/grant/publicViewer", None)
            .await
            .expect("answers");
        assert_eq!(status, 200, "{body}");
        let body: serde_json::Value = serde_json::from_str(&body).unwrap();
        let viewer = body["token"].as_str().expect("token").to_string();
        let (status, _) = http_on(port, "GET", "/api/tags", Some(&viewer))
            .await
            .unwrap();
        assert_eq!(status, 200);
        let (status, _) = http_on(port, "DELETE", "/api/tags/1", Some(&viewer))
            .await
            .unwrap();
        assert_eq!(status, 403);
        let account = rest_login(&state.rest_auth, "admin", false).await;

        apply(&state, true, port, false).await.expect("OFF");

        assert!(!state.rest_auth.verify(&viewer));
        let (status, _) = http_on(port, "GET", "/api/tags", Some(&viewer))
            .await
            .unwrap();
        assert_eq!(status, 401);
        let (status, _) = http_on(port, "POST", "/api/auth/grant/publicViewer", None)
            .await
            .unwrap();
        assert_eq!(status, 403);
        assert!(state.rest_auth.verify(&account), "ログインは残る");
        let (status, _) = http_on(port, "GET", "/api/tags", Some(&account))
            .await
            .unwrap();
        assert_eq!(status, 200);

        stop_running_server(&state).await;
    }

    /// 閲覧公開 OFF で適用すると、LAN を止める（`enabled = false`）ときも
    /// 発行済みの閲覧者のトークンは失効する（admin-template v3.0.0 の
    /// `server_apply_with_viewer_public_off_revokes_public_viewer_tokens`）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn server_apply_with_viewer_public_off_revokes_public_viewer_tokens() {
        let state = admin_app_state().await;
        let spec = banto_server::GrantSpec::public_viewer(state.settings.clone());
        let token = state
            .rest_auth
            .issue_grant_token(&spec, state.rest_auth.grant_generation(&spec.kind))
            .expect("issued");
        assert!(state.rest_auth.verify(&token));

        apply(&state, false, free_port(), false)
            .await
            .expect("apply");

        assert!(!state.rest_auth.verify(&token));
    }

    /// I2b（PR #499 レビュー P2）: ログイン不要モード + LAN + 閲覧公開が全部
    /// ON の状態から、閲覧公開を外し LAN も止めて適用する（画面で閲覧公開を
    /// 先に外した場合の操作順）。適用は通り、サーバーは止まり、発行済みの
    /// 閲覧者のトークンは失効する。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn server_apply_auth_disabled_stop_lan_and_viewer_public_revokes_tokens() {
        let state = admin_app_state().await;
        let port = free_port();
        apply(&state, true, port, true).await.expect("ON");
        let mut auth = state.settings.auth_config().await.unwrap();
        auth.disabled = true;
        state.settings.set_auth_config(&auth).await.expect("auth");
        let (_, body) = http_on(port, "POST", "/api/auth/grant/publicViewer", None)
            .await
            .expect("answers");
        let body: serde_json::Value = serde_json::from_str(&body).unwrap();
        let viewer = body["token"].as_str().expect("token").to_string();
        assert!(state.rest_auth.verify(&viewer));

        let status = apply(&state, false, port, false).await.expect("stop");

        assert!(!status.running);
        assert!(state.server.lock().await.is_none());
        let saved = state.settings.server_config().await.unwrap();
        assert!(!saved.enabled && !saved.viewer_public);
        assert!(!state.rest_auth.verify(&viewer), "閲覧者のトークンは失効");
        assert!(port_is_free(port));
    }

    /// `enabled = false` stops the running server (its port is released) and
    /// saves `enabled = false`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn server_apply_disabled_stops_the_running_server() {
        let state = admin_app_state().await;
        let port = free_port();
        apply(&state, true, port, false).await.expect("start");
        assert!(answers_http(port).await);

        let status = apply(&state, false, port, false).await.expect("stop");

        assert!(!status.running);
        assert!(state.server.lock().await.is_none());
        assert!(!state.settings.server_config().await.unwrap().enabled);
        assert!(port_is_free(port));
    }

    /// A non-admin is refused and nothing changes.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn server_apply_is_admin_only() {
        let state = app_state().await;
        let viewer = state
            .users
            .create_user("viewer", "password123", "閲覧者", Role::Viewer)
            .await
            .expect("create_user");
        state.set_session_for_test(Some(DesktopSession::Account(viewer)));
        let before = state.settings.server_config().await.unwrap();
        let port = free_port();

        let err = apply(&state, true, port, false).await.unwrap_err();

        assert!(matches!(err, BantoError::Forbidden), "{err:?}");
        assert_eq!(state.settings.server_config().await.unwrap(), before);
        assert!(state.server.lock().await.is_none());
        assert!(port_is_free(port));
        assert!(settings_change_entries(&state).await.is_empty());
    }

    /// 起動時の自動開始の判定（[`lan_autostart_allowed`]）の総当たり: LAN が
    /// 有効で、「ログイン不要モード + LAN」なら閲覧公開 ON のときだけ。
    /// 反証: 旧来の `!(disabled && enabled)` に戻すと、閲覧公開 ON の行が
    /// 落ちる。
    #[test]
    fn lan_autostart_allowed_table() {
        let cases = [
            // (auth.disabled, server.enabled, viewer_public, expected)
            (false, false, false, false),
            (false, false, true, false),
            (false, true, false, true),
            (false, true, true, true),
            (true, false, false, false),
            (true, false, true, false),
            (true, true, false, false),
            (true, true, true, true),
        ];
        for (disabled, enabled, viewer_public, expected) in cases {
            let auth = AuthSettings {
                disabled,
                ..AuthSettings::default()
            };
            let server = ServerSettings {
                enabled,
                viewer_public,
                ..ServerSettings::default()
            };
            assert_eq!(
                lan_autostart_allowed(&auth, &server),
                expected,
                "disabled={disabled} enabled={enabled} viewer_public={viewer_public}"
            );
        }
    }

    // --- #393: 表示グループ（Tauri 経路） ---------------------------------------

    fn display_group_input(name: &str, kind: &str, tag_ids: &[i64]) -> DisplayGroupPayload {
        serde_json::from_value(serde_json::json!({
            "name": name,
            "kind": kind,
            "pens": tag_ids.iter().map(|id| serde_json::json!({ "tagId": id })).collect::<Vec<_>>(),
        }))
        .expect("DisplayGroupPayload")
    }

    /// `display_groups` の監査の (action, result, origin)。
    async fn display_group_audit(state: &AppState) -> Vec<(String, String, String)> {
        let page = state
            .audit
            .list(ListParams::default())
            .await
            .expect("audit list");
        let mut rows: Vec<_> = page
            .rows
            .into_iter()
            .filter(|row| row.resource == "display_groups")
            .collect();
        rows.sort_by_key(|row| row.id);
        rows.into_iter()
            .map(|row| (row.action, row.result, row.origin))
            .collect()
    }

    /// **#393（Tauri 経路）**: REST と同じサービス・同じ検証・同じ監査の形。
    /// viewer は読むだけ（書き込みは `Forbidden` で `denied` を監査）、editor は
    /// 作成・更新・並べ替え・削除ができ、古い版の更新は `expectedRevision` の
    /// 検証エラー。ペンに割り当てたタグの `tags_delete` は参照元つきで拒否する。
    ///
    /// 反証（2026-10-08 実施）: `tags_delete_body` を `state.tags.delete(id)` に
    /// 戻すと、`expect_err("参照されているタグ…")` が落ちる。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn display_group_commands_crud_rbac_audit_and_tag_delete_refusal() {
        let state = app_state().await;
        let editor = state
            .users
            .create_user("editor", "password123", "編集者", Role::Editor)
            .await
            .expect("create editor");
        let viewer = state
            .users
            .create_user("viewer", "password123", "閲覧者", Role::Viewer)
            .await
            .expect("create viewer");

        let conn = state
            .plc_connections
            .create(connection_payload("plc", "modbus-tcp").into_create_input())
            .await
            .expect("create connection");
        let group = state
            .collection_groups
            .create(
                CollectionGroupPayload {
                    name: "cg".to_string(),
                    plc_connection_id: conn.id,
                    period_ms: 1000,
                    enabled: true,
                }
                .into(),
            )
            .await
            .expect("create collection group");
        let mut tag_ids = Vec::new();
        for (name, address) in [("t1", "40001"), ("t2", "40002")] {
            let tag = state
                .tags
                .create(tag_payload(name, group.id, address).into())
                .await
                .expect("create tag");
            tag_ids.push(tag.id);
        }

        // viewer: 読めるが書けない。
        state.set_session_for_test(Some(DesktopSession::Account(viewer)));
        assert!(display_groups_list_body(&state).await.unwrap().is_empty());
        let err = display_groups_create_body(&state, display_group_input("L1", "trend", &tag_ids))
            .await
            .expect_err("viewer が作成できてしまった");
        assert!(matches!(err, BantoError::Forbidden), "{err:?}");

        // editor: 作成 → 更新 → 古い版 → 並べ替え → タグの削除の拒否 → 削除。
        state.set_session_for_test(Some(DesktopSession::Account(editor)));
        let created =
            display_groups_create_body(&state, display_group_input("L1", "trend", &tag_ids))
                .await
                .expect("create");
        let other = display_groups_create_body(&state, display_group_input("L2", "bar", &[]))
            .await
            .expect("create other");
        let err = display_groups_create_body(&state, display_group_input("L1", "gauge", &[]))
            .await
            .expect_err("同名");
        assert_eq!(only_field_error(err).field, "name");

        let mut edit = display_group_input("L1改", "digital", &tag_ids[..1]);
        edit.expected_revision = Some(created.revision);
        let updated = display_groups_update_body(&state, created.id, edit.clone())
            .await
            .expect("update");
        assert_eq!(updated.revision, created.revision + 1);
        let err = display_groups_update_body(&state, created.id, edit)
            .await
            .expect_err("古い版");
        assert_eq!(only_field_error(err).field, "expectedRevision");

        let reordered = display_groups_reorder_body(&state, vec![other.id, created.id])
            .await
            .expect("reorder");
        assert_eq!(reordered[0].id, other.id);

        let err = tags_delete_body(&state, tag_ids[0])
            .await
            .expect_err("参照されているタグが消せてしまった");
        let field = only_field_error(err);
        assert_eq!(field.field, "displayGroups");
        assert!(field.message.contains("「L1改」"), "{}", field.message);
        assert!(state.tags.get(tag_ids[0]).await.is_ok());
        tags_delete_body(&state, tag_ids[1])
            .await
            .expect("参照されていないタグは消せる");

        display_groups_delete_body(&state, created.id)
            .await
            .expect("delete");
        tags_delete_body(&state, tag_ids[0])
            .await
            .expect("グループを消せばタグも消せる");

        assert_eq!(
            display_group_audit(&state).await,
            [
                ("denied", "denied"),
                ("create", "ok"),
                ("create", "ok"),
                ("update", "ok"),
                ("reorder", "ok"),
                ("delete", "ok"),
            ]
            .map(|(action, result)| (
                action.to_string(),
                result.to_string(),
                "tauri".to_string()
            ))
        );
    }
}
