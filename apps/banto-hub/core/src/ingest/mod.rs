//! 取り込み口 `push`（R2a、docs/banto-hub-ingest-design.md）: 外の送り手（DB
//! ブリッジ・スクリプト）が Hub へ値を**送り込む**接続の、Hub 側の置き場
//! （§12 Q5 = (a)「core の `ingest/`」）。
//!
//! `push` は R2a では `DriverKind::Session` にせず、Hub 側の
//! `banto_collect::ExternalSource` として作る（§2、Q1）。このモジュールに揃う
//! もの（予定を含む）:
//!
//! - [`driver`]（R2a-2a）: 静的な部分 [`driver::PushDriver`]（`DriverBase`: 識別・
//!   設定の検証・キーの検証）。**レジストリには登録しない**（R2b-1 で host が
//!   [`driver::push_driver`] を登録する）。R2a の正は今までどおり
//!   `banto_tags::ALLOWED_PROTOCOLS` と SQL の `CHECK`。
//! - [`IngestHub`]（R2a-1c、このファイル）: 受信側の保持値・Live の判定・commit・
//!   epoch・ブリッジの送る集合。Hub の収集（`crate::hub::CollectorManager`）は
//!   [`build_hub_config`]（`PushSource::Available`）で `push` 接続を計画し、
//!   `Collector::start_with_sources` にこれを源として渡す。
//! - 送信口と、その共通部品（R2a-2c）。WS（R2a-2d）も同じ部品を使う:
//!   - [`auth`] モジュール: 認証の層 [`auth::require_ingest_auth`]（API キーだけ・`ingest:#{id}`・
//!     接続の有無を権限の確認より前に明かさない順序、§5・§6 の表）。
//!   - [`validate`] モジュール: メッセージの形（[`validate::parse_values_message`]）と件ごとの
//!     検証（[`validate_item`]、§6.3）。
//!   - [`limits`] モジュール: 件数の上限と接続ごとの頻度のトークンバケット
//!     （[`limits::ConnectionLimiter`]、§7）。状態は接続の slot に置き、REST と WS で
//!     共有する（[`IngestHub::admit`]）。
//!   - [`audit`] モジュール: 監査の行（§8）。値は載せない。未知のキーの抑制
//!     （[`audit::UnknownKeyThrottle`]）も slot に置く。
//!   - [`rest`] モジュール: `POST /api/v1/ingest/{id}/values`（§6.2）。
//!
//! 規則そのもの（キーの文法、`settings` の検証、上限の定数 `PUSH_LIMITS`、ブリッジの
//! 量の見積もり `bridge_load`）は `banto_tags::push` にあり、ここは包むだけ
//! （タグの検証・接続の検証と同じものを使うため）。
//!
//! ## 値の流れ（§4.2〜§4.4）
//!
//! 送り手の 1 メッセージは [`IngestHub::apply`] で接続ごとの保持値（キー →
//! `ExternalReading`）に入る。収集のタスク（`banto_collect` の
//! `run_external_connection`）はグループの周期ごとに
//! [`ExternalSource::snapshot`]（`"conn:{id}"`）で保持値を写し、
//! `classify_external` の表で表示・記録する（sample and hold。変わらない値も
//! 毎周期 Good で記録、`stale` は表示だけで記録は NULL）。リンクの Live / Down は
//! [`link_state`]（送り手の最後の通信から `stale_timeout_ms` 以内なら Live）。
//!
//! ## ロックの形（§4.2「1 メッセージ 1 回の書き込みロック」「snapshot はロックを長く持たない」）
//!
//! - 外側の `std::sync::RwLock<Inner>`（接続の表と「収集中か」）。
//!   [`IngestHub::commit`] と収集の遷移（`on_collection_*`）だけが書き込みで取る。
//!   `apply` / `ping` / `snapshot` は読み取りで取る。
//! - 接続ごとの `RwLock<Held>`（epoch・保持値・最後の通信の時刻）。`apply` は
//!   **1 メッセージにつき 1 回**書き込みで取り、全件をその中で判定・反映する
//!   （スナップショットがメッセージの途中を見ない）。`snapshot` は読み取りで取り、
//!   保持値の `HashMap` を複製するだけ。
//! - 順序は常に「外側 → 接続」。どちらも `.await` をまたいで持たない（同期の
//!   区間だけ）。`apply` は接続のロックを持つ間ずっと外側の読み取りも持つので、
//!   `commit` が接続の設定（登録済みのキー・送る集合）を入れ替える瞬間に、古い
//!   設定で判定した値が新しい設定の下へ書き込まれることは無い。
//! - 接続の保持値（`SlotShared`）は `Arc` で、`commit` は設定（`SlotConfig`）だけを
//!   作り直して保持値を引き継ぐ（収集のタスクの作り直し、`apply_config` の
//!   replaced でも保持値は消えない。§4.2）。
//!
//! ## 収集の遷移と epoch（§4.5・§4.6、Q19 = (a)）
//!
//! [`transition_effect`] の表（純関数）を `crate::controller` の遷移から呼ぶ:
//!
//! | 時点 | 一般の送り手 | ブリッジ（`settings.bridge` あり） |
//! | --- | --- | --- |
//! | 停止（`stop_locked` の `manager.stop()` の後）| 保持値を捨てる・`state` を知らせる | 保持値を捨てる・epoch を進める・`state` を知らせる |
//! | 開始（`start_locked` の `apply_run` より前）| 何もしない | 保持値を捨てる・epoch を進める（知らせない） |
//! | 開始の確定（`apply_run` の成功の枝）| `state { collecting: true }` | `state { collecting: true, epoch }` |
//!
//! 稼働中の構成の再適用（`commit_catalog_and_apply_live`）は遷移ではないので epoch を
//! 進めない。ブリッジの設定の変更は [`commit_effect`] の表で epoch を進める。
//!
//! 送り手への `state` の通知は接続ごとの `tokio::sync::watch`
//! （[`IngestHub::subscribe_state`]）。R2a-2d の WebSocket がこれを読んで
//! `{"op":"state",..}` を送る。R2a-1c ではテストがこれで「`collecting: true` が
//! 送られたか」を確かめる。

