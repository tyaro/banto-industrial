//! API キー基盤 (docs/hub/tag-server-design.md §5.6「認証（全プロトコル共通）」、
//! T0-2 実装指示 §1「API キー基盤」)。`/api/v1/*` を叩く機械クライアント
//! （MES/クラウド収集/自作ダッシュボード等）向けの、管理 UI セッションとは
//! 別系統の認証情報。管理 API（`/api/api-keys/*`、admin ロール限定）で
//! 発行・一覧・失効を行い、実際の照合は `crate::rest` の `/api/v1/*`
//! ミドルウェアがこのモジュールの [`ApiKeysService::lookup`] を呼んで行う。
//!
//! ## 共有 crate `banto-api-keys` との分担（#1161 PR3、
//! docs/chronogazer/chronogazer-agent-api-design.md §3.1・§3.2）
//!
//! アプリに依らない部分は共有 crate `banto_api_keys` にある（ChronoGazer と
//! 共有する）: キーの形式 `bh_{prefix}_{secret}` の生成と固定長の読み取り、
//! SHA-256 のハッシュと定数時間の比較（argon2 にしない理由もそちらの doc
//! comment）、照合の順と結果の型、表 `api_keys` への SQL、拒否の HTTP 応答、
//! 監査の主体の表示 `名前（prefix）`。このモジュールは同じ名前で re-export
//! する（[`ApiKeysService`] は共有のサービスに Hub のスコープ [`HubScopes`] を
//! 渡した型の別名）ので、呼び出し側の型の名前は移す前と同じ。
//!
//! Hub に残る物: キーの接頭辞 `bh`、スコープの書式（[`HubScopes`] の
//! `validate_scope`）と判定（[`HubScopeChecks`]。`ApiKeyContext` は共有 crate の
//! 型なので、判定は拡張 trait のメソッドにした - 呼び出し側は
//! `use crate::api_keys::HubScopeChecks` が要る）、一時停止（トリップ）を付ける
//! 条件（書き込みのレート超過）、表を作るマイグレーション `0101_api_keys.sql`、
//! 監査の書き方、`/api/v1/*`・MCP・gRPC の配線、照合できないときのログ
//! （`crate::rest::api_key_rejection_response`）。
//!
//! ## スコープ構文検証（設計 §5.6・T0-2 スコープ外の明示、H10 ③・T21 で拡張）
//!
//! `read`・`read:{connection}.{group}.{tag}`・`read:{connection}.{group}.*`・
//! `write:{connection}.{group}.{tag}`・`admin`・`ingest:#{id}`（R2a-2b、下の
//! 「取り込み」）のみを許可する。write
//! スコープはワイルドカード不可・3セグメントちょうど・各セグメント非空を
//! 発行時に検証するが、**実際の書き込み検査（T2）はここでは行わない**
//! （書き込み API 自体が T0-2 の時点でまだ存在しない）。
//!
//! `admin`（T21 S1-a、docs/hub/banto-hub-t21-design.md §3.1）は、MCP から行う
//! 構成操作（接続/グループ/タグ CRUD・設定・API キー発行等）専用の独立
//! スコープ。read/write のタグ値アクセスとは**意図的に直交**しており、
//! `admin` だけを持つキーはタグの値を一切読み書きできない
//! （[`HubScopeChecks::has_admin_scope`] 参照）。T21 S1-b で MCP 構成補助
//! ツール（接続 CRUD: create/delete/list_connections）が既に `admin` を
//! 要求する形で配線済み。以降のスライスでグループ/タグ/設定へ拡張予定。
//!
//! ### 取り込み `ingest:#{id}`（R2a-2b、docs/hub/banto-hub-ingest-design.md §5・
//! §12 Q9、2026-10-21 オーナー決定）
//!
//! `ingest:#{id}` は `push` 接続（`plc_connections.id` = `{id}`）へ値を送る
//! 権限。`{id}` は 10 進の正規形だけを受ける（`[1-9][0-9]*`・`i64` の範囲。
//! 先頭の 0・符号・空白・`*` は断る - [`parse_ingest_scope`]）。正規形に限る
//! ので、判定（[`HubScopeChecks::can_ingest`]）は文字列の完全一致で済む。
//! 発行時は構文だけを確かめ、接続の存在は確かめない（read / write と同じ）。
//!
//! - **名前ではなく id で縛る**（Q9 = (b)）: 接続を改名してもキーは通り続け、
//!   削除した接続の id は AUTOINCREMENT で再利用されない（設計 §5.1）ので、
//!   削除して同じ名前で作り直しても古いキーは通らない。`#` は「名前ではなく
//!   id」であることを文字列の上で示す。接続名は `#` で始まりうるので、
//!   **`ingest:` の後ろに名前を置く形は今後も足さない**（足すなら別の接頭辞）。
//! - **read / write / admin と直交**（設計 §5「判定」）: `ingest:` だけの
//!   キーは [`HubScopeChecks::has_any_read`]・[`HubScopeChecks::can_read_value`]・
//!   [`HubScopeChecks::has_write_scope`]・[`HubScopeChecks::has_admin_scope`] が
//!   すべて false（タグの値を読めず、PLC へ書けず、構成も触れない）。逆に
//!   `read` / `write:` / `admin` のキーは `can_ingest` が false で、**`admin` も
//!   送れない**（`admin` は `ingest:` を含意しない）。どの判定も接頭辞
//!   （`read` / `read:` / `write:` / `admin`）の一致で見るので、`ingest:` の
//!   文字列が他の判定に当たることは無い。
//! - 送信口（`/api/v1/ingest/{id}/…`、R2a-2c / 2d）はまだ無い。`can_ingest`
//!   はその認証層が、パスの id とこのスコープを照合するために使う。
//!
//! ### read のタグ単位化（H10 ③、Option B、2026-08-08 オーナー決定・
//! docs/archive/h10-3-read-scope-proposal.md §5 S1・§6）
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
//! ミスに気づきやすい」）。ゲートの二段構成は [`HubScopeChecks::has_any_read`]
//! （認証層: read 系ルートに入れるか）と [`HubScopeChecks::can_read_value`]
//! （値ハンドラ: 個々のタグの値を読めるか）に対応する。
//!
//! ## トリップ（T2-4、設計 §6-4・2026-08-05 決定）
//!
//! `tripped_at` は `revoked_at`（T0-2、不可逆の失効）とは**別の解除可能な
//! 状態**。書き込みレート制限（`crate::write_rate`）を超過したキーは
//! `crate::rest` の書き込みハンドラが [`ApiKeysService::trip`] を呼んで
//! トリップさせ、admin が管理 UI から [`ApiKeysService::clear_trip`] で
//! 手動解除する。[`ApiKeysService::lookup`] の判定順は
//! **ハッシュ一致 → revoked → tripped → expired**（
//! 共有 crate の「照合の順」の情報漏洩防止の理由付けと同じ順序 - T0-2 の
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
//! 前段に置いてはいけない: 共有 crate の「照合の順」の情報漏洩防止の
//! 規律（ハッシュ不一致なら revoked/tripped の別を問わず常に `NotFound`）を
//! expired にも同じ形で及ぼす必要があるため（[`ApiKeyLookup::Expired`] の
//! doc comment参照）。UI 側の「期限接近」「長期未使用」警告は
//! `apps/banto-hub/src/lib/banto/apiKeysAdmin.ts` の `apiKeyWarnings`
//! （表示のみ・認可判断はしない）が担う。
//!
//! ## 照合できないことと、キーが無効なことを区別する（#434、2026-09-24）
//!
//! 3 つの経路（MCP・REST のタグ空間 `/api/v1/*` とサイドカー用の 2 エンド
//! ポイント・gRPC）は [`ApiKeysService::lookup`] を直接呼ばず、
//! [`ApiKeysService::check`] を通す。`check` は照合に
//! [`API_KEY_CHECK_TIMEOUT`]（5 秒）の上限を掛け、結果を [`classify_lookup`]
//! （純関数）で 2 つに分ける:
//!
//! - [`ApiKeyCheck::Answered`]: DB が答えた（有効・失効・トリップ・期限切れ・
//!   存在しない）。無効なキーは従来どおり 401 / `UNAUTHENTICATED`（トリップは
//!   403 `key_tripped` / `PERMISSION_DENIED`。#435 で MCP と `/api/sink/*` も
//!   401 から 403 にそろえた）。
//! - [`ApiKeyCheck::Unavailable`]: 照合そのものができなかった（DB エラー・
//!   タイムアウト）。REST と MCP は 500（`ApiError`、#431 でセッションの照合
//!   失敗に返しているのと同じ形）、gRPC は `UNAVAILABLE`。以前は 401 だった
//!   ため、401 を「キーが無効になった」と解釈するクライアントが一時的な障害で
//!   再試行をやめたり、キーを捨てたりしうる。
//!
//! 応答への変換は、3 経路とも [`api_key_verdict`]（純関数。使える / どう拒否
//! するか + 監査用の情報）を通し、HTTP（REST・MCP）は
//! `crate::rest::api_key_rejection_response`、gRPC は
//! `crate::grpc::api_key_rejection_status` の 1 か所で決める（#435）。

