# banto-driver-api 詳細設計（R1: ドライバ API とレジストリ）

作成日: 2026-10-19
状態: **設計案の下書き（2026-10-19、上位モデルの監査中・オーナーのレビュー待ち）・未着手**。[banto-hub-driver-refactor-plan.md](banto-hub-driver-refactor-plan.md) §4 の R1（R1-1〜R1-7）を実装できる粒度に落としたもの。本書の §12 の未決事項にオーナーの判断が付いた時点で「決定」に進め、R1 の各 PR はこの文書を指示書として使う。本文中の行番号は 2026-10-19 の `main` のもの。見積りは「推定」と明記した。
対象: `banto-driver-api`（公開する型と trait）・適合テスト一式・組み立て役 `banto-drivers`・既存の Modbus TCP / SLMP の載せ替え・ホストのバイナリの package 分離・CI の判定。R2a（取り込み口 `push`）・R2b（I/O の一本化・アドレスの不透明化・SQL の `CHECK` の撤去）・W（WASM 本体）が **API を壊さずに** 載ることを設計の条件にする。
関連: [banto-hub-driver-refactor-plan.md](banto-hub-driver-refactor-plan.md)（段の順番・決定事項）、[banto-hub-driver-plugin-design.md](banto-hub-driver-plugin-design.md)（§2・§3.1・§3.2・§6 の WIT・§12、監査 B2 / M5 / M6）、[banto-hub-external-db-design.md](banto-hub-external-db-design.md) §9.5（`push` はセッション型）、[tag-server-design.md](tag-server-design.md) §3.4 / §3.4.1 / §6（書き込みの安全）、[h9-slmp-structured-error-spec.md](h9-slmp-structured-error-spec.md)（構造化エラー）、[implementation-checklist.md](implementation-checklist.md)（PR の手順）。

---

## 0. 要約（決めたこと）

| #   | 決めたこと                                                                                                                                                                                                                                                                                                                                                           | 根拠の節  |
| --- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------- |
| 1   | `banto-driver-api` は **型と trait だけ**。必須の依存はゼロ（`serde` は feature で任意）。すべての公開 struct / enum に `#[non_exhaustive]`、trait の追加メソッドは既定実装つき、で semver の破壊を避ける                                                                                                                                                            | §2.1, §9  |
| 2   | 適合テスト一式は **別 crate `banto-driver-conformance`**（API crate の feature ではない）。tokio・モックのピアを持つため                                                                                                                                                                                                                                             | §2.2, §7  |
| 3   | trait は WIT と同じ形の **フレーム型**（`FrameDriver` / `FrameConnection`、I/O を持たない）と **セッション型**（`SessionDriver` / `Session`、I/O を自分で持つ）の 2 種。共通の静的な部分（識別・設定の検証・アドレス関数）は `DriverBase`                                                                                                                            | §4, §5    |
| 4   | R1 の Modbus / SLMP は **フレーム型にもセッション型にもしない**。`DriverBase`（識別・設定・アドレス関数）だけを API の形で実装し、I/O は今の `BrokerSession` / `ClientFactory` を**差し込める表**にして組み立て役から渡す（レジストリの `DriverKind::HostBound`、R1 限定）。フレーム型への載せ替えは I/O の一本化（R2b-3）と同時でなければ振る舞いを変えずにできない | §4.4      |
| 5   | R1 の要求の型は今の `banto_plc::Address` のまま。API には不透明な `DriverAddress`（WIT の `parsed`）を **並走で**入れ、R2b-2 で要求の型を切り替える                                                                                                                                                                                                                  | §3.3      |
| 6   | `DriverRegistry` は R1 で作るが、R1 で正にするのは **broker の `DRIVERS` と `is_supported_protocol` だけ**（振る舞い不変で差し替え）。`ALLOWED_PROTOCOLS`・SQL の `CHECK`・`BrokerSimRegistry::resolve`・UI は R2b（R1 は「レジストリ ⊆ 既存の表」のトリップワイヤのテストだけ）                                                                                     | §6        |
| 7   | ホストのバイナリは `apps/banto-hub/host`（package `banto-hub-host`）と `apps/chronogazer/host`（package `chronogazer-host`）へ分ける。**CI の逆依存は dev-dependency も辺にする**ので、core の統合テストのうち実ドライバを要るものも host 側へ移さないと core は対象から外れない（R1-7b）                                                                            | §2.4, §10 |
| 8   | R1 の CI の短縮は **ドライバの薄い crate と組み立て役の変更に限る**。`banto-plc` の中身（SLMP / Modbus の実装）は R2b-3 までホストの core が直接使うので、その変更は今までどおり core に届く                                                                                                                                                                         | §2.5      |

## 1. 前提（コードの現状、2026-10-19 に照合）

### 1.1 今の境界と型

| 層                 | 場所                                                                                                                                                                                                                                                                                                             | 形                                                                                                                                                             |
| ------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 読み取りの trait   | `crates/banto-plc/src/client.rs:56-79` `PlcClient { connect, read_batch(&[ReadRequest]), disconnect }`、`BoxFuture` は同 `:47`                                                                                                                                                                                   | 接続ごと 1 インスタンス、`Err` = 接続致命                                                                                                                      |
| 書き込みの trait   | `crates/banto-plc-write/src/client.rs:46-78` `PlcWriteClient`                                                                                                                                                                                                                                                    | 同上                                                                                                                                                           |
| broker の境界      | `crates/banto-broker/src/session.rs:70-92` `pub(crate) trait BrokerSession { read_batch(&[BatchReadRequest]), write_batch(&[BatchWriteRequest]), disconnect }`、`SessionError(String)` 同 `:102-103`、`Connector` 同 `:126-128`                                                                                  | `pub(crate)`。登録表 `DRIVERS: &[(&str, DriverSpawn)]` は `lib.rs:743-753`、`is_supported_protocol` は `:798-800`、`ensure_connection` の引き当ては `:840-848` |
| 共通の型           | `crates/banto-plc/src/types.rs` `DataType :29-40`、`ReadRequest :99-103`、`TagValue :117-121`、`StringEncoding :179-186`、`StringReadRequest :196-208`、`BatchReadRequest :214-218`、`PlcValue :230-238`、`BatchReadResult :263-267`                                                                             | `ReadRequest` / `BatchReadRequest` は `Copy`                                                                                                                   |
| アドレス           | `crates/banto-plc/src/address.rs:93-130` `Address { ModbusRef{area,offset,bit} \| Slmp{device,number,bit} }`（`Copy`）、`AddressArea` は `crates/banto-plc-address/src/lib.rs:42-53`、`is_writable` 同 `:75-77`                                                                                                  | 2 つだけの enum。外から足せない（tag-server-design §3.4.1）                                                                                                    |
| エラー             | `crates/banto-plc/src/error.rs:20-125` `PlcError`、致命 / 非致命の切り替えは `is_connection_fatal :145-154`（既定は致命）。書き込み側は `crates/banto-plc-write/src/error.rs:24-140` `PlcWriteError`                                                                                                             | 非致命 = `ModbusException`・`SlmpEndCode`・`UnsupportedCombination`・`AddressProtocolMismatch`・`StringSpanUnsupported`                                        |
| SLMP の構造化      | `crates/banto-plc/src/slmp/mod.rs:305-340` `SlmpErrorClass` / `classify_slmp`                                                                                                                                                                                                                                    | h9 の受け入れ済み（文言パースなし）                                                                                                                            |
| 収集の差し込み口   | `crates/banto-collect/src/task.rs:341` `ClientFactory = Arc<dyn Fn(&ClientSpec) -> Box<dyn PlcClient> + Send + Sync>`、`ClientSpec :271-304`、`ClientProtocol :248-251`、`default_client_factory :385-409`。`Collector::start_with_client_factory` は `collector.rs:363-377`、`start_with_sources` は `:379-389` | `ClientSpec` は手で保守する写し（#334 の罠が doc に書いてある）                                                                                                |
| 収集のプロトコル   | `crates/banto-collect/src/config.rs:84-87` `pub enum Protocol { ModbusTcp, Slmp }`、`pub(crate) enum ProtocolConfig { ModbusTcp, Slmp, External } :111-115`、`build_request :1457-1477`（`Protocol` で `Address::parse` / `parse_slmp` を選ぶ）                                                                  | banto-hub が `Protocol` を網羅的に `match` する                                                                                                                |
| 外部ソース         | `crates/banto-collect/src/external.rs:151-153` `pub trait ExternalSource: Send + Sync { fn snapshot(&self) -> ExternalSnapshot }`                                                                                                                                                                                | ChronoGazer の `hub` 接続。R2a の `push` と同種                                                                                                                |
| Hub の相乗り       | `apps/banto-hub/core/src/broker_glue.rs:834-873` `hub_client_factory`（SLMP・Modbus とも `BrokerReadClient`）、`BrokerSimRegistry::resolve :704-752`（`protocol == "modbus-tcp"` 以外は SLMP シミュレータ）                                                                                                      | #337 の 1 接続 = 1 ソケット                                                                                                                                    |
| 文字列で分岐       | `apps/banto-hub/core/src/write_path.rs:1634-1637, 1782-1783`、`read_path.rs:264-265`（`"modbus-tcp"` 以外を `parse_slmp`）、`is_supported_protocol` の呼び出しは `hub.rs:1989`・`read_path.rs:180`・`rest.rs:8142`・`write_path.rs:676`                                                                          | plugin-design §3.2                                                                                                                                             |
| レジストリの固定値 | `crates/banto-tags/src/plc_connection.rs:236` `ALLOWED_PROTOCOLS`、`migrations/0019_plc_connections_allow_hub.sql:61` の `CHECK`、`tag.rs:37` `ALLOWED_DATA_TYPES`、`tag.rs:451` `modbus_read_only_area`、`tag.rs:966`（64bit 型は Modbus のみ）                                                                 | SQL の `CHECK` の撤去は R2b-1                                                                                                                                  |
| ホストのバイナリ   | `apps/banto-hub/core/src/bin/banto-hub.rs`（145 行）・`banto-hub-elev.rs`（124 行）・`banto_hub/win_service.rs`、`apps/chronogazer/core/src/bin/banto-serve.rs`（175 行）・`chronogazer-server.rs`（1,526 行）+ `chronogazer_server/`                                                                            | どちらも core と同じ package。`apps/chronogazer/core/tests/chronogazer_server.rs:145, 917` が `env!("CARGO_BIN_EXE_chronogazer-server")` を使う                |

