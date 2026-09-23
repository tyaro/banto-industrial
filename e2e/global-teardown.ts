/**
 * Removes the temp SQLite directory playwright.config.ts creates for
 * `BANTO_DB` before each run (`fs.mkdtempSync` under `os.tmpdir()`) - without
 * this, every `pnpm e2e` invocation (locally or in CI) leaves one more
 * `chronogazer-e2e-XXXXXX` directory behind under the OS temp folder forever.
 * The directory and the run's token come from the internal variables the
 * config module sets (same process, main config load happens before this
 * runs), since global setup/teardown scripts have no direct handle on the
 * config object's local variables.
 *
 * **Deletes only a directory this run owns** (#412 owner review,
 * 2026-09-23): the ownership marker inside it must hold this run's token
 * (`chronogazer-e2e-run-dir.ts`). Anything else - a path that came from the
 * caller's environment, a marker that is missing or different - is left
 * alone with a one-line warning, because this is a recursive delete.
 */
import fs from 'node:fs';
import { RUN_DIR_ENV, RUN_TOKEN_ENV, ownsRunDir } from './chronogazer-e2e-run-dir';

export default function globalTeardown(): void {
	const dbDir = process.env[RUN_DIR_ENV];
	if (!dbDir) return;
	if (!ownsRunDir(dbDir, process.env[RUN_TOKEN_ENV])) {
		console.warn(`[chronogazer e2e] ${dbDir} はこの実行の所有マーカーと一致しないため削除しません`);
		return;
	}
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
