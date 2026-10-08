//! 表示グループ（#393 = #524 の段階 1、recorder-requirements.md §2・§3.2・
//! §3.7.3 (1)・§3.7.8・§3.7.9）。
//!
//! 表示グループは**表示の単位**で、収集グループ（banto-tags の
//! `collection_groups`、PLC 一括読み出しの単位）とは別物。テーブルは app の
//! `migrations-sqlite/0101_display_groups.sql`（ChronoGazer 固有）。
//!
//! ## データの形（JSON で往復できる宣言データ）
//!
//! ```json
//! {
//!   "id": 1, "revision": 3,
//!   "name": "ライン1", "sortOrder": 0, "kind": "trend",
//!   "attributes": { "timeWindowSec": 600 },
//!   "pens": [ { "tagId": 12, "colorSlot": null }, { "tagId": 13, "colorSlot": 4 } ]
//! }
//! ```
//!
//! 読み取りの応答（[`DisplayGroup`]）をそのまま作成・更新の本文
//! （[`DisplayGroupPayload`]）に送り返せる（`id`/`revision` は本文では無視され、
//! 更新の版は `expectedRevision` で渡す）。段階 3 の関所（検証 → プレビュー →
//! 承認 → 保存）と段階 5 の AI / MCP は、この形のまま同じ検証を通す。
//!
//! - `kind`: `trend` / `digital` / `bar` / `gauge` の 4 種から 1 つ（§3.2 の固定
//!   パターン。種別はデータで増やせない - §3.7.7）。
//! - `attributes`: 種別ごとの**閉じた項目の集合**（§3.7.3 (1)）。今の語彙は
//!   トレンドの既定時間窓 `timeWindowSec`（[`TREND_TIME_WINDOWS_SEC`] から 1 つ、
//!   既定 600 = R1-D の「既定窓 10 分・選択可」）だけで、デジタル・バー・計器は
//!   空。種別に無い項目は検証で拒否する（黙って捨てない）。項目を足すのは
//!   「語彙を広げる」ことで、要件の改訂（オーナー決定）として行う。
//! - `pens`: 最大 [`MAX_PENS_PER_GROUP`]。各ペンはタグへの参照（ID）と表示属性。
//!   表示属性は今は色だけで、`colorSlot` = banto チャートの系列色の枠
//!   （1..=8、`--banto-chart-N`）。`null` は既定（ペンの位置 + 1 の枠、
//!   `@banto/charts` の `seriesColorVar(index)` と同じ割り当て）。任意の色コード
//!   にしないのは、テーマ（ライト / ダーク）に追従させるため。
//! - **しきい値は持たない**。しきい値はタグ定義の属性（§3.7.1、2026-10-08
//!   オーナー決定）で、表示グループからは参照するだけ（画面はタグの値を
//!   読み取り専用で見せる）。
//! - `sortOrder`: 一覧の並び順（昇順、同じ値は `id` 順。0〜[`MAX_SORT_ORDER`]、
//!   DB の CHECK でも縛る）。作成で省略すると末尾（溢れるなら振り直す）。
//!   **更新（PUT）の本文の `sortOrder` は無視する** - 並びを変える口は
//!   [`DisplayGroupService::reorder`]（`PUT /api/display-groups/order`）だけ。
//!   並べ替えは版（`revision`）を進めない（別の人の内容の編集を、並べ替えだけで
//!   衝突させないため）ので、更新が前に読んだ `sortOrder` を書き戻すと並べ替えを
//!   黙って取り消してしまう（2026-10-08 Codex レビュー P2-3）。
//!
//! ## 検証は 1 関数（§3.7.4）
//!
//! [`validate_display_group`] が経路（REST・Tauri、のちの関所・MCP）を問わず
//! 唯一の検証関数。形（[`validate_shape`]: 名前・種別・種別ごとの属性・ペンの数・
//! 色・同じタグの重複）と、DB を見る部分（グループ数の上限
//! [`MAX_DISPLAY_GROUPS`]・名前の重複・参照するタグの存在）をまとめて確かめ、
//! 見つかった誤りを `field_errors` に全部並べて返す（1 件目で止めない）。
//! 作成・更新は `BEGIN IMMEDIATE` のトランザクションの中で「検証 → 書き込み」を
//! 行うので、検証と保存の間に別の書き込みが割り込まない（上限・名前・タグの
//! 存在が保存の時点でも成り立つ）。
//!
//! ## 参照されているタグの削除（§3.7.9 の 8）
//!
//! ペンはタグを ID で参照する（改名は通る）。ペンに割り当てられたタグの削除は、
//! 参照元のグループ名を並べて拒否する - [`DisplayGroupService::delete_tag_unless_referenced`]
//! を REST の `DELETE /api/tags/{id}` と Tauri の `tags_delete` の両方が呼ぶ。
//! `tags` への FK は張らない（migration の注記: banto-tags は `tags` を作り直す
//! ことがある）ので、`BEGIN IMMEDIATE` の中で「参照元の確認 → 削除」を行い、
//! 確認と削除の間にペンが足されないようにしている。
//!
//! ## 楽観ロック
//!
//! 更新は `expectedRevision` を受け取り、保存されている版と違えば
//! `{kind:"validation", field_errors:[{field:"expectedRevision", ...}]}` で拒否する
//! （[`revision_conflict_error`]。REST は `409`、Tauri は同じ形の検証エラー）。
//! タグの楽観ロック（#525）と同じ形にしてある。省略した更新は版を確かめない
//! （banto-tags の `expected_revision` と同じ扱い。画面は常に送る）。

