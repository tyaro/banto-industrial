# banto-scada 設計ドキュメント（草案）

作成日: 2026-09-30  
状態: **設計中（初版ドラフト）**

本書は banto-industrial のタグサーバー banto-hub をデータ境界として利用する
汎用 SCADA / HMI アプリケーション **banto-scada** の設計方針をまとめる。

先行実装として、特定工場向け read-only 監視 Web アプリ
tyaro/ems-apps で得た知見（背景画像＋オーバーレイ、設備詳細、MQTT ライブ値、
タグ探索ツリー、ドラッグ＆ドロップ割付、トレンド、アラーム表示、環境間設定移行）
を継承し、工場固有部分を一般化する。

---

## 0. 背景と目的

banto-hub には現在値・タグ catalog・書き込み・外部 DB Source/Sink 等のデータ面があり、
ChronoGazer には時系列収集・トレンド資産がある。

次段階として、以下を一つの SCADA 製品として提供する。

- プロセス画面
- Equipment / Symbol
- Faceplate
- Popup / Dialog
- Alarm
- Historical Trend
- 搬送 Tracking
- DB の Table / View 表示
- Recipe / 実績表示・操作
- Event / Action
- 外部 API 呼び出し
- 外部プログラム起動
- Project Editor
- Project Import / Export / Update
- AI / automation friendly Design API

単一案件にハードコードした HMI ではなく、
**Editor で定義した Project を Runtime が実行する汎用 SCADA**を目標とする。

---

## 1. 最上位設計原則

### 1.1 PLC is the Control Authority

PLC を設備制御の authoritative source とする。

以下は PC・Hub・SCADA・DB・上位 LAN が停止しても PLC 単独で継続できなければならない。

- 安全インターロック
- 非常停止関連
- 設備シーケンス
- 搬送シーケンス
- PLC 間ハンドシェイク
- 搬送 Tracking
- 設備保護に必要な Alarm 判定
- 現在運転中の Recipe

banto-hub / banto-scada は supervisory layer であり、
PC 側の停止が設備停止条件にならないことを原則とする。

~~~text
PLC = Control Authority
PC  = Supervisory / Monitoring / Operation Support
~~~

SCADA から PLC に操作要求を出す場合も、最終的な実行可否は PLC が判定する。
SCADA 側の permissive 表示は補助表示であり、安全・運転許可の authoritative 判定にはしない。

### 1.2 SCADA は PLC へ直接接続しない

原則:

~~~text
banto-scada
    |
    v
banto-tagclient / Hub API
    |
    v
banto-hub
    |
    v
PLC / DB / external systems
~~~

SCADA が SLMP / Modbus 等の PLC ドライバを直接所有しない。
PLC セッション、タグ解決、認証、監査、書き込み保護は Hub 側へ集約する。

### 1.3 Read と Action を分離する

表示 Binding と変更 Action を別概念にする。

- TagBinding: 値の表示
- AlarmBinding: Alarm 状態の表示
- TrackingBinding: 搬送物・位置の表示
- DataGrid binding: 表形式データの表示
- Action: PLC write / ACK / API / DB command / program launch 等

「画面に表示できる」ことと「変更できる」ことを同義にしない。

### 1.4 Runtime と Editor を分離する

Editor は Project Model を生成・編集するツールであり、
Runtime は Project Model を解釈して描画・動作する。

~~~text
Project Model
   |         \
   v          v
Renderer     Editor
   |
   v
Runtime
~~~

Editor が無くても手書き Project から Runtime が成立する境界を保つ。

---

## 2. 全体アーキテクチャ

~~~text
PLC / field devices
        |
        v
+----------------------------+
|         banto-hub          |
|                            |
| Tag Space                  |
| Historian                  |
| Alarm Engine               |
| Tracking ingest/state      |
| DB resources               |
| Write / Audit              |
| Server Actions             |
+-------------+--------------+
              |
      REST / WS / future IF
              |
              v
      banto-tagclient
              |
              v
+----------------------------+
|        banto-scada         |
|                            |
| Screen Runtime             |
| Alarm Viewer               |
| Trend Viewer               |
| Tracking Viewer            |
| DataGrid / Form            |
| Event / Action Engine      |
| Project Editor             |
+----------------------------+
~~~

---

## 3. v1 の対象と非対象

### 対象

- Hub 接続
- Stable Tag ID Binding
- Screen / Symbol
- EquipmentType / EquipmentInstance
- Faceplate
- Dialog / Popup
- SVG ベース Process Renderer
- Data Binding
- Event / Action
- Project 保存・読込
- Project Import / Export / update 判定
- 基本 Editor
- DataGrid
- Alarm 表示基盤
- Trend 表示基盤
- Design API（Project / Screen / Equipment / Binding / Validate / Plan / Apply）
- Design API の OpenAPI 公開

