# banto-hub T14 詳細設計（ランタイム状態管理・制御面分離）

作成日: 2026-08-09
状態: **設計提案（レビュー用 draft）。主要判断は §1、要オーナー承認。** 実装未着手。
最終検証日(コード照合): 2026-08-09
基準コミット: `175d36d`（main、banto-hub-desktop-plan.md §16 マージ済み）

関連: [banto-hub-desktop-plan.md](banto-hub-desktop-plan.md)（§4 状態モデル / §5 共通
ランタイム / §7 安全規則 / §16.3 T14 未決事項）、[tag-server-design.md](tag-server-design.md)
（§4.1 revision 契約 / §4.3 オンライン部分再構成）、[plan.md](plan.md) §4c。

## 0. スコープと位置づけ

本書は banto-hub-desktop-plan.md §16.3「T14 着手前に確定すべき未決事項」を、現行コードの
精密照合（2026-08-09、上位モデル）に基づき詳細設計へ落としたものである。対象は T14 の6論点:

1. HubRuntime ライブラリ化（挙動不変の composition root 抽出）
2. 収集状態機械と直列化 controller
3. `rebuild()` の分割（catalog commit と collector apply の分離）
4. 2レビジョン（configured / running）と消費者マトリクス
5. 全構成 preflight の実現方式
6. 書き込み受付 OFF 連動・no-spawn、常駐タスクのライフサイクル、broker stop-and-join、
   T14 サブスライス分割

本書は設計の一次資料であり、実装 PR（T14-1〜T14-4）はここを参照する。決定は §15 の運用
どおりオーナー承認を経てから実装へ反映する。

## 1. 要オーナー承認の主要判断（サマリ）

以下は代替案のある設計判断で、実装範囲・共有クレート表面積・外部契約に影響する。詳細と根拠は
各節に。**推奨（2026-08-09、上位モデル）** を示すが、実装着手前にオーナー承認を求める。

| ID  | 論点                        | 推奨                                                                                 | 主な代替                                    | 影響                              |
| --- | --------------------------- | ------------------------------------------------------------------------------------ | ------------------------------------------- | --------------------------------- |
| P1  | 全構成 preflight の実現     | SQLite savepoint 内で提案を仮適用 + スナップショット入力の検証関数（`build_*_from`） | 検証系の純化のみ / savepoint のみ           | banto-collect の新エントリ追加    |
| P2  | broker per-connection 停止  | banto-broker（`SessionDirectory`）に stop-and-join を共有 API として追加             | HubSessions 限定ラッパで内部実装            | relay-wright への表面積波及       |
| P3  | controller の所有権         | `CollectorManager` は再ホームせず、その上位に薄い状態層 `CollectionController`       | 全 run-context Arc を controller へ再ホーム | 改修の爆発半径                    |
| P4  | 全量再発行トリガ            | MQTT publish_all は running（Running 遷移）で発火し、configured 保存では撃たない     | 現行どおり単一 revision で発火              | 保存時の旧値再発行を回避          |
| P5  | 書き込み受付と収集状態      | `WriteControl` は薄い AtomicBool のまま、遷移関数が `disable()` を呼ぶフック方式     | 収集状態の従属フィールド化                  | 復帰時 auto-enable しない仕様確定 |
| P6  | 常駐ループ（computed/prune) | プロセス寿命のまま JoinHandle 捕捉し `RunningHub::shutdown` で abort                 | 収集 run-context 寿命へ移す                 | 停止中も内部/演算タグ評価継続     |

## 2. 現行コード地図（設計の前提となる要点）

いずれも `175d36d` で照合済み。行番号は同コミット。

- `hub_run::run(shutdown)`（`apps/banto-hub/core/src/bin/banto_hub/hub_run.rs:105-397`）が
  composition root。`init_db`(111) / `settings.server_config`(136) / `settings.store_config`(140)
  / `start`(348)、および `HubSessions::new` 内 `BrokerSupervisor::spawn`(broker_glue.rs:283) が
  `expect()` でプロセスを落とす。非致命フォールバック（ログのみ）は `load_retained_values`・
  `load_persisted_enabled`・`mqtt.apply`・`grpc.apply`・起動時 `rebuild`。
