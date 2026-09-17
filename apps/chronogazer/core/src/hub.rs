//! Hub 接続（#332 chronogazer 分）: `banto-hub-bootstrap` をこのアプリの
//! 設定ストア・OS キーリングに配線するサービス層。
//!
//! chronogazer はこれまで banto-hub に接続するコードを一切持っていなかった
//! （PLC 直結の独立アプリで、設定画面の「接続」は自アプリの LAN 公開設定）。
//! したがってここは「既存接続の認証自動化」ではなく **Hub 接続機能の新規
//! 実装**にあたる。
//!
//! # 役割分担
//!
//! * 発行規則・6 状態・スコープのホワイトリストは
//!   [`banto_hub_bootstrap`] 側にある（アプリ非依存）。このモジュールは
//!   「どこに保存するか」だけを与える。
//! * 平文の API キーは **OS キーリングだけ**に入る（[`KeyStore`]）。設定 KV
//!   には [`HubRecord`] の JSON（接続先・キー ID・キー名・キーリングの
//!   アカウント名・選択タグ）しか書かない - この型にキー欄が無いことが、
//!   「設定 DB に平文が無い」ことの根拠になる。
//! * **購読（`TagClientHandle`/`start`）はこちらの仕事**（#383 段階1）。
//!   bootstrapper は [`Bootstrapper::rest_client`] で認証済みクライアントを
//!   渡すだけで、世代は所有しない。
//!
//! # 購読の世代（#383 段階1）
//!
//! **1 接続 = 1 世代**。世代の同一性は「接続先 + 購読するタグ集合（名前と
//! stable ID の組）」（[`Generation::fingerprint`]）で決める。これが一致し、
//! かつ資格情報が変わっていない限り [`HubService::reconcile`] は**何もしない**。
//! 設定画面が [`HubService::status`] をポーリングするたびに WS を張り直すのを
//! 防ぐ、この節で一番大事な不変条件。
//!
//! そのほかの規律:
//!
//! * 選んだタグのうち catalog に無いもの（Hub から消えた／権限で見えない）
//!   は `unresolved`、購読プロトコルが受け付けない綴り（カンマ入り・空白
//!   だけ）は `unsupported` に出し、**残りだけで購読する**。1 個の事故で
//!   購読全体を殺さない・空表示に潰さない。
//! * **購読の失敗で [`HubStatus`] の 6 状態を変えない**。接続設定の状態と
//!   購読の状態は別物で、購読が張れない理由は
//!   [`HubSubscriptionView::reason`] に出す。
//! * `banto-serve`（[`UnavailableKeyStore`]）は keyring を持てないので
//!   [`Bootstrapper::rest_client`] が `None` を返し、購読を張れない。
//!   これはエラーではなく、理由付きの「停止」として表示する。
//! * **値が二度と流れない状態を作らない**: 1 本の常駐タスク（supervisor）が
//!   30 秒ごとに「世代が無い／`Unauthorized`／`Rebinding`」だけを拾って
//!   張り直す。詳しくは [`HubService::spawn_supervisor`]。
//!
//! # 同期 trait と非同期設定ストアの橋渡し
//!
//! [`BootstrapState`] は意図的に同期 trait（crate 側の設計、`KeyStore` が
//! 元々ブロッキングな OS 呼び出しであることに合わせている）だが、
//! [`SettingsService`] は sqlx で非同期。そこで [`SettingsMirror`] という
//! 小さなインメモリの写しを置き、[`HubService`] が bootstrapper を呼ぶ
//! **前に設定から hydrate し、後に変更があれば flush する**。
//! `block_on`/`block_in_place` は使わない（tauri のコマンドは既に非同期
//! ランタイム上で走っているため、同じランタイムで待つと panic する）。

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use banto_core::BantoError;
use banto_hub_bootstrap::{
    error::{Error as BootstrapError, ErrorKind as BootstrapErrorKind},
    BootstrapState, Bootstrapper, HubConnection, HubRecord, HubStatus, KeyStore,
};
use banto_tagclient::{
    BindingRequest, CatalogSnapshot, CatalogTag, Endpoint, StableTagId, TagClientConnectionState,
    TagClientHandle, TagClientState, ValuesSnapshot,
};
use serde::Serialize;
use tokio::sync::{watch, Mutex as AsyncMutex, MutexGuard as AsyncMutexGuard};

use crate::settings::SettingsService;

/// `banto-hub` が発行するキー名の先頭に付くアプリ識別子。
pub const APP_ID: &str = "chronogazer";

/// 設定 KV のキー。`hub.record` は [`HubRecord`] の JSON、
/// `hub.installation_id` はこのインストールの一意 ID。
const KEY_HUB_RECORD: &str = "hub.record";
const KEY_HUB_INSTALLATION_ID: &str = "hub.installation_id";

/// 画面に出す 1 タグ分（`GET /api/v1/tags` の catalog 行のうち、選択 UI に
/// 必要な列だけ）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HubTagView {
    pub external_name: String,
    pub name: String,
    pub data_type: String,
    pub unit: Option<String>,
    pub tag_kind: String,
}

impl From<&CatalogTag> for HubTagView {
    fn from(tag: &CatalogTag) -> Self {
        Self {
            external_name: tag.external_name.clone(),
            name: tag.name.clone(),
            data_type: tag.data_type.clone(),
            unit: tag.unit.clone(),
            tag_kind: tag.tag_kind.clone(),
        }
    }
}

/// Hub 接続画面が 1 回の往復で必要とするものすべて。`status` は
/// `banto-hub-bootstrap` の 6 状態の判別共用体をそのまま載せる
/// （`{"state":"connected","tagCount":0}` 等）。
///
/// `tags` は catalog を**実際に読めたときだけ** `Some`。読めなかったときに
/// 空配列を返すと「タグ 0 件で接続済み」と区別が付かなくなるため、
/// `null` と空配列を意図的に別物にしている（受入条件）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HubView {
    pub status: HubStatus,
    pub endpoint: Option<String>,
    pub key_name: Option<String>,
    pub selected_tags: Vec<String>,
    pub tags: Option<Vec<HubTagView>>,
    /// 購読の状態（#383 段階1）。`status` の 6 状態とは**別軸**で、購読が
    /// 張れなくても 6 状態は汚れない。
    pub subscription: HubSubscriptionView,
}

impl HubView {
    fn new(
        status: HubStatus,
        record: Option<&HubRecord>,
        tags: Option<Vec<HubTagView>>,
        subscription: HubSubscriptionView,
    ) -> Self {
        Self {
            status,
            endpoint: record.map(|record| record.endpoint.clone()),
            key_name: record.and_then(|record| record.key_name.clone()),
            selected_tags: record
                .map(|record| record.selected_tags.clone())
                .unwrap_or_default(),
            tags,
            subscription,
        }
    }
}

/// 画面に出す 1 値分（`TagClientState::current()` の `ValuesSnapshot` の
/// 1 行）。`v` が `null` なのは「値がまだ無い」であって 0 ではない。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HubValueView {
    pub tag: String,
    pub v: Option<f64>,
    /// `good|stale|bad`、または Hub が送ってきた未知のラベルそのまま
    /// （`banto-tagclient` が `Unknown(raw)` を保つのと同じで、知らない品質を
    /// `good` に丸めない）。
    pub q: String,
    pub t: i64,
    /// `real|simulation|computed|...`、または未知のラベルそのまま。
    pub value_source: String,
}

/// 購読の状態（#383 段階1）。**ネットワークを叩かずに**組み立てられる
/// （`watch::Receiver` を読むだけ）ので、設定画面のポーリングはこれを見る。
///
/// `values` が入るのは `state == "live"` のときだけ:
/// [`TagClientState::current`] は Live 以外で `None` を返す仕様なので、
/// その性質にそのまま乗る（**Live でないのに古い値を出さない**）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HubSubscriptionView {
    /// `stopped|connecting|handshaking|live|rebinding|reconnecting|unauthorized`
    /// （[`TagClientConnectionState`] の `Display` と同じ綴り）。
    pub state: &'static str,
    /// 停止している理由（タグ未選択 / キーリング不可 / 未接続など）。
    /// 世代を持っているときは `None`。
    pub reason: Option<String>,
    pub subscribed_count: usize,
    /// 選んだのに catalog に無かった external name（Hub から消えた／権限で
    /// 見えない）。**空表示に潰さない**。
    pub unresolved: Vec<String>,
    /// 購読プロトコルが受け付けない綴りの external name（カンマ入り・空白
    /// だけ）。`unresolved` とは**理由が違う**ので混ぜない。
    pub unsupported: Vec<String>,
    /// [`TagClientState::last_error`] の分類名（`ErrorKind::as_str`）。
    pub last_error: Option<String>,
    /// 最後に観測した [`ValuesSnapshot::t`]。**`Live` を離れても消えない**
    /// （`banto-tagclient` の `current()` と違い、こちらで覚えている）。
    /// 更新の粒度は最大 30 秒 - 理由は `Subscription::observe_last_value`。
    pub last_value_at: Option<i64>,
    pub values: Vec<HubValueView>,
}

impl HubSubscriptionView {
    /// 世代を持っていないときの形。`unresolved` / `unsupported` は理由と
    /// 独立に出す。`last_value_at` も残す - 止まっていても「いつまで受けて
    /// いたか」は有用な情報。
    fn stopped(
        reason: Option<String>,
        unresolved: Vec<String>,
        unsupported: Vec<String>,
        last_value_at: Option<i64>,
    ) -> Self {
        Self {
            state: "stopped",
            reason,
            subscribed_count: 0,
            unresolved,
            unsupported,
            last_error: None,
            last_value_at,
            values: Vec::new(),
        }
    }
}

/// [`BootstrapState`] のインメモリ実装。[`HubService`] が設定ストアとの
/// 間を往復させる（モジュール doc の「橋渡し」節）。
#[derive(Debug, Default)]
struct SettingsMirror {
    inner: Mutex<MirrorInner>,
}

#[derive(Debug, Default)]
struct MirrorInner {
    record: Option<HubRecord>,
    dirty: bool,
}

impl SettingsMirror {
    /// 設定から読んだ値で写しを置き換える（dirty はここでクリアする）。
    fn reset(&self, record: Option<HubRecord>) {
        let mut inner = self.inner.lock().expect("hub mirror poisoned");
        inner.record = record;
        inner.dirty = false;
    }

    /// 変更があれば「書き戻すべき値」を返す（`Some(None)` = 消去）。
    fn take_dirty(&self) -> Option<Option<HubRecord>> {
        let mut inner = self.inner.lock().expect("hub mirror poisoned");
        if !inner.dirty {
            return None;
        }
        inner.dirty = false;
        Some(inner.record.clone())
    }

    fn current(&self) -> Option<HubRecord> {
        self.inner
            .lock()
            .expect("hub mirror poisoned")
            .record
            .clone()
    }
}

impl BootstrapState for SettingsMirror {
    fn load(&self) -> Result<Option<HubRecord>, BootstrapError> {
        Ok(self.current())
    }

    fn save(&self, record: &HubRecord) -> Result<(), BootstrapError> {
        let mut inner = self.inner.lock().expect("hub mirror poisoned");
        inner.record = Some(record.clone());
        inner.dirty = true;
        Ok(())
    }

    fn clear(&self) -> Result<(), BootstrapError> {
        let mut inner = self.inner.lock().expect("hub mirror poisoned");
        inner.record = None;
        inner.dirty = true;
        Ok(())
    }
}

