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
//! [`banto_collect::CollectorConfig::tag_count`] を見て、0 件なら
//! [`CollectorState::NoTargets`] という**専用の状態**を返す - #332 / #385 で
//! 確立した「**空とエラーを別の状態にする**」規律
//! （docs/implementation-checklist.md §5 の 1 行目）をここでも守る。
//!
//! **数えるのはタグ**であって、グループでも接続でもない。利用者にとっての
//! 「収集対象」は**タグ 1 本 1 本**で、グループと接続はその入れ物にすぎない
//! （入れ物だけ作って中身がまだ無い、は設定作業の途中として普通に起こる）。
//! [`banto_collect::build_config`] は**タグが 1 本も無い有効グループも計画に
//! 残す**ので、`group_count()` で数えると「有効な接続 1 件・有効なグループ
//! 1 件・**有効タグ 0 件**」という構成が素通りし、読む物が何も無いのに PLC へ
//! 繋ぎに行って tstore まで開いてしまう（#406 レビュー P2）。
//! `tag_count() == 0` は**グループ 0 件・接続 0 件の場合も必ず含む**（タグは
//! グループの中にしか居ない）ので、判定はこの 1 本で足りる - 「起こす条件」を
//! 2 本に割らない（docs/implementation-checklist.md §5）。
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
//! # ライフサイクルは専用タスクが所有する（ロックで守らない）
//!
//! [`Collector`] の**所有権そのもの**と、それに対応する状態遷移は
//! [`CollectorService`] ではなく**1 本の tokio タスク**（[`Lifecycle`]）が
//! 持つ。`start`/`stop`/`restart` と読み出しは、**コマンド（mpsc）+ 応答
//! （oneshot）**でそのタスクに依頼するだけで、呼び出し側は
//! [`Collector`] に指一本触れない。
//!
//! **なぜロックではなくタスクなのか（キャンセル安全性。#406 レビュー P2）**:
//! 以前はサービス側が `AsyncMutex<Option<Collector>>` を `take()` してから
//! `Collector::stop().await` していた。axum のハンドラは接続が切れれば
//! future を drop するので、**`take()` の後・`stop()` の完了前に呼び出し側が
//! 消える**ことが現実に起こる。そうなると `collector` は `None`・状態は
//! `Running` という食い違いが**そのまま残り**、しかも次の `stop()` は
//! 「走っていない」分岐に落ちて状態を直せない。さらに `Collector::stop()` は
//! **接続タスクの join と writer の最終 flush** なので、状態表示だけ直しても
//! 実体の停止は保証できない。ライフサイクル処理をタスク側に置けば、
//! **oneshot の受信側が落ちても送信が失敗するだけで、開始済みの処理は
//! 最後まで進む**（#400 で潰した「飛行中の操作」と同じ層の、キャンセル側）。
//!
//! この形から**ただで**出てくる保証:
//!
//! * タスクはコマンドを**逐次**処理するので、**新しい `start()` は前の
//!   `stop()` が完了するまで進まない**。二重起動の防止も「呼ばれた順に
//!   効く」も、ロックではなく**所有権と 1 本のキュー**が担保する。
//! * 読み出し（[`CollectorService::connection_status`] /
//!   [`CollectorService::current_values`]）も同じキューを通るので、
//!   **ライフサイクル操作の途中の [`Collector`] を覗くことがない** -
//!   返ってくるのは必ず「どれかの操作と操作の間」の姿で、状態と食い違わない。
//!   代償として、**起動処理の最中は読み出しがその完了まで待つ**（`start()` の
//!   無応答に上限を付けるのは C-2 の課題として申し送り済み）。
//! * コマンドを**送る前**に呼び出し側が消えた場合は、そもそも何も起きない
//!   （キューに積まれていないので、タスクは知らないまま）。
//!
//! 残るロックは**状態ロック（葉）** [`CollectorContext::state`]
//! （`std::sync::Mutex`）だけで、**書くのはライフサイクルタスクだけ**。
//! `.await` をまたいで保持しないし、ここから他のロックを取らない。
//! ポーリング経路（[`CollectorService::state`]）は**キューを通らない**ので、
//! 起動中（tstore を開いている最中）でも待たされない。
//!
//! # 終了フックからの停止
//!
//! `src-tauri` の `shutdown_app_state` も**同じ口**（[`CollectorService::stop`]）
//! を通る。あちらは後始末全体に 5 秒の予算があり、超えたら待つのをやめるが、
//! **やめるのは待つ側だけで、タスク側の停止処理（join と最終 flush）は
//! そのまま続く**。プロセスがその直後に終わるので実害は無い - 途中で
//! 切り上げても、失われうるのは最後の未 flush 分だけ（固まる方が悪い）。
//!
//! # 保持期間（`retention.days`）について
//!
//! `crate::settings::StoreSettings` が `data.dir` / `retention.days` を持つが、
//! **このモジュールのコードはファイルを一切削除しない**。期限超過ファイルの
//! 自動削除は docs/recorder-requirements.md §3.4 にある機能だが、**別途
//! 実装する**（誤って削除を先取りしない）。ここは設定値を持つだけ。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use banto_collect::{
    build_config, ClientFactory, CollectError, CollectEvent, Collector, CollectorOptions,
    ConnectionStatus, CurrentValuesHandle, EventSink,
};
use banto_core::BantoError;
use banto_tstore::{Clock, SystemClock};
use serde::Serialize;
use sqlx::SqlitePool;
use tokio::sync::{broadcast, mpsc, oneshot};

