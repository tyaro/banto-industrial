//! #383 段階3 P6「実機確認（Hub 経由の SLMP と直結 SLMP を別ポートで比較）」
//! のハーネス（docs/plan.md §5）。
//!
//! ChronoGazer の Hub 経由のタグ収集（P3〜P5: `crate::hub`・`crate::collect`・
//! `banto_collect::external`）が、**実 PLC を相手に、直結 SLMP の収集と同じ値を
//! 記録する**ことを確かめる。GUI は通さず、デスクトップ（`src-tauri`）と同じ
//! サービス層（[`HubService`]・[`CollectorService`]・[`register_from_hub`]）を
//! 直接組み立てる（前例: `hub_real_smoke.rs`）。**製品コード（`src/`）は
//! 変更しない。**
//!
//! ## 構成
//!
//! ```text
//!   PLC ──SLMP(P6_HUB_PLC_PORT)──> banto-hub（このハーネスが起動する使い捨て）
//!    │                                 │ REST + WS（127.0.0.1:P6_HUB_PORT）
//!    │                                 v
//!    └──SLMP(P6_DIRECT_PLC_PORT)──> ChronoGazer（同じプロセス内のサービス層）
//!                                      grp:直結  ← 直結 SLMP のタグ
//!                                      grp:Hub   ← 同じアドレスの Hub 経由のタグ
//! ```
//!
//! * banto-hub は**このハーネスが子プロセスとして起動し、終わりに止める**
//!   （一時 DB・データは `P6_WORK_DIR` の下）。素の banto-hub は試運転モード
//!   （未ロックダウン）で起動するので、**管理者アカウントは作らない**:
//!   構成（接続・グループ・タグ・収集の開始）は loopback の
//!   `POST /api/auth/grant/commissioning`（資格情報なしの合成 admin の grant。
//!   `apps/banto-hub/core/src/commissioning.rs`）で行い、ChronoGazer の API
//!   キーは [`HubService::connect`] が試運転中の Hub に自己発行させる
//!   （デスクトップの「Hub 接続」と同じ経路）。
//! * **PLC へは書き込まない**。Hub のタグは `writable: false`、ChronoGazer の
//!   タグも書き込み不可。ハーネスは書き込みの API を一切呼ばない。
//! * 同じアドレスを 2 つのタグ（直結と Hub 経由）で読み、データファイル
//!   （tstore）と現在値を**組ごとに**突き合わせる。
//!
//! ## 実行方法
//!
//! ```text
//! cargo build -p banto-hub-core --bin banto-hub
//! PLC_HOST=<PLC の IP> P6_WORK_DIR=<一時ディレクトリ> \
//!   cargo run --example hub_p6_compare -p chronogazer-core
//! ```
//!
//! | 変数 | 既定 | 意味 |
//! | --- | --- | --- |
//! | `PLC_HOST` | （必須） | PLC の IP。**リポジトリに書かない** |
//! | `P6_HUB_PLC_PORT` | `3101` | Hub が PLC へつなぐ SLMP のポート |
//! | `P6_DIRECT_PLC_PORT` | `3102` | ChronoGazer の直結 SLMP のポート |
//! | `P6_HUB_BIN` | `<workspace>/target/debug/banto-hub.exe` | 起動する banto-hub |
//! | `P6_HUB_PORT` | `8731` | 一時 Hub の REST/WS ポート（127.0.0.1） |
//! | `P6_WORK_DIR` | OS の一時ディレクトリ配下 | 一時 Hub の DB・データ・ログ、ChronoGazer の DB・データ |
//! | `P6_RUN_SECS` | `60` | 定常運転の秒数 |
//! | `P6_PERIOD_MS` | `1000` | 収集周期（Hub・直結とも同じ） |
//! | `P6_OUTAGE` | 空 | `process`（Hub のプロセスを止めて起動し直す。Hub の接続キーに `plc_disconnected` → `plc_reconnected` を期待）/ `collection`（Hub の収集を stop/start。リンクは Live のままなのでイベントは出ないことを期待）/ 空（行わない） |
//! | `P6_OUTAGE_SECS` | `15` | Hub を止めておく秒数 |
//! | `P6_RECOVER_SECS` | `20` | 再開後に観測を続ける秒数 |
//!
//! 出力の作法は `hub_real_smoke.rs` と同じ: 番号付きの段階見出し、途中で
//! panic しない、最後に PASS/FAIL 表、前段の失敗で試せなかった段は SKIP。
//! 終了コードは全段が成功（または任意段の省略）なら 0、それ以外は 1。
//!
//! ## 発行した API キーの後始末
//!
//! 手順の最後に、**このインストールが自己発行した `key_id`（`hub.record` の
//! もの、接続先が一時 Hub であることを確かめてから）だけを** grant の下で
//! 失効させる（docs/implementation-checklist.md §5「同じ接続先のものか」）。
//! 一時 Hub の DB ごと消すので実害は無いが、作法として行う。
//!
//! ## やらないこと
//!
//! * 製品コード（`src/`）の変更、CI への追加（実機が要る）。
//! * インストール済みの banto-hub（サービス）や、自分が起動していない
//!   プロセスへの操作。

use std::collections::{BTreeMap, HashMap};
use std::env;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use banto_hub_bootstrap::state::memory::MemoryKeyStore;
use banto_hub_bootstrap::{HubRecord, HubStatus, KeyStore};
use banto_tags::{
    CollectionGroupInput, CollectionGroupService, PlcConnectionInput, PlcConnectionService,
    TagInput, TagService,
};
use banto_tstore::{list_data_files, TsReader};
use chronogazer_core::collect::{
    resolve_data_dir, CollectEventRow, CollectorService, CollectorState, EventPage, QualityView,
    Readout,
};
use chronogazer_core::db::{init_db, Db};
use chronogazer_core::hub::HubService;
use chronogazer_core::hub_tags::{register_from_hub, HubTagRegistration, HubTagRow, NewHubGroup};
use chronogazer_core::settings::SettingsService;
use serde_json::{json, Value};
use tokio::time::sleep;

/// Hub 側の接続・グループの名前（外部名 = `接続.グループ.タグ`）。
const HUB_CONN_NAME: &str = "P6PLC";
const HUB_GROUP_NAME: &str = "P6";
/// ChronoGazer 側の名前。
const DIRECT_CONN_NAME: &str = "P6直結";
const DIRECT_GROUP_NAME: &str = "直結";
const HUB_SIDE_GROUP_NAME: &str = "Hub経由";
/// `chronogazer_core::hub` の設定キー（private な定数の読み取り専用のミラー。
/// `hub_real_smoke.rs` と同じ理由 - `key_id` と接続先を読むため）。
const HUB_RECORD_SETTINGS_KEY: &str = "hub.record";
const WORD_ORDER: &str = "low_high";
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);