use banto_core::{BantoError, FieldError};
use banto_tags::TagService;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sqlx::{SqliteConnection, SqlitePool};
use std::collections::{BTreeMap, HashSet};

/// 監査の `resource`（REST・Tauri 共通）。
pub const AUDIT_RESOURCE: &str = "display_groups";

/// グループ数の上限（§3.2「グループ数上限（v1 目標）: 16」）。
pub const MAX_DISPLAY_GROUPS: usize = 16;

/// 1 グループのペンの上限（§2「最大 8 ペン」）。
pub const MAX_PENS_PER_GROUP: usize = 8;

/// 名前の最大文字数（タグ設定の名前欄と同じ 100）。
pub const MAX_NAME_CHARS: usize = 100;

/// `sortOrder` の上限（並べ替えは 0 から詰めるので、16 グループには十分）。
pub const MAX_SORT_ORDER: i64 = 9999;

/// 色の枠の数（`@banto/charts` の `MAX_CHART_SERIES`）。
pub const COLOR_SLOTS: i64 = 8;

/// トレンドの既定時間窓の選択肢（秒）。1 分・5 分・10 分・30 分・1 時間。
pub const TREND_TIME_WINDOWS_SEC: [i64; 5] = [60, 300, 600, 1800, 3600];

/// トレンドの既定時間窓の既定値（R1-D「既定窓 10 分」）。
pub const DEFAULT_TREND_TIME_WINDOW_SEC: i64 = 600;

/// 楽観ロックの食い違いの表し方はタグ（#525）と共通（`crate::revision`）。
pub use crate::revision::{is_revision_conflict, REVISION_CONFLICT_FIELD};

/// 版の食い違いの案内（画面にそのまま出る）。
pub const REVISION_CONFLICT_MESSAGE: &str = "他の人（または別の画面）がこの表示グループを先に更新しました。一覧を再読み込みしてから、もう一度編集して保存してください。";

/// 参照されているタグの削除を拒否するときのフィールド名。
pub const TAG_REFERENCED_FIELD: &str = "displayGroups";

/// 表示種別（§3.2 の 4 種固定）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DisplayKind {
    Trend,
    Digital,
    Bar,
    Gauge,
}

impl DisplayKind {
    /// 4 種すべて（画面の選択肢・検証の案内の順）。
    pub const ALL: [DisplayKind; 4] = [
        DisplayKind::Trend,
        DisplayKind::Digital,
        DisplayKind::Bar,
        DisplayKind::Gauge,
    ];

    /// ワイヤ・DB での名前。
    pub fn as_str(self) -> &'static str {
        match self {
            DisplayKind::Trend => "trend",
            DisplayKind::Digital => "digital",
            DisplayKind::Bar => "bar",
            DisplayKind::Gauge => "gauge",
        }
    }

    /// 画面・検証の案内での名前。
    pub fn label(self) -> &'static str {
        match self {
            DisplayKind::Trend => "トレンド",
            DisplayKind::Digital => "デジタル",
            DisplayKind::Bar => "バー",
            DisplayKind::Gauge => "計器",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == value)
    }

    /// この種別が持てる表示属性（`attributes` のキー）。閉じた集合。
    pub fn allowed_attributes(self) -> &'static [&'static str] {
        match self {
            DisplayKind::Trend => &["timeWindowSec"],
            DisplayKind::Digital | DisplayKind::Bar | DisplayKind::Gauge => &[],
        }
    }
}

