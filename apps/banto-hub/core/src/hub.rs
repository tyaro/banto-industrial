//! Hub の中核: [`CollectorManager`]（Collector のライフサイクル管理、設計
//! §3.2/§4.3）と [`TagMap`]（外部名 catalog のスナップショット、設計
//! §4/§4.1）。
//!
//! ## `CollectorManager`: T7 で「部分適用」に卒業（設計 §4.3、2026-08-05）
//!
//! T0〜T6 は接続単位の部分再構成（I7、設計 §4.3 表の (c)）を実装せず、
//! レジストリが変わるたびに `banto_collect::Collector` をまるごと作り直して
//! いた（ChronoGazer と同型）。**T7 でこれを卒業**し、既に稼働中の
//! `Collector` があるときは [`banto_collect::Collector::apply_config`]
//! （T7-1、差分適用・`ApplyReport` 返却）を呼ぶよう [`CollectorManager::rebuild`]
//! を書き換えた。段階実装の狙いどおり、外部契約（`revision`・`config_changed`・
//! `last_error`、設計 §4.1）はこの移行の前後でまったく変わっていない —
//! クライアントは何も気付かずに「変更時に他接続の値まで一瞬 Bad になっていた
//! (T0〜T6) → ならなくなった (T7)」という体験の向上だけを受け取る（設計 §4.3
//! 冒頭の見込みどおり）。`Collector` がまだ無い場合（起動直後の初回成功、
//! または空構成から復帰する場合）は従来どおり
//! [`banto_collect::Collector::start_with_client_factory`] で新規起動する
//! （差し替える対象が無いので部分適用の出番がない）。
//!
//! ### `apply_config` 呼び出し時の読み取り整合性（T7 追加の設計判断）
//!
//! `apply_config` は `&mut Collector` を要求する（内部の `tasks` マップを
//! 直接入れ替えるため）。一方 [`CollectorManager::connection_status`] は
//! 同じ `Collector` から `&self` で読む。この排他性の衝突を吸収するため、
//! T7 で `Collector` 本体の保護を `std::sync::Mutex`（`inner` の一部）から
//! **`tokio::sync::Mutex`**（`CollectorManager::collector` フィールド）へ
//! 切り替えた - `apply_config`/`start_with_client_factory` の `.await` を
//! またいでロックを保持する必要があるため（このモジュールのもう一つの
//! ロック `rebuild_lock` と同じ理由）。この結果 `connection_status` は
//! `async fn` になったが、**`current_values` は非同期化していない**:
//! `banto_collect::Collector::current_values` が返す
//! `CurrentValuesHandle` はそれ自体 `Arc` ベースの安定した参照であり、
//! `apply_config` はこのハンドルの「中身」を書き換える(`retain`)だけで
//! **ハンドルの実体そのものを差し替えることはない**（`Collector` の
//! `current` フィールドは `apply_config` の全ステップを通じて同一インス
//! タンス）。そこで `CollectorManager` は `current_values()` が返す値を
//! `inner`（`std::sync::Mutex`、短時間保持のまま）に**キャッシュ**し、
//! `Collector` を新規作成した瞬間（起動時 / 空構成からの復帰時）だけ
//! 更新する。`apply_config` 実行中でも `current_values()` は一切ブロック
//! されず、常に最新の（かつ正しい）ハンドルを返す — 「無関係な接続 A の値が
//! rebuild 中も途切れない」という T7 の対外的要件を、REST/WS/gRPC/MQTT の
//! 呼び出し側コードを一切変更せずに満たす（`connection_status` はブロック
//! される可能性があるが、そのブロックは「待てば正しい最新状態が返る」だけで
//! 「間違った/空の状態が返る」ことは決してない - 遅延であって不整合ではない）。
//!
//! ## SLMP broker セッションの削除同期（T7-2、2026-08-05）
//!
//! T2-2 は broker セッションの同期を `ensure_connection` のみ（追加専用）と
//! していた（`crate::broker_glue::HubSessions` のモジュール doc「Session
//! sync policy」参照）。T7-2 で `banto_broker::SessionDirectory::remove` が
//! 追加されたことを受け、[`CollectorManager::rebuild`] は毎回「追加 +
//! 削除」の完全同期を行う: レジストリの現在の有効 SLMP 接続集合を
//! `ensure_connection`（従来どおり）した後、**Collector 側のコミットが
//! 成功した後で**（= 削除対象接続の collect タスクが既に停止済みである
//! ことが保証された後で）、`HubSessions` が保持している接続 ID のうち
//! この集合に無いものを `HubSessions::remove` する。この順序
//! （collect タスク停止 → broker セッション削除）が逆転すると、まだ
//! `read_batch` を呼んでいる `BrokerReadClient` の下でセッションが消える
//! 危険がある - 詳細は `crate::broker_glue` のモジュール doc参照。
//!
//! ## SLMP 接続単位のシミュレーションモード（T9-2、2026-08-06/07）
//!
//! `crate::broker_glue::SlmpSimRegistry`（`sim_registry` フィールド）は
//! `sessions`（`HubSessions`）と対の、`CollectorManager` の外で構築・生存する
//! `Arc` - broker 経由 SLMP 接続の `simulation = true` を実際に有効化する
//! T9-2 の実装本体（詳しくは `SlmpSimRegistry` 自身の doc comment、および
//! `crate::broker_glue` のモジュール doc「T9-1/T9-2 note」節を参照）。
//! [`CollectorManager::sync_slmp_sessions`] は `ensure_connection` を呼ぶ前に
//! 接続ごとに `SlmpSimRegistry::resolve` を呼び、シミュレーション中なら実際の
//! ダイヤル先をシミュレータの loopback アドレスへ差し替え、宛先が変わって
//! いれば（`changed == true`）`HubSessions::remove` してから
//! `ensure_connection` して古いセッションの使い回しを防ぐ。[`Self::rebuild`]
//! はさらに、この rebuild で broker が担当した SLMP 接続キー集合を
//! `banto_collect::CollectorConfig::suppress_simulation_for` に渡し、
//! `Collector` 自身が同じ接続に対して二重にシミュレータを起動しないようにする
//! （`crates/banto-collect/src/config.rs`の`suppress_simulation_for`の doc
//! comment参照）。
//!
//! **ここまでだけでは実は不十分**（自前の E2E テスト
//! `apps/banto-hub/core/tests/t9_simulation.rs`で発覚）: `simulation`を
//! 常に`false`へ強制すると、broker 経由 SLMP 接続の`ConnectionPlan`は
//! トグル前後で構造的に同一と比較され、`Collector::apply_config`はそれを
//! 「unchanged」と分類してその接続の収集タスクに一切手を触れない -
//! ところがタスクが起動時に捕まえた`ClientFactory`（そのクロージャが閉じる
//! `ReadOnlyHandle`）はタスクの生存期間を通じて固定なので、rebuild の度に
//! 新しく組み立てる`hub_client_factory`はタスクへ届かない。結果、broker
//! セッションが`SlmpSimRegistry::resolve`の`changed`検出で実際には
//! 入れ替わっていても、動き続けている収集タスクは古い（既に directory
//! から外れた）セッションを黙って読み続けてしまう。そこで`Self::rebuild`は
//! `sync_slmp_sessions`が返す解決済みダイヤル先を
//! `banto_collect::CollectorConfig::set_broker_dial_target`で該当接続の
//! plan に書き戻す - `ProtocolConfig`（実際の接続には使われない、diff 専用の
//! 値）が変わることで`apply_config`が正しく「replaced」に分類し、新しい
//! `ClientFactory`を伴ってタスクを再起動する（`set_broker_dial_target`の
//! doc comment参照）。
//!
//! ## all-or-nothing の実現（設計 §4.3 最終段落、T7 で `apply_config` にも
//! 継承）
//!
//! [`CollectorManager::rebuild`] は `build_config`（純粋な読み取り、
//! 副作用なし）→ Collector 側の適用（`apply_config` または
//! `start_with_client_factory` - どちらも「失敗したら呼び出し前の状態を
//! 完全に維持する」契約を持つ）→ 成功して初めて `map`/`revision`/
//! `last_error`/`last_apply` を commit、という順序を守る。Collector 側が
//! `Err` を返した場合（`apply_config` の失敗要因は tstore の writer を
//! 開けない場合のみ - banto-collect 側のモジュール doc 参照）は
//! `last_error` に記録するだけで catalog/`Collector`/`last_apply` は
//! 一切進めない。
//!
//! **既知の制約は T7 で解消（旧 T0〜T6 の記述を更新）**: 従来の全体再構築
//! 方式では、レジストリが同じ接続を指したまま構成が変わった場合（例:
//! 既存タグの追加編集）、新旧 `Collector` が短時間だけ同じ PLC へ同時に
//! ソケットを張る瞬間がありえた。T7 の部分適用では変更のあった接続だけが
//! 「旧タスクを stop してから新タスクを spawn」する順序で扱われる
//! （`banto_collect::Collector::apply_config` 自身のモジュール doc 参照）
//! ため、この「二重接続窓」はもう発生しない - 新旧 Collector という概念
//! 自体が存在しない（同一の `Collector` インスタンスを in-place で
//! 書き換えるだけ）。
//!
//! ## `rebuild` は直列化されている（監査レビュー指摘・2026-08-05 対応）
//!
//! [`CollectorManager::rebuild`] 全体（レジストリ読み取り〜commit〜旧
//! `Collector` の stop）は `rebuild_lock`（`tokio::sync::Mutex<()>`）で
//! 直列化する。直列化なしだと、管理 UI の2セッションが同時に I1 CRUD を
//! 叩いた場合に rebuild A（古いレジストリを読取）と rebuild B（新しい
//! レジストリを読取）が交錯し、**B が先に commit されたあとで A が commit**
//! すると「revision は2回進むが catalog/`Collector` は A が読んだ古い
//! レジストリ状態のまま、`last_error` は `None`」という不整合が外部から
//! 検知できない形で残ってしまう（catalog の revision と実体が食い違う）。
//! `rebuild_lock` を rebuild() の冒頭で取得し、返すまで保持することで、
//! 「最後に走り終えた rebuild が必ず最新のレジストリ状態を読んで commit
//! する」ことを保証する（ロック取得後にレジストリを読むため、後から
//! 呼ばれた rebuild は必ず先行 rebuild の commit 後の状態を見る）。
//! `inner` を保護する `std::sync::Mutex` は従来どおり短時間保持のまま
//! （await をまたがせない）— 直列化の役目は `rebuild_lock` だけが持つ。
//!
//! ## T1: `revision` watch と `subscribe_events`（設計 §4.1/§5.2）
//!
//! `crate::stream`（WebSocket 購読）が使う2つの購読口をここに足した:
//! [`CollectorManager::subscribe_revision`]（`rebuild` が commit する
//! たびに新しい revision を流す `watch` チャンネル - `config_changed` の
//! 送信元）と [`CollectorManager::subscribe_events`]（`CollectEvent` の
//! 中継元）。後者は `events` フィールド（[`EventSink`]、設計 §3.2/§4.1
//! ドキュメントどおり「manager が1個保有し再構築を跨いで不変」）から直接
//! subscribe する - 実行中の `Collector`（`CollectorManager::collector`）
//! から取ると、`Collector` が丸ごと新規作成される瞬間（起動時 / 空構成
//! からの復帰時）に古い方の受信側が孤立するため（T7 の部分適用が
//! 既存 `Collector` を in-place で書き換える経路ではこの懸念自体が
//! 発生しない）。
//!
//! ## T2-2: SLMP 接続は broker 経由（設計 §6-5、2026-08-05 決定）
//!
//! [`CollectorManager`] は [`crate::broker_glue::HubSessions`]（`sessions`
//! フィールド）を保持する - **`CollectorManager` の外**（`bin/banto-hub.rs`）
//! で構築・生存する共有 `Arc` で、`rebuild` を跨いでも SLMP セッションが
//! 切れない。`rebuild` は毎回 [`CollectorManager::sync_slmp_sessions`] で
//! レジストリの現在の SLMP 接続集合を `sessions` に
//! `ensure_connection`（新規なら起動・既存ならそのまま）し、得た
//! ハンドル群を `crate::broker_glue::hub_client_factory` に渡す - SLMP 接続は
//! [`crate::broker_glue::BrokerReadClient`]、Modbus 接続は従来どおりの
//! 直接クライアントで読む。旧 T0〜T6 の全体再構築方式では「新旧 `Collector`
//! が同じ PLC へ同時にソケットを張る瞬間」を SLMP について解消する役目も
//! 兼ねていたが、T7 の部分適用移行後は新旧 `Collector` という概念自体が
//! 無くなったため、この節の主眼は「broker セッションの追加+削除の完全同期」
//! に移った - このモジュールの doc 冒頭「SLMP broker セッションの削除同期
//! （T7-2）」参照。

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::{broadcast, watch};

