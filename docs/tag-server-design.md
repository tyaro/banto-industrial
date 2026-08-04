# タグサーバーアプリ 設計ドキュメント（草案）

作成日: 2026-08-04
状態: **設計先行（実装未着手）**。ChronoGazer / relay-wright の開発が実機・現場に
依存して進められない期間に、次の製品柱であるタグサーバーの設計を先に固めるための
文書。マイルストーンは §9（T系）、オーナー判断待ちの未決事項は §10。

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
- **アプリ名（仮）**: `tag-herald`（伝令 — タグ値を外部へ布告する）。
  リポジトリの命名流儀（ChronoGazer, relay-wright）に合わせた案。対案:
  `signal-porter` / `tag-warden`。**オーナー決定待ち**（§10-1）。以下本文書では
  「タグサーバー」と呼ぶ。

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

### 3.2 内部構造 — 「タグ空間」は新規実装しない

FA-Server の「タグ空間」に相当するものは **`banto-collect` の
`CurrentValuesHandle`（現在値 + 品質のキャッシュ、Stale は読み出し時判定）が
既にそのもの**である。タグサーバーのコアは次の既存部品の束ね直しで成立する:

| 役割 | 使う資産 | 備考 |
| --- | --- | --- |
| タグ定義・CRUD | `banto-tags`（I1） | サーバー自身の SQLite に同居。`banto_tags::migrate` 起動時適用 |
| PLC 読み取り | `banto-plc`（I2/I2a） | `banto-collect` 経由で使用（直接は触らない） |
| 周期収集・再接続・品質 | `banto-collect`（I3b） | 接続毎1タスク・指数バックオフ・全NULL行記録の既存設計をそのまま |
| タグ空間（現在値） | `CurrentValuesHandle` | 全外部 IF の読み取り側はこれだけを見る |
| イベント | `EventSink`（broadcast + `collect_events`） | WebSocket / MQTT / gRPC のイベント配信源 |
| ローカル記録 | `banto-tstore`（I3a） | §3.3 |
| 書き込み | `banto-plc-write`（I5）+ ブローカー | §6 |
| 認証・HTTP 骨格 | `banto_server`（banto 本体） | bearer token + CSRF + SSE |

構成変更の扱いも `banto-collect` の司令塔決定に従う: タグ定義の変更は
スナップショット（`build_config` → `CollectorConfig`）の再構築 + `Collector`
の作り直しであり、タグサーバー本体がレジストリ変更を検知して再起動を指示する
（ChronoGazer と同型）。

### 3.3 ローカル記録（tstore）の扱い

現状の `Collector` は `TsWriter` が必須（`data_dir` を要求し、日次ファイルへ
常時追記する）。選択肢は2つ:

- **(a) そのまま記録する（v1 推奨）**: タグサーバーも tstore ファイルを書く。
  追加実装ゼロで、外部システムが取りこぼした期間のバックフィル読み出し
  （`banto-tsquery` は自己記述ファイルを別プロセスから読める）が無料で付く。
  保持期間は `prune_files` の設定で短め（例: 既定7日）にして記録計との
  役割重複を避ける。
- **(b) 純ゲートウェイモード**: `CollectorOptions` に writer 無効化を追加する
  小改修（I 系バックログ）。ディスク書き込みゼロが要件の現場向け。v1 では
  やらず、(a) の保持期間短縮で代替する。

### 3.4 スレッド/タスク構造

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
- **リネームは破壊的変更**: 管理 UI 上で「このタグは外部公開されている」
  ことを前提に、リネーム時に警告する（v1 は警告まで。参照追跡はしない）

## 5. 外部インターフェース設計

優先順は **REST → WebSocket → (書き込み) → MQTT → gRPC → (OPC UA)**。
REST/WebSocket は axum に同居するため追加依存ゼロで先行できる。

### 5.1 REST（T0）

`banto_server` の骨格（bearer 認証 + CSRF + 監査ログ）に載せる。人間用の
管理 UI セッションと、機械クライアント用の **API キー**（§5.6）の2系統。