pub mod audit;
pub mod auth;
pub mod driver;
pub mod limits;
pub mod rest;
pub mod validate;

pub use validate::validate_item;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

use banto_collect::{
    build_config_from_with, CollectError, CollectorConfig, ExternalLink, ExternalReading,
    ExternalSnapshot, ExternalSource, PushSource, RegistrySnapshot,
};
use banto_tags::{
    validate_push_key, validate_push_settings, DEFAULT_STALE_TIMEOUT_MS, PLC_TAG_KIND,
    PUSH_PROTOCOL,
};
use banto_tstore::Clock;
use serde::{Deserialize, Deserializer};
use serde_json::Value;
use tokio::sync::watch;
use utoipa::ToSchema;

/// Hub の収集の計画（厳格版）: `push` 接続を外部の接続として計画する
/// （`PushSource::Available`、§4.1・§4.2）。Hub の計画を作る全部の点
/// （`CollectorManager::rebuild` / `commit_catalog` / `apply_run` と REST の
/// preflight）がこれを通る - preflight と実際の計画がずれて「保存できたのに収集が
/// 断る」ことが無いように。
pub fn build_hub_config(snapshot: &RegistrySnapshot) -> Result<CollectorConfig, CollectError> {
    build_config_from_with(snapshot, PushSource::Available)
}

/// 1 件の値。
// 実装の注（§6.1・§6.2 の `items[]` の 1 要素）: 形だけを持ち、中身の検証は
// `IngestHub::apply`（`validate_item`）が件ごとに行う。`v` が無いのと `null` は同じ。
// `q` が無ければ `good`。`q` / `t` の明示的な `null` は省略と区別し、それぞれ
// `invalid_quality` / `invalid_time`。doc comment は OpenAPI の説明になるので wire の
// 言葉だけにしている（R2a-2c）。
#[derive(Debug, Clone, PartialEq, Deserialize, ToSchema)]
pub struct IngestItem {
    /// タグのキー（送り手が決めた文字列。大文字小文字を区別）。
    pub key: String,
    /// 値: 数値・bool（1 / 0）・null。`q` が `good`（省略を含む）なら null は不可。
    /// `bit` のタグは 0・1・bool だけ。
    #[serde(default)]
    pub v: Value,
    /// 品質: `good`（省略時）/ `stale` / `bad`。
    #[serde(default, deserialize_with = "present")]
    #[schema(value_type = Option<String>)]
    pub q: Option<Value>,
    /// 発生時刻（epoch ms、任意）。鮮度・記録の時刻には使わない。
    #[serde(default, deserialize_with = "present")]
    #[schema(value_type = Option<i64>)]
    pub t: Option<Value>,
}

/// 省略（`default` で `None`）と明示的な `null`（`Some(Value::Null)`）を区別する。
fn present<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(d).map(Some)
}

