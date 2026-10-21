//! Collection group (recorder-requirements.md §3.1: "収集周期はタグ毎ではなく
//! 収集グループ毎"): the unit of periodic PLC bulk read. Every
//! [`crate::tag::Tag`] belongs to exactly one group, and a group's
//! `period_ms` (one of [`ALLOWED_PERIOD_MS`]) is how often the collection
//! engine (I3) reads every tag in it in one PLC round-trip.
//!
//! ## DB Source のグループ（S2、docs/banto-hub-external-db-design.md §4.1・
//! §4.2、2026-09-06 オーナー決定「案A: 既存3階層の流用」）
//!
//! `protocol = `[`crate::plc_connection::POSTGRES_PROTOCOL`] の接続配下の
//! グループは「1 周期 = 1 SELECT = N タグ」の単位になる - [`CollectionGroup::
//! query_sql`] がその SELECT 文で、`period_ms` は PLC グループと全く同じ
//! 意味（何 ms ごとに1回実行するか）を持つ。配下に置けるのは
//! `tag_kind = "db"` のタグだけで、その `address` は結果**列名**になる
//! （[`crate::tag::DB_TAG_KIND`]、配置制約は
//! `crate::tag::validate_tag_kind_placement`）。
//!
//! S1a の時点ではこの protocol 配下にグループを作ること自体を拒否していた
//! （`query_sql` も `db` タグも無く、置き場が存在しなかったため）。S2 が
//! その予告どおり解禁し、代わりに [`validate_query_sql`] が「postgres 配下
//! では `query_sql` 必須・それ以外では指定不可」という2方向のルールを
//! 強制する。

use banto_core::{BantoError, FieldError, ListParams, ListResult};
use banto_storage::ColumnMap;
use serde::{Deserialize, Serialize};
use sqlx::{QueryBuilder, Sqlite, SqliteConnection, SqlitePool};

use crate::plc_connection::{HUB_PROTOCOL, POSTGRES_PROTOCOL, PUSH_PROTOCOL};
use crate::support::{map_write_error, max_length_message, required_message};

/// Selectable collection periods, milliseconds (recorder-requirements.md
/// §3.1: "標準 1s / 選択肢 100ms・200ms・500ms・2s・5s・10s・1min") - mirrors
/// the SQL `CHECK` in `migrations/0002_collection_groups.sql`.
pub const ALLOWED_PERIOD_MS: &[i64] = &[100, 200, 500, 1_000, 2_000, 5_000, 10_000, 60_000];

const MAX_NAME_LEN: usize = 100;

fn default_enabled() -> bool {
    true
}

/// T19 S1-b（UX-34、docs/banto-hub-t19-design.md §2・§3.3、2026-09-02
/// オーナー決定）: `default_writable` の既定値。UX-34 の全体方針「既定
/// ON」に合わせ、`CollectionGroupInput` に列を省略した既存クライアント
/// （relay-wright/chronogazer にはこの列自体が無いので該当しないが、将来
/// 追従が遅れた banto-hub クライアントを想定した後方互換）は「既定 ON」の
/// グループとして作成される - `migrations/0012_collection_groups_add_
/// default_writable.sql` の列既定値 `1` と揃えてある。
fn default_writable_true() -> bool {
    true
}

/// A row of the `collection_groups` table, wire-shaped (camelCase) for a
/// future settings grid (recorder-requirements.md §6 "グループ設定" screen).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct CollectionGroup {
    pub id: i64,
    pub name: String,
    pub plc_connection_id: i64,
    pub period_ms: i64,
    pub enabled: bool,
    /// T19 S1-b（UX-34）: このグループへ新規タグを登録するときの
    /// `writable` チェックボックスの初期値（banto-hub の UI 向け既定値で
    /// あり、`tags.writable` 自体の検証ルールとは無関係 - `tag.rs::
    /// validate_tag_input` の computed タグ拒否・Modbus 読み取り専用領域
    /// 拒否はこの値に関係なくそのまま効く）。relay-wright/chronogazer の
    /// 収集・書き込み動作はこの列を一切参照しない（`migrations/0012_
    /// collection_groups_add_default_writable.sql` 参照）。
    pub default_writable: bool,
    /// 外部 DB 連携 S2（docs/banto-hub-external-db-design.md §4.1・§4.2、
    /// 2026-09-06 オーナー決定「案A」）: このグループが1周期ごとに実行する
    /// SELECT 文（「1 グループ = 1 SELECT = N タグ」）。
    /// `protocol = "postgres"` の接続配下のグループでは**必須**、それ以外の
    /// 接続配下では常に `None` - 両方向とも [`validate_query_sql`] が強制
    /// する（`migrations/0015_db_source_query_sql_and_tag_kind.sql`）。
    pub query_sql: Option<String>,
}

/// Create/update payload.
#[derive(Debug, Clone, Deserialize)]
pub struct CollectionGroupInput {
    pub name: String,
    pub plc_connection_id: i64,
    pub period_ms: i64,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// T19 S1-b（UX-34）: 省略時は `default_writable_true`（既定 ON、
    /// migration 0012 の列既定値と同じ）。
    #[serde(default = "default_writable_true")]
    pub default_writable: bool,
    /// S2: [`CollectionGroup::query_sql`] の入力側。省略時は `None`
    /// （PLC 接続配下のグループはこれが正しい - `postgres` 接続配下で
    /// 省略した場合は [`validate_query_sql`] が「必須です」エラーにする）。
    #[serde(default)]
    pub query_sql: Option<String>,
}

/// Validate a [`CollectionGroupInput`]: `name` trimmed non-empty and capped
/// at `MAX_NAME_LEN`, `period_ms` in [`ALLOWED_PERIOD_MS`]. `plc_connection_id`
/// referring to a real row is enforced by the `FOREIGN KEY` at write time
/// (see [`crate::support::map_write_error`]), not here - a plain existence
/// check here would be a second round trip for something the database
/// constraint already guarantees atomically.
fn validate_collection_group_input(input: &CollectionGroupInput) -> Result<(), BantoError> {
    let mut errors: Vec<FieldError> = Vec::new();

    let trimmed_name = input.name.trim();
    if trimmed_name.is_empty() {
        errors.push(FieldError {
            field: "name".to_string(),
            message: required_message(),
        });
    } else if trimmed_name.chars().count() > MAX_NAME_LEN {
        errors.push(FieldError {
            field: "name".to_string(),
            message: max_length_message(MAX_NAME_LEN),
        });
    }

    if !ALLOWED_PERIOD_MS.contains(&input.period_ms) {
        let choices = ALLOWED_PERIOD_MS
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        errors.push(FieldError {
            field: "periodMs".to_string(),
            message: format!("収集周期は次のいずれかを選択してください: {choices}"),
        });
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(BantoError::Validation {
            field_errors: errors,
        })
    }
}

/// [`CollectionGroupInput::query_sql`] の長さ上限（外部 DB 連携設計 §4.2
/// 「文の検証はベストエフォート」）。8 KiB は「人が書いた1本の SELECT」に
/// 対しては十分に大きく、それでも無制限ではない、という anti-abuse の境界
/// （[`MAX_NAME_LEN`] が `name` に対して果たしているのと同じ役割）。
const MAX_QUERY_SQL_LEN: usize = 8192;

/// S2（docs/banto-hub-external-db-design.md §4.1・§4.2、2026-09-06 オーナー
/// 決定「案A」）: グループの `query_sql` を、そのグループが属する接続の
/// `protocol` に照らして検証し、**保存する値**（`postgres` 配下なら trim 済み
/// の `Some`、それ以外は常に `None`）を返す。S1a の
/// `reject_group_under_postgres_connection`（「DB 接続配下にグループを作れ
/// ない」）を置き換えるもので、S2 で解禁するというその関数の doc comment の
/// 予告そのもの。
///
/// 2方向の1つのルール:
///
/// - `protocol == `[`POSTGRES_PROTOCOL`]: `query_sql` は**必須**（「1 グループ
///   = 1 SELECT = N タグ」§4.2 - SQL の無いグループには回すものが無い）。
/// - それ以外のプロトコル: `query_sql` は**指定できない**（PLC グループが
///   迷子の SQL 文を抱えないようにする - `plc_connection.rs` の
///   `database`/`username`/`password` が `postgres` 以外で禁止されているのと
///   同型の対称ルール）。空文字/空白のみは「未指定」と同じ扱いにして、
///   フォームが常に空文字を送ってくるクライアントを弾かない。
///
/// **文そのものの検証はベストエフォート**（§4.2）: trim して非空・
/// [`MAX_QUERY_SQL_LEN`] 以内・先頭キーワードが `SELECT` または `WITH`
/// （大文字小文字を問わない）・`;` を含まない（単文であること）だけを見る。
/// SQL パーサは持たない - **本当の防御は DB 側の read-only ユーザー**であり、
/// ここの検証は「うっかり UPDATE を貼り付けた」を早期に気づかせるための
/// ものに過ぎない。この方針は UI と docs にも明記する（§4.2）。
///
/// `plc_connection_id` が存在しない行を指す場合（`protocol` が `None`）は
/// 何も検証せず `Ok(None)` を返す - そちらは既存の FK 制約が
/// [`FK_MESSAGE`] 付きで拒否する、この関数の関心事ではない（S1a の同じ
/// 判断を引き継ぐ）。
fn validate_query_sql(
    protocol: Option<&str>,
    query_sql: Option<&str>,
) -> Result<Option<String>, BantoError> {
    let trimmed = query_sql.map(str::trim).filter(|s| !s.is_empty());

    let error = |message: String| -> BantoError {
        BantoError::Validation {
            field_errors: vec![FieldError {
                field: "querySql".to_string(),
                message,
            }],
        }
    };

    match protocol {
        None => Ok(None),
        Some(POSTGRES_PROTOCOL) => {
            let Some(sql) = trimmed else {
                return Err(error(required_message()));
            };
            if let Some(message) = query_sql_error(sql) {
                return Err(error(message));
            }
            Ok(Some(sql.to_string()))
        }
        Some(_) => {
            if trimmed.is_some() {
                return Err(error(
                    "querySql は postgres 接続配下のグループでのみ指定できます".to_string(),
                ));
            }
            Ok(None)
        }
    }
}

