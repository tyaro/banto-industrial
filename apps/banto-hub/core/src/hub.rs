//! Hub の中核: [`CollectorManager`]（Collector のライフサイクル管理、設計
//! §3.2/§4.3）と [`TagMap`]（外部名 catalog のスナップショット、設計
//! §4/§4.1）。
//!
//! ## `CollectorManager`: T0 は「全体再構築」（設計 §4.3 最終段落）
//!
//! T0 は接続単位の部分再構成（I7、設計 §4.3 表の (c)）を実装せず、
//! レジストリが変わるたびに `banto_collect::Collector` をまるごと作り直す
//! （ChronoGazer と同型）。外部契約（`revision` + `last_error`、設計 §4.1）が
//! 実装差を吸収するので、I7 を後から入れてもクライアント非互換にならない
//! — 外部から見える差は「変更時に他接続の値まで一瞬 Bad になるか否か」だけ
//! （設計 §4.3）。
//!
//! ## all-or-nothing の実現（設計 §4.3 最終段落）
//!
//! [`CollectorManager::rebuild`] は `build_config`（純粋な読み取り、
//! 副作用なし）→ `Collector::start`（新しい方を先に起動）→ 成功したら
//! 旧 `Collector` を stop、という順序を守る。**新しい方を先に起動する**のが
//! 肝: `build_config` は非同期リストア取得のみで失敗しても何も変更しない
//! （不変条件は自動的に守られる）が、`Collector::start` の失敗
//! （tstore を開けない等）まで「旧 Collector と旧 TagMap を維持」を保証
//! するには、旧 Collector を stop する前に新 Collector の起動成功を
//! 確認しなければならない。
//!
//! **既知の制約（T0 で未解決 — 完了報告に記載）**: 上記の順序により、
//! レジストリが同じ接続を指したまま構成が変わった場合（例: 既存タグの
//! 追加編集）、新旧 `Collector` が短時間だけ同じ PLC へ同時にソケットを
//! 張る瞬間がありうる（設計冒頭の「PLC セッションの重複」問題そのもの）。
//! 接続単位の部分再構成（I7、T7）が入るまでの間、T0 の「全体再構築」方式に
//! 内在する制約として許容する。
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

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tokio::sync::Mutex as AsyncMutex;

use banto_collect::{
    build_config, Collector, CollectorOptions, ConnectionStatus, CurrentValuesHandle, EventSink,
};
use banto_core::ListParams;
use banto_tags::{CollectionGroupService, PlcConnectionService, Tag, TagService};
use banto_tstore::Clock;
use serde::Serialize;
use sqlx::SqlitePool;

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
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TagEntry {
    pub external_name: String,
    pub tag_key: String,
    /// Stable `(connection_id, group_id, tag_id)` - the "同じ ID なら
    /// リネームされた/消えたら削除された" signal design §4.1 calls for.
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
}

