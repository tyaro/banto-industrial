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
//!   は `unresolved`、そのままでは購読要求に載せられないもの（名前にカンマを
//!   含む・空白だけ、同じ安定 ID を指す重複）は `unsupported` に出し、
//!   **残りだけで購読する**。1 個の事故で
//!   購読全体を殺さない・空表示に潰さない。
//! * **購読の失敗で [`HubStatus`] の 6 状態を変えない**。接続設定の状態と
//!   購読の状態は別物で、購読が張れない理由は
//!   [`HubSubscriptionView::reason`] に出す。
//! * `banto-serve`（[`UnavailableKeyStore`]）は keyring を持てないので
//!   [`Bootstrapper::rest_client`] が `None` を返し、購読を張れない。
//!   これはエラーではなく、理由付きの「停止」として表示する。
//! * **値が二度と流れない状態を作らない**: 1 本の常駐タスク（supervisor）が
//!   30 秒ごとに「自力では復帰しない世代」だけを拾って張り直す - 世代が無い
//!   ／`Unauthorized`／エラーで終了した `Stopped`／`Rebinding`／**要求セットが
//!   catalog と食い違ったまま待っている `Reconnecting`**（状態名だけでなく
//!   `last_error` の分類まで見る）。詳しくは
//!   [`HubService::spawn_supervisor`]。
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
    BindingRequest, CatalogSnapshot, CatalogTag, Endpoint, ErrorKind as TagErrorKind, StableTagId,
    TagClientConnectionState, TagClientHandle, TagClientState, ValuesSnapshot,
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
    /// そのままでは購読要求に載せられなかった external name（名前にカンマを
    /// 含む・空白だけ、または他の名前と同じ安定 ID を指す重複）。
    /// `unresolved` とは**理由が違う**ので混ぜない。
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
    /// 写しが変わるたびに 1 増える版番号。[`SettingsMirror::mark_flushed`] が
    /// 「今書き終えたのは本当に最新の版か」を判断するのに使う（`HubRecord`
    /// は `PartialEq` を持たないので、値の比較ではなく版で見る）。
    revision: u64,
}

impl SettingsMirror {
    /// 設定から読んだ値で写しを置き換える（dirty はここでクリアする）。
    fn reset(&self, record: Option<HubRecord>) {
        let mut inner = self.inner.lock().expect("hub mirror poisoned");
        inner.record = record;
        inner.dirty = false;
        inner.revision = inner.revision.wrapping_add(1);
    }

    /// 変更があれば「書き戻すべき値」と、その値の版番号を返す
    /// （`Some((rev, None))` = 消去）。**dirty はここでは落とさない**
    /// （[`SettingsMirror::mark_flushed`] の doc 参照）。
    fn pending(&self) -> Option<(u64, Option<HubRecord>)> {
        let inner = self.inner.lock().expect("hub mirror poisoned");
        if !inner.dirty {
            return None;
        }
        Some((inner.revision, inner.record.clone()))
    }

    /// 設定ストアへの書き込みが**成功した**ことを伝え、dirty を落とす。
    ///
    /// **なぜ「取ってから書く」をやめたか**（#394 のレビュー P1-2）: 以前の
    /// `take_dirty()` は書き込みの**前に** dirty を落としていたため、
    /// `SettingsService::set` が失敗すると「変更はどこにも書かれていないのに
    /// 写しは綺麗（dirty=false）」という状態が残った。後続の `flush()` は
    /// 何も書かず、次の [`HubService::hydrate`] が写しを DB の値（＝古い、
    /// あるいはレコード無し）で上書きするので、**発行済みの API キーの記録が
    /// メモリからも消える**。Hub 側にはキーが残っているのに、こちらは
    /// 「前のキー」を知らないので失効させられず、接続し直すたびに 1 本ずつ
    /// 増えていく。成功してから落とせば、失敗は次の `flush()` で再試行される。
    ///
    /// 書き込んでいる間に写しがさらに変わっていたら（版が進んでいたら）
    /// dirty は落とさない - 落とすと**新しい方の変更**が書かれないまま
    /// 消えるため。
    fn mark_flushed(&self, revision: u64) {
        let mut inner = self.inner.lock().expect("hub mirror poisoned");
        if inner.revision == revision {
            inner.dirty = false;
        }
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
        inner.revision = inner.revision.wrapping_add(1);
        Ok(())
    }

