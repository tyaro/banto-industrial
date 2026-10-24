# banto-hub REST / MCP の形の変更と ChronoGazer の追従 詳細設計（R2b-7）

作成日: 2026-10-24
最終更新: 2026-10-24（オーナー決定を反映）
状態: **設計確定（2026-10-24 オーナー決定。§11 の Q1〜Q10 はすべて推奨どおり）・未実装**。コード調査（§1）は 2026-10-24 の main（`33952ad0`、v0.2.0-alpha.39、R2b-6 完了・#821〜#825 マージ後）に対して行った。前提のオーナー決定: **使用者はいない前提で、既存データの移行と旧い形との互換は考えない**（2026-10-22。R2b-7 以降も同じ）・**破壊的変更してよい。新しい形だけ受けて、合わなければ断る**（2026-10-17）・**接続の項目エラーの欄名を R2b-7 で `settings.<key>` に揃える**（2026-10-24、#821）・**シリアル接続（#803）は凍結、R2b-4 と iframe の枠は W 段**。
対象: [banto-hub-driver-refactor-plan.md](banto-hub-driver-refactor-plan.md) §4 の R2b-7「REST / MCP / 設定パッケージの形の変更（設定パッケージは版を上げる）と ChronoGazer の追従」。前の段で R2b-7 へ送った分を §1 で拾う。
関連: [banto-hub-connection-settings-design.md](banto-hub-connection-settings-design.md)（R2b-1。§2 の R2b-7 との境、§4.2 の検証の順、§4.3 平らな形のアダプタ、Q4・Q6・Q13）、[banto-hub-driver-address-design.md](banto-hub-driver-address-design.md)（R2b-2。Q7 ChronoGazer の追従、§5 の前倒しの案）、[banto-hub-driver-io-design.md](banto-hub-driver-io-design.md)（R2b-3。Q5 の「保存時の規則は R2b-7」）、[banto-hub-driver-sim-design.md](banto-hub-driver-sim-design.md)（R2b-5。§5 の `address_notation`・`banto-plc` の依存）、[banto-hub-driver-ui-design.md](banto-hub-driver-ui-design.md)（R2b-6。§4.2 のスキーマでの保存、§5 REST の形、Q2・Q7・Q11）、[banto-hub-mcp-reference.md](banto-hub-mcp-reference.md)（MCP の接続ツール）、[implementation-checklist.md](implementation-checklist.md)。

## 0. 要約

| #   | 提案                                                                                                                                                                                                                                                                                                                                                                                                         | 根拠の節   |
| --- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ---------- |
| 1   | MCP の接続ツール（`list_connections`・`create_connection`・`update_connection`・`test_connection`）を REST と同じ **`{ name, protocol, enabled, simulation, settings }`** の形にし、平らな形のアダプタ `mcp/flat_connection.rs` と `banto_tags::KeepStored` を消す。`update_connection` は REST と同じ「`settings` は丸ごと置き換え、秘密のキーは省略 = 保つ」                                               | §3, Q1, Q2 |
| 2   | 接続の**項目エラーの欄名を全ドライバで `settings.<key>`（スキーマの snake_case）に揃える**（`host`・`port` も `settings.host`・`settings.port`）。`rest_field_error` の camelCase の写しと、UI の `mapSettingsFieldErrors` の `camelToSnake` を消す。`push` は今のまま、`postgres` は触らない（R2b-8 で消える）                                                                                              | §4         |
| 3   | `banto-tags` の**読み取りの平らな欄**（`PlcConnection` の `host`・`port`・`unit_id`・`word_order`・`cpu_type`・`database`・`username`、`legacy::flat_view`）を消し、Hub の `postgres` の読み手（`db_source`・`GET /api/sink/config`）は `settings` から読む。**書き込みの平らな入力（`PlcConnectionInput`・`settings_from_flat`・`PlcConnectionService::new`）はホストの接続とテストのためだけに残す**（Q3） | §5, Q3     |
| 4   | ChronoGazer の host に **`banto_drivers::builtin().registry` を注入**（`PlcConnectionService` / `CollectionGroupService` / `TagService` の `with_registry`）し、保存時の旧規則（`banto_collect::check_tag_address`・`address_notation`・`Protocol`・ChronoGazer の `tag_address.rs` の検査）を消す。`banto-collect` → `banto-plc` の通常の依存を外す                                                         | §6.1, Q4   |
| 5   | ChronoGazer の REST・Tauri の接続の形を Hub と同じ `{ name, protocol, enabled, simulation, settings }` にし、`reject_disallowed_connection_protocol` の固定の許可リストを「レジストリにあるドライバ + `hub`」に替える。`hub` 接続は今のまま（`settings` は `{}`、`idx_plc_connections_single_hub` は変えない）                                                                                               | §6.2, Q5   |
| 6   | ChronoGazer の接続の画面を、Hub の汎用フォーム（`DriverSettingsForm.svelte`・`driverSettingsForm.ts`）の**写しを置いて**組み立てる。ドライバの一覧は ChronoGazer にも `GET /api/drivers` と Tauri コマンド `drivers_list` を足して取る（一覧を組み立てる純関数は `banto-tags` へ移して両アプリで使う）                                                                                                       | §6.3, Q6   |
| 7   | 設定パッケージは R2b-6 で版 3（`settings` の形）になっているので **R2b-7 では版を上げない**。版は 7e で **0.3.0-alpha.1**（connection-settings-design Q13 のとおり。Q7）                                                                                                                                                                                                                                     | §7, Q7     |
| 8   | 持ち越し: `banto-plc` の `#[cfg(test)] decode_string_value` は 7d-1 で消す。上限を超える文字列でエンジンが `Unsupported("計画に含まれない")` と返して理由が落ちる件は、本番のエンジンの変更なので R2b-7 に入れず Issue にする（Q8）                                                                                                                                                                          | §8, Q8     |
| 9   | PR は **7a（MCP）→ 7b（項目エラーの欄名）→ 7d-1（ChronoGazer の保存の規則）→ 7d-2（ChronoGazer の REST・Tauri・UI）→ 7c（`banto-tags` の平らな欄の撤去。読み手が全部移った後）→ 7e（docs・CHANGELOG・版）**。7a と 7d-1 は並べられる                                                                                                                                                                         | §9         |

### 0.1 既存の決定・計画から外れる点