/// Build a fresh [`TagMap`] straight from the registry (I1), independent of
/// [`build_config`] - the catalog must show *every* tag, enabled or not
/// (design §4: "欠測を隠さない"), while `build_config` deliberately only
/// resolves the enabled/reachable subset it will actually collect. A tag
/// whose group or connection row cannot be found (should not happen - both
/// are `NOT NULL REFERENCES ... ON DELETE RESTRICT` - but defensive against
/// a future relaxation) is skipped rather than panicking.
async fn build_catalog(pool: &SqlitePool) -> Result<TagMap, banto_core::BantoError> {
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

    let conn_by_id: HashMap<i64, _> = connections.iter().map(|c| (c.id, c)).collect();
    let group_by_id: HashMap<i64, _> = groups.iter().map(|g| (g.id, g)).collect();

    let mut entries: Vec<TagEntry> = Vec::with_capacity(tags.len());
    for tag in &tags {
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

/// Mutable state behind [`CollectorManager`]'s lock: the currently running
/// `Collector` (`None` when nothing is enabled to collect - a normal state,
/// not an error, see [`CollectorManager::rebuild`]), the current catalog
/// snapshot, the generation counter, and the last rebuild failure (if any).
struct Inner {
    collector: Option<Collector>,
    map: Arc<TagMap>,
    revision: u64,
    last_error: Option<String>,
}

/// Owns the running [`Collector`]'s lifecycle end to end (design §3.2 table
/// "構成変更の扱いも banto-collect の司令塔決定に従う"): start once at boot,
/// rebuild from scratch on every registry write, expose the read handles the
/// REST layer needs (`current_values`/`connection_status`/`tag_map`/
/// `revision`/`last_error`).
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
    inner: Mutex<Inner>,
    /// Serializes the whole body of [`CollectorManager::rebuild`] - see this
    /// module's doc comment ("`rebuild` は直列化されている") for why a plain
    /// `inner` lock alone is not enough (it is only held across the short
    /// commit step, not across the registry read that precedes it).
    rebuild_lock: AsyncMutex<()>,
}

impl CollectorManager {
    /// `clock` is shared with the store (rotation) and the current-value
    /// cache (staleness) - pass `Arc::new(SystemClock)` in production, a
    /// `ManualClock` in tests, same contract as `Collector::start`.
    pub fn new(
        pool: SqlitePool,
        data_dir: PathBuf,
        clock: Arc<dyn Clock>,
        options: CollectorOptions,
    ) -> Self {
        let events = EventSink::new(pool.clone());
        Self {
            pool,
            data_dir,
            clock,
            options,
            events,
            inner: Mutex::new(Inner {
                collector: None,
                map: Arc::new(TagMap::empty()),
                revision: 0,
                last_error: None,
            }),
            rebuild_lock: AsyncMutex::new(()),
        }
    }

    /// The shared registry pool - handed to callers (e.g. `rest.rs`'s
    /// `/api/v1/status` handler, which needs connection names the catalog
    /// alone does not carry for a connection with zero tags) rather than
    /// duplicated.
    pub fn pool(&self) -> SqlitePool {
        self.pool.clone()
    }

    /// The shared clock (design: 値のタイムスタンプはサンプル取得時刻 - `rest.rs`
    /// uses this for the `/api/v1/values` snapshot's own `t` field and for a
    /// sample-less tag's fallback timestamp).
    pub fn clock(&self) -> Arc<dyn Clock> {
        self.clock.clone()
    }

    /// Rebuild the catalog and the `Collector` from the current registry
    /// state (design §4.3: T0's "全体再構築"). Called once at boot and after
    /// every I1 CRUD write that succeeds.
    ///
    /// On success: `revision` advances by exactly 1, `last_error` clears,
    /// and the new catalog/`Collector` (or no `Collector` at all, if nothing
    /// is enabled - see below) are live.
    ///
    /// On failure (registry read error, a config-level problem
    /// `build_config` catches - e.g. an unparsable address - or a
    /// `Collector::start` lifecycle failure): the OLD catalog and OLD
    /// `Collector` are left completely untouched, `revision` does not
    /// advance, and `last_error` is set to the failure message. The caller
    /// (an I1 CRUD handler) must NOT treat this `Err` as its own failure -
    /// the write itself already succeeded; only the collector's view is
    /// stale until the registry is fixed and rebuilt again (design's T0-1
    /// instructions: "rebuild 失敗は CRUD 自体の失敗にしない").
    ///
    /// A registry with nothing enabled (`build_config` returns zero
    /// connections) is NOT a failure: `Collector::start` itself refuses to
    /// run with zero connections, so this stops any previously-running
    /// `Collector`, commits an empty (or whatever it resolves to) catalog,
    /// and still advances `revision` - a legitimate "collecting nothing"
    /// state, not an error (design's instructions: "タグが0件でも正常起動")．
    ///
    /// **Serialized**: concurrent callers queue on `rebuild_lock` and run one
    /// at a time, each reading the registry fresh after acquiring the lock -
    /// see this module's doc comment ("`rebuild` は直列化されている") for why
    /// this matters (without it, two racing rebuilds could commit
    /// out of order and leave `revision` advanced but the catalog/`Collector`
    /// reflecting a stale registry read).
    pub async fn rebuild(&self) -> Result<(), String> {
        let _guard = self.rebuild_lock.lock().await;

        let new_map = match build_catalog(&self.pool).await {
            Ok(map) => map,
            Err(err) => {
                let message = format!("catalog の読み取りに失敗しました: {err}");
                self.set_last_error(message.clone());
                return Err(message);
            }
        };

        let config = match build_config(&self.pool).await {
            Ok(config) => config,
            Err(err) => {
                let message = err.to_string();
                self.set_last_error(message.clone());
                return Err(message);
            }
        };

        // `CollectorConfig`'s internals are `pub(crate)` to banto-collect, so
        // `group_count() == 0` is the public equivalent of "nothing
        // collectible" - `build_config` already drops any connection with
        // zero collectible groups (see its own doc comment), so this is
        // exactly the same condition `Collector::start`'s own
        // `connections.is_empty()` check would use.
        if config.group_count() == 0 {
            let old = {
                let mut inner = self.inner.lock().expect("hub state lock poisoned");
                inner.map = Arc::new(new_map);
                inner.revision += 1;
                inner.last_error = None;
                inner.collector.take()
            };
            if let Some(collector) = old {
                let _ = collector.stop().await;
            }
            return Ok(());
        }

        // Start the new Collector BEFORE touching the old one - see this
        // module's doc comment for why the ordering matters (preserving the
        // old Collector/TagMap on failure requires it).
        let new_collector = match Collector::start(
            config,
            &self.data_dir,
            self.clock.clone(),
            self.events.clone(),
            self.options,
        )
        .await
        {
            Ok(collector) => collector,
            Err(err) => {
                let message = err.to_string();
                self.set_last_error(message.clone());
                return Err(message);
            }
        };

        let old = {
            let mut inner = self.inner.lock().expect("hub state lock poisoned");
            let old = inner.collector.replace(new_collector);
            inner.map = Arc::new(new_map);
            inner.revision += 1;
            inner.last_error = None;
            old
        };
        if let Some(collector) = old {
            let _ = collector.stop().await;
        }
        Ok(())
    }

    fn set_last_error(&self, message: String) {
        self.inner
            .lock()
            .expect("hub state lock poisoned")
            .last_error = Some(message);
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
    pub fn current_values(&self) -> Option<CurrentValuesHandle> {
        self.inner
            .lock()
            .expect("hub state lock poisoned")
            .collector
            .as_ref()
            .map(Collector::current_values)
    }

    /// Per-connection status (`"conn:{id}"` keys, matching
    /// `banto_collect`'s own convention), empty when nothing is running.
    pub fn connection_status(&self) -> HashMap<String, ConnectionStatus> {
        self.inner
            .lock()
            .expect("hub state lock poisoned")
            .collector
            .as_ref()
            .map(Collector::status)
            .unwrap_or_default()
    }

    /// Stop the running `Collector` cleanly (flushes tstore), if any. Called
    /// once at process shutdown (`bin/banto-hub.rs`).
    pub async fn shutdown(&self) {
        let old = self
            .inner
            .lock()
            .expect("hub state lock poisoned")
            .collector
            .take();
        if let Some(collector) = old {
            let _ = collector.stop().await;
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
    use tempfile::tempdir;

    /// `CollectorManager` needs a *file-backed* registry DB, same reasoning
    /// as `banto-collect`'s own integration tests (`build_config` and the
    /// per-connection tasks each hand out their own pool connection; a
    /// `:memory:` DB is a fresh empty database per connection).
    async fn manager_env() -> (sqlx::SqlitePool, tempfile::TempDir, CollectorManager) {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("registry.sqlite3");
        let pool = init_db(&db_path).await.expect("init_db");
        let data_dir = dir.path().join("data");
        let manager = CollectorManager::new(
            pool.clone(),
            data_dir,
            Arc::new(SystemClock),
            CollectorOptions {
                connect_timeout: Duration::from_millis(200),
                response_timeout: Duration::from_millis(200),
                ..CollectorOptions::default()
            },
        );
        (pool, dir, manager)
    }

    #[tokio::test]
    async fn rebuild_on_an_empty_registry_is_not_an_error() {
        let (_pool, _dir, manager) = manager_env().await;
        manager.rebuild().await.expect("empty rebuild should be Ok");
        assert_eq!(manager.revision(), 1);
        assert_eq!(manager.last_error(), None);
        assert!(manager.tag_map().is_empty());
        assert!(manager.current_values().is_none());
    }

    #[tokio::test]
    async fn rebuild_builds_a_catalog_entry_with_effective_enabled_state() {
        let (pool, _dir, manager) = manager_env().await;

        let conn = PlcConnectionService::new(pool.clone())
            .create(PlcConnectionInput {
                name: "line1".to_string(),
                protocol: "modbus-tcp".to_string(),
                host: "127.0.0.1".to_string(),
                port: 15020,
                unit_id: 1,
                enabled: true,
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
    }

    #[tokio::test]
    async fn rebuild_keeps_the_old_state_on_a_config_error() {
        let (pool, _dir, manager) = manager_env().await;

        let conn = PlcConnectionService::new(pool.clone())
            .create(PlcConnectionInput {
                name: "line1".to_string(),
                protocol: "modbus-tcp".to_string(),
                host: "127.0.0.1".to_string(),
                port: 15021,
                unit_id: 1,
                enabled: true,
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
    }

    /// Two concurrent `rebuild()` calls must not panic (no double-lock /
    /// re-entrancy deadlock on `rebuild_lock`) and must both actually run to
    /// completion (not silently coalesce) - `revision` ends up advanced by
    /// exactly 2 either way, since both calls see the same (empty) registry
    /// and each legitimately commits its own generation. This does not
    /// assert anything about *ordering* (not this fix's concern, per the
    /// review note) - only that serialization does not corrupt state or
    /// deadlock under concurrency.
    #[tokio::test]
    async fn concurrent_rebuild_calls_are_serialized_and_both_succeed() {
        let (_pool, _dir, manager) = manager_env().await;
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
