# banto-driver-api 詳細設計（R1: ドライバ API とレジストリ）

作成日: 2026-10-19
状態: **設計確定（2026-10-19 オーナー決定。§12 の Q1〜Q8 と §0.1 の再確認（P0-3 の 6 本目を含む）を決定済み）・未着手**。2026-10-19 レビュー指摘で書き込みの手順を状態機械に修正（§4.2.1。`encode_write` / `check_write` を `write_next` に置き換え、適合テストに C13〜C16 を追加）。同日、§3.3・§7.3・§12・§13 に残っていた「再確認待ち」「推奨」の記述を決定済みに揃えた。[banto-hub-driver-refactor-plan.md](banto-hub-driver-refactor-plan.md) §4 の R1 を実装できる粒度に落としたもの。R1 の各 PR は本書を指示書として使う。
対象: `banto-driver-api`（公開する型と trait）・適合テスト一式・組み立て役 `banto-drivers`・既存の Modbus TCP / SLMP の載せ替え・banto-hub のバイナリの package 分離・CI の判定。R2a（取り込み口 `push`）・R2b（I/O の一本化・アドレスの不透明化・SQL の `CHECK` の撤去）・W（WASM 本体）が **API を壊さずに** 載ることを設計の条件にする。
関連: [banto-hub-driver-refactor-plan.md](banto-hub-driver-refactor-plan.md)（段の順番・決定事項）、[banto-hub-driver-plugin-design.md](banto-hub-driver-plugin-design.md)（§2・§3.1・§3.2・§6 の WIT・§12、監査 B2 / M5 / M6。**§6.2 の WIT は本書 §9.2 と同じ PR で直す**）、[banto-hub-external-db-design.md](banto-hub-external-db-design.md) §9.5（`push` はセッション型）、[tag-server-design.md](tag-server-design.md) §3.4 / §3.4.1 / §6（書き込みの安全）、[h9-slmp-structured-error-spec.md](h9-slmp-structured-error-spec.md)（構造化エラー）、[implementation-checklist.md](implementation-checklist.md)（PR の手順）。

---

## 0. 要約（決めたこと）

| #   | 決めたこと                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              | 根拠の節       |
| --- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------- |
| 1   | `banto-driver-api` は **型と trait だけ**。必須の依存はゼロ（`serde` は feature で任意）。**`banto-plc` から移す型（`DataType`・`TagValue`・`Value`・`StringEncoding`・`WordOrder`）は `#[non_exhaustive]` にしない**（今の呼び出し側が網羅的に `match` している）。API で新しく作る型だけ `#[non_exhaustive]` にし、全部にコンストラクタを付ける。trait の追加メソッドは既定実装つき                                                                                                                                   | §3.1, §3.2, §9 |
| 2   | 適合テスト一式は **別 crate `banto-driver-conformance`**（API crate の feature ではない）。API には `{ path, version }` で依存し、両方を `cargo publish --dry-run` で 1 回に検証する                                                                                                                                                                                                                                                                                                                                    | §2.2, §7       |
| 3   | trait は WIT と同じ形の **フレーム型**（`FrameDriver` / `FrameConnection`、I/O を持たない）と **セッション型**（`SessionDriver` / `Session`、I/O を自分で持つ）の 2 種。共通の静的な部分（識別・設定の検証・アドレス関数）は `DriverBase`。`decode_read` / `write_next` は **元の `specs` / `items` を受け取る**（同じフレームを U32 と F32 で読み分けるため）。**書き込みは `write_next` の状態機械**（直前の応答を受けて次の step か最終結果を返す。SLMP のワード内ビットの RMW のため）、読み取りは 1 回の計画のまま | §4, §4.2.1, §5 |
| 4   | R1 の Modbus / SLMP は **フレーム型にもセッション型にもしない**。`DriverBase` だけを API の形で実装し、I/O は broker の **`DriverTable`（接続 → `Connector` を作る関数の表）** を組み立て役から注入する（`DriverKind::HostBound`、R1 限定）。`spawn` と `TaskEntry` は broker に残す（外から実装できないため）                                                                                                                                                                                                          | §4.4           |
| 5   | R1 の要求の型は今の `banto_plc::Address` のまま。API には不透明な `DriverAddress`（WIT の `parsed`）を **並走で**入れ、R2b-2 で要求の型を切り替える。`station` は持たせない（マルチドロップの局番は接続設定。§8）                                                                                                                                                                                                                                                                                                       | §3.3, §8       |
| 6   | `DriverRegistry` は R1 で作るが、R1 で正にするのは **broker の `DRIVERS` と `is_supported_protocol` だけ**（振る舞い不変で差し替え。`#[deprecated]` は使わず同じ PR で呼び出し側 4 か所を直す）。`ALLOWED_PROTOCOLS`・SQL の `CHECK`・`BrokerSimRegistry::resolve`・UI は R2b                                                                                                                                                                                                                                           | §6             |
| 7   | banto-hub のバイナリは `apps/banto-hub/host`（package `banto-hub-host`）へ分ける（R1-7a）。**統合テストの移動（旧 R1-7b）は R1 に含めず R2b の冒頭（R2b-0）にする**（core の `src` 内の単体テストも実ドライバを使うため、R1 の「振る舞いを変えない」範囲に収まらない）。ChronoGazer の host 分離と `EngineConfig` への注入も R2b                                                                                                                                                                                        | §2.4, §10      |
| 8   | R1 の CI の短縮は **ドライバの薄い crate と組み立て役の変更に限る**。`banto-plc` の中身（SLMP / Modbus の実装）は R2b-3 までホストの core が直接使うので、その変更は今までどおり core に届く。core 自身が対象から外れるのも R2b-0 の後                                                                                                                                                                                                                                                                                  | §2.5           |
| 9   | 版は `0.1.0` → `0.2.0` → …（0.x の minor = 破壊）。alpha は使わない。CI に `cargo-semver-checks` を足す                                                                                                                                                                                                                                                                                                                                                                                                                 | §9             |

### 0.1 監査で推奨を変更・オーナー再確認（2026-10-19 オーナーがすべて「監査後の推奨」で承認）

オーナーは 2026-10-19 に §12 の Q1〜Q7 を「推奨で」と決めた。その後の監査で、次の項目は推奨そのものを変えた（または新しく問いが出た）。**2026-10-19 にオーナーが再確認し、すべて「監査後の推奨」の列のとおりに決定した**。

| 項目                       | オーナーが承認した推奨（2026-10-19）                                                     | 監査後の推奨                                                                                                                                                                | 理由                                                                                                                                                                                                                                                                                                                                   |
| -------------------------- | ---------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Q4（R1-7b）                | 7a / 7b に分け、7b（統合テストの移動）も R1 に含める                                     | **7b は R1 から外し、R2b-0 として R2b の冒頭に置く**。R1 の CI の短縮はアダプター crate の変更に限る（§2.5）                                                                | core の `src` 内の単体テスト（`broker_glue.rs:904-940` ほか、`ensure_connection(` が `broker_glue.rs` に 7 か所・`hub.rs`・`rest.rs` に各 2 か所）が `Simulator::start` + 実ドライバで動く。`test_support` は `#[cfg(test)] pub(crate)`（`lib.rs:229-230`）で外から使えない。「移動だけ」では済まず、core の単体テストの書き換えが要る |
| Q6（`station`）            | `DriverAddress.station: Option<u32>` を R1 の型に入れる                                  | **入れない**。マルチドロップの局番は接続設定（`keys::UNIT_ID` / `STATION`）で持ち、1 ポートの複数局はエンジンのポート共有で扱う（§8）                                       | 局番を「アドレス」と「ポートの多重化」の 2 か所でモデル化すると、`covers` の分割規則とエンジンの直列化の両方に局番が出て二重になる（監査 M5）。WIT の `parsed` にも無い                                                                                                                                                                |
| M7（ChronoGazer）          | §2.4 で `chronogazer-host` を R1-7a で分け、`EngineConfig.client_factory` を R1-3 で注入 | **両方を R2b へ送る**。R1 の `builtin().client_factory` は `banto_collect::default_client_factory()` そのもの（写しを作らない）なので、ChronoGazer 側は R1 で何も変わらない | `ClientSpec` の組み立てを写すと #334 / #605 と同じ落とし方をする（`task.rs:248-304`、`:385-409`）。ChronoGazer に入れる理由が R1 には無い                                                                                                                                                                                              |
| Q3（版の表記）             | R1 完了直後に `0.1.0-alpha` を公開                                                       | **`0.1.0` を公開**（alpha を使わない）。R2b-3 で `HostBound` を消して `0.2.0`                                                                                               | `^0.1.0-alpha.1` の利用者は `0.1.0-alpha.2` の破壊を自動で踏む。0.x の minor を破壊の単位にすれば caret で守れる（監査 M1）                                                                                                                                                                                                            |
| **Q8（新規）**             | -                                                                                        | Modbus / SLMP の `canonical` を **今の `Address` の `Display`（Modbus は 6 桁 `400001`、bit は先頭ゼロなしの `.3`）に揃える**（§3.3 の案 A）                                | `Address::parse` は `40001` / `400001` / `40001.03` を同じ `Address` に読む（`address.rs:199-215`、bit の接尾辞は 10 進・最大 2 桁 `:78-84`）。「入力を正規形にする」案では 1 つの `Address` に複数の正規形ができ、C1 と R2b-2 の照合が成り立たない                                                                                    |
| **P0-3 の 6 本目（新規）** | -                                                                                        | refactor-plan §8.3 の P0-3 に「SLMP の応答が分割して届いても 1 つの応答として読む」を足す                                                                                   | P0-3 は v0.2.0 では落ちるので SLMP の分割受信を外した。本ブランチの v0.2.1 で通るようになった。R2b-3 の判定基準は P0-3 が正なので、R1-5 の C5 だけに任せない（§13 の 9）                                                                                                                                                               |

## 1. 前提（コードの現状、2026-10-19 に照合）

### 1.1 今の境界と型

| 層                 | 場所                                                                                                                                                                                                                                                                                                                                                                                                            | 形                                                                                                                                                                                                                                                                                              |
| ------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 読み取りの trait   | `crates/banto-plc/src/client.rs:56-79` `PlcClient { connect, read_batch(&[ReadRequest]), disconnect }`、`BoxFuture` は同 `:47`                                                                                                                                                                                                                                                                                  | 接続ごと 1 インスタンス、`Err` = 接続致命                                                                                                                                                                                                                                                       |
| 書き込みの trait   | `crates/banto-plc-write/src/client.rs:46-78` `PlcWriteClient`                                                                                                                                                                                                                                                                                                                                                   | 同上                                                                                                                                                                                                                                                                                            |
| broker の境界      | `crates/banto-broker/src/session.rs:70-92` `pub(crate) trait BrokerSession { read_batch(&[BatchReadRequest]), write_batch(&[BatchWriteRequest]), disconnect }`、`SessionError(String)` 同 `:102-103`、`Connector` 同 `:126-128`                                                                                                                                                                                 | `pub(crate)`。登録表 `DRIVERS: &[(&str, DriverSpawn)]` は `lib.rs:743-753`、`is_supported_protocol` は `:798-800`、`ensure_connection` の引き当ては `:840-848`                                                                                                                                  |
| broker の内部      | `TaskEntry`（`lib.rs:731-734`、private）、`spawn_task_with_connector`（`:1054`、private）、`spawn_slmp_driver`（`:760-771`）が `handle.slmp_cpu` に書く（`:769`、field の定義は `:475`、private）、SLMP 専用の `spawn_task`（`:1097`、private、`slmp_driver::connector` 決め打ち）、`slmp_config_for`（`:275`）/ `modbus_config_for`（`:326`）は `lib.rs` の private 関数で、その単体テストは `:1509` / `:1524` | **`DriverSpawn` を外から実装することはできない**（§4.4）。`examples/real_plc_verify.rs` は `:41` で `banto_broker::BrokerSupervisor` を import し `:879` で `spawn` する（実機の検証用）                                                                                                        |
| 共通の型           | `crates/banto-plc/src/types.rs` `DataType :29-40`（`register_span :69-76` は `pub(crate)` の固有メソッド、`Display :79-93`）、`ReadRequest :99-103`、`TagValue :117-121`、`StringEncoding :179-186`、`StringReadRequest :196-208`、`BatchReadRequest :214-218`、`PlcValue :230-238`、`From<TagValue> for PlcValue` と `as_tag_value :240-259`、`BatchReadResult :263-267`                                       | `ReadRequest` / `BatchReadRequest` は `Copy`                                                                                                                                                                                                                                                    |
| 網羅的な `match`   | `DataType`: `banto-plc/src/decode.rs:66-100`、`banto-plc-write/src/encode.rs:192`、`apps/banto-hub/core/src/write_path.rs:408-444`。`WordOrder`: `decode.rs:34-50`。`StringEncoding`: `encode.rs:303`。`TagValue::` を使うファイルは 23、`PlcValue::` は `banto-plc` の外で 6（例 `rest.rs:7736`）                                                                                                              | 移す型を `#[non_exhaustive]` にすると、これらが別 crate からの網羅的 `match` として壊れる（§3.1）                                                                                                                                                                                               |
| アドレス           | `crates/banto-plc/src/address.rs:93-130` `Address { ModbusRef{area,offset,bit} \| Slmp{device,number,bit} }`（`Copy`）、`parse :168`、`Display :278-299`（Modbus は常に 6 桁）、bit の接尾辞は `:78-84`（Modbus: 10 進・最大 2 桁）、`AddressArea` は `crates/banto-plc-address/src/lib.rs:42-53`、`is_writable` 同 `:75-77`                                                                                    | 2 つだけの enum。外から足せない（tag-server-design §3.4.1）                                                                                                                                                                                                                                     |
| エラー             | `crates/banto-plc/src/error.rs:21-125` `PlcError`、致命 / 非致命の切り替えは `is_connection_fatal :145-154`（既定は致命）。書き込み側は `crates/banto-plc-write/src/error.rs:25-204` `PlcWriteError`、`is_connection_fatal :240-256`                                                                                                                                                                            | 読み取りの非致命 = `ModbusException`・`SlmpEndCode`・`UnsupportedCombination`・`AddressProtocolMismatch`・`StringSpanUnsupported`。書き込みはさらに `ValueTypeMismatch`・`ValueOutOfRange`・`ConflictingBitWrite`・`BitWriteVerificationFailed`・`ModbusReadOnlyArea`・`UnsupportedRequestKind` |
| SLMP の構造化      | `crates/banto-plc/src/slmp/mod.rs:305-340` `SlmpErrorClass` / `classify_slmp`。ワイヤ層は `tyaro/slmp` **v0.2.1**（ルート `Cargo.toml:121`。13 バイトのヘッダを `read_exact` で読み、宣言長ちょうどを 1 つの `recv_timeout` の中で読む。分割受信は解消）                                                                                                                                                        | h9 の受け入れ済み（文言パースなし）。テスト用シミュレータの「壊れたフレーム」は長さではなく応答コードを偽る形（`b53111c`）                                                                                                                                                                      |
| 収集の差し込み口   | `crates/banto-collect/src/task.rs:341` `ClientFactory = Arc<dyn Fn(&ClientSpec) -> Box<dyn PlcClient> + Send + Sync>`、`ClientSpec :271-304`、`ClientProtocol :248-251`、`default_client_factory :385-409`。`Collector::start_with_client_factory` は `collector.rs:363-377`、`start_with_sources` は `:379-389`                                                                                                | `ClientSpec` は手で保守する写し（#334 / #605 の罠が doc に書いてある）。**写しを増やさない**                                                                                                                                                                                                    |
| 収集のプロトコル   | `crates/banto-collect/src/config.rs:84-87` `pub enum Protocol { ModbusTcp, Slmp }`、`pub(crate) enum ProtocolConfig { ModbusTcp, Slmp, External } :111-115`、`build_request :1457-1477`（`Protocol` で `Address::parse` / `parse_slmp` を選ぶ）                                                                                                                                                                 | banto-hub が `Protocol` を網羅的に `match` する                                                                                                                                                                                                                                                 |
| 外部ソース         | `crates/banto-collect/src/external.rs:151-153` `pub trait ExternalSource: Send + Sync { fn snapshot(&self) -> ExternalSnapshot }`                                                                                                                                                                                                                                                                               | ChronoGazer の `hub` 接続。R2a の `push` と同種                                                                                                                                                                                                                                                 |
| Hub の相乗り       | `apps/banto-hub/core/src/broker_glue.rs:834-873` `hub_client_factory`（SLMP・Modbus とも `BrokerReadClient`、fallback は `:837` の `default_client_factory()`）、`BrokerSimRegistry::resolve :704-752`（`:716-720` は `if conn.protocol == "modbus-tcp" { .. } else { Slmp }` で、網羅的 `match` ではない）                                                                                                     | #337 の 1 接続 = 1 ソケット                                                                                                                                                                                                                                                                     |
| 文字列で分岐       | `apps/banto-hub/core/src/write_path.rs:1634-1637, 1782-1783`、`read_path.rs:264-265`（`"modbus-tcp"` 以外を `parse_slmp`）、`is_supported_protocol` の呼び出しは `hub.rs:1989`・`read_path.rs:180`・`rest.rs:8142`・`write_path.rs:676`（**4 か所とも `manager`（`CollectorManager`）が手元にあり、`manager.sessions()`（`hub.rs:1265`）が引ける**）                                                            | plugin-design §3.2                                                                                                                                                                                                                                                                              |
| レジストリの固定値 | `crates/banto-tags/src/plc_connection.rs:236` `ALLOWED_PROTOCOLS`、`migrations/0019_plc_connections_allow_hub.sql:61` の `CHECK`、`tag.rs:37` `ALLOWED_DATA_TYPES`、`tag.rs:451` `modbus_read_only_area`、`tag.rs:966`（64bit 型は Modbus のみ）                                                                                                                                                                | SQL の `CHECK` の撤去は R2b-1                                                                                                                                                                                                                                                                   |
| アドレスの補助     | 連続登録・オフセットコピー・アドレスの説明は **TS にしか無い**: `apps/banto-hub/src/lib/banto/continuousRegistration.ts`・`offsetCopy.ts`・`tagAddressHelp.ts`（それぞれ `*.test.ts` あり）                                                                                                                                                                                                                     | `DriverBase::offset_address` / `address_help` の R1 の実装はこれを Rust へ移植し、TS のテストを fixtures にする（§10 R1-4b）                                                                                                                                                                    |
| ホストのバイナリ   | `apps/banto-hub/core/src/bin/banto-hub.rs`（145 行）・`banto-hub-elev.rs`（124 行）・`banto_hub/win_service.rs`。`banto-hub-elev` の `requireAdministrator` マニフェストは `core/build.rs:62-82` の `embed_resource::compile_for("banto-hub-elev.rc", ["banto-hub-elev"], ..)` と `core/banto-hub-elev.{manifest,rc}`、`core/Cargo.toml:139-146` の `embed-resource`                                            | どちらも core と同じ package。ChronoGazer（`banto-serve`・`chronogazer-server`）は R1 では分けない（§0.1 M7）                                                                                                                                                                                   |
| テストの補助       | `apps/banto-hub/core/src/lib.rs:229-230` `#[cfg(test)] pub(crate) mod test_support;`、`apps/banto-hub/core/tests/common/`                                                                                                                                                                                                                                                                                       | `test_support` は crate の外から使えない                                                                                                                                                                                                                                                        |

