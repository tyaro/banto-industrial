//! TAG-P0-3（2026-08-11 方針改定）: 運転中編集の提案変更を保持する
//! pending queue の service 層。
//!
//! このモジュールは「反映エンジン」ではなく「保存・状態遷移・取得」の責務に
//! 限定する。実行構成への適用（preflight + apply_run）や controller 連携は
//! `crate::rest`/`crate::controller` 側が後続スライスで担う。
//!
//! # 接続のパスワードを平文で持たない（R2b-1e、docs/banto-hub-connection-settings-design.md §5.3）
//!
//! `plc_connections.create` / `.update` のペイロード（`{"input": PlcConnectionPayload}`）は
//! パスワードを持ちうる。R2b-1d までは平文の `input.password` のまま保存していた。R2b-1e から:
//!
//! | 場面 | ペイロードの `input` |
//! | ---- | -------------------- |
//! | 積む（[`PendingChangesService::create_pending`]、REST・MCP の全入口がここを通る） | 非空の `password` を封印して `sealedPassword`（`crate::secrets::sealed_password_json`）に替える。空文字（消す）と省略・`null`（保つ）はそのまま（秘密ではない） |
//! | 取り消し・失敗・再投入 | ペイロードごと動く（封印した値のまま。孤児にならない） |
//! | `applied` / `canceled` へ移す（[`PendingChangesService::mark_applied`]・[`PendingChangesService::cancel_pending`]） | **同じトランザクションで** `password` と `sealedPassword` を外して `passwordSet` に替える（履歴の行に平文も blob も残さない。`plain-v1` の blob は平文そのもの） |
//! | 応答の写し（`crate::rest` の `redact_pending_change_for_response`） | [`redact_queued_password`]: `password` / `sealedPassword` を外して `passwordSet` |
//! | 起動時の後処理（`crate::upgrade::secrets_upkeep`） | 未適用（`pending`・`applying`・`failed`）の平文の `password` を封印、`plain-v1` の `sealedPassword` を（protector があれば）封印し直す、履歴から両方を外す |
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
        // R2b-1e（設計 §5.3）: 接続のパスワードは積む時点で封印する（平文をキューに置かない）。
        let mut payload = payload.clone();
        seal_queued_password(&ConnectionSecrets::platform_default(), source, &mut payload)
            .map_err(|error| BantoError::Validation {
                field_errors: vec![FieldError {
                    field: "password".to_string(),
                    message: error.to_string(),
                }],
            })?;
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
    /// `password` と `sealedPassword` を外して `passwordSet` に替える（このモジュールの doc）。
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
    /// `password` と `sealedPassword` を外して `passwordSet` に替える（このモジュールの doc）。
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

// ---- 接続のパスワードのペイロードの扱い（R2b-1e、このモジュールの doc） ---------------------

/// パスワードを持ちうる source（`plc_connections.create` / `.update`）。
pub const CONNECTION_PASSWORD_SOURCES: [&str; 2] =
    ["plc_connections.create", "plc_connections.update"];

/// ペイロードの `input` の封印したパスワードのキー（`crate::secrets::sealed_password_json`）。
pub const SEALED_PASSWORD_FIELD: &str = "sealedPassword";
const PASSWORD_FIELD: &str = "password";
const PASSWORD_SET_FIELD: &str = "passwordSet";

