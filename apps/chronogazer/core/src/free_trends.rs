//! 自由トレンド（#1207・#1231 PR 8a、docs/chronogazer/trend-design.md §2・§3・§9・§10 の 8a）。
//!
//! 登録されたタグから運転員がペンを選び、線の属性を変え、名前を付けて保存する。**全員で共有する
//! 名前付きのもの**で、利用者ごとにはしない（§2 の 3）。テーブルは
//! `migrations-sqlite/0109_free_trends.sql`（`free_trends`・`free_trend_pens`）。画面は 8b で作る。
//!
//! ## データの形
//!
//! ```json
//! {
//!   "id": 1, "revision": 2, "name": "1 号炉の温度",
//!   "timeWindowSec": 600,
//!   "pens": [ { "tagId": 12, "colorSlot": null, "label": null, ... } ],
//!   "createdBy": "operator1", "createdAt": "2026-11-20 01:02:03",
//!   "updatedBy": "operator1", "updatedAt": "2026-11-20 01:02:03",
//!   "canEdit": true
//! }
//! ```
//!
//! - `pens`: 最大 [`MAX_PENS_PER_FREE_TREND`]（16）。ペンの型と属性の検証は**表示グループと同じ**
//!   （応答は [`crate::display_groups::Pen`]、属性の検証は `display_groups::validate_pen_style`、色の枠は
//!   `display_groups::check_color_slot`、DB の列は `display_groups::PEN_COLUMNS`）。違いはペンの指し方
//!   だけで、自由トレンドは**登録済みのタグ（`tagId`）だけ**を受ける（Hub のカタログのタグ `hubTag` は
//!   受けない。本文の型 [`FreeTrendPenPayload`] にその項目が無い）。属性はキー無し = `null` = 既定、
//!   ペンの並びは丸ごと置き換え、応答は全部のキーを出す。同じタグの重複ペンは断る。
//! - `timeWindowSec`: 表示グループのトレンドの [`TREND_TIME_WINDOWS_SEC`] から 1 つ。キー無し・`null` は
//!   [`DEFAULT_TREND_TIME_WINDOW_SEC`]。
//! - `name`: 前後の空白を除いて空でなく [`MAX_NAME_CHARS`]（64）文字以内、ほかと重ならない。
//! - 保存は全体で [`MAX_FREE_TRENDS`]（200）まで（§9 Q2）。
//! - `createdBy`・`updatedBy`: 作った人・最後に変えた人（アカウント名、またはこの PC からの接続の
//!   `localDesktop`）。`canEdit`: **今の主体が**この自由トレンドを変えられるか（画面がボタンを出し分ける。
//!   要求ごとに主体から求めるので、同じ行でも主体によって違う）。
//!
//! 誤りは `field_errors` に全部並べて返す（1 件目で止めない）。作成・更新は `BEGIN IMMEDIATE` の中で
//! 「権限 → 版 → 検証 → 書き込み」を行う（上限・名前・タグの存在・作った人が保存の時点でも成り立つ）。
//! 版の食い違いは表示グループと同じ形（[`revision_conflict_error`]、REST は 409）。
//!
//! ## 権限（2026-11-20 オーナー決定、§9 Q1・Q7）
//!
//! 主体は経路（REST・Tauri）の主体を [`FreeTrendActor`] に写して渡す。
//!
//! | 主体 | 開く（一覧・1 件） | 作成 | 変更・削除 |
//! |---|---|---|---|
//! | アカウントのログイン（viewer 以上） | 可 | 可 | 作った人か editor 以上 |
//! | この PC からの接続（`localDesktop` の grant） | 可 | 可（作った人は `localDesktop`） | `localDesktop` の作ったものか、grant の役割が editor 以上 |
//! | ログイン不要モード（デスクトップの合成のセッション） | 可 | 不可 | 不可 |
//! | 閲覧公開（`publicViewer` の grant）・ほかの grant | 可 | 不可 | 不可 |
//! | API キー（`read`・`config`） | 可 | 不可 | 不可 |
//!
//! - ログインなしのセッションのうち、作れるのは `localDesktop` だけ（だれが作ったかを残せないため。
//!   `localDesktop` はこの PC の操作として `localDesktop` の名前で残す）。
//! - API キーは読むだけ（`config` のキーでも書けない）。MCP のツールは足さない（後で）。
//! - **見分け方は名前ではなくセッションの出どころ**: REST は banto の `AuthenticatedSession::grant`
//!   （`None` = アカウント、`Some("localDesktop")`、それ以外の grant）、Tauri はデスクトップの
//!   セッションの種類（アカウントか、ログイン不要モードの合成のセッションか）。
//! - **作った人はアカウントの ID で見分ける**（2026-11-20 オーナーの P2）: 作った人は
//!   `created_by_kind`（`account`・`localDesktop`）と `created_by_user_id`（account のときの
//!   `users.id`）で持ち、account は ID の一致、localDesktop は種類の一致で同じ人とする。`created_by` は
//!   表示用の当時の名前で、判定に使わない。名前で比べると、予約名（`crate::local_grant::RESERVED_USERNAMES`）
//!   と同じ名前の古いアカウントや、消したアカウントと同じ名前で作り直したアカウントが、前の人の物を
//!   自分の物にできてしまう。
//! - アカウントの削除（banto の `UsersService`）は `users` の行を消す（論理削除ではない）。`users.id` は
//!   AUTOINCREMENT なので消した ID は再利用されず、消したアカウントの物はだれの物でもなくなる（変えられる
//!   のは editor 以上だけ）。`created_by_user_id` に FK は張らない（0109 の注記）。
//! - 主体の ID: Tauri はアカウントのセッションの `UserIdentity.id` をそのまま渡す。REST のセッションは
//!   名前（`identity.id`）しか持たないので、保存（・読み取り）のトランザクションの中で `users` から
//!   名前 → ID を引く（[`FreeTrendActor::Account`] の `user_id: None`）。引けなければ書けない主体として
//!   扱う（作成・変更・削除は `Forbidden`、`canEdit` は false）。
//! - 断ったときは経路の側が `denied` を監査して 403（`Forbidden`）にする。サービスは権限が無いとき
//!   `BantoError::Forbidden` を返すだけ（作った人の確認は保存と同じトランザクションの中）。
//!
//! ## 参照されているタグの削除
//!
//! ペンはタグを ID で参照する（FK は張らない。0101 の注記）。ペンに割り当てられたタグの削除は、
//! 表示グループと一緒に `display_groups::DisplayGroupService::delete_tag_unless_referenced` が
//! [`referrers_on`] で確かめて断る（欄 [`TAG_REFERENCED_FIELD`]、自由トレンドの名前を並べる）。

