# Banto クライアントの Hub 自動接続（#332）

状態: **chronogazer 分のみ実装済み**（共有 crate `crates/banto-hub-bootstrap` + chronogazer の
設定カテゴリ「Hub 接続」）。**選んだタグの購読（#383 段階1）まで実装済み** — §10 参照。
**relay-wright への配線は保留**（relay-wright 自体を 2026-09-17 の
オーナー決定で凍結、plan.md §4b）。
**banto-hub 側は変更ゼロ**（`apps/banto-hub/core/tests/client_bootstrap.rs` が前提を回帰固定）。
最終更新: 2026-09-17

---

## 1. 目的

Banto アプリ（chronogazer / relay-wright）が banto-hub のタグを読むには API キーが要る。従来は
運用者が Hub の管理画面でキーを発行し、アプリ側へコピー＆ペーストする必要があった。#332 は
この手作業を、**試運転（commissioning）中に限りアプリが自分用のキーを自己発行する**ことで
なくす。

```text
アプリ                                   banto-hub
  │  GET /api/commissioning/status  ──▶   未認証で読める（設計 §5.6）
  │  ◀── { "lockedDown": false }
  │  POST /api/api-keys              ──▶  試運転中は bearer 認証をバイパス
  │      { name, scopes: ["read"] }        （synthetic admin identity）
  │  ◀── { id, name, prefix, scopes, key }  平文 key はこの応答限り
  │  KeyStore::set                          → OS キーリング
  │  GET /api/v1/tags（banto-tagclient） ─▶ API キー認証（従来どおり）
  │  ◀── 200 { "tags": [...] }              タグ 0 件でも 200 + []
```

## 2. issue の採用方針との対応

issue #332 本文冒頭の「採用方針（2026-09-14）」＋2026-09-08 のコード検証コメントが現行方針。
そこで決まった 3 点を、この実装は次のように満たしている。

| issue の決定事項                                       | 実装                                                                                                                                                        |
| ------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------- |
| キー名の重複と再ブートストラップ                       | 名前を `{app_id}-{installation_id}-{発行時刻(unix秒)}` にする。keyring 喪失後の再発行でも `api_keys.name` の UNIQUE 制約に当たらない。                      |
| `admin` は「原則」ではなく **絶対に**自動発行しない    | crate 内にホワイトリスト（`ISSUABLE_SCOPES = ["read"]`）を置き、`admin` と `write:` を含む要求をネットワーク呼び出し前に `Err` で拒否（ユニットテスト有）。 |
| AC「同一 PC の任意プロセスが偽装できない」は満たせない | 満たすと主張しない。代わりに §7 の「同一 PC 限定である理由」を明記し、ロックダウン後に権限が増えないことを実装で担保する。                                  |

## 3. crate の責務と trait 境界（`crates/banto-hub-bootstrap`）

`banto-tagclient` と同じ方針で、**OS と DB はこの crate の外**に置く。

```rust
pub trait KeyStore {                       // 平文の API キーの置き場（OS キーリング）
    fn get(&self, account: &str) -> Result<Option<String>>;
    fn set(&self, account: &str, secret: &str) -> Result<()>;
    fn delete(&self, account: &str) -> Result<()>;
}

pub trait BootstrapState {                 // 非機密の接続記録の置き場（アプリの設定ストア）
    fn load(&self) -> Result<Option<HubRecord>>;
    fn save(&self, record: &HubRecord) -> Result<()>;
    fn clear(&self) -> Result<()>;
}

pub struct HubRecord {                     // ← キー欄が無いことが「設定 DB に平文が無い」根拠
    pub endpoint: String,
    pub installation_id: String,
    pub key_id: Option<i64>,               // 失効に使う唯一のハンドル
    pub key_name: Option<String>,          // 表示・監査用。失効の検索には使わない
    pub keyring_account: String,           // hub:{host}:{port}{path}:{installation_id}
    pub selected_tags: Vec<String>,
}
```