/// 種別ごとの表示属性（検証済み・既定値を埋めた形）。種別に無い項目は `None`
/// で、JSON には出ない。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DisplayAttributes {
    /// トレンドの既定時間窓（秒）。トレンドだけ。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_window_sec: Option<i64>,
}

/// ペン（検証済み）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pen {
    /// banto-tags の `tags.id`。
    pub tag_id: i64,
    /// 系列色の枠（1..=8）。`None` = 既定（ペンの位置 + 1）。
    pub color_slot: Option<i64>,
}

/// 表示グループ（読み取りの応答）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DisplayGroup {
    pub id: i64,
    pub name: String,
    pub sort_order: i64,
    pub kind: DisplayKind,
    pub attributes: DisplayAttributes,
    pub pens: Vec<Pen>,
    pub revision: i64,
}

/// 作成・更新の本文（camelCase）。`kind`・`attributes` を緩い型で受けるのは、
/// 未知の種別・未知の属性を serde の拒否（axum 既定の平文の 422）ではなく、
/// [`validate_display_group`] の人間可読な `field_errors` で返すため。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DisplayGroupPayload {
    pub name: String,
    /// 作成のときだけ使う（省略で末尾）。**更新では無視する**（モジュール doc）。
    #[serde(default)]
    pub sort_order: Option<i64>,
    pub kind: String,
    #[serde(default)]
    pub attributes: Map<String, Value>,
    #[serde(default)]
    pub pens: Vec<PenPayload>,
    /// 楽観ロック（更新のみ）。編集画面が最後に読んだ `revision`。
    #[serde(default)]
    pub expected_revision: Option<i64>,
}

/// ペンの本文。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PenPayload {
    pub tag_id: i64,
    #[serde(default)]
    pub color_slot: Option<i64>,
}

/// 並べ替えの本文: 全グループの ID を、新しい並び順で 1 回ずつ。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReorderPayload {
    pub ids: Vec<i64>,
}

/// 参照元（タグの削除を拒否するときに並べる）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DisplayGroupRef {
    pub id: i64,
    pub name: String,
}

/// 検証を通った定義（保存する形）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidDisplayGroup {
    /// 前後の空白を除いた名前。
    pub name: String,
    pub sort_order: Option<i64>,
    pub kind: DisplayKind,
    pub attributes: DisplayAttributes,
    pub pens: Vec<Pen>,
}

fn field_error(field: impl Into<String>, message: impl Into<String>) -> FieldError {
    FieldError {
        field: field.into(),
        message: message.into(),
    }
}

/// 表示グループの版の食い違いを表すエラー（形は `crate::revision` と共通）。
pub fn revision_conflict_error() -> BantoError {
    crate::revision::revision_conflict_error(REVISION_CONFLICT_MESSAGE)
}

fn kind_choices() -> String {
    DisplayKind::ALL
        .iter()
        .map(|kind| format!("{}（{}）", kind.as_str(), kind.label()))
        .collect::<Vec<_>>()
        .join(" / ")
}

/// 種別ごとの属性を検証し、既定値を埋める（閉じた集合: 種別に無い項目は拒否）。
fn validate_attributes(
    kind: DisplayKind,
    attributes: &Map<String, Value>,
    errors: &mut Vec<FieldError>,
) -> DisplayAttributes {
    let allowed = kind.allowed_attributes();
    // キー順を固定して、エラーの並びを決定的にする。
    let sorted: BTreeMap<&String, &Value> = attributes.iter().collect();
    for key in sorted.keys() {
        if !allowed.contains(&key.as_str()) {
            errors.push(field_error(
                format!("attributes.{key}"),
                format!("{}の表示グループでは使えない項目です", kind.label()),
            ));
        }
    }
    let mut out = DisplayAttributes::default();
    if kind == DisplayKind::Trend {
        out.time_window_sec = Some(match attributes.get("timeWindowSec") {
            None | Some(Value::Null) => DEFAULT_TREND_TIME_WINDOW_SEC,
            Some(value) => match value.as_i64() {
                Some(sec) if TREND_TIME_WINDOWS_SEC.contains(&sec) => sec,
                _ => {
                    errors.push(field_error(
                        "attributes.timeWindowSec",
                        format!(
                            "既定の時間窓は {} 秒のいずれかです",
                            TREND_TIME_WINDOWS_SEC
                                .iter()
                                .map(|sec| sec.to_string())
                                .collect::<Vec<_>>()
                                .join(" / ")
                        ),
                    ));
                    DEFAULT_TREND_TIME_WINDOW_SEC
                }
            },
        });
    }
    out
}

