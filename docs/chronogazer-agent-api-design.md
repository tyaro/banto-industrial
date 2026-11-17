# ChronoGazer の AI エージェント向け API（API キー・MCP・OpenAPI）設計

作成日: 2026-11-17
状態: **実装中**（2026-11-17）。PR2（#1167）・PR3（#1168）・PR4（#1169・#1170、ChronoGazer の API キー）はマージ済み、PR5a（処理の切り出し、§5.4）を実装中、PR5b（MCP）は未着手。§2 の決定は 2026-11-17 のオーナー決定。実装は §9 の PR の順に進め、各 PR でこの行と §9 の表を更新する。

関連: [ChronoGazer 画面改善案](chronogazer-ui-improvement-proposal.md) §8（AI エージェントからの設定）・§5.2（LAN の通信）、[HUB の MCP リファレンス](banto-hub-mcp-reference.md)、[HUB の構成補助 MCP（T21）](banto-hub-t21-design.md)、Issue #1161。

## 1. 目的と範囲

ログインできない AI エージェントが、ChronoGazer 自身の設定（接続先・収集グループ・記録項目・表示グループ・しきい値）を読み書きし、収集を操作できるようにする。そのために次の 3 つを足す。

- **API キー**: スコープを持ち、管理者が発行・失効する。
- **MCP**: `POST /mcp`。HUB と同じ自前の JSON-RPC。
- **OpenAPI**: エージェントが REST を直接使うときのため。

ChronoGazer は PLC への書き込みを持たない。エージェントが変えられるのは ChronoGazer 自身の設定に限る。ユーザー・サーバー設定・HUB 連携・バックアップ・監査ログ・API キー自身は、エージェントから触れない（§3.3）。

`banto`（v7.2.0）には API キー・MCP・OpenAPI の仕組みが無い（2026-11-17 に確認）。そのため本リポジトリの共有 crate に置き、HUB と ChronoGazer の両方が使う。banto に同等の物が入ったら、そちらへ寄せる。

## 2. 決定事項（2026-11-17 オーナー決定）

| 項目                       | 決定                                                                                                                                          | 退けた案                                                                       |
| -------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------ |
| REST をキーで使う形        | **今の `/api/*` で受ける**。`Authorization: Bearer cg_…` のときはキーで認証し、CSRF のヘッダーを求めない。ルートは増やさない                  | HUB のように機械向けの `/api/v1/*` を別に作る（設定系のルートが 2 系統になる） |
| キーの仕組み               | **共有 crate に切り出す**。形式・ハッシュ・照合・失効・期限・保存を共有し、スコープの書式は各アプリが決める。HUB を先に移す（挙動は変えない） | ChronoGazer に HUB の作りを写す（同じ仕組みが 2 つになる）                     |
| スコープ                   | **`read` と `config` の 2 つ**                                                                                                                | `admin` を足してユーザー管理・サーバー設定・キーの発行まで許す                 |
| MCP からのキーの発行・失効 | **許さない**。発行・失効は管理者がログインして画面か REST で行う                                                                              | HUB と同じく MCP から発行できる（エージェントが自分で権限を増やせる）          |

ほかに #1161・改善案 §8 で決まっていること:

- MCP の骨組み（受け付け・ツールの登録・エラーの形）は本リポジトリの共有 crate に切り出し、ChronoGazer はツールを足すだけにする。HUB は挙動を変えない。
- ツールは REST と同じ処理を呼ぶ。改善案 §4 の「記録項目の登録とペンの割り当ての一括保存」も、作るときに UI・REST・MCP で同じ処理を使う。
- 操作は監査ログにキーで残す。

## 3. API キー

### 3.1 共有 crate `crates/banto-api-keys`

HUB の `apps/banto-hub/core/src/api_keys.rs` から、アプリに依らない部分を移す。

