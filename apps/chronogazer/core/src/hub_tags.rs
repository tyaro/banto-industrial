//! 「Hub から追加」（#383 段階3 P4、2026-10-11 オーナー決定）: Hub の
//! カタログから選んだタグを、レジストリ（`banto-tags`）の hub 接続の下の
//! タグとして登録する。REST（`crate::rest` の `POST /api/tags/from-hub`）と
//! Tauri（`apps/chronogazer/src-tauri/src/lib.rs` の `tags_register_from_hub`）の
//! **両方がこのモジュールの [`register_from_hub`] を呼ぶ**（経路で規則が
//! 割れないように）。
//!
//! # 登録の規則（H1・H3・H5 と P4 の決定）
//!
//! * **定義は登録時にカタログから複製する**（H3）: 名前 = Hub のタグ名
//!   （`name`。外部名の末尾）、データ型・単位・小数桁 = Hub のタグのもの、
//!   アドレス = **外部名**（同一性は外部名、scada-design.md §9.6）、書き込み
//!   不可（H5）、スケーリングなし（Hub は工学値を送る。`banto-tags` も hub
//!   タグのスケーリングを断る）。
//! * **数値と bit だけ**: 文字列・構造体などは登録しない（記録計は数値だけを
//!   扱う）。ダイアログは選べなくし、ここでも行のエラーで断る。
//! * **名前が衝突したら黙って改名しない**: 登録先のグループに同じ名前の
//!   タグがある・リクエスト内で同じ名前が重なる、は**その行の**検証エラーに
//!   する（`banto-tags` の `create_batch_tx` の規則をそのまま返す）。
//! * **登録済みの外部名は登録しない**（同じ Hub のタグを 2 本のタグにしない）。
//!   ダイアログは「登録済み」として選べなくし、ここでも行のエラーで断る。
//! * **1 回の登録は全部か無しか**: 1 行でもエラーがあれば何も書かない
//!   （接続・グループの暗黙の作成も含めて巻き戻す）。エラーは**全部の行の分を
//!   まとめて**返す（`index` = リクエストの `tags` の位置）。
//! * **Hub 接続は暗黙に作る**: hub 接続（`protocol = hub`、Hub は 1 つ・
//!   接続先とキーは `hub.record`）がまだ無ければ、新しいグループを作る登録の
//!   ときに名前 [`HUB_CONNECTION_NAME`]（使われていれば「Hub 2」…）で作る。
//!   既存のグループを選ぶ登録は、そのグループが hub 接続の下にあることを
//!   確かめる。
//! * **Hub には問い合わせない**: 行（外部名・名前・型・単位・小数桁）は
//!   ダイアログが読んだカタログをそのまま送る。食い違い（その後 Hub で消えた・
//!   型が変わった）は収集の開始時の突き合わせが外す（P3、#414 と同じ）。
//!
//! # トランザクション
//!
//! 読み取り（登録済みの外部名・hub 接続・グループの所属）の後に書くので
//! `BEGIN IMMEDIATE` で始める（docs/implementation-checklist.md §5）。
//!
//! # 応答
//!
//! banto-hub の `POST /api/tags/batch` と同じく、検証の不合格は**例外では
//! なく通常の応答**（`ok: false` + 行ごとのエラー）。認証・権限・DB の想定外の
//! 失敗だけが `Err` になる。

use std::collections::{HashMap, HashSet};

use banto_core::{BantoError, FieldError};
use banto_tags::{
    BatchTagOutcome, CollectionGroup, CollectionGroupInput, CollectionGroupService,
    PlcConnectionInput, PlcConnectionService, Tag, TagInput, TagService, HUB_PROTOCOL,
    NUMERIC_DATA_TYPES, PLC_TAG_KIND,
};
use serde::{Deserialize, Serialize};
use sqlx::{SqliteConnection, SqlitePool};

/// 暗黙に作る hub 接続の名前（使われていれば「Hub 2」「Hub 3」…）。
pub const HUB_CONNECTION_NAME: &str = "Hub";