### 初版で限定する項目

- Renderer は SVG + Svelte を第一候補とする
- Editor の基本操作は select / move / resize / property / binding
- Animation は value / color / visibility / state を中心に開始
- DB Table/View は read-only 表示を先行
- Action の workflow は単純 sequence + success/failure から開始
- Recipe / Tracking / Alarm の高度機能は段階導入する

### 非対象

- PC が無いと設備が動かない制御ロジック
- SCADA 側での安全インターロック決定
- SCADA からの任意 SQL 実行
- Project に任意 shell command を埋め込む機構
- 初版からの汎用 PLC プログラミング環境
- AI による Runtime 常時監視・自律運転を必須機能とすること

---

## 4. SCADA Project Model

### 4.1 Project

~~~text
ScadaProject
|
+-- Manifest
+-- Screens
+-- Equipment Types
+-- Equipment Instances
+-- Symbols
+-- Faceplates
+-- Dialogs
+-- Bindings
+-- Events
+-- Actions
+-- Alarm presentation config
+-- Tracking presentation config
+-- DB resource references
+-- External connection references
+-- Assets
~~~

### 4.2 Stable ID

以下は名前とは別に stable ID を持つ。

- ScreenId
- SymbolId
- FaceplateId
- DialogId
- EquipmentTypeId
- EquipmentId
- ActionId
- ProjectId

Name は人間向け、ID は内部参照向けとする。

rename で参照が壊れないことを必須とする。

### 4.3 Project Manifest

最低限以下を持つ。

~~~text
product            = banto-scada
schemaVersion      = Project file format
projectId          = same project identity
projectRevision    = project update generation
minRuntimeVersion  = minimum compatible runtime
exportedAt
contentHash
~~~

用途:

- schemaVersion: migration 判定
- projectId: 同一案件 Project 判定
- projectRevision: 新旧判定
- minRuntimeVersion: Runtime 互換判定
- contentHash: 同 revision なのに中身が異なる競合検知

---

## 5. Project Import / Export

### 5.1 Package

単一 JSON ではなく、将来的には ZIP 系 package を第一候補とする。

拡張子案:

~~~text
plant-a.bantoscada
~~~

内部例:

~~~text
manifest.json
project.json

screens/
symbols/
faceplates/
dialogs/
equipment/
actions/
assets/

checksums.json
~~~

### 5.2 更新判定

同じ projectId なら projectRevision を比較する。

- incoming > local: newer
- incoming < local: older
- incoming == local && hash equal: same
- incoming == local && hash differs: conflict

### 5.3 Import Preview

適用前に差分を表示する。

例:

~~~text
Project revision 120 -> 127

Screens
  + 2
  ~ 4
  - 1

Symbols
  ~ MotorStandard

Faceplates
  + PIDLoop

Equipment
  + CV103
  ~ CV102
~~~

以下は security-sensitive change として強調する。

- PLC Write Action
- External Program
- HTTP endpoint
- DB command
- authentication type

### 5.4 Secret

Project package に秘密情報を含めない。

除外例:

- Hub API key
- HTTP bearer token
- Basic Auth password
- OAuth client secret
- DB password
- OS credential

Import 後に不足 secret を明示して設定させる。

### 5.5 Migration

~~~text
v1 project
   |
 migrate
   v
v2
   |
 migrate
   v
current model
~~~

読み込み時に in-memory migration -> validate -> diff -> apply の順とする。
保存時は current schema で書き出す。

---

## 6. Screen / Renderer

### 6.1 SVG + Svelte

Process 画面は SVG ベースを第一候補とする。

理由:

- DOM object として扱える
- selection / pointer event が扱いやすい
- transform / group / path / text が自然
- Editor と Runtime で同じ renderer を共有しやすい

大量描画等で必要性が出た場合のみ Canvas/WebGL を追加検討する。

### 6.2 ScreenObject

基本 Object 候補:

- Rectangle
- Ellipse
- Line
- Path
- Text
- Image
- ValueDisplay
- Button
- Group
- SymbolInstance

Motor / Valve / Pump は ObjectType へ直接増やさず Symbol と Equipment で表現する。

### 6.3 Dynamic Property

初期対応:

- Value
- Text
- Color
- Stroke
- Visibility
- State
- Position
- Rotation
- Scale
- Blink

---

## 7. Equipment / Symbol / Faceplate / Dialog

