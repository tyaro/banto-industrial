//! TAG-P0-3（2026-08-11 方針改定）: 運転中編集の提案変更を保持する
//! pending queue の service 層。
//!
//! このモジュールは「反映エンジン」ではなく「保存・状態遷移・取得」の責務に
//! 限定する。実行構成への適用（preflight + apply_run）や controller 連携は
//! `crate::rest`/`crate::controller` 側が後続スライスで担う。
//!
//! # 接続の秘密値を平文で持たない（R2b-1e・R2b-6b、docs/banto-hub-connection-settings-design.md §5.3、docs/banto-hub-driver-ui-design.md §5）
//!
//! `plc_connections.create` / `.update` のペイロード（`{"input": ...}`）は秘密値を持ちうる。
//! R2b-6b から `input` は `settings` の形（`crate::rest::PlcConnectionPayload`）で、秘密の
//! キー（ドライバの `secret_keys()`）の値は入口が**積む前に**封印して `sealedSecrets` に移す:
//!
//! | 場面 | ペイロードの `input` |
//! | ---- | -------------------- |
//! | 積む（REST・MCP の入口の `PlcConnectionPayload::queue_form` → [`PendingChangesService::create_pending`]） | 秘密のキーの文字列を封印して `sealedSecrets`（キー → `crate::secrets::sealed_password_json` の形）に移し、`settings` から外す。空文字（消す）は `null`、`null`（消す）と省略（保つ）はそのまま（秘密ではない）。`create_pending` は R2b-6b の形でない接続の提案（平文の `password` を持ちうる古い形）を断る |
//! | 取り消し・失敗・再投入 | ペイロードごと動く（封印した値のまま。孤児にならない） |
//! | `applied` / `canceled` へ移す（[`PendingChangesService::mark_applied`]・[`PendingChangesService::cancel_pending`]） | **同じトランザクションで** `sealedSecrets` を外して `secretsSet`（キーの一覧）に替える（履歴の行に blob を残さない。`plain-v1` の blob は平文そのもの） |
//! | 応答の写し（`crate::rest` の `redact_pending_change_for_response`） | [`redact_queued_password`]: `sealedSecrets` を外して `secretsSet` |
//! | 起動時の後処理（`crate::upgrade::secrets_upkeep`） | 未適用（`pending`・`applying`・`failed`）の**古い形の行は捨てる**（Q10、[`is_current_connection_input`]）。`plain-v1` の `sealedSecrets` を（protector があれば）封印し直す、履歴から外す |
//!
//! 封印は [`crate::secrets::ConnectionSecrets::platform_default`]（Windows は DPAPI）。

use banto_core::{BantoError, FieldError};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sqlx::SqlitePool;

