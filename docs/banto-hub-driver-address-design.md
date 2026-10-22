# banto-hub タグのアドレスの不透明化 詳細設計（R2b-2）

作成日: 2026-10-22
状態: **設計確定（2026-10-22 オーナー決定。§11 の Q1〜Q9 はすべて推奨どおり）・R2b-2a（#781）・R2b-2b（#782）・R2b-2c 実装済み、R2b-2d・2e は未実装**。コード調査（§1）は 2026-10-22 の main（`bbd40a50`、v0.2.0-alpha.34、R2b-1 完了の後）に対して行った。§0.1 に既存の決定・計画から外れる点をまとめた。2026-10-22 のオーナー決定「使用者無しの前提なので移行は考えなくてよい（R2b-7 以降も同じ）」に従い、既存のデータの移行・全件照合・互換は設計しない。
対象: [banto-hub-driver-refactor-plan.md](banto-hub-driver-refactor-plan.md) §4 の R2b-2「アドレスの不透明化（`DriverAddress`）」を実装できる粒度に落とす。タグのアドレスをドライバの `parse_address` で検証して `canonical` を保存すること、Hub の中の `"modbus-tcp"` によるアドレスの解析の分岐をレジストリに置き換えること、要求の型の切り替えの範囲（R2b-3 との境）、エラーの分類、画面・API・CSV の見え方、PR の分け方。
関連: [banto-driver-api-design.md](banto-driver-api-design.md)（§0.1 の 5・Q6・Q8、§3.3 のアドレス、§3.4 のエラーの分類、§6.2 の「R2b に残したもの」、§7 の適合テスト C1・C4）、[banto-hub-driver-plugin-design.md](banto-hub-driver-plugin-design.md)（§3.2・§7.4・§12.2 の 4）、[banto-hub-connection-settings-design.md](banto-hub-connection-settings-design.md)（R2b-1。`with_registry` の注入と grep のテストの前例）、[implementation-checklist.md](implementation-checklist.md)。

---

## 0. 要約

| #   | 提案                                                                                                                                                                                                                                                                                                                | 根拠の節 |
| --- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------- |
| 1   | タグの `address` 列には、接続のドライバの `parse_address(text, kind)` が返した **`canonical` を保存する**（Q8 = A のとおり、Modbus は 6 桁 `400001`、SLMP は大文字・先頭ゼロなし）。保存の層（`banto-tags`）にレジストリを注入して行う（R2b-1 の `PlcConnectionService::with_registry` と同じ形）                   | §3       |
| 2   | 検証は**保存の時点で、すべてのタグに**かける（無効のタグ・`string` のタグも）。今は Hub の保存前の検証が「有効な数値のタグ」しか解析しない（§1.3）。接続のプロトコルの変更・グループの別の接続への移動も、配下の全タグを新しいドライバで解析し直し、通らなければ一覧つきで断る                                      | §3.3     |
| 3   | DB のスキーマは変えない（migration なし）。`address` は今も `TEXT` で、`canonical` もただの文字列。既存の行の書き換え・照合・preflight・バックアップは行わない（オーナー決定、§0.1）                                                                                                                                | §5       |
| 4   | Hub の core から `"modbus-tcp"` によるアドレスの解析の分岐（4 か所、§1.2）を消す。検証と `writable_area` はレジストリの `parse_address`、`banto_plc::Address` への戻しは `banto-plc` の 1 つの関数（HostBound の橋、R2b-3 で消える）に寄せる                                                                        | §4       |
| 5   | 要求の型の切り替えは **ホスト側まで**: 計画・即時読み取り・書き込みの入口は `TagSpec`（`DriverAddress` + `TagKind`）で考え、broker・`PlcClient`・`banto-plc` の計画は R2b-3 まで今の `Address` のまま（ドライバの境界で戻す）。束ね（領域・語数）はドライバの計画の内側で、ホストは `area`・`word_count` で束ねない | §4.2     |
| 6   | 種類（データ型）に依る断り（ビット指定 + `bit` 以外、領域とデータ型の不整合、SLMP の 64 bit、Modbus の `string`）を各ドライバの `parse_address` へ移し、`banto-tags` の `modbus_read_only_area`・`MODBUS_ONLY_DATA_TYPES` のプロトコル名の分岐を Hub では使わない                                                   | §3.2     |
| 7   | `InvalidAddress` は保存と計画の時点で止まり、接続の後には起きない。分類は致命から外し、そのタグだけの `ItemError::Unsupported` にする（適合テスト C4 を直す）                                                                                                                                                       | §6       |
| 8   | 画面・API の見え方: `40001` と入れると保存後は `400001` が返る。CSV の差分は正規形で比べる（Hub の解析の API を使い、TS に文法を写さない）                                                                                                                                                                          | §7       |
| 9   | ChronoGazer は R2b-2 では旧規則のまま（レジストリを持たない。`TagService::new` は今の振る舞い）。追従は R2b-7（または前倒し、Q7）                                                                                                                                                                                   | §8       |
| 10  | PR は R2b-2a〜R2b-2e の 5 本。版は最後に一度だけ上げる                                                                                                                                                                                                                                                              | §10      |

### 0.1 既存の決定・計画から外れる点

