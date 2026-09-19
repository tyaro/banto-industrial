//! 収集サービス（#383 段階2b / R1-C の C-1）: `banto-collect` の
//! [`Collector`] をこのアプリの設定 DB・データディレクトリに配線する
//! サービス層。
//!
//! chronogazer はこれまで収集エンジンを一切起動していなかった（`banto-collect`
//! は R1-A で依存に足しただけで使用箇所 0）。ここはその**最初の配線**にあたる。
//!
//! # この PR（C-1）の範囲
//!
//! **骨格だけ**。サービス層の `start`/`stop`/`restart`/`state`/読み出しまでで、
//!
//! * 操作コマンド・REST/Tauri の口・権限・監査は **C-2**、
//! * 画面・現在値 API・イベント一覧は **C-3**、
//! * シミュレータハーネスと E2E は **C-4**、
//! * Hub 経由で受けている値の保存・合流は **段階3**（`crate::hub` は触らない）。
//!
//! この PR では**誰も [`CollectorService::start`] を呼ばない**（アプリ起動時の
//! 自動開始は C-2）。`src-tauri` はサービスを組み立てて `AppState` に載せ、
//! 終了時に [`CollectorService::stop`] を呼ぶだけ。
//!
//! # 扱う対象（段階2b）
//!
//! **SLMP / Modbus TCP の直結のみ**。Hub 接続は設定 KV（`hub.record`）のままの
//! 別建てで、`plc_connections` には何も足していない（マイグレーション無し）。
//!
//! # 状態の表現 — 「収集対象 0 件」は異常ではない
//!
//! [`banto_collect::Collector::start`] は接続 0 件の構成を
//! [`CollectError::Config`] で拒否する。エンジンにとっては正しい（開く
//! スキーマが無い）が、**利用者にとってはエラーではなく「まだ何も登録して
//! いない」**。そこでこのサービスは `Collector::start` を呼ぶ**前に**
//! [`banto_collect::CollectorConfig::group_count`] を見て、0 件なら
//! [`CollectorState::NoTargets`] という**専用の状態**を返す - #332 / #385 で
//! 確立した「**空とエラーを別の状態にする**」規律
//! （docs/implementation-checklist.md §5 の 1 行目）をここでも守る。
//!
//! 状態は 4 つだけ（[`CollectorState`]）: 停止中 / 動作中 / 収集対象なし /
//! 起動失敗（理由付き）。起動失敗の `reason` は [`CollectError`] の
//! 文言をそのまま載せる（分類して捨てない）。
//!
//! # 下位の失敗で上位の状態を書き換えない
//!
//! [`CollectorState`] が変わるのは **[`CollectorService`] 自身の
//! start/stop/restart のときだけ**。走り出した後の PLC 断・読み取り失敗・
//! append 失敗は `banto-collect` 側が `Bad` 品質フラグと `collect_events` の
//! イベントに変換して**回り続ける**設計なので（`banto-collect` の
//! `error.rs` / `task.rs` のモジュール doc）、それでこのサービスの状態を
//! 「停止」や「起動失敗」に落とすことはしない（#385 の規律 =
//! docs/implementation-checklist.md §5「状態を別軸に保つ」）。
//!
//! # ロック順序（一方向に固定）
//!
//! `crate::hub::HubService` と**同じ書き方で揃える**:
//!
//! 1. **操作ロック（外）** [`CollectorInner::operation`] -
//!    `start`/`stop`/`restart` の全体を直列化する。二重起動の防止と
//!    「start と stop が同時に来たときは呼ばれた順に効く」を、これ 1 本で
//!    保証している。
//! 2. **収集ロック（内）** [`CollectorInner::collector`] - 走っている
//!    [`Collector`] 本体。[`Collector::stop`] が `self` を消費するので
//!    `Option` に入れて `take()` できる形にしてある。
//! 3. **状態ロック（葉）** [`CollectorInner::state`] - `std::sync::Mutex`。
//!    **`.await` をまたいで保持しない**し、ここから他のロックを取らない。
//!
//! 逆順で取る経路を作らないこと。`*_locked` の付いた private 関数は
//! **操作ロックを呼び出し元が既に持っている前提**で書いてあり、その証拠として
//! ガードの参照を引数で受け取る（`HubService` の `reconcile*` と同じ約束を、
//! こちらは型で見えるようにした）。
//!
//! ポーリング経路（[`CollectorService::state`]）は**操作ロックも収集ロックも
//! 取らない**（状態ロックだけ）ので、起動中（tstore を開いている最中）でも
//! 待たされない。
//!
//! # 保持期間（`retention.days`）について
//!
//! `crate::settings::StoreSettings` が `data.dir` / `retention.days` を持つが、
//! **このモジュールのコードはファイルを一切削除しない**。期限超過ファイルの
//! 自動削除は docs/recorder-requirements.md §3.4 にある機能だが、**別途
//! 実装する**（誤って削除を先取りしない）。ここは設定値を持つだけ。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use banto_collect::{
    build_config, ClientFactory, CollectError, CollectEvent, Collector, CollectorOptions,
    ConnectionStatus, CurrentValuesHandle, EventSink,
};
use banto_core::BantoError;
use banto_tstore::{Clock, SystemClock};
use serde::Serialize;
use sqlx::SqlitePool;
use tokio::sync::{broadcast, Mutex as AsyncMutex, MutexGuard as AsyncMutexGuard};