- 停止ハンドルを持たない detached タスクは2本のみ:
  computed 250ms 評価ループ（hub_run.rs:236、コメント「no graceful shutdown handle」）と
  tstore 剪定 24h ループ（hub_run.rs:255）。どちらも `JoinHandle` を捨てている。
- シャットダウン順序は `mqtt → grpc → manager → sessions → sim_registry → server`
  （hub_run.rs:386-396）。`sessions`/`sim_registry`/`computed_engine` は bin が Arc を保持し
  `CollectorManager` へ clone 注入（hub_run.rs:155-208）。
- `CollectorManager::rebuild()`（hub.rs:723-909）は `rebuild_lock` で直列化し、
  build_catalog(726, DB×3) → `computed::build_plan`(742, 純関数) → build_config(751, DB×3・
  **アドレス parse と型検証の唯一の場所**) → sync_slmp_sessions(771, **副作用: broker セッション
  起動・simulator 起動**) → `group_count()==0` の停止分岐(806-833) or apply/start 分岐(835-908)
  → 成功時のみ `inner.map`/`revision+1`/`current`/`last_apply` を同一クリティカルセクションで commit。
- 停止分岐(806-833)は既に「catalog と revision を進めつつ Collector を起動しない」を体現する
  テンプレート。`Collector::stop(mut self)`(collector.rs:577) は self を消費するため、停止↔再開は
  `None` を経由し次回 `start` で新インスタンスを作る形が既に成立している
  （`collector: AsyncMutex<Option<Collector>>`, hub.rs:565）。
- `revision` は単一 u64 + 単一 watch（hub.rs:483/585）。定義は「catalog(TagMap)スナップショットの
  世代」（hub.rs:346-348）。消費者は §6 の表。
- `HubSessions`（broker_glue.rs:266-370）に per-connection の停止/join は無い。`remove` は
  clone を落とすだけでタスク終了を保証せず（broker_glue.rs:343-355）、`shutdown` は supervisor を
  `Option::take` で消費する一回限り（broker_glue.rs:357-369）。banto-broker のセッションタスクは
  (a) supervisor 共有 shutdown watch、(b) 全 handle drop の2条件でのみ終了（lib.rs:848-976）。
  唯一の join 経路は `BrokerSupervisor::shutdown(self)`（lib.rs:714、全接続一括・self 消費）。
  `SlmpSimRegistry::shutdown` は drain のみで再利用可（broker_glue.rs:512）。
- 書き込みゲートは REST/gRPC 共有の `execute_write`（write_path.rs:238）。gate5 で
  `write_control.is_enabled()` を判定（fail-closed, write_path.rs:295-308）し、これは物理書き込み
  = gate8 の `write_broker_handle`→`ensure_connection`（hub.rs:1152、**セッションが無ければ spawn**）
  より前にある。`WriteControl`（write_control.rs:42-80）は AtomicBool + 表示専用永続値のみで、起動時
  必ず OFF。収集状態と write_control は現状**完全独立**（連動ゼロ）。

## 3. D1 — HubRuntime ライブラリ化（挙動不変）

`hub_run::run` の composition root を `banto_hub_core` の再利用可能ランタイムへ抽出する。

```rust
pub struct HubConfig {
    pub db_path: PathBuf,
    pub allow_setup: bool,
    pub bind: IpAddr,          // 既定 127.0.0.1（§8 セキュリティ、現行 env 既定を踏襲）
    pub port: u16,             // 既定 8722
    pub data_dir_override: Option<PathBuf>,
    pub static_assets: Arc<dyn StaticAssetProvider>, // bin の FrontendAssets を注入（core は非依存）
}

pub enum HubStartError { InitDb(..), ServerConfig(..), StoreConfig(..), Bind(..), Broker(..) }

impl HubRuntime {
    pub async fn start(config: HubConfig) -> Result<RunningHub, HubStartError>;
}
impl RunningHub {
    pub fn local_addr(&self) -> SocketAddr;
    pub fn controller(&self) -> Arc<CollectionController>; // D2
    pub async fn shutdown(self);
}
```

決定:

- 現行の5つの `expect()`（§2）を `?` + `HubStartError` へ置換し、UI で説明できる構造化エラーに
  する（plan §5.1）。非致命フォールバックは現状どおりログ + 継続（挙動不変）。