| 既存の決定・計画                                                                                                                                                                                                                          | 本書の提案                                                                                                                                           | 理由                                                                                                                                                                                                                                                         |
| ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| refactor-plan §4 R2b-2「既存タグのアドレスを移行時に全件照合する（plugin-design §7.4 の preflight と同じ仕組み）」、plugin-design §12.4「移行時に全件を照合する」、refactor-plan §6「移行時に全件照合し、1 件でも合わなければ移行しない」 | **行わない**。移行・全件照合・起動を断る preflight・自動バックアップ・予行・戻し方・古い DB との互換は設計しない                                     | **2026-10-22 オーナー決定「使用者無しの前提なので移行は考えなくてよい」**（同日の追加で「R2b-7 以降も同じ」）。R2b-1 の起動時の仕組み（`upgrade.rs`）は R2b-2 では使わない。plugin-design §7.4 の preflight（プラグインの版の入れ替え）は W 段の話として残る |
| api-design §3.3 の案 C「`address_text` 列で利用者の綴りを残す（R2b-2 の拡張候補）」                                                                                                                                                       | 足さない（Q9）                                                                                                                                       | 綴りを残す理由は表示だけで、正規形の表示で困った報告は無い。列を足すと保存・更新・CSV・設定パッケージの全部に 2 つ目の値が要る                                                                                                                               |
| api-design §3.4「`InvalidAddress` は R1 の互換のため致命のまま」                                                                                                                                                                          | 非致命（`ItemError::Unsupported`）にする（Q5）                                                                                                       | api-design 自身が「R2b-2 で要求が `DriverAddress` になれば接続後には起きない」と書いている。`PlcError::InvalidAddress` を作るのは解析だけ（`banto-plc/src/address.rs:170`・`slmp/address.rs:423`）で、今も接続の後には返らない                               |
| api-design §3.3「R2b-2 で `TagSpec.address: DriverAddress` を要求の型にし、各ドライバが `canonical` を内部表現へ戻す」・§4.4 の B「セッション型で包むのは R2b-2 と同時」                                                                  | ホスト側の入口だけを `TagSpec` にし、broker・`PlcClient` の要求の型は R2b-3 まで変えない（Q3）                                                       | broker の job と `PlcClient::read_batch(&[ReadRequest])`・`banto-plc` の計画（`Address` の領域で束ねる）は R2b-3 で Frame / Session のエンジンに置き換わる。R2b-2 で同じ場所を一度書き換えると、R2b-3 で二度目になる                                         |
| refactor-plan §4 R2b-7「REST / MCP / 設定パッケージの形の変更と ChronoGazer の追従」（互換のため後ろに置いた）                                                                                                                            | 前提が「使用者無し・互換なし」（2026-10-22）に変わったので、R2b-2 で要る形の変更（解析の API）は R2b-2 で行う。ChronoGazer の追従を前倒しするかは Q7 | 互換を保つための後回しの理由が消えた。段の順番はオーナーの判断なので、範囲の選択は §11 に問いとして残す                                                                                                                                                      |

## 1. 前提（コードの現状、2026-10-22 の main `bbd40a50` に照合）

行番号は目安。各 PR は着手時に再確認する（`rg -n 'Address::parse|parse_slmp|"modbus-tcp"' --type rust apps crates`）。

### 1.1 保存と検証（`banto-tags`）

| 項目                       | 場所                                                                                                                                                           | 事実                                                                                                                                                                                                                                   |
| -------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 保存する値                 | `crates/banto-tags/src/tag.rs:553`・`:829`                                                                                                                     | 前後の空白を除いた**利用者の綴りのまま**保存する。`40001`・`d100`・`X0010` はそのまま残る                                                                                                                                              |
| 書式の検証                 | `tag.rs:546-553`（「Address format is protocol-dependent and deliberately left to I2」）                                                                       | `plc` タグは「空でない」だけ。Modbus / SLMP の文法は見ない                                                                                                                                                                             |
| 種類ごとの規則             | `tag.rs:898` `placement_verdict`（呼び出し `:1143`・`:1357`、接続の `protocol` を同じトランザクションで読む）                                                  | `hub` は外部名（`validate_hub_tag_address :222`）、`push` はキー（`push_tag_errors`、`validate_push_key`）、`db` は結果列名（`is_db_column_identifier :846`）、`computed` / `internal` は空。`plc` は `virtual`・`postgres` の下を断る |
| プロトコル名の分岐（残り） | `tag.rs:452` `modbus_read_only_area`（先頭の桁で領域を引く狭い写し）、`:78`・`:985` `MODBUS_ONLY_DATA_TYPES`（`protocol != MODBUS_PROTOCOL` で 64 bit を断る） | どちらも `banto-plc` の文法の写しかプロトコル名の比較                                                                                                                                                                                  |
| 一意性                     | `tag.rs:1224` `unique_address`                                                                                                                                 | `hub` の外部名と `push` のキーだけ。Modbus / SLMP のアドレスは重複してよい（同じワードを別の型で読む）                                                                                                                                 |
| レジストリ                 | `TagService::new(pool)` だけ（`tag.rs:1613`）。`CollectionGroupService` も同じ                                                                                 | R2b-1 で `PlcConnectionService::with_registry`（`plc_connection.rs:1430`）だけが入った                                                                                                                                                 |
| グループの移動の前例       | `crates/banto-tags/src/collection_group.rs:245-264`                                                                                                            | `hub` をまたぐ移動は配下のタグの数を見て断る（同じトランザクション）                                                                                                                                                                   |

### 1.2 アドレスを解析する本番の経路（テストを除く）

| 場所                                                                                    | 何をするか                                                                                                                | 分岐                                                                       |
| --------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------- |
| `crates/banto-collect/src/config.rs:1729` `build_request`・`:1736` `parse_read_request` | 収集の計画（厳格版・寛容版の両方）と保存前の検証の唯一の解釈器。ビット指定 + `bit` 以外を断る（`BitAddressOnNonBitType`） | `Protocol::{ModbusTcp, Slmp}`（`:1565` `parse_protocol` が文字列から作る） |
| `config.rs:1849` `check_tag_address`                                                    | ChronoGazer の保存時の検証（`apps/chronogazer/core/src/tag_address.rs`）。Hub は使わない                                  | `"modbus-tcp"` / `"slmp"`、それ以外と `string` は素通し                    |
| `apps/banto-hub/core/src/rest/tags.rs:279-330` `GET /api/tags/address-writable`         | 画面の「書き込み可」の既定のための領域の判定                                                                              | `"modbus-tcp"`・`"slmp" \| "virtual"`・その他                              |
| `apps/banto-hub/core/src/read_path.rs:254-267` `build_plc_read_request`                 | 即時読み取り                                                                                                              | `"modbus-tcp"` 以外を SLMP                                                 |
| `apps/banto-hub/core/src/write_path.rs:1595-1646`・`:1769-1786`                         | 単発・一括の書き込み（数値・文字列）                                                                                      | 同上                                                                       |
| `crates/banto-collect/src/simulation.rs:347`・`:375`                                    | シミュレータの記憶域のキー                                                                                                | 呼ぶ側がプロトコルで選ぶ（R2b-5 で値の層へ移る）                           |
| `crates/banto-driver-{modbus,slmp}/src/base.rs:138`                                     | R1 の `parse_address`。`canonical = Address::to_string()`、`word_count = kind.word_count()`                               | -（ドライバの中）。**種類（`kind`）による断りは無い**                      |

