//! PLC 接続の CRUD（`/api/plc-connections`）と接続テスト（保存前の疎通確認・保存済み
//! 接続のテスト）。
//!
//! `rest.rs` から中身を変えずに移したもの（docs/banto-hub-driver-refactor-plan.md §8.1、
//! P0-1 の手順 3）。I1 CRUD の共通層は `rest/crud.rs` にあり、認証・ガード・監査の補助は
//! 親の `rest.rs` にある。どちらも `use super::{...}` で使う。外から使われるパス
//! （`crate::rest::X`）は親の re-export で保つ。

use super::{
    commit_catalog_and_notify, default_payload_enabled, json, preflight_transaction,
    queue_pending_registry_change, record_write, registry_before, registry_change_should_queue,
    reject_delete_if_referenced_by_sink_group, require_editor, storage_api_error, ApiError,
    BantoError, BrokerConnectionStatus, BrokerError, CollectorManager, Deserialize, Duration,
    FieldError, HeaderMap, IntoResponse, Json, Path, PlcConnection, PlcConnectionInput,
    RegistryMutationError, RegistryMutationResult, Response, Serialize, State, StatusCode,
    TagRegistryState,
};
use banto_driver_api::{keys, ConnectContext, FatalError, ItemError, Settings, TagSpec, Value};

fn default_plc_protocol() -> String {
    "modbus-tcp".to_string()
}

fn default_plc_unit_id() -> i64 {
    1
}

/// T9-2 (docs/ux-plan.md §1): a `PlcConnectionPayload` missing `simulation`
/// (an old client, or a create/update that never mentions it) keeps the
/// existing safe default - a connection is never accidentally simulated by
/// omission.
fn default_plc_simulation() -> bool {
    false
}

/// P3-b（監査指摘 2026-08-12）/ 2026-09-08 オーナー決定: a
/// `PlcConnectionPayload` missing `wordOrder` (an old client, or a
/// create/update that never mentions it) defers to
/// `banto_tags::plc_connection`'s **protocol-aware** default - `"high_low"`
/// for `modbus-tcp`, `"low_high"` for everything else - by handing it the
/// empty-string "unspecified" sentinel rather than a concrete order.
///
/// Returning `"low_high"` here (as this fn did until 2026-09-08) would mask
/// that default entirely: the payload would arrive at
/// `PlcConnectionInput` already filled in, so an omitted `wordOrder` on a
/// Modbus connection could never reach the `"high_low"` default the owner
/// decision requires. `""` is a safe sentinel because it is not in
/// `ALLOWED_WORD_ORDERS` and migration `0010`'s SQL `CHECK` rejects it, so
/// it was never a storable value.
fn default_plc_word_order() -> String {
    String::new()
}

/// Wire-shaped (camelCase) create/update payload for `plc_connections` -
/// copied from relay-wright's `PlcConnectionPayload` (invariant across every
/// app that exposes I1 over REST: one payload shape).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlcConnectionPayload {
    pub name: String,
    #[serde(default = "default_plc_protocol")]
    pub protocol: String,
    pub host: String,
    pub port: i64,
    #[serde(default = "default_plc_unit_id")]
    pub unit_id: i64,
    #[serde(default = "default_payload_enabled")]
    pub enabled: bool,
    /// T9-2 (docs/ux-plan.md §1, 「接続単位のシミュレーションモード」): opts
    /// this connection into an in-process simulator instead of a real PLC -
    /// see `banto_tags::PlcConnection::simulation`'s doc comment for what
    /// this actually does at collection time
    /// (`crates/banto-collect/src/simulation.rs`) and, for broker-routed
    /// connections specifically, `crate::broker_glue::BrokerSimRegistry`.
    #[serde(default = "default_plc_simulation")]
    pub simulation: bool,
    /// P3-b（監査指摘 2026-08-12）: ワード順（32bit値 u32/f32 等、および
    /// issue #325 で追加された 64bit 値 i64/u64/f64（4レジスタ）の上位/下位
    /// ワードの並び）。`"low_high"` / `"high_low"` のいずれか - 検証は
    /// `banto_tags::plc_connection::validate_plc_connection_input` 側
    /// （`ALLOWED_WORD_ORDERS`）に委ねる。
    ///
    /// 2026-09-08 オーナー決定: **modbus-tcp でも有効な設定**（既定は
    /// `"high_low"` = Modbus/IEEE 慣習、SLMP は `"low_high"` = MELSEC 標準）。
    /// それ以前は収集経路が Modbus のこの列を無視して `HighLow` 固定で動く
    /// 一方 read-on-demand/書き込み経路は列を読んでおり、同じタグの値が経路
    /// ごとにワード反転して食い違っていた。フォームも "slmp" 選択時しか
    /// 表示していなかったが、現在は modbus-tcp でも表示する。virtual/postgres
    /// 接続では引き続き無意味（`unit_id` と同じ扱い）。
    ///
    /// 省略時は [`default_plc_word_order`] の "unspecified" 番兵を経由して
    /// プロトコル依存の既定が適用される。
    #[serde(default = "default_plc_word_order")]
    pub word_order: String,
    /// #605: SLMP の CPU 種別（`"Q"` / `"R"`）。省略と空文字は
    /// `banto_tags::PlcConnectionInput::cpu_type` と同じ意味
    /// （作成は `"R"`、更新は既存の値を保つ）。未知の値は検証エラー。
    #[serde(default)]
    pub cpu_type: Option<String>,
    /// S1（docs/banto-hub-external-db-design.md §4.1）: `protocol: "postgres"`
    /// 接続の DB 名。他プロトコルでは省略（`None`）のままにする -
    /// `banto_tags::plc_connection::validate_plc_connection_input`が
    /// postgres 以外での指定を拒否する。
    #[serde(default)]
    pub database: Option<String>,
    /// S1: `protocol: "postgres"` 接続の DB ユーザー名。[`Self::database`]と
    /// 同じ省略時ルール。
    #[serde(default)]
    pub username: Option<String>,
    /// S1: `protocol: "postgres"` 接続のパスワード。R2b-1d から入口で封印して秘密の表に
    /// 置く（R2b-1e: Windows は DPAPI、それ以外は `plain-v1`。平文の列は無い）。
    /// **create と update で意味が異なる** -
    /// `banto_tags::PlcConnectionInput::password`の doc comment 参照:
    /// - create: 省略/`null`/空文字列はいずれも「パスワード無し」。
    /// - update: 省略/`null`は「現在のパスワードを変更しない」、空文字列は
    ///   「消去」、それ以外の文字列は「置き換え」。
    ///
    /// GET/list のレスポンスにはこのフィールドは無く、代わりに
    /// [`PlcConnectionResponse::password_set`]（bool）を返す - 平文
    /// パスワードを一度保存した接続を読み返すだけで露出させないため。
    #[serde(default)]
    pub password: Option<String>,
    /// R2a-2e（docs/banto-hub-ingest-design.md §3.2）: `protocol: "push"` 接続の
    /// `settings`（`stale_timeout_ms` とブリッジの区画、JSON のオブジェクト）。
    /// 他プロトコルでは省略する（`banto_tags::PlcConnectionInput::settings` が
    /// push 以外での指定を拒否する）。**create と update で意味が異なる**
    /// （`cpuType` と同じ）: create の省略は既定（SQL の `NULL`）、update の省略・
    /// `null` は「保存済みの値を保つ」、オブジェクトは置き換え（`{}` でブリッジを
    /// 外せる）。秘密値は持たない（ブリッジの DB のパスワードは参照先の `postgres`
    /// 接続の列のまま、§10.2）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings: Option<serde_json::Value>,
}