use banto_api_keys::ApiKeyScopes;

pub use banto_api_keys::{
    api_key_verdict, classify_lookup, is_expired, should_touch_last_used, ApiKeyCheck,
    ApiKeyContext, ApiKeyDeleteOutcome, ApiKeyLookup, ApiKeyRejection, ApiKeySummary, DeniedKey,
    IssuedApiKey, UnauthenticatedReason, API_KEY_CHECK_TIMEOUT,
};

/// Hub の API キーのサービス（共有 crate のサービスに Hub のスコープを渡した
/// 型の別名。`ApiKeysService::new(pool)` は移す前と同じ形で使える）。
pub type ApiKeysService = banto_api_keys::ApiKeysService<HubScopes>;

/// Hub がキーに決めること: 接頭辞 `bh` とスコープの書式（このモジュールの
/// doc comment「スコープ構文検証」）。値を持たない印の型。
#[derive(Debug, Clone, Copy)]
pub struct HubScopes;

impl ApiKeyScopes for HubScopes {
    const KEY_PREFIX: &'static str = "bh";

    fn validate_scope(scope: &str) -> Result<(), String> {
        validate_scope(scope)
    }
}

// --- スコープ構文検証 -------------------------------------------------------

/// `ingest:#{id}` の接頭辞（R2a-2b、このモジュールの doc comment「取り込み」）。
pub const INGEST_SCOPE_PREFIX: &str = "ingest:#";

