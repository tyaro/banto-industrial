# banto-hub シミュレーションを値の層へ 詳細設計（R2b-5）

作成日: 2026-10-23
最終更新: 2026-10-23（オーナーのレビューの P2 を反映: 型ごとのセル・文字列の末尾を残す・エンコーディングでの長さ）
状態: **設計確定（2026-10-23 オーナー決定。§11 の Q1〜Q11 は推奨どおり）。実装中: 5a（#806）・5b 済み、5c・5d は未着手**。コード調査（§1）は 2026-10-23 の main（`b7234a51`、v0.2.0-alpha.37、R2b-3 完了の後）に対して行った。前提の決定: R2b-4（接続ごとの実装の選択）は W 段へ後回し（2026-10-23 オーナー判断）、シリアル接続は #803 で凍結（本書はシリアルに依存しない）、使用者はまだいない（既存データの移行・REST / MCP / 設定パッケージの旧い形との互換は考えない。2026-10-22 オーナー決定）。
対象: [banto-hub-driver-refactor-plan.md](banto-hub-driver-refactor-plan.md) §4 の R2b-5「シミュレーションを値の層へ移す（[banto-hub-driver-plugin-design.md](banto-hub-driver-plugin-design.md) §12.2 の 6）」: プロトコル別のシミュレータの代わりに「正規化アドレス → 値」を持つ汎用の `SimSession` を 1 つ置き、WASM を含むすべてのドライバをそのままシミュレーションでき、#363（書き込みはシミュレータへ反映）も成り立たせる。シミュレーションはワイヤの処理を通らなくなるので、ワイヤのテストはドライバ側の CI で行う。
関連: [banto-hub-driver-io-design.md](banto-hub-driver-io-design.md)（R2b-3。§4.4「シミュレーション（変えないが、ダイヤルの材料が変わる）」、§8 ChronoGazer）、[banto-driver-api-design.md](banto-driver-api-design.md)（§5 セッション型、§6.2 の「R2b の後の注記」、§7.4 の `FakeSessionDriver`）、[ux-plan.md](ux-plan.md) §1（T9 接続単位のシミュレーション: ランプ波・トグル・事故防止の 3 点）、[implementation-checklist.md](implementation-checklist.md)。

---

## 0. 要約

| #   | 提案                                                                                                                                                                                                                                                                                           | 根拠の節 |
| --- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------- |
| 1   | 公開 crate `banto-driver-engine` に **`sim` モジュール**を足す: 接続 1 本ぶんの値の表 `SimDevice`（`canonical` → セル）と、それを読む / 書く `Session` の実装 `SimSession`。ドライバを一切呼ばない（`TagSpec` が運ぶ `DriverAddress` と `TagKind` だけで足りる）                               | §3       |
| 2   | broker のダイヤルに **「シミュレーションの接続」の口**を足す（`ensure_simulated_connection(conn, device)`）。`BrokerSimRegistry::resolve` は `host` / `port` を差し替える代わりに `DialTarget::{Real(Settings), Sim(Arc<SimDevice>)}` を返す。設定の検証と指紋の約束（変われば張り直し）は保つ | §4       |
| 3   | 値の生成は**読まれたときに時刻から計算**する（ランプ波・トグル・文字列）。背景のタスクと「先頭 16 番地」の窓を無くす。書かれたセルは held として生成を止める（#363 の約束はそのまま）                                                                                                          | §3.2     |
| 4   | **カバレッジのプリフライト**（`classify_plc_tag`・`GET /api/collection/simulation-coverage`・ChronoGazer の #413 の一覧）は、全タグが対象になるので**消す**                                                                                                                                    | §5, Q5   |
| 5   | シミュレーションの対象を「名前が `modbus-tcp` / `slmp`」から「**レジストリの Frame / Session のドライバ全部**」にする（all-simulation の上書き・`/api/v1/tags` の実効の判定・収集の計画の除外）。WASM のドライバもそのまま対象になる                                                           | §5       |
| 6   | `banto-plc` のワイヤのシミュレータ（Modbus / SLMP）は**本番の経路から外れ、テストと `dev_plc` のためだけに残る**。ランプ波の生成はそこへ付け替える。ドライバ crate の P0-3・適合テストは変えない                                                                                               | §6, Q6   |
| 7   | PR は **5a（`SimSession`）→ 5b（broker と登録簿の切り替え、Hub / 収集 / ChronoGazer のテスト）→ 5c（本番のシミュレータとカバレッジの削除）→ 5d（docs・CHANGELOG・版）**。5a・5c・5d は sonnet 1 体で足りる大きさ                                                                               | §9       |

### 0.1 既存の決定・計画から外れる点

| 既存の決定・計画                                                                                                        | 本書の提案                                                                             | 理由                                                                                                                    |
| ----------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------- |
| ux-plan §1「先頭 16 番地だけランプ波・トグル」「文字列は対象外」、T15-2 のカバレッジのプリフライト（desktop-plan §9.7） | 全タグが対象（文字列も）。プリフライトは消す                                           | 値の層では「どの番地が窓に入るか」という概念そのものが無い。残すと常に「未対応 0 件」を返す口になる                     |
| ux-plan §1「収集のコードパスは一切変更しない」（シミュレータは実 TCP で、ドライバは相手を区別しない）                   | シミュレーションはワイヤを通らない（plugin-design §12.2 の 6 のとおり）                | 2026-10-19 の方針。ワイヤの確かめはドライバ crate の P0-3・適合テスト（§6）                                             |
| io-design §4.4「ダイヤルの材料（`host` / `port`）を差し替える」                                                         | 差し替えない。broker に「このデバイスで張る」口を足す                                  | 差し替えは「ワイヤのシミュレータが TCP で居る」前提。値の層ではダイヤルする相手が無い                                   |
| api-design §7.4 の `FakeSessionDriver`（conformance の中、テストの道具）                                                | 本番の `SimSession` は engine に置き、`FakeSessionDriver` はそのまま（統合しない。Q1） | conformance は dev 用の crate。本番の Hub が依存するのは筋が悪い。形は似るが `fail_next` などテストの鉤は本番に要らない |