use std::collections::{BTreeMap, HashSet};
use std::str::FromStr;

use banto_core::{BantoError, FieldError};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{SqliteConnection, SqlitePool};

use crate::display_groups::{
    bind_pen_columns, check_color_slot, field_error, validate_pen_style, Pen, PenColumnsRow,
    PenPayload, DEFAULT_TREND_TIME_WINDOW_SEC, MAX_PENS_PER_GROUP, PEN_COLUMNS,
    TREND_TIME_WINDOWS_SEC,
};
use crate::local_grant::LOCAL_GRANT_ACTOR_ID;
use crate::users::Role;

/// 監査の `resource`（REST・Tauri 共通）。
pub const AUDIT_RESOURCE: &str = "free_trends";

/// 保存できる自由トレンドの数（全体、§9 Q2）。
pub const MAX_FREE_TRENDS: usize = 200;

/// 1 つの自由トレンドのペンの上限（表示グループと同じ 16）。
pub const MAX_PENS_PER_FREE_TREND: usize = MAX_PENS_PER_GROUP;

/// 名前の上限（文字数）。
pub const MAX_NAME_CHARS: usize = 64;

/// 版の食い違いの案内。
pub const REVISION_CONFLICT_MESSAGE: &str = "他の人（または別の画面）がこの自由トレンドを先に更新しました。一覧を再読み込みしてから、もう一度編集して保存してください。";

/// タグの削除を断るときの欄。
pub const TAG_REFERENCED_FIELD: &str = "freeTrends";

