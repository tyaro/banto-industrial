# banto-hub 接続設定とアドレス補助をドライバから組み立てる 詳細設計（R2b-6）

作成日: 2026-10-24
最終更新: 2026-10-24（PR #810 のレビューの指摘を反映: オフセットの点数とワード数を分ける §4.3・文字列を含む種類の語彙 §3.4・保存前の接続テストの形 §5。2 回目: ワード内ビットのワード単位のオフセット `offset_words` §3.4・§4.3、ホストの接続のデータ型の選択肢 §7。3 回目: オフセットの要求の接続を項目ごとに §4.3、設定パッケージの形を R2b-6 で変える §5・Q11、REST の形を使う箇所の一覧 §5。4 回目: 設定パッケージの push のブリッジの接続の持ち運び §5、構造体登録の衝突判定のためのアドレスの位置 `index` §3.6。同日、置き換える TS の働きを総点検して §7.1 に表を足した。5 回目（tyaro のレビュー）: 接続テストの `simulation` §5、衝突判定の収集グループの境 §3.6、パッケージの欠けてよい欄 §7.1）
状態: **設計確定（2026-10-24 オーナー決定。§11 の Q1〜Q11 は推奨どおり（すべて a）。オフセットコピーでワード内ビットをワード単位で動かす §4.3・Modbus の連続登録の開始番号の既定が 400001 になる §7.1 も含む）。実装は未着手**。コード調査（§1）は 2026-10-24 の main（`9626f09c`、v0.2.0-alpha.38、R2b-5 完了の後）に対して行った。前提のオーナー指示（2026-10-24）: **最初に対応するドライバは MELSEC（SLMP）と Modbus（TCP）**。
対象: [banto-hub-driver-refactor-plan.md](banto-hub-driver-refactor-plan.md) §4 の R2b-6「UI をドライバのスキーマから組み立てる汎用フォームに替え、iframe の枠を用意する」と、[banto-hub-driver-plugin-design.md](banto-hub-driver-plugin-design.md) §12.2 の 7「UI はドライバから組み立てる。プロトコル別の Svelte の分岐と TS の写しを消す」。
関連: [banto-driver-api-design.md](banto-driver-api-design.md)（§4.1 `DriverBase`、§9 WIT との対応）、[banto-hub-connection-settings-design.md](banto-hub-connection-settings-design.md)（R2b-1。§2 の R2b-6 / R2b-7 の境、§3.2 型つきの `settings`、§4.3 平らな形のアダプタ）、[banto-hub-driver-address-design.md](banto-hub-driver-address-design.md)（R2b-2。§7.1 `POST /api/tags/parse-addresses`）、[banto-hub-driver-plugin-design.md](banto-hub-driver-plugin-design.md) §8（同梱 UI と iframe）。

---

## 0. 要約

| #   | 提案                                                                                                                                                                                                            | 根拠の節 |
| --- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------- |
| 1   | `banto-driver-api` に**設定のスキーマ**（`SettingsSchema` = 欄の並び。欄は キー・表示名・説明・種類・必須・既定値・区分）と `DriverBase::settings_schema()`（既定は空）を足す。WIT は `config.schema` を足す    | §3       |
| 2   | ドライバの**データ型の対応**（`supported_kinds`）も同じく `DriverBase` に足す。UI のデータ型の選択肢と `banto-tags` の `MODBUS_ONLY_DATA_TYPES` をこれに替える                                                  | §3.4     |
| 3   | Hub に **`GET /api/drivers`**（認証付き）を足す: 記述子・スキーマ・データ型・`address_help`。UI のプロトコルの選択肢・既定ポート・既定のワード順はすべてここから取る                                            | §4       |
| 4   | **ホストがスキーマで型・必須・範囲を確かめ、型つきの JSON で保存する**（`settings_from_flat` のプロトコル別の写しを、スキーマに従う 1 つの関数に替える）。意味の検証は今までどおり `validate_config`            | §4.2     |
| 5   | Hub の REST `/api/plc-connections` の形を R2b-6 で `{name, protocol, enabled, simulation, settings}` に替える（R2b-7 から前倒し。MCP・ChronoGazer は R2b-7 のまま。設定パッケージは Q11）                       | §5, Q2   |
| 6   | `ConnectionDrawer.svelte` の SLMP / Modbus の分岐を、スキーマから組み立てる**汎用フォーム**（`DriverSettingsForm.svelte`）に替える。`postgres`・`push` は今の専用の欄を残す（ホストの接続で、ドライバではない） | §6, Q3   |
| 7   | タグの画面の **TS の写し（`slmpDeviceTable.ts`）を消す**。連続登録・オフセットコピー・構造体登録のアドレスの計算を、ドライバの `offset_address` を呼ぶ **`POST /api/tags/offset-addresses`** に替える           | §7, Q5   |
| 8   | **iframe の枠は W 段へ送る**（R2b-4 と同じ理由: 中身を出せるドライバが W 段まで無い）。R2b-6 では記述子に UI の入口を持てる場所を決めるだけにする                                                               | §8, Q6   |
| 9   | PR は **6a（API とスキーマ・適合テスト）→ 6b（Hub の `/api/drivers`・スキーマでの保存・REST の形）→ 6c（汎用フォーム）→ 6d（アドレス補助の API 化と TS の写しの削除）→ 6e（docs・CHANGELOG・版）**              | §9       |

### 0.1 既存の決定・計画から外れる点