impl IngestItem {
    /// 品質を省略した（= `good`）1 件。
    pub fn new(key: impl Into<String>, v: Value) -> Self {
        Self {
            key: key.into(),
            v,
            q: None,
            t: None,
        }
    }

    /// 品質の綴り（`"good"` / `"stale"` / `"bad"`、それ以外は `invalid_quality`）を付ける。
    pub fn with_quality(mut self, q: &str) -> Self {
        self.q = Some(Value::String(q.to_string()));
        self
    }
}

/// 件ごとに断る理由（§6.3 の `code`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectCode {
    /// その接続に無いキー。
    UnknownKey,
    /// 登録はあるがタグ・グループが無効。
    InactiveKey,
    /// ブリッジの接続で、`settings.bridge` の対応に無いキー（§4.6）。
    NotInBridgeSet,
    /// 同じメッセージに同じキーが 2 回（後の方を断る）。
    DuplicateKey,
    /// `v` が数値・bool・null 以外 / `bit` に 0・1・bool 以外 / `good` で `null`。
    InvalidValue,
    /// `q` が `good` / `stale` / `bad` 以外。
    InvalidQuality,
    /// `t` が整数でない。
    InvalidTime,
}

impl RejectCode {
    /// wire の綴り（§6.3）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UnknownKey => "unknown_key",
            Self::InactiveKey => "inactive_key",
            Self::NotInBridgeSet => "not_in_bridge_set",
            Self::DuplicateKey => "duplicate_key",
            Self::InvalidValue => "invalid_value",
            Self::InvalidQuality => "invalid_quality",
            Self::InvalidTime => "invalid_time",
        }
    }
}

/// 断った 1 件（`index` はメッセージの中の位置）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemRejection {
    pub index: usize,
    pub key: String,
    pub code: RejectCode,
}

/// 受け付けたメッセージの結果（§6.1 の `ack`・§6.2 の 200 の本文の中身）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyAck {
    pub accepted: usize,
    pub rejected: Vec<ItemRejection>,
    /// 収集が Running か（§4.5、Q11: 停止中も受ける）。
    pub collecting: bool,
    /// この接続の今の epoch（§4.6）。
    pub epoch: u64,
    pub stale_timeout_ms: u64,
}

/// メッセージ全体を断った理由（何も反映せず、`last_seen_ms` も進めない）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyRefusal {
    /// その id の `push` 接続が無い（削除済み・`push` でない）。§6 の 404 `unknown_connection`。
    UnknownConnection,
    /// 接続が無効。§6 の 409 `connection_disabled`。
    ConnectionDisabled,
    /// epoch が今のものと違う（ブリッジの接続では無いのも）。§4.6、409 `stale_epoch`。
    StaleEpoch { epoch: u64 },
}

impl ApplyRefusal {
    /// wire の綴り（§6）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UnknownConnection => "unknown_connection",
            Self::ConnectionDisabled => "connection_disabled",
            Self::StaleEpoch { .. } => "stale_epoch",
        }
    }
}

/// [`IngestHub::apply`] の結果。
pub type ApplyOutcome = Result<ApplyAck, ApplyRefusal>;

/// 送り手へ知らせる状態（§6.1 の `state`）。接続ごとの `watch` に載る。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IngestState {
    pub collecting: bool,
    pub epoch: u64,
    /// ブリッジの設定が変わったことによる通知か（§4.6。ブリッジは設定を取り直す）。
    pub config_changed: bool,
}

/// 1 つの `push` 接続の今の様子（送信口の `welcome` と状態表示、テスト用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestConnectionInfo {
    pub name: String,
    pub enabled: bool,
    pub bridge: bool,
    pub epoch: u64,
    pub stale_timeout_ms: u64,
    pub collecting: bool,
    /// 保持している値の数（登録済み・有効なキーだけ。§7「保持値」）。
    pub held_values: usize,
}

/// 接続の種類（§4.5 の表の列）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SenderKind {
    /// `settings.bridge` なし（スクリプト等）。
    General,
    /// `settings.bridge` あり（DB ブリッジ）。
    Bridge,
}

/// 収集の遷移（§4.5 の表の行）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectionTransition {
    /// `stop_locked` の `manager.stop()` の後。
    Stopped,
    /// `start_locked` の `apply_run` より前。
    Starting,
    /// `apply_run` が成功して Running が確定した後。
    Started,
}

/// 遷移・設定の変更が 1 接続に及ぼすこと。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeldEffect {
    /// 保持値を全部捨てる。
    pub discard_held: bool,
    /// epoch を 1 進める。
    pub advance_epoch: bool,
    /// `state` を送り手へ知らせる。
    pub notify: bool,
}

