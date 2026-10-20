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
//! ## broker セッションの削除同期（T7-2、2026-08-05／#131 で SLMP 限定を解除）
//!
//! T2-2 は broker セッションの同期を `ensure_connection` のみ（追加専用）と
//! していた（`crate::broker_glue::HubSessions` のモジュール doc「Session
//! sync policy」参照）。T7-2 で `banto_broker::SessionDirectory::remove` が
//! 追加されたことを受け、[`CollectorManager::rebuild`] は毎回「追加 +
//! 削除」の完全同期を行う: レジストリの現在の有効な broker 管理対象プロト
//! コル接続集合（#131、2026-09-01: `HubSessions::is_broker_managed` -
//! 現状 slmp と modbus-tcp。以前は slmp のみだった）を
//! `ensure_connection`（従来どおり）した後、**Collector 側のコミットが
//! 成功した後で**（= 削除対象接続の collect タスクが既に停止済みである
//! ことが保証された後で）、`HubSessions` が保持している接続 ID のうち
//! この集合に無いものを `HubSessions::remove` する。この順序
//! （collect タスク停止 → broker セッション削除）が逆転すると、まだ
//! `read_batch` を呼んでいる `BrokerReadClient` の下でセッションが消える
//! 危険がある - 詳細は `crate::broker_glue` のモジュール doc参照。#131 当時
//! この対象拡大は「書き込み・ステータス用セッションのライフサイクル管理」
//! だけを意味し、Modbus の**収集読み取り**は broker 経由ではなかったが、
//! #337（2026-09-08）でその収集読み取りも broker セッションへ相乗りした
//! ため、現在は SLMP と Modbus TCP で扱いが完全に同一である
//! （`crate::broker_glue::hub_client_factory`の doc comment 参照）。
//!
//! ## 接続単位のシミュレーションモード（T9-2、2026-08-06/07。#337 で
//! Modbus TCP にも同じ扱いが及ぶ）
//!
//! `crate::broker_glue::BrokerSimRegistry`（`sim_registry` フィールド）は
//! `sessions`（`HubSessions`）と対の、`CollectorManager` の外で構築・生存する
//! `Arc` - broker 経由接続の `simulation = true` を実際に有効化する
//! T9-2 の実装本体（詳しくは `BrokerSimRegistry` 自身の doc comment、および
//! `crate::broker_glue` のモジュール doc「T9-1/T9-2 note」節を参照）。
//! [`CollectorManager::sync_broker_sessions_from`] は `ensure_connection` を
//! 呼ぶ前に接続ごとに `BrokerSimRegistry::resolve` を呼び、シミュレーション中
//! なら実際のダイヤル先をシミュレータの loopback アドレスへ差し替え、宛先が
//! 変わっていれば（`changed == true`）`HubSessions::remove` してから
//! `ensure_connection` して古いセッションの使い回しを防ぐ（#131、
//! 2026-09-01 以降この resolve/ensure_connection 自体は Modbus TCP 接続にも
//! 及ぶ - `sync_broker_sessions_from`のモジュール doc参照）。[`Self::rebuild`]
//! はさらに、`sync_broker_sessions_from`の第4戻り値（`read_routed_keys` =
//! *収集読み取り*が broker 経由になっている接続キーの集合）を
//! `banto_collect::CollectorConfig::suppress_simulation_for` に渡し、
//! `Collector` 自身が同じ接続に対して二重にシミュレータを起動しないようにする
//! （`crates/banto-collect/src/config.rs`の`suppress_simulation_for`の doc
//! comment参照）。#131 では、この集合を SLMP だけに絞ることが必須だった:
//! 当時 Modbus TCP 接続の*収集読み取り*は broker 経由にならず`Collector`自身の
//! `ModbusTcpClient`が使っていたため、`simulation = true`な Modbus 接続を
//! suppress 対象に含めると、読み取りだけがシミュレータ起動を抑制されて実機
//! （大抵は開発/テスト環境では到達不能）へダイヤルしてしまうからである。
//! **#337（2026-09-08）でその絞り込みは不要になった**: Modbus の収集読み取りも
//! broker セッション経由になり、`BrokerSimRegistry::resolve`が Modbus 用の
//! シミュレータを起動する（#131 以降）ので、`Collector`側のシミュレータを
//! 止めるのがむしろ正しい。現在 `read_routed_keys` は
//! `sync_broker_sessions_from` が `broker_handles` から導出する（二度と食い違わ
//! ないように、独立に積み上げない）- 同 fn の doc comment 参照。
//!
//! **ここまでだけでは実は不十分**（自前の E2E テスト
//! `apps/banto-hub/core/tests/t9_simulation.rs`で発覚）: `simulation`を
//! 常に`false`へ強制すると、broker 経由 SLMP 接続の`ConnectionPlan`は
//! トグル前後で構造的に同一と比較され、`Collector::apply_config`はそれを
//! 「unchanged」と分類してその接続の収集タスクに一切手を触れない -
//! ところがタスクが起動時に捕まえた`ClientFactory`（そのクロージャが閉じる
//! `ReadOnlyHandle`）はタスクの生存期間を通じて固定なので、rebuild の度に
//! 新しく組み立てる`hub_client_factory`はタスクへ届かない。結果、broker
//! セッションが`BrokerSimRegistry::resolve`の`changed`検出で実際には
//! 入れ替わっていても、動き続けている収集タスクは古い（既に directory
//! から外れた）セッションを黙って読み続けてしまう。そこで`Self::rebuild`は
//! `sync_broker_sessions_from`が返す解決済みダイヤル先を
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
//! 切れない。`rebuild` は毎回 [`CollectorManager::sync_broker_sessions_from`] で
//! レジストリの現在の broker 管理対象接続集合を `sessions` に
//! `ensure_connection`（新規なら起動・既存ならそのまま）し、得た
//! ハンドル群を `crate::broker_glue::hub_client_factory` に渡す - broker
//! 管理対象の接続（SLMP、および #337 以降は Modbus TCP）は
//! [`crate::broker_glue::BrokerReadClient`] で、その共有セッションを通して
//! 読む（セッションが張れなかった接続だけが防御的に直接クライアントへ
//! フォールバックする）。旧 T0〜T6 の全体再構築方式では「新旧 `Collector`
//! が同じ PLC へ同時にソケットを張る瞬間」を SLMP について解消する役目も
//! 兼ねていたが、T7 の部分適用移行後は新旧 `Collector` という概念自体が
//! 無くなったため、この節の主眼は「broker セッションの追加+削除の完全同期」
//! に移った - このモジュールの doc 冒頭「broker セッションの削除同期
//! （T7-2）」参照。

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::{broadcast, watch};

use banto_broker::{BrokerConnectionStatus, BrokerError, BrokerHandle, ReadOnlyHandle};
use banto_collect::{
    build_config_from, connections_with_collected_groups, ApplyReport, CollectEvent, Collector,
    CollectorOptions, ConnectionStatus, CurrentReading, CurrentSample, CurrentValuesHandle,
    EventSink, Quality, RegistrySnapshot,
};
use banto_core::ListParams;
use banto_tags::{CollectionGroupService, PlcConnection, PlcConnectionService, Tag, TagService};
use banto_tstore::Clock;
use serde::Serialize;
use sqlx::SqlitePool;
use utoipa::ToSchema;

use crate::broker_glue::{hub_client_factory, BrokerSimRegistry, HubSessions};
use crate::computed::{self, ComputedEngine, ServerTagStore};
use crate::db_source::{self, DbConnectionStatus, DbSourceEngine};
use crate::diag_log::DiagLog;

/// Build the non-persistent runtime snapshot for a collection mode.
///
/// The catalog must continue to describe the saved registry, while the
/// collector and broker session setup need the effective run-time flags. An
/// all-simulation run therefore overrides only enabled physical connections
/// in a cloned snapshot; the database-backed snapshot remains untouched.
fn runtime_snapshot_for_mode(
    snapshot: &RegistrySnapshot,
    mode: crate::controller::RunMode,
) -> RegistrySnapshot {
    if mode != crate::controller::RunMode::AllSimulation {
        return snapshot.clone();
    }

    let mut runtime = snapshot.clone();
    for connection in &mut runtime.connections {
        if connection.enabled && matches!(connection.protocol.as_str(), "modbus-tcp" | "slmp") {
            connection.simulation = true;
        }
    }
    runtime
}

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
    let view = effective_current_view(
        entry,
        sample.map(|sample| CurrentReading {
            sample,
            last_good_ms: None,
            quality_since_ms: None,
        }),
        now_ms,
    );
    (view.value, view.quality, view.timestamp_ms)
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
/// comment). Projects [`read_current_view`] onto the compatibility tuple,
/// so the `!entry.enabled` /
/// missing-sample rules stay defined in exactly one place regardless of
/// `tag_kind`.
pub fn read_current(
    entry: &TagEntry,
    collect: Option<&CurrentValuesHandle>,
    server_store: &ServerTagStore,
    now_ms: i64,
) -> (Option<f64>, Quality, i64) {
    let view = read_current_view(entry, collect, server_store, now_ms);
    (view.value, view.quality, view.timestamp_ms)
}

/// Current value plus source and quality-transition timestamps. This is an
/// internal view; existing interface tuples retain their attempt timestamp.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CurrentView {
    pub value: Option<f64>,
    pub quality: Quality,
    pub timestamp_ms: i64,
    /// Last finite Good sample time, retained across Bad/Stale; unknown for
    /// missing or disabled tags and before the first usable sample.
    pub source_time_ms: Option<i64>,
    /// Start of the current quality, or None for missing/disabled tags.
    pub quality_since_ms: Option<i64>,
}

/// Values accepted by every current-value interface, including computed inputs.
pub(crate) fn valid_current_value(entry: &TagEntry, value: f64) -> bool {
    value.is_finite() && (entry.data_type != "bit" || value == 0.0 || value == 1.0)
}

fn effective_current_view(
    entry: &TagEntry,
    reading: Option<CurrentReading>,
    now_ms: i64,
) -> CurrentView {
    let timestamp_ms = reading.map_or(now_ms, |r| r.sample.ptime_ms);
    if !entry.enabled || reading.is_none() {
        return CurrentView {
            value: None,
            quality: Quality::Bad,
            timestamp_ms,
            source_time_ms: None,
            quality_since_ms: None,
        };
    }
    let reading = reading.expect("present reading checked above");
    if !reading
        .sample
        .value
        .is_some_and(|value| valid_current_value(entry, value))
    {
        // A finite invalid bit can be stored as Good by the untyped cache
        // and later derive Stale. Neither event makes that value usable or
        // starts a new effective Bad interval.
        let invalid_bit = reading.sample.value.is_some_and(f64::is_finite);
        return CurrentView {
            value: None,
            quality: Quality::Bad,
            timestamp_ms,
            // The raw store may have recorded an invalid bit as Good. It
            // cannot provide a usable source time for that update.
            source_time_ms: if reading.sample.quality != Quality::Bad
                && reading.sample.value.is_some()
            {
                reading.last_good_ms.filter(|time| *time != timestamp_ms)
            } else {
                reading.last_good_ms
            },
            quality_since_ms: Some(reading.quality_since_ms.map_or(timestamp_ms, |since| {
                if invalid_bit && reading.sample.quality != Quality::Bad {
                    timestamp_ms
                } else {
                    since
                }
            })),
        };
    }
    CurrentView {
        value: reading.sample.value,
        quality: reading.sample.quality,
        timestamp_ms,
        source_time_ms: reading.last_good_ms,
        quality_since_ms: reading.quality_since_ms,
    }
}

/// Common timestamp-aware read path for PLC, computed, internal and DB tags.
pub fn read_current_view(
    entry: &TagEntry,
    collect: Option<&CurrentValuesHandle>,
    server_store: &ServerTagStore,
    now_ms: i64,
) -> CurrentView {
    let reading = if entry.tag_kind == banto_tags::PLC_TAG_KIND {
        collect.and_then(|c| c.get_reading(&entry.tag_key))
    } else {
        server_store.get_reading(&entry.tag_key)
    };
    let mut view = effective_current_view(entry, reading, now_ms);
    view.quality_since_ms =
        server_store.effective_quality_since(&entry.tag_key, view.quality, view.quality_since_ms);
    view
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
    /// `banto_collect::simulation`/`crate::broker_glue::BrokerSimRegistry`),
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
    /// group, tag の名前順)"). Each external name appears **once**.
    ///
    /// `plc_connections.name` is globally `UNIQUE`,
    /// `collection_groups.name` is `UNIQUE(plc_connection_id, name)`
    /// (migration 0022, 2026-10-19) and `tags.name` is
    /// `UNIQUE(collection_group_id, name)` (migration 0011), so the triple
    /// `(connection, group, tag)` is unique; but names may contain `.`, so two different
    /// `(connection, group, tag)` triples can still join to the same
    /// external name (`a.b`/`c`/`d` and `a`/`b.c`/`d` are both `a.b.c.d`,
    /// #635). Since 2026-10-17 the registry writes reject a write that
    /// creates such a collision (`crate::rest::preflight_transaction`), but
    /// rows written before that may still collide: the last entry in
    /// catalog order wins the name (`by_external`), the others are listed in
    /// `collisions` and reported as warning lines when the catalog is
    /// installed ([`warn_external_name_collisions`]).
    ordered: Vec<String>,
    /// External names shared by more than one tag (#635), in catalog order.
    collisions: Vec<ExternalNameCollision>,
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

    /// External names shared by more than one tag (#635). Only the last tag
    /// of each collision (catalog order) is reachable by its external name.
    pub fn collisions(&self) -> &[ExternalNameCollision] {
        &self.collisions
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

/// The external name of one tag: `{connection}.{group}.{tag}` (design §4.1).
/// The **only** place that joins the three names - the catalog and the
/// collision check (#635) both go through [`catalog_entries`], which calls
/// this, so the two can never disagree on how a name is assembled.
pub fn external_name(connection: &str, group: &str, tag: &str) -> String {
    format!("{connection}.{group}.{tag}")
}

/// One tag that shares its external name with another tag (#635).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollidingTag {
    /// `(connection id, group id, tag id)`, as [`TagEntry::ids`].
    pub ids: (i64, i64, i64),
    pub connection: String,
    pub group: String,
    pub name: String,
}

impl CollidingTag {
    fn describe(&self) -> String {
        format!(
            "接続「{}」/ グループ「{}」/ タグ「{}」（タグ ID {}）",
            self.connection, self.group, self.name, self.ids.2
        )
    }
}

/// An external name shared by two or more tags (#635), with the tags in
/// catalog order. The catalog's lookup resolves the name to the **last** one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalNameCollision {
    pub external_name: String,
    pub tags: Vec<CollidingTag>,
}

impl ExternalNameCollision {
    /// The warning line for an existing collision (the Hub keeps running -
    /// 2026-10-17 オーナー決定): which tags can no longer be reached by the
    /// external name, and which one the name resolves to.
    pub fn warning_line(&self) -> String {
        let (winner, hidden) = self
            .tags
            .split_last()
            .expect("a collision has at least two tags");
        let hidden = hidden
            .iter()
            .map(CollidingTag::describe)
            .collect::<Vec<_>>()
            .join("、");
        format!(
            "[WARN] banto-hub: 外部名「{}」が複数のタグで重なっています（#635）。{hidden} は外部名で引けません（REST・WebSocket・MQTT・gRPC・OPC UA）。外部名は {} を指します。接続・グループ・タグのどれかの名前を変えてください",
            self.external_name,
            winner.describe()
        )
    }
}

/// Every catalog entry of `snapshot`, sorted in catalog order - the one
/// assembly shared by [`build_catalog_from`] and
/// [`external_name_collisions`].
fn catalog_entries(snapshot: &RegistrySnapshot) -> Vec<TagEntry> {
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
            external_name: external_name(&conn.name, &group.name, &tag.name),
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
    entries
}