両 trait は**同期**。keyring 呼び出しは元々ブロッキングであり、dyn 互換にもなる。設定ストアが
非同期なアプリ（chronogazer の `SettingsService` は sqlx）は、bootstrapper を呼ぶ前後で
インメモリの写しを hydrate / flush する（`chronogazer_core::hub::SettingsMirror`）。
`block_on` は使わない（Tauri コマンドは既に同じランタイム上で走るため）。

公開 API:

| API                                     | 役割                                                                                                 |
| --------------------------------------- | ---------------------------------------------------------------------------------------------------- |
| `connect(endpoint)`                     | 保存済みキーを再利用、無ければ状態確認 → 発行 → 保存 → catalog で確認                                |
| `connect_with_scopes(endpoint, scopes)` | 同上（スコープ明示。ホワイトリストを必ず通る唯一の発行経路）                                         |
| `status()`                              | 保存済み設定で状態確認のみ。**発行しない**                                                           |
| `refresh_catalog()`                     | 保存済み設定でタグ一覧を再取得                                                                       |
| `set_selected_tags(tags)`               | 選択タグの保存（空も可）                                                                             |
| `adopt_manual_key(endpoint, key)`       | ロックダウン後の手動連携。平文は keyring、設定には参照だけ                                           |
| `disconnect()`                          | ローカルの設定と keyring を消す。**Hub 側のキーは失効させない**                                      |
| `record()`                              | 保存済み `HubRecord` の読み出し                                                                      |
| `rest_client()`                         | 認証済み `RestClient` を 1 つ作る（#383 段階1、購読用）。未設定／keyring にエントリ無しは `Ok(None)` |

HTTP の内訳:

- 管理 API 2 つ（`/api/commissioning/status`、`/api/api-keys[/{id}/revoke]`）だけを crate 内の
  薄い `reqwest` 層で叩く。どちらにも `X-Banto-Client: banto` を付ける（管理ルーター全体に
  掛かる CSRF マーカー。`/api/v1/*` には付けない）。
- **catalog は `banto_tagclient::RestClient::fetch_catalog()` を使う**。この crate は catalog の
  ボディを自前で解釈しない。
- 例外が 1 つだけある: `fetch_catalog` は 401 と 403 を `ErrorKind::Unauthorized` 1 つに畳むため、
  §5 の「認証に失敗」と「権限が不足」を分けられない。`banto-tagclient` は変更しない方針なので、
  **`Unauthorized` を受けたときに限り** `GET /api/v1/tags` へステータスだけを読む 1 回の追加
  リクエスト（`AdminClient::probe_tags_status`）を出して 401/403 を分ける。正常系では出さない。

## 4. 発行の規則