/// 収集サービスの状態。**必要最小限の 4 つ**だけ（語彙を増やさない）。
///
/// * [`Self::Stopped`] - 止まっている（まだ一度も起動していない、または
///   停止した）。
/// * [`Self::Running`] - 走っている。`groups`/`tags` は**起動時に実際に
///   採用された**収集対象の数。
/// * [`Self::NoTargets`] - **有効なタグが 1 件も無い**ので起動しなかった
///   （有効な接続やグループだけがあってタグが空、も含む）。**エラーでは
///   ない**（このモジュールの doc 参照）。
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

/// ライフサイクルタスクへの依頼。応答はそれぞれの `oneshot` に返す。
///
/// **応答の受信側が落ちても（呼び出し側のキャンセル）、タスクは処理を
/// 最後まで続ける** - `send` が `Err` になるだけ。それがこの設計の要点
/// （このモジュールの doc「ライフサイクルは専用タスクが所有する」）。
enum Command {
    Start(oneshot::Sender<Result<CollectorState, BantoError>>),
    Stop(oneshot::Sender<Result<CollectorState, BantoError>>),
    Restart(oneshot::Sender<Result<CollectorState, BantoError>>),
    /// 読み出しも同じキューを通す（操作の途中の [`Collector`] を覗かない）。
    ConnectionStatus(oneshot::Sender<Option<HashMap<String, ConnectionStatus>>>),
    CurrentValues(oneshot::Sender<Option<CurrentValuesHandle>>),
}

/// コマンドキューの深さ。ライフサイクル操作は人間の操作由来で秒に何度も
/// 来るものではなく、読み出しのポーリングもこの深さで詰まることはない。
/// 満杯になったら `send().await` が待つ（= 背圧）だけで、取りこぼさない。
const COMMAND_QUEUE_DEPTH: usize = 32;

/// 収集エンジンを組み立てるのに要る材料と、外から読める状態。
/// **[`CollectorService`] とライフサイクルタスクの両方が `Arc` で持つ**
/// （[`Collector`] 本体だけはタスクの専有）。
struct CollectorContext {
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
    /// **状態ロック（葉）**。**書くのは [`Lifecycle`] タスクだけ**なので、
    /// `Collector` の実体と食い違わない。読み取りはどこからでもよい
    /// （[`CollectorService::state`] はキューを通らない）。
    state: Mutex<CollectorState>,
}

impl CollectorContext {
    fn state(&self) -> CollectorState {
        self.state
            .lock()
            .expect("collector state lock poisoned")
            .clone()
    }

    fn set_state(&self, state: CollectorState) {
        *self.state.lock().expect("collector state lock poisoned") = state;
    }
}