/// §4.5 の表（純関数）。
pub fn transition_effect(kind: SenderKind, transition: CollectionTransition) -> HeldEffect {
    use CollectionTransition as T;
    use SenderKind as K;
    match (transition, kind) {
        // 停止: 全接続の保持値を捨てる（停止前の Good を残さない）。ブリッジは
        // 停止の前に問い合わせた値が後から届いても受けないよう epoch を進める。
        (T::Stopped, K::General) => HeldEffect {
            discard_held: true,
            advance_epoch: false,
            notify: true,
        },
        (T::Stopped, K::Bridge) => HeldEffect {
            discard_held: true,
            advance_epoch: true,
            notify: true,
        },
        // 開始（apply_run の前）: 一般の送り手は停止中に届いた値で記録を始める
        // （Q11）。ブリッジは停止中の値も捨て、開始の後の epoch の値まで Bad。
        // collecting: true はまだ知らせない（apply_run が失敗しうる）。
        (T::Starting, K::General) => HeldEffect {
            discard_held: false,
            advance_epoch: false,
            notify: false,
        },
        (T::Starting, K::Bridge) => HeldEffect {
            discard_held: true,
            advance_epoch: true,
            notify: false,
        },
        // 開始の確定: 知らせるだけ（epoch は Starting で進めた値）。
        (T::Started, _) => HeldEffect {
            discard_held: false,
            advance_epoch: false,
            notify: true,
        },
    }
}

/// `commit` で 1 接続の設定が変わったときの表（純関数、§4.6）。
///
/// - ブリッジの設定（`settings.bridge` の JSON）が変わった: 保持値を捨て、epoch を
///   進め、`state { config_changed: true }` を知らせる。
/// - 有効 / 無効が変わった: 保持値を捨てる（無効の間の値・無効にする前の値を、
///   有効に戻した後の Good にしない）。ブリッジの接続なら epoch も進めて知らせる
///   （無効にする前に問い合わせた値が後から届いても受けない）。
/// - それ以外（改名・`stale_timeout_ms`・タグの増減）: 何もしない（登録から消えた
///   キーの値は呼び出し側が個別に捨てる）。
pub fn commit_effect(
    old_bridge: Option<&Value>,
    new_bridge: Option<&Value>,
    enabled_changed: bool,
) -> HeldEffect {
    let bridge_changed = old_bridge != new_bridge;
    let is_bridge = old_bridge.is_some() || new_bridge.is_some();
    let advance_epoch = bridge_changed || (enabled_changed && is_bridge);
    HeldEffect {
        discard_held: bridge_changed || enabled_changed,
        advance_epoch,
        notify: advance_epoch,
    }
}

/// §4.3 の Live の判定（純関数）: 送り手の最後の通信から `stale_timeout_ms` 以内なら
/// Live。時計が戻って `now_ms < last_seen_ms` なら Live のまま（差が負）。Down の
/// detail は資格情報・アドレスを含まない（`ExternalLink::Down` の約束）。
pub fn link_state(last_seen_ms: Option<i64>, stale_timeout_ms: u64, now_ms: i64) -> ExternalLink {
    let Some(last_seen_ms) = last_seen_ms else {
        return ExternalLink::Down {
            detail: Some("送り手が一度も接続していません".to_string()),
        };
    };
    let silent_ms = now_ms.saturating_sub(last_seen_ms);
    let timeout_ms = i64::try_from(stale_timeout_ms).unwrap_or(i64::MAX);
    if silent_ms <= timeout_ms {
        ExternalLink::Live
    } else {
        ExternalLink::Down {
            detail: Some(format!(
                "送り手からの通信が {} 秒ありません",
                silent_ms / 1_000
            )),
        }
    }
}

/// 登録済みのキー 1 つ。
#[derive(Debug, Clone, PartialEq, Eq)]
struct KeyInfo {
    /// タグの `data_type`（`bit` の値の検証）。
    data_type: String,
    /// 接続・グループ・タグがすべて有効か（無効なら `inactive_key`）。
    active: bool,
}

/// ブリッジの送る集合（§4.6）。
#[derive(Debug, Clone, PartialEq)]
struct BridgeSendSet {
    /// 設定の指紋: `settings.bridge` の JSON そのもの（変わったら epoch を進める）。
    fingerprint: Value,
    /// `queries[].columns[].key`。`settings` が検証を通らなければ空（全部を断る、
    /// fail closed）。
    keys: HashSet<String>,
}