| 共有 crate に置く                                                                                                                                                             | 各アプリに残す                                                        |
| ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------- |
| キーの形式 `{アプリの接頭辞}_{prefix}_{secret}`（prefix は 6 バイトの乱数の base64url で 8 文字、secret は 32 バイトの乱数の base64url で 43 文字）、生成・固定長での読み取り | アプリの接頭辞（HUB は `bh`、ChronoGazer は `cg`）                    |
| SHA-256 のハッシュと定数時間の比較（理由は HUB の `api_keys.rs` 冒頭のとおり。秘密は 256 bit の乱数で、毎リクエスト照合する）                                                 | スコープの書式と意味（`trait` で渡す）                                |
| 照合の順（ハッシュ → 失効 → 一時停止 → 期限）と結果の型。ハッシュが合わないときは常に「無い」で、失効の有無を漏らさない                                                       | 一時停止（HUB の書き込みのレート超過、`tripped_at`）を付ける条件      |
| 発行・一覧・失効・削除・一時停止の解除・最終使用（60 秒に 1 回だけ書く）の SQL（SQLite、表 `api_keys`）                                                                       | 表を作るマイグレーション（各アプリの `migrations-sqlite/`、列は同じ） |
| 拒否の HTTP 応答（無い・失効・期限切れ = 401、一時停止 = 403 `key_tripped`、照合できない = 500）。HUB の `api_key_rejection_response` を移す                                  | 監査への書き方（§3.4）                                                |
| 監査に載せる主体の表示 `名前（prefix）`（失効したキーを消して同じ名前で発行し直せるので、名前だけでは区別できない）                                                           |                                                                       |

スコープは `trait ApiKeyScopes`（文字列の構文の確認と、判定に使う型への変換）で各アプリが決める。共有 crate は保存と照合のとき、スコープを文字列の並びとして扱う。

表は HUB の `0101_api_keys.sql` と同じ列（`id, name UNIQUE, prefix UNIQUE, key_hash, scopes, created_at, last_used_at, revoked_at, tripped_at, expires_at`）にする。ChronoGazer は `0103_api_keys.sql` で同じ表を作る。一時停止は ChronoGazer では使わないが、共有の SQL を一つにするため列は持つ。

### 3.2 HUB の移行（挙動を変えない）

HUB の `api_keys.rs` の該当部分を共有 crate の呼び出しに置き換える。キーの形式・ハッシュ・表・応答の形・監査はそのまま。HUB のスコープ（`read`・`read:{conn}.{grp}.{tag}`・`write:…`・`admin`・`ingest:#{id}`）の書式と判定は HUB に残し、`ApiKeyScopes` を実装する。既存の HUB のテスト（API キー・MCP・`/api/v1/*`・gRPC）が変えずに通ることで確かめる。

### 3.3 ChronoGazer のスコープ

| スコープ | できること                                                                                                                                      |
| -------- | ----------------------------------------------------------------------------------------------------------------------------------------------- |
| `read`   | 設定（接続先・収集グループ・記録項目・表示グループ・しきい値・ドライバ）と、収集の状態・現在値・接続の状態・イベント・履歴の読み取り            |
| `config` | `read` のすべてに加えて、接続先・収集グループ・記録項目・表示グループ（並べ替えを含む）・しきい値の作成・更新・削除と、収集の開始・停止・再起動 |

- `config` は `read` を含む。設定を変えるには今の設定を読む必要があるため。
- どのスコープでも、次の範囲はキーでは通さない（ログインした管理者だけ）: ユーザー管理、サーバー設定（`/api/settings/*`）、HUB 連携（`/api/hub*`、`/api/tags/from-hub`）、バックアップ、監査ログ、API キー（`/api/api-keys*`）、認証（`/api/auth/*`）、UI 設定、SSE（`/api/events`）。
- キーには対象を絞る書式（HUB の `read:{conn}.{grp}.{tag}` のような物）を設けない。ChronoGazer の設定は 1 台の中で閉じていて、絞る必要がまだ無い。要るようになったら書式を足す。

### 3.4 発行・失効