| 規則                 | 内容                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              |
| -------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| スコープ             | `["read"]` 固定。`ISSUABLE_SCOPES` のホワイトリストに無いものはネットワーク呼び出し前に `Err`。`admin` / `write:*` は将来も追加しない（`admin` キーは MCP 経由でロックダウンを恒久的に迂回できるため）。                                                                                                                                                                                                                                                                                                                                          |
| 再利用               | `connect` はまず keyring を見る。使えるキーがあれば `POST /api/api-keys` を一切呼ばない（起動ごとの発行をしない）。                                                                                                                                                                                                                                                                                                                                                                                                                               |
| 発行の条件           | キーが無い（または無効）かつ `lockedDown: false` のときだけ。`lockedDown: true` なら発行せず `NeedsPairing`。状態が取れないとき（通信障害）も発行しない。                                                                                                                                                                                                                                                                                                                                                                                         |
| キー名               | `{app_id}-{installation_id}-{発行時刻(unix秒)}`。時刻を含むので keyring 喪失後の再発行で同名衝突しない。                                                                                                                                                                                                                                                                                                                                                                                                                                          |
| 名前重複             | Hub は 409 ではなく `Validation { field: "name" }` を返す。時刻を 1 秒進めて **1 回だけ**再試行する。                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| 旧キーの失効         | 設定に前回の `key_id` があり、未ロックダウンなら `POST /api/api-keys/{id}/revoke` を best effort で呼ぶ。失敗しても続行。                                                                                                                                                                                                                                                                                                                                                                                                                         |
| 失効の対象           | **自分が発行した id だけ**、しかも**同じ接続先に対して発行したものだけ**。`GET /api/api-keys` で名前から探して失効することは絶対にしない（他インストールのキーを巻き込まないため）。                                                                                                                                                                                                                                                                                                                                                              |
| 接続先の切り替え     | `key_id` / `key_name` / `keyring_account` は接続先に従属する（`api_keys.id` は Hub ごとの行 ID なので、別 Hub で同じ数字が別のキーを指す）。保存済みレコードと**正規化して比較**した接続先が違うときは、前のレコードの `key_id`/`key_name`/選択タグを一切持ち越さず、失効もしない。                                                                                                                                                                                                                                                               |
| keyring のアカウント | `hub:{host}:{port}{path}:{installation_id}`。`path` は正規化後の値なので必ず `/` で始まり `/` で終わる（ルートは `/`、接頭辞付きは `/hub/`）。**パス接頭辞まで含める**のは、リバースプロキシ配下で `host:port` が同じでも接頭辞だけ違う 2 つの Hub があり得るため（含めないと keyring のエントリを共有して上書きし合う）。`/` はエスケープしない（keyring のアカウントはどのバックエンドでも不透明な文字列で、この値を解析し直すこともない）。保存済みレコードの `keyring_account` は**そのまま使う**ので、古い綴りで保存されたエントリも読める。 |
| keyring 書き込み失敗 | 発行直後に `KeyStore::set` が失敗したら、発行したキーを best effort で失効させてから `Err`。誰も持っていないキーを Hub に残さない。                                                                                                                                                                                                                                                                                                                                                                                                               |
| 確認                 | 発行後に `fetch_catalog()` で**認証確認**する。到達確認だけで「認証済み」にしない。                                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| 切断                 | `disconnect()` は設定と keyring だけを消す。Hub 側のキーは残る（共有サーバー上のキーを失効させるかは管理者の判断であり、ローカル設定の削除はその判断ではない）。                                                                                                                                                                                                                                                                                                                                                                                  |

## 5. 6 状態

| 状態                     | 意味                                                                           | 画面での次の一手                                             |
| ------------------------ | ------------------------------------------------------------------------------ | ------------------------------------------------------------ |
| `notConfigured`          | 接続先が未設定                                                                 | URL を入れて「接続」                                         |
| `connected { tagCount }` | 認証済みで catalog を読めた。**`tagCount: 0` も正常**                          | タグを選んで保存（0 件なら「接続済み・利用可能なタグなし」） |
| `authFailed`             | 保存済みキーが無効（HTTP 401）                                                 | 「接続」で再発行（ロックダウン済みなら連携が必要）           |
| `forbidden`              | 認証は通るが read スコープが無い（HTTP 403）                                   | 管理者発行のキーを手入力して採用                             |
| `unreachable { cause }`  | 通信障害・応答不正（`transport`/`protocol`/`server_error`/`invalid_endpoint`） | URL と Hub の稼働を確認                                      |
| `needsPairing`           | ロックダウン済み かつ 使えるキーが無い                                         | 管理者発行のキーを手入力して採用                             |

> **v1 はレコードを 1 件しか持たない**ので、接続先を切り替えると前の接続先の `key_id` は忘れる。
> 前のキーは Hub と OS キーリング（接続先ごとに別アカウント）に残り、そちらへ戻ったときは
> キーリングのエントリが使えればそのまま再利用され、使えなければ**新しい名前で**発行し直す
> （古いキーは Hub 側に残るので、不要なら管理画面で失効させる）。放置するのは「他インストールの
> キーを触らない」と同じ原則による。

