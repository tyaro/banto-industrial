# banto-hub

FA-Server 型の独立タグサーバー。産業用 PLC（Modbus TCP / MELSEC SLMP）から
タグを収集し、**REST / WebSocket / MQTT publish / gRPC** の4経路で外部
（MES・クラウド・自作画面等）へ公開する。**Tauri は使わない、axum + SQLite
のヘッドレス単一 exe**（`banto-hub.exe`）で、管理 UI もこのプロセス自身が
配信する。

設計の背景・決定事項は [../../docs/hub/tag-server-design.md](../../docs/hub/tag-server-design.md)、
現場での運用手順（API キー運用・TLS/リバースプロキシ・Windows サービス化・
インストーラ等）は [../../docs/hub/banto-hub-operations.md](../../docs/hub/banto-hub-operations.md)
を参照。本 README は最小限の起動方法のみを扱う。

## 起動方法

唯一の起動経路は `banto-hub` バイナリ（パッケージ `banto-hub-host`、
`host/src/bin/banto-hub.rs`。本体のライブラリは `banto-hub-core`）。

```sh
# 管理UIの静的ビルドを埋め込まずに起動（REST/WS/MQTT/gRPC の動作確認用。
# 管理UI自体はプレースホルダページになる）
cargo run -p banto-hub-host --bin banto-hub

# 管理UIも含めて動かす場合は、先にフロントエンドをビルドしてから
# embed-ui feature を付けて起動する
pnpm --filter banto-hub build
cargo run -p banto-hub-host --bin banto-hub --features embed-ui
```

引数なしはコンソールモード（Ctrl-C で停止）。Windows では
`banto-hub.exe install` / `uninstall` / `run-service` でサービス化もできる
（`host/src/bin/banto_hub/win_service.rs`。手順は
[../../docs/hub/banto-hub-operations.md](../../docs/hub/banto-hub-operations.md) の
「Windows サービス化」節を参照）。

管理UIの画面だけを Vite の dev server（ホットリロード付き）で動かしたい
場合は、上記の `cargo run`（既定ポート 8722）をバックエンドとして別プロセス
で起動した状態で `pnpm --filter banto-hub dev` を実行する（`/api` への
fetch を `vite.config.ts` のプロキシ設定で 127.0.0.1:8722 へ中継する）。

## 既定のポート・bind・gRPC

| 項目                             | 既定値                                                   | 変更方法                                               |
| -------------------------------- | -------------------------------------------------------- | ------------------------------------------------------ |
| 管理UI / REST / WebSocket ポート | `8722`                                                   | 環境変数 `PORT`                                        |
| 管理UI / REST / WebSocket bind   | `127.0.0.1`                                              | 環境変数 `BANTO_BIND`                                  |
| gRPC                             | **既定無効**（有効化時ポート `50051`・bind `127.0.0.1`） | 管理UIの設定画面（gRPC設定、`PUT /api/grpc-settings`） |

gRPC は API キー認証が必須だが TLS が無いため、既定では起動せず、有効化は
管理者が設定画面で明示的に行う（有効化すると API キーが平文で流れるため、
公開範囲は bind の設定と合わせて検討すること）。

## 初回セットアップ

banto v8 から、初回セットアップの画面・`POST /api/auth/setup`・
`BANTO_ALLOW_SETUP` は無い。新しい DB の Hub は試運転モードで、loopback の
bind（既定の `127.0.0.1`）でしか起動しない（LAN の bind では起動を断る）。
最初の管理者は、**Hub・サービスを止めてから** `banto-hub create-admin` で作る。
管理者の作成とロックダウン（ログイン必須）を 1 つのトランザクションで行い、
以後は LAN の bind でも起動できる。

```powershell
Stop-Service BantoHub            # サービスで動かしているとき（管理者の PowerShell）
.\banto-hub.exe create-admin --username owner --display-name "オーナー"
Start-Service BantoHub
```

パスワードは端末からは非表示で 2 回尋ねる。スクリプトからは標準入力の 1 行目を
読む（`Get-Content password.txt | .\banto-hub.exe create-admin --username owner`）。
DB の場所は起動と同じ env（`BANTO_HUB_ROOT`・`BANTO_HUB_PROFILE`・`BANTO_DB`）で決まる
ので、サービスに env を設定しているときは同じ値で実行する。Hub が動いている間は
何も書かずに断る。

詳細は
[../../docs/hub/banto-hub-operations.md](../../docs/hub/banto-hub-operations.md)
の「起動・環境変数」節を参照。

## OPC UA 外部クライアント smoke

Python `asyncua==1.1.8` の独立クライアントで、実 Hub の OPC UA endpoint を確認する。
試験は専用一時 DB/data directory と loopback ポートを使い、REST API 経由で
試験タグを作る。

次のコマンドはリポジトリルートから実行する。

```sh
cargo build --locked -p banto-hub-host --bin banto-hub
python3.12 -m venv /tmp/banto-hub-opcua-interop-venv
/tmp/banto-hub-opcua-interop-venv/bin/python -m pip install -r interop/opcua/requirements.txt
/tmp/banto-hub-opcua-interop-venv/bin/python interop/opcua/smoke.py target/debug/banto-hub
```

自動試験の範囲と Windows Service / UaExpert の手動試験の結果は
[../../docs/testing/banto-hub-opcua-verification.md](../../docs/testing/banto-hub-opcua-verification.md)
を参照。

## 関連ドキュメント

- 設計: [../../docs/hub/tag-server-design.md](../../docs/hub/tag-server-design.md)
- 運用ガイド: [../../docs/hub/banto-hub-operations.md](../../docs/hub/banto-hub-operations.md)
- 全体計画（T系マイルストーン）: [../../docs/plan.md](../../docs/plan.md) §4c
