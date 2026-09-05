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
//! ## スコープ構文検証（設計 §5.6・T0-2 スコープ外の明示、H10 ③・T21 で拡張）
//!
//! `read`・`read:{connection}.{group}.{tag}`・`read:{connection}.{group}.*`・
//! `write:{connection}.{group}.{tag}`・`admin` のみを許可する。write
//! スコープはワイルドカード不可・3セグメントちょうど・各セグメント非空を
//! 発行時に検証するが、**実際の書き込み検査（T2）はここでは行わない**
//! （書き込み API 自体が T0-2 の時点でまだ存在しない）。
//!
//! `admin`（T21 S1-a、docs/banto-hub-t21-design.md §3.1）は、MCP から行う
//! 構成操作（接続/グループ/タグ CRUD・設定・API キー発行等）専用の独立
//! スコープ。read/write のタグ値アクセスとは**意図的に直交**しており、
//! `admin` だけを持つキーはタグの値を一切読み書きできない
//! （[`ApiKeyContext::has_admin_scope`] 参照）。T21 S1-b で MCP 構成補助
//! ツール（接続 CRUD: create/delete/list_connections）が既に `admin` を
//! 要求する形で配線済み。以降のスライスでグループ/タグ/設定へ拡張予定。
//!
//! ### read のタグ単位化（H10 ③、Option B、2026-08-08 オーナー決定・
//! docs/h10-3-read-scope-proposal.md §5 S1・§6）
//!
//! read は write と**意図的に非対称**: write の完全一致に加え、read に
//! 限り `{connection}.{group}.*` のグループ・ワイルドカードも許可する
//! （read は一括操作が自然で、`crate::subscribe_core::TagPattern::GroupWildcard`
//! と文法を揃えるため。write は誤書き込みの被害が大きく、引き続き
//! ワイルドカード不可・完全一致のみ）。
//!
//! per-tag read スコープが絞るのは**値の読み取り経路**（単一・バルク・
//! WebSocket/gRPC ストリーム）だけ — catalog（`GET /api/v1/tags`・gRPC
//! `GetCatalog`）は per-tag スコープの影響を受けず、read 系スコープを
//! 1つでも持つキーには常に全タグ（PLC アドレス込み）を返す（案 B の核、
//! 「発見 ≠ 値アクセス」。オーナー理由: 「PLC アドレスも見えた方が割り付け
//! ミスに気づきやすい」）。ゲートの二段構成は [`ApiKeyContext::has_any_read`]
//! （認証層: read 系ルートに入れるか）と [`ApiKeyContext::can_read_value`]
//! （値ハンドラ: 個々のタグの値を読めるか）に対応する。
//!
//! ## トリップ（T2-4、設計 §6-4・2026-08-05 決定）
//!
//! `tripped_at` は `revoked_at`（T0-2、不可逆の失効）とは**別の解除可能な
//! 状態**。書き込みレート制限（`crate::write_rate`）を超過したキーは
//! `crate::rest` の書き込みハンドラが [`ApiKeysService::trip`] を呼んで
//! トリップさせ、admin が管理 UI から [`ApiKeysService::clear_trip`] で
//! 手動解除する。[`ApiKeysService::lookup`] の判定順は
//! **ハッシュ一致 → revoked → tripped → expired**（この節見出しの上、
//! 「照合のタイミングについて」の情報漏洩防止の理由付けと同じ順序 - T0-2 の
//! revoked チェックがハッシュ一致より先に来てはならないのと同じ理由で、
//! tripped/expired チェックも revoked の後段に置く。expired が末尾に来る
//! 理由は次節「有効期限」参照）。トリップ中のキーは read/write いずれの
//! `/api/v1/*` リクエストも拒否される（`crate::rest::require_tag_space_auth`
//! 参照）。
//!
//! ## 有効期限（H10 ①、docs/improvement-plan.md・2026-08-08 オーナー決定）
//!
//! API キーは**既定で無期限**（`expires_at` 列が `NULL`）のまま — 主たる
//! 統制は引き続き失効（`revoke`）と `last_used_at` の監視。その上で、
//! キー発行時に**任意**で絶対 epoch ミリ秒の期限を設定できる
//! （`ApiKeysService::issue` の `expires_at` 引数、検証は
//! `crate::rest::api_keys_create` が「未来限定」を発行時点で行う）。
//!
//! 判定は [`is_expired`]（純関数、`now_ms >= expires_at_ms` で真）を
//! [`ApiKeysService::lookup`] の末尾 — ハッシュ一致・revoked・tripped の
//! いずれのチェックも通過した**後**、`Valid` を返す直前に置く。これより
//! 前段に置いてはいけない: 「照合のタイミングについて」節の情報漏洩防止の
//! 規律（ハッシュ不一致なら revoked/tripped の別を問わず常に `NotFound`）を
//! expired にも同じ形で及ぼす必要があるため（[`ApiKeyLookup::Expired`] の
//! doc comment参照）。UI 側の「期限接近」「長期未使用」警告は
//! `apps/banto-hub/src/lib/banto/apiKeysAdmin.ts` の `apiKeyWarnings`
//! （表示のみ・認可判断はしない）が担う。

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