## 1. 前提（コードの現状、2026-10-23 の main `b7234a51` に照合）

行番号は目安。着手時に `rg -n 'BrokerSimRegistry|simulator_protocol|classify_plc_tag|RAMP_ADDRESS_COUNT|"modbus-tcp" \| "slmp"' --type rust apps crates` で再確認する。

### 1.1 シミュレータの選び方とダイヤル

- `crates/banto-collect/src/broker_sim.rs:62-200` `BrokerSimRegistry`: 接続 id ごとに高々 1 つの `SimulatorHandle` と、直近のダイヤルの指紋を持つ。`resolve(conn, registry)`（128 行）は `conn.simulation` のとき `simulator_protocol(&conn.protocol)`（`config.rs:1549-1555`、`"modbus-tcp"` / `"slmp"` の名前で選ぶ）でシミュレータを起動し、設定の `host` / `port` をそのループバックのアドレスに差し替える。シミュレータの無いプロトコルは `None`（セッションを張らない）。指紋 `dial_fingerprint`（44 行）が前回と違えば `changed = true` で、呼び手が古いセッションを捨てる。
- 呼び手は 2 か所: 収集の `crates/banto-collect/src/sessions.rs:91-150` `BrokerSessions::sync`（ChronoGazer もこれを使う。`apps/chronogazer/core/src/collect.rs:2951-2960`）と Hub の `apps/banto-hub/core/src/hub.rs:2072-2082` `sync_broker_sessions_from`。どちらも `resolve` → `changed` なら `remove` → `ensure_connection_with_settings(conn, settings)`（`crates/banto-broker/src/lib.rs:893-950`）の順。
- broker はダイヤルの相手を知らない: `resolve_session_driver`（`lib.rs:497-510`）がレジストリの `Frame` を `FrameDial`（`session.rs:100-170`）で包み、`validate_config` と `frame_transport`（TCP だけ）を通してエンジンの `FrameSession::connect` へ。シミュレータは TCP の相手として見えているだけ。
- 計画も名前で除外する: `config.rs:1470-1482` `resolve_connection` は `conn.simulation && simulator_protocol(..).is_none()` を `ExclusionReason::UnsupportedProtocol` にする。`Protocol` enum（`config.rs:64-80`）は「シミュレータの種類」と「保存時の旧規則 `check_tag_address` の記法」の 2 役。

### 1.2 ワイヤのシミュレータと値の生成

- `crates/banto-plc/src/modbus/simulator.rs`（1008 行）・`slmp/simulator.rs`（906 行）: 実 TCP・実バイト列。読み書きのほか、テストの鉤（`hang` / `inject_exception` / `inject_end_code` / `emit_malformed_frames` / `set_response_chunking` / `hold_next_response` / `write_command_count` など）。#363 の held（ワイヤで書かれた番地は seed の `set_*` が no-op。`modbus/simulator.rs:17-50`）。`crates/banto-plc-write/src/modbus/simulator.rs` は書き込みのテスト用の別の Modbus シミュレータ（FC5/6/15/16 と FC1/3）。
- `crates/banto-collect/src/simulation.rs`（576 行）: `start` / `start_on`（161・180 行）がシミュレータを起動し、`RAMP_PERIOD_MS = 100` ごとに先頭 `RAMP_ADDRESS_COUNT = 16` 番地へランプ波（u16 のラップ）・トグルを書き続ける背景タスクを spawn する。文字列は対象外。`classify_plc_tag`（310 行）は T15-2 のカバレッジの判定で、`banto_plc::Address::{parse,parse_slmp}` を名前で選ぶ（R2b-2 の後も残る名前の分岐）。
- `banto-collect` は `banto-plc = { features = ["simulator"] }` に通常の依存（`Cargo.toml:39`）。ドライバ crate は dev-dependency として `banto-plc` / `banto-plc-write` の `simulator` feature（`crates/banto-driver-{modbus,slmp}/Cargo.toml:39-46`）。

### 1.3 Hub の面

- all-simulation: `apps/banto-hub/core/src/controller.rs:96-105` `RunMode::AllSimulation`。`hub.rs:226-240` `runtime_snapshot_for_mode` が **`"modbus-tcp" | "slmp"`** の有効な接続に `simulation = true` を上書き。REST `POST /api/collection/start-all-simulation`（`rest/collection_control.rs:102・267`）。
- カバレッジのプリフライト: `hub.rs:1082-1160` `SimulationCoverageReport` / `simulation_coverage_report_from`（名前の分岐 1125 行）→ REST `GET /api/collection/simulation-coverage`（`collection_control.rs:238・280`）、テスト `apps/banto-hub/core/tests/t15_simulation_coverage.rs`。**UI は呼んでいない**（`apps/banto-hub/src` に `simulation-coverage` の参照なし）。
- 実効のシミュレーション: `rest/tag_space.rs:119-130` が `matches!(protocol, "modbus-tcp" | "slmp") && (configured_simulation || mode == AllSimulation)`。`/api/status` の接続の `simulation` / `configured_simulation` / `effective_simulation`（`rest/admin_status.rs:88-102`）。#335: 読み取り出力（`value_source`）はシミュレーションで塞がない。
- 書き込み（#363）: `write_path.rs:521-560` `WriteTarget::{Plc,Simulator}` は監査の `detail` にだけ使う（`{"target":"simulator"}`）。関門の順は変えない。書き込み自体は broker のセッション経由なので、相手がワイヤのシミュレータでも値の層でも同じ道。
- 接続テスト: `rest/plc_connections.rs:784-792` フォームの `simulation = true` は `ok:false` の 200 で断る（ux-plan §4）。MCP の `tool_test_connection`（`mcp.rs:2403-2412`）はこれに委譲。
- 警告: `hub.rs:2199` `log_simulation_warnings`（有効なシミュレーション接続を起動時と rebuild のたびに列挙。ux-plan §1 の事故防止）。
- UI: 接続 Drawer のチェックボックス（`ConnectionDrawer.svelte:910`）、`⚠ シミュレーション中`（1265 行）、状態ページの全シミュレーションの表示（`status/+page.svelte:399-428・1008`）、設定パッケージの `simulation`（`configPackage.ts:431・739`）。いずれも接続の `simulation` フラグだけを見る。

