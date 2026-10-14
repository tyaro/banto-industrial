//! LAN の公開設定と認証の設定（#392 段階 B の B2-1、2026-10-14。設計は
//! docs/chronogazer-service-design.md §3.2「REST の差分を埋める」・§5 の決定 8・9）。
//!
//! これまで LAN の公開設定（`server_status`/`server_apply`）と認証モード
//! （`auth_config_get/apply`）はデスクトップの Tauri のコマンドにしか無かった。サービス
//! （B3）では画面とエンジンが別のプロセスになるので、REST に足す。**LAN の listener の
//! 入れ替えの本体（順序・巻き戻し・監査）は [`apply_server_settings`] 1 か所**で、
//! デスクトップの `server_apply`（Tauri のコマンドのまま）とこのモジュールの
//! `PUT /api/settings/server` の両方がこれを呼ぶ。
//!
//! # ルート
//!
//! | Method | Path | Body | Response |
//! |--------|------|------|----------|
//! | GET | `/api/settings/server` | - | [`ServerStatusView`]（admin） |
//! | PUT | `/api/settings/server` | `{enabled,bind,port,viewerPublic}` | [`ServerStatusView`]（admin、**この PC からの要求だけ**、`chronogazer-server` だけ） |
//! | GET | `/api/settings/auth` | - | [`AuthConfigView`]（admin） |
//! | PUT | `/api/settings/auth` | `{localGrant}` | [`AuthConfigView`]（admin、**この PC からの要求だけ**） |
//!
//! 監査は Tauri のコマンドと同じ `settings_change`（`resource: settings`）で、
//! `origin: rest`。権限の拒否は `RoleGuard` が `denied` を残す。
//!
//! # LAN の入れ替え（決定 9）
//!
//! - **入れ替えるのは LAN の listener（[`LanControl::slot`]）だけ**。`chronogazer-server` の
//!   常時の listener（`127.0.0.1:<port>`、`0.0.0.0` のときはそれにまとめた 1 本）は起動時に
//!   決まり、その場では入れ替えない（決定 9「loopback の listener は常時」）。保存した設定で
//!   常時の listener の形が変わる（**ポートを変える**、`0.0.0.0` にする・やめる）ときは、
//!   保存だけして `restartRequired: true` を返す（次の起動で反映。新しいポートは保存の前に
//!   一度 bind できるかを確かめる - 塞がったポートを保存して次の起動が止まるのを防ぐ）。
//!   常時の listener の上で来た要求が、自分の通っている listener を止める（graceful な
//!   停止が自分の要求の終わりを待ち合う）ことが無いようにするため。
//! - **`PUT` はこの PC からの要求（peer と Host の両方が loopback）だけ**: LAN の listener
//!   から来た要求でその listener を入れ替えると、同じ待ち合いが起きる（banto の停止は
//!   5 秒で要求を打ち切る）。LAN の公開を LAN の向こうから変えられないのは安全の面でも
//!   よい（公開をやめた瞬間に自分が締め出される）。
//! - **デスクトップ・`banto-serve` の REST では `PUT` を断る**（`403`）。デスクトップの LAN の
//!   listener は画面（Tauri の `server_apply`）が入れ替えるもので、REST から入れ替えると
//!   その listener 自身の上の要求になりうる。`GET` はどの host でも答える。
//!
//! # 認証の設定と、ログイン不要モード（`auth.disabled`）の扱い
//!
//! `PUT /api/settings/auth` で変えられるのは **`localGrant`（設定 `auth.local_grant`）だけ**。
//! ログイン不要モード（`auth.disabled`・`auth.disabled_role`）は `GET` で読めるが、
//! **REST からは変えない**（`disabled`・`disabledRole` を送ると `400`）: このモードは
//! デスクトップの画面（Tauri）の合成のセッションを作り直す仕組み（`auth_config_apply` が
//! `auth_config_lock` の下で画面のセッションを入れ替える）で、REST のセッションには
//! 何の効果も無い。サービスでは代わりに `localGrant` を使う（設計書 §6「サービス host では
//! ログイン不要モード自体を `local_grant` に置き換える」）。自動ログインは OS キーリング
//! （利用者ごと）にパスワードを持つデスクトップだけの機能なので REST に出さない。
//!
//! `localGrant` を変えたら、保存の後に発行済みの `localDesktop` のトークンを失効させる
//! （ADR-0017 の順序）。新しい値で発行するのは次の起動から（[`crate::local_grant`] の
//! 「ロールの変更は再起動で反映」）。

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{FromRequest, Request, State};
use axum::http::StatusCode;
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use banto_core::{BantoError, ErrorBody};
use banto_server::routes::{actor_identity, require_role_at_least, RoleGuard};
use banto_server::{
    bind as bind_listener, lan_urls_for_bind, request_is_loopback_local, require_auth, ApiError,
    AuthState, GrantKind, Identity, RunningServer, ServerConfig,
};
use qrcode::render::svg;
use qrcode::QrCode;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex as AsyncMutex;

