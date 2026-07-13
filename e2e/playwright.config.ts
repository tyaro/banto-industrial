/**
 * Playwright config for ChronoGazer's R1-A smoke E2E suite (docs/r1-plan.md
 * R1-A "CI に frontend/E2E ジョブ追加").
 *
 * Mirrors banto's own e2e/playwright.config.ts: a LAN/REST-mode smoke pass,
 * NOT a mocked-frontend test. `pnpm --filter chronogazer build` produces the
 * static SvelteKit build, and `cargo build -p chronogazer-core --bin
 * banto-serve --features embed-ui` produces the binary this config's
 * `webServer` launches directly (NOT `cargo run` - launching the
 * already-built binary keeps startup near-instant and avoids a surprise
 * recompile mid-test-run). Neither build step runs from this config; run
 * them first (see README/CI workflow), same division of labor as
 * `.claude/launch.json`'s `banto-serve` entry.
 *
 * The whole suite runs single-worker/serial in one spec file
 * (`tests/smoke.spec.ts`) against one shared `page` - each scenario builds
 * on state the previous one created (the admin account, ...), mirroring how
 * a person would actually click through the app once. That is also why
 * `webServer` always starts a fresh server against a fresh temp-directory
 * SQLite file: scenario 1 exercises the first-run setup screen, which only
 * appears against a database with zero users - reusing a server/db left
 * over from a previous run would skip it.
 */
import { defineConfig, devices } from '@playwright/test';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const dirname = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(dirname, '..');

const PORT = 8798;
const BASE_URL = `http://127.0.0.1:${PORT}`;

// `SqliteConnectOptions::create_if_missing` (banto's crates/banto-storage/
// src/sqlite.rs) creates the DB *file* but not missing parent directories,
// so the temp dir itself must exist before banto-serve starts.
const dbDir = fs.mkdtempSync(path.join(os.tmpdir(), 'chronogazer-e2e-'));
const dbPath = path.join(dbDir, 'chronogazer-e2e.sqlite3');
// Read by global-teardown.ts to remove `dbDir` again after the run - see
// that file's doc comment for why an env var, not a direct import.
process.env.BANTO_E2E_DB_DIR = dbDir;

const bantoServeBin = path.join(
	repoRoot,
	'target',
	'debug',
	process.platform === 'win32' ? 'banto-serve.exe' : 'banto-serve'
);

export default defineConfig({
	testDir: './tests',
	// Explicit (not the './test-results'/'./playwright-report' defaults):
	// Playwright resolves those defaults relative to the process's current
	// working directory, not this config file's directory, and `pnpm e2e`
	// (root package.json) invokes this config via `--config=e2e/...` from
	// the repo root - an implicit default would litter the repo root
	// instead of staying under e2e/ (see .gitignore's `e2e/test-results/`).
	outputDir: path.join(dirname, 'test-results'),
	globalTeardown: path.join(dirname, 'global-teardown.ts'),
	fullyParallel: false,
	workers: 1,
	// A single retry in CI absorbs shared-runner hiccups (slow first paint)
	// without masking a genuinely broken scenario - locally we want an
	// immediate, unambiguous failure instead.
	retries: process.env.CI ? 1 : 0,
	reporter: process.env.CI
		? [
				['github'],
				['html', { open: 'never', outputFolder: path.join(dirname, 'playwright-report') }]
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
		command: bantoServeBin,
		url: BASE_URL,
		// Never reuse a leftover server: it would carry over yesterday's
		// (already-set-up) database, which breaks scenario 1's "database
		// starts empty" assumption on a second local run.
		reuseExistingServer: false,
		timeout: 30_000,
		env: {
			PORT: String(PORT),
			BANTO_BIND: '127.0.0.1',
			BANTO_DB: dbPath,
			// chronogazer-core/src/bin/banto-serve.rs: POST /api/auth/setup is
			// 403'd unless explicitly opted into - required for scenario 1.
			BANTO_ALLOW_SETUP: '1'
		}
	}
});