use banto_broker::{BrokerConnectionStatus, BrokerError, BrokerHandle, ReadOnlyHandle};
use banto_collect::{
    build_config_from, ApplyReport, CollectEvent, Collector, CollectorOptions, ConnectionStatus,
    CurrentSample, CurrentValuesHandle, EventSink, Quality, RegistrySnapshot,
};
use banto_core::ListParams;
use banto_tags::{CollectionGroupService, PlcConnection, PlcConnectionService, Tag, TagService};
use banto_tstore::Clock;
use serde::Serialize;
use sqlx::SqlitePool;
use utoipa::ToSchema;

use crate::broker_glue::{hub_client_factory, HubSessions, SlmpSimRegistry};
use crate::computed::{self, ComputedEngine, ServerTagStore};
use crate::diag_log::DiagLog;

/// The effective `(value, quality, timestamp_ms)` triple for one catalog
/// entry - shared by `crate::rest`'s `/api/v1/values*` and `crate::stream`'s
/// `data` messages so the two read paths (poll vs. push) can never drift on
/// what "the current value of a tag" means (T1 実装指示: 「wire 形式は REST
/// /api/v1/values と同じ...共通ヘルパへの整理は可」).
///
/// A disabled tag (its own flag, or its group's/connection's) always reads
/// `(None, Quality::Bad, ...)` regardless of what a stale cached sample says
/// (design §4: 欠測を隠さない - see [`TagEntry`]'s own doc comment for the
/// same reasoning `rest.rs`'s original `value_entry` carried).
pub fn effective_sample(
    entry: &TagEntry,
    sample: Option<CurrentSample>,
    now_ms: i64,
) -> (Option<f64>, Quality, i64) {
    if !entry.enabled {
        (
            None,
            Quality::Bad,
            sample.map(|s| s.ptime_ms).unwrap_or(now_ms),
        )
    } else {
        match sample {
            Some(s) => (s.value, s.quality, s.ptime_ms),
            None => (None, Quality::Bad, now_ms),
        }
    }
}

/// T6-2 (docs/tag-server-design.md §4.2/§4.3(a)): the single read-path
/// unification point every IF (REST `values`/`status`, WS, MQTT, gRPC) must
/// call through instead of reading [`CurrentValuesHandle`] directly - the
/// T6-2 implementation instructions asked for exactly this ("既存の
/// hub::effective_sample / TagMap 参照箇所を...共通ヘルパに集約...個別 IF に
/// 分岐を撒かない"). `entry.tag_kind` decides the source: `"plc"` reads
/// `collect` ([`CurrentValuesHandle`], written by `banto-collect`'s
/// collection tasks); `"computed"`/`"internal"` read `server_store`
/// ([`ServerTagStore`], written by [`ComputedEngine`]'s evaluation loop /
/// `crate::write_path::execute_write`'s internal-tag branch) - both keyed by
/// the same `entry.tag_key` (`"tag:{id}"`) convention, so a rename never
/// loses the current value either way (`TagEntry::tag_key`'s own doc
/// comment). Delegates entirely to [`effective_sample`] once the right
/// `Option<CurrentSample>` is in hand, so the `!entry.enabled` /
/// missing-sample rules stay defined in exactly one place regardless of
/// `tag_kind`.
pub fn read_current(
    entry: &TagEntry,
    collect: Option<&CurrentValuesHandle>,
    server_store: &ServerTagStore,
    now_ms: i64,
) -> (Option<f64>, Quality, i64) {
    let sample = if entry.tag_kind == banto_tags::PLC_TAG_KIND {
        collect.and_then(|c| c.get(&entry.tag_key))
    } else {
        server_store.get(&entry.tag_key)
    };
    effective_sample(entry, sample, now_ms)
}

/// The wire string for a [`Quality`] - `"good"`/`"bad"`/`"stale"`, identical
/// across every `/api/v1/*` surface (REST and WebSocket).
pub fn quality_str(quality: Quality) -> &'static str {
    match quality {
        Quality::Good => "good",
        Quality::Bad => "bad",
        Quality::Stale => "stale",
    }
}

/// `tag:{id}` - must stay byte-for-byte identical to `banto_collect`'s own
/// (private) key derivation (`crates/banto-collect/src/config.rs::tag_key`,
/// documented as stable in the T0-1 implementation instructions: "タグキーは
/// `tag:{id}`") - this is how [`TagEntry::tag_key`] is used to look up
/// [`banto_collect::CurrentSample`]s in [`CollectorManager::current_values`]'s
/// handle.
fn tag_key(id: i64) -> String {
    format!("tag:{id}")
}

/// One catalog entry: an external name bound to a stable internal tag,
/// carrying everything a REST client needs to display/interpret it (design
/// §4.1 "catalog はバインディング契約である" + §5.1 "PLC アドレスを既定で
/// 含める", 2026-08-05 決定).
///
/// `enabled` here is the *effective* collected state (`connection.enabled &&
/// group.enabled && tag.enabled`), not just the tag row's own `enabled`
/// column - a tag under a disabled group/connection is just as uncollected
/// as a tag that is itself disabled, and design §4 requires the catalog not
/// hide that ("欠測を隠さない"). This is a T0-1 judgment call (the
/// implementation instructions specify a single `enabled: bool` field
/// without pinning which of the three flags it reflects); documented here so
/// the REST layer and tests share the same understanding.
///
/// Field names are plain `snake_case` on the wire (no `camelCase` rename,
/// unlike the admin-UI resources' JSON) - the `/api/v1/*` namespace is
/// machine-client-facing (design §5.1/§5.6) and the design doc's own
/// examples for sibling `/api/v1/*` payloads (`t`, `v`, `q`,
/// `last_config_error`) are snake_case, not camelCase.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct TagEntry {
    pub external_name: String,
    pub tag_key: String,
    /// Stable `(connection_id, group_id, tag_id)` - the "同じ ID なら
    /// リネームされた/消えたら削除された" signal design §4.1 calls for.
    ///
    /// `#[schema(value_type = Vec<i64>)]` (T0-2, docs/tag-server-design.md
    /// §10-6 utoipa 採用): utoipa's `ToSchema` derive has no blanket impl for
    /// arbitrary tuples, so the OpenAPI schema for this field is declared as
    /// a 3-element JSON array of integers instead - this is a schema-only
    /// annotation, it does not touch how `serde` actually serializes the
    /// tuple (still a bare `[id, id, id]` JSON array either way, so the wire
    /// format this crate's tests assert on is unchanged).
    #[schema(value_type = Vec<i64>)]
    pub ids: (i64, i64, i64),
    pub connection: String,
    pub group: String,
    pub name: String,
    pub address: String,
    pub data_type: String,
    pub unit: Option<String>,
    pub decimals: i64,
    pub period_ms: i64,
    pub enabled: bool,
    /// Write opt-in (design §4 "メタデータ: ...書き込み可否(§6)を catalog 系
    /// API で公開", §6 item 1).
    pub writable: bool,
    /// One of `banto_tags::ALLOWED_TAG_KINDS` (`"plc"`/`"computed"`/
    /// `"internal"`, design §4.2). T2-3 deferred exposing this in the catalog
    /// ("T6 まで非公開" - `tag_kind` was always `"plc"` back then, so there
    /// was nothing to distinguish); **T6-2 lifts that now that all three
    /// species exist** (catalog exposure decision, this field). This is also
    /// the field [`crate::hub::read_current`] branches on - no other IF
    /// should re-derive "is this tag PLC-backed" from anything else.
    pub tag_kind: String,
    /// Computed-tag formula source (design §4.2, T6-2). `Some` only for
    /// `tag_kind == "computed"` - `None` for `"plc"`/`"internal"` (mirrors
    /// `banto_tags::Tag::expression`'s own invariant, enforced at
    /// registration by `banto_tags::tag::validate_tag_input`).
    pub expression: Option<String>,
    /// Internal-tag "restore last value on restart" flag (design §4.2,
    /// T6-2). Meaningless for `"plc"`/`"computed"` tags (always `false` for
    /// them - mirrors `banto_tags::Tag::retain`).
    pub retain: bool,
    /// T9-2 (docs/ux-plan.md §1, accident-prevention requirement (b)):
    /// mirrors the tag's *owning connection's* `simulation` flag
    /// (`banto_tags::PlcConnection::simulation`), not the tag's own row -
    /// tags have no such column of their own. External clients (and the
    /// future T10 tag monitor) need this to tell that a tag's live value is
    /// synthetic (produced by an in-process simulator,
    /// `banto_collect::simulation`/`crate::broker_glue::SlmpSimRegistry`),
    /// not read from a real PLC.
    pub simulation: bool,
}

