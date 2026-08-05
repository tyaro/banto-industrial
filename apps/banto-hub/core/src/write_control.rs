//! 書き込み受付状態 (docs/tag-server-design.md §6-6, relay-wright の
//! `engine/arming.rs` と同型)。[`WriteControl`] は「この hub プロセスは
//! いま `/api/v1/values/{tag}` への書き込みを受け付けてよいか」を持つ、
//! ロックフリーの薄いフラグ保持者。
//!
//! ## 一番大事なルール: 起動時は必ず disabled
//!
//! [`WriteControl::new`] は永続値に関わらず、ライブの `enabled` フラグを
//! 常に `false` で構築する。永続テーブル `write_control_state`
//! (`db.rs::apply_app_schema` 参照) の `enabled_persisted` は
//! [`WriteControl::was_enabled_before_restart`] にのみ反映され、ライブの
//! フラグには一切影響しない。
//!
//! これは relay-wright の arming 同様「唯一のルール」— プロセス再起動
//! (クラッシュ・電源断・再デプロイ) が黙って書き込み受付を再開しては
//! ならない、というのが hub の書き込み安全設計そのものの前提
//! (§6 item 6: 「書き込み受付は起動時 disabled とし、管理 UI で明示的に
//! 有効化する」)。管理者が `POST /api/write-control/enable` を叩くまで、
//! `/api/v1/values/{tag}` は常に 503 `writes_disabled` を返す
//! (`crate::rest`)。
//!
//! ## Pure, sync, DB-free
//!
//! [`ArmingState`](relay-wright) と同じく、このフラグ自体は `AtomicBool`
//! のみで DB を持たない。永続化 ([`persist_enabled`]) と読み出し
//! ([`load_persisted_enabled`]) はこのモジュールの自由関数として提供し、
//! 呼び出し元 (`crate::rest` の管理 REST ハンドラ) が
//! `WriteControl::enable`/`disable` と対にして呼ぶ。relay-wright は
//! この2つを `engine::write_audit` 側に置いているが、hub には対応する
//! モジュールがまだないため、ここに同居させる (状態機械とその永続化が
//! 1ファイルにまとまっている方が追いやすいという判断- 状態自体が小さい
//! ため分割の実益がない)。

use std::sync::atomic::{AtomicBool, Ordering};

use banto_core::BantoError;
use sqlx::SqlitePool;

/// ライブの「書き込み受付中か」フラグ + 再起動前の永続値 (表示専用)。
/// `Arc` で共有する前提 (安価に clone できる `AtomicBool` の塊)。
#[derive(Debug)]
pub struct WriteControl {
    /// ライブの「/api/v1/values/{tag} への書き込みを受け付けるか」フラグ。
    /// 構築時は常に `false`(このモジュールの doc comment 参照)。
    enabled: AtomicBool,
    /// 起動時に読んだ永続値 - 表示専用 ([`Self::was_enabled_before_restart`])。
    was_enabled_before_restart: bool,
}

impl WriteControl {
    /// ライブの `enabled` を常に `false` に固定して構築する。
    /// `was_enabled_persisted` は `write_control_state.enabled_persisted`
    /// から読んだ値をそのまま渡す - [`Self::was_enabled_before_restart`]
    /// 経由でのみ観測でき、ライブフラグには反映されない。
    pub fn new(was_enabled_persisted: bool) -> Self {
        Self {
            enabled: AtomicBool::new(false),
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

    /// 起動時に永続テーブルから読んだ値 - UI 表示専用
    /// (`GET /api/v1/status` の `write_was_enabled_before_restart`)。
    /// ライブの受付可否には一切影響しない。
    pub fn was_enabled_before_restart(&self) -> bool {
        self.was_enabled_before_restart
    }
}

/// `write_control_state.enabled_persisted` (id=1 の単一行) を読む。
/// `db.rs::apply_app_schema` が起動時に必ず1行 seed するので
/// `fetch_one` で問題ない。[`WriteControl::new`] に渡す
/// `was_enabled_persisted` の取得元 - **ライブフラグにはしない**
/// (このモジュールの doc comment 参照)。
pub async fn load_persisted_enabled(pool: &SqlitePool) -> Result<bool, BantoError> {
    let enabled: i64 =
        sqlx::query_scalar("SELECT enabled_persisted FROM write_control_state WHERE id = 1")
            .fetch_one(pool)
            .await
            .map_err(banto_storage::storage_error)?;
    Ok(enabled != 0)
}

/// `write_control_state` の永続値を更新する (誰が・いつ変更したかも記録)。
/// これは履歴/UI 表示専用で、次回起動時のライブフラグを決めるものでは
/// **ない** (`WriteControl::new` が常に `false` で構築するため)。
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
    fn constructs_disabled_even_when_persisted_enabled() {
        let control = WriteControl::new(true);
        assert!(
            !control.is_enabled(),
            "live enabled flag must be false on construction regardless of persisted value \
             (T2-4 の安全ゲートの核: 再起動が黙って書き込み受付を再開してはならない)"
        );
        assert!(
            control.was_enabled_before_restart(),
            "persisted enabled=true must survive as informational history"
        );
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
    async fn persisted_state_seeds_disabled_and_round_trips_through_persist() {
        let pool = migrate_memory().await.expect("migrate_memory");
        assert!(
            !load_persisted_enabled(&pool).await.unwrap(),
            "write_control_state should seed enabled_persisted=0"
        );

        persist_enabled(&pool, true, Some("admin")).await.unwrap();
        assert!(load_persisted_enabled(&pool).await.unwrap());

        let by: Option<String> =
            sqlx::query_scalar("SELECT last_changed_by FROM write_control_state WHERE id = 1")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(by.as_deref(), Some("admin"));
    }

    /// T2-4 のテスト計画5「再起動安全」: 永続値を enabled にしてから
    /// `WriteControl::new` すると disabled になっている、という
    /// load_persisted_enabled -> new の組み合わせそのものを確認する。
    #[tokio::test]
    async fn a_new_write_control_from_a_persisted_enabled_state_is_disabled() {
        let pool = migrate_memory().await.expect("migrate_memory");
        persist_enabled(&pool, true, Some("admin")).await.unwrap();

        let persisted = load_persisted_enabled(&pool).await.unwrap();
        let control = WriteControl::new(persisted);
        assert!(!control.is_enabled(), "restart must default to disabled");
        assert!(control.was_enabled_before_restart());
    }
}
