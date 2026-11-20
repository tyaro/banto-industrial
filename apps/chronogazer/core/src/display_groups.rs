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
//!   "name": "ライン1", "bundle": "ライン A", "sortOrder": 0, "kind": "trend",
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
//! - `pens`: 最大 [`MAX_PENS_PER_GROUP`]（16）。各ペンはタグへの参照（ID）と表示属性。
//!   表示属性は色（`colorSlot`）と、#1207・#1231 PR 6a で足したトレンドの語彙（下の「ペンの属性」）。
//!   `colorSlot` = banto チャートの系列色の枠
//!   （1..=16。1..=8 は `--banto-chart-N`、9..=16 は `--bi-chart-N`）。`null` は既定（ペンの位置 + 1 の枠、
//!   `@banto/charts` の `seriesColorVar(index)` と同じ割り当て）。任意の色コード
//!   にしないのは、テーマ（ライト / ダーク）に追従させるため。
//!   - ペンの属性（すべて任意、`null`・キー無し = 既定。応答は全部のキーを出す）:
//!     `label`（凡例の名前、[`MAX_PEN_LABEL_CHARS`] 文字まで、既定はタグの名前）・`unit`
//!     （[`MAX_PEN_UNIT_CHARS`] 文字まで、既定はタグの単位）・`decimals`（0〜[`MAX_PEN_DECIMALS`]、
//!     既定はタグの設定）・`lineStyle`（[`LineStyle`]）・`lineWidth`（1〜4 の 0.5 刻み、
//!     既定 [`DEFAULT_LINE_WIDTH`]）・`marker`（[`Marker`]）・`markerSize`（2〜8、既定
//!     [`DEFAULT_MARKER_SIZE`]）・`axis`（[`Axis`]、既定 left）・`yMin`・`yMax`（有限の数、両方
//!     あれば `yMin < yMax`、既定は工学値レンジ）。種別を問わず保存する（種別は後で変えられる）が、
//!     画面で効くのはトレンドだけ。
//!     管理線（PR 7a）: `limitLines`（true で HH・H・L・LL の線を引く。`null`・false = 引かない）・
//!     `limitHh`・`limitH`・`limitL`・`limitLl`（有限の数。`null` = タグのしきい値を使う。値は
//!     表示だけに使い、しきい値のイベントは変えない。設定したものどうしは `LL <= L <= H <= HH`、
//!     `crate::tag_thresholds` と同じ決まり。`limitLines` が無効でも保存する）。bit の表し方は PR 7b 以降。
//! - **しきい値は持たない**。しきい値は記録計の側のタグごとの設定
//!   （`crate::tag_thresholds`、#532。2026-10-08 オーナー決定）で、表示グループ
//!   からは参照するだけ（画面はその設定を読み取り専用で見せる）。
//! - `bundle`: 束の名前（作成では任意、省略 = 束なし。**更新では必須**で、キーが
//!   無い本文は拒否する（古い本文が束を黙って外さないため）。束から外すときは
//!   `null` を送る（保つときは読んだ値を送る。OpenAPI の PUT は別スキーマ
//!   `DisplayGroupUpdatePayload` で `bundle` を必須にしている）。前後の空白を除いて空なら `null`、[`MAX_BUNDLE_CHARS`]（50）
//!   文字以内。[`NO_BUNDLE_LABEL`]（「束なし」）は束に入れないグループの表示名なので、
//!   束の名前には使えない）。同じ名前のグループが 1 つの束になる
//!   名前だけの 1 階層の入れ物で、束のテーブルは持たない（trend-design.md §9 Q5、
//!   2026-11-20 オーナー決定）。`sortOrder` と違い内容なので、更新（PUT）が書き、
//!   版（`revision`）を進める。
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
//! ## Hub のタグをペンにする一括保存（#1179 PR5）
//!
//! ペンは登録済みの記録項目（`tagId`）の代わりに、Hub のカタログのタグ
//! （`hubTag` = [`HubTagRow`]）を指せる（どちらか 1 つ）。作成・更新は 1 つの
//! `BEGIN IMMEDIATE` の中で次を行う（[`DisplayGroupService::create_with_hub_tags`]・
//! [`DisplayGroupService::update_with_hub_tags`]。REST・MCP・Tauri の作成・更新は
//! すべてここを通る - docs/chronogazer/chronogazer-ui-improvement-proposal.md §4・§8）:
//!
//! 1. 更新なら、対象の存在と `expectedRevision`。
//! 2. `hubTag` の外部名を、hub 接続の下の登録済みのタグ（`crate::hub_tags` の
//!    「登録済み」と同じ範囲）に解決する。登録済みならその id を参照するだけ
//!    （二重登録しない）。同じ外部名を 2 本のペンが指すのは後ろのペンの誤り。
//! 3. 未登録のものがあれば、本文の `hubTagGroup`（既存の hub の収集グループか、
//!    新しいグループ）に `crate::hub_tags` の登録の本体で登録する（hub 接続の暗黙の
//!    作成も含む）。行の誤りはペンの位置（`pens[i].hubTag.*`）、登録先の誤りは
//!    `hubTagGroup.*` に付け替える。未登録のものが無ければ `hubTagGroup` は使わない。
//! 4. 解決した id のペンで [`validate_display_group`] → 書き込み。
//!
//! どこかで失敗すれば何も書かない（登録も巻き戻る）。登録で落ちたときも、形の
//! 誤り（名前・種別・属性・ペンの数・色）は一緒に返す。
//!
//! ## 楽観ロック
//!
//! 更新は `expectedRevision` を受け取り、保存されている版と違えば
//! `{kind:"validation", field_errors:[{field:"expectedRevision", ...}]}` で拒否する
//! （[`revision_conflict_error`]。REST は `409`、Tauri は同じ形の検証エラー）。
//! タグの楽観ロック（#525）と同じ形にしてある。省略した更新は版を確かめない
//! （banto-tags の `expected_revision` と同じ扱い。画面は常に送る）。

use banto_core::{BantoError, FieldError};
use banto_tags::{Tag, TagService};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sqlx::{SqliteConnection, SqlitePool};
use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};