- api-design §1.1 が「4 か所」と書いた `"modbus-tcp"` の分岐は、今は `rest/tags.rs`・`read_path.rs`・`write_path.rs` の 2 か所の計 4 か所（P0-1 の分割で `rest.rs` から移った）。
- `PlcError::InvalidAddress` を作るのは解析だけ（`banto-plc/src/address.rs:170`、`slmp/address.rs:423`）。broker と `PlcClient` は解析済みの `Address` しか受けないので、接続の後に `InvalidAddress` が返る経路は今も無い。分類は `banto-driver-{modbus,slmp}/src/classify.rs:46` で `FatalError::InvalidConfig`、`banto-plc/src/error.rs:145-154` の `is_connection_fatal` でも致命（既定）。

### 1.3 保存前の検証で漏れるタグ（Hub）

Hub の書き込み（REST・MCP・未適用キューの適用・一括・CSV・設定パッケージの取り込み）は、同じトランザクションの中で `preflight_transaction`（`apps/banto-hub/core/src/rest.rs:1185`）→ `preflight_snapshot`（`:1089`）→ `ingest::build_hub_config`（厳格版の計画、`banto-collect/src/config.rs:673`）を通る。この計画は次のタグのアドレスを**解析しない**ので、文法の誤ったアドレスのまま保存できる:

- 無効の接続・グループ・タグ（`config.rs:1219` は有効な接続だけ、グループとタグも有効なものだけを計画に入れる）。
- `string` のタグ（`config.rs:1289` で `build_request` の前に飛ばす。即時読み取り・書き込みでは `read_path`・`write_path` で初めて解析され `InvalidAddress` の 422 になる）。

### 1.4 アドレスの文法と正規形（今の `Display`）

| 入力の例                             | `canonical`                  | 根拠                                                                           |
| ------------------------------------ | ---------------------------- | ------------------------------------------------------------------------------ |
| Modbus `40001`・`400001`・`40001`    | `400001`                     | `address.rs:168-222`（5 桁・6 桁を受け、前後の空白を除く）、`Display :278-299` |
| Modbus `40001.03`・`40001.3`         | `400001.3`                   | bit の接尾辞は 10 進・2 桁まで（`address.rs:78-84`）                           |
| SLMP `d100`・`D0100`・`X0010`・`x1a` | `D100`・`D100`・`X10`・`X1A` | `slmp/address.rs:422-480`（大文字化・先頭ゼロ可）、`format :488-497`           |
| SLMP `d100.a`                        | `D100.A`                     | bit の接尾辞は 16 進 1 桁                                                      |
| `push` `a.b`                         | `a.b`                        | `apps/banto-hub/core/src/ingest/driver.rs:121`（`validate_push_key`）          |

- 冪等（`parse(canonical) == parse(text)`）は既存のテスト（`banto-plc/src/address.rs:527`、`banto-driver-{modbus,slmp}/src/base.rs:391-413`）と適合テスト C1（`crates/banto-driver-conformance/src/statics.rs:47-115`）が固定済み。
- Modbus と SLMP の文法は交わらない（Modbus は数字だけ、SLMP はニーモニックの英字で始まる）。

### 1.5 アドレスを持たない・別の意味を持つ接続

| `protocol`   | Hub のレジストリ        | タグの `address`                                   | 今の `parse_address`                                                                                       |
| ------------ | ----------------------- | -------------------------------------------------- | ---------------------------------------------------------------------------------------------------------- |
| `modbus-tcp` | `HostBound`             | 参照番号                                           | `Address::parse` を包む                                                                                    |
| `slmp`       | `HostBound`             | デバイス表記                                       | `Address::parse_slmp` を包む                                                                               |
| `push`       | `HostProvided`          | 送り手のキー（大文字小文字を区別、接続の中で一意） | `validate_push_key`。`writable_area = false`                                                               |
| `postgres`   | `HostProvided`          | `db` タグの結果列名                                | **常に断る**（`apps/banto-hub/core/src/db_source/driver.rs:128`、「postgres 接続の下にタグは置けません」） |
| `virtual`    | `HostProvided`          | 空（`computed` / `internal`）                      | 常に断る（`computed/driver.rs:81`）                                                                        |
| `hub`        | 載らない（ChronoGazer） | Hub の外部名                                       | -                                                                                                          |

### 1.6 画面・CSV・ChronoGazer

- CSV の更新の分類（`apps/banto-hub/src/lib/banto/tagCsvDiff.ts:110-148`）は、既存のタグと CSV の `address` を**文字列で**比べる。
- 連続登録・オフセットコピー・アドレスの説明は TS にある（`continuousRegistration.ts`・`offsetCopy.ts`・`tagAddressHelp.ts`・`slmpDeviceTable.ts`・`structRegistration.ts`）。R1 で Rust の `offset_address` / `address_help` に移植したが、画面はまだ TS を使う（R2b-6）。連続登録の TS は Modbus で 5 桁（`40002`）を作る。
- ChronoGazer は `banto-tags`・`banto-collect` を共有し、レジストリを持たない（R2b-1 の Q6）。保存時の検証は `check_tag_address`（`tag_address.rs`、作成・更新・プロトコルの変更・グループの移動の 4 か所）。

## 2. 範囲: R2b-2 で変えるもの・残すもの

| 面                                                                    | R2b-2                                                       | 後の段                              |
| --------------------------------------------------------------------- | ----------------------------------------------------------- | ----------------------------------- |
| タグの `address` に入る値（Hub）                                      | **`canonical`**（§3）                                       | -                                   |
| DB のスキーマ                                                         | 変えない（§5）                                              | -                                   |
| 保存時の検証（Hub）                                                   | レジストリの `parse_address` を全タグに（§3）               | -                                   |
| Hub の core の `"modbus-tcp"` によるアドレスの分岐                    | 消す（§4.1）                                                | -                                   |
| 収集の計画・即時読み取り・書き込みの入口                              | `TagSpec` で考え、`banto_plc::Address` へは橋で戻す（§4.2） | R2b-3 で橋ごと消える                |
| broker・`PlcClient`・`banto-plc` / `banto-plc-write` の計画と要求の型 | 変えない                                                    | R2b-3（Frame / Session のエンジン） |
| `InvalidAddress` の分類                                               | 非致命へ（§6）                                              | -                                   |
| 解析の REST（画面・CSV が使う）                                       | 足す（§7.1、Q6）                                            | -                                   |
| 連続登録・オフセットコピー・アドレスの説明の TS                       | 変えない（保存すれば正規形になる）                          | R2b-6（ドライバに聞く）             |
| シミュレータ（`simulation.rs`・`BrokerSimRegistry`）                  | 変えない（正規形も今の文法で読める）                        | R2b-5                               |
| ChronoGazer                                                           | 変えない（Q7）                                              | R2b-7                               |
| `db` タグ（`postgres` の結果列名）                                    | 変えない（`banto-tags` の規則のまま）                       | R2b-8 で撤去                        |
| REST・MCP・設定パッケージのペイロードの形                             | 変えない（`address` は文字列のまま。値が正規形になるだけ）  | R2b-7                               |