impl PlcConnectionPayload {
    /// banto-tags の入力に写す。R2b-1d（docs/banto-hub-connection-settings-design.md §5.3）
    /// から、`password` の平文は**ここで封印**して `PlcConnectionInput::secrets` に載せる
    /// （banto-tags は平文を受けない）。tri-state は [`crate::secrets::password_changes`]:
    /// 省略・`null` は `Keep`、空文字は `Clear`、値は `Set(封印した値)`。封印は
    /// [`crate::secrets::ConnectionSecrets::platform_default`]（R2b-1e: Windows は DPAPI の
    /// `dpapi-machine-v1`、それ以外は `plain-v1`）。封印に失敗したとき（大きすぎる値・乱数源・
    /// DPAPI の失敗）は `password` の項目エラーで、値は載せない。
    ///
    /// 未適用キューは R2b-1e から積む時点で封印した値（`sealedPassword`）を持ち、適用は
    /// [`Self::into_input_with_sealed`] を通る。
    pub(crate) fn into_input(self) -> Result<PlcConnectionInput, BantoError> {
        let secrets = crate::secrets::password_changes(
            &crate::secrets::ConnectionSecrets::platform_default(),
            self.password.as_deref(),
        )
        .map_err(|error| BantoError::Validation {
            field_errors: vec![FieldError {
                field: "password".to_string(),
                message: error.to_string(),
            }],
        })?;
        self.into_input_with(secrets)
    }

    /// 未適用キューの適用（R2b-1e、設計 §5.3）: 積む時点で封印した `sealed` があれば、それを
    /// **平文に戻さず** `SecretChange::Set` として渡す（参照は封印したときのもの。新しい行を
    /// 足すので、同じ参照の行が既にあれば DB の主キーが断り、適用は失敗する）。無ければ
    /// [`Self::into_input`]（空文字の「消す」・省略の「保つ」と、R2b-1e の前に積んだ平文の
    /// `password` の行）。
    ///
    /// `sealed` と非空の `password` が両方ある・`sealed` がこの PC で開けない（別の PC で
    /// 積んだ・壊れた）なら `password` の項目エラー（読めない値で接続を書き換えない）。
    pub(crate) fn into_input_with_sealed(
        self,
        sealed: Option<crate::secrets::SealedSecret>,
    ) -> Result<PlcConnectionInput, BantoError> {
        let Some(sealed) = sealed else {
            return self.into_input();
        };
        let password_error = |message: &str| BantoError::Validation {
            field_errors: vec![FieldError {
                field: "password".to_string(),
                message: message.to_string(),
            }],
        };
        if self.password.is_some() {
            return Err(password_error(
                "未適用キューの提案に平文と封印したパスワードの両方があります",
            ));
        }
        if crate::secrets::ConnectionSecrets::platform_default()
            .open(banto_tags::PASSWORD_KEY, &sealed)
            .is_err()
        {
            return Err(password_error(
                "未適用キューの提案のパスワードをこの PC で読めません（提案を破棄して、\
                 パスワードを入れ直して作り直してください）",
            ));
        }
        let mut secrets = banto_tags::SecretChanges::new();
        secrets.insert(
            banto_tags::PASSWORD_KEY.to_owned(),
            banto_tags::SecretChange::Set(sealed.into()),
        );
        self.into_input_with(secrets)
    }

    fn into_input_with(
        self,
        secrets: banto_tags::SecretChanges,
    ) -> Result<PlcConnectionInput, BantoError> {
        Ok(PlcConnectionInput {
            name: self.name,
            protocol: self.protocol,
            host: self.host,
            port: self.port,
            unit_id: self.unit_id,
            enabled: self.enabled,
            // T9-2: wired through - see `PlcConnectionPayload::simulation`'s
            // doc comment.
            simulation: self.simulation,
            // P3-b: wired through - see `PlcConnectionPayload::word_order`'s
            // doc comment.
            word_order: self.word_order,
            // #605: 省略は None のままサービスへ渡す（作成は R、更新は既存を保つ）。
            cpu_type: self.cpu_type,
            // S1: wired through verbatim.
            database: self.database,
            username: self.username,
            secrets,
            // R2a-2e: wired through - `None`（省略）は create では既定、update では
            // 「保存済みの値を保つ」（`banto_tags::PlcConnectionInput::settings`）
            // ので、この項目を知らないクライアントの更新が push 接続の設定を
            // 消すことは無い。
            settings: self.settings,
        })
    }
}

/// #383 段階3 P2（2026-10-10 オーナー決定 H1）: banto-tags のレジストリは
/// `protocol: "hub"`（banto-hub をデータ源とする接続、ChronoGazer が Hub 経由の
/// タグを記録するための種別）を受け付けるが、**banto-hub 自身はそれを作らない**。
/// Hub が自分自身を購読することは無く、Hub の収集（`banto_collect::build_config_from`
/// の厳格版）も `hub` 接続を「未対応」として全体を止める。接続を作る・変える
/// すべての入口 - REST の作成/更新（[`plc_connections_create`]/
/// [`plc_connections_update`]）、MCP の `create_connection`/`update_connection`、
/// 未適用キューの適用（[`execute_pending_apply`] の `plc_connections.*`）- が
/// キューへ積む**前に**これを呼び、`protocol` の項目エラーとして拒否する
/// （キューに積んでから適用時に落ちるのでは、どの変更が悪いかが遅れて分かる）。
/// 設定パッケージの取り込み（`configPackage.ts`）は REST の作成/更新を呼ぶので、
/// ここで同じく拒否される。
pub(crate) fn reject_hub_connection_protocol(
    payload: &PlcConnectionPayload,
) -> Result<(), BantoError> {
    if payload.protocol != banto_tags::HUB_PROTOCOL {
        return Ok(());
    }
    Err(BantoError::Validation {
        field_errors: vec![FieldError {
            field: "protocol".to_string(),
            message: "banto-hub では Hub 経由の接続（hub）は作成できません\
                      （Hub 経由の接続は記録計などの Hub を使う側の設定です）"
                .to_string(),
        }],
    })
}

/// S1（docs/banto-hub-external-db-design.md §4.1・§2.2）: GET/list が返す
/// `plc_connections` の読み取り DTO。パスワードは決して外部へ出さず、
/// [`Self::password_set`]（設定されているかの bool）と [`Self::password_state`]
/// （R2b-1d、読めるか）だけを返す。
///
/// ## 2 つの組み立て方（R2b-1d）
///
/// - 応答（REST・MCP）は [`PlcConnectionResponse::with_secret_rows`]。`passwordState`
///   （`none` / `set` / `unreadable`、設計 §5.5）を秘密の行を開いて決める。
/// - `From<PlcConnection>` は未適用キューの指紋（`crate::rest::compute_pending_base_fingerprint`
///   と適用時の照合）。**alpha.33 の応答と同じ形**（`passwordState` を持たない）にして、
///   版を上げる前に積んだ更新・削除の指紋が上げた後も一致するようにする（設計 §2・§5.3、
///   R2b-7 まで REST の形を保つ）。読めるかどうか（PC を移した）で指紋を変えない。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlcConnectionResponse {
    pub id: i64,
    pub name: String,
    pub protocol: String,
    pub host: String,
    pub port: i64,
    pub unit_id: i64,
    pub enabled: bool,
    pub simulation: bool,
    pub word_order: String,
    /// #605: `"Q"` または `"R"`。SLMP 以外の行も列は持つ（既定 `"R"`）。
    pub cpu_type: String,
    pub database: Option<String>,
    pub username: Option<String>,
    /// S1: true のとき、この接続にはパスワードが保存されている（値そのものは返さない）。
    /// R2b-1d から「`passwordState == "set"`」（読めないものは `false`）。
    pub password_set: bool,
    /// R2a-2e（docs/banto-hub-ingest-design.md §3.2）: `push` 接続の `settings`。
    /// **既定のままなら `null`**（R2b-1d: 0025 から列は `NOT NULL` で既定は `{}` だが、
    /// alpha.33 の応答は既定のままの push 接続に `null` を返していた。`{}` と `null` は同じ
    /// 意味（既定）なので、REST の形と指紋を保つために `null` に戻す。設計 §2）。他の
    /// プロトコルでは常に `null`。`push` の `settings` は秘密値を持たないのでそのまま返す。
    pub settings: Option<serde_json::Value>,
    /// R2b-1d（設計 §5.5）: `"none"`（無い）/ `"set"`（ある・読める）/ `"unreadable"`
    /// （参照はあるが行が無い・この PC で開けない）。応答だけに付く（指紋の形には
    /// 無い: `From<PlcConnection>` は `None` で、そのときは項目ごと出さない）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password_state: Option<&'static str>,
}

