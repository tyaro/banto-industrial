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
 * recompile mid-test-run). Since R1-C C-4 there is a second prebuilt
 * binary, the dev PLC (`cargo build -p chronogazer-core --example dev_plc`),
 * started as a second `webServer` entry (see `DEV_PLC_PORT` below). None of
 * these build steps runs from this config; run them first (see README/CI
 * workflow), same division of labor as `.claude/launch.json`'s `banto-serve`
 * entry.
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
const DB_FILE_NAME = 'chronogazer-e2e.sqlite3';

// `SqliteConnectOptions::create_if_missing` (banto's crates/banto-storage/
// src/sqlite.rs) creates the DB *file* but not missing parent directories,
// so the temp dir itself must exist before banto-serve starts.
//
// **Created once per run, not once per config evaluation** (R1-C C-4,
// 2026-09-23): Playwright evaluates this module again in every worker
// process. The main process evaluates it first (and starts `webServer` with
// the path it picked); workers are spawned afterwards and inherit its
// `process.env`, so reusing `BANTO_E2E_DB_DIR` when it is already set makes
// a worker see **the same directory banto-serve is using**. Before C-4 each
// worker made (and leaked) a fresh, unused temp dir here, which nothing
// noticed because no spec looked at the path - `user-simulator-roundtrip
// .spec.ts` now reads `<dbDir>/data` with Node's `fs` to check that the
// collector really wrote a data file, and asserts that the DB file is there
// too (i.e. that it is looking at the server's directory, not a stray one).
if (!process.env.BANTO_E2E_DB_DIR) {
	process.env.BANTO_E2E_DB_DIR = fs.mkdtempSync(path.join(os.tmpdir(), 'chronogazer-e2e-'));
}
// Read by global-teardown.ts to remove `dbDir` again after the run - see
// that file's doc comment for why an env var, not a direct import.
const dbDir = process.env.BANTO_E2E_DB_DIR;
const dbPath = path.join(dbDir, DB_FILE_NAME);

// R1-C C-4: the dev PLC (`apps/chronogazer/core/examples/dev_plc.rs`, a
// Modbus TCP simulator with a ramp on 40001-40016) that
// `user-simulator-roundtrip.spec.ts` registers as an ordinary connection.
// **Port 8803**: the next free number in this repo's E2E block (8798
// chronogazer / 8799 banto-hub / 8800 relay-wright / 8801 banto-hub perf /
// 8802 banto-hub locked-down - see e2e/README.md), so every E2E port stays
// in one greppable range; it is outside this repo's other fixed ports
// (banto-serve's default 8721, the MCP ports 3101-/3200/3201 in the
// real-machine notes, the test PLC's 5200) and outside the Windows
// excluded port ranges (`netsh int ipv4 show excludedportrange
// protocol=tcp` - 50000-50059 etc. on the dev machine). It is also not
// dev_plc's own default (15020), so a dev PLC a developer left running by
// hand does not block the suite.
const DEV_PLC_PORT = 8803;
const devPlcBin = path.join(
	repoRoot,
	'target',
	'debug',
	'examples',
	process.platform === 'win32' ? 'dev_plc.exe' : 'dev_plc'
);

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
	webServer: [
		{
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
		},
		{
			// Prebuilt like banto-serve (`cargo build -p chronogazer-core
			// --example dev_plc`), for the same reasons. It binds 127.0.0.1 only;
			// `port` makes Playwright wait until it accepts a TCP connection.
			// **Not reused**: a leftover process on 8803 fails the run instead
			// of silently serving it (dev_plc itself exits non-zero with "already
			// in use" if the port is taken). Playwright kills it with the run.
			command: `"${devPlcBin}" --protocol modbus --port ${DEV_PLC_PORT}`,
			port: DEV_PLC_PORT,
			reuseExistingServer: false,
			timeout: 30_000,
			stdout: 'pipe'
		}
	]
});