## 3. 保存: レジストリで解析して `canonical` を書く

### 3.1 注入

```rust
// crates/banto-tags/src/tag.rs・collection_group.rs
impl TagService {
    pub fn new(pool: SqlitePool) -> Self;                                         // 旧規則（ChronoGazer・banto-tags のテスト）
    pub fn with_registry(pool: SqlitePool, registry: Arc<DriverRegistry>) -> Self; // Hub
}
impl CollectionGroupService { /* 同じ 2 つ */ }
```

- Hub の入口（`runtime.rs:709-711`、`mcp.rs:307-309`、`hub.rs` の予約接続、未適用キューの適用）は全部 `with_registry`。R2b-1 と同じく、Hub の core の非テストのコードに `TagService::new(`・`CollectionGroupService::new(` が無いことを `apps/banto-hub/host/tests/registry_entrances.rs` の grep に足す（読むだけの `TagService::new(..).get(..)`（`read_path.rs:182`・`write_path.rs:923` ほか）も `with_registry` に揃え、例外の一覧を作らない）。

### 3.2 解析の規則（`plc` タグ）

1. 同じ書き込みトランザクションで接続の `protocol` を読み（今の `placement_verdict` と同じ場所）、`registry.get(protocol)` が無ければ `address` の項目エラー（「この接続のドライバ {protocol} が見つかりません」）。
2. タグの行から `TagKind` を作る（`data_type` → `TagKind::numeric`、`string` → `TagKind::string(string_length, encoding)`。純関数 `banto_tags::tag_kind(&TagInput) -> Option<TagKind>` を新設し、計画と即時読み書きも同じものを使う）。`data_type` が不正なら今の `dataType` のエラーだけを返し、解析しない（R2b-1 §4.2 の「(1) が断ったら後は走らせない」と同じ）。
3. `parse_address(text, &kind)`。`Err(AddressError)` は `field: "address"`・`message: AddressError::message()`（ドライバの文言。例「"D3000" は Modbus の参照番号（例: 40001, 40001.3）として読めません」）。
4. `Ok(parsed)` なら、`parsed.canonical` を保存する値にする。`writable == true` かつ `!parsed.writable_area` なら `writable` の項目エラー（今の `modbus_read_only_area` の文言を一般化: 「{area} はプロトコル上の読み取り専用領域のため writable にできません」）。

種類に依る断り（Q4）は各ドライバの `parse_address` に置く:

| 断り                                          | 今の場所                                                                                                              | R2b-2 の置き場                                             |
| --------------------------------------------- | --------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------- |
| ビット指定 + `bit` 以外                       | `banto-collect/src/config.rs:1752-1758`（`BitAddressOnNonBitType`、計画の時点）                                       | Modbus・SLMP の `parse_address`                            |
| 領域とデータ型の不整合（コイル + `f32` ほか） | `banto-plc/src/planning.rs:200-225`・`slmp/planning.rs:284-300`（毎周期そのタグだけ Bad）                             | 同上（計画の側の判定は守りとして残す）                     |
| 64 bit 型は Modbus だけ                       | `banto-tags/src/tag.rs:985`（`protocol != MODBUS_PROTOCOL`）                                                          | SLMP の `parse_address`（`banto-tags` は旧規則でだけ使う） |
| Modbus の `string`                            | 書き込みは Modbus のセッションの `UnsupportedRequestKind`（`banto-driver-modbus/src/session.rs:141`）、読み取りは Bad | Modbus の `parse_address`                                  |

- `push` は今の `validate_push_key` と `push_tag_errors` の残り（`string`・`writable`・スケーリング）。キーの一意性（`unique_address`）は正規形で比べる（今も前後の空白を除いた形で、`validate_push_key` の正規形と同じ）。
- `postgres` の `db` タグ・`virtual` の `computed` / `internal`・ChronoGazer の `hub` は、ドライバの `parse_address` を呼ばない（`tag_kind` と接続の種類による今の規則のまま）。`postgres` / `virtual` の `parse_address` が常に断るのは、「その下に `plc` タグは置けない」という今の配置の規則（`tag.rs:916-935`）と同じ意味なので変えない。
- 種類ごとの正規化（`plc` はドライバ、`db`・`computed`・`internal` は上の今の規則）は 1 つの関数 `banto_tags::normalize_tag_address` にまとめ、保存と解析の API（§7.1）が共有する（R2b-2b で作る）。
- `banto-tags` の旧規則（`TagService::new`）は今の振る舞いのまま（ChronoGazer、§8）。

### 3.3 接続のプロトコルの変更・グループの移動

- `PlcConnectionService` の更新で `protocol` が変わるとき、`CollectionGroupService` の更新で `plc_connection_id` が別のドライバの接続に変わるとき、同じトランザクションで配下の**全タグ**（無効・`string` を含む）を新しいドライバで解析し直す。1 件でも通らなければ、タグの名前とドライバの文言を最大 5 件並べて断る（ChronoGazer の `ensure_protocol_change_keeps_tags_readable` の形、`LISTED_TAGS_LIMIT = 5`）。通れば、各タグの `address` を新しいドライバの `canonical` に書き直す（Q2）。
- 同じドライバの接続の間の移動は、`canonical` が変わらないので解析し直さない。
- 今の Hub は保存前の検証（§1.3）で有効な数値のタグだけを見ていたので、無効のタグ・`string` のタグがあると、これまで通った変更が断られるようになる。

### 3.4 書き込みの全入口

REST の作成・更新・一括作成・一括更新、MCP の `create_tag` / `update_tag`、未適用キューの適用、CSV の取り込み（一括作成・`POST /api/tags/batch-update`）、設定パッケージの取り込み（REST の作成・更新を呼ぶ）。どれも `TagService` を通るので、§3.1 の注入で覆う。未適用キューは積んだ時点の綴りをペイロードに持ち、適用で正規形になる（キューの指紋は接続とグループの行で、タグの綴りは載らない）。

## 4. 読む側: 分岐を消し、`TagSpec` で考える

### 4.1 Hub の core の 4 か所

