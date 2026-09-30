# banto-scada 設計ドキュメント（草案）

作成日: 2026-09-30  
最終更新: 2026-09-30（DB Table/View を Dataset で表現する案 B に決定（§14.2）。repo の置き場所を banto-industrial 内に決定（§20）。Replay を将来機能（有料版候補）として §13.3 に追加し、v1 に残す前提条件を列挙。冗長化の方針を §13.2 に追記: PLC / Hub / SCADA server の 3 層で独立、読み取り・評価は全台、副作用は 1 台、調停は PLC 調停を第一候補。詳細は別草案。v1 の範囲を決定（§3）: 画面とライブ値・操作まで、scada-server 系は v1.1。SCADA server の構成を決定（§13.2）: 24/365 の処理は UI と別のライブラリ core に置き、v1 はアプリ埋め込み、後から Windows サービス host を足す。§10.3 / §11.6 / §22 の #7 #15 #17 #18 を決定済みに。同日: Historian は ChronoGazer と共有（§13）、Binding identity は名前のみ（§9.6）、Hub データ型対応（§8.3）ほか）  
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

```text
PLC = Control Authority
PC  = Supervisory / Monitoring / Operation Support
```

SCADA から PLC に操作要求を出す場合も、最終的な実行可否は PLC が判定する。
SCADA 側の permissive 表示は補助表示であり、安全・運転許可の authoritative 判定にはしない。

### 1.2 SCADA は PLC へ直接接続しない

原則:

```text
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
```

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

```text
Project Model
   |         \
   v          v
Renderer     Editor
   |
   v
Runtime
```

Editor が無くても手書き Project から Runtime が成立する境界を保つ。

---

## 2. 全体アーキテクチャ

SCADA は Hub を PLC / DB 等への接続境界として利用する。
ただし Alarm / Tracking / automation 等の追加 domain を
**すべて banto-hub 本体へ実装することは現時点では決定しない**。

```text
PLC / field devices
        |
        v
+----------------------------+
|         banto-hub          |
|                            |
| Tag Space / Catalog        |
| PLC Read / Write           |
| Auth / Write Audit         |
| existing DB source/sink IF |
+-------------+--------------+
              |
      REST / WS / future IF
              |
              v
      Hub client layer
              |
              v
+----------------------------+
|        banto-scada         |
|                            |
| Screen Runtime             |
| Trend / Alarm UI           |
| Tracking UI                |
| DataGrid / Form            |
| UI Event / Action          |
| Project Editor             |
+----------------------------+

Optional / future domains
  Alarm domain
  Tracking domain
  History（ChronoGazer と共有する記録・トレンド資産。Hub 外、§13）
  DB tabular resource
  always-on Event / Action
       |
       +-- Alarm / History / always-on Event / Action の host は SCADA server core
       |   （§13.2、2026-09-30 決定）。Hub には置かない
       +-- DB tabular resource は Hub 側（§14。Hub API → DB を接続境界とする）
       +-- Tracking domain の host は未決（§12。PLC authoritative。ingest を Hub 側に
           置くか SCADA server に置くかは後続）
```

Hub を integration boundary として使うことと、
全 domain の実行主体を Hub process に集約することは同義ではない。
追加機能は独立 crate / service を選択できる境界を維持する。

---

## 3. v1 スコープ（2026-09-30 オーナー決定）

**v1 は「画面を作り、Hub の値を安全に表示し、基本操作できる」縦切り**とする。Hub と組み合わせれば
v1 だけで運転画面として価値が出ること、記録は当面 ChronoGazer が担えることが理由。
scada-server core（§13.2）は**ライブラリ境界と起動モードの分岐だけを v1 に含め**、中身の recorder と
Alarm は v1.1 に置く。

### Core v1

- Hub 接続
- `external_name` Binding
- Screen / Symbol
- EquipmentType / EquipmentInstance
- Faceplate
- Dialog / Popup
- SVG ベース Process Renderer
- Data Binding
- 基本 UI Event / Action
- Project 保存・読込
- Project Import / Export / update 判定
- 基本 Editor
- Tag Browser / Binding UX
- validation / diagnostics
- scada-server core のライブラリ境界と起動モードの分岐（§13.2。ローカルモードの埋め込み起動と
  共有サービス接続モードの区別。中身は空でよい）

### v1.1（scada-server 系）

- Historical Trend（recorder、履歴 API、Trend widget。§13）
- Alarm（汎用 model、Alarm API、Alarm Viewer、ACK / Shelve。§11）
- 常時実行 Event / Action（§10.3）
- Windows サービス host（§13.2 host 3）

### 後続 Extension

全体設計には含めるが、v1 / v1.1 には含めない。

- DataGrid / DB Table/View（§14。Hub 側 Dataset API の設計後）
- Tracking
- Recipe / 実績
- HTTP Action / External Program
- Design API の REST / OpenAPI 露出（Editor / CLI / AI が共有する Design Domain 自体（§19.2）は Core に含める）
- AI 設計支援
- 高度な Event / Action flow
- Replay（§13.3。有料版の機能候補。v1 に残すのは Project schema と Runtime の境界だけで、recorder 側の実装は v1.1）

### 初版で限定する項目

- Renderer は SVG + Svelte を第一候補とする
- Editor の基本操作は select / move / resize / property / binding
- Animation は value / color / visibility / state を中心に開始
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

```text
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
```

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

```text
product            = banto-scada
schemaVersion      = Project file format
projectId          = same project identity
projectRevision    = project update generation
minRuntimeVersion  = minimum compatible runtime
exportedAt
contentHash
```

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

```text
plant-a.bantoscada
```

内部例:

```text
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
```

### 5.2 更新判定

同じ projectId なら projectRevision を比較する。

- incoming > local: newer
- incoming < local: older
- incoming == local && hash equal: same
- incoming == local && hash differs: conflict

### 5.3 Import Preview

適用前に差分を表示する。

例:

```text
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
```

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

Runtime PC 側の保管先は、banto-hub-bootstrap が使う OS キーリング方式
（banto-hub-client-bootstrap.md）の再利用を第一候補とする。§10.5 の `secret reference` はこの保管先の
キーを指す。

### 5.5 Migration

```text
v1 project
   |
 migrate
   v
v2
   |
 migrate
   v
current model
```

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

```text
EquipmentType: Motor

Binding Slots
  running   bool    read
  fault     bool    read
  command   bool    read/write
  speed     number  read
  current   number  read

Default Symbol
Default Faceplate
```

### 7.2 EquipmentInstance

```text
Motor01
  type = Motor

bindings
  running -> PLC01.Fast.Motor01.Run
  fault   -> PLC01.Fast.Motor01.Fault
  command -> PLC01.Fast.Motor01.Command
  speed   -> PLC01.Slow.Motor01.Speed
```

Symbol と Faceplate に個別に tag を再設定せず、
EquipmentInstance の binding context を共有する。

### 7.3 Symbol

同一 EquipmentType の多数配置を再利用する。

```text
EquipmentType
    |
    +-- SymbolDefinition
    +-- FaceplateDefinition
             |
     EquipmentInstance
             |
         BindingSet
```

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

```text
System Overlay
Modal Dialog Layer
Faceplate / Popup Layer
Process Screen
```

Alarm banner 等は System Overlay とする。

---

## 8. Editor UX

先行実装 ems-apps で有効だった以下の UX を継承する。

### 8.1 Inspector / Property Panel

