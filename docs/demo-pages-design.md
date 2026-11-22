# ブラウザで触れるデモ（GitHub Pages）の設計

作成日: 2026-11-22
状態: **実装中**（2026-11-23、§6 の 2 を 2a・2b に分けた。2a = 共通パッケージ `packages/demo-backend`・両アプリのデモ用の起動とログイン無し・対象のルートごとの静的な入口・静的な配信の上の E2E（#1308 でマージ済み）。2b-1 = `deploy-demo.yml`・普段の CI の `e2e-demo-pages` ジョブ・implementation-checklist の 1 行（#1309 でマージ済み）。2b-2 = 公開の関門（メニューを対象だけに・対象外の案内・「デモを最初から」・`public-gate.spec.ts`。#1310 でマージ済み）。§6 の 3 = Hub の `/tags` の中身（登録・編集・削除と WebSocket の値。#1311 でマージ済み）。§6 の 4 を 4a・4b に分けた。4a = ChronoGazer の旧 demo モードを消す（#1312）。4b = ChronoGazer の `/tags` の模擬（#1313 でマージ済み）。§6 の 5 = ChronoGazer の `/monitor` の履歴の模擬（本 PR））。2026-11-22 オーナー決定（目的・範囲・配信の契機・ログイン無し）を記録し、作りの案を置いた。同日、オーナーのレビュー（#1301）で、直に開く・読み直すことの保証（§3.5）と、本物の API に出ないことの制約（§3.1・§3.2）を足した。実装は §6 の順に小さな PR で進める。

関連: banto の `deploy-demo.yml`（admin-template を InMemory で配信）、temp-scada の `pages.yml`・`docs/guides/demo-pages.md`（模擬値の画面を配信）。

## 1. 目的とオーナー決定（2026-11-22）

banto・temp-scada と同じく、インストールせずにブラウザで画面を触れるデモを GitHub Pages に置く。

- **範囲**: Hub は**タグ登録とモニタ**（`/tags`。#1255 でモニタは `/tags` に吸収済み）。ChronoGazer は**タグ登録とトレンドの模擬**（`/tags` と `/monitor`）。ほかの画面はデモの対象外。
- **ログイン無し**: 開いたらすぐ画面に入る。ログイン画面・初回の管理者作成は出さない。
- **配信の契機**: PR・main への push では配信しない。**リリースのたびに手で回す**（`workflow_dispatch`）。
- banto v8 で部品の口が変わる見込みがあるので、基盤は v8 の影響を受けにくい場所（§3）に置く。

## 2. いまの状態（2026-11-22 の調べ）

- このリポジトリの Pages は有効（`https://tyaro.github.io/banto-industrial/`、ソースは GitHub Actions）。配信するワークフローは無い。
- 両アプリとも adapter-static・`fallback: 'index.html'` の SPA で、`paths.base` は未設定。
- fetch は 1 か所にまとまっていない。Hub は `lib/banto/*Admin.ts` ごとに `httpRequest` を持つ（約 17 ファイル）。ChronoGazer は `tagRegistryAdmin.ts`・`collectAdmin.ts` などに分かれる。パスは `/api/...` を直書きしている。`@banto/admin-core` の provider も `/api/auth/*`・`/api/ui-settings/*`・`/api/events` を fetch する。
- Hub の値は WebSocket（`/api/v1/stream`、`tagMonitorAdmin.ts`）、ChronoGazer の値は `GET /api/collect/values` のポーリング、履歴は `GET /api/collect/history`。
- ChronoGazer には `VITE_BANTO_DEMO=1` の demo がある（`isDemoBuild()`、InMemory の空データ・admin/admin のログイン）。ただ、`collectAdmin.ts`・`tagRegistryAdmin.ts` が demo ではすべての呼び出しを断るので、タグ登録もトレンドも動かない（`apps/chronogazer/README.md` の起動の節）。Hub に demo は無い。

## 3. 作りの案

### 3.1 模擬は通信の境目に置く

`fetch`・`WebSocket`・（SSE が `EventSource` なら）`EventSource` を、デモ用のビルドのときだけ起動の最初に差し替える。差し替えた口が `/api/...` をメモリ上の模擬サーバーへ振り分ける。