| 既存の決定・計画                                                                   | 本書の提案                                                 | 理由                                                                                                                                                                    |
| ---------------------------------------------------------------------------------- | ---------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| connection-settings-design §2「REST `/api/plc-connections` は R2b-7 で形を変える」 | Hub の REST だけ R2b-6 で変える（Q2）                      | 汎用フォームは `settings` をそのまま送る。平らな形のまま作ると、フォームの欄 ↔ 平らな欄の写しをプロトコルごとに TS で書くことになり、R2b-6 の目的（分岐を消す）に反する |
| refactor-plan §3「ドライバ同梱 UI を載せる iframe の枠」を R2 の中で作る           | W 段へ送る（Q6）                                           | 中身を配れるのは W 段のプラグインだけ。枠だけ作ると、使われない sandbox・CSP・ブリッジを W 段まで保守することになる（R2b-4 を W 段へ送った 2026-10-23 の判断と同じ形）  |
| plugin-design §6.2 の WIT（`config` は `validate` と `secret-keys` だけ）          | `config.schema` と `address.supported-kinds`（仮名）を足す | 汎用フォームとデータ型の選択肢の材料。WIT と trait は同じ PR で直す約束（plugin-design §6.2 の補足）                                                                    |

## 1. 前提（コードの現状、2026-10-24 の main `9626f09c` に照合）

行番号は目安。着手時に `rg -n "protocol === '(slmp|modbus-tcp)'|slmpDeviceTable|MODBUS_ONLY_DATA_TYPES|DEFAULT_PORTS|PROTOCOL_OPTIONS" apps crates` で再確認する。

### 1.1 ドライバ API

- `crates/banto-driver-api/src/driver.rs` の `DriverBase`: `descriptor`・`validate_config`・`secret_keys`・`parse_address`・`offset_address`・`address_help`・`probe`。**設定の欄の形（スキーマ）を返す口は無い**。`validate_config` は文字列の対の `Settings` を受けて、キーごとの `FieldError` を返す。
- `DriverDescriptor`（`#[non_exhaustive]`）: `id`・`version`・`display_name`・`vendor`・`transports`・`capabilities`・`api_version`・`settings_version`。
- 既定つきのメソッドの追加は API を壊さない（api-design §9.1。R2b-3a-2 の `probe` が前例）。

### 1.2 接続の UI

- `apps/banto-hub/src/lib/banto/plcConnectionForm.ts`（758 行）: `PROTOCOL_OPTIONS`（84 行、`modbus-tcp`・`slmp`・`postgres`・`push` の固定の表）、`DEFAULT_PORTS`（99 行）、既定のワード順、`protocol === 'modbus-tcp' || 'slmp'` の分岐（106・143 行）、フォーム ↔ 平らな REST の写し。
- `apps/banto-hub/src/lib/components/ConnectionDrawer.svelte`（1,653 行）: 編集（774〜1,000 行付近）と表示（1,213〜1,340 行付近）の両方で、`slmp` の CPU 種別・`postgres` のデータベース・`push` の送り手の欄をプロトコル名で出し分ける。既定ポート・ワード順の「触ったか」の追跡（372・425 行）。

### 1.3 タグの UI

- `apps/banto-hub/src/lib/banto/slmpDeviceTable.ts`（184 行）: `crates/banto-plc` の SLMP のデバイス表の**手書きの写し**（plugin-design §12.1 の 5）。使い手は `continuousRegistration.ts`・`offsetCopy.ts`・`structRegistration.ts`（アドレスの番号を進める・ニーモニックを読む）。
- `apps/banto-hub/src/lib/banto/tagAddressHelp.ts`（228 行）: プロトコル別のアドレスの説明と占有の注意（`slmpOccupancyHint`・`modbusOccupancyHint`・`pushOccupancyHint`）。
- `routes/(app)/tags/+page.svelte:239` `dataTypeOptionsFor`: `modbus-tcp` だけ 64bit 型を出す（#325）。サーバー側は `crates/banto-tags/src/tag.rs:93` の `MODBUS_ONLY_DATA_TYPES`。
- 書き込み可の既定（`writableDefault.ts`）と保存前の解析は、既に `POST /api/tags/parse-addresses`（`apps/banto-hub/core/src/rest/tags.rs:1327`、R2b-2d）でドライバに聞いている。**番号を進める（offset）口は REST に無い**。

### 1.4 REST と保存

- `crates/banto-tags/src/plc_connection.rs:999` `PlcConnectionInput` は平らな形（`host`・`port`・`unit_id`・`word_order`・`cpu_type`・`database`・`username`・`password`・push の `settings`）。`legacy::settings_from_flat` がプロトコルごとに型つきの JSON へ写す（connection-settings-design §4.3）。
- MCP・設定パッケージ・ChronoGazer も同じ平らな形（R2b-7 で変える約束）。

## 2. 範囲: R2b-6 で変えるもの・残すもの

| 面                                        | R2b-6                                                                                     | 後の段                     |
| ----------------------------------------- | ----------------------------------------------------------------------------------------- | -------------------------- |
| `banto-driver-api`（公開 crate）          | 足す: `SettingsSchema`・`settings_schema()`・`supported_kinds()`。版は 0.3.0（Q8）        | -                          |
| Modbus / SLMP のドライバ                  | スキーマとデータ型を実装。適合テストを足す                                                | -                          |
| Hub の REST                               | 足す: `GET /api/drivers`・`POST /api/tags/offset-addresses`。変える: 接続の入出力の形     | -                          |
| `banto-tags`                              | スキーマに従う保存。`MODBUS_ONLY_DATA_TYPES` を `supported_kinds` に替える                | 平らな欄の撤去（R2b-7）    |
| Hub の UI（接続・タグ）                   | 汎用フォーム。TS の写しとプロトコル名の分岐を消す（`postgres`・`push`・`virtual` は残す） | `postgres` の撤去（R2b-8） |
| MCP                                       | 変えない（平らな形のアダプタを残す）。設定パッケージは 6b で版を上げる（§5、Q11）         | R2b-7                      |
| ChronoGazer（REST・Tauri・UI）            | 変えない                                                                                  | R2b-7（Q7）                |
| ドライバ同梱 UI（iframe・配信・ブリッジ） | 作らない                                                                                  | W 段（Q6）                 |

