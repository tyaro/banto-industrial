# banto-hub I/O の一本化 詳細設計（R2b-3）

作成日: 2026-10-22
状態: **草稿（オーナー判断待ち。§13 の Q1〜Q14）**。コード調査（§1）は 2026-10-22 の main（`708ec58a`、v0.2.0-alpha.35、R2b-2 完了の後）に対して行った。§0.1 に既存の決定・計画から外れる点をまとめた。2026-10-22 のオーナー決定「使用者無しの前提: 既存データの移行・旧い REST / MCP / 設定パッケージとの互換は考えない（R2b-7 以降も同じ）」と「版上げは段の最後でまとめて」に従う。
対象: [banto-hub-driver-refactor-plan.md](banto-hub-driver-refactor-plan.md) §4 の R2b-3「I/O を担う部分の一本化（Hub と ChronoGazer で共用）。トランスポートを抽象化し、TCP・シリアル（COM ポート直結）・変換器向けの TCP（生データ）を扱える形にする（最初の実装は TCP と COM 直結）。P0-3 のテストを全部通す」を実装できる粒度に落とす。オーナーの言い方では「broker と `PlcClient` の要求の型を `DriverAddress`（`TagSpec`）にし、間をつなぐ橋（HostBound の橋、`banto_plc::Address` への戻し）をなくす段」。
関連: [banto-driver-api-design.md](banto-driver-api-design.md)（R1。§4.2 の要求 - 応答の手順、§4.2.1 の書き込みの状態機械、§4.4 の `HostBound`、§6.2 の「R2b に残したもの」、§7 の適合テスト C9〜C11・C16、§8 のシリアル、§9 の版 0.2.0、§13 の 5）、[banto-hub-driver-address-design.md](banto-hub-driver-address-design.md)（R2b-2。§4.2「要求の型と HostBound の橋（Q3）」）、[banto-hub-connection-settings-design.md](banto-hub-connection-settings-design.md)（R2b-1。§3.5 の `$secret` を接続にだけ開封して渡す話）、[banto-hub-ingest-design.md](banto-hub-ingest-design.md)（`push` は `HostProvided`）、[implementation-checklist.md](implementation-checklist.md)。

---

## 0. 要約

| #   | 提案                                                                                                                                                                                                                                                                                                                                | 根拠の節 |
| --- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------- |
| 1   | R2b-3 は 1 本の PR にせず、**3a（要求の型の切り替えと橋の撤去）→ 3b（エンジン）→ 3c（Modbus を Frame へ）→ 3d（SLMP を Frame へ）→ 3e（シリアル）→ 3f（docs・版）** の PR 列にする。3a はさらに 3 本（ドライバ側・broker と Hub・収集と ChronoGazer）に分ける                                                                       | §12      |
| 2   | **最初に要求の型だけを替える**: Modbus / SLMP を `HostBound` から `DriverKind::Session` に移し、今の I/O（`banto-plc` の計画と実行）をドライバ crate の `Session` の中に閉じ込める。canonical → `banto_plc::Address` の戻しはドライバの中の private な処理になり、broker・`PlcClient`・Hub・`banto-collect` から `Address` が消える | §4       |
| 3   | broker の要求と応答の型を API の型（`TagSpec` / `WriteItem` → `Result<Value, ItemError>`、致命は `FatalError`）にし、ダイヤルは `DriverRegistry` の `Session` / `Frame` の項目から `Settings` と `ConnectContext` で行う。`DriverTable`・`DialFn`・`DriverDial`・`HostBound` を消す                                                 | §4.2     |
| 4   | 要求 - 応答の手順（api-design §4.2・§4.2.1）を実装する**エンジンを公開の新しい crate `banto-driver-engine`** に置き、適合テストのハーネスも同じエンジンを記憶上のトランスポートで動かす（手順の実装を 1 つにする）                                                                                                                  | §3, §5   |
| 5   | Modbus TCP と SLMP を `FrameDriver` にし、`banto-plc` / `banto-plc-write` の I/O（クライアント・実行・ダイヤル）を消す。計画・デコード・フレーム・シミュレータは残す。SLMP は 4E バイナリのフレームを `banto-driver-slmp` が自分で組む（`slmp` crate の `SLMPClient` はフレームと I/O が一体で、フレームだけ取り出す口が無い）      | §6, §1.7 |
| 6   | トランスポートは `Transport` trait（送る・受ける・受信の残りを捨てる・閉じる）。実装は TCP と COM 直結。変換器向けの TCP（生データ）は TCP の実装をそのまま使い、フレームの規則はドライバが決める。最初のシリアルのドライバは Modbus RTU（Q8）                                                                                      | §7       |
| 7   | ChronoGazer の収集も同じ broker とエンジンを通す（`default_client_factory`・`ClientSpec`・`ClientProtocol` を消す）。保存時のアドレスの規則は R2b-7 まで旧規則のまま                                                                                                                                                                | §8       |
| 8   | 書き込みの安全（関門の順・監査の先書き・1 ジョブ = 1 回の論理書き込み・割り込ませない）は変えない。RMW の判断（`Address::Slmp { bit: Some(_) }` を見る Hub の分岐）はドライバの `write_next` に移す                                                                                                                                 | §9       |
| 9   | `banto-driver-api` は 0.2.0（`DriverKind::HostBound` と `DriverEntry::host_bound` の削除が破壊）。追加（既定つきの `probe`・設定キー）は破壊ではない。CHANGELOG に「何が壊れ、どう直すか」を書く                                                                                                                                    | §10      |
| 10  | 完了条件: P0-3 の 6 本と Hub の `modbus_collection_uses_exactly_one_socket` を新しい居場所（エンジンの上）に移して通す。適合テストは `session::run_all_against`（実際の TCP・シミュレータ）と `frame::run_all`（記憶上）の両方を Modbus / SLMP で通し、C9〜C11・C16 を足す                                                          | §11      |

### 0.1 既存の決定・計画から外れる点

| 既存の決定・計画                                                                                                                                                      | 本書の提案                                                                                        | 理由                                                                                                                                                                                                                                                                                                      |
| --------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| api-design §4.4・`registry.rs:39-42`「`HostBound` は R2b-3 で `Frame` へ載せ替えて消す」                                                                              | いったん **`Session` へ載せ替えて消し**（3a）、その後 `Frame` へ移す（3c・3d）                    | `Frame` に移すにはエンジン（3b）が要る。`Session` なら今の I/O をそのまま包めるので、要求の型の切り替え（オーナーが最初に挙げた部分）をエンジンと切り離して先に入れられる。包む処理は今の dev-only の `LegacySession`（`crates/banto-driver-slmp/tests/common/mod.rs:155`）とほぼ同じで、捨てる量が小さい |
| api-design §4.4 の表の B「セッション型で包むのは R2b-2 と同時」・address 設計 Q3「broker・`PlcClient` は R2b-3 まで `Address` のまま」                                | R2b-3 の最初（3a）で行う                                                                          | address 設計の決定どおりの時期。二度書きは 3a の `Session` の包み（`LegacySession` の昇格）だけ                                                                                                                                                                                                           |
| api-design §2.2「適合テスト一式は Hub なしで走らせられる」・§7.2 のハーネスの手順は conformance の中（`crates/banto-driver-conformance/src/frame/engine.rs`、838 行） | 本番のエンジンを公開の crate `banto-driver-engine` に置き、ハーネスはそれを使う（Q3）             | 同じ手順（分割受信・時間切れで捨てる・`Discard`・書き込みの上限）を 2 か所で実装すると食い違う。エンジンを `banto-tags` などの git 依存から切り離した crate にすれば、公開の conformance から依存できる                                                                                                   |
| api-design §6 の表 A「SLMP はフレームだけ取り出すには `tyaro/slmp` 側の改造が要る」                                                                                   | `banto-driver-slmp` が 4E バイナリのフレームを自分で組み、`slmp` crate の I/O を使わない（Q4）    | 使っているのは一括読み出し（Bool / U16）と一括書き込みだけ（§1.7）。改造より小さく、ドライバ crate から git 依存が消える（crates.io に出せる、refactor-plan §7 Q2・Q3）                                                                                                                                   |
| api-design §0.1 M7・§6.4「ChronoGazer の `EngineConfig` への注入は R2b」、address 設計 Q7「ChronoGazer の追従は R2b-7」                                               | 収集の I/O だけ R2b-3 で寄せる（Q5）。保存時の規則（`check_tag_address`、綴りのまま保存）は R2b-7 | 「Hub と ChronoGazer で共用」が R2b-3 の定義。ChronoGazer だけ旧い I/O を残すと、`banto-plc` のクライアントと `Address` の要求の型を消せない                                                                                                                                                              |
| refactor-plan §7 Q4「エンジンはポートごとに 1 つの直列化キューを持つ（マルチドロップ）」                                                                              | R2b-3 は 1 つの COM ポート = 1 つの接続（保存で断る）。ポートの共有は形だけ用意し、実装は後（Q9） | 時間切れ・切断の扱いがポートをまたぐ（1 局の時間切れで他局の接続を切るか）など、決めることが多い。最初のシリアルの機器で 1 対 1 を確かめてから足す                                                                                                                                                        |

## 1. 前提（コードの現状、2026-10-22 の main `708ec58a` に照合）

行番号は目安。各 PR は着手時に `rg -n 'Address::|BatchReadRequest|BatchWriteRequest|PlcClient|DriverTable|HostBound|default_client_factory' --type rust apps crates` で再確認する。

### 1.1 要求の型と橋（R2b-2 の後）

```text
保存: text ──parse_address(kind)──▶ canonical（DB）
Hub の計画・即時読み書き: canonical ──parse_address──▶ DriverAddress ──橋──▶ banto_plc::Address ──▶ ReadRequest / BatchReadRequest / BatchWriteRequest
broker: Job { Vec<BatchReadRequest> | Vec<BatchWriteRequest> } ──▶ BrokerSession（ドライバ crate）──▶ banto_plc / banto_plc_write の計画と実行 ──▶ ソケット
ChronoGazer の収集: text ──Address::parse*──▶ ReadRequest ──▶ PlcClient（ModbusTcpClient / SlmpClient が自分のソケットを持つ）
```