/// 小数桁の範囲（`banto_tags::tag` の `MIN_DECIMALS`/`MAX_DECIMALS` と同じ）。
/// Hub のタグの小数桁がこれを外れていたら丸める（Hub 側の値で登録を
/// 失敗させない）。
const MIN_DECIMALS: i64 = 0;
const MAX_DECIMALS: i64 = 6;

/// 登録済みの外部名を読む SQL: hub 接続の下の**すべての**タグの `address`
/// （有効・無効を問わない - 購読の対象と「登録済み」の判定はどちらも
/// 有効フラグに依らない）。
const REGISTERED_SQL: &str = "SELECT t.address FROM tags t \
     JOIN collection_groups g ON g.id = t.collection_group_id \
     JOIN plc_connections c ON c.id = g.plc_connection_id \
     WHERE c.protocol = ? ORDER BY t.address";

/// レジストリに登録済みの hub タグの外部名（前後の空白を除き、空を捨て、
/// ソート・重複なし）。`crate::hub::HubService` の購読の対象と、ダイアログの
/// 「登録済み」の判定に使う。
pub async fn registered_external_names(pool: &SqlitePool) -> Result<Vec<String>, BantoError> {
    let rows: Vec<String> = sqlx::query_scalar(REGISTERED_SQL)
        .bind(HUB_PROTOCOL)
        .fetch_all(pool)
        .await
        .map_err(banto_storage::storage_error)?;
    Ok(normalize(rows))
}

async fn registered_external_names_tx(
    connection: &mut SqliteConnection,
) -> Result<HashSet<String>, BantoError> {
    let rows: Vec<String> = sqlx::query_scalar(REGISTERED_SQL)
        .bind(HUB_PROTOCOL)
        .fetch_all(&mut *connection)
        .await
        .map_err(banto_storage::storage_error)?;
    Ok(normalize(rows).into_iter().collect())
}

fn normalize(names: Vec<String>) -> Vec<String> {
    let mut names: Vec<String> = names
        .into_iter()
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .collect();
    names.sort();
    names.dedup();
    names
}

/// Hub のデータ型のうち、登録できるもの（数値と bit。`banto_tags` の
/// `NUMERIC_DATA_TYPES`）。**純関数** - ダイアログの選べる/選べないと同じ規則
/// （`apps/chronogazer/src/lib/banto/hubRegistration.ts` の
/// `isRegistrableDataType`）。
pub fn is_registrable_data_type(data_type: &str) -> bool {
    NUMERIC_DATA_TYPES.contains(&data_type)
}

/// 登録する 1 行（ダイアログが読んだカタログの行）。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HubTagRow {
    pub external_name: String,
    pub name: String,
    pub data_type: String,
    #[serde(default)]
    pub unit: Option<String>,
    #[serde(default)]
    pub decimals: Option<i64>,
}

/// 登録先に新しく作る収集グループ（hub 接続の下）。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewHubGroup {
    pub name: String,
    pub period_ms: i64,
}

/// `POST /api/tags/from-hub` / `tags_register_from_hub` の入力。登録先は
/// `collection_group_id`（hub 接続の下の既存グループ）か `new_group` の
/// **どちらか 1 つ**。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HubTagRegistration {
    #[serde(default)]
    pub collection_group_id: Option<i64>,
    #[serde(default)]
    pub new_group: Option<NewHubGroup>,
    pub tags: Vec<HubTagRow>,
}

/// 1 行分のエラー（`index` = リクエストの `tags` の位置）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HubRowError {
    pub index: usize,
    pub external_name: String,
    pub field_errors: Vec<FieldError>,
}