| 場所                                                | R2b-2                                                                                                                                                                                                                                                                                                                                                                                                 |
| --------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `read_path.rs:254` `build_plc_read_request`         | `registry.get(conn.protocol)?.parse_address(entry.address, &tag_kind)` → `TagSpec` → 橋（§4.2）で `BatchReadRequest`。解析の失敗は今の `ReadNowRejection::InvalidAddress`（422、broker へ渡さない）                                                                                                                                                                                                   |
| `write_path.rs:1595`・`:1769`                       | 同じ。失敗は `WriteRejection::InvalidAddress`（422）。書き込みの関門の順（`writable`・スコープ・受付・頻度・監査）は変えない。解析の位置も今のまま（関門 8 の監査行 `insert_pending` の後・broker へ渡す前。単発・一括とも）で、解析に失敗した書き込みは 422 で broker へ届かず、監査には `failed` の行が 1 つ残る（2026-10-22 オーナー決定、§11）                                                    |
| `rest/tags.rs:279` `GET /api/tags/address-writable` | §7.1 の解析の API に置き換える（Q6）。置き換えない場合も中身は `parse_address` → `writable_area` / `area`。今の `virtual` の `Some(true)` の枝は引き継がない: `writableDefault.ts` は `tagKind !== 'plc'` のとき API を呼ばずに既定を決め（`internal` の既定は画面の側、`writableDefault.ts:49-101`）、`plc` タグは `virtual` の下に置けない（`tag.rs:916-935`）ので、この枝に届く `plc` の要求は無い |

- R2b-2 の後、Hub の core の非テストのコードに `Address::parse(`・`parse_slmp(`・アドレスの解析を選ぶ `"modbus-tcp"` が無いことを grep のテストで固定する（接続テスト `rest/plc_connections.rs:909` ほかの固定のリテラルは HostBound の接続テストで、R2b-3 まで例外の一覧に載せる）。

### 4.2 要求の型と HostBound の橋（Q3）

```text
保存: text ──parse_address(kind)──▶ canonical（DB）
計画・即時読み書き: canonical ──parse_address(kind)──▶ TagSpec { DriverAddress, TagKind }
  HostBound（modbus-tcp・slmp、R2b-3 まで）: TagSpec ──橋──▶ banto_plc::Address → ReadRequest / BatchReadRequest / BatchWriteRequest
  R2b-3 の後: TagSpec をそのまま FrameDriver::plan / Session::read_batch へ
```

- **橋**は `banto-plc` の 1 つの関数 `Address::from_driver_address(&DriverAddress) -> Result<Address, PlcError>`（`driver` が `modbus-tcp` なら `parse`、`slmp` なら `parse_slmp`、それ以外は `UnsupportedDriver`）。プロトコル名を知る場所はここだけになり、HostBound と一緒に R2b-3 で消える（Q3）。`DriverAddress` を作ったドライバと同じ文法で戻すので、冪等（C1）により失敗しない。失敗するのは手で書き換えた行だけ。
- **収集の計画**（`banto-collect`）: レジストリつきの `build_config_from_with_registry`（Hub）は、`build_request` の前に `registry` の `parse_address` で解析し（失敗は今の `ExclusionReason::InvalidTag`、文言はドライバの `AddressError`）、`TagSpec` を橋で `ReadRequest` に戻す。`ClientSpec`・`ProtocolConfig` は今のまま `Protocol` を持つ（HostBound の接続の組み立てに要る。R2b-3 で消える）。レジストリを持たない組み立て（ChronoGazer の `build_config_from`・`build_config_lenient_from`・`check_tag_address`）は変えない。
- **束ね**: 領域・語数・オフセットによる一括読み取りの束ねは今の `banto_plc::planning`（`Address` の領域で束ねる）のまま。ホスト（`banto-collect`・Hub）は `DriverAddress.area`・`word_count` を束ねに使わない（それはドライバの計画の内側。R2b-3 の `FrameDriver::plan`）。`area`・`writable_area` は表示と書き込み可の判定だけに使う。
- 毎周期の再解析は無い: 計画は catalog の組み立て（変更のとき）に一度だけ作る。即時読み書きは 1 要求ごとに 1 回（今の `Address::parse` と同じ回数）。

## 5. DB

- スキーマは変えない。migration は足さない（`tags.address` は `TEXT NOT NULL`、正規形も文字列）。`PlcConnection` の列も変えない（checklist §5 の `load_connection` の項は当たらない）。
- 既存の行の書き換え・照合はしない（§0.1）。R2b-2 の前に作った開発用の DB の綴り（`40001`・`d100`）は、どの解析でも同じアドレスに読めるので動く。綴りが正規形になるのは、その行を保存し直したときだけ。文法の誤った行（無効・`string` のタグ、§1.3）は計画で `InvalidTag` として外れ（今と同じ）、即時読み書きは 422。直すには保存し直す（その時点で断られる）か、DB を作り直す。
- 値の層の記録（`banto-tstore` の列名・OPC UA・MQTT の外部名）はタグの名前（`conn.group.tag`）で、アドレスを含まないので影響しない。

## 6. エラーの分類（Q5）

| 段                           | R2b-2 の後                                                                                                                                                                                |
| ---------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 保存                         | `parse_address` の `AddressError` → `address` の項目エラー（422）                                                                                                                         |
| 計画（catalog の組み立て）   | 解析の失敗 → そのタグだけ `ExclusionReason::InvalidTag`（寛容版）／保存前の検証（厳格版）では断る（今と同じ）                                                                             |
| 即時読み書き                 | 解析・橋の失敗 → `InvalidAddress` の 422。broker へ渡さない（書き込みは監査に `failed` の行が残る、§4.1）                                                                                 |
| 接続の後（broker・ドライバ） | `PlcError::InvalidAddress` は起きない。`classify_read` / `classify_write` は `ItemError::Unsupported` に写す（非致命）。`banto_plc::PlcError::is_connection_fatal` の非致命の一覧にも足す |

- 非致命にする理由: 接続の後に届くとしても 1 つのタグの設定の誤りで、同じ接続の他のタグのソケットを切る理由にならない（checklist §5「1 件の事故で全体を止めない」）。今は経路が無いので振る舞いは変わらない。
- 適合テスト C4 の「R1 の `LegacySession` では接続後の `InvalidAddress` が `FatalError::InvalidConfig`」を「`ItemError::Unsupported`」に直す。`banto-driver-api/src/error.rs:119` の doc も直す。

## 7. 画面・API・MCP・CSV・設定パッケージ

### 7.1 解析の API（Q6）