/// OS キーリングが無い実行形態（`banto-serve`: Tauri 非依存の開発・E2E
/// 用サーバー）向けの [`KeyStore`]。
///
/// 読み出しは「エントリ無し」を返し、書き込みは**失敗する** - 平文の API
/// キーを設定 DB やファイルに落とす代替経路を作らないため。この実行形態
/// でも「到達不能な Hub へ接続を試みて `Unreachable` になる」「ロック
/// ダウン済みで `NeedsPairing` になる」といった、キーを保存しない範囲の
/// 動作はそのまま確認できる。
#[derive(Debug, Default)]
pub struct UnavailableKeyStore;

impl KeyStore for UnavailableKeyStore {
    fn get(&self, _account: &str) -> Result<Option<String>, BootstrapError> {
        Ok(None)
    }

    fn set(&self, _account: &str, _secret: &str) -> Result<(), BootstrapError> {
        Err(BootstrapError::with_detail(
            BootstrapErrorKind::KeyStore,
            "この実行形態では OS キーリングを利用できません（デスクトップアプリから設定してください）",
        ))
    }

    fn delete(&self, _account: &str) -> Result<(), BootstrapError> {
        Ok(())
    }
}

/// `banto-hub-bootstrap` のエラーを、このアプリの境界型へ。
///
/// `detail` は crate 側で「鍵・エンドポイントのパスを含めない」と規定されて
/// いるものだけが入る（[`KeyStore`] 実装が返す「キーリングが使えない理由」
/// など）ので、そのまま画面に出してよい。
fn to_banto_error(error: BootstrapError) -> BantoError {
    let detail = error.detail().unwrap_or_default().to_string();
    match error.kind() {
        BootstrapErrorKind::InvalidEndpoint => BantoError::Validation {
            field_errors: vec![banto_core::FieldError {
                field: "endpoint".to_string(),
                message: "接続先は http://ホスト:ポート の形式で入力してください".to_string(),
            }],
        },
        BootstrapErrorKind::InvalidKey => BantoError::Validation {
            field_errors: vec![banto_core::FieldError {
                field: "key".to_string(),
                message: "APIキーの形式が正しくありません".to_string(),
            }],
        },
        BootstrapErrorKind::ForbiddenScope => BantoError::Other(format!(
            "このスコープの自動発行は許可されていません: {detail}"
        )),
        BootstrapErrorKind::KeyStore => BantoError::Other(if detail.is_empty() {
            "OSキーリングを利用できません".to_string()
        } else {
            detail
        }),
        BootstrapErrorKind::State => {
            BantoError::Storage(format!("Hub接続設定の保存に失敗しました: {detail}"))
        }
        BootstrapErrorKind::NotConfigured => {
            BantoError::Other("Hubへの接続がまだ設定されていません".to_string())
        }
        // `ErrorKind` は `#[non_exhaustive]`。将来 crate 側に分類が増えた
        // ときも、既知の分類の文言を取り違えるより「不明」として出す。
        _ => BantoError::Other(format!("Hubへの接続に失敗しました: {error}")),
    }
}

// --- #383 段階1: 購読の世代 --------------------------------------------------

/// 購読を張らない理由（[`HubSubscriptionView::reason`]）。接続の 6 状態と
/// 混ざらないよう、購読側だけの語彙にしている。
const REASON_NOT_STARTED: &str = "購読の状態をまだ確認していません。";
const REASON_NOT_CONFIGURED: &str = "Hubへの接続が設定されていません。";
const REASON_NOT_CONNECTED: &str = "Hubに接続できていないため購読していません。";
const REASON_NO_TAGS: &str = "購読するタグが選ばれていません。";
const REASON_ALL_UNRESOLVED: &str =
    "選んだタグがHubのタグ一覧に見つからないため、購読できるタグがありません。";
const REASON_ALL_UNSUPPORTED: &str =
    "選んだタグの名前を購読プロトコルが受け付けないため、購読できるタグがありません。";
const REASON_NONE_SUBSCRIBABLE: &str =
    "選んだタグはHubのタグ一覧に無いか、購読プロトコルが受け付けない名前のため、購読できるタグがありません。";
const REASON_NO_KEY: &str =
    "保存済みのAPIキーを取り出せないため購読できません（デスクトップアプリから接続し直してください）。";
/// 選択は保存できたが、直後の catalog 再取得に失敗した状態。**保存は成功
/// している**ことと、**放っておいても見張りが張り直す**ことが伝わる文言に
/// する（ユーザーに再操作を要求しない）。
const REASON_SELECTION_CHANGED_REFRESH_FAILED: &str =
    "選択を保存しましたが、Hubのタグ一覧を取り直せませんでした。まもなく自動で再試行します。";

/// 購読プロトコル（`banto-tagclient` の `stream_core::validate_tag_selection`）
/// が受け付けない external name か。
///
/// 購読要求はタグ名をカンマ区切りで並べるため、**名前自体にカンマを含める
/// ことができない**。空白だけの名前も同様に拒否される。`RestClient::start`
/// はこの検査をしない（重複と空だけを見る）ので、1 件混ざると**ワーカーが
/// 毎回 `InvalidTagSelection` で失敗し、購読全体が死ぬ**（retryable でも
/// rebindable でもない）。したがってアプリ側で先に落とし、**残りのタグは
/// 購読する**。
fn is_unsupported_tag_name(name: &str) -> bool {
    name.trim().is_empty() || name.contains(',')
}

/// [`plan_bindings`] の結果: 実際に購読する要求と、購読できなかった名前を
/// **理由別に**分けたもの。`unresolved`（Hub から消えた／権限で見えない）と
/// `unsupported`（購読プロトコルが受け付けない綴り）は次の一手が違うので
/// 混ぜない。
#[derive(Debug, Clone, PartialEq, Eq)]
struct BindingPlan {
    requests: Vec<BindingRequest>,
    unresolved: Vec<String>,
    unsupported: Vec<String>,
}

/// 選択タグ（external name）を catalog と突き合わせ、購読要求・未解決・
/// 購読不可に分ける**純関数**（ネットワークも状態も触らないので、分岐を
/// テストで固定できる）。
///
/// * `binding_key` は external name をそのまま使う - 画面の一覧と 1:1 に
///   対応させ、返ってきた値をそのまま行に載せられるようにするため。
/// * 購読プロトコルが受け付けない綴り（[`is_unsupported_tag_name`]）は
///   catalog を引く前に `unsupported` へ落とす。catalog にあっても購読は
///   できないので、「消えた」とは別の事実として扱う。
/// * catalog に無い external name は `unresolved` に入れる。
/// * どちらも**残りだけで購読する**。1 個の事故で購読全体を殺さない。
/// * 重複する external name はここで 1 つに畳む
///   （`resolve_bindings`/`start` は重複 `binding_key` / 重複 `stable_id` を
///   エラーにするため、通す前に潰しておく）。
/// * `requests` が空なら購読しない（`start()` は空 requests を
///   `InvalidTagSelection` で拒否する）。
fn plan_bindings(selected: &[String], catalog: &CatalogSnapshot) -> BindingPlan {
    let by_name: HashMap<&str, &CatalogTag> = catalog
        .tags
        .iter()
        .map(|tag| (tag.external_name.as_str(), tag))
        .collect();
    let mut seen: HashSet<&str> = HashSet::with_capacity(selected.len());
    let mut requests = Vec::with_capacity(selected.len());
    let mut unresolved = Vec::new();
    let mut unsupported = Vec::new();
    for name in selected {
        if !seen.insert(name.as_str()) {
            continue;
        }
        if is_unsupported_tag_name(name) {
            unsupported.push(name.clone());
            continue;
        }
        match by_name.get(name.as_str()) {
            Some(tag) => requests.push(BindingRequest {
                binding_key: name.clone(),
                stable_id: tag.ids,
            }),
            None => unresolved.push(name.clone()),
        }
    }
    BindingPlan {
        requests,
        unresolved,
        unsupported,
    }
}

/// 世代の同一性。**接続先（正規化済み）+ 購読するタグ集合**で、これが一致
/// するなら張り直さない。
///
/// タグ集合は名前だけでなく **stable ID の組**（名前でソート）にする:
/// Hub 側でタグを消して同じ名前で作り直すと `StableTagId` が変わるが、名前
/// しか見ないと fingerprint が一致してしまい、**古い ID で購読し続けて
/// unresolved になったまま復帰しない**（Copilot F4）。
type Fingerprint = (String, Vec<(String, StableTagId)>);

/// [`plan_bindings`] の要求から [`Fingerprint`] のタグ部分を作る（名前で
/// ソートして、選択の並び替えだけで世代が入れ替わらないようにする）。
fn fingerprint_tags(requests: &[BindingRequest]) -> Vec<(String, StableTagId)> {
    let mut tags: Vec<(String, StableTagId)> = requests
        .iter()
        .map(|request| (request.binding_key.clone(), request.stable_id))
        .collect();
    tags.sort_by(|left, right| left.0.cmp(&right.0));
    tags
}

/// 接続先を比較可能な形に正規化する。
///
/// `banto_tagclient::Endpoint` が `banto-hub-bootstrap` と同じ正規化
/// （末尾スラッシュの吸収など）を行うので、そこから導いた不変の URL を鍵に
/// する。`Endpoint` は正規化後の文字列そのものを公開していないため、
/// 決定的に導ける `tags_url()` を使う（値そのものに意味は無く、同じ接続先が
/// 同じ文字列になることだけが要件）。解釈できない綴りのときは入力をそのまま
/// 使う - 「同じなら同じ、違うなら違う」が保てればよい。
fn fingerprint_endpoint(endpoint: &str) -> String {
    Endpoint::new(endpoint)
        .map(|parsed| parsed.tags_url().to_string())
        .unwrap_or_else(|_| endpoint.trim().to_owned())
}

/// 1 つの購読世代。`TagClientHandle` を所有し、その `watch` を読むだけで
/// 状態を出せる。
struct Generation {
    handle: TagClientHandle,
    states: watch::Receiver<TagClientState>,
    fingerprint: Fingerprint,
    started_at: i64,
    /// このサービスが何本目に張った世代か（1 始まり）。ログと、テストが
    /// 「張り直したか／据え置いたか」を見るための観測点。`fingerprint` は
    /// 同じでも張り直すことがある（資格情報の変更・終端状態）ので、同一性の
    /// 比較ではこれを使わない。
    sequence: usize,
}

/// 現世代の [`TagClientState`] のうち、張り直しの判断に使う部分だけ。
/// 判断を純関数に保つための小さな写し。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GenerationHealth {
    state: TagClientConnectionState,
    /// `last_error` があるか（**どの分類か**は判断に使わない - ここで見たい
    /// のは「エラーで終わったのか、まだ／もう何も起きていないのか」だけ）。
    failed: bool,
}

/// 世代が**終端している**（自力では二度と復帰しない）状態か。
///
/// `banto-tagclient` のワーカーは 2 通りの終わり方をする:
///
/// * `Unauthorized` … 401/403。単発の終端失敗で、再試行しない。
/// * `Stopped` + `last_error` あり … retryable でも rebindable でもない
///   分類（`InvalidTagSelection` など）でワーカーが**終了した**形。
///   **世代（と `TagClientHandle`）は残ったまま**なので、「世代が無い」条件
///   では拾えない。
///
/// どちらもこちらが張り直さない限り値は二度と流れない。
///
/// `last_error` の有無まで見るのは、**起動直後の一瞬も `Stopped`** だから:
/// `start()` した世代は最初の状態が `Stopped`（`last_error` 無し）で、
/// すぐに `Connecting` へ移る。ここを終端扱いにすると、その瞬間に
/// `status()` が走っただけで張り直してしまい、「`status()` のたびに WS を
/// 張り直さない」という一番大事な不変条件が壊れる。正常停止
/// （`shutdown()`）も `last_error` 無しの `Stopped` なので、同じ判定で
/// 区別できる。
fn is_terminal(health: GenerationHealth) -> bool {
    match health.state {
        TagClientConnectionState::Unauthorized => true,
        TagClientConnectionState::Stopped => health.failed,
        _ => false,
    }
}