プロトコル名の文字列で分岐・固定する箇所は、refactor-plan 付録 A（P0-2 の棚卸し）が正。本書の数字は使わず、着手時に次で再確認する: `rg -l '"modbus-tcp"|"slmp"' --type rust --type ts --type svelte apps crates | rg -v '(tests?|__tests__|\.test\.)' `。

### 1.2 依存グラフ（今。`cargo metadata --no-deps` で確認）

```
banto-plc-address ─┬─ banto-tags ─────────┬─ banto-collect ─┬─ chronogazer-core ── chronogazer / chronogazer-elev
                   └─ banto-plc ─┬────────┤                 └─ banto-hub-core ── banto-hub-shell
                                 │        └─ banto-broker ──── banto-hub-core
                                 └─ banto-plc-write ─ banto-broker
banto-hub-core は banto-plc・banto-plc-write・banto-broker・banto-collect を直接も使う
banto-hub-sink は banto-hub-core（と broker・collect）を dev-dependency で使う（apps/banto-hub-sink/Cargo.toml:66-72。Hub をプロセス内に建てる）
```

- `banto-plc` に SLMP / Modbus の実装と共通の型が同居しているので、ドライバの中身だけ変えても `banto-hub-core`（統合テストのバイナリ約 20 本）と `chronogazer-core` が作り直される（plugin-design §12.6）。
- **CI の判定（`.github/scripts/ci-changes.mjs:214-240` `graphFromMetadata`）は normal・dev・build の依存をすべて辺にする**。dev-dependency で core がドライバを使う限り、ドライバの変更は core に届く。R1-7 の設計はこれを前提にする（§2.4）。

### 1.3 公開可能性

- ワークスペースは `publish = false`（ルート `Cargo.toml` の `[workspace.package]`）。公開する crate は自分の `Cargo.toml` で `publish = true` と独自の `version` を持つ。
- `banto-plc` / `banto-plc-write` / `banto-broker` は git 依存 `slmp`（ルート `Cargo.toml:121`、`tag = "v0.2.1"`）を持ち、`banto-tags` / `banto-collect` は `banto-core` / `banto-storage`（git）を持つ。**git 依存を持つ crate は crates.io に公開できない**ので、公開する crate はこれらに依存してはならない。path 依存は `{ path = "..", version = ".." }` の形なら公開できる（公開時に path が落ち version だけ残る）。`banto-plc-address`（依存ゼロ）だけは公開できるが、R1 では API crate に取り込まず再輸出もしない（§3.3）。
- cargo-deny の `[sources]`（`deny.toml:102-118`、`required-git-spec = "tag"`）は変えない。

## 2. crate / package の構成（R1 後）

### 2.1 `banto-driver-api`（`crates/banto-driver-api`、公開する）

```toml
[package]
name = "banto-driver-api"
version = "0.1.0"              # ワークスペースの版と切り離す（§9）。alpha は使わない
edition = "2021"
rust-version = "1.85"          # 暫定。§9.1 の MSRV ジョブが通る最も低い版に R1-1 で確定する
license = "MIT"
publish = true
description = "banto-hub / ChronoGazer のドライバの境界: 型と trait だけ"
repository = "https://github.com/tyaro/banto-industrial"
[dependencies]
serde = { version = "1", features = ["derive"], optional = true }
[features]
serde = ["dep:serde"]
```

- 置くもの: §3 の型、§4・§5 の trait、§6 の `DriverRegistry`（`BTreeMap` の引き当てだけ）、`BoxFuture`（`std` だけで書ける: `Pin<Box<dyn Future<Output = T> + Send + 'a>>`、`banto_plc::BoxFuture` と同じ定義）。
- 置かないもの: ロジック（計画・デコード・ワード順の実装）、I/O、tokio、`thiserror`（`Display` は手で書く）、`banto-plc` 系への依存、`banto-plc-address`（`AddressArea` は Modbus の語彙なので API には入れない。§3.3）。
- 検証: `cargo publish --dry-run -p banto-driver-api -p banto-driver-conformance`（R1-2 以降。R1-1 の時点では `-p banto-driver-api` だけ）。

### 2.2 `banto-driver-conformance`（`crates/banto-driver-conformance`、公開する）

**判断: API crate の feature ではなく別 crate にする（Q1、決定）。** 理由:

1. 適合テストは tokio のランタイム・モックのピア（TCP のリスナー・分割送信・遅延応答）・`assert` の補助を持つ。これを feature で API crate に入れると、API crate の `Cargo.toml` に tokio が（optional でも）並び、「型と trait だけ・依存ゼロ」（plugin-design §12.6 の条件 1）が崩れる。
2. 適合テストは API より頻繁に育つ（新しい振る舞いの固定を足す）。同じ crate だと適合テストの追加のたびに API の版が動く。別 crate なら `banto-driver-conformance` の版だけ上げればよい。

依存: `banto-driver-api = { path = "../banto-driver-api", version = "0.1.0" }`（**`path` と `version` の両方**。これで公開できる。§1.3）・`tokio`（`rt`, `time`, `net`, `io-util`, `macros`）。conformance 単独の `cargo publish --dry-run` は API が crates.io に無い間は失敗するので、**検証は必ず 2 パッケージ同時の `cargo publish --dry-run -p banto-driver-api -p banto-driver-conformance`**（複数パッケージの publish は cargo 1.90 で安定。ピンは 1.98.1）。plan.md §4 R1-5 の「`banto-driver-api` に置く」はこの判断で読み替える。

### 2.3 ドライバの crate と組み立て役

| crate                                                 | 公開 | 持つもの                                                                                                                                                                                                                                                                                                                                                                                                                                                                                | 依存                                                                                                |
| ----------------------------------------------------- | ---- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------- |
| `crates/banto-driver-modbus`（`banto-driver-modbus`） | ×    | Modbus TCP の **R1 の薄いアダプター**: `DriverBase` の実装（識別・設定の検証・`parse_address` は `banto_plc::Address::parse` を包む・`offset_address` / `address_help` は TS からの移植）、`banto-broker` の `modbus_driver.rs`（448 行）と `modbus_config_for`（`lib.rs:326`）とそれらの単体テスト・`supervisor_hands_out_a_handle_per_modbus_tcp_connection`（`lib.rs:1601`）を**移したもの**（`ModbusSession` + `connector` + `dial`）。シミュレータは `banto-plc` の feature のまま | `banto-driver-api`、`banto-plc`、`banto-plc-write`、`banto-broker`、`banto-tags`（`PlcConnection`） |
| `crates/banto-driver-slmp`（`banto-driver-slmp`）     | ×    | 同じく SLMP: `slmp_driver.rs`（100 行）・`slmp_config_for`（`lib.rs:275`）とその単体テスト（`slmp_config_for_*`: `:1509, :1524`）と、broker の単体テストのうち SLMP の振る舞いそのものを見るもの（§4.4 の表）を移したもの。`parse_address` は `Address::parse_slmp`、`cpu`（#605）を設定に持つ。**`spawn_task`（`:1097`）は移さない**（private の `spawn_task_with_connector` と `TaskEntry` を返すので外では書けない。§4.4）                                                           | 同上（`slmp` は `banto-plc` 経由）                                                                  |
| `crates/banto-drivers`（`banto-drivers`）             | ×    | 組み立て役。`builtin() -> Builtin { registry: DriverRegistry, broker: banto_broker::DriverTable, client_factory: banto_collect::ClientFactory }`。**`client_factory` は `banto_collect::default_client_factory()` をそのまま返す**（`ClientSpec` の組み立ての写しは作らない。§0.1 M7）。トリップワイヤのテスト（§6.3）。`examples/real_plc_verify.rs` は broker からここへ移す。R2b 以降ここが「公式ドライバの一覧」になり、API の公開後は別リポジトリへ出す（plan §7 Q2）              | 上の 2 つ + `banto-driver-api` + `banto-broker` + `banto-collect`                                   |

- `banto-plc` / `banto-plc-write` / `banto-broker` / `banto-collect` は **R1 では残る**（型・計画・デコード・I/O の実装はそのまま）。変わるのは (a) 共通の型が `banto-driver-api` へ移って再輸出になる、(b) broker の登録表の「中身」がドライバ crate へ移って、表そのものは注入されるようになる、の 2 点。
- `banto-broker` の `BrokerSession` / `SessionError` / `Connector` は **`pub` にする**（ドライバ crate が実装するため。tag-server-design §3.4.1 の「別リポジトリでは作り切れない部分」の 1 つが解ける）。`TaskEntry`・`spawn_task_with_connector`・job ループは **private のまま**（§4.4）。
- 依存の向き: ドライバ crate → `banto-broker`（trait と `DriverDial`）。`banto-broker` はドライバ crate に依存しない（dev-dependency でもしない。循環と二重コンパイルを避ける）。broker 自身の単体テストはドライバを要らない偽の `Connector` で書く（§4.4）。

### 2.4 banto-hub のバイナリの package 分離（R1-7a）

| 新 package                                | 場所                              | 移すもの                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             | 依存                                                                                    |
| ----------------------------------------- | --------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------- |
| `banto-hub-host`（`apps/banto-hub/host`） | bin `banto-hub`・`banto-hub-elev` | `apps/banto-hub/core/src/bin/` 一式（`banto-hub.rs`・`banto-hub-elev.rs`・`banto_hub/win_service.rs`・`win_service_tests.rs`）。**`banto-hub-elev` のマニフェスト埋め込み一式**: `core/build.rs:62-82` の `embed_resource::compile_for` 部分、`core/banto-hub-elev.manifest`・`.rc`、`core/Cargo.toml:139-146` の `embed-resource`（`[build-dependencies]`）を host へ。core の `build.rs` には proto のコード生成だけ残す。`embed-ui` feature は host 側に置き `banto-hub-core/embed-ui` へ転送する | `banto-hub-core`、`banto-drivers`、`banto-service-control` 等（bin が今使っているもの） |
| `banto-hub-shell`（Tauri）                | 既存                              | 変更なし（R1-3b で `banto-drivers` を dependency に足し、`HubRuntime::start` に `builtin()` を渡している）                                                                                                                                                                                                                                                                                                                                                                                           |                                                                                         |
| ChronoGazer                               | -                                 | **R1 では分けない**（§0.1 M7）。`EngineConfig` への注入・`chronogazer-host` は R2b                                                                                                                                                                                                                                                                                                                                                                                                                   |                                                                                         |

- マニフェスト埋め込みの移動には **確かめるテスト**を付ける: `apps/chronogazer/elev/src/tests.rs:174` の `the_manifest_requires_administrator_and_the_rc_embeds_it` と同じ形（`include_str!` で manifest に `requireAdministrator` があり、`.rc` が manifest を指し、`build.rs` が `["banto-hub-elev"]` と `.manifest_required()` を含む）を `apps/banto-hub/host/src/` に置く。移動のときに埋め込みが落ちるのを機械的に止めるため。
- **R1-7a の時点では CI の対象は変わらない**: core は `banto-drivers` を dev-dependency で持ち続ける（`src` 内の単体テスト・`tests/` の統合テストが実ドライバで動くため）。`.config/nextest.toml:64`（`binary_id(banto-hub-core)` = lib のテスト）と `:68`（`package(banto-hub-core) & binary(soak)`）は **R1 では変えない**（どちらも core に残る）。`.github/workflows/ci.yml:502` の `cargo test -p banto-hub-core --test db_source` も同じ。`.github/scripts/ci-changes.test.mjs:146` の `HUB_RUST` には `-p banto-hub-host` を足す。
- **R2b-0（旧 R1-7b、R1 の外）**: `apps/banto-hub/core/tests/` のうち実ドライバを要るもの（推定で 20 本の大半）を `apps/banto-hub/host/tests/` へ移し、core の `src` 内の単体テストを偽ドライバ（§7.4）か `HubDrivers::none()` で書き直し、core の dev-dependency から `banto-drivers` を外す。`test_support` を `pub mod` にするか `tests/common/` へ寄せるかもここで決める。`nextest.toml:68` と `ci.yml:502` の package 名もここで直す。**core が CI の対象から外れるのはこの後**。

ビルドの入口で package 名を書いている箇所（R1-7a で全部直す）: `.github/workflows/ci.yml:604, 611, 631, 693`、`.github/workflows/opcua-interop.yml:49`、`scripts/build-release.ps1:68-69`、`apps/banto-hub/installer/src/main.rs:48, 150`、`package.json:20`（`dev_plc` は ChronoGazer なので変えない）、`AGENTS.md:57, 79, 91`、`apps/banto-hub/README.md:22, 27, 79`、`apps/banto-hub/vite.config.ts:37`、`apps/banto-hub/core/src/assets.rs:14`、`implementation-checklist.md` §2-7（`cargo build -p <core> --bin <bin>`）。着手時に `rg -n 'banto-hub-core' --glob '!target' --glob '!*.lock'` で再確認する。

### 2.5 R1 後の依存グラフと CI の見込み

```
banto-driver-api（公開）◀── banto-plc ◀── banto-plc-write ◀── banto-broker ◀── banto-hub-core
        ▲                     ▲                                   ▲
        │                     └── banto-collect ◀── chronogazer-core / banto-hub-core
        │
        ├── banto-driver-modbus / banto-driver-slmp（＝ 薄いアダプター。banto-plc・banto-broker に依存）
        │         ▲
        └── banto-drivers ◀── banto-hub-host / banto-hub-shell / banto-hub-sink(dev) / banto-hub-core(dev、R2b-0 まで)
banto-driver-conformance（公開）◀── banto-driver-modbus / -slmp の dev-dependency
```

| 変更したもの                                             | Rust の対象（R1 の後）                                                                                                                                                                                                                                                                 | E2E  |
| -------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---- |
| `crates/banto-driver-modbus` / `-slmp` / `banto-drivers` | そのドライバ・`banto-drivers`・`banto-hub-host`・`banto-hub-shell`・`banto-hub-sink`・**`banto-hub-core`（dev-dependency 経由。R2b-0 で外れる）**。`chronogazer-core` は入らない                                                                                                       | Hub  |
| `crates/banto-plc`（SLMP / Modbus の実装本体）           | 今までどおり 10 パッケージ超（core を含む）。**R1 では縮まない**（core が `banto_plc::SlmpCpu`・`address`・`planning`・`PlcValue` を直接使う: `rest.rs:3608, 4527, 7736`、`read_path.rs:251`、`write_path.rs:252, 376`）。縮むのは R2b-2 / R2b-3 で実装本体がドライバ crate へ移った後 | 両方 |
| `crates/banto-driver-api`                                | `banto-plc` が依存するので実質 `--workspace`（`RDEPS_FULL_RATIO` 0.8 に届く見込み）。API を動かさない方針（§9）と整合                                                                                                                                                                  | 両方 |

