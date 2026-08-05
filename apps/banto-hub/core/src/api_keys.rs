//! API キー基盤 (docs/tag-server-design.md §5.6「認証（全プロトコル共通）」、
//! T0-2 実装指示 §1「API キー基盤」)。`/api/v1/*` を叩く機械クライアント
//! （MES/クラウド収集/自作ダッシュボード等）向けの、管理 UI セッションとは
//! 別系統の認証情報。管理 API（`/api/api-keys/*`、admin ロール限定）で
//! 発行・一覧・失効を行い、実際の照合は `crate::rest` の `/api/v1/*`
//! ミドルウェアがこのモジュールの [`ApiKeysService::lookup`] を呼んで行う。
//!
//! ## キー形式
//!
//! 平文キーは `bh_{prefix}_{secret}`:
//! - `prefix`: 6 バイトの CSPRNG を base64url（パディングなし）で符号化した
//!   8 文字。行の検索キー（`api_keys.prefix`、`UNIQUE`）として平文保存する
//!   — 秘密ではなく識別子。
//! - `secret`: 32 バイトの CSPRNG を base64url（パディングなし）で符号化
//!   した 43 文字。こちらは検証にしか使わず、平文は発行応答で一度だけ
//!   返して DB には保存しない（保存するのは下記のハッシュのみ）。
//!
//! base64url の文字集合には `-`/`_` が含まれるため、`prefix`/`secret` の
//! 内部に `_` が現れる可能性がある。したがって `bh_{prefix}_{secret}` を
//! **`_` で分割して**パースするのは誤り（区切り文字と本体の文字が衝突
//! しうる）。[`parse_key`] は `"bh_"` を剥がした後、`prefix` の長さが
//! 生成時に必ず 8 文字になる（6 バイト = 48 ビット = 8 × 6 ビット、
//! base64 は端数なしできっちり割り切れる）という construction 上の
//! 不変条件を使い、**固定長スライスで**パースする（区切り文字を探さない）。
//!
//! ## ハッシュ方式（argon2 ではなく SHA-256 の理由）
//!
//! `crate::users` のパスワードは argon2id（意図的に低速なハッシュ）で
//! 保存している。API キーの `secret` にはこの方式を使わない:
//!
//! - argon2 が低速なのは、人間が選ぶパスワードは低エントロピー（辞書攻撃・
//!   総当たりが現実的な探索空間）だからで、**攻撃者側の計算を意図的に
//!   遅くする**ことに意味がある。
//! - `secret` は 32 バイトの CSPRNG（256 ビットのエントロピー）そのもので、
//!   総当たりは非現実的（2^256 通り）。低速化しても攻撃耐性は実質増えない
//!   一方、`/api/v1/*` は**毎リクエスト**照合が走る経路なので、argon2 の
//!   コスト（意図的に数十〜数百 ms オーダーにチューニングされる）は
//!   スループットの実害・DoS 増幅要因にしかならない。
//! - 高エントロピーなランダムシークレットの照合に SHA-256 のような速い
//!   一方向ハッシュを使うのは業界標準的な設計（GitHub Personal Access
//!   Token 等と同型）。`sha2` クレートをこの用途で使う。
//!
//! ## 照合のタイミングについて
//!
//! [`constant_time_eq`] は固定時間比較を明示的に書いているが、これは
//! 過剰な安全側の慣行に近い: 比較しているのは元の `secret` ではなく
//! **SHA-256 ダイジェスト同士**であり、ダイジェストは一方向性を持つため
//! 「何バイト目まで一致したか」が漏れたとしても元の `secret` の推測には
//! つながらない（`secret` 自体は DB のどこにも保存されていない）。とはいえ
//! コストがほぼゼロなので、早期リターンする単純な `==` より安全側に倒す。
//!
//! ## スコープ構文検証（設計 §5.6・T0-2 スコープ外の明示）
//!
//! `read` と `write:{connection}.{group}.{tag}` のみを許可する。書き込み
//! スコープはワイルドカード不可・3セグメントちょうど・各セグメント非空を
//! 発行時に検証するが、**実際の書き込み検査（T2）はここでは行わない**
//! （書き込み API 自体が T0-2 の時点でまだ存在しない）。
//!
//! ## トリップ（T2-4、設計 §6-4・2026-08-05 決定）
//!
//! `tripped_at` は `revoked_at`（T0-2、不可逆の失効）とは**別の解除可能な
//! 状態**。書き込みレート制限（`crate::write_rate`）を超過したキーは
//! `crate::rest` の書き込みハンドラが [`ApiKeysService::trip`] を呼んで
//! トリップさせ、admin が管理 UI から [`ApiKeysService::clear_trip`] で
//! 手動解除する。[`ApiKeysService::lookup`] の判定順は
//! **ハッシュ一致 → revoked → tripped**（この節見出しの上、「照合の
//! タイミングについて」の情報漏洩防止の理由付けと同じ順序 - T0-2 の
//! revoked チェックがハッシュ一致より先に来てはならないのと同じ理由で、
//! tripped チェックも revoked の後段に置く）。トリップ中のキーは
//! read/write いずれの `/api/v1/*` リクエストも拒否される
//! （`crate::rest::require_tag_space_auth` 参照）。

