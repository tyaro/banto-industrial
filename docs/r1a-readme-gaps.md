# R1-A: banto README「テンプレートから自分のアプリを作る」手順の穴

> **📦 アーカイブ（2026-08-14）**: 上流 banto（v0.1.1）への外部フィードバック用チェックリストで、
> 本リポジトリの実装仕様ではない。現行の banto 依存は v1.2.0。経緯として保存。

作成日: 2026-07-13。R1-A（`apps/admin-template` → `apps/chronogazer`
コピー・リネーム・デモ削除）を実施した際に見つかった、banto の
[README.md](https://github.com/tyaro/banto/blob/v0.1.1/README.md)
（コピー手順・§1/§2/§3）と実態の乖離を記録する。banto へのフィードバック PR
を作る際のチェックリストとして使う想定。banto 側は一切変更していない
（read-only で参照したのみ）。

対象バージョン: banto `v0.1.1`（MIT 統一後）。

## チェックリスト

- [ ] **`sqlx::migrate!` の二重使用は同一DB上で衝突する（最重要・実際にテスト全滅した）**
  - README/`docs/publishing.md`/`banto_tags::migrate`のdocコメントは、消費側アプリが
    「自前の `sqlx::migrate!` を実行した直後に `banto_tags::migrate(&pool)` を同じ
    プールに対して呼ぶ」ことを前提にしている（`banto_tags::migrate`のdocコメント:
    "the consuming app calls this once at startup, mirroring
    apps/admin-template/core/src/db.rs's pattern"）。
  - しかし `sqlx` 0.8 の migration ブックキーピングテーブル（`_sqlx_migrations`）は
    **データベース全体で1つ**であり、クレート単位でテーブル名を分ける API
    （`Migrator::set_table_name`等）は存在しない（`sqlx-core-0.8.6`のソースで確認済み）。
    そのため、アプリ自身の `sqlx::migrate!`（バージョン1..4）と
    `banto_tags::migrate` 内部の `sqlx::migrate!`（バージョン1..3、別内容）を
    同じプールに対して両方実行すると、バージョン番号が衝突し
    `MigrateError::VersionMismatch`/`VersionMissing` で **必ず** 落ちる
    （空DBへの最初の1回目から発生。実測: `cargo test --workspace` で
    chronogazer-core/chronogazer の対象テストが軒並み失敗した）。
  - `crates/banto-collect/Cargo.toml`（banto-industrial側、I3b）は自分の
    `collect_events` テーブルについて全く同じ理由から意図的に
    `sqlx::migrate!` を避け `CREATE TABLE IF NOT EXISTS` を使っている、と
    コメントで説明している。**この教訓がアプリ本体側（消費側）の
    `db.rs` パターンには反映されていない** ように見える。
  - 対応（本リポジトリ）: `apps/chronogazer/core/src/db.rs` のアプリ自身の
    スキーマ適用を `sqlx::migrate!` からべた書きの冪等 DDL
    （`CREATE TABLE IF NOT EXISTS`・`role`列は`pragma_table_info`で存在確認してから
    `ALTER TABLE`）に変更し、`banto_tags::migrate` だけが `_sqlx_migrations`
    を使う形にした。`migrations/*.sql` ファイル自体はスキーマのドキュメントとして
    残したが、実行はされない（コメントで明記）。
  - banto へのフィードバック案: `banto-server`/`banto-storage`/`banto-tags`など
    I系クレートを consuming app が `sqlx::migrate!` と併用する前提のドキュメント
    （banto_tags のdocコメント、あるいは banto 本体のREADME/publishing.md）に、
    この衝突と回避策（冪等DDL、または消費側も migrate! を使わない）を明記すべき。

- [ ] **npm git依存（`github:owner/repo#tag&path:subdir`）は問題なく動作した**
  - `pnpm install` は `@banto/admin-core` 等5パッケージすべてを
    `https://codeload.github.com/tyaro/banto/tar.gz/<v0.1.1のコミット>#path:packages/<name>`
    として正しく解決した（`pnpm-lock.yaml`で確認）。docs/publishing.md の記載通り。
    pnpm 10.33.0 で動作確認済み（追加の設定・ワークアラウンド不要）。

- [ ] **Rust git依存（`{ git = ..., tag = "vX.Y.Z" }`）も問題なく動作した**
  - `banto-core`/`banto-storage`/`banto-server` を root `Cargo.toml` の
    `[workspace.dependencies]` に git タグ参照で追加し、
    `cargo check --workspace`/`cargo test --workspace` とも成功。
    追加の認証設定は不要だった（public リポジトリのため）。

- [ ] **root `Cargo.toml` の `[workspace.package]` に `repository` フィールドが無いと
      コピーした crate の `repository.workspace = true` がビルドエラーになる**
  - banto 本体の `[workspace.package]` は `repository = "https://github.com/tyaro/banto"`
    を持つが、banto-industrial の `[workspace.package]` にはこのフィールドが
    無い（`version`/`edition`/`license`/`publish`のみ）。
    `apps/admin-template/core/Cargo.toml`・`src-tauri/Cargo.toml`の
    `repository.workspace = true` をそのままコピーすると解決不能でビルド不可。
  - 対応: 両方の `Cargo.toml` から `repository.workspace = true` を削除し、
    代わりに（banto-industrial の既存クレート群の慣習に合わせて）
    `publish.workspace = true` を追加した。
  - README §1 の「Rust ワークスペース `Cargo.toml` の
    `workspace.package.repository`」への言及は、同一リポジトリ内フォークの
    ケース（banto自身をフォークする場合）を想定しており、
    「別リポジトリが consuming app として1クレートだけコピーする」ケースでは
    このフィールド自体が存在しない可能性があることに触れていない。

- [ ] **`src-tauri/Cargo.toml` の `banto-core = { path = "../../../crates/banto-core" }`
      はコピー先に `crates/banto-core` が存在しないと即壊れる**
  - admin-template の `src-tauri/Cargo.toml` は `banto-core` を
    `{ workspace = true }` ではなく明示的な相対 `path` 参照にしている
    （同一リポジトリ内なので動く）。banto-industrial 側にはこの相対パスが
    指す `crates/banto-core` が存在しない（banto-core は git タグ経由）。
  - 対応: `{ workspace = true }` に書き換え、root `Cargo.toml` の
    `banto-core` git依存エントリを共有した。
  - これ自体は R1-A の実施計画（`docs/r1-plan.md`）で「既知の追加作業」として
    事前に想定されていたが、README 自体には Rust 依存の書き換えについて
    一切記載がない（npm側の `workspace:*` 書き換えのみ言及、§1・§3参照）。

- [ ] **eslint.config.js が実際に必要とする devDependencies が、
      root package.json のリストだけからは分からない**
  - banto の root `package.json` の `devDependencies` は
    `@eslint/js`/`eslint`/`eslint-config-prettier`/`eslint-plugin-svelte`/
    `globals`/`prettier`/`prettier-plugin-svelte`/`typescript-eslint` の8つ。
    このうち `eslint-config-prettier` と `globals` は
    `eslint.config.js` が `import` している必須パッケージだが、
    README の「テンプレートから自分のアプリを作る」節はこの一覧を
    明示していない（root `package.json` を見て初めて分かる）。
  - また `typescript` 自体は banto の root `package.json` には
    **無い**（`apps/admin-template/package.json` 側のみ）。しかし pnpm の
    非hoistなワークスペース構成では、root で `eslint .` を実行したときに
    `typescript-eslint` のパーサが `typescript` を解決するには root 自身の
    devDependency として持つ必要がある（実測: 無いと動かなかったため、
    今回は明示的に追加した）。
  - 対応: banto-industrial の root `package.json` には上記全部
    （`typescript`含む）を devDependencies として追加した。

- [ ] **prettier を初めて既存リポジトリに導入すると、無関係な既存ファイルが
      大量に整形対象になる（特に `pnpm-lock.yaml` が危険）**
  - banto-industrial は R1-A 以前から `docs/plan.md`・
    `docs/recorder-requirements.md`・`README.md`・`.github/workflows/ci.yml`・
    `pnpm-lock.yaml`（Milestone 2で生成）を持っていたが、これらは
    banto からコピーした `.prettierrc.json` に対して未整形だった。
    `pnpm format`/`format:check` を実行すると、これらが軒並み検出・
    書き換え対象になる（`pnpm-lock.yaml` は1600行超の差分になった）。
  - banto 自身のリポジトリでは発生しない問題（prettier 導入時点から
    リポジトリ全体が対象だったため）。**「既存の別リポジトリに
    テンプレートをコピーする」ケース特有の落とし穴**で、README には
    一切記載が無い。
  - 対応: banto-industrial に `.prettierignore`（`pnpm-lock.yaml` を除外、
    eslint.config.js の ignores 記載と同じ理由）を新設。R1-A の範囲外の
    既存ドキュメント（docs/plan.md 等）は未整形のまま残した
    （別コミットでの対応が必要 - `pnpm format:check` は現状これらの
    4ファイルで失敗する）。

- [ ] **README §1 のブランディング置換対象ファイルの一部が実態と食い違う**
  - README: 「アプリ内の表示文言（`src/app.html` の `<title>`、
    `src/lib/components/Header.svelte`・`src/routes/login/+page.svelte`
    等の「Banto」表記）」
  - 実態:
    - `src/app.html` に `<title>` タグは **存在しない**
      （ページタイトルは実質どこにも設定されていない。唯一
      `<title>`を書いていたのは dock-svelte のポップアウト用ルート
      `routes/panel/[id]/+page.svelte` だけで、これは §3 の
      dock-svelte 削除対象そのもの）。
    - `Header.svelte` に「Banto」の文字列は無い。実際のロゴ/ブランド名
      （`🏮 Banto`）は `src/lib/components/Sidebar.svelte` にある。
    - `routes/login/+page.svelte` の記載は正しい（`🏮 Banto` が2箇所）。
  - 対応: 今回は `Sidebar.svelte`・`routes/login/+page.svelte` を
    ChronoGazer に置換。`navigation.ts`の `pageTitle()` のフォールバック値
    （`'Banto'`）も見つけて置換した（README には言及なし）。

- [ ] **README §3「`@banto/dock-svelte`」の削除手順が、Tauri側の
      `panel_open` コマンドと `routes/panel/[id]` ルートに触れていない**
  - README §3 は「`DockHost`/`dock`/`onPopOut` 関連コード、
    `src/lib/banto/panels.ts`・`src/lib/banto/popout.ts` を削除」としか
    書いていないが、実際には以下も dock-svelte のポップアウト機能専用で、
    削除しないと**呼び出し元のない孤立コード**として残ってしまう:
    - `src/routes/panel/[id]/+page.svelte`（スタンドアロンのポップアウト
      ウィンドウ用ルート。`DashboardPanel`・`panels.ts`に依存）
    - `src-tauri/src/lib.rs` の `panel_open` Tauri コマンド（
      `WebviewWindowBuilder`で`panel/{id}`ルートを開く。フロント側の
      呼び出し元（`popout.ts`）を削除すると誰も呼ばなくなる）
    - `src-tauri/capabilities/default.json` の `"windows": ["main", "panel-*"]`
      の `"panel-*"` エントリ（ポップアウトウィンドウのケイパビリティ許可）
  - 対応: 上記3つも合わせて削除した（`panel_open`本体・
    `invoke_handler`への登録・関連doc、capabilities の`panel-*`）。

- [ ] **`X-Banto-Client: banto` CSRF ヘッダは "Banto" という文字列に
      見えるが、リネームしてはいけない固定プロトコル値**
  - `apps/admin-template/core/src/rest.rs`（および banto-industrial側の
    `chronogazer-core/src/rest.rs`）がドキュメントしている
    `X-Banto-Client: banto` ヘッダは、外部の `banto_server::csrf`
    モジュール内にハードコードされた固定値であり、消費側では変更できない
    （`banto-server`はgitタグ依存で不変）。ブランディング置換で
    うっかりこの文字列まで書き換えると LAN REST 認証が壊れる。
    README にはこの区別（「これは残す」）の明記が無い。
  - 対応: 今回は変更しなかった（rest.rs内のテスト・doc comment上の
    `"X-Banto-Client"`/`"banto"` はそのまま）。

- [ ] **`keyring_store.rs` の `SERVICE_NAME` 定数は README のリネーム
      チェックリストに載っていないが、明らかにアプリ固有の識別子**
  - `src-tauri/src/keyring_store.rs` の `const SERVICE_NAME: &str =
"dev.banto.admin-template"` は自動リネームの対象になり得るファイル名
    （`admin-template`という語を含む）ではあるが、README §1 の
    リネーム箇所一覧には出てこない。OS keyring のサービス名なので、
    放置すると新アプリのものが古い banto テンプレートの識別子のまま残る。
  - 対応: `tauri.conf.json` の新 `identifier`（`dev.tyaro.chronogazer`）に
    合わせて手動で `"dev.tyaro.chronogazer"` に変更した。

- [ ] **git 依存で導入した `@banto/*` は Vite の依存事前バンドルに
      衝突し、`vite dev`（= `tauri dev`）が起動時にエラーになる**
  - banto モノレポ内では `@banto/*` は `workspace:*` リンクのため Vite の
    dep optimizer（esbuild による事前バンドル）から自動除外される。
    git 依存に切り替えると実パッケージ（`node_modules/.pnpm/...`）扱いに
    なり事前バンドル対象となるが、`@banto/*` はソース配布
    （`.svelte`/`.svelte.ts` を含む未コンパイルの TS）のため
    `vite-plugin-svelte-module:optimize-svelte` の処理でパースエラー
    （`js_parse_error`、`import` の先頭1文字が欠けた表示）になる。
    実測: `pnpm build`（本番ビルド）と `pnpm check` は通るのに
    `tauri dev` / `vite dev` だけが失敗する、という分かりにくい形で発症。
  - 対応: `apps/chronogazer/vite.config.ts` に
    `optimizeDeps.exclude: ['@banto/admin-core', '@banto/charts',
'@banto/forms', '@banto/grid-svelte', '@banto/theme']` を追加して解消。
  - banto へのフィードバック案: 各 `packages/*/package.json` に
    `"svelte"` フィールド（例: `"svelte": "./src/index.ts"`）を追加すれば
    vite-plugin-svelte が Svelte ライブラリとして認識し消費側の設定不要で
    自動除外される見込み。少なくとも README のコピー手順に
    optimizeDeps.exclude の必要性を明記すべき。

- [ ] **`banto-serve` バイナリ名は README のリネーム対象に含まれない**
  - `core/Cargo.toml` の `[[bin]] name = "banto-serve"` はテンプレート名の
    ままで良いのか、リネームすべきかが README から読み取れない。
    今回は変更しなかった（動作に支障は無いが、ブランディングとしては
    `chronogazer-serve` 等への改名が自然かもしれない）。

- [ ] **クレートのリネームで rustfmt の import 並び順が変わり、
      `cargo fmt --check` が事後に落ちる**
  - `admin_template_core` → `chronogazer_core` のリネームで、
    `use` 群のアルファベット順の位置が `banto_*` クレートと入れ替わる
    （`a...` は `banto_*` より前、`c...` は後）。コピー直後のソースは
    旧名基準の並びのままなので、`cargo fmt --all --check` が
    `banto-serve.rs`・`src-tauri/src/lib.rs` で失敗する。
    README §1 のリネーム手順には「リネーム後に `cargo fmt --all` を
    掛け直す」旨の記載が無い。
  - 同様に、items デモ削除（§3相当）で `rest.rs` のテストモジュールの
    import（`FilterOp`/`FilterState`/`Pagination`/`SortDirection`/
    `SortState`）が未使用のまま残り、`cargo clippy -D warnings` で落ちる
    （`cargo test` だけでは unused-imports は warning 止まりなので、
    CI に clippy を足すまで顕在化しない）。
  - 対応: R1-A の CI ジョブ追加時に `cargo fmt --all` を適用し、
    未使用 import を削除した（コミット「R1-A: add frontend and E2E jobs
    to CI」）。
  - banto へのフィードバック案: README のコピー/デモ削除手順の最後に
    「`cargo fmt --all` と `cargo clippy --all-targets -- -D warnings` を
    一度通す」チェック項目を足すと、この種の残骸を機械的に拾える。

- [ ] **banto の e2e スイートは README のコピー手順の対象に含まれないが、
      config はほぼそのまま流用できる（spec 側に1つ落とし穴）**
  - `e2e/playwright.config.ts`・`global-teardown.ts`・`tsconfig.json` は
    ポート番号・クレート名（`-p chronogazer-core`）・temp DB の
    プレフィックスの差し替えだけで cross-repo コピーがそのまま動いた
    （`PORT`/`BANTO_DB`/`BANTO_ALLOW_SETUP` の env 契約は
    `banto-serve.rs` 側がテンプレート由来で同一のため）。
  - 落とし穴は spec 側: banto の `smoke.spec.ts` の
    `getByRole('heading', { name: 'ダッシュボード' })` パターンは、実は
    Header.svelte が描画するページタイトルの `<h1>` にマッチしている
    （banto のダッシュボードページ自体は同名の `<h2>` を持たない）。
    ページ本体が Header と同じテキストの `<h2>` を持つ画面
    （chronogazer の 監視/ヒストリカル/イベント プレースホルダ）では
    `<h1>`/`<h2>` の2要素にマッチして strict mode violation になる。
    `{ level: 2, name: ... }` で本体側見出しに限定して解消した
    （`e2e/tests/smoke.spec.ts`）。

## 実施箇所への参照（本リポジトリ側）

- `apps/chronogazer/core/src/db.rs`: migrate!衝突の回避（冪等DDL化）
- `Cargo.toml`（root）: `banto-core`/`banto-storage`/`banto-server` の
  git依存追加、`repository`フィールド問題の回避
- `apps/chronogazer/core/Cargo.toml`・`src-tauri/Cargo.toml`:
  `repository.workspace = true` 削除、path依存→workspace依存化
- `package.json`（root）: devDependencies に `globals`・
  `eslint-config-prettier`・`typescript` を追加
- `.prettierignore`（新設）: `pnpm-lock.yaml` を除外
- `apps/chronogazer/src/lib/components/Sidebar.svelte`・
  `routes/login/+page.svelte`・`src/lib/navigation.ts`: ブランディング置換
- `apps/chronogazer/src-tauri/src/lib.rs`・`capabilities/default.json`・
  `apps/chronogazer/src/routes/panel/`・`src/lib/banto/panels.ts`・
  `popout.ts`: dock-svelte ポップアウト機構の完全削除
- `apps/chronogazer/src-tauri/src/keyring_store.rs`: `SERVICE_NAME` 更新

## 未対応・今後に残したもの

- アイコン（`src-tauri/icons/icon.ico`/`icon.png`）は banto テンプレートの
  ままで未再生成（`pnpm --filter chronogazer tauri icon <画像>` は未実施）。
- ~~`docs/plan.md`・`docs/recorder-requirements.md`・`README.md`・
  `.github/workflows/ci.yml` は prettier 未整形のまま~~ → 対応済み
  （2026-07-13、コミット「R1-A: format pre-existing docs and CI config
  with prettier」。差分は markdown テーブルの列幅揃えのみで意味変更なし。
  以後 `pnpm format:check` はリポジトリ全体で green）。
- `cargo test --workspace`/`pnpm check`/`pnpm lint`/`pnpm format:check`/
  `pnpm e2e`（Playwright smoke 4本、Windows ローカル）はすべて green
  （2026-07-13）。
- LAN モード検証済み（2026-07-13）: `pnpm build` →
  `cargo run -p chronogazer-core --bin banto-serve --features embed-ui` で
  初回セットアップ → ログイン → `/monitor` の空状態表示・
  `/historical`/`/events` プレースホルダ表示をブラウザ実機で確認。
  コンソールエラーなし。
- `tauri dev` 検証済み（2026-07-13）: 上記 optimizeDeps 修正後、
  `chronogazer.exe` が起動し vite dev ログにエラーなし
  （デスクトップウィンドウ内でのログイン操作の目視確認は未実施 —
  UI 自体は LAN モードと同一ビルド）。