    fn clear(&self) -> Result<(), BootstrapError> {
        let mut inner = self.inner.lock().expect("hub mirror poisoned");
        inner.record = None;
        inner.dirty = true;
        inner.revision = inner.revision.wrapping_add(1);
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
    "選んだタグをそのままでは購読できないため（名前が購読プロトコルの制約に合わない、または同じタグを重複して指している）、購読できるタグがありません。";
const REASON_NONE_SUBSCRIBABLE: &str =
    "選んだタグはHubのタグ一覧に無いか、そのままでは購読できないため、購読できるタグがありません。";
const REASON_NO_KEY: &str =
    "保存済みのAPIキーを取り出せないため購読できません（デスクトップアプリから接続し直してください）。";
/// キーは取り出せたが Hub に拒否された。[`REASON_NO_KEY`] とは原因も次の
/// 一手も違うので混ぜない（こちらは再接続か手動キーの採用で直る）。
const REASON_KEY_REJECTED: &str =
    "保存済みのAPIキーがHubに拒否されたため購読できません（「接続」で再発行するか、APIキーを採用してください）。";
/// アプリを終了している最中（[`HubService::shutdown`]）。**この理由になった
/// 世代は二度と張り直されない** - 見張りは停止済みで、`resume()` も
/// 起こし直さない。ほかの停止理由（環境が直れば復帰する）と混ぜないために
/// 独立した語彙にする。
const REASON_SHUTTING_DOWN: &str = "アプリを終了しているため購読を停止しました。";
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
///
/// 名前の綴りだけを見る - 同じ安定 ID を指す重複は catalog を引いて初めて
/// 分かるので [`plan_bindings`] 側で落とす。
fn is_unsupported_tag_name(name: &str) -> bool {
    name.trim().is_empty() || name.contains(',')
}

/// [`plan_bindings`] の結果: 実際に購読する要求と、購読できなかった名前を
/// **理由別に**分けたもの。`unresolved`（Hub から消えた／権限で見えない）と
/// `unsupported`（そのままでは購読要求に載せられない）は次の一手が違うので
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
/// * **同じ [`StableTagId`] を指す 2 つ目以降の名前**も `unsupported` へ
///   落とす（先勝ち）。`start()` は重複 `stable_id` を
///   `DuplicateRequestedStableId` で拒否するので、1 件混ざると**購読全体が
///   立たない**。ここで落とせば残りは購読できる。
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
    let mut claimed: HashSet<StableTagId> = HashSet::with_capacity(selected.len());
    let mut requests = Vec::with_capacity(selected.len());
    let mut unresolved = Vec::new();
    let mut unsupported = Vec::new();
    for name in selected {
        // 選択リスト内の同名重複は利用者の入力の話なので、黙って 1 つに畳む。
        if !seen.insert(name.as_str()) {
            continue;
        }
        if is_unsupported_tag_name(name) {
            unsupported.push(name.clone());
            continue;
        }
        match by_name.get(name.as_str()) {
            // 別々の名前が同じ安定 ID を指すのは Hub 側の catalog の不整合
            // だが、そのまま `start()` へ渡すと全体が拒否される。先勝ちで
            // 1 つだけ購読し、残りは「購読できなかった名前」として見せる。
            Some(tag) if !claimed.insert(tag.ids) => unsupported.push(name.clone()),
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

/// [`Subscription::last_value_at`] が「どの購読についての事実か」を表す
/// **粗い**同一性: 正規化した接続先 + 選択タグ（ソート済み）。
///
/// [`Fingerprint`] と違って stable ID を含まない。catalog を読めていないとき
/// （切断中・認証エラー中）にも計算できる必要があるため - 「同じ購読が止まって
/// いるだけ」かどうかは、まさに catalog を読めない場面で判断したい。
type SubscriptionIdentity = (String, Vec<String>);

fn subscription_identity(record: Option<&HubRecord>) -> Option<SubscriptionIdentity> {
    let record = record?;
    let mut tags = record.selected_tags.clone();
    tags.sort();
    tags.dedup();
    Some((fingerprint_endpoint(&record.endpoint), tags))
}

/// 覚えている「最後に値を受けた時刻」を、これからの購読でも出してよいか。
///
/// * **残す** = 同じ購読が止まっているだけ（終端エラー・再試行待ち・
///   `Unauthorized` / `Rebinding`）。**いつまでデータが来ていたかは記録計の
///   診断に効く**ので、止まった瞬間に消してはいけない。
/// * **消す** = その購読がもう無い（切断・未設定）か、別物になった（接続先が
///   変わった・選択タグが変わった）。**別の Hub・別のタグ集合の時刻を出すと
///   嘘になる。**
fn keeps_last_value_at(
    recorded: Option<&SubscriptionIdentity>,
    next: Option<&SubscriptionIdentity>,
) -> bool {
    match (recorded, next) {
        (Some(recorded), Some(next)) => recorded == next,
        _ => false,
    }
}

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
    /// 直近の失敗の分類。
    ///
    /// **状態名だけでは足りない**（2026-09-17 実機で判明）: `banto-tagclient`
    /// は同じ `Reconnecting` に「transport の backoff 中」と「**要求セットが
    /// catalog と食い違ったまま**待ち続けている」の両方を載せる。後者を
    /// 状態名だけで「backoff の担当だから放置」と扱うと、タグが 1 つ消えた
    /// だけで購読全体が二度と戻らない。
    last_error: Option<TagErrorKind>,
}

impl GenerationHealth {
    /// 何らかの失敗を経験しているか。
    fn failed(self) -> bool {
        self.last_error.is_some()
    }
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
        TagClientConnectionState::Stopped => health.failed(),
        _ => false,
    }
}

/// 要求セットと catalog の食い違いが原因で、**待っても直らない**分類か。
///
/// `banto-tagclient` はこれらを rebindable として数回やり直すが
/// （`worker.rs` の `plan_failure`）、**回数を使い切ると `FailurePlan::Backoff`
/// に落ちる**。`Backoff` は `Reconnecting` + `last_error` を publish するだけ
/// なので、以後は**古い（存在しないタグを含む）要求セットのまま**永久に
/// 再試行し続ける。直すには catalog を読み直して**要求を作り直す**しかなく、
/// それはアプリの仕事（`banto-tagclient` は変更しない）。
fn needs_replan(kind: TagErrorKind) -> bool {
    matches!(
        kind,
        TagErrorKind::BindingUnresolved
            | TagErrorKind::RevisionMismatch
            | TagErrorKind::RuntimeMetadataMismatch
    )
}

/// `Reconnecting` のうち、**古い要求セットのまま待ち続けている**ものか。
///
/// `Reconnecting` を状態名だけで「`banto-tagclient` の backoff の担当」と
/// 扱ってはいけない、という唯一の例外。transport 系（`Transport` /
/// `ProtocolError` / `CatalogUnavailable`）の `Reconnecting` は従来どおり
/// 放置する - あちらは待てば直るので、割り込むと backoff と喧嘩するだけ。
///
/// この状態は**ワーカー自身が「要求セットが catalog と食い違っている」と
/// 言っている**のだから、同じ要求のまま待たせても直らない。起こすだけでなく
/// **張り直しても直す**必要がある（[`needs_rebuild`] に入れてある理由）。
fn waits_on_a_stale_request_set(health: GenerationHealth) -> bool {
    health.state == TagClientConnectionState::Reconnecting
        && health.last_error.is_some_and(needs_replan)
}

/// この世代は**張り直さないと直らない**か。
///
/// [`needs_retry`]（見張りが起こすか）と
/// [`must_restart_despite_same_fingerprint`]（起こしたあと実際に張り直すか）
/// が**食い違わない**ように、1 つの述語を両方から使う。片方だけ真だと、
/// 見張りが catalog を取り直しても同一性一致の早期 return に落ちて何もせず、
/// ワーカーは壊れたループのまま残る（#383 実機で `Reconnecting` が、
/// #385 で `Rebinding` が、それぞれこの形で壊れていた）。
///
/// * 終端（[`is_terminal`]）… 放っておくと復帰しない。
/// * `Rebinding` … requests が catalog と合っていない。catalog が変わって
///   いれば同一性も変わるので張り直されるが、変わらない原因
///   （`RevisionMismatch` / `RuntimeMetadataMismatch`）だと同一性は同じまま
///   なので、**ここで明示的に張り直さないと rebind ループから抜けられない**。
fn needs_rebuild(health: GenerationHealth) -> bool {
    is_terminal(health)
        || health.state == TagClientConnectionState::Rebinding
        || waits_on_a_stale_request_set(health)
}

/// 同一性（[`Fingerprint`]）が一致しているのに、それでも張り直すべきか。
///
/// 純関数にしてあるのは、ここが**値が二度と流れない状態を作らない**ための
/// 判断そのものだから:
///
/// * [`Trigger::CredentialsChanged`] … `connect` / `adopt_manual_key` の後。
///   キーが増えた／差し替わったので、同じ接続先・同じタグでも新しいキーで
///   張り直さないと意味が無い。
/// * 現世代が [`needs_rebuild`] … 同一性が同じでも張り直す（ダメならまた
///   同じ状態になるだけ）。これが無いと、見張りが再試行しても同一性一致で
///   no-op になり、壊れた世代が居座り続ける。
///
/// [`waits_on_a_stale_request_set`] も [`needs_rebuild`] に含む。当初は
/// 「catalog が変われば同一性も変わるので自然に張り直る」と考えて外していたが、
/// **それが成り立つのは `BindingUnresolved` だけ**だった:
///
/// * `BindingUnresolved`（タグが消えた）… 再計画すると要求セットが変わる →
///   同一性が変わって張り直る（実機で確認済み）。
/// * `RevisionMismatch` / `RuntimeMetadataMismatch` … **タグ集合は同じまま**
///   なので同一性が変わらず、早期 return に落ちて**見張りが 30 秒ごとに
///   catalog を読むだけ**になる。ワーカーは壊れたループのまま。
///
/// ワーカー自身が「要求セットが catalog と食い違っている」と言っている以上、
/// 同じ要求のまま待たせても直らない。張り直せば catalog を読み直して要求を
/// 作り直すので、メタデータ不一致の 2 つも回復できる。条件が続く間は
/// [`SUPERVISOR_INTERVAL`] ごとに張り直すことになるが、**永久に止まっている
/// よりはよい**（周期で上限が付いている）。
///
/// これは [`Fingerprint`] に catalog の `revision` / `run_id` /
/// `collection_mode` を**含めない**という選択と対になっている: 含めると Hub
/// 側の無関係な構成変更のたびに購読を切ってしまうので含めず、そのぶん
/// 「メタデータ不一致で詰まった世代」はこちらで拾って張り直す。
fn must_restart_despite_same_fingerprint(
    trigger: Trigger,
    health: Option<GenerationHealth>,
) -> bool {
    trigger == Trigger::CredentialsChanged || health.is_some_and(needs_rebuild)
}

/// catalog を取り直しても変わらない「落ち着いた停止」か（世代が無いとき）。
///
/// 選択が空 / 選んだ名前が全部購読プロトコル非対応、のどちらも Hub 側で
/// 何が起きても状況は変わらない（次に変わるのは**ユーザーが選び直したとき**
/// で、それは明示操作の突き合わせが拾う）。ここで見張りを回すと 30 秒ごとに
/// `GET /api/v1/tags` を撃ち続けるだけになる。
///
/// [`REASON_ALL_UNRESOLVED`] は**含めない** - Hub 側にタグが戻れば直るので
/// 取り直す価値がある。[`REASON_NONE_SUBSCRIBABLE`]（未解決と購読不可の
/// 混在）も未解決の分は戻り得るので同じ。キーリング不可・再取得失敗・
/// 未確認も、環境が変われば直るので従来どおり再試行する。
fn is_settled_without_a_generation(reason: Option<&str>) -> bool {
    reason.is_some_and(|reason| reason == REASON_NO_TAGS || reason == REASON_ALL_UNSUPPORTED)
}

/// 見張り（[`HubService::spawn_supervisor`]）の 1 周期で再試行すべきか。
/// 判断表はそちらの doc comment にある。純関数なのでテストで固定できる。
fn needs_retry(health: Option<GenerationHealth>, reason: Option<&str>) -> bool {
    match health {
        // 世代が無い: まだ／もう張れていない。ただし catalog を取り直しても
        // 変わらない理由なら撃たない。
        None => !is_settled_without_a_generation(reason),
        // 張り直しが要る状態か、`Reconnecting` でも古い要求セットのまま
        // 待ち続けている状態（再計画でしか直らない）。
        Some(health) => needs_rebuild(health) || waits_on_a_stale_request_set(health),
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
    /// ということなので、観測したら保持する。**止まっただけなら残し、購読が
    /// 別物になった／無くなったら消す**（[`keeps_last_value_at`]）。
    last_value_at: Option<i64>,
    /// [`Self::last_value_at`] がどの購読についての事実か。世代が止まっても
    /// 残す値なので、世代とは別に覚える。
    last_value_from: Option<SubscriptionIdentity>,
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
            last_value_from: None,
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

    /// これからの購読の同一性を渡し、覚えている最終受信時刻を残すか捨てるかを
    /// 決める。**突き合わせのたびに（世代を張る前に）通す。**
    fn retain_last_value_for(&mut self, next: Option<&SubscriptionIdentity>) {
        if !keeps_last_value_at(self.last_value_from.as_ref(), next) {
            self.last_value_at = None;
            self.last_value_from = None;
        }
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
                last_error: state.last_error(),
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

/// Hub への 1 往復（ネットワークを伴う bootstrapper 呼び出し）に許す最大
/// 時間（#394 のレビュー P1-1）。
///
/// **なぜ要るか**: `banto-hub-bootstrap` の `AdminClient` も
/// `banto-tagclient` の `RestClient` も reqwest の既定（**タイムアウト無し**）
/// で組み立てられている。TCP は繋がるが応答を返さない相手（accept だけして
/// 黙るリスナ、フリーズした Hub、経路の途中で落ちた NAT）に当たると
/// **永久に待つ**。しかもその await は [`HubService::begin_operation`] の
/// 操作ロックを握ったまま行われるので、1 回の無応答で `status` / `connect` /
/// `disconnect` / 選択の保存 / 見張りの 1 周期まで**すべて止まり、復旧は
/// アプリの再起動しか無くなる**。共有 crate（他アプリと共用、変更しない
/// 方針）ではなく、このアプリ側で上限を持つ。
///
/// **なぜ 15 秒か**: Hub は同一 PC ないし同じ LAN にいる前提で、catalog は
/// v1 目標の 256 タグ - 通常の応答は 1 秒に満たない。15 秒は「遅い」では
/// なく「明らかに応答していない」と言い切れる十分に緩い上限で、Wi-Fi の
/// 一時的なもたつきや起動直後の Hub の重さを誤って切らない。遅い LAN や
/// 大きな catalog が現実になったら設定値に出す余地はある（上限がこの定数
/// 1 箇所に閉じているので、設定から与える形にしても呼び出し側は変わらない）。
///
/// **ロックが残らない理由**: [`tokio::time::timeout`] は期限が来ると内側の
/// future を **drop する**ので、こちらの待機は必ず終わり、呼び出し元の `?` が
/// 操作ラックのガードを落とす。タイムアウトしたままロックが握られ続けることは
/// ない。
///
/// **ただしこれは「こちらが待つのをやめる」だけ**（#395 のレビュー B）。
/// drop できるのはローカルの future であって、**Hub が既に受け取って処理した
/// 要求は取り消せない**。したがってこの上限は
/// **Hub 側に副作用を残さない往復に**使う - `status` / `refresh_catalog` /
/// `set_selected_tags` の再取得 / `resume_locked` はどれも `GET` の読み取り
/// だけ。`adopt_manual_key` も同じ側で、ネットワークに出るのは
/// `verify()`（`GET /api/v1/tags`）だけ、成功後の保存は OS キーリングと
/// 設定という**ローカル 2 つ**に閉じている（#395 のレビュー P2 で crate の
/// 実装を確認: `previous_for` は保存済みの記録を**読むだけ**で、Hub 側の
/// 発行も失効も行わない）。打ち切っても Hub には何も残らないので、
/// 短い上限で切ってよい。**Hub 側にキーを作る** `connect` だけが
/// [`HUB_MUTATING_TIMEOUT`] を使う。
const HUB_OPERATION_TIMEOUT: Duration = Duration::from_secs(15);

/// **Hub 側に副作用を残す**往復（`connect` だけ）に許す最大時間
/// （#395 のレビュー B）。
///
/// 対象は `connect` **のみ**: Hub にキーを発行させるのはこの経路だけで、
/// `adopt_manual_key` は利用者が貼ったキーを検証して**ローカルに保存する
/// だけ**なので読み取り側（[`HUB_OPERATION_TIMEOUT`]）に置く
/// （#395 のレビュー P2）。
///
/// **なぜ別にするか**: `Bootstrapper::connect()` は 1 回の呼び出しの中で
/// 「旧キーの revoke → 新キーの issue → keyring 保存 → 記録の保存 → verify」
/// と進む。`tokio::time::timeout` は**cancellation-safe ではない**:
/// `POST /api/api-keys` が Hub 側で成立した直後に応答が遅れて打ち切ると、
/// **こちらは発行結果を知らないまま Hub にキーが残る**（防ごうとしている
/// 孤児キーを、別の経路で自分で作ってしまう）。読み取りと同じ 15 秒で切ると
/// この窓が現実的な確率で開くので、**正常な Hub なら到達しない余裕**を取る。
///
/// **なぜ 60 秒か**: キー発行は revoke + issue + verify の 3 往復ぶんで、
/// 遅い Hub（起動直後、ディスクの詰まった Windows）でも合計数秒に収まる。
/// 60 秒は「もう応答は来ない」と言い切れる側に倒した値で、P1-1 の目的
/// （**永久に固まらない**）だけを満たす。
///
/// **残留リスク**: それでも打ち切りが副作用を取り消さないことは変わらない。
/// 根本的に無くすには、crate 側に idempotency key（同じ要求を 2 回投げても
/// 1 本しか発行されない）か「発行済みか問い合わせる」口が要る - 共有 crate
/// の変更になるため別途判断。打ち切ったときは利用者に
/// [`hub_mutating_timeout_reason`] で「Hub 側にキーが残っている可能性」と
/// 探し方を伝えるところまでをこの PR の範囲とする。
const HUB_MUTATING_TIMEOUT: Duration = Duration::from_secs(60);

/// [`HUB_OPERATION_TIMEOUT`] を使い切ったときに載せる購読の停止理由。
///
/// 上限そのものを文言に入れるのは、**もう待っていない**ことが利用者に
/// 伝わるようにするため。定数から作るので、上限を変えても文言とずれない。
fn hub_timeout_reason() -> String {
    format!(
        "Hubが{}秒以内に応答しませんでした。接続先とHubの状態を確認してください。",
        HUB_OPERATION_TIMEOUT.as_secs()
    )
}

/// [`HubService::set_selected_tags`] の再取得がタイムアウトしたときの理由。
///
/// この経路だけは**保存そのものは成功している**（再取得は best effort）ので、
/// それが伝わらない [`hub_timeout_reason`] は使わない。見張りが next tick で
/// 拾い直すことも添えて、ユーザーに再操作を要求しない
/// （[`REASON_SELECTION_CHANGED_REFRESH_FAILED`] と同じ考え方）。
fn selection_saved_hub_timeout_reason() -> String {
    format!(
        "選択を保存しましたが、Hubが{}秒以内に応答しませんでした。まもなく自動で再試行します。",
        HUB_OPERATION_TIMEOUT.as_secs()
    )
}

/// Hub 側に副作用を残す往復（`connect` のみ。[`HUB_MUTATING_TIMEOUT`]）を
/// 打ち切ったときの理由。
///
/// 読み取り側（[`hub_timeout_reason`]）と違い、**Hub 側に APIキーが残って
/// いるかもしれない**ことと、その探し方（キー名の接頭辞）まで伝える -
/// こちらからは失効させられない（`key_id` を受け取れていない）ので、
/// 運用者が Hub のキー一覧で見つけて消せるようにするため。接頭辞は
/// `Bootstrapper` の `key_name`（`{app_id}-{installation_id}-{issued_at}`）と
/// 同じ組み立てで、**このインストールの実際の値**を埋める。
fn hub_mutating_timeout_reason(installation_id: &str) -> String {
    format!(
        "Hubが{}秒以内に応答しませんでした。Hub側にAPIキーが作成されている可能性があります（キー名の接頭辞 {}-{}-）。Hubのキー一覧を確認してください。",
        HUB_MUTATING_TIMEOUT.as_secs(),
        APP_ID,
        installation_id
    )
}

/// Hub への 1 往復の結果（[`with_hub_timeout`] の戻り）。
///
/// `reason_override` は、この往復が**タイムアウトで打ち切られた**ことを
/// 購読の [`HubSubscriptionView::reason`] に伝えるためだけのもの。`None` なら
/// [`HubService::reconcile_with_reason`] が通常どおり状態から理由を決める。
struct HubCall {
    connection: HubConnection,
    reason_override: Option<String>,
}

/// bootstrapper の**ネットワークを伴う**呼び出しは、操作ロックを持ったまま
/// await するので、必ず上限を付ける - 包み忘れが 1 箇所でもあると、そこだけで
/// 全体が固まる。**Hub 側に副作用を残さない経路（読み取り、および保存先が
/// ローカルだけの `adopt_manual_key`）はこれで包む**。Hub にキーを発行させる
/// `connect` だけは [`with_timeout`] + [`HUB_MUTATING_TIMEOUT`] を使う
/// （どちらを使うかの根拠は [`HUB_OPERATION_TIMEOUT`] の doc 参照）。
///
/// **タイムアウトはエラーではなく 6 状態の
/// [`HubStatus::Unreachable`]（`UnreachableCause::Transport`）として返す**
/// （#395 のレビュー対応）。エラーで返すと画面には消えるトーストが出るだけで
/// **状態表示は「接続済み」のまま残り**、購読も `live` のまま据え置かれる -
/// 「ポーリングが恒久的に失敗しても『受信中』と最後の値を出し続ける」という、
/// この PR が直しているのと同じ「実態より良く見える」欠陥を新しく作ってしまう。
/// `Ok(Unreachable)` なら状態表示が実態と一致し、[`HubService::reconcile`] が
/// 走るので購読も止まる。`banto-hub-bootstrap` 自身、`UnreachableCause::Transport`
/// の doc で timeout をこの分類に入れているので、**新しい状態も語彙も増えない**。
///
/// 打ち切った上限を文言に残す（「15 秒で切った」という具体性）ため、理由は
/// `reason` で受け取る（自由文の購読理由に載る）。
async fn with_timeout<F>(
    budget: Duration,
    reason: impl FnOnce() -> String,
    future: F,
) -> Result<HubCall, BantoError>
where
    F: std::future::Future<Output = Result<HubConnection, BootstrapError>>,
{
    match tokio::time::timeout(budget, future).await {
        Ok(result) => result.map(HubCall::answered).map_err(to_banto_error),
        Err(_elapsed) => Ok(HubCall::timed_out(reason())),
    }
}

/// **冪等な（読み取りだけの）**往復用。打ち切っても Hub には何も残らない
/// ので、[`HUB_OPERATION_TIMEOUT`] の短い上限で切ってよい。
async fn with_hub_timeout<F>(future: F) -> Result<HubCall, BantoError>
where
    F: std::future::Future<Output = Result<HubConnection, BootstrapError>>,
{
    with_timeout(HUB_OPERATION_TIMEOUT, hub_timeout_reason, future).await
}

impl HubCall {
    fn answered(connection: HubConnection) -> Self {
        Self {
            connection,
            reason_override: None,
        }
    }

    /// 打ち切った往復。`catalog` は `None` - 読めていないものを空の
    /// スナップショットで表さないのは crate 側の `HubConnection` と同じ規律。
    fn timed_out(reason: String) -> Self {
        Self {
            connection: HubConnection {
                status: HubStatus::Unreachable {
                    cause: banto_hub_bootstrap::UnreachableCause::Transport,
                },
                catalog: None,
            },
            reason_override: Some(reason),
        }
    }

    fn timed_out_flag(&self) -> bool {
        self.reason_override.is_some()
    }
}

/// [`HubService`] の実体。`HubService` はこれへの `Arc` 1 本だけを持つので、
/// supervisor タスクは [`Weak`] を持てる（= 全 clone が落ちたらタスクも
/// 終わる。ぶら下がったタスクがテストや `banto-serve` の終了を妨げない）。
struct HubInner {
    settings: SettingsService,
    mirror: Arc<SettingsMirror>,
    bootstrapper: Arc<Bootstrapper>,
    /// このインストールの ID。発行されるキー名の接頭辞
    /// （`{APP_ID}-{installation_id}-`）を利用者に案内するために持つ
    /// （[`hub_mutating_timeout_reason`]）。`Bootstrapper` にも同じ値を
    /// 渡してあるが、あちらは記録が無いと外へ出せないので、**まだ何も
    /// 保存されていない打ち切り**でも案内できるようにこちらでも保持する。
    installation_id: String,
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
    /// **終了の合図**（[`HubService::shutdown`]）。`true` になったら、
    /// 見張りは次の周期を待たずに抜け、`resume()` は起こし直さず、
    /// [`HubService::reconcile_with_reason`] は新しい世代を張らない。
    ///
    /// `AtomicBool` ではなく `watch` にしてあるのは、**見張りの
    /// [`SUPERVISOR_INTERVAL`] の sleep を打ち切る**ため。フラグだけだと
    /// 「次の周期の頭で抜ける」＝最大 30 秒タスクが残り、終了処理が
    /// それを待てなくなる（待たなければ終了は固まらないが、`Weak` が
    /// 切れるまで回っていた従来と大差なくなる）。読み取りは
    /// [`HubService::is_shutting_down`]。
    ///
    /// 送信側しか持たないので、送るときは `send`（受信者ゼロで失敗する）
    /// ではなく `send_replace` を使う。
    shutdown: watch::Sender<bool>,
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
            installation_id.clone(),
            keys,
            Arc::clone(&mirror) as Arc<dyn BootstrapState>,
        ));
        Ok(Self {
            inner: Arc::new(HubInner {
                settings,
                mirror,
                bootstrapper,
                installation_id,
                operation: AsyncMutex::new(()),
                subscription: AsyncMutex::new(Subscription::default()),
                supervisor_spawns: AtomicUsize::new(0),
                shutdown: watch::channel(false).0,
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
        let call = with_hub_timeout(self.inner.bootstrapper.refresh_catalog()).await?;
        let saved = self.flush().await;
        self.finish(saved, call, Trigger::Observe).await
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
        // **終了中は起こし直さない**（[`Self::shutdown`]）。見張りの再 spawn
        // だけでなく、その場の張り直し（`resume_inner`）も止める - 終了処理
        // が世代を落とした直後にここが走ると、閉じたはずの WS がもう 1 本
        // 立ったままプロセスが終わる。
        if self.is_shutting_down() {
            return;
        }
        self.spawn_supervisor();
        if let Err(err) = self.resume_inner().await {
            eprintln!("banto: 起動時のHub購読の再開に失敗しました: {err}");
        }
    }

    /// アプリ終了時の後始末（#383 R1-C の前提）。**購読世代を止め、見張りを
    /// 終わらせ、以後は誰が `resume()` を呼んでも起き上がらない**ようにする。
    ///
    /// 収集エンジンを同じ Tauri プロセスに置くというオーナー決定
    /// （2026-09-18、`docs/recorder-requirements.md` §4）の下では、プロセスの
    /// 終了が唯一の「止める場所」になる。managed state は Tauri が drop
    /// しないので、ここを呼ばない限り WS は close を送らずに切れる。
    ///
    /// # 待たないもの（ここが一番大事）
    ///
    /// **操作ロック（[`HubInner::operation`]）を無条件には待たない。**
    /// `connect` は [`HUB_MUTATING_TIMEOUT`]（60 秒）を持ったまま操作ロックを
    /// 握るので、素直に `lock().await` すると**終了が最大 60 秒固まる**。
    /// ウィンドウを閉じたのに 1 分プロセスが残る方が、購読の後始末を
    /// 取りこぼすより悪い（取りこぼしても、OS が TCP を畳む）。そこで
    /// `try_lock` で取れたときだけ握り、取れなければ**購読ロックだけで**
    /// 世代を止める。ロック順序（操作 → 購読）は変えていない。
    ///
    /// 見張りタスクの終了も**待たない**（`JoinHandle` を持たない）。合図は
    /// [`HubInner::shutdown`] で送ってあり、タスクは sleep を打ち切って抜ける。
    /// 待とうとすると、まさに今 60 秒の `connect` を握っているかもしれない
    /// 相手を待つことになる。
    ///
    /// # 上限
    ///
    /// この関数自身は上限を持たない（内側の await は `try_lock` の失敗で
    /// 早々に抜けるか、購読ロック + `TagClientHandle::shutdown` だけ）。
    /// **終了処理全体の上限は呼び出し側**（`src-tauri` の `RunEvent::Exit`）
    /// が 1 本で掛ける - LAN サーバーの graceful shutdown や DB プールの
    /// close も同じ予算の中で諦めさせたいため。
    pub async fn shutdown(&self) {
        // 1. 先に合図を上げる。これ以降に走る見張り・`resume()`・
        //    `reconcile_with_reason` は新しい世代を張らないので、2. で止めた
        //    ものが後から立ち上がり直すことがない。
        self.inner.shutdown.send_replace(true);
        // 2. 操作ロックは取れたら握る（取れなくても進む。doc 参照）。取れた
        //    ときは、他の操作と同じ順序で購読ロックへ進むことになる。
        let _operation = self.inner.operation.try_lock().ok();
        self.inner
            .subscription
            .lock()
            .await
            .stop(Some(REASON_SHUTTING_DOWN.to_owned()))
            .await;
    }

    /// 終了の合図が上がっているか（[`HubInner::shutdown`]）。
    fn is_shutting_down(&self) -> bool {
        *self.inner.shutdown.borrow()
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
    /// | 無い（理由が「タグ未選択」「全部購読不可」以外） | ○ | まだ／もう張れていない。再計画で直る可能性がある |
    /// | 無い（理由が「タグ未選択」「全部購読不可」） | × | catalog を取り直しても変わらない。次に変わるのはユーザーが選び直したときで、それは明示操作の突き合わせが拾う（[`is_settled_without_a_generation`]） |
    /// | `Unauthorized` | ○ | 終端状態。キーが差し替わっていれば直る（放っておくと戻らない） |
    /// | `Stopped` + `last_error` あり | ○ | ワーカーが retryable でも rebindable でもない分類（`InvalidTagSelection` など）で**終了した**形。世代は残るので「無い」では拾えず、拾わないと永久に止まったまま |
    /// | `Stopped` + `last_error` 無し | × | 張った直後の初期状態（すぐ `Connecting` へ移る）。終端扱いにすると張った直後の `status()` で張り直してしまう |
    /// | `Rebinding` | ○ | requests が catalog と合っていない。**再計画でしか直らない** |
    /// | `Live` | × | 正常。触る理由が無い |
    /// | `Connecting` / `Handshaking` | × | 進行中。割り込むと無駄に張り直す |
    /// | `Reconnecting`（`last_error` が `BindingUnresolved` / `RevisionMismatch` / `RuntimeMetadataMismatch`） | ○ | **一律放置ではない**。`banto-tagclient` は再バインドの回数を使い切ると `Backoff`（= `Reconnecting`）へ落ち、**古い要求セットのまま**永久に再試行する。catalog を読み直して要求を作り直すしかない（2026-09-17 実機: 選択中のタグを 1 つ消したら購読全体が戻らなくなった） |
    /// | `Reconnecting`（transport 系） | × | **`banto-tagclient` 側の backoff の仕事**。ここで `stop → start` すると backoff と喧嘩し、再接続を遅らせるか Hub を叩く回数を増やすだけ |
    ///
    /// 起こす（[`needs_retry`]）と実際に張り直す
    /// （[`must_restart_despite_same_fingerprint`]）は同じ述語
    /// [`needs_rebuild`] を使うので食い違わない - 片方だけ真だと、catalog を
    /// 取り直しても同一性一致の早期 return に落ちて何もせず、壊れたワーカーが
    /// 残る。**世代があるかぎり「起こすなら必ず張り直す」**。世代が無いときの
    /// 再試行だけは張り直しの話にならない（張るものが無く、`reconcile_with`
    /// が新しく張る）。
    ///
    /// タスクは [`Weak`] 越しに [`HubInner`] を掴むので、`HubService` の全
    /// clone が落ちれば次の周期で終わる。**`resume()` からしか起動しない**
    /// ので、`reconcile_with` を直接叩くユニットテストが勝手にネットワーク
    /// を叩くことはない。
    ///
    /// **止め方**（[`Self::shutdown`]）: `Weak` が切れるのを待つのではなく、
    /// [`HubInner::shutdown`] の合図で sleep を打ち切って抜ける。合図が
    /// 上がった後は spawn 自体を断るので、終了中に `resume()` が来ても
    /// 見張りは起き上がらない。
    fn spawn_supervisor(&self) {
        // 終了中は起動しない（`resume()` 側でも見ているが、ここが最後の砦）。
        if self.is_shutting_down() {
            return;
        }
        if self
            .inner
            .supervisor_spawns
            .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }
        let weak: Weak<HubInner> = Arc::downgrade(&self.inner);
        // 受信側だけをタスクへ渡す。`watch::Receiver` は watch の内部状態を
        // 掴むだけで `HubInner` を延命しないので、`Weak` の設計は変わらない。
        let mut stop = self.inner.shutdown.subscribe();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(SUPERVISOR_INTERVAL) => {}
                    // 合図（または送信側の消滅）。周期の途中でも即座に抜ける。
                    _ = stop.changed() => break,
                }
                // `service` はこのブロックの中だけで生きる - sleep を跨いで
                // 強参照を持つと `HubService` が解放されなくなる。
                let Some(inner) = weak.upgrade() else {
                    break;
                };
                let service = HubService { inner };
                // 周期の頭で合図を見る（`select!` が sleep 側を選んだ直後に
                // 合図が上がった場合の取りこぼしをここで拾う）。
                if service.is_shutting_down() {
                    break;
                }
                service.supervise_once().await;
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

        // 画面を閉じていても最終受信時刻が進むように、何をするか決める**前**に
        // 無条件で 1 回観測する（`observe_last_value` の doc 参照）。
        self.inner.subscription.lock().await.observe_last_value();

        // **レコードの有無を判断する前に hydrate する。** 写しが空なのは
        // 「本当に未設定」だけでなく「起動時の `hydrate()` が失敗した」
        // （設定 DB の一時的な読み取り失敗など）ときもあり得る。写しだけを
        // 見て諦めると、後者のときに毎周期ここで止まり**保存済みの購読が
        // 二度と再開されない**。設定 DB はローカルの SQLite で、読むのは
        // 30 秒に 1 回なので、毎周期 hydrate しても負担にならない。
        //
        // 明示操作と同じ `hydrate_keeping_pending` を通す（#395 のレビュー
        // A）: 見張りは 30 秒ごとに回るので、ここが素通しだと**未書き込みの
        // 変更を最初に潰すのはたいていこの経路**になる。逆に言えば、
        // 画面を触らなくても 30 秒ごとに書き戻しが再試行される。
        if let Err(err) = self.hydrate_keeping_pending().await {
            eprintln!(
                "banto: Hub接続設定の読み取りに失敗しました（次の周期で再試行します）: {err}"
            );
            return;
        }
        if self.inner.mirror.current().is_none() {
            return;
        }
        {
            let slot = self.inner.subscription.lock().await;
            if !needs_retry(slot.health(), slot.reason.as_deref()) {
                return;
            }
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
        let call = with_hub_timeout(self.inner.bootstrapper.refresh_catalog()).await?;
        // 保存に失敗しても**突き合わせは必ず行う**（#394 のレビュー P1-2）。
        // 設定 DB が一時的に書けないことと、購読を張り直せるかどうかは別の
        // 話で、前者で後者を止めると値が流れないまま放置される。
        let saved = self.flush().await;
        self.reconcile(&call, Trigger::Observe).await;
        saved
    }

    /// 接続（保存済みキーがあれば再利用、無ければ試運転中のみ自己発行）。
    ///
    /// キーが増えた／差し替わった可能性があるので、購読は
    /// [`Trigger::CredentialsChanged`] で**必ず張り直す**。
    pub async fn connect(&self, endpoint: &str) -> Result<HubView, BantoError> {
        let _operation = self.begin_operation().await?;
        // **Hub 側にキーを発行させる唯一の経路**なので上限は
        // [`HUB_MUTATING_TIMEOUT`]（#395 のレビュー B）。打ち切りは Hub 側の
        // 発行を取り消せないので、その可能性を理由に載せる。
        let call = with_timeout(
            HUB_MUTATING_TIMEOUT,
            || hub_mutating_timeout_reason(&self.inner.installation_id),
            self.inner.bootstrapper.connect(endpoint.trim()),
        )
        .await?;
        let saved = self.flush().await;
        self.finish(saved, call, Trigger::CredentialsChanged).await
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
        // **Hub 側に副作用は無い**ので読み取り側の上限（#395 のレビュー P2）。
        // この経路がネットワークに出るのは貼られたキーの検証
        // （`verify()` = `GET /api/v1/tags`）だけで、成功後に書くのは OS
        // キーリングと設定というローカル 2 つ。Hub 側のキーを発行も失効も
        // しない（`previous_for` は保存済みの記録を読むだけ）ので、`connect`
        // のような「打ち切ったら Hub にキーが残るかもしれない」窓が無い。
        let call = with_hub_timeout(
            self.inner
                .bootstrapper
                .adopt_manual_key(endpoint.trim(), key),
        )
        .await?;
        let saved = self.flush().await;
        self.finish(saved, call, Trigger::CredentialsChanged).await
    }

    /// タグ一覧の再取得。
    pub async fn refresh_catalog(&self) -> Result<HubView, BantoError> {
        let _operation = self.begin_operation().await?;
        let call = with_hub_timeout(self.inner.bootstrapper.refresh_catalog()).await?;
        let saved = self.flush().await;
        self.finish(saved, call, Trigger::Observe).await
    }

    /// 選択タグの保存。空でも保存できる（受入条件）。
    ///
    /// ワイヤ形（引数・戻り値）は #332 のまま。保存したあとに catalog を
    /// 1 回読み直してから購読を張り直すだけで、**追加の 1 往復はユーザー
    /// 操作なので許容**する（画面のポーリングはこの経路を通らない）。
    /// 読み直しに失敗しても**保存は成功のまま返す** - 保存の成否と購読の
    /// 張り直しは別事象で、前者を後者の失敗で覆さない。逆に、設定 DB への
    /// 保存（[`Self::flush`]）が失敗したときは**失敗を返す**が、その前に
    /// 購読の突き合わせ・停止まで必ず済ませる（#394 のレビュー P1-2）。
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
        // 保存の失敗（`saved`）は最後に返すが、**途中で抜けない**
        // （#394 のレビュー P1-2）。抜けると購読が古い選択のまま残り、
        // 「保存もされず、購読も直らない」という一番悪い形になる。
        let saved = self.flush().await;
        match with_hub_timeout(self.inner.bootstrapper.refresh_catalog()).await {
            Ok(mut call) => {
                // 打ち切ったときも**保存は成功のまま**返す（再取得は best
                // effort）。ただし理由は「保存できている」ことが伝わる方に
                // 差し替える（#395 のレビュー対応）。状態自体は
                // `Unreachable` なので、突き合わせが古い世代を止める。
                if call.timed_out_flag() {
                    call.reason_override = Some(selection_saved_hub_timeout_reason());
                }
                let saved_again = self.flush().await;
                self.reconcile(&call, Trigger::Observe).await;
                self.saved_outcome(saved.and(saved_again))?;
            }
            Err(err) => {
                eprintln!(
                    "banto: 選択タグ保存後のHubタグ一覧の再取得に失敗しました（古い購読を止めて再試行を待ちます）: {err}"
                );
                let mut slot = self.inner.subscription.lock().await;
                // 選択が変わったなら、前の選択についての最終受信時刻はもう
                // 別の購読の話（同じ選択で保存し直しただけなら残る）。
                let identity = subscription_identity(self.inner.mirror.current().as_ref());
                slot.retain_last_value_for(identity.as_ref());
                // 未解決・購読不可は**直前の選択**を catalog と突き合わせた
                // 結果なので、選択が変わった今はもう何も語っていない。新しい
                // 選択と並べて出すと理由（再取得できなかった）と中身が食い違う
                // ため、`reconcile_with` の未接続分岐と同じく伏せる。
                slot.unresolved.clear();
                slot.unsupported.clear();
                slot.stop(Some(REASON_SELECTION_CHANGED_REFRESH_FAILED.to_owned()))
                    .await;
                // 購読を整合させた**後で**保存の失敗を返す（上と同じ理由）。
                saved?;
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
        // ここも「保存の失敗より先に購読を整合させる」（#394 のレビュー
        // P1-2）。切断したのに購読だけ生きている、という形を作らない。
        let saved = self.flush().await;
        let view = self.not_configured_view().await;
        saved.map(|()| view)
    }

    /// 報告すべき保存の失敗があるか（#395 のレビュー C）。
    ///
    /// [`Self::flush`] は写しが dirty な限り同じ変更を書き直すので、
    /// **2 回目の呼び出しは 1 回目の再試行でもある**。1 回目が一時的な理由
    /// （設定 DB のロックなど）で失敗し、2 回目で書けたのなら保存は成立して
    /// いる - そこで「保存できませんでした」と返すと、実際には保存できて
    /// いるのに UI が失敗を出す（嘘の表示になる）。
    ///
    /// 判断は結果の組み合わせではなく**写しに未書き込みが残っているか**で
    /// 行う: これが `flush` の事後条件そのもので、`flush` を何回呼んでも
    /// 同じ規準で答えられる。
    fn saved_outcome(&self, reported: Result<(), BantoError>) -> Result<(), BantoError> {
        if self.inner.mirror.pending().is_none() {
            return Ok(());
        }
        reported
    }

    /// 設定の保存結果と、突き合わせ済みの [`HubView`] を 1 つの答えにする。
    ///
    /// **`saved` を先に受け取る形にしてあるのが肝**（#394 のレビュー P1-2）:
    /// `flush()` の結果を `?` で即座に返していた頃は、保存に失敗すると
    /// [`Self::view`] に到達せず**購読の突き合わせが丸ごと飛んだ**。呼び出し
    /// 側が `self.flush().await` の戻り値をここへ渡す限り、突き合わせの前に
    /// 抜ける書き方はできない。
    async fn finish(
        &self,
        saved: Result<(), BantoError>,
        call: HubCall,
        trigger: Trigger,
    ) -> Result<HubView, BantoError> {
        let view = self.view(call, trigger).await;
        saved.map(|()| view)
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
    async fn view(&self, call: HubCall, trigger: Trigger) -> HubView {
        self.reconcile(&call, trigger).await;
        let record = self.inner.mirror.current();
        let tags = call
            .connection
            .catalog
            .as_ref()
            .map(|catalog| catalog.tags.iter().map(HubTagView::from).collect());
        HubView::new(
            call.connection.status,
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
    async fn reconcile(&self, call: &HubCall, trigger: Trigger) {
        self.reconcile_with_reason(
            &call.connection.status,
            call.connection.catalog.as_ref(),
            trigger,
            call.reason_override.as_deref(),
        )
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
        self.reconcile_with_reason(status, catalog, trigger, None)
            .await;
    }

    /// [`Self::reconcile_with`] の本体。`reason_override` は「この往復は
    /// タイムアウトで打ち切った」のように、**状態だけでは言えない事情**を
    /// 購読の理由に載せるためのもの（#395 のレビュー対応）。状態からの通常の
    /// 分類（`NotConfigured` / `AuthFailed` / それ以外）より優先する -
    /// `Unreachable` に丸めた結果「Hubに接続できていないため購読していません」
    /// だけが出ると、**15 秒で打ち切ったという事実**が消えてしまうため。
    ///
    /// 世代を張れる（`Connected` + catalog あり）経路では使われない: そこは
    /// 購読が成立しているので、打ち切りの話はもう関係ない。
    async fn reconcile_with_reason(
        &self,
        status: &HubStatus,
        catalog: Option<&CatalogSnapshot>,
        trigger: Trigger,
        reason_override: Option<&str>,
    ) {
        let record = self.inner.mirror.current();
        let identity = subscription_identity(record.as_ref());
        let mut slot = self.inner.subscription.lock().await;
        // 「同じ購読が止まっているだけ」なら最終受信時刻を残し、別物になった
        // ／無くなったなら捨てる。世代を張る前に決める。
        slot.retain_last_value_for(identity.as_ref());

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
            let reason = match (reason_override, status) {
                (Some(reason), _) => reason.to_owned(),
                (None, HubStatus::NotConfigured) => REASON_NOT_CONFIGURED.to_owned(),
                // `AuthFailed` は「キーが無い」と「キーが拒否された」の両方で
                // 返る: `Bootstrapper::refresh_catalog()` はキーリングに
                // エントリが無いと `rest_client()` に到達する前にこれを返す。
                // 原因も次の一手も違うので、ここで分ける。`rest_client()` は
                // keyring を読むだけで**ネットワークを叩かない**ので、この
                // 確認で往復は増えない。
                (None, HubStatus::AuthFailed) => match self.inner.bootstrapper.rest_client() {
                    Ok(None) => REASON_NO_KEY.to_owned(),
                    _ => REASON_KEY_REJECTED.to_owned(),
                },
                (None, _) => REASON_NOT_CONNECTED.to_owned(),
            };
            slot.stop(Some(reason)).await;
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
        // 終了中は**張り直さない**（[`Self::shutdown`]）。見張りの 1 周期が
        // Hub への往復の途中で終了処理と行き違ったときに、ここまで来てしまう
        // - そのまま `start()` すると、終了処理が閉じた直後に新しい WS が
        // 1 本立ったままプロセスが終わる。古い世代は上の `stop()` で既に
        // 落ちているので、理由だけ差し替えて抜ける。
        if self.is_shutting_down() {
            slot.reason = Some(REASON_SHUTTING_DOWN.to_owned());
            return;
        }
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
                // 最終受信時刻がどの購読のものかを記録する（残すか捨てるかは
                // 上の `retain_last_value_for` が既に決めている - 同じ接続先・
                // 同じ選択のまま張り直したときは残る）。
                slot.last_value_from = identity;
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
    ///
    /// **このガードを持ったまま外部を待つ経路は、必ず上限を付けること**
    /// （#394 のレビュー P1-1）。共有 crate の HTTP クライアントには
    /// タイムアウトが無く、1 箇所でも包み忘れると無応答の相手 1 回で Hub
    /// 機能全体が固まる。上限は 2 本あり、**どちらを使うかは副作用の有無で
    /// 決める**（#395 のレビュー B / P2）: Hub 側に何も残らないなら
    /// [`with_hub_timeout`]（[`HUB_OPERATION_TIMEOUT`]。読み取りと、保存先が
    /// ローカルだけの `adopt_manual_key`）、Hub にキーを発行させる `connect`
    /// だけは [`with_timeout`] + [`HUB_MUTATING_TIMEOUT`]。打ち切りは
    /// **こちらが待つのをやめるだけ**で、Hub が既に処理した要求は取り消せない。
    async fn begin_operation(&self) -> Result<AsyncMutexGuard<'_, ()>, BantoError> {
        let guard = self.inner.operation.lock().await;
        self.hydrate_keeping_pending().await?;
        Ok(guard)
    }

    /// 操作の入口の hydrate。**未書き込みの変更を先に書き切り、書けなければ
    /// hydrate しない**（#395 のレビュー A）。
    ///
    /// **なぜ要るか**: [`Self::hydrate`] は [`SettingsMirror::reset`] で
    /// `dirty` を落とす。したがって「[`Self::flush`] が失敗して写しに変更が
    /// 残っている」状態で次の操作（や見張りの周期）が素通しで hydrate すると、
    /// **DB の古い値（記録が書けていなければ『レコード無し』）で写しが
    /// 上書きされ、dirty も消える**。P1-2 で残したはずの「次の機会に書き直す」
    /// 経路がここで断ち切られ、発行済みキーの記録がメモリからも消えて
    /// しまう - 直そうとしていた孤児キーがそのまま残る。
    ///
    /// **規律**: `dirty` な写しを `hydrate()` が古い DB 値で上書きしない。
    /// 書き直せたときだけ DB を正とする。
    ///
    /// 書き戻しに失敗した回は**写しを保ったまま進む**（操作自体は失敗させ
    /// ない）。写しはメモリ上の最新であり、続く `flush()` が同じ変更を
    /// もう一度試して、そこで初めて呼び出し元に失敗が返る。
    async fn hydrate_keeping_pending(&self) -> Result<(), BantoError> {
        if self.inner.mirror.pending().is_some() {
            if let Err(err) = self.flush().await {
                eprintln!(
                    "banto: 未保存のHub接続設定を書き戻せませんでした（写しは保持し、この回は設定DBで上書きしません）: {err}"
                );
                return Ok(());
            }
        }
        self.hydrate().await
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
    ///
    /// **書けてから dirty を落とす**（[`SettingsMirror::mark_flushed`] に
    /// 理由）。失敗しても写しは dirty のまま残るので、次の操作や見張りの
    /// `flush()` が同じ変更を書き直せる。
    ///
    /// 失敗の文言は**接続の失敗と混ぜない**: ここで失敗しているのは
    /// 「ローカルの設定 DB への保存」であって、Hub との通信ではない
    /// （Hub 側の処理は既に終わっている）。
    ///
    /// **残留リスク（完全には防げない）**: `connect` は「Hub にキーを発行
    /// させる → キーリングに保存 → 写しに保存」まで済ませてからここへ来る
    /// ので、ここで失敗したまま**アプリが落ちれば**（あるいは設定 DB が
    /// 壊れたままなら）、発行済みのキーが Hub 側に残り、こちらはその
    /// `key_id` を知らないので失効させられない。自動失効は入れない -
    /// まだ使うかもしれないキーを消す方が危険で、「自分が発行したものだけ・
    /// 確証があるときだけ失効させる」（#387 で確立した規律）にも反する。
    /// 運用側は Hub の APIキー一覧で名前の接頭辞
    /// `chronogazer-{installation_id}-`（`Bootstrapper` の `key_name`）から
    /// このインストールのキーを見つけて、不要な世代を手で失効できる。
    async fn flush(&self) -> Result<(), BantoError> {
        let Some((revision, change)) = self.inner.mirror.pending() else {
            return Ok(());
        };
        let value = match change {
            Some(record) => serde_json::to_string(&record).map_err(|err| {
                BantoError::Storage(format!("Hub接続設定のシリアライズに失敗しました: {err}"))
            })?,
            None => String::new(),
        };
        self.inner
            .settings
            .set(KEY_HUB_RECORD, &value)
            .await
            .map_err(|err| {
                BantoError::Storage(format!(
                    "Hub接続設定を保存できませんでした（Hubとの通信ではなくローカル設定の保存に失敗しています。次の操作で自動的に書き直します）: {err}"
                ))
            })?;
        self.inner.mirror.mark_flushed(revision);
        Ok(())
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
        let (_pool, settings, hub) = service_with_pool().await;
        (settings, hub)
    }

    /// [`service`] と同じだが、設定 DB のプールも返す。**プールを閉じると
    /// 以後の書き込みが失敗する**ので、`flush()` の失敗経路を実際に踏ませる
    /// テスト（#394 のレビュー P1-2）に使う。
    async fn service_with_pool() -> (sqlx::SqlitePool, SettingsService, HubService) {
        let pool = init_db_memory().await.expect("init_db_memory");
        let settings = SettingsService::new(pool.clone());
        let hub = HubService::new(settings.clone(), Arc::new(UnavailableKeyStore))
            .await
            .expect("HubService::new");
        (pool, settings, hub)
    }

    fn seed_record(selected: &[&str]) -> HubRecord {
        HubRecord {
            endpoint: "http://127.0.0.1:3100".to_owned(),
            installation_id: "inst".to_owned(),
            key_id: Some(3),
            key_name: Some("chronogazer-inst-1000".to_owned()),
            keyring_account: "hub:127.0.0.1:3100/:inst".to_owned(),
            selected_tags: selected.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    // --- #394 レビュー P1-1: 無応答の Hub で操作ロックを握り続けない -------

    /// 応答しない相手（`pending()` = 永久に解決しない future）でも
    /// [`HUB_OPERATION_TIMEOUT`] で必ず打ち切り、**エラーではなく 6 状態の
    /// `Unreachable`** として返す（#395 のレビュー対応）。`start_paused` なので
    /// **実時間は 1 秒も待たない**（時計はランタイムが進める）。
    #[tokio::test(start_paused = true)]
    async fn a_hub_call_that_never_answers_becomes_unreachable() {
        let started = tokio::time::Instant::now();
        let call =
            with_hub_timeout(std::future::pending::<Result<HubConnection, BootstrapError>>())
                .await
                .expect("打ち切りはエラーではなく状態で返す");
        assert!(
            started.elapsed() >= HUB_OPERATION_TIMEOUT,
            "上限までは待つ（短気に切らない）"
        );
        assert_eq!(
            call.connection.status,
            HubStatus::Unreachable {
                cause: banto_hub_bootstrap::UnreachableCause::Transport
            },
            "状態表示が実態と一致する（『接続済み』のまま残さない）"
        );
        assert!(
            call.connection.catalog.is_none(),
            "読めていない catalog を空のスナップショットで表さない"
        );
        let reason = call
            .reason_override
            .expect("打ち切ったという具体性を理由に載せる");
        assert!(
            reason.contains(&HUB_OPERATION_TIMEOUT.as_secs().to_string()),
            "もう待っていないことが分かるよう上限を出す: {reason}"
        );
    }

    /// 応答しないリスナ（TCP は繋がるが 1 バイトも返さない＝この PR が
    /// 直している「無応答」そのもの）を相手に、**実際の操作経路**で打ち切りが
    /// 起きることを確かめるための足場。
    ///
    /// **なぜ手で時計を進めるか**: `tokio::time::pause()` 中のランタイムは
    /// 「暇になった瞬間」に時計を一番近い期限まで飛ばす。sqlite の問い合わせは
    /// blocking スレッドへ出るので、`begin_operation()` の `hydrate()` の間
    /// ランタイムは暇になり、そこで時計が飛ぶと **sqlx のプール取得（既定
    /// 30 秒）が即タイムアウトする** - 実際に 5〜8% の頻度で
    /// `pool timed out while waiting for an open connection` として落ちた。
    /// そこで「常に実行可能なタスク」を 1 本置いてランタイムを暇にさせず、
    /// **Hub へ接続が届いた（= DB の仕事が終わり、打ち切りのタイマーも
    /// 仕掛かっている）のを確かめてから**自動進行を許す。実時間は待たない。
    async fn drive_until_the_hub_is_reached<F>(
        operation: F,
        reached: &mut tokio::sync::mpsc::UnboundedReceiver<()>,
    ) -> F::Output
    where
        F: std::future::Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let spinner = tokio::spawn(async {
            loop {
                tokio::task::yield_now().await;
            }
        });
        let handle = tokio::spawn(operation);
        reached
            .recv()
            .await
            .expect("Hub への接続がリスナに届く（ここまでに DB の読み取りは終わっている）");
        // ここから先は誰も DB を触らないので、時計が飛んでよい。
        spinner.abort();
        handle.await.expect("join")
    }

    /// accept だけして**何も返さず**接続を握り続けるリスナ（drop すると
    /// 切断＝応答になってしまうので保持する）。接続が届くたびに通知する。
    async fn silent_hub() -> (
        String,
        tokio::task::JoinHandle<()>,
        tokio::sync::mpsc::UnboundedReceiver<()>,
    ) {
        use tokio::net::TcpListener;

        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let (reached_tx, reached_rx) = tokio::sync::mpsc::unbounded_channel();
        let server = tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((stream, _)) = listener.accept().await {
                held.push(stream);
                let _ = reached_tx.send(());
            }
        });
        (endpoint, server, reached_rx)
    }

    /// 打ち切りが**購読まで届く**こと: 状態は `Unreachable`、購読は理由付きで
    /// 停止、catalog は `null`（0 件ではない）。
    #[tokio::test]
    async fn a_hub_that_never_answers_stops_the_subscription_with_the_timeout_reason() {
        let (endpoint, silent, mut reached) = silent_hub().await;
        let (_settings, hub) = service_with_keyring(&endpoint, &["alpha"]).await;

        tokio::time::pause();
        let probe = hub.clone();
        let view =
            drive_until_the_hub_is_reached(async move { probe.status().await }, &mut reached)
                .await
                .expect("打ち切りはエラーにしない");

        assert_eq!(
            view.status.as_str(),
            "unreachable",
            "状態表示が実態と一致する"
        );
        assert!(view.tags.is_none(), "読めていない一覧を 0 件にしない");
        assert_eq!(
            view.subscription.state, "stopped",
            "接続できていないのに購読が live のまま残らない"
        );
        let reason = view.subscription.reason.expect("停止の理由を出す");
        assert!(
            reason.contains(&HUB_OPERATION_TIMEOUT.as_secs().to_string()),
            "『到達不能』に丸めず、打ち切った具体性を残す: {reason}"
        );

        silent.abort();
    }

    /// 打ち切りの上限と文言は**経路で変える**（#395 のレビュー P2）。
    ///
    /// * `adopt_manual_key` は Hub 側に副作用を持たない（ネットワークに出るのは
    ///   貼られたキーの検証だけで、保存先は keyring と設定というローカル 2 つ。
    ///   `previous_for` は保存済みの記録を読むだけ）。15 秒で切ってよく、
    ///   ここで「Hub 側にAPIキーが作成されている可能性」と出すのは**誤情報**。
    /// * `connect` は Hub にキーを発行させるので、打ち切ると本当に残り得る。
    ///   60 秒まで待ち、残留の可能性と探し方を伝える。
    #[tokio::test]
    async fn only_the_key_issuing_path_warns_about_a_key_left_on_the_hub() {
        const ORPHAN_WARNING: &str = "APIキーが作成されている可能性";

        let (endpoint, silent, mut reached) = silent_hub().await;
        let (_settings, hub) = service_with_keyring(&endpoint, &["alpha"]).await;

        tokio::time::pause();

        // 手動キーの採用: Hub 側には何も残らないので、孤児キーの案内は出さない。
        let adopting = hub.clone();
        let target = endpoint.clone();
        let adopted = drive_until_the_hub_is_reached(
            async move {
                adopting
                    .adopt_manual_key(&target, "bh_abcd1234_opaque-secret".to_owned())
                    .await
            },
            &mut reached,
        )
        .await
        .expect("打ち切りはエラーにしない");
        assert_eq!(adopted.status.as_str(), "unreachable");
        let reason = adopted.subscription.reason.expect("停止の理由を出す");
        assert!(
            reason.contains(&HUB_OPERATION_TIMEOUT.as_secs().to_string()),
            "読み取り側の上限で切る: {reason}"
        );
        assert!(
            !reason.contains(ORPHAN_WARNING),
            "Hub 側にキーを作らない経路で「残っているかも」と言わない: {reason}"
        );

        // キーの発行を伴う接続: こちらは残り得るので、探し方まで伝える。
        let connecting = hub.clone();
        let target = endpoint.clone();
        let connected = drive_until_the_hub_is_reached(
            async move { connecting.connect(&target).await },
            &mut reached,
        )
        .await
        .expect("打ち切りはエラーにしない");
        assert_eq!(connected.status.as_str(), "unreachable");
        let reason = connected.subscription.reason.expect("停止の理由を出す");
        assert!(
            reason.contains(ORPHAN_WARNING),
            "発行を伴う経路では残留の可能性を伝える: {reason}"
        );
        assert!(
            reason.contains(&HUB_MUTATING_TIMEOUT.as_secs().to_string()),
            "副作用側の上限で切る: {reason}"
        );
        assert!(
            reason.contains(&format!("{APP_ID}-{}-", hub.inner.installation_id)),
            "実際のキー名の接頭辞を出す（運用側が Hub の一覧で探せるように）: {reason}"
        );

        silent.abort();
    }

    /// 打ち切った**後に別の操作が進める**こと。これが直したかったもの:
    /// 以前は無応答 1 回で操作ロックが永久に握られ、`status`/`connect`/
    /// 見張りまで全部止まった（復旧はアプリ再起動のみ）。
    ///
    /// [`HubService::begin_operation`] と同じ形（操作ロックを持ったまま
    /// 外部を待つ）を、ネットワークを使わずに最小限で再現する。
    #[tokio::test(start_paused = true)]
    async fn the_operation_lock_is_released_after_a_timeout() {
        let operation = Arc::new(AsyncMutex::new(()));
        let holder = Arc::clone(&operation);
        let (acquired_tx, acquired_rx) = tokio::sync::oneshot::channel();
        let hung = tokio::spawn(async move {
            let _guard = holder.lock().await;
            acquired_tx.send(()).expect("receiver is alive");
            with_hub_timeout(std::future::pending::<Result<HubConnection, BootstrapError>>()).await
        });
        acquired_rx.await.expect("無応答の操作がロックを取った");

        // 上限の 4 倍を上限に待つ - 直す前はここで永久に待たされた。
        let next = tokio::time::timeout(HUB_OPERATION_TIMEOUT * 4, operation.lock()).await;
        assert!(next.is_ok(), "タイムアウトでガードが落ち、次の操作が進める");
        assert!(
            hung.await
                .expect("join")
                .expect("打ち切りはエラーにしない")
                .timed_out_flag(),
            "打ち切られた側は『打ち切った』と分かる形で返る"
        );
    }

    // --- #394 レビュー P1-2: 書けてから dirty を落とす ---------------------

    /// 書き込みが成功するまで変更を手放さない。以前は書く**前**に dirty を
    /// 落としていたため、`SettingsService::set` が失敗すると変更が二度と
    /// 書かれず、Hub 上の API キーが孤児になった。
    #[test]
    fn the_mirror_keeps_a_change_until_the_write_succeeds() {
        let mirror = SettingsMirror::default();
        assert!(mirror.pending().is_none(), "変更が無ければ何も書かない");

        mirror.save(&seed_record(&["a"])).unwrap();
        let (revision, change) = mirror.pending().expect("書き戻すべき変更がある");
        assert_eq!(change.expect("消去ではない").selected_tags, owned(&["a"]));
        // ここで書き込みが失敗したとする（= `mark_flushed` を呼ばない）。
        assert!(
            mirror.pending().is_some(),
            "失敗したら次の機会に書き直せること"
        );

        // 成功したときだけ落ちる。
        mirror.mark_flushed(revision);
        assert!(mirror.pending().is_none());
    }

    /// 書いている最中に写しがさらに変わったら、**古い版の成功で新しい変更を
    /// 消さない**（`hydrate` が上書きする前に書き直す機会を残す）。
    #[test]
    fn a_change_made_while_writing_is_not_dropped() {
        let mirror = SettingsMirror::default();
        mirror.save(&seed_record(&["a"])).unwrap();
        let (revision, _) = mirror.pending().expect("変更がある");

        mirror.save(&seed_record(&["b"])).unwrap();
        mirror.mark_flushed(revision);

        let (_, change) = mirror.pending().expect("新しい変更は残る");
        assert_eq!(change.expect("消去ではない").selected_tags, owned(&["b"]));
    }

    /// 消去（`clear`）も同じ規律で扱う（`Some(None)` = 空文字列の upsert）。
    #[test]
    fn clearing_the_mirror_is_a_pending_erase() {
        let mirror = SettingsMirror::default();
        mirror.save(&seed_record(&["a"])).unwrap();
        mirror.clear().unwrap();
        let (revision, change) = mirror.pending().expect("消去も書き戻す変更");
        assert!(change.is_none(), "`Some(None)` は消去");
        mirror.mark_flushed(revision);
        assert!(mirror.pending().is_none());
    }

    /// 設定 DB へ書けなくても、**購読の突き合わせには必ず到達する**
    /// （以前は `flush().await?` で早期 return し、購読が古いまま残った）。
    /// 失敗は「保存できなかった」として呼び出し元に返り、変更は写しに
    /// 残って次の機会に書き直せる。
    #[tokio::test]
    async fn a_failed_flush_still_reconciles_and_keeps_the_change() {
        let (pool, settings, hub) = service_with_pool().await;
        settings
            .set(
                KEY_HUB_RECORD,
                &serde_json::to_string(&seed_record(&["a"])).unwrap(),
            )
            .await
            .unwrap();
        hub.hydrate().await.unwrap();
        // 写しだけを変える（= flush が書くべき変更がある状態）。
        hub.inner.mirror.save(&seed_record(&["b"])).unwrap();
        // 以後、設定 DB への書き込みは必ず失敗する。
        pool.close().await;

        let err = hub
            .resume_locked()
            .await
            .expect_err("保存の失敗は呼び出し元に返る");
        assert!(
            matches!(err, BantoError::Storage(_)),
            "接続の失敗ではなく保存の失敗として返る: {err}"
        );
        assert!(
            err.to_string().contains("保存できませんでした"),
            "文言が接続の失敗と混ざらない: {err}"
        );

        // 突き合わせには到達している（理由付きで止まっている）。
        let view = hub.subscription().await;
        assert_eq!(view.state, "stopped");
        assert_eq!(
            view.reason.as_deref(),
            Some(REASON_NO_KEY),
            "購読は突き合わせ済みの理由を持つ"
        );
        // 変更は失われていない。
        let (_, change) = hub.inner.mirror.pending().expect("次の機会に書き直せる");
        assert_eq!(change.expect("消去ではない").selected_tags, owned(&["b"]));
    }

    // --- #395 レビュー A: dirty な写しを hydrate で潰さない -----------------

    /// 設定 DB への**書き込みだけ**を失敗させる（読み取りは通る）。
    ///
    /// プールを閉じる方法だと復旧できないので、「次の機会に書き直す」という
    /// P1-2 の本題を確かめられない。`INSERT`/`UPDATE` の両方を塞ぐのは、
    /// `SettingsService::set` が upsert（`ON CONFLICT DO UPDATE`）だから。
    async fn block_settings_writes(pool: &sqlx::SqlitePool) {
        for sql in [
            "CREATE TRIGGER settings_block_insert BEFORE INSERT ON settings \
             BEGIN SELECT RAISE(ABORT, 'simulated settings write failure'); END",
            "CREATE TRIGGER settings_block_update BEFORE UPDATE ON settings \
             BEGIN SELECT RAISE(ABORT, 'simulated settings write failure'); END",
        ] {
            sqlx::query(sql)
                .execute(pool)
                .await
                .expect("create trigger");
        }
    }

    async fn unblock_settings_writes(pool: &sqlx::SqlitePool) {
        for sql in [
            "DROP TRIGGER settings_block_insert",
            "DROP TRIGGER settings_block_update",
        ] {
            sqlx::query(sql).execute(pool).await.expect("drop trigger");
        }
    }

    /// 書き込みが失敗している状態の写しと DB を用意する（3 つのテストの共通
    /// 足場）。DB には `["a"]`、写しには未書き込みの `["b"]` が残る。
    async fn service_with_a_failed_write() -> (sqlx::SqlitePool, SettingsService, HubService) {
        let (pool, settings, hub) = service_with_pool().await;
        settings
            .set(
                KEY_HUB_RECORD,
                &serde_json::to_string(&seed_record(&["a"])).unwrap(),
            )
            .await
            .unwrap();
        hub.hydrate().await.unwrap();
        hub.inner.mirror.save(&seed_record(&["b"])).unwrap();

        block_settings_writes(&pool).await;
        let err = hub
            .resume_locked()
            .await
            .expect_err("書き込みを塞いでいるので保存は失敗する");
        assert!(matches!(err, BantoError::Storage(_)), "{err}");
        assert!(
            hub.inner.mirror.pending().is_some(),
            "失敗した変更は写しに残る"
        );
        (pool, settings, hub)
    }

    async fn stored_record(settings: &SettingsService) -> String {
        settings
            .get(KEY_HUB_RECORD)
            .await
            .unwrap()
            .expect("レコードの行がある")
    }

    /// 直したかった本丸: `flush()` が失敗したあと、**次の操作**が
    /// `hydrate()` で写しを古い DB 値に戻してしまうと、変更は二度と書かれない
    /// （発行済みキーの記録がメモリからも消える）。DB が復旧したら、次の操作の
    /// 入口で**実際に DB へ書かれる**ところまでを通しで固定する。
    #[tokio::test]
    async fn a_pending_change_is_written_by_the_next_operation_once_the_db_recovers() {
        let (pool, settings, hub) = service_with_a_failed_write().await;
        assert!(
            !stored_record(&settings).await.contains("\"b\""),
            "まだ DB には届いていない"
        );

        unblock_settings_writes(&pool).await;
        // `status()` は `begin_operation()` を通る = 明示操作の入口。
        hub.status().await.expect("読み取りだけの操作は成功する");

        assert!(
            hub.inner.mirror.pending().is_none(),
            "書き切れたので未書き込みは残らない"
        );
        let stored = stored_record(&settings).await;
        assert!(
            stored.contains("\"b\""),
            "写しの変更が DB に届いている: {stored}"
        );
    }

    /// 見張りの経路も同じ扱い。30 秒ごとに回るので、素通しだと**未書き込みを
    /// 最初に潰すのはたいていこちら**になる。逆に言えば、画面を触らなくても
    /// 書き戻しが再試行される。
    #[tokio::test]
    async fn the_supervisor_retries_a_pending_write_instead_of_dropping_it() {
        let (pool, settings, hub) = service_with_a_failed_write().await;

        unblock_settings_writes(&pool).await;
        hub.supervise_once().await;

        assert!(hub.inner.mirror.pending().is_none());
        let stored = stored_record(&settings).await;
        assert!(
            stored.contains("\"b\""),
            "見張りが未書き込みを書き戻す: {stored}"
        );
    }

    // --- #395 レビュー C: 2 回目で書けたら「保存失敗」と言わない ------------

    /// `flush()` は dirty な限り同じ変更を書き直すので、2 回目は 1 回目の
    /// 再試行でもある。1 回目が一時的に失敗しても、2 回目で書けたなら保存は
    /// 成立していて、そこで「保存できませんでした」と返すのは嘘になる。
    #[tokio::test]
    async fn a_retried_flush_that_succeeds_is_not_reported_as_a_failure() {
        let (pool, settings, hub) = service_with_a_failed_write().await;
        assert!(
            hub.saved_outcome(Err(BantoError::Storage("1回目の失敗".to_owned())))
                .is_err(),
            "まだ書けていない間は失敗として報告する"
        );

        unblock_settings_writes(&pool).await;
        hub.flush().await.expect("2 回目は同じ変更を書き直せる");

        assert!(hub.inner.mirror.pending().is_none(), "dirty が解消する");
        assert!(
            hub.saved_outcome(Err(BantoError::Storage("1回目の失敗".to_owned())))
                .is_ok(),
            "2 回目で書けたのに 1 回目の失敗を返さない"
        );
        let stored = stored_record(&settings).await;
        assert!(stored.contains("\"b\""), "実際に書かれている: {stored}");
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

    /// 別々の名前が同じ安定 ID を指す catalog（Hub 側の不整合）。そのまま
    /// `start()` へ渡すと `DuplicateRequestedStableId` で**購読全体が立たない**
    /// ので、先勝ちで 1 つだけ購読し、残りは購読できなかった名前として出す。
    #[test]
    fn plan_bindings_folds_names_that_point_at_the_same_stable_id() {
        let ids = StableTagId::new(1, 1, 1);
        let duplicated = CatalogSnapshot {
            tags: vec![catalog_tag("alpha", ids), catalog_tag("beta", ids)],
            ..catalog(&[])
        };

        let plan = plan_bindings(&owned(&["alpha", "beta"]), &duplicated);

        assert_eq!(
            plan.requests
                .iter()
                .map(|request| request.binding_key.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha"],
            "先に出てきた方だけを購読する"
        );
        assert_eq!(
            plan.unsupported,
            owned(&["beta"]),
            "落とした名前は黙って消さず一覧に出す"
        );
        assert!(
            plan.unresolved.is_empty(),
            "catalog にはあるので未解決ではない"
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
            last_error: None,
        })
    }

    /// 失敗を経験している世代。分類まで問わない場面では transport（待てば
    /// 直る側）を使う。
    fn failed(state: TagClientConnectionState) -> Option<GenerationHealth> {
        failing(state, TagErrorKind::Transport)
    }

    fn failing(state: TagClientConnectionState, kind: TagErrorKind) -> Option<GenerationHealth> {
        Some(GenerationHealth {
            state,
            last_error: Some(kind),
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
        assert!(
            must_restart_despite_same_fingerprint(Trigger::Observe, healthy(State::Rebinding)),
            "catalog が変わらない原因の rebind ループは、起こすだけでは抜けられない"
        );
        assert!(
            must_restart_despite_same_fingerprint(
                Trigger::Observe,
                failing(State::Reconnecting, TagErrorKind::RevisionMismatch)
            ),
            "メタデータ不一致はタグ集合が変わらないので、張り直さないと抜けられない"
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
            healthy(State::Reconnecting),
        ] {
            assert!(!must_restart_despite_same_fingerprint(
                Trigger::Observe,
                health
            ));
        }
    }

    /// **世代があるかぎり「起こすなら必ず張り直す」**（#388 レビュー対応）。
    ///
    /// 片方だけ真だと、見張りが catalog を取り直しても同一性一致の早期 return
    /// に落ちて何もせず、壊れたワーカーが残る。この形のバグは #385 で
    /// `Rebinding`、#383 実機で `Reconnecting`（binding 系）、#388 で
    /// `Reconnecting`（メタデータ不一致）と 3 回出た。1 つの述語
    /// （`needs_rebuild`）を両方から使い、それを表で固定することで、
    /// **構造的に入らないようにする**。
    ///
    /// 世代が無いときだけは張り直しの話にならない（張るものが無く、
    /// `reconcile_with` が新しく張る）ので、この不変条件の対象外。
    #[test]
    fn waking_the_supervisor_always_implies_actually_restarting() {
        use TagClientConnectionState as State;

        for state in [
            State::Stopped,
            State::Connecting,
            State::Handshaking,
            State::Live,
            State::Rebinding,
            State::Reconnecting,
            State::Unauthorized,
        ] {
            let mut cases = vec![healthy(state)];
            for kind in [
                TagErrorKind::Transport,
                TagErrorKind::ProtocolError,
                TagErrorKind::CatalogUnavailable,
                TagErrorKind::BindingUnresolved,
                TagErrorKind::RevisionMismatch,
                TagErrorKind::RuntimeMetadataMismatch,
                TagErrorKind::Unauthorized,
                TagErrorKind::InvalidTagSelection,
            ] {
                cases.push(failing(state, kind));
            }
            for health in cases {
                assert_eq!(
                    needs_retry(health, None),
                    must_restart_despite_same_fingerprint(Trigger::Observe, health),
                    "{state} ({health:?}): 起こすなら張り直す、が崩れている"
                );
            }
        }
    }

    /// Hub から受けた 1 スナップショットを画面の形へ写す。**未知のラベルを
    /// 丸めない**のが要点: `banto-tagclient` は知らない品質・値の出所を
    /// `Unknown(raw)` として保つので、こちらもそのまま出す（知らない品質を
    /// `good` に、知らない出所を `real` に見せない）。
    #[test]
    fn value_views_copy_a_snapshot_without_rounding_unknown_labels() {
        use banto_tagclient::{ValueEntry, ValueQuality, ValueSource};

        let snapshot = ValuesSnapshot {
            revision: 3,
            t: 1_722_758_400_123,
            run_id: Some(7),
            collection_mode: banto_tagclient::CollectionMode::Configured,
            values: vec![
                ValueEntry {
                    tag: "line1.fast.temp01".to_owned(),
                    v: Some(25.4),
                    q: ValueQuality::Good,
                    t: 1_722_758_400_100,
                    value_source: ValueSource::Real,
                },
                ValueEntry {
                    tag: "line1.fast.empty".to_owned(),
                    v: None,
                    q: ValueQuality::Unknown("future_quality".to_owned()),
                    t: 1_722_758_400_110,
                    value_source: ValueSource::Unknown("future_source".to_owned()),
                },
            ],
        };

        let views = value_views(&snapshot);
        assert_eq!(views.len(), 2);

        assert_eq!(views[0].tag, "line1.fast.temp01");
        assert_eq!(views[0].v, Some(25.4));
        assert_eq!(views[0].q, "good");
        assert_eq!(views[0].t, 1_722_758_400_100);
        assert_eq!(views[0].value_source, "real");

        // 値が無いことは 0 ではない。
        assert_eq!(views[1].v, None);
        assert_eq!(
            views[1].q, "future_quality",
            "知らない品質を good に丸めない"
        );
        assert_eq!(
            views[1].value_source, "future_source",
            "知らない出所を real に丸めない"
        );

        // 画面へ出る JSON は camelCase（`valueSource`）。
        let json = serde_json::to_value(&views[1]).unwrap();
        assert_eq!(json["valueSource"], serde_json::json!("future_source"));
        assert_eq!(json["v"], serde_json::json!(null));
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

    /// 「最後に値を受けた時刻」を残す条件と消す条件（#385 レビュー第10巡）。
    ///
    /// 一律で残すと**別の Hub・別のタグ集合の時刻**を出して嘘になり、一律で
    /// 消すと**いつまでデータが来ていたか**という記録計にとって一番効く診断が
    /// 失われる。「同じ購読が止まっているだけか」で分ける。
    #[test]
    fn the_last_value_time_is_kept_only_while_it_describes_the_same_subscription() {
        let hub_a = (
            "http://127.0.0.1:3100/api/v1/tags".to_owned(),
            owned(&["alpha", "beta"]),
        );
        let hub_b = (
            "http://127.0.0.1:3101/api/v1/tags".to_owned(),
            owned(&["alpha", "beta"]),
        );
        let fewer_tags = (hub_a.0.clone(), owned(&["alpha"]));

        // 同じ購読が止まっているだけ（`Unauthorized` / `Rebinding` / 終端 /
        // 再試行待ち）: 接続先も選択も変わらないので残す。
        assert!(keeps_last_value_at(Some(&hub_a), Some(&hub_a)));
        // 接続先が変わった / 選択タグが変わった: 別の購読なので消す。
        assert!(!keeps_last_value_at(Some(&hub_a), Some(&hub_b)));
        assert!(!keeps_last_value_at(Some(&hub_a), Some(&fewer_tags)));
        // 購読そのものが無くなった（切断・未設定）。
        assert!(!keeps_last_value_at(Some(&hub_a), None));
        // まだ何も覚えていない。
        assert!(!keeps_last_value_at(None, Some(&hub_a)));
        assert!(!keeps_last_value_at(None, None));
    }

    /// 同一性は**接続先と選択タグだけ**で、catalog を読めていなくても計算
    /// できる（「同じ購読が止まっているだけか」はまさに読めない場面で判断
    /// したい）。選択の並び順や重複では変わらない。
    #[test]
    fn the_subscription_identity_ignores_order_and_duplicates() {
        let base = keyring_record("http://127.0.0.1:3100", &["beta", "alpha"]);
        let shuffled = keyring_record("http://127.0.0.1:3100/", &["alpha", "beta", "alpha"]);
        assert_eq!(
            subscription_identity(Some(&base)),
            subscription_identity(Some(&shuffled))
        );
        assert_eq!(subscription_identity(None), None);
    }

    /// 切断すると、その Hub の受信時刻は残さない。
    #[tokio::test]
    async fn disconnecting_forgets_the_last_value_time() {
        let (_settings, hub) = service_with_keyring(&closed_endpoint(), &["alpha"]).await;
        hub.reconcile_with(
            &HubStatus::Connected { tag_count: 1 },
            Some(&catalog(&["alpha"])),
            Trigger::Observe,
        )
        .await;
        hub.inner.subscription.lock().await.last_value_at = Some(1_234);
        assert_eq!(hub.subscription().await.last_value_at, Some(1_234));

        hub.disconnect().await.unwrap();

        assert_eq!(
            hub.subscription().await.last_value_at,
            None,
            "レコードごと消えたので前の Hub の受信時刻を残さない"
        );
    }

    /// 接続先や選択タグが変わったら、前の購読の受信時刻は残さない。
    #[tokio::test]
    async fn a_different_endpoint_or_selection_forgets_the_last_value_time() {
        for changed in [
            keyring_record("http://127.0.0.1:3101", &["alpha"]),
            keyring_record("http://127.0.0.1:3100", &["beta"]),
        ] {
            let (_settings, hub) = service_with_keyring("http://127.0.0.1:3100", &["alpha"]).await;
            hub.reconcile_with(
                &HubStatus::Connected { tag_count: 1 },
                Some(&catalog(&["alpha"])),
                Trigger::Observe,
            )
            .await;
            hub.inner.subscription.lock().await.last_value_at = Some(1_234);

            hub.inner.mirror.reset(Some(changed));
            hub.reconcile_with(&HubStatus::AuthFailed, None, Trigger::Observe)
                .await;

            assert_eq!(
                hub.subscription().await.last_value_at,
                None,
                "別の購読の時刻を出すと嘘になる"
            );
        }
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

        assert!(needs_retry(None, None), "世代が無ければ張りに行く");
        assert!(needs_retry(healthy(State::Unauthorized), None));
        assert!(
            needs_retry(failed(State::Stopped), None),
            "エラーで終了した世代は自力で復帰しない"
        );
        assert!(needs_retry(healthy(State::Rebinding), None));
        for health in [
            healthy(State::Stopped),
            healthy(State::Live),
            healthy(State::Connecting),
            healthy(State::Handshaking),
            healthy(State::Reconnecting),
        ] {
            assert!(
                !needs_retry(health, None),
                "進行中・正常・張った直後には割り込まない"
            );
        }
    }

    /// **`Reconnecting` は一律放置ではない**（2026-09-17 実機で判明）。
    ///
    /// `banto-tagclient` は再バインドの回数を使い切ると `Backoff`（=
    /// `Reconnecting`）へ落ち、**古い要求セットのまま**永久に再試行する。
    /// 選択中のタグを 1 つ消すとこれに入り、残りのタグまで流れなくなる。
    /// 状態名が同じでも**分類で分ける**。
    #[test]
    fn a_reconnecting_generation_is_replanned_only_for_a_stale_request_set() {
        use TagClientConnectionState as State;

        // 要求セットと catalog の食い違い = 待っても直らない。起こして、
        // **同一性が同じでも張り直す** - `RevisionMismatch` /
        // `RuntimeMetadataMismatch` はタグ集合が変わらないので、張り直さないと
        // 見張りが 30 秒ごとに catalog を読むだけになる（#388）。
        for kind in [
            TagErrorKind::BindingUnresolved,
            TagErrorKind::RevisionMismatch,
            TagErrorKind::RuntimeMetadataMismatch,
        ] {
            let health = failing(State::Reconnecting, kind);
            assert!(
                needs_retry(health, None),
                "{} は再計画でしか直らない",
                kind.as_str()
            );
            assert!(
                must_restart_despite_same_fingerprint(Trigger::Observe, health),
                "{} は同一性が同じでも張り直す",
                kind.as_str()
            );
        }
        // 通信系 = 待てば直る。`banto-tagclient` の backoff に任せる。
        for kind in [
            TagErrorKind::Transport,
            TagErrorKind::ProtocolError,
            TagErrorKind::CatalogUnavailable,
        ] {
            let health = failing(State::Reconnecting, kind);
            assert!(
                !needs_retry(health, None),
                "{} は backoff の担当。割り込むと再接続を遅らせるだけ",
                kind.as_str()
            );
            assert!(
                !must_restart_despite_same_fingerprint(Trigger::Observe, health),
                "{} で張り直すと backoff と喧嘩する",
                kind.as_str()
            );
        }
        // 進行中の他の状態は分類を問わず放置（そちらは前に進んでいる）。
        for state in [State::Connecting, State::Handshaking, State::Live] {
            for kind in [TagErrorKind::BindingUnresolved, TagErrorKind::Transport] {
                assert!(!needs_retry(failing(state, kind), None), "{state}");
            }
        }
    }

    /// 世代が無いときは**理由で分ける**。catalog を取り直しても変わらない
    /// 停止（タグ未選択 / 全部購読不可）で 30 秒ごとに `GET /api/v1/tags` を
    /// 撃ち続けない。
    #[test]
    fn the_supervisor_does_not_poll_the_catalog_for_a_settled_stop() {
        for reason in [REASON_NO_TAGS, REASON_ALL_UNSUPPORTED] {
            assert!(
                !needs_retry(None, Some(reason)),
                "ユーザーが選び直すまで変わらない: {reason}"
            );
        }
        for reason in [
            REASON_ALL_UNRESOLVED,
            REASON_NONE_SUBSCRIBABLE,
            REASON_NO_KEY,
            REASON_NOT_CONNECTED,
            REASON_NOT_CONFIGURED,
            REASON_NOT_STARTED,
            REASON_SELECTION_CHANGED_REFRESH_FAILED,
        ] {
            assert!(
                needs_retry(None, Some(reason)),
                "Hub 側や環境が変われば直る: {reason}"
            );
        }
        assert!(needs_retry(None, None), "理由が無ければ従来どおり再試行");
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

        hub.reconcile_with(
            &HubStatus::Unreachable {
                cause: banto_hub_bootstrap::UnreachableCause::Transport,
            },
            None,
            Trigger::Observe,
        )
        .await;

        let view = hub.subscription().await;
        assert_eq!(view.state, "stopped");
        assert!(view.unresolved.is_empty());
        assert_eq!(view.reason.as_deref(), Some(REASON_NOT_CONNECTED));
    }

    /// `AuthFailed` は「キーがそもそも無い」と「キーが拒否された」の両方で
    /// 返る（`Bootstrapper::refresh_catalog()` はキーリングにエントリが
    /// 無いと `rest_client()` に到達する前にこれを返す）。原因も次の一手も
    /// 違うので、購読の理由では分ける。`UnavailableKeyStore` は前者なので、
    /// 汎用の「接続できていない」ではなく「キーを取り出せない」が出る。
    #[tokio::test]
    async fn auth_failed_without_a_retrievable_key_says_the_key_is_missing() {
        let (_settings, hub) = service().await;
        hub.inner.mirror.reset(Some(HubRecord {
            endpoint: "http://127.0.0.1:3100".to_owned(),
            installation_id: "inst".to_owned(),
            key_id: None,
            key_name: None,
            keyring_account: "hub:127.0.0.1:3100/:inst".to_owned(),
            selected_tags: owned(&["a"]),
        }));

        hub.reconcile_with(&HubStatus::AuthFailed, None, Trigger::Observe)
            .await;

        let view = hub.subscription().await;
        assert_eq!(view.state, "stopped");
        assert_eq!(view.reason.as_deref(), Some(REASON_NO_KEY));
    }

    /// キーは取り出せたのに拒否された場合は、別の理由（再接続 / 手動キーの
    /// 採用で直る）を出す。
    #[tokio::test]
    async fn auth_failed_with_a_stored_key_says_the_key_was_rejected() {
        let (_settings, hub) = service_with_keyring("http://127.0.0.1:3100", &["a"]).await;

        hub.reconcile_with(&HubStatus::AuthFailed, None, Trigger::Observe)
            .await;

        let view = hub.subscription().await;
        assert_eq!(view.state, "stopped");
        assert_eq!(view.reason.as_deref(), Some(REASON_KEY_REJECTED));
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

    // --- #383 段階1: 実際に値を受ける経路（モック Hub） ---------------------

    /// この節だけが `banto-tagclient` の内側まで含めて**実際に購読を成立
    /// させる**。閉じたポートを使う他のテストは「世代を張ったか」しか見ない
    /// ので、`state_watch` → `current()` → [`HubValueView`] の配線が壊れても
    /// 気付けない。足場（catalog / WS / REST values に応答するモック）は
    /// `crates/banto-tagclient` の `handle.rs` / `worker.rs` のテストと同じ
    /// 作法で、**固定 sleep を使わず** `state_watch` の変化を待つ。
    mod live_subscription {
        use std::sync::Mutex as StdMutex;
        use std::time::Duration;

        use banto_tagclient::{ValueEntry, ValueQuality, ValueSource};
        use futures_util::{SinkExt, StreamExt};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::{TcpListener, TcpStream};
        use tokio_tungstenite::{accept_async, tungstenite::Message};

        use super::*;

        /// `TagClientHandle` が使う購読 ID（`banto-tagclient` の
        /// `handle::SUBSCRIPTION_ID`）。data フレームの `id` がこれと一致して
        /// いないと `ProtocolError` で捨てられる。
        const SUBSCRIPTION_ID: i64 = 1;

        /// モック Hub が返すもの。テストの途中で差し替えられる（タグが Hub
        /// から消える場面を作るため）。
        #[derive(Default)]
        struct MockHub {
            catalog: StdMutex<String>,
            values: StdMutex<String>,
            frame: StdMutex<String>,
        }

        impl MockHub {
            fn set(&self, catalog: &CatalogSnapshot, values: &ValuesSnapshot, frame: String) {
                *self.catalog.lock().unwrap() = serde_json::to_string(catalog).unwrap();
                *self.values.lock().unwrap() = serde_json::to_string(values).unwrap();
                *self.frame.lock().unwrap() = frame;
            }
        }

        async fn read_http_request(stream: &mut TcpStream) {
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            loop {
                let count = stream.read(&mut buffer).await.unwrap_or(0);
                if count == 0 {
                    return;
                }
                request.extend_from_slice(&buffer[..count]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    return;
                }
            }
        }

        async fn write_response(stream: &mut TcpStream, body: String) {
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes()).await;
        }

        /// 受け取ったリクエストの見出しだけを覗く（WS のアップグレードか、
        /// どの REST ルートかを判別するため。`accept_async` は生のストリームを
        /// 要求するので、消費せずに `peek` する）。
        async fn peek_head(stream: &TcpStream) -> String {
            let mut head = [0_u8; 1024];
            for _ in 0..50 {
                let count = stream.peek(&mut head).await.unwrap_or(0);
                let text = String::from_utf8_lossy(&head[..count]).to_ascii_lowercase();
                if text.contains("\r\n\r\n") {
                    return text;
                }
                tokio::task::yield_now().await;
            }
            String::from_utf8_lossy(&head).to_ascii_lowercase()
        }

        /// 要求に応じて応答し続けるモック Hub。ワーカーは 1 世代につき
        /// catalog → WS（subscribe を待って data フレームを 1 つ）→ REST
        /// values の順に叩き（`worker::run_attempt`）、再バインドのたびに
        /// catalog を取り直す。何回来ても答えられるようにループにしてある。
        async fn serve_hub(listener: TcpListener, hub: Arc<MockHub>) {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let head = peek_head(&stream).await;
                if head.contains("upgrade: websocket") {
                    let frame = hub.frame.lock().unwrap().clone();
                    tokio::spawn(async move {
                        let Ok(mut socket) = accept_async(stream).await else {
                            return;
                        };
                        let subscribe =
                            tokio::time::timeout(Duration::from_secs(5), socket.next()).await;
                        if !matches!(subscribe, Ok(Some(Ok(Message::Text(_))))) {
                            return;
                        }
                        if socket.send(Message::Text(frame.into())).await.is_err() {
                            return;
                        }
                        // 閉じるまで開けたままにし、close には close で返す
                        // （`shutdown()` が待たされないように）。
                        while let Some(Ok(message)) = socket.next().await {
                            if let Message::Close(reply) = message {
                                let _ = socket.send(Message::Close(reply)).await;
                                break;
                            }
                        }
                    });
                    continue;
                }
                let body = if head.contains("/api/v1/values") {
                    hub.values.lock().unwrap().clone()
                } else {
                    hub.catalog.lock().unwrap().clone()
                };
                let mut stream = stream;
                read_http_request(&mut stream).await;
                write_response(&mut stream, body).await;
            }
        }

        fn entry(tag: &str, v: f64, quality: ValueQuality, source: ValueSource) -> ValueEntry {
            ValueEntry {
                tag: tag.to_owned(),
                v: Some(v),
                q: quality,
                t: 1_000,
                value_source: source,
            }
        }

        fn values_of(catalog: &CatalogSnapshot, values: Vec<ValueEntry>) -> ValuesSnapshot {
            ValuesSnapshot {
                revision: catalog.revision,
                t: 1_000,
                run_id: catalog.run_id,
                collection_mode: catalog.collection_mode.clone(),
                values,
            }
        }

        /// `banto-tagclient` の WS は tag/v/q/t しか運ばない（`value_source` は
        /// REST 側にしか無い）。`t` を REST より新しくして、こちらが勝つこと
        /// を見られるようにする。
        fn data_frame(tag: &str, v: f64, quality: &str) -> String {
            format!(
                concat!(
                    r#"{{"op":"data","id":{},"t":2000,"#,
                    r#""values":[{{"tag":"{}","v":{},"q":"{}","t":2000}}]}}"#
                ),
                SUBSCRIPTION_ID, tag, v, quality
            )
        }

        /// 現世代が `Live` になるまで待つ（固定 sleep を使わない）。世代を
        /// 張り直すと `watch` も別物になるので、そのたびに取り直す。
        async fn wait_live(hub: &HubService) {
            let mut states = {
                let slot = hub.inner.subscription.lock().await;
                slot.generation.as_ref().expect("世代が立つ").states.clone()
            };
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    if states.borrow().connection_state() == TagClientConnectionState::Live {
                        return;
                    }
                    states.changed().await.unwrap();
                }
            })
            .await
            .expect("Live になる");
        }

        async fn sorted_values(hub: &HubService) -> Vec<HubValueView> {
            let mut values = hub.subscription().await.values;
            values.sort_by(|left, right| left.tag.cmp(&right.tag));
            values
        }

        /// 購読が成立して値が画面の形まで届くこと。**未知の品質・出所ラベルを
        /// 丸めない**ことと、WS で受けた新しい値が REST のスナップショットを
        /// 上書きすることもここで固める。
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn a_live_subscription_carries_values_into_the_view() {
            let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
            let endpoint = format!("http://{}", listener.local_addr().unwrap());
            let catalog = catalog(&["alpha", "beta"]);

            let mock = Arc::new(MockHub::default());
            mock.set(
                &catalog,
                &values_of(
                    &catalog,
                    vec![
                        entry("alpha", 1.0, ValueQuality::Good, ValueSource::Real),
                        // `value_source` は REST 側にしか無いので、未知の出所は
                        // こちらに混ぜる。
                        entry(
                            "beta",
                            2.0,
                            ValueQuality::Good,
                            ValueSource::Unknown("future_source".to_owned()),
                        ),
                    ],
                ),
                data_frame("alpha", 42.5, "future_quality"),
            );
            let server = tokio::spawn(serve_hub(listener, Arc::clone(&mock)));

            let (_settings, hub) = service_with_keyring(&endpoint, &["alpha", "beta"]).await;
            hub.reconcile_with(
                &HubStatus::Connected { tag_count: 2 },
                Some(&catalog),
                Trigger::Observe,
            )
            .await;
            wait_live(&hub).await;

            let view = hub.subscription().await;
            assert_eq!(view.state, "live");
            assert_eq!(view.subscribed_count, 2);
            assert_eq!(view.reason, None);
            assert_eq!(view.last_error, None);
            assert!(view.unresolved.is_empty());
            assert!(view.unsupported.is_empty());

            let received = sorted_values(&hub).await;
            assert_eq!(received.len(), 2);

            // WS で受けた新しい値が REST のスナップショットを上書きする。
            assert_eq!(received[0].tag, "alpha");
            assert_eq!(received[0].v, Some(42.5));
            assert_eq!(received[0].t, 2_000);
            assert_eq!(
                received[0].q, "future_quality",
                "知らない品質を good に丸めない"
            );
            assert_eq!(received[0].value_source, "real");

            // 触られていない側は REST の値のまま。
            assert_eq!(received[1].tag, "beta");
            assert_eq!(received[1].v, Some(2.0));
            assert_eq!(received[1].q, "good");
            assert_eq!(
                received[1].value_source, "future_source",
                "知らない出所を real に丸めない"
            );

            // 最終受信時刻はスナップショットの `t`（REST と WS の新しい方）。
            assert_eq!(view.last_value_at, Some(2_000));

            hub.inner.subscription.lock().await.stop(None).await;
            server.abort();
        }

        /// **選択中のタグが Hub から消えても、残りのタグで購読し直せること**
        /// （2026-09-17 実機で壊れていた経路の出口側）。
        ///
        /// 見張りが「古い要求セットのまま `Reconnecting`」を起こす判断は
        /// `a_reconnecting_generation_is_replanned_only_for_a_stale_request_set`
        /// が固める。こちらは起こしたあとの**再計画 → 残りのタグで live に
        /// 戻る → 消えた名前が `unresolved` に出る**ところを、実際に値が流れる
        /// 経路で固める。
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn a_deleted_tag_is_replanned_and_the_rest_keeps_flowing() {
            let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
            let endpoint = format!("http://{}", listener.local_addr().unwrap());
            let both = catalog(&["alpha", "beta"]);

            let mock = Arc::new(MockHub::default());
            mock.set(
                &both,
                &values_of(
                    &both,
                    vec![
                        entry("alpha", 1.0, ValueQuality::Good, ValueSource::Real),
                        entry("beta", 2.0, ValueQuality::Good, ValueSource::Real),
                    ],
                ),
                data_frame("alpha", 10.0, "good"),
            );
            let server = tokio::spawn(serve_hub(listener, Arc::clone(&mock)));

            let (_settings, hub) = service_with_keyring(&endpoint, &["alpha", "beta"]).await;
            hub.reconcile_with(
                &HubStatus::Connected { tag_count: 2 },
                Some(&both),
                Trigger::Observe,
            )
            .await;
            wait_live(&hub).await;
            assert_eq!(hub.subscription().await.subscribed_count, 2);

            // Hub 側で `beta` を削除する。
            let only_alpha = CatalogSnapshot {
                tags: both.tags.iter().take(1).cloned().collect(),
                ..both.clone()
            };
            mock.set(
                &only_alpha,
                &values_of(
                    &only_alpha,
                    vec![entry("alpha", 11.0, ValueQuality::Good, ValueSource::Real)],
                ),
                data_frame("alpha", 11.0, "good"),
            );

            // 見張りが起こしたあとにやること = catalog を読み直して突き合わせ。
            hub.refresh_catalog().await.unwrap();
            wait_live(&hub).await;

            let view = hub.subscription().await;
            assert_eq!(view.state, "live", "残りのタグで live に戻る");
            assert_eq!(view.subscribed_count, 1);
            assert_eq!(
                view.unresolved,
                owned(&["beta"]),
                "消えた名前は黙って落とさず一覧に出す"
            );
            let received = sorted_values(&hub).await;
            assert_eq!(received.len(), 1);
            assert_eq!(received[0].tag, "alpha");
            assert_eq!(received[0].v, Some(11.0), "残りのタグの値は流れ続ける");

            hub.inner.subscription.lock().await.stop(None).await;
            server.abort();
        }
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

    /// **写しが空 = 未設定、とは限らない。** 起動時の `hydrate()` が失敗
    /// （設定 DB の一時的な読み取り失敗など）しても写しは空のままなので、
    /// 写しだけを見て諦めると毎周期ここで止まり、**保存済みの購読が二度と
    /// 再開されない**。レコードの有無を判断する前に hydrate する。
    #[tokio::test]
    async fn the_supervisor_hydrates_before_concluding_there_is_no_record() {
        let (_settings, hub) = service_with_keyring(&closed_endpoint(), &["a"]).await;
        // 設定 DB にはレコードがあるのに写しは空 = 起動時 hydrate が失敗した形。
        hub.inner.mirror.reset(None);
        assert_eq!(
            hub.subscription().await.reason.as_deref(),
            Some(REASON_NOT_STARTED),
            "まだ一度も突き合わせていない"
        );

        hub.supervise_once().await;

        assert!(
            hub.inner.mirror.current().is_some(),
            "写しを見て諦めず、hydrate してから判断する"
        );
        assert_eq!(
            hub.subscription().await.reason.as_deref(),
            Some(REASON_NOT_CONNECTED),
            "そのまま再開まで進む（接続先は到達不能なので未接続の理由になる）"
        );
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

    // --- 終了時の後始末（R1-C の前提） --------------------------------------

    /// 世代を持った状態で [`HubService::shutdown`] すると、**世代が落ち**、
    /// 停止理由が「終了中」になる。
    #[tokio::test]
    async fn shutdown_drops_the_subscription_generation() {
        let (_settings, hub) = service_with_keyring(&closed_endpoint(), &["a"]).await;
        hub.reconcile_with(
            &HubStatus::Connected { tag_count: 1 },
            Some(&catalog(&["a"])),
            Trigger::Observe,
        )
        .await;
        assert_eq!(
            generation_sequence(&hub).await,
            Some(1),
            "前提: 世代が 1 本立っている"
        );

        hub.shutdown().await;

        assert_eq!(generation_sequence(&hub).await, None, "世代が落ちる");
        let view = hub.subscription().await;
        assert_eq!(view.state, "stopped");
        assert_eq!(
            view.reason.as_deref(),
            Some(REASON_SHUTTING_DOWN),
            "終了中であることが理由に出る（環境が直れば戻る停止と混ぜない）"
        );
    }

    /// **操作ロックが他に握られていても、待たずに完了する**（#383 R1-C の
    /// 前提でいちばん大事な性質）。`connect` は 60 秒の上限を持ったまま操作
    /// ロックを握るので、ここで待つとウィンドウを閉じてから最大 1 分
    /// プロセスが残る。
    ///
    /// 時計は止めてある（`pause`）ので**実時間は待たない** - `shutdown()` が
    /// 誤って操作ロックを待つように退行すると、ランタイムが暇になって時計が
    /// 飛び、下の `timeout` が `Err` になって落ちる。DB を触るのは
    /// `service_with_keyring` までで、そこは pause の前に済ませてある
    /// （`drive_until_the_hub_is_reached` の doc にある sqlx プールの罠を
    /// 踏まないため）。
    #[tokio::test]
    async fn shutdown_does_not_wait_for_the_operation_lock() {
        let (_settings, hub) = service_with_keyring(&closed_endpoint(), &["a"]).await;
        hub.reconcile_with(
            &HubStatus::Connected { tag_count: 1 },
            Some(&catalog(&["a"])),
            Trigger::Observe,
        )
        .await;
        // 「いま `connect` が Hub の応答を待っている」状態を作る。
        let held = hub.inner.operation.lock().await;

        tokio::time::pause();
        tokio::time::timeout(HUB_MUTATING_TIMEOUT, hub.shutdown())
            .await
            .expect("操作ロックの解放を待たずに完了する");

        assert_eq!(
            generation_sequence(&hub).await,
            None,
            "操作ロックを取れなくても、購読ロックだけで世代は止める"
        );
        drop(held);
    }

    /// [`HubService::shutdown`] の後は `resume()` が**見張りを起こさない**
    /// （終了処理が閉じたものを、終了中に立て直さない）。
    #[tokio::test]
    async fn resume_after_shutdown_does_not_start_the_supervisor() {
        let (_settings, hub) = service_with_keyring(&closed_endpoint(), &["a"]).await;
        hub.shutdown().await;

        hub.resume().await;

        assert_eq!(
            hub.inner.supervisor_spawns.load(Ordering::SeqCst),
            0,
            "見張りは spawn されない"
        );
        assert_eq!(
            generation_sequence(&hub).await,
            None,
            "その場の張り直しも起きない"
        );
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