use banto_core::{BantoError, FieldError};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;

/// 平文キーの先頭リテラル。
const KEY_PREFIX: &str = "bh_";
/// `prefix` 部分の文字数（6 バイト → base64url ちょうど 8 文字、端数なし）。
const PREFIX_LEN: usize = 8;
/// last_used_at の更新スロットル幅（設計 T0-2 実装指示: 「前回更新から60秒
/// 以上経過時のみ更新」）。
const LAST_USED_THROTTLE_MS: i64 = 60_000;

// --- スコープ構文検証 -------------------------------------------------------

/// 1つのスコープ文字列の構文検証（設計 §5.6）。
///
/// - `"read"`: そのまま許可。
/// - `"write:{connection}.{group}.{tag}"`: ワイルドカード（`*`）禁止・
///   ピリオド区切りでちょうど3セグメント・各セグメント非空。
/// - それ以外は全て不正。
///
/// **実際の書き込み可否判定（T2）はここでは行わない** — ここは発行時の
/// 構文チェックのみ。
fn validate_scope(scope: &str) -> Result<(), String> {
    if scope == "read" {
        return Ok(());
    }
    let Some(pattern) = scope.strip_prefix("write:") else {
        return Err(format!(
            "不明なスコープです（'read' または 'write:{{connection}}.{{group}}.{{tag}}' のみ許可）: {scope}"
        ));
    };
    if pattern.contains('*') {
        return Err(format!(
            "write スコープにワイルドカードは使えません（明示列挙のみ）: {scope}"
        ));
    }
    let segments: Vec<&str> = pattern.split('.').collect();
    if segments.len() != 3 || segments.iter().any(|segment| segment.is_empty()) {
        return Err(format!(
            "write スコープは {{connection}}.{{group}}.{{tag}} の3セグメントで指定してください: {scope}"
        ));
    }
    Ok(())
}