/// Immutable snapshot of the external-name catalog, rebuilt from scratch on
/// every [`CollectorManager::rebuild`] (design §4.1's "revision" is the
/// generation counter of exactly this snapshot). Cheap to clone (wrap in
/// `Arc`) since a REST handler reading the catalog must not block a
/// concurrent rebuild.
#[derive(Debug, Default)]
pub struct TagMap {
    by_external: HashMap<String, TagEntry>,
    /// Stable display/listing order: connection name, then group name, then
    /// tag name (design's own hub.rs sketch: "catalog の安定順(connection,
    /// group, tag の名前順)"). `tags.name`/`collection_groups.name`/
    /// `plc_connections.name` are each globally `UNIQUE` in the registry
    /// schema, so external names never collide.
    ordered: Vec<String>,
}

impl TagMap {
    fn empty() -> Self {
        Self::default()
    }

    pub fn get(&self, external_name: &str) -> Option<&TagEntry> {
        self.by_external.get(external_name)
    }

    /// Every entry, in the stable catalog order.
    pub fn iter(&self) -> impl Iterator<Item = &TagEntry> {
        self.ordered
            .iter()
            .filter_map(move |name| self.by_external.get(name))
    }

    pub fn len(&self) -> usize {
        self.ordered.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ordered.is_empty()
    }

    /// Test-only builder helper: insert one entry directly, bypassing
    /// `build_catalog`'s registry read. `crate::computed`'s unit tests use
    /// this to build a `TagMap` in-memory (no SQLite registry needed) for
    /// `computed::build_plan`/`ComputedEngine::evaluate_tick` scenarios -
    /// `TagMap` otherwise has no public constructor from a plain entry list
    /// (production code only ever builds one from the registry).
    #[cfg(test)]
    pub(crate) fn insert_for_test(&mut self, entry: TagEntry) {
        self.ordered.push(entry.external_name.clone());
        self.by_external.insert(entry.external_name.clone(), entry);
    }
}

/// Build a fresh [`TagMap`] straight from the registry (I1), independent of
/// [`build_config`] - the catalog must show *every* tag, enabled or not
/// (design §4: "欠測を隠さない"), while `build_config` deliberately only
/// resolves the enabled/reachable subset it will actually collect. A tag
/// whose group or connection row cannot be found (should not happen - both
/// are `NOT NULL REFERENCES ... ON DELETE RESTRICT` - but defensive against
/// a future relaxation) is skipped rather than panicking.
pub async fn build_catalog(pool: &SqlitePool) -> Result<TagMap, banto_core::BantoError> {
    let connections = PlcConnectionService::new(pool.clone())
        .list(ListParams::default())
        .await?
        .rows;
    let groups = CollectionGroupService::new(pool.clone())
        .list(ListParams::default())
        .await?
        .rows;
    let tags: Vec<Tag> = TagService::new(pool.clone())
        .list(ListParams::default())
        .await?
        .rows;

    build_catalog_from(&RegistrySnapshot {
        connections,
        groups,
        tags,
    })
}

/// Build the external-name catalog from one registry snapshot. This is kept
/// separate from the pool-reading compatibility wrapper so a catalog commit
/// and a collector preflight can use exactly the same logical registry read.
pub fn build_catalog_from(snapshot: &RegistrySnapshot) -> Result<TagMap, banto_core::BantoError> {
    let connections = &snapshot.connections;
    let groups = &snapshot.groups;
    let tags = &snapshot.tags;
    let conn_by_id: HashMap<i64, _> = connections.iter().map(|c| (c.id, c)).collect();
    let group_by_id: HashMap<i64, _> = groups.iter().map(|g| (g.id, g)).collect();

    let mut entries: Vec<TagEntry> = Vec::with_capacity(tags.len());
    for tag in tags {
        let Some(group) = group_by_id.get(&tag.collection_group_id) else {
            continue;
        };
        let Some(conn) = conn_by_id.get(&group.plc_connection_id) else {
            continue;
        };
        entries.push(TagEntry {
            external_name: format!("{}.{}.{}", conn.name, group.name, tag.name),
            tag_key: tag_key(tag.id),
            ids: (conn.id, group.id, tag.id),
            connection: conn.name.clone(),
            group: group.name.clone(),
            name: tag.name.clone(),
            address: tag.address.clone(),
            data_type: tag.data_type.clone(),
            unit: tag.unit.clone(),
            decimals: tag.decimals,
            period_ms: group.period_ms,
            enabled: conn.enabled && group.enabled && tag.enabled,
            writable: tag.writable,
            tag_kind: tag.tag_kind.clone(),
            expression: tag.expression.clone(),
            retain: tag.retain,
            simulation: conn.simulation,
        });
    }

    entries.sort_by(|a, b| {
        (&a.connection, &a.group, &a.name).cmp(&(&b.connection, &b.group, &b.name))
    });

    let mut by_external = HashMap::with_capacity(entries.len());
    let mut ordered = Vec::with_capacity(entries.len());
    for entry in entries {
        ordered.push(entry.external_name.clone());
        by_external.insert(entry.external_name.clone(), entry);
    }

    Ok(TagMap {
        by_external,
        ordered,
    })
}

/// Mutable state behind [`CollectorManager`]'s `inner` (`std::sync::Mutex`,
/// short-held, never across an `.await` - unchanged discipline from before
/// T7): the current catalog snapshot, the generation counter, the last
/// rebuild failure (if any), the cached [`CurrentValuesHandle`] (T7 - see
/// this module's doc comment, "`apply_config` 呼び出し時の読み取り整合性",
/// for why this is cached here instead of read through
/// [`CollectorManager::collector`] on every call), and the most recent
/// [`ApplyReport`] (T7-2, surfaced at `/api/v1/status`).
///
/// The running [`Collector`] itself is **not** in here - see
/// [`CollectorManager::collector`]'s own field doc comment for why it needs a
/// different (async-aware) lock.
struct Inner {
    map: Arc<TagMap>,
    revision: u64,
    running_revision: u64,
    last_error: Option<String>,
    /// `None` when no `Collector` is running (nothing enabled, or before the
    /// first successful rebuild) - mirrors the old
    /// `inner.collector.as_ref().map(Collector::current_values)` exactly,
    /// just computed once at the moment a `Collector` is created/replaced
    /// instead of on every read (see this module's doc comment for why that
    /// is safe: the handle's identity never changes across an `apply_config`
    /// call on the same `Collector`).
    current: Option<CurrentValuesHandle>,
    /// The most recent `apply_config` call's report, or `None` if the last
    /// successful rebuild did not go through `apply_config` (a fresh
    /// `Collector` start, or a transition to the empty state - see
    /// [`CollectorManager::rebuild`]'s doc comment). Cleared together with
    /// `current` whenever a rebuild does not call `apply_config`.
    last_apply: Option<ApplyReport>,
}