| 既存の決定・計画                                                                                                          | 本書の提案                                                                                                                           | 理由                                                                                                                                                                                                                                                                                                     |
| ------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| refactor-plan §4 R2b-7「設定パッケージは版を上げる」                                                                      | R2b-7 では上げない                                                                                                                   | R2b-6（6b-3、Q11）で版 3 になり、接続は既に `{ name, protocol, enabled, simulation, settings }`（`apps/banto-hub/src/lib/banto/configPackage.ts:65`）。R2b-7 で変える外の形（MCP・ChronoGazer・項目エラー）はパッケージに載らない                                                                        |
| connection-settings-design §2「`banto-tags` の平らな欄を消す」・ui-design §2「平らな欄の撤去（R2b-7）」                   | 消すのは**読み取り**の写し（`PlcConnection` の欄・`flat_view`）。書き込みの平らな入力は、ホストの接続とテストのために残す（Q3）      | ホストの接続（`postgres`・`push`・`virtual`・`hub`）の書き込みは R2b-6b の `settings_input.rs` が `PlcConnectionInput` に写して通している。`postgres` は R2b-8 で消える。テストの作り手（Hub・ChronoGazer・banto-collect で約 60 か所）は旧規則の `new()` に依っていて、書き換えると正規形の前提も変わる |
| connection-settings-design Q6・api-design §6.2「`ALLOWED_PROTOCOLS` は ChronoGazer の旧規則として R2b-7 まで残る → 撤去」 | 本番の入口（Hub・ChronoGazer）は参照しなくなるが、定数は旧規則の `validate_plc_connection_input`（テスト・ホストの接続）のために残る | Q3 と同じ理由。本番の非テストのコードが `PlcConnectionService::new(` と `ALLOWED_PROTOCOLS` を参照しないことを grep のテストで固定する（connection-settings-design §4.2 の Hub の約束を ChronoGazer にも広げる）                                                                                         |
| connection-settings-design §2「REST は R2b-7 で `{driver, settings}` の形へ」                                             | 「`protocol`」の名前のまま（R2b-6 の形）                                                                                             | R2b-6 で `protocol` の名前で実装済み。R2b-7 は MCP・ChronoGazer を同じ形に揃えるだけ                                                                                                                                                                                                                     |
| ui-design Q7「ChronoGazer の接続の UI は R2b-7 で REST と一緒に追従」                                                     | 追従する。形は Hub の汎用フォームの写し（Q6）                                                                                        | 2 つの SvelteKit アプリに共通の社内パッケージが無い（`pnpm-workspace.yaml` は `apps/*` だけ。共有は `@banto/forms` 等の外の banto だけ）                                                                                                                                                                 |

## 1. 前提（コードの現状、2026-10-24 の main `33952ad0` に照合）

### 1.1 Hub の REST（R2b-6 で済んでいる）

- `/api/plc-connections` の入出力は `PlcConnectionPayload { name, protocol, enabled, simulation, settings }`・`PlcConnectionResponse { id, name, protocol, enabled, simulation, settings }`（`apps/banto-hub/core/src/rest/plc_connections.rs:39-48`・`:220-227`）。秘密のキーは応答で `{"$secret": "set" | "unreadable"}`。保存前の接続テスト `PlcConnectionTestPayload`（同 `:739`）も `settings` の形。
- 書き込みの入口は `banto_tags::ConnectionSettingsInput`（`crates/banto-tags/src/plc_connection/settings_input.rs:40-50`）→ `create_settings_tx` / `update_settings_tx`（同 `:102-160`）。機器のドライバは `settings_from_schema` → `validate_config`、ホストの接続（`postgres`・`push`・`virtual`・`hub`）は `settings` を `PlcConnectionInput` に写して旧い検証を通す（同 `:1-20` のモジュールの doc）。
- 設定パッケージは版 3（`configPackage.ts:65`）で、接続は `settings` の形。`checkConfigPackageConnectionFields` が `GET /api/drivers` のスキーマで断る（ui-design §5 の 6b-3 の注記）。

### 1.2 MCP（平らな形のまま）

- `create_connection` / `update_connection` / `test_connection` の `inputSchema` は `host`・`port`・`unitId`・`wordOrder`・`cpuType`・`database`・`username`・`password`・`settings`（push 用）を直下に持つ（`apps/banto-hub/core/src/mcp.rs:596-645`・`:666-730`・`:731-765`）。`update_connection` は全項目指定（`REQUIRED_CONNECTION_KEYS`、同 `:2210-2232`）で、`cpuType` と `settings` だけ省略すると保つ。
- アダプタ `apps/banto-hub/core/src/mcp/flat_connection.rs`（モジュールの doc に「R2b-7 で消える」）: 入力は `PlcConnectionService::validate_flat_input`（旧い検証 (1)。項目名は `unitId` 等）→ `legacy::settings_from_flat` → REST と同じ `PlcConnectionPayload` へ（`into_settings_payload`、同 `:116`）。「省略は保つ」は `banto_tags::KeepStored`（`settings_input.rs:71`。doc に「R2b-7 で消える」）の印で運び、未適用キューにも印のまま積む。応答は `FlatPlcConnectionResponse::build`（同 `:183-210`）が `conn.host` ほかの平らな欄から組む。接続テストは `into_test_payload`（同 `:260`）が保存済みの `wordOrder`・`cpuType` を補う。
- 呼び手のテスト: `apps/banto-hub/host/tests/mcp.rs`（`create_connection` / `update_connection` の参照 71 か所）。

### 1.3 項目エラーの欄名

- ドライバの `FieldError.key`（snake_case）を REST の綴りに写すのは `rest_field_error`（`crates/banto-tags/src/settings_json.rs:99-125`）: `push` は `settings.<key>`（入れ子は `settings.bridge.queries[0].sql`）、それ以外は `unit_id → unitId`・`word_order → wordOrder`・`cpu_type → cpuType`、`host`・`port` はそのまま。テスト `field_keys_are_written_in_the_rest_spelling`（同 `:827`）。
- `settings_from_schema` の型・必須・未知のキーのエラーは JSON のキーのまま（`unit_id`。`settings_input.rs:11-12`）。つまり今は同じ欄に 2 通りの名前（`unit_id` と `unitId`）が返り得る。
- UI の `mapSettingsFieldErrors`（`apps/banto-hub/src/lib/banto/driverSettingsForm.ts:292-310`）が `settings.<key>`・`<key>`・`camelToSnake(<key>)` の 3 通りを欄に当てて吸収している。`ConnectionDrawer.svelte:280-295` は `push`（`mapPushFieldErrors`）・ドライバ・`postgres`（`POSTGRES_SETTING_KEYS`）で分けて呼ぶ。
- 旧い検証 (1) `validate_plc_connection_input`（`crates/banto-tags/src/plc_connection.rs:1150`）は `host`・`port`・`unitId`・`wordOrder`・`cpuType`・`database`・`username` の名前で断る。走るのは、旧規則のサービス（`new()`）の `create` / `update_tx`（同 `:1707`・`:1845`）、MCP の `validate_flat_input`、ホストの接続の `settings_input.rs` の経路。`connection_secret.rs:222` も同じ綴りを使う。
- R2b-1c の一致の表 `apps/banto-hub/host/tests/connection_validation_consistency.rs` は (1) と (4) が同じ集合を断ることを固定している（connection-settings-design §4.2）。