- REST: `GET,POST /api/api-keys`、`POST /api/api-keys/{id}/revoke`、`DELETE /api/api-keys/{id}`（失効したキーだけ消せる）。**ログインした管理者のセッションだけ**。キーでは呼べない（§2）。発行の応答だけが平文のキーを返す。
- 画面: メニューバーの「管理 ▾」に「API キー」を足す（ユーザー管理・監査ログと並ぶ）。名前・スコープ・期限（任意）で発行し、一覧から失効・削除する。
- 期限は任意で、省略すると無期限。期限の「未来に限る」は REST で確かめる（HUB と同じ）。
- 発行・失効・削除は監査ログに残す（主体は発行した管理者）。
- 画面で管理できるのは REST の待ち受けの画面（LAN・サービス、`server` モード）だけ。Tauri のデスクトップの画面では、そちらで管理するよう案内だけを出す（2026-11-17 オーナー決定。キーを使うのは REST の待ち受けで、デスクトップ用に Tauri のコマンドは足さない）。

## 4. 認証と権限

### 4.1 `/api/*` での受け付け

ChronoGazer の `api_router` の前に、bearer の値で道を分ける層を置く。

1. `Authorization: Bearer cg_…` → API キーとして照合する。通れば「主体 = キー」を request の extensions に載せる。通らなければ §3.1 の応答で止め、**セッションには落とさない**。
2. それ以外 → 今のとおり banto のセッション（`require_auth`）。

キーで通った要求には CSRF のヘッダー（`X-Banto-Client`）を求めない。CSRF はブラウザが資格情報を勝手に付ける場合の脅威で、キーはブラウザが勝手に付けないため（HUB の `/api/v1/*` と同じ考え）。セッションの要求は今のとおり CSRF のヘッダーが要る。

§3.3 の「キーでは通さない」範囲は、ルートの集まりごとに層を掛けて、キーの主体を 403 で止める。新しいルートを足すときに漏れないよう、**キーを通すルートの集まりを明示して許す**形にする（既定は通さない）。

閲覧公開（`server.viewer_public`）の grant はセッションの一種で、この変更の影響を受けない。

### 4.2 主体の型

今の REST のハンドラは、権限の確かめ（`require_editor`）と監査（banto の `record_write`）が「bearer のセッション」を前提にしている。banto の `record_write` は `origin` が `rest` に固定で、主体をセッションから引くので、キーの主体では使えない。

そこで ChronoGazer に主体の型を置く。

```text
enum Actor {
    Session { username, role },         // 今のログイン
    ApiKey { id, label /* 名前（prefix） */, scopes },
}
```

- 権限: `Actor::can_read()`（viewer 以上 / `read`・`config`）、`Actor::can_configure()`（editor 以上 / `config`）、`Actor::can_operate_collect()`（今の `COLLECT_OPERATION_ROLE` / `config`）。
- 今の `require_editor(...)` と `RoleGuard` の呼び出しを、`Actor` を受ける形に置き換える。セッションの判定と監査（権限不足の `denied` の記録）は今と同じ結果になること。
- 監査: `Actor` から `AuditEntry` を作る関数を ChronoGazer に置く。セッションは今の `record_write` と同じ値、キーは `actor_username = 名前（prefix）`、`actor_role = "api_key"`、`origin = "rest"` または `"mcp"`。action・resource・entity_id・detail は REST と同じ（HUB の MCP と同じ規則。HUB の T21 設計書の action の例 `mcp.create_connection` は実装と違い、実装は REST と同じ action 名に `origin: "mcp"` を付けている）。
- キーのスコープ不足は `denied` を記録する（今の `require_editor` と同じ）。存在しないキー・形式の誤りは記録しない（誰か分からないため。HUB と同じ）。失効・期限切れのキーは記録する。

banto の `record_write` が主体を受けられるようになったら、ChronoGazer の関数はそちらへ寄せる（banto 側の Issue は実装の PR で立てる）。

## 5. MCP

### 5.1 共有 crate `crates/banto-mcp`

HUB の `apps/banto-hub/core/src/mcp.rs` から骨組みを移す。