use crate::hub_tags::{
    register_in_tx, registered_tag_ids_tx, HubRegistrationResult, HubTagRegistrar,
    HubTagRegistration, HubTagRow, NewHubGroup,
};

/// 監査の `resource`（REST・Tauri 共通）。
pub const AUDIT_RESOURCE: &str = "display_groups";

/// グループ数の上限（trend-design.md §6。旧 16 → 36、#1207・#1231 PR 5a）。
pub const MAX_DISPLAY_GROUPS: usize = 36;

/// 1 グループのペンの上限（trend-design.md §6。旧 8 → 16、#1207・#1231 PR 5a）。
pub const MAX_PENS_PER_GROUP: usize = 16;

/// 名前の最大文字数（記録の名前欄と同じ 100）。
pub const MAX_NAME_CHARS: usize = 100;
/// 束の名前の最大文字数（`display_groups.bundle` の CHECK と揃える）。
pub const MAX_BUNDLE_CHARS: usize = 50;
/// 束に入れないグループの表示名（画面側 `bundleLogic.ts` の `NO_BUNDLE_LABEL` と同じ）。
/// 本物の束の名前と衝突するので、束の名前には使えない。
pub const NO_BUNDLE_LABEL: &str = "束なし";

/// `sortOrder` の上限（並べ替えは 0 から詰めるので、36 グループには十分）。
pub const MAX_SORT_ORDER: i64 = 9999;

/// 色の枠の数。1〜8 は banto の系列色（`--banto-chart-N`、`MAX_CHART_SERIES`）、
/// 9〜16 は industrial のテーマの `--bi-chart-N`（trend-design.md §9 Q4）。
pub const COLOR_SLOTS: i64 = 16;

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

/// ペンの凡例の名前の最大文字数（char 数）。
pub const MAX_PEN_LABEL_CHARS: usize = 64;

/// ペンの単位の最大文字数（char 数）。
pub const MAX_PEN_UNIT_CHARS: usize = 16;

/// ペンの小数の桁の上限（下限は 0）。
pub const MAX_PEN_DECIMALS: i64 = 6;

/// ペンの線幅（px）の範囲。範囲内の 0.5 刻みだけ。
pub const LINE_WIDTH_RANGE: (f64, f64) = (1.0, 4.0);

/// `lineWidth` が `null` のときの線幅（px）。TS 側
/// （`displayGroupsAdmin.ts` の `DEFAULT_LINE_WIDTH`）と同じ値にする。
pub const DEFAULT_LINE_WIDTH: f64 = 1.5;

/// ペンのマーカーの大きさ（px）の範囲。
pub const MARKER_SIZE_RANGE: (i64, i64) = (2, 8);

/// `markerSize` が `null` のときの大きさ（px）。TS 側の `DEFAULT_MARKER_SIZE` と同じ値にする。
pub const DEFAULT_MARKER_SIZE: i64 = 4;

/// ペンの線種。`null` = 実線。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[derive(utoipa::ToSchema)]
pub enum LineStyle {
    Solid,
    Dashed,
    DashDot,
    DashDotDot,
    None,
}

impl LineStyle {
    pub const ALL: [LineStyle; 5] = [
        LineStyle::Solid,
        LineStyle::Dashed,
        LineStyle::DashDot,
        LineStyle::DashDotDot,
        LineStyle::None,
    ];

    /// ワイヤ・DB での名前。
    pub fn as_str(self) -> &'static str {
        match self {
            LineStyle::Solid => "solid",
            LineStyle::Dashed => "dashed",
            LineStyle::DashDot => "dashDot",
            LineStyle::DashDotDot => "dashDotDot",
            LineStyle::None => "none",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_str() == value)
    }
}

/// ペンのマーカー。`null` = なし。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[derive(utoipa::ToSchema)]
pub enum Marker {
    None,
    Circle,
    Square,
    Triangle,
    Diamond,
}

impl Marker {
    pub const ALL: [Marker; 5] = [
        Marker::None,
        Marker::Circle,
        Marker::Square,
        Marker::Triangle,
        Marker::Diamond,
    ];

    /// ワイヤ・DB での名前。
    pub fn as_str(self) -> &'static str {
        match self {
            Marker::None => "none",
            Marker::Circle => "circle",
            Marker::Square => "square",
            Marker::Triangle => "triangle",
            Marker::Diamond => "diamond",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_str() == value)
    }
}

/// ペンの縦軸の側。`null` = 左。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[derive(utoipa::ToSchema)]
pub enum Axis {
    Left,
    Right,
}

impl Axis {
    pub const ALL: [Axis; 2] = [Axis::Left, Axis::Right];

    /// ワイヤ・DB での名前。
    pub fn as_str(self) -> &'static str {
        match self {
            Axis::Left => "left",
            Axis::Right => "right",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_str() == value)
    }
}

/// 表示種別（§3.2 の 4 種固定）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(utoipa::ToSchema)]
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
#[derive(utoipa::ToSchema)]
pub struct DisplayAttributes {
    /// トレンドの既定時間窓（秒）。トレンドだけ。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_window_sec: Option<i64>,
}

