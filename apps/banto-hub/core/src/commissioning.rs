//! 試運転モードとロックダウン (docs/tag-server-design.md §5.6「試運転モードと
//! ロックダウン」・2026-08-30 オーナー決定)。
//!
//! ## 状態は2つだけ
//!
//! - **試運転モード（初期状態）**: 管理 UI / 管理 REST は、**資格情報なしで発行
//!   される合成 admin の grant セッション**（banto v3.0.0 ADR-0017、kind
//!   [`COMMISSIONING_GRANT_KIND`] = `commissioning`、`POST
//!   /api/auth/grant/commissioning`）で操作できる。発行できるのは loopback の
//!   peer だけ（[`commissioning_grant_spec`]）。発行後は通常の bearer セッション
//!   として `crate::rest::require_session`・`RoleGuard`・監査・ストリームの
//!   再検証にそのまま乗る - **認証を迂回する分岐は無い**（2026-10-04 までは
//!   `require_auth_or_commissioning` 等 34 か所が未ロックダウン中の要求を
//!   無認証で通していた。banto-industrial の v3.0.0 追従で廃止、オーナー決定
//!   2026-10-02〜04）。
//! - **ロックダウン済み**: 従来どおり bearer セッションのログインが必要。
//!   grant の条件（未ロックダウン）が閉じるので、以後 `commissioning` の
//!   発行は 403 になり、発行済みのトークンは[`CommissioningService::lock_down`]
//!   が**フラグの保存の直後に同じ関数内で**失効させる（ADR-0017 §2・§5。
//!   「保存 → `revoke_grant_tokens`」の順。並行する発行との競合は `AuthState`
//!   の世代が閉じる）。
//!
//! 永続先は既存の `settings` テーブル（`crate::settings::SettingsService`と
//! 同じ key/value ストア、`migrations-sqlite/0002_settings.sql`で作成済み）に相乗りする。
//! このためだけの新規テーブルは作らない（実装指示: 「既存の設定テーブルが
//! あればそこへ」）。値は `"true"`/`"false"` の文字列。
//!
//! ## ロックダウン状態は保存されたフラグのみで決まる（2026-08-30 オーナー決定）
//!
//! [`resolve_locked_down`]はフラグが未設定なら（新規 DB・既存 DB を
//! 問わず）常に**試運転モード**を返す - **ユーザーアカウントの有無は判定に
//! 一切関与しない**。これは意図した決定であり、「保護が抜けている」わけ
//! ではない（旧案では「状態未設定 + 既存ユーザーあり → ロックダウン済み」
//! という既存環境保護を必須としていたが、オーナー判断でこの判定ロジック
//! そのものを撤回した - 単純に「フラグが立っていなければ試運転モード」で
//! よい、という単純化）。
//!
//! 単純化後も安全性が保たれる理由: 代わりに[`enforce_loopback_when_commissioning`]
//! （制約1「非 loopback バインドでは未ロックダウンの起動を拒否」）が
//! 効く。既存の LAN バインド環境がこの機能を含むバージョンへアップデート
//! して再起動した場合、ユーザーの有無によらずフラグは未設定＝試運転
//! モードだが、非 loopback バインドのままなら**起動そのものが明確な
//! エラーで拒否される** - 「黙って無防備になる」のではなく「起動時に
//! 気づく」形の安全網になる。loopback バインド（開発機・現場での試運転）
//! のみ、フラグ未設定＝試運転モードのまま起動できる。
//!
//! ## 遷移
//!
//! 「ロックダウン」操作（[`CommissioningService::lock_down`]、
//! `POST /api/commissioning/lock-down`・要 admin）でのみ試運転モード →
//! ロックダウン済みへ移る。逆方向（[`revert_to_commissioning`]）は
//! `banto-hub-elev.exe` 経由のみで、REST には一切公開しない - 2026-08-31
//! に`crate::service_elevated`の`revert-to-commissioning`アクション
//! （`revert_to_commissioning_with_audit`）から配線済み。
//!
//! ## loopback 制約
//!
//! [`is_loopback_bind`]/[`enforce_loopback_when_commissioning`]参照
//! （`crate::runtime::HubRuntime::start`が起動時に一度だけ呼ぶ）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use banto_core::{BantoError, FieldError};
use banto_server::{AuthState, GrantCondition, GrantKind, GrantSpec, Identity};