/// [`validate_scope`] をリスト全体に適用し、1件でも空・不正なら
/// `BantoError::Validation` を返す。`ApiKeysService::issue` の入口で使う。
fn validate_scopes(scopes: &[String]) -> Result<(), BantoError> {
    if scopes.is_empty() {
        return Err(BantoError::Validation {
            field_errors: vec![FieldError {
                field: "scopes".to_string(),
                message: "少なくとも1つのスコープを指定してください".to_string(),
            }],
        });
    }
    for scope in scopes {
        if let Err(message) = validate_scope(scope) {
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
    // が失敗時に panic するのと同じ扱い（crate::users::hash_password 参照）。
    getrandom::getrandom(&mut buf).expect("システム乱数生成器の呼び出しに失敗しました");
    URL_SAFE_NO_PAD.encode(buf)
}

/// 6 バイト → 8 文字（このモジュールの doc comment 参照: [`parse_key`] は
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

/// 固定時間文字列比較（このモジュールの doc comment「照合のタイミングに
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

/// `"bh_{prefix}_{secret}"` を `(prefix, secret)` に分解する。区切り文字
/// 探索ではなく固定長スライスで行う理由はこのモジュールの doc comment
/// 「キー形式」参照。`"bh_"` プレフィックスがない・長さが足りない・
/// prefix の直後が `_` でない、のいずれかなら `None`（この関数の戻り値が
/// `None` = 「`bh_` 形式のキーとして構文が壊れている」であり、
/// [`ApiKeyLookup::NotFound`] として扱われる）。
fn parse_key(token: &str) -> Option<(&str, &str)> {
    let rest = token.strip_prefix(KEY_PREFIX)?;
    if rest.len() <= PREFIX_LEN + 1 {
        return None;
    }
    let (prefix, tail) = rest.split_at(PREFIX_LEN);
    let secret = tail.strip_prefix('_')?;
    if secret.is_empty() {
        return None;
    }
    Some((prefix, secret))
}

// --- 60秒スロットル（純関数 - 単体テスト対象） ------------------------------

/// `last_used_at` を更新すべきか（設計 T0-2 実装指示: 「前回更新から60秒
/// 以上経過時のみ更新」）。DB/クロックに依存しない純関数として切り出して
/// あるので、実際の時計を動かさずに単体テストできる。
///
/// `last_used_at_ms` が `None`（一度も使われていない）なら常に `true`。
pub fn should_touch_last_used(now_ms: i64, last_used_at_ms: Option<i64>) -> bool {
    match last_used_at_ms {
        None => true,
        Some(prev) => now_ms.saturating_sub(prev) >= LAST_USED_THROTTLE_MS,
    }
}

// --- 公開の型 ---------------------------------------------------------------

/// 発行直後にのみ平文キーを含めて返す応答。DB には `key`/`key_hash` の
/// うち `key`（平文）は一切保存されない（このモジュールの doc comment
/// 参照）。
#[derive(Debug, Clone)]
pub struct IssuedApiKey {
    pub id: i64,
    pub name: String,
    pub prefix: String,
    pub scopes: Vec<String>,
    /// 平文キー全体（`bh_...`）。この構造体を捨てたら二度と復元できない
    /// （設計: 「key はこの応答限り」）。
    pub key: String,
}

/// `GET /api/api-keys` の一覧行、および `POST /api/api-keys/{id}/revoke`
/// の応答。`key_hash` は含めない（設計: 「key_hash は返さない」）。
///
/// `last_used_at`/`created_at`/`revoked_at` は DB 保存形式そのままではなく
/// epoch ミリ秒（`created_at`/`revoked_at` は `datetime('now')` の
/// ISO 文字列を素直に返す一方、`last_used_at` だけ数値なのは
/// [`ApiKeysService`] の doc comment 参照）。
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
    /// T2-4（設計 §6-4）: このキーが現在トリップ中か、その日時
    /// （ISO 文字列、`datetime('now')` そのまま）。`revoked_at` とは別の
    /// 解除可能な状態 - このモジュールの doc comment「トリップ」参照。
    pub tripped_at: Option<String>,
}

/// [`ApiKeysService::lookup`] が有効なキーに対して返す文脈情報 -
/// `crate::rest` の `/api/v1/*` ミドルウェアがスコープ判定と
/// `last_used_at` スロットル更新に使う。
#[derive(Debug, Clone, PartialEq)]
pub struct ApiKeyContext {
    pub id: i64,
    pub name: String,
    pub scopes: Vec<String>,
    /// 直近の `last_used_at`（epoch ミリ秒）。[`should_touch_last_used`] に
    /// そのまま渡す。
    pub last_used_at_ms: Option<i64>,
}

impl ApiKeyContext {
    /// `/api/v1/*` の認証は `read` スコープを要求する（設計 §5.6）。
    pub fn has_read_scope(&self) -> bool {
        self.scopes.iter().any(|scope| scope == "read")
    }

    /// T2-4（設計 §6 実装指示 §5「認証」）: `POST /api/v1/values/{tag}` は
    /// `write:{external_name}` の**完全一致**が必須（ワイルドカードは
    /// 発行時点で拒否済み、`validate_scope` 参照）。`read` スコープでは
    /// 書けない。
    pub fn has_write_scope(&self, external_name: &str) -> bool {
        let needle = format!("write:{external_name}");
        self.scopes.iter().any(|scope| scope == &needle)
    }
}

/// [`ApiKeysService::lookup`] の結果。`Revoked`/`Tripped` を `NotFound` と
/// 分けるのは「失効済み/トリップ中のキーでのアクセス試行は audit_log に
/// 記録する」（設計 T0-2 実装指示、T2-4 で `Tripped` にも同じ扱いを拡張）
/// ため — 呼び出し元（`crate::rest`）がこれらを区別して扱う。
///
/// ハッシュが一致しない場合は revoked/tripped かどうかに関わらず常に
/// `NotFound` を返す（`Revoked`/`Tripped` を返してしまうと、secret を
/// 知らない攻撃者に「このプレフィックスは存在し、かつ失効済み/トリップ
/// 中だ」という情報を漏らすことになるため — [`ApiKeysService::lookup`]
/// の実装コメント参照）。
#[derive(Debug, Clone, PartialEq)]
pub enum ApiKeyLookup {
    Valid(ApiKeyContext),
    Revoked {
        id: i64,
        name: String,
    },
    /// T2-4（設計 §6-4・2026-08-05 決定）: レート制限ブレーカがトリップ
    /// させた状態。`revoked_at` と違い、admin が
    /// [`ApiKeysService::clear_trip`] で解除できる。
    Tripped {
        id: i64,
        name: String,
    },
    NotFound,
}

/// API キーの発行・一覧・失効・照合（設計 §5.6、T0-2 実装指示 §1）。
///
/// `Clone` は安価（`SqlitePool` は `Arc` バックド）、他の `*Service` と
/// 同じ規約。
#[derive(Clone)]
pub struct ApiKeysService {
    pool: SqlitePool,
}

type ApiKeyRow = (
    i64,
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
);

/// [`ApiKeysService::lookup`]'s row shape: `(id, name, key_hash, scopes,
/// last_used_at, revoked_at, tripped_at)` - deliberately not [`ApiKeyRow`]
/// (which also carries `prefix`/`created_at` instead of `key_hash`); the two
/// queries select different columns for different purposes.
type LookupRow = (
    i64,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
);

fn row_to_summary(row: ApiKeyRow) -> Result<ApiKeySummary, BantoError> {
    let (id, name, prefix, scopes_json, created_at, last_used_at, revoked_at, tripped_at) = row;
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
    })
}

