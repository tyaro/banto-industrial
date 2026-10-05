// banto v3.0.1（タグ v3.0.1 = 7eea5bc）の admin-template
// `apps/admin-template/src/lib/banto/startupGate.ts` を無改変でコピー（banto #321）。
/**
 * Startup gate for route guards (Issue #321, follow-up to #286).
 *
 * On the first load SvelteKit renders nothing - not even the root layout and
 * its `StartupSplash` - until every `load` of the target route has finished.
 * A protected route's guard that simply `await`s `bantoReady` therefore keeps
 * the screen blank while the server is unreachable: startup waits for the
 * user's "reconnect" on the splash, and the splash is never drawn. Pages that
 * need no guard (`/login`) showed the splash; directly opened protected
 * pages (`/dashboard`, a bookmark, a wall monitor's URL) did not.
 *
 * So a guard never waits for startup. While it is still running the guard
 * THROWS the startup deferral ({@link deferUntilStarted}) before touching any
 * provider or the session: the protected layout, page loads (they all
 * `await parent()`) and components do not run. SvelteKit then renders the root
 * layout with the error, and the root layout:
 * - keeps the splash up ("starting…" -> "cannot connect" + reconnect, #286),
 *   never the error page, while the deferral is the current error
 *   ({@link isStartupDeferral}), and
 * - re-runs the loads (`invalidateAll()`) once `bantoReady` resolves - the
 *   guard then confirms the session as usual and the SAME URL opens (or goes
 *   to /login, the public viewer, or the session-check retry page).
 *
 * A failed probe stays "not ready" (never demo, #286): the deferral is
 * transport only, it decides nothing about the environment.
 */
import { error } from '@sveltejs/kit';

/**
 * Throws the startup deferral unless startup has finished. `message` is only
 * what an error page would show if one ever rendered it (the root layout shows
 * the splash instead); it is computed only when thrown.
 */
export function deferUntilStarted(ready: boolean, message: () => string): void {
	if (ready) return;
	error(503, { message: message(), startupPending: true });
}

/** Whether the current page error is the startup deferral (and not a real error). */
export function isStartupDeferral(err: App.Error | null | undefined): boolean {
	return err?.startupPending === true;
}