選択中 object に応じて context-sensitive に表示する。

```text
Inspector

General
Transform
Appearance
Binding
Events
Actions
```

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

```text
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

Datasets（Tags と別系統。§14.2）
  table / view / registered query
```

`Datasets` は Tag ではなく独立した resource（§14.2、案 B）なので、Tag Browser では Tags と別の
最上位ノードに出す。DataGrid の source と Recipe / 実績（§15）の参照先になる。

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

```text
running requires BOOL
Temperature is F32
=> assignment rejected
```

Slot 型は SCADA 側の抽象型であり、Hub の `data_type` へ次のように対応させる。
Hub に `bool` 型は無く、ビットは `bit` である（`banto-tags` の `ALLOWED_DATA_TYPES`:
`bit` / `i16` / `u16` / `i32` / `u32` / `f32` / `i64` / `u64` / `f64` / `string`）。

| Slot 型  | Hub `data_type`                                               | 備考                                                                   |
| -------- | ------------------------------------------------------------- | ---------------------------------------------------------------------- |
| `bool`   | `bit`                                                         | T20 のワードデバイスのビット指定（`.0`〜`.F`）も `bit` として扱う      |
| `number` | `i16` / `u16` / `i32` / `u32` / `f32` / `i64` / `u64` / `f64` | 64bit 整数は Hub 内部で `f64` 搬送のため 2^53 超で精度が落ちる（§6.3） |
| `string` | `string`                                                      | 現在値経路は T20 の read-on-demand（WS/MQTT には流れない）             |

### 8.4 未割付・不成立 Binding の安全な扱い

Editor 中は未割付を許容する。
Project に未解決 Binding や型不整合が残っていても、Runtime 全体を起動不能にはしない。

表示系は **fail-safe / degraded presentation** とする。

例:

- unresolved tag -> `--` / unresolved 表示
- Bad / Stale -> quality を反映した表示
- 型不整合 -> invalid 表示
- Expression 評価不能 -> error/invalid 表示
- 一部 Object の不成立が他の正常 Object の描画を止めない

validation は Editor / Import / Design API から常時利用でき、
warning/error をまとめて確認できるようにする。ただし「全項目が valid でなければ Runtime を
動かせない」という hard publish gate は初期設計では採用しない。

Action は対象 Binding・引数・権限等が成立しない場合に**実行しない**。
失敗は利用者へ表示するとともに Action execution record / audit に残す。

最低限の記録候補:

- timestamp
- screen / object / action
- requested target
- failure code
- failure detail
- user / actor（取得可能な場合）

「表示の設定ミス」と「操作が実行された」を曖昧にしない。

---

## 9. Binding Engine

### 9.1 永続 Binding ID は `external_name`

SCADA Project の永続 Binding は Hub の公開外部名を保存する。

```text
{connection}.{group}.{tag}

例:
PLC01.Fast.Motor01.Run
```

この `external_name` は Hub が外部 API / write scope / catalog で利用する公開アドレスであり、
SCADA Project と Hub の間の Binding 契約として扱う。

SCADA Project は Hub 内部 DB の数値 ID
（`connection_id` / `group_id` / `tag_id`）を永続 Binding ID として保存しない。

### 9.2 Runtime 解決

Runtime は Project に保存された `external_name` を Hub catalog へ照合し、
表示・書き込みの検証に使う metadata（data type / writable / unit）と catalog revision を得る。
購読・書き込みも `external_name` で行う。Hub の公開契約（REST `POST /api/v1/values/{tag}`、
WS subscribe、write scope、MQTT トピック）はすべて名前であり、SCADA は Hub 内部の数値 ID
（`StableTagId`）を使わない（§9.6、2026-09-30 オーナー決定）。

```text
Project
  external_name
       |
       v
Hub catalog（revision 付き）
       |
       +-- data type
       +-- writable
       +-- unit
       |
       v
Runtime binding cache（名前 → metadata、解決時の revision）
```

これにより Hub の config export/import で内部数値 ID が変わっても、
また Hub 側でタグを削除して同名で作り直しても、`connection.group.tag` が同じなら
SCADA Project の Binding は維持できる。

### 9.3 Rename 契約

Hub 上で `connection` / `group` / `tag` の名前を直接変更し、
`external_name` が変わった場合は **SCADA Binding が unresolved になってよい**。
外部名の変更を透過的に追従することは要件にしない。

SCADA Editor から名称変更する場合は、単なる文字列編集ではなく
**意味のある Design operation** として扱う。

例:

```text
rename_tag

1. 現在の Hub catalog / Project revision を確認
2. Hub API で対象名を変更
3. SCADA Project 内の該当参照を更新
4. Expression / Action 等の構造化参照を更新
5. validate
6. Project revision を進める
```

途中失敗時に Hub と Project のどちらが更新済みかを明示し、
silent mismatch を作らない。可能なら plan / apply と validation を利用する。

Hub 側の名称変更は構成 API（T21 の `admin` スコープ）を要する。Runtime が使う `read` /
`write:{pattern}` キーとは別に、**Editor 専用の admin キー**を持つ前提とし、その保管場所は
§19.13 の Design API の境界と合わせて決める。Runtime-only deployment には admin キーを配置しない。

Expression 中のタグ参照は単純な文字列置換ではなく、
parser / structured reference を介して更新することを優先する。

### 9.4 Cross-environment import

開発 Hub -> 現場 Hub への移送では `external_name` を再解決キーとする。

```text
開発 Hub:
  PLC01.Fast.Motor01.Run

現場 Hub:
  PLC01.Fast.Motor01.Run
```

であれば内部数値 ID が異なっても Binding は成立する。

現場 Hub に同じ `external_name` が存在しない場合は unresolved とし、
自動で類似名の別タグへ Binding しない。

Project は validation 補助として期待型・writable 等の metadata を保持してよいが、
それらを別タグへの自動再割付キーにはしない。

### 9.5 Expression

Binding を直接 property へつなぐだけでなく、

```text
Tag(s)
  |
Expression
  |
Property
```

を許容する。

可能であれば既存 banto-expr を利用する。

### 9.6 Binding identity の決定（2026-09-30 オーナー決定: 名前のみ。書き込みも名前）

**決定**: SCADA の Binding の同一性は、購読も書き込みも Hub の `external_name` とし、
Hub 内部の数値 ID（`StableTagId`）は SCADA では使わない。

経緯: 本 PR 当初は「Project は `external_name`、Runtime / SDK 内部は `StableTagId`」としていたが、
関連文書と揃っていなかった（tag-server-design.md §4.1 は外部名 + 安定 ID の併記、
banto-tagclient-design.md §3.1 / §4.4 は stable ID のみ）。レビューで次の 4 案を比較した。

#### 検討した案

| 案                                       | 内容                   | 判断                                                                        |
| ---------------------------------------- | ---------------------- | --------------------------------------------------------------------------- |
| A. `external_name` のみ                  | 購読・書き込みとも名前 | **採用**                                                                    |
| B. `StableTagId` のみ                    | SDK の現行契約         | 環境をまたぐと id が変わる。Project が人間・AI に読めない                   |
| C. Project は名前、環境別キャッシュに id | 改名候補の提示ができる | 案 A と Project 形式は同じ。必要が出たら後付けできる拡張として保留          |
| D. Hub にタグ UUID を追加                | 環境をまたぐ同一性     | Hub のマイグレーション・CSV 互換（#264）・config package に波及する。将来案 |