/// 収集サービスの状態。**必要最小限の 4 つ**だけ（語彙を増やさない）。
///
/// * [`Self::Stopped`] - 止まっている（まだ一度も起動していない、または
///   停止した）。
/// * [`Self::Running`] - 走っている。`groups`/`tags` は**起動時に実際に
///   採用された**収集対象の数。
/// * [`Self::NoTargets`] - 有効な接続・グループ・タグが 1 件も無いので
///   起動しなかった。**エラーではない**（このモジュールの doc 参照）。
/// * [`Self::StartFailed`] - 起動を試みて失敗した。`reason` は
///   [`CollectError`] の文言そのまま。
///
/// `Serialize` は C-2（コマンド／REST）で**そのまま**ワイヤに載せるため。
/// `crate::hub` が `HubStatus` を判別共用体のまま画面へ渡しているのと同じ
/// 作法（`{"state":"running","groups":2,"tags":10}`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum CollectorState {
    Stopped,
    Running { groups: usize, tags: usize },
    NoTargets,
    StartFailed { reason: String },
}

impl CollectorState {
    /// 走っているか。`state()` の呼び出し側が毎回 `matches!` を書かずに
    /// 済むだけの薄い述語。
    pub fn is_running(&self) -> bool {
        matches!(self, Self::Running { .. })
    }
}

/// [`CollectorService`] の実体。`CollectorService` はこれへの `Arc` 1 本だけを
/// 持つので、`Clone` しても**同じ収集エンジン**を指す（デスクトップの
/// コマンドと LAN の REST が別々のエンジンを立てない）。
struct CollectorInner {
    /// レジストリ（`plc_connections`/`collection_groups`/`tags`）と
    /// `collect_events` を同居させている、このアプリ唯一の SQLite プール。
    /// [`build_config`] の読み取り元であり、[`EventSink`] の書き込み先でもある。
    pool: SqlitePool,
    /// 時系列ファイル（`banto-tstore`）の置き場。設定 `data.dir` を
    /// [`resolve_data_dir`] で解決したもの。
    data_dir: PathBuf,
    /// ストアのローテーションと現在値の Stale 判定が**同じ「今」**を見るための
    /// 時計。本番は [`SystemClock`]。
    clock: Arc<dyn Clock>,
    /// イベントの二系統出力（live broadcast + `collect_events` テーブル）。
    /// **[`Collector`] ではなくこのサービスが所有する**ので、
    /// [`CollectorService::subscribe_events`] で取った受信ハンドルは
    /// start/stop をまたいで生き続ける（banto-hub の `CollectorManager` が
    /// rebuild をまたいで `EventSink` を使い回しているのと同じ理由）。
    events: EventSink,
    options: CollectorOptions,
    /// 接続タスクが再接続に使うクライアントの生成口。本番は
    /// [`banto_collect::default_client_factory`]（SLMP / Modbus TCP の直結）。
    /// テストだけが差し替える。
    factory: ClientFactory,
    /// **操作ロック（外）**。このモジュール doc の「ロック順序」参照。
    operation: AsyncMutex<()>,
    /// **収集ロック（内）**。`None` = 走っていない。
    /// [`Collector::stop`] が `self` を消費するので `Option` + `take()`。
    collector: AsyncMutex<Option<Collector>>,
    /// **状態ロック（葉）**。書き換えるのは操作ロックを持っている最中だけ
    /// （= `collector` の中身と食い違わない）。読み取りはロック順序の外から
    /// でもよい。
    state: Mutex<CollectorState>,
}