/// `ingest:#{id}` の `{id}` を取り出す（R2a-2b、docs/hub/banto-hub-ingest-design.md
/// §5「文法」）。正規形だけを受ける: `ingest:#` の直後が `[1-9][0-9]*` で
/// 終わり、`i64` に収まること。`ingest:`・`ingest:#`・`ingest:#0`・
/// `ingest:#012`・`ingest:#-1`・`ingest:#+1`・`ingest:# 12`・`ingest:#12x`・
/// `ingest:#*`・`ingest:Bridge1`（名前）・`i64` を超える数は `None`。
///
/// 発行時の検証（[`validate_scope`]）と、一覧の表示で id を取り出す側
/// （`crate::mcp` の `list_api_keys`）が同じこの関数を使う。
pub fn parse_ingest_scope(scope: &str) -> Option<i64> {
    parse_canonical_connection_id(scope.strip_prefix(INGEST_SCOPE_PREFIX)?)
}

/// 接続 id の 10 進の正規形（`[1-9][0-9]*`・`i64` の範囲）だけを受ける。
/// [`parse_ingest_scope`] の `#` の後ろと、送信口のパスの `{id}`
/// （R2a-2c、`crate::ingest::auth`。docs/hub/banto-hub-ingest-design.md §6 の
/// 404 `unknown_connection`（パスの形））が同じこの関数を使う - スコープの
/// 文字列とパスの id が同じ規則で 1 対 1 に対応するように。
pub fn parse_canonical_connection_id(digits: &str) -> Option<i64> {
    let mut bytes = digits.bytes();
    if !matches!(bytes.next(), Some(b'1'..=b'9')) {
        return None;
    }
    if !bytes.all(|b| b.is_ascii_digit()) {
        return None;
    }
    // 先頭が 1〜9・残りが ASCII の数字だけなので、失敗するのは i64 の
    // 範囲を超えるときだけ。
    digits.parse::<i64>().ok()
}