### 7.1 EquipmentType

~~~text
EquipmentType: Motor

Binding Slots
  running   bool    read
  fault     bool    read
  command   bool    read/write
  speed     number  read
  current   number  read

Default Symbol
Default Faceplate
~~~

### 7.2 EquipmentInstance

~~~text
Motor01
  type = Motor

bindings
  running -> StableTagId(...)
  fault   -> StableTagId(...)
  command -> StableTagId(...)
  speed   -> StableTagId(...)
~~~

Symbol と Faceplate に個別に tag を再設定せず、
EquipmentInstance の binding context を共有する。

### 7.3 Symbol

同一 EquipmentType の多数配置を再利用する。

~~~text
EquipmentType
    |
    +-- SymbolDefinition
    +-- FaceplateDefinition
             |
     EquipmentInstance
             |
         BindingSet
~~~

### 7.4 Faceplate

Equipment 操作・詳細表示に使う専用 View。

初期 presentation:

- floating
- modal

将来:

- anchored
- docked

Faceplate 内から以下を利用可能にする。

- equipment slot value
- Alarm
- Trend
- command action
- interlock/permissive 表示

### 7.5 Dialog / Popup

Equipment に限定しない汎用 View。

用途:

- 確認
- 数値入力
- Recipe 選択
- Alarm 詳細
- Tracking 詳細
- Trend 詳細
- 操作理由入力
- 外部システム結果表示

### 7.6 View Stack

~~~text
System Overlay
Modal Dialog Layer
Faceplate / Popup Layer
Process Screen
~~~

Alarm banner 等は System Overlay とする。

---

## 8. Editor UX

先行実装 ems-apps で有効だった以下の UX を継承する。

### 8.1 Inspector / Property Panel

選択中 object に応じて context-sensitive に表示する。

~~~text
Inspector

General
Transform
Appearance
Binding
Events
Actions
~~~

Editor panel は project data と分離した workspace state とする。

workspace state 例:

- Panel position
- Dock state
- Zoom
- Tag tree expansion
- Recently used tags
- Current selection

Project export には含めない。

### 8.2 Tag Browser

Hub catalog をそのまま階層表示する。

~~~text
Tags
  PLC01
    Fast
      Motor01.Run
      Motor01.Fault
      Motor01.Current
    Slow

  Computed
  Internal
  DB Values
  DB Tables
~~~

表示候補:

- quality
- data type
- unit
- writable
- source kind

### 8.3 Tag assignment

以下をすべて許容する。

1. Drag & Drop
2. Double Click
3. Search + selection
4. Expression editor への insertion

Equipment slot には型検査を行う。

例:

~~~text
running requires BOOL
Temperature is F32
=> assignment rejected
~~~

### 8.4 未割付

Editor 中は未割付を許容する。

Runtime display:

- unresolved -> -- / invalid presentation

Publish/Deploy 前の validation で warning/error をまとめて表示する。

---

## 9. Binding Engine

### 9.1 StableTagId

通常の画面 Binding は Hub の stable ID を保存する。

~~~text
connection_id
group_id
tag_id
~~~

外部名は表示・再解決支援用 metadata とする。

### 9.2 Cross-environment import

開発 Hub -> 現場 Hub では stable ID が一致しない場合がある。

Project には fallback hint を保持してよい。

例:

~~~text
stableTagId
hint:
  externalName
  dataType
  unit
~~~

自動で別タグへ勝手に binding せず、
未解決 -> candidate -> user confirmation とする。

### 9.3 Expression

Binding を直接 property へつなぐだけでなく、

~~~text
Tag(s)
  |
Expression
  |
Property
~~~

を許容する。

可能であれば既存 banto-expr を利用する。

---

## 10. Event Engine / Action Engine

### 10.1 Event

候補:

- on_click
- on_double_click
- on_screen_open
- on_screen_close
- on_value_changed
- on_rising_edge
- on_falling_edge
- on_alarm_activated
- on_alarm_cleared
- on_tracking_enter
- on_tracking_leave
- timer

### 10.2 Action

候補:

- Navigate
- OpenFaceplate
- OpenDialog
- CloseView
- WriteTag
- Toggle
- AcknowledgeAlarm
- ShelveAlarm
- OpenTrend
- HttpRequest
- LaunchProgram
- ExecuteDbCommand

### 10.3 execution target

~~~text
Local
  Navigate
  Dialog
  Faceplate
  LaunchProgram

Hub/Server
  PLC Write
  HTTP webhook
  DB command
  alarm/tracking triggered action
~~~

