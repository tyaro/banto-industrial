# banto-hub 運用ガイド

対象読者: banto-hub を現場に導入・運用するオペレータ／導入担当者。
banto-hub は Rust(axum) + SQLite の**単一 exe・ヘッドレスサーバー**です
（Tauri アプリではありません）。産業用 PLC からタグを収集し、
REST / WebSocket / MQTT / gRPC の4経路で外部へ公開する「タグサーバー」
という位置づけです。

設計の一次ソースは [docs/tag-server-design.md](tag-server-design.md)
です。本書は運用手順に絞った実務ガイドで、設計の背景や決定の経緯は
そちらを参照してください。

## 目次

1. [起動・環境変数](#1-起動環境変数)
2. [ポート運用](#2-ポート運用)
3. [API キー運用](#3-api-キー運用)
4. [書き込み受付の運用手順](#4-書き込み受付の運用手順)
5. [ビット書き込みのワード専有規約](#5-ビット書き込みのワード専有規約)
6. [TLS / リバースプロキシ](#6-tls--リバースプロキシ)
7. [MQTT 設定](#7-mqtt-設定)
8. [gRPC 設定](#8-grpc-設定)
9. [死活監視](#9-死活監視)
10. [Windows サービス化（常駐）](#10-windows-サービス化常駐)
11. [インストーラ](#11-インストーラ)

## 1. 起動・環境変数

banto-hub の唯一の起動経路は `banto-hub.exe`（ビルド元:
`apps/banto-hub/core/src/bin/banto-hub.rs`）です。Tauri シェルはなく、
このバイナリを直接実行するだけでサーバー・管理 UI・収集エンジンすべてが
起動します。

### 環境変数

| 変数                | 既定値                                         | 説明                                                                                                                                 |
| ------------------- | ---------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------ |
| `PORT`              | 設定 `server.port`（未設定なら **8722**）      | 管理 UI + REST + WebSocket が listen するポート                                                                                      |
| `BANTO_BIND`        | 設定 `server.bind`（未設定なら **127.0.0.1**） | bind するアドレス。既定はローカルホストのみ — LAN 上の他端末からアクセスさせる場合は `0.0.0.0` を指定する（§6 の前提と合わせて検討） |
| `BANTO_DB`          | `./banto-hub.sqlite3`                          | SQLite データベースファイルのパス                                                                                                    |
| `BANTO_HUB_DATA`    | 設定 `data.dir`（未設定なら **`./data`**）     | tstore（ローカル記録ファイル）の出力先ディレクトリ                                                                                   |
| `BANTO_ALLOW_SETUP` | 未設定（＝許可しない）                         | `1` を指定すると `POST /api/auth/setup`（初回管理者アカウント作成）を許可する                                                        |

`PORT`/`BANTO_BIND`/`BANTO_HUB_DATA` は「環境変数 → DB に保存された設定
→ 組み込みの既定値」の優先順位で決まります。一度 DB に設定が保存された
後も、環境変数を指定すればその起動時だけ上書きできます。

### 初回セットアップの運用

`BANTO_ALLOW_SETUP=1` は**初回起動時のみ**指定してください。この環境変数
が立っている間は `POST /api/auth/setup` で誰でも最初の管理者アカウントを
作成できてしまうため、初回セットアップ（最初の管理者アカウント作成）が
終わったら次回起動からは外す運用にします。閉域 LAN 前提とはいえ、
セットアップ用の穴を常時開けたままにしないという基本的な運用規律です。

### 起動コマンド例（Windows PowerShell）

初回セットアップ時:

```powershell
$env:BANTO_ALLOW_SETUP = "1"
.\banto-hub.exe
```

ブラウザで `http://127.0.0.1:8722`（既定ポート）を開き、最初の管理者
アカウントを作成します。作成が終わったら一度サーバーを停止し、
`BANTO_ALLOW_SETUP` を外して通常運用に切り替えます。

2回目以降（通常運用）:

```powershell
.\banto-hub.exe
```

LAN 上に公開する場合（同一閉域 LAN 内の他端末からアクセスさせたい場合）:

```powershell
$env:BANTO_BIND = "0.0.0.0"
.\banto-hub.exe
```

起動後、コンソールに DB のパス・データディレクトリ・listen 中の URL・
gRPC の状態・初回セットアップの許可状態が表示されます。停止は
コンソールで `Ctrl-C` です（MQTT publisher → gRPC サーバー →
収集エンジン（tstore flush 含む）→ broker セッション → HTTP サーバーの
順で安全にシャットダウンします）。

## 2. ポート運用

| ポート    | 用途                           | 既定の有効/無効                                          |
| --------- | ------------------------------ | -------------------------------------------------------- |
| **8722**  | 管理 UI + REST API + WebSocket | 常時有効（`server.port`）                                |
| **50051** | gRPC                           | **既定は無効**（`grpc.enabled=false`。有効化は §8 参照） |

いずれも管理 UI の設定または環境変数で変更可能です。gRPC サーバーは
`BANTO_BIND` の値に関わらず**常に全インターフェース（`0.0.0.0`）に
bind** します。REST/WS/UI（8722 番ポート）だけが `BANTO_BIND` の対象
であることに注意してください — gRPC を有効化した時点で、そのポートは
`BANTO_BIND=127.0.0.1` のままでも LAN 上の他端末から到達可能になります。

### 既存2アプリとのポート重複について

社内の既存2アプリ（ChronoGazer / relay-wright）の LAN モードは、
どちらも既定ポートが **8721** です（banto-hub は隣の 8722 を使うことで
意図的に衝突を避けています）。ただし ChronoGazer / relay-wright の
LAN モードはどちらも既定で無効なので、素の状態では実害はありません。
同一 PC 上で banto-hub と ChronoGazer・relay-wright の LAN モードを
**両方有効化して同居させる場合**は、どちらか一方のポートを変更する
必要がある点に注意してください（両アプリとも 8721 が既定のため、
同時に有効化すると衝突します）。

### ファイアウォール開放

LAN 上の他端末から banto-hub にアクセスさせる場合、Windows
ファイアウォールで次のポートを開放してください（受信規則、TCP）。

- **8722**（またはカスタムポート）: 管理 UI・REST・WebSocket
- **50051**（またはカスタムポート）: gRPC を有効化した場合のみ

MQTT publish は banto-hub が**外部ブローカーへ接続しに行くクライアント**
（受信 listen はしない）なので、banto-hub 側でポートを開放する必要は
ありません。ブローカー側（AWS IoT / EMQX / Mosquitto 等）への outbound
通信が許可されていることを確認してください。

## 3. API キー運用

機械クライアント（MES・上位 SCADA・自作ダッシュボード等）が
`/api/v1/*`（REST・WebSocket・gRPC の tag-space API）へアクセスする
際の認証情報です。管理 UI のログイン（セッション bearer token）とは
別系統です。

### キー形式とスコープ

平文キーは `bh_{prefix}_{secret}` の形式です（例:
`bh_AbCdEfGh_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx`）。
リクエストには `Authorization: Bearer <平文キー>` ヘッダを付けます
（WebSocket は接続時ヘッダ、gRPC は metadata）。

スコープは発行時に文字列のリストで指定します。

- `read`: `/api/v1/tags`・`/api/v1/values`・`/api/v1/stream` などの
  読み取り系すべてに使える
- `write:{connection}.{group}.{tag}`: 指定した1タグへの書き込みのみ許可
  （例: `write:line1.fast.setpoint01`）

**write スコープはワイルドカード不可・完全一致のみ**です
（`write:line1.fast.*` のような指定や3セグメント以外の指定は発行時に
拒否されます）。複数タグへの書き込みを許可したい場合は、スコープを
タグの数だけ列挙してください。

### 管理 REST（admin ロール限定、管理 UI からも操作可能）

| Method | Path                            | 内容                                                     |
| ------ | ------------------------------- | -------------------------------------------------------- |
| POST   | `/api/api-keys`                 | 発行（`{ "name", "scopes": [...] }` → 平文キー含む応答） |
| GET    | `/api/api-keys`                 | 一覧（平文キー・ハッシュは含まない）                     |
| POST   | `/api/api-keys/{id}/revoke`     | 失効（不可逆・履歴として残る。冪等）                     |
| POST   | `/api/api-keys/{id}/clear-trip` | レート制限トリップの解除（§4 参照。冪等）                |

### 運用上の注意（平文キーは発行応答限り）

**平文キーが見られるのは `POST /api/api-keys` の応答だけです。** DB に
保存されるのはハッシュのみで、`GET /api/api-keys` の一覧にも平文キーや
ハッシュは一切含まれません。発行画面を離れる・応答をコピーし忘れる等で
平文キーを紛失した場合、**同じキーを再表示する手段はありません**。
その場合は新しいキーを発行し、古いキーは失効させてください。

失効（`revoke`）は不可逆です。誤って失効させても復元はできないため、
必ず新しいキーを発行し直してクライアント側の設定を更新してください。

## 4. 書き込み受付の運用手順

banto-hub の書き込みは relay-wright のようなルールエンジンを持たず、
「外部クライアントの明示要求を1回転送するだけ」のパススルーです。
受付経路は **REST（`POST /api/v1/values/{tag}`）と gRPC
（`WriteValue`）の2つのみ** — WebSocket に書き込み op はなく、MQTT
経由の書き込みも解禁されていません。

安全設計として8段のゲート（catalog 解決 → writable 判定 → 実効
enabled → プロトコル対応 → 受付トグル → レート制限 →
値変換・範囲チェック → log-before-write）を通しますが、運用上
押さえておくべきは次の2点です。

### 起動時は必ず OFF、管理 UI から明示的に有効化する

**書き込み受付は、プロセスを起動するたびに必ず無効（disabled）から
始まります。** 前回終了時に有効にしていても、再起動後は自動的には
再開しません（`GET /api/v1/status` の `write_was_enabled_before_restart`
に「再起動前は有効だったか」の履歴が表示されますが、これは表示専用で、
実際の受付可否には一切影響しません）。

書き込みを受け付けさせるには、管理 UI から明示的に有効化するか、
admin 権限で次の管理 REST を呼びます。

| Method | Path                         | 内容                 |
| ------ | ---------------------------- | -------------------- |
| POST   | `/api/write-control/enable`  | 書き込み受付を有効化 |
| POST   | `/api/write-control/disable` | 書き込み受付を無効化 |

有効化されていない間、`/api/v1/values/{tag}` への書き込みリクエストは
すべて拒否されます（受付トグル OFF による拒否）。運用フローとしては、
「起動 → 収集・読み取りが安定していることを確認 → 必要なら書き込み受付を
有効化」という順序を徹底してください。

### レート制限トリップ時の復帰手順

書き込みはタグ毎 + 全体の2段でレート制限がかかっています（既定値:
60秒ウィンドウで全体30件・タグ毎10件、v1では設定変更不可の固定値）。
上限を超えると、超過を起こした API キーが**トリップ**（一時停止）
状態になります。

トリップはキーの**失効ではありません**。誤ってトリップした場合や、
原因を確認した上で書き込みを再開したい場合は、管理 UI または
`POST /api/api-keys/{id}/clear-trip`（admin 限定）で手動解除します。
自動では解除されません。トリップ中のキーは `read`/`write` いずれの
`/api/v1/*` リクエストも拒否されるため、読み取り用途で同じキーを
併用している場合は影響範囲に注意してください（読み取り専用の運用では
別キーを分けることを推奨します）。

## 5. ビット書き込みのワード専有規約

banto-hub はワードデバイス（SLMP の `D` レジスタ等）の個別ビットを、
アドレスにビット記法（例: `D100.5`）を付けた通常のタグとして
読み書きできます。**この機能を使う場合、現場の PLC プログラム側で
必ず守るべき規約があります。**

### RMW（読み→変更→書き戻し→確認読み）の仕組み

SLMP にはワードデバイスへのビット単位書き込みコマンドがないため、
banto-hub がビット書き込みを行う際は次の手順を1つのジョブとして
実行します。

1. 対象ワードを読む
2. 該当ビットだけを変更する
3. ワード全体を書き戻す
4. 書き戻し後の値をもう一度読み、意図通りに反映されたか確認する

同一バッチ内で同じワードの複数ビットを書く場合は、マスク合成して
1回の RMW にまとめます。この一連の手順は broker（PLC セッションの
直列化機構）の1ジョブとして実行されるため、banto-hub や relay-wright
からの並行書き込み同士が競合することはワイヤ上あり得ません。

### なぜ「外部から書くビットを含むワードは PLC 側から書かない」規約が必要か

RMW の**読みと書きの間**に、PLC のラダープログラム側が同じワードの
**別のビット**を書き換えると、banto-hub の書き戻しがその変更を
上書きして消してしまいます。これは banto-hub 側の実装の問題ではなく、
「ワード単位でしか書けないデバイスに対して、外部と PLC の両方が
非同期にビット単位の意図を持つ」という状況そのものが原理的に競合を
防げない構造だからです（アトミックなビット書き込みコマンドが
存在しない以上、回避できません）。

banto-hub は競合を**検出**します。書き戻し後の確認読みで期待した値と
異なっていた場合、その書き込みは失敗（Bad）として扱われ、
「書き戻し競合の可能性があります」という理由とともに書き込み監査ログ
に記録されます。検出はできますが、値そのものを守ることはできません。

**運用規約**: 外部（banto-hub 経由）から書き込むビットを含むワードは、
**PLC 側のラダープログラムから一切書き込まない**でください。これは
ハンドシェイク領域（PLC↔外部システム間の受け渡し専用ワード）を
専有する、という考え方です。同じワードに PLC 側とタグサーバー側の
両方が書き込む設計にすると、競合検出はできても値の欠落は防げません。
新規にビット書き込み用のワードを設計する際は、専用の予備ワードを
割り当ててください。

（Modbus 対応のビット書き込みを実装する場合は FC22 Mask Write
Register がアトミックなためこの制約自体が不要になりますが、v1 時点で
Modbus への書き込みは未対応です）。

## 6. TLS / リバースプロキシ

banto-hub の v1 は**平文通信（HTTP/gRPC 平文）+ 閉域 LAN 前提**です。
外部からの盗聴・改ざんのリスクを許容できない環境や、TLS 終端が
必須の要件がある場合は、banto-hub の前段にリバースプロキシを置いて
TLS を終端してください。banto-hub 自体には TLS 対応を組み込みません。

### Caddy を使う例

[Caddy](https://caddyserver.com/) は自動証明書取得・設定の簡潔さから
おすすめです。閉域 LAN 内で自己署名証明書を使う最小構成の例
（`Caddyfile`）:

```
hub.local:443 {
    tls internal
    reverse_proxy 127.0.0.1:8722
}
```

- `tls internal` は Caddy 内蔵の CA で自己署名証明書を発行します
  （社内 LAN 限定であれば、クライアント側にこの CA を信頼させるだけで
  警告なしに使えます）。パブリック CA の証明書が必要な場合は
  ドメインとポート開放を用意した上で通常の ACME 設定に切り替えます。
- WebSocket（`/api/v1/stream`）は `reverse_proxy` がプロトコル
  アップグレードを自動で通すため、追加設定は不要です。
- gRPC（50051 番ポート）を TLS 終端したい場合は別ブロックで
  `reverse_proxy` に `transport http { versions h2c }` を指定するか、
  gRPC 専用のリバースプロキシ構成を別途検討してください（gRPC は
  HTTP/2 前提のため REST/WS とは設定が異なります）。

いずれの場合も、banto-hub 自体は `BANTO_BIND=127.0.0.1` のまま
（リバースプロキシとの通信のみループバックで受ける）にし、外部からの
直接アクセスはリバースプロキシのポートのみを開放する構成を推奨します。

## 7. MQTT 設定

banto-hub は MQTT ブローカーへ接続しに行く**クライアント**として
タグ値を publish します（組み込みブローカーは持ちません）。

### 設定項目

管理 UI の設定ページ、または admin 限定の管理 REST
`GET/PUT /api/mqtt-settings` で設定します。下表の「キー」は DB の
`settings` テーブル上のキー名です。`GET`/`PUT` の JSON ボディでは
同じ項目が camelCase（例: `mqtt.client_id` → `clientId`、
`mqtt.min_interval_ms` → `minIntervalMs`）になります。

| キー                   | 既定値      | 説明                                                                         |
| ---------------------- | ----------- | ---------------------------------------------------------------------------- |
| `mqtt.enabled`         | `false`     | MQTT publish の有効/無効                                                     |
| `mqtt.host`            | （空）      | ブローカーのホスト名/IP。`enabled=true` の場合は必須                         |
| `mqtt.port`            | `1883`      | ブローカーのポート                                                           |
| `mqtt.client_id`       | `banto-hub` | MQTT クライアント ID                                                         |
| `mqtt.username`        | （未設定）  | ブローカー認証のユーザー名（任意）                                           |
| `mqtt.password`        | （未設定）  | ブローカー認証のパスワード（任意）                                           |
| `mqtt.prefix`          | `banto`     | トピックの先頭セグメント（トピックは `{prefix}/{connection}/{group}/{tag}`） |
| `mqtt.qos`             | `1`         | QoS（`0`/`1` のみ対応、`2` は未対応）                                        |
| `mqtt.min_interval_ms` | `1000`      | 最短発行間隔のスロットル（ミリ秒）                                           |

`PUT` で保存すると即座に適用されます（古い接続を止めて新しい設定で
再接続）。再起動は不要です。

### 運用上の注意

- `GET /api/mqtt-settings` は `password` を一切返しません。管理 UI で
  設定を更新する際、パスワード欄を空のままにすると「変更なし（既存の
  パスワードを維持）」として扱われます。パスワードを変更したい場合は
  必ず新しい値を入力してください。
- **`mqtt.password` は DB に平文で保存されます。** API キーのハッシュ
  とは異なり、ブローカーへの認証情報はクライアントへ渡す時点で
  どのみち平文に戻す必要があるため、ハッシュ化しても保護になりません
  （§6 と同じ「閉域 LAN 前提」の線引きです）。DB ファイル
  （`banto-hub.sqlite3`）自体のアクセス権限を適切に管理してください。
- `GET /api/v1/status` の `mqtt.connected` で実際にブローカーへ
  接続できているかを確認できます（`mqtt.enabled` は設定値、
  `connected` はライブの接続状態で、両者は独立しています）。

## 8. gRPC 設定

gRPC は既定で**無効**です。.NET / Java / Python 等の上位システムから
型付き API で接続したい場合に、管理 UI または admin 限定の管理 REST
`GET/PUT /api/grpc-settings` で明示的に有効化してください。

| キー           | 既定値  | 説明                     |
| -------------- | ------- | ------------------------ |
| `grpc.enabled` | `false` | gRPC サーバーの有効/無効 |
| `grpc.port`    | `50051` | listen ポート            |

`PUT` で保存すると即座に適用されます（古いサーバーを止めて新しい設定で
再起動）。**gRPC サーバーは有効化すると `BANTO_BIND` の値に関わらず
常に `0.0.0.0`（全インターフェース）で listen します**（§2 参照）。
LAN 上に公開したくない場合は `grpc.enabled=false` のままにするか、
ファイアウォールで 50051 番ポートへのアクセスを制限してください。

proto 定義は `proto/tagserver/v1/` にリポジトリ内で管理されており、
外部クライアント SDK 生成の元になります。

## 9. 死活監視

`GET /api/v1/status` はサーバー全体の死活・状態を1回のリクエストで
返します（`read` スコープの API キー、または管理 UI セッションで
アクセス可能）。外形監視（ヘルスチェック）にはこのエンドポイントを
使ってください。

主な内容（フィールド名は応答 JSON そのまま — この応答は snake_case です。
API キー一覧など他の管理 REST 応答は camelCase なので、実装をまたいで
パースするクライアントは混同しないよう注意してください）:

| フィールド                         | 内容                                                                          |
| ---------------------------------- | ----------------------------------------------------------------------------- |
| `version`                          | banto-hub のバージョン                                                        |
| `revision`                         | タグ定義・接続構成の世代番号（構成変更のたびに単調増加）                      |
| `last_config_error`                | 直近の構成適用エラー（あれば）。`null` なら直近の適用は成功                   |
| `connections[]`                    | 接続毎の状態（`connected`/`reconnecting`/`stopped`）と再接続試行回数          |
| `write_enabled`                    | 書き込み受付の**現在の**状態（§4 参照）                                       |
| `write_was_enabled_before_restart` | 再起動前は有効だったかの履歴表示（現在の受付可否には影響しない）              |
| `mqtt.enabled` / `mqtt.connected`  | MQTT publish の設定値 / 実際にブローカーへ接続できているか（§7 参照）         |
| `grpc.enabled` / `grpc.port`       | gRPC の設定値（listen できているかのライブ状態は持たない。§8 参照）           |
| `last_apply`                       | 直近の構成再構築（`apply_config`）で追加/削除/入れ替え/無変更になった接続一覧 |

監視間隔の目安としては、PLC の再接続バックオフや MQTT の再接続を
見逃さない範囲で数十秒〜1分程度のポーリングを推奨します。MQTT の
`$state`（LWT、`{prefix}/$state` トピックに `online`/`offline`）と
組み合わせれば、MQTT ブローカー側からも banto-hub の死活を監視できます。

`last_config_error` が非 null の状態が続く場合、タグ定義や接続設定に
問題がある可能性があります（構成変更は all-or-nothing で適用される
ため、エラー時は直前の正常な構成のまま動作が継続します）。管理 UI で
該当する接続・タグ定義を確認してください。

## 10. Windows サービス化（常駐）

T5-1（`windows-service` クレートによる本実装）。§1
のコンソール起動（Ctrl-C で停止する対話プロセス）に加えて、
`banto-hub.exe` は Windows サービスとして常駐登録できます。ログオン
ユーザーがいなくても OS 起動時に自動的に立ち上がる運用にしたい場合は
こちらを使ってください。コンソール起動とサービス起動はどちらも同じ
実行ファイルで、サブコマンドの違いだけです。

**以下の操作はすべて管理者権限の PowerShell（「管理者として実行」）が
必要です。** 一般ユーザー権限では Service Control Manager
（SCM）への登録・操作ができません。

### サービスの登録（install）

```powershell
# 管理者権限の PowerShell で、banto-hub.exe のあるディレクトリから実行
.\banto-hub.exe install
```

登録されるサービスの情報:

| 項目           | 値                                                                           |
| -------------- | ---------------------------------------------------------------------------- |
| サービス名     | `BantoHub`（`Get-Service`/`sc query` 等で使う内部識別子）                    |
| 表示名         | `banto-hub タグサーバー`                                                     |
| 実行ファイル   | `install` 実行時の `banto-hub.exe` の絶対パス（起動引数 `run-service` 付き） |
| 起動種別       | 自動（遅延開始）                                                             |
| 実行アカウント | ローカルシステムアカウント                                                   |

起動種別を「自動（**遅延**開始）」にしているのは、banto-hub が起動直後に
TCP bind や（設定次第で）LAN 上の PLC への接続を試みるため、OS 起動
直後のネットワークスタック初期化前に起動が競合する事故を避けるためです。
OS 起動から実際にサービスが立ち上がるまで、他の自動開始サービスより
少し遅れます（数十秒程度）。

`install` は冪等ではありません。既に `BantoHub` サービスが登録済みの
状態で再度 `install` を実行するとエラーになります。設定
（実行ファイルパス等）を変更したい場合は、先に `uninstall` してから
`install` し直してください。

環境変数（§1 の `PORT`/`BANTO_BIND`/`BANTO_DB`/`BANTO_HUB_DATA`/
`BANTO_ALLOW_SETUP`）はサービスのプロセス環境には引き継がれません。
サービスとして運用する場合にこれらを固定したい場合は、OS のシステム
環境変数として設定してから `install`（＝サービス登録）してください
（プロセス起動時に読み込まれるため、システム環境変数に設定しておけば
サービスにも反映されます）。特に初回セットアップ
（`BANTO_ALLOW_SETUP=1`）は、コンソールモードで一度セットアップを
済ませてからサービス化する運用を推奨します。

### 起動確認

```powershell
# サービスは起動種別が「自動（遅延開始）」なので OS 再起動で自動的に
# 立ち上がるが、その場で起動したい場合は明示的に開始する
Start-Service BantoHub

# 状態確認（PowerShell ネイティブ）
Get-Service BantoHub

# 状態確認（sc.exe、詳細な終了コード等も見たい場合）
sc query BantoHub
```

`Get-Service BantoHub` の `Status` が `Running` になっていれば起動
成功です。`http://127.0.0.1:8722`（既定ポート、§2 参照）へブラウザで
アクセスして管理 UI が開くことも合わせて確認してください。

### サービスログの確認

サービスにはコンソールがないため、コンソールモードの `println!`/
`eprintln!` 出力（起動時のリスニング URL・シャットダウン順序の各ステップ
等）はどこにも表示されません。代わりに、サービスモードで動いている間は
同じ内容がログファイルにも1行ずつ（タイムスタンプ付きで）追記されます。

```
{data_dir}\banto-hub-service.log
```

`data_dir` は §1 の `BANTO_HUB_DATA`（未設定なら既定 `./data`、
サービスのプロセスの作業ディレクトリからの相対パス）です。既定のまま
`install` した場合、実行ファイルと同じディレクトリの `.\data\
banto-hub-service.log` に出力されます。

```powershell
# 直近50行を確認
Get-Content .\data\banto-hub-service.log -Tail 50

# リアルタイムで追いかける
Get-Content .\data\banto-hub-service.log -Tail 20 -Wait
```

ログの1行は `[YYYY-MM-DD HH:MM:SS] メッセージ` の形式です（メッセージ
本文はコンソールモードの出力と同一 — 「Ctrl-C で停止」という案内文が
サービスログにも出ますが、実際の停止操作は次項の `Stop-Service`
（＝ SCM 経由の停止要求）です。サービスにはコンソールが無いため
Ctrl-C 自体は無意味ですが、これは共通コードを流用しているだけの無害な
案内文です）。

### 停止

```powershell
Stop-Service BantoHub
```

SCM からの停止要求を受け取ると、コンソールモードの Ctrl-C と全く同じ
シャットダウン順序（MQTT publisher → gRPC サーバー → 収集エンジン
（tstore flush 含む）→ broker セッション → HTTP サーバー、§1 参照）で
安全に停止します。

### 障害時の自動復帰（推奨設定）

既定では、サービスがクラッシュ（異常終了）しても自動再起動されません。
現場常駐用途では、`sc failure` で障害時アクションを設定しておくことを
推奨します。

```powershell
# 1回目・2回目・3回目以降の失敗すべてで60秒後に再起動、
# 失敗カウンタは86400秒（24時間）でリセット
sc failure BantoHub reset= 86400 actions= restart/60000/restart/60000/restart/60000
```

設定内容を確認する場合:

```powershell
sc qfailure BantoHub
```

これは Windows サービス標準の障害復帰機構で、banto-hub 固有の設定では
ありません（`windows-service` クレート・banto-hub のコードは一切関与
しません）。詳細は Microsoft の `sc.exe failure` ドキュメントを参照して
ください。

### アンインストール

```powershell
.\banto-hub.exe uninstall
```

実行中であれば自動的に停止してから登録解除します。登録解除後も
DB（`banto-hub.sqlite3`）・データディレクトリ・サービスログファイルは
削除されません（必要ならファイル自体を手動で削除してください）。

### ファイアウォール

サービスとして常駐する場合も、開放すべきポートは §2
「ファイアウォール開放」の内容がそのまま当てはまります（管理 UI/REST/
WebSocket の 8722 番、gRPC を有効化した場合は 50051 番）。コンソール
モードとサービスモードでリスニングするポート・プロトコルに違いは
ありません。

## 11. インストーラ

T5-2（docs/t5-handoff.md §3「インストーラ（既存2アプリのインストーラ
構成を踏襲）」）。banto-hub は Tauri アプリではない（`src-tauri` を
持たない、T0 決定）ため `cargo tauri build` は使えないが、既存2アプリ
（ChronoGazer / relay-wright）と同じ NSIS 形式のインストーラを、
`tauri-bundler` クレートを単体ライブラリとして呼び出す専用ツール
（`apps/banto-hub/installer/`）で生成する。本節はこのインストーラを
オーナーが実際にビルド・入手する手順と、インストール時の挙動を
まとめたものです。

### ビルド手順

```powershell
# 1. banto-hub.exe をリリースビルド（§1 と同じ、既存の標準手順）
pnpm --filter banto-hub build
cargo build -p banto-hub-core --bin banto-hub --features embed-ui --release

# 2. インストーラを生成（既定で target/release/banto-hub.exe を対象にする）
cargo run --manifest-path apps/banto-hub/installer/Cargo.toml --release
```

生成物は `target/release/bundle/nsis/BantoHub_<version>_x64-setup.exe`
です。`apps/banto-hub/installer/` はルートの `banto-industrial`
ワークスペースの member ではなく（独立した Cargo ワークスペース -
`apps/banto-hub/installer/Cargo.toml` のコメント参照）、
`cargo check --workspace --all-targets`（CI が ubuntu-latest 上で回す
コマンド）には一切含まれません。このインストーラのビルドは Windows
上でのパッケージング専用の作業であり、Windows 実機でのみ実行します。

初回ビルド時、NSIS ツールセットがローカルにキャッシュされていない場合は
自動的にダウンロードされます（インターネット接続が必要 -
`%LOCALAPPDATA%\tauri\NSIS` にキャッシュされ、2回目以降はオフラインで
ビルドできます）。

### インストール時の挙動

- **インストールモード**: 「全ユーザー（PerMachine、`C:\Program
Files\BantoHub\`）」固定です。ChronoGazer/relay-wright の既定
  （ユーザー単位インストールも選べる `Both` モード）とは異なり、
  banto-hub は Windows サービスとして常駐させる前提のアプリのため、
  インストーラ自体を管理者権限で実行する必要があります（UAC
  プロンプトが出ます）。
- **Windows サービスの自動登録**: インストール完了時（post-install
  フック）に `banto-hub.exe install`（§10 参照）が自動的に実行され、
  `BantoHub` サービスが登録されます。登録に失敗した場合（例:
  既に同名サービスが存在する等）もインストーラ自体は中断せず、
  進捗画面に案内メッセージを表示するだけに留まります - 失敗した場合は
  §10「サービスの登録（install）」の手順で手動登録してください。
  同様に、アンインストール開始時（pre-uninstall フック）には
  `banto-hub.exe uninstall` が自動的に実行され、サービス登録を解除します。
  サービスの**起動**（`Start-Service BantoHub`）はインストーラの範囲外
  です - §10「起動確認」の手順で別途行ってください（起動種別が
  「自動（遅延開始）」のため、OS 再起動でも自動的に立ち上がります）。
- **「インストール後に BantoHub を実行する」チェックボックス（既知の
  制約）**: tauri-bundler の NSIS テンプレートには、GUI アプリを前提と
  した「完了ページでアプリを起動する」チェックボックスが標準で
  含まれており、banto-hub 向けにこれを消す設定項目は tauri-bundler
  側に用意されていません（`NsisSettings` を調査済み - 完全に消すには
  テンプレート全体を独自の `.nsi` に差し替える必要があり、T5-2 の
  スコープでは見送った）。**このチェックボックスをオンのまま完了すると、
  `banto-hub.exe` がコンソール無しの前面プロセスとして直接起動します**
  （サービス経由ではない）。既にサービスが起動している状態でこれを行うと
  ポート（既定 8722）の二重 bind で失敗します。インストール完了画面では
  **このチェックボックスを外す**ことを推奨します。誤って起動してしまった
  場合は、そのプロセスを終了してから `Start-Service BantoHub`
  でサービス経由に切り替えてください。
- 環境変数（`PORT`/`BANTO_BIND`等）はサービス登録後のプロセスには
  引き継がれません（§10 に記載の制約と同じ）。固定したい場合は
  インストール前に OS のシステム環境変数として設定しておいてください。

### アンインストール

「アプリと機能」（Windows 設定）または `C:\Program
Files\BantoHub\uninstall.exe` からアンインストールできます。
前述のとおり、アンインストール開始時に自動的に `banto-hub.exe
uninstall`（サービス登録解除）が実行されます。DB
（`banto-hub.sqlite3`）・データディレクトリ・サービスログファイルは
削除されません（§10「アンインストール」と同じ - 必要ならファイル自体を
手動で削除してください）。

### 既知の制約（tauri-bundler 単体利用について）

- `apps/banto-hub/installer/` は完全な Tauri アプリ（`src-tauri`）を
  作らず、`tauri_bundler::{SettingsBuilder, bundle_project}` を直接
  呼び出す小さな Rust バイナリです。`tauri-bundler` クレートの安定な
  公開 API（`Settings`/`SettingsBuilder`/`BundleSettings`/
  `WindowsSettings`/`NsisSettings`/`PackageType`/`BundleBinary`/
  `bundle_project`）だけで完結しており、`tauri.conf.json` や
  `cargo tauri` CLI は一切経由しません。
- `WindowsSettings::webview_install_mode`/`NsisSettings::install_mode`
  の型（`WebviewInstallMode`/`NSISInstallerMode`）は `tauri-bundler`
  のクレートルートからは再エクスポートされていないため、`tauri-utils`
  （tauri-bundler 2.9.4 が実際に依存している 2.9.3 系）を直接の依存に
  追加する必要があった。
- NSIS の post-install/pre-uninstall フックへのカスタムスクリプト差し込み
  （`NsisSettings::installer_hooks`）は tauri-bundler が公式にサポートする
  拡張点で、`${MAINBINARYNAME}`/`$INSTDIR` 変数がその時点で参照できる
  （`apps/banto-hub/installer/hooks/service-hooks.nsh` 参照）。この機構の
  おかげで T5-1 の `install`/`uninstall` サブコマンドとの連携が実装できた
  （上記「Windows サービスの自動登録」）。
- 前述の「インストール後に実行」チェックボックスのように、GUI
  アプリ前提の挙動を完全には消せない拡張点も存在する（`installer_hooks`
  は4つの固定フックポイントのみで、任意の UI 変更はできない）。