## 3. 設定のスキーマ（6a）

### 3.1 型（`banto-driver-api`、新しいモジュール `schema`）

```rust
/// 接続の設定の欄の並び（表示順）。キーは重複しない。
#[non_exhaustive]
pub struct SettingsSchema { pub fields: Vec<SettingField> }

#[non_exhaustive]
pub struct SettingField {
    pub key: String,              // settings の JSON のキー（"host"・"port"・"cpu_type"）
    pub label: String,            // 表示名（日本語固定、plugin-design §6.2 の補足）
    pub help: Option<String>,     // 欄の下の説明
    pub kind: FieldKind,
    pub required: bool,
    pub default: Option<SettingValue>,
    pub group: FieldGroup,        // Basic（常に見せる）/ Advanced（「詳細」に畳む）
}

#[non_exhaustive]
pub enum FieldKind {
    Text { max_len: Option<u32>, placeholder: Option<String> },
    Host,                                   // IPv4 / ホスト名（IPv6 は対象外、2026-09-29 オーナー決定）
    Integer { min: Option<i64>, max: Option<i64> },
    Choice { options: Vec<ChoiceOption> },  // { value, label }。値は文字列
    Bool,
    Secret,                                 // secret_keys() と一致させる（§3.3）
}

pub enum SettingValue { Text(String), Integer(i64), Bool(bool) }
```

- `serde` の feature で JSON にする（`DriverDescriptor` と同じ）。UI へはこの JSON をそのまま渡す。
- **条件つきの表示（ある欄の値で別の欄を出す・隠す）は v1 に入れない**。Modbus / SLMP には要らず、要るドライバは W 段の同梱 UI で扱う（Q4）。欄どうしの整合は今までどおり `validate_config` が見る。
- 「ポート」は `Integer { min: 1, max: 65535 }` で表す（専用の種類を作らない）。

### 3.2 `DriverBase` に足すメソッド

```rust
/// 接続の設定の欄（WIT `config.schema`）。既定は空 = 汎用フォームは欄を出さない。
fn settings_schema(&self) -> SettingsSchema { SettingsSchema::default() }
```

- 空のスキーマのドライバ（今の偽ドライバ・将来スキーマを書かないドライバ）は、汎用フォームが「このドライバには設定の欄がありません」と出す。JSON を手で書く欄は作らない（Q4）。

### 3.3 適合テスト（`banto-driver-conformance` に足す。C17〜C19 の仮番。C20 は §3.4）

- **C17**: キーが重複しない。`Choice` の値が重複しない。既定値が種類と合う（`Integer` の範囲・`Choice` の値の中）。
- **C18**: `secret_keys()` と、`kind = Secret` の欄のキーが**一致する**。
- **C19**: スキーマの既定値だけで埋めた設定に、ドライバの適合テスト用の有効な設定（既にある fixture）を重ねたものが `validate_config` を通る。fixture の各キーはスキーマにある（スキーマに無いキーをドライバが読んでいないことの近似。`validate_config` の中身は覗けないため）。

### 3.4 データ型の対応

```rust
/// タグの種類の名前（UI のデータ型の選択肢と同じ語彙）。`DataType` は bit・数値だけで、
/// 文字列は別の `TagKind::Str` なので、文字列を含めて表せる型を別に置く。
#[non_exhaustive]
pub enum KindName { Numeric(DataType), String }   // JSON では "bit"・"u16"・…・"f64"・"string"

/// このドライバが読めるタグの種類（WIT `address.supported-kinds`、仮名）。既定は 64bit を除く数値と bit。
fn supported_kinds(&self) -> Vec<KindName> { ... }
```

- Modbus は 64bit を含む数値と bit で、**文字列は含まない**（今の Modbus の `check_kind` が string を断るのと同じ）。SLMP は 64bit を除く数値と bit と **`String`**。`banto-tags` の `MODBUS_ONLY_DATA_TYPES`（#325）の判定を、接続のドライバの `supported_kinds` に替える。`push` は今の `push_tag_errors` のまま（ホストの接続）。
- **ワード単位のオフセット**（§4.3 の `"words"` が使う。WIT `address.offset-words`、仮名）:

```rust
/// `canonical` を n ワードぶん物理的に動かした正規形。ワード内ビットはビットを保ち、ビットデバイスは n 点。
fn offset_words(&self, canonical: &str, n: i64) -> Result<String, AddressError> { /* 既定: 非対応のエラー */ }
```

- 適合テスト **C21**: `offset_words(c, 0) == c`。ワード内ビットのアドレスでビットが保たれる。`offset_words` の後のアドレスを元の型で `parse_address` できる（範囲内のとき）。
- 適合テスト **C20**: `supported_kinds` に含む各種類で、ドライバの例のアドレス（`probe` の点か fixture）を `parse_address` できる。含まない種類（Modbus の `String`・SLMP の 64bit）は `parse_address` が断る（一覧と実際の受け入れが食い違わないこと）。

### 3.5 Modbus / SLMP のスキーマ（connection-settings-design §3.2 と同じキー・型）

| ドライバ     | 欄（表示順）                                                                                                                                                               |
| ------------ | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `modbus-tcp` | `host`（Host・必須）・`port`（Integer 1〜65535・既定 502）・`unit_id`（Integer 0〜255・既定 1）・`word_order`（Choice `high_low` / `low_high`・既定 `high_low`・Advanced） |
| `slmp`       | `host`（Host・必須）・`port`（Integer 1〜65535・既定 5007）・`cpu_type`（Choice `Q` / `R`・既定 `R`）・`word_order`（Choice・既定 `low_high`・Advanced）                   |