use crate::settings::SettingsService;
use crate::users::{Role, UsersService};

/// `settings` テーブルのキー。値は `"true"`/`"false"`、未設定は常に
/// 試運転モード扱い（[`resolve_locked_down`]参照 - 2026-08-30 オーナー決定
/// でユーザー数によるフォールバック判定は撤回した）。
const KEY_LOCKED_DOWN: &str = "commissioning.locked_down";

/// 試運転モードの grant セッション（[`commissioning_grant_spec`]）の固定
/// identity の `id`。実在ユーザーの `username`（`users.username`は
/// 空文字を許さない・`MIN_USERNAME_LEN = 1`）とは絶対に衝突しない固定値と
/// して、実運用のユーザー名と紛れないよう `_` を含む形にしてある。監査ログ
/// (`audit_log.actor_username`)にこの値がそのまま残ることで、後から
/// 「試運転モード中に行われた操作」だと判別できる（設計 §5.6「監査ログには
/// 合成 identity がそのまま記録される」・意図した挙動）。
pub const SYNTHETIC_ACTOR_ID: &str = "commissioning";

/// 試運転モードの grant セッションが持つ固定の identity。role は常に
/// `Role::Admin` 相当（設計 §5.6）。banto v3.0.0 の grant は発行口で
/// identity/role を一切受け取らず（ADR-0017「帰結」）、この値が
/// `GrantSpec.identity` としてトークンに結び付く。監査の `actor_username` は
/// 従来（要求ごとの合成 identity）と同じ `commissioning` になる。
pub fn synthetic_identity() -> Identity {
    Identity {
        id: SYNTHETIC_ACTOR_ID.to_string(),
        name: "試運転モード".to_string(),
        role: Role::Admin.as_str().to_string(),
    }
}

/// 試運転モードの grant の kind（ADR-0017 §2「kind の識別子」: URL
/// `POST /api/auth/grant/commissioning`・`GET /api/auth/status` の
/// `grants.commissioning`・`GET /api/auth/identity` の `kind`・画面の
/// `SessionKind` で**同じ文字列**を使う）。
pub const COMMISSIONING_GRANT_KIND: &str = "commissioning";

/// 試運転 grant の種別ごとの FIFO 上限（ADR-0017 §5「`max_sessions` は閲覧公開の
/// 256 より小さくしてよい。admin 相当のトークンの数は少ないほどよい」）。管理 UI
/// のタブと bootstrap の自己発行（`crates/banto-hub-bootstrap`、発行後すぐ
/// logout する）が同時に使う数として十分に大きく、上限を超えた最古は失効する。
pub const COMMISSIONING_GRANT_MAX_SESSIONS: usize = 16;

/// [`COMMISSIONING_GRANT_KIND`] を `GrantKind` として返す。
pub fn commissioning_grant_kind() -> GrantKind {
    GrantKind::new(COMMISSIONING_GRANT_KIND).expect("`commissioning` は grant kind の文法に合う")
}