impl From<PlcConnection> for PlcConnectionResponse {
    /// 指紋の形（`passwordState` なし、`passwordSet` は参照の有無）。応答には
    /// [`PlcConnectionResponse::with_secret_rows`] を使う。
    fn from(conn: PlcConnection) -> Self {
        // R2b-1d: a password is "set" when `settings.password` holds a secret reference
        // (design §3.3) - the same bool alpha.33 derived from its plaintext column.
        let password_set = conn.secret_ref(banto_tags::PASSWORD_KEY).is_some();
        Self::build(conn, password_set, None)
    }
}

impl PlcConnectionResponse {
    /// 応答の形: `passwordState` を `rows`（その接続の秘密の行）を開いて決め、
    /// `passwordSet` を `passwordState == "set"` にする（設計 §5.5）。
    pub fn with_secret_rows(conn: PlcConnection, rows: &[banto_tags::StoredSecretRow]) -> Self {
        let state = crate::secrets::secret_state(
            &crate::secrets::ConnectionSecrets::platform_default(),
            &conn,
            banto_tags::PASSWORD_KEY,
            rows,
        );
        Self::build(
            conn,
            state == crate::secrets::SecretState::Set,
            Some(state.as_str()),
        )
    }

    fn build(
        conn: PlcConnection,
        password_set: bool,
        password_state: Option<&'static str>,
    ) -> Self {
        // The wire keeps R2a's / alpha.33's shape (design §2, Q4): `settings` is a push
        // row's settings object, `null` while it is still the defaults (`{}` since
        // migration 0025 made the column NOT NULL - semantically identical to alpha.33's
        // `NULL`), and `null` for every other protocol - never another protocol's
        // settings object (a postgres one holds the secret reference; the flat fields
        // above are its wire form until R2b-7).
        let settings = (conn.protocol == banto_tags::PUSH_PROTOCOL && !conn.settings.is_empty())
            .then(|| serde_json::Value::Object(conn.settings.clone()));
        Self {
            id: conn.id,
            name: conn.name,
            protocol: conn.protocol,
            host: conn.host,
            port: conn.port,
            unit_id: conn.unit_id,
            enabled: conn.enabled,
            simulation: conn.simulation,
            word_order: conn.word_order,
            cpu_type: conn.cpu_type.clone(),
            database: conn.database,
            username: conn.username,
            password_set,
            settings,
            password_state,
        }
    }
}

/// [`PlcConnectionResponse::with_secret_rows`] for one connection, reading its sealed
/// rows (R2b-1d).
pub(crate) async fn connection_response(
    service: &banto_tags::PlcConnectionService,
    conn: PlcConnection,
) -> Result<PlcConnectionResponse, BantoError> {
    let rows = service.secret_rows(conn.id).await?;
    Ok(PlcConnectionResponse::with_secret_rows(conn, &rows))
}

/// [`PlcConnectionResponse::with_secret_rows`] for every connection. The connections
/// and the sealed rows are read in one read transaction (PR #777 review P2: a password
/// update committing between two separate reads would show a healthy connection as
/// `unreadable`).
pub(crate) async fn connection_responses(
    service: &banto_tags::PlcConnectionService,
) -> Result<Vec<PlcConnectionResponse>, BantoError> {
    let (connections, rows) = service.list_with_secret_rows().await?;
    Ok(connections
        .into_iter()
        .map(|conn| {
            let own: Vec<banto_tags::StoredSecretRow> = rows
                .iter()
                .filter(|row| row.connection_id == conn.id)
                .cloned()
                .collect();
            PlcConnectionResponse::with_secret_rows(conn, &own)
        })
        .collect())
}

pub(super) async fn plc_connections_list(
    State(state): State<TagRegistryState>,
) -> Result<Json<Vec<PlcConnectionResponse>>, ApiError> {
    Ok(Json(connection_responses(&state.plc_connections).await?))
}

pub(super) async fn plc_connections_get(
    State(state): State<TagRegistryState>,
    Path(id): Path<i64>,
) -> Result<Json<PlcConnectionResponse>, ApiError> {
    // One read transaction for the row and its sealed rows (PR #777 review P2).
    let (conn, rows) = state.plc_connections.get_with_secret_rows(id).await?;
    Ok(Json(PlcConnectionResponse::with_secret_rows(conn, &rows)))
}

pub(super) async fn plc_connections_create(
    State(state): State<TagRegistryState>,
    headers: HeaderMap,
    Json(input): Json<PlcConnectionPayload>,
) -> RegistryMutationResult<Response> {
    require_editor(
        &state.auth,
        &state.audit,
        &headers,
        "plc_connections",
        "POST",
        "/api/plc-connections",
    )
    .await?;
    // #383 段階3 P2: キューへ積む前に拒否する（`reject_hub_connection_protocol`）。
    if let Err(err) = reject_hub_connection_protocol(&input) {
        return Err(ApiError(err).into());
    }
    if let Some(status) = registry_change_should_queue(&state.controller, &state.commissioning) {
        return queue_pending_registry_change(
            &state,
            &headers,
            "plc_connections.create",
            json!({ "input": input }),
            status,
        )
        .await;
    }
    // R2b-1d: the password is sealed here (`PlcConnectionPayload::into_input`).
    let input = input.into_input().map_err(ApiError)?;
    // 読み取りを挟む書き込みなので BEGIN IMMEDIATE（checklist §5 / sink/service.rs の doc）。
    let mut tx = state
        .manager
        .pool()
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(storage_api_error)?;
    // #635: 書き込みの前の registry。書き込みで新しく生まれた外部名の重なりだけを断る。
    let registry_before = match registry_before(&mut tx, state.manager.registry()).await {
        Ok(before) => before,
        Err(err) => {
            let _ = tx.rollback().await;
            return Err(err.into());
        }
    };
    let created = match state.plc_connections.create_tx(&mut tx, input).await {
        Ok(created) => created,
        Err(err) => {
            let _ = tx.rollback().await;
            return Err(ApiError(err).into());
        }
    };
    // #341 レビュー対応: この snapshot は**保存前検証（preflight）専用**。
    // catalog へ反映するのは `CollectionController::commit_catalog_and_apply_live`
    // が transition ロックの下で読み直す最新の snapshot（同 fn の doc
    // comment「古い snapshot の窓」節）。
    let _preflighted = match preflight_transaction(&mut tx, &registry_before).await {
        Ok(snapshot) => snapshot,
        Err(err) => {
            let _ = tx.rollback().await;
            return Err(err.into());
        }
    };
    tx.commit().await.map_err(storage_api_error)?;
    record_write(
        &state.audit,
        &state.auth,
        &headers,
        "create",
        "plc_connections",
        Some(&created.id.to_string()),
        Some(json!({ "name": created.name, "enabled": created.enabled })),
    )
    .await;
    commit_catalog_and_notify(&state.controller, &state.events, "plc_connections")
        .await
        .map_err(RegistryMutationError::LiveApplyFailed)?;
    let response = connection_response(&state.plc_connections, created)
        .await
        .map_err(ApiError)?;
    Ok(Json(response).into_response())
}