use crate::audit::{AuditEntry, AuditLogService};
use crate::engine::ServerSlot;
use crate::local_grant::{
    local_desktop_grant_kind, read_setting, run_detached, write_setting, LocalGrantSetting,
    LocalGrantStart,
};
use crate::server_host::listen_plan_from_settings;
use crate::settings::{AuthSettings, ServerSettings, SettingsService};
use crate::users::{Role, UsersService};

// --- 状態の表示 -------------------------------------------------------------------

/// LAN のアクセス URL 1 つと、その QR コード（インラインの SVG、spec §11.4）。
#[derive(Debug, Clone, Serialize)]
pub struct QrSvgEntry {
    pub url: String,
    pub svg: String,
}

/// `GET`/`PUT /api/settings/server` とデスクトップの `server_status`/`server_apply` の
/// 応答。`src/lib/banto/serverAdmin.ts::ServerStatus` とフィールドを揃える
/// （`restartRequired` は B2-1 で足した）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerStatusView {
    pub enabled: bool,
    /// LAN から届く listener がいま開いているか。
    pub running: bool,
    pub bind: String,
    pub port: u16,
    /// 閲覧公開（ADR-0012）。
    pub viewer_public: bool,
    pub urls: Vec<String>,
    pub qr_svgs: Vec<QrSvgEntry>,
    /// 保存した設定が常時の listener の形を変えるので、反映に再起動が要る
    /// （`chronogazer-server` だけ。デスクトップは常に `false`）。
    pub restart_required: bool,
}

/// `data`（LAN のアクセス URL）の QR コードをインラインの SVG にする。失敗しても
/// panic せず空にする（短い `http://host:port` で起きない。画面の表示にだけ使う）。
fn qr_svg_for(data: &str) -> String {
    QrCode::new(data)
        .map(|code| code.render::<svg::Color>().min_dimensions(160, 160).build())
        .unwrap_or_default()
}

/// 状態の表示を組む。`running` は LAN から届く listener の実際のアドレス（あれば
/// URL はそこから。`crate::listening_urls`）。無ければ保存した `bind`/`port` から。
pub fn build_status(
    config: &ServerSettings,
    running: Option<SocketAddr>,
    restart_required: bool,
) -> ServerStatusView {
    let urls = match running {
        Some(addr) => crate::listening_urls(addr),
        None => lan_urls_for_bind(&config.bind, config.port),
    };
    let qr_svgs = urls
        .iter()
        .map(|url| QrSvgEntry {
            url: url.clone(),
            svg: qr_svg_for(url),
        })
        .collect();
    ServerStatusView {
        enabled: config.enabled,
        running: running.is_some(),
        bind: config.bind.clone(),
        port: config.port,
        viewer_public: config.viewer_public,
        urls,
        qr_svgs,
        restart_required,
    }
}

// --- LAN の listener の入れ物 ----------------------------------------------------

/// LAN 用のルーターを組む関数（入れ替えで新しい listener に載せる）。
pub type LanRouterFactory = Arc<dyn Fn() -> Router + Send + Sync>;

/// `chronogazer-server` の常時の listener（起動時に決まり、入れ替えない）。
#[derive(Clone)]
pub struct AlwaysOnListener {
    /// 起動時の計画の形（`127.0.0.1:<port>`、まとめたときは `0.0.0.0:<port>`。
    /// 代わりの `127.0.0.1` に落ちていても計画の形のまま - 保存した設定と比べるため）。
    pub config: ServerConfig,
    pub slot: Arc<ServerSlot>,
}

