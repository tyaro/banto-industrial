//! ChronoGazer の最初の管理者（banto v8、ADR-0034 §7・docs/banto-v8-upgrade-plan.md §2 の 3）。
//!
//! banto v8 で新しい DB はログインなし（`auth.disabled = true`、上流の `0015`）で始まる。
//! デスクトップはログインなしの間にユーザー管理でアカウントを作り、設定でログイン必須へ
//! 切り替える（`SettingsService::set_auth_config`）。**サービス（`chronogazer-server`）には
//! その口が無い**（`PUT /api/settings/auth` はログインなしの間 400）ので、ログインなしの
//! まま、loopback の peer と Host なら誰でも admin の `localDesktop` を受け取れてしまう
//! （`crate::local_grant` の `LocalGrantStart::NoLogin`）。v7 は最初の管理者を作ると
//! 失効して既定 off に落ちていた。
//!
//! そこで Hub（`banto_hub_core::first_admin`）・上流の `banto-serve create-admin` と同じ形の
//! CLI `chronogazer-server create-admin --username <名前> [--display-name <表示名>]`
//! （パスワードは標準入力）で最初の管理者を作る。**ネットワーク越しには作らない**。
//! 開発用の `banto-serve create-admin` も同じ [`create_admin_and_require_login_on`] を使う。
//!
//! 1 回の実行で、次を**1 つのトランザクション**で書く（どれかが失敗したら何も残らない）:
//!
//! 1. 管理者のアカウント（ユーザー管理と同じ検査。予約名 `admin`・
//!    [`crate::local_grant::reserved_usernames`] は作れない）。
//! 2. ログイン必須（`auth.disabled = false`）。この後の起動は `LocalGrantStart::Initialized`
//!    になり、`localDesktop` は設定 `auth.local_grant`（既定 off）どおりに開く・閉じる。
//! 3. 監査の行（組み込みの `admin` が CLI（`actor_kind = cli`）で行った操作として）。
//!
//! サービス・アプリを止めてから実行する（動いているエンジンはセッションと grant の形を
//! メモリに持っていて、ここからは終わらせられない）。止まっていることは、起動と同じ A1 の
//! 排他を取って確かめる（[`run_create_admin`]）。予約されたリストアがあれば何も書かずに
//! 止める（上流と同じ。適用すると予約を勝手に進め、無視すると次の起動が適用して作った
//! アカウントを消すため。どちらを先にするかは利用者が決める）。

use std::path::Path;

use banto_core::BantoError;
use sqlx::SqlitePool;

use crate::audit::{AuditEntry, ACTOR_KIND_CLI};
use crate::backup::BackupService;
use crate::instance_lock::ChronoGazerHost;
use crate::paths::{DataLayout, DirAccessSource};
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

/// [`create_admin_and_require_login_on`] が作ったアカウント。
#[derive(Debug, Clone)]
pub struct CreatedAdmin {
    pub user: UserIdentity,
}

/// migration を当て終えた設定 DB に、管理者・ログイン必須・監査の行を 1 つの
/// トランザクションで書く（モジュール doc）。既にログイン必須の DB でも動く（同じ値を
/// 書き直す。管理者を作り直す復旧の用途）。
pub async fn create_admin_and_require_login_on(
    pool: &SqlitePool,
    admin: NewAdmin<'_>,
) -> Result<CreatedAdmin, BantoError> {
    let users = UsersService::new(crate::db::Db::Sqlite(pool.clone()))
        .with_reserved_usernames(crate::local_grant::reserved_usernames());
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
    crate::settings::require_login_on_sqlite(&mut tx).await?;
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
            detail: Some(serde_json::json!({ "authDisabled": false, "via": "create-admin" })),
            ..entry("settings_change", "settings")
        },
    )
    .await?;
    tx.commit().await.map_err(banto_storage::storage_error)?;
    Ok(CreatedAdmin { user })
}