pub(super) async fn plc_connections_update(
    State(state): State<TagRegistryState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(input): Json<PlcConnectionPayload>,
) -> RegistryMutationResult<Response> {
    require_editor(
        &state.auth,
        &state.audit,
        &headers,
        "plc_connections",
        "PUT",
        "/api/plc-connections/{id}",
    )
    .await?;
    // #383 段階3 P2: キューへ積む前に拒否する（`reject_hub_connection_protocol`）。
    if let Err(err) = reject_hub_connection_protocol(&input) {
        return Err(ApiError(err).into());
    }
    if let Some(status) = registry_change_should_queue(&state.controller, &state.commissioning) {
        return queue_pending_registry_change(
            &state,
            &headers,
            "plc_connections.update",
            json!({ "id": id, "input": input }),
            status,
        )
        .await;
    }
    // R2b-1d: the password is sealed here (`PlcConnectionPayload::into_input`).
    let input = input.into_input().map_err(ApiError)?;
    // 読み取りを挟む書き込みなので BEGIN IMMEDIATE（checklist §5 / sink/service.rs の doc）。
    let mut tx = state
        .manager
        .pool()
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(storage_api_error)?;
    // #635: 書き込みの前の registry。書き込みで新しく生まれた外部名の重なりだけを断る。
    let registry_before = match registry_before(&mut tx, state.manager.registry()).await {
        Ok(before) => before,
        Err(err) => {
            let _ = tx.rollback().await;
            return Err(err.into());
        }
    };
    let updated = match state.plc_connections.update_tx(&mut tx, id, input).await {
        Ok(updated) => updated,
        Err(err) => {
            let _ = tx.rollback().await;
            return Err(ApiError(err).into());
        }
    };
    // #341 レビュー対応: この snapshot は**保存前検証（preflight）専用**。
    // catalog へ反映するのは `CollectionController::commit_catalog_and_apply_live`
    // が transition ロックの下で読み直す最新の snapshot（同 fn の doc
    // comment「古い snapshot の窓」節）。
    let _preflighted = match preflight_transaction(&mut tx, &registry_before).await {
        Ok(snapshot) => snapshot,
        Err(err) => {
            let _ = tx.rollback().await;
            return Err(err.into());
        }
    };
    tx.commit().await.map_err(storage_api_error)?;
    record_write(
        &state.audit,
        &state.auth,
        &headers,
        "update",
        "plc_connections",
        Some(&id.to_string()),
        Some(json!({ "name": updated.name, "enabled": updated.enabled })),
    )
    .await;
    commit_catalog_and_notify(&state.controller, &state.events, "plc_connections")
        .await
        .map_err(RegistryMutationError::LiveApplyFailed)?;
    let response = connection_response(&state.plc_connections, updated)
        .await
        .map_err(ApiError)?;
    Ok(Json(response).into_response())
}

