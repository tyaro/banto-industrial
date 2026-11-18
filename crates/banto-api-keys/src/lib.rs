//! 機械クライアント向けの API キー（発行・一覧・失効・削除・一時停止・照合）
//! を、**アプリに依らない部分だけ**まとめた共有 crate。banto-hub（接頭辞
//! `bh`）が使い、ChronoGazer（`cg`）が続く（docs/chronogazer/chronogazer-agent-api-design.md
//! §3.1・#1161）。もとは banto-hub の `apps/banto-hub/core/src/api_keys.rs` に
//! あった物で、移すときに挙動（キーの形式・ハッシュ・表・照合の順・応答）は
//! 変えていない。
//!
//! ## この crate が持つ物と、アプリが持つ物
//!
//! この crate: キーの形式と生成・固定長の読み取り、SHA-256 と定数時間の比較、
//! 照合の順と結果の型（[`ApiKeyLookup`]・[`ApiKeyCheck`]・[`api_key_verdict`]）、
//! 表 `api_keys` への SQL（[`ApiKeysService`]）、拒否の HTTP 応答
//! （[`api_key_rejection_response`]）、監査に載せる主体の表示
//! （[`ApiKeyContext::actor_label`]）。
//!
//! アプリ（[`ApiKeyScopes`] を実装する）: キーの接頭辞、スコープの書式と意味。
//! この crate はスコープを**文字列の並び**として保存・照合するだけで、何を
//! 許すかは判定しない（発行時の構文の確認は [`ApiKeyScopes::validate_scope`]
//! に任せ、判定は照合で得た [`ApiKeyContext::scopes`] をアプリが読む）。
//! ほかにアプリに残す物: 表を作るマイグレーション（列は下の「表」）、一時停止
//! （[`ApiKeysService::trip`]）を付ける条件、監査の書き方、ルートの配線。
//!
//! ## キー形式
//!
//! 平文キーは `{アプリの接頭辞}_{prefix}_{secret}`（banto-hub は `bh_…`）:
//! - `prefix`: 6 バイトの CSPRNG を base64url（パディングなし）で符号化した
//!   8 文字。行の検索キー（`api_keys.prefix`、`UNIQUE`）として平文保存する
//!   — 秘密ではなく識別子。
//! - `secret`: 32 バイトの CSPRNG を base64url（パディングなし）で符号化
//!   した 43 文字。こちらは検証にしか使わず、平文は発行応答で一度だけ
//!   返して DB には保存しない（保存するのはキー全体のハッシュのみ）。
//!
//! base64url の文字集合には `-`/`_` が含まれるため、`prefix`/`secret` の
//! 内部に `_` が現れる可能性がある。したがってキーを **`_` で分割して**
//! パースするのは誤り（区切り文字と本体の文字が衝突しうる）。`parse_key` は
//! アプリの接頭辞と `_` を剥がした後、`prefix` の長さが生成時に必ず 8 文字に
//! なる（6 バイト = 48 ビット = 8 × 6 ビット、base64 は端数なしできっちり
//! 割り切れる）という construction 上の不変条件を使い、**固定長スライスで**
//! パースする（区切り文字を探さない）。
//!
//! ## ハッシュ方式（argon2 ではなく SHA-256 の理由）
//!
//! 利用者のパスワードは argon2id（意図的に低速なハッシュ）で保存している
//! （banto-admin-services の `UsersService`）。API キーの `secret` にはこの
//! 方式を使わない:
//!
//! - argon2 が低速なのは、人間が選ぶパスワードは低エントロピー（辞書攻撃・
//!   総当たりが現実的な探索空間）だからで、**攻撃者側の計算を意図的に
//!   遅くする**ことに意味がある。
//! - `secret` は 32 バイトの CSPRNG（256 ビットのエントロピー）そのもので、
//!   総当たりは非現実的（2^256 通り）。低速化しても攻撃耐性は実質増えない
//!   一方、キーを使う API は**毎リクエスト**照合が走る経路なので、argon2 の
//!   コスト（意図的に数十〜数百 ms オーダーにチューニングされる）は
//!   スループットの実害・DoS 増幅要因にしかならない。
//! - 高エントロピーなランダムシークレットの照合に SHA-256 のような速い
//!   一方向ハッシュを使うのは業界標準的な設計（GitHub Personal Access
//!   Token 等と同型）。`sha2` クレートをこの用途で使う。
//!
//! ## 照合のタイミングについて
//!
//! `constant_time_eq` は固定時間比較を明示的に書いているが、これは
//! 過剰な安全側の慣行に近い: 比較しているのは元の `secret` ではなく
//! **SHA-256 ダイジェスト同士**であり、ダイジェストは一方向性を持つため
//! 「何バイト目まで一致したか」が漏れたとしても元の `secret` の推測には
//! つながらない（`secret` 自体は DB のどこにも保存されていない）。とはいえ
//! コストがほぼゼロなので、早期リターンする単純な `==` より安全側に倒す。
//!
//! ## 照合の順（ハッシュ → 失効 → 一時停止 → 期限）
//!
//! [`ApiKeysService::lookup`] は **ハッシュ一致 → revoked → tripped → expired**
//! の順に見る。ハッシュが合わないときは、そのキーが失効・一時停止・期限切れか
//! どうかに関わらず常に [`ApiKeyLookup::NotFound`] を返す（secret を知らない
//! 者に「この prefix は在り、失効している」などを漏らさない）。
//!
//! - **一時停止**（`tripped_at`）は失効（`revoked_at`、不可逆）とは別の
//!   解除できる状態。付ける条件はアプリが決め（banto-hub は書き込みのレート
//!   超過）、[`ApiKeysService::trip`] で付け、[`ApiKeysService::clear_trip`] で
//!   外す。
//! - **有効期限**（`expires_at`）は既定で無期限（`NULL`）。判定
//!   （[`is_expired`]、`now_ms >= expires_at` で真）は照合の最後に置く。
//!
//! ## 照合できないことと、キーが無効なことを区別する
//!
//! 認証の経路は [`ApiKeysService::lookup`] を直接呼ばず
//! [`ApiKeysService::check`] を通す。`check` は照合に
//! [`API_KEY_CHECK_TIMEOUT`]（5 秒）の上限を掛け、[`classify_lookup`] で
//! 「DB が答えた」（[`ApiKeyCheck::Answered`]）と「照合できなかった」
//! （[`ApiKeyCheck::Unavailable`]、DB エラー・タイムアウト）に分ける。
//! 応答への変換は [`api_key_verdict`] → [`api_key_rejection_response`]:
//! 失効・期限切れ・存在しない = 401、一時停止 = 403 `key_tripped`、照合できない
//! = 500。照合できないときに 401 を返すと、401 を「キーが無効になった」と
//! 解釈するクライアントが一時的な障害でキーを捨てうるため分ける。
//!
//! ## 表
//!
//! 表 `api_keys` はアプリのマイグレーションが作る。列は
//! `id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE,
//! prefix TEXT NOT NULL UNIQUE, key_hash TEXT NOT NULL, scopes TEXT NOT NULL,
//! created_at TEXT NOT NULL DEFAULT (datetime('now')), last_used_at TEXT,
//! revoked_at TEXT, tripped_at TEXT, expires_at TEXT`（banto-hub の
//! `0101_api_keys.sql` と同じ）。`scopes` は JSON の文字列の配列、
//! `last_used_at`/`expires_at` は epoch ミリ秒の 10 進文字列、
//! `created_at`/`revoked_at`/`tripped_at` は `datetime('now')` の文字列。