/// 形の検証（DB を見ない部分）。[`validate_display_group`] の前半で、単体でも
/// 使える（総当たりのテストのため）。
pub fn validate_shape(payload: &DisplayGroupPayload) -> Result<ValidDisplayGroup, Vec<FieldError>> {
    let mut errors = Vec::new();

    let name = payload.name.trim().to_string();
    if name.is_empty() {
        errors.push(field_error("name", "名前を入力してください"));
    } else if name.chars().count() > MAX_NAME_CHARS {
        errors.push(field_error(
            "name",
            format!("名前は {MAX_NAME_CHARS} 文字以内にしてください"),
        ));
    }

    if let Some(order) = payload.sort_order {
        if !(0..=MAX_SORT_ORDER).contains(&order) {
            errors.push(field_error(
                "sortOrder",
                format!("並び順は 0〜{MAX_SORT_ORDER} にしてください"),
            ));
        }
    }

    let kind = DisplayKind::parse(&payload.kind);
    let attributes = match kind {
        Some(kind) => validate_attributes(kind, &payload.attributes, &mut errors),
        None => {
            errors.push(field_error(
                "kind",
                format!("表示種別は {} のいずれかです", kind_choices()),
            ));
            DisplayAttributes::default()
        }
    };

    if payload.pens.len() > MAX_PENS_PER_GROUP {
        errors.push(field_error(
            "pens",
            format!(
                "ペンは 1 グループに {MAX_PENS_PER_GROUP} 本までです（{} 本あります）",
                payload.pens.len()
            ),
        ));
    }
    let mut seen = HashSet::new();
    let mut pens = Vec::with_capacity(payload.pens.len());
    for (index, pen) in payload.pens.iter().enumerate() {
        if !seen.insert(pen.tag_id) {
            errors.push(field_error(
                format!("pens[{index}].tagId"),
                "同じタグを 1 つのグループに 2 回割り当てることはできません",
            ));
        }
        if let Some(slot) = pen.color_slot {
            if !(1..=COLOR_SLOTS).contains(&slot) {
                errors.push(field_error(
                    format!("pens[{index}].colorSlot"),
                    format!("色は 1〜{COLOR_SLOTS} の枠から選んでください"),
                ));
            }
        }
        pens.push(Pen {
            tag_id: pen.tag_id,
            color_slot: pen.color_slot,
        });
    }

    match (kind, errors.is_empty()) {
        (Some(kind), true) => Ok(ValidDisplayGroup {
            name,
            sort_order: payload.sort_order,
            kind,
            attributes,
            pens,
        }),
        _ => Err(errors),
    }
}

/// 表示グループの定義を検証する、経路共通の**唯一の**関数（モジュール doc
/// 「検証は 1 関数」）。`target_id` は更新する行の ID（作成は `None`）。
///
/// 形（[`validate_shape`]）に加えて、`conn` の上で次を確かめる:
/// - 作成なら、グループ数が [`MAX_DISPLAY_GROUPS`] に達していないこと
/// - 名前が他のグループと重ならないこと（自分自身は除く）
/// - 各ペンのタグが存在すること
///
/// 誤りはすべて `BantoError::Validation` の `field_errors` に並べて返す。
/// 保存は呼び出し側が同じトランザクションの中で行う。
pub async fn validate_display_group(
    conn: &mut SqliteConnection,
    payload: &DisplayGroupPayload,
    target_id: Option<i64>,
) -> Result<ValidDisplayGroup, BantoError> {
    let shape = validate_shape(payload);
    let mut errors = match &shape {
        Ok(_) => Vec::new(),
        Err(errors) => errors.clone(),
    };

    if target_id.is_none() {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM display_groups")
            .fetch_one(&mut *conn)
            .await
            .map_err(banto_storage::storage_error)?;
        if count >= MAX_DISPLAY_GROUPS as i64 {
            errors.push(field_error(
                "displayGroups",
                format!("表示グループは {MAX_DISPLAY_GROUPS} 個までです。不要なグループを削除してから作成してください"),
            ));
        }
    }

    let name = payload.name.trim();
    if !name.is_empty() {
        let clash: Option<i64> =
            sqlx::query_scalar("SELECT id FROM display_groups WHERE name = ? AND id IS NOT ?")
                .bind(name)
                .bind(target_id)
                .fetch_optional(&mut *conn)
                .await
                .map_err(banto_storage::storage_error)?;
        if clash.is_some() {
            errors.push(field_error("name", "既に使用されています"));
        }
    }

    for (index, pen) in payload.pens.iter().enumerate() {
        let exists: Option<i64> = sqlx::query_scalar("SELECT id FROM tags WHERE id = ?")
            .bind(pen.tag_id)
            .fetch_optional(&mut *conn)
            .await
            .map_err(banto_storage::storage_error)?;
        if exists.is_none() {
            errors.push(field_error(
                format!("pens[{index}].tagId"),
                format!("タグ（ID {}）が見つかりません", pen.tag_id),
            ));
        }
    }

    match shape {
        Ok(valid) if errors.is_empty() => Ok(valid),
        _ => Err(BantoError::Validation {
            field_errors: errors,
        }),
    }
}