/// [`validate_query_sql`] の SQL の形の規則（純関数）: 前後の空白を除いて空でない・
/// [`MAX_QUERY_SQL_LEN`] 文字以内・`;` なし・先頭のキーワードが `SELECT` か `WITH`。
/// 破っていればその文言。R2a-2a で `push` のブリッジのクエリ
/// （`crate::push::validate_push_settings`、ingest-design §3.2「`validate_query_sql`
/// と同じ規則」）と共有するために切り出した。
pub(crate) fn query_sql_error(sql: &str) -> Option<String> {
    let sql = sql.trim();
    if sql.is_empty() {
        return Some(required_message());
    }
    if sql.chars().count() > MAX_QUERY_SQL_LEN {
        return Some(max_length_message(MAX_QUERY_SQL_LEN));
    }
    if sql.contains(';') {
        return Some("SQL は単文で指定してください（';' は使用できません）".to_string());
    }
    let first_keyword: String = sql
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect::<String>()
        .to_ascii_uppercase();
    if first_keyword != "SELECT" && first_keyword != "WITH" {
        return Some("SQL は SELECT または WITH で始まる必要があります".to_string());
    }
    None
}

/// #383 段階3 P2（`crate::plc_connection` の "`\"hub\"`" 節）: グループの現在の
/// 接続のプロトコルと、配下のタグ数。[`hub_group_move_error`] の入力。R2a-2a で
/// 今の接続の id を足した（[`push_group_move_error`] の入力）。
const GROUP_PROTOCOL_AND_TAG_COUNT_SQL: &str = "SELECT pc.protocol, \
     (SELECT COUNT(*) FROM tags WHERE collection_group_id = cg.id), cg.plc_connection_id \
     FROM collection_groups cg JOIN plc_connections pc ON pc.id = cg.plc_connection_id \
     WHERE cg.id = ?";

/// #383 段階3 P2: タグを持つグループを `hub` 接続と `hub` 以外の接続の間で
/// 移すことを拒否する。Hub 経由のタグの `address` は外部名、PLC のタグの
/// `address` はデバイスアドレスで互換が無いため、移した瞬間に配下の全タグが
/// 新しい接続で解釈できなくなる（接続のプロトコル変更を拒否するのと同じ理由 -
/// `PlcConnectionService::update`）。`current` は (今の接続のプロトコル,
/// 配下のタグ数)、`None` はグループが存在しない（UPDATE が not-found を返す）。
/// `new_protocol` は移動先の接続のプロトコル、`None` は接続が存在しない
/// （FK が拒否する）。
fn hub_group_move_error(
    current: Option<(&str, i64)>,
    new_protocol: Option<&str>,
) -> Option<BantoError> {
    let (current_protocol, tag_count) = current?;
    let new_protocol = new_protocol?;
    let crosses = (current_protocol == HUB_PROTOCOL) != (new_protocol == HUB_PROTOCOL);
    if !crosses || tag_count == 0 {
        return None;
    }
    Some(BantoError::Validation {
        field_errors: vec![FieldError {
            field: "plcConnectionId".to_string(),
            message: format!(
                "このグループにはタグが{tag_count}件あるため、Hub 接続と他の接続の間で移せません\
                 （Hub 経由のタグのアドレスは外部名で、PLC のアドレスとは互換がありません）"
            ),
        }],
    })
}

/// R2a-2a（PR #753 のレビュー P2）: タグを持つグループを、`push` 接続へ・`push`
/// 接続から**別の接続へ**移すことを拒否する（`push` 接続どうしの移動も含む）。
/// `push` のタグのキーは**接続ごとに**一意（`crate::tag`、migration `0024`）で、
/// `push` のタグだけの規則（`string`・書き込み可・スケーリング不可）もある。グループを
/// 移すとタグの書き込みを通らずに接続が変わるので、移動先で重複したキーや規則に
/// 合わないタグが生まれうる。移動先の接続全体を検査し直す代わりに、`hub` と同じく
/// 移動そのものを断る（空のグループは移せる。同じ接続の中での更新は移動ではない）。
/// DB 側の最後の砦は `0024` の `collection_groups_push_key_unique_move` トリガ。
///
/// `current` は (今の接続のプロトコル, 配下のタグ数, 今の接続の id)、`None` は
/// グループが存在しない。`new` は (移動先の接続の id, そのプロトコル)、プロトコルが
/// `None` は接続が存在しない（FK が拒否する）。
fn push_group_move_error(
    current: Option<(&str, i64, i64)>,
    new_connection_id: i64,
    new_protocol: Option<&str>,
) -> Option<BantoError> {
    let (current_protocol, tag_count, current_connection_id) = current?;
    let new_protocol = new_protocol?;
    let touches_push = current_protocol == PUSH_PROTOCOL || new_protocol == PUSH_PROTOCOL;
    if !touches_push || current_connection_id == new_connection_id || tag_count == 0 {
        return None;
    }
    Some(BantoError::Validation {
        field_errors: vec![FieldError {
            field: "plcConnectionId".to_string(),
            message: format!(
                "このグループにはタグが{tag_count}件あるため、push 接続へ・push 接続から別の接続へ\
                 移せません（push のタグのキーは接続ごとに一意で、移すと重複や規則に合わない\
                 タグが生まれうるため）"
            ),
        }],
    })
}

fn column_map() -> ColumnMap {
    ColumnMap::new()
        .column("id", "id")
        .column("name", "name")
        .column("plcConnectionId", "plc_connection_id")
        .column("periodMs", "period_ms")
        .column("enabled", "enabled")
        .column("defaultWritable", "default_writable")
        .column("querySql", "query_sql")
}

const RESOURCE: &str = "collection_groups";
const COLUMNS: &str =
    "id, name, plc_connection_id, period_ms, enabled, default_writable, query_sql";
const FK_MESSAGE: &str = "指定されたPLC接続が見つかりません";

/// UNIQUE-violation message for `collection_groups.name`, which is unique
/// **per connection** as of migration 0022 (`UNIQUE(plc_connection_id, name)`,
/// 2026-10-19 オーナー決定 - 接続内で一意). Mirrors `tag.rs`'s
/// `NAME_ALREADY_USED_IN_GROUP`: the scope is spelled out so a user who sees
/// the same name accepted under another connection is not confused. An
/// update that moves a group to another connection (`plcConnectionId`
/// changed) can collide in the destination - that is the same constraint and
/// gets the same `name` field error.
const NAME_ALREADY_USED_IN_CONNECTION: &str = "この接続内では既に使用されています";

/// Service layer for the `collection_groups` resource.
#[derive(Clone)]
pub struct CollectionGroupService {
    pool: SqlitePool,
}

impl CollectionGroupService {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn list(
        &self,
        params: ListParams,
    ) -> Result<ListResult<CollectionGroup>, BantoError> {
        let columns = column_map();

        let mut rows_builder: QueryBuilder<Sqlite> =
            QueryBuilder::new(format!("SELECT {COLUMNS} FROM collection_groups"));
        banto_storage::list_query::sqlite::apply_list_params(&mut rows_builder, &columns, &params)?;
        let rows: Vec<CollectionGroup> = rows_builder
            .build_query_as::<CollectionGroup>()
            .fetch_all(&self.pool)
            .await
            .map_err(banto_storage::storage_error)?;

        let mut count_builder: QueryBuilder<Sqlite> =
            QueryBuilder::new("SELECT COUNT(*) FROM collection_groups");
        banto_storage::list_query::sqlite::append_where(
            &mut count_builder,
            &columns,
            &params.filters,
        )?;
        let total_count: i64 = count_builder
            .build_query_scalar()
            .fetch_one(&self.pool)
            .await
            .map_err(banto_storage::storage_error)?;

        Ok(ListResult {
            rows,
            total_count: total_count as u64,
        })
    }

