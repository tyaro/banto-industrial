/**
 * ブラウザで触れるデモ（GitHub Pages）の E2E（docs/demo-pages-design.md §3.5・§6 の 2）。
 *
 * 開発サーバーではなく、組み立てた Pages の成果物（`pnpm demo:build` の
 * `target/demo-pages/`）を、fallback の無い静的なサーバー（`e2e/demo-pages-server.mjs`、
 * GitHub Pages の 301・404 の写し）で配って確かめる。先に `pnpm demo:build` を走らせる。
 *
 * **ポート 8808**: このリポジトリの E2E の区画（e2e/README.md）の次の空き（8807 は chronogazer のサービス接続）。Rust の
 * ビルドは要らない。
 */
import { defineConfig, devices } from '@playwright/test';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const dirname = path.dirname(fileURLToPath(import.meta.url));

const PORT = 8808;
const BASE_URL = `http://127.0.0.1:${PORT}`;

export default defineConfig({
	testDir: './tests-demo-pages',
	outputDir: path.join(dirname, 'test-results-demo-pages'),
	fullyParallel: false,
	workers: 1,
	retries: process.env.CI ? 1 : 0,
	reporter: process.env.CI
		? [
				['github'],
				[
					'html',
					{ open: 'never', outputFolder: path.join(dirname, 'playwright-report-demo-pages') }
				]
			]
		: [['list']],
	expect: {
		timeout: 15_000
	},
	use: {
		baseURL: BASE_URL,
		trace: 'retain-on-failure',
		screenshot: 'only-on-failure'
	},
	projects: [{ name: 'demo-pages', use: { ...devices['Desktop Chrome'] } }],
	webServer: {
		command: `node ${JSON.stringify(path.join(dirname, 'demo-pages-server.mjs'))} ${PORT}`,
		url: `${BASE_URL}/banto-industrial/`,
		reuseExistingServer: false,
		timeout: 30_000,
		stdout: 'pipe'
	}
});