設備イベントに依存する常時実行 Action は SCADA Runtime ではなく Hub 側を優先する。
SCADA を閉じたことで通知・記録が消える設計にしない。

### 10.4 External Program

任意 shell 文字列を Project へ保存しない。

管理者が許可済み program definition を登録し、Project は ID を参照する。

~~~text
ExternalProgramDefinition
  id
  executable
  allowed args
  working directory
  single instance
  timeout
~~~

shell.exe /c 相当を原則使用せず、実行ファイルを直接起動する。

### 10.5 HTTP

Project へ認証 secret / raw connection string を保存しない。

~~~text
HttpConnection
  id
  base URL
  auth type
  secret reference
  timeout
~~~

Action は connection ID と relative path を参照する。

### 10.6 Action flow

初版は複雑な node workflow にしない。

~~~text
ActionStep
  action
  on_success
  on_failure
~~~

程度から開始する。

### 10.7 暴走防止

Event に以下を考慮する。

- debounce
- minimum interval
- rate limit
- timeout
- concurrency limit
- duplicate suppression
- retry policy

PLC write や non-idempotent POST は暗黙自動 retry しない。

---

## 11. Alarm

### 11.1 Alarm は tag kind ではない

Alarm を tag_kind = alarm として本体化しない。

~~~text
Tag / PLC alarm source
       |
       v
   Alarm Engine
       |
       +-- AlarmDefinition
       +-- AlarmState
       +-- AlarmEvent
       +-- AlarmHistory
~~~

必要に応じて read-only alarm state tag を projection として公開してもよいが、
Alarm entity の authoritative source にはしない。

### 11.2 Alarm Source

2 系統を正式に扱う。

~~~text
AlarmSource
  +-- Hub evaluated
  |      tag/expression -> alarm
  |
  +-- PLC generated
         PLC alarm state/event -> Hub
~~~

### 11.3 PLC-generated alarm

重要設備 Alarm は PLC 側で判定する。

PLC 側に Alarm bitmap / state を持ち、
Hub はそれを ingest する。

三菱向けでは、SLMP Monitor の Entry/Execute Monitor 機能を
高速 snapshot 取得に利用する候補とする。

ただし monitor は push ではないため、
短時間 ON/OFF の完全保証にはしない。

### 11.4 Fast notification + recovery

高信頼用途では以下を分離する。

~~~text
PLC
 |
 +-- UDP / event notification ---- low latency
 |
 +-- SLMP Monitor ---------------- current state sync
 |
 +-- event FIFO + sequence ------- loss recovery
             |
             v
        Alarm Ingest
~~~

UDP は低レイテンシ用途であり唯一の真実にしない。

### 11.5 banto-alarm

Alarm state machine は独立 crate 候補:

~~~text
crates/banto-alarm
~~~

当面は banto-hub process 内で動かす。
別 process 化は durable event transport が必要になってから検討する。

状態候補:

- Normal
- Pending
- ActiveUnacked
- ActiveAcked
- ReturnedUnacked
- Shelved

Alarm Definition 候補:

- source/expression
- severity
- deadband
- on delay
- off delay
- latch
- enabled
- message

設定 UI は単純条件から始めても、内部 model は expression 拡張を阻害しない。

---

## 12. Tracking

搬送系では Tracking を独立 domain とする。

~~~text
Tags       = current values
History    = time series
Alarm      = abnormal states
Tracking   = identity / location / movement
~~~

### 12.1 PLC authoritative

Tracking ID・現在位置・次行先・PLC 間 transfer 状態は PLC が保持する。

SCADA / Hub が停止しても搬送継続できること。

### 12.2 Logical Location

物理座標ではなく論理 Location を基本とする。

例:

~~~text
CV01.IN
CV01.ZONE01
CV01.ZONE02
CV01.OUT
STATION10
LIFTER01
~~~

SCADA は Logical Location を画面座標へ mapping する。

### 12.3 Tracking Unit

~~~text
TrackingUnit
  tracking_id
  carrier_type
  current_location
  state
~~~

Hub 側には必要に応じて詳細 metadata を付加する。

- barcode
- product
- lot
- recipe
- route
- order
- inspection result

PLC には運転に必要な最小情報だけを持たせる。

### 12.4 PLC 間 handoff

Hub を transfer handshake の成立条件にしない。

PLC-A / PLC-B 間で ownership transfer を成立させる。

状態例:

- Owned
- Offered
- Accepted
- Committed

### 12.5 Snapshot + Event FIFO

Alarm と同様に、

~~~text
Current tracking table
+
Event sequence / FIFO
~~~

を組み合わせる。

Hub は source run id + source sequence 等で重複排除・欠番検出できる構造を検討する。