```text
POST /api/tags/parse-addresses        （viewer 以上・副作用なし。/api/v1 には載せない）
  { "connectionId": 3, "items": [ { "tagKind": "plc", "address": "40001", "dataType": "u16", "stringLength": null, "stringEncoding": "utf8" } ] }
→ { "items": [ { "ok": true, "canonical": "400001", "area": "holding register", "writableArea": true, "bit": null, "wordCount": 1 }
             | { "ok": true, "canonical": "temperature", "area": null, "writableArea": null, "bit": null, "wordCount": null }   // db など、ドライバを呼ばない種類
             | { "ok": false, "message": "..." } ] }
```

- 接続の id から `protocol` を引き、§3.2 と同じ関数で解析する（保存と同じ結果）。件数の上限は一括作成と同じ。
- **`tagKind` は必須**で、保存と同じ種類ごとの規則を当てる（2026-10-22 のオーナーのレビュー P2）。保存の層の正規化は 1 つの関数 `banto_tags::normalize_tag_address(protocol, &TagInput, registry) -> Result<String, FieldError>` にまとめ、保存（§3.2）と本 API の両方がそれを呼ぶ:
  - `plc`: §3.2 の 1〜4（ドライバの `parse_address`。`push` 接続の下の `plc` タグもここで `validate_push_key` を通る）。
  - `db`: 今の `banto-tags` の規則（結果列名 `is_db_column_identifier`、`postgres` 接続の下だけ）。`canonical` は前後の空白を除いた列名。ドライバの `parse_address` は呼ばない。
  - `computed` / `internal`: 空でなければ断る（今の規則）。`canonical` は `""`。
  - 配置の誤り（`postgres` の下の `plc` など）は保存と同じ文言で `ok: false`。
- `area`・`writableArea`・`bit`・`wordCount` は `plc` のときだけ値を持ち、それ以外は `null`。
- `GET /api/tags/address-writable` はこれに置き換えて消す（使用者が無い前提、2026-10-22。`writableDefault.ts` の「判定不能なら既定を当てない」の契約は `ok: false` で保つ）。

### 7.2 見え方

| 面                                  | R2b-2 の後                                                                                                                                                                                                                                                                                                                                                                           |
| ----------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| タグのフォーム・一覧                | `40001` と入れて保存すると、応答と一覧は `400001`。入力の途中で正規形を見せるかは画面の判断（解析の API を呼べる）。保存の後にフォームを読み直す今の流れで正規形に揃う                                                                                                                                                                                                               |
| 連続登録・オフセットコピー          | TS が作った綴り（`40002`）のまま送り、保存で正規形になる。プレビューは TS の綴り（R2b-6 でドライバの `offset_address` に替える）                                                                                                                                                                                                                                                     |
| CSV の書き出し                      | DB の値（正規形）                                                                                                                                                                                                                                                                                                                                                                    |
| CSV の更新の分類（`tagCsvDiff.ts`） | 比べる前に CSV の `address` を解析の API で正規形にする（同じ接続の行をまとめて 1 回）。**全行を `tagKind` つきで送り**、`db`・`computed`・`internal` の行も保存と同じ規則で正規化する（CSV の側で種類による除外をしない。規則を TS に写さないため）。そうしないと `40001` の行が毎回「変更あり」になり、送るたびに revision と監査が増える。解析に失敗した行は今の `error` に入れる |
| MCP の `create_tag` / `update_tag`  | 形は変えない。`address` の説明に「保存時に正規形にする（Modbus は 6 桁 400001、SLMP は大文字）」を足す。応答は正規形                                                                                                                                                                                                                                                                 |
| 設定パッケージ                      | 形と版は変えない。書き出しは正規形、取り込みは REST を通るので正規形になる                                                                                                                                                                                                                                                                                                           |

## 8. ChronoGazer（Q7）

- 推奨: R2b-2 では変えない。`TagService::new`・`CollectionGroupService::new`・レジストリを持たない `banto-collect` の組み立て・`check_tag_address` は今の振る舞い（綴りのまま保存、`banto-collect` の解釈器で検証）。`banto-tags`・`banto-collect` の旧規則の経路を残すので、ChronoGazer のテストは変わらない。
- 前倒しする場合（Q7 の (b)）: ChronoGazer の host が `banto_drivers::builtin().registry`（`modbus-tcp`・`slmp`）を `with_registry` で注入し、`hub` 接続の下は今の規則のまま。ChronoGazer の core は `banto-drivers`（`banto-broker` を含む）への依存が増える。R2b-7 の `ALLOWED_PROTOCOLS` の整理と一緒にやる方が 1 回で済む。

## 9. テスト

