//! Local credential store (spec §8.2): a `users` table in the app's SQLite
//! settings DB, with argon2id password hashes. Replaces the fixed
//! admin/admin demo credential check that used to live on `AppState` in
//! `src-tauri` (see that crate's former TODO).
//!
//! Design note (spec §8.2 mentions `keyring` for credentials): keyring is a
//! *client-side, single-user* OS credential store, which does not fit a
//! multi-user LAN-server app where any device on the network - not just the
//! machine running the desktop app - needs to authenticate against the same
//! account database. Argon2id hashes stored in the same SQLite settings DB
//! the rest of the app already uses cover both the desktop-only case and the
//! LAN-server case with one mechanism. `keyring` remains a good option later
//! for CLIENT-side bearer-token storage (i.e. the LAN browser/desktop
//! caching its *own* login token more securely than `sessionStorage`), which
//! is an orthogonal concern from where the account database itself lives.

use std::fmt;
use std::str::FromStr;
use std::sync::OnceLock;

use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use banto_core::{BantoError, FieldError};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

const MIN_USERNAME_LEN: usize = 1;
const MAX_USERNAME_LEN: usize = 32;
const MIN_PASSWORD_LEN: usize = 8;

/// Account role (spec M10 RBAC, `docs/roadmap.md`): three fixed levels,
/// `viewer` < `editor` < `admin`, each a superset of the previous one's
/// permissions. Stored as lowercase TEXT in the `users.role` column
/// (migration `0004_user_roles.sql`, which also `CHECK`s the DB-side set of
/// allowed values) and travels over the wire the same way (`#[serde(rename_all
/// = "lowercase")]`), so this is the single place both the DB round-trip and
/// the JSON wire shape agree on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Admin,
    Editor,
    Viewer,
}

impl Role {
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::Admin => "admin",
            Role::Editor => "editor",
            Role::Viewer => "viewer",
        }
    }

    /// Total order used by [`Role::at_least`]: `viewer` is the least
    /// privileged, `admin` the most.
    fn rank(&self) -> u8 {
        match self {
            Role::Viewer => 0,
            Role::Editor => 1,
            Role::Admin => 2,
        }
    }

    /// Is this role at least as privileged as `min`? The core RBAC
    /// predicate every role guard (REST middleware, Tauri's `require_role`)
    /// is built on.
    pub fn at_least(&self, min: Role) -> bool {
        self.rank() >= min.rank()
    }

    /// `editor` or `admin` - resources' create/update/delete (spec M10:
    /// "editor: + create/update/delete").
    pub fn can_write_resources(&self) -> bool {
        self.at_least(Role::Editor)
    }

    pub fn is_admin(&self) -> bool {
        matches!(self, Role::Admin)
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Role {
    type Err = BantoError;

    /// Parses the lowercase DB/wire representation. Used both to read the
    /// `role` TEXT column back out of SQLite and (in `chronogazer-core::rest`)
    /// to turn a bearer token's `Identity.role` string back into a typed
    /// `Role` for the REST role-guard middleware. An unrecognized value is a
    /// `BantoError::Other` (not `Validation`): it does not correspond to any
    /// particular request field at either call site.
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "admin" => Ok(Role::Admin),
            "editor" => Ok(Role::Editor),
            "viewer" => Ok(Role::Viewer),
            other => Err(BantoError::Other(format!("不明なロールです: {other}"))),
        }
    }
}

fn password_too_short_message() -> String {
    "パスワードは8文字以上で入力してください".to_string()
}

fn hash_password(password: &str) -> Result<String, BantoError> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|err| BantoError::Other(format!("パスワードのハッシュ化に失敗しました: {err}")))
}

