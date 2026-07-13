/**
 * Removes the temp SQLite directory playwright.config.ts creates for
 * `BANTO_DB` before each run (`fs.mkdtempSync` under `os.tmpdir()`) - without
 * this, every `pnpm e2e` invocation (locally or in CI) leaves one more
 * `chronogazer-e2e-XXXXXX` directory behind under the OS temp folder forever.
 * `BANTO_E2E_DB_DIR` is set by the config module itself (same process, main
 * config load happens before this runs) rather than passed some other way,
 * since global setup/teardown scripts have no direct handle on the config
 * object's local variables.
 */
import fs from 'node:fs';

export default function globalTeardown(): void {
	const dbDir = process.env.BANTO_E2E_DB_DIR;
	if (!dbDir) return;
	try {
		fs.rmSync(dbDir, { recursive: true, force: true });
	} catch {
		// Best-effort only: on Windows the `webServer` child process
		// (banto-serve.exe) can still hold the sqlite file open for a moment
		// after Playwright signals it to stop, which turns a same-tick rmSync
		// into EPERM. Leaving one temp dir behind under the OS temp folder is
		// harmless (the OS reclaims it eventually) - it must never fail the
		// overall `pnpm e2e` run, which every scenario already passed by the
		// time this hook runs.
	}
}