| 種類                             | 内容                                                                                                                                                                                                                                                                                                                                                                                                                                                          | PR         |
| -------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------- |
| 正規形の表                       | Modbus `40001`・`400001`・`40001`（前後の空白）・`40001.03`・`40001.3`・`065536`、SLMP `d100`・`D0100`・`X0010`・`x1a`・`d100.a`、`push` の前後の空白: `canonical` が §1.4 の表どおり・`parse(canonical) == parse(text)`（冪等）・同じアドレスの別の綴りが同じ `canonical`（一意）                                                                                                                                                                            | R2b-2a     |
| 種類に依る断り（Q4）             | ビット指定 + `u16`、コイル + `f32`、SLMP + `i64`、Modbus + `string` を `parse_address` が断り、`bit` / 適合する組み合わせは通る。計画の側の `UnsupportedCombination` は守りとして残ることも 1 件                                                                                                                                                                                                                                                              | R2b-2a     |
| 橋                               | 全ての正規形の表の行で `Address::from_driver_address(parse_address(text)) == Address::parse*(text)`。別のドライバの `DriverAddress` は断る                                                                                                                                                                                                                                                                                                                    | R2b-2a     |
| 分類                             | `classify_read(InvalidAddress)` / `classify_write` が `ItemError::Unsupported`、`is_connection_fatal` が `false`。適合テスト C4 の更新                                                                                                                                                                                                                                                                                                                        | R2b-2a     |
| 保存（全入口）                   | REST の作成・更新・一括作成・一括更新、MCP、未適用キューの適用、CSV、設定パッケージの取り込みで `40001` / `d100` を送り、DB の行が `400001` / `D100`。文法違反は `address` の項目エラーで行が増えない。**無効のタグと `string` のタグも**断る                                                                                                                                                                                                                 | R2b-2b     |
| プロトコルの変更・グループの移動 | 配下に無効・`string` を含む Modbus のタグがある接続を `slmp` に変える → 一覧つきで断り、何も変わらない。読めるタグだけなら通り、`address` が新しい正規形になる。同じドライバの間の移動は書き換えない                                                                                                                                                                                                                                                          | R2b-2b     |
| 入口の grep                      | Hub の core の非テストに `TagService::new(`・`CollectionGroupService::new(`・`Address::parse(`・`parse_slmp(` が無い（例外の一覧つき）                                                                                                                                                                                                                                                                                                                        | R2b-2b・2c |
| 計画・即時読み書き               | 生の SQL で `address = 'D3000'` を Modbus の有効なタグに入れた DB: 寛容版の計画はそのタグだけ `InvalidTag`、他のタグは Good、即時読み取り・書き込みは 422 で broker に要求が届かず、書き込みの監査には `failed` の行が 1 つだけ残る（§4.1）                                                                                                                                                                                                                   | R2b-2c     |
| 既存のテスト                     | `40001` を保存して `40001` が返ると期待するもの（`banto-tags/src/tag.rs:2858`・`:2875` ほか、Hub の REST のテスト、E2E）を正規形に直す。直したテストの数を PR に書く                                                                                                                                                                                                                                                                                          | R2b-2b     |
| 画面                             | `tagCsvDiff` が `40001` と既存の `400001` を「変更なし」に分類（解析の API はテストで差し替えず、本物の HTTP ヘルパーを通す）。**`plc`・`db`（`postgres` の下の結果列名）・`computed`・`internal`（空のアドレス）を含む変更の無い CSV がすべて「変更なし」になり、`error` が 0 件**（§7.1 の種類ごとの規則の回帰）。解析の API の単体: 同じ 4 種類で保存と同じ結果（`normalize_tag_address` を共有していること）。E2E: タグを `40001` で作って一覧に `400001` | R2b-2d     |
| ChronoGazer                      | `cargo test -p chronogazer-core` が変わらず通る（旧規則の経路が残ること）                                                                                                                                                                                                                                                                                                                                                                                     | 各 PR      |

反証（checklist §5）: 保存の層の解析を外す → 「保存（全入口）」が落ちる、プロトコルの変更の解析し直しを外す → 「プロトコルの変更」が落ちる、`classify` の写しを戻す → 「分類」が落ちる、を各 PR で 1 回ずつ確かめて PR に書く。migration が無いので checklist §5 の migration の 2 項目は当たらない。

## 10. PR の分け方

共通: R2b-1 と同じ検証の順（`cargo fmt --all --check` → test → clippy → 公開 API を変える PR は `cargo check --workspace --all-targets`（checklist §2-3）→ TS を触ったら eslint・check・test → prettier → 新しいファイルは `ci-changes.test.mjs` → `cargo deny check`）。migration が無いので、どの PR も revert で戻る。

| PR     | 範囲                                                                                                                                                                                                                                                                                                               | 主な触るファイル                                                                                                                                                                                                        | 監査の焦点                                                                                                      | モデル                                |
| ------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------- | ------------------------------------- |
| R2b-2a | ドライバ側の部品（配線しない）: `parse_address` の種類に依る断り（Q4）、橋 `Address::from_driver_address`、`banto_tags::tag_kind`、`classify` と `is_connection_fatal` の `InvalidAddress`、適合テストの fixtures と C4                                                                                            | `crates/banto-driver-{modbus,slmp}/src/{base,classify}.rs`、`crates/banto-plc/src/{address,error}.rs`、`crates/banto-driver-conformance`、`crates/banto-driver-api/src/error.rs`（doc）、`crates/banto-tags/src/tag.rs` | 断りが今の計画の判定（`planning.rs`）と同じ集合か（表のテスト）。`banto-driver-api` は doc だけ                 | sonnet                                |
| R2b-2b | **保存の切り替え**: `TagService` / `CollectionGroupService` の `with_registry`、種類ごとの正規化 `normalize_tag_address`（§3.2・§7.1）、保存の値を `canonical` に、全タグの解析、プロトコルの変更・グループの移動の解析し直しと書き直し（同じトランザクション）、Hub の全入口、grep のテスト、既存のテストの期待値 | `crates/banto-tags/src/{tag,collection_group,plc_connection}.rs`、`apps/banto-hub/core/src/{runtime,mcp,hub,rest/*}.rs`、`apps/banto-hub/host/tests/registry_entrances.rs`、テスト多数                                  | 読んで書き戻す処理がトランザクションの中か（`BEGIN IMMEDIATE`、checklist §5）。入口の取りこぼし。断る一覧の文言 | 上位（実装は sonnet）                 |
| R2b-2c | **読む側**: `build_config_from_with_registry` の解析、`read_path`・`write_path` の 3 か所、`"modbus-tcp"` の分岐の削除、grep のテスト                                                                                                                                                                              | `crates/banto-collect/src/config.rs`、`apps/banto-hub/core/src/{read_path,write_path}.rs`                                                                                                                               | 書き込みの関門の順と解析の位置が変わらないこと（§4.1）。ChronoGazer の経路に触れていないこと                    | 上位（書き込みの安全）・実装は sonnet |
| R2b-2d | 画面と API: `POST /api/tags/parse-addresses`（`tagKind` 必須、`normalize_tag_address` を保存と共有）、`address-writable` の置き換え（`writableDefault.ts`）、`tagCsvDiff.ts` の正規形での比較、MCP の説明、E2E                                                                                                     | `apps/banto-hub/core/src/rest/tags.rs`、`apps/banto-hub/src/lib/banto/{tagCsvDiff,writableDefault,tagRegistryAdmin}.ts`、`tags/+page.svelte`、`mcp.rs`                                                                  | 判定不能を既定 ON に倒さないこと（`writableDefault.ts` の契約）                                                 | sonnet                                |
| R2b-2e | docs（本書・api-design §3.3 / §3.4 / §6.2・refactor-plan・plugin-design §12.4・MCP の手引き・運用ガイドのタグの節）、CHANGELOG と版の 4 点セット（Q8）                                                                                                                                                             | `docs/*`、`CHANGELOG.md`、版の 4 点                                                                                                                                                                                     | 「既存の行は書き換えない」の書き方                                                                              | haiku（docs）＋ 上位の確認            |

- 2a は独立。2b・2c は 2a の後で、互いに独立（どちらが先でも main は緑: 2c だけ先でも、綴りのままの行を `parse_address` は読める）。2d は 2b の後（保存が正規形になってから CSV の比較を直す）。2e は最後。
- 2b が最大（既存のテストの期待値の直しを含む）。期待値の直しはコミットを分けて、レビューで機械的な差分と本体を分けて見られるようにする（直しは haiku でもよい）。