plugin-design §12.6 の「ドライバを変えたときの CI の対象はドライバ・組み立て役・バイナリだけ」は、R1 の時点では **アダプター crate の変更でも core が対象に残る**（dev-dependency）。R1 で得るのは「ChronoGazer に届かない」ことと、R2b-0 で core を外せる形。実装本体の分までそうなるのは R2b の後（Q2、決定）。

## 3. 型（`banto-driver-api::types`）

### 3.1 `banto-plc` から移す型（R1-1。再輸出で呼び出し側は無変更）

**移す型は `#[non_exhaustive]` にしない**（監査 B1）。理由: `banto-plc` の外に網羅的な `match` がある（`banto-plc-write/src/encode.rs:192` の `DataType`、`:303` の `StringEncoding`、`apps/banto-hub/core/src/write_path.rs:408-444` の `DataType`）。`#[non_exhaustive]` は定義 crate の外で網羅的 `match` を禁じるので、「呼び出し側は無変更」と両立しない。移す型を `#[non_exhaustive]` に変えるのは 0.x の minor（破壊）として R2b 以降に別途判断する。

| API の型                      | 元                             | 変更                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
| ----------------------------- | ------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `DataType`                    | `banto-plc/src/types.rs:29-40` | そのまま移す（`Bit`・`I16`〜`F64` の 9 つ、`parse`・`Display`）。**`register_span`（`:69-76`）は固有メソッドなので一緒に移し、`pub(crate)` → `pub` にする**（型が API に移ると `banto-plc` 側で固有メソッドを足せない。呼び出し側 `decode.rs:72`・`planning.rs:236`・`slmp/planning.rs:361` と `types.rs:296-304` のテストは名前を変えないので差分なし）。**`String` 変種は足さない**（`DataType::parse("string")` が `Some` になると `banto-collect/src/config.rs:1265` の除外の判定が変わる） |
| `TagValue`                    | `:117-121`                     | そのまま（`Copy` の数値 / bit）。R2b で `Value` に畳む候補                                                                                                                                                                                                                                                                                                                                                                                                                                      |
| `PlcValue` → API では `Value` | `:230-238`                     | `Value { Bit(bool), F64(f64), Str(String) }`。**`impl From<TagValue> for PlcValue` と `as_tag_value`（`:240-259`）も一緒に移す**（両方の型が API に移るので orphan rule で `banto-plc` には置けない）。`banto-plc` は `pub use banto_driver_api::Value as PlcValue` で名前を保つ                                                                                                                                                                                                                |
| `StringEncoding`              | `:179-186`                     | そのまま（`Utf8` / `ShiftJis`）                                                                                                                                                                                                                                                                                                                                                                                                                                                                 |
| `WordOrder`                   | `banto-plc/src/decode.rs:17`   | enum だけ移す（`HighLow` / `LowHigh`、`Default = HighLow`）。デコードの実装（`combine_u32` 等 `:34-50`）は `banto-plc` に残り、そこでの網羅的 `match` はそのまま動く。接続設定の共通キー `word_order` の語彙になる（§3.5）                                                                                                                                                                                                                                                                      |

`banto-plc` の `lib.rs:87-90` の `pub use types::{...}` はそのまま残し、中身を API からの再輸出に替える。`banto-plc-write/src/lib.rs:97` の再輸出もそのまま。これで `banto_plc::DataType` 等を使う全呼び出し側（`write_path.rs:252, 450`、`broker_glue.rs:169, 173` 等）は 1 行も変わらない。

### 3.2 API だけにある新しい型（WIT `types` の写し）

新しい型は `#[non_exhaustive]`（§9）。**`#[non_exhaustive]` の struct は定義 crate の外から struct リテラルで作れないので、全部にコンストラクタを付ける**（監査 B2。適合テスト crate とドライバ crate が作れなければならない）。

```rust
/// WIT: variant value { bool, int(s64), uint(u64), float(f64), bytes(list<u8>) }
/// フレーム型ドライバの decode が返す生の値。Hub（エンジン）が Value へ写す
/// （整数は f64 へ広げ、bytes は TagKind::Str の encoding で文字列へ）。
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub enum RawValue { Bool(bool), Int(i64), Uint(u64), Float(f64), Bytes(Vec<u8>) }

/// 読み書きの単位の「種類」。WIT の tag-spec.data-type / string-words に対応する（§9.2 の表）。
/// banto-plc の slmp::planning::ReadKind（:99-115）は Numeric / Str / BitInWord の 3 つだが、
/// API では BitInWord を variant にしない: ワード内ビットは DriverAddress.bit + Numeric(DataType::Bit) で表し、
/// 「ワードを読んでビットを抜く」のはドライバの計画の内側の話にする。
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TagKind {
    Numeric(DataType),
    /// 文字列。words = 16 bit 語の数（banto-tags の string_length、1..=128）。
    /// encoding は Hub 側の情報で WIT には渡さない（WIT ではプラグインが bytes を返し Hub が decode する）。
    /// R1 のアダプターは banto-plc の decode を使うので、ここで運ぶ。
    Str { words: u16, encoding: StringEncoding },
}
impl TagKind {
    pub fn numeric(data_type: DataType) -> Self;
    pub fn string(words: u16, encoding: StringEncoding) -> Self;
    /// このタグが占める 16 bit 語の数（Numeric は DataType::register_span、Str は words）
    pub fn word_count(&self) -> u16;
}

/// WIT: record tag-spec { address, data-type, string-words }
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub struct TagSpec { pub address: DriverAddress, pub kind: TagKind }
impl TagSpec { pub fn new(address: DriverAddress, kind: TagKind) -> Self; }

/// WIT: record write-item { spec, value }
/// value は Hub の Value（Bit / F64 / Str）。WIT の value との対応は §9.2（書き込み側は bool / float / bytes の 3 つだけを使う）
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub struct WriteItem { pub spec: TagSpec, pub value: Value }
impl WriteItem { pub fn new(spec: TagSpec, value: Value) -> Self; }
```

### 3.3 アドレス: R1 の形と R2b-2 への道

- **R1 の要求の型は `banto_plc::Address`（`address.rs:93-130`）のまま**。`BatchReadRequest` / `BatchWriteRequest` も `banto-plc` / `banto-plc-write` に残る。API に `Address` enum を移すと「プロトコルを足すたびに API が変わる」（plugin-design §12.6 の条件 2）ので、移さない。`AddressArea`（`banto-plc-address`）も同じ理由で API には入れない（Modbus の語彙。`is_writable` は `DriverAddress::writable_area` に写る）。
- API には WIT の `parsed` をそのまま **`DriverAddress`** として入れる（R1-1）。R1 では `DriverBase::parse_address` の戻り値として作られ、適合テスト（§7）と R2b-2 の移行の全件照合（plugin-design §7.4 の preflight）が使う。R1 の本番経路（`build_request`・`write_path`・`read_path`）は今までどおり `Address::parse` / `parse_slmp` を直接呼び、`DriverAddress` を**通らない**。

```rust
/// WIT: record parsed { canonical, area, writable-area, bit, word-count }
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DriverAddress {
    pub driver: DriverId,
    /// 正規化した文字列。parse(canonical) == 自分自身（冪等）。DB にはこれを保存する（R2b-2）。規則は Q8（§0.1）
    pub canonical: String,
    /// 領域の名前（Modbus: "coil" 等 = AddressArea の Display と同じ綴り。SLMP: デバイス名 "D" 等）
    pub area: String,
    /// プロトコルの事実としての書き込み可否（D5。書き込み許可の判断には使わない）
    pub writable_area: bool,
    /// ワード内ビット（SLMP は .0〜.F の 16 進、Modbus は .0〜.15 の 10 進。T20-④）
    pub bit: Option<u8>,
    /// このアドレスが占める 16 bit 語の数（bit デバイスは 1）。TagKind::word_count と同じ単位（u16）。WIT の u32 へは境界で広げる
    pub word_count: u16,
}
impl DriverAddress {
    pub fn new(driver: DriverId, canonical: impl Into<String>, area: impl Into<String>, writable_area: bool, word_count: u16) -> Self;
    pub fn with_bit(self, bit: u8) -> Self;
}
```

- **`station` は持たせない**（§0.1 Q6）。RS-485 のマルチドロップは接続設定の局番とエンジンのポート共有で扱う（§8）。
- **canonical の規則（Q8、2026-10-19 オーナー決定: 案 A）**。今のコードの事実: Modbus は `40001`・`400001`・`40001.03`・`40001.3` がすべて同じ `Address`（`address.rs:199-215`、bit の接尾辞は 10 進・最大 2 桁 `:78-84`）で、`Display` は常に 6 桁・bit は先頭ゼロなし（`:278-299`、`400001.3`）。SLMP は `slmp_address::format` が `Display`。選択肢:

  | 案                    | canonical                                                                         | 利点                                                                                                                                                           | 欠点                                                                                                                                                                                          |
  | --------------------- | --------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
  | **A（決定）**         | **今の `Address` の `Display`**（Modbus `400001` / `400001.3`、SLMP は `format`） | 1 つの `Address` に 1 つの正規形。冪等（`parse(Display) == Address`）は既存テスト `address.rs:527` が固定済み。実装は `Address::parse(text)?.to_string()` だけ | 利用者が `40001` と書いても `400001` に揃う。R2b-2 の移行で DB の `address` 列が書き換わる（preflight の一覧で見える）。UI の表示が変わる                                                     |
  | B（不採用）           | 入力をトリム（SLMP は大文字化）したもの                                           | 利用者の綴りが残る                                                                                                                                             | 同じ `Address` に複数の正規形（`40001` / `400001` / `40001.03`）。C1 の「冪等」は通るが「一意」が通らず、R2b-2 の重複検出・plugin-design §7.4 の preflight の比較が成り立たない。**採らない** |
  | C（R2b-2 の拡張候補） | A + DB に利用者の綴り `address_text` を別列で残す（R2b-2）                        | A の一意性と B の表示を両立                                                                                                                                    | 列が 1 つ増える。R1 には影響しない                                                                                                                                                            |

  **決定は A（2026-10-19 オーナー決定、§0.1）**（UI の表示で困るなら R2b-2 で C に広げる）。R1 の適合テスト C1 は「`parse(text).canonical == parse(canonical).canonical`」と「`canonical == Address::parse(text).to_string()`」の両方を確かめる。

- **R2b-2 でやること**（本書の範囲外だが、R1 の形がそれを邪魔しない根拠）: `TagSpec.address: DriverAddress` を要求の型にし、各ドライバが `canonical` を自分の内部表現へ戻す（Modbus / SLMP は `Address::parse` を再実行。接続ごとに `HashMap<String, Address>` でキャッシュすれば毎ポーリングの再解析は無い）。`protocol == "modbus-tcp"` の分岐 4 か所（§1.1）は `registry.get(id).parse_address` に置き換わる。

### 3.4 エラーの分類（h9 と `is_connection_fatal` を型にする）

```rust
/// WIT: variant item-error { device(device-error), unsupported(string), rejected(string) } ― 非致命（そのタグだけ Bad）
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemError {
    /// 機器が完全な応答で断った。code は機器の生の値（SLMP の終了コード、Modbus の例外コード）
    Device(DeviceError),
    /// 設定の不整合（データ型と領域の組み合わせ、文字列長の範囲外、別プロトコルの表記）。ワイヤに出る前に決まる
    Unsupported(String),
    /// ドライバ自身が断った、または確認で不一致を見つけた（同じワードの同じビットへ true / false が同時、
    /// RMW の確認読みの不一致）。機器の拒否でも設定の不整合でもない非致命
    Rejected(String),
}
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceError { pub code: u32, pub message: String }
impl DeviceError { pub fn new(code: u32, message: impl Into<String>) -> Self; }

/// WIT: variant fatal-error { framing, protocol, invalid-config } ― 接続致命。
/// Io / Timeout は WIT には無い（WASM では Hub 側で起きる）。**フレーム型は返してはならない**（C4 で確かめる）。セッション型だけが返す
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FatalError {
    Framing(String),
    Protocol(String),
    InvalidConfig(String),
    Io(String),
    Timeout,
}
```

`banto_plc::PlcError` / `PlcWriteError` からの写し。R1-4b で `banto-driver-modbus` / `-slmp` に **`fn classify_read(PlcError) -> Result<ItemError, FatalError>` と `fn classify_write(PlcWriteError) -> Result<ItemError, FatalError>`** の 2 本を置き、表のとおりになることを単体テストで固定する（`is_connection_fatal` が `false` のものがすべて `Ok(ItemError)` になることを、両 enum の全 variant を並べたテストで確かめる。本番経路は R1 では使わない）:

| 元                                                                                                                       | 行き先                                                                              |
| ------------------------------------------------------------------------------------------------------------------------ | ----------------------------------------------------------------------------------- |
| `ModbusException { function, code, message }`（読み書き両方）                                                            | `ItemError::Device { code: code as u32, message: "FC=0x{function:02x} {message}" }` |
| `SlmpEndCode { code, message }`（読み書き両方）                                                                          | `ItemError::Device { code: code as u32, message }`                                  |
| `UnsupportedCombination` / `AddressProtocolMismatch` / `StringSpanUnsupported`（読み書き両方）                           | `ItemError::Unsupported(err.to_string())`                                           |
| `PlcWriteError::ValueTypeMismatch` / `ValueOutOfRange` / `ModbusReadOnlyArea` / `UnsupportedRequestKind`                 | `ItemError::Unsupported(..)`                                                        |
| `PlcWriteError::ConflictingBitWrite`（`error.rs:174`）/ `BitWriteVerificationFailed`（`:189`）。今も非致命（`:240-256`） | `ItemError::Rejected(err.to_string())`                                              |
| `ConnectTimeout` / `Connection` / `NotConnected`                                                                         | `FatalError::Io(..)`                                                                |
| `ResponseTimeout`                                                                                                        | `FatalError::Timeout`                                                               |
| `Protocol(..)`                                                                                                           | `FatalError::Framing(..)`（ワイヤの構造の壊れ）                                     |
| `InvalidAddress(..)`（今は致命に分類されている、`is_connection_fatal :145-154`）                                         | `FatalError::InvalidConfig(..)`（致命のまま。分類を変えない）                       |

- **既定は致命**（WIT §6.1 と同じ）。`ItemError` に写せない variant が増えたら `FatalError` へ倒す。
- `InvalidAddress` を致命のままにするのは R1 の互換のため。API の契約としては「アドレスの文法違反は `parse_address` の `AddressError` で接続前に止める」ので、R2b-2 で要求が `DriverAddress` になれば `InvalidAddress` は接続後には起きない。implementation-checklist §5 の「致命 / 非致命の反証」はこの行を含めて確かめる（C4）。

### 3.5 接続設定

```rust
/// WIT: record setting { key, value }。順序つき（UI と監査の表示順）、重複キーなし
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Settings { items: Vec<Setting> }     // field は private（重複なしを保つ）
impl Settings {
    pub fn new() -> Self;
    pub fn from_pairs<K: Into<String>, V: Into<String>>(pairs: impl IntoIterator<Item = (K, V)>) -> Result<Self, DuplicateKey>;
    pub fn get(&self, key: &str) -> Option<&str>;
    pub fn insert(&mut self, key: impl Into<String>, value: impl Into<String>);   // 同じキーは上書き
    pub fn iter(&self) -> impl Iterator<Item = &Setting>;
}
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Setting { pub key: String, pub value: String }
impl Setting { pub fn new(key: impl Into<String>, value: impl Into<String>) -> Self; }

/// 共通のキー（ホストが固定列から入れる。R2b-1 で JSON へ移っても同じキー）
pub mod keys {
    pub const HOST: &str = "host";
    pub const PORT: &str = "port";
    pub const UNIT_ID: &str = "unit_id";           // Modbus の局番（unit identifier）。SLMP では無視
    pub const WORD_ORDER: &str = "word_order";     // "high_low" | "low_high"
    pub const CONNECT_TIMEOUT_MS: &str = "connect_timeout_ms";
    pub const RESPONSE_TIMEOUT_MS: &str = "response_timeout_ms";
    pub const CPU_TYPE: &str = "cpu_type";         // SLMP の #605（"R" | "Q"）
    pub const TRANSPORT: &str = "transport";       // §8。R1 は "tcp" 固定
    pub const SERIAL_PORT: &str = "serial_port";   // §8（予約。R2b-3）
    pub const STATION: &str = "station";           // §8（予約。RS-485 マルチドロップの局番。R1 の Modbus は unit_id がこれ）
}

/// WIT: record field-error { key, message }
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldError { pub key: String, pub message: String }
impl FieldError { pub fn new(key: impl Into<String>, message: impl Into<String>) -> Self; }
```

R1 では `banto_tags::PlcConnection` の固定列（`host`・`port`・`unit_id`・`word_order`・`cpu_type`）から `Settings` を組む純関数 `banto_drivers::settings_from_connection(&PlcConnection) -> Settings` を置き、`DriverBase::validate_config` と R2b-1 の JSON 化の両方がこれを通る。ワード順は `word_order` キーで**接続設定として**ドライバへ渡す（plugin-design の監査 B2: 読み取り単位には持たせない。`ClientSpec.word_order` と同じ扱い、`task.rs:281-297`）。

## 4. フレーム型の trait（WIT §6.2 の写し）