**エラーを空のタグ一覧に置き換えない**のがこの分類の要点。`HubConnection.catalog` は
`Connected` のときだけ `Some` で、失敗時は `None`（空の snapshot ではない）。UI 側の
`HubView.tags` も同じ規約で `null` と `[]` を区別する。

## 6. chronogazer 側の配線

| 層          | 実体                                                                                                                                                                                                                                                                            |
| ----------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| keyring     | `apps/chronogazer/src-tauri/src/keyring_store.rs` の `KeyringKeyStore`（サービス名は従来どおり `dev.tyaro.chronogazer`。アカウントは `hub:{host}:{port}{path}:{installation_id}`。自動ログイン用の `set_password`/`get_password`/`delete_password` は薄いラッパとして互換維持） |
| 設定        | `apps/chronogazer/core/src/hub.rs`。設定 KV の `hub.record`（`HubRecord` の JSON）と `hub.installation_id`（UUID v4）                                                                                                                                                           |
| サービス    | 同 `HubService`（`status`/`connect`/`adopt_manual_key`/`refresh_catalog`/`set_selected_tags`/`disconnect`）                                                                                                                                                                     |
| Tauri       | `hub_status` / `hub_connect` / `hub_refresh_catalog` / `hub_set_selected_tags` / `hub_adopt_manual_key` / `hub_disconnect`（すべて admin 限定）                                                                                                                                 |
| REST（LAN） | `/api/hub`（GET/DELETE）・`/api/hub/connect`・`/api/hub/adopt-key`・`/api/hub/refresh`・`/api/hub/selected-tags`（admin 限定）                                                                                                                                                  |
| 画面        | 設定カテゴリ `hub`（ラベル「Hub接続」、6 番目）。`HubSection.svelte` / `lib/banto/hubAdmin.ts`                                                                                                                                                                                  |

`installation_id` は `uuid` クレートを足さず、既にワークスペース依存にある `getrandom` で
128 ビットを取り version 4 / variant 1 のビットを立てて生成する（Cargo.lock に新しいノードを
増やさない）。

**実行形態と keyring**: keyring は `src-tauri` だけの依存（ワークスペース `Cargo.toml` の注記どおり、
`chronogazer-core` は CI コンテナでもビルドできる必要がある）。そこで `KeyStore` の実装は呼び出し元が
渡す:

- デスクトップ（`src-tauri`）… `KeyringKeyStore`。LAN ブラウザ向けの組み込みサーバーにも**同じ実体**を
  渡すので、LAN 経由でも Hub 接続を設定できる。
- `banto-serve`（Tauri 非依存の開発・E2E 用サーバー）… `hub::UnavailableKeyStore`。読み出しは
  「エントリ無し」、書き込みは失敗する。平文キーを設定 DB やファイルに落とす代替経路を作らないため。
  到達確認・ロックダウン判定・未設定判定といった「キーを保存しない範囲」はそのまま動くので、
  E2E はこのサーバーで実施できる。

## 7. 同一 PC 限定である理由

banto-hub は**未ロックダウンのまま非 loopback バインドで起動することを拒否する**
（`commissioning::enforce_loopback_when_commissioning`、[tag-server-design.md](tag-server-design.md) §5.6 制約1）。
したがって自己発行の窓が開いている間、Hub はネットワークに露出していない。

- 利点: 「LAN 越しに第三者が勝手にキーを取る」は起動時点で構造的に不可能。
- 限界: **別 PC の Banto アプリはこの方式ではカバーされない**。別 PC からは、管理者が Hub の
  管理画面で発行したキーを `adopt_manual_key`（画面の「このキーを採用」）で渡す。