/// 試運転モードの grant（ADR-0017 の 2 種類目）の仕様。レビューはまずここを見る:
///
/// - identity: [`synthetic_identity`]（固定・admin）。
/// - 条件: **未ロックダウン**（[`CommissioningState::is_locked_down`] の否定。
///   要求ごとにプロセス内のフラグを読むだけで DB は読まない。失敗しない）。
/// - `require_loopback_peer: true`: 発行は loopback の peer だけ。peer 不明
///   （`ConnectInfo` の無い起動経路・`tower::oneshot` のテスト）も 403
///   （fail closed）。試運転モードは loopback バインドでしか起動できない
///   （[`enforce_loopback_when_commissioning`]）が、ロックダウン後に LAN へ
///   出してから elev で試運転へ戻した構成でも、LAN の第三者が admin 相当の
///   トークンを得られないようにする（ADR-0017「注意点 2」）。同一ホストの
///   リバースプロキシ配下では peer が常にプロキシなので守れない - 運用条件
///   （ADR-0017 §6、運用ガイド §6・§19）。
/// - `max_sessions`: [`COMMISSIONING_GRANT_MAX_SESSIONS`]。
/// - 寿命: 既定（8h / idle 1h、remembered にしない）。
pub fn commissioning_grant_spec(state: CommissioningState) -> GrantSpec {
    let enabled: GrantCondition = Arc::new(move || {
        let state = state.clone();
        Box::pin(async move { Ok(!state.is_locked_down()) })
    });
    let mut spec = GrantSpec::new(commissioning_grant_kind(), synthetic_identity(), enabled);
    spec.max_sessions = COMMISSIONING_GRANT_MAX_SESSIONS;
    spec.require_loopback_peer = true;
    spec
}

/// ロックダウン済みかどうかの軽量な共有ハンドル（`Arc<AtomicBool>`・
/// `Clone`は安価）。試運転 grant の条件（[`commissioning_grant_spec`]）と、
/// `crate::rest` の「試運転中は構成変更を即時反映する」判断
/// （`registry_change_should_queue`、#341）・`crate::mcp` の write 系ツールの
/// アドバイザリ化が、DB を叩かずに毎回チェックできるようにする -
/// 状態が変わるのは [`CommissioningService::lock_down`]（プロセス内、
/// 明示的なロックダウン操作）のときだけなので、`Arc<AtomicBool>`で
/// 十分（`revert_to_commissioning`は elev 用で別プロセス実行前提 - 実行中の
/// Hub プロセスがそれを見て自分の状態を書き換える経路は無く、次回起動時に
/// `resolve_locked_down`が DB から読み直す想定）。
#[derive(Clone)]
pub struct CommissioningState {
    locked_down: Arc<AtomicBool>,
}

impl CommissioningState {
    fn new(locked_down: bool) -> Self {
        Self {
            locked_down: Arc::new(AtomicBool::new(locked_down)),
        }
    }

    /// 現在ロックダウン済みか。`Ordering::SeqCst`は頻度（HTTP リクエスト
    /// ごと）に対してオーバーヘッドが無視できる一方、素直に「最新の値が
    /// 見える」ことだけを保証したいので緩い順序を選ぶ理由がない。
    pub fn is_locked_down(&self) -> bool {
        self.locked_down.load(Ordering::SeqCst)
    }

    fn set_locked_down(&self, value: bool) {
        self.locked_down.store(value, Ordering::SeqCst);
    }
}

/// 起動時（`crate::runtime::HubRuntime::start`）・テストで、DB の永続状態
/// から「ロックダウン済みか」を判定する。
///
/// 2026-08-30 オーナー決定: **保存されたフラグのみで決まる** -
/// `commissioning.locked_down` が未設定なら、新規 DB・既存 DB を問わず
/// 常に試運転モード（`false`）を返す。**ユーザーアカウントの有無は判定に
/// 一切関与しない** - 当初案にあった「状態未設定 + 既存ユーザーあり →
/// ロックダウン済み」という既存環境保護ロジックは撤回済み（意図的な単純化
/// であり、実装漏れではない - このモジュールの doc comment 参照）。
pub async fn resolve_locked_down(settings: &SettingsService) -> Result<bool, BantoError> {
    Ok(settings
        .get(KEY_LOCKED_DOWN)
        .await?
        .map(|value| value == "true")
        .unwrap_or(false))
}