### 1.4 ChronoGazer

- 収集は `banto_collect::BrokerSessions` を共有（§1.1）。`simulation = true` の値は tstore に記録しない（`crates/banto-collect/src/task.rs:836` `if ctx.simulation { return; }`。#413）。
- `apps/chronogazer/core/src/simulation.rs` は `classify_plc_tag` で「値が動かないタグ」を `/tags` の一覧へ出す（REST `rest.rs:1383`）。
- `apps/chronogazer/core/examples/dev_plc.rs`: ランプ波のワイヤのシミュレータを**別プロセス・固定ポート**で立て、ChronoGazer からは普通の接続として登録する（E2E の `webServer`、`pnpm dev:plc`）。本番コードはこれを知らない。

### 1.5 ワイヤのシミュレータに頼るテスト

| 層                                                                                                                                                                                                                                                                                                           | 何に頼るか                                                                                                                | R2b-5 の後                                                                                                     |
| ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------- |
| ドライバ crate: `crates/banto-driver-{modbus,slmp}/tests/{p0_3,conformance,frame_conformance,broker}.rs`                                                                                                                                                                                                     | `banto_plc::{modbus,slmp}::simulator` の鉤（hang・分割・不正フレーム・終了コード）。P0-3 の 6 本                          | **変えない**（ワイヤの CI の居場所。plugin-design §3.4.1 の決まり 3）                                          |
| `crates/banto-collect/tests/integration.rs`（`sim.hang()` 884・1115・1423 行）、Hub の `host/tests/{integration,t8_bit_access,write,t15_write_peek,t20_*,grpc,mcp,mqtt,stream,soak,t7,t12,broker_glue}.rs`                                                                                                   | `simulation = false` の接続をシミュレータのアドレスへ向け、「そこに居る PLC」として読む。`write_command_count` などで観測 | **変えない**（Q7）。dev-dependency のまま。ランプ波が要るテストは §6 の付け替え先を使う                        |
| `simulation = true` で `BrokerSimRegistry` のシミュレータに頼るテスト: Hub の `t9_simulation.rs`・`t9_simulation_write.rs`・`computed.rs`・`write.rs`・`connection_validation_consistency.rs`、ChronoGazer の `simulation_not_recorded.rs`・`collect.rs` の単体、`banto-collect/tests/integration.rs` の一部 | 値が動く・書いた番地が held・隣は動き続ける・`simulation: true` の報告                                                    | **5b で `SimDevice` の観測に書き換える**（値が動く → 読み戻し、held → `SimDevice::get`）。約束の集合は変えない |
| `apps/banto-hub/core/tests/t15_simulation_coverage.rs`、ChronoGazer の `simulation.rs` のテスト                                                                                                                                                                                                              | カバレッジの判定                                                                                                          | 5c で消す（Q5）                                                                                                |
| ChronoGazer の E2E・`dev_plc`、`tests/collect_roundtrip.rs`                                                                                                                                                                                                                                                  | 別プロセスのワイヤのシミュレータ（記録まで一巡）                                                                          | 変えない。`dev_plc` は §6 の付け替え先を呼ぶ                                                                   |

## 2. 範囲: R2b-5 で変えるもの・残すもの

変える: シミュレーションの実体（ワイヤ → 値の層）、broker のダイヤルの口、`BrokerSimRegistry` の返り値、シミュレーションの対象の決め方（名前 → レジストリ）、カバレッジのプリフライトの削除、`banto-collect` の `simulation` モジュールの削除、`banto-collect` → `banto-plc` の通常の依存の削除。

残す（変えない）: 接続の `simulation` 列と REST / MCP / 設定パッケージ / UI の形、`RunMode::AllSimulation` と REST の口、書き込みの関門の順と `WriteTarget` の監査、`task.rs` の「記録しない」約束（#413）、接続テストの「シミュレーションは断る」（Q8）、`log_simulation_warnings`、ドライバ crate のワイヤのシミュレータとテスト、`dev_plc`。保存時の旧規則 `check_tag_address`（`Protocol` の残る役。R2b-7）。

やらない: 障害の注入（切断・時間切れ・不良品質）を本番の UI / REST から操る口（今も無い。テストの鉤は `SimDevice` に `#[doc(hidden)]` で置くだけ）、波形の種類の設定（ux-plan「凝りすぎない」）、タグ同士の重なり（i32 の 2 語目と隣の u16）の再現（§3.3）。

## 3. `SimDevice` と `SimSession`（5a）