- 静的 UI 配信は現在 `static_router::<FrontendAssets>`（hub_run.rs:344）でジェネリック。core が
  bin の埋め込みアセットに依存しないよう、`static_assets` を trait object で注入する。Tauri の
  `frontendDist` を別配布しない方針（plan §8.1）と整合。
- console / Windows サービス / デスクトップシェルの3ホストは `HubConfig` を組み立てて
  `start` を呼び、各自の停止トリガ（Ctrl-C / SCM Stop / トレイ）で `shutdown` する薄い層になる。
- 常駐ループ2本（computed / prune）の `JoinHandle` を捕捉し、`RunningHub::shutdown` で abort する
  （D7）。これは現行の「プロセス終了任せ」から挙動が変わる唯一の点だが、ライブラリとして
  clean shutdown を保証するために必要で、外部から観測できる収集/IF の挙動は不変。
- **T14-1 は収集セマンティクスを一切変えない**。起動時 rebuild もそのまま残す（自動開始の廃止は
  D2 以降で controller 経由に切り替える）。

## 4. D2 — 収集状態機械と直列化 controller

新規 `CollectionController` を導入する。**P3 の推奨に従い `CollectorManager` を再ホームせず**、
その上位の薄い状態層とする（`Arc<CollectorManager>` と `Arc<WriteControl>` を保持）。理由: collector /
sessions / sim_registry / computed の Arc 所有と `rebuild_lock` の直列化は既に機能しており、全再ホームは
`rest.rs`(4.8k 行)・`hub.rs`(1.5k 行) 横断の高リスク改修になる。controller は「状態・遷移直列化・
run_id」だけを新たに所有し、実処理は `CollectorManager` の分割後 API（D3）を駆動する。

```rust
enum CollectionState { Stopped, Starting, Running, Stopping, Faulted }
enum RunMode { Configured, AllSimulation }        // AllSimulation の実装は T15
struct RunContext { mode: RunMode, run_id: RunId } // Starting / Running のとき Some

struct CollectionController {
    manager: Arc<CollectorManager>,
    write_control: Arc<WriteControl>,
    state: Mutex<RuntimeState>,             // 現在状態 + RunContext
    transition: AsyncMutex<()>,             // 遷移の直列化（rebuild_lock とは別レイヤ）
    status_tx: watch::Sender<RuntimeStatus>,// D4 の running 側 watch
    run_seq: AtomicU64,                      // run_id 採番（単調・非再利用）
}
```

決定:

- **遷移直列化**: `transition` async ロックで start / stop / mode 切替を直列化する。これは
  `rebuild_lock`（rebuild 同士の直列化）とは別レイヤで、CRUD-commit（D3）と収集遷移の相互排他まで
  広げる。plan §4.3「単一 controller で直列化」に対応。
- **冪等 API**: 遷移中（Starting / Stopping）に来た start / stop 要求は**キューせず現在状態を返す**
  （plan §4.3「遷移中は新しい要求を重ねず現在状態を返す」）。Running で同一 mode の start は no-op。
  これは `rebuild_lock` の「キューして順次実行」とは異なる意味論なので、`transition` は try-lock 的に
  「遷移中なら即座に現在状態を返す」形で実装する。
- **run_id**: `run_seq` の `fetch_add` で採番（`Date`/乱数に依存しない単調 ID）。開始ごとに変わり
  再利用しない（plan §5.4）。
- **遷移の先頭で必ず書き込み受付を OFF**（D6、§7 step2）。
- **`configured` ⇄ `all_simulation` の切替は必ず `stopped` を経由**（plan §4.3）。
- **`faulted`**: start / stop / 切替の失敗で入る。実機収集を自動再開しない。診断（`last_runtime_error`）
  を残し、明示操作を待つ。特に **SCM がサービスを起動して `apply_run(Configured)` が失敗した場合は
  `faulted` を公開する**（SCM へ Running を報告したまま黙って停止していない状態を作らない。arch
  レビュー指摘）。
- **ホスト別の起動時挙動**: T14-2 以降、起動時に暗黙で収集を開始しない。デスクトップホストは
  `Stopped` のまま起動し、サービスホストは `controller.start(Configured)` を呼ぶ。plan §4.3 と一致。