/// `bind`文字列（`crate::settings::ServerSettings::bind`/`BANTO_BIND`）が
/// loopback（同一ホストからしか到達できないアドレス）かどうか。
///
/// 判定は保守的（安全側）: `IpAddr`としてパースできて`is_loopback()`が
/// `true`のとき、または大文字小文字を無視して文字列が`"localhost"`と
/// 一致するときだけ loopback とみなす。それ以外（`"0.0.0.0"`・`"::"`・
/// 任意の LAN IP・パース不能な文字列）は非 loopback 扱いにする - 実装指示
/// 「迷ったら安全側（認証を要求する側）に倒すこと」のとおり、未知の
/// 入力を loopback だと誤認して試運転モードの起動を許してしまう方向の
/// 間違いは絶対に避ける。
pub fn is_loopback_bind(bind: &str) -> bool {
    if bind.eq_ignore_ascii_case("localhost") {
        return true;
    }
    bind.parse::<std::net::IpAddr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(false)
}

/// 設計 §5.6 制約1「試運転モードは loopback バインド時のみ許可する」の
/// 起動時ガード。`crate::runtime::HubRuntime::start`が bind アドレスと
/// ロックダウン状態の両方を解決した直後に一度だけ呼ぶ - コンソール
/// （`bin/banto-hub.rs`）・Windows サービス（`bin/banto_hub/win_service.rs`）
/// ・デスクトップシェル（`apps/banto-hub/src-tauri`）は全て
/// `HubRuntime::start`のこの1箇所を経由するので（`runtime.rs`のモジュール
/// doc「起動シーケンス」参照）、ここに置くだけで3ホストすべて、特に
/// 実装指示が名指しした`bin/banto-hub.rs`の起動経路にも確実に効く -
/// ホストごとの env 読み取り（`build_hub_config_from_env`等）側にこの
/// チェックを置くと、ホストを追加/変更するたびに個別にガードし直す必要が
/// 生まれ、抜け道になり得る。
///
/// 認証なしの状態（試運転モード）がネットワークへ露出する経路を原理的に
/// 塞ぐのが目的 - 試運転はハブが動いている機械の前で行う前提（設計
/// §5.6）なので、LAN/外部に公開した状態で試運転モードのまま起動する
/// ことを起動時エラーで拒否する。2026-08-30 の単純化（ロックダウン状態が
/// フラグのみで決まるようになった変更）により、この制約の重要度は
/// むしろ上がっている - 既存の LAN バインド環境がアップデートしただけで
/// 黙って無防備になるのではなく、ここで確実に起動を止める安全網になる。
pub fn enforce_loopback_when_commissioning(bind: &str, locked_down: bool) -> Result<(), String> {
    if locked_down || is_loopback_bind(bind) {
        return Ok(());
    }
    Err(format!(
        "banto-hub: 試運転モード（未ロックダウン）のまま非 loopback バインド（{bind}）で\
         起動することはできません。対処: (a) 管理 UI からロックダウンを実行してから\
         起動し直すか、(b) server.bind / BANTO_BIND を 127.0.0.1 等の loopback アドレスに\
         変更してください。（docs/tag-server-design.md §5.6「試運転モードとロックダウン」\
         制約1: 認証なしの状態をネットワークへ露出させないための安全装置です）"
    ))
}

/// ロックダウン実行時に「管理者アカウントが1件も無い」場合のエラー。
fn no_admin_account_error() -> BantoError {
    BantoError::Validation {
        field_errors: vec![FieldError {
            field: "lockDown".to_string(),
            message: "管理者（admin ロール）アカウントが1件も存在しないため、ロックダウン\
                      できません。ロックダウンするとログインが必須になるため、先に管理者\
                      アカウントを作成してください。"
                .to_string(),
        }],
    }
}

/// `GET /api/commissioning/status`（未認証で取得可）が返す形。
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CommissioningStatus {
    pub locked_down: bool,
}

/// 試運転モード/ロックダウンの読み書きを一箇所にまとめたサービス層
/// （`crate::settings::SettingsService`・`crate::users::UsersService`と
/// 同じ「service 層は axum を知らない」規約）。`crate::rest`の
/// `commissioning_router`から使う。
#[derive(Clone)]
pub struct CommissioningService {
    settings: SettingsService,
    users: UsersService,
    state: CommissioningState,
    /// 試運転 grant のトークンを持つ `AuthState`（REST の本番の `AuthState` と
    /// **同じもの**）。[`Self::lock_down`] がフラグの保存の直後に
    /// `revoke_grant_tokens` を呼ぶために持つ（ADR-0017 §5「同じ関数内で」）。
    auth: AuthState,
}