/// SQLite の DB `db_path` に予約されたリストアがあれば、CLI を止める誤りにする（ファイルは
/// 探すだけ。開かない・適用しない・消さない。上流の `first_account` と同じ）。
pub fn refuse_a_pending_restore(db_path: &Path) -> Result<(), BantoError> {
    let pending = BackupService::pending_restore_file(db_path)?;
    match pending.try_exists() {
        Ok(false) => Ok(()),
        Ok(true) => Err(BantoError::Other(format!(
            "予約されたリストアがあります（{}）。何も変更していません。\
             先にリストアを適用する（サービス・アプリを一度起動する）か、\
             このファイルを消してから、もう一度実行してください",
            pending.display()
        ))),
        Err(err) => Err(BantoError::Other(format!(
            "予約されたリストアの有無を確かめられませんでした（{}）: {err}。何も変更していません",
            pending.display()
        ))),
    }
}

/// `chronogazer-server create-admin` の本体（bin が引数とパスワードを読んでから呼ぶ）。
///
/// 1. 起動と同じ A1 の排他を取る（`crate::server_host::lock`。ルートは起動と同じ
///    `DataLayout`）。サービス・コンソール・デスクトップが同じデータを使っていれば
///    取れないので、何も書かずに止める（「止めてから」）。
/// 2. 予約されたリストアがあれば止める（[`refuse_a_pending_restore`]）。
/// 3. 起動と同じ DB の開き方（`LockedServer::open`。v8 より前の DB はここで断る。
///    migration を当てる）。
/// 4. [`create_admin_and_require_login_on`]。
///
/// 排他と DB は、返る前に閉じて離す。誤りは利用者が読む文にして返す。
pub async fn run_create_admin(
    layout: DataLayout,
    admin: NewAdmin<'_>,
) -> Result<CreatedAdmin, String> {
    let locked = crate::server_host::lock(
        layout.clone(),
        ChronoGazerHost::Server,
        &DirAccessSource::CurrentProcess,
    )
    .map_err(|failure| {
        format!(
            "データのルート（{}）を使えません（サービス・アプリが動いているなら止めてから、\
             もう一度実行してください）。何も変更していません: {}",
            layout.root().display(),
            failure.message
        )
    })?;
    refuse_a_pending_restore(&layout.db_path()).map_err(|err| failure_reason(&err))?;
    let opened = locked
        .open()
        .await
        .map_err(|failure| format!("DB を開けませんでした: {}", failure.message))?;
    let result = create_admin_and_require_login_on(opened.pool(), admin).await;
    // Windows は開いた SQLite のファイルを握るので、排他を離す前に閉じる。
    opened.close().await;
    result.map_err(|err| failure_reason(&err))
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
    use crate::local_grant::{LocalGrantSetting, LocalGrantStart};
    use crate::settings::SettingsService;

    const OWNER: NewAdmin<'static> = NewAdmin {
        username: "owner",
        password: "password123",
        display_name: "オーナー",
    };

    /// 起動時に決まる grant の形と、その形の条件（`GrantSpec::enabled`）をいまの DB で読む。
    async fn grant_at_start(pool: &SqlitePool) -> (LocalGrantStart, bool) {
        let db = crate::db::Db::Sqlite(pool.clone());
        let settings = SettingsService::new(db.clone());
        let start = LocalGrantStart::resolve(&settings, "test").await;
        let spec = crate::local_grant::local_desktop_grant_spec(
            crate::local_grant::LocalAccess {
                start,
                always_on_listener: true,
            },
            UsersService::new(db),
            settings,
        );
        let open = (spec.enabled)().await.unwrap();
        (start, open)
    }

    #[tokio::test]
    async fn creates_the_admin_and_requires_login_at_once() {
        let pool = crate::db::init_db_memory().await.unwrap();
        let settings = SettingsService::new(crate::db::Db::Sqlite(pool.clone()));
        assert!(settings.auth_config().await.unwrap().disabled);

        let created = create_admin_and_require_login_on(&pool, OWNER)
            .await
            .unwrap();
        assert_eq!(created.user.username, "owner");
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
        // 起動時の検査を通る（ログイン必須で、ログインできる管理者がいる）。
        crate::db::startup_check(&crate::db::Db::Sqlite(pool.clone()))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_refused_account_writes_nothing() {
        let pool = crate::db::init_db_memory().await.unwrap();
        for username in ["admin", "Admin", crate::local_grant::LOCAL_GRANT_ACTOR_ID] {
            let err = create_admin_and_require_login_on(&pool, NewAdmin { username, ..OWNER })
                .await
                .unwrap_err();
            assert!(
                !matches!(err, BantoError::Storage(_)),
                "{username}: {err:?}"
            );
        }
        let short = create_admin_and_require_login_on(
            &pool,
            NewAdmin {
                password: "x",
                ..OWNER
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(short, BantoError::Validation { .. }), "{short:?}");
        let settings = SettingsService::new(crate::db::Db::Sqlite(pool.clone()));
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

    /// P1 の本体: 新しい DB のサービスは loopback に admin の grant を出す → create-admin の
    /// 後の起動では既定 off で出ない → 設定を admin にすれば v7 と同じく出る。
    #[tokio::test]
    async fn the_local_grant_follows_the_setting_after_create_admin() {
        let pool = crate::db::init_db_memory().await.unwrap();
        let (start, open) = grant_at_start(&pool).await;
        assert_eq!(start, LocalGrantStart::NoLogin(Role::Admin));
        assert_eq!(start.issued_role(), Some(Role::Admin));
        assert!(open);

        create_admin_and_require_login_on(&pool, OWNER)
            .await
            .unwrap();
        let (start, open) = grant_at_start(&pool).await;
        assert_eq!(start, LocalGrantStart::Initialized(LocalGrantSetting::Off));
        assert_eq!(start.issued_role(), None);
        assert!(!open);

        let settings = SettingsService::new(crate::db::Db::Sqlite(pool.clone()));
        crate::local_grant::write_setting(&settings, LocalGrantSetting::Role(Role::Admin))
            .await
            .unwrap();
        let (start, open) = grant_at_start(&pool).await;
        assert_eq!(start.issued_role(), Some(Role::Admin));
        assert!(open);
    }

    #[tokio::test]
    async fn the_running_process_keeps_the_root_and_nothing_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let layout = DataLayout::new(dir.path().join("root"));
        let held = crate::server_host::lock(
            layout.clone(),
            ChronoGazerHost::Server,
            &DirAccessSource::CurrentProcess,
        )
        .ok()
        .unwrap();
        let err = run_create_admin(layout.clone(), OWNER).await.unwrap_err();
        assert!(err.contains("何も変更していません"), "{err}");
        assert!(!layout.db_path().exists());
        drop(held);

        let created = run_create_admin(layout.clone(), OWNER).await.unwrap();
        assert_eq!(created.user.username, "owner");
        // 2 回目は同じ名前を断る（排他と DB は 1 回目が離している）。
        let again = run_create_admin(layout, OWNER).await.unwrap_err();
        assert!(!again.contains("使えません"), "{again}");
    }

    #[tokio::test]
    async fn a_pending_restore_stops_it_before_the_database() {
        let dir = tempfile::tempdir().unwrap();
        let layout = DataLayout::new(dir.path().join("root"));
        std::fs::create_dir_all(layout.root()).unwrap();
        let pending = BackupService::pending_restore_file(&layout.db_path()).unwrap();
        std::fs::create_dir_all(pending.parent().unwrap()).unwrap();
        std::fs::write(&pending, b"staged").unwrap();
        let err = run_create_admin(layout.clone(), OWNER).await.unwrap_err();
        assert!(err.contains("予約されたリストア"), "{err}");
        assert!(!layout.db_path().exists());
        assert!(pending.exists());
    }
}