プロトコル名の文字列（`"modbus-tcp"` / `"slmp"`）で分岐・固定する箇所は、テストを除いて **51 ファイル**（2026-10-19 の grep。テストを含めると 92）。P0-2 の棚卸しはこの一覧を元に行う。

### 1.2 依存グラフ（今。`cargo metadata --no-deps` で確認）

```
banto-plc-address ─┬─ banto-tags ─────────┬─ banto-collect ─┬─ chronogazer-core ── chronogazer / chronogazer-elev
                   └─ banto-plc ─┬────────┤                 └─ banto-hub-core ── banto-hub-shell
                                 │        └─ banto-broker ──── banto-hub-core
                                 └─ banto-plc-write ─ banto-broker
banto-hub-core は banto-plc・banto-plc-write・banto-broker・banto-collect を直接も使う
banto-hub-sink は banto-hub-core（と broker・collect）を dev-dependency で使う（Hub をプロセス内に建てる）
```

- `banto-plc` に SLMP / Modbus の実装と共通の型が同居しているので、ドライバの中身だけ変えても `banto-hub-core`（統合テストのバイナリ約 20 本）と `chronogazer-core` が作り直される（plugin-design §12.6）。
- **CI の判定（`.github/scripts/ci-changes.mjs:214-240` `graphFromMetadata`）は normal・dev・build の依存をすべて辺にする**。dev-dependency で core がドライバを使う限り、ドライバの変更は core に届く。R1-7 の設計はこれを前提にする（§2.4）。

### 1.3 公開可能性

- ワークスペースは `publish = false`（ルート `Cargo.toml` の `[workspace.package]`）。公開する crate は自分の `Cargo.toml` で `publish = true` と独自の `version` を持つ。
- `banto-plc` / `banto-plc-write` / `banto-broker` は git 依存 `slmp`（ルート `Cargo.toml` の `slmp = { git = "https://github.com/tyaro/slmp", tag = "v0.2.0" }`）を持ち、`banto-tags` / `banto-collect` は `banto-core` / `banto-storage`（git）を持つ。**git 依存と path 依存を持つ crate は crates.io に公開できない**ので、公開する crate はこれらに依存してはならない。`banto-plc-address`（依存ゼロ）だけは公開できるが、R1 では API crate に取り込まず再輸出もしない（§3.3）。
- cargo-deny の `[sources]`（`deny.toml:102-118`、`required-git-spec = "tag"`）は変えない。

## 2. crate / package の構成（R1 後）

### 2.1 `banto-driver-api`（`crates/banto-driver-api`、公開する）