impl CommissioningService {
    /// 起動時に一度だけ呼ぶ: DB から現在の状態を解決し（[`resolve_locked_down`]）、
    /// 以後リクエストごとに参照する[`CommissioningState`]を構築する。`auth` は
    /// REST に渡すものと同じ `AuthState`（試運転 grant のトークンの置き場。別の
    /// `AuthState` を渡すとロックダウンが本物のトークンを失効できない）。
    pub async fn load(
        settings: SettingsService,
        users: UsersService,
        auth: AuthState,
    ) -> Result<Self, BantoError> {
        let locked_down = resolve_locked_down(&settings).await?;
        Ok(Self {
            settings,
            users,
            state: CommissioningState::new(locked_down),
            auth,
        })
    }

    /// リクエスト処理側（ミドルウェア・ハンドラ）に配るための軽量ハンドル。
    pub fn state(&self) -> CommissioningState {
        self.state.clone()
    }

    /// `GrantRegistry` に登録する試運転 grant の仕様（[`commissioning_grant_spec`]）。
    pub fn grant_spec(&self) -> GrantSpec {
        commissioning_grant_spec(self.state.clone())
    }

    pub fn is_locked_down(&self) -> bool {
        self.state.is_locked_down()
    }

    /// 試運転モード → ロックダウン済みへ遷移する（設計 §5.6「遷移」・唯一の
    /// 正方向の経路）。既にロックダウン済みなら何もせず成功を返す
    /// （冪等 - 二重にロックダウンを叩いても壊れない）。
    ///
    /// 管理者（admin ロール）アカウントが1件も無ければ失敗する - 誰も
    /// ログインできない状態のまま施錠して詰むことを防ぐガード
    /// （実装指示「実行時に管理者アカウントが1件以上存在することを必須と
    /// する」）。
    ///
    /// 順序は **「フラグの保存（DB → プロセス内）→ 直後に同じ関数内で
    /// `revoke_grant_tokens(commissioning)`」**（ADR-0017 §2「発行と失効の
    /// 直列化」・§5。2026-10-04 オーナー決定）。逆にしない: 先に失効すると、
    /// 保存までの間に `enabled()` がまだ true を返し、その発行は失効の後の世代で
    /// 挿入されて残る。保存が先なら、保存後の判定は false、保存前に true を取った
    /// 発行は `AuthState` の世代の前進で弾かれ、どちらも残らない。戻り値は失効
    /// させた試運転 grant のトークン数（呼び出し側が監査の detail に
    /// `revokedGrants: n` として残す）。既にロックダウン済みなら保存はせず
    /// 失効だけ行う（通常 0。冪等）。
    pub async fn lock_down(&self) -> Result<usize, BantoError> {
        if !self.is_locked_down() {
            if !self.users.has_admin().await? {
                return Err(no_admin_account_error());
            }
            self.settings.set(KEY_LOCKED_DOWN, "true").await?;
            self.state.set_locked_down(true);
        }
        Ok(self.auth.revoke_grant_tokens(&commissioning_grant_kind()))
    }