| 何                                | 場所（本番）                                                                                                                                                                                                | 事実                                                                                                                                                                         |
| --------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 橋 `Address::from_driver_address` | `crates/banto-plc/src/address.rs:147`。呼ぶのは `crates/banto-collect/src/config.rs:1812`（`build_request_with_registry :1795`）と `apps/banto-hub/core/src/read_path.rs:273`（`resolve_plc_address :253`） | プロトコル名を知る最後の場所（address 設計 §4.2）                                                                                                                            |
| Hub の即時読み取り                | `read_path.rs:288` `build_plc_read_request` → `BatchReadRequest`（`:302`・`:319`）→ `handle.read`（`:198`）                                                                                                 | 外側の時間の上限は無い。セッションの応答の時間切れに頼る                                                                                                                     |
| Hub の書き込み                    | `write_path.rs:1613` `build_plc_write_request`、`:1651` で橋、`:1685` の `matches!(address, Address::Slmp { bit: Some(_), .. })` で `BitInWord`（RMW）か `Numeric` かを選ぶ、文字列は `:1781`               | **RMW を選ぶ判断が Hub にある**（プロトコルの知識）。単位の逆変換・範囲の検査（`:1624-1645`）と文字列の符号化の検査（`:1799`）も Hub                                         |
| broker の要求                     | `crates/banto-broker/src/lib.rs:378-385` `Job { Read(Vec<BatchReadRequest>) \| Write(Vec<BatchWriteRequest>) }`、`BrokerHandle::read :439`・`write :460`、`ReadOnlyHandle :499-520`                         | 1 接続 = 1 タスク = 1 セッション。ジョブを 1 つずつ最後まで `await` する（`run_broker_task :1154-1286`）。直列化は構造で守られている                                         |
| broker のセッションの trait       | `crates/banto-broker/src/session.rs:78-100` `BrokerSession`、`SessionError(String) :110`、`Connector :135-137`                                                                                              | `Err` = 接続致命。要求は `banto_plc` / `banto_plc_write` の型                                                                                                                |
| broker の表                       | `lib.rs:697-701` `DriverDial { connector, slmp_cpu }`、`:708` `DialFn = fn(&PlcConnection, u16)`、`:730-769` `DriverTable`、`:821-857` `ensure_connection`（`conn.port` を `u16` に）                       | ダイヤルの材料は `PlcConnection` の写しの列（`host`・`port`・`unit_id`・`word_order`・`cpu_type`。`crates/banto-tags/src/plc_connection.rs:851-890`）で、`Settings` ではない |
| `slmp_cpu`                        | `lib.rs:423`・`:429`、`DriverDial` 経由。読むのは `apps/banto-hub/core/src/rest/plc_connections.rs:1094`（接続テストが共有セッションを使ってよいかの照合 `slmp_session_cpu_refusal :1054`）                 | SLMP 固有の値が broker の公開の型に載っている                                                                                                                                |
| ドライバの登録                    | `crates/banto-drivers/src/lib.rs:64-80` `builtin()`（`registry` に `host_bound`、`broker = broker_table() :84-95`、`client_factory = default_client_factory()`）                                            | Modbus / SLMP は `DriverKind::HostBound`（`crates/banto-driver-api/src/registry.rs:42`、`DriverEntry::host_bound :103-108`）                                                 |
| 収集の要求                        | `crates/banto-collect/src/config.rs:209-221` `GroupPlan.requests: Vec<ReadRequest>`、`:110-114` `ProtocolConfig { ModbusTcp, Slmp, External }`、`:83-86` `Protocol`                                         | Hub は `build_config_from_with_registry`（`:677`）、ChronoGazer はレジストリ無しの `build_request`（`:1742`、`Address::parse` / `parse_slmp` `:1755-1756`）                  |
| 収集の差し込み口                  | `crates/banto-collect/src/task.rs:248` `ClientProtocol`、`:271` `ClientSpec`、`:341` `ClientFactory`、`:346` `client_spec`、`:385-409` `default_client_factory`、`:580` `run_connection`                    | `ClientSpec` は手で保守する写しで、#334（ワード順）・#605（CPU）で 2 回落とした（`task.rs:324-341` の doc）                                                                  |
| Hub の収集                        | `apps/banto-hub/core/src/broker_glue.rs:954-985` `hub_client_factory`（broker の読み取りハンドルを `BrokerReadClient :315` で `PlcClient` に包む）                                                          | #337 の 1 接続 = 1 ソケット。`BrokerReadClient::connect`（`:362-436`）は broker の状態を見るだけでダイヤルしない（#344）                                                     |

### 1.2 プロトコルを名前で選ぶ残り（Hub）

| 場所                                                                                                                                     | 何                                                                                                   | R2b-3 で                                                                             |
| ---------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------ |
| `rest/plc_connections.rs:902` `test_modbus_connection`・`:1082` `test_slmp_connection`、固定のリテラル `:909`・`:1022`・`:1098`・`:1202` | 接続テスト。収集中は共有セッションで、そうでなければ `ModbusTcpClient` / `SlmpClient` を直接ダイヤル | 汎用化（Q11）。`registry_entrances.rs:52-65` の例外 4 件（「R2b-3 で消える」）を外す |
| `broker_glue.rs:835-838`（`conn.protocol == "modbus-tcp"` でシミュレータの種類）、`:857`（`"slmp"` だけ CPU を同一性に）                 | シミュレーション                                                                                     | シミュレータの種類は R2b-5 まで残す。同一性は設定の指紋に（§4.4）                    |
| `crates/banto-collect/src/collector.rs:891-900`・`simulation.rs:198-199`                                                                 | ChronoGazer のシミュレーション（`Protocol` で選ぶ）                                                  | R2b-5 まで残す（`Protocol` はシミュレーションのためだけに残る）                      |

### 1.3 ドライバ crate と `banto-plc` の I/O

| 何                                      | 場所                                                                                                                                                                                                                                                                                                               | 使う側                                                                                                                                                           |
| --------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| SLMP のセッション                       | `crates/banto-driver-slmp/src/session.rs:34-67` `SlmpSession { client: slmp::SLMPClient, word_order }`、`dial :69`・`slmp_config_for :91`（`lib.rs`）                                                                                                                                                              | broker（`DriverTable`）                                                                                                                                          |
| Modbus のセッション                     | `crates/banto-driver-modbus/src/session.rs:42-79` `ModbusSession { stream, unit_id, response_timeout, word_order, next_transaction_id }`、`write_batch` で `String` / `BitInWord` を `UnsupportedRequestKind`（`:122-150`）                                                                                        | 同上。**読みと書きでトランザクション ID の数え手を 1 つにする**のが一番大事な性質（`:48-78`）                                                                    |
| `PlcClient`                             | `crates/banto-plc/src/client.rs:56-79`。実装は `ModbusTcpClient`（`modbus/mod.rs:339`）・`SlmpClient`（`slmp/mod.rs:695`）・Hub の `BrokerReadClient`・`NoDriverClient`（`broker_glue.rs:271`）と各テストの偽物                                                                                                    | 本番の直接のソケットは ChronoGazer の収集（`apps/chronogazer/core/src/collect.rs:1921`・`:2575`）、Hub の接続テスト、Hub の fallback（`broker_glue.rs:958`）だけ |
| 実行とダイヤル                          | `banto-plc`: `dial_modbus`（`modbus/mod.rs:209`）・`execute_modbus_reads`（`:252`）・`dial_slmp`（`slmp/mod.rs:388`）・`execute_slmp_batch_reads`（`:608`）。`banto-plc-write`: `execute_modbus_writes`（`modbus/mod.rs:289`）・`execute_slmp_writes`（`slmp/mod.rs:239`）・`execute_one_bit_write`（`:354`、RMW） | ドライバ crate のセッション                                                                                                                                      |
| 書き込みのクライアント `PlcWriteClient` | `crates/banto-plc-write/src/client.rs:46-78`、`SlmpWriteClient`・`ModbusWriteClient`                                                                                                                                                                                                                               | **本番では使われていない**（テストと `crates/banto-drivers/examples/real_plc_verify.rs` だけ）                                                                   |
| フレームの部品                          | Modbus: `banto-plc/src/modbus/frame.rs`（`build_request_frame :107`・`wrap_mbap :185`・`parse_mbap_header :210`・`parse_response_pdu :246`、MBAP は 7 バイト）。SLMP は `slmp` crate の中（§1.7）                                                                                                                  | Modbus はフレームが既に関数で分かれている                                                                                                                        |
| 文字列の decode                         | `banto-plc/src/decode.rs:137` `decode_string_value`（`pub(crate)`）。符号化は `banto-plc-write/src/encode.rs:298` `encode_string_value`                                                                                                                                                                            | 読み取りの decode はドライバの層（SLMP の実行の中）。api-design §13 の 5 で「エンジン側へ揃えるのは R2b-3」                                                      |
| シミュレータ                            | `banto-plc/src/modbus/simulator.rs`（分割送信 `set_response_chunking :346`、遅延 `hold_next_response :358`、ID 違い `set_wrong_transaction_id :366`、`connection_count :381`）、`slmp/simulator.rs`（`:340`）、`banto-plc-write/src/slmp/simulator.rs`                                                             | P0-3 のテスト、Hub と ChronoGazer のシミュレーション（`banto-collect` が feature `simulator` で本番でも使う、`crates/banto-collect/Cargo.toml:31`）              |

### 1.4 `banto-driver-api` と適合テスト

- `DriverKind { Frame, Session, HostBound, HostProvided }`（`registry.rs:31-62`、`#[non_exhaustive]`）。`FrameConnection`（`frame.rs:237-285`: `handshake_next`・`farewell`・`is_complete`・`plan_read`・`decode_read`・`write_next`）と `Session`（`session.rs:73-98`）は R1-2 で定義済みで、本番で動かすホストはまだ無い。
- 適合テストのフレーム型のハーネスは **自前のエンジン**（`crates/banto-driver-conformance/src/frame/engine.rs`: 記憶上の線路・仮想の時刻・分割 / 遅延 / 伸び続ける応答 / 先頭のごみ / 途中の切断を起こす。違反を `Failure` に集める）を持ち、参照実装 `EchoFrameDriver`（`fake/echo.rs`、1211 行）で C3〜C7・C9〜C12 を通している。
- Modbus / SLMP の適合テストは dev-only の `LegacySession`（`crates/banto-driver-{modbus,slmp}/tests/common/mod.rs:152`・`:155`。`BrokerSession` を `api::Session` に包み、canonical を `Address::parse*` で戻し、`classify_read` / `classify_write` でエラーを写す。**致命のエラーは `SessionError(String)` の文字列から元の variant を復元している**）で `session::run_all_against` を走らせる（C1〜C8・C12、SLMP は C13〜C15）。
- **`banto-driver-api` はまだ 1 度も公開していない**: `crates/banto-driver-api/CHANGELOG.md:8` は「0.1.0 — 未公開」、タグ `banto-driver-api-v*` は無い（`git tag -l`）。CI の `cargo-semver-checks` はタグが無いので飛んでいる（`.github/workflows/ci.yml:631-646`）。api-design Q3 の「R1 完了直後に 0.1.0 を公開」は実施されていない。