pub(super) async fn plc_connections_delete(
    State(state): State<TagRegistryState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> RegistryMutationResult<Response> {
    require_editor(
        &state.auth,
        &state.audit,
        &headers,
        "plc_connections",
        "DELETE",
        "/api/plc-connections/{id}",
    )
    .await?;
    if let Some(status) = registry_change_should_queue(&state.controller, &state.commissioning) {
        return queue_pending_registry_change(
            &state,
            &headers,
            "plc_connections.delete",
            json!({ "id": id }),
            status,
        )
        .await;
    }
    // 読み取りを挟む書き込みなので BEGIN IMMEDIATE（checklist §5 / sink/service.rs の doc）。
    let mut tx = state
        .manager
        .pool()
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(storage_api_error)?;
    // #635: 書き込みの前の registry。書き込みで新しく生まれた外部名の重なりだけを断る。
    let registry_before = match registry_before(&mut tx, state.manager.registry()).await {
        Ok(before) => before,
        Err(err) => {
            let _ = tx.rollback().await;
            return Err(err.into());
        }
    };
    // 外部 DB 連携 S4（docs/banto-hub-external-db-design.md §5.2、
    // `crate::sink`のモジュール doc comment「FK を張らない理由」参照）:
    // この接続を参照する sink group が1件でもあれば削除を拒否する
    // （RESTRICT 相当）。`banto_tags`はこの接続の下にある Hub 専用
    // テーブルを知らないため、`cascade_delete_tx`を呼ぶ**前**にここで
    // チェックする。
    if let Err(err) = reject_delete_if_referenced_by_sink_group(&mut tx, id).await {
        let _ = tx.rollback().await;
        return Err(ApiError(err).into());
    }
    // T19 S2-b（UX-38、docs/banto-hub-t19-design.md §3.4・§7.5、2026-09-02
    // オーナー決定）: 「タグがあってもグループ・接続を削除可。定義のみ削除
    // し、履歴は残す」- `delete_tx`（子が居れば拒否）ではなく
    // `cascade_delete_tx`（配下のグループ・タグごと1トランザクションで削除、
    // `banto_tags::plc_connection::PlcConnectionService::cascade_delete_tx`
    // の doc comment参照）を使う。`banto-tstore` の履歴データはこのクレート
    // が触らない別ストレージなので無傷のまま残る。予約接続（calc/mem）は
    // このカスケード経路でも従来どおり削除できない。
    let cascade = match state.plc_connections.cascade_delete_tx(&mut tx, id).await {
        Ok(cascade) => cascade,
        Err(err) => {
            let _ = tx.rollback().await;
            return Err(ApiError(err).into());
        }
    };
    // #341 レビュー対応: この snapshot は**保存前検証（preflight）専用**。
    // catalog へ反映するのは `CollectionController::commit_catalog_and_apply_live`
    // が transition ロックの下で読み直す最新の snapshot（同 fn の doc
    // comment「古い snapshot の窓」節）。
    let _preflighted = match preflight_transaction(&mut tx, &registry_before).await {
        Ok(snapshot) => snapshot,
        Err(err) => {
            let _ = tx.rollback().await;
            return Err(err.into());
        }
    };
    tx.commit().await.map_err(storage_api_error)?;
    record_write(
        &state.audit,
        &state.auth,
        &headers,
        "delete",
        "plc_connections",
        Some(&id.to_string()),
        Some(json!({
            "cascade": {
                "deletedGroups": cascade.deleted_groups,
                "deletedTags": cascade.deleted_tags,
            },
        })),
    )
    .await;
    commit_catalog_and_notify(&state.controller, &state.events, "plc_connections")
        .await
        .map_err(RegistryMutationError::LiveApplyFailed)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// `POST /api/plc-connections/{id}/test` - S1a
/// （docs/banto-hub-external-db-design.md §4.6・§7 スライス表「S1」:
/// 「接続テストが `SELECT 1`〈実際は `SELECT version()`〉と列一覧を
/// 返す」の PostgreSQL 側実装。**保存済み**の接続 `id` を対象にする点で
/// T12 の `POST /api/plc-connections/test`（保存前のフォーム値を直接
/// 受け取る、PLC 専用）とは別物 - 名前を分けて共存させている
/// (`plc_connections_test`が既存、こちらは`plc_connections_test_saved`)。
///
/// 認可は他の `plc_connections` 書き込みハンドラ（create/update/delete）と
/// 同じ`require_editor`に揃える - このリソースの REST 面は既に role
/// ベースの editor ゲートで統一されており（T21 の MCP 側だけが admin
/// スコープを使う別体系）、ここだけ新しいゲートを持ち込まない。
///
/// S1a では `protocol == "postgres"` の接続のみ対応する
/// （[`banto_tags::PlcConnection::is_db_source`]）。それ以外は`400`で
/// 「S1では postgres のみ対応」と明示する - `ok:false`の200ではなく
/// 明確なリクエストエラーにする（T12の`test_connection`が`virtual`/
/// `simulation`を`ok:false`の200で表現するのとは意図的に区別: あちらは
/// 「テスト対象として意味を成さないが正常な入力」、こちらは「この
/// エンドポイントがそもそも対応しないプロトコル」）。
///
/// レジストリへの書き込みは一切発生しない読み取り専用の疎通確認なので、
/// `record_write`/`commit_catalog_and_notify`は呼ばない -
/// `plc_connections_test`（T12）と同じ判断。
pub(super) async fn plc_connections_test_saved(
    State(state): State<TagRegistryState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<Response, ApiError> {
    require_editor(
        &state.auth,
        &state.audit,
        &headers,
        "plc_connections",
        "POST",
        "/api/plc-connections/{id}/test",
    )
    .await?;

    // One read transaction for the row and its sealed rows (PR #777 review P2).
    let (conn, secret_rows) = state.plc_connections.get_with_secret_rows(id).await?;
    if !conn.is_db_source() {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(json!({
                "message": "S1ではpostgres接続のみ接続テストに対応しています。",
            })),
        )
            .into_response());
    }

    let outcome = crate::db_source::test_connection(&conn, &secret_rows).await;
    Ok((StatusCode::OK, Json(outcome)).into_response())
}

// --- T12 (docs/ux-plan.md §4): 保存前の接続テスト ---------------------------
//
// `POST /api/plc-connections/test` は保存不要でホスト/ポート/プロトコルの
// 疎通を確認する - TCP 接続だけでなく実プロトコルで軽い読み出し1回
// (先頭デバイス1点)まで行うことで、ポートは開いているがプロトコル不一致、
// という誤設定も検出する(§4 の設計方針)。レジストリへの書き込みが一切
// 発生しない読み取り専用の疎通確認なので、`record_write`/`rebuild_and_notify`
// は呼ばない。
//
// 重要な制約(実機 R08ENCPU・オムロン KM-D1-ETN、`crates/banto-broker/src/lib.rs`
// のモジュール doc、`crate::broker_glue`のモジュール doc「Session sync
// policy」節参照): 三菱 SLMP は対象ポートが既に別の接続で使用中だと同じ
// ポートへの2本目を受け付けない(2026-08-07 実機確認: ポート毎に1接続、CPU
// 側で複数ポートを開けていれば複数同時セッションは可能)。Modbus TCP も
// #337 で収集読み取りが broker セッションに相乗りするようになった結果、
// KM-D1-ETN(KANC-718B 12-1)のような1対1接続しか受け付けない機器では同じ
// 制約が生じる(2026-09-08 オーナー決定)。そのため、保存済み接続
// (`connectionId`あり)のテストはプロトコルを問わず、既存の broker
// セッションが生きていればそれを再利用して読み、無い場合のみ直接ダイヤル
// する([`test_connection`]参照)。
//
// R2b-3a-2(docs/banto-hub-driver-io-design.md §4.2・Q11): プロトコル別の
// `test_modbus_connection`/`test_slmp_connection`(`ModbusTcpClient`/
// `SlmpClient` を直接ダイヤルし、固定のリテラル `40001`/`D0` を読む)を
// ドライバに依らない [`test_connection`] にまとめた。読む 1 点はドライバの
// `DriverBase::probe()`、ダイヤルはbroker が解決した Session / Frame ドライバの
// `connect`、収集中のセッションを使ってよいかは設定の指紋で見る
// (#605 の CPU 種別の照合を、どの設定にも広げたもの)。

/// 接続テストの疎通確認に使うタイムアウト(接続・応答とも共通)。数秒固定
/// (ux-plan.md §4「タイムアウトは短め（数秒）に固定」)。
const PLC_TEST_TIMEOUT: Duration = Duration::from_secs(3);

/// 直接ダイヤル失敗時(`tcp`・`timeout`)に付けるセッション上限ヒント。
/// R2b-3a-2 でプロトコル別の 2 つ(Modbus・SLMP)を 1 つにまとめた: 実機
/// R08ENCPU は対象ポートが既に使用中だと 2 本目を受け付けず、KM-D1-ETN のような
/// 1 対 1 の機器も同じ(このファイルの上の「重要な制約」)。
const SESSION_HINT: &str = " 対象機器のポートが既に別の接続(この hub の収集や他アプリ)で使用中の可能性があります。機器によっては同じポートへの2本目の接続を受け付けません(SLMP の実機 R08ENCPU・Modbus の KM-D1-ETN で確認済み)。";

/// `POST /api/plc-connections/test` のリクエストボディ - 保存前のフォーム値を
/// そのまま受け取る(`PlcConnectionPayload`とは別型: 接続 id を持たないのが
/// 通常で、保存済み接続の編集中のみ`connectionId`を添える)。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlcConnectionTestPayload {
    pub protocol: String,
    pub host: String,
    pub port: i64,
    #[serde(default = "default_plc_unit_id")]
    pub unit_id: i64,
    /// フォームの「シミュレーションモード」チェックボックスの現在値。
    /// シミュレーション接続のテストは常に内蔵シミュレータへ繋がり無意味なので
    /// 拒否する(ux-plan.md §4「protocol: virtual と simulation: true 相当の
    /// テストは明示エラー」)。保存済み行の simulation フラグではなく、
    /// フォームの現在値をそのまま送らせる設計(未保存の編集中でも即座に弾ける
    /// ようにするため)。
    #[serde(default)]
    pub simulation: bool,
    /// 保存済み接続を編集中にテストする場合のみ送る。broker 経由の既存
    /// セッション判定にのみ使う(接続情報自体は上記 host/port/protocol/unitId
    /// を毎回使う - フォームの現在入力値をテストするのがこの API の目的の
    /// ため)。
    #[serde(default)]
    pub connection_id: Option<i64>,
    /// #605: 直接ダイヤルの SLMP フレームに使う CPU 種別。省略は保存済みの値
    /// (無ければドライバの既定 = iQ-R)、`"Q"` は Q シリーズ。未知の値は
    /// ドライバの `validate_config` が断り、ダイヤルしない。既存セッションを
    /// 再利用する経路で、保存済みセッションの設定と食い違うときは試さずに
    /// 「保存してからテストしてください」と返す（2 本目の TCP を張らない既存の
    /// 制約があるため、直接ダイヤルには切り替えない。R2b-3a-2 から CPU に限らず
    /// 設定の指紋で比べる）。
    #[serde(default)]
    pub cpu_type: Option<String>,
}

/// `POST /api/plc-connections/test` の応答。`ToSchema` は付けない - 下記
/// `plc_connections_test`のdoc comment参照。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlcConnectionTestResponse {
    pub ok: bool,
    pub elapsed_ms: u64,
    pub error: Option<PlcConnectionTestError>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlcConnectionTestError {
    /// "tcp" | "timeout" | "protocol" | "device" | "unsupported"
    pub kind: String,
    /// 日本語、対処ヒント込み。
    pub message: String,
}

fn test_error(kind: &str, message: impl Into<String>) -> PlcConnectionTestError {
    PlcConnectionTestError {
        kind: kind.to_string(),
        message: message.into(),
    }
}

