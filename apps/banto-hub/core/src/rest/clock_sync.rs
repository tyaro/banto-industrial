//! H2（tyaro/banto-industrial#1053）: PLC の時計合わせの REST
//! `GET`・`PUT`・`POST /api/plc-connections/{id}/clock-sync`。中身は
//! `crate::clock_sync`（設定・実行・監査・直近の結果）で、ここは認可と形だけ。
//! 書き方は診断（`rest/diagnostics.rs`）に合わせる。
//!
//! - 3 本とも editor（診断の GET と同じ）。拒否は `require_editor` が監査する。
//! - `GET`・`PUT` は保存済みの接続（`plc_connections`）が無ければ 404。適用済みの
//!   カタログ（収集中だけある）ではなく保存済みで見るのは、収集を止めたまま
//!   時刻表を設定できるようにするため。
//! - `PUT` の本文は `{"enabled": bool, "schedule": {...}}`（未知のキー・範囲外は
//!   400）。保留キューを通さず即時に保存し、監査（`update`）する。保存・監査・応答の
//!   view は `tokio::spawn` で切り離して `JoinHandle` を待つ（下の POST と同じ理由）。
//! - `POST` は適用済みのカタログに無い接続には 404（診断の POST と同じ）。Hub の
//!   `enabled` を問わず今すぐ 1 回実行して結果を返す（PLC に書くのは editor の
//!   明示の操作。ゲストの `clock_set` と運用書込みの loader のゲートは残る）。
//!   実行は `tokio::spawn` で切り離して `JoinHandle` を待つ（要求の接続が切れても
//!   監査と直近の結果を書き切る、docs/implementation-checklist.md §5）。PUT の保存と
//!   監査も同じ形（保存の後・監査の前に切れて、監査なしで時刻表が有効になるのを防ぐ）。
//!   どちらも Hub の数え（`crate::detached`、`manager.detached().spawn`）で数え、Hub の
//!   終了は pool を閉じる前に待つ。
//!   終了の受付を閉じた後は spawn せずに 503（何も保存・送信しない）。
//!
//! H3: `GET /api/plc-connections/{id}/clock`（PLC の時計の読取り）もここに置く。
//! 認可は editor（PLC と通信するため。診断の GET・上の 3 本と同じ）、保存済みの接続が
//! 無ければ 404。Hub の `clockSync.enabled` は問わない。中身は
//! `ClockSync::read_plc_clock`（直近の結果を 5 秒使い回す。`crate::clock_read`）。
//! PLC に何も書かず監査もしないので、切り離しタスクは作らず handler の中で待つ
//! （要求の接続が切れて読取りが捨てられても、broker のジョブは最後まで走り、
//! 残すものは無い）。

use super::{
    actor_identity, record_write, require_editor, shutting_down_response, ApiError, BantoError,
    HeaderMap, IntoResponse, Json, Path, Response, State, StatusCode, TagRegistryState,
};
use crate::clock_read::ClockReadView;
use crate::clock_sync::{
    parse_settings, ClockActor, ClockSyncRecord, ClockSyncSettings, ClockSyncView, ClockTrigger,
    AUDIT_RESOURCE,
};
use axum::body::Bytes;
use serde_json::json;

const PATH: &str = "/api/plc-connections/{id}/clock-sync";
const CLOCK_PATH: &str = "/api/plc-connections/{id}/clock";

/// 保存済みの接続か（無ければ 404）。
async fn require_saved_connection(state: &TagRegistryState, id: i64) -> Result<(), ApiError> {
    state.plc_connections.get(id).await?;
    Ok(())
}