### 1.5 P0-3 の 6 本の今の層

| #   | テスト（ファイル:行）                                                                                                                                                                                   | 掛かっている層                                                              |
| --- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------- |
| 1   | `a_response_split_across_segments_is_read_as_one_response`（`crates/banto-plc/src/modbus/integration_tests.rs:323`）                                                                                    | `ModbusTcpClient`（`PlcClient`）+ Modbus シミュレータの分割送信             |
| 2a  | `a_late_response_after_a_timeout_is_never_returned_as_a_later_value`（同 `:376`）／`a_late_response_on_a_reused_stream_is_a_fatal_protocol_error`（同 `:408`、`execute_modbus_reads` を同じ stream で） | `ModbusTcpClient` と `execute_modbus_reads`                                 |
| 2b  | `a_response_with_the_wrong_transaction_id_is_fatal`（同 `:458`）                                                                                                                                        | 同上                                                                        |
| 3   | `connect_to_a_closed_port_fails_without_hanging`（同 `:485`。SLMP にも同名 `slmp/integration_tests.rs:624`）                                                                                            | `connect`                                                                   |
| 4   | `read_batch_mixed_reads_numerics_and_keeps_strings_bad`（同 `:520`）                                                                                                                                    | `plan_batch_requests` + `execute_modbus_reads`                              |
| 5   | `collection_over_several_periods_uses_exactly_one_socket`（`crates/banto-collect/tests/integration.rs:1598`）                                                                                           | `banto-collect` の task + `default_client_factory`（直接のソケット）        |
| 6   | `a_response_split_across_segments_is_read_as_one_response`（`crates/banto-plc/src/slmp/integration_tests.rs:1163`）                                                                                     | `SlmpClient` + SLMP シミュレータの分割送信（`slmp` v0.2.1 の `read_exact`） |
| -   | Hub 経由の `modbus_collection_uses_exactly_one_socket`（`apps/banto-hub/host/tests/integration.rs:1159`）                                                                                               | Hub の収集 → broker                                                         |

どれも **消す I/O の API（`PlcClient`・`execute_*`）を直接呼んでいる**ので、「そのまま通す」ことはできない。シナリオ（シミュレータの設定と期待値）を保ったまま、新しい I/O の上に書き直す（§11.1）。

### 1.6 ChronoGazer

- 収集は `banto-collect` の task が `default_client_factory` で直接のソケットを持つ（`apps/chronogazer/core/src/collect.rs:1921`・`:2575`、テスト `collect/history/tests.rs:604`）。broker は使わない。書き込みは無い（`banto_plc_write` への依存なし）。
- `chronogazer-core` は `banto-plc`・`banto-collect`・`banto-tags` に依存し、`banto-broker`・`banto-drivers`・`banto-driver-api` には依存しない（`apps/chronogazer/core/Cargo.toml:31-34`）。`EngineConfig`（`apps/chronogazer/core/src/engine.rs:260`）にドライバの注入口は無い。bin `banto-serve` は `chronogazer-core` の package の中（`Cargo.toml:137-138`）。
- シミュレーションは `banto-collect` の `Collector` がプランのアドレスを差し替える（`collector.rs:423`・`:632`）。

### 1.7 `tyaro/slmp`（v0.2.1、`40a2f1d`）の中身

`~/.cargo/git/checkouts/slmp-a7b07d91a5a0438e/40a2f1d/src/` を読んだ:

- `SLMPClient`（`lib.rs:191-197`）は `Arc<Mutex<Option<TcpStream>>>` と受信バッファを持ち、要求の組み立て・送信・受信・検証を private の `request_response`（`:245-286`）1 つでする。サブヘッダの組み立て `create_subheader`（`:165`）は private、コマンドの組み立ては `pub(crate) mod commands`（`commands/mod.rs:15-16`）の中で、外からは呼べない。応答の解釈（例: `bulk_read` のビットの展開 `:513-545`）も各メソッドの中。**公開の API からはフレームだけを取り出せない**。
- 検証（`validate_response :288-327`）は応答コード・**固定の `serial_id`**・経路の欄を見る。要求ごとの番号は無い。接続の時間切れ `CONNECT_TIMEOUT` は 1 秒の定数（`:28`、`connect :236`）。
- banto が使うのは `bulk_read`（`Bool` と `U16`。`banto-plc/src/slmp/mod.rs:495`・`:498`）と `bulk_write`（`banto-plc-write/src/slmp/mod.rs:263`・`:381`、RMW の今の値の読み出しは `:324` の `bulk_read`）だけ。コマンドは一括読み出し 0x0401・一括書き込み 0x1401 と、Q / R のサブコマンドの違い（#605）。
- banto 側のシミュレータ（`banto-plc/src/slmp/simulator.rs`）は同じフレームの受け側（`parse_bulk_request :462`・`frame :631`）を既に持つ。

### 1.8 シリアル

- `serialport`・`tokio-serial`・`mio-serial` は `Cargo.lock` に無い。`deny.toml` の `[licenses].allow` に `MPL-2.0` はある（`serialport` は MPL-2.0 と記憶しているが、**未確認**）。
- 予約の設定キー `keys::TRANSPORT`・`SERIAL_PORT`・`STATION`（`crates/banto-driver-api/src/settings.rs:39` 付近）、`Transport { Tcp, Serial, SerialOverTcp }`（`driver.rs:153`）は R1 で用意済み。Modbus / SLMP の `validate_config` は `transport` が `tcp` 以外なら断る（`banto-driver-modbus/src/base.rs:327`、`banto-driver-slmp/src/base.rs:352`）。
- 接続のフォーム（`ConnectionDrawer.svelte` ほか）はプロトコルごとの手書きで、新しいドライバの設定欄は R2b-6（汎用フォーム）まで出ない。

## 2. 範囲: R2b-3 で変えるもの・残すもの

| 面                                                                                 | R2b-3                                                                      | 後の段                                         |
| ---------------------------------------------------------------------------------- | -------------------------------------------------------------------------- | ---------------------------------------------- |
| broker・収集・Hub の要求の型                                                       | `TagSpec` / `WriteItem` と `Value` / `ItemError` / `FatalError`（3a）      | -                                              |
| `banto_plc::Address` への橋（`from_driver_address`・`resolve_plc_address` の戻り） | 消す（3a）。ドライバの中の private な戻しは 3c / 3d で計画ごと消える       | -                                              |
| `DriverKind::HostBound`・`DriverTable`・`DialFn`・`DriverDial`                     | 消す（3a）                                                                 | -                                              |
| エンジン（Frame の要求 - 応答・`write_next`）                                      | 作る（3b）                                                                 | -                                              |
| Modbus TCP・SLMP                                                                   | `FrameDriver` に（3c・3d）                                                 | -                                              |
| `banto-plc` / `banto-plc-write` の I/O                                             | 消す（3c・3d）。計画・デコード・フレーム・シミュレータは残す（Q7）         | 実装本体のドライバ crate への移動（Q7 の (b)） |
| トランスポート                                                                     | TCP・COM 直結（3b・3e）。変換器向けの TCP（生データ）は TCP をそのまま使う | マルチドロップのポートの共有（Q9）             |
| シリアルのドライバ                                                                 | Modbus RTU（Q8）                                                           | 他のシリアルのドライバ                         |
| ChronoGazer                                                                        | 収集の I/O を同じ broker とエンジンへ（Q5）                                | 保存時の規則・`ALLOWED_PROTOCOLS`（R2b-7）     |
| 接続テスト                                                                         | ドライバに依らない形に（Q11）                                              | -                                              |
| シミュレータの選び方（`"modbus-tcp"` の分岐）                                      | 変えない                                                                   | R2b-5                                          |
| 接続ごとに実装を選ぶ                                                               | 変えない（1 つの id に 1 つの実装）                                        | R2b-4                                          |
| 画面（シリアルの設定欄）                                                           | 足さない（Q10）                                                            | R2b-6                                          |
| REST・MCP・設定パッケージの形                                                      | 変えない（接続の `settings` に新しいキーが増えるだけ）                     | R2b-7                                          |

## 3. crate の構成（R2b-3 の後）

```
banto-driver-api（公開、依存ゼロ）
   ▲
banto-driver-engine（公開・新規。tokio・encoding_rs・シリアルの crate。git 依存なし）
   ▲            ▲
   │            └── banto-driver-conformance（公開。ハーネスがエンジンを記憶上のトランスポートで動かす）
   │
banto-broker（接続ごとのタスク・再接続・ジョブの直列化・状態の watch。エンジンを使う）
   ▲
banto-collect（収集の計画と task。読むのは broker の読み取りハンドルだけ）── chronogazer-core / banto-hub-core
banto-driver-modbus / -slmp（FrameDriver。banto-plc / banto-plc-write の計画・デコード・フレームを使う）
   ▲
banto-drivers（組み立て役: registry だけを返す）── banto-hub-host / -shell / ChronoGazer の bin
```