use std::marker::PhantomData;
use std::time::Duration;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use banto_core::{BantoError, ErrorBody, FieldError};
use banto_server::ApiError;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;

/// API キーの照合を待つ上限。これを超えたら「照合できなかった」
/// （[`ApiKeyCheck::Unavailable`]）として扱う。照合は索引付きの SELECT 1 本
/// なので、5 秒かかる時点で DB は実質的に応答していない。上限が無いと、
/// 接続プールの取得待ち（sqlx の既定で 30 秒）まで要求が止まる。
pub const API_KEY_CHECK_TIMEOUT: Duration = Duration::from_secs(5);

/// `prefix` 部分の文字数（6 バイト → base64url ちょうど 8 文字、端数なし）。
const PREFIX_LEN: usize = 8;
/// last_used_at の更新の間引きの幅（前回の更新から 60 秒以上経ったときだけ
/// 書く）。
const LAST_USED_THROTTLE_MS: i64 = 60_000;

// --- アプリが決めること -------------------------------------------------------

/// アプリが決めること（キーの接頭辞とスコープの書式）。値を持たない印の型に
/// 実装し、[`ApiKeysService`] の型引数に渡す。
pub trait ApiKeyScopes: Send + Sync + 'static {
    /// キーの先頭（`_` を含まない。banto-hub は `"bh"`）。発行するキーは
    /// `{KEY_PREFIX}_{prefix}_{secret}` になり、照合はこの接頭辞のキーだけを
    /// 受ける（ほかは [`ApiKeyLookup::NotFound`]）。
    const KEY_PREFIX: &'static str;

    /// スコープ 1 つの構文の確認（発行時だけ）。`Err` の文言はそのまま
    /// 利用者に見せる（`BantoError::Validation` の `scopes` 欄）。
    fn validate_scope(scope: &str) -> Result<(), String>;
}

