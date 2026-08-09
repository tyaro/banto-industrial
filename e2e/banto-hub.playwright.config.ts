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

// chronogazer の `BANTO_E2E_DB_DIR`/`dbDir` と同じ理由（SqliteConnectOptions::
// create_if_missing はファイルは作るが親ディレクトリは作らない）で、一時
// ディレクトリ自体を先に用意してから `BANTO_DB` に渡す。env 変数名は
// `BANTO_HUB_E2E_DB_DIR` — chronogazer の `BANTO_E2E_DB_DIR` と衝突しない
// 別名にして、`global-teardown-banto-hub.ts` が誤って chronogazer 側の一時
// ディレクトリを消してしまわないようにする。
const dbDir = fs.mkdtempSync(path.join(os.tmpdir(), 'banto-hub-e2e-'));
const dbPath = path.join(dbDir, 'banto-hub-e2e.sqlite3');
process.env.BANTO_HUB_E2E_DB_DIR = dbDir;

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
	projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'] } }],
	webServer: {
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
	}
});