### 4.1 共通の静的な部分

```rust
/// API の版。WIT の manifest.hubApi の主版と同じ数
pub const API_VERSION: u32 = 1;

/// 識別。WIT の manifest（id / version / displayName / vendor / transports / capabilities / hubApi）に対応
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriverDescriptor {
    pub id: DriverId,               // [a-z0-9][a-z0-9-]*。plc_connections.protocol の値そのもの（"modbus-tcp" / "slmp"）
    pub version: String,            // SemVer。組み込みは crate の版
    pub display_name: String,
    pub vendor: Option<String>,
    pub transports: Vec<Transport>, // §8。R1 の Modbus / SLMP は [Tcp]
    pub capabilities: Capabilities, // { read: bool, write: bool }。WIT の world driver / driver-rw
    pub api_version: u32,           // = API_VERSION。new が入れる。WIT の hubApi（文字列）の主版に対応
}
impl DriverDescriptor {
    pub fn new(id: DriverId, version: impl Into<String>, display_name: impl Into<String>, capabilities: Capabilities) -> Self; // transports = [Tcp]
    pub fn with_vendor(self, vendor: impl Into<String>) -> Self;
    pub fn with_transports(self, transports: Vec<Transport>) -> Self;
}
#[non_exhaustive] #[derive(Debug, Clone, Copy, PartialEq, Eq)] pub struct Capabilities { pub read: bool, pub write: bool }
impl Capabilities { pub const fn new(read: bool, write: bool) -> Self; pub const READ_ONLY: Self; pub const READ_WRITE: Self; }
#[non_exhaustive] #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)] pub enum Transport { Tcp, Serial, SerialOverTcp }

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DriverId(String);
impl DriverId { pub fn new(s: &str) -> Result<Self, InvalidDriverId>; pub fn as_str(&self) -> &str; }
impl std::borrow::Borrow<str> for DriverId {}   // BTreeMap<DriverId, _>::get(&str) のため（§6.1）
impl std::fmt::Display for DriverId {}

/// 状態を持たない部分。REST / MCP / CSV / 設定パッケージ / UI から並行に呼ばれる（§6.5）
pub trait DriverBase: Send + Sync + 'static {
    fn descriptor(&self) -> &DriverDescriptor;
    /// WIT config.validate
    fn validate_config(&self, settings: &Settings) -> Vec<FieldError>;
    /// WIT config.secret-keys。既定は無し
    fn secret_keys(&self) -> Vec<String> { Vec::new() }
    /// WIT address.parse(text, data-type, string-words)。kind は語数を決めるために要る
    fn parse_address(&self, text: &str, kind: &TagKind) -> Result<DriverAddress, AddressError>;
    /// WIT address.offset(canonical, data-type, string-words, n)（連続登録・オフセットコピー）
    fn offset_address(&self, canonical: &str, kind: &TagKind, n: i64) -> Result<String, AddressError>;
    /// WIT address.help（日本語固定、plugin-design §6.2 補足）
    fn address_help(&self) -> String;
}
#[non_exhaustive] #[derive(Debug, Clone, PartialEq, Eq)] pub struct AddressError { message: String }
impl AddressError { pub fn new(message: impl Into<String>) -> Self; pub fn message(&self) -> &str; }
```

### 4.2 フレーム型（I/O を持たない）

```rust
/// WIT interface session の frame-rule / step（監査 M6: none = 応答を待たない送信）
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameRule {
    /// 応答なし（ブロードキャスト等）。エンジンは送信後すぐ次へ進む
    None,
    /// 固定長
    Fixed(u32),
    /// 長さ接頭辞: 受信した offset..offset+width を encoding で整数に読み、+ adjust が総長
    LengthPrefixed(PrefixSpec),
    /// 終端文字列（例: ホストリンクの CR）
    Terminator(Vec<u8>),
    /// 受信のたびに is_complete を呼ぶ。max_len を超えたら致命
    DriverDecides,
}
/// 長さ欄の読み方。SLMP の ASCII フレームは長さ欄が ASCII の 16 進 4 文字（監査 M5）
#[non_exhaustive] #[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrefixEncoding { Binary, AsciiHex, AsciiDec }
#[non_exhaustive] #[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrefixSpec { pub offset: u32, pub width: u8, pub big_endian: bool, pub adjust: i32, pub encoding: PrefixEncoding }
impl PrefixSpec {
    pub fn new(offset: u32, width: u8, big_endian: bool, adjust: i32) -> Self;   // encoding = Binary
    pub fn with_encoding(self, encoding: PrefixEncoding) -> Self;
}

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step { pub send: Vec<u8>, pub expect: FrameRule, pub max_len: u32 }
impl Step { pub fn new(send: Vec<u8>, expect: FrameRule, max_len: u32) -> Self; }

/// WIT read.planned / write.planned-write（covers = plan_read に渡した specs、または write_next に渡した items の添字）
#[non_exhaustive] #[derive(Debug, Clone, PartialEq, Eq)] pub struct Planned { pub step: Step, pub covers: Vec<u32> }
impl Planned { pub fn new(step: Step, covers: Vec<u32>) -> Self; }

/// is_complete の答え（監査 M5: 「完結」「まだ」「先頭の n バイトはごみ」の 3 つ）
#[non_exhaustive] #[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameCheck {
    Incomplete,
    /// 先頭から len バイトが 1 つの応答。残りは捨てる（次の step に持ち越さない）
    Complete { len: u32 },
    /// 先頭の n バイトを捨てて続ける（RS-485 の自己エコー、前の応答の残り、ノイズ）。n >= 1
    Discard(u32),
}

/// write_next の答え（§4.2.1）
#[non_exhaustive] #[derive(Debug, Clone, PartialEq)]
pub enum WriteProgress {
    /// 次に送る step。covers はこの step が関わる items の添字（ログ・監査の対応付け用。結果は Done で返す）
    Send(Planned),
    /// 1 回の論理書き込みの終わり。items と同じ長さ・順序の結果。これ以降、同じ書き込みでは呼ばれない
    Done(Vec<Result<(), ItemError>>),
}

pub trait FrameDriver: DriverBase {
    /// WIT connection.create。接続ごとに 1 つ（§6.5）
    fn create_connection(&self, settings: &Settings) -> Result<Box<dyn FrameConnection>, FatalError>;
}

/// WIT resource connection + read + write。1 つの broker タスクが専有する（Send、Sync 不要）
pub trait FrameConnection: Send + 'static {
    /// WIT handshake-next。最初は None、以降は直前の step の応答。None を返したら完了。
    /// 拒否応答なら Err（接続成功とみなさない）
    fn handshake_next(&mut self, prev_response: Option<&[u8]>) -> Result<Option<Step>, FatalError>;
    /// 切断の直前に送る step（EtherNet/IP の UnRegisterSession 等）。既定は無し。応答は待たない
    fn farewell(&mut self) -> Option<Step> { None }
    /// WIT is-complete（DriverDecides のときだけ呼ばれる）。step は今待っている応答の step
    fn is_complete(&self, step: &Step, buf: &[u8]) -> Result<FrameCheck, FatalError>;
    /// WIT read.plan。&mut self: トランザクション ID 等の採番を読み書きで共有する（modbus_driver.rs:41-79）
    fn plan_read(&mut self, specs: &[TagSpec]) -> Result<Vec<Planned>, FatalError>;
    /// WIT read.decode。specs は plan_read に渡したものと同じスライス。戻りは planned.covers と同じ長さ・順序。
    /// ドライバは specs[covers[i]].kind で解釈を決める（同じアドレス・同じフレームでも U32 と F32 は別の値）
    fn decode_read(&self, planned: &Planned, specs: &[TagSpec], response: &[u8]) -> Result<Vec<Result<RawValue, ItemError>>, FatalError>;
    /// WIT write.write-next（§4.2.1）。書き込みは「前の step の応答を受けて次の step か最終結果を返す」状態機械。
    /// 1 回の論理書き込み（items 全体）につき、最初は prev_response = None、以降は直前の step の応答。
    /// items は 1 回の書き込みの間ずっと同じスライス。capabilities.write == false なら呼ばれない（既定は Err）
    fn write_next(&mut self, _items: &[WriteItem], _prev_response: Option<&[u8]>) -> Result<WriteProgress, FatalError> {
        Err(FatalError::Protocol("このドライバは書き込みに対応していません".into()))
    }
}
```

- **decode に文脈を渡す理由**（レビュー 1）: `Planned` は `Step` と `covers` しか持たないので、`decode_read` が「どの型で読むか」を知るには元の `specs` が要る。今の `banto-plc` も `planning.rs:85-114` の `MappedRequest` が `offset_in_read`・`data_type`・`bit` を持ち、`decode.rs:66-100` が `data_type` で分岐している。同じ Modbus のアドレスを U32 と F32 で読むとフレームも `covers` も同じになるので、`specs` を渡さない契約では区別できない。エンジンの不変条件: **`decode_read` に渡す `specs` は直前の `plan_read` に渡したスライスと同一**（ハーネスがこれを守る。C3 の「同じアドレス・別の型」のケースで確かめる）。
- **状態を変える呼び出し**: `handshake_next`・`farewell`（接続の状態）、`plan_read`（採番）、`write_next`（採番と書き込みの途中の状態）。`decode_read`・`is_complete` は `&self`。WIT の `borrow<connection>` は可変性を区別しないが、WASM ホストでも接続は 1 タスクが専有するので同じ契約になる。
- **要求 - 応答の手順（エンジンが R2b-3 で実装する契約。R1 では適合テストのハーネスが同じ手順を持つ）**: (1) `plan_read` で `Vec<Planned>` を得る → (2) 各 `Planned` について `step.send` を送り、`expect` に従って受信（`None` なら受信しない。`DriverDecides` なら受信のたびに `is_complete`。`Discard(n)` なら先頭 n バイトを捨てて続ける）→ (3) `max_len` 超過・`response_timeout` 超過は致命で、**受信バッファを捨てて接続を切る**（遅れて届いた応答を次の step に流用しない。`banto-plc/src/error.rs:39-47` の `ResponseTimeout` の理由と同じ）→ (4) `decode_read(planned, specs, response)` → (5) `covers` の添字で結果を `specs` の順に並べる。`covers` に出てこない添字は `ItemError::Unsupported("計画に含まれない")`。
- **Modbus RTU の終端の決め方**（監査 M5。R2b-3 の実装者向けに R1 で固定する規則）: `DriverDecides` で、長さは **FC とバイト数の欄から**決める（例外応答は FC の最上位ビットで判定し 5 バイト固定）。CRC は**検証**に使い、合えば `Complete`、合わなければ `FatalError::Framing`。「CRC が合った時点を完結とする」規則は採らない（途中のバイト列で偶然 CRC が合う誤判定があるため）。
- **ワード順**は `Settings` の `word_order` から `create_connection` が読む（監査 B2）。文字列は `RawValue::Bytes` で返し、エンジンが `TagKind::Str.encoding` で decode する（`banto-plc/src/decode.rs` の `decode_string_value` と同じ規則: 語の下位バイトが先、最初の NUL で切る）。

### 4.2.1 書き込みは状態機械（2026-10-19 レビュー指摘で修正）

**なぜ要るか**: 今の SLMP のワード内ビット書き込みは「今の値を読む → 対象ビットだけ変えて書き戻す → 確認読み」の手順で、書き戻す値は最初に読んだ値で決まる（`crates/banto-plc-write/src/slmp/mod.rs:354-419` の `execute_one_bit_write`: `:360` で `current` を読み、`:375` で `new_word = (current & !(set_mask | clear_mask)) | set_mask`、`:381` で書き戻し、`:395` で確認読み、ビットごとに照合）。例えば D0.3 = true のとき、D0 が `0x0000` なら `0x0008`、`0x0100` なら `0x0108` を書かないと他のビットを壊す。送るバイト列を通信の前に全部決める形（旧案の `encode_write` → `Vec<Planned>`、応答は `check_write` で成否だけ）ではこの手順を表せない。

**形**: `handshake_next` と同じく、ドライバが前の応答を受けて次の step を返す。

```rust
// エンジン（R2b-3）・適合テストのハーネスの手順（1 回の論理書き込み = items 全体）
let mut prev: Option<Vec<u8>> = None;
loop {
    match conn.write_next(items, prev.as_deref())? {          // Err は致命 → 接続を切る
        WriteProgress::Send(planned) => {
            steps += 1;
            if steps > max_steps(items.len()) { return Err(FatalError::Protocol("書き込みの手順が上限を超えた".into())); }
            prev = Some(send_and_receive(&planned.step)?);   // 応答待ちの規則は読み取りと同じ（§4.2 の手順 (2)(3)）
        }
        WriteProgress::Done(results) => {
            // 件数の食い違いはドライバの契約違反。panic させず致命として接続を切る
            if results.len() != items.len() { return Err(FatalError::Protocol("書き込みの結果の件数が要求と合わない".into())); }
            return Ok(results);
        }
    }
}
```

**契約**:

1. **始まりと終わり**: `prev_response = None` の呼び出しが新しい書き込みの始まり。ドライバは途中の状態をここで必ず作り直す（前の書き込みが致命で中断していても持ち越さない）。`Done` を返したら、その書き込みは終わり。
2. **items は不変**: 1 回の書き込みの間、エンジンは同じ `items` を渡し続ける。ドライバは途中の状態（どの step まで進んだか、読んだ今の値）を接続の中に持ち、`items` の添字で結果を組む。
3. **covers**: `Send(planned)` の `covers` は、その step が関わる items の添字（RMW なら同じワードに落ちる全ビットの要求）。結果の対応付けには使わず、ログと監査のためにある。結果は必ず `Done` で items の順に返す。
4. **非致命のエラー**（機器の終了コード・確認読みの不一致）: そのワード（RMW のまとまり）に関わる items を `ItemError::Device` / `ItemError::Rejected` にして、**そのまとまりにはそれ以上 step を送らない**。他のまとまりは続けてよい。今の `execute_one_bit_write` と同じ（読み取りで拒否されたら書き戻さない、書き戻しで拒否されたら確認しない）。不一致はビットごとに判定し、正しく反映されたビットを道連れに Bad にしない（`:404-416`）。
5. **致命のエラー**（`Err`、応答タイムアウト、`max_len` 超過、手順の上限超過）: エンジンは受信バッファを捨てて接続を切り、`write_next` をそれ以上呼ばない。書き込み全体は `Err` で返る（今の `execute_slmp_writes` が `Some(err)` で全体を `Err` にするのと同じ）。接続は作り直すので、ドライバの途中の状態も接続ごと捨てられる。
6. **割り込ませない**: 1 回の書き込みの最初の `write_next(None)` から `Done` まで（確認読みを含む）、エンジンは同じ接続に他の要求（読み取り・別の書き込み・`farewell`）を挟まない。R1（`HostBound`）では broker のジョブが 1 接続を専有して `execute_slmp_writes` を 1 ジョブで走らせているので、今と同じ形で守られている。R2b-3 のエンジンは「1 ジョブ = 1 回の論理書き込み」を同じ単位で直列化する。
7. **手順の上限**: エンジンは 1 回の書き込みの step 数に上限を持つ（目安は `3 × items.len() + 8`。RMW は 1 まとまりあたり 3 step）。超えたら致命。ドライバの無限ループで接続を握り続けないため（C16）。
8. **安全の関門との順序**: `write_path.rs` の関門（`writable`・スコープ・受付トグル・頻度・監査の先書き）は、最初の `write_next(None)` より前に Hub 側で済ませる（§11）。状態機械はプロトコルの手順だけを表し、許可の判断はしない。

**Modbus**: `banto-plc-write` の Modbus はワード内ビット書き込みを**受け付けない**（`crates/banto-plc-write/src/modbus/planning.rs:69-86`。FC22 Mask Write は使わず、`"40001.3"` のような書き込みは計画の段階で拒否）。今の Modbus の書き込みは要求ごとに 1 step（FC5 / FC6 / FC15 / FC16 と応答のエコーの照合、`modbus/mod.rs:122-227`）なので、フレーム型にするときは `write_next` が step を順に `Send` し、最後に `Done` を返すだけの単純な状態機械になる。将来 FC22 を使うなら 1 step のまま表せる。

**読み取りは 1 回で決まる形のまま**: `plan_read` → 各 `Planned` を送る → `decode_read` のまま変えない。今の Modbus / SLMP の読み取りは、応答を見て次の要求を変える手順を持たない。応答に応じて追いかけて読むプロトコル（ページ送りなど）が出てきたら、既定の実装つきの `read_next` を足す（trait に既定つきのメソッドを足すのは破壊にならない。§9）。

**R1 と R2b-3**: R1 では Modbus / SLMP は `HostBound` のままで、RMW は今の `banto-plc-write` のコードが行う（`write_next` を実装するドライバは R1 には無い。適合テストの C13〜C15 は `LegacySession` 経由で今の実装に掛ける、§7.3）。`write_next` は、R2b-3 で Modbus / SLMP をフレーム型に移すときの契約。

### 4.3 R1 での使われ方

