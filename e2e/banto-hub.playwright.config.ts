/**
 * Playwright config for banto-hub's own E2E suite (T18-1、
 * docs/banto-hub-desktop-plan.md §16.3「banto-hub の Playwright/DOM テスト
 * 基盤を T18-1 の成果物へ前倒し」+ §9 TAG-P0-1 の残受け入れ「実 DOM
 * テストを追加する」)。
 *
 * `e2e/playwright.config.ts`（ChronoGazer 用）を踏襲した「LAN/REST-mode の
 * 実サーバーに対する smoke/DOM テスト」で、モックした frontend ではない。
 * banto-hub は Tauri を使わない headless axum サーバー専用アプリ
 * （設計 §3.1）なので、`webServer` は `apps/banto-hub/core`（crate 名
 * `banto-hub-core`）の `banto-hub` バイナリをそのまま起動する
 * （`cargo run` ではなく既にビルド済みのバイナリ — 起動をほぼ瞬時にし、
 * テスト実行中の不意な再コンパイルを避ける、chronogazer 側と同じ理由）。
 * `pnpm --filter banto-hub build` と
 * `cargo build -p banto-hub-core --bin banto-hub --features embed-ui` は
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

const dirname = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(dirname, '..');

// chronogazer は 8798（e2e/playwright.config.ts）: banto-hub はその隣の
// 8799 を使い、同一マシンで両方の `webServer` を同時に起動しても衝突しない。
const PORT = 8799;
const BASE_URL = `http://127.0.0.1:${PORT}`;

// #341（2026-09-14）: **ロックダウン済み専用の2台目**。試運転中（未ロック
// ダウン）は収集中の構成 CRUD が即時・無停止で反映されるようになったため、
// 「収集中の CRUD は未適用キューへ積まれる」ことを見る
// `banto-hub-status-pending-apply-cancel.spec.ts` だけは**ロックダウン済み**の
// サーバーが要る。一方このスイートの他の spec は試運転モード（＝認証バイパス
// 無しでも admin トークンで動くが、ロックダウンすると初回セットアップ前提の
// smoke が壊れる）を前提に組まれていて、同じサーバーを途中でロックダウンする
// と後続が壊れる（`banto-hub-tags-tree-context-menu.spec.ts` 冒頭の注記と
// 同じ理由）。そこで upstream banto の e2e `public-viewer`（別ポートの2本目
// banto-serve）と同じ型で、サーバーごと分ける。
//
// ポートは 8802 - 8798 chronogazer / 8799 banto-hub 本体 /
// 8800 relay-wright / 8801 banto-hub perf（`banto-hub-perf.playwright.config.ts`）
// のいずれとも衝突しない（指示の 8801 は perf が既に使用済みのため1つずらした）。
const LOCKED_DOWN_PORT = 8802;
const LOCKED_DOWN_BASE_URL = `http://127.0.0.1:${LOCKED_DOWN_PORT}`;

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
	'**/banto-hub-stream-revoked.spec.ts'
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
	// #341: 試運転モードのサーバー（既定）と、ロックダウン済み専用サーバーの
	// 2プロジェクト。`LOCKED_DOWN_SPECS` に載っている spec だけが後者で走り、
	// 他の spec は前者で走る（`testIgnore`/`testMatch` で厳密に排他にして
	// あるので、どちらのプロジェクトでも二重に走る spec は無い）。
	// `workers: 1` / `fullyParallel: false` は維持しているため、2つの
	// プロジェクトも順番に実行される（同時に2つのサーバーが**起動**しては
	// いるが、テストが同時に走ることはない）。
	projects: [
		{
			name: 'chromium',
			testIgnore: LOCKED_DOWN_SPECS,
			use: { ...devices['Desktop Chrome'] }
		},
		{
			name: 'chromium-locked-down',
			testMatch: LOCKED_DOWN_SPECS,
			use: { ...devices['Desktop Chrome'], baseURL: LOCKED_DOWN_BASE_URL }
		}
	],
	webServer: [
		{
			command: bantoHubBin,
			url: BASE_URL,
			// 前回実行の(既にセットアップ済みの)DB を引き継ぐと、setup 画面の
			// 「ユーザー0件」前提が崩れる - chronogazer 側と同じ理由で常に
			// 新規サーバー/新規DBを起動する。
			reuseExistingServer: false,
			timeout: 30_000,
			env: {
				PORT: String(PORT),
				BANTO_BIND: '127.0.0.1',
				BANTO_DB: dbPath,
				// apps/banto-hub/core/src/bin/banto-hub.rs: POST /api/auth/setup は
				// 明示的に opt-in しないと 403 になる - 初回セットアップ画面の
				// シナリオに必要。
				BANTO_ALLOW_SETUP: '1'
			}
		},
		{
			// #341: ロックダウン済み専用（`chromium-locked-down` プロジェクト）。
			// 起動時点では1台目と同じ試運転モードで、spec の `beforeAll` が
			// 初回セットアップ → `POST /api/commissioning/lock-down` まで進める
			// （ロックダウンは不可逆なので、専用サーバーでしかできない）。
			command: bantoHubBin,
			url: LOCKED_DOWN_BASE_URL,
			reuseExistingServer: false,
			timeout: 30_000,
			env: {
				PORT: String(LOCKED_DOWN_PORT),
				// 試運転モードのまま起動するので loopback 必須
				// （`enforce_loopback_when_commissioning`、設計 §5.6）。
				BANTO_BIND: '127.0.0.1',
				BANTO_DB: lockedDownDbPath,
				BANTO_ALLOW_SETUP: '1',
				// 上記 `lockedDownRoot` のコメント参照（profile 排他ロックを
				// 1台目と分ける）。
				BANTO_HUB_ROOT: lockedDownRoot,
				BANTO_HUB_PROFILE: 'e2e-locked-down'
			}
		}
	]
});