置き場所: `crates/banto-driver-engine/src/sim.rs`（Q1）。engine は公開 crate で `banto-tags` に依存せず、`Session` を実装する部品がすでにある。`banto-broker`・`banto-collect`・Hub・ChronoGazer のどれからも使える。

### 3.1 値の表

```rust
pub struct SimDevice { inner: Arc<Mutex<SimState>>, epoch: Instant }
struct SimState { cells: HashMap<String, Vec<(TagKind, SimCell)>>, writes: u64 }   // canonical → 種類ごとのセル
struct SimCell { held: Option<Value>, index: u32 }   // index: 初めて見た順（位相に使う）
```

- 鍵は **`DriverAddress::canonical` そのまま**（Q3）。ドライバが 1 アドレスに 1 つの正規形を約束している（api-design §3.3）ので、同じタグをどのセッション・どの再接続から見ても同じセル。`driver` は 1 接続 = 1 ドライバなので鍵に入れない。
- **セルは `(canonical, TagKind)` で分ける**（2026-10-23 オーナーのレビュー #805 の P2 2）: 同じ `D100` を `I16`・`F32`・文字列で登録できる（SLMP の型別アドレス検査、PLC タグに番地の一意の制約は無い）。canonical だけで共有すると、文字列で書いた `D100` を数値で読んで `Value::Str` が返る。種類（データ型・文字列の語数・エンコーディング）が違えば別のセルにし、要求と違う種類の held を正常な応答として返さない。タグの型や文字列長を変えた後は新しいセル（生成値）になり、前の種類の held は見えなくなる（語の重なりの再現はしない、Q3）。
- `SimDevice` は `Clone`（`Arc` の共有）。寿命は `BrokerSimRegistry` が持つ（§4.2）: セッションが切れて張り直されても表は残る（今のシミュレータのインスタンスと held の寿命と同じ）。接続の `simulation` を切る・接続を消す・all-simulation を止めると捨てる（held も消える。`simulation.rs:55-60` の約束のまま）。
- 観測の口（テストと将来の UI）: `get(canonical, &TagKind) -> Option<Value>`（held だけ。生成値は返さない）、`set(canonical, TagKind, Value)`（held にする）、`write_count()`、`held_count()`。

### 3.2 読み取り: 読まれたときに生成

`SimSession::read_batch(specs)` は spec ごとに、held があればそれ、無ければ `TagKind` と経過時間 `t = epoch.elapsed()` から:

- `Numeric(Bit)` と `bit: Some(_)` のアドレス: `((t / 100 ms) + index) % 2 == 1` のトグル。
- `Numeric(整数)`: ランプ波 `((t / 100 ms) + index * 16) % 範囲` を型の範囲に収めた `Value::F64`。u16 なら今と同じラップ。符号付きは 0 から正の側だけ（今の u16 のランプと見た目を揃える）。
- `Numeric(f32 / f64)`: 同じランプを `f64` に。
- `Str { words, .. }`: `format!("SIM-{index}-{phase}")`（`phase` は数値と同じ `tick + index * 16`。1 語でも同じ tick の隣のセルと値が違う。2026-10-23 Codex の P2）の**末尾** `words * 2` バイトを残す（ASCII だけなので encoding は問わない）。末尾は tick の下の桁なので、最短の 1 語（`"23"` のような 2 桁）でも tick ごとに変わる（2026-10-23 オーナーのレビュー #805 の P2 1: 先頭から切ると 1〜2 語で値が固定される）。今まで対象外だった文字列が動く。

生成は決定的（時刻と index だけ）なので、同じ 100 ms の間に 2 度読めば同じ値、`on_change` の購読は 100 ms ごとに発火する（ux-plan §1 の「固定値では購読が発火しない」に応える）。背景タスクは無い（Q4）。

### 3.3 書き込み: #363 の約束

`write_batch(items)` は item ごとに:

1. `spec.address.writable_area == false` → `ItemError::Rejected("この領域は書き込めません")`（実機なら `Device` の例外。入力レジスタ・接点入力・SLMP の `X` など）。
2. 値の種類が `kind` と合わない（`Bit` に `F64` など）→ `ItemError::Rejected`。整数型は範囲の検査（api-design §9.2 のドライバの仕事を値の層で代行。範囲外は `Rejected`）。有限でない数（NaN・±inf）はどの型でも、整数型への小数も `Rejected`（Hub の値の検査 `write_path.rs` と同じ）。文字列は**タグのエンコーディング（UTF-8 / Shift-JIS）で符号化したバイト数**が `words * 2` を超えるもの、そのエンコーディングで表せない文字を含むものを `Rejected`（Hub の関門と `encode_string_value` と同じ。2026-10-23 オーナーのレビュー #806 の P2 3）。
3. 通れば held に置き、`writes += 1`。`Ok(())`。

**held は「実機で書いて読み戻したときの値」にする**（2026-10-23、Codex の P2 が同じ形で続いたので、実機の経路とのずれを総点検した。#806）:

