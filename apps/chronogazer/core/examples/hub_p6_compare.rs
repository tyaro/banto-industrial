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
//!   （一時 DB・データ・profile の root は `P6_WORK_DIR` の下）。素の banto-hub は試運転モード
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
//! cargo build -p banto-hub-host --bin banto-hub
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
//! ## 起動した子以外の Hub に触らない（#584 レビュー）
//!
//! 管理 API（grant・構成・収集の stop/start・キーの失効）は、**今回起動した
//! 子の一時インスタンスだと確かめた相手にだけ**送る。取り違えの疑いがあれば
//! 何も操作せずに中止する。
//!
//! 1. **ポートの空き確認**（[`ensure_port_free`]）: 起動前に
//!    `127.0.0.1:P6_HUB_PORT` へ接続できたら（既にリスナーがいる）即中止。
//! 2. **子の終了の検出**（[`HubProcess::ensure_alive`]）: 起動待ちの間も子を
//!    `try_wait` で見張り、プロファイル競合・bind 失敗などで終了したら即中止
//!    （既存の Hub の応答で「起動できた」と誤認しない）。
//! 3. **応答元の照合**（[`confirm_own_hub`]）: 認証不要の
//!    `GET /api/v1/openapi.json` の `info.x-banto-hub-profile-id` が今回だけの
//!    一意な profile ID と一致し、専用 root の `profile.lock`（排他の保持者の
//!    診断ファイル）の pid が子の pid と一致すること。起動時に加え、Hub 接続
//!    （キーの自己発行）・収集の stop/start・キーの失効・ログアウトの直前にも
//!    確かめ直す。照合前の相手には grant のトークンを送らない。
//! 4. **専用 root・一意な profile**（[`HubProcess`] の doc）: インストール済みの
//!    Hub（`%ProgramData%\BantoHub`・profile `default`・mutex
//!    `Global\BantoHub.default`）と排他を共有しない。
//!
//! 限界: 照合は「操作の段の直前」に行い、要求 1 本ごとではない。照合の後、
//! 同じ段の途中で子が落ちて別のプロセスが同じポートを取ると、その段の残りの
//! 要求はそちらへ届きうる（次の段の照合で止まる）。
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
//! 一時 Hub の DB ごと消すので実害は無いが、作法として行う。キーの発行後に
//! 手順2（タグ登録・購読）が失敗しても失効させる（設定 DB と `HubService` を
//! 作った時点で後片付け用に控える）。一時 Hub の停止は失敗経路でも必ず行う。
//!
//! ## やらないこと
//!
//! * 製品コード（`src/`）の変更、CI への追加（実機が要る）。
//! * インストール済みの banto-hub（サービス）や、自分が起動していない
//!   プロセスへの操作。

use std::collections::{BTreeMap, HashMap};
use std::env;
use std::fs::OpenOptions;
use std::net::{SocketAddr, TcpListener, TcpStream};
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
///
/// **インストール済みの Hub と排他を共有しない**: banto-hub は DB を開く前に
/// profile 単位の排他（Windows の named mutex `Global\BantoHub.<profile-id>`。
/// 名前は profile ID だけで決まる - `banto_hub_core::profile_paths::mutex_name`）
/// を取る。`BANTO_HUB_ROOT` / `BANTO_HUB_PROFILE` を渡さないと既定の
/// `%ProgramData%\BantoHub` と profile `default` に戻り、インストール済みの
/// サービスと同じ mutex を取り合う。そこで作業ディレクトリ配下の専用 root と、
/// **試験ごとに一意な profile ID**（[`unique_profile_id`]）を明示して渡す。
struct HubProcess {
    bin: PathBuf,
    port: u16,
    work: PathBuf,
    /// `BANTO_HUB_ROOT`（作業ディレクトリ配下・絶対パス）。
    root: PathBuf,
    /// `BANTO_HUB_PROFILE`（試験ごとに一意）。
    profile_id: String,
    child: Option<Child>,
}