/// Owns the running [`Collector`]'s lifecycle end to end (design §3.2 table
/// "構成変更の扱いも banto-collect の司令塔決定に従う"): start once at boot,
/// reconfigure (in place, T7) on every registry write, expose the read
/// handles the REST layer needs (`current_values`/`connection_status`/
/// `tag_map`/`revision`/`last_error`).
///
/// Shared behind `Arc` by the REST layer (cloning a `CollectorManager`
/// itself is deliberately not supported - a `Collector` is not `Clone` and
/// there must be exactly one lifecycle owner per process).
pub struct CollectorManager {
    pool: SqlitePool,
    data_dir: PathBuf,
    clock: Arc<dyn Clock>,
    options: CollectorOptions,
    /// Built once in [`CollectorManager::new`] and cloned into every
    /// `Collector::start` call (cheap - `EventSink` is `Arc`-backed) rather
    /// than rebuilt per rebuild, so a live event subscriber's
    /// `broadcast::Receiver` survives a rebuild instead of being silently
    /// orphaned. (T0 REST does not expose live event subscription - `/api/v1
    /// /events` reads the durable `collect_events` table directly - but the
    /// collector-internal event flow (`plc_connected`/threshold edges/etc.)
    /// still needs a stable sink to persist to.)
    events: EventSink,
    /// T2-2 (docs/tag-server-design.md §6-5): the broker session directory,
    /// owned OUTSIDE this manager (`bin/banto-hub.rs` constructs it and holds
    /// its own `Arc` clone for final shutdown) so an SLMP session survives a
    /// `rebuild` - see `crate::broker_glue::HubSessions`'s doc comment for
    /// the full rationale and the session-sync policy `rebuild` follows.
    sessions: Arc<HubSessions>,
    /// T9-2 (docs/ux-plan.md §1): the SLMP simulator registry, owned OUTSIDE
    /// this manager for exactly the same reason `sessions` is - it must
    /// survive every `rebuild` (a simulator started for a connection stays
    /// up across rebuilds that leave it `simulation = true`, mirroring how a
    /// broker session stays up), and `bin/banto_hub` needs its own `Arc`
    /// clone to call `SlmpSimRegistry::shutdown` at the correct point in
    /// process shutdown (after `sessions.shutdown()` - simulators must
    /// outlive the broker sessions that dial them). See
    /// `crate::broker_glue::SlmpSimRegistry`'s doc comment for the full
    /// mechanism, and this module's doc comment ("SLMP 接続単位の
    /// シミュレーションモード") for how `rebuild` uses it.
    sim_registry: Arc<SlmpSimRegistry>,
    /// T6-2 (docs/tag-server-design.md §4.2/§4.3(a)): owned OUTSIDE this
    /// manager for the same reason `sessions` is (`bin/banto-hub.rs`
    /// constructs it and keeps its own `Arc` clone so `ServerTagStore`'s
    /// retained values and the running evaluation loop both outlive any
    /// single `rebuild`). [`CollectorManager::rebuild`] calls
    /// [`ComputedEngine`]'s pure `crate::computed::build_plan` against the
    /// freshly-built catalog and commits the result in the SAME
    /// all-or-nothing step as the catalog/`Collector` swap (§4.3(a): "変更の
    /// 影響半径 = 触ったものだけ" - a bad computed-tag expression must leave
    /// the entire rebuild, not just the computed engine, on the old state).
    computed: Arc<ComputedEngine>,
    /// The running `Collector` itself (`None` when nothing is enabled to
    /// collect - a normal state, not an error, see
    /// [`CollectorManager::rebuild`]). **`tokio::sync::Mutex`, not
    /// `std::sync::Mutex`** (T7): [`Collector::apply_config`] needs `&mut
    /// Collector` held across several `.await` points (stopping/joining
    /// changed tasks, possibly reopening the tstore writer), which a std
    /// lock must never be held across (this module's doc comment on
    /// `rebuild_lock` states the same discipline for `inner`). See this
    /// module's doc comment ("`apply_config` 呼び出し時の読み取り整合性") for
    /// why [`CollectorManager::connection_status`] (the one reader that goes
    /// through this lock) is `async` while [`CollectorManager::current_values`]
    /// is not.
    collector: AsyncMutex<Option<Collector>>,
    inner: Mutex<Inner>,
    /// Serializes the whole body of [`CollectorManager::rebuild`] - see this
    /// module's doc comment ("`rebuild` は直列化されている") for why a plain
    /// `inner` lock alone is not enough (it is only held across the short
    /// commit step, not across the registry read that precedes it).
    rebuild_lock: AsyncMutex<()>,
    /// T1 (docs/tag-server-design.md §4.1「構成変更通知」・§5.2の
    /// `config_changed`): the current `revision`, mirrored onto a
    /// `watch` channel so `crate::stream`'s per-connection tasks can await
    /// "revision changed" without polling. Sent from inside the same
    /// `inner` critical section that advances `revision` (see
    /// [`CollectorManager::rebuild`]), so a `watch::Receiver` never observes
    /// a `revision()` that is stale relative to what it already saw here -
    /// only ever the same value or a later one. A `watch` channel coalesces
    /// rapid updates (a receiver that is not actively awaiting only ever
    /// sees the *latest* value, not every intermediate one) - harmless here
    /// because `config_changed`'s contract is "re-fetch the catalog", and
    /// re-fetching once at the latest revision is equivalent to re-fetching
    /// once per intermediate revision.
    revision_tx: watch::Sender<u64>,
    /// T9-2 フォローアップ（2026-08-06、`crate::diag_log` モジュール doc
    /// 参照）: `rebuild`/`sync_slmp_sessions`/`log_simulation_warnings` の
    /// 診断ログの出力先。[`Self::new`] では [`DiagLog::default`]（素の
    /// `println!`/`eprintln!` と同じ）で初期化され、[`Self::with_diag_log`]
    /// を呼んだ場合のみ差し替わる - `bin/banto_hub` はここへ
    /// `hub_log::log_line`/`log_err_line` を配線し、Windows サービスモード
    /// でもこれらの診断がサービスログファイルに届くようにする。
    diag_log: DiagLog,
}

impl CollectorManager {
    /// `clock` is shared with the store (rotation) and the current-value
    /// cache (staleness) - pass `Arc::new(SystemClock)` in production, a
    /// `ManualClock` in tests, same contract as `Collector::start`. `sessions`
    /// is the broker session directory (T2-2, §6-5) - constructed and owned
    /// by the caller (`bin/banto-hub.rs`) so it outlives every
    /// `CollectorManager::rebuild`; see [`CollectorManager`]'s `sessions`
    /// field doc comment. `sim_registry` (T9-2) is the SLMP simulator
    /// registry, constructed and owned the same way - see
    /// [`CollectorManager`]'s `sim_registry` field doc comment.
    pub fn new(
        pool: SqlitePool,
        data_dir: PathBuf,
        clock: Arc<dyn Clock>,
        options: CollectorOptions,
        sessions: Arc<HubSessions>,
        sim_registry: Arc<SlmpSimRegistry>,
        computed: Arc<ComputedEngine>,
    ) -> Self {
        let events = EventSink::new(pool.clone());
        let (revision_tx, _revision_rx) = watch::channel(0);
        Self {
            pool,
            data_dir,
            clock,
            options,
            events,
            sessions,
            sim_registry,
            computed,
            collector: AsyncMutex::new(None),
            inner: Mutex::new(Inner {
                map: Arc::new(TagMap::empty()),
                revision: 0,
                running_revision: 0,
                last_error: None,
                current: None,
                last_apply: None,
            }),
            rebuild_lock: AsyncMutex::new(()),
            revision_tx,
            diag_log: DiagLog::default(),
        }
    }

    /// T9-2 フォローアップ (2026-08-06): この manager の診断ログ
    /// （`rebuild`/`sync_slmp_sessions`/`log_simulation_warnings`）を
    /// `hub_log::log_line`/`log_err_line` 経由にルーティングし、Windows
    /// サービスモードでもサービスログファイルへ届くようにする - `bin/
    /// banto_hub` が呼ぶ。これを呼ばなければ既定の素の
    /// `println!`/`eprintln!` 相当のまま（`crate::diag_log` モジュール doc
    /// 参照）。
    pub fn with_diag_log(mut self, diag_log: DiagLog) -> Self {
        self.diag_log = diag_log;
        self
    }

    /// T6-2: the shared computed/internal-tag current-value store - the
    /// non-PLC counterpart to [`Self::current_values`]. Every read IF should
    /// reach this only through [`read_current`] (this module's function),
    /// never call [`ServerTagStore::get`] directly.
    pub fn server_store(&self) -> Arc<ServerTagStore> {
        self.computed.server_store()
    }

    /// T6-2: the shared [`ComputedEngine`] - `bin/banto-hub.rs` clones this
    /// to hand to the background 250ms evaluation loop task (this struct's
    /// `computed` field doc comment: the engine's plan/state must outlive any
    /// single `rebuild`, but the tick loop itself is not part of `rebuild`).
    pub fn computed_engine(&self) -> Arc<ComputedEngine> {
        self.computed.clone()
    }

    /// The shared registry pool - handed to callers (e.g. `rest.rs`'s
    /// `/api/v1/status` handler, which needs connection names the catalog
    /// alone does not carry for a connection with zero tags) rather than
    /// duplicated.
    pub fn pool(&self) -> SqlitePool {
        self.pool.clone()
    }

    /// T12(docs/ux-plan.md §4)の接続テストAPIが、broker経由SLMP接続の既存
    /// セッションを覗くために使う - `HubSessions::handle_for`参照。
    pub fn sessions(&self) -> &Arc<HubSessions> {
        &self.sessions
    }

    /// The shared clock (design: 値のタイムスタンプはサンプル取得時刻 - `rest.rs`
    /// uses this for the `/api/v1/values` snapshot's own `t` field and for a
    /// sample-less tag's fallback timestamp).
    pub fn clock(&self) -> Arc<dyn Clock> {
        self.clock.clone()
    }

