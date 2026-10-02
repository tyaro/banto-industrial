// banto v2.0.0（タグ v2.0.0 = dc61fc1）の admin-template
// `apps/admin-template/src/lib/banto/ownerChange.ts` からコピー（v2 移行 PR1d）。
// banto-hub 固有の差: なし（本文は無改変。OWNER_CHANGE_POLICY は既定の rebuild のまま、オーナー決定: 現場の共有端末で使われうるので通知のみ）。
/**
 * Wiring ② of the protected layout (Issue #260 実装-3, design §6.1, I-17,
 * I-24): a change of user the controller recorded (`snapshot.
 * pendingOwnerChange` - another tab logged in as someone else, #257) is
 * reported here, then marked handled (`acknowledgeOwnerChange()`).
 *
 * - The record lives in the controller, not in the layout, so a change
 *   confirmed while the layout was not mounted (the 503 retry page between
 *   A's hold and B's confirmation, S-81) is still reported when it mounts
 *   again - `watchOwnerChanges` looks at the snapshot once on start. It is
 *   kept across `unknown` and a re-confirmation of the same user, and
 *   discarded when `none` is confirmed (S-83: a user who logged out and then
 *   explicitly logged in as C is not told about the earlier A -> B, and is
 *   not sent back to /login).
 * - Only an `active` snapshot is handled: while the session is being
 *   confirmed again (`unknown`) the record waits.
 * - The screen itself is rebuilt by wiring ① (the generation changed); this
 *   only tells the user. Neither policy clears the shared token (I-17) -
 *   another tab's B stays logged in.
 *
 * `ownerChangePolicy` (design §6.1, decision 8):
 * - `'rebuild'` (default): notify only; the screen is rebuilt for the new
 *   user with its permissions, nothing of the old user's unsaved input kept.
 * - `'relogin'`: notify, then go to /login (a shared terminal that must
 *   re-authenticate the person in front of it). What the login screen does
 *   with the still-valid session of the other tab is that app's own
 *   login-screen requirement (S-37).
 *
 * A derived app picks the policy by changing `OWNER_CHANGE_POLICY`.
 */
import type { SessionController, SessionSnapshot } from '@banto/admin-core';

export type OwnerChangePolicy = 'rebuild' | 'relogin';

/** This app's policy (design §6.1 `ownerChangePolicy`). */
export const OWNER_CHANGE_POLICY: OwnerChangePolicy = 'rebuild';

export interface OwnerChangeHandlers {
	policy: OwnerChangePolicy;
	/** Tell the user (a toast). Called once per change, with the policy that applies. */
	notify: (policy: OwnerChangePolicy, change: { from: string | null; to: string | null }) => void;
	/** `'relogin'` only: go to the login screen (with wiring ① held, `leaveForLogin`). */
	goToLogin: () => Promise<void>;
}

/**
 * Handle `controller.snapshot.pendingOwnerChange` now (on mount) and on
 * every later change. Returns the unsubscribe function - use it as an
 * `$effect`'s cleanup.
 */
export function watchOwnerChanges(
	controller: SessionController,
	handlers: OwnerChangeHandlers
): () => void {
	const handle = (snapshot: SessionSnapshot): void => {
		const change = snapshot.pendingOwnerChange;
		if (!change || snapshot.status !== 'active') return;
		// Marked handled first: a listener that throws below must not make the
		// same change be reported again on the next snapshot.
		controller.acknowledgeOwnerChange();
		handlers.notify(handlers.policy, change);
		if (handlers.policy === 'relogin') void handlers.goToLogin();
	};
	handle(controller.snapshot);
	return controller.subscribe((snapshot) => handle(snapshot));
}
