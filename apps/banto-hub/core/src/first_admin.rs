//! Hub の最初の管理者（banto v8、ADR-0034 §7・docs/banto-v8-upgrade-plan.md §2 の 2）。
//!
//! banto v8 で初回セットアップ（`POST /api/auth/setup`・`BANTO_ALLOW_SETUP`）は無くなった。
//! Hub のサービス（ヘッドレス）の最初の管理者は、上流の `banto-serve create-admin` と同じ形の
//! CLI `banto-hub create-admin --username <名前> [--display-name <表示名>]`（パスワードは
//! 標準入力）で作る。**ネットワーク越しには作らない**。
//!
//! 1 回の実行で、次を**1 つのトランザクション**で書く（どれかが失敗したら何も残らない）:
//!
//! 1. 管理者のアカウント（ユーザー管理と同じ検査。予約名 `admin`・[`crate::commissioning::reserved_usernames`] は作れない）。
//! 2. ログイン必須（banto の `auth.disabled = false`。`0015_auth_mode_defaults.sql` が新しい DB に
//!    `true` を入れるため。Hub の REST は `auth.disabled` を読まないが、banto の設定と食い違わせない）。
//! 3. 試運転モードの終わり（ロックダウン、`commissioning.locked_down = true`）。Hub で
//!    「ログイン必須」に当たるのはロックダウンで、管理者を作った後に loopback の合成 admin の
//!    grant（試運転）を残さない。
//! 4. 監査の行（組み込みの `admin` が CLI（`actor_kind = cli`）で行った操作として）。
//!
//! サービス・コンソールの Hub を止めてから実行する（動いている Hub はセッションと
//! ロックダウンの状態をメモリに持っていて、ここからは終わらせられない）。止まっていることは、
//! 起動と同じ profile の排他を取って確かめる（`crate::runtime` の [`run_create_admin`]）。

use banto_core::BantoError;
use sqlx::SqlitePool;

use crate::audit::{AuditEntry, ACTOR_KIND_CLI};
use crate::users::{user_create_detail, Role, UserIdentity, UsersService, BUILTIN_ADMIN_USERNAME};

/// 監査の行の `origin`。
const ORIGIN_CLI: &str = "cli";

/// 作るアカウント。パスワードはコマンドラインから受け取らない（プロセスの一覧と
/// シェルの履歴に残るため）。CLI は標準入力から読む。
#[derive(Clone, Copy)]
pub struct NewAdmin<'a> {
    pub username: &'a str,
    pub password: &'a str,
    pub display_name: &'a str,
}

/// [`create_admin_and_lock_down_on`] が作ったアカウント。
#[derive(Debug, Clone)]
pub struct CreatedAdmin {
    pub user: UserIdentity,
}

const UPSERT_SETTING: &str = "INSERT INTO settings (key, value) VALUES (?, ?) \
     ON CONFLICT(key) DO UPDATE SET value = excluded.value";

/// migration を当て終えた Hub の設定 DB に、管理者・ログイン必須・ロックダウン・監査の行を
/// 1 つのトランザクションで書く（モジュール doc）。既にロックダウン済みの DB でも動く
/// （同じ値を書き直す。管理者を作り直す復旧の用途）。
pub async fn create_admin_and_lock_down_on(
    pool: &SqlitePool,
    admin: NewAdmin<'_>,
) -> Result<CreatedAdmin, BantoError> {
    let users = UsersService::new(crate::db::Db::Sqlite(pool.clone()))
        .with_reserved_usernames(crate::commissioning::reserved_usernames());
    // BEGIN IMMEDIATE: 最初の文から書き込みのロックを取る。
    let mut tx = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(banto_storage::storage_error)?;
    let user = users
        .create_user_on_sqlite(
            &mut tx,
            admin.username,
            admin.password,
            admin.display_name,
            Role::Admin,
        )
        .await?;
    banto_admin_services::settings::require_login_on_sqlite(&mut tx).await?;
    sqlx::query(UPSERT_SETTING)
        .bind(crate::commissioning::KEY_LOCKED_DOWN)
        .bind("true")
        .execute(&mut *tx)
        .await
        .map_err(banto_storage::storage_error)?;
    // 組み込みの行は `kind` で引く（名前では引かない）。無い DB（直に書き換えた DB）は
    // ここでは直さない。行は id なしで残り、次の起動の検査がその DB を断る。
    let builtin_admin_id: Option<i64> =
        sqlx::query_scalar("SELECT id FROM users WHERE kind = 'builtin' ORDER BY id LIMIT 1")
            .fetch_optional(&mut *tx)
            .await
            .map_err(banto_storage::storage_error)?;
    let entity_id = user.id.to_string();
    let entry = |action: &'static str, resource: &'static str| AuditEntry {
        actor_username: Some(BUILTIN_ADMIN_USERNAME),
        actor_id: builtin_admin_id,
        actor_role: Some(Role::Admin.as_str()),
        actor_kind: Some(ACTOR_KIND_CLI),
        action,
        resource,
        entity_id: None,
        detail: None,
        origin: ORIGIN_CLI,
        result: "ok",
    };
    crate::audit::record_on_sqlite(
        &mut tx,
        &AuditEntry {
            entity_id: Some(&entity_id),
            detail: Some(user_create_detail(&user)),
            ..entry("create", "users")
        },
    )
    .await?;
    crate::audit::record_on_sqlite(
        &mut tx,
        &AuditEntry {
            entity_id: Some("1"),
            detail: Some(serde_json::json!({ "authDisabled": false, "via": "create-admin" })),
            ..entry("lock_down", "commissioning")
        },
    )
    .await?;
    tx.commit().await.map_err(banto_storage::storage_error)?;
    Ok(CreatedAdmin { user })
}