    /// Rebuild the catalog and reconfigure the `Collector` from the current
    /// registry state (design §4.3, T7: "部分適用"). Called once at boot and
    /// after every I1 CRUD write that succeeds.
    ///
    /// On success: `revision` advances by exactly 1, `last_error` clears,
    /// and the new catalog/`Collector` state (or no `Collector` at all, if
    /// nothing is enabled - see below) are live.
    ///
    /// On failure (registry read error, a config-level problem
    /// `build_config` catches - e.g. an unparsable address - or a
    /// `Collector` lifecycle failure, from either `apply_config` or
    /// `start_with_client_factory`): the OLD catalog and OLD `Collector`
    /// state are left completely untouched, `revision` does not advance, and
    /// `last_error` is set to the failure message. The caller (an I1 CRUD
    /// handler) must NOT treat this `Err` as its own failure - the write
    /// itself already succeeded; only the collector's view is stale until
    /// the registry is fixed and rebuilt again (design's T0-1 instructions:
    /// "rebuild 失敗は CRUD 自体の失敗にしない").
    ///
    /// A registry with nothing enabled (`build_config` returns zero
    /// connections) is NOT a failure: this stops any previously-running
    /// `Collector` outright (see this module's doc comment - the T7-2
    /// instructions keep this branch as the pre-T7 "stop" path rather than
    /// routing it through `apply_config`), commits an empty (or whatever it
    /// resolves to) catalog, and still advances `revision` - a legitimate
    /// "collecting nothing" state, not an error (design's instructions: "タグ
    /// が0件でも正常起動")．
    ///
    /// **Serialized**: concurrent callers queue on `rebuild_lock` and run one
    /// at a time, each reading the registry fresh after acquiring the lock -
    /// see this module's doc comment ("`rebuild` は直列化されている") for why
    /// this matters (without it, two racing rebuilds could commit
    /// out of order and leave `revision` advanced but the catalog/`Collector`
    /// reflecting a stale registry read).
    pub async fn rebuild(&self) -> Result<(), String> {
        let _guard = self.rebuild_lock.lock().await;

        let snapshot = match RegistrySnapshot::load(&self.pool).await {
            Ok(snapshot) => snapshot,
            Err(err) => {
                let message = format!("レジストリのスナップショット取得に失敗しました: {err}");
                self.set_last_error(message.clone());
                return Err(message);
            }
        };

        let new_map = match build_catalog_from(&snapshot) {
            Ok(map) => map,
            Err(err) => {
                let message = format!("catalog の読み取りに失敗しました: {err}");
                self.set_last_error(message.clone());
                return Err(message);
            }
        };

        // T6-2 (docs/tag-server-design.md §4.2/§4.3(a)): compile every
        // `computed` tag's expression against `new_map` and validate the DAG
        // - pure computation, no mutation of `self.computed` yet (mirrors
        // `build_catalog`/`build_config` just above: a failure here must
        // leave EVERYTHING - catalog, `Collector`, and the computed engine's
        // plan - on the old state, §4.3(a)'s all-or-nothing). Committed only
        // at the same point the catalog/`Collector` swap happens, below.
        let computed_plan = match computed::build_plan(&new_map) {
            Ok(plan) => plan,
            Err(err) => {
                let message = format!("演算タグの検証に失敗しました: {err}");
                self.set_last_error(message.clone());
                return Err(message);
            }
        };

        let mut config = match build_config_from(&snapshot) {
            Ok(config) => config,
            Err(err) => {
                let message = err.to_string();
                self.set_last_error(message.clone());
                return Err(message);
            }
        };

        // T2-2/T7-2 (docs/tag-server-design.md §6-5/§4.3): additive
        // ensure_connection (unchanged from T2-2) plus the set of tracked
        // ids that are no longer wanted - see `Self::sync_slmp_sessions`'s
        // doc comment. Deliberately unconditional (runs even when
        // `config.group_count() == 0` just below) - a connection can be
        // enabled with no collectible groups yet and still deserve a live
        // broker session ready for T2-4's write path, and this step never
        // touches `inner`/`collector` so it carries no all-or-nothing risk
        // either way. `stale_slmp_ids` is only actually removed AFTER a
        // successful commit below - see `Self::remove_stale_slmp_sessions`'s
        // doc comment for why the ordering matters.
        let (slmp_handles, stale_slmp_ids, resolved_slmp_targets) =
            self.sync_slmp_sessions_from(&snapshot).await;

        // T9-2: every key in `slmp_handles` is, by construction, a
        // broker-routed enabled SLMP connection whose dial target
        // `Self::sync_slmp_sessions` already resolved (simulator substitution
        // included, via `SlmpSimRegistry::resolve`) before `ensure_connection`
        // ran. Telling `Collector` to treat these as `simulation = false`
        // stops it from starting a second, redundant in-process simulator for
        // a connection `SlmpSimRegistry` already simulates - see
        // `CollectorConfig::suppress_simulation_for`'s doc comment
        // (`crates/banto-collect/src/config.rs`) and this module's doc
        // comment ("SLMP 接続単位のシミュレーションモード").
        config.suppress_simulation_for(&slmp_handles.keys().cloned().collect());

        // T9-2: also stamp each broker-routed SLMP plan with the SAME
        // resolved dial target `sync_slmp_sessions` just used, so a
        // simulation toggle (or a simulator restart, or an in-place host/port
        // edit) that actually moved the broker session makes `apply_config`'s
        // `PartialEq` diff notice and respawn that connection's task with a
        // fresh `ClientFactory` - suppressing `simulation` alone leaves the
        // plan otherwise byte-for-byte identical across such a toggle, which
        // would classify the connection "unchanged" and leave its running
        // task wired to a now-superseded broker session forever. See
        // `CollectorConfig::set_broker_dial_target`'s doc comment
        // (`crates/banto-collect/src/config.rs`) for the full derivation.
        for (key, (host, port)) in &resolved_slmp_targets {
            config.set_broker_dial_target(key, host.clone(), *port);
        }

        // `CollectorConfig`'s internals are `pub(crate)` to banto-collect, so
        // `group_count() == 0` is the public equivalent of "nothing
        // collectible" - `build_config` already drops any connection with
        // zero collectible groups (see its own doc comment), so this is
        // exactly the same condition `Collector::start_with_client_factory`'s
        // own `connections.is_empty()` check would use.
        if config.group_count() == 0 {
            // T6-2: commit the validated computed-tag plan in the same
            // all-or-nothing step as the catalog swap below - see
            // `computed_plan`'s own comment above.
            self.computed.commit(computed_plan);
            let old_collector = self.collector.lock().await.take();
            let new_revision = {
                let mut inner = self.inner.lock().expect("hub state lock poisoned");
                inner.map = Arc::new(new_map);
                inner.revision += 1;
                inner.last_error = None;
                inner.current = None;
                inner.last_apply = None;
                inner.revision
            };
            // Sent while still holding `rebuild_lock` (not `inner`'s lock,
            // already released above) - see this struct's `revision_tx` doc
            // comment for why serializing the send against `rebuild`'s own
            // serialization is what keeps a `watch::Receiver` from ever
            // observing a value older than what `revision()` already reports.
            let _ = self.revision_tx.send(new_revision);
            if let Some(collector) = old_collector {
                let _ = collector.stop().await;
            }
            self.remove_stale_slmp_sessions(&stale_slmp_ids).await;
            self.log_simulation_warnings().await;
            return Ok(());
        }

        // T7 (docs/tag-server-design.md §4.3): reconfigure the already-running
        // `Collector` in place via `apply_config` if one exists, otherwise
        // start a fresh one - see this module's doc comment for the full
        // derivation. Either path leaves `self.collector`/`self.inner`
        // completely untouched on `Err` (preserving the old Collector/TagMap
        // on failure), matching the pre-T7 "start the new one before
        // touching the old one" discipline - `apply_config` and
        // `start_with_client_factory` both carry that same "no partial
        // effect on failure" contract themselves now. T2-2: the client
        // factory routes SLMP connections through the broker sessions just
        // synced above and leaves Modbus connections on the default direct
        // client (`crate::broker_glue::hub_client_factory`'s doc comment).
        let factory = hub_client_factory(Arc::new(slmp_handles));
        let mut collector_guard = self.collector.lock().await;
        let commit: Result<(Option<ApplyReport>, CurrentValuesHandle), String> =
            if let Some(collector) = collector_guard.as_mut() {
                match collector.apply_config(config, factory).await {
                    Ok(report) => Ok((Some(report), collector.current_values())),
                    Err(err) => Err(err.to_string()),
                }
            } else {
                match Collector::start_with_client_factory(
                    config,
                    &self.data_dir,
                    self.clock.clone(),
                    self.events.clone(),
                    self.options,
                    factory,
                )
                .await
                {
                    Ok(collector) => {
                        let handle = collector.current_values();
                        *collector_guard = Some(collector);
                        Ok((None, handle))
                    }
                    Err(err) => Err(err.to_string()),
                }
            };
        drop(collector_guard);

        let (apply_report, current_handle) = match commit {
            Ok(pair) => pair,
            Err(message) => {
                self.set_last_error(message.clone());
                return Err(message);
            }
        };

        if let Some(report) = &apply_report {
            self.diag_log.err_line(&format!(
                "banto-hub: rebuild (部分適用) added={:?} removed={:?} replaced={:?} unchanged={:?} writer_rotated={}",
                report.added, report.removed, report.replaced, report.unchanged, report.writer_rotated,
            ));
        }

        // T6-2: commit alongside the catalog/`Collector` state - same
        // reasoning as the `group_count() == 0` branch above.
        self.computed.commit(computed_plan);
        let new_revision = {
            let mut inner = self.inner.lock().expect("hub state lock poisoned");
            inner.map = Arc::new(new_map);
            inner.revision += 1;
            inner.last_error = None;
            inner.current = Some(current_handle);
            inner.last_apply = apply_report;
            inner.revision
        };
        let _ = self.revision_tx.send(new_revision);

        self.remove_stale_slmp_sessions(&stale_slmp_ids).await;
        self.log_simulation_warnings().await;

        Ok(())
    }

    fn set_last_error(&self, message: String) {
        self.inner
            .lock()
            .expect("hub state lock poisoned")
            .last_error = Some(message);
    }

