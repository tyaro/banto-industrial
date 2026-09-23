/**
 * The chronogazer E2E run's own temp directory: the names of the internal
 * variables that carry it, its ownership marker, and the check that a
 * directory really is this run's (#412 owner review, 2026-09-23).
 *
 * Shared by `playwright.config.ts` (creates the directory once per run and
 * lets workers reuse it), `global-teardown.ts` (deletes it recursively) and
 * `tests/user-simulator-roundtrip.spec.ts` (reads the collector's data files
 * inside it). Kept free of side effects so importing it never creates a
 * directory.
 *
 * **Why a marker and a token, not just a path**: the teardown runs
 * `fs.rmSync(dir, { recursive: true })`. A path alone can come from anywhere
 * (the caller's shell once handed `BANTO_E2E_DB_DIR` straight to it), so the
 * run proves ownership instead: it writes a random per-run token into
 * {@link RUN_MARKER_FILE} inside the directory it just `mkdtemp`'d, and both
 * reuse (by workers) and deletion (by the teardown) require the marker to
 * hold exactly that token. A directory without the marker - any directory
 * this run did not create - is never reused and never deleted.
 */
import fs from 'node:fs';
import path from 'node:path';

/** Internal: the run's temp directory (set by `playwright.config.ts`). */
export const RUN_DIR_ENV = 'CHRONOGAZER_E2E_RUN_DIR';
/** Internal: the run's token, also written into {@link RUN_MARKER_FILE}. */
export const RUN_TOKEN_ENV = 'CHRONOGAZER_E2E_RUN_TOKEN';
/** Ownership marker inside the run directory (content = the token). */
export const RUN_MARKER_FILE = '.chronogazer-e2e-run';

/** Does `dir` carry this run's ownership marker with exactly `token`? */
export function ownsRunDir(dir: string | undefined, token: string | undefined): boolean {
	if (!dir || !token) return false;
	try {
		return fs.readFileSync(path.join(dir, RUN_MARKER_FILE), 'utf8') === token;
	} catch {
		return false;
	}
}