/// Group sorted entries by external name and keep the shared ones.
fn collisions_in(entries: &[TagEntry]) -> Vec<ExternalNameCollision> {
    let mut by_name: HashMap<&str, Vec<&TagEntry>> = HashMap::new();
    let mut order: Vec<&str> = Vec::new();
    for entry in entries {
        let members = by_name.entry(entry.external_name.as_str()).or_default();
        if members.is_empty() {
            order.push(entry.external_name.as_str());
        }
        members.push(entry);
    }
    order
        .into_iter()
        .filter_map(|name| {
            let members = &by_name[name];
            (members.len() > 1).then(|| ExternalNameCollision {
                external_name: name.to_owned(),
                tags: members
                    .iter()
                    .map(|entry| CollidingTag {
                        ids: entry.ids,
                        connection: entry.connection.clone(),
                        group: entry.group.clone(),
                        name: entry.name.clone(),
                    })
                    .collect(),
            })
        })
        .collect()
}

/// External names shared by more than one tag in `snapshot` (#635). Pure;
/// assembles names exactly as [`build_catalog_from`] does.
pub fn external_name_collisions(snapshot: &RegistrySnapshot) -> Vec<ExternalNameCollision> {
    collisions_in(&catalog_entries(snapshot))
}

/// A collision that a registry write would **create** (#635): `introduced`
/// are the tags the write created or renamed (their external name differs
/// from `before`, or they did not exist), `existing` are the tags that
/// already had this external name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewExternalNameCollision {
    pub external_name: String,
    pub introduced: Vec<CollidingTag>,
    pub existing: Vec<CollidingTag>,
}

impl NewExternalNameCollision {
    /// The validation message: which existing tag the name collides with.
    pub fn message(&self) -> String {
        let list = |tags: &[CollidingTag]| {
            tags.iter()
                .map(CollidingTag::describe)
                .collect::<Vec<_>>()
                .join("、")
        };
        if self.existing.is_empty() {
            format!(
                "外部名「{}」が、この変更で作る・変えるタグどうし（{}）で重なります。接続・グループ・タグの名前をドットでつないだ外部名が同じだと、外部から区別できません。名前を変えてください",
                self.external_name,
                list(&self.introduced)
            )
        } else {
            format!(
                "外部名「{}」が既存のタグ（{}）と重なります。接続・グループ・タグの名前をドットでつないだ外部名が同じだと、外部から区別できません。名前を変えてください",
                self.external_name,
                list(&self.existing)
            )
        }
    }
}

/// The collisions a registry write creates, comparing the registry before
/// and after the write (#635, 2026-10-17 オーナー決定 案 A). A collision
/// counts as new when it pairs two tags that did not already share that
/// external name before the write; collisions that already existed (rows
/// written before this check) are left alone so that unrelated edits and
/// the edits that resolve them still go through. Pure.
pub fn new_external_name_collisions(
    before: &RegistrySnapshot,
    after: &RegistrySnapshot,
) -> Vec<NewExternalNameCollision> {
    let before_names: HashMap<i64, String> = catalog_entries(before)
        .into_iter()
        .map(|entry| (entry.ids.2, entry.external_name))
        .collect();
    external_name_collisions(after)
        .into_iter()
        .filter_map(|collision| {
            let (existing, introduced): (Vec<_>, Vec<_>) = collision
                .tags
                .into_iter()
                .partition(|tag| before_names.get(&tag.ids.2) == Some(&collision.external_name));
            (!introduced.is_empty()).then_some(NewExternalNameCollision {
                external_name: collision.external_name,
                introduced,
                existing,
            })
        })
        .collect()
}

/// #635: report the collisions of a catalog being installed as warning
/// lines (the Hub keeps running - 2026-10-17 オーナー決定). A collision
/// already present in the catalog being replaced is not repeated.
fn warn_external_name_collisions(old: &TagMap, new: &TagMap) {
    for collision in &new.collisions {
        if !old.collisions.contains(collision) {
            crate::hub_log::log_err_line(&collision.warning_line());
        }
    }
}