    /// T2-2/T7-2/T9-2 (docs/tag-server-design.md §6-5/§4.3, docs/ux-plan.md
    /// §1): additive `ensure_connection` (unchanged from T2-2) over
    /// `self.sessions`'s broker tasks against the registry's current
    /// enabled-SLMP-connection set, returning both the `"conn:{id}"`-keyed
    /// handle map [`crate::broker_glue::hub_client_factory`] needs AND the
    /// connection ids `self.sessions` still tracks that are NOT in that set
    /// (deleted from the registry, disabled, or no longer `protocol ==
    /// "slmp"`) - see `crate::broker_glue::HubSessions`'s doc comment
    /// ("Session sync policy") for the full T7-2 policy this implements.
    ///
    /// **T9-2 addition**: before calling `ensure_connection` for a
    /// connection, this now calls `self.sim_registry.resolve(conn)` to get
    /// the *effective* `(host, port)` to dial - the connection's own
    /// host/port unless `conn.simulation` is true, in which case it is the
    /// address of an in-process simulator `SlmpSimRegistry` starts/reuses on
    /// this connection's behalf (see `SlmpSimRegistry::resolve`'s doc
    /// comment). `ensure_connection` itself is called against a `PlcConnection`
    /// copy with only `host`/`port` swapped for the resolved values (`..conn.clone()`
    /// keeps everything else - id/name/protocol/enabled/unit_id/simulation -
    /// as the registry's own truth) - `banto_broker::SessionDirectory::ensure_connection`
    /// reuses a session purely by connection id, so if `resolve` reports the
    /// dial target *changed* since the last rebuild (simulation toggled,
    /// simulator restarted, or - incidentally - a real connection's host/port
    /// was edited in place), `self.sessions.remove(conn.id)` is called FIRST
    /// so `ensure_connection` is forced to spawn a fresh session against the
    /// new target instead of silently keeping the stale one alive forever -
    /// see `SlmpSimRegistry::resolve`'s doc comment for the full derivation
    /// of why this matters.
    ///
    /// The stale ids are returned, not removed here - [`Self::rebuild`]
    /// only calls [`Self::remove_stale_slmp_sessions`] with them AFTER the
    /// collector-side commit for this same rebuild has succeeded (so any
    /// collect task reading through that connection's session is already
    /// confirmed stopped - see this module's doc comment "SLMP broker
    /// セッションの削除同期").
    ///
    /// A registry read failure here is logged and treated as "no SLMP
    /// connections this rebuild, nothing stale either" (empty on both
    /// counts: every SLMP connection falls back to
    /// `banto_collect::default_client_factory` for this one rebuild, per
    /// `hub_client_factory`'s defensive fallback, and no session is removed
    /// on a registry hiccup) rather than failing the whole `rebuild` - this
    /// sync step is additive/best-effort infrastructure wiring, not a
    /// correctness precondition for the collector build that follows it
    /// (unlike `build_catalog`/`build_config`, whose failures `rebuild` does
    /// propagate). Treating a read failure as "nothing stale" specifically
    /// avoids tearing down a session that is still wanted just because the
    /// registry could not be read this one time.
    ///
    /// **T9-2 third return value**: `resolved_targets` (`"conn:{id}"` ->
    /// `(host, port)`) carries the SAME resolved dial target `resolve` just
    /// computed for every broker-routed SLMP connection, regardless of
    /// whether it changed. [`Self::rebuild`] feeds every entry into
    /// `banto_collect::CollectorConfig::set_broker_dial_target` - necessary
    /// because `SlmpSimRegistry::resolve`'s `changed`-triggered
    /// `sessions.remove` + re-`ensure_connection` swaps the broker session
    /// underneath a connection, but does NOT by itself make the running
    /// collect task notice: `Collector::apply_config` only rebuilds a
    /// connection's task (and therefore its captured `ClientFactory`/handle)
    /// when that connection's whole `ConnectionPlan` compares unequal to the
    /// previous one, and neither `ConnectionPlan::simulation` (unconditionally
    /// forced `false` for broker-routed connections by
    /// `suppress_simulation_for`) nor its `ProtocolConfig`(mirrors the
    /// registry row verbatim, not the resolved target) reflects a
    /// simulation toggle or a simulator restart on its own - see
    /// `CollectorConfig::set_broker_dial_target`'s own doc comment for the
    /// full derivation (found necessary by this crate's own E2E coverage of
    /// the toggle path, `apps/banto-hub/core/tests/t9_simulation.rs`).
    async fn sync_slmp_sessions_from(
        &self,
        snapshot: &RegistrySnapshot,
    ) -> (
        HashMap<String, ReadOnlyHandle>,
        Vec<i64>,
        HashMap<String, (String, i64)>,
    ) {
        let mut handles = HashMap::new();
        let mut resolved_targets = HashMap::new();
        let mut wanted_ids: HashSet<i64> = HashSet::new();
        for conn in snapshot
            .connections
            .iter()
            .filter(|c| c.enabled && c.protocol == "slmp")
        {
            wanted_ids.insert(conn.id);

            // T9-2: resolve the effective dial target (simulator address if
            // `conn.simulation`, else the connection's own host/port) BEFORE
            // ensure_connection - see this fn's own doc comment and
            // `SlmpSimRegistry::resolve`'s doc comment for why the ordering
            // and the `changed`-triggered `sessions.remove` matter.
            let (host, port, changed) = self.sim_registry.resolve(conn).await;
            if changed {
                self.sessions.remove(conn.id);
            }
            let key = format!("conn:{}", conn.id);
            resolved_targets.insert(key.clone(), (host.clone(), port));
            let dial_conn = PlcConnection {
                host,
                port,
                ..conn.clone()
            };

            match self.sessions.ensure_connection(&dial_conn) {
                Ok(handle) => {
                    handles.insert(key, handle.read_only());
                }
                Err(err) => {
                    // Should not happen given the protocol filter above
                    // (UnsupportedProtocol) and banto-tags' own port
                    // validation (InvalidPort) - logged and skipped rather
                    // than failing the rebuild, matching this fn's doc
                    // comment. `hub_client_factory`'s defensive fallback
                    // covers this connection for the current rebuild.
                    self.diag_log.err_line(&format!(
                        "banto-hub: SLMP ブローカーセッションの起動に失敗しました (接続 {}): {err}",
                        conn.id
                    ));
                }
            }
        }

        let stale_ids: Vec<i64> = self
            .sessions
            .connection_ids()
            .into_iter()
            .filter(|id| !wanted_ids.contains(id))
            .collect();

        (handles, stale_ids, resolved_targets)
    }

    /// T14-2/T7-2/T9-2: [`crate::broker_glue::HubSessions::stop_and_join`] and
    /// [`crate::broker_glue::SlmpSimRegistry::remove`] for every id in
    /// `stale`. Must only be called AFTER the collector-side commit for the
    /// same rebuild has succeeded (see
    /// [`Self::rebuild`]/[`Self::sync_slmp_sessions`]'s doc comments) - by
    /// then, `apply_config`/the pre-commit `Collector` stop has already
    /// stopped any collect task that was reading through one of these
    /// connections' broker sessions. `async` (T9-2: `SlmpSimRegistry::remove`
    /// is `.await`-heavy, stopping a simulator's ramp task) - was sync before.
    async fn remove_stale_slmp_sessions(&self, stale: &[i64]) {
        for &connection_id in stale {
            let _ = self.sessions.stop_and_join(connection_id).await;
            self.sim_registry.remove(connection_id).await;
        }
    }

    /// T9-2 accident-prevention (c): a one-line warning listing every
    /// enabled `simulation = true` connection, printed after every
    /// successful `rebuild` commit (both the empty-config early return and
    /// the normal path) so an operator watching hub's stdout is reminded a
    /// simulated connection is live - simulation mode is a dev/test
    /// convenience (docs/ux-plan.md §1) and must never silently persist into
    /// production use unnoticed. Routed through `self.diag_log` (T9-2
    /// フォローアップ 2026-08-06、`crate::diag_log` モジュール doc 参照) so
    /// this warning reaches `bin/banto_hub`'s `hub_log` service log file
    /// (and therefore an operator running as a Windows service, not just a
    /// console) whenever `CollectorManager::with_diag_log` has been called -
    /// this is the main diagnostic the T9-2 followup fix targets. Anything
    /// that never calls `with_diag_log` keeps the default plain
    /// `println!`/`eprintln!` behavior ([`DiagLog::default`]). A registry
    /// read failure here is logged (`self.diag_log.err_line`) and otherwise
    /// ignored - this is a diagnostic, never a reason to fail the rebuild
    /// that already committed successfully.
    async fn log_simulation_warnings(&self) {
        let connections = match PlcConnectionService::new(self.pool.clone())
            .list(ListParams::default())
            .await
        {
            Ok(result) => result.rows,
            Err(err) => {
                self.diag_log.err_line(&format!(
                    "banto-hub: シミュレーション接続の確認のための接続一覧取得に失敗しました: {err}"
                ));
                return;
            }
        };

        let names: Vec<String> = connections
            .iter()
            .filter(|c| c.enabled && c.simulation)
            .map(|c| format!("{} (id={})", c.name, c.id))
            .collect();

        if !names.is_empty() {
            self.diag_log.line(&format!(
                "banto-hub: [注意] シミュレーションモードの接続が {} 件あります: {} - 本番運用では無効化してください",
                names.len(),
                names.join(", "),
            ));
        }
    }

    /// T2-2 (docs/tag-server-design.md §6-5): the broker's own connection
    /// status for an SLMP connection, or `None` if no broker session has ever
    /// been started for it (never SLMP, or no rebuild has run yet). Used by
    /// `crate::rest`'s `/api/v1/status` handler in place of
    /// [`Self::connection_status`] for connections whose `protocol ==
    /// "slmp"` - see `crate::broker_glue`'s module doc ("The two-backoff
    /// double bookkeeping") for why the broker's status, not
    /// banto-collect's own, is the one that answers "is the physical session
    /// up".
    pub fn broker_status(&self, connection_id: i64) -> Option<BrokerConnectionStatus> {
        self.sessions
            .status_watch(connection_id)
            .map(|watch| *watch.borrow())
    }

    /// T2-4（docs/tag-server-design.md §6 item 5「読み書き単一セッション」）:
    /// `conn`（SLMP 接続）の書き込み可能な [`BrokerHandle`] を取得する。
    /// `crate::rest` の書き込みハンドラの唯一の入口 - `self.sessions`
    /// （`Self::sync_slmp_sessions`が rebuild の度に確保する broker セッション
    /// directory）に委譲するだけで、`Self::sync_slmp_sessions`が読み取り専用
    /// ハンドルへ絞る（`ReadOnlyHandle`、`banto_collect::PlcClient` 経由）のと
    /// 対称的に、こちらは書き込み可能なフル `BrokerHandle` をそのまま返す
    /// （収集と書き込みが同じ物理セッションを通る、というのがこの broker
    /// 統合方針の核心 - 設計 §6 item 5）。
    ///
    /// `HubSessions::ensure_connection` は冪等（既存セッションがあれば
    /// それをそのまま返す）なので、直近の `rebuild` が既に確保済みの
    /// セッションと同じものが返る。`rebuild` と競合しても安全（同じ
    /// セッションに収束する）。
    pub fn write_broker_handle(&self, conn: &PlcConnection) -> Result<BrokerHandle, BrokerError> {
        self.sessions.ensure_connection(conn)
    }

    /// Current catalog snapshot. Cheap (`Arc` clone) - safe to call once per
    /// REST request.
    pub fn tag_map(&self) -> Arc<TagMap> {
        self.inner
            .lock()
            .expect("hub state lock poisoned")
            .map
            .clone()
    }

    pub fn revision(&self) -> u64 {
        self.inner.lock().expect("hub state lock poisoned").revision
    }

    /// The catalog revision. This named accessor makes the T14-3 distinction
    /// explicit while the older `revision()` API remains the configured
    /// revision on the existing REST/stream wire.
    pub fn configured_revision(&self) -> u64 {
        self.revision()
    }