/// LAN の listener の入れ物と、入れ替えに要るもの。
#[derive(Clone)]
pub struct LanControl {
    /// 入れ替えられる LAN の listener（デスクトップの `AppState::server` と同じもの）。
    pub slot: Arc<ServerSlot>,
    /// 常時の listener（`chronogazer-server` だけ）。
    pub always_on: Option<AlwaysOnListener>,
    /// 新しい LAN の listener に載せるルーター（REST の `PUT` が使う。デスクトップは
    /// Tauri のコマンドが自分で渡す）。
    pub router: Option<LanRouterFactory>,
    /// 最後に一貫して読めた状態（[`LanStatusCache`]）。
    pub published: LanStatusCache,
}

/// 最後に一貫して読めた LAN の状態（`GET /api/settings/server` が適用の途中に返すもの。
/// #600 のレビュー）。適用（[`apply_server_settings`]）が終わるたび、と、slot を待たずに
/// 取れた `GET` のたびに書き換える。エンジンに 1 つ（`EngineServices::lan_status`）。
pub type LanStatusCache = Arc<std::sync::Mutex<Option<ServerStatusView>>>;

fn publish(cache: &LanStatusCache, view: &ServerStatusView) {
    *cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(view.clone());
}

impl LanControl {
    /// エンジンに繋がっていない入れ物（テストの単体のルーター用）。`PUT` は断る。
    pub fn detached() -> Self {
        Self {
            slot: Arc::new(ServerSlot::default()),
            always_on: None,
            router: None,
            published: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    /// REST の `PUT /api/settings/server` で入れ替えられる host か（`chronogazer-server`）。
    pub fn rest_apply_supported(&self) -> bool {
        self.always_on.is_some() && self.router.is_some()
    }

    /// LAN から届く listener のいまのアドレス: LAN の listener、無ければ LAN に出ている
    /// 常時の listener（`0.0.0.0` にまとめたとき）。
    pub async fn running_lan_addr(&self) -> Option<SocketAddr> {
        if let Some(addr) = self.slot.local_addr().await {
            return Some(addr);
        }
        self.always_on_lan_addr().await
    }

    /// LAN に出ている常時の listener（`0.0.0.0` にまとめたとき）のアドレス。
    pub async fn always_on_lan_addr(&self) -> Option<SocketAddr> {
        match &self.always_on {
            Some(always_on) => always_on
                .slot
                .local_addr()
                .await
                .filter(|addr| !addr.ip().is_loopback()),
            None => None,
        }
    }

    /// `config` を反映するのに再起動が要るか（常時の listener の形が変わる）。
    fn restart_required(&self, auth: &AuthSettings, config: &ServerSettings) -> bool {
        self.always_on.as_ref().is_some_and(|always_on| {
            listen_plan_from_settings(auth, config).loopback != always_on.config
        })
    }

    /// `config` のときに LAN の listener として開くもの。デスクトップは保存した
    /// bind/port そのもの、`chronogazer-server` は待ち受けの計画の LAN の分。
    fn desired_lan(&self, auth: &AuthSettings, config: &ServerSettings) -> Option<ServerConfig> {
        if self.always_on.is_some() {
            listen_plan_from_settings(auth, config).lan
        } else if config.enabled {
            Some(ServerConfig {
                bind: config.bind.clone(),
                port: config.port,
            })
        } else {
            None
        }
    }
}

/// 保存済みの設定といまの listener から状態を組む。
pub async fn current_status(
    settings: &SettingsService,
    lan: &LanControl,
) -> Result<Option<ServerStatusView>, BantoError> {
    // 保存値と実行中のアドレスを混ぜずに読む（#600 のレビュー）が、**slot を待たない**
    // （#600 の再レビュー）: 適用（[`apply_server_settings`]）は slot を持ったまま古い
    // LAN の listener を graceful に止めるので、その listener 経由の `GET` が slot を
    // 待つと、停止はその要求の終わりを待ち、banto の 5 秒の打ち切りまで適用が止まる。
    // slot が空いていれば持ったまま読み（この間に来た適用は待つだけで、`GET` は
    // 必ず終わる）、適用の途中なら最後に公開された状態（適用の前の一貫した状態）を
    // 返す。どちらも無いとき（起動の後に一度も読めていないうちの適用の途中）は `None`。
    let Ok(slot) = lan.slot.try_lock() else {
        return Ok(lan
            .published
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone());
    };
    let config = settings.server_config().await?;
    let restart_required = if lan.always_on.is_some() {
        let auth = settings.auth_config().await?;
        lan.restart_required(&auth, &config)
    } else {
        false
    };
    let running = match slot.as_ref().map(RunningServer::local_addr) {
        Some(addr) => Some(addr),
        // 常時の listener は入れ替えないので、slot を持たずに読んでよい。
        None => lan.always_on_lan_addr().await,
    };
    let view = build_status(&config, running, restart_required);
    publish(&lan.published, &view);
    drop(slot);
    Ok(Some(view))
}

// --- 適用（デスクトップと REST で共有） ------------------------------------------

/// 監査に残す操作者。
#[derive(Debug, Clone, Copy)]
pub struct ApplyActor<'a> {
    pub username: Option<&'a str>,
    pub role: Option<&'a str>,
}

/// [`apply_server_settings`] に要るもの。
pub struct ApplyContext<'a> {
    pub settings: &'a SettingsService,
    pub audit: &'a AuditLogService,
    /// 閲覧公開を OFF にしたときの失効に使う（listener を入れ替えても続く 1 つの状態）。
    pub rest_auth: &'a AuthState,
    pub lan: &'a LanControl,
    /// 新しい LAN の listener に載せるルーター。
    pub router: &'a (dyn Fn() -> Router + Send + Sync),
    /// 検証・保存・失効を `auth.*` の他の書き手と直列にする lock（デスクトップは
    /// `AppState::auth_config_lock`）。lock の順序は「LAN の slot → この lock」。
    pub auth_config_lock: &'a AsyncMutex<()>,
    /// 監査の `origin`（`tauri` / `rest`）。
    pub origin: &'a str,
    /// ログの接頭辞（`banto` / `chronogazer-server`）。
    pub log_label: &'a str,
}