/// 1つのスコープ文字列の構文検証（設計 §5.6、H10 ③ で read 側を拡張 -
/// このモジュールの doc comment「スコープ構文検証」参照）。
///
/// - `"read"`: そのまま許可（全タグ、従来どおり）。
/// - `"read:{connection}.{group}.{tag}"`: [`validate_read_scope`] へ委譲
///   （完全一致、`write:` と対称）。
/// - `"read:{connection}.{group}.*"`: [`validate_read_scope`] へ委譲
///   （グループ・ワイルドカード、read に限り許可）。
/// - `"write:{connection}.{group}.{tag}"`: ワイルドカード（`*`）禁止・
///   ピリオド区切りでちょうど3セグメント・各セグメント非空。
/// - `"admin"`: そのまま許可（T21 S1-a、構成操作専用 - このモジュールの
///   doc comment「スコープ構文検証」参照。read/write とは直交で、
///   データアクセスは付与しない）。
/// - それ以外は全て不正。
///
/// **実際の読み取り/書き込み可否判定（値ハンドラ側）はここでは行わない**
/// — ここは発行時の構文チェックのみ。
fn validate_scope(scope: &str) -> Result<(), String> {
    if scope == "read" {
        return Ok(());
    }
    if scope == "admin" {
        return Ok(());
    }
    if let Some(pattern) = scope.strip_prefix("read:") {
        return validate_read_scope(pattern, scope);
    }
    let Some(pattern) = scope.strip_prefix("write:") else {
        return Err(format!(
            "不明なスコープです（'read'、'read:{{connection}}.{{group}}.{{tag}}'、\
             'read:{{connection}}.{{group}}.*'、'write:{{connection}}.{{group}}.{{tag}}'、\
             'admin' のいずれかのみ許可）: {scope}"
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

/// `"read:"` の後半（`pattern`）の構文検証（H10 ③ S1、
/// docs/h10-3-read-scope-proposal.md §5・§6）。`scope` は元のスコープ文字列
/// 全体 - エラーメッセージにそのまま出す。
///
/// 2つの形を許可する:
/// - グループ・ワイルドカード `{connection}.{group}.*`: ちょうど3セグメント・
///   先頭2つ（connection/group）が非空・3番目が「`*`」の1文字ちょうど
///   （タグ名の一部だけを `*` にする「`temp*`」等は不可 - 末尾セグメント全体が
///   リテラルの `*` である場合のみワイルドカードとして認める）。
/// - 完全一致 `{connection}.{group}.{tag}`: `write:` と同じ厳密さ
///   （ワイルドカード禁止・ちょうど3セグメント・各セグメント非空）。
///
/// read/write が意図的に非対称である理由はこのモジュールの doc comment
/// 「read のタグ単位化」参照。
fn validate_read_scope(pattern: &str, scope: &str) -> Result<(), String> {
    let segments: Vec<&str> = pattern.split('.').collect();
    if segments.len() == 3
        && segments[2] == "*"
        && !segments[0].is_empty()
        && !segments[1].is_empty()
    {
        return Ok(());
    }
    // グループ・ワイルドカードの形に当てはまらなければ、write と同じ
    // 厳密さ（ワイルドカード禁止・完全一致3セグメント）を要求する。
    if pattern.contains('*') {
        return Err(format!(
            "read スコープのワイルドカードは {{connection}}.{{group}}.* の形のみ許可されています\
             （タグ名の一部だけを * にはできません）: {scope}"
        ));
    }
    if segments.len() != 3 || segments.iter().any(|segment| segment.is_empty()) {
        return Err(format!(
            "read スコープは {{connection}}.{{group}}.{{tag}}（完全一致）または \
             {{connection}}.{{group}}.*（グループ単位）の3セグメントで指定してください: {scope}"
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
    getrandom::fill(&mut buf).expect("システム乱数生成器の呼び出しに失敗しました");
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

/// H10 ①（docs/improvement-plan.md、2026-08-08 オーナー決定）: キーの
/// 有効期限切れ判定。`should_touch_last_used` と同じく DB/クロックに
/// 依存しない純関数（単体テスト対象）。呼び出し箇所・判定順は
/// [`ApiKeysService::lookup`] とこのモジュールの doc comment「有効期限」
/// 参照。
///
/// `expires_at_ms` が `None`（無期限、既定）なら常に `false`。`Some(e)` の
/// 場合は `now_ms >= e` で真（境界を含む - 期限ちょうどの瞬間は「期限切れ」
/// 側に倒す。`ApiKeySummary::created_at`/`revoked_at` の ISO 文字列とは違い
/// `expires_at` も `last_used_at` と同じ epoch ミリ秒の10進文字列で保存する
/// - db.rs の列コメント参照）。
pub fn is_expired(now_ms: i64, expires_at_ms: Option<i64>) -> bool {
    match expires_at_ms {
        None => false,
        Some(expires_at) => now_ms >= expires_at,
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
    /// H10 ①（docs/improvement-plan.md、2026-08-08 オーナー決定）: 任意の
    /// 有効期限（epoch ミリ秒）。`None` = 無期限（既定・動作不変）。
    /// `last_used_at` と同じく DB 保存形式（10進文字列）をそのまま数値化
    /// したもの - このモジュールの doc comment「有効期限」参照。UI 側の
    /// 警告表示（期限接近/長期未使用/期限切れ）はこの生値を使って
    /// `apps/banto-hub/src/lib/banto/apiKeysAdmin.ts` の `apiKeyWarnings`
    /// が計算する（サーバー側はここでは判定しない）。
    pub expires_at: Option<i64>,
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
    /// H10 ③（Option B、docs/h10-3-read-scope-proposal.md §6）: 認証層の
    /// 「`/api/v1/*` の read 系ルートに入れるか」のゲート。素の `read` か、
    /// 任意の `read:...`（完全一致・グループ・ワイルドカードいずれも）を
    /// 1つでも持っていれば true。write 専用キーは従来どおり false（403）
    /// のまま — `crate::rest::require_tag_space_auth`・
    /// `crate::grpc::GrpcService::authenticate` が呼ぶ、旧 `has_read_scope`
    /// が担っていたゲートをそのまま引き継ぐ（catalog はこのゲートだけで
    /// 完結し、個々のタグへの絞り込みは行わない - 案 B の核。このモジュール
    /// の doc comment「read のタグ単位化」参照）。
    pub fn has_any_read(&self) -> bool {
        self.scopes
            .iter()
            .any(|scope| scope == "read" || scope.starts_with("read:"))
    }

    /// H10 ③（Option B、docs/h10-3-read-scope-proposal.md §6）: 個々の
    /// タグの**値**を読めるか（catalog の可視性とは別軸 - catalog は絞らず、
    /// 値ハンドラだけがこれを使う）。素の `read`、`external_name` との
    /// 完全一致 `read:{external_name}`、または `read:{connection}.{group}.*`
    /// の `{connection}.{group}.` が `external_name` の前方一致、のいずれか
    /// で true。
    ///
    /// `external_name` は `{connection}.{group}.{tag}` で各セグメント内部に
    /// `.` を含まない（`crate::hub::build_catalog` の組み立て - `hub.rs:432`
    /// 付近 - による不変条件）ため、`"{connection}.{group}."`
    /// （末尾ドット込み）への前方一致は「グループ丸ごと」に対して安全に
    /// 効く。例えば `read:line1.fast.*` は `line1.fast.temp01` にはマッチ
    /// するが `line1.fastx.temp01` にはマッチしない（末尾ドットが
    /// `fast`/`fastx` の混同を防ぐ）。
    ///
    /// 注意（fail-closed、既知のトレードオフ - 設計提案書 §6）:
    /// `external_name` はタグの安定 id ではなくリネームで変わりうる。タグを
    /// リネームすると、そのタグ名を指す既存の `read:{name}` スコープは
    /// 新しい名前に自動追従しない（＝値が読めなくなる）。安全側に倒れる
    /// （見えなくなるだけで、意図しないタグが見えてしまうことはない）ため
    /// 許容する — 安定 id ベースの照合は複雑さに見合わないとして今回は
    /// 採らない（オーナー決定）。
    pub fn can_read_value(&self, external_name: &str) -> bool {
        self.scopes.iter().any(|scope| {
            if scope == "read" {
                return true;
            }
            let Some(pattern) = scope.strip_prefix("read:") else {
                return false;
            };
            match pattern.strip_suffix('*') {
                // `pattern` は発行時に validate_scope 済みなので、`*` 付きは
                // 必ず "{connection}.{group}.*" の形（strip_suffix('*') の
                // 結果は末尾ドット込みの "{connection}.{group}." になる）。
                Some(group_prefix) => external_name.starts_with(group_prefix),
                None => pattern == external_name,
            }
        })
    }

    /// T2-4（設計 §6 実装指示 §5「認証」）: `POST /api/v1/values/{tag}` は
    /// `write:{external_name}` の**完全一致**が必須（ワイルドカードは
    /// 発行時点で拒否済み、`validate_scope` 参照）。`read` スコープでは
    /// 書けない。
    pub fn has_write_scope(&self, external_name: &str) -> bool {
        let needle = format!("write:{external_name}");
        self.scopes.iter().any(|scope| scope == &needle)
    }

    /// T21 S1-a（docs/banto-hub-t21-design.md §3.1）: 構成補助 MCP ツール
    /// （接続/グループ/タグ CRUD・設定・API キー発行等）が要求する管理
    /// スコープを持つか。`admin` は read/write とは**直交** — `admin`
    /// だけを持つキーは [`has_any_read`](Self::has_any_read)・
    /// [`has_write_scope`](Self::has_write_scope) がいずれも false のまま
    /// で、タグの値の読み書きは一切できない。T21 S1-b で MCP 構成補助
    /// ツール（接続 CRUD）が既にこの判定を使って配線済み
    /// （`crate::mcp` の `require_admin_scope` 参照）。
    pub fn has_admin_scope(&self) -> bool {
        self.scopes.iter().any(|s| s == "admin")
    }
}

/// [`ApiKeysService::lookup`] の結果。`Revoked`/`Tripped`/`Expired` を
/// `NotFound` と分けるのは「失効済み/トリップ中/期限切れのキーでの
/// アクセス試行は audit_log に記録する」（設計 T0-2 実装指示、T2-4 で
/// `Tripped` に、H10 ①（docs/improvement-plan.md、2026-08-08 オーナー
/// 決定）で `Expired` に同じ扱いを拡張）ため — 呼び出し元
/// （`crate::rest`/`crate::grpc`）がこれらを区別して扱う。
///
/// ハッシュが一致しない場合は revoked/tripped/expired かどうかに関わらず
/// 常に `NotFound` を返す（`Revoked`/`Tripped`/`Expired` を返してしまうと、
/// secret を知らない攻撃者に「このプレフィックスは存在し、かつ失効済み/
/// トリップ中/期限切れだ」という情報を漏らすことになるため —
/// [`ApiKeysService::lookup`] の実装コメント参照）。
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
    /// H10 ①（docs/improvement-plan.md、2026-08-08 オーナー決定）: 任意で
    /// 設定された `expires_at` を過ぎたキー。`revoked_at`/`tripped_at` と
    /// 違い、admin が管理 UI から「解除」できる状態ではない（有効期限を
    /// 過ぎたキーは再び使うなら再発行する運用 - このモジュールの doc
    /// comment「有効期限」参照）。
    Expired {
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

/// `(id, name, prefix, scopes, created_at, last_used_at, revoked_at,
/// tripped_at, expires_at)` - H10 ①で末尾に `expires_at` を追加(`list`/
/// `fetch_summary` の SELECT 列順と一致させること)。
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

/// [`ApiKeysService::lookup`]'s row shape: `(id, name, key_hash, scopes,
/// last_used_at, revoked_at, tripped_at, expires_at)` - deliberately not
/// [`ApiKeyRow`] (which also carries `prefix`/`created_at` instead of
/// `key_hash`); the two queries select different columns for different
/// purposes. H10 ①で末尾に `expires_at` を追加。
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
    ///
    /// `expires_at`（H10 ①、docs/improvement-plan.md・2026-08-08 オーナー
    /// 決定）: 任意の絶対 epoch ミリ秒。`None` = 無期限（既定・動作不変）。
    /// **ここでは値の妥当性を再検証しない** - 「現在時刻より未来」の検証は
    /// 呼び出し元 `crate::rest::api_keys_create` が発行前に行う（この
    /// サービス層は `now_ms`/クロックを持たないため - このモジュールの
    /// doc comment「有効期限」参照）。
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

    /// 発行済みキーの一覧（`crate::rest`'s `GET /api/api-keys` - 設計:
    /// `key_hash` は返さない）。作成順。
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

    /// `Authorization: Bearer <token>` から渡された `bh_...` トークンを
    /// 照合する。`crate::rest`/`crate::grpc` の唯一の入口。
    ///
    /// **ハッシュ一致を revoked/tripped/expired チェックより先に行う**:
    /// prefix だけ一致して secret（＝ハッシュ）が一致しない場合は、その
    /// キーが失効済み/トリップ中/期限切れか有効かに関わらず常に
    /// [`ApiKeyLookup::NotFound`] を返す（[`ApiKeyLookup`] の doc comment
    /// 参照 - 情報漏洩防止)。判定順は
    /// **ハッシュ一致 → revoked → tripped → expired**
    /// （このモジュールの doc comment「トリップ」「有効期限」参照 - T0-2 と
    /// 同じ順序の規律）。
    ///
    /// `now_ms`（H10 ①）: 呼び出し元のクロック（`CollectorManager::clock()`）
    /// から渡す - [`is_expired`] の判定にのみ使う。既存の
    /// `last_used_at`/`touch_last_used` は今までどおり呼び出し元が自前で
    /// `now_ms` を取得して別途呼ぶ（このメソッドは触らない）。
    pub async fn lookup(&self, full_key: &str, now_ms: i64) -> Result<ApiKeyLookup, BantoError> {
        let Some((prefix, _secret)) = parse_key(full_key) else {
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
        assert!(validate_scope("unknown").is_err());
        assert!(validate_scope("").is_err());
    }

    // --- T21 S1-a: admin スコープ -------------------------------------------

    #[test]
    fn validate_scope_accepts_admin() {
        assert!(validate_scope("admin").is_ok());
    }

    // --- H10 ③: read のタグ単位スコープ構文（S1） -----------------------

    #[test]
    fn validate_scope_accepts_exact_read_scope() {
        assert!(validate_scope("read:line1.fast.temp01").is_ok());
    }

    #[test]
    fn validate_scope_accepts_group_wildcard_read_scope() {
        assert!(validate_scope("read:line1.fast.*").is_ok());
    }

    /// write と違い、read はグループ・ワイルドカードに限り許可される
    /// （このモジュールの doc comment「read のタグ単位化」参照）。
    #[test]
    fn validate_scope_still_rejects_wildcard_in_write() {
        assert!(validate_scope("write:line1.fast.*").is_err());
        assert!(validate_scope("write:*").is_err());
    }

    #[test]
    fn validate_scope_rejects_read_wildcard_with_wrong_segment_count() {
        // "read:a.*" - ちょうど2セグメントで、グループ・ワイルドカードの
        // 3セグメント形に当てはまらない。
        assert!(validate_scope("read:a.*").is_err());
        // 4セグメント(末尾がワイルドカードでも不可)。
        assert!(validate_scope("read:line1.fast.group.*").is_err());
    }

    #[test]
    fn validate_scope_rejects_read_with_wrong_segment_count() {
        assert!(validate_scope("read:a.b.c.d").is_err());
        assert!(validate_scope("read:line1.fast").is_err());
    }

    #[test]
    fn validate_scope_rejects_empty_read_scope() {
        assert!(validate_scope("read:").is_err());
    }

    #[test]
    fn validate_scope_rejects_read_with_empty_tag_segment() {
        assert!(validate_scope("read:a.b.").is_err());
    }

    #[test]
    fn validate_scope_rejects_read_with_empty_connection_or_group_segment() {
        assert!(validate_scope("read:.fast.temp01").is_err());
        assert!(validate_scope("read:line1..temp01").is_err());
        // ワイルドカード形でも先頭2セグメントは非空が必須。
        assert!(validate_scope("read:.fast.*").is_err());
        assert!(validate_scope("read:line1..*").is_err());
    }

    #[test]
    fn validate_scope_rejects_partial_wildcard_in_read_tag_segment() {
        // 末尾セグメント丸ごとが `*` の場合のみワイルドカード - タグ名の
        // 一部だけを `*` にはできない。
        assert!(validate_scope("read:line1.fast.te*mp").is_err());
        assert!(validate_scope("read:line1.fast.*temp01").is_err());
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

    // --- H10 ①: 有効期限判定（純関数） -----------------------------------

    #[test]
    fn is_expired_is_false_when_unlimited() {
        assert!(!is_expired(1_000_000, None));
        assert!(!is_expired(i64::MAX, None));
    }

    #[test]
    fn is_expired_is_false_before_the_deadline() {
        assert!(!is_expired(999_999, Some(1_000_000)));
    }

    #[test]
    fn is_expired_is_true_at_and_after_the_deadline() {
        // 境界含む(ちょうど期限の瞬間は「期限切れ」側)。
        assert!(is_expired(1_000_000, Some(1_000_000)));
        assert!(is_expired(1_000_001, Some(1_000_000)));
    }

    // --- サービス: issue/list/revoke/lookup ------------------------------

    #[tokio::test]
    async fn issue_then_lookup_round_trips() {
        let svc = service().await;
        let issued = svc
            .issue("mes-gateway", vec!["read".to_string()], None)
            .await
            .expect("issue should succeed");
        assert!(issued.key.starts_with("bh_"));

        let lookup = svc
            .lookup(&issued.key, 1_000_000)
            .await
            .expect("lookup should succeed");
        match lookup {
            ApiKeyLookup::Valid(ctx) => {
                assert_eq!(ctx.id, issued.id);
                assert_eq!(ctx.name, "mes-gateway");
                assert!(ctx.has_any_read());
                assert_eq!(ctx.last_used_at_ms, None);
            }
            other => panic!("expected Valid, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn lookup_with_wrong_secret_is_not_found() {
        let svc = service().await;
        let issued = svc
            .issue("mes-gateway", vec!["read".to_string()], None)
            .await
            .unwrap();
        let (prefix, _secret) = parse_key(&issued.key).unwrap();
        let forged = format!("bh_{prefix}_{}", generate_secret());

        let lookup = svc.lookup(&forged, 1_000_000).await.unwrap();
        assert_eq!(lookup, ApiKeyLookup::NotFound);
    }

    #[tokio::test]
    async fn lookup_unknown_prefix_is_not_found() {
        let svc = service().await;
        let lookup = svc
            .lookup(
                &format!("bh_{}_{}", generate_prefix(), generate_secret()),
                1_000_000,
            )
            .await
            .unwrap();
        assert_eq!(lookup, ApiKeyLookup::NotFound);
    }

    #[tokio::test]
    async fn issue_rejects_invalid_scope_syntax() {
        let svc = service().await;
        let err = svc
            .issue("bad-scope", vec!["write:line1.fast.*".to_string()], None)
            .await
            .unwrap_err();
        assert!(matches!(err, BantoError::Validation { .. }));
    }

    #[tokio::test]
    async fn issue_rejects_duplicate_name() {
        let svc = service().await;
        svc.issue("dup", vec!["read".to_string()], None)
            .await
            .unwrap();
        let err = svc
            .issue("dup", vec!["read".to_string()], None)
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
            .issue("revoke-me", vec!["read".to_string()], None)
            .await
            .unwrap();

        let summary = svc.revoke(issued.id).await.expect("revoke should succeed");
        assert!(summary.revoked_at.is_some());

        let lookup = svc.lookup(&issued.key, 1_000_000).await.unwrap();
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
            .issue("idempotent", vec!["read".to_string()], None)
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
            .issue("revoked-forged", vec!["read".to_string()], None)
            .await
            .unwrap();
        svc.revoke(issued.id).await.unwrap();

        let (prefix, _secret) = parse_key(&issued.key).unwrap();
        let forged = format!("bh_{prefix}_{}", generate_secret());
        let lookup = svc.lookup(&forged, 1_000_000).await.unwrap();
        assert_eq!(lookup, ApiKeyLookup::NotFound);
    }

    #[tokio::test]
    async fn list_reflects_issued_and_revoked_keys() {
        let svc = service().await;
        svc.issue("a", vec!["read".to_string()], None)
            .await
            .unwrap();
        let b = svc
            .issue("b", vec!["read".to_string()], None)
            .await
            .unwrap();
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
            .issue("throttled", vec!["read".to_string()], None)
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

    // --- H10 ③: has_any_read / can_read_value ------------------------------

    fn ctx_with(scopes: &[&str]) -> ApiKeyContext {
        ApiKeyContext {
            id: 1,
            name: "k".to_string(),
            scopes: scopes.iter().map(|s| s.to_string()).collect(),
            last_used_at_ms: None,
        }
    }

    #[test]
    fn has_any_read_is_true_for_bare_read() {
        assert!(ctx_with(&["read"]).has_any_read());
    }

    #[test]
    fn has_any_read_is_true_for_any_read_colon_scope() {
        assert!(ctx_with(&["read:line1.fast.temp01"]).has_any_read());
        assert!(ctx_with(&["read:line1.fast.*"]).has_any_read());
    }

    #[test]
    fn has_any_read_is_false_for_write_only_key() {
        assert!(!ctx_with(&["write:line1.fast.temp01"]).has_any_read());
    }

    #[test]
    fn has_any_read_is_false_for_a_key_with_no_scopes_at_all() {
        assert!(!ctx_with(&[]).has_any_read());
    }

    /// 素の `read` は全タグの値を読める(後方互換 - このモジュールの doc
    /// comment「read のタグ単位化」S2 参照)。
    #[test]
    fn can_read_value_bare_read_matches_anything() {
        let ctx = ctx_with(&["read"]);
        assert!(ctx.can_read_value("line1.fast.temp01"));
        assert!(ctx.can_read_value("line2.slow.press01"));
    }

    #[test]
    fn can_read_value_exact_scope_matches_only_that_tag() {
        let ctx = ctx_with(&["read:line1.fast.temp01"]);
        assert!(ctx.can_read_value("line1.fast.temp01"));
        assert!(!ctx.can_read_value("line1.fast.temp02"));
        assert!(!ctx.can_read_value("line2.slow.press01"));
    }

    #[test]
    fn can_read_value_group_wildcard_matches_every_tag_in_that_group_only() {
        let ctx = ctx_with(&["read:line1.fast.*"]);
        assert!(ctx.can_read_value("line1.fast.temp01"));
        assert!(ctx.can_read_value("line1.fast.temp02"));
        // 別グループ・別接続はマッチしない。
        assert!(!ctx.can_read_value("line1.slow.temp01"));
        assert!(!ctx.can_read_value("line2.fast.temp01"));
    }

    /// 前方一致の末尾ドットが `fast`/`fastx` のような接頭辞衝突を防ぐこと
    /// の回帰防止（[`ApiKeyContext::can_read_value`] の doc comment参照）。
    #[test]
    fn can_read_value_group_wildcard_does_not_prefix_collide_with_a_similarly_named_group() {
        let ctx = ctx_with(&["read:line1.fast.*"]);
        assert!(!ctx.can_read_value("line1.fastx.temp01"));
    }

    #[test]
    fn can_read_value_is_false_when_no_scope_matches() {
        let ctx = ctx_with(&["read:line1.fast.temp01", "write:line2.slow.press01"]);
        assert!(!ctx.can_read_value("line2.slow.press01"));
    }

    #[test]
    fn can_read_value_is_false_for_a_key_with_no_read_scopes_at_all() {
        assert!(!ctx_with(&["write:line1.fast.temp01"]).can_read_value("line1.fast.temp01"));
        assert!(!ctx_with(&[]).can_read_value("line1.fast.temp01"));
    }

    #[test]
    fn can_read_value_multiple_scopes_union() {
        let ctx = ctx_with(&["read:line1.fast.temp01", "read:line2.slow.*"]);
        assert!(ctx.can_read_value("line1.fast.temp01"));
        assert!(ctx.can_read_value("line2.slow.press01"));
        assert!(!ctx.can_read_value("line1.fast.temp02"));
        assert!(!ctx.can_read_value("line3.fast.temp01"));
    }

    // --- T21 S1-a: has_admin_scope ------------------------------------------

    #[test]
    fn has_admin_scope_is_true_when_admin_is_present() {
        assert!(ctx_with(&["admin"]).has_admin_scope());
        assert!(ctx_with(&["read", "admin"]).has_admin_scope());
    }

    #[test]
    fn has_admin_scope_is_false_without_admin() {
        assert!(!ctx_with(&["read"]).has_admin_scope());
        assert!(!ctx_with(&["write:line1.fast.temp01"]).has_admin_scope());
        assert!(!ctx_with(&[]).has_admin_scope());
    }

    /// admin は read/write と直交 - admin だけを持つキーはデータの
    /// 読み書きを一切許可しない（このモジュールの doc comment
    /// 「スコープ構文検証」参照）。
    #[test]
    fn admin_only_key_has_no_read_or_write_access() {
        let ctx = ctx_with(&["admin"]);
        assert!(!ctx.has_any_read());
        assert!(!ctx.can_read_value("line1.fast.temp01"));
        assert!(!ctx.has_write_scope("line1.fast.temp01"));
    }

    // --- T2-4: trip/clear_trip/lookup ordering -----------------------------

    #[tokio::test]
    async fn trip_then_lookup_reports_tripped_not_valid_or_not_found() {
        let svc = service().await;
        let issued = svc
            .issue("writer", vec!["write:line1.fast.temp01".to_string()], None)
            .await
            .unwrap();

        let summary = svc.trip(issued.id).await.expect("trip should succeed");
        assert!(summary.tripped_at.is_some());
        assert!(summary.revoked_at.is_none(), "trip must not revoke");

        let lookup = svc.lookup(&issued.key, 1_000_000).await.unwrap();
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
            .issue("idempotent-trip", vec!["read".to_string()], None)
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
            .issue("clear-me", vec!["read".to_string()], None)
            .await
            .unwrap();
        svc.trip(issued.id).await.unwrap();

        let cleared = svc
            .clear_trip(issued.id)
            .await
            .expect("clear_trip should succeed");
        assert!(cleared.tripped_at.is_none());

        let lookup = svc.lookup(&issued.key, 1_000_000).await.unwrap();
        match lookup {
            ApiKeyLookup::Valid(ctx) => assert_eq!(ctx.id, issued.id),
            other => panic!("expected Valid after clear_trip, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn clear_trip_is_idempotent() {
        let svc = service().await;
        let issued = svc
            .issue("clear-idempotent", vec!["read".to_string()], None)
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
            .issue("revoked-and-tripped", vec!["read".to_string()], None)
            .await
            .unwrap();
        svc.trip(issued.id).await.unwrap();
        svc.revoke(issued.id).await.unwrap();

        let lookup = svc.lookup(&issued.key, 1_000_000).await.unwrap();
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
            .issue("tripped-forged", vec!["read".to_string()], None)
            .await
            .unwrap();
        svc.trip(issued.id).await.unwrap();

        let (prefix, _secret) = parse_key(&issued.key).unwrap();
        let forged = format!("bh_{prefix}_{}", generate_secret());
        let lookup = svc.lookup(&forged, 1_000_000).await.unwrap();
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
        let a = svc
            .issue("a", vec!["read".to_string()], None)
            .await
            .unwrap();
        svc.issue("b", vec!["read".to_string()], None)
            .await
            .unwrap();
        svc.trip(a.id).await.unwrap();

        let listed = svc.list().await.unwrap();
        let a = listed.iter().find(|k| k.name == "a").unwrap();
        assert!(a.tripped_at.is_some());
        let b = listed.iter().find(|k| k.name == "b").unwrap();
        assert!(b.tripped_at.is_none());
    }

    // --- H10 ①: expires_at / lookup ordering -------------------------------

    /// 無期限キー（`expires_at: None`）は今までどおり - 期限判定を追加した
    /// ことで既定動作が変わっていないことの回帰防止（実装指示: 「無期限
    /// キーの従来動作不変」）。`now_ms` にどれだけ大きな値を渡しても
    /// `Valid` のまま。
    #[tokio::test]
    async fn unlimited_key_is_valid_regardless_of_now_ms() {
        let svc = service().await;
        let issued = svc
            .issue("unlimited", vec!["read".to_string()], None)
            .await
            .unwrap();

        let lookup = svc.lookup(&issued.key, i64::MAX).await.unwrap();
        match lookup {
            ApiKeyLookup::Valid(ctx) => assert_eq!(ctx.id, issued.id),
            other => panic!("expected Valid, got {other:?}"),
        }
    }

    /// 期限付きキー: 期限前は `Valid`、期限ちょうど/以降は `Expired`
    /// （実装指示の受け入れ条件: 「期限切れキーの 401 と... テスト」の
    /// サービス層側）。
    #[tokio::test]
    async fn expiring_key_is_valid_before_and_expired_at_or_after_the_deadline() {
        let svc = service().await;
        let issued = svc
            .issue("expiring", vec!["read".to_string()], Some(2_000_000))
            .await
            .unwrap();

        let before = svc.lookup(&issued.key, 1_999_999).await.unwrap();
        match before {
            ApiKeyLookup::Valid(ctx) => assert_eq!(ctx.id, issued.id),
            other => panic!("expected Valid before the deadline, got {other:?}"),
        }

        let at_deadline = svc.lookup(&issued.key, 2_000_000).await.unwrap();
        match at_deadline {
            ApiKeyLookup::Expired { id, name } => {
                assert_eq!(id, issued.id);
                assert_eq!(name, "expiring");
            }
            other => panic!("expected Expired at the deadline, got {other:?}"),
        }

        let after = svc.lookup(&issued.key, 2_000_001).await.unwrap();
        assert!(matches!(after, ApiKeyLookup::Expired { .. }));
    }

    /// 判定順: ハッシュ一致 -> revoked -> tripped -> expired。失効済みかつ
    /// 期限切れのキーは `Revoked` を報告する（`revoked_key_is_reported_
    /// as_revoked_even_if_also_tripped` と同じ精度で expired にも確認）。
    #[tokio::test]
    async fn revoked_key_is_reported_as_revoked_even_if_also_expired() {
        let svc = service().await;
        let issued = svc
            .issue("revoked-and-expired", vec!["read".to_string()], Some(1))
            .await
            .unwrap();
        svc.revoke(issued.id).await.unwrap();

        let lookup = svc.lookup(&issued.key, 1_000_000).await.unwrap();
        match lookup {
            ApiKeyLookup::Revoked { id, .. } => assert_eq!(id, issued.id),
            other => panic!("expected Revoked, got {other:?}"),
        }
    }

    /// トリップ中かつ期限切れのキーは `Tripped` を報告する（tripped の判定が
    /// expired より先に来る、というこのモジュールの doc comment の順序を
    /// 固定する）。
    #[tokio::test]
    async fn tripped_key_is_reported_as_tripped_even_if_also_expired() {
        let svc = service().await;
        let issued = svc
            .issue("tripped-and-expired", vec!["read".to_string()], Some(1))
            .await
            .unwrap();
        svc.trip(issued.id).await.unwrap();

        let lookup = svc.lookup(&issued.key, 1_000_000).await.unwrap();
        match lookup {
            ApiKeyLookup::Tripped { id, .. } => assert_eq!(id, issued.id),
            other => panic!("expected Tripped, got {other:?}"),
        }
    }

    /// Info-leak guard（このモジュールの doc comment参照）: 期限切れキーの
    /// prefix に対する偽造 secret は、他の偽造と同じく常に `NotFound` -
    /// `wrong_secret_against_a_revoked_key_is_still_not_found`/
    /// `wrong_secret_against_a_tripped_key_is_still_not_found` と同型。
    #[tokio::test]
    async fn wrong_secret_against_an_expired_key_is_still_not_found() {
        let svc = service().await;
        let issued = svc
            .issue("expired-forged", vec!["read".to_string()], Some(1))
            .await
            .unwrap();

        let (prefix, _secret) = parse_key(&issued.key).unwrap();
        let forged = format!("bh_{prefix}_{}", generate_secret());
        let lookup = svc.lookup(&forged, 1_000_000).await.unwrap();
        assert_eq!(lookup, ApiKeyLookup::NotFound);
    }

    /// `list`/`ApiKeySummary` にも `expires_at` がそのまま(epoch ミリ秒)で
    /// 反映される。
    #[tokio::test]
    async fn list_reflects_expires_at() {
        let svc = service().await;
        svc.issue("unlimited", vec!["read".to_string()], None)
            .await
            .unwrap();
        svc.issue("expiring", vec!["read".to_string()], Some(2_000_000))
            .await
            .unwrap();

        let listed = svc.list().await.unwrap();
        let unlimited = listed.iter().find(|k| k.name == "unlimited").unwrap();
        assert_eq!(unlimited.expires_at, None);
        let expiring = listed.iter().find(|k| k.name == "expiring").unwrap();
        assert_eq!(expiring.expires_at, Some(2_000_000));
    }
}