/// 監査の `detail`（REST・Tauri 共通）。値そのものではなく要約だけ
/// （`audit_log.detail` の規約）。
pub fn audit_detail(group: &DisplayGroup) -> Value {
    json!({
        "name": group.name,
        "kind": group.kind.as_str(),
        "pens": group.pens.len(),
    })
}

/// タグの削除を拒否するときのエラー（参照元を並べる）。
pub fn tag_referenced_error(referrers: &[DisplayGroupRef]) -> BantoError {
    let names = referrers
        .iter()
        .map(|group| format!("「{}」", group.name))
        .collect::<Vec<_>>()
        .join("");
    BantoError::Validation {
        field_errors: vec![field_error(
            TAG_REFERENCED_FIELD,
            format!("このタグは表示グループ{names}のペンに割り当てられているため削除できません。先にグループ設定でペンから外してください"),
        )],
    }
}

#[derive(sqlx::FromRow)]
struct GroupRow {
    id: i64,
    name: String,
    sort_order: i64,
    kind: String,
    attributes: String,
    revision: i64,
}

#[derive(sqlx::FromRow)]
struct PenRow {
    group_id: i64,
    tag_id: i64,
    color_slot: Option<i64>,
}

fn storage(err: sqlx::Error) -> BantoError {
    banto_storage::storage_error(err)
}

fn not_found(id: i64) -> BantoError {
    BantoError::NotFound {
        resource: AUDIT_RESOURCE.to_string(),
        id: id.to_string(),
    }
}

fn into_group(row: GroupRow, pens: Vec<Pen>) -> Result<DisplayGroup, BantoError> {
    let kind = DisplayKind::parse(&row.kind).ok_or_else(|| {
        BantoError::Storage(format!(
            "display_groups.kind に未知の値があります: {}",
            row.kind
        ))
    })?;
    let attributes: DisplayAttributes = serde_json::from_str(&row.attributes)
        .map_err(|err| BantoError::Storage(format!("display_groups.attributes: {err}")))?;
    Ok(DisplayGroup {
        id: row.id,
        name: row.name,
        sort_order: row.sort_order,
        kind,
        attributes,
        pens,
        revision: row.revision,
    })
}

/// 一意制約違反（名前）を、検証と同じ形のエラーにする。検証は同じ
/// トランザクションの中で名前の重複を見ているので通常は届かない（最後の砦）。
fn map_write_error(err: sqlx::Error) -> BantoError {
    if let sqlx::Error::Database(db) = &err {
        if db.is_unique_violation() {
            return BantoError::Validation {
                field_errors: vec![field_error("name", "既に使用されています")],
            };
        }
    }
    storage(err)
}

/// 表示グループのサービス（REST・Tauri 共通）。`Clone` で同じ pool を共有する。
#[derive(Clone)]
pub struct DisplayGroupService {
    pool: SqlitePool,
}

impl DisplayGroupService {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// 全グループ（並び順 → ID 順）。
    ///
    /// グループの行とペンは 2 つの SELECT で読むので、**1 つの読み取り
    /// トランザクションの中で**読む（オーナーレビュー P2-1）。素の接続で読むと、
    /// 2 つの間に別の更新が commit されたとき、古い名前・種別・版に新しいペンが
    /// 混ざった応答になりうる（その応答の `revision` で保存すると、見ていない
    /// ペンを上書きする）。WAL の読み取りトランザクションは最初の SELECT の時点の
    /// スナップショットを最後まで見る。
    pub async fn list(&self) -> Result<Vec<DisplayGroup>, BantoError> {
        self.list_paused(None).await
    }