/// 同一性（[`Fingerprint`]）が一致しているのに、それでも張り直すべきか。
///
/// 純関数にしてあるのは、ここが**値が二度と流れない状態を作らない**ための
/// 判断そのものだから:
///
/// * [`Trigger::CredentialsChanged`] … `connect` / `adopt_manual_key` の後。
///   キーが増えた／差し替わったので、同じ接続先・同じタグでも新しいキーで
///   張り直さないと意味が無い。
/// * 現世代が終端している（[`is_terminal`]）… 放っておくと復帰しない。
///   同一性が同じでも張り直す（ダメならまた同じ終端状態になるだけ）。
///   これが無いと、見張りが再試行しても同一性一致で no-op になり、止まった
///   世代が居座り続ける。
fn must_restart_despite_same_fingerprint(
    trigger: Trigger,
    health: Option<GenerationHealth>,
) -> bool {
    trigger == Trigger::CredentialsChanged || health.is_some_and(is_terminal)
}

/// 見張り（[`HubService::spawn_supervisor`]）の 1 周期で再試行すべきか。
/// 判断表はそちらの doc comment にある。純関数なのでテストで固定できる。
fn needs_retry(health: Option<GenerationHealth>) -> bool {
    match health {
        // 世代が無い: まだ／もう張れていない。
        None => true,
        // 終端している（`Unauthorized` / エラーで終わった `Stopped`）:
        // 放っておくと戻らない。
        // `Rebinding`: requests が catalog と合っておらず再計画でしか直らない。
        Some(health) => is_terminal(health) || health.state == TagClientConnectionState::Rebinding,
    }
}

/// [`HubService`] が持つ購読スロット。
///
/// 指示書の素案は `Option<Generation>` だったが、**未解決タグと停止理由は
/// 世代が無いときにこそ画面に出す**必要がある（選んだタグが全部 catalog から
/// 消えると `requests` が空になり世代を持てないが、そのとき何が消えたのかを
/// 出さないと「タグ 0 件」に潰れてしまう）。そこで世代と並べて 1 つの
/// ミューテックスに入れ、両者が食い違わないようにしている。
struct Subscription {
    generation: Option<Generation>,
    /// 世代を持っていない理由。持っているときは `None`。
    reason: Option<String>,
    /// 直近に catalog と突き合わせた結果の未解決タグ。
    unresolved: Vec<String>,
    /// 直近の突き合わせで購読プロトコルに弾かれた名前。
    unsupported: Vec<String>,
    /// 最後に観測した [`ValuesSnapshot::t`]（「いつまで値を受けていたか」）。
    ///
    /// **ここに覚えておく必要がある**: `banto-tagclient` は `Live` を離れると
    /// `current()` を捨てるので、そこから導くと再接続・再バインドに入った
    /// 瞬間に消えてしまう。まさに「最後に受けた時刻」を知りたい場面で消える
    /// ということなので、観測したら保持する。世代を張り直したらリセットし
    /// （別の購読の話になるため）、止まっただけなら残す。
    last_value_at: Option<i64>,
}

/// 起動直後（まだ一度も突き合わせていない）も「理由付きの停止」で表現する。
/// `reason` の無い停止は「理由を出し忘れている」ことにしたいので、初期値
/// にも理由を入れておく。
impl Default for Subscription {
    fn default() -> Self {
        Self {
            generation: None,
            reason: Some(REASON_NOT_STARTED.to_owned()),
            unresolved: Vec::new(),
            unsupported: Vec::new(),
            last_value_at: None,
        }
    }
}

impl Subscription {
    /// 既存の世代を止める。エラーは best effort でログのみ - 購読の後始末で
    /// 接続の状態を壊さない。
    async fn stop(&mut self, reason: Option<String>) {
        if let Some(generation) = self.generation.take() {
            let (sequence, started_at) = (generation.sequence, generation.started_at);
            if let Err(err) = generation.handle.shutdown().await {
                eprintln!(
                    "banto: Hubの購読の停止に失敗しました（第{sequence}世代 / started_at={started_at}）: {}",
                    err.kind().as_str()
                );
            }
        }
        self.reason = reason;
    }

    /// 現世代の `watch` を覗き、live なスナップショットがあれば
    /// [`Self::last_value_at`] を進める。
    ///
    /// `banto-tagclient` は `Live` のときだけ `current()` を返すので、
    /// **見えているうちに記録しておく**しかない。世代ごとに `watch` を
    /// 購読する専用タスクを立てれば取りこぼしは無くなるが、タスクを増やさず
    /// にこの表示要件は満たせる: 呼ぶのは (1) 画面を開いている間の 2 秒
    /// ポーリング（[`Self::view`]）と (2) 見張りの各周期（30 秒）なので、
    /// **更新の粒度は最大 30 秒**。「最後に値を受けたのはいつか」という
    /// 用途にはその精度で足りる。
    fn observe_last_value(&mut self) {
        let Some(generation) = self.generation.as_ref() else {
            return;
        };
        let observed = generation.states.borrow().current().map(|s| s.t);
        self.last_value_at = advance_last_value_at(self.last_value_at, observed);
    }

    fn view(&mut self) -> HubSubscriptionView {
        self.observe_last_value();
        let last_value_at = self.last_value_at;
        let Some(generation) = self.generation.as_ref() else {
            return HubSubscriptionView::stopped(
                self.reason.clone(),
                self.unresolved.clone(),
                self.unsupported.clone(),
                last_value_at,
            );
        };
        let state = generation.states.borrow();
        HubSubscriptionView {
            state: connection_state_str(state.connection_state()),
            reason: None,
            subscribed_count: generation.fingerprint.1.len(),
            unresolved: self.unresolved.clone(),
            unsupported: self.unsupported.clone(),
            last_error: state.last_error().map(|kind| kind.as_str().to_owned()),
            last_value_at,
            values: state.current().map(value_views).unwrap_or_default(),
        }
    }

    /// 現世代の健康状態（世代が無ければ `None`）。
    fn health(&self) -> Option<GenerationHealth> {
        self.generation.as_ref().map(|generation| {
            let state = generation.states.borrow();
            GenerationHealth {
                state: state.connection_state(),
                failed: state.last_error().is_some(),
            }
        })
    }
}

/// 「最後に値を受けた時刻」を観測結果で進める（純関数）。
///
/// `observed` が `None` なのは「今は live なスナップショットが無い」＝
/// **`Live` を離れた**という意味で、そのときは**覚えている値をそのまま残す**
/// （`banto-tagclient` の `current()` をそのまま出すと、まさに知りたい場面で
/// `null` になってしまう）。進めるときは単調に - 巻き戻る `t` を受けても
/// 「最後に受けた時刻」を過去に戻さない。
fn advance_last_value_at(current: Option<i64>, observed: Option<i64>) -> Option<i64> {
    match (current, observed) {
        (last, None) => last,
        (None, Some(t)) => Some(t),
        (Some(last), Some(t)) => Some(last.max(t)),
    }
}

fn connection_state_str(state: TagClientConnectionState) -> &'static str {
    match state {
        TagClientConnectionState::Stopped => "stopped",
        TagClientConnectionState::Connecting => "connecting",
        TagClientConnectionState::Handshaking => "handshaking",
        TagClientConnectionState::Live => "live",
        TagClientConnectionState::Rebinding => "rebinding",
        TagClientConnectionState::Reconnecting => "reconnecting",
        TagClientConnectionState::Unauthorized => "unauthorized",
    }
}

fn value_views(snapshot: &ValuesSnapshot) -> Vec<HubValueView> {
    snapshot
        .values
        .iter()
        .map(|entry| HubValueView {
            tag: entry.tag.clone(),
            v: entry.v,
            q: entry.q.as_str().to_owned(),
            t: entry.t,
            value_source: entry.value_source.as_str().to_owned(),
        })
        .collect()
}

fn unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or_default()
}

/// 突き合わせ（[`HubService::reconcile`]）を呼んだ理由。
///
/// **資格情報が変わったかもしれない経路**を区別するために要る:
/// `unauthorized` で止まった世代は接続先もタグ集合も変わらないので、
/// [`Fingerprint`] の比較だけでは「同じだから何もしない」になってしまい、
/// 新しいキーで張り直されない（Copilot F3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Trigger {
    /// 表示・定期の突き合わせ。同一性が一致するなら何もしない。
    Observe,
    /// キーが増えた／差し替わった可能性のある操作（`connect` /
    /// `adopt_manual_key`）。同一性が一致していても**必ず張り直す**。
    CredentialsChanged,
}

/// 購読の見張り（supervisor）の周期。
///
/// 30 秒: 「Hub を後から起動した」「消したタグを作り直した」といった人の
/// 操作に対して十分速く、かつ復旧しない状態（keyring 不可など）で
/// `GET /api/v1/tags` を叩き続けても Hub の負荷にならない粒度。画面を
/// 開いているときの 2 秒ポーリングは**メモリしか読まない**別物なので、
/// こちらだけが実際のネットワーク再試行の頻度になる。
const SUPERVISOR_INTERVAL: Duration = Duration::from_secs(30);

/// [`HubService`] の実体。`HubService` はこれへの `Arc` 1 本だけを持つので、
/// supervisor タスクは [`Weak`] を持てる（= 全 clone が落ちたらタスクも
/// 終わる。ぶら下がったタスクがテストや `banto-serve` の終了を妨げない）。
struct HubInner {
    settings: SettingsService,
    mirror: Arc<SettingsMirror>,
    bootstrapper: Arc<Bootstrapper>,
    /// 「設定を読む → bootstrapper を呼ぶ → 書き戻す → 購読を突き合わせる」
    /// を 1 つの操作として**直列化する**ロック。
    ///
    /// **なぜ要るか**: [`SettingsMirror`] は設定ストアの写しで、
    /// [`HubService::hydrate`] が `reset()`（= `dirty` を落として DB の値で
    /// 置き換える）、[`HubService::flush`] が変更分だけを書き戻す。ここに
    /// 見張り（30 秒ごとに `resume_inner()` → `hydrate()`）が割り込むと、
    /// 次の順序で**ユーザーの変更が黙って消える**:
    ///
    /// 1. `set_selected_tags` が写しを更新（`dirty = true`）。
    /// 2. `flush()` の前に見張りのティックが `hydrate()` を呼ぶ → 写しが
    ///    **古い DB の値**で置き換わり、`dirty` も落ちる。
    /// 3. `set_selected_tags` の `flush()` は「変更なし」と見て何も書かず
    ///    **成功を返す**。選択は保存されていないのに保存されたと見える。
    ///
    /// **ロック順序は一方向に固定**: この操作ロック（外）→ [`Self::subscription`]
    /// （内）。`reconcile*` はこのロックを**取らない**前提で書いてあるので
    /// （呼び出し元が既に持っている）、逆順で取る経路を作らないこと。
    /// ポーリング経路（[`HubService::subscription`]）はこのロックを取らず
    /// 購読ロックだけを取る - 2 秒ごとの読み取りが Hub 往復のある操作を
    /// 待たされないように。
    operation: AsyncMutex<()>,
    /// 購読世代（#383 段階1）。`Clone` したハンドル同士が**同じ世代**を
    /// 共有する（LAN ブラウザとデスクトップで WS が 2 本張られない）。
    subscription: AsyncMutex<Subscription>,
    /// spawn した supervisor タスクの本数。0 → 1 の
    /// `compare_exchange` に勝った 1 本だけが走る（`resume()` を何度呼んでも
    /// 増えない）。
    supervisor_spawns: AtomicUsize,
    /// これまでに張った購読世代の本数（[`Generation::sequence`] の採番元）。
    generations_started: AtomicUsize,
}