### 4.1 状態遷移表

| 現在     | 操作              | 前提                 | 次状態                         |
| -------- | ----------------- | -------------------- | ------------------------------ |
| Stopped  | start(mode)       | preflight 成功       | Starting → Running / Faulted   |
| Stopped  | CRUD/preflight    | —                    | Stopped（configured のみ前進） |
| Running  | stop              | —                    | Stopping → Stopped / Faulted   |
| Running  | 別 mode へ切替    | —                    | Stopping → Stopped → Starting  |
| Starting | start/stop 再要求 | —                    | 現在状態を返す（冪等）         |
| Stopping | start/stop 再要求 | —                    | 現在状態を返す（冪等）         |
| Faulted  | start             | 明示・preflight 成功 | Starting …                     |
| Faulted  | —（自動）         | —                    | 自動再開しない                 |

## 5. D3 — `rebuild()` の分割（catalog commit と collector apply の分離）

現行 `rebuild()`（hub.rs:723-909）を2つの公開操作へ割る。自然な切れ目は「副作用のない検証/commit」と
「副作用のある sync_slmp_sessions + collector 起動」の境界。

```rust
// 停止中も実行可。catalog(TagMap)/computed plan と configured_revision を進める。Collector 非起動。
async fn commit_catalog(&self, snapshot: &RegistrySnapshot) -> Result<CatalogRevision, ConfigError>;

// 開始 / mode 切替時のみ。build_config + sync_slmp_sessions（副作用）+ Collector start/apply_config、
// running_revision を追いつかせる。
async fn apply_run(&self, mode: RunMode) -> Result<RunReport, ConfigError>;
```

決定:

- `commit_catalog` = 現行 build_catalog + `computed::build_plan` + `inner.map`/computed plan/
  `configured_revision` の commit。**現行の停止分岐(806-833)の commit 部分を一般化**し、停止中でも
  catalog を前進させる（Collector には触れない）。
- `apply_run` = build_config + sync_slmp_sessions + Collector `start_with_client_factory` /
  `apply_config`（現行 835-908）+ `running_revision` 前進。**副作用段（sync_slmp_sessions）は
  apply 側に属する**（map 制約: 停止中に broker セッションを張らないため）。
- `Collector::stop(mut self)` が self を消費する制約（§2）に合わせ、controller の stop は現行の
  「`take()` して `stop()`、collector を `None` に戻す」経路（hub.rs:811/827）を使う。再開は次回
  `apply_run` が `start_with_client_factory` で新インスタンスを作る。
- **起動時 preflight の再実行**: `apply_run` は開始時に build_config / build_plan を再実行してから
  collector を起動する（plan §5.2、T14 受入「運転開始時にも同じ preflight を再実行」）。
- CRUD/一括/CSV の保存は `commit_catalog`（preflight 込み、D5）を呼ぶだけで Collector を起動しない。
  現行の `rebuild_and_notify`（rest.rs:1694、rebuild 失敗握り潰し）はこの経路へ置換され、
  banto-hub-desktop-plan.md §16.2 の「保存成功＝実行可能」を満たす。

## 6. D4 — 2レビジョンと消費者マトリクス

`configured_revision`（保存/検証で前進）と `running_revision`（開始/apply で前進）を導入する。
現行の単一 `revision` は定義上「catalog 世代」なので **`configured_revision` に読み替え、ワイヤ上の
フィールド名 `revision` は後方互換のため configured を指すまま維持**する。

- `configured_revision`: `commit_catalog` 成功で +1。catalog(TagMap) の世代。既存 watch を流用。
- `running_revision`: `apply_run`（start 成功 / mode 切替 / stop）で +1。実行構成の世代 + `run_id`。
  新設の `status_tx: watch::Sender<RuntimeStatus>` で配信（RuntimeStatus = {state, mode, run_id,
  configured_revision, running_revision}）。

消費者マトリクス（現行アンカーと、どちらの世代を見るか）:

