// banto v2.0.0（タグ v2.0.0 = dc61fc1）の admin-template
// `apps/admin-template/src/lib/banto/logout.svelte.ts` からコピー（v2 移行 PR1c）。
// chronogazer 固有の差: なし（本文は無改変）。
/**
 * The logout, and leaving the protected screens for /login (Issue #260
 * 実装-3, design §6.1, I-10/I-18).
 *
 * Order: `logout()` -> `resolveSettled()` -> `goto('/login')` only when that
 * confirmed `none`, with `isLeavingForLogin()` true for the whole sequence.
 *
 * - The logout does not decide the outcome. The provider clears only the
 *   credential the logout started from (compare-and-set) and reports it;
 *   the controller then confirms what the stored credential is NOW
 *   (`resolveSettled` as a signal: only a probe started after the logout
 *   can answer it). `none` -> /login. `active` -> another session was
 *   confirmed meanwhile (another tab's login, S-51/S-17): stay, and let
 *   wiring ① rebuild the screen for it once this sequence ends.
 *   `unverified` -> stay; the controller keeps confirming in the background
 *   and wiring ① re-runs the guard, which shows the retry page (503) while
 *   it cannot confirm. No `end()` (I-10): v2.0.0 removed `endSession()`, and
 *   `end()` is for app policies holding their own ticket, never a logout.
 * - While leaving, the protected layout's generation check (wiring ①,
 *   `(app)/+layout.svelte`) does not call `refreshAll()`. The logout's
 *   own hold moves the generation, and SvelteKit lets such an invalidation
 *   win over the `goto('/login')` started right after it - with public
 *   viewing on, the re-run minted a public-viewer session and the tab stayed
 *   on the protected screen (E2E public-viewer 5a, found in 実装-2).
 * - The login screen appears only AFTER the logout and its confirmation
 *   finished. A login submitted there while the logout request was still in
 *   flight would lose the provider's compare-and-set to it ("another session
 *   was confirmed first", CI smoke scenario 7 of #265).
 * - The outcome is returned, never thrown (independent audit of 実装-3
 *   P2-2): `'left'` (confirmed `none`, now on /login), `'stayed'` (still
 *   signed in - the logout was refused, e.g. a structured Tauri error, or
 *   another login was confirmed meanwhile) or `'unverified'` (the session
 *   could not be confirmed). A rejected `logout()` is decided the same way -
 *   the confirmation still runs. `options.notify` hears `'stayed'` and
 *   `'unverified'` so the UI can tell the user (`#lib/banto/logoutNotice.ts`);
 *   before, a rejection was rethrown into a click handler nobody caught.
 */
import {
	getAuthProvider,
	getSessionController,
	resolveSettled,
	type AuthProvider,
	type SessionController
} from '@banto/admin-core';

let leaving = $state(0);

/** Reactive: this tab is logging out or otherwise leaving for /login (wiring ① holds its re-load meanwhile). */
export function isLeavingForLogin(): boolean {
	return leaving > 0;
}

/**
 * Navigate to the login screen with wiring ① held until the navigation
 * settled (`ownerChangePolicy: 'relogin'`, `#lib/banto/ownerChange.ts`).
 */
export async function leaveForLogin(goToLogin: () => Promise<void>): Promise<void> {
	leaving += 1;
	try {
		await goToLogin();
	} finally {
		leaving -= 1;
	}
}

/** What a logout ended in (see the module doc). */
export type LogoutOutcome = 'left' | 'stayed' | 'unverified';

export interface LogoutOptions {
	/** Tell the user about an outcome other than `'left'`. */
	notify?: (outcome: Exclude<LogoutOutcome, 'left'>) => void;
	provider?: AuthProvider;
	controller?: SessionController;
}

/** Log out, confirm the session, and go to the login screen only when it is confirmed `none`. Never rejects on the logout's own failure. */
export async function logoutAndLeave(
	goToLogin: () => Promise<void>,
	options: LogoutOptions = {}
): Promise<LogoutOutcome> {
	const provider = options.provider ?? getAuthProvider();
	const controller = options.controller ?? getSessionController();
	leaving += 1;
	try {
		try {
			await provider.logout();
		} catch {
			// Decided by the confirmation below, like a completed logout.
		}
		const result = await resolveSettled(controller, { cause: 'signal' });
		if (result.outcome === 'confirmed' && result.snapshot.status === 'none') {
			await goToLogin();
			return 'left';
		}
		const outcome = result.outcome === 'unverified' ? 'unverified' : 'stayed';
		options.notify?.(outcome);
		return outcome;
	} finally {
		leaving -= 1;
	}
}
