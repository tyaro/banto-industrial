// banto v4.0.0（タグ v4.0.0 = 4df169c）の admin-template
// `apps/admin-template/src/lib/banto/navigationSettled.svelte.ts` を写した（banto #326）。
// chronogazer 固有の差: 未保存の変更の確認（`beforeNavigate`）は今は持たないので、
// doc の `#lib/unsavedChanges.ts` への参照は template のもの（判定そのものは同じ）。
/**
 * "No navigation in progress" for the layouts' `refreshAll()` (Issue
 * #326, first met in #321).
 *
 * (`refreshAll()` replaced the deprecated `invalidateAll()` with SvelteKit 3,
 * #325. Both run the same `_invalidate()` in kit 3.0's `client.js` - the only
 * difference is that `refreshAll()` keeps `page.state`, which this app never
 * sets (no shallow routing).)
 *
 * The problem this guards against was observed on SvelteKit 2.70: an
 * `invalidateAll()` that started while SvelteKit was still running a
 * navigation made that navigation abort without clearing its internal
 * "navigating" flag (@sveltejs/kit 2.70 `client.js`: `navigate()` returns on
 * `token !== nav_token` with `is_navigating` still true). SvelteKit 3.0
 * separates the navigation and invalidation tokens, so an ordinary
 * invalidation no longer aborts the navigation in flight; the guard is kept
 * on 3.0 conservatively (an invalidation started mid-navigation can still be
 * dropped, and waiting for the navigation to settle avoids losing the
 * re-run). On 2.70 two things followed from the abort:
 * the move the user asked for is lost (the re-run reloads the URL the
 * navigation started from), and `beforeNavigate` - the unsaved-changes
 * guard (`#lib/unsavedChanges.ts`) - is skipped for the next navigation, so
 * leaving a form with unsaved input no longer asks.
 *
 * So the layouts start their re-runs only when {@link isNavigationSettled}:
 * - the root layout's startup re-run (`routes/+layout.svelte`, #321), and
 * - wiring ① of `(app)/+layout.svelte` (#326: the generation can move while
 *   a navigation is in flight - another tab's login, a revocation confirmed
 *   in the background).
 * A navigation that is still running re-runs the guard itself; whatever it
 * could not see is caught by the deferred re-run once it has completed.
 *
 * `navigating.to` alone is not enough: SvelteKit does not publish the FIRST
 * navigation (`type === 'enter'`) in `navigating`, so until that one has
 * finished it reads `null` while the flag is set. The end of the first
 * navigation is recorded app-wide by the root layout
 * ({@link trackFirstNavigation}) - not per component: a layout mounted by a
 * `refreshAll()` (no navigation) never gets an `afterNavigate` call of
 * its own.
 */
import { afterNavigate } from '$app/navigation';
import { navigating } from '$app/state';

let firstNavigationDone = $state(false);

/**
 * Records the end of the first navigation. Call once, during the root
 * layout's initialisation (it is mounted by that navigation).
 */
export function trackFirstNavigation(): void {
	afterNavigate(({ shallow }) => {
		if (shallow) return;
		firstNavigationDone = true;
	});
}

/** True when no SvelteKit navigation is in progress. Reactive (read it in an `$effect`). */
export function isNavigationSettled(): boolean {
	return firstNavigationDone && navigating.to === null;
}
