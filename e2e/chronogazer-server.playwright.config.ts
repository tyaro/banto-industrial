/**
 * Playwright config for ChronoGazer's service-connection E2E (#392 B2-2):
 * the frontend served by `chronogazer-server` (the production headless host)
 * entering "this PC" (`localDesktop` grant) without logging in.
 *
 * **Why not `banto-serve`**: `banto-serve` (the smoke suite's server) always
 * closes the `localDesktop` grant - only `chronogazer-server`'s always-on
 * listener opens it (`chronogazer_core::local_grant`). So this suite runs
 * `chronogazer-server` itself.
 *
 * **No `webServer`**: the spec starts, restarts and stops `chronogazer-server`
 * as a child process (scenario 3 restarts it on the same data root; the
 * opt-out case uses a fresh root). Each start gets `CHRONOGAZER_ROOT` = a
 * directory inside this run's temp directory (never `%ProgramData%`), the
 * debug-build-only `CHRONOGAZER_TEST_PORT` = 8807 (written into the fresh
 * settings DB's `server.port` before the listen plan) and `--control-stdin`
 * (debug-build-only: `ctrl-c` on stdin stops it through the Ctrl-C path).
 *
 * **Port 8807**: the next free number in this repo's E2E block (e2e/README.md;
 * 8806 is the disconnect spec's dev PLC). The spec refuses to start when the
 * port is already taken, so it never talks to another process's server.
 *
 * Build first: `pnpm --filter chronogazer build` and `cargo build -p
 * chronogazer-core --bin chronogazer-server --features embed-ui` (debug - the
 * test hooks above exist only in debug builds).
 *
 * Temp directory / teardown: the same ownership-marker scheme as the other
 * chronogazer configs (`chronogazer-e2e-run-dir.ts`, `global-teardown.ts`).
 */
import { defineConfig, devices } from '@playwright/test';
import crypto from 'node:crypto';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { RUN_DIR_ENV, RUN_MARKER_FILE, RUN_TOKEN_ENV, ownsRunDir } from './chronogazer-e2e-run-dir';

const dirname = path.dirname(fileURLToPath(import.meta.url));

/** The port `chronogazer-server` listens on in this suite (see the module doc). */
const CHRONOGAZER_SERVER_E2E_PORT = 8807;
const BASE_URL = `http://127.0.0.1:${CHRONOGAZER_SERVER_E2E_PORT}`;

if (!ownsRunDir(process.env[RUN_DIR_ENV], process.env[RUN_TOKEN_ENV])) {
	const token = crypto.randomUUID();
	const runDir = fs.mkdtempSync(path.join(os.tmpdir(), 'chronogazer-e2e-server-'));
	fs.writeFileSync(path.join(runDir, RUN_MARKER_FILE), token, 'utf8');
	process.env[RUN_DIR_ENV] = runDir;
	process.env[RUN_TOKEN_ENV] = token;
}

export default defineConfig({
	testDir: './tests-chronogazer-server',
	outputDir: path.join(dirname, 'test-results-chronogazer-server'),
	globalTeardown: path.join(dirname, 'global-teardown.ts'),
	fullyParallel: false,
	workers: 1,
	// The serial group restarts the server on one data root; a retry would
	// start over on that already-initialized root. Each case uses a fresh root
	// instead (the spec), so no retries.
	retries: 0,
	reporter: process.env.CI
		? [
				['github'],
				[
					'html',
					{
						open: 'never',
						outputFolder: path.join(dirname, 'playwright-report-chronogazer-server')
					}
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
	projects: [{ name: 'chronogazer-server', use: { ...devices['Desktop Chrome'] } }]
});
