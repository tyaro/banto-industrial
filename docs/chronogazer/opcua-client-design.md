# ChronoGazer の OPC UA のクライアント（共有のドライバ）の設計

作成日: 2026-11-24
状態: **設計確定（2026-11-24 のオーナー決定を記録。§7 の 11 項目も同日に決めた。実装は banto v8 の後で、未着手。2026-11-26: §6 の段 1 の banto-opcua 側は v0.3.3 で済み、Hub の固定も v0.3.3 に上げた（#1332。None・Anonymous の公開は `with_none_endpoint(true)` で保つ）。段 2 は次の PR）**
関連: [#1324](https://github.com/tyaro/banto-industrial/issues/1324)（Hub 経由の専用のつなぎをやめ、OPC UA でつなぐ）、[#678](https://github.com/tyaro/banto-industrial/issues/678)（OPC UA のクライアントのドライバ）、[#383](https://github.com/tyaro/banto-industrial/issues/383)（3 ドライバの決定。この設計で改める）、[#700](https://github.com/tyaro/banto-industrial/pull/700)（OPC UA のクライアントは WASM の対象外）、[#1306](https://github.com/tyaro/banto-industrial/issues/1306)（記録の持ち主は ChronoGazer）、tyaro/banto-opcua の [ADR-0005](https://github.com/tyaro/banto-opcua/blob/v0.2.0/docs/adr/0005-client-as-hub-driver.md)・[ADR-0006](https://github.com/tyaro/banto-opcua/blob/v0.2.0/docs/adr/0006-client-subscription-latest-values.md)、提案 [banto-opcua#54](https://github.com/tyaro/banto-opcua/issues/54)（セキュリティ）・[#55](https://github.com/tyaro/banto-opcua/issues/55)（属性）・[#56](https://github.com/tyaro/banto-opcua/issues/56)（型）、[記録計要件](recorder-requirements.md) §7、[画面改善案](chronogazer-ui-improvement-proposal.md) §5、[タグサーバーの設計](../hub/tag-server-design.md) §3.4.1・§5.5、[Driver API の設計](../hub/driver/banto-driver-api-design.md)

## 1. 目的と範囲・決まったこと

ChronoGazer（記録計）が、Hub にも他社の OPC UA サーバーにも、同じ OPC UA のクライアントでつないで記録できるようにする。OPC UA のクライアントは Hub 専用にせず、共有のドライバ（`crates/banto-drivers` に登録する組み込みのドライバ）として作り、Hub と ChronoGazer の両方が使う。

- 対象: OPC UA のクライアントのドライバの形（置き場所・アドレス・設定・型・品質と時刻・再接続・読み方）、ChronoGazer での使い方（接続・記録項目・表示グループ・REST・MCP）、セキュリティの段階、段取り、Hub 経由の今のつなぎを消す範囲。
- 対象外: banto-opcua の部品の中身（tyaro/banto-opcua の ADR）、Hub の OPC UA サーバーの作り（[tag-server-design.md](../hub/tag-server-design.md) §5.5）、2 段目の保存（[historian-archive-design.md](historian-archive-design.md)）、ChronoGazer から機器への書き込み（ChronoGazer は書かない）。

### 1.1 決まったこと

| 日付       | 決定                                                                                                                                                                                                                                                                                                                                          | 出どころ                      |
| ---------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------- |
| 2026-10-17 | OPC UA のクライアントを SLMP・Modbus と同じ並びのドライバにする。最初は SecurityPolicy None / Anonymous だけ。読み方は周期 Read で、購読は後から。書き込みは最初から（Hub で使う）。タグは NodeId を手で入れ、Browse は後回し。                                                                                                               | #678                          |
| 2026-10-18 | 購読モードでは、購読で受けた最新値を部品が持ち、ドライバは周期ごとに取り出す。読み方（周期 Read か購読か）は接続ごとに選ぶ。                                                                                                                                                                                                                  | ADR-0006                      |
| 2026-10-19 | OPC UA のクライアントのドライバは、Hub の OPC UA サーバーと同じ `async-opcua` のリビジョンにそろえる。                                                                                                                                                                                                                                        | tag-server-design §3.4.1 の 4 |
| 2026-11-17 | ドライバは ChronoGazer も使える共有の置き場に作る。ChronoGazer では読み取り（収集）だけを使う。                                                                                                                                                                                                                                               | #678 のコメント               |
| 2026-11-22 | 機能の実装は banto v8 まで止める。                                                                                                                                                                                                                                                                                                            | STATUS                        |
| 2026-11-24 | ChronoGazer の Hub 経由の専用のつなぎ（banto-tagclient・banto-hub-bootstrap・Hub の TLS の固定・`hub_tags` など）をやめ、共有のドライバの OPC UA のクライアントで Hub にも他社のサーバーにもつなぐ。**#383（SLMP / Modbus TCP / Hub 経由の 3 ドライバ、2026-09-17）を改める**。互換は保たない（旧い形は OPC UA のドライバができた後に消す）。 | #1324                         |
| 2026-11-24 | **ChronoGazer は OPC UA から周期で記録する**。今の SLMP・Modbus と同じく、収集グループの周期で 1 回ずつ記録する。購読は最新値を持つために使ってよいが、記録の単位は周期。                                                                                                                                                                     | この設計                      |
| 2026-11-24 | **string のタグは記録しない**。記録するのは数値と bit だけ。string は理由を添えて選べなくする。                                                                                                                                                                                                                                               | この設計                      |
| 2026-11-24 | §7 の 11 項目を決めた（推奨どおり。6 は今の Hub 経由のつなぎが未リリースなので利用者への配慮をせず消す、7 は確かめる相手に UaExpert と Unified Automation のデモサーバー（UaAnsiCServer）を足す）。                                                                                                                                           | この設計                      |
| 2026-11-24 | 今回は設計書まで。実装は banto v8 の後。                                                                                                                                                                                                                                                                                                      | この設計                      |

### 1.2 なぜ OPC UA にするか（#1324 の要約）

- 専用のつなぎを丸ごと消せる（Hub と ChronoGazer の版合わせ、TLS の固定、自動接続、squat の試験）。#1293〜#1295 も ChronoGazer のためには要らなくなる。
- OPC UA を話すものなら何でも記録できる（他社の SCADA、OPC UA を持つ PLC やゲートウェイ）。
- 同じドライバで、Hub も他社の OPC UA サーバーから集められる（#678）。
- 「記録の持ち主は ChronoGazer、Hub は短い期間だけ」（#1306）と合う。標準の口の上で、お互いの中を知らずに済む。

## 2. 今の作り

### 2.1 共有のドライバ（Driver API）

- **`crates/banto-driver-api`**（crates.io に公開する型と trait だけの crate、版 0.3.0）が、ドライバと使う側（Hub のエンジン、ChronoGazer の収集）の言葉をそろえる。
  - `DriverBase`: 設定の確かめ（`validate_config`）、秘密の設定のキー（`secret_keys`）、アドレスの解析（`parse_address(text, kind)` が不透明な `DriverAddress` を返す）、アドレスの説明（`address_help`）、接続試験で読む 1 点（`probe`）、設定の schema（`settings_schema`。画面の汎用のフォームが描く）、扱える型（`supported_kinds`）。
  - ドライバは 2 種類（`registry.rs` の `DriverEntry::frame`・`DriverEntry::session`）。SLMP・Modbus は**フレーム型**（I/O は `banto-driver-engine`）。I/O を自分で持つものは**セッション型**（`SessionDriver::connect` → `Session`）。`session.rs` の冒頭に、OPC UA・DB・`push` がセッション型の例として書いてある。plugin-design（[banto-hub-driver-plugin-design.md](../hub/driver/banto-hub-driver-plugin-design.md) §6.1）でも OPC UA のクライアントは組み込みのセッション型に決まっている。
  - `Session::read_batch(&[TagSpec])` は `Result<Vec<Result<Value, ItemError>>, FatalError>`。**`Err` は接続ごとの失敗**（broker が切って張り直す）、項目ごとの失敗は `ItemError`。`write_batch` も同じ形で、既定は「書き込みに対応していない」。
  - 読んだ値の型 `Value` は `Bit(bool)`・`F64(f64)`・`Str(String)` の 3 つだけ。**品質と時刻を載せる場所が無い**（Good か `ItemError` かの 2 択）。§3.6 で扱う。
  - `ItemError` は `Device`・`Unsupported`・`Rejected`・`VerificationFailed`。`#[non_exhaustive]` なので足せる。`Value` は `banto-plc` から移した型で `#[non_exhaustive]` ではなく、足すと破壊的変更になる（crate の冒頭の説明）。
- **`crates/banto-drivers`** が組み込みのドライバの登録簿を作る（`builtin().registry`。今は `modbus-tcp` と `slmp`、ほかに WASM を足す口）。Hub と ChronoGazer の host がこれを詰める（ChronoGazer は `apps/chronogazer/core/src/engine.rs` の `banto_drivers::builtin().registry`）。
- **接続の表** `plc_connections`（`crates/banto-tags`）は migration `0025` で `protocol` の `CHECK (protocol IN (...))` を外し、設定はドライバごとの JSON（`settings`）に移った。登録簿を持つサービス（Hub と ChronoGazer）は、ドライバの `validate_config` を通す。**新しいドライバを足すのに migration は要らない**（`ALLOWED_PROTOCOLS` は登録簿を持たないサービス向けの古い規則として残っている）。
- アドレスの解析は `banto-tags` の登録で `parse_address` を呼ぶ。保存の時点で解析できないアドレスは断る。
- シミュレーション（`simulation = true`）は `banto-collect` の `BrokerSimRegistry` が、フレーム型・セッション型を問わず `SimDevice` に差し替える（`broker_sim.rs` の冒頭）。OPC UA の接続もこの仕組みで模擬できる見込み（実装のときに確かめる）。

### 2.2 ChronoGazer が SLMP・Modbus をどう使っているか

- 収集は `apps/chronogazer/core/src/collect.rs`（`banto-collect` の `Collector` を配線）。SLMP・Modbus の接続は、Hub と同じ `banto-broker` のセッション（`banto_collect::BrokerSessions`）を通して読む。計画はドライバの `parse_address` で組む。
- 収集グループの周期ごとに、そのグループのタグをまとめて読み、1 回の周期を 1 行として保存する。保存（`banto-tstore`）は **1 つの時刻（`ptime_ms`、ホストの時計）と `f64` の値の並び**（`writer.rs` の `append(group_key, ptime_ms, &[Option<f64>])`）。**タグごとの時刻は持たない**。i64・u64 も `f64` で持つので、2^53 を超える整数は今も精度が落ちる。
- 品質は Good・Bad を保存し、Stale は読むときに求める（`current.rs`）。外から受けた「stale」は Stale として持ち、記録は `null`（`external.rs` の表）。
- 接続の画面は `GET /api/drivers` の schema から汎用のフォーム（`apps/chronogazer/src/lib/components/DriverSettingsForm.svelte`）で描く。MCP の `list_drivers`・`create_connection`・`create_tag` も登録簿を通す。

### 2.3 Hub 経由のつなぎ（消す対象）

#383 段階 1〜3（2026-09〜10）で作った、ChronoGazer が Hub の REST・WebSocket から値を受ける経路。ChronoGazer が OPC UA で Hub につなげるようになった後に、次を消す（§6 の段 5）。**Hub・Sink も使っている crate は残す**（下の「残すもの」）。

**ChronoGazer の Rust**

| 対象                                                                                       | 中身                                                                                                                                                                                                                      |
| ------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `apps/chronogazer/core/src/hub.rs`（約 7000 行）と `hub/tests/`                            | Hub 接続のサービス（`HubService`。bootstrap の配線、購読の世代、`collect_source` = `ExternalSource` の実装）                                                                                                              |
| `apps/chronogazer/core/src/hub_tags.rs`                                                    | 「Hub から追加」（`register_from_hub`）。表示グループの保存も中で呼ぶ                                                                                                                                                     |
| `apps/chronogazer/core/src/hub_tls.rs` と `hub_tls/`（`fake.rs`・`places.rs`・`tests.rs`） | 同じ PC の Hub の証明書の固定（#644 PR3b・#1121 PR4a）                                                                                                                                                                    |
| `apps/chronogazer/core/src/key_store.rs`                                                   | Hub の API キーの置き場（`<root>\secrets\hub-keys\`、DPAPI）                                                                                                                                                              |
| `apps/chronogazer/core/src/collect.rs` の「Hub 経由の接続」の部分                          | `attach_hub`・`prepare_collect`・`release_collect`、`hubTagNotFound`・`hubTagTypeChanged`・`hubSourceUnavailable`・`hubNotConfigured` の除外                                                                              |
| `apps/chronogazer/core/src/engine.rs` の Hub の組み立て                                    | 起動の 5（`HubService::new`・`attach_registry`）と終了                                                                                                                                                                    |
| `apps/chronogazer/core/src/display_groups.rs` の `hubTag`・`hubTagGroup`                   | ペンが Hub のカタログのタグを直に指す口（#1179 PR5）                                                                                                                                                                      |
| `apps/chronogazer/core/src/rest.rs`・`rest/openapi.rs` の Hub の口                         | `/api/hub`・`/api/hub/connect`・`/api/hub/adopt-key`・`/api/hub/refresh`・`/api/hub/subscription`・`/api/hub/catalog`・`/api/hub/legacy-selection/dismiss`・`POST /api/tags/from-hub`                                     |
| `apps/chronogazer/core/src/mcp.rs` の `hubTag`・`hubTagGroup`                              | 表示グループの作成・更新のツールの引数                                                                                                                                                                                    |
| `apps/chronogazer/core/src/bin/banto-serve.rs` の Hub の部分                               | 開発用の `MemoryKeyStore`                                                                                                                                                                                                 |
| `apps/chronogazer/src-tauri/src/lib.rs` の Hub のコマンド                                  | `hub_status`・`hub_connect`・`hub_subscription`・`hub_refresh_catalog`・`hub_catalog`・`hub_dismiss_legacy_selection`・`tags_register_from_hub`・`hub_adopt_manual_key`・`hub_disconnect`・`desktop_key_store` とその試験 |
| `apps/chronogazer/core/tests/hub_collect_roundtrip.rs`・`squat_scenarios.rs`               | Hub 経由の一巡と squat の試験                                                                                                                                                                                             |
| `apps/chronogazer/elev` の `secrets\hub-keys\` の ACL                                      | Hub のキーの置き場を作る手順                                                                                                                                                                                              |
| `apps/chronogazer/core/Cargo.toml`・`src-tauri/Cargo.toml` の依存                          | `banto-hub-bootstrap`・`banto-tagclient`・`banto-hub-tls-trust`・`banto-hub-squat-harness`（dev）。`banto-secret-store` は Hub のキーのために入れたもの（ほかの使い道は未確認）                                           |

**共有の crate の中の Hub 経由の部分**

- `crates/banto-tags`: 接続の種別 `hub`（`HUB_PROTOCOL`、`normalize_hub_input`、`validate_hub_tag_address`、外部名の長さ `MAX_HUB_EXTERNAL_NAME_LEN`）、migration `0019`（`hub` を許す・`idx_plc_connections_single_hub`、0025 で作り直し）と `0020`（外部名の一意のトリガ）。消すには新しい migration が要る（索引・トリガを落とし、残っている `hub` の接続を配下の収集グループ・記録項目ごと消す。§7 の 6）。
- `crates/banto-collect`: `hub` の接続を外す・読む分岐。**`ExternalSource` の仕組みそのものは残す**（Hub の取り込み口 `push` が使っている。`apps/banto-hub/core/src/ingest/mod.rs`）。
- `apps/banto-hub/core/src/rest.rs` の `reject_hub_connection_protocol`（Hub が `hub` 接続を断る口）。種別が消えれば要らない。

**ChronoGazer の画面と E2E**

- `apps/chronogazer/src/routes/(app)/settings/hub/`（`+page.svelte`・`+page.ts`）、`settings/HubSection.svelte`、`tags/HubImportDialog.svelte`。
- `apps/chronogazer/src/lib/banto/hubAdmin.ts`・`hubRegistration.ts` と試験、`PenTagPicker.svelte`・`penTagPicker.ts` の Hub のカタログの部分、`collectAdmin.ts`・`displayGroupsAdmin.ts`・`tagRegistryAdmin.ts`・`monitor/applyState.ts`・`monitor/valueColumn.ts`・`groups/groupsPageLogic.ts` の Hub の分岐。
- `apps/chronogazer/src/lib/demo/chronogazerRoutes.ts` の「`/api/hub*` は 501」の注記。
- `e2e/tests/user-settings-hub.spec.ts`・`user-tags-from-hub.spec.ts`・`user-display-groups-hub.spec.ts`・`user-simulator-monitor-hub.spec.ts`、`user-unsaved-changes.spec.ts` の Hub の部分。

**設定 DB の値**: 設定の KV の `hub.record`・`hub.installation_id`。段 5 の migration で消す（互換は保たない）。

**docs**: [recorder-requirements.md](recorder-requirements.md)（§3.1・§3.7.2 の #383 段階 3 の記述）、[chronogazer-service-design.md](chronogazer-service-design.md)（Hub のキーの置き場）、[chronogazer-ui-improvement-proposal.md](chronogazer-ui-improvement-proposal.md) §5、[banto-hub-client-bootstrap.md](../hub/banto-hub-client-bootstrap.md)、[hub-desktop-tls-design.md](../hub/hub-desktop-tls-design.md) のクライアント（ChronoGazer）の節、[tag-server-design.md](../hub/tag-server-design.md) §7 の「ChronoGazer の banto-hub 経由ドライバ」の行、[implementation-checklist.md](../implementation-checklist.md) の Hub 経由の行、[docs/README.md](../README.md) の索引。どの節を直すかは段 5 で `rg` で拾い直す。

**残すもの**（ChronoGazer のほかにも使い手がある）

- `crates/banto-tagclient`: Hub のデスクトップシェルの health probe（`apps/banto-hub/core/Cargo.toml` の `PinnedHubTls`、#644 PR4）と `apps/banto-hub-sink`（S5）が使う。SCADA の設計（[scada-design.md](../scada/scada-design.md)）も前提にしている。
- `crates/banto-hub-tls-trust`: Hub 本体（`banto-hub-core`・`src-tauri`）が使う。
- `crates/banto-hub-squat-harness`: Hub・Sink・tagclient の試験が使う。
- `crates/banto-hub-bootstrap`: ChronoGazer のほかの通常の依存は無い（`banto-secret-store` が KeyStore のアダプタのために依存）。§7 の 3 のとおり、段 5 の着手のときに SCADA で使う予定を確かめ、無ければ消す。

### 2.4 Hub の OPC UA サーバーの今

`banto-opcua-server`（tyaro/banto-opcua v0.3.3。2026-11-26 に v0.2.0 から上げた、#1332）を `apps/banto-hub/core/src/opcua.rs` で Hub に組み込んだ、**読み取り専用**のサーバー。設計書の位置づけは「診断用」（[tag-server-design.md](../hub/tag-server-design.md) §5.5）。2026-11-24 にコードを読んで確かめた（#1324 本文の表）。

| 項目         | 今                                                                                                                                                                                             | ChronoGazer で使うのに                                                                              |
| ------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------- |
| 有効・待受   | **既定は無効**。管理 API `PUT /api/opcua-settings`（admin）で有効にする。既定の待受は `127.0.0.1:4840/`、名前空間 `urn:banto:opcua:tags`（どちらも設定で変えられる。`advertised_host` もある） | ChronoGazer から使うには、Hub の管理者が OPC UA を有効にする                                        |
| アドレス空間 | `Objects/Banto/<接続>/<グループ>/<タグ>`、NodeId は `{接続}.{グループ}.{タグ}`（外部名）の文字列。ブラウズできる                                                                               | 足りる。ChronoGazer は `nsu=urn:banto:opcua:tags;s=<外部名>` で書く。改名で NodeId が変わる（§3.2） |
| 属性         | Description・EngineeringUnits・EURange は出していない                                                                                                                                          | 単位・説明・レンジを渡す（banto-opcua#55 の後、§6 の段 2）                                          |
| 型           | bit は Boolean、数値は全部 Double、**string は出さない**（`opcua.rs` の `data_type`）                                                                                                          | ChronoGazer は数値と bit だけを記録するので、string を出さないことは困らない。i64・u64 は §3.4      |
| 品質・時刻   | Good / Stale→UncertainLastUsableValue / Bad。SourceTimestamp は Good なら採取の時刻、Stale・Bad なら品質が変わった時刻                                                                         | 足りる。記録の時刻は §3.5                                                                           |
| 購読         | Subscription・MonitoredItem あり。Hub の側は 250 ms ごとに照合して、変わったときだけ通知                                                                                                       | 使える（§3.7）                                                                                      |
| HistoryRead  | 無い                                                                                                                                                                                           | 要らない（履歴は ChronoGazer が持つ）                                                               |
| セキュリティ | **SecurityPolicy None・Anonymous のエンドポイントだけを開く**（banto-opcua v0.3.3）。クライアントの証明書は `pki_dir` の信頼の一覧で判断する                                                   | 別の機械から使うには banto-opcua#54 が要る（§5）                                                    |

## 3. OPC UA のドライバの作り

#678 のドライバを、Hub 専用ではなく共有のドライバとして作る。ChronoGazer はそれを登録簿で有効にするだけにする。

### 3.1 置き場所

- **新しい crate `crates/banto-driver-opcua`**（内部 crate、`publish = false`）に `SessionDriver` の実装を置き、`crates/banto-drivers` の `builtin()` に `DriverEntry::session` で登録する。ドライバの id は `opcua`。
  - プロトコルの部品は banto-opcua の `banto-opcua-client`（ADR-0005）。async-opcua の型はドライバの外に出ない（部品の公開 API は core の語彙だけ）。
  - 依存は git の tag で固定する（`deny.toml` の `required-git-spec = "tag"`。`allow-git` に banto-opcua と async-opcua は入っている）。サーバーと同じ `async-opcua` のリビジョンにそろえる（tag-server-design §3.4.1 の 4）。
  - crates.io に公開する `banto-driver-api` は git の依存を持てないので、ドライバは公開しない。
- **WASM の対象外**（#700、plugin-design §6.1）。セキュアチャネル・セッション・購読の状態を持つので、組み込みのセッション型のまま残す。
- `banto-drivers` に入れると、Hub と ChronoGazer の両方の依存の木に `async-opcua`（クライアントの feature）が入る。ChronoGazer は今まで持っていなかった依存なので、ビルドの時間・バイナリの大きさ・`deny.toml` の例外（RUSTSEC-2023-0071 は 2027-01-15 まで、Phase 3 で見直す）が ChronoGazer にも掛かる。Cargo の feature でドライバを選べるようにするかは段 3 の着手のときに決める（§7 の 8。最初は入れない）。

### 3.2 アドレス（NodeId の文字列）

- タグのアドレスは NodeId の文字列。書き方は `ns=3;s=Line1.Speed`、`ns=2;i=1001`、`nsu=urn:…;s=…`、`nsu=urn:…;i=…`（ADR-0005 の「core に置くもの」）。解析は banto-opcua-core のものを使い、`parse_address` の中で呼ぶ。解析できなければ登録の時点で断る（今の SLMP・Modbus と同じ）。
- `nsu=` の URI の `;` は `%3B`、`%` は `%25` に符号化して書く（Part 6）。画面の入力の説明（`address_help`）にも書く。GUID（`g=`）・ByteString（`b=`）は当面「未対応の識別子の型」として断る。
- **`ns=` の番号はサーバーの再起動で変わることがある**ので、画面の説明では `nsu=` を勧める。Hub につなぐときは `nsu=urn:banto:opcua:tags;s=<外部名>`（Hub の名前空間の URI を変えていなければ）。
- `offset_address`（連続登録）は、NodeId に「隣」が無いので断る（エラーを返す）。数値の識別子 `i=` だけ番号を足す案もあるが、意味のある並びとは限らないので入れない。
- `probe`（接続試験で読む 1 点）は、どのサーバーにもある Server のノードを読む。候補は `ns=0;i=2259`（ServerStatus の State）。型が数値として受けられるかは着手のときに確かめる（未確認）。
- 改名・削除: Hub のタグの改名で NodeId（外部名）が変わる。古い NodeId は `BadNodeIdUnknown` になり、そのタグだけ Bad になる（ほかのタグの収集は続く）。今の Hub 経由のつなぎと同じく、ChronoGazer の記録項目のアドレスを利用者が直す。Browse が入れば選び直せる（§4.2）。

### 3.3 接続の設定

`settings_schema` に次を出す。汎用のフォームと MCP の `create_connection` がこれで描く・確かめる。

| キー               | 中身                                                                                                | 既定                 |
| ------------------ | --------------------------------------------------------------------------------------------------- | -------------------- |
| `endpointUrl`      | エンドポイント URL（`opc.tcp://host:port/path`）                                                    | 無し（必須）         |
| `readMode`         | 読み方。`read`（周期 Read）か `subscribe`（購読、§3.7）                                             | `read`               |
| `keepAliveMs` など | keep-alive の間隔と失敗の回数（ADR-0005: 静かな切断を見つけるため必ず設定する）                     | 部品の既定に合わせる |
| 購読の設定         | Publish の間隔・sampling の間隔・lifetime と keep-alive の回数（`readMode = subscribe` のときだけ） | 部品の既定に合わせる |

- 接続の時間切れと要求の時間切れは、host が `ConnectContext`（`connect_timeout`・`response_timeout`）で渡す。要求の時間切れは収集の周期より短くする（ADR-0005）。
- SecurityPolicy・利用者の認証は、banto-opcua#54 が入るまで設定に出さない（None / Anonymous に決め打ち）。**平文・匿名でつないでいることを接続の画面に出す**（#678 の受入れ条件。ChronoGazer も同じ）。
- キーの名前・既定の値は段 3 の着手のときに部品の `ClientConfig` に合わせて決める（上の表は形の案）。

### 3.4 型の写し

ChronoGazer の記録は `f64` と bit だけ（§2.2）。ChronoGazer で受ける型は次のとおり。

| OPC UA の型（読んだ値）                    | ChronoGazer の記録項目の型 | 受け方                                                                                                                              |
| ------------------------------------------ | -------------------------- | ----------------------------------------------------------------------------------------------------------------------------------- |
| Boolean                                    | `bit`                      | そのまま                                                                                                                            |
| SByte〜Int32・Byte〜UInt32・Float・Double  | 数値の型                   | `f64` に広げる                                                                                                                      |
| Int64・UInt64                              | 数値の型                   | `f64` に広げる。2^53 を超えると精度が落ちる（今の SLMP・Modbus の 64 bit と同じ。保存が `f64` なので ChronoGazer では避けられない） |
| String                                     | **選べない**               | 記録しない（§1.1 の決定）                                                                                                           |
| 配列・それ以外の型（DateTime・構造体など） | 選べない                   | 読めてもそのタグだけ Bad（ADR-0005: 部品が「対応しない値」にする）                                                                  |

- **string を選べなくする理由**（画面と REST・MCP のエラーに出す文言の元）: ChronoGazer の記録は数値の時系列（`f64` の列）で、トレンド・統計・帳票はどれも数値を前提にしている。文字の値を持つ場所が保存の形に無い。今の「Hub から追加」も文字列は登録しない（`hub_tags.rs` の規則）。
- **記録項目の型と読んだ値の型が違うとき**（#678 の「数値どうしなら変換して受け付ける」）: Hub は数値を全部 Double で出すので、記録項目を `i16` などで登録すると毎回変換が入る。**整数の型の記録項目に小数や範囲外の値が来たら、そのタグだけ Bad にする**（黙って丸めない・切り詰めない）。Hub につなぐときの既定の型は `f64` を案内する（§7 の 5）。
- string を選べなくするのは ChronoGazer の側の規則にする。Hub の #678 は string を扱うので、ドライバの `supported_kinds` は string を含めてよい。ChronoGazer の登録で数値と bit だけに絞る（今の `banto-tags` の登録の型の確かめに、ChronoGazer の規則を足す）。
- なお、`banto-driver-api` の string の型（`TagKind::Str { words, encoding }`）は PLC の語の並びを前提にした形で、OPC UA の String には合わない。Hub で string を扱うときの写し方は #678 の着手のときに決める（ChronoGazer には関係しない）。

### 3.5 品質と時刻

- **品質**: StatusCode の深刻度で写す（#678。`== Good` では比べない。あふれのビットなどが立つことがある、ADR-0006）。
  - Good → Good（記録する）
  - Uncertain → Stale（記録は `null`。今の Hub 経由の「stale は `null`」と同じ）
  - Bad → Bad（記録は `null`）
- **今の Driver API では Uncertain を返せない**（§2.1。`read_batch` の項目は値か `ItemError` だけ）。直し方は §3.6。
- **記録の時刻**（§7 の 1 で決めた）: **今と同じく周期の時刻（ホストの時計の `ptime_ms`）で記録し、SourceTimestamp は記録に使わない**。
  - 保存は「1 周期 = 1 行 = 1 つの時刻」なので（§2.2）、タグごとの SourceTimestamp を時刻として残す場所が無い。グループの中で時刻がばらばらになり、行の形を変えることになる。
  - SourceTimestamp は相手によって当てにならない。付けないサーバーがある（ADR-0005: 部品は現在時刻で補わない）、機器の時計がずれている（#1162 の PLC の時計）、購読ではサーバーの既定のフィルタ（値か StatusCode が変わったら通知）のため、値が変わらない間は最後に変わったときの時刻のまま。SourceTimestamp で鮮度を判定すると、変わらない定常値が古く見える。今の Hub 経由の H4（Hub の `t` で鮮度を判定しない）と同じく、鮮度の判定にも使わない。
  - なお Hub の OPC UA サーバーは、照合の鍵（`opcua.rs` の `ReadingKey`）に採取の時刻を含むので、周期 Read では値が同じでも Hub の採取ごとに SourceTimestamp が進む見込み（コードからの推定。段 4 で確かめる）。
  - SourceTimestamp は、ドライバから受け取って現在値の画面の診断（「機器の時刻」）に出す程度に留める（段 4 の任意）。
- #678（Hub）では「SourceTimestamp があれば採取の時刻に使い、無ければ受けた時刻」と決めている。Hub は値ごとに時刻を持てるので、Hub と ChronoGazer で使い方が違ってよい。

### 3.6 Driver API に足すもの

Uncertain（Stale）と SourceTimestamp をドライバから渡すために、`banto-driver-api` に口を足す。次の形にする（§7 の 2 で決めた）。

- `Session` に、品質と時刻つきで読む**既定の実装つきのメソッド**を足す（例: `read_batch_detailed`。既定は `read_batch` を呼んで Good・時刻無しに包む）。返す型は新しい `#[non_exhaustive]` の型（例: `Reading { value, quality, source_time_ms }`）。
  - trait に既定つきのメソッドを足すのは破壊的変更にならない（設計 §9.1 と同じ考え）。`Value` は変えない。
  - 使う側（`banto-collect` の broker の読み取り）は新しいメソッドを呼び、Uncertain を Stale として扱う。
- 別案: `ItemError` に「Uncertain」を足す。足すのは易しいが、値を持てない（UncertainLastUsableValue の値を捨てる）うえ、エラーとして数えられるので採らない。
- 書き込みの途中の切断（ADR-0005 の `WriteOutcome` の `Unknown`・`NotSent`）を返す形も Hub の #678 で要る。ChronoGazer は書かないので、この設計では扱わない。

### 3.7 再接続と読み方

- **再接続は broker に任せる**（ADR-0005 の既定。部品の自動再接続は止める）。接続が切れたら `read_batch` は `Err`（接続ごとの失敗）を返し、broker が新しい接続を張り直す（間隔は 1 秒から 30 秒、今の SLMP・Modbus と同じ）。張り直すたびに部品が NamespaceArray を読み直し、`nsu=` を番号に直す。
- **周期 Read**（`readMode = read`、既定）: 収集グループの周期ごとに `read_batch` が部品のまとめて Read を呼ぶ（`maxAge = 0`、`MaxNodesPerRead` で分割）。相手のサーバーに周期ごとの負荷が掛かる。
- **購読**（`readMode = subscribe`）: ADR-0006 のとおり、部品が購読で受けた最新値を持ち、`read_batch` は周期ごとにその最新値を取り出す。**記録の単位は周期のまま**（§1.1 の決定）。周期の間に何度変わっても記録は最後の値 1 つ。
  - 相手のサーバーの負荷は小さい（変わったときだけ届く）。Hub の OPC UA サーバーは 250 ms ごとに照合して通知するので、周期が 250 ms より短いと同じ値が続けて記録される（今の Hub 経由と同じ）。
  - 最初の通知が届く前は `BadWaitingForInitialData`（Bad）。購読を失った・作り直すまでは接続ごとの失敗。
  - async-opcua のサーバーは 1 つの購読に 1000 ノードまで（ADR-0006）。部品は購読を複数に分けない（後回し）ので、1 つの接続で 1000 本を超えると超えた分は `BadTooManyMonitoredItems`。ChronoGazer の収集グループは 1999 本まで登録できる（#1303）ので、**購読の接続は 1000 本までと画面で案内する**か、接続を分けてもらう。
- 読み方は接続ごと（ADR-0006）。速く変わるタグだけ購読、は接続を分けて行う。
- 最初の版（段 4）は周期 Read だけを出し、購読は段 4 の後半か次の PR で出す（#678 の「購読は後から」と同じ順）。Hub の OPC UA につなぐ場合は、購読が入ったら購読を勧める（§7 の 4）。

## 4. ChronoGazer での使い方

### 4.1 接続

- 接続の種類に OPC UA（`opcua`）が増える。登録簿に入れれば、`GET /api/drivers`・汎用のフォーム・MCP の `list_drivers`・`create_connection` にそのまま出る（§2.1）。画面に専用のコードはほぼ要らない。
- 接続の画面に「平文・匿名でつないでいます」を出す（§5）。汎用のフォームに、ドライバが注意書きを出す口があるかは未確認。無ければ schema の説明か、`opcua` のときだけの表示を足す。
- 接続試験は `probe`（§3.2）で読む。
- シミュレーションの接続（`simulation = true`）は、セッション型も `SimDevice` に差し替わる（§2.1）。OPC UA の接続でも使えるかは段 4 で確かめる。

### 4.2 記録項目の登録

- **段 4（最初の版）**: NodeId を手で入れる（#678）。今の「記録」ページの右ペイン・CSV の取り込み・MCP の `create_tag` がそのまま使える（アドレスの欄に NodeId）。型は数値と bit だけを選べる（§3.4）。単位・小数桁は利用者が入れる。
- **Hub から集める人の手間**: Hub の外部名を写して `nsu=urn:banto:opcua:tags;s=` を前に付ける。Hub の管理画面（`/tags`）から NodeId を写しやすくする（例: 行のメニューに「OPC UA の NodeId をコピー」）のは、Hub の側の小さな作業として段 2 に入れてよい（§7 の 10）。
- **後の段（Browse）**: 相手のサーバーをブラウズして選ぶ。単位・説明（banto-opcua#55 の後）・型を写して登録する。今の「Hub から追加」（`HubImportDialog.svelte`）の置き換えになる。Driver API に Browse の口が無いので、足し方（ドライバの任意のメソッドにするか、`opcua` だけの口にするか）はその段で決める。画面は画面改善の段階 1（左ツリー＝接続先の 3 ペイン）に合わせる（#678 の 2026-11-17 のコメント）。

### 4.3 表示グループのタグの選択

- 今はペンが `tagId`（登録済みの記録項目）か `hubTag`（Hub のカタログのタグ。未登録なら保存のときに `hubTagGroup` へ登録）を指す（`display_groups.rs`・`mcp.rs`）。
- **段 5 で `hubTag`・`hubTagGroup` を消し、ペンは `tagId` だけにする**。表示グループの右ペインのタグの選び方（`PenTagPicker.svelte`）は登録済みの記録項目からだけになる。OPC UA のタグは「記録」で先に登録してから選ぶ。
- Browse の段で「表示グループの画面から接続先をブラウズして、選んだものを記録項目に登録してペンにする」を入れるかは、その段で決める（今の `hubTag` と同じ使い勝手を戻すかどうか）。

### 4.4 REST・MCP・API キー

- 接続・記録項目の口は今の汎用のもの（`/api/plc-connections`・`/api/tags`、MCP の `create_connection`・`create_tag` など）をそのまま使う。OPC UA だけの口は足さない（Browse の段を除く）。
- 消す口は §2.3（`/api/hub*`・`/api/tags/from-hub`、MCP の `hubTag`・`hubTagGroup`）。OpenAPI（`rest/openapi.rs`）からも消す。
- ChronoGazer 自身の API キー（`read`・`config`、[chronogazer-agent-api-design.md](chronogazer-agent-api-design.md)）は変わらない。消えるのは Hub の API キーの保存（`key_store.rs`）。

## 5. セキュリティ

- **最初は SecurityPolicy None・Anonymous だけ**（#678、ADR-0005）。値は平文で流れ、相手のサーバーが本物かも確かめない。接続の画面に「平文・匿名」を出す。ChronoGazer は書かないので、改ざんされても機器へ書くことは無いが、偽のサーバーにつながると偽の値を記録する。
- Hub の OPC UA サーバーも None・Anonymous だけで、クライアントを選ばない。LAN へ出すと、同じ LAN の誰でも Hub の全部のタグを読める（読み取り専用なので書かれはしない）。
- **LAN の別の機械の Hub・他社のサーバーに安全につなぐには、banto-opcua#54**（SecurityPolicy・署名と暗号化・UserName・証明書の信頼の一覧をサーバーとクライアントに）が要る。入ったら、クライアントの証明書・相手のサーバーの証明書の信頼・資格情報の保存を、#1017 の証明書・Trust の管理と同じ考え方で Hub と ChronoGazer の両方に作る（#678 の 2026-11-17 のコメント）。資格情報（UserName のパスワード）は `secret_keys` にして、今の接続の秘密と同じく封印して DB に置く（`plc_connection_secrets`）。
- Phase 3 で暗号化を入れると RUSTSEC-2023-0071（RSA）にクライアントも当たる（ADR-0005 の帰結、`deny.toml` の例外の期限 2027-01-15）。
- **#54 までの推奨の構成**（§7 の 9）:
  1. **Hub につなぐのは同じ PC の Hub に限る**（`opc.tcp://127.0.0.1:4840/`。Hub の OPC UA の既定の待受のまま）。値は PC の外に出ない。今の Hub 経由のつなぎ（同じ PC は TLS の固定、#644）と比べると、同じ PC の別のプロセスが Hub の止まっている間に 4840 を先に取ると偽の値を記録しうる。秘密（API キー）を送らないので、#644 で守っていた「秘密が漏れる」は起きない。
  2. 別の機械の Hub・他社のサーバーへは、閉じた制御 LAN の中で、平文であることを利用者が受け入れたときだけ使う（画面の表示で知らせる）。ChronoGazer の側で宛先を 127.0.0.1 に限る仕組みは入れない（他社のサーバーにつなげなくなる）。
  3. Hub の OPC UA を LAN に出す（`bind` を変える）のは、#54 の後を推奨とする。Hub の管理画面にも平文で LAN に出していることを出す（Hub の側の作業、段 2）。

## 6. 段取り（PR の分け方）

実装は banto v8 の後。各段は小さな PR に分ける。

| 段  | 中身                                                                                                                                                                                                | 前提                                                      | 完了の条件                                                                                                                                                                                                                                                                                                                                            |
| --- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 1   | **banto-opcua の側**: #54（セキュリティ）・#55（属性）・#56（型）。提案は出してある。ChronoGazer の最初の版（None・Anonymous、同じ PC）は #54 を待たずに進められる                                  | なし                                                      | #55・#56 の入った版のタグが打たれる（#54 は段 4 の前提にしない）                                                                                                                                                                                                                                                                                      |
| 2   | **Hub の側**: 単位・説明・レンジを OPC UA へ渡す（#55 の版に上げる）。数値の型をそのまま出すか（#56）。LAN へ出すときの表示。外部名から NodeId を写す口（§4.2、任意）                               | 段 1 の #55（#56）                                        | Hub の OPC UA を読むと EngineeringUnits・Description・EURange が出る（UaExpert で確かめる）。Hub の OPC UA の試験が緑                                                                                                                                                                                                                                 |
| 3   | **共有のドライバ（#678）**: `banto-driver-api` に品質と時刻の口（§3.6）、`crates/banto-driver-opcua`、`banto-drivers` に登録。Hub で接続・NodeId の手入力・周期 Read・書き込み・平文の表示          | banto-opcua-client の採用できる版（v0.3.3 で済み。#1332） | #678 の受入れ条件（opc-plc と banto-opcua のサーバーを相手に、収集・書き込み・切断と再接続、存在しない NodeId はそのタグだけ Bad、分割 Write の途中切断の区別、cargo-deny と CI が緑）。適合試験（`banto-driver-conformance`）を通す                                                                                                                  |
| 4   | **ChronoGazer で使う**: 登録簿に入るので接続・記録項目はほぼそのまま。数値と bit だけに絞る規則、string の理由の文言、Uncertain→Stale、平文の表示、購読（後半または次の PR）                        | 段 3                                                      | 同じ PC の Hub の OPC UA から周期で記録でき、トレンド・履歴・日報に出る。Hub を止めると Bad、戻すと自動で戻る。string は選べず理由が出る。E2E（Hub の OPC UA か banto-opcua のサーバーを相手に）が緑。同じ Hub に UaExpert からもつなぎ、ChronoGazer の記録と値・品質が合う。Unified Automation のデモサーバー（UaAnsiCServer）からも周期で記録できる |
| 5   | **Hub 経由のつなぎを消す・docs を直す**: §2.3 の一覧。`hub` の接続の種別を消す migration、設定の KV の扱い、画面・E2E・docs（recorder-requirements の §7 の行、banto-hub-client-bootstrap.md など） | 段 4 がリリースに入る（オーナーの判断）                   | `rg -n 'hub_tags\|HubService\|from-hub\|hubTag' apps/chronogazer` が空。ChronoGazer の依存から `banto-hub-bootstrap`・`banto-tagclient`・`banto-hub-tls-trust` が消える（`cargo metadata` で確かめる）。CI が緑                                                                                                                                       |
| 6   | **Browse**（後の段）: 相手のサーバーをブラウズして記録項目を選ぶ。単位・説明を写す                                                                                                                  | 段 4、#55（単位を写すなら）                               | 「Hub から追加」と同じ使い勝手で、Hub・他社のサーバーのノードを選んで登録できる                                                                                                                                                                                                                                                                       |
| 7   | **セキュリティ**（#54 の後）: SecurityPolicy・UserName・証明書の信頼の管理を Hub と ChronoGazer に                                                                                                  | banto-opcua#54、#1017 の考え方                            | 暗号化した接続で LAN の Hub・他社のサーバーから記録できる                                                                                                                                                                                                                                                                                             |

- 段 5 は「互換を保たない」（#1324）。今の Hub 経由のつなぎはリリースしていないので、利用者への配慮（付け替えの口・作り直しの手順・起動の時の案内）は入れない（§7 の 6）。`hub` の接続は migration で配下の収集グループ・記録項目ごと消し、設定の KV の `hub.record`・`hub.installation_id` も消す。

## 7. 決めたこと（2026-11-24 オーナー決定）

11 項目とも推奨どおりに決めた。6 と 7 はオーナーの指示で次のとおり直した。

| #   | 項目                                                                            | 決定                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       |
| --- | ------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| 1   | 記録の時刻の出どころ（周期の時刻か SourceTimestamp か）                         | **周期の時刻（ホストの時計）**。保存が 1 周期 1 行で、タグごとの時刻を持てない。SourceTimestamp は付かない・機器の時計がずれる・購読では最後に変わったときの時刻のまま、と当てにならない。SourceTimestamp は診断の表示だけに使う（§3.5）                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                   |
| 2   | Uncertain と SourceTimestamp をドライバから渡す口                               | `Session` に既定の実装つきのメソッド（品質と時刻つきの読み取り）を足す。`ItemError` に足す案は採らない（§3.6）                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| 3   | banto-hub-bootstrap・banto-tagclient を、ChronoGazer が使わなくなった後に残すか | **banto-tagclient は残す**（Hub のシェルの health probe・Sink・SCADA の設計が使う）。**banto-hub-bootstrap は、段 5 の着手のときに SCADA（scada-design.md の Runtime PC 側の保管先の記述が banto-hub-bootstrap の方式を前提にしている）で使う予定があるかを確かめ、無ければ消す**。#1293〜#1295 は Sink・SCADA のために要るかで残すか閉じるかを決める                                                                                                                                                                                                                                                                                                                                                                                      |
| 4   | Hub の OPC UA につなぐときの既定の読み方                                        | 最初は周期 Read（#678 の順）。購読が入ったら、Hub につなぐときは購読を勧める（Hub の負荷が小さい）。1000 本の上限を画面で案内する                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                          |
| 5   | 整数の型の記録項目に小数・範囲外の値が来たとき                                  | そのタグだけ Bad（黙って丸めない）。Hub につなぐときの既定の型は `f64` を案内する                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                          |
| 6   | 段 5 で残っている `hub` の接続・記録項目の扱い                                  | **今の Hub 経由のつなぎはリリースしていないので、利用者への配慮はしない**（オーナーの指示）。migration で `hub` の接続を配下の収集グループ・記録項目ごと消し、設定の KV の `hub.*` も消す。付け替えの口・起動の時の案内・リリースノートの作り直しの手順は作らない                                                                                                                                                                                                                                                                                                                                                                                                                                                                          |
| 7   | 他社の OPC UA の相手を最初の対象にどこまで含めるか                              | 最初に確かめる相手は **Hub の OPC UA・banto-opcua のサーバー・opc-plc に、Unified Automation の UaExpert（クライアント）とデモサーバー **UaAnsiCServer**（ANSI C SDK のデモサーバー。オーナーの手元にある）を足す**（オーナーの指示）。デモサーバーは他社のサーバーとして ChronoGazer（OPC UA のドライバ）からつなぎ、NodeId の書き方（`ns=`・`nsu=`）・型の写し・品質・購読・切断と再接続を確かめる（段 3・段 4）。UaExpert は他社のクライアントとして Hub の OPC UA サーバーにつなぎ、ブラウズ・属性（単位・説明・レンジ）・型・品質・購読を確かめる（段 2・段 4 の実機確認）。実機の他社のサーバー（PLC の OPC UA ユニットなど）は多くが暗号化を求めるので #54 の後に回す。None で動く他社のゲートウェイがあれば、段 4 の実機確認に足す |
| 8   | ドライバを Cargo の feature で選べるようにするか                                | 最初は入れない（Hub と ChronoGazer の両方が使うので常に入れる）。ChronoGazer のビルドの時間・大きさが目立って増えたら考える                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| 9   | #54 までの推奨の構成                                                            | Hub は同じ PC（`127.0.0.1:4840`）に限るのを推奨。LAN は閉じた制御 LAN で平文を受け入れたときだけ。ChronoGazer の側で宛先を絞る仕組みは入れない（§5）                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       |
| 10  | Browse の段の時期と形                                                           | 段 4 の後、画面改善の段階 1（左ツリー＝接続先）と合わせて。Driver API に Browse の口を足すか、`opcua` だけの口にするかはその段で決める。それまでは Hub の管理画面から NodeId を写す口（段 2、任意）で手間を減らす                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                          |
| 11  | Hub の名前空間の URI の扱い                                                     | Hub の名前空間の URI は設定で変えられる（既定 `urn:banto:opcua:tags`）。ChronoGazer は `nsu=` で書くので、Hub の URI を変えると全部のタグが `BadNodeIdUnknown` になる。Hub の管理画面の URI の欄に、変えるとクライアントの NodeId が変わることを書く                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       |

## 8. 小さな現場の負荷の注意

- 小さな現場で Hub と ChronoGazer の両方が同じ PLC へ直に読みに行くと、PLC の接続の数と負荷を食う（SLMP・Modbus の同時接続の数には機種ごとの上限がある）。
- そういう現場では **「PLC は Hub だけが読み、ChronoGazer は Hub の OPC UA から読む」** を推奨の構成として案内する（#1324）。読み方は購読（§3.7）にすると Hub の負荷も小さい。
- Hub を置かない現場では、今までどおり ChronoGazer が SLMP・Modbus で直に読む。
- 案内の置き場所（ChronoGazer の README・利用者の手引き・接続の画面の説明）は段 4 で決める。
