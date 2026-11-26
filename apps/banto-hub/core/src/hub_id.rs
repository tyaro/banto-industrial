//! 設置ごとに変わらない Hub の id（#1295、2026-11-26 オーナー決定）。
//!
//! 目的: 消費側（temp-scada など）が、同じ URL のまま別の Hub に差し替えられたことを
//! 見分ける。`GET /api/v1/status` の `hub_id` として返す。`run_id` は収集の世代で、
//! Hub の同一性を表さない（必須でもない）ので使わない。
//!
//! - 置き場は設定 DB の `settings` テーブル（キー [`HUB_ID_KEY`]）。ChronoGazer の
//!   `db_generation`（`store_generation.rs`）と同じ考え方で、DB と運命を共にする。
//! - 値は乱数 16 バイトから作った UUID v4（小文字・ハイフン付き）。
//! - DB を作り直すと新しい id になる。DB のバックアップから戻すと、その DB が持つ id
//!   に戻る（戻す＝その Hub に戻る）。同じ id に戻るのは `hub.id` の入ったバックアップ
//!   （この版以降の DB）だけ。この版より前の DB のバックアップや、設定パッケージから
//!   作り直した新しい DB は、次の起動で新しい id になる（別の Hub として扱われる）。
//! - **限界**: id は「設定 DB の系譜」の印で、機械の識別ではない。同じバックアップを
//!   別の機械に戻すと、同じ id の Hub が 2 台できうる。
//! - id が同じでも、設定が同じ時点だとは限らない（同じ系譜の古いバックアップを戻した
//!   場合）。`revision`・`run_id` は稼働中の変更を知るための番号で、再起動で 0 から
//!   数え直すので（`CollectorManager`・`controller`）、再起動をまたいで設定の時点を
//!   見分ける値にはならない。消費側は、再接続のたびに今のカタログでタグの割り付けを
//!   照合する（オーナーのレビュー P2、2026-11-26）。
//!
//! 起動時に 1 回だけ「読む → 無ければ作って保存」する（[`load_or_create`]）。並行する
//! 起動の競合は `INSERT ... ON CONFLICT DO NOTHING` の後に読み直した値を正とすることで
//! 避ける。空や UUID として壊れた値は、黙って書き直さず起動のエラーにする。

use banto_core::BantoError;
use sqlx::SqlitePool;

/// 設定 DB のキー。
pub const HUB_ID_KEY: &str = "hub.id";

/// 16 バイトから UUID v4 の文字列を作る（version・variant のビットを立てる）。
fn uuid_v4_from_bytes(mut bytes: [u8; 16]) -> String {
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let mut out = String::with_capacity(36);
    for (i, byte) in bytes.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            out.push('-');
        }
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// 小文字・ハイフン付きの UUID v4 か。
pub fn is_valid_hub_id(value: &str) -> bool {
    let b = value.as_bytes();
    if b.len() != 36 {
        return false;
    }
    b.iter().enumerate().all(|(i, &c)| match i {
        8 | 13 | 18 | 23 => c == b'-',
        14 => c == b'4',
        19 => matches!(c, b'8' | b'9' | b'a' | b'b'),
        _ => c.is_ascii_digit() || (b'a'..=b'f').contains(&c),
    })
}

fn new_hub_id() -> Result<String, BantoError> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|err| BantoError::Other(format!("Hub の id を作れません: {err}")))?;
    Ok(uuid_v4_from_bytes(bytes))
}

fn db_err(err: sqlx::Error) -> BantoError {
    BantoError::Other(format!("Hub の id を設定 DB に読み書きできません: {err}"))
}

/// Hub の id を読む。無ければ作って書く（新しい DB の最初の起動）。
///
/// 壊れた値（空・UUID v4 でない）が入っていたらエラーを返す。直すには、設定 DB の
/// `settings` テーブルの `hub.id` の行を削除する（次の起動で新しい id ができる）。
pub async fn load_or_create(pool: &SqlitePool) -> Result<String, BantoError> {
    sqlx::query("INSERT INTO settings (key, value) VALUES (?, ?) ON CONFLICT(key) DO NOTHING")
        .bind(HUB_ID_KEY)
        .bind(new_hub_id()?)
        .execute(pool)
        .await
        .map_err(db_err)?;
    let stored: String = sqlx::query_scalar("SELECT value FROM settings WHERE key = ?")
        .bind(HUB_ID_KEY)
        .fetch_one(pool)
        .await
        .map_err(db_err)?;
    if !is_valid_hub_id(&stored) {
        return Err(BantoError::Other(format!(
            "設定 DB の `{HUB_ID_KEY}` が UUID として壊れています（値: {stored:?}）。\
             設定 DB の settings テーブルからこのキーの行を削除すると、次の起動で新しい id を作ります"
        )));
    }
    Ok(stored)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrate_memory;

    #[test]
    fn generated_id_is_lowercase_uuid_v4() {
        for _ in 0..50 {
            let id = new_hub_id().unwrap();
            assert!(is_valid_hub_id(&id), "{id}");
        }
        assert_eq!(
            uuid_v4_from_bytes([0xff; 16]),
            "ffffffff-ffff-4fff-bfff-ffffffffffff"
        );
        assert!(!is_valid_hub_id("FFFFFFFF-FFFF-4FFF-BFFF-FFFFFFFFFFFF"));
        assert!(!is_valid_hub_id(""));
    }

    #[tokio::test]
    async fn created_once_and_kept_on_the_same_db() {
        let pool = migrate_memory().await.unwrap();
        let first = load_or_create(&pool).await.unwrap();
        assert!(is_valid_hub_id(&first));
        assert_eq!(load_or_create(&pool).await.unwrap(), first);
    }

    #[tokio::test]
    async fn a_new_db_gets_a_different_id() {
        let a = load_or_create(&migrate_memory().await.unwrap())
            .await
            .unwrap();
        let b = load_or_create(&migrate_memory().await.unwrap())
            .await
            .unwrap();
        assert_ne!(a, b);
    }

    #[tokio::test]
    async fn broken_values_are_refused_not_rewritten() {
        let pool = migrate_memory().await.unwrap();
        for bad in ["", "not-a-uuid"] {
            sqlx::query(
                "INSERT INTO settings (key, value) VALUES (?, ?) \
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            )
            .bind(HUB_ID_KEY)
            .bind(bad)
            .execute(&pool)
            .await
            .unwrap();
            let err = load_or_create(&pool).await.unwrap_err().to_string();
            assert!(err.contains(HUB_ID_KEY), "{err}");
            let still: String = sqlx::query_scalar("SELECT value FROM settings WHERE key = ?")
                .bind(HUB_ID_KEY)
                .fetch_one(&pool)
                .await
                .unwrap();
            assert_eq!(still, bad);
        }
    }
}