| 共有 crate に置く                                                                                                                                                  | 各アプリに残す                                                                             |
| ------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------ |
| `POST /mcp` の受け付け: 本文を自分で読む（不正な JSON でも JSON-RPC の `-32700` で返すため）、`id` の無い通知は 202、ドメインの失敗も JSON-RPC のエラーも HTTP 200 | ツールの中身、アプリの状態の型                                                             |
| `initialize`（protocolVersion を返す、`serverInfo` はアプリが渡す）・`ping`・`tools/list`・`tools/call`・それ以外は `-32601`                                       | 認証（§3 のキーを照合して主体を作る）。共有 crate は「要求から主体を作る」`trait` を受ける |
| JSON-RPC のエラーの型とコード（`-32700/-32600/-32601/-32602`）                                                                                                     |                                                                                            |
| ツールの結果の形: 成功は JSON を文字列にした `text`、失敗は `isError: true` の `text`。`BantoError::Validation` と `FieldError` を `field: message; …` に直す      |                                                                                            |
| **ツール表**: 名前・説明・入力のスキーマ・処理の関数を 1 か所に並べる。`tools/list` と `tools/call` は表から作る                                                   |                                                                                            |
| 引数の確かめの補助（必須の項目、`confirm: true` の確かめ）                                                                                                         |                                                                                            |

今の HUB は `tool_definitions()`（`Vec<json!>`）と `handle_tools_call` の `match` の 2 か所でツールを持っている。共有 crate では表 1 つにし、足すときに 1 か所だけ書けば済むようにする。

### 5.2 HUB の移行（挙動を変えない）

1. 移す前に、HUB の `tools/list` の応答全体をスナップショットのテストにする（ツールの名前・順・説明・スキーマ）。
2. HUB の 42 のツールを表に並べ直し、受け付けと結果の形を共有 crate に任せる。ツールの中身・認証（`bh_` キーだけ、セッションは不可）・監査・ロックダウンの扱いは変えない。
3. スナップショットと既存の MCP のテスト（`apps/banto-hub/host/tests/mcp.rs`、113 本）が変えずに通ることで確かめる。

### 5.3 ChronoGazer のツール

`POST /mcp` は CSRF の層の外に置き、`cg_` キーだけを受ける（セッションは不可。HUB と同じ）。ツールは §4.2 の主体でスコープを確かめる。

| ツール                                                                                              | スコープ | 処理                                       |
| --------------------------------------------------------------------------------------------------- | -------- | ------------------------------------------ |
| `get_status`                                                                                        | read     | 収集の状態（`GET /api/collect` と同じ）    |
| `list_connections` / `get_connection`                                                               | read     | 接続先                                     |
| `list_drivers`                                                                                      | read     | ドライバ                                   |
| `list_groups` / `get_group`                                                                         | read     | 収集グループ                               |
| `list_tags` / `get_tag`                                                                             | read     | 記録項目                                   |
| `list_display_groups` / `get_display_group`                                                         | read     | 表示グループ                               |
| `list_thresholds` / `get_threshold`                                                                 | read     | しきい値                                   |
| `read_values`                                                                                       | read     | 現在値（`GET /api/collect/values` と同じ） |
| `create_connection` / `update_connection` / `delete_connection`                                     | config   | 接続先                                     |
| `create_group` / `update_group` / `delete_group`                                                    | config   | 収集グループ                               |
| `create_tag` / `update_tag` / `delete_tag`                                                          | config   | 記録項目                                   |
| `create_display_group` / `update_display_group` / `delete_display_group` / `reorder_display_groups` | config   | 表示グループ                               |
| `set_threshold`                                                                                     | config   | しきい値                                   |
| `start_collection` / `stop_collection` / `restart_collection`                                       | config   | 収集の操作                                 |

- 削除は `confirm: true` を要する（HUB の T21 と同じ）。
- 今の ChronoGazer は、設定を変えても収集へ自動では反映せず、`restart` で反映する。ツールも同じにし、設定を変えるツールの結果に `restartRequired: true` を付ける。エージェントは `restart_collection` を呼んで反映する。
- 履歴（`/api/collect/history`）・イベントは初めは入れない（量が大きく、エージェントの設定の用途から外れる）。要るようになったら足す。
- 改善案 §4 の一括保存を作るときは、同じ処理を呼ぶツールを足す。

### 5.4 処理を一つにする

今の REST のハンドラは「権限の確かめ → 前検査（接続のプロトコルの制限、アドレスの検査など）→ サービスの呼び出し → 監査 → `hub.registry_changed()`」を手で並べている。MCP から同じことをするため、前検査からサービスの呼び出し・`registry_changed` までを `Actor` を受ける関数に切り出し、REST のハンドラと MCP のツールの両方がそれを呼ぶ。監査もその関数の中で書く（`origin` は呼び出し元が渡す）。これで「REST だけ前検査がある」「MCP だけ `registry_changed` を忘れる」ことを防ぐ。