- 各 `*Admin.ts` と banto の provider には手を入れない。画面と部品から見ると、本物のサーバーと話しているのと同じになる。
- banto v8 で部品や provider の形が変わっても、Hub・ChronoGazer の HTTP の口が同じなら模擬は直さなくてよい。直すのは、自分たちの API の口を変えたときだけ。
- パスが `/api/...` の直書きでも、ネットワークに出る前に横取りするのでベースパスと関係なく動く。
- 模擬のコードは `import.meta.env.VITE_BANTO_DEMO === '1'` のときだけ動的に import する。製品のビルドには入らない（実装の PR でバンドルに含まれないことを確かめる）。

**本物の API に出ないことは実装の制約にする**（2026-11-22 オーナーのレビュー）。観測で確かめるだけにしない。

- 振り分けは「模擬に無い口は本物へ流す」にしない。`/api/` で始まるもの（同じオリジンでも別のオリジンでも）は、模擬に無ければ 501（§3.4）を返し、元の `fetch` を呼ばない。元の `fetch` へ渡すのは、ベースパスの下の静的な資産（JS・CSS・画像・フォント）だけ。それ以外の宛先は失敗で返す。
- 差し替えた `WebSocket`・`EventSource` は本物の接続を一度も開かない。模擬に無いパスは、開いた直後に閉じる。
- 起動の順は「模擬を入れ終える → 通常の provider（HTTP 認証・UI 設定・イベント）を作る」。模擬の import か初期化が失敗したら、通常の provider を起動せず、デモを始められない旨の画面を出して止まる（`setup.ts` を呼ばない）。
- デモのビルドでは、Tauri・本物のサーバーを探す処理（ChronoGazer の `probeBackend` など）も模擬に答えさせ、実際のサーバーへの問い合わせを残さない。

置き場所は、振り分けと値の生成の共通部分を `packages/` に新しいパッケージ（仮に `@banto-industrial/demo-backend`）として置き、口ごとの応答は `apps/<app>/src/lib/demo/` に置く。

### 3.2 ログイン無しで入る

模擬の `/api/auth/*` が、いつもログイン済みの admin を返す（`identity` は `{ id, name, role: 'admin' }`）。