/// 試験ごとに一意な profile ID（`p6-<ハーネスの pid>-<起動時刻 ms>`）。
///
/// banto-hub の `validate_profile_id` は空・パス区切り・`.`/`..`・`:`・予約名
/// （`NUL` など）・末尾の `.`/空白だけを拒否し（後の 3 つは #626 で足した）、
/// **拒否したときは黙って `default` に戻す**
/// （`profile_paths::resolve_profile_paths_from_env`）。英数字と `-` だけで
/// 組み立てる（万一戻った場合も、応答元の照合 [`confirm_own_hub`] が
/// profile ID の不一致で止める）。
fn unique_profile_id() -> String {
    format!("p6-{}-{}", std::process::id(), now_ms())
}

/// `127.0.0.1:port` に誰もいないことを確かめる。**既にリスナーがいれば即中止**
/// （接続できた = 別のプロセスがいる）。接続できず bind もできない場合は、
/// 直前に止めた子の残り（TIME_WAIT など）に備えて `limit` まで待ってから
/// 中止する。
async fn ensure_port_free(port: u16, limit: Duration) -> Result<(), String> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let deadline = Instant::now() + limit;
    loop {
        if TcpStream::connect_timeout(&addr, Duration::from_millis(500)).is_ok() {
            return Err(format!(
                "127.0.0.1:{port} に既にリスナーがいる（このハーネスが起動したものではない。何も操作せず中止）"
            ));
        }
        match TcpListener::bind(addr) {
            Ok(listener) => {
                drop(listener);
                return Ok(());
            }
            Err(e) if Instant::now() >= deadline => {
                return Err(format!(
                    "127.0.0.1:{port} を確保できない（使用中の疑い。何も操作せず中止）: {e}"
                ))
            }
            Err(_) => sleep(Duration::from_millis(300)).await,
        }
    }
}

impl HubProcess {
    /// 子の診断ファイル `{root}/profiles/<profile-id>/profile.lock`
    /// （`banto_instance_lock` が排他の取得時に保持者の pid を書く）。
    fn lock_file(&self) -> PathBuf {
        self.root
            .join("profiles")
            .join(&self.profile_id)
            .join("profile.lock")
    }