/// 接続 id から `ingest:#{id}` を組み立てる（[`parse_ingest_scope`] の逆）。
pub fn ingest_scope(connection_id: i64) -> String {
    format!("{INGEST_SCOPE_PREFIX}{connection_id}")
}

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
/// - `"ingest:#{id}"`: [`parse_ingest_scope`] の正規形だけ（R2a-2b、この
///   モジュールの doc comment「取り込み」参照）。`ingest:` の後ろが `#` で
///   始まらない（名前など）ものは別の文言で断る。
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
    if let Some(target) = scope.strip_prefix("ingest:") {
        return validate_ingest_scope(target, scope);
    }
    let Some(pattern) = scope.strip_prefix("write:") else {
        return Err(format!(
            "不明なスコープです（'read'、'read:{{connection}}.{{group}}.{{tag}}'、\
             'read:{{connection}}.{{group}}.*'、'write:{{connection}}.{{group}}.{{tag}}'、\
             'admin'、'ingest:#{{接続 id}}' のいずれかのみ許可）: {scope}"
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
/// docs/archive/h10-3-read-scope-proposal.md §5・§6）。`scope` は元のスコープ文字列
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

/// `"ingest:"` の後半（`target`）の構文検証（R2a-2b）。判定そのものは
/// [`parse_ingest_scope`]。`target` が `#` で始まらないもの（`ingest:Bridge1`
/// のような名前・空）は「id で指定する」ことを、`#` の後ろが正規形でない
/// ものは id の形を、それぞれ文言で示す。
fn validate_ingest_scope(target: &str, scope: &str) -> Result<(), String> {
    if parse_ingest_scope(scope).is_some() {
        return Ok(());
    }
    if !target.starts_with('#') {
        return Err(format!(
            "ingest スコープは接続の id で ingest:#{{id}} の形で指定してください\
             （接続名では指定できません）: {scope}"
        ));
    }
    Err(format!(
        "ingest スコープの id は 1 以上の 10 進の整数で指定してください\
         （先頭の 0・符号・空白・ワイルドカードは使えません）: {scope}"
    ))
}

// --- スコープの判定 -----------------------------------------------------------

/// Hub のスコープの判定（`ApiKeyContext` は共有 crate の型なので、inherent の
/// メソッドではなくこの拡張 trait で足す。判定の中身は移す前と同じ）。
pub trait HubScopeChecks {
    /// H10 ③（Option B、docs/archive/h10-3-read-scope-proposal.md §6）: 認証層の
    /// 「`/api/v1/*` の read 系ルートに入れるか」のゲート。素の `read` か、
    /// 任意の `read:...`（完全一致・グループ・ワイルドカードいずれも）を
    /// 1つでも持っていれば true。write 専用キーは従来どおり false（403）
    /// のまま — `crate::rest::require_tag_space_auth`・
    /// `crate::grpc::GrpcService::authenticate` が呼ぶ、旧 `has_read_scope`
    /// が担っていたゲートをそのまま引き継ぐ（catalog はこのゲートだけで
    /// 完結し、個々のタグへの絞り込みは行わない - 案 B の核。このモジュール
    /// の doc comment「read のタグ単位化」参照）。
    fn has_any_read(&self) -> bool;

    /// H10 ③（Option B、docs/archive/h10-3-read-scope-proposal.md §6）: 個々の
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
    fn can_read_value(&self, external_name: &str) -> bool;

    /// T2-4（設計 §6 実装指示 §5「認証」）: `POST /api/v1/values/{tag}` は
    /// `write:{external_name}` の**完全一致**が必須（ワイルドカードは
    /// 発行時点で拒否済み、`validate_scope` 参照）。`read` スコープでは
    /// 書けない。
    fn has_write_scope(&self, external_name: &str) -> bool;

    /// T21 S1-a（docs/hub/banto-hub-t21-design.md §3.1）: 構成補助 MCP ツール
    /// （接続/グループ/タグ CRUD・設定・API キー発行等）が要求する管理
    /// スコープを持つか。`admin` は read/write とは**直交** — `admin`
    /// だけを持つキーは [`has_any_read`](Self::has_any_read)・
    /// [`has_write_scope`](Self::has_write_scope) がいずれも false のまま
    /// で、タグの値の読み書きは一切できない。T21 S1-b で MCP 構成補助
    /// ツール（接続 CRUD）が既にこの判定を使って配線済み
    /// （`crate::mcp` の `require_admin_scope` 参照）。
    fn has_admin_scope(&self) -> bool;

    /// R2a-2b（docs/hub/banto-hub-ingest-design.md §5「判定」）: `push` 接続
    /// `connection_id` へ値を送れるか。`ingest:#{connection_id}` の**完全
    /// 一致**だけで true（ワイルドカードは無い）。発行時に正規形へ限っている
    /// （[`parse_ingest_scope`]）ので、文字列の比較で足りる。0 以下の id は
    /// どのスコープにも当たらない。
    ///
    /// read / write / admin とは**直交**: `read`・`write:…`・`admin` だけの
    /// キーはどの id でも false（`admin` も送れない）。逆に `ingest:` だけの
    /// キーは [`has_any_read`](Self::has_any_read)・
    /// [`has_write_scope`](Self::has_write_scope)・
    /// [`has_admin_scope`](Self::has_admin_scope) がいずれも false のまま
    /// （このモジュールの doc comment「取り込み」参照）。接続がいまも `push`
    /// で有効かどうかは、ここではなく送信口（R2a-2c / 2d）が確かめる。
    fn can_ingest(&self, connection_id: i64) -> bool;
}

impl HubScopeChecks for ApiKeyContext {
    fn has_any_read(&self) -> bool {
        self.scopes
            .iter()
            .any(|scope| scope == "read" || scope.starts_with("read:"))
    }

    fn can_read_value(&self, external_name: &str) -> bool {
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

    fn has_write_scope(&self, external_name: &str) -> bool {
        let needle = format!("write:{external_name}");
        self.scopes.iter().any(|scope| scope == &needle)
    }

    fn has_admin_scope(&self) -> bool {
        self.scopes.iter().any(|s| s == "admin")
    }

    fn can_ingest(&self, connection_id: i64) -> bool {
        if connection_id <= 0 {
            return false;
        }
        let needle = ingest_scope(connection_id);
        self.scopes.iter().any(|scope| scope == &needle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrate_memory;
    use banto_core::BantoError;

    // アプリに依らない部分（キーの形式・照合の順・失効・トリップ・期限・
    // 最終使用の間引き・応答の分け方）のテストは共有 crate
    // `banto-api-keys` の `tests.rs` へ移した。ここには Hub のスコープの書式と
    // 判定、それに Hub の接頭辞とスコープが共有のサービスに渡っていることの
    // 確かめを残す。

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

    // --- R2a-2b: ingest:#{id} の文法 -----------------------------------------

    /// docs/hub/banto-hub-ingest-design.md §11 の R2a-2b の文法の表。
    #[test]
    fn ingest_scope_grammar_table() {
        let accepted: &[(&str, i64)] = &[
            ("ingest:#12", 12),
            ("ingest:#1", 1),
            ("ingest:#9223372036854775807", i64::MAX),
        ];
        for (scope, id) in accepted {
            assert_eq!(parse_ingest_scope(scope), Some(*id), "{scope}");
            assert!(validate_scope(scope).is_ok(), "{scope}");
        }
        let rejected = [
            "ingest:",
            "ingest:#",
            "ingest:#0",
            "ingest:#012",
            "ingest:#00",
            "ingest:#-1",
            "ingest:#+1",
            "ingest:# 12",
            "ingest:#12 ",
            "ingest:#12x",
            "ingest:#1.5",
            "ingest:#*",
            "ingest:*",
            "ingest:Bridge1",
            "ingest:#Bridge1",
            "ingest:##12",
            "ingest:#１２",
            "ingest:#9223372036854775808",
            "ingest:#99999999999999999999",
            "ingest",
            "Ingest:#12",
            " ingest:#12",
        ];
        for scope in rejected {
            assert_eq!(parse_ingest_scope(scope), None, "{scope}");
            assert!(validate_scope(scope).is_err(), "{scope}");
        }
    }

    /// 名前で指したものは「id で指定する」文言、`#` の後ろが崩れたものは
    /// id の形の文言で断る（発行の画面・MCP の利用者が直し方を読めるように）。
    #[test]
    fn ingest_scope_rejection_messages_say_how_to_fix_it() {
        let by_name = validate_scope("ingest:Bridge1").unwrap_err();
        assert!(by_name.contains("接続名では指定できません"), "{by_name}");
        let empty = validate_scope("ingest:").unwrap_err();
        assert!(empty.contains("ingest:#{id}"), "{empty}");
        let bad_id = validate_scope("ingest:#012").unwrap_err();
        assert!(bad_id.contains("1 以上の 10 進の整数"), "{bad_id}");
    }

    #[test]
    fn ingest_scope_round_trips_through_parse() {
        for id in [1, 12, 4096, i64::MAX] {
            assert_eq!(parse_ingest_scope(&ingest_scope(id)), Some(id));
        }
    }

    #[tokio::test]
    async fn issue_accepts_an_ingest_scope_and_rejects_a_name() {
        let service = service().await;
        let issued = service
            .issue("bridge", vec!["ingest:#12".to_string()], None)
            .await
            .expect("ingest:#12 must be issuable");
        assert_eq!(issued.scopes, vec!["ingest:#12".to_string()]);
        let err = service
            .issue("by-name", vec!["ingest:Bridge1".to_string()], None)
            .await
            .unwrap_err();
        assert!(matches!(err, BantoError::Validation { .. }), "{err:?}");
    }

    // --- Hub のスコープと接頭辞が共有のサービスに渡っている -----------------

    /// Hub のサービスが発行するキーは `bh_` で始まり、照合した文脈で Hub の
    /// 判定（[`HubScopeChecks`]）が使える。
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
        // 接頭辞の違うキー（ChronoGazer の `cg_` など）は Hub では読めない。
        let foreign = format!("cg{}", &issued.key[2..]);
        assert_eq!(
            svc.lookup(&foreign, 1_000_000).await.unwrap(),
            ApiKeyLookup::NotFound
        );
    }

    /// 発行の入口で Hub のスコープの書式が効く（write のワイルドカードは断る）。
    #[tokio::test]
    async fn issue_rejects_invalid_scope_syntax() {
        let svc = service().await;
        let err = svc
            .issue("bad-scope", vec!["write:line1.fast.*".to_string()], None)
            .await
            .unwrap_err();
        assert!(matches!(err, BantoError::Validation { .. }));
    }

    // --- T2-4: has_write_scope --------------------------------------------

    #[test]
    fn has_write_scope_requires_an_exact_match() {
        let ctx = ApiKeyContext {
            id: 1,
            name: "k".to_string(),
            prefix: "kkkkkkkk".to_string(),
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
            prefix: "kkkkkkkk".to_string(),
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
            prefix: "kkkkkkkk".to_string(),
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
    /// の回帰防止（[`HubScopeChecks::can_read_value`] の doc comment参照）。
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

    // --- R2a-2b: can_ingest と直交性 ------------------------------------------

    #[test]
    fn can_ingest_requires_an_exact_id_match() {
        let ctx = ctx_with(&["ingest:#12"]);
        assert!(ctx.can_ingest(12));
        assert!(!ctx.can_ingest(1));
        assert!(!ctx.can_ingest(120));
        assert!(!ctx.can_ingest(0));
        assert!(!ctx.can_ingest(-12));
    }

    #[test]
    fn can_ingest_unions_several_ingest_scopes() {
        let ctx = ctx_with(&["ingest:#3", "ingest:#12"]);
        assert!(ctx.can_ingest(3));
        assert!(ctx.can_ingest(12));
        assert!(!ctx.can_ingest(4));
    }

    /// 設計 §5「判定」: `ingest:` だけのキーはタグの値を読めず、PLC へ書けず、
    /// 構成も触れない（R2a-2b の監査: 直交性）。
    #[test]
    fn ingest_only_key_has_no_read_write_or_admin_access() {
        let ctx = ctx_with(&["ingest:#12"]);
        assert!(!ctx.has_any_read());
        assert!(!ctx.can_read_value("line1.fast.temp01"));
        assert!(!ctx.has_write_scope("line1.fast.temp01"));
        assert!(!ctx.has_admin_scope());
    }

    /// 設計 §5「判定」: `read` / `write:` / `admin` のキーは送れない。
    /// `admin` も `ingest:` を含意しない。
    #[test]
    fn read_write_and_admin_keys_cannot_ingest() {
        for scopes in [
            &["read"][..],
            &["read:line1.fast.*"][..],
            &["read:line1.fast.temp01"][..],
            &["write:line1.fast.temp01"][..],
            &["admin"][..],
            &["read", "admin", "write:line1.fast.temp01"][..],
            &[][..],
        ] {
            let ctx = ctx_with(scopes);
            for id in [1, 12, i64::MAX] {
                assert!(!ctx.can_ingest(id), "{scopes:?} must not ingest into #{id}");
            }
        }
    }

    /// 他のスコープと併せ持つキーでも、`ingest:` は他の判定を広げない
    /// （`read:` の値の絞り込みは `read:` のスコープだけで決まる）。
    #[test]
    fn ingest_scope_does_not_widen_other_scopes() {
        let ctx = ctx_with(&["read:line1.fast.temp01", "ingest:#12"]);
        assert!(ctx.has_any_read());
        assert!(ctx.can_read_value("line1.fast.temp01"));
        assert!(!ctx.can_read_value("line1.fast.temp02"));
        assert!(!ctx.has_write_scope("line1.fast.temp01"));
        assert!(!ctx.has_admin_scope());
        assert!(ctx.can_ingest(12));
    }
}