```toml
[package]
name = "banto-driver-api"
version = "0.1.0-alpha.1"      # ワークスペースの版と切り離す（§9）
edition = "2021"
rust-version = "1.98"          # 検証している toolchain（rust-toolchain.toml）に合わせる。上げるときは minor
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
- 検証: `cargo publish --dry-run -p banto-driver-api`（path / git 依存が無いことの機械的な確認。R1-1 の完了条件）。

### 2.2 `banto-driver-conformance`（`crates/banto-driver-conformance`、公開する）

**判断: API crate の feature ではなく別 crate にする。** 理由:

1. 適合テストは tokio のランタイム・モックのピア（TCP のリスナー・分割送信・遅延応答）・`assert` の補助を持つ。これを feature で API crate に入れると、API crate の `Cargo.toml` に tokio が（optional でも）並び、「型と trait だけ・依存ゼロ」（plugin-design §12.6 の条件 1）が崩れる。
2. 適合テストは API より頻繁に育つ（新しい振る舞いの固定を足す）。同じ crate だと適合テストの追加のたびに API の版が動く。別 crate なら `banto-driver-conformance` の版だけ上げればよい。
3. 第三者は `[dev-dependencies]` に conformance だけを足す（API は normal dependency）。feature 方式でも書けるが、dev-dependency の feature が normal の feature と統一されて（feature unification）本番ビルドに tokio が混ざる事故がある。

依存: `banto-driver-api`（caret）・`tokio`（`rt`, `time`, `net`, `io-util`, `macros`）。plan.md §4 R1-5 の「`banto-driver-api` に置く」はこの判断で読み替える（§12 Q1）。

### 2.3 ドライバの crate と組み立て役

| crate                                                 | 公開 | 持つもの                                                                                                                                                                                                                                                                                                                                         | 依存                                                                                |
| ----------------------------------------------------- | ---- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ----------------------------------------------------------------------------------- |
| `crates/banto-driver-modbus`（`banto-driver-modbus`） | ×    | Modbus TCP の **R1 の薄いアダプター**: `DriverBase` の実装（識別・設定の検証・`parse_address` は `banto_plc::Address::parse` を包む）、`banto-broker` の `modbus_driver.rs`（448 行）を**移したもの**（`ModbusSession` + `connector`）、`banto-collect` の `default_client_factory` の Modbus の腕。シミュレータは `banto-plc` の feature のまま | `banto-driver-api`、`banto-plc`、`banto-plc-write`、`banto-broker`、`banto-collect` |
| `crates/banto-driver-slmp`（`banto-driver-slmp`）     | ×    | 同じく SLMP: `slmp_driver.rs`（100 行）を移したもの、`parse_address` は `Address::parse_slmp`、`cpu`（#605）を設定に持つ                                                                                                                                                                                                                         | 同上（`slmp` は `banto-plc` 経由）                                                  |
| `crates/banto-drivers`（`banto-drivers`）             | ×    | 組み立て役。`builtin() -> Builtin { registry: DriverRegistry, broker: banto_broker::DriverTable, client_factory: banto_collect::ClientFactory }`。トリップワイヤのテスト（§6.3）。R2b 以降ここが「公式ドライバの一覧」になり、API の公開後は別リポジトリへ出す（plan §7 Q2）                                                                     | 上の 2 つ + `banto-driver-api` + `banto-broker` + `banto-collect`                   |

- `banto-plc` / `banto-plc-write` / `banto-broker` / `banto-collect` は **R1 では残る**（型・計画・デコード・I/O の実装はそのまま）。変わるのは (a) 共通の型が `banto-driver-api` へ移って再輸出になる、(b) broker の登録表と collect の既定の factory の「中身」がドライバ crate へ移って、表そのものは注入されるようになる、の 2 点。
- `banto-broker` の `BrokerSession` / `SessionError` / `Connector` は **`pub` にする**（ドライバ crate が実装するため。tag-server-design §3.4.1 の「別リポジトリでは作り切れない部分」の 1 つが解ける）。

### 2.4 ホストのバイナリの package 分離（R1-7）

| 新 package                                    | 場所                                    | 移すもの                                                                                                                                                                                                                                                                                    | 依存                                                                                    |
| --------------------------------------------- | --------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------- |
| `banto-hub-host`（`apps/banto-hub/host`）     | bin `banto-hub`・`banto-hub-elev`       | `apps/banto-hub/core/src/bin/` 一式（`banto-hub.rs`・`banto-hub-elev.rs`・`banto_hub/win_service.rs`・`win_service_tests.rs`）。`embed-ui` feature は host 側に置き `banto-hub-core/embed-ui` へ転送する                                                                                    | `banto-hub-core`、`banto-drivers`、`banto-service-control` 等（bin が今使っているもの） |
| `chronogazer-host`（`apps/chronogazer/host`） | bin `banto-serve`・`chronogazer-server` | `apps/chronogazer/core/src/bin/` 一式。`tests/chronogazer_server.rs` は `CARGO_BIN_EXE_chronogazer-server` を使うので **host 側へ移す**（Cargo はその package の bin にしか `CARGO_BIN_EXE_*` を出さない）。`examples/dev_plc.rs`・`hub_p6_compare.rs` は core に残す（ドライバを要らない） | `chronogazer-core`、`banto-drivers`                                                     |
| `banto-hub-shell`・`chronogazer`（Tauri）     | 既存                                    | 変更なし。`banto-drivers` を dependency に足し、`HubRuntime::start` / `EngineRuntime` に `builtin()` を渡す                                                                                                                                                                                 |                                                                                         |

**R1-7 を 2 段に分ける**（plan §4 の R1-7 は「bin を分ければ core が対象から外れる」と読めるが、§1.2 のとおり dev-dependency も辺になるので足りない）:

- **R1-7a（bin の分離）**: 上の表。core は `banto-drivers` を **dev-dependency で持ち続ける**（統合テストが実ドライバで動くため）。この時点では CI の対象は変わらない（ドライバの変更は dev-dependency 経由で core に届く）。
- **R1-7b（テストの移動）**: `apps/banto-hub/core/tests/` のうち `HubRuntime` / `CollectorManager` を実ドライバで動かすもの（`integration.rs`・`write.rs`・`t9_simulation*.rs`・`t20_*.rs`・`t8_bit_access.rs`・`t12_connection_test.rs`・`t7_partial_reconfig.rs`・`t15_*.rs`・`soak.rs`・`stream.rs`・`grpc.rs`・`mcp.rs`・`mqtt.rs`・`computed.rs`・`live_reconfig.rs`・`t11_batch_tags.rs`・`rbac.rs`・`client_bootstrap.rs`・`db_source.rs`・`expression_check.rs`。実際に `banto_drivers::builtin()` を要るものだけ。推定で 20 本の大半）を `apps/banto-hub/host/tests/` へ移し、core の dev-dependency から `banto-drivers` を外す。`apps/chronogazer/core/tests/*` も同様（`collect_roundtrip.rs` 等は `default_client_factory` を使うので host へ）。`tests/common/` と `test_support.rs`（`pub mod test_support`、`lib.rs:229-234` 付近）は core の公開 API として残し host のテストから使う。`.config/nextest.toml:68` の `package(banto-hub-core) & binary(soak)` は `package(banto-hub-host)` に直す。
  - **R1-7b が CI の短縮の本体**。core に残る統合テストは「ドライバの無いレジストリ（§7.4 の偽ドライバ）で動くもの」だけになる。R1 の時点で core のテストを偽ドライバで書き直すことは**しない**（振る舞いを変えない段で大きなテストの書き換えは避ける）。
  - 推定: 移動だけなら差分は大きいが機械的（`use` のパスと Cargo.toml）。移した後に `cargo test -p banto-hub-host` の件数が移動前の core の該当バイナリの合計と一致することを完了条件にする。

ビルドの入口で package 名を書いている箇所（R1-7a で全部直す）: `.github/workflows/ci.yml:604, 611, 631, 693`、`.github/workflows/opcua-interop.yml:49`、`scripts/build-release.ps1:68-69`、`apps/banto-hub/installer/src/main.rs:48, 150`、`package.json:20`（`dev_plc` は core のままなので変えない）、`AGENTS.md:57, 79, 91`、`apps/banto-hub/README.md:22, 27, 79`、`apps/banto-hub/vite.config.ts:37`、`apps/banto-hub/core/src/assets.rs:14`、`implementation-checklist.md` §2-7（`cargo build -p <core> --bin <bin>`）。

### 2.5 R1 後の依存グラフと CI の見込み

```
banto-driver-api（公開）◀── banto-plc ◀── banto-plc-write ◀── banto-broker ◀── banto-hub-core
        ▲                     ▲                                   ▲
        │                     └── banto-collect ◀── chronogazer-core / banto-hub-core
        │
        ├── banto-driver-modbus / banto-driver-slmp（＝ 薄いアダプター。banto-plc 等に依存）
        │         ▲
        └── banto-drivers ◀── banto-hub-host / chronogazer-host / banto-hub-shell / chronogazer / banto-hub-sink(dev)
banto-driver-conformance（公開）◀── banto-driver-modbus / -slmp の dev-dependency
```

| 変更したもの                                             | Rust の対象（R1-7b の後）                                                                                                                                                                                                                                                              | E2E                |
| -------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------ |
| `crates/banto-driver-modbus` / `-slmp` / `banto-drivers` | そのドライバ・`banto-drivers`・`banto-hub-host`・`chronogazer-host`・`banto-hub-shell`・`chronogazer`・`banto-hub-sink`。**`banto-hub-core`・`chronogazer-core` は外れる**                                                                                                             | Hub と ChronoGazer |
| `crates/banto-plc`（SLMP / Modbus の実装本体）           | 今までどおり 10 パッケージ超（core を含む）。**R1 では縮まない**（core が `banto_plc::SlmpCpu`・`address`・`planning`・`PlcValue` を直接使う: `rest.rs:3608, 4527, 7736`、`read_path.rs:251`、`write_path.rs:252, 376`）。縮むのは R2b-2 / R2b-3 で実装本体がドライバ crate へ移った後 | 両方               |
| `crates/banto-driver-api`                                | `banto-plc` が依存するので実質 `--workspace`（`RDEPS_FULL_RATIO` 0.8 に届く見込み）。API を動かさない方針（§9）と整合                                                                                                                                                                  | 両方               |

plugin-design §12.6 の「ドライバを変えたときの CI の対象はドライバ・組み立て役・バイナリだけ」は、**R1 ではアダプター crate の変更にだけ**当てはまる。実装本体の分までそうなるのは R2b の後（§12 Q2 で確認）。

## 3. 型（`banto-driver-api::types`）

### 3.1 `banto-plc` から移す型（R1-1。再輸出で呼び出し側は無変更）

| API の型                      | 元                             | 変更                                                                                                                                                                                                                                                                                                                                                                            |
| ----------------------------- | ------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `DataType`                    | `banto-plc/src/types.rs:29-40` | そのまま移す（`Bit`・`I16`〜`F64` の 9 つ、`parse`・`Display`）。`pub(crate) fn register_span`（`:69-76`）は API では `pub fn word_count(self) -> u16`（16 bit 語の数）にし、`banto-plc` 側の `register_span` はそれを呼ぶ薄い関数に残す。**`String` 変種は足さない**（`DataType::parse("string")` が `Some` になると `banto-collect/src/config.rs:1265` の除外の判定が変わる） |
| `TagValue`                    | `:117-121`                     | そのまま（`Copy` の数値 / bit）。R2b で `Value` に畳む候補                                                                                                                                                                                                                                                                                                                      |
| `PlcValue` → API では `Value` | `:230-238`                     | `Value { Bit(bool), F64(f64), Str(String) }`。`banto-plc` は `pub use banto_driver_api::Value as PlcValue` で名前を保つ                                                                                                                                                                                                                                                         |
| `StringEncoding`              | `:179-186`                     | そのまま（`Utf8` / `ShiftJis`）                                                                                                                                                                                                                                                                                                                                                 |
| `WordOrder`                   | `banto-plc/src/decode.rs:17`   | enum だけ移す（`HighLow` / `LowHigh`、`Default = HighLow`）。デコードの実装は移さない。接続設定の共通キー `word_order` の語彙になる（§3.5）                                                                                                                                                                                                                                     |

`banto-plc` の `lib.rs:87-90` の `pub use types::{...}` はそのまま残し、中身を API からの再輸出に替える。`banto-plc-write/src/lib.rs:97` の再輸出もそのまま。これで `banto_plc::DataType` 等を使う全呼び出し側（`write_path.rs:252, 450`、`broker_glue.rs:169, 173` 等）は 1 行も変わらない。

### 3.2 API だけにある新しい型（WIT `types` の写し）

```rust
/// WIT: variant value { bool, int(s64), uint(u64), float(f64), bytes(list<u8>) }
/// フレーム型ドライバの decode が返す生の値。Hub（エンジン）が Value へ写す
/// （整数は f64 へ広げ、bytes は TagKind::Str の encoding で文字列へ）。
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub enum RawValue { Bool(bool), Int(i64), Uint(u64), Float(f64), Bytes(Vec<u8>) }

/// 読み書きの単位の「種類」。banto-plc の slmp::planning::ReadKind（:99-108）と同じ割り方。
/// WIT の tag-spec.data-type / string-words に対応する（§9.2 の表）。
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TagKind {
    Numeric(DataType),
    /// 文字列。words = 16 bit 語の数（banto-tags の string_length、1..=128）。
    /// encoding は Hub 側の情報で WIT には渡さない（WIT ではプラグインが bytes を返し Hub が decode する）。
    /// R1 のアダプターは banto-plc の decode を使うので、ここで運ぶ。
    Str { words: u16, encoding: StringEncoding },
}

/// WIT: record tag-spec { address, data-type, string-words }
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub struct TagSpec { pub address: DriverAddress, pub kind: TagKind }

/// WIT: record write-item { spec, value }
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub struct WriteItem { pub spec: TagSpec, pub value: Value }
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
    /// 正規化した文字列。parse(canonical) == 自分自身（冪等）。DB にはこれを保存する（R2b-2）。
    pub canonical: String,
    /// 領域の名前（Modbus: "coil" 等 = AddressArea の Display と同じ綴り。SLMP: デバイス名 "D" 等）
    pub area: String,
    /// プロトコルの事実としての書き込み可否（D5。書き込み許可の判断には使わない）
    pub writable_area: bool,
    /// ワード内ビット（SLMP は .0〜.F の 16 進、Modbus は .0〜.15 の 10 進。T20-④）
    pub bit: Option<u8>,
    /// このアドレスが占める 16 bit 語の数（bit デバイスは 1）
    pub word_count: u32,
    /// §8: マルチドロップの局番。R1 の Modbus / SLMP は None（局番は接続設定の unit_id）
    pub station: Option<u32>,
}
```

- **canonical の規則（R1 で固定し、適合テストで確かめる）**: Modbus = `Address::parse` が受けた文字列をトリムしたもの（5 桁 / 6 桁 + 任意の `.N`、`.N` は先頭ゼロなしの 10 進）。SLMP = デバイス記号を大文字にし、番号は `SlmpDevice` の基数（10 進 / 16 進）どおりで先頭ゼロなし、`.N` は大文字の 16 進。どちらも「`parse` → `Display` の往復で同じ `Address` になる」ことが R2b-2 で移行の照合に使える条件。
- **R2b-2 でやること**（本書の範囲外だが、R1 の形がそれを邪魔しない根拠）: `TagSpec.address: DriverAddress` を要求の型にし、各ドライバが `canonical` を自分の内部表現へ戻す（Modbus / SLMP は `Address::parse` を再実行。接続ごとに `HashMap<String, Address>` でキャッシュすれば毎ポーリングの再解析は無い）。`protocol == "modbus-tcp"` の分岐 4 か所（§1.1）は `registry.get(id).parse_address` に置き換わる。

### 3.4 エラーの分類（h9 と `is_connection_fatal` を型にする）

```rust
/// WIT: variant item-error { device(device-error), unsupported(string) } ― 非致命（そのタグだけ Bad）
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemError {
    /// 機器が完全な応答で断った。code は機器の生の値（SLMP の終了コード、Modbus の例外コード）
    Device(DeviceError),
    /// 設定の不整合（データ型と領域の組み合わせ、文字列長の範囲外、別プロトコルの表記）。ワイヤに出る前に決まる
    Unsupported(String),
}
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceError { pub code: u32, pub message: String }

/// WIT: variant fatal-error { framing, protocol, invalid-config } ― 接続致命。
/// Io / Timeout は WIT には無い（WASM では Hub 側で起きる）。セッション型だけが返す。
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

`banto_plc::PlcError` / `PlcWriteError` からの写し（R1-4 で `banto-driver-modbus` / `-slmp` に `fn classify(PlcError) -> Result<ItemError, FatalError>` として置き、表のとおりになることを単体テストで固定する。本番経路は R1 では使わない）:

| 元（`error.rs`）                                                                                         | 行き先                                                                              |
| -------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------- |
| `ModbusException { function, code, message }`                                                            | `ItemError::Device { code: code as u32, message: "FC=0x{function:02x} {message}" }` |
| `SlmpEndCode { code, message }`                                                                          | `ItemError::Device { code: code as u32, message }`                                  |
| `UnsupportedCombination` / `AddressProtocolMismatch` / `StringSpanUnsupported`                           | `ItemError::Unsupported(err.to_string())`                                           |
| `ConnectTimeout` / `Connection` / `NotConnected`                                                         | `FatalError::Io(..)`                                                                |
| `ResponseTimeout`                                                                                        | `FatalError::Timeout`                                                               |
| `Protocol(..)`                                                                                           | `FatalError::Framing(..)`（ワイヤの構造の壊れ）                                     |
| `InvalidAddress(..)`（今は致命に分類されている、`is_connection_fatal :145-154`）                         | `FatalError::InvalidConfig(..)`（致命のまま。分類を変えない）                       |
| `PlcWriteError::ValueTypeMismatch` / `ValueOutOfRange` / `ModbusReadOnlyArea` / `UnsupportedRequestKind` | `ItemError::Unsupported(..)`                                                        |

**既定は致命**（WIT §6.1 と同じ）。`ItemError` に写せない variant が増えたら `FatalError` へ倒す。

### 3.5 接続設定

```rust
/// WIT: record setting { key, value }。順序つき（UI と監査の表示順）、重複キーなし
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Settings { pub items: Vec<Setting> }
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Setting { pub key: String, pub value: String }
impl Settings { pub fn get(&self, key: &str) -> Option<&str>; pub fn insert(&mut self, key: &str, value: impl Into<String>); }

/// 共通のキー（ホストが固定列から入れる。R2b-1 で JSON へ移っても同じキー）
pub mod keys {
    pub const HOST: &str = "host";
    pub const PORT: &str = "port";
    pub const UNIT_ID: &str = "unit_id";           // Modbus。SLMP では無視
    pub const WORD_ORDER: &str = "word_order";     // "high_low" | "low_high"
    pub const CONNECT_TIMEOUT_MS: &str = "connect_timeout_ms";
    pub const RESPONSE_TIMEOUT_MS: &str = "response_timeout_ms";
    pub const CPU_TYPE: &str = "cpu_type";         // SLMP の #605（"R" | "Q"）
    pub const TRANSPORT: &str = "transport";       // §8。R1 は "tcp" 固定
    pub const SERIAL_PORT: &str = "serial_port";   // §8（予約。R2b-3）
}

/// WIT: record field-error { key, message }
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldError { pub key: String, pub message: String }
```

R1 では `banto_tags::PlcConnection` の固定列（`host`・`port`・`unit_id`・`word_order`・`cpu_type`）から `Settings` を組む純関数 `banto_drivers::settings_from_connection(&PlcConnection) -> Settings` を置き、`DriverBase::validate_config` と R2b-1 の JSON 化の両方がこれを通る。ワード順は `word_order` キーで**接続設定として**ドライバへ渡す（plugin-design の監査 B2: 読み取り単位には持たせない。`ClientSpec.word_order` と同じ扱い、`task.rs:281-297`）。

## 4. フレーム型の trait（WIT §6.2 の写し）

### 4.1 共通の静的な部分

```rust
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
    pub api_version: u32,           // 1。WIT の hubApi
}
#[non_exhaustive] #[derive(Debug, Clone, Copy, PartialEq, Eq)] pub struct Capabilities { pub read: bool, pub write: bool }
#[non_exhaustive] #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)] pub enum Transport { Tcp, Serial, SerialOverTcp }

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DriverId(String);
impl DriverId { pub fn new(s: &str) -> Result<Self, InvalidDriverId>; pub fn as_str(&self) -> &str; }

/// 状態を持たない部分。REST / MCP / CSV / 設定パッケージ / UI から並行に呼ばれる（§6.5）
pub trait DriverBase: Send + Sync + 'static {
    fn descriptor(&self) -> &DriverDescriptor;
    /// WIT config.validate
    fn validate_config(&self, settings: &Settings) -> Vec<FieldError>;
    /// WIT config.secret-keys。既定は無し
    fn secret_keys(&self) -> Vec<String> { Vec::new() }
    /// WIT address.parse（text, data-type）。kind は語数を決めるために要る
    fn parse_address(&self, text: &str, kind: &TagKind) -> Result<DriverAddress, AddressError>;
    /// WIT address.offset（連続登録・オフセットコピー）
    fn offset_address(&self, canonical: &str, kind: &TagKind, n: i64) -> Result<String, AddressError>;
    /// WIT address.help（日本語固定、plugin-design §6.2 補足）
    fn address_help(&self) -> String;
}
#[non_exhaustive] #[derive(Debug, Clone, PartialEq, Eq)] pub struct AddressError(pub String);
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
    /// 長さ接頭辞: 受信した offset..offset+width の整数（big_endian）+ adjust が総長
    LengthPrefixed(PrefixSpec),
    /// 終端文字列（例: ホストリンクの CR）
    Terminator(Vec<u8>),
    /// 受信のたびに is_complete を呼ぶ。max_len を超えたら致命
    DriverDecides,
}
#[non_exhaustive] #[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrefixSpec { pub offset: u32, pub width: u8, pub big_endian: bool, pub adjust: i32 }

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step { pub send: Vec<u8>, pub expect: FrameRule, pub max_len: u32 }
impl Step { pub fn new(send: Vec<u8>, expect: FrameRule, max_len: u32) -> Self; }

/// WIT read.planned / write.planned-write（covers = specs / items の添字）
#[non_exhaustive] #[derive(Debug, Clone, PartialEq, Eq)] pub struct Planned { pub step: Step, pub covers: Vec<u32> }

pub trait FrameDriver: DriverBase {
    /// WIT connection.create。接続ごとに 1 つ（§6.5）
    fn create_connection(&self, settings: &Settings) -> Result<Box<dyn FrameConnection>, FatalError>;
}

/// WIT resource connection + read + write。1 つの broker タスクが専有する（Send、Sync 不要）
pub trait FrameConnection: Send + 'static {
    /// WIT handshake-next。最初は None、以降は直前の step の応答。None を返したら完了。
    /// 拒否応答なら Err（接続成功とみなさない）
    fn handshake_next(&mut self, prev_response: Option<&[u8]>) -> Result<Option<Step>, FatalError>;
    /// WIT is-complete（DriverDecides のときだけ呼ばれる）
    fn is_complete(&self, buf: &[u8]) -> Result<Option<u32>, FatalError>;
    /// WIT read.plan（borrow<connection> = &self）
    fn plan_read(&self, specs: &[TagSpec]) -> Result<Vec<Planned>, FatalError>;
    /// WIT read.decode。戻りは planned.covers と同じ長さ・順序
    fn decode_read(&self, planned: &Planned, response: &[u8]) -> Result<Vec<Result<RawValue, ItemError>>, FatalError>;
    /// WIT write.encode。capabilities.write == false なら呼ばれない（既定は Err）
    fn encode_write(&self, _items: &[WriteItem]) -> Result<Vec<Planned>, FatalError> {
        Err(FatalError::Protocol("このドライバは書き込みに対応していません".into()))
    }
    /// WIT write.check
    fn check_write(&self, _planned: &Planned, _response: &[u8]) -> Result<Vec<Result<(), ItemError>>, FatalError> {
        Err(FatalError::Protocol("このドライバは書き込みに対応していません".into()))
    }
}
```

- 状態を変えるのは `handshake_next` だけ（FINS のノードアドレス、EtherNet/IP のセッションハンドル）。`plan` / `decode` / `encode` / `check` は `&self`（WIT の `borrow<connection>` と同じ）。これで「decode の途中で plan の前提が変わる」事故を型で塞ぐ。
- **要求 - 応答の手順（エンジンが R2b-3 で実装する契約。R1 では適合テストのハーネスが同じ手順を持つ）**: (1) `plan_read` で `Vec<Planned>` を得る → (2) 各 `Planned` について `step.send` を送り、`expect` に従って受信（`None` なら受信しない。`DriverDecides` なら受信のたびに `is_complete`）→ (3) `max_len` 超過・`response_timeout` 超過は致命で、**受信バッファを捨てて接続を切る**（遅れて届いた応答を次の step に流用しない。`banto-plc/src/error.rs:39-47` の `ResponseTimeout` の理由と同じ）→ (4) `decode_read` → (5) `covers` の添字で結果を `specs` の順に並べる。`covers` に出てこない添字は `ItemError::Unsupported("計画に含まれない")`。
- **ワード順**は `Settings` の `word_order` から `create_connection` が読む（監査 B2）。文字列は `RawValue::Bytes` で返し、エンジンが `TagKind::Str.encoding` で decode する（`banto-plc/src/decode.rs` の `decode_string_value` と同じ規則: 語の下位バイトが先、最初の NUL で切る）。

### 4.3 R1 での使われ方

**R1 では `FrameDriver` / `FrameConnection` を実装するドライバは無い**（Modbus / SLMP は §4.4）。R1-2 で trait と、適合テストのための**参照実装**（`banto-driver-conformance` の `examples/echo_driver.rs`: 固定長フレームで echo する最小のフレーム型ドライバ。plugin-design §10 P1 の「組み込みにないアドレス文法を持つテスト用プラグイン」の前身）を入れ、trait が実装可能であることとハーネスの手順を固める。

### 4.4 R1 のアダプター: Modbus / SLMP をどう載せるか

| 選択肢                                                                                             | I/O に触るか                                                                                                                                                          | 振る舞いの危険                                                                                                  | 判定         |
| -------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------- | ------------ |
| A. 真のフレーム型にする（`plan` / `decode` を `banto_plc::planning` から組み直し、I/O はエンジン） | **触る**。エンジン（R2b-3）が無いと動かない。SLMP は `tyaro/slmp` の `SLMPClient` がフレームと I/O を一体で持つので、フレームだけ取り出すにはライブラリ側の改造が要る | 分割受信・遅延応答・トランザクション ID の一本化（`modbus_driver.rs:46-79`）を落とす危険。P0-3 の固定テストが先 | R2b-3 でやる |
| B. セッション型で包む（`BrokerSession` → `api::Session` のアダプター）                             | broker の内部（`Job` が運ぶ `BatchReadRequest`、`ConnState`）を `api::Session` と `TagSpec` に替える必要がある。canonical → `Address` の再解析が毎回入る              | broker の job ループ・再接続の状態機械に手が入る。write_path / read_path の要求の型も変わる                     | R2b-2 と同時 |
| **C. `HostBound`（本書の採用）**                                                                   | **触らない**。`slmp_driver.rs` / `modbus_driver.rs` をファイルごとドライバ crate へ移し、`DRIVERS` の表と `default_client_factory` の腕を組み立て役から注入する       | 無し（移動と注入だけ。既存テストがそのまま判定基準）                                                            | **R1**       |

C の形:

```rust
// banto-driver-api::registry
#[non_exhaustive]
pub enum DriverKind {
    Frame(Arc<dyn FrameDriver>),
    Session(Arc<dyn SessionDriver>),
    /// R1 限定。識別・設定・アドレス関数は API の形で持ち、I/O はホストが別の表
    /// （banto_broker::DriverTable / banto_collect::ClientFactory）で結ぶ組み込みドライバ。
    /// R2b-3 で Frame に載せ替えて消す（§9 の 0.x の間の破壊として扱う）
    HostBound(Arc<dyn DriverBase>),
}
```

```rust
// banto-broker（R1-3）: 固定の const DRIVERS を注入式に
pub type DriverSpawn = fn(&PlcConnection, u16, BackoffConfig, watch::Receiver<bool>) -> (BrokerHandle, TaskEntry); // 今の lib.rs:743-744 を pub に
#[derive(Clone)]
pub struct DriverTable { entries: Arc<[(DriverId, DriverSpawn)]> }
impl DriverTable {
    pub fn new(entries: Vec<(DriverId, DriverSpawn)>) -> Result<Self, DuplicateDriverId>;
    pub fn is_supported(&self, protocol: &str) -> bool;   // 今の is_supported_protocol(:798-800) の中身
    pub fn spawn_for(&self, protocol: &str) -> Option<DriverSpawn>;
}
impl BrokerSupervisor { pub fn spawn(..., drivers: DriverTable) -> Self; }   // SessionDirectory::ensure_connection(:840-848) は self.drivers.spawn_for を引く
pub fn is_supported_protocol(protocol: &str) -> bool;  // R1 では残す: 組み込み 2 つの固定表を見る互換関数。#[deprecated]。R1-4 で呼び出し側 4 か所を DriverTable 経由に直したら消す
```

```rust
// banto-drivers（R1-3 / R1-4）
pub struct Builtin {
    pub registry: DriverRegistry,                 // modbus-tcp / slmp を HostBound で登録
    pub broker: banto_broker::DriverTable,        // [("slmp", banto_driver_slmp::spawn), ("modbus-tcp", banto_driver_modbus::spawn)]
    pub client_factory: banto_collect::ClientFactory, // 今の default_client_factory と同じ中身をドライバ crate の腕で組む
}
pub fn builtin() -> Builtin;
```

- `banto_collect::default_client_factory`（`task.rs:385-409`）は R1 では**残す**（ChronoGazer の `collect.rs:1919, 2573` と banto-collect 自身のテストが使う。中身は `banto-plc` の直結クライアントの構築で、`banto-collect` は `banto-plc` に依存したままなので動く）。`banto_drivers::builtin().client_factory` は同じものを返し、ホストはそちらを渡す。`default_client_factory` の撤去は R2b-3。
- Hub の `hub_client_factory`（`broker_glue.rs:834-873`）は `ClientProtocol` の `match` を **`spec.protocol` に関係なく `broker_handles.get(&spec.connection_key)` を見る 1 本**にできる（どちらの腕も同じ中身。#337 の後の形）。これは振る舞い不変の整理で R1-4 に含める。`ClientProtocol` 自体の撤去は R2b。

## 5. セッション型の trait（I/O を自分で持つ）

```rust
/// 接続時にホストが渡す文脈。tokio は API に入れないので、ハンドルは渡さない
/// （実装はホストと同じ tokio ランタイムの上で spawn されるので、tokio::spawn をそのまま使える）
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct ConnectContext { pub connection_key: String, pub connect_timeout: Duration, pub response_timeout: Duration }

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
- **`push`（external-db-design §9.5）**: `SessionDriver` の実装。`connect` は WebSocket / REST の送信口から値を受ける受信側（`watch` か `RwLock<HashMap>`）を握った `Session` を返し、`read_batch` は「保持している最新値」を返す（external.rs のモジュール doc「sample and hold」と同じ）。`ExternalSource`（`external.rs:151-153`）は R1 では**そのまま**残す。R2a で `push` を `SessionDriver` にするとき、ChronoGazer の `hub` 接続を `ExternalSource` のまま残すか `SessionDriver` に寄せるかを決める（§12 Q5）。
- OPC UA クライアントのドライバ（tag-server-design §3.4.1 の決まり 4）はこの trait の実装になる。組み込みのみ（plugin-design §6.1）。
- レジストリでの共存: `DriverKind::Session`。エンジン（R2b-3）は `Frame` なら「トランスポート + フレーム規則」で、`Session` なら「`connect` して `Session` を直接」で、どちらも同じ broker タスク（`run_broker_task`）に載せる。R1 では `Session` の実装は無く、適合テストの偽ドライバ（§7.4）だけがある。

## 6. `DriverRegistry`

### 6.1 形（`banto-driver-api::registry`、R1-3）

```rust
#[non_exhaustive]
pub struct DriverEntry { pub descriptor: DriverDescriptor, pub kind: DriverKind }
impl DriverEntry { pub fn base(&self) -> &dyn DriverBase; }   // kind のどれでも静的な部分は取れる

#[derive(Default)]
pub struct DriverRegistry { entries: BTreeMap<DriverId, DriverEntry> }
impl DriverRegistry {
    pub fn register(&mut self, entry: DriverEntry) -> Result<(), RegistryError>;  // 同じ id は拒否（plugin-design §4.2「組み込みの ID は上書きできない」の前身）
    pub fn get(&self, id: &str) -> Option<&DriverEntry>;
    pub fn contains(&self, id: &str) -> bool;
    pub fn ids(&self) -> impl Iterator<Item = &DriverId>;
    pub fn len(&self) -> usize; pub fn is_empty(&self) -> bool;
}
#[non_exhaustive] #[derive(Debug, Clone, PartialEq, Eq)] pub enum RegistryError { DuplicateId(DriverId) }
```

- 同一性は `(id, version)`。R1 では 1 つの id に 1 つの実装（「接続ごとに実装を選べる」R2b-4 は `register_alternate(id, variant_name, entry)` を**後から足す**。`#[non_exhaustive]` なので API の破壊にならない）。
- `Arc<DriverRegistry>` をホストが作り、`HubConfig`（`runtime.rs:204`）/ `CollectorManager::new`（`hub.rs:1162-1170`）/ `EngineConfig`（ChronoGazer）に渡す。R1 では読む側は `is_supported`（§6.2）だけ。

### 6.2 R1 で「レジストリが正」になるもの・ならないもの

| 今の正                                                                      | R1                                                                                                                                                                     | R2b                                                              |
| --------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------- |
| broker の `DRIVERS`（`lib.rs:750-753`）                                     | **`DriverTable` を注入**（§4.4）。中身は組み立て役が `builtin()` で同じ 2 つを渡す。振る舞い不変                                                                       | `FrameDriver` / `SessionDriver` をエンジンが直接使い、表は消える |
| `is_supported_protocol`（`:798-800`、呼び出し 4 か所）                      | **`HubSessions::is_broker_managed(&str)`（`DriverTable::is_supported`）に差し替え**。`banto_broker::is_supported_protocol` は `#[deprecated]` で残し R1-4 の最後に消す | `registry.contains(id)`                                          |
| `banto-collect` の `ClientFactory`（既に注入式）                            | 組み立て役の `client_factory` を渡す。`default_client_factory` は残す                                                                                                  | `default_client_factory`・`ClientProtocol`・`Protocol` の撤去    |
| `ALLOWED_PROTOCOLS`（`plc_connection.rs:236`）と SQL の `CHECK`             | **変えない**。トリップワイヤ（§6.3）                                                                                                                                   | R2b-1 で撤去、`banto-tags` の検証にレジストリの述語を注入        |
| `BrokerSimRegistry::resolve`（`broker_glue.rs:716-720`）                    | **変えない**（`"modbus-tcp"` 以外を SLMP に倒す既定もそのまま。レジストリの id は 2 つなので同じ結果）                                                                 | R2b-5（値の層のシミュレータ）                                    |
| `"modbus-tcp"` でアドレスの解析を選ぶ 4 か所（`write_path` / `read_path`）  | **変えない**                                                                                                                                                           | R2b-2（`registry.get(id).parse_address`）                        |
| UI・設定パッケージの固定値（`configPackage.ts`・`ConnectionDrawer.svelte`） | 変えない                                                                                                                                                               | R2b-6 / R2b-7                                                    |

### 6.3 トリップワイヤ（`banto-drivers` の単体テスト、R1-3）

1. `builtin().registry.ids()` のすべてが `banto_tags::ALLOWED_PROTOCOLS` に含まれる（`HostBound` の id は `plc_connections.protocol` の値と一致していなければ接続を引けない）。
2. `builtin().broker` の id の集合 == `registry` のうち `HostBound` の id の集合（broker に居るのにレジストリに居ない、の逆も断る）。
3. `builtin().client_factory` が `ClientProtocol::ModbusTcp` / `Slmp` の両方で `Box<dyn PlcClient>` を返す（`task.rs:2051` の `default_client_factory_sends_the_configured_cpu_subcommand` と同じ形で、`cpu` と `word_order` が落ちていないことを含む。#334 / #605 の再発防止）。
4. `registry` の各 `descriptor.id` が `DriverId` の文法に合う。

### 6.4 注入点

| 場所                                                  | 今                                                                  | R1                                                                                                                                                                                                                          |
| ----------------------------------------------------- | ------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `HubRuntime::start(config)`（`runtime.rs:301`）       | `HubSessions::new(BackoffConfig)`（`:511`）が中で broker を作る     | `HubConfig` に `drivers: Arc<banto_drivers::Builtin>`（型は core 側の `HubDrivers { registry, broker, client_factory }` に写す。core は `banto-drivers` に依存しない）。`HubSessions::new(backoff, drivers.broker.clone())` |
| `CollectorManager::new`（`hub.rs:1162-1170`）         | `hub_client_factory(broker_handles)` を `rebuild` で組む（`:1580`） | `HubDrivers.client_factory` を既定として持ち、`hub_client_factory` の fallback（`broker_glue.rs:852, 867` の `default(spec)`）に使う                                                                                        |
| ChronoGazer `EngineRuntime` / `collect.rs:1919, 2573` | `banto_collect::default_client_factory()`                           | `EngineConfig.client_factory: ClientFactory`（既定 = `builtin().client_factory`。host が渡す）                                                                                                                              |
| `banto-hub-sink` のテスト（Hub をプロセス内に建てる） | `HubRuntime::start`                                                 | dev-dependency に `banto-drivers` を足して渡す                                                                                                                                                                              |
| テスト用の空レジストリ                                | -                                                                   | `HubDrivers::none()`（ドライバ無し。`DriverTable::new(vec![])`、`client_factory` は `ClientProtocol` を受けると panic ではなく `NotConnected` を返す偽クライアント）。R1-7b 後に core に残るテストが使う                    |

## 7. 適合テスト一式（`banto-driver-conformance`、R1-5）

### 7.1 固定する振る舞い

| #   | 振る舞い                                                                                                                                                | 対象                       | どう確かめるか                                                                                                                                  |
| --- | ------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------- |
| C1  | `parse_address(canonical) == parse_address(text)`（正規化の冪等）、`offset_address(.., 0) == canonical`                                                 | `DriverBase`               | 作者が渡す `StaticFixtures { valid: Vec<(text, kind, expected canonical)>, invalid: Vec<(text, kind)> }` を総当たり                             |
| C2  | `validate_config` は未知のキーを断らず、必須キーの欠けと型違いを `FieldError` で返す。`secret_keys` ⊆ 実際に読むキー                                    | `DriverBase`               | 最小の `Settings`・空の `Settings`・`port = "abc"`                                                                                              |
| C3  | 結果の件数と順序 = 要求の件数と順序（`covers` の網羅、重複なし）                                                                                        | Frame / Session            | 要求 N 件（同じアドレスの重複・順序の入れ替えを含む）                                                                                           |
| C4  | 致命 / 非致命の分類: 機器の拒否は `ItemError::Device`、壊れたフレームは `FatalError::Framing`、別プロトコルの表記 / 型と領域の不整合は `Unsupported`    | Frame / Session            | ピアに (a) 例外応答 (b) 長さ不整合 (c) 途中で切断、をさせる                                                                                     |
| C5  | 分割受信: 応答を 1 バイトずつ・境界をまたいで届けても同じ値                                                                                             | Frame（ハーネス）/ Session | ハーネスが `FrameRule` を解釈して分割（Frame）。Session はモックの TCP サーバーが分割送信                                                       |
| C6  | 遅れて届いた応答を次の要求に流用しない: step N を時間切れにし、その応答を N+1 の直前に流すと **接続致命**になり、N+1 の値として採用されない             | Frame（ハーネス）/ Session | `banto-plc/src/error.rs:39-47` の規則。Frame 型では契約の主体はエンジンなので、ハーネス自身のテストでもある                                     |
| C7  | 再接続: 接続致命の後、新しい `connect` / `create_connection` で読める。古い接続の状態（ハンドシェイクで得た値）を持ち越さない                           | Frame / Session            | ピアを一度落として立て直す                                                                                                                      |
| C8  | 1 接続 = 1 ソケット（#337）: 読みと書きを交互に 100 回行う間、ピアが受けた接続は 1 本                                                                   | Session                    | モックの TCP サーバーが accept を数える（`apps/banto-hub/core/tests/integration.rs` の `modbus_collection_uses_exactly_one_socket` と同じ観点） |
| C9  | `max_len` 超過・`DriverDecides` が完結しないまま伸びる → 致命（読み取りが止まらないことがない）                                                         | Frame                      | ピアが延々と送る                                                                                                                                |
| C10 | 書き込みの非合体: `encode_write` は隣接しない要求を 1 つの step にまとめない（`banto-plc-write/src/client.rs:62-68` の gap-tolerance-zero）             | Frame（write）             | 離れた 2 アドレスを 1 バッチで                                                                                                                  |
| C11 | パニックしない: 任意のバイト列（長さ 0〜max_len、乱数の固定シード 1,000 本）を `decode_read` / `is_complete` / `handshake_next` に渡しても `Err` で返る | Frame                      | `std::panic::catch_unwind` で包み、panic は失敗                                                                                                 |
| C12 | `TagKind::Str` の語数の範囲外（0・上限 + 1）は `Unsupported`（`PlcError::StringSpanUnsupported` と同じ）                                                | Frame / Session            |                                                                                                                                                 |

### 7.2 形（第三者が Hub なしで走らせる）

```rust
// 第三者の crate の tests/conformance.rs
use banto_driver_conformance::{frame, StaticFixtures};
#[tokio::test]
async fn conforms() {
    let driver = my_driver::Driver::default();
    frame::run_all(&driver, &StaticFixtures { .. }, &my_driver::test_peer::Responder::new()).await.unwrap();
}
```

- `frame::run_all(driver: &dyn FrameDriver, fixtures, peer: &mut dyn Responder)`: `Responder { fn reply(&mut self, request: &[u8]) -> Reply }`、`Reply { Bytes(Vec<u8>) | Chunks(Vec<Vec<u8>>, Duration) | Silence | Close }`。ハーネスが §4.2 の手順を実行し C1〜C12 を順に走らせ、失敗した項目を一覧で返す（`Vec<Failure { id: &'static str, detail: String }>`）。
- `session::run_all(driver: &dyn SessionDriver, fixtures, peer: MockTcpServer)`: `MockTcpServer` は `tokio::net::TcpListener` を 127.0.0.1 の空きポートに立て、`Responder` に委ねる。Hub のコードは一切要らない。
- `banto_driver_conformance::static_checks(driver: &dyn DriverBase, fixtures)`（C1・C2 だけ。tokio 不要）。

### 7.3 Modbus / SLMP での走らせ方（CI）

R1 の Modbus / SLMP は `HostBound` なので、`frame::run_all` は走らせられない。R1-5 でやること:

1. `static_checks` を `banto-driver-modbus` / `-slmp` の `tests/conformance.rs` で走らせる（C1・C2。fixtures は `banto-plc/src/address.rs` と `slmp/address.rs` の既存テストの入力から写す）。
2. `session::run_all` を **dev-only のアダプター** `LegacySession`（`Box<dyn BrokerSession>` を `api::Session` に包む。`TagSpec.address.canonical` を `Address::parse` / `parse_slmp` で `BatchReadRequest` に戻す。R1-4 の `classify` でエラーを写す）で走らせる。ピアは `banto-plc` の Modbus シミュレータ（`modbus/simulator.rs`、feature `simulator`）と `banto-plc-write` の SLMP シミュレータ（`slmp/simulator.rs`）を `Responder` に包んだもの。C4 の壊れたフレームと C6 の遅延は、シミュレータの前に置く TCP のプロキシ（conformance 側の `FaultProxy`: 遅延・分割・切断を挟む）で起こす。
3. これで §7.1 のうち C1〜C8・C12 が Modbus / SLMP で CI に乗る。C9〜C11 は Frame 型になる R2b-3 で乗る。**P0-3（振る舞いの固定テスト）はこの 2 を核にする**: R2b-3 で I/O を一本化したとき、同じ `session::run_all` / `frame::run_all` が新しいエンジンに対して通ることが完了条件。

### 7.4 偽ドライバ

`banto_driver_conformance::fake`: `FakeSessionDriver`（`HashMap<canonical, Value>` を返す。`fail_next: Option<FatalError>` で致命を起こせる）と `EchoFrameDriver`（§4.3）。R1-7b の後に core に残るテストと、R2a の `push` の実装前のテストが使う。

## 8. シリアルを見込んで R1 で押さえる形（R2b-3 で API を壊さないため）

| 論点                                     | R1 の型での扱い                                                                                                                                                                                                                                                                        |
| ---------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| トランスポートの種類                     | `DriverDescriptor.transports: Vec<Transport { Tcp, Serial, SerialOverTcp }>` と設定キー `transport`。R1 は `Tcp` だけを受け、ほかは `validate_config` で断る（`FieldError`）。R2b-3 でエンジンが `Serial`（COM 直結）と `SerialOverTcp`（変換器へ生データ）を足しても API は変わらない |
| フレームの終わりを無音の間隔で判定しない | `FrameRule` に「無音」の変種を**入れない**（plan §7 Q4）。終端は `Fixed` / `LengthPrefixed` / `Terminator` / `DriverDecides` のどれか。Modbus RTU の CRC は `DriverDecides` で CRC が合った時点を完結とする                                                                            |
| 変換器を通る分のタイムアウト             | 接続設定の `response_timeout_ms`（接続ごと）。`Step` には持たせない。`#[non_exhaustive]` なので、半二重の切り替え待ち（`turnaround`）が要ると分かったら `Step` に `Option<Duration>` を**後から足せる**（§9）                                                                          |
| RS-485 のマルチドロップを 1 接続で       | `DriverAddress.station: Option<u32>`（§3.3）と `Planned.covers` の組で、1 接続の中で局番の違う要求を別の step に分ける。設定キー `unit_id` は「局番を持たないアドレスの既定」になる。R1 の Modbus TCP は `station = None`                                                              |
| シリアルの速度等は変換器側               | `Settings` は自由なキーなので、COM 直結のときだけ `serial_port` / `baud` 等を足す。R1 では予約名だけ決める（`keys::SERIAL_PORT`）                                                                                                                                                      |
| 1 ポートに複数接続                       | R2b-3 の論点。API では「1 `FrameConnection` = 1 論理接続」のままにし、ポートの共有はエンジンの中（`Transport` の多重化）で吸収する。API に出さない                                                                                                                                     |

## 9. 版と安定性の方針

### 9.1 semver

- R1 で `0.1.0-alpha.N`（`HostBound` を含む）。R2b-3 で `HostBound` を消し `0.1.0`。WIT が確定する W 段で `1.0.0`。0.x の間は minor で破壊してよいが、**変更履歴（`crates/banto-driver-api/CHANGELOG.md`）に「何が壊れ、どう直すか」を必ず書く**（plan §5）。
- crates.io への初回公開の時期は §12 Q3。
- 破壊になるもの: trait への既定実装の無いメソッドの追加 / 型の変更 / enum の variant の削除・改名 / struct の field の削除・改名 / `DriverId` の文法の縮小。
- 破壊にならないもの（`#[non_exhaustive]` のおかげ）: enum の variant の追加 / struct の field の追加 / 既定実装つきのメソッドの追加 / 設定キーの追加。利用側は `..` を使った match と `Step::new` 等のコンストラクタを使う（適合テストの `examples` がその書き方の手本）。
- MSRV: `rust-version = "1.98"`（`rust-toolchain.toml` のピン）。上げるときは minor を上げ、CHANGELOG に書く。
- `banto-driver-conformance` は API に caret で依存し、API の minor が上がったら合わせて上げる。

### 9.2 WIT との対応表（R1 の trait ↔ plugin-design §6.2）

| Rust                                                    | WIT                                                            | 備考                                                           |
| ------------------------------------------------------- | -------------------------------------------------------------- | -------------------------------------------------------------- |
| `DriverDescriptor`                                      | `manifest.json`（§4.2）                                        | `api_version` ↔ `hubApi`                                       |
| `DriverBase::validate_config`                           | `config.validate`                                              |                                                                |
| `DriverBase::secret_keys`                               | `config.secret-keys`                                           |                                                                |
| `DriverBase::parse_address(text, kind)`                 | `address.parse(text, data-type)`                               | `kind` = `data-type` + `string-words`                          |
| `DriverBase::offset_address`                            | `address.offset`                                               |                                                                |
| `DriverBase::address_help`                              | `address.help`                                                 |                                                                |
| `DriverAddress`                                         | `address.parsed`                                               | `driver`・`station` は Rust 側の追加（WIT では文脈から分かる） |
| `FrameDriver::create_connection`                        | `connection.create(settings)`                                  |                                                                |
| `FrameConnection::handshake_next`                       | `connection.handshake-next`                                    |                                                                |
| `FrameConnection::is_complete`                          | `connection.is-complete`                                       |                                                                |
| `FrameConnection::plan_read`                            | `read.plan`                                                    | `&self` ↔ `borrow<connection>`                                 |
| `FrameConnection::decode_read`                          | `read.decode`                                                  |                                                                |
| `FrameConnection::encode_write`                         | `write.encode`                                                 | `Capabilities.write` ↔ world `driver-rw`                       |
| `FrameConnection::check_write`                          | `write.check`                                                  |                                                                |
| `TagKind::Numeric(DataType)` / `TagKind::Str{words,..}` | `tag-spec.data-type` / `tag-spec.string-words`                 | `encoding` は WIT に無い（Hub が decode）                      |
| `RawValue`                                              | `types.value`                                                  |                                                                |
| `Value`                                                 | -（Hub 側の型）                                                |                                                                |
| `ItemError` / `DeviceError`                             | `types.item-error` / `device-error`                            |                                                                |
| `FatalError::{Framing, Protocol, InvalidConfig}`        | `types.fatal-error`                                            | `Io` / `Timeout` は WIT に無い（Hub 側）                       |
| `FrameRule` / `PrefixSpec` / `Step` / `Planned`         | `session.frame-rule` / `prefix-spec` / `step` / `read.planned` | `DriverDecides` ↔ `plugin-decides`                             |
| `SessionDriver` / `Session`                             | -（WIT の対象外。組み込みのみ）                                |                                                                |
| `DriverKind::HostBound`                                 | -（R1 限定）                                                   |                                                                |

W 段で WASM ホストを足すときは `WasmFrameDriver: FrameDriver`（wasmtime の `Store` を `FrameConnection` が持つ）が「もう 1 つの実装」になる。上の表に無い関数を WIT に足す必要が出たら、本書と plugin-design §6.2 を同じ PR で直す。

## 10. R1 の PR の分け方（plan §4 の R1-1〜R1-7 を詳細化）

共通の決まり: 各 PR は振る舞いを変えない。`cargo fmt --all --check` → `cargo test -p <触った crate>` → `cargo clippy -p <触った crate> --all-targets -- -D warnings` → **`cargo check --workspace --all-targets`**（公開 API の形を変える PR が多いので毎回。implementation-checklist §2-3）→ `pnpm exec prettier --write <触った md/mjs>` → 新しいディレクトリを足した PR は `git add` 後に `node --test .github/scripts/ci-changes.test.mjs`（§2-6）→ `cargo deny check`。E2E は R1-4・R1-7 だけ（Hub と ChronoGazer の両方、1 回ずつ）。ロールバックはどれも「その PR を revert」で戻る（DB のマイグレーションを含む PR は無い）。

| PR    | 範囲                                                                                                                                                                                                                                                                                                                                                                                     | 触るファイル                                                                                                                                                                                                                                                                                                                                                    | 不変条件・完了条件                                                                                                                                                                                                                                                                                                                                                |
| ----- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| R1-1  | `banto-driver-api` を作り §3 の型を入れる。`banto-plc` の `DataType`・`TagValue`・`PlcValue`・`StringEncoding`・`WordOrder` を API からの再輸出に替える。`DriverAddress`・`ItemError`・`FatalError`・`Settings`・`TagKind`・`TagSpec`・`RawValue` を追加                                                                                                                                 | 新 `crates/banto-driver-api/{Cargo.toml,src/lib.rs,src/types.rs,src/address.rs,src/error.rs,src/settings.rs,CHANGELOG.md}`、ルート `Cargo.toml`（members）、`crates/banto-plc/{Cargo.toml,src/types.rs,src/decode.rs,src/lib.rs}`、`.github/scripts/ci-changes.test.mjs`（G に追加）                                                                            | `cargo publish --dry-run -p banto-driver-api` が通る。`banto_plc::DataType` 等を使う呼び出し側に差分が無い（`git diff --stat` で `apps/` が 0）。`DataType::parse` の受理集合が変わらない（既存テスト `data_type_parse_*`）。`cargo test --workspace` の件数が前後で同じ                                                                                          |
| R1-2  | §4・§5・§6.1 の trait と `DriverRegistry`。`banto-driver-conformance` の骨格（`static_checks`、ハーネス、`EchoFrameDriver`、`FakeSessionDriver`）。`banto-broker` の `BrokerSession` / `SessionError` / `Connector` / `DriverSpawn` を `pub` に                                                                                                                                          | `crates/banto-driver-api/src/{driver.rs,frame.rs,session.rs,registry.rs}`、新 `crates/banto-driver-conformance/`、`crates/banto-broker/src/{session.rs,lib.rs}`                                                                                                                                                                                                 | `EchoFrameDriver` が `frame::run_all` の C3〜C7・C9〜C12 を通る（ハーネス自身の検証）。`cargo doc -p banto-driver-api --no-deps` が警告ゼロ。`broker` の `pub` 化は呼び出し側に影響なし                                                                                                                                                                           |
| R1-3  | `DriverTable` の注入（§4.4）。`banto-drivers` と `builtin()`、トリップワイヤ（§6.3）。`HubRuntime` / `CollectorManager` / `EngineRuntime` に `HubDrivers` を渡す。`is_supported_protocol` を `#[deprecated]` に                                                                                                                                                                          | `crates/banto-broker/src/lib.rs`（`DRIVERS` → `DriverTable`、`BrokerSupervisor::spawn` の引数）、新 `crates/banto-drivers/`、`apps/banto-hub/core/src/{runtime.rs,hub.rs,broker_glue.rs}`、`apps/chronogazer/core/src/{engine.rs,collect.rs}`、ホストの bin・shell・`banto-hub-sink` の `Cargo.toml`                                                            | この時点では `banto-drivers` の中身は `banto-broker` の `slmp_driver` / `modbus_driver` を呼ぶだけ（モジュールはまだ移さない）。既存テスト全通過。`cargo test -p banto-drivers`                                                                                                                                                                                   |
| R1-4  | `slmp_driver.rs` / `modbus_driver.rs` を `banto-driver-slmp` / `-modbus` へ移し、`DriverBase` を実装（`parse_address` は `Address::parse*` を包む、`classify` の表 §3.4、`settings_from_connection`）。`hub_client_factory` の腕の統合。`is_supported_protocol` の呼び出し 4 か所を `HubSessions::is_broker_managed` に直して旧関数を消す                                                | 新 `crates/banto-driver-{modbus,slmp}/`、`crates/banto-broker/src/{lib.rs,slmp_driver.rs→削除,modbus_driver.rs→削除}`、`crates/banto-drivers/src/lib.rs`、`apps/banto-hub/core/src/{hub.rs,read_path.rs,rest.rs,write_path.rs,broker_glue.rs}`                                                                                                                  | `git log --follow` で移動が追えること（`git mv`）。`banto-broker` の `modbus_driver_shares_one_transaction_id_counter_across_reads_and_writes` 等のテストもドライバ crate へ移って通る。Hub の E2E・ChronoGazer の E2E が通る。`apps/banto-hub/core/tests/integration.rs` の `modbus_collection_uses_exactly_one_socket` が通る                                   |
| R1-5  | §7.3 の 1・2（`static_checks`、`LegacySession` での `session::run_all`、`FaultProxy`）。Modbus / SLMP の fixtures                                                                                                                                                                                                                                                                        | `crates/banto-driver-{modbus,slmp}/tests/conformance.rs`、`crates/banto-driver-conformance/src/{session.rs,proxy.rs}`                                                                                                                                                                                                                                           | C1〜C8・C12 が両ドライバで緑。**反証**: `LegacySession` の `classify` で `SlmpEndCode` を `Fatal` に倒すと C4 が落ちる（implementation-checklist §5 の反証の確認）。`.config/nextest.toml` の test-groups に入れる必要が無いこと（空きポートを都度取る）を確認                                                                                                    |
| R1-6  | CI の判定。`RUST_PACKAGES`・`JOB_PACKAGES` はまだ core のまま。`ci-changes.test.mjs` の固定グラフ G に `banto-driver-api`・`-conformance`・`-modbus`・`-slmp`・`banto-drivers` を足し、「`crates/banto-driver-modbus` だけの変更 → 届く先に `banto-drivers` と host・shell・sink が入り、E2E は両方」の行を足す（この時点では core も入る。R1-7b で外れる期待値に書き換える）            | `.github/scripts/ci-changes.test.mjs`（必要なら `ci-changes.mjs` のコメント）                                                                                                                                                                                                                                                                                   | `node --test .github/scripts/ci-changes.test.mjs` 緑。「リポジトリの全ファイルが明示の規則に当たる」「crates/ の下の追跡ファイルはすべてどれかのパッケージに属する」の 2 本が新ディレクトリで落ちないこと（#632 の再発防止）                                                                                                                                      |
| R1-7a | ホストの bin を `banto-hub-host` / `chronogazer-host` へ（§2.4）。ビルドの入口の package 名を全部直す。`ci-changes.mjs` の `RUST_PACKAGES.hub` に `banto-hub-host`、`chronogazer` に `chronogazer-host`、`JOB_PACKAGES.run_e2e_hub = ['banto-hub-host']`、`run_e2e_chronogazer = ['chronogazer-host', 'chronogazer-core']`（`dev_plc` が core に残るため）、`run_linux_clippy` に両 host | `apps/banto-hub/{core,host}/`、`apps/chronogazer/{core,host}/`、ルート `Cargo.toml`、`.github/workflows/{ci.yml,opcua-interop.yml}`、`.github/scripts/ci-changes.{mjs,test.mjs}`、`scripts/build-release.ps1`、`apps/banto-hub/installer/src/main.rs`、`AGENTS.md`、`apps/banto-hub/README.md`、`docs/implementation-checklist.md` §2-7、`.config/nextest.toml` | `cargo build -p banto-hub-host --bin banto-hub --features embed-ui` と `-p chronogazer-host --bin banto-serve --features embed-ui` で両 E2E が通る。`chronogazer_server.rs` のテストが host 側で通る。インストーラ（`build-release.ps1`）の dry run。`banto-hub-elev` のサービス登録の実機確認は不要（移動のみ）だが、`win_service_tests.rs` が host 側で通ること |
| R1-7b | 実ドライバを要る統合テストを host 側へ移し、core の dev-dependency から `banto-drivers` を外す。core に残るテストは `HubDrivers::none()` か偽ドライバ。`ci-changes.test.mjs` の期待値を「ドライバだけの変更 → core は対象外・両 E2E は対象」に書き換える                                                                                                                                 | `apps/banto-hub/{core,host}/tests/`、`apps/chronogazer/{core,host}/tests/`、両 core の `Cargo.toml`、`.github/scripts/ci-changes.test.mjs`、`.config/nextest.toml:68`                                                                                                                                                                                           | 移動前後で `cargo test --workspace` の件数が同じ（移動元の合計 = 移動先の合計、削除なし）。`node --test` で「`crates/banto-driver-modbus/src/lib.rs` だけ → `rust_args` に `banto-hub-core` / `chronogazer-core` が**無く**、`run_e2e_hub` と `run_e2e_chronogazer` が true」。`cargo metadata` の実グラフで `banto-hub-core` の deps に `banto-drivers` が無い   |

R1 の完了条件（plan §4）: 既存テストが全部通り、Modbus / SLMP が適合テスト（§7.3 の範囲）を通る。加えて本書では、R1-7b の判定テストが緑であることを完了条件に足す。推定の規模: R1-1〜R1-3 は各 1 PR・中、R1-4 は大（移動 + 4 か所の差し替え）、R1-5 は中、R1-6 は小、R1-7a は中（機械的だが入口が多い）、R1-7b は大（移動は機械的だが `use` の書き換えが多い）。

## 11. 書き込みの安全との関係（tag-server-design §6、D5）

- API の `DriverAddress.writable_area` と `Capabilities.write` は**プロトコルの事実**であり、書き込みの許可の判断ではない。`write_path.rs` の関門（`writable`・API キーの `write` スコープ・受付トグル・頻度・監査・broker の直列化）は R1 で一切触らない（§10 の R1-4 で触るのは `is_supported_protocol` の呼び出し 1 行だけ）。
- `FrameConnection::encode_write` の非合体（C10）は `banto-plc-write` の gap-tolerance-zero をそのまま API の契約にしたもの。

## 12. 未決事項（オーナー判断。短く）

| Q   | 問い                                                                                                                                                  | 推奨                                                                                                                                                                                     |
| --- | ----------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Q1  | 適合テスト一式を plan §4 R1-5 の「`banto-driver-api` に置く」から**別 crate `banto-driver-conformance`** に変えてよいか（§2.2）                       | 別 crate。API を依存ゼロに保つため。plan §4 の R1-5 の行を直す                                                                                                                           |
| Q2  | R1 の CI の短縮が「アダプター crate と組み立て役の変更」に限られ、`banto-plc` の実装本体の変更は R2b まで core に届き続けること（§2.5）を受け入れるか | 受け入れる。実装本体を R1 で移すと `rest.rs` 等の `banto_plc::` 直接参照 10 か所超を同時に直すことになり R2b-2 と重なる。plugin-design §12.6 の表に「R1 ではアダプター分だけ」と注記する |
| Q3  | crates.io への初回公開の時期: (a) R1 完了直後に `0.1.0-alpha`（`HostBound` 入り）を公開、(b) R2b-3 で `HostBound` を消してから `0.1.0` を公開         | (a)。名前の予約と、別リポジトリ先行（tag-server-design §3.4.1 の staging）の KV / OPC UA のアダプターが `DriverBase` と `SessionDriver` を試せる。alpha なので壊してよい                 |
| Q4  | R1-7 を 7a / 7b に分け、7b（統合テストの移動）まで R1 に含めるか                                                                                      | 含める。7b が無いと §12.6 の利点が出ない。7b は移動だけで書き換えはしない                                                                                                                |
| Q5  | R2a で ChronoGazer の `hub` 接続（`ExternalSource`）も `SessionDriver` に寄せるか                                                                     | R2a で `push` を `SessionDriver` にし、`ExternalSource` は R2a の完了条件（同じ値・品質）を見てから R2b で寄せる。R1 では触らない                                                        |
| Q6  | `DriverAddress.station` を R1 の型に入れるか（シリアルの先取り、§8）                                                                                  | 入れる。`#[non_exhaustive]` で後から足すこともできるが、適合テストの C3（順序）が局番を跨ぐ要求を最初から前提にできる                                                                    |
| Q7  | ドライバ crate の名前: `banto-driver-modbus` / `banto-driver-slmp`（本書）か `banto-plc-modbus` 系か                                                  | `banto-driver-*`。API の公開後に別リポジトリへ出す単位（plan §7 Q2）と同じ名前にしておく                                                                                                 |

## 13. 既存の計画と食い違う点（レビューで直す候補）

1. plan §4 R1-5「適合テスト一式を `banto-driver-api` に置き」→ 別 crate（Q1）。
2. plan §4 R1-7「bin を分ければ core のテストが対象から外れる」→ dev-dependency も辺になるので、統合テストの移動（R1-7b）が要る（§2.4）。
3. plugin-design §12.6「ドライバを変えたときの CI の対象はドライバ・組み立て役・バイナリだけ」→ R1 ではアダプター crate の変更に限る。実装本体（`banto-plc` の `slmp/` / `modbus/`）は R2b-2 / R2b-3 で移るまで core に届く（§2.5、Q2）。同節の「`banto-collect` / `banto-broker` ── `banto-hub-core` ← API にだけ依存」も R2b の姿であり R1 ではない。
4. plan §4 R1-4「Modbus / SLMP を trait の実装に載せ替える（I/O は今のまま）」→ フレーム型にもセッション型にもできず（§4.4 の A・B）、R1 は `DriverBase` + `HostBound` の載せ替えになる。「I/O は今のまま」の意図どおりだが、plan の文言は「静的な部分だけ API の形にする」に直す。
5. plugin-design §6.2 の WIT で `tag-spec` に文字列の `encoding` が無いのは意図どおり（Hub が decode）だが、ネイティブの `TagKind::Str` は R1 のアダプターが `banto-plc` で decode するため `encoding` を持つ（§3.2）。WIT 側に合わせて「エンジンが decode する」形に揃えるのは R2b-3。
6. plan §4 の R1 の完了条件「Modbus / SLMP が適合テストを通る」は、R1 では §7.3 の範囲（C1〜C8・C12、`LegacySession` 経由）に限ると明記する。C9〜C11 は R2b-3。