    /// [`Self::list`] の本体。`pause` はテスト専用の割り込み点（2 つの SELECT の
    /// 間で止まる、[`ReadPause`]）で、本番は常に `None`。
    pub(crate) async fn list_paused(
        &self,
        pause: Option<&ReadPause>,
    ) -> Result<Vec<DisplayGroup>, BantoError> {
        let mut tx = self.pool.begin().await.map_err(storage)?;
        let groups = list_on(&mut tx, pause).await?;
        tx.commit().await.map_err(storage)?;
        Ok(groups)
    }

    /// 1 つ。[`Self::list`] と同じく 1 つの読み取りトランザクションで読む。
    pub async fn get(&self, id: i64) -> Result<DisplayGroup, BantoError> {
        self.get_paused(id, None).await
    }

    /// [`Self::get`] の本体（`pause` は [`Self::list_paused`] と同じ）。
    pub(crate) async fn get_paused(
        &self,
        id: i64,
        pause: Option<&ReadPause>,
    ) -> Result<DisplayGroup, BantoError> {
        let mut tx = self.pool.begin().await.map_err(storage)?;
        let group = get_on(&mut tx, id, pause).await?;
        tx.commit().await.map_err(storage)?;
        Ok(group)
    }

    /// 保存せずに検証だけする（段階 3 の「検証だけする口」の土台。今は画面から
    /// は使っていない）。
    pub async fn validate(
        &self,
        payload: &DisplayGroupPayload,
        target_id: Option<i64>,
    ) -> Result<ValidDisplayGroup, BantoError> {
        let mut conn = self.pool.acquire().await.map_err(storage)?;
        validate_display_group(&mut conn, payload, target_id).await
    }