### 1.4 `banto-tags` の平らな欄

- 読み取り: `PlcConnection` は `settings` に加えて `host`・`port`・`unit_id`・`word_order`・`cpu_type`・`database`・`username` を持ち（`plc_connection.rs:857-872`）、`FromRow` → `from_settings` が `legacy::flat_view`（`settings_json.rs:428`）で作る。
- 読み手（非テスト、`banto-tags` の外）: Hub の MCP のアダプタ（§1.2）、Hub の `postgres`（`db_source/mod.rs:510-526`・`:648-664`、`db_source/plan.rs:318-437`、`rest/sink.rs:386-394` の `GET /api/sink/config`）、ChronoGazer（`rest.rs:1029-1046` の応答、`hub.rs:4825-4830`・`hub_tags.rs:677-690` の `hub` 接続の更新で既存の行から `PlcConnectionInput` を組み直す）。`banto-collect` は `settings_to_api(&conn.settings)` で読む（`config.rs:1478`）。`GET /api/ingest/bridge-config` は `push` の `settings` を読む（平らな欄は使わない）。
- 書き込み: `PlcConnectionInput`（`plc_connection.rs:1003`）を組む非テストのコードは、Hub の MCP のアダプタ（§1.2）と `settings_input.rs` のホストの接続の経路、ChronoGazer（`rest.rs:940-965` の `into_input`、`hub_tags.rs:452-466` の「Hub から追加」が作る `hub` 行、`hub.rs:4825`・`hub_tags.rs:677` の `hub` 行の有効/無効）。テストの作り手は Hub の `hub.rs`（10 か所）・`runtime.rs`・`secrets.rs`・`db_source/plan.rs`、`banto-collect` の `config.rs`・`collector.rs`（約 25 か所）、`banto-drivers/src/lib.rs`、ChronoGazer の core（約 10 か所）と `core/tests/*.rs`（7 本 × 2）。
- `ALLOWED_PROTOCOLS`（`plc_connection.rs:322`）: 旧い検証 (1) と `settings_input.rs:187`（レジストリに無く、ホストの一覧にも無いプロトコルを断る）が参照。SQL の `CHECK` は R2b-1 で消えている。

### 1.5 ChronoGazer

- サービスは旧規則で作る: `engine.rs:897-900` の `PlcConnectionService::new`・`CollectionGroupService::new`・`TagService::new`。レジストリは同じ `engine.rs:313` で `banto_drivers::builtin().registry` を `EngineConfig::drivers` に詰めている（R2b-3a-3。収集の I/O だけ broker 経由）。
- 保存時のアドレスの規則は `apps/chronogazer/core/src/tag_address.rs`（#414 段階 1）: `banto_collect::check_tag_address`（`crates/banto-collect/src/config.rs:1705-1721`。`modbus-tcp` / `slmp` だけ `banto_plc::Address` で解釈し、他は通す）を、タグの作成・更新・接続のプロトコル変更・グループの接続変更の 4 経路で REST と Tauri の両方から呼ぶ。`address_notation`（`config.rs:1540`）と `Protocol`（`config.rs:77`）はエラー文言の記法を選ぶためだけに残っている（sim-design §5 の注記「R2b-7 でまとめて消える」）。`banto-collect` → `banto-plc` の通常の依存（`Cargo.toml:30`）は `use banto_plc::{Address, DataType}`（`config.rs:42`）のため。
- 登録簿モードの `banto-tags` は同じ検査を持つ: `TagService::with_registry` は `normalize_tag_address` でドライバの `parse_address` を通し（`tag.rs:1772-1800`）、**レジストリに無いプロトコル（`hub`）の `plc` タグは trim した文字列をそのまま正規形にして通す**（同 `:1766-1772` の `else if let Some(entry)` の外）。接続のプロトコル変更（`plc_connection.rs:1895-1915` の `reparse_tags`、`ReparseScope::Connection`）とグループの接続変更（`collection_group.rs:639`、`ReparseScope::Group`）で配下のタグを読み直して断る。つまり ChronoGazer の `tag_address.rs` の 4 経路は `with_registry` で置き換えられる。
- REST・Tauri の接続の形は平ら: `PlcConnectionPayload { name, protocol, host, port, unit_id, enabled, word_order, simulation: Option<bool> }`（`rest.rs:918-932`。`cpu_type` は常に `None` = 作成は R、更新は保つ。#605）、`PlcConnectionResponse`（`rest.rs:1018-1028`）。Tauri の `plc_connections_create` / `_update`（`src-tauri/src/lib.rs:2327-2447`）は同じ型を使う。許可リストは `reject_disallowed_connection_protocol`（`rest.rs:854-880`。`modbus-tcp`・`slmp`・`hub`）。
- UI: `apps/chronogazer/src/lib/banto/tagRegistryAdmin.ts:57-92` の `PlcConnection` / `PlcConnectionInput`（`unitId`・`wordOrder`）、`routes/(app)/tags/+page.svelte:208-211` の固定の `protocolOptions`、`tagsPageLogic.ts`。接続のフォームはタグの画面の中（`PLC_CREATE` / `PLC_EDIT`）。
- `hub` 接続: 「Hub から追加」（`hub_tags.rs:445-470`）が `host: ""`・`port: 0` の行を 1 つ作る。`idx_plc_connections_single_hub`（`migrations/0025_plc_connections_settings_json.sql:180`）が「`hub` は高々 1 行」を守る。Hub 側は `reject_hub_connection_protocol`（`rest/plc_connections.rs:192`）で断る。

### 1.6 版

- banto-hub は v0.2.0-alpha.39（ルート `Cargo.toml:98`・`apps/banto-hub/package.json:4`・`installer/src/main.rs:127`・`CHANGELOG.md`）。`banto-driver-api`・`banto-driver-conformance` は 0.3.0、`banto-driver-engine` は 0.2.0。alpha.34〜39 のタグ・リリースはまだ無い（STATUS）。