    /// ロックダウン済み → 試運転モードへ戻す**内部関数**。
    ///
    /// **REST では絶対に公開しないこと**（設計 §5.6「逆方向は
    /// `banto-hub-elev.exe` 経由でのみ可能とし、UI・REST からは解除
    /// できない」）- `crate::rest`にこれを呼ぶハンドラ/ルートを追加しては
    /// いけない。`banto-hub-elev.exe`は稼働中の Hub プロセスとは別プロセス
    /// なので、この「実行中の`CommissioningService`を持つ」メソッド版を
    /// 直接呼ぶことはできない - elev 側は代わりに自由関数版
    /// [`revert_to_commissioning`]（DB プールだけを渡せる）を使う
    /// （2026-08-31、`crate::service_elevated`から配線済み）。このメソッド版
    /// はテストからのみ呼ばれる（本体は同じ処理を`CommissioningState`経由で
    /// 行うだけなので、テストで実行中プロセスの状態遷移として検証する用途で
    /// 残してある）。
    ///
    /// 呼び出し元は、戻した直後に「非 loopback バインドのままだと次回
    /// 起動が拒否される」（[`enforce_loopback_when_commissioning`]、設計
    /// §5.6「復帰させた場合は制約1が再び効く」・意図した副作用）ことを
    /// 利用者に案内する責務を持つ。既存のアカウントのトークンは失効させない
    /// （2026-10-04 オーナー決定。従来どおり）。
    #[allow(dead_code)]
    pub async fn revert_to_commissioning(&self) -> Result<(), BantoError> {
        self.settings.set(KEY_LOCKED_DOWN, "false").await?;
        self.state.set_locked_down(false);
        Ok(())
    }
}