// --- 純関数 -------------------------------------------------------------------

/// last_used_at を更新すべきか（前回の更新から 60 秒以上経ったときだけ）。
/// DB/クロックに依存しない純関数として切り出してあるので、実際の時計を
/// 動かさずに単体テストできる。
///
/// `last_used_at_ms` が `None`（一度も使われていない）なら常に `true`。
pub fn should_touch_last_used(now_ms: i64, last_used_at_ms: Option<i64>) -> bool {
    match last_used_at_ms {
        None => true,
        Some(prev) => now_ms.saturating_sub(prev) >= LAST_USED_THROTTLE_MS,
    }
}

/// キーの有効期限切れ判定。`should_touch_last_used` と同じく DB/クロックに
/// 依存しない純関数。呼び出し箇所・判定順は [`ApiKeysService::lookup`] と
/// この crate の doc comment「照合の順」参照。
///
/// `expires_at_ms` が `None`（無期限、既定）なら常に `false`。`Some(e)` の
/// 場合は `now_ms >= e` で真（境界を含む - 期限ちょうどの瞬間は「期限切れ」
/// 側に倒す）。
pub fn is_expired(now_ms: i64, expires_at_ms: Option<i64>) -> bool {
    match expires_at_ms {
        None => false,
        Some(expires_at) => now_ms >= expires_at,
    }
}

/// スコープの並び全体の確認。空なら断り、1 つずつ
/// [`ApiKeyScopes::validate_scope`] に掛ける。[`ApiKeysService::issue`] の
/// 入口で使う。
fn validate_scopes<S: ApiKeyScopes>(scopes: &[String]) -> Result<(), BantoError> {
    if scopes.is_empty() {
        return Err(BantoError::Validation {
            field_errors: vec![FieldError {
                field: "scopes".to_string(),
                message: "少なくとも1つのスコープを指定してください".to_string(),
            }],
        });
    }
    for scope in scopes {
        if let Err(message) = S::validate_scope(scope) {
            return Err(BantoError::Validation {
                field_errors: vec![FieldError {
                    field: "scopes".to_string(),
                    message,
                }],
            });
        }
    }
    Ok(())
}

// --- キー生成・ハッシュ ------------------------------------------------------

/// `byte_len` バイトの CSPRNG を base64url（パディングなし）で符号化する。
fn random_b64(byte_len: usize) -> String {
    let mut buf = vec![0u8; byte_len];
    // getrandom の失敗（OS 側の乱数源が壊れている等）は復旧不能なので
    // panic させる - `password_hash::rand_core::OsRng` 経由の argon2 塩生成
    // が失敗時に panic するのと同じ扱い（banto-admin-services の `users`
    // のハッシュ化参照）。
    getrandom::fill(&mut buf).expect("システム乱数生成器の呼び出しに失敗しました");
    URL_SAFE_NO_PAD.encode(buf)
}

/// 6 バイト → 8 文字（この crate の doc comment 参照: `parse_key` は
/// これがちょうど [`PREFIX_LEN`] 文字になることに依存する）。
fn generate_prefix() -> String {
    random_b64(6)
}

/// 32 バイト（256 ビット）の secret。
fn generate_secret() -> String {
    random_b64(32)
}

fn hash_key(full_key: &str) -> String {
    hex::encode(Sha256::digest(full_key.as_bytes()))
}

/// 固定時間文字列比較（この crate の doc comment「照合のタイミングに
/// ついて」参照）。
fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// `"{app_prefix}_{prefix}_{secret}"` を `(prefix, secret)` に分解する。
/// 区切り文字探索ではなく固定長スライスで行う理由はこの crate の doc comment
/// 「キー形式」参照。接頭辞と `_` がない・長さが足りない・prefix の直後が `_`
/// でない、のいずれかなら `None`（「このアプリのキーとして構文が壊れている」
/// であり、[`ApiKeyLookup::NotFound`] として扱われる）。
fn parse_key<'a>(app_prefix: &str, token: &'a str) -> Option<(&'a str, &'a str)> {
    let rest = token.strip_prefix(app_prefix)?.strip_prefix('_')?;
    if rest.len() <= PREFIX_LEN + 1 {
        return None;
    }
    // `rest.len()` は文字数ではなくバイト数。先頭 8 バイトの途中で文字が
    // 切れる（非 ASCII が混じる）キーは、生成したキーではありえないので
    // 壊れたキーとして断る（`split_at` の panic を避ける）。
    if !rest.is_char_boundary(PREFIX_LEN) {
        return None;
    }
    let (prefix, tail) = rest.split_at(PREFIX_LEN);
    let secret = tail.strip_prefix('_')?;
    if secret.is_empty() {
        return None;
    }
    Some((prefix, secret))
}

