# banto-industrial

[banto](https://github.com/tyaro/banto)（UI・認証・保存などの共通基盤）の上に、
工場・現場向けの共通資産とアプリを置くリポジトリ。Rust の workspace と
SvelteKit / Tauri のフロントで構成する。

- docs 全体の地図・現状・各文書の「正」: [docs/README.md](docs/README.md)
- 全体計画: [docs/plan.md](docs/plan.md)
- タグサーバー banto-hub の設計: [docs/hub/tag-server-design.md](docs/hub/tag-server-design.md)

## アプリ・パッケージ

| 場所                                           | 役目                                                                                                                                                                                               |
| ---------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| [apps/banto-hub](apps/banto-hub/README.md)     | FA-Server 型のタグサーバー。PLC（Modbus TCP / MELSEC SLMP）から収集し、REST / WebSocket / MQTT / gRPC / MCP と OPC UA（任意で有効・読み取り専用）で公開する。ヘッドレスの単一 exe で管理 UI も配信 |
| apps/banto-hub-sink                            | banto-hub のタグを購読し、値を外部の PostgreSQL へまとめて書き込む別プロセスのサイドカー（`banto-hub-sink.exe`。設定ファイルで動き、Windows サービスとして登録できる）                             |
| [apps/chronogazer](apps/chronogazer/README.md) | 記録計。監視・ヒストリカル・日報・期間 CSV・表示グループ・自由トレンド。Hub 経由・SLMP・Modbus の 3 ドライバで単体でも動く                                                                         |
| [packages/trend](packages/trend/README.md)     | トレンドのチャートの部品（`@banto-industrial/trend`）。ChronoGazer と temp-scada が使う。タグ `trend-v*` で配る                                                                                    |

## クレート（crates/）

各 crate の設計意図は `src/lib.rs` 先頭のモジュールドキュメント（`//!`）に書いてある。

**タグと収集・時系列**

- `banto-tags`: タグのレジストリ（タグ・収集グループ・表示グループ）
- `banto-collect`: 収集エンジン（PLC 接続ごとに周期で読み、時系列へ書く）
- `banto-tstore`: 時系列ストレージ
- `banto-tsquery`: 時系列の期間クエリ（ヒストリカル・CSV・日報の元）
- `banto-expr`: 演算タグの式の評価
- `banto-db-source`: 外部 DB（PostgreSQL）を Source にする共有部品

**PLC とドライバ**

- `banto-plc`: PLC 通信の共通部分（アドレス・読み取り計画・デコード・シミュレータ）
- `banto-plc-address`: アドレスがどの領域か・書き込めるかの判定
- `banto-plc-write`: PLC への書き込みクライアント（読み取りだけの利用側と分けた crate）
- `banto-register-codec`: 16 ビットレジスタ列の数値デコード
- `banto-broker`: PLC への接続を CPU ごとに 1 本に束ねるブローカー
- `banto-driver-api`: ドライバの境界（型とトレイトのみ）
- `banto-driver-engine`: フレーム型ドライバを期限つきの Session で動かす実行系
- `banto-driver-slmp` / `banto-driver-modbus`: SLMP・Modbus TCP のドライバ
- `banto-driver-wasm`: Wasmtime の Component Model で動くドライバ（署名つき書き込みは別の経路）
- `banto-drivers`: ホストがエンジンへ渡すドライバの組
- `banto-driver-conformance`: Hub なしでドライバを検査する道具
- `banto-rtsp`: RTSP の基礎型（映像の入口。アプリ層の外に置く）

**Hub との接続・公開**

- `banto-tagclient`: Hub を読む（と単一タグを書く）クライアント
- `banto-hub-bootstrap`: 設定なしで Hub に接続するための API キー取得の部品
- `banto-hub-tls-trust`: Hub の loopback TLS の置き場を、書き込みせずに検証する
- `banto-hub-squat-harness`: ポートの横取りでも資格情報が漏れないことの試験用の道具
- `banto-api-keys`: 機械クライアント向け API キー（発行・失効・照合）の共有部分
- `banto-mcp`: MCP サーバーの骨組み

**デスクトップ・サービス**

- `banto-desktop-tray`: トレイ常駐アプリの窓と終了まわり
- `banto-instance-lock`: 同じデータを開くプロセスを 1 つにする排他
- `banto-exit-once`: 終了の後始末をプロセスで 1 回だけ走らせる
- `banto-service-control`: Windows の SCM の状態取得と start / stop / 自動開始
- `banto-service-elev`: 昇格ヘルパーが使う Win32 の部品

**安全と秘密**

- `banto-safe-root`: 利用者が触れうるフォルダの木を、サービスが辿らずに扱う
- `banto-secret-store`: 秘密を DPAPI で守ったファイルとして保存する

## banto の参照

banto の部品は npm（`github:tyaro/banto#<タグ>&path:packages/...`）と cargo
（`git`・`tag`）の両方を git タグで参照する。上げるときは両方を同じタグに揃える。
実際の版は `package.json`・`Cargo.toml` を正とする（例: `v7.9.0`）。
banto に必要なものが見つかったら、industrial 側で回り道を作らず banto に Issue で提案する。

## 開発の始め方

各アプリの README と [e2e/README.md](e2e/README.md) を参照。ルートで使う主なもの:

- `pnpm format:check`: prettier の検査（Markdown・TS・Svelte を編集したら `pnpm exec prettier --write` を先にかける）
- `pnpm lint`: eslint
- `pnpm check`: 各パッケージの型検査
- `pnpm test`: 各パッケージの試験

## relay-wright

PLC への自動書き込みアプリ `apps/relay-wright` は 2026-09-24 に main から外し、
タグ `archive/relay-wright-2026-09-24` に退避した（[docs/plan.md](docs/plan.md) §4b）。

## ライセンス

本リポジトリは **MIT ライセンス**（[LICENSE](LICENSE)）。ライブラリ・
アプリともに自由に利用・改変・再配布・販売できる（docs/plan.md §2）。

商用のシステム構築（導入支援・カスタム開発・保守）は tyaro が有償で
提供する。

### PLC への書き込みに関する安全上の注意

banto-hub は REST・gRPC・MCP から **稼働中の PLC へ値を書き込める**。書き込みには
タグごとの許可（`writable`）と監査記録が要り、受付全体のトグル（`write_enabled`）もあるが、
設定ミスや不具合が意図しない機器動作を引き起こしうる。なお受付トグルと新規の収集
グループの書き込み許可の既定は「有効」である（2026-09-09 の #340 で変更。
[docs/hub/banto-hub-mcp-reference.md](docs/hub/banto-hub-mcp-reference.md)）。
MIT ライセンスにより**無保証（AS IS）**で提供される。安全な導入（実機投入前の検証・
インターロック/非常停止の確保・適用法令の遵守）は利用者の責任で行うこと。