## 2. 範囲: R2b-7 で変えるもの・残すもの

| 面                                                      | R2b-7                                                                                                               | 後の段・触らない                                                                          |
| ------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------- |
| Hub の REST `/api/plc-connections`・`/test`             | 形は変えない。項目エラーの欄名だけ `settings.<key>` に（§4）                                                        | -                                                                                         |
| Hub の MCP の接続ツール                                 | `settings` の形へ（§3）。`flat_connection.rs`・`KeepStored` を消す                                                  | 他の MCP ツール（タグ・グループ等）は変えない                                             |
| 設定パッケージ                                          | 変えない（版 3 のまま。§7）                                                                                         | -                                                                                         |
| `GET /api/sink/config`・`GET /api/ingest/bridge-config` | 応答の形は変えない。`postgres` の欄の出どころを `settings` にする（7c）                                             | `postgres` ごと消すのは R2b-8                                                             |
| `banto-tags`                                            | 読み取りの平らな欄・`flat_view` を消す。`rest_field_error` の写しを消す。`KeepStored`・`validate_flat_input` を消す | 書き込みの平らな入力・`new()`・`ALLOWED_PROTOCOLS` はホストの接続とテストのため残す（Q3） |
| `banto-collect`                                         | `check_tag_address`・`address_notation`・`Protocol`・`TagAddressIssue` 等の旧規則と `banto-plc` の通常の依存を消す  | -                                                                                         |
| ChronoGazer の core（保存の規則）                       | 3 サービスを `with_registry` に。`tag_address.rs` の検査を消す（§6.1）                                              | 収集の I/O は R2b-3 で済み                                                                |
| ChronoGazer の REST・Tauri・UI                          | 接続を `settings` の形に。ドライバの一覧の口を足す。汎用フォームの写し（§6.2・§6.3）                                | `hub` 接続の作り方（「Hub から追加」）は変えない                                          |
| Hub の UI                                               | `mapSettingsFieldErrors` の camelCase を消すだけ                                                                    | -                                                                                         |
| 版                                                      | 7e で上げる（Q7）                                                                                                   | タグ・リリースの扱いは STATUS のとおり（alpha.34 の後）                                   |
| シリアル（#803）・R2b-4・iframe・`postgres` の撤去      | 触らない                                                                                                            | 凍結 / W 段 / R2b-8                                                                       |

並行の約束（2026-10-22 オーナー指示）を守る: lock の `encoding_rs` は 0.8.35 のまま（`cargo update` しない。7e の `cargo update -w` は版の 4 点セットのためだけで、依存の版は動かさない。動いたら戻す）、ルートの `Cargo.toml`・`Cargo.lock` の変更は最小、書き込みの関門の順と RMW の判断の意味は変えない（本書は書き込みの経路に触らない）。

## 3. MCP の接続ツール（7a、Q1・Q2）

- `create_connection` / `update_connection` の `inputSchema` を REST と同じ `{ name, protocol, enabled, simulation, settings }` にする。`protocol` の `enum` は固定せず（`description` に「`list_drivers` で得られる id、または `postgres` / `push`」と書く。Q9）、`settings` は `object`。秘密のキー（`postgres` の `password`）は **`settings` の中に平文で入れ、Hub の入口が REST と同じ tri-state で封印する**（省略 = 保つ、`null`・空文字 = 消す、文字列 = 置き換え。ui-design §5 のオーナー決定）。応答は REST の `PlcConnectionResponse` と同じ（秘密は `{"$secret": "set" | "unreadable"}`。`passwordSet` / `passwordState` は消す）。
- `update_connection` の意味を REST に揃える: **`settings` は丸ごと置き換え**（省略した欄はスキーマの既定に戻る。`cpuType` と push の `settings` の「省略は保つ」は無くなる）。`REQUIRED_CONNECTION_KEYS` は `name`・`protocol`・`enabled`・`simulation`・`settings` に縮む（`missing_fields` の仕組みは残す）。これで `KeepStored` と、未適用キューの「印のまま積む」経路が消える。
- `test_connection` は REST の `PlcConnectionTestPayload { protocol, simulation, settings, connectionId? }` と同じ形。`into_test_payload` の「保存済みから `wordOrder`・`cpuType` を補う」は消える（フォームが送る値がそのまま）。`test_saved_connection` は変えない（`postgres` 専用、R2b-8）。
- `list_connections` の応答も `PlcConnectionResponse` と同じ。`list_drivers`（ドライバの一覧。`GET /api/drivers` と同じ中身）を MCP に足す（Q9）。足さない場合は `description` にスキーマのキーを書く。
- 実装: `mcp.rs` の 4 ツールが `crate::rest::PlcConnectionPayload` / `PlcConnectionTestPayload` を直接 `serde_json::from_value` で受け、REST のハンドラと同じ関数（封印・未適用キュー・`create_settings_tx` / `update_settings_tx`）を呼ぶ。`flat_connection.rs` を消し、`banto-tags` の `KeepStored`・`validate_flat_input` を消す（`settings_input.rs:71-100`・`:198-205`）。未適用キューの中身は印が無くなるぶん単純になる（古い形の項目は R2b-6 の Q10 と同じく「読めなければ捨てて起動ログに出す」）。
- docs: `banto-hub-mcp-reference.md` §5 の「接続の入力は平らな形のまま」と `update_connection` の例外の記述、`banto-hub-operations.md:2574` 付近の対応表。

## 4. 項目エラーの欄名を `settings.<key>` に（7b）

オーナー決定（2026-10-24、#821）: 接続の項目エラーの欄名を全ドライバで `settings.<key>`（スキーマの snake_case）に揃え、UI の写しを 1 通りにする。