| 消費者                               | 現行アンカー          | 追従先               | 根拠                                                                             |
| ------------------------------------ | --------------------- | -------------------- | -------------------------------------------------------------------------------- |
| WS `config_changed`                  | stream.rs:172/209     | **configured**       | 契約は「catalog を再取得」。保存で catalog が変わる                              |
| WS 値再解決（250ms ポーリング）      | stream.rs:487         | catalog（configured) | 既に tag_map ポーリング。変更不要                                                |
| MQTT `publish_all` トリガ            | mqtt.rs:341/354       | **running**（P4）    | topic は catalog 由来だが値は running 由来。保存だけで旧値を新トポロジで撒かない |
| gRPC `get_catalog`/`read_values` rev | grpc.rs:430/447       | configured           | catalog 世代。ワイヤ互換で `revision` 維持                                       |
| gRPC `stream_values`                 | grpc.rs:605           | catalog              | 既に tag_map ポーリング。変更不要                                                |
| REST `/tags` `/values` rev           | rest.rs:2715/2817     | configured           | catalog 構造。ワイヤ互換で `revision` 維持                                       |
| REST `/status`                       | rest.rs:3042          | **両方 + run_id**    | 診断。既に running を映す `last_apply`/`connections` を返す                      |
| 書き込みゲート（`entry.enabled`）    | write_path.rs:245-256 | configured + 状態    | D6。保存済みの enabled を見るが、収集状態でゲート                                |
| computed 評価ループ                  | hub_run.rs:241        | catalog              | 既に tag_map ポーリング。変更不要                                                |

決定:

- **MQTT の全量再発行を running 遷移で発火**（P4）。現行は単一 revision（=保存）で撃つため、2レビジョン化で
  「停止中の保存で稼働前に全量再発行し、旧稼働値を新トポロジへ撒く」問題が生じる。これを避けるため
  `publish_all` は `status_tx` を購読し **Running へ入った時**に撃つ。MQTT の SIM 時ポリシー（全 PLC SIM 中は
  既定 OFF・stream 能動終了）は T15 で重ねる。
- `/status` に `configured_revision` / `running_revision` / `run_id` / `collection_state` /
  `collection_mode` / `last_runtime_error` を追加（plan §5.4）。ワイヤ後方互換のため既存 `revision`
  は configured を指すまま残す。
- `running_revision` を watch にする際、watch の coalesce（中間値の取りこぼし）は許容。MQTT 再発行は
  「最新を撒き直す」冪等操作なので取りこぼしは無害（現行 revision watch と同じ論拠, hub.rs:579-584）。
- **潜在バグの同時修正**: 現行 rebuild は build_catalog と build_config が同じ3テーブルを別々に read
  し（計6クエリ・rebuild 内に読取ズレ窓）、単一スナップショットで束ねていない。D5 の
  `RegistrySnapshot` 導入でこのズレ窓も閉じる。

## 7. D5 — 全構成 preflight の実現方式（P1）

**推奨: SQLite savepoint + スナップショット入力の検証関数（`build_*_from`）のハイブリッド。**

制約（§2）: `CollectorConfig` のフィールドは `pub(crate)` で、生成口は `build_config(pool)` のみ。
`build_catalog` は private module fn。`build_plan` のみ純関数。banto-tags の
`validate_tag_kind_placement` 等は DB を読む配置/一意性検証を持つ。

決定:

1. banto-collect と hub に**スナップショット入力版**を追加する:
   - `build_config_from(&RegistrySnapshot) -> Result<CollectorConfig, CollectError>`
   - `build_catalog_from(&RegistrySnapshot) -> Result<TagMap, BantoError>`（pub 化 + snapshot 入力）
   - `RegistrySnapshot` = connections/groups/tags を**1回**読んだ Vec 群。既存の pool 読取版は
     この薄いラッパとして残す（後方互換）。
2. **保存経路の preflight は SQLite savepoint 内で実行**:
   `SAVEPOINT` → 提案ミューテーション適用 → `RegistrySnapshot` を1回読む →
   `build_catalog_from` + `build_config_from` + `build_plan` を実行 → 全通過なら `RELEASE`（commit）
   して `configured_revision` 前進、いずれか失敗なら `ROLLBACK` して**接続/グループ/タグ/CSV 行に
   紐付く構造化エラー**を返す（plan §9.3 TAG-P0-2 受入）。
   - savepoint を選ぶ理由: banto-tags の既存 DB 検証（配置・UNIQUE 名・FK）を**そのまま原子的に
     再利用**でき、純化で再実装する誤りを避けられる。アドレス parse / 型 / DAG は snapshot 入力の
     `build_*_from` が担う。
