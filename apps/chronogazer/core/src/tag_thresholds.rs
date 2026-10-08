//! 記録計の側のタグごとのしきい値（#532、recorder-requirements.md §3.1・
//! §3.7.3 の (0b)）。
//!
//! 2026-10-08 オーナー決定: しきい値 H / HH / L / LL は**データ点（タグ定義）の
//! 性質ではなく、使う側（記録計・SCADA）が持つ設定**。ChronoGazer は自分の DB の
//! `recorder_tag_settings`（`migrations-sqlite/0102_recorder_tag_settings.sql`）に
//! 持ち、banto-tags の `tags.threshold_*` の列は**読みも書きもしない**（列そのものは
//! banto-hub が使っているので #533 まで残る。ChronoGazer のタグの保存は常に
//! `None` を渡す - `crate::rest::TagPayload`）。
//!
//! - **既定は設定なし**。行が無いタグはしきい値なし（色・帯・しきい値イベントの
//!   どれも出さない）。行があって 4 つとも `null` のときも同じ。
//! - **検証はこのモジュールの [`validate_thresholds_for`] 1 か所**（REST・Tauri
//!   共通。のちの関所・MCP もここを通す）: 対象タグの存在、文字列タグには付けない、
//!   有限の数、大小関係（`banto_tags::validate_thresholds` をそのまま使う - 規則と
//!   フィールド名 `thresholdLl`/`thresholdL`/`thresholdH`/`thresholdHh` が
//!   banto-tags と同じになる）。
//! - **更新は全項目置換**（省略した欄は「設定なし」になる）。`expectedRevision` が
//!   今の版と違えば `{kind:"validation", field_errors:[{field:"expectedRevision"}]}`
//!   で拒否する（`crate::revision`。REST は `409`、Tauri は同じ形の検証エラー）。
//!   行が無いタグの版は **0**（GET が返す `revision: 0` をそのまま送れば通る）。
//!   省略した更新は版を確かめない（タグ・表示グループと同じ扱い。画面は常に送る）。
//! - 「版の確認 → 検証 → 書き込み」は `BEGIN IMMEDIATE` の中で行う
//!   （docs/implementation-checklist.md §5。読んでから書くトランザクション）。
//!   行は消さずに残す（全部消した保存も版を進める）ので、消して作り直したときに
//!   版が 0 に戻って古い画面の保存が通る、ということが起きない。
//! - タグの削除と一緒に消す（[`delete_for_tag_on`]）。呼ぶのはタグの削除の
//!   トランザクション（`DisplayGroupService::delete_tag_unless_referenced`）。
//!
//! ## 収集への反映
//!
//! 収集は**開始（再起動）の時点の**設定で判定する（[`collect_thresholds`] を
//! `crate::collect` の開始が読み、`banto_collect::build_config_lenient_with_thresholds`
//! に渡す）。保存しても走っている収集には反映されない - タグの保存と同じ約束
//! （recorder-requirements.md §3.7.2「手動で変更したときの今の動作」）。判定に
//! 使った値はしきい値イベントに残る（`collect_events.limit_value`）。
//!
//! ## 点の指し方
//!
//! 今はタグ ID（`tags.id`）。Hub 経由のタグ（#383）をどう載せるかは migration
//! `0102` の注記を参照。

use std::collections::HashMap;

use banto_core::{BantoError, FieldError};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{SqliteConnection, SqlitePool};

use crate::revision::revision_conflict_error;

/// 監査の `resource`（REST・Tauri 共通）。
pub const AUDIT_RESOURCE: &str = "tag_thresholds";

/// 版の食い違いの案内（画面にそのまま出る）。
pub const REVISION_CONFLICT_MESSAGE: &str = "他の人（または別の画面）がこのタグのしきい値を先に更新しました。再読み込みしてから、もう一度編集して保存してください。";

/// 1 つのタグのしきい値（読み取りの応答）。`revision` は楽観ロックの版で、行が
/// 無い（一度も設定していない）タグは 0。
#[derive(Debug, Clone, PartialEq, Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct TagThresholds {
    pub tag_id: i64,
    pub threshold_ll: Option<f64>,
    pub threshold_l: Option<f64>,
    pub threshold_h: Option<f64>,
    pub threshold_hh: Option<f64>,
    pub revision: i64,
}

impl TagThresholds {
    /// 一度も設定していないタグ（行が無い）の形。
    pub fn unset(tag_id: i64) -> Self {
        Self {
            tag_id,
            threshold_ll: None,
            threshold_l: None,
            threshold_h: None,
            threshold_hh: None,
            revision: 0,
        }
    }

    /// 4 つとも設定なしか。
    pub fn is_empty(&self) -> bool {
        self.threshold_ll.is_none()
            && self.threshold_l.is_none()
            && self.threshold_h.is_none()
            && self.threshold_hh.is_none()
    }
}