/// 接続の時計合わせの設定と直近の結果。
#[utoipa::path(
    get,
    path = "/api/plc-connections/{id}/clock-sync",
    params(("id" = i64, Path, description = "PLC 接続の id")),
    responses(
        (status = 200, description = "設定（行が無ければ既定の「無効・毎時 5 分」）と直近の結果（メモリだけ。無ければ null）", body = ClockSyncView),
        (status = 403, description = "editor 未満"),
        (status = 404, description = "保存済みの接続に無い"),
    ),
    tag = "clock-sync",
)]
pub(super) async fn clock_sync_get(
    State(state): State<TagRegistryState>,
    headers: HeaderMap,
    Path(connection_id): Path<i64>,
) -> Result<Json<ClockSyncView>, ApiError> {
    require_editor(
        &state.auth,
        &state.audit,
        &headers,
        "plc_connections",
        "GET",
        PATH,
    )
    .await?;
    require_saved_connection(&state, connection_id).await?;
    Ok(Json(state.manager.clock_sync().view(connection_id).await?))
}

/// 接続の時計合わせの設定を変える（保留キューを通さず即時）。
#[utoipa::path(
    put,
    path = "/api/plc-connections/{id}/clock-sync",
    params(("id" = i64, Path, description = "PLC 接続の id")),
    request_body(
        content = ClockSyncSettings,
        description = "enabled（定期の実行。既定は無効）と時刻表（毎時 m 分 {\"kind\":\"hourly\",\"minute\":m} か毎日 hh:mm {\"kind\":\"daily\",\"hour\":h,\"minute\":m}）。時刻は Hub の PC の現地時刻で、過ぎた回は後から実行しない（追いつかない）。危険: PLC 自身の SNTP・PLC のプログラム・GOT の時刻同期などが同じ時計を合わせていると互いに上書きする。毎回 PLC のイベント履歴に「時計設定」（イベントコード 24000）が残るので、間隔を短くしすぎないこと。実際に書くにはゲストの接続設定 clock_set と、署名付きで書込みを選んだ運用書込みの loader も要る。",
    ),
    responses(
        (status = 200, description = "保存した設定と直近の結果", body = ClockSyncView),
        (status = 400, description = "未知のキー・型の違い・範囲外（分 0–59、時 0–23）"),
        (status = 403, description = "editor 未満"),
        (status = 404, description = "保存済みの接続に無い"),
        (status = 503, description = "Hub の終了中（保存しない）"),
    ),
    tag = "clock-sync",
)]
pub(super) async fn clock_sync_put(
    State(state): State<TagRegistryState>,
    headers: HeaderMap,
    Path(connection_id): Path<i64>,
    body: Bytes,
) -> Result<Response, ApiError> {
    require_editor(
        &state.auth,
        &state.audit,
        &headers,
        "plc_connections",
        "PUT",
        PATH,
    )
    .await?;
    let settings = parse_settings(&body).map_err(BantoError::BadRequest)?;
    require_saved_connection(&state, connection_id).await?;
    // 保存→監査→view を切り離す（保存の後・監査の前に要求の接続が切れても、監査なしで
    // 時刻表が有効になることがないように。POST と同じ）。
    let manager = state.manager.clone();
    let audit = state.audit.clone();
    let auth = state.auth.clone();
    let Some(task) = state.manager.detached().spawn(async move {
        let clock_sync = manager.clock_sync();
        clock_sync.save_settings(connection_id, &settings).await?;
        record_write(
            &audit,
            &auth,
            &headers,
            "update",
            AUDIT_RESOURCE,
            Some(&connection_id.to_string()),
            Some(json!({ "enabled": settings.enabled, "schedule": settings.schedule })),
        )
        .await;
        clock_sync.view(connection_id).await
    }) else {
        return Ok(shutting_down_response());
    };
    let view = task.await.map_err(|err| {
        BantoError::Other(format!("時計合わせの設定のタスクが異常終了しました: {err}"))
    })??;
    Ok(Json(view).into_response())
}