    /// The revision of the currently applied/stopped collection run.
    pub fn running_revision(&self) -> u64 {
        self.inner
            .lock()
            .expect("hub state lock poisoned")
            .running_revision
    }

    /// Advance the run revision after a successful lifecycle operation.
    pub(crate) fn advance_running_revision(&self) -> u64 {
        let mut inner = self.inner.lock().expect("hub state lock poisoned");
        inner.running_revision += 1;
        inner.running_revision
    }

    /// Commit only the catalog and computed plan. No collector, broker
    /// session, or simulator is started or stopped by this method.
    pub async fn commit_catalog(&self, snapshot: &RegistrySnapshot) -> Result<u64, String> {
        let _guard = self.rebuild_lock.lock().await;
        let new_map = match build_catalog_from(snapshot) {
            Ok(map) => map,
            Err(err) => {
                let message = format!("catalog の検証に失敗しました: {err}");
                self.set_last_error(message.clone());
                return Err(message);
            }
        };
        let computed_plan = match computed::build_plan(&new_map) {
            Ok(plan) => plan,
            Err(err) => {
                let message = format!("演算タグの検証に失敗しました: {err}");
                self.set_last_error(message.clone());
                return Err(message);
            }
        };
        if let Err(err) = build_config_from(snapshot) {
            let message = err.to_string();
            self.set_last_error(message.clone());
            return Err(message);
        }

        self.computed.commit(computed_plan);
        let revision = {
            let mut inner = self.inner.lock().expect("hub state lock poisoned");
            inner.map = Arc::new(new_map);
            inner.revision += 1;
            inner.last_error = None;
            inner.revision
        };
        let _ = self.revision_tx.send(revision);
        Ok(revision)
    }

    /// Apply a fresh registry snapshot to a configured collection run. The
    /// snapshot is validated before broker/collector side effects begin.
    pub async fn apply_run(&self, mode: crate::controller::RunMode) -> Result<(), String> {
        if mode != crate::controller::RunMode::Configured {
            return Err("all_simulation は T15 で実装されます".to_string());
        }
        let _guard = self.rebuild_lock.lock().await;
        let snapshot = RegistrySnapshot::load(&self.pool)
            .await
            .map_err(|err| format!("レジストリのスナップショット取得に失敗しました: {err}"))?;
        let new_map = build_catalog_from(&snapshot)
            .map_err(|err| format!("catalog の検証に失敗しました: {err}"))?;
        computed::build_plan(&new_map)
            .map_err(|err| format!("演算タグの検証に失敗しました: {err}"))?;
        let mut config = build_config_from(&snapshot).map_err(|err| err.to_string())?;

        let (slmp_handles, stale_slmp_ids, resolved_slmp_targets) =
            self.sync_slmp_sessions_from(&snapshot).await;
        config.suppress_simulation_for(&slmp_handles.keys().cloned().collect());
        for (key, (host, port)) in &resolved_slmp_targets {
            config.set_broker_dial_target(key, host.clone(), *port);
        }

        if config.group_count() == 0 {
            let old_collector = self.collector.lock().await.take();
            if let Some(collector) = old_collector {
                let _ = collector.stop().await;
            }
            {
                let mut inner = self.inner.lock().expect("hub state lock poisoned");
                inner.current = None;
                inner.last_apply = None;
                inner.last_error = None;
            }
            self.remove_stale_slmp_sessions(&stale_slmp_ids).await;
            self.advance_running_revision();
            return Ok(());
        }

        let factory = hub_client_factory(Arc::new(slmp_handles));
        let mut collector_guard = self.collector.lock().await;
        let commit: Result<(Option<ApplyReport>, CurrentValuesHandle), String> =
            if let Some(collector) = collector_guard.as_mut() {
                match collector.apply_config(config, factory).await {
                    Ok(report) => Ok((Some(report), collector.current_values())),
                    Err(err) => Err(err.to_string()),
                }
            } else {
                match Collector::start_with_client_factory(
                    config,
                    &self.data_dir,
                    self.clock.clone(),
                    self.events.clone(),
                    self.options,
                    factory,
                )
                .await
                {
                    Ok(collector) => {
                        let handle = collector.current_values();
                        *collector_guard = Some(collector);
                        Ok((None, handle))
                    }
                    Err(err) => Err(err.to_string()),
                }
            };
        drop(collector_guard);
        let (apply_report, current_handle) = commit.inspect_err(|message| {
            self.set_last_error(message.clone());
        })?;
        {
            let mut inner = self.inner.lock().expect("hub state lock poisoned");
            inner.current = Some(current_handle);
            inner.last_apply = apply_report;
            inner.last_error = None;
        }
        self.remove_stale_slmp_sessions(&stale_slmp_ids).await;
        self.log_simulation_warnings().await;
        self.advance_running_revision();
        Ok(())
    }

    /// The most recent rebuild failure message, or `None` if the last
    /// rebuild (or the only one so far) succeeded. Surfaced at
    /// `/api/v1/status` as `last_config_error`.
    pub fn last_error(&self) -> Option<String> {
        self.inner
            .lock()
            .expect("hub state lock poisoned")
            .last_error
            .clone()
    }

    /// A handle onto the running collector's current-value cache, or `None`
    /// if nothing is currently collecting (no `Collector` running - either
    /// before the first successful [`CollectorManager::rebuild`], or because
    /// the registry currently has nothing enabled). Every tag in the catalog
    /// reads as `quality: "bad", value: null` in that case (design: 未収集
    /// タグは 404 にせず bad を返す) - the REST layer distinguishes "no
    /// current sample for this tag" from "tag is undefined" using the
    /// catalog, not this handle.
    ///
    /// **Deliberately synchronous, and deliberately NOT read through
    /// [`Self::collector`]** (T7) - see this module's doc comment
    /// ("`apply_config` 呼び出し時の読み取り整合性") for why a cached clone in
    /// `inner` is both safe and necessary: reading straight through the
    /// `Collector`'s own async lock would make this `async` and would block
    /// (or worse, read a torn `None`, if implemented naively) while an
    /// unrelated connection's `apply_config` is in flight - exactly the
    /// "other connections blip" regression T7 exists to remove.
    pub fn current_values(&self) -> Option<CurrentValuesHandle> {
        self.inner
            .lock()
            .expect("hub state lock poisoned")
            .current
            .clone()
    }

    /// The most recent `apply_config` call's [`ApplyReport`] (T7-2, design
    /// §4.3), or `None` if the last successful rebuild did not go through
    /// `apply_config` (a fresh `Collector` start, or a transition to the
    /// empty state - see [`Self::rebuild`]'s doc comment). Surfaced at
    /// `/api/v1/status` as `last_apply`.
    pub fn last_apply(&self) -> Option<ApplyReport> {
        self.inner
            .lock()
            .expect("hub state lock poisoned")
            .last_apply
            .clone()
    }

    /// T1 (docs/tag-server-design.md §5.2 要件7「イベント中継」): subscribe a
    /// live [`CollectEvent`] consumer. Deliberately goes through
    /// `self.events` (the one [`EventSink`] this manager builds once in
    /// [`CollectorManager::new`] and hands to every `Collector::start` call)
    /// rather than `self.inner.collector.subscribe_events()` - the running
    /// `Collector` is replaced wholesale on every [`CollectorManager::rebuild`]
    /// (this module's doc comment, "T0 は「全体再構築」"), which would orphan
    /// a receiver obtained from the old one. `EventSink` itself is `Arc`-backed
    /// and stays the same value across every rebuild (see its field doc
    /// comment above), so a WS connection's subscription survives a collector
    /// rebuild transparently - no re-subscribe needed after `config_changed`.
    pub fn subscribe_events(&self) -> broadcast::Receiver<CollectEvent> {
        self.events.subscribe()
    }

    /// T1 (docs/tag-server-design.md §4.1/§5.2「構成変更通知」): subscribe to
    /// `revision` changes without polling - see [`CollectorManager`]'s
    /// `revision_tx` field doc comment for the coalescing/ordering contract.
    pub fn subscribe_revision(&self) -> watch::Receiver<u64> {
        self.revision_tx.subscribe()
    }

    /// Per-connection status (`"conn:{id}"` keys, matching
    /// `banto_collect`'s own convention), empty when nothing is running.
    ///
    /// **`async`, unlike [`Self::current_values`]** (T7) - `banto_collect`
    /// has no public way to obtain a cacheable handle onto its internal
    /// status map (unlike [`CurrentValuesHandle`], `banto_collect::StatusMap`
    /// is `pub(crate)`), so this must call through [`Self::collector`]'s
    /// async lock on every call. This only ever *waits* for a concurrent
    /// `apply_config`/start to finish (never returns a stale/empty snapshot
    /// while one is in flight) - a latency cost, not a correctness one - see
    /// this module's doc comment for the full reasoning.
    pub async fn connection_status(&self) -> HashMap<String, ConnectionStatus> {
        self.collector
            .lock()
            .await
            .as_ref()
            .map(Collector::status)
            .unwrap_or_default()
    }

    /// Stop the running `Collector` cleanly (flushes tstore), if any. Called
    /// once at process shutdown (`bin/banto-hub.rs`).
    pub async fn shutdown(&self) {
        let old = self.collector.lock().await.take();
        if let Some(collector) = old {
            let _ = collector.stop().await;
        }
        let mut inner = self.inner.lock().expect("hub state lock poisoned");
        inner.current = None;
        inner.last_apply = None;
    }