/// 収集エンジンのサービス層。`src-tauri` の（C-2 で足す）コマンドと
/// `crate::rest` の（同じく C-2 の）ルーターが共有する - 他のサービスと同じ
/// 「service 層は tauri も axum も知らない」規約。
#[derive(Clone)]
pub struct CollectorService {
    inner: Arc<CollectorInner>,
}

impl CollectorService {
    /// 収集サービスを組み立てる。**この時点では何も起動しない**
    /// （`data_dir` も作らない。実際に開くのは [`Self::start`] の中の
    /// `TsWriter`）。
    ///
    /// `clock` を引数に取らないのは、`src-tauri`（このアプリ自身の新規依存を
    /// 持たない、という invariant）が [`SystemClock`] を名指しできないため。
    /// 本番の時計は 1 つしかないので、差し替えはテスト専用の
    /// `new_for_test` に閉じている。
    pub fn new(pool: SqlitePool, data_dir: PathBuf) -> Self {
        Self::build(
            pool.clone(),
            data_dir,
            Arc::new(SystemClock),
            CollectorOptions::default(),
            banto_collect::default_client_factory(),
        )
    }

    fn build(
        pool: SqlitePool,
        data_dir: PathBuf,
        clock: Arc<dyn Clock>,
        options: CollectorOptions,
        factory: ClientFactory,
    ) -> Self {
        let events = EventSink::new(pool.clone());
        Self {
            inner: Arc::new(CollectorInner {
                pool,
                data_dir,
                clock,
                events,
                options,
                factory,
                operation: AsyncMutex::new(()),
                collector: AsyncMutex::new(None),
                state: Mutex::new(CollectorState::Stopped),
            }),
        }
    }

    /// 収集を開始する。
    ///
    /// 1. レジストリから構成を作る（[`build_config`]）。
    /// 2. **収集対象が 0 件なら [`CollectorState::NoTargets`]**（`Ok`）。
    ///    `Collector::start` には渡さない - このモジュール doc 参照。
    /// 3. それ以外は [`Collector`] を起動して [`CollectorState::Running`]。
    ///
    /// **既に走っているときは何もしない**（現在の状態をそのまま返す）。
    /// 二重起動は操作ロックで防いでいる（このモジュール doc の「ロック順序」）。
    ///
    /// `Err` を返すのは「起動を試みて失敗した」ときだけ。そのとき状態は
    /// [`CollectorState::StartFailed`] になり、**理由が残る**。
    pub async fn start(&self) -> Result<CollectorState, BantoError> {
        let operation = self.inner.operation.lock().await;
        self.start_locked(&operation).await
    }

    /// 収集を停止する。**走っていなければ何もしない**（冪等）- 状態にも
    /// 触らないので、[`CollectorState::NoTargets`] や
    /// [`CollectorState::StartFailed`] の理由が `stop()` で消えることはない。
    ///
    /// `Err` は「最終 flush に失敗した」ときだけ返る。その場合でも
    /// **エンジンは確かに止まっている**ので状態は
    /// [`CollectorState::Stopped`] にする（状態は現実を写す）。
    pub async fn stop(&self) -> Result<CollectorState, BantoError> {
        let operation = self.inner.operation.lock().await;
        self.stop_locked(&operation).await
    }

    /// 停止してから開始する。**間に他の `start`/`stop` を割り込ませない**
    /// ため、操作ロックを 1 回だけ取って両方をその中で行う
    /// （`stop().await` → `start().await` と 2 回に分けると、その隙間に
    /// 別の呼び出しが入れてしまう）。
    ///
    /// 停止側の失敗（最終 flush）は**開始を中止する理由にしない** - ログに
    /// 出して開始へ進む。落ちるのは旧ファイルの未 flush 分だけで、それは
    /// 「新しい構成で収集を再開できるか」とは無関係だから（`banto-collect` の
    /// `apply_config` が同じ天秤で同じ側を選んでいる）。
    pub async fn restart(&self) -> Result<CollectorState, BantoError> {
        let operation = self.inner.operation.lock().await;
        if let Err(err) = self.stop_locked(&operation).await {
            eprintln!(
                "banto: 収集の再起動中、停止側の後始末に失敗しました（開始は続行します）: {err}"
            );
        }
        self.start_locked(&operation).await
    }