/// commit で作る 1 接続の設定（不変。commit のたびに作り直す）。
#[derive(Debug, Clone, PartialEq)]
struct SlotConfig {
    name: String,
    enabled: bool,
    keys: HashMap<String, KeyInfo>,
    stale_timeout_ms: u64,
    bridge: Option<BridgeSendSet>,
}

impl SlotConfig {
    fn kind(&self) -> SenderKind {
        if self.bridge.is_some() {
            SenderKind::Bridge
        } else {
            SenderKind::General
        }
    }
}

/// 接続の保持値（§4.2 の `Held`）。
#[derive(Debug)]
struct Held {
    /// 単調増加。Hub の起動（接続の slot の作成）で 1。
    epoch: u64,
    values: HashMap<String, ExternalReading>,
    /// 受け付けた最後の通信（値・ping）の時刻。
    last_seen_ms: Option<i64>,
}

/// commit をまたいで引き継ぐ部分。
///
/// `limiter` と `unknown_keys`（R2a-2c）は送信口の上限と監査の抑制の状態。接続の slot に
/// 置くので、REST と WS が同じバケットを使い（§7「WS と REST の合計」）、改名をまたいで
/// 続き、削除された接続の分は slot と一緒に消える（メモリは `push` 接続の数で頭打ち）。
/// どちらも末端のロックで、`held` と同時には持たない。
#[derive(Debug)]
struct SlotShared {
    held: RwLock<Held>,
    state: watch::Sender<IngestState>,
    limiter: Mutex<limits::ConnectionLimiter>,
    unknown_keys: Mutex<audit::UnknownKeyThrottle>,
}

impl SlotShared {
    fn new(collecting: bool) -> Self {
        let (state, _) = watch::channel(IngestState {
            collecting,
            epoch: 1,
            config_changed: false,
        });
        Self {
            held: RwLock::new(Held {
                epoch: 1,
                values: HashMap::new(),
                last_seen_ms: None,
            }),
            state,
            limiter: Mutex::new(limits::ConnectionLimiter::new(limits::PUSH_LIMITS)),
            unknown_keys: Mutex::new(audit::UnknownKeyThrottle::default()),
        }
    }

    /// `effect` を保持値に当て、知らせるなら `state` を送る。
    fn apply_effect(&self, effect: HeldEffect, collecting: bool, config_changed: bool) {
        let epoch = {
            let mut held = write(&self.held);
            if effect.discard_held {
                held.values.clear();
            }
            if effect.advance_epoch {
                held.epoch += 1;
            }
            held.epoch
        };
        if effect.notify {
            self.state.send_replace(IngestState {
                collecting,
                epoch,
                config_changed,
            });
        }
    }
}

#[derive(Debug)]
struct IngestSlot {
    config: SlotConfig,
    shared: Arc<SlotShared>,
}

#[derive(Debug, Default)]
struct Inner {
    slots: HashMap<i64, Arc<IngestSlot>>,
    /// 収集が Running か（`on_collection_started` で true、`on_collection_stopped` で false）。
    collecting: bool,
}

/// 取り込みの受信側（§4.2）。`crate::hub::CollectorManager` が 1 つ作り、収集の
/// 外部ソースとして `Collector` に渡す。モジュールの doc にロックの形と遷移の規則。
pub struct IngestHub {
    inner: RwLock<Inner>,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for IngestHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IngestHub").finish_non_exhaustive()
    }
}

fn read<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(PoisonError::into_inner)
}

fn write<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write().unwrap_or_else(PoisonError::into_inner)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `"conn:{id}"`（`banto_collect` の `ConnectionPlan.key`）の id。
fn parse_connection_key(connection_key: &str) -> Option<i64> {
    connection_key.strip_prefix("conn:")?.parse().ok()
}

