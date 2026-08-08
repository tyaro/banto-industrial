# banto-hub

FA-Server 型の独立タグサーバー。産業用 PLC（Modbus TCP / MELSEC SLMP）から
タグを収集し、**REST / WebSocket / MQTT publish / gRPC** の4経路で外部
（MES・クラウド・自作画面等）へ公開する。**Tauri は使わない、axum + SQLite
のヘッドレス単一 exe**（`banto-hub.exe`）で、管理 UI もこのプロセス自身が
配信する。

設計の背景・決定事項は [../../docs/tag-server-design.md](../../docs/tag-server-design.md)、
現場での運用手順（API キー運用・TLS/リバースプロキシ・Windows サービス化・
インストーラ等）は [../../docs/banto-hub-operations.md](../../docs/banto-hub-operations.md)
を参照。本 README は最小限の起動方法のみを扱う。

## 起動方法

唯一の起動経路は `banto-hub` バイナリ（クレート `banto-hub-core`、
`core/src/bin/banto-hub.rs`）。

```sh
# 管理UIの静的ビルドを埋め込まずに起動（REST/WS/MQTT/gRPC の動作確認用。
# 管理UI自体はプレースホルダページになる）
cargo run -p banto-hub-core --bin banto-hub

# 管理UIも含めて動かす場合は、先にフロントエンドをビルドしてから
# embed-ui feature を付けて起動する
pnpm --filter banto-hub build
cargo run -p banto-hub-core --bin banto-hub --features embed-ui
```

引数なしはコンソールモード（Ctrl-C で停止）。Windows では
`banto-hub.exe install` / `uninstall` / `run-service` でサービス化もできる
（`core/src/bin/banto_hub/win_service.rs`。手順は
[../../docs/banto-hub-operations.md](../../docs/banto-hub-operations.md) の
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

初回起動時のみ環境変数 `BANTO_ALLOW_SETUP=1` を指定して起動し、
`POST /api/auth/setup`（管理 UI 上では初回セットアップ画面）で最初の
管理者アカウントを作成する。作成が終わったら一度サーバーを停止し、
`BANTO_ALLOW_SETUP` を外して再起動すること — 立てたままだと誰でも管理者
アカウントを作成できてしまう。

```powershell
$env:BANTO_ALLOW_SETUP = "1"
.\banto-hub.exe
```

詳細は
[../../docs/banto-hub-operations.md](../../docs/banto-hub-operations.md)
の「起動・環境変数」節を参照。

## 関連ドキュメント

- 設計: [../../docs/tag-server-design.md](../../docs/tag-server-design.md)
- 運用ガイド: [../../docs/banto-hub-operations.md](../../docs/banto-hub-operations.md)
- 全体計画（T系マイルストーン）: [../../docs/plan.md](../../docs/plan.md) §4c