**R1 では `FrameDriver` / `FrameConnection` を実装するドライバは無い**（Modbus / SLMP は §4.4）。R1-2 で trait と、適合テストのための**参照実装**（`banto-driver-conformance` の `examples/echo_driver.rs`: 固定長フレームで echo する最小のフレーム型ドライバ。plugin-design §10 P1 の「組み込みにないアドレス文法を持つテスト用プラグイン」の前身）を入れ、trait が実装可能であることとハーネスの手順を固める。

### 4.4 R1 のアダプター: Modbus / SLMP をどう載せるか

| 選択肢                                                                                             | I/O に触るか                                                                                                                                                                                                                                          | 振る舞いの危険                                                                                                  | 判定         |
| -------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------- | ------------ |
| A. 真のフレーム型にする（`plan` / `decode` を `banto_plc::planning` から組み直し、I/O はエンジン） | **触る**。エンジン（R2b-3）が無いと動かない。SLMP は `tyaro/slmp` の `SLMPClient` がフレームと I/O を一体で持つので、フレームだけ取り出すにはライブラリ側の改造が要る                                                                                 | 分割受信・遅延応答・トランザクション ID の一本化（`modbus_driver.rs:41-79`）を落とす危険。P0-3 の固定テストが先 | R2b-3 でやる |
| B. セッション型で包む（`BrokerSession` → `api::Session` のアダプター）                             | broker の内部（`Job` が運ぶ `BatchReadRequest`、`ConnState`）を `api::Session` と `TagSpec` に替える必要がある。canonical → `Address` の再解析が毎回入る                                                                                              | broker の job ループ・再接続の状態機械に手が入る。write_path / read_path の要求の型も変わる                     | R2b-2 と同時 |
| **C. `HostBound`（本書の採用）**                                                                   | **触らない**。`slmp_driver.rs` / `modbus_driver.rs` をファイルごとドライバ crate へ移し、`DRIVERS` の表を「接続 → `Connector`」の関数の表として組み立て役から注入する。**spawn・`TaskEntry`・job ループは broker に残す**（外から実装できない。§1.1） | 無し（移動と注入だけ。既存テストがそのまま判定基準）                                                            | **R1**       |

C の形:

```rust
// banto-driver-api::registry
#[non_exhaustive]
pub enum DriverKind {
    Frame(Arc<dyn FrameDriver>),
    Session(Arc<dyn SessionDriver>),
    /// R1 限定。識別・設定・アドレス関数は API の形で持ち、I/O はホストが別の表
    /// （banto_broker::DriverTable）で結ぶ組み込みドライバ。
    /// R2b-3 で Frame に載せ替えて消す（§9 の 0.x の minor の破壊として扱う）
    HostBound(Arc<dyn DriverBase>),
}
```

```rust
// banto-broker（R1-3a）: 固定の const DRIVERS を注入式に。今の DriverSpawn（lib.rs:743-744）は
// (BrokerHandle, TaskEntry) を返すが TaskEntry は private で外から作れない（:731）ので、
// 外に出すのは「接続 → Connector」の部分だけにし、spawn は broker が行う
pub struct DriverDial { pub connector: Connector, pub slmp_cpu: Option<SlmpCpu> }   // slmp_cpu は BrokerHandle::slmp_cpu（:475）へ写す（#605）
pub type DialFn = fn(&PlcConnection, u16) -> DriverDial;   // u16 = 検証済みのポート（今の spawn_slmp_driver と同じ引数）
#[derive(Clone)]
pub struct DriverTable { entries: Arc<[(DriverId, DialFn)]> }
impl DriverTable {
    pub fn new(entries: Vec<(DriverId, DialFn)>) -> Result<Self, DuplicateDriverId>;
    pub fn is_supported(&self, protocol: &str) -> bool;   // 今の is_supported_protocol(:798-800) の中身
    pub fn dial_for(&self, protocol: &str) -> Option<DialFn>;
    pub fn ids(&self) -> impl Iterator<Item = &DriverId>;
    #[doc(hidden)] pub fn builtin() -> Self;   // R1-3a だけの橋: 今の spawn_slmp_driver / spawn_modbus_driver を dial に直したもの。R1-4a で消す
}
impl BrokerSupervisor { pub fn spawn(connections, backoff: BackoffConfig, drivers: DriverTable) -> Self; }
// SessionDirectory::ensure_connection(:840-848): DRIVERS.iter().find(..) を self.drivers.dial_for(&conn.protocol) に。
//   let DriverDial { connector, slmp_cpu } = dial(conn, port);
//   let (mut handle, task) = spawn_task_with_connector(conn.id, connector, backoff, shutdown_rx);  // :1054、private のまま
//   handle.slmp_cpu = slmp_cpu;
// banto_broker::is_supported_protocol は消す（#[deprecated] にしない。§6.2）
```

```rust
// banto-driver-slmp / -modbus（R1-4a）: 今の spawn_slmp_driver(:758-771) / spawn_modbus_driver の中身
pub fn dial(conn: &PlcConnection, port: u16) -> banto_broker::DriverDial;   // slmp_config_for(conn, port) → connector
```

```rust
// banto-drivers（R1-3b / R1-4）
pub struct Builtin {
    pub registry: DriverRegistry,                     // R1-4b から。modbus-tcp / slmp を HostBound で登録
    pub broker: banto_broker::DriverTable,            // R1-3b: DriverTable::builtin()。R1-4a: [("slmp", banto_driver_slmp::dial), ("modbus-tcp", banto_driver_modbus::dial)]
    pub client_factory: banto_collect::ClientFactory, // = banto_collect::default_client_factory()（写しを作らない）
}
pub fn builtin() -> Builtin;
```

- **broker の単体テストの行き先（R1-4a）**。broker の `#[cfg(test)] mod tests`（`lib.rs:1322-`）は今 SLMP / Modbus のシミュレータで動くが、`slmp_driver.rs` / `modbus_driver.rs` が出ていくと broker の中からは実ドライバの `Connector` が作れない（**broker はドライバ crate を dev-dependency にもしない**: 循環と二重コンパイルを避ける）。そこで 2 つに分ける:

  | 種類                                                           | テスト（`crates/banto-broker/src/lib.rs`）                                                                                                                                                                                                                                                                                                                                                                                                                                                                         | 行き先・直し方                                                                                                                                                                                                                                                                                                                                                                                                                          |
  | -------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
  | job ループ・再接続・状態・表の振る舞い（プロトコルに依らない） | `spawn_task` を直接呼ぶ 4 本（`:1884` `a_request_while_disconnected_fails_fast_not_a_hang`・`:1921` `fatal_error_triggers_reconnect_and_later_requests_succeed`・`:1968` `status_watch_observes_connected_then_reconnecting_after_stop`・`:2058` `stop_and_join_aborts_an_inflight_connect_attempt`）、`:1585` `supervisor_rejects_an_unsupported_protocol_connection`、`:1629`・`:1651`・`:1661`（remove / ensure）、`:1822` `serialized_concurrent_requests_do_not_corrupt_each_other`、`:2005`・`:2029`（停止） | **broker に残し、偽の `Connector` で書き直す**。`#[cfg(test)] mod fake`（in-memory の語 / ビットの表を持つ `FakeSession: BrokerSession`、接続を断る・次の 1 回を致命にする口つき）を置く。`spawn_task(..SlmpConfig..)` は `spawn_task_with_connector(id, fake::connector(..), ..)` に。表を通るテストは `DriverTable::new(vec![("slmp", fake::dial)])`（`DialFn` は `fn` なので状態は `dial` の中で作る `Connector` の closure が持つ） |
  | プロトコルの振る舞いそのもの                                   | `:1546` `cpu_type_selects_the_slmp_subcommand_for_read_and_write`・`:1613` `supervisor_hands_out_a_handle_per_slmp_connection`・`:1689` `read_returns_simulated_device_values`・`:1704`・`:1751`・`:1790`（書いて読む・ワード内ビット・文字列）→ SLMP。`:1601` `supervisor_hands_out_a_handle_per_modbus_tcp_connection` → Modbus。`slmp_config_for_*`（`:1509, :1524`）                                                                                                                                           | **ドライバ crate の `tests/broker.rs` へ移す**。公開の `BrokerSupervisor::spawn(.., DriverTable::new(vec![("slmp", banto_driver_slmp::dial)]))` で書き直す（中身の assert は変えない）。`slmp_config_for_*` は `slmp_config_for` と一緒に `src` 内へ                                                                                                                                                                                    |

  `spawn_task`（SLMP 専用の入口）は R1-4a で消す（呼び出し側は上の 4 本と `spawn_slmp_driver`（`:768`）だけで、後者は `banto_driver_slmp::dial` に替わる）。`examples/real_plc_verify.rs`（実機の検証。偽物では意味が無い）は `crates/banto-drivers/examples/` へ移し、`BrokerSupervisor::spawn(.., builtin().broker)` に直す。移動の前後で `cargo test -p banto-broker -p banto-driver-slmp -p banto-driver-modbus` のテスト件数の合計が減らないこと（削除なし）を PR に書く。

- `banto_collect::default_client_factory`（`task.rs:385-409`）は R1 では**残す**（ChronoGazer の `collect.rs:1919, 2573` と banto-collect 自身のテストが使う）。`banto_drivers::builtin().client_factory` はこれをそのまま返す。`default_client_factory` の撤去は R2b-3。
- Hub の `hub_client_factory`（`broker_glue.rs:834-873`）は `ClientProtocol` の `match` を **`spec.protocol` に関係なく `broker_handles.get(&spec.connection_key)` を見る 1 本**にできる（どちらの腕も同じ中身。#337 の後の形）。これは振る舞い不変の整理で R1-4c に含める。`ClientProtocol` 自体の撤去は R2b。

## 5. セッション型の trait（I/O を自分で持つ）

```rust
/// 接続時にホストが渡す文脈。tokio は API に入れないので、ハンドルは渡さない
/// （実装はホストと同じ tokio ランタイムの上で spawn されるので、tokio::spawn をそのまま使える）
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct ConnectContext { pub connection_key: String, pub connect_timeout: Duration, pub response_timeout: Duration }
impl ConnectContext { pub fn new(connection_key: impl Into<String>, connect_timeout: Duration, response_timeout: Duration) -> Self; }

pub trait SessionDriver: DriverBase {
    /// 接続を張って Session を返す。失敗は致命（ホストがバックオフする）。
    /// BoxFuture<'static> なのは banto_broker::Connector（session.rs:126-128）と同じ理由（接続試行を別タスクで走らせる）
    fn connect(&self, settings: &Settings, cx: ConnectContext) -> BoxFuture<'static, Result<Box<dyn Session>, FatalError>>;
}

/// banto_broker::BrokerSession（session.rs:70-92）と同じ形・同じ契約: Err = 接続致命、Ok は specs と同じ長さ・順序
pub trait Session: Send + 'static {
    fn read_batch<'a>(&'a mut self, specs: &'a [TagSpec]) -> BoxFuture<'a, Result<Vec<Result<Value, ItemError>>, FatalError>>;
    fn write_batch<'a>(&'a mut self, _items: &'a [WriteItem]) -> BoxFuture<'a, Result<Vec<Result<(), ItemError>>, FatalError>> {
        Box::pin(async { Err(FatalError::Protocol("このドライバは書き込みに対応していません".into())) })
    }
    fn disconnect(&mut self) -> BoxFuture<'_, ()>;
}
```

- `Value` を返す（`RawValue` ではない）: セッション型は自分で型を知っている（OPC UA の Variant、DB の列、`push` の JSON）ので、エンジンに生値の解釈を頼まない。
- 1 接続 = 1 `Session`、1 つの broker タスクが専有し `read` / `write` を直列化する（`BrokerSession` と同じ。`&mut self` が型で保証）。
- **`push`（external-db-design §9.5）**: `SessionDriver` の実装。`connect` は WebSocket / REST の送信口から値を受ける受信側（`watch` か `RwLock<HashMap>`）を握った `Session` を返し、`read_batch` は「保持している最新値」を返す（external.rs のモジュール doc「sample and hold」と同じ）。`ExternalSource`（`external.rs:151-153`）は R1 では**そのまま**残す。R2a で `push` を `SessionDriver` にするとき、ChronoGazer の `hub` 接続を `ExternalSource` のまま残すか `SessionDriver` に寄せるかを決める（Q5、決定: R2a の完了条件を見てから R2b で）。
- OPC UA クライアントのドライバ（tag-server-design §3.4.1 の決まり 4）はこの trait の実装になる。組み込みのみ（plugin-design §6.1）。
- レジストリでの共存: `DriverKind::Session`。エンジン（R2b-3）は `Frame` なら「トランスポート + フレーム規則」で、`Session` なら「`connect` して `Session` を直接」で、どちらも同じ broker タスク（`run_broker_task`）に載せる。R1 では `Session` の実装は無く、適合テストの偽ドライバ（§7.4）と R1-5 の `LegacySession`（dev-only）だけがある。

## 6. `DriverRegistry`

### 6.1 形（`banto-driver-api::registry`、R1-2）

```rust
#[non_exhaustive]
pub struct DriverEntry { kind: DriverKind }     // descriptor は持たない（base().descriptor() と二重になるため）
impl DriverEntry {
    pub fn frame(driver: Arc<dyn FrameDriver>) -> Self;
    pub fn session(driver: Arc<dyn SessionDriver>) -> Self;
    pub fn host_bound(driver: Arc<dyn DriverBase>) -> Self;    // R1 限定
    pub fn base(&self) -> &dyn DriverBase;                     // kind のどれでも静的な部分は取れる
    pub fn descriptor(&self) -> &DriverDescriptor { self.base().descriptor() }
    pub fn kind(&self) -> &DriverKind;
}

#[derive(Default)]
pub struct DriverRegistry { entries: BTreeMap<DriverId, DriverEntry> }
impl DriverRegistry {
    pub fn register(&mut self, entry: DriverEntry) -> Result<(), RegistryError>;  // 同じ id は拒否（plugin-design §4.2「組み込みの ID は上書きできない」の前身）
    pub fn get(&self, id: &str) -> Option<&DriverEntry>;        // DriverId: Borrow<str> で &str 引き
    pub fn contains(&self, id: &str) -> bool;
    pub fn ids(&self) -> impl Iterator<Item = &DriverId>;
    pub fn len(&self) -> usize; pub fn is_empty(&self) -> bool;
}
impl std::fmt::Debug for DriverRegistry {}   // id の一覧だけ出す（Arc<dyn ..> は Debug でない）
#[non_exhaustive] #[derive(Debug, Clone, PartialEq, Eq)] pub enum RegistryError { DuplicateId(DriverId) }
```

- 同一性は `(id, version)`。R1 では 1 つの id に 1 つの実装（「接続ごとに実装を選べる」R2b-4 は `register_alternate(id, variant_name, entry)` を**後から足す**。既定実装つきの追加なので API の破壊にならない）。
- `Arc<DriverRegistry>` はホストが作り、`HubDrivers`（§6.4）の一部として `HubSessions` が持つ。R1 では読む側は `DriverTable::is_supported`（§6.2）だけで、`DriverRegistry` 自体を読む本番経路は無い（トリップワイヤが読む）。

### 6.2 R1 で「レジストリが正」になるもの・ならないもの

| 今の正                                                                      | R1                                                                                                                                                                                                                                                                                                                                                            | R2b                                                              |
| --------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------- |
| broker の `DRIVERS`（`lib.rs:750-753`）                                     | **`DriverTable` を注入**（§4.4）。中身は組み立て役が `builtin()` で同じ 2 つを渡す。振る舞い不変                                                                                                                                                                                                                                                              | `FrameDriver` / `SessionDriver` をエンジンが直接使い、表は消える |
| `is_supported_protocol`（`:798-800`、呼び出し 4 か所）                      | **`HubSessions::is_broker_managed(&str)`（`DriverTable::is_supported`）に差し替え、`banto_broker::is_supported_protocol` は同じ PR（R1-3a）で消す**。4 か所（`hub.rs:1989`・`read_path.rs:180`・`rest.rs:8142`・`write_path.rs:676`）はどれも `manager.sessions()` が引ける。`#[deprecated]` は使わない（`clippy -D warnings` が呼び出し側で落ちる。監査 M9） | `registry.contains(id)`                                          |
| `banto-collect` の `ClientFactory`（既に注入式）                            | `hub_client_factory` の fallback（`broker_glue.rs:837`）を `HubDrivers.client_factory` に。中身は `default_client_factory()` のまま                                                                                                                                                                                                                           | `default_client_factory`・`ClientProtocol`・`Protocol` の撤去    |
| `ALLOWED_PROTOCOLS`（`plc_connection.rs:236`）と SQL の `CHECK`             | **変えない**。トリップワイヤ（§6.3）                                                                                                                                                                                                                                                                                                                          | R2b-1 で撤去、`banto-tags` の検証にレジストリの述語を注入        |
| `BrokerSimRegistry::resolve`（`broker_glue.rs:716-720`）                    | **変えない**（`"modbus-tcp"` 以外を SLMP に倒す `if / else` もそのまま。レジストリの id は 2 つなので同じ結果）                                                                                                                                                                                                                                               | R2b-5（値の層のシミュレータ）                                    |
| `"modbus-tcp"` でアドレスの解析を選ぶ 4 か所（`write_path` / `read_path`）  | **変えない**                                                                                                                                                                                                                                                                                                                                                  | R2b-2（`registry.get(id).parse_address`）                        |
| UI・設定パッケージの固定値（`configPackage.ts`・`ConnectionDrawer.svelte`） | 変えない                                                                                                                                                                                                                                                                                                                                                      | R2b-6 / R2b-7                                                    |