// --- 公開の型 ---------------------------------------------------------------

/// 発行直後にのみ平文キーを含めて返す応答。DB には `key`（平文）は一切保存
/// されない（この crate の doc comment 参照）。
#[derive(Debug, Clone)]
pub struct IssuedApiKey {
    pub id: i64,
    pub name: String,
    pub prefix: String,
    pub scopes: Vec<String>,
    /// 平文キー全体（`bh_...` など）。この構造体を捨てたら二度と復元できない
    /// （「key はこの応答限り」）。
    pub key: String,
}

/// 一覧の行、および失効・一時停止などの応答。`key_hash` は含めない
/// （「key_hash は返さない」）。
///
/// `created_at`/`revoked_at`/`tripped_at` は `datetime('now')` の文字列を
/// 素直に返し、`last_used_at`/`expires_at` は保存形式（10 進文字列）を数値化
/// した epoch ミリ秒。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiKeySummary {
    pub id: i64,
    pub name: String,
    pub prefix: String,
    pub scopes: Vec<String>,
    pub created_at: String,
    pub last_used_at: Option<i64>,
    pub revoked_at: Option<String>,
    /// このキーが現在一時停止中か、その日時。`revoked_at` とは別の解除
    /// できる状態 - この crate の doc comment「照合の順」参照。
    pub tripped_at: Option<String>,
    /// 任意の有効期限（epoch ミリ秒）。`None` = 無期限（既定）。期限接近・
    /// 長期未使用などの警告はアプリの画面が計算する（ここでは判定しない）。
    pub expires_at: Option<i64>,
}

/// [`ApiKeysService::delete`] の結果。
#[derive(Debug, Clone, PartialEq)]
pub enum ApiKeyDeleteOutcome {
    /// 削除した（削除前の要約）。
    Deleted(ApiKeySummary),
    /// 失効していないので削除しなかった。
    StillActive,
}

/// [`ApiKeysService::lookup`] が有効なキーに対して返す文脈情報 - アプリの
/// 認証層がスコープの判定と `last_used_at` の間引き更新に使う。
#[derive(Debug, Clone, PartialEq)]
pub struct ApiKeyContext {
    pub id: i64,
    pub name: String,
    /// 平文で保存している先頭 8 文字（`api_keys.prefix`）。失効したキーを
    /// 削除すると同じ名前で発行し直せるため、名前だけでは旧いキーと新しい
    /// キーを区別できない。監査の主体表記（[`ApiKeyContext::actor_label`]）に
    /// 使う。
    pub prefix: String,
    /// 発行時に [`ApiKeyScopes::validate_scope`] を通ったスコープの並び。
    /// 何を許すかの判定はアプリが行う。
    pub scopes: Vec<String>,
    /// 直近の `last_used_at`（epoch ミリ秒）。[`should_touch_last_used`] に
    /// そのまま渡す。
    pub last_used_at_ms: Option<i64>,
}

impl ApiKeyContext {
    /// 監査ログ・保留中の変更の依頼者に残す主体の表記
    /// `"{name}（{prefix}）"`。失効したキーを削除すると同じ名前で
    /// 再発行できるため、名前だけでは旧いキーと新しいキーの操作を区別できない。
    pub fn actor_label(&self) -> String {
        format!("{}（{}）", self.name, self.prefix)
    }
}

/// [`ApiKeysService::lookup`] の結果。`Revoked`/`Tripped`/`Expired` を
/// `NotFound` と分けるのは、失効済み/一時停止中/期限切れのキーでの
/// アクセス試行を監査に残せるようにするため - 呼び出し元がこれらを区別して
/// 扱う。
///
/// ハッシュが一致しない場合は revoked/tripped/expired かどうかに関わらず
/// 常に `NotFound` を返す（`Revoked`/`Tripped`/`Expired` を返してしまうと、
/// secret を知らない攻撃者に「このプレフィックスは存在し、かつ失効済み/
/// 一時停止中/期限切れだ」という情報を漏らすことになるため —
/// [`ApiKeysService::lookup`] 参照）。
#[derive(Debug, Clone, PartialEq)]
pub enum ApiKeyLookup {
    Valid(ApiKeyContext),
    Revoked {
        id: i64,
        name: String,
    },
    /// アプリが付けた一時停止（banto-hub は書き込みのレート超過）。
    /// `revoked_at` と違い、[`ApiKeysService::clear_trip`] で解除できる。
    Tripped {
        id: i64,
        name: String,
    },
    /// 任意で設定された `expires_at` を過ぎたキー。解除はできない（再び
    /// 使うなら発行し直す）。
    Expired {
        id: i64,
        name: String,
    },
    NotFound,
}