/// レジストリから全 `push` 接続の設定を作る（純関数）。
fn slot_configs(snapshot: &RegistrySnapshot) -> HashMap<i64, SlotConfig> {
    let mut configs: HashMap<i64, SlotConfig> = snapshot
        .connections
        .iter()
        .filter(|conn| conn.protocol == PUSH_PROTOCOL)
        .map(|conn| {
            let (stale_timeout_ms, bridge) = match &conn.settings {
                None => (DEFAULT_STALE_TIMEOUT_MS, None),
                Some(raw) => match validate_push_settings(raw) {
                    Ok(settings) => (
                        settings.stale_timeout_ms,
                        settings.bridge.map(|bridge| BridgeSendSet {
                            fingerprint: raw.get("bridge").cloned().unwrap_or(Value::Null),
                            keys: bridge
                                .queries
                                .iter()
                                .flat_map(|query| query.columns.iter())
                                .map(|column| column.key.clone())
                                .collect(),
                        }),
                    ),
                    // 書き込みの入口が検証するので本来は起きない（手で書き換えた DB）。
                    // ブリッジの区画があれば送る集合を空にして全部を断る（fail closed）。
                    Err(_) => (
                        DEFAULT_STALE_TIMEOUT_MS,
                        raw.get("bridge").map(|fingerprint| BridgeSendSet {
                            fingerprint: fingerprint.clone(),
                            keys: HashSet::new(),
                        }),
                    ),
                },
            };
            (
                conn.id,
                SlotConfig {
                    name: conn.name.clone(),
                    enabled: conn.enabled,
                    keys: HashMap::new(),
                    stale_timeout_ms,
                    bridge,
                },
            )
        })
        .collect();
    let groups: HashMap<i64, (i64, bool)> = snapshot
        .groups
        .iter()
        .map(|group| (group.id, (group.plc_connection_id, group.enabled)))
        .collect();
    for tag in &snapshot.tags {
        if tag.tag_kind != PLC_TAG_KIND {
            continue;
        }
        let Some(&(connection_id, group_enabled)) = groups.get(&tag.collection_group_id) else {
            continue;
        };
        let Some(config) = configs.get_mut(&connection_id) else {
            continue;
        };
        let key = validate_push_key(&tag.address).unwrap_or_else(|_| tag.address.trim().into());
        config.keys.insert(
            key,
            KeyInfo {
                data_type: tag.data_type.clone(),
                active: config.enabled && group_enabled && tag.enabled,
            },
        );
    }
    configs
}