/// Build the external-name catalog from one registry snapshot. This is kept
/// separate from the pool-reading compatibility wrapper so a catalog commit
/// and a collector preflight can use exactly the same logical registry read.
pub fn build_catalog_from(snapshot: &RegistrySnapshot) -> Result<TagMap, banto_core::BantoError> {
    let entries = catalog_entries(snapshot);
    let collisions = collisions_in(&entries);

    let mut by_external = HashMap::with_capacity(entries.len());
    let mut ordered = Vec::with_capacity(entries.len());
    for entry in entries {
        // #635: a shared external name is listed once (at its first
        // position); the lookup keeps the last entry, as before.
        let name = entry.external_name.clone();
        if by_external.insert(name.clone(), entry).is_none() {
            ordered.push(name);
        }
    }

    Ok(TagMap {
        by_external,
        ordered,
        collisions,
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
    /// #341 レビュー対応（2026-09-14）: **直近で実際に適用に成功した
    /// 「runtime snapshot」**（= run mode のオーバーライド適用後。
    /// [`runtime_snapshot_for_mode`] の出力そのもの）。
    /// [`CollectorManager::apply_run`] が collector 側のコミットに失敗した
    /// ときの broker セッション集合のロールバック基準として**だけ**使う
    /// （同 fn の「失敗時のロールバック」段落参照）。まだ一度も適用に
    /// 成功していない場合と [`CollectorManager::stop`] 直後は
    /// `RegistrySnapshot::default()`（空 = セッションを1本も張らない状態）。
    applied_runtime: RegistrySnapshot,
    /// #341 レビュー対応2（2026-09-14）: `applied_runtime` の**生の**
    /// （run mode を適用する前の）相方。catalog・演算 plan・DB Source plan
    /// のロールバック基準で、[`CollectorManager::rollback_catalog`] が
    /// [`CollectorManager::commit_catalog`] へ渡す。
    ///
    /// `applied_runtime` と分けているのは、`commit_catalog` が受け取るのが
    /// レジストリそのもの（run mode 非依存の catalog 導出）だからで、
    /// 実際には catalog は run mode に依存しない（`simulation` の実効値は
    /// 応答時に controller の mode から導出され、`TagMap` には焼き込まれ
    /// ない）ため `applied_runtime` でも同じ結果になる。それでも「catalog
    /// には生の registry を渡す」という `commit_catalog` の前提をコードの
    /// 形で保つために別に持つ。更新点・クリア点は `applied_runtime` と同じ。
    applied_registry: RegistrySnapshot,
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
    /// #341 レビュー対応（2026-09-14）: **テスト専用の失敗注入**。
    /// `true` のとき、次の [`Self::apply_run`] は collector 側のコミットを
    /// 実際には行わずに `Err` を返す（フラグはその場で降りる）。
    /// `crate::rest` の endpoint 越しのテスト
    /// （`pending_apply_live_reconfigure_failure_is_500_and_marks_applied`）
    /// が「実行構成への反映だけが失敗した」状態を決定的に作るために要る -
    /// 本物の失敗要因（tstore の writer を開けない）を外から再現するのは
    /// OS 依存が強すぎる。`#[cfg(test)]` なのでライブラリのユニットテスト
    /// からしか触れず、`cargo build`（統合テスト・本番）には一切残らない。
    #[cfg(test)]
    fail_next_apply: std::sync::atomic::AtomicBool,
    /// テスト専用（[`Self::fail_next_apply`] と同じ流儀）: 次の
    /// [`Self::apply_run`] をこのミリ秒だけ遅らせる。0 なら遅らせない。
    #[cfg(test)]
    delay_next_apply: std::sync::atomic::AtomicU64,
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
    /// collector-internal event flow (`plc_connected`/`plc_disconnected`/etc.)
    /// still needs a stable sink to persist to. #533: no threshold edges -
    /// [`build_config_from`] gives no tag any limits, so the hub never
    /// judges a threshold.)
    events: EventSink,
    /// T2-2 (docs/tag-server-design.md §6-5): the broker session directory,
    /// owned OUTSIDE this manager (`bin/banto-hub.rs` constructs it and holds
    /// its own `Arc` clone for final shutdown) so a broker session (SLMP
    /// since T2-2, Modbus TCP since #131/#337) survives a
    /// `rebuild` - see `crate::broker_glue::HubSessions`'s doc comment for
    /// the full rationale and the session-sync policy `rebuild` follows.
    sessions: Arc<HubSessions>,
    /// T9-2 (docs/ux-plan.md §1): the broker session simulator registry
    /// (`BrokerSimRegistry`, formerly `SlmpSimRegistry` - see that type's own
    /// doc comment for why the old name said SLMP only even though it has
    /// driven Modbus TCP too since #131, and why #339 renamed it),
    /// owned OUTSIDE this manager for exactly the same reason `sessions` is - it must
    /// survive every `rebuild` (a simulator started for a connection stays
    /// up across rebuilds that leave it `simulation = true`, mirroring how a
    /// broker session stays up), and `bin/banto_hub` needs its own `Arc`
    /// clone to call `BrokerSimRegistry::shutdown` at the correct point in
    /// process shutdown (after `sessions.shutdown()` - simulators must
    /// outlive the broker sessions that dial them). See
    /// `crate::broker_glue::BrokerSimRegistry`'s doc comment for the full
    /// mechanism, and this module's doc comment ("接続単位の
    /// シミュレーションモード") for how `rebuild` uses it.
    sim_registry: Arc<BrokerSimRegistry>,
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
    /// 外部 DB 連携 S2（docs/banto-hub-external-db-design.md §4.2・§4.5）:
    /// DB Source のポーリングエンジン。`computed` と**全く同じ扱い**で
    /// [`Self::rebuild`]/[`Self::commit_catalog`] が
    /// `crate::db_source::build_plan`（純関数）の結果を catalog/`Collector`/
    /// 演算 plan の入れ替えと同じ all-or-nothing の1ステップで commit する
    /// （§4.5「Source の task は `commit_catalog` の完了を受けて配下の
    /// グループ集合を再読込する」）。
    ///
    /// **S2b（§4.8・§6-16、2026-09-06 オーナー決定）**: この manager は
    /// 引き続き**計画の commit だけ**を担い、タスクの起動・停止は
    /// `crate::controller::CollectionController` が収集の Running 遷移に
    /// 合わせて行う（[`Self::db_source_engine`] 経由。理由はあちらの
    /// `db_source` フィールド doc comment）。停止中の commit は「計画を
    /// 覚えるだけ」になるので、ここのコードは S2 から一切変わらない。
    ///
    /// `computed` と違い**この manager の内部で構築する**（`Self::new` の
    /// 引数に足していない）: 演算エンジンは `crate::runtime` が
    /// `ServerTagStore` の retain 復元と250ms 評価ループのために外から
    /// 所有する必要があるが、DB Source はタスクを自前で持つので、外の
    /// 所有者が要るのは「収集の開始/停止とシャットダウンで起こす・止める」
    /// ためだけであり、それは [`Self::db_source_engine`] で足りる。既存の26箇所の
    /// `CollectorManager::new` 呼び出し（テスト含む）に引数を足さずに済む
    /// という実利もある。ストアは `computed.server_store()` を共有する -
    /// `db` タグの値は `computed`/`internal` と同じ `ServerTagStore` へ書く
    /// （設計 §3-3: `read_current` は `tag_kind != "plc"` を全部そこから
    /// 読むので、新しい種別を足しても読み出し側は無改造）。
    db_source: Arc<DbSourceEngine>,
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
    /// 保持期間の削除（[`CollectorManager::prune_data_files`]）と、収集の
    /// 停止（`take()` して `Collector::stop`）を直列にするゲート（#541）。
    /// 削除が書き込み側（`TsWriter`）の強い参照を持っている間に停止が
    /// `TsWriter::close` へ進むと、共有扱いでプールが閉じられない。逆に停止の
    /// 最中（`collector` は既に `None`）に書き手なしの削除が走ると、最終 flush
    /// が書くファイルを消しうる。ロック順序は `rebuild_lock` → `writer_gate` →
    /// `collector`（`apply_config` 側は `writer_gate` を取らない）。
    writer_gate: AsyncMutex<()>,
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
    /// 参照）: `rebuild`/`sync_broker_sessions_from`/`log_simulation_warnings` の
    /// 診断ログの出力先。[`Self::new`] では [`DiagLog::default`]（素の
    /// `println!`/`eprintln!` と同じ）で初期化され、[`Self::with_diag_log`]
    /// を呼んだ場合のみ差し替わる - `bin/banto_hub` はここへ
    /// `hub_log::log_line`/`log_err_line` を配線し、Windows サービスモード
    /// でもこれらの診断がサービスログファイルに届くようにする。
    diag_log: DiagLog,
    /// #616: 最後に出したシミュレーションの注意の文面と時刻（`clock` のミリ秒）。設定の
    /// 1 回の変更で `rebuild`・`apply_run` が続けて呼ばれ、同じ注意が同じ時刻に 2〜4 回
    /// 出ていたのを、[`sim_warning_due`] で抑える。シミュレーションの接続が無くなったら
    /// `None` に戻す（再び有効にしたときに出す）。
    last_sim_warning: std::sync::Mutex<Option<(String, i64)>>,
}

/// #616: 同じ文面のシミュレーションの注意を、続けて出さない間隔（10 分）。永久には抑えない
/// （この間隔を過ぎた後の設定の適用では、もう一度出す）。
pub const SIM_WARNING_REPEAT_MS: i64 = 10 * 60 * 1000;

/// #616: シミュレーションの注意を今出すか（純関数）。出すのは、前に出していない（起動直後・
/// シミュレーションの接続が無くなった後に再び有効にした）、文面が変わった（接続が増えた・
/// 減った・名前が変わった）、同じ文面でも前に出してから [`SIM_WARNING_REPEAT_MS`] 以上経った、
/// のどれか。時計が戻ったとき（`now_ms` が前より小さい）も出す。
pub fn sim_warning_due(last: Option<&(String, i64)>, text: &str, now_ms: i64) -> bool {
    match last {
        None => true,
        Some((last_text, at_ms)) => {
            last_text != text
                || now_ms < *at_ms
                || now_ms.saturating_sub(*at_ms) >= SIM_WARNING_REPEAT_MS
        }
    }
}

/// T15-2 (docs/banto-hub-desktop-plan.md §9.7): 1タグがシミュレータの値生成
/// ウィンドウから外れている理由 - [`CollectorManager::simulation_coverage_report`]
/// の`unsupported`リスト1件分。`reason`は
/// `banto_collect::simulation::classify_plc_tag`の日本語メッセージそのもの。
///
/// JSON は camelCase(この manager が組み立てる他の admin-UI 向けレスポンスと
/// 同じ流儀 - `crate::rest`の`CollectionStatusResponse`/`MqttSettingsResponse`
/// 等参照。`/api/v1/*`の snake_case とは別系統)。
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SimulationCoverageEntry {
    pub name: String,
    pub connection: String,
    pub group: String,
    pub address: String,
    pub data_type: String,
    pub reason: String,
}

/// T15-2: all-simulation 開始前のプリフライト結果 - プラン §9.7 のモックアップ
/// 「対応: N タグ / 未対応: M タグ」をそのまま裏付ける集計。
/// [`CollectorManager::simulation_coverage_report`]が構築する。
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SimulationCoverageReport {
    pub supported_count: usize,
    pub unsupported_count: usize,
    /// カタログ順(connection, group, tag の名前順 - [`TagMap`]と同じ安定順)。
    pub unsupported: Vec<SimulationCoverageEntry>,
}

/// [`CollectorManager::simulation_coverage_report`]の純粋な計算本体 - レジス
/// トリの読み取りとロジックを分けておくと(`build_catalog`/`build_catalog_from`
/// と同じ分割方針)、`apps/banto-hub/core/tests/t15_simulation_coverage.rs`が
/// 実 DB 無しの`RegistrySnapshot`だけでロジックを直接テストできる。
///
/// all-simulation は「現在 `simulation` フラグが立っている接続」だけでなく
/// **有効な物理接続(protocol が `modbus-tcp`/`slmp`)全て**を強制的に
/// シミュレーション対象にする([`runtime_snapshot_for_mode`]参照) - この
/// プリフライトも同じ集合(有効な接続・有効なグループ・有効なタグ、かつ
/// `tag_kind == "plc"`)を対象にする。computed/internal タグは PLC を読まない
/// ので対象外(このモジュールの`TagEntry::tag_kind`の doc comment参照)。
fn simulation_coverage_report_from(snapshot: &RegistrySnapshot) -> SimulationCoverageReport {
    let conn_by_id: HashMap<i64, &PlcConnection> =
        snapshot.connections.iter().map(|c| (c.id, c)).collect();
    let group_by_id: HashMap<i64, &banto_tags::CollectionGroup> =
        snapshot.groups.iter().map(|g| (g.id, g)).collect();

    let mut rows: Vec<(&str, &str, &Tag, &str)> = Vec::new();
    for tag in &snapshot.tags {
        if !tag.enabled || tag.tag_kind != banto_tags::PLC_TAG_KIND {
            continue;
        }
        let Some(group) = group_by_id.get(&tag.collection_group_id) else {
            continue;
        };
        if !group.enabled {
            continue;
        }
        let Some(conn) = conn_by_id.get(&group.plc_connection_id) else {
            continue;
        };
        if !conn.enabled || !matches!(conn.protocol.as_str(), "modbus-tcp" | "slmp") {
            continue;
        }
        rows.push((
            conn.name.as_str(),
            group.name.as_str(),
            tag,
            conn.protocol.as_str(),
        ));
    }
    rows.sort_by(|a, b| (a.0, a.1, &a.2.name).cmp(&(b.0, b.1, &b.2.name)));

    let mut supported_count = 0usize;
    let mut unsupported = Vec::new();
    for (connection, group, tag, protocol) in rows {
        match banto_collect::simulation::classify_plc_tag(protocol, &tag.address, &tag.data_type) {
            banto_collect::simulation::SimulationCoverage::Supported => supported_count += 1,
            banto_collect::simulation::SimulationCoverage::Unsupported { reason } => {
                unsupported.push(SimulationCoverageEntry {
                    name: tag.name.clone(),
                    connection: connection.to_string(),
                    group: group.to_string(),
                    address: tag.address.clone(),
                    data_type: tag.data_type.clone(),
                    reason,
                });
            }
        }
    }

    SimulationCoverageReport {
        supported_count,
        unsupported_count: unsupported.len(),
        unsupported,
    }
}

impl CollectorManager {
    /// `clock` is shared with the store (rotation) and the current-value
    /// cache (staleness) - pass `Arc::new(SystemClock)` in production, a
    /// `ManualClock` in tests, same contract as `Collector::start`. `sessions`
    /// is the broker session directory (T2-2, §6-5) - constructed and owned
    /// by the caller (`bin/banto-hub.rs`) so it outlives every
    /// `CollectorManager::rebuild`; see [`CollectorManager`]'s `sessions`
    /// field doc comment. `sim_registry` (T9-2) is the broker simulator
    /// registry, constructed and owned the same way - see
    /// [`CollectorManager`]'s `sim_registry` field doc comment.
    pub fn new(
        pool: SqlitePool,
        data_dir: PathBuf,
        clock: Arc<dyn Clock>,
        options: CollectorOptions,
        sessions: Arc<HubSessions>,
        sim_registry: Arc<BrokerSimRegistry>,
        computed: Arc<ComputedEngine>,
    ) -> Self {
        let events = EventSink::new(pool.clone());
        let (revision_tx, _revision_rx) = watch::channel(0);
        let db_source = Arc::new(DbSourceEngine::new(computed.server_store(), clock.clone()));
        Self {
            #[cfg(test)]
            fail_next_apply: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            delay_next_apply: std::sync::atomic::AtomicU64::new(0),
            pool,
            data_dir,
            clock,
            options,
            events,
            sessions,
            sim_registry,
            computed,
            db_source,
            collector: AsyncMutex::new(None),
            inner: Mutex::new(Inner {
                map: Arc::new(TagMap::empty()),
                revision: 0,
                running_revision: 0,
                last_error: None,
                current: None,
                last_apply: None,
                applied_runtime: RegistrySnapshot::default(),
                applied_registry: RegistrySnapshot::default(),
            }),
            rebuild_lock: AsyncMutex::new(()),
            writer_gate: AsyncMutex::new(()),
            revision_tx,
            diag_log: DiagLog::default(),
            last_sim_warning: std::sync::Mutex::new(None),
        }
    }

    /// T9-2 フォローアップ (2026-08-06): この manager の診断ログ
    /// （`rebuild`/`sync_broker_sessions_from`/`log_simulation_warnings`）を
    /// `hub_log::log_line`/`log_err_line` 経由にルーティングし、Windows
    /// サービスモードでもサービスログファイルへ届くようにする - `bin/
    /// banto_hub` が呼ぶ。これを呼ばなければ既定の素の
    /// `println!`/`eprintln!` 相当のまま（`crate::diag_log` モジュール doc
    /// 参照）。
    pub fn with_diag_log(mut self, diag_log: DiagLog) -> Self {
        // 外部 DB 連携 S2: DB Source のタスクも同じ宛先へ流す（`db_source`
        // フィールドの doc comment 参照 - Windows サービスモードで warn が
        // サービスログファイルに残るのは、この配線があってこそ）。
        self.db_source.set_diag_log(diag_log.clone());
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

    /// 外部 DB 連携 S2: DB Source エンジン - `crate::runtime::RunningHub`
    /// がシャットダウン時に `DbSourceEngine::shutdown` を呼ぶために、
    /// また S2b（§4.8・§6-16）以降は
    /// `crate::controller::CollectionController` が収集の Running 遷移で
    /// `start`/`stop` を呼ぶために取る（`db_source` フィールドの doc
    /// comment 参照）。
    pub fn db_source_engine(&self) -> Arc<DbSourceEngine> {
        self.db_source.clone()
    }

    /// 外部 DB 連携 S2: `GET /api/v1/status`・`GET /api/status` の
    /// `db_source`/`dbSource` 節が読む運転状態（`crate::db_source::status`）。
    pub fn db_source_status(&self) -> Vec<DbConnectionStatus> {
        self.db_source.status_snapshot()
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

    /// The **effective** tstore data directory - the same `PathBuf` this
    /// manager was constructed with, i.e. `crate::runtime::HubRuntime::start`'s
    /// `data_dir` after `data_dir_override`/`BANTO_HUB_DATA` has already been
    /// applied over the persisted `store_config().data_dir`. T19 S2-d
    /// （UX-39）の `POST /api/store-settings/prune-now`/`prune-preview` は
    /// このアクセサ経由で「実際に書き込み・削除が起きるディレクトリ」を
    /// 得る - `SettingsService::store_config().data_dir` を直接読むと
    /// override 環境で実ディレクトリとズレる可能性があるため使わない
    /// （`crate::runtime::HubRuntime::start`のdoc comment「PORT/BANTO_BIND/
    /// BANTO_HUB_DATA（via `*_override`）override the persisted settings」
    /// 参照）。
    pub fn data_dir(&self) -> &std::path::Path {
        &self.data_dir
    }

    /// 保持期間を過ぎたデータファイルを消す（定期の剪定と
    /// `POST /api/store-settings/prune-now` の共通の口、#541）。
    /// 収集が走っていれば、**書き込み側（`TsWriter`）のロックの下で**消す
    /// （[`banto_tstore::TsWriter::prune_files`]。ファイルの切り替えに失敗して
    /// 古い日付のまま開いているファイルは、保持日数を過ぎていても消さない）。
    /// 走っていなければ書き手が無いので、そのまま [`banto_tstore::prune_files`]。
    ///
    /// `apply_config` が書き手を入れ替えうるので、書き手は**毎回**
    /// `Collector::writer_handle` から読み直す。`collector` のロックは削除の
    /// 間ずっと持ち（入れ替えと直列）、`writer_gate` で停止とも直列にする。
    /// 取った強い参照は削除が終わったら（ゲートを放す前に）手放す - 停止の
    /// `Collector::stop` は「自分だけが持つ `Arc`」のときだけ接続プールを
    /// 閉じるため、参照を残すとファイルが掴まれたままになる。
    pub async fn prune_data_files(
        &self,
        retention_days: u32,
        today: banto_tstore::LocalDate,
    ) -> Result<banto_tstore::PruneReport, banto_tstore::TstoreError> {
        let _gate = self.writer_gate.lock().await;
        let guard = self.collector.lock().await;
        if let Some(collector) = guard.as_ref() {
            let writer = collector.writer_handle().borrow().clone();
            let result = writer.prune_files(retention_days, today).await;
            drop(writer);
            return result;
        }
        drop(guard);
        let dir = self.data_dir.clone();
        match tokio::task::spawn_blocking(move || {
            banto_tstore::prune_files(&dir, retention_days, today)
        })
        .await
        {
            Ok(result) => result,
            Err(err) => Err(banto_tstore::TstoreError::Storage(format!(
                "削除の処理が異常終了しました: {err}"
            ))),
        }
    }

    /// [`Self::prune_data_files`] の予行（何も消さない）。収集中は書き込み側の
    /// 「開いている日付は消さない」判定を含めて数えるので、設定画面の
    /// プレビューが約束した件数と、実際に消える件数がずれない（#541）。
    /// 走っていなければ素の [`banto_tstore::plan_prune`]。ロックの取り方は
    /// `prune_data_files` と同じ。
    pub async fn plan_prune_data_files(
        &self,
        retention_days: u32,
        today: banto_tstore::LocalDate,
    ) -> Result<banto_tstore::PruneReport, banto_tstore::TstoreError> {
        let _gate = self.writer_gate.lock().await;
        let guard = self.collector.lock().await;
        if let Some(collector) = guard.as_ref() {
            let writer = collector.writer_handle().borrow().clone();
            let result = writer.plan_prune(retention_days, today).await;
            drop(writer);
            return result;
        }
        drop(guard);
        let dir = self.data_dir.clone();
        match tokio::task::spawn_blocking(move || {
            banto_tstore::plan_prune(&dir, retention_days, today)
        })
        .await
        {
            Ok(result) => result,
            Err(err) => Err(banto_tstore::TstoreError::Storage(format!(
                "削除予定の確認が異常終了しました: {err}"
            ))),
        }
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

        // 外部 DB 連携 S2 (docs/banto-hub-external-db-design.md §4.2・§4.5):
        // `computed_plan` と全く同じ扱い - 純粋な組み立てだけをここで行い、
        // commit は catalog/`Collector` の入れ替えと同じ点で行う。DB Source
        // の計画が不整合なら rebuild 全体が失敗し、走行中のタスクは
        // 一切触られない（§4.3(a) の all-or-nothing）。
        let db_source_plan = match db_source::build_plan(&snapshot) {
            Ok(plan) => plan,
            Err(err) => {
                let message = format!("DB Source の検証に失敗しました: {err}");
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

        // T2-2/T7-2/#131 (docs/tag-server-design.md §6-5/§4.3): additive
        // ensure_connection (unchanged from T2-2, now covers every
        // broker-managed protocol - `HubSessions::is_broker_managed`, not
        // just SLMP) plus the set of tracked ids that are no longer
        // wanted - see `Self::sync_broker_sessions_from`'s doc comment.
        // Deliberately unconditional (runs even when `config.group_count()
        // == 0` just below) - independent of whether the resulting
        // collector config ends up empty, every OTHER still-collectible
        // connection's broker session bookkeeping (add/remove) must still
        // happen this rebuild, and this step never touches
        // `inner`/`collector` so it carries no all-or-nothing risk either
        // way. **Since T19 S2-a (UX-48)**, a connection with zero enabled
        // collection groups no longer gets a session synced here at all -
        // see `Self::sync_broker_sessions_from`'s own doc comment (this
        // paragraph used to claim the opposite - "a connection can be
        // enabled with no collectible groups yet and still deserve a live
        // broker session" - which stopped being true the moment that fn
        // started filtering on `banto_collect::connections_with_collected_groups`).
        // `CollectionController::commit_catalog_and_apply_live`'s doc
        // comment (#341, T19 S2-a 案B の後継) covers how such a connection
        // can still get a session before the next start/stop cycle, via a
        // registry change committed while a run is already `Running` -
        // that path now goes all the way through `Self::apply_run`, so this
        // very fn is what runs. `stale_broker_ids` is only
        // actually removed AFTER a successful commit below - see
        // `Self::remove_stale_broker_sessions`'s doc comment for why the
        // ordering matters.
        let (broker_handles, stale_broker_ids, resolved_broker_targets, read_routed_keys) =
            self.sync_broker_sessions_from(&snapshot).await;

        // T9-2/#131/#337: `read_routed_keys` (the fourth return value) is
        // the set of connections whose *collection reads* are broker-routed,
        // whose dial target `Self::sync_broker_sessions_from` already resolved
        // (simulator substitution included, via `BrokerSimRegistry::resolve`)
        // before `ensure_connection` ran. Telling `Collector` to treat these
        // as `simulation = false` stops it from starting a second, redundant
        // in-process simulator for a connection `BrokerSimRegistry` already
        // simulates - see `CollectorConfig::suppress_simulation_for`'s doc
        // comment (`crates/banto-collect/src/config.rs`) and this module's
        // doc comment ("接続単位のシミュレーションモード") for the full
        // derivation. Under #131 this set was deliberately narrower than
        // `broker_handles.keys()` (SLMP only, because Modbus reads were not
        // broker-routed); since #337 they coincide, and
        // `sync_broker_sessions_from` derives this one from `broker_handles`
        // precisely so the two cannot drift apart again - see that fn's
        // `read_routed_keys` doc paragraph.
        config.suppress_simulation_for(&read_routed_keys);

        // T9-2: also stamp each broker-routed connection plan with the SAME
        // resolved dial target `sync_broker_sessions_from` just used, so a
        // simulation toggle (or a simulator restart, or an in-place host/port
        // edit) that actually moved the broker session makes `apply_config`'s
        // `PartialEq` diff notice and respawn that connection's task with a
        // fresh `ClientFactory` - suppressing `simulation` alone leaves the
        // plan otherwise byte-for-byte identical across such a toggle, which
        // would classify the connection "unchanged" and leave its running
        // task wired to a now-superseded broker session forever. See
        // `CollectorConfig::set_broker_dial_target`'s doc comment
        // (`crates/banto-collect/src/config.rs`) for the full derivation.
        for (key, (host, port)) in &resolved_broker_targets {
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
            self.db_source.commit(db_source_plan);
            // 停止は削除と直列（`Self::writer_gate`）。`stop` が終わるまで持つ。
            let _writer_gate = self.writer_gate.lock().await;
            let old_collector = self.collector.lock().await.take();
            let new_revision = {
                let mut inner = self.inner.lock().expect("hub state lock poisoned");
                self.server_store()
                    .observe_catalog(&inner.map, &new_map, self.clock.now_ms());
                warn_external_name_collisions(&inner.map, &new_map);
                inner.map = Arc::new(new_map);
                inner.revision += 1;
                inner.last_error = None;
                inner.current = None;
                inner.last_apply = None;
                // #341 レビュー対応: `Self::apply_run` と同じロールバック
                // 基準をここでも更新する（`rebuild` は run mode を持たない
                // ので snapshot そのものが runtime snapshot）。
                inner.applied_runtime = snapshot.clone();
                inner.applied_registry = snapshot.clone();
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
            self.remove_stale_broker_sessions(&stale_broker_ids).await;
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
        // effect on failure" contract themselves now. T2-2/#337: the client
        // factory routes every broker-managed connection (SLMP since T2-2,
        // Modbus TCP since #337) through the broker sessions just synced
        // above, falling back to a direct client only when a connection is
        // defensively missing from the handle map
        // (`crate::broker_glue::hub_client_factory`'s doc comment).
        let factory = hub_client_factory(Arc::new(broker_handles), &self.sessions);
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
                // #341 レビュー対応: `Self::apply_run` と同じ理由・同じ
                // 手当て（同 fn の「失敗時のロールバック」節）。
                self.rollback_broker_sessions().await;
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
        self.db_source.commit(db_source_plan);
        let new_revision = {
            let mut inner = self.inner.lock().expect("hub state lock poisoned");
            self.server_store()
                .observe_catalog(&inner.map, &new_map, self.clock.now_ms());
            warn_external_name_collisions(&inner.map, &new_map);
            inner.map = Arc::new(new_map);
            inner.revision += 1;
            inner.last_error = None;
            inner.current = Some(current_handle);
            inner.last_apply = apply_report;
            inner.applied_runtime = snapshot.clone();
            inner.applied_registry = snapshot.clone();
            inner.revision
        };
        let _ = self.revision_tx.send(new_revision);

        self.remove_stale_broker_sessions(&stale_broker_ids).await;
        self.log_simulation_warnings().await;

        Ok(())
    }

    fn set_last_error(&self, message: String) {
        self.inner
            .lock()
            .expect("hub state lock poisoned")
            .last_error = Some(message);
    }

    /// [`Self::fail_next_apply`] を読んで下ろす（テスト専用）。本番ビルドでは
    /// `None` を返すだけの `#[inline]` な関数になり、分岐ごと最適化で消える。
    #[cfg(test)]
    fn take_injected_apply_failure(&self) -> Option<String> {
        self.fail_next_apply
            .swap(false, std::sync::atomic::Ordering::SeqCst)
            .then(|| "テスト用に注入された実行構成の適用失敗".to_string())
    }

    #[cfg(not(test))]
    #[inline]
    fn take_injected_apply_failure(&self) -> Option<String> {
        None
    }

    /// 次の [`Self::apply_run`] を失敗させる（テスト専用 -
    /// [`Self::fail_next_apply`] の doc comment 参照）。
    #[cfg(test)]
    pub(crate) fn fail_next_apply_for_test(&self) {
        self.fail_next_apply
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// 次の [`Self::apply_run`] を指定ミリ秒だけ遅らせる（テスト専用）。
    /// #341 レビュー対応2: 「live apply の最中に来た stop が無視されない」
    /// ことを決定的に確かめるために要る（`crate::controller` の
    /// `stop_during_a_live_apply_waits_instead_of_being_ignored`）。
    /// `fail_next_apply_for_test` と同じ流儀で `#[cfg(test)]`。
    #[cfg(test)]
    pub(crate) fn delay_next_apply_for_test(&self, delay: std::time::Duration) {
        self.delay_next_apply.store(
            delay.as_millis() as u64,
            std::sync::atomic::Ordering::SeqCst,
        );
    }

    /// [`Self::delay_next_apply`] を読んで下ろす（テスト専用）。
    #[cfg(test)]
    async fn take_injected_apply_delay(&self) {
        let millis = self
            .delay_next_apply
            .swap(0, std::sync::atomic::Ordering::SeqCst);
        if millis > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(millis)).await;
        }
    }

    #[cfg(not(test))]
    #[inline]
    async fn take_injected_apply_delay(&self) {}

    /// #341 レビュー対応2（2026-09-14）: catalog・演算タグ plan・DB Source
    /// plan を `inner.applied_registry`（直近で適用に成功した registry
    /// snapshot）へ**戻す** - live 経路
    /// （`crate::controller::CollectionController::commit_catalog_and_apply_live`）
    /// が `commit_catalog` に成功したあと [`Self::apply_run`] で失敗した
    /// ときだけ呼ばれる。
    ///
    /// これが無いと「読み側（catalog・演算・DB Source）は新構成、収集側は
    /// 旧構成」という食い違いが残る（collector/broker のロールバックだけ
    /// では埋まらない）。戻したあとの状態は「**DB は新・runtime 側は全部
    /// 旧**」で一貫し、回復手段は `crate::rest::LIVE_APPLY_RECOVERY_HINT`
    /// のとおり（次の CRUD / 収集の停止→開始 / `POST /api/collection/reapply`）。
    ///
    /// `configured_revision` がこのロールバックでもう一度進み
    /// `config_changed` が飛ぶのは**意図どおり** - 新 catalog を取りに来た
    /// クライアントに「戻った」ことを知らせる契機が要る。
    ///
    /// ベストエフォート: 戻す `commit_catalog` 自体が失敗しても元の失敗
    /// 理由を上書きしない（`last_error` は呼び出し前の値＝ `apply_run` の
    /// 失敗理由へ必ず復元する。`commit_catalog` は成功すると
    /// `last_error` を `None` にしてしまうため）。
    pub(crate) async fn rollback_catalog(&self) {
        let (previous, last_error) = {
            let inner = self.inner.lock().expect("hub state lock poisoned");
            (inner.applied_registry.clone(), inner.last_error.clone())
        };
        let result = self.commit_catalog(&previous).await;
        {
            let mut inner = self.inner.lock().expect("hub state lock poisoned");
            inner.last_error = last_error;
        }
        match result {
            Ok(_) => self.diag_log.err_line(
                "banto-hub: 実行構成の適用に失敗したため、catalog を直前の構成へ戻しました",
            ),
            Err(err) => self.diag_log.err_line(&format!(
                "banto-hub: catalog を直前の構成へ戻せませんでした（DB の内容と catalog が食い違ったままです。POST /api/collection/reapply で再適用してください）: {err}"
            )),
        }
    }

    /// #341 レビュー対応（2026-09-14）: broker セッション集合を
    /// `inner.applied_runtime`（直近で適用に成功した runtime snapshot）へ
    /// 戻す - [`Self::apply_run`] が collector 側のコミットに失敗したときの
    /// ベストエフォートのロールバック（同 fn の doc comment
    /// 「失敗時のロールバック」節に保証の範囲を書いてある）。
    ///
    /// [`Self::sync_broker_sessions_from`] + [`Self::remove_stale_broker_sessions`]
    /// を**失敗した適用のときと同じ順序で**、ただし旧 snapshot に対して
    /// 呼び直すだけ - 新しい構成のためだけに張られたセッションは
    /// 「wanted に無い」＝ stale として落ち、ダイヤル先を差し替えられて
    /// いた接続は元の宛先で張り直される。まだ一度も適用に成功していない
    /// （`applied_runtime` が `RegistrySnapshot::default()`）場合は
    /// 「セッションを1本も張らない状態」が正しい戻し先なので、結果として
    /// 今回張ったものが全部落ちる。
    ///
    /// セッション同期そのものは `Err` を返さない（個々の
    /// `ensure_connection` 失敗は `HubSessions` 側でログに出るだけ）ので、
    /// この関数も戻り値を持たない - 呼び出し元は元の失敗理由をそのまま
    /// `last_error`/`Err` として返す。
    async fn rollback_broker_sessions(&self) {
        let previous = self
            .inner
            .lock()
            .expect("hub state lock poisoned")
            .applied_runtime
            .clone();
        let (_handles, stale_ids, _targets, _read_routed) =
            self.sync_broker_sessions_from(&previous).await;
        self.remove_stale_broker_sessions(&stale_ids).await;
        self.diag_log.err_line(
            "banto-hub: 実行構成の適用に失敗したため、broker セッションを直前の構成へ戻しました",
        );
    }

    /// **Naming note (#337, 2026-09-08)**: this fn's name used to name the
    /// SLMP protocol specifically, from T2-2 until this rename to
    /// `sync_broker_sessions_from`. It started SLMP-only; #131 (2026-09-01)
    /// widened it to also manage Modbus TCP's broker session (write/status
    /// routing only at the time), and #337 then moved Modbus's *collection
    /// reads* onto that same session too, closing the last gap where the old
    /// SLMP-flavoured name was misleading.
    ///
    /// T2-2/T7-2/T9-2/#131 (docs/tag-server-design.md §6-5/§4.3, docs/ux-plan.md
    /// §1): additive `ensure_connection` (unchanged from T2-2) over
    /// `self.sessions`'s broker tasks against the registry's current
    /// enabled-and-broker-managed-protocol connection set (`#131`,
    /// 2026-09-01: `HubSessions::is_broker_managed` - `"slmp"` and
    /// `"modbus-tcp"` today), returning the `"conn:{id}"`-keyed handle map
    /// [`crate::broker_glue::hub_client_factory`] needs, the connection ids
    /// `self.sessions` still tracks that are NOT in that set (deleted from
    /// the registry, disabled, or changed to a protocol the broker no longer
    /// manages), resolved dial targets, and (see the fourth return value's
    /// own doc paragraph below) the read-routed subset. Widening this past
    /// SLMP is what made a Modbus connection's *writes*
    /// (`crate::write_path`) and *status* (`Self::broker_status`)
    /// broker-routed under #131; **#337 (2026-09-08)** then moved a Modbus
    /// connection's *collection reads* onto the same session too
    /// (`crate::broker_glue::hub_client_factory`'s doc comment), so the
    /// read-routed subset is no longer narrower than the handle map - see the
    /// fourth return value's own doc paragraph. See
    /// `crate::broker_glue::HubSessions`'s doc comment
    /// ("Session sync policy") for the full T7-2 policy this implements.
    ///
    /// **T9-2 addition**: before calling `ensure_connection` for a
    /// connection, this now calls `self.sim_registry.resolve(conn)` to get
    /// the *effective* `(host, port)` to dial - the connection's own
    /// host/port unless `conn.simulation` is true, in which case it is the
    /// address of an in-process simulator `BrokerSimRegistry` starts/reuses on
    /// this connection's behalf (see `BrokerSimRegistry::resolve`'s doc
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
    /// see `BrokerSimRegistry::resolve`'s doc comment for the full derivation
    /// of why this matters.
    ///
    /// The stale ids are returned, not removed here - [`Self::rebuild`]
    /// only calls [`Self::remove_stale_broker_sessions`] with them AFTER the
    /// collector-side commit for this same rebuild has succeeded (so any
    /// collect task reading through that connection's session is already
    /// confirmed stopped - see this module's doc comment "broker
    /// セッションの削除同期").
    ///
    /// A registry read failure here is logged and treated as "no
    /// broker-managed connections this rebuild, nothing stale either" (empty
    /// on both counts: every broker-managed connection falls back to
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
    /// computed for every broker-routed connection, regardless of
    /// whether it changed. [`Self::rebuild`] feeds every entry into
    /// `banto_collect::CollectorConfig::set_broker_dial_target` - necessary
    /// because `BrokerSimRegistry::resolve`'s `changed`-triggered
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
    /// `set_broker_dial_target` used to mutate SLMP plans only, which was
    /// correct while a Modbus plan's `simulation` flag still varied across a
    /// toggle (it was never suppressed, so `apply_config`'s diff noticed the
    /// toggle on its own). #337 made Modbus reads broker-routed and therefore
    /// `suppress_simulation_for`-suppressed too, which flattened that signal
    /// away - so `set_broker_dial_target` now stamps a Modbus plan as well
    /// (see that fn's own doc comment).
    ///
    /// **T9-2 fourth return value (`read_routed_keys`; #131 2026-09-01,
    /// reworked by #337 2026-09-08)**: the connection keys whose *collection
    /// reads* are broker-routed - the set [`Self::rebuild`]/[`Self::apply_run`]
    /// feed into `banto_collect::CollectorConfig::suppress_simulation_for`
    /// ("something else already substitutes this connection's simulator,
    /// stand down your own").
    ///
    /// Under #131 this was strictly narrower than `handles.keys()`: it was
    /// accumulated with a direct `protocol == "slmp"` check, because a Modbus
    /// connection held a broker session for writes/status while its reads
    /// stayed on banto-collect's own direct `ModbusTcpClient`. #337 moved
    /// Modbus reads onto the broker session too
    /// (`crate::broker_glue::hub_client_factory`), so "holds a broker
    /// session" and "reads through the broker" are now the same set, and
    /// this value is **derived from `handles`** at the end of this fn instead
    /// of accumulated alongside it.
    ///
    /// It is still returned as its own value rather than having the callers
    /// write `handles.keys()` themselves, because the two answer different
    /// questions - "which sessions did this sync establish" vs. "whose
    /// simulator must `Collector` stand down for" - and only this fn is in a
    /// position to say that they currently coincide. Deriving one from the
    /// other (rather than building both) is what makes the coincidence
    /// structural instead of a rule two loops have to keep agreeing on.
    ///
    /// Deriving it from `handles` also closes a latent hole in the accumulate
    /// version: a key was inserted *before* `ensure_connection` ran, so a
    /// connection whose session failed to spawn was still reported
    /// read-routed even though `hub_client_factory` would fall back to a
    /// direct client for it - a `simulation = true` connection in that state
    /// had `Collector`'s own simulator suppressed and dialed its real
    /// host/port. `handles` only contains connections whose
    /// `ensure_connection` actually succeeded, which is exactly the condition
    /// `hub_client_factory` routes on.
    ///
    /// **T19 S2-a (UX-48, docs/banto-hub-t19-design.md §3.8)**: a connection
    /// with nothing to collect - no *enabled* [`banto_tags::CollectionGroup`]
    /// under it at all - is no longer in the wanted set either, so it gets
    /// no broker session pre-synced here. This mirrors `banto-collect`'s own
    /// `build_config_from` skip rule exactly ("A connection with no
    /// collected groups gets no task and no socket") via the shared
    /// [`banto_collect::connections_with_collected_groups`] predicate - same
    /// input snapshot, same rule, so the two can never silently disagree.
    /// Before this, EVERY enabled+broker-managed-protocol connection got a
    /// session regardless of tag count, which is what made a "registered but
    /// still empty" connection sit in `reconnecting` on the status screen
    /// forever during development.
    ///
    /// A connection that already falls out of the wanted set here (its only
    /// group got disabled, or its last group/tag was deleted) is handled by
    /// the SAME pre-existing mechanism as any other now-unwanted connection:
    /// it lands in the `stale_ids` this fn already computes below and gets
    /// torn down via [`Self::remove_stale_broker_sessions`] after the caller's
    /// collector-side commit succeeds (T7-2's "add + remove" full sync) - no
    /// new removal path was needed.
    ///
    /// **This does NOT reopen the on-demand `ensure_connection`-on-write
    /// path** (`crate::write_path` never calls it - see
    /// [`Self::write_broker_handle_peek`]'s doc comment, T15-4: the write
    /// path only *peeks* an already-synced session and fails closed
    /// (`WriteRejection::WriteFailed`) rather than dialing on demand, to
    /// avoid resurrecting a session a concurrent `stop` just tore down).
    ///
    /// **T19 S2-a 案B (2026-09-03) closed the write-gap this paragraph used
    /// to describe, and #341 (2026-09-14) widened that fix**: a connection
    /// with zero enabled groups the last time `Self::rebuild`/
    /// `Self::apply_run` ran no longer has to wait for the next
    /// rebuild/apply_run (typically: stopping and restarting the collection
    /// run) to get a session once a group/tag is registered under it -
    /// [`crate::rest::commit_catalog_and_notify`] drives
    /// [`crate::controller::CollectionController::commit_catalog_and_apply_live`]
    /// after every registry change, which re-runs this very fn via
    /// [`Self::apply_run`] whenever a collection run is already `Running`.
    /// 案B only re-synced sessions (leaving the running `Collector`'s task
    /// set alone, with the removal-ordering caveat that came with it);
    /// since #341 the whole `apply_run` runs, so the session sync and the
    /// collect tasks move together - see that method's doc comment for the
    /// full derivation, including why it is safe against the same T15-4
    /// stop-vs-write race this fn's caller ([`Self::rebuild`]) already had
    /// to reckon with.
    async fn sync_broker_sessions_from(
        &self,
        snapshot: &RegistrySnapshot,
    ) -> (
        HashMap<String, ReadOnlyHandle>,
        Vec<i64>,
        HashMap<String, (String, i64)>,
        HashSet<String>,
    ) {
        let mut handles = HashMap::new();
        let mut resolved_targets = HashMap::new();
        let mut wanted_ids: HashSet<i64> = HashSet::new();
        // T19 S2-a (UX-48): see this fn's own doc comment above - the same
        // predicate `build_config_from` uses to decide "does this connection
        // have anything to collect", shared via banto-collect so the two
        // definitions of "tagless connection" cannot drift apart.
        let collectible_connection_ids = connections_with_collected_groups(&snapshot.groups);
        // #131 (2026-09-01): every connection whose protocol the broker has a
        // driver for - not just SLMP - gets a session synced here (needed for
        // write/status routing, `crate::write_path`/`CollectorManager::broker_status`).
        // #337 (2026-09-08) removed the one remaining exception: a Modbus
        // connection's *collection reads* now go through this same session
        // too, so "the broker manages this protocol" and "this connection's
        // reads are broker-routed" have become one and the same question -
        // see this fn's `read_routed_keys` doc paragraph and
        // `crate::broker_glue::hub_client_factory`'s doc comment.
        for conn in snapshot.connections.iter().filter(|c| {
            c.enabled
                && self.sessions.is_broker_managed(&c.protocol)
                && collectible_connection_ids.contains(&c.id)
        }) {
            wanted_ids.insert(conn.id);

            // T9-2: resolve the effective dial target (simulator address if
            // `conn.simulation`, else the connection's own host/port) BEFORE
            // ensure_connection - see this fn's own doc comment and
            // `BrokerSimRegistry::resolve`'s doc comment for why the ordering
            // and the `changed`-triggered `sessions.remove` matter.
            let (host, port, changed) = self.sim_registry.resolve(conn).await;
            if changed {
                self.sessions.remove(conn.id);
            }
            let key = format!("conn:{}", conn.id);
            resolved_targets.insert(key.clone(), (host.clone(), port));

            // #337 (2026-09-08): there used to be a `if conn.protocol ==
            // "slmp"` filter here, deliberately narrower than the
            // `is_broker_managed` filter above, building a separate
            // `read_routed_keys` set. Its reason: `hub_client_factory`'s
            // `ModbusTcp` arm read through banto-collect's own direct
            // `ModbusTcpClient` (#131 moved only Modbus writes/status onto
            // the broker), so putting a Modbus key into the set
            // `Self::rebuild`/`Self::apply_run` feed to
            // `CollectorConfig::suppress_simulation_for` - which tells
            // `Collector` "something else already handles this connection's
            // simulation, stand down your own simulator" - would have left a
            // `simulation = true` Modbus connection's reads dialing the real
            // (in dev/test, unreachable) host while only its writes reached
            // the in-process simulator.
            //
            // That reason is gone: #337 routes Modbus collection reads
            // through the broker session too, and
            // `crate::broker_glue::BrokerSimRegistry::resolve` has started a
            // Modbus-flavoured simulator for such a connection since #131 -
            // so suppressing `Collector`'s own simulator is now exactly the
            // right thing for Modbus as well. The set is therefore no longer
            // accumulated here at all; it is derived from `handles` below,
            // which is what actually decides read routing.

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
                        "banto-hub: ブローカーセッションの起動に失敗しました (接続 {}): {err}",
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

        // #337: derived from `handles`, never accumulated separately - see
        // this fn's `read_routed_keys` doc paragraph for why deriving is what
        // keeps the two sets from drifting, and why a connection whose
        // `ensure_connection` failed above must be excluded.
        let read_routed_keys: HashSet<String> = handles.keys().cloned().collect();

        (handles, stale_ids, resolved_targets, read_routed_keys)
    }

    /// **Naming note (#337, 2026-09-08)**: renamed alongside
    /// [`Self::sync_broker_sessions_from`] from an SLMP-specific name - see
    /// that fn's doc comment for why the old name stopped matching reality.
    ///
    /// T14-2/T7-2/T9-2: [`crate::broker_glue::HubSessions::stop_and_join`] and
    /// [`crate::broker_glue::BrokerSimRegistry::remove`] for every id in
    /// `stale`. Must only be called AFTER the collector-side commit for the
    /// same rebuild has succeeded (see
    /// [`Self::rebuild`]/[`Self::sync_broker_sessions_from`]'s doc comments) - by
    /// then, `apply_config`/the pre-commit `Collector` stop has already
    /// stopped any collect task that was reading through one of these
    /// connections' broker sessions. `async` (T9-2: `BrokerSimRegistry::remove`
    /// is `.await`-heavy, stopping a simulator's ramp task) - was sync before.
    async fn remove_stale_broker_sessions(&self, stale: &[i64]) {
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
    ///
    /// #616: 同じ文面は続けて出さない（[`sim_warning_due`]。設定の 1 回の変更で
    /// `rebuild`・`apply_run` が続けて呼ばれ、同じ時刻に 2〜4 回出ていた）。起動直後・
    /// 再び有効にしたとき・文面が変わったとき・[`SIM_WARNING_REPEAT_MS`] を過ぎた後は出す。
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

        let mut last = self
            .last_sim_warning
            .lock()
            // 診断のためだけに panic させない（#655 のレビュー Nit 4）。
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if names.is_empty() {
            // 無くなったら忘れる（再び有効にしたときに出す）。
            *last = None;
            return;
        }
        let text = format!(
            "banto-hub: [注意] シミュレーションモードの接続が {} 件あります: {} - 本番運用では無効化してください",
            names.len(),
            names.join(", "),
        );
        let now_ms = self.clock.now_ms();
        // #616: 設定の 1 回の変更で続けて呼ばれても、同じ文面は 1 回だけ出す。
        if sim_warning_due(last.as_ref(), &text, now_ms) {
            self.diag_log.line(&text);
            *last = Some((text, now_ms));
        }
    }

    /// T2-2/#131 (docs/tag-server-design.md §6-5): the broker's own
    /// connection status for a broker-managed connection, or `None` if no
    /// broker session has ever been started for it (never a protocol the
    /// broker manages - `HubSessions::is_broker_managed` - or no
    /// rebuild has run yet). Used by `crate::rest`'s `/api/v1/status` handler
    /// in place of [`Self::connection_status`] for connections whose
    /// protocol the broker manages (as of #131, 2026-09-01: `"slmp"` and
    /// `"modbus-tcp"`, not just SLMP) - see `crate::broker_glue`'s module doc
    /// ("The two-backoff double bookkeeping") for why the broker's status,
    /// not banto-collect's own, is the one that answers "is the physical
    /// session up". (#344, 2026-09-15: banto-collect's own status now
    /// *follows* this one - `BrokerReadClient::connect` fails while the
    /// broker session is down - so the two rarely disagree any more. This
    /// method is still the authority, and `/api/v1/status` still sources it.)
    pub fn broker_status(&self, connection_id: i64) -> Option<BrokerConnectionStatus> {
        self.sessions
            .status_watch(connection_id)
            .map(|watch| *watch.borrow())
    }

    /// T2-4（docs/tag-server-design.md §6 item 5「読み書き単一セッション」）:
    /// `conn`（broker が管理するプロトコルの接続 - 現状 slmp と
    /// modbus-tcp）の書き込み可能な [`BrokerHandle`] を取得する。
    /// `crate::rest` の書き込みハンドラの唯一の入口 - `self.sessions`
    /// （`Self::sync_broker_sessions_from`が rebuild の度に確保する broker
    /// セッション directory）に委譲するだけで、
    /// `Self::sync_broker_sessions_from`が読み取り専用ハンドルへ絞る
    /// （`ReadOnlyHandle`、`banto_collect::PlcClient` 経由）のと対称的に、
    /// こちらは書き込み可能なフル `BrokerHandle` をそのまま返す（SLMP は
    /// 収集と書き込みが同じ物理セッションを通る、というのがこの broker
    /// 統合方針の核心 - 設計 §6 item 5。Modbus TCP は #131 で書き込みが、
    /// #337（2026-09-08）で収集読み取りもこの同じセッションを通るように
    /// なり、現在は SLMP と完全に同じ扱いである -
    /// `crate::broker_glue::hub_client_factory`の doc comment 参照）。
    ///
    /// `HubSessions::ensure_connection` は冪等（既存セッションがあれば
    /// それをそのまま返す）なので、直近の `rebuild` が既に確保済みの
    /// セッションと同じものが返る。`rebuild` と競合しても安全（同じ
    /// セッションに収束する）。
    ///
    /// **T15-4 以降、書き込み経路（`crate::write_path::write_plc_tag`）は
    /// これを使わない** - `ensure_connection` はセッションが無ければ新規に
    /// ダイヤルしてしまい、`CollectionController::stop`(→ この
    /// `sessions.stop_and_join`)と `execute_write` の `.await` 区間が
    /// 競合した場合に実機へ意図しないセッションを張ってしまう
    /// （[`Self::write_broker_handle_peek`] の doc comment 参照）。書き込み
    /// 経路は必ず `write_broker_handle_peek` を使うこと。このメソッド自体は
    /// ensure セマンティクスを必要とする他の呼び出し元向けに残してある
    /// （現状、`crate` 内に他の呼び出し元はない）。
    pub fn write_broker_handle(&self, conn: &PlcConnection) -> Result<BrokerHandle, BrokerError> {
        self.sessions.ensure_connection(conn)
    }

    /// T15-4（このモジュールの上の [`Self::write_broker_handle`] doc comment
    /// 参照）: 書き込み経路専用の non-spawning peek。`connection_id` の
    /// broker セッションが既に生きていればその書き込み可能な
    /// [`BrokerHandle`] を返し、無ければ `None`（新規にはダイヤルしない -
    /// fail closed）。`crate::write_path::write_plc_tag` の唯一の入口。
    pub fn write_broker_handle_peek(&self, connection_id: i64) -> Option<BrokerHandle> {
        self.sessions.write_handle_for(connection_id)
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

    /// T15-2 (docs/banto-hub-desktop-plan.md §9.7): all-simulation 開始前の
    /// カバレッジプリフライト - レジストリを新鮮に読み直し（`Self::rebuild`/
    /// `Self::apply_run`と同じ`RegistrySnapshot::load`）、有効な物理 PLC タグ
    /// 全件を[`banto_collect::simulation::classify_plc_tag`]で分類する。
    /// **`start(AllSimulation)`自体はブロックしない** - このメソッドは表示専用
    /// の集計を返すだけで、呼び出し元(`crate::rest`)は結果を無視しても
    /// all-simulation を開始できる(プラン §9.7 の決定どおり)。
    ///
    /// 現在コミット済みの[`TagMap`]ではなく、その場でレジストリを読み直す -
    /// [`Self::apply_run`]の`AllSimulation`分岐(`runtime_snapshot_for_mode`)が
    /// 見るのと同じ「レジストリの今の状態」であるべきで、直前の rebuild 以降に
    /// 未反映の登録変更を古いカタログ越しに見せないため。
    pub async fn simulation_coverage_report(&self) -> Result<SimulationCoverageReport, String> {
        let snapshot = RegistrySnapshot::load(&self.pool)
            .await
            .map_err(|err| format!("レジストリのスナップショット取得に失敗しました: {err}"))?;
        Ok(simulation_coverage_report_from(&snapshot))
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
        // 外部 DB 連携 S2: `Self::rebuild` と同じ位置・同じ理由
        // （§4.5「Source の task は `commit_catalog` の完了を受けて配下の
        // グループ集合を再読込する」- 稼働中の pending queue 適用がここを
        // 通るので、走行中の DB Source もこの1点で追従する）。
        let db_source_plan = match db_source::build_plan(snapshot) {
            Ok(plan) => plan,
            Err(err) => {
                let message = format!("DB Source の検証に失敗しました: {err}");
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
        self.db_source.commit(db_source_plan);
        let revision = {
            let mut inner = self.inner.lock().expect("hub state lock poisoned");
            self.server_store()
                .observe_catalog(&inner.map, &new_map, self.clock.now_ms());
            warn_external_name_collisions(&inner.map, &new_map);
            inner.map = Arc::new(new_map);
            inner.revision += 1;
            inner.last_error = None;
            inner.revision
        };
        let _ = self.revision_tx.send(revision);
        Ok(revision)
    }

    /// Apply a fresh registry snapshot to a collection run. The saved
    /// snapshot is validated for the catalog, while the selected mode may
    /// provide a non-persistent runtime override before collector and broker
    /// side effects begin.
    ///
    /// # run mode は **collector 構成と broker セッションの両方**に効く
    ///
    /// （#341 レビューでの指摘に対する明示。指摘は「セッション同期は生の
    /// registry snapshot で行われるので `AllSimulation` でも実機へダイヤル
    /// する」だったが、**事実ではない**）: このメソッドは
    /// [`runtime_snapshot_for_mode`] を**先に**適用し、その `runtime_snapshot`
    /// を `build_config_from` と [`Self::sync_broker_sessions_from`] の
    /// **両方**へ渡す。したがって `AllSimulation` 運転中は、永続設定が
    /// `simulation: false` の接続でも
    /// `crate::broker_glue::BrokerSimRegistry::resolve` がシミュレータの
    /// loopback アドレスを解決し、broker セッションはそちらへ張られる
    /// （実機の host/port へは一度もダイヤルしない）。
    /// 生の `snapshot` を使うのは catalog / 演算タグ / DB Source の
    /// **検証**だけで、これらは run mode に依存しない。
    /// （run mode を一切見ないのは [`Self::rebuild`] の方で、だからこそ
    /// 稼働中の live re-apply には使えない -
    /// `crate::controller::CollectionController::commit_catalog_and_apply_live`
    /// の doc comment 参照。）
    ///
    /// # 失敗時のロールバック（#341 レビュー対応、2026-09-14）
    ///
    /// 順序は「セッション同期（新規 ensure・ダイヤル先変更の差し替え）→
    /// collector 側のコミット（`apply_config` / `start_with_client_factory`）
    /// → stale セッション削除」。collector 側が失敗すると、**セッション
    /// 変更だけが残る**（新しい構成のために張ったセッションが、走り続けて
    /// いる旧構成の下に残る）。これを避けるため、collector 側が `Err` を
    /// 返したら `inner.applied_runtime`（直近で適用に成功した runtime
    /// snapshot）でセッション同期をやり直し、**セッション集合を旧構成へ
    /// 戻す**（[`Self::rollback_broker_sessions`]）。
    ///
    /// **保証の範囲はベストエフォート**: 戻し操作自体も I/O なので失敗
    /// しうる（その場合は `last_error` に追記するだけで、元の失敗理由を
    /// 上書きしない）。また戻せるのは**セッション集合**（どの接続に
    /// セッションが存在するか・どこへダイヤルしているか）だけで、
    /// 戻す過程で張り直したセッションは新しい TCP 接続になる。collector
    /// 側は `apply_config`/`start_with_client_factory` 自身の
    /// all-or-nothing 契約により元のまま無傷で、catalog/`revision` は
    /// そもそもこのメソッドが触らない。
    pub async fn apply_run(&self, mode: crate::controller::RunMode) -> Result<(), String> {
        let _guard = self.rebuild_lock.lock().await;
        // #341 レビュー対応（2026-09-14）: 検証失敗も必ず `last_error`
        // （`/api/v1/status` の `last_config_error`）へ残す - 以前はここだけ
        // `map_err(..)?` で素通りしていて、`crate::rest::commit_catalog_and_notify`
        // の契約（「500 の詳細は `last_config_error` にも残る」）と食い違って
        // いた。`Self::rebuild`/`Self::commit_catalog` は元から同じ扱い。
        let snapshot = RegistrySnapshot::load(&self.pool).await.map_err(|err| {
            self.set_last_error(format!(
                "レジストリのスナップショット取得に失敗しました: {err}"
            ));
            format!("レジストリのスナップショット取得に失敗しました: {err}")
        })?;
        let new_map = build_catalog_from(&snapshot).map_err(|err| {
            let message = format!("catalog の検証に失敗しました: {err}");
            self.set_last_error(message.clone());
            message
        })?;
        computed::build_plan(&new_map).map_err(|err| {
            let message = format!("演算タグの検証に失敗しました: {err}");
            self.set_last_error(message.clone());
            message
        })?;
        // 外部 DB 連携 S2: ここは `computed` と同じく**検証だけ**行い commit
        // はしない - 構成の反映（= 計画の入れ替え）は
        // `Self::rebuild`/`Self::commit_catalog` の担当だから。
        //
        // S2b（§4.8・§6-16）: 収集開始で DB Source の task を起こすのは
        // `crate::controller::CollectionController::start_locked` であって
        // ここではない - この `apply_run` は「収集の遷移」以外に
        // `crate::controller::CollectionController::commit_catalog_and_apply_live`
        // （#341 の live re-apply。2026-09-14 より前は
        // `commit_catalog_and_notify` の legacy live reconfigure）からも
        // 呼ばれるので、ここを起動点にすると停止中でも DB へ繋いでしまう
        // （controller の `db_source` フィールド doc comment 参照）。
        db_source::build_plan(&snapshot).map_err(|err| {
            let message = format!("DB Source の検証に失敗しました: {err}");
            self.set_last_error(message.clone());
            message
        })?;
        let runtime_snapshot = runtime_snapshot_for_mode(&snapshot, mode);
        let mut config = build_config_from(&runtime_snapshot).map_err(|err| {
            let message = err.to_string();
            self.set_last_error(message.clone());
            message
        })?;

        // テスト専用の遅延注入（`Self::delay_next_apply`）- 本番ビルドでは
        // 空の `async fn` になり消える。
        self.take_injected_apply_delay().await;

        let (broker_handles, stale_broker_ids, resolved_broker_targets, read_routed_keys) =
            self.sync_broker_sessions_from(&runtime_snapshot).await;
        // See `Self::rebuild`'s matching call for what `read_routed_keys` is
        // and why it is `sync_broker_sessions_from`'s job to derive it (#131's
        // narrowing, reworked by #337).
        config.suppress_simulation_for(&read_routed_keys);
        for (key, (host, port)) in &resolved_broker_targets {
            config.set_broker_dial_target(key, host.clone(), *port);
        }

        if config.group_count() == 0 {
            // 停止は削除と直列（`Self::writer_gate`）。`stop` が終わるまで持つ。
            let _writer_gate = self.writer_gate.lock().await;
            let old_collector = self.collector.lock().await.take();
            if let Some(collector) = old_collector {
                let _ = collector.stop().await;
            }
            {
                let mut inner = self.inner.lock().expect("hub state lock poisoned");
                inner.current = None;
                inner.last_apply = None;
                inner.last_error = None;
                // #341 レビュー対応: この分岐も「適用に成功した」状態なので
                // ロールバック基準を更新する（この runtime snapshot に対して
                // 同期済みのセッション集合が、以後の正しい戻し先）。
                inner.applied_runtime = runtime_snapshot.clone();
                inner.applied_registry = snapshot.clone();
            }
            self.remove_stale_broker_sessions(&stale_broker_ids).await;
            self.advance_running_revision();
            return Ok(());
        }

        let factory = hub_client_factory(Arc::new(broker_handles), &self.sessions);
        let mut collector_guard = self.collector.lock().await;
        let commit: Result<(Option<ApplyReport>, CurrentValuesHandle), String> =
            if let Some(message) = self.take_injected_apply_failure() {
                // テスト専用の注入（`Self::fail_next_apply`）。セッション同期は
                // 既に走っているので、下のロールバック経路もそのまま通る。
                Err(message)
            } else if let Some(collector) = collector_guard.as_mut() {
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
                // #341 レビュー対応（2026-09-14、このメソッドの doc comment
                // 「失敗時のロールバック」）: collector 側が失敗した時点で、
                // このメソッドが上で行ったセッション変更（新規 ensure・
                // ダイヤル先の差し替え）だけが残っている。走り続けている
                // 旧構成に合わせて戻す。
                self.rollback_broker_sessions().await;
                self.set_last_error(message.clone());
                return Err(message);
            }
        };
        {
            let mut inner = self.inner.lock().expect("hub state lock poisoned");
            inner.current = Some(current_handle);
            inner.last_apply = apply_report;
            inner.last_error = None;
            // #341 レビュー対応: 次に失敗したときの戻し先はこの構成。
            inner.applied_runtime = runtime_snapshot.clone();
            inner.applied_registry = snapshot.clone();
        }
        self.remove_stale_broker_sessions(&stale_broker_ids).await;
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
        // 停止は削除と直列（`Self::writer_gate`）。`stop` が終わるまで持つ。
        let _writer_gate = self.writer_gate.lock().await;
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
        // #341 レビュー対応: セッションを全部落とすので、以後のロール
        // バック基準も「1本も張っていない状態」に戻す（次の
        // `Self::apply_run` が失敗したときに、止めたはずのセッションを
        // 復活させないため）。
        {
            let mut inner = self.inner.lock().expect("hub state lock poisoned");
            inner.applied_runtime = RegistrySnapshot::default();
            inner.applied_registry = RegistrySnapshot::default();
        }
        let connection_ids = self.sessions.connection_ids();
        for connection_id in connection_ids {
            let _ = self.sessions.stop_and_join(connection_id).await;
            self.sim_registry.remove(connection_id).await;
        }
    }

    // #341 (2026-09-14): T19 S2-a 案B の `resync_broker_sessions` はここに
    // あったが、catalog-only commit そのものが無くなった（稼働中の commit は
    // `crate::controller::CollectionController::commit_catalog_and_apply_live`
    // が [`Self::apply_run`] まで通す）ため撤去した。broker セッションの
    // 追加/削除同期は `apply_run` が内包しており、しかも collector 側の
    // コミット後に `remove_stale_broker_sessions` するので、案B が受け入れて
    // いた「最後のグループを失った接続の collect タスクが、削除済みセッションを
    // しばらく読み続けうる」隙間も同時に閉じている。
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::init_db;
    use banto_collect::CollectorOptions;
    use banto_tags::{
        CollectionGroupInput, CollectionGroupService, PlcConnection, PlcConnectionInput, TagInput,
    };
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
        let sessions = Arc::new(HubSessions::new(
            banto_broker::BackoffConfig::default(),
            crate::test_support::test_drivers(),
        ));
        let sim_registry = Arc::new(BrokerSimRegistry::new());
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

    /// 保持期間の削除（#541）のテスト用: 時計を差し替えられる管理役を
    /// [`manager_env`] と同じ形で作る。
    async fn manager_env_with_clock(
        clock: Arc<dyn Clock>,
    ) -> (
        crate::test_support::TempDir,
        CollectorManager,
        sqlx::SqlitePool,
    ) {
        let dir = crate::test_support::TempDir::new("manager-env-clock");
        let db_path = dir.path().join("registry.sqlite3");
        let pool = init_db(&db_path).await.expect("init_db");
        let data_dir = dir.path().join("data");
        let sessions = Arc::new(HubSessions::new(
            banto_broker::BackoffConfig::default(),
            crate::test_support::test_drivers(),
        ));
        let sim_registry = Arc::new(BrokerSimRegistry::new());
        let computed = Arc::new(ComputedEngine::new(Arc::new(ServerTagStore::new())));
        let manager = CollectorManager::new(
            pool.clone(),
            data_dir,
            clock,
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

    /// シミュレーション接続 1 本・有効タグ 1 つを登録する（`rebuild` で収集が
    /// 実際に走る最小構成）。
    async fn seed_running_simulation(pool: &sqlx::SqlitePool) {
        let conn = PlcConnectionService::new(pool.clone())
            .create(PlcConnectionInput {
                name: "prune".to_string(),
                protocol: "modbus-tcp".to_string(),
                host: "127.0.0.1".to_string(),
                port: 15031,
                unit_id: 1,
                enabled: true,
                simulation: true,
                word_order: "low_high".to_string(),
                cpu_type: None,
                database: None,
                username: None,
                password: None,
            })
            .await
            .unwrap();
        let group = CollectionGroupService::new(pool.clone())
            .create(CollectionGroupInput {
                name: "g".to_string(),
                plc_connection_id: conn.id,
                period_ms: 100,
                enabled: true,
                default_writable: true,
                query_sql: None,
            })
            .await
            .unwrap();
        TagService::new(pool.clone())
            .create(TagInput {
                name: "t".to_string(),
                collection_group_id: group.id,
                address: "40001".to_string(),
                data_type: "i16".to_string(),
                string_length: None,
                string_encoding: "utf8".to_string(),
                raw_lo: None,
                raw_hi: None,
                eng_lo: None,
                eng_hi: None,
                unit: None,
                decimals: 0,
                enabled: true,
                writable: false,
                tag_kind: "plc".to_string(),
                expression: None,
                retain: false,
                expected_revision: None,
            })
            .await
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn invalid_plc_bit_metadata_is_stable_across_derived_stale() {
        let clock = Arc::new(banto_tstore::ManualClock::new(1_000, 0));
        let (_dir, manager, pool) = manager_env_with_clock(clock.clone()).await;
        seed_running_simulation(&pool).await;
        manager.rebuild().await.unwrap();
        let collect = manager.current_values().unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if collect.get("tag:1").is_some_and(|sample| {
                    sample
                        .value
                        .is_some_and(|value| value != 0.0 && value != 1.0)
                }) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        manager.shutdown().await;
        let map = manager.tag_map();
        let mut entry = map.iter().next().unwrap().clone();
        entry.data_type = "bit".to_string();
        let store = manager.server_store();
        let raw = collect.get_reading(&entry.tag_key).unwrap();
        assert_eq!(raw.sample.quality, Quality::Good);
        assert_eq!(raw.last_good_ms, Some(raw.sample.ptime_ms));
        let fresh = read_current_view(&entry, Some(&collect), &store, clock.now_ms());
        assert_eq!((fresh.value, fresh.quality), (None, Quality::Bad));
        assert_eq!(fresh.source_time_ms, None);
        assert_eq!(fresh.quality_since_ms, Some(raw.sample.ptime_ms));
        clock.set_now_ms(10_000);
        let stale = collect.get_reading(&entry.tag_key).unwrap();
        assert_eq!(stale.sample.quality, Quality::Stale);
        assert_ne!(stale.quality_since_ms, raw.quality_since_ms);
        assert_eq!(
            read_current_view(&entry, Some(&collect), &store, clock.now_ms()),
            fresh
        );
        clock.set_now_ms(20_000);
        assert_eq!(
            read_current_view(&entry, Some(&collect), &store, clock.now_ms()),
            fresh
        );
        // An older usable receive remains meaningful; an explicit Bad
        // sample also retains its existing quality interval.
        for quality in [Quality::Good, Quality::Stale, Quality::Bad] {
            let view = effective_current_view(
                &entry,
                Some(CurrentReading {
                    sample: CurrentSample {
                        value: Some(2.0),
                        quality,
                        ptime_ms: 1_000,
                    },
                    last_good_ms: Some(500),
                    quality_since_ms: Some(700),
                }),
                20_000,
            );
            assert_eq!(view.source_time_ms, Some(500));
            assert_eq!(
                view.quality_since_ms,
                Some(if quality == Quality::Bad { 700 } else { 1_000 })
            );
        }
    }

    /// Re-enabling an unchanged server value begins a new effective Good
    /// interval, even if no reads or source writes occurred while disabled.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reenabled_server_value_starts_quality_at_catalog_publication() {
        for (catalog_only, collecting) in [(true, false), (false, false), (false, true)] {
            let clock = Arc::new(banto_tstore::ManualClock::new(1_000, 0));
            let (_dir, manager, pool) = manager_env_with_clock(clock.clone()).await;
            seed_running_simulation(&pool).await;
            // Convert the fixture into a valid reserved internal connection.
            sqlx::query("UPDATE plc_connections SET name='mem', protocol='virtual', host='', port=0, simulation=0")
                .execute(&pool).await.unwrap();
            sqlx::query("UPDATE collection_groups SET name='memory'")
                .execute(&pool)
                .await
                .unwrap();
            sqlx::query("UPDATE tags SET tag_kind='internal', address=''")
                .execute(&pool)
                .await
                .unwrap();
            if collecting {
                seed_running_simulation(&pool).await;
            }
            let publish = || async {
                if catalog_only {
                    let snapshot = RegistrySnapshot::load(&pool).await.unwrap();
                    manager.commit_catalog(&snapshot).await.unwrap();
                } else {
                    manager.rebuild().await.unwrap();
                }
            };
            publish().await;
            assert_eq!(
                manager.current_values().is_some(),
                collecting && !catalog_only,
                "fixture must select the intended collector publication branch"
            );
            let store = manager.server_store();
            store.set("tag:1", Some(42.0), Quality::Good, 100);
            let read = || {
                let map = manager.tag_map();
                read_current_view(
                    map.get("mem.memory.t").unwrap(),
                    manager.current_values().as_ref(),
                    &store,
                    clock.now_ms(),
                )
            };
            assert_eq!(read().quality_since_ms, Some(100));
            clock.set_now_ms(2_000);
            sqlx::query("UPDATE tags SET enabled=0 WHERE id=1")
                .execute(&pool)
                .await
                .unwrap();
            publish().await;
            // Deliberately no read during this disabled interval.
            clock.set_now_ms(3_000);
            sqlx::query("UPDATE tags SET enabled=1 WHERE id=1")
                .execute(&pool)
                .await
                .unwrap();
            publish().await;
            let resumed = read();
            assert_eq!(resumed.quality_since_ms, Some(3_000));
            assert_eq!(resumed.source_time_ms, Some(100));
            assert_eq!(resumed.timestamp_ms, 100);
            assert_eq!(resumed.value, Some(42.0));
            clock.set_now_ms(4_000);
            publish().await;
            assert_eq!(
                read(),
                resumed,
                "unchanged catalogs must not move the epoch"
            );
            // Run-mode overrides do not disable internal tags or their cached
            // values, so switching modes must not begin a new quality interval.
            manager
                .apply_run(crate::controller::RunMode::AllSimulation)
                .await
                .unwrap();
            manager
                .apply_run(crate::controller::RunMode::Configured)
                .await
                .unwrap();
            assert_eq!(read(), resumed);
            manager.shutdown().await;
        }
    }

    fn local_today(clock: &dyn Clock) -> banto_tstore::LocalDate {
        banto_tstore::LocalDate::from_epoch_ms(clock.now_ms(), clock.utc_offset_ms())
    }

    /// 収集していないとき（書き手が無い）は、素の `prune_files` で消す
    /// （prune-now の REST も同じ口を通る）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn prune_data_files_without_a_collector_deletes_aged_out_files() {
        let (dir, manager, _pool) = manager_env().await;
        let data_dir = dir.path().join("data");
        std::fs::create_dir_all(&data_dir).unwrap();
        let old = data_dir.join("20200101-001.sqlite3");
        std::fs::write(&old, b"").unwrap();
        let today = local_today(manager.clock().as_ref());

        let report = manager.prune_data_files(7, today).await.expect("prune");
        assert_eq!(report.deleted, vec![old.clone()]);
        assert!(!old.exists());
    }

    /// 収集中の削除は書き手を通して古いファイルを消し、開いている今日の
    /// ファイルは残す。削除が書き手を掴んだまま残らないので、そのあとの
    /// 停止は書き手を閉じ、データディレクトリがすぐ消せる。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn prune_while_running_does_not_defeat_the_writer_close_on_stop() {
        let (dir, manager, pool) = manager_env().await;
        seed_running_simulation(&pool).await;
        manager.rebuild().await.expect("rebuild");
        let data_dir = dir.path().join("data");
        let old = data_dir.join("20200101-001.sqlite3");
        std::fs::write(&old, b"").unwrap();
        let today = local_today(manager.clock().as_ref());

        let report = manager.prune_data_files(7, today).await.expect("prune");
        assert_eq!(report.deleted, vec![old.clone()]);
        assert!(!old.exists());
        assert_eq!(
            banto_tstore::list_data_files(&data_dir).unwrap().len(),
            1,
            "開いている今日のファイルは残る"
        );

        manager.shutdown().await;
        std::fs::remove_dir_all(&data_dir).expect("停止で書き手が閉じられていれば、すぐ消せる");
    }

    /// 書き込みの失敗でファイルの切り替えができず古い日付のまま開いている間は、
    /// 保持日数が小さくてもそのファイルを消さない。回復して切り替わったあとは
    /// 消える。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn prune_while_running_keeps_the_file_held_by_a_failed_rotation() {
        use sqlx::Connection;

        let clock = Arc::new(banto_tstore::ManualClock::new(
            banto_tstore::LocalDate::new(2026, 7, 12).to_days_since_epoch() * 86_400_000
                + 12 * 3_600_000,
            0,
        ));
        let (dir, manager, pool) = manager_env_with_clock(clock.clone()).await;
        seed_running_simulation(&pool).await;
        manager.rebuild().await.expect("rebuild");
        let data_dir = dir.path().join("data");

        let day1 = banto_tstore::list_data_files(&data_dir).unwrap();
        assert_eq!(day1.len(), 1, "収集が今日のファイルを開いている");
        let day1_file = day1[0].path.clone();

        // 書き込み側の INSERT を必ず失敗させる（ディスクフル等の代わり）。
        // トリガーはファイルに付くので、別の接続から付けても書き手に効く。
        let url = format!("sqlite://{}", day1_file.display());
        let mut side = sqlx::SqliteConnection::connect(&url).await.expect("side");
        sqlx::query("PRAGMA busy_timeout = 5000")
            .execute(&mut side)
            .await
            .unwrap();
        sqlx::query(
            "CREATE TRIGGER fail_insert BEFORE INSERT ON samples_1 \
             BEGIN SELECT RAISE(ABORT, 'simulated write failure'); END",
        )
        .execute(&mut side)
        .await
        .expect("create trigger");

        // 時計を進める前に、書き手が 1 日目宛ての未書き込みの行を持つまで待つ。
        // トリガー後に最初の標本が溜まる前に進めると、切り替えの flush が空振りで
        // 成功して 1 日目のファイルが閉じられ、剪定が自前の `side` 接続の掴む
        // ファイルを消そうとして落ちる（Windows CI の os error 32 のフレーク）。
        // `flush` は行があれば失敗し（バッファは減らさない）、無ければ何もせず
        // 成功するので、失敗を待てば「失敗する行を持っている」が確かめられる。
        let writer = manager
            .collector
            .lock()
            .await
            .as_ref()
            .expect("収集が走っている")
            .writer_handle()
            .borrow()
            .clone();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while writer.flush().await.is_ok() {
            assert!(
                std::time::Instant::now() < deadline,
                "1 日目宛ての行が溜まるはず"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        // 3 日進める。日付の切り替え前の flush が失敗し続けるので、書き手は
        // 1 日目のファイルを開いたまま、行を持ち続ける。行を持った後なので、
        // 切り替えがいつ試みられても失敗し、この sleep は試みを待つだけ。
        clock.advance_ms(3 * 86_400_000);
        let today = local_today(clock.as_ref());
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(
            writer.flush().await.is_err(),
            "切り替えに失敗して行を持ったまま"
        );
        drop(writer);

        let report = manager.prune_data_files(1, today).await.expect("prune");
        assert!(report.deleted.is_empty(), "開いている日付は消さない");
        assert!(day1_file.exists());

        // 設定画面のプレビュー（plan_prune_data_files）は、実際の削除と同じ件数。
        // 素の plan_prune は 1 件と数えてしまう（#541 の P2）。
        let plan = manager.plan_prune_data_files(1, today).await.expect("plan");
        assert!(
            plan.deleted.is_empty(),
            "プレビューも開いている日付は数えない"
        );
        assert_eq!(
            banto_tstore::plan_prune(&data_dir, 1, today)
                .expect("plain plan")
                .deleted
                .len(),
            1
        );
        assert!(day1_file.exists(), "プレビューは消さない");

        // 回復 -> 切り替えが成功して 1 日目のファイルは閉じられ、消せる。
        sqlx::query("DROP TRIGGER fail_insert")
            .execute(&mut side)
            .await
            .expect("drop trigger");
        side.close().await.unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        loop {
            let report = manager.prune_data_files(1, today).await.expect("prune");
            if report.deleted.contains(&day1_file) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "回復して切り替わったあとは消せるはず"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(!day1_file.exists());

        manager.shutdown().await;
        std::fs::remove_dir_all(&data_dir).expect("停止で書き手が閉じられていれば、すぐ消せる");
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

    #[test]
    fn all_simulation_overrides_only_enabled_physical_connections() {
        let snapshot = RegistrySnapshot {
            connections: vec![
                PlcConnection {
                    id: 1,
                    name: "modbus".to_string(),
                    protocol: "modbus-tcp".to_string(),
                    host: "127.0.0.1".to_string(),
                    port: 502,
                    unit_id: 1,
                    enabled: true,
                    simulation: false,

                    word_order: "low_high".to_string(),
                    cpu_type: "R".to_string(),
                    database: None,
                    username: None,
                    password: None,
                },
                PlcConnection {
                    id: 2,
                    name: "slmp".to_string(),
                    protocol: "slmp".to_string(),
                    host: "127.0.0.1".to_string(),
                    port: 5007,
                    unit_id: 1,
                    enabled: true,
                    simulation: false,

                    word_order: "low_high".to_string(),
                    cpu_type: "R".to_string(),
                    database: None,
                    username: None,
                    password: None,
                },
                PlcConnection {
                    id: 3,
                    name: "disabled".to_string(),
                    protocol: "modbus-tcp".to_string(),
                    host: "127.0.0.1".to_string(),
                    port: 502,
                    unit_id: 1,
                    enabled: false,
                    simulation: false,

                    word_order: "low_high".to_string(),
                    cpu_type: "R".to_string(),
                    database: None,
                    username: None,
                    password: None,
                },
                PlcConnection {
                    id: 4,
                    name: "virtual".to_string(),
                    protocol: "virtual".to_string(),
                    host: String::new(),
                    port: 0,
                    unit_id: 0,
                    enabled: true,
                    simulation: false,

                    word_order: "low_high".to_string(),
                    cpu_type: "R".to_string(),
                    database: None,
                    username: None,
                    password: None,
                },
            ],
            groups: Vec::new(),
            tags: Vec::new(),
        };

        let configured =
            runtime_snapshot_for_mode(&snapshot, crate::controller::RunMode::Configured);
        assert_eq!(configured, snapshot);

        let all_simulation =
            runtime_snapshot_for_mode(&snapshot, crate::controller::RunMode::AllSimulation);
        assert!(!snapshot.connections[0].simulation);
        assert!(!snapshot.connections[1].simulation);
        assert!(all_simulation.connections[0].simulation);
        assert!(all_simulation.connections[1].simulation);
        assert!(!all_simulation.connections[2].simulation);
        assert!(!all_simulation.connections[3].simulation);
    }

    /// 2026-08-31 オーナー決定「タグ名の一意性を全体一意→収集グループ内一意へ
    /// 緩和」の回帰防止: `tags.name` が2つのグループで重複していても、
    /// `external_name` は `{connection}.{group}.{tag}` の合成のまま従来どおり
    /// 組み立てられ、かつグループをまたいで衝突しないこと -
    /// 「`plc_connections.name` はグローバルに `UNIQUE`、グループ名は接続内で
    /// `UNIQUE`（0022）だから、同じタグ名でもグループが違えば外部名は衝突しない」
    /// ことを、実際に `build_catalog_from` を通して確かめる（名前にドットを
    /// 含むときの重なりは #635、`build_catalog_from_detects_an_existing_dotted_name_collision`）。
    /// DB を経由しない純粋関数テスト
    /// (`all_simulation_overrides_only_enabled_physical_connections` と同じ
    /// スタイル)。
    #[test]
    fn build_catalog_from_assembles_distinct_external_names_for_a_shared_tag_name_across_groups() {
        let conn1 = PlcConnection {
            id: 1,
            name: "line1".to_string(),
            protocol: "modbus-tcp".to_string(),
            host: "127.0.0.1".to_string(),
            port: 502,
            unit_id: 1,
            enabled: true,
            simulation: false,

            word_order: "low_high".to_string(),
            cpu_type: "R".to_string(),
            database: None,
            username: None,
            password: None,
        };
        let conn2 = PlcConnection {
            id: 2,
            name: "line2".to_string(),
            protocol: "modbus-tcp".to_string(),
            host: "127.0.0.2".to_string(),
            port: 502,
            unit_id: 1,
            enabled: true,
            simulation: false,

            word_order: "low_high".to_string(),
            cpu_type: "R".to_string(),
            database: None,
            username: None,
            password: None,
        };
        let group1 = banto_tags::CollectionGroup {
            id: 10,
            name: "fast".to_string(),
            plc_connection_id: conn1.id,
            period_ms: 1_000,
            enabled: true,
            default_writable: true,
            query_sql: None,
        };
        let group2 = banto_tags::CollectionGroup {
            id: 20,
            name: "slow".to_string(),
            plc_connection_id: conn2.id,
            period_ms: 1_000,
            enabled: true,
            default_writable: true,
            query_sql: None,
        };
        let tag_in_group1 = Tag {
            id: 100,
            name: "D100".to_string(),
            collection_group_id: group1.id,
            address: "D100".to_string(),
            data_type: "i16".to_string(),
            string_length: None,
            string_encoding: "utf8".to_string(),
            raw_lo: None,
            raw_hi: None,
            eng_lo: None,
            eng_hi: None,
            unit: None,
            decimals: 0,
            enabled: true,
            writable: false,
            tag_kind: "plc".to_string(),
            expression: None,
            retain: false,
            revision: 1,
        };
        // Same `name` ("D100"), a different collection group - exactly the
        // "two identical PLCs, same PLC address" scenario migration 0011
        // exists to unblock.
        let tag_in_group2 = Tag {
            id: 200,
            collection_group_id: group2.id,
            ..tag_in_group1.clone()
        };

        let snapshot = RegistrySnapshot {
            connections: vec![conn1, conn2],
            groups: vec![group1, group2],
            tags: vec![tag_in_group1, tag_in_group2],
        };

        let map = build_catalog_from(&snapshot).expect("build_catalog_from");
        assert_eq!(map.iter().count(), 2, "both same-named tags must appear");

        let entry1 = map.get("line1.fast.D100").expect("line1.fast.D100");
        assert_eq!(entry1.ids, (1, 10, 100));
        let entry2 = map.get("line2.slow.D100").expect("line2.slow.D100");
        assert_eq!(entry2.ids, (2, 20, 200));
        assert_ne!(
            entry1.external_name, entry2.external_name,
            "external names must not collide across groups"
        );
    }

    /// #635 のテスト用: 名前だけ違う接続・グループ・タグで snapshot を組む。
    /// `tags` は `(タグ ID, グループ ID, 名前)`。
    fn named_snapshot(
        connections: &[(i64, &str)],
        groups: &[(i64, i64, &str)],
        tags: &[(i64, i64, &str)],
    ) -> RegistrySnapshot {
        RegistrySnapshot {
            connections: connections
                .iter()
                .map(|&(id, name)| PlcConnection {
                    id,
                    name: name.to_string(),
                    protocol: "modbus-tcp".to_string(),
                    host: "127.0.0.1".to_string(),
                    port: 502,
                    unit_id: 1,
                    enabled: true,
                    simulation: false,
                    word_order: "low_high".to_string(),
                    cpu_type: "R".to_string(),
                    database: None,
                    username: None,
                    password: None,
                })
                .collect(),
            groups: groups
                .iter()
                .map(|&(id, conn, name)| banto_tags::CollectionGroup {
                    id,
                    name: name.to_string(),
                    plc_connection_id: conn,
                    period_ms: 1_000,
                    enabled: true,
                    default_writable: true,
                    query_sql: None,
                })
                .collect(),
            tags: tags
                .iter()
                .map(|&(id, group, name)| Tag {
                    id,
                    name: name.to_string(),
                    collection_group_id: group,
                    address: "40001".to_string(),
                    data_type: "i16".to_string(),
                    string_length: None,
                    string_encoding: "utf8".to_string(),
                    raw_lo: None,
                    raw_hi: None,
                    eng_lo: None,
                    eng_hi: None,
                    unit: None,
                    decimals: 0,
                    enabled: true,
                    writable: false,
                    tag_kind: "plc".to_string(),
                    expression: None,
                    retain: false,
                    revision: 1,
                })
                .collect(),
        }
    }

    /// 2026-10-19 オーナー決定（migration 0022、グループ名は接続内で一意）の
    /// 回帰防止: 2 つの接続がどちらも「fast」グループを持ち、その下に同じ名前の
    /// タグがあっても、外部名は接続名で分かれ、重なり（#635）として数えない。
    #[test]
    fn build_catalog_from_keeps_same_named_groups_under_two_connections_apart() {
        let snapshot = named_snapshot(
            &[(1, "line1"), (2, "line2")],
            &[(10, 1, "fast"), (20, 2, "fast")],
            &[(100, 10, "D100"), (200, 20, "D100")],
        );
        let map = build_catalog_from(&snapshot).expect("build_catalog_from");
        assert_eq!(map.len(), 2);
        assert_eq!(map.get("line1.fast.D100").expect("line1").ids, (1, 10, 100));
        assert_eq!(map.get("line2.fast.D100").expect("line2").ids, (2, 20, 200));
        assert!(map.collisions().is_empty());
        assert!(external_name_collisions(&snapshot).is_empty());
    }

    /// #635: 接続「a.b」/グループ「c」/タグ「d」と 接続「a」/グループ「b.c」/タグ「d」は
    /// どちらも外部名 `a.b.c.d`。既にこう重なっているデータでも catalog は作れ、
    /// 重なりを検出して警告の行を出す。一覧（`iter`・`len`）には外部名ごとに 1 回だけ
    /// 出し、外部名で引けるのは catalog 順で最後のタグ。
    #[test]
    fn build_catalog_from_detects_an_existing_dotted_name_collision() {
        let snapshot = named_snapshot(
            &[(1, "a.b"), (2, "a")],
            &[(10, 1, "c"), (20, 2, "b.c")],
            &[(100, 10, "d"), (200, 20, "d"), (300, 20, "e")],
        );

        let map = build_catalog_from(&snapshot).expect("build_catalog_from");
        assert_eq!(map.len(), 2, "a.b.c.d と a.b.c.e");
        let listed: Vec<_> = map
            .iter()
            .map(|entry| entry.external_name.as_str())
            .collect();
        assert_eq!(listed, ["a.b.c.d", "a.b.c.e"], "重なった名前は 1 回だけ");
        assert_eq!(map.get("a.b.c.d").expect("a.b.c.d").ids, (1, 10, 100));

        let collisions = map.collisions();
        assert_eq!(collisions.len(), 1);
        assert_eq!(collisions, external_name_collisions(&snapshot).as_slice());
        let collision = &collisions[0];
        assert_eq!(collision.external_name, "a.b.c.d");
        let ids: Vec<_> = collision.tags.iter().map(|tag| tag.ids).collect();
        assert_eq!(
            ids,
            [(2, 20, 200), (1, 10, 100)],
            "catalog 順（接続名 a < a.b）"
        );

        let line = collision.warning_line();
        assert!(
            line.starts_with("[WARN] banto-hub: 外部名「a.b.c.d」"),
            "{line}"
        );
        assert!(
            line.contains(
                "接続「a」/ グループ「b.c」/ タグ「d」（タグ ID 200） は外部名で引けません"
            ),
            "{line}"
        );
        assert!(
            line.contains(
                "外部名は 接続「a.b」/ グループ「c」/ タグ「d」（タグ ID 100） を指します"
            ),
            "{line}"
        );
    }

    /// #635: 重なりの無い catalog は `collisions` が空。
    #[test]
    fn build_catalog_from_reports_no_collision_for_distinct_names() {
        let snapshot = named_snapshot(
            &[(1, "a.b"), (2, "a")],
            &[(10, 1, "c"), (20, 2, "b.x")],
            &[(100, 10, "d"), (200, 20, "d")],
        );
        let map = build_catalog_from(&snapshot).expect("build_catalog_from");
        assert!(map.collisions().is_empty());
        assert_eq!(map.len(), 2);
    }

    /// #635 案 A: 作成・改名（タグ・グループ・接続、グループの付け替え）で新しく
    /// 生まれる重なりを返し、既存のタグを示す。既にあった重なりは数えない
    /// （無関係な変更・重なりを解く変更を塞がない）。
    #[test]
    fn new_external_name_collisions_reports_only_collisions_the_write_creates() {
        let base_conns: &[(i64, &str)] = &[(1, "a.b"), (2, "a")];
        let base_groups: &[(i64, i64, &str)] = &[(10, 1, "c"), (20, 2, "b.c"), (30, 2, "x")];
        let before = named_snapshot(base_conns, base_groups, &[(100, 10, "d"), (200, 20, "e")]);

        // タグの作成
        let created = named_snapshot(
            base_conns,
            base_groups,
            &[(100, 10, "d"), (200, 20, "e"), (300, 20, "d")],
        );
        let new = new_external_name_collisions(&before, &created);
        assert_eq!(new.len(), 1);
        assert_eq!(new[0].external_name, "a.b.c.d");
        assert_eq!(
            new[0]
                .introduced
                .iter()
                .map(|t| t.ids.2)
                .collect::<Vec<_>>(),
            [300]
        );
        assert_eq!(
            new[0].existing.iter().map(|t| t.ids.2).collect::<Vec<_>>(),
            [100]
        );
        let message = new[0].message();
        assert!(
            message.contains("外部名「a.b.c.d」が既存のタグ（接続「a.b」/ グループ「c」/ タグ「d」（タグ ID 100））と重なります"),
            "{message}"
        );

        // タグの改名
        let renamed = named_snapshot(base_conns, base_groups, &[(100, 10, "d"), (200, 20, "d")]);
        let new = new_external_name_collisions(&before, &renamed);
        assert_eq!(new.len(), 1);
        assert_eq!(new[0].introduced[0].ids.2, 200);

        // タグのグループの付け替え（x → b.c）
        let before_move =
            named_snapshot(base_conns, base_groups, &[(100, 10, "d"), (200, 30, "d")]);
        let new = new_external_name_collisions(&before_move, &renamed);
        assert_eq!(new.len(), 1, "付け替えも外部名が変わる");

        // グループの改名（x → b.c は名前が使用中なので、b.c の無い構成で）
        let before_group = named_snapshot(
            base_conns,
            &[(10, 1, "c"), (30, 2, "x")],
            &[(100, 10, "d"), (200, 30, "d")],
        );
        let after_group = named_snapshot(
            base_conns,
            &[(10, 1, "c"), (30, 2, "b.c")],
            &[(100, 10, "d"), (200, 30, "d")],
        );
        assert_eq!(
            new_external_name_collisions(&before_group, &after_group).len(),
            1
        );

        // 接続の改名（m → a）
        let before_conn = named_snapshot(
            &[(1, "a.b"), (2, "m")],
            &[(10, 1, "c"), (20, 2, "b.c")],
            &[(100, 10, "d"), (200, 20, "d")],
        );
        let after_conn = named_snapshot(
            &[(1, "a.b"), (2, "a")],
            &[(10, 1, "c"), (20, 2, "b.c")],
            &[(100, 10, "d"), (200, 20, "d")],
        );
        assert_eq!(
            new_external_name_collisions(&before_conn, &after_conn).len(),
            1
        );

        // 同じ書き込みで作るタグどうしの重なり（一括作成）は、既存が無いと示す。
        let empty = named_snapshot(base_conns, base_groups, &[]);
        let new = new_external_name_collisions(&empty, &renamed);
        assert_eq!(new.len(), 1);
        assert!(new[0].existing.is_empty());
        assert!(new[0]
            .message()
            .contains("この変更で作る・変えるタグどうし"));

        // 既にあった重なりは数えない: 無関係なタグの追加・既存の重なりの解消。
        let unrelated = named_snapshot(
            base_conns,
            base_groups,
            &[(100, 10, "d"), (200, 20, "d"), (400, 30, "z")],
        );
        assert!(new_external_name_collisions(&renamed, &unrelated).is_empty());
        assert!(new_external_name_collisions(&renamed, &before).is_empty());
        // 既に重なっているタグに 3 つ目（接続「a」/ グループ「b」/ タグ「c.d」）を
        // 足すのは新しい重なり。既存の 2 つを示す。
        let third = named_snapshot(
            base_conns,
            &[(10, 1, "c"), (20, 2, "b.c"), (50, 2, "b")],
            &[(100, 10, "d"), (200, 20, "d"), (600, 50, "c.d")],
        );
        let new = new_external_name_collisions(&renamed, &third);
        assert_eq!(new.len(), 1);
        assert_eq!(
            new[0]
                .introduced
                .iter()
                .map(|t| t.ids.2)
                .collect::<Vec<_>>(),
            [600]
        );
        assert_eq!(
            new[0].existing.iter().map(|t| t.ids.2).collect::<Vec<_>>(),
            [200, 100]
        );
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

                word_order: "low_high".to_string(),
                cpu_type: None,
                database: None,
                username: None,
                password: None,
            })
            .await
            .unwrap();
        let group = CollectionGroupService::new(pool.clone())
            .create(CollectionGroupInput {
                name: "fast".to_string(),
                plc_connection_id: conn.id,
                period_ms: 100,
                enabled: true,
                default_writable: true,
                query_sql: None,
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
                string_encoding: "utf8".to_string(),
                raw_lo: None,
                raw_hi: None,
                eng_lo: None,
                eng_hi: None,
                unit: None,
                decimals: 0,
                enabled: false,
                writable: false,
                tag_kind: "plc".to_string(),
                expression: None,
                retain: false,
                expected_revision: None,
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

    /// #635: 既に外部名が重なっている DB（2026-10-17 の検証より前に書いた行）でも
    /// catalog の再構築は失敗しない（起動を止めない、オーナー決定）。重なりは
    /// catalog に載り、警告の行に出る（`warn_external_name_collisions`）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rebuild_keeps_running_with_an_existing_external_name_collision() {
        let (_dir, manager, pool) = manager_env().await;
        let mut ids = Vec::new();
        for (conn_name, group_name, port) in [("a.b", "c", 15024), ("a", "b.c", 15025)] {
            let conn = PlcConnectionService::new(pool.clone())
                .create(PlcConnectionInput {
                    name: conn_name.to_string(),
                    protocol: "modbus-tcp".to_string(),
                    host: "127.0.0.1".to_string(),
                    port,
                    unit_id: 1,
                    enabled: true,
                    simulation: false,
                    word_order: "low_high".to_string(),
                    cpu_type: None,
                    database: None,
                    username: None,
                    password: None,
                })
                .await
                .unwrap();
            let group = CollectionGroupService::new(pool.clone())
                .create(CollectionGroupInput {
                    name: group_name.to_string(),
                    plc_connection_id: conn.id,
                    period_ms: 100,
                    enabled: true,
                    default_writable: true,
                    query_sql: None,
                })
                .await
                .unwrap();
            // サービスは外部名を知らないので、検証より前の行と同じく重なりを書ける。
            let tag = TagService::new(pool.clone())
                .create(TagInput {
                    name: "d".to_string(),
                    collection_group_id: group.id,
                    address: "40001".to_string(),
                    data_type: "i16".to_string(),
                    string_length: None,
                    string_encoding: "utf8".to_string(),
                    raw_lo: None,
                    raw_hi: None,
                    eng_lo: None,
                    eng_hi: None,
                    unit: None,
                    decimals: 0,
                    enabled: false,
                    writable: false,
                    tag_kind: "plc".to_string(),
                    expression: None,
                    retain: false,
                    expected_revision: None,
                })
                .await
                .unwrap();
            ids.push(tag.id);
        }

        manager
            .rebuild()
            .await
            .expect("rebuild must not fail on a collision");
        let map = manager.tag_map();
        assert_eq!(map.len(), 1);
        assert_eq!(map.collisions().len(), 1);
        let hidden = &map.collisions()[0].tags[0];
        assert_eq!((hidden.connection.as_str(), hidden.ids.2), ("a", ids[1]));
        manager.shutdown().await;
    }

    /// T19 S2-a (UX-48, docs/banto-hub-t19-design.md §3.8): a connection
    /// with zero groups at all gets no broker session pre-synced - the same
    /// "reading nothing from a PLC is pointless" rule `banto_collect`
    /// already applied to its own task/socket, now shared with the
    /// write-side session sync via
    /// `banto_collect::connections_with_collected_groups`. Before this
    /// change, `manager.sessions().connection_ids()` would have contained
    /// `conn.id` and `broker_status` would have returned
    /// `Some(BrokerConnectionStatus::Reconnecting { .. })` forever - the
    /// "registered but still empty connection stuck in reconnecting"
    /// grievance this slice fixes.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rebuild_does_not_sync_a_broker_session_for_a_tagless_connection() {
        let (_dir, manager, pool) = manager_env().await;

        let conn = PlcConnectionService::new(pool.clone())
            .create(PlcConnectionInput {
                name: "tagless".to_string(),
                protocol: "modbus-tcp".to_string(),
                host: "127.0.0.1".to_string(),
                port: 15021,
                unit_id: 1,
                enabled: true,
                simulation: false,

                word_order: "low_high".to_string(),
                cpu_type: None,
                database: None,
                username: None,
                password: None,
            })
            .await
            .unwrap();

        manager.rebuild().await.expect("rebuild should be Ok");

        assert!(
            !manager.sessions().connection_ids().contains(&conn.id),
            "a connection with no collection group must not get a broker session"
        );
        assert_eq!(
            manager.broker_status(conn.id),
            None,
            "no session was ever synced, so there is nothing to report status for"
        );
        // T15-4: the write path only peeks an already-synced session and
        // never dials on demand (see `CollectorManager::write_broker_handle_peek`'s
        // doc comment) - so a write targeting a tag under this connection
        // would fail closed (`WriteRejection::WriteFailed`) at gate 9, not
        // succeed via an on-demand session spawn.
        assert!(
            manager.write_broker_handle_peek(conn.id).is_none(),
            "write path must fail closed - it never spawns a session on demand"
        );

        manager.shutdown().await;
    }

    /// Regression guard for the flip side of
    /// `rebuild_does_not_sync_a_broker_session_for_a_tagless_connection`: a
    /// connection with an enabled group and an enabled tag keeps getting its
    /// broker session pre-synced exactly as before T19 S2-a.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rebuild_still_syncs_a_broker_session_for_a_connection_with_a_tag() {
        let (_dir, manager, pool) = manager_env().await;

        let conn = PlcConnectionService::new(pool.clone())
            .create(PlcConnectionInput {
                name: "tagged".to_string(),
                protocol: "modbus-tcp".to_string(),
                host: "127.0.0.1".to_string(),
                port: 15022,
                unit_id: 1,
                enabled: true,
                simulation: false,

                word_order: "low_high".to_string(),
                cpu_type: None,
                database: None,
                username: None,
                password: None,
            })
            .await
            .unwrap();
        let group = CollectionGroupService::new(pool.clone())
            .create(CollectionGroupInput {
                name: "g1".to_string(),
                plc_connection_id: conn.id,
                period_ms: 1_000,
                enabled: true,
                default_writable: true,
                query_sql: None,
            })
            .await
            .unwrap();
        TagService::new(pool.clone())
            .create(TagInput {
                name: "t1".to_string(),
                collection_group_id: group.id,
                address: "40001".to_string(),
                data_type: "i16".to_string(),
                string_length: None,
                string_encoding: "utf8".to_string(),
                raw_lo: None,
                raw_hi: None,
                eng_lo: None,
                eng_hi: None,
                unit: None,
                decimals: 0,
                enabled: true,
                writable: true,
                tag_kind: "plc".to_string(),
                expression: None,
                retain: false,
                expected_revision: None,
            })
            .await
            .unwrap();

        manager.rebuild().await.expect("rebuild should be Ok");

        assert!(
            manager.sessions().connection_ids().contains(&conn.id),
            "a connection with a tag must still get a broker session, as before T19 S2-a"
        );
        assert!(
            manager.broker_status(conn.id).is_some(),
            "a synced session always has SOME status (at least Reconnecting), never None"
        );
        assert!(
            manager.write_broker_handle_peek(conn.id).is_some(),
            "the pre-synced session must be immediately writable without an on-demand dial"
        );

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

                word_order: "low_high".to_string(),
                cpu_type: None,
                database: None,
                username: None,
                password: None,
            })
            .await
            .unwrap();
        let group = CollectionGroupService::new(pool.clone())
            .create(CollectionGroupInput {
                name: "fast".to_string(),
                plc_connection_id: conn.id,
                period_ms: 100,
                enabled: true,
                default_writable: true,
                query_sql: None,
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
                string_encoding: "utf8".to_string(),
                raw_lo: None,
                raw_hi: None,
                eng_lo: None,
                eng_hi: None,
                unit: None,
                decimals: 0,
                enabled: true,
                writable: false,
                tag_kind: "plc".to_string(),
                expression: None,
                retain: false,
                expected_revision: None,
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

                word_order: "low_high".to_string(),
                cpu_type: None,
                database: None,
                username: None,
                password: None,
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

    /// #616: シミュレーションの注意を出すかの表（純関数）。
    #[test]
    fn sim_warning_due_table() {
        let text = "注意 A";
        type Last = Option<(String, i64)>;
        let last = |t: &str, at: i64| Some((t.to_string(), at));
        let cases: Vec<(&str, Last, i64, bool)> = vec![
            (
                "前に出していない（起動直後・再び有効にした）",
                None,
                0,
                true,
            ),
            ("同じ文面・すぐ", last(text, 1_000), 1_000, false),
            (
                "同じ文面・間隔の直前",
                last(text, 1_000),
                1_000 + SIM_WARNING_REPEAT_MS - 1,
                false,
            ),
            (
                "同じ文面・間隔を過ぎた（永久には抑えない）",
                last(text, 1_000),
                1_000 + SIM_WARNING_REPEAT_MS,
                true,
            ),
            ("文面が変わった", last("注意 B", 1_000), 1_000, true),
            ("時計が戻った", last(text, 5_000), 1_000, true),
        ];
        for (name, last, now, expected) in cases {
            assert_eq!(
                sim_warning_due(last.as_ref(), text, now),
                expected,
                "{name}"
            );
        }
    }

    fn sim_input(simulation: bool) -> PlcConnectionInput {
        PlcConnectionInput {
            name: "simline".to_string(),
            protocol: "modbus-tcp".to_string(),
            host: "127.0.0.1".to_string(),
            port: 15024,
            unit_id: 1,
            enabled: true,
            simulation,
            word_order: "low_high".to_string(),
            cpu_type: None,
            database: None,
            username: None,
            password: None,
        }
    }

    /// #616: 設定の 1 回の変更で `rebuild`・`apply_run` が続けて呼ばれても、同じ注意は 1 回
    /// だけ。間隔を過ぎた後・シミュレーションの接続を無くしてから再び有効にした後は、もう一度
    /// 出す。
    // `crate::test_support`'s module doc: `TempDir::drop`'s retry needs a
    // multi-thread runtime.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_simulation_warning_is_not_repeated_for_the_same_change() {
        let clock = Arc::new(banto_tstore::ManualClock::new(1_000, 0));
        let (_dir, manager, pool) = manager_env_with_clock(clock.clone()).await;
        let lines: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured = lines.clone();
        let manager = manager.with_diag_log(DiagLog::new(
            move |msg| captured.lock().unwrap().push(msg.to_string()),
            |_msg| {},
        ));
        let count = || {
            lines
                .lock()
                .unwrap()
                .iter()
                .filter(|line| line.contains("シミュレーションモードの接続"))
                .count()
        };
        let service = PlcConnectionService::new(pool.clone());
        let created = service.create(sim_input(true)).await.unwrap();

        for _ in 0..3 {
            manager.rebuild().await.expect("rebuild should be Ok");
        }
        assert_eq!(count(), 1, "同じ変更で重複した");

        clock.advance_ms(SIM_WARNING_REPEAT_MS);
        manager.rebuild().await.expect("rebuild should be Ok");
        assert_eq!(count(), 2, "間隔を過ぎたらもう一度出す");

        // シミュレーションを外すと忘れ、再び有効にしたら（間隔の中でも）出す。
        service.update(created.id, sim_input(false)).await.unwrap();
        manager.rebuild().await.expect("rebuild should be Ok");
        service.update(created.id, sim_input(true)).await.unwrap();
        manager.rebuild().await.expect("rebuild should be Ok");
        assert_eq!(count(), 3, "再び有効にしたら出す");

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