- **`banto-driver-engine`**（Q3）: `Transport` trait と TCP・シリアルの実装、`FrameSession`（`FrameDriver` + `Transport` を `api::Session` にする本体。§5 の手順）、`dial(entry, settings, cx) -> Box<dyn Session>`（`Frame` ならトランスポートを開いてハンドシェイク、`Session` ならドライバの `connect`）、文字列の decode（`RawValue::Bytes` → `Value::Str`。今の `banto-plc/src/decode.rs:137` を移す）。`banto-tags`・`banto-plc` には依存しない（公開するため。api-design §1.3）。
- **`banto-broker`**: 今の job ループ・再接続・状態の watch・`SessionDirectory` はそのまま。`Connector` の中身が「`DriverDial` の closure」から「`banto_driver_engine::dial`」に替わる。`banto-plc` / `banto-plc-write` への依存は消える（要求の型が API の型になるため）。
- **`banto-drivers`**: `Builtin { registry }` だけになる（`broker` と `client_factory` が消える）。
- **`banto-plc` / `banto-plc-write`**: I/O を消した後は「プロトコルの部品」（アドレスの文法・計画・デコード・符号化・フレーム・シミュレータ）。どちらも `slmp` の git 依存を持つ（`banto-plc/src/slmp/mod.rs:126`・`:151` の `to_wire` で `slmp::CPU`・`DeviceType` を使う）ので、3d で SLMP のフレームを自前にしたら、`slmp` への依存はデバイスの符号（`DeviceType` の wire code）だけになるか、無くせる（§6.2）。

## 4. 要求の型の切り替えと橋の撤去（3a）

### 4.1 Modbus / SLMP を `Session` へ（3a-1）

- `banto-driver-modbus` / `-slmp` に `SessionDriver` を実装する。`connect(settings, cx)` は `settings`（`host`・`port`・`unit_id`・`word_order`・`cpu_type`）から今の `ModbusTcpConfig` / `SlmpConfig` を組み、今のダイヤル（`dial_modbus` / `dial_slmp`）で接続する。タイムアウトは `cx.connect_timeout` / `cx.response_timeout`。
- `Session::read_batch(&[TagSpec])` は、各 `TagSpec.address.canonical` をドライバ自身の文法（`Address::parse` / `parse_slmp`）で戻して今の `BatchReadRequest` を組み、今の `ModbusSession` / `SlmpSession` の処理に渡す。結果は `classify_read`（`classify.rs`、R1-4b）で `ItemError` に写す。**戻しはドライバの中の private な関数**で、`banto_plc::Address::from_driver_address`（公開）は消す。
- `write_batch(&[WriteItem])` は、`spec.address.bit.is_some()` かつ `TagKind::Numeric(Bit)` なら RMW（SLMP の `BitInWord`）、Modbus はそれを `ItemError::Unsupported`（今の `UnsupportedRequestKind` と同じ意味）にする。今の Hub の `matches!(address, Address::Slmp { bit: Some(_), .. })`（`write_path.rs:1685`）の判断がここへ移る。
- 致命は `FatalError` の型で返す（`SessionError(String)` の文字列からの復元、`LegacySession` の `plc_error_from` をやめる）。
- 中身は `LegacySession`（`tests/common/mod.rs`）を本番の型に昇格したもの。適合テストは本番のセッションに掛け替え、`LegacySession` を消す。
- この PR では登録を変えない（`HostBound` のまま）。broker も今のまま。

### 4.2 broker と Hub（3a-2）

- broker の `Job` を `Read { specs: Vec<TagSpec>, .. }`・`Write { items: Vec<WriteItem>, .. }`、応答を `Vec<Result<Value, ItemError>>` / `Vec<Result<(), ItemError>>` にする。`BrokerSession` は `api::Session` に置き換えて消す（`session.rs` は `Connector` だけになる）。`SessionError` は `FatalError`。
- `SessionDirectory::ensure_connection` は `DriverTable` の代わりに `Arc<DriverRegistry>` を持ち、`registry.get(protocol)` の `DriverKind::Session`（3b 以降は `Frame` も）でダイヤルする。`HostProvided` と未登録は今の `BrokerError::UnsupportedProtocol`。`is_broker_managed` は「`Frame` か `Session` の項目がある」になる。
- ダイヤルの材料は `PlcConnection` の列ではなく **`banto_tags::settings_to_api(&conn.settings)`**（R2b-1 の純関数、connection-settings 設計 §3.5）と `ConnectContext`。`conn.port` の `u16` の検査（`BrokerError::InvalidPort`）はドライバの `validate_config` に任せる。
- **秘密値**: R2b-1 §3.5 の「`$secret` は検証に渡さず、接続にだけ開封して渡す」をここで配線する。`Connector` の closure がダイヤルの直前に、ドライバの `secret_keys()` のキーだけを開封して `Settings` に入れる（開封の口は host が注入する。平文は `Connector` の中の 1 回のダイヤルの間だけ、`Zeroizing`）。Modbus / SLMP は秘密のキーを持たないので、テストは偽のセッション型ドライバで行う。
- `BrokerHandle::slmp_cpu` を消し、代わりに **設定の指紋** `BrokerHandle::settings_fingerprint()`（ダイヤルに使った `Settings` の秘密を除いた正規形の hash）を出す。接続テストの「収集中の共有セッションを使ってよいか」（`slmp_session_cpu_refusal`）と `BrokerSimRegistry` の同一性（`broker_glue.rs:857`）はこれで比べる（プロトコルに依らない）。
- Hub の `read_path` / `write_path` は `TagSpec` / `WriteItem` を組んで渡す。`resolve_plc_address` は `DriverAddress` を返す（`Address` へ戻さない）。単位の逆変換・範囲の検査・文字列の長さの検査（`write_path.rs:1624-1645`・`:1799`）は Hub に残す（関門の一部。§9）。
- 接続テストの汎用化（Q11）もこの PR。
- core のテストの偽ドライバ（`apps/banto-hub/core/src/test_support.rs:172-309`。今は `DialFn` と `host_bound`）を `SessionDriver` の偽物に替える（`banto_driver_conformance::fake::FakeSessionDriver` を土台にし、今と同じく `banto_plc` の文法を持たせる）。
- grep のテスト（`apps/banto-hub/host/tests/registry_entrances.rs`）に needle を足す: Hub の core の非テストのコードに `BatchReadRequest`・`BatchWriteRequest`・`from_driver_address`・`banto_plc::Address` が無い。`Address::parse(` / `parse_slmp(` の例外 4 件（`:52-65`）は Q11 で消えるので外す。
- トリップワイヤ: `banto-drivers` の 2（broker の表 == `HostBound` の id、`lib.rs:265`）と host の `registry_tripwire.rs:62-76` は、「`builtin()` の項目はすべて `Frame` か `Session`」「Hub の registry の `Frame` / `Session` の集合 == broker が管理するプロトコルの集合」に書き換える。

### 4.3 収集と ChronoGazer（3a-3）

- `GroupPlan.requests` を `Vec<TagSpec>` にする。レジストリつきの組み立て（Hub）は今の `build_request_with_registry` から橋を除いたもの（`parse_address` の結果をそのまま `TagSpec`）。ビット指定 + `bit` 以外の断り（`finish_request`、`config.rs:1771` 付近）は R2b-2a でドライバの `parse_address` に移っているので、`TagSpec` の側では持たない。
- `banto-collect` の差し込み口は Q6。推奨の (a) では `PlcClient` を `TagSpec` の trait（仮称 `CollectClient { connect, read_batch(&[TagSpec]) -> Result<Vec<Result<Value, ItemError>>, FatalError>, disconnect }`）に替え、本番の実装は broker の読み取りハンドルの包み 1 つ（今の Hub の `BrokerReadClient`、#344 の `connect` の意味ごと `banto-collect` へ移す）にする。偽物（ChronoGazer の `OfflineClient`・`GatedClient`、`collector.rs` の 3 つ）はこの trait で書き直す。
- `ClientFactory` は「接続のキー → `CollectClient`」になり、`ClientSpec`・`ClientProtocol`・`client_spec`・`default_client_factory`・`ProtocolConfig::{ModbusTcp, Slmp}` を消す（#334・#605 の「手で保守する写し」が無くなる）。`ProtocolConfig` は「broker が読む」か「外部ソース」（`External`）の 2 つになる。`Protocol` はシミュレーションのためだけに残す（R2b-5）。
- ChronoGazer は §8。

### 4.4 シミュレーション（変えないが、ダイヤルの材料が変わる）

- Hub の `BrokerSimRegistry::resolve`（`broker_glue.rs:822`）は今「`(host, port, 同一性)`」を返し、broker のダイヤルに使う列を差し替えている。3a-2 では「`Settings` の `host` / `port` を差し替えた `Settings`」を返す形にする。シミュレータの種類を `"modbus-tcp"` で選ぶ分岐（`:835`）は R2b-5 まで残す（grep の例外に件数つきで載せる）。
- ChronoGazer のシミュレーションも同じ形にする（§8）。

## 5. エンジン（3b）

api-design §4.2 の手順と §4.2.1 の契約をそのまま実装する。新しい決めごとは作らない。

### 5.1 `Transport`

```rust
// banto-driver-engine::transport
pub trait Transport: Send + 'static {
    fn send<'a>(&'a mut self, bytes: &'a [u8]) -> BoxFuture<'a, Result<(), FatalError>>;
    /// 届いた分を buf に足す（0 バイトの戻りは相手が閉じた = FatalError::Io）
    fn recv<'a>(&'a mut self, buf: &'a mut Vec<u8>) -> BoxFuture<'a, Result<usize, FatalError>>;
    /// 受信の残りを捨てる（TCP は閉じるので使わない。シリアルは時間切れの後に使う、§7.3）
    fn discard_input(&mut self) -> BoxFuture<'_, ()>;
    fn close(&mut self) -> BoxFuture<'_, ()>;
}
pub enum TransportSpec { Tcp { host: String, port: u16 }, Serial(SerialSpec) }   // 設定の transport から作る
```

- `SerialOverTcp`（変換器へ生データ）は `TransportSpec::Tcp` と同じ実装。違うのはフレームの規則で、それはドライバが決める（Modbus RTU のドライバが `transport = "serial_over_tcp"` を受け、RTU のフレームを TCP に流す）。
- テスト用に記憶上のトランスポート（`tokio::io::duplex` の上、分割・遅延・切断を起こせる）を crate の中に置く。適合テストのハーネスはこれを使う（§5.6）。

### 5.2 読み取り

1 回の `Session::read_batch(specs)` で:

1. `conn.plan_read(specs)` → `Vec<Planned>`（`Err` は致命）。`covers` の範囲外・重複は契約違反として致命（api-design C3）。
2. 各 `Planned` について `step.send` を送り、`expect` に従って受ける。受信は 1 つの `response_timeout` の中で、`FrameRule` の規則で完結を判定する: `Fixed(n)`・`LengthPrefixed(spec)`（`PrefixEncoding` の Binary / AsciiHex / AsciiDec）・`Terminator(bytes)`・`DriverDecides`（受信のたびに `is_complete`。`Discard(n)` は先頭 n バイトを捨てて続ける。`n == 0` は契約違反で致命）・`None`（受けない）。`max_len` を超えたら致命（`FatalError::Framing`）。
3. 時間切れ（`FatalError::Timeout`）・`max_len` 超過・相手の切断は致命。**受信バッファを捨て、トランスポートを閉じる**（TCP）。遅れて届いた応答を次の step に使わない（`banto-plc/src/error.rs:39-47` の `ResponseTimeout` の理由と同じ）。
4. `Complete { len }` の後ろに余りがあれば捨てる（次の step に持ち越さない、`frame.rs` の `FrameCheck::Complete` の doc）。
5. `conn.decode_read(planned, specs, response)` → `covers` と同じ長さの結果（長さの食い違いは致命）。`RawValue` を `Value` に写す: `Bool` → `Bit`、`Int` / `Uint` / `Float` → `F64`、`Bytes` → `TagKind::Str` の `encoding` で decode（今の `decode_string_value` の規則: 語の下位バイトが先、最初の NUL で切る）。`Numeric` に `Bytes` が来たら、そのタグだけ `ItemError::Unsupported`。
6. `covers` に出てこない添字は `ItemError::Unsupported("計画に含まれない")`。結果を `specs` の順に並べる。
7. フレーム型のドライバが `FatalError::Io` / `Timeout` を返したら契約違反（C4）。エンジンは致命として扱い、ログに残す。

### 5.3 書き込み

api-design §4.2.1 のループ（`write_next(items, None)` → `Send(planned)` を送って応答を受ける → `write_next(items, Some(resp))` … → `Done(results)`）。

- 1 つの `Session::write_batch(items)` = 1 回の論理書き込み。broker のジョブ 1 つがそれを最後まで `await` するので、**途中に同じ接続の読み取り・別の書き込みが挟まらない**（契約 6）。broker の job ループの構造（`run_broker_task`、一度に 1 ジョブ）が今と同じく守る。
- step の上限 `3 × items.len() + 8`（契約 7）。超えたら致命。`Done` の件数の食い違いも致命（panic させない）。
- 応答の受け方・時間切れの扱いは読み取りと同じ（§5.2 の 2〜4）。致命なら書き込み全体が `Err`、接続は作り直す（ドライバの途中の状態も接続ごと捨てる、契約 5）。

### 5.4 接続・ハンドシェイク・別れ

- 接続: `TransportSpec` を開く（`connect_timeout` で上限）→ `create_connection(settings)` → `handshake_next(None)` から `None` まで（step は §5.2 と同じ受け方、上限 32 step はハーネスの `MAX_HANDSHAKE_STEPS` と同じ）。拒否は `Err`（接続成功とみなさない）。
- 切断（broker の停止・設定の変更）: `farewell()` が `Some(step)` なら送るだけ（応答は待たない）→ `close`。

### 5.5 時間の上限（「返ってこない」障害）

Hub の即時読み取り（`read_path.rs:198`）と書き込み（`write_path.rs:1485`・`:1585`）は broker の応答に外側の上限を掛けていない。今はセッションの中の時間切れ（Modbus は要求ごとの `response_timeout`、SLMP は `slmp` の `recv_timeout`）が上限になっている。エンジンは **すべての `await` に上限を持つ**: 接続（`connect_timeout`）、送信（`response_timeout`。送信が詰まるシリアルのため）、受信（step ごとの `response_timeout`）、書き込みの step 数。これで 1 ジョブの最長は「step 数 × `response_timeout`」で抑えられる（checklist §6 の「返ってこない」障害）。

### 5.6 適合テストのハーネスを本物のエンジンへ（Q3）

- `frame::run_all` の公開の形（`driver`・`FrameFixtures`・`Responder`）は変えず、中身を「本物のエンジン + 記憶上のトランスポート（`Responder` が答える） + tokio の止めた時計（`start_paused`）」に替える。障害（分割・遅延・伸び続ける・先頭のごみ・途中の切断）は記憶上のトランスポートが起こす。今のハーネスのエンジン（`frame/engine.rs`）は消す。
- 「エンジンの契約違反の検出」（`covers` の範囲外・`Discard(0)`・`Done` の件数・フレーム型の `Io`）は本物のエンジンが返すエラーとログを見て `Failure` にする。エンジン側にテスト用の観測口（違反を数える `EngineEvents`）を足す。
- C16（割り込みと上限）は broker + エンジンで確かめる（ジョブの直列化は broker の構造なので、ハーネスだけでは確かめられない）。`banto-broker` の単体テストに足す。

## 6. Modbus TCP・SLMP を Frame へ（3c・3d）

### 6.1 Modbus TCP（3c）

| 部品                | 中身                                                                                                                                                                                                                                                                                                                      |
| ------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `create_connection` | `settings` から `unit_id`・`word_order`。`next_transaction_id: u16` を接続の中に 1 つ持つ（`plan_read`・`write_next` とも `&mut self` なので、読みと書きで 1 つの数え手を共有する今の性質がそのまま残る。`ModbusSession` の doc `session.rs:48-78`）                                                                      |
| `plan_read`         | 今の `plan_batch_requests`（`banto-plc/src/planning.rs`）を `TagSpec` から呼ぶ（canonical → `Address` はドライバの中）。1 グループ = 1 step、`Step.send = wrap_mbap(tid, unit_id, build_request_frame(..))`、`expect = LengthPrefixed(PrefixSpec::new(4, 2, true, 6))`（MBAP の長さ欄 = 後ろのバイト数、総長 = 6 + 長さ） |
| `decode_read`       | `parse_mbap_header` で ID と unit を照合（違えば `FatalError::Protocol` = P0-3 の 2b）、`parse_response_pdu` で例外応答は `ItemError::Device`、値は今の `decode_register_value` / `decode_register_bit`（ワード順は接続の `word_order`）                                                                                  |
| `write_next`        | 今の `plan_modbus_writes` の 1 要求 = 1 step（FC5/6/15/16 と応答のエコーの照合、`banto-plc-write/src/modbus/mod.rs:122-227`）。隣り合わない要求をまとめない（C10）。ビット指定は `ItemError::Unsupported`                                                                                                                 |
| 消すもの            | `ModbusTcpClient`（`PlcClient` の実装）・`dial_modbus`・`execute_modbus_reads`・`execute_modbus_writes`・`ModbusWriteClient`・`ModbusSession`（3a-1 で作った `Session` の実装）                                                                                                                                           |

### 6.2 SLMP（3d、Q4）

| 部品             | 中身                                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| ---------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| フレーム（新規） | `banto-driver-slmp/src/frame.rs`: 4E バイナリの要求（サブヘッダ 0x5400・`serial_id`・経路・データ長・CPU 監視タイマ・コマンド・サブコマンド・デバイス）と応答の検証（0xD400・`serial_id`・経路・データ長・終了コード）。一括読み出し 0x0401 / 一括書き込み 0x1401、ワード / ビット、Q / R のサブコマンドとデバイス番号の幅（#605）。`slmp` の `create_subheader`・`validate_response` と、banto のシミュレータの `parse_bulk_request` / `frame` を照合の相手にする |
| `plan_read`      | 今の `plan_slmp_batch`（`banto-plc/src/slmp/planning.rs`）。`expect = LengthPrefixed(PrefixSpec::new(11, 2, false, 13))`（ヘッダ 13 バイト、データ長は 11..13 の LE）                                                                                                                                                                                                                                                                                              |
| `decode_read`    | 終了コード ≠ 0 はそのまとまりの `ItemError::Device`（今の `classify_slmp` と同じ）、欄の不一致は `FatalError::Framing`。文字列は `RawValue::Bytes`（decode はエンジン、§5.2）                                                                                                                                                                                                                                                                                      |
| `write_next`     | 今の `execute_slmp_writes` の手順を状態機械に: 語の書き込みは 1 step、ワード内ビットは「今の値を読む → 対象のビットだけ変えて書く → 確認読み」の 3 step（`execute_one_bit_write`、`banto-plc-write/src/slmp/mod.rs:354-419`）。同じ語の複数のビットは 1 回の RMW。各段の機器の拒否はそのまとまりだけ `ItemError::Device` にして以降の step を送らない、確認の不一致はビットごとに `ItemError::Rejected`（C13〜C15）                                                |
| `serial_id`      | 今と同じ固定値（Q 化しない。遅れた応答は時間切れでの切断が守る。要求ごとの番号にするのは後でやること）                                                                                                                                                                                                                                                                                                                                                             |
| 消すもの         | `SlmpClient`・`dial_slmp`・`execute_slmp_batch_reads`・`execute_slmp_reads`・`execute_slmp_writes`・`SlmpWriteClient`・`SlmpSession`。`slmp` への依存はデバイスの wire code（`SlmpDevice::to_wire`、`banto-plc/src/slmp/mod.rs:151`）だけになるので、表を写して依存ごと外せるか 3d で確かめる                                                                                                                                                                      |

- `banto-drivers/examples/real_plc_verify.rs`（1255 行、`SlmpClient` / `SlmpWriteClient` / `BatchReadRequest` を直接使う実機の検証）は、エンジン経由（`banto_driver_engine::dial` で `Session` を作る）に書き直す。実機の確かめの手順書（C-1〜C-14）がこれを使っているかは着手時に確かめる。

## 7. トランスポートとシリアル（3e）

### 7.1 設定