fn verify_password(password: &str, hash: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(hash) else {
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

/// A valid argon2id PHC hash of an arbitrary fixed password, computed once
/// per process. Used only as the comparison target for the dummy verify in
/// `UsersService::verify` below, so an unknown username still "pays" the
/// argon2 cost before returning `None` - see that method's doc comment.
fn dummy_hash() -> &'static str {
    static HASH: OnceLock<String> = OnceLock::new();
    HASH.get_or_init(|| {
        hash_password("banto-dummy-verify-target-password")
            .expect("hashing a fixed password should never fail")
    })
}

fn validate_password_len(password: &str, field: &str) -> Result<(), BantoError> {
    if password.chars().count() < MIN_PASSWORD_LEN {
        return Err(BantoError::Validation {
            field_errors: vec![FieldError {
                field: field.to_string(),
                message: password_too_short_message(),
            }],
        });
    }
    Ok(())
}

fn validate_username(username: &str) -> Result<String, BantoError> {
    let trimmed = username.trim();
    let len = trimmed.chars().count();
    if !(MIN_USERNAME_LEN..=MAX_USERNAME_LEN).contains(&len) {
        return Err(BantoError::Validation {
            field_errors: vec![FieldError {
                field: "username".to_string(),
                message: format!("{MIN_USERNAME_LEN}〜{MAX_USERNAME_LEN}文字で入力してください"),
            }],
        });
    }
    Ok(trimmed.to_string())
}

/// Identity of a verified/created user. Spec §3.3's wire `Identity` carries
/// only `id`/`name`; this carries the full row needed by the REST/Tauri
/// command layers (e.g. `username`, to look the account back up for
/// `change_password`).
#[derive(Debug, Clone, PartialEq)]
pub struct UserIdentity {
    pub id: i64,
    pub username: String,
    pub display_name: String,
    pub role: Role,
}

/// Public listing of an account (spec M10's user-management screen):
/// everything the admin grid needs, deliberately NOT `password_hash`. Unlike
/// [`UserIdentity`] this derives `Serialize` since it is returned directly
/// over the wire (REST JSON body / Tauri command return value) rather than
/// only used internally.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UserSummary {
    pub id: i64,
    pub username: String,
    pub display_name: String,
    pub role: Role,
    pub created_at: String,
}

/// Local credential store (spec §8.2): argon2id password hashes in the
/// `users` table (migration `0003_users.sql`). No seed user - the app starts
/// "uninitialized" and the first run walks through `setup_first_user`.
///
/// `Clone` is cheap (`SqlitePool` is an `Arc`-backed handle), matching
/// `ItemsService`/`SettingsService`.
#[derive(Clone)]
pub struct UsersService {
    pool: SqlitePool,
}

impl UsersService {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Has *any* account been created yet? Used by the login page (spec
    /// §3.3/§8.2) to decide between the first-run setup form and the normal
    /// login form.
    pub async fn is_initialized(&self) -> Result<bool, BantoError> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
            .fetch_one(&self.pool)
            .await
            .map_err(banto_storage::storage_error)?;
        Ok(count > 0)
    }

    /// Create the very first account. Only succeeds while the `users` table
    /// is empty - once any account exists, this always fails with
    /// `BantoError::Other`, regardless of the requested username (spec:
    /// "first run is uninitialized", not "create if missing").
    pub async fn setup_first_user(
        &self,
        username: &str,
        password: &str,
        display_name: &str,
    ) -> Result<UserIdentity, BantoError> {
        if self.is_initialized().await? {
            return Err(BantoError::Other("既に初期化されています".to_string()));
        }

        let username = validate_username(username)?;
        validate_password_len(password, "password")?;
        let display_name = display_name.trim();
        let hash = hash_password(password)?;

        // The very first account is always `admin` (spec M10): there is no
        // one else yet to have assigned it a lesser role, and the app needs
        // at least one admin to exist to manage everyone else.
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO users (username, password_hash, display_name, role) VALUES (?, ?, ?, ?) \
             RETURNING id",
        )
        .bind(&username)
        .bind(&hash)
        .bind(display_name)
        .bind(Role::Admin.as_str())
        .fetch_one(&self.pool)
        .await
        .map_err(banto_storage::storage_error)?;