#### ID を使わない理由（オーナー判断）

- **ID に束縛が引っ張られると変更しにくい。** 削除して同名で作り直す、CSV を再取り込みする、接続を作り直す、
  といった試運転中の操作で ID は変わる。名前が同じでも ID 束縛は切れる。ChronoGazer の Hub ドライバは
  この回避策（`apps/chronogazer/core/src/hub.rs` の fingerprint）を既に抱えている
- **環境をまたぐと ID は持ち越せない。** Hub の config package は名前で往復する
- **Hub の公開契約はすべて名前。** REST の書き込み先、WS の subscribe、write scope のパターン、MQTT トピック、
  Expression、Design API、AI の割付提案。SCADA が名前だけに依存すれば Hub の内部表現に結合しない
- **改名は破壊的変更として利用者責任**（tag-server-design.md §4.1、2026-08-05）と整合する。改名で
  Binding が unresolved になるのは仕様
- ID が持つ利点（改名への追従、改名と削除の区別、旧名を引き継いだ別タグへの誤書き込み防止）は
  以上の不利益に対して小さい。最後の 1 点は下記のとおり方針として受容する

#### 名前で書き込む場合の安全規則

- Runtime は書き込み前に、現在の catalog revision で対象名が解決済みであり、`writable` かつ型が一致する
  ことを確認する。未解決、または `config_changed` 受信後の再解決が済んでいない間は書かない（fail-closed）
- Hub は存在しない名前に 404、writable でない・scope 外に 403 を返す。いずれも失敗として表示・記録する
  （§8.4、§16）
- 「改名の後に旧名で別タグが登録された」場合、書き込みはその別タグに届く。これは **SCADA / SDK では塞がず、
  方針として受容する**（2026-09-30 オーナー判断）。名前を契約にした以上、この状況は意図的な「削除→同名再作成」
  と区別できず、区別すべきでもない。起きるには Hub 側で改名と同名の再登録を行い、さらにその新タグを
  `writable` にし、型も一致させる必要がある。守りは既存機構に置く: Hub のタグごとの `writable` opt-in
  （新規タグは既定で書けない）、上記の書き込み前の型・writable 照合と再解決中は書かない規則、Hub の改名時の
  警告、Hub の write 監査と SCADA の execution record（§16）による事後追跡。write 要求に期待 catalog
  revision を付けて Hub が照合する拡張は**採らない**
- 自動再割付はしない（§9.4）

#### SDK への影響

banto-tagclient は `BindingRequest` と `write_tag` が `StableTagId` を要求している。
この決定に合わせて **SDK の binding 同一性も `external_name` に改める**（2026-09-30 オーナー指示）。
Hub との wire は既に名前（WS subscribe、`POST /api/v1/values/{tag}`）なので、変わるのは SDK の公開型と
呼び出し側（ChronoGazer の Hub ドライバ、banto-hub-sink）である。banto-tagclient-design.md §3.1 / §4.4 の
改訂と実装は別 PR で行う。

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

### 10.3 execution target（2026-09-30 オーナー決定）

UI に閉じた Action は SCADA Runtime で実行する。

```text
Local / UI
  Navigate
  Dialog
  Faceplate
  LaunchProgram
  operator initiated WriteTag
```

次のような「UI が閉じていても常時成立させたい Event / Action」は
**SCADA server core（§13.2）が担う**。Hub には置かない。

- value edge 起点の処理
- alarm 起点の外部通知
- tracking 起点の処理
- timer
- server-side HTTP / DB command

検討した候補と判断:

1. banto-hub に Server Action/Event 機能を持たせる → 採らない（Hub の責務を増やす）
2. SCADA Runtime（UI プロセス）を常駐実行主体として扱う → 採らない（寿命が UI と同じ）
3. 独立した automation / event service を設ける → **採用**。SCADA server core として、recorder・Alarm engine と
   同じライブラリに置き、host は埋め込み / headless / Windows サービスの 3 通り（§13.2）
4. 対象機能ごとの既存 domain service に委ねる → 採らない（分散すると運用が増える）

**Hub に余計な責務を増やさないことを重要な評価軸**とし、
「常時実行だから Hub に入れる」とは自動的に決めない。

先例として、2026-09-06 の DB Sink 決定（banto-hub-external-db-design.md §2.1 / §5）がある。
24/365 で動くエンジンは Hub の運転基盤（トレイ・SCM サービス・状態画面・pending queue）に載せるが、
重いエンジン本体は **Hub が設定と監視を持つ別プロセスのサイドカー**とした。候補 3 をこの形で
定義すれば「Hub に責務を増やさない」軸と両立する。候補 2（SCADA Runtime 常駐）は、同決定で
退けられた「寿命が UI と同じ」問題を再び踏む。

Core v1 の Event は click / double click / screen open/close 等の UI Event を中心に扱い、
常時実行 Event / Action は v1.1（§3、2026-09-30 決定）。

### 10.4 External Program

任意 shell 文字列を Project へ保存しない。

管理者が許可済み program definition を登録し、Project は ID を参照する。

```text
ExternalProgramDefinition
  id
  executable
  allowed args
  working directory
  single instance
  timeout
```

shell.exe /c 相当を原則使用せず、実行ファイルを直接起動する。

### 10.5 HTTP

Project へ認証 secret / raw connection string を保存しない。

```text
HttpConnection
  id
  base URL
  auth type
  secret reference
  timeout
```

Action は connection ID と relative path を参照する。

### 10.6 Action flow

初版は複雑な node workflow にしない。

```text
ActionStep
  action
  on_success
  on_failure
```

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

### 11.1 方針: まず汎用 Alarm model を作る

初版では MELSEC / SLMP 等の特定 PLC・プロトコルに依存しない
**汎用 Alarm domain / state machine / API / UI** を先に設計する。

Alarm を `tag_kind = alarm` として本体化しない。

```text
Alarm Source
    |
    v
Alarm Engine
    |
    +-- AlarmDefinition
    +-- AlarmState
    +-- AlarmEvent
    +-- AlarmHistory
```

必要に応じて read-only alarm state tag を projection として公開してもよいが、
Alarm entity の authoritative source にはしない。

### 11.2 Generic Alarm Source

Alarm Engine は source の取り込み方式を protocol 固有にしない。

候補:

```text
AlarmSource
  +-- expression / tag condition
  +-- external discrete state
  +-- external event
  +-- future protocol adapter
```

初期実装は Hub が既に取得できる Tag / Expression を source とする方式から開始できる。

例:

- `Tank.Level >= 90`
- `Motor.Command && !Motor.Feedback`
- bool tag の rising / falling state

条件式の評価は Hub の computed tag（banto-expr、tag-server-design.md §4.2）に登録し、Alarm Engine は
その `bit` を購読するだけにすると、式評価器を二重に持たずに済む。初期実装の最小形として検討する。

将来 PLC 側で生成済みの Alarm state/event を取り込む場合も、
Alarm Engine 本体へ MELSEC 固有情報を持ち込まず source adapter で接続する。

### 11.3 State model

状態候補:

- Normal
- Pending
- ActiveUnacked
- ActiveAcked
- ReturnedUnacked
- Shelved

Alarm Definition 候補:

- source / expression
- severity
- message
- deadband
- on delay
- off delay
- latch
- enabled

設定 UI は単純条件から開始してよいが、内部 model は expression / external source 拡張を阻害しない。

