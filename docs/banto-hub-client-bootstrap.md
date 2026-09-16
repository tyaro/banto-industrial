# Banto クライアントの Hub 自動接続（#332）

状態: **chronogazer 分のみ実装済み**（共有 crate `crates/banto-hub-bootstrap` + chronogazer の
設定カテゴリ「Hub 接続」）。relay-wright への配線は別 PR、選んだタグをデータ源へ繋ぐ購読は別 issue。
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
    pub keyring_account: String,           // hub:{host}:{port}:{installation_id}
    pub selected_tags: Vec<String>,
}
```

両 trait は**同期**。keyring 呼び出しは元々ブロッキングであり、dyn 互換にもなる。設定ストアが
非同期なアプリ（chronogazer の `SettingsService` は sqlx）は、bootstrapper を呼ぶ前後で
インメモリの写しを hydrate / flush する（`chronogazer_core::hub::SettingsMirror`）。
`block_on` は使わない（Tauri コマンドは既に同じランタイム上で走るため）。

公開 API:

| API                                     | 役割                                                                  |
| --------------------------------------- | --------------------------------------------------------------------- |
| `connect(endpoint)`                     | 保存済みキーを再利用、無ければ状態確認 → 発行 → 保存 → catalog で確認 |
| `connect_with_scopes(endpoint, scopes)` | 同上（スコープ明示。ホワイトリストを必ず通る唯一の発行経路）          |
| `status()`                              | 保存済み設定で状態確認のみ。**発行しない**                            |
| `refresh_catalog()`                     | 保存済み設定でタグ一覧を再取得                                        |
| `set_selected_tags(tags)`               | 選択タグの保存（空も可）                                              |
| `adopt_manual_key(endpoint, key)`       | ロックダウン後の手動連携。平文は keyring、設定には参照だけ            |
| `disconnect()`                          | ローカルの設定と keyring を消す。**Hub 側のキーは失効させない**       |
| `record()`                              | 保存済み `HubRecord` の読み出し                                       |

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

| 規則                 | 内容                                                                                                                                                                                                     |
| -------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| スコープ             | `["read"]` 固定。`ISSUABLE_SCOPES` のホワイトリストに無いものはネットワーク呼び出し前に `Err`。`admin` / `write:*` は将来も追加しない（`admin` キーは MCP 経由でロックダウンを恒久的に迂回できるため）。 |
| 再利用               | `connect` はまず keyring を見る。使えるキーがあれば `POST /api/api-keys` を一切呼ばない（起動ごとの発行をしない）。                                                                                      |
| 発行の条件           | キーが無い（または無効）かつ `lockedDown: false` のときだけ。`lockedDown: true` なら発行せず `NeedsPairing`。状態が取れないとき（通信障害）も発行しない。                                                |
| キー名               | `{app_id}-{installation_id}-{発行時刻(unix秒)}`。時刻を含むので keyring 喪失後の再発行で同名衝突しない。                                                                                                 |
| 名前重複             | Hub は 409 ではなく `Validation { field: "name" }` を返す。時刻を 1 秒進めて **1 回だけ**再試行する。                                                                                                    |
| 旧キーの失効         | 設定に前回の `key_id` があり、未ロックダウンなら `POST /api/api-keys/{id}/revoke` を best effort で呼ぶ。失敗しても続行。                                                                                |
| 失効の対象           | **自分が発行した id だけ**。`GET /api/api-keys` で名前から探して失効することは絶対にしない（他インストールのキーを巻き込まないため）。                                                                   |
| keyring 書き込み失敗 | 発行直後に `KeyStore::set` が失敗したら、発行したキーを best effort で失効させてから `Err`。誰も持っていないキーを Hub に残さない。                                                                      |
| 確認                 | 発行後に `fetch_catalog()` で**認証確認**する。到達確認だけで「認証済み」にしない。                                                                                                                      |
| 切断                 | `disconnect()` は設定と keyring だけを消す。Hub 側のキーは残る（共有サーバー上のキーを失効させるかは管理者の判断であり、ローカル設定の削除はその判断ではない）。                                         |

## 5. 6 状態

| 状態                     | 意味                                                                           | 画面での次の一手                                             |
| ------------------------ | ------------------------------------------------------------------------------ | ------------------------------------------------------------ |
| `notConfigured`          | 接続先が未設定                                                                 | URL を入れて「接続」                                         |
| `connected { tagCount }` | 認証済みで catalog を読めた。**`tagCount: 0` も正常**                          | タグを選んで保存（0 件なら「接続済み・利用可能なタグなし」） |
| `authFailed`             | 保存済みキーが無効（HTTP 401）                                                 | 「接続」で再発行（ロックダウン済みなら連携が必要）           |
| `forbidden`              | 認証は通るが read スコープが無い（HTTP 403）                                   | 管理者発行のキーを手入力して採用                             |
| `unreachable { cause }`  | 通信障害・応答不正（`transport`/`protocol`/`server_error`/`invalid_endpoint`） | URL と Hub の稼働を確認                                      |
| `needsPairing`           | ロックダウン済み かつ 使えるキーが無い                                         | 管理者発行のキーを手入力して採用                             |

**エラーを空のタグ一覧に置き換えない**のがこの分類の要点。`HubConnection.catalog` は
`Connected` のときだけ `Some` で、失敗時は `None`（空の snapshot ではない）。UI 側の
`HubView.tags` も同じ規約で `null` と `[]` を区別する。

## 6. chronogazer 側の配線

| 層          | 実体                                                                                                                                                                                                                                                                      |
| ----------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| keyring     | `apps/chronogazer/src-tauri/src/keyring_store.rs` の `KeyringKeyStore`（サービス名は従来どおり `dev.tyaro.chronogazer`。アカウントは `hub:{host}:{port}:{installation_id}`。自動ログイン用の `set_password`/`get_password`/`delete_password` は薄いラッパとして互換維持） |
| 設定        | `apps/chronogazer/core/src/hub.rs`。設定 KV の `hub.record`（`HubRecord` の JSON）と `hub.installation_id`（UUID v4）                                                                                                                                                     |
| サービス    | 同 `HubService`（`status`/`connect`/`adopt_manual_key`/`refresh_catalog`/`set_selected_tags`/`disconnect`）                                                                                                                                                               |
| Tauri       | `hub_status` / `hub_connect` / `hub_refresh_catalog` / `hub_set_selected_tags` / `hub_adopt_manual_key` / `hub_disconnect`（すべて admin 限定）                                                                                                                           |
| REST（LAN） | `/api/hub`（GET/DELETE）・`/api/hub/connect`・`/api/hub/adopt-key`・`/api/hub/refresh`・`/api/hub/selected-tags`（admin 限定）                                                                                                                                            |
| 画面        | 設定カテゴリ `hub`（ラベル「Hub接続」、6 番目）。`HubSection.svelte` / `lib/banto/hubAdmin.ts`                                                                                                                                                                            |

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

- **購読**（`banto_tagclient::RestClient::start` / `TagClientHandle`）。選んだタグをトレンド等の
  データ源へ繋ぐのは別 issue。この PR は `start()` を一度も呼ばない（空の requests は
  `ErrorKind::InvalidTagSelection` で拒否される、という理由もある）。
- **relay-wright への配線**（別 PR）。crate は app 非依存なので、`KeyStore`/`BootstrapState` の
  実装と設定カテゴリを足すだけで同じものが使える。
- Named Pipe / 実行ファイル署名検証 / mTLS / LAN pairing / 独自 Trusted Client 認証。
- **banto-hub 側の変更**。前提は `apps/banto-hub/core/tests/client_bootstrap.rs` が固定している
  （試運転中の status は未認証で読める / `X-Banto-Client` は必須 / 未認証で `read` キーを発行できる /
  タグ 0 件でも `/api/v1/tags` は 200 + `[]` / 重複名は `validation` の `name` エラー /
  ロックダウン後は未認証の発行が 401）。

## 9. テスト

| 場所                                                       | 内容                                                                                                                                                               |
| ---------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `crates/banto-hub-bootstrap`（27 本）                      | モック Hub 相手の再利用／自己発行／ロックダウン／401・403・通信障害／keyring 喪失後の revoke + 別名再発行／名前重複の 1 回再試行／スコープ拒否／切断／手動キー採用 |
| `apps/chronogazer/core`（hub モジュール 9 本 + REST 3 本） | `installation_id` の生成と再利用、設定 KV に平文が無いこと、空選択の保存、切断、壊れた記録の扱い、`/api/hub/*` の admin 限定と未設定応答                           |
| `apps/banto-hub/core/tests/client_bootstrap.rs`（3 本）    | Hub 側の前提（§8）の回帰固定                                                                                                                                       |
| `apps/chronogazer` vitest（`hubAdmin.test.ts`）            | 6 状態 → 画面文言のマッピング（状態が互いに潰れない／タグ 0 件が失敗にならない）                                                                                   |
| `e2e/tests/user-settings-hub.spec.ts`                      | admin にカテゴリが出る／到達不能 URL で「Hubに到達できません」になり設定は保存されない／非 admin では見えず直接遷移は弾かれる                                      |