/// 更新の本文（全項目置換。省略した欄は設定なし）。読み取りの応答
/// （[`TagThresholds`]）をそのまま送り返せる（`tagId`/`revision` は無視し、版は
/// `expectedRevision` で渡す）。
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TagThresholdsPayload {
    #[serde(default)]
    pub threshold_ll: Option<f64>,
    #[serde(default)]
    pub threshold_l: Option<f64>,
    #[serde(default)]
    pub threshold_h: Option<f64>,
    #[serde(default)]
    pub threshold_hh: Option<f64>,
    /// 楽観ロック: 編集を始めたときの `revision`（行が無ければ 0）。省略すると
    /// 版を確かめない。
    #[serde(default)]
    pub expected_revision: Option<i64>,
}

/// 更新の結果（監査の `detail` にタグ名を残すため、名前も返す）。
#[derive(Debug, Clone, PartialEq)]
pub struct UpdatedThresholds {
    pub thresholds: TagThresholds,
    pub tag_name: String,
}

/// 監査の `detail`（REST・Tauri 共通）。しきい値は秘密ではなく、警報の設定の
/// 変更の記録として値そのものに意味があるので、4 つとも残す。
pub fn audit_detail(updated: &UpdatedThresholds) -> Value {
    let t = &updated.thresholds;
    json!({
        "name": updated.tag_name,
        "thresholdLl": t.threshold_ll,
        "thresholdL": t.threshold_l,
        "thresholdH": t.threshold_h,
        "thresholdHh": t.threshold_hh,
    })
}

fn storage(err: sqlx::Error) -> BantoError {
    banto_storage::storage_error(err)
}

fn tag_not_found(tag_id: i64) -> BantoError {
    BantoError::NotFound {
        resource: "tags".to_string(),
        id: tag_id.to_string(),
    }
}

const FIELDS: [&str; 4] = ["thresholdLl", "thresholdL", "thresholdH", "thresholdHh"];