ロックダウンは**この機能のためにも、解除も無認証化もしない**。ロックダウン済み Hub に対しては
自己発行を試みることすらせず `NeedsPairing` を返す。

監査: 試運転中の発行は actor が synthetic の `試運転モード` として記録されるため、どのアプリが
発行したかは actor からは判別できない。キー名は監査 detail に入るので、`{app_id}-{installation_id}-{時刻}`
という命名がそのまま追跡手段になる。

## 8. この PR に含まれないもの

- ~~**購読**（`banto_tagclient::RestClient::start` / `TagClientHandle`）~~ → **#383 段階1 で実装済み**
  （§10）。#332 の時点では `start()` を一度も呼んでいなかった（空の requests は
  `ErrorKind::InvalidTagSelection` で拒否される、という理由もあった）。**値の保存（tstore）と
  トレンド・計器表示は引き続き未実装**（#383 段階3）、**SLMP / Modbus TCP 直結も未実装**（段階2）。
- **relay-wright への配線**（**保留**）。crate は app 非依存なので、`KeyStore`/`BootstrapState` の
  実装と設定カテゴリを足すだけで同じものが使える — が、relay-wright 自体が 2026-09-17 の
  オーナー決定で凍結（構想の練り直し、plan.md §4b）。途中まで書いた配線はローカルブランチ
  `feat/332-hub-bootstrap-relay-wright` に WIP として残してある（未 push・未完成）。
- Named Pipe / 実行ファイル署名検証 / mTLS / LAN pairing / 独自 Trusted Client 認証。
- **banto-hub 側の変更**。前提は `apps/banto-hub/core/tests/client_bootstrap.rs` が固定している
  （試運転中の status は未認証で読める / `X-Banto-Client` は必須 / 未認証で `read` キーを発行できる /
  タグ 0 件でも `/api/v1/tags` は 200 + `[]` / 重複名は `validation` の `name` エラー /
  ロックダウン後は未認証の発行が 401）。

## 9. テスト

| 場所                                                        | 内容                                                                                                                                                                                                                             |
| ----------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `crates/banto-hub-bootstrap`（28 本）                       | モック Hub 相手の再利用／自己発行／ロックダウン／401・403・通信障害／keyring 喪失後の revoke + 別名再発行／名前重複の 1 回再試行／スコープ拒否／切断／手動キー採用／`rest_client()` の 3 ケース                                  |
| `apps/chronogazer/core`（hub モジュール 20 本 + REST 4 本） | `installation_id` の生成と再利用、設定 KV に平文が無いこと、空選択の保存、切断、壊れた記録の扱い、`plan_bindings` の 5 分岐、keyring 不可でも 6 状態が壊れないこと、`HubView` の JSON 形、`/api/hub/*` の admin 限定と未設定応答 |
| `apps/banto-hub/core/tests/client_bootstrap.rs`（3 本）     | Hub 側の前提（§8）の回帰固定                                                                                                                                                                                                     |
| `apps/chronogazer` vitest（`hubAdmin.test.ts`）             | 6 状態 → 画面文言のマッピング（状態が互いに潰れない／タグ 0 件が失敗にならない）、購読 7 状態 → 文言（`stopped` は理由を必ず併記）                                                                                               |
| `e2e/tests/user-settings-hub.spec.ts`                       | admin にカテゴリが出る／到達不能 URL で「Hubに到達できません」になり設定は保存されない／購読ブロックが「停止」を理由付きで出し値の表を出さない／非 admin では見えず直接遷移は弾かれる                                            |

## 10. 購読（#383 段階1）

**選んだタグを実際に購読して値を受け取る**ところまで。トレンド表示・保存（tstore）は #383 段階3、
SLMP / Modbus TCP 直結は段階2 なので、ここには含まれない。

### 10.1 世代の同一性 = 接続先 + 購読するタグ集合