    pub async fn get(&self, id: i64) -> Result<CollectionGroup, BantoError> {
        // AssertSqlSafe: 補間されるのは COLUMNS 定数（本ファイル内の固定文字列）
        // のみで、外部入力は含まれない。id はプレースホルダでバインドする。
        sqlx::query_as::<_, CollectionGroup>(sqlx::AssertSqlSafe(format!(
            "SELECT {COLUMNS} FROM collection_groups WHERE id = ?"
        )))
        .bind(id)
        .fetch_one(&self.pool)
        .await
        .map_err(|err| banto_storage::not_found(err, RESOURCE, id.to_string()))
    }

    pub async fn create(&self, input: CollectionGroupInput) -> Result<CollectionGroup, BantoError> {
        validate_collection_group_input(&input)?;
        let existing_protocol: Option<String> =
            sqlx::query_scalar("SELECT protocol FROM plc_connections WHERE id = ?")
                .bind(input.plc_connection_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(banto_storage::storage_error)?;
        let stored_query_sql =
            validate_query_sql(existing_protocol.as_deref(), input.query_sql.as_deref())?;
        // AssertSqlSafe: get() と同じ理由 - COLUMNS 定数のみを埋め込む固定
        // 文字列。値はすべてプレースホルダでバインドする。
        sqlx::query_as::<_, CollectionGroup>(sqlx::AssertSqlSafe(format!(
            "INSERT INTO collection_groups (name, plc_connection_id, period_ms, enabled, default_writable, query_sql) \
             VALUES (?, ?, ?, ?, ?, ?) RETURNING {COLUMNS}"
        )))
        .bind(input.name.trim())
        .bind(input.plc_connection_id)
        .bind(input.period_ms)
        .bind(input.enabled)
        .bind(input.default_writable)
        .bind(stored_query_sql)
        .fetch_one(&self.pool)
        .await
        .map_err(|err| {
            map_write_error(
                err,
                "name",
                NAME_ALREADY_USED_IN_CONNECTION,
                "plcConnectionId",
                FK_MESSAGE,
            )
        })
    }

    /// Transaction-compatible counterpart of [`Self::create`].
    pub async fn create_tx(
        &self,
        connection: &mut SqliteConnection,
        input: CollectionGroupInput,
    ) -> Result<CollectionGroup, BantoError> {
        validate_collection_group_input(&input)?;
        let existing_protocol: Option<String> =
            sqlx::query_scalar("SELECT protocol FROM plc_connections WHERE id = ?")
                .bind(input.plc_connection_id)
                .fetch_optional(&mut *connection)
                .await
                .map_err(banto_storage::storage_error)?;
        let stored_query_sql =
            validate_query_sql(existing_protocol.as_deref(), input.query_sql.as_deref())?;
        // AssertSqlSafe: get() と同じ理由 - COLUMNS 定数のみを埋め込む固定
        // 文字列。値はすべてプレースホルダでバインドする。
        sqlx::query_as::<_, CollectionGroup>(sqlx::AssertSqlSafe(format!(
            "INSERT INTO collection_groups (name, plc_connection_id, period_ms, enabled, default_writable, query_sql) \
             VALUES (?, ?, ?, ?, ?, ?) RETURNING {COLUMNS}"
        )))
        .bind(input.name.trim())
        .bind(input.plc_connection_id)
        .bind(input.period_ms)
        .bind(input.enabled)
        .bind(input.default_writable)
        .bind(stored_query_sql)
        .fetch_one(&mut *connection)
        .await
        .map_err(|err| {
            map_write_error(
                err,
                "name",
                NAME_ALREADY_USED_IN_CONNECTION,
                "plcConnectionId",
                FK_MESSAGE,
            )
        })
    }

    pub async fn update(
        &self,
        id: i64,
        input: CollectionGroupInput,
    ) -> Result<CollectionGroup, BantoError> {
        validate_collection_group_input(&input)?;
        let existing_protocol: Option<String> =
            sqlx::query_scalar("SELECT protocol FROM plc_connections WHERE id = ?")
                .bind(input.plc_connection_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(banto_storage::storage_error)?;
        let stored_query_sql =
            validate_query_sql(existing_protocol.as_deref(), input.query_sql.as_deref())?;
        let current: Option<(String, i64, i64)> = sqlx::query_as(GROUP_PROTOCOL_AND_TAG_COUNT_SQL)
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(banto_storage::storage_error)?;
        if let Some(err) = hub_group_move_error(
            current
                .as_ref()
                .map(|(protocol, count, _)| (protocol.as_str(), *count)),
            existing_protocol.as_deref(),
        ) {
            return Err(err);
        }
        if let Some(err) = push_group_move_error(
            current
                .as_ref()
                .map(|(protocol, count, conn)| (protocol.as_str(), *count, *conn)),
            input.plc_connection_id,
            existing_protocol.as_deref(),
        ) {
            return Err(err);
        }
        // AssertSqlSafe: get() と同じ理由 - COLUMNS 定数のみを埋め込む固定
        // 文字列。値はすべてプレースホルダでバインドする。
        sqlx::query_as::<_, CollectionGroup>(sqlx::AssertSqlSafe(format!(
            "UPDATE collection_groups SET name = ?, plc_connection_id = ?, period_ms = ?, enabled = ?, default_writable = ?, query_sql = ? \
             WHERE id = ? RETURNING {COLUMNS}"
        )))
        .bind(input.name.trim())
        .bind(input.plc_connection_id)
        .bind(input.period_ms)
        .bind(input.enabled)
        .bind(input.default_writable)
        .bind(stored_query_sql)
        .bind(id)
        .fetch_one(&self.pool)
        .await
        .map_err(|err| match err {
            sqlx::Error::RowNotFound => BantoError::NotFound {
                resource: RESOURCE.to_string(),
                id: id.to_string(),
            },
            other => map_write_error(other, "name", NAME_ALREADY_USED_IN_CONNECTION, "plcConnectionId", FK_MESSAGE),
        })
    }

    /// Transaction-compatible counterpart of [`Self::update`].
    pub async fn update_tx(
        &self,
        connection: &mut SqliteConnection,
        id: i64,
        input: CollectionGroupInput,
    ) -> Result<CollectionGroup, BantoError> {
        validate_collection_group_input(&input)?;
        let existing_protocol: Option<String> =
            sqlx::query_scalar("SELECT protocol FROM plc_connections WHERE id = ?")
                .bind(input.plc_connection_id)
                .fetch_optional(&mut *connection)
                .await
                .map_err(banto_storage::storage_error)?;
        let stored_query_sql =
            validate_query_sql(existing_protocol.as_deref(), input.query_sql.as_deref())?;
        let current: Option<(String, i64, i64)> = sqlx::query_as(GROUP_PROTOCOL_AND_TAG_COUNT_SQL)
            .bind(id)
            .fetch_optional(&mut *connection)
            .await
            .map_err(banto_storage::storage_error)?;
        if let Some(err) = hub_group_move_error(
            current
                .as_ref()
                .map(|(protocol, count, _)| (protocol.as_str(), *count)),
            existing_protocol.as_deref(),
        ) {
            return Err(err);
        }
        if let Some(err) = push_group_move_error(
            current
                .as_ref()
                .map(|(protocol, count, conn)| (protocol.as_str(), *count, *conn)),
            input.plc_connection_id,
            existing_protocol.as_deref(),
        ) {
            return Err(err);
        }
        // AssertSqlSafe: get() と同じ理由 - COLUMNS 定数のみを埋め込む固定
        // 文字列。値はすべてプレースホルダでバインドする。
        sqlx::query_as::<_, CollectionGroup>(sqlx::AssertSqlSafe(format!(
            "UPDATE collection_groups SET name = ?, plc_connection_id = ?, period_ms = ?, enabled = ?, default_writable = ?, query_sql = ? \
             WHERE id = ? RETURNING {COLUMNS}"
        )))
        .bind(input.name.trim())
        .bind(input.plc_connection_id)
        .bind(input.period_ms)
        .bind(input.enabled)
        .bind(input.default_writable)
        .bind(stored_query_sql)
        .bind(id)
        .fetch_one(&mut *connection)
        .await
        .map_err(|err| match err {
            sqlx::Error::RowNotFound => BantoError::NotFound {
                resource: RESOURCE.to_string(),
                id: id.to_string(),
            },
            other => map_write_error(other, "name", NAME_ALREADY_USED_IN_CONNECTION, "plcConnectionId", FK_MESSAGE),
        })
    }

    /// Delete, refusing when any [`crate::tag::Tag`] still references this
    /// group (docs/plan.md I1 spec: same "在籍タグ/グループ数を数えて
    /// Validation エラー" rule as
    /// [`crate::plc_connection::PlcConnectionService::delete`]).
    pub async fn delete(&self, id: i64) -> Result<(), BantoError> {
        let tag_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM tags WHERE collection_group_id = ?")
                .bind(id)
                .fetch_one(&self.pool)
                .await
                .map_err(banto_storage::storage_error)?;
        if tag_count > 0 {
            return Err(BantoError::Validation {
                field_errors: vec![FieldError {
                    field: "id".to_string(),
                    message: format!(
                        "このグループに属するタグが{tag_count}件あるため削除できません"
                    ),
                }],
            });
        }

        let result = sqlx::query("DELETE FROM collection_groups WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(banto_storage::storage_error)?;
        if result.rows_affected() == 0 {
            return Err(BantoError::NotFound {
                resource: RESOURCE.to_string(),
                id: id.to_string(),
            });
        }
        Ok(())
    }

    /// Transaction-compatible counterpart of [`Self::delete`].
    pub async fn delete_tx(
        &self,
        connection: &mut SqliteConnection,
        id: i64,
    ) -> Result<(), BantoError> {
        let tag_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM tags WHERE collection_group_id = ?")
                .bind(id)
                .fetch_one(&mut *connection)
                .await
                .map_err(banto_storage::storage_error)?;
        if tag_count > 0 {
            return Err(BantoError::Validation {
                field_errors: vec![FieldError {
                    field: "id".to_string(),
                    message: format!(
                        "このグループに属するタグが{tag_count}件あるため削除できません"
                    ),
                }],
            });
        }
        let result = sqlx::query("DELETE FROM collection_groups WHERE id = ?")
            .bind(id)
            .execute(&mut *connection)
            .await
            .map_err(banto_storage::storage_error)?;
        if result.rows_affected() == 0 {
            return Err(BantoError::NotFound {
                resource: RESOURCE.to_string(),
                id: id.to_string(),
            });
        }
        Ok(())
    }

    /// T19 S2-b（UX-38、docs/banto-hub-t19-design.md §3.4・§7.5）: カスケード
    /// 削除の入口 - [`crate::plc_connection::PlcConnectionService::cascade_delete_tx`]
    /// の doc comment がここでの設計判断（[`Self::delete`]/[`Self::delete_tx`]
    /// は変更せず新メソッドを足す理由 - relay-wright との契約）をそのまま
    /// 説明しているので、そちらを参照。このグループに属する全タグ →
    /// このグループ、の順（FK 順）で削除する。tstore の履歴には触れない
    /// （同じ理由 - このクレートは banto-tstore に依存しない）。
    pub async fn cascade_delete_tx(
        &self,
        connection: &mut SqliteConnection,
        id: i64,
    ) -> Result<CollectionGroupCascadeOutcome, BantoError> {
        let deleted_tags = sqlx::query("DELETE FROM tags WHERE collection_group_id = ?")
            .bind(id)
            .execute(&mut *connection)
            .await
            .map_err(banto_storage::storage_error)?
            .rows_affected() as i64;

        let result = sqlx::query("DELETE FROM collection_groups WHERE id = ?")
            .bind(id)
            .execute(&mut *connection)
            .await
            .map_err(banto_storage::storage_error)?;
        if result.rows_affected() == 0 {
            return Err(BantoError::NotFound {
                resource: RESOURCE.to_string(),
                id: id.to_string(),
            });
        }

        Ok(CollectionGroupCascadeOutcome { deleted_tags })
    }

    /// Non-transactional counterpart of [`Self::cascade_delete_tx`] - opens
    /// its own transaction so both DELETEs stay atomic (mirrors
    /// [`crate::plc_connection::PlcConnectionService::cascade_delete`]).
    pub async fn cascade_delete(
        &self,
        id: i64,
    ) -> Result<CollectionGroupCascadeOutcome, BantoError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(banto_storage::storage_error)?;
        let outcome = self.cascade_delete_tx(&mut tx, id).await?;
        tx.commit().await.map_err(banto_storage::storage_error)?;
        Ok(outcome)
    }
}

/// What [`CollectionGroupService::cascade_delete`]/[`CollectionGroupService::cascade_delete_tx`]
/// removed besides the group row itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectionGroupCascadeOutcome {
    pub deleted_tags: i64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrate;
    use crate::plc_connection::{PlcConnectionInput, PlcConnectionService};
    use banto_core::{FilterOp, FilterState, Pagination, SortDirection, SortState};
    use serde_json::json;

    async fn setup() -> (PlcConnectionService, CollectionGroupService, i64) {
        let pool = banto_storage::connect_sqlite_memory()
            .await
            .expect("connect_sqlite_memory");
        migrate(&pool).await.expect("migrate");
        let plc_svc = PlcConnectionService::new(pool.clone());
        let group_svc = CollectionGroupService::new(pool);
        let conn = plc_svc
            .create(PlcConnectionInput {
                name: "PLC1".to_string(),
                protocol: "modbus-tcp".to_string(),
                host: "10.0.0.1".to_string(),
                port: 502,
                unit_id: 1,
                enabled: true,
                simulation: false,

                word_order: "low_high".to_string(),
                cpu_type: None,
                database: None,
                username: None,
                secrets: Default::default(),
                settings: None,
            })
            .await
            .unwrap();
        (plc_svc, group_svc, conn.id)
    }

    fn sample_input(name: &str, plc_connection_id: i64) -> CollectionGroupInput {
        CollectionGroupInput {
            name: name.to_string(),
            plc_connection_id,
            period_ms: 1_000,
            enabled: true,
            default_writable: true,
            query_sql: None,
        }
    }

    /// S2: a ready-made `"postgres"` connection input - the DB Source tests
    /// below all need one and only differ in the connection `name`.
    fn postgres_connection_input(name: &str) -> PlcConnectionInput {
        PlcConnectionInput {
            name: name.to_string(),
            protocol: POSTGRES_PROTOCOL.to_string(),
            host: "10.0.0.50".to_string(),
            port: 5432,
            unit_id: 1,
            enabled: true,
            simulation: false,
            word_order: "low_high".to_string(),
            cpu_type: None,
            database: Some("erp".to_string()),
            username: Some("reader".to_string()),
            secrets: Default::default(),
            settings: None,
        }
    }

    #[tokio::test]
    async fn create_then_get_round_trips() {
        let (_plc_svc, svc, conn_id) = setup().await;
        let created = svc
            .create(sample_input("Group1", conn_id))
            .await
            .expect("create should succeed");
        assert_eq!(created.name, "Group1");
        assert_eq!(created.plc_connection_id, conn_id);
        assert_eq!(created.period_ms, 1_000);
        assert!(created.enabled);
        assert!(created.default_writable);

        let fetched = svc.get(created.id).await.expect("get should succeed");
        assert_eq!(fetched, created);
    }

    /// T19 S1-b（UX-34）: `default_writable` を省略した入力（後方互換 -
    /// 現行 banto-hub クライアントは常に明示的に送るが、JSON 側の
    /// `#[serde(default)]` を裏付けるテスト）は既定 `true` になる。
    #[tokio::test]
    async fn create_defaults_default_writable_to_true_when_json_omits_it() {
        let (_plc_svc, svc, conn_id) = setup().await;
        // `CollectionGroupInput` itself deserializes snake_case (no
        // `#[serde(rename_all = "camelCase")]` - only `CollectionGroup`, the
        // read model, is wire-shaped that way; the camelCase mirroring for
        // input happens one layer up, in `banto_hub_core::rest::
        // CollectionGroupPayload`). This test exercises this crate's own
        // `#[serde(default)]` directly, so it uses this type's own field
        // names.
        let input: CollectionGroupInput = serde_json::from_value(json!({
            "name": "NoDefaultWritableField",
            "plc_connection_id": conn_id,
            "period_ms": 1_000,
        }))
        .expect("deserialize should succeed without default_writable");
        let created = svc.create(input).await.expect("create should succeed");
        assert!(created.default_writable);
    }

    /// T19 S1-b（UX-34「収集グループ単位で既定値を変更できる」）:
    /// `default_writable: false` で作成・更新した値が往復する。
    #[tokio::test]
    async fn create_and_update_round_trip_default_writable_false() {
        let (_plc_svc, svc, conn_id) = setup().await;
        let mut input = sample_input("OptOut", conn_id);
        input.default_writable = false;
        let created = svc.create(input).await.expect("create should succeed");
        assert!(!created.default_writable);

        let mut update_input = sample_input("OptOut", conn_id);
        update_input.default_writable = true;
        let updated = svc
            .update(created.id, update_input)
            .await
            .expect("update should succeed");
        assert!(updated.default_writable);
    }

    #[tokio::test]
    async fn create_rejects_empty_name() {
        let (_plc_svc, svc, conn_id) = setup().await;
        let mut input = sample_input("X", conn_id);
        input.name = "  ".to_string();
        let err = svc.create(input).await.unwrap_err();
        match err {
            BantoError::Validation { field_errors } => assert_eq!(field_errors[0].field, "name"),
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_rejects_name_over_max_len() {
        let (_plc_svc, svc, conn_id) = setup().await;
        let mut input = sample_input("X", conn_id);
        input.name = "あ".repeat(MAX_NAME_LEN + 1);
        let err = svc.create(input).await.unwrap_err();
        match err {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "name");
                assert_eq!(field_errors[0].message, max_length_message(MAX_NAME_LEN));
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_accepts_every_allowed_period() {
        let (_plc_svc, svc, conn_id) = setup().await;
        for (i, period) in ALLOWED_PERIOD_MS.iter().enumerate() {
            let mut input = sample_input(&format!("G{i}"), conn_id);
            input.period_ms = *period;
            svc.create(input)
                .await
                .unwrap_or_else(|e| panic!("period {period} should be accepted: {e:?}"));
        }
    }

    #[tokio::test]
    async fn create_rejects_disallowed_period() {
        let (_plc_svc, svc, conn_id) = setup().await;
        let mut input = sample_input("X", conn_id);
        input.period_ms = 750;
        let err = svc.create(input).await.unwrap_err();
        match err {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "periodMs")
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_rejects_duplicate_name() {
        let (_plc_svc, svc, conn_id) = setup().await;
        svc.create(sample_input("Dup", conn_id)).await.unwrap();
        let err = svc.create(sample_input("Dup", conn_id)).await.unwrap_err();
        match err {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "name");
                assert_eq!(
                    field_errors[0].message,
                    "この接続内では既に使用されています"
                );
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    /// A second connection for the per-connection uniqueness tests below.
    async fn second_connection(plc_svc: &PlcConnectionService) -> i64 {
        plc_svc
            .create(PlcConnectionInput {
                name: "PLC2".to_string(),
                protocol: "modbus-tcp".to_string(),
                host: "10.0.0.2".to_string(),
                port: 502,
                unit_id: 1,
                enabled: true,
                simulation: false,
                word_order: "low_high".to_string(),
                cpu_type: None,
                database: None,
                username: None,
                secrets: Default::default(),
                settings: None,
            })
            .await
            .unwrap()
            .id
    }

    /// 2026-10-19 オーナー決定（migration 0022）: グループ名は接続内で一意。
    /// 別の接続の下なら同じ名前を付けられる（作成・トランザクション版とも）。
    #[tokio::test]
    async fn same_name_is_accepted_under_two_connections() {
        let (plc_svc, svc, conn_a) = setup().await;
        let conn_b = second_connection(&plc_svc).await;
        let a = svc.create(sample_input("fast", conn_a)).await.unwrap();
        let b = svc
            .create(sample_input("fast", conn_b))
            .await
            .expect("same name under another connection should be accepted");
        assert_ne!(a.id, b.id);
        assert_eq!(b.plc_connection_id, conn_b);

        // The transactional path behaves the same (and still refuses a
        // duplicate within one connection).
        let mut tx = svc.pool.begin().await.unwrap();
        let conn_c = {
            let created = plc_svc
                .create_tx(
                    &mut tx,
                    PlcConnectionInput {
                        name: "PLC3".to_string(),
                        protocol: "modbus-tcp".to_string(),
                        host: "10.0.0.3".to_string(),
                        port: 502,
                        unit_id: 1,
                        enabled: true,
                        simulation: false,
                        word_order: "low_high".to_string(),
                        cpu_type: None,
                        database: None,
                        username: None,
                        secrets: Default::default(),
                        settings: None,
                    },
                )
                .await
                .unwrap();
            created.id
        };
        svc.create_tx(&mut tx, sample_input("fast", conn_c))
            .await
            .expect("create_tx under a third connection");
        let err = svc
            .create_tx(&mut tx, sample_input("fast", conn_c))
            .await
            .unwrap_err();
        match err {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "name");
                assert_eq!(field_errors[0].message, NAME_ALREADY_USED_IN_CONNECTION);
            }
            other => panic!("expected Validation, got {other:?}"),
        }
        tx.rollback().await.unwrap();
    }

    /// Moving a group to another connection that already has a group of the
    /// same name collides in the destination: `name` field error (both the
    /// pool and the transactional update). Renaming it on the way in works.
    #[tokio::test]
    async fn update_moving_into_a_connection_that_has_the_name_is_a_name_error() {
        let (plc_svc, svc, conn_a) = setup().await;
        let conn_b = second_connection(&plc_svc).await;
        svc.create(sample_input("fast", conn_a)).await.unwrap();
        let moving = svc.create(sample_input("fast", conn_b)).await.unwrap();

        let err = svc
            .update(moving.id, sample_input("fast", conn_a))
            .await
            .unwrap_err();
        match err {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors.len(), 1);
                assert_eq!(field_errors[0].field, "name");
                assert_eq!(field_errors[0].message, NAME_ALREADY_USED_IN_CONNECTION);
            }
            other => panic!("expected Validation, got {other:?}"),
        }

        let mut tx = svc.pool.begin().await.unwrap();
        let err = svc
            .update_tx(&mut tx, moving.id, sample_input("fast", conn_a))
            .await
            .unwrap_err();
        match err {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "name");
                assert_eq!(field_errors[0].message, NAME_ALREADY_USED_IN_CONNECTION);
            }
            other => panic!("expected Validation, got {other:?}"),
        }
        tx.rollback().await.unwrap();

        let moved = svc
            .update(moving.id, sample_input("fast2", conn_a))
            .await
            .expect("moving under a fresh name is fine");
        assert_eq!(moved.plc_connection_id, conn_a);
        assert_eq!(svc.get(moving.id).await.unwrap().name, "fast2");
    }