/// LAN の設定を適用する（spec §11.4 の「保存して適用」。admin-template v3.0.0 の
/// `server_apply`、banto v2.1.0 #287 / #288 / #294 の順序・巻き戻し・監査。B2-1 で
/// デスクトップの `src-tauri` から移し、REST と共有した）。
///
/// 順序と失敗の約束: 検証 → 古い LAN の listener を止める → 新しい listener を `bind`
/// （塞がったポートはここで失敗し、まだ何も保存していない）→ 保存（1 トランザクション）→
/// serve。新しい listener は保存が済むまで要求に答えないので、新しい listener と古い
/// 保存値を同時に見られることはない（閲覧公開の発行は保存値を読む）。bind か保存が
/// 失敗したら、新しい listener を捨て、何も保存せず、動いていた listener を（そのときの
/// アドレスで）開き直してエラーを返す。`enabled = false` は新しい listener が無い。
/// 試みは `settings_change` / `result: "failed"`（`detail.saved: false`）で、成功は `ok`
/// で残す。
///
/// **常時の listener の形が変わる設定**（`chronogazer-server` のポートの変更・`0.0.0.0`
/// への出し入れ）は listener に触れず、検証して保存だけ行い `restartRequired: true` を
/// 返す（モジュール doc「LAN の入れ替え」）。ポートを変えるときは、保存の前に新しい
/// `127.0.0.1:<port>` を一度 bind できるか確かめる。
///
/// lock の順序: LAN の slot（適用の間ずっと。並行する適用の直列化と、状態の表示が
/// 止めて開き直す間の隙間を見ないため）→ `auth_config_lock`（検証・保存・失効の間だけ。
/// 古い listener の停止と bind の間は持たない）。
pub async fn apply_server_settings(
    ctx: &ApplyContext<'_>,
    config: ServerSettings,
    actor: ApplyActor<'_>,
) -> Result<ServerStatusView, BantoError> {
    let mut slot = ctx.lan.slot.lock().await;

    // 許されない認証と LAN の組み合わせは、何かを止める前に断る。
    ctx.settings.validate_server_config(&config).await?;
    let auth = if ctx.lan.always_on.is_some() {
        Some(ctx.settings.auth_config().await?)
    } else {
        None
    };
    // 失敗したときに公開する状態（何も保存しないので、いまの保存値のまま）。
    let previous = ctx.settings.server_config().await?;
    let restart_required_for = |config: &ServerSettings| {
        auth.as_ref()
            .is_some_and(|auth| ctx.lan.restart_required(auth, config))
    };

    if let Some(auth) = &auth {
        if ctx.lan.restart_required(auth, &config) {
            let outcome = async {
                let always_on = ctx.lan.always_on.as_ref().expect("always_on");
                if config.port != always_on.config.port {
                    // 塞がったポートを保存すると次の起動が止まる。一度 bind して確かめる
                    // （確かめた後に別のプロセスが取ることは防げない - 次の起動の失敗で
                    // 分かる）。
                    drop(
                        bind_listener(ServerConfig {
                            bind: "127.0.0.1".to_string(),
                            port: config.port,
                        })
                        .await?,
                    );
                }
                save_server_config_locked(ctx, &config).await
            }
            .await;
            let running = match slot.as_ref().map(RunningServer::local_addr) {
                Some(addr) => Some(addr),
                None => ctx.lan.always_on_lan_addr().await,
            };
            // slot を手放す前に、適用の結果の状態を公開する（`GET` は slot を待たない）。
            let view = match &outcome {
                Ok(()) => build_status(&config, running, true),
                Err(_) => build_status(&previous, running, restart_required_for(&previous)),
            };
            publish(&ctx.lan.published, &view);
            drop(slot);
            return match outcome {
                Ok(()) => {
                    record(
                        ctx,
                        actor,
                        "ok",
                        serde_json::json!({
                            "serverEnabled": config.enabled,
                            "bind": config.bind,
                            "port": config.port,
                            "viewerPublic": config.viewer_public,
                            "restartRequired": true,
                        }),
                    )
                    .await;
                    Ok(view)
                }
                Err(err) => {
                    record(
                        ctx,
                        actor,
                        "failed",
                        serde_json::json!({
                            "serverEnabled": config.enabled,
                            "bind": config.bind,
                            "port": config.port,
                            "viewerPublic": config.viewer_public,
                            "restartRequired": true,
                            "saved": false,
                            "restoredPrevious": true,
                            "error": err.to_string(),
                        }),
                    )
                    .await;
                    Err(err)
                }
            };
        }
    }

    let desired = match &auth {
        Some(auth) => ctx.lan.desired_lan(auth, &config),
        None => ctx.lan.desired_lan(&AuthSettings::default(), &config),
    };

    // 巻き戻しは「動いていたもの」を、そのときのアドレスで開き直す。
    let was_running = slot.take();
    let previous_addr = was_running.as_ref().map(RunningServer::local_addr);
    if let Some(running) = was_running {
        running.stop().await;
    }

    // banto #287 / #294 review: bind -> save -> serve。
    let outcome: Result<Option<RunningServer>, BantoError> = async {
        let bound = match &desired {
            Some(config) => Some(bind_listener(config.clone()).await?),
            None => None,
        };
        // 失敗したら `bound` はここで捨てられ、ポートを返す。
        save_server_config_locked(ctx, &config).await?;
        Ok(bound.map(|bound| bound.serve((ctx.router)())))
    }
    .await;

    match outcome {
        Ok(started) => {
            let started_addr = started.as_ref().map(RunningServer::local_addr);
            *slot = started;
            let running = match started_addr {
                Some(addr) => Some(addr),
                None => ctx.lan.always_on_lan_addr().await,
            };
            let view = build_status(&config, running, false);
            publish(&ctx.lan.published, &view);
            drop(slot);
            record(
                ctx,
                actor,
                "ok",
                serde_json::json!({
                    "serverEnabled": config.enabled,
                    "bind": config.bind,
                    "port": config.port,
                    "viewerPublic": config.viewer_public,
                }),
            )
            .await;
            Ok(view)
        }
        Err(err) => {
            // 何も保存していない。動いていた listener を開き直し、いまの状態を（変わって
            // いない）保存値に揃える。
            let mut restored = previous_addr.is_none();
            if let Some(addr) = previous_addr {
                match bind_listener(ServerConfig {
                    bind: addr.ip().to_string(),
                    port: addr.port(),
                })
                .await
                {
                    Ok(bound) => {
                        *slot = Some(bound.serve((ctx.router)()));
                        restored = true;
                    }
                    Err(restore_err) => eprintln!(
                        "{}: LAN設定の適用に失敗し、旧設定でのサーバー再起動にも失敗しました: {restore_err}",
                        ctx.log_label
                    ),
                }
            }
            let running = match slot.as_ref().map(RunningServer::local_addr) {
                Some(addr) => Some(addr),
                None => ctx.lan.always_on_lan_addr().await,
            };
            publish(
                &ctx.lan.published,
                &build_status(&previous, running, restart_required_for(&previous)),
            );
            drop(slot);
            record(
                ctx,
                actor,
                "failed",
                serde_json::json!({
                    "serverEnabled": config.enabled,
                    "bind": config.bind,
                    "port": config.port,
                    "viewerPublic": config.viewer_public,
                    "saved": false,
                    "restoredPrevious": restored,
                    "error": err.to_string(),
                }),
            )
            .await;
            Err(err)
        }
    }
}

