/**
 * Playwright config for banto-hub's own E2E suite (T18-1、
 * docs/hub/banto-hub-desktop-plan.md §16.3「banto-hub の Playwright/DOM テスト
 * 基盤を T18-1 の成果物へ前倒し」+ §9 TAG-P0-1 の残受け入れ「実 DOM
 * テストを追加する」)。
 *
 * `e2e/playwright.config.ts`（ChronoGazer 用）を踏襲した「LAN/REST-mode の
 * 実サーバーに対する smoke/DOM テスト」で、モックした frontend ではない。
 * banto-hub は Tauri を使わない headless axum サーバー専用アプリ
 * （設計 §3.1）なので、`webServer` は `apps/banto-hub/host`（package 名
 * `banto-hub-host`）の `banto-hub` バイナリをそのまま起動する
 * （`cargo run` ではなく既にビルド済みのバイナリ — 起動をほぼ瞬時にし、
 * テスト実行中の不意な再コンパイルを避ける、chronogazer 側と同じ理由）。
 * `pnpm --filter banto-hub build` と
 * `cargo build -p banto-hub-host --bin banto-hub --features embed-ui` は
 * このファイルの外（README/CI ワークフロー）で先に実行しておくこと。
 *
 * chronogazer の `playwright.config.ts`/`smoke.spec.ts` とはポート・
 * 一時DB・テスト用ディレクトリ・出力先ディレクトリを分離してある（下記）
 * ので、両方の `pnpm e2e*` を同一マシンで独立に実行できる。**chronogazer
 * 用ファイル（playwright.config.ts/global-teardown.ts/tests/smoke.spec.ts）
 * はこの config から一切参照・変更しない。**
 *
 * `testDir` を chronogazer と同じ `e2e/tests/` ではなく専用の
 * `e2e/tests-banto-hub/` にしているのも同じ分離目的:
 * chronogazer 側の `playwright.config.ts` は `testDir: './tests'` を
 * `testMatch` で絞り込んでいない（デフォルトの `*.spec.ts` パターンで
 * `./tests` 配下を丸ごと拾う）ため、banto-hub 用の spec を
 * `e2e/tests/` に置くと `pnpm e2e`（chronogazer 側）がこの config の
 * `webServer`（banto-hub）ではなく自分の `webServer`（chronogazer）に対して
 * banto-hub 用 spec も実行してしまい、chronogazer 側の管理者アカウント作成
 * （初回セットアップ）を banto-hub 用 spec に先取りされて壊れる
 * （実測済みの回帰 - `pnpm e2e` 側は変更禁止のファイルなのでこちら側の
 * ディレクトリ分離で解決する）。
 */
import { defineConfig, devices } from '@playwright/test';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import {
	HUB_ADMIN_DISPLAY_NAME,
	HUB_ADMIN_PASSWORD,
	HUB_ADMIN_USERNAME
} from './tests-banto-hub/banto-hub-auth';

const dirname = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(dirname, '..');

// chronogazer は 8798（e2e/playwright.config.ts）: banto-hub は 8805 を使い、
// 同一マシンで両方の `webServer` を同時に起動しても衝突しない。2026-10-09
// オーナー決定で 8799 から移した（オーナーが 8799 を別の作業で使うため。
// banto 本体の E2E は今も 8799 を使う）。
const PORT = 8805;
const BASE_URL = `http://127.0.0.1:${PORT}`;