3. **開始経路の preflight**（`apply_run`）は DB の現行スナップショットに対し `build_*_from` を再実行
   （savepoint 不要 = 保存済み構成が対象）してから collector を起動。
4. **単票 / 連続 / CSV / 運転開始が同一 preflight を共有**する（plan §16.2、§16.4 の dry-run 偽陽性も
   これで解消 — 既存 `dry_run` は banto-tags 検証のみでアドレス parse を含まない）。

## 8. D6 — 書き込み受付 OFF 連動と no-spawn（P5）

決定:

- `CollectionController` が `Arc<WriteControl>` を保持し、**全遷移（start/stop/mode 切替）の先頭で
  `write_control.disable()` を呼ぶ**（§7 step2）。`WriteControl` は薄い AtomicBool のまま
  （relay-wright arming との同型を維持、P5）。
- **自動復元しない**（plan §7）: SIM→configured 復帰や start 成功で write を自動 ON にしない。運転開始後の
  書き込みは管理画面から明示的に再度 enable する運用。
- **no-spawn-while-stopped**: gate5（write_enabled）は gate8（`write_broker_handle`→`ensure_connection`
  spawn）より前（write_path.rs:295-308）。遷移で write_control を OFF にし自動復元しないため、
  **停止中の書き込みは gate5 で弾かれ spawn に到達しない**。write_path 自体は無改修。
- もう一つの spawn 経路 `sync_slmp_sessions`（hub.rs:1009-1048、enabled+slmp へ無条件 ensure）は
  D3 で `apply_run`（開始時のみ）へ移るため、**停止中は走らず spawn しない**。
- SIM 中に「既存 SIM セッションへの書き込みのみ許可し新規 spawn しない」ための HubSessions の
  write 可能 peek API（spawn 無し）は **T15 の write-during-SIM で追加**する（T14 では停止時ゲートで十分）。

## 9. D7 — 常駐タスクのライフサイクル（P6）

`hub_run` の全 `tokio::spawn` を寿命で分類する。

| タスク                    | 現行アンカー        | 寿命                     | 停止機構（設計後）                                  |
| ------------------------- | ------------------- | ------------------------ | --------------------------------------------------- |
| computed 250ms 評価ループ | hub_run.rs:236      | プロセス（runtime）      | JoinHandle を捕捉し `RunningHub::shutdown` で abort |
| tstore 剪定 24h ループ    | hub_run.rs:255      | プロセス（runtime）      | 同上                                                |
| MQTT poll/eval            | mqtt.rs:266/275     | MqttPublisher の run-ctx | 既存: apply/shutdown で abort                       |
| gRPC serve                | grpc.rs:783         | runtime                  | 既存: shutdown で abort                             |
| gRPC 各ストリーム         | grpc.rs:597/646     | RPC スコープ             | 自己終了                                            |
| WS 各接続                 | stream.rs:169       | 接続スコープ             | 自己終了                                            |
| broker セッションタスク   | banto-broker lib.rs | 収集 run-context         | D8 の stop-and-join（新規）                         |

決定:

- computed 評価ループと剪定ループは**収集 stop では止めない**（プロセス/runtime 寿命）。演算・内部タグは
  PLC 非依存（plan §4.3(a)）なので停止中も catalog に対し評価継続（`current_values()` が `None` の間、
  PLC 由来入力は Bad になるだけ）。`RunningHub::shutdown` でのみ止める。
- **収集 run-context 寿命のタスクは Collector 内部タスク（`Collector::stop` が畳む）と broker セッション
  タスク（D8）のみ**。controller の stop はこの2種を止める。

## 10. D8 — broker per-connection stop-and-join（P2）

現状、再開可能な収集停止に必要な「1接続を止めて終了を待つ」API が無い（§2）。plan §5.3 の
「接続単位の remove 等で再利用可能へ戻す」は現行 `remove`（join 非保証）だけでは T14 受入
「PLC セッションが残らない」を検証可能な形で満たせない。