| キー                                       | 値                                                | 備考                                                                                       |
| ------------------------------------------ | ------------------------------------------------- | ------------------------------------------------------------------------------------------ |
| `transport`                                | `tcp`（既定）・`serial`・`serial_over_tcp`        | ドライバの `descriptor().transports` に無いものは `validate_config` が断る（今と同じ形）   |
| `serial_port`                              | Windows は `COM1`〜`COM256`、Linux は `/dev/tty*` | 許可リストの文法で検査する（checklist §5 の「外から来た名前」。`NUL`・`:` などを通さない） |
| `baud`・`data_bits`・`parity`・`stop_bits` | 9600 / 8 / `none` / 1 など                        | 新しいキー（追加なので API の破壊ではない、api-design §9.1）                               |
| `station`                                  | 局番                                              | Modbus RTU は `unit_id` と同じ意味（api-design §3.5 の `keys::STATION`）                   |
| `response_timeout_ms`                      | 接続ごと                                          | 変換器を通る分、長めにとれる（refactor-plan §7 Q4）                                        |

### 7.2 Windows の COM ポート

- **排他**: Windows の COM ポートは 1 つのプロセスしか開けない（2 つ目は `ERROR_ACCESS_DENIED`）。Hub のサービスと他のアプリ（設定ツール・ターミナル）がぶつかると接続が致命 `FatalError::Io` になり、broker のバックオフで再試行し続ける。文言に「他のアプリがこのポートを使っていないか」を出す。
- **権限**: COM ポートを開くのに管理者は要らない（LocalSystem のサービスでも利用者のデスクトップでも開ける）。**推測**: USB シリアル変換器の抜き差しで番号が変わることがあり、そのときは設定の変更が要る。
- **名前**: `COM10` 以上は `\\.\COM10` の形で開く必要がある。**推測**: `serialport` crate は内部でこの接頭辞を付ける。3e で確かめる。
- **CI**: Windows の CI に仮想の COM ポートは無い。Linux の CI で pty の対（**推測**: `tokio-serial` の `SerialStream::pair()`）を使ってトランスポートの単体を確かめ、Windows の実機は手で確かめる（実機の確かめの表に行を足す）。

### 7.3 時間切れの後のシリアル

TCP は時間切れでソケットを閉じれば遅れた応答は届かない。シリアルは線が残るので閉じても後から届く（ポートを開き直す費用も大きい）。エンジンは、シリアルで致命になったら:

1. 受信の残りを捨てる（`discard_input`）。
2. 待ち（`response_timeout` の半分、上限 1 秒）の後にもう一度捨てる（遅れて届く応答を流す）。
3. 接続（`FrameConnection`）は作り直す（ハンドシェイクからやり直す）。ポートは開いたまま。

フレームの終わりは無音の間隔では判定しない（refactor-plan §7 Q4・api-design §8）。Modbus RTU は `DriverDecides` で FC とバイト数から長さを決め、CRC は検証に使う（api-design §4.2）。局番と FC が要求と合わない応答は `FatalError::Framing`（ほかの局の遅れた応答を採らない）。

### 7.4 マルチドロップ（Q9）

推奨の (b) では、R2b-3 は 1 つのシリアルのポート（`serial_port`、変換器なら `host:port`）を有効な接続 1 つだけが使える。2 つ目の有効な接続の保存を断る（`banto-tags` の保存の検証に、レジストリつきの経路でだけ足す）。エンジンの `TransportSpec` を「ポートの鍵」として扱う形にしておけば、後でポートごとの直列化（局をまたいで 1 つずつ要求 - 応答を流す）を足すのは追加で済む。

### 7.5 Modbus RTU（Q8）

- 新しいドライバ id `modbus-rtu`（`banto-driver-modbus` の中に 2 つ目の `FrameDriver`）。アドレスの文法・計画・デコードは Modbus TCP と同じ（`Address::parse`・`plan_batch_requests`）。違うのはフレーム（局番 + PDU + CRC-16、`DriverDecides`）と `transports = [Serial, SerialOverTcp]`。
- 書き込みは Modbus TCP と同じ範囲（FC5/6/15/16）。ブロードキャスト（局番 0）は扱わない（`FrameRule::None` を使う場面が無い）。
- 画面からは作れない（Q10）。REST・MCP・設定パッケージで作る。

## 8. ChronoGazer（Q5）

- ChronoGazer の収集も broker を通す: `EngineConfig` に `drivers: Arc<DriverRegistry>`（bin が `banto_drivers::builtin().registry` を詰める）を足し、`CollectService` が `BrokerSupervisor` を持つ。`banto-collect` の計画はレジストリつきの組み立て（Hub と同じ `parse_address` → `TagSpec`）にする。
- 保存時の規則は変えない（`check_tag_address`・`TagService::new` の旧規則、綴りのまま保存。address 設計 Q7 のとおり R2b-7）。綴りのままでもドライバの `parse_address` は読める（R2b-2 §5）。
- ChronoGazer には書き込みが無いので、broker の書き込みの口は使わない。
- **振る舞いの変化**: 今の ChronoGazer の収集は自分でソケットを張り直し、`plc_disconnected` / `plc_reconnected` は自分の接続の状態で出していた。broker を通すと Hub と同じく #344 の形（broker の状態を見て、`BROKER_RECONNECT_GRACE` = 3 秒待つ、`broker_glue.rs:306`）になる。出来事の時刻が少しずれうるので、ChronoGazer の出来事のテストを 3a-3 で見直す。
- 依存: `chronogazer-core` は `banto-broker`・`banto-driver-api` に依存する。`banto-drivers` を normal で持つのは bin（`banto-serve` は core の package の中）なので、core の package の normal の依存に `banto-drivers` が入る（Hub の R1-3b の一時の形と同じ。api-design §6.4）。`chronogazer-host` の分離（CI の対象を縮める）は R2b-3 に含めない。
- シミュレーション: 今の `Collector` のアドレスの差し替え（`collector.rs:423`・`:632`）を、Hub の `BrokerSimRegistry` と同じ「`Settings` の `host` / `port` を差し替えてから `ensure_connection`」にする。Hub と ChronoGazer で同じ部品を使えるよう、`BrokerSimRegistry` を `banto-collect` へ移すかは 3a-3 で決める（決め手は Hub の固有の依存の有無）。

## 9. 書き込みの安全（tag-server-design §6、api-design §11）

- 関門の順（`writable`・API キーのスコープ・受付・頻度・監査の先書き `insert_pending`）とアドレスの解析の位置（監査行の後、2026-10-22 オーナー決定）は変えない。変わるのは「解析した結果を何に組むか」（`BatchWriteRequest` → `WriteItem`）だけ。
- 単位の逆変換・型の範囲の検査（`validate_numeric_range`）・文字列の長さの検査は Hub に残す（関門の一部で、422 を返す場所が変わらない）。整数への丸めと範囲の最終の検査はドライバの `write_next` も持つ（今の `encode.rs:192` の `require_integral_in_range`、WIT §9.2 の表と同じ）。
- RMW を選ぶ判断は Hub からドライバへ移る（§4.1）。Hub は `WriteItem { spec: TagSpec { address (bit つき), kind: Numeric(Bit) }, value: Bit }` を渡すだけ。
- 1 ジョブ = 1 回の論理書き込み、確認読みを含めて途中に他の要求を挟まない、は broker の job ループの構造で守る（今と同じ）。一括書き込み（`write_path.rs:1485`、同じ接続の要求を 1 ジョブ）もそのまま。
- 監査に入る結果は `ItemError` の文言（`Device` は機器の終了コード / 例外コード、`Rejected` は確認の不一致）。文言が今と変わるときは、監査の表示（`write_audit`）のテストの期待値を直す。

## 10. `banto-driver-api` 0.2.0

| 変更                                                                                       | 破壊か                              | 直し方（CHANGELOG に書く）                                                     |
| ------------------------------------------------------------------------------------------ | ----------------------------------- | ------------------------------------------------------------------------------ |
| `DriverKind::HostBound`・`DriverEntry::host_bound` を消す                                  | **破壊**（minor を上げる）          | 組み込みのドライバは `DriverEntry::frame` か `DriverEntry::session` で登録する |
| `DriverBase::probe() -> Option<(String, TagKind)>`（既定は `None`。接続テストで読む 1 点） | 破壊ではない（既定つき）            | -                                                                              |
| `keys::BAUD`・`DATA_BITS`・`PARITY`・`STOP_BITS` を足す                                    | 破壊ではない                        | -                                                                              |
| `ConnectContext` に field を足すなら（例: 開封した秘密値の扱い）                           | 破壊ではない（`#[non_exhaustive]`） | -                                                                              |
| doc の直し（`registry.rs` の `HostBound` の説明、`lib.rs:69-72`、`address.rs:9-15`）       | -                                   | -                                                                              |

- `banto-driver-conformance` も 0.2.0（API の minor に合わせる、api-design §9.1）。`frame::run_all` の形は変えないが、中身が本物のエンジンになる。`LegacySession` は crate の外（ドライバのテスト）にあったので conformance の API には出ていない。
- `banto-driver-engine` は新しく 0.1.0（公開する。§3）。
- 公開の時期は Q12。

## 11. テスト

### 11.1 P0-3 の 6 本の移し先

| #   | 新しい居場所                                                                 | シナリオ（今と同じ）           | 新しい I/O の上での期待                                                                                                                                                                                          |
| --- | ---------------------------------------------------------------------------- | ------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 1   | `crates/banto-driver-modbus/tests/p0_3.rs`（エンジン + Modbus シミュレータ） | 1・3・7・8 バイトずつ、各 2 回 | 同じ値。2 回目もずれない                                                                                                                                                                                         |
| 2a  | 同上                                                                         | 時間切れ後に前の応答を流す     | 時間切れは致命、エンジンがソケットを閉じる。次の読み取りは新しい接続で今の値（古い値は返らない）。「同じ stream を使い続ける」版は、エンジンが使い続けないことを固定するテストに替える（閉じた後に送らないこと） |
| 2b  | 同上（と `decode_read` の単体）                                              | ID の違う応答                  | `FatalError::Protocol`                                                                                                                                                                                           |
| 3   | 同上・SLMP も                                                                | 閉じたポート                   | `connect_timeout` 以内に失敗、ハングしない                                                                                                                                                                       |
| 4   | 同上                                                                         | 型の混ざった一括読み取り       | 数値は Good。文字列は `parse_address` が断る（R2b-2a）ので計画に来たら `ItemError::Unsupported`                                                                                                                  |
| 5   | `crates/banto-collect/tests/integration.rs`（broker 経由になった収集）       | 数周期の収集                   | Good で読め、`connection_count() == 1`                                                                                                                                                                           |
| 6   | `crates/banto-driver-slmp/tests/p0_3.rs`（エンジン + SLMP シミュレータ）     | 1・5・13 バイトずつ、各 2 回   | 同じ値                                                                                                                                                                                                           |
| -   | `apps/banto-hub/host/tests/integration.rs:1159`                              | Hub の収集                     | そのまま通る                                                                                                                                                                                                     |