/// Hub 接続のサービス層。`src-tauri` の `hub_*` コマンドと
/// `crate::rest` の `/api/hub/*` ルーターが共有する（他のサービスと同じ
/// 「service 層は tauri も axum も知らない」規約）。
#[derive(Clone)]
pub struct HubService {
    inner: Arc<HubInner>,
}

impl HubService {
    /// このインストールの `installation_id` を設定から読み（無ければ生成
    /// して保存し）、bootstrapper を組み立てる。
    ///
    /// `keys` は実行形態ごとに差し替える: デスクトップ（`src-tauri`）は OS
    /// キーリング、`banto-serve` は [`UnavailableKeyStore`]。
    pub async fn new(
        settings: SettingsService,
        keys: Arc<dyn KeyStore>,
    ) -> Result<Self, BantoError> {
        let installation_id = match settings.get(KEY_HUB_INSTALLATION_ID).await? {
            Some(value) if !value.trim().is_empty() => value,
            _ => {
                let generated = generate_installation_id();
                settings.set(KEY_HUB_INSTALLATION_ID, &generated).await?;
                generated
            }
        };
        let mirror = Arc::new(SettingsMirror::default());
        let bootstrapper = Arc::new(Bootstrapper::new(
            APP_ID,
            installation_id,
            keys,
            Arc::clone(&mirror) as Arc<dyn BootstrapState>,
        ));
        Ok(Self {
            inner: Arc::new(HubInner {
                settings,
                mirror,
                bootstrapper,
                operation: AsyncMutex::new(()),
                subscription: AsyncMutex::new(Subscription::default()),
                supervisor_spawns: AtomicUsize::new(0),
                generations_started: AtomicUsize::new(0),
            }),
        })
    }

    /// 保存済みの設定で現在の状態を返す。**発行は絶対に行わない**
    /// （設定画面を開いただけでキーが増えないように）。
    ///
    /// catalog を毎回読み直すので**ポーリングには使わない**。購読状態だけ
    /// なら [`Self::subscription`] を見る（ネットワークを叩かない）。
    pub async fn status(&self) -> Result<HubView, BantoError> {
        let _operation = self.begin_operation().await?;
        let record = self.inner.mirror.current();
        if record.is_none() {
            return Ok(self.not_configured_view().await);
        }
        let connection = self
            .inner
            .bootstrapper
            .refresh_catalog()
            .await
            .map_err(to_banto_error)?;
        self.flush().await?;
        Ok(self.view(connection, Trigger::Observe).await)
    }

    /// 購読の状態だけを返す（ポーリング用）。**メモリ上の `watch` を読む
    /// だけ**で、Hub へのリクエストは 1 本も出さない。
    pub async fn subscription(&self) -> HubSubscriptionView {
        self.inner.subscription.lock().await.view()
    }

    /// 起動時の再開（#383 段階1）。保存済みレコードがあれば catalog を
    /// 読み直して購読を張る - 設定画面を開かなくても値が流れる、というのが
    /// この PR の到達点。あわせて見張り（[`Self::spawn_supervisor`]）を
    /// 起動する。
    ///
    /// **失敗しても起動を止めない**。呼び出し元（`src-tauri` の `setup()`、
    /// `banto-serve` の起動）は spawn して投げっぱなしにしてよい。1 回目が
    /// 失敗しても見張りが拾うので、**Hub を後から起動しても値は流れ出す**。
    pub async fn resume(&self) {
        self.spawn_supervisor();
        if let Err(err) = self.resume_inner().await {
            eprintln!("banto: 起動時のHub購読の再開に失敗しました: {err}");
        }
    }

