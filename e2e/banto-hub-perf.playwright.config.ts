/**
 * banto-hub の性能計測ハーネス専用 Playwright config（T18-5a 第2段、
 * docs/banto-hub-t18-design.md §4 決定6「実測ファースト方針」）。
 *
 * `banto-hub.playwright.config.ts`（T18-1 本体 e2e、CI 対象）をほぼ丸ごと
 * 踏襲した「実サーバーに対する DOM テスト」だが、下記の理由で**完全に
 * 別の config・別ポート・別 DB・別ディレクトリ**に分離してある:
 *
 * - **CI では絶対に走らせない**: CI ワークフロー（`.github/workflows/ci.yml`）
 *   は `pnpm e2e:banto-hub`（本体 config、`testMatch: 'banto-hub-*.spec.ts'`）
 *   しか呼ばない。この config の `testMatch: 'perf-*.spec.ts'` は本体
 *   config のパターンに一致しないファイル名しか拾わないので、仮に将来 CI が
 *   `e2e/` 配下を広く拾うよう変わっても二重実行しない（保険）。opt-in の
 *   ローカル実行専用（`pnpm e2e:banto-hub:perf`）。
 * - **共有 DB を 10,000 タグ・500 グループで汚さない**: 本体 e2e の
 *   `banto-hub-*.spec.ts` 群は同じ `webServer`/DB を使い回して積み上げる
 *   （`banto-hub-auth.ts` 参照）ため、そこへ性能計測用の大量データを混ぜると
 *   後続 spec が仮想化グリッドで自分の行を見失う・件数表示が食い違う等の
 *   回帰を招く。本体 config と同じ「一時ディレクトリの使い捨て SQLite」
 *   パターン（chronogazer 発祥）を踏襲し、この config 専用の DB を毎回
 *   新規作成・実行後に破棄する。
 * - **ポート 8801**: chronogazer(8798)/banto-hub 本体(8799) と衝突しない
 *   別ポート。同一マシンで3つの `pnpm e2e*` を独立に実行できる。
 *
 * ビルド前提・実行コマンド・結果の見方は e2e/README.md 参照。
 */
import { defineConfig, devices } from '@playwright/test';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const dirname = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(dirname, '..');

const PORT = 8801;
const BASE_URL = `http://127.0.0.1:${PORT}`;

// `BANTO_HUB_PERF_E2E_DB_DIR` - 本体 e2e の `BANTO_HUB_E2E_DB_DIR`/
// chronogazer の `BANTO_E2E_DB_DIR` と衝突しない専用の env 変数名
// （`global-teardown-banto-hub-perf.ts` が読む）。
const dbDir = fs.mkdtempSync(path.join(os.tmpdir(), 'banto-hub-perf-e2e-'));
const dbPath = path.join(dbDir, 'banto-hub-perf-e2e.sqlite3');
process.env.BANTO_HUB_PERF_E2E_DB_DIR = dbDir;

const bantoHubBin = path.join(
	repoRoot,
	'target',
	'debug',
	process.platform === 'win32' ? 'banto-hub.exe' : 'banto-hub'
);

export default defineConfig({
	testDir: './tests-banto-hub-perf',
	// 本体 e2e（`banto-hub-*.spec.ts`）と絶対に一致しない命名 - 誤ってどちらか
	// 一方の config がもう片方の spec を拾わないようにする二重の保険
	// （testDir 分離が主、命名規則が保険）。
	testMatch: 'perf-*.spec.ts',
	outputDir: path.join(dirname, 'test-results-banto-hub-perf'),
	globalTeardown: path.join(dirname, 'global-teardown-banto-hub-perf.ts'),
	fullyParallel: false,
	workers: 1,
	retries: 0,
	// 10,000 タグ・500 グループの seed + 検索 p95 サンプリング + 1,000 件
	// 連続登録の dry-run/適用まで1 spec 内でやり切るため、既定の 30 秒では
	// 到底足りない。CI には出さないハーネスなので大きめに振っておく。
	timeout: 10 * 60_000,
	reporter: [['list']],
	expect: {
		timeout: 15_000
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
		reuseExistingServer: false,
		timeout: 30_000,
		env: {
			PORT: String(PORT),
			BANTO_BIND: '127.0.0.1',
			BANTO_DB: dbPath,
			BANTO_ALLOW_SETUP: '1'
		}
	}
});
