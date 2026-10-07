/**
 * Playwright config for ChronoGazer's public-viewer (閲覧公開, grant
 * `publicViewer`, I2b / #499) E2E suite (#507).
 *
 * Mirrors upstream banto's `pnpm e2e:public-viewer` (its `public-viewer`
 * project in e2e/playwright.config.ts): a SECOND `banto-serve` on its own
 * port and its own fresh SQLite DB, started with `BANTO_VIEWER_PUBLIC=1`.
 * It has to be a separate server: `banto-serve` reads `BANTO_VIEWER_PUBLIC`
 * only at startup (it seeds the settings-DB flag, and there is no REST route
 * to toggle it), and switching it on for the smoke server (`playwright.config.ts`)
 * would change smoke scenario 1's "an unauthenticated visit shows the setup
 * screen" premise. A separate config file (not a second project of the smoke
 * config) keeps `pnpm e2e` from starting an idle second server and keeps the
 * two suites' temp dirs and test-results apart - the same shape as the
 * banto-hub configs.
 *
 * **Port 8804**: the next free number in this repo's E2E block (see
 * e2e/README.md; 8803 is the chronogazer dev PLC). Like the smoke suite it
 * serves the prebuilt `banto-serve --features embed-ui` binary, so
 * `pnpm --filter chronogazer build` and `cargo build -p chronogazer-core
 * --bin banto-serve --features embed-ui` must run first. The dev PLC is not
 * needed here.
 *
 * Temp directory / teardown: the same ownership-marker scheme as the smoke
 * config (`chronogazer-e2e-run-dir.ts`, `global-teardown.ts`) - a fresh
 * `mkdtemp` directory per run, deleted only if it carries this run's token.
 */
import { defineConfig, devices } from '@playwright/test';
import crypto from 'node:crypto';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { RUN_DIR_ENV, RUN_MARKER_FILE, RUN_TOKEN_ENV, ownsRunDir } from './chronogazer-e2e-run-dir';

const dirname = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(dirname, '..');

const PORT = 8804;
const BASE_URL = `http://127.0.0.1:${PORT}`;

// Created once per run (workers re-evaluate this module and reuse the
// directory only if its marker holds this run's token) - see playwright.config.ts.
if (!ownsRunDir(process.env[RUN_DIR_ENV], process.env[RUN_TOKEN_ENV])) {
	const token = crypto.randomUUID();
	const runDir = fs.mkdtempSync(path.join(os.tmpdir(), 'chronogazer-e2e-pv-'));
	fs.writeFileSync(path.join(runDir, RUN_MARKER_FILE), token, 'utf8');
	process.env[RUN_DIR_ENV] = runDir;
	process.env[RUN_TOKEN_ENV] = token;
}
const dbPath = path.join(
	process.env[RUN_DIR_ENV] as string,
	'chronogazer-e2e-public-viewer.sqlite3'
);

const bantoServeBin = path.join(
	repoRoot,
	'target',
	'debug',
	process.platform === 'win32' ? 'banto-serve.exe' : 'banto-serve'
);

export default defineConfig({
	testDir: './tests-public-viewer',
	outputDir: path.join(dirname, 'test-results-public-viewer'),
	globalTeardown: path.join(dirname, 'global-teardown.ts'),
	fullyParallel: false,
	workers: 1,
	retries: process.env.CI ? 1 : 0,
	reporter: process.env.CI
		? [
				['github'],
				[
					'html',
					{ open: 'never', outputFolder: path.join(dirname, 'playwright-report-public-viewer') }
				]
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
	projects: [{ name: 'public-viewer', use: { ...devices['Desktop Chrome'] } }],
	webServer: {
		command: bantoServeBin,
		url: BASE_URL,
		// Never reuse a leftover server: it would carry over an already-set-up
		// database, breaking the "zero users" premise (scenarios 1-3 run before
		// any account exists; scenario 4 creates the admin).
		reuseExistingServer: false,
		timeout: 30_000,
		stdout: 'pipe',
		env: {
			PORT: String(PORT),
			BANTO_BIND: '127.0.0.1',
			BANTO_DB: dbPath,
			// Scenario 4 creates the first admin from the login screen to prove
			// that signing in leaves the public-viewer session.
			BANTO_ALLOW_SETUP: '1',
			BANTO_VIEWER_PUBLIC: '1'
		}
	}
});