### 12.6 banto-tracking

独立 crate 候補:

~~~text
crates/banto-tracking
~~~

Tracking anomaly は banto-alarm へ接続する。

例:

- duplicate tracking ID
- lost tracking
- impossible transition
- destination occupied
- transfer timeout
- route mismatch

---

## 13. Historian / Trend

既存資産を利用する。

~~~text
banto-tstore
    |
banto-tsquery
    |
banto-hub History API
    |
banto-tagclient
    |
banto-scada Trend
~~~

SCADA が tstore file を直接読む方式にはしない。

これにより Hub PC と SCADA PC が分離していても Historical Trend を利用できる。

---

## 14. DB Resources

### 14.1 原則

SCADA Runtime は DB へ直接接続しない。

~~~text
SCADA
  |
Hub API
  |
DB
~~~

DB credential / SQL / DB driver を SCADA Project に持ち込まない。

### 14.2 既存 DB scalar tag

現行 Hub の DB Source は、

~~~text
PostgreSQL query
  -> result column
  -> db scalar tag
  -> current value
~~~

として利用する。

これは単一現在値に向いている。

### 14.3 DB Table / View

SCADA で表・View を表示する用途には tabular resource を追加する。

UI 上は「DB タグ」として統合してもよいが、
実装では scalar/current-value と rowset を分離する。

概念例:

~~~text
DB Resource
  +-- DB Value Tag
  |     scalar/current value
  |
  +-- DB Table Tag
        rows x columns
~~~

tag kind 候補:

- db
- db_table

capability 例:

~~~text
PLC tag
  current_value = true
  history       = true

DB scalar tag
  current_value = true

DB table tag
  tabular       = true
  current_value = false
~~~

db_table は通常の values WS/MQTT stream へ流さない。

### 14.4 Table / View / Query

source 候補:

- table
- view
- registered query

実案件では View を推奨する。

DB 内部 schema と SCADA の契約境界を View に置くことで、
内部テーブル変更の影響を局所化できる。

### 14.5 API

例:

~~~text
GET /api/v1/db-resources
GET /api/v1/db-resources/{id}
GET /api/v1/db-resources/{id}/schema
GET /api/v1/db-resources/{id}/rows
~~~

rows では pagination / sort / allowed filter を提供する。

SCADA から任意 SQL を送る API は作らない。

### 14.6 DataGrid

SCADA の一級 widget とする。

~~~text
DataGrid
  source
  columns
  refresh
  page size
  sort
  row events
~~~

row context を Dialog / Action へ渡せるようにする。

例:

~~~text
row.order_id
row.product
row.status
~~~

---

## 15. Recipe / 実績

DB Resource を Recipe / Production Result に活用する。

### 15.1 Recipe

~~~text
DB Recipe Table/View
       |
      Hub
       |
   SCADA selection
       |
  Recipe Download Action
       |
      PLC
~~~

PLC に転送後は Hub/SCADA が停止しても現在 Recipe で運転継続できること。

Recipe に revision を持たせ、
PLC 側にも active_recipe_id / active_recipe_revision を保持できる構造を推奨する。

### 15.2 Production Result

表示は DB table/view resource を利用できる。

更新・登録は任意 SQL ではなく、登録済み DB Command / Server Action を使用する。

~~~text
ExecuteDbCommand
  command = complete-production
  typed params
~~~

内部実装は parameterized SQL / stored procedure 等を許容する。

### 15.3 Tracking との統合

Tracking ID を中心に、

- Production order
- Recipe
- Process history
- Inspection result
- Final result

を関連付けられるようにする。

---

## 16. Security / Audit

以下を共通の Operator Action Audit 対象とする。

- PLC write
- Alarm ACK / Shelve
- Recipe download
- DB command
- HTTP action
- External program launch
- Project import/update

Audit 候補:

- timestamp
- user
- screen
- object
- action id
- target
- result
- duration
- tracking id
- equipment id
- detail

---

## 17. ems-apps から継承する知見

先行実装 tyaro/ems-apps で有効だったもの:

- 背景画像＋相対座標 overlay
- x/y を 0..1 で保持
- 設備オブジェクトから詳細画面への遷移
- MQTT live update
- topic expression
- Tag/Topic tree
- tree expansion state の保持
- search
- drag & drop assignment
- double click assignment
- draggable property panels
- context-sensitive property editing
- unresolved binding を許容
- Trend group
- Alarm viewer
- environment-specific secret を除外した portable settings

banto-scada では以下を一般化する。