    /// Stop a collection run while keeping the broker supervisor reusable for
    /// the next run. The collector is stopped first so no task retains a
    /// broker handle while the per-connection broker tasks are joined.
    pub async fn stop(&self) {
        self.shutdown().await;
        let connection_ids = self.sessions.connection_ids();
        for connection_id in connection_ids {
            let _ = self.sessions.stop_and_join(connection_id).await;
            self.sim_registry.remove(connection_id).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::init_db;
    use banto_collect::CollectorOptions;
    use banto_tags::{CollectionGroupInput, CollectionGroupService, PlcConnectionInput, TagInput};
    use banto_tstore::SystemClock;
    use std::time::Duration;

    /// `CollectorManager` needs a *file-backed* registry DB, same reasoning
    /// as `banto-collect`'s own integration tests (`build_config` and the
    /// per-connection tasks each hand out their own pool connection; a
    /// `:memory:` DB is a fresh empty database per connection).
    ///
    /// Returns `(dir, manager, pool)` - **dir first, pool last** - not the
    /// more natural-looking `(pool, dir, manager)`. See
    /// `crate::test_support`'s module doc for why: every call site
    /// destructures this directly with `let (_dir, manager, _pool) = ...`,
    /// and a tuple's bindings from one `let` pattern drop in *reverse* of
    /// how they're listed - so `pool`, listed last, drops *first*, before
    /// `dir`'s cleanup runs. Getting this order backwards silently leaks the
    /// temp dir on every run (measured before this fix).
    async fn manager_env() -> (
        crate::test_support::TempDir,
        CollectorManager,
        sqlx::SqlitePool,
    ) {
        let dir = crate::test_support::TempDir::new("manager-env");
        let db_path = dir.path().join("registry.sqlite3");
        let pool = init_db(&db_path).await.expect("init_db");
        let data_dir = dir.path().join("data");
        let sessions = Arc::new(HubSessions::new(banto_broker::BackoffConfig::default()));
        let sim_registry = Arc::new(SlmpSimRegistry::new());
        let computed = Arc::new(ComputedEngine::new(Arc::new(ServerTagStore::new())));
        let manager = CollectorManager::new(
            pool.clone(),
            data_dir,
            Arc::new(SystemClock),
            CollectorOptions {
                connect_timeout: Duration::from_millis(200),
                response_timeout: Duration::from_millis(200),
                ..CollectorOptions::default()
            },
            sessions,
            sim_registry,
            computed,
        );
        (dir, manager, pool)
    }

    // `crate::test_support`'s module doc: `TempDir::drop`'s retry needs a
    // multi-thread runtime.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rebuild_on_an_empty_registry_is_not_an_error() {
        let (_dir, manager, _pool) = manager_env().await;
        manager.rebuild().await.expect("empty rebuild should be Ok");
        assert_eq!(manager.revision(), 1);
        assert_eq!(manager.last_error(), None);
        assert!(manager.tag_map().is_empty());
        assert!(manager.current_values().is_none());
    }

    // `crate::test_support`'s module doc: `TempDir::drop`'s retry needs a
    // multi-thread runtime.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rebuild_builds_a_catalog_entry_with_effective_enabled_state() {
        let (_dir, manager, pool) = manager_env().await;

        let conn = PlcConnectionService::new(pool.clone())
            .create(PlcConnectionInput {
                name: "line1".to_string(),
                protocol: "modbus-tcp".to_string(),
                host: "127.0.0.1".to_string(),
                port: 15020,
                unit_id: 1,
                enabled: true,
                simulation: false,
            })
            .await
            .unwrap();
        let group = CollectionGroupService::new(pool.clone())
            .create(CollectionGroupInput {
                name: "fast".to_string(),
                plc_connection_id: conn.id,
                period_ms: 100,
                enabled: true,
            })
            .await
            .unwrap();
        let tag = TagService::new(pool.clone())
            .create(TagInput {
                name: "temp01".to_string(),
                collection_group_id: group.id,
                address: "40001".to_string(),
                data_type: "i16".to_string(),
                string_length: None,
                raw_lo: None,
                raw_hi: None,
                eng_lo: None,
                eng_hi: None,
                unit: None,
                decimals: 0,
                threshold_h: None,
                threshold_hh: None,
                threshold_l: None,
                threshold_ll: None,
                enabled: false,
                writable: false,
                tag_kind: "plc".to_string(),
                expression: None,
                retain: false,
            })
            .await
            .unwrap();

        // Nothing enabled to actually collect (the tag itself is disabled),
        // but the catalog must still show it (design §4: 欠測を隠さない).
        manager.rebuild().await.expect("rebuild should be Ok");
        let map = manager.tag_map();
        let entry = map.get("line1.fast.temp01").expect("catalog entry");
        assert_eq!(entry.ids, (conn.id, group.id, tag.id));
        assert_eq!(entry.address, "40001");
        assert!(!entry.enabled, "tag itself is disabled");

        // `rebuild()` above spawned a real (if unreachable) per-connection
        // collector task that would otherwise outlive this test and keep
        // its `EventSink` pool clone checked out - see
        // `crate::test_support`'s module doc for why that alone defeats
        // `TempDir::drop`'s retry regardless of ordering.
        manager.shutdown().await;
    }

    // `crate::test_support`'s module doc: `TempDir::drop`'s retry needs a
    // multi-thread runtime.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rebuild_keeps_the_old_state_on_a_config_error() {
        let (_dir, manager, pool) = manager_env().await;

        let conn = PlcConnectionService::new(pool.clone())
            .create(PlcConnectionInput {
                name: "line1".to_string(),
                protocol: "modbus-tcp".to_string(),
                host: "127.0.0.1".to_string(),
                port: 15021,
                unit_id: 1,
                enabled: true,
                simulation: false,
            })
            .await
            .unwrap();
        let group = CollectionGroupService::new(pool.clone())
            .create(CollectionGroupInput {
                name: "fast".to_string(),
                plc_connection_id: conn.id,
                period_ms: 100,
                enabled: true,
            })
            .await
            .unwrap();

        // First rebuild: still empty (no tags yet), succeeds and bumps
        // revision to 1.
        manager.rebuild().await.unwrap();
        assert_eq!(manager.revision(), 1);

        // A tag with an address `build_config` cannot parse under Modbus
        // rules ("99999" has an unknown area prefix, see
        // banto-collect::config's own `invalid_address_is_a_config_error`
        // test) - passes banto-tags' own (non-empty-only) validation but
        // fails at build_config time.
        TagService::new(pool.clone())
            .create(TagInput {
                name: "bad".to_string(),
                collection_group_id: group.id,
                address: "99999".to_string(),
                data_type: "i16".to_string(),
                string_length: None,
                raw_lo: None,
                raw_hi: None,
                eng_lo: None,
                eng_hi: None,
                unit: None,
                decimals: 0,
                threshold_h: None,
                threshold_hh: None,
                threshold_l: None,
                threshold_ll: None,
                enabled: true,
                writable: false,
                tag_kind: "plc".to_string(),
                expression: None,
                retain: false,
            })
            .await
            .unwrap();

        let err = manager
            .rebuild()
            .await
            .expect_err("bad address should fail rebuild");
        assert!(!err.is_empty());
        // Old state (empty catalog, revision 1) is untouched.
        assert_eq!(manager.revision(), 1);
        assert_eq!(manager.last_error(), Some(err));
        assert!(manager.tag_map().is_empty());

        // See the shutdown note in `rebuild_builds_a_catalog_entry_with_effective_enabled_state`
        // above - the first (successful, empty-registry) rebuild here never
        // spawns a task, but this test's whole point is the SECOND rebuild
        // attempt with a real connection, which does (and fails at
        // build_config time before ever reaching that task - but the FIRST
        // rebuild already committed the empty config with zero tasks, so
        // there is nothing to stop from that one either; shutdown is still
        // the harmless, correct thing to call unconditionally).
        manager.shutdown().await;
    }

    /// T9-2 フォローアップ (2026-08-06): `log_simulation_warnings`（この
    /// テストでは `rebuild` 経由で間接的に呼ばれる）が実際に注入された
    /// `DiagLog` を経由すること - 素の `println!` に黙って落ちないことの
    /// 回帰確認（`crate::diag_log` モジュール doc「PR #43 監査指摘」参照）。
    // `crate::test_support`'s module doc: `TempDir::drop`'s retry needs a
    // multi-thread runtime.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rebuild_routes_the_simulation_warning_through_the_injected_diag_log() {
        let (_dir, manager, pool) = manager_env().await;
        let lines: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured = lines.clone();
        let manager = manager.with_diag_log(DiagLog::new(
            move |msg| captured.lock().unwrap().push(msg.to_string()),
            |_msg| {},
        ));

        PlcConnectionService::new(pool.clone())
            .create(PlcConnectionInput {
                name: "simline".to_string(),
                protocol: "modbus-tcp".to_string(),
                host: "127.0.0.1".to_string(),
                port: 15023,
                unit_id: 1,
                enabled: true,
                simulation: true,
            })
            .await
            .unwrap();

        manager.rebuild().await.expect("rebuild should be Ok");

        {
            let captured_lines = lines.lock().unwrap();
            assert!(
                captured_lines
                    .iter()
                    .any(|line| line.contains("simline") && line.contains("シミュレーション")),
                "expected a simulation warning line, got: {captured_lines:?}"
            );
        }

        manager.shutdown().await;
    }

    /// Two concurrent `rebuild()` calls must not panic (no double-lock /
    /// re-entrancy deadlock on `rebuild_lock`) and must both actually run to
    /// completion (not silently coalesce) - `revision` ends up advanced by
    /// exactly 2 either way, since both calls see the same (empty) registry
    /// and each legitimately commits its own generation. This does not
    /// assert anything about *ordering* (not this fix's concern, per the
    /// review note) - only that serialization does not corrupt state or
    /// deadlock under concurrency.
    // `crate::test_support`'s module doc: `TempDir::drop`'s retry needs a
    // multi-thread runtime.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_rebuild_calls_are_serialized_and_both_succeed() {
        let (_dir, manager, _pool) = manager_env().await;
        let manager = Arc::new(manager);

        let a = manager.clone();
        let b = manager.clone();
        let (result_a, result_b) = tokio::join!(
            tokio::spawn(async move { a.rebuild().await }),
            tokio::spawn(async move { b.rebuild().await }),
        );
        result_a
            .expect("task a should not panic")
            .expect("rebuild a should succeed");
        result_b
            .expect("task b should not panic")
            .expect("rebuild b should succeed");

        assert_eq!(manager.revision(), 2);
        assert_eq!(manager.last_error(), None);
    }
}