/// ペン（検証済み）。属性の `None` は既定（各項目の doc とモジュール doc）。
/// 応答は全部のキーを常に出す（既定のものは `null`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[derive(utoipa::ToSchema)]
pub struct Pen {
    /// banto-tags の `tags.id`。
    pub tag_id: i64,
    /// 系列色の枠（1..=16）。`None` = 既定（ペンの位置 + 1）。
    pub color_slot: Option<i64>,
    /// 凡例の名前。`None` = タグの名前。
    pub label: Option<String>,
    /// 単位。`None` = タグの単位。
    pub unit: Option<String>,
    /// 小数の桁（0..=6）。`None` = タグの設定。
    pub decimals: Option<i64>,
    /// 線種。`None` = 実線。
    pub line_style: Option<LineStyle>,
    /// 線幅（px、1〜4 の 0.5 刻み）。`None` = [`DEFAULT_LINE_WIDTH`]。
    pub line_width: Option<f64>,
    /// マーカー。`None` = なし。
    pub marker: Option<Marker>,
    /// マーカーの大きさ（px、2..=8）。`None` = [`DEFAULT_MARKER_SIZE`]。
    pub marker_size: Option<i64>,
    /// 縦軸の側。`None` = 左。
    pub axis: Option<Axis>,
    /// 縦軸の最小。`None` = 工学値レンジ。
    pub y_min: Option<f64>,
    /// 縦軸の最大。`None` = 工学値レンジ。
    pub y_max: Option<f64>,
    /// 管理線を引くか。`None`・`false` = 引かない。
    pub limit_lines: Option<bool>,
    /// 管理線の HH の上書き。`None` = タグのしきい値。
    pub limit_hh: Option<f64>,
    /// 管理線の H の上書き。`None` = タグのしきい値。
    pub limit_h: Option<f64>,
    /// 管理線の L の上書き。`None` = タグのしきい値。
    pub limit_l: Option<f64>,
    /// 管理線の LL の上書き。`None` = タグのしきい値。
    pub limit_ll: Option<f64>,
}

impl Pen {
    /// 色以外の属性を 1 つでも持つか（監査の要約用）。
    fn has_style(&self) -> bool {
        self.label.is_some()
            || self.unit.is_some()
            || self.decimals.is_some()
            || self.line_style.is_some()
            || self.line_width.is_some()
            || self.marker.is_some()
            || self.marker_size.is_some()
            || self.axis.is_some()
            || self.y_min.is_some()
            || self.y_max.is_some()
            || self.limit_lines.is_some()
            || self.limit_hh.is_some()
            || self.limit_h.is_some()
            || self.limit_l.is_some()
            || self.limit_ll.is_some()
    }
}

/// 表示グループ（読み取りの応答）。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
#[derive(utoipa::ToSchema)]
pub struct DisplayGroup {
    pub id: i64,
    pub name: String,
    /// 束の名前（`None` = 束なし）。
    pub bundle: Option<String>,
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
#[derive(utoipa::ToSchema)]
pub struct DisplayGroupPayload {
    pub name: String,
    /// 束の名前。キーが無い = `None`（作成では束なし、更新では拒否）、`null` =
    /// `Some(None)`（束から外す）。空・空白だけは検証で `None` に正規化する。
    #[serde(default, deserialize_with = "deserialize_present")]
    #[schema(value_type = Option<String>)]
    pub bundle: Option<Option<String>>,
    /// 作成のときだけ使う（省略で末尾）。**更新では無視する**（モジュール doc）。
    #[serde(default)]
    pub sort_order: Option<i64>,
    pub kind: String,
    #[serde(default)]
    #[schema(value_type = Object)]
    pub attributes: Map<String, Value>,
    #[serde(default)]
    pub pens: Vec<PenPayload>,
    /// 楽観ロック（更新のみ）。編集画面が最後に読んだ `revision`。
    #[serde(default)]
    pub expected_revision: Option<i64>,
    /// 未登録の `hubTag` のペンを記録項目として登録する先（#1179 PR5、モジュール
    /// doc）。未登録の `hubTag` が無ければ使わない。
    #[serde(default)]
    pub hub_tag_group: Option<HubTagGroupPayload>,
}

/// ペンの本文。`tagId`（登録済みの記録項目）か `hubTag`（Hub のカタログのタグ、
/// #1179 PR5）の**ちょうど 1 つ**。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
#[derive(utoipa::ToSchema)]
pub struct PenPayload {
    #[serde(default)]
    pub tag_id: Option<i64>,
    #[serde(default)]
    pub hub_tag: Option<HubTagRow>,
    #[serde(default)]
    pub color_slot: Option<i64>,
    /// 属性（#1207・#1231 PR 6a）。キー無し = `null` = 既定。列挙は緩い型で受け、
    /// 誤りは `validate_shape` が `pens[i].lineStyle` などの `field_errors` で返す。
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub unit: Option<String>,
    #[serde(default)]
    pub decimals: Option<i64>,
    #[serde(default)]
    pub line_style: Option<String>,
    #[serde(default)]
    pub line_width: Option<f64>,
    #[serde(default)]
    pub marker: Option<String>,
    #[serde(default)]
    pub marker_size: Option<i64>,
    #[serde(default)]
    pub axis: Option<String>,
    #[serde(default)]
    pub y_min: Option<f64>,
    #[serde(default)]
    pub y_max: Option<f64>,
    /// 管理線の属性（#1207・#1231 PR 7a）。キー無し = `null` = 既定。
    #[serde(default)]
    pub limit_lines: Option<bool>,
    #[serde(default)]
    pub limit_hh: Option<f64>,
    #[serde(default)]
    pub limit_h: Option<f64>,
    #[serde(default)]
    pub limit_l: Option<f64>,
    #[serde(default)]
    pub limit_ll: Option<f64>,
}

impl PenPayload {
    /// 登録済みの記録項目のペン（`tagId` だけ）。
    pub fn tag(tag_id: i64, color_slot: Option<i64>) -> Self {
        Self {
            tag_id: Some(tag_id),
            color_slot,
            ..Self::default()
        }
    }
}

/// `hubTag` の登録先（#1179 PR5）。hub 接続の下の既存の収集グループ
/// （`collectionGroupId`）か新しいグループ（`newGroup`）の**どちらか 1 つ**。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
#[derive(utoipa::ToSchema)]
pub struct HubTagGroupPayload {
    #[serde(default)]
    pub collection_group_id: Option<i64>,
    #[serde(default)]
    pub new_group: Option<NewHubGroup>,
}

/// 作成・更新の応答（REST・MCP・Tauri 共通、#1179 PR5）: [`DisplayGroup`] の
/// 項目に、今回 Hub のタグから登録した記録項目（`registeredTags`、無ければ出さない）を
/// 足した形。GET の応答と同じ項目を持つので、そのまま本文に送り返せる。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
#[derive(utoipa::ToSchema)]
pub struct DisplayGroupSaveResponse {
    #[serde(flatten)]
    pub group: DisplayGroup,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub registered_tags: Vec<Tag>,
}

/// 保存の結果（サービスの戻り値）。`registration` は今回 Hub のタグを登録した
/// ときだけ `Some`（監査の `detail` = `crate::hub_tags::registration_audit_detail`）。
#[derive(Debug, Clone)]
pub struct DisplayGroupSaved {
    pub group: DisplayGroup,
    pub registration: Option<HubRegistrationResult>,
}

impl DisplayGroupSaved {
    /// 今回登録した記録項目（無ければ空）。
    pub fn registered_tags(&self) -> &[Tag] {
        self.registration
            .as_ref()
            .map(|result| result.tags.as_slice())
            .unwrap_or_default()
    }