/// 比較するタグ（名前・アドレス・型）。読み取り専用の安全範囲
/// （D3000〜D3019 / M1000〜M1007）。bit・i16・u16・i32 を混ぜる。
fn tag_specs() -> Vec<(String, String, &'static str)> {
    let mut specs = Vec::new();
    for n in 3000..3010 {
        specs.push((format!("d{n}"), format!("D{n}"), "i16"));
    }
    for n in 3010..3014 {
        specs.push((format!("d{n}"), format!("D{n}"), "u16"));
    }
    for n in [3014, 3016, 3018] {
        specs.push((format!("d{n}"), format!("D{n}"), "i32"));
    }
    for n in 1000..1008 {
        specs.push((format!("m{n}"), format!("M{n}"), "bit"));
    }
    specs
}

// --- 結果の表（hub_real_smoke.rs と同じ作法） ----------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StepStatus {
    Pass,
    Fail,
    SkippedBlocked,
    SkippedOptional,
}

impl StepStatus {
    fn mark(self) -> &'static str {
        match self {
            StepStatus::Pass => "OK",
            StepStatus::Fail => "NG",
            StepStatus::SkippedBlocked | StepStatus::SkippedOptional => "SKIP",
        }
    }

    fn is_pass(self) -> bool {
        matches!(self, StepStatus::Pass | StepStatus::SkippedOptional)
    }
}

struct StepResult {
    name: &'static str,
    status: StepStatus,
    detail: String,
}

impl StepResult {
    fn new(name: &'static str, ok: bool, detail: impl Into<String>) -> Self {
        Self {
            name,
            status: if ok {
                StepStatus::Pass
            } else {
                StepStatus::Fail
            },
            detail: detail.into(),
        }
    }

    fn skipped(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            status: StepStatus::SkippedBlocked,
            detail: detail.into(),
        }
    }

    fn skipped_optional(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            status: StepStatus::SkippedOptional,
            detail: detail.into(),
        }
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn env_or(name: &str, default: &str) -> String {
    env::var(name)
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_owned())
}