既定の値は今の `DEFAULT_PORTS`・`defaultWordOrderFor`・`banto-tags` の正規化と同じにする（6a の着手時に 3 か所を突き合わせ、食い違いがあれば止めて報告）。

### 3.6 アドレスの位置（構造体登録の衝突判定のため）

- 構造体登録（`structRegistration.ts:189-205`）は、欄どうし・既存のタグとのアドレスの重なりを、`slmpDeviceTable.ts` で読んだ**デバイスの番号**（基数を考えた数値）と軸（デバイス名）の範囲で判定している。`parse-addresses` の `area` は軸しか返さず、`offset-addresses` は文字列しか返さないので、このままでは重なりを判定できず、PLC の範囲が重なるタグを登録できてしまう。
- **`DriverAddress` に `index: u64`（`area` の中の位置）を足す**: ワードのアドレスはワードの番号（SLMP の 16 進のデバイスも数値に直す）、ビットデバイスはビットの番号。ワード内ビットはそのワードの番号（`bit` は今のまま別の欄）。範囲は `[index, index + word_count - 1]`（ビットデバイスは点）。`DriverAddress` は `#[non_exhaustive]` でないので、欄の追加は API の版を上げる変更（6a の 0.3.0 に含める。Q8）。
- `parse-addresses` の応答に `area`・`index`・`bit`・`wordCount` を返し、UI の衝突判定はこれで汎用に行う（**既存のタグは対象の収集グループ（`collectionGroupId`）で先に絞る**。今の `structRegistration.ts:234-236,252` と同じく、同じ PLC でも別のグループの重なりは許す。そのうえで同じ `area`・範囲が重なる。**ワード内ビットは `bit` を見ずにそのワードを占めるものとして扱い、同じワードの別のビットどうしも「重なる」と判定する**。今の `structRegistration.ts:173-181` と同じ安全側: 重なりの見落としを避ける）。適合テスト **C22**: `offset_words(c, n)` の `index` が `c` の `index + n`（ワードのアドレス）になる。
- 判定を UI に残すのは、構造体登録が保存前のプレビューで重なりを示すため。保存の層で重なりを断るかは今と同じ（断らない）。

## 4. Hub の API と保存（6b）

### 4.1 `GET /api/drivers`

- 認証付き（読み取りの権限。plugin-design §8 の「一覧 API は認証付き」と同じ）。
- 応答: `[{ id, displayName, vendor, version, capabilities, transports, settingsSchema, supportedKinds, addressHelp, kind: "frame" | "session" | "host" }]`。レジストリの登録順。
- `postgres`・`push`・`virtual` は**載せない**（ホストの接続で、フォームは専用。§6.2）。UI は「ドライバの一覧 + ホストの接続」をまとめて選択肢にする。

### 4.2 スキーマでの保存

- `banto-tags` に純関数 `settings_from_schema(schema, input_json) -> Result<Map, Vec<FieldError>>` を置く: キーごとに種類を確かめ（`Integer` に `"502"` が来たら断る。文字列から数への読み替えはしない）、必須の欠けを報告し、既定値を埋め、スキーマに無いキーは**断る**（ドライバの `validate_config` は未知のキーを断らない約束なので、入口で止める。Q9）。
- その後に今までどおり `settings_to_api` → `validate_config`。エラーは同じ `FieldError` の形で REST に返す（キーは JSON のキーのまま。UI は欄の下に出す）。
- `legacy::settings_from_flat` の Modbus / SLMP の写しは、MCP のために R2b-7 まで残す（設定パッケージは 6b で `settings` の形になるので使わない。Q11）（平らな形 → `settings` の JSON を作り、上の関数を通す）。

### 4.3 `POST /api/tags/offset-addresses`（6d）

- 入力: `{ items: [{ connectionId, address, dataType, stringWords?, n, unit }] }`。**接続は項目ごとに持つ**: 今のオフセットコピーはツリーの絞り込みが「すべて」のとき SLMP と Modbus のタグをまとめて選べるので、1 回の要求に複数の接続が混ざる。サーバーは項目ごとにその接続のドライバで処理する（接続が無い・ドライバの接続でない項目はその項目だけエラー）。出力: 入力と同じ順で `{ ok: { canonical } } | { err: { message } }`。`parse-addresses`（R2b-2d）と同じ形・同じ件数の上限。
- **`unit` で「点数」と「ワード数」を分ける**（ドライバの `offset_address` は `kind.word_count() × n` で進むため。Modbus・SLMP の `base.rs` の `offset_address`）:
  - `"points"`（連続登録）: `n` 点ぶん。元の型のまま `offset_address(canonical, kind, n)`（f32 の `D100` を `n = 1` で `D102`）。
  - `"words"`（オフセットコピー・構造体登録の累積オフセット）: `n` ワードぶんの物理的な変位。まず**元の型で `parse_address`**（アドレスと型の検証は保つ）し、次にドライバの新しいメソッド **`offset_words(canonical, n)`**（§3.4）で進める。`offset_address` に `u16` を渡す形は採らない: SLMP の `offset_address` は軸をアドレスの末尾（`.5`）だけで選ぶので `D100.5` が `D100.6` になり、Modbus は末尾つきのアドレスを 0 以外で進めると断るため。`offset_words` の約束:
    - ワードのアドレス: `n` ワード進める（f32 の `D100` を `n = 1` で `D101`。`400001` は `400002`）。
    - **ワード内ビット（`D100.5`・`400001.3`）: ワードの部分だけ進め、ビットは保つ**（`D100.5` を `n = 1` で `D101.5`、`400001.3` は `400002.3`）。物理的なワードの変位なので、構造体やブロックをまとめて写したとき、ビットの位置関係が崩れない。**今の `offsetCopy.ts` はビットの軸で進める（`D100.5` + 1 → `D100.6`）ので、ここは振る舞いを変える**（使用者無しの前提で、正しい形に直す。§12）。
    - ビットデバイス（SLMP の `M`・`X`・`Y`、Modbus のコイル・入力）: ワードの単位が無いので `n` 点進める（今の `incrementAddress` と同じ）。
    - 既定の実装は「このドライバはワード単位のオフセットに対応しません」のエラーを返す（既定つきの追加で API を壊さない）。
  - 進めた後のアドレスを元の型で `parse_address` し直し、範囲外（例えば 64bit が末尾をはみ出す）はエラーで返す。