    /// 現在の状態。**ネットワークもディスクも DB も触らない**し、操作ロック・
    /// 収集ロックのどちらも取らないので、起動処理の最中でも待たされない
    /// （ポーリングはこれを見る）。
    pub fn state(&self) -> CollectorState {
        self.inner
            .state
            .lock()
            .expect("collector state lock poisoned")
            .clone()
    }

    /// 接続ごとの状態のスナップショット。**走っていなければ `None`** -
    /// 空の `HashMap` を返して「接続 0 件」と混同させない
    /// （docs/implementation-checklist.md §5）。
    pub async fn connection_status(&self) -> Option<HashMap<String, ConnectionStatus>> {
        self.inner
            .collector
            .lock()
            .await
            .as_ref()
            .map(|collector| collector.status())
    }

    /// 現在値キャッシュのハンドル。**走っていなければ `None`**（同上 -
    /// 「値がまだ無い」キャッシュを返して「0 件」に潰さない）。
    pub async fn current_values(&self) -> Option<CurrentValuesHandle> {
        self.inner
            .collector
            .lock()
            .await
            .as_ref()
            .map(|collector| collector.current_values())
    }

    /// 収集イベントの live 購読。
    ///
    /// ここだけ `Option` ではないのは、[`EventSink`] を**[`Collector`] では
    /// なくこのサービスが所有している**から: 購読者は start/stop をまたいで
    /// 同じ受信ハンドルを使い続けられ、`collection_started` を取りこぼさない。
    /// 「イベントが流れてこない」＝「走っていない」ではないので、走っているか
    /// どうかは [`Self::state`] を見ること。
    pub fn subscribe_events(&self) -> broadcast::Receiver<CollectEvent> {
        self.inner.events.subscribe()
    }

    // --- 操作ロックを持っている前提の実体 --------------------------------
    //
    // 引数の `_operation` は使わないが、**呼び出し元が操作ロックを持っている
    // ことの証拠**として受け取る（このモジュール doc の「ロック順序」）。

    async fn start_locked(
        &self,
        _operation: &AsyncMutexGuard<'_, ()>,
    ) -> Result<CollectorState, BantoError> {
        // 二重起動の防止。操作ロックの中で見ているので、「見た直後に誰かが
        // 起動していた」は起こらない。
        if self.inner.collector.lock().await.is_some() {
            return Ok(self.state());
        }

        let config = match build_config(&self.inner.pool).await {
            Ok(config) => config,
            Err(err) => return Err(self.fail_start(err)),
        };

        // 「収集対象なし」は **`Collector::start` に渡す前に**分岐する。
        // 渡すと `CollectError::Config` になり、本物の構成エラー（アドレスが
        // 解釈できない等）と同じ入れ物に入ってしまう。`group_count() == 0` は
        // `build_config` が「収集グループを 1 つも持たない接続は計画に入れ
        // ない」ので、そのまま「接続 0 件」と同値。
        if config.group_count() == 0 {
            self.set_state(CollectorState::NoTargets);
            return Ok(CollectorState::NoTargets);
        }

        let groups = config.group_count();
        let tags = config.tag_count();
        let collector = match Collector::start_with_client_factory(
            config,
            &self.inner.data_dir,
            self.inner.clock.clone(),
            self.inner.events.clone(),
            self.inner.options,
            self.inner.factory.clone(),
        )
        .await
        {
            Ok(collector) => collector,
            Err(err) => return Err(self.fail_start(err)),
        };

        // **成功を確かめてから**状態を上げる（先に Running にして失敗時に
        // 降ろす、という順序にしない - docs/implementation-checklist.md §6
        // の「失敗経路での状態の落とし方」）。
        *self.inner.collector.lock().await = Some(collector);
        let state = CollectorState::Running { groups, tags };
        self.set_state(state.clone());
        Ok(state)
    }