- 数値: f32 は f32 に丸めた値（f32 で無限大になる有限の値は `Rejected`）。整数型は整数への変換を通した値（`-0.0` → `0`）。
- 文字列: タグのエンコーディングで符号化し（容量は途中・末尾の NUL も含めて数える）、`decode_string` で戻した値。途中に NUL があれば最初の NUL まで（`"A\0B"` → `"A"`）、Shift-JIS で往復すると変わる文字（`¥` → `\`）も戻した側。空文字列・容量ちょうど・末尾の空白はそのまま。
- `bit: Some(_)` のアドレスで種類が `Bit` でないもの（ドライバの `parse_address` が登録時に断り、計画では `Unsupported`）は、読みも書きもその項目だけ `ItemError::Unsupported`。
- 直さないもの: 断りの変種（実機のドライバは種類違い・範囲外を `Unsupported` にするが、Hub の関門が先に止めるので経路に現れない。シミュレータは `Rejected` にそろえる）。canonical の解釈が要るもの（ビットデバイスとワードデバイスの区別・番号の上限・ドライバごとの型の対応・語の重なり / RMW）は `parse_address` と適合テストの仕事で、値の層は解釈しない。

RMW（SLMP の bit-in-word、`crates/banto-driver-slmp/src/frame_conn.rs:32-45`）は値の層に無い: `D100.5` は `D100` と別のセル（Q3）。多語の重なりも再現しない。これらの再現はドライバ crate の P0-3・C13〜C15 で足りている（§1.5）。Hub の関門（`writable`・スコープ・レート・値検査・監査の先書き・`write_enabled`）は broker の手前なので変わらない。

### 3.4 セッションの約束

- `SimDriver`（`SessionDriver`、engine の中だけ。レジストリには**登録しない**）: `connect` は即 `Ok(Box<SimSession>)`。`disconnect` は no-op。致命は返さない（再接続の経路に入らない）。`FatalError` を返す鉤はテスト用に `SimDevice::fail_next(FatalError)` を `#[doc(hidden)]` で置く（broker の再接続のテストが `FakeDriver` ではなく本番の経路で書けるように）。
- `read_batch` は `specs.len()` 個を順に返す（契約どおり）。空の `specs` は空。
- 時間の上限（io-design §5.5）は broker の job loop がそのまま掛ける。`SimSession` は待たないので影響しない。

### 3.5 適合テスト

`banto-driver-conformance` の `session::run_all_against` のうち、ワイヤに依らない項目（読みの順序・空の要求・書いて読み戻す・`Rejected` の形）を `SimDriver` に対して走らせる単体テストを engine に置く（conformance は dev-dependency。循環しない: conformance → engine は既にある依存なので、engine の dev-dependency にはできない。**代わりに engine の `tests/sim.rs` に同じ項目を書き写す**か、conformance 側の `tests/` に `SimDriver` を対象にしたテストを足す。後者を推奨）。

## 4. broker と登録簿の切り替え（5b）

### 4.1 broker: シミュレーションの接続を張る口

```rust
impl SessionDirectory {
    pub fn ensure_simulated_connection(&self, conn: &PlcConnection, settings: Settings, device: SimDevice)
        -> Result<BrokerHandle, BrokerError>;
}
```

- `ensure_connection_with_settings` と同じ前半（レジストリに Frame / Session が無ければ `UnsupportedProtocol`、`validate_config` が断れば `InvalidSettings`。**シミュレーションでも設定の検証は通す**: 「シミュレーションで動いたのに実機で動かない」を減らすのが T9 の趣旨）。後半は `FrameDial` の代わりに `SimDriver::connect` の connector を spawn する。`connect_limit` はセッション型の値。
- 指紋: `SettingsFingerprint::of_connection` に **「シミュレーションである」印**を混ぜる必要がある（今は `host` / `port` の差し替えが指紋を変えていた。`lib.rs:416-450` は `protocol`・設定・秘密の参照しか見ない）。`of_connection_simulated(..)`（または `simulated: bool` 引数）で `"\u{0}sim"` を hash に足す。`dial_fingerprint`（`broker_sim.rs:44`）も同じ印を使い、`CollectorConfig::set_broker_dial_target` に渡す値と broker のハンドルの値が一致し続けるようにする。
- `BrokerConnectionStatus` は `Connected` になる。`/api/status` の接続の状態は今と同じ見え方（今もシミュレータへ繋がれば `connected`）。

### 4.2 `BrokerSimRegistry::resolve` の返り値

```rust
pub enum DialTarget { Real(Settings), Sim(Settings, SimDevice) }
pub async fn resolve(&self, conn, registry) -> (DialTarget, bool /* changed */)
```

- `Option` を外す: シミュレータの無いプロトコルという概念が無くなるので、`None` の枝（`broker_sim.rs:140-150`、`sessions.rs:108-110`、`hub.rs:2072-2078`）を消す。
- `conn.simulation` のとき、接続 id の `SimDevice` を作る（無ければ）か再利用し、`Sim(settings, device)`。**再利用は同じ `protocol` のときだけ**: 登録簿は `SimDevice` と一緒に作ったときの `protocol` を持ち、接続の `protocol` が変わっていれば古い `SimDevice` を捨てて作り直す（前のドライバの held が、正規形の重なる新しいドライバのタグへ漏れないように。2026-10-23 Codex の P2）。5b のテストで「プロトコルを変えると held が消える」を確かめる。`false` なら持っていた `SimDevice` を捨てて `Real(settings)`。
- `changed` の意味はそのまま: 指紋（§4.1 の印つき）が前回と違う。シミュレーションの on / off・設定の変更・秘密の参照の差し替えで `true`。
- 呼び手（`sessions.rs:91-150`・`hub.rs:2072-2082`）は `match target { Real(s) => ensure_connection_with_settings, Sim(s, d) => ensure_simulated_connection }`。`remove` / `shutdown` の順（セッションを止めてから）はそのまま。
- `banto_collect::simulation` への依存が `broker_sim.rs` から消える。名前 `BrokerSimRegistry` は残す（#339 で付けた名、呼び手が多い）。

### 4.3 「シミュレーションの対象」をレジストリで決める

名前の分岐を 4 か所消し、`banto_broker::is_broker_managed(registry, protocol)` に替える:

- `hub.rs:236` `runtime_snapshot_for_mode`（all-simulation の上書き）。
- `hub.rs:1125`（カバレッジ。5c で関数ごと消える）。
- `rest/tag_space.rs:129`（実効のシミュレーション）。
- `config.rs:1470-1482` `resolve_connection`: `conn.simulation && simulator_protocol(..).is_none()` の除外を消す。`Plc(Option<Protocol>, ..)` の `Protocol` はアドレスのエラー文言の記法にだけ使われているので、記法の選び方は保存時の旧規則と同じ `Protocol`（`config.rs:1723`）に揃える（R2b-7 でまとめて消える）。

`push` / `postgres` / `virtual`（`HostProvided`）は `is_broker_managed` が `false` なので対象外のまま（今の名前の分岐と同じ集合。差は「将来の Frame / Session のドライバ（WASM 含む）が自動的に対象になる」ことだけ）。

### 4.4 テストの書き換え（5b の中）

§1.5 の 3 行目のテストを `SimDevice` の観測に替える。`t9_simulation_write.rs` の 5 点は: 拒否されない（そのまま）・シミュレータに届く（`device.get(canonical) == Some(値)`）・held（もう一度読んで同じ値）・隣は動く（別のタグを 2 回読んで違う）・監査の `target`（そのまま）。Hub のテストが `SimDevice` に届く口: `BrokerSimRegistry::device(connection_id) -> Option<SimDevice>`。

## 5. 本番のシミュレータとカバレッジの削除（5c）

- `crates/banto-collect/src/simulation.rs` を消す（`start` / `start_on` / ランプのタスク / `classify_plc_tag` / `SimulationCoverage` / `RAMP_*`）。`lib.rs:82-104` の再公開、`config.rs:1549` `simulator_protocol`、`Cargo.toml:39` の `banto-plc` の通常の依存を消す（dev-dependency は残す。§1.5 の 2 行目）。
- Hub: `SimulationCoverageReport` / `simulation_coverage_report*`（`hub.rs:1082-1160`）、REST `GET /api/collection/simulation-coverage`（`collection_control.rs:51・232-245・280`）、`tests/t15_simulation_coverage.rs`、OpenAPI / MCP に載っていればその項（grep `simulation-coverage|simulation_coverage`）。UI は呼んでいないので触らない。
- ChronoGazer: `core/src/simulation.rs` と REST `rest.rs:1383` の口、画面の「値が動かないタグ」の一覧（Tauri のコマンド `simulation_coverage_list`、`apps/chronogazer/src/lib/banto/tagRegistryAdmin.ts:328-330` の `listSimulationCoverage` とそのテスト、`routes/(app)/tags/+page.svelte:1795` の `.simulation-coverage`。#413 の一覧の表示だけを消す。`simulation: true` のバッジと「値は記録されません」は残す）。
- `dev_plc` と、ランプ波が要るワイヤのテストの付け替え（§6）。
- 文書: `simulation.rs` を指すコメント（`hub.rs:72-102`・`write_path.rs:93`・`task.rs`・`plc_connections.rs:73`・`banto-tags/src/plc_connection.rs` の T9-1 の節）を新しい居場所に直す。

## 6. ワイヤのシミュレータの行き先（Q6）

`banto-plc` の `modbus::simulator` / `slmp::simulator` は `simulator` feature のまま残す（ドライバ crate の P0-3・適合テストのピア。plugin-design §3.4.1 の決まり 3 の「ワイヤのテストはドライバ側の CI」）。`banto-collect/src/simulation.rs` のランプのタスク（`modbus_ramp_task` / `slmp_ramp_task`、先頭 16 番地・100 ms）は、ランプが要る所のために **`banto-plc` の `simulator` feature の中へ `ramp` モジュール**として移す（`start_with_ramp(addr) -> RampHandle`）。使う所: `dev_plc`（固定ポート）、ChronoGazer の `collect_roundtrip.rs`・`simulation_not_recorded.rs` の「対照の実機」、`banto-collect/tests/integration.rs` の値が動くことを見るテスト。

`banto-plc-write/src/modbus/simulator.rs`（書き込みのテスト用の別物）は触らない（R2b-5 の範囲外。重複の整理は P3）。

## 7. ChronoGazer

収集は `BrokerSessions` を共有しているので、5b で自動的に値の層へ移る。`task.rs:836` の「記録しない」約束は変えない。消すのは #413 の「値が動かないタグ」の一覧だけ（§5）。`dev_plc` は §6。保存時のアドレスの規則は R2b-7 のまま。

## 8. 書き込みの安全（tag-server-design §6）

関門の順・監査の先書き・`WriteTarget` の判定（`write_path.rs:538-551`）は変えない。変わるのは関門の先の相手だけ（ワイヤのシミュレータ → `SimSession`）。`SimSession` が `Rejected` を返す条件（§3.3）は実機のドライバより狭い（領域・型・範囲だけ）ので、「シミュレーションでは通ったが実機で断られる」書き込みは残る（例: SLMP の局が書き込みを禁じている）。これは今も同じ（シミュレータは終了コードを返さない）。

## 9. PR の分け方

共通: 検証の順は checklist §2（`cargo fmt --all --check` → 対象 crate のテスト → clippy → 公開 API の形を変える PR は `cargo check --workspace --all-targets` → prettier → `cargo deny check`）。migration は無い。どの PR も revert で戻る。`cargo update` はしない（`encoding_rs` 0.8.35、MSRV 1.85）。