/// [`CollectorService`] の実体。`CollectorService` はこれへの `Arc` 1 本だけを
/// 持つので、`Clone` しても**同じ収集エンジン**を指す（デスクトップの
/// コマンドと LAN の REST が別々のエンジンを立てない）。
struct CollectorInner {
    ctx: Arc<CollectorContext>,
    /// ライフサイクルタスクへのコマンド口。**初回の非同期操作で spawn する**
    /// （[`CollectorService::new`] は `tauri` の `setup` から**ランタイムの
    /// 外**で呼ばれるので、そこで `tokio::spawn` すると panic する）。
    /// `OnceLock` なので二重には立たない。
    commands: OnceLock<mpsc::Sender<Command>>,
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
                ctx: Arc::new(CollectorContext {
                    pool,
                    data_dir,
                    clock,
                    events,
                    options,
                    factory,
                    state: Mutex::new(CollectorState::Stopped),
                }),
                commands: OnceLock::new(),
            }),
        }
    }

    /// 収集を開始する。
    ///
    /// 1. レジストリから構成を作る（[`build_config`]）。
    /// 2. **有効タグが 0 件なら [`CollectorState::NoTargets`]**（`Ok`）。
    ///    `Collector::start` には渡さない - このモジュール doc 参照。
    /// 3. それ以外は [`Collector`] を起動して [`CollectorState::Running`]。
    ///
    /// **既に走っているときは何もしない**（現在の状態をそのまま返す）。
    /// 二重起動を防いでいるのはロックではなく、ライフサイクルタスクが
    /// コマンドを**逐次**処理すること（このモジュール doc 参照）。
    ///
    /// `Err` を返すのは「起動を試みて失敗した」ときだけ。そのとき状態は
    /// [`CollectorState::StartFailed`] になり、**理由が残る**。
    pub async fn start(&self) -> Result<CollectorState, BantoError> {
        self.lifecycle(Command::Start).await
    }

    /// 収集を停止する。**走っていなければ何もしない**（冪等）- 状態にも
    /// 触らないので、[`CollectorState::NoTargets`] や
    /// [`CollectorState::StartFailed`] の理由が `stop()` で消えることはない。
    ///
    /// `Err` は「最終 flush に失敗した」ときだけ返る。その場合でも
    /// **エンジンは確かに止まっている**ので状態は
    /// [`CollectorState::Stopped`] にする（状態は現実を写す）。
    ///
    /// **この `await` を途中でやめても停止は進む**（依頼はもうタスク側にある）。
    /// 終了フックの 5 秒予算が切れたときに起こるのがまさにこれで、待つのを
    /// やめるだけで join と最終 flush は続く。
    pub async fn stop(&self) -> Result<CollectorState, BantoError> {
        self.lifecycle(Command::Stop).await
    }

    /// 停止してから開始する。**間に他の `start`/`stop` を割り込ませない** -
    /// タスク側で「停止 → 開始」を 1 つのコマンドとして処理するので、
    /// `stop().await` → `start().await` と 2 回に分けたときのような隙間が
    /// そもそも無い。
    ///
    /// 停止側の失敗（最終 flush）は**開始を中止する理由にしない** - ログに
    /// 出して開始へ進む。落ちるのは旧ファイルの未 flush 分だけで、それは
    /// 「新しい構成で収集を再開できるか」とは無関係だから（`banto-collect` の
    /// `apply_config` が同じ天秤で同じ側を選んでいる）。
    pub async fn restart(&self) -> Result<CollectorState, BantoError> {
        self.lifecycle(Command::Restart).await
    }

    /// 現在の状態。**ネットワークもディスクも DB も触らない**し、コマンド
    /// キューも通らないので、起動処理の最中でも待たされない（ポーリングは
    /// これを見る）。
    pub fn state(&self) -> CollectorState {
        self.inner.ctx.state()
    }

    /// 接続ごとの状態のスナップショット。**走っていなければ `None`** -
    /// 空の `HashMap` を返して「接続 0 件」と混同させない
    /// （docs/implementation-checklist.md §5）。
    ///
    /// ライフサイクルタスクに問い合わせるので、**飛行中の start/stop が
    /// 終わってから**answer が返る（操作の途中の [`Collector`] は見えない）。
    pub async fn connection_status(&self) -> Option<HashMap<String, ConnectionStatus>> {
        self.readout(Command::ConnectionStatus).await
    }

    /// 現在値キャッシュのハンドル。**走っていなければ `None`**（同上 -
    /// 「値がまだ無い」キャッシュを返して「0 件」に潰さない）。
    pub async fn current_values(&self) -> Option<CurrentValuesHandle> {
        self.readout(Command::CurrentValues).await
    }

    /// 収集イベントの live 購読。
    ///
    /// ここだけ `Option` ではないのは、[`EventSink`] を**[`Collector`] では
    /// なくこのサービスが所有している**から: 購読者は start/stop をまたいで
    /// 同じ受信ハンドルを使い続けられ、`collection_started` を取りこぼさない。
    /// 「イベントが流れてこない」＝「走っていない」ではないので、走っているか
    /// どうかは [`Self::state`] を見ること。
    pub fn subscribe_events(&self) -> broadcast::Receiver<CollectEvent> {
        self.inner.ctx.events.subscribe()
    }

    // --- ライフサイクルタスクへの依頼 ------------------------------------

    /// ライフサイクルタスクへのコマンド口。**初回の呼び出しで spawn する** -
    /// [`Self::new`] は `tauri` の `setup`（tokio ランタイムの外）から呼ばれる
    /// ので、そこで `tokio::spawn` はできない。ここへ来る経路は全部 `async fn`
    /// の中なので、必ずランタイムの上にいる。
    fn commands(&self) -> &mpsc::Sender<Command> {
        self.inner.commands.get_or_init(|| {
            let (tx, rx) = mpsc::channel(COMMAND_QUEUE_DEPTH);
            let lifecycle = Lifecycle {
                ctx: self.inner.ctx.clone(),
                collector: None,
            };
            tokio::spawn(lifecycle.run(rx));
            tx
        })
    }

    /// `start`/`stop`/`restart` 共通の往復。タスクが居なくなっていたら
    /// （= panic した。通常は起こらない）**黙って成功にしない**。
    async fn lifecycle(
        &self,
        make: fn(oneshot::Sender<Result<CollectorState, BantoError>>) -> Command,
    ) -> Result<CollectorState, BantoError> {
        match self.request(make).await {
            Some(result) => result,
            None => Err(BantoError::Other(
                "収集サービスの内部タスクが停止しています（アプリを再起動してください）"
                    .to_string(),
            )),
        }
    }

    /// 読み出し共通の往復。タスクが居ないときは「走っていない」と同じ `None`
    /// になる - 走っていないのは事実（タスクごと消えているので誰も収集して
    /// いない）なので、ここは潰していることにならない。
    async fn readout<T>(&self, make: fn(oneshot::Sender<Option<T>>) -> Command) -> Option<T> {
        self.request(make).await.flatten()
    }

    /// コマンドを 1 つ送って応答を待つ。`None` = タスクが居ない / 応答が
    /// 返らなかった。
    async fn request<T>(&self, make: fn(oneshot::Sender<T>) -> Command) -> Option<T> {
        let (reply_tx, reply_rx) = oneshot::channel();
        // ここでキャンセルされた場合（まだ送れていない）は、タスクは依頼を
        // 知らないまま = 何も起きない。送れた後は、待つのをやめても処理は
        // 最後まで進む。
        self.commands().send(make(reply_tx)).await.ok()?;
        reply_rx.await.ok()
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

/// 走っている [`Collector`] を**専有する**タスクの中身。
///
/// [`CollectorService`] のどのメソッドもここへコマンドを送るだけなので、
/// `Collector` はこの構造体の外に一度も出ない（`Arc` にも `Mutex` にも
/// 入っていない）。状態を書くのもここだけ。
struct Lifecycle {
    ctx: Arc<CollectorContext>,
    /// `None` = 走っていない。[`Collector::stop`] が `self` を消費するので
    /// `Option` + `take()`。
    collector: Option<Collector>,
}

impl Lifecycle {
    /// コマンドを**逐次**処理する。1 つのコマンドを処理している間、次は
    /// 受け取らない - これが「新しい `start()` が前の `stop()` の完了前に
    /// 進まない」の全てで、そのためのロックは 1 つも要らない。
    ///
    /// [`CollectorService`] が全部 drop されると `Sender` が落ち、この
    /// ループが抜けてタスクが終わる。そのとき走っている [`Collector`] は
    /// `stop()` されずに drop される（接続タスクは切り離される）が、これは
    /// サービスごと捨てられる場面 = プロセス終了時だけで、正規の終了経路は
    /// `shutdown_app_state` が明示的に [`CollectorService::stop`] を通る。
    async fn run(mut self, mut commands: mpsc::Receiver<Command>) {
        while let Some(command) = commands.recv().await {
            match command {
                // `send` の `Err`（呼び出し側が待つのをやめた）は捨てる。
                // **処理そのものはもう終わっている**ので、誰も受け取らなく
                // ても状態と実体は一致している。
                Command::Start(reply) => {
                    let _ = reply.send(self.start().await);
                }
                Command::Stop(reply) => {
                    let _ = reply.send(self.stop().await);
                }
                Command::Restart(reply) => {
                    let _ = reply.send(self.restart().await);
                }
                Command::ConnectionStatus(reply) => {
                    let _ = reply.send(self.collector.as_ref().map(|c| c.status()));
                }
                Command::CurrentValues(reply) => {
                    let _ = reply.send(self.collector.as_ref().map(|c| c.current_values()));
                }
            }
        }
    }

    async fn start(&mut self) -> Result<CollectorState, BantoError> {
        // 二重起動の防止。逐次処理なので「見た直後に誰かが起動していた」は
        // 起こらない。
        if self.collector.is_some() {
            return Ok(self.ctx.state());
        }

        let config = match build_config(&self.ctx.pool).await {
            Ok(config) => config,
            Err(err) => return Err(self.fail_start(err)),
        };

        // 「収集対象なし」は **`Collector::start` に渡す前に**分岐する。
        // 渡すと `CollectError::Config` になり、本物の構成エラー（アドレスが
        // 解釈できない等）と同じ入れ物に入ってしまう。**数えるのはタグ** -
        // `build_config` はタグが空の有効グループも計画に残すので、
        // `group_count()` では「有効グループ 1・有効タグ 0」を素通りさせて
        // しまう（#406 レビュー P2。このモジュール doc 参照）。
        if config.tag_count() == 0 {
            self.ctx.set_state(CollectorState::NoTargets);
            return Ok(CollectorState::NoTargets);
        }

        let groups = config.group_count();
        let tags = config.tag_count();
        let collector = match Collector::start_with_client_factory(
            config,
            &self.ctx.data_dir,
            self.ctx.clock.clone(),
            self.ctx.events.clone(),
            self.ctx.options,
            self.ctx.factory.clone(),
        )
        .await
        {
            Ok(collector) => collector,
            Err(err) => return Err(self.fail_start(err)),
        };

        // **成功を確かめてから**状態を上げる（先に Running にして失敗時に
        // 降ろす、という順序にしない - docs/implementation-checklist.md §6
        // の「失敗経路での状態の落とし方」）。
        self.collector = Some(collector);
        let state = CollectorState::Running { groups, tags };
        self.ctx.set_state(state.clone());
        Ok(state)
    }

    async fn stop(&mut self) -> Result<CollectorState, BantoError> {
        let Some(collector) = self.collector.take() else {
            // 走っていない: **何もしない**。状態も触らない（`NoTargets` /
            // `StartFailed` の理由を握り潰さないため）。
            return Ok(self.ctx.state());
        };

        // ここから先は誰にも中断されない（呼び出し側が消えてもこのタスクは
        // 生きている）ので、`take()` 済み・状態は `Running` のまま、という
        // 食い違いが残ることはない。
        let result = collector.stop().await;
        // 止まったことは確定なので、flush の成否に関わらず `Stopped` にする。
        self.ctx.set_state(CollectorState::Stopped);
        match result {
            Ok(()) => Ok(CollectorState::Stopped),
            Err(err) => Err(collect_error(err)),
        }
    }

    async fn restart(&mut self) -> Result<CollectorState, BantoError> {
        if let Err(err) = self.stop().await {
            eprintln!(
                "banto: 収集の再起動中、停止側の後始末に失敗しました（開始は続行します）: {err}"
            );
        }
        self.start().await
    }

    /// 起動の失敗を状態に焼き付けて、同じ理由を `Err` として返す。
    /// **[`CollectError`] の文言をそのまま捨てない**。
    fn fail_start(&self, err: CollectError) -> BantoError {
        let reason = err.to_string();
        self.ctx.set_state(CollectorState::StartFailed {
            reason: reason.clone(),
        });
        collect_error_with_reason(err, reason)
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
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;
    use tokio::sync::Notify;

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

    /// 停止の途中で**確実に**止まってくれるゲート。接続タスクの graceful
    /// exit は `client.disconnect().await` を通るので、そこを塞ぐと
    /// [`Collector::stop`]（接続タスクの join → 最終 flush）が中で止まる。
    ///
    /// **1 回だけ**効く（`armed`）: 同じテストの中で立て直した 2 本目の
    /// エンジンの後始末まで塞ぐと、テストが終われなくなるため。
    ///
    /// 実時間は一切待たない - 「入った」も「開けた」も [`Notify`] で
    /// 待ち合わせる（`notify_one` は待ち手が居なければ permit を溜めるので、
    /// どちらが先でも取りこぼさない）。
    struct StopGate {
        entered: Notify,
        release: Notify,
        armed: AtomicBool,
    }

    impl StopGate {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                entered: Notify::new(),
                release: Notify::new(),
                armed: AtomicBool::new(true),
            })
        }

        /// 偽クライアントの `disconnect` から呼ばれる。
        async fn pass(&self) {
            if self.armed.swap(false, Ordering::SeqCst) {
                self.entered.notify_one();
                self.release.notified().await;
            }
        }

        /// 停止処理が「実体の停止」の途中まで進んだことを待つ。
        async fn wait_entered(&self) {
            self.entered.notified().await;
        }

        fn release(&self) {
            self.release.notify_one();
        }
    }

    /// [`OfflineClient`] と同じだが、切断だけ [`StopGate`] を通る。
    struct GatedClient {
        gate: Arc<StopGate>,
    }

    impl PlcClient for GatedClient {
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
            let gate = self.gate.clone();
            Box::pin(async move { gate.pass().await })
        }
    }

    fn gated_factory(gate: &Arc<StopGate>) -> ClientFactory {
        let gate = gate.clone();
        Arc::new(move |_spec| Box::new(GatedClient { gate: gate.clone() }) as Box<dyn PlcClient>)
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

    /// [`service`] と同じだが、切断を [`StopGate`] で塞げるサービス。
    async fn gated_service(dir: &TempDir) -> (SqlitePool, CollectorService, Arc<StopGate>) {
        let pool = init_db_memory().await.expect("init_db_memory");
        let gate = StopGate::new();
        let svc = CollectorService::new_for_test(
            pool.clone(),
            dir.path().join("data"),
            fast_options(),
            gated_factory(&gate),
        );
        (pool, svc, gate)
    }

    /// 指定した種類のイベントが来るまで待つ。**実時間を待たない**
    /// （broadcast の受信で待ち合わせるだけ）。
    async fn wait_for(rx: &mut broadcast::Receiver<CollectEvent>, kind: banto_collect::EventKind) {
        loop {
            let event = rx.recv().await.expect("イベント購読が切れた");
            if event.kind == kind {
                return;
            }
        }
    }

    /// 有効な接続 1 と、その下の**有効グループ 1**をレジストリに入れる
    /// （**タグは作らない**）。グループ id を返すので、テストが自分で
    /// 「タグ 0 件」「無効タグだけ」といった構成を組める。
    async fn seed_enabled_group(pool: &SqlitePool) -> i64 {
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
        group.id
    }

    /// [`seed_enabled_group`] のグループにタグを 1 本足す。
    /// `address` を呼び出し側が決められるので、「解釈できないアドレス」で
    /// 構成組み立ての失敗も作れる。`enabled` で無効タグも作れる。
    async fn seed_tag(pool: &SqlitePool, group_id: i64, address: &str, enabled: bool) {
        TagService::new(pool.clone())
            .create(TagInput {
                name: "T1".to_string(),
                collection_group_id: group_id,
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
                enabled,
                writable: false,
                tag_kind: "plc".to_string(),
                expression: None,
                retain: false,
                expected_revision: None,
            })
            .await
            .expect("create tag");
    }

    /// 有効な接続 1 / グループ 1 / **有効タグ 1** - 「走る」構成。
    async fn seed_one_tag(pool: &SqlitePool, address: &str) {
        let group_id = seed_enabled_group(pool).await;
        seed_tag(pool, group_id, address, true).await;
    }

    /// **この PR の一番大事な受入条件**: 有効な収集対象が 1 件も無いときは
    /// 「収集対象なし」であって、**エラーではない**。
    ///
    /// 反証（回帰の検出）: `Lifecycle::start` の `tag_count() == 0` 分岐を
    /// 消して `Collector::start` の `CollectError::Config` をそのまま返す実装に
    /// 戻すと、`start()` が `Err` を返すのでこのテストは `expect` で落ちる。
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

    /// #406 レビュー P2: **接続とグループは有効なのに、タグが 1 本も
    /// 登録されていない**構成。`build_config` はタグが空の有効グループも計画に
    /// 残す（`GroupPlan` の `tags`/`requests` が空になるだけ）ので
    /// `group_count() == 1` / `tag_count() == 0` になり、`group_count` で
    /// 判定していた頃は **`Running { groups: 1, tags: 0 }` になって PLC へ
    /// 繋ぎに行き、tstore まで開いていた**。収集する物は 1 つも無い。
    ///
    /// 反証（回帰の検出）: 判定を `config.group_count() == 0` に戻すと
    /// `NoTargets` ではなく `Running { groups: 1, tags: 0 }` が返り、
    /// `connection_status()` も `Some` になるのでこのテストは落ちる。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_enabled_group_with_no_tag_at_all_is_no_targets() {
        let dir = TempDir::new();
        let (pool, svc) = service(&dir).await;
        seed_enabled_group(&pool).await;

        let state = svc.start().await.expect("有効タグ 0 件はエラーではない");
        assert_eq!(state, CollectorState::NoTargets);
        assert_eq!(svc.state(), CollectorState::NoTargets);
        // **収集エンジンを起動していない** = PLC へ繋ぎにも行っていない。
        assert!(
            svc.connection_status().await.is_none(),
            "収集対象が無いのにエンジンが立っている"
        );
        assert!(svc.current_values().await.is_none());
    }

    /// 同上の、**タグは登録されているが全部無効**な場合。`build_config` は
    /// 無効タグを落とすので、レジストリに行はあっても計画のタグは 0 件になる。
    ///
    /// 反証（回帰の検出）: 上と同じ - `group_count()` 判定に戻すと
    /// `Running { groups: 1, tags: 0 }` になって落ちる。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_enabled_group_whose_tags_are_all_disabled_is_no_targets() {
        let dir = TempDir::new();
        let (pool, svc) = service(&dir).await;
        let group_id = seed_enabled_group(&pool).await;
        seed_tag(&pool, group_id, "40001", false).await;

        let state = svc.start().await.expect("有効タグ 0 件はエラーではない");
        assert_eq!(state, CollectorState::NoTargets);
        assert_eq!(svc.state(), CollectorState::NoTargets);
        assert!(
            svc.connection_status().await.is_none(),
            "収集対象が無いのにエンジンが立っている"
        );
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

    /// `start` と `stop` を**同時に**投げても、ライフサイクルタスクが逐次
    /// 処理するので「半分だけ起動した」状態は残らない - 終わったあとは必ず
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

    // --- 停止の途中キャンセル（#406 レビュー P2） ------------------------

    /// **この修正の一番大事な受入条件**: `stop()` の呼び出し側が
    /// 停止処理の途中で消えても、**停止は実体まで完了し、状態と食い違わない**。
    ///
    /// 旧実装（サービス側で `AsyncMutex<Option<Collector>>` を `take()` して
    /// から `collector.stop().await`）では、この瞬間にキャンセルされると
    /// `collector = None` / 状態 = `Running` のまま**永久に**固定され、
    /// もう一度 `stop()` を呼んでも「走っていない」分岐に落ちて直せなかった。
    /// さらに接続タスクの join も最終 flush も行われないままだった。
    ///
    /// 実時間は待たない（ゲートは [`Notify`]、完了待ちは読み出しのキュー）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelling_stop_still_finishes_the_stop() {
        let dir = TempDir::new();
        let (pool, svc, gate) = gated_service(&dir).await;
        seed_one_tag(&pool, "40001").await;
        let mut rx = svc.subscribe_events();

        svc.start().await.expect("start");
        // 接続タスクが `Connected` になるまで待つ。そこまで行かないと
        // graceful exit が `disconnect` を通らず、ゲートに入らない。
        wait_for(&mut rx, banto_collect::EventKind::PlcConnected).await;

        let stopper = {
            let svc = svc.clone();
            tokio::spawn(async move { svc.stop().await })
        };
        // 「停止が実体の停止の途中まで進んだ」ことを確かめてから切る
        // （切るのが早すぎると、そもそも何も始まっていない）。
        gate.wait_entered().await;
        stopper.abort();
        assert!(
            stopper.await.unwrap_err().is_cancelled(),
            "呼び出し側は確かにキャンセルされた"
        );

        gate.release();

        // 読み出しはライフサイクルタスクのキューを通るので、**飛行中の停止が
        // 終わってから**返る - sleep で待つ必要がない。
        assert!(
            svc.connection_status().await.is_none(),
            "キャンセル後も停止は実体まで完了している"
        );
        assert!(svc.current_values().await.is_none());
        assert_eq!(
            svc.state(),
            CollectorState::Stopped,
            "状態と実体が一致している"
        );

        let kinds = drain(&mut rx);
        assert!(
            kinds.contains(&banto_collect::EventKind::CollectionStopped),
            "最終 flush まで進んだ（`collection_stopped` は stop の最後に出る）: {kinds:?}"
        );

        // キャンセルの後にもう一度呼んでも壊れない（冪等のまま）。
        assert_eq!(
            svc.stop().await.expect("キャンセル後の stop"),
            CollectorState::Stopped
        );
    }

    /// キャンセルされた `stop()` の**後に投げた `start()` は、その停止が
    /// 完了するまで進まない**（オーナー指定）。
    ///
    /// 証拠はイベントの順序: `collection_stopped` は旧エンジンの最終 flush の
    /// **後**に出るので、2 本目の `collection_started` がその後ろに来ていれば、
    /// 新しい起動は確かに前の停止の完了を待っている。
    ///
    /// `start` の依頼を**キューに積んでからゲートを開ける**ために、ここだけ
    /// 内部の [`Command`] を直接送っている（`svc.start()` を別タスクで
    /// 走らせる書き方だと、依頼が積まれる前にゲートを開けてしまう競争になり、
    /// テストが「たまたま順番どおり」でも通ってしまう）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_start_queued_after_a_cancelled_stop_waits_for_it() {
        let dir = TempDir::new();
        let (pool, svc, gate) = gated_service(&dir).await;
        seed_one_tag(&pool, "40001").await;
        let mut rx = svc.subscribe_events();

        svc.start().await.expect("start");
        wait_for(&mut rx, banto_collect::EventKind::PlcConnected).await;

        let stopper = {
            let svc = svc.clone();
            tokio::spawn(async move { svc.stop().await })
        };
        gate.wait_entered().await;
        stopper.abort();
        let _ = stopper.await;

        // 停止はまだゲートの中。ここで start をキューへ積む。
        let (reply_tx, reply_rx) = oneshot::channel();
        svc.commands()
            .send(Command::Start(reply_tx))
            .await
            .map_err(|_| ())
            .expect("start をキューに積む");
        gate.release();
        let state = reply_rx
            .await
            .expect("start の応答")
            .expect("キャンセル後の start");
        assert_eq!(state, CollectorState::Running { groups: 1, tags: 1 });

        let kinds = drain(&mut rx);
        let stopped = kinds
            .iter()
            .position(|k| *k == banto_collect::EventKind::CollectionStopped)
            .unwrap_or_else(|| panic!("前の停止が完了していない: {kinds:?}"));
        let started = kinds
            .iter()
            .position(|k| *k == banto_collect::EventKind::CollectionStarted)
            .unwrap_or_else(|| panic!("新しい起動が見当たらない: {kinds:?}"));
        assert!(
            stopped < started,
            "新しい start が、前の stop の完了を待たずに進んだ: {kinds:?}"
        );

        svc.stop().await.expect("後始末の stop");
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