~~~text
MQTT topic             -> Hub StableTagId
TopEquipmentObject     -> EquipmentInstance / SymbolInstance
Equipment screen       -> Faceplate / Screen
Topic expression       -> Binding Expression / banto-expr
portable-settings      -> Project Package
direct DB trend        -> Hub History API
alarm table viewer     -> Hub Alarm API
~~~

---

## 18. Runtime / Editor 構成

初期は一つの製品 executable でもよい。

~~~text
banto-scada
  Runtime mode
  Editor mode
~~~

内部 module/package は分離する。

候補:

~~~text
scada-model
scada-renderer
scada-editor
scada-runtime
~~~

将来、要求が出た場合に、

~~~text
banto-scada-runtime
banto-scada-studio
~~~

へ製品分離できるようにする。

---

## 19. AI / Design API

### 19.1 AI の利用範囲

banto-scada での AI 利用は **設計時を主対象**とする。

主な用途:

- Screen の作成・変更
- Symbol / Faceplate / Dialog の生成
- EquipmentInstance の作成
- Hub tag catalog を使った Binding 候補提示・割付
- Event / Action の設定
- Project validation の修正支援
- 既存画面・Equipment の複製、連番展開
- Project 差分の説明
- Import / migration の補助

Runtime で AI が常時設備を監視・自律操作することは本設計の必須要件にしない。
運転データの取得・PLC write・Alarm・Tracking 等は既存の Hub API / Hub MCP の責務とする。

~~~text
               AI / automation
                  /       \
                 /         \
       SCADA Design API     Hub API / MCP
       project / screen     tags / values
       equipment / binding  alarm / tracking
       validate / plan      DB resources
~~~

### 19.2 API-first

AI 連携の本体を MCP に置かない。

**Design API を一次契約**とし、以下が同じ domain service を利用する。

~~~text
                 SCADA Design Domain
                 /       |        \
                /        |         \
          Editor UI     REST       CLI
                         |
                         +-- AI Agent
                         +-- optional MCP adapter
~~~

目的:

- Editor と AI で validation / mutation 規則を二重実装しない
- Project schema の内部表現を外部ツールへ直接露出しすぎない
- AI 以外の自動生成ツール・CLI からも利用可能にする
- MCP が不要な環境でも同一機能を利用できる
- 将来 MCP を追加しても薄い adapter で済む

### 19.3 Design API の公開範囲

候補:

~~~text
Project
  GET  /api/design/v1/project
  POST /api/design/v1/validate

Screen
  GET  /api/design/v1/screens
  POST /api/design/v1/screens
  GET  /api/design/v1/screens/{id}
  PUT  /api/design/v1/screens/{id}

Object
  POST /api/design/v1/screens/{id}/objects
  PUT  /api/design/v1/objects/{id}
  DELETE /api/design/v1/objects/{id}

Equipment
  GET  /api/design/v1/equipment-types
  GET  /api/design/v1/equipment
  POST /api/design/v1/equipment
  PUT  /api/design/v1/equipment/{id}/bindings

Library
  GET  /api/design/v1/symbols
  GET  /api/design/v1/faceplates
  GET  /api/design/v1/dialogs

Binding
  PUT  /api/design/v1/bindings/{id}
  POST /api/design/v1/bindings/expression/check

Action
  GET  /api/design/v1/actions
  POST /api/design/v1/actions

Change set
  POST /api/design/v1/changes/plan
  POST /api/design/v1/changes/apply
~~~

上記は方向性であり、URL・粒度は実装時に確定する。

### 19.4 Semantic API

AI 向けに、

~~~text
update_json(path="screens[0].objects[17]...")
~~~

のような raw JSON path mutation を主 API にしない。

代わりに、

- create_screen
- add_object
- move_object
- create_equipment
- bind_equipment_slot
- set_property_binding
- create_faceplate
- set_event_action

等の SCADA domain operation を提供する。

Project file の内部 schema と外部操作契約を分離し、
schema migration が Design API consumer を不必要に壊さない構造とする。

### 19.5 OpenAPI

Design API は OpenAPI document を生成・公開する。

AI Agent は OpenAPI から API 契約を取得できるため、
**MCP が無くても機械操作可能**であることを設計目標とする。

OpenAPI には少なくとも以下を明示する。

- request / response schema
- stable error code
- project revision conflict
- validation result
- security-sensitive change classification
- capability / enum

### 19.6 Validate

`validate_project` 相当の機能を Editor / API / AI で共有する。

例:

~~~text
errors:
  - Motor01.running requires bool, bound tag is f32
  - Screen Line01 references deleted faceplate

warnings:
  - Motor03.command is unassigned
  - Dialog RecipeSelect references unavailable DB resource