/// 検証し直して（`set_server_config` が `auth.disabled` を読み直す）保存し、閲覧公開が
/// OFF なら閲覧者のトークンを失効させる。すべて `auth_config_lock` の下（検証と保存の
/// 間に `auth.*` の書き込みが割り込まない。#294 review）。
async fn save_server_config_locked(
    ctx: &ApplyContext<'_>,
    config: &ServerSettings,
) -> Result<(), BantoError> {
    let _auth_config = ctx.auth_config_lock.lock().await;
    ctx.settings.set_server_config(config).await?;
    if !config.viewer_public {
        // ADR-0017 §2: 閉じた条件を保存してから失効。
        ctx.rest_auth
            .revoke_grant_tokens(&GrantKind::public_viewer());
    }
    Ok(())
}

async fn record(
    ctx: &ApplyContext<'_>,
    actor: ApplyActor<'_>,
    result: &str,
    detail: serde_json::Value,
) {
    ctx.audit
        .record(AuditEntry {
            actor_username: actor.username,
            actor_role: actor.role,
            action: "settings_change",
            resource: "settings",
            entity_id: None,
            detail: Some(detail),
            origin: ctx.origin,
            result,
        })
        .await;
}

// --- 認証の設定 --------------------------------------------------------------------

/// `GET`/`PUT /api/settings/auth` の応答。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthConfigView {
    /// ログイン不要モード（デスクトップの画面だけに効く。REST では読むだけ）。
    pub disabled: bool,
    pub disabled_role: Role,
    /// 保存されている `auth.local_grant`（`off`/`viewer`/`editor`/`admin`）。
    pub local_grant: &'static str,
    /// この host が `localDesktop` を発行するか（`chronogazer-server` だけ `true`）。
    pub local_grant_supported: bool,
    /// 保存されている値で発行するのに再起動が要るか。
    pub local_grant_restart_required: bool,
}