// #341（2026-09-14）: **ロックダウン済み専用の2台目**。試運転中（未ロック
// ダウン）は収集中の構成 CRUD が即時・無停止で反映されるようになったため、
// 「収集中の CRUD は未適用キューへ積まれる」ことを見る
// `banto-hub-status-pending-apply-cancel.spec.ts` だけは**ロックダウン済み**の
// サーバーが要る。banto v8 追従で、1 台目・2 台目とも `create-admin` により最初から
// ロックダウン済みになった（下の firstAdminEnv）ので、台を分ける理由は薄れたが、
// 専用サーバー・専用 spec の構成（`LOCKED_DOWN_SPECS`）はそのまま残している
// （ロックダウン専用の spec が他の spec の状態に触れない分離として）。
// 試運転モードのサーバーは 3 台目（`COMMISSIONING_PORT`）だけが受け持つ。
//
// ポートは 8802 - 8798 chronogazer / 8805 banto-hub 本体（2026-10-09 に 8799 から移した） /
// 8800 relay-wright / 8801 banto-hub perf（`banto-hub-perf.playwright.config.ts`）
// のいずれとも衝突しない（指示の 8801 は perf が既に使用済みのため1つずらした）。
const LOCKED_DOWN_PORT = 8802;
const LOCKED_DOWN_BASE_URL = `http://127.0.0.1:${LOCKED_DOWN_PORT}`;

// banto v8 追従: **試運転モード専用の 3 台目**。`create-admin` を通さずに起動した
// 新しい DB の Hub は試運転モード（loopback だけ・ログインなしの admin の grant）で
// 始まる。`banto-hub-lockdown-by-grant.spec.ts` が「grant で UI からアカウントを作る →
// UI でロックダウン → ログイン画面へ」の流れを見るのに使う。8798〜8808 のどれとも
// 衝突しない 8809。
const COMMISSIONING_PORT = 8809;
const COMMISSIONING_BASE_URL = `http://127.0.0.1:${COMMISSIONING_PORT}`;

// **試運転モード専用の 4 台目**。ロックダウンは 1 つの Hub につき一方向なので、grant 側の
// 流れ（3 台目）とアカウント側の流れ（`banto-hub-lockdown-by-account.spec.ts`）は別の Hub で
// 走らせる。8798〜8809 のどれとも衝突しない 8810。
const COMMISSIONING_ACCOUNT_PORT = 8810;
const COMMISSIONING_ACCOUNT_BASE_URL = `http://127.0.0.1:${COMMISSIONING_ACCOUNT_PORT}`;

/**
 * 試運転モードのサーバー（3 台目）で走らせる spec 一覧。この spec は 1 本で
 * サーバーをロックダウンまで進めるので、1 台を 1 本の spec が使い切る。
 */
const COMMISSIONING_SPECS = ['**/banto-hub-lockdown-by-grant.spec.ts'];

/**
 * 試運転モードのサーバー（4 台目）で走らせる spec 一覧。アカウントでログインしたままの
 * ロックダウン。3 台目とは別の Hub（ロックダウンは一方向のため）。
 */
const COMMISSIONING_ACCOUNT_SPECS = ['**/banto-hub-lockdown-by-account.spec.ts'];

/**
 * ロックダウン済みサーバーで走らせる spec 一覧（下記 `projects` 参照）。
 * #359（PR #371 の Copilot レビュー是正）で `banto-hub-settings-guard.
 * spec.ts` を追加した - `guardCategory` の redirect（非可視カテゴリの
 * `security` を再現するにはロックダウン済みが要る）と、`/settings/data`
 * 直接遷移時の構成パッケージ import ガード回帰の固定を、この専用サーバーで
 * 行う（同 spec の doc comment参照）。
 */
const LOCKED_DOWN_SPECS = [
	'**/banto-hub-status-pending-apply-cancel.spec.ts',
	'**/banto-hub-settings-guard.spec.ts',
	// #441: セッションで開いたストリーム（`/api/v1/stream`）の失効は
	// ロックダウン済みでしか起きない。
	'**/banto-hub-stream-revoked.spec.ts',
	// #441 レビュー対応: 同じくセッションで開くストリームに close を注入する。
	'**/banto-hub-stream-injected.spec.ts',
	// #445: 切れている間の失効（再接続の拒否を注入し、実サーバーで失効させる）。
	'**/banto-hub-stream-reconnect-rejected.spec.ts',
	// banto v1.7.2（tyaro/banto#241）: モニタ以外の画面が SSE の失効の知らせでログイン画面へ移る。
	'**/banto-hub-user-session-ended.spec.ts',
	// #1253: 下のログのパネルの見せる範囲（閲覧者には接続の行だけ）。閲覧者の権限の差は
	// ロックダウン済みでしか出ない。
	'**/banto-hub-user-log-panel-viewer.spec.ts'
];