/// 照合の結果（この crate の doc comment「照合できないことと、キーが無効な
/// ことを区別する」）。
#[derive(Debug)]
pub enum ApiKeyCheck {
    /// DB が答えた（有効か、どう無効か）。
    Answered(ApiKeyLookup),
    /// 照合そのものができなかった（DB エラー・タイムアウト）。キーが無効だと
    /// 分かったわけではない。
    Unavailable(BantoError),
}

/// 照合の結果を分ける純関数。`outcome` が `None` は
/// [`API_KEY_CHECK_TIMEOUT`] を超えた（タイムアウト）。
pub fn classify_lookup(outcome: Option<Result<ApiKeyLookup, BantoError>>) -> ApiKeyCheck {
    match outcome {
        Some(Ok(lookup)) => ApiKeyCheck::Answered(lookup),
        Some(Err(err)) => ApiKeyCheck::Unavailable(err),
        None => ApiKeyCheck::Unavailable(BantoError::Other(format!(
            "API キーを照合できませんでした（{} 秒以内に DB が応答しませんでした）",
            API_KEY_CHECK_TIMEOUT.as_secs()
        ))),
    }
}

/// 使えないキーをどう拒否するか。経路（REST・MCP・gRPC など）はこの分類
/// だけを見て応答を決める（HTTP は [`api_key_rejection_response`]）。
#[derive(Debug)]
pub enum ApiKeyRejection {
    /// キーとして認められない（失効・期限切れ・存在しない）。401 /
    /// `UNAUTHENTICATED`。
    Unauthenticated(UnauthenticatedReason),
    /// 認証はできたが、今は使えない（一時停止中。解除すれば同じキーで
    /// 戻る）。403 `key_tripped` / `PERMISSION_DENIED`。
    Tripped,
    /// 照合そのものができなかった。500 / `UNAVAILABLE`。
    Unavailable(BantoError),
}

/// [`ApiKeyRejection::Unauthenticated`] の理由（gRPC のメッセージの出し分け用。
/// HTTP の応答はどれも同じ 401）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnauthenticatedReason {
    Revoked,
    Expired,
    NotFound,
}

/// 拒否したキーの監査用の情報（id・名前・理由）。存在しないキーと照合でき
/// なかったときは `None`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeniedKey {
    pub id: i64,
    pub name: String,
    pub reason: &'static str,
}

/// 照合の結果を「使える（コンテキスト）」か「どう拒否するか」に変換する
/// 純関数。一時停止・失効・期限切れ・存在しない・照合できない、のすべてを
/// ここ 1 か所で決める。拒否したときは監査用の [`DeniedKey`] も返す。
pub fn api_key_verdict(
    check: ApiKeyCheck,
) -> Result<ApiKeyContext, (ApiKeyRejection, Option<DeniedKey>)> {
    let denied = |id: i64, name: String, reason: &'static str| Some(DeniedKey { id, name, reason });
    match check {
        ApiKeyCheck::Answered(ApiKeyLookup::Valid(ctx)) => Ok(ctx),
        ApiKeyCheck::Answered(ApiKeyLookup::Revoked { id, name }) => Err((
            ApiKeyRejection::Unauthenticated(UnauthenticatedReason::Revoked),
            denied(id, name, "revoked"),
        )),
        ApiKeyCheck::Answered(ApiKeyLookup::Expired { id, name }) => Err((
            ApiKeyRejection::Unauthenticated(UnauthenticatedReason::Expired),
            denied(id, name, "expired"),
        )),
        ApiKeyCheck::Answered(ApiKeyLookup::Tripped { id, name }) => {
            Err((ApiKeyRejection::Tripped, denied(id, name, "tripped")))
        }
        ApiKeyCheck::Answered(ApiKeyLookup::NotFound) => Err((
            ApiKeyRejection::Unauthenticated(UnauthenticatedReason::NotFound),
            None,
        )),
        ApiKeyCheck::Unavailable(err) => Err((ApiKeyRejection::Unavailable(err), None)),
    }
}

// --- HTTP の応答 --------------------------------------------------------------

/// 401（`banto_server::require_auth` と同じ本文 `ErrorBody::Unauthorized`）。
/// キーが無い・`Bearer` でないなど、照合の前に断るときにもアプリが使う。
pub fn unauthorized_response() -> Response {
    (StatusCode::UNAUTHORIZED, Json(ErrorBody::Unauthorized)).into_response()
}

/// 一時停止中のキー - 403 `{"error": "key_tripped"}`。
fn key_tripped_response() -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(serde_json::json!({ "error": "key_tripped" })),
    )
        .into_response()
}