### 11.4 ACK / Shelve

ACK / Shelve は汎用 Alarm Engine の operator state として設計する。

初版では特定 PLC の ACK bit / reset handshake と直接結合しない。
PLC 側 ACK が必要な設備では、将来 protocol/source adapter または明示 Action として
接続できるよう境界を残す。

### 11.5 Protocol-specific PLC Alarm は後続

MELSEC / PX Developer を意識した以下は有力な将来案だが、初期 Alarm 実装の必須要件にしない。

- SLMP Monitor Entry / Execute Monitor
- Alarm bitmap
- UDP event notification
- PLC event FIFO / sequence
- loss recovery
- MELSEC 固有 ACK / reset handshake

将来のイメージ:

```text
MELSEC PLC
  +-- UDP event -------- low latency
  +-- SLMP monitor ----- current state
  +-- FIFO / sequence -- loss recovery
             |
             v
      MELSEC Alarm Adapter
             |
             v
      Generic Alarm Engine
```

汎用 Alarm Engine を先に固定し、MELSEC adapter はその契約へ後付けする。

### 11.6 実装境界

Alarm state machine は独立 crate 候補:

```text
crates/banto-alarm
```

host は **SCADA server core（§13.2、2026-09-30 決定）**。Alarm domain の汎用性（crate）と
host process の選択（§13.2 の 3 通り）は分離したままにする。

---

## 12. Tracking

搬送系では Tracking を独立 domain とする。

```text
Tags       = current values
History    = time series
Alarm      = abnormal states
Tracking   = identity / location / movement
```

### 12.1 PLC authoritative

Tracking ID・現在位置・次行先・PLC 間 transfer 状態は PLC が保持する。

SCADA / Hub が停止しても搬送継続できること。

### 12.2 Logical Location

物理座標ではなく論理 Location を基本とする。

例:

```text
CV01.IN
CV01.ZONE01
CV01.ZONE02
CV01.OUT
STATION10
LIFTER01
```

SCADA は Logical Location を画面座標へ mapping する。

### 12.3 Tracking Unit

```text
TrackingUnit
  tracking_id
  carrier_type
  current_location
  state
```

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

```text
Current tracking table
+
Event sequence / FIFO
```

を組み合わせる。

Hub は source run id + source sequence 等で重複排除・欠番検出できる構造を検討する。

### 12.6 banto-tracking

独立 crate 候補:

```text
crates/banto-tracking
```

Tracking anomaly は banto-alarm へ接続する。

例:

- duplicate tracking ID
- lost tracking
- impossible transition
- destination occupied
- transfer timeout
- route mismatch

---

## 13. Historian / Trend と SCADA server（2026-09-30 オーナー決定: ChronoGazer と共有）

ChronoGazer は記録計の単体商品として残す。banto-scada はトレンド機能を含む製品なので、
記録・トレンドの資産の大部分を ChronoGazer と共有する。

**Hub に History API は足さない。** tag-server-design.md §2 の「Hub は履歴を持たず読み返さない」
（2026-08-04、2026-09-06 の部分撤回は Sink のみ）を維持し、Hub の tstore（§3.3、既定 7 日の
バックフィル用）も SCADA からは読まない方針で開始する。

```text
banto-hub（現在値 / catalog）
      |
      v  Hub 経由購読ドライバ（ChronoGazer #383 段階1 と共有）
   recorder（記録経路）
      |
      v
  banto-tstore（日次ファイル）
      |
      v
  banto-tsquery
      |
      v
  Trend UI（ChronoGazer と共有）
```

共有する資産:

- Hub 経由購読ドライバ（banto-tagclient の世代管理、未解決タグの扱い）
- banto-tstore / banto-tsquery
- トレンド UI（LineChart ストリーミング、`read_decimated` の初期窓 → append、
  null を 0 と区別する規律。r1-plan.md R1-D）

SCADA 固有:

- Trend widget の Project model（trend group を Screen object / Faceplate から参照する）
- trend group を Project package に含める
- Faceplate / Dialog からの Trend 呼出

これにより Hub PC と SCADA PC が分離していても、SCADA 側の recorder が Hub を購読して
Historical Trend を成立させる。

### 13.1 この方針の妥当性（2026-09-30 レビュー）

方向性は妥当と判断する。根拠:

- Hub を履歴非保持のまま保てる（2026-08-04 決定と整合）。保持期間・間引き・容量の方針を Hub に持ち込まない
- ChronoGazer の core crate（`apps/chronogazer/core`）は tauri 非依存で、headless の `banto-serve`
  バイナリと REST 層、Hub 経由購読ドライバ（#383 段階1）を既に持つ。「共有」は新規の切り出しではなく
  既存の構造をそのまま使える
- 商品の線引きが明快。ChronoGazer = 記録計単体、SCADA = 画面 + 記録計 core の同梱

ただし次を満たさないと「寿命が UI と同じ」問題（2026-09-06 の Sink 決定で退けた構造）を SCADA に持ち込む。
成立のさせ方は §13.2 で決めた。

1. **記録は UI の中身と混ぜない。** recorder は SCADA server core（§13.2）に置き、SCADA の画面は
   その API を読む。画面が tstore ファイルを直接読む方式にはしない。v1 の埋め込み起動（§13.2 host 1）は
   同じプロセスに同居するが、ライブラリ境界は守る
2. **現場の recorder は論理サービスとして 1 つ。** 操作卓と事務所閲覧など SCADA client が複数ある現場で
   client ごとに記録すると、履歴が重複し欠測もばらつく。「1 つ」はプロセス数の制限ではなく**論理サービスの
   単位**で、将来の冗長化（複数インスタンス）は許容する（§13.2）。この現場は Windows サービス host
   （§13.2 host 3）を使い、client は共有サービス接続モードで接続する。既存の ChronoGazer をそのまま
   recorder に流用することは要件にしない（共有は crate 単位）
3. **履歴読み出し API は SCADA server core に置く。** 実装は ChronoGazer core の REST（I4 `read_decimated`、
   R1-D 未着手）と共有 crate 化して二重実装を避ける。「History API は要る。ただし所有者は Hub ではなく
   recorder」が正確な言い方になる
4. **記録対象タグの所有者は SCADA Project。** server core は配布された Project の trend group を読んで購読する。
   Editor から recorder へ selected-tags を同期する仕組みは作らない（§13.2）
5. **キーの整合。** Hub 経由ドライバは Hub の `external_name` でタグを引く（`core/src/hub.rs`）ので、
   tstore の `tag_key` もこれに揃え、SCADA Binding（§9）と同じ語彙で履歴を引けるようにする。§9.6 の決定どおり
   履歴のキーも名前とする

未決（§22 へ）:

- 共有 UI の切り出し方。pnpm workspace は現在 `apps/*` のみで共有 package が無い
- Hub の tstore を SCADA が読む場面を作るか（読まない方針で開始）

### 13.2 SCADA server（常駐処理）の構成（2026-09-30 オーナー決定）

**決定**: 24/365 で動かしたい処理（recorder、Alarm engine、常時実行 Event / Action、履歴・Alarm の
読み出し API）は、SCADA の画面とは別の **SCADA server core** に置く。Hub には足さない。
host は banto-hub と同じ 3 層とし、**v1 はアプリへの埋め込み起動で作り、Windows サービス host は後から足す**。