/// しきい値の検証（純関数。DB を見る部分 - タグの存在と型 - は呼び出し側が
/// `data_type` を渡す）。誤りはすべて `field_errors` に並べる。
///
/// - 文字列タグ（`data_type == "string"`）には付けられない（banto-tags の
///   タグの検証と同じ文言）。
/// - 有限の数であること（JSON は NaN・無限大を運べないが、のちの経路に備える）。
/// - 大小関係 `LL <= L <= H <= HH`（設定されたものだけ比べる。
///   `banto_tags::validate_thresholds` そのもの）。
pub fn validate_thresholds_for(
    data_type: &str,
    payload: &TagThresholdsPayload,
) -> Result<(), BantoError> {
    let values = [
        payload.threshold_ll,
        payload.threshold_l,
        payload.threshold_h,
        payload.threshold_hh,
    ];
    let mut errors = Vec::new();
    for (field, value) in FIELDS.iter().zip(values) {
        match value {
            Some(_) if data_type == banto_tags::STRING_DATA_TYPE => errors.push(FieldError {
                field: field.to_string(),
                message: "string 型ではしきい値を設定できません".to_string(),
            }),
            Some(v) if !v.is_finite() => errors.push(FieldError {
                field: field.to_string(),
                message: "有限の数値を入力してください".to_string(),
            }),
            _ => {}
        }
    }
    if errors.is_empty() {
        if let Err(BantoError::Validation { field_errors }) = banto_tags::validate_thresholds(
            payload.threshold_ll,
            payload.threshold_l,
            payload.threshold_h,
            payload.threshold_hh,
        ) {
            errors.extend(field_errors);
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(BantoError::Validation {
            field_errors: errors,
        })
    }
}

/// `recorder_tag_settings` を [`TagThresholds`] の形で読む SELECT（`sqlx` 0.9 は
/// 静的な SQL 文字列しか受け取らないので、`format!` ではなく `concat!` で組む）。
macro_rules! select_thresholds {
    ($tail:literal) => {
        concat!(
            "SELECT tag_id, threshold_ll, threshold_l, threshold_h, threshold_hh, revision \
             FROM recorder_tag_settings",
            $tail
        )
    };
}

async fn row_on(
    conn: &mut SqliteConnection,
    tag_id: i64,
) -> Result<Option<TagThresholds>, BantoError> {
    sqlx::query_as::<_, TagThresholds>(select_thresholds!(" WHERE tag_id = ?"))
        .bind(tag_id)
        .fetch_optional(&mut *conn)
        .await
        .map_err(storage)
}

/// タグの名前とデータ型（存在の確認を兼ねる）。
async fn tag_on(
    conn: &mut SqliteConnection,
    tag_id: i64,
) -> Result<Option<(String, String)>, BantoError> {
    sqlx::query_as::<_, (String, String)>("SELECT name, data_type FROM tags WHERE id = ?")
        .bind(tag_id)
        .fetch_optional(&mut *conn)
        .await
        .map_err(storage)
}

/// タグの削除と一緒に、そのタグのしきい値の行を消す。タグの削除と**同じ
/// トランザクション**の中で呼ぶ（`DisplayGroupService::delete_tag_unless_referenced`）。
pub(crate) async fn delete_for_tag_on(
    conn: &mut SqliteConnection,
    tag_id: i64,
) -> Result<(), BantoError> {
    sqlx::query("DELETE FROM recorder_tag_settings WHERE tag_id = ?")
        .bind(tag_id)
        .execute(&mut *conn)
        .await
        .map_err(storage)?;
    Ok(())
}

/// 収集の開始が使う「タグ ID → しきい値」（モジュール doc「収集への反映」）。
/// 4 つとも設定なしの行は載せない（載せなくても「設定なし」で同じ）。
pub async fn collect_thresholds(
    pool: &SqlitePool,
) -> Result<HashMap<i64, banto_collect::Thresholds>, BantoError> {
    let rows = sqlx::query_as::<_, TagThresholds>(select_thresholds!(""))
        .fetch_all(pool)
        .await
        .map_err(storage)?;
    Ok(rows
        .into_iter()
        .filter(|row| !row.is_empty())
        .map(|row| {
            (
                row.tag_id,
                banto_collect::Thresholds {
                    hh: row.threshold_hh,
                    h: row.threshold_h,
                    l: row.threshold_l,
                    ll: row.threshold_ll,
                },
            )
        })
        .collect())
}

/// しきい値のサービス（REST・Tauri 共通）。`Clone` で同じ pool を共有する。
#[derive(Clone)]
pub struct TagThresholdService {
    pool: SqlitePool,
}

impl TagThresholdService {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// 設定のあるタグ（行のあるタグ）の一覧（タグ ID 順）。監視画面・グループ
    /// 設定画面が、タグの一覧と突き合わせて使う（無いタグは設定なし）。
    pub async fn list(&self) -> Result<Vec<TagThresholds>, BantoError> {
        sqlx::query_as::<_, TagThresholds>(select_thresholds!(" ORDER BY tag_id"))
            .fetch_all(&self.pool)
            .await
            .map_err(storage)
    }

    /// 1 つのタグのしきい値。タグが無ければ `NotFound`、行が無ければ
    /// [`TagThresholds::unset`]（版 0）。
    pub async fn get(&self, tag_id: i64) -> Result<TagThresholds, BantoError> {
        let mut tx = self.pool.begin().await.map_err(storage)?;
        if tag_on(&mut tx, tag_id).await?.is_none() {
            return Err(tag_not_found(tag_id));
        }
        let row = row_on(&mut tx, tag_id).await?;
        tx.commit().await.map_err(storage)?;
        Ok(row.unwrap_or_else(|| TagThresholds::unset(tag_id)))
    }

    /// 全項目の置き換え（モジュール doc）。「タグの確認 → 版の確認 → 検証 →
    /// 書き込み」を 1 つの `BEGIN IMMEDIATE` の中で行う。
    pub async fn update(
        &self,
        tag_id: i64,
        payload: &TagThresholdsPayload,
    ) -> Result<UpdatedThresholds, BantoError> {
        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(storage)?;
        let Some((tag_name, data_type)) = tag_on(&mut tx, tag_id).await? else {
            return Err(tag_not_found(tag_id));
        };
        let current = row_on(&mut tx, tag_id).await?.map_or(0, |row| row.revision);
        if let Some(expected) = payload.expected_revision {
            if expected != current {
                return Err(revision_conflict_error(REVISION_CONFLICT_MESSAGE));
            }
        }
        validate_thresholds_for(&data_type, payload)?;
        sqlx::query(
            "INSERT INTO recorder_tag_settings \
             (tag_id, threshold_ll, threshold_l, threshold_h, threshold_hh) \
             VALUES (?, ?, ?, ?, ?) \
             ON CONFLICT(tag_id) DO UPDATE SET \
             threshold_ll = excluded.threshold_ll, threshold_l = excluded.threshold_l, \
             threshold_h = excluded.threshold_h, threshold_hh = excluded.threshold_hh, \
             revision = revision + 1, updated_at = datetime('now')",
        )
        .bind(tag_id)
        .bind(payload.threshold_ll)
        .bind(payload.threshold_l)
        .bind(payload.threshold_h)
        .bind(payload.threshold_hh)
        .execute(&mut *tx)
        .await
        .map_err(storage)?;
        let thresholds = row_on(&mut tx, tag_id)
            .await?
            .ok_or_else(|| BantoError::Storage("しきい値の行を書けませんでした".to_string()))?;
        tx.commit().await.map_err(storage)?;
        Ok(UpdatedThresholds {
            thresholds,
            tag_name,
        })
    }
}

#[cfg(test)]
mod tests;