~~~

validation 対象例:

- object reference
- EquipmentType / slot type
- StableTagId resolution
- expression type
- writable requirement
- Faceplate / Dialog reference
- Action target
- DB Resource reference
- Recipe / Tracking reference
- unresolved secret requirement

AI の典型フローを以下とする。

~~~text
inspect
  |
plan/edit
  |
validate
  |
fix
  |
validate
~~~

### 19.7 Plan / Apply

AI による大規模変更は、原則として plan -> review -> apply を利用できるようにする。

例:

~~~text
+ Screen Line02
+ Motor x12
+ Valve x6
+ 54 bindings
~ Navigation
+ 3 write actions
~~~

既存 Project Import の diff engine と共通化できる部分は共通化する。

以下を security-sensitive change として plan 上で強調する。

- writable tag binding
- PLC Write Action
- DB Command
- HTTP endpoint / method
- External Program
- authentication / secret requirement

### 19.8 Optimistic Concurrency

Design API mutation は projectRevision を利用する。

~~~text
read:
  projectRevision = 127

apply:
  expectedRevision = 127
~~~

人間または別 Agent が先に編集して currentRevision = 128 になっていれば、
古い revision に基づく apply を拒否する。

stable error 例:

~~~text
project_revision_conflict
~~~

これにより AI と人間の同時編集による silent overwrite を防ぐ。

### 19.9 Editor との同期

Design API から Project が変更された場合、Editor は変更を検知して最新 revision を読み直せること。

方式候補:

- local event
- WebSocket / SSE
- watch channel

API mutation と Editor 内 mutation は同じ Project service / revision 管理を通す。
Editor が開いている Project file を AI がファイルシステム経由で直接書き換える運用を標準経路にしない。

### 19.10 Binding Assistance

AI は Hub catalog と EquipmentType の slot metadata を利用して Binding 候補を提示できる。

~~~text
EquipmentType: Motor

running:
  type: bool
  description: motor running feedback

fault:
  type: bool
  description: motor fault status

command:
  type: bool
  writable: true
  description: motor start command
~~~

候補例:

~~~text
Motor01.running
  -> PLC01.Fast.Motor01_Run

Motor01.fault
  -> PLC01.Fast.Motor01_Fault
~~~

型、unit、writable、名前、description 等を候補評価に利用できる。

**writable slot の Binding は自動確定より明示的な確認を優先**する。

### 19.11 MCP の位置づけ

SCADA 専用 MCP は初期必須要件にしない。

必要性が出た場合のみ、

~~~text
MCP Adapter
    |
    v
Design API / Design Domain
~~~

という薄い adapter として追加する。

MCP 固有の validation / mutation / permission logic は作らない。

MCP の利点は主に以下に限定する。

- tools/list による tool discovery
- MCP 対応 AI client からの接続容易性
- tool schema の標準化

これらが OpenAPI / Agent tool integration で十分なら MCP は実装しなくてよい。

### 19.12 Runtime との境界

SCADA Design API は設計時機能であり、Runtime の物理操作経路を増やすものではない。

AI が運転データや PLC 操作を必要とする場合は、

~~~text
AI
 |
Hub API / Hub MCP
 |
Hub permission / audit / write guard
 |
PLC
~~~

を利用する。

SCADA Design API に PLC write の runtime bypass を作らない。

### 19.13 公開範囲とセキュリティ

Design API は Editor mode でのみ有効にすることを基本とし、
初期実装では loopback bind を第一候補とする。

Runtime-only deployment では Design API を無効化できる構造とする。

Project 設計変更は operator runtime audit と区別し、
必要に応じて design change history として以下を記録する。

- actor / client
- before revision
- after revision
- change summary
- timestamp

---

## 20. Repository / CI

SCADA の repository 分割は現時点では確定しない。

まず banto-industrial Issue #468 の path-aware CI を導入し、

- Hub change -> Hub CI
- ChronoGazer change -> ChronoGazer CI
- SCADA change -> SCADA CI
- docs-only -> minimum CI

を成立させる。

その後、

- independent release cycle
- concurrent development conflicts
- Issue/PR ownership
- cross-repository atomic changes

を基準に repository 分割を再評価する。

---

## 21. 実装ロードマップ案

### S0 Architecture

- 本書を設計の基準文書として確定
- Project schema の最小型を定義
- Hub に必要な追加 API を洗い出す
- Design API の最小契約を確定

### S1 scada-model

- Project
- Screen
- Object
- Equipment
- Symbol
- Faceplate
- Dialog
- Binding
- Event / Action
- schema migration