- Hub の通常の「ログイン無し」は試運転の grant（`grants.commissioning`）なので、そのまま使うと試運転の表示やロックダウンの操作が画面に出る。デモでは使わず、ログイン済みの利用者として始める。トークンの置き方（起動の前に保存しておくか、grant の応答で渡すか）は、画面に試運転の表示が出ない方を実装の PR で選ぶ。
- ChronoGazer は `localDesktop` の grant でもよいが、Hub と揃えてログイン済みの admin にする。
- ChronoGazer の今の demo（InMemory・admin/admin のログイン・全部断る）は、この作りで置き換えて消す（互換は考えない）。
- **模擬の利用者・認証はメモリだけに持つ**（2026-11-22 オーナーのレビュー）。Pages は `tyaro.github.io` のオリジンを banto・temp-scada のデモと共有するので、ほかのデモの設定やセッションを拾わない・残さない。
  - 模擬はトークンの中身で何も判断しない（どのトークンでも admin、トークンが無くても admin）。
  - `@banto/admin-core` の `createHttpAuthProvider` は、トークンを必ず localStorage・sessionStorage に書く（選べるのはキーの名前 `storageKey` だけ）。メモリに持つ口を banto に提案した（[tyaro/banto#838](https://github.com/tyaro/banto/issues/838)、v8 の作業に合わせる）。入るまでは、デモ専用のキー（例 `banto-industrial.demo.hub.token`）を使い、起動のたびに消してから始める。既定のキー `banto.auth.token` は読みも書きもしない。
  - テーマなどの保存が要るなら、デモ専用の接頭辞のキーにする（今の範囲では保存しない。§3.3）。
  - **実装の決定（2026-11-23）**: デモのビルドでは `localStorage`・`sessionStorage` の両方をタブの中のメモリの `Storage` に差し替える（`packages/demo-backend` の `installDemoStorage()`、各アプリの `src/hooks.client.ts` の最上位で、アプリのモジュールの読み込みより前に）。テーマ・ドックの配置・見張りの一覧・トークンなど既存の画面のコードが既定のキーで書いても、オリジンの本物の storage には何も残らず、ほかのデモの値も拾わない。`app.html` の FOUC 対策は JS の前に本物の `banto.theme`・`banto.preset` を**読むだけ**（書かない）なので、そのままにしている。

### 3.3 模擬のデータと値

- **状態はタブの中のメモリだけ**。読み直すと初期のデータに戻る。保存はしない（ui-settings もメモリ）。
- **初期のデータ**: 模擬の PLC の接続 2 本、収集グループ 2 つ、タグ 20 本ほど（数値・bit・文字列を混ぜる）。ChronoGazer は表示グループ 2 つ。ドライバの一覧は本物と同じ種類を返す（アドレスは見た目だけで、どこにもつながない）。
- **登録・編集・削除**は模擬の中で本当に反映し、一覧・ツリー・値の購読に出る。アドレスの解析・式のチェックなど判断の要る口は、最小限の規則で返す（細かい検証の再現はしない）。
- **値**: タグ ID と時刻から決まる関数で作る（数値はゆっくりした正弦波とゆらぎ、bit は周期で切り替え、文字列は固定）。更新の間隔は収集グループの周期（下限 500ms）。
- **履歴**: 同じ関数を時刻で引いて作るので、どの期間を開いてもデータがある（保存しない）。`bins` に合わせて最小・最大・平均を返す。
- Hub の WebSocket は subscribe の `tags`（パターン）と `ids` に従い、購読の直後に 1 回、そのあとは変わった値を送る。`config_changed` はタグの登録・編集のたびに送る。
- SSE（`/api/events`）は、つないだまま何も送らない。

### 3.4 対象外の画面

- **決定（2026-11-23、2b-2）**: 両方やる。メニュー（メニューバー・サイドバー・ドック・コマンドパレット）には対象の画面だけを出す（各アプリの `navigation.ts` の `DEMO_NAV_PATHS`。Hub は `/tags`、ChronoGazer は `/tags`・`/monitor`）。それでも対象外の画面へアプリの中から着いたとき（履歴・画面内のリンク）は、`(app)/+layout.svelte` が中身を描かず「この画面はデモでは使えません」と対象の画面へのリンクを出す（中身を描けば、その画面が模擬に無い `/api` を呼ぶ）。直に開く・読み直すは §3.5 の 404。Hub の `/` は `/tags` へ入る。
- **ログアウトは出さない**（ログイン画面にデモの模擬が無く、戻れなくなるため）。ヘッダーを「デモを最初から」に替え、入口を読み込み直す（状態は全部メモリなので最初に戻る）。コマンドパレットのログアウトと構成パッケージの口も出さない。
- 試験は `e2e/tests-demo-pages/public-gate.spec.ts`（組み立てた成果物で、メニューの全リンクが対象だけ・押して読み直しても出る・案内・「デモを最初から」・送信 0 件）。配信のワークフローがこの試験を名指しで走らせる。製品のビルドには入らない（`import.meta.env.VITE_BANTO_DEMO` の分岐）。
- 対象外の口は 501 と `{ "error": "demo_unsupported" }` を返す。画面の上にデモであることを示す帯を出す。

### 3.5 配信

- ワークフロー `deploy-demo.yml` を足す。契機は `workflow_dispatch` だけで、入力に配信する ref（既定は main。リリースのときはタグ名）を取る。
- 2 つのアプリを同じサイトの `/banto-industrial/hub/` と `/banto-industrial/chronogazer/` に置き、`/banto-industrial/` に入口のページを置く。ベースパスは `paths.base` を環境変数（`BASE_PATH`）で切り替え、未設定（Tauri・LAN 配信）なら今と同じビルドにする。
- サードパーティの Action は ci.yml の方針どおりコミットの SHA で固定する。
- 配信は main か `v*` のタグだけを受け付け、`e2e/tests-demo-pages/public-gate.spec.ts`（公開の関門の試験、2b-2）が無い ref は断る（2026-11-23 オーナーのレビュー #1309）。

**対象の URL を直に開く・読み直すことも、配信物で保証する**（2026-11-22 オーナーのレビュー）。両アプリは SPA の fallback（`index.html`）で作っているが、GitHub Pages は無いパスをサブディレクトリの `index.html` へ回さない。無いパスにはサイトの最上位の `404.html` を返すだけ（[GitHub Pages の 404 の仕様](https://docs.github.com/en/pages/getting-started-with-github-pages/creating-a-custom-404-page-for-your-github-pages-site)）。

- 対象のルートごとに静的な入口を作る。組み立ての後に fallback の `index.html` を `hub/tags/index.html`・`chronogazer/tags/index.html`・`chronogazer/monitor/index.html`（と各アプリの最上位・入口）へ写す。写した先でも資産を読めるよう、デモのビルドでは資産のパスを絶対パス（`paths.relative: false`）にする。
- Pages は `/hub/tags` を `/hub/tags/` へ転送する。SvelteKit の `trailingSlash` と行き来して転送が止まらない、ということが無いかを確かめ、要るならデモのビルドだけ `trailingSlash: 'always'` にする。
- サイトの最上位の `404.html` は、入口へのリンクを出すだけにする（対象外のパスを SPA として開かない）。
- **確かめ方**: 開発サーバーではなく、組み立てた Pages の成果物を、fallback の無い静的なサーバーで配って確かめる。Hub の `/tags`、ChronoGazer の `/tags`・`/monitor` を、それぞれ直に開き、読み直す。

**本物の API への送信が 0 件であることを試験で確かめる**（§3.1 の制約の検査）。同じ静的な配信の上で Playwright を走らせ、ページの全部の要求と WebSocket を記録する。起動・タグの追加・値の購読・履歴の表示・対象外の操作のそれぞれで、`/api` への本物の要求・WebSocket の接続・ほかのオリジンへの要求が 0 件であること、対象の画面に値が出ることを確かめる。配信の前にこれを通す。

### 3.6 壊れていることに気づく仕組み

配信は手で回すが、**模擬のコードは普段の CI の検査に入れる**。リリースのときに初めて壊れていると分かるのを避けるため。

- 模擬のコードは各アプリの `src/` と `packages/` に置くので、今の frontend のジョブ（`svelte-check`・lint・単体の試験）にそのまま入る。
- 振り分けと値の生成には単体の試験を書く。
- **模擬の経路を変えた PR では、普段の CI でデモを組み立てて確かめる**（配信はしない）。対象は `packages/demo-backend/**`・`apps/*/src/lib/demo/**`、起動の配線（各アプリの `setup.ts`・ルートの layout）、`vite.config.ts`、`deploy-demo.yml`。これらが変わったら、デモのビルドと §3.5 の静的な配信の上の試験（直に開く・読み直す・送信 0 件）を走らせる。それ以外の PR では走らせない。
- API の口を変えた PR は、模擬の応答も同じ PR で直す（implementation-checklist に 1 行足す）。

## 4. 決めないこと・後で決めること

- デモの中で書き込み（手動書き込み）の操作を見せるか。段階 2（#1280）の実装が入るまで扱わない。
- ほかの画面（ChronoGazer の自由トレンド・ヒストリカル・日報など）を足すか。範囲を広げるときにオーナーが決める。
- Tauri の画面は対象外（ブラウザだけ）。

## 5. 危ういところ

- 模擬が本物の応答とずれる。対策は §3.6 と、応答の型を各 `*Admin.ts` の型から取ること（模擬の側で型を書き直さない）。
- `fetch` の差し替えが、banto の部品の内部の通信（あれば）を取りこぼす。§3.1 で「模擬に無いものは本物へ流さない」を制約にし、§3.5 の送信 0 件の試験で確かめる。

## 6. 進め方（PR の分け方）

1. この設計書（本 PR）。
2. 土台（2026-11-23 に 2a・2b に分けた）。**受け入れの条件**: 組み立てた成果物を静的に配って、対象の 3 つの URL を直に開き・読み直して画面が出ること。起動の間の本物の API への送信が 0 件であること。模擬の初期化を失敗させたら、通常の provider が起動しないこと。画面は開くが中身は空でよい。
   - 2a（#1308 でマージ済み）: 共通パッケージ（振り分け・値の生成と単体の試験）、両アプリのデモ用の起動とログイン無し、ベースパスの切り替え、対象のルートごとの静的な入口と `404.html`、`pnpm demo:build`・`pnpm e2e:demo-pages`。
   - 2b-1（#1309 でマージ済み）: `deploy-demo.yml`（手動。組み立てた成果物の E2E を関門に配信）、§3.6 の普段の CI の `e2e-demo-pages` ジョブ（判定は `ci-changes.mjs` の `DEMO_PATHS`）、implementation-checklist の「API の口を変えたら模擬も直す」の 1 行。
   - 2b-2（#1310 でマージ済み、**公開の関門**）: メニューを対象の画面だけに絞り、対象外へ着いたら案内を出し、ログアウトを「デモを最初から」に替える（§3.4）。組み立てた成果物でメニュー操作も E2E（`public-gate.spec.ts`）で確かめ、`deploy-demo.yml` がそれを名指しで走らせる。
3. Hub の `/tags`: 登録・編集・削除と WebSocket の値（#1311 でマージ済み）。
   - `apps/banto-hub/src/lib/demo/`: `hubState.ts`（接続 2・グループ 2・タグ 20 をメモリに持つ登録。必須・名前の重複・参照の存在・桁数・`expectedRevision` だけを検証し、変更のたびに `revision` を進めて通知）、`hubSeed.ts`・`hubDrivers.ts`（初期のデータと、本物と同じ種類の SLMP・Modbus/TCP の `/api/drivers`）、`hubRoutes.ts`（口）、`hubStream.ts`（`/api/v1/stream`）。
   - 拾った口（`/tags` を開く・追加・編集・削除・接続の作成で要るもの）: `GET` の `/api/drivers`・`/api/plc-connections`・`/api/collection-groups`・`/api/tags`・`/api/tags/:id`・`/api/tags/group-counts`・`/api/tag-catalog`・`/api/status`・`/api/pending-changes`・`/api/commissioning/status`、`POST` の `/api/tags`・`/api/tags/list`・`/api/tags/parse-addresses`・`/api/tags/batch`・`/api/tags/batch-update`・`/api/tags/batch-delete`・`/api/plc-connections`・`/api/plc-connections/test`・`/api/collection-groups`、`PUT`・`DELETE` の `/api/tags/:id`・`/api/plc-connections/:id`・`/api/collection-groups/:id`。ここに無い口（診断・時計・設定パッケージなど）は 501。
   - WebSocket は `stream.rs` と同じ形: `subscribe`（`tags` のパターン・`ids`）の直後に 1 回のスナップショット、以後は 250ms の評価でグループの周期（下限 500ms）ごとに**変わった値だけ**。登録が変わるたび `config_changed`。値は `packages/demo-backend/src/tagValues.ts` の純関数（単体の試験あり）。文字列のタグは本物と同じく数値の値を持たない（`v: null`）。接続が閉じたらタイマーを止める。
   - E2E: `e2e/tests-demo-pages/hub-tags.spec.ts`（初期のタグ・値が出て変わる・「再接続中…」が出ない・追加・編集・削除・送信 0 件・口の警告 0 件）。
4. ChronoGazer: 今の demo を置き換え、`/tags` の登録（2026-11-23 に 4a・4b に分けた）。
   - 4a（#1312）: 旧 demo モード（`BantoMode` の `'demo'`・InMemory の provider・`providers/demo.ts` の admin/admin・各 `*Admin.ts` の「デモモードでは利用できません」と画面の案内・ログイン画面の admin/admin の案内）を消す。probe の `'none'`（静的ホスト・`vite dev` の 404）も「届かない」と同じ扱いにし、再試行の後に「サーバーに接続できません」を出す（Hub と同じ。バックエンドの無い `vite dev` では画面に入れない）。`VITE_BANTO_DEMO=1` のビルドは今までどおり模擬を入れて `server` で動く。
   - 4b（#1313 でマージ済み）: `/tags` の登録・編集・削除の模擬と E2E。
     - `apps/chronogazer/src/lib/demo/`: `chronogazerState.ts`（接続 2・グループ 2・タグ 20・しきい値・表示グループ 2 をメモリに持つ登録。必須・名前の重複・参照の存在・周期（`ALLOWED_PERIOD_MS`）・桁数・`expectedRevision`・しきい値の順序だけを検証）、`chronogazerSeed.ts`・`chronogazerDrivers.ts`（初期のデータと、`listDrivers()` に出る SLMP・Modbus/TCP。Hub 経由の接続はドライバではなく「Hub から追加」が作る）、`chronogazerRoutes.ts`（口）、`chronogazerDemo.test.ts`（単体の試験）。ChronoGazer の `Tag` は文字列型を持たず（数値・bit だけ）、収集グループに `defaultWritable` も無い（Hub との違い）。
     - 拾った口: `GET` の `/api/drivers`・`/api/plc-connections`・`/api/collection-groups`・`/api/tags`・`/api/tag-thresholds`・`/api/tag-thresholds/:tagId`・`/api/config-exclusions`・`/api/display-groups`・`/api/collect/values`（表示グループのペンがあると `/monitor` の起動が現在値を引くので、タグ ID と時刻だけで決まる値を返す）、`POST` の `/api/plc-connections`・`/api/collection-groups`・`/api/tags`、`PUT`・`DELETE` の `/api/tags/:id`・`/api/plc-connections/:id`・`/api/collection-groups/:id`、`PUT` の `/api/tag-thresholds/:tagId`。`/tags` を開いて操作しても警告は 0 件。`/api/tags/from-hub`・`/api/hub*`・表示グループの編集は 501（押して初めて呼ばれる）。`GET /api/collect/history` は `/monitor` を開くだけで（猶予の 2 秒後に）呼ばれるので、系列の無い空の履歴（`state: ready`・`series: []`・`unknownTagIds` に要求のタグ）を返す。本当の履歴の模擬は §6 の 5。
     - 誤りの形は本物（`banto_tags`・`rest.rs`）に合わせる: 検証は 422 `{ kind: "validation", field_errors }`、使われている接続・グループの削除も 422（`field: "id"`。Hub と違い配下を道連れにせず、残っていれば断る）、`expectedRevision` の不一致は 409 で本文は `kind: validation`（`field: "expectedRevision"`）、無い id は 404 `not_found`。現在値は、タグ・収集グループ・接続のどれかが無効ならキーごと載せない（本物の収集の構成が外す）。表示グループのペンに使われているタグの削除は 422（`field: "displayGroups"`、参照元の名前つき）で断り、何も消さない（本物 `display_groups.rs` の `delete_tag_unless_referenced`。自由トレンドは模擬しない）。
     - E2E: `e2e/tests-demo-pages/chronogazer-tags.spec.ts`（初期の接続・グループ・タグ、追加・編集・削除と重複の誤りの文言、読み直すと初期に戻る、収集グループの追加とタグが残るグループの削除の拒否、送信 0 件・口の警告 0 件）。
5. ChronoGazer の `/monitor`: 現在値のポーリングと履歴（トレンド）（本 PR）。
   - `packages/demo-backend/src/tagHistory.ts`（純関数・単体の試験 `tagHistory.test.ts`）: 期間を `bins` 個に等分（区間の幅は `ceil((toMs - fromMs + 1) / bins)`）し、各区間を最大 24 点で標本して `{ tMs, min, max }` にする（本物の `HistoryPoint` と同じく平均は無い。`tMs` は区間の始まり）。値は `tagValues.ts` の現在値と同じ関数なので、履歴と現在値の線がつながる。口は `chronogazerRoutes.ts` の `historyOf`（`GET /api/collect/history`）。
   - 本物（`core/src/collect/history.rs`）に合わせた点: 検証は `validate_history_request` と同じ文言・同じ `field` の 422 `validation`（タグ 1〜16 本・期間は `fromMs < toMs` かつ 31 日以内・`bins` は 1〜2000・本数 × `bins` ≤ 32000）。居ないタグだけを `unknownTagIds` に、居るタグは系列（無効なタグ・グループ・接続のタグは全区間が欠測の点＝`min`・`max` が `null`）。文字列のタグも `null`。
   - 本物と違う点: `now` より後の区間は点を出さない（`now` をまたぐ区間は `now` まで）。`simulation` は常に `false`（本物はシミュレーション接続の値を記録せず `null` にするが、デモは収集を動かさないので模擬の値を出す。初期の接続 2 本は `simulation: true`）。
   - 拾った口: 新しい口は無し（`/monitor` が呼ぶ `/api/collect/history`・`/api/collect/values` は 4b までで揃っていた）。表示グループ「プレス」のトレンドに 4 本の線が描かれ、現在値のポーリングで線が伸びる。
   - E2E: `e2e/tests-demo-pages/chronogazer-monitor.spec.ts`（直に開く・線（`path.bi-trend__line`）が描かれる・「表示できる値はまだありません」が出ない・時間が進むと線の形が変わる・送信 0 件・口の警告 0 件）。現在値そのものを表示する部品は `/monitor` の既定の画面には無いので、線が伸びること（現在値のポーリングが進む）で確かめる。
6. リリースのときに手で回して公開し、README から入口に案内する。

banto v8 への追従と重なったら、v8 の追従を先にして、模擬は §3.1 の境目の口だけを直す。