```text
                banto-hub（現在値 / catalog / write）
                   ^                        ^
      ライブ値・書き込み                    購読（recorder / Alarm / Event）
                   |                        |
  +----------------+-------+     +----------+------------------------+
  |  banto-scada 画面      |     |  scada-server core（ライブラリ）    |
  |  Runtime / Editor      |<--->|  recorder（tstore / tsquery）       |
  |  画面・操作・Project    | 履歴 |  banto-alarm                       |
  +------------------------+ Alarm|  常時実行 Event / Action            |
                            設定  |  履歴 / Alarm / 状態 API            |
                                 +-----------------------------------+
                                     host は 3 通り（下記）
```

banto-hub の型（`HubRuntime` をライブラリとし、コンソール / サービス / デスクトップシェルの 3 つの host
から呼ぶ。banto-hub-t16-design.md、banto-hub-t17-design.md）をそのまま使う。

- **ライブラリ `scada-server` core**: Tauri にも axum の起動方法にも依存しない。設定・Project・記録ファイルは
  サービスの実行ユーザーからも読める場所・権限に置く（Hub の profile / port lock の規約を流用）
- **host 1 = SCADA アプリ本体への埋め込み（v1）**: `banto-scada.exe` を起動すると、サービスが見つからなければ
  プロセス内で core を起動してそのまま画面を出す。トレイと状態画面もここで持つ。詳しくない人は
  「アプリを起動すれば全部動く」で済む
- **host 2 = headless コンソール**: ChronoGazer の `banto-serve` 相当
- **host 3 = Windows サービス（v1.1。§3）**: T17 の SCM 登録と `banto-hub-elev` の型を流用した薄い wrapper。
  アプリはサービスを検出したら自分では起動せず、そこへ接続する（T16-2 と同じ判定）。複数 client の現場、
  アプリを閉じても記録を続けたい現場向け

規律:

- core を画面プロセスの中身と混ぜない。画面 → core の呼び出しは可、逆は不可
- アプリの起動モードを**起動前に**区別し、埋め込み起動の責任を分ける:
  - **ローカルモード**: ローカルの SCM の SCADA server サービスが Running かつ healthy のときだけ接続し、
    Stopped / NotInstalled なら埋め込み起動、遷移中や判定できないときは安全側（埋め込み起動しない）
    に倒す（T16-2 の判定をそのまま流用）。v1 時点ではサービスが無いので常に埋め込み起動になるが、この分岐が
    あればサービス化のときにアプリ側の変更がほぼ要らない
  - **共有サービス接続モード**: 設定で指定した別 PC の SCADA server に接続する閲覧・操作端末。
    **このモードではローカルサービス未検出や接続失敗を埋め込み起動の条件にしない**。接続失敗は
    「未接続」として表示して再試行する。協調しない別系統の recorder / Alarm / 常時実行 Event が端末ごとに
    暗黙に起動し、履歴や ACK 状態が分裂したり外部処理が重複したりすることを防ぐため
- **論理サービスとインスタンスを分ける**: SCADA server は論理サービスの識別子（現場 / Project 単位）と
  インスタンスの識別子（プロセス / ホスト単位）を別に持つ。将来の冗長化（複数インスタンス）はこの前提で
  後続設計する。recorder の並行記録と履歴統合、Alarm の ACK / Shelve 状態の共有、Action の二重実行防止は
  それぞれ別の契約として §22 に残す。方式は次項のとおり**読み取りと評価は Active/Active** と決め、
  未決なのは failover、状態複製、トポロジの詳細に絞る
- **冗長化の方針（2026-09-30 オーナー方針、詳細は別設計）**: 冗長化は **PLC（ドライバ層）、Hub、SCADA server
  の 3 層で独立に扱う**。PLC の冗長系（系 A / 系 B）は `banto-plc` / `banto-collect` の接続層が複数
  endpoint と切替で隠し、Hub から見える接続とタグの外部名は変えない。Hub と SCADA server の冗長化は
  一つの設計草案（`docs/banto-hub-redundancy-design.md`、未作成）に起こし、原則は両者とも
  **「読み取りと評価は全台、副作用は 1 台」**とする。recorder と Alarm 評価は台数無制限の Active/Active
  （履歴は読み出し時に統合、Alarm は操作者状態のイベントだけ複製）、常時実行 Action・MQTT publish・
  Sink の DB 書き込みのような副作用は**リースの抽象**で 1 台に限定し、リースの実装は後で選ぶ。
  3 台以上の分断対策の調停は Hub 同士の過半数（Raft 系）ではなく **PLC 調停**（PLC 内のハートビートと
  担当のワード。PLC が control authority である原則と整合し、冗長 PLC ではトラッキングで引き継がれる）を
  第一候補とする。今日決めた名前束縛（§9.6）は、client が別インスタンスへ再接続して名前で再バインドできる
  という点で、この冗長化の前提になっている。現場の可用性は Hub で頭打ちになるため、SCADA server の
  冗長化だけを先行させない
- v1 の埋め込み起動は「寿命が UI と同じ」を割り切りとして受容し、文書と UI で案内する。記録を止めたくない
  現場はサービス化する

これで決まること:

- §22 #7 常時実行 Event / Action の実行主体 = scada-server core（§10.3）
- §22 #15 記録プロセスの寿命 = v1 は UI 同居（割り切り）、サービス host で分離
- §22 #17 recorder の配布形態 = SCADA 同梱（core）。ChronoGazer との共有は crate 単位、プロセスは別
- §22 #18 記録対象タグの所有者 = SCADA Project（§13.1 の 4）
- §11.6 Alarm engine の host = scada-server core

### 13.3 Replay（将来機能、有料版の候補。2026-09-30 オーナー方針）

**狙い**: Alarm の前後や任意の時刻について、設備の状態をトレンドの線ではなく**運転画面そのもので再現して
再生する**。他の SCADA との差別化機能と位置づけ、将来機能（有料版の候補）とする。v1 / v1.1 には含めないが、
後から足せるように前提条件だけを v1 に残す。

**方式: Replay ドライバ**。画面・Binding Engine・Equipment・Faceplate は値の出所を知らないまま動かし、
Hub の代わりに **Replay ドライバが tstore / tsquery から時刻カーソルに従って値を流す**。SCADA 側の変更は
再生の操作 UI と安全の規律に絞る。

- **A. プロセス内の driver（先行）**: banto-tagclient が公開する状態ストリーム（`TagClientState` の watch。
  catalog revision、current、品質）と同じ形を履歴から生成する実装を scada-server core に置き、Runtime は
  ライブ client と Replay driver のどちらの watch でも受け取れるようにする。SDK 側で状態の構築子を公開する
  小さな変更が要る。時刻カーソル、再生速度、コマ送りは driver の API
- **B. 仮想 Hub（必要が出たら）**: SCADA server が Hub と同じ REST catalog と WS values を過去の時刻で提供し、
  tagclient の接続先を切り替えるだけにする。他の client でも再生できるが、publish gate が要求する catalog
  revision と values の整合を過去時点で再現する必要があり、書き込みは 403 で拒否する
- Alarm の発生・復帰・ACK（§11）と execution record（§16）も同じ driver の時刻カーソルで切り出して流し、
  Alarm banner や Faceplate の Alarm 表示も出所を知らずに再現する

**v1 に残す前提条件**（Project schema と Runtime の拡張可能な境界だけ。recorder 側の実装は v1.1、
Replay 本体は将来）:

1. **Project schema に「記録対象の範囲」の設定を持てること。** `trend group のみ` / `Project の全 Binding` を
   選べる項目を Project model に置く（v1 は schema のみで、既定は trend group）。Replay には画面・
   Equipment・Faceplate が束縛する全タグの履歴が要るため。この設定を読んで記録する recorder、および
   その時点の catalog のメタデータ（型、単位、writable）と Project revision を記録に添える処理は
   **v1.1 の recorder**（§13.1、S12）に置く
2. **Project package を revision ごとに保管できる形。** §5 の projectRevision と package 形式を v1 で確定する。
   当時の画面で再生するための revision 別の保管と参照は v1.1 以降
3. **rename の名前履歴。** 履歴のキーは名前なので rename で履歴が分かれる。§9.3 の rename を Design operation に
   した決定を活かし、Project に「旧名 → 新名、有効時刻」の名前履歴を持てる schema を v1 で置く
4. **Runtime が値の出所を差し替えられる境界。** ライブ client と Replay driver を同じ受け口で受ける
   （v1 で境界だけ置く。driver 本体は将来）

**SCADA 側に残る作業**（Replay 固有、画面の上に乗る薄い層）: タイムラインのスクラバ、再生 / 一時停止 / 速度 /
コマ送り、再生中の表示（枠の色と時刻）、**副作用のある Action の全面無効化**（§1.3 の延長）、Alarm Viewer の
「この Alarm の前後を再生」とトレンドの時刻選択からの入口、再生位置（画面 + 時刻）を共有できる URL。

**有料版の候補としての切り方**: Replay driver と再生 UI は独立した module / crate に置き、ライセンスで有効化
できる形にする。記録側の前提条件 1〜3 はライセンスに関係なく v1 の基盤に含める（後から有効化したときに
過去へ遡って再生できるかは、記録対象の設定次第）。

---

## 14. DB Table / View 連携（2026-09-30 オーナー決定: Dataset / DB Resource）

### 14.1 決定済みの境界

SCADA Runtime は DB へ直接接続せず、DB credential / SQL / DB driver を
SCADA Project に持ち込まない。

```text
SCADA
  |
Hub API
  |
DB
```

Hub を DB 接続境界として利用する方針は維持する。

既存 Hub の DB Source は、

```text
PostgreSQL query
  -> result column
  -> db scalar tag
  -> current value
```

として単一現在値用途に利用できる。

### 14.2 tabular data は Dataset / DB Resource（案 B）で表現する

Table / View の rows × columns を SCADA の DataGrid に提供する。**Hub 内の domain model は
Tag とは別の first-class resource「Dataset」とする**（2026-09-30 オーナー決定、案 B）。
案 A（DB Table Tag）は採らない。理由は案 A の懸念に列挙したとおりで、Hub の非 PLC 値の経路
（`ServerTagStore` の `Option<f64>`、250ms poll）に表形式は乗らず、「Tag で統一」は Editor 上の
見え方だけになるため、Tag の current-value semantics を守って責務を分ける。

置き場所と形:

- Hub 側に置き、既存の DB 接続（banto-hub-external-db-design.md の DB connection）を Source / Sink と共有する
- read-only の API から始める。候補: `GET /api/v1/datasets`、`GET /api/v1/datasets/{id}/schema`、
  `GET /api/v1/datasets/{id}/rows`（pagination / sort / allowed filter）。具体形は Hub 側の設計で確定する
- 登録できる source は table / view / registered query。任意 SQL は受けない（§14.3）。実案件では
  View を推奨し、DB 内部 schema と SCADA の契約境界を View に置く
- Recipe / 実績（§15）が使う登録済み DB Command も同じ resource 系統に置く（write 側は別 slice）
- SCADA の Editor の Resource Browser では Tags と別の Datasets ノードに出す（§8.2）。Project は
  Dataset を名前で参照する（Tag と同じく数値 id を持ち越さない）

検討した案（記録）:

#### 案A: DB Table Tag

既存 Tag Browser / DB connection の概念へ寄せる。

```text
DB Value Tag  -> scalar
DB Table Tag  -> rows x columns
```

利点:

- 利用者から見て「Hub に登録した DB データ」で統一できる
- Editor の Resource Browser に自然に統合しやすい

懸念:

- current value / quality / WS/MQTT を前提とする Tag domain に tabular semantics が混ざる
- 各層で `tabular` 特例が増える可能性がある
- Hub の非 PLC 値は `ServerTagStore` の `Option<f64>` のみで、購読は 250ms poll の `read_current`、
  グループ周期は CHECK の固定集合（banto-hub-external-db-design.md §3）。tabular は既存の値経路に
  一切乗らないため、「統一」は UI 上の見え方だけになり、実装は案 B と同じく別経路が要る
- 案 A を採っても値経路・API は新設になる点で案 B との実装コスト差は小さい

#### 案B: Dataset / DB Resource

Tag とは別の first-class resource とする。

```text
Tags
  PLC / computed / internal / db scalar

Datasets
  table / view / registered query
```

利点:

- rows / columns / pagination / sort / filter の責務が明確
- Tag の current-value semantics を壊さない

懸念:

- Hub API / client / Editor Browser に新しい resource 系統が増える
- Recipe / Result 等との naming / ownership を整理する必要がある

### 14.3 必要な capability

SCADA から任意 SQL を送る API は作らない。

必要な機能候補:

- schema / column metadata
- rows
- pagination
- sort
- allowed filter
- timeout / cancellation
- query concurrency limit
- read-only first

API の具体形は Hub 側の設計（banto-hub-external-db-design.md への追補、または新規の設計文書）で確定する。

### 14.4 DataGrid

SCADA 側には一級 widget として DataGrid を持つ案を維持する。

```text
DataGrid
  source
  columns
  refresh
  page size
  sort
  row events
```

row context を Dialog / Action へ渡せるようにする。

例:

```text
row.order_id
row.product
row.status
```

---

## 15. Recipe / 実績

DB Resource を Recipe / Production Result に活用する。

### 15.1 Recipe

```text
DB Recipe Table/View
       |
      Hub
       |
   SCADA selection
       |
  Recipe Download Action
       |
      PLC
```

PLC に転送後は Hub/SCADA が停止しても現在 Recipe で運転継続できること。

Recipe に revision を持たせ、
PLC 側にも active_recipe_id / active_recipe_revision を保持できる構造を推奨する。

### 15.2 Production Result

表示は DB table/view resource を利用できる。

更新・登録は任意 SQL ではなく、登録済み DB Command / Server Action を使用する。

```text
ExecuteDbCommand
  command = complete-production
  typed params
```

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

Action は成功時だけでなく、**実行前検証で拒否した失敗も記録対象**とする。
「要求されたが安全に実行しなかった」ことを追跡できること。

PLC write の監査は Hub 側（`write_audit`、log-before-write）にあるが、実行前検証で拒否した失敗は
Hub に届かない。したがって SCADA 側に独自の **execution record store** を持ち、Hub の write 監査と
突合できるよう write 要求に相関 ID を渡せることを Hub 側の追加候補とする（§21 S0 の
「Hub に必要な追加 API」に含める）。

以下を共通の Operator Action Audit / execution record 対象とする。

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
- result / failure code
- failure detail
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