/// 登録の結果。`ok: false` のときは何も書いていない。
///
/// `field_errors` は行に属さないエラー（登録先の指定・新しいグループの
/// 名前 `newGroupName` / 周期 `newGroupPeriodMs`）、`row_errors` は行ごと。
/// `ok: true` のときだけ `collection_group` / `tags` が入る。接続の行は
/// 返さない（`PlcConnection` は平文の `password` 列を持つため。ID と
/// 作ったかどうかだけ返す）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HubRegistrationResult {
    pub ok: bool,
    pub field_errors: Vec<FieldError>,
    pub row_errors: Vec<HubRowError>,
    pub connection_id: Option<i64>,
    pub connection_created: bool,
    pub collection_group: Option<CollectionGroup>,
    pub group_created: bool,
    pub tags: Vec<Tag>,
    /// 移行前の選択（`hub.record.selected_tags`）を消したか。消すのは
    /// 呼び出し側（REST / Tauri）で、登録が成功した後（[`register_from_hub`]
    /// 自体は `false` のまま返す）。
    pub legacy_selection_cleared: bool,
}

impl HubRegistrationResult {
    fn rejected(field_errors: Vec<FieldError>, row_errors: Vec<HubRowError>) -> Self {
        Self {
            ok: false,
            field_errors,
            row_errors,
            connection_id: None,
            connection_created: false,
            collection_group: None,
            group_created: false,
            tags: Vec::new(),
            legacy_selection_cleared: false,
        }
    }
}

fn field_error(field: &str, message: impl Into<String>) -> FieldError {
    FieldError {
        field: field.to_owned(),
        message: message.into(),
    }
}

/// 行の事前検証（**純関数**）: 外部名が空・リクエスト内で重複・登録済み、
/// データ型が数値と bit 以外。戻りは行と同じ長さで、各行のエラー（無ければ
/// 空）。名前の衝突は `banto-tags` の `create_batch_tx` が判定する。
pub fn prevalidate_rows(rows: &[HubTagRow], registered: &HashSet<String>) -> Vec<Vec<FieldError>> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for row in rows {
        *counts.entry(row.external_name.trim()).or_default() += 1;
    }
    rows.iter()
        .map(|row| {
            let mut errors = Vec::new();
            let external_name = row.external_name.trim();
            if external_name.is_empty() {
                errors.push(field_error(
                    "externalName",
                    "Hub のタグ名（外部名）が空です",
                ));
            } else if registered.contains(external_name) {
                errors.push(field_error(
                    "externalName",
                    format!("Hub のタグ {external_name} は既に登録されています"),
                ));
            } else if counts.get(external_name).copied().unwrap_or(0) > 1 {
                errors.push(field_error(
                    "externalName",
                    format!("Hub のタグ {external_name} がリクエスト内で重複しています"),
                ));
            }
            if !is_registrable_data_type(&row.data_type) {
                errors.push(field_error(
                    "dataType",
                    format!(
                        "Hub の {} 型のタグは登録できません（記録できるのは数値と bit だけです）",
                        row.data_type
                    ),
                ));
            }
            errors
        })
        .collect()
}

/// 1 行をタグの入力にする（**純関数**）。モジュール doc「登録の規則」。
pub fn tag_input_for(row: &HubTagRow, collection_group_id: i64) -> TagInput {
    TagInput {
        name: row.name.clone(),
        collection_group_id,
        address: row.external_name.trim().to_owned(),
        data_type: row.data_type.clone(),
        string_length: None,
        string_encoding: "utf8".to_owned(),
        raw_lo: None,
        raw_hi: None,
        eng_lo: None,
        eng_hi: None,
        unit: row.unit.clone(),
        decimals: row
            .decimals
            .unwrap_or(MIN_DECIMALS)
            .clamp(MIN_DECIMALS, MAX_DECIMALS),
        enabled: true,
        writable: false,
        tag_kind: PLC_TAG_KIND.to_owned(),
        expression: None,
        retain: false,
        expected_revision: None,
    }
}

/// 新しいグループの検証エラーのフィールド名を、ダイアログの欄の名前に
/// 載せ替える（`name` → `newGroupName`、`periodMs` → `newGroupPeriodMs`）。
fn new_group_field(field: &str) -> String {
    match field {
        "name" => "newGroupName".to_owned(),
        "periodMs" => "newGroupPeriodMs".to_owned(),
        other => other.to_owned(),
    }
}