/// 接続の段で起きた致命のエラー([`FatalError`])を [`PlcConnectionTestError`] に
/// 分類する(R2b-3a-2: プロトコル別の `PlcError` の分類から置き換えた)。
/// `connecting` は接続の段か(時間切れの文言を分ける)。`hint` が `Some` の
/// とき、`kind` が `"tcp"`・`"timeout"` のときだけ末尾に付ける - 呼び出し側は
/// 「セッション上限ヒントを付けたい経路」(直接ダイヤル)でだけ `Some` を渡す。
/// 既存セッションの再利用の経路は「2 本目をダイヤルしない」こと自体が対策
/// なので常に `None`。
fn classify_fatal(
    err: &FatalError,
    connecting: bool,
    hint: Option<&str>,
) -> PlcConnectionTestError {
    let (kind, mut message) = match err {
        FatalError::Timeout if connecting => (
            "timeout",
            "接続タイムアウトです。ホスト/ポート、ネットワーク到達性とハンドシェイクを確認してください。"
                .to_string(),
        ),
        FatalError::Timeout => (
            "timeout",
            "応答タイムアウトです(3秒)。接続はできましたが応答がありませんでした。".to_string(),
        ),
        FatalError::Io(msg) => (
            "tcp",
            format!("TCP接続に失敗しました(ポートが閉じている、または到達できません): {msg}"),
        ),
        FatalError::Framing(msg) | FatalError::Protocol(msg) => (
            "protocol",
            format!(
                "プロトコルエラー: 応答が不正です({msg})。プロトコル選択やポート番号が実機と一致しているか確認してください。"
            ),
        ),
        FatalError::InvalidConfig(msg) => ("protocol", format!("接続設定が不正です: {msg}")),
        other => ("protocol", other.to_string()),
    };
    if matches!(kind, "tcp" | "timeout") {
        if let Some(hint) = hint {
            message.push_str(hint);
        }
    }
    test_error(kind, message)
}

/// 読む 1 点のタグごとのエラー([`ItemError`])の分類。接続はできている。
fn classify_item(err: &ItemError) -> PlcConnectionTestError {
    match err {
        ItemError::Device(device) => test_error(
            "device",
            format!(
                "デバイス読み出しエラー: 接続はできましたが、指定したデバイス/レジスタを読み出せませんでした({device})。アドレス設定や機種依存の可能性があります(致命的ではありません)。"
            ),
        ),
        other => test_error("device", format!("読み出しに失敗しました: {other}")),
    }
}

/// 試す設定(R2b-3a-2): フォームの値を接続の `settings` の形にしたもの
/// (`settings_from_flat` → `settings_to_api`、保存と同じ写し)。フォームが
/// 送らない欄(`wordOrder`、省略した `cpuType`)は、保存済みの接続(`connectionId`
/// があり、同じプロトコル)の値を使う - 「省略は保存済みのまま」(#605 の CPU の
/// 照合の規則を全部の欄に広げたもの)。保存済みが無ければその欄は置かない
/// (ドライバの既定)。保存済みの接続も返す(指紋に入る秘密値の参照の出どころ)。
pub(super) async fn requested_settings(
    manager: &CollectorManager,
    payload: &PlcConnectionTestPayload,
) -> (Settings, Option<PlcConnection>) {
    let saved = match payload.connection_id {
        Some(id) => manager
            .plc_connections()
            .get(id)
            .await
            .ok()
            .filter(|conn| conn.protocol == payload.protocol),
        None => None,
    };
    let cpu_type = payload
        .cpu_type
        .clone()
        .filter(|cpu| !cpu.is_empty())
        .or_else(|| saved.as_ref().map(|conn| conn.cpu_type.clone()));
    let input = PlcConnectionInput {
        name: String::new(),
        protocol: payload.protocol.clone(),
        host: payload.host.clone(),
        port: payload.port,
        unit_id: payload.unit_id,
        enabled: true,
        simulation: false,
        word_order: saved
            .as_ref()
            .map(|conn| conn.word_order.clone())
            .unwrap_or_default(),
        cpu_type,
        database: None,
        username: None,
        secrets: Default::default(),
        settings: None,
    };
    let mut map = banto_tags::settings_json::legacy::settings_from_flat(&input);
    if map
        .get(keys::WORD_ORDER)
        .and_then(|v| v.as_str())
        .is_some_and(str::is_empty)
    {
        map.remove(keys::WORD_ORDER);
    }
    (banto_tags::settings_to_api(&map), saved)
}

/// ドライバの `validate_config` が断った設定を、ダイヤルせずに返す。接続先の
/// 欄(`host`・`port`・`unit_id`)は従来の直接ダイヤルの範囲検査と同じ
/// `"tcp"`、それ以外(CPU 種別など)は `"protocol"`。
fn refused_settings(errors: &[banto_driver_api::FieldError]) -> PlcConnectionTestError {
    let kind = if errors
        .iter()
        .all(|e| matches!(e.key.as_str(), keys::HOST | keys::PORT | keys::UNIT_ID))
    {
        "tcp"
    } else {
        "protocol"
    };
    let detail = errors
        .iter()
        .map(|e| format!("{}: {}", e.key, e.message))
        .collect::<Vec<_>>()
        .join("、");
    test_error(kind, format!("接続設定が不正です({detail})。"))
}

/// ドライバが名乗る読む 1 点([`banto_driver_api::DriverBase::probe`])を、
/// そのドライバの文法で [`TagSpec`] にする。`None` は「読まずに接続だけ確かめる」。
fn probe_spec(
    base: &dyn banto_driver_api::DriverBase,
) -> Result<Option<TagSpec>, PlcConnectionTestError> {
    let Some((text, kind)) = base.probe() else {
        return Ok(None);
    };
    let address = base.parse_address(&text, &kind).map_err(|err| {
        test_error(
            "protocol",
            format!("ドライバの接続テストの読み取り点 {text:?} を解釈できません: {err}"),
        )
    })?;
    Ok(Some(TagSpec::new(address, kind)))
}

/// 読む 1 点の結果(1 件)を接続テストの結果にする。
fn probe_outcome(results: Vec<Result<Value, ItemError>>) -> (bool, Option<PlcConnectionTestError>) {
    match results.into_iter().next() {
        Some(Ok(_)) => (true, None),
        Some(Err(err)) => (false, Some(classify_item(&err))),
        None => (
            false,
            Some(test_error("device", "読み出し結果が空でした。")),
        ),
    }
}