**推奨: banto-broker（`SessionDirectory` / `BrokerSupervisor`）に再利用可能な per-connection
stop-and-join を追加**（P2）。

```rust
// banto-broker
impl SessionDirectory {
    // per-task の停止シグナルを送って当該タスクの JoinHandle を await する（bounded）。
    pub async fn stop_and_join(&self, connection_id: i64) -> bool;
}
```

設計:

- 各セッションタスクに **per-task の停止シグナル**（`watch::Sender<bool>` か `oneshot`）を `tasks`
  マップと並置し、`run_broker_task` の `select!` に3本目の分岐を足す（現行は supervisor 共有 shutdown と
  mpsc close の2分岐, lib.rs:868-907）。
- `stop_and_join` は per-task シグナルを送ってから当該 `JoinHandle` を await する。**per-task
  シグナルがあれば残存 handle clone に関係なくタスクが break するため join は bounded**（現行
  `remove` が join を避けた理由＝clone 保持でハングし得る、が解消する）。
- 呼び出し順の契約は現行どおり維持: **収集タスク（ClientFactory 経由で clone 保持）を先に止めてから**
  `stop_and_join` を呼ぶ。controller の stop は `Collector::stop`（収集タスク停止・clone drop）→
  broker `stop_and_join` → simulator stop の順。
- `BrokerSupervisor::shutdown(self)`（全接続一括・終端）は現状のまま残す。relay-wright は新 API を
  使わない（remove すら未使用, lib.rs:158-163）。共有クレートの表面積は「1タスク1 watch + select 1分岐 +
  1メソッド」に限定。
- `HubSessions` に `stop_and_join(connection_id)` を追加し `SessionDirectory` へ委譲。
- `SlmpSimRegistry` は既に再利用可（drain, broker_glue.rs:512）なので変更不要。停止シーケンスでは
  現行 `remove_stale_slmp_sessions`（sessions.remove + sim_registry.remove のペア, hub.rs:1070-1074）を
  stop-and-join ベースへ差し替える。
- **停止 step5 の完了条件 = broker タスクの JoinHandle 解決**。テストは停止後
  `connection_count()==0` かつ TCP レベルで ESTABLISHED セッションが残らないことを確認（§14）。
- 補足: 再開時は新タスク = `status_watch` の Receiver・attempt カウンタがリセットされる（lib.rs:753）。
  停止中は `status_watch` が `None` を返す穴（hub.rs:1132）があるため、`/status` の SLMP 状態は
  停止中「収集停止」を明示表示する（`connection_status` を状態機械側の値で上書き）。

## 11. §7 停止シーケンスの確定手順

controller の stop / mode 切替は次の順で実行する（plan §7 を本設計の API へ具体化）:

1. `transition` ロックで直列化し、`Stopping` を公開（`status_tx`）。
2. `write_control.disable()`（自動復元しない）。
3. 値消費・外部 publish を停止/停止状態へ: MQTT は running watch で自然に停止側へ、gRPC 値 stream は
   （T15 で SIM 対応時に能動終了、T14 では収集停止に伴い Bad 化）。
4. `Collector::stop()` で収集タスク停止 + tstore flush（collector.rs:577）。
5. broker `stop_and_join`（各接続、D8）でセッションを join。
6. simulator 停止（`SlmpSimRegistry::remove`/stop）。
7. 現在値を `Bad`/`null` 相当へ、`Stopped` を公開、`running_revision` 前進。

失敗時は `faulted` へ入り、実機収集を自動再開しない。

## 12. T14 サブスライス分割と受入条件

plan §15「大規模項目は1サブスライス1PR」を T14 へ適用（rest.rs 4.8k 行・hub.rs 1.5k 行が対象のため
上位モデルの差分レビュー粒度に収める）。依存はほぼ直線 T14-1 → T14-2 → T14-3 → T14-4。

### T14-1: HubRuntime 抽出（挙動不変）

- D1 / D7 の JoinHandle 捕捉。`expect`→`HubStartError`。3ホストを薄い層へ。収集セマンティクスは不変。
- 受入: 既存 Rust/フロント/E2E テスト green。起動時 rebuild が現行どおり走り、シャットダウン順序が不変。
  `HubRuntime::start` の失敗が panic でなく `Result` で返る。

