# タグサーバーアプリ 設計ドキュメント（草案）

作成日: 2026-08-04
状態: **実装追従中（2026-09-01 更新）**。起案時は設計先行だったが、
apps/banto-hub として実装が進行 — T0〜T18-6 実装済み・残 T18-5c/d
（Windows 実機往復・72h soak）と P3-b の残件（SLMP CPU 種別/アクセスルート露出、
バックログ。word order 自体は #127 で完了済み）。実装状況は §9（T系）の表を正とする。マイルストーンは §9、
オーナー判断待ちの未決事項は §10。T14 以降の運転計画・UI/UX 決定台帳は
[banto-hub-desktop-plan.md](banto-hub-desktop-plan.md)、docs 全体の地図は
[README.md](README.md)。UX 改善（T9〜T13）の経緯は [ux-plan.md](ux-plan.md)（アーカイブ）。
**2026-08-31 追記**: §5.6「試運転モードとロックダウン」のバックエンド・
UI 実装が完了（起動時ガード・認証バイパス・ロックダウン API・ルートガード
迂回・消せない警告バナー）。同節に管理 UI と `/api/v1/*` の認証境界（案A）
を追記。§4 のタグ名一意性を全体一意→収集グループ内一意へ緩和
（migration `0011`、実機で判明した複数台同型構成の不具合対応）。

---

## 0. 背景と目的

banto-industrial の I 系クレート（タグレジストリ・PLC 通信・収集エンジン・
時系列ストレージ/クエリ）は、現状 ChronoGazer / relay-wright という **2つの
Tauri アプリに in-process で埋め込まれて**いる。この構成は単体アプリとしては
正しいが、次の制約がある:

1. **PLC セッションの重複**: 同一 PC・同一 PLC に対して複数アプリを同時稼働
   させると、各アプリが自前のソケットを張る。古い FX/Q 系や Modbus 機器は
   同時セッション数が 1〜2 のものがあり（plan.md W5 の実機検証項目）、
   正面からぶつかる。
2. **外部システムとの接続点がない**: MES / 上位 SCADA / 自作ダッシュボード /
   クラウド収集などから現在値を読む・購読する標準的な口が存在しない。
3. **収集の寿命が UI プロセスと同じ**: ChronoGazer の 24/365 収集は Tauri
   プロセス内で動くため、UI を終了すると収集も止まる。