/// 上限に達したときの欄。
pub const LIMIT_FIELD: &str = "freeTrends";

/// 作った人の種類（DB の `created_by_kind`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OwnerKind {
    Account,
    LocalDesktop,
}

impl OwnerKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Account => "account",
            Self::LocalDesktop => "localDesktop",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "account" => Some(Self::Account),
            "localDesktop" => Some(Self::LocalDesktop),
            _ => None,
        }
    }
}

/// 自由トレンドに対する主体（モジュール doc「権限」）。経路の主体をここに写す。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FreeTrendActor {
    /// アカウントのログイン（REST の grant の無いセッション、Tauri のアカウントのセッション）。
    /// `user_id` は `users.id`。`None` は保存のトランザクションの中で `username` から引く（REST の
    /// セッションは名前しか持たない。モジュール doc「権限」）。`username` は記録の表示用の名前。
    Account {
        user_id: Option<i64>,
        username: String,
        role: Role,
    },
    /// この PC からの接続（REST の `localDesktop` の grant のセッション）。`role` は grant の役割。
    LocalDesktop { role: Role },
    /// 開くだけの主体（ログイン不要モード・閲覧公開・ほかの grant・API キー）。
    ReadOnly,
}

impl FreeTrendActor {
    /// REST のセッションの主体から（`grant` は `AuthenticatedSession::grant` の kind。`None` = アカウント）。
    /// 役割が読めないセッションは開くだけにする（閉じる側に倒す）。
    pub fn from_session(username: &str, role: &str, grant: Option<&str>) -> Self {
        let Ok(role) = Role::from_str(role) else {
            return Self::ReadOnly;
        };
        match grant {
            None => Self::Account {
                user_id: None,
                username: username.to_string(),
                role,
            },
            Some(kind) if kind == crate::local_grant::LOCAL_DESKTOP_GRANT_KIND => {
                Self::LocalDesktop { role }
            }
            Some(_) => Self::ReadOnly,
        }
    }

    fn role(&self) -> Option<Role> {
        match self {
            Self::Account { role, .. } | Self::LocalDesktop { role } => Some(*role),
            Self::ReadOnly => None,
        }
    }

    /// 作れる種類・役割か（viewer 以上のアカウント・`localDesktop`）。REST のアカウントが今も
    /// `users` にあるかは、保存のトランザクションの中で確かめる（[`writer_on`]）。
    pub fn can_create(&self) -> bool {
        !matches!(self, Self::ReadOnly)
            && self.role().is_some_and(|role| role.at_least(Role::Viewer))
    }
}

/// 作った人（DB の `created_by_kind`・`created_by_user_id`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Owner {
    /// `users.id`。
    Account(i64),
    LocalDesktop,
}

impl Owner {
    fn kind(self) -> OwnerKind {
        match self {
            Self::Account(_) => OwnerKind::Account,
            Self::LocalDesktop => OwnerKind::LocalDesktop,
        }
    }

    fn user_id(self) -> Option<i64> {
        match self {
            Self::Account(id) => Some(id),
            Self::LocalDesktop => None,
        }
    }
}

/// トランザクションの中で確かめた主体（[`writer_on`]）。作った人の判定はこれで行う。
#[derive(Debug, Clone, Copy)]
enum Writer<'a> {
    /// 書ける主体: 作った人としての ID・記録に残す名前・役割。
    Some {
        owner: Owner,
        name: &'a str,
        role: Role,
    },
    /// 書けない主体（開くだけ、または REST の名前が `users` に無い）。
    None,
}

impl Writer<'_> {
    fn can_create(&self) -> bool {
        matches!(self, Self::Some { role, .. } if role.at_least(Role::Viewer))
    }

    /// 作った人が `owner` の自由トレンドを変えられるか（作った人か editor 以上）。
    fn can_edit(&self, owner: Owner) -> bool {
        match self {
            Self::Some {
                owner: me, role, ..
            } => role.at_least(Role::Viewer) && (role.at_least(Role::Editor) || *me == owner),
            Self::None => false,
        }
    }
}