### S2 Renderer

- 手書き Project JSON から Process 画面を表示
- SVG renderer
- basic objects
- dynamic property

### S3 Hub Live Binding

- banto-tagclient 接続
- StableTagId binding
- quality / unresolved
- reconnect / rebinding

### S4 Project Package

- projectId / revision
- import / export
- migration
- validation
- diff preview
- backup / rollback

### S5 Design API

- Project inspect
- semantic mutation
- OpenAPI
- validate
- plan / apply
- projectRevision optimistic concurrency
- Editor change notification
- loopback/editor-mode security boundary

### S6 Editor Core

- select
- move
- resize
- zoom
- inspector
- undo / redo
- Design API / Project service との共通 mutation 経路

### S7 Tag Browser / Binding UX

- Hub catalog tree
- search
- drag/drop
- double click
- type validation
- expression insertion
- AI binding assistance 用 metadata

### S8 Equipment / Symbol / Faceplate

- EquipmentType
- EquipmentInstance
- Symbol reuse
- Faceplate / Dialog
- View Stack

### S9 Action Engine

- Navigate
- Dialog
- PLC Write
- external API
- external program
- audit

### S10 Alarm

- banto-alarm
- Alarm API
- Alarm Viewer
- ACK / Shelve
- PLC generated alarm input
- SLMP monitor / event recovery 設計

### S11 Tracking

- banto-tracking
- logical location
- current tracking
- event FIFO/sequence
- SCADA tracking presentation

### S12 History

- Hub History API
- banto-tagclient history extension
- Trend widget

### S13 DB Resources

- DB table/view registration
- schema/rows API
- DataGrid
- row context
- pagination

### S14 Recipe / Production Result

- Recipe selection/display
- PLC download action
- revision confirmation
- DB commands
- production result viewer

### Optional: MCP Adapter

Design API / OpenAPI だけでは AI client 統合が不十分という実要件が出た場合にのみ、
SCADA MCP adapter を追加する。
MCP 自体は roadmap の blocking milestone にしない。

---

## 22. 未決事項

以下は実装前に個別決定する。

1. banto-scada を banto-industrial 内に置くか別 repository にするか
2. Project package の正式拡張子
3. Stable ID の UUID/ULID 方式
4. Screen coordinate の内部単位（normalized / logical pixel の併用方針）
5. banto-expr を client/runtime でそのまま利用するか
6. DB tabular resource の正式名称（db_table / dataset 等）
7. Alarm PLC event FIFO の標準 PLC memory layout
8. Tracking PLC block の標準 memory layout
9. Server Action を Hub 本体に置く範囲
10. Editor/Runtime の executable 分離時期
11. Design API の最終 transport / bind policy（初期候補: editor mode + loopback REST）
12. AI change plan の承認を必須にする変更範囲

---

## 23. 現時点の主要決定

- PLC は control authority。PC 停止で設備制御を止めない
- SCADA は PLC に直接接続せず Hub を介する
- Screen model / Renderer / Editor を分離する
- SVG + Svelte を Process Renderer の第一候補とする
- EquipmentType / EquipmentInstance を持つ
- Symbol と Faceplate は同じ Equipment binding context を共有する
- Faceplate / Dialog / Popup を Project model の一級要素とする
- Binding / Event / Action Engine を Runtime の中心に置く
- External API / External Program を Action として扱う
- Alarm は tag kind ではなく独立 domain
- PLC generated alarm と Hub evaluated alarm の両方を扱う
- 搬送 Tracking は独立 domain。PLC authoritative
- Historian は Hub History API 経由
- DB Table/View は Hub 登録 resource として SCADA へ公開する
- scalar DB tag と tabular DB resource は実行経路を分離する
- Recipe / 実績は DB Resource + Action/Command を再利用する
- Project Import/Export は projectId / projectRevision / schemaVersion を持つ
- Project package へ secret を含めない
- ems-apps の Editor UX を先行実装の知見として継承する
- AI 利用は設計時を主対象とし、Runtime AI を必須要件にしない
- AI 連携は MCP-first ではなく Design API-first とする
- Editor / CLI / AI は同じ Design Domain / mutation / validation を共有する
- Design API は OpenAPI を公開し、AI が MCP 無しでも操作できることを目標とする
- AI の Project 変更は projectRevision による optimistic concurrency を使う
- 大規模・security-sensitive な AI 変更は plan / diff / validate を経て apply できる構造とする
- SCADA 専用 MCP は optional adapter とし、必要性が出るまで実装を必須にしない
- SCADA Design API に PLC runtime write の bypass を作らない