/// 使えない API キーへの HTTP 応答（[`api_key_verdict`] の拒否をそのまま渡す）:
/// - 失効・期限切れ・存在しない → 401（`ErrorBody::Unauthorized`）
/// - 一時停止 → 403 `{"error": "key_tripped"}`（認証はできたが今は使えない。
///   解除すれば同じキーで戻るので、キーを捨てさせない）
/// - 照合できない → 500（`ApiError`、セッションの照合失敗と同じ形）
///
/// ログは書かない（照合できなかったことをどこへどう残すかはアプリが決める）。
pub fn api_key_rejection_response(rejection: ApiKeyRejection) -> Response {
    match rejection {
        ApiKeyRejection::Unauthenticated(_) => unauthorized_response(),
        ApiKeyRejection::Tripped => key_tripped_response(),
        ApiKeyRejection::Unavailable(err) => ApiError(err).into_response(),
    }
}

// --- サービス -----------------------------------------------------------------

/// API キーの発行・一覧・失効・削除・一時停止・照合。`S` はアプリの
/// [`ApiKeyScopes`]（キーの接頭辞とスコープの構文）。
///
/// `Clone` は安価（`SqlitePool` は `Arc` バックド）。
pub struct ApiKeysService<S> {
    pool: SqlitePool,
    _scopes: PhantomData<fn() -> S>,
}

// `derive(Clone)` は `S: Clone` を求めてしまうので手で書く（`S` は値を持たない
// 印の型）。
impl<S> Clone for ApiKeysService<S> {
    fn clone(&self) -> Self {
        Self {
            pool: self.pool.clone(),
            _scopes: PhantomData,
        }
    }
}