async fn hub_connection_id(connection: &mut SqliteConnection) -> Result<Option<i64>, BantoError> {
    sqlx::query_scalar("SELECT id FROM plc_connections WHERE protocol = ? ORDER BY id LIMIT 1")
        .bind(HUB_PROTOCOL)
        .fetch_optional(&mut *connection)
        .await
        .map_err(banto_storage::storage_error)
}

/// 暗黙に作る hub 接続の名前: [`HUB_CONNECTION_NAME`]、使われていれば
/// 「Hub 2」「Hub 3」…（接続名は全体で一意）。
async fn free_hub_connection_name(connection: &mut SqliteConnection) -> Result<String, BantoError> {
    for n in 1..=100 {
        let candidate = if n == 1 {
            HUB_CONNECTION_NAME.to_owned()
        } else {
            format!("{HUB_CONNECTION_NAME} {n}")
        };
        let taken: Option<i64> =
            sqlx::query_scalar("SELECT id FROM plc_connections WHERE name = ?")
                .bind(&candidate)
                .fetch_optional(&mut *connection)
                .await
                .map_err(banto_storage::storage_error)?;
        if taken.is_none() {
            return Ok(candidate);
        }
    }
    Err(BantoError::Validation {
        field_errors: vec![field_error(
            "newGroupName",
            "Hub 接続の名前を決められませんでした（「Hub」「Hub 2」…がすべて使われています）",
        )],
    })
}

/// 「Hub から追加」の本体（モジュール doc）。
pub async fn register_from_hub(
    pool: &SqlitePool,
    plc_connections: &PlcConnectionService,
    collection_groups: &CollectionGroupService,
    tags: &TagService,
    registration: HubTagRegistration,
) -> Result<HubRegistrationResult, BantoError> {
    if registration.tags.is_empty() {
        return Ok(HubRegistrationResult::rejected(
            vec![field_error(
                "tags",
                "登録する Hub のタグを 1 つ以上選んでください",
            )],
            Vec::new(),
        ));
    }
    let target_count = usize::from(registration.collection_group_id.is_some())
        + usize::from(registration.new_group.is_some());
    if target_count != 1 {
        return Ok(HubRegistrationResult::rejected(
            vec![field_error(
                "collectionGroupId",
                "登録先の収集グループを選ぶか、新しいグループを作ってください（どちらか 1 つ）",
            )],
            Vec::new(),
        ));
    }

    let mut tx = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(banto_storage::storage_error)?;
    let outcome = register_in_tx(
        &mut tx,
        plc_connections,
        collection_groups,
        tags,
        &registration,
    )
    .await;
    match outcome {
        Ok(result) if result.ok => {
            tx.commit().await.map_err(banto_storage::storage_error)?;
            Ok(result)
        }
        Ok(result) => {
            let _ = tx.rollback().await;
            Ok(result)
        }
        Err(err) => {
            let _ = tx.rollback().await;
            Err(err)
        }
    }
}