    async fn stop_locked(
        &self,
        _operation: &AsyncMutexGuard<'_, ()>,
    ) -> Result<CollectorState, BantoError> {
        let running = self.inner.collector.lock().await.take();
        let Some(collector) = running else {
            // 走っていない: **何もしない**。状態も触らない（`NoTargets` /
            // `StartFailed` の理由を握り潰さないため）。
            return Ok(self.state());
        };

        // 止まったことは確定なので、flush の成否に関わらず `Stopped` にする。
        let result = collector.stop().await;
        self.set_state(CollectorState::Stopped);
        match result {
            Ok(()) => Ok(CollectorState::Stopped),
            Err(err) => Err(collect_error(err)),
        }
    }

    /// 起動の失敗を状態に焼き付けて、同じ理由を `Err` として返す。
    /// **[`CollectError`] の文言をそのまま捨てない**。
    fn fail_start(&self, err: CollectError) -> BantoError {
        let reason = err.to_string();
        self.set_state(CollectorState::StartFailed {
            reason: reason.clone(),
        });
        collect_error_with_reason(err, reason)
    }

    fn set_state(&self, state: CollectorState) {
        *self
            .inner
            .state
            .lock()
            .expect("collector state lock poisoned") = state;
    }

    /// テスト専用の組み立て口: 時計・チューニング・クライアント生成口を
    /// 差し替える。本番の [`Self::new`] を 1 本に保ったまま、テストが
    /// 短いタイムアウトと偽クライアントを使えるようにするためだけのもの。
    #[cfg(test)]
    fn new_for_test(
        pool: SqlitePool,
        data_dir: PathBuf,
        options: CollectorOptions,
        factory: ClientFactory,
    ) -> Self {
        Self::build(pool, data_dir, Arc::new(SystemClock), options, factory)
    }
}

/// [`CollectError`] をこのアプリの共通エラー型へ。`Registry` は元々
/// [`BantoError`] なのでそのまま戻し（検証エラーの `field_errors` を
/// 文字列に潰さない）、それ以外は文言を保つ。
fn collect_error(err: CollectError) -> BantoError {
    let reason = err.to_string();
    collect_error_with_reason(err, reason)
}

fn collect_error_with_reason(err: CollectError, reason: String) -> BantoError {
    match err {
        CollectError::Registry(inner) => inner,
        _ => BantoError::Other(reason),
    }
}