// chronogazer の `BANTO_E2E_DB_DIR`/`dbDir` と同じ理由（SqliteConnectOptions::
// create_if_missing はファイルは作るが親ディレクトリは作らない）で、一時
// ディレクトリ自体を先に用意してから `BANTO_DB` に渡す。env 変数名は
// `BANTO_HUB_E2E_DB_DIR` — chronogazer の `BANTO_E2E_DB_DIR` と衝突しない
// 別名にして、`global-teardown-banto-hub.ts` が誤って chronogazer 側の一時
// ディレクトリを消してしまわないようにする。
const dbDir = fs.mkdtempSync(path.join(os.tmpdir(), 'banto-hub-e2e-'));
const dbPath = path.join(dbDir, 'banto-hub-e2e.sqlite3');
process.env.BANTO_HUB_E2E_DB_DIR = dbDir;

// 2台目の DB も**同じ**一時ディレクトリ配下に置く - `global-teardown-banto-hub.ts`
// は `BANTO_HUB_E2E_DB_DIR` を丸ごと消すので、これだけで両方が片付く。
const lockedDownDbPath = path.join(dbDir, 'banto-hub-e2e-locked-down.sqlite3');
// 3 台目（試運転モード専用）も同じ一時ディレクトリ配下。
const commissioningDbPath = path.join(dbDir, 'banto-hub-e2e-commissioning.sqlite3');

// 2台目は **profile を分ける**必要がある: `HubRuntime::start` は
// `BANTO_HUB_PROFILE`（既定 `default`）ごとに profile 排他ロックを取るので
// （`apps/banto-hub/core/src/profile_lock.rs`）、同じ profile の2プロセスは
// 同時に起動できない。あわせて `BANTO_HUB_ROOT` も一時ディレクトリへ向け、
// lock ファイル・logs・tstore データが実機の `%ProgramData%\BantoHub` を
// 汚さず teardown で消えるようにする（1台目は従来どおり既定 root/profile の
// ままで挙動を変えない）。`BANTO_DB` と違って root はディレクトリなので、
// `mkdtempSync` と同じ理由で先に作っておく。
const lockedDownRoot = path.join(dbDir, 'locked-down-root');
fs.mkdirSync(lockedDownRoot, { recursive: true });
// 3 台目も profile・root を分ける（理由は上記と同じ）。
const commissioningRoot = path.join(dbDir, 'commissioning-root');
fs.mkdirSync(commissioningRoot, { recursive: true });
// 4 台目（試運転モード専用・アカウント側）も同じ。
const commissioningAccountDbPath = path.join(dbDir, 'banto-hub-e2e-commissioning-account.sqlite3');
const commissioningAccountRoot = path.join(dbDir, 'commissioning-account-root');
fs.mkdirSync(commissioningAccountRoot, { recursive: true });

// 1台目も profile・root を一時の値にする（banto v8 追従）。`banto-hub create-admin` は
// 起動と同じ profile の排他を取る（Hub が動いていれば取れず止まる）ので、この PC で
// デスクトップシェルが `default` を握っていても邪魔されないようにする。
const mainRoot = path.join(dbDir, 'main-root');
fs.mkdirSync(mainRoot, { recursive: true });