/// 主体を確かめる（モジュール doc「権限」）。REST のアカウント（`user_id: None`）は `users` から名前 →
/// ID を引き、無ければ書けない主体にする。呼ぶ側は同じトランザクションで保存・読み取りを行う。
async fn writer_on<'a>(
    conn: &mut SqliteConnection,
    actor: &'a FreeTrendActor,
) -> Result<Writer<'a>, BantoError> {
    Ok(match actor {
        FreeTrendActor::Account {
            user_id,
            username,
            role,
        } => {
            let id = match user_id {
                Some(id) => Some(*id),
                None => sqlx::query_scalar("SELECT id FROM users WHERE username = ?")
                    .bind(username)
                    .fetch_optional(&mut *conn)
                    .await
                    .map_err(storage)?,
            };
            match id {
                Some(id) => Writer::Some {
                    owner: Owner::Account(id),
                    name: username,
                    role: *role,
                },
                None => Writer::None,
            }
        }
        FreeTrendActor::LocalDesktop { role } => Writer::Some {
            owner: Owner::LocalDesktop,
            name: LOCAL_GRANT_ACTOR_ID,
            role: *role,
        },
        FreeTrendActor::ReadOnly => Writer::None,
    })
}

/// 自由トレンド（読み取りの応答）。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
#[derive(utoipa::ToSchema)]
pub struct FreeTrend {
    pub id: i64,
    pub name: String,
    /// 時間窓（秒）。
    pub time_window_sec: i64,
    pub pens: Vec<Pen>,
    pub revision: i64,
    /// 作った人（アカウント名、またはこの PC からの接続の `localDesktop`）。
    pub created_by: String,
    /// 作った日時（UTC、`YYYY-MM-DD HH:MM:SS`）。
    pub created_at: String,
    /// 最後に変えた人。
    pub updated_by: String,
    /// 最後に変えた日時（UTC、`YYYY-MM-DD HH:MM:SS`）。
    pub updated_at: String,
    /// 今の主体がこの自由トレンドを変えられるか（変更・削除。モジュール doc「権限」）。
    pub can_edit: bool,
}

/// 作成・更新の本文（camelCase）。`timeWindowSec` を緩い型で受けるのは、誤りを serde の拒否
/// （axum 既定の平文の 422）ではなく `field_errors` で返すため（表示グループの `attributes` と同じ）。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
#[derive(utoipa::ToSchema)]
pub struct FreeTrendPayload {
    pub name: String,
    /// 時間窓（秒）。キー無し・`null` は既定（600）。
    #[serde(default)]
    #[schema(value_type = Option<i64>)]
    pub time_window_sec: Option<Value>,
    #[serde(default)]
    pub pens: Vec<FreeTrendPenPayload>,
    /// 楽観ロック（更新のみ）。編集画面が最後に読んだ `revision`。
    #[serde(default)]
    pub expected_revision: Option<i64>,
}

/// ペンの本文。登録済みのタグ（`tagId`）だけ（`hubTag` は受けない）。属性は表示グループの
/// `PenPayload` と同じ項目・同じ意味（キー無し = `null` = 既定）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
#[derive(utoipa::ToSchema)]
pub struct FreeTrendPenPayload {
    #[serde(default)]
    pub tag_id: Option<i64>,
    #[serde(default)]
    pub color_slot: Option<i64>,
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
    #[serde(default)]
    pub bit_style: Option<String>,
    #[serde(default)]
    pub bit_band_height: Option<i64>,
}

impl FreeTrendPenPayload {
    /// タグだけのペン（属性は既定）。
    pub fn tag(tag_id: i64) -> Self {
        Self {
            tag_id: Some(tag_id),
            ..Self::default()
        }
    }