| Method | Path | 内容 |
| --- | --- | --- |
| GET | `/api/v1/tags` | catalog: 全タグの定義 + メタデータ + 安定 ID + `revision`（`?group=`, `?connection=` フィルタ）。**PLC アドレスは既定で含めない**（§4.1 のデータプレーンクライアントには不要な内部情報。レジストリ同期クライアント向けには `catalog:full` スコープの API キーでのみ露出 — §7） |
| GET | `/api/v1/values` | 全タグの現在値 + 品質 + 時刻の一括スナップショット（`?tags=a,b,c` 部分指定） |
| GET | `/api/v1/values/{tag}` | 単一タグの現在値 |
| POST | `/api/v1/values/{tag}` | 書き込み（§6。writable タグのみ、監査必須） |
| GET | `/api/v1/status` | サーバー状態: 接続毎の `ConnectionStatus`、収集稼働状況、バージョン |
| GET | `/api/v1/events` | `collect_events` の範囲クエリ（収集開始/停止・PLC断/復旧・しきい値） |
| GET | `/api/v1/openapi.json` | スキーマ（utoipa 等での自動生成を検討、§10-6） |

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
- バックプレッシャ: 送信バッファが閾値超過したクライアントは切断
  （遅い購読者が収集側を止めない — broadcast lag と同じ思想）
- SSE 版（`banto_server` に既存）は管理 UI 内部用にのみ使い、外部公開の
  購読 IF は WebSocket に一本化する

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
- **MQTT 経由の書き込み（`.../set` 購読）は v1 ではやらない**: 認証主体の
  特定と監査の要件（§6）を MQTT の認可モデルで満たす設計が別途必要なため。
  ブローカー側 ACL 前提の設計を §10-5 で判断

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

## 6. 書き込み経路の安全設計

relay-wright が確立した規律を、タグサーバーの文脈に翻訳して引き継ぐ。
**タグサーバーの書き込みはルールエンジンを持たない**（条件判断・自動化は
relay-wright の専管）。それでも外部システム起点の書き込みには同水準の
ガードを敷く:

1. **per-tag opt-in**: タグ定義に `writable`（既定 false）を追加（I1 の
   スキーマ拡張、§10-2）。writable でないタグへの書き込みは 403
2. **API キースコープ**: 書き込みは `write:` スコープ内のタグのみ。
   read キーで書けない
3. **log-before-write**: relay-wright の write_audit と同型の監査テーブルに
   「誰が（キーID）・どのタグへ・何を・結果」を**書き込み実行前に**記録し、
   実行後に結果を追記
4. **レート制限ブレーカ**: タグ毎 + 全体の書き込みレート上限。超過で
   該当キーを自動失効（トリップ）し、イベント発行。復帰は管理 UI から手動
5. **読み書き単一セッション**: 書き込みは収集と同じ PLC セッションを通す
   必要がある（実機のセッション数上限）。relay-wright の
   `engine/broker.rs`（W3-A: 接続毎1タスク・mpsc 直列化・read/write が
   ワイヤ上で交錯しない構造保証）と同じ設計が必要 — **broker を共有クレート
   `banto-broker`（仮、I6）へ抽出**し、relay-wright とタグサーバーの両方が
   使う形を提案（§10-3）。v1 実装順では、書き込み対応（T2）の時点で
   `banto-collect` の読み取り専有タスクとブローカーの統合方法を確定させる
   （collect のタスクがブローカー経由で読むよう改修するか、書き込み対象
   接続のみブローカー管理とするか — T2 の設計課題として持ち越し）
6. **再起動での安全側復帰**: 書き込み受付は起動時 disabled とし、管理 UI で
   明示的に有効化する（relay-wright のアーミングと同じ「再起動で必ず安全側」）

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
- **(B) レジストリ同期クライアント（移行期の特例）**: `catalog:full` スコープで
  アドレス込みの完全定義を取得し、自分で PLC と直接通信する。既存アプリの
  移行過渡期と、書き込みセッションを専有し続ける relay-wright のための形態

段階制の移行計画:

| 段階 | 構成 | 説明 |
| --- | --- | --- |
| 現状 | 各アプリが直接 PLC 接続 | 単独稼働なら問題なし。同居時はセッション数に注意（W5 実機検証の結果待ち） |
| v1 | タグサーバー単独でも製品 | 外部システム連携（MES/クラウド/自作画面）が主用途 |
| v1.x | クライアント SDK クレート（`banto-tagclient` 仮） | §4.1 のバインドモデル（catalog キャッシュ・`config_changed` での再バインド・購読の再接続・オフライン時の unresolved 化）を1回だけ実装し、以後の全アプリが再利用する I 系資産。SCADA 着手の前提 |
| v2 候補 | 新規 SCADA アプリ | 最初から (A) データプレーンクライアントとして作る — タグマネージャー画面は「タグサーバーの catalog ブラウザ + バインド管理」になる |
| v2 候補 | ChronoGazer リモート収集モード | 収集をタグサーバーへ委譲し、自分は購読 + tsquery で読む (A) 型モードを追加。UI と収集の寿命分離も同時に達成 |
| v2 候補 | relay-wright の読み取り委譲 | 条件評価の入力読み取りをタグサーバー購読 (A) へ。**書き込みセッションは relay-wright 専有を維持**（安全機構と一体のため、(B) の恒久例外） |

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
- **ポート**（既定、全て設定可能）: 管理 UI + REST + WS = 1ポート（8085 案。
  ChronoGazer / relay-wright の既定ポートと衝突しない値を選定 — 要確認）、
  gRPC = 50051