### 6.3 トリップワイヤ（`banto-drivers` の単体テスト、R1-3b / R1-4b）

1. `builtin().registry.ids()` のすべてが `banto_tags::ALLOWED_PROTOCOLS` に含まれる（`HostBound` の id は `plc_connections.protocol` の値と一致していなければ接続を引けない）。R1-4b から。
2. `builtin().broker.ids()` の集合 == `registry` のうち `HostBound` の id の集合（broker に居るのにレジストリに居ない、の逆も断る）。R1-4b から。
3. `builtin().client_factory` が `ClientProtocol::ModbusTcp` / `Slmp` の両方で `Box<dyn PlcClient>` を返す（`default_client_factory` そのものなので中身のテストは `task.rs:2051` の `default_client_factory_sends_the_configured_cpu_subcommand` に任せ、ここは「組み立て役が差し替えていない」ことだけ）。R1-3b から。
4. `registry` の各 `descriptor.id` が `DriverId` の文法に合う。R1-4b から。
5. `builtin().broker.ids()` == `{"slmp", "modbus-tcp"}`（R1-3b から。今の `DRIVERS` と同じ 2 つ）。

### 6.4 注入点（R1-3b）

core 側に `HubDrivers` を置く（core は `banto-drivers` に normal では依存しない。dev-dependency は R2b-0 まで持つ）:

```rust
// apps/banto-hub/core/src/broker_glue.rs
#[derive(Clone)]
pub struct HubDrivers { pub registry: Arc<DriverRegistry>, pub broker: banto_broker::DriverTable, pub client_factory: banto_collect::ClientFactory }
impl HubDrivers {
    pub fn new(registry: Arc<DriverRegistry>, broker: DriverTable, client_factory: ClientFactory) -> Self;
    /// ドライバ無し。DriverTable::new(vec![])、client_factory は NotConnected を返す偽クライアント。R2b-0 で core のテストが使う
    pub fn none() -> Self;
}
impl std::fmt::Debug for HubDrivers {}   // ClientFactory は Debug でないので手で書く（HubConfig が derive(Debug)、runtime.rs:203）
impl HubSessions { pub fn new(backoff: BackoffConfig, drivers: HubDrivers) -> Self; pub fn drivers(&self) -> &HubDrivers; pub fn is_broker_managed(&self, protocol: &str) -> bool; }
// HubConfig（runtime.rs:204）に pub drivers: HubDrivers を足す。build_hub_config_from_env(host_kind, drivers)（profile_paths.rs:307）
```

`HubSessions` は既に `CollectorManager::new`（`hub.rs:1162-1170`、`sessions: Arc<HubSessions>`）へ渡っているので、**`CollectorManager::new` の形は変えない**（呼び出し 36 か所、`rg -n 'CollectorManager::new\(' apps crates`、に触らない。監査 M8 の「R1-3 を (`DriverTable` + `HubSessions`) と (`CollectorManager` / `HubConfig`) に分ける」案は、`CollectorManager` を触らずに済むので 3a / 3b の分け方（3a = broker の表と `HubSessions` の内側、3b = 注入の配線）で受ける）。触る呼び出し側は次の 2 種で、着手時に grep で一覧を取って PR の説明に貼る:

| 呼び出し                     | 数（2026-10-19）                                                                                                                                        | 一覧の取り方                                                                        | 直し方                                                                                                                                                                                                                                         |
| ---------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `HubSessions::new(`          | 40（本番 `runtime.rs:511, :2988` ほか、残りはテスト。`opcua.rs:636`・`controller.rs:734`・`mqtt.rs:785`・`stream.rs:1423`・`rest.rs:10382, :10436` 等） | `rg -n 'HubSessions::new\(' apps/banto-hub/core apps/banto-hub-sink`                | 本番: `config.drivers.clone()`。`src` 内のテスト: `#[cfg(test)] pub(crate) fn test_drivers() -> HubDrivers`（`test_support.rs` に置き、`banto_drivers::builtin()` を `HubDrivers::new` に詰める）。`tests/`: `banto_drivers::builtin()` を直接 |
| `HubConfig {` リテラル       | 18（`profile_paths.rs:307` `build_hub_config_from_env` を含む）                                                                                         | `rg -n 'HubConfig \{' apps/banto-hub/core apps/banto-hub-sink apps/banto-hub/shell` | `drivers: test_drivers()` / `builtin()` を足す                                                                                                                                                                                                 |
| ChronoGazer `EngineConfig {` | 15                                                                                                                                                      | `rg -n 'EngineConfig \{' apps crates`                                               | **R1 では触らない**（§0.1 M7）                                                                                                                                                                                                                 |

| 場所                                                            | 今                                                              | R1                                                                                                         |
| --------------------------------------------------------------- | --------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------- |
| `HubRuntime::start(config)`（`runtime.rs:301`）                 | `HubSessions::new(BackoffConfig)`（`:511`）が中で broker を作る | `HubSessions::new(backoff, config.drivers.clone())`                                                        |
| `hub_client_factory`（`broker_glue.rs:837`）                    | `banto_collect::default_client_factory()` を fallback に組む    | `sessions.drivers().client_factory.clone()` を fallback に（中身は同じ）                                   |
| bin `banto-hub` / `banto-hub-shell` / `banto-hub-sink` のテスト | `HubRuntime::start`                                             | `banto-drivers` を dependency（sink は dev-dependency）に足し、`HubConfig.drivers` に `builtin()` を詰める |
| ChronoGazer `EngineRuntime` / `collect.rs:1919, 2573`           | `banto_collect::default_client_factory()`                       | **変えない**（R2b）                                                                                        |

## 7. 適合テスト一式（`banto-driver-conformance`、R1-5）

### 7.1 固定する振る舞い

| #   | 振る舞い                                                                                                                                                                                                                                                                                                                                                                                                                           | 対象                                       | どう確かめるか                                                                                                                                                                                     |
| --- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| C1  | `parse_address(text).canonical == parse_address(canonical).canonical`（正規化の冪等）、`offset_address(.., 0) == canonical`、fixtures の `expected` と一致。`canonical == Address::parse(text).to_string()`（Q8 = A、決定済み）                                                                                                                                                                                                    | `DriverBase`                               | 作者が渡す `StaticFixtures { valid: Vec<(text, kind, expected canonical)>, invalid: Vec<(text, kind)>, offsets: Vec<(canonical, kind, n, expected)> }` を総当たり                                  |
| C2  | `validate_config` は未知のキーを断らず、必須キーの欠けと型違いを `FieldError` で返す。`secret_keys` ⊆ 実際に読むキー                                                                                                                                                                                                                                                                                                               | `DriverBase`                               | 最小の `Settings`・空の `Settings`・`port = "abc"`                                                                                                                                                 |
| C3  | 結果の件数と順序 = 要求の件数と順序（`covers` の網羅、重複なし、`covers[i] < specs.len()`）。**同じアドレスを別の型で読む要求が別の値になる**（U16 と F32 等）                                                                                                                                                                                                                                                                     | Frame / Session                            | 要求 N 件（同じアドレスの重複・順序の入れ替え・同じアドレス別型を含む）                                                                                                                            |
| C4  | 致命 / 非致命の分類: 機器の拒否は `ItemError::Device`、壊れたフレームは `FatalError::Framing`、別プロトコルの表記 / 型と領域の不整合は `Unsupported`、文法違反のアドレスは `parse_address` で止まる。R1 の `LegacySession` では接続後の `InvalidAddress` が `FatalError::InvalidConfig`（致命のまま、§3.4 の表）になることも固定する。**フレーム型は `FatalError::Io` / `Timeout` を返さない**（型では防げないのでハーネスが見る） | Frame / Session                            | ピアに (a) 例外応答 (b) 応答コード / 長さの不整合 (c) 途中で切断、をさせる                                                                                                                         |
| C5  | 分割受信: 応答を 1 バイトずつ・境界をまたいで届けても同じ値                                                                                                                                                                                                                                                                                                                                                                        | Frame（ハーネス）/ Session                 | ハーネスが `FrameRule` を解釈して分割（Frame）。Session はモックの TCP サーバー / `FaultProxy` が分割送信。**SLMP の前提は `tyaro/slmp` v0.2.1**（本ブランチで済み。v0.2.0 では Framing で落ちる） |
| C6  | 遅れて届いた応答を次の要求に流用しない: step N を時間切れにし、その応答を N+1 の直前に流すと **接続致命**になり、N+1 の値として採用されない                                                                                                                                                                                                                                                                                        | Frame（ハーネス）/ Session                 | `banto-plc/src/error.rs:39-47` の規則。Frame 型では契約の主体はエンジンなので、ハーネス自身のテストでもある                                                                                        |
| C7  | 再接続: 接続致命の後、新しい `connect` / `create_connection` で読める。古い接続の状態（ハンドシェイクで得た値）を持ち越さない                                                                                                                                                                                                                                                                                                      | Frame / Session                            | ピアを一度落として立て直す                                                                                                                                                                         |
| C8  | 1 接続 = 1 ソケット（#337）: 読みと書きを交互に 100 回行う間、ピアが受けた接続は 1 本                                                                                                                                                                                                                                                                                                                                              | Session                                    | `FaultProxy` が accept を数える（`apps/banto-hub/core/tests/integration.rs` の `modbus_collection_uses_exactly_one_socket` と同じ観点）                                                            |
| C9  | `max_len` 超過・`DriverDecides` が完結しないまま伸びる → 致命（読み取りが止まらないことがない）。`Discard(n)` は進む（n >= 1）                                                                                                                                                                                                                                                                                                     | Frame                                      | ピアが延々と送る・先頭にごみを付ける                                                                                                                                                               |
| C10 | 書き込みの非合体: `write_next` は隣接しない要求を 1 つの step にまとめない（`banto-plc-write/src/client.rs:62-68` の gap-tolerance-zero）                                                                                                                                                                                                                                                                                          | Frame（write）                             | 離れた 2 アドレスを 1 バッチで                                                                                                                                                                     |
| C11 | パニックしない: 任意のバイト列（長さ 0〜max_len、乱数の固定シード 1,000 本）を `decode_read` / `is_complete` / `handshake_next` に渡しても `Err` で返る                                                                                                                                                                                                                                                                            | Frame                                      | `std::panic::catch_unwind(AssertUnwindSafe(..))` で包み（`&mut dyn FrameConnection` は `UnwindSafe` でない）、panic は失敗                                                                         |
| C12 | `TagKind::Str` の語数の範囲外（0・上限 + 1）は `Unsupported`（`PlcError::StringSpanUnsupported` と同じ）                                                                                                                                                                                                                                                                                                                           | Frame / Session                            |                                                                                                                                                                                                    |
| C13 | ワード内ビットの RMW が他のビットを保つ: 今の値が `0x0000`・`0x0100`・`0xFFFF`・`0x0008` のそれぞれで、ビット 3 を true / false に書くと、書き戻しは今の値の他のビットを保った値（例: `0x0100` に true → `0x0108`）。同じワードの複数ビットの要求は 1 回の RMW にまとまる                                                                                                                                                          | Frame（write）/ Session（`LegacySession`） | SLMP は R1 で `LegacySession` 経由。書き戻しのフレームの中身を `FaultProxy` で記録して照合                                                                                                         |
| C14 | 確認読みの不一致は、そのビットの要求だけ `ItemError::Rejected`。同じワードで正しく反映されたビットの要求は成功のまま（`slmp/mod.rs:404-416`）                                                                                                                                                                                                                                                                                      | Frame（write）/ Session                    | 確認読みの応答を `FaultProxy` で書き換える                                                                                                                                                         |
| C15 | 途中の非致命のエラー（読み取り・書き戻し・確認読みの各段の機器の拒否）で、そのまとまりの要求が `ItemError::Device` になり、それ以上 step を送らない。接続は保ち、次の書き込みは最初から正しく動く                                                                                                                                                                                                                                  | Frame（write）/ Session                    | 各段で終了コードを返すシミュレータの口（`slmp/simulator.rs` の `inject_end_code`）                                                                                                                 |
| C16 | 割り込みと上限: 書き込みの途中に別の読み取りを投げても、`Done` まで同じ接続には送られない。step 数の上限を超える偽ドライバは致命で切られる。途中で致命になったあとの次の書き込みは、新しい接続で `write_next(None)` から始まる                                                                                                                                                                                                     | Frame（write）                             | R2b-3（エンジンができてから）。R1 では偽ドライバとハーネスで手順だけ確かめる                                                                                                                       |

### 7.2 形（第三者が Hub なしで走らせる）

```rust
// 第三者の crate の tests/conformance.rs
use banto_driver_conformance::{frame, Failure, StaticFixtures};
#[tokio::test]
async fn conforms() {
    let driver = my_driver::Driver::default();
    let mut peer = my_driver::test_peer::Responder::new();
    let failures: Vec<Failure> = frame::run_all(&driver, &StaticFixtures { .. }, &mut peer).await;
    assert!(failures.is_empty(), "{failures:#?}");
}
```

- `frame::run_all(driver: &dyn FrameDriver, fixtures: &StaticFixtures, peer: &mut dyn Responder) -> Vec<Failure>`: `Responder { fn reply(&mut self, request: &[u8]) -> Reply }`、`Reply { Bytes(Vec<u8>) | Chunks(Vec<Vec<u8>>, Duration) | Silence | Close }`。ハーネスが §4.2 の手順を実行し C1〜C12 を順に走らせ、失敗した項目を一覧で返す（`Failure { id: &'static str, detail: String }`。`Result` ではなく `Vec` で返すのは 1 回の実行で全部の失敗を見るため）。
- `session::run_all(driver: &dyn SessionDriver, settings: &Settings, fixtures: &SessionFixtures, peer: &mut dyn Responder) -> Vec<Failure>`: ハーネスが `tokio::net::TcpListener` を 127.0.0.1 の空きポートに立て、`Responder` に委ねる。Hub のコードは一切要らない。
- `session::run_all_against(driver, settings, fixtures, peer: SocketAddr, faults: FaultProxy) -> Vec<Failure>`: **ピアが既に TCP サーバーとして立っている**とき（`banto-plc` の Modbus シミュレータ・SLMP シミュレータは `Responder` ではなく TCP サーバー）に使う。`FaultProxy` は 127.0.0.1 の空きポートで listen してピアへ中継し、遅延・分割・切断・応答の差し替え（先頭 n バイトを書き換える）・accept の計数を挟む。
- `banto_driver_conformance::static_checks(driver: &dyn DriverBase, fixtures: &StaticFixtures) -> Vec<Failure>`（C1・C2 だけ。tokio 不要）。
- `SessionFixtures { reads: Vec<ReadCase { text, kind, expected: Value }>, refused: Vec<(text, kind)>, unsupported: Vec<(text, kind)>, writes: Vec<WriteCase { text, kind, value }> }`。`reads.expected` はピア（シミュレータ）が返すと分かっている値。`refused` はピアが拒否応答を返す入力（SLMP シミュレータは未対応コマンド / デバイスに `END_CODE_WRONG_COMMAND`、`banto-plc-write/src/slmp/simulator.rs:360-393`。Modbus シミュレータは決まった例外コードを返す口を持つ: `banto-plc/src/modbus/simulator.rs:14, :78-80`。どちらも足りなければ `FaultProxy` の応答の差し替えで拒否フレームを作る）。

### 7.3 Modbus / SLMP での走らせ方（CI）

R1 の Modbus / SLMP は `HostBound` なので、`frame::run_all` は走らせられない。R1-5 でやること:

