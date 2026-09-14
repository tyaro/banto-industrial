//! 書き込み受付状態 (docs/tag-server-design.md §6-6)。[`WriteControl`] は
//! 「この hub プロセスはいま `/api/v1/values/{tag}` への書き込みを受け付けて
//! よいか」を持つ、ロックフリーの薄いフラグ保持者。
//!
//! ## ルール: 既定は有効、再起動では永続値をそのまま復元する
//!
//! (2026-09-09 オーナー決定, #340 - 旧ルール「起動時は必ず disabled」を撤回)
//!
//! [`WriteControl::new`] は永続テーブル `write_control_state`
//! (`db.rs::apply_app_schema` 参照) の `enabled_persisted` を読んだ値
//! (`was_enabled_persisted` 引数) を、そのままライブの `enabled` フラグの
//! 初期値として使う。seed は `enabled_persisted = 1` (既定で書き込み可)。
//!
//! banto-hub は relay-wright のようなルールエンジンを持たない、外部クライ
//! アントの明示要求を1回転送するだけのパススルーであり、再起動後に条件が
//! 揃えば自律的に書き込みを再開するという危険が存在しない。書き込みの
//! 可否は per-tag `writable` (既定 false) と API キーの `write` スコープが
//! 実質担っており、このグローバルトグルは運用者が手で書き込みを止める
//! **非常停止スイッチ**に徹する。手動で無効化した状態はプロセス再起動を
//! 跨いで保持される (`WriteControl::new` が永続値をそのまま復元するため)。
//! [`WriteControl::was_enabled_before_restart`] はこの「起動時に永続テーブル
//! から復元した値」を指す名称として残す (外部クライアント Thermal Monitor
//! が `GET /api/v1/status` の `write_was_enabled_before_restart` を読んでいる
//! ため、フィールド名・REST フィールド名とも互換のため変更しない)。
//!
//! ## Pure, sync, DB-free
//!
//! このフラグ自体は `AtomicBool` のみで DB を持たない。永続化
//! ([`persist_enabled`]) と読み出し ([`load_persisted_enabled`]) はこの
//! モジュールの自由関数として提供し、呼び出し元 (`crate::rest` の管理 REST
//! ハンドラ) が `WriteControl::enable`/`disable` と対にして呼ぶ。

use std::sync::atomic::{AtomicBool, Ordering};

use banto_core::BantoError;
use sqlx::SqlitePool;

/// ライブの「書き込み受付中か」フラグ + 起動時に復元した永続値。
/// `Arc` で共有する前提 (安価に clone できる `AtomicBool` の塊)。
#[derive(Debug)]
pub struct WriteControl {
    /// ライブの「/api/v1/values/{tag} への書き込みを受け付けるか」フラグ。
    /// 構築時は `was_enabled_persisted` で復元される (このモジュールの
    /// doc comment 参照)。
    enabled: AtomicBool,
    /// 起動時に永続テーブルから読んだ値 (= 構築時のライブフラグの初期値)。
    /// [`Self::was_enabled_before_restart`] 参照。
    was_enabled_before_restart: bool,
}

impl WriteControl {
    /// ライブの `enabled` を `was_enabled_persisted` で復元して構築する。
    /// `was_enabled_persisted` は `write_control_state.enabled_persisted`
    /// から読んだ値をそのまま渡す。
    pub fn new(was_enabled_persisted: bool) -> Self {
        Self {
            enabled: AtomicBool::new(was_enabled_persisted),
            was_enabled_before_restart: was_enabled_persisted,
        }
    }

    pub fn enable(&self) {
        self.enabled.store(true, Ordering::SeqCst);
    }

    pub fn disable(&self) {
        self.enabled.store(false, Ordering::SeqCst);
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::SeqCst)
    }

    /// 起動時に永続テーブルから復元した値
    /// (`GET /api/v1/status` の `write_was_enabled_before_restart`)。
    /// [`WriteControl::new`] がライブフラグの初期値としてそのまま使った
    /// 値と同じ (以後の enable/disable ではこの値自体は変わらない)。
    pub fn was_enabled_before_restart(&self) -> bool {
        self.was_enabled_before_restart
    }
}