- `rest_field_error`（`settings_json.rs:99-125`）を「どのプロトコルでも `settings.<key>`」にする。`push` の入れ子の戻し（`settings.bridge.queries[0].sql`）は今のまま。`unit_id → unitId` 等の写しと `host`・`port` の素通りを消す（`host` も `settings.host`）。テスト `field_keys_are_written_in_the_rest_spelling` を書き換える。
- `settings_from_schema` のエラー（型・必須・未知のキー。今は `unit_id` のキーそのまま）も `settings.<key>` にする（`settings_input.rs:102-160` の経路で付ける）。これで同じ欄に 2 通りの名前が返ることが無くなる。
- 接続の名前・プロトコル・有効・シミュレーションのエラーは `name`・`protocol`・`enabled`・`simulation` のまま（`settings` の外の欄）。秘密のキーの「必須です」（`secret_field_error`、`rest/plc_connections.rs:50`）も `settings.<key>` に揃える。
- ホストの接続: `push` は今も `settings.*` で変わらない。**`postgres` は触らない**（旧い検証 (1) の `host`・`port`・`database`・`username`・`password` のまま。R2b-8 で消える）。`virtual`・`hub` は項目が無い。
- UI: `mapSettingsFieldErrors` は `settings.<key>` を剥がして欄に当てるだけにし、`camelToSnake` を消す。`postgres` の呼び方（`POSTGRES_SETTING_KEYS`。欄名は `host` 等でキーと同じ綴り）は `settings.` の無い `<key>` を当てる分岐として残す（R2b-8 で消える）。vitest（`driverSettingsForm.test.ts` があればそこ）を直す。
- `connection_validation_consistency.rs`（R2b-1c の一致の表）: ドライバの経路は (1) を通らなくなったので、表の「(1) と (4) が同じ集合を断る」はホストの接続（`postgres`・`push`）だけに縮める。ドライバの行は「`settings.<key>` で断る」ことを確かめる表に置き換える。
- `connection_secret.rs:222` の「REST の項目名」のコメントと定数を直す。

## 5. `banto-tags` の平らな欄の撤去（7c、Q3）

読み手が全部移った後（§9 の順）に行う。

- `PlcConnection` から `host`・`port`・`unit_id`・`word_order`・`cpu_type`・`database`・`username` を消し、`from_settings` を `settings` を持つだけにする。`legacy::flat_view`・`FlatView` を消す。代わりに `PlcConnection::setting_str(key) -> Option<&str>` / `setting_i64(key) -> Option<i64>` の小さな読み手を足す（`postgres` の読み手と ChronoGazer の `hub` 行の有効/無効の組み直しが使う）。
- Hub の `postgres` の読み手（`db_source/mod.rs`・`db_source/plan.rs`・`rest/sink.rs`）は `setting_str("host")` 等に替える。`SinkConfigConnectionEntry` 等の**応答の形は変えない**。`port` の範囲外の扱い（`db_source/mod.rs:510-516`）は今のまま。
- ChronoGazer の `hub` 行の有効/無効（`hub.rs:4825`・`hub_tags.rs:677`）は 7d-2 で `update_settings_tx`（`settings` は `{}`）に替わっているので、7c では読まない。
- 書き込みの平らな入力は残す（Q3 の (a)）: `PlcConnectionInput`・`legacy::settings_from_flat`・`PlcConnectionService::new`・`validate_plc_connection_input`・`ALLOWED_PROTOCOLS`。モジュールの doc に「ホストの接続（`settings_input.rs` が写す）とテストのためだけ。本番の入口は `ConnectionSettingsInput`」と書く。本番の非テストのコードが `PlcConnectionService::new(`・`TagService::new(`・`CollectionGroupService::new(`・`ALLOWED_PROTOCOLS`・`PlcConnectionInput {` を参照しないことを、Hub（既存の grep のテストを広げる）と ChronoGazer（`apps/chronogazer/core/tests` に同じ形で足す）で固定する。`settings_json.rs` の冒頭の doc（「REST・MCP・設定パッケージ・ChronoGazer の平らな形」）を直す。
- `banto-tags` の `settings_from_flat` の `modbus-tcp` / `slmp` の写しは、旧規則の `new()` のテストが使うので残る。`ALLOWED_PROTOCOLS[0] == MODBUS_PROTOCOL` 等の既存のテストはそのまま。

## 6. ChronoGazer の追従

### 6.1 保存の規則をレジストリへ（7d-1、Q4）

- `engine.rs:897-900` の 3 サービスを `with_registry(pool, registry)` にする。`registry` は同じ関数で作っている `Arc<DriverRegistry>`（`engine.rs:313`）を共有する（`EngineConfig::drivers` と同じ 1 つ）。`init_db` の後の順は Hub の `runtime.rs:717-722` と同じ。
- `hub` 接続はレジストリに無いので、`settings_input.rs` のホストの一覧（`HUB_PROTOCOL`、空の `settings` だけ）で通る。`hub` の下の `plc` タグ（外部名 `PLC9.G.Temp` 等）は `normalize_tag_address` が trim して通す（§1.5）。**反証**: `hub` の下のタグが登録簿モードで断られないことを、`hub_collect_roundtrip.rs` か `hub_tags.rs` の既存のテストで確かめる。
- `tag_address.rs` の 4 経路（`ensure_tag_fits_its_connection`・`ensure_tag_update_fits_its_connection`・`ensure_protocol_change_keeps_tags_readable`・`ensure_group_move_keeps_tags_readable`）と、それを呼ぶ REST・Tauri の箇所を消す。`banto-tags` の登録簿モードが同じ検査（`normalize_tag_address`・`reparse_tags`）を書き込みのトランザクションの中で行う。文言と `field`（`address`・`dataType`・`protocol`・`plcConnectionId`）は `banto-tags` のものに変わる。`update_needs_address_check`（#418 P2: 無効化だけの更新は検査しない）に当たる約束が `banto-tags` の登録簿モードにあるかを 7d-1 の着手時に確かめ、無ければ `TagService::update` の「読む場所が変わらない更新は読み直さない」として足す（Q4）。`update_tag_checked`（楽観ロック）は残す。
- `banto-collect` の `check_tag_address`・`TagAddressField`・`TagAddressIssue`・`address_notation`・`Protocol`・`parse_read_request` と `use banto_plc::{Address, DataType}`（`config.rs:42`）を消し、`Cargo.toml:30` の通常の依存を外す（dev-dependency の `simulator` は R2b-5 のとおり残す）。`ResolvedConnection::Plc(Option<Protocol>, ..)` の `Protocol` を消し、アドレスのエラー文言は `banto-tags` の登録簿モードが出すものに任せる（sim-design §4.3 の注記の解消）。
- `banto-plc` の `#[cfg(test)] decode_string_value`（`decode.rs:140`）とそのテストをここで消す（Q8 の (a)）。
- 既存のタグの正規形: `TagService::new` は trim した綴りのまま保存し、`with_registry` はドライバの正規形で保存する。使用者無しの前提で既存データの移行はしない（address-design の R2b-2 と同じ）。保存済みの綴りのままでもドライバは読める（io-design §8）。

### 6.2 REST・Tauri の接続の形（7d-2、Q5）