    #[tokio::test]
    async fn create_rejects_missing_plc_connection_with_friendly_message() {
        let (_plc_svc, svc, _conn_id) = setup().await;
        let err = svc.create(sample_input("X", 999)).await.unwrap_err();
        match err {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "plcConnectionId");
                assert_eq!(field_errors[0].message, FK_MESSAGE);
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    /// S2 (docs/banto-hub-external-db-design.md §4.2): `postgres` 接続配下の
    /// グループは `query_sql` **必須** - S1a の「DB 接続配下にはそもそも
    /// グループを作れない」拒否を置き換えたルール（このモジュール doc
    /// comment の「DB Source のグループ」節参照）。
    #[tokio::test]
    async fn create_rejects_a_group_under_a_postgres_connection_without_query_sql() {
        let (plc_svc, svc, _conn_id) = setup().await;
        let pg_conn = plc_svc
            .create(postgres_connection_input("ERP DB"))
            .await
            .unwrap();

        let err = svc
            .create(sample_input("G1", pg_conn.id))
            .await
            .unwrap_err();
        match err {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "querySql");
                assert_eq!(field_errors[0].message, required_message());
            }
            other => panic!("expected Validation, got {other:?}"),
        }

        // ...and succeeds once the SELECT is supplied.
        let mut ok_input = sample_input("G1", pg_conn.id);
        ok_input.query_sql = Some("  SELECT a, b FROM v_line1  ".to_string());
        let created = svc
            .create(ok_input)
            .await
            .expect("a postgres group with a SELECT should be accepted");
        assert_eq!(
            created.query_sql.as_deref(),
            Some("SELECT a, b FROM v_line1")
        );
    }

    /// The `update`-side twin: moving an *existing* group onto a
    /// `"postgres"` connection needs the same `query_sql` the create path
    /// does.
    #[tokio::test]
    async fn update_rejects_moving_a_group_onto_a_postgres_connection_without_query_sql() {
        let (plc_svc, svc, conn_id) = setup().await;
        let group = svc.create(sample_input("G1", conn_id)).await.unwrap();
        let pg_conn = plc_svc
            .create(postgres_connection_input("ERP DB"))
            .await
            .unwrap();

        let err = svc
            .update(group.id, sample_input("G1", pg_conn.id))
            .await
            .unwrap_err();
        match err {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "querySql");
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn update_changes_fields() {
        let (_plc_svc, svc, conn_id) = setup().await;
        let created = svc.create(sample_input("Before", conn_id)).await.unwrap();
        let mut input = sample_input("After", conn_id);
        input.period_ms = 5_000;
        let updated = svc.update(created.id, input).await.expect("update ok");
        assert_eq!(updated.name, "After");
        assert_eq!(updated.period_ms, 5_000);
    }

    #[tokio::test]
    async fn update_missing_id_is_not_found() {
        let (_plc_svc, svc, conn_id) = setup().await;
        let err = svc
            .update(999, sample_input("X", conn_id))
            .await
            .unwrap_err();
        assert!(
            matches!(err, BantoError::NotFound { resource, id } if resource == "collection_groups" && id == "999")
        );
    }

    #[tokio::test]
    async fn delete_then_get_is_not_found() {
        let (_plc_svc, svc, conn_id) = setup().await;
        let created = svc.create(sample_input("Doomed", conn_id)).await.unwrap();
        svc.delete(created.id).await.expect("delete should succeed");
        let err = svc.get(created.id).await.unwrap_err();
        assert!(matches!(err, BantoError::NotFound { .. }));
    }

    #[tokio::test]
    async fn delete_missing_id_is_not_found() {
        let (_plc_svc, svc, _conn_id) = setup().await;
        let err = svc.delete(999).await.unwrap_err();
        assert!(matches!(err, BantoError::NotFound { .. }));
    }

    #[tokio::test]
    async fn delete_refuses_when_a_tag_references_it() {
        let (_plc_svc, svc, conn_id) = setup().await;
        let group = svc.create(sample_input("InUse", conn_id)).await.unwrap();

        sqlx::query(
            "INSERT INTO tags (name, collection_group_id, address, data_type, decimals, enabled) \
             VALUES ('T1', ?, '40001', 'i16', 0, 1)",
        )
        .bind(group.id)
        .execute(&svc.pool)
        .await
        .unwrap();

        let err = svc.delete(group.id).await.unwrap_err();
        match err {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "id");
                assert!(field_errors[0].message.contains('1'));
            }
            other => panic!("expected Validation, got {other:?}"),
        }
        svc.get(group.id).await.expect("group should survive");
    }

    // --- T19 S2-b (UX-38, docs/banto-hub-t19-design.md §3.4/§7.5): cascade
    // delete - a NEW method, `delete`/`delete_tx` above are unchanged (see
    // `CollectionGroupService::cascade_delete_tx`'s doc comment, which points
    // at `PlcConnectionService::cascade_delete_tx`'s longer explanation of
    // why: relay-wright depends on the guarded methods refusing).

    /// The core UX-38 behavior for a group: tags go with it in one call.
    #[tokio::test]
    async fn cascade_delete_removes_tags_with_the_group() {
        let (_plc_svc, svc, conn_id) = setup().await;
        let group = svc.create(sample_input("InUse", conn_id)).await.unwrap();
        for name in ["T1", "T2"] {
            sqlx::query(
                "INSERT INTO tags (name, collection_group_id, address, data_type) \
                 VALUES (?, ?, '40001', 'i16')",
            )
            .bind(name)
            .bind(group.id)
            .execute(&svc.pool)
            .await
            .unwrap();
        }

        let outcome = svc
            .cascade_delete(group.id)
            .await
            .expect("cascade_delete should succeed even with tags");
        assert_eq!(outcome.deleted_tags, 2);

        assert!(matches!(
            svc.get(group.id).await.unwrap_err(),
            BantoError::NotFound { .. }
        ));
        let remaining_tags: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tags")
            .fetch_one(&svc.pool)
            .await
            .unwrap();
        assert_eq!(remaining_tags, 0);
    }

    /// A childless group behaves exactly like the old `delete`.
    #[tokio::test]
    async fn cascade_delete_with_no_tags_behaves_like_a_plain_delete() {
        let (_plc_svc, svc, conn_id) = setup().await;
        let group = svc.create(sample_input("Lonely", conn_id)).await.unwrap();
        let outcome = svc.cascade_delete(group.id).await.unwrap();
        assert_eq!(outcome.deleted_tags, 0);
        assert!(matches!(
            svc.get(group.id).await.unwrap_err(),
            BantoError::NotFound { .. }
        ));
    }

    #[tokio::test]
    async fn cascade_delete_missing_id_is_not_found() {
        let (_plc_svc, svc, _conn_id) = setup().await;
        let err = svc.cascade_delete(999).await.unwrap_err();
        assert!(matches!(err, BantoError::NotFound { .. }));
    }

    /// Same atomicity contract as
    /// `plc_connection::tests::cascade_delete_tx_rolls_back_together_with_the_callers_transaction`:
    /// `cascade_delete_tx` must not commit anything by itself, so a caller
    /// (`apps/banto-hub/core/src/rest.rs::collection_groups_delete`) that
    /// rolls its own transaction back after a failed preflight leaves both
    /// the group and its tags untouched.
    #[tokio::test]
    async fn cascade_delete_tx_rolls_back_together_with_the_callers_transaction() {
        let (_plc_svc, svc, conn_id) = setup().await;
        let group = svc.create(sample_input("Rollback", conn_id)).await.unwrap();
        sqlx::query(
            "INSERT INTO tags (name, collection_group_id, address, data_type) \
             VALUES ('T1', ?, '40001', 'i16')",
        )
        .bind(group.id)
        .execute(&svc.pool)
        .await
        .unwrap();

        let mut tx = svc.pool.begin().await.unwrap();
        let outcome = svc.cascade_delete_tx(&mut tx, group.id).await.unwrap();
        assert_eq!(outcome.deleted_tags, 1);
        tx.rollback().await.unwrap();

        svc.get(group.id)
            .await
            .expect("group should survive the rollback");
        let remaining_tags: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tags")
            .fetch_one(&svc.pool)
            .await
            .unwrap();
        assert_eq!(remaining_tags, 1);
    }

    #[tokio::test]
    async fn list_filters_sorts_and_paginates_with_total_count() {
        let (_plc_svc, svc, conn_id) = setup().await;
        for (name, period) in [("A", 100), ("B", 1_000), ("C", 5_000)] {
            let mut input = sample_input(name, conn_id);
            input.period_ms = period;
            svc.create(input).await.unwrap();
        }

        let result = svc
            .list(ListParams {
                sort: vec![SortState {
                    field: "periodMs".to_string(),
                    direction: SortDirection::Desc,
                }],
                filters: vec![FilterState {
                    field: "periodMs".to_string(),
                    op: FilterOp::Gte,
                    value: json!(1_000),
                }],
                pagination: Some(Pagination {
                    offset: 0,
                    limit: 1,
                }),
            })
            .await
            .expect("list should succeed");

        assert_eq!(result.total_count, 2);
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0].name, "C");
    }

    // --- S2 (docs/banto-hub-external-db-design.md §4.2): `query_sql` の
    // ベストエフォート検証。純関数なので DB を触らない unit test で固定する
    // （`validate_query_sql` の doc comment: 本当の防御は DB 側の read-only
    // ユーザーであり、ここは「うっかり UPDATE」を早期に気づかせるだけ）。

    #[test]
    fn validate_query_sql_accepts_select_and_with_case_insensitively() {
        for sql in [
            "SELECT a FROM t",
            "select a from t",
            "  \n select a from t \n ",
            "WITH x AS (SELECT 1) SELECT * FROM x",
            "with x as (select 1) select * from x",
        ] {
            let stored = validate_query_sql(Some(POSTGRES_PROTOCOL), Some(sql))
                .unwrap_or_else(|e| panic!("{sql:?} should be accepted: {e:?}"));
            assert_eq!(stored.as_deref(), Some(sql.trim()));
        }
    }

    #[test]
    fn validate_query_sql_rejects_non_select_statements() {
        for sql in [
            "UPDATE t SET a = 1",
            "DELETE FROM t",
            "INSERT INTO t VALUES (1)",
            "-- SELECT a FROM t",
            "(SELECT a FROM t)",
        ] {
            let err = validate_query_sql(Some(POSTGRES_PROTOCOL), Some(sql)).unwrap_err();
            match err {
                BantoError::Validation { field_errors } => {
                    assert_eq!(field_errors[0].field, "querySql", "sql: {sql:?}");
                    assert!(
                        field_errors[0].message.contains("SELECT"),
                        "sql: {sql:?} -> {:?}",
                        field_errors[0].message
                    );
                }
                other => panic!("expected Validation for {sql:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn validate_query_sql_rejects_multiple_statements() {
        let err = validate_query_sql(
            Some(POSTGRES_PROTOCOL),
            Some("SELECT a FROM t; DROP TABLE t"),
        )
        .unwrap_err();
        match err {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "querySql");
                assert!(field_errors[0].message.contains("単文"));
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[test]
    fn validate_query_sql_rejects_an_over_long_statement() {
        let sql = format!("SELECT {}", "a".repeat(MAX_QUERY_SQL_LEN));
        let err = validate_query_sql(Some(POSTGRES_PROTOCOL), Some(&sql)).unwrap_err();
        match err {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "querySql");
                assert_eq!(
                    field_errors[0].message,
                    max_length_message(MAX_QUERY_SQL_LEN)
                );
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[test]
    fn validate_query_sql_forbids_a_statement_on_a_non_postgres_connection() {
        let err = validate_query_sql(Some("modbus-tcp"), Some("SELECT a FROM t")).unwrap_err();
        match err {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "querySql");
                assert!(field_errors[0].message.contains("postgres"));
            }
            other => panic!("expected Validation, got {other:?}"),
        }
        // Blank/absent is the normal PLC case and stores as NULL.
        assert_eq!(validate_query_sql(Some("modbus-tcp"), None).unwrap(), None);
        assert_eq!(
            validate_query_sql(Some("modbus-tcp"), Some("   ")).unwrap(),
            None
        );
    }

    /// A dangling `plc_connection_id` (protocol `None`) is the FK's problem,
    /// not this function's - same stance S1a took.
    #[test]
    fn validate_query_sql_is_silent_for_an_unknown_connection() {
        assert_eq!(
            validate_query_sql(None, Some("SELECT a FROM t")).unwrap(),
            None
        );
    }

    /// A PLC group that sends a stray SQL statement is rejected end to end
    /// (not just in the pure helper above).
    #[tokio::test]
    async fn create_rejects_query_sql_under_a_plc_connection() {
        let (_plc_svc, svc, conn_id) = setup().await;
        let mut input = sample_input("G1", conn_id);
        input.query_sql = Some("SELECT a FROM t".to_string());
        let err = svc.create(input).await.unwrap_err();
        match err {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "querySql");
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    /// `query_sql` round-trips through create -> get -> update on a postgres
    /// group (the column is really persisted and really read back - the S1a
    /// `plc_connections` credential columns hit exactly this class of
    /// "forgot the column in a hand-written SELECT" bug).
    #[tokio::test]
    async fn query_sql_round_trips_through_create_get_and_update() {
        let (plc_svc, svc, _conn_id) = setup().await;
        let pg_conn = plc_svc
            .create(postgres_connection_input("ERP DB"))
            .await
            .unwrap();

        let mut input = sample_input("G1", pg_conn.id);
        input.query_sql = Some("SELECT a FROM v1".to_string());
        let created = svc.create(input).await.expect("create should succeed");
        assert_eq!(created.query_sql.as_deref(), Some("SELECT a FROM v1"));

        let fetched = svc.get(created.id).await.expect("get should succeed");
        assert_eq!(fetched, created);

        let mut update_input = sample_input("G1", pg_conn.id);
        update_input.query_sql = Some("SELECT b FROM v2".to_string());
        let updated = svc
            .update(created.id, update_input)
            .await
            .expect("update should succeed");
        assert_eq!(updated.query_sql.as_deref(), Some("SELECT b FROM v2"));

        let listed = svc.list(ListParams::default()).await.expect("list");
        assert_eq!(
            listed
                .rows
                .iter()
                .find(|g| g.id == created.id)
                .and_then(|g| g.query_sql.as_deref()),
            Some("SELECT b FROM v2")
        );
    }

    // --- #383 段階3 P2: moving a group across the "hub" boundary ---------

    /// A group that has tags cannot move between a hub connection and a
    /// non-hub one (its tags' addresses would stop making sense); an empty
    /// group can, and a move that stays on one side is unaffected - on both
    /// the pool and the transaction paths.
    #[tokio::test]
    async fn a_group_with_tags_cannot_move_across_the_hub_boundary() {
        let (plc_svc, group_svc, plc_id) = setup().await;
        let mut hub_input = postgres_connection_input("Hub");
        hub_input.protocol = HUB_PROTOCOL.to_string();
        hub_input.database = None;
        hub_input.username = None;
        let hub = plc_svc.create(hub_input).await.unwrap();
        let mut other_plc_input = postgres_connection_input("PLC2");
        other_plc_input.protocol = "slmp".to_string();
        other_plc_input.database = None;
        other_plc_input.username = None;
        let other_plc = plc_svc.create(other_plc_input).await.unwrap();

        let group = group_svc.create(sample_input("G", plc_id)).await.unwrap();
        sqlx::query(
            "INSERT INTO tags (name, collection_group_id, address, data_type) \
             VALUES ('T', ?, '40001', 'i16')",
        )
        .bind(group.id)
        .execute(&group_svc.pool)
        .await
        .unwrap();

        let err = group_svc
            .update(group.id, sample_input("G", hub.id))
            .await
            .unwrap_err();
        match err {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "plcConnectionId");
                assert!(field_errors[0].message.contains("1件"), "{field_errors:?}");
            }
            other => panic!("expected Validation, got {other:?}"),
        }
        let mut tx = group_svc.pool.begin().await.unwrap();
        assert!(matches!(
            group_svc
                .update_tx(&mut tx, group.id, sample_input("G", hub.id))
                .await,
            Err(BantoError::Validation { .. })
        ));
        drop(tx);

        // Same side of the boundary: fine.
        group_svc
            .update(group.id, sample_input("G", other_plc.id))
            .await
            .expect("modbus -> slmp move is not the hub's concern");

        // An empty group may cross, in either direction.
        let empty = group_svc.create(sample_input("E", plc_id)).await.unwrap();
        group_svc
            .update(empty.id, sample_input("E", hub.id))
            .await
            .expect("an empty group may move under the hub");
        group_svc
            .update(empty.id, sample_input("E", plc_id))
            .await
            .expect("and back");
    }

    #[test]
    fn hub_group_move_error_table() {
        type Case<'a> = (Option<(&'a str, i64)>, Option<&'a str>, bool);
        let cases: &[Case] = &[
            (Some(("modbus-tcp", 1)), Some("hub"), true),
            (Some(("hub", 3)), Some("slmp"), true),
            (Some(("modbus-tcp", 0)), Some("hub"), false),
            (Some(("hub", 3)), Some("hub"), false),
            (Some(("slmp", 3)), Some("modbus-tcp"), false),
            (None, Some("hub"), false),
            (Some(("modbus-tcp", 3)), None, false),
        ];
        for &(current, new, refused) in cases {
            assert_eq!(
                hub_group_move_error(current, new).is_some(),
                refused,
                "{current:?} -> {new:?}"
            );
        }
    }

    #[test]
    fn push_group_move_error_table() {
        type Case<'a> = (Option<(&'a str, i64, i64)>, i64, Option<&'a str>, bool);
        let cases: &[Case] = &[
            (Some(("push", 1, 1)), 2, Some("push"), true),
            (Some(("modbus-tcp", 1, 1)), 2, Some("push"), true),
            (Some(("push", 3, 1)), 2, Some("slmp"), true),
            (Some(("push", 3, 1)), 1, Some("push"), false),
            (Some(("push", 0, 1)), 2, Some("push"), false),
            (Some(("modbus-tcp", 3, 1)), 2, Some("slmp"), false),
            (None, 2, Some("push"), false),
            (Some(("push", 3, 1)), 2, None, false),
        ];
        for &(current, new_id, new, refused) in cases {
            assert_eq!(
                push_group_move_error(current, new_id, new).is_some(),
                refused,
                "{current:?} -> {new_id}/{new:?}"
            );
        }
    }

    // --- migration 0022 (collection_groups: 全体一意 → 接続内一意) ----------

    const TAG_ROW_SQL: &str = "SELECT quote(id) || '|' || quote(name) || '|' || \
         quote(collection_group_id) || '|' || quote(address) || '|' || quote(data_type) || '|' || \
         quote(string_length) || '|' || quote(raw_lo) || '|' || quote(raw_hi) || '|' || \
         quote(eng_lo) || '|' || quote(eng_hi) || '|' || quote(unit) || '|' || \
         quote(decimals) || '|' || quote(enabled) || '|' || quote(writable) || '|' || \
         quote(tag_kind) || '|' || quote(expression) || '|' || quote(retain) || '|' || \
         quote(revision) || '|' || quote(string_encoding) FROM tags ORDER BY id";
    const GROUP_ROW_SQL: &str = "SELECT quote(id) || '|' || quote(name) || '|' || \
         quote(plc_connection_id) || '|' || quote(period_ms) || '|' || quote(enabled) || '|' || \
         quote(default_writable) || '|' || quote(query_sql) FROM collection_groups ORDER BY id";

    /// Migration 0022 rebuilds `collection_groups` with
    /// `UNIQUE(plc_connection_id, name)` (2026-10-19 オーナー決定). Run the
    /// real migrator up to 0021 on a populated database, then
    /// [`crate::migrate`] - what an app does on its next startup. Every group
    /// and tag row comes back column for column with its id, every
    /// AUTOINCREMENT position survives (a deleted group's id is not reused),
    /// FKs still restrict in both directions, the index and both 0020
    /// triggers exist and still fire, and the new uniqueness scope holds: the
    /// same name under another connection is accepted, under the same
    /// connection refused.
    #[tokio::test]
    async fn migration_0022_preserves_rows_sequence_triggers_and_foreign_keys() {
        let pool = banto_storage::connect_sqlite_memory()
            .await
            .expect("connect_sqlite_memory");
        let mut migrator = sqlx::migrate!("./migrations");
        migrator.dangerous_set_table_name(crate::MIGRATIONS_TABLE);
        migrator
            .run_to(21, &pool)
            .await
            .expect("migrate up to 0021");

        for sql in [
            "INSERT INTO plc_connections (id, name, protocol, host, port, unit_id, enabled, \
             simulation, word_order, cpu_type) \
             VALUES (1, 'Line1', 'slmp', '192.168.1.10', 5007, 3, 0, 1, 'high_low', 'Q')",
            "INSERT INTO plc_connections (id, name, protocol, host, port, database, username, \
             password) VALUES (2, 'ERP', 'postgres', '10.0.0.50', 5432, 'erp', 'reader', 's')",
            "INSERT INTO plc_connections (id, name, protocol, host, port) \
             VALUES (3, 'Hub', 'hub', '', 0)",
            "INSERT INTO collection_groups (id, name, plc_connection_id, period_ms, enabled, \
             default_writable) VALUES (4, 'G1', 1, 2000, 0, 0)",
            "INSERT INTO collection_groups (id, name, plc_connection_id, period_ms, enabled, \
             default_writable, query_sql) VALUES (5, 'Q1', 2, 5000, 1, 1, 'SELECT 1 AS v')",
            "INSERT INTO collection_groups (id, name, plc_connection_id, period_ms) \
             VALUES (6, 'H', 3, 1000)",
            "INSERT INTO collection_groups (id, name, plc_connection_id, period_ms) \
             VALUES (7, 'Gone', 1, 1000)",
            "DELETE FROM collection_groups WHERE id = 7",
            "INSERT INTO tags (id, name, collection_group_id, address, data_type, string_length, \
             raw_lo, raw_hi, eng_lo, eng_hi, unit, decimals, enabled, writable, tag_kind, \
             expression, retain, revision, string_encoding) VALUES (11, 'T1', 4, 'D100', \
             'string', 8, 0, 100, 0, 50, 'degC', 2, 0, 1, 'plc', NULL, 1, 3, 'shift_jis')",
            "INSERT INTO tags (id, name, collection_group_id, address, data_type, tag_kind) \
             VALUES (12, 'V', 5, 'v', 'f64', 'db')",
            "INSERT INTO tags (id, name, collection_group_id, address, data_type) \
             VALUES (13, 't', 6, 'L1.fast.t', 'f64')",
        ] {
            sqlx::query(sqlx::AssertSqlSafe(sql))
                .execute(&pool)
                .await
                .unwrap_or_else(|e| panic!("seed failed ({sql}): {e}"));
        }
        let groups_before: Vec<String> = sqlx::query_scalar(GROUP_ROW_SQL)
            .fetch_all(&pool)
            .await
            .unwrap();
        let tags_before: Vec<String> = sqlx::query_scalar(TAG_ROW_SQL)
            .fetch_all(&pool)
            .await
            .unwrap();
        let sequences_before: Vec<(String, i64)> =
            sqlx::query_as("SELECT name, seq FROM sqlite_sequence ORDER BY name")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert!(sequences_before.contains(&("collection_groups".to_string(), 7)));
        assert_eq!(groups_before.len(), 3);
        assert_eq!(tags_before.len(), 3);

        // Up to 0022 only (not `crate::migrate`): the object list below is
        // 0022's world. Later migrations add their own objects (0024's push
        // triggers) and are checked by their own tests
        // (`plc_connection`'s `migration_0023_and_0024_*`).
        migrator.run_to(22, &pool).await.expect("0022 should apply");

        let groups_after: Vec<String> = sqlx::query_scalar(GROUP_ROW_SQL)
            .fetch_all(&pool)
            .await
            .unwrap();
        let tags_after: Vec<String> = sqlx::query_scalar(TAG_ROW_SQL)
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(groups_after, groups_before);
        assert_eq!(tags_after, tags_before);
        let sequences_after: Vec<(String, i64)> =
            sqlx::query_as("SELECT name, seq FROM sqlite_sequence ORDER BY name")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(sequences_after, sequences_before);

        // Column order is what `COLUMNS` / `SELECT *` consumers expect.
        let columns: Vec<String> =
            sqlx::query_scalar("SELECT name FROM pragma_table_info('collection_groups')")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            columns,
            [
                "id",
                "name",
                "plc_connection_id",
                "period_ms",
                "enabled",
                "default_writable",
                "query_sql"
            ]
        );

        let violations: Vec<(String,)> = sqlx::query_as("PRAGMA foreign_key_check")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert!(violations.is_empty(), "dangling FKs: {violations:?}");

        let objects: Vec<(String, String)> = sqlx::query_as(
            "SELECT type, name FROM sqlite_master \
             WHERE (type = 'index' AND name LIKE 'idx_%') OR type = 'trigger' ORDER BY type, name",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        let objects: Vec<(&str, &str)> = objects
            .iter()
            .map(|(t, n)| (t.as_str(), n.as_str()))
            .collect();
        assert_eq!(
            objects,
            vec![
                ("index", "idx_collection_groups_plc_connection_id"),
                ("index", "idx_plc_connections_single_hub"),
                ("index", "idx_tags_collection_group_id"),
                ("trigger", "tags_hub_external_name_unique_insert"),
                ("trigger", "tags_hub_external_name_unique_update"),
            ]
        );

        // The 0020 triggers still fire (insert and update).
        let err = sqlx::query(
            "INSERT INTO tags (name, collection_group_id, address, data_type) \
             VALUES ('t2', 6, 'L1.fast.t', 'f64')",
        )
        .execute(&pool)
        .await
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("banto_tags: hub external name already registered"),
            "{err}"
        );
        sqlx::query(
            "INSERT INTO tags (id, name, collection_group_id, address, data_type) \
             VALUES (14, 't3', 6, 'L1.fast.u', 'f64')",
        )
        .execute(&pool)
        .await
        .expect("a different external name is fine");
        let err = sqlx::query("UPDATE tags SET address = 'L1.fast.t' WHERE id = 14")
            .execute(&pool)
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("banto_tags: hub external name already registered"),
            "{err}"
        );

        // New uniqueness scope.
        sqlx::query(
            "INSERT INTO collection_groups (id, name, plc_connection_id, period_ms) \
             VALUES (NULL, 'G1', 3, 1000)",
        )
        .execute(&pool)
        .await
        .expect("same group name under another connection is accepted");
        let err = sqlx::query(
            "INSERT INTO collection_groups (name, plc_connection_id, period_ms) \
             VALUES ('G1', 1, 1000)",
        )
        .execute(&pool)
        .await
        .unwrap_err();
        assert!(
            err.as_database_error()
                .is_some_and(|e| e.is_unique_violation()),
            "{err}"
        );

        // FKs still restrict in both directions.
        assert!(
            sqlx::query(
                "INSERT INTO collection_groups (name, plc_connection_id, period_ms) \
                 VALUES ('orphan', 999, 1000)"
            )
            .execute(&pool)
            .await
            .is_err(),
            "group -> connection FK"
        );
        assert!(
            sqlx::query("DELETE FROM plc_connections WHERE id = 1")
                .execute(&pool)
                .await
                .is_err(),
            "a connection with groups cannot be deleted"
        );
        assert!(
            sqlx::query("DELETE FROM collection_groups WHERE id = 4")
                .execute(&pool)
                .await
                .is_err(),
            "a group with tags cannot be deleted (tags' FK points at the rebuilt table)"
        );
        assert!(
            sqlx::query(
                "INSERT INTO tags (name, collection_group_id, address, data_type) \
                 VALUES ('orphan', 999, 'D1', 'i16')"
            )
            .execute(&pool)
            .await
            .is_err(),
            "tag -> group FK"
        );

        // The deleted group's id 7 is not reused (the one inserted above got 8).
        let max_id: i64 = sqlx::query_scalar("SELECT MAX(id) FROM collection_groups")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(max_id, 8);
    }

    /// 0022 on a database with no groups at all: nothing to park or copy,
    /// and the result has the new scope.
    #[tokio::test]
    async fn migration_0022_applies_to_an_empty_registry() {
        let pool = banto_storage::connect_sqlite_memory()
            .await
            .expect("connect_sqlite_memory");
        let mut migrator = sqlx::migrate!("./migrations");
        migrator.dangerous_set_table_name(crate::MIGRATIONS_TABLE);
        migrator
            .run_to(21, &pool)
            .await
            .expect("migrate up to 0021");
        migrate(&pool).await.expect("0022 should apply");
        let sql: String = sqlx::query_scalar(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'collection_groups'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(sql.contains("UNIQUE (plc_connection_id, name)"), "{sql}");
        assert!(!sql.contains("name TEXT NOT NULL UNIQUE"), "{sql}");
    }
}