/// `PUT /api/settings/auth` の本文。ログイン不要モードの欄は受け取るが、送られたら断る。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AuthConfigPayload {
    local_grant: String,
}

/// ログイン不要モードの欄（値が `null` でも、欄があれば断る - #600 のレビュー。
/// `Option<Value>` では明示の `null` が「無い」と区別できず、更新が通ってしまった）。
const AUTH_DISABLED_FIELDS: [&str; 2] = ["disabled", "disabledRole"];

// --- REST ----------------------------------------------------------------------------

/// `/api/settings/*` のルーターの状態。
#[derive(Clone)]
pub struct SettingsAdminState {
    pub settings: SettingsService,
    pub users: UsersService,
    pub audit: AuditLogService,
    pub auth: AuthState,
    pub lan: LanControl,
    pub local_grant: LocalGrantStart,
    /// REST の設定の書き込みを直列にする lock（`chronogazer-server` の
    /// [`ApplyContext::auth_config_lock`]）。
    pub auth_config_lock: Arc<AsyncMutex<()>>,
    pub log_label: &'static str,
}

/// `/api/settings/server`・`/api/settings/auth`（admin。モジュール doc の表）。
pub fn settings_admin_router(state: SettingsAdminState) -> Router {
    let guard = RoleGuard {
        auth: state.auth.clone(),
        min: Role::Admin,
        resource: "settings",
        audit: state.audit.clone(),
    };
    let auth = state.auth.clone();
    Router::new()
        .route(
            "/api/settings/server",
            get(server_settings_get).put(server_settings_put),
        )
        .route(
            "/api/settings/auth",
            get(auth_settings_get).put(auth_settings_put),
        )
        .with_state(state)
        .layer(middleware::from_fn_with_state(guard, require_role_at_least))
        .layer(middleware::from_fn_with_state(auth, require_auth))
}