    /// 応答の形にする。
    pub fn into_response(self) -> DisplayGroupSaveResponse {
        DisplayGroupSaveResponse {
            group: self.group,
            registered_tags: self
                .registration
                .map(|result| result.tags)
                .unwrap_or_default(),
        }
    }
}

/// ペンの `tagId`・`hubTag` が両方ある・どちらも無いときの案内。
pub const PEN_SOURCE_MESSAGE: &str = "tagId か hubTag のどちらか 1 つを指定してください";

/// 並べ替えの本文: 全グループの ID を、新しい並び順で 1 回ずつ。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
#[derive(utoipa::ToSchema)]
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

/// キーがあれば（`null` でも）`Some` に包む。省略は `default` の `None`。
fn deserialize_present<'de, D>(deserializer: D) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer).map(Some)
}

/// 検証を通った定義（保存する形）。
#[derive(Debug, Clone, PartialEq)]
pub struct ValidDisplayGroup {
    /// 前後の空白を除いた名前。
    pub name: String,
    /// 前後の空白を除いた束の名前（空は `None`）。
    pub bundle: Option<String>,
    pub sort_order: Option<i64>,
    pub kind: DisplayKind,
    pub attributes: DisplayAttributes,
    pub pens: Vec<Pen>,
}

/// ペンの属性の検証。誤りは `pens[index].<項目>` の `field_errors` に積む。返す `Pen` の
/// `tag_id`・`color_slot` は呼び出し側が埋める（誤りのある項目は `None`）。
fn validate_pen_style(pen: &PenPayload, index: usize, errors: &mut Vec<FieldError>) -> Pen {
    let mut bad = |name: &str, message: String| {
        errors.push(field_error(format!("pens[{index}].{name}"), message));
    };
    let text = |value: &Option<String>,
                name: &str,
                max: usize,
                bad: &mut dyn FnMut(&str, String)|
     -> Option<String> {
        let trimmed = value.as_deref()?.trim();
        if trimmed.is_empty() {
            return None;
        }
        if trimmed.chars().any(char::is_control) {
            bad(name, "制御文字は使えません".to_string());
            return None;
        }
        if trimmed.chars().count() > max {
            bad(name, format!("{max} 文字以内にしてください"));
            return None;
        }
        Some(trimmed.to_string())
    };
    let label = text(&pen.label, "label", MAX_PEN_LABEL_CHARS, &mut bad);
    let unit = text(&pen.unit, "unit", MAX_PEN_UNIT_CHARS, &mut bad);
    let decimals = pen.decimals.and_then(|d| {
        if (0..=MAX_PEN_DECIMALS).contains(&d) {
            Some(d)
        } else {
            bad(
                "decimals",
                format!("小数の桁は 0〜{MAX_PEN_DECIMALS} にしてください"),
            );
            None
        }
    });
    let line_style = pen.line_style.as_deref().and_then(|v| {
        let parsed = LineStyle::parse(v);
        if parsed.is_none() {
            bad(
                "lineStyle",
                "線種は solid・dashed・dashDot・dashDotDot・none から選んでください".to_string(),
            );
        }
        parsed
    });
    let line_width = pen.line_width.and_then(|w| {
        let (lo, hi) = LINE_WIDTH_RANGE;
        if w.is_finite() && (lo..=hi).contains(&w) && (w * 2.0).fract() == 0.0 {
            Some(w)
        } else {
            bad(
                "lineWidth",
                format!("線幅は {lo}〜{hi} の 0.5 刻みにしてください"),
            );
            None
        }
    });
    let marker = pen.marker.as_deref().and_then(|v| {
        let parsed = Marker::parse(v);
        if parsed.is_none() {
            bad(
                "marker",
                "マーカーは none・circle・square・triangle・diamond から選んでください".to_string(),
            );
        }
        parsed
    });
    let marker_size = pen.marker_size.and_then(|s| {
        let (lo, hi) = MARKER_SIZE_RANGE;
        if (lo..=hi).contains(&s) {
            Some(s)
        } else {
            bad(
                "markerSize",
                format!("マーカーの大きさは {lo}〜{hi} にしてください"),
            );
            None
        }
    });
    let axis = pen.axis.as_deref().and_then(|v| {
        let parsed = Axis::parse(v);
        if parsed.is_none() {
            bad("axis", "縦軸は left・right から選んでください".to_string());
        }
        parsed
    });
    let finite = |value: Option<f64>, name: &str, bad: &mut dyn FnMut(&str, String)| {
        value.and_then(|v| {
            if v.is_finite() {
                Some(v)
            } else {
                bad(name, "有限の数にしてください".to_string());
                None
            }
        })
    };
    let y_min = finite(pen.y_min, "yMin", &mut bad);
    let y_max = finite(pen.y_max, "yMax", &mut bad);
    if let (Some(lo), Some(hi)) = (y_min, y_max) {
        if lo >= hi {
            bad("yMax", "縦軸の最大は最小より大きくしてください".to_string());
        }
    }
    let limit_hh = finite(pen.limit_hh, "limitHh", &mut bad);
    let limit_h = finite(pen.limit_h, "limitH", &mut bad);
    let limit_l = finite(pen.limit_l, "limitL", &mut bad);
    let limit_ll = finite(pen.limit_ll, "limitLl", &mut bad);
    // LL <= L <= H <= HH（設定したものだけ比べる）。`tag_thresholds::validate_threshold_order`
    // と同じ決まり（あちらは非公開で欄の名前が thresholdXx なので、ここで limitXx の名前で書く）。
    // 欄の付け方も同じで、並びの後ろの側（大きいはずの側）の欄に付ける。
    let chain = [
        ("limitLl", limit_ll),
        ("limitL", limit_l),
        ("limitH", limit_h),
        ("limitHh", limit_hh),
    ];
    let mut prev: Option<(&str, f64)> = None;
    for (name, value) in chain {
        let Some(value) = value else { continue };
        if let Some((prev_name, prev_value)) = prev {
            if prev_value > value {
                bad(name, format!("{prev_name} 以上の値にしてください"));
                break;
            }
        }
        prev = Some((name, value));
    }
    Pen {
        tag_id: 0,
        color_slot: None,
        label,
        unit,
        decimals,
        line_style,
        line_width,
        marker,
        marker_size,
        axis,
        y_min,
        y_max,
        limit_lines: pen.limit_lines,
        limit_hh,
        limit_h,
        limit_l,
        limit_ll,
    }
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
/// 使える（総当たりのテストのため）。`hubTag` のペン（まだ記録項目に解決して
/// いない）は `pens[i].hubTag` の誤りにする - 解決は保存の中で行う（モジュール doc）。
pub fn validate_shape(payload: &DisplayGroupPayload) -> Result<ValidDisplayGroup, Vec<FieldError>> {
    let mut errors = Vec::new();
    match check_shape(payload, true, &mut errors) {
        Some(valid) if errors.is_empty() => Ok(valid),
        _ => Err(errors),
    }
}

/// 形の検証の本体。`report_unresolved` が偽なら、`hubTag` のペンは誤りにせず
/// 飛ばす（保存の中で、解決の前に形の誤りだけを集めるため）。戻りは誤りも
/// 未解決のペンも無いときだけ `Some`。
fn check_shape(
    payload: &DisplayGroupPayload,
    report_unresolved: bool,
    errors: &mut Vec<FieldError>,
) -> Option<ValidDisplayGroup> {
    let errors_before = errors.len();
    let mut unresolved = false;

    let name = payload.name.trim().to_string();
    if name.is_empty() {
        errors.push(field_error("name", "名前を入力してください"));
    } else if name.chars().count() > MAX_NAME_CHARS {
        errors.push(field_error(
            "name",
            format!("名前は {MAX_NAME_CHARS} 文字以内にしてください"),
        ));
    }

    let bundle = payload
        .bundle
        .as_ref()
        .and_then(|bundle| bundle.as_deref())
        .map(str::trim)
        .filter(|bundle| !bundle.is_empty())
        .map(str::to_string);
    if let Some(bundle) = &bundle {
        if bundle == NO_BUNDLE_LABEL {
            errors.push(field_error(
                "bundle",
                format!("「{NO_BUNDLE_LABEL}」は束の名前に使えません（束に入れないグループの表示名です）。束に入れないときは空にしてください。"),
            ));
        } else if bundle.chars().count() > MAX_BUNDLE_CHARS {
            errors.push(field_error(
                "bundle",
                format!("束は {MAX_BUNDLE_CHARS} 文字以内にしてください"),
            ));
        }
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
        Some(kind) => validate_attributes(kind, &payload.attributes, errors),
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
        let style = validate_pen_style(pen, index, errors);
        match (pen.tag_id, &pen.hub_tag) {
            (Some(tag_id), None) => {
                if !seen.insert(tag_id) {
                    errors.push(field_error(
                        format!("pens[{index}].tagId"),
                        "同じタグを 1 つのグループに 2 回割り当てることはできません",
                    ));
                }
                pens.push(Pen {
                    tag_id,
                    color_slot: pen.color_slot,
                    ..style
                });
            }
            (None, Some(_)) => {
                unresolved = true;
                if report_unresolved {
                    errors.push(field_error(
                        format!("pens[{index}].hubTag"),
                        "Hub のタグは表示グループの保存（作成・更新）のときに記録項目へ解決します",
                    ));
                }
            }
            _ => errors.push(field_error(format!("pens[{index}]"), PEN_SOURCE_MESSAGE)),
        }
        if let Some(slot) = pen.color_slot {
            if !(1..=COLOR_SLOTS).contains(&slot) {
                errors.push(field_error(
                    format!("pens[{index}].colorSlot"),
                    format!("色は 1〜{COLOR_SLOTS} の枠から選んでください"),
                ));
            }
        }
    }

    match kind {
        Some(kind) if errors.len() == errors_before && !unresolved => Some(ValidDisplayGroup {
            name,
            bundle,
            sort_order: payload.sort_order,
            kind,
            attributes,
            pens,
        }),
        _ => None,
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
        // `tagId` だけのペン（それ以外は形の誤り）。
        let (Some(tag_id), None) = (pen.tag_id, &pen.hub_tag) else {
            continue;
        };
        let exists: Option<i64> = sqlx::query_scalar("SELECT id FROM tags WHERE id = ?")
            .bind(tag_id)
            .fetch_optional(&mut *conn)
            .await
            .map_err(banto_storage::storage_error)?;
        if exists.is_none() {
            errors.push(field_error(
                format!("pens[{index}].tagId"),
                format!("タグ（ID {tag_id}）が見つかりません"),
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
        "bundle": group.bundle,
        "kind": group.kind.as_str(),
        "pens": group.pens.len(),
        // 色以外の属性を持つペンの数（値そのものではなく要約）。
        "styledPens": group.pens.iter().filter(|pen| pen.has_style()).count(),
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
            format!("このタグは表示グループ{names}のペンに割り当てられているため削除できません。先に「表示グループ」ページでペンから外してください"),
        )],
    }
}

#[derive(sqlx::FromRow)]
struct GroupRow {
    id: i64,
    name: String,
    bundle: Option<String>,
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
    label: Option<String>,
    unit: Option<String>,
    decimals: Option<i64>,
    line_style: Option<String>,
    line_width: Option<f64>,
    marker: Option<String>,
    marker_size: Option<i64>,
    axis: Option<String>,
    y_min: Option<f64>,
    y_max: Option<f64>,
    limit_lines: Option<i64>,
    limit_hh: Option<f64>,
    limit_h: Option<f64>,
    limit_l: Option<f64>,
    limit_ll: Option<f64>,
}

/// DB の列挙の文字列を読む（CHECK で守っているので、未知の値は壊れた DB）。
fn parse_column<T>(
    value: Option<String>,
    column: &str,
    parse: fn(&str) -> Option<T>,
) -> Result<Option<T>, BantoError> {
    value
        .map(|v| {
            parse(&v).ok_or_else(|| {
                BantoError::Storage(format!(
                    "display_group_pens.{column} に未知の値があります: {v}"
                ))
            })
        })
        .transpose()
}

impl PenRow {
    fn into_pen(self) -> Result<Pen, BantoError> {
        Ok(Pen {
            tag_id: self.tag_id,
            color_slot: self.color_slot,
            label: self.label,
            unit: self.unit,
            decimals: self.decimals,
            line_style: parse_column(self.line_style, "line_style", LineStyle::parse)?,
            line_width: self.line_width,
            marker: parse_column(self.marker, "marker", Marker::parse)?,
            marker_size: self.marker_size,
            axis: parse_column(self.axis, "axis", Axis::parse)?,
            y_min: self.y_min,
            y_max: self.y_max,
            limit_lines: self.limit_lines.map(|v| v != 0),
            limit_hh: self.limit_hh,
            limit_h: self.limit_h,
            limit_l: self.limit_l,
            limit_ll: self.limit_ll,
        })
    }
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
        bundle: row.bundle,
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

    /// 作成（Hub のタグの登録はしない。`hubTag` のペンは登録済みのものだけ解決する）。
    /// 経路（REST・MCP・Tauri）は [`Self::create_with_hub_tags`] を使う。
    pub async fn create(&self, payload: &DisplayGroupPayload) -> Result<DisplayGroup, BantoError> {
        Ok(self.save(None, None, payload).await?.group)
    }

    /// 作成の一括保存（モジュール doc「Hub のタグをペンにする一括保存」）。
    pub async fn create_with_hub_tags(
        &self,
        registrar: &HubTagRegistrar,
        payload: &DisplayGroupPayload,
    ) -> Result<DisplayGroupSaved, BantoError> {
        self.save(Some(registrar), None, payload).await
    }

    /// 内容（名前・種別・属性・ペン）の全項目の置き換え。**`sortOrder` は本文に
    /// あっても無視し、今の並び順を保つ**（Codex レビュー P2-3: 並べ替えは版を
    /// 進めないので、前に読んだ `sortOrder` を載せた本文で並びを書き戻すと、その
    /// 間の並べ替えを黙って取り消してしまう。並びを変える口は [`Self::reorder`]
    /// だけ。拒否ではなく無視にしたのは、GET の応答をそのまま送り返せる往復の形を
    /// 保つため）。
    /// `expectedRevision` が保存されている版と違えば [`revision_conflict_error`]。
    /// Hub のタグの登録はしない（[`Self::create`] と同じ。経路は
    /// [`Self::update_with_hub_tags`]）。
    pub async fn update(
        &self,
        id: i64,
        payload: &DisplayGroupPayload,
    ) -> Result<DisplayGroup, BantoError> {
        Ok(self.save(None, Some(id), payload).await?.group)
    }

    /// 更新の一括保存（[`Self::update`] の規則に、モジュール doc の Hub のタグの
    /// 解決・登録を足したもの）。
    pub async fn update_with_hub_tags(
        &self,
        registrar: &HubTagRegistrar,
        id: i64,
        payload: &DisplayGroupPayload,
    ) -> Result<DisplayGroupSaved, BantoError> {
        self.save(Some(registrar), Some(id), payload).await
    }

    /// 作成・更新の共通の入口: 1 つの `BEGIN IMMEDIATE` で [`save_on`] を行い、
    /// 成功したときだけ commit する（失敗は drop で巻き戻る）。
    async fn save(
        &self,
        registrar: Option<&HubTagRegistrar>,
        target_id: Option<i64>,
        payload: &DisplayGroupPayload,
    ) -> Result<DisplayGroupSaved, BantoError> {
        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(storage)?;
        let saved = save_on(&mut tx, registrar, target_id, payload).await?;
        tx.commit().await.map_err(storage)?;
        Ok(saved)
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
        // #532: 記録計の側のしきい値の行も同じトランザクションで消す（FK は
        // 張っていない - `0102_recorder_tag_settings.sql` の注記）。
        crate::tag_thresholds::delete_for_tag_on(&mut tx, tag_id).await?;
        tx.commit().await.map_err(storage)?;
        Ok(())
    }

    /// この service の pool（#532: `crate::rest::api_router` が、同じ pool の
    /// `TagThresholdService` を組むのに使う。呼び出し元の引数を増やさないため）。
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}

/// 作成・更新の本体（呼び出し側のトランザクションの中。commit しない）。
/// `target_id` が `None` なら作成。`registrar` が `None` なら未登録の Hub のタグは
/// 登録せず `hubTagGroup` の誤りにする。手順はモジュール doc。
pub(crate) async fn save_on(
    conn: &mut SqliteConnection,
    registrar: Option<&HubTagRegistrar>,
    target_id: Option<i64>,
    payload: &DisplayGroupPayload,
) -> Result<DisplayGroupSaved, BantoError> {
    if let Some(id) = target_id {
        let current: Option<i64> =
            sqlx::query_scalar("SELECT revision FROM display_groups WHERE id = ?")
                .bind(id)
                .fetch_optional(&mut *conn)
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
        // 更新は束を必ず指定する（REST・Tauri・MCP の共通の関所）。
        if payload.bundle.is_none() {
            return Err(BantoError::Validation {
                field_errors: vec![field_error(
                    "bundle",
                    "更新では束（bundle）を必ず指定してください。束から外すときは null を送ってください。",
                )],
            });
        }
    }

    let (resolved, registration) = resolve_hub_tags(conn, registrar, payload).await?;
    let valid = validate_display_group(conn, &resolved, target_id)
        .await
        .map_err(|err| hub_pen_fields(err, payload))?;
    let attributes = serde_json::to_string(&valid.attributes)
        .map_err(|err| BantoError::Other(err.to_string()))?;
    let id = match target_id {
        None => {
            let sort_order = match valid.sort_order {
                Some(order) => order,
                None => end_sort_order(conn).await?,
            };
            sqlx::query_scalar(
                "INSERT INTO display_groups (name, bundle, sort_order, kind, attributes) \
                 VALUES (?, ?, ?, ?, ?) RETURNING id",
            )
            .bind(&valid.name)
            .bind(&valid.bundle)
            .bind(sort_order)
            .bind(valid.kind.as_str())
            .bind(&attributes)
            .fetch_one(&mut *conn)
            .await
            .map_err(map_write_error)?
        }
        Some(id) => {
            sqlx::query(
                "UPDATE display_groups SET name = ?, bundle = ?, kind = ?, attributes = ?, \
                 revision = revision + 1, updated_at = datetime('now') WHERE id = ?",
            )
            .bind(&valid.name)
            .bind(&valid.bundle)
            .bind(valid.kind.as_str())
            .bind(&attributes)
            .bind(id)
            .execute(&mut *conn)
            .await
            .map_err(map_write_error)?;
            sqlx::query("DELETE FROM display_group_pens WHERE group_id = ?")
                .bind(id)
                .execute(&mut *conn)
                .await
                .map_err(storage)?;
            id
        }
    };
    insert_pens(conn, id, &valid.pens).await?;
    let group = get_on(conn, id, None).await?;
    Ok(DisplayGroupSaved {
        group,
        registration,
    })
}

/// `hubTagGroup` の誤りのフィールド名。
pub const HUB_TAG_GROUP_FIELD: &str = "hubTagGroup";

/// `hubTag` のペンを記録項目の id に解決する（必要なら登録する。モジュール doc の
/// 2・3）。`hubTag` のペンが無ければ本文をそのまま返す。戻りの本文は全部のペンが
/// `tagId` だけの形。誤りは形の誤り（[`check_shape`]）と一緒に `Validation` で返す。
async fn resolve_hub_tags<'a>(
    conn: &mut SqliteConnection,
    registrar: Option<&HubTagRegistrar>,
    payload: &'a DisplayGroupPayload,
) -> Result<(Cow<'a, DisplayGroupPayload>, Option<HubRegistrationResult>), BantoError> {
    if payload.pens.iter().all(|pen| pen.hub_tag.is_none()) {
        return Ok((Cow::Borrowed(payload), None));
    }
    let mut errors = Vec::new();
    // 形の誤りを先に集める（登録で落ちても一緒に返す）。タグの存在は解決の後。
    check_shape(payload, false, &mut errors);

    let registered = registered_tag_ids_tx(conn).await?;
    let mut resolved: Vec<Option<i64>> = vec![None; payload.pens.len()];
    let mut seen: HashSet<String> = HashSet::new();
    // 登録する行と、その行のペンの位置（同じ並び）。
    let mut rows: Vec<HubTagRow> = Vec::new();
    let mut row_pens: Vec<usize> = Vec::new();
    for (index, pen) in payload.pens.iter().enumerate() {
        let (None, Some(hub_tag)) = (pen.tag_id, &pen.hub_tag) else {
            continue;
        };
        let external_name = hub_tag.external_name.trim();
        if !external_name.is_empty() {
            if !seen.insert(external_name.to_owned()) {
                errors.push(field_error(
                    format!("pens[{index}].hubTag.externalName"),
                    "同じタグを 1 つのグループに 2 回割り当てることはできません",
                ));
                continue;
            }
            if let Some(&tag_id) = registered.get(external_name) {
                // 登録済み: 参照するだけ（二重登録しない）。
                resolved[index] = Some(tag_id);
                continue;
            }
        }
        // 空の外部名も登録の事前検証に任せる（行の誤りとして返る）。
        rows.push(hub_tag.clone());
        row_pens.push(index);
    }

    let mut registration = None;
    if !rows.is_empty() {
        match (registration_target(payload), registrar) {
            (Err(error), _) => errors.push(error),
            (Ok(_), None) => errors.push(field_error(
                HUB_TAG_GROUP_FIELD,
                "この経路では Hub のタグを記録項目として登録できません",
            )),
            (Ok((collection_group_id, new_group)), Some(registrar)) => {
                let request = HubTagRegistration {
                    collection_group_id,
                    new_group,
                    tags: rows,
                };
                let outcome = register_in_tx(
                    conn,
                    &registrar.plc_connections,
                    &registrar.collection_groups,
                    &registrar.tags,
                    &request,
                )
                .await;
                match outcome {
                    Ok(result) if result.ok => {
                        let by_name: HashMap<&str, usize> = request
                            .tags
                            .iter()
                            .zip(&row_pens)
                            .map(|(row, &pen)| (row.external_name.trim(), pen))
                            .collect();
                        for tag in &result.tags {
                            if let Some(&pen) = by_name.get(tag.address.trim()) {
                                resolved[pen] = Some(tag.id);
                            }
                        }
                        registration = Some(result);
                    }
                    Ok(result) => {
                        errors.extend(result.field_errors.into_iter().map(|error| {
                            field_error(hub_tag_group_field(&error.field), error.message)
                        }));
                        for row_error in result.row_errors {
                            let Some(&pen) = row_pens.get(row_error.index) else {
                                continue;
                            };
                            errors.extend(row_error.field_errors.into_iter().map(|error| {
                                field_error(
                                    format!(
                                        "pens[{pen}].hubTag.{}",
                                        hub_tag_row_field(&error.field)
                                    ),
                                    error.message,
                                )
                            }));
                        }
                    }
                    Err(BantoError::Validation { field_errors }) => {
                        errors.extend(field_errors.into_iter().map(|error| {
                            field_error(hub_tag_group_field(&error.field), error.message)
                        }));
                    }
                    Err(other) => return Err(other),
                }
            }
        }
    }

    if !errors.is_empty() {
        return Err(BantoError::Validation {
            field_errors: errors,
        });
    }
    let mut out = payload.clone();
    for (pen, tag_id) in out.pens.iter_mut().zip(resolved) {
        if pen.hub_tag.is_some() {
            let Some(tag_id) = tag_id else {
                return Err(BantoError::Other(
                    "Hub のタグを記録項目に解決できませんでした".to_owned(),
                ));
            };
            pen.tag_id = Some(tag_id);
            pen.hub_tag = None;
        }
    }
    out.hub_tag_group = None;
    Ok((Cow::Owned(out), registration))
}

/// `hubTagGroup` を登録先（`collectionGroupId` か `newGroup` のちょうど 1 つ）にする。
fn registration_target(
    payload: &DisplayGroupPayload,
) -> Result<(Option<i64>, Option<NewHubGroup>), FieldError> {
    let message = "Hub のタグを記録項目として登録する先（hubTagGroup の collectionGroupId か newGroup のどちらか 1 つ）を指定してください";
    match &payload.hub_tag_group {
        Some(HubTagGroupPayload {
            collection_group_id: Some(id),
            new_group: None,
        }) => Ok((Some(*id), None)),
        Some(HubTagGroupPayload {
            collection_group_id: None,
            new_group: Some(group),
        }) => Ok((None, Some(group.clone()))),
        _ => Err(field_error(HUB_TAG_GROUP_FIELD, message)),
    }
}

/// 登録の行に属さない誤り（`crate::hub_tags` のダイアログの欄の名前）を、本文の
/// `hubTagGroup` の位置に載せ替える。
fn hub_tag_group_field(field: &str) -> String {
    match field {
        "collectionGroupId" => "hubTagGroup.collectionGroupId".to_owned(),
        "newGroupName" => "hubTagGroup.newGroup.name".to_owned(),
        "newGroupPeriodMs" => "hubTagGroup.newGroup.periodMs".to_owned(),
        _ => HUB_TAG_GROUP_FIELD.to_owned(),
    }
}

/// 登録の行の誤りのフィールド名を `hubTag` の項目名にする（タグの `address` は
/// 外部名そのもの）。
fn hub_tag_row_field(field: &str) -> &str {
    match field {
        "address" => "externalName",
        other => other,
    }
}

/// 解決した本文の検証で出た `pens[i].tagId` の誤りを、もとが `hubTag` のペンなら
/// `pens[i].hubTag.externalName` に付け替える（利用者が送った項目の名前で返す）。
fn hub_pen_fields(err: BantoError, payload: &DisplayGroupPayload) -> BantoError {
    let BantoError::Validation { field_errors } = err else {
        return err;
    };
    let field_errors = field_errors
        .into_iter()
        .map(|mut error| {
            for (index, pen) in payload.pens.iter().enumerate() {
                if pen.hub_tag.is_some() && error.field == format!("pens[{index}].tagId") {
                    error.field = format!("pens[{index}].hubTag.externalName");
                }
            }
            error
        })
        .collect();
    BantoError::Validation { field_errors }
}

async fn insert_pens(
    conn: &mut SqliteConnection,
    group_id: i64,
    pens: &[Pen],
) -> Result<(), BantoError> {
    for (position, pen) in pens.iter().enumerate() {
        sqlx::query(
            "INSERT INTO display_group_pens (group_id, position, tag_id, color_slot, label, unit, \
             decimals, line_style, line_width, marker, marker_size, axis, y_min, y_max, limit_lines, limit_hh, limit_h, limit_l, limit_ll) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(group_id)
        .bind(position as i64)
        .bind(pen.tag_id)
        .bind(pen.color_slot)
        .bind(&pen.label)
        .bind(&pen.unit)
        .bind(pen.decimals)
        .bind(pen.line_style.map(LineStyle::as_str))
        .bind(pen.line_width)
        .bind(pen.marker.map(Marker::as_str))
        .bind(pen.marker_size)
        .bind(pen.axis.map(Axis::as_str))
        .bind(pen.y_min)
        .bind(pen.y_max)
        .bind(pen.limit_lines.map(i64::from))
        .bind(pen.limit_hh)
        .bind(pen.limit_h)
        .bind(pen.limit_l)
        .bind(pen.limit_ll)
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
        "SELECT id, name, bundle, sort_order, kind, attributes, revision FROM display_groups \
         ORDER BY sort_order, id",
    )
    .fetch_all(&mut *conn)
    .await
    .map_err(storage)?;
    if let Some(pause) = pause {
        pause.hold().await;
    }
    let pen_rows: Vec<PenRow> = sqlx::query_as("SELECT group_id, tag_id, color_slot, label, unit, decimals, line_style, line_width, marker, 
         marker_size, axis, y_min, y_max, limit_lines, limit_hh, limit_h, limit_l, limit_ll FROM display_group_pens ORDER BY group_id, position")
    .fetch_all(&mut *conn)
    .await
    .map_err(storage)?;
    let mut pens_by_group: BTreeMap<i64, Vec<Pen>> = BTreeMap::new();
    for pen in pen_rows {
        let group_id = pen.group_id;
        pens_by_group
            .entry(group_id)
            .or_default()
            .push(pen.into_pen()?);
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
        "SELECT id, name, bundle, sort_order, kind, attributes, revision FROM display_groups WHERE id = ?",
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
    let pens: Vec<PenRow> = sqlx::query_as("SELECT group_id, tag_id, color_slot, label, unit, decimals, line_style, line_width, marker, 
         marker_size, axis, y_min, y_max, limit_lines, limit_hh, limit_h, limit_l, limit_ll FROM display_group_pens WHERE group_id = ? ORDER BY position")
    .bind(id)
    .fetch_all(&mut *conn)
    .await
    .map_err(storage)?;
    into_group(
        row,
        pens.into_iter()
            .map(PenRow::into_pen)
            .collect::<Result<Vec<_>, _>>()?,
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