/// 接続テストの本体(R2b-3a-2、docs/banto-hub-driver-io-design.md Q11)。
/// プロトコルに依らない:
///
/// 1. 試す設定を作る([`requested_settings`])。
/// 2. `payload.connection_id` があり、その接続の broker セッションが既に
///    生きていれば、それを再利用して読む(新規ダイヤルしない - 実機 R08ENCPU・
///    KM-D1-ETN は対象ポートが既に使用中だと 2 本目を受け付けないため、これを
///    誤診しないための対策。このファイルの上のコメント参照)。ただし、
///    セッションの設定の指紋([`banto_broker::BrokerHandle::settings_fingerprint`]、
///    いま読む handle 自身が作成時に持っていたもの)が試す設定の指紋と違えば、
///    セッションの読みはフォームの値を試したことにならないので、試さずに
///    「保存してからテストしてください」と返す(#605 の CPU 種別の照合を
///    全部の設定に広げたもの)。
/// 3. 無ければ直接ダイヤルする: broker と共通の Session / Frame ドライバで
///    `validate_config` → `connect`(接続・各応答の時間切れは
///    [`PLC_TEST_TIMEOUT`]、外側は broker と同じ種類別の上限)→ ドライバの読む 1 点を
///    `read_batch`(外側の上限は broker と同じ式)→ 必ず `disconnect`。
async fn test_connection(
    manager: &CollectorManager,
    payload: &PlcConnectionTestPayload,
) -> (bool, Option<PlcConnectionTestError>) {
    let registry = manager.registry();
    let Some(resolved) = banto_broker::resolve_session_driver(registry, &payload.protocol) else {
        return (
            false,
            Some(test_error(
                "unsupported",
                format!("不明なプロトコルです: {}", payload.protocol),
            )),
        );
    };
    let driver = resolved.driver();
    // 保存済みの接続（同じプロトコル）は、指紋に入る秘密値の参照の出どころ
    // （フォームは秘密値を送らない。参照は保存済みのまま、と読む）。
    let (settings, saved) = requested_settings(manager, payload).await;
    let probe = match probe_spec(driver.as_ref()) {
        Ok(probe) => probe,
        Err(err) => return (false, Some(err)),
    };

    if let Some(connection_id) = payload.connection_id {
        if let Some(handle) = manager.sessions().handle_for(connection_id) {
            let wanted =
                manager
                    .sessions()
                    .dial_fingerprint(&payload.protocol, saved.as_ref(), &settings);
            if wanted.is_none() || handle.settings_fingerprint() != wanted {
                return (
                    false,
                    Some(test_error(
                        "protocol",
                        "この接続は収集中で、保存済みの設定のセッションを使っています。フォームの設定（接続先・CPU 種別など）は、保存してからテストしてください。",
                    )),
                );
            }
            let Some(spec) = probe else {
                // 読む点を持たないドライバ: セッションが張れていれば成功。
                return match *handle.status_watch().borrow() {
                    BrokerConnectionStatus::Connected => (true, None),
                    _ => (false, Some(test_error("tcp", SHARED_SESSION_DOWN))),
                };
            };
            // `ReadOnlyHandle::read` は broker がジョブに上限を掛ける
            // (design §5.5)が、テストは従来どおり数秒で切る。
            return match tokio::time::timeout(PLC_TEST_TIMEOUT, handle.read(vec![spec])).await {
                Err(_elapsed) => (
                    false,
                    Some(test_error(
                        "timeout",
                        "応答タイムアウトです(3秒)。共有セッションが応答しませんでした。",
                    )),
                ),
                Ok(Err(BrokerError::Disconnected { .. })) => {
                    (false, Some(test_error("tcp", SHARED_SESSION_DOWN)))
                }
                Ok(Err(BrokerError::ConnectionFailed { reason, .. })) => (
                    false,
                    Some(test_error(
                        "tcp",
                        format!("共有セッションが接続断で失敗しました: {reason}"),
                    )),
                ),
                Ok(Err(BrokerError::TaskGone { .. })) => (
                    false,
                    Some(test_error(
                        "tcp",
                        "内部エラー: セッションタスクが終了しています。",
                    )),
                ),
                // 防御的フォールバック: この経路では通常発生しない
                // (UnsupportedProtocol/InvalidSettings は ensure_connection
                // 時点で弾かれているはず)。
                Ok(Err(err)) => (false, Some(test_error("protocol", err.to_string()))),
                // 既存セッション再利用経路: セッション上限ヒントは付けない
                // (design の意図: 「2本目をダイヤルしない」こと自体が対策)。
                Ok(Ok(results)) => probe_outcome(results),
            };
        }
    }

    // 直接ダイヤル(connectionId が無い、またはセッションが見つからない場合)。
    let refused = driver.validate_config(&settings);
    if !refused.is_empty() {
        return (false, Some(refused_settings(&refused)));
    }
    // シリアルの線は、致命の後もポートを開いたまま線の表へ戻る（io-design §7.3）。broker の
    // task の外の 1 回きりのダイヤルなので、失敗・時間切れ・要求の取り消し（future の drop）の
    // どの道でも、この番人の drop で自分が借りた線を閉じる。`session` より先に作るので、
    // 貸し出しが表へ戻った後に閉じる。成功の道の `disconnect` の後に閉じても害はない。
    let _line = resolved.one_shot_line(&settings);
    let cx = ConnectContext::new("connection-test", PLC_TEST_TIMEOUT, PLC_TEST_TIMEOUT);
    let connected = tokio::time::timeout(
        resolved.connect_limit(banto_broker::SessionTimeouts {
            connect: PLC_TEST_TIMEOUT,
            response: PLC_TEST_TIMEOUT,
        }),
        driver.connect(&settings, cx),
    )
    .await
    .unwrap_or(Err(FatalError::Timeout));
    let mut session = match connected {
        Ok(session) => session,
        Err(err) => return (false, Some(classify_fatal(&err, true, Some(SESSION_HINT)))),
    };
    let outcome = match probe {
        None => (true, None),
        Some(spec) => {
            let specs = [spec];
            let read = tokio::time::timeout(
                banto_broker::read_job_limit(specs.len(), PLC_TEST_TIMEOUT),
                session.read_batch(&specs),
            )
            .await
            .unwrap_or(Err(FatalError::Timeout));
            match read {
                Ok(results) => probe_outcome(results),
                Err(err) => (false, Some(classify_fatal(&err, false, Some(SESSION_HINT)))),
            }
        }
    };
    let _ = tokio::time::timeout(banto_broker::DISCONNECT_LIMIT, session.disconnect()).await;
    outcome
}

/// 共有セッションが切断中のときの文言。
const SHARED_SESSION_DOWN: &str = "この接続の共有セッションは現在切断中です(再接続待機中)。PLCの電源やネットワーク、または他アプリとのセッション競合を確認してください。";

/// `POST /api/plc-connections/test` - T12(docs/ux-plan.md §4)。保存前に
/// host/port/protocol の疎通確認(実プロトコルでの軽い読み出し1回まで)を
/// 行う。**意図的に** `#[utoipa::path]`を付けず`ApiDoc::paths(...)`にも
/// 加えない - utoipa がドキュメント対象にしているのは`/api/v1/*`の機械
/// クライアント向け API のみで(このファイル冒頭「二系統に分かれた
/// ルーター」節参照)、他の`/api/plc-connections/*`管理ハンドラ
/// (create/update/delete)も同様に対象外にしている。ここだけドキュメント
/// 対象を広げると既存方針と矛盾するため、意図的に対象外のままにする。
///
/// 権限は他の I1 書き込みハンドラと同じ`require_editor`。CSRF は
/// `tag_registry_router`が属する管理系ルーター全体に既に`require_banto_client_header`
/// がかかっているので、ここでの追加対応は不要。
///
/// 失敗(`ok: false`)は通常の 200 応答として返す(`tags_batch`と同じ
/// 「ok:false は通常の応答」という判断) - レジストリへの書き込みが一切
/// 発生しない読み取り専用の疎通確認なので、監査ログ記録(`record_write`)や
/// `rebuild_and_notify`は呼ばない。
///
/// 疎通確認そのもの(virtual/simulation の即時拒否・ドライバによるダイヤル・
/// 所要時間計測)は[`run_plc_connection_test`]へ切り出してある - T21
/// S1-c（docs/banto-hub-t21-design.md）の MCP `test_connection`ツール
/// ([`crate::mcp`]の`tool_test_connection`)がこの REST ハンドラと**全く同じ
/// 疎通確認**を行うために共有する(二重実装しない、このファイル冒頭
/// 「§3.7」節と同じ規律)。
pub(super) async fn plc_connections_test(
    State(state): State<TagRegistryState>,
    headers: HeaderMap,
    Json(payload): Json<PlcConnectionTestPayload>,
) -> Result<Json<PlcConnectionTestResponse>, ApiError> {
    require_editor(
        &state.auth,
        &state.audit,
        &headers,
        "plc_connections",
        "POST",
        "/api/plc-connections/test",
    )
    .await?;

    Ok(Json(
        run_plc_connection_test(&state.manager, &payload).await,
    ))
}