impl IngestHub {
    /// `clock` は収集と同じ時計（Live の判定の `now`）。
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            inner: RwLock::new(Inner::default()),
            clock,
        }
    }

    /// catalog の入れ替えと同じ点で呼ぶ（`CollectorManager::rebuild` の 2 つの枝と
    /// `commit_catalog`）。`push` 接続の追加・削除と、キーの集合・送る集合・
    /// `stale_timeout_ms` の更新。登録から消えたキー・無効になったキーの保持値は
    /// 捨てる（キーの改名で古い値が残らない）。ブリッジの設定・有効が変わった接続は
    /// [`commit_effect`] の表に従う。データ型が変わったキーの保持値も捨てる（型が
    /// 変われば `bit` の検証などが変わり、古い値は新しい型の値ではない）。
    ///
    /// 設計（§4.2）の `commit(map, snapshot)` の `map` は取らない: キー・有効・型は
    /// すべて `snapshot` から決まり、catalog（外部名で引く表）は外部名の重なり
    /// （#635）で一部のタグを引けないことがあるため、正にしない。
    pub fn commit(&self, snapshot: &RegistrySnapshot) {
        let configs = slot_configs(snapshot);
        let mut inner = write(&self.inner);
        let collecting = inner.collecting;
        let mut next = HashMap::with_capacity(configs.len());
        for (id, config) in configs {
            let shared = match inner.slots.get(&id) {
                Some(old) => {
                    let effect = commit_effect(
                        old.config.bridge.as_ref().map(|b| &b.fingerprint),
                        config.bridge.as_ref().map(|b| &b.fingerprint),
                        old.config.enabled != config.enabled,
                    );
                    let bridge_changed = old.config.bridge.as_ref().map(|b| &b.fingerprint)
                        != config.bridge.as_ref().map(|b| &b.fingerprint);
                    {
                        let mut held = write(&old.shared.held);
                        held.values.retain(|key, _| {
                            config.keys.get(key).is_some_and(|info| {
                                info.active
                                    && old
                                        .config
                                        .keys
                                        .get(key)
                                        .is_some_and(|o| o.data_type == info.data_type)
                            })
                        });
                    }
                    old.shared.apply_effect(effect, collecting, bridge_changed);
                    old.shared.clone()
                }
                None => Arc::new(SlotShared::new(collecting)),
            };
            next.insert(id, Arc::new(IngestSlot { config, shared }));
        }
        inner.slots = next;
    }

    /// 収集の停止（`controller.rs` の `stop_locked`、`manager.stop()` の後 = 収集の
    /// タスクが join された後）: 全接続の保持値を捨て、ブリッジの接続は epoch を進め、
    /// `state { collecting: false }` を知らせる（§4.5）。
    pub fn on_collection_stopped(&self) {
        self.on_transition(CollectionTransition::Stopped);
    }

    /// 収集の開始（`start_locked` の `apply_run` より前）: ブリッジの接続の保持値を
    /// 捨て epoch を進める（§4.5）。ここでは何も知らせない（`collecting` は false の
    /// まま）。
    pub fn on_collection_starting(&self) {
        self.on_transition(CollectionTransition::Starting);
    }

    /// `apply_run` が成功して Running が確定した後（`start_locked` の started の枝、
    /// `db_source.start()` と同じ位置）: 全接続へ `state { collecting: true, epoch }` を
    /// 知らせる。`apply_run` が失敗して Faulted になったときは呼ばない。
    pub fn on_collection_started(&self) {
        self.on_transition(CollectionTransition::Started);
    }

    fn on_transition(&self, transition: CollectionTransition) {
        let mut inner = write(&self.inner);
        match transition {
            CollectionTransition::Stopped => inner.collecting = false,
            CollectionTransition::Started => inner.collecting = true,
            CollectionTransition::Starting => {}
        }
        let collecting = inner.collecting;
        for slot in inner.slots.values() {
            let effect = transition_effect(slot.config.kind(), transition);
            slot.shared.apply_effect(effect, collecting, false);
        }
    }

    /// 1 メッセージ分を **1 回の書き込みロック**で反映する（スナップショットが途中を
    /// 見ない）。結果は §6.3 の件ごとの結果。ブリッジの接続では `message_epoch` が
    /// 無い・今の epoch と違えばメッセージ全体を断る。一般の送り手では `epoch` は
    /// 任意で、付ければ同じく照合する（§4.6）。受け付けたら `now_ms` を最後の通信に
    /// する（全件を断っても、メッセージは受け付けている）。
    ///
    /// 呼ぶのは送信口（R2a-2c の REST・R2a-2d の WebSocket）だけ。認証・上限
    /// （件数・頻度）はそちらが先に済ませる。
    pub fn apply(
        &self,
        conn_id: i64,
        message_epoch: Option<u64>,
        items: &[IngestItem],
        now_ms: i64,
    ) -> ApplyOutcome {
        let inner = read(&self.inner);
        let slot = inner
            .slots
            .get(&conn_id)
            .ok_or(ApplyRefusal::UnknownConnection)?;
        if !slot.config.enabled {
            return Err(ApplyRefusal::ConnectionDisabled);
        }
        let mut held = write(&slot.shared.held);
        let epoch_matches = match message_epoch {
            Some(epoch) => epoch == held.epoch,
            None => slot.config.bridge.is_none(),
        };
        if !epoch_matches {
            return Err(ApplyRefusal::StaleEpoch { epoch: held.epoch });
        }
        held.last_seen_ms = Some(now_ms);

        let mut seen: HashSet<&str> = HashSet::with_capacity(items.len());
        let mut accepted = 0;
        let mut rejected = Vec::new();
        for (index, item) in items.iter().enumerate() {
            match judge_item(&slot.config, &mut seen, item) {
                Ok(reading) => {
                    held.values.insert(item.key.clone(), reading);
                    accepted += 1;
                }
                Err(code) => rejected.push(ItemRejection {
                    index,
                    key: item.key.clone(),
                    code,
                }),
            }
        }
        Ok(ApplyAck {
            accepted,
            rejected,
            collecting: inner.collecting,
            epoch: held.epoch,
            stale_timeout_ms: slot.config.stale_timeout_ms,
        })
    }

    /// 値を伴わない通信（WS の `ping` / `status`）: 最後の通信の時刻だけを進める
    /// （§4.3）。epoch は見ない（`ping` は epoch を持たない）。
    pub fn ping(&self, conn_id: i64, now_ms: i64) -> Result<(), ApplyRefusal> {
        let inner = read(&self.inner);
        let slot = inner
            .slots
            .get(&conn_id)
            .ok_or(ApplyRefusal::UnknownConnection)?;
        if !slot.config.enabled {
            return Err(ApplyRefusal::ConnectionDisabled);
        }
        write(&slot.shared.held).last_seen_ms = Some(now_ms);
        Ok(())
    }

    /// 送信口の上限（R2a-2c、§7）: 1 メッセージ（`item_count` 件）を受けてよいか。
    /// 件数の上限と接続ごとの頻度のバケット（REST と WS の合計）を
    /// [`limits::ConnectionLimiter::admit`] で判定し、超過の始まり・終わり（監査の端、
    /// §8）も返す。`apply` より前に呼ぶ（断ったメッセージは何も反映せず、
    /// `last_seen_ms` も進めない）。接続が無い・無効なら `apply` と同じ拒否。
    pub fn admit(
        &self,
        conn_id: i64,
        item_count: usize,
        now_ms: i64,
    ) -> Result<limits::Admission, ApplyRefusal> {
        let inner = read(&self.inner);
        let slot = inner
            .slots
            .get(&conn_id)
            .ok_or(ApplyRefusal::UnknownConnection)?;
        if !slot.config.enabled {
            return Err(ApplyRefusal::ConnectionDisabled);
        }
        let admission = lock(&slot.shared.limiter).admit(item_count, now_ms);
        Ok(admission)
    }

    /// 未知のキー（`apply` が `unknown_key` で断ったもの）の監査の間引き（§8）。
    /// 記録すべき行だけを返す（(接続, キー) ごとに 1 時間に 1 回、1 時間に 100 キーを
    /// 超えた分は数だけ）。接続が無ければ空。
    pub fn throttle_unknown_keys<'a>(
        &self,
        conn_id: i64,
        keys: impl IntoIterator<Item = &'a str>,
        now_ms: i64,
    ) -> Vec<audit::UnknownKeyRecord> {
        let inner = read(&self.inner);
        let Some(slot) = inner.slots.get(&conn_id) else {
            return Vec::new();
        };
        let records = lock(&slot.shared.unknown_keys).observe(keys, now_ms);
        records
    }

    /// 接続 `conn_id` の `state` の購読（R2a-2d の WebSocket が `state` を送るため）。
    /// `push` 接続でなければ `None`。commit をまたいで同じチャンネル。
    pub fn subscribe_state(&self, conn_id: i64) -> Option<watch::Receiver<IngestState>> {
        read(&self.inner)
            .slots
            .get(&conn_id)
            .map(|slot| slot.shared.state.subscribe())
    }

    /// 収集が Running か（`on_collection_started` 〜 `on_collection_stopped`）。
    pub fn collecting(&self) -> bool {
        read(&self.inner).collecting
    }

    /// 接続 `conn_id` の今の様子。`push` 接続でなければ `None`。
    pub fn connection_info(&self, conn_id: i64) -> Option<IngestConnectionInfo> {
        let inner = read(&self.inner);
        let slot = inner.slots.get(&conn_id)?;
        let held = read(&slot.shared.held);
        Some(IngestConnectionInfo {
            name: slot.config.name.clone(),
            enabled: slot.config.enabled,
            bridge: slot.config.bridge.is_some(),
            epoch: held.epoch,
            stale_timeout_ms: slot.config.stale_timeout_ms,
            collecting: inner.collecting,
            held_values: held.values.len(),
        })
    }
}