## 5. REST の形（6b、Q2）

- `/api/plc-connections` の作成・更新・応答を `{ name, protocol, enabled, simulation, settings }` に替える。`settings` の秘密のキーは応答で `{ "$secret": "set" | "unreadable" }`（今の `passwordState` の代わり）（値・参照は返さない）。更新で秘密のキーを省略すると保つ、`null` で消す、文字列で置き換える（今のパスワードの tri-state と同じ意味。connection-settings-design §4.3）。
- `postgres`・`push` も同じ形にする（`settings` の中身は §3.2 の表のとおり）。push の `settings` は今も JSON なので、平らな欄が無くなるだけ。
- **未適用キュー**: 中身の形（内部）が変わる。使用者無しの前提（2026-10-22 オーナー）なので、古い形のキューの項目は読めなければ捨てて起動ログに出す（Q10）。
- **保存前の接続テスト `POST /api/plc-connections/test` も同じ `settings` の形にする**。今は別の `PlcConnectionTestPayload`（`apps/banto-hub/core/src/rest/plc_connections.rs:778-806`、トップレベルの `host` / `port` が必須）で、`requested_settings`（同 891 行付近）が平らな設定から組み直している。フォームを `settings` に替えた後にこの経路を残すと、未保存のフォームの接続テストが成り立たない。新しい形は `{ protocol, simulation, settings, connectionId? }`（編集中なら `connectionId` で保存済みの秘密値を補う。今の振る舞いと同じ）で、§4.2 の `settings_from_schema` を通してからダイヤルする。**`simulation` は未保存のフォームの今の値を送り、`true` ならダイヤルの前に断る**（今の `PlcConnectionTestPayload` の `simulation`、`plc_connections.rs:784-791` と、`true` を断る `:1200-1207` と同じ。保存済みの行だけを見ると、保存前に SIM をオンにした編集や新規の接続で実機へテストしてしまう）。既存の broker のセッションがある接続では、今の指紋の照合（合わなければ保存を促す、`:993-1009,1033-1046`）も保つ。同じ payload を使う MCP は R2b-7 まで平らな形のまま据え置くので、平らな形 → `settings` のアダプタを MCP 側に分けて残す（`simulation` もそのまま引き継ぐ）。
- **設定パッケージも R2b-6（6b）で `{ protocol, settings }` の形にする**（Q11）。設定パッケージの書き出し・読み込みは UI（`configPackage.ts::sanitizeConnection` が `listPlcConnections()` の平らな `host`・`port`・`unitId`・`wordOrder` を取り出す、`configPackageAdmin.ts::applyConfigPackageInner` がパッケージの平らな接続をそのまま `createPlcConnection` / `updatePlcConnection` へ渡す）で行うので、REST だけ替えると書き出しで欄が落ち、読み込みで古い形を送る。平らな形 ↔ `settings` の写しを TS に置くと R2b-6 の目的に反するので、パッケージの版を上げて接続を `{ name, protocol, enabled, simulation, settings }`（秘密のキーは持たない。今と同じ）で持つ。古い版のパッケージは断る（使用者無しの前提、2026-10-22 オーナー）。**ただし接続をまたぐ参照は今の持ち運びの写しを保つ**: push のブリッジの `bridge.db_connection_id` は、書き出しで接続名 `dbConnectionName` に写し、読み込みで名前から引き直す（今の `configPackage.ts:455-486`）。`settings` をそのまま写すと、接続 id の違う別の Hub で、ブリッジが読めないか、関係のない PostgreSQL の接続を読む。この写しは push（ホストの接続）だけのもので、ドライバの `settings` には接続 id を持たせない。書き出し → 読み込みの往復のテストを 6b に入れる（**書き出し元と読み込み先で接続 id がずれる場合**を含む）。
- **REST の形を使う他の箇所**（2026-10-24 に `rg -l "api/plc-connections"` と UI の `listPlcConnections` 等の呼び手で洗い出した。6b の着手時に同じ検索で取りこぼしを確かめる）: UI では `ConnectionDrawer.svelte`・`plcConnectionForm.ts`・`configPackage.ts`・`configPackageAdmin.ts` だけが平らな欄を読む（他の画面は `name`・`protocol`・`id` だけ）。Rust では Hub の core の REST のテスト（`rest/tests/` の十数本と `core/tests/rbac.rs`）、`banto-hub-host` の統合テスト（`integration.rs`・`live_reconfig.rs`・`t11`・`t12`・`t7`・`t9`）、`banto-hub-sink` のテスト（`bridge.rs`・`bridge_parity.rs`）、ChronoGazer の example `hub_p6_compare.rs`。どれも 6b で新しい形に書き換える。
- MCP の接続のツール・`GET /api/sink/config` 等は変えない（R2b-7）。

## 6. 汎用フォーム（6c）

### 6.1 部品