/// [`plc_connections_test`]のdoc comment参照 - REST ハンドラと MCP の
/// `test_connection`ツールが共有する疎通確認本体。認可判定
/// (`require_editor`/MCP 側の`require_admin_scope`)は呼び出し元の責務で、
/// ここでは行わない。
pub(crate) async fn run_plc_connection_test(
    manager: &CollectorManager,
    payload: &PlcConnectionTestPayload,
) -> PlcConnectionTestResponse {
    let started = std::time::Instant::now();

    // ガード: virtual / simulation / 未知プロトコルは即座に ok:false, kind:
    // "unsupported"(実プロトコルの疎通確認より前に判定。未知プロトコルは
    // `test_connection` の最初)。
    let (ok, error) = if payload.protocol == "virtual" {
        (
            false,
            Some(PlcConnectionTestError {
                kind: "unsupported".to_string(),
                message: "virtual接続はテスト対象外です(calc/mem予約接続)。".to_string(),
            }),
        )
    } else if payload.simulation {
        (
            false,
            Some(PlcConnectionTestError {
                kind: "unsupported".to_string(),
                message: "シミュレーション接続はテスト不要です(常に内蔵シミュレータに接続され、常に成功します)。"
                    .to_string(),
            }),
        )
    } else {
        // R2b-3a-2: プロトコル名で分けない。broker が管理しないプロトコル
        // (レジストリに Session / Frame ドライバが無い)は `test_connection` が
        // "unsupported" で返す。
        test_connection(manager, payload).await
    };

    PlcConnectionTestResponse {
        ok,
        elapsed_ms: started.elapsed().as_millis() as u64,
        error,
    }
}

#[cfg(test)]
mod frame_connection_tests {
    use super::*;
    use crate::{
        broker_glue::{BrokerSimRegistry, HubDrivers, HubSessions},
        computed::{ComputedEngine, ServerTagStore},
    };
    use banto_driver_api::{
        AddressError, DataType, DriverAddress, DriverBase, DriverDescriptor, DriverEntry, DriverId,
        FieldError, FrameConnection, FrameDriver, TagKind,
    };
    use banto_driver_conformance::fake::echo::{EchoFrameDriver, EchoResponder};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    // The legacy connection-test payload supplies host/port for modbus-tcp.
    // Override only the fixture id; production registrations remain unchanged.
    struct ProbeFrameDriver {
        descriptor: DriverDescriptor,
        echo: EchoFrameDriver,
        creates: Arc<AtomicUsize>,
    }
    impl DriverBase for ProbeFrameDriver {
        fn descriptor(&self) -> &DriverDescriptor {
            &self.descriptor
        }
        fn validate_config(&self, settings: &Settings) -> Vec<FieldError> {
            self.echo.validate_config(settings)
        }
        fn parse_address(&self, text: &str, kind: &TagKind) -> Result<DriverAddress, AddressError> {
            let mut address = self.echo.parse_address(text, kind)?;
            address.driver = self.descriptor.id.clone();
            Ok(address)
        }
        fn offset_address(
            &self,
            text: &str,
            kind: &TagKind,
            n: i64,
        ) -> Result<String, AddressError> {
            self.echo.offset_address(text, kind, n)
        }
        fn address_help(&self) -> String {
            self.echo.address_help()
        }
        fn probe(&self) -> Option<(String, TagKind)> {
            Some(("W10".into(), TagKind::numeric(DataType::U16)))
        }
    }
    impl FrameDriver for ProbeFrameDriver {
        fn create_connection(
            &self,
            settings: &Settings,
        ) -> Result<Box<dyn FrameConnection>, FatalError> {
            self.creates.fetch_add(1, Ordering::SeqCst);
            self.echo.create_connection(settings)
        }
    }
    async fn fixture() -> (
        CollectorManager,
        Arc<HubSessions>,
        sqlx::SqlitePool,
        Arc<AtomicUsize>,
    ) {
        let pool = crate::db::init_db_memory().await.unwrap();
        let creates = Arc::new(AtomicUsize::new(0));
        let echo = EchoFrameDriver::new();
        let mut descriptor = echo.descriptor().clone();
        descriptor.id = DriverId::new("modbus-tcp").unwrap();
        let mut drivers = HubDrivers::none();
        Arc::get_mut(&mut drivers.registry)
            .unwrap()
            .register(DriverEntry::frame(Arc::new(ProbeFrameDriver {
                descriptor,
                echo,
                creates: creates.clone(),
            })))
            .unwrap();
        let sessions = Arc::new(HubSessions::new(
            banto_broker::BackoffConfig::default(),
            drivers,
        ));
        let manager = CollectorManager::new(
            pool.clone(),
            std::env::temp_dir(),
            Arc::new(banto_tstore::SystemClock),
            banto_collect::CollectorOptions::default(),
            sessions.clone(),
            Arc::new(BrokerSimRegistry::new()),
            Arc::new(ComputedEngine::new(Arc::new(ServerTagStore::new()))),
        );
        (manager, sessions, pool, creates)
    }
    fn payload(port: i64) -> PlcConnectionTestPayload {
        PlcConnectionTestPayload {
            protocol: "modbus-tcp".into(),
            host: "127.0.0.1".into(),
            port,
            unit_id: 1,
            simulation: false,
            connection_id: None,
            cpu_type: None,
        }
    }

    #[tokio::test]
    async fn frame_connection_test_refuses_invalid_settings_without_io() {
        let (manager, sessions, pool, creates) = fixture().await;
        assert!(sessions.is_broker_managed("modbus-tcp"));
        let response = run_plc_connection_test(&manager, &payload(0)).await;
        assert!(!response.ok);
        let error = response.error.unwrap();
        assert_eq!(
            error.kind, "tcp",
            "Frame must reach validation, not unknown protocol"
        );
        assert!(error.message.contains("port"), "{error:?}");
        assert_eq!(creates.load(Ordering::SeqCst), 0);
        manager.shutdown().await;
        sessions.shutdown().await;
        pool.close().await;
    }

    #[tokio::test]
    async fn frame_connection_test_completes_handshake_probe_and_disconnect_over_tcp() {
        use banto_driver_conformance::frame::{Reply, Responder};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let peer = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut echo = EchoResponder::new();
            echo.set_word('W', 10, 7);
            echo.on_connect();
            let mut codes = vec![];
            loop {
                let mut header = [0; 6];
                match stream.read_exact(&mut header).await {
                    Ok(_) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => break,
                    Err(error) => panic!("read request: {error}"),
                }
                let mut request = header.to_vec();
                request.resize(
                    6 + usize::from(u16::from_be_bytes([header[4], header[5]])),
                    0,
                );
                stream.read_exact(&mut request[6..]).await.unwrap();
                codes.push(header[1]);
                let Reply::Bytes(reply) = echo.reply(&request) else {
                    panic!("echo reply");
                };
                stream.write_all(&reply).await.unwrap();
            }
            codes
        });
        let (manager, sessions, pool, creates) = fixture().await;
        let response = tokio::time::timeout(
            Duration::from_secs(5),
            run_plc_connection_test(&manager, &payload(i64::from(port))),
        )
        .await
        .unwrap();
        assert!(response.ok, "{response:?}");
        assert_eq!(creates.load(Ordering::SeqCst), 1);
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), peer)
                .await
                .unwrap()
                .unwrap(),
            b"HR"
        );
        assert!(
            sessions.handle_for(1).is_none(),
            "direct test must not create a broker task"
        );
        manager.shutdown().await;
        sessions.shutdown().await;
        pool.close().await;
    }
}