1. `static_checks` を `banto-driver-modbus` / `-slmp` の `tests/conformance.rs` で走らせる（C1・C2。fixtures は `banto-plc/src/address.rs` と `slmp/address.rs` の既存テストの入力から写す。`offsets` は TS の `continuousRegistration.test.ts` / `offsetCopy.test.ts` から写す）。
2. `session::run_all_against` を **dev-only のアダプター** `LegacySession`（`Box<dyn BrokerSession>` を `api::Session` に包む。`TagSpec.address.canonical` を `Address::parse` / `parse_slmp` で `BatchReadRequest` に戻す。R1-4b の `classify_read` / `classify_write` でエラーを写す）で走らせる。ピアは `banto-plc` の Modbus シミュレータ（`modbus/simulator.rs`、feature `simulator`）と SLMP シミュレータ（`banto-plc/src/slmp/simulator.rs`・`banto-plc-write/src/slmp/simulator.rs`）を TCP サーバーのまま使い、C4 の壊れたフレーム・C6 の遅延・C8 の計数は `FaultProxy` で起こす。
3. これで §7.1 のうち C1〜C8・C12 が Modbus / SLMP で CI に乗る。書き込みの手順の C13〜C15 は **SLMP だけ**が R1 で乗る（`LegacySession` の `write_batch` が今の `execute_one_bit_write` を通る。確認読みの不一致は `FaultProxy` の「k 番目の応答の差し替え」で起こす）。Modbus はワード内ビットの書き込みが未対応（`banto-plc-write/src/modbus/planning.rs:258-270` で `UnsupportedCombination`）なので、C13〜C15 の fixtures を空にし、「`.N` 付きの書き込みが `Unsupported` になる」ことだけを固定する。C9〜C11・C16 は Frame 型になる R2b-3 で乗る（C16 の R1 での相当は broker の `serialized_concurrent_requests_do_not_corrupt_each_other`、`lib.rs:1822`）。
4. **P0-3（refactor-plan §8.3）との関係**: P0-3 は `banto-plc` / `banto-collect` の層で「今のクライアント」の振る舞いを固定する 6 本（Modbus の分割受信・遅延応答・閉じたポート・型の混ざった一括読み取り、collect の 1 接続 = 1 ソケット、SLMP の分割受信）で、R1-5 とは層が違う。**両方残す**。R2b-3 の完了条件は「P0-3 の 6 本と §7.3 の `session::run_all_against` / `frame::run_all` の両方が新しいエンジンで通る」（refactor-plan §4 R2b の完了条件と同じ意味）。P0-3 には当初 SLMP の分割受信が無かった（v0.2.0 の時点で足さないと決めたため）。v0.2.1 で前提が変わったので、**P0-3 に 6 本目として足した**（2026-10-19 オーナー決定、§0.1。refactor-plan §8.3 の表の 6）。R2b-3 の完了条件の「P0-3」はこの 6 本を指す。

### 7.4 偽ドライバ

`banto_driver_conformance::fake`: `FakeSessionDriver`（`HashMap<canonical, Value>` を返す。`fail_next: Option<FatalError>` で致命を起こせる）と `EchoFrameDriver`（§4.3）。R2b-0 の後に core に残るテストと、R2a の `push` の実装前のテストが使う。

## 8. シリアルを見込んで R1 で押さえる形（R2b-3 で API を壊さないため）

| 論点                                     | R1 の型での扱い                                                                                                                                                                                                                                                                                                                                                                                                  |
| ---------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| トランスポートの種類                     | `DriverDescriptor.transports: Vec<Transport { Tcp, Serial, SerialOverTcp }>` と設定キー `transport`。R1 は `Tcp` だけを受け、ほかは `validate_config` で断る（`FieldError`）。R2b-3 でエンジンが `Serial`（COM 直結）と `SerialOverTcp`（変換器へ生データ）を足しても API は変わらない                                                                                                                           |
| フレームの終わりを無音の間隔で判定しない | `FrameRule` に「無音」の変種を**入れない**（plan §7 Q4）。終端は `Fixed` / `LengthPrefixed` / `Terminator` / `DriverDecides` のどれか。Modbus RTU は `DriverDecides` で **FC とバイト数から長さを決め、CRC は検証にだけ使う**（§4.2）。SLMP の ASCII フレームは `LengthPrefixed` の `PrefixEncoding::AsciiHex`                                                                                                   |
| 先頭のごみ・自己エコー                   | RS-485 の 2 線式で自分の送信が受信側に戻る、前の応答の残りが先頭に付く。`is_complete` が `FrameCheck::Discard(n)` で先頭を捨てられる（C9）                                                                                                                                                                                                                                                                       |
| 変換器を通る分のタイムアウト             | 接続設定の `response_timeout_ms`（接続ごと）。`Step` には持たせない。`#[non_exhaustive]` なので、半二重の切り替え待ち（`turnaround`）が要ると分かったら `Step` に `Option<Duration>` を**後から足せる**（§9）                                                                                                                                                                                                    |
| RS-485 のマルチドロップを 1 ポートで     | **モデルは 1 つ: 「1 接続 = 1 機器（局番は接続設定の `unit_id` / `station`）、1 つのポート（COM / 変換器）を複数の接続で共有するのはエンジン」**。`DriverAddress` に局番は持たせない（§0.1 Q6）。エンジン（R2b-3）はポートごとに 1 つの直列化キューを持ち、接続（局番）をまたいで要求 - 応答を 1 つずつ流す。ドライバは局番を `Settings` から読んで各 `Step.send` に入れる。R1 の Modbus TCP は `unit_id` がこれ |
| 切断の前の挨拶                           | `FrameConnection::farewell`（既定 `None`）。EtherNet/IP の UnRegisterSession 等。応答は待たない                                                                                                                                                                                                                                                                                                                  |
| シリアルの速度等は変換器側               | `Settings` は自由なキーなので、COM 直結のときだけ `serial_port` / `baud` 等を足す。R1 では予約名だけ決める（`keys::SERIAL_PORT`・`keys::STATION`）                                                                                                                                                                                                                                                               |

## 9. 版と安定性の方針

### 9.1 semver

- R1 で **`0.1.0`**（`HostBound` を含む）を公開（Q3、§0.1）。R2b-3 で `HostBound` を消して **`0.2.0`**。以降も 0.x の間は **minor で破壊**し、patch は互換のみ。WIT が確定する W 段で `1.0.0`。alpha / beta は使わない（`^0.1.0-alpha.1` の利用者が alpha 同士の破壊を自動で踏むため）。**変更履歴（`crates/banto-driver-api/CHANGELOG.md`）に「何が壊れ、どう直すか」を必ず書く**（plan §5）。
- **`cargo-semver-checks` を CI に足す**（R1-1）: `ci.yml` の `driver-api` ジョブ（R1-1 で rust ジョブではなく、MSRV の check と同じ ubuntu の軽いジョブに置いた。対象に `banto-driver-api` が入るときだけ走る）に `cargo semver-checks check-release -p banto-driver-api`（`cargo install cargo-semver-checks --locked`）。baseline は直前のタグ（初回は `0.1.0` のタグができるまでスキップ）。**タグの名前は `banto-driver-api-v<版>`**（例 `banto-driver-api-v0.1.0`。ワークスペースのリリースのタグとは別。R1-1 で決めた。`crates/banto-driver-api/CHANGELOG.md` の冒頭にも書いた）。minor を上げる PR では `--baseline-rev` を書いて期待どおりの破壊だけであることを確かめる。
- 破壊になるもの: trait への既定実装の無いメソッドの追加 / 型の変更 / enum の variant の削除・改名 / struct の field の削除・改名 / `DriverId` の文法の縮小 / **§3.1 の移す型（`#[non_exhaustive]` なし）への variant の追加**。
- 破壊にならないもの（新しい型の `#[non_exhaustive]` のおかげ）: enum の variant の追加 / struct の field の追加 / 既定実装つきのメソッドの追加 / 設定キーの追加。利用側は `..` を使った match と `Step::new` 等のコンストラクタを使う（適合テストの `examples` がその書き方の手本）。
- **MSRV**: `rust-version` は **暫定 `1.85`**（std だけの crate なので低くできる。1.98 はワークスペースのピンで、第三者には新しすぎる）。R1-1 で CI に MSRV ジョブ（`dtolnay/rust-toolchain@1.85` + `cargo check -p banto-driver-api`、R1-2 からは `-p banto-driver-conformance` も）を足し、通らなければ通る最も低い版に `rust-version` を上げる。ワークスペースの `rust-toolchain.toml`（1.98.1）はこのジョブでは使わない（`RUSTUP_TOOLCHAIN` で上書き）。上げるときは minor を上げ、CHANGELOG に書く。
- `banto-driver-conformance` は API に `{ path, version }` で依存し、API の minor が上がったら合わせて上げる。

### 9.2 WIT との対応表（R1 の trait ↔ plugin-design §6.2）

本書で WIT を変えた点（plugin-design §6.2 を同じ PR で直した）: `write.encode` / `write.check` を状態機械の `write.write-next` に置き換える（§4.2.1）、`address.parse` / `address.offset` に `string-words` を足す、`read.decode` / `write.check` に `specs` / `items` を渡す、`item-error` に `rejected` を足す、`prefix-spec` に `encoding` を足す、`is-complete` が `frame-check` を返す、`data-type` の綴りを `DataType` の `Display`（`bit / i16 / u16 / i32 / u32 / f32 / i64 / u64 / f64`、`types.rs:79-93`）と `string` に固定する、`write.value` の書き込み側が使う variant を限定する。

| Rust                                                               | WIT                                                                                | 備考                                                                                                                                                                                                                                     |
| ------------------------------------------------------------------ | ---------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `DriverDescriptor`                                                 | `manifest.json`（§4.2）                                                            | `api_version`（u32）↔ `hubApi`（文字列）の主版                                                                                                                                                                                           |
| `DriverBase::validate_config`                                      | `config.validate`                                                                  |                                                                                                                                                                                                                                          |
| `DriverBase::secret_keys`                                          | `config.secret-keys`                                                               |                                                                                                                                                                                                                                          |
| `DriverBase::parse_address(text, kind)`                            | `address.parse(text, data-type, string-words)`                                     | `kind` = `data-type` + `string-words`                                                                                                                                                                                                    |
| `DriverBase::offset_address(canonical, kind, n)`                   | `address.offset(canonical, data-type, string-words, n)`                            |                                                                                                                                                                                                                                          |
| `DriverBase::address_help`                                         | `address.help`                                                                     |                                                                                                                                                                                                                                          |
| `DriverAddress`                                                    | `address.parsed`                                                                   | `driver` は Rust 側の追加（WIT では文脈から分かる）。`word_count` は u16 ↔ u32                                                                                                                                                           |
| `FrameDriver::create_connection`                                   | `connection.create(settings)`                                                      |                                                                                                                                                                                                                                          |
| `FrameConnection::handshake_next`                                  | `connection.handshake-next`                                                        |                                                                                                                                                                                                                                          |
| `FrameConnection::farewell`                                        | `connection.farewell`（新規、`option<step>`）                                      | WASM 側の既定は `none`                                                                                                                                                                                                                   |
| `FrameConnection::is_complete(step, buf)`                          | `connection.is-complete(step, buf) -> frame-check`                                 | `FrameCheck` ↔ `frame-check { incomplete, complete(u32), discard(u32) }`                                                                                                                                                                 |
| `FrameConnection::plan_read`（`&mut self`）                        | `read.plan(borrow<connection>, specs)`                                             | WIT の `borrow` は可変性を区別しない。接続は 1 タスクが専有する                                                                                                                                                                          |
| `FrameConnection::decode_read(planned, specs, response)`           | `read.decode(borrow<connection>, p, specs, response)`                              | `specs` は `plan` に渡したものと同じ                                                                                                                                                                                                     |
| `FrameConnection::write_next(items, prev_response)`（`&mut self`） | `write.write-next(borrow<connection>, items, option<list<u8>>)`                    | §4.2.1。旧案の `write.encode` / `write.check` を置き換えた（2026-10-19 レビュー）                                                                                                                                                        |
| `TagKind::Numeric(DataType)` / `TagKind::Str{words,..}`            | `tag-spec.data-type` / `tag-spec.string-words`                                     | `data-type` の綴りは `DataType` の `Display` と `"string"`。`encoding` は WIT に無い（Hub が decode）                                                                                                                                    |
| `RawValue`                                                         | `types.value`                                                                      | 読み取りの戻り                                                                                                                                                                                                                           |
| `WriteItem.value: Value`                                           | `write-item.value: types.value`                                                    | WASM ホストは `Bit → bool`、`F64 → float`、`Str → bytes`（`encoding` で符号化）に写す。**書き込み側で `int` / `uint` は使わない**。整数型への丸めと範囲の検査はプラグイン側（`encode.rs:192` の `require_integral_in_range` と同じ規則） |
| `Value`                                                            | -（Hub 側の型）                                                                    |                                                                                                                                                                                                                                          |
| `ItemError` / `DeviceError`                                        | `types.item-error { device, unsupported, rejected }` / `device-error`              |                                                                                                                                                                                                                                          |
| `FatalError::{Framing, Protocol, InvalidConfig}`                   | `types.fatal-error`                                                                | `Io` / `Timeout` は WIT に無い（Hub 側）。フレーム型は返さない（C4）                                                                                                                                                                     |
| `FrameRule` / `PrefixSpec` / `PrefixEncoding` / `Step` / `Planned` | `session.frame-rule` / `prefix-spec` / `prefix-encoding` / `step` / `read.planned` | `DriverDecides` ↔ `plugin-decides`                                                                                                                                                                                                       |
| `SessionDriver` / `Session`                                        | -（WIT の対象外。組み込みのみ）                                                    |                                                                                                                                                                                                                                          |
| `DriverKind::HostBound`                                            | -（R1 限定）                                                                       |                                                                                                                                                                                                                                          |

W 段で WASM ホストを足すときは `WasmFrameDriver: FrameDriver`（wasmtime の `Store` を `FrameConnection` が持つ）が「もう 1 つの実装」になる。上の表に無い関数を WIT に足す必要が出たら、本書と plugin-design §6.2 を同じ PR で直す。

## 10. R1 の PR の分け方（plan §4 の R1-1〜R1-7 を詳細化）

共通の決まり: 各 PR は振る舞いを変えない。`cargo fmt --all --check` → `cargo test -p <触った crate>` → `cargo clippy -p <触った crate> --all-targets -- -D warnings` → **`cargo check --workspace --all-targets`**（公開 API の形を変える PR が多いので毎回。implementation-checklist §2-3）→ `pnpm exec prettier --write <触った md/mjs>` → 新しいディレクトリ / crate を足した PR は `ci-changes.test.mjs` の固定グラフ G にその crate を足し、`git add` 後に `node --test .github/scripts/ci-changes.test.mjs`（§2-6、#632）→ `cargo deny check`。E2E は R1-4c・R1-7a だけ（Hub）。ロールバックはどれも「その PR を revert」で戻る（DB のマイグレーションを含む PR は無い）。順番は表のとおり（R1-6 は R1-7a の後）。