    /// 子が生きていればその pid。終了していれば（起動失敗・プロファイル競合・
    /// bind 失敗など）終了状態を載せた `Err`。
    fn ensure_alive(&mut self) -> Result<u32, String> {
        let Some(child) = self.child.as_mut() else {
            return Err("一時 Hub が動いていない（起動していない・停止済み）".to_owned());
        };
        match child.try_wait() {
            Ok(None) => Ok(child.id()),
            Ok(Some(status)) => Err(format!(
                "一時 Hub（pid {}）が終了した: {status}（hub.log を参照）",
                child.id()
            )),
            Err(e) => Err(format!("一時 Hub の状態を確かめられない: {e}")),
        }
    }

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
            // 専用 root と一意な profile（インストール済みの Hub と排他・
            // ディレクトリを共有しない。この型の doc）。
            .env("BANTO_HUB_ROOT", &self.root)
            .env("BANTO_HUB_PROFILE", &self.profile_id)
            // 管理者のセットアップは使わない（試運転モードの grant で構成する）。
            .env_remove("BANTO_ALLOW_SETUP")
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

// --- 応答元の照合（起動した子以外の Hub に管理操作を送らない） --------------

/// [`confirm_own_hub`] の失敗の種類。
enum Identity {
    /// まだ照合できない（起動中で応答が無い・診断ファイルが未作成）。待てば
    /// 照合できる可能性がある。
    NotYet(String),
    /// 取り違えの疑い（子が終了した・別の profile の Hub が応答した）。
    /// **待たずに中止する。**
    Mismatch(String),
}

/// 応答元が**今回起動した子の一時インスタンス**であることを確かめる。
///
/// 1. 子が生きている（`try_wait`）。
/// 2. `GET /api/v1/openapi.json`（認証不要。Hub の `rest.rs` の
///    `openapi_json`）の `info.x-banto-hub-profile-id` が、今回だけの一意な
///    profile ID と一致する（インストール済みの Hub の health probe
///    `http_hub_health.rs` が使うのと同じ口）。
/// 3. 専用 root の下の `profile.lock`（排他の保持者の診断ファイル）の pid が
///    子の pid と一致する。
///
/// profile ID は排他（mutex）の単位なので、2 が一致する Hub は同時に 1 つしか
/// 動けない。3 で、それが自分の子であることまで結び付ける。
async fn confirm_own_hub(admin: &HubAdmin, proc: &mut HubProcess) -> Result<u32, Identity> {
    let pid = proc.ensure_alive().map_err(Identity::Mismatch)?;
    // 照合前の相手に grant のトークンを渡さない（`HubAdmin::call` は付ける）。
    let response = admin
        .http
        .get(format!("{}/api/v1/openapi.json", admin.base))
        .send()
        .await
        .map_err(|e| Identity::NotYet(format!("openapi.json: 送信失敗: {e}")))?;
    let status = response.status().as_u16();
    let bytes = response
        .bytes()
        .await
        .map_err(|e| Identity::NotYet(format!("openapi.json: 応答を読めない: {e}")))?;
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    if status != 200 {
        return Err(Identity::NotYet(format!(
            "/api/v1/openapi.json が HTTP {status}"
        )));
    }
    let wire = body["info"]["x-banto-hub-profile-id"].as_str();
    if wire != Some(proc.profile_id.as_str()) {
        return Err(Identity::Mismatch(format!(
            "応答した Hub の profile ID が今回のもの（{}）でない: {wire:?}（別の Hub の疑い。何も操作せず中止）",
            proc.profile_id
        )));
    }
    match banto_instance_lock::read_owner_info(&proc.lock_file()) {
        Some(owner) if owner.pid == pid => {}
        Some(owner) => {
            return Err(Identity::NotYet(format!(
                "profile.lock の保持者 pid {} が子の pid {pid} でない",
                owner.pid
            )))
        }
        None => {
            return Err(Identity::NotYet(format!(
                "{} を読めない",
                proc.lock_file().display()
            )))
        }
    }
    // 照合の間に子が終わっていないこと（終わっていれば、応答したのは別物）。
    let after = proc.ensure_alive().map_err(Identity::Mismatch)?;
    if after != pid {
        return Err(Identity::Mismatch("照合中に子が入れ替わった".to_owned()));
    }
    Ok(pid)
}

/// 管理操作の直前に 1 回だけ照合する（待たない）。
async fn check_own_hub(admin: &HubAdmin, proc: &mut HubProcess) -> Result<u32, String> {
    confirm_own_hub(admin, proc).await.map_err(|e| match e {
        Identity::NotYet(m) | Identity::Mismatch(m) => format!("応答元の照合に失敗: {m}"),
    })
}

/// 起動した子が応答し、照合が通るまで待つ。**子が終了したら即中止**、別の
/// Hub が応答したら即中止（待たない）。
async fn wait_own_hub(
    admin: &HubAdmin,
    proc: &mut HubProcess,
    limit: Duration,
) -> Result<u32, String> {
    let deadline = Instant::now() + limit;
    loop {
        match confirm_own_hub(admin, proc).await {
            Ok(pid) => return Ok(pid),
            Err(Identity::Mismatch(m)) => return Err(m),
            Err(Identity::NotYet(m)) if Instant::now() >= deadline => {
                return Err(format!("一時 Hub の照合が {limit:?} 以内に通らない: {m}"))
            }
            Err(Identity::NotYet(_)) => sleep(Duration::from_millis(300)).await,
        }
    }
}

/// ポートが空いていることを確かめてから子を起動し、照合が通るまで待つ。
async fn start_own_hub(admin: &HubAdmin, proc: &mut HubProcess) -> Result<u32, String> {
    ensure_port_free(proc.port, Duration::from_secs(10)).await?;
    let pid = proc.spawn()?;
    println!(
        "  起動: pid {pid}（profile {}・root {}）",
        proc.profile_id,
        proc.root.display()
    );
    let confirmed = wait_own_hub(admin, proc, Duration::from_secs(60)).await?;
    println!(
        "  照合: 子 pid {confirmed} が 127.0.0.1:{} で応答（profile ID・profile.lock の pid 一致）",
        proc.port
    );
    Ok(confirmed)
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
            cpu_type: None,
            database: None,
            username: None,
            secrets: Default::default(),
            settings: None,
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

/// 比較表の数値列（見出し・値）。**見出しと値を 1 か所で対にする**ことで、
/// 列の順序が見出しとずれない（2026-10-11 版は見出し「Hub行 / Hubnull /
/// 直先頭」の下に `direct_leading_nulls / hub_rows / hub_nulls` を出していた）。
/// 見出しは空白を含めない（単体テストが空白区切りで照合する）。
fn pair_columns(s: &PairStats) -> [(&'static str, usize); 11] {
    [
        ("直行", s.direct_rows),
        ("直null", s.direct_nulls),
        ("直先頭", s.direct_leading_nulls),
        ("Hub行", s.hub_rows),
        ("Hubnull", s.hub_nulls),
        ("Hub先頭", s.hub_leading_nulls),
        ("直種", s.direct_distinct),
        ("Hub種", s.hub_distinct),
        ("比較", s.compared),
        ("一致", s.equal),
        ("説明不能", s.unexplained),
    ]
}

/// 比較表の行（見出し 1 行 + 組ごとに 1 行）。
fn pair_table_lines(stats: &[PairStats]) -> Vec<String> {
    let header_cols: Vec<&str> = match stats.first() {
        Some(s) => pair_columns(s).iter().map(|(label, _)| *label).collect(),
        None => return Vec::new(),
    };
    let mut lines = vec![format!(
        "  {:<6} {}  最後の値(直/Hub)",
        "tag",
        header_cols
            .iter()
            .map(|label| format!("{label:>7}"))
            .collect::<Vec<_>>()
            .join(" ")
    )];
    for s in stats {
        lines.push(format!(
            "  {:<6} {}  {}",
            s.name,
            pair_columns(s)
                .iter()
                .map(|(_, value)| format!("{value:>7}"))
                .collect::<Vec<_>>()
                .join(" "),
            s.sample
        ));
    }
    lines
}

fn print_pair_table(stats: &[PairStats]) {
    for line in pair_table_lines(stats) {
        println!("{line}");
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

/// Hub の接続キーに全区間で期待する plc_* の列。Hub の接続キーのイベントは
/// **Hub とのリンク（購読）**の切断・再接続を表す（`banto_collect::external`
/// のモジュール doc）ので、`process`（Hub のプロセスが落ちる）だけが
/// disconnected → reconnected を足す。`collection`（Hub の収集の stop/start）
/// はリンクが Live のままなので足さない。
fn expected_hub_link_kinds(outage: &str) -> Vec<String> {
    let mut kinds = vec!["plc_connected".to_owned()];
    if outage == "process" {
        kinds.push("plc_disconnected".to_owned());
        kinds.push("plc_reconnected".to_owned());
    }
    kinds
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

/// 一時 Hub の `GET /api/api-keys` の応答から、まだ失効していないキーの
/// id を取り出す。
///
/// **名前や発行時刻では絞らず、有効なキーを全部**対象にする。理由: 一時 Hub
/// は試験ごとの一意な profile・一時 DB で、キーを発行するのはこのハーネスの
/// ChronoGazer だけ（`check_own_hub` で今回の子と照合してから使う）。名前の
/// 規則（`{app_id}-{installation_id}-{秒}`）に頼ると、`connect` が同秒の
/// 衝突で秒をずらした再発行などで規則から外れたときに取りこぼす。本番の
/// Hub では絶対にやらないこと（docs/banto-hub-client-bootstrap.md）。
fn live_key_ids(list: &Value) -> Vec<i64> {
    list.as_array()
        .map(|rows| {
            rows.iter()
                .filter(|row| row["revokedAt"].is_null())
                .filter_map(|row| row["id"].as_i64())
                .collect()
        })
        .unwrap_or_default()
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
    let _ = std::fs::create_dir_all(&work);
    // 一時 Hub の root は絶対パスで渡す（子の作業ディレクトリに依存させない）。
    let work = std::path::absolute(&work).unwrap_or(work);
    println!("作業ディレクトリ: {}", work.display());
    let hub_url = format!("http://127.0.0.1:{hub_port}");

    let mut results: Vec<StepResult> = Vec::new();
    let mut hub_proc = HubProcess {
        bin: hub_bin.clone(),
        port: hub_port,
        work: work.join("hub"),
        root: work.join("hub").join("root"),
        profile_id: unique_profile_id(),
        child: None,
    };
    let _ = std::fs::create_dir_all(&hub_proc.root);
    let mut admin = HubAdmin::new(hub_url.clone());

    // 1. 一時 Hub の起動・構成 -------------------------------------------
    println!(
        "\n[1] 一時 banto-hub の起動と構成（試運転モードの grant。管理者アカウントは作らない）"
    );
    let step1 = async {
        // ポートの空き確認 → 起動 → 応答元の照合（子の終了・別の Hub は即中止）。
        // 照合が通るまで grant も構成もしない。
        start_own_hub(&admin, &mut hub_proc).await?;
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
    // 後片付け（キーの失効）に要る一式。`HubService::connect` がキーを発行した
    // **後**に手順2が失敗しても `ctx` は作られないので、作った時点でここに
    // 控え、失敗経路でも手順9が `hub.record` の `key_id` を失効させる。
    let mut cleanup_handles: Option<(sqlx::SqlitePool, SettingsService, HubService)> = None;
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
            cleanup_handles = Some((pool.clone(), settings.clone(), hub.clone()));
            // キーを発行させる前に、相手が今回の子であることを確かめ直す。
            check_own_hub(&admin, &mut hub_proc).await?;
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
                    // 自分が起動した子だけを止める（`HubProcess::kill`）。
                    hub_proc.kill();
                    Ok("プロセスを停止".to_owned())
                } else {
                    match check_own_hub(&admin, &mut hub_proc).await {
                        Ok(_) => admin.collection("stop").await,
                        Err(e) => Err(e),
                    }
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
                        // 初回と同じ: ポートの空き確認 → 起動 → 照合。
                        let pid = start_own_hub(&admin, &mut hub_proc).await?;
                        admin.grant().await?;
                        let s = admin.collection("start").await?;
                        Ok::<String, String>(format!("pid {pid} で再起動・収集開始 {s}"))
                    } else {
                        check_own_hub(&admin, &mut hub_proc).await?;
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
                // 読めなかったことを「イベント 0 件」に潰さない（collection では
                // 0 件が期待値なので、潰すと読めないまま PASS する）。
                let events = read_events(svc).await;
                let hub_conn_key = format!("conn:{}", hub_side.0);
                let direct_conn_key = format!("conn:{}", direct.0);
                let (hub_kinds, direct_kinds) = match &events {
                    Ok(events) => (
                        link_kinds(events, &hub_conn_key, down_at),
                        link_kinds(events, &direct_conn_key, down_at),
                    ),
                    Err(e) => {
                        println!("  イベントを読めない: {e}");
                        (Vec::new(), Vec::new())
                    }
                };
                println!("  停止以後のイベント: Hub {hub_kinds:?} / 直結 {direct_kinds:?}");
                // Hub の接続キーのイベントは **Hub とのリンク（購読）**の切断・
                // 再接続を表す（`banto_collect::external` のモジュール doc）。
                // - process: Hub が落ちる = リンクが落ちる → disconnected → reconnected。
                // - collection: Hub は生きていてリンクは Live のまま、値だけが
                //   Bad になる → イベントは出ない（設計どおり。2026-10-11 の P6
                //   実機で確認）。
                let expected_hub: Vec<String> = expected_hub_link_kinds(&outage)
                    .into_iter()
                    .skip(1)
                    .collect();
                let events_ok =
                    events.is_ok() && hub_kinds == expected_hub && direct_kinds.is_empty();
                let events_note = match &events {
                    Ok(_) => format!("イベント Hub {hub_kinds:?} 直結 {direct_kinds:?}"),
                    Err(e) => format!("イベントを読めない（判定不能）: {e}"),
                };
                results.push(StepResult::new(
                    "5. Hub 停止・再開",
                    stop.is_ok() && went_bad && direct_good_during && recovered && events_ok,
                    format!(
                        "停止={} / Bad+null={went_bad} / 直結 good={direct_good_during} / 復帰={recovered} / {events_note}",
                        stop.is_ok()
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
                // **全区間**の plc_* を期待の列と完全一致で比べる（定常区間だけに
                // 絞ると、停止・再開の区間の予期しないイベント - 直結の切断や
                // Hub の余分な再接続 - を見逃す）。
                let expected_hub = expected_hub_link_kinds(&outage);
                let expected_direct = vec!["plc_connected".to_owned()];
                // 両接続のどちらでもない plc_* も予期しない。
                let stray: Vec<String> = events
                    .iter()
                    .filter(|r| {
                        r.kind.starts_with("plc_")
                            && r.connection_key.as_deref() != Some(hub_conn_key.as_str())
                            && r.connection_key.as_deref() != Some(direct_conn_key.as_str())
                    })
                    .map(|r| format!("{}({:?})", r.kind, r.connection_key))
                    .collect();
                let ok = hub_kinds == expected_hub
                    && direct_kinds == expected_direct
                    && stray.is_empty();
                results.push(StepResult::new(
                    "8. イベント（全区間の plc_* が期待どおり）",
                    ok,
                    format!(
                        "Hub {hub_kinds:?}（期待 {expected_hub:?}）/ 直結 {direct_kinds:?}（期待 {expected_direct:?}）/ その他 {stray:?}"
                    ),
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
    // 手順2の途中で失敗しても（`ctx` が無くても）、キーが発行されていれば
    // 失効させる（`cleanup_handles` の doc）。
    if let Some((pool, settings, hub)) = &cleanup_handles {
        let record = read_hub_record(settings).await;
        hub.shutdown().await;
        match record {
            Some(record) if record.endpoint.trim_end_matches('/') == hub_url => {
                if let Some(key_id) = record.key_id {
                    let revoked = async {
                        // 失効を送る前に、相手が今回の子であることを確かめる
                        // （止まっていれば、別の Hub に送らずに失敗にする）。
                        check_own_hub(&admin, &mut hub_proc).await?;
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
        // `hub.record` に `key_id` が残らない経路（`connect` が発行後に
        // 保存まで進めず Err・タイムアウトで落ちた等）の取りこぼしを、
        // 一時 Hub 上の有効なキー全部の失効で塞ぐ（`live_key_ids` の doc）。
        let swept = async {
            check_own_hub(&admin, &mut hub_proc).await?;
            if admin.token.is_none() {
                admin.grant().await?;
            }
            let list = admin
                .ok(reqwest::Method::GET, "/api/api-keys", None)
                .await?;
            let ids = live_key_ids(&list);
            let mut done = Vec::new();
            for id in ids {
                check_own_hub(&admin, &mut hub_proc).await?;
                admin
                    .ok(
                        reqwest::Method::POST,
                        &format!("/api/api-keys/{id}/revoke"),
                        None,
                    )
                    .await?;
                done.push(id);
            }
            Ok::<_, String>(done)
        }
        .await;
        match swept {
            Ok(ids) if ids.is_empty() => {}
            Ok(ids) => cleanup_notes.push(format!("一覧から残りの有効キー {ids:?} を失効")),
            Err(e) => {
                cleanup_ok = false;
                cleanup_notes.push(format!("キー一覧による失効に失敗: {e}"));
            }
        }
        pool.close().await;
    }
    // grant のトークンは今回の子にだけ返す（照合できなければ送らずに捨てる）。
    if check_own_hub(&admin, &mut hub_proc).await.is_ok() {
        admin.logout().await;
    } else {
        admin.token = None;
    }
    if hub_proc.child.is_some() {
        hub_proc.kill();
        cleanup_notes.push("一時 Hub を停止".to_owned());
    } else {
        cleanup_notes.push("一時 Hub は動いていない（起動していない・停止済み）".to_owned());
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn stats_with_distinct_values() -> PairStats {
        PairStats {
            name: "d3000".to_owned(),
            direct_rows: 11,
            direct_nulls: 12,
            direct_leading_nulls: 13,
            hub_rows: 14,
            hub_nulls: 15,
            hub_leading_nulls: 16,
            direct_distinct: 17,
            hub_distinct: 18,
            compared: 19,
            equal: 20,
            unexplained: 21,
            sample: "100 / 100".to_owned(),
        }
    }

    /// 見出しの各列の下に、その見出しが指すフィールドの値が出る
    /// （#584 のレビュー: 「Hub行 / Hubnull / 直先頭」の下に別の値が出ていた）。
    #[test]
    fn pair_table_values_sit_under_their_headers() {
        let lines = pair_table_lines(&[stats_with_distinct_values()]);
        assert_eq!(lines.len(), 2);
        let header: Vec<&str> = lines[0].split_whitespace().collect();
        let row: Vec<&str> = lines[1].split_whitespace().collect();
        let value_under = |label: &str| -> &str {
            let i = header
                .iter()
                .position(|h| *h == label)
                .unwrap_or_else(|| panic!("見出し {label} が無い: {header:?}"));
            row[i]
        };
        assert_eq!(value_under("tag"), "d3000");
        for (label, expected) in [
            ("直行", "11"),
            ("直null", "12"),
            ("直先頭", "13"),
            ("Hub行", "14"),
            ("Hubnull", "15"),
            ("Hub先頭", "16"),
            ("直種", "17"),
            ("Hub種", "18"),
            ("比較", "19"),
            ("一致", "20"),
            ("説明不能", "21"),
        ] {
            assert_eq!(value_under(label), expected, "列 {label}");
        }
        assert!(lines[1].ends_with("100 / 100"));
    }

    #[test]
    fn live_key_ids_skips_revoked_and_malformed_rows() {
        let list = serde_json::json!([
            {"id": 1, "name": "a", "revokedAt": null},
            {"id": 2, "name": "b", "revokedAt": "2026-10-12 01:02:03"},
            {"name": "no-id", "revokedAt": null},
            {"id": 7, "name": "c"}
        ]);
        assert_eq!(live_key_ids(&list), vec![1, 7]);
        assert!(live_key_ids(&serde_json::json!({"error": "x"})).is_empty());
        assert!(live_key_ids(&Value::Null).is_empty());
    }

    #[test]
    fn pair_table_is_empty_without_stats() {
        assert!(pair_table_lines(&[]).is_empty());
    }

    /// profile ID は banto-hub の検証（空・パス区切り・`.`/`..` を拒否し、
    /// 拒否すると黙って `default` に戻す）を通る形で、`default` ではない。
    #[test]
    fn unique_profile_id_is_a_valid_non_default_id() {
        let id = unique_profile_id();
        assert!(id.starts_with("p6-"));
        assert_ne!(id, "default");
        assert!(id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
    }

    /// 既にリスナーがいるポートでは、起動前の確認が即座に中止を返す。
    #[tokio::test]
    async fn ensure_port_free_refuses_an_occupied_port() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let started = Instant::now();
        let result = ensure_port_free(port, Duration::from_secs(10)).await;
        assert!(result.is_err(), "{result:?}");
        assert!(started.elapsed() < Duration::from_secs(5));
        drop(listener);
        assert!(ensure_port_free(port, Duration::from_secs(10))
            .await
            .is_ok());
    }
}