### T14-2: 状態機械 + 再開可能 controller + broker stop-and-join

- D2 / D8。`CollectionController`（状態・遷移直列化・run_id）、banto-broker stop-and-join + HubSessions 委譲。
  起動時挙動をホスト別に切替（デスクトップ=Stopped、サービス=Configured 開始）。
- 受入: start/stop を繰り返して tstore が flush され PLC セッションが残らない（TCP 検証）。多重 start /
  start-stop 競合で Collector が二重起動しない。SCM 起動後の `configured` 開始を維持し、失敗時は `faulted`。
  デスクトップ起動直後に PLC へ TCP 接続しない。

### T14-3: rebuild 分割 + 全構成 preflight + 2レビジョン

- D3 / D4 / D5。`commit_catalog` / `apply_run` 分割、`RegistrySnapshot` + `build_*_from`、savepoint
  preflight（単票/一括/CSV/開始で共有）、`configured_revision` / `running_revision` + `status_tx`、
  MQTT を running 発火へ、`/status` に両世代 + run_id。
- 受入: 不正 Modbus/SLMP アドレス・未解決参照・循環を保存時と開始時に同一規則で検出。保存成功構成は
  必ず実行可能。`configured_revision` と `running_revision` の差を識別できる。停止中の保存が collector を
  起動しない。連続/CSV の dry-run が偽陽性を出さない。

### T14-4: 状態/制御 API + 運転中編集ロック(409) + 書き込み受付連動

- D6 / plan §5.4。`GET /status` 拡張 + admin 制御 API（開始/全 PLC SIM 開始/停止、admin 権限・
  Origin/Host 検証・監査・冪等）。運転中の CRUD/一括/CSV/直接 REST/gRPC 書込を `409 Conflict` +
  現在状態で拒否。全遷移先頭で `write_control.disable()`（自動復元なし）。
- 受入: 運転中の直接 REST を含む構成変更が `409` になり実行構成を変えない。画面は「停止して編集」を表示。
  停止完了前に編集可能へ戻さない。停止中の REST/gRPC 書込を fail-closed で拒否。

## 13. 未解決事項・T15 以降へ送るもの

- **全 PLC シミュレーション（`RunMode::AllSimulation` の実体）**は T15。本設計は enum と遷移の器だけ用意。
- **MQTT/gRPC の SIM 時ポリシー**（全 PLC SIM 中は既定 OFF・既存 stream 能動終了・テスト出力）は T15。
  T14 では MQTT を running 発火へ寄せる配線までを行う。
- **HubSessions の write 可能 peek（spawn 無し）** は T15（write-during-SIM）で追加。
- **desktop⇔service 切替の中間状態**（2プロセス + SCM を跨ぐ）は T16/T17。本設計の状態機械は単一
  プロセス内に閉じる。切替進行はシェル（ネイティブ側）が所有する（plan §16.3 参照）。
- `RuntimeStatus` のワイヤ表現（フィールド名・後方互換）は T14-3/T14-4 実装時に最終確定。

## 14. テスト計画（T14 分）

- 収集状態機械の単体: 遷移表（§4.1）全経路、冪等（遷移中の再要求で現在状態）、faulted 非自動再開。
- start/stop 反復統合: tstore flush、broker セッション残留ゼロ（TCP レベル）、二重 start 防止。
- preflight: 不正 Modbus/SLMP アドレス・未解決参照・循環を保存時/開始時に同一検出。savepoint
  ロールバックで DB・`configured_revision`・collector を変えない。dry-run 偽陽性の回帰。
- 2レビジョン: 停止中保存で configured のみ前進、開始で running が追いつく。`/status` 両世代 + run_id。
  MQTT が保存では撃たず Running 遷移で全量再発行。
- 書き込み: 遷移で write_control OFF・自動復元なし。停止中 REST/gRPC 書込を fail-closed 拒否。
  停止中の書込試行が新規 TCP セッションを発生させない。
- 運転中編集ロック: 単票/一括/CSV/直接 REST/gRPC 書込が `409`、実行構成不変、二重再構成なし。
- banto-broker stop-and-join: per-connection 停止で join 完了、再 ensure で新規 spawn・read 成功。
- 既存 banto-hub core / フロント / ワークスペーステストの回帰。