- `apps/banto-hub/src/lib/components/DriverSettingsForm.svelte`: `schema` と `value`（`settings` の JSON）を受け、欄を種類ごとの入力に描く（Text / Host → `input`、Integer → `input type=number`、Choice → `select`、Bool → チェックボックス、Secret → 「設定済み・変える・消す」の 3 択と入力）。Advanced の欄は「詳細」に畳む。
- 欄の下のエラーは `FieldError.key` で引く（サーバーの応答と、保存前の UI の検査の両方）。UI の検査は種類・必須・範囲だけ（サーバーと同じ規則。意味の検証はサーバーだけ）。
- 表示（読み取り専用）も同じ部品で描く（今の 1,213〜1,340 行の分岐を消す）。

### 6.2 `ConnectionDrawer.svelte`

- プロトコルの選択肢: `GET /api/drivers` の一覧 + ホストの接続（`postgres`・`push`）。`PROTOCOL_OPTIONS`・`DEFAULT_PORTS`・既定のワード順の表を消す。
- ドライバを替えたとき: 新しいドライバのスキーマの既定値で埋め直す。**同じキーで、利用者が触った欄の値だけ引き継ぐ**（今の `portTouched`・`wordOrderTouched` の約束を一般化）。
- `postgres`・`push` の専用の欄は残す（Q3）。`postgres` は R2b-8 で消えるので作り直さない。

### 6.3 消すもの

- `plcConnectionForm.ts` のプロトコル別の表と平らな形の写し（ファイルは汎用の部分だけ残すか、`driverSettingsForm.ts` に寄せる。着手時に決める）。
- `ConnectionDrawer.svelte` の `form.protocol === 'slmp'` 等の分岐。接続テストのボタンも、フォームの `settings` をそのまま §5 の新しい接続テストへ送る（平らな形への写しと SLMP の分岐を消す）。

## 7. アドレスの補助（6d、Q5）

- `tagAddressHelp.ts`: アドレスの文法の説明は `GET /api/drivers` の `addressHelp` に替える。**占有の注意**（ワード数・ビットの扱い）はデータ型と `parse-addresses` の `wordCount` から汎用に作る（`slmpOccupancyHint`・`modbusOccupancyHint` を 1 つにする）。`push` の注意は残す。
- `slmpDeviceTable.ts` を消す。`continuousRegistration.ts`・`offsetCopy.ts`・`structRegistration.ts` の番号の計算を `POST /api/tags/offset-addresses` に替える（一覧を作る前に 1 回まとめて呼ぶ。入力に追従して呼ぶ箇所は `parse-addresses` と同じ間引き）。
- ドライバが解釈できないアドレス（今の「SLMP として読めなければ末尾の数字を進める」のフォールバック）は、API のエラーをそのまま行に出す（フォールバックの推測をやめる）。Modbus の `40001` 形式もドライバの `offset_address` で進む。
- `dataTypeOptionsFor` のうち、**ドライバの接続（Modbus・SLMP）の分だけ** `supportedKinds` に替える（§3.4）。SLMP では string を選べ、Modbus では選べない（今と同じ）。
- **ホストの接続（`push`・`postgres`・`virtual`）と、接続の無いグループ（`computed`・`internal`・`db`・未選択）は今の対応を残す**（`/api/drivers` に載らないため。§4.1）: `push` は string を除く全部（64bit を含む）、それ以外は 64bit を除く安全側（今の `+page.svelte:239-245` と同じ）。`dataTypeOptionsFor` を「ドライバの `supportedKinds` があればそれ、無ければホストの表」の形にし、ホストの表の各場合を vitest で固定する。

### 7.1 置き換えで保つ働き（2026-10-24 の総点検で洗い出したもの）

`slmpDeviceTable.ts` と TS のプロトコル別の分岐が担っていた働きのうち、上の節で触れていないもの。6c・6d はこの表を完了条件に含める。