/// `(id, name, prefix, scopes, created_at, last_used_at, revoked_at,
/// tripped_at, expires_at)`（`list`/`fetch_summary` の SELECT 列順と一致させる
/// こと）。
type ApiKeyRow = (
    i64,
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

/// [`ApiKeysService::lookup`] の行: `(id, name, key_hash, scopes,
/// last_used_at, revoked_at, tripped_at, expires_at)` - 意図して
/// [`ApiKeyRow`] とは別（目的が違うので選ぶ列が違う）。
type LookupRow = (
    i64,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

fn row_to_summary(row: ApiKeyRow) -> Result<ApiKeySummary, BantoError> {
    let (
        id,
        name,
        prefix,
        scopes_json,
        created_at,
        last_used_at,
        revoked_at,
        tripped_at,
        expires_at,
    ) = row;
    let scopes: Vec<String> = serde_json::from_str(&scopes_json).map_err(|err| {
        BantoError::Other(format!("スコープのデシリアライズに失敗しました: {err}"))
    })?;
    Ok(ApiKeySummary {
        id,
        name,
        prefix,
        scopes,
        created_at,
        last_used_at: last_used_at.and_then(|value| value.parse::<i64>().ok()),
        revoked_at,
        tripped_at,
        expires_at: expires_at.and_then(|value| value.parse::<i64>().ok()),
    })
}

impl<S: ApiKeyScopes> ApiKeysService<S> {
    pub fn new(pool: SqlitePool) -> Self {
        Self {
            pool,
            _scopes: PhantomData,
        }
    }

    /// 新しい API キーを発行する。`name` は空白トリム後に非空必須・
    /// 重複不可（生の `UNIQUE` 制約違反をフォームに出さず
    /// `BantoError::Validation` に変換する）。`scopes` は
    /// [`ApiKeyScopes::validate_scope`] で構文検証する（不正なら 400 相当の
    /// `Validation`）。
    ///
    /// `prefix` の衝突（6バイト = 48ビット空間からの一様ランダム）は
    /// 確率的に無視できるほど小さい（約 2.8 × 10^14 分の1）ため、
    /// 衝突時は素直に `UNIQUE` 制約違反の `Storage` エラーとして
    /// 呼び出し元へ伝播させる（リトライは実装しない）。
    ///
    /// `expires_at`: 任意の絶対 epoch ミリ秒。`None` = 無期限（既定）。
    /// **ここでは値の妥当性を再検証しない** - 「現在時刻より未来」の検証は
    /// 呼び出し元が発行前に行う（このサービスはクロックを持たないため）。
    pub async fn issue(
        &self,
        name: &str,
        scopes: Vec<String>,
        expires_at: Option<i64>,
    ) -> Result<IssuedApiKey, BantoError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(BantoError::Validation {
                field_errors: vec![FieldError {
                    field: "name".to_string(),
                    message: "名前を入力してください".to_string(),
                }],
            });
        }
        validate_scopes::<S>(&scopes)?;

        let existing: Option<i64> = sqlx::query_scalar("SELECT id FROM api_keys WHERE name = ?")
            .bind(name)
            .fetch_optional(&self.pool)
            .await
            .map_err(banto_storage::storage_error)?;
        if existing.is_some() {
            return Err(BantoError::Validation {
                field_errors: vec![FieldError {
                    field: "name".to_string(),
                    message: "この名前は既に使用されています".to_string(),
                }],
            });
        }

        let prefix = generate_prefix();
        let secret = generate_secret();
        let key = format!("{}_{prefix}_{secret}", S::KEY_PREFIX);
        let key_hash = hash_key(&key);
        let scopes_json = serde_json::to_string(&scopes).map_err(|err| {
            BantoError::Other(format!("スコープのシリアライズに失敗しました: {err}"))
        })?;

        let id: i64 = sqlx::query_scalar(
            "INSERT INTO api_keys (name, prefix, key_hash, scopes, expires_at) \
             VALUES (?, ?, ?, ?, ?) RETURNING id",
        )
        .bind(name)
        .bind(&prefix)
        .bind(&key_hash)
        .bind(&scopes_json)
        .bind(expires_at.map(|value| value.to_string()))
        .fetch_one(&self.pool)
        .await
        .map_err(banto_storage::storage_error)?;

        Ok(IssuedApiKey {
            id,
            name: name.to_string(),
            prefix,
            scopes,
            key,
        })
    }

    /// 発行済みキーの一覧（`key_hash` は返さない）。作成順。
    pub async fn list(&self) -> Result<Vec<ApiKeySummary>, BantoError> {
        let rows: Vec<ApiKeyRow> = sqlx::query_as(
            "SELECT id, name, prefix, scopes, created_at, last_used_at, revoked_at, tripped_at, \
             expires_at FROM api_keys ORDER BY id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(banto_storage::storage_error)?;
        rows.into_iter().map(row_to_summary).collect()
    }

    /// `id` を失効させる（`revoked_at` を立てるだけ - ここでは物理削除
    /// しない。削除は失効の後に [`Self::delete`] で別に行う）。**冪等**:
    /// 既に失効済みの場合は何もせず現在の状態を返す（2回目の呼び出しで
    /// `revoked_at` が上書きされて最初の失効時刻が失われることはない）。
    /// `id` が存在しない場合のみ `NotFound`。
    pub async fn revoke(&self, id: i64) -> Result<ApiKeySummary, BantoError> {
        sqlx::query(
            "UPDATE api_keys SET revoked_at = datetime('now') \
             WHERE id = ? AND revoked_at IS NULL",
        )
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(banto_storage::storage_error)?;

        self.fetch_summary(id).await
    }

    /// **失効済みの** `id` を物理削除する（名前を使い直せるようにする）。
    /// 失効していないキーは [`ApiKeyDeleteOutcome::StillActive`] で断る（先に
    /// [`Self::revoke`]）。存在しない `id` は `NotFound`。成功時は削除した行の
    /// 要約（監査ログに名前とプレフィックスを残すため）を返す。
    pub async fn delete(&self, id: i64) -> Result<ApiKeyDeleteOutcome, BantoError> {
        let summary = self.fetch_summary(id).await?;
        // 判定と削除を 1 文にする（間に別の要求が割り込んでも、失効していない
        // キーは消えない）。
        let result = sqlx::query("DELETE FROM api_keys WHERE id = ? AND revoked_at IS NOT NULL")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(banto_storage::storage_error)?;
        if result.rows_affected() == 1 {
            return Ok(ApiKeyDeleteOutcome::Deleted(summary));
        }
        // 0 行: 直前の確認の後に消えた（NotFound）か、失効していない。
        self.fetch_summary(id).await?;
        Ok(ApiKeyDeleteOutcome::StillActive)
    }

    /// `id` を一時停止する（`tripped_at` を立てる - `revoked_at` と同じ
    /// パターンだが別の列・別の解除経路）。**冪等**: 既に一時停止中なら何も
    /// しない。付ける条件はアプリが決める。
    pub async fn trip(&self, id: i64) -> Result<ApiKeySummary, BantoError> {
        sqlx::query(
            "UPDATE api_keys SET tripped_at = datetime('now') \
             WHERE id = ? AND tripped_at IS NULL",
        )
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(banto_storage::storage_error)?;

        self.fetch_summary(id).await
    }

    /// `id` の一時停止を解除する（`tripped_at` を `NULL` に戻す）。**冪等**:
    /// 一時停止していなければ何もしない。
    pub async fn clear_trip(&self, id: i64) -> Result<ApiKeySummary, BantoError> {
        sqlx::query("UPDATE api_keys SET tripped_at = NULL WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(banto_storage::storage_error)?;

        self.fetch_summary(id).await
    }

    /// [`Self::revoke`]/[`Self::trip`]/[`Self::clear_trip`] 共通の
    /// 「更新後の行を読み直して `ApiKeySummary` にする」処理。`id` が
    /// 存在しない場合のみ `NotFound`。
    async fn fetch_summary(&self, id: i64) -> Result<ApiKeySummary, BantoError> {
        let row: Option<ApiKeyRow> = sqlx::query_as(
            "SELECT id, name, prefix, scopes, created_at, last_used_at, revoked_at, tripped_at, \
             expires_at FROM api_keys WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(banto_storage::storage_error)?;

        match row {
            Some(row) => row_to_summary(row),
            None => Err(BantoError::NotFound {
                resource: "api_keys".to_string(),
                id: id.to_string(),
            }),
        }
    }

    /// `Authorization: Bearer <token>` から渡されたトークンを照合する。
    ///
    /// **ハッシュ一致を revoked/tripped/expired チェックより先に行う**:
    /// prefix だけ一致して secret（＝ハッシュ）が一致しない場合は、その
    /// キーが失効済み/一時停止中/期限切れか有効かに関わらず常に
    /// [`ApiKeyLookup::NotFound`] を返す（[`ApiKeyLookup`] の doc comment
    /// 参照 - 情報漏洩防止)。判定順は
    /// **ハッシュ一致 → revoked → tripped → expired**。
    ///
    /// `now_ms`: 呼び出し元のクロックから渡す - [`is_expired`] の判定にのみ
    /// 使う。`last_used_at` の更新は呼び出し元が [`Self::touch_last_used`] を
    /// 別に呼ぶ（このメソッドは触らない）。
    pub async fn lookup(&self, full_key: &str, now_ms: i64) -> Result<ApiKeyLookup, BantoError> {
        let Some((prefix, _secret)) = parse_key(S::KEY_PREFIX, full_key) else {
            return Ok(ApiKeyLookup::NotFound);
        };

        let row: Option<LookupRow> = sqlx::query_as(
            "SELECT id, name, key_hash, scopes, last_used_at, revoked_at, tripped_at, \
             expires_at FROM api_keys WHERE prefix = ?",
        )
        .bind(prefix)
        .fetch_optional(&self.pool)
        .await
        .map_err(banto_storage::storage_error)?;

        let Some((
            id,
            name,
            key_hash,
            scopes_json,
            last_used_at,
            revoked_at,
            tripped_at,
            expires_at,
        )) = row
        else {
            return Ok(ApiKeyLookup::NotFound);
        };

        let candidate_hash = hash_key(full_key);
        if !constant_time_eq(&candidate_hash, &key_hash) {
            return Ok(ApiKeyLookup::NotFound);
        }

        if revoked_at.is_some() {
            return Ok(ApiKeyLookup::Revoked { id, name });
        }

        if tripped_at.is_some() {
            return Ok(ApiKeyLookup::Tripped { id, name });
        }

        let expires_at_ms = expires_at.and_then(|value| value.parse::<i64>().ok());
        if is_expired(now_ms, expires_at_ms) {
            return Ok(ApiKeyLookup::Expired { id, name });
        }

        let scopes: Vec<String> = serde_json::from_str(&scopes_json).map_err(|err| {
            BantoError::Other(format!("スコープのデシリアライズに失敗しました: {err}"))
        })?;
        let last_used_at_ms = last_used_at.and_then(|value| value.parse::<i64>().ok());

        Ok(ApiKeyLookup::Valid(ApiKeyContext {
            id,
            name,
            prefix: prefix.to_string(),
            scopes,
            last_used_at_ms,
        }))
    }

    /// 認証の経路はこれを使う。[`Self::lookup`] に [`API_KEY_CHECK_TIMEOUT`] の
    /// 上限を掛け、[`classify_lookup`] で「DB が答えた」と「照合できなかった」
    /// に分ける。
    pub async fn check(&self, full_key: &str, now_ms: i64) -> ApiKeyCheck {
        classify_lookup(
            tokio::time::timeout(API_KEY_CHECK_TIMEOUT, self.lookup(full_key, now_ms))
                .await
                .ok(),
        )
    }

    /// [`should_touch_last_used`] が `true` を返す場合のみ `last_used_at`
    /// を `now_ms` で更新する（60 秒の間引き）。
    ///
    /// 同時リクエストがこの判定と UPDATE の間にレースしても、最悪
    /// もう一度余分に UPDATE が走るだけ（「最終利用時刻」はベストエフォート
    /// な表示情報であり、厳密な直列化は不要）。
    pub async fn touch_last_used(
        &self,
        id: i64,
        now_ms: i64,
        last_used_at_ms: Option<i64>,
    ) -> Result<(), BantoError> {
        if !should_touch_last_used(now_ms, last_used_at_ms) {
            return Ok(());
        }
        sqlx::query("UPDATE api_keys SET last_used_at = ? WHERE id = ?")
            .bind(now_ms.to_string())
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(banto_storage::storage_error)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