- **設定**: 既存アプリ同様 SQLite 内 settings + 起動時引数。タグ定義は
  管理 UI で CRUD（I1 サービス層をそのまま利用）
- **監視**: `/api/v1/status` が死活・接続状態・書き込み受付状態を返す。
  MQTT の `$state` retain と合わせて外部監視に載せられる
- **ソークテスト**: 収集 24/365 + 外部クライアント購読を維持した状態での
  連続稼働試験（banto-collect の 72h ソーク雛形を流用）を出荷条件に含める

## 9. マイルストーン（T系）

| # | 内容 | 依存 | 備考 |
| --- | --- | --- | --- |
| T0 | 骨格: ヘッドレス axum アプリ + レジストリ配線（I1 CRUD + 管理UI雛形）+ banto-collect 組み込み + REST 読み取り一式（§5.1）+ API キー基盤（§5.6） | I1〜I3b | Tauri なし構成の初例。シミュレータで E2E |
| T1 | WebSocket 購読（§5.2: subscribe/on_change/interval・初期スナップショット・バックプレッシャ切断） | T0 | |
| T2 | 書き込み経路（§6: writable フラグ = I1 拡張、監査、レート制限ブレーカ、ブローカー統合方針の確定 = I6 判断） | T0, I5 | 安全設計レビューを実装前に実施 |
| T3 | MQTT publish（§5.3: rumqttc、on_change/interval、retain、LWT） | T0 | plan.md 保留の MQTT 行を回収 |
| T4 | gRPC（§5.4: tonic、proto を `proto/` で版管理、Stream 系） | T1 | 意味論は WS と共通化してから |
| T5 | 配布・運用強化（サービス化検討・ソーク・インストーラ・運用ガイド） | T0〜T4 | 実機検証（W5 と同じ項目 + 多重クライアント）含む |

T0/T1 だけでも「読み取り専用タグサーバー」として出荷可能な形を保つ
（書き込み・MQTT・gRPC は積み増し）。**実機なしで進められる範囲が広い**のが
本計画の狙い: I 系のシミュレータ（Modbus/SLMP、in-process + 実バイト列）が
そのまま使えるため、T0〜T4 は全てシミュレータ相手に実装・テストできる。

## 10. 未決事項（オーナー判断待ち）

1. **アプリ名**: `tag-herald` 案の採否（§1）
2. **I1 スキーマ拡張**: `writable` フラグの置き場所（`tags` 列追加 vs
   タグサーバー側の別テーブル）。I1 は3アプリ共有クレートのため、列追加なら
   ChronoGazer / relay-wright のマイグレーション追従が必要
3. **I6（banto-broker）**: relay-wright の broker.rs を共有クレートへ抽出するか、
   タグサーバー側で同型を再実装するか（W3-A の設計文書には「将来の poller/writer」
   前提の記述があり抽出向きだが、relay-wright の安定を優先して凍結する選択もある）
4. **MQTT 組み込みブローカー**（rumqttd 同梱）を将来提供するか
5. **MQTT 経由の書き込み**を将来解禁するか（ブローカー ACL 前提の認可設計が必要）
6. **OpenAPI 自動生成**（utoipa）を入れるか、手書きドキュメントで始めるか
7. **既定ポート番号**の割り当て（既存2アプリの既定値と衝突しない体系を決める）
8. **タグサーバーのローカル記録**: §3.3 (a) 案（記録あり・保持短め）で
   問題ないか。純ゲートウェイ要件の現場が見えているなら (b) の I 系改修を
   T0 に前倒し。※製品機能としてのロガー/日報は作らないことは決定済み（§2）
9. **リネームポリシー**（§4.1）: v1 の「警告のみ」で足りるか、外部公開済み
   タグの名前変更を管理 UI でブロック（要確認ダイアログ + 影響一覧）まで
   やるか
10. **`catalog:full`（アドレス露出）の扱い**（§5.1/§7）: レジストリ同期
    クライアント (B) を正式サポートするか、ChronoGazer / relay-wright の
    移行はエクスポート/インポートで済ませて API からのアドレス露出自体を
    やめるか
11. **`banto-tagclient`（クライアント SDK クレート）**の起票時期（§7）:
    SCADA 計画が具体化する前に T1 完了時点で先行着手するか