    /// 表示グループのペンの本文の形にする（属性の検証 `validate_pen_style` を共有するため）。
    fn as_pen_payload(&self) -> PenPayload {
        PenPayload {
            tag_id: self.tag_id,
            hub_tag: None,
            color_slot: self.color_slot,
            label: self.label.clone(),
            unit: self.unit.clone(),
            decimals: self.decimals,
            line_style: self.line_style.clone(),
            line_width: self.line_width,
            marker: self.marker.clone(),
            marker_size: self.marker_size,
            axis: self.axis.clone(),
            y_min: self.y_min,
            y_max: self.y_max,
            limit_lines: self.limit_lines,
            limit_hh: self.limit_hh,
            limit_h: self.limit_h,
            limit_l: self.limit_l,
            limit_ll: self.limit_ll,
            bit_style: self.bit_style.clone(),
            bit_band_height: self.bit_band_height,
        }
    }
}

/// 検証を通った定義（保存する形）。
#[derive(Debug, Clone, PartialEq)]
pub struct ValidFreeTrend {
    /// 前後の空白を除いた名前。
    pub name: String,
    pub time_window_sec: i64,
    pub pens: Vec<Pen>,
}

/// 参照元（タグの削除を断るときに並べる）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FreeTrendRef {
    pub id: i64,
    pub name: String,
}

/// 版の食い違いを表すエラー（形は `crate::revision` と共通）。
pub fn revision_conflict_error() -> BantoError {
    crate::revision::revision_conflict_error(REVISION_CONFLICT_MESSAGE)
}

fn time_window_message() -> String {
    format!(
        "時間窓は {} 秒のいずれかです",
        TREND_TIME_WINDOWS_SEC
            .iter()
            .map(|sec| sec.to_string())
            .collect::<Vec<_>>()
            .join(" / ")
    )
}

/// 形の検証（DB を見ない部分）。[`validate_free_trend`] の前半で、単体でも使える。
pub fn validate_shape(payload: &FreeTrendPayload) -> Result<ValidFreeTrend, Vec<FieldError>> {
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

    let time_window_sec = match &payload.time_window_sec {
        None | Some(Value::Null) => DEFAULT_TREND_TIME_WINDOW_SEC,
        Some(value) => match value.as_i64() {
            Some(sec) if TREND_TIME_WINDOWS_SEC.contains(&sec) => sec,
            _ => {
                errors.push(field_error("timeWindowSec", time_window_message()));
                DEFAULT_TREND_TIME_WINDOW_SEC
            }
        },
    };

    if payload.pens.len() > MAX_PENS_PER_FREE_TREND {
        errors.push(field_error(
            "pens",
            format!(
                "ペンは 1 つの自由トレンドに {MAX_PENS_PER_FREE_TREND} 本までです（{} 本あります）",
                payload.pens.len()
            ),
        ));
    }
    let mut seen = HashSet::new();
    let mut pens = Vec::with_capacity(payload.pens.len());
    for (index, pen) in payload.pens.iter().enumerate() {
        let style = validate_pen_style(&pen.as_pen_payload(), index, &mut errors);
        check_color_slot(pen.color_slot, index, &mut errors);
        match pen.tag_id {
            Some(tag_id) => {
                if !seen.insert(tag_id) {
                    errors.push(field_error(
                        format!("pens[{index}].tagId"),
                        "同じタグを 1 つの自由トレンドに 2 回割り当てることはできません",
                    ));
                }
                pens.push(Pen {
                    tag_id,
                    color_slot: pen.color_slot,
                    ..style
                });
            }
            None => errors.push(field_error(
                format!("pens[{index}].tagId"),
                "登録済みのタグ（tagId）を指定してください",
            )),
        }
    }

    if errors.is_empty() {
        Ok(ValidFreeTrend {
            name,
            time_window_sec,
            pens,
        })
    } else {
        Err(errors)
    }
}