## 11. オーナー判断

**2026-10-22 オーナー決定: Q1〜Q9 はすべて推奨どおり（「推奨で」）。**

**2026-10-22 オーナー決定（R2b-2c の実装中）**: 書き込みのアドレスの解析は、今のコードどおり関門 8 の監査行（`insert_pending`）の**後**に置いたままにする（関門の順を変えない）。初版の §4.1「解析は関門 8 の前」・§9「監査に書く前の行が残らない」は今のコードと合っていなかったので直した。解析に失敗した書き込みは 422 で broker へ届かず、監査に `failed` の行が残る（届くのは手で書き換えた行だけ）。

| Q   | 問い                                                           | 選択肢                                                                                                                                                                                                                                                        | 推奨                                                                                                                                                                                                                                              |
| --- | -------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Q1  | 保存時の解析の範囲                                             | (a) 全タグ（無効・`string` を含む）を保存の層（`banto-tags` へ注入）で / (b) 今の保存前の検証（有効な数値のタグだけ）の延長                                                                                                                                   | **(a)**。保存で通ったものは収集でも読める、を全タグで成り立たせる。今の漏れ（§1.3）を閉じる                                                                                                                                                       |
| Q2  | 接続のプロトコルの変更・グループの別のドライバへの移動         | (a) 配下の全タグを新しいドライバで解析し直し、1 件でも通らなければ一覧つきで断る。通れば `address` を新しい正規形に書き直す / (b) 断らず、計画で外す（`InvalidTag`）                                                                                          | **(a)**。ChronoGazer の保存時の検証と同じ形（§1.6）。(b) は保存が通って収集で黙って外れる                                                                                                                                                         |
| Q3  | 要求の型の切り替えの範囲と HostBound の橋                      | (a) ホスト側の入口だけ `TagSpec`、broker・`PlcClient` は R2b-3 まで `Address`。橋は `banto-plc` の 1 関数 / (b) (a) で橋を broker の `DriverTable` の欄（ドライバ crate が提供）に / (c) R2b-2 で broker・`PlcClient` も `TagSpec` に（api-design §4.4 の B） | **(a)**。(c) は R2b-3 で置き換わる場所を二度書く。(b) はプロトコル名を banto-plc から消せるが、ChronoGazer（`DriverTable` を持たない）の経路に別の橋が要る。橋は R2b-3 で HostBound と一緒に消える                                                |
| Q4  | 種類（データ型）に依る断りを `parse_address` へ移すか          | (a) 移す（ビット指定 + `bit` 以外・領域とデータ型の不整合・SLMP の 64 bit・Modbus の `string`）。計画の側の判定は守りとして残す / (b) ビット指定だけ移す / (c) 移さない                                                                                       | **(a)**。保存の時点で分かる誤りを毎周期の Bad にしない。`banto-tags` のプロトコル名の分岐（`MODBUS_ONLY_DATA_TYPES`・`modbus_read_only_area`）を Hub で使わなくなる。適合テスト C4 の「型と領域の不整合は `Unsupported`」は接続後の守りとして残る |
| Q5  | `InvalidAddress` の分類                                        | (a) 非致命（`ItemError::Unsupported`、`is_connection_fatal` も）/ (b) 致命のまま（経路が無いことをテストで固定）                                                                                                                                              | **(a)**。api-design §3.4 の「R2b-2 で変える」どおり。1 つのタグの誤りでソケットを切らない                                                                                                                                                         |
| Q6  | 画面・CSV の正規形の扱い                                       | (a) `POST /api/tags/parse-addresses` を足し、CSV の分類はそれで比べ、`GET /api/tags/address-writable` を置き換えて消す / (b) (a) の API を足し、`address-writable` は残す / (c) API を足さず、CSV の「変更あり」の空振りを受け入れる                          | **(a)**。文法を TS に写さない（2026-09-02 の判断「アドレスのパースをサーバーに一本化」）。使用者が無い前提（2026-10-22）なので古い口を残す理由が無い                                                                                              |
| Q7  | ChronoGazer の追従の時期                                       | (a) R2b-7（今の計画）/ (b) R2b-2 で `builtin()` のレジストリを注入して正規形に                                                                                                                                                                                | **(a)**。ChronoGazer の core に `banto-drivers` への依存が増え、R2b-7 の `ALLOWED_PROTOCOLS` の整理と別々にやると 2 回触る。互換の理由は消えた（2026-10-22）ので、R2b-3 の I/O の共用のときに寄せてもよい                                         |
| Q8  | 版と CHANGELOG                                                 | (a) R2b-2 の全部（2e）の後に alpha を 1 つ上げる / (b) PR ごとに / (c) 上げない（R2b-3 とまとめる）                                                                                                                                                           | **(a)**（R2b-1 の Q13 と同じ）。CHANGELOG に「アドレスを正規形で保存（`40001` → `400001`）・保存時に全タグを検証・既存の行は書き換えない」                                                                                                        |
| Q9  | 利用者の綴りを残す列（api-design §3.3 の案 C、`address_text`） | (a) 足さない（案 C を閉じる）/ (b) 足す                                                                                                                                                                                                                       | **(a)**。オーナー決定（2026-10-22）で互換・移行のための仕掛けは作らない。表示の要望が出たら画面の側で入力の綴りを見せる                                                                                                                           |

## 12. 既存の文書の直し（R2b-2e で）

1. refactor-plan §4 R2b-2・§6 の「移行時に全件照合」、plugin-design §12.4 の同じ文: 2026-10-22 のオーナー決定で行わない、と書く。
2. api-design §3.3（「R2b-2 の移行の全件照合が使う」・案 C）・§3.4（`InvalidAddress` は致命のまま）・§6.2 の「`"modbus-tcp"` でアドレスの解析を選ぶ 4 か所」の行・§7.1 の C4: 本書の決定に合わせる。
3. `banto-driver-api/src/address.rs:1-10` と `crates/banto-driver-modbus/src/base.rs:1-10` の module doc（「R2b-2 の移行の照合」「本番の経路は通らない」）: R2b-2b・2c で直す。
4. `banto-tags/src/tag.rs:546-553` の「Address format is ... left to I2」と `banto-collect/src/config.rs:1700-1727` の「`banto-tags` deliberately does not validate this at registration time」: レジストリつきの経路では保存の層が検証する、と直す（旧規則の経路の説明として残す）。