```text
MQTT topic             -> Hub external_name binding
TopEquipmentObject     -> EquipmentInstance / SymbolInstance
Equipment screen       -> Faceplate / Screen
Topic expression       -> Binding Expression / banto-expr
portable-settings      -> Project Package
direct DB trend        -> 共有 tstore / tsquery + Trend UI（§13）
alarm table viewer     -> SCADA server Alarm API（§13.2）
```

---

## 18. Runtime / Editor 構成

初期は一つの製品 executable でもよい。

```text
banto-scada
  Runtime mode
  Editor mode
```

内部 module/package は分離する。

候補:

```text
scada-model
scada-renderer
scada-editor
scada-runtime
scada-server（§13.2。recorder / alarm / 常時実行 Event / API。画面に依存しないライブラリ）
```

将来、要求が出た場合に、

```text
banto-scada-runtime
banto-scada-studio
```

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
運転データの取得・PLC write・DB resource は既存の Hub API / Hub MCP の責務、
Alarm・履歴・常時実行 Event / Action は SCADA server API（§13.2）の責務とする。
Tracking は §12 の host 決定に従う。

```text
                    AI / automation
                /          |           \
   SCADA Design API   Hub API / MCP   SCADA server API
   project / screen   tags / values   alarm / history
   equipment/binding  PLC write       always-on event
   validate / plan    DB resources    (§13.2)
```

### 19.2 API-first

AI 連携の本体を MCP に置かない。

**Design API を一次契約**とし、以下が同じ domain service を利用する。

```text
                 SCADA Design Domain
                 /       |        \
                /        |         \
          Editor UI     REST       CLI
                         |
                         +-- AI Agent
                         +-- optional MCP adapter
```

目的:

- Editor と AI で validation / mutation 規則を二重実装しない
- Project schema の内部表現を外部ツールへ直接露出しすぎない
- AI 以外の自動生成ツール・CLI からも利用可能にする
- MCP が不要な環境でも同一機能を利用できる
- 将来 MCP を追加しても薄い adapter で済む

### 19.3 Design API の公開範囲

候補:

```text
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
```

上記は方向性であり、URL・粒度は実装時に確定する。

### 19.4 Semantic API

AI 向けに、

```text
update_json(path="screens[0].objects[17]...")
```

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

```text
errors:
  - Motor01.running requires bool, bound tag is f32
  - Screen Line01 references deleted faceplate

warnings:
  - Motor03.command is unassigned
  - Dialog RecipeSelect references unavailable DB resource
```

validation 対象例:

- object reference
- EquipmentType / slot type
- external_name resolution
- expression type
- writable requirement
- Faceplate / Dialog reference
- Action target
- DB Resource reference
- Recipe / Tracking reference
- unresolved secret requirement

AI の典型フローを以下とする。

```text
inspect
  |
plan/edit
  |
validate
  |
fix
  |
validate
```

### 19.7 Plan / Apply

AI による大規模変更は、原則として plan -> review -> apply を利用できるようにする。

例:

```text
+ Screen Line02
+ Motor x12
+ Valve x6
+ 54 bindings
~ Navigation
+ 3 write actions
```

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

```text
read:
  projectRevision = 127

apply:
  expectedRevision = 127
```

人間または別 Agent が先に編集して currentRevision = 128 になっていれば、
古い revision に基づく apply を拒否する。

stable error 例:

```text
project_revision_conflict
```

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

```text
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
```

候補例:

```text
Motor01.running
  -> PLC01.Fast.Motor01_Run

Motor01.fault
  -> PLC01.Fast.Motor01_Fault
```

型、unit、writable、名前、description 等を候補評価に利用できる。

**writable slot の Binding は自動確定より明示的な確認を優先**する。

### 19.11 MCP の位置づけ

SCADA 専用 MCP は初期必須要件にしない。

必要性が出た場合のみ、

```text
MCP Adapter
    |
    v
Design API / Design Domain
```

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

```text
AI
 |
Hub API / Hub MCP
 |
Hub permission / audit / write guard
 |
PLC
```

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

## 20. Repository / CI（2026-09-30 オーナー決定: banto-industrial 内に置く）

**banto-scada は banto-industrial の中に置く。** 配置は `apps/banto-scada`（画面と host）と
`crates/scada-*`（§18 の scada-model / renderer / editor / runtime / server のうち Rust で書く部分）。
理由:

- path-aware CI（#469）が `apps/banto-scada` を足す手順を ci.yml と `ci-changes.mjs` の冒頭に既に
  用意している（RULES・AREAS・RUST_PACKAGES と frontend-scada / e2e-scada ジョブの追加）
- banto-tagclient、tstore / tsquery、banto-alarm など共有 crate を同じ workspace で参照でき、
  ChronoGazer と共有するトレンド UI の package 化（§13）も同じ pnpm workspace で済む
- Hub や ChronoGazer と横断する変更（SDK の契約変更など）を 1 つの PR で出せる

banto-industrial Issue #468 の path-aware CI は #469 で導入済み（2026-09-30 時点の main）。現状は

- Hub change -> Hub CI
- ChronoGazer change -> ChronoGazer CI
- SCADA change -> SCADA CI（`apps/banto-scada` の追加時に上記手順で有効化する）
- docs-only -> minimum CI

を成立させる。

分割は確定事項ではなく、

- independent release cycle
- concurrent development conflicts
- Issue/PR ownership
- cross-repository atomic changes

を基準に repository 分割を再評価する。

---

## 21. 実装ロードマップ案

スライス番号は付けた順で、実装順・依存関係を表さない。実装順は §3 の区分（Core v1 / v1.1 / 後続 Extension）と
各見出しの注記、および本節の並び順に従う。

### S0 Architecture

- 本書を設計の基準文書として確定
- Project schema の最小型を定義
- Hub に必要な追加 API を洗い出す（write 要求の相関 ID、§16）
- Design Domain（共有 mutation / validation）の最小契約を確定。REST / OpenAPI 露出は S5

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
- external_name binding
- quality / unresolved
- reconnect / rebinding
- live tag source を抽象化し、Runtime は具体的な banto-tagclient に直接依存しない（Replay driver を同じ
  受け口で受けるための境界。§13.3 の前提条件 4。v1 は境界だけで、driver 本体は将来）

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

### S9 Action Engine（v1 の基本 Action。§3）

- Navigate
- Dialog
- PLC Write
- audit
- workflow は単純 sequence + success/failure まで（§3「初版で限定する項目」）

### S9b scada-server core + host 1（v1 の基盤。S10 / S12 の前提）

- scada-server core のライブラリ境界（画面非依存、設定・Project・記録の置き場所と権限、§13.2）
- 起動モードの区別: ローカルモード（サービス検出 → 接続、無ければ埋め込み起動。T16-2 の判定）と
  共有サービス接続モード（埋め込み起動しない）
- host 1: アプリ埋め込み起動、トレイ、状態画面。v1 時点では core の中身は空でよい
- Replay の前提となる Project schema（記録対象の範囲、名前履歴。§13.3）

### S10 Alarm（汎用、v1.1。host は S9b の scada-server core）

- banto-alarm
- generic AlarmDefinition / AlarmState / AlarmEvent
- tag / expression based source
- Alarm API
- Alarm Viewer
- operator ACK / Shelve
- protocol-specific PLC alarm adapter は後続

### S10b 常時実行 Event / Action（v1.1。host は S9b の scada-server core。§10.3）

- server-side Event: value edge / timer / alarm 起点（tracking 起点は Tracking の host 決定後）
- scada-server core 上の常時実行 engine（UI が閉じていても成立。Project の revision を読み、
  変更通知で再読込）