fn forbidden_with(message: &str) -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(ErrorBody::Other {
            message: message.to_string(),
        }),
    )
        .into_response()
}

async fn server_settings_get(State(state): State<SettingsAdminState>) -> Response {
    match current_status(&state.settings, &state.lan).await {
        Ok(Some(view)) => Json(view).into_response(),
        Ok(None) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ErrorBody::Other {
                message: "LAN の設定を適用している途中です。少し待ってから読み直してください"
                    .to_string(),
            }),
        )
            .into_response(),
        Err(err) => ApiError(err).into_response(),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ServerSettingsPayload {
    enabled: bool,
    bind: String,
    port: u16,
    viewer_public: bool,
}

async fn server_settings_put(State(state): State<SettingsAdminState>, req: Request) -> Response {
    if !state.lan.rest_apply_supported() {
        return forbidden_with(
            "この host では LAN の公開設定を REST から変更できません（デスクトップは画面の設定から変更してください）",
        );
    }
    if !request_is_loopback_local(&req) {
        return forbidden_with(
            "LAN の公開設定は、この PC からの接続（127.0.0.1）でだけ変更できます",
        );
    }
    let headers = req.headers().clone();
    let Json(payload) = match Json::<ServerSettingsPayload>::from_request(req, &()).await {
        Ok(payload) => payload,
        Err(rejection) => return rejection.into_response(),
    };
    let actor = actor_identity(&headers, &state.auth);
    let config = ServerSettings {
        enabled: payload.enabled,
        bind: payload.bind,
        port: payload.port,
        viewer_public: payload.viewer_public,
    };
    // 適用（古い listener の停止・bind・保存・巻き戻し・監査）は応答の future から
    // 切り離す（#600 の監査 F2・F4、`crate::local_grant::run_detached`）: handler の中で
    // 待つと、要求を送った側が接続を切ったときに hyper が future を捨て、slot が空のまま
    // （保存値は「公開」なのに LAN は止まったまま）・監査の行が無いまま終わる。
    let label = state.log_label;
    let applied = run_detached(label, async move {
        let factory = state.lan.router.clone().expect("rest_apply_supported");
        let router = move || factory();
        let ctx = ApplyContext {
            settings: &state.settings,
            audit: &state.audit,
            rest_auth: &state.auth,
            lan: &state.lan,
            router: &router,
            auth_config_lock: &state.auth_config_lock,
            origin: "rest",
            log_label: state.log_label,
        };
        apply_server_settings(
            &ctx,
            config,
            ApplyActor {
                username: actor.as_ref().map(|identity| identity.id.as_str()),
                role: actor.as_ref().map(|identity| identity.role.as_str()),
            },
        )
        .await
    })
    .await;
    match applied {
        Ok(Ok(status)) => Json(status).into_response(),
        Ok(Err(err)) => ApiError(err).into_response(),
        Err(failed) => failed.into_response(),
    }
}