**1 接続 = 1 世代**。`chronogazer_core::hub::HubService` が `TagClientHandle` を 1 つだけ所有し
（`banto-hub-bootstrap` は世代を所有しない — `rest_client()` で認証済みクライアントを渡すだけ）、
その同一性を **`(正規化した接続先, ソート済みの external name 集合)`** で判定する。

| 状況                                      | 世代                                           |
| ----------------------------------------- | ---------------------------------------------- |
| `Connected` 以外                          | 落とす（理由を出す）                           |
| `Connected` だが購読要求が空              | 落とす（**異常ではない**。理由と未解決を出す） |
| `Connected` で同一性が既存と一致          | **何もしない**                                 |
| `Connected` で同一性が違う／世代が無い    | 既存を止めてから新しく張る                     |
| `rest_client()` が `None`（keyring 不可） | 世代を持たない（理由を出す）                   |

**`status()` を叩くたびに張り直さない**のがここで一番大事な不変条件。設定画面は 6 状態の表示の
ために `GET /api/hub` を呼ぶが、同じ接続先・同じタグ集合である限り WS は再接続されない。
`TagClientHandle::restart` は使わず素直に stop → start する（`restart` は自身を消費する API で、
こちらはどのみち keyring から `RestClient` を作り直すため、所有関係が単純になる方を採る）。

### 10.2 未解決タグを潰さない

選んだ external name を catalog と突き合わせる純関数 `plan_bindings` が、要求と未解決に分ける。

- catalog に無い名前（Hub から消えた／権限で見えない）は `unresolved` に入れ、**残りだけで購読する**。
  1 個消えただけで全部止めない。
- 重複する名前は 1 つに畳む（`resolve_bindings`/`start` は重複 `binding_key` / 重複 `stable_id` を
  エラーにするため、通す前に潰す）。
- 要求が 0 件なら `start()` を呼ばない（空 requests は `InvalidTagSelection`）。このとき**未解決の
  一覧は出し続ける** — 「タグ 0 件」や空表示に潰さないのが受入条件。
- `binding_key` は external name をそのまま使う（画面の行と 1:1 に対応させるため）。

### 10.3 購読の失敗は接続の 6 状態を汚さない

購読の状態は §5 の 6 状態とは**別軸**（`HubView.subscription`）。購読が張れない理由
（タグ未選択／全部未解決／keyring 不可／未接続）は `subscription.reason` にだけ出て、
`HubStatus` は変わらない。`values` が入るのは `state === "live"` のときだけ
（`TagClientState::current()` が Live 以外で `None` を返す仕様にそのまま乗る＝**Live でないのに
古い値を出さない**）。

`banto-serve` は `UnavailableKeyStore` なので `rest_client()` が常に `None` を返し、**購読を
張れない**。これはエラーではなく理由付きの「停止」として表示する。

### 10.4 ポーリング口と起動時 resume

- **`hub_subscription` / `GET /api/hub/subscription`**（admin 限定）はメモリ上の `watch` を読むだけで
  **ネットワークを叩かない**。設定画面は Hub 設定ページを開いている間だけ 2 秒間隔でこれを読み、
  タブが隠れたら止める。catalog を毎回取り直す `GET /api/hub` はポーリングに使わない。
- `set_selected_tags` / `connect` / `refresh_catalog` / `adopt_manual_key` / `disconnect` の
  **ワイヤ形は #332 のまま**。内部で世代を突き合わせるだけ（`set_selected_tags` だけは保存後に
  catalog を 1 回読み直してから突き合わせる。ユーザー操作なので 1 往復追加は許容する。読み直しに
  失敗しても保存は成功のまま返す）。
- **起動時 resume**: `HubService::resume()` を `src-tauri` の `setup()` と `banto-serve` の起動で
  spawn する。保存済みレコードがあれば catalog を読み直して購読を張る＝**設定画面を開かなくても
  値が流れる**。**失敗しても起動は止めない**（`resume` 自身が失敗を飲んでログに出す）。