| PR    | 範囲                                                                                                                                                                                                                                                                                                                                                                                                                                       | 触るファイル                                                                                                                                                                                                                                                                                                                                                                 | 不変条件・完了条件                                                                                                                                                                                                                                                                                                                                                                               |
| ----- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| R1-1  | `banto-driver-api` を作り §3 の型を入れる。`banto-plc` の `DataType`（`register_span` を `pub` で一緒に）・`TagValue`・`PlcValue`（`From` / `as_tag_value` も）・`StringEncoding`・`WordOrder` を API からの再輸出に替える（**`#[non_exhaustive]` を付けない**）。`DriverAddress`・`ItemError`・`FatalError`・`Settings`・`TagKind`・`TagSpec`・`WriteItem`・`RawValue` を追加（コンストラクタつき）。CI に semver-checks と MSRV のジョブ | 新 `crates/banto-driver-api/{Cargo.toml,src/lib.rs,src/types.rs,src/address.rs,src/error.rs,src/settings.rs,CHANGELOG.md}`、ルート `Cargo.toml`（members）、`crates/banto-plc/{Cargo.toml,src/types.rs,src/decode.rs,src/planning.rs,src/slmp/planning.rs,src/lib.rs}`、`.github/workflows/ci.yml`、`.github/scripts/ci-changes.test.mjs`（G に追加）                        | `cargo publish --dry-run -p banto-driver-api` が通る。`banto_plc::DataType` 等を使う呼び出し側に差分が無い（`git diff --stat` で `apps/` と `crates/banto-plc-write` が 0）。`DataType::parse` の受理集合が変わらない（既存テスト `data_type_parse_*`）。`cargo test --workspace` の件数が前後で同じ。MSRV ジョブが緑                                                                            |
| R1-2  | §4・§5・§6.1 の trait と `DriverRegistry`。`banto-driver-conformance` の骨格（`static_checks`、`frame::run_all`、`EchoFrameDriver`、`FakeSessionDriver`、`FaultProxy`）。`banto-broker` の `BrokerSession` / `SessionError` / `Connector` を `pub` に                                                                                                                                                                                      | `crates/banto-driver-api/src/{driver.rs,frame.rs,session.rs,registry.rs}`、新 `crates/banto-driver-conformance/`、`crates/banto-broker/src/{session.rs,lib.rs}`                                                                                                                                                                                                              | `EchoFrameDriver` が `frame::run_all` の C3〜C7・C9〜C12 を通る（ハーネス自身の検証。C3 は「同じアドレス別型」を含む）。`cargo publish --dry-run -p banto-driver-api -p banto-driver-conformance` が通る。`cargo doc -p banto-driver-api --no-deps` が警告ゼロ。`broker` の `pub` 化は呼び出し側に影響なし                                                                                       |
| R1-3a | broker の `DriverTable` / `DriverDial` / `DialFn`（§4.4）。`BrokerSupervisor::spawn(.., drivers)`。`DriverTable::builtin()`（橋）。`HubSessions` が表を持ち `is_broker_managed` を出す（`new(backoff)` の形はこの PR では変えず、中で `builtin()`）。`is_supported_protocol` の呼び出し 4 か所を `manager.sessions().is_broker_managed(..)` に直し、`banto_broker::is_supported_protocol` を消す                                           | `crates/banto-broker/src/lib.rs`、`apps/banto-hub/core/src/{broker_glue.rs,hub.rs,read_path.rs,rest.rs,write_path.rs}`                                                                                                                                                                                                                                                       | 既存テスト全通過（broker の単体テストはまだ動かさない）。`rg is_supported_protocol` が 0 件                                                                                                                                                                                                                                                                                                      |
| R1-3b | `banto-drivers` と `builtin()`（`broker = DriverTable::builtin()`、`client_factory = default_client_factory()`）。core の `HubDrivers`・`HubConfig.drivers`・`HubSessions::new(backoff, drivers)`・`build_hub_config_from_env(kind, drivers)`・`hub_client_factory` の fallback。呼び出し側（§6.4 の grep）。bin・shell・sink の `Cargo.toml`。トリップワイヤ 3・5                                                                         | 新 `crates/banto-drivers/`、`apps/banto-hub/core/src/{broker_glue.rs,runtime.rs,profile_paths.rs,test_support.rs,bin/banto-hub.rs}` と §6.4 の grep に出るテスト、`apps/banto-hub/core/Cargo.toml`（dev-dependency に `banto-drivers`）、`apps/banto-hub/shell`・`apps/banto-hub-sink` の `Cargo.toml` とテスト、`ci-changes.test.mjs`（G）                                  | 既存テスト全通過。`cargo test -p banto-drivers`。`cargo metadata` で `banto-hub-core` の normal deps に `banto-drivers` が**無い**（dev のみ）                                                                                                                                                                                                                                                   |
| R1-4a | `slmp_driver.rs` / `modbus_driver.rs`・`slmp_config_for` / `modbus_config_for` とそれらの単体テスト・`examples/real_plc_verify.rs` を `git mv` でドライバ crate / `banto-drivers` へ（visibility は `pub(crate)` → `pub` を必要な分だけ）。`dial` を公開。`DriverTable::builtin()` と `spawn_task` を消し、`builtin()` が driver crate の `dial` で表を組む。broker の単体テストは §4.4 の表のとおり 2 つに分ける                          | 新 `crates/banto-driver-{modbus,slmp}/`（`tests/broker.rs` を含む）、`crates/banto-broker/src/{lib.rs,slmp_driver.rs→移動,modbus_driver.rs→移動}`、`crates/banto-broker/examples/real_plc_verify.rs→移動`、`crates/banto-drivers/{Cargo.toml,src/lib.rs,examples/}`、`ci-changes.test.mjs`（G）                                                                              | `git log --follow` で移動が追えること。`modbus_driver_shares_one_transaction_id_counter_across_reads_and_writes`・`slmp_config_for_*` と §4.4 の表の「プロトコルの振る舞い」がドライバ crate で、`spawn_task` を使っていた 4 本ほかが broker で偽 `Connector` で通る。`cargo test --workspace` の件数が前後で同じ。`cargo metadata` で `banto-broker` の deps（dev 含む）にドライバ crate が無い |
| R1-4b | `DriverBase` の実装（`parse_address` は `Address::parse*` を包む、`offset_address` / `address_help` は TS からの移植、`classify_read` / `classify_write` の表 §3.4、`settings_from_connection`）。`Builtin.registry`・`HubDrivers.registry`。トリップワイヤ 1・2・4                                                                                                                                                                        | `crates/banto-driver-{modbus,slmp}/src/{lib.rs,base.rs,classify.rs}`、`crates/banto-drivers/src/lib.rs`、`apps/banto-hub/core/src/broker_glue.rs`                                                                                                                                                                                                                            | `classify_*` の単体テストが両 enum の全 variant を並べ、`is_connection_fatal() == false` ⇔ `Ok(ItemError)`。`offset_address` の fixtures が `continuousRegistration.test.ts` / `offsetCopy.test.ts` の期待値と一致。`address_help` が `tagAddressHelp.test.ts` の文言と一致。本番経路は変わらない（`git diff --stat` で `apps/` は `broker_glue.rs` だけ）                                       |
| R1-4c | `hub_client_factory` の腕の統合（`spec.protocol` に依らず `broker_handles` を引く 1 本）                                                                                                                                                                                                                                                                                                                                                   | `apps/banto-hub/core/src/broker_glue.rs`                                                                                                                                                                                                                                                                                                                                     | Hub の E2E が通る。`apps/banto-hub/core/tests/integration.rs` の `modbus_collection_uses_exactly_one_socket` が通る                                                                                                                                                                                                                                                                              |
| R1-5  | §7.3 の 1・2（`static_checks`、`LegacySession` での `session::run_all_against`）。Modbus / SLMP の fixtures                                                                                                                                                                                                                                                                                                                                | `crates/banto-driver-{modbus,slmp}/tests/conformance.rs`、`crates/banto-driver-conformance/src/{session.rs,proxy.rs}`                                                                                                                                                                                                                                                        | C1〜C8・C12 が両ドライバで緑。**反証**: `LegacySession` の `classify_read` で `SlmpEndCode` を `Fatal` に倒すと C4 が落ちる、`FaultProxy` の分割を外しても C5 が通る（slmp v0.2.1 が前提。v0.2.0 に戻すと落ちることを 1 度だけ手で確認して PR に書く）。`.config/nextest.toml` の test-groups に入れる必要が無いこと（空きポートを都度取る）を確認                                               |
| R1-7a | banto-hub の bin を `banto-hub-host` へ（§2.4）。マニフェスト埋め込みの移動と確かめるテスト。ビルドの入口の package 名を全部直す。`ci-changes.mjs` の `RUST_PACKAGES.hub` に `banto-hub-host`、`JOB_PACKAGES.run_e2e_hub = ['banto-hub-host']`、`run_linux_clippy` に host、`ci-changes.test.mjs:146` の `HUB_RUST`                                                                                                                        | `apps/banto-hub/{core,host}/`、ルート `Cargo.toml`、`.github/workflows/{ci.yml,opcua-interop.yml}`、`.github/scripts/ci-changes.{mjs,test.mjs}`、`scripts/build-release.ps1`、`apps/banto-hub/installer/src/main.rs`、`AGENTS.md`、`apps/banto-hub/README.md`、`apps/banto-hub/vite.config.ts`、`apps/banto-hub/core/src/assets.rs`、`docs/implementation-checklist.md` §2-7 | `cargo build -p banto-hub-host --bin banto-hub --features embed-ui` で Hub の E2E が通る。`win_service_tests.rs` とマニフェストのテストが host 側で通る。インストーラ（`build-release.ps1`）の dry run。`cargo test -p banto-hub-core` の件数が前後で同じ（`tests/` は動かさない）                                                                                                               |
| R1-6  | CI の判定の期待値。`ci-changes.test.mjs` に「`crates/banto-driver-modbus` だけの変更 → `rust_args` に `banto-driver-modbus`・`banto-drivers`・`banto-hub-host`・`banto-hub-shell`・`banto-hub-sink`・**`banto-hub-core`（dev 経由）** が入り、`chronogazer-core` は入らず、`run_e2e_hub` が true・`run_e2e_chronogazer` が false」の行を足す（R2b-0 で core を外す期待値に書き換える）                                                     | `.github/scripts/ci-changes.test.mjs`（必要なら `ci-changes.mjs` のコメント）                                                                                                                                                                                                                                                                                                | `node --test .github/scripts/ci-changes.test.mjs` 緑。「リポジトリの全ファイルが明示の規則に当たる」「crates/ の下の追跡ファイルはすべてどれかのパッケージに属する」の 2 本が新ディレクトリで落ちないこと（#632 の再発防止）                                                                                                                                                                     |

R1 の完了条件（plan §4）: 既存テストが全部通り、Modbus / SLMP が適合テスト（§7.3 の範囲）を通る。推定の規模: R1-1・R1-2 は各 1 PR・中、R1-3a は小、R1-3b は中（機械的だが呼び出し側が約 60 か所）、R1-4a は中（移動）、R1-4b は中（TS の移植を含む）、R1-4c は小、R1-5 は中、R1-7a は中（機械的だが入口が多い）、R1-6 は小。

**R2b-0（旧 R1-7b、R1 の外）**: §2.4 の最後の項。完了条件は「`node --test` で `crates/banto-driver-modbus/src/lib.rs` だけ → `rust_args` に `banto-hub-core` が**無く**、`run_e2e_hub` が true」「`cargo metadata` の実グラフで `banto-hub-core` の deps（dev 含む）に `banto-drivers` が無い」「移動前後で `cargo test --workspace` の件数が同じ（削除なし）」。

## 11. 書き込みの安全との関係（tag-server-design §6、D5）

- API の `DriverAddress.writable_area` と `Capabilities.write` は**プロトコルの事実**であり、書き込みの許可の判断ではない。`write_path.rs` の関門（`writable`・API キーの `write` スコープ・受付トグル・頻度・監査・broker の直列化）は R1 で一切触らない（§10 で触るのは R1-3a の `is_supported_protocol` の呼び出し 1 行だけ）。
- `FrameConnection::write_next` の非合体（C10）は `banto-plc-write` の gap-tolerance-zero をそのまま API の契約にしたもの。
- 書き込みの状態機械（§4.2.1）はプロトコルの手順（RMW と確認読み）だけを表す。関門と監査の先書きは最初の `write_next(None)` より前に Hub が済ませ、1 回の論理書き込みの間は同じ接続に他の要求を挟まない（今の broker の 1 ジョブ = 1 書き込みと同じ）。
- `ItemError::Rejected` は `ConflictingBitWrite` / `BitWriteVerificationFailed` の写しで、今と同じく非致命（そのタグだけ Bad、接続は保つ）。RMW の確認読みの不一致を「成功」に倒さないための variant で、関門の判断には使わない。

## 12. オーナー判断（2026-10-19 すべて決定済み）

2026-10-19 にオーナーが Q1〜Q7 を「推奨で」と決めた。監査で推奨を変えた Q3・Q4（M7 の ChronoGazer の扱いを含む）・Q6 と、新しい Q8・P0-3 の 6 本目も、**同日オーナーが再確認し §0.1 の「監査後の推奨」の列のとおりに決定した**。未決の問いは本書に残っていない。

| Q   | 問い                                                                                                                                                  | 推奨 → 決定                                                                                                                                                                                                                                                                                                                                                                                                                               |
| --- | ----------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Q1  | 適合テスト一式を plan §4 R1-5 の「`banto-driver-api` に置く」から**別 crate `banto-driver-conformance`** に変えてよいか（§2.2）                       | **決定（2026-10-19）**: 別 crate。API を依存ゼロに保つため。plan §4 の R1-5 の行を直した                                                                                                                                                                                                                                                                                                                                                  |
| Q2  | R1 の CI の短縮が「アダプター crate と組み立て役の変更」に限られ、`banto-plc` の実装本体の変更は R2b まで core に届き続けること（§2.5）を受け入れるか | **決定（2026-10-19）**: 受け入れる。実装本体を R1 で移すと `rest.rs` 等の `banto_plc::` 直接参照 10 か所超を同時に直すことになり R2b-2 と重なる。plugin-design §12.6 の表に「R1 ではアダプター分だけ」と注記する。（監査後の補足: core 自身も R2b-0 までは dev-dependency で対象に残る）                                                                                                                                                  |
| Q3  | crates.io への初回公開の時期: (a) R1 完了直後に公開（`HostBound` 入り）、(b) R2b-3 で `HostBound` を消してから公開                                    | **決定（2026-10-19）**: (a)。**版の表記は監査で変更し、同日オーナーが再確認して決定**: `0.1.0-alpha` ではなく **`0.1.0`**（§9.1、§0.1）。名前の予約と、別リポジトリ先行（tag-server-design §3.4.1 の staging）の KV / OPC UA のアダプターが `DriverBase` と `SessionDriver` を試せる                                                                                                                                                      |
| Q4  | R1-7 を 7a / 7b に分け、7b（統合テストの移動）まで R1 に含めるか                                                                                      | **決定（2026-10-19、監査で変更しオーナーが再確認）**: 7a だけを R1 に含め、**7b は R2b-0 として R2b の冒頭**にする（§0.1、§2.4）。7b は「移動だけ」では済まない（core の `src` 内の単体テストが実ドライバを使う）。同じく **ChronoGazer の `chronogazer-host` 分離と `EngineConfig.client_factory` への注入も R2b へ送る**（監査 M7。R1 の `client_factory` は `default_client_factory()` そのものなので ChronoGazer に入れる理由が無い） |
| Q5  | R2a で ChronoGazer の `hub` 接続（`ExternalSource`）も `SessionDriver` に寄せるか                                                                     | **決定（2026-10-19）**: R2a で `push` を `SessionDriver` にし、`ExternalSource` は R2a の完了条件（同じ値・品質）を見てから R2b で寄せる。R1 では触らない                                                                                                                                                                                                                                                                                 |
| Q6  | `DriverAddress.station` を R1 の型に入れるか（シリアルの先取り、§8）                                                                                  | **決定（2026-10-19、監査で変更しオーナーが再確認）**: **入れない**。局番は接続設定（`keys::UNIT_ID` / `STATION`）、ポートの共有はエンジン（§8）。アドレスに局番を持つとマルチドロップのモデルが 2 つになる                                                                                                                                                                                                                                |
| Q7  | ドライバ crate の名前: `banto-driver-modbus` / `banto-driver-slmp`（本書）か `banto-plc-modbus` 系か                                                  | **決定（2026-10-19）**: `banto-driver-*`。API の公開後に別リポジトリへ出す単位（plan §7 Q2）と同じ名前にしておく                                                                                                                                                                                                                                                                                                                          |
| Q8  | **（新規）** Modbus / SLMP の `canonical` の規則: A（今の `Display`: `400001` / `400001.3`）、B（入力のトリム）、C（A + 表示用の別列）                | **決定（2026-10-19、§0.1）**: A（§3.3 の表）。B は 1 つの `Address` に複数の正規形ができ、R2b-2 の照合が成り立たない。R1 では値を計算するだけで DB は変えない                                                                                                                                                                                                                                                                             |

## 13. 既存の計画と食い違う点（レビューで直す候補）

1. plan §4 R1-5「適合テスト一式を `banto-driver-api` に置き」→ 別 crate（Q1。plan の行は直した）。
2. plan §4 R1-7「bin を分ければ core のテストが対象から外れる」→ dev-dependency も辺になるので、統合テストと core の単体テストの書き換え（R2b-0）が要る（§2.4）。R1 では bin の分離（R1-7a）だけ。
3. plugin-design §12.6「ドライバを変えたときの CI の対象はドライバ・組み立て役・バイナリだけ」→ R1 では成り立たない（core が dev-dependency で残る）。実装本体（`banto-plc` の `slmp/` / `modbus/`）は R2b-2 / R2b-3 で移るまで core に届く（§2.5、Q2）。同節の「`banto-collect` / `banto-broker` ── `banto-hub-core` ← API にだけ依存」も R2b の姿であり R1 ではない。
4. plan §4 R1-4「Modbus / SLMP を trait の実装に載せ替える（I/O は今のまま）」→ フレーム型にもセッション型にもできず（§4.4 の A・B）、R1 は `DriverBase` + `HostBound` の載せ替えになる。「I/O は今のまま」の意図どおりだが、plan の文言は「静的な部分だけ API の形にする」に直す（直した）。
5. plugin-design §6.2 の WIT で `tag-spec` に文字列の `encoding` が無いのは意図どおり（Hub が decode）だが、ネイティブの `TagKind::Str` は R1 のアダプターが `banto-plc` で decode するため `encoding` を持つ（§3.2）。WIT 側に合わせて「エンジンが decode する」形に揃えるのは R2b-3。
6. plan §4 の R1 の完了条件「Modbus / SLMP が適合テストを通る」は、R1 では §7.3 の範囲（C1〜C8・C12、SLMP は C13〜C15 も。`LegacySession` 経由）に限ると明記する。C9〜C11・C16 は R2b-3。
7. plan §4 R1-1「読み取り要求を `banto-plc` から移す」（plugin-design §12.6 の条件 5 の「読み取り要求」）→ **R1 では移さない**。`ReadRequest` / `BatchReadRequest` は `Address` を含み、`Address` を API に出さない（§3.3）ので、要求の型が API に移るのは R2b-2（`TagSpec`）。R1-1 で移すのは `DataType`・`TagValue`・`Value`・`StringEncoding`・`WordOrder` の 5 つ。
8. plugin-design §6.2 の WIT は本書 §9.2 の変更（`string-words`・`specs` / `items`・`rejected`・`prefix-encoding`・`frame-check`・`farewell`・データ型名の綴り）を反映して直した。P0（plugin-design §10）の「WIT 確定」はこの版を起点にする。
9. refactor-plan §8.3（P0-3）は SLMP の分割受信のテストを「v0.2.0 では落ちるので足さない」とした。本ブランチで slmp v0.2.1 に上げたので前提が変わった。本書では SLMP の分割受信を R1-5 の C5（`FaultProxy` の分割送信）で固定するが、**R2b-3 の判定基準は P0-3 が正**なので、P0-3 にも 6 本目「SLMP の応答が分割して届いても 1 つの応答として読む（`banto-plc` の SLMP シミュレータに分割送信のモード）」を足す。**2026-10-19 オーナー決定で足した**（refactor-plan §8.3 の表の 6。§0.1、§7.3 の 4）。