/// `source` がパスワードを持ちうる接続の作成・更新か。
pub fn is_connection_password_source(source: &str) -> bool {
    CONNECTION_PASSWORD_SOURCES.contains(&source)
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

/// 積む時点・起動時の後処理（設計 §5.3・§6.5）: `input.password` が非空の文字列なら、
/// `secrets` で封印して `input.sealedPassword` に替え、`password` を外す。書き換えたら
/// `true`。空文字（消す）・`null`・省略（保つ）は秘密ではないのでそのまま。
///
/// 封印に失敗したら（大きすぎる・乱数源・DPAPI の失敗）ペイロードは変えずに `Err`（値は
/// 載せない）。
pub fn seal_queued_password(
    secrets: &ConnectionSecrets,
    source: &str,
    payload: &mut Value,
) -> Result<bool, SecretError> {
    let Some(input) = connection_input_mut(source, payload) else {
        return Ok(false);
    };
    let sealed = match input.get(PASSWORD_FIELD) {
        Some(Value::String(plain)) if !plain.is_empty() => {
            secrets.seal(banto_tags::PASSWORD_KEY, plain)?
        }
        _ => return Ok(false),
    };
    input.remove(PASSWORD_FIELD);
    input.insert(
        SEALED_PASSWORD_FIELD.to_string(),
        crate::secrets::sealed_password_json(&sealed),
    );
    Ok(true)
}

/// 起動時の後処理（設計 §6.5、2026-10-21 レビュー P2-3）: `input.sealedPassword` が
/// `plain-v1` で、`secrets` に protector があれば、**同じ参照のまま**封印し直す。書き換えたら
/// `true`。protector が無い・`plain-v1` でない・`sealedPassword` が無いなら `false`。
/// 形が違う・開けないなら `Err`（ペイロードは変えない）。
pub fn reseal_queued_password(
    secrets: &ConnectionSecrets,
    source: &str,
    payload: &mut Value,
) -> Result<bool, SecretError> {
    let Some(input) = connection_input_mut(source, payload) else {
        return Ok(false);
    };
    let Some(value) = input.get(SEALED_PASSWORD_FIELD) else {
        return Ok(false);
    };
    let sealed = crate::secrets::parse_sealed_password(value)?;
    let Some(resealed) = secrets.reseal_plain(banto_tags::PASSWORD_KEY, &sealed)? else {
        return Ok(false);
    };
    input.insert(
        SEALED_PASSWORD_FIELD.to_string(),
        crate::secrets::sealed_password_json(&resealed),
    );
    Ok(true)
}

/// `input` に `password` か `sealedPassword` があれば外して `passwordSet` に替える。
/// `passwordSet` は、封印した値があれば `true`、`password` が文字列なら非空かどうか、
/// `null` なら作成は `false`・更新は `null`（「保つ」）、それ以外の値は `false`。
///
/// 何も外さなければ `None`。外したら `Some(平文を外したか)`: 非空の文字列の `password`、
/// または `dpapi-machine-v1` と読めない `sealedPassword`（`plain-v1`・形が違うもの。blob が
/// 平文の base64 でありうるので安全側）。DPAPI の blob だけなら `false`。
fn strip_input(source: &str, input: &mut Map<String, Value>) -> Option<bool> {
    let password = input.remove(PASSWORD_FIELD);
    let sealed = input.remove(SEALED_PASSWORD_FIELD);
    if password.is_none() && sealed.is_none() {
        return None;
    }
    let plaintext_password = matches!(&password, Some(Value::String(plain)) if !plain.is_empty());
    let plaintext_sealed = sealed.as_ref().is_some_and(|value| {
        value.get("scheme").and_then(Value::as_str)
            != Some(crate::secrets::SecretScheme::DpapiMachineV1.as_str())
    });
    let password_set = if sealed.is_some() {
        Value::Bool(true)
    } else {
        match password {
            Some(Value::String(plain)) => Value::Bool(!plain.is_empty()),
            None | Some(Value::Null) => absent_password_set(source),
            // 文字列でない不正な値（本来は起こらない）も、値そのものは外へ出さない。
            Some(_) => Value::Bool(false),
        }
    };
    input.insert(PASSWORD_SET_FIELD.to_string(), password_set);
    Some(plaintext_password || plaintext_sealed)
}

/// パスワードの指定が無いときの `passwordSet`: 作成は `false`（無し）、更新は `null`
/// （「今のパスワードを保つ」。この提案は今の保存状態を持たないので true/false にしない）。
fn absent_password_set(source: &str) -> Value {
    if source == "plc_connections.create" {
        Value::Bool(false)
    } else {
        Value::Null
    }
}

/// 履歴へ移すとき・起動時の後処理（設計 §5.3・§6.5）: `input` から `password` と
/// `sealedPassword` の**両方**を外して `passwordSet` に替える。書き換えたら `true`。
pub fn strip_queued_password(source: &str, payload: &mut Value) -> bool {
    strip_queued_password_detail(source, payload).is_some()
}

/// [`strip_queued_password`] に、外したのが平文だったか（[`strip_input`] の戻り値）を足した形。
/// 平文を外したら呼び出し側は清掃待ちの印を立てる（設計 §6.5 の 2）。
pub fn strip_queued_password_detail(source: &str, payload: &mut Value) -> Option<bool> {
    connection_input_mut(source, payload).and_then(|input| strip_input(source, input))
}

/// 応答の写し（S1a レビュー対応、R2b-1e で封印した値にも広げた）: `password` と
/// `sealedPassword` を外して `passwordSet` を出す。どちらも無ければ、`passwordSet` が
/// 既にある（履歴へ移すときに替えた）ならそのまま、無ければ作成は `false`・更新は `null`。
/// DB の行は変えない（写しにだけ掛ける）。それ以外の source はそのまま返す。
pub fn redact_queued_password(source: &str, payload: &Value) -> Value {
    let mut payload = payload.clone();
    if let Some(input) = connection_input_mut(source, &mut payload) {
        if strip_input(source, input).is_none() && !input.contains_key(PASSWORD_SET_FIELD) {
            input.insert(PASSWORD_SET_FIELD.to_string(), absent_password_set(source));
        }
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

    fn create_payload(password: serde_json::Value) -> serde_json::Value {
        serde_json::json!({ "input": {
            "name": "pg", "protocol": "postgres", "host": "db", "port": 5432,
            "database": "erp", "username": "reader", "password": password
        }})
    }

    async fn raw_payload(svc: &PendingChangesService, id: i64) -> String {
        sqlx::query_scalar("SELECT payload FROM pending_changes WHERE id = ?")
            .bind(id)
            .fetch_one(&svc.pool)
            .await
            .unwrap()
    }

    /// 積む時点で封印する: 非空の `password` は `sealedPassword` に替わり、DB の行にも
    /// 平文が無い。空文字（消す）・`null`（保つ）・接続以外の source はそのまま。
    #[tokio::test]
    async fn create_pending_seals_a_connection_password_before_storing_it() {
        let svc = service().await;
        let created = svc
            .create_pending(
                "plc_connections.create",
                &create_payload(serde_json::json!(QUEUED_SECRET)),
                1,
                None,
                None,
                None,
            )
            .await
            .unwrap();
        let input = &created.payload["input"];
        assert!(input.get("password").is_none(), "{input}");
        let sealed = crate::secrets::parse_sealed_password(&input["sealedPassword"]).unwrap();
        assert_eq!(
            sealed.scheme,
            crate::secrets::ConnectionSecrets::platform_default().seal_scheme()
        );
        assert_eq!(
            crate::secrets::ConnectionSecrets::platform_default()
                .open(banto_tags::PASSWORD_KEY, &sealed)
                .unwrap()
                .as_str(),
            QUEUED_SECRET
        );
        if cfg!(windows) {
            assert!(!raw_payload(&svc, created.id).await.contains(QUEUED_SECRET));
        }
        // The plaintext itself never sits under `password` in the stored row.
        let raw: serde_json::Value =
            serde_json::from_str(&raw_payload(&svc, created.id).await).unwrap();
        assert!(raw["input"].get("password").is_none());

        for (source, payload) in [
            (
                "plc_connections.update",
                create_payload(serde_json::json!("")),
            ),
            (
                "plc_connections.update",
                create_payload(serde_json::Value::Null),
            ),
            (
                "tags.create",
                serde_json::json!({ "input": { "password": QUEUED_SECRET } }),
            ),
        ] {
            let created = svc
                .create_pending(source, &payload, 1, None, None, None)
                .await
                .unwrap();
            assert_eq!(created.payload, payload, "{source}");
        }
    }

    /// 履歴（`applied`・`canceled`）へ移すと、同じトランザクションで `sealedPassword` と
    /// `password` が外れて `passwordSet` に替わる（DB の行に平文も blob も残らない）。
    /// `failed` は未適用なので封印した値のまま。
    #[tokio::test]
    async fn moving_to_applied_or_canceled_strips_the_sealed_password() {
        let svc = service().await;
        let queue = |svc: &PendingChangesService| {
            let svc = svc.clone();
            async move {
                svc.create_pending(
                    "plc_connections.create",
                    &create_payload(serde_json::json!(QUEUED_SECRET)),
                    1,
                    None,
                    None,
                    None,
                )
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
            assert!(input.get("sealedPassword").is_none(), "{input}");
            assert!(input.get("password").is_none(), "{input}");
            assert_eq!(input["passwordSet"], serde_json::json!(true));
            let raw = raw_payload(&svc, row.id).await;
            assert!(!raw.contains("sealedPassword"), "{raw}");
            assert!(!raw.contains(QUEUED_SECRET), "{raw}");
            assert_eq!(svc.get(row.id).await.unwrap().payload, row.payload);
        }

        let failed = queue(&svc).await;
        svc.start_applying(failed.id).await.unwrap();
        let failed = svc.mark_failed(failed.id, "boom").await.unwrap();
        assert!(failed.payload["input"].get("sealedPassword").is_some());
        // failed -> canceled strips it too.
        let canceled = svc.cancel_pending(failed.id).await.unwrap();
        assert!(canceled.payload["input"].get("sealedPassword").is_none());
        assert_eq!(
            canceled.payload["input"]["passwordSet"],
            serde_json::json!(true)
        );
    }

    /// 応答の写し: `password` も `sealedPassword` も外し、`passwordSet` を出す（作成・更新の
    /// tri-state）。履歴へ移すときに入れた `passwordSet` はそのまま（もう一度掛けても同じ）。
    #[test]
    fn redact_queued_password_hides_plaintext_and_sealed_values() {
        let sealed = crate::secrets::sealed_password_json(
            &crate::secrets::ConnectionSecrets::new(None)
                .seal(banto_tags::PASSWORD_KEY, QUEUED_SECRET)
                .unwrap(),
        );
        let cases = [
            (
                "plc_connections.create",
                serde_json::json!({ "sealedPassword": sealed.clone() }),
                serde_json::json!(true),
            ),
            (
                "plc_connections.update",
                serde_json::json!({ "sealedPassword": sealed.clone() }),
                serde_json::json!(true),
            ),
            (
                "plc_connections.create",
                serde_json::json!({ "password": QUEUED_SECRET }),
                serde_json::json!(true),
            ),
            (
                "plc_connections.update",
                serde_json::json!({ "password": "" }),
                serde_json::json!(false),
            ),
            (
                "plc_connections.update",
                serde_json::json!({ "password": null }),
                serde_json::Value::Null,
            ),
            (
                "plc_connections.update",
                serde_json::json!({}),
                serde_json::Value::Null,
            ),
            (
                "plc_connections.create",
                serde_json::json!({}),
                serde_json::json!(false),
            ),
            (
                "plc_connections.update",
                serde_json::json!({ "passwordSet": true }),
                serde_json::json!(true),
            ),
        ];
        for (source, input, want) in cases {
            let payload = serde_json::json!({ "input": input });
            let redacted = redact_queued_password(source, &payload);
            let text = redacted.to_string();
            assert!(!text.contains(QUEUED_SECRET), "{text}");
            assert!(!text.contains("sealedPassword"), "{text}");
            assert_eq!(redacted["input"]["passwordSet"], want, "{source} {payload}");
            assert_eq!(
                redact_queued_password(source, &redacted),
                redacted,
                "idempotent"
            );
        }
        // Other sources are returned as they are.
        let other = serde_json::json!({ "input": { "password": "x" } });
        assert_eq!(redact_queued_password("tags.create", &other), other);
    }
}