        Ok(UserIdentity {
            id,
            username,
            display_name: display_name.to_string(),
            role: Role::Admin,
        })
    }

    /// Verify a username/password pair. `Ok(None)` (rather than an error)
    /// covers both "no such user" and "wrong password" - the caller must not
    /// distinguish the two (standard login-form hygiene: do not tell an
    /// attacker which part was wrong).
    ///
    /// Timing note: on an unknown username we still run a dummy argon2
    /// verify against a fixed hash before returning `None`, so "unknown
    /// user" and "wrong password" take roughly the same amount of time -
    /// a best-effort mitigation (argon2's cost dominates either way), not a
    /// constant-time guarantee.
    pub async fn verify(
        &self,
        username: &str,
        password: &str,
    ) -> Result<Option<UserIdentity>, BantoError> {
        let row: Option<(i64, String, String, String)> = sqlx::query_as(
            "SELECT id, password_hash, display_name, role FROM users WHERE username = ?",
        )
        .bind(username)
        .fetch_optional(&self.pool)
        .await
        .map_err(banto_storage::storage_error)?;

        match row {
            Some((id, hash, display_name, role)) => {
                if verify_password(password, &hash) {
                    Ok(Some(UserIdentity {
                        id,
                        username: username.to_string(),
                        display_name,
                        role: Role::from_str(&role)?,
                    }))
                } else {
                    Ok(None)
                }
            }
            None => {
                let _ = verify_password(password, dummy_hash());
                Ok(None)
            }
        }
    }

    /// Verify `current`, then update the account's password to `new`
    /// (validated the same way as `setup_first_user`'s password, but with
    /// field name `newPassword` so the Tauri/REST layers can map the error
    /// straight onto the change-password form's second input).
    pub async fn change_password(
        &self,
        username: &str,
        current: &str,
        new: &str,
    ) -> Result<(), BantoError> {
        let row: Option<(i64, String)> =
            sqlx::query_as("SELECT id, password_hash FROM users WHERE username = ?")
                .bind(username)
                .fetch_optional(&self.pool)
                .await
                .map_err(banto_storage::storage_error)?;

        let wrong_current = || BantoError::Validation {
            field_errors: vec![FieldError {
                field: "currentPassword".to_string(),
                message: "現在のパスワードが違います".to_string(),
            }],
        };

        let Some((id, hash)) = row else {
            // Same dummy-verify timing note as `verify()` above.
            let _ = verify_password(current, dummy_hash());
            return Err(wrong_current());
        };

        if !verify_password(current, &hash) {
            return Err(wrong_current());
        }

        validate_password_len(new, "newPassword")?;
        let new_hash = hash_password(new)?;

        sqlx::query(
            "UPDATE users SET password_hash = ?, updated_at = datetime('now') WHERE id = ?",
        )
        .bind(&new_hash)
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(banto_storage::storage_error)?;

        Ok(())
    }

    // --- M10: user management (admin-only CRUD + RBAC) -------------------

    /// All accounts, for the admin user-management grid (spec M10).
    /// `password_hash` deliberately never leaves this module - see
    /// [`UserSummary`].
    pub async fn list_users(&self) -> Result<Vec<UserSummary>, BantoError> {
        let rows: Vec<(i64, String, String, String, String)> = sqlx::query_as(
            "SELECT id, username, display_name, role, created_at FROM users ORDER BY id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(banto_storage::storage_error)?;

        rows.into_iter()
            .map(|(id, username, display_name, role, created_at)| {
                Ok(UserSummary {
                    id,
                    username,
                    display_name,
                    role: Role::from_str(&role)?,
                    created_at,
                })
            })
            .collect()
    }

    /// Look up an account's full identity by username, without verifying a
    /// password. Used by the REST layer to recover the acting caller's
    /// numeric row id (needed by [`UsersService::delete_user`]'s
    /// self-deletion guard) from a bearer token's `Identity.id`, which
    /// carries the *username* (spec convention, see
    /// `banto_server::auth::Identity`'s doc comment), not the row id.
    pub async fn get_by_username(
        &self,
        username: &str,
    ) -> Result<Option<UserIdentity>, BantoError> {
        let row: Option<(i64, String, String)> =
            sqlx::query_as("SELECT id, display_name, role FROM users WHERE username = ?")
                .bind(username)
                .fetch_optional(&self.pool)
                .await
                .map_err(banto_storage::storage_error)?;

        match row {
            Some((id, display_name, role)) => Ok(Some(UserIdentity {
                id,
                username: username.to_string(),
                display_name,
                role: Role::from_str(&role)?,
            })),
            None => Ok(None),
        }
    }

    /// Create an additional account (spec M10; distinct from
    /// [`UsersService::setup_first_user`], which only ever runs once and
    /// always assigns `admin`). Validates the same way `setup_first_user`
    /// does, plus a friendly `BantoError::Validation` on a duplicate
    /// username (rather than surfacing the raw UNIQUE-constraint storage
    /// error to the admin form).
    pub async fn create_user(
        &self,
        username: &str,
        password: &str,
        display_name: &str,
        role: Role,
    ) -> Result<UserIdentity, BantoError> {
        let username = validate_username(username)?;
        validate_password_len(password, "password")?;
        let display_name = display_name.trim();

        let existing: Option<i64> = sqlx::query_scalar("SELECT id FROM users WHERE username = ?")
            .bind(&username)
            .fetch_optional(&self.pool)
            .await
            .map_err(banto_storage::storage_error)?;
        if existing.is_some() {
            return Err(BantoError::Validation {
                field_errors: vec![FieldError {
                    field: "username".to_string(),
                    message: "このユーザー名は既に使用されています".to_string(),
                }],
            });
        }

        let hash = hash_password(password)?;
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO users (username, password_hash, display_name, role) VALUES (?, ?, ?, ?) \
             RETURNING id",
        )
        .bind(&username)
        .bind(&hash)
        .bind(display_name)
        .bind(role.as_str())
        .fetch_one(&self.pool)
        .await
        .map_err(banto_storage::storage_error)?;

        Ok(UserIdentity {
            id,
            username,
            display_name: display_name.to_string(),
            role,
        })
    }

    /// Current role of account `id`, or `NotFound` if it does not exist.
    /// Shared by the last-admin guards on [`UsersService::update_user`] and
    /// [`UsersService::delete_user`].
    async fn role_of(&self, id: i64) -> Result<Role, BantoError> {
        let role: Option<String> = sqlx::query_scalar("SELECT role FROM users WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(banto_storage::storage_error)?;
        match role {
            Some(role) => Role::from_str(&role),
            None => Err(BantoError::NotFound {
                resource: "users".to_string(),
                id: id.to_string(),
            }),
        }
    }

    /// Guard (spec M10 completion condition): refuse an operation on
    /// account `id` that would leave zero `admin` accounts. Counts admins
    /// OTHER than `id` - if that count is zero, `id` is the last admin and
    /// the caller must not be allowed to demote or delete it.
    async fn ensure_not_last_admin(&self, id: i64) -> Result<(), BantoError> {
        let remaining_admins: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE role = 'admin' AND id != ?")
                .bind(id)
                .fetch_one(&self.pool)
                .await
                .map_err(banto_storage::storage_error)?;
        if remaining_admins == 0 {
            return Err(BantoError::Other(
                "最後の管理者を降格・削除することはできません".to_string(),
            ));
        }
        Ok(())
    }

    /// Update an account's `display_name`/`role` (spec M10; password
    /// changes go through [`UsersService::change_password`] (self-service)
    /// or [`UsersService::reset_password`] (admin) instead). Refuses to
    /// demote the last `admin` account.
    pub async fn update_user(
        &self,
        id: i64,
        display_name: &str,
        role: Role,
    ) -> Result<UserSummary, BantoError> {
        let display_name = display_name.trim();

        let current_role = self.role_of(id).await?;
        if current_role.is_admin() && !role.is_admin() {
            self.ensure_not_last_admin(id).await?;
        }

        let row: Option<(i64, String, String, String, String)> = sqlx::query_as(
            "UPDATE users SET display_name = ?, role = ?, updated_at = datetime('now') WHERE id = ? \
             RETURNING id, username, display_name, role, created_at",
        )
        .bind(display_name)
        .bind(role.as_str())
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(banto_storage::storage_error)?;

        let Some((row_id, username, display_name, role_str, created_at)) = row else {
            return Err(BantoError::NotFound {
                resource: "users".to_string(),
                id: id.to_string(),
            });
        };

        Ok(UserSummary {
            id: row_id,
            username,
            display_name,
            role: Role::from_str(&role_str)?,
            created_at,
        })
    }

    /// Admin-initiated password reset (spec M10): unlike
    /// [`UsersService::change_password`], does not require the account's
    /// current password - this is an administrative action on someone
    /// else's account, not self-service.
    pub async fn reset_password(&self, id: i64, new_password: &str) -> Result<(), BantoError> {
        validate_password_len(new_password, "newPassword")?;
        let hash = hash_password(new_password)?;

        let result = sqlx::query(
            "UPDATE users SET password_hash = ?, updated_at = datetime('now') WHERE id = ?",
        )
        .bind(&hash)
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(banto_storage::storage_error)?;

        if result.rows_affected() == 0 {
            return Err(BantoError::NotFound {
                resource: "users".to_string(),
                id: id.to_string(),
            });
        }
        Ok(())
    }

    /// Delete account `id` (spec M10). Refuses two cases with a
    /// `BantoError`, both guards the M10 completion criteria call out
    /// explicitly: deleting the last `admin`, and an account deleting
    /// itself (`acting_user_id`, the caller's own numeric row id, is
    /// resolved by the REST/Tauri layer before calling this - see
    /// [`UsersService::get_by_username`] for the REST side, which only has
    /// the caller's username from the session token).
    pub async fn delete_user(&self, id: i64, acting_user_id: i64) -> Result<(), BantoError> {
        if id == acting_user_id {
            return Err(BantoError::Other(
                "自分自身を削除することはできません".to_string(),
            ));
        }

        let role = self.role_of(id).await?;
        if role.is_admin() {
            self.ensure_not_last_admin(id).await?;
        }

        let result = sqlx::query("DELETE FROM users WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(banto_storage::storage_error)?;
        if result.rows_affected() == 0 {
            return Err(BantoError::NotFound {
                resource: "users".to_string(),
                id: id.to_string(),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrate_memory;

    async fn service() -> UsersService {
        let pool = migrate_memory().await.expect("migrate_memory");
        UsersService::new(pool)
    }

    #[tokio::test]
    async fn is_initialized_is_false_on_a_fresh_db() {
        let svc = service().await;
        assert!(!svc.is_initialized().await.unwrap());
    }

    #[tokio::test]
    async fn setup_first_user_then_verify_round_trips() {
        let svc = service().await;
        let created = svc
            .setup_first_user("owner", "password123", "オーナー")
            .await
            .expect("setup should succeed");
        assert_eq!(created.username, "owner");
        assert_eq!(created.display_name, "オーナー");

        assert!(svc.is_initialized().await.unwrap());

        let identity = svc
            .verify("owner", "password123")
            .await
            .unwrap()
            .expect("verify should succeed with the right password");
        assert_eq!(identity.username, "owner");
    }

    #[tokio::test]
    async fn verify_wrong_password_is_none() {
        let svc = service().await;
        svc.setup_first_user("owner", "password123", "オーナー")
            .await
            .unwrap();
        assert!(svc
            .verify("owner", "wrong-password")
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn verify_unknown_user_is_none() {
        let svc = service().await;
        assert!(svc.verify("nobody", "whatever1").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn setup_first_user_can_only_run_once() {
        let svc = service().await;
        svc.setup_first_user("owner", "password123", "オーナー")
            .await
            .unwrap();
        let err = svc
            .setup_first_user("someone-else", "password123", "誰か")
            .await
            .unwrap_err();
        match err {
            BantoError::Other(message) => assert_eq!(message, "既に初期化されています"),
            other => panic!("expected Other, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn setup_first_user_rejects_short_password() {
        let svc = service().await;
        let err = svc
            .setup_first_user("owner", "short", "オーナー")
            .await
            .unwrap_err();
        match err {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "password");
                assert_eq!(
                    field_errors[0].message,
                    "パスワードは8文字以上で入力してください"
                );
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn setup_first_user_rejects_blank_username() {
        let svc = service().await;
        let err = svc
            .setup_first_user("   ", "password123", "オーナー")
            .await
            .unwrap_err();
        assert!(matches!(err, BantoError::Validation { .. }));
    }

    #[tokio::test]
    async fn duplicate_username_on_a_future_create_path_is_a_storage_error() {
        // `setup_first_user` itself can only ever run once (see the test
        // above); this exercises the UNIQUE constraint directly, standing in
        // for any future "add another user" path that would otherwise hit
        // the same constraint.
        let svc = service().await;
        svc.setup_first_user("owner", "password123", "オーナー")
            .await
            .unwrap();
        let hash = hash_password("password123").unwrap();
        let err = sqlx::query(
            "INSERT INTO users (username, password_hash, display_name) VALUES (?, ?, ?)",
        )
        .bind("owner")
        .bind(&hash)
        .bind("Duplicate")
        .execute(&svc.pool)
        .await
        .unwrap_err();
        assert!(err.to_string().to_lowercase().contains("unique"));
    }

    #[tokio::test]
    async fn change_password_happy_path() {
        let svc = service().await;
        svc.setup_first_user("owner", "password123", "オーナー")
            .await
            .unwrap();
        svc.change_password("owner", "password123", "newpassword1")
            .await
            .expect("change_password should succeed");

        assert!(svc.verify("owner", "password123").await.unwrap().is_none());
        assert!(svc.verify("owner", "newpassword1").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn change_password_rejects_wrong_current_password() {
        let svc = service().await;
        svc.setup_first_user("owner", "password123", "オーナー")
            .await
            .unwrap();
        let err = svc
            .change_password("owner", "not-the-password", "newpassword1")
            .await
            .unwrap_err();
        match err {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "currentPassword");
                assert_eq!(field_errors[0].message, "現在のパスワードが違います");
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn change_password_rejects_short_new_password() {
        let svc = service().await;
        svc.setup_first_user("owner", "password123", "オーナー")
            .await
            .unwrap();
        let err = svc
            .change_password("owner", "password123", "short")
            .await
            .unwrap_err();
        match err {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "newPassword");
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[test]
    fn hash_then_verify_round_trips() {
        let hash = hash_password("hunter2hunter").unwrap();
        assert!(verify_password("hunter2hunter", &hash));
        assert!(!verify_password("wrong", &hash));
    }

    // --- Role -------------------------------------------------------------

    #[test]
    fn role_as_str_and_from_str_round_trip() {
        for role in [Role::Admin, Role::Editor, Role::Viewer] {
            assert_eq!(Role::from_str(role.as_str()).unwrap(), role);
        }
    }

    #[test]
    fn role_from_str_rejects_unknown_values() {
        assert!(Role::from_str("superuser").is_err());
    }

    #[test]
    fn role_at_least_orders_viewer_editor_admin() {
        assert!(Role::Admin.at_least(Role::Admin));
        assert!(Role::Admin.at_least(Role::Editor));
        assert!(Role::Admin.at_least(Role::Viewer));
        assert!(Role::Editor.at_least(Role::Editor));
        assert!(Role::Editor.at_least(Role::Viewer));
        assert!(!Role::Editor.at_least(Role::Admin));
        assert!(Role::Viewer.at_least(Role::Viewer));
        assert!(!Role::Viewer.at_least(Role::Editor));
        assert!(!Role::Viewer.at_least(Role::Admin));
    }

    #[test]
    fn can_write_resources_is_editor_and_above() {
        assert!(Role::Admin.can_write_resources());
        assert!(Role::Editor.can_write_resources());
        assert!(!Role::Viewer.can_write_resources());
    }

    #[test]
    fn is_admin_is_admin_only() {
        assert!(Role::Admin.is_admin());
        assert!(!Role::Editor.is_admin());
        assert!(!Role::Viewer.is_admin());
    }

    // --- M10 user management CRUD -----------------------------------------

    #[tokio::test]
    async fn setup_first_user_is_always_admin() {
        let svc = service().await;
        let created = svc
            .setup_first_user("owner", "password123", "オーナー")
            .await
            .unwrap();
        assert_eq!(created.role, Role::Admin);

        let identity = svc
            .verify("owner", "password123")
            .await
            .unwrap()
            .expect("verify should succeed");
        assert_eq!(identity.role, Role::Admin);
    }

    #[tokio::test]
    async fn create_list_update_reset_password_and_delete_round_trip() {
        let svc = service().await;
        let owner = svc
            .setup_first_user("owner", "password123", "オーナー")
            .await
            .unwrap();

        let created = svc
            .create_user("editor1", "password123", "編集者1", Role::Editor)
            .await
            .expect("create_user should succeed");
        assert_eq!(created.role, Role::Editor);

        let listed = svc.list_users().await.unwrap();
        assert_eq!(listed.len(), 2);
        let listed_editor = listed
            .iter()
            .find(|u| u.username == "editor1")
            .expect("editor1 should be listed");
        assert_eq!(listed_editor.role, Role::Editor);
        assert_eq!(listed_editor.display_name, "編集者1");

        let updated = svc
            .update_user(created.id, "編集者1改", Role::Viewer)
            .await
            .expect("update_user should succeed");
        assert_eq!(updated.display_name, "編集者1改");
        assert_eq!(updated.role, Role::Viewer);

        svc.reset_password(created.id, "resetpassword1")
            .await
            .expect("reset_password should succeed");
        assert!(svc
            .verify("editor1", "password123")
            .await
            .unwrap()
            .is_none());
        assert!(svc
            .verify("editor1", "resetpassword1")
            .await
            .unwrap()
            .is_some());

        svc.delete_user(created.id, owner.id)
            .await
            .expect("delete_user should succeed");
        assert_eq!(svc.list_users().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn create_user_rejects_duplicate_username() {
        let svc = service().await;
        svc.setup_first_user("owner", "password123", "オーナー")
            .await
            .unwrap();
        let err = svc
            .create_user("owner", "password123", "別オーナー", Role::Editor)
            .await
            .unwrap_err();
        match err {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "username");
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn update_user_rejects_demoting_the_last_admin() {
        let svc = service().await;
        let owner = svc
            .setup_first_user("owner", "password123", "オーナー")
            .await
            .unwrap();

        let err = svc
            .update_user(owner.id, "オーナー", Role::Editor)
            .await
            .unwrap_err();
        assert!(matches!(err, BantoError::Other(_)));

        // Once a second admin exists, the first can be demoted.
        svc.create_user("owner2", "password123", "オーナー2", Role::Admin)
            .await
            .unwrap();
        let demoted = svc
            .update_user(owner.id, "オーナー", Role::Editor)
            .await
            .expect("demotion should succeed once another admin exists");
        assert_eq!(demoted.role, Role::Editor);
    }

    #[tokio::test]
    async fn update_user_allows_demoting_a_non_last_admin() {
        let svc = service().await;
        let owner = svc
            .setup_first_user("owner", "password123", "オーナー")
            .await
            .unwrap();
        svc.create_user("owner2", "password123", "オーナー2", Role::Admin)
            .await
            .unwrap();

        let demoted = svc
            .update_user(owner.id, "オーナー", Role::Viewer)
            .await
            .expect("demoting one of two admins should succeed");
        assert_eq!(demoted.role, Role::Viewer);
    }

    #[tokio::test]
    async fn delete_user_rejects_deleting_the_last_admin() {
        let svc = service().await;
        let owner = svc
            .setup_first_user("owner", "password123", "オーナー")
            .await
            .unwrap();
        let editor = svc
            .create_user("editor1", "password123", "編集者1", Role::Editor)
            .await
            .unwrap();

        // `editor1` deletes `owner` (the only admin) - must be rejected.
        let err = svc.delete_user(owner.id, editor.id).await.unwrap_err();
        assert!(matches!(err, BantoError::Other(_)));
        assert_eq!(svc.list_users().await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn delete_user_rejects_self_deletion() {
        let svc = service().await;
        let owner = svc
            .setup_first_user("owner", "password123", "オーナー")
            .await
            .unwrap();
        svc.create_user("owner2", "password123", "オーナー2", Role::Admin)
            .await
            .unwrap();

        // Even though a second admin exists, `owner` may not delete itself.
        let err = svc.delete_user(owner.id, owner.id).await.unwrap_err();
        assert!(matches!(err, BantoError::Other(_)));
        assert_eq!(svc.list_users().await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn delete_user_missing_id_is_not_found() {
        let svc = service().await;
        let owner = svc
            .setup_first_user("owner", "password123", "オーナー")
            .await
            .unwrap();
        let err = svc.delete_user(999, owner.id).await.unwrap_err();
        assert!(
            matches!(err, BantoError::NotFound { resource, id } if resource == "users" && id == "999")
        );
    }

    #[tokio::test]
    async fn get_by_username_finds_and_misses() {
        let svc = service().await;
        svc.setup_first_user("owner", "password123", "オーナー")
            .await
            .unwrap();
        let found = svc
            .get_by_username("owner")
            .await
            .unwrap()
            .expect("owner should be found");
        assert_eq!(found.role, Role::Admin);
        assert!(svc.get_by_username("nobody").await.unwrap().is_none());
    }
}
