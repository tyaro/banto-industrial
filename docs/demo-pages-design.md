# ブラウザで触れるデモ（GitHub Pages）の設計

作成日: 2026-11-22
状態: **設計中（未実装）**。2026-11-22 オーナー決定（目的・範囲・配信の契機・ログイン無し）を記録し、作りの案を置いた。実装は §6 の順に小さな PR で進める。

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

置き場所は、振り分けと値の生成の共通部分を `packages/` に新しいパッケージ（仮に `@banto-industrial/demo-backend`）として置き、口ごとの応答は `apps/<app>/src/lib/demo/` に置く。

### 3.2 ログイン無しで入る

模擬の `/api/auth/*` が、いつもログイン済みの admin を返す（`identity` は `{ id, name, role: 'admin' }`）。

- Hub の通常の「ログイン無し」は試運転の grant（`grants.commissioning`）なので、そのまま使うと試運転の表示やロックダウンの操作が画面に出る。デモでは使わず、ログイン済みの利用者として始める。トークンの置き方（起動の前に保存しておくか、grant の応答で渡すか）は、画面に試運転の表示が出ない方を実装の PR で選ぶ。
- ChronoGazer は `localDesktop` の grant でもよいが、Hub と揃えてログイン済みの admin にする。
- ChronoGazer の今の demo（InMemory・admin/admin のログイン・全部断る）は、この作りで置き換えて消す（互換は考えない）。

### 3.3 模擬のデータと値

- **状態はタブの中のメモリだけ**。読み直すと初期のデータに戻る。保存はしない（ui-settings もメモリ）。
- **初期のデータ**: 模擬の PLC の接続 2 本、収集グループ 2 つ、タグ 20 本ほど（数値・bit・文字列を混ぜる）。ChronoGazer は表示グループ 2 つ。ドライバの一覧は本物と同じ種類を返す（アドレスは見た目だけで、どこにもつながない）。
- **登録・編集・削除**は模擬の中で本当に反映し、一覧・ツリー・値の購読に出る。アドレスの解析・式のチェックなど判断の要る口は、最小限の規則で返す（細かい検証の再現はしない）。
- **値**: タグ ID と時刻から決まる関数で作る（数値はゆっくりした正弦波とゆらぎ、bit は周期で切り替え、文字列は固定）。更新の間隔は収集グループの周期（下限 500ms）。
- **履歴**: 同じ関数を時刻で引いて作るので、どの期間を開いてもデータがある（保存しない）。`bins` に合わせて最小・最大・平均を返す。
- Hub の WebSocket は subscribe の `tags`（パターン）と `ids` に従い、購読の直後に 1 回、そのあとは変わった値を送る。`config_changed` はタグの登録・編集のたびに送る。
- SSE（`/api/events`）は、つないだまま何も送らない。

### 3.4 対象外の画面

- メニューには対象の画面だけを出すか、対象外を開くと「デモでは使えません」と出す。どちらにするかは実装の PR で、手数の少ない方を選ぶ。
- 対象外の口は 501 と `{ "error": "demo_unsupported" }` を返す。画面の上にデモであることを示す帯を出す。

### 3.5 配信

- ワークフロー `deploy-demo.yml` を足す。契機は `workflow_dispatch` だけで、入力に配信する ref（既定は main。リリースのときはタグ名）を取る。
- 2 つのアプリを同じサイトの `/banto-industrial/hub/` と `/banto-industrial/chronogazer/` に置き、`/banto-industrial/` に入口のページを置く。ベースパスは `paths.base` を環境変数（`BASE_PATH`）で切り替え、未設定（Tauri・LAN 配信）なら今と同じビルドにする。
- サードパーティの Action は ci.yml の方針どおりコミットの SHA で固定する。
- 配信の前に、デモのビルドで両アプリが開き、対象の画面に値が出るかを Playwright で 1 本だけ確かめる。

### 3.6 壊れていることに気づく仕組み

配信は手で回すが、**模擬のコードは普段の CI の検査に入れる**。リリースのときに初めて壊れていると分かるのを避けるため。

- 模擬のコードは各アプリの `src/` と `packages/` に置くので、今の frontend のジョブ（`svelte-check`・lint・単体の試験）にそのまま入る。
- 振り分けと値の生成には単体の試験を書く。
- API の口を変えた PR は、模擬の応答も同じ PR で直す（implementation-checklist に 1 行足す）。

## 4. 決めないこと・後で決めること

- デモの中で書き込み（手動書き込み）の操作を見せるか。段階 2（#1280）の実装が入るまで扱わない。
- ほかの画面（ChronoGazer の自由トレンド・ヒストリカル・日報など）を足すか。範囲を広げるときにオーナーが決める。
- Tauri の画面は対象外（ブラウザだけ）。

## 5. 危ういところ

- 模擬が本物の応答とずれる。対策は §3.6 と、応答の型を各 `*Admin.ts` の型から取ること（模擬の側で型を書き直さない）。
- `fetch` の差し替えが、banto の部品の内部の通信（あれば）を取りこぼす。実装の PR で、デモのビルドのネットワークの記録に `/api` への本物の送信が 0 件であることを確かめる。

## 6. 進め方（PR の分け方）

1. この設計書（本 PR）。
2. 土台: 共通パッケージ（振り分け・値の生成と単体の試験）、両アプリのデモ用の起動とログイン無し、ベースパスの切り替え、`deploy-demo.yml` と入口のページ。画面は開くが中身は空でよい。
3. Hub の `/tags`: 登録・編集・削除と WebSocket の値。
4. ChronoGazer: 今の demo を置き換え、`/tags` の登録。
5. ChronoGazer の `/monitor`: 現在値のポーリングと履歴（トレンド）。
6. リリースのときに手で回して公開し、README から入口に案内する。

banto v8 への追従と重なったら、v8 の追従を先にして、模擬は §3.1 の境目の口だけを直す。