/// 設定 `data.dir` を実際のディレクトリへ解決する。
///
/// 既定値は banto-hub に倣った相対パス `"./data"` なので、そのまま使うと
/// **プロセスの作業ディレクトリ**（デスクトップアプリでは何であるか分からない
/// 場所）に時系列ファイルを作ってしまう。そこで**相対パスは `base`
/// （アプリのデータディレクトリ）からの相対**として解決する。絶対パスが
/// 設定されていればそれをそのまま使う（運用で外付けドライブを指したい、が
/// この設定の主目的）。
///
/// 空文字・空白だけは「未設定」とみなし、既定の `"./data"` と同じ扱いにする。
pub fn resolve_data_dir(base: &Path, configured: &str) -> PathBuf {
    let trimmed = configured.trim();
    if trimmed.is_empty() {
        return base.join("data");
    }
    let path = PathBuf::from(trimmed);
    if path.is_absolute() {
        path
    } else {
        base.join(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::init_db_memory;
    use crate::test_support::TempDir;
    use banto_collect::BackoffConfig;
    use banto_plc::{BoxFuture, PlcClient, PlcError, ReadRequest, ReadResult, TagValue};
    use banto_tags::{
        CollectionGroupInput, CollectionGroupService, PlcConnectionInput, PlcConnectionService,
        TagInput, TagService,
    };
    use banto_tstore::WriterOptions;
    use std::time::Duration;

    /// 速い・決定的なチューニング。`banto-collect` の `tests/integration.rs`
    /// の `fast_options` と同じ意図（**テストの中で実時間を待たない**ための
    /// もので、待ち合わせのための sleep はどのテストにも書いていない）。
    fn fast_options() -> CollectorOptions {
        CollectorOptions {
            backoff: BackoffConfig {
                base: Duration::from_millis(20),
                max: Duration::from_millis(100),
            },
            connect_timeout: Duration::from_millis(100),
            response_timeout: Duration::from_millis(100),
            writer_options: WriterOptions::default(),
        }
    }

    /// 接続しに行かない偽クライアント。**実 PLC を模さない**（一巡の確認は
    /// C-4 のハーネスの仕事）が、「収集対象があるときに `Collector` が確かに
    /// 立ち上がる」を実ネットワーク無しで押さえるために使う。
    struct OfflineClient;

    impl PlcClient for OfflineClient {
        fn connect(&mut self) -> BoxFuture<'_, Result<(), PlcError>> {
            Box::pin(async { Ok(()) })
        }

        fn read_batch<'a>(
            &'a mut self,
            requests: &'a [ReadRequest],
        ) -> BoxFuture<'a, Result<Vec<ReadResult>, PlcError>> {
            Box::pin(async move {
                Ok(requests
                    .iter()
                    .map(|_| ReadResult::Value(TagValue::F64(1.0)))
                    .collect())
            })
        }

        fn disconnect(&mut self) -> BoxFuture<'_, ()> {
            Box::pin(async {})
        }
    }

    fn offline_factory() -> ClientFactory {
        Arc::new(|_spec| Box::new(OfflineClient) as Box<dyn PlcClient>)
    }

    /// いま溜まっているイベントの種類を全部取り出す（待たない）。
    fn drain(rx: &mut broadcast::Receiver<CollectEvent>) -> Vec<banto_collect::EventKind> {
        let mut kinds = Vec::new();
        while let Ok(event) = rx.try_recv() {
            kinds.push(event.kind);
        }
        kinds
    }

    fn kind_count(kinds: &[banto_collect::EventKind], kind: banto_collect::EventKind) -> usize {
        kinds.iter().filter(|k| **k == kind).count()
    }

    fn count_kind(
        rx: &mut broadcast::Receiver<CollectEvent>,
        kind: banto_collect::EventKind,
    ) -> usize {
        kind_count(&drain(rx), kind)
    }

    async fn service(dir: &TempDir) -> (SqlitePool, CollectorService) {
        let pool = init_db_memory().await.expect("init_db_memory");
        let svc = CollectorService::new_for_test(
            pool.clone(),
            dir.path().join("data"),
            fast_options(),
            offline_factory(),
        );
        (pool, svc)
    }

    /// 有効な接続 1 / グループ 1 / タグ 1 をレジストリに入れる。
    /// `address` を呼び出し側が決められるので、「解釈できないアドレス」で
    /// 構成組み立ての失敗も作れる。
    async fn seed_one_tag(pool: &SqlitePool, address: &str) {
        let conn = PlcConnectionService::new(pool.clone())
            .create(PlcConnectionInput {
                name: "PLC1".to_string(),
                protocol: "modbus-tcp".to_string(),
                // 127.0.0.1:1 は何も listen していない予約ポート。
                // `offline_factory` を使うのでそもそも誰も dial しない。
                host: "127.0.0.1".to_string(),
                port: 1,
                unit_id: 1,
                enabled: true,
                simulation: false,
                word_order: "low_high".to_string(),
                database: None,
                username: None,
                password: None,
            })
            .await
            .expect("create plc connection");
        let group = CollectionGroupService::new(pool.clone())
            .create(CollectionGroupInput {
                name: "G1".to_string(),
                plc_connection_id: conn.id,
                period_ms: 1000,
                enabled: true,
                default_writable: true,
                query_sql: None,
            })
            .await
            .expect("create collection group");
        TagService::new(pool.clone())
            .create(TagInput {
                name: "T1".to_string(),
                collection_group_id: group.id,
                address: address.to_string(),
                data_type: "i16".to_string(),
                string_length: None,
                string_encoding: "utf8".to_string(),
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
                expected_revision: None,
            })
            .await
            .expect("create tag");
    }

    /// **この PR の一番大事な受入条件**: 有効な収集対象が 1 件も無いときは
    /// 「収集対象なし」であって、**エラーではない**。
    ///
    /// 反証（回帰の検出）: `start_locked` の `group_count() == 0` 分岐を消して
    /// `Collector::start` の `CollectError::Config` をそのまま返す実装に戻すと、
    /// `start()` が `Err` を返すのでこのテストは `expect` で落ちる。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn no_enabled_targets_is_a_state_of_its_own_not_an_error() {
        let dir = TempDir::new();
        let (_pool, svc) = service(&dir).await;

        let state = svc
            .start()
            .await
            .expect("収集対象 0 件は「エラー」ではない（Ok が返る）");
        assert_eq!(state, CollectorState::NoTargets);
        assert_eq!(svc.state(), CollectorState::NoTargets);
        // 「走っていない」ことが読み出し側からも分かる（空に潰さない）。
        assert!(svc.connection_status().await.is_none());
        assert!(svc.current_values().await.is_none());
    }

    /// 停止中に `stop()` を呼んでも壊れない（冪等）。あわせて、**`stop()` が
    /// 直前の理由（ここでは `NoTargets`）を握り潰さない**ことも固定する。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stop_is_idempotent_and_keeps_the_previous_reason() {
        let dir = TempDir::new();
        let (_pool, svc) = service(&dir).await;

        assert_eq!(
            svc.stop().await.expect("停止中の stop"),
            CollectorState::Stopped
        );
        assert_eq!(svc.state(), CollectorState::Stopped);

        svc.start().await.expect("start");
        assert_eq!(svc.state(), CollectorState::NoTargets);

        // 走っていないので「何もしない」= NoTargets のまま。
        assert_eq!(
            svc.stop().await.expect("走っていない stop"),
            CollectorState::NoTargets
        );
        assert_eq!(
            svc.stop().await.expect("2 回目の stop"),
            CollectorState::NoTargets
        );
    }

    /// `start()` に失敗したら**理由が状態に残る**。解釈できないアドレスの
    /// タグを 1 本入れて `build_config` を失敗させる（`banto-tags` は
    /// アドレスの書式を検証しない - 書式は I2/I3b の担当）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_failed_start_keeps_its_reason_in_the_state() {
        let dir = TempDir::new();
        let (pool, svc) = service(&dir).await;
        seed_one_tag(&pool, "これはアドレスではない").await;

        let err = svc.start().await.expect_err("構成が組み立てられない");
        let message = err.to_string();

        match svc.state() {
            CollectorState::StartFailed { reason } => {
                assert!(
                    !reason.trim().is_empty(),
                    "理由が空になっている: {reason:?}"
                );
                assert!(
                    message.contains(&reason) || reason.contains("収集設定エラー"),
                    "状態の理由と返ったエラーが食い違っている: state={reason:?} err={message:?}"
                );
            }
            other => panic!("StartFailed を期待したが {other:?}"),
        }
        assert!(svc.connection_status().await.is_none());
    }

    /// 収集対象があるときは本当に起動し、読み出しが「走っている」形になる。
    /// 二重 `start()` が 2 つ目のエンジンを立てないことも同時に固定する。
    ///
    /// **数え方**: 接続ごとの `ConnectionStatus` は接続タスクが**自分の
    /// タイミングで**書き込むので、起動直後の件数を数えるのは時間に依存する
    /// （それを待つのは「実時間待ちのテスト」になる）。代わりに
    /// `collection_started` イベントの本数を数える - これは
    /// `Collector::start` が**戻る前に**必ず 1 回出すので、2 本目のエンジンが
    /// 立っていれば必ず 2 件になる。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn start_twice_runs_exactly_one_engine() {
        let dir = TempDir::new();
        let (pool, svc) = service(&dir).await;
        seed_one_tag(&pool, "40001").await;
        let mut rx = svc.subscribe_events();

        let first = svc.start().await.expect("1 回目の start");
        assert_eq!(first, CollectorState::Running { groups: 1, tags: 1 });
        assert!(svc.state().is_running());
        assert!(
            svc.connection_status().await.is_some(),
            "走っているので Some（`None` = 走っていない、と読み分けられる）"
        );
        assert!(svc.current_values().await.is_some());

        let second = svc.start().await.expect("2 回目の start");
        assert_eq!(second, first, "二重起動はせず、同じ状態を返す");
        assert_eq!(
            count_kind(&mut rx, banto_collect::EventKind::CollectionStarted),
            1,
            "2 回目の start がもう 1 つエンジンを立てていない"
        );

        svc.stop().await.expect("stop");
        assert_eq!(svc.state(), CollectorState::Stopped);
        assert!(svc.connection_status().await.is_none());
    }

    /// `start` と `stop` を**同時に**投げても、操作ロックで直列化されるので
    /// 「半分だけ起動した」状態は残らない - 終わったあとは必ず
    /// 「走っている（`Running`）」か「止まっている（`Stopped`）」の
    /// どちらかで、しかも状態と読み出しが一致する。
    ///
    /// 実時間を待たない（`join` するだけ。`sleep` も `tokio::time` の進行も
    /// 使っていない）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn start_and_stop_racing_leave_a_consistent_state() {
        let dir = TempDir::new();
        let (pool, svc) = service(&dir).await;
        seed_one_tag(&pool, "40001").await;

        let starter = {
            let svc = svc.clone();
            tokio::spawn(async move { svc.start().await })
        };
        let stopper = {
            let svc = svc.clone();
            tokio::spawn(async move { svc.stop().await })
        };
        starter.await.expect("start task").expect("start");
        stopper.await.expect("stop task").expect("stop");

        let state = svc.state();
        let running = svc.connection_status().await.is_some();
        assert_eq!(
            state.is_running(),
            running,
            "状態（{state:?}）と読み出し（走っている={running}）が食い違っている"
        );
        assert!(
            matches!(
                state,
                CollectorState::Running { .. } | CollectorState::Stopped
            ),
            "中途半端な状態が残っている: {state:?}"
        );

        // 後始末（走っていれば止める。止まっていれば何もしない）。
        svc.stop().await.expect("後始末の stop");
    }

    /// `restart()` は止めてから起動する。走っていない状態から呼んでも
    /// 単なる起動として成立し、走っている状態から呼ぶと**止めてから**
    /// 立て直す（= 前のエンジンが残らない）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn restart_stops_the_old_engine_before_starting_a_new_one() {
        let dir = TempDir::new();
        let (pool, svc) = service(&dir).await;
        seed_one_tag(&pool, "40001").await;
        let mut rx = svc.subscribe_events();

        // 走っていない状態からの restart = ただの起動（停止は何もしない）。
        let state = svc.restart().await.expect("restart");
        assert_eq!(state, CollectorState::Running { groups: 1, tags: 1 });

        let again = svc.restart().await.expect("2 回目の restart");
        assert_eq!(again, CollectorState::Running { groups: 1, tags: 1 });

        let events = drain(&mut rx);
        assert_eq!(
            kind_count(&events, banto_collect::EventKind::CollectionStarted),
            2,
            "restart 2 回で起動は 2 回: {events:?}"
        );
        assert_eq!(
            kind_count(&events, banto_collect::EventKind::CollectionStopped),
            1,
            "2 回目の restart は古いエンジンを確かに止めてから立て直した: {events:?}"
        );

        svc.stop().await.expect("stop");
    }

    /// `subscribe_events()` は start/stop をまたいで生き続ける
    /// （`EventSink` を `Collector` ではなくサービスが持っているため）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_event_subscription_survives_start_and_stop() {
        let dir = TempDir::new();
        let (pool, svc) = service(&dir).await;
        seed_one_tag(&pool, "40001").await;

        let mut rx = svc.subscribe_events();
        svc.start().await.expect("start");
        svc.stop().await.expect("stop");

        let kinds = drain(&mut rx);
        assert!(
            kinds.contains(&banto_collect::EventKind::CollectionStarted),
            "購読は start 前に取ったので collection_started が届く: {kinds:?}"
        );
        assert!(
            kinds.contains(&banto_collect::EventKind::CollectionStopped),
            "同じ受信ハンドルで stop も見える: {kinds:?}"
        );
    }

    // --- data.dir の解決 --------------------------------------------------

    #[test]
    fn relative_data_dir_resolves_against_the_app_data_dir() {
        let base = Path::new("C:/appdata");
        assert_eq!(
            resolve_data_dir(base, "./data"),
            base.join("./data"),
            "既定の相対パスは作業ディレクトリではなくデータディレクトリ基準"
        );
    }

    #[test]
    fn absolute_data_dir_is_used_as_is() {
        let base = Path::new("C:/appdata");
        let absolute = if cfg!(windows) { "D:/ts" } else { "/var/ts" };
        assert_eq!(resolve_data_dir(base, absolute), PathBuf::from(absolute));
    }

    #[test]
    fn blank_data_dir_falls_back_to_the_default_location() {
        let base = Path::new("C:/appdata");
        assert_eq!(resolve_data_dir(base, "   "), base.join("data"));
    }
}