fn env_num<T: std::str::FromStr>(name: &str, default: T) -> T {
    env::var(name)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

// --- 一時 banto-hub（子プロセス） -----------------------------------------

/// このハーネスが起動した banto-hub。`Drop` で必ず止める（自分が起動した
/// プロセスだけ）。
struct HubProcess {
    bin: PathBuf,
    port: u16,
    work: PathBuf,
    child: Option<Child>,
}

impl HubProcess {
    fn spawn(&mut self) -> Result<u32, String> {
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.work.join("hub.log"))
            .map_err(|e| format!("hub.log を開けない: {e}"))?;
        let log_err = log
            .try_clone()
            .map_err(|e| format!("hub.log を複製できない: {e}"))?;
        let child = Command::new(&self.bin)
            .env("PORT", self.port.to_string())
            .env("BANTO_BIND", "127.0.0.1")
            .env("BANTO_DB", self.work.join("hub.sqlite3"))
            .env("BANTO_HUB_DATA", self.work.join("hub-data"))
            // 管理者のセットアップは使わない（試運転モードの grant で構成する）。
            .env_remove("BANTO_ALLOW_SETUP")
            .env_remove("BANTO_HUB_ROOT")
            .env_remove("BANTO_HUB_PROFILE")
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(log_err))
            .spawn()
            .map_err(|e| format!("{} を起動できない: {e}", self.bin.display()))?;
        let pid = child.id();
        self.child = Some(child);
        Ok(pid)
    }

    fn kill(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for HubProcess {
    fn drop(&mut self) {
        self.kill();
    }
}

// --- 一時 Hub の管理 REST（試運転モードの grant） --------------------------

struct HubAdmin {
    base: String,
    http: reqwest::Client,
    /// grant のトークン。**ログに出さない。**
    token: Option<String>,
}

impl HubAdmin {
    fn new(base: String) -> Self {
        let http = reqwest::ClientBuilder::new()
            .no_proxy()
            .timeout(HTTP_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("reqwest client");
        Self {
            base,
            http,
            token: None,
        }
    }

    async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<(u16, Value), String> {
        let mut request = self
            .http
            .request(method.clone(), format!("{}{path}", self.base))
            .header("X-Banto-Client", "banto");
        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }
        if let Some(body) = body {
            request = request
                .header("Content-Type", "application/json")
                .body(serde_json::to_vec(body).unwrap_or_default());
        }
        let response = request
            .send()
            .await
            .map_err(|e| format!("{method} {path}: 送信失敗: {e}"))?;
        let status = response.status().as_u16();
        let bytes = response
            .bytes()
            .await
            .map_err(|e| format!("{method} {path}: 応答の読み取り失敗: {e}"))?;
        let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        Ok((status, value))
    }

    async fn ok(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value, String> {
        let (status, value) = self.call(method.clone(), path, body).await?;
        if (200..300).contains(&status) {
            Ok(value)
        } else {
            let text = value.to_string();
            let short: String = text.chars().take(300).collect();
            Err(format!("{method} {path}: HTTP {status}: {short}"))
        }
    }

    async fn wait_ready(&self, limit: Duration) -> Result<(), String> {
        let deadline = Instant::now() + limit;
        loop {
            match self
                .call(reqwest::Method::GET, "/api/auth/status", None)
                .await
            {
                Ok((200, _)) => return Ok(()),
                Ok((status, _)) if Instant::now() >= deadline => {
                    return Err(format!("/api/auth/status が HTTP {status}"))
                }
                Err(e) if Instant::now() >= deadline => return Err(e),
                _ => sleep(Duration::from_millis(300)).await,
            }
        }
    }

    async fn grant(&mut self) -> Result<(), String> {
        self.token = None;
        let body = self
            .ok(reqwest::Method::POST, "/api/auth/grant/commissioning", None)
            .await?;
        match body["token"].as_str() {
            Some(token) if body["success"] == true => {
                self.token = Some(token.to_owned());
                Ok(())
            }
            _ => Err("grant の応答にトークンが無い".to_owned()),
        }
    }

    async fn logout(&mut self) {
        if self.token.is_some() {
            let _ = self
                .call(reqwest::Method::POST, "/api/auth/logout", None)
                .await;
            self.token = None;
        }
    }

    async fn collection(&self, action: &str) -> Result<String, String> {
        let body = self
            .ok(
                reqwest::Method::POST,
                &format!("/api/collection/{action}"),
                None,
            )
            .await?;
        let text = body.to_string();
        Ok(text.chars().take(200).collect())
    }
}

/// 一時 Hub に接続・グループ・タグを作る（収集停止中）。
async fn configure_hub(
    admin: &HubAdmin,
    plc_host: &str,
    plc_port: u16,
    period_ms: i64,
) -> Result<usize, String> {
    let conn = admin
        .ok(
            reqwest::Method::POST,
            "/api/plc-connections",
            Some(&json!({
                "name": HUB_CONN_NAME,
                "protocol": "slmp",
                "host": plc_host,
                "port": plc_port,
                "unitId": 1,
                "enabled": true,
                // 既定に頼らない（実機を読む）。
                "simulation": false,
                "wordOrder": WORD_ORDER,
            })),
        )
        .await?;
    let conn_id = conn["id"].as_i64().ok_or("接続の id が無い")?;
    let group = admin
        .ok(
            reqwest::Method::POST,
            "/api/collection-groups",
            Some(&json!({
                "name": HUB_GROUP_NAME,
                "plcConnectionId": conn_id,
                "periodMs": period_ms,
                "enabled": true,
                "defaultWritable": false,
            })),
        )
        .await?;
    let group_id = group["id"].as_i64().ok_or("グループの id が無い")?;
    let mut created = 0;
    for (name, address, data_type) in tag_specs() {
        admin
            .ok(
                reqwest::Method::POST,
                "/api/tags",
                Some(&json!({
                    "name": name,
                    "collectionGroupId": group_id,
                    "address": address,
                    "dataType": data_type,
                    "decimals": 0,
                    "enabled": true,
                    // 読み取りのみ（PLC へ書かない）。
                    "writable": false,
                })),
            )
            .await?;
        created += 1;
    }
    Ok(created)
}

// --- ChronoGazer 側の登録 -------------------------------------------------

/// 直結の接続・グループ・タグを登録し、`名前 → tag:<id>` を返す。
async fn register_direct(
    pool: &sqlx::SqlitePool,
    plc_host: &str,
    plc_port: u16,
    period_ms: i64,
) -> Result<Registered, String> {
    let conn = PlcConnectionService::new(pool.clone())
        .create(PlcConnectionInput {
            name: DIRECT_CONN_NAME.to_owned(),
            protocol: "slmp".to_owned(),
            host: plc_host.to_owned(),
            port: i64::from(plc_port),
            unit_id: 1,
            enabled: true,
            simulation: false,
            word_order: WORD_ORDER.to_owned(),
            database: None,
            username: None,
            password: None,
        })
        .await
        .map_err(|e| format!("直結の接続を登録できない: {e}"))?;
    let group = CollectionGroupService::new(pool.clone())
        .create(CollectionGroupInput {
            name: DIRECT_GROUP_NAME.to_owned(),
            plc_connection_id: conn.id,
            period_ms,
            enabled: true,
            default_writable: false,
            query_sql: None,
        })
        .await
        .map_err(|e| format!("直結のグループを登録できない: {e}"))?;
    let tags = TagService::new(pool.clone());
    let mut keys = BTreeMap::new();
    for (name, address, data_type) in tag_specs() {
        let tag = tags
            .create(TagInput {
                name: name.clone(),
                collection_group_id: group.id,
                address,
                data_type: data_type.to_owned(),
                string_length: None,
                string_encoding: "utf8".to_owned(),
                raw_lo: None,
                raw_hi: None,
                eng_lo: None,
                eng_hi: None,
                unit: None,
                decimals: 0,
                enabled: true,
                writable: false,
                tag_kind: "plc".to_owned(),
                expression: None,
                retain: false,
                expected_revision: None,
            })
            .await
            .map_err(|e| format!("直結のタグ {name} を登録できない: {e}"))?;
        keys.insert(name, format!("tag:{}", tag.id));
    }
    Ok((conn.id, group.id, keys))
}

/// 「Hub から追加」と同じ経路（[`register_from_hub`]）で Hub 経由のタグを
/// 登録し、`(接続 id, グループ id, 名前 → tag:<id>)` を返す。
async fn register_hub_side(
    pool: &sqlx::SqlitePool,
    hub: &HubService,
    period_ms: i64,
) -> Result<Registered, String> {
    let catalog = hub
        .catalog_for_registration()
        .await
        .map_err(|e| format!("catalog を読めない: {e}"))?;
    let tags = catalog
        .tags
        .ok_or("catalog を読めなかった（tags = null）")?;
    let prefix = format!("{HUB_CONN_NAME}.{HUB_GROUP_NAME}.");
    let wanted: Vec<String> = tag_specs().into_iter().map(|(n, _, _)| n).collect();
    let rows: Vec<HubTagRow> = tags
        .iter()
        .filter(|t| t.external_name.starts_with(&prefix) && wanted.contains(&t.name))
        .map(|t| HubTagRow {
            external_name: t.external_name.clone(),
            name: t.name.clone(),
            data_type: t.data_type.clone(),
            unit: t.unit.clone(),
            decimals: Some(t.decimals),
        })
        .collect();
    if rows.len() != wanted.len() {
        return Err(format!(
            "catalog に期待したタグが揃っていない（{} / {}）",
            rows.len(),
            wanted.len()
        ));
    }
    let result = register_from_hub(
        pool,
        &PlcConnectionService::new(pool.clone()),
        &CollectionGroupService::new(pool.clone()),
        &TagService::new(pool.clone()),
        HubTagRegistration {
            collection_group_id: None,
            new_group: Some(NewHubGroup {
                name: HUB_SIDE_GROUP_NAME.to_owned(),
                period_ms,
            }),
            tags: rows,
        },
    )
    .await
    .map_err(|e| format!("register_from_hub が失敗: {e}"))?;
    if !result.ok {
        return Err(format!(
            "登録が断られた: field={:?} rows={:?}",
            result.field_errors, result.row_errors
        ));
    }
    hub.registry_changed().await;
    let conn_id = result.connection_id.ok_or("接続 id が無い")?;
    let group_id = result
        .collection_group
        .as_ref()
        .map(|g| g.id)
        .ok_or("グループが無い")?;
    let keys = result
        .tags
        .iter()
        .map(|t| (t.name.clone(), format!("tag:{}", t.id)))
        .collect();
    Ok((conn_id, group_id, keys))
}

// --- 記録の読み出しと比較 -------------------------------------------------

/// あるグループの全サンプル（時刻・`tag:<id>` → 列の位置）。
struct GroupRows {
    rows: Vec<(i64, Vec<Option<f64>>)>,
    columns: HashMap<String, usize>,
}

async fn read_group(data_dir: &Path, group_key: &str) -> Result<GroupRows, String> {
    let mut rows = Vec::new();
    let mut columns = HashMap::new();
    let files = list_data_files(data_dir).map_err(|e| format!("データファイルの列挙: {e}"))?;
    for file in files {
        let reader = TsReader::open(&file.path)
            .await
            .map_err(|e| format!("{} を開けない: {e}", file.path.display()))?;
        let Some(meta) = reader.group(group_key) else {
            continue;
        };
        let local: HashMap<String, usize> = meta
            .columns
            .iter()
            .enumerate()
            .map(|(i, c)| (c.tag_key.clone(), i))
            .collect();
        if columns.is_empty() {
            columns = local.clone();
        } else if columns != local {
            return Err(format!("{group_key} の列がファイル間で違う"));
        }
        for sample in reader
            .read_range(group_key, i64::MIN, i64::MAX)
            .await
            .map_err(|e| format!("{group_key} を読めない: {e}"))?
        {
            rows.push((sample.ptime_ms, sample.values));
        }
    }
    rows.sort_by_key(|(t, _)| *t);
    Ok(GroupRows { rows, columns })
}

impl GroupRows {
    fn column(&self, tag_key: &str, from: i64, to: i64) -> Vec<(i64, Option<f64>)> {
        let Some(&i) = self.columns.get(tag_key) else {
            return Vec::new();
        };
        self.rows
            .iter()
            .filter(|(t, _)| *t >= from && *t <= to)
            .map(|(t, v)| (*t, v.get(i).copied().flatten()))
            .collect()
    }

    fn count(&self, from: i64, to: i64) -> usize {
        self.rows
            .iter()
            .filter(|(t, _)| *t >= from && *t <= to)
            .count()
    }
}

/// 登録の結果: `(接続 id, グループ id, 名前 → tag:<id>)`。
type Registered = (i64, i64, BTreeMap<String, String>);
/// 現在値の 1 タグ分（値・品質）。
type CurrentPair = Option<(Option<f64>, QualityView)>;
/// 手順2で組み立てる ChronoGazer 側の一式。
type Context = (
    sqlx::SqlitePool,
    SettingsService,
    HubService,
    Registered,
    Registered,
);

/// 1 組（直結と Hub 経由）の比較結果。
struct PairStats {
    name: String,
    direct_rows: usize,
    direct_nulls: usize,
    /// 直結側の先頭の null（最初の読み取りが終わる前の行）。
    direct_leading_nulls: usize,
    hub_rows: usize,
    hub_nulls: usize,
    /// Hub 側の先頭の null（最初の値が届く前）。
    hub_leading_nulls: usize,
    direct_distinct: usize,
    hub_distinct: usize,
    /// 時刻が周期以内の行同士で比べた数・一致した数。
    compared: usize,
    equal: usize,
    /// 不一致のうち、±2 周期の直結の値のどれとも合わないもの。
    unexplained: usize,
    sample: String,
}

fn distinct(values: &[(i64, Option<f64>)]) -> usize {
    let mut seen: Vec<u64> = values
        .iter()
        .filter_map(|(_, v)| v.map(f64::to_bits))
        .collect();
    seen.sort_unstable();
    seen.dedup();
    seen.len()
}

fn compare_pair(
    name: &str,
    direct: &[(i64, Option<f64>)],
    hub: &[(i64, Option<f64>)],
    period_ms: i64,
) -> PairStats {
    let hub_leading_nulls = hub.iter().take_while(|(_, v)| v.is_none()).count();
    let direct_leading_nulls = direct.iter().take_while(|(_, v)| v.is_none()).count();
    let direct_good: Vec<(i64, f64)> = direct
        .iter()
        .filter_map(|(t, v)| v.map(|v| (*t, v)))
        .collect();
    let mut compared = 0;
    let mut equal = 0;
    let mut unexplained = 0;
    for (t, v) in hub.iter().filter_map(|(t, v)| v.map(|v| (*t, v))) {
        let nearest = direct_good.iter().min_by_key(|(dt, _)| (dt - t).abs());
        let Some((dt, dv)) = nearest else {
            continue;
        };
        if (dt - t).abs() > period_ms {
            continue;
        }
        compared += 1;
        if *dv == v {
            equal += 1;
        } else if !direct_good
            .iter()
            .any(|(dt, dv)| (dt - t).abs() <= 2 * period_ms && *dv == v)
        {
            unexplained += 1;
        }
    }
    let last = |values: &[(i64, Option<f64>)]| {
        values
            .iter()
            .rev()
            .find_map(|(_, v)| *v)
            .map(|v| v.to_string())
            .unwrap_or_else(|| "-".to_owned())
    };
    PairStats {
        name: name.to_owned(),
        direct_rows: direct.len(),
        direct_nulls: direct.iter().filter(|(_, v)| v.is_none()).count(),
        direct_leading_nulls,
        hub_rows: hub.len(),
        hub_nulls: hub.iter().filter(|(_, v)| v.is_none()).count(),
        hub_leading_nulls,
        direct_distinct: distinct(direct),
        hub_distinct: distinct(hub),
        compared,
        equal,
        unexplained,
        sample: format!("{} / {}", last(direct), last(hub)),
    }
}

// 見出しを値と同じ幅指定で揃えるため、文字列リテラルを引数で渡す。
#[allow(clippy::print_literal)]
fn print_pair_table(stats: &[PairStats]) {
    println!(
        "  {:<6} {:>5} {:>5} {:>5} {:>5} {:>5} {:>5} {:>4} {:>4} {:>5} {:>5} {:>5}  {}",
        "tag",
        "直行",
        "直null",
        "Hub行",
        "Hubnull",
        "直先頭",
        "Hub先頭",
        "直種",
        "Hub種",
        "比較",
        "一致",
        "説明不能",
        "最後の値(直/Hub)"
    );
    for s in stats {
        println!(
            "  {:<6} {:>5} {:>5} {:>5} {:>5} {:>5} {:>5} {:>4} {:>4} {:>5} {:>5} {:>5}  {}",
            s.name,
            s.direct_rows,
            s.direct_nulls,
            s.direct_leading_nulls,
            s.hub_rows,
            s.hub_nulls,
            s.hub_leading_nulls,
            s.direct_distinct,
            s.hub_distinct,
            s.compared,
            s.equal,
            s.unexplained,
            s.sample
        );
    }
}

async fn read_events(svc: &CollectorService) -> Result<Vec<CollectEventRow>, String> {
    match svc.events(EventPage::new(None, Some(500))).await {
        Readout::Ready { data } => {
            let mut rows = data.rows;
            rows.sort_by_key(|r| r.id);
            Ok(rows)
        }
        other => Err(format!("イベント一覧が読めない: {}", other.as_str())),
    }
}

fn link_kinds(events: &[CollectEventRow], conn_key: &str, after_ms: i64) -> Vec<String> {
    events
        .iter()
        .filter(|r| {
            r.kind.starts_with("plc_")
                && r.connection_key.as_deref() == Some(conn_key)
                && r.ts_ms >= after_ms
        })
        .map(|r| r.kind.clone())
        .collect()
}

/// 現在値の 1 組分（品質・値）の要約。
fn current_pair_line(
    values: &HashMap<String, chronogazer_core::collect::CurrentSampleView>,
    direct_key: &str,
    hub_key: &str,
) -> (CurrentPair, CurrentPair) {
    (
        values.get(direct_key).map(|s| (s.value, s.quality)),
        values.get(hub_key).map(|s| (s.value, s.quality)),
    )
}

async fn read_hub_record(settings: &SettingsService) -> Option<HubRecord> {
    let raw = settings.get(HUB_RECORD_SETTINGS_KEY).await.ok()??;
    serde_json::from_str(&raw).ok()
}

/// 全 Hub 経由のタグの現在値が条件を満たすまで待つ。
async fn wait_hub_tags<F>(svc: &CollectorService, keys: &[String], limit: Duration, pred: F) -> bool
where
    F: Fn(Option<f64>, QualityView) -> bool,
{
    let deadline = Instant::now() + limit;
    loop {
        if let Readout::Ready { data } = svc.values_response().readout {
            if keys
                .iter()
                .all(|k| data.get(k).is_some_and(|s| pred(s.value, s.quality)))
            {
                return true;
            }
        }
        if Instant::now() >= deadline {
            return false;
        }
        sleep(Duration::from_millis(200)).await;
    }
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    println!("=== ChronoGazer #383 段階3 P6 実機比較ハーネス ===");
    let Some(plc_host) = env::var("PLC_HOST")
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
    else {
        println!("PLC_HOST が未設定です（PLC の IP を環境変数で渡してください）");
        std::process::exit(2);
    };
    let hub_plc_port: u16 = env_num("P6_HUB_PLC_PORT", 3101);
    let direct_plc_port: u16 = env_num("P6_DIRECT_PLC_PORT", 3102);
    let hub_port: u16 = env_num("P6_HUB_PORT", 8731);
    let run_secs: u64 = env_num("P6_RUN_SECS", 60);
    let period_ms: i64 = env_num("P6_PERIOD_MS", 1000);
    let outage = env_or("P6_OUTAGE", "");
    let outage_secs: u64 = env_num("P6_OUTAGE_SECS", 15);
    let recover_secs: u64 = env_num("P6_RECOVER_SECS", 20);
    let default_bin = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../target/debug/banto-hub.exe")
        .display()
        .to_string();
    let hub_bin = PathBuf::from(env_or("P6_HUB_BIN", &default_bin));
    let work = PathBuf::from(env_or(
        "P6_WORK_DIR",
        &env::temp_dir()
            .join(format!("chronogazer-p6-{}", now_ms()))
            .display()
            .to_string(),
    ));
    if !matches!(outage.as_str(), "" | "process" | "collection") {
        println!("P6_OUTAGE は process / collection / 空 のいずれかです（{outage}）");
        std::process::exit(2);
    }
    println!(
        "Hub→PLC ポート {hub_plc_port} / 直結→PLC ポート {direct_plc_port} / 一時 Hub 127.0.0.1:{hub_port} / 周期 {period_ms}ms / 定常 {run_secs}s / 停止・再開 {}",
        if outage.is_empty() { "なし" } else { outage.as_str() }
    );
    println!("作業ディレクトリ: {}", work.display());
    let _ = std::fs::create_dir_all(&work);
    let hub_url = format!("http://127.0.0.1:{hub_port}");

    let mut results: Vec<StepResult> = Vec::new();
    let mut hub_proc = HubProcess {
        bin: hub_bin.clone(),
        port: hub_port,
        work: work.join("hub"),
        child: None,
    };
    let _ = std::fs::create_dir_all(&hub_proc.work);
    let mut admin = HubAdmin::new(hub_url.clone());

    // 1. 一時 Hub の起動・構成 -------------------------------------------
    println!(
        "\n[1] 一時 banto-hub の起動と構成（試運転モードの grant。管理者アカウントは作らない）"
    );
    let step1 = async {
        let pid = hub_proc.spawn()?;
        println!("  起動: pid {pid}");
        admin.wait_ready(Duration::from_secs(60)).await?;
        admin.grant().await?;
        let tags = configure_hub(&admin, &plc_host, hub_plc_port, period_ms).await?;
        let started = admin.collection("start").await?;
        println!("  タグ {tags} 本を作成、収集開始: {started}");
        Ok::<usize, String>(tags)
    }
    .await;
    let hub_ok = match &step1 {
        Ok(tags) => {
            results.push(StepResult::new(
                "1. 一時 Hub の起動・構成",
                true,
                format!("pid 起動・タグ {tags} 本・収集開始（grant のみ）"),
            ));
            true
        }
        Err(e) => {
            println!("  失敗: {e}");
            results.push(StepResult::new(
                "1. 一時 Hub の起動・構成",
                false,
                e.clone(),
            ));
            false
        }
    };

    // 2. ChronoGazer の準備（Hub 接続・登録） ----------------------------
    println!("\n[2] ChronoGazer: 設定 DB・Hub 接続（キー自己発行）・直結と Hub 経由のタグの登録");
    let cg_dir = work.join("chronogazer");
    let _ = std::fs::create_dir_all(&cg_dir);
    let data_dir = resolve_data_dir(&cg_dir, "./data");
    let mut ctx: Option<Context> = None;
    if hub_ok {
        let step2 = async {
            let pool = init_db(cg_dir.join("chronogazer.sqlite3"))
                .await
                .map_err(|e| format!("設定 DB: {e}"))?;
            let settings = SettingsService::new(Db::Sqlite(pool.clone()));
            let keys: Arc<dyn KeyStore> = Arc::new(MemoryKeyStore::new());
            let hub = HubService::new(settings.clone(), keys)
                .await
                .map_err(|e| format!("HubService: {e}"))?;
            hub.attach_registry(pool.clone());
            let view = hub
                .connect(&hub_url)
                .await
                .map_err(|e| format!("Hub に接続できない: {e}"))?;
            if !matches!(view.status, HubStatus::Connected { .. }) {
                return Err(format!("Hub の状態が connected でない: {:?}", view.status));
            }
            println!(
                "  Hub 接続: {:?}（キー名 {:?}）",
                view.status, view.key_name
            );
            let direct = register_direct(&pool, &plc_host, direct_plc_port, period_ms).await?;
            let hub_side = register_hub_side(&pool, &hub, period_ms).await?;
            println!(
                "  登録: 直結 conn:{} grp:{} {} 本 / Hub 経由 conn:{} grp:{} {} 本",
                direct.0,
                direct.1,
                direct.2.len(),
                hub_side.0,
                hub_side.1,
                hub_side.2.len()
            );
            // 購読が live になり、登録した全タグを購読するまで待つ。
            let deadline = Instant::now() + Duration::from_secs(60);
            loop {
                let sub = hub.subscription().await;
                if sub.state == "live" && sub.subscribed_count == hub_side.2.len() {
                    println!(
                        "  購読: live・{} 本（未解決 {:?} / 購読不可 {:?}）",
                        sub.subscribed_count, sub.unresolved, sub.unsupported
                    );
                    break;
                }
                if Instant::now() >= deadline {
                    return Err(format!(
                        "購読が live にならない: state={} count={} reason={:?} lastError={:?}",
                        sub.state, sub.subscribed_count, sub.reason, sub.last_error
                    ));
                }
                sleep(Duration::from_millis(300)).await;
            }
            Ok((pool, settings, hub, direct, hub_side))
        }
        .await;
        match step2 {
            Ok(value) => {
                results.push(StepResult::new(
                    "2. Hub 接続・タグ登録・購読",
                    true,
                    "キー自己発行・register_from_hub・購読 live",
                ));
                ctx = Some(value);
            }
            Err(e) => {
                println!("  失敗: {e}");
                results.push(StepResult::new("2. Hub 接続・タグ登録・購読", false, e));
            }
        }
    } else {
        results.push(StepResult::skipped(
            "2. Hub 接続・タグ登録・購読",
            "手順1が失敗",
        ));
    }

    // 3〜6. 収集 -----------------------------------------------------------
    let names: Vec<String> = tag_specs().into_iter().map(|(n, _, _)| n).collect();
    let mut svc_opt: Option<CollectorService> = None;
    let mut run_start = 0_i64;
    let mut steady_end = 0_i64;
    let mut outage_window: Option<(i64, i64, i64)> = None;
    if let Some((pool, _settings, hub, direct, hub_side)) = &ctx {
        println!("\n[3] 収集の開始（attach_hub 済み = デスクトップと同じ形）");
        let svc = CollectorService::new(pool.clone(), data_dir.clone());
        let attached = svc.attach_hub(hub.clone());
        match svc.start().await {
            Ok(outcome) => {
                let state = svc.state();
                let ok = attached
                    && !outcome.pending
                    && matches!(&state, CollectorState::Running { groups: 2, tags, exclusions }
                        if *tags == names.len() * 2 && exclusions.is_empty());
                println!("  状態: {state:?}");
                results.push(StepResult::new(
                    "3. 収集開始（2 グループ・外し 0）",
                    ok,
                    format!("{state:?}").chars().take(160).collect::<String>(),
                ));
                run_start = now_ms();
                svc_opt = Some(svc);
            }
            Err(e) => {
                println!("  失敗: {e}");
                results.push(StepResult::new(
                    "3. 収集開始（2 グループ・外し 0）",
                    false,
                    e.to_string(),
                ));
            }
        }

        if let Some(svc) = &svc_opt {
            println!("\n[4] 定常運転 {run_secs} 秒");
            let hub_keys: Vec<String> = hub_side.2.values().cloned().collect();
            let first_good = wait_hub_tags(svc, &hub_keys, Duration::from_secs(15), |v, q| {
                v.is_some() && q == QualityView::Good
            })
            .await;
            println!("  Hub 経由の全タグが good: {first_good}");
            let deadline = Instant::now() + Duration::from_secs(run_secs);
            let mut tick = 0;
            while Instant::now() < deadline {
                sleep(Duration::from_secs(1)).await;
                tick += 1;
                if tick % 15 == 0 {
                    if let Readout::Ready { data } = svc.values_response().readout {
                        let bad = data
                            .values()
                            .filter(|s| s.quality != QualityView::Good)
                            .count();
                        println!(
                            "  {tick:>3}s: 現在値 {} 本中 good 以外 {bad} 本",
                            data.len()
                        );
                    }
                }
            }
            steady_end = now_ms();
            // 現在値の突き合わせ
            let mut current_ok = true;
            let mut current_lines = Vec::new();
            match svc.values_response().readout {
                Readout::Ready { data } => {
                    for name in &names {
                        let (d, h) = current_pair_line(&data, &direct.2[name], &hub_side.2[name]);
                        let same = matches!((d, h), (Some((dv, dq)), Some((hv, hq)))
                            if dq == QualityView::Good && hq == QualityView::Good && dv == hv);
                        if !same {
                            current_ok = false;
                        }
                        current_lines.push(format!(
                            "{name}: 直 {d:?} / Hub {h:?}{}",
                            if same { "" } else { "  <-- 不一致" }
                        ));
                    }
                }
                other => {
                    current_ok = false;
                    current_lines.push(format!("現在値が読めない: {}", other.as_str()));
                }
            }
            for line in &current_lines {
                println!("  {line}");
            }
            results.push(StepResult::new(
                "4. 現在値（組ごとに品質 good・値一致）",
                first_good && current_ok,
                format!("初回 good={first_good} / 全組一致={current_ok}"),
            ));

            // 5. Hub の停止・再開
            if outage.is_empty() {
                results.push(StepResult::skipped_optional(
                    "5. Hub 停止・再開",
                    "P6_OUTAGE 未指定",
                ));
            } else {
                println!("\n[5] Hub の停止・再開（{outage}、停止 {outage_secs}s・再開後 {recover_secs}s）");
                let direct_keys: Vec<String> = direct.2.values().cloned().collect();
                let down_at = now_ms();
                let stop = if outage == "process" {
                    hub_proc.kill();
                    Ok("プロセスを停止".to_owned())
                } else {
                    admin.collection("stop").await
                };
                println!("  停止: {stop:?}");
                let went_bad = wait_hub_tags(
                    svc,
                    &hub_keys,
                    Duration::from_secs(outage_secs.max(5) + 15),
                    |v, q| v.is_none() && q == QualityView::Bad,
                )
                .await;
                let bad_at = now_ms();
                println!(
                    "  Hub 経由の全タグが Bad + null: {went_bad}（停止から {} ms）",
                    bad_at - down_at
                );
                let direct_good_during = match svc.values_response().readout {
                    Readout::Ready { data } => direct_keys
                        .iter()
                        .all(|k| data.get(k).is_some_and(|s| s.quality == QualityView::Good)),
                    _ => false,
                };
                println!("  その間の直結: 全タグ good = {direct_good_during}");
                let remaining = Duration::from_secs(outage_secs)
                    .saturating_sub(Duration::from_millis((bad_at - down_at).max(0) as u64));
                sleep(remaining).await;
                let up_at = now_ms();
                let restart = async {
                    if outage == "process" {
                        let pid = hub_proc.spawn()?;
                        admin.wait_ready(Duration::from_secs(60)).await?;
                        admin.grant().await?;
                        let s = admin.collection("start").await?;
                        Ok::<String, String>(format!("pid {pid} で再起動・収集開始 {s}"))
                    } else {
                        admin.collection("start").await
                    }
                }
                .await;
                println!("  再開: {restart:?}");
                let recovered = restart.is_ok()
                    && wait_hub_tags(svc, &hub_keys, Duration::from_secs(90), |v, q| {
                        v.is_some() && q == QualityView::Good
                    })
                    .await;
                let recovered_at = now_ms();
                println!(
                    "  Hub 経由の全タグが good に戻った: {recovered}（再開から {} ms）",
                    recovered_at - up_at
                );
                sleep(Duration::from_secs(recover_secs)).await;
                outage_window = Some((down_at, bad_at, up_at));
                let events = read_events(svc).await.unwrap_or_default();
                let hub_conn_key = format!("conn:{}", hub_side.0);
                let direct_conn_key = format!("conn:{}", direct.0);
                let hub_kinds = link_kinds(&events, &hub_conn_key, down_at);
                let direct_kinds = link_kinds(&events, &direct_conn_key, down_at);
                println!("  停止以後のイベント: Hub {hub_kinds:?} / 直結 {direct_kinds:?}");
                // Hub の接続キーのイベントは **Hub とのリンク（購読）**の切断・
                // 再接続を表す（`banto_collect::external` のモジュール doc）。
                // - process: Hub が落ちる = リンクが落ちる → disconnected → reconnected。
                // - collection: Hub は生きていてリンクは Live のまま、値だけが
                //   Bad になる → イベントは出ない（設計どおり。2026-10-11 の P6
                //   実機で確認）。
                let expected_hub: Vec<String> = if outage == "process" {
                    vec!["plc_disconnected".to_owned(), "plc_reconnected".to_owned()]
                } else {
                    Vec::new()
                };
                let events_ok = hub_kinds == expected_hub && direct_kinds.is_empty();
                results.push(StepResult::new(
                    "5. Hub 停止・再開",
                    went_bad && direct_good_during && recovered && events_ok,
                    format!(
                        "Bad+null={went_bad} / 直結 good={direct_good_during} / 復帰={recovered} / イベント Hub {hub_kinds:?} 直結 {direct_kinds:?}"
                    ),
                ));
            }

            println!("\n[6] 収集の停止");
            match svc.stop().await {
                Ok(_) => results.push(StepResult::new("6. 収集停止", true, "")),
                Err(e) => results.push(StepResult::new("6. 収集停止", false, e.to_string())),
            }
        } else {
            for name in [
                "4. 現在値（組ごとに品質 good・値一致）",
                "5. Hub 停止・再開",
                "6. 収集停止",
            ] {
                results.push(StepResult::skipped(name, "手順3が失敗"));
            }
        }
    } else {
        for name in [
            "3. 収集開始（2 グループ・外し 0）",
            "4. 現在値（組ごとに品質 good・値一致）",
            "5. Hub 停止・再開",
            "6. 収集停止",
        ] {
            results.push(StepResult::skipped(name, "手順2が失敗"));
        }
    }

    // 7. 記録の比較 ---------------------------------------------------------
    if let (Some((_, _, _, direct, hub_side)), Some(svc)) = (&ctx, &svc_opt) {
        println!("\n[7] データファイル（tstore）の比較: 定常区間（開始〜{run_secs}s）");
        let direct_group = format!("grp:{}", direct.1);
        let hub_group = format!("grp:{}", hub_side.1);
        match (
            read_group(&data_dir, &direct_group).await,
            read_group(&data_dir, &hub_group).await,
        ) {
            (Ok(d), Ok(h)) => {
                let d_rows = d.count(run_start, steady_end);
                let h_rows = h.count(run_start, steady_end);
                println!("  行数（定常区間）: 直結 {d_rows} / Hub 経由 {h_rows}（全区間: 直結 {} / Hub 経由 {}）", d.rows.len(), h.rows.len());
                let stats: Vec<PairStats> = names
                    .iter()
                    .map(|name| {
                        compare_pair(
                            name,
                            &d.column(&direct.2[name], run_start, steady_end),
                            &h.column(&hub_side.2[name], run_start, steady_end),
                            period_ms,
                        )
                    })
                    .collect();
                print_pair_table(&stats);
                let rows_ok = d_rows > 0 && (d_rows as i64 - h_rows as i64).abs() <= 2;
                let null_ok = stats.iter().all(|s| {
                    s.direct_nulls == s.direct_leading_nulls
                        && s.direct_leading_nulls <= 2
                        && s.hub_nulls == s.hub_leading_nulls
                        && s.hub_leading_nulls <= 2
                });
                let match_ok = stats
                    .iter()
                    .all(|s| s.compared + 2 >= s.hub_rows && s.unexplained == 0);
                results.push(StepResult::new(
                    "7. 記録の比較（行数・null・値）",
                    rows_ok && null_ok && match_ok,
                    format!("行数 直{d_rows}/Hub{h_rows} 差≤2={rows_ok} / null は両側とも先頭≤2 行だけ={null_ok} / 値一致={match_ok}"),
                ));

                if let Some((down_at, bad_at, up_at)) = outage_window {
                    println!("\n[7b] 停止区間の記録（Hub が Bad になってから再開まで）");
                    let in_bad: Vec<(usize, usize, usize, usize)> = names
                        .iter()
                        .map(|name| {
                            let dc = d.column(&direct.2[name], bad_at, up_at);
                            let hc = h.column(&hub_side.2[name], bad_at, up_at);
                            (
                                dc.len(),
                                dc.iter().filter(|(_, v)| v.is_none()).count(),
                                hc.len(),
                                hc.iter().filter(|(_, v)| v.is_none()).count(),
                            )
                        })
                        .collect();
                    let (dl, dn, hl, hn) = in_bad[0];
                    println!("  {}: 直結 {dl} 行 null {dn} / Hub 経由 {hl} 行 null {hn}（停止から Bad まで {} ms）", names[0], bad_at - down_at);
                    let ok = in_bad
                        .iter()
                        .all(|(dl, dn, hl, hn)| *dl > 0 && *dn == 0 && *hl > 0 && hn == hl);
                    let after: Vec<(usize, usize)> = names
                        .iter()
                        .map(|name| {
                            let hc = h.column(&hub_side.2[name], up_at, i64::MAX);
                            let trailing = hc.iter().rev().take_while(|(_, v)| v.is_some()).count();
                            (hc.len(), trailing)
                        })
                        .collect();
                    println!(
                        "  再開後: Hub 経由 {} 行のうち末尾の連続した値あり {} 行",
                        after[0].0, after[0].1
                    );
                    let after_ok = after.iter().all(|(_, trailing)| *trailing >= 5);
                    results.push(StepResult::new(
                        "7b. 停止区間の記録（Hub null・直結は値）",
                        ok && after_ok,
                        format!("停止中 Hub 全 null・直結 全値={ok} / 再開後に値が続く={after_ok}"),
                    ));
                }
            }
            (d, h) => {
                let e = format!("{:?} / {:?}", d.err(), h.err());
                println!("  失敗: {e}");
                results.push(StepResult::new("7. 記録の比較（行数・null・値）", false, e));
            }
        }

        println!("\n[8] イベント（collect_events）");
        match read_events(svc).await {
            Ok(events) => {
                let hub_conn_key = format!("conn:{}", hub_side.0);
                let direct_conn_key = format!("conn:{}", direct.0);
                let hub_kinds = link_kinds(&events, &hub_conn_key, 0);
                let direct_kinds = link_kinds(&events, &direct_conn_key, 0);
                let all: Vec<String> = events
                    .iter()
                    .map(|r| match &r.connection_key {
                        Some(k) => format!("{}({k})", r.kind),
                        None => r.kind.clone(),
                    })
                    .collect();
                println!("  全イベント: {all:?}");
                let steady_hub: Vec<String> = events
                    .iter()
                    .filter(|r| {
                        r.kind.starts_with("plc_")
                            && r.connection_key.as_deref() == Some(hub_conn_key.as_str())
                            && r.ts_ms <= steady_end
                    })
                    .map(|r| r.kind.clone())
                    .collect();
                let ok = steady_hub == vec!["plc_connected".to_owned()]
                    && direct_kinds == vec!["plc_connected".to_owned()];
                results.push(StepResult::new(
                    "8. イベント（定常区間は両接続とも plc_connected 1 件）",
                    ok,
                    format!("Hub {hub_kinds:?} / 直結 {direct_kinds:?}"),
                ));
            }
            Err(e) => results.push(StepResult::new("8. イベント", false, e)),
        }
    } else {
        results.push(StepResult::skipped(
            "7. 記録の比較（行数・null・値）",
            "収集が走らなかった",
        ));
        results.push(StepResult::skipped("8. イベント", "収集が走らなかった"));
    }

    // 9. 後片付け -----------------------------------------------------------
    println!("\n[9] 後片付け（発行キーの失効・Hub 購読の停止・一時 Hub の停止）");
    let mut cleanup_notes = Vec::new();
    let mut cleanup_ok = true;
    if let Some((pool, settings, hub, _, _)) = &ctx {
        let record = read_hub_record(settings).await;
        hub.shutdown().await;
        match record {
            Some(record) if record.endpoint.trim_end_matches('/') == hub_url => {
                if let Some(key_id) = record.key_id {
                    let revoked = async {
                        if admin.token.is_none() {
                            admin.grant().await?;
                        }
                        admin
                            .ok(
                                reqwest::Method::POST,
                                &format!("/api/api-keys/{key_id}/revoke"),
                                None,
                            )
                            .await
                    }
                    .await;
                    match revoked {
                        Ok(_) => cleanup_notes.push(format!("キー id {key_id} を失効")),
                        Err(e) => {
                            cleanup_ok = false;
                            cleanup_notes.push(format!("キー id {key_id} の失効に失敗: {e}"));
                        }
                    }
                } else {
                    cleanup_notes.push("key_id が記録に無い".to_owned());
                }
            }
            Some(_) => {
                cleanup_ok = false;
                cleanup_notes.push("hub.record の接続先が一時 Hub でない（失効しない）".to_owned());
            }
            None => cleanup_notes.push("hub.record が無い（キー未発行）".to_owned()),
        }
        pool.close().await;
    }
    admin.logout().await;
    hub_proc.kill();
    cleanup_notes.push("一時 Hub を停止".to_owned());
    println!("  {}", cleanup_notes.join(" / "));
    results.push(StepResult::new(
        "9. 後片付け",
        cleanup_ok,
        cleanup_notes.join(" / "),
    ));

    println!("\n=== 結果 ===");
    for r in &results {
        println!("  [{}] {} - {}", r.status.mark(), r.name, r.detail);
    }
    let all_ok = results.iter().all(|r| r.status.is_pass());
    println!("\n総合: {}", if all_ok { "PASS" } else { "FAIL" });
    std::process::exit(if all_ok { 0 } else { 1 });
}