- 移す PR: 1〜4 は 3c、6 は 3d、5 は 3a-3（broker 経由）で一度通し 3c でもう一度通す。元のテスト（`banto-plc/src/*/integration_tests.rs` のうち消す I/O を使うもの）は移した PR で消す。消すテストの名前と移した先の対応を PR に書く（テストの数が減らないこと）。
- 既存の振る舞いの表（refactor-plan §8.3 の「既にあった振る舞い」: 応答の時間切れで壊す・途中切断・再接続・バックオフ・致命 / 非致命・ワード順・文字列・ビット）も、`ModbusTcpClient` / `SlmpClient` を使うものは同じ形で移す。

### 11.2 適合テスト

| 走らせ方                                                                     | 対象                                                                   | 項目                                                                                                                                         | PR           |
| ---------------------------------------------------------------------------- | ---------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------- | ------------ |
| `session::run_all_against`（本番のセッション + `FaultProxy` + シミュレータ） | Modbus / SLMP の `Session`（3a-1）→ エンジンで包んだ `Frame`（3c・3d） | C1〜C8・C12（SLMP は C13〜C15）                                                                                                              | 3a-1・3c・3d |
| `frame::run_all`（本物のエンジン + 記憶上のトランスポート）                  | `EchoFrameDriver`                                                      | C3〜C7・C9〜C12（今と同じ）                                                                                                                  | 3b           |
| `frame::run_all`                                                             | Modbus TCP / SLMP / Modbus RTU                                         | C3〜C7・C9〜C12。`Responder` はシミュレータの「要求のバイト列 → 応答のバイト列」の部分（`build_response`、今は private）を公開して包む       | 3c・3d・3e   |
| broker の単体                                                                | エンジン + 偽のフレーム型ドライバ                                      | C16（書き込みの途中に読み取りを投げても `Done` まで送られない・step の上限で致命・致命の後の書き込みは新しい接続で `write_next(None)` から） | 3b           |

### 11.3 反証（checklist §5）

各 PR で 1 回ずつ確かめて PR に書く: エンジンで時間切れの後に受信バッファを捨てない → P0-3 の 2a が落ちる、Modbus の ID の照合を外す → 2b が落ちる、`LengthPrefixed` の総長の計算を外す → 1・6 が落ちる、書き込みの step の上限を外す → C16 が落ちる、broker のジョブを並行に流す → C16 が落ちる、SLMP の RMW で今の値の他のビットを保たない → C13 が落ちる。

## 12. PR の分け方

共通: 検証の順は checklist §2（`cargo fmt --all --check` → test → clippy → **公開 API の形を変える PR は全部 `cargo check --workspace --all-targets`**（§2-3）→ prettier → 新しい crate は `ci-changes.test.mjs` の固定グラフと `git add` 後の `node --test`（§2-6）→ `cargo deny check`）。migration は無い（設定のキーが増えるだけ）ので、どの PR も revert で戻る。ディスクが 3 GB しか無いので、テストは対象の crate だけを回す（checklist §2-2）。

| PR   | 範囲                                                                                                                                                                                                                      | 主な触るファイル                                                                                                                                                                                                                                                   | 監査の焦点                                                                                                      | モデル                     |
| ---- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | --------------------------------------------------------------------------------------------------------------- | -------------------------- |
| 3a-1 | Modbus / SLMP の `SessionDriver`（§4.1）。適合テストを本番のセッションへ、`LegacySession` を消す。登録は変えない                                                                                                          | `crates/banto-driver-{modbus,slmp}/src/{session,lib}.rs`・`tests/`                                                                                                                                                                                                 | RMW の判断（`bit` + `Bit`）が今の Hub の判断と同じ集合か。致命の写しが文字列に頼らないこと                      | sonnet                     |
| 3a-2 | broker の要求の型と `Session` でのダイヤル、`DriverTable`・`HostBound`・橋の削除、設定の指紋、秘密値の口、Hub の read / write / 接続テスト（Q11）、core の偽ドライバ、grep・トリップワイヤ（§4.2）                        | `crates/banto-broker`、`crates/banto-driver-api/src/registry.rs`、`crates/banto-drivers`、`crates/banto-plc/src/address.rs`、`apps/banto-hub/core/src/{broker_glue,read_path,write_path,test_support}.rs`・`rest/plc_connections.rs`、`apps/banto-hub/host/tests/` | 書き込みの関門の順と RMW の判断の移動（§9）。秘密値を開封する範囲。broker の直列化が変わっていないこと          | 上位（実装は sonnet）      |
| 3a-3 | 収集の計画を `TagSpec`、`CollectClient`、`default_client_factory`・`ClientSpec`・`ClientProtocol` の削除、ChronoGazer を broker へ（§4.3・§8）                                                                            | `crates/banto-collect`、`apps/banto-hub/core/src/{broker_glue,hub}.rs`、`apps/chronogazer/core`・bin                                                                                                                                                               | #344 の出来事の意味が ChronoGazer で保たれるか。#334 / #605 の写しが消えたこと                                  | 上位（実装は sonnet）      |
| 3b   | `banto-driver-engine`（`Transport`・TCP・`FrameSession`・文字列の decode・時間の上限）、broker が `Frame` をダイヤル、適合テストのハーネスを本物のエンジンへ、C9〜C11・C16（§5）                                          | 新 `crates/banto-driver-engine`、`crates/banto-driver-conformance/src/frame*`、`crates/banto-broker`                                                                                                                                                               | すべての `await` に上限があるか。時間切れで受信バッファを捨てて閉じるか。ハーネスの違反の検出を落としていないか | 上位（opus / fable）       |
| 3c   | Modbus TCP の `FrameDriver`、P0-3 の 1〜5 の移動、Modbus の I/O の削除（§6.1）                                                                                                                                            | `crates/banto-driver-modbus`、`crates/banto-plc/src/modbus/`、`crates/banto-plc-write/src/modbus/`                                                                                                                                                                 | トランザクション ID の数え手が 1 つ。テストの数が減らないこと                                                   | sonnet ＋ 上位の監査       |
| 3d   | SLMP の 4E フレーム（Q4）と `FrameDriver`・RMW の `write_next`、P0-3 の 6・C13〜C15、SLMP の I/O の削除、`real_plc_verify` の書き直し（§6.2）                                                                             | `crates/banto-driver-slmp`、`crates/banto-plc/src/slmp/`、`crates/banto-plc-write/src/slmp/`、`crates/banto-drivers/examples/`、ルート `Cargo.toml`（`slmp`）                                                                                                      | RMW の各段の拒否・確認の不一致（§4.2.1 の契約 4）。Q / R のフレームの違い（#605）                               | 上位（書き込みの安全）     |
| 3e   | シリアルのトランスポート、設定キー、Modbus RTU（Q8）、1 ポート = 1 接続の検査（Q9）、時間切れの後のシリアルの扱い（§7）                                                                                                   | `crates/banto-driver-engine/src/transport/serial.rs`、`crates/banto-driver-modbus`、`crates/banto-tags`、`deny.toml`（要れば）                                                                                                                                     | 名前の検査（checklist §5）。Linux の CI で余計な system library を要らないか（`libudev`、§14）                  | sonnet ＋ 上位の監査       |
| 3f   | docs（本書・api-design・refactor-plan・plugin-design・tag-server-design §3.4・運用ガイドの接続の節）、`banto-driver-api` / conformance の CHANGELOG と版 0.2.0、`banto-driver-engine` 0.1.0、banto-hub の版の 4 点（Q13） | `docs/*`、`crates/*/CHANGELOG.md`、版の 4 点                                                                                                                                                                                                                       | 破壊の書き方                                                                                                    | haiku（docs）＋ 上位の確認 |

- 依存: 3a-1 → 3a-2 → 3a-3。3b は 3a-2 の後（broker が API の型になってから `Frame` を足す。エンジンの crate 自体は 3a と並行に作れる）。3c・3d は 3b の後で互いに独立。3e は 3b・3c の後（Modbus の部品を共有）。3f は最後。どの PR の後も main は緑。
- 一番大きいのは 3a-2（Hub のテストの期待値の直しを含む）と 3b。期待値の直しはコミットを分ける（R2b-2b と同じ）。

## 13. オーナー判断