async fn register_in_tx(
    connection: &mut SqliteConnection,
    plc_connections: &PlcConnectionService,
    collection_groups: &CollectionGroupService,
    tags: &TagService,
    registration: &HubTagRegistration,
) -> Result<HubRegistrationResult, BantoError> {
    let registered = registered_external_names_tx(connection).await?;
    let mut row_errors = prevalidate_rows(&registration.tags, &registered);
    let existing_hub = hub_connection_id(connection).await?;

    let mut connection_created = false;
    let mut group_created = false;
    let (connection_id, group) = match (&registration.collection_group_id, &registration.new_group)
    {
        (Some(group_id), _) => {
            let found: Option<(i64,)> =
                sqlx::query_as("SELECT plc_connection_id FROM collection_groups WHERE id = ?")
                    .bind(group_id)
                    .fetch_optional(&mut *connection)
                    .await
                    .map_err(banto_storage::storage_error)?;
            match (found, existing_hub) {
                (Some((conn_id,)), Some(hub_id)) if conn_id == hub_id => {
                    let group = sqlx::query_as::<_, CollectionGroup>(
                        "SELECT id, name, plc_connection_id, period_ms, enabled, default_writable, \
                         query_sql FROM collection_groups WHERE id = ?",
                    )
                    .bind(group_id)
                    .fetch_one(&mut *connection)
                    .await
                    .map_err(banto_storage::storage_error)?;
                    (hub_id, group)
                }
                _ => {
                    return Ok(HubRegistrationResult::rejected(
                        vec![field_error(
                            "collectionGroupId",
                            "登録先には Hub 接続の収集グループを選んでください",
                        )],
                        Vec::new(),
                    ))
                }
            }
        }
        (None, Some(new_group)) => {
            let hub_id = match existing_hub {
                Some(id) => id,
                None => {
                    let name = free_hub_connection_name(connection).await?;
                    let created = plc_connections
                        .create_tx(
                            connection,
                            PlcConnectionInput {
                                name,
                                protocol: HUB_PROTOCOL.to_owned(),
                                host: String::new(),
                                port: 0,
                                unit_id: 1,
                                enabled: true,
                                simulation: false,
                                word_order: String::new(),
                                database: None,
                                username: None,
                                password: None,
                            },
                        )
                        .await?;
                    connection_created = true;
                    created.id
                }
            };
            let created = collection_groups
                .create_tx(
                    connection,
                    CollectionGroupInput {
                        name: new_group.name.clone(),
                        plc_connection_id: hub_id,
                        period_ms: new_group.period_ms,
                        enabled: true,
                        // Hub 経由のタグは書き込み不可（H5）。
                        default_writable: false,
                        query_sql: None,
                    },
                )
                .await;
            match created {
                Ok(group) => {
                    group_created = true;
                    (hub_id, group)
                }
                Err(BantoError::Validation { field_errors }) => {
                    let field_errors = field_errors
                        .into_iter()
                        .map(|error| FieldError {
                            field: new_group_field(&error.field),
                            message: error.message,
                        })
                        .collect();
                    // 行の事前検証の結果も一緒に返す（直せるものを 1 回で全部
                    // 見せる）。名前の衝突はグループが無いと判定できない。
                    return Ok(HubRegistrationResult::rejected(
                        field_errors,
                        row_errors_of(&registration.tags, row_errors),
                    ));
                }
                Err(other) => return Err(other),
            }
        }
        (None, None) => unreachable!("register_from_hub checks the target first"),
    };

    let inputs: Vec<TagInput> = registration
        .tags
        .iter()
        .map(|row| tag_input_for(row, group.id))
        .collect();
    match tags.create_batch_tx(connection, &inputs).await? {
        BatchTagOutcome::Invalid(errors) => {
            for error in errors {
                if let Some(slot) = row_errors.get_mut(error.index) {
                    for field_error in error.field_errors {
                        // 事前検証と同じ指摘を二重に出さない（型の不正は両方が言う）。
                        if !slot
                            .iter()
                            .any(|existing| existing.field == field_error.field)
                        {
                            slot.push(field_error);
                        }
                    }
                }
            }
            Ok(HubRegistrationResult::rejected(
                Vec::new(),
                row_errors_of(&registration.tags, row_errors),
            ))
        }
        BatchTagOutcome::Valid { tags: created, .. } => {
            let row_errors = row_errors_of(&registration.tags, row_errors);
            if !row_errors.is_empty() {
                // 事前検証で落ちた行がある（登録済み・重複など）。バッチ自体は
                // 通っても全部か無しかなので書かない（呼び出し側が巻き戻す）。
                return Ok(HubRegistrationResult::rejected(Vec::new(), row_errors));
            }
            Ok(HubRegistrationResult {
                ok: true,
                field_errors: Vec::new(),
                row_errors: Vec::new(),
                connection_id: Some(connection_id),
                connection_created,
                collection_group: Some(group),
                group_created,
                tags: created.unwrap_or_default(),
                legacy_selection_cleared: false,
            })
        }
    }
}