/// `write_control_state.enabled_persisted` (id=1 の単一行) を読む。
/// `db.rs::apply_app_schema` が起動時に必ず1行 seed するので
/// `fetch_one` で問題ない。[`WriteControl::new`] に渡す
/// `was_enabled_persisted` の取得元であり、そのままライブフラグの初期値
/// として使われる (このモジュールの doc comment 参照)。
pub async fn load_persisted_enabled(pool: &SqlitePool) -> Result<bool, BantoError> {
    let enabled: i64 =
        sqlx::query_scalar("SELECT enabled_persisted FROM write_control_state WHERE id = 1")
            .fetch_one(pool)
            .await
            .map_err(banto_storage::storage_error)?;
    Ok(enabled != 0)
}

/// `write_control_state` の永続値を更新する (誰が・いつ変更したかも記録)。
/// この値は次回起動時のライブフラグの初期値になる
/// (`WriteControl::new` がそのまま復元するため)。
/// `crate::rest` の `POST /api/write-control/enable|disable` ハンドラが
/// `WriteControl::enable`/`disable` と対で呼ぶ。
pub async fn persist_enabled(
    pool: &SqlitePool,
    enabled: bool,
    actor: Option<&str>,
) -> Result<(), BantoError> {
    sqlx::query(
        "UPDATE write_control_state \
         SET enabled_persisted = ?, last_changed_at = datetime('now'), last_changed_by = ? \
         WHERE id = 1",
    )
    .bind(enabled as i64)
    .bind(actor)
    .execute(pool)
    .await
    .map_err(banto_storage::storage_error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrate_memory;

    #[test]
    fn constructs_with_live_flag_restored_from_persisted_value() {
        let control = WriteControl::new(true);
        assert!(
            control.is_enabled(),
            "live enabled flag must be restored to true when persisted value is true \
             (2026-09-09 オーナー決定 #340: 既定は有効、再起動では永続値を復元)"
        );
        assert!(control.was_enabled_before_restart());

        let control = WriteControl::new(false);
        assert!(
            !control.is_enabled(),
            "live enabled flag must be restored to false when persisted value is false"
        );
        assert!(!control.was_enabled_before_restart());
    }

    #[test]
    fn enable_disable_round_trips() {
        let control = WriteControl::new(false);
        assert!(!control.is_enabled());
        control.enable();
        assert!(control.is_enabled());
        control.disable();
        assert!(!control.is_enabled());
    }

    #[tokio::test]
    async fn persisted_state_seeds_enabled_and_round_trips_through_persist() {
        let pool = migrate_memory().await.expect("migrate_memory");
        assert!(
            load_persisted_enabled(&pool).await.unwrap(),
            "write_control_state should seed enabled_persisted=1 (既定は書き込み可, #340)"
        );

        persist_enabled(&pool, false, Some("admin")).await.unwrap();
        assert!(!load_persisted_enabled(&pool).await.unwrap());

        let by: Option<String> =
            sqlx::query_scalar("SELECT last_changed_by FROM write_control_state WHERE id = 1")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(by.as_deref(), Some("admin"));
    }

    /// #340: 永続値を enabled にしてから `WriteControl::new` すると、
    /// その値がそのままライブフラグとして復元される
    /// (load_persisted_enabled -> new の組み合わせそのものを確認する)。
    #[tokio::test]
    async fn a_new_write_control_from_a_persisted_enabled_state_is_enabled() {
        let pool = migrate_memory().await.expect("migrate_memory");
        persist_enabled(&pool, true, Some("admin")).await.unwrap();

        let persisted = load_persisted_enabled(&pool).await.unwrap();
        let control = WriteControl::new(persisted);
        assert!(
            control.is_enabled(),
            "restart must restore the persisted enabled value"
        );
        assert!(control.was_enabled_before_restart());
    }

    /// #340: 運用者が手で disable した状態は、プロセス再起動 (=
    /// 新しい `WriteControl::new` の構築) を跨いで保持される。
    #[tokio::test]
    async fn manual_disable_persists_across_restart() {
        let pool = migrate_memory().await.expect("migrate_memory");
        // seed は enabled=1 なので、まず明示的に disable する。
        persist_enabled(&pool, false, Some("admin")).await.unwrap();

        let persisted = load_persisted_enabled(&pool).await.unwrap();
        let control = WriteControl::new(persisted);
        assert!(
            !control.is_enabled(),
            "manual disable must survive a simulated restart"
        );
        assert!(!control.was_enabled_before_restart());
    }
}