// banto v8（ADR-0034 §5/§7）: 初回セットアップ（`POST /api/auth/setup`・`BANTO_ALLOW_SETUP`）は
// 無い。新しい DB は管理者を作るまでログインなしで、`banto-hub create-admin` が管理者の作成・
// ログイン必須・試運転の口のロックダウンを 1 度に行う。`serve-with-first-admin.mjs` が起動の
// 前にそれを実行する（パスワードは標準入力で渡す）。**create-admin はロックダウンもするので、
// 起動する Hub はどちらも最初からロックダウン済み**（試運転モードの Hub は E2E に無い）。
const serveWithFirstAdmin = path.join(dirname, 'serve-with-first-admin.mjs');
const firstAdminEnv = {
	BANTO_E2E_ADMIN_USERNAME: HUB_ADMIN_USERNAME,
	BANTO_E2E_ADMIN_PASSWORD: HUB_ADMIN_PASSWORD,
	BANTO_E2E_ADMIN_DISPLAY_NAME: HUB_ADMIN_DISPLAY_NAME
};

const bantoHubBin = path.join(
	repoRoot,
	'target',
	'debug',
	process.platform === 'win32' ? 'banto-hub.exe' : 'banto-hub'
);

export default defineConfig({
	testDir: './tests-banto-hub',
	// `testDir` 自体が chronogazer と分離済み(上記)だが、命名規則も
	// `banto-hub-*.spec.ts` に絞っておく - 将来 `tests-banto-hub/` に非
	// spec のヘルパー以外のファイルが増えても誤って拾わない保険。
	testMatch: 'banto-hub-*.spec.ts',
	// 出力先も chronogazer と別ディレクトリ（`e2e/test-results/` /
	// `e2e/playwright-report/` ではなく `-banto-hub` サフィックス付き）に
	// する - 同一マシンで両方の `pnpm e2e*` を実行しても互いの結果を
	// 上書きしない。
	outputDir: path.join(dirname, 'test-results-banto-hub'),
	globalTeardown: path.join(dirname, 'global-teardown-banto-hub.ts'),
	fullyParallel: false,
	workers: 1,
	retries: process.env.CI ? 1 : 0,
	reporter: process.env.CI
		? [
				['github'],
				['html', { open: 'never', outputFolder: path.join(dirname, 'playwright-report-banto-hub') }]
			]
		: [['list']],
	expect: {
		timeout: 10_000
	},
	use: {
		baseURL: BASE_URL,
		trace: 'retain-on-failure',
		screenshot: 'only-on-failure'
	},
	// #341: 既定のサーバー（`create-admin` 済み）・ロックダウン済み専用サーバー・
	// 試運転モード専用サーバー 2 台（banto v8 追従で追加。grant 側・アカウント側）の 5 プロジェクト。
	// `LOCKED_DOWN_SPECS` の spec はロックダウン済み専用、`COMMISSIONING_SPECS` の
	// spec は試運転モード専用で走り、他の spec は既定のサーバーで走る
	// （`testIgnore`/`testMatch` で厳密に排他にしてあるので、二重に走る spec は無い）。
	// `workers: 1` / `fullyParallel: false` は維持しているため、3つの
	// プロジェクトも順番に実行される（同時に3つのサーバーが**起動**しては
	// いるが、テストが同時に走ることはない）。
	projects: [
		{
			name: 'chromium',
			testIgnore: [...LOCKED_DOWN_SPECS, ...COMMISSIONING_SPECS, ...COMMISSIONING_ACCOUNT_SPECS],
			use: { ...devices['Desktop Chrome'] }
		},
		{
			name: 'chromium-locked-down',
			testMatch: LOCKED_DOWN_SPECS,
			use: { ...devices['Desktop Chrome'], baseURL: LOCKED_DOWN_BASE_URL }
		},
		{
			name: 'chromium-commissioning',
			testMatch: COMMISSIONING_SPECS,
			// CI のリトライは Hub・DB を作り直さない。途中まで進んだ（アカウント作成・ロックダウン
			// 済みの）状態から直列の流れをやり直すと別の理由で落ちるので、リトライしない。
			retries: 0,
			use: { ...devices['Desktop Chrome'], baseURL: COMMISSIONING_BASE_URL }
		},
		{
			name: 'chromium-commissioning-account',
			testMatch: COMMISSIONING_ACCOUNT_SPECS,
			// 上の `chromium-commissioning` と同じ理由でリトライしない。
			retries: 0,
			use: { ...devices['Desktop Chrome'], baseURL: COMMISSIONING_ACCOUNT_BASE_URL }
		}
	],
	webServer: [
		{
			command: `node "${serveWithFirstAdmin}" "${bantoHubBin}"`,
			url: BASE_URL,
			// 前回実行の DB を引き継ぐと「create-admin 済みの新しい DB」前提が崩れる
			// - chronogazer 側と同じ理由で常に新規サーバー/新規DBを起動する。
			reuseExistingServer: false,
			timeout: 30_000,
			env: {
				PORT: String(PORT),
				BANTO_BIND: '127.0.0.1',
				BANTO_DB: dbPath,
				BANTO_HUB_ROOT: mainRoot,
				BANTO_HUB_PROFILE: 'e2e-main',
				...firstAdminEnv
			}
		},
		{
			// #341: ロックダウン済み専用（`chromium-locked-down` プロジェクト）。
			// 起動の前に `create-admin` がロックダウンまで済ませる（spec の
			// `beforeAll` の `POST /api/commissioning/lock-down` は冪等で、何もしない）。
			command: `node "${serveWithFirstAdmin}" "${bantoHubBin}"`,
			url: LOCKED_DOWN_BASE_URL,
			reuseExistingServer: false,
			timeout: 30_000,
			env: {
				PORT: String(LOCKED_DOWN_PORT),
				// create-admin でロックダウン済みだが、loopback のままでよい
				// （試運転モードの 3 台目は loopback 必須。`enforce_loopback_when_commissioning`、設計 §5.6）。
				BANTO_BIND: '127.0.0.1',
				BANTO_DB: lockedDownDbPath,
				// 上記 `lockedDownRoot` のコメント参照（profile 排他ロックを
				// 1台目と分ける）。
				BANTO_HUB_ROOT: lockedDownRoot,
				BANTO_HUB_PROFILE: 'e2e-locked-down',
				...firstAdminEnv
			}
		},
		{
			// banto v8 追従: 試運転モード専用（`chromium-commissioning` プロジェクト）。
			// `create-admin` を通さず banto-hub を直接起動する（新しい DB の Hub は
			// 試運転モードで始まり、loopback だけ・ログインなしの admin の grant）。
			command: `"${bantoHubBin}"`,
			url: COMMISSIONING_BASE_URL,
			reuseExistingServer: false,
			timeout: 30_000,
			env: {
				PORT: String(COMMISSIONING_PORT),
				// 試運転モードなので loopback 必須。
				BANTO_BIND: '127.0.0.1',
				BANTO_DB: commissioningDbPath,
				BANTO_HUB_ROOT: commissioningRoot,
				BANTO_HUB_PROFILE: 'e2e-commissioning'
			}
		},
		{
			// 試運転モード専用・アカウント側（`chromium-commissioning-account` プロジェクト）。
			// 3 台目と同じ作りで、DB・root・profile だけ別。
			command: `"${bantoHubBin}"`,
			url: COMMISSIONING_ACCOUNT_BASE_URL,
			reuseExistingServer: false,
			timeout: 30_000,
			env: {
				PORT: String(COMMISSIONING_ACCOUNT_PORT),
				BANTO_BIND: '127.0.0.1',
				BANTO_DB: commissioningAccountDbPath,
				BANTO_HUB_ROOT: commissioningAccountRoot,
				BANTO_HUB_PROFILE: 'e2e-commissioning-account'
			}
		}
	]
});