/// CLI の `create-admin` の本体（`banto-hub` の bin が引数とパスワードを読んでから呼ぶ）。
///
/// 1. 起動と同じ profile の排他を取る（`config.skip_profile_lock` でなければ）。Hub が動いて
///    いれば取れないので、何も書かずに止める（「Hub を止めてから」）。
/// 2. 起動と同じ `init_db`（v8 より前の DB・旧形式の DB はここで断る。migration を当てる）。
/// 3. [`create_admin_and_lock_down_on`]。
///
/// 排他と DB は、返る前に閉じて離す。
pub async fn run_create_admin(
    config: &crate::runtime::HubConfig,
    admin: NewAdmin<'_>,
) -> Result<CreatedAdmin, BantoError> {
    let _lock = if config.skip_profile_lock {
        None
    } else {
        // 起動（`HubRuntime::start`）と同じ root の解き方（同じ関数）。`config` は起動と
        // 同じ `build_hub_config_from_env` で組む（bin の `create_admin`）。
        let root = crate::profile_paths::resolve_hub_root_from_env();
        let paths = crate::profile_paths::resolve_profile_paths(&root, &config.profile_id)
            .map_err(|err| BantoError::Other(format!("profile の場所を決められません: {err}")))?;
        Some(
            crate::profile_lock::try_acquire_profile_lock(&paths, config.host_kind).map_err(
                |err| {
                    BantoError::Other(format!(
                        "Hub の profile を使えません（Hub・サービスが動いているなら止めてから、\
                         もう一度実行してください）。何も変更していません: {err}"
                    ))
                },
            )?,
        )
    };
    let pool = crate::db::init_db_with_registry(&config.db_path, &config.drivers.registry)
        .await
        .map_err(BantoError::from)?;
    let result = create_admin_and_lock_down_on(&pool, admin).await;
    // Windows は開いた SQLite のファイルを握るので、排他を離す前に閉じる。
    pool.close().await;
    result
}

/// 失敗の理由を、利用者が読む 1 行にする（検査の誤りは欄ごとの理由を並べる。予約名・
/// パスワードが短い、など。`BantoError::Validation` の本文は一般的な文言なので）。
pub fn failure_reason(err: &BantoError) -> String {
    match err {
        BantoError::Validation { field_errors } if !field_errors.is_empty() => field_errors
            .iter()
            .map(|field| field.message.as_str())
            .collect::<Vec<_>>()
            .join(" / "),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWNER: NewAdmin<'static> = NewAdmin {
        username: "owner",
        password: "password123",
        display_name: "オーナー",
    };

    #[tokio::test]
    async fn creates_the_admin_locks_down_and_requires_login_at_once() {
        let pool = crate::db::init_db_memory().await.unwrap();
        let settings = crate::settings::SettingsService::new(crate::db::Db::Sqlite(pool.clone()));
        // 新しい DB は試運転モード（フラグ未設定）・`auth.disabled = true`（0015）。
        assert!(!crate::commissioning::resolve_locked_down(&settings)
            .await
            .unwrap());
        assert!(settings.auth_config().await.unwrap().disabled);

        let created = create_admin_and_lock_down_on(&pool, OWNER).await.unwrap();
        assert_eq!(created.user.username, "owner");
        assert!(crate::commissioning::resolve_locked_down(&settings)
            .await
            .unwrap());
        assert!(!settings.auth_config().await.unwrap().disabled);
        let users = UsersService::new(crate::db::Db::Sqlite(pool.clone()));
        assert!(users.has_admin().await.unwrap());
        let rows: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE origin = 'cli' AND actor_username = 'admin' \
             AND actor_kind = 'cli' AND actor_id IS NOT NULL",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(rows, 2);
    }

    #[tokio::test]
    async fn a_refused_account_writes_nothing() {
        let pool = crate::db::init_db_memory().await.unwrap();
        for username in ["admin", "Admin", crate::commissioning::SYNTHETIC_ACTOR_ID] {
            let err = create_admin_and_lock_down_on(&pool, NewAdmin { username, ..OWNER })
                .await
                .unwrap_err();
            assert!(
                !matches!(err, BantoError::Storage(_)),
                "{username}: {err:?}"
            );
        }
        let settings = crate::settings::SettingsService::new(crate::db::Db::Sqlite(pool.clone()));
        assert!(!crate::commissioning::resolve_locked_down(&settings)
            .await
            .unwrap());
        assert!(settings.auth_config().await.unwrap().disabled);
        let accounts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE kind = 'account'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(accounts, 0);
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE origin = 'cli'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rows, 0);
    }
}