| 働き（今の場所）                                                                                                                      | R2b-6 での形                                                                                                                                                                                                                                                                                                                                                                                                        |
| ------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 連続登録の名前パターンの既定（`D3000` → `D{n}`、`40001` → `tag{n}`。`continuousRegistration.ts:231-241`）                             | `parse-addresses` の `canonical` が `area` で始まる（大小文字を無視）なら `{area}{n}`、そうでなければ `tag{n}`                                                                                                                                                                                                                                                                                                      |
| 連続登録の開始番号の既定（`D3000` → 3000、`40001` → 40001。同 `:259-269`）                                                            | `canonical` が `area` で始まるなら `index`（§3.6）、そうでなければ `canonical` の数字を 10 進で読む。Modbus は正規形が 6 桁なので `40001` は 400001 になる（表示が変わるだけで、決めるのは名前の番号）                                                                                                                                                                                                              |
| オフセットコピーの「名前がアドレスと同じなら新しい名前も新しいアドレス」（`offsetCopy.ts:78-94,135-148`、大小文字の流儀を保つ）       | 名前と元のアドレスの `canonical` を大小文字を無視して比べる。同じなら新しいアドレスの `canonical` を、元の名前の大小文字の流儀（全部小文字なら小文字）で名前にする。違えば今どおり末尾の数字を進める（名前の規則でアドレスの文法ではないので TS に残す）                                                                                                                                                            |
| 構造体登録のベースアドレスにビット指定（`D100.5`）を断る（`structRegistration.ts:91-98`）                                             | `parse-addresses` の `bit` が null でなければ UI で同じ文言で断る（`offset_words` はワード内ビットを通すので、ここで止める）                                                                                                                                                                                                                                                                                        |
| 構造体登録で既存タグ全件の範囲を作る                                                                                                  | 既存タグを `parse-addresses` にまとめて渡す。上限（10,000 件、`rest/tags.rs:219`）を超えるときは分けて呼ぶ                                                                                                                                                                                                                                                                                                          |
| `virtual` と接続の未選択のアドレスの説明（`tagAddressHelp.ts:171-180,217-226`）                                                       | ホストの表に残す（`push` と同じ扱い）                                                                                                                                                                                                                                                                                                                                                                               |
| 欄のヒント（SLMP の CPU 種別の選び方、ワード順の長い説明、`ConnectionDrawer.svelte:865-899`）                                         | §3.5 のスキーマの `help` に移す（6a で文言をそのまま移す）。「SLMP では局番は未使用」は、SLMP のスキーマに `unit_id` が無いので要らなくなる                                                                                                                                                                                                                                                                         |
| 新規作成の既定のプロトコル（`modbus-tcp`、`plcConnectionForm.ts:319-344`）                                                            | `GET /api/drivers` の先頭（レジストリの登録順。今は Modbus が先）                                                                                                                                                                                                                                                                                                                                                   |
| ホストの接続 → ドライバへ切り替えたときの欄                                                                                           | 引き継ぐ欄が無いので、ドライバのスキーマの既定で埋める                                                                                                                                                                                                                                                                                                                                                              |
| `postgres` の専用の欄（必須・長さ・パスワードの tri-state・`passwordState` の表示、`plcConnectionForm.ts:383-423`）                   | 欄は残すが、REST が `settings` と `$secret` の形になるので、読み書きは 6c で新しい形に替える（§5）                                                                                                                                                                                                                                                                                                                  |
| 設定パッケージの「欄の省略を既定で埋めずに断る」（版 2 で `wordOrder`・`cpuType`・`unitId` の欠けを断る、`configPackage.ts:420-452`） | 新しい版でも保つ: 読み込みでは `settings_from_schema` の既定の補完を使わず、**書き出しが必ず書く欄**（`Secret` 以外で、既定値を持つ欄と必須の欄）が欠けた接続は断る（欠けは手で書き換えたパッケージだけ）。`Secret` の欄（パッケージに載せない）と、既定値の無い任意の欄（値が無ければ書き出さない）は欠けてよい。`postgres` のパスワードの要否は今の `passwordSet` の代わりに `settings` の `$secret` の状態で見る |

## 8. iframe の枠（Q6）

- **推奨: W 段へ送る**。R2b-6 では作らない。理由は §0.1。
- 決めておくこと（W 段で API を壊さないため）: 記述子への UI の入口の追加は `DriverDescriptor` が `#[non_exhaustive]` なので既定つきで足せる。汎用フォームと同梱 UI の関係は「同梱 UI があっても汎用フォームを残し、iframe は『補助』のタブとして並べる。保存は Hub 側のフォームで確定」（plugin-design §8 のブリッジの約束と同じ）。この 2 点を plugin-design §8 に注記する。
- 代案（Q6 の b）: 6f として `DriverUiFrame.svelte`（`sandbox="allow-scripts"`・`event.source` の確認・ブリッジの型）と、Hub 本体の CSP の `frame-src 'self'` だけを入れる。中身は開発用のテストページだけ。

## 9. PR の分け方

| PR  | 内容                                                                                                                                                                                                                                                                                           | モデルの目安                       |
| --- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------- |
| 6a  | `banto-driver-api` の `schema`・`settings_schema`・`supported_kinds`・`offset_words`・`DriverAddress::index`、Modbus / SLMP の実装、適合テスト C17〜C22、WIT（plugin-design §6.2）と api-design の対応表                                                                                       | sonnet                             |
| 6b  | `banto-tags` の `settings_from_schema` と `supported_kinds` による型の判定、Hub の `GET /api/drivers`、`/api/plc-connections` と保存前の接続テスト `/api/plc-connections/test` の形の変更、設定パッケージの版上げ（UI の書き出し・読み込み）（MCP 用の平らな形のアダプタは残す）、未適用キュー | opus（保存・秘密値・キューに触る） |
| 6c  | `DriverSettingsForm.svelte` と `ConnectionDrawer.svelte` の置き換え（接続テストのボタンを含む）、`plcConnectionForm.ts` の整理、vitest                                                                                                                                                         | sonnet                             |
| 6d  | `POST /api/tags/offset-addresses`、`parse-addresses` の応答に `index`・`bit`、構造体登録の衝突判定の置き換え、`slmpDeviceTable.ts`・プロトコル別の説明の削除、タグの画面の置き換え                                                                                                             | sonnet                             |
| 6e  | docs（本書・refactor-plan・operations・mcp-reference の注記）・CHANGELOG・版（banto-hub alpha.39）                                                                                                                                                                                             | haiku                              |

- 6c と 6d は 6b の後なら並べられる（触るファイルが分かれる）。
- 6b で REST の形を変えた時点で、6c が入るまで今の UI は接続を保存できない。**6b と 6c は同じ版の中で続けて入れ、版は 6e で上げる**（途中の main は UI が壊れた状態になる。避けるなら 6b に今の UI の最小の追従を入れる。Q2 の判断と一緒に決める）。

## 10. テスト

