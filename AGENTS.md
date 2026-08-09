# AGENTS.md

プロジェクト全体の規約は [CLAUDE.md](CLAUDE.md)（AI の役割分担・開発規約）を参照。
本ファイルは主に Cursor Cloud 環境で作業するエージェント向けの補足を扱う。

## Cursor Cloud specific instructions

このセクションは、更新スクリプト（`pnpm install --frozen-lockfile` / `cargo fetch`）が
実行済みで、システム依存（Node 24・Tauri/WebKitGTK・Playwright Chromium）が
スナップショットに含まれた状態から起動する将来のエージェント向けの注意書き。
標準的なビルド/実行コマンドは各 README・`package.json` の scripts・CI
（[.github/workflows/ci.yml](.github/workflows/ci.yml)）に既に書かれているので、
ここでは重複させず「ハマりどころ」だけを記録する。

### プロダクト構成（要点）

- **banto-hub**（T系, `apps/banto-hub`）: Tauri なしのヘッドレス axum サーバー。
  管理 UI も同プロセスが配信する。このリポジトリで最も E2E 確認しやすい主力製品。
- **ChronoGazer**（R系, `apps/chronogazer`）: 記録計。Tauri デスクトップ版と
  LAN/ヘッドレス版（`banto-serve` バイナリ）がある。
- **relay-wright**（W系, `apps/relay-wright`）: 条件付き PLC 自動書き込み。
  同じく Tauri 版と `relay-wright-serve` 版がある。**実 PLC へ書き込む**製品なので
  [apps/relay-wright/README.md](apps/relay-wright/README.md) の安全上の注意を読むこと。
- `crates/*` はライブラリのみ（`cargo test -p <crate>` で確認、常駐プロセスではない）。

### Node のバージョン（重要なハマりどころ）

- 本リポジトリは `node >= 24`（`package.json` の `engines`、CI も Node 24）。
- この VM には `/exec-daemon/node`（**古い v22**）が入っており、PATH 上で nvm より
  前に来る。そのため素の `node` は v22 を指すことがある。**フロントエンドの
  build/dev/test/e2e を動かす前に Node 24 を有効化すること**:
  ```sh
  nvm use 24    # default alias は 24 に設定済み
  ```
  対話シェルでは `~/.bashrc` が nvm の default(24) を PATH 先頭へ寄せるよう設定済み。
  非対話（スクリプト）で使うときは明示的に `nvm use 24` するのが確実。
  `pnpm install` 自体は v22 でも通るが、Vite ビルド等は Node 24 で揃えること。
- `pnpm` は corepack 由来（`packageManager: pnpm@10.33.0`）。

### Rust ツールチェイン

- `rust-toolchain.toml` で **1.94.1 にピン留め**（rustup が自動同期）。
- CI の Rust ジョブは **windows-latest** で全ワークスペースを回す。この Linux VM でも
  `cargo build/test --workspace` は通る（3 つの `src-tauri` クレートを含む）。ただし
  それには **Tauri/WebKitGTK のシステム依存**が必要で、本スナップショットには
  導入済み（`libwebkit2gtk-4.1-dev` / `libgtk-3-dev` / `libsoup-3.0-dev` /
  `libayatana-appindicator3-dev` / `librsvg2-dev` / `libxdo-dev` / `libssl-dev` /
  `build-essential` 等）。もし未導入の環境に当たったら `apt-get` で入れ直す。
- gRPC の `protoc` はシステムに無くてよい（`protoc-bin-vendored` をビルドスクリプトが
  使う。`Cargo.toml` のコメント参照）。

### banto-hub を動かす（hello-world 済み）

```sh
pnpm --filter banto-hub build     # 先に管理 UI をビルド（Node 24 で）
cargo run -p banto-hub-core --bin banto-hub --features embed-ui
```

- 既定 `PORT=8722` / `BANTO_BIND=127.0.0.1` / DB `./banto-hub.sqlite3` /
  data `./data`。リポジトリを汚さないため、動作確認では `BANTO_DB` と
  `BANTO_HUB_DATA` を `/tmp/...` 等の作業ディレクトリに向けるのがよい。
- **初回セットアップは `BANTO_ALLOW_SETUP=1` を付けて起動**し、管理 UI の初回
  セットアップ画面（または `POST /api/auth/setup`）で最初の管理者を作る。作成後は
  `BANTO_ALLOW_SETUP` を外して再起動する。
- **`/api/*` は CSRF 保護あり**。`curl` で素朴に叩くと
  `{"message":"CSRFヘッダがありません"}` で弾かれる（`/api/auth/status` でも）。
  API 動作確認は管理 UI（ブラウザ）経由か、UI が付与する CSRF ヘッダを再現すること。
  認証不要の `GET /api/v1/openapi.json` は疎通確認に使える。
- **`--features embed-ui` の有無で banto-hub-core が再コンパイル**される（feature 差分）。
  UI 込みで動かす経路と、UI 無し（プレースホルダ）経路を行き来すると毎回ビルドが走る。
- UI をホットリロードで触りたいときは、上記バックエンドを起動したまま
  `pnpm --filter banto-hub dev`（Vite が `/api` を `127.0.0.1:8722` へプロキシ）。

### ChronoGazer / relay-wright（LAN・ヘッドレス）

```sh
pnpm --filter chronogazer build
cargo run -p chronogazer-core --bin banto-serve --features embed-ui   # 既定 PORT=8721
```

- Tauri デスクトップ版は `pnpm --filter <app> tauri dev`。**chronogazer と
  relay-wright はどちらも Vite の既定ポートが 1420** なので、2 つ同時に
  `tauri dev` するとポート衝突する。片方ずつ動かすこと。

### テスト

- フロント: `pnpm lint` / `pnpm format:check` / `pnpm check` / `pnpm --recursive test`
  / `pnpm build`（vitest は現状 banto-hub のみ）。
- Rust: `cargo test --workspace`（`cargo fmt --all --check` / `cargo clippy` も CI 準拠）。
- E2E（ChronoGazer smoke, `pnpm e2e`）: 設定が `target/debug/banto-serve` を**直接起動**
  するため、事前に `pnpm build` と
  `cargo build -p chronogazer-core --bin banto-serve --features embed-ui` を済ませ、
  `pnpm exec playwright install chromium`（本スナップショットは導入済み）が必要。
