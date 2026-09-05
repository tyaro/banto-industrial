# banto-hub T21 設計: 構成補助 MCP（管理面ツール）

作成日: 2026-09-05
状態: **計画（決定済み・実装可、2026-09-05）**。オーナー決定は §2・§8、安全境界の設計は §3（本 slice の要）。
対象: MCP から接続/グループ/タグ・各種設定・収集/書き込み制御・API キーを操作する
「構成補助」ツール群。AI エージェントや MES セットアップツールが**会話的に banto-hub を
構成**できるようにする。
関連: [banto-hub-mcp-reference.md](banto-hub-mcp-reference.md)（既存 MCP データ面）、
[tag-server-design.md](tag-server-design.md) §5.6（試運転モードとロックダウン）、
[banto-hub-t20-design.md](banto-hub-t20-design.md) §3.7（MCP は抜け道を作らない）。

---

## 1. 背景と狙い

現状 MCP は**データ面**（値の read/write）＋状態参照に限定し、**構成変更（管理面）は
commissioning のバイパスか管理者 Bearer トークンでしか触れない**という境界を意図的に
引いている。一方 2026-09-05 の T20 実機検証では、接続/グループ/タグ作成・収集開始・
write 有効化・API キー発行を admin REST で手作業した — これを MCP クライアント（AI
エージェント、MES 設定ツール）から会話的に行えると、セットアップと保全が大幅に楽になる。

**狙い**: 既存 admin REST の機能を MCP ツールとして公開し、`tools/list` で発見でき、
`bh_` API キー1本で構成まで完結できるようにする。実装は既存の admin ハンドラの
**薄い MCP ラッパー**（検証・DB 操作・pending queue 挙動は共有）。

## 2. オーナー決定（2026-09-05）

- **対象範囲 = フル**: 接続/グループ/タグの CRUD ＋ 設定（gRPC/MQTT/retention）＋
  収集 start/stop・write-control ＋ **API キー発行/失効** まで。理由: **開発補助目的**。
- **安全境界 = 常時可 ＋ admin スコープ ＋ 監査**: commissioning 限定にはせず、
  ロックダウン後も `admin` スコープの API キーなら構成変更を許可する。全操作を監査する。
- 進め方 = **設計文書を先に作る**（本文書）→ 決定 → 実装。
- **有効化ガードは設けない（無条件常時可）**: §3.2 の `mcp_admin_enabled` は**不採用**。
  admin スコープを持つキーがあれば環境を問わず構成 MCP が使える。
- **API キー発行は無制限**: MCP の `create_api_key` は admin を含む任意スコープを発行できる
  （admin キーの自己増殖を許容）。
- **不可逆操作は confirm 必須**: `lock_down` と delete 系は `{"confirm":true}` を要求する。

## 3. 安全境界の設計（本 slice の要）

「常時可＋フル＋API キー発行」は、**`admin` スコープの `bh_` キー1本がサーバー全権**を
持つことを意味する（任意 host:port への接続作成／別 admin キーの発行／設定変更／実質的な
ロックダウン回避）。開発補助には妥当だが、本番露出のリスクが大きい。以下のガードを敷く。

### 3.1 新スコープ `admin`

- API キーに新スコープ `admin` を追加（`banto_hub_core::api_keys::validate_scope`）。
  構成 MCP ツールは**すべて `admin` スコープを要求**する（有効化ガードは無いので
  `admin` スコープ＋監査＋confirm が唯一のガード＝オーナー決定 §2）。read/write データ
  スコープでは構成に触れない（既存境界を維持）。
- `admin` は**既定では絶対に付与しない**。発行者が明示列挙したときのみ付く。
- 監査: `admin` スコープ付きキーの発行自体も audit_log に残す。

### 3.2 有効化ガード（`mcp_admin_enabled`）— 不採用（オーナー決定 2026-09-05）

当初は「構成 MCP 面はサーバー設定 `mcp_admin_enabled`（既定 OFF）で ON にしたときのみ
存在する」ガードを推奨したが、**オーナー決定により不採用**。構成 MCP は
**無条件で常時利用可能**（admin スコープのキーがあれば環境を問わず使える）。
これに伴い、唯一のガードは §3.1 の `admin` スコープ・§3.3 の監査・§4 の confirm となる。
本番でも admin スコープのキーを発行した時点で全権 MCP 面が開くことを、運用側が
認識して admin キーの配布を厳格に管理する前提とする。

### 3.3 監査（全構成操作）

- 構成 MCP の全操作を **audit_log**（既存の管理監査、`crate::audit`）に記録する:
  actor = API キー名スナップショット、action（例 `mcp.create_connection`）、target、結果。
- 値の書き込み（write_tag_value 等）は従来どおり hub_write_audit に残る（二系統は分離のまま）。

### 3.4 ロックダウンとの関係（明示）

- オーナー決定によりロックダウン後も `admin` スコープで構成変更可。これは
  「ロックダウン＝構成凍結」という §5.6 の既定思想からの**意図的な緩和**であり、
  `admin` スコープ＋`mcp_admin_enabled` の二重ゲートで限定する。