    /// 購読の見張りを 1 本だけ起動する（#385 レビュー対応 F1/F2）。
    ///
    /// **なぜ要るか**: `resume()` は 1 回きりなので、そのとき Hub が落ちて
    /// いれば購読は二度と張られない。さらに `banto-tagclient` のワーカーは
    /// 未解決が 1 件でもあると `BindingUnresolved` を返して `Rebinding` を
    /// 繰り返す（同じ requests で再試行し続ける）ので、**購読中に選んだタグ
    /// が 1 つ Hub から消えると、残りのタグまで流れなくなる**。どちらも
    /// 「catalog を読み直して再計画する」ことでしか直らず、それはアプリの
    /// 仕事（`banto-tagclient` は変更しない）。
    ///
    /// **動く条件**（[`SUPERVISOR_INTERVAL`] ごとに評価）: 保存済みレコード
    /// があり、かつ次のいずれか。
    ///
    /// | 現世代 | 動くか | 理由 |
    /// | --- | --- | --- |
    /// | 無い | ○ | まだ／もう張れていない。再計画で直る可能性がある |
    /// | `Unauthorized` | ○ | 終端状態。キーが差し替わっていれば直る（放っておくと戻らない） |
    /// | `Stopped` | ○ | ワーカーが retryable でも rebindable でもない分類（`InvalidTagSelection` など）で**終了した**形。世代は残るので「無い」では拾えず、拾わないと永久に止まったまま。起動直後の一瞬も `Stopped` だが、次の評価は [`SUPERVISOR_INTERVAL`] 後なので、そのころには先へ進んでいるか本当に死んでいるかのどちらか |
    /// | `Rebinding` | ○ | requests が catalog と合っていない。**再計画でしか直らない** |
    /// | `Live` | × | 正常。触る理由が無い |
    /// | `Connecting` / `Handshaking` | × | 進行中。割り込むと無駄に張り直す |
    /// | `Reconnecting` | × | **`banto-tagclient` 側の backoff の仕事**。ここで `stop → start` すると backoff と喧嘩し、再接続を遅らせるか Hub を叩く回数を増やすだけ |
    ///
    /// タスクは [`Weak`] 越しに [`HubInner`] を掴むので、`HubService` の全
    /// clone が落ちれば次の周期で終わる。**`resume()` からしか起動しない**
    /// ので、`reconcile_with` を直接叩くユニットテストが勝手にネットワーク
    /// を叩くことはない。
    fn spawn_supervisor(&self) {
        if self
            .inner
            .supervisor_spawns
            .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }
        let weak: Weak<HubInner> = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(SUPERVISOR_INTERVAL).await;
                // `service` はこのブロックの中だけで生きる - sleep を跨いで
                // 強参照を持つと `HubService` が解放されなくなる。
                let Some(inner) = weak.upgrade() else {
                    break;
                };
                HubService { inner }.supervise_once().await;
            }
        });
    }

    /// 見張りの 1 周期分。上の表の「動く」ときだけ catalog を読み直す。
    async fn supervise_once(&self) {
        // 操作ロックは**待つ**。周期が 30 秒なのでユーザー操作 1 回分の待ちは
        // 短く、「取れなかったから次の周期まで何もしない」より素直（取れない
        // = ちょうど誰かが設定を触っている、という一番再計画したい瞬間でも
        // ある）。判定と再試行をこの 1 つの操作の中で続けるので、判定した
        // 状態のまま張り直せる。
        let _operation = self.inner.operation.lock().await;
        {
            let mut slot = self.inner.subscription.lock().await;
            // 画面を閉じていても最終受信時刻が進むように、判定より**前**に
            // 無条件で 1 回観測する（`observe_last_value` の doc 参照）。
            slot.observe_last_value();
            if self.inner.mirror.current().is_none() || !needs_retry(slot.health()) {
                return;
            }
        }
        if let Err(err) = self.hydrate().await {
            eprintln!(
                "banto: Hub接続設定の読み取りに失敗しました（次の周期で再試行します）: {err}"
            );
            return;
        }
        if let Err(err) = self.resume_locked().await {
            eprintln!("banto: Hub購読の再試行に失敗しました（次の周期で再試行します）: {err}");
        }
    }

    async fn resume_inner(&self) -> Result<(), BantoError> {
        let _operation = self.begin_operation().await?;
        self.resume_locked().await
    }

    /// [`Self::resume_inner`] の本体。**操作ロックを呼び出し元が持っている
    /// 前提**（見張りは判定と再試行を 1 つの操作として続けたいので、ここを
    /// 直接呼ぶ）。`hydrate` は [`Self::begin_operation`] が済ませている。
    async fn resume_locked(&self) -> Result<(), BantoError> {
        if self.inner.mirror.current().is_none() {
            return Ok(());
        }
        let connection = self
            .inner
            .bootstrapper
            .refresh_catalog()
            .await
            .map_err(to_banto_error)?;
        self.flush().await?;
        self.reconcile(&connection, Trigger::Observe).await;
        Ok(())
    }

    /// 接続（保存済みキーがあれば再利用、無ければ試運転中のみ自己発行）。
    ///
    /// キーが増えた／差し替わった可能性があるので、購読は
    /// [`Trigger::CredentialsChanged`] で**必ず張り直す**。
    pub async fn connect(&self, endpoint: &str) -> Result<HubView, BantoError> {
        let _operation = self.begin_operation().await?;
        let connection = self
            .inner
            .bootstrapper
            .connect(endpoint.trim())
            .await
            .map_err(to_banto_error)?;
        self.flush().await?;
        Ok(self.view(connection, Trigger::CredentialsChanged).await)
    }

    /// ロックダウン済み Hub 向けの手動連携。平文はキーリングにだけ入る。
    ///
    /// キーが差し替わるので、購読は [`Trigger::CredentialsChanged`] で
    /// **必ず張り直す**。
    pub async fn adopt_manual_key(
        &self,
        endpoint: &str,
        key: String,
    ) -> Result<HubView, BantoError> {
        let _operation = self.begin_operation().await?;
        let connection = self
            .inner
            .bootstrapper
            .adopt_manual_key(endpoint.trim(), key)
            .await
            .map_err(to_banto_error)?;
        self.flush().await?;
        Ok(self.view(connection, Trigger::CredentialsChanged).await)
    }

    /// タグ一覧の再取得。
    pub async fn refresh_catalog(&self) -> Result<HubView, BantoError> {
        let _operation = self.begin_operation().await?;
        let connection = self
            .inner
            .bootstrapper
            .refresh_catalog()
            .await
            .map_err(to_banto_error)?;
        self.flush().await?;
        Ok(self.view(connection, Trigger::Observe).await)
    }

    /// 選択タグの保存。空でも保存できる（受入条件）。
    ///
    /// ワイヤ形（引数・戻り値）は #332 のまま。保存したあとに catalog を
    /// 1 回読み直してから購読を張り直すだけで、**追加の 1 往復はユーザー
    /// 操作なので許容**する（画面のポーリングはこの経路を通らない）。
    /// 読み直しに失敗しても**保存は成功のまま返す** - 保存の成否と購読の
    /// 張り直しは別事象で、前者を後者の失敗で覆さない。
    ///
    /// ただし読み直しに失敗したときは**古い世代を止める**。選択を変えた
    /// 以上、古い選択のタグの値はもう誤情報であり、**一時的に何も出ない
    /// 方が、古い選択の値を流し続けるよりまし**だから。世代が無くなれば
    /// 見張り（[`Self::spawn_supervisor`]）が次の周期で拾って張り直す -
    /// 世代を生かしたままだと `Live` のまま据え置かれ、見張りも動かないので
    /// 誰かが明示操作するまで古い値が流れ続けてしまう。
    pub async fn set_selected_tags(&self, tags: Vec<String>) -> Result<(), BantoError> {
        let _operation = self.begin_operation().await?;
        self.inner
            .bootstrapper
            .set_selected_tags(tags)
            .map_err(to_banto_error)?;
        self.flush().await?;
        match self.inner.bootstrapper.refresh_catalog().await {
            Ok(connection) => {
                self.flush().await?;
                self.reconcile(&connection, Trigger::Observe).await;
            }
            Err(err) => {
                eprintln!(
                    "banto: 選択タグ保存後のHubタグ一覧の再取得に失敗しました（古い購読を止めて再試行を待ちます）: {err}"
                );
                let mut slot = self.inner.subscription.lock().await;
                // 未解決・購読不可は**直前の選択**を catalog と突き合わせた
                // 結果なので、選択が変わった今はもう何も語っていない。新しい
                // 選択と並べて出すと理由（再取得できなかった）と中身が食い違う
                // ため、`reconcile_with` の未接続分岐と同じく伏せる。
                slot.unresolved.clear();
                slot.unsupported.clear();
                slot.stop(Some(REASON_SELECTION_CHANGED_REFRESH_FAILED.to_owned()))
                    .await;
            }
        }
        Ok(())
    }

    /// 切断: ローカルの設定とキーリングだけを消す。Hub 側のキーは失効
    /// させない（他のインストールを巻き込まないため - crate 側の
    /// `disconnect` の doc comment参照）。
    pub async fn disconnect(&self) -> Result<HubView, BantoError> {
        let _operation = self.begin_operation().await?;
        self.inner
            .bootstrapper
            .disconnect()
            .map_err(to_banto_error)?;
        self.flush().await?;
        Ok(self.not_configured_view().await)
    }

    /// 「未設定」の応答。記録が無い以上、購読も持てないので世代を落とす。
    async fn not_configured_view(&self) -> HubView {
        self.reconcile_with(&HubStatus::NotConfigured, None, Trigger::Observe)
            .await;
        HubView::new(
            HubStatus::NotConfigured,
            None,
            None,
            self.subscription().await,
        )
    }

    /// 1 回の往復の答えを組み立てる。**すべての操作の最後**にここを通り、
    /// 購読の突き合わせ（[`Self::reconcile`]）もここで行う。
    async fn view(&self, connection: HubConnection, trigger: Trigger) -> HubView {
        self.reconcile(&connection, trigger).await;
        let record = self.inner.mirror.current();
        let tags = connection
            .catalog
            .as_ref()
            .map(|catalog| catalog.tags.iter().map(HubTagView::from).collect());
        HubView::new(
            connection.status,
            record.as_ref(),
            tags,
            self.subscription().await,
        )
    }

    /// 接続の結果と選択タグを突き合わせ、購読世代を「あるべき姿」に寄せる
    /// （#383 段階1）。
    ///
    /// | 状況                                                      | 世代                                            |
    /// | --------------------------------------------------------- | ----------------------------------------------- |
    /// | `Connected` 以外                                          | 落とす（理由を出す）                            |
    /// | `Connected` だが購読要求が空                              | 落とす（**異常ではない**。理由と未解決/購読不可を出す） |
    /// | `Connected` で同一性が一致 かつ `Observe` かつ現世代が `Unauthorized` でない | **何もしない**                |
    /// | `Connected` で同一性が一致 だが [`Trigger::CredentialsChanged`] | 張り直す（新しいキーを使う）               |
    /// | `Connected` で同一性が一致 だが現世代が `Unauthorized`    | 張り直す（キーリングが更新されているかもしれない） |
    /// | `Connected` で同一性が違う / 世代無                       | 既存を止めてから新しく張る                      |
    /// | `rest_client()` が `None`（keyring 不可）                 | 持たない（理由を出す）                          |
    ///
    /// **[`HubStatus`] の 6 状態はここで一切変えない**。購読が張れないこと
    /// は接続設定の失敗ではないので、理由は
    /// [`HubSubscriptionView::reason`] にだけ出る。
    async fn reconcile(&self, connection: &HubConnection, trigger: Trigger) {
        self.reconcile_with(&connection.status, connection.catalog.as_ref(), trigger)
            .await;
    }

    /// [`Self::reconcile`] の本体。`HubConnection` を組み立てられない経路
    /// （未設定・切断直後）からも同じ判断を通せるように、状態と catalog を
    /// 直接受ける。
    async fn reconcile_with(
        &self,
        status: &HubStatus,
        catalog: Option<&CatalogSnapshot>,
        trigger: Trigger,
    ) {
        let record = self.inner.mirror.current();
        let mut slot = self.inner.subscription.lock().await;

        // 1. 接続できていない / catalog を読めていない: 世代は持てない。
        //    未解決タグは catalog と突き合わせて初めて分かるものなので、
        //    ここでは伏せて（空にして）理由の方を出す - 古い判定を残して
        //    「今も消えている」と誤読させない。購読不可の名前は catalog に
        //    依らないので、こちらは残しても嘘にならない - が、対になる
        //    未解決を消す以上まとめて伏せ、次の突き合わせで作り直す。
        let (Some(record), HubStatus::Connected { .. }, Some(catalog)) = (record, status, catalog)
        else {
            slot.unresolved.clear();
            slot.unsupported.clear();
            let reason = match status {
                HubStatus::NotConfigured => REASON_NOT_CONFIGURED,
                _ => REASON_NOT_CONNECTED,
            };
            slot.stop(Some(reason.to_owned())).await;
            return;
        };

        // 2. 未解決タグ・購読できない名前は世代の有無に関わらず画面に出す。
        let plan = plan_bindings(&record.selected_tags, catalog);
        slot.unresolved = plan.unresolved;
        slot.unsupported = plan.unsupported;
        if plan.requests.is_empty() {
            let reason = match (slot.unresolved.is_empty(), slot.unsupported.is_empty()) {
                (true, true) => REASON_NO_TAGS,
                (false, true) => REASON_ALL_UNRESOLVED,
                (true, false) => REASON_ALL_UNSUPPORTED,
                (false, false) => REASON_NONE_SUBSCRIBABLE,
            };
            slot.stop(Some(reason.to_owned())).await;
            return;
        }

        // 3. 同じ接続先・同じタグ集合（名前と stable ID）なら張り直さない
        //    （一番大事な不変条件。設定画面が `status()` を叩くたびに WS を
        //    再接続しない）。ただし**資格情報が変わったかもしれないとき**と
        //    **現世代が `Unauthorized` で終端しているとき**は、同一性が
        //    一致していても張り直す - そうしないと新しいキーが使われず、
        //    値が二度と流れない状態が残る。
        let fingerprint: Fingerprint = (
            fingerprint_endpoint(&record.endpoint),
            fingerprint_tags(&plan.requests),
        );
        let same_generation = slot
            .generation
            .as_ref()
            .is_some_and(|generation| generation.fingerprint == fingerprint);
        if same_generation && !must_restart_despite_same_fingerprint(trigger, slot.health()) {
            slot.reason = None;
            return;
        }

        // 4. 張り直し。`TagClientHandle::restart` は使わない - 消費する API
        //    で、しかも毎回 keyring から作り直した `RestClient` を渡すため、
        //    素直に stop -> start する方が所有関係が単純になる。
        slot.stop(None).await;
        let client = match self.inner.bootstrapper.rest_client() {
            Ok(Some(client)) => client,
            // keyring を持てない実行形態（`banto-serve`）やエントリ喪失。
            // エラーにして接続状態を壊さない。
            Ok(None) => {
                slot.reason = Some(REASON_NO_KEY.to_owned());
                return;
            }
            Err(err) => {
                slot.reason = Some(format!(
                    "購読用のクライアントを作れませんでした: {}",
                    err.kind().as_str()
                ));
                return;
            }
        };
        match client.start(plan.requests) {
            Ok(handle) => {
                // 新しい世代は別の購読なので、前の世代の最終受信時刻は
                // 引き継がない。
                slot.last_value_at = None;
                slot.generation = Some(Generation {
                    states: handle.state_watch(),
                    handle,
                    fingerprint,
                    started_at: unix_seconds(),
                    sequence: self
                        .inner
                        .generations_started
                        .fetch_add(1, Ordering::SeqCst)
                        + 1,
                });
                slot.reason = None;
            }
            Err(err) => {
                slot.reason = Some(format!(
                    "購読を開始できませんでした: {}",
                    err.kind().as_str()
                ));
            }
        }
    }

    /// 1 操作分の直列化を開始し、設定ストアから写しを hydrate する。
    ///
    /// **設定に触る公開操作はすべてここから始める**。返したガードが生きて
    /// いる間だけ「hydrate → bootstrapper → flush → reconcile」が自分の
    /// ものになる（`HubInner::operation` の doc にある握り潰しのシナリオ）。
    /// ロック順序は 操作（外）→ 購読（内）で固定なので、ここから呼ぶ
    /// `reconcile*` は操作ロックを取らない。
    async fn begin_operation(&self) -> Result<AsyncMutexGuard<'_, ()>, BantoError> {
        let guard = self.inner.operation.lock().await;
        self.hydrate().await?;
        Ok(guard)
    }

    /// 設定ストア → インメモリの写し。
    async fn hydrate(&self) -> Result<(), BantoError> {
        let raw = self.inner.settings.get(KEY_HUB_RECORD).await?;
        let record = match raw {
            Some(value) if !value.trim().is_empty() => {
                match serde_json::from_str(&value) {
                    Ok(record) => Some(record),
                    Err(err) => {
                        // 壊れた行で起動不能にはしない - 未設定として扱い、
                        // 次の接続で上書きされる。
                        eprintln!("banto: Hub接続設定の読み取りに失敗しました（未設定として扱います）: {err}");
                        None
                    }
                }
            }
            _ => None,
        };
        self.inner.mirror.reset(record);
        Ok(())
    }

    /// インメモリの写し → 設定ストア（変更があったときだけ）。
    ///
    /// `SettingsService` に削除 API は無いので、消去は空文字列の upsert で
    /// 表す（[`Self::hydrate`] が空文字列を「未設定」として読む）。
    async fn flush(&self) -> Result<(), BantoError> {
        let Some(change) = self.inner.mirror.take_dirty() else {
            return Ok(());
        };
        let value = match change {
            Some(record) => serde_json::to_string(&record).map_err(|err| {
                BantoError::Storage(format!("Hub接続設定のシリアライズに失敗しました: {err}"))
            })?,
            None => String::new(),
        };
        self.inner.settings.set(KEY_HUB_RECORD, &value).await
    }
}