| PR  | 範囲                                                                                                                                                                                                                                                                                    | 主な触るファイル                                                                                                                                                                                                                                                                                                          | 監査の焦点                                                                                                                                                                               | モデル                |
| --- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------- |
| 5a  | `banto-driver-engine::sim`（`SimDevice`・`SimSession`・`SimDriver`、§3）と単体テスト・conformance の `tests/` に `SimDriver` 向けの項目（§3.5）。配線はしない（振る舞いは変わらない）                                                                                                   | 新 `crates/banto-driver-engine/src/sim.rs`、`lib.rs`、`crates/banto-driver-conformance/tests/`、engine の CHANGELOG                                                                                                                                                                                                       | 生成が決定的で 100 ms ごとに変わるか。held が再接続をまたいで残るか。`Rejected` の条件が §3.3 の集合か                                                                                   | sonnet                |
| 5b  | broker の `ensure_simulated_connection` と指紋の印（§4.1）、`BrokerSimRegistry::resolve` の `DialTarget`（§4.2）、呼び手 2 か所、名前の分岐をレジストリへ（§4.3）、`simulation = true` のテストの書き換え（§4.4）。`banto-collect/src/simulation.rs` はまだ残す（ランプのテストが参照） | `crates/banto-broker/src/lib.rs`・`session.rs`、`crates/banto-collect/src/{broker_sim,sessions,config}.rs`、`apps/banto-hub/core/src/{hub,rest/tag_space}.rs`、`apps/banto-hub/host/tests/{t9_simulation,t9_simulation_write,computed,write,connection_validation_consistency}.rs`                                        | 指紋: シミュレーションの on / off で必ず張り直すか。`remove` の順（セッションの後に `SimDevice`）。all-simulation の対象の集合が今と同じか（`push` / `postgres` / `virtual` が入らない） | 上位（実装は sonnet） |
| 5c  | 本番のシミュレータとカバレッジの削除（§5）、ランプの付け替え（§6）、`dev_plc`、コメントの直し                                                                                                                                                                                           | `crates/banto-collect/src/{simulation,lib,config}.rs`・`Cargo.toml`、`crates/banto-plc/src/{modbus,slmp}/` と新 `ramp.rs`、`apps/banto-hub/core/src/{hub,rest/collection_control}.rs`・`tests/t15_simulation_coverage.rs`、`apps/chronogazer/core/{src/simulation.rs,src/rest.rs,examples/dev_plc.rs}`、ChronoGazer の UI | 消した後に `"modbus-tcp" \| "slmp"` の分岐が残っていないか（grep）。`banto-collect` が `banto-plc` に通常の依存をしていないか。E2E の `dev_plc` が動くか                                 | sonnet                |
| 5d  | docs（本書の `状態:`・refactor-plan §4・plugin-design §12.2 の 6・api-design §6.2 の注記・ux-plan §1 の注記・README）、CHANGELOG、版（banto-hub alpha.38、engine 0.2.0: `sim` の追加は破壊ではないが minor）                                                                            | `docs/`、`CHANGELOG.md`、各 `Cargo.toml`                                                                                                                                                                                                                                                                                  | ヘッダの `状態:` と本文の矛盾（checklist §4）                                                                                                                                            | haiku / sonnet        |

## 10. テスト

### 10.1 完了条件

- Hub: `t9_simulation.rs`・`t9_simulation_write.rs`・`computed.rs`・`write.rs`・`connection_validation_consistency.rs` が `SimDevice` の観測で通る。`simulation = true` の Modbus / SLMP の接続で **ワイヤのソケットが開かない**こと（`SessionDirectory` に TCP の相手が無い。`modbus_collection_uses_exactly_one_socket` と同じ数え方で 0 本）。
- 文字列タグ・先頭 16 番地の外のタグが `simulation = true` で動く値を返す（新しいテスト 1 本。今まで「未対応」だったもの）。
- 5a の単体テスト: 1・2・3 語の文字列が隣の tick で変わる。同じ canonical の文字列に書いた後で数値として読むと held でなく生成値、held の文字列より短い語数で読むと held を返さない。1 語の Shift-JIS の `"あ"` は通り、1 語の UTF-8 の `"あ"` は長すぎ、Shift-JIS で表せない文字は `Rejected`。
- ChronoGazer: `simulation_not_recorded.rs` の (a)(b)(c) がそのまま通る。
- ドライバ crate の P0-3・適合テストは触らず全部通る。
- 全部の `tests/` で `banto_collect::simulation` の参照が 0。

### 10.2 反証（checklist §5）

5a: 生成の `index` を外す → 「隣は動く」が同じ値になり落ちる。held を外す → `t9_simulation_write` の held が落ちる。5b: 指紋の印を外す → シミュレーションを切っても古い `SimSession` が残り、`t9_simulation` の `simulation: false` へ戻す場面が落ちる。`is_broker_managed` を `true` 固定にする → `push` が all-simulation で `simulation = true` になり `ingest` のテストが落ちる。各 PR で 1 回ずつ確かめて PR に書く。

## 11. オーナー判断（2026-10-23 決定: すべて推奨どおり）