/// [`CommissioningService::revert_to_commissioning`]と同じ処理を、実行中の
/// `CommissioningService`（＝実行中の Hub プロセス）を持たない呼び出し元
/// （`banto-hub-elev.exe`は Hub プロセスとは別プロセスで、UAC 昇格のために
/// 起動されるヘルパーであり、稼働中の Hub の内部状態を直接触れない -
/// `service_elevated.rs`のモジュール doc 参照）から、DB プールだけを渡して
/// 直接呼べるようにした自由関数版。
///
/// **REST では絶対に公開しないこと**（[`CommissioningService::revert_to_commissioning`]
/// のdoc comment参照）。2026-08-31 に
/// `crate::service_elevated::revert_to_commissioning_with_audit`
/// （`ElevatedAction::RevertToCommissioning`＝`revert-to-commissioning`
/// アクションの中核処理）から配線された - 対象 profile の DB を直接開いて
/// この関数を呼び、直後に監査ログへ1行記録する（§5.6 制約4）。
pub async fn revert_to_commissioning(pool: &sqlx::SqlitePool) -> Result<(), BantoError> {
    let settings = SettingsService::new(pool.clone());
    settings.set(KEY_LOCKED_DOWN, "false").await
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn services() -> (SettingsService, UsersService) {
        let (settings, users, _pool) = services_with_pool().await;
        (settings, users)
    }

    /// テスト用の `AuthState`（ログインは常に失敗、照合なし）。試運転 grant の
    /// トークンの置き場としてだけ使う。
    fn auth() -> AuthState {
        AuthState::new(
            |_u: String, _p: String| Box::pin(async { None }),
            banto_server::SessionValidation::DisabledNoRevocation,
        )
    }

    /// [`services`]と同じだが、生の`SqlitePool`も返す - `lock_down_fails_
    /// without_any_admin_account`が`UsersService::update_user`（最後の
    /// admin の降格を拒否するガード付き）を迂回して直接 SQL でロールを
    /// 書き換えるために必要。
    async fn services_with_pool() -> (SettingsService, UsersService, sqlx::SqlitePool) {
        let pool = crate::db::migrate_memory().await.expect("migrate_memory");
        (
            SettingsService::new(pool.clone()),
            UsersService::new(pool.clone()),
            pool,
        )
    }

    /// 2026-08-30 オーナー決定: フラグ未設定なら、既存ユーザーの有無に
    /// かかわらず常に試運転モード。ここで「ユーザーがいても試運転モード
    /// になる」ことを明示的に固定する - 当初案の既存環境保護
    /// （ユーザーがいればロックダウン済み扱い）は撤回済みで、この挙動は
    /// 意図した仕様であることの回帰テスト。
    #[tokio::test]
    async fn unset_state_resolves_to_commissioning_mode_regardless_of_users() {
        let (settings, users) = services().await;
        users
            .setup_first_user("owner", "password123", "オーナー")
            .await
            .expect("setup_first_user");

        let locked_down = resolve_locked_down(&settings)
            .await
            .expect("resolve_locked_down");
        assert!(
            !locked_down,
            "フラグ未設定は既存ユーザーの有無によらず試運転モードであるべき"
        );
    }

    /// フラグ未設定 + ユーザー0件でも同じく試運転モード（新規環境の既定
    /// 挙動）。
    #[tokio::test]
    async fn unset_state_with_no_users_is_also_commissioning_mode() {
        let (settings, _users) = services().await;

        let locked_down = resolve_locked_down(&settings)
            .await
            .expect("resolve_locked_down");
        assert!(!locked_down);
    }

    /// 明示的に設定された値はそのまま使われる - ロックダウン後に全ユーザーを
    /// 削除しても試運転モードへは戻らない。
    #[tokio::test]
    async fn explicit_true_is_respected() {
        let (settings, _users) = services().await;
        settings
            .set(KEY_LOCKED_DOWN, "true")
            .await
            .expect("set locked_down=true");

        let locked_down = resolve_locked_down(&settings)
            .await
            .expect("resolve_locked_down");
        assert!(locked_down);
    }

    #[tokio::test]
    async fn commissioning_service_load_matches_resolve_locked_down() {
        let (settings, users) = services().await;
        settings
            .set(KEY_LOCKED_DOWN, "true")
            .await
            .expect("set locked_down=true");

        let service = CommissioningService::load(settings, users, auth())
            .await
            .expect("load");
        assert!(service.is_locked_down());
    }

    #[tokio::test]
    async fn lock_down_fails_without_any_admin_account() {
        let (settings, users, pool) = services_with_pool().await;
        // admin 以外のロールしか無い状態を作る: 最初のユーザーは常に admin
        // で作成される（`UsersService::setup_first_user`）ため、
        // `UsersService::update_user`（最後の admin の降格を拒否するガード
        // 付き）を迂回して直接 SQL でロールを書き換え、「admin が1人も
        // いない」状況を再現する。
        users
            .setup_first_user("owner", "password123", "オーナー")
            .await
            .expect("setup_first_user");
        sqlx::query("UPDATE users SET role = 'editor' WHERE username = 'owner'")
            .execute(&pool)
            .await
            .expect("downgrade to editor via raw SQL");

        let service = CommissioningService::load(settings, users, auth())
            .await
            .expect("load");
        assert!(!service.is_locked_down());

        let err = service
            .lock_down()
            .await
            .expect_err("lock_down should fail without any admin account");
        assert!(matches!(err, BantoError::Validation { .. }));
        assert!(
            !service.is_locked_down(),
            "failed lock_down must not flip the in-memory state"
        );
    }

    #[tokio::test]
    async fn lock_down_succeeds_with_an_admin_account_and_persists() {
        let (settings, users) = services().await;
        users
            .setup_first_user("owner", "password123", "オーナー")
            .await
            .expect("setup_first_user");

        let service = CommissioningService::load(settings.clone(), users, auth())
            .await
            .expect("load");
        assert!(!service.is_locked_down());

        service.lock_down().await.expect("lock_down should succeed");
        assert!(service.is_locked_down());

        let persisted = settings
            .get(KEY_LOCKED_DOWN)
            .await
            .expect("get")
            .expect("value should be persisted");
        assert_eq!(persisted, "true");
    }

    #[tokio::test]
    async fn lock_down_is_idempotent_when_already_locked_down() {
        let (settings, users) = services().await;
        users
            .setup_first_user("owner", "password123", "オーナー")
            .await
            .expect("setup_first_user");
        let service = CommissioningService::load(settings, users, auth())
            .await
            .expect("load");
        service.lock_down().await.expect("first lock_down");
        service
            .lock_down()
            .await
            .expect("second lock_down should be a harmless no-op");
        assert!(service.is_locked_down());
    }

    #[tokio::test]
    async fn revert_to_commissioning_flips_state_back() {
        let (settings, users) = services().await;
        users
            .setup_first_user("owner", "password123", "オーナー")
            .await
            .expect("setup_first_user");
        let service = CommissioningService::load(settings, users, auth())
            .await
            .expect("load");
        service.lock_down().await.expect("lock_down");
        assert!(service.is_locked_down());

        service
            .revert_to_commissioning()
            .await
            .expect("revert_to_commissioning");
        assert!(!service.is_locked_down());
    }

    /// ADR-0017 §2・§5: ロックダウンは「保存 → 失効」。発行済みの試運転 grant の
    /// トークンは同じ関数の中で消え、件数が戻り、以後の発行は世代の不一致でも
    /// 条件でも通らない。
    #[tokio::test]
    async fn lock_down_revokes_the_commissioning_grant_tokens_after_persisting_the_flag() {
        let (settings, users) = services().await;
        users
            .setup_first_user("owner", "password123", "オーナー")
            .await
            .expect("setup_first_user");
        let auth = auth();
        let service = CommissioningService::load(settings.clone(), users, auth.clone())
            .await
            .expect("load");
        let spec = service.grant_spec();
        assert!(spec.require_loopback_peer);
        assert_eq!(spec.max_sessions, COMMISSIONING_GRANT_MAX_SESSIONS);
        assert_eq!(spec.identity.id, SYNTHETIC_ACTOR_ID);
        assert_eq!(spec.identity.role, Role::Admin.as_str());
        assert!((spec.enabled)().await.expect("condition never fails"));

        let observed = auth.grant_generation(&spec.kind);
        let token_a = auth
            .issue_grant_token(&spec, observed)
            .expect("issued while commissioning");
        let token_b = auth
            .issue_grant_token(&spec, observed)
            .expect("issued while commissioning");
        assert_eq!(
            auth.identity_for(&token_a).map(|i| i.id),
            Some(SYNTHETIC_ACTOR_ID.to_string())
        );

        // 判定（true）だけ済ませて保留した発行があったとする（世代は保存前のもの）。
        let parked = auth.grant_generation(&spec.kind);

        let revoked = service.lock_down().await.expect("lock_down");
        assert_eq!(revoked, 2);
        assert!(service.is_locked_down());
        assert_eq!(
            settings.get(KEY_LOCKED_DOWN).await.expect("get").as_deref(),
            Some("true")
        );
        assert!(auth.identity_for(&token_a).is_none());
        assert!(auth.identity_for(&token_b).is_none());
        // 保留していた発行を再開しても、世代が進んでいるので挿入されない。
        assert!(auth.issue_grant_token(&spec, parked).is_none());
        // 条件も閉じている。
        assert!(!(spec.enabled)().await.expect("condition never fails"));
        // 2 回目は保存せず失効だけ（0 件）。
        assert_eq!(service.lock_down().await.expect("idempotent"), 0);
    }

    #[test]
    fn is_loopback_bind_accepts_loopback_forms() {
        assert!(is_loopback_bind("127.0.0.1"));
        assert!(is_loopback_bind("::1"));
        assert!(is_loopback_bind("localhost"));
        assert!(is_loopback_bind("LOCALHOST"));
    }

    #[test]
    fn is_loopback_bind_rejects_non_loopback_forms() {
        assert!(!is_loopback_bind("0.0.0.0"));
        assert!(!is_loopback_bind("192.168.1.10"));
        assert!(!is_loopback_bind("::"));
        assert!(!is_loopback_bind(""));
        assert!(!is_loopback_bind("not-an-ip"));
    }

    #[test]
    fn enforce_loopback_when_commissioning_allows_loopback_while_unlocked() {
        assert!(enforce_loopback_when_commissioning("127.0.0.1", false).is_ok());
    }

    #[test]
    fn enforce_loopback_when_commissioning_allows_any_bind_once_locked_down() {
        assert!(enforce_loopback_when_commissioning("0.0.0.0", true).is_ok());
    }

    #[test]
    fn enforce_loopback_when_commissioning_rejects_non_loopback_while_unlocked() {
        let err = enforce_loopback_when_commissioning("0.0.0.0", false)
            .expect_err("non-loopback + unlocked must be rejected");
        assert!(err.contains("loopback"));
    }
}