/// 自由トレンドの定義を検証する、経路共通の**唯一の**関数。`target_id` は更新する行の ID
/// （作成は `None`）。形（[`validate_shape`]）に加えて、`conn` の上で上限（作成のとき）・名前の
/// 重複（自分は除く）・ペンのタグの存在を確かめ、誤りを全部並べて返す。
pub async fn validate_free_trend(
    conn: &mut SqliteConnection,
    payload: &FreeTrendPayload,
    target_id: Option<i64>,
) -> Result<ValidFreeTrend, BantoError> {
    let shape = validate_shape(payload);
    let mut errors = match &shape {
        Ok(_) => Vec::new(),
        Err(errors) => errors.clone(),
    };

    if target_id.is_none() {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM free_trends")
            .fetch_one(&mut *conn)
            .await
            .map_err(storage)?;
        if count >= MAX_FREE_TRENDS as i64 {
            errors.push(field_error(
                LIMIT_FIELD,
                format!("自由トレンドは {MAX_FREE_TRENDS} 個までです。不要な自由トレンドを削除してから保存してください"),
            ));
        }
    }

    let name = payload.name.trim();
    if !name.is_empty() {
        let clash: Option<i64> =
            sqlx::query_scalar("SELECT id FROM free_trends WHERE name = ? AND id IS NOT ?")
                .bind(name)
                .bind(target_id)
                .fetch_optional(&mut *conn)
                .await
                .map_err(storage)?;
        if clash.is_some() {
            errors.push(field_error("name", "既に使用されています"));
        }
    }

    for (index, pen) in payload.pens.iter().enumerate() {
        let Some(tag_id) = pen.tag_id else { continue };
        let exists: Option<i64> = sqlx::query_scalar("SELECT id FROM tags WHERE id = ?")
            .bind(tag_id)
            .fetch_optional(&mut *conn)
            .await
            .map_err(storage)?;
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

/// 監査の `detail`（REST・Tauri 共通）。値そのものではなく要約だけ。
pub fn audit_detail(trend: &FreeTrend) -> Value {
    json!({
        "name": trend.name,
        "timeWindowSec": trend.time_window_sec,
        "pens": trend.pens.len(),
        "styledPens": trend.pens.iter().filter(|pen| pen.has_style()).count(),
    })
}

/// タグの削除を断るときの誤り（参照元の自由トレンドの名前を並べる）。
pub fn tag_referenced_field_error(referrers: &[FreeTrendRef]) -> FieldError {
    let names = referrers
        .iter()
        .map(|trend| format!("「{}」", trend.name))
        .collect::<Vec<_>>()
        .join("");
    field_error(
        TAG_REFERENCED_FIELD,
        format!("このタグは自由トレンド{names}のペンに割り当てられているため削除できません。先に自由トレンドの画面でペンから外してください"),
    )
}

/// `tag_id` をペンに使っている自由トレンド（名前順）。呼び出し側のトランザクションの中で読む
/// （`display_groups::DisplayGroupService::delete_tag_unless_referenced`）。
pub(crate) async fn referrers_on(
    conn: &mut SqliteConnection,
    tag_id: i64,
) -> Result<Vec<FreeTrendRef>, BantoError> {
    let rows: Vec<(i64, String)> = sqlx::query_as(
        "SELECT t.id, t.name FROM free_trends t WHERE t.id IN \
         (SELECT free_trend_id FROM free_trend_pens WHERE tag_id = ?) ORDER BY t.name, t.id",
    )
    .bind(tag_id)
    .fetch_all(&mut *conn)
    .await
    .map_err(storage)?;
    Ok(rows
        .into_iter()
        .map(|(id, name)| FreeTrendRef { id, name })
        .collect())
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

/// 一意制約違反（名前）を、検証と同じ形のエラーにする（最後の砦）。
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

#[derive(sqlx::FromRow)]
struct TrendRow {
    id: i64,
    name: String,
    time_window_sec: i64,
    revision: i64,
    created_by_kind: String,
    created_by_user_id: Option<i64>,
    created_by: String,
    created_at: String,
    updated_by: String,
    updated_at: String,
}

impl TrendRow {
    fn owner(&self) -> Result<Owner, BantoError> {
        match (
            OwnerKind::parse(&self.created_by_kind),
            self.created_by_user_id,
        ) {
            (Some(OwnerKind::Account), Some(id)) => Ok(Owner::Account(id)),
            (Some(OwnerKind::LocalDesktop), None) => Ok(Owner::LocalDesktop),
            _ => Err(BantoError::Storage(format!(
                "free_trends の作った人が読めません: created_by_kind = {}, created_by_user_id = {:?}",
                self.created_by_kind, self.created_by_user_id
            ))),
        }
    }

    fn into_trend(self, pens: Vec<Pen>, writer: &Writer<'_>) -> Result<FreeTrend, BantoError> {
        let can_edit = writer.can_edit(self.owner()?);
        Ok(FreeTrend {
            id: self.id,
            name: self.name,
            time_window_sec: self.time_window_sec,
            pens,
            revision: self.revision,
            created_by: self.created_by,
            created_at: self.created_at,
            updated_by: self.updated_by,
            updated_at: self.updated_at,
            can_edit,
        })
    }
}

#[derive(sqlx::FromRow)]
struct PenRow {
    free_trend_id: i64,
    #[sqlx(flatten)]
    pen: PenColumnsRow,
}

const PEN_TABLE: &str = "free_trend_pens";

const TREND_COLUMNS: &str = "id, name, time_window_sec, revision, created_by_kind, \
     created_by_user_id, created_by, created_at, updated_by, updated_at";

/// 自由トレンドのサービス（REST・Tauri 共通）。`Clone` で同じ pool を共有する。
#[derive(Clone)]
pub struct FreeTrendService {
    pool: SqlitePool,
}

impl FreeTrendService {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// 全部（名前順）。行とペンは 1 つの読み取りトランザクションで読む（表示グループと同じ理由:
    /// 2 つの SELECT の間に更新が入っても、古い版に新しいペンが混ざらない）。
    pub async fn list(&self, actor: &FreeTrendActor) -> Result<Vec<FreeTrend>, BantoError> {
        let mut tx = self.pool.begin().await.map_err(storage)?;
        let writer = writer_on(&mut tx, actor).await?;
        let rows: Vec<TrendRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {TREND_COLUMNS} FROM free_trends ORDER BY name, id"
        )))
        .fetch_all(&mut *tx)
        .await
        .map_err(storage)?;
        let pen_rows: Vec<PenRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT free_trend_id, {PEN_COLUMNS} FROM {PEN_TABLE} ORDER BY free_trend_id, position"
        )))
        .fetch_all(&mut *tx)
        .await
        .map_err(storage)?;
        tx.commit().await.map_err(storage)?;
        let mut pens_by_trend: BTreeMap<i64, Vec<Pen>> = BTreeMap::new();
        for row in pen_rows {
            pens_by_trend
                .entry(row.free_trend_id)
                .or_default()
                .push(row.pen.into_pen(PEN_TABLE)?);
        }
        rows.into_iter()
            .map(|row| {
                let pens = pens_by_trend.remove(&row.id).unwrap_or_default();
                row.into_trend(pens, &writer)
            })
            .collect()
    }

    /// 1 つ。
    pub async fn get(&self, actor: &FreeTrendActor, id: i64) -> Result<FreeTrend, BantoError> {
        let mut tx = self.pool.begin().await.map_err(storage)?;
        let writer = writer_on(&mut tx, actor).await?;
        let trend = get_on(&mut tx, &writer, id).await?;
        tx.commit().await.map_err(storage)?;
        Ok(trend)
    }

    /// 作成。作れない主体は `Forbidden`（監査は経路の側）。
    pub async fn create(
        &self,
        actor: &FreeTrendActor,
        payload: &FreeTrendPayload,
    ) -> Result<FreeTrend, BantoError> {
        if !actor.can_create() {
            return Err(BantoError::Forbidden);
        }
        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(storage)?;
        let writer = writer_on(&mut tx, actor).await?;
        let Writer::Some {
            owner, name: by, ..
        } = writer
        else {
            return Err(BantoError::Forbidden);
        };
        if !writer.can_create() {
            return Err(BantoError::Forbidden);
        }
        let valid = validate_free_trend(&mut tx, payload, None).await?;
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO free_trends \
             (name, time_window_sec, created_by_kind, created_by_user_id, created_by, updated_by) \
             VALUES (?, ?, ?, ?, ?, ?) RETURNING id",
        )
        .bind(&valid.name)
        .bind(valid.time_window_sec)
        .bind(owner.kind().as_str())
        .bind(owner.user_id())
        .bind(by)
        .bind(by)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_write_error)?;
        insert_pens(&mut tx, id, &valid.pens).await?;
        let trend = get_on(&mut tx, &writer, id).await?;
        tx.commit().await.map_err(storage)?;
        Ok(trend)
    }

    /// 全項目の置き換え。順は「存在 → 権限（作った人か editor 以上）→ 版 → 検証 → 書き込み」で、
    /// 変えられない主体には版の食い違いより先に `Forbidden` を返す。
    pub async fn update(
        &self,
        actor: &FreeTrendActor,
        id: i64,
        payload: &FreeTrendPayload,
    ) -> Result<FreeTrend, BantoError> {
        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(storage)?;
        let row = row_on(&mut tx, id).await?;
        let writer = writer_on(&mut tx, actor).await?;
        let Writer::Some { name: by, .. } = writer else {
            return Err(BantoError::Forbidden);
        };
        if !writer.can_edit(row.owner()?) {
            return Err(BantoError::Forbidden);
        }
        if let Some(expected) = payload.expected_revision {
            if expected != row.revision {
                return Err(revision_conflict_error());
            }
        }
        let valid = validate_free_trend(&mut tx, payload, Some(id)).await?;
        sqlx::query(
            "UPDATE free_trends SET name = ?, time_window_sec = ?, revision = revision + 1, \
             updated_by = ?, updated_at = datetime('now') WHERE id = ?",
        )
        .bind(&valid.name)
        .bind(valid.time_window_sec)
        .bind(by)
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(map_write_error)?;
        sqlx::query("DELETE FROM free_trend_pens WHERE free_trend_id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        insert_pens(&mut tx, id, &valid.pens).await?;
        let trend = get_on(&mut tx, &writer, id).await?;
        tx.commit().await.map_err(storage)?;
        Ok(trend)
    }

    /// 削除（ペンは `ON DELETE CASCADE`）。削除した行を返す（監査の `detail` に名前を残すため）。
    /// 変えられない主体は `Forbidden`。
    pub async fn delete(&self, actor: &FreeTrendActor, id: i64) -> Result<FreeTrend, BantoError> {
        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(storage)?;
        let writer = writer_on(&mut tx, actor).await?;
        let existing = get_on(&mut tx, &writer, id).await?;
        if !existing.can_edit {
            return Err(BantoError::Forbidden);
        }
        sqlx::query("DELETE FROM free_trends WHERE id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        tx.commit().await.map_err(storage)?;
        Ok(existing)
    }
}

async fn row_on(conn: &mut SqliteConnection, id: i64) -> Result<TrendRow, BantoError> {
    let row: Option<TrendRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {TREND_COLUMNS} FROM free_trends WHERE id = ?"
    )))
    .bind(id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(storage)?;
    row.ok_or_else(|| not_found(id))
}