- 6a: 適合テスト C17〜C22 を Modbus / SLMP で通す。偽ドライバ（スキーマ空）で既定のメソッドを確かめる。 C20 は SLMP の string が通り、Modbus の string・SLMP の 64bit が断られることを含む。
- 6b: `settings_from_schema` の単体（種類の違い・必須の欠け・範囲外・未知のキー・既定値の補完・秘密のキーの tri-state）。REST の統合テストを新しい形に書き換える（`banto-hub-host` 側。実ドライバが要る）。64bit 型の判定が Modbus で通り SLMP で断られる（#325 の約束が保たれる）。 接続テストは、保存前（`connectionId` 無し）と編集中（保存済みの秘密値を補う・設定を変えた値で張る）の両方を新しい形で確かめ、MCP の平らな形の接続テストが今のまま通ることも確かめる。 設定パッケージの書き出し → 読み込みの往復（Modbus・SLMP・postgres・push）と、古い版の断り。 接続テストで `simulation = true` のときは、新規・編集中のどちらでもダイヤル・読み取りが 0 回で断られる。
- 6c: `DriverSettingsForm` の vitest（種類ごとの描画・Advanced の畳み・エラーの表示・ドライバを替えたときの引き継ぎ）。
- 6d: `offset-addresses` の REST の統合テスト（SLMP の 16 進のデバイス・ビットの `.F` の繰り上がり・Modbus の `400001`・範囲外のエラー）。連続登録・オフセットコピーの vitest は API を偽で差し替える。`unit` の違い: f32 / 64bit / string のタグを `"words"` で 1 ワード進めると 1 ワードだけ動き、`"points"` では型のワード数ぶん動く。u16 の直後に f32 を置く混在型の構造体登録の累積オフセット。ワード内ビットの `"words"`（`D100.5` + 1 → `D101.5`、`400001.3` + 1 → `400002.3`）とビットデバイス（`M100` + 1 → `M101`）。ホストの接続（`push`・`postgres`）と接続の無いグループのデータ型の選択肢が今と同じこと。UI 側は連続登録の行数と型ごとの選択肢（SLMP の string を選べ Modbus では選べない）の vitest。 SLMP と Modbus のタグが混ざったオフセットコピーが、項目ごとにそれぞれのドライバで進むこと。 構造体登録の衝突判定: 手動の割り当てと既存タグとの重なり（SLMP の 16 進のデバイス `X1F` と `X20`、ワード内ビット、f32 が 2 ワードにまたがる重なり）を、今の vitest と同じ場合で固定する。 衝突判定は「同じグループの重なりは断る」「同じ接続でも別のグループなら許す」の両方。
- 実機: 6e の前に、MELSEC（R08 試験機）と Modbus で、接続の作成・編集・接続テスト・連続登録を UI から 1 回ずつ（STATUS の実機の確かめの欄に入れる）。

## 11. オーナー判断（2026-10-24 決定: すべて推奨どおり）

| Q   | 問い                                                        | 選択肢                                                                                                                                                  | 推奨 |
| --- | ----------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------- | ---- |
| Q1  | スキーマの表し方                                            | (a) 本書の小さな欄の型（WIT で表せる）/ (b) JSON Schema（汎用だが WIT で表しにくく、UI の描き方も広すぎる）                                             | (a)  |
| Q2  | REST の形をいつ変えるか                                     | (a) Hub の REST だけ R2b-6（6b）で変え、MCP・ChronoGazer は R2b-7（設定パッケージは Q11） / (b) R2b-7 を先にやる / (c) R2b-6 は平らな形のまま TS で写す | (a)  |
| Q3  | 汎用フォームの対象                                          | (a) ドライバ（Modbus・SLMP）だけ。`postgres`・`push` は専用の欄を残す / (b) `postgres`・`push` もスキーマで書く                                         | (a)  |
| Q4  | 条件つきの表示・スキーマが空のときの JSON の手書き欄        | (a) どちらも v1 に入れない / (b) 入れる                                                                                                                 | (a)  |
| Q5  | アドレス補助の API 化（TS の写しの削除）を R2b-6 に含めるか | (a) 含める（6d）/ (b) 別の段にする                                                                                                                      | (a)  |
| Q6  | iframe の枠                                                 | (a) W 段へ送る（記述子の入口と汎用フォームとの関係だけ注記）/ (b) 6f で枠とブリッジの型だけ入れる                                                       | (a)  |
| Q7  | ChronoGazer の接続の UI                                     | (a) R2b-7 で REST と一緒に追従 / (b) R2b-6 で汎用フォームを共有                                                                                         | (a)  |
| Q8  | 版                                                          | (a) `banto-driver-api` 0.3.0（6a）、banto-hub は 6e で alpha.39 / (b) PR ごと                                                                           | (a)  |
| Q9  | スキーマに無いキーを入口で                                  | (a) 断る / (b) 黙って捨てる / (c) 通す（今の `validate_config` の約束のまま）                                                                           | (a)  |
| Q10 | 古い形の未適用キューの項目                                  | (a) 読めなければ捨てて起動ログに出す / (b) 平らな形のアダプタで読み替える                                                                               | (a)  |
| Q11 | 設定パッケージの形                                          | (a) R2b-6（6b）で版を上げ `{ protocol, settings }` にする（古い版は断る）/ (b) 版は据え置き、TS に平らな形 ↔ `settings` の写しを置く                    | (a)  |

## 12. 確かめきれなかったこと・危険

- `DEFAULT_PORTS`・`defaultWordOrderFor`（UI）と `banto-tags` の正規化・ドライバの既定が同じか（§3.5）。6a の着手時に突き合わせる。
- 連続登録・オフセットコピーのフォールバック（SLMP として読めないアドレスの末尾の数字を進める）に依存した使い方が無いか。使用者無しの前提で、エラーを出す形に変えてよいと見ている（§7）。
- オフセットコピーでワード内ビットのアドレスの動き方が変わる（§4.3。今はビットの軸、案はワードの軸）。ブロックを写す用途では案のほうが正しいが、「ビットを 1 つずらして写す」使い方をしていたなら変わる。
- 6b と 6c の間で main の UI が接続を保存できない窓（§9）。
- 構造体登録（`structRegistration.ts`）がニーモニックを読む用途は、offset だけで置き換えられるか。6d の着手時に確かめ、無理なら `parse-addresses` の応答に `area` を足す（R2b-2d の形に既定つきで追加）。