- `PlcConnectionPayload` を Hub と同じ `{ name, protocol, enabled, simulation?, settings }` に、`PlcConnectionResponse` を `{ id, name, protocol, enabled, simulation, settings }` にする。`simulation` の更新での省略（#417 の `update_preserving_simulation`）は ChronoGazer の約束なので `Option<bool>` のまま残し、`settings_input` の `update_settings_tx` に `simulation` を渡す前に既存の値と合わせる（今の `update_preserving_simulation` と同じく書き込みのトランザクションの中で。`settings_input.rs` に `update_settings_preserving_simulation_tx` を足す）。
- `cpu_type` を常に `None` にしていた #605 の暫定（作成は R、更新は保つ）は消える: スキーマの `cpu_type` に既定 R があり、フォームが欄を出す。
- `reject_disallowed_connection_protocol` を「`registry.get(protocol)` が `Frame` / `Session` のドライバか、`hub`」に替える（`postgres`・`push`・`virtual` は今までどおり断る。文言は今のまま）。REST と Tauri の両方から呼ぶ約束は変えない。
- `hub` 行の有効/無効（`hub.rs`・`hub_tags.rs`）は `update_settings_tx` に `settings: {}`・`secrets: 空` で置き換える。「Hub から追加」の `hub` 行の作成は `create_settings_tx` に替える。
- ドライバの一覧: `GET /api/drivers`（Hub と同じ応答。認証付き）と Tauri コマンド `drivers_list` を足す。応答を組み立てる純関数（`apps/banto-hub/core/src/rest/drivers.rs` の記述子・スキーマ・`supportedKinds`・`addressHelp` → JSON）を `banto-tags`（`banto-driver-api` に依存し、両アプリが使う）へ `driver_catalog` として移し、Hub もそれを呼ぶ。ChronoGazer のルートのスナップショットのテストがあれば直す。
- 監査の `plc_connection_audit_detail` は `settings` から組む（秘密の値は載せない。ChronoGazer に秘密のキーを持つプロトコルは無いが、`is_secret_ref` で落とす）。
- example `apps/chronogazer/core/examples/hub_p6_compare.rs` は Hub の REST の形を既に R2b-6b で追従済み（ui-design §5）。ChronoGazer 側の `PlcConnectionService::new` の利用は example なので残す（grep のテストの対象外）。

### 6.3 UI（7d-2 の中、Q6）

- `tagRegistryAdmin.ts` の `PlcConnection` / `PlcConnectionInput` を `settings: Record<string, unknown>` の形にし、`listDrivers()`（REST / Tauri の両方）を足す。
- タグの画面の接続のフォーム（`PLC_CREATE` / `PLC_EDIT`）を、Hub の `DriverSettingsForm.svelte`・`driverSettingsForm.ts`（`settingsFromForm`・`mapSettingsFieldErrors`・`summarizeDriverValues`）の**写し**で組み立てる。`protocolOptions` の固定は `listDrivers()` の結果（`hub` は載せない。一覧では読み取り専用で出す今の扱いのまま）に替える。
- 写しにする理由（Q6）: 2 つのアプリに共通の社内パッケージが無い（`pnpm-workspace.yaml` は `apps/*`）。パッケージを新設すると pnpm のワークスペース・CI の `ci-changes`・vitest の配置が増え、並行の約束（ルートの変更は最小）に触れる。写しは 2 ファイルで、分岐を持たない汎用の部品なので R2b-6 の「プロトコル別の写しを消す」とは反しない。共通化は W 段で iframe の枠と一緒に見直す。
- ChronoGazer の vitest（`tagsPageLogic.test.ts`・`tagRegistryAdmin.test.ts`）を新しい形に書き換える。

## 7. 設定パッケージと版（Q7）

- 設定パッケージは変えない（§0.1）。`checkConfigPackageConnectionFields` の `HOST_CONNECTION_FIELDS`（`postgres`・`push`）もそのまま。
- 版: 7e で **0.3.0-alpha.1** に上げる（connection-settings-design Q13「外の形を変える R2b-7 で 0.3.0 系へ」）。4 点セット（ルート `Cargo.toml`・`apps/banto-hub/package.json`・`installer/src/main.rs`・`CHANGELOG.md`）と `cargo update -w`（checklist §7）。chronogazer は 0.1.0 のまま（bump も CHANGELOG も不要、checklist §7）。CHANGELOG には「MCP の接続ツールの形（破壊的）」「項目エラーの欄名（破壊的）」「ChronoGazer の接続の形と保存時の規則」を書く。
- alpha.34〜39 のタグ・リリースは STATUS のとおり（実機の確かめの後）。0.3.0-alpha.1 の扱いも同じ列に載せる。

## 8. 持ち越し（#799 から。Q8）

- `banto-plc` の `decode_string_value` は `#[cfg(test)]` だけで本番の呼び手が無い: 7d-1 で消す（§6.1）。
- 上限を超える文字列（`string_length` が計画に載らない）をエンジンが一律 `Err(ItemError::Unsupported("計画に含まれない"))`（`crates/banto-driver-engine/src/lib.rs:355`）で返し、ドライバの断りの理由が落ちる: 本番のエンジンの振る舞いの変更で R2b-7 の形の変更と無関係なので、**Issue を立てて別にする**（直し方の候補: `plan_read` が計画から外した項目の理由を `Unsupported(理由)` で返す口を `DriverBase` に足す）。
- `real_plc_verify` の書き込みフェーズ 2・3 は R2b-7 の実機の確かめ（§10）と一緒に扱うかをオーナーに確かめる。

## 9. PR の分け方