- 基本的な Action execution（PLC Write / audit。§16 の execution record を共有）
- 副作用のある Action を 1 台に限定する境界（将来リースで抑止。§13.2 の冗長化方針、§22 #19）。
  v1.1 は単一インスタンス前提でよいが、engine が「自分が実行担当か」を問い合わせる口だけ置く
- server-side HTTP / DB command は S9c

### S12 History / Trend（ChronoGazer と共有、v1.1。host は S9b の scada-server core）

- Hub 経由購読ドライバの共有化（#383 段階1 の切り出し）
- tstore / tsquery を使う SCADA recorder（記録対象の範囲の設定を読む。catalog メタデータと
  Project revision を記録に添える）
- 履歴読み出し API（ChronoGazer R1-D と共有 crate 化）
- トレンド UI の共有 package 化
- Trend widget（Project model、Faceplate からの呼出）

### S12b SCADA server の追加 host（host 3 は v1.1、host 2 は後続）

- host 3: Windows サービス（v1.1。§3。T17 の SCM 登録・elev の型を流用）
- host 2: headless コンソール（後続）

### S11 Tracking（後続 Extension。§3。host は §22 #20 の決定後）

- banto-tracking
- logical location
- current tracking
- event FIFO/sequence
- SCADA tracking presentation

### S9c HTTP Action / External Program（後続 Extension。§3）

- external API（HTTP Action）
- external program
- 高度な Event / Action flow

### S13 DB Table/View（Dataset、Hub 側の API 設計後）

- Hub 側: Dataset の登録（table / view / registered query）、schema / rows API、pagination / sort /
  allowed filter、timeout / concurrency limit（banto-hub-external-db-design.md への追補）
- table/view registration
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

1. banto-scada を banto-industrial 内に置くか別 repository にするか → 2026-09-30 決定済み（§20、banto-industrial 内。`apps/banto-scada` と `crates/scada-*`）
2. Project package の正式拡張子
3. Stable ID の UUID/ULID 方式
4. Screen coordinate の内部単位（normalized / logical pixel の併用方針）
5. banto-expr を client/runtime でそのまま利用するか
6. DB tabular resource の domain model → 2026-09-30 決定済み（§14.2、案 B の Dataset / DB Resource。API の具体形は Hub 側の設計で確定）
7. 常時実行 Event / Action の実行主体 → 2026-09-30 決定済み（§13.2、scada-server core）
8. Core v1 に含める Extension の範囲 → 2026-09-30 決定済み（§3。v1 は画面とライブ値・操作まで、scada-server 系は v1.1）
9. Tracking PLC block の標準 memory layout
10. Editor/Runtime の executable 分離時期
11. Design API の最終 transport / bind policy（初期候補: editor mode + loopback REST）
12. AI change plan の承認を必須にする変更範囲
13. protocol-specific Alarm adapter の優先順位（MELSEC は汎用 Alarm 後）
14. Binding identity の方式 → 2026-09-30 決定済み（§9.6、名前のみ。案 C の改名候補提示は必要が出たら拡張）
15. SCADA 記録プロセスの寿命 → 2026-09-30 決定済み（§13.2、v1 は埋め込み起動、サービス host で分離）
16. ChronoGazer と共有するトレンド UI の package 化の方法（§13）
17. SCADA 同梱 recorder の配布形態 → 2026-09-30 決定済み（§13.2、SCADA 同梱の core。ChronoGazer の流用は要件にしない）
18. 記録対象タグの所有者 → 2026-09-30 決定済み（§13.1 の 4、SCADA Project）
19. 冗長化の詳細設計（Hub と SCADA server を横断する別草案 `docs/banto-hub-redundancy-design.md`）: リースの実装方式（PLC 調停を第一候補）、Hub の warm standby と構成・API キー・internal タグの同期、client の複数エンドポイント切替、recorder の履歴統合、Alarm の操作者状態の複製、PLC 冗長系のドライバ対応（複数 endpoint、MELSEC の制御系指定の確認）。原則は §13.2 で決定済み。scada-server と Hub の単一構成が動いてから着手する
20. Tracking domain の host（Hub 側の ingest か SCADA server か。§12、§2）
21. Replay の実装時期とライセンス上の扱い（§13.3。将来機能、有料版候補。前提条件 1〜4 は v1 に残す）

---

## 23. 議論継続中の主要論点

- protocol-specific PLC Alarm adapter の順序・契約。MELSEC 対応は汎用 Alarm model 後

---

## 24. 現時点の主要決定（2026-09-30 オーナー決定。§9.6 の再検討中項目を除く）

- PLC は control authority。PC 停止で設備制御を止めない
- banto-scada は banto-industrial 内に置く（`apps/banto-scada` と `crates/scada-*`。2026-09-30 オーナー決定、§20）
- v1 は「画面を作り、Hub の値を安全に表示し、基本操作できる」縦切り。scada-server core はライブラリ境界と起動モードの分岐だけを v1 に含め、recorder / Alarm / 常時実行 Event / サービス host は v1.1（2026-09-30 オーナー決定、§3）
- SCADA は PLC に直接接続せず Hub を介する
- SCADA Project の永続 Tag Binding は Hub の `external_name`（`connection.group.tag`）を正とする（2026-09-30 再確認、§9.6）
- 購読・書き込みも `external_name` で行い、Hub 内部の `StableTagId` は SCADA では使わない。banto-tagclient の binding 同一性も名前に改める（2026-09-30 オーナー決定、§9.6）
- Hub 側で直接 rename して Binding が切れることは許容する。SCADA Editor からの rename は Hub API と Project 参照更新を協調して行う
- Screen model / Renderer / Editor を分離する
- SVG + Svelte を Process Renderer の第一候補とする
- EquipmentType / EquipmentInstance を持つ
- Symbol と Faceplate は同じ Equipment binding context を共有する
- Faceplate / Dialog / Popup を Project model の一級要素とする
- Binding / Event / Action Engine を Runtime の中心に置く
- 不完全な Binding があっても Runtime 全体を止めず、表示は safe/degraded state とする
- Action が成立しない場合は実行せず、失敗を表示・記録する
- External API / External Program を Action として扱う
- Alarm は tag kind ではなく独立した汎用 domain として先に設計する
- 初期 Alarm は protocol 非依存とし、MELSEC 固有の Alarm ingest は後続 adapter とする
- 搬送 Tracking は独立 domain。PLC authoritative
- Historian / Trend は Hub History API ではなく ChronoGazer と共有する記録・トレンド資産で実現し、Hub は履歴を持たない（2026-09-30 オーナー決定、§13）
- 24/365 の処理（recorder、Alarm engine、常時実行 Event / Action、履歴・Alarm API）は画面と別の scada-server core に置き、Hub には足さない。host は埋め込み / headless / Windows サービスの 3 層で、v1 は埋め込み起動、サービス host は後続。共有サービス接続モードでは埋め込み起動しない。論理サービスとインスタンスの識別子を分け、将来の冗長化を許容する（2026-09-30 オーナー決定、§13.2）
- 記録対象タグの所有者は SCADA Project。server core は配布された Project を読む（2026-09-30 オーナー決定、§13.1）
- SCADA の DB Table/View アクセスは Hub を接続境界とし、表形式データは Tag と別の first-class resource「Dataset」で表現する（案 B。2026-09-30 オーナー決定、§14.2）
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
