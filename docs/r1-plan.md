# R1 実施計画 — ChronoGazer アプリ骨格 + 設定 + 監視画面

作成日: 2026-07-13（I0〜I4 完了時点で司令塔が事前設計。新セッションへの引き継ぎ文書を兼ねる）
状態: **運用中**。R1-A（アプリ骨格）は完了。R1-B（レジストリ CRUD）は #383
段階2a で着手し、PLC接続/収集グループ/タグの3エンティティ CRUD が両経路
（REST/Tauri）・設定画面まで実装済み（2026-09-18）。R1-B のうち表示グループは
未着手（Cargo.toml のコメントどおり）。**R1-C（収集ランタイム統合）は
C-1〜C-4 がすべて入り、完了条件を満たした**（2026-09-23。内訳は
[R1-C の節](#r1-c-収集ランタイム統合)）- シミュレータ（別プロセスの開発用
PLC）相手に「設定 → 収集開始 → データファイル生成 → イベント記録」の一巡が
通ることを、Rust の統合テストと E2E で固定した（C-4）。R1-D は未着手。
**2026-09-23 オーナー決定（#413）**: R1-B で「banto-hub 固有」として閉じていた
**接続単位シミュレーション（製品の `simulation`）を開けた**（実装済み。[R1-B の節](#r1-b-設定画面レジストリ-crud--表示グループ)）。
シミュレーション接続の値が**データファイルに記録されない**約束は変えていない
（C-4 の一巡は別プロセスの開発用 PLC を普通の接続として登録するので、記録される）。
**2026-09-23 オーナー決定（#414 段階2）**: 収集の開始時に**不正なタグ・グループ・
接続だけを外して残りを動かす**ようにした（実装済み。[R1-C の節](#r1-c-収集ランタイム統合)）。
外したものは `/settings/collect` の「除外あり（N 件）」と `/tags` の一覧で分かる。
外したタグの値は DB 上 null。**R1-D の 4 種の表示すべてで null を 0 と区別して
表示する**ことを R1-D の要件に加えた。
2026-09-06 時点で一度「アーカイブ・R1 完了」と誤って記録されたが、
`apps/chronogazer/core/Cargo.toml:21-25` のコメントが当時から一貫して
「まだ配線されていない」と明記しており実態と食い違っていたため訂正した
（2026-09-18）。
最終検証日(コード照合): 2026-09-18
前提: [recorder-requirements.md](recorder-requirements.md)（R0、未決事項ゼロ）と [plan.md](plan.md) §4。
実施プロセスは banto と同じ（司令塔が設計・分割 → 実装は general-purpose(sonnet)、
難所は opus に委譲 → 検証 → Phase 毎コミット → PR + CI → ユーザーマージ）。

## 消費するバージョン

- banto は **git タグ `v1.1.0`**（2026-08-09 現在 v1.1.0 に統一。最新は
  Cargo.toml / package.json を正とする）
  - npm: `pnpm add "github:tyaro/banto#v1.1.0&path:packages/admin-core"` 等
  - Rust: `{ git = "https://github.com/tyaro/banto.git", tag = "v1.1.0" }`
  - **2026-09-01 時点の実値**: npm `@banto/*` は `v1.2.0`、Rust クレートは
    `v1.4.0`（両者は独立に追従するため一致しない。上記は 2026-08-09 時点の
    記録としてそのまま残す。実際の値は各 `package.json`/`Cargo.toml` を正とする）
- I 系クレート（banto-tags/plc/tstore/collect/tsquery）は同一ワークスペースの path 依存

## Phase 分割

### R1-A: アプリ骨格（テンプレートコピーの実地検証を兼ねる）

- banto-industrial に pnpm ワークスペースを新設（現状 cargo のみ。
  pnpm-workspace.yaml + ルート package.json。banto の lint/format 構成も
  この機に持ち込む — prettier/eslint 設定は banto からコピー）
- banto の README「テンプレートから自分のアプリを作る」手順どおりに
  `apps/admin-template` → `apps/chronogazer` をコピー・リネーム
  - **既知の追加作業**（手順は同一リポジトリ内コピーを想定しているため）:
    `@banto/*` の `workspace:*` 参照 → git 依存（`#v0.1.1&path:`）への
    書き換え、Rust の path 依存 → git tag 依存への書き換えが必要
  - デモリソース（items 一式・ダッシュボードパネル）は手順どおり削除。
    ナビは 監視/ヒストリカル(R2 プレースホルダ)/イベント/設定系 に置換
- Rust 側: `apps/chronogazer/core`（admin-template-core 由来。items を除去し
  banto-tags のマイグレーション適用 + I 系クレート依存を追加）
- 完了条件: デスクトップ（tauri dev）と LAN（banto-serve 相当）の両モードで
  ログイン → 空の監視ページ表示まで動く。CI に frontend/E2E ジョブ追加。
  **README 手順の穴をリスト化**（→ banto へのフィードバック PR を別途作成）

### R1-B: 設定画面（レジストリ CRUD + 表示グループ）

> **2026-09-18 実施状況**: 下記のうち「PlcConnection / CollectionGroup /
> Tag CRUD」と「画面: PLC接続 / 収集グループ / タグ」は #383 段階2a
> （`feat/383-registry-crud`）で実装済み（REST + Tauri 両経路、editor 以上・
> 監査記録、`/tags` 画面）。**表示グループ（新エンティティ・ペン割当 UI）は
> このPRのスコープ外で未着手** - 指示書（#383 段階2a）が明示的に3エンティティ
> の CRUD のみに絞ったため。表示グループをいつ・どの段階で実施するかは未定。

> **2026-09-23 オーナー決定（#413）: 接続単位シミュレーションを開ける。**
> R1-B の実装（#383 段階2a）は、PLC 接続の `simulation`（接続単位シミュレーション、
> banto-tags の列）を「banto-hub 固有機能」としてワイヤから落とし、REST/Tauri の
> 両経路で常に `false` を書いていた。これを変更し、chronogazer でも設定できる
> ようにした。理由（オーナー）:「**実機が無いときに設定できないのは使い物に
> ならない**」。
>
> - **ワイヤ**: `PlcConnectionPayload.simulation`（省略可。**作成で省略 = `false`**
>   = 送らない既存クライアントの挙動は不変、**更新で省略 = 既存の値を保つ** -
>   省略しただけで黙って実機へ接続しに行かないため。他の項目の更新は全項目の
>   置き換えのまま）、`PlcConnectionResponse.simulation`。REST と Tauri の
>   両経路で対称、監査の `detail` に `simulation` を残す。
> - **記録されない約束は維持**: シミュレーション接続の値は現在値・しきい値イベント
>   には出るが、**データファイル（tstore）には記録されない**（`banto-collect` の
>   約束で banto-hub と共有。変えない）。`/tags` の接続一覧と `/settings/collect`
>   の接続ごとの状態に「値は記録されません」を常に出し、統合テスト
>   `apps/chronogazer/core/tests/simulation_not_recorded.rs` で固定した。
> - **反映は「収集を再起動」**（レジストリの変更で自動再起動しない C-2 の決定の
>   まま）。`/settings/collect` の表示は**走っている収集が実際に使っている値**
>   （`GET /api/collect/connections` の `simulation`、viewer 以上）。
> - シミュレータが値を動かすのは先頭 16 番地だけ。範囲外のタグは `/tags` の
>   「シミュレーションで値が動かないタグ」に出る（判定は
>   `banto_collect::simulation::classify_plc_tag` だけ。`GET /api/simulation-coverage`
>   / Tauri `simulation_coverage_list`、viewer 以上）。
> - やっていないこと: シミュレーション値の記録、シミュレータの番地範囲の拡張、
>   書き込み。

- banto-tags の PlcConnection / CollectionGroup / Tag CRUD を REST + Tauri
  両経路で公開（banto の users/audit ルーターの流儀。editor 以上、監査記録）
- **表示グループは新エンティティ**（chronogazer 固有、収集グループとは別物 —
  R0 §2: 表示の単位・最大8ペン・表示種別 トレンド/デジタル/バー/計器）。
  app DB にテーブル + CRUD + 画面
- 画面: PLC接続 / 収集グループ / タグ（一覧グリッド + フォーム、
  BantoGrid/BantoForm）/ 表示グループ（ペン割当 UI）
- 完了条件: 全 CRUD が両経路で動き、検証エラーが人間可読で出る。
  viewer は閲覧のみ・変更 403

### R1-C: 収集ランタイム統合

- Collector のライフサイクル管理: 起動時に build_config → start、
  設定変更後の「収集を再起動」操作（editor 以上、監査記録、
  tstore は構成ハッシュ変化で自動ローテーション）
- 接続状態/ヘルス表示（status()）、collect_events のイベント一覧ページ
  （banto 監査ログページの流儀）、現在値 API（CurrentValuesHandle →
  REST ポーリング + Tauri。SSE 化は R1-D の必要に応じて）
- 完了条件: シミュレータ（banto-plc simulator を dev 用 PLC として起動する
  dev コマンド/フィーチャを用意）相手に、設定 → 収集開始 → データファイル
  生成 → イベント記録まで一巡

#### 実施の分割（#383 段階2b、2026-09-23 時点）

**この完了条件を満たすのは C-4 の一巡が通ったとき**で、C-1/C-2/C-3a/C-3b が
入っただけでは「動く」とは言えない - **C-4 が入り、完了条件を満たした**
（2026-09-23。下の「C-4 で満たしたこと」）。

|      | 範囲                                                                                                                                                                                                                                        | 状況                 |
| ---- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------- |
| C-1  | 収集サービスの骨格（`chronogazer_core::collect::CollectorService`。`start`/`stop`/`restart`/状態、ライフサイクル専用タスク、終了時の停止）                                                                                                  | **実装済み**（#406） |
| C-2  | 操作の口（`collect_*` コマンドと `/api/collect*`。読み取りは viewer 以上・変更は editor 以上、監査 `resource: "collect"`）、起動時の自動開始、無応答への上限と `Starting` 状態                                                              | **実装済み**（#407） |
| C-3a | 読み出しの口（`GET /api/collect/{values,connections,events}` と `collect_values`/`collect_connections`/`collect_events_list`。viewer 以上、「走っていない/読めなかった/0 件」を別の結果にする `Readout`、現在値をキューから外して葉に公開） | **実装済み**（#408） |
| C-3b | 画面（`/settings/collect` の状態表示・接続ごとの状態・操作、`/events` のイベント一覧ページ）。**現在値の表示は含まない** - 値の表示は R1-D の監視画面                                                                                       | **実装済み**（#409） |
| C-4  | シミュレータ・ハーネスと E2E（設定 → 収集開始 → データファイル生成 → イベント記録の**一巡**）                                                                                                                                               | **実装済み**（#412） |

C-4 で満たしたこと（完了条件の「dev 用 PLC として起動する dev コマンド/
フィーチャ」と「一巡」）:

- **開発用 PLC は製品の外の別プロセス**: `apps/chronogazer/core/examples/dev_plc.rs`
  （`pnpm dev:plc`、`--protocol modbus|slmp`・`--port`）が
  `banto_collect::simulation::start_on` でランプ波シミュレータを固定ポートに
  立てる。ChronoGazer からは**普通の Modbus TCP / SLMP 接続**
  （`127.0.0.1:<port>`）として登録する - 接続単位のシミュレーション
  （`simulation`）は値が tstore に記録されない
  （`crates/banto-collect/src/task.rs` の `if ctx.simulation { return; }`
  付近）ため、データファイル生成まで含む一巡はこの経路でしか確かめられない
  （製品の `simulation` は #413 で設定できるようにしたが、記録されない約束は
  そのままなので、一巡の確認は引き続きこの経路。R1-B の節）。実際のワイヤ経路
  （接続・読み取り・デコード・tstore への書き込み・イベント記録）をそのまま通る。
  ポートが使用中なら panic せず、分かる文言で非ゼロ終了する。
- **一巡の固定は 2 段**: Rust の統合テスト
  `apps/chronogazer/core/tests/collect_roundtrip.rs` が **Modbus と SLMP の
  両方**で、状態 `Running`・現在値が `good` で変化すること・データファイルに
  **値の入ったサンプル行**があること・`collection_started`/`plc_connected`・
  停止後の `collection_stopped` と `Stopped` を確かめる。E2E
  `e2e/tests/user-simulator-roundtrip.spec.ts` は **Modbus** で、画面から
  接続・グループ・タグを作り、`/settings/collect` で開始 → 「収集中」と
  「接続中」→ データディレクトリにファイル（Node の `fs` で確認）→ `/events` に
  「収集開始」「PLC接続」→ 停止して「停止」と「収集停止」までを通す。
- 残る制約: E2E の一巡は Modbus だけ（SLMP の一巡は Rust 側で固定）。E2E は
  先行スペック（`tags.spec.ts`）が残す接続を一時的に無効化して走り、終わると
  元に戻す（実在しうる機器のアドレスへ E2E から接続しに行かないため、また
  一巡の確認に他の接続の状態・イベントを混ぜないため。当初の「Modbus として
  解釈できない `D3000` のタグで構成の組み立てが失敗する」理由は、#418 の保存時
  検査と #414 段階2 の「外して残りを動かす」で成り立たなくなった。詳細は
  スペックの doc）。

#### 2026-09-23 オーナー決定（#414 段階2）: 開始時に不正な設定だけを外し、残りを動かす

「開始時に不正なタグ・接続だけを外し、**残りを動かす**。**不正なものがあることは
必ず分かるようにする**。**DB は null でよい**。トレンドや計器の表示でも null と
分かればよい」「**除外は収集イベントには記録しない**」（2026-09-23 オーナー）。
それまでは `banto_collect::build_config`（厳格版）で組んでいたので、保存時の
検査（#418 = 段階1）より前に入った不正なタグが 1 本あるだけで、**正常な接続も
含めて何も収集されなかった**。実装済み（#414 段階2）:

- **組み立て**: `banto_collect::build_config_lenient_from` を足した。失敗の単位は
  厳格版と同じ 3 段で、**不正な接続**（プロトコル・ポート・ユニット ID）は配下の
  グループ・タグごと、**不正なグループ**（収集周期）は配下のタグごと、
  **不正なタグ**（アドレス・データ型・ビット指定）はその 1 本だけを外し、外した
  一覧（単位・id・キー・名前・理由の分類と文言）を返す。**厳格版は「緩い版の
  最初の除外で失敗する」形に寄せ、banto-hub から見える挙動と文言は一字一句
  同じ**（段階2 の前の実装に対して先に通したゴールデンテストで固定）。
  banto-hub は引き続き厳格版を使う。
- **状態**: 有効なタグが 1 本でも残れば `running`、全部外れたら `noTargets`
  （`startFailed` にはしない）。除外の一覧は**状態の中に**持つ
  （`running` / `noTargets` の `exclusions`）ので、停止・再起動で状態が変われば
  一緒に入れ替わり、前回の一覧は残らない。起動そのものが失敗した
  （`startFailed`）ときは一覧を持たない。
- **公開範囲**: `GET /api/collect` / `collect_status`（viewer 以上）に一覧を載せる。
  載せるのはレジストリの行 id・キー・名前・理由の分類と文言だけで、文言に入るのは
  その行が持つ値（プロトコル名・ポート番号・ユニット ID・収集周期・アドレス・
  データ型）と親の名前 - どれも `GET /api/plc-connections` 等で viewer が既に
  読める。接続先ホスト・資格情報・ファイルパスは載らない（実際の JSON をテストで
  確認）。
- **現在値**: 外したタグは `/api/collect/values`（と `collect_values`）で
  `value: null`・`ptimeMs: null`・品質 **`invalid`**。`invalid` は chronogazer の
  公開型 `QualityView` にだけ足した（`banto_collect::Quality` は変えない）。
  通信エラーの `bad` と区別する。
- **DB = null（方式 B: 列を作らない）**: 外したタグには tstore の列を作らない。
  `banto-tsquery` は元から「そのファイルのスキーマに無い `tag_key`」を欠測
  （`null` / 間引きでは `Gap`）として返すので、外したタグの履歴を読むと**エラー
  ではなく null** が返る（`apps/chronogazer/core/tests/exclusion_roundtrip.rs`）。
  方式 A（列を残して常に NULL を書く）は、収集タスクの「読み取り要求 i ↔ タグ i
  ↔ 列 `c{i+1}`」の位置対応を崩す変更が `banto-collect` の書き込み経路に要り、
  読み出し側に新しい意味も増えないので採らなかった。外したタグを直して
  再起動すると列が増えて構成ハッシュが変わり、ファイルがローテーションする
  （タグを足したときと同じ）。接続・グループごと外した場合は、そのグループの
  行が無い（欠測）。
- **分かるようにする経路**: `/settings/collect` の「除外あり（N 件）」と一覧
  （種類・名前・理由）と `/tags` へのリンク、`/tags` の「収集の開始時に外される
  設定」（収集が走っていなくても出る。判定は収集の開始と同じ Rust の組み立て
  だけ = `GET /api/config-exclusions` / Tauri `config_exclusions_list`、viewer
  以上。TS に書き写さない）。タグの理由の文言は #418 の保存時の拒否理由と同じ。
  **収集イベントには記録しない**（`EventKind` は増やさない）。
- 残る制約: 語順（`word_order`）は不正な値でも既定へ倒れる（厳格版と同じ。
  除外の理由にはならない）。`/tags` の一覧は 3 つの一覧を別々に読んで判定する
  （同じトランザクションではない - 編集のたびに取り直すので次で追いつく）。
  外した理由は画面を再読み込みしても状態から読める（起動失敗の理由とは違う）。

C-3b 時点の既知の制約（いずれも設計判断として記録済み。詳細は
`apps/chronogazer/core/src/collect.rs` のモジュール doc と
`apps/chronogazer/src/lib/banto/collectAdmin.ts`）:

- **レジストリ（接続・グループ・タグ）の変更では自動再起動しない。** 反映は
  明示的な「収集を再起動」操作だけ（本文の「設定変更後の『収集を再起動』
  操作」どおり）。
- **同じ `data.dir` を 2 プロセスで開く防止機構は無い。** デスクトップアプリと
  `banto-serve` を同時に起動して同じ `data.dir` を指すと二重書き込みになる。
- 保持期間（`retention.days`）による自動削除は**まだ何もしない**（R0 §3.4 の
  機能だが別途）。
- **起動に失敗した理由が状態表示に出ない。** 状態の読み取りは viewer 以上に
  開いているため、公開用の `CollectorStateView` は `startFailed` の `reason`
  （接続先やパスを含みうる）を落としてある（C-2 レビュー P2-2）。画面は
  「操作すると理由が返る」ことを説明し、実際に操作したときの理由
  （`collect_start` のエラー、または `CollectOutcome.status` の `reason`。
  どちらも editor 以上＝操作した本人だけが受け取る）を必ず出す。**画面を
  再読み込みすると理由は分からなくなる**のは C-2 からの残る制約のまま。
- **イベント一覧に `detail` 列が無い。** C-3a が型でも SQL でも落としている
  （自由文で、切断理由や書き込み先のファイルパスを含みうる）ため、切断理由や
  書き込み失敗の詳細は画面から見られない。必要になったら発生源
  （`banto-collect`）で「見せてよい理由」を分類するのが筋。

### R1-D: 監視画面（グループ表示4種 + リアルタイムトレンド）

- 表示種別: トレンド（M13 LineChart ストリーミング、既定窓10分・選択可、
  しきい値は bands）/ デジタル（数値大表示 + 品質色分け）/ バー（縦バー +
  しきい値色）/ 計器（charts Gauge）。デジタル/バーは chronogazer 内の
  軽量コンポーネント（テンプレートに入れない — 4条件を満たさない）
- 現在値ポーリング（グループ周期に同期、Stale/Bad の視覚化）+
  トレンド初期窓は I4 read_decimated → 以後 append
- **null を 0 と区別する（2026-09-23 オーナー決定、#414 段階2）**: トレンド・
  デジタル・バー・計器の **4 種すべてで**、値が無いこと（欠測・`Gap`・現在値の
  `value: null`、とくに開始時に外したタグの品質 `invalid`）を **0 と区別して**
  表示する（トレンドは線を切る、デジタルは数値を出さず「—」等、バー・計器は
  0 の位置に描かない）。`invalid`（設定が不正で外した）は `bad`（通信エラー）とも
  区別する（`collectAdmin.ts` の `qualityLabel`）。
- グループ切替（タブ + コマンドパレット）、キオスク（認証無効モード）確認
- 完了条件: シミュレータ相手に 4 種すべてが実データで動き、
  断線 → Bad 表示 → 復旧が目視確認できる。**null（欠測・`invalid`）が 4 種
  すべてで 0 と区別して表示される**ことも確認する。ブラウザ実機検証必須

## 検証方針

- 各 Phase で `pnpm check` / `pnpm lint` / `cargo test --workspace` +
  ブラウザ実機（preview ツール、LAN モードは実バックエンド）
- R1 完了時にミニソーク: シミュレータ相手に収集 30 分 + UI 閲覧で
  行欠落・メモリ増加がないこと（72h ソークは R4）

## 新セッションの開始手順（引き継ぎ）

1. 永続メモリ（banto-industrial-repo / model-delegation-rules /
   pr-merge-workflow）が読み込まれていることを前提に、本ファイルと
   recorder-requirements.md を読む
2. R1-A から順に、Phase 毎に feature ブランチ → 実装委譲 → 検証 →
   PR + CI → ユーザーのマージ承認、で進める
3. README コピー手順の穴が見つかったら banto へのフィードバック PR を
   忘れずに（R1-A の完了条件）