| PR   | 内容                                                                                                                                                                                                             | モデルの目安                                                                          |
| ---- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------- |
| 7a   | MCP の 4 ツールを `settings` の形に、`list_drivers`（Q9）、`flat_connection.rs`・`KeepStored`・`validate_flat_input` の削除、`host/tests/mcp.rs` の書き換え、mcp-reference・operations の注記                    | sonnet（REST と同じ関数を呼ぶだけ。未適用キューに触るのでレビューは上位）             |
| 7b   | `rest_field_error`・`settings_from_schema` のエラーを `settings.<key>` に、UI の `mapSettingsFieldErrors`、`connection_validation_consistency.rs` の縮小、`connection_secret.rs` のコメント                      | sonnet                                                                                |
| 7d-1 | ChronoGazer の 3 サービスを `with_registry` に、`tag_address.rs` の検査の削除、`banto-collect` の旧規則と `banto-plc` の通常の依存の削除、`decode_string_value` の削除、grep のテスト（ChronoGazer）             | opus（保存の規則・`hub` のタグ・正規形に関わる）                                      |
| 7d-2 | ChronoGazer の REST・Tauri の接続の形、`reject_disallowed_connection_protocol`、`hub` 行の書き込みの置き換え、`GET /api/drivers`・`drivers_list`・`banto_tags::driver_catalog`、UI（汎用フォームの写し）、vitest | sonnet（REST・Tauri・UI の 3 面で量が多い。2 つに割ってもよい: 7d-2a core、7d-2b UI） |
| 7c   | `PlcConnection` の平らな欄・`flat_view` の削除、`setting_str` / `setting_i64`、Hub の `postgres` の読み手の置き換え、grep のテスト（Hub の拡張）、`settings_json.rs` の doc                                      | opus（横断。`db_source`・sink に触る）                                                |
| 7e   | docs（本書・refactor-plan・README の索引・connection-settings / address / io / sim / ui 設計の「R2b-7 で」の注記・mcp-reference・operations）・CHANGELOG・版（Q7）                                               | haiku                                                                                 |

- 順: 7a と 7d-1 は並べられる（触るファイルが分かれる。ただし CI は 2 本まで、ローカルのビルドは 1 本ずつ）。7b は 7a の後（MCP の旧い検証 (1) の経路が消えてから、(1) をホストの接続だけに縮める）。7d-2 は 7d-1 と 7b の後（ChronoGazer のフォームが `settings.<key>` のエラーを受ける）。**7c は最後**（`conn.host` 等の読み手が MCP・ChronoGazer から消えてから。先に消すと 2 アプリのビルドが同時に壊れる）。
- 各 PR の指示書に書くこと（checklist §1）: ブランチを切る・入れ子のサブエージェント禁止・`.claude/worktrees` に入らない・検証の順（§2 をそのまま）・やらないこと（§2 の「触らない」列、並行の約束）・答えの無い分岐は止まって報告。Hub の REST のルートを足す・消すときは `apps/banto-hub/core/tests/snapshots/routes.txt`（7a は MCP だけなので対象外、7d-2 の ChronoGazer に同じ仕組みがあればそれ）。

## 10. テスト

- 7a: `host/tests/mcp.rs` を新しい形に書き換える（作成・更新・一覧・接続テスト・未適用キューに積む/適用・秘密の tri-state・`missing_fields`）。旧い平らなキー（`unitId` 直下）が「`settings` が必須」等で断られることを 1 本。`list_drivers` の中身が `GET /api/drivers` と同じ。
- 7b: `settings_json.rs` の単体（全ドライバの全キーが `settings.<key>`、`push` の入れ子はそのまま、`host`・`port` も `settings.`）。`settings_from_schema` のエラーの名前。REST の統合テスト（`banto-hub-host`）で型違い・範囲外・未知のキー・必須の秘密の欄が `settings.<key>` で返る。UI の vitest（`settings.<key>` だけで欄に当たる、`unitId` は `other` に落ちる）。E2E（#823 の追従の E2E）で欄の下にエラーが出ることを 1 回。
- 7d-1: ChronoGazer の core のテストのうち、旧規則の文言・`field` に依るもの（`tag_address.rs` のテスト、`rest.rs` のプロトコル変更・グループ移動のテスト）を `banto-tags` の登録簿モードの文言に書き換える。`hub` の下のタグが断られない（§6.1 の反証）。`collect_roundtrip.rs` 等の 7 本は `PlcConnectionInput` で作っているのでそのまま通る（`new()` は残る）。grep のテスト。`cargo test -p banto-collect` で `banto-plc` 無しに通る。
- 7d-2: REST（`rest.rs` のテスト）と Tauri（`src-tauri` のテスト）で、作成・更新・一覧の新しい形、`simulation` の省略が保つ、`hub` 行の有効/無効、`reject_disallowed_connection_protocol` が `postgres`・`push`・`virtual`・未知の id を断り `modbus-tcp`・`slmp`・`hub` を通す。`GET /api/drivers` の中身が Hub と同じ順（SLMP が先頭）。vitest。
- 7c: `banto-tags` の単体（`setting_str` / `setting_i64`、`from_settings`）。`db_source` と `sink` の既存のテスト（`db_source/mod.rs:815-943` の `conn.port = 70_000` 等は `settings` に書く形に直す）。`GET /api/sink/config`・`GET /api/ingest/bridge-config` の応答の形が変わらないことを既存のテストで。grep のテスト。
- 実機（7e の前。STATUS の実機の確かめの欄に入れる）: MELSEC（R08 試験機）と Modbus で、(1) Hub の MCP から接続の作成・更新・接続テスト（`settings` の形）、(2) ChronoGazer の画面から接続の作成・編集・タグの登録・収集の開始、(3) ChronoGazer の「Hub から追加」で `hub` 接続とタグができ、収集が動く（R2b-3a-3 の未確認の項目と一緒に）。
- 反証（checklist §5）: 7b で `unitId` を返す経路が残っていないことは、`rg "unitId|wordOrder|cpuType"` を `crates/banto-tags/src`・`apps/banto-hub/core/src` に掛けて 0 件（`postgres` を除く）で確かめる。7c で `conn.host` 等の読み手が残っていないことはコンパイルが証明する。

## 11. オーナー判断

**2026-10-24 オーナー決定: Q1〜Q10 はすべて推奨どおり**（Q10 は (b)、それ以外は (a)）。