async fn auth_view(state: &SettingsAdminState) -> Result<AuthConfigView, BantoError> {
    let auth = state.settings.auth_config().await?;
    let saved = read_setting(&state.settings).await?;
    let initialized = state.users.is_initialized().await?;
    Ok(AuthConfigView {
        disabled: auth.disabled,
        disabled_role: auth.disabled_role,
        local_grant: saved.as_str(),
        local_grant_supported: state.local_grant != LocalGrantStart::Unsupported,
        local_grant_restart_required: state.local_grant.restart_required(saved, initialized),
    })
}

async fn auth_settings_get(
    State(state): State<SettingsAdminState>,
) -> Result<Json<AuthConfigView>, ApiError> {
    Ok(Json(auth_view(&state).await?))
}

async fn auth_settings_put(State(state): State<SettingsAdminState>, req: Request) -> Response {
    // `localGrant` は「この PC から誰に何を渡すか」なので、変更もこの PC からの要求だけ
    // （`PUT /api/settings/server` と同じ扱い。LAN からは admin でも `403`。2026-10-14
    // オーナー決定）。
    if !request_is_loopback_local(&req) {
        return forbidden_with(
            "この PC からの接続の設定は、この PC からの接続（127.0.0.1）でだけ変更できます",
        );
    }
    let headers = req.headers().clone();
    let Json(body) =
        match Json::<serde_json::Map<String, serde_json::Value>>::from_request(req, &()).await {
            Ok(body) => body,
            Err(rejection) => return rejection.into_response(),
        };
    if AUTH_DISABLED_FIELDS
        .iter()
        .any(|field| body.contains_key(*field))
    {
        return ApiError(BantoError::BadRequest(
            "ログイン不要モードは REST からは変更できません（デスクトップの画面の設定から変更してください。サービスではこの PC からの接続（localGrant）を使います）".to_string(),
        ))
        .into_response();
    }
    let payload = match serde_json::from_value::<AuthConfigPayload>(serde_json::Value::Object(body))
    {
        Ok(payload) => payload,
        Err(err) => {
            return ApiError(BantoError::BadRequest(format!(
                "本文を読めません（{{\"localGrant\": \"off\"}} の形）: {err}"
            )))
            .into_response()
        }
    };
    let value = match LocalGrantSetting::parse(&payload.local_grant) {
        Ok(value) => value,
        Err(err) => return ApiError(err).into_response(),
    };
    // 操作者は保存・失効の**前に**確定する（#600 のレビュー）: `localDesktop` の admin の
    // トークンで値を変えると、下の失効がこの要求自身のトークンも消し、後から引くと
    // 操作者が空のまま特権の変更が監査に残る（banto-hub の lock-down と同じ扱い）。
    let actor = actor_identity(&headers, &state.auth);
    // 保存・失効・監査は応答の future から切り離す（#600 の監査 F4。保存と失効の間に
    // await は無いが、その後の監査の行が接続の切断で落ちうる）。
    let label = state.log_label;
    let applied = run_detached(label, async move {
        auth_settings_apply(&state, actor, value).await
    })
    .await;
    match applied {
        Ok(Ok(view)) => Json(view).into_response(),
        Ok(Err(err)) => err.into_response(),
        Err(failed) => failed.into_response(),
    }
}

async fn auth_settings_apply(
    state: &SettingsAdminState,
    actor: Option<Identity>,
    value: LocalGrantSetting,
) -> Result<AuthConfigView, ApiError> {
    let revoked = {
        let _guard = state.auth_config_lock.lock().await;
        let previous = read_setting(&state.settings).await?;
        write_setting(&state.settings, value).await?;
        // ADR-0017: 閉じた条件を保存してから失効。値が変わったときだけ（同じ値の保存で
        // 動いているセッションを切らない）。
        if previous != value {
            state.auth.revoke_grant_tokens(&local_desktop_grant_kind())
        } else {
            0
        }
    };
    state
        .audit
        .record(AuditEntry {
            actor_username: actor.as_ref().map(|identity| identity.id.as_str()),
            actor_role: actor.as_ref().map(|identity| identity.role.as_str()),
            action: "settings_change",
            resource: "settings",
            entity_id: None,
            detail: Some(serde_json::json!({
                "localGrant": value.as_str(),
                "revokedGrants": revoked,
            })),
            origin: "rest",
            result: "ok",
        })
        .await;
    Ok(auth_view(state).await?)
}