/// UUID v4 形式のインストール ID。
///
/// `uuid` クレートはこのワークスペースの直接依存に無く、これ 1 箇所の
/// ために増やす理由も無いので、既にワークスペース依存にある `getrandom`
/// （`apps/banto-hub/core::api_keys` が API キーの生成に使っているのと同じ
/// もの。新しい依存ノードは増えない）で 128 ビットを取り、version 4 /
/// variant 1 のビットを立てて正規形に整形する。
fn generate_installation_id() -> String {
    let mut bytes = [0u8; 16];
    // getrandom の失敗（OS 側の乱数源が壊れている等）は復旧不能なので
    // `expect` で落とす - `apps/banto-hub/core::api_keys` と同じ扱い。
    getrandom::fill(&mut bytes).expect("システム乱数生成器の呼び出しに失敗しました");
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

#[cfg(test)]
mod tests {
    use banto_tagclient::StableTagId;

    use super::*;
    use crate::db::init_db_memory;

    async fn service() -> (SettingsService, HubService) {
        let pool = init_db_memory().await.expect("init_db_memory");
        let settings = SettingsService::new(pool);
        let hub = HubService::new(settings.clone(), Arc::new(UnavailableKeyStore))
            .await
            .expect("HubService::new");
        (settings, hub)
    }

    #[tokio::test]
    async fn installation_id_is_generated_once_and_reused() {
        let (settings, _hub) = service().await;
        let first = settings
            .get(KEY_HUB_INSTALLATION_ID)
            .await
            .unwrap()
            .expect("installation id must be persisted");
        assert_eq!(first.len(), 36, "canonical UUID form");
        assert_eq!(&first[14..15], "4", "version 4");

        let again = HubService::new(settings.clone(), Arc::new(UnavailableKeyStore))
            .await
            .unwrap();
        drop(again);
        assert_eq!(
            settings.get(KEY_HUB_INSTALLATION_ID).await.unwrap(),
            Some(first)
        );
    }

    #[tokio::test]
    async fn generated_ids_differ() {
        assert_ne!(generate_installation_id(), generate_installation_id());
    }

    #[tokio::test]
    async fn status_is_not_configured_before_any_connect() {
        let (settings, hub) = service().await;
        let view = hub.status().await.unwrap();
        assert_eq!(view.status, HubStatus::NotConfigured);
        assert!(view.endpoint.is_none());
        assert!(view.tags.is_none(), "an unread catalog is null, not empty");
        assert_eq!(settings.get(KEY_HUB_RECORD).await.unwrap(), None);
    }

    #[tokio::test]
    async fn an_unreachable_endpoint_leaves_the_app_unconfigured() {
        // Bind then drop so the port is well-formed but closed.
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);

        let (settings, hub) = service().await;
        let view = hub.connect(&endpoint).await.unwrap();
        assert_eq!(view.status.as_str(), "unreachable");
        assert!(view.tags.is_none());
        assert_eq!(
            settings.get(KEY_HUB_RECORD).await.unwrap(),
            None,
            "a failed connect must not persist a record"
        );
    }

    #[tokio::test]
    async fn an_invalid_endpoint_is_a_field_validation_error() {
        let (_settings, hub) = service().await;
        match hub.connect("https://example.test").await {
            Err(BantoError::Validation { field_errors }) => {
                assert_eq!(field_errors[0].field, "endpoint");
            }
            other => panic!("expected a validation error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn the_record_round_trips_through_the_settings_kv_without_a_key() {
        let (settings, hub) = service().await;
        // Seed a record the way a successful connect would have.
        let record = HubRecord {
            endpoint: "http://127.0.0.1:3100".to_owned(),
            installation_id: "inst".to_owned(),
            key_id: Some(3),
            key_name: Some("chronogazer-inst-1000".to_owned()),
            keyring_account: "hub:127.0.0.1:3100:inst".to_owned(),
            selected_tags: vec!["line1.fast.temp01".to_owned()],
        };
        settings
            .set(KEY_HUB_RECORD, &serde_json::to_string(&record).unwrap())
            .await
            .unwrap();

        hub.set_selected_tags(vec!["line1.fast.temp02".to_owned()])
            .await
            .unwrap();

        let stored = settings.get(KEY_HUB_RECORD).await.unwrap().unwrap();
        assert!(
            !stored.contains("\"key\""),
            "no plaintext key may be stored: {stored}"
        );
        let read_back: HubRecord = serde_json::from_str(&stored).unwrap();
        assert_eq!(
            read_back.selected_tags,
            vec!["line1.fast.temp02".to_owned()]
        );
        assert_eq!(read_back.key_id, Some(3));
    }

    #[tokio::test]
    async fn an_empty_selection_is_savable_and_disconnect_clears_the_record() {
        let (settings, hub) = service().await;
        let record = HubRecord {
            endpoint: "http://127.0.0.1:3100".to_owned(),
            installation_id: "inst".to_owned(),
            key_id: Some(3),
            key_name: None,
            keyring_account: "hub:127.0.0.1:3100:inst".to_owned(),
            selected_tags: vec!["line1.fast.temp01".to_owned()],
        };
        settings
            .set(KEY_HUB_RECORD, &serde_json::to_string(&record).unwrap())
            .await
            .unwrap();

        hub.set_selected_tags(Vec::new()).await.unwrap();
        let stored: HubRecord =
            serde_json::from_str(&settings.get(KEY_HUB_RECORD).await.unwrap().unwrap()).unwrap();
        assert!(stored.selected_tags.is_empty());

        let view = hub.disconnect().await.unwrap();
        assert_eq!(view.status, HubStatus::NotConfigured);
        assert_eq!(
            settings.get(KEY_HUB_RECORD).await.unwrap().as_deref(),
            Some(""),
            "clearing is an empty upsert (SettingsService has no delete)"
        );
        assert_eq!(hub.status().await.unwrap().status, HubStatus::NotConfigured);
    }

    #[tokio::test]
    async fn a_corrupt_record_reads_as_unconfigured_instead_of_failing() {
        let (settings, hub) = service().await;
        settings.set(KEY_HUB_RECORD, "{not json").await.unwrap();
        assert_eq!(hub.status().await.unwrap().status, HubStatus::NotConfigured);
    }

    // --- #383 段階1: 購読 ---------------------------------------------------

    fn catalog_tag(name: &str, ids: StableTagId) -> CatalogTag {
        CatalogTag {
            external_name: name.to_owned(),
            tag_key: format!("tag:{name}"),
            ids,
            connection: "line1".into(),
            group: "fast".into(),
            name: name.into(),
            address: "D100".into(),
            data_type: "f64".into(),
            unit: None,
            decimals: 0,
            period_ms: 100,
            enabled: true,
            writable: false,
            tag_kind: "plc".into(),
            expression: None,
            retain: false,
            simulation: false,
            configured_simulation: false,
            effective_simulation: false,
            value_source: banto_tagclient::ValueSource::Real,
        }
    }

    fn catalog(names: &[&str]) -> CatalogSnapshot {
        CatalogSnapshot {
            revision: 1,
            run_id: Some(1),
            collection_mode: banto_tagclient::CollectionMode::Configured,
            tags: names
                .iter()
                .enumerate()
                .map(|(index, name)| catalog_tag(name, StableTagId::new(1, 1, index as i64 + 1)))
                .collect(),
        }
    }

    fn owned(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn plan_bindings_resolves_everything_when_the_catalog_still_has_it() {
        let plan = plan_bindings(&owned(&["a", "b"]), &catalog(&["a", "b", "c"]));
        assert_eq!(
            plan.requests
                .iter()
                .map(|request| request.binding_key.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b"],
            "binding_key は external name そのまま（画面と 1:1）"
        );
        assert!(plan.unresolved.is_empty());
    }

    #[test]
    fn plan_bindings_subscribes_to_the_rest_when_one_tag_vanished() {
        let plan = plan_bindings(&owned(&["a", "gone", "b"]), &catalog(&["a", "b"]));
        assert_eq!(plan.requests.len(), 2, "1 個消えただけで全部止めない");
        assert_eq!(plan.unresolved, owned(&["gone"]));
    }

    #[test]
    fn plan_bindings_reports_every_missing_tag_instead_of_an_empty_list() {
        let plan = plan_bindings(&owned(&["gone1", "gone2"]), &catalog(&["a"]));
        assert!(
            plan.requests.is_empty(),
            "購読できるものが無ければ start しない"
        );
        assert_eq!(
            plan.unresolved,
            owned(&["gone1", "gone2"]),
            "未解決を空表示に潰さない"
        );
    }

    #[test]
    fn plan_bindings_folds_duplicates_so_start_is_not_rejected() {
        // `start()` は重複 binding_key / 重複 stable_id を拒否するので、
        // ここで畳んでおかないと購読そのものが立ち上がらない。
        let plan = plan_bindings(&owned(&["a", "a", "gone", "gone"]), &catalog(&["a"]));
        assert_eq!(plan.requests.len(), 1);
        assert_eq!(plan.unresolved, owned(&["gone"]));
    }

    #[test]
    fn plan_bindings_on_an_empty_selection_is_empty_but_not_an_error() {
        let plan = plan_bindings(&[], &catalog(&["a"]));
        assert!(plan.requests.is_empty());
        assert!(plan.unresolved.is_empty());
        assert!(plan.unsupported.is_empty());
    }

    /// 購読プロトコルはタグ名をカンマ区切りで並べるので、名前にカンマを
    /// 含められない。1 件混ざるとワーカーが毎回 `InvalidTagSelection` で
    /// 失敗し**購読全体が死ぬ**ので、先に落として残りを購読する。
    #[test]
    fn plan_bindings_separates_names_the_subscription_protocol_cannot_carry() {
        let plan = plan_bindings(
            &owned(&["a", "bad,name", "   ", "b"]),
            &catalog(&["a", "b", "bad,name", "   "]),
        );
        assert_eq!(
            plan.requests
                .iter()
                .map(|request| request.binding_key.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b"],
            "購読できる残りはそのまま購読する"
        );
        assert_eq!(
            plan.unsupported,
            owned(&["bad,name", "   "]),
            "カンマ入りと空白だけの名前は購読不可として分けて出す"
        );
        assert!(
            plan.unresolved.is_empty(),
            "catalog にあるのだから「消えた」ではない - 理由が違うものを混ぜない"
        );
    }

    #[test]
    fn plan_bindings_keeps_unsupported_and_unresolved_apart() {
        let plan = plan_bindings(&owned(&["a,b", "gone", "ok"]), &catalog(&["ok"]));
        assert_eq!(plan.requests.len(), 1);
        assert_eq!(plan.unsupported, owned(&["a,b"]));
        assert_eq!(plan.unresolved, owned(&["gone"]));
    }

    /// Hub 側でタグを消して**同じ名前で作り直す**と `StableTagId` が変わる。
    /// 名前しか見ない fingerprint だと「同じ」と判定されて張り直されず、
    /// 古い ID で購読し続けて復帰しない（Copilot F4）。
    #[test]
    fn the_fingerprint_changes_when_a_tag_is_recreated_under_the_same_name() {
        let before = plan_bindings(&owned(&["a"]), &catalog(&["a"]));
        let recreated = CatalogSnapshot {
            tags: vec![catalog_tag("a", StableTagId::new(1, 1, 99))],
            ..catalog(&["a"])
        };
        let after = plan_bindings(&owned(&["a"]), &recreated);

        assert_eq!(
            fingerprint_tags(&before.requests)
                .iter()
                .map(|(name, _)| name.clone())
                .collect::<Vec<_>>(),
            fingerprint_tags(&after.requests)
                .iter()
                .map(|(name, _)| name.clone())
                .collect::<Vec<_>>(),
            "名前だけ見ると同じ"
        );
        assert_ne!(
            fingerprint_tags(&before.requests),
            fingerprint_tags(&after.requests),
            "stable ID まで見れば別物"
        );
    }

    #[test]
    fn the_fingerprint_tag_order_does_not_depend_on_the_selection_order() {
        let one = plan_bindings(&owned(&["b", "a"]), &catalog(&["a", "b"]));
        let other = plan_bindings(&owned(&["a", "b"]), &catalog(&["a", "b"]));
        assert_eq!(
            fingerprint_tags(&one.requests),
            fingerprint_tags(&other.requests)
        );
    }

    fn healthy(state: TagClientConnectionState) -> Option<GenerationHealth> {
        Some(GenerationHealth {
            state,
            failed: false,
        })
    }

    fn failed(state: TagClientConnectionState) -> Option<GenerationHealth> {
        Some(GenerationHealth {
            state,
            failed: true,
        })
    }

    /// 同一性が一致していても張り直すべき場合（資格情報の変更と終端状態）。
    #[test]
    fn a_matching_fingerprint_is_still_restarted_for_new_credentials_or_a_terminal_state() {
        use TagClientConnectionState as State;

        // 資格情報が変わったかもしれない経路は常に張り直す。
        for health in [None, healthy(State::Live), healthy(State::Reconnecting)] {
            assert!(must_restart_despite_same_fingerprint(
                Trigger::CredentialsChanged,
                health
            ));
        }
        // 終端した世代は、観測のための突き合わせでも張り直す。これが無いと
        // 見張りが再試行しても同一性一致で no-op になり、止まった世代が
        // 居座り続ける。
        assert!(must_restart_despite_same_fingerprint(
            Trigger::Observe,
            healthy(State::Unauthorized)
        ));
        assert!(
            must_restart_despite_same_fingerprint(Trigger::Observe, failed(State::Stopped)),
            "エラーで終了した世代は残っていても張り直す"
        );
        // それ以外は据え置き - `status()` のたびに WS を張り直さない。
        // **起動直後の `Stopped`（`last_error` 無し）を含む**: ここを終端
        // 扱いにすると、張った直後に `status()` が走っただけで張り直す。
        for health in [
            None,
            healthy(State::Stopped),
            healthy(State::Live),
            healthy(State::Connecting),
            healthy(State::Handshaking),
            healthy(State::Rebinding),
            healthy(State::Reconnecting),
        ] {
            assert!(!must_restart_despite_same_fingerprint(
                Trigger::Observe,
                health
            ));
        }
    }

    /// `banto-tagclient` は `Live` を離れると `current()` を捨てるので、
    /// そこから導くと**再接続・再バインドに入った瞬間に最終受信時刻が消える**。
    /// 観測できないこと（`None`）を「受けていない」と読み替えない。
    #[test]
    fn the_last_value_time_survives_leaving_live() {
        // Live で t=5 を観測 → 覚える。
        assert_eq!(advance_last_value_at(None, Some(5)), Some(5));
        // Live を離れた（`current()` が `None`）→ 消さない。
        assert_eq!(advance_last_value_at(Some(5), None), Some(5));
        // まだ一度も受けていなければ `None` のまま（0 に潰さない）。
        assert_eq!(advance_last_value_at(None, None), None);
        // 進むときは単調 - 巻き戻る `t` で過去に戻さない。
        assert_eq!(advance_last_value_at(Some(5), Some(9)), Some(9));
        assert_eq!(advance_last_value_at(Some(5), Some(3)), Some(5));
    }

    /// 世代が `Live` でなくても、覚えている最終受信時刻を view に出す
    /// （世代があるとき・無いときの両方）。
    #[tokio::test]
    async fn the_view_reports_the_remembered_last_value_time_outside_live() {
        let (_settings, hub) = service_with_keyring(&closed_endpoint(), &["a"]).await;
        hub.reconcile_with(
            &HubStatus::Connected { tag_count: 1 },
            Some(&catalog(&["a"])),
            Trigger::Observe,
        )
        .await;

        // 到達不能な接続先なので世代は `Live` にならない＝`current()` は
        // `None`。Live の間に観測したことにして覚えさせる。
        {
            let mut slot = hub.inner.subscription.lock().await;
            assert!(slot.generation.is_some());
            slot.last_value_at = Some(1_234);
        }
        assert_eq!(
            hub.subscription().await.last_value_at,
            Some(1_234),
            "Live を離れても最終受信時刻は消えない"
        );

        // 世代を止めても「いつまで受けていたか」は残す。
        hub.reconcile_with(&HubStatus::AuthFailed, None, Trigger::Observe)
            .await;
        let view = hub.subscription().await;
        assert_eq!(view.state, "stopped");
        assert_eq!(view.last_value_at, Some(1_234));
    }

    /// 見張りが 1 周期で動く条件（`spawn_supervisor` の判断表）。
    ///
    /// エラーで終了した `Stopped` が要るのは、ワーカーが retryable でも
    /// rebindable でもない分類で終わったとき**世代が残る**から - 「世代が
    /// 無い」条件では拾えず、拾わないと永久に止まったままになる（UI の
    /// 「まもなく自動で再試行します」も嘘になる）。
    #[test]
    fn the_supervisor_retries_only_states_that_cannot_recover_on_their_own() {
        use TagClientConnectionState as State;

        assert!(needs_retry(None), "世代が無ければ張りに行く");
        assert!(needs_retry(healthy(State::Unauthorized)));
        assert!(
            needs_retry(failed(State::Stopped)),
            "エラーで終了した世代は自力で復帰しない"
        );
        assert!(needs_retry(healthy(State::Rebinding)));
        for health in [
            healthy(State::Stopped),
            healthy(State::Live),
            healthy(State::Connecting),
            healthy(State::Handshaking),
            healthy(State::Reconnecting),
        ] {
            assert!(
                !needs_retry(health),
                "進行中・正常・張った直後には割り込まない（`Reconnecting` は tagclient 側の backoff の仕事）"
            );
        }
    }

    #[test]
    fn the_fingerprint_endpoint_ignores_spelling_differences_of_the_same_hub() {
        assert_eq!(
            fingerprint_endpoint("http://127.0.0.1:3100"),
            fingerprint_endpoint("http://127.0.0.1:3100/")
        );
        assert_ne!(
            fingerprint_endpoint("http://127.0.0.1:3100"),
            fingerprint_endpoint("http://127.0.0.1:3101")
        );
    }

    /// `banto-serve` の `UnavailableKeyStore` では keyring からキーを
    /// 取り出せないので世代を持てない。それは**接続の 6 状態を汚さない**
    /// 別軸の事実で、理由付きの「停止」として出る。
    #[tokio::test]
    async fn reconcile_without_a_keyring_keeps_the_six_states_intact() {
        let (settings, hub) = service().await;
        let record = HubRecord {
            endpoint: "http://127.0.0.1:3100".to_owned(),
            installation_id: "inst".to_owned(),
            key_id: None,
            key_name: None,
            keyring_account: "hub:127.0.0.1:3100/:inst".to_owned(),
            selected_tags: owned(&["a", "gone"]),
        };
        settings
            .set(KEY_HUB_RECORD, &serde_json::to_string(&record).unwrap())
            .await
            .unwrap();
        hub.hydrate().await.unwrap();

        hub.reconcile_with(
            &HubStatus::Connected { tag_count: 1 },
            Some(&catalog(&["a"])),
            Trigger::Observe,
        )
        .await;

        let view = hub.subscription().await;
        assert_eq!(view.state, "stopped");
        assert_eq!(view.subscribed_count, 0);
        assert_eq!(
            view.unresolved,
            owned(&["gone"]),
            "世代を持てなくても未解決タグは出す"
        );
        assert!(view.reason.is_some(), "停止している理由を必ず出す");
        assert!(view.values.is_empty());
        // 接続状態は購読の失敗で変わらない（記録はそのまま残っている）。
        assert!(settings
            .get(KEY_HUB_RECORD)
            .await
            .unwrap()
            .is_some_and(|raw| !raw.is_empty()));
    }

    /// 選択が全部 catalog から消えた状態。**異常ではない**ので理由付きで
    /// 止まり、消えたタグを一覧に出す。
    #[tokio::test]
    async fn reconcile_with_only_unresolved_tags_stops_with_a_reason_and_lists_them() {
        let (_settings, hub) = service().await;
        hub.inner.mirror.reset(Some(HubRecord {
            endpoint: "http://127.0.0.1:3100".to_owned(),
            installation_id: "inst".to_owned(),
            key_id: None,
            key_name: None,
            keyring_account: "hub:127.0.0.1:3100/:inst".to_owned(),
            selected_tags: owned(&["gone1", "gone2"]),
        }));

        hub.reconcile_with(
            &HubStatus::Connected { tag_count: 1 },
            Some(&catalog(&["a"])),
            Trigger::Observe,
        )
        .await;

        let view = hub.subscription().await;
        assert_eq!(view.state, "stopped");
        assert_eq!(view.unresolved, owned(&["gone1", "gone2"]));
        assert_eq!(view.reason.as_deref(), Some(REASON_ALL_UNRESOLVED));
    }

    /// catalog を読めていない状態では未解決の判定ができないので、古い判定を
    /// 残さず理由の方を出す（「今も消えている」と誤読させない）。
    #[tokio::test]
    async fn reconcile_when_disconnected_reports_the_reason_not_a_stale_unresolved_list() {
        let (_settings, hub) = service().await;
        hub.inner.mirror.reset(Some(HubRecord {
            endpoint: "http://127.0.0.1:3100".to_owned(),
            installation_id: "inst".to_owned(),
            key_id: None,
            key_name: None,
            keyring_account: "hub:127.0.0.1:3100/:inst".to_owned(),
            selected_tags: owned(&["gone"]),
        }));
        hub.reconcile_with(
            &HubStatus::Connected { tag_count: 0 },
            Some(&catalog(&[])),
            Trigger::Observe,
        )
        .await;
        assert_eq!(hub.subscription().await.unresolved, owned(&["gone"]));

        hub.reconcile_with(&HubStatus::AuthFailed, None, Trigger::Observe)
            .await;

        let view = hub.subscription().await;
        assert_eq!(view.state, "stopped");
        assert!(view.unresolved.is_empty());
        assert_eq!(view.reason.as_deref(), Some(REASON_NOT_CONNECTED));
    }

    /// 選んだタグが全部「購読できない名前」だったときは、未解決とは**別の**
    /// 理由を出す（次の一手が違う: 名前を直す vs Hub にタグを戻す）。
    #[tokio::test]
    async fn reconcile_with_only_unsupported_names_says_so_instead_of_blaming_the_catalog() {
        let (_settings, hub) = service().await;
        hub.inner.mirror.reset(Some(HubRecord {
            endpoint: "http://127.0.0.1:3100".to_owned(),
            installation_id: "inst".to_owned(),
            key_id: None,
            key_name: None,
            keyring_account: "hub:127.0.0.1:3100/:inst".to_owned(),
            selected_tags: owned(&["a,b"]),
        }));

        hub.reconcile_with(
            &HubStatus::Connected { tag_count: 1 },
            Some(&catalog(&["a,b"])),
            Trigger::Observe,
        )
        .await;

        let view = hub.subscription().await;
        assert_eq!(view.state, "stopped");
        assert_eq!(view.unsupported, owned(&["a,b"]));
        assert!(view.unresolved.is_empty());
        assert_eq!(view.reason.as_deref(), Some(REASON_ALL_UNSUPPORTED));
    }

    // --- #385 レビュー対応: 世代の張り直し ----------------------------------

    /// 待ち受けの無いポート。`start()` 自体は成功して世代が立ち、ワーカーは
    /// backoff に入る - 「世代を張ったか／据え置いたか」だけを見たいこの節に
    /// はそれで十分（実 Hub は要らない）。
    fn closed_endpoint() -> String {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        endpoint
    }

    const KEYRING_ACCOUNT: &str = "hub:127.0.0.1:0/:inst";

    fn keyring_record(endpoint: &str, selected: &[&str]) -> HubRecord {
        HubRecord {
            endpoint: endpoint.to_owned(),
            installation_id: "inst".to_owned(),
            key_id: None,
            key_name: None,
            keyring_account: KEYRING_ACCOUNT.to_owned(),
            selected_tags: owned(selected),
        }
    }

    /// keyring を持つ実行形態（デスクトップ）相当。`UnavailableKeyStore` では
    /// `rest_client()` が常に `None` を返して世代が立たないため、張り直しの
    /// 検証にはインメモリのキーストアを使う。
    ///
    /// レコードは設定 KV にも書く（`hydrate()` を通る経路のテストのため）。
    async fn service_with_keyring(
        endpoint: &str,
        selected: &[&str],
    ) -> (SettingsService, HubService) {
        use banto_hub_bootstrap::state::memory::MemoryKeyStore;

        let pool = init_db_memory().await.expect("init_db_memory");
        let settings = SettingsService::new(pool);
        let keys = Arc::new(MemoryKeyStore::new());
        keys.set(KEYRING_ACCOUNT, "bh_abcd1234_opaque-secret")
            .unwrap();
        let hub = HubService::new(settings.clone(), Arc::clone(&keys) as Arc<dyn KeyStore>)
            .await
            .expect("HubService::new");
        let record = keyring_record(endpoint, selected);
        settings
            .set(KEY_HUB_RECORD, &serde_json::to_string(&record).unwrap())
            .await
            .unwrap();
        hub.inner.mirror.reset(Some(record));
        (settings, hub)
    }

    async fn generation_sequence(hub: &HubService) -> Option<usize> {
        hub.inner
            .subscription
            .lock()
            .await
            .generation
            .as_ref()
            .map(|generation| generation.sequence)
    }

    /// 同じ接続先・同じタグなら据え置き、**同じ名前で作り直された**（stable
    /// ID が変わった）タグがあれば張り直す（Copilot F4）。
    #[tokio::test]
    async fn a_recreated_tag_replaces_the_generation_while_an_unchanged_one_does_not() {
        let (_settings, hub) = service_with_keyring(&closed_endpoint(), &["a"]).await;
        let connected = HubStatus::Connected { tag_count: 1 };

        hub.reconcile_with(&connected, Some(&catalog(&["a"])), Trigger::Observe)
            .await;
        let first = generation_sequence(&hub)
            .await
            .expect("keyring があれば世代が立つ");

        hub.reconcile_with(&connected, Some(&catalog(&["a"])), Trigger::Observe)
            .await;
        assert_eq!(
            generation_sequence(&hub).await,
            Some(first),
            "同じ接続先・同じタグ・同じ stable ID なら張り直さない"
        );

        let recreated = CatalogSnapshot {
            tags: vec![catalog_tag("a", StableTagId::new(1, 1, 99))],
            ..catalog(&["a"])
        };
        hub.reconcile_with(&connected, Some(&recreated), Trigger::Observe)
            .await;
        assert_eq!(
            generation_sequence(&hub).await,
            Some(first + 1),
            "同名で作り直されたら古い ID のまま購読し続けない"
        );
    }

    /// `connect` / `adopt_manual_key` の後は、同一性が一致していても新しい
    /// キーで張り直す（Copilot F3）。
    #[tokio::test]
    async fn a_credentials_change_restarts_the_generation_despite_an_identical_fingerprint() {
        let (_settings, hub) = service_with_keyring(&closed_endpoint(), &["a"]).await;
        let connected = HubStatus::Connected { tag_count: 1 };

        hub.reconcile_with(&connected, Some(&catalog(&["a"])), Trigger::Observe)
            .await;
        let first = generation_sequence(&hub).await.expect("世代が立つ");

        hub.reconcile_with(
            &connected,
            Some(&catalog(&["a"])),
            Trigger::CredentialsChanged,
        )
        .await;
        assert_eq!(
            generation_sequence(&hub).await,
            Some(first + 1),
            "接続先もタグも同じでも、キーが変わったなら張り直す"
        );
    }

    /// 選択を変えた直後に catalog を取り直せなかったとき、**古い世代を
    /// 生かしたままにしない**（#385 レビュー第2巡 A）。生かしたままだと
    /// `Live` のまま据え置かれ、見張りも動かないので、ユーザーが選び直した
    /// あとも古い選択のタグの値が流れ続けてしまう。
    #[tokio::test]
    async fn a_selection_change_whose_refresh_fails_drops_the_stale_generation() {
        // 古い選択には「消えたタグ」と「購読できない名前」も混ぜておく -
        // これらが選択変更後に残らないことまで確かめたいので。
        let (settings, hub) = service_with_keyring(&closed_endpoint(), &["a", "gone", "x,y"]).await;
        hub.reconcile_with(
            &HubStatus::Connected { tag_count: 1 },
            Some(&catalog(&["a"])),
            Trigger::Observe,
        )
        .await;
        assert!(
            generation_sequence(&hub).await.is_some(),
            "まず古い選択で世代が立っている"
        );
        let before = hub.subscription().await;
        assert_eq!(before.unresolved, owned(&["gone"]));
        assert_eq!(before.unsupported, owned(&["x,y"]));

        // `refresh_catalog()` が `Err` になる接続先へ差し替える（到達不能な
        // 接続先は `Ok(Unreachable)` になり、そちらは通常の突き合わせで
        // 世代が落ちるため、ここでは解釈できない綴りを使う）。
        let broken = keyring_record("https://example.test", &["a", "gone", "x,y"]);
        settings
            .set(KEY_HUB_RECORD, &serde_json::to_string(&broken).unwrap())
            .await
            .unwrap();

        hub.set_selected_tags(owned(&["b"]))
            .await
            .expect("保存の成否は再取得の失敗で覆さない");

        assert!(
            generation_sequence(&hub).await.is_none(),
            "古い選択のまま値を流し続けない"
        );
        let view = hub.subscription().await;
        assert_eq!(view.state, "stopped");
        assert_eq!(
            view.reason.as_deref(),
            Some(REASON_SELECTION_CHANGED_REFRESH_FAILED)
        );
        // 未解決・購読不可は**直前の選択**の判定なので、選択が変わった今は
        // 残さない（理由と中身が食い違う）。
        assert!(view.unresolved.is_empty());
        assert!(view.unsupported.is_empty());
        // 選択自体は保存されている（見張りが次の周期で張り直す材料になる）。
        let stored: HubRecord =
            serde_json::from_str(&settings.get(KEY_HUB_RECORD).await.unwrap().unwrap()).unwrap();
        assert_eq!(stored.selected_tags, owned(&["b"]));
    }

    /// 見張りのティックがユーザー操作に割り込んでも、**保存した選択が
    /// 黙って消えない**（#385 レビュー第5巡 A）。
    ///
    /// 直列化が無いと次の順序で消える: `set_selected_tags` が写しを更新
    /// （`dirty`）→ `flush()` の前に見張りの `hydrate()` が**古い DB の値**で
    /// 写しを置き換えて `dirty` を落とす → `flush()` は「変更なし」と見て
    /// 何も書かずに成功を返す。
    ///
    /// 接続先は解釈できない綴りにしてあるので、どちらの経路も
    /// `refresh_catalog()` が即 `Err` になりネットワークを触らない - 見たい
    /// のは設定の読み書きの競合だけ。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_supervisor_tick_never_swallows_a_saved_selection() {
        let (settings, hub) = service_with_keyring("https://example.test", &["a"]).await;

        for round in 0..5 {
            let saved = format!("tag{round}");
            let writer = hub.clone();
            let supervisor = hub.clone();
            let tag = saved.clone();
            // 見張りのティックと保存を本当に同時に走らせる。
            let (result, _) = tokio::join!(
                async move { writer.set_selected_tags(vec![tag]).await },
                async move {
                    let _ = supervisor.resume_inner().await;
                }
            );
            result.expect("保存は成功する");

            let stored: HubRecord =
                serde_json::from_str(&settings.get(KEY_HUB_RECORD).await.unwrap().unwrap())
                    .unwrap();
            assert_eq!(
                stored.selected_tags,
                vec![saved],
                "第{round}回: 見張りの hydrate が保存を上書きしてはいけない"
            );
        }
    }

    /// 直列化そのものを決定的に固定する: **ある操作が hydrate〜flush の
    /// 途中である間、見張りのティックは進めない**。
    ///
    /// 上のテストは「同時に走らせても結果が壊れない」という利用者から見た
    /// 保証だが、割り込みの窓が開くかどうかはタイミング次第で、失敗を必ず
    /// 捕まえられるとは限らない。こちらは操作ロックを外から握ることで
    /// 「操作の途中」を作り出し、見張りが待たされること自体を見る。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_operation_in_flight_blocks_the_supervisor_tick() {
        let (_settings, hub) = service_with_keyring("https://example.test", &["a"]).await;

        // 「誰かが hydrate〜flush の途中」の状態を作る。
        let held = hub.inner.operation.lock().await;
        let mut ticking = tokio::spawn({
            let hub = hub.clone();
            async move {
                let _ = hub.resume_inner().await;
            }
        });

        assert!(
            tokio::time::timeout(Duration::from_millis(150), &mut ticking)
                .await
                .is_err(),
            "操作の途中は見張りが hydrate まで進めない（ここで進めると保存が消える）"
        );

        drop(held);
        tokio::time::timeout(Duration::from_secs(2), ticking)
            .await
            .expect("操作が終われば見張りは進む")
            .unwrap();
    }

    /// 見張りは何度 `resume()` しても 1 本だけ（clone 越しでも増えない）。
    #[tokio::test]
    async fn resume_starts_at_most_one_supervisor_task() {
        let (_settings, hub) = service().await;
        hub.resume().await;
        hub.resume().await;
        assert_eq!(hub.inner.supervisor_spawns.load(Ordering::SeqCst), 1);

        // clone は同じ `HubInner` を共有するので、そちらから呼んでも増えない。
        hub.clone().resume().await;
        assert_eq!(hub.inner.supervisor_spawns.load(Ordering::SeqCst), 1);
    }

    /// 見張りは保存済みレコードが無ければ何もしない（ネットワークも叩かない）。
    #[tokio::test]
    async fn the_supervisor_does_nothing_without_a_record() {
        let (_settings, hub) = service().await;
        hub.supervise_once().await;
        assert_eq!(hub.subscription().await.state, "stopped");
    }

    #[tokio::test]
    async fn a_view_without_a_subscription_is_stopped_and_carries_no_key() {
        let (_settings, hub) = service().await;
        let view = hub.status().await.unwrap();
        assert_eq!(view.status, HubStatus::NotConfigured);
        assert_eq!(view.subscription.state, "stopped");
        assert_eq!(
            view.subscription.reason.as_deref(),
            Some(REASON_NOT_CONFIGURED)
        );
        assert!(view.subscription.values.is_empty());

        let json = serde_json::to_value(&view).unwrap();
        let subscription = json
            .get("subscription")
            .expect("HubView に subscription が入る");
        for field in [
            "state",
            "reason",
            "subscribedCount",
            "unresolved",
            "unsupported",
            "lastError",
            "lastValueAt",
            "values",
        ] {
            assert!(
                subscription.get(field).is_some(),
                "{field} が camelCase で出る"
            );
        }
        let raw = serde_json::to_string(&view).unwrap();
        assert!(!raw.contains("\"key\""), "平文キーの欄は存在しない: {raw}");
    }

    /// 起動時 resume は保存済みレコードが無ければ何もせず、失敗しても
    /// 呼び出し元へ伝播しない（起動を止めない）。
    #[tokio::test]
    async fn resume_is_a_no_op_without_a_record_and_never_propagates_failure() {
        let (_settings, hub) = service().await;
        hub.resume().await;
        assert_eq!(hub.subscription().await.state, "stopped");
    }

    #[tokio::test]
    async fn the_unavailable_key_store_refuses_to_write_but_reads_as_empty() {
        let store = UnavailableKeyStore;
        assert_eq!(store.get("hub:x:1:y").unwrap(), None);
        assert_eq!(
            store.set("hub:x:1:y", "secret").unwrap_err().kind(),
            BootstrapErrorKind::KeyStore
        );
        store.delete("hub:x:1:y").unwrap();
    }
}