これらを解決するのが**独立プロセスのタグサーバー**である。参考製品は
[FA-Server 6（ロボティクスウェア）](https://www.roboticsware.com/jp/faserver6/):
「タグ = 共有メモリ的なデータ空間。PLC デバイス値が常時タグへ反映され、
タグへの書き込みは対応デバイスへ自動転送される。外部アプリはタグ空間へ
自由にアクセスする」というモデル。本設計はこのタグ空間モデルを
banto-industrial の資産で実現し、外部公開インターフェースとして
**REST / WebSocket / MQTT / gRPC** を段階的に載せる
（**OPC UA は工数対効果から後回し** — §5.5）。

## 1. 位置づけ

```
                         ┌──────────────────────────────┐
  PLC (Modbus TCP) ──────┤                              ├── REST (T0)
  PLC (MELSEC SLMP) ─────┤   タグサーバー（本設計）      ├── WebSocket (T1)
                         │   I1+I2/I2a+I3a/I3b を内蔵    ├── MQTT publish (T3)
                         │   タグ空間 = 現在値キャッシュ  ├── gRPC (T4)
                         └──────────────────────────────┘    (OPC UA: 将来)
                                     │
                          自己記述 tstore ファイル（I3a）
                          → banto-tsquery（I4）で別プロセスからも読める
```

- **リポジトリ内の位置**: `apps/` 配下の3本目の製品アプリ。I 系クレートを
  束ねる消費者であり、クレート側の書き直しは前提としない（必要な小改修は
  §10 に列挙）。
- **既存2アプリとの関係**: v1 では ChronoGazer / relay-wright は**現状のまま
  独立動作を続ける**（同居時のセッション多重化は §7 の移行ロードマップ）。
- **アプリ名**: **`banto-hub`**（2026-08-04 オーナー決定）。製品群の中心で
  タグ空間と catalog を提供する「ハブ」の役割をそのまま表す。配置は
  `apps/banto-hub/`。以下本文書では役割名として「タグサーバー」とも呼ぶ
  （= banto-hub のこと）。

### 1.1 FA-Server との概念対応（マニュアル調査 2026-08-04）

> 調査手段の注記: 当初は検索スニペット由来の二次情報だったが、
> **2026-08-04 にマニュアル本体（v6.0.17）を直接取得して一次情報で裏取り済み**。
> 本節・§4.2・§5.2 の FA-Server 記述は一次情報準拠。

FA-Server は**タグ・イベント・アクション・ビュー・インターフェース**の
5概念で構成され、タグは **Unit / Folder / Tag の3層構造**を持つ
（[タグ編](https://docs.roboticsware.com/ja/6.0.17/fa-server/contents/cmn_tag.html)、
[タグの基本](https://docs.roboticsware.com/ja/6.0.17/fa-server/contents/cmn_tag_overview.html)）。
各層の役割は本製品群の I1 3層と正確に対応する: **Unit** は PLC 1台毎の
接続設定（= `PlcConnection`）、**Folder** は**値の更新周期のグループ化**と
マルチドロップ/ネットワーク経路設定を担い、ネスト不可
（= `CollectionGroup` の周期グループと同じ役割・同じ非ネスト制約）、
**Tag** はデバイス1点 = 1タグで、データ型・アドレス・型変換/工学値変換
フィルタを持つ（= `Tag` + スケーリング）。タグパスは Unit / Folder / Tag を
ピリオド `.` で連結（例 `U01.F01.T01`）— 本設計の外部名
`{connection}.{group}.{tag}`（§4）と区切り文字まで一致する。
本設計との対応:

| FA-Server                                                                          | 本製品群                                                                   | 備考                                                                                                                                                                                                                                                                                                |
| ---------------------------------------------------------------------------------- | -------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Unit / Folder / Tag                                                                | `PlcConnection` / `CollectionGroup` / `Tag`（I1）                          | 3層構造が偶然一致 — 外部名 `{connection}.{group}.{tag}`（§4）は業界慣行と整合（区切りも同じ `.`）                                                                                                                                                                                                   |
| スタティックタグ（事前登録・PLC通信）                                              | タグ空間 = `CurrentValuesHandle`                                           | §3.2。FA-Server の既定形態。マニュアル自身も「迷ったらスタティック推奨」（変更影響の局所化・未接続時もタグ一覧で確認可） — catalog バインドモデル（§4.1）と同じ論拠                                                                                                                                 |
| アクティブタグ（`#` 前置で PLC アドレス直接指定、事前登録なし。例 `U01.F01.#D0`）  | **意図的に非対応**                                                         | 本設計はキャッシュ迂回のオンデマンド直接読み出しを提供しない（§5.1 の設計原則）。catalog バインドモデル（§4.1）とも相反する                                                                                                                                                                         |
| タグ（内部演算ワークエリア）                                                       | 演算タグ・内部タグ（§4.2、T6）                                             | サーバー側で一元実装（2026-08-04 決定）                                                                                                                                                                                                                                                             |
| エイリアスタグ（CSV 定義ファイルで別名→実タグパスを対応付け。読み書き両対応）      | 非対応（安定 ID で代替）                                                   | FA-Server での主用途は「既設 OPC クライアントのタグ名を変えずにリプレース」。本設計のリネーム追従は §4.1 の安定 ID + revision が担い、別名層は導入しない（同種の移行要件が実案件で出たら再考）                                                                                                      |
| システムタグ（二重化状態・システム情報の参照）                                     | `/api/v1/status` で代替                                                    | タグ空間には混ぜない（値と運用情報を API で分離）                                                                                                                                                                                                                                                   |
| ネットワークタグ（ノード間連携。2階層 `App/Tag`・区切り `/`）                      | 非スコープ                                                                 | FA-Server 側でも旧方式（下位互換用）で、現行は IPLink サーバーユニットが後継。複数サーバー構成が必要になったら検討                                                                                                                                                                                  |
| イベント                                                                           | `CollectEvent` + WS/gRPC/MQTT 配信                                         | §5                                                                                                                                                                                                                                                                                                  |
| アクション（スクリプト SC1/SC2・ロガー・メール等）                                 | **アプリ分担で置換**: 自動書き込みは relay-wright、通知は非スコープ        | モノリスにしない方針（§2）。§4.2 に文法比較あり                                                                                                                                                                                                                                                     |
| ビュー（タグリスト・タグモニタ等の画面）                                           | **アプリ分担で置換**: 記録・トレンドは ChronoGazer、監視画面は将来の SCADA | 同上。管理 UI のタグ一覧・接続状態モニタが最小限のビュー相当                                                                                                                                                                                                                                        |
| インターフェース（OPC DA 1.0/2.0 / DDE (EX_Table・CF_Text) / IPLink / Redundancy） | REST / WebSocket / MQTT / gRPC                                             | OPC **UA** を将来枠に（DA/DDE はレガシーのため追わない）。IPLink は同社独自の TCP/IP クライアント/サーバープロトコル（§5.2 注記）。Redundancy（メイン/サブ二重化）は本設計では非スコープ — この構図は §7 の (A) データプレーンクライアントと同型（初版で「Panel IF」と記した経路の正式名は IPLink） |

FA-Server が1製品に統合している機能群を、本製品群は「タグサーバー +
ChronoGazer + relay-wright + 将来の SCADA」に分割して受け持つ — 各アプリの
スコープの護り（§2）はこの分担の裏返しである。

**FA-Server に対する差別化点（2026-08-04 オーナー指摘）**: FA-Server は
**稼働中の動的構成変更ができない**（変更にサーバー再起動/リロードが必要）。
本設計はこれを解消する — オンライン動的変更を最初から要件とし（§4.3）、
変更の影響半径を「触った接続だけ」に閉じる。

## 2. スコープ / 非スコープ（v1）

### スコープ

- タグレジストリ（I1 と同一の3層モデル `PlcConnection` → `CollectionGroup` →
  `Tag`）に基づく**周期収集**と**現在値キャッシュ（= タグ空間）**の外部公開
- **読み取り**: REST（ポーリング）+ WebSocket（購読・プッシュ）
- **書き込み**: 明示的に許可したタグのみ（per-tag opt-in）、認証・監査ログ・
  レート制限付きのパススルー書き込み（§6）
- **MQTT publish**: 外部ブローカーへのタグ値発行（plan.md §3 で保留になっていた
  「MQTT クライアント（rumqttc + タグバインド）」をここで回収）
- **gRPC**: 現在値読み取り + サーバーストリーミング購読
- **演算タグ・内部タグ**（§4.2、T6）: PLC に紐づかないサーバー側タグ。
  演算は他タグの純関数のみ（副作用なし — FA-Server の「アクション」は作らない）
- **オンライン動的変更**（§4.3）: タグ定義の追加・変更・削除を稼働中に適用。
  影響半径は触った接続のみ（FA-Server の不満点解消 — §1.1）
- 管理 UI（banto テンプレート由来、タグ定義 CRUD + 接続状態モニタ + 監査ログ）
- Windows 常駐（コンソール起動 + 自動起動登録。サービス化は §8）

### 非スコープ（v1 で入れない線 — ChronoGazer の「SCADA 化の誘惑対策」と同じ規律）

- **条件付き・自動書き込み**（ルールエンジン）: relay-wright の専管。タグサーバーの
  書き込みは「外部クライアントの明示要求を1回転送する」パススルーのみ
- アラーム状態遷移管理（ACK 等）・多段通知
- 汎用画面エディタ / 可視化（トレンド表示は ChronoGazer の商品価値。タグサーバーの
  管理 UI は設定と状態確認まで）
- OPC UA / OPC DA サーバー（§5.5）
- **ロガー・日報・帳票機能**（FA-Server の FA-Logger / FA-Report 相当）:
  作らない（2026-08-04 オーナー決定）。記録は ChronoGazer の商品価値であり、
  タグサーバー側の tstore 書き込み（§3.3）はバックフィル用の内部実装で
  あって製品機能としてのロガーではない
- タグブリッジ（PLC↔PLC ゲートウェイ、FA-Server にある機能）・スクリプト —
  必要案件が出るまで保留

## 3. 全体アーキテクチャ

### 3.1 プロセス構成

**単一プロセスのヘッドレス axum サーバー**（workspace 既存の axum 0.8）。
Tauri は使わない — 管理 UI は banto テンプレートの SvelteKit 静的ビルドを
axum が配信する（ChronoGazer / relay-wright の LAN モードと同じ構造で、
`banto_server` の auth / CSRF / SSE をそのまま使う）。デスクトップ窓が
不要なサーバー用途なので、Tauri シェルを省くことで配布物が単純になる。

> **2026-08-09 追記**（[banto-hub-t16-design.md](banto-hub-t16-design.md) P1、
> T16-0 着手に伴う再解釈）: 上記の「Tauri は使わない」は、**ヘッドレス exe
> （`banto-hub.exe`、本節・§8「常駐」）を一次形態として維持する**という
> 決定として読み替える。[banto-hub-desktop-plan.md](banto-hub-desktop-plan.md)
> §8/§10/§16.3 の方針に従い、**薄いデスクトップシェル
> （`apps/banto-hub/src-tauri`、パッケージ名 `banto-hub-shell`）を二次
> ホストとして追加することを許可する** - `HubRuntime`
> （`banto_hub_core::runtime`）を埋め込み、WebView は本節末尾の「管理 UI の
> 共通利用」と同じく Hub 自身が配信する `http://127.0.0.1:<port>/` を
> 開くだけで、独自の `frontendDist`/`invoke` API は持たない（二重 UI 化
> しない）。禁止していたのは「収集ランタイムを UI フレームワークへ埋め込む
> こと」「管理 UI をデスクトップ専用に二重実装すること」であり、どちらも
> このシェルでは発生しない（詳細は banto-hub-t16-design.md §2）。

**管理 UI の共通利用（2026-08-05 オーナー決定）**: 管理 UI の実体は
banto-hub が配信する1つだけとし、ChronoGazer / relay-wright には
「タグサーバー管理」メニューを追加して、設定に保存した hub URL
（`http://<host>:8722`）を **Tauri の WebviewWindow で開く**。これで
どのアプリからも同一の管理画面に入れる。アプリへの UI コンポーネント
組み込みは行わない — アプリ同梱 UI と稼働中 hub の API のバージョンずれを
構造的に排除するため（UI とサーバーは同じ exe に同梱され常に一致）。
認証は hub 側の bearer セッションログインを窓内でそのまま使う。
管理 UI を触るのは管理者のみという運用前提。将来の SCADA の catalog
ブラウズ/バインド画面はこれとは別物で、`banto-tagclient` SDK（§7）による
アプリ内統合の領分。

### 3.2 内部構造 — 「タグ空間」は新規実装しない

FA-Server の「タグ空間」に相当するものは **`banto-collect` の
`CurrentValuesHandle`（現在値 + 品質のキャッシュ、Stale は読み出し時判定）が
既にそのもの**である。タグサーバーのコアは次の既存部品の束ね直しで成立する:

| 役割                   | 使う資産                                    | 備考                                                            |
| ---------------------- | ------------------------------------------- | --------------------------------------------------------------- |
| タグ定義・CRUD         | `banto-tags`（I1）                          | サーバー自身の SQLite に同居。`banto_tags::migrate` 起動時適用  |
| PLC 読み取り           | `banto-plc`（I2/I2a）                       | `banto-collect` 経由で使用（直接は触らない）                    |
| 周期収集・再接続・品質 | `banto-collect`（I3b）                      | 接続毎1タスク・指数バックオフ・全NULL行記録の既存設計をそのまま |
| タグ空間（現在値）     | `CurrentValuesHandle`                       | 全外部 IF の読み取り側はこれだけを見る                          |
| イベント               | `EventSink`（broadcast + `collect_events`） | WebSocket / MQTT / gRPC のイベント配信源                        |
| ローカル記録           | `banto-tstore`（I3a）                       | §3.3                                                            |
| 書き込み               | `banto-plc-write`（I5）+ ブローカー         | §6                                                              |
| 認証・HTTP 骨格        | `banto_server`（banto 本体）                | bearer token + CSRF + SSE                                       |

構成変更の扱いも `banto-collect` の司令塔決定に従う: タグ定義の変更は
スナップショット（`build_config` → `CollectorConfig`）の再構築 + `Collector`
の作り直しであり、タグサーバー本体がレジストリ変更を検知して再起動を指示する
（ChronoGazer と同型）。

### 3.3 ローカル記録（tstore）の扱い

現状の `Collector` は `TsWriter` が必須（`data_dir` を要求し、日次ファイルへ
常時追記する）。選択肢は2つ:

- **(a) そのまま記録する（v1 採用 — 2026-08-04 決定）**: タグサーバーも tstore ファイルを書く。
  追加実装ゼロで、外部システムが取りこぼした期間のバックフィル読み出し
  （`banto-tsquery` は自己記述ファイルを別プロセスから読める）が無料で付く。
  保持期間は `prune_files` の設定で短め（例: 既定7日）にして記録計との
  役割重複を避ける。
- **(b) 純ゲートウェイモード**: `CollectorOptions` に writer 無効化を追加する
  小改修（I 系バックログ）。ディスク書き込みゼロが要件の現場向け。v1 では
  やらず、(a) の保持期間短縮で代替する。

### 3.4 PLC 通信ドライバとの関係

FA-Server の「通信ドライバ」に相当するのは `banto-plc`（I2/I2a）の
プロトコル実装であり、**hub のバイナリにコンパイル時に同梱される Rust
クレート**（プラグインや別プロセスではない）。関係は次の一方向の層構造:

```
banto-hub ──> banto-collect (I3b) ──> banto-plc (I2)  ──> ワイヤ
 タグ空間      接続毎1タスク・周期      PlcClient trait
 外部IF        再接続・品質判定         ├ ModbusTcpClient
 管理UI        読み取り計画の実行       ├ SlmpClient (I2a)
                                       └ シミュレータ（テスト用）
```

- **hub はドライバを直接呼ばない**: hub が見るのは `CurrentValuesHandle` と
  `CollectEvent` だけ。プロトコルの違いは `PlcClient` trait の背後に隠れ、
  banto-collect さえもワイヤプロトコルを知らない
- **ドライバの選択はデータ駆動**: I1 の `PlcConnection` が持つプロトコル種別・
  接続設定から `build_config` がスナップショットを作り、接続毎タスクが
  対応するクライアント実装を起動する（FA-Server で Unit がドライバを
  選ぶのと同じ構図）
- **役割分担は既存設計のまま**: banto-plc は読み取り専用・一括読み・
  再接続なし（アドレスパース・読み取り計画・デコードは純関数）。再接続
  ループ・バックオフ・品質判定は banto-collect の責務。書き込みは別クレート
  `banto-plc-write`（I5）の `PlcWriteClient` trait で、T2 で broker（I6）が
  読み書きを単一セッションに直列化する
- **対応プロトコルの拡張**: 新メーカー対応（例: オムロン FINS）は
  banto-plc への trait 実装追加 + I1 のプロトコル種別追加であり、hub 側の
  変更は不要。追加すれば ChronoGazer / relay-wright にも同時に行き渡る
  （ドライバはアプリ資産でなく I 系共有資産）。FA-Server の「100機種」に
  対する現在の対応は Modbus TCP / MELSEC SLMP の2系統 + シミュレータ

### 3.5 スレッド/タスク構造

```
main ──┬── Collector（banto-collect: PLC接続毎に1タスク、既存設計のまま）
       ├── axum サーバー（REST + WebSocket + 管理UI配信 + SSE）
       ├── MQTT publisher タスク（T3: EventSink 購読 + 周期スナップショット発行）
       ├── gRPC サーバー（T4: tonic、別ポート）
       └── 書き込みブローカー（T2: relay-wright W3-A と同型、§6.2）
```

外部 IF は全て `CurrentValuesHandle`（clone 可能）と
`broadcast::Receiver<CollectEvent>` を読むだけの消費者であり、収集ループに
背圧をかけない（broadcast の遅延受信者は lag してメッセージを落とすだけ —
これは仕様。取りこぼしは REST / バックフィルで回復する）。

## 4. タグ空間のセマンティクス

外部 IF 全プロトコル共通の意味論。ここを1箇所で固定し、各プロトコルは
表現（JSON / protobuf / MQTT ペイロード）だけを変える。

- **タグの外部名**: `{connection}.{group}.{tag}`（例: `line1.fast.temp01`）。
  各階層は I1 の `name`。tstore の `tag_key` と同様、表示名でなく安定キー。
  区切りは `.`、MQTT トピックでは `/` に置換（§5.3）。
- **タグ名の一意性スコープ（2026-08-31 オーナー決定）**: `tags.name` は
  **収集グループ内でのみ一意**（`UNIQUE(collection_group_id, name)`、
  `migrations/0011_tags_unique_name_per_group.sql`）。外部名は
  `{connection}.{group}.{tag}` の合成である以上、末端のタグ名まで全体
  一意にする必要はない。従来の全体一意は「同型構成の装置を複数台つないで
  同じ PLC アドレス（例: 2台目も `D100`）を使う」という産業用途の実態と
  衝突しており、実機で2台目の登録が「既に使用されています」で弾かれる
  不具合として発覚した。**接続名・収集グループ名は今も全体一意のまま**
  なので、外部名の一意性（catalog のバインディングキー、§4.1）は保たれる。
- **値**: スケーリング適用後の工学値（`banto-collect` が格納する値そのまま）。
  生値は公開しない。型は I1 の `data_type`（i16/u16/i32/u32/f32/string）に
  由来し、数値は JSON では number、gRPC では oneof。
- **品質**: `Good` / `Bad` / `Stale` の3値（`banto-collect` の `Quality`。
  Stale は読み出し時判定 = 最終更新から周期×2.5 超）。全プロトコルで値と
  必ず対にして返す — **品質なしの値は返さない**（記録計と同じ規律）。
- **タイムスタンプ**: サンプル取得時刻（サーバー時計、UTC、ミリ秒）。
  PLC 時計は使わない。
- **欠測を隠さない**: 未収集・PLC 断のタグも一覧に現れ、`Bad`/`Stale` + 最終値
  （あれば）を返す。404 になるのは定義が存在しないタグだけ。
- **メタデータ**: 単位・小数桁・しきい値・書き込み可否（§6）を catalog 系 API で
  公開。クライアントが表示・判定に使えるように。

### 4.1 catalog はバインディング契約である（中央レジストリ構想）

将来の関連アプリ（SCADA 等）は**自前のタグマネージャーに PLC アドレスを
定義しない**。タグサーバーに登録済みのタグを catalog から参照（ブラウズ →
選択 → 外部名でバインド）して使う — これが本製品群の中核構想であり
（2026-08-04 オーナー決定）、OPC サーバー / FA-Server とクライアントの
関係と同じモデル。この決定は catalog を「メタデータ提供 API」から
**クライアントとの互換性契約**へ格上げする:

- **バインディングキー**: クライアントが保存するのは外部名
  `{connection}.{group}.{tag}` + catalog が併記する**安定 ID**（I1 の
  `id` 3層をそのまま公開）。表示・購読は外部名で行い、リネーム検出は
  安定 ID で行う（名前が変わっても ID が同じなら「リネームされた」、
  ID が消えたら「削除された」とクライアントが判別できる）
- **catalog リビジョン**: catalog 応答に単調増加の `revision` を含める。
  実体は「収集スナップショット（`build_config` → `Collector` 再起動）の
  世代番号」— タグ定義変更でサーバーが収集を再構築するタイミングと正確に
  一致する
- **構成変更通知**: 定義変更時に WS / gRPC ストリームへ `config_changed`
  イベント（新 revision 付き）を流す。クライアントは catalog を再取得して
  再バインド（消えたタグは画面上で unresolved 表示にする、等の挙動は
  クライアント側の責務）
- **クライアントの公開ゲート**: `banto-tagclient` は catalog の `revision` と
  values/snapshot の `revision`、`run_id`、`collection_mode` が同一generationで一致した
  場合だけ値を current として公開する。未知のcollection_modeや不一致時は catalog取得
  から bounded retry し、`config_changed` 連打は単一の rebinding に coalesce する。
  再解決中は旧値を current として扱わない。初期接続・再接続では先にWS購読を成立させ、
  dataを保留したままREST snapshotを取得してからpublish gateを通すことで、snapshot後から
  WS成立前の通知取り逃しを防ぐ。WSにmetadataがないため、確定snapshotのmetadataを接続
  世代へ固定し、values側の`value_source`をauthoritativeに扱う。詳細と
  SDK DTO の `run_id` / `collection_mode` / `value_source` 保持、未知の
  `value_source` の安全な扱いは [banto-tagclient-design.md](banto-tagclient-design.md)
  を正とする。
- **リネームは破壊的変更**: 管理 UI 上で「このタグは外部公開されている」
  ことを前提に、リネーム時に警告する（**警告のみで確定** — 2026-08-05
  オーナー決定。オンライン変更時の外部クライアントへの影響はユーザーの
  責任範囲とし、サーバー側でのブロックや参照追跡はしない）

### 4.2 演算タグ・内部タグ（タグ種別の導入）

**方針（2026-08-04 オーナー決定）**: 演算タグは各クライアントアプリで
実装せず**タグサーバー側で一元実装**する。理由: (1) 同じ演算を各アプリが
持つと式のバージョン差で値が食い違う — タグ空間で1回計算すれば全クライアント・
全プロトコル（WS/MQTT/gRPC）が同一の値・品質・時刻を見る。(2) catalog に
現れるので、クライアントから PLC タグと演算タグは完全に区別なく扱える
（バインドモデル §4.1 がそのまま適用される）。

I1 に**タグ種別**を導入する（§10-2 の `writable` と合わせて1回のスキーマ拡張
で済ませる）:

| 種別                | 値の源                 | 書き込み                           | 備考                                                                              |
| ------------------- | ---------------------- | ---------------------------------- | --------------------------------------------------------------------------------- |
| `plc`（既定・既存） | 収集タスク             | §6 の opt-in パススルー            | `address` 必須                                                                    |
| `computed`          | 式の評価               | 不可（値は常に式が決める）         | `address` なし、`expression` 必須                                                 |
| `internal`          | クライアントの書き込み | タグ空間内で完結（PLC へ送らない） | SCADA の設定値・アプリ間の状態共有用。`retain` フラグで再起動時の最終値復元を選択 |

演算タグの意味論:

- **純関数のみ**: 入力は他タグの現在値、出力は自タグの値。副作用なし・
  外部 I/O なし・PLC 書き込みなし — FA-Server の「アクション」（スクリプト・
  メール送信等）とは明確に一線を引く。自動書き込みへ演算結果を使いたければ
  relay-wright が演算タグを**購読**すればよい（責務分担は崩れない）
- **式言語は最小の宣言的文法**: 四則演算・比較・論理・条件（`if(c,a,b)`）・
  `min/max/abs/round/clamp` 程度から始める。外部式評価クレートではなく
  **自前の小さな AST + 純関数評価器**を推奨 — I 系の流儀（scaling / planning が
  純関数）と一致し、文法が閉じているので監査可能・決定論的。ループ・
  ユーザー定義関数は入れない（アクション化への滑り坂）
- **DAG のみ許可**: 演算タグが演算タグを参照するのは可、循環は登録時
  検証で拒否（relay-wright W2 の書き込みループサイクル検出と同じ規律。
  実装も流用候補）
- **評価タイミング**: 入力タグの更新イベント駆動で再計算（入力の更新は
  結局グループ周期に律速される）。品質は入力の最悪値を継承
  （Bad > Stale > Good）、時刻は再計算のトリガとなった入力の時刻
- **外部名**: 接続に属さないため、予約セグメント `calc` / `mem` を第1階層に
  使う（例: `calc.line1.temp_avg`、`mem.ui.setpoint1`）。実接続名との衝突は
  登録時検証で拒否

**FA-Server の演算機構との比較（マニュアル v6.0.17 一次調査 2026-08-04）**:
FA-Server のサーバー側演算はスクリプト言語「ロボスクリプト」の3構文で行う —
SC1（VB 互換演算子の簡易構文）、SC2（if / for・ユーザー定義関数を持つ
フル言語）、演算式構文（サマリアクションの計算フィールド等で使う式のみの
構文 — 本設計の演算タグ式に最も近い位置づけ）。いずれも「アクション」として
実行される汎用機構であり、周期実行やタグ変化検出をトリガに任意処理
（外部アプリ実行・SQL 呼び出し・メール等）へ接続できる。本設計の
「純関数のみ・ループなし・ユーザー定義関数なし」はこの SC2 型フル言語への
滑り坂を意図的に断つ選択であり、FA-Server 比で表現力を削る代わりに
決定論・監査可能性・値の一貫性（§4.2 冒頭の一元実装の理由）を取る。
式に必要な機能が増えた場合も、関数の**組み込み追加**（`avg` 等）で対応し、
制御構文は導入しない。

SC2 のオペレータ一式（[オペレータ](https://docs.roboticsware.com/ja/6.0.17/fa-server/contents/cmn_script_sc2_5.html)）
から得た式文法（§10-12）の設計材料:

- 演算子セット自体は本設計の想定と同水準: 四則 + 比較（`==` `!=` `<` `>`
  `<=` `>=`）+ 論理（`!` `&&` `||`）+ ビット演算（`&` `|`）
- **暗黙型変換は踏襲しない**: SC2 は `"1" + 2` が数値 3、`&` が左辺文字列時に
  文字列結合、ブール値が四則で 1/0 になる等、文脈依存の暗黙変換を多用する。
  本設計の式は I1 の型情報があるため**登録時に型検査し、暗黙変換はしない**
  （型不一致は登録時エラー。決定論と監査可能性を優先）
- **ビット抽出の実需要は確認できた**: SC2 ではワードからのビット取り出しを
  `(a & 0x1) > 0` のようなマスク演算で行う。§10-12 の「ビット抽出の要否」は
  「要」と見てよく、マスク演算子を入れるより `bit(tag, n)` の組み込み関数
  1個で提供する方が本設計の文法規模に合う（オーナー確認は §10-12 のまま）
- 時刻演算（時刻±秒・時刻差）・配列ブロードキャスト演算は v1 非対応で
  問題ない（演算タグの入力は現在値スカラーのみ。時系列集計は ChronoGazer /
  tsquery の領分）

### 4.3 オンライン動的変更（FA-Server 不満点の解消）

**要件**: タグ定義の追加・変更・削除を**サーバー無停止**で適用する。
設計原則は「**変更の影響半径 = 触ったものだけ**」。変更を3クラスに分ける:

| クラス                       | 例                                               | 影響半径                                                                       |
| ---------------------------- | ------------------------------------------------ | ------------------------------------------------------------------------------ |
| (a) サーバー側タグ           | 演算タグ・内部タグの追加/変更/削除               | **ゼロ**（PLC 通信に一切触れない。検証 → 即時反映）                            |
| (b) 追加系                   | 新規接続・新規グループ・既存グループへのタグ追加 | 既存の収集・購読に影響なし（新規分のタスク起動のみ）                           |
| (c) 既存 PLC タグの変更/削除 | アドレス修正・周期変更・タグ削除                 | **当該接続のタスクだけ**を新スナップショットで入れ替え。他接続は無停止・無切断 |

実現の土台は現行アーキテクチャに既にある:

- `banto-collect` は**接続毎に1タスク**なので、(c) の「接続単位の入れ替え」は
  自然な粒度。必要なのは Collector 全体でなく**接続単位の部分再構成 API**
  （I 系バックログ **I7 候補**: `Collector::replace_connection(snapshot)` 相当。
  スナップショット不変の設計は接続単位でもそのまま成立する）
- tstore は**スキーマ凍結 + ローテーション**が既存設計なので、構成変更時は
  ファイルが連番ローテーションするだけ — プロセス停止は元々不要
- クライアント互換性は §4.1 の契約（revision + `config_changed` → catalog
  再取得 → 再バインド）で吸収。変更されなかったタグの購読は流れ続ける
- **段階実装**: T0 は現行の「全体再構築」（ChronoGazer と同じ Collector
  作り直し）で開始してよい。revision / `config_changed` の外部契約が実装差を
  隠すため、I7（部分再構成）を後から入れてもクライアント非互換にならない —
  外部から見える差は「変更時に他接続の値まで一瞬 Bad になるか否か」だけ
- 適用は**編集トランザクション単位の all-or-nothing**: 検証（アドレスパース・
  循環検出・名前衝突）を通った変更だけが revision を進める。中途半端な
  構成が外部へ見える瞬間を作らない

**T7 実装済み（2026-08-05）**: 段階実装（上記「T0 は全体再構築で開始してよい」
の見込みどおり）を卒業し、I7（接続単位の部分再構成 = `banto_collect::Collector::apply_config`、
T7-1）を `apps/banto-hub/core/src/hub.rs`（`CollectorManager::rebuild`、T7-2）
へ配線した。外部契約（`revision`/`config_changed`）は移行の前後で一切変えて
おらず、既存クライアントは何も変更せずに「変更時に他接続の値まで一瞬 Bad に
なっていた → ならなくなった」という体験向上だけを受け取る（本節冒頭の見込み
どおり、クライアント非互換なしで後入れできた）。以下は移行に伴う変更点:

- **二重接続窓の解消**: T0〜T6 の「全体再構築」方式が内在させていた「レジス
  トリが同じ接続を指したまま構成が変わった場合、新旧 `Collector` が短時間
  だけ同じ PLC へ同時にソケットを張る瞬間」は、`apply_config` が同一
  `Collector` インスタンスを in-place で書き換える（変更接続は stop →
  spawn の順）ため発生しない — 新旧 `Collector` という概念自体が無くなった
- **SLMP broker セッションの削除同期**: T2-2 で「`ensure_connection` のみ
  (追加専用、削除なし)」としていた broker セッション同期を、
  `banto_broker::SessionDirectory::remove`（T7-2 追加）を使った「追加 +
  削除」の完全同期に変更した。削除対象接続の collect タスクが停止した
  ことを確認した後でセッションを落とす順序を守る（`crate::broker_glue`/
  `crate::hub::CollectorManager` のモジュール doc 参照）
- `ApplyReport`（追加/削除/入れ替え/無変更の接続キー一覧・writer ローテート
  有無）を `GET /api/v1/status` の `last_apply` として公開（§9 参照）

> **2026-08-09 オーナー決定（banto-hub-desktop-plan.md UX-5 / TAG-P0-2）**:
> banto-hub のデスクトップアプリ／サービス運転計画（T14〜T18）では、初版の
> 公開 UI と直接 REST で**運転（実機・SIM 収集）中の構成編集をロックする**
> （運転中の CRUD は `409 Conflict`）。本節のオンライン部分再構成基盤
> （`apply_config`）は廃止せず、内部の状態遷移と将来の下書き一括反映の基盤
> として維持する。あわせて、I1 CRUD の「rebuild 失敗は CRUD 自体の失敗に
> しない」設計（`rebuild_and_notify`）は、停止中保存の**全構成 preflight**
> （保存前検証 → 保存成功＝実行可能を保証）へ置き換える。詳細は
> [banto-hub-desktop-plan.md](banto-hub-desktop-plan.md) §5.2 / §9.1 / §16。
>
> **2026-08-11 追補（UX-5 方針改定）**: 公開操作の契約を更新し、運転中編集は
> 一律 `409` 拒否ではなく **pending queue への保存**を許可する。実行構成への
> 反映は人が任意タイミングで明示 `適用` したときのみ行い、適用前は
> `キャンセル` で破棄できる。`apply_config` は引き続き内部基盤として維持し、
> 即時自動反映ではなく「明示適用時の反映エンジン」として用いる。最新の受入条件は
> [banto-hub-desktop-plan.md](banto-hub-desktop-plan.md) §9.3 TAG-P0-3 を正とする。
>
> **2026-08-12 追補**: pending change の適用時に、対象リソース
> （`plc_connections`/`collection_groups` の update/delete）が enqueue
> 後に別経路で変更・削除されていないかを確認する per-resource の
> フィンガープリントガードを追加した。詳細・不一致時の挙動は
> [banto-hub-desktop-plan.md](banto-hub-desktop-plan.md) §9.3 を正とする。

## 5. 外部インターフェース設計

優先順は **REST → WebSocket → (書き込み) → MQTT → gRPC → (OPC UA)**。
REST/WebSocket は axum に同居するため追加依存ゼロで先行できる。

### 5.1 REST（T0）

`banto_server` の骨格（bearer 認証 + CSRF + 監査ログ）に載せる。人間用の
管理 UI セッションと、機械クライアント用の **API キー**（§5.6）の2系統。

| Method | Path                   | 内容                                                                                                                                                                                                                                                                                                                                                 |
| ------ | ---------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| GET    | `/api/v1/tags`         | catalog: 全タグの定義 + メタデータ + 安定 ID + `revision`（`?group=`, `?connection=` フィルタ）。**PLC アドレスを既定で含める**（2026-08-05 オーナー決定 — 外部クライアント側からもどの PLC アドレスか判る方が取り違えを防ぎ、アドレス-タグ対応表を別途参照する煩わしさをなくす。閉域 LAN 前提 §5.6 が背景。専用スコープ `catalog:full` は設けない） |
| GET    | `/api/v1/values`       | 全タグの現在値 + 品質 + 時刻の一括スナップショット（`?tags=a,b,c` 部分指定）                                                                                                                                                                                                                                                                         |
| GET    | `/api/v1/values/{tag}` | 単一タグの現在値                                                                                                                                                                                                                                                                                                                                     |
| POST   | `/api/v1/values/{tag}` | 書き込み（§6。writable タグのみ、監査必須）                                                                                                                                                                                                                                                                                                          |
| GET    | `/api/v1/status`       | サーバー状態: 接続毎の `ConnectionStatus`、収集稼働状況、バージョン                                                                                                                                                                                                                                                                                  |
| GET    | `/api/v1/events`       | `collect_events` の範囲クエリ（収集開始/停止・PLC断/復旧・しきい値）                                                                                                                                                                                                                                                                                 |
| GET    | `/api/v1/openapi.json` | スキーマ（**utoipa による自動生成 — 2026-08-04 決定**。catalog は互換性契約（§4.1）のため、コードとスキーマを単一ソース化する）                                                                                                                                                                                                                      |

設計原則: **読み取りは全て `CurrentValuesHandle` の読み出しのみ**で完結し、
PLC への追加要求を発生させない（外部クライアントがいくらポーリングしても
PLC 負荷は一定 — FA-Server 型タグ空間の本質的な利点）。オンデマンドの
直接読み出し（キャッシュ迂回）は提供しない。

### 5.2 WebSocket（T1）

axum の `ws` アップグレードで `/api/v1/stream`。メッセージは JSON テキスト。

```jsonc
// クライアント → サーバー
{ "op": "subscribe",   "id": 1, "tags": ["line1.fast.temp01", "line1.fast.*"],
  "mode": "on_change",           // "on_change" | "interval"
  "interval_ms": 1000 }          // mode=interval のとき必須。下限 = 対象グループの period_ms
{ "op": "unsubscribe", "id": 1 }
{ "op": "ping" }

// サーバー → クライアント
{ "op": "data",  "id": 1, "t": 1722758400123,
  "values": [ { "tag": "line1.fast.temp01", "v": 25.4, "q": "good", "t": 1722758400100 } ] }
{ "op": "event", "kind": "plc_disconnected", "connection": "line1", "t": ... }
{ "op": "config_changed", "revision": 42 }   // §4.1: catalog 再取得と再バインドの合図
{ "op": "error", "id": 1, "code": "unknown_tag", "detail": "..." }
```

- 購読はコネクション内 `id` で多重化（1ソケットで画面毎に別購読を持てる）
- `on_change` は値**または品質**の変化で発火（Stale 遷移も通知される —
  読み出し時判定なので購読タスク側が周期×2.5 のタイマで評価する）
- ワイルドカード `*` は末尾のみ（`connection.group.*`）。全タグ購読は
  `*` 単独
- 接続時に現在値の初期スナップショットを必ず1回送る（購読直後の空白防止）
- **書き込み op は設けない**（読み取り・購読専用チャネル。書き込みは
  REST / gRPC のみ — §6、2026-08-04 決定）
- バックプレッシャ: 送信バッファが閾値超過したクライアントは切断
  （遅い購読者が収集側を止めない — broadcast lag と同じ思想）
- SSE 版（`banto_server` に既存）は管理 UI 内部用にのみ使い、外部公開の
  購読 IF は WebSocket に一本化する
- 比較材料の注記（マニュアル v6.0.17 一次調査 2026-08-04）: FA-Server の
  クライアント購読経路 IPLink は TCP/IP ベースの独自プロトコルで、公開
  マニュアルにあるのはサーバー側設定（有効化・IP 最大3つ・ポート・
  タイムアウト）と ActiveX クライアントライブラリの利用法まで — ワイヤ仕様・
  購読粒度・更新通知の意味論は非公開。本節の購読設計は比較対象なしの
  独自設計として確定してよい。Web 標準（WS + JSON）でクライアント
  ライブラリなしに購読できること自体が IPLink（ActiveX = Windows +
  COM 前提）に対する明確な差別化になる

**ブラウザ WS クライアントの認証（T10、判断の記録、2026-08-07）**:
本節冒頭の「アップグレードリクエストの Authorization ヘッダで検証」
（§5.6）は機械クライアントを想定した書きぶりで、ブラウザ組み込みの
`WebSocket` コンストラクタが `Authorization` 等の任意ヘッダを送れない
という制約を考慮していなかった。T10（管理 UI のライブタグモニタ）が
初めてブラウザから直接 `/api/v1/stream` へ接続するクライアントになった
ため、この欠落を埋める必要が生じた。`?token=` のようなクエリパラメータ
方式は採用しない（トークンがサーバーのアクセスログやブラウザ履歴に残る）。
代わりに `Sec-WebSocket-Protocol` ヘッダをトークンの運び役に使う
（`new WebSocket(url, ['bearer', token])` と書くとブラウザが
`Sec-WebSocket-Protocol: bearer, <token>` を自動送信する、AWS IoT の
ブラウザ MQTT-over-WS SDK 等でも使われる標準的な回避策）。`GET
/api/v1/stream` 1ルートのみのフォールバックとし、他の `/api/v1/*` は
一切影響を受けない（実装は `apps/banto-hub/core/src/rest.rs` の
`extract_ws_protocol_token`、`require_tag_space_auth` からの呼び出し
参照）。あわせて `ws_upgrade`（`apps/banto-hub/core/src/stream.rs`）は
`WebSocketUpgrade::protocols(["bearer"])` でクライアントが実際に
`bearer` をオファーした場合のみ応答へ選択結果を返す（オファーされて
いないサブプロトコルを一方的に選択しないという RFC 6455 の規律どおり
で、`Authorization` ヘッダのみで接続する既存の機械クライアントには
一切影響しない）。

### 5.3 MQTT publish（T3）

**外部ブローカーへ接続するクライアント**として実装する（rumqttc）。
組み込みブローカー（rumqttd 同梱）は運用上魅力があるが、現場には既に
ブローカーがあるケース（AWS IoT / EMQX / Mosquitto）が多く、まずは
クライアントモードのみ。組み込みは §10-4 の判断待ち。

- トピック: `{prefix}/{connection}/{group}/{tag}`（prefix 既定 `banto`、設定可）
- ペイロード: `{"v": 25.4, "q": "good", "t": 1722758400100}`（WebSocket と同形）
- 発行モード: タグ毎に `on_change` / `interval` を設定（既定 on_change、
  最短発行間隔でスロットル）。retain 有効（新規購読者が即座に最終値を得る）
- QoS: 既定 1。設定で 0/1 切り替え（2 は使わない）
- LWT（Last Will）: `{prefix}/$state` に `online`/`offline`（birth/death）。
  PLC 断は値の品質 `bad` として流れるので別トピックにしない
- **MQTT 経由の書き込み（`.../set` 購読）はやらない**（2026-08-04 決定 —
  書き込み受付は REST / gRPC の2経路に固定、§6。ブローカー介在では
  認証主体の特定・監査・結果応答が §6 のモデルに乗らない）

**T3 実装状況（2026-08-05、`apps/banto-hub/core/src/mqtt.rs`）**:

- **タグ毎の発行モード設定は T3 では未実装**: 上記「発行モードはタグ毎に
  on_change / interval を設定」は per-tag 設定ストレージ（I1 スキーマ
  拡張）が要るため T3 では見送り、**全タグ一律 on_change +
  `min_interval_ms` スロットル**を既定動作として先行実装した。per-tag 化
  はバックログ（実装する場合は `tags` テーブルへの列追加と管理 UI・
  catalog 露出が必要）
- **削除されたタグの retain クリアはやらない**: catalog から消えたタグの
  古い retain メッセージが MQTT ブローカー側に残り続ける既知の制約。
  トピックは catalog の `{connection}/{group}/{tag}` から機械的に決まる
  ため、購読側は `GET /api/v1/tags` の現行 catalog と突き合わせれば
  「もう存在しないトピック」を判別できる
- **設定は settings テーブルに保存**（`mqtt.enabled`/`mqtt.host`/
  `mqtt.port`/`mqtt.client_id`/`mqtt.username`/`mqtt.password`/
  `mqtt.prefix`/`mqtt.qos`/`mqtt.min_interval_ms`、既定値は本節どおり）。
  **`mqtt.password` は平文保存** — §5.6「v1 では平文 + 閉域 LAN 前提」と
  同じ線引き（ブローカーへの認証情報はクライアントへ渡す時点でどのみち
  平文に戻す必要があり、ハッシュ化しても保護にならない）
- 管理 REST: `GET/PUT /api/mqtt-settings`（admin 限定、CSRF 必須）。
  `GET` は `password` を一切返さない。`PUT` の `password` は**空文字を
  「変更なし」**として扱う。保存成功で `MqttPublisher::apply` を呼び
  **即時適用**する（`CollectorManager::rebuild` と同じ「古いタスクを
  止めて新しいタスクを起動」パターン）
- `GET /api/v1/status` に `mqtt: { "enabled": bool, "connected": bool }`
  を追加（`enabled` は設定値、`connected` は実際にブローカーへ接続できて
  いるかのライブ状態 - 両者は独立）
- 依存クレート: `rumqttc`（クライアント、既定 feature `use-rustls` を
  落として平文接続のみに絞る）。テスト用に `rumqttd`（in-process ブロー
  カー、`banto-hub-core` の dev-dependency のみ）を追加し、
  `apps/banto-hub/core/tests/mqtt.rs` で E2E（発行・retain・`$state`・
  スロットル・enabled 切替）を検証する

### 5.4 gRPC（T4）

tonic + prost。ポートは REST と分離（既定: REST 880x 系 / gRPC 50051、§8）。
proto は `proto/tagserver/v1/` にリポジトリ内で管理し、外部クライアント SDK の
生成元とする。

```proto
service TagService {
  rpc GetCatalog(GetCatalogRequest) returns (GetCatalogResponse);
  rpc ReadValues(ReadValuesRequest) returns (ReadValuesResponse);      // スナップショット
  rpc StreamValues(StreamValuesRequest) returns (stream ValueBatch);   // 購読（WSと同意味論）
  rpc StreamEvents(StreamEventsRequest) returns (stream Event);
  rpc WriteValue(WriteValueRequest) returns (WriteValueResponse);      // §6 と同一経路
}

message TagValue {
  string tag = 1;
  oneof value { double num = 2; string str = 3; }
  Quality quality = 4;         // GOOD / BAD / STALE
  int64 timestamp_ms = 5;
}
```

意味論は §4/§5.2 と完全に同一（購読モード・初期スナップショット・
バックプレッシャ切断）。REST/WS が JSON で果たす役割の型付き版であり、
主用途は .NET / Java / Python 製の上位システム連携。

**T4 実装状況（2026-08-05、`apps/banto-hub/core/src/grpc.rs`）**:

- **proto ビルドは自己完結**: `protoc-bin-vendored`（Google 配布の `protoc`
  実行体をクレートに静的同梱、ビルド時ネットワーク不要）を
  `apps/banto-hub/core/build.rs` の build-dependency にし、
  `prost_build::Config::protoc_executable` へ明示的に渡す。システム
  `protoc` の有無に依存しない - このコンテナにも GitHub Actions
  `ubuntu-latest` にも system `protoc` は入っていない前提で選んだ（`apt-get`
  でのインストールをビルド前提に持ち込まない）。tonic は 0.13 以降 prost
  統合が `tonic-prost`（コーデック）/`tonic-prost-build`（コード生成）へ
  分離されているため、この2つと `prost` を個別に依存宣言している
  （最新安定系 0.14）。生成コードは `OUT_DIR` 方式でリポジトリにはコミット
  しない。
- **書き込みゲートは `write_path.rs` へ完全抽出**: T2-4 で
  `crate::rest::v1_write_value` に直接実装されていたゲート1〜8
  （catalog 解決・writable・実効 enabled・プロトコル対応・受付トグル・
  レート制限・値変換・log-before-write）を `crate::write_path::execute_write`
  へ切り出し、REST/gRPC の両方がこれを呼ぶ。両者の差分は「この関数に来る
  前の transport 固有の前段」（REST: セッション token 拒否・JSON body
  パース。gRPC: metadata の bearer 認証・`oneof num|bool` 分解）と、
  `WriteRejection` を自分のワイヤ表現（HTTP ステータス+JSON / tonic::Status）
  へ変換する最後の1ステップのみ。**レート制限器（`WriteRateLimiter`）も
  REST/gRPC で1個の `Arc` を共有する**よう `crate::rest::api_router`/
  `tag_space_router` のシグネチャを変更した（呼び出し元が1個だけ構築して
  両方へ配る）- ゲート実装を共有しても状態(バジェット)が別インスタンスだと
  「タグ毎+全体の書き込み上限」が実質2倍緩む抜け道になるため。
- **購読ロジックは `subscribe_core.rs` へ抽出**: `TagPattern`（ワイルドカード
  パース・マッチ）・`resolve`・`Mode`・`interval_floor_ms`・`Subscription`・
  on_change diff/interval 評価の本体を `crate::stream`（WS）から
  `crate::subscribe_core` へ切り出し、`crate::grpc`の`StreamValues`が同じ
  関数群を呼ぶ。250ms 評価・初期スナップショット必須・ワイルドカードの
  評価時再解決という意味論は構造的に一致する。**唯一の意味論差**: gRPC の
  `StreamValues` は明示的な `config_changed` 相当のフレームを送らない
  （proto のメッセージ設計が `TagValue`/`ValueBatch` 中心で、その専用型を
  持たないため）。新規タグの出現自体は WS と同じ仕組み（250ms ごとの
  `TagMap` 再照合）で暗黙に処理されるので、ワイルドカード購読者は新しい
  `ValueBatch` を受け取れる - 変わるのは「構成が変わったこと自体の明示通知」
  の有無のみ。
- **認証は各ハンドラ冒頭方式**（interceptor ではなく）: 設計は
  「interceptor または各ハンドラ冒頭」のどちらも許容していたが、
  `WriteValue` だけ `read` スコープ不要・`write:{tag}` は body を見ないと
  判定できないという非対称性があり、tonic の `Interceptor`（`Request<()>`
  の段階でメソッド名の判別が REST のパスベース判定より複雑）より、
  `GrpcService::authenticate(&self, &Request<T>, RequireScope)` を各
  ハンドラの冒頭で呼ぶ方式の方が REST の `require_tag_space_auth` +
  `v1_write_value` 自身のスコープ検査という既存の非対称構造と揃う。
  `Revoked`/`Tripped`/未認証は REST と同じく `audit_log` に `origin: "grpc"`
  で記録する。
- **設定・起動制御**: `settings` テーブルに `grpc.enabled`（既定
  `false`）/`grpc.port`（既定 `50051`）を追加。`GrpcServer`は
  `MqttPublisher`と同じ「停止状態で構築 → `apply(&settings)`で再起動可能な
  マネージャ」パターン（ただし停止は `JoinHandle::abort` — `GrpcService`
  自体は状態を持たない読み取り専用ハンドルの束なので、グレースフル
  シャットダウンを待つ理由がない）。管理 REST `GET/PUT
/api/grpc-settings`（admin 限定、CSRF 必須、保存で即時適用）と管理 UI の
  設定ページに gRPC セクションを追加。`GET /api/v1/status` に
  `grpc: { enabled, port }` を追加（MQTT と違い「実際に接続できているか」
  のライブ状態は持たない — gRPC は listen するだけのサーバーで、設定値が
  そのまま意図した状態を表すため）。
- **エラー写像**: 404→NOT_FOUND、403→PERMISSION_DENIED、
  409→FAILED_PRECONDITION、422→INVALID_ARGUMENT、429→RESOURCE_EXHAUSTED、
  501→UNIMPLEMENTED、502→UNAVAILABLE、503→FAILED_PRECONDITION（`message`に
  `"{元の REST エラーコード名}: {detail}"`の形で併記）。ゲート抽出時に
  生じた `Internal`（レジストリ再読み込み失敗等の防御的分岐）は
  `INTERNAL`/REST 500 に丸めた（表に無い追加区分だが、両実装とも到達しない
  想定の分岐）。
- **未解決事項**: OpenAPI 相当のスキーマ配信（gRPC reflection サービス）は
  T4 スコープ外 - proto ファイル自体をリポジトリ内で版管理し、SDK 生成元
  とする方針（本節冒頭）で足りるとした。

### 5.5 OPC UA（後回し — 判断の記録)

FA-Server との比較で最も見劣りする欠落だが、v1 から外す:

- Rust の OPC UA サーバー実装（`opcua` クレート等）は情報モデル・
  セキュリティポリシー・証明書管理まで含めると検証コストが大きい
- 国内現場の実態として、上位接続の要求は REST/MQTT で足りるケースが増えている
- **足場だけ確保**: 外部 IF は全て §4 のタグ空間セマンティクスの薄い皮として
  実装する規律を守れば、OPC UA も「もう1枚の皮」として後付けできる。
  タグの外部名・品質3値・タイムスタンプは OPC UA の
  NodeId / StatusCode / SourceTimestamp へ素直に写像可能

### 5.6 認証（全プロトコル共通)

- **管理 UI**: `banto_server` の bearer token セッション（既存のまま）
- **機械クライアント（REST/WS/gRPC）**: **API キー**（`Authorization: Bearer <key>`、
  WS は接続時ヘッダ、gRPC は metadata）。キーは管理 UI で発行・失効し、
  **スコープ**を持つ: `read` / `write:{connection.group.tag パターン}`。
  書き込みスコープはワイルドカード不可（明示列挙のみ — §6 の規律）
- **MQTT**: ブローカーへの接続認証はブローカー側の責務（ユーザー名/パスワード
  or 証明書を設定で渡すのみ）
- TLS: v1 では平文 + 「閉域 LAN 前提」を明記（ChronoGazer の LAN モードと
  同じ前提）。リバースプロキシ（Caddy 等）での終端を運用ガイドに記載

#### 試運転モードとロックダウン（2026-08-30 オーナー決定）

**背景**: 開発時・試運転時はタグ定義や収集設定の変更が頻繁で、そのたびに
ログインを求められるのは煩わしい。一方で試運転完了後は、現場の第三者が
設定を触れないよう施錠したい。恒久的な「認証なしモード」ではなく、
**ライフサイクルの状態**として設計する（産業機器の「試運転 → 施錠」の型）。

**状態は2つだけ**:

- **試運転モード（初期状態）**: 管理 UI / 管理 REST は認証なしで操作できる。
- **ロックダウン済み**: 従来どおり bearer セッションのログインが必要。

**遷移**: 「ロックダウン」操作でのみ試運転モード → ロックダウン済みへ移る。
アカウント作成そのものは遷移条件にしない（試運転中に運用者アカウントを
準備しておき、完了時にまとめて締められる）。**逆方向（ロックダウン済み →
試運転モード）は `banto-hub-elev.exe` 経由でのみ可能**とし、UI・REST からは
解除できない。改造や再試運転の場面で必要になるため経路自体は残すが、
**ローカルの管理者権限を持つ人（＝機械の前に立てる人）に限定**する。

**安全側の制約（いずれも必須）**:

1. **試運転モードは loopback バインド時のみ許可する。** `BANTO_BIND` が
   `0.0.0.0` 等の非 loopback で、かつ未ロックダウンの構成は**起動時に拒否**
   する。認証なしの状態がネットワークへ露出する経路を原理的に塞ぐ
   （試運転は Hub が動いている機械の前で行う前提）。
2. **未ロックダウンの間は画面に消せない警告を常時表示する。** 最大のリスクは
   「試運転モードのまま出荷される」ことなので、状態が一目で分かるようにする。
   起動ログにも出す。
3. **`banto-hub-elev.exe` に2つのアクションを追加する**: パスワード再設定
   （紛失時の回復）と、試運転モードへの復帰（改造・再試運転向け）。いずれも
   UAC 昇格が要るため、ネットワーク越しには実行できない。**復帰させた場合は
   制約1が再び効く**ので、LAN バインドのままでは起動しなくなる点に注意する
   （復帰の副作用として意図した挙動）。
4. **試運転モードへの復帰は監査ログに記録する。** 認証を外す操作なので、
   いつ誰が（ローカル OS ユーザー）実行したかを残す。

**追跡性への影響は限定的**: PLC 書き込みの監査（`hub_write_audit`）は
`api_key_id` NOT NULL で **API キーに紐づいており、ログインユーザーとは独立**
している。したがって試運転モードでも「誰が PLC を書いたか」は失われない。
失われるのは管理操作（タグ定義変更・収集開始停止）の主体記録のみで、
試運転中は許容する。

**実装状況（2026-08-30〜31）**: バックエンド（`crate::commissioning` の
`CommissioningService`、起動時ガード、`require_auth_or_commissioning`/
`RoleGuard`、`GET /api/commissioning/status`、
`POST /api/commissioning/lock-down`）・UI（ルートガード迂回
`shouldBypassLoginForCommissioning`、閉じるボタンの無い
`CommissioningBanner`、設定画面のロックダウン操作）とも実装済み。

#### 管理 UI と `/api/v1/*` の境界（2026-08-31 オーナー決定・案A）

試運転モードの実装を通す過程で、「管理 UI が読みに行くエンドポイントと
`/api/v1/*` を混同すると試運転モードが壊れる」という境界が明確になった。

- **管理 UI（ブラウザ）は `/api/status`・`/api/values`・
  `/api/tag-catalog`・`/api/tag-stream`（WS）を使う。** 認可は
  `require_auth_or_commissioning`（`RoleGuard` を掛けない = role 不問、
  読み取り専用）のみで、未ロックダウン中は無条件に通し、ロックダウン後は
  セッション bearer を要求する。
- **`/api/v1/*`（`GET /api/v1/tags`・`/api/v1/values`・`/api/v1/status`、
  WS の `/api/v1/stream` 等）は機械クライアント専用のまま**
  `require_tag_space_auth`（API キー or セッション bearer）固定とし、
  **試運転モードのバイパス対象にしない**。書き込み経路（§6）と同じ
  認証境界を守るため、機械クライアントの認証要件を試運転モードで緩めない
  という判断（案A）。
- ロジックは共通関数で共有し、二重実装にはしない: `compute_status`/
  `resolve_value_names`/`build_values_response`/`v1_tags` と
  `build_catalog_response` を管理系・`/api/v1/*` の両ハンドラが呼び、
  管理系レスポンスは camelCase の DTO へ包み直すだけにする。管理系 WS
  （`GET /api/tag-stream`）はハンドラ本体（`crate::stream::ws_upgrade`）
  自体を `/api/v1/stream` と共有する。

**この区別が失われると試運転モードが壊れる**（実装中に発覚した2件）:

1. 管理 UI（`hubStatus.ts`）が `GET /api/v1/status`・`GET /api/v1/values`
   を直接叩いていたため、試運転モード中（未ログイン・API キーなし）は
   401 になり、状態ページの「サーバー状態」「タグ現在値」が空になった。
   `hostSwitchGate.isPreflightOk` も構成 revision の取得失敗で連鎖的に
   T16 切替ウィザードのゲートを閉じていた。管理系 `GET /api/status`・
   `GET /api/values` を新設して解消した。
2. 続けて `tagMonitorAdmin.ts` が `GET /api/v1/tags`（catalog）・
   `GET /api/v1/stream`（WS）を直接叩いていたことも見落としとして発覚し、
   ライブタグモニタが同じ理由で空になった。管理系 `GET /api/tag-catalog`・
   `GET /api/tag-stream` を新設して解消した（管理系 WS は CSRF レイヤー
   の内側に置くとブラウザの WebSocket が繋がらなくなるため、
   `tag_space_router` と同様に CSRF 層の外側で merge する専用ルーターに
   する必要があった）。

## 6. 書き込み経路の安全設計

relay-wright が確立した規律を、タグサーバーの文脈に翻訳して引き継ぐ。
**タグサーバーの書き込みはルールエンジンを持たない**（条件判断・自動化は
relay-wright の専管）。

**受付経路は REST（§5.1）と gRPC（§5.4）の2つのみ**（2026-08-04
オーナー決定）: どちらも要求/応答型で「1書き込み = 1リクエスト = 1監査行 +
1結果応答」が構造的に対応する。WebSocket は購読専用チャネルとして
書き込み op を設けず（§5.2）、MQTT 経由の書き込みも解禁しない（§5.3 —
ブローカー介在では認証主体の特定と結果応答がこのモデルに乗らない）。
書き込みできる経路を2つに固定することで、認証（API キースコープ）・監査・
レート制限の実装と検査面が1系統に閉じる。

外部システム起点の書き込みには同水準のガードを敷く:

1. **per-tag opt-in**: タグ定義に `writable`（既定 false）を追加（I1 の
   スキーマ拡張、§10-2）。writable でないタグへの書き込みは 403
2. **API キースコープ**: 書き込みは `write:` スコープ内のタグのみ。
   read キーで書けない
3. **log-before-write**: relay-wright の write_audit と同型の監査テーブルに
   「誰が（キーID）・どのタグへ・何を・結果」を**書き込み実行前に**記録し、
   実行後に結果を追記
4. **レート制限ブレーカ**: タグ毎 + 全体の書き込みレート上限。超過で
   該当キーを**トリップ**させ、イベント発行。復帰は管理 UI から手動。
   （2026-08-05 決定: トリップは失効 `revoked_at` とは**別の解除可能な状態**
   `tripped_at` とする — 失効は不可逆の監査設計（T0-2）のため。実装は
   relay-wright の rate_limiter（クロック注入・決定論的テスト）を
   タグ毎 + 全体の2段に読み替えて流用）
5. **読み書き単一セッション**: 書き込みは収集と同じ PLC セッションを通す
   必要がある（実機のセッション数上限）。relay-wright の
   `engine/broker.rs`（W3-A: 接続毎1タスク・mpsc 直列化・read/write が
   ワイヤ上で交錯しない構造保証）と同じ設計が必要 — **broker を共有クレート
   `banto-broker`（I6）へ抽出**し、relay-wright とタグサーバーの両方が
   使う（§10-3 で決定済み、2026-08-04）。
   **統合方式（2026-08-05 オーナー承認・持ち越し課題の確定）**:
   **SLMP 接続のみブローカー管理**とする。banto-collect の接続タスク構造は
   変えず、broker の読み取りハンドルを `PlcClient` trait のアダプタで包んで
   クライアント生成の差し替え口（新設）から注入する — 接続状態表示・PLC断
   イベントは既存機構がそのまま機能する。broker 本体は CollectorManager の
   **外**で生存させ、構成再構築を跨いで SLMP セッションを維持する
   （T0 既知の「再構築時の二重接続窓」も SLMP については解消）。
   Modbus 接続は現行の直接クライアントのまま（v1 では書き込み手段が
   存在しないため共有の必要がない）
6. **再起動での安全側復帰**: 書き込み受付は起動時 disabled とし、管理 UI で
   明示的に有効化する（relay-wright のアーミングと同じ「再起動で必ず安全側」）
7. **プロトコル整合（2026-08-05 オーナー承認）**: 書き込みスタック
   （banto-plc-write / broker）は SLMP 専用、収集は Modbus のみ（I8 前）で
   重なりがなかったため、**I8（banto-collect の SLMP 対応）を T2 の前提
   スライスとし、書き込み第一弾は SLMP** とする。Modbus 書き込み
   （banto-plc-write への FC5/6/15/16 追加 + broker のプロトコル抽象化）は
   **I9 バックログ**。受付経路の制約: writable にできるのは SLMP 接続配下の
   タグのみ（Modbus タグへの書き込み要求は明示エラー）
8. **T2 の受付主体**: 書き込みは API キーの `write:` スコープのみ
   （2026-08-05 決定）。管理 UI からの手動書き込み（relay-wright の
   タグモニタ相当）は将来スライス
9. **I1 拡張の段階解禁**: スキーマは4列一括（§10-2）だが、`tag_kind` は
   T2 時点で `plc` のみ受理し、`computed`/`internal` の受理は T6 で解禁
   （2026-08-05 決定）

### 6.1 ワードデバイスのビットアクセス（T8、2026-08-06 オーナー決定）

ワードデバイス（SLMP の D 等 / Modbus 保持レジスタ）の個別ビットを
タグとして読み書きする。

- **名前づけはアドレス側のビット記法で行う**: タグ定義のアドレスに
  `D100.5`（Modbus は `40001.3`）を許し、data_type=bit の通常タグとして
  登録する。`hoge.0` のような**タグ名の後置記法は不採用** — catalog に
  列挙されない派生名はバインディング契約（§4.1）を壊し、意図的に
  非対応としたアクティブタグ（§1.1)と同じ構図になるため。ビットには
  意味のある名前（例: `line1.status.running`）を付けて catalog に載せる
- **読み取り**: 収集は元々ワード単位の一括読みなので、同一ワードの
  16 ビットを何タグ定義しても PLC 負荷は不変（デコード時にビット抽出）
- **書き込みはドライバ層の RMW**（2026-08-06 決定）: SLMP はワード
  デバイスへのビット単位書き込みコマンドを持たないため、
  banto-plc-write が「ワード読み → ビット変更 → 書き戻し → **確認読み**」
  を1手順として実装する。これを **broker の1ジョブ内で実行**することで、
  hub / relay-wright 側の並行書き込みとの競合はワイヤ上あり得ない
  （W3-A の one-socket-at-a-time がジョブ粒度で効く — broker 本体の
  変更は不要）。同一バッチ内で同じワードを狙うビット書き込みはマスク
  合成で1回の RMW にまとめる（異なるワードは結合しない — 書き込み
  プランナの gap-tolerance-zero 規律を維持）
- **PLC 側との競合は原理的に防げない**（オーナー了承済み）: RMW の
  読みと書きの間に PLC スキャンが同じワードの別ビットを書くと書き戻しで
  潰れる。確認読みで**検出**して該当要求を Bad + 詳細記録とし、
  **外部から書くビットを含むワードは PLC 側から書かない**（ハンド
  シェイク領域の専有）を運用ガイドの規約とする
- Modbus 書き込み（I9）実装時は FC22（Mask Write Register）が
  アトミックなため RMW 自体が不要になる — I9 の設計材料として記録

**T8 実装済み（2026-08-06）**: 2スライスに分割して実装した。

- **T8-1（ドライバ層）**: `banto-plc` の `Address` にビット付きアドレス記法
  （`D100.5`/`40001.3`、`Address::Slmp`/`Address::ModbusRef` の
  `bit: Option<u8>` フィールド）を追加し、読み取り側（`decode`/`planning`）
  が同一ワードの複数ビットタグを1回のワード読みに折り込む。
  `banto-plc-write` に `BatchWriteRequest::BitInWord` を追加し、SLMP の
  読み・変更・書き戻し・**確認読み**（RMW）を broker の1ジョブ内で実行する
  （broker 本体は無変更）。確認読み不一致は
  `PlcWriteError::BitWriteVerificationFailed` として per-request `Bad` になる。
  **既知事項**: relay-wright のタグモニタ（手動書き込み）UI は、
  `monitor_write` 自体は `BatchWriteRequest::BitInWord` を受け取れるが、
  UI 側（`monitor_tag_write` の request 組み立て）は `.N` ビット付き
  アドレスのタグをまだ配線していない（従来のフルワード/ビットデバイス
  書き込みのみ対応）。必要になったら別スライスで対応する
  （`apps/relay-wright/core/src/engine/monitor.rs` のコメント参照）。
- **T8-2（hub 配線）**: `banto-collect::config::build_request` が、ビット付き
  アドレスと `data_type != bit` の組み合わせを構成エラーとして拒否する。
  `CollectorManager::rebuild` の all-or-nothing により、このエラーは
  `last_config_error` に現れ、旧構成が維持される。`banto-hub` の書き込み
  ゲート7（`write_path::execute_write` の `write_plc_tag`）は、対象
  アドレスがビット付き（`Address::Slmp` の `bit` フィールドが `Some`）なら
  `BatchWriteRequest::BitInWord` へ、それ以外（通常のワード/ビット
  デバイス）は従来どおり `BatchWriteRequest::Numeric` へ変換する。確認読み
  不一致による `WriteResult::Bad` は 502 `write_failed` として応答し、
  `write_audit` の `detail` にも同じ理由文言（「書き戻し競合の可能性が
  あります」）を記録する（`WriteAuditService::set_result` が `detail`
  引数を取るよう拡張）。banto-tags（I1）はアドレス書式を引き続き検証しない
  （既存方針どおり、address format は I2/I3b の関心事）。タグ登録フォームの
  アドレス欄にビット記法のヘルプ文言を追記した。E2E は
  `apps/banto-hub/core/tests/t8_bit_access.rs`（収集の共有読み・書き込み
  RMW・構成エラー・確認読み不一致の4本）。

## 7. アプリ群の中でのタグサーバー — 中央レジストリ構想と移行ロードマップ

**方針（2026-08-04 オーナー決定）**: タグサーバーは製品群の**タグ定義の
一次ソース（single source of truth）**となる。今後作る関連アプリ（SCADA 等）は
自前のタグマネージャーで PLC アドレスを定義せず、タグサーバーの catalog を
ブラウズしてタグをバインドする（§4.1）。クライアントアプリの消費形態は
2種類を区別する:

- **(A) データプレーンクライアント（今後の既定）**: タグ定義への参照
  （外部名 + 安定 ID）だけを持ち、値は WS/gRPC 購読、書き込みはタグサーバー
  経由。**PLC アドレスを一切知らない** — PLC 通信・セッション管理・品質判定は
  全てタグサーバー側の責務になり、アプリは軽くなる。将来の SCADA はこの形
- **(B) レジストリ同期クライアント（移行期の特例）**: catalog から
  アドレス込みの完全定義を取得し、自分で PLC と直接通信する。既存アプリの
  移行過渡期と、書き込みセッションを専有し続ける relay-wright のための形態
  （catalog は既定でアドレスを含める決定（§5.1、2026-08-05）により、
  (A)/(B) は取得 API を共有する — 違いはスコープでなく「アドレスを使って
  自前通信するか否か」というクライアント側の行儀のみ）

段階制の移行計画:

| 段階    | 構成                                           | 説明                                                                                                                                                                                                                                                  |
| ------- | ---------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 現状    | 各アプリが直接 PLC 接続                        | 単独稼働なら問題なし。同居時はセッション数に注意（W5 実機検証の結果待ち）                                                                                                                                                                             |
| v1      | タグサーバー単独でも製品                       | 外部システム連携（MES/クラウド/自作画面）が主用途                                                                                                                                                                                                     |
| v1.x    | クライアント SDK クレート（`banto-tagclient`） | 読み取り専用SDKの実装前設計は [banto-tagclient-design.md](banto-tagclient-design.md)。catalog キャッシュ・`config_changed`での再バインド・購読の再接続・オフライン時のunresolved化を1回だけ実装し、以後の全アプリが再利用するI系資産。SCADA着手の前提 |
| v2 候補 | 新規 SCADA アプリ                              | 最初から (A) データプレーンクライアントとして作る — タグマネージャー画面は「タグサーバーの catalog ブラウザ + バインド管理」になる                                                                                                                    |
| v2 候補 | ChronoGazer リモート収集モード                 | 収集をタグサーバーへ委譲し、自分は購読 + tsquery で読む (A) 型モードを追加。UI と収集の寿命分離も同時に達成                                                                                                                                           |
| v2 候補 | relay-wright の読み取り委譲                    | 条件評価の入力読み取りをタグサーバー購読 (A) へ。**書き込みセッションは relay-wright 専有を維持**（安全機構と一体のため、(B) の恒久例外）                                                                                                             |

タグレジストリ DB の共有はしない（現状どおりアプリ毎に別ファイル）。
中央化は「DB を共有する」のではなく「catalog API を契約にする」ことで
達成する — DB 共有はスキーママイグレーションの結合を生むが、API 契約は
バージョン管理できる。

## 8. 配布・運用

- **形態**: 単一 exe（axum + 静的 UI 同梱）+ SQLite + データディレクトリ。
  Windows 専用前提は tstore と同じ
- **常駐**: v1 はコンソール起動 + タスクスケジューラ/スタートアップ登録の
  手順書で開始。Windows サービス化（`windows-service` クレート）は T5 で
  検討（サービスは対話セッション不在での動作検証が必要）
- **ポート**（既定、全て設定可能。2026-08-04 決定）: 管理 UI + REST + WS =
  **8722**、gRPC = **50051**。調査結果: ChronoGazer / relay-wright の LAN
  モードは**どちらも既定 8721**（両者とも既定無効のため実害未発生だが、
  同居で両方有効化すると衝突する潜在問題 — どちらかの既定をずらす件を
  既存アプリ側のバックログとする）。8722 は 8721 の隣で「banto 系 872x」の
  台帳に乗せる
- **設定**: 既存アプリ同様 SQLite 内 settings + 起動時引数。タグ定義は
  管理 UI で CRUD（I1 サービス層をそのまま利用）
- **監視**: `/api/v1/status` が死活・接続状態・書き込み受付状態を返す。
  MQTT の `$state` retain と合わせて外部監視に載せられる
- **ソークテスト**: 収集 24/365 + 外部クライアント購読を維持した状態での
  連続稼働試験（banto-collect の 72h ソーク雛形を流用）を出荷条件に含める

## 9. マイルストーン（T系）

| #   | 内容                                                                                                                                            | 依存    | 備考                                                                                             |
| --- | ----------------------------------------------------------------------------------------------------------------------------------------------- | ------- | ------------------------------------------------------------------------------------------------ |
| T0  | 骨格: ヘッドレス axum アプリ + レジストリ配線（I1 CRUD + 管理UI雛形）+ banto-collect 組み込み + REST 読み取り一式（§5.1）+ API キー基盤（§5.6） | I1〜I3b | Tauri なし構成の初例。シミュレータで E2E。**実装済み（〜T0-3、2026-08-05）**                     |
| T1  | WebSocket 購読（§5.2: subscribe/on_change/interval・初期スナップショット・バックプレッシャ切断）                                                | T0      | **実装済み（2026-08-05）**                                                                       |
| T2  | 書き込み経路（§6: writable フラグ = I1 拡張、監査、レート制限ブレーカ、ブローカー統合方針の確定 = I6 判断）                                     | T0, I5  | 安全設計レビューを実装前に実施。**実装済み（T2-0〜T2-4、2026-08-05）**                           |
| T3  | MQTT publish（§5.3: rumqttc、on_change/interval、retain、LWT）                                                                                  | T0      | plan.md 保留の MQTT 行を回収。**実装済み（2026-08-05）**                                         |
| T4  | gRPC（§5.4: tonic、proto を `proto/` で版管理、Stream 系）                                                                                      | T1      | 意味論は WS と共通化してから。**実装済み（2026-08-05）**                                         |
| T5  | 配布・運用強化（サービス化検討・ソーク・インストーラ・運用ガイド）                                                                              | T0〜T4  | 実機検証（W5 と同じ項目 + 多重クライアント）含む                                                 |
| T6  | 演算タグ・内部タグ（§4.2: タグ種別 = I1 拡張、式評価器、DAG 検証、`calc`/`mem` 名前空間）                                                       | T0      | 依存は T0 のみ — SCADA 計画次第で T1 直後へ前倒し可。**実装済み（T6-1/T6-2、2026-08-05）**       |
| T7  | オンライン部分再構成（§4.3(c): I7 = 接続単位の入れ替え。それまでは全体再構築で代替）                                                            | T0, I7  | 外部契約（revision/config_changed）は不変のため後入れ可能。**実装済み（T7-1/T7-2、2026-08-05）** |
| T8  | ワードデバイスのビットアクセス（§6.1: `D100.5` 記法、RMW + 確認読み）                                                                           | T2      | **実装済み（T8-1/T8-2、2026-08-06）**                                                            |
| T9  | 接続単位のシミュレーションモード（[ux-plan.md](ux-plan.md) §1）                                                                                 | T0      | UX 改善第1弾（2026-08-06 オーナー決定）。**実装済み（T9-1/T9-2、2026-08-07）**                   |
| T10 | ライブタグモニタ（[ux-plan.md](ux-plan.md) §2）                                                                                                 | T1      | T9 との相乗効果のため T9 の直後。**実装済み（2026-08-07）**                                      |
| T11 | タグ定義の CSV インポート/エクスポート（[ux-plan.md](ux-plan.md) §3）                                                                           | T0      | **実装済み（T11-1/T11-2、2026-08-07）**                                                          |
| T12 | PLC 接続テストボタン（[ux-plan.md](ux-plan.md) §4）                                                                                             | T0      | **実装済み（2026-08-07）**                                                                       |

**T0 実装時の発見（2026-08-05）**: banto-collect の `build_config` は当時
modbus-tcp のみ対応で、SLMP 接続は構成エラーになっていた（banto-plc の
SLMP クライアント I2a は存在するが collect 側が未配線）。§1 の図が謳う
MELSEC SLMP 収集には **I8（banto-collect の SLMP 対応、I 系バックログ）**が
必要だった。**I8 は T2-0 で実装済み（2026-08-05）** — `Protocol::Slmp` /
`SlmpConfig` が `build_config`・接続タスクの client factory に配線され、
SLMP 接続の収集が有効になった（管理 UI の「収集は未対応」注記も撤去済み）。

**P3-b 実装時の発見（監査指摘 2026-08-12、ブランチ `claude/slmp-word-order`、
#127 で 2026-08-11 マージ済み）**: I8 で配線された `SlmpConfig` は `host`/`port`
以外が常に `SlmpConfig::default()` 固定で、`word_order`
（`WordOrder::LowHigh` 既定・MELSEC 標準）を接続ごとに変えられなかった -
`WordOrder::HighLow` を要する機種につなぐと u32/f32 の値が静かに化ける
問題があった。これは banto-broker（banto-hub の SLMP 読み取り経路。
`crate::broker_glue::BrokerReadClient` 参照）と banto-collect の
`slmp_config_for`（relay-wright/chronogazer が使う直接経路）の**両方**に
同じ形で存在していた。`plc_connections.word_order`（migration `0010`、
既定 `low_high` で後方互換）を追加し、`banto_tags::PlcConnection`/
`PlcConnectionInput` → 両クレートの `SlmpConfig` 構築 → banto-hub の
`plc-connections` フォーム（"slmp" 選択時のみ表示）まで配線した。CPU 種別 /
アクセスルート（network/PC/IO/area id）は今回のスコープ外
（`banto-collect::config::slmp_config_for` の doc comment に "Known
limitation" として明記、別スライス候補）。

T0/T1 だけでも「読み取り専用タグサーバー」として出荷可能な形を保つ
（書き込み・MQTT・gRPC は積み増し）。**実機なしで進められる範囲が広い**のが
本計画の狙い: I 系のシミュレータ（Modbus/SLMP、in-process + 実バイト列）が
そのまま使えるため、T0〜T4 は全てシミュレータ相手に実装・テストできる。

**T6-2 実装時の判断（2026-08-05）**: §4.2 の予約セグメント `calc`/`mem` は
I1 の3層構造（接続→グループ→タグ）をそのまま使い、`protocol = "virtual"` の
予約接続として実現した（`banto_tags::plc_connection` モジュール doc 参照 -
`banto-hub` が起動時に自動プロビジョニングし、編集・削除は API 層で拒否）。
演算タグの評価エンジン（`ComputedEngine`）と内部タグの現在値ストア
（`ServerTagStore`）は `apps/banto-hub/core/src/computed.rs` に実装し、
全 IF（REST/WS/MQTT/gRPC）の読み取りは `crate::hub::read_current` という
単一の分岐点（`tag_kind` を見て `CurrentValuesHandle`/`ServerTagStore` を
振り分ける）に統合した。`banto-collect::build_config` は `"virtual"` 接続を
収集対象から除外する（エラーにしない）。詳細は `computed.rs`/
`write_path.rs`/`hub.rs` の doc comment を参照。

## 10. 未決事項（オーナー判断待ち）

1. ~~アプリ名~~ → **`banto-hub` に決定**（2026-08-04、§1）
2. ~~I1 スキーマ拡張の置き場所~~ → **I1 `tags` テーブルへの列追加に決定**
   （2026-08-04）: `writable`（既定 false）・`tag_kind`（既定 `plc`）・
   `expression`（NULL 可）・`retain`（既定 false）を**1回のマイグレーション**に
   まとめる。既定値付き列追加のため後方互換で、ChronoGazer / relay-wright は
   `banto_tags::migrate` の起動時適用にそのまま乗る。別テーブル案はタグの
   同一性が割れて catalog / CRUD / DAG 検証全部が JOIN を背負うため不採用
3. ~~I6（banto-broker）~~ → **共有クレートへの抽出に決定**（2026-08-04）:
   broker.rs は「read/write がワイヤ上で交錯しない」安全保証そのものであり、
   同型再実装は2実装の乖離リスクが最悪。W3-A 設計文書は元々 poller/writer
   以外の利用を想定しており、挙動変更なしで抽出可能。relay-wright の
   既存テストが回帰網になる
4. ~~MQTT 組み込みブローカー~~ → **将来的に実装する方向で、しばらく保留**
   （2026-08-05 オーナー決定。v1 は外部ブローカー接続のクライアントモード
   のみ（§5.3）。実装時期は未定 — ロードマップ上は T5 以降の拡張枠）
5. ~~MQTT 経由の書き込み~~ → **解禁しない**（2026-08-04 決定 — 書き込み
   受付は REST / gRPC の2経路に固定、§6）
6. ~~OpenAPI 自動生成~~ → **utoipa 採用に決定**（2026-08-04、§5.1）:
   catalog は互換性契約のためコードとスキーマを単一ソース化する
7. ~~既定ポート番号~~ → **決定**（2026-08-04、§8）: banto-hub = 8722
   （UI/REST/WS）、gRPC = 50051。既存2アプリの LAN モード既定が両方 8721 で
   重複している潜在問題を発見 — どちらかをずらす件は既存アプリ側バックログ
8. ~~タグサーバーのローカル記録~~ → **§3.3 (a) 案に決定**（2026-08-04）:
   記録あり・保持既定7日。純ゲートウェイ要件が実際に現れたら (b) を
   I 系バックログで対応。※製品機能としてのロガー/日報は作らない（§2）
9. ~~リネームポリシー~~ → **警告のみで確定**（2026-08-05、§4.1）:
   オンライン変更時の外部クライアントへの影響はユーザーの責任範囲。
   ブロック・影響一覧はやらない
10. ~~catalog:full（アドレス露出）の扱い~~ → **PLC アドレスを catalog の
    既定に含める**（2026-08-05、§5.1）: 外部からでもどの PLC アドレスか
    判る方が取り違えを防ぎ、アドレス-タグ対応表を別途見る煩わしさをなくす
    （オーナーの実務所感）。専用スコープ `catalog:full` は廃止し、
    (A)/(B) クライアントは同一 catalog を共有（§7）
11. ~~banto-tagclient（クライアント SDK クレート）の起票時期~~ →
    **本統合作業の設計ゲート（2026-08-29）で実装前設計を確定**。
    REST + WebSocketの読み取り専用データプレーンクライアントとし、
    `config_changed`によるstable ID再bindingを初版に含める。詳細は
    [banto-tagclient-design.md](banto-tagclient-design.md)。実装・release互換commit/tagの
    決定は未了。
12. ~~内部タグを作るか~~ → **採用決定**（2026-08-04、§4.2 で演算タグと
    合わせてタグ種別として設計済み）。
    ~~式文法の確定~~ → **決定**（2026-08-05）: 四則・比較（`==` `!=` `<`
    `>` `<=` `>=`）・論理（`!` `&&` `||`）・条件 `if(c,a,b)`・関数
    `min/max/abs/round/clamp` + **`bit(tag, n)`**。文字列演算は v1 見送り、
    暗黙型変換なし（型不一致は登録時エラー）。**実装済み（T6-1、
    `crates/banto-expr`、2026-08-05）**。残る詳細判断:
    - ~~演算タグ・内部タグの記録要否~~ → **v1 は記録しない（決定・実装済み、
      T6-2、2026-08-05）**: 演算タグ・内部タグはタグ空間のみ（tstore に
      書かない） - `apps/banto-hub/core/src/computed.rs` の
      `ServerTagStore` はオンメモリのみで、tstore への記録経路を持たない。
      演算タグは「入力が記録されていれば再計算可能」という設計どおり、
      内部タグの永続化は `retain` フラグによる最終値のみ（`hub_retained_values`
      テーブル、時系列記録ではない）。記録が必要になった場合は I 系
      バックログとして再検討する
    - ~~T6 の実施時期~~ → **T1 の後、実装済み（T6-1/T6-2、2026-08-05）**