- `get_server_status` の `lockedDown` は従来どおり返す（クライアントが判断できる）。
- **自己昇格の残リスク（受容）**: admin キーは別の admin キーを発行でき、設定も変えられる。
  開発補助目的として受容するが、§3.2 の既定 OFF と §3.3 の監査で影響を可観測にする。

### 3.5 接続先ホストの扱い

- 接続作成は任意 host:port を指せる（admin REST と同じ）。開発補助のため allowlist は
  設けない。リスクは §3.2 の有効化ゲートと監査でカバーする（allowlist は将来の拡張余地）。

## 4. ツール一覧（既存 admin REST への対応）

すべて `admin` スコープ必須（有効化ガードは無し＝§3.2）。delete 系と `lock_down` は
`{"confirm":true}` を要求する。応答は既存 MCP と同じ JSON 文字列。

| MCP ツール（案）                                  | 対応 admin REST                          | 備考                                                             |
| ------------------------------------------------- | ---------------------------------------- | ---------------------------------------------------------------- |
| `list_connections`                                | GET /api/plc-connections                 | 読み取り                                                         |
| `create_connection`                               | POST /api/plc-connections                | 任意 host:port                                                   |
| `update_connection`                               | PUT /api/plc-connections/{id}            |                                                                  |
| `delete_connection`                               | DELETE /api/plc-connections/{id}         | 収集中は pending queue 挙動を継承                                |
| `test_connection`                                 | POST /api/plc-connections/test           | 保存前の疎通確認（副作用なし）                                   |
| `list_groups`                                     | GET /api/collection-groups               |                                                                  |
| `create_group`                                    | POST /api/collection-groups              |                                                                  |
| `update_group`/`delete_group`                     | PUT/DELETE /api/collection-groups/{id}   |                                                                  |
| `create_tag`                                      | POST /api/tags                           | `list_tags` は既存                                               |
| `update_tag`/`delete_tag`                         | PUT/DELETE /api/tags/{id}                |                                                                  |
| `create_tags_batch`                               | POST /api/tags/batch                     | 連続/構造体/CSV 相当の一括登録                                   |
| `set_collection`                                  | POST /api/collection/start, /stop        | start は write_enabled をリセット                                |
| `set_write_control`                               | POST /api/write-control/enable, /disable |                                                                  |
| `get_grpc_settings`/`set_grpc_settings`           | GET/PUT /api/grpc-settings               |                                                                  |
| `get_mqtt_settings`/`set_mqtt_settings`           | GET/PUT /api/mqtt-settings               |                                                                  |
| `get_retention`/`set_retention`                   | 設定（T20-d）                            |                                                                  |
| `create_api_key`/`list_api_keys`/`revoke_api_key` | POST/GET /api/api-keys, revoke           | **任意スコープ発行可（admin 含む・自己増殖許容＝オーナー決定）** |
| `lock_down`                                       | POST /api/commissioning/lock-down        | 不可逆。`{"confirm":true}` 必須（§8）                            |

（`users`（アカウント作成）・`reset-password` は**対象外**＝パスワード/アカウント操作は
MCP に載せない。既存の安全規約と一致。）

## 5. 実装方針

- 既存 admin ハンドラのロジック（`banto_tags` の CRUD、検証、pending queue 投入）を
  共有し、MCP ツールは入力スキーマ → 既存ハンドラ相当の呼び出し → 構造化 JSON 応答、の
  薄いラッパーにする（`crate::mcp` に追加）。REST と二重実装しない。
- 収集中の登録変更は REST と同じく **pending queue（202 相当）**になる。MCP 応答でも
  「pending に入った」ことを明示する。
- エラーは既存の wire コード（`value_out_of_range` 等）と揃える。

## 6. スライス構成（案）

1. **T21-S1**: `admin` スコープ ＋ 監査基盤 ＋ 接続/グループ/タグ CRUD
   （`test_connection` 含む、delete は confirm）。← セットアップの中核。
2. **T21-S2**: 設定（gRPC/MQTT/retention）＋ 収集 start/stop ＋ write-control。
3. **T21-S3**: API キー発行/失効 ＋ `lock_down`（confirm 引数）。← 最も強力。

## 7. 更新対象ドキュメント

- 本文書（設計・決定台帳）。
- [banto-hub-mcp-reference.md](banto-hub-mcp-reference.md): 構成ツール節を追加（実装後）。
- [tag-server-design.md](tag-server-design.md) §5.6: ロックダウン緩和（admin スコープ）の追記。
- docs/README.md 索引。

## 8. 決定事項（2026-09-05、オーナー確認済み）

- **有効化ガード（`mcp_admin_enabled`）は不採用** — 無条件常時可（§3.2）。
- **API キー発行は MCP から無制限**（admin スコープ含む任意スコープを発行可、自己増殖許容）。
- **不可逆操作は confirm 必須** — `lock_down` と delete 系に `{"confirm":true}`。

残る安全策は §3.1 `admin` スコープ（既定付与しない）・§3.3 監査（全構成操作を audit_log）・
§4 confirm の3点。運用は admin キーの配布厳格管理を前提とする。