fn row_errors_of(rows: &[HubTagRow], errors: Vec<Vec<FieldError>>) -> Vec<HubRowError> {
    errors
        .into_iter()
        .enumerate()
        .filter(|(_, field_errors)| !field_errors.is_empty())
        .map(|(index, field_errors)| HubRowError {
            index,
            external_name: rows
                .get(index)
                .map(|row| row.external_name.clone())
                .unwrap_or_default(),
            field_errors,
        })
        .collect()
}

/// 監査の `detail`（REST と Tauri で同じもの）。
pub fn registration_audit_detail(result: &HubRegistrationResult) -> serde_json::Value {
    serde_json::json!({
        "source": "hub",
        "count": result.tags.len(),
        "collectionGroupId": result.collection_group.as_ref().map(|group| group.id),
        "groupCreated": result.group_created,
        "connectionId": result.connection_id,
        "connectionCreated": result.connection_created,
        "externalNames": result.tags.iter().map(|tag| tag.address.clone()).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::init_db_memory;

    struct Registry {
        pool: SqlitePool,
        connections: PlcConnectionService,
        groups: CollectionGroupService,
        tags: TagService,
    }

    async fn new_registry() -> Registry {
        let pool = init_db_memory().await.expect("init_db_memory");
        Registry {
            connections: PlcConnectionService::new(pool.clone()),
            groups: CollectionGroupService::new(pool.clone()),
            tags: TagService::new(pool.clone()),
            pool,
        }
    }

    fn row(external_name: &str, name: &str, data_type: &str) -> HubTagRow {
        HubTagRow {
            external_name: external_name.to_owned(),
            name: name.to_owned(),
            data_type: data_type.to_owned(),
            unit: Some("℃".to_owned()),
            decimals: Some(2),
        }
    }

    fn new_group(name: &str) -> Option<NewHubGroup> {
        Some(NewHubGroup {
            name: name.to_owned(),
            period_ms: 1000,
        })
    }

    async fn register(
        registry: &Registry,
        collection_group_id: Option<i64>,
        new_group: Option<NewHubGroup>,
        rows: Vec<HubTagRow>,
    ) -> HubRegistrationResult {
        register_from_hub(
            &registry.pool,
            &registry.connections,
            &registry.groups,
            &registry.tags,
            HubTagRegistration {
                collection_group_id,
                new_group,
                tags: rows,
            },
        )
        .await
        .expect("register_from_hub")
    }

    #[test]
    fn registrable_data_types_are_numeric_and_bit_only() {
        for data_type in [
            "bit", "i16", "u16", "i32", "u32", "f32", "i64", "u64", "f64",
        ] {
            assert!(is_registrable_data_type(data_type), "{data_type}");
        }
        for data_type in ["string", "struct", "", "F32"] {
            assert!(!is_registrable_data_type(data_type), "{data_type}");
        }
    }

    #[test]
    fn prevalidate_rows_reports_each_row_by_reason() {
        let registered: HashSet<String> = ["L.G.done".to_owned()].into_iter().collect();
        let rows = vec![
            row("L.G.a", "a", "f32"),
            row("L.G.done", "done", "f32"),
            row("L.G.dup", "dup1", "i16"),
            row("L.G.dup", "dup2", "i16"),
            row("L.G.text", "text", "string"),
            row("  ", "blank", "bit"),
        ];
        let errors = prevalidate_rows(&rows, &registered);
        let fields: Vec<Vec<&str>> = errors
            .iter()
            .map(|row| row.iter().map(|e| e.field.as_str()).collect())
            .collect();
        assert_eq!(
            fields,
            vec![
                vec![],
                vec!["externalName"],
                vec!["externalName"],
                vec!["externalName"],
                vec!["dataType"],
                vec!["externalName"],
            ]
        );
        assert!(errors[1][0].message.contains("既に登録"));
        assert!(errors[2][0].message.contains("重複"));
    }

    #[test]
    fn a_row_becomes_a_read_only_unscaled_tag_addressed_by_external_name() {
        let mut source = row(" Line1.Fast.Temp01 ", "Temp01", "f64");
        source.decimals = Some(9);
        let input = tag_input_for(&source, 7);
        assert_eq!(input.address, "Line1.Fast.Temp01");
        assert_eq!(input.name, "Temp01");
        assert_eq!(input.data_type, "f64");
        assert_eq!(input.unit.as_deref(), Some("℃"));
        assert_eq!(input.decimals, MAX_DECIMALS, "Hub の小数桁は範囲に丸める");
        assert!(!input.writable);
        assert_eq!(
            (input.raw_lo, input.raw_hi, input.eng_lo, input.eng_hi),
            (None, None, None, None)
        );
        assert_eq!(input.tag_kind, PLC_TAG_KIND);
        assert_eq!(input.collection_group_id, 7);
    }

    /// 初めての登録: hub 接続とグループを暗黙に作り、行をタグにする。
    /// 登録済みの外部名として読み出せる。
    #[tokio::test]
    async fn the_first_registration_creates_the_hub_connection_and_the_group() {
        let registry = new_registry().await;
        let result = register(
            &registry,
            None,
            new_group("hub-fast"),
            vec![row("L.G.a", "a", "f32"), row("L.G.b", "b", "bit")],
        )
        .await;
        assert!(result.ok, "{result:?}");
        assert!(result.connection_created);
        assert!(result.group_created);
        assert_eq!(result.tags.len(), 2);
        let conn = registry
            .connections
            .get(result.connection_id.unwrap())
            .await
            .unwrap();
        assert_eq!(conn.protocol, HUB_PROTOCOL);
        assert_eq!(conn.name, HUB_CONNECTION_NAME);
        assert_eq!(
            registered_external_names(&registry.pool).await.unwrap(),
            vec!["L.G.a".to_owned(), "L.G.b".to_owned()]
        );

        // 2 回目は同じ hub 接続の下に既存のグループを選んで足す。
        let group_id = result.collection_group.unwrap().id;
        let second = register(
            &registry,
            Some(group_id),
            None,
            vec![row("L.G.c", "c", "i16")],
        )
        .await;
        assert!(second.ok, "{second:?}");
        assert!(!second.connection_created);
        assert!(!second.group_created);
        assert_eq!(second.connection_id, result.connection_id);
    }

    /// 名前の衝突・登録済みは行ごとのエラーで返し、何も書かない（暗黙に
    /// 作るはずの接続・グループも残らない）。黙って改名しない。
    #[tokio::test]
    async fn collisions_are_per_row_errors_and_nothing_is_written() {
        let registry = new_registry().await;
        let first = register(
            &registry,
            None,
            new_group("g1"),
            vec![row("L.G.a", "a", "f32")],
        )
        .await;
        assert!(first.ok);
        let group_id = first.collection_group.unwrap().id;

        let result = register(
            &registry,
            Some(group_id),
            None,
            vec![
                row("L2.G.a", "a", "f32"),    // 名前がグループ内で衝突
                row("L.G.a", "again", "f32"), // 登録済み
                row("L.G.ok", "ok", "f32"),
            ],
        )
        .await;
        assert!(!result.ok);
        let by_index: Vec<(usize, Vec<&str>)> = result
            .row_errors
            .iter()
            .map(|e| {
                (
                    e.index,
                    e.field_errors.iter().map(|f| f.field.as_str()).collect(),
                )
            })
            .collect();
        assert_eq!(by_index, vec![(0, vec!["name"]), (1, vec!["externalName"])]);
        assert_eq!(result.row_errors[0].external_name, "L2.G.a");
        // 何も書いていない（ok の行も）。
        assert_eq!(
            registered_external_names(&registry.pool).await.unwrap(),
            vec!["L.G.a".to_owned()]
        );

        // 新しいグループでの失敗は、暗黙に作った接続ごと巻き戻す。
        let fresh = new_registry().await;
        let rejected = register(
            &fresh,
            None,
            new_group("g"),
            vec![row("L.G.x", "same", "f32"), row("L.G.y", "same", "f32")],
        )
        .await;
        assert!(!rejected.ok);
        assert_eq!(rejected.row_errors.len(), 2);
        let connections = fresh
            .connections
            .list(banto_core::ListParams::default())
            .await
            .unwrap();
        assert!(connections.rows.is_empty(), "暗黙の接続も残さない");
    }

    /// 登録先の指定の誤り: PLC 接続のグループは選べない・どちらも無い／両方。
    #[tokio::test]
    async fn the_target_must_be_a_hub_group_or_a_new_group() {
        let registry = new_registry().await;
        let plc = registry
            .connections
            .create(PlcConnectionInput {
                name: "plc".to_owned(),
                protocol: "slmp".to_owned(),
                host: "127.0.0.1".to_owned(),
                port: 5000,
                unit_id: 1,
                enabled: true,
                simulation: false,
                word_order: String::new(),
                database: None,
                username: None,
                password: None,
            })
            .await
            .unwrap();
        let plc_group = registry
            .groups
            .create(CollectionGroupInput {
                name: "plc-g".to_owned(),
                plc_connection_id: plc.id,
                period_ms: 1000,
                enabled: true,
                default_writable: false,
                query_sql: None,
            })
            .await
            .unwrap();
        let result = register(
            &registry,
            Some(plc_group.id),
            None,
            vec![row("L.G.a", "a", "f32")],
        )
        .await;
        assert!(!result.ok);
        assert_eq!(result.field_errors[0].field, "collectionGroupId");

        let neither = register(&registry, None, None, vec![row("L.G.a", "a", "f32")]).await;
        assert_eq!(neither.field_errors[0].field, "collectionGroupId");
        let both = register(
            &registry,
            Some(plc_group.id),
            new_group("x"),
            vec![row("L.G.a", "a", "f32")],
        )
        .await;
        assert_eq!(both.field_errors[0].field, "collectionGroupId");
        let empty = register(&registry, None, new_group("x"), Vec::new()).await;
        assert_eq!(empty.field_errors[0].field, "tags");
    }

    /// 新しいグループの検証エラーはダイアログの欄の名前で返す。名前「Hub」の
    /// PLC 接続があっても、暗黙の hub 接続は「Hub 2」で作る。
    #[tokio::test]
    async fn new_group_errors_use_dialog_field_names_and_the_hub_name_avoids_collisions() {
        let registry = new_registry().await;
        registry
            .connections
            .create(PlcConnectionInput {
                name: HUB_CONNECTION_NAME.to_owned(),
                protocol: "slmp".to_owned(),
                host: "127.0.0.1".to_owned(),
                port: 5000,
                unit_id: 1,
                enabled: true,
                simulation: false,
                word_order: String::new(),
                database: None,
                username: None,
                password: None,
            })
            .await
            .unwrap();
        let bad_period = register_from_hub(
            &registry.pool,
            &registry.connections,
            &registry.groups,
            &registry.tags,
            HubTagRegistration {
                collection_group_id: None,
                new_group: Some(NewHubGroup {
                    name: "g".to_owned(),
                    period_ms: 1234,
                }),
                tags: vec![row("L.G.a", "a", "f32")],
            },
        )
        .await
        .unwrap();
        assert!(!bad_period.ok);
        assert_eq!(bad_period.field_errors[0].field, "newGroupPeriodMs");

        let ok = register(
            &registry,
            None,
            new_group("g"),
            vec![row("L.G.a", "a", "f32")],
        )
        .await;
        assert!(ok.ok, "{ok:?}");
        let conn = registry
            .connections
            .get(ok.connection_id.unwrap())
            .await
            .unwrap();
        assert_eq!(conn.name, "Hub 2");
    }
}