async fn get_on(
    conn: &mut SqliteConnection,
    writer: &Writer<'_>,
    id: i64,
) -> Result<FreeTrend, BantoError> {
    let row = row_on(conn, id).await?;
    let pens: Vec<PenRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT free_trend_id, {PEN_COLUMNS} FROM {PEN_TABLE} WHERE free_trend_id = ? ORDER BY position"
    )))
    .bind(id)
    .fetch_all(&mut *conn)
    .await
    .map_err(storage)?;
    let pens = pens
        .into_iter()
        .map(|row| row.pen.into_pen(PEN_TABLE))
        .collect::<Result<Vec<_>, _>>()?;
    row.into_trend(pens, writer)
}

async fn insert_pens(
    conn: &mut SqliteConnection,
    free_trend_id: i64,
    pens: &[Pen],
) -> Result<(), BantoError> {
    let sql = format!(
        "INSERT INTO {PEN_TABLE} (free_trend_id, position, {PEN_COLUMNS}) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
    );
    for (position, pen) in pens.iter().enumerate() {
        let query = sqlx::query(sqlx::AssertSqlSafe(sql.clone()))
            .bind(free_trend_id)
            .bind(position as i64);
        bind_pen_columns(query, pen)
            .execute(&mut *conn)
            .await
            .map_err(storage)?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests;