impl ApiKeysService {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// 新しい API キーを発行する。`name` は空白トリム後に非空必須・
    /// 重複不可（`crate::users::UsersService::create_user` の
    /// ユーザー名重複チェックと同じパターン - 生の `UNIQUE` 制約違反を
    /// フォームに出さず `BantoError::Validation` に変換する）。`scopes` は
    /// [`validate_scopes`] で構文検証する（不正なら 400 相当の
    /// `Validation`）。
    ///
    /// `prefix` の衝突（6バイト = 48ビット空間からの一様ランダム）は
    /// 確率的に無視できるほど小さい（約 2.8 × 10^14 分の1）ため、
    /// 衝突時は素直に `UNIQUE` 制約違反の `Storage` エラーとして
    /// 呼び出し元へ伝播させる（リトライは実装しない）。
    pub async fn issue(&self, name: &str, scopes: Vec<String>) -> Result<IssuedApiKey, BantoError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(BantoError::Validation {
                field_errors: vec![FieldError {
                    field: "name".to_string(),
                    message: "名前を入力してください".to_string(),
                }],
            });
        }
        validate_scopes(&scopes)?;

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
        let key = format!("{KEY_PREFIX}{prefix}_{secret}");
        let key_hash = hash_key(&key);
        let scopes_json = serde_json::to_string(&scopes).map_err(|err| {
            BantoError::Other(format!("スコープのシリアライズに失敗しました: {err}"))
        })?;

        let id: i64 = sqlx::query_scalar(
            "INSERT INTO api_keys (name, prefix, key_hash, scopes) VALUES (?, ?, ?, ?) \
             RETURNING id",
        )
        .bind(name)
        .bind(&prefix)
        .bind(&key_hash)
        .bind(&scopes_json)
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

    /// 発行済みキーの一覧（`crate::rest`'s `GET /api/api-keys` - 設計:
    /// `key_hash` は返さない）。作成順。
    pub async fn list(&self) -> Result<Vec<ApiKeySummary>, BantoError> {
        let rows: Vec<ApiKeyRow> = sqlx::query_as(
            "SELECT id, name, prefix, scopes, created_at, last_used_at, revoked_at, tripped_at \
             FROM api_keys ORDER BY id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(banto_storage::storage_error)?;
        rows.into_iter().map(row_to_summary).collect()
    }

    /// `id` を失効させる（`revoked_at` を立てるだけ - 物理削除しない、
    /// 設計: 「DELETE は設けない（失効履歴を残す方針）」）。**冪等**:
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

    /// T2-4（設計 §6-4）: `id` をトリップさせる（`tripped_at` を立てる -
    /// `revoked_at` と同じパターンだが別の列・別の解除経路）。**冪等**:
    /// 既にトリップ中なら何もしない。`crate::rest` の書き込みハンドラが
    /// レート制限超過時に呼ぶ。
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

    /// T2-4（設計 §6-4）: `id` のトリップを解除する（`tripped_at` を
    /// `NULL` に戻す）。**冪等**: トリップしていなければ何もしない。
    /// `crate::rest` の `POST /api/api-keys/{id}/clear-trip`（admin 限定）
    /// から呼ぶ。
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
            "SELECT id, name, prefix, scopes, created_at, last_used_at, revoked_at, tripped_at \
             FROM api_keys WHERE id = ?",
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

    /// `Authorization: Bearer <token>` から渡された `bh_...` トークンを
    /// 照合する。`crate::rest` の `/api/v1/*` ミドルウェアの唯一の入口。
    ///
    /// **ハッシュ一致を revoked/tripped チェックより先に行う**: prefix
    /// だけ一致して secret（＝ハッシュ）が一致しない場合は、そのキーが
    /// 失効済み/トリップ中か有効かに関わらず常に [`ApiKeyLookup::NotFound`]
    /// を返す（[`ApiKeyLookup`] の doc comment 参照 - 情報漏洩防止)。
    /// 判定順は **ハッシュ一致 → revoked → tripped**
    /// （このモジュールの doc comment「トリップ」参照 - T0-2 と同じ順序の
    /// 規律）。
    pub async fn lookup(&self, full_key: &str) -> Result<ApiKeyLookup, BantoError> {
        let Some((prefix, _secret)) = parse_key(full_key) else {
            return Ok(ApiKeyLookup::NotFound);
        };

        let row: Option<LookupRow> = sqlx::query_as(
            "SELECT id, name, key_hash, scopes, last_used_at, revoked_at, tripped_at \
             FROM api_keys WHERE prefix = ?",
        )
        .bind(prefix)
        .fetch_optional(&self.pool)
        .await
        .map_err(banto_storage::storage_error)?;

        let Some((id, name, key_hash, scopes_json, last_used_at, revoked_at, tripped_at)) = row
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

        let scopes: Vec<String> = serde_json::from_str(&scopes_json).map_err(|err| {
            BantoError::Other(format!("スコープのデシリアライズに失敗しました: {err}"))
        })?;
        let last_used_at_ms = last_used_at.and_then(|value| value.parse::<i64>().ok());

        Ok(ApiKeyLookup::Valid(ApiKeyContext {
            id,
            name,
            scopes,
            last_used_at_ms,
        }))
    }

    /// [`should_touch_last_used`] が `true` を返す場合のみ `last_used_at`
    /// を `now_ms` で更新する（設計 T0-2 実装指示: 60秒スロットル）。
    /// 保存形式が epoch ミリ秒の10進文字列である理由は [`ApiKeySummary`]
    /// の doc comment 参照。
    ///
    /// 同時リクエストがこの判定と UPDATE の間にレースしても、最悪
    /// もう一度余分に UPDATE が走るだけ（「最終利用時刻」はベストエフォート
    /// な表示情報であり、厳密な直列化は不要 - 設計に排他制御の要求はない）。
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
mod tests {
    use super::*;
    use crate::db::migrate_memory;

    async fn service() -> ApiKeysService {
        let pool = migrate_memory().await.expect("migrate_memory");
        ApiKeysService::new(pool)
    }

    // --- スコープ構文検証 ----------------------------------------------

    #[test]
    fn validate_scope_accepts_read_and_well_formed_write() {
        assert!(validate_scope("read").is_ok());
        assert!(validate_scope("write:line1.fast.temp01").is_ok());
    }

    #[test]
    fn validate_scope_rejects_wildcard() {
        assert!(validate_scope("write:line1.fast.*").is_err());
        assert!(validate_scope("write:*").is_err());
    }

    #[test]
    fn validate_scope_rejects_wrong_segment_count() {
        assert!(validate_scope("write:line1.fast").is_err());
        assert!(validate_scope("write:line1.fast.temp01.extra").is_err());
    }

    #[test]
    fn validate_scope_rejects_empty_segment() {
        assert!(validate_scope("write:line1..temp01").is_err());
    }

    #[test]
    fn validate_scope_rejects_unknown_kind() {
        assert!(validate_scope("admin").is_err());
        assert!(validate_scope("").is_err());
    }

    // --- キー生成・パース・ハッシュ ---------------------------------------

    #[test]
    fn generated_prefix_is_always_prefix_len_chars() {
        for _ in 0..50 {
            assert_eq!(generate_prefix().len(), PREFIX_LEN);
        }
    }

    #[test]
    fn parse_key_round_trips_a_generated_key() {
        let prefix = generate_prefix();
        let secret = generate_secret();
        let key = format!("{KEY_PREFIX}{prefix}_{secret}");
        let (parsed_prefix, parsed_secret) = parse_key(&key).expect("should parse");
        assert_eq!(parsed_prefix, prefix);
        assert_eq!(parsed_secret, secret);
    }

    /// base64url は `_` を含みうる文字集合なので、prefix/secret の内部に
    /// `_` が現れても固定長パースが壊れないことを明示的に確認する
    /// (このモジュールの doc comment「キー形式」の核心)。
    #[test]
    fn parse_key_survives_underscores_inside_prefix_and_secret() {
        let prefix = "ab_d_f_h"; // 8 chars, contains '_'
        let secret = "s_e_c_r_e_t_with_underscores";
        let key = format!("{KEY_PREFIX}{prefix}_{secret}");
        let (parsed_prefix, parsed_secret) = parse_key(&key).expect("should parse");
        assert_eq!(parsed_prefix, prefix);
        assert_eq!(parsed_secret, secret);
    }

    #[test]
    fn parse_key_rejects_malformed_tokens() {
        assert!(parse_key("not-a-bh-key").is_none());
        assert!(parse_key("bh_tooshort").is_none());
        assert!(parse_key("bh_12345678_").is_none()); // empty secret
    }

    #[test]
    fn constant_time_eq_matches_regular_equality() {
        assert!(constant_time_eq("abc", "abc"));
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "abcd"));
    }

    // --- 60秒スロットル（純関数） -------------------------------------

    #[test]
    fn should_touch_last_used_is_true_when_never_used() {
        assert!(should_touch_last_used(1_000_000, None));
    }

    #[test]
    fn should_touch_last_used_is_false_within_the_window() {
        assert!(!should_touch_last_used(1_000_000, Some(999_500)));
        assert!(!should_touch_last_used(1_000_000, Some(940_001)));
    }

    #[test]
    fn should_touch_last_used_is_true_at_and_after_the_threshold() {
        assert!(should_touch_last_used(1_000_000, Some(940_000)));
        assert!(should_touch_last_used(1_000_000, Some(0)));
    }

    // --- サービス: issue/list/revoke/lookup ------------------------------

    #[tokio::test]
    async fn issue_then_lookup_round_trips() {
        let svc = service().await;
        let issued = svc
            .issue("mes-gateway", vec!["read".to_string()])
            .await
            .expect("issue should succeed");
        assert!(issued.key.starts_with("bh_"));

        let lookup = svc
            .lookup(&issued.key)
            .await
            .expect("lookup should succeed");
        match lookup {
            ApiKeyLookup::Valid(ctx) => {
                assert_eq!(ctx.id, issued.id);
                assert_eq!(ctx.name, "mes-gateway");
                assert!(ctx.has_read_scope());
                assert_eq!(ctx.last_used_at_ms, None);
            }
            other => panic!("expected Valid, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn lookup_with_wrong_secret_is_not_found() {
        let svc = service().await;
        let issued = svc
            .issue("mes-gateway", vec!["read".to_string()])
            .await
            .unwrap();
        let (prefix, _secret) = parse_key(&issued.key).unwrap();
        let forged = format!("bh_{prefix}_{}", generate_secret());

        let lookup = svc.lookup(&forged).await.unwrap();
        assert_eq!(lookup, ApiKeyLookup::NotFound);
    }

    #[tokio::test]
    async fn lookup_unknown_prefix_is_not_found() {
        let svc = service().await;
        let lookup = svc
            .lookup(&format!("bh_{}_{}", generate_prefix(), generate_secret()))
            .await
            .unwrap();
        assert_eq!(lookup, ApiKeyLookup::NotFound);
    }

    #[tokio::test]
    async fn issue_rejects_invalid_scope_syntax() {
        let svc = service().await;
        let err = svc
            .issue("bad-scope", vec!["write:line1.fast.*".to_string()])
            .await
            .unwrap_err();
        assert!(matches!(err, BantoError::Validation { .. }));
    }

    #[tokio::test]
    async fn issue_rejects_duplicate_name() {
        let svc = service().await;
        svc.issue("dup", vec!["read".to_string()]).await.unwrap();
        let err = svc
            .issue("dup", vec!["read".to_string()])
            .await
            .unwrap_err();
        match err {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, "name");
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn revoke_then_lookup_reports_revoked_not_not_found() {
        let svc = service().await;
        let issued = svc
            .issue("revoke-me", vec!["read".to_string()])
            .await
            .unwrap();

        let summary = svc.revoke(issued.id).await.expect("revoke should succeed");
        assert!(summary.revoked_at.is_some());

        let lookup = svc.lookup(&issued.key).await.unwrap();
        match lookup {
            ApiKeyLookup::Revoked { id, name } => {
                assert_eq!(id, issued.id);
                assert_eq!(name, "revoke-me");
            }
            other => panic!("expected Revoked, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn revoke_is_idempotent_and_keeps_the_first_timestamp() {
        let svc = service().await;
        let issued = svc
            .issue("idempotent", vec!["read".to_string()])
            .await
            .unwrap();

        let first = svc.revoke(issued.id).await.unwrap();
        let second = svc.revoke(issued.id).await.unwrap();
        assert_eq!(first.revoked_at, second.revoked_at);
    }

    #[tokio::test]
    async fn revoke_unknown_id_is_not_found() {
        let svc = service().await;
        let err = svc.revoke(999).await.unwrap_err();
        assert!(matches!(err, BantoError::NotFound { .. }));
    }

    #[tokio::test]
    async fn wrong_secret_against_a_revoked_key_is_still_not_found() {
        // Info-leak guard: a forged secret against a *revoked* key's prefix
        // must not distinguish itself from a forged secret against a live
        // key's prefix - both are NotFound (see ApiKeyLookup's doc comment).
        let svc = service().await;
        let issued = svc
            .issue("revoked-forged", vec!["read".to_string()])
            .await
            .unwrap();
        svc.revoke(issued.id).await.unwrap();

        let (prefix, _secret) = parse_key(&issued.key).unwrap();
        let forged = format!("bh_{prefix}_{}", generate_secret());
        let lookup = svc.lookup(&forged).await.unwrap();
        assert_eq!(lookup, ApiKeyLookup::NotFound);
    }

    #[tokio::test]
    async fn list_reflects_issued_and_revoked_keys() {
        let svc = service().await;
        svc.issue("a", vec!["read".to_string()]).await.unwrap();
        let b = svc.issue("b", vec!["read".to_string()]).await.unwrap();
        svc.revoke(b.id).await.unwrap();

        let listed = svc.list().await.unwrap();
        assert_eq!(listed.len(), 2);
        let a = listed.iter().find(|k| k.name == "a").unwrap();
        assert!(a.revoked_at.is_none());
        let b = listed.iter().find(|k| k.name == "b").unwrap();
        assert!(b.revoked_at.is_some());
        // key_hash must never appear on the wire - ApiKeySummary has no such
        // field at all, so this is a compile-time guarantee, not a runtime
        // assertion; documented here for visibility.
    }

    #[tokio::test]
    async fn touch_last_used_respects_the_throttle() {
        let svc = service().await;
        let issued = svc
            .issue("throttled", vec!["read".to_string()])
            .await
            .unwrap();

        svc.touch_last_used(issued.id, 1_000_000, None)
            .await
            .unwrap();
        let after_first = svc.list().await.unwrap();
        let entry = after_first.iter().find(|k| k.id == issued.id).unwrap();
        assert_eq!(entry.last_used_at, Some(1_000_000));

        // Within the 60s window: must not move.
        svc.touch_last_used(issued.id, 1_000_500, Some(1_000_000))
            .await
            .unwrap();
        let still = svc.list().await.unwrap();
        let entry = still.iter().find(|k| k.id == issued.id).unwrap();
        assert_eq!(entry.last_used_at, Some(1_000_000));

        // Past the window: must advance.
        svc.touch_last_used(issued.id, 1_061_000, Some(1_000_000))
            .await
            .unwrap();
        let moved = svc.list().await.unwrap();
        let entry = moved.iter().find(|k| k.id == issued.id).unwrap();
        assert_eq!(entry.last_used_at, Some(1_061_000));
    }

    // --- T2-4: has_write_scope --------------------------------------------

    #[test]
    fn has_write_scope_requires_an_exact_match() {
        let ctx = ApiKeyContext {
            id: 1,
            name: "k".to_string(),
            scopes: vec!["write:line1.fast.temp01".to_string()],
            last_used_at_ms: None,
        };
        assert!(ctx.has_write_scope("line1.fast.temp01"));
        assert!(!ctx.has_write_scope("line1.fast.temp02"));
        assert!(!ctx.has_write_scope("line1.fast"));
    }

    #[test]
    fn has_write_scope_is_false_for_a_read_only_key() {
        let ctx = ApiKeyContext {
            id: 1,
            name: "k".to_string(),
            scopes: vec!["read".to_string()],
            last_used_at_ms: None,
        };
        assert!(!ctx.has_write_scope("line1.fast.temp01"));
    }

    // --- T2-4: trip/clear_trip/lookup ordering -----------------------------

    #[tokio::test]
    async fn trip_then_lookup_reports_tripped_not_valid_or_not_found() {
        let svc = service().await;
        let issued = svc
            .issue("writer", vec!["write:line1.fast.temp01".to_string()])
            .await
            .unwrap();

        let summary = svc.trip(issued.id).await.expect("trip should succeed");
        assert!(summary.tripped_at.is_some());
        assert!(summary.revoked_at.is_none(), "trip must not revoke");

        let lookup = svc.lookup(&issued.key).await.unwrap();
        match lookup {
            ApiKeyLookup::Tripped { id, name } => {
                assert_eq!(id, issued.id);
                assert_eq!(name, "writer");
            }
            other => panic!("expected Tripped, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn trip_is_idempotent_and_keeps_the_first_timestamp() {
        let svc = service().await;
        let issued = svc
            .issue("idempotent-trip", vec!["read".to_string()])
            .await
            .unwrap();

        let first = svc.trip(issued.id).await.unwrap();
        let second = svc.trip(issued.id).await.unwrap();
        assert_eq!(first.tripped_at, second.tripped_at);
    }

    #[tokio::test]
    async fn clear_trip_restores_valid_lookup() {
        let svc = service().await;
        let issued = svc
            .issue("clear-me", vec!["read".to_string()])
            .await
            .unwrap();
        svc.trip(issued.id).await.unwrap();

        let cleared = svc
            .clear_trip(issued.id)
            .await
            .expect("clear_trip should succeed");
        assert!(cleared.tripped_at.is_none());

        let lookup = svc.lookup(&issued.key).await.unwrap();
        match lookup {
            ApiKeyLookup::Valid(ctx) => assert_eq!(ctx.id, issued.id),
            other => panic!("expected Valid after clear_trip, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn clear_trip_is_idempotent() {
        let svc = service().await;
        let issued = svc
            .issue("clear-idempotent", vec!["read".to_string()])
            .await
            .unwrap();

        // Clearing a key that was never tripped must not error.
        let cleared = svc
            .clear_trip(issued.id)
            .await
            .expect("clear_trip should succeed");
        assert!(cleared.tripped_at.is_none());
    }

    #[tokio::test]
    async fn revoked_key_is_reported_as_revoked_even_if_also_tripped() {
        // 判定順: ハッシュ一致 -> revoked -> tripped. A revoked-and-tripped
        // key must report Revoked, matching the doc comment's ordering.
        let svc = service().await;
        let issued = svc
            .issue("revoked-and-tripped", vec!["read".to_string()])
            .await
            .unwrap();
        svc.trip(issued.id).await.unwrap();
        svc.revoke(issued.id).await.unwrap();

        let lookup = svc.lookup(&issued.key).await.unwrap();
        match lookup {
            ApiKeyLookup::Revoked { id, .. } => assert_eq!(id, issued.id),
            other => panic!("expected Revoked, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn wrong_secret_against_a_tripped_key_is_still_not_found() {
        // Same info-leak guard as the revoked case (this module's doc
        // comment): a forged secret against a tripped key's prefix must not
        // distinguish itself from any other forged secret.
        let svc = service().await;
        let issued = svc
            .issue("tripped-forged", vec!["read".to_string()])
            .await
            .unwrap();
        svc.trip(issued.id).await.unwrap();

        let (prefix, _secret) = parse_key(&issued.key).unwrap();
        let forged = format!("bh_{prefix}_{}", generate_secret());
        let lookup = svc.lookup(&forged).await.unwrap();
        assert_eq!(lookup, ApiKeyLookup::NotFound);
    }

    #[tokio::test]
    async fn trip_unknown_id_is_not_found() {
        let svc = service().await;
        let err = svc.trip(999).await.unwrap_err();
        assert!(matches!(err, BantoError::NotFound { .. }));
    }

    #[tokio::test]
    async fn list_reflects_tripped_state() {
        let svc = service().await;
        let a = svc.issue("a", vec!["read".to_string()]).await.unwrap();
        svc.issue("b", vec!["read".to_string()]).await.unwrap();
        svc.trip(a.id).await.unwrap();

        let listed = svc.list().await.unwrap();
        let a = listed.iter().find(|k| k.name == "a").unwrap();
        assert!(a.tripped_at.is_some());
        let b = listed.iter().find(|k| k.name == "b").unwrap();
        assert!(b.tripped_at.is_none());
    }
}