| Q   | 問い                                                                                     | 選択肢                                                                                                                                                                                                         | 推奨                                                                                                                                           |
| --- | ---------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------- |
| Q1  | `SimSession` の置き場所                                                                  | (a) `banto-driver-engine::sim`（公開 crate、`Session` の部品がある）/ (b) `banto-broker` の中 / (c) `banto-collect` の中 / (d) conformance の `FakeSessionDriver` を昇格                                       | **(a)**。broker・収集・Hub・ChronoGazer のどれからも使え、`banto-tags` に依存しない。(d) は dev 用 crate を本番が持つことになる                |
| Q2  | broker にどう「シミュレーション」を伝えるか                                              | (a) `ensure_simulated_connection(conn, settings, device)` の口と `DialTarget` / (b) 設定のキー `simulation = "true"` を broker が解釈 / (c) `SimDriver` をレジストリに登録し `protocol` を差し替える           | **(a)**。設定のキーは `validate_config` と指紋に混ざって紛らわしい。(c) は接続の `protocol` を偽ることになり、`/api/status` などの表示が崩れる |
| Q3  | セルの鍵                                                                                 | (a) `canonical` そのまま（`D100.5` と `D100` は別のセル）/ (b) 語の正規形を鍵にして bit-in-word と多語の重なりを再現（`DriverAddress` に語の正規形を足す。API の変更）                                         | **(a)**。(b) は API 0.2.0 の変更と全ドライバの対応が要り、再現したい振る舞いは P0-3・C13〜C15 で既に固定している。必要になれば後から足せる     |
| Q4  | 値の生成                                                                                 | (a) 読まれたときに時刻から計算（背景タスク無し）/ (b) 今と同じ 100 ms の背景タスクで全セルを更新                                                                                                               | **(a)**。タスクと停止の順序が無くなり、`SimDevice` の寿命管理が単純になる。どの番地も対象にできる                                              |
| Q5  | カバレッジのプリフライト（REST・`classify_plc_tag`・ChronoGazer の #413 の一覧）         | (a) 消す / (b) 口は残し常に「未対応 0 件」                                                                                                                                                                     | **(a)**。UI の呼び手が無く、使用者もいない。残す理由が無い                                                                                     |
| Q6  | ワイヤのシミュレータとランプのタスクの居場所                                             | (a) `banto-plc` の `simulator` feature に残し、ランプを `ramp` モジュールとしてそこへ移す / (b) ランプは `dev_plc` の中にだけ書く（テストは値の動きを見ない形に直す）/ (c) ドライバ crate の `tests/common` へ | **(a)**。`collect_roundtrip` / `simulation_not_recorded` の「対照の実機」と `dev_plc` が共有できる。(c) は 2 crate に写しができる              |
| Q7  | `simulation = false` でワイヤのシミュレータを「そこに居る PLC」とする Hub / 収集のテスト | (a) そのまま残す / (b) `SimDriver` に寄せる                                                                                                                                                                    | **(a)**。これらは Hub を本物のドライバの上で確かめるテストで、R2b-5 の目的と無関係。寄せると監視できる範囲が減る                               |
| Q8  | 接続テストの `simulation = true`                                                         | (a) 今のまま `ok:false` の 200 で断る / (b) `SimSession` で即 `ok:true`                                                                                                                                        | **(a)**。「テスト対象として意味を成さない」は値の層でも同じ。REST / MCP の形を保つ                                                             |
| Q9  | `SimSession` の書き込みの検査                                                            | (a) 領域（`writable_area`）・値の種類・型の範囲・文字列の長さ / (b) 何でも受ける / (c) ドライバの `write_next` を記憶上で回して実機と同じ拒否を得る                                                            | **(a)**。(b) は実機との差が大きすぎる。(c) はフレーム型だけに効き、`write_next` が読み戻しを要る RMW で手順が複雑                              |
| Q10 | all-simulation と実効の判定の対象                                                        | (a) レジストリの Frame / Session 全部（`is_broker_managed`）/ (b) 名前の一覧のまま                                                                                                                             | **(a)**。R2b-5 の目的（どのドライバもシミュレーションできる）そのもの。集合は今と同じで、増えるのは将来のドライバだけ                          |
| Q11 | 版                                                                                       | (a) banto-hub は R2b-5 の最後（5d）で alpha.38、engine は 0.2.0 / (b) PR ごと                                                                                                                                  | **(a)**。「版上げは段の最後でまとめて」（2026-10-22 オーナー決定）                                                                             |

## 12. 確かめきれなかったこと・危険

- `BrokerConnectionStatus` に「シミュレーション」の区別は無い。`/api/status` の `effective_simulation` は接続のフラグと run mode から出しているので表示は変わらないが、broker の状態だけを見る所（ChronoGazer の `collect_events` の `simulation` は `simulated_connection_keys` から。`collect.rs:1313`）に取りこぼしが無いか 5b で grep する。
- `SettingsFingerprint` に印を足す所は、`CollectorConfig::set_broker_dial_target` と `BrokerHandle::settings_fingerprint` の両方が同じ関数を通ることを 5b のテストで固定する（片方だけ変えると、収集のタスクが「replaced」にならず古いハンドルを握り続ける。`broker_sim.rs:104-125` の注意書きの事故）。
- ChronoGazer の `collect.rs` の単体テストに `simulation: true` の行が多い（`collect.rs:3211・5700`）。`BrokerSessions` を経ずに `factory_override` で読んでいるものは値の層に影響されないはずだが、5b で全部回す。
- `dev_plc` の E2E（`e2e/playwright.config.ts` の `webServer`）はビルド済みの example に頼る。5c でランプの居場所を変えたら E2E を 1 回回す（checklist §3）。
- 読まれたときの生成は `Instant` に依る。Hub のテストが `fast_options` で周期を縮めていても 100 ms の刻みは固定なので、「2 回読んで違う」を見るテストは 100 ms 超の間を置く（固定の sleep ではなく条件のポーリング）。

## 13. 既存の文書の直し（5d で）

- refactor-plan §4 の R2b-5 の行と `状態:`、plugin-design §12.2 の 6 に「実装済み（本書）」、api-design §6.2 の注記に `BrokerSimRegistry` の返り値と `SimSession` を追記、io-design §4.4 に「R2b-5 で差し替えをやめた」の注記、ux-plan §1 に「16 番地の窓と文字列の除外は R2b-5 で無くなった」の注記、`docs/README.md` の表に本書、`banto-hub-desktop-plan.md` §9.7（T15-2 のプリフライト）に削除の注記。