use crate::secrets::{ConnectionSecrets, SecretError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PendingChangeState {
    Pending,
    Applying,
    Applied,
    Canceled,
    Failed,
}

impl PendingChangeState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Applying => "applying",
            Self::Applied => "applied",
            Self::Canceled => "canceled",
            Self::Failed => "failed",
        }
    }

    fn parse(value: &str) -> Result<Self, BantoError> {
        match value {
            "pending" => Ok(Self::Pending),
            "applying" => Ok(Self::Applying),
            "applied" => Ok(Self::Applied),
            "canceled" => Ok(Self::Canceled),
            "failed" => Ok(Self::Failed),
            other => Err(BantoError::Other(format!(
                "不明な pending change 状態です: {other}"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingChange {
    pub id: i64,
    pub state: PendingChangeState,
    pub source: String,
    pub payload: serde_json::Value,
    pub base_configured_revision: i64,
    pub created_at: String,
    pub updated_at: String,
    pub requested_by_username: Option<String>,
    pub requested_by_role: Option<String>,
    pub failure_reason: Option<String>,
    /// TAG-P0-3 follow-up（2026-08-12）: 適用対象リソースの enqueue 時点の
    /// スナップショット（`serde_json::to_string` した DTO 文字列）。
    /// `execute_pending_apply` が適用直前に再取得・再シリアライズして
    /// 一致を確認し、他経路での編集を検出する per-resource ガード。
    /// `configured_revision`（global）は使わない — 複数件を連続適用すると
    /// 毎回 revision が進むため、global 比較では成立しない。
    pub base_fingerprint: Option<String>,
}

type PendingRow = (
    i64,
    String,
    String,
    String,
    i64,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

fn row_to_pending(row: PendingRow) -> Result<PendingChange, BantoError> {
    let (
        id,
        state,
        source,
        payload,
        base_configured_revision,
        created_at,
        updated_at,
        requested_by_username,
        requested_by_role,
        failure_reason,
        base_fingerprint,
    ) = row;
    Ok(PendingChange {
        id,
        state: PendingChangeState::parse(&state)?,
        source,
        payload: serde_json::from_str(&payload).map_err(|err| {
            BantoError::Other(format!("payload のデシリアライズに失敗しました: {err}"))
        })?,
        base_configured_revision,
        created_at,
        updated_at,
        requested_by_username,
        requested_by_role,
        failure_reason,
        base_fingerprint,
    })
}

#[derive(Clone)]
pub struct PendingChangesService {
    pool: SqlitePool,
}

impl PendingChangesService {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_pending(
        &self,
        source: &str,
        payload: &serde_json::Value,
        base_configured_revision: i64,
        base_fingerprint: Option<&str>,
        requested_by_username: Option<&str>,
        requested_by_role: Option<&str>,
    ) -> Result<PendingChange, BantoError> {
        // R2b-6b: 接続の秘密値は入口（`PlcConnectionPayload::queue_form`）が積む前に封印して
        // `sealedSecrets` に移す。古い平らな形（平文の `password` を持ちうる）は積まない。
        if !is_current_connection_input(source, payload) {
            return Err(BantoError::Validation {
                field_errors: vec![FieldError {
                    field: "input".to_string(),
                    message: "接続の提案の形が違います（R2b-6b の settings の形だけを積みます）"
                        .to_string(),
                }],
            });
        }
        let payload_json = serde_json::to_string(&payload).map_err(|err| {
            BantoError::Other(format!("payload のシリアライズに失敗しました: {err}"))
        })?;
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO pending_changes (
               state,
               source,
               payload,
               base_configured_revision,
               base_fingerprint,
               requested_by_username,
               requested_by_role
             ) VALUES (?, ?, ?, ?, ?, ?, ?) RETURNING id",
        )
        .bind(PendingChangeState::Pending.as_str())
        .bind(source)
        .bind(payload_json)
        .bind(base_configured_revision)
        .bind(base_fingerprint)
        .bind(requested_by_username)
        .bind(requested_by_role)
        .fetch_one(&self.pool)
        .await
        .map_err(banto_storage::storage_error)?;

        self.get(id).await
    }

    pub async fn get(&self, id: i64) -> Result<PendingChange, BantoError> {
        let row: Option<PendingRow> = sqlx::query_as(
            "SELECT
               id,
               state,
               source,
               payload,
               base_configured_revision,
               created_at,
               updated_at,
               requested_by_username,
               requested_by_role,
               failure_reason,
               base_fingerprint
             FROM pending_changes
             WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(banto_storage::storage_error)?;

        match row {
            Some(row) => row_to_pending(row),
            None => Err(BantoError::NotFound {
                resource: "pending_changes".to_string(),
                id: id.to_string(),
            }),
        }
    }

    pub async fn list(&self, limit: i64) -> Result<Vec<PendingChange>, BantoError> {
        let limit = limit.clamp(1, 1000);
        let rows: Vec<PendingRow> = sqlx::query_as(
            "SELECT
               id,
               state,
               source,
               payload,
               base_configured_revision,
               created_at,
               updated_at,
               requested_by_username,
               requested_by_role,
               failure_reason,
               base_fingerprint
             FROM pending_changes
             ORDER BY id DESC
             LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(banto_storage::storage_error)?;

        rows.into_iter().map(row_to_pending).collect()
    }

    /// pending だけでなく failed からもキャンセル可能（TAG-P0-3 follow-up、
    /// 2026-08-12）: 適用時のフィンガープリント不一致で failed になった
    /// 提案を破棄して再作成する運用上の復旧手段。applying/applied からの
    /// キャンセルは引き続き不可。
    ///
    /// R2b-1e: `canceled` へ移すのと同じトランザクションで、接続のペイロードから
    /// `sealedSecrets` を外して `secretsSet` に替える（このモジュールの doc）。
    pub async fn cancel_pending(&self, id: i64) -> Result<PendingChange, BantoError> {
        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(banto_storage::storage_error)?;
        let row: Option<PendingRow> = sqlx::query_as(
            "UPDATE pending_changes
             SET
               state = ?,
               updated_at = datetime('now')
             WHERE id = ? AND (state = ? OR state = ?)
             RETURNING
               id,
               state,
               source,
               payload,
               base_configured_revision,
               created_at,
               updated_at,
               requested_by_username,
               requested_by_role,
               failure_reason,
               base_fingerprint",
        )
        .bind(PendingChangeState::Canceled.as_str())
        .bind(id)
        .bind(PendingChangeState::Pending.as_str())
        .bind(PendingChangeState::Failed.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(banto_storage::storage_error)?;

        if let Some(row) = row {
            // R2b-1e（設計 §5.3）: 履歴へ移すのと同じトランザクションでパスワードを外す。
            let row = strip_history_row(&mut tx, row).await?;
            tx.commit().await.map_err(banto_storage::storage_error)?;
            return row_to_pending(row);
        }
        drop(tx);

        let current = self.get(id).await?;
        if current.state == PendingChangeState::Canceled {
            return Ok(current);
        }
        Err(BantoError::Other(
            "pending 状態以外の提案はキャンセルできません".to_string(),
        ))
    }

    /// failed→pending 差し戻し。fingerprint/payload/base_configured_revision
    /// は変更せず据え置く。再 apply 時に既存の検証（フィンガープリント
    /// ガード等）が再実行されるため、一過性の失敗（収集稼働中の 409 など）
    /// は再 apply で成功しうるが、真のコンフリクト（enqueue 後に対象行が
    /// 別経路で変わっている場合）は再度安全に fail する。TAG-P0-3
    /// follow-up、2026-08-14。
    pub async fn requeue_pending(&self, id: i64) -> Result<PendingChange, BantoError> {
        let row: Option<PendingRow> = sqlx::query_as(
            "UPDATE pending_changes
             SET
               state = ?,
               failure_reason = NULL,
               updated_at = datetime('now')
             WHERE id = ? AND state = ?
             RETURNING
               id,
               state,
               source,
               payload,
               base_configured_revision,
               created_at,
               updated_at,
               requested_by_username,
               requested_by_role,
               failure_reason,
               base_fingerprint",
        )
        .bind(PendingChangeState::Pending.as_str())
        .bind(id)
        .bind(PendingChangeState::Failed.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(banto_storage::storage_error)?;

        if let Some(row) = row {
            return row_to_pending(row);
        }

        let current = self.get(id).await?;
        if current.state == PendingChangeState::Pending {
            return Ok(current);
        }
        Err(BantoError::Other(
            "failed 状態の提案のみ再試行できます".to_string(),
        ))
    }

    pub async fn start_applying(&self, id: i64) -> Result<PendingChange, BantoError> {
        let row: Option<PendingRow> = sqlx::query_as(
            "UPDATE pending_changes
             SET
               state = ?,
               failure_reason = NULL,
               updated_at = datetime('now')
             WHERE id = ? AND state = ?
             RETURNING
               id,
               state,
               source,
               payload,
               base_configured_revision,
               created_at,
               updated_at,
               requested_by_username,
               requested_by_role,
               failure_reason,
               base_fingerprint",
        )
        .bind(PendingChangeState::Applying.as_str())
        .bind(id)
        .bind(PendingChangeState::Pending.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(banto_storage::storage_error)?;

        if let Some(row) = row {
            return row_to_pending(row);
        }

        let current = self.get(id).await?;
        Err(BantoError::Other(format!(
            "pending 状態以外の提案は適用開始できません(state={})",
            current.state.as_str()
        )))
    }

    /// R2b-1e: `applied` へ移すのと同じトランザクションで、接続のペイロードから
    /// `sealedSecrets` を外して `secretsSet` に替える（このモジュールの doc）。
    pub async fn mark_applied(&self, id: i64) -> Result<PendingChange, BantoError> {
        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(banto_storage::storage_error)?;
        let row: Option<PendingRow> = sqlx::query_as(
            "UPDATE pending_changes
             SET
               state = ?,
               failure_reason = NULL,
               updated_at = datetime('now')
             WHERE id = ? AND state = ?
             RETURNING
               id,
               state,
               source,
               payload,
               base_configured_revision,
               created_at,
               updated_at,
               requested_by_username,
               requested_by_role,
               failure_reason,
               base_fingerprint",
        )
        .bind(PendingChangeState::Applied.as_str())
        .bind(id)
        .bind(PendingChangeState::Applying.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(banto_storage::storage_error)?;

        if let Some(row) = row {
            // R2b-1e（設計 §5.3）: 履歴へ移すのと同じトランザクションでパスワードを外す。
            let row = strip_history_row(&mut tx, row).await?;
            tx.commit().await.map_err(banto_storage::storage_error)?;
            return row_to_pending(row);
        }
        drop(tx);

        let current = self.get(id).await?;
        Err(BantoError::Other(format!(
            "applying 状態以外の提案は適用完了にできません(state={})",
            current.state.as_str()
        )))
    }

    pub async fn mark_failed(&self, id: i64, reason: &str) -> Result<PendingChange, BantoError> {
        let row: Option<PendingRow> = sqlx::query_as(
            "UPDATE pending_changes
             SET
               state = ?,
               failure_reason = ?,
               updated_at = datetime('now')
             WHERE id = ? AND state = ?
             RETURNING
               id,
               state,
               source,
               payload,
               base_configured_revision,
               created_at,
               updated_at,
               requested_by_username,
               requested_by_role,
               failure_reason,
               base_fingerprint",
        )
        .bind(PendingChangeState::Failed.as_str())
        .bind(reason)
        .bind(id)
        .bind(PendingChangeState::Applying.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(banto_storage::storage_error)?;

        if let Some(row) = row {
            return row_to_pending(row);
        }

        let current = self.get(id).await?;
        Err(BantoError::Other(format!(
            "applying 状態以外の提案は失敗扱いにできません(state={})",
            current.state.as_str()
        )))
    }
}

/// 履歴（`applied` / `canceled`）へ移した直後の行のペイロードからパスワードを外して書き戻す
/// （呼び出し側のトランザクションの中で）。書き換えたら返す行のペイロードも替える。
async fn strip_history_row(
    tx: &mut sqlx::SqliteConnection,
    mut row: PendingRow,
) -> Result<PendingRow, BantoError> {
    let (id, _, ref source, ref payload_text, ..) = row;
    let Ok(mut payload) = serde_json::from_str::<Value>(payload_text) else {
        // 読めないペイロードは触らない（`row_to_pending` が同じ理由で失敗を返す）。
        return Ok(row);
    };
    let Some(plaintext) = strip_queued_password_detail(source, &mut payload) else {
        return Ok(row);
    };
    let text = serde_json::to_string(&payload)
        .map_err(|err| BantoError::Other(format!("payload のシリアライズに失敗しました: {err}")))?;
    sqlx::query("UPDATE pending_changes SET payload = ? WHERE id = ?")
        .bind(&text)
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(banto_storage::storage_error)?;
    if plaintext {
        // 平文を外したら、同じトランザクションで清掃待ちの印を立てる（#778 のレビュー P2。
        // 起動時の封印し直しに失敗して未適用キューに残った行を、VACUUM の後に履歴へ移す
        // 場合。印が無いと、空きページと WAL の平文が次の起動でも掃除されない）。
        set_vacuum_pending_in(&mut *tx).await?;
    }
    row.3 = text;
    Ok(row)
}

/// 清掃待ちの印 `secrets.vacuum_pending` を、呼び出し側のトランザクションの中で立てる
/// （設計 §6.5 の 2。消すのは `crate::upgrade::vacuum_if_pending`）。
pub(crate) async fn set_vacuum_pending_in(
    conn: &mut sqlx::SqliteConnection,
) -> Result<(), BantoError> {
    sqlx::query(
        "INSERT INTO settings (key, value) VALUES (?, 'true') \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind(crate::upgrade::VACUUM_PENDING_KEY)
    .execute(conn)
    .await
    .map_err(banto_storage::storage_error)?;
    Ok(())
}

// ---- 接続の秘密値のペイロードの扱い（R2b-1e・R2b-6b、このモジュールの doc） ----------------

/// 秘密値を持ちうる source（`plc_connections.create` / `.update`）。
pub const CONNECTION_PASSWORD_SOURCES: [&str; 2] =
    ["plc_connections.create", "plc_connections.update"];

/// ペイロードの `input` の封印した秘密値のキー（キー → `crate::secrets::sealed_password_json`
/// の形。`crate::rest::PlcConnectionPayload::queue_form` が積む前に作る）。
pub const SEALED_SECRETS_FIELD: &str = "sealedSecrets";
/// 履歴の行・応答の写しで `sealedSecrets` の代わりに置く、封印した値があったキーの一覧。
pub const SECRETS_SET_FIELD: &str = "secretsSet";
/// R2b-6b より前の形の欄（起動時に捨てる古い行にだけある。外す対象として残す）。
const LEGACY_PASSWORD_FIELD: &str = "password";
const LEGACY_SEALED_PASSWORD_FIELD: &str = "sealedPassword";

/// `source` が秘密値を持ちうる接続の作成・更新か。
pub fn is_connection_password_source(source: &str) -> bool {
    CONNECTION_PASSWORD_SOURCES.contains(&source)
}

/// R2b-6b の `input` の欄（`crate::rest` の `QueuedPlcConnectionPayload` と同じ）。
const CURRENT_INPUT_FIELDS: [&str; 6] = [
    "name",
    "protocol",
    "enabled",
    "simulation",
    "settings",
    SEALED_SECRETS_FIELD,
];

/// 接続の作成・更新のキューの `input` が R2b-6b の形か（Q10: 読めない古い形は起動時に捨てる、
/// `crate::upgrade`）。`name`・`protocol` が文字列、`settings` があればオブジェクト、
/// それ以外の欄が無いこと。接続以外の source は `true`。
pub fn is_current_connection_input(source: &str, payload: &Value) -> bool {
    if !is_connection_password_source(source) {
        return true;
    }
    let Some(input) = payload.get("input").and_then(Value::as_object) else {
        return false;
    };
    input
        .keys()
        .all(|key| CURRENT_INPUT_FIELDS.contains(&key.as_str()))
        && input.get("name").is_some_and(Value::is_string)
        && input.get("protocol").is_some_and(Value::is_string)
        && input.get("settings").is_none_or(Value::is_object)
}

fn connection_input_mut<'a>(
    source: &str,
    payload: &'a mut Value,
) -> Option<&'a mut Map<String, Value>> {
    if !is_connection_password_source(source) {
        return None;
    }
    payload.get_mut("input").and_then(Value::as_object_mut)
}

/// 起動時の後処理（設計 §6.5、2026-10-21 レビュー P2-3）: `input.sealedSecrets` の
/// `plain-v1` の値を、`secrets` に protector があれば**同じ参照のまま**封印し直す。書き換えたら
/// `true`。protector が無い・`plain-v1` が無いなら `false`。形が違う・開けないなら `Err`
/// （ペイロードは変えない）。
pub fn reseal_queued_password(
    secrets: &ConnectionSecrets,
    source: &str,
    payload: &mut Value,
) -> Result<bool, SecretError> {
    let Some(input) = connection_input_mut(source, payload) else {
        return Ok(false);
    };
    let Some(Value::Object(sealed)) = input.get(SEALED_SECRETS_FIELD) else {
        return Ok(false);
    };
    let mut resealed = sealed.clone();
    let mut changed = false;
    for (key, value) in sealed {
        let parsed = crate::secrets::parse_sealed_password(value)?;
        if let Some(again) = secrets.reseal_plain(key, &parsed)? {
            resealed.insert(key.clone(), crate::secrets::sealed_password_json(&again));
            changed = true;
        }
    }
    if changed {
        input.insert(SEALED_SECRETS_FIELD.to_string(), Value::Object(resealed));
    }
    Ok(changed)
}

/// `input` に `sealedSecrets`（と、古い形の `password`・`sealedPassword`）があれば外して、
/// 封印した値があったキーの一覧 `secretsSet` に替える。何も外さなければ `None`。外したら
/// `Some(平文を外したか)`: `dpapi-machine-v1` と読めない封印した値（`plain-v1`・形が違うもの。
/// blob が平文の base64 でありうるので安全側）、または古い形の非空の `password`。
fn strip_input(input: &mut Map<String, Value>) -> Option<bool> {
    let sealed = input.remove(SEALED_SECRETS_FIELD);
    let legacy_password = input.remove(LEGACY_PASSWORD_FIELD);
    let legacy_sealed = input.remove(LEGACY_SEALED_PASSWORD_FIELD);
    if sealed.is_none() && legacy_password.is_none() && legacy_sealed.is_none() {
        return None;
    }
    let not_dpapi = |value: &Value| {
        value.get("scheme").and_then(Value::as_str)
            != Some(crate::secrets::SecretScheme::DpapiMachineV1.as_str())
    };
    let mut keys: Vec<String> = Vec::new();
    let mut plaintext = false;
    match &sealed {
        Some(Value::Object(sealed)) => {
            for (key, value) in sealed {
                keys.push(key.clone());
                plaintext |= not_dpapi(value);
            }
        }
        // 形の違う値も外す（中身は外へ出さない）。
        Some(_) => plaintext = true,
        None => {}
    }
    plaintext |= matches!(&legacy_password, Some(Value::String(plain)) if !plain.is_empty());
    plaintext |= legacy_sealed.as_ref().is_some_and(not_dpapi);
    keys.sort();
    let mut set: Vec<Value> = match input.remove(SECRETS_SET_FIELD) {
        Some(Value::Array(existing)) => existing,
        _ => Vec::new(),
    };
    for key in keys {
        if !set.iter().any(|v| v.as_str() == Some(key.as_str())) {
            set.push(Value::String(key));
        }
    }
    input.insert(SECRETS_SET_FIELD.to_string(), Value::Array(set));
    Some(plaintext)
}

/// 履歴へ移すとき・起動時の後処理（設計 §5.3・§6.5）: `input` から封印した値を外して
/// `secretsSet` に替える。書き換えたら `true`。
pub fn strip_queued_password(source: &str, payload: &mut Value) -> bool {
    strip_queued_password_detail(source, payload).is_some()
}

/// [`strip_queued_password`] に、外したのが平文だったか（[`strip_input`] の戻り値）を足した形。
/// 平文を外したら呼び出し側は清掃待ちの印を立てる（設計 §6.5 の 2）。
pub fn strip_queued_password_detail(source: &str, payload: &mut Value) -> Option<bool> {
    connection_input_mut(source, payload).and_then(strip_input)
}

/// 応答の写し（S1a レビュー対応、R2b-1e・R2b-6b）: 封印した値を外して `secretsSet` を出す。
/// 何も無ければそのまま（秘密のキーの `null` は「消す」で、秘密ではない）。DB の行は変えない
/// （写しにだけ掛ける）。それ以外の source はそのまま返す。
pub fn redact_queued_password(source: &str, payload: &Value) -> Value {
    let mut payload = payload.clone();
    if let Some(input) = connection_input_mut(source, &mut payload) {
        strip_input(input);
    }
    payload
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrate_memory;

    async fn service() -> PendingChangesService {
        let pool = migrate_memory().await.expect("migrate_memory");
        PendingChangesService::new(pool)
    }

    #[tokio::test]
    async fn create_and_get_round_trip() {
        let svc = service().await;
        let created = svc
            .create_pending(
                "tags.create",
                &serde_json::json!({ "name": "temp01" }),
                42,
                None,
                Some("admin"),
                Some("admin"),
            )
            .await
            .unwrap();
        assert_eq!(created.state, PendingChangeState::Pending);
        assert_eq!(created.source, "tags.create");
        assert_eq!(created.base_configured_revision, 42);

        let fetched = svc.get(created.id).await.unwrap();
        assert_eq!(fetched.id, created.id);
        assert_eq!(fetched.payload["name"], "temp01");
    }

    #[tokio::test]
    async fn list_returns_latest_first() {
        let svc = service().await;
        let first = svc
            .create_pending(
                "tags.create",
                &serde_json::json!({ "name": "a" }),
                1,
                None,
                None,
                None,
            )
            .await
            .unwrap();
        let second = svc
            .create_pending(
                "tags.create",
                &serde_json::json!({ "name": "b" }),
                2,
                None,
                None,
                None,
            )
            .await
            .unwrap();

        let listed = svc.list(100).await.unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].id, second.id);
        assert_eq!(listed[1].id, first.id);
    }

    #[tokio::test]
    async fn cancel_is_idempotent_for_already_canceled_rows() {
        let svc = service().await;
        let created = svc
            .create_pending(
                "tags.update",
                &serde_json::json!({ "id": 1 }),
                3,
                None,
                None,
                None,
            )
            .await
            .unwrap();

        let canceled = svc.cancel_pending(created.id).await.unwrap();
        assert_eq!(canceled.state, PendingChangeState::Canceled);

        let canceled_again = svc.cancel_pending(created.id).await.unwrap();
        assert_eq!(canceled_again.state, PendingChangeState::Canceled);
    }

    #[tokio::test]
    async fn applying_state_transitions_are_strict() {
        let svc = service().await;
        let created = svc
            .create_pending(
                "tags.create",
                &serde_json::json!({ "name": "strict" }),
                5,
                None,
                None,
                None,
            )
            .await
            .unwrap();

        let applying = svc.start_applying(created.id).await.unwrap();
        assert_eq!(applying.state, PendingChangeState::Applying);

        let failed = svc
            .mark_failed(created.id, "simulated failure")
            .await
            .unwrap();
        assert_eq!(failed.state, PendingChangeState::Failed);
        assert_eq!(failed.failure_reason.as_deref(), Some("simulated failure"));

        let err = svc.start_applying(created.id).await.unwrap_err();
        assert!(err
            .to_string()
            .contains("pending 状態以外の提案は適用開始できません"));
    }

    #[tokio::test]
    async fn requeue_moves_failed_back_to_pending_and_clears_reason() {
        let svc = service().await;
        let created = svc
            .create_pending(
                "tags.create",
                &serde_json::json!({ "name": "requeue-me" }),
                11,
                Some("fp-abc123"),
                Some("admin"),
                Some("admin"),
            )
            .await
            .unwrap();
        let original_fingerprint = created.base_fingerprint.clone();
        let original_payload = created.payload.clone();

        svc.start_applying(created.id).await.unwrap();
        svc.mark_failed(created.id, "boom").await.unwrap();

        let requeued = svc.requeue_pending(created.id).await.unwrap();
        assert_eq!(requeued.state, PendingChangeState::Pending);
        assert_eq!(requeued.failure_reason, None);
        assert_eq!(requeued.base_fingerprint, original_fingerprint);
        assert_eq!(requeued.payload, original_payload);
    }

    #[tokio::test]
    async fn requeue_rejects_non_failed() {
        let svc = service().await;

        // applied からは再試行できない（pending からの再試行は
        // requeue_is_idempotent_for_already_pending が示す通り成功する
        // no-op のため、ここでは対象外）。
        let applied_only = svc
            .create_pending(
                "tags.create",
                &serde_json::json!({ "name": "already-applied" }),
                13,
                None,
                None,
                None,
            )
            .await
            .unwrap();
        svc.start_applying(applied_only.id).await.unwrap();
        svc.mark_applied(applied_only.id).await.unwrap();
        let err = svc.requeue_pending(applied_only.id).await.unwrap_err();
        assert!(err
            .to_string()
            .contains("failed 状態の提案のみ再試行できます"));
    }

    #[tokio::test]
    async fn requeue_is_idempotent_for_already_pending() {
        let svc = service().await;
        let created = svc
            .create_pending(
                "tags.create",
                &serde_json::json!({ "name": "no-op" }),
                14,
                None,
                None,
                None,
            )
            .await
            .unwrap();

        let requeued = svc.requeue_pending(created.id).await.unwrap();
        assert_eq!(requeued.state, PendingChangeState::Pending);
    }

    #[tokio::test]
    async fn cancel_allows_failed_but_not_applying_or_applied() {
        let svc = service().await;

        // failed -> canceled は許可（TAG-P0-3 follow-up: フィンガープリント
        // 不一致で failed になった提案を破棄して再作成できるようにする）。
        let created = svc
            .create_pending(
                "tags.create",
                &serde_json::json!({ "name": "recoverable" }),
                7,
                None,
                None,
                None,
            )
            .await
            .unwrap();
        svc.start_applying(created.id).await.unwrap();
        svc.mark_failed(created.id, "boom").await.unwrap();

        let canceled = svc.cancel_pending(created.id).await.unwrap();
        assert_eq!(canceled.state, PendingChangeState::Canceled);

        // applying からのキャンセルは不可のまま。
        let applying_only = svc
            .create_pending(
                "tags.create",
                &serde_json::json!({ "name": "mid-apply" }),
                8,
                None,
                None,
                None,
            )
            .await
            .unwrap();
        svc.start_applying(applying_only.id).await.unwrap();
        let err = svc.cancel_pending(applying_only.id).await.unwrap_err();
        assert!(err
            .to_string()
            .contains("pending 状態以外の提案はキャンセルできません"));

        // applied からのキャンセルも不可のまま。
        let applied_only = svc
            .create_pending(
                "tags.create",
                &serde_json::json!({ "name": "done" }),
                9,
                None,
                None,
                None,
            )
            .await
            .unwrap();
        svc.start_applying(applied_only.id).await.unwrap();
        svc.mark_applied(applied_only.id).await.unwrap();
        let err = svc.cancel_pending(applied_only.id).await.unwrap_err();
        assert!(err
            .to_string()
            .contains("pending 状態以外の提案はキャンセルできません"));
    }

    // ---- R2b-1e（docs/banto-hub-connection-settings-design.md §5.3）--------------------

    const QUEUED_SECRET: &str = "TOPSECRET-queue-pw-5d1e";

    /// R2b-6b の形の作成の提案（入口の `queue_form` が封印した後の形）。
    fn create_payload(sealed: Option<serde_json::Value>) -> serde_json::Value {
        let mut input = serde_json::json!({
            "name": "pg", "protocol": "postgres",
            "settings": { "host": "db", "port": 5432, "database": "erp", "username": "reader" },
        });
        if let Some(sealed) = sealed {
            input["sealedSecrets"] = serde_json::json!({ "password": sealed });
        }
        serde_json::json!({ "input": input })
    }

    fn sealed_json(secrets: &crate::secrets::ConnectionSecrets) -> serde_json::Value {
        crate::secrets::sealed_password_json(
            &secrets
                .seal(banto_tags::PASSWORD_KEY, QUEUED_SECRET)
                .unwrap(),
        )
    }

    async fn raw_payload(svc: &PendingChangesService, id: i64) -> String {
        sqlx::query_scalar("SELECT payload FROM pending_changes WHERE id = ?")
            .bind(id)
            .fetch_one(&svc.pool)
            .await
            .unwrap()
    }

    /// R2b-6b: `create_pending` は封印済みの R2b-6b の形をそのまま積み、古い平らな形
    /// （平文の `password` を持ちうる）の接続の提案は断る。接続以外の source はそのまま。
    #[tokio::test]
    async fn create_pending_keeps_the_sealed_form_and_refuses_the_old_flat_shape() {
        let svc = service().await;
        let payload = create_payload(Some(sealed_json(
            &crate::secrets::ConnectionSecrets::platform_default(),
        )));
        let created = svc
            .create_pending("plc_connections.create", &payload, 1, None, None, None)
            .await
            .unwrap();
        assert_eq!(created.payload, payload);

        let old = serde_json::json!({ "input": {
            "name": "pg", "protocol": "postgres", "host": "db", "port": 5432,
            "password": QUEUED_SECRET,
        }});
        for source in ["plc_connections.create", "plc_connections.update"] {
            let err = svc
                .create_pending(source, &old, 1, None, None, None)
                .await
                .unwrap_err();
            assert!(!format!("{err:?}").contains(QUEUED_SECRET));
        }
        let other = serde_json::json!({ "input": { "password": QUEUED_SECRET } });
        let created = svc
            .create_pending("tags.create", &other, 1, None, None, None)
            .await
            .unwrap();
        assert_eq!(created.payload, other);
    }

    /// 履歴（`applied`・`canceled`）へ移すと、同じトランザクションで `sealedSecrets` が
    /// 外れて `secretsSet` に替わる（DB の行に blob が残らない）。`failed` は未適用なので
    /// 封印した値のまま。
    #[tokio::test]
    async fn moving_to_applied_or_canceled_strips_the_sealed_secrets() {
        let svc = service().await;
        let plain = crate::secrets::ConnectionSecrets::new(None);
        let queue = |svc: &PendingChangesService| {
            let svc = svc.clone();
            let payload = create_payload(Some(sealed_json(&plain)));
            async move {
                svc.create_pending("plc_connections.create", &payload, 1, None, None, None)
                    .await
                    .unwrap()
            }
        };

        let applied = queue(&svc).await;
        svc.start_applying(applied.id).await.unwrap();
        let done = svc.mark_applied(applied.id).await.unwrap();
        let canceled = queue(&svc).await;
        let gone = svc.cancel_pending(canceled.id).await.unwrap();
        for row in [&done, &gone] {
            let input = &row.payload["input"];
            assert!(input.get("sealedSecrets").is_none(), "{input}");
            assert_eq!(input["secretsSet"], serde_json::json!(["password"]));
            let raw = raw_payload(&svc, row.id).await;
            assert!(!raw.contains("sealedSecrets"), "{raw}");
            assert!(!raw.contains(QUEUED_SECRET), "{raw}");
            assert_eq!(svc.get(row.id).await.unwrap().payload, row.payload);
        }

        let failed = queue(&svc).await;
        svc.start_applying(failed.id).await.unwrap();
        let failed = svc.mark_failed(failed.id, "boom").await.unwrap();
        assert!(failed.payload["input"].get("sealedSecrets").is_some());
        // failed -> canceled strips it too.
        let canceled = svc.cancel_pending(failed.id).await.unwrap();
        assert!(canceled.payload["input"].get("sealedSecrets").is_none());
        assert_eq!(
            canceled.payload["input"]["secretsSet"],
            serde_json::json!(["password"])
        );
    }

    /// 応答の写し: `sealedSecrets`（と古い形の `password`・`sealedPassword`）を外して
    /// `secretsSet` を出す。何も無ければそのまま（`null` は「消す」で秘密ではない）。もう一度
    /// 掛けても同じ。
    #[test]
    fn redact_queued_password_hides_sealed_values() {
        let sealed = sealed_json(&crate::secrets::ConnectionSecrets::new(None));
        let cases = [
            (
                serde_json::json!({ "sealedSecrets": { "password": sealed.clone() } }),
                Some(serde_json::json!(["password"])),
            ),
            (
                serde_json::json!({ "settings": { "password": null } }),
                None,
            ),
            (
                serde_json::json!({ "password": QUEUED_SECRET }),
                Some(serde_json::json!([])),
            ),
            (serde_json::json!({}), None),
            (
                serde_json::json!({ "secretsSet": ["password"] }),
                Some(serde_json::json!(["password"])),
            ),
        ];
        for source in ["plc_connections.create", "plc_connections.update"] {
            for (input, want) in &cases {
                let payload = serde_json::json!({ "input": input });
                let redacted = redact_queued_password(source, &payload);
                let text = redacted.to_string();
                assert!(!text.contains(QUEUED_SECRET), "{text}");
                assert!(!text.contains("sealedSecrets"), "{text}");
                assert_eq!(
                    redacted["input"].get("secretsSet").cloned(),
                    want.clone(),
                    "{source} {payload}"
                );
                assert_eq!(
                    redact_queued_password(source, &redacted),
                    redacted,
                    "idempotent"
                );
            }
        }
        // Other sources are returned as they are.
        let other = serde_json::json!({ "input": { "password": "x" } });
        assert_eq!(redact_queued_password("tags.create", &other), other);
    }

    /// Q10: R2b-6b の形かどうか（起動時に捨てる古い形の見分け）。
    #[test]
    fn the_current_connection_input_shape_is_recognised() {
        let source = "plc_connections.update";
        assert!(is_current_connection_input(source, &create_payload(None)));
        assert!(is_current_connection_input(
            source,
            &serde_json::json!({ "id": 1, "input": { "name": "a", "protocol": "slmp" } })
        ));
        for old in [
            // The MCP flat adapter's "keep the stored value" mark (gone in R2b-7a): an
            // old queue row is discarded at startup.
            serde_json::json!({ "id": 1, "input": { "name": "a", "protocol": "slmp",
                                 "keepStored": { "keys": ["cpu_type"] } } }),
            serde_json::json!({ "input": { "name": "a", "protocol": "slmp", "host": "h", "port": 1 } }),
            serde_json::json!({ "input": { "name": "a", "protocol": "slmp", "settings": [] } }),
            serde_json::json!({ "input": { "protocol": "slmp" } }),
            serde_json::json!({ "id": 1 }),
        ] {
            assert!(!is_current_connection_input(source, &old), "{old}");
        }
        assert!(is_current_connection_input(
            "tags.create",
            &serde_json::json!({})
        ));
    }
}