/// 今すぐ 1 回、PLC の時計を合わせる。
#[utoipa::path(
    post,
    path = "/api/plc-connections/{id}/clock-sync",
    params(("id" = i64, Path, description = "PLC 接続の id")),
    responses(
        (status = 200, description = "この回の結果（result: ok・unsupported・rejected・device（code に終了コード）・verificationFailed・unavailable・unknown（結果不明。自動では再送しない）・notOffered（このドライバ・loader は時計合わせを出さない）・busy）。監査と直近の結果にも残る。Hub の enabled を問わず出せるが、ゲストの接続設定 clock_set と運用書込みの loader のゲートは残る。危険: PLC 自身の SNTP・PLC のプログラム・GOT の時刻同期などと互いに上書きする。毎回 PLC のイベント履歴に「時計設定」（24000）が残る。", body = ClockSyncRecord),
        (status = 403, description = "editor 未満"),
        (status = 404, description = "適用済みのカタログ（収集中の構成）に無い接続"),
        (status = 503, description = "Hub の終了中（何も送らない）"),
    ),
    tag = "clock-sync",
)]
pub(super) async fn clock_sync_post(
    State(state): State<TagRegistryState>,
    headers: HeaderMap,
    Path(connection_id): Path<i64>,
) -> Result<Response, ApiError> {
    require_editor(
        &state.auth,
        &state.audit,
        &headers,
        "plc_connections",
        "POST",
        PATH,
    )
    .await?;
    if state.manager.detached().is_closed() {
        return Ok(shutting_down_response());
    }
    let (_, applied) = state.manager.diagnostic_registry_snapshot();
    if !applied.connections.iter().any(|c| c.id == connection_id) {
        return Ok((
            StatusCode::NOT_FOUND,
            Json(json!({ "status": "unavailable", "reason": "connection not in applied catalog" })),
        )
            .into_response());
    }
    // `require_editor` を通ったので identity はある（無ければ監査は actor なし）。
    let actor = actor_identity(&headers, &state.auth).map(|identity| (identity.id, identity.role));
    let manager = state.manager.clone();
    let audit = state.audit.clone();
    let Some(task) = state.manager.detached().spawn(async move {
        let actor = actor.as_ref().map(|(username, role)| ClockActor {
            username: username.as_str(),
            role: role.as_str(),
        });
        manager
            .clock_sync()
            .run(
                manager.sessions(),
                &audit,
                connection_id,
                ClockTrigger::Manual,
                actor,
            )
            .await
    }) else {
        return Ok(shutting_down_response());
    };
    let record = task
        .await
        .map_err(|err| BantoError::Other(format!("時計合わせのタスクが異常終了しました: {err}")))?;
    Ok(Json(record).into_response())
}

/// PLC の今の時計を読む（H3）。
#[utoipa::path(
    get,
    path = "/api/plc-connections/{id}/clock",
    params(("id" = i64, Path, description = "PLC 接続の id")),
    responses(
        (status = 200, description = "PLC の時刻（plcTime、ok のときだけ）・Hub の PC の現地時刻（pcTime、broker の応答を受けた直後を秒に切り捨て）・差（offsetSeconds = plcTime − pcTime、PLC が進んでいれば正、±1 秒の誤差、ok のときだけ）。result は ok・unsupported・rejected・device（code に終了コード）・verificationFailed・unavailable（送っていない）・unknown（結果不明。読取りは何も書かないので時計には影響しない。接続は再接続される）・notOffered（このドライバは時計の読取りを出さない）。PLC に何も書かない。PLC のプログラムで SM213 を ON にしておく必要がある（OFF なら rejected）。iQ-R 系と標準 Q・L だけ（他は unsupported）。直近の結果を接続ごとに 5 秒使い回す（cached: true。unavailable は使い回さない）。Hub の clockSync.enabled は問わない。監査には残さない。", body = ClockReadView),
        (status = 403, description = "editor 未満"),
        (status = 404, description = "保存済みの接続に無い"),
    ),
    tag = "clock-sync",
)]
pub(super) async fn plc_clock_get(
    State(state): State<TagRegistryState>,
    headers: HeaderMap,
    Path(connection_id): Path<i64>,
) -> Result<Json<ClockReadView>, ApiError> {
    require_editor(
        &state.auth,
        &state.audit,
        &headers,
        "plc_connections",
        "GET",
        CLOCK_PATH,
    )
    .await?;
    require_saved_connection(&state, connection_id).await?;
    Ok(Json(
        state
            .manager
            .clock_sync()
            .read_plc_clock(state.manager.sessions(), connection_id)
            .await,
    ))
}