| Q   | 問い                                                                  | 選択肢                                                                                                                                                                                                  | 推奨                                                                                                                                                                                          |
| --- | --------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Q1  | MCP の接続ツールの形                                                  | (a) REST と同じ `{ name, protocol, enabled, simulation, settings }`、秘密は `settings` の中で tri-state / (b) 平らな形のまま残す（アダプタを保守）                                                      | **(a)**。R2b-6 の Q2 で「MCP は R2b-7」と決めた形。アダプタ・`KeepStored`・未適用キューの印が消える                                                                                           |
| Q2  | `update_connection` の「省略は保つ」（`cpuType`・push の `settings`） | (a) 無くす（REST と同じ丸ごと置き換え。秘密のキーだけ省略 = 保つ）/ (b) `settings` の省略したキーを保存済みから補う（`KeepStored` を残す）                                                              | **(a)**。REST と同じ約束に揃える。MCP の呼び手（LLM）は `list_connections` で今の `settings` を読んでから全体を送れる                                                                         |
| Q3  | `banto-tags` の書き込みの平らな入力（`PlcConnectionInput`・`new()`）  | (a) ホストの接続とテストのために残し、本番の非テストのコードが参照しないことを grep のテストで固定 / (b) 消して、テストの作り手（約 60 か所・6 crate）を `ConnectionSettingsInput` に書き換える         | **(a)**。(b) は `TagService::new` の旧規則に依るテストの正規形の前提も変わり、R2b-7 の PR が大きくなる。`postgres` が消える R2b-8 で `settings_input.rs` のホストの経路と一緒に見直す         |
| Q4  | ChronoGazer の `update_needs_address_check`（#418 P2）の扱い          | (a) `banto-tags` の登録簿モードに同じ約束が無ければ「読む場所が変わらない更新は読み直さない」として足す / (b) ChronoGazer の検査をこの 1 点だけ残す                                                     | **(a)**。検査を 1 か所にする（R2b-7 の目的）。Hub にも同じ利点がある                                                                                                                          |
| Q5  | ChronoGazer の `hub` 接続とレジストリ                                 | (a) `hub` はレジストリの外のまま（`settings_input` のホストの一覧で通す。`idx_plc_connections_single_hub` は変えない）/ (b) `hub` を `HostProvided` のドライバとして ChronoGazer のレジストリに登録する | **(a)**。`hub` の接続先とキーは `hub.record` にあり、行は印だけ。ドライバにしても得るものが無い。索引は DB の不変条件として残す（connection-settings-design §3.1 の「R2b-7 で見直す」の答え） |
| Q6  | ChronoGazer の接続のフォーム                                          | (a) Hub の汎用フォーム 2 ファイルの写しを置く / (b) `packages/` に共有パッケージを新設して両アプリから使う / (c) 固定の欄（host・port・unitId・wordOrder）のまま `settings` に写す                      | **(a)**。(b) はワークスペース・CI の変更が要り、並行の約束に触れる。(c) は R2b-6 で消した「プロトコル別の TS の写し」を ChronoGazer に残す。共通化は W 段                                     |
| Q7  | 版                                                                    | (a) 7e で 0.3.0-alpha.1（connection-settings-design Q13）/ (b) 0.2.0-alpha.40 のまま                                                                                                                    | **(a)**。外の形（REST は R2b-6、MCP・ChronoGazer は R2b-7）の変更が揃う段。alpha.34〜39 と同じく、タグは実機の確かめの後                                                                      |
| Q8  | #799 の持ち越し                                                       | (a) `decode_string_value` の削除だけ 7d-1 に入れ、エンジンの `Unsupported` の理由は Issue にする / (b) 両方 R2b-7 に入れる（7f）/ (c) 両方 Issue                                                        | **(a)**。理由の件は本番のエンジンの変更で、形の段と混ぜない                                                                                                                                   |
| Q9  | MCP にドライバの一覧 `list_drivers` を足すか                          | (a) 足す（`GET /api/drivers` と同じ中身。`protocol` の `enum` は固定しない）/ (b) 足さず、`description` にスキーマのキーを書く                                                                          | **(a)**。MCP の呼び手が `settings` のキー・既定値・選択肢を知る口が要る。`enum` の固定はドライバが増えるたびに直すことになる（R2b-6 の `/api/drivers` と同じ理由）                            |
| Q10 | 7d-2 の分割                                                           | (a) 1 本（core + UI）/ (b) 7d-2a（REST・Tauri・`driver_catalog`）と 7d-2b（UI）に割る                                                                                                                   | **(b)**。1 PR が 1 エージェントに任せられる大きさにする。7d-2a と 7d-2b の間は ChronoGazer の画面が接続を保存できない窓になる（R2b-6 の 6b/6c と同じ。同じ版の中で続けて入れる）              |

## 12. 確かめきれなかったこと・危険

- `banto-tags` の登録簿モードに `update_needs_address_check`（#418 P2）相当があるか（Q4）。7d-1 の着手時に `TagService::update_tx` を読んで確かめる。
- ChronoGazer の `hub` の下の `plc` タグが登録簿モードで通ること（§1.5 の読みでは通る）。既存のテストで反証する。
- 7d-1 で ChronoGazer の保存時の文言が `banto-tags` のものに変わる。ChronoGazer の E2E（ルートの `e2e/` に ChronoGazer の対象があれば）が文言に依っていないか。
- 7c の後、`banto-hub-sink` の `flush.rs` は `GET /api/sink/config` の応答（別の型）を読むので影響は無いが、`bridge_parity.rs` 等の sink のテストが `PlcConnection` の平らな欄を組んでいれば直す。
- 7a で MCP の未適用キューの項目の形が変わる。R2b-6 の Q10 と同じく、古い形は捨てて起動ログに出す。キューに残った項目があるまま版を上げる使い方は無い前提。
- `encoding_rs` 0.8.35: 7e の `cargo update -w` で動かないことを `git diff Cargo.lock` で確かめる。
- `real_plc_verify` の書き込みフェーズ 2・3（`REAL_PLC_CONFIRM_WRITE=1`）は未実行のまま（§8）。

## 13. 既存の文書の直し（7e で）

- `banto-hub-driver-refactor-plan.md` §4 の R2b-7 の行: 完了の印・PR 列・版・本書へのリンク。
- `docs/README.md` の索引: 本書の行（下書きの時点で足す）。
- `banto-hub-connection-settings-design.md`: §2 の表の R2b-7 の列（「平らな欄を消す」「`{driver, settings}` の形へ」「形を変える」「版を上げる」「追従」）、§4.2 の「R2b-7 で消える」、§4.3 の見出し「（R2b-7 まで）」、Q6・Q13 の注記、§3.1 の `idx_plc_connections_single_hub` の「R2b-7 で見直す」（Q5 の答え）。
- `banto-hub-driver-address-design.md` §5 の前倒しの案（`248` 行）と Q7、`banto-hub-driver-io-design.md` の「保存時の規則は R2b-7」（`21`・`34`・`140`・`388` 行）、`banto-hub-driver-sim-design.md` §4.3・§5 の「R2b-7 でまとめて消える」、`banto-hub-driver-ui-design.md` §2・§4.2・§5 の「R2b-7」。
- `banto-hub-mcp-reference.md` §5（接続ツールの形・`update_connection` の例外・`list_drivers`）、`banto-hub-operations.md` の対応表（`2574` 行付近）、`CHANGELOG.md`。
- `crates/banto-tags/src/settings_json.rs`・`settings_input.rs`・`plc_connection.rs` のモジュールの doc（「R2b-7 まで」「R2b-7 で消える」）。