デスクトップ版（`src-tauri`）のコマンドと REST の挙動を揃えるという今の約束（`rest.rs` の doc）は変えない。

## 6. OpenAPI

- utoipa を ChronoGazer に足す（HUB と同じ workspace の版）。
- 対象は、キーで通す §3.3 の REST（設定と収集の状態・操作）。管理系（ユーザー・サーバー設定・HUB 連携・API キー・認証）は載せない。
- セキュリティの定義は bearer（`cg_` キー）。CSRF のヘッダーはキーでは要らないことを説明に書く。
- 公開: `GET /api/openapi.json`（認証なし、HUB と同じく文書は秘密ではない）。Swagger UI は載せない（ChronoGazer の画面の容量を増やさないため。要るなら後で足す）。
- `openapi.json` のスナップショットのテストと、ルート一覧のスナップショット（ChronoGazer に無ければ足す）で、ルートの追加と文書のずれを止める。

## 7. 通信と公開範囲

- キーは要求ごとに平文の bearer で送られる。LAN に HTTP で出している ChronoGazer では、キーが平文で LAN を流れる。改善案 §5.2 の HUB 接続と同じ問題で、LAN の HTTPS が決まるまでは次のとおりにする。
  - キーの発行画面で、ChronoGazer が loopback 以外で HTTP で待ち受けているときは、キーが平文で流れることを示す。
  - エージェントは同じ PC（`127.0.0.1`）から使うことを既定の案内にする。
- 閲覧公開（`server.viewer_public`）が有効でも、キーと MCP の扱いは変わらない（閲覧公開は viewer のセッションで、`/mcp` は通らない）。

## 8. テスト

- 共有 crate: キーの形式・照合の順・失効・期限・一時停止・最終使用の間引き、MCP の受け付け（不正な JSON・通知・未知のメソッド・未知のツール・引数の誤り）を単体テストで。
- HUB: §3.2・§5.2 のとおり既存のテストとスナップショットが変えずに通る。
- ChronoGazer の REST: キーで読める・`read` で変えられない（403 と `denied` の監査）・`config` で変えられて監査が `名前（prefix）`・`api_key`・`rest` になる・キーで管理系に入れない・キーの要求に CSRF が要らない・セッションの要求には今どおり CSRF が要る・失効と期限切れのキーが 401。
- ChronoGazer の MCP: ツールごとの成功・スコープ不足・`confirm` 無しの削除・`restartRequired`・監査の `origin: "mcp"`、セッションの token では 401。
- 画面: API キーのページの発行・一覧・失効・削除の E2E。

## 9. PR の分け方と状況

| PR  | 中身                                                                             | 状況                                            |
| --- | -------------------------------------------------------------------------------- | ----------------------------------------------- |
| 1   | この設計書                                                                       | マージ済み（#1166）                             |
| 2   | `crates/banto-mcp` と HUB の移行（先に `tools/list` のスナップショット）         | マージ済み（#1167）                             |
| 3   | `crates/banto-api-keys` と HUB の移行                                            | マージ済み（#1168）                             |
| 4   | ChronoGazer の API キー（表・REST・画面）と `/api/*` での受け付け・`Actor`・監査 | マージ済み（#1169・#1170）                      |
| 5   | ChronoGazer の MCP のツールと処理の切り出し（§5.4）                              | PR5a（処理の切り出し）実装中／PR5b（MCP）未着手 |
| 6   | ChronoGazer の OpenAPI                                                           | 未着手                                          |

2 と 3 は互いに依存しない。4 は 3 の後、5 は 2 と 4 の後、6 は 4 の後に入れる。

## 10. 未決事項

- LAN の HTTPS（改善案 §5.2 と共通）。決まるまでは §7 のとおり。
- banto に主体を受ける監査（`record_write` の拡張）や API キーの仕組みが入る時期。入ったら §3.1・§4.2 を寄せる。
- 記録項目の対象を絞るスコープ（§3.3）。要るようになったら書式を足す。