    pub async fn create(&self, payload: &DisplayGroupPayload) -> Result<DisplayGroup, BantoError> {
        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(storage)?;
        let valid = validate_display_group(&mut tx, payload, None).await?;
        let sort_order = match valid.sort_order {
            Some(order) => order,
            None => end_sort_order(&mut tx).await?,
        };
        let attributes = serde_json::to_string(&valid.attributes)
            .map_err(|err| BantoError::Other(err.to_string()))?;
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO display_groups (name, sort_order, kind, attributes) \
             VALUES (?, ?, ?, ?) RETURNING id",
        )
        .bind(&valid.name)
        .bind(sort_order)
        .bind(valid.kind.as_str())
        .bind(&attributes)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_write_error)?;
        insert_pens(&mut tx, id, &valid.pens).await?;
        let created = get_on(&mut tx, id, None).await?;
        tx.commit().await.map_err(storage)?;
        Ok(created)
    }

    /// 内容（名前・種別・属性・ペン）の全項目の置き換え。**`sortOrder` は本文に
    /// あっても無視し、今の並び順を保つ**（Codex レビュー P2-3: 並べ替えは版を
    /// 進めないので、前に読んだ `sortOrder` を載せた本文で並びを書き戻すと、その
    /// 間の並べ替えを黙って取り消してしまう。並びを変える口は [`Self::reorder`]
    /// だけ。拒否ではなく無視にしたのは、GET の応答をそのまま送り返せる往復の形を
    /// 保つため）。
    /// `expectedRevision` が保存されている版と違えば [`revision_conflict_error`]。
    pub async fn update(
        &self,
        id: i64,
        payload: &DisplayGroupPayload,
    ) -> Result<DisplayGroup, BantoError> {
        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(storage)?;
        let current: Option<i64> =
            sqlx::query_scalar("SELECT revision FROM display_groups WHERE id = ?")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(storage)?;
        let Some(revision) = current else {
            return Err(not_found(id));
        };
        if let Some(expected) = payload.expected_revision {
            if expected != revision {
                return Err(revision_conflict_error());
            }
        }
        let valid = validate_display_group(&mut tx, payload, Some(id)).await?;
        let attributes = serde_json::to_string(&valid.attributes)
            .map_err(|err| BantoError::Other(err.to_string()))?;
        sqlx::query(
            "UPDATE display_groups SET name = ?, kind = ?, attributes = ?, \
             revision = revision + 1, updated_at = datetime('now') WHERE id = ?",
        )
        .bind(&valid.name)
        .bind(valid.kind.as_str())
        .bind(&attributes)
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(map_write_error)?;
        sqlx::query("DELETE FROM display_group_pens WHERE group_id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        insert_pens(&mut tx, id, &valid.pens).await?;
        let updated = get_on(&mut tx, id, None).await?;
        tx.commit().await.map_err(storage)?;
        Ok(updated)
    }

    /// 削除（ペンは `ON DELETE CASCADE` で一緒に消える）。削除した行を返す
    /// （監査の `detail` に名前を残すため）。
    pub async fn delete(&self, id: i64) -> Result<DisplayGroup, BantoError> {
        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(storage)?;
        let existing = get_on(&mut tx, id, None).await?;
        sqlx::query("DELETE FROM display_groups WHERE id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        tx.commit().await.map_err(storage)?;
        Ok(existing)
    }

    /// 並べ替え: `ids` は全グループの ID を新しい順で 1 回ずつ（過不足・重複が
    /// あれば、画面が古い一覧を見ているので拒否する）。`sortOrder` を 0 から
    /// 振り直す。版は進めない（モジュール doc）。
    pub async fn reorder(&self, ids: &[i64]) -> Result<Vec<DisplayGroup>, BantoError> {
        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(storage)?;
        let existing: Vec<i64> = sqlx::query_scalar("SELECT id FROM display_groups")
            .fetch_all(&mut *tx)
            .await
            .map_err(storage)?;
        let existing: HashSet<i64> = existing.into_iter().collect();
        let requested: HashSet<i64> = ids.iter().copied().collect();
        if requested.len() != ids.len() || requested != existing {
            return Err(BantoError::Validation {
                field_errors: vec![field_error(
                    "ids",
                    "表示グループの一覧が最新ではありません。再読み込みしてから並べ替えてください",
                )],
            });
        }
        for (order, id) in ids.iter().enumerate() {
            sqlx::query("UPDATE display_groups SET sort_order = ? WHERE id = ?")
                .bind(order as i64)
                .bind(id)
                .execute(&mut *tx)
                .await
                .map_err(storage)?;
        }
        let groups = list_on(&mut tx, None).await?;
        tx.commit().await.map_err(storage)?;
        Ok(groups)
    }

    /// `tag_id` をペンに割り当てているグループ（並び順）。
    pub async fn referrers_of_tag(&self, tag_id: i64) -> Result<Vec<DisplayGroupRef>, BantoError> {
        let mut conn = self.pool.acquire().await.map_err(storage)?;
        referrers_on(&mut conn, tag_id).await
    }

    /// タグを削除する。ただし表示グループのペンに割り当てられていれば、参照元を
    /// 並べて拒否する（§3.7.9 の 8）。REST の `DELETE /api/tags/{id}` と Tauri の
    /// `tags_delete` の両方がこれを呼ぶ。確認と削除は同じ `BEGIN IMMEDIATE` の
    /// 中で行うので、その間にペンが足されることは無い。
    pub async fn delete_tag_unless_referenced(
        &self,
        tags: &TagService,
        tag_id: i64,
    ) -> Result<(), BantoError> {
        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(storage)?;
        let referrers = referrers_on(&mut tx, tag_id).await?;
        if !referrers.is_empty() {
            return Err(tag_referenced_error(&referrers));
        }
        tags.delete_tx(&mut tx, tag_id).await?;
        tx.commit().await.map_err(storage)?;
        Ok(())
    }
}

async fn insert_pens(
    conn: &mut SqliteConnection,
    group_id: i64,
    pens: &[Pen],
) -> Result<(), BantoError> {
    for (position, pen) in pens.iter().enumerate() {
        sqlx::query(
            "INSERT INTO display_group_pens (group_id, position, tag_id, color_slot) \
             VALUES (?, ?, ?, ?)",
        )
        .bind(group_id)
        .bind(position as i64)
        .bind(pen.tag_id)
        .bind(pen.color_slot)
        .execute(&mut *conn)
        .await
        .map_err(storage)?;
    }
    Ok(())
}

/// 作成で `sortOrder` を省略したときの末尾の位置（Codex レビュー P2-4）。
/// 「最大 + 1」が [`MAX_SORT_ORDER`] を超えるときは、今の並びのまま 0 から振り
/// 直してから末尾を返す（グループは最大 [`MAX_DISPLAY_GROUPS`] なので、振り直せば
/// 必ず収まる）。呼び出し側の書き込みトランザクションの中で呼ぶ。
async fn end_sort_order(conn: &mut SqliteConnection) -> Result<i64, BantoError> {
    let next: i64 =
        sqlx::query_scalar("SELECT COALESCE(MAX(sort_order) + 1, 0) FROM display_groups")
            .fetch_one(&mut *conn)
            .await
            .map_err(storage)?;
    if next <= MAX_SORT_ORDER {
        return Ok(next);
    }
    let ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM display_groups ORDER BY sort_order, id")
        .fetch_all(&mut *conn)
        .await
        .map_err(storage)?;
    for (order, id) in ids.iter().enumerate() {
        sqlx::query("UPDATE display_groups SET sort_order = ? WHERE id = ?")
            .bind(order as i64)
            .bind(id)
            .execute(&mut *conn)
            .await
            .map_err(storage)?;
    }
    Ok(ids.len() as i64)
}

/// テスト専用の割り込み点: 読み取りの 2 つの SELECT（グループの行 → ペン）の
/// 間で止まり、`reached` を知らせて `resume` を待つ。その間に別の接続から更新を
/// commit すると、読み取りが 1 つのスナップショットで行われているかを決定的に
/// 確かめられる。本番の呼び出しは常に `None`。
pub(crate) struct ReadPause {
    pub(crate) reached: std::sync::Arc<tokio::sync::Notify>,
    pub(crate) resume: std::sync::Arc<tokio::sync::Notify>,
}

impl ReadPause {
    async fn hold(&self) {
        self.reached.notify_one();
        self.resume.notified().await;
    }
}

async fn list_on(
    conn: &mut SqliteConnection,
    pause: Option<&ReadPause>,
) -> Result<Vec<DisplayGroup>, BantoError> {
    let rows: Vec<GroupRow> = sqlx::query_as(
        "SELECT id, name, sort_order, kind, attributes, revision FROM display_groups \
         ORDER BY sort_order, id",
    )
    .fetch_all(&mut *conn)
    .await
    .map_err(storage)?;
    if let Some(pause) = pause {
        pause.hold().await;
    }
    let pen_rows: Vec<PenRow> = sqlx::query_as(
        "SELECT group_id, tag_id, color_slot FROM display_group_pens ORDER BY group_id, position",
    )
    .fetch_all(&mut *conn)
    .await
    .map_err(storage)?;
    let mut pens_by_group: BTreeMap<i64, Vec<Pen>> = BTreeMap::new();
    for pen in pen_rows {
        pens_by_group.entry(pen.group_id).or_default().push(Pen {
            tag_id: pen.tag_id,
            color_slot: pen.color_slot,
        });
    }
    rows.into_iter()
        .map(|row| {
            let pens = pens_by_group.remove(&row.id).unwrap_or_default();
            into_group(row, pens)
        })
        .collect()
}

async fn get_on(
    conn: &mut SqliteConnection,
    id: i64,
    pause: Option<&ReadPause>,
) -> Result<DisplayGroup, BantoError> {
    let row: Option<GroupRow> = sqlx::query_as(
        "SELECT id, name, sort_order, kind, attributes, revision FROM display_groups WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(storage)?;
    let Some(row) = row else {
        return Err(not_found(id));
    };
    if let Some(pause) = pause {
        pause.hold().await;
    }
    let pens: Vec<PenRow> = sqlx::query_as(
        "SELECT group_id, tag_id, color_slot FROM display_group_pens WHERE group_id = ? \
         ORDER BY position",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await
    .map_err(storage)?;
    into_group(
        row,
        pens.into_iter()
            .map(|pen| Pen {
                tag_id: pen.tag_id,
                color_slot: pen.color_slot,
            })
            .collect(),
    )
}

async fn referrers_on(
    conn: &mut SqliteConnection,
    tag_id: i64,
) -> Result<Vec<DisplayGroupRef>, BantoError> {
    let rows: Vec<(i64, String)> = sqlx::query_as(
        "SELECT g.id, g.name FROM display_groups g WHERE g.id IN \
         (SELECT group_id FROM display_group_pens WHERE tag_id = ?) \
         ORDER BY g.sort_order, g.id",
    )
    .bind(tag_id)
    .fetch_all(&mut *conn)
    .await
    .map_err(storage)?;
    Ok(rows
        .into_iter()
        .map(|(id, name)| DisplayGroupRef { id, name })
        .collect())
}

#[cfg(test)]
pub(crate) mod tests;