/// 1 件の判定: キーの照合（登録・有効・送る集合・重複）の後に [`validate_item`]。
fn judge_item<'a>(
    config: &SlotConfig,
    seen: &mut HashSet<&'a str>,
    item: &'a IngestItem,
) -> Result<ExternalReading, RejectCode> {
    let info = config.keys.get(&item.key).ok_or(RejectCode::UnknownKey)?;
    if !info.active {
        return Err(RejectCode::InactiveKey);
    }
    if let Some(bridge) = &config.bridge {
        if !bridge.keys.contains(&item.key) {
            return Err(RejectCode::NotInBridgeSet);
        }
    }
    if !seen.insert(item.key.as_str()) {
        return Err(RejectCode::DuplicateKey);
    }
    validate_item(item, &info.data_type)
}

impl ExternalSource for IngestHub {
    /// `"conn:{id}"` の接続の保持値。読み取りロック 2 つ（外側・接続）と値の複製だけ
    /// （`ExternalSource` の契約: tick を止めない）。接続が無ければ `down(None)`。
    fn snapshot(&self, connection_key: &str) -> ExternalSnapshot {
        let Some(conn_id) = parse_connection_key(connection_key) else {
            return ExternalSnapshot::down(None);
        };
        let now_ms = self.clock.now_ms();
        let inner = read(&self.inner);
        let Some(slot) = inner.slots.get(&conn_id) else {
            return ExternalSnapshot::down(None);
        };
        let held = read(&slot.shared.held);
        match link_state(held.last_seen_ms, slot.config.stale_timeout_ms, now_ms) {
            ExternalLink::Live => ExternalSnapshot::live(held.values.clone()),
            ExternalLink::Down { detail } => ExternalSnapshot::down(detail),
        }
    }
}

#[cfg(test)]
use banto_collect::ExternalQuality;

#[cfg(test)]
mod tests;