| Q   | 問い                                                             | 選択肢                                                                                                                                                                                                                                                                       | 推奨                                                                                                                                                                                                                                                                                    |
| --- | ---------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Q1  | R2b-3 の切り方                                                   | (a) 3a〜3f の PR 列（§12）/ (b) 要求の型の切り替え（3a）だけを R2b-3 にし、エンジン・Frame 化・シリアルを R2b-3' として別の段に / (c) 1〜2 本の大きな PR                                                                                                                     | **(a)**。refactor-plan の R2b-3 の定義（トランスポート・シリアル・P0-3）を 1 段に保ちつつ、PR ごとに main を緑に保てる。(b) は段の完了条件（P0-3 を新しい I/O で）が 3a だけでは満たせないので段の区切りとして弱い                                                                      |
| Q2  | 要求の型の切り替えの順                                           | (a) まず `Session` で包んで型を替え（3a）、後で `Frame` へ（3c・3d）/ (b) エンジンを先に作り、`HostBound` から `Frame` へ一度に                                                                                                                                              | **(a)**。オーナーが最初に挙げた部分（橋の撤去）を、危険の大きいエンジンと切り離して先に入れられる。二度書きは `LegacySession` の昇格だけ。(b) は 1 つの PR に要求の型・エンジン・2 つのプロトコルのフレームが重なる                                                                     |
| Q3  | エンジンの置き場所と適合テストのハーネス                         | (a) 公開の新しい crate `banto-driver-engine`。ハーネスも本物のエンジンを使い、今のハーネスのエンジンを消す / (b) エンジンは `banto-broker` の中、ハーネスは今のまま（手順の実装が 2 つ）/ (c) (a) の crate を作るがハーネスは今のまま                                        | **(a)**。§4.2 の手順（分割・時間切れで捨てる・`Discard`・書き込みの上限）を 1 つの実装にする。第三者も本物のエンジンで適合テストを走らせられる。`banto-broker` は `banto-tags`（git 依存の `banto-core`）に依存するので公開できず、(b) ではハーネスが本物を使えない                     |
| Q4  | SLMP のフレーム                                                  | (a) `banto-driver-slmp` が 4E バイナリを自分で組み、`slmp` の I/O を使わない / (b) `tyaro/slmp` に純粋なフレームの API を足して版を上げる / (c) SLMP は `Session` のまま（`SLMPClient` を包む）                                                                              | **(a)**。使うのは一括読み書きだけで小さく（§1.7）、シミュレータが受け側を持つので照合できる。ドライバ crate から git 依存が消える（公開・別リポジトリへの移動の前提）。(c) は SLMP だけ分割受信・時間切れの扱いがエンジンの外に残り、「一本化」にならない                               |
| Q5  | ChronoGazer の収集の I/O                                         | (a) R2b-3 で broker とエンジンを通す（`EngineConfig` にレジストリ、保存時の規則は R2b-7）/ (b) R2b-7 まで今の直接のソケット                                                                                                                                                  | **(a)**。R2b-3 の定義が「Hub と ChronoGazer で共用」。(b) では `PlcClient`・`ModbusTcpClient`・`SlmpClient`・`Address` の要求の型が ChronoGazer のためだけに残り、橋を消せない                                                                                                          |
| Q6  | `banto-collect` の差し込み口                                     | (a) `PlcClient` を `TagSpec` の trait に替え、本番の実装は broker の読み取りハンドルの包み 1 つ（今の `BrokerReadClient` を移す）/ (b) task が `ReadOnlyHandle` を直接持ち、trait を消す                                                                                     | **(a)**。#344 の `connect` の意味（broker の状態を見る）を 1 か所に保ち、テストの偽物（ChronoGazer の 2 つ・`collector.rs` の 3 つ）の差し込み口を残せる                                                                                                                                |
| Q7  | `banto-plc` / `banto-plc-write` の行き先                         | (a) I/O を消し、計画・デコード・フレーム・シミュレータを残す / (b) Modbus の中身を `banto-driver-modbus`、SLMP を `banto-driver-slmp` へ移し、`banto-plc` は共通の部品だけ                                                                                                   | **(a)**（R2b-3 では）。差分とレビューを小さくする。(b) は CI の対象を縮める（api-design §2.5）が、R2b-3 の危険（I/O）と無関係な大きな移動になる。公式ドライバを別リポジトリへ出すとき（refactor-plan §7 Q2）にまとめてやる                                                              |
| Q8  | シリアルの範囲                                                   | (a) トランスポート（COM 直結）+ Modbus RTU のドライバ / (b) トランスポートだけ（ドライバは次の段）/ (c) シリアルを R2b-3 から外す                                                                                                                                            | **(a)**。ドライバが無いとトランスポートを本番の形で確かめられない。Modbus RTU は Modbus TCP の計画・デコードを共有し、同じ実機で比べられる（refactor-plan §7 Q1 の候補）                                                                                                                |
| Q9  | マルチドロップ（1 つのポートに局番違いの接続が複数）             | (a) R2b-3 でポートごとの直列化まで / (b) R2b-3 は 1 ポート = 1 接続（保存で断る）、共有は形だけ用意して後で                                                                                                                                                                  | **(b)**。1 局の時間切れで同じポートの他局をどうするか（§7.3 の捨て方・作り直しの範囲）を、最初の実機で 1 対 1 を確かめてから決める。`TransportSpec` をポートの鍵にしておけば後から足せる                                                                                                |
| Q10 | シリアルの接続を画面から作れるか                                 | (a) R2b-6（汎用フォーム）まで REST・MCP・設定パッケージだけ / (b) 接続のフォームに最小の COM の欄を手で足す                                                                                                                                                                  | **(a)**。R2b-6 で汎用フォームに替わるので、手で足した欄は捨てることになる。実機の確かめは MCP か REST で行う                                                                                                                                                                            |
| Q11 | 接続テスト                                                       | (a) `DriverBase::probe()`（既定つき）を足し、Hub の接続テストはエンジンの 1 回きりの接続（ハンドシェイク + `probe` の 1 点）で行う。収集中は共有セッションを使い、同一性は設定の指紋で見る / (b) 今のプロトコル別（`ModbusTcpClient` / `SlmpClient` と固定のリテラル）を残す | **(a)**。(b) では `PlcClient` の実装と `Address::parse` の例外が接続テストのためだけに残る。100 種類のドライバに接続テストを書き足す形にならない                                                                                                                                        |
| Q12 | `banto-driver-api` の版と公開                                    | (a) 0.2.0 に上げ（`HostBound` の削除）、R2b-3 の後に `banto-driver-api` 0.2.0・conformance 0.2.0・engine 0.1.0 を crates.io に初めて公開する / (b) 0.2.0 に上げるが公開はまだしない（W 段や外部の作者が出てきたら）/ (c) 0.1.0 は未公開なので版を上げずに中身を変える        | **(a)**。0.1.0 は未公開（§1.4）なので、一時の `HostBound` を含まない最初の版を公開する方が利用者に優しい（api-design Q3 の (b) の利点）。版を上げるのは CHANGELOG の「何が壊れたか」を R1 の形から追えるようにするため。公開するかどうかは別に決めてもよい（(b)）                       |
| Q13 | banto-hub の版                                                   | (a) R2b-3 の全部（3f）の後に alpha を 1 つ上げる / (b) PR ごと / (c) 上げない                                                                                                                                                                                                | **(a)**（R2b-1 の Q13・R2b-2 の Q8 と同じ）。番号は 3f の時点で空いている次の番号（alpha.36 は alpha.34 の実機の確かめの直しに取ってある。2026-10-22 オーナー了承）。CHANGELOG に「ChronoGazer の収集も共有セッション」「シリアル（Modbus RTU、COM 直結）」「接続テストの汎用化」を書く |
| Q14 | R2b-2 の残り「Hub の REST のテストのハーネスを本物のドライバに」 | (a) R2b-3 とは別のまま（後回し）/ (b) 3a-2 に含める（core の偽ドライバをどのみち `SessionDriver` に書き直すので）                                                                                                                                                            | **(a)**。3a-2 は既に大きい。3a-2 の偽ドライバは今と同じく `banto_plc` の文法を持たせ、43 件が落ちる問題を持ち込まない                                                                                                                                                                   |

## 14. 確かめきれなかったこと・危険

- **シリアルの crate**: `serialport` のライセンス（MPL-2.0 と記憶）、Linux の既定の feature が `libudev`（ビルドに `libudev-dev` と `pkg-config` が要る）か、`tokio-serial` の `default-features = false` で避けられるか、Windows の `COM10` 以上の扱い、pty の対で CI のテストができるかは、どれも記憶に基づく推測。3e の最初に確かめる（`cargo deny check` と Linux / Windows の CI）。
- **SLMP のデバイスの wire code**: `slmp` crate への依存を完全に外せるか（`DeviceType` の表を写して足りるか）は 3d で確かめる。外せなくても I/O を使わないので推奨（Q4 の (a)）は変わらない。
- **`real_plc_verify` と実機の手順書**: `crates/banto-drivers/examples/real_plc_verify.rs` が実機の確かめ（C-1〜C-14、B-16〜B-28）で使われているかは見ていない。使われているなら 3d の書き直しの後に手順書も直す。
- **ChronoGazer の出来事の時刻**: broker を通すと `plc_disconnected` / `plc_reconnected` の出方が Hub と同じになる（§8）。ChronoGazer の利用者向けの意味（記録の欠けの表示）が変わらないかは 3a-3 で ChronoGazer のテストを見て確かめる。
- **適合テストのハーネスの書き直しの量**: 今のハーネス（`frame.rs` 906 行・`frame/engine.rs` 838 行）の障害の起こし方を、記憶上のトランスポートと止めた時計で同じ決定性に保てるかは 3b の最初に小さく試す。保てなければ Q3 の (c)（ハーネスは今のまま）に倒し、その旨を PR に書く。
- **`Session` の結果の型**: `api::Session::read_batch` は `Value` を返し（`RawValue` ではない、api-design §5）、文字列の decode はセッション型ではドライバの責任のまま。エンジンが decode を揃えるのはフレーム型だけ。3a のセッション型の Modbus / SLMP は今の `banto-plc` の decode を使う。
- **alpha の番号**: alpha.34 の実機の確かめで直しが要れば alpha.36 を使う決まり。R2b-3 の版の番号はその時点で決まる（Q13）。

## 15. 既存の文書の直し（3f で）

1. refactor-plan §4 の R2b-3 の行と状態の行: PR 列と完了を書く。§7 Q4 の「エンジンはポートごとの直列化」に Q9 の決定を足す。
2. api-design §0 の 4・§4.4（「R2b-3 で `Frame` に載せ替えて消す」→ `Session` を経て `Frame` へ）・§4.2 の「エンジンが R2b-3 で実装する契約」（実装済み、置き場所は `banto-driver-engine`）・§6.2 の表の R2b の列（`DRIVERS` の表は消えた、`default_client_factory`・`ClientProtocol` も）・§7.3（C9〜C11・C16 が乗った、`LegacySession` は消えた）・§9.1（0.2.0 と公開）・§13 の 5（文字列の decode をエンジンへ揃えた）。
3. address 設計 §4.2 の図（「R2b-3 の後: `TagSpec` をそのまま」を実装済みに）と grep の例外の行。
4. connection-settings 設計 §3.5 の `$secret`（接続にだけ開封して渡す、を実装した場所）と §6 の「broker・ドライバの `dial` は R2b-3 で `Settings` を直接使う」。
5. plugin-design §12.6（CI の対象）と §6.2 の WIT（変更があれば）。
6. tag-server-design §3.4 / §3.4.1（ドライバの置き場所、`tyaro/slmp` のワイヤ層の扱い）。
7. `crates/banto-driver-api/src/{lib,registry,address}.rs` の module doc の「R2b-3 まで」の記述（各 PR で直す）。
8. 運用ガイドの接続の節: シリアル（COM の排他・名前・時間切れ）と接続テストの文言。
